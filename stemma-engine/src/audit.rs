//! The audit core (RFC 0001): certify what changed between two documents.
//!
//! One question, answered mechanically: given a `before` and an `after`,
//! WHAT changed (tracked and untracked), what happened to the changes that
//! were already pending, and is everything else provably untouched? The
//! session form (`Document::review` — baseline captured at parse) and the
//! stateless form (`crate::api::audit` — any two byte packages) are the same
//! computation; the session handle merely supplies the baseline.
//!
//! THE CONTRACT (the reason this module exists): "declared success without
//! read-back" is the universal agent failure mode. The audit is the
//! read-back, as one call:
//! every claim below is derived from the engine's canonical model, never
//! from what a writer SAID it did.
//!
//! Design decisions, each load-bearing:
//!
//! - **Census by record identity, never by raw id ranges.** New revisions
//!   are the enumeration records of `after` left unmatched by `before`'s
//!   records. A numeric `id > watermark` rule would over-report ids that
//!   didn't survive normalization (the receipt's documented pitfall) and is
//!   UNSOUND for the stateless form: another tool (Word included) is under
//!   no obligation to allocate above the baseline's max id.
//! - **Disposition by content, never by marker absence.** A pre-existing
//!   revision still present with identical content is `Untouched`; present
//!   with different content is `Modified`; absent is `Resolved`. Whether a
//!   `Resolved` revision was accepted or rejected is deliberately NOT
//!   claimed in v1 — that distinction requires the committed-content
//!   comparison the resolution gates use, and a wrong claim here would be
//!   worse than an honest "resolved". (RFC 0001 defers it to v1.1.)
//! - **Direct (untracked) delta via reject-all projections.** A tracked
//!   change leaves the reject-all projection invariant; an untracked edit
//!   does not. So `diff(reject_all(before), reject_all(after))` is exactly
//!   the committed delta. Resolving a PRE-EXISTING revision also moves the
//!   committed content — those rows are annotated with the revision ids
//!   whose resolution they coincide with, never silently dropped.
//! - **Untouched proof by fidelity equality, never `semantic_hash`.**
//!   The block guard deliberately ignores formatting, comments, and
//!   revision metadata (`semantic_hash.rs` — it is a staleness guard, not
//!   an identity), so it cannot prove "untouched". The proof pairs the
//!   blocks the raw diff left unmentioned and requires equality under the
//!   roundtrip comparator's exhaustive fidelity classification
//!   (`roundtrip_compare::compare_tracked_block_pair`): everything that is
//!   document content compares; parse-time artifacts (internal node ids,
//!   provenance flags, computed hashes) that legitimately differ between
//!   two independent parses do not. Any pair that fails is a violation,
//!   LOUD, including the "the diff itself missed this" case.
//!
//! Scope boundary (named, per CLAUDE.md): the proof covers block content of
//! the body and every story (headers, footers, footnotes, endnotes,
//! comments) plus the body section properties via census+direct delta.
//! Package-level state outside the block model (styles, settings, media,
//! docProps) is policed by the validator (section 4) and the fidelity
//! ratchet suite, not by this proof.

use std::collections::{HashMap, HashSet};

use crate::diff::diff_documents;
use crate::domain::{
    BlockNode, CanonDoc, DiffChange, HeaderFooterKind, NodeId, StoryScope, TrackedBlock,
};
use crate::runtime::{
    ErrorCode, ErrorDetails, RuntimeError, ValidationReport, first_quarantined_block,
    first_unparseable_opaque_with_revisions, map_diff_error,
};
use crate::styles::StyleTable;
use crate::tracked_model::{
    RevisionKind, RevisionRecord, block_node_id, enumerate_revisions, extract_block_text_for_hash,
    reject_all_with_styles,
};

/// The audit report: five engine-derived sections (RFC 0001).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditReport {
    /// Section 1a: revisions present in `after` with no matching record in
    /// `before` — the session's (or the edit's) tracked-change census.
    pub new_revisions: Vec<RevisionRecord>,
    /// Section 1b: every revision that was already pending in `before`,
    /// with what happened to it.
    pub preexisting_revisions: Vec<PreexistingRevision>,
    /// Section 2: committed-content changes with no covering tracked change
    /// (the untracked delta). In a tracked-changes session this being
    /// non-empty is itself a finding.
    pub direct_changes: Vec<DirectChange>,
    /// Section 3: the untouched proof.
    pub untouched: UntouchedProof,
    /// Section 4: the package verdict on `after` — supplied by the byte
    /// edge (the caller validates the actual bytes; the canonical model
    /// cannot).
    pub validator: ValidationReport,
}

/// A revision that was already pending in `before`, and its fate in `after`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreexistingRevision {
    /// The record as enumerated in `before`.
    pub record: RevisionRecord,
    pub disposition: RevisionDisposition,
}

/// What happened to a pre-existing revision, judged by record identity and
/// content — never by marker absence alone (judging by absence produces false
/// "reverted" verdicts when a marker is merely rewritten).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RevisionDisposition {
    /// The same revision is present in `after` with identical content.
    Untouched,
    /// A revision with the same identity is present in `after` but its
    /// affected content differs — someone edited inside it.
    Modified {
        /// The record's content in `after`.
        after_excerpt: String,
    },
    /// No matching revision in `after`: it was resolved (accepted or
    /// rejected — v1 deliberately does not claim which; see module doc).
    Resolved,
}

