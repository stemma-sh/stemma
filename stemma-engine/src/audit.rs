//! The audit core (RFC 0001): certify what changed between two documents.
//!
//! One question, answered mechanically: given a `before` and an `after`,
//! WHAT changed (tracked and untracked), what happened to the changes that
//! were already pending, and is everything else provably untouched? The
//! session form (`Document::review` — baseline captured at parse) and the
//! stateless form (`crate::api::audit` — any two byte packages) share the
//! same checks. Session snapshots additionally supply a shared block-ID
//! namespace; independently parsed packages use explicit insertion carriers
//! and an exact-text alignment of the baseline blocks, never similarity-based
//! lineage.
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
//!   with different content is `EditedInside` when rejecting exactly the
//!   revisions new in `after` restores it (a tracked edit stacked on it),
//!   otherwise `Modified`; absent is `Resolved`. Whether a
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

use crate::audit_delta::{AuditDelta, BlockIdentity, audit_changes};
use crate::domain::{
    BlockNode, CanonDoc, HeaderFooterKind, InlineNode, NodeId, OpaqueKind, StoryScope,
    TrackedBlock, TrackingStatus,
};
use crate::runtime::{
    ErrorCode, ErrorDetails, RuntimeError, ValidationReport, first_quarantined_block,
    first_unresolvable_opaque_with_revisions,
};
use crate::styles::StyleTable;
use crate::tracked_model::{
    ResolveSelectionAction, RevisionKind, RevisionRecord, block_node_id, enumerate_revisions,
    extract_block_text_for_hash, reject_all_with_styles, resolve_selected_revisions_with_styles,
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
    /// A revision with the same identity is present in `after` with different
    /// content, and the revisions new in `after` explain all of the
    /// difference: rejecting exactly those restores this revision's content.
    /// Another author's tracked edit stacked inside it (Bob deleting a word
    /// inside Alice's pending insertion) is this case. The revision itself
    /// is neither resolved nor rewritten.
    EditedInside {
        /// The record's content in `after`.
        after_excerpt: String,
    },
    /// A revision with the same identity is present in `after` but its
    /// content differs in a way the new revisions do not explain: rejecting
    /// them does not restore it. An untracked edit inside a pending
    /// revision lands here (the reject-all committed delta cannot see it,
    /// because rejecting the revision discards the edited text with it).
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
    /// A document setting that bears on how the document reads or looks
    /// differs (`appearance::settings_in_effect`); the excerpts hold the
    /// setting as each side writes it, `None` where a side leaves it out.
    SettingsChanged,
    /// The document's background (`w:background`, §17.2.1) differs.
    BackgroundChanged,
    /// A comment present on both sides is attributed differently: its
    /// author, initials or date (§17.13.4.2), or whether its thread is
    /// marked resolved (MS-DOCX §2.5.1). The excerpts hold each side's.
    CommentAttributionChanged,
    /// What the theme gives an entry the document uses (a colour, a font,
    /// the shape format scheme) differs; the excerpts hold the entry's name
    /// and each side's value.
    ThemeChanged,
    /// A data store node a content control is bound to (§17.5.2.6) reads
    /// differently: Word fills the control with it on open. The excerpts
    /// hold the binding and each side's value.
    BoundValueChanged,
    /// A font the font table embedded (§17.8.3.3) is no longer embedded:
    /// where it is not installed, its text shows in another font.
    EmbeddedFontDropped,
    /// A note separator (`w:footnote`/`w:endnote` of a separator type,
    /// §17.11.23) differs; the excerpts hold each side's.
    NoteSeparatorChanged,
    /// A style a `STYLEREF` field names (§17.16.5.65) resolves differently
    /// or no longer resolves: Word recomputes the field in headers and
    /// footers as it lays them out. The excerpts hold the name and what it
    /// resolves to on each side.
    FieldStyleTargetChanged,
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
            DirectChangeKind::SettingsChanged => "settings_changed",
            DirectChangeKind::BackgroundChanged => "background_changed",
            DirectChangeKind::CommentAttributionChanged => "comment_attribution_changed",
            DirectChangeKind::ThemeChanged => "theme_changed",
            DirectChangeKind::BoundValueChanged => "bound_value_changed",
            DirectChangeKind::EmbeddedFontDropped => "embedded_font_dropped",
            DirectChangeKind::NoteSeparatorChanged => "note_separator_changed",
            DirectChangeKind::FieldStyleTargetChanged => "field_style_target_changed",
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
/// Block IDs are treated as independent import-local addresses. Session
/// callers use [`audit_session_documents`] with explicit shared-ID provenance.
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
///
/// A `CanonDoc` does not hold the bytes of a body block the model cannot
/// represent (a sealed content control), so revisions inside one are not in
/// this census; a change to such a block fails the untouched proof instead.
/// [`crate::api::audit`] and `Document::review` see them.
pub fn audit_documents(
    before: &CanonDoc,
    after: &CanonDoc,
    before_styles: Option<&StyleTable>,
    after_styles: Option<&StyleTable>,
    validator: ValidationReport,
) -> Result<AuditReport, RuntimeError> {
    audit_documents_with_identity(
        before,
        after,
        before_styles,
        after_styles,
        validator,
        BlockIdentity::IndependentParses,
        SealedRevisions::default(),
        UntouchedEquality::Fidelity,
        None,
    )
}

/// Session snapshots retain the baseline's block identity namespace. Only
/// callers holding that provenance may use identity-based correspondence.
/// Both documents must descend from the same imported snapshot without
/// serialization/re-import between them. For independently parsed documents,
/// use [`audit_documents`] instead: parser-assigned IDs are not shared identity.
pub fn audit_session_documents(
    before: &CanonDoc,
    after: &CanonDoc,
    before_styles: Option<&StyleTable>,
    after_styles: Option<&StyleTable>,
    validator: ValidationReport,
) -> Result<AuditReport, RuntimeError> {
    audit_documents_with_identity(
        before,
        after,
        before_styles,
        after_styles,
        validator,
        BlockIdentity::SharedSession,
        SealedRevisions::default(),
        UntouchedEquality::Fidelity,
        None,
    )
}

/// The revisions inside each side's sealed body blocks (see
/// [`crate::tracked_model::enumerate_body_opaque_block_revisions`]). Their
/// bytes live in a snapshot's scaffold, not the `CanonDoc`, so only a caller
/// holding snapshots can supply them; the `CanonDoc`-only entry points pass
/// none, and a changed sealed block then fails the untouched proof instead.
#[derive(Default)]
pub(crate) struct SealedRevisions {
    pub(crate) before: Vec<RevisionRecord>,
    pub(crate) after: Vec<RevisionRecord>,
}

/// Each side's package facts, before then after (`runtime::package_facts`).
pub(crate) type RelationshipTargets<'a> = (
    &'a crate::appearance::PackageFacts,
    &'a crate::appearance::PackageFacts,
);

