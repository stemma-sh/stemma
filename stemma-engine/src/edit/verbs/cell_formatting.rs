//! `SetCellFormatting` — author a tracked cell-formatting change
//! (`w:tcPrChange`, §17.13.5.37). "Shade this cell / border it / right-align
//! it, tracked."
//!
//! This is the EXEMPLAR for the table/row/cell formatting long-tail. It mirrors
//! `paragraph_formatting.rs` at the cell level: it sets only the requested
//! `tcPr` properties (borders / shading / width / vertical alignment / margins)
//! **in place** on ONE cell and records the cell's prior `tcPr` in the existing
//! `CellFormattingChange`. It touches neither the materializer (Invariant M) nor
//! the serializer — a `tcPr` change is an in-place property delta, not a segment
//! insert/delete, so it bypasses segment lowering entirely. The existing
//! accept/reject projection already resolves `formatting_change`
//! (`tracked_model.rs`: reject restores `previous_width`/`previous_borders`/
//! `previous_shading`/`previous_v_align`/`previous_margins`; accept clears the
//! change, keeping the new `tcPr`), and the serializer already emits the
//! complete inner `tcPr` snapshot for `w:tcPrChange`. So this verb is a pure
//! authoring-side lift.
//!
//! ## In-place property edit (no whole-table rebuild)
//!
//! Like `apply_set_cell_text_in_place` (edit/mod.rs), this verb edits ONE cell
//! in place: it byte-preserves `tblPr`, every `trPr`, and all OTHER cells, and
//! only overwrites the target cell's requested `tcPr` properties — it never
//! rebuilds the table through the v4 grammar. (Historically it also had to
//! DELIBERATELY skip the blanket `validate_base_table_v4_compatible` refusal
//! that the whole-table REPLACE path used; RFC-0003 DELETED that guard — replace
//! now carries the base's formatting instead of dropping it — so there is no
//! blanket guard left to skip. The narrow `TableMidRedline` guard still applies
//! to the structural ops, not to this in-place property edit.)
//!
//! v1 scope (fail loud beyond it, per CLAUDE.md "start narrow"):
//! - top-level tables only (a nested table surfaces as `BlockNotFound`);
//! - the cell is addressed by LOGICAL `{row_index, col_index}` (after
//!   `gridBefore`, advancing by each cell's `gridSpan`) — the same address the
//!   read view mints, so a cold agent's read-off address resolves here;
//! - the cell must not already carry a `tcPrChange` (accept/reject it first);
//! - the patch grammar covers exactly the five `tcPr` properties the
//!   accept/reject projection restores (width/borders/shading/vAlign/margins).