/// One committed-content change with no covering tracked change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectChange {
    /// Which story the change lives in.
    pub story: StoryScope,
    pub kind: DirectChangeKind,
    /// The before-side block id where one exists (`BlockDeleted`,
    /// `BlockModified`, `TableChanged`); the after-side id for
    /// `BlockInserted`; `None` for story-level and section-properties rows.
    pub block_id: Option<NodeId>,
    pub old_excerpt: Option<String>,
    pub new_excerpt: Option<String>,
    /// Revision ids of pre-existing revisions (disposition `Resolved` or
    /// `Modified`) at this location: resolving a pending revision moves the
    /// committed content, so this row may be that resolution's committed
    /// effect rather than a hand edit. Empty = no such coincidence.
    pub coincides_with_resolution: Vec<u32>,
}

/// The shape of a committed-content change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectChangeKind {
    BlockInserted,
    BlockDeleted,
    BlockModified,
    /// Table structure or cell content changed (reported per table).
    TableChanged,
    /// A whole story (header/footer/footnote/endnote/comment) appeared.
    StoryInserted,
    /// A whole story disappeared.
    StoryDeleted,
    /// The body-level section properties differ between the committed
    /// projections.
    SectionPropertiesChanged,
}

impl DirectChangeKind {
    /// Wire name, used by transports serializing the report.
    pub fn as_str(self) -> &'static str {
        match self {
            DirectChangeKind::BlockInserted => "block_inserted",
            DirectChangeKind::BlockDeleted => "block_deleted",
            DirectChangeKind::BlockModified => "block_modified",
            DirectChangeKind::TableChanged => "table_changed",
            DirectChangeKind::StoryInserted => "story_inserted",
            DirectChangeKind::StoryDeleted => "story_deleted",
            DirectChangeKind::SectionPropertiesChanged => "section_properties_changed",
        }
    }
}

/// Section 3: every block outside sections 1–2 verified structurally
/// identical to the baseline, across all story parts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UntouchedProof {
    /// Blocks verified `TrackedBlock`-equal across all parts.
    pub verified_blocks: usize,
    /// Story-part families the proof walked and found content in
    /// (`"document"`, `"headers"`, `"footers"`, `"footnotes"`, `"endnotes"`,
    /// `"comments"`).
    pub parts: Vec<&'static str>,
    /// Every failure of the proof. Empty = everything outside the reported
    /// changes is provably untouched.
    pub violations: Vec<UntouchedViolation>,
}

/// One failure of the untouched proof — a difference between `before` and
/// `after` that no census row and no direct-change row accounts for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UntouchedViolation {
    pub story: StoryScope,
    pub kind: UntouchedViolationKind,
    /// Human context: which blocks, what differs.
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UntouchedViolationKind {
    /// A paired block differs structurally although no reported change
    /// covers it (either an unreported edit or a diff blind spot — both are
    /// findings, never absorbed).
    BlockDiffers {
        before_block_id: NodeId,
        after_block_id: NodeId,
    },
    /// After removing all diff-mentioned blocks the two sequences do not
    /// pair 1:1 — the diff did not fully explain the sequence difference.
    SequenceLengthMismatch {
        before_remaining: usize,
        after_remaining: usize,
    },
    /// A story present in `before` has no counterpart in `after` and no
    /// diff row reported its removal.
    StoryMissing,
    /// A story present in `after` has no counterpart in `before` and no
    /// diff row reported its insertion.
    StoryUnexpected,
}