#[allow(clippy::too_many_arguments)]
pub(crate) fn audit_documents_with_identity(
    before: &CanonDoc,
    after: &CanonDoc,
    before_styles: Option<&StyleTable>,
    after_styles: Option<&StyleTable>,
    validator: ValidationReport,
    identity: BlockIdentity,
    sealed: SealedRevisions,
    equality: UntouchedEquality,
    // What each side's relationships point at, when the caller holds the
    // packages. With them, a held element's references are compared by
    // their targets under either equality.
    relationships: Option<RelationshipTargets<'_>>,
) -> Result<AuditReport, RuntimeError> {
    refuse_unauditable(before, "before")?;
    refuse_unauditable(after, "after")?;

    // A note's id pairs its reference with its story and is the writer's
    // choice; the number a reader sees comes from the reference's place in
    // the document. Another program's save may renumber the ids: the notes
    // are then compared by their references' order.
    let mut sealed = sealed;
    let after_with_before_note_ids;
    let after = match equality {
        UntouchedEquality::Appearance => {
            let renames = note_renames(before, after);
            if renames.is_empty() {
                after
            } else {
                let mut renamed = after.clone();
                apply_note_renames(&mut renamed, &renames);
                for record in &mut sealed.after {
                    rename_note_scope(&mut record.location, &renames);
                }
                after_with_before_note_ids = renamed;
                &after_with_before_note_ids
            }
        }
        UntouchedEquality::Fidelity => after,
    };
    // A header or footer story is named by its relationship's target, which
    // one writer spells relative to the main part (`header1.xml`) and
    // another absolute (`/word/header1.xml`): the same part.
    let after_with_before_part_names;
    let after = match relationships {
        Some((before_facts, after_facts)) => {
            let renames = story_part_renames(
                before,
                after,
                &before_facts.main_part,
                &after_facts.main_part,
            );
            if renames.is_empty() {
                after
            } else {
                let mut renamed = after.clone();
                for story in &mut renamed.headers {
                    if let Some(name) = renames.get(&story.part_name) {
                        story.part_name = name.clone();
                    }
                }
                for story in &mut renamed.footers {
                    if let Some(name) = renames.get(&story.part_name) {
                        story.part_name = name.clone();
                    }
                }
                for record in &mut sealed.after {
                    if let StoryScope::Header { part_path, .. }
                    | StoryScope::Footer { part_path, .. } = &mut record.location
                        && let Some(name) = renames.get(part_path)
                    {
                        *part_path = name.clone();
                    }
                }
                let rename_references = |section: &mut crate::domain::SectionProperties| {
                    for reference in section
                        .header_refs
                        .iter_mut()
                        .chain(&mut section.footer_refs)
                    {
                        if let Some(name) = renames.get(&reference.part_path) {
                            reference.part_path = name.clone();
                        }
                    }
                };
                if let Some(section) = &mut renamed.body_section_properties {
                    rename_references(section);
                }
                for block in &mut renamed.blocks {
                    if let BlockNode::Paragraph(paragraph) = &mut block.block
                        && let Some(section) = &mut paragraph.section_properties
                    {
                        rename_references(section);
                    }
                }
                after_with_before_part_names = renamed;
                &after_with_before_part_names
            }
        }
        None => after,
    };

    let mut before_records = enumerate_revisions(before);
    before_records.extend(sealed.before);
    let restated = {
        let mut after_records = enumerate_revisions(after);
        after_records.extend(sealed.after.iter().cloned());
        restated_identities(&before_records, &after_records)
    };
    let after_with_before_identities;
    let after = if restated.is_empty() {
        after
    } else {
        let mut renamed = after.clone();
        crate::import::for_each_rev_carrier_mut(&mut renamed, &mut |carrier| {
            if let Some(identity) = restated.get(carrier.identity) {
                *carrier.identity = *identity;
            }
        });
        for record in &mut sealed.after {
            if let Some(identity) = restated.get(&record.revision_id) {
                record.revision_id = *identity;
            }
        }
        after_with_before_identities = renamed;
        &after_with_before_identities
    };

    // Section 1: the census delta.
    let mut after_records = enumerate_revisions(after);
    after_records.extend(sealed.after.iter().cloned());
    let (new_revisions, mut preexisting_revisions) =
        match_census(before_records.clone(), after_records);
    classify_edited_inside(
        before_records,
        after,
        after_styles,
        sealed.after,
        &new_revisions,
        &mut preexisting_revisions,
    );

    // Section 2: the committed (untracked) delta. Reject with each document's
    // own style table so a rejected paragraph-style change re-resolves its
    // runs' style-inherited marks (the bare, style-free reject would leave them
    // baked and inject a spurious committed-delta row).
    let mut before_committed = before.clone();
    reject_all_with_styles(&mut before_committed, before_styles);
    let mut after_committed = after.clone();
    reject_all_with_styles(&mut after_committed, after_styles);
    // Under appearance equality the committed delta is taken between the
    // two committed documents in appearance form, so a program that rewrote
    // bookkeeping without changing the document reports no direct change.
    // The form is applied after the reject, which reads the provenance the
    // form removes. Which bookmarks exist is read from the raw documents:
    // a reject removes markers, never adds one.
    let bookmarks = match equality {
        UntouchedEquality::Appearance => Some((
            crate::appearance::bookmarks_in_effect(before),
            crate::appearance::bookmarks_in_effect(after),
        )),
        UntouchedEquality::Fidelity => None,
    };
    let before_side = crate::appearance::Side {
        styles: before_styles.map(|styles| &styles.0),
        bookmarks: bookmarks.as_ref().map(|(before, _)| before),
        parts: relationships.map(|(before, _)| before),
        relationships: None,
    };
    let after_side = crate::appearance::Side {
        styles: after_styles.map(|styles| &styles.0),
        bookmarks: bookmarks.as_ref().map(|(_, after)| after),
        parts: relationships.map(|(_, after)| after),
        relationships: None,
    };
    if equality == UntouchedEquality::Appearance {
        crate::appearance::document_for_appearance(&mut before_committed, before_side);
        crate::appearance::document_for_appearance(&mut after_committed, after_side);
    }
    let committed_changes = audit_changes(&before_committed, &after_committed, identity);
    let mut direct_changes = direct_rows(&committed_changes);
    append_hyperlink_metadata_rows(
        &before_committed,
        &after_committed,
        &mut direct_changes,
        identity,
    );
    append_image_attribute_rows(
        &before_committed,
        &after_committed,
        &mut direct_changes,
        identity,
    )?;
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
    if before_committed.document_background != after_committed.document_background {
        direct_changes.push(DirectChange {
            story: StoryScope::Body,
            kind: DirectChangeKind::BackgroundChanged,
            block_id: None,
            old_excerpt: None,
            new_excerpt: None,
            coincides_with_resolution: Vec::new(),
        });
    }
    if let Some((before_facts, after_facts)) = relationships {
        direct_changes.extend(settings_rows(&before_facts.settings, &after_facts.settings));
        direct_changes.extend(theme_rows(
            &before_facts.theme_in_use,
            &after_facts.theme_in_use,
        ));
        // A store value changed under controls that show what they showed:
        // Word replaces their text with the new value on open. Where the
        // controls' text changed too (an edit written through to the
        // store), the change is the controls', compared where they are.
        for old in &before_facts.bound_values {
            if let Some(new) = after_facts
                .bound_values
                .iter()
                .find(|candidate| candidate.binding == old.binding)
                && old.value != new.value
                && old.shown == new.shown
            {
                direct_changes.push(DirectChange {
                    story: StoryScope::Body,
                    kind: DirectChangeKind::BoundValueChanged,
                    block_id: None,
                    old_excerpt: Some(format!("{}: {}", old.binding, old.value)),
                    new_excerpt: Some(format!("{}: {}", new.binding, new.value)),
                    coincides_with_resolution: Vec::new(),
                });
            }
        }
        for (kind, rows) in [
            (
                DirectChangeKind::NoteSeparatorChanged,
                (&before_facts.note_separators, &after_facts.note_separators),
            ),
            (
                DirectChangeKind::FieldStyleTargetChanged,
                (
                    &before_facts.styleref_targets,
                    &after_facts.styleref_targets,
                ),
            ),
        ] {
            let (old_rows, new_rows) = rows;
            for (name, old) in old_rows {
                if let Some((_, new)) = new_rows.iter().find(|(candidate, _)| candidate == name)
                    && old != new
                {
                    direct_changes.push(DirectChange {
                        story: StoryScope::Body,
                        kind,
                        block_id: None,
                        old_excerpt: Some(format!("{name}: {old}")),
                        new_excerpt: Some(format!("{name}: {new}")),
                        coincides_with_resolution: Vec::new(),
                    });
                }
            }
        }
        for font in &before_facts.embedded_fonts {
            if !after_facts.embedded_fonts.contains(font) {
                direct_changes.push(DirectChange {
                    story: StoryScope::Body,
                    kind: DirectChangeKind::EmbeddedFontDropped,
                    block_id: None,
                    old_excerpt: Some(font.clone()),
                    new_excerpt: None,
                    coincides_with_resolution: Vec::new(),
                });
            }
        }
    }
    // A comment is its text and who wrote it when; the text is compared
    // with the stories, the attribution here.
    // On the documents as they stand: a comment whose anchor is itself a
    // pending insertion is gone from the rejected reading, yet shown.
    for before_comment in &before.comments {
        let Some(after_comment) = after
            .comments
            .iter()
            .find(|candidate| candidate.id == before_comment.id)
        else {
            continue;
        };
        let attribution = |document: &CanonDoc, comment: &crate::domain::CommentStory| {
            format!(
                "author={:?} initials={:?} date={:?} resolved={:?} reply_to={:?}",
                comment.author,
                comment.initials,
                comment.date,
                comment_resolved(document, comment),
                comment_parent(document, comment)
            )
        };
        let (old, new) = (
            attribution(before, before_comment),
            attribution(after, after_comment),
        );
        if old != new {
            direct_changes.push(DirectChange {
                story: StoryScope::Comment {
                    id: before_comment.id.clone(),
                },
                kind: DirectChangeKind::CommentAttributionChanged,
                block_id: None,
                old_excerpt: Some(old),
                new_excerpt: Some(new),
                coincides_with_resolution: Vec::new(),
            });
        }
    }
    annotate_resolutions(&mut direct_changes, &preexisting_revisions);

    // Section 3: the untouched proof, over the RAW documents (pending
    // revisions and all): the raw diff explains every sequence difference;
    // whatever it leaves unmentioned must pair 1:1 and be structurally
    // identical, except pairs a census row already accounts for.
    let raw_changes = audit_changes(before, after, identity);
    let untouched = untouched_proof(
        before,
        after,
        &Equality {
            mode: equality,
            before: before_side,
            after: after_side,
        },
        &raw_changes,
        &direct_changes,
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
    if let Some((opaque_id, reason)) = first_unresolvable_opaque_with_revisions(doc) {
        return Err(RuntimeError {
            code: ErrorCode::UnsupportedEdit,
            message: format!(
                "audit refused: {side} document's opaque '{opaque_id}' cannot resolve its tracked state: {reason}"
            ),
            details: ErrorDetails::default(),
        });
    }
    Ok(())
}

/// Identities `after` states differently for revisions `before` already
/// held, as after identity -> before identity.
///
/// A revision's identity is derived from its record and, for a change with
/// no inserted text, from the paragraph that carries it: the paragraph's own
/// id where it has one. A program that gives every paragraph an id when it
/// saves therefore shifts those identities without touching the revisions.
/// A `before` identity `after` does not hold and an `after` identity
/// `before` does not hold name the same revision when everything their
/// records state is equal: story, kind, author, date and content, row for
/// row. They pair in document order.
///
/// Where the revision sits is not decided here. The blocks are compared
/// afterwards under the paired identities, and so are the accepted and
/// rejected readings; a revision that moved or changed shows there.
fn restated_identities(before: &[RevisionRecord], after: &[RevisionRecord]) -> HashMap<u32, u32> {
    type Statement = Vec<(
        StoryScope,
        RevisionKind,
        Option<String>,
        Option<String>,
        String,
    )>;
    fn statements(records: &[RevisionRecord], absent_from: &HashSet<u32>) -> Vec<(u32, Statement)> {
        let mut grouped: Vec<(u32, Statement)> = Vec::new();
        for record in records {
            if record.revision_id == 0 || absent_from.contains(&record.revision_id) {
                continue;
            }
            let row = (
                record.location.clone(),
                record.kind,
                record.author.clone(),
                record.date.clone(),
                record.excerpt.clone(),
            );
            match grouped
                .iter_mut()
                .find(|(identity, _)| *identity == record.revision_id)
            {
                Some((_, rows)) => rows.push(row),
                None => grouped.push((record.revision_id, vec![row])),
            }
        }
        grouped
    }
    let before_identities: HashSet<u32> = before.iter().map(|r| r.revision_id).collect();
    let after_identities: HashSet<u32> = after.iter().map(|r| r.revision_id).collect();
    let orphaned = statements(before, &after_identities);
    let unknown = statements(after, &before_identities);
    let mut taken = vec![false; orphaned.len()];
    let mut restated = HashMap::new();
    for (after_identity, statement) in unknown {
        let same = orphaned
            .iter()
            .enumerate()
            .position(|(i, (_, candidate))| !taken[i] && *candidate == statement);
        if let Some(i) = same {
            taken[i] = true;
            restated.insert(after_identity, orphaned[i].0);
        }
    }
    restated
}

/// One row per document setting that differs between the two sides (each
/// side's settings in canonical form, sorted by name).
fn settings_rows(before: &[(String, String)], after: &[(String, String)]) -> Vec<DirectChange> {
    let names: std::collections::BTreeSet<&String> =
        before.iter().chain(after).map(|(name, _)| name).collect();
    let spelling = |settings: &[(String, String)], name: &String| -> Option<String> {
        let values: Vec<&str> = settings
            .iter()
            .filter(|(candidate, _)| candidate == name)
            .map(|(_, value)| value.as_str())
            .collect();
        (!values.is_empty()).then(|| values.join(""))
    };
    names
        .into_iter()
        .filter_map(|name| {
            let (old, new) = (spelling(before, name), spelling(after, name));
            (old != new).then(|| DirectChange {
                story: StoryScope::Body,
                kind: DirectChangeKind::SettingsChanged,
                block_id: None,
                old_excerpt: old,
                new_excerpt: new,
                coincides_with_resolution: Vec::new(),
            })
        })
        .collect()
}

/// Whether a comment's thread is marked resolved (`w15:done` of the thread
/// root's `w15:commentEx`, MS-DOCX §2.5.1); `None` when the comment has no
/// record.
fn comment_resolved(document: &CanonDoc, comment: &crate::domain::CommentStory) -> Option<bool> {
    let record = |para_id: &str| {
        document
            .comments_extended
            .iter()
            .find(|record| record.para_id == para_id)
    };
    let mut current = record(comment.last_para_id()?)?;
    // Walk to the thread root; a cycle in the parent links stops the walk.
    for _ in 0..document.comments_extended.len() {
        match current.para_id_parent.as_deref().and_then(record) {
            Some(parent) => current = parent,
            None => break,
        }
    }
    Some(current.done)
}

/// The id of the comment a comment replies to (`w15:paraIdParent` of its
/// `w15:commentEx`, MS-DOCX §2.5.1, naming the parent's last paragraph);
/// `None` for a comment that starts a thread or has no record.
fn comment_parent(document: &CanonDoc, comment: &crate::domain::CommentStory) -> Option<String> {
    let para_id = comment.last_para_id()?;
    let record = document
        .comments_extended
        .iter()
        .find(|record| record.para_id == para_id)?;
    let parent = record.para_id_parent.as_deref()?;
    Some(
        document
            .comments
            .iter()
            .find(|candidate| candidate.last_para_id() == Some(parent))
            .map_or_else(
                || format!("paragraph {parent}"),
                |candidate| candidate.id.clone(),
            ),
    )
}

/// One row per theme entry both sides use whose value differs. An entry
/// only one side uses is that side's content using it, compared where the
/// content is.
fn theme_rows(before: &[(String, String)], after: &[(String, String)]) -> Vec<DirectChange> {
    before
        .iter()
        .filter_map(|(name, old)| {
            let (_, new) = after.iter().find(|(candidate, _)| candidate == name)?;
            (old != new).then(|| DirectChange {
                story: StoryScope::Body,
                kind: DirectChangeKind::ThemeChanged,
                block_id: None,
                old_excerpt: Some(format!("{name}: {old}")),
                new_excerpt: Some(format!("{name}: {new}")),
                coincides_with_resolution: Vec::new(),
            })
        })
        .collect()
}

/// Per header or footer part name `after` spells differently from the one
/// `before` gives the same part, `before`'s spelling. A name is a
/// relationship target: absolute (`/word/header1.xml`) or relative to the
/// main part's folder.
fn story_part_renames(
    before: &CanonDoc,
    after: &CanonDoc,
    before_main: &str,
    after_main: &str,
) -> HashMap<String, String> {
    fn resolved(main: &str, name: &str) -> String {
        match name.strip_prefix('/') {
            Some(absolute) => crate::docx_package::normalize_package_path(absolute),
            None => crate::docx_package::normalize_package_path(&format!(
                "{}{name}",
                crate::docx_package::part_dir(main)
            )),
        }
    }
    let before_names: HashMap<String, &String> = before
        .headers
        .iter()
        .map(|story| &story.part_name)
        .chain(before.footers.iter().map(|story| &story.part_name))
        .map(|name| (resolved(before_main, name), name))
        .collect();
    after
        .headers
        .iter()
        .map(|story| &story.part_name)
        .chain(after.footers.iter().map(|story| &story.part_name))
        .filter_map(|name| {
            let before_name = before_names.get(&resolved(after_main, name))?;
            (*before_name != name).then(|| (name.clone(), (*before_name).clone()))
        })
        .collect()
}

/// Which family of notes an id belongs to: footnotes and endnotes number
/// separately (§17.11.10, §17.11.7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum NoteFamily {
    Footnote,
    Endnote,
}

