//! `SetParagraphFormatting` — author a tracked paragraph-formatting change
//! (`w:pPrChange`, §17.13.5.29). "Center this clause / indent it, tracked."
//!
//! Mirrors the `SetRunFormatting` reference verb (`run_formatting.rs`) at the
//! paragraph level: it sets only the requested pPr attributes (alignment,
//! indentation, line + before/after spacing) **in place** and records the
//! paragraph's prior pPr in the existing `ParagraphFormattingChange`. It does
//! **not** swap the paragraph role (that stays role-only via
//! `SetBlockRangeAttr`), and it does **not** touch the materializer
//! (Invariant M) — a pPr change is an in-place property delta, not a segment
//! insert/delete, so it bypasses segment lowering entirely.
//!
//! The existing accept/reject projection already resolves `formatting_change`
//! (`tracked_model.rs`: reject restores `previous_alignment` /
//! `previous_indentation` / `previous_spacing`; accept clears the change,
//! keeping the new pPr), and the serializer already emits the complete inner
//! pPr snapshot for `w:pPrChange`. So this verb is a pure authoring-side lift.
//!
//! v1 scope (fail loud beyond it, per CLAUDE.md "start narrow"):
//! - top-level paragraphs only (a nested in-cell paragraph surfaces as
//!   `BlockNotFound`);
//! - the paragraph must be Normal with no existing tracked segments;
//! - the paragraph must not already carry a `pPrChange` (accept/reject it
//!   first);
//! - only alignment / indentation / spacing are in the patch grammar; any
//!   other pPr attr (keep options, borders, shading, numbering) stays role-only
//!   via `SetBlockRangeAttr`.

