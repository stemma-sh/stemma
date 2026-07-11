//! Footnote / endnote authoring verbs (§17.11). "Add a footnote here; edit
//! footnote 3's text; delete that endnote."
//!
//! A footnote/endnote is two coupled pieces:
//!
//! 1. a **reference run** in the body (`w:footnoteReference` §17.11.3 /
//!    `w:endnoteReference` §17.11.7) — an inline opaque
//!    (`OpaqueKind::FootnoteReference` / `EndnoteReference`) spliced into the
//!    target paragraph exactly like a cross-reference field
//!    (`fields_crossrefs`), and
//! 2. a **note story** in `doc.footnotes` / `doc.endnotes` (§17.11.10 /
//!    §17.11.2) whose first paragraph carries the auto-number decoration
//!    (`w:footnoteRef` §17.11.6 / `w:endnoteRef` §17.11.1) plus the body text.
//!
//! The two are linked by a shared numeric `w:id` (`NoteReferenceData.reference_id`
//! == `FootnoteStory.id` / `EndnoteStory.id`).
//!
//! [`NoteKind`] unifies footnote and endnote: their authoring behaviour is
//! genuinely identical and there are two real call sites, which is what
//! justifies the small enum (CLAUDE.md "generics: rare, deliberate"). It maps
//! to `OpaqueKind::FootnoteReference` vs `EndnoteReference` and to
//! `doc.footnotes` vs `doc.endnotes`. It is NOT generalized further.
//!
//! **Invariant M (untouched):** the reference-run insert rides the existing
//! segment splice + accept/reject projection, proven by `fields_crossrefs`. The
//! note STORY content is built directly here (its own `TrackedBlock`/
//! `ParagraphNode`/`TrackedSegment` shape is identical to the body's), not
//! lowered through the body materializer's paragraph-list plumbing — but
//! `EditNote`'s TrackedChange path DOES reuse the body's word-diff engine
//! directly (`apply_replace_paragraph_text`) on the story's `ParagraphNode`,
//! and `DeleteNote`'s reference-removal reuses the body's opaque-delete engine
//! (`apply_opaque_delete`). Accept-all keeps the inserted reference run + the
//! (also tracked-Inserted) story; reject-all drops BOTH — an `InsertNote`
//! authored in TrackedChange mode leaves no orphan story on reject, and
//! `doc.footnotes`/`doc.endnotes` are swept of any story a resolution emptied
//! (`tracked_model::{accept_all,reject_all,resolve_selected_revisions}`).
//!
//! **No stacking.** `EditNote`/`DeleteNote` in TrackedChange mode refuse
//! (`BlockHasTrackedStatus`) when the addressed story block already carries a
//! pending tracked status, rather than layering a second change onto it — the
//! same convention `validate_block_is_editable` enforces for ordinary body
//! paragraphs. A caller must resolve the pending change first.
//!
//! **Renumbering is POSITIONAL.** Word renumbers footnotes/endnotes on open
//! from their document order. This verb stores NO display number anywhere: the
//! reference run carries only the link id, the story carries only the
//! `footnoteRef`/`endnoteRef` placeholder. `DeleteNote` removes story +
//! reference and relies on Word's open-time renumber. **There is intentionally
//! no renumber pass — do not add one.**
//!
//! v1 scope (fail loud beyond it, per CLAUDE.md "start narrow"):
//! - `InsertNote` targets top-level body paragraphs only; the `expect` anchor
//!   must lie within a single contiguous run of `Text` nodes in one Normal
//!   segment;
//! - `body` must be non-empty (an empty note is refused, not defaulted);
//! - `EditNote` replaces the note story's body from the provided text (v1:
//!   single-paragraph body only — `NoteBodyMultiParagraph` beyond that),
//!   preserving the leading `footnoteRef`/`endnoteRef` decoration and any
//!   pre-existing opaque inlines in the note story (else `OpaqueDestroyed`);
//!   `Direct` mode wholesale-rebuilds the paragraph, `TrackedChange` mode
//!   surgically word-diffs it;
//! - `DeleteNote` removes (`Direct`) or tracked-deletes (`TrackedChange`) the
//!   story AND every matching reference run; a story with no body reference
//!   (or a reference with no story) is a hard error (`NoteReferenceMissing`),
//!   never a half-delete.

use super::super::{
    ContentFragment, ParagraphContent, apply_opaque_delete, apply_replace_paragraph_text,
    find_opaque_flat_index, next_revision,
};
use super::super::{EditError, MaterializationMode};
use super::super::{find_block_index, validate_block_is_editable};
use crate::domain::{
    BlockNode, CanonDoc, DecorationNode, DecorationType, DocPart, EndnoteStory, FootnoteStory,
    InlineNode, NodeId, NoteReferenceData, NoteType, OpaqueInlineNode, OpaqueKind, ParagraphNode,
    ProofRef, RevisionInfo, StyleProps, TrackedBlock, TrackedSegment, TrackingStatus,
};
use crate::semantic_hash::check_block_guard;

/// Which note family a `*Note` verb targets. Maps to the reference opaque kind
/// and the story collection. See the module doc for why this small enum is
/// justified rather than two duplicated verbs or a full generic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteKind {
    Footnote,
    Endnote,
}