/// Per family, the `after` note ids to read as `before` ids: the note each
/// side references n-th, in body order, is the same note. Empty for a
/// family whose references do not pair one to one (a reference added or
/// removed, an id referenced twice, a renamed id that would collide with a
/// note left unrenamed): its notes are then compared by id, as written.
fn note_renames(before: &CanonDoc, after: &CanonDoc) -> HashMap<(NoteFamily, String), String> {
    let mut renames = HashMap::new();
    for family in [NoteFamily::Footnote, NoteFamily::Endnote] {
        let (Some(before_references), Some(after_references)) = (
            note_references(before, family),
            note_references(after, family),
        ) else {
            continue;
        };
        if before_references.len() != after_references.len() {
            continue;
        }
        let distinct = |references: &[String]| {
            references.iter().collect::<HashSet<_>>().len() == references.len()
        };
        if !distinct(&before_references) || !distinct(&after_references) {
            continue;
        }
        let after_story_ids: Vec<&String> = match family {
            NoteFamily::Footnote => after.footnotes.iter().map(|story| &story.id).collect(),
            NoteFamily::Endnote => after.endnotes.iter().map(|story| &story.id).collect(),
        };
        let referenced: HashSet<&String> = after_references.iter().collect();
        let collides = before_references.iter().any(|target| {
            after_story_ids
                .iter()
                .any(|id| *id == target && !referenced.contains(id))
        });
        if collides {
            continue;
        }
        for (after_id, before_id) in after_references.into_iter().zip(before_references) {
            if after_id != before_id {
                renames.insert((family, after_id), before_id);
            }
        }
    }
    renames
}