use super::super::{
    CellFormattingPatch, EditError, MaterializationMode, find_block_index, snapshot_cell_formatting,
};
use crate::domain::{BlockNode, CanonDoc, NodeId, RevisionInfo, TableCellNode, VerticalMerge};
use crate::semantic_hash::check_block_guard;

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply(
    doc: &mut CanonDoc,
    block_id: &NodeId,
    row_index: usize,
    col_index: usize,
    semantic_hash: Option<&str>,
    patch: &CellFormattingPatch,
    revision: &RevisionInfo,
    mode: MaterializationMode,
    step_index: usize,
) -> Result<(), EditError> {
    // Refuse a no-op request before touching anything — no empty tcPrChange
    // (CLAUDE.md "no silent fallbacks").
    if patch.is_empty() {
        return Err(EditError::NoCellFormattingRequested { step_index });
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

    // Resolve `col_index` (a LOGICAL grid column) to the physical cell that
    // STARTS at that column, advancing by each cell's gridSpan after the row's
    // gridBefore — exactly how `apply_set_cell_text_in_place` and the read view
    // mint the address. Require an exact start match: addressing the interior of
    // a spanning cell is out of range rather than silently snapped to the anchor.
    let mut col = row.grid_before as usize;
    let mut found: Option<usize> = None;
    let mut logical_width = col;
    for (phys_idx, cell) in row.cells.iter().enumerate() {
        if col == col_index {
            found = Some(phys_idx);
        }
        col += cell.grid_span.max(1) as usize;
        logical_width = col;
    }
    logical_width += row.grid_after as usize;
    let phys_idx = found.ok_or_else(|| EditError::TableColumnIndexOutOfRange {
        block_id: block_id.clone(),
        col_index,
        column_count: logical_width,
        step_index,
    })?;

    let cell = &mut row.cells[phys_idx];

    // A vertical-merge CONTINUE cell holds no formatting of its own — its tcPr
    // belongs to the merge anchor in a higher row. Refuse and point at the
    // anchor (no silent retarget), matching the in-place cell-text verb.
    if cell.v_merge == VerticalMerge::Continue {
        return Err(EditError::TableCellNotEditable {
            block_id: block_id.clone(),
            reason: format!(
                "cell at row {row_index}, col {col_index} is a vertical-merge \
                 continuation; its formatting lives in the merge anchor — format the anchor cell"
            ),
            step_index,
        });
    }

    // A cell carrying a tracked structural insert/delete is not a clean format
    // target — accept/reject the structural change first.
    if cell.tracking_status.is_some() {
        return Err(EditError::TableCellNotEditable {
            block_id: block_id.clone(),
            reason: format!(
                "cell at row {row_index}, col {col_index} carries a tracked \
                 insert/delete; resolve it first"
            ),
            step_index,
        });
    }

    // Refuse to stack a new tracked tcPrChange on a cell that already carries
    // one — accept or reject the existing change first. Mirrors the paragraph-
    // and run-level guards.
    if cell.formatting_change.is_some() {
        return Err(EditError::TableCellNotEditable {
            block_id: block_id.clone(),
            reason: format!(
                "cell at row {row_index}, col {col_index} already has a tracked \
                 formatting change (tcPrChange); accept or reject it before formatting again"
            ),
            step_index,
        });
    }

    apply_to_cell(cell, patch, revision, mode);
    Ok(())
}

/// Short-circuit if the patch sets exactly the cell's current value(s) (no
/// visible change → no empty tcPrChange), then apply only the requested
/// properties and — in `TrackedChange` mode — snapshot the prior `tcPr` into
/// `formatting_change`.
fn apply_to_cell(
    cell: &mut TableCellNode,
    patch: &CellFormattingPatch,
    revision: &RevisionInfo,
    mode: MaterializationMode,
) {
    // No-op short-circuit: a request that sets exactly the current value(s)
    // would produce a visually-empty tcPrChange. Compare only the fields the
    // patch would overwrite.
    let no_change = patch
        .borders
        .as_ref()
        .is_none_or(|b| Some(b) == cell.formatting.borders.as_ref())
        && patch
            .shading
            .as_ref()
            .is_none_or(|s| Some(s) == cell.formatting.shading.as_ref())
        && patch
            .width
            .as_ref()
            .is_none_or(|w| Some(w) == cell.formatting.width.as_ref())
        && patch
            .v_align
            .as_ref()
            .is_none_or(|v| Some(v) == cell.formatting.v_align.as_ref())
        && patch
            .margins
            .as_ref()
            .is_none_or(|m| Some(m) == cell.formatting.margins.as_ref());
    if no_change {
        return;
    }

    // Snapshot BEFORE mutating so the inner tcPr of the tcPrChange is the
    // complete previous state (§17.13.5.37).
    let snapshot = snapshot_cell_formatting(cell, revision);

    // Apply only the requested properties. The serializer emits each
    // `cell.formatting.*` whenever `Some`, so setting the field is the whole
    // story (no `has_direct_*` gate, unlike run/paragraph properties). Every
    // other cell property — and every other cell, row, and the table's tblPr —
    // is left byte-identical.
    if let Some(borders) = &patch.borders {
        cell.formatting.borders = Some(borders.clone());
        // The edit AUTHORS this slot: claim provenance so the serializer emits
        // it (unauthored cell slots are stripped as table-style materialization).
        // The edit is the new authored set, so it also supersedes any authored
        // snapshot captured at import — otherwise the serializer would re-emit
        // the stale pre-edit edges instead of what the caller just set.
        cell.formatting.has_direct_borders = true;
        cell.formatting.authored_borders = Some(borders.clone());
    }
    if let Some(shading) = &patch.shading {
        cell.formatting.shading = Some(shading.clone());
        cell.formatting.has_direct_shading = true;
    }
    if let Some(width) = &patch.width {
        cell.formatting.width = Some(width.clone());
    }
    if let Some(v_align) = &patch.v_align {
        cell.formatting.v_align = Some(v_align.clone());
    }
    if let Some(margins) = &patch.margins {
        cell.formatting.margins = Some(margins.clone());
    }

    match mode {
        // Author a tracked change: record the previous tcPr; the serializer
        // emits w:tcPrChange, accept/reject resolves it.
        MaterializationMode::TrackedChange => {
            cell.formatting_change = Some(snapshot);
        }
        // Direct mutation: keep the new tcPr, no tracked change.
        MaterializationMode::Direct => {
            cell.formatting_change = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::CellFormattingPatch;
    use crate::domain::{Shading, VerticalAlignment};

    #[test]
    fn empty_patch_is_empty() {
        assert!(CellFormattingPatch::default().is_empty());
    }

    #[test]
    fn patch_with_shading_is_not_empty() {
        let patch = CellFormattingPatch {
            shading: Some(Shading {
                fill: Some("FFFF00".to_string()),
                val: None,
                color: None,
                extra_attrs: Vec::new(),
            }),
            ..Default::default()
        };
        assert!(!patch.is_empty());
    }

    #[test]
    fn patch_with_v_align_is_not_empty() {
        let patch = CellFormattingPatch {
            v_align: Some(VerticalAlignment::Center),
            ..Default::default()
        };
        assert!(!patch.is_empty());
    }
}
