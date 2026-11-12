//! `table_ops` — granular structural edits on an EXISTING table that lower
//! through the SAME table-diff machinery `ReplaceTable` uses.
//!
//! Today `ReplaceTable` swaps a whole table; a contract-review agent that wants
//! to "add a row", "delete a column", "merge these cells", or "fix the text in
//! one cell" had to re-author the entire table. These verbs let it name the
//! table by `block_id`, describe the structural mutation, and have the engine:
//!
//!   1. locate the base table (`find_block_index`; `NotATable` otherwise);
//!   2. for STRUCTURAL ops (insert/delete row/col, merge): fail loud only when
//!      the base carries an UNRESOLVED tracked change
//!      (`validate_table_not_mid_redline`). Formatting (borders/shading/widths/
//!      row-height/style) is NO LONGER refused: the target is a clone of the
//!      base, so every modeled property round-trips byte-identically (RFC-0003
//!      lifted the earlier blanket-refusal ceiling);
//!   3. build a MODIFIED target `TableNode` from a clone of the base (so every
//!      untouched cell keeps its id and content, and the diff sees it as
//!      Unchanged);
//!   4. lower (base, target) through `lower_table_target` — the shared diff +
//!      `apply_table_structure_changed` tail. The materializer (Invariant M) is
//!      NOT modified: these verbs build its input and call it.
//!
//! `SetCellText` is the exception: editing ONE cell's text does NOT touch the
//! table's / row's / cell's formatting, so it does NOT route through the
//! whole-table replace schema and the v4-formatting refusal does NOT apply to
//! it. `apply` dispatches it to `apply_set_cell_text_in_place`, which locates
//! the cell by its logical grid `{row, col}` (the address `read_block.cells`
//! exposes) and replaces ONLY that cell's paragraph text through the SAME
//! paragraph-text materializer `ReplaceParagraphText` uses — producing an inline
//! tracked `w:ins`/`w:del` inside the cell while `tblPr`, `trPr`, every `tcPr`,
//! and every other cell are byte-preserved.
//!
//! Tracked by default: row insert/delete → `w:trPr/w:ins`/`w:trPr/w:del`,
//! cell insert/delete → `w:cellIns`/`w:cellDel`, in-cell text → inline
//! `w:ins`/`w:del` (OOXML §17.13.5). `MaterializationMode::Direct` overwrites
//! the table outright.
//!
//! ## Fail loud (no silent fallbacks)
//! - table not found / not a table → `BlockNotFound` / `NotATable`;
//! - row/column index out of range → `TableRowIndexOutOfRange` /
//!   `TableColumnIndexOutOfRange`;
//! - a ragged base grid (rows of differing logical width) → `RaggedTableGrid`;
//! - inserting a column into a merged grid (any `gridSpan>1` / `vMerge`), or a
//!   tracked column change on one → `TableColumnOpOnMergedGrid`. A direct
//!   column delete is defined there: a cell spanning the column shrinks by one
//!   (`delete_grid_column`);
//! - a merge region that is not a clean rectangle → `MergeRegionNotRectangular`.
//!
//! ## Formatting on the base (RFC-0003): borders / shading / cell-widths /
//! row-height / table-style are all preserved. Because `build_target` clones
//! the base `TableNode` and mutates the clone, and `apply_table_structure_changed`
//! carries `tblPr` + every matched/deleted row's `trPr`/`tcPr` through
//! unchanged, a structural edit on a fully-formatted wild table round-trips its
//! formatting. Newly INSERTED rows inherit reference height/header status and
//! cell-box formatting. New columns keep their separate cell-box policy.
//! Both inherit structural shape (gridSpan, cnfStyle, hideMark), paragraph
//! formatting and leading text formatting. New text does
//! not inherit source run identities or pending formatting revisions.

use super::super::{
    EditError, MaterializationMode, build_text_node_from_exemplar, find_block_index,
    first_text_formatting, lower_table_target, validate_table_not_mid_redline,
};
use crate::domain::{
    BlockNode, CanonDoc, InlineNode, NodeId, ParagraphNode, RevisionInfo, TableCellNode, TableNode,
    TableRowNode, VerticalMerge, normal_segment,
};
#[cfg(test)]
use crate::domain::{RunRprAuthored, TextNode, TrackedSegment};
use crate::import::compute_table_structure_hash;

/// Where to insert a new row/column relative to a reference index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableInsertPosition {
    Before,
    After,
}

/// The granular structural table operation to author. Each variant names a
/// table by `block_id`; the dispatch resolves it and routes through the shared
/// table-diff lowering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TableOp {
    /// Insert a new row before/after `ref_row`, copying the column structure
    /// of the reference row (cell count + each cell's gridSpan). `cells`
    /// gives each new cell's plain text, left-to-right: fewer entries than
    /// columns leaves the rest empty; MORE entries than the reference row's
    /// column count is refused (`TableInsertRowCellCountExceedsColumns`) —
    /// never clamped. `None`/empty is the old all-blank insert.
    InsertRow {
        ref_row: usize,
        position: TableInsertPosition,
        cells: Option<Vec<String>>,
        /// The row whose formatting the new row copies; `ref_row` when None.
        /// Lets a row added at the end take the formatting of an earlier one.
        format_from_row: Option<usize>,
    },
    /// Delete the row at `row_index`.
    DeleteRow { row_index: usize },
    /// Relocate the complete row relative to another row. Both indices refer
    /// to the table before this operation; no content is reconstructed.
    MoveRow {
        row_index: usize,
        ref_row: usize,
        position: TableInsertPosition,
    },
    /// One target permutation of every source row, preserving complete rows.
    ReorderRows { row_order: Vec<usize> },
    /// Relocate one column, before or after another, by grid column index
    /// (the read view's cell `col`). Every cell moves with its content and
    /// properties and its grid width with it; the table's width and layout are
    /// untouched. On a merged grid every cell must lie wholly in the moving
    /// column, wholly in the columns it moves across, wholly outside both, or
    /// span both. Direct only: a tracked move is refused.
    MoveColumn {
        col_index: usize,
        ref_col: usize,
        position: TableInsertPosition,
        /// The text the caller expects in the moving column's top cell; a
        /// mismatch refuses the op (see [`check_column_header`]).
        expect_header: Option<String>,
    },
    /// Exchange the contents of two cells, each keeping its own cell
    /// properties (width, borders, shading): "put the 1st Respondent heading on
    /// the left and the Applicant heading on the right". Cells are addressed
    /// like `SetCellText` (row, grid column). Direct only, like `MoveColumn`.
    SwapCells {
        row_index: usize,
        col_index: usize,
        with_row: usize,
        with_col: usize,
    },
    /// Insert a new (empty) column before/after `ref_col` (a simple-grid op).
    InsertColumn {
        ref_col: usize,
        position: TableInsertPosition,
    },
    /// Delete the column at `col_index` (a simple-grid op). `expect_header`
    /// guards the address like `MoveColumn`'s.
    DeleteColumn {
        col_index: usize,
        expect_header: Option<String>,
    },
    /// Merge the rectangular cell region [start_row..=end_row] ×
    /// [start_col..=end_col] into a single logical cell (gridSpan horizontally,
    /// vMerge vertically). Simple-grid op.
    MergeCells {
        start_row: usize,
        start_col: usize,
        end_row: usize,
        end_col: usize,
    },
    /// Replace the text of the cell at (row, col) with `text` (single run).
    SetCellText {
        row_index: usize,
        col_index: usize,
        text: String,
    },
}

/// Refuse a column op whose `expected` header is not the text of grid column
/// `col_index`'s top cell (the cell in row 0 covering that column, so a
/// horizontally merged header heads every column it spans). Whitespace runs
/// compare as one space. An agent that counts columns from the first data
/// column, or forgets a label column, names the header it means and is told
/// the index it should have used.
fn check_column_header(
    table: &TableNode,
    block_id: &NodeId,
    col_index: usize,
    expected: &str,
    step_index: usize,
) -> Result<(), EditError> {
    let canonical = crate::table::canonicalize_table(table).map_err(|reason| {
        EditError::UnsupportedTableColumnGeometry {
            block_id: block_id.clone(),
            reason: format!("the column headers cannot be read to check expect_header: {reason}"),
            step_index,
        }
    })?;
    let normalized = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let headers: Vec<String> = (0..canonical.n_cols)
        .map(|col| {
            canonical
                .cell_at(0, col)
                .map_or_else(String::new, |cell| normalized(&cell.text))
        })
        .collect();
    if headers.get(col_index).map(String::as_str) == Some(normalized(expected).as_str()) {
        return Ok(());
    }
    Err(EditError::TableColumnHeaderMismatch {
        block_id: block_id.clone(),
        col_index,
        expected: expected.to_string(),
        headers,
        step_index,
    })
}