/// The ids of a family's note references in the body, in document order.
/// A reference held inside an opaque block (its bytes are compared as they
/// are, so its id cannot be read as another) yields `None`: the family's
/// notes are then compared by id.
fn note_references(doc: &CanonDoc, family: NoteFamily) -> Option<Vec<String>> {
    fn collect(block: &BlockNode, family: NoteFamily, out: &mut Vec<String>) -> Option<()> {
        match block {
            BlockNode::Paragraph(paragraph) => {
                for inline in paragraph.segments.iter().flat_map(|s| &s.inlines) {
                    if let Some((found, reference)) = note_reference(inline)
                        && found == family
                    {
                        out.push(reference.reference_id.clone());
                    }
                }
            }
            BlockNode::Table(table) => {
                for block in table
                    .rows
                    .iter()
                    .flat_map(|row| &row.cells)
                    .flat_map(|cell| &cell.blocks)
                {
                    collect(block, family, out)?;
                }
            }
            BlockNode::OpaqueBlock(opaque) => {
                let held = match family {
                    NoteFamily::Footnote => &opaque.note_references.footnotes,
                    NoteFamily::Endnote => &opaque.note_references.endnotes,
                };
                if !held.is_empty() {
                    return None;
                }
            }
        }
        Some(())
    }
    let mut out = Vec::new();
    for block in &doc.blocks {
        collect(&block.block, family, &mut out)?;
    }
    Some(out)
}

fn note_reference(
    inline: &crate::domain::InlineNode,
) -> Option<(NoteFamily, &crate::domain::NoteReferenceData)> {
    use crate::domain::{InlineNode, OpaqueKind};
    match inline {
        InlineNode::OpaqueInline(opaque) => match &opaque.kind {
            OpaqueKind::FootnoteReference(reference) => Some((NoteFamily::Footnote, reference)),
            OpaqueKind::EndnoteReference(reference) => Some((NoteFamily::Endnote, reference)),
            _ => None,
        },
        _ => None,
    }
}

/// `doc` with its notes and their body references renamed.
fn apply_note_renames(doc: &mut CanonDoc, renames: &HashMap<(NoteFamily, String), String>) {
    use crate::domain::{InlineNode, OpaqueKind};
    fn rename_in(block: &mut BlockNode, renames: &HashMap<(NoteFamily, String), String>) {
        match block {
            BlockNode::Paragraph(paragraph) => {
                for inline in paragraph.segments.iter_mut().flat_map(|s| &mut s.inlines) {
                    let InlineNode::OpaqueInline(opaque) = inline else {
                        continue;
                    };
                    let (family, reference) = match &mut opaque.kind {
                        OpaqueKind::FootnoteReference(reference) => {
                            (NoteFamily::Footnote, reference)
                        }
                        OpaqueKind::EndnoteReference(reference) => (NoteFamily::Endnote, reference),
                        _ => continue,
                    };
                    if let Some(id) = renames.get(&(family, reference.reference_id.clone())) {
                        reference.reference_id = id.clone();
                    }
                }
            }
            BlockNode::Table(table) => {
                for block in table
                    .rows
                    .iter_mut()
                    .flat_map(|row| &mut row.cells)
                    .flat_map(|cell| &mut cell.blocks)
                {
                    rename_in(block, renames);
                }
            }
            BlockNode::OpaqueBlock(_) => {}
        }
    }
    for block in &mut doc.blocks {
        rename_in(&mut block.block, renames);
    }
    for story in &mut doc.footnotes {
        if let Some(id) = renames.get(&(NoteFamily::Footnote, story.id.clone())) {
            story.id = id.clone();
        }
    }
    for story in &mut doc.endnotes {
        if let Some(id) = renames.get(&(NoteFamily::Endnote, story.id.clone())) {
            story.id = id.clone();
        }
    }
}

fn rename_note_scope(scope: &mut StoryScope, renames: &HashMap<(NoteFamily, String), String>) {
    let (family, id) = match scope {
        StoryScope::Footnote { id } => (NoteFamily::Footnote, id),
        StoryScope::Endnote { id } => (NoteFamily::Endnote, id),
        _ => return,
    };
    if let Some(renamed) = renames.get(&(family, id.clone())) {
        *id = renamed.clone();
    }
}