use super::super::{
    EditError, MaterializationMode, ParagraphFormattingPatch, next_revision,
    snapshot_paragraph_formatting,
};
use super::super::{
    block_at, block_at_mut, check_ancestor_table_tracking, find_paragraph_path,
    validate_block_is_editable,
};
use crate::domain::{BlockNode, CanonDoc, NodeId, ParagraphNode, RevisionInfo};
use crate::semantic_hash::check_block_guard;

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply(
    doc: &mut CanonDoc,
    block_id: &NodeId,
    semantic_hash: Option<&str>,
    patch: &ParagraphFormattingPatch,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    mode: MaterializationMode,
    step_index: usize,
) -> Result<(), EditError> {
    // Refuse a no-op request before touching anything — no empty pPrChange
    // (CLAUDE.md "no silent fallbacks").
    if patch.is_empty() {
        return Err(EditError::NoParagraphFormattingRequested { step_index });
    }

    // Resolve the target paragraph anywhere — top-level OR inside a table cell
    // (find_paragraph_path recurses into cells). In-cell pPr gates the same way as
    // a text replace: the block must be editable, and no enclosing row/cell may be
    // tracked-inserted/deleted.
    let path = find_paragraph_path(doc, block_id).ok_or_else(|| EditError::BlockNotFound {
        block_id: block_id.clone(),
        step_index,
    })?;
    if path.is_top_level() {
        validate_block_is_editable(&doc.blocks[path.top_block], step_index)?;
    } else {
        check_ancestor_table_tracking(doc, &path, block_id, step_index)?;
    }

    // Block kind + staleness guard (immutable borrow, released before the apply).
    {
        let block = block_at(doc, &path);
        match block {
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
            && let Err(actual) = check_block_guard(block, expected)
        {
            return Err(EditError::BlockSemanticHashMismatch {
                block_id: block_id.clone(),
                expected: expected.to_string(),
                actual,
                step_index,
            });
        }
    }

    let BlockNode::Paragraph(para) = block_at_mut(doc, &path) else {
        unreachable!("checked paragraph above");
    };

    // Refuse to stack a new tracked pPrChange on a paragraph that already
    // carries one — accept or reject the existing change first. Mirrors the
    // run-level guard in run_formatting.rs.
    if para.formatting_change.is_some() {
        return Err(EditError::UnsupportedParagraphStructure {
            block_id: block_id.clone(),
            reason: "the paragraph already has a tracked formatting change (pPrChange); \
                     accept or reject it before formatting again"
                .to_string(),
            step_index,
        });
    }

    apply_to_paragraph(para, patch, revision, rev_counter, mode);
    Ok(())
}

/// Compute the would-be new pPr, short-circuit if it equals the current pPr
/// (no visible change → no empty pPrChange), then apply the patch and — in
/// `TrackedChange` mode — snapshot the prior pPr into `formatting_change`.
fn apply_to_paragraph(
    para: &mut ParagraphNode,
    patch: &ParagraphFormattingPatch,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    mode: MaterializationMode,
) {
    // No-op short-circuit: a request that sets exactly the current value(s)
    // would produce a visually-empty pPrChange. Compare only the fields the
    // patch would overwrite.
    let no_change = patch
        .align
        .as_ref()
        .is_none_or(|a| Some(a) == para.align.as_ref())
        && patch
            .indent
            .as_ref()
            .is_none_or(|i| Some(i) == para.indent.as_ref())
        && patch
            .spacing
            .as_ref()
            .is_none_or(|s| Some(s) == para.spacing.as_ref())
        && patch
            .borders
            .as_ref()
            .is_none_or(|b| Some(b) == para.borders.as_ref())
        && patch
            .shading
            .as_ref()
            .is_none_or(|s| Some(s) == para.shading.as_ref());
    if no_change {
        return;
    }

    // Snapshot BEFORE mutating so the inner pPr of the pPrChange is the
    // complete previous state (§17.13.5.29).
    let rev_for_change = next_revision(revision, rev_counter);
    let snapshot = snapshot_paragraph_formatting(para, &rev_for_change);

    // Apply only the requested attributes, marking each as direct so the
    // serializer emits the new value (it gates emission on has_direct_*).
    if let Some(align) = &patch.align {
        para.align = Some(align.clone());
        para.has_direct_align = true;
    }
    if let Some(indent) = &patch.indent {
        para.indent = Some(indent.clone());
        para.has_direct_indent = true;
        // The edit AUTHORS this as direct pPr — it is both the effective and the
        // authored value (no inheritance to resolve for an explicit patch).
        para.authored_indent = Some(indent.clone());
    }
    if let Some(spacing) = &patch.spacing {
        para.spacing = Some(spacing.clone());
        para.has_direct_spacing = true;
        para.authored_spacing = Some(spacing.clone());
    }
    // The edit AUTHORS these slots: claim provenance so the serializer emits
    // them (unauthored pPr slots are stripped as style materialization).
    if let Some(borders) = &patch.borders {
        para.borders = Some(borders.clone());
        para.has_direct_borders = true;
    }
    if let Some(shading) = &patch.shading {
        para.shading = Some(shading.clone());
        para.has_direct_shading = true;
    }

    // Clear cached text hash / rendered text so any downstream consumer that
    // compares against a hash recomputes (mirrors SetBlockRangeAttr).
    para.block_text_hash = None;
    para.rendered_text = None;

    match mode {
        // Author a tracked change: record the previous pPr; the serializer
        // emits w:pPrChange, accept/reject resolves it.
        MaterializationMode::TrackedChange => {
            para.formatting_change = Some(snapshot);
        }
        // Direct mutation: keep the new pPr, no tracked change.
        MaterializationMode::Direct => {
            para.formatting_change = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::ParagraphFormattingPatch;
    use crate::domain::{Alignment, Indentation};

    #[test]
    fn empty_patch_is_empty() {
        assert!(ParagraphFormattingPatch::default().is_empty());
    }

    #[test]
    fn patch_with_alignment_is_not_empty() {
        let patch = ParagraphFormattingPatch {
            align: Some(Alignment::Center),
            indent: None,
            spacing: None,
            borders: None,
            shading: None,
        };
        assert!(!patch.is_empty());
    }

    #[test]
    fn patch_with_indent_is_not_empty() {
        let patch = ParagraphFormattingPatch {
            align: None,
            indent: Some(Indentation {
                left: Some(720),
                right: None,
                effective_first_line_twips: None,
                start_chars: None,
                end_chars: None,
                first_line_chars: None,
                hanging_chars: None,
            }),
            spacing: None,
            borders: None,
            shading: None,
        };
        assert!(!patch.is_empty());
    }

    #[test]
    fn patch_with_shading_is_not_empty() {
        let patch = ParagraphFormattingPatch {
            align: None,
            indent: None,
            spacing: None,
            borders: None,
            shading: Some(crate::domain::Shading {
                fill: Some("FFFF00".to_string()),
                val: None,
                color: None,
                extra_attrs: Vec::new(),
            }),
        };
        assert!(!patch.is_empty());
    }
}