/// Dispatch entry: locate the table, validate, build the target, and lower it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply(
    doc: &mut CanonDoc,
    block_id: &NodeId,
    semantic_hash: Option<&str>,
    op: &TableOp,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    transaction_floor: u32,
    mode: MaterializationMode,
    step_index: usize,
) -> Result<(), EditError> {
    let idx = find_block_index(&doc.blocks, block_id).ok_or_else(|| EditError::BlockNotFound {
        block_id: block_id.clone(),
        step_index,
    })?;

    let base_table = match &doc.blocks[idx].block {
        BlockNode::Table(t) => t.clone(),
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
    };

    // The semantic-hash guard is checked for EVERY op (including SetCellText)
    // before any mutation, so a stale base fails the same way regardless of op.
    if let Some(expected) = semantic_hash
        && let Err(actual) =
            crate::semantic_hash::check_block_guard(&doc.blocks[idx].block, expected)
    {
        return Err(EditError::BlockSemanticHashMismatch {
            block_id: block_id.clone(),
            expected: expected.to_string(),
            actual,
            step_index,
        });
    }

    // A column op naming the header it expects is refused, before anything
    // else, when that column is headed otherwise: the caller counted wrong.
    if let TableOp::DeleteColumn {
        col_index,
        expect_header: Some(expected),
    }
    | TableOp::MoveColumn {
        col_index,
        expect_header: Some(expected),
        ..
    } = op
    {
        check_column_header(&base_table, block_id, *col_index, expected, step_index)?;
    }

    // SetCellText edits ONE cell's text in place — it does NOT touch the table's
    // / row's / cell's formatting, so the whole-table v4 formatting refusal is
    // irrelevant to it. Route it straight to the paragraph-text materializer
    // (the SAME one ReplaceParagraphText uses), preserving tblPr/trPr/tcPr and
    // every other cell byte-for-byte. The structural ops below (insert/delete
    // row/col, merge, whole-table replace) keep the v4-formatting guard because
    // they DO rewrite the table through the replace schema, which can't carry
    // borders/shading/widths.
    if let TableOp::SetCellText {
        row_index,
        col_index,
        text,
    } = op
    {
        return super::super::apply_set_cell_text_in_place(
            doc,
            idx,
            block_id,
            *row_index,
            *col_index,
            text,
            revision,
            rev_counter,
            transaction_floor,
            mode,
            step_index,
        );
    }

    // These structural ops build their target by CLONING the base and mutating
    // the clone (`build_target`), so every table/row/cell property the IR models
    // is carried through — the old blanket formatting refusal no longer
    // applies. Only refuse a base carrying an UNRESOLVED tracked change, which
    // the structural diff can't layer a fresh revision over.
    // Relocation requires a clean source row set, including within a batch:
    // relocating an earlier pending insertion/deletion would change its scope.
    // OOXML records no column move: a tracked one would be every cell deleted
    // and re-inserted (w:cellDel / w:cellIns), which merged grids cannot carry
    // reliably. Refuse it, naming the direct route.
    if matches!(op, TableOp::MoveColumn { .. } | TableOp::SwapCells { .. })
        && mode == MaterializationMode::TrackedChange
    {
        return Err(EditError::TrackedColumnMoveUnsupported {
            block_id: block_id.clone(),
            step_index,
        });
    }
    let allowed_transaction_floor = if matches!(
        op,
        TableOp::MoveRow { .. }
            | TableOp::ReorderRows { .. }
            | TableOp::MoveColumn { .. }
            | TableOp::SwapCells { .. }
    ) {
        None
    } else {
        Some(transaction_floor)
    };
    validate_table_not_mid_redline(&base_table, step_index, allowed_transaction_floor)?;

    // A TRACKED column insert/delete over an EXPLICIT tblGrid is materialized
    // directly (not through the generic table diff): the diff's positional
    // cell-pairing misaligns a mid-position column insert, and the merged grid
    // must splice/remove the matching `gridCol` width. `apply_tracked_column_op`
    // marks the exact column's cells Inserted/Deleted and keeps `grid_cols`
    // physically consistent. Deletion partitions the adjacent survivor slot
    // and records complete prior geometry; resolution settles native width
    // absorption. Empty-grid
    // (unformatted) tables keep the legacy `build_target` path unchanged.
    if mode == MaterializationMode::TrackedChange
        && matches!(
            op,
            TableOp::InsertColumn { .. } | TableOp::DeleteColumn { .. }
        )
        && !base_table.formatting.grid_cols.is_empty()
    {
        return apply_tracked_column_op(
            doc,
            idx,
            block_id,
            &base_table,
            op,
            revision,
            rev_counter,
            step_index,
        );
    }

    // Build the modified target table from a clone of the base. Untouched cells
    // keep their ids (so the diff aligns them as Unchanged).
    let target = build_target(&base_table, op, block_id, step_index)?;

    if let TableOp::ReorderRows { row_order } = op {
        return super::reorder_rows::apply(
            doc,
            idx,
            &base_table,
            &target,
            row_order,
            revision,
            rev_counter,
            mode,
            step_index,
        );
    }

    // Word's Merge Cells records no revision with Track Changes on: the cells
    // merge and the absorbed text moves into the top-left cell, untracked.
    // OOXML's only tracked merge is `w:cellMerge`, which Word does not write
    // for it, so the merge and its text relocation are one direct change even
    // in a tracked transaction. Tracking only the relocation would let Reject
    // put text back into a continuation cell, which Word does not display.
    let mode = if matches!(op, TableOp::MergeCells { .. }) {
        MaterializationMode::Direct
    } else {
        mode
    };
    lower_table_target(
        doc,
        idx,
        block_id,
        &base_table,
        &target,
        revision,
        rev_counter,
        mode,
        "edit_table_op",
        step_index,
    )
}

/// Build the mutated target `TableNode` for one op. The base is cloned; the op
/// mutates the clone; the structure hash is recomputed.
fn build_target(
    base: &TableNode,
    op: &TableOp,
    block_id: &NodeId,
    step_index: usize,
) -> Result<TableNode, EditError> {
    let mut t = base.clone();
    match op {
        TableOp::InsertRow {
            ref_row,
            position,
            cells,
            format_from_row,
        } => {
            check_row_index(&t, *ref_row, block_id, step_index)?;
            let template_row = format_from_row.unwrap_or(*ref_row);
            check_row_index(&t, template_row, block_id, step_index)?;
            let template = &t.rows[template_row];
            if let Some(given) = cells
                && given.len() > template.cells.len()
            {
                return Err(EditError::TableInsertRowCellCountExceedsColumns {
                    block_id: block_id.clone(),
                    given: given.len(),
                    columns: template.cells.len(),
                    step_index,
                });
            }
            let mut new_row = fresh_row_like(template, &t.id, cells.as_deref(), step_index)?;
            let at = match position {
                TableInsertPosition::Before => *ref_row,
                TableInsertPosition::After => *ref_row + 1,
            };
            new_row.para_id = Some(stable_authored_row_para_id(&t, at, cells.as_deref()));
            t.rows.insert(at, new_row);
        }
        TableOp::MoveRow {
            row_index,
            ref_row,
            position,
        } => {
            check_row_index(&t, *row_index, block_id, step_index)?;
            check_row_index(&t, *ref_row, block_id, step_index)?;
            super::move_row::reorder(&mut t, *row_index, *ref_row, *position, step_index)?;
        }
        TableOp::ReorderRows { row_order } => {
            super::reorder_rows::reorder(&mut t, row_order, step_index)?;
        }
        TableOp::MoveColumn {
            col_index,
            ref_col,
            position,
            ..
        } => {
            move_column(
                &mut t, *col_index, *ref_col, *position, block_id, step_index,
            )?;
        }
        TableOp::SwapCells {
            row_index,
            col_index,
            with_row,
            with_col,
        } => {
            let a = cell_at(&t, *row_index, *col_index, block_id, step_index)?;
            let b = cell_at(&t, *with_row, *with_col, block_id, step_index)?;
            if a == b {
                return Err(EditError::UnsupportedTableColumnGeometry {
                    block_id: block_id.clone(),
                    reason: format!(
                        "cell ({row_index}, {col_index}) cannot be swapped with itself"
                    ),
                    step_index,
                });
            }
            let first = std::mem::take(&mut t.rows[a.0].cells[a.1].blocks);
            let second = std::mem::replace(&mut t.rows[b.0].cells[b.1].blocks, first);
            t.rows[a.0].cells[a.1].blocks = second;
            let first = std::mem::take(&mut t.rows[a.0].cells[a.1].content_sdt_wraps);
            let second = std::mem::replace(&mut t.rows[b.0].cells[b.1].content_sdt_wraps, first);
            t.rows[a.0].cells[a.1].content_sdt_wraps = second;
        }
        TableOp::DeleteRow { row_index } => {
            check_row_index(&t, *row_index, block_id, step_index)?;
            if t.rows.len() == 1 {
                return Err(EditError::TableWouldBeEmpty {
                    block_id: block_id.clone(),
                    step_index,
                });
            }
            let prior_rows: Vec<_> = t.rows.iter().map(|row| row.id.clone()).collect();
            t.rows.remove(*row_index);
            crate::tracked_model::reanchor_table_ranges_after_row_removal(&mut t, &prior_rows);
        }
        TableOp::InsertColumn { ref_col, position } => {
            let width = simple_grid_width(&t, block_id, step_index)?;
            check_col_index(width, *ref_col, block_id, step_index)?;
            let at = match position {
                TableInsertPosition::Before => *ref_col,
                TableInsertPosition::After => *ref_col + 1,
            };
            for row in &mut t.rows {
                let template = &row.cells[(*ref_col).min(row.cells.len() - 1)];
                let new_cell = fresh_cell_like(template, &row.id, at, "", step_index)?;
                row.cells.insert(at, new_cell);
            }
            // Keep tblGrid in lock-step with the logical column count: on a
            // formatted table `grid_cols` has one entry per column, so an
            // out-of-sync length corrupts the grid. The new column inherits the
            // reference column's width (a column "like this one"). Empty
            // `grid_cols` (unformatted table) stays empty.
            if !t.formatting.grid_cols.is_empty() {
                let src = (*ref_col).min(t.formatting.grid_cols.len() - 1);
                let w = t.formatting.grid_cols[src];
                let at = at.min(t.formatting.grid_cols.len());
                t.formatting.grid_cols.insert(at, w);
            }
        }
        TableOp::DeleteColumn { col_index, .. } => {
            delete_grid_column(&mut t, *col_index, block_id, step_index)?;
        }
        TableOp::MergeCells {
            start_row,
            start_col,
            end_row,
            end_col,
        } => {
            merge_cells(
                &mut t, *start_row, *start_col, *end_row, *end_col, block_id, step_index,
            )?;
        }
        TableOp::SetCellText { .. } => {
            // SetCellText never reaches the whole-table replace path: `apply`
            // dispatches it to `apply_set_cell_text_in_place` (an in-place
            // cell-paragraph-text edit that preserves tblPr/trPr/tcPr) before
            // `build_target` is ever called. Reaching here is a routing bug.
            unreachable!("SetCellText is handled in-place by apply, not build_target");
        }
    }
    t.structure_hash = compute_table_structure_hash(&t.rows);
    Ok(t)
}

/// A newly authored row needs a carrier identity that survives save/reopen.
/// `w14:paraId` is Word's persisted row identity; deriving it from the current
/// table state and requested insertion makes replay deterministic and avoids
/// using a shifting row index as revision identity evidence.
fn stable_authored_row_para_id(
    table: &TableNode,
    insertion_index: usize,
    cells: Option<&[String]>,
) -> String {
    let mut hash = 0x811c9dc5u32;
    let mut feed = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u32::from(*byte);
            hash = hash.wrapping_mul(0x01000193);
        }
    };
    feed(table.id.0.as_bytes());
    feed(&table.rows.len().to_be_bytes());
    feed(&insertion_index.to_be_bytes());
    if let Some(cells) = cells {
        for cell in cells {
            feed(&cell.len().to_be_bytes());
            feed(cell.as_bytes());
        }
    }
    let occupied: std::collections::HashSet<&str> = table
        .rows
        .iter()
        .filter_map(|row| row.para_id.as_deref())
        .collect();
    hash &= 0x7fff_ffff;
    loop {
        let candidate = format!("{hash:08X}");
        if !occupied.contains(candidate.as_str()) {
            return candidate;
        }
        hash = hash.wrapping_add(1) & 0x7fff_ffff;
    }
}