/// The identity of a revision record across two enumerations. `revision_id` is
/// engine-minted from the canonical revision record and is the semantic key;
/// raw OOXML `w:id` is diagnostic only and never enters correspondence. The
/// attribution the identity does not already cover rides along: the author,
/// and the explicit UTC date (`w16du:dateUtc`), so a result that rewrote a
/// pre-existing revision's UTC date does not correspond to it, exactly as one
/// that rewrote its author or `w:date` (which the identity covers).
type CensusKey = (
    StoryScope,
    u32,
    RevisionKind,
    Option<String>,
    Option<String>,
);

fn census_key(r: &RevisionRecord) -> CensusKey {
    (
        r.location.clone(),
        r.revision_id,
        r.kind,
        r.author
            .as_ref()
            .filter(|author| !author.is_empty())
            .cloned(),
        r.date_utc.clone(),
    )
}

/// Whether two records with one identity hold the same content. A
/// paragraph-mark revision's content is the paragraph break itself; its
/// excerpt also names the paragraph's text for the reader, which an edit of
/// that text changes without touching the break.
fn same_revision_content(after: &RevisionRecord, before: &RevisionRecord) -> bool {
    use crate::tracked_model::is_paragraph_mark_record;
    if is_paragraph_mark_record(before) || is_paragraph_mark_record(after) {
        return is_paragraph_mark_record(before) && is_paragraph_mark_record(after);
    }
    after.excerpt == before.excerpt
}

/// Match `before`'s records against `after`'s by semantic identity, in document
/// order. One user intention can have several census rows under the same
/// identity (for example content and paragraph-mark carriers), so rows pair as
/// a multiset: exact-content matches first, then modified leftovers.
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
                .find(|&i| !consumed[i] && same_revision_content(&after[i], &record))
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

    // An edit inside a pre-existing revision can split it into more rows than
    // it had (Bob deleting a word inside Alice's insertion leaves her text on
    // both sides of his change). A leftover row under a pre-existing identity
    // is a piece of that revision, never a new one: fold it into the
    // revision, which was edited inside.
    for (i, record) in after.iter().enumerate() {
        if consumed[i] {
            continue;
        }
        let key = census_key(record);
        let Some(prior) = preexisting.iter_mut().rev().find(|prior| {
            census_key(&prior.record) == key
                && !matches!(prior.disposition, RevisionDisposition::Resolved)
        }) else {
            continue;
        };
        consumed[i] = true;
        let pieces = match &prior.disposition {
            RevisionDisposition::Modified { after_excerpt } => after_excerpt.clone(),
            _ => prior.record.excerpt.clone(),
        };
        prior.disposition = RevisionDisposition::Modified {
            after_excerpt: format!("{pieces}{}", record.excerpt),
        };
    }

    let new_revisions = after
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !consumed[*i])
        .map(|(_, r)| r)
        .collect();
    (new_revisions, preexisting)
}

/// Turn each `Modified` pre-existing revision that the new revisions fully
/// explain into `EditedInside`. The proof is a projection, not a heuristic:
/// reject exactly the selectable revisions new in `after`, re-run the census
/// against `before`, and a row whose content comes back identical was changed
/// only by those revisions. Identical content may come back in several pieces
/// (the stacked edit split one wrapper into two, and rejecting it leaves both
/// halves): the pieces, read in document order, are compared as one.
///
/// Fail closed: a row is promoted only on that proof. When there is nothing
/// selectable to reject, or the selective resolver refuses the selection
/// (a shared table grid, a blocked join), every row keeps `Modified`, which
/// every delivery policy treats as a finding. Sealed-block revisions are not
/// in the `CanonDoc`, so they are not rejected; their records ride along
/// unchanged on both sides of the proof census.
fn classify_edited_inside(
    before_records: Vec<RevisionRecord>,
    after: &CanonDoc,
    after_styles: Option<&StyleTable>,
    sealed_after: Vec<RevisionRecord>,
    new_revisions: &[RevisionRecord],
    preexisting: &mut [PreexistingRevision],
) {
    if !preexisting
        .iter()
        .any(|p| matches!(p.disposition, RevisionDisposition::Modified { .. }))
    {
        return;
    }
    let selectable: HashSet<u32> = enumerate_revisions(after)
        .iter()
        .map(|r| r.revision_id)
        .filter(|&id| id != 0)
        .collect();
    let new_ids: HashSet<u32> = new_revisions
        .iter()
        .map(|r| r.revision_id)
        .filter(|id| selectable.contains(id))
        .collect();
    if new_ids.is_empty() {
        return;
    }
    let mut restored = after.clone();
    if resolve_selected_revisions_with_styles(
        &mut restored,
        &new_ids,
        ResolveSelectionAction::Reject,
        after_styles,
    )
    .is_err()
    {
        return;
    }
    let mut restored_records = enumerate_revisions(&restored);
    restored_records.extend(sealed_after);
    let (_, proof) = match_census(before_records, restored_records);
    assert_eq!(
        proof.len(),
        preexisting.len(),
        "the proof census runs over the same before records"
    );
    for (row, proof_row) in preexisting.iter_mut().zip(proof) {
        debug_assert_eq!(row.record, proof_row.record);
        let restored = match &proof_row.disposition {
            RevisionDisposition::Untouched => true,
            RevisionDisposition::Modified { after_excerpt } => {
                *after_excerpt == proof_row.record.excerpt
            }
            RevisionDisposition::EditedInside { .. } | RevisionDisposition::Resolved => false,
        };
        if let RevisionDisposition::Modified { after_excerpt } = &row.disposition
            && restored
        {
            row.disposition = RevisionDisposition::EditedInside {
                after_excerpt: after_excerpt.clone(),
            };
        }
    }
}