/// Audit `after` against `before`, both as canonical documents. The
/// `validator` verdict for `after`'s BYTES is computed at the byte edge and
/// passed in (see `crate::api::audit` / `Document::review`).
///
/// Refuses (mutating nothing) when either document carries revisions the
/// census cannot see — a quarantined block or an unparseable opaque with
/// embedded tracked changes. Auditing around them would silently
/// under-report, which is the exact failure this module exists to kill.
///
/// `before_styles` / `after_styles` are each document's own style table (parse
/// them from the corresponding DOCX bytes with
/// [`crate::style_table_from_docx`]). They let the committed-baseline
/// projection re-resolve style-inherited run marks when a tracked
/// paragraph-style change is rejected — without them, a document carrying such
/// a change would produce a spurious committed-delta row. Pass `None` for a
/// document with no style table.
pub fn audit_documents(
    before: &CanonDoc,
    after: &CanonDoc,
    before_styles: Option<&StyleTable>,
    after_styles: Option<&StyleTable>,
    validator: ValidationReport,
) -> Result<AuditReport, RuntimeError> {
    refuse_unauditable(before, "before")?;
    refuse_unauditable(after, "after")?;

    // Section 1: the census delta.
    let before_records = enumerate_revisions(before);
    let after_records = enumerate_revisions(after);
    let (new_revisions, preexisting_revisions) = match_census(before_records, after_records);

    // Section 2: the committed (untracked) delta. Reject with each document's
    // own style table so a rejected paragraph-style change re-resolves its
    // runs' style-inherited marks (the bare, style-free reject would leave them
    // baked and inject a spurious committed-delta row).
    let mut before_committed = before.clone();
    reject_all_with_styles(&mut before_committed, before_styles);
    let mut after_committed = after.clone();
    reject_all_with_styles(&mut after_committed, after_styles);
    let committed_diff =
        diff_documents(&before_committed, &after_committed).map_err(map_diff_error)?;
    let mut direct_changes = direct_rows(&committed_diff.changes);
    if before_committed.body_section_properties != after_committed.body_section_properties {
        direct_changes.push(DirectChange {
            story: StoryScope::Body,
            kind: DirectChangeKind::SectionPropertiesChanged,
            block_id: None,
            old_excerpt: None,
            new_excerpt: None,
            coincides_with_resolution: Vec::new(),
        });
    }
    annotate_resolutions(&mut direct_changes, &preexisting_revisions);

    // Section 3: the untouched proof, over the RAW documents (pending
    // revisions and all): the raw diff explains every sequence difference;
    // whatever it leaves unmentioned must pair 1:1 and be structurally
    // identical, except pairs a census row already accounts for.
    let raw_diff = diff_documents(before, after).map_err(map_diff_error)?;
    let untouched = untouched_proof(
        before,
        after,
        &raw_diff.changes,
        &new_revisions,
        &preexisting_revisions,
    );

    Ok(AuditReport {
        new_revisions,
        preexisting_revisions,
        direct_changes,
        untouched,
        validator,
    })
}

/// Fail loud when a document carries revisions invisible to the census —
/// mirrors `EditSnapshot::project`'s accept/reject preflight.
fn refuse_unauditable(doc: &CanonDoc, side: &str) -> Result<(), RuntimeError> {
    if let Some(block_id) = first_quarantined_block(doc) {
        return Err(RuntimeError {
            code: ErrorCode::UnsupportedEdit,
            message: format!(
                "audit refused: {side} document's block '{block_id}' is quarantined (nested \
                 tracked changes are not representable); its revisions are invisible to the \
                 census, so the audit would silently under-report"
            ),
            details: ErrorDetails::default(),
        });
    }
    if let Some(opaque_id) = first_unparseable_opaque_with_revisions(doc) {
        return Err(RuntimeError {
            code: ErrorCode::UnsupportedEdit,
            message: format!(
                "audit refused: {side} document's opaque '{opaque_id}' carries tracked changes \
                 inside raw_xml that could not be parsed; they are invisible to the census, so \
                 the audit would silently under-report"
            ),
            details: ErrorDetails::default(),
        });
    }
    Ok(())
}

/// The identity of a revision record across two enumerations. Deliberately
/// excludes `block_id` (importer-assigned and positional — unstable between
/// two INDEPENDENT parses of related documents) and `date`; content
/// (`excerpt`) is compared separately to classify Untouched vs Modified.
type CensusKey = (StoryScope, u32, RevisionKind, Option<String>);

fn census_key(r: &RevisionRecord) -> CensusKey {
    (r.location.clone(), r.revision_id, r.kind, r.author.clone())
}

/// Match `before`'s records against `after`'s by identity, in document
/// order. Duplicate identities (Word may reuse `w:id` values) pair off as a
/// multiset: exact-content matches first, then leftovers in order — a
/// deterministic pairing, never a guess about which duplicate "really"
/// survived.
fn match_census(
    before: Vec<RevisionRecord>,
    after: Vec<RevisionRecord>,
) -> (Vec<RevisionRecord>, Vec<PreexistingRevision>) {
    let mut after_by_key: HashMap<CensusKey, Vec<usize>> = HashMap::new();
    for (idx, record) in after.iter().enumerate() {
        after_by_key
            .entry(census_key(record))
            .or_default()
            .push(idx);
    }
    let mut consumed = vec![false; after.len()];

    let mut preexisting = Vec::with_capacity(before.len());
    for record in before {
        let candidates = after_by_key.get(&census_key(&record));
        // Pass 1: an unconsumed candidate with identical content.
        let untouched_match = candidates.and_then(|idxs| {
            idxs.iter()
                .copied()
                .find(|&i| !consumed[i] && after[i].excerpt == record.excerpt)
        });
        if let Some(i) = untouched_match {
            consumed[i] = true;
            preexisting.push(PreexistingRevision {
                record,
                disposition: RevisionDisposition::Untouched,
            });
            continue;
        }
        // Pass 2: an unconsumed candidate with the same identity but
        // different content.
        let modified_match =
            candidates.and_then(|idxs| idxs.iter().copied().find(|&i| !consumed[i]));
        if let Some(i) = modified_match {
            consumed[i] = true;
            preexisting.push(PreexistingRevision {
                record,
                disposition: RevisionDisposition::Modified {
                    after_excerpt: after[i].excerpt.clone(),
                },
            });
            continue;
        }
        preexisting.push(PreexistingRevision {
            record,
            disposition: RevisionDisposition::Resolved,
        });
    }

    let new_revisions = after
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !consumed[*i])
        .map(|(_, r)| r)
        .collect();
    (new_revisions, preexisting)
}