/// Materialize a TRACKED column insert/delete directly on a simple-grid table
/// (RFC-0003). Unlike the whole-table diff path, this marks the EXACT column's
/// cells (no positional-pairing misalignment on a mid-position insert) and keeps
/// `grid_cols` physically consistent with the pre-resolution column count. The
/// accept/reject projection (`uniformly_removed_columns`) then drops the right
/// `gridCol` on resolution, so per-column widths stay in lock-step through BOTH
/// accept and reject.
///
/// - **Insert** at logical `at`: every row gets a fresh `w:cellIns`-tracked cell
///   at that column; `grid_cols` gets the reference column's width spliced in at
///   `at` (physical count N+1). Reject removes the inserted cells AND drops that
///   `gridCol` (→ N); accept keeps both (→ N+1).
/// - **Delete** at `col`: every row's cell there is marked `w:cellDel` with its
///   content tracked-deleted; `grid_cols` is unchanged (the deleted cell is still
///   physically present, count N). Accept removes the cells AND drops that
///   `gridCol` (→ N-1); reject keeps both (→ N).
///
/// Column ops require a SIMPLE grid (uniform cell count, no `gridSpan`/`vMerge`)
/// — a merged grid is refused (`TableColumnOpOnMergedGrid`), same as the legacy
/// path.
#[allow(clippy::too_many_arguments)]
fn apply_tracked_column_op(
    doc: &mut CanonDoc,
    idx: usize,
    block_id: &NodeId,
    base: &TableNode,
    op: &TableOp,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    step_index: usize,
) -> Result<(), EditError> {
    let width = simple_grid_width(base, block_id, step_index)?;
    let first_annotation = *rev_counter;
    let mut merged = base.clone();
    match op {
        TableOp::InsertColumn { ref_col, position } => {
            check_col_index(width, *ref_col, block_id, step_index)?;
            let at = match position {
                TableInsertPosition::Before => *ref_col,
                TableInsertPosition::After => *ref_col + 1,
            };
            for row in &mut merged.rows {
                let template = &row.cells[(*ref_col).min(row.cells.len() - 1)];
                let mut new_cell = fresh_cell_like(template, &row.id, at, "", step_index)?;
                new_cell.tracking_status = Some(crate::domain::TrackingStatus::Inserted(
                    crate::tracked_model::next_revision(revision, rev_counter),
                ));
                row.cells.insert(at, new_cell);
            }
            if !merged.formatting.grid_cols.is_empty() {
                let src = (*ref_col).min(merged.formatting.grid_cols.len() - 1);
                let w = merged.formatting.grid_cols[src];
                let at = at.min(merged.formatting.grid_cols.len());
                merged.formatting.grid_cols.insert(at, w);
            }
        }
        TableOp::DeleteColumn { col_index, .. } => {
            check_col_index(width, *col_index, block_id, step_index)?;
            if width == 1 {
                return Err(EditError::TableWouldBeEmpty {
                    block_id: block_id.clone(),
                    step_index,
                });
            }
            // Word absorbs an accepted cellDel's physical slot into its
            // neighboring survivor. Partition that survivor's intended width
            // across the two pending slots, rather than donating the deleted
            // column's original width. Previous geometry is complete history,
            // so Reject restores the source instead of guessing it.
            let refuse = |reason: String| EditError::UnsupportedTableColumnGeometry {
                block_id: block_id.clone(),
                reason,
                step_index,
            };
            if merged.formatting.grid_cols.len() != width
                || merged
                    .rows
                    .iter()
                    .any(|row| row.grid_before != 0 || row.grid_after != 0)
            {
                return Err(refuse(
                    "physical cells must match the complete explicit grid without row offsets"
                        .to_owned(),
                ));
            }
            let neighbor = if *col_index == 0 { 1 } else { *col_index - 1 };
            let survivor_width = merged.formatting.grid_cols[neighbor];
            // Native fixed-layout cells clamp below 360 twips. Two physical
            // carriers cannot represent a narrower unchanged survivor.
            if survivor_width < 720 {
                return Err(refuse(format!(
                    "survivor column {neighbor} is {survivor_width} twips; a reversible two-slot partition requires at least 720"
                )));
            }
            let target_width = merged
                .formatting
                .grid_cols
                .iter()
                .enumerate()
                .filter(|(index, _)| *index != *col_index)
                .try_fold(0u32, |total, (_, width)| total.checked_add(*width))
                .ok_or_else(|| refuse("target table width overflows twips".to_owned()))?;
            let table_revision = crate::tracked_model::next_revision(revision, rev_counter);
            merged.formatting_change = Some(super::super::snapshot_table_formatting(
                &merged,
                &table_revision,
            ));
            merged.formatting.grid_history = Some(crate::domain::TableGridHistory {
                columns: merged.formatting.grid_cols.clone(),
            });
            merged.formatting.grid_cols[*col_index] = survivor_width / 2;
            merged.formatting.grid_cols[neighbor] = survivor_width - survivor_width / 2;
            // `tblPrChange` cannot carry `tblGrid`: its previous projection
            // holds the current grid, as import reads it back, and the prior
            // grid lives only in `grid_history` (which Reject restores).
            if let Some(change) = merged.formatting_change.as_mut() {
                change.previous.grid_cols = merged.formatting.grid_cols.clone();
            }
            merged.formatting.width = Some(crate::domain::TableMeasurement {
                w: target_width,
                width_type: crate::domain::WidthType::Dxa,
                pct_literal: None,
            });
            merged.formatting.layout = Some(crate::domain::TableLayout::Fixed);
            for row in &mut merged.rows {
                // Preferred widths must describe the complete current frame.
                // Stale preferred widths let native autofit rebalance it on
                // Reject. Record history only for widths that actually change.
                for column in 0..width {
                    let measurement = crate::domain::TableMeasurement {
                        w: merged.formatting.grid_cols[column],
                        width_type: crate::domain::WidthType::Dxa,
                        pct_literal: None,
                    };
                    if row.cells[column].formatting.width.as_ref() == Some(&measurement) {
                        continue;
                    }
                    let cell_revision = crate::tracked_model::next_revision(revision, rev_counter);
                    let cell = &mut row.cells[column];
                    cell.formatting_change =
                        Some(super::super::snapshot_cell_formatting(cell, &cell_revision));
                    cell.formatting.width = Some(measurement);
                }
                let cell = &mut row.cells[*col_index];
                cell.tracking_status = Some(crate::domain::TrackingStatus::Deleted(
                    crate::tracked_model::next_revision(revision, rev_counter),
                ));
                crate::tracked_model::mark_cell_content_deleted(cell, revision, rev_counter);
            }
        }
        _ => unreachable!("apply_tracked_column_op only handles Insert/DeleteColumn"),
    }
    merged.structure_hash = compute_table_structure_hash(&merged.rows);
    doc.blocks[idx].block = BlockNode::from(merged);
    if matches!(op, TableOp::DeleteColumn { .. }) {
        super::super::group_authored_annotations(doc, first_annotation..*rev_counter, revision)?;
    }
    Ok(())
}

/// Move grid column `col_index` before or after `ref_col`. The moving column
/// (A) and the columns it crosses (B) swap places in every row and in the grid.
/// A cell must lie wholly in A, wholly in B, wholly outside both, or span both
/// (a full-width row stays as it is); a cell that straddles a boundary of A or B
/// has no place in the moved table and is refused.
fn move_column(
    t: &mut TableNode,
    col_index: usize,
    ref_col: usize,
    position: TableInsertPosition,
    block_id: &NodeId,
    step_index: usize,
) -> Result<(), EditError> {
    let refuse = |reason: String| EditError::UnsupportedTableColumnGeometry {
        block_id: block_id.clone(),
        reason,
        step_index,
    };
    if t.rows
        .iter()
        .any(|row| row.grid_before != 0 || row.grid_after != 0)
    {
        return Err(refuse(
            "rows with grid offsets (w:gridBefore / w:gridAfter)".into(),
        ));
    }
    let Some(first_row) = t.rows.first() else {
        return Err(EditError::TableRowIndexOutOfRange {
            block_id: block_id.clone(),
            row_index: 0,
            row_count: 0,
            step_index,
        });
    };
    let width = row_logical_width(first_row) as usize;
    if let Some((row_index, row)) = t
        .rows
        .iter()
        .enumerate()
        .find(|(_, row)| row_logical_width(row) as usize != width)
    {
        return Err(EditError::RaggedTableGrid {
            row_index,
            expected_columns: width as u32,
            actual_columns: row_logical_width(row),
            step_index,
        });
    }
    check_col_index(width, col_index, block_id, step_index)?;
    check_col_index(width, ref_col, block_id, step_index)?;
    let boundary = ref_col + usize::from(position == TableInsertPosition::After);
    if boundary == col_index || boundary == col_index + 1 {
        return Ok(());
    }
    // The moving column and the block it crosses, as half-open source grid
    // ranges; `first` is the one whose cells end up first in the region.
    let moving = (col_index, col_index + 1);
    let crossed = if boundary < col_index {
        (boundary, col_index)
    } else {
        (col_index + 1, boundary)
    };
    let (first, second) = if boundary < col_index {
        (moving, crossed)
    } else {
        (crossed, moving)
    };
    let (low, high) = (first.0.min(second.0), first.1.max(second.1));
    for (row_index, row) in t.rows.iter_mut().enumerate() {
        let mut start = 0;
        let mut before = Vec::new();
        let mut front = Vec::new();
        let mut back = Vec::new();
        let mut after = Vec::new();
        for cell in std::mem::take(&mut row.cells) {
            let end = start + cell.grid_span.max(1) as usize;
            let within = |range: (usize, usize)| start >= range.0 && end <= range.1;
            if end <= low {
                before.push(cell);
            } else if start >= high {
                after.push(cell);
            } else if start <= low && end >= high {
                // Spans both blocks: the row keeps this cell where it is.
                before.push(cell);
            } else if within(first) {
                front.push(cell);
            } else if within(second) {
                back.push(cell);
            } else {
                return Err(refuse(format!(
                    "row {row_index} has a cell over grid columns {start}..{end}, across the \
                     boundary of the moving column {col_index} or of the columns it moves over"
                )));
            }
            start = end;
        }
        row.cells = before
            .into_iter()
            .chain(front)
            .chain(back)
            .chain(after)
            .collect();
    }
    if t.formatting.grid_cols.len() == width {
        let columns = &mut t.formatting.grid_cols;
        let moved: Vec<u32> = columns[first.0..first.1]
            .iter()
            .chain(&columns[second.0..second.1])
            .copied()
            .collect();
        columns.splice(low..high, moved);
    }
    Ok(())
}

