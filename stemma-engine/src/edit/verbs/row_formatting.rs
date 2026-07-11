//! `SetRowFormatting` — author a tracked row-formatting change
//! (`w:trPrChange`, §17.13.5.36). "Make this row taller / pin its height,
//! tracked."
//!
//! This is the row-level sibling of `cell_formatting.rs` (the exemplar). It
//! sets only the requested `trPr` properties (row height + height rule) **in
//! place** on ONE row and records the row's prior height/height-rule in the
//! existing `RowFormattingChange`. It touches neither the materializer
//! (Invariant M) nor the serializer — a `trPr` change is an in-place property
//! delta, not a segment insert/delete, so it bypasses segment lowering
//! entirely. The existing accept/reject projection already resolves
//! `formatting_change` (`tracked_model.rs` ~5197: reject restores
//! `previous_height`/`previous_height_rule`; accept clears the change, keeping
//! the new `trHeight`), and the serializer already emits the complete inner
//! `trPr` snapshot for `w:trPrChange`. So this verb is a pure authoring-side
//! lift.
//!
//! ## In-place property edit (no whole-table rebuild)
//!
//! Like `cell_formatting::apply` and `apply_set_cell_text_in_place`
//! (edit/mod.rs), this verb edits ONE row in place: it byte-preserves `tblPr`,
//! every OTHER row, and every cell of the target row, and only overwrites the
//! target row's `trHeight` — it never rebuilds the table through the v4 grammar.
//! (RFC-0003 deleted the old blanket `validate_base_table_v4_compatible`
//! refusal; `replace(table)` now carries base formatting rather than dropping
//! it, so there is no blanket guard to skip.)
//!
//! v1 scope (fail loud beyond it, per CLAUDE.md "start narrow"):
//! - top-level tables only (a nested table surfaces as `BlockNotFound`);
//! - the row is addressed by `row_index` (the same address the read view mints);
//! - the row must not carry a tracked structural insert/delete;
//! - the row must not already carry a `trPrChange` (accept/reject it first);
//! - the patch grammar covers exactly the two `trPr` properties the
//!   accept/reject projection restores (height/height_rule).

use super::super::{
    EditError, MaterializationMode, RowFormattingPatch, find_block_index, snapshot_row_formatting,
};
use crate::domain::{BlockNode, CanonDoc, NodeId, RevisionInfo, TableRowNode};
use crate::semantic_hash::check_block_guard;

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply(
    doc: &mut CanonDoc,
    block_id: &NodeId,
    row_index: usize,
    semantic_hash: Option<&str>,
    patch: &RowFormattingPatch,
    revision: &RevisionInfo,
    mode: MaterializationMode,
    step_index: usize,
) -> Result<(), EditError> {
    // Refuse a no-op request before touching anything — no empty trPrChange
    // (CLAUDE.md "no silent fallbacks").
    if patch.is_empty() {
        return Err(EditError::NoRowFormattingRequested { step_index });
    }

    // Resolve the table block. A non-table target reuses the same NotATable
    // error the whole-table verbs raise.
    let idx = find_block_index(&doc.blocks, block_id).ok_or_else(|| EditError::BlockNotFound {
        block_id: block_id.clone(),
        step_index,
    })?;
    match &doc.blocks[idx].block {
        BlockNode::Table(_) => {}
        BlockNode::Paragraph(_) => {
            return Err(EditError::NotATable {
                block_id: block_id.clone(),
                actual_kind: "paragraph",
                step_index,
            });
        }
        BlockNode::OpaqueBlock(_) => {
            return Err(EditError::NotATable {
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

    let BlockNode::Table(table) = &mut doc.blocks[idx].block else {
        unreachable!("checked table above");
    };

    if row_index >= table.rows.len() {
        return Err(EditError::TableRowIndexOutOfRange {
            block_id: block_id.clone(),
            row_index,
            row_count: table.rows.len(),
            step_index,
        });
    }
    let row = &mut table.rows[row_index];

    // A row carrying a tracked structural insert/delete is not a clean format
    // target — accept/reject the structural change first (mirrors the cell-level
    // tracking-status guard).
    if row.tracking_status.is_some() {
        return Err(EditError::TableRowNotEditable {
            block_id: block_id.clone(),
            reason: format!("row {row_index} carries a tracked insert/delete; resolve it first"),
            step_index,
        });
    }

    // Refuse to stack a new tracked trPrChange on a row that already carries one
    // — accept or reject the existing change first. Mirrors the cell-, paragraph-
    // and run-level guards.
    if row.formatting_change.is_some() {
        return Err(EditError::TableRowNotEditable {
            block_id: block_id.clone(),
            reason: format!(
                "row {row_index} already has a tracked formatting change \
                 (trPrChange); accept or reject it before formatting again"
            ),
            step_index,
        });
    }

    apply_to_row(row, patch, revision, mode);
    Ok(())
}

/// Short-circuit if the patch sets exactly the row's current value(s) (no
/// visible change → no empty trPrChange), then apply only the requested
/// properties and — in `TrackedChange` mode — snapshot the prior `trPr` into
/// `formatting_change`.
fn apply_to_row(
    row: &mut TableRowNode,
    patch: &RowFormattingPatch,
    revision: &RevisionInfo,
    mode: MaterializationMode,
) {
    // No-op short-circuit: a request that sets exactly the current value(s)
    // would produce a visually-empty trPrChange. Compare only the fields the
    // patch would overwrite.
    let no_change = patch.height.is_none_or(|h| Some(h) == row.height)
        && patch
            .height_rule
            .as_ref()
            .is_none_or(|r| Some(r) == row.height_rule.as_ref());
    if no_change {
        return;
    }

    // Snapshot BEFORE mutating so the inner trPr of the trPrChange is the
    // complete previous state (§17.13.5.36).
    let snapshot = snapshot_row_formatting(row, revision);

    // Apply only the requested properties. The serializer emits `trHeight`
    // whenever `height` is `Some`, so setting the field is the whole story.
    // Every other row property — and every other row, the cells, and the
    // table's tblPr — is left byte-identical.
    if let Some(height) = patch.height {
        row.height = Some(height);
    }
    if let Some(rule) = &patch.height_rule {
        row.height_rule = Some(rule.clone());
    }

    match mode {
        // Author a tracked change: record the previous trPr; the serializer
        // emits w:trPrChange, accept/reject resolves it.
        MaterializationMode::TrackedChange => {
            row.formatting_change = Some(snapshot);
        }
        // Direct mutation: keep the new trPr, no tracked change.
        MaterializationMode::Direct => {
            row.formatting_change = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::RowFormattingPatch;
    use crate::domain::HeightRule;

    #[test]
    fn empty_patch_is_empty() {
        assert!(RowFormattingPatch::default().is_empty());
    }

    #[test]
    fn patch_with_height_is_not_empty() {
        let patch = RowFormattingPatch {
            height: Some(720),
            ..Default::default()
        };
        assert!(!patch.is_empty());
    }

    #[test]
    fn patch_with_height_rule_is_not_empty() {
        let patch = RowFormattingPatch {
            height_rule: Some(HeightRule::Exact),
            ..Default::default()
        };
        assert!(!patch.is_empty());
    }
}