/// Flatten a committed-projection diff into direct-change rows. Story
/// `Modified` variants recurse into their block-level changes under the
/// story's scope; story insert/delete become single story-level rows.
fn direct_rows(changes: &[DiffChange]) -> Vec<DirectChange> {
    let mut rows = Vec::new();
    for change in changes {
        push_direct_rows(&mut rows, change, &StoryScope::Body);
    }
    rows
}

fn blocks_text(blocks: &[BlockNode]) -> String {
    let mut out = String::new();
    for block in blocks {
        out.push_str(&extract_block_text_for_hash(block));
        out.push(' ');
    }
    out.trim_end().to_string()
}

fn push_direct_rows(rows: &mut Vec<DirectChange>, change: &DiffChange, story: &StoryScope) {
    let row = |kind: DirectChangeKind,
               block_id: Option<NodeId>,
               old_excerpt: Option<String>,
               new_excerpt: Option<String>| DirectChange {
        story: story.clone(),
        kind,
        block_id,
        old_excerpt,
        new_excerpt,
        coincides_with_resolution: Vec::new(),
    };
    match change {
        DiffChange::BlockDeleted {
            block_id, old_text, ..
        } => rows.push(row(
            DirectChangeKind::BlockDeleted,
            Some(block_id.clone()),
            Some(old_text.clone()),
            None,
        )),
        DiffChange::BlockInserted { block, .. } => rows.push(row(
            DirectChangeKind::BlockInserted,
            Some(block_node_id(block)),
            None,
            Some(extract_block_text_for_hash(block)),
        )),
        DiffChange::BlockModified {
            block_id,
            old_text,
            new_text,
            ..
        } => rows.push(row(
            DirectChangeKind::BlockModified,
            Some(block_id.clone()),
            Some(old_text.clone()),
            Some(new_text.clone()),
        )),
        DiffChange::TableStructureChanged {
            table_id,
            old_text,
            new_text,
            ..
        }
        | DiffChange::TableCellsModified {
            table_id,
            old_text,
            new_text,
            ..
        } => rows.push(row(
            DirectChangeKind::TableChanged,
            Some(table_id.clone()),
            Some(old_text.clone()),
            Some(new_text.clone()),
        )),

        DiffChange::HeaderModified {
            kind,
            base_part_name,
            block_changes,
            ..
        } => {
            let scope = StoryScope::Header {
                part_path: base_part_name.clone(),
                kind: kind.clone(),
            };
            for inner in block_changes {
                push_direct_rows(rows, inner, &scope);
            }
        }
        DiffChange::HeaderDeleted {
            kind,
            part_name,
            blocks,
            ..
        } => rows.push(DirectChange {
            story: StoryScope::Header {
                part_path: part_name.clone(),
                kind: kind.clone(),
            },
            kind: DirectChangeKind::StoryDeleted,
            block_id: None,
            old_excerpt: Some(blocks_text(blocks)),
            new_excerpt: None,
            coincides_with_resolution: Vec::new(),
        }),
        DiffChange::HeaderInserted {
            kind,
            part_name,
            blocks,
            ..
        } => rows.push(DirectChange {
            story: StoryScope::Header {
                part_path: part_name.clone(),
                kind: kind.clone(),
            },
            kind: DirectChangeKind::StoryInserted,
            block_id: None,
            old_excerpt: None,
            new_excerpt: Some(blocks_text(blocks)),
            coincides_with_resolution: Vec::new(),
        }),

        DiffChange::FooterModified {
            kind,
            base_part_name,
            block_changes,
            ..
        } => {
            let scope = StoryScope::Footer {
                part_path: base_part_name.clone(),
                kind: kind.clone(),
            };
            for inner in block_changes {
                push_direct_rows(rows, inner, &scope);
            }
        }
        DiffChange::FooterDeleted {
            kind,
            part_name,
            blocks,
            ..
        } => rows.push(DirectChange {
            story: StoryScope::Footer {
                part_path: part_name.clone(),
                kind: kind.clone(),
            },
            kind: DirectChangeKind::StoryDeleted,
            block_id: None,
            old_excerpt: Some(blocks_text(blocks)),
            new_excerpt: None,
            coincides_with_resolution: Vec::new(),
        }),
        DiffChange::FooterInserted {
            kind,
            part_name,
            blocks,
            ..
        } => rows.push(DirectChange {
            story: StoryScope::Footer {
                part_path: part_name.clone(),
                kind: kind.clone(),
            },
            kind: DirectChangeKind::StoryInserted,
            block_id: None,
            old_excerpt: None,
            new_excerpt: Some(blocks_text(blocks)),
            coincides_with_resolution: Vec::new(),
        }),

        DiffChange::FootnoteModified {
            id, block_changes, ..
        } => {
            let scope = StoryScope::Footnote { id: id.clone() };
            for inner in block_changes {
                push_direct_rows(rows, inner, &scope);
            }
        }
        DiffChange::FootnoteDeleted { id, blocks, .. } => rows.push(DirectChange {
            story: StoryScope::Footnote { id: id.clone() },
            kind: DirectChangeKind::StoryDeleted,
            block_id: None,
            old_excerpt: Some(blocks_text(blocks)),
            new_excerpt: None,
            coincides_with_resolution: Vec::new(),
        }),
        DiffChange::FootnoteInserted { id, blocks, .. } => rows.push(DirectChange {
            story: StoryScope::Footnote { id: id.clone() },
            kind: DirectChangeKind::StoryInserted,
            block_id: None,
            old_excerpt: None,
            new_excerpt: Some(blocks_text(blocks)),
            coincides_with_resolution: Vec::new(),
        }),

        DiffChange::EndnoteModified {
            id, block_changes, ..
        } => {
            let scope = StoryScope::Endnote { id: id.clone() };
            for inner in block_changes {
                push_direct_rows(rows, inner, &scope);
            }
        }
        DiffChange::EndnoteDeleted { id, blocks, .. } => rows.push(DirectChange {
            story: StoryScope::Endnote { id: id.clone() },
            kind: DirectChangeKind::StoryDeleted,
            block_id: None,
            old_excerpt: Some(blocks_text(blocks)),
            new_excerpt: None,
            coincides_with_resolution: Vec::new(),
        }),
        DiffChange::EndnoteInserted { id, blocks, .. } => rows.push(DirectChange {
            story: StoryScope::Endnote { id: id.clone() },
            kind: DirectChangeKind::StoryInserted,
            block_id: None,
            old_excerpt: None,
            new_excerpt: Some(blocks_text(blocks)),
            coincides_with_resolution: Vec::new(),
        }),

        DiffChange::CommentModified {
            id, block_changes, ..
        } => {
            let scope = StoryScope::Comment { id: id.clone() };
            for inner in block_changes {
                push_direct_rows(rows, inner, &scope);
            }
        }
        DiffChange::CommentDeleted { id, blocks, .. } => rows.push(DirectChange {
            story: StoryScope::Comment { id: id.clone() },
            kind: DirectChangeKind::StoryDeleted,
            block_id: None,
            old_excerpt: Some(blocks_text(blocks)),
            new_excerpt: None,
            coincides_with_resolution: Vec::new(),
        }),
        DiffChange::CommentInserted { id, blocks, .. } => rows.push(DirectChange {
            story: StoryScope::Comment { id: id.clone() },
            kind: DirectChangeKind::StoryInserted,
            block_id: None,
            old_excerpt: None,
            new_excerpt: Some(blocks_text(blocks)),
            coincides_with_resolution: Vec::new(),
        }),
    }
}