/// Flatten a committed-projection diff into direct-change rows. Story
/// `Modified` variants recurse into their block-level changes under the
/// story's scope; story insert/delete become single story-level rows.
fn direct_rows(changes: &[AuditDelta]) -> Vec<DirectChange> {
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

fn push_direct_rows(rows: &mut Vec<DirectChange>, change: &AuditDelta, story: &StoryScope) {
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
        AuditDelta::BlockDeleted {
            block_id, old_text, ..
        } => rows.push(row(
            DirectChangeKind::BlockDeleted,
            Some(block_id.clone()),
            Some(old_text.clone()),
            None,
        )),
        AuditDelta::BlockInserted { block, .. } => rows.push(row(
            DirectChangeKind::BlockInserted,
            Some(block_node_id(block)),
            None,
            Some(extract_block_text_for_hash(block)),
        )),
        AuditDelta::BlockModified {
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
        AuditDelta::TableStructureChanged {
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

        AuditDelta::HeaderModified {
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
        AuditDelta::HeaderDeleted {
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
        AuditDelta::HeaderInserted {
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

        AuditDelta::FooterModified {
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
        AuditDelta::FooterDeleted {
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
        AuditDelta::FooterInserted {
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

        AuditDelta::FootnoteModified {
            id, block_changes, ..
        } => {
            let scope = StoryScope::Footnote { id: id.clone() };
            for inner in block_changes {
                push_direct_rows(rows, inner, &scope);
            }
        }
        AuditDelta::FootnoteDeleted { id, blocks, .. } => rows.push(DirectChange {
            story: StoryScope::Footnote { id: id.clone() },
            kind: DirectChangeKind::StoryDeleted,
            block_id: None,
            old_excerpt: Some(blocks_text(blocks)),
            new_excerpt: None,
            coincides_with_resolution: Vec::new(),
        }),
        AuditDelta::FootnoteInserted { id, blocks, .. } => rows.push(DirectChange {
            story: StoryScope::Footnote { id: id.clone() },
            kind: DirectChangeKind::StoryInserted,
            block_id: None,
            old_excerpt: None,
            new_excerpt: Some(blocks_text(blocks)),
            coincides_with_resolution: Vec::new(),
        }),

        AuditDelta::EndnoteModified {
            id, block_changes, ..
        } => {
            let scope = StoryScope::Endnote { id: id.clone() };
            for inner in block_changes {
                push_direct_rows(rows, inner, &scope);
            }
        }
        AuditDelta::EndnoteDeleted { id, blocks, .. } => rows.push(DirectChange {
            story: StoryScope::Endnote { id: id.clone() },
            kind: DirectChangeKind::StoryDeleted,
            block_id: None,
            old_excerpt: Some(blocks_text(blocks)),
            new_excerpt: None,
            coincides_with_resolution: Vec::new(),
        }),
        AuditDelta::EndnoteInserted { id, blocks, .. } => rows.push(DirectChange {
            story: StoryScope::Endnote { id: id.clone() },
            kind: DirectChangeKind::StoryInserted,
            block_id: None,
            old_excerpt: None,
            new_excerpt: Some(blocks_text(blocks)),
            coincides_with_resolution: Vec::new(),
        }),

        AuditDelta::CommentModified {
            id, block_changes, ..
        } => {
            let scope = StoryScope::Comment { id: id.clone() };
            for inner in block_changes {
                push_direct_rows(rows, inner, &scope);
            }
        }
        AuditDelta::CommentDeleted { id, blocks, .. } => rows.push(DirectChange {
            story: StoryScope::Comment { id: id.clone() },
            kind: DirectChangeKind::StoryDeleted,
            block_id: None,
            old_excerpt: Some(blocks_text(blocks)),
            new_excerpt: None,
            coincides_with_resolution: Vec::new(),
        }),
        AuditDelta::CommentInserted { id, blocks, .. } => rows.push(DirectChange {
            story: StoryScope::Comment { id: id.clone() },
            kind: DirectChangeKind::StoryInserted,
            block_id: None,
            old_excerpt: None,
            new_excerpt: Some(blocks_text(blocks)),
            coincides_with_resolution: Vec::new(),
        }),
    }
}

/// Account for direct image attributes only after proving that every other
/// field of the paragraph remains unchanged. A failed proof leaves the
/// paragraph to the untouched checker; it does not grant a partial exemption.
fn append_image_attribute_rows(
    before: &CanonDoc,
    after: &CanonDoc,
    rows: &mut Vec<DirectChange>,
    identity: BlockIdentity,
) -> Result<(), RuntimeError> {
    for (old_index, new_index) in
        crate::audit_delta::paired_block_indices(&before.blocks, &after.blocks, identity)
    {
        let old = &before.blocks[old_index];
        let new = &after.blocks[new_index];
        let block_id = block_node_id(&old.block);
        if rows
            .iter()
            .any(|row| row.story == StoryScope::Body && row.block_id.as_ref() == Some(&block_id))
        {
            continue;
        }
        if crate::audit_image::only_image_attributes_changed(old, new)? {
            rows.push(DirectChange {
                story: StoryScope::Body,
                kind: DirectChangeKind::BlockModified,
                block_id: Some(block_id),
                old_excerpt: Some(extract_block_text_for_hash(&old.block)),
                new_excerpt: Some(extract_block_text_for_hash(&new.block)),
                coincides_with_resolution: Vec::new(),
            });
        }
    }
    Ok(())
}

/// Hyperlink relationship retargeting changes committed metadata while leaving
/// display text unchanged. The semantic text diff deliberately treats that
/// paragraph as matched, so add the missing audit row explicitly from the typed
/// hyperlink model instead of letting the untouched proof report a blind spot.
fn append_hyperlink_metadata_rows(
    before: &CanonDoc,
    after: &CanonDoc,
    rows: &mut Vec<DirectChange>,
    identity: BlockIdentity,
) {
    for (old_index, new_index) in
        crate::audit_delta::paired_block_indices(&before.blocks, &after.blocks, identity)
    {
        let before_block = &before.blocks[old_index];
        let after_block = &after.blocks[new_index];
        let block_id = block_node_id(&before_block.block);
        if hyperlink_targets(&before_block.block) == hyperlink_targets(&after_block.block) {
            continue;
        }
        if rows
            .iter()
            .any(|row| row.story == StoryScope::Body && row.block_id.as_ref() == Some(&block_id))
        {
            continue;
        }
        rows.push(DirectChange {
            story: StoryScope::Body,
            kind: DirectChangeKind::BlockModified,
            block_id: Some(block_id),
            old_excerpt: Some(extract_block_text_for_hash(&before_block.block)),
            new_excerpt: Some(extract_block_text_for_hash(&after_block.block)),
            coincides_with_resolution: Vec::new(),
        });
    }
}

/// A block's hyperlink identities in document order: TARGET (url + anchor)
/// per hyperlink. Deliberately NOT the inline `NodeId` — that id is minted
/// from a document-global counter and renumbers whenever any earlier inline
/// content shifts, so comparing it flags untouched hyperlink paragraphs
/// after a legitimate tracked edit elsewhere. Document order plus the
/// per-block pairing the caller already does carries the positional
/// identity.
fn hyperlink_targets(block: &BlockNode) -> Vec<(Option<String>, Option<String>)> {
    let mut targets = Vec::new();
    collect_hyperlink_targets(block, &mut targets);
    targets
}

fn collect_hyperlink_targets(
    block: &BlockNode,
    targets: &mut Vec<(Option<String>, Option<String>)>,
) {
    match block {
        BlockNode::Paragraph(paragraph) => {
            for segment in &paragraph.segments {
                for inline in &segment.inlines {
                    if let InlineNode::OpaqueInline(opaque) = inline
                        && let OpaqueKind::Hyperlink(link) = &opaque.kind
                    {
                        targets.push((link.url.clone(), link.anchor.clone()));
                    }
                }
            }
        }
        BlockNode::Table(table) => {
            for row in &table.rows {
                for cell in &row.cells {
                    for nested in &cell.blocks {
                        collect_hyperlink_targets(nested, targets);
                    }
                }
            }
        }
        BlockNode::OpaqueBlock(_) => {}
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

/// Per story key (a header's or footer's part name, a note's or comment's
/// id): the blocks its own inner changes account for, before then after.
type StoryRemovals = HashMap<String, (HashSet<NodeId>, HashSet<NodeId>)>;

/// A block-level location a census row implicates, used to exempt its PAIR
/// from the proof (the change is accounted for in section 1, so the pair is
/// neither "verified untouched" nor a violation).
type Implicated = HashSet<(StoryScope, NodeId)>;

/// How the untouched proof decides that a pair of blocks is the same.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum UntouchedEquality {
    /// The same markup, under the round-trip fidelity comparison: the
    /// question for a document this engine wrote.
    #[default]
    Fidelity,
    /// The same text and the same resolved formatting, whatever the
    /// bookkeeping and the run boundaries (`crate::appearance`): the
    /// question for a document another program has saved.
    Appearance,
}

/// The equality of one audit, with what its appearance form needs from each
/// document.
struct Equality<'a> {
    mode: UntouchedEquality,
    before: crate::appearance::Side<'a>,
    after: crate::appearance::Side<'a>,
}

impl Equality<'_> {
    fn differences(
        &self,
        before: &TrackedBlock,
        after: &TrackedBlock,
        story: &StoryScope,
    ) -> Vec<crate::roundtrip_compare::Difference> {
        let before_side = self.before.for_story(story);
        let after_side = self.after.for_story(story);
        let mut differences = match self.mode {
            UntouchedEquality::Fidelity => crate::roundtrip_compare::compare_tracked_block_pair(
                &crate::appearance::with_relationship_targets(before, before_side),
                &crate::appearance::with_relationship_targets(after, after_side),
            ),
            UntouchedEquality::Appearance => crate::roundtrip_compare::compare_tracked_block_pair(
                &crate::appearance::for_appearance(before, before_side),
                &crate::appearance::for_appearance(after, after_side),
            ),
        };
        // What a list reference resolves to lives in the numbering part,
        // outside the block: compared under either equality.
        differences.extend(crate::appearance::numbering_differences(
            before,
            after,
            before_side,
            after_side,
        ));
        // What run formatting the styles part gives text the block holds
        // unresolved: compared under either equality.
        differences.extend(crate::appearance::style_differences(
            before,
            after,
            before_side,
            after_side,
        ));
        differences.extend(crate::appearance::table_style_differences(
            before,
            after,
            before_side,
            after_side,
        ));
        differences
    }
}

fn untouched_proof(
    before: &CanonDoc,
    after: &CanonDoc,
    equality: &Equality<'_>,
    raw_changes: &[AuditDelta],
    direct_changes: &[DirectChange],
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
        note_move_carrier_implications(after, r, &mut implicated_after);
        note_text_frame_host_implications(after, r, &mut implicated_after);
    }
    let mut implicated_before: Implicated = HashSet::new();
    for p in preexisting {
        if !matches!(p.disposition, RevisionDisposition::Untouched) {
            note_implication(&mut implicated_before, &mut implicated_stories, &p.record);
            note_move_carrier_implications(before, &p.record, &mut implicated_before);
            note_text_frame_host_implications(before, &p.record, &mut implicated_before);
            if matches!(
                p.disposition,
                RevisionDisposition::Modified { .. } | RevisionDisposition::EditedInside { .. }
            ) {
                note_move_carrier_implications(after, &p.record, &mut implicated_after);
                note_text_frame_host_implications(after, &p.record, &mut implicated_after);
            }
        }
    }
    for change in direct_changes {
        if let Some(block_id) = &change.block_id {
            implicated_before.insert((change.story.clone(), block_id.clone()));
            implicated_after.insert((change.story.clone(), block_id.clone()));
        }
    }

    // Comment stories and their body anchors are one annotation. The raw diff
    // reports the story insertion/deletion, but the zero-width range markers
    // added to or removed from the body paragraph are not a separate block
    // change. Account for those markers here so the untouched proof does not
    // misreport the correctly anchored paragraph as an unexplained mutation.
    for change in raw_changes {
        match change {
            AuditDelta::CommentInserted { id, .. } => {
                implicate_comment_anchor_blocks(after, id, &mut implicated_after);
            }
            AuditDelta::CommentDeleted { id, .. } => {
                implicate_comment_anchor_blocks(before, id, &mut implicated_before);
            }
            _ => {}
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
        equality,
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
    let mut header_removed: StoryRemovals = HashMap::new();
    let mut footer_removed: StoryRemovals = HashMap::new();
    let mut note_removed: HashMap<&'static str, StoryRemovals> = HashMap::new();
    for change in raw_changes {
        match change {
            // A modified story is not exempt as a whole: the blocks its
            // inner changes name are accounted for there, and every other
            // block of the story must still pair and compare equal. (Two
            // independent parses never hold equal blocks, so every story
            // arrives here as modified; exempting it would leave a change
            // the text diff does not see, such as formatting or the text
            // of a link or a text box, unexamined.)
            AuditDelta::HeaderModified {
                base_part_name,
                block_changes,
                ..
            } => {
                header_removed.insert(base_part_name.clone(), raw_removed_block_ids(block_changes));
            }
            AuditDelta::HeaderDeleted { part_name, .. } => {
                header_touched_before.insert(part_name.clone());
            }
            AuditDelta::HeaderInserted { part_name, .. } => {
                header_touched_after.insert(part_name.clone());
            }
            AuditDelta::FooterModified {
                base_part_name,
                block_changes,
                ..
            } => {
                footer_removed.insert(base_part_name.clone(), raw_removed_block_ids(block_changes));
            }
            AuditDelta::FooterDeleted { part_name, .. } => {
                footer_touched_before.insert(part_name.clone());
            }
            AuditDelta::FooterInserted { part_name, .. } => {
                footer_touched_after.insert(part_name.clone());
            }
            AuditDelta::FootnoteModified { id, block_changes } => {
                note_removed
                    .entry("footnotes")
                    .or_default()
                    .insert(id.clone(), raw_removed_block_ids(block_changes));
            }
            AuditDelta::FootnoteDeleted { id, .. } | AuditDelta::FootnoteInserted { id, .. } => {
                note_touched
                    .entry("footnotes")
                    .or_default()
                    .insert(id.clone());
            }
            AuditDelta::EndnoteModified { id, block_changes } => {
                note_removed
                    .entry("endnotes")
                    .or_default()
                    .insert(id.clone(), raw_removed_block_ids(block_changes));
            }
            AuditDelta::EndnoteDeleted { id, .. } | AuditDelta::EndnoteInserted { id, .. } => {
                note_touched
                    .entry("endnotes")
                    .or_default()
                    .insert(id.clone());
            }
            AuditDelta::CommentModified { id, block_changes } => {
                note_removed
                    .entry("comments")
                    .or_default()
                    .insert(id.clone(), raw_removed_block_ids(block_changes));
            }
            AuditDelta::CommentDeleted { id, .. } | AuditDelta::CommentInserted { id, .. } => {
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
            &header_removed,
            &implicated_stories,
            &implicated_before,
            &implicated_after,
            equality,
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
            &footer_removed,
            &implicated_stories,
            &implicated_before,
            &implicated_after,
            equality,
            &mut verified,
            &mut violations,
        );
    }
    // Footnotes / endnotes / comments (keyed by id; insert/delete touched
    // sets are shared per family since ids identify both sides).
    {
        let touched = note_touched.remove("footnotes").unwrap_or_default();
        let removed = note_removed.remove("footnotes").unwrap_or_default();
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
            &removed,
            &implicated_stories,
            &implicated_before,
            &implicated_after,
            equality,
            &mut verified,
            &mut violations,
        );
    }
    {
        let touched = note_touched.remove("endnotes").unwrap_or_default();
        let removed = note_removed.remove("endnotes").unwrap_or_default();
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
            &removed,
            &implicated_stories,
            &implicated_before,
            &implicated_after,
            equality,
            &mut verified,
            &mut violations,
        );
    }
    {
        let touched = note_touched.remove("comments").unwrap_or_default();
        let removed = note_removed.remove("comments").unwrap_or_default();
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
            &removed,
            &implicated_stories,
            &implicated_before,
            &implicated_after,
            equality,
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

fn implicate_comment_anchor_blocks(doc: &CanonDoc, comment_id: &str, out: &mut Implicated) {
    for block in &doc.blocks {
        if block_contains_comment_anchor(&block.block, comment_id) {
            out.insert((StoryScope::Body, block_node_id(&block.block)));
        }
    }
}

fn block_contains_comment_anchor(block: &BlockNode, comment_id: &str) -> bool {
    match block {
        BlockNode::Paragraph(paragraph) => paragraph.segments.iter().any(|segment| {
            segment.inlines.iter().any(|inline| {
                matches!(
                    inline,
                    InlineNode::CommentRangeStart { id }
                        | InlineNode::CommentRangeEnd { id }
                        if id == comment_id
                )
            })
        }),
        BlockNode::Table(table) => table.rows.iter().any(|row| {
            row.cells.iter().any(|cell| {
                cell.blocks
                    .iter()
                    .any(|nested| block_contains_comment_anchor(nested, comment_id))
            })
        }),
        BlockNode::OpaqueBlock(_) => false,
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

/// A textbox revision's census row is attributed to the text frame's own
/// story (`StoryScope::TextFrame`), but the frame lives inside a drawing in a
/// paragraph of the hosting story, and the untouched proof pairs that host
/// paragraph. Implicate the host so a change inside the textbox (a resolution
/// or an edit there) accounts for its host paragraph instead of reading as an
/// unexplained difference.
///
/// The row names the host paragraph by id but not the host story, so every
/// story with a drawing in a paragraph of that id is implicated. A positional
/// id can repeat across stories (`p_1` in the body and in a header); when both
/// host a drawing, both are exempted, which only narrows the proof to fewer
/// verified pairs and never hides a block the census does not account for in
/// some story.
///
/// Named limit: a textbox's interior is outside the committed (reject-all)
/// delta, so once its host is implicated, an untracked edit elsewhere in the
/// same textbox is not reported by this proof either. Before, every change
/// inside a textbox failed the proof, supported ones included.
fn note_text_frame_host_implications(
    doc: &CanonDoc,
    record: &RevisionRecord,
    blocks: &mut Implicated,
) {
    if !matches!(record.location, StoryScope::TextFrame { .. }) {
        return;
    }
    crate::opaque_targets::visit_opaque_interiors(doc, &mut |host, location, interior| {
        if let crate::opaque_targets::OpaqueInteriorRef::Inline(opaque) = interior
            && matches!(opaque.kind, OpaqueKind::Drawing)
            && *host == record.block_id
        {
            blocks.insert((location.clone(), host.clone()));
        }
    });
}

/// A move census row represents every source and destination carrier sharing
/// one engine identity. The representative row names only the first carrier
/// in document order, so the untouched proof must recover and implicate the
/// complete carrier set from the canonical document.
fn note_move_carrier_implications(
    doc: &CanonDoc,
    record: &RevisionRecord,
    blocks: &mut Implicated,
) {
    if record.kind != RevisionKind::Move {
        return;
    }

    let mut note_story = |story: StoryScope, story_blocks: &[TrackedBlock]| {
        if story != record.location {
            return;
        }
        for block in story_blocks {
            if block.move_id.is_some()
                && tracked_block_has_revision_identity(block, record.revision_id)
            {
                blocks.insert((story.clone(), block_node_id(&block.block)));
            }
        }
    };

    note_story(StoryScope::Body, &doc.blocks);
    for header in &doc.headers {
        note_story(
            header_scope(&header.part_name, &header.kind),
            &header.blocks,
        );
    }
    for footer in &doc.footers {
        note_story(
            footer_scope(&footer.part_name, &footer.kind),
            &footer.blocks,
        );
    }
    for footnote in &doc.footnotes {
        note_story(
            StoryScope::Footnote {
                id: footnote.id.clone(),
            },
            &footnote.blocks,
        );
    }
    for endnote in &doc.endnotes {
        note_story(
            StoryScope::Endnote {
                id: endnote.id.clone(),
            },
            &endnote.blocks,
        );
    }
    for comment in &doc.comments {
        note_story(
            StoryScope::Comment {
                id: comment.id.clone(),
            },
            &comment.blocks,
        );
    }
}

fn tracked_block_has_revision_identity(block: &TrackedBlock, identity: u32) -> bool {
    fn status_has_identity(status: &TrackingStatus, identity: u32) -> bool {
        match status {
            TrackingStatus::Normal => false,
            TrackingStatus::Inserted(revision) | TrackingStatus::Deleted(revision) => {
                revision.identity == identity
            }
            TrackingStatus::InsertedThenDeleted(stacked) => {
                stacked.inserted.identity == identity || stacked.deleted.identity == identity
            }
        }
    }

    if status_has_identity(&block.status, identity) {
        return true;
    }
    let BlockNode::Paragraph(paragraph) = &block.block else {
        return false;
    };
    paragraph
        .segments
        .iter()
        .any(|segment| status_has_identity(&segment.status, identity))
        || paragraph
            .para_mark_status
            .as_ref()
            .is_some_and(|status| status_has_identity(status, identity))
}

/// Before-side / after-side block ids the raw diff's BODY-level rows
/// mention. Story-level rows are handled per family.
fn raw_removed_block_ids(changes: &[AuditDelta]) -> (HashSet<NodeId>, HashSet<NodeId>) {
    let mut before_removed = HashSet::new();
    let mut after_removed = HashSet::new();
    for change in changes {
        match change {
            AuditDelta::BlockDeleted { block_id, .. } => {
                before_removed.insert(block_id.clone());
            }
            AuditDelta::BlockInserted { block, .. } => {
                after_removed.insert(block_node_id(block));
            }
            // A modified block stays in the sequence: it pairs with its
            // counterpart and is compared like any other pair, unless a
            // census or direct-change row accounts for it. A raw difference
            // that neither the census nor the committed delta explains (a
            // pending revision moved within its paragraph leaves both
            // unchanged) is the proof's to report.
            // A changed table likewise pairs with its counterpart.
            AuditDelta::BlockModified { .. } | AuditDelta::TableStructureChanged { .. } => {}
            _ => {}
        }
    }
    (before_removed, after_removed)
}

/// The ids of every block nested in a table's cells, at any depth (none for
/// a paragraph or an opaque block).
fn nested_block_ids(block: &BlockNode) -> impl Iterator<Item = NodeId> {
    fn collect(block: &BlockNode, out: &mut Vec<NodeId>) {
        if let BlockNode::Table(table) = block {
            for cell in table.rows.iter().flat_map(|row| &row.cells) {
                for nested in &cell.blocks {
                    out.push(block_node_id(nested));
                    collect(nested, out);
                }
            }
        }
    }
    let mut out = Vec::new();
    collect(block, &mut out);
    out.into_iter()
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
    equality: &Equality<'_>,
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
        // A census row in a table cell names the cell paragraph, while the
        // pair is the top-level table: a row inside implicates the table.
        let exempt = implicated_before.contains(&(story.clone(), b_id.clone()))
            || implicated_after.contains(&(story.clone(), a_id.clone()))
            || nested_block_ids(&b.block)
                .any(|id| implicated_before.contains(&(story.clone(), id)))
            || nested_block_ids(&a.block).any(|id| implicated_after.contains(&(story.clone(), id)));
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
        let mut differences: Vec<String> = equality
            .differences(b, a, story)
            .iter()
            .map(|d| d.to_string())
            .collect();
        if let Some(difference) = sealed_bytes_difference(&b.block, &a.block) {
            differences.push(difference);
        }
        if differences.is_empty() {
            *verified += 1;
        } else {
            let named: Vec<String> = differences.iter().take(3).cloned().collect();
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

/// A body block the model cannot hold is recorded only as the hash of its
/// bytes (`body_item_sha256:…`), which the fidelity comparator skips. Equal
/// hashes prove the block untouched; different ones mean its content may
/// have changed, which the proof must not certify away. (Equivalent content
/// can hash differently, e.g. after a tracked edit inside it is rejected;
/// reporting that is the safe direction.)
fn sealed_bytes_difference(before: &BlockNode, after: &BlockNode) -> Option<String> {
    const PREFIX: &str = "body_item_sha256:";
    let (BlockNode::OpaqueBlock(b), BlockNode::OpaqueBlock(a)) = (before, after) else {
        return None;
    };
    (b.opaque_ref.starts_with(PREFIX)
        && a.opaque_ref.starts_with(PREFIX)
        && b.opaque_ref != a.opaque_ref)
        .then(|| "the preserved block's bytes changed".to_string())
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
    removed: &StoryRemovals,
    implicated_stories: &HashSet<StoryScope>,
    implicated_before: &Implicated,
    implicated_after: &Implicated,
    equality: &Equality<'_>,
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
                    removed.get(&key).map_or(&empty, |(before, _)| before),
                    removed.get(&key).map_or(&empty, |(_, after)| after),
                    implicated_before,
                    implicated_after,
                    equality,
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