/// The (row, physical cell) holding grid column `col_index` of row
/// `row_index`, as the read view addresses cells: the cell must start at that
/// column. A vertical-merge continuation holds no content of its own.
fn cell_at(
    t: &TableNode,
    row_index: usize,
    col_index: usize,
    block_id: &NodeId,
    step_index: usize,
) -> Result<(usize, usize), EditError> {
    check_row_index(t, row_index, block_id, step_index)?;
    let row = &t.rows[row_index];
    let mut col = row.grid_before as usize;
    for (index, cell) in row.cells.iter().enumerate() {
        if col == col_index {
            if cell.v_merge == VerticalMerge::Continue {
                return Err(EditError::UnsupportedTableColumnGeometry {
                    block_id: block_id.clone(),
                    reason: format!(
                        "cell ({row_index}, {col_index}) continues a vertical merge; its \
                         content belongs to the cell above where the merge starts"
                    ),
                    step_index,
                });
            }
            return Ok((row_index, index));
        }
        col += cell.grid_span.max(1) as usize;
    }
    Err(EditError::TableColumnIndexOutOfRange {
        block_id: block_id.clone(),
        col_index,
        column_count: col + row.grid_after as usize,
        step_index,
    })
}

/// Logical width of a row = sum of its cells' gridSpans.
/// Delete grid column `col` (a logical column, counted across spans). In each
/// row the cell covering it goes, or, when that cell spans further columns (a
/// title row across the table), its span and width shrink by the column, as
/// Word's Delete Column does. Vertical merges are per column, so a column's
/// cells leave together. Rows of differing width, or rows with gridBefore /
/// gridAfter, give the column no single meaning and are refused.
fn delete_grid_column(
    t: &mut TableNode,
    col: usize,
    block_id: &NodeId,
    step_index: usize,
) -> Result<(), EditError> {
    let Some(first) = t.rows.first() else {
        return Err(EditError::TableWouldBeEmpty {
            block_id: block_id.clone(),
            step_index,
        });
    };
    let width = row_logical_width(first);
    for (row_index, row) in t.rows.iter().enumerate() {
        if row_logical_width(row) != width {
            return Err(EditError::RaggedTableGrid {
                row_index,
                expected_columns: width,
                actual_columns: row_logical_width(row),
                step_index,
            });
        }
        if row.grid_before != 0 || row.grid_after != 0 {
            return Err(EditError::TableColumnOpOnMergedGrid {
                block_id: block_id.clone(),
                step_index,
            });
        }
    }
    check_col_index(width as usize, col, block_id, step_index)?;
    if width == 1 {
        return Err(EditError::TableWouldBeEmpty {
            block_id: block_id.clone(),
            step_index,
        });
    }
    let column_width = t.formatting.grid_cols.get(col).copied();
    for row in &mut t.rows {
        let mut start = 0usize;
        let mut covering = None;
        for (index, cell) in row.cells.iter().enumerate() {
            let span = cell.grid_span.max(1) as usize;
            if (start..start + span).contains(&col) {
                covering = Some(index);
                break;
            }
            start += span;
        }
        let index = covering.expect("an even-width row covers every grid column");
        let cell = &mut row.cells[index];
        if cell.grid_span > 1 {
            cell.grid_span -= 1;
            if let (Some(measure), Some(removed)) = (cell.formatting.width.as_mut(), column_width)
                && measure.width_type == crate::domain::WidthType::Dxa
            {
                measure.w = measure.w.saturating_sub(removed);
            }
        } else {
            row.cells.remove(index);
        }
    }
    // Drop the matching tblGrid entry so `grid_cols` length stays equal to the
    // column count (see InsertColumn).
    if col < t.formatting.grid_cols.len() {
        t.formatting.grid_cols.remove(col);
    }
    Ok(())
}

fn row_logical_width(row: &TableRowNode) -> u32 {
    row.cells.iter().map(|c| c.grid_span.max(1)).sum()
}

/// Validate that the table is a SIMPLE grid (every row has the same cell count,
/// no gridSpan>1, no vMerge) and return that uniform column count. Column ops
/// require this: a merged grid makes column identity ambiguous, so we refuse
/// rather than guess (same reasoning as the `tables-merged` ragged refusal).
fn simple_grid_width(
    t: &TableNode,
    block_id: &NodeId,
    step_index: usize,
) -> Result<usize, EditError> {
    if t.rows.is_empty() {
        return Err(EditError::TableRowIndexOutOfRange {
            block_id: block_id.clone(),
            row_index: 0,
            row_count: 0,
            step_index,
        });
    }
    let first = t.rows[0].cells.len();
    for (row_index, row) in t.rows.iter().enumerate() {
        // Ragged: differing logical width across rows.
        if row_logical_width(row) != row_logical_width(&t.rows[0]) {
            return Err(EditError::RaggedTableGrid {
                row_index,
                expected_columns: row_logical_width(&t.rows[0]),
                actual_columns: row_logical_width(row),
                step_index,
            });
        }
        // Merged: any spanning cell makes positional column ops ambiguous.
        if row.cells.len() != first
            || row
                .cells
                .iter()
                .any(|c| c.grid_span > 1 || c.v_merge != VerticalMerge::None)
        {
            return Err(EditError::TableColumnOpOnMergedGrid {
                block_id: block_id.clone(),
                step_index,
            });
        }
    }
    Ok(first)
}

fn check_row_index(
    t: &TableNode,
    row_index: usize,
    block_id: &NodeId,
    step_index: usize,
) -> Result<(), EditError> {
    if row_index >= t.rows.len() {
        return Err(EditError::TableRowIndexOutOfRange {
            block_id: block_id.clone(),
            row_index,
            row_count: t.rows.len(),
            step_index,
        });
    }
    Ok(())
}

fn check_col_index(
    width: usize,
    col_index: usize,
    block_id: &NodeId,
    step_index: usize,
) -> Result<(), EditError> {
    if col_index >= width {
        return Err(EditError::TableColumnIndexOutOfRange {
            block_id: block_id.clone(),
            col_index,
            column_count: width,
            step_index,
        });
    }
    Ok(())
}

/// Merge a rectangular cell region into one logical cell, as Word's Merge Cells
/// does (Word oracle, 2026-11-01). Horizontal extent → gridSpan on the leftmost
/// cell of each row in the region (the others are removed); vertical extent →
/// vMerge restart on the top row, continue below. The merged cell holds the
/// paragraphs of every non-empty cell of the region in reading order (an empty
/// cell contributes nothing); each continuation cell keeps one empty paragraph.
///
/// A region of one column works on any table whose rows in the region hold one
/// cell per grid column, extending or joining existing vertical merges, as long
/// as it does not cut one: its top cell must not continue a merge from above,
/// and the cell below it must not continue into it. A region of several columns
/// needs a table with no merged cells.
fn merge_cells(
    t: &mut TableNode,
    start_row: usize,
    start_col: usize,
    end_row: usize,
    end_col: usize,
    block_id: &NodeId,
    step_index: usize,
) -> Result<(), EditError> {
    if start_row > end_row || start_col > end_col {
        return Err(EditError::MergeRegionNotRectangular {
            block_id: block_id.clone(),
            reason: "start index exceeds end index".to_string(),
            step_index,
        });
    }
    if start_row == end_row && start_col == end_col {
        return Err(EditError::MergeRegionNotRectangular {
            block_id: block_id.clone(),
            reason: "a single cell is not a merge region".to_string(),
            step_index,
        });
    }
    if end_row >= t.rows.len() {
        return Err(EditError::TableRowIndexOutOfRange {
            block_id: block_id.clone(),
            row_index: end_row,
            row_count: t.rows.len(),
            step_index,
        });
    }
    let unsupported = |reason: String| EditError::MergeCellsUnsupported {
        block_id: block_id.clone(),
        reason,
        step_index,
    };
    if start_col == end_col {
        for row_index in start_row..=end_row {
            let row = &t.rows[row_index];
            if row.grid_before > 0
                || row.grid_after > 0
                || row.cells.iter().any(|cell| cell.grid_span > 1)
            {
                return Err(unsupported(format!(
                    "row {row_index} has cells spanning several columns, so column \
                     {start_col} does not name one cell in every row of the region"
                )));
            }
            if start_col >= row.cells.len() {
                return Err(EditError::TableColumnIndexOutOfRange {
                    block_id: block_id.clone(),
                    col_index: start_col,
                    column_count: row.cells.len(),
                    step_index,
                });
            }
        }
        if t.rows[start_row].cells[start_col].v_merge == VerticalMerge::Continue {
            return Err(unsupported(format!(
                "cell ({start_row}, {start_col}) continues a merge from the row above; \
                 start the region at the top of that merge"
            )));
        }
        if let Some(below) = t.rows.get(end_row + 1)
            && below.cells.get(start_col).map(|cell| &cell.v_merge)
                == Some(&VerticalMerge::Continue)
        {
            return Err(unsupported(format!(
                "cell ({}, {start_col}) continues the merge the region ends in; end the \
                 region at the bottom of that merge",
                end_row + 1
            )));
        }
    } else if let Err(EditError::TableColumnOpOnMergedGrid { .. }) =
        simple_grid_width(t, block_id, step_index)
    {
        return Err(unsupported(
            "a merge across several columns needs a table with no merged cells; a \
             merge down one column works on this table"
                .to_string(),
        ));
    }
    let width = if start_col == end_col {
        t.rows[start_row].cells.len()
    } else {
        simple_grid_width(t, block_id, step_index)?
    };
    if end_col >= width {
        return Err(EditError::TableColumnIndexOutOfRange {
            block_id: block_id.clone(),
            col_index: end_col,
            column_count: width,
            step_index,
        });
    }

    // The merged cell's content: every non-empty cell's paragraphs, in reading
    // order. Moving blocks out of a cell would orphan a content control that
    // wraps them, so such a cell is refused rather than unwrapped.
    let mut moved = Vec::new();
    for row_index in start_row..=end_row {
        for col in start_col..=end_col {
            let cell = &t.rows[row_index].cells[col];
            if cell_is_empty(cell) {
                continue;
            }
            if !cell.content_sdt_wraps.is_empty() {
                return Err(unsupported(format!(
                    "cell ({row_index}, {col}) holds a content control; merging would \
                     move its paragraphs out of it"
                )));
            }
            moved.extend(cell.blocks.iter().cloned());
        }
    }

    let span = (end_col - start_col + 1) as u32;
    for (row_offset, row_index) in (start_row..=end_row).enumerate() {
        let row = &mut t.rows[row_index];
        // Horizontal merge: leftmost cell of the region absorbs the span; the
        // cells start_col+1..=end_col are removed (their content moved above).
        let absorbed: Vec<TableCellNode> = row.cells.drain(start_col..=end_col).collect();
        let mut anchor = absorbed.into_iter().next().expect("region has >=1 cell");
        if span > 1 {
            anchor.grid_span = span;
        }
        if row_offset == 0 {
            if !moved.is_empty() {
                anchor.blocks = std::mem::take(&mut moved);
            }
        } else if !cell_is_empty(&anchor) {
            // Word leaves a run-less paragraph (`<w:p/>` with the cell's
            // paragraph properties), which import reads with no segments.
            let mut empty = template_paragraph(&anchor, &anchor.id, 0, "", step_index)?;
            empty.segments = Vec::new();
            anchor.blocks = vec![BlockNode::from(empty)];
        }
        // Vertical merge: top row restarts, the rest continue.
        if end_row > start_row {
            anchor.v_merge = if row_offset == 0 {
                VerticalMerge::Restart
            } else {
                VerticalMerge::Continue
            };
        }
        row.cells.insert(start_col, anchor);
    }
    Ok(())
}