/// Annotate each direct row with the pre-existing revisions (Resolved or
/// Modified) at its location: resolving a pending revision changes the
/// committed content, so the row may be that resolution's effect. Matching
/// is by story, plus block id when the row has one; story-level rows match
/// any record in the same story. Annotation only — a row is never removed.
fn annotate_resolutions(rows: &mut [DirectChange], preexisting: &[PreexistingRevision]) {
    let moved: Vec<&PreexistingRevision> = preexisting
        .iter()
        .filter(|p| !matches!(p.disposition, RevisionDisposition::Untouched))
        .collect();
    if moved.is_empty() {
        return;
    }
    for row in rows.iter_mut() {
        let matches: Vec<u32> = moved
            .iter()
            .filter(|p| {
                p.record.location == row.story
                    && match &row.block_id {
                        Some(block_id) => &p.record.block_id == block_id,
                        None => true,
                    }
            })
            .map(|p| p.record.revision_id)
            .collect();
        row.coincides_with_resolution = matches;
    }
}

// ─── Section 3: the untouched proof ──────────────────────────────────────────

/// A block-level location a census row implicates, used to exempt its PAIR
/// from the proof (the change is accounted for in section 1, so the pair is
/// neither "verified untouched" nor a violation).
type Implicated = HashSet<(StoryScope, NodeId)>;

