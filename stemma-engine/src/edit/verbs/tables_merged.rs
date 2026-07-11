//! `tables-merged` — widen the existing `ReplaceTable` path so a `replace(table)`
//! can author **merged cells** (`w:gridSpan`, `w:vMerge`) and **header rows**
//! (`w:tblHeader`), and so row/column insert-delete on a merged grid materialize
//! as tracked changes (§17.4.17 gridSpan, §17.4.84 vMerge, §17.4.49 tblHeader,
//! §17.13.5 cellIns/cellDel/trPr-ins/del).
//!
//! This is an authoring-side **lift**, not a build. The IR already models
//! merge/header (`TableCellNode.grid_span`,
//! `.v_merge`, `TableRowNode.is_header`), the serializer already emits all of it,
//! and the materializer (`tracked_model::apply_table_structure_changed`) already
//! adopts the target's `grid_span`/`v_merge` on matched cells and emits row/cell
//! tracked changes. The ONLY thing missing was the authoring INPUT: the v4
//! grammar's `TableCellAttrs`/`TableRowAttrs` were empty stubs and
//! `resolve_table_spec` hard-coded `grid_span:1` / `v_merge:None` /
//! `is_header:false`, so `validate_base_table_v4_compatible` had to fail loud
//! rather than mangle. This verb fills the spec and narrows that guard.
//!
//! Because `ReplaceTable` already carries everything (block_id, semantic_hash,
//! rationale, replacement) there is **no new `EditStep`**; the lift is to the
//! `TableBlockSpec` payload shape and to the two validators. The body that is
//! unique to this verb — the merge-grid validity checks that keep the refusals
//! airtight — lives here.
//!
//! v1 scope (fail loud beyond it, per CLAUDE.md "start narrow"):
//! - merge state is `gridSpan` (horizontal) and `vMerge` restart/continue
//!   (vertical) plus the `tblHeader` row flag;
//! - column insert/delete is the existing positional cell insert/delete inside a
//!   matched row — NOT a first-class column op. A spec whose per-row column
//!   *count* (sum of gridSpans) is not uniform across rows is a **ragged grid**
//!   and is refused (`RaggedTableGrid`): the positional matched-row cell loop in
//!   `apply_table_structure_changed` cannot align such a grid unambiguously, and
//!   we refuse rather than guess column identity;
//! - a `vMerge=continue` cell with no `vMerge=restart` anchor above it in the
//!   same logical column is an **orphan continue** and is refused
//!   (`OrphanVMergeContinue`): `canonicalize_table` rejects it downstream, but we
//!   catch it at authoring time so the error names the row/cell, not an opaque
//!   canonicalization failure.

use super::super::{EditError, TableBlockSpec, VerticalMergeSpec};