/// A cell holding only paragraphs with no content: no text, no picture, no
/// marker. Word's Merge Cells moves nothing out of such a cell.
fn cell_is_empty(cell: &TableCellNode) -> bool {
    cell.blocks.iter().all(|block| match block {
        BlockNode::Paragraph(paragraph) => paragraph
            .all_inlines()
            .all(|inline| matches!(inline, InlineNode::Text(text) if text.text.is_empty())),
        _ => false,
    })
}

/// Build a fresh row structurally like `template`: same number of cells and
/// gridSpans, fresh ids, Normal tracking. Each cell's paragraph carries the
/// corresponding entry of `cell_texts` (left-to-right); a cell past the end
/// of `cell_texts` (or when `cell_texts` is `None`) gets empty text. The
/// caller has already validated `cell_texts.len() <= template.cells.len()`.
fn fresh_row_like(
    template: &TableRowNode,
    table_id: &NodeId,
    cell_texts: Option<&[String]>,
    step_index: usize,
) -> Result<TableRowNode, EditError> {
    let row_id = NodeId::from(format!("{}_insrow_{}", table_id.0, fresh_suffix()));
    let cells = template
        .cells
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let text = cell_texts
                .and_then(|texts| texts.get(i))
                .map_or("", |s| s.as_str());
            let mut cell = fresh_cell_like(c, &row_id, i, text, step_index)?;
            // A new row borrows each reference cell's presentation, including
            // direct-property provenance. Column construction has a separate
            // width policy and does not pass through this row-only boundary.
            cell.formatting = c.formatting.clone();
            Ok(cell)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(TableRowNode {
        source_row_attrs: Vec::new(),
        id: row_id,
        cells,
        grid_before: template.grid_before,
        grid_after: template.grid_after,
        tracking_status: None,
        is_header: template.is_header,
        height: template.height,
        height_rule: template.height_rule.clone(),
        formatting_change: None,
        para_id: None,
        text_id: None,
        cant_split: template.cant_split,
        jc: template.jc.clone(),
        w_before: template.w_before.clone(),
        w_after: template.w_after.clone(),
        cnf_style: template.cnf_style.clone(),
        tbl_pr_ex: template.tbl_pr_ex.clone(),
        tbl_pr_ex_change: None,
        cell_spacing: template.cell_spacing,
        preserved: Vec::new(),
    })
}