fn untouched_proof(
    before: &CanonDoc,
    after: &CanonDoc,
    raw_changes: &[DiffChange],
    new_revisions: &[RevisionRecord],
    preexisting: &[PreexistingRevision],
) -> UntouchedProof {
    // Census-implicated block locations. A new revision implicates its
    // after-side block; a Resolved/Modified pre-existing revision implicates
    // its before-side block. A pair is exempt when EITHER side is
    // implicated (covers status-only changes the text diff cannot see,
    // e.g. a new Delete leg stacked onto an existing insertion).
    let mut implicated_after: Implicated = HashSet::new();
    let mut implicated_stories: HashSet<StoryScope> = HashSet::new();
    for r in new_revisions {
        note_implication(&mut implicated_after, &mut implicated_stories, r);
    }
    let mut implicated_before: Implicated = HashSet::new();
    for p in preexisting {
        if !matches!(p.disposition, RevisionDisposition::Untouched) {
            note_implication(&mut implicated_before, &mut implicated_stories, &p.record);
        }
    }

    let mut verified = 0usize;
    let mut violations = Vec::new();
    let mut parts = Vec::new();

    // Body.
    if !(before.blocks.is_empty() && after.blocks.is_empty()) {
        parts.push("document");
    }
    let (body_before_removed, body_after_removed) = raw_removed_block_ids(raw_changes);
    verify_block_sequences(
        &StoryScope::Body,
        &before.blocks,
        &after.blocks,
        &body_before_removed,
        &body_after_removed,
        &implicated_before,
        &implicated_after,
        &mut verified,
        &mut violations,
    );

    // Headers / footers: paired by part name; raw-diff story rows mark
    // touched slots.
    let mut header_touched_before: HashSet<String> = HashSet::new();
    let mut header_touched_after: HashSet<String> = HashSet::new();
    let mut footer_touched_before: HashSet<String> = HashSet::new();
    let mut footer_touched_after: HashSet<String> = HashSet::new();
    let mut note_touched: HashMap<&'static str, HashSet<String>> = HashMap::new();
    for change in raw_changes {
        match change {
            DiffChange::HeaderModified {
                base_part_name,
                target_part_name,
                ..
            } => {
                header_touched_before.insert(base_part_name.clone());
                header_touched_after.insert(target_part_name.clone());
            }
            DiffChange::HeaderDeleted { part_name, .. } => {
                header_touched_before.insert(part_name.clone());
            }
            DiffChange::HeaderInserted { part_name, .. } => {
                header_touched_after.insert(part_name.clone());
            }
            DiffChange::FooterModified {
                base_part_name,
                target_part_name,
                ..
            } => {
                footer_touched_before.insert(base_part_name.clone());
                footer_touched_after.insert(target_part_name.clone());
            }
            DiffChange::FooterDeleted { part_name, .. } => {
                footer_touched_before.insert(part_name.clone());
            }
            DiffChange::FooterInserted { part_name, .. } => {
                footer_touched_after.insert(part_name.clone());
            }
            DiffChange::FootnoteModified { id, .. }
            | DiffChange::FootnoteDeleted { id, .. }
            | DiffChange::FootnoteInserted { id, .. } => {
                note_touched
                    .entry("footnotes")
                    .or_default()
                    .insert(id.clone());
            }
            DiffChange::EndnoteModified { id, .. }
            | DiffChange::EndnoteDeleted { id, .. }
            | DiffChange::EndnoteInserted { id, .. } => {
                note_touched
                    .entry("endnotes")
                    .or_default()
                    .insert(id.clone());
            }
            DiffChange::CommentModified { id, .. }
            | DiffChange::CommentDeleted { id, .. }
            | DiffChange::CommentInserted { id, .. } => {
                note_touched
                    .entry("comments")
                    .or_default()
                    .insert(id.clone());
            }
            _ => {}
        }
    }

    // Headers.
    {
        let before_stories: Vec<(String, StoryScope, &Vec<TrackedBlock>)> = before
            .headers
            .iter()
            .filter(|s| !s.synthesized)
            .map(|s| {
                (
                    s.part_name.clone(),
                    header_scope(&s.part_name, &s.kind),
                    &s.blocks,
                )
            })
            .collect();
        let after_stories: Vec<(String, StoryScope, &Vec<TrackedBlock>)> = after
            .headers
            .iter()
            .filter(|s| !s.synthesized)
            .map(|s| {
                (
                    s.part_name.clone(),
                    header_scope(&s.part_name, &s.kind),
                    &s.blocks,
                )
            })
            .collect();
        if !(before_stories.is_empty() && after_stories.is_empty()) {
            parts.push("headers");
        }
        verify_story_family(
            before_stories,
            after_stories,
            &header_touched_before,
            &header_touched_after,
            &implicated_stories,
            &implicated_before,
            &implicated_after,
            &mut verified,
            &mut violations,
        );
    }
    // Footers.
    {
        let before_stories: Vec<(String, StoryScope, &Vec<TrackedBlock>)> = before
            .footers
            .iter()
            .filter(|s| !s.synthesized)
            .map(|s| {
                (
                    s.part_name.clone(),
                    footer_scope(&s.part_name, &s.kind),
                    &s.blocks,
                )
            })
            .collect();
        let after_stories: Vec<(String, StoryScope, &Vec<TrackedBlock>)> = after
            .footers
            .iter()
            .filter(|s| !s.synthesized)
            .map(|s| {
                (
                    s.part_name.clone(),
                    footer_scope(&s.part_name, &s.kind),
                    &s.blocks,
                )
            })
            .collect();
        if !(before_stories.is_empty() && after_stories.is_empty()) {
            parts.push("footers");
        }
        verify_story_family(
            before_stories,
            after_stories,
            &footer_touched_before,
            &footer_touched_after,
            &implicated_stories,
            &implicated_before,
            &implicated_after,
            &mut verified,
            &mut violations,
        );
    }
    // Footnotes / endnotes / comments (keyed by id; insert/delete touched
    // sets are shared per family since ids identify both sides).
    {
        let touched = note_touched.remove("footnotes").unwrap_or_default();
        let before_stories: Vec<(String, StoryScope, &Vec<TrackedBlock>)> = before
            .footnotes
            .iter()
            .map(|s| {
                (
                    s.id.clone(),
                    StoryScope::Footnote { id: s.id.clone() },
                    &s.blocks,
                )
            })
            .collect();
        let after_stories: Vec<(String, StoryScope, &Vec<TrackedBlock>)> = after
            .footnotes
            .iter()
            .map(|s| {
                (
                    s.id.clone(),
                    StoryScope::Footnote { id: s.id.clone() },
                    &s.blocks,
                )
            })
            .collect();
        if !(before_stories.is_empty() && after_stories.is_empty()) {
            parts.push("footnotes");
        }
        verify_story_family(
            before_stories,
            after_stories,
            &touched,
            &touched,
            &implicated_stories,
            &implicated_before,
            &implicated_after,
            &mut verified,
            &mut violations,
        );
    }
    {
        let touched = note_touched.remove("endnotes").unwrap_or_default();
        let before_stories: Vec<(String, StoryScope, &Vec<TrackedBlock>)> = before
            .endnotes
            .iter()
            .map(|s| {
                (
                    s.id.clone(),
                    StoryScope::Endnote { id: s.id.clone() },
                    &s.blocks,
                )
            })
            .collect();
        let after_stories: Vec<(String, StoryScope, &Vec<TrackedBlock>)> = after
            .endnotes
            .iter()
            .map(|s| {
                (
                    s.id.clone(),
                    StoryScope::Endnote { id: s.id.clone() },
                    &s.blocks,
                )
            })
            .collect();
        if !(before_stories.is_empty() && after_stories.is_empty()) {
            parts.push("endnotes");
        }
        verify_story_family(
            before_stories,
            after_stories,
            &touched,
            &touched,
            &implicated_stories,
            &implicated_before,
            &implicated_after,
            &mut verified,
            &mut violations,
        );
    }
    {
        let touched = note_touched.remove("comments").unwrap_or_default();
        let before_stories: Vec<(String, StoryScope, &Vec<TrackedBlock>)> = before
            .comments
            .iter()
            .map(|s| {
                (
                    s.id.clone(),
                    StoryScope::Comment { id: s.id.clone() },
                    &s.blocks,
                )
            })
            .collect();
        let after_stories: Vec<(String, StoryScope, &Vec<TrackedBlock>)> = after
            .comments
            .iter()
            .map(|s| {
                (
                    s.id.clone(),
                    StoryScope::Comment { id: s.id.clone() },
                    &s.blocks,
                )
            })
            .collect();
        if !(before_stories.is_empty() && after_stories.is_empty()) {
            parts.push("comments");
        }
        verify_story_family(
            before_stories,
            after_stories,
            &touched,
            &touched,
            &implicated_stories,
            &implicated_before,
            &implicated_after,
            &mut verified,
            &mut violations,
        );
    }

    UntouchedProof {
        verified_blocks: verified,
        parts,
        violations,
    }
}

