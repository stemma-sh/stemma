//! `SetParagraphNumbering` — author a tracked paragraph numbering change
//! (`w:pPrChange` carrying the previous `w:numPr`, §17.13.5.29). "Make this
//! paragraph item (a) of the list, tracked."
//!
//! Like `run_formatting` (which lifted `w:rPrChange` to the authoring side),
//! this verb adds authoring grammar only. It records the paragraph's previous
//! numbering in a `ParagraphFormattingChange` and sets the new
//! `ParagraphNode.numbering`. It builds NO new serialization: the serializer
//! already emits the live `numPr` (serialize.rs "Position 6") and the previous
//! numbering inside `pPrChange`'s inner `pPr` (including the `numId=0`
//! explicitly-absent signal), and accept/reject already resolves
//! `formatting_change` (reject restores `previous_numbering`; accept keeps the
//! new numbering and clears the change). It does **not** touch the materializer
//! (Invariant M) — a numbering change is an in-place pPr delta, not a segment
//! insert/delete.
//!
//! ## Why the caller supplies `synthesized_text` / `is_bullet`
//!
//! `NumberingInfo` carries two *derived* fields — `synthesized_text` (e.g.
//! "1.", "(a)") and `is_bullet` — that come from the document's
//! `NumberingDefinitions` (numbering.xml). The apply path operates on a
//! `CanonDoc` value, which does **not** carry the parsed definitions. Rather
//! than fabricate these fields (CLAUDE.md "no silent fallbacks"), the verb
//! requires the caller — which parsed the document and therefore has the
//! definitions — to supply the already-resolved values for `SetList`/`SetLevel`.
//! For `Remove`, the new numbering is simply `None`.
//!
//! v1 scope (fail loud beyond it):
//! - top-level paragraphs only (no table-cell numbering);
//! - the target paragraph must have Normal tracking and no tracked segments;
//! - the paragraph must not already carry a tracked formatting change (one
//!   `w:pPrChange` per paragraph — accept/reject the existing one first);
//! - `SetLevel` refuses on a paragraph that has no current structural numbering
//!   (cannot indent/outdent a non-list paragraph);
//! - manual-numbering paragraphs (a stripped `literal_prefix`) are refused — v1
//!   does not convert manual prefixes to structural numbering;
//! - a structurally-equal no-op request is refused.

use super::super::{EditError, MaterializationMode, NumberingOp};
use super::super::{find_block_index, validate_block_is_editable};
use crate::domain::{
    BlockNode, CanonDoc, NodeId, NumberingInfo, ParagraphFormattingChange, ParagraphNode,
    RevisionInfo,
};
use crate::semantic_hash::check_block_guard;

/// The narrow set of paragraph-numbering operations the verb authors. Mirrors
/// the four list operations a reviewer performs in Word: attach a list, change
/// the indent level, restart the counter, or detach the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NumberingChange {
    /// Attach/replace the paragraph's list: set `num_id` + `ilvl`, optionally
    /// requesting a counter restart. `synthesized_text`/`is_bullet` are the
    /// caller-resolved derived values (see module docs).
    SetList {
        num_id: u32,
        ilvl: u32,
        restart: bool,
        synthesized_text: String,
        is_bullet: bool,
    },
    /// Change only the indent level (indent/outdent), keeping the current
    /// `num_id`. Refused if the paragraph has no current numbering.
    /// `synthesized_text`/`is_bullet` are the caller-resolved values for the
    /// new level.
    SetLevel {
        ilvl: u32,
        synthesized_text: String,
        is_bullet: bool,
    },
    /// Detach the list: set `numbering = None`.
    Remove,

    /// Indent one level (ilvl + 1), keeping the current `num_id`/`is_bullet`.
    /// Refused on an unnumbered paragraph (`NumberingLevelOnUnnumbered`) and at
    /// the maximum level 8 (`NumberingLevelOutOfBounds`). The displayed label is
    /// re-derived by Word from `word/numbering.xml` at the new level, so the IR's
    /// `synthesized_text` (a non-serialized diff hint) is carried unchanged
    /// rather than fabricated — the live `w:numPr` only carries `numId`/`ilvl`.
    Indent,

    /// Outdent one level (ilvl - 1), keeping the current `num_id`/`is_bullet`.
    /// Refused on an unnumbered paragraph (`NumberingLevelOnUnnumbered`) and at
    /// the minimum level 0 (`NumberingLevelOutOfBounds`). Same label note as
    /// `Indent`.
    Outdent,

    /// Restart the list counter at this paragraph (sets `restart_numbering =
    /// true`), keeping `num_id`/`ilvl`. First-class form of `SetList{restart:
    /// true, ..}`. Refused on an unnumbered paragraph.
    Restart,

    /// Continue the previous list run at this paragraph (sets `restart_numbering
    /// = false`), keeping `num_id`/`ilvl`. First-class form of `SetList{restart:
    /// false, ..}`. Refused on an unnumbered paragraph.
    Continue,

    /// Swap the list KIND (bullet <-> numbered) by re-pointing the paragraph at
    /// an EXISTING `num_id` of the target kind, keeping the current `ilvl`. The
    /// caller resolves `num_id` (and the derived `synthesized_text`/`is_bullet`)
    /// from `word/numbering.xml` — if no list of the target kind exists in the
    /// document, the caller must fail loud rather than fabricate a new
    /// definition (create-new-list-definition is DEFERRED). Refused on an
    /// unnumbered paragraph.
    SetType {
        num_id: u32,
        synthesized_text: String,
        is_bullet: bool,
    },

    /// Split this list at the target paragraph: the target item and every
    /// CONTIGUOUS following item that shares the target's `num_id` are re-pointed
    /// (as tracked `w:pPrChange`s) at a BRAND-NEW `num_id` whose `<w:abstractNum>`
    /// clones the source list's level formats, so the tail renumbers from 1
    /// independently while looking identical. Items before the split keep the
    /// original `num_id`; `ilvl` is preserved on every moved item.
    ///
    /// Unlike the other variants this one operates on a RUN of paragraphs and
    /// also authors a new numbering definition: it stages a
    /// [`crate::edit::NumberingOp::CreateDefinition`] (cloned at save time from
    /// `word/numbering.xml`). It does NOT allocate the real `num_id` — the verb
    /// core is pure over `&CanonDoc` and cannot see `word/numbering.xml`, so it
    /// re-points the tail at a sentinel PLACEHOLDER id and lets the save path
    /// (which can see the numbering part's authoritative id population) allocate
    /// the real id and rewrite the placeholder. Refused on an unnumbered
    /// paragraph (`NumberingSplitOnUnnumbered`).
    Split,
}