/// Build a fresh cell structurally like `template` (gridSpan kept, vMerge
/// cleared), fresh id, paragraphs cloned from the template's first paragraph
/// (so the cell shape is honest) carrying `text`: one paragraph per line, since
/// a line break in a cell is a new paragraph there, as Enter makes it in Word
/// (a raw line feed inside w:t would read as a space).
fn fresh_cell_like(
    template: &TableCellNode,
    row_id: &NodeId,
    col: usize,
    text: &str,
    step_index: usize,
) -> Result<TableCellNode, EditError> {
    let cell_id = NodeId::from(format!("{}_c{col}_{}", row_id.0, fresh_suffix()));
    let blocks = text
        .split('\n')
        .enumerate()
        .map(|(line, line_text)| {
            let line_text = line_text.strip_suffix('\r').unwrap_or(line_text);
            template_paragraph(template, &cell_id, line, line_text, step_index).map(BlockNode::from)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(TableCellNode {
        cell_merge: None,
        id: cell_id,
        blocks,
        grid_span: template.grid_span.max(1),
        v_merge: VerticalMerge::None,
        formatting: crate::domain::CellFormatting::default(),
        formatting_change: None,
        tracking_status: None,
        row_sdt_wrapper: None,
        content_sdt_wraps: Vec::new(),
        cnf_style: template.cnf_style.clone(),
        hide_mark: template.hide_mark,
        preserved: Vec::new(),
    })
}

/// Build a paragraph carrying `text` using the cell's first paragraph as a
/// formatting template, with a fresh-content Normal segment. Paragraph and
/// leading run formatting travel together; old text and run provenance do not.
fn template_paragraph(
    cell: &TableCellNode,
    cell_id: &NodeId,
    line: usize,
    text: &str,
    step_index: usize,
) -> Result<ParagraphNode, EditError> {
    let para_id = if line == 0 {
        NodeId::from(format!("{}_p", cell_id.0))
    } else {
        NodeId::from(format!("{}_p{line}", cell_id.0))
    };
    // A carrier with no addressable paragraph does not supply a formatting
    // template. Refuse explicitly instead of inventing default formatting.
    let template = cell
        .blocks
        .iter()
        .find_map(|b| match b {
            BlockNode::Paragraph(p) => Some(p),
            _ => None,
        })
        .ok_or_else(|| EditError::UnsupportedParagraphStructure {
            block_id: cell.id.clone(),
            reason: "reference cell has no addressable paragraph to inherit formatting from".into(),
            step_index,
        })?;
    let inline = InlineNode::from(build_text_node_from_exemplar(
        NodeId::from(format!("{}_t", para_id.0)),
        &first_text_formatting(template),
        text.to_string(),
        None,
    ));
    let mut np = template.as_ref().clone();
    // A new cell borrows formatting, never the original paragraph's Word
    // identity/version or section boundary (MS-DOCX 2.6.2.3–2.6.2.4).
    // Use the same template boundary as fresh body paragraphs, before lowering.
    super::super::strip_position_bound_state(&mut np);
    np.id = para_id;
    np.segments = normal_segment(vec![inline]);
    np.block_text_hash = Some(crate::import::sha256_hex(text.as_bytes()));
    np.rendered_text = None;
    np.formatting_change = None;
    np.para_mark_status = None;
    np.para_split = false;
    np.section_property_change = None;
    Ok(np)
}

/// Minimal paragraph for synthetic unit witnesses, never a production fallback.
#[cfg(test)]
fn minimal_paragraph(id: NodeId, segments: Vec<TrackedSegment>, text: &str) -> ParagraphNode {
    ParagraphNode {
        id,
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
        segments,
        block_text_hash: Some(crate::import::sha256_hex(text.as_bytes())),
        numbering: None,
        has_direct_numbering: true,
        numbering_suppressed: false,
        direct_numbering_omits_ilvl: false,
        materialized_numbering: None,
        rendered_text: None,
        literal_prefix: None,
        literal_prefix_marks: Vec::new(),
        literal_prefix_style_props: crate::domain::StyleProps::default(),
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
        paragraph_mark_style_props: crate::domain::StyleProps::default(),
        paragraph_mark_rfonts: crate::domain::AuthoredRFonts::default(),
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
        text_direction: None,
        suppress_auto_hyphens: None,
        snap_to_grid: None,
        overflow_punct: None,
        adjust_right_ind: None,
        word_wrap: None,
        frame_pr: None,
        para_id: None,
        text_id: None,
        cnf_style: None,
        source_paragraph_attrs: Vec::new(),
        preserved_ppr: Vec::new(),
        cell_move_id: None,
    }
}

/// Monotonic-ish suffix for fresh ids within a transaction so inserted rows /
/// cells don't collide with existing ids. Uses a thread-local counter (the
/// engine applies a transaction on one thread) — deterministic within a run.
fn fresh_suffix() -> u64 {
    use std::cell::Cell;
    thread_local! {
        static COUNTER: Cell<u64> = const { Cell::new(0) };
    }
    COUNTER.with(|c| {
        let v = c.get();
        c.set(v + 1);
        v
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn simple_table(id: &str, rows: usize, cols: usize) -> TableNode {
        let mut row_nodes = Vec::new();
        for r in 0..rows {
            let row_id = NodeId::from(format!("{id}_r{r}"));
            let mut cells = Vec::new();
            for c in 0..cols {
                let cell_id = NodeId::from(format!("{id}_r{r}_c{c}"));
                let para = minimal_paragraph(
                    NodeId::from(format!("{}_p", cell_id.0)),
                    normal_segment(vec![InlineNode::from(TextNode {
                        id: NodeId::from(format!("{}_t", cell_id.0)),
                        text_role: None,
                        text: format!("r{r}c{c}"),
                        marks: Vec::new(),
                        style_props: crate::domain::StyleProps::default(),
                        rpr_authored: RunRprAuthored::default(),
                        source_run_attrs: Vec::new(),
                        formatting_change: None,
                    })]),
                    &format!("r{r}c{c}"),
                );
                cells.push(TableCellNode {
                    cell_merge: None,
                    id: cell_id,
                    blocks: vec![BlockNode::from(para)],
                    grid_span: 1,
                    v_merge: VerticalMerge::None,
                    formatting: crate::domain::CellFormatting::default(),
                    formatting_change: None,
                    tracking_status: None,
                    row_sdt_wrapper: None,
                    content_sdt_wraps: Vec::new(),
                    cnf_style: None,
                    hide_mark: false,
                    preserved: Vec::new(),
                });
            }
            row_nodes.push(TableRowNode {
                source_row_attrs: Vec::new(),
                id: row_id,
                cells,
                grid_before: 0,
                grid_after: 0,
                tracking_status: None,
                is_header: false,
                height: None,
                height_rule: None,
                formatting_change: None,
                para_id: None,
                text_id: None,
                cant_split: None,
                jc: None,
                w_before: None,
                w_after: None,
                cnf_style: None,
                tbl_pr_ex: None,
                tbl_pr_ex_change: None,
                cell_spacing: None,
                preserved: Vec::new(),
            });
        }
        let structure_hash = compute_table_structure_hash(&row_nodes);
        TableNode {
            range_markers: Vec::new(),
            id: NodeId::from(id.to_string()),
            rows: row_nodes,
            structure_hash,
            formatting: crate::domain::TableFormatting::default(),
            formatting_change: None,
        }
    }

    #[test]
    fn reordered_target_preserves_complete_rows_on_both_projections() {
        use crate::domain::{
            CompatSettings, DocFingerprint, DocMeta, DocPart, FieldData, FieldKind,
            INTERNAL_IDS_VERSION_V0, OpaqueInlineNode, OpaqueKind, ProofRef, SCHEMA_VERSION_V0,
            StyleProps, normal_tracked_block,
        };

        // A structural reorder changes order, never the complete row payload.
        // Reject must restore the original row order and per-row properties.
        let mut base = simple_table("move_table", 3, 2);
        for (index, row) in base.rows.iter_mut().enumerate() {
            row.height = Some(400 + index as u32 * 37);
            row.height_rule = Some(crate::domain::HeightRule::AtLeast);
            row.cant_split = (index == 2).then_some(true);
            row.source_row_attrs = vec![("w:rsidR".to_string(), format!("0000000{index}"))];
            for cell in &mut row.cells {
                cell.formatting.width = Some(crate::domain::TableMeasurement {
                    w: 1200 + index as u32 * 41,
                    width_type: crate::domain::WidthType::Dxa,
                    pct_literal: None,
                });
                cell.formatting.no_wrap = Some(index == 2);
            }
        }
        let field = InlineNode::from(OpaqueInlineNode {
            formatting_change: None,
            id: NodeId::from("move_field"),
            kind: OpaqueKind::Field(FieldData {
                field_kind: FieldKind::Simple,
                instruction_text: Some(" DATE ".to_string()),
                result_text: Some("2026".to_string()),
                semantic: None,
            }),
            opaque_ref: "move_field".to_string(),
            proof_ref: ProofRef {
                part: DocPart::DocumentXml,
                block_id: NodeId::from("move_table"),
                docx_anchor: "move_field".to_string(),
            },
            wrapper_marks: vec![],
            wrapper_style_props: StyleProps::default(),
            wrapper_rpr_authored: RunRprAuthored::ALL,
            source_run_attrs: vec![],
            joins_following_text_run: false,
            raw_xml: Some(b"<w:fldSimple xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" w:instr=\" DATE \"><w:r><w:t>2026</w:t></w:r></w:fldSimple>".to_vec()),
            content_hash: None,
        });
        base.rows[2].cells[0]
            .blocks
            .push(BlockNode::from(minimal_paragraph(
                NodeId::from("move_field_paragraph"),
                normal_segment(vec![field]),
                "2026",
            )));
        base.rows[2].cells[1]
            .blocks
            .insert(0, BlockNode::from(simple_table("nested", 1, 1)));
        base.structure_hash = compute_table_structure_hash(&base.rows);
        let mut target = base.clone();
        let moved = target.rows.remove(2);
        target.rows.insert(0, moved);
        target.structure_hash = compute_table_structure_hash(&target.rows);
        let mut doc = CanonDoc {
            id: NodeId::from("document"),
            blocks: vec![normal_tracked_block(BlockNode::from(base.clone()))],
            meta: DocMeta {
                schema_version: SCHEMA_VERSION_V0.to_string(),
                docx_fingerprint: DocFingerprint("synthetic".to_string()),
                internal_ids_version: INTERNAL_IDS_VERSION_V0.to_string(),
            },
            headers: vec![],
            footers: vec![],
            footnotes: vec![],
            endnotes: vec![],
            comments: vec![],
            comments_extended: vec![],
            body_section_properties: None,
            body_section_property_change: None,
            compat_settings: CompatSettings::default(),
            even_and_odd_headers: None,
            document_background: None,
            document_protection: None,
        };
        let revision = RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("Reviewer".to_string()),
            date: Some("2026-01-01T00:00:00Z".to_string()),
            apply_op_id: None,
        };
        let mut counter = 2;
        lower_table_target(
            &mut doc,
            0,
            &base.id,
            &base,
            &target,
            &revision,
            &mut counter,
            MaterializationMode::TrackedChange,
            "synthetic_row_reorder",
            0,
        )
        .expect("complete row target is representable");
        let mut accepted = doc.clone();
        crate::tracked_model::accept_all(&mut accepted);
        crate::tracked_model::reject_all_with_styles(&mut doc, None);
        for (actual, expected) in [(&accepted, &target), (&doc, &base)] {
            let BlockNode::Table(table) = &actual.blocks[0].block else {
                panic!("table remains a table");
            };
            assert_eq!(table.rows, expected.rows, "complete row payload and order");
            assert_eq!(table.formatting, expected.formatting);
        }
    }

    #[test]
    fn move_row_positions_noops_and_bounds_are_explicit() {
        let base = simple_table("t", 3, 2);
        for (row_index, ref_row, position) in [
            (1, 1, TableInsertPosition::Before),
            (1, 1, TableInsertPosition::After),
            (1, 0, TableInsertPosition::After),
            (1, 2, TableInsertPosition::Before),
        ] {
            let op = TableOp::MoveRow {
                row_index,
                ref_row,
                position,
            };
            assert_eq!(build_target(&base, &op, &base.id, 0).unwrap(), base);
        }
        for (row_index, ref_row) in [(3, 0), (0, 3)] {
            let op = TableOp::MoveRow {
                row_index,
                ref_row,
                position: TableInsertPosition::Before,
            };
            assert!(matches!(
                build_target(&base, &op, &base.id, 0),
                Err(EditError::TableRowIndexOutOfRange { .. })
            ));
        }
    }

    #[test]
    fn move_row_refuses_vertical_merge_dependencies() {
        let mut base = simple_table("t", 3, 2);
        base.rows[0].cells[0].v_merge = VerticalMerge::Restart;
        base.rows[1].cells[0].v_merge = VerticalMerge::Continue;
        let op = TableOp::MoveRow {
            row_index: 0,
            ref_row: 2,
            position: TableInsertPosition::After,
        };
        assert!(matches!(
            build_target(&base, &op, &base.id, 0),
            Err(EditError::UnsupportedTableRowMove { .. })
        ));
    }

    #[test]
    fn move_row_refuses_table_range_boundaries() {
        use crate::domain::{
            RangeMarkerFamily, RangeMarkerMeta, RangeMarkerRole, TableRangeMarker,
            TableRangePosition,
        };
        let mut base = simple_table("t", 3, 2);
        for (position, role, raw_xml) in [
            (TableRangePosition::BeforeRow(base.rows[0].id.clone()), RangeMarkerRole::Start,
             b"<w:bookmarkStart xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" w:id=\"1\" w:name=\"range\"/>".as_slice()),
            (TableRangePosition::End, RangeMarkerRole::End,
             b"<w:bookmarkEnd xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" w:id=\"1\"/>".as_slice()),
        ] {
            base.range_markers.push(TableRangeMarker { position, marker: RangeMarkerMeta {
                family: RangeMarkerFamily::Bookmark, id: "1".to_string(), role, raw_xml: raw_xml.to_vec(),
            }});
        }
        let op = TableOp::MoveRow {
            row_index: 0,
            ref_row: 2,
            position: TableInsertPosition::After,
        };
        assert!(matches!(
            build_target(&base, &op, &base.id, 0),
            Err(EditError::UnsupportedTableRowMove { .. })
        ));
    }

    #[test]
    fn move_row_retains_horizontal_grid_spans() {
        let mut base = simple_table("t", 3, 2);
        for row in &mut base.rows {
            row.cells[0].grid_span = 2;
        }
        let op = TableOp::MoveRow {
            row_index: 2,
            ref_row: 0,
            position: TableInsertPosition::Before,
        };
        let target = build_target(&base, &op, &base.id, 0).unwrap();
        assert_eq!(
            target.rows,
            vec![
                base.rows[2].clone(),
                base.rows[0].clone(),
                base.rows[1].clone()
            ]
        );
    }

    fn cell_ids(table: &TableNode) -> Vec<Vec<String>> {
        table
            .rows
            .iter()
            .map(|row| row.cells.iter().map(|cell| cell.id.0.to_string()).collect())
            .collect()
    }

    /// Moving a column takes each row's cell there, and its grid width, to the
    /// new place; the other columns keep their order.
    #[test]
    fn move_column_relocates_cells_and_grid_widths() {
        let mut base = simple_table("t", 2, 5);
        base.formatting.grid_cols = vec![1870, 2085, 2340, 1525, 1530];
        let op = TableOp::MoveColumn {
            col_index: 4,
            ref_col: 3,
            position: TableInsertPosition::Before,
            expect_header: None,
        };
        let target = build_target(&base, &op, &base.id, 0).unwrap();
        let ids = cell_ids(&base);
        let expected: Vec<Vec<String>> = ids
            .iter()
            .map(|row| {
                vec![
                    row[0].clone(),
                    row[1].clone(),
                    row[2].clone(),
                    row[4].clone(),
                    row[3].clone(),
                ]
            })
            .collect();
        assert_eq!(cell_ids(&target), expected);
        assert_eq!(
            target.formatting.grid_cols,
            vec![1870, 2085, 2340, 1530, 1525]
        );
        // Moving it back is the identity.
        let back = TableOp::MoveColumn {
            col_index: 3,
            ref_col: 4,
            position: TableInsertPosition::After,
            expect_header: None,
        };
        let restored = build_target(&target, &back, &base.id, 0).unwrap();
        assert_eq!(cell_ids(&restored), ids);
        assert_eq!(restored.formatting.grid_cols, base.formatting.grid_cols);
    }

    /// Two side-by-side blocks of a merged grid swap: rows of a two-column
    /// heading cell and a one-column cell swap them, and a full-width row stays
    /// as it is (a counsel table: Applicant | 1st Respondent, then a notes row).
    #[test]
    fn move_column_swaps_blocks_of_a_merged_grid() {
        let mut base = simple_table("t", 3, 3);
        base.formatting.grid_cols = vec![2000, 3000, 4000];
        for row in &mut base.rows[..2] {
            row.cells.remove(1);
            row.cells[0].grid_span = 2;
        }
        base.rows[2].cells.truncate(1);
        base.rows[2].cells[0].grid_span = 3;
        let op = TableOp::MoveColumn {
            col_index: 2,
            ref_col: 0,
            position: TableInsertPosition::Before,
            expect_header: None,
        };
        let target = build_target(&base, &op, &base.id, 0).unwrap();
        for row in 0..2 {
            assert_eq!(target.rows[row].cells[0].id, base.rows[row].cells[1].id);
            assert_eq!(target.rows[row].cells[1].id, base.rows[row].cells[0].id);
            assert_eq!(target.rows[row].cells[1].grid_span, 2);
        }
        assert_eq!(
            target.rows[2], base.rows[2],
            "the full-width row is untouched"
        );
        assert_eq!(target.formatting.grid_cols, vec![4000, 2000, 3000]);
    }

    /// A cell straddling the moving column's boundary has no place in the moved
    /// table: refused, not split or guessed.
    #[test]
    fn move_column_refuses_a_straddling_cell() {
        let mut base = simple_table("t", 1, 3);
        base.rows[0].cells.remove(2);
        base.rows[0].cells[1].grid_span = 2;
        let op = TableOp::MoveColumn {
            col_index: 1,
            ref_col: 0,
            position: TableInsertPosition::Before,
            expect_header: None,
        };
        let err = build_target(&base, &op, &base.id, 0).unwrap_err();
        assert!(
            matches!(err, EditError::UnsupportedTableColumnGeometry { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn move_column_onto_its_own_place_changes_nothing() {
        let base = simple_table("t", 2, 3);
        for (col_index, ref_col, position) in [
            (1, 1, TableInsertPosition::Before),
            (1, 0, TableInsertPosition::After),
            (1, 2, TableInsertPosition::Before),
        ] {
            let op = TableOp::MoveColumn {
                col_index,
                ref_col,
                position,
                expect_header: None,
            };
            let target = build_target(&base, &op, &base.id, 0).unwrap();
            assert_eq!(cell_ids(&target), cell_ids(&base));
        }
    }

    /// Swapping two cells exchanges their contents; each cell keeps its box
    /// (its width here), so the left column stays the left column's width.
    #[test]
    fn swap_cells_exchanges_contents_and_keeps_cell_boxes() {
        let mut base = simple_table("t", 2, 2);
        base.rows[1].cells[0].formatting.width = Some(crate::domain::TableMeasurement {
            w: 5245,
            width_type: crate::domain::WidthType::Dxa,
            pct_literal: None,
        });
        let op = TableOp::SwapCells {
            row_index: 1,
            col_index: 0,
            with_row: 1,
            with_col: 1,
        };
        let target = build_target(&base, &op, &base.id, 0).unwrap();
        assert_eq!(target.rows[1].cells[0].blocks, base.rows[1].cells[1].blocks);
        assert_eq!(target.rows[1].cells[1].blocks, base.rows[1].cells[0].blocks);
        assert_eq!(
            target.rows[1].cells[0].formatting,
            base.rows[1].cells[0].formatting
        );
        assert_eq!(target.rows[1].cells[0].id, base.rows[1].cells[0].id);
        assert_eq!(target.rows[0], base.rows[0], "other rows are untouched");
    }

    #[test]
    fn swap_cells_refuses_itself_and_a_merge_continuation() {
        let mut base = simple_table("t", 2, 2);
        let same = TableOp::SwapCells {
            row_index: 0,
            col_index: 0,
            with_row: 0,
            with_col: 0,
        };
        assert!(build_target(&base, &same, &base.id, 0).is_err());
        base.rows[0].cells[0].v_merge = VerticalMerge::Restart;
        base.rows[1].cells[0].v_merge = VerticalMerge::Continue;
        let continuation = TableOp::SwapCells {
            row_index: 1,
            col_index: 0,
            with_row: 1,
            with_col: 1,
        };
        let err = build_target(&base, &continuation, &base.id, 0).unwrap_err();
        assert!(
            matches!(err, EditError::UnsupportedTableColumnGeometry { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn insert_row_after_grows_grid() {
        let base = simple_table("t", 2, 3);
        let target = build_target(
            &base,
            &TableOp::InsertRow {
                ref_row: 0,
                position: TableInsertPosition::After,
                cells: None,
                format_from_row: None,
            },
            &base.id,
            0,
        )
        .unwrap();
        assert_eq!(target.rows.len(), 3);
        assert_eq!(target.rows[1].cells.len(), 3);
    }

    #[test]
    fn inserted_row_inherits_reference_header_flag() {
        // Native row insertion preserves a reference header before and after
        // it. Header status is presentation, not source revision identity.
        for position in [TableInsertPosition::Before, TableInsertPosition::After] {
            let mut base = simple_table("header", 2, 1);
            base.rows[0].is_header = true;
            let target = build_target(
                &base,
                &TableOp::InsertRow {
                    ref_row: 0,
                    position,
                    cells: None,
                    format_from_row: None,
                },
                &base.id,
                0,
            )
            .unwrap();
            assert!(target.rows[0].is_header);
            assert!(target.rows[1].is_header);
            assert!(!target.rows[2].is_header);
        }
    }

    #[test]
    fn inserted_row_retains_reference_row_height() {
        // Authoring contract: a reference-row insertion borrows presentation,
        // not source identity or old content. A fixed height is presentation.
        let mut base = simple_table("t", 2, 1);
        base.rows[0].height = Some(480);
        base.rows[0].height_rule = Some(crate::domain::HeightRule::AtLeast);
        let target = build_target(
            &base,
            &TableOp::InsertRow {
                ref_row: 0,
                position: TableInsertPosition::After,
                cells: None,
                format_from_row: None,
            },
            &base.id,
            0,
        )
        .unwrap();
        assert_eq!(target.rows[1].height, Some(480));
        assert_eq!(target.rows[1].height_rule, base.rows[0].height_rule);
    }

    #[test]
    fn inserted_row_retains_reference_cell_direct_formatting() {
        // The new cell has new content, but must not discard the reference
        // cell's direct width/alignment or turn an explicit false into absent.
        let mut base = simple_table("t", 2, 1);
        base.rows[0].cells[0].formatting.width = Some(crate::domain::TableMeasurement {
            w: 2400,
            width_type: crate::domain::WidthType::Dxa,
            pct_literal: None,
        });
        base.rows[0].cells[0].formatting.no_wrap = Some(false);
        let target = build_target(
            &base,
            &TableOp::InsertRow {
                ref_row: 0,
                position: TableInsertPosition::After,
                cells: None,
                format_from_row: None,
            },
            &base.id,
            0,
        )
        .unwrap();
        assert_eq!(
            target.rows[1].cells[0].formatting,
            base.rows[0].cells[0].formatting
        );
    }

    #[test]
    fn inserted_row_does_not_inherit_source_session_provenance() {
        let mut base = simple_table("table", 2, 1);
        base.rows[0].source_row_attrs = vec![("w:rsidR".into(), "00112233".into())];
        base.rows[1].source_row_attrs = vec![("w:rsidTr".into(), "00445566".into())];
        let target = build_target(
            &base,
            &TableOp::InsertRow {
                ref_row: 0,
                position: TableInsertPosition::After,
                cells: None,
                format_from_row: None,
            },
            &base.id,
            0,
        )
        .unwrap();
        assert_eq!(
            target.rows[0].source_row_attrs,
            base.rows[0].source_row_attrs
        );
        assert!(target.rows[1].source_row_attrs.is_empty());
        assert_eq!(
            target.rows[2].source_row_attrs,
            base.rows[1].source_row_attrs
        );
    }

    #[test]
    fn inserted_cell_without_paragraph_template_refuses_invented_formatting() {
        let mut base = simple_table("t", 1, 1);
        base.rows[0].cells[0].blocks.clear();
        let error = build_target(
            &base,
            &TableOp::InsertRow {
                ref_row: 0,
                position: TableInsertPosition::After,
                cells: Some(vec!["New requirement".into()]),
                format_from_row: None,
            },
            &base.id,
            3,
        )
        .unwrap_err();
        assert!(matches!(error, EditError::UnsupportedParagraphStructure {
            block_id, step_index: 3, ..
        } if block_id == base.rows[0].cells[0].id));
    }

    #[test]
    fn inserted_cell_text_inherits_reference_run_formatting_not_identity() {
        let mut base = simple_table("t", 1, 1);
        let BlockNode::Paragraph(p) = &mut base.rows[0].cells[0].blocks[0] else {
            panic!("synthetic cell must contain a paragraph");
        };
        let InlineNode::Text(text) = &mut p.segments[0].inlines[0] else {
            panic!("synthetic paragraph must contain text");
        };
        text.marks = vec![crate::domain::Mark::Italic];
        text.style_props.font_family = Some("Example Serif".into());
        text.style_props.font_size = Some(22);
        text.style_props.lang_bidi = Some("ar-SA".into());
        text.rpr_authored = RunRprAuthored::ALL;
        let expected = text.as_ref().clone();
        let target = build_target(
            &base,
            &TableOp::InsertRow {
                ref_row: 0,
                position: TableInsertPosition::After,
                cells: Some(vec!["New requirement".into()]),
                format_from_row: None,
            },
            &base.id,
            0,
        )
        .unwrap();
        let BlockNode::Paragraph(inserted) = &target.rows[1].cells[0].blocks[0] else {
            panic!("inserted cell must contain a paragraph");
        };
        let actual = inserted.first_content_text_node().unwrap();
        assert_eq!(actual.text, "New requirement");
        assert_eq!(actual.marks, expected.marks);
        assert_eq!(actual.style_props, expected.style_props);
        assert_eq!(actual.rpr_authored, expected.rpr_authored);
        assert_ne!(actual.id, expected.id);
        assert!(actual.source_run_attrs.is_empty());
        assert!(actual.formatting_change.is_none());
        assert_eq!(target.rows[0], base.rows[0]);
    }

    #[test]
    fn insert_row_with_cells_sets_cell_text_and_pads_short_lists() {
        let base = simple_table("t", 2, 3);
        let target = build_target(
            &base,
            &TableOp::InsertRow {
                ref_row: 0,
                position: TableInsertPosition::After,
                cells: Some(vec!["a".to_string(), "b".to_string()]),
                format_from_row: None,
            },
            &base.id,
            0,
        )
        .unwrap();
        let new_row = &target.rows[1];
        assert_eq!(cell_text(&new_row.cells[0]), "a");
        assert_eq!(cell_text(&new_row.cells[1]), "b");
        assert_eq!(
            cell_text(&new_row.cells[2]),
            "",
            "unspecified cell is empty"
        );
    }

    #[test]
    fn insert_row_with_too_many_cells_refused() {
        let base = simple_table("t", 2, 2);
        let err = build_target(
            &base,
            &TableOp::InsertRow {
                ref_row: 0,
                position: TableInsertPosition::After,
                cells: Some(vec!["a".to_string(), "b".to_string(), "c".to_string()]),
                format_from_row: None,
            },
            &base.id,
            0,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            EditError::TableInsertRowCellCountExceedsColumns {
                given: 3,
                columns: 2,
                ..
            }
        ));
    }

    fn cell_text(cell: &TableCellNode) -> String {
        cell.blocks
            .iter()
            .find_map(|b| match b {
                BlockNode::Paragraph(p) => Some(p),
                _ => None,
            })
            .map(|p| {
                p.segments
                    .iter()
                    .flat_map(|s| &s.inlines)
                    .filter_map(|i| match i {
                        InlineNode::Text(t) => Some(t.text.as_str()),
                        _ => None,
                    })
                    .collect::<String>()
            })
            .unwrap_or_default()
    }

    #[test]
    fn delete_only_row_refused() {
        let base = simple_table("t", 1, 2);
        let err =
            build_target(&base, &TableOp::DeleteRow { row_index: 0 }, &base.id, 0).unwrap_err();
        assert!(matches!(err, EditError::TableWouldBeEmpty { .. }));
    }

    #[test]
    fn delete_row_out_of_range_refused() {
        let base = simple_table("t", 2, 2);
        let err =
            build_target(&base, &TableOp::DeleteRow { row_index: 9 }, &base.id, 0).unwrap_err();
        assert!(matches!(err, EditError::TableRowIndexOutOfRange { .. }));
    }

    #[test]
    fn insert_column_grows_every_row() {
        let base = simple_table("t", 2, 2);
        let target = build_target(
            &base,
            &TableOp::InsertColumn {
                ref_col: 1,
                position: TableInsertPosition::After,
            },
            &base.id,
            0,
        )
        .unwrap();
        for row in &target.rows {
            assert_eq!(row.cells.len(), 3);
        }
    }

    #[test]
    fn delete_column_under_a_spanning_cell_shrinks_the_span() {
        let mut base = simple_table("t", 2, 2);
        base.rows[0].cells[0].grid_span = 2;
        base.rows[0].cells.pop(); // a title cell spanning both columns
        let target = build_target(
            &base,
            &TableOp::DeleteColumn {
                col_index: 0,
                expect_header: None,
            },
            &base.id,
            0,
        )
        .unwrap();
        assert_eq!(target.rows[0].cells.len(), 1, "the title cell stays");
        assert_eq!(
            target.rows[0].cells[0].grid_span, 1,
            "spanning one column fewer"
        );
        assert_eq!(target.rows[1].cells.len(), 1, "the column's own cell goes");
        assert_eq!(target.rows[1].cells[0].id, base.rows[1].cells[1].id);
    }

    #[test]
    fn insert_column_on_merged_grid_refused() {
        let mut base = simple_table("t", 2, 2);
        base.rows[0].cells[0].grid_span = 2;
        base.rows[0].cells.pop(); // one cell spanning 2
        let err = build_target(
            &base,
            &TableOp::InsertColumn {
                ref_col: 0,
                position: TableInsertPosition::After,
            },
            &base.id,
            0,
        )
        .unwrap_err();
        assert!(matches!(err, EditError::TableColumnOpOnMergedGrid { .. }));
    }

    #[test]
    fn merge_single_cell_refused() {
        let base = simple_table("t", 2, 2);
        let err = build_target(
            &base,
            &TableOp::MergeCells {
                start_row: 0,
                start_col: 0,
                end_row: 0,
                end_col: 0,
            },
            &base.id,
            0,
        )
        .unwrap_err();
        assert!(matches!(err, EditError::MergeRegionNotRectangular { .. }));
    }

    #[test]
    fn merge_horizontal_sets_gridspan() {
        let base = simple_table("t", 1, 3);
        let target = build_target(
            &base,
            &TableOp::MergeCells {
                start_row: 0,
                start_col: 0,
                end_row: 0,
                end_col: 1,
            },
            &base.id,
            0,
        )
        .unwrap();
        // Row 0 now has 2 cells: a span-2 anchor + the untouched 3rd cell.
        assert_eq!(target.rows[0].cells.len(), 2);
        assert_eq!(target.rows[0].cells[0].grid_span, 2);
    }

    fn cell_texts(cell: &TableCellNode) -> Vec<String> {
        cell.blocks
            .iter()
            .map(|block| match block {
                BlockNode::Paragraph(p) => p
                    .all_inlines()
                    .filter_map(|i| match i {
                        InlineNode::Text(t) => Some(t.text.as_str()),
                        _ => None,
                    })
                    .collect(),
                other => panic!("{other:?}"),
            })
            .collect()
    }

    fn merge(
        base: &TableNode,
        rows: (usize, usize),
        cols: (usize, usize),
    ) -> Result<TableNode, EditError> {
        build_target(
            base,
            &TableOp::MergeCells {
                start_row: rows.0,
                start_col: cols.0,
                end_row: rows.1,
                end_col: cols.1,
            },
            &base.id,
            0,
        )
    }

    fn empty_cell(table: &mut TableNode, row: usize, col: usize) {
        let BlockNode::Paragraph(p) = &mut table.rows[row].cells[col].blocks[0] else {
            panic!()
        };
        p.segments.clear();
    }

    /// Word's Merge Cells down a column moves the lower cell's text into the
    /// top cell as its next paragraph and leaves the continuation cell one
    /// empty paragraph (Word oracle: "7" over "x" merges to "7", "x").
    #[test]
    fn merge_vertical_moves_the_lower_text_into_the_top_cell() {
        let base = simple_table("t", 2, 2);
        let target = merge(&base, (0, 1), (0, 0)).unwrap();
        assert_eq!(target.rows[0].cells[0].v_merge, VerticalMerge::Restart);
        assert_eq!(target.rows[1].cells[0].v_merge, VerticalMerge::Continue);
        assert_eq!(cell_texts(&target.rows[0].cells[0]), ["r0c0", "r1c0"]);
        assert_eq!(cell_texts(&target.rows[1].cells[0]), [""]);
    }

    /// An empty top cell contributes nothing: Word's merge of an empty cell
    /// over "x" holds just "x".
    #[test]
    fn merge_into_an_empty_top_cell_holds_only_the_moved_text() {
        let mut base = simple_table("t", 2, 2);
        empty_cell(&mut base, 0, 0);
        let target = merge(&base, (0, 1), (0, 0)).unwrap();
        assert_eq!(cell_texts(&target.rows[0].cells[0]), ["r1c0"]);
    }

    /// Word's horizontal merge keeps the absorbed cell's text ("a", "b" merge
    /// to one cell holding "a", "b"), not just the first cell's.
    #[test]
    fn merge_horizontal_keeps_the_absorbed_cells_text() {
        let base = simple_table("t", 1, 3);
        let target = merge(&base, (0, 0), (0, 1)).unwrap();
        assert_eq!(cell_texts(&target.rows[0].cells[0]), ["r0c0", "r0c1"]);
    }

    /// A merge down one column extends an existing vertical merge over the
    /// row below it, as Word's Merge Cells does over a merged group and a new
    /// row (restart, continue, continue).
    #[test]
    fn merge_down_a_column_extends_an_existing_merge() {
        let mut base = simple_table("t", 4, 2);
        base.rows[1].cells[0].v_merge = VerticalMerge::Restart;
        base.rows[2].cells[0].v_merge = VerticalMerge::Continue;
        empty_cell(&mut base, 2, 0);
        empty_cell(&mut base, 3, 0);
        let target = merge(&base, (1, 3), (0, 0)).unwrap();
        let merges: Vec<_> = target
            .rows
            .iter()
            .map(|r| r.cells[0].v_merge.clone())
            .collect();
        assert_eq!(
            merges,
            [
                VerticalMerge::None,
                VerticalMerge::Restart,
                VerticalMerge::Continue,
                VerticalMerge::Continue
            ]
        );
        assert_eq!(cell_texts(&target.rows[1].cells[0]), ["r1c0"]);
    }

    #[test]
    fn merge_that_cuts_a_vertical_merge_is_refused() {
        let mut base = simple_table("t", 4, 2);
        base.rows[1].cells[0].v_merge = VerticalMerge::Restart;
        base.rows[2].cells[0].v_merge = VerticalMerge::Continue;
        base.rows[3].cells[0].v_merge = VerticalMerge::Continue;
        for (rows, needle) in [
            ((2, 3), "start the region at the top"),
            ((0, 2), "end the region at the bottom"),
        ] {
            let err = merge(&base, rows, (0, 0)).unwrap_err();
            assert!(
                matches!(&err, EditError::MergeCellsUnsupported { reason, .. } if reason.contains(needle)),
                "{rows:?}: {err}"
            );
        }
    }

    #[test]
    fn merge_across_columns_of_a_merged_table_is_refused_by_name() {
        let mut base = simple_table("t", 3, 2);
        base.rows[1].cells[0].v_merge = VerticalMerge::Restart;
        base.rows[2].cells[0].v_merge = VerticalMerge::Continue;
        let err = merge(&base, (0, 0), (0, 1)).unwrap_err();
        assert!(
            matches!(&err, EditError::MergeCellsUnsupported { .. }),
            "{err:?}"
        );
        assert!(!err.to_string().contains("inserting a column"), "{err}");
    }

    // SetCellText is now an in-place cell-paragraph-text edit (it never reaches
    // `build_target`); its range checks, formatting-preservation, and tracking
    // invariants are covered end-to-end in
    // `stemma-engine/tests/table_set_cell_text.rs`.
}