fn header_scope(part_name: &str, kind: &HeaderFooterKind) -> StoryScope {
    StoryScope::Header {
        part_path: part_name.to_string(),
        kind: kind.clone(),
    }
}

fn footer_scope(part_name: &str, kind: &HeaderFooterKind) -> StoryScope {
    StoryScope::Footer {
        part_path: part_name.to_string(),
        kind: kind.clone(),
    }
}

/// Record which block (or whole story) a census row implicates. The
/// `comment_story` sentinel implicates its whole comment story; the
/// `body_section` sentinel implicates no block (it lives outside the block
/// sequences).
fn note_implication(
    blocks: &mut Implicated,
    stories: &mut HashSet<StoryScope>,
    record: &RevisionRecord,
) {
    let id = record.block_id.to_string();
    if id == "comment_story" {
        stories.insert(record.location.clone());
    } else if id != "body_section" {
        blocks.insert((record.location.clone(), record.block_id.clone()));
    }
}

/// Before-side / after-side block ids the raw diff's BODY-level rows
/// mention. Story-level rows are handled per family.
fn raw_removed_block_ids(changes: &[DiffChange]) -> (HashSet<NodeId>, HashSet<NodeId>) {
    let mut before_removed = HashSet::new();
    let mut after_removed = HashSet::new();
    for change in changes {
        match change {
            DiffChange::BlockDeleted { block_id, .. } => {
                before_removed.insert(block_id.clone());
            }
            DiffChange::BlockInserted { block, .. } => {
                after_removed.insert(block_node_id(block));
            }
            DiffChange::BlockModified {
                block_id,
                new_block,
                ..
            } => {
                before_removed.insert(block_id.clone());
                after_removed.insert(block_node_id(new_block));
            }
            DiffChange::TableStructureChanged {
                table_id,
                target_table_id,
                ..
            }
            | DiffChange::TableCellsModified {
                table_id,
                target_table_id,
                ..
            } => {
                before_removed.insert(table_id.clone());
                after_removed.insert(target_table_id.clone());
            }
            _ => {}
        }
    }
    (before_removed, after_removed)
}