/// OOXML list levels are 0..=8 (`w:ilvl`, §17.9.3 `ST_DecimalNumber` clamped to
/// 9 levels by Word). Indent/outdent refuse outside this range rather than
/// clamp silently.
const MAX_ILVL: u32 = 8;

/// Base of the sentinel `num_id` range a list-split stages as a PLACEHOLDER.
///
/// The verb core is pure over `&CanonDoc` and cannot see `word/numbering.xml`,
/// so it does not know which `num_id`s the numbering PART already defines —
/// orphan definitions, style-linked lists, and story-only (header/footer/
/// footnote) lists routinely occupy ids no body paragraph references. Guessing a
/// fresh id from a body scan (`max(body num_id) + 1`) therefore collides with
/// those part-only ids on a large fraction of real documents, and the save path
/// refuses the collision. Instead the verb re-points the split tail at a
/// placeholder in this reserved range and stages a
/// [`NumberingOp::CreateDefinition`]; the save path
/// (`runtime::apply_pending_numbering_ops`), which CAN see the part, allocates
/// the real id against the authoritative population and rewrites the
/// placeholder — mirroring how [`crate::edit::PendingMedia`]'s `logical_rid`
/// becomes a real rId at save time.
///
/// The range sits far above any `num_id` Word authors (small, densely-packed
/// ids), so it cannot alias a real definition; a document that genuinely defines
/// a `num_id` this high is refused loudly at save time rather than captured.
const SPLIT_PLACEHOLDER_NUM_ID_BASE: u32 = 0xF000_0000;

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply(
    doc: &mut CanonDoc,
    block_id: &NodeId,
    semantic_hash: Option<&str>,
    change: &NumberingChange,
    revision: &RevisionInfo,
    mode: MaterializationMode,
    step_index: usize,
    numbering_ops: &mut Vec<NumberingOp>,
) -> Result<(), EditError> {
    // Split is a whole-run operation that also authors a new list definition; it
    // does not fit the single-paragraph dispatch below, so it is handled first.
    if matches!(change, NumberingChange::Split) {
        return apply_split(
            doc,
            block_id,
            semantic_hash,
            revision,
            step_index,
            numbering_ops,
        );
    }

    // v1: top-level paragraphs only. A nested (table-cell) paragraph is not
    // found here and surfaces as BlockNotFound.
    let idx = find_block_index(&doc.blocks, block_id).ok_or_else(|| EditError::BlockNotFound {
        block_id: block_id.clone(),
        step_index,
    })?;

    // Block status + segment-Normal preconditions (same gate as a text replace).
    validate_block_is_editable(&doc.blocks[idx], step_index)?;

    match &doc.blocks[idx].block {
        BlockNode::Paragraph(_) => {}
        BlockNode::Table(_) => {
            return Err(EditError::NotAParagraph {
                block_id: block_id.clone(),
                actual_kind: "table",
                step_index,
            });
        }
        BlockNode::OpaqueBlock(_) => {
            return Err(EditError::NotAParagraph {
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

    let BlockNode::Paragraph(para) = &mut doc.blocks[idx].block else {
        unreachable!("checked paragraph above");
    };
    apply_to_paragraph(para, block_id, change, revision, mode, step_index)
}

fn apply_to_paragraph(
    para: &mut ParagraphNode,
    block_id: &NodeId,
    change: &NumberingChange,
    revision: &RevisionInfo,
    mode: MaterializationMode,
    step_index: usize,
) -> Result<(), EditError> {
    // Refuse to stack a second tracked pPrChange on the same paragraph — accept
    // or reject the existing one first. This is the shared "one tracked
    // pPrChange per paragraph" precondition (coordinated with the
    // paragraph-formatting verb).
    if para.formatting_change.is_some() {
        return Err(EditError::UnsupportedParagraphStructure {
            block_id: block_id.clone(),
            reason: "the paragraph already has a tracked formatting change; \
                     accept or reject it before changing numbering again"
                .to_string(),
            step_index,
        });
    }

    // v1: manual-numbering paragraphs (a stripped literal prefix) are out of
    // scope — converting a manual prefix to structural numbering interacts with
    // the previous_numbering_explicitly_absent predicate in ways we do not
    // author yet. Refuse rather than guess.
    if para.literal_prefix.is_some() {
        return Err(EditError::NumberingManualPrefixUnsupported {
            block_id: block_id.clone(),
            step_index,
        });
    }

    // Compute the requested new numbering.
    let new_numbering: Option<NumberingInfo> =
        match change {
            NumberingChange::SetList {
                num_id,
                ilvl,
                restart,
                synthesized_text,
                is_bullet,
            } => Some(NumberingInfo {
                num_id: *num_id,
                ilvl: *ilvl,
                synthesized_text: synthesized_text.clone(),
                is_bullet: *is_bullet,
                restart_numbering: *restart,
            }),
            NumberingChange::SetLevel {
                ilvl,
                synthesized_text,
                is_bullet,
            } => {
                // Indent/outdent keeps the current num_id; refuse if there is no
                // current numbering to re-level.
                let current = para.numbering.as_ref().ok_or_else(|| {
                    EditError::NumberingLevelOnUnnumbered {
                        block_id: block_id.clone(),
                        step_index,
                    }
                })?;
                Some(NumberingInfo {
                    num_id: current.num_id,
                    ilvl: *ilvl,
                    synthesized_text: synthesized_text.clone(),
                    is_bullet: *is_bullet,
                    // Re-leveling does not request a counter restart.
                    restart_numbering: false,
                })
            }
            NumberingChange::Remove => None,
            NumberingChange::Indent => Some(relevel(para, block_id, step_index, 1)?),
            NumberingChange::Outdent => Some(relevel(para, block_id, step_index, -1)?),
            NumberingChange::Restart => Some(set_restart(para, block_id, step_index, true)?),
            NumberingChange::Continue => Some(set_restart(para, block_id, step_index, false)?),
            NumberingChange::SetType {
                num_id,
                synthesized_text,
                is_bullet,
            } => {
                // Swap the list kind by re-pointing at an existing num_id; keep the
                // current level. Refused on an unnumbered paragraph.
                let current = para.numbering.as_ref().ok_or_else(|| {
                    EditError::NumberingLevelOnUnnumbered {
                        block_id: block_id.clone(),
                        step_index,
                    }
                })?;
                Some(NumberingInfo {
                    num_id: *num_id,
                    ilvl: current.ilvl,
                    synthesized_text: synthesized_text.clone(),
                    is_bullet: *is_bullet,
                    restart_numbering: current.restart_numbering,
                })
            }
            NumberingChange::Split => {
                unreachable!("Split is dispatched in `apply` before reaching apply_to_paragraph")
            }
        };

    // No-op guard: structural equality (num_id + ilvl) AND identical restart
    // intent means nothing changes. A bare restart of the same list IS a
    // change (it flips restart_numbering), so it is not a no-op.
    if is_noop(&para.numbering, &new_numbering) {
        return Err(EditError::NoNumberingChangeRequested {
            block_id: block_id.clone(),
            step_index,
        });
    }

    // Snapshot the previous numbering before mutating.
    let previous_numbering = para.numbering.clone();
    // Load-bearing reject-view signal: "base had no numbering at all" (no numPr
    // AND no literal prefix) while numbering is being added. Copies the exact
    // predicate from the merge producer (tracked_model.rs). literal_prefix is
    // refused above, so the `!literal_prefix.is_some()` term is always true here.
    let previous_numbering_explicitly_absent =
        para.numbering.is_none() && new_numbering.is_some() && para.literal_prefix.is_none();

    match mode {
        // Author a tracked change: snapshot the COMPLETE previous paragraph
        // properties into pPrChange (§17.13.5.29 requires a complete previous
        // state, not just the changed field), then set the new numbering. The
        // constructor mirrors the merge producer (tracked_model.rs) so the
        // inner pPr snapshot is complete and reject restores everything.
        MaterializationMode::TrackedChange => {
            para.formatting_change = Some(ParagraphFormattingChange {
                revision_id: revision.revision_id,
                previous_alignment: para.align.clone(),
                // Snapshot AUTHORED-direct indent/spacing (the previous DIRECT
                // pPr), not the resolved effective value — see
                // snapshot_paragraph_formatting.
                previous_indentation: para.authored_indent.clone().or_else(|| para.indent.clone()),
                previous_spacing: para
                    .authored_spacing
                    .clone()
                    .or_else(|| para.spacing.clone()),
                previous_numbering,
                previous_numbering_explicitly_absent,
                previous_style_id: para.style_id.clone(),
                previous_keep_next: para.keep_next,
                previous_keep_lines: para.keep_lines,
                previous_page_break_before: para.page_break_before,
                previous_widow_control: para.widow_control,
                previous_contextual_spacing: para.contextual_spacing,
                previous_shading: para.shading.clone(),
                previous_borders: para.borders.clone(),
                previous_tab_stops: para.tab_stops.clone(),
                previous_literal_prefix_leading_tab_twips: para.literal_prefix_leading_tab_twips,
                previous_literal_prefix_trailing_tab_stop_twips: para
                    .literal_prefix_trailing_tab_stop_twips,
                previous_paragraph_mark_marks: para.paragraph_mark_marks.clone(),
                previous_paragraph_mark_style_props: para.paragraph_mark_style_props.clone(),
                previous_paragraph_mark_rpr_off: para.paragraph_mark_rpr_off,
                previous_text_direction: para.text_direction.clone(),
                previous_text_alignment: para.text_alignment.clone(),
                previous_mirror_indents: para.mirror_indents,
                previous_auto_space_de: para.auto_space_de,
                previous_auto_space_dn: para.auto_space_dn,
                previous_bidi: para.bidi,
                previous_suppress_auto_hyphens: para.suppress_auto_hyphens,
                previous_snap_to_grid: para.snap_to_grid,
                previous_overflow_punct: para.overflow_punct,
                previous_adjust_right_ind: para.adjust_right_ind,
                previous_word_wrap: para.word_wrap,
                previous_frame_pr: para.frame_pr.clone(),
                previous_preserved_ppr: para.preserved_ppr.clone(),
                author: revision.author.clone().unwrap_or_default(),
                date: revision.date.clone(),
            });
            // The verb AUTHORS a direct numPr, so claim provenance: emit the
            // numbering it just set (or, when clearing, drop the gate too).
            para.has_direct_numbering = new_numbering.is_some();
            para.numbering = new_numbering;
        }
        // Direct mutation: set the new numbering, no tracked change.
        MaterializationMode::Direct => {
            para.has_direct_numbering = new_numbering.is_some();
            para.numbering = new_numbering;
            para.formatting_change = None;
        }
    }

    Ok(())
}

/// Split a list at `block_id`: re-point the split item and every CONTIGUOUS
/// following top-level paragraph that shares its `num_id` at a freshly-allocated
/// `num_id`, and stage a [`NumberingOp::CreateDefinition`] so the save path
/// authors that `num_id`'s definition (cloning the source list's levels).
///
/// "Contiguous" means: starting at the split item, consume forward as long as
/// the next top-level block is a paragraph whose live numbering points at the
/// SAME original `num_id` AND the same `ilvl`. A different num_id, a different
/// level, a non-list paragraph, or a table ends the run. Items at a deeper
/// `ilvl` between two split items are NOT re-pointed in v1 — the run is the flat
/// sequence of items at the split's own level; this keeps the re-point set
/// well-defined. (A nested-list split is out of scope; the common case —
/// splitting a flat numbered list — is exactly this.)
///
/// The tail is re-pointed at a sentinel PLACEHOLDER `num_id` (see
/// [`SPLIT_PLACEHOLDER_NUM_ID_BASE`]); the save path allocates the real
/// `num_id` and matching `abstractNumId` against `word/numbering.xml` and
/// rewrites the placeholder.
fn apply_split(
    doc: &mut CanonDoc,
    block_id: &NodeId,
    semantic_hash: Option<&str>,
    revision: &RevisionInfo,
    step_index: usize,
    numbering_ops: &mut Vec<NumberingOp>,
) -> Result<(), EditError> {
    let idx = find_block_index(&doc.blocks, block_id).ok_or_else(|| EditError::BlockNotFound {
        block_id: block_id.clone(),
        step_index,
    })?;

    // Resolve the split point's current numbering. Must be a list item.
    let (source_num_id, source_ilvl) = match &doc.blocks[idx].block {
        BlockNode::Paragraph(p) => match &p.numbering {
            Some(n) => (n.num_id, n.ilvl),
            None => {
                return Err(EditError::NumberingSplitOnUnnumbered {
                    block_id: block_id.clone(),
                    step_index,
                });
            }
        },
        BlockNode::Table(_) => {
            return Err(EditError::NotAParagraph {
                block_id: block_id.clone(),
                actual_kind: "table",
                step_index,
            });
        }
        BlockNode::OpaqueBlock(_) => {
            return Err(EditError::NotAParagraph {
                block_id: block_id.clone(),
                actual_kind: "opaque_block",
                step_index,
            });
        }
    };

    // Stage a PLACEHOLDER num_id (see SPLIT_PLACEHOLDER_NUM_ID_BASE): the pure
    // verb core cannot see the numbering part, so it does not allocate the real
    // id — the save path does, against the part's authoritative population, and
    // rewrites this placeholder in the re-pointed paragraphs' live `w:numPr`. A
    // distinct placeholder per staged definition (one per split in this
    // transaction) keeps that save-path rewrite unambiguous when a single
    // transaction splits several lists.
    let placeholder_num_id = SPLIT_PLACEHOLDER_NUM_ID_BASE
        .checked_add(numbering_ops.len() as u32)
        .expect("list-split placeholder space exhausted — not reachable for real transactions");

    // Collect the contiguous run of block ids to re-point: the split item plus
    // following top-level paragraphs at the SAME (num_id, ilvl). We collect ids
    // (not indices) first so we can re-point each via the shared paragraph path.
    let mut run_ids: Vec<NodeId> = Vec::new();
    for tb in &doc.blocks[idx..] {
        match &tb.block {
            BlockNode::Paragraph(p) => match &p.numbering {
                Some(n) if n.num_id == source_num_id && n.ilvl == source_ilvl => {
                    run_ids.push(p.id.clone());
                }
                // Same list but a deeper/shallower level, a different list, or no
                // list ends the contiguous run.
                _ => break,
            },
            // A table (or opaque block) breaks the run.
            _ => break,
        }
    }

    debug_assert!(
        !run_ids.is_empty(),
        "the split item itself always matches its own (num_id, ilvl)"
    );

    // Re-point each item in the run at the new num_id, keeping ilvl, as a tracked
    // pPrChange (same shape SetType produces). The split point carries the
    // caller-supplied semantic-hash guard; the rest are unguarded (they were
    // resolved from the live run, not addressed by the caller).
    for (run_pos, run_id) in run_ids.iter().enumerate() {
        let hash = if run_pos == 0 { semantic_hash } else { None };
        let run_idx =
            find_block_index(&doc.blocks, run_id).ok_or_else(|| EditError::BlockNotFound {
                block_id: run_id.clone(),
                step_index,
            })?;
        validate_block_is_editable(&doc.blocks[run_idx], step_index)?;

        let BlockNode::Paragraph(para) = &mut doc.blocks[run_idx].block else {
            unreachable!("run ids were collected from paragraphs only");
        };

        if let Some(expected) = hash
            && let Err(actual) = check_block_guard(&BlockNode::Paragraph(para.clone()), expected)
        {
            return Err(EditError::BlockSemanticHashMismatch {
                block_id: run_id.clone(),
                expected: expected.to_string(),
                actual,
                step_index,
            });
        }

        // Reuse the single-paragraph re-point (SetType keeps ilvl, swaps num_id).
        // Carry the current is_bullet/synthesized_text so the diff hint is not
        // fabricated; Word re-derives the label from the cloned definition.
        let (synthesized_text, is_bullet) = para
            .numbering
            .as_ref()
            .map(|n| (n.synthesized_text.clone(), n.is_bullet))
            .expect("run paragraph has numbering (collected above)");

        apply_to_paragraph(
            para,
            run_id,
            &NumberingChange::SetType {
                num_id: placeholder_num_id,
                synthesized_text,
                is_bullet,
            },
            revision,
            MaterializationMode::TrackedChange,
            step_index,
        )?;
    }

    // Stage the new definition for the save path. Cloned from the SOURCE num_id
    // so the new list's levels render identically; fails loud at save if the
    // source has no resolvable abstractNum.
    numbering_ops.push(NumberingOp::CreateDefinition {
        placeholder_num_id,
        cloned_from_num_id: source_num_id,
    });

    Ok(())
}

/// Compute the new `NumberingInfo` for an indent (`delta = 1`) or outdent
/// (`delta = -1`). Refuses on an unnumbered paragraph and at the 0..=8 bounds.
/// `num_id`/`is_bullet`/`synthesized_text` are carried from the current level —
/// Word re-derives the displayed label from `word/numbering.xml` at the new
/// `ilvl`, so the IR's `synthesized_text` (a non-serialized diff hint) is not
/// fabricated for the new level.
fn relevel(
    para: &ParagraphNode,
    block_id: &NodeId,
    step_index: usize,
    delta: i64,
) -> Result<NumberingInfo, EditError> {
    let current = para
        .numbering
        .as_ref()
        .ok_or_else(|| EditError::NumberingLevelOnUnnumbered {
            block_id: block_id.clone(),
            step_index,
        })?;
    let new_ilvl = current.ilvl as i64 + delta;
    if new_ilvl < 0 || new_ilvl > MAX_ILVL as i64 {
        return Err(EditError::NumberingLevelOutOfBounds {
            block_id: block_id.clone(),
            requested: new_ilvl,
            step_index,
        });
    }
    Ok(NumberingInfo {
        num_id: current.num_id,
        ilvl: new_ilvl as u32,
        synthesized_text: current.synthesized_text.clone(),
        is_bullet: current.is_bullet,
        // Re-leveling does not request a counter restart.
        restart_numbering: false,
    })
}

/// Compute the new `NumberingInfo` for a Restart (`restart = true`) or Continue
/// (`restart = false`), keeping `num_id`/`ilvl`/`is_bullet`/`synthesized_text`.
/// Refuses on an unnumbered paragraph.
fn set_restart(
    para: &ParagraphNode,
    block_id: &NodeId,
    step_index: usize,
    restart: bool,
) -> Result<NumberingInfo, EditError> {
    let current = para
        .numbering
        .as_ref()
        .ok_or_else(|| EditError::NumberingLevelOnUnnumbered {
            block_id: block_id.clone(),
            step_index,
        })?;
    Ok(NumberingInfo {
        num_id: current.num_id,
        ilvl: current.ilvl,
        synthesized_text: current.synthesized_text.clone(),
        is_bullet: current.is_bullet,
        restart_numbering: restart,
    })
}

/// A request is a no-op when the new numbering is structurally equal to the
/// current one AND carries the same restart intent. Bare-restart of the same
/// list is therefore NOT a no-op.
fn is_noop(current: &Option<NumberingInfo>, new: &Option<NumberingInfo>) -> bool {
    match (current, new) {
        (None, None) => true,
        (Some(c), Some(n)) => c.structurally_eq(n) && c.restart_numbering == n.restart_numbering,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{NumberingInfo, StyleProps, TrackedSegment, TrackingStatus};

    fn ninfo(num_id: u32, ilvl: u32) -> NumberingInfo {
        NumberingInfo {
            num_id,
            ilvl,
            synthesized_text: "1.".to_string(),
            is_bullet: false,
            restart_numbering: false,
        }
    }

    /// A minimal Normal paragraph with every field defaulted, parameterized only
    /// by id + numbering — the two fields the refusal logic reads.
    fn bare_para(id: &str, numbering: Option<NumberingInfo>) -> ParagraphNode {
        ParagraphNode {
            id: NodeId::new(id.to_string()),
            style_id: None,
            align: None,
            has_direct_align: false,
            indent: None,
            has_direct_indent: false,
            authored_indent: None,
            spacing: None,
            has_direct_spacing: false,
            authored_spacing: None,
            borders: None,
            keep_next: None,
            keep_lines: None,
            page_break_before: false,
            widow_control: None,
            contextual_spacing: None,
            shading: None,
            has_direct_keep_next: true,
            has_direct_keep_lines: true,
            has_direct_page_break_before: true,
            has_direct_widow_control: true,
            has_direct_contextual_spacing: true,
            has_direct_shading: true,
            has_direct_borders: true,
            tab_stops: vec![],
            effective_tab_stops_rel: vec![],
            segments: vec![TrackedSegment {
                status: TrackingStatus::Normal,
                inlines: vec![],
            }],
            block_text_hash: None,
            numbering,
            has_direct_numbering: true,
            numbering_suppressed: false,
            materialized_numbering: None,
            rendered_text: None,
            literal_prefix: None,
            literal_prefix_marks: Vec::new(),
            literal_prefix_style_props: StyleProps::default(),
            literal_prefix_rpr_authored: crate::domain::RunRprAuthored::default(),
            literal_prefix_leading_rpr: None,
            literal_prefix_trailing_rpr: None,
            literal_prefix_leading_tab_twips: None,
            literal_prefix_leading_tab_count: 0,
            literal_prefix_leading_ws: String::new(),
            literal_prefix_trailing_ws: String::new(),
            literal_prefix_has_trailing_tab: false,
            literal_prefix_trailing_tab_stop_twips: None,
            outline_lvl: None,
            heading_level: None,
            para_mark_status: None,
            paragraph_mark_marks: vec![],
            paragraph_mark_style_props: StyleProps::default(),
            paragraph_mark_rpr_off: Default::default(),
            para_split: false,
            section_property_change: None,
            formatting_change: None,
            section_properties: None,
            mirror_indents: None,
            auto_space_de: None,
            auto_space_dn: None,
            bidi: None,
            text_alignment: None,
            suppress_auto_hyphens: None,
            snap_to_grid: None,
            overflow_punct: None,
            adjust_right_ind: None,
            word_wrap: None,
            frame_pr: None,
            para_id: None,
            text_id: None,
            text_direction: None,
            cnf_style: None,
            preserved_ppr: Vec::new(),
        }
    }

    fn rev() -> RevisionInfo {
        RevisionInfo {
            revision_id: 1,
            author: Some("Test".to_string()),
            date: Some("2026-06-01T00:00:00Z".to_string()),
            apply_op_id: None,
        }
    }

    #[test]
    fn set_level_on_unnumbered_paragraph_is_refused() {
        let mut p = bare_para("p1", None);
        let err = apply_to_paragraph(
            &mut p,
            &NodeId::new("p1".to_string()),
            &NumberingChange::SetLevel {
                ilvl: 1,
                synthesized_text: "a.".to_string(),
                is_bullet: false,
            },
            &rev(),
            MaterializationMode::TrackedChange,
            0,
        )
        .unwrap_err();
        assert!(matches!(err, EditError::NumberingLevelOnUnnumbered { .. }));
    }

    #[test]
    fn structurally_equal_request_is_noop_refused() {
        let mut p = bare_para("p1", Some(ninfo(3, 0)));
        let err = apply_to_paragraph(
            &mut p,
            &NodeId::new("p1".to_string()),
            &NumberingChange::SetList {
                num_id: 3,
                ilvl: 0,
                restart: false,
                synthesized_text: "1.".to_string(),
                is_bullet: false,
            },
            &rev(),
            MaterializationMode::TrackedChange,
            0,
        )
        .unwrap_err();
        assert!(matches!(err, EditError::NoNumberingChangeRequested { .. }));
    }

    #[test]
    fn bare_restart_of_same_list_is_not_a_noop() {
        // restart=true on the same num_id/ilvl flips restart_numbering, which is
        // a real change — it must be authored, not refused as a no-op.
        let mut p = bare_para("p1", Some(ninfo(3, 0)));
        apply_to_paragraph(
            &mut p,
            &NodeId::new("p1".to_string()),
            &NumberingChange::SetList {
                num_id: 3,
                ilvl: 0,
                restart: true,
                synthesized_text: "1.".to_string(),
                is_bullet: false,
            },
            &rev(),
            MaterializationMode::Direct,
            0,
        )
        .expect("bare restart is a real change");
        assert!(p.numbering.as_ref().unwrap().restart_numbering);
    }

    #[test]
    fn manual_prefix_paragraph_is_refused() {
        let mut p = bare_para("p1", None);
        p.literal_prefix = Some("(a)".to_string());
        let err = apply_to_paragraph(
            &mut p,
            &NodeId::new("p1".to_string()),
            &NumberingChange::SetList {
                num_id: 3,
                ilvl: 0,
                restart: false,
                synthesized_text: "1.".to_string(),
                is_bullet: false,
            },
            &rev(),
            MaterializationMode::TrackedChange,
            0,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            EditError::NumberingManualPrefixUnsupported { .. }
        ));
    }

    #[test]
    fn second_tracked_change_is_refused() {
        let mut p = bare_para("p1", None);
        // First tracked change: attach a list.
        apply_to_paragraph(
            &mut p,
            &NodeId::new("p1".to_string()),
            &NumberingChange::SetList {
                num_id: 3,
                ilvl: 0,
                restart: false,
                synthesized_text: "1.".to_string(),
                is_bullet: false,
            },
            &rev(),
            MaterializationMode::TrackedChange,
            0,
        )
        .expect("first change attaches list");
        // Second tracked change on the same (already-tracked) paragraph: refused.
        let err = apply_to_paragraph(
            &mut p,
            &NodeId::new("p1".to_string()),
            &NumberingChange::SetLevel {
                ilvl: 1,
                synthesized_text: "a.".to_string(),
                is_bullet: false,
            },
            &rev(),
            MaterializationMode::TrackedChange,
            1,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            EditError::UnsupportedParagraphStructure { .. }
        ));
    }

    #[test]
    fn tracked_attach_records_previous_absent_signal() {
        let mut p = bare_para("p1", None);
        apply_to_paragraph(
            &mut p,
            &NodeId::new("p1".to_string()),
            &NumberingChange::SetList {
                num_id: 3,
                ilvl: 0,
                restart: false,
                synthesized_text: "1.".to_string(),
                is_bullet: false,
            },
            &rev(),
            MaterializationMode::TrackedChange,
            0,
        )
        .expect("attach list");
        let fc = p.formatting_change.as_ref().expect("formatting change set");
        assert!(fc.previous_numbering.is_none());
        // Base had neither numPr nor literal prefix → explicitly-absent signal.
        assert!(fc.previous_numbering_explicitly_absent);
        assert_eq!(p.numbering.as_ref().unwrap().num_id, 3);
    }

    #[test]
    fn indent_increments_level_keeping_num_id() {
        let mut p = bare_para("p1", Some(ninfo(3, 0)));
        apply_to_paragraph(
            &mut p,
            &NodeId::new("p1".to_string()),
            &NumberingChange::Indent,
            &rev(),
            MaterializationMode::Direct,
            0,
        )
        .expect("indent");
        let n = p.numbering.as_ref().unwrap();
        assert_eq!(n.ilvl, 1);
        assert_eq!(n.num_id, 3, "indent keeps the same list");
    }

    #[test]
    fn outdent_at_level_zero_is_out_of_bounds() {
        let mut p = bare_para("p1", Some(ninfo(3, 0)));
        let err = apply_to_paragraph(
            &mut p,
            &NodeId::new("p1".to_string()),
            &NumberingChange::Outdent,
            &rev(),
            MaterializationMode::Direct,
            0,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            EditError::NumberingLevelOutOfBounds { requested: -1, .. }
        ));
    }

    #[test]
    fn indent_at_level_eight_is_out_of_bounds() {
        let mut p = bare_para("p1", Some(ninfo(3, 8)));
        let err = apply_to_paragraph(
            &mut p,
            &NodeId::new("p1".to_string()),
            &NumberingChange::Indent,
            &rev(),
            MaterializationMode::Direct,
            0,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            EditError::NumberingLevelOutOfBounds { requested: 9, .. }
        ));
    }

    #[test]
    fn indent_on_unnumbered_is_refused() {
        let mut p = bare_para("p1", None);
        let err = apply_to_paragraph(
            &mut p,
            &NodeId::new("p1".to_string()),
            &NumberingChange::Indent,
            &rev(),
            MaterializationMode::Direct,
            0,
        )
        .unwrap_err();
        assert!(matches!(err, EditError::NumberingLevelOnUnnumbered { .. }));
    }

    #[test]
    fn restart_sets_restart_flag() {
        let mut p = bare_para("p1", Some(ninfo(3, 0)));
        apply_to_paragraph(
            &mut p,
            &NodeId::new("p1".to_string()),
            &NumberingChange::Restart,
            &rev(),
            MaterializationMode::Direct,
            0,
        )
        .expect("restart");
        assert!(p.numbering.as_ref().unwrap().restart_numbering);
    }

    #[test]
    fn continue_on_already_continuing_list_is_noop_refused() {
        // ninfo() has restart_numbering=false already, so Continue is a no-op.
        let mut p = bare_para("p1", Some(ninfo(3, 0)));
        let err = apply_to_paragraph(
            &mut p,
            &NodeId::new("p1".to_string()),
            &NumberingChange::Continue,
            &rev(),
            MaterializationMode::Direct,
            0,
        )
        .unwrap_err();
        assert!(matches!(err, EditError::NoNumberingChangeRequested { .. }));
    }

    #[test]
    fn set_type_swaps_num_id_keeping_level() {
        let mut p = bare_para("p1", Some(ninfo(3, 2)));
        apply_to_paragraph(
            &mut p,
            &NodeId::new("p1".to_string()),
            &NumberingChange::SetType {
                num_id: 7,
                synthesized_text: String::new(),
                is_bullet: true,
            },
            &rev(),
            MaterializationMode::Direct,
            0,
        )
        .expect("set_type");
        let n = p.numbering.as_ref().unwrap();
        assert_eq!(n.num_id, 7, "re-pointed at the bullet list");
        assert_eq!(n.ilvl, 2, "level preserved across kind swap");
        assert!(n.is_bullet);
    }

    #[test]
    fn set_type_on_unnumbered_is_refused() {
        let mut p = bare_para("p1", None);
        let err = apply_to_paragraph(
            &mut p,
            &NodeId::new("p1".to_string()),
            &NumberingChange::SetType {
                num_id: 7,
                synthesized_text: "1.".to_string(),
                is_bullet: false,
            },
            &rev(),
            MaterializationMode::Direct,
            0,
        )
        .unwrap_err();
        assert!(matches!(err, EditError::NumberingLevelOnUnnumbered { .. }));
    }
}