impl NoteKind {
    fn label(self) -> &'static str {
        match self {
            NoteKind::Footnote => "footnote",
            NoteKind::Endnote => "endnote",
        }
    }
}

/// A located insertion point inside one Normal segment: after inline index
/// `inline_idx` (a `Text` node), at char offset `char_end` inside it. The
/// reference opaque is spliced in right after that boundary. Mirrors
/// `fields_crossrefs::InsertPlan`.
struct InsertPlan {
    seg_idx: usize,
    inline_idx: usize,
    char_end: usize,
}

/// Allocate the next sequential note id by scanning BOTH `doc.footnotes` and
/// `doc.endnotes`. Footnote and endnote ids share Word's numeric id space at
/// the part level, but to be safe (and to keep a single obvious allocator) we
/// take `max(all ids) + 1` across both collections.
///
/// The reserved separator/continuationSeparator ids (typically -1 and 0) are
/// skipped: `max + 1` is always >= 1 once any normal note exists, and we floor
/// the first allocation at 1 so we never collide with the reserved ids.
///
/// **No silent fallback:** a non-numeric existing id (a malformed import) is a
/// hard error with context, not a "pretend it's zero" default.
fn allocate_note_id(doc: &CanonDoc, step_index: usize) -> Result<String, EditError> {
    let mut max: i64 = 0; // floor at 0 so the first allocation is id 1.
    for id in doc
        .footnotes
        .iter()
        .map(|f| f.id.as_str())
        .chain(doc.endnotes.iter().map(|e| e.id.as_str()))
    {
        let n: i64 = id.parse().map_err(|_| EditError::NoteIdNotNumeric {
            note_id: id.to_string(),
            step_index,
        })?;
        if n > max {
            max = n;
        }
    }
    Ok((max + 1).to_string())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_insert(
    doc: &mut CanonDoc,
    block_id: &NodeId,
    expect: &str,
    semantic_hash: Option<&str>,
    note_kind: NoteKind,
    body: &str,
    revision: &RevisionInfo,
    mode: MaterializationMode,
    rev_counter: &mut u32,
    step_index: usize,
) -> Result<(), EditError> {
    // No silent fallback: an empty note body is meaningless, refuse it.
    if body.trim().is_empty() {
        return Err(EditError::NoteEmptyBody { step_index });
    }

    // v1: top-level body paragraphs only. A nested paragraph surfaces as
    // BlockNotFound here, matching the other anchor-splice verbs.
    let idx = find_block_index(&doc.blocks, block_id).ok_or_else(|| EditError::BlockNotFound {
        block_id: block_id.clone(),
        step_index,
    })?;
    validate_block_is_editable(&doc.blocks[idx], step_index)?;

    match &doc.blocks[idx].block {
        BlockNode::Paragraph(_) => {}
        BlockNode::Table(_) => {
            return Err(EditError::NoteAnchorNotAParagraph {
                block_id: block_id.clone(),
                actual_kind: "table",
                step_index,
            });
        }
        BlockNode::OpaqueBlock(_) => {
            return Err(EditError::NoteAnchorNotAParagraph {
                block_id: block_id.clone(),
                actual_kind: "opaque_block",
                step_index,
            });
        }
    }

    if let Some(expected) = semantic_hash
        && let Err(actual) = check_block_guard(&doc.blocks[idx].block, expected)
    {
        return Err(EditError::BlockSemanticHashMismatch {
            block_id: block_id.clone(),
            expected: expected.to_string(),
            actual,
            step_index,
        });
    }

    // Allocate the link id before borrowing the paragraph mutably.
    let note_id = allocate_note_id(doc, step_index)?;

    let BlockNode::Paragraph(para) = &mut doc.blocks[idx].block else {
        unreachable!("checked paragraph above");
    };

    let plan = find_anchor(&para.segments, expect).ok_or_else(|| {
        let visible: String = para
            .segments
            .iter()
            .flat_map(|s| s.inlines.iter())
            .filter_map(|i| match i {
                InlineNode::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect();
        EditError::ExpectMismatch {
            block_id: block_id.clone(),
            expected: expect.to_string(),
            actual_text: visible,
            step_index,
        }
    })?;

    let reference = synthesize_reference_inline(&para.id, &note_id, note_kind);
    splice_reference(
        &mut para.segments,
        plan,
        reference,
        revision,
        mode,
        rev_counter,
    );
    para.block_text_hash = None;
    para.rendered_text = None;

    // Build the note story. Its first (and only, in v1) paragraph carries the
    // footnoteRef/endnoteRef auto-number decoration followed by the body text.
    //
    // The story's carrier is `Inserted(revision)` in TrackedChange mode — NOT
    // `Normal` — so Word shows the footnote body itself as inserted text and
    // reject-all removes the whole note (the reference-run splice above only
    // handles the body's link; the story is a SEPARATE tracked carrier and
    // must be marked independently, invariant #2 of this verb's contract).
    // Each carrier gets its OWN freshly stamped revision id (never the bare
    // transaction-level `revision` reused verbatim) — the same convention
    // `apply_insert_paragraphs` uses for body block inserts.
    let story_para = build_note_body_paragraph(&note_id, note_kind, body);
    let story_status = match mode {
        MaterializationMode::TrackedChange => {
            TrackingStatus::Inserted(next_revision(revision, rev_counter))
        }
        MaterializationMode::Direct => TrackingStatus::Normal,
    };
    let blocks = vec![TrackedBlock {
        status: story_status,
        block: BlockNode::from(story_para),
        move_id: None,
        block_sdt_wrap: None,
    }];
    let content_hash =
        crate::import::compute_story_content_hash(std::slice::from_ref(&blocks[0].block));

    match note_kind {
        NoteKind::Footnote => doc.footnotes.push(FootnoteStory {
            id: note_id,
            note_type: NoteType::Normal,
            blocks,
            content_hash,
        }),
        NoteKind::Endnote => doc.endnotes.push(EndnoteStory {
            id: note_id,
            note_type: NoteType::Normal,
            blocks,
            content_hash,
        }),
    }
    Ok(())
}

/// Edit an existing note's body.
///
/// `Direct` mode keeps the pre-existing contract exactly: wholesale-rebuild
/// the story's single paragraph from the new text (a fresh `footnoteRef`/
/// `endnoteRef` decoration + new `TextNode`s, no identity preserved).
///
/// `TrackedChange` mode is a SURGICAL word-diff, not a rebuild: it reuses the
/// SAME engine the body's `ReplaceParagraphText` step uses
/// (`apply_replace_paragraph_text`) directly on the story's `ParagraphNode`,
/// producing minimal `Deleted`(old)/`Inserted`(new) segments around just the
/// changed words — not a whole-paragraph delete+reinsert. The leading
/// `footnoteRef`/`endnoteRef` decoration survives via the SAME structural-
/// marker offset mechanism that protects a body paragraph's comment ranges
/// (`has_structural_markers`/`inject_structural_markers_at_offsets`): the
/// decoration is `InlineNode::Decoration` at text-offset 0, so it round-trips
/// back to the front of the rebuilt segments untouched.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_edit(
    doc: &mut CanonDoc,
    note_id: &str,
    note_kind: NoteKind,
    body: &str,
    revision: &RevisionInfo,
    mode: MaterializationMode,
    rev_counter: &mut u32,
    step_index: usize,
) -> Result<(), EditError> {
    if body.trim().is_empty() {
        return Err(EditError::NoteEmptyBody { step_index });
    }

    // Locate the existing story (by id, within the right collection — addressed
    // via the StoryRef family, see story_addr.rs). A missing id is a hard error.
    let existing_blocks = match note_kind {
        NoteKind::Footnote => doc
            .footnotes
            .iter()
            .find(|f| f.id == note_id)
            .map(|f| &f.blocks),
        NoteKind::Endnote => doc
            .endnotes
            .iter()
            .find(|e| e.id == note_id)
            .map(|e| &e.blocks),
    }
    .ok_or_else(|| EditError::NoteNotFound {
        note_id: note_id.to_string(),
        note_kind: note_kind.label(),
        step_index,
    })?;

    // Opaque preservation: neither path may drop a pre-existing opaque inline
    // (an image, a field, a nested reference) that lived in the note story.
    // v1 authors plain text, so any opaque present means a richer note we
    // cannot faithfully rebuild/diff from a flat string — fail loud rather
    // than silently destroy it.
    let opaque_ids = story_opaque_ids(existing_blocks);
    if !opaque_ids.is_empty() {
        let preview = story_text_preview(existing_blocks);
        let missing_inline_kinds = vec!["opaque"; opaque_ids.len()];
        return Err(EditError::OpaqueDestroyed {
            step_index,
            target_block_id: NodeId::from(format!("{}_{note_id}", note_kind.label())),
            missing_opaque_ids: opaque_ids,
            missing_inline_kinds,
            original_text_preview: preview,
        });
    }

    match mode {
        MaterializationMode::Direct => {
            // Unchanged pre-existing contract: rebuild the single-paragraph
            // body (footnoteRef/endnoteRef + new text).
            let story_para = build_note_body_paragraph(note_id, note_kind, body);
            let blocks = vec![TrackedBlock {
                status: TrackingStatus::Normal,
                block: BlockNode::from(story_para),
                move_id: None,
                block_sdt_wrap: None,
            }];
            let content_hash =
                crate::import::compute_story_content_hash(std::slice::from_ref(&blocks[0].block));
            match note_kind {
                NoteKind::Footnote => {
                    let story = doc
                        .footnotes
                        .iter_mut()
                        .find(|f| f.id == note_id)
                        .expect("located above");
                    story.blocks = blocks;
                    story.content_hash = content_hash;
                }
                NoteKind::Endnote => {
                    let story = doc
                        .endnotes
                        .iter_mut()
                        .find(|e| e.id == note_id)
                        .expect("located above");
                    story.blocks = blocks;
                    story.content_hash = content_hash;
                }
            }
        }
        MaterializationMode::TrackedChange => {
            // v1 scope: a single-paragraph body only (module doc). A richer
            // shape would silently lose every paragraph past the first if we
            // diffed only `blocks[0]` — refuse instead of guessing.
            if existing_blocks.len() != 1 {
                return Err(EditError::NoteBodyMultiParagraph {
                    note_id: note_id.to_string(),
                    note_kind: note_kind.label(),
                    paragraph_count: existing_blocks.len(),
                    step_index,
                });
            }

            let tracked_block = match note_kind {
                NoteKind::Footnote => doc
                    .footnotes
                    .iter_mut()
                    .find(|f| f.id == note_id)
                    .map(|f| &mut f.blocks[0]),
                NoteKind::Endnote => doc
                    .endnotes
                    .iter_mut()
                    .find(|e| e.id == note_id)
                    .map(|e| &mut e.blocks[0]),
            }
            .expect("located above");

            // No stacking: a story block that already carries a pending
            // tracked status (e.g. a not-yet-resolved tracked InsertNote, or a
            // pending DeleteNote) is refused rather than layering a second
            // change onto it — the SAME convention `validate_block_is_editable`
            // enforces for ordinary body-paragraph edits.
            validate_block_is_editable(tracked_block, step_index)?;

            let BlockNode::Paragraph(para) = &mut tracked_block.block else {
                unreachable!(
                    "story_opaque_ids treats Table/OpaqueBlock as opaque and refuses above"
                );
            };

            // Isolate the leading footnoteRef/endnoteRef decoration from the
            // word-diff entirely, rather than leaning on the body's generic
            // structural-marker offset repositioning
            // (`inject_structural_markers_at_offsets`). That machinery
            // re-inserts a marker INTO whichever segment the diff produces at
            // its offset — correct for a comment range (its fate legitimately
            // follows the text it brackets), but wrong here: offset 0 can land
            // inside a freshly `Deleted` segment (the first word changed), and
            // accepting that deletion would delete the decoration WITH it —
            // permanently destroying the note's auto-number marker. The
            // decoration has no such text relationship; it must survive
            // untouched no matter what the diff does to the words after it.
            // So: pop it off before diffing, run the surgical diff on the
            // remaining content, then prepend it back as its own leading
            // `Normal` segment.
            let leading_decoration = match para.segments.first_mut() {
                Some(seg) if matches!(seg.inlines.first(), Some(InlineNode::Decoration(_))) => {
                    let deco = seg.inlines.remove(0);
                    if seg.inlines.is_empty() {
                        para.segments.remove(0);
                    }
                    Some(deco)
                }
                _ => None,
            };

            let content = ParagraphContent {
                fragments: vec![ContentFragment::Text(body.to_string())],
            };
            // No enclosing block insertion: a note paragraph has no top-level
            // block-insertion axis (the pending-block-insertion case), and the note-block guard
            // upstream refuses a tracked-inserted note block.
            apply_replace_paragraph_text(para, &content, revision, None, rev_counter);

            if let Some(deco) = leading_decoration {
                para.segments.insert(
                    0,
                    TrackedSegment {
                        status: TrackingStatus::Normal,
                        inlines: vec![deco],
                    },
                );
            }

            let content_hash = crate::import::compute_story_content_hash(std::slice::from_ref(
                &tracked_block.block,
            ));
            match note_kind {
                NoteKind::Footnote => {
                    let story = doc
                        .footnotes
                        .iter_mut()
                        .find(|f| f.id == note_id)
                        .expect("located above");
                    story.content_hash = content_hash;
                }
                NoteKind::Endnote => {
                    let story = doc
                        .endnotes
                        .iter_mut()
                        .find(|e| e.id == note_id)
                        .expect("located above");
                    story.content_hash = content_hash;
                }
            }
        }
    }
    Ok(())
}

/// Delete a note: remove BOTH the reference run(s) and the story.
///
/// `Direct` mode physically removes both (unchanged pre-existing contract).
///
/// `TrackedChange` mode marks both as a tracked deletion instead: each
/// reference run goes through `apply_opaque_delete` (the SAME status-flip
/// engine `ReplaceSpanText`'s opaque-delete path and cross-ref/image deletes
/// use — `Deleted` if Normal, `InsertedThenDeleted` if it was someone else's
/// pending insert, un-proposed if it was the caller's own pending insert),
/// and the story's block status becomes `Deleted(revision)`. Accept-all then
/// removes note + reference (the story's projection drops the Deleted block,
/// and the new footnotes/endnotes `retain` cleans up the emptied story);
/// reject-all restores both fully. No stacking: a story that already carries
/// a pending tracked status refuses (`BlockHasTrackedStatus`) rather than
/// layering a second change onto it — same convention as `EditNote`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_delete(
    doc: &mut CanonDoc,
    note_id: &str,
    note_kind: NoteKind,
    revision: &RevisionInfo,
    mode: MaterializationMode,
    rev_counter: &mut u32,
    step_index: usize,
) -> Result<(), EditError> {
    // The story must exist.
    let story_present = match note_kind {
        NoteKind::Footnote => doc.footnotes.iter().any(|f| f.id == note_id),
        NoteKind::Endnote => doc.endnotes.iter().any(|e| e.id == note_id),
    };
    if !story_present {
        return Err(EditError::NoteNotFound {
            note_id: note_id.to_string(),
            note_kind: note_kind.label(),
            step_index,
        });
    }

    // Count the matching reference runs across the body BEFORE mutating, so we
    // can fail loud when a story has no body reference (no half-delete).
    let reference_count = count_references(&doc.blocks, note_id, note_kind);
    if reference_count == 0 {
        return Err(EditError::NoteReferenceMissing {
            note_id: note_id.to_string(),
            note_kind: note_kind.label(),
            step_index,
        });
    }

    if mode == MaterializationMode::TrackedChange {
        let story_blocks = match note_kind {
            NoteKind::Footnote => doc
                .footnotes
                .iter()
                .find(|f| f.id == note_id)
                .map(|f| &f.blocks),
            NoteKind::Endnote => doc
                .endnotes
                .iter()
                .find(|e| e.id == note_id)
                .map(|e| &e.blocks),
        }
        .expect("story_present checked above");
        for tb in story_blocks {
            validate_block_is_editable(tb, step_index)?;
        }
    }

    // Mark/remove every matching reference run from body paragraphs (a note
    // may be referenced more than once) — mode-aware via `apply_opaque_delete`
    // for BOTH branches, so this one walk covers Direct's physical removal and
    // TrackedChange's status flip.
    for tb in &mut doc.blocks {
        mark_references_deleted_in_block(
            &mut tb.block,
            note_id,
            note_kind,
            mode,
            revision,
            rev_counter,
        );
    }

    match mode {
        MaterializationMode::Direct => match note_kind {
            NoteKind::Footnote => doc.footnotes.retain(|f| f.id != note_id),
            NoteKind::Endnote => doc.endnotes.retain(|e| e.id != note_id),
        },
        MaterializationMode::TrackedChange => {
            let story_blocks = match note_kind {
                NoteKind::Footnote => doc
                    .footnotes
                    .iter_mut()
                    .find(|f| f.id == note_id)
                    .map(|f| &mut f.blocks),
                NoteKind::Endnote => doc
                    .endnotes
                    .iter_mut()
                    .find(|e| e.id == note_id)
                    .map(|e| &mut e.blocks),
            }
            .expect("story_present checked above");
            for tb in story_blocks.iter_mut() {
                tb.status = TrackingStatus::Deleted(next_revision(revision, rev_counter));
                if let BlockNode::Paragraph(p) = &mut tb.block {
                    p.para_mark_status = Some(TrackingStatus::Deleted(next_revision(
                        revision,
                        rev_counter,
                    )));
                }
            }
        }
    }
    Ok(())
}

// ─── reference-run synthesis / splice (mirrors fields_crossrefs) ──────────────

/// Build a fresh inline `OpaqueKind::FootnoteReference` / `EndnoteReference`
/// with `raw_xml: None` carrying the link id. The serializer rebuilds the
/// `<w:footnoteReference w:id="N"/>` / `<w:endnoteReference w:id="N"/>` run
/// from `NoteReferenceData.reference_id` (with the FootnoteReference /
/// EndnoteReference rStyle), the same pattern as a synthesized field.
fn synthesize_reference_inline(para_id: &NodeId, note_id: &str, kind: NoteKind) -> InlineNode {
    let id = NodeId::from(format!("{}_{}ref_{note_id}", para_id.0, kind.label()));
    let data = NoteReferenceData {
        reference_id: note_id.to_string(),
    };
    let opaque_kind = match kind {
        NoteKind::Footnote => OpaqueKind::FootnoteReference(data),
        NoteKind::Endnote => OpaqueKind::EndnoteReference(data),
    };
    InlineNode::from(OpaqueInlineNode {
        id: id.clone(),
        kind: opaque_kind,
        opaque_ref: format!("noteref_{}", id.0),
        proof_ref: ProofRef {
            part: DocPart::DocumentXml,
            block_id: id.clone(),
            docx_anchor: String::new(),
        },
        wrapper_marks: Vec::new(),
        wrapper_style_props: StyleProps::default(),
        raw_xml: None,
        content_hash: None,
    })
}

/// Locate `expect` inside a single contiguous run of `Text` nodes within one
/// Normal segment. Identical strategy to `fields_crossrefs::find_anchor`.
fn find_anchor(segments: &[TrackedSegment], expect: &str) -> Option<InsertPlan> {
    if expect.is_empty() {
        return None;
    }
    for (seg_idx, seg) in segments.iter().enumerate() {
        if seg.status != TrackingStatus::Normal {
            continue;
        }
        let inlines = &seg.inlines;
        let mut i = 0;
        while i < inlines.len() {
            if !matches!(inlines[i], InlineNode::Text(_)) {
                i += 1;
                continue;
            }
            let mut j = i;
            let mut concat = String::new();
            while j < inlines.len() {
                match &inlines[j] {
                    InlineNode::Text(t) => {
                        concat.push_str(&t.text);
                        j += 1;
                    }
                    _ => break,
                }
            }
            if let Some(match_end_chars) = char_find_end(&concat, expect) {
                let mut consumed = 0usize;
                for (k, inline) in inlines.iter().enumerate().take(j).skip(i) {
                    let InlineNode::Text(t) = inline else {
                        unreachable!("run is all TextNodes");
                    };
                    let len = t.text.chars().count();
                    if match_end_chars <= consumed + len {
                        return Some(InsertPlan {
                            seg_idx,
                            inline_idx: k,
                            char_end: match_end_chars - consumed,
                        });
                    }
                    consumed += len;
                }
            }
            i = j.max(i + 1);
        }
    }
    None
}

fn char_find_end(haystack: &str, needle: &str) -> Option<usize> {
    let byte = haystack.find(needle)?;
    let start = haystack[..byte].chars().count();
    Some(start + needle.chars().count())
}

/// Splice the reference opaque into the located segment right after the anchor
/// text. Identical structure to `fields_crossrefs::splice_field`: the host
/// segment splits into head (Normal) / reference (Inserted in TrackedChange,
/// Normal in Direct) / tail (Normal). Accept-all keeps the reference; reject-all
/// drops the Inserted segment and the two Normal halves re-coalesce to the
/// original — reversibility handled entirely by the existing segment-level
/// accept/reject projection (Invariant M untouched).
fn splice_reference(
    segments: &mut Vec<TrackedSegment>,
    plan: InsertPlan,
    reference: InlineNode,
    revision: &RevisionInfo,
    mode: MaterializationMode,
    rev_counter: &mut u32,
) {
    let host = segments.remove(plan.seg_idx);
    let mut inlines = host.inlines;

    if let InlineNode::Text(node) = &inlines[plan.inline_idx] {
        let chars: Vec<char> = node.text.chars().collect();
        if plan.char_end < chars.len() {
            let before: String = chars[..plan.char_end].iter().collect();
            let after: String = chars[plan.char_end..].iter().collect();
            let mut head = node.clone();
            head.text = before;
            let mut tail = node.clone();
            tail.id = NodeId::new(format!("{}_ntail", node.id.0));
            tail.text = after;
            inlines.splice(
                plan.inline_idx..=plan.inline_idx,
                [InlineNode::Text(head), InlineNode::Text(tail)],
            );
        }
    }

    let split_at = plan.inline_idx + 1;
    let head_inlines: Vec<InlineNode> = inlines.drain(..split_at).collect();
    let tail_inlines: Vec<InlineNode> = inlines;

    let ref_status = match mode {
        MaterializationMode::TrackedChange => {
            TrackingStatus::Inserted(next_revision(revision, rev_counter))
        }
        MaterializationMode::Direct => TrackingStatus::Normal,
    };

    let mut rebuilt: Vec<TrackedSegment> = Vec::new();
    if !head_inlines.is_empty() {
        rebuilt.push(TrackedSegment {
            status: TrackingStatus::Normal,
            inlines: head_inlines,
        });
    }
    rebuilt.push(TrackedSegment {
        status: ref_status,
        inlines: vec![reference],
    });
    if !tail_inlines.is_empty() {
        rebuilt.push(TrackedSegment {
            status: TrackingStatus::Normal,
            inlines: tail_inlines,
        });
    }

    segments.splice(plan.seg_idx..plan.seg_idx, rebuilt);
}

// ─── note-story body synthesis ───────────────────────────────────────────────

/// Build the note story's first paragraph: a leading `w:footnoteRef` /
/// `w:endnoteRef` decoration (the auto-number placeholder Word renders, §17.11.6
/// / §17.11.1) followed by the body text. The decoration is carried as an
/// `InlineNode::Decoration` with synthesized `raw_xml` — exactly the shape the
/// importer produces for these elements, so the serializer's
/// `decoration_requires_run_wrapper` branch emits `<w:r><w:footnoteRef/></w:r>`.
fn build_note_body_paragraph(note_id: &str, kind: NoteKind, body: &str) -> ParagraphNode {
    let para_id = format!("{}_{note_id}_p1", kind.label());
    let mut para = ParagraphNode::new_story_body(&para_id, body, None);
    // new_story_body produces a single Normal segment with one text node. Prepend
    // the auto-number decoration ahead of the body text in that segment.
    let (ref_tag, ref_style) = match kind {
        NoteKind::Footnote => ("footnoteRef", "FootnoteReference"),
        NoteKind::Endnote => ("endnoteRef", "EndnoteReference"),
    };
    // Word renders the auto-number via the note-reference character style. The
    // body reference run gets it synthesized in `build_wrapper_rpr`
    // (`note_reference_style_name`); mirror that on the story's ref decoration
    // so the created note's number is styled the same, not left at the default.
    let ref_style_props = crate::domain::StyleProps {
        char_style_id: Some(ref_style.into()),
        ..crate::domain::StyleProps::default()
    };
    let deco = InlineNode::from(DecorationNode {
        id: NodeId::from(format!("{para_id}_ref")),
        // Footnote/endnote auto-number markers are not in DecorationType; they
        // round-trip purely through raw_xml (the importer also classifies them
        // as a non-specific decoration kind). Bookmark is the importer's
        // fallback kind and is inert for serialization here.
        kind: DecorationType::Bookmark,
        opaque_ref: format!("{para_id}_ref"),
        proof_ref: ProofRef {
            part: DocPart::DocumentXml,
            block_id: NodeId::from(format!("{para_id}_ref")),
            docx_anchor: String::new(),
        },
        wrapper_marks: Vec::new(),
        wrapper_style_props: ref_style_props,
        raw_xml: Some(format!("<w:{ref_tag}/>").into_bytes()),
        origin: None,
    });
    if let Some(seg) = para.segments.first_mut() {
        seg.inlines.insert(0, deco);
    }
    para.block_text_hash = None;
    para.rendered_text = None;
    para
}

// ─── reference scanning / removal helpers ─────────────────────────────────────

/// True when `inline` is a reference run for `note_id` of the given kind.
fn is_reference_for(inline: &InlineNode, note_id: &str, kind: NoteKind) -> bool {
    let InlineNode::OpaqueInline(o) = inline else {
        return false;
    };
    match (&o.kind, kind) {
        (OpaqueKind::FootnoteReference(rd), NoteKind::Footnote) => rd.reference_id == note_id,
        (OpaqueKind::EndnoteReference(rd), NoteKind::Endnote) => rd.reference_id == note_id,
        _ => false,
    }
}

fn count_references(blocks: &[TrackedBlock], note_id: &str, kind: NoteKind) -> usize {
    let mut count = 0;
    for tb in blocks {
        count += count_references_in_block(&tb.block, note_id, kind);
    }
    count
}

fn count_references_in_block(block: &BlockNode, note_id: &str, kind: NoteKind) -> usize {
    match block {
        BlockNode::Paragraph(p) => p
            .segments
            .iter()
            .flat_map(|s| s.inlines.iter())
            .filter(|i| is_reference_for(i, note_id, kind))
            .count(),
        BlockNode::Table(t) => t
            .rows
            .iter()
            .flat_map(|r| r.cells.iter())
            .flat_map(|c| c.blocks.iter())
            .map(|b| count_references_in_block(b, note_id, kind))
            .sum(),
        BlockNode::OpaqueBlock(_) => 0,
    }
}

/// Mode-aware reference-run removal for `DeleteNote`: physically drops the
/// reference in `Direct` mode, wraps it as a tracked deletion in
/// `TrackedChange` mode. Reuses `apply_opaque_delete` — the SAME status-flip
/// engine `ReplaceSpanText`'s opaque-delete path uses for images/cross-refs —
/// so a footnote/endnote reference gets the identical own-insert-un-proposes /
/// cross-author-stacks / already-tombstoned-is-idempotent handling, rather
/// than a second bespoke implementation of that state machine.
fn mark_references_deleted_in_block(
    block: &mut BlockNode,
    note_id: &str,
    kind: NoteKind,
    mode: MaterializationMode,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
) {
    match block {
        BlockNode::Paragraph(p) => {
            let ids: Vec<NodeId> = p
                .segments
                .iter()
                .flat_map(|s| s.inlines.iter())
                .filter_map(|i| match i {
                    InlineNode::OpaqueInline(o) if is_reference_for(i, note_id, kind) => {
                        Some(o.id.clone())
                    }
                    _ => None,
                })
                .collect();
            for id in ids {
                if let Some(idx) = find_opaque_flat_index(p, &id) {
                    apply_opaque_delete(p, idx, mode, revision, rev_counter);
                }
            }
        }
        BlockNode::Table(t) => {
            for row in &mut t.rows {
                for cell in &mut row.cells {
                    for b in &mut cell.blocks {
                        mark_references_deleted_in_block(
                            b,
                            note_id,
                            kind,
                            mode,
                            revision,
                            rev_counter,
                        );
                    }
                }
            }
        }
        BlockNode::OpaqueBlock(_) => {}
    }
}

/// Collect the ids of every inline opaque (image, field, reference, etc.) in
/// the story. Used by `EditNote` to refuse a wholesale text replace that would
/// silently destroy such content. A table / opaque block in a note story is
/// itself non-flat-text content, surfaced under its block id.
fn story_opaque_ids(blocks: &[TrackedBlock]) -> Vec<String> {
    let mut ids = Vec::new();
    for tb in blocks {
        match &tb.block {
            BlockNode::Paragraph(p) => {
                for inline in p.segments.iter().flat_map(|s| s.inlines.iter()) {
                    if let InlineNode::OpaqueInline(o) = inline {
                        ids.push(o.id.0.to_string());
                    }
                }
            }
            BlockNode::Table(t) => ids.push(t.id.0.to_string()),
            BlockNode::OpaqueBlock(o) => ids.push(o.id.0.to_string()),
        }
    }
    ids
}

/// Short visible-text preview of a note story (first paragraph's text), for the
/// `OpaqueDestroyed` error message.
fn story_text_preview(blocks: &[TrackedBlock]) -> String {
    for tb in blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            let text: String = p
                .segments
                .iter()
                .flat_map(|s| s.inlines.iter())
                .filter_map(|i| match i {
                    InlineNode::Text(t) => Some(t.text.as_str()),
                    _ => None,
                })
                .collect();
            if !text.is_empty() {
                return text.chars().take(80).collect();
            }
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::TextNode;

    fn empty_doc() -> CanonDoc {
        CanonDoc {
            id: NodeId::from("doc"),
            blocks: vec![],
            meta: crate::domain::DocMeta {
                schema_version: crate::domain::SCHEMA_VERSION_V0.to_string(),
                docx_fingerprint: crate::domain::DocFingerprint("test".to_string()),
                internal_ids_version: crate::domain::INTERNAL_IDS_VERSION_V0.to_string(),
            },
            headers: vec![],
            footers: vec![],
            footnotes: vec![],
            endnotes: vec![],
            comments: vec![],
            comments_extended: vec![],
            body_section_properties: None,
            body_section_property_change: None,
            compat_settings: crate::domain::CompatSettings::default(),
            even_and_odd_headers: None,
            document_background: None,
            document_protection: None,
        }
    }

    fn footnote(id: &str, note_type: NoteType) -> FootnoteStory {
        FootnoteStory {
            id: id.to_string(),
            note_type,
            blocks: vec![],
            content_hash: String::new(),
        }
    }

    #[test]
    fn allocate_note_id_floors_at_one_skipping_reserved() {
        // Only the reserved separator ids present → first normal note is 1.
        let mut doc = empty_doc();
        doc.footnotes = vec![
            footnote("-1", NoteType::Separator),
            footnote("0", NoteType::ContinuationSeparator),
        ];
        assert_eq!(allocate_note_id(&doc, 0).unwrap(), "1");
    }

    #[test]
    fn allocate_note_id_spans_both_collections() {
        let mut doc = empty_doc();
        doc.footnotes = vec![footnote("1", NoteType::Normal)];
        doc.endnotes = vec![EndnoteStory {
            id: "5".to_string(),
            note_type: NoteType::Normal,
            blocks: vec![],
            content_hash: String::new(),
        }];
        // max across both is 5 → next is 6.
        assert_eq!(allocate_note_id(&doc, 0).unwrap(), "6");
    }

    #[test]
    fn allocate_note_id_rejects_non_numeric() {
        let mut doc = empty_doc();
        doc.footnotes = vec![footnote("abc", NoteType::Normal)];
        let err = allocate_note_id(&doc, 9).expect_err("non-numeric id must fail");
        match err {
            EditError::NoteIdNotNumeric {
                note_id,
                step_index,
            } => {
                assert_eq!(note_id, "abc");
                assert_eq!(step_index, 9);
            }
            other => panic!("expected NoteIdNotNumeric, got {other:?}"),
        }
    }

    #[test]
    fn note_body_paragraph_leads_with_ref_decoration() {
        let para = build_note_body_paragraph("3", NoteKind::Footnote, "See clause 4.");
        let seg = &para.segments[0];
        // First inline is the footnoteRef decoration, then the body text.
        match &seg.inlines[0] {
            InlineNode::Decoration(d) => {
                assert_eq!(d.raw_xml.as_deref(), Some(b"<w:footnoteRef/>".as_slice()));
            }
            other => panic!("expected leading decoration, got {other:?}"),
        }
        match &seg.inlines[1] {
            InlineNode::Text(t) => assert_eq!(t.text, "See clause 4."),
            other => panic!("expected body text, got {other:?}"),
        }
    }

    #[test]
    fn endnote_body_uses_endnote_ref() {
        let para = build_note_body_paragraph("2", NoteKind::Endnote, "x");
        match &para.segments[0].inlines[0] {
            InlineNode::Decoration(d) => {
                assert_eq!(d.raw_xml.as_deref(), Some(b"<w:endnoteRef/>".as_slice()));
            }
            other => panic!("expected endnoteRef decoration, got {other:?}"),
        }
    }

    #[test]
    fn synthesized_reference_carries_link_id_and_no_raw_xml() {
        let inline = synthesize_reference_inline(&NodeId::from("p_1"), "7", NoteKind::Footnote);
        let InlineNode::OpaqueInline(o) = inline else {
            panic!("expected opaque inline");
        };
        match &o.kind {
            OpaqueKind::FootnoteReference(rd) => assert_eq!(rd.reference_id, "7"),
            other => panic!("expected FootnoteReference, got {other:?}"),
        }
        assert!(
            o.raw_xml.is_none(),
            "serializer must rebuild from the link id"
        );
    }

    fn text(id: &str, t: &str) -> InlineNode {
        InlineNode::from(TextNode {
            id: NodeId::from(id),
            text_role: None,
            text: t.to_string(),
            marks: vec![],
            style_props: Default::default(),
            rpr_authored: crate::domain::RunRprAuthored::default(),
            formatting_change: None,
        })
    }

    #[test]
    fn splice_reference_lands_after_anchor() {
        let mut segs = vec![TrackedSegment {
            status: TrackingStatus::Normal,
            inlines: vec![text("r0", "Hello world")],
        }];
        let plan = find_anchor(&segs, "Hello").expect("found anchor");
        let reference = synthesize_reference_inline(&NodeId::from("p_1"), "1", NoteKind::Footnote);
        let rev = RevisionInfo {
            revision_id: 0,
            author: None,
            date: None,
            apply_op_id: None,
        };
        splice_reference(
            &mut segs,
            plan,
            reference,
            &rev,
            MaterializationMode::Direct,
            &mut 0u32,
        );

        // Expect: Normal["Hello"], Normal[reference], Normal[" world"].
        let visible: String = segs
            .iter()
            .flat_map(|s| s.inlines.iter())
            .filter_map(|i| match i {
                InlineNode::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(visible, "Hello world");
        let has_ref = segs
            .iter()
            .flat_map(|s| s.inlines.iter())
            .any(|i| is_reference_for(i, "1", NoteKind::Footnote));
        assert!(has_ref);
    }
}