/// The core of the proof: remove diff-mentioned blocks from each side, pair
/// the remainder 1:1 in order, exempt census-implicated pairs, and require
/// full `TrackedBlock` equality of every remaining pair.
#[allow(clippy::too_many_arguments)]
fn verify_block_sequences(
    story: &StoryScope,
    before_blocks: &[TrackedBlock],
    after_blocks: &[TrackedBlock],
    before_removed: &HashSet<NodeId>,
    after_removed: &HashSet<NodeId>,
    implicated_before: &Implicated,
    implicated_after: &Implicated,
    verified: &mut usize,
    violations: &mut Vec<UntouchedViolation>,
) {
    let before_rest: Vec<&TrackedBlock> = before_blocks
        .iter()
        .filter(|tb| !before_removed.contains(&block_node_id(&tb.block)))
        .collect();
    let after_rest: Vec<&TrackedBlock> = after_blocks
        .iter()
        .filter(|tb| !after_removed.contains(&block_node_id(&tb.block)))
        .collect();

    if before_rest.len() != after_rest.len() {
        violations.push(UntouchedViolation {
            story: story.clone(),
            kind: UntouchedViolationKind::SequenceLengthMismatch {
                before_remaining: before_rest.len(),
                after_remaining: after_rest.len(),
            },
            detail: format!(
                "after removing diff-reported changes, {} before-side and {} after-side blocks \
                 remain — the diff did not fully explain the sequence difference",
                before_rest.len(),
                after_rest.len()
            ),
        });
        // The pairing is broken; comparing misaligned pairs would produce
        // noise on top of the (already loud) mismatch. Stop here for this
        // sequence.
        return;
    }

    for (b, a) in before_rest.iter().zip(after_rest.iter()) {
        let b_id = block_node_id(&b.block);
        let a_id = block_node_id(&a.block);
        let exempt = implicated_before.contains(&(story.clone(), b_id.clone()))
            || implicated_after.contains(&(story.clone(), a_id.clone()));
        if exempt {
            // Accounted for by a census row (section 1); neither verified
            // nor a violation.
            continue;
        }
        // Fidelity equality, not derive(PartialEq): the roundtrip comparator
        // ignores parse-time artifacts (internal node ids, provenance flags,
        // computed hashes) that legitimately differ between two independent
        // parses of identical content, and compares everything that IS
        // document content — the same classification the roundtrip suite
        // enforces exhaustively per field.
        let differences = crate::roundtrip_compare::compare_tracked_block_pair(b, a);
        if differences.is_empty() {
            *verified += 1;
        } else {
            let named: Vec<String> = differences.iter().take(3).map(|d| d.to_string()).collect();
            violations.push(UntouchedViolation {
                story: story.clone(),
                kind: UntouchedViolationKind::BlockDiffers {
                    before_block_id: b_id.clone(),
                    after_block_id: a_id.clone(),
                },
                detail: format!(
                    "block '{b_id}' differs from its counterpart '{a_id}' although no tracked \
                     change and no direct-change row covers it ({} difference(s), first: {})",
                    differences.len(),
                    named.join("; ")
                ),
            });
        }
    }
}

/// Verify one story family (headers, footers, footnotes, endnotes,
/// comments): stories the raw diff already reported are exempt; the rest
/// must pair by key and verify block-by-block.
#[allow(clippy::too_many_arguments)]
fn verify_story_family(
    before_stories: Vec<(String, StoryScope, &Vec<TrackedBlock>)>,
    after_stories: Vec<(String, StoryScope, &Vec<TrackedBlock>)>,
    touched_before: &HashSet<String>,
    touched_after: &HashSet<String>,
    implicated_stories: &HashSet<StoryScope>,
    implicated_before: &Implicated,
    implicated_after: &Implicated,
    verified: &mut usize,
    violations: &mut Vec<UntouchedViolation>,
) {
    let empty: HashSet<NodeId> = HashSet::new();
    let mut after_by_key: HashMap<String, (StoryScope, &Vec<TrackedBlock>)> = after_stories
        .iter()
        .filter(|(key, _, _)| !touched_after.contains(key))
        .map(|(key, scope, blocks)| (key.clone(), (scope.clone(), *blocks)))
        .collect();

    for (key, scope, before_blocks) in before_stories {
        if touched_before.contains(&key) {
            continue; // reported by the diff → accounted for in section 2.
        }
        if implicated_stories.contains(&scope) {
            after_by_key.remove(&key);
            continue; // accounted for by a story-level census row.
        }
        match after_by_key.remove(&key) {
            Some((_, after_blocks)) => {
                verify_block_sequences(
                    &scope,
                    before_blocks,
                    after_blocks,
                    &empty,
                    &empty,
                    implicated_before,
                    implicated_after,
                    verified,
                    violations,
                );
            }
            None => violations.push(UntouchedViolation {
                story: scope.clone(),
                kind: UntouchedViolationKind::StoryMissing,
                detail: format!(
                    "story '{key}' exists in before but has no counterpart in after, and no \
                     diff row reported its removal"
                ),
            }),
        }
    }

    for (key, (scope, _)) in after_by_key {
        if implicated_stories.contains(&scope) {
            continue;
        }
        violations.push(UntouchedViolation {
            story: scope,
            kind: UntouchedViolationKind::StoryUnexpected,
            detail: format!(
                "story '{key}' exists in after but has no counterpart in before, and no diff \
                 row reported its insertion"
            ),
        });
    }
}
