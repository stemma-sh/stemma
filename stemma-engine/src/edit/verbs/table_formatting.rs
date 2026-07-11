//! `SetTableFormatting` — author a tracked table-level formatting change
//! (`w:tblPrChange`, §17.13.5.34). "Box this table / widen it / set its default
//! cell margins, tracked."
//!
//! This is the table-level sibling of `cell_formatting.rs` (`w:tcPrChange`). It
//! sets only the requested `tblPr` properties (borders / width / default cell
//! margins) **in place** on ONE table and records the table's prior `tblPr` in
//! the existing `TableFormattingChange`. It touches neither the materializer
//! (Invariant M) nor the serializer — a `tblPr` change is an in-place property
//! delta, not a segment insert/delete, so it bypasses segment lowering entirely.
//! The existing accept/reject projection already resolves `formatting_change`
//! (`tracked_model.rs`: reject restores `previous_width`/`previous_borders`/
//! `previous_default_cell_margins`; accept clears the change, keeping the new
//! `tblPr`), and the serializer already emits the complete inner `tblPr`
//! snapshot for `w:tblPrChange`. So this verb is a pure authoring-side lift.
//!
//! ## In-place property edit (no whole-table rebuild)
//!
//! Like `cell_formatting::apply` and `apply_set_cell_text_in_place`
//! (edit/mod.rs), this verb edits the table's `tblPr` in place: it byte-preserves
//! EVERY `w:tr`, EVERY `w:tc`, and all OTHER `tblPr` properties, only overwriting
//! the table's requested `tblPr` fields — a whole-table-replace is exactly what
//! this verb avoids. (RFC-0003 deleted the old blanket
//! `validate_base_table_v4_compatible` refusal; `replace(table)` now carries base
//! formatting rather than dropping it, so there is no blanket guard to skip.)
//!
//! v1 scope (fail loud beyond it, per CLAUDE.md "start narrow"):
//! - top-level tables only (a nested table surfaces as `NotATable`/`BlockNotFound`);
//! - the table must not already carry a `tblPrChange` (accept/reject it first);
//! - the patch grammar covers exactly the three `tblPr` properties the
//!   accept/reject projection restores (borders / width / default_cell_margins).
//!   Table-level shading is NOT in the grammar: `tblPr` carries none (cell
//!   shading lives on `w:tcPr`), so there is nothing for it to land on.

use super::super::{
    EditError, MaterializationMode, TableFormattingPatch, find_block_index, next_revision,
    snapshot_row_formatting, snapshot_table_formatting,
};
use crate::domain::{BlockNode, CanonDoc, NodeId, RevisionInfo, TableNode};
use crate::semantic_hash::check_block_guard;

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply(
    doc: &mut CanonDoc,
    block_id: &NodeId,
    semantic_hash: Option<&str>,
    patch: &TableFormattingPatch,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    mode: MaterializationMode,
    step_index: usize,
) -> Result<(), EditError> {
    // Refuse a no-op request before touching anything — no empty tblPrChange
    // (CLAUDE.md "no silent fallbacks").
    if patch.is_empty() {
        return Err(EditError::NoTableFormattingRequested { step_index });
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

    // Refuse to stack a new tracked tblPrChange on a table that already carries
    // one — accept or reject the existing change first. Mirrors the cell-,
    // paragraph-, and run-level guards.
    if table.formatting_change.is_some() {
        return Err(EditError::TableAlreadyHasFormattingChange {
            block_id: block_id.clone(),
            step_index,
        });
    }

    apply_to_table(table, patch, revision, rev_counter, mode);
    Ok(())
}

/// Short-circuit if the patch sets exactly the table's current value(s) (no
/// visible change → no empty tblPrChange), then apply only the requested
/// properties and — in `TrackedChange` mode — snapshot the prior `tblPr` into
/// `formatting_change`.
fn apply_to_table(
    table: &mut TableNode,
    patch: &TableFormattingPatch,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    mode: MaterializationMode,
) {
    // No-op short-circuit: a request that sets exactly the current value(s)
    // would produce a visually-empty tblPrChange. Compare only the fields the
    // patch would overwrite.
    let no_change = patch
        .borders
        .as_ref()
        .is_none_or(|b| Some(b) == table.formatting.borders.as_ref())
        && patch
            .width
            .as_ref()
            .is_none_or(|w| Some(w) == table.formatting.width.as_ref())
        && patch
            .default_cell_margins
            .as_ref()
            .is_none_or(|m| Some(m) == table.formatting.default_cell_margins.as_ref());
    if no_change {
        return;
    }

    // Snapshot BEFORE mutating so the inner tblPr of the tblPrChange is the
    // complete previous state (§17.13.5.34).
    let snapshot = snapshot_table_formatting(table, revision);

    // Apply only the requested properties. The serializer emits each
    // `table.formatting.*` whenever `Some`. Every other tblPr property — and
    // every row and cell — is left byte-identical.
    if let Some(borders) = &patch.borders {
        table.formatting.borders = Some(borders.clone());
        // The edit AUTHORS this slot: claim provenance (see CellFormatting).
        table.formatting.has_direct_borders = true;
    }
    if let Some(width) = &patch.width {
        table.formatting.width = Some(width.clone());
    }
    if let Some(margins) = &patch.default_cell_margins {
        table.formatting.default_cell_margins = Some(margins.clone());
        // The edit AUTHORS this slot: claim provenance (see borders above) —
        // an unclaimed tblCellMar is stripped as style materialization.
        table.formatting.has_direct_cell_margins = true;
    }

    match mode {
        // Author a tracked change: record the previous tblPr; the serializer
        // emits w:tblPrChange, accept/reject resolves it.
        MaterializationMode::TrackedChange => {
            table.formatting_change = Some(snapshot);
            // WORD RULE (bisected against real Word):
            // Word NEVER registers a lone `w:tblPrChange` — the revision is
            // invisible in the review pane and reject-all silently KEEPS the
            // new formatting. Any row/cell-level change carrier makes Word
            // register the coalesced table_property revision and honor
            // reject. Word's own writer emits no-op trPrChange snapshots for
            // exactly this reason; mirror it with ONE no-op trPrChange
            // (previous = the row's current trPr) on the first row that
            // carries no row change of its own, stamped with its
            // own annotation id (Word's writer gives each carrier a unique
            // id; the validator enforces it via I-ANN-001). If every row
            // already carries a trPrChange, the table already registers in
            // Word and no companion is needed.
            if let Some(row) = table
                .rows
                .iter_mut()
                .find(|r| r.formatting_change.is_none() && r.tracking_status.is_none())
            {
                let companion_rev = next_revision(revision, rev_counter);
                row.formatting_change = Some(snapshot_row_formatting(row, &companion_rev));
            }
        }
        // Direct mutation: keep the new tblPr, no tracked change.
        MaterializationMode::Direct => {
            table.formatting_change = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::TableFormattingPatch;
    use crate::domain::{TableMeasurement, WidthType};

    #[test]
    fn empty_patch_is_empty() {
        assert!(TableFormattingPatch::default().is_empty());
    }

    #[test]
    fn patch_with_width_is_not_empty() {
        let patch = TableFormattingPatch {
            width: Some(TableMeasurement {
                w: 5000,
                width_type: WidthType::Pct,
                pct_literal: None,
            }),
            ..Default::default()
        };
        assert!(!patch.is_empty());
    }
}