/// Validate a `TableBlockSpec` whose cells/rows now carry merge/header state, so
/// the merge-aware `replace(table)` refusals are airtight BEFORE the spec is
/// resolved and handed to the diff/materializer.
///
/// Two invariants (both are real OOXML constraints, not arbitrary policy):
///
/// 1. **Rectangular logical grid (no ragged rows).** Every row's *logical*
///    column count — the sum of each cell's `gridSpan` (default 1) — must be
///    equal across all rows. OOXML tables are rectangular: `w:tblGrid` defines
///    N columns and every row occupies exactly N grid units (via gridSpan,
///    gridBefore/gridAfter). The positional matched-row cell alignment in
///    `apply_table_structure_changed` relies on this; a ragged spec would make
///    column identity ambiguous, so v1 refuses with `RaggedTableGrid`.
///
/// 2. **Every `vMerge=continue` has a restart anchor above it in its column.**
///    A continuation cell merges upward into the nearest `vMerge=restart` in the
///    *same logical column*. We walk the grid column-by-column (accounting for
///    gridSpan width) and refuse a `continue` that has no open restart above
///    (`OrphanVMergeContinue`). `canonicalize_table` is the downstream backstop;
///    failing here yields a row/cell-addressed, actionable error instead.
pub(crate) fn validate_merge_spec(
    spec: &TableBlockSpec,
    step_index: usize,
) -> Result<(), EditError> {
    // Empty-structure refusals (no rows / no cells) are owned by
    // `resolve_table_spec`; this validator assumes at least the shape exists and
    // focuses on merge-grid validity. We tolerate the empty case (it falls
    // through to resolve_table_spec's own error).
    if spec.rows.is_empty() {
        return Ok(());
    }

    // ── Invariant 1: rectangular logical grid ──────────────────────────────
    // The logical width of a row is the sum of its cells' gridSpans.
    let logical_width = |cells: &[super::super::TableCellSpec]| -> u32 {
        cells.iter().map(|c| c.merge_h.unwrap_or(1).max(1)).sum()
    };
    let first_width = logical_width(&spec.rows[0].cells);
    for (row_index, row) in spec.rows.iter().enumerate() {
        let w = logical_width(&row.cells);
        if w != first_width {
            return Err(EditError::RaggedTableGrid {
                row_index,
                expected_columns: first_width,
                actual_columns: w,
                step_index,
            });
        }
    }

    // ── Invariant 2: no orphan vMerge=continue ─────────────────────────────
    // Walk top to bottom. For each logical column track whether a vertical
    // merge is currently "open" (a restart, or a continue extending one). A
    // continue in a column with no open merge above it is an orphan.
    //
    // We map each cell to the logical column range it occupies (its starting
    // column .. + gridSpan). A `restart` opens a merge on its starting column;
    // a `continue` requires its starting column to already be open.
    // Invariant 1 guarantees a rectangular grid, so every column is written by
    // every row and `column_open` carries forward each column's merge state.
    let mut column_open: Vec<bool> = vec![false; first_width as usize];
    for (row_index, row) in spec.rows.iter().enumerate() {
        let mut col_cursor: usize = 0;
        for (cell_index, cell) in row.cells.iter().enumerate() {
            let span = cell.merge_h.unwrap_or(1).max(1) as usize;
            match cell.merge_v {
                Some(VerticalMergeSpec::Restart) => {
                    // Opens (or re-opens) the merge on this cell's columns.
                    column_open[col_cursor..col_cursor + span].fill(true);
                }
                Some(VerticalMergeSpec::Continue) => {
                    // Requires an open merge above in the start column.
                    if !column_open[col_cursor] {
                        return Err(EditError::OrphanVMergeContinue {
                            row_index,
                            cell_index,
                            column: col_cursor as u32,
                            step_index,
                        });
                    }
                }
                None => {
                    // A non-merged cell closes any open merge in its columns:
                    // the vertical merge does not extend through it.
                    column_open[col_cursor..col_cursor + span].fill(false);
                }
            }
            col_cursor += span;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::super::{TableBlockSpec, TableCellSpec, TableRowSpec, VerticalMergeSpec};
    use super::*;

    fn cell(merge_h: Option<u32>, merge_v: Option<VerticalMergeSpec>) -> TableCellSpec {
        TableCellSpec {
            // content emptiness is checked by resolve_table_spec, not here; a
            // non-empty placeholder keeps the spec structurally honest.
            content: vec![],
            merge_h,
            merge_v,
            formatting: None,
        }
    }

    fn row(cells: Vec<TableCellSpec>) -> TableRowSpec {
        TableRowSpec {
            cells,
            is_header: false,
            height: None,
            height_rule: None,
        }
    }

    #[test]
    fn uniform_grid_with_gridspan_passes() {
        // Row 0: one cell spanning 2 cols. Row 1: two 1-col cells. Both width 2.
        let spec = TableBlockSpec {
            formatting: None,
            rows: vec![
                row(vec![cell(Some(2), None)]),
                row(vec![cell(None, None), cell(None, None)]),
            ],
        };
        assert!(validate_merge_spec(&spec, 0).is_ok());
    }

    #[test]
    fn ragged_grid_is_refused() {
        // Row 0 width 2 (gridSpan 2), row 1 width 1 — ragged.
        let spec = TableBlockSpec {
            formatting: None,
            rows: vec![row(vec![cell(Some(2), None)]), row(vec![cell(None, None)])],
        };
        let err = validate_merge_spec(&spec, 3).unwrap_err();
        match err {
            EditError::RaggedTableGrid {
                row_index,
                expected_columns,
                actual_columns,
                step_index,
            } => {
                assert_eq!(row_index, 1);
                assert_eq!(expected_columns, 2);
                assert_eq!(actual_columns, 1);
                assert_eq!(step_index, 3);
            }
            other => panic!("expected RaggedTableGrid, got {other:?}"),
        }
    }

    #[test]
    fn vmerge_restart_then_continue_passes() {
        let spec = TableBlockSpec {
            formatting: None,
            rows: vec![
                row(vec![
                    cell(None, Some(VerticalMergeSpec::Restart)),
                    cell(None, None),
                ]),
                row(vec![
                    cell(None, Some(VerticalMergeSpec::Continue)),
                    cell(None, None),
                ]),
            ],
        };
        assert!(validate_merge_spec(&spec, 0).is_ok());
    }

    #[test]
    fn orphan_vmerge_continue_is_refused() {
        // First row's first cell is a continue with no restart above it.
        let spec = TableBlockSpec {
            formatting: None,
            rows: vec![
                row(vec![
                    cell(None, Some(VerticalMergeSpec::Continue)),
                    cell(None, None),
                ]),
                row(vec![cell(None, None), cell(None, None)]),
            ],
        };
        let err = validate_merge_spec(&spec, 7).unwrap_err();
        match err {
            EditError::OrphanVMergeContinue {
                row_index,
                cell_index,
                column,
                step_index,
            } => {
                assert_eq!(row_index, 0);
                assert_eq!(cell_index, 0);
                assert_eq!(column, 0);
                assert_eq!(step_index, 7);
            }
            other => panic!("expected OrphanVMergeContinue, got {other:?}"),
        }
    }

    #[test]
    fn non_merged_cell_closes_the_column() {
        // restart, then a plain cell (closes), then continue -> orphan.
        let spec = TableBlockSpec {
            formatting: None,
            rows: vec![
                row(vec![cell(None, Some(VerticalMergeSpec::Restart))]),
                row(vec![cell(None, None)]),
                row(vec![cell(None, Some(VerticalMergeSpec::Continue))]),
            ],
        };
        let err = validate_merge_spec(&spec, 0).unwrap_err();
        assert!(
            matches!(err, EditError::OrphanVMergeContinue { row_index: 2, .. }),
            "a non-merged cell must close the column so a later continue is orphaned: {err:?}"
        );
    }
}
