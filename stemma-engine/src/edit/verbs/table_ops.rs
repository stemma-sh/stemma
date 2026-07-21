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
//! - column ops on a merged grid (any `gridSpan>1` / `vMerge`) → refused as
//!   `TableColumnOpOnMergedGrid`: column identity is ambiguous when cells span,
//!   so v1 refuses rather than guess (same reasoning as the `tables-merged`
//!   ragged refusal);
//! - a merge region that is not a clean rectangle → `MergeRegionNotRectangular`.
//!
//! ## Formatting on the base (RFC-0003): borders / shading / cell-widths /
//! row-height / table-style are all preserved. Because `build_target` clones
//! the base `TableNode` and mutates the clone, and `apply_table_structure_changed`
//! carries `tblPr` + every matched/deleted row's `trPr`/`tcPr` through
//! unchanged, a structural edit on a fully-formatted wild table round-trips its
//! formatting. Newly INSERTED rows/cells start with default cell formatting (a
//! fresh cell is unformatted); they inherit the reference row's structural
//! shape (gridSpan, cnfStyle, hideMark) via `fresh_row_like`/`fresh_cell_like`.

use super::super::{
    EditError, MaterializationMode, find_block_index, lower_table_target,
    validate_table_not_mid_redline,
};
use crate::domain::{
    BlockNode, CanonDoc, InlineNode, NodeId, ParagraphNode, RevisionInfo, RunRprAuthored,
    TableCellNode, TableNode, TableRowNode, TextNode, TrackedSegment, VerticalMerge,
    normal_segment,
};
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
    },
    /// Delete the row at `row_index`.
    DeleteRow { row_index: usize },
    /// Insert a new (empty) column before/after `ref_col` (a simple-grid op).
    InsertColumn {
        ref_col: usize,
        position: TableInsertPosition,
    },
    /// Delete the column at `col_index` (a simple-grid op).
    DeleteColumn { col_index: usize },
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
    validate_table_not_mid_redline(&base_table, step_index, Some(transaction_floor))?;

    // A TRACKED column insert/delete over an EXPLICIT tblGrid is materialized
    // directly (not through the generic table diff): the diff's positional
    // cell-pairing misaligns a mid-position column insert, and the merged grid
    // must splice/remove the matching `gridCol` width. `apply_tracked_column_op`
    // marks the exact column's cells Inserted/Deleted and keeps `grid_cols`
    // physically consistent; the accept/reject projection then drops the right
    // `gridCol` on resolution (`uniformly_removed_columns`). Empty-grid
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
        } => {
            check_row_index(&t, *ref_row, block_id, step_index)?;
            let template = &t.rows[*ref_row];
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
            let new_row = fresh_row_like(template, &t.id, cells.as_deref());
            let at = match position {
                TableInsertPosition::Before => *ref_row,
                TableInsertPosition::After => *ref_row + 1,
            };
            t.rows.insert(at, new_row);
        }
        TableOp::DeleteRow { row_index } => {
            check_row_index(&t, *row_index, block_id, step_index)?;
            if t.rows.len() == 1 {
                return Err(EditError::TableWouldBeEmpty {
                    block_id: block_id.clone(),
                    step_index,
                });
            }
            t.rows.remove(*row_index);
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
                let new_cell = fresh_cell_like(template, &row.id, at, "");
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
        TableOp::DeleteColumn { col_index } => {
            let width = simple_grid_width(&t, block_id, step_index)?;
            check_col_index(width, *col_index, block_id, step_index)?;
            if width == 1 {
                return Err(EditError::TableWouldBeEmpty {
                    block_id: block_id.clone(),
                    step_index,
                });
            }
            for row in &mut t.rows {
                row.cells.remove(*col_index);
            }
            // Drop the matching tblGrid entry so `grid_cols` length stays equal
            // to the column count (see InsertColumn).
            if *col_index < t.formatting.grid_cols.len() {
                t.formatting.grid_cols.remove(*col_index);
            }
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
                let mut new_cell = fresh_cell_like(template, &row.id, at, "");
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
        TableOp::DeleteColumn { col_index } => {
            check_col_index(width, *col_index, block_id, step_index)?;
            if width == 1 {
                return Err(EditError::TableWouldBeEmpty {
                    block_id: block_id.clone(),
                    step_index,
                });
            }
            for row in &mut merged.rows {
                let cell = &mut row.cells[*col_index];
                cell.tracking_status = Some(crate::domain::TrackingStatus::Deleted(
                    crate::tracked_model::next_revision(revision, rev_counter),
                ));
                crate::tracked_model::mark_cell_content_deleted(cell, revision, rev_counter);
            }
            // grid_cols unchanged: the deleted cell is still physically present
            // until the deletion is accepted (then the projection drops it).
        }
        _ => unreachable!("apply_tracked_column_op only handles Insert/DeleteColumn"),
    }
    merged.structure_hash = compute_table_structure_hash(&merged.rows);
    doc.blocks[idx].block = BlockNode::from(merged);
    Ok(())
}

/// Logical width of a row = sum of its cells' gridSpans.
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

/// Merge a rectangular cell region into one logical cell. Requires a simple
/// grid. Horizontal extent → gridSpan on the leftmost cell of each row in the
/// region; vertical extent → vMerge restart on the top row, continue below.
/// The merged anchor keeps the top-left cell's content; the absorbed cells are
/// removed (horizontal) or marked continue (vertical).
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
    let width = simple_grid_width(t, block_id, step_index)?;
    if end_row >= t.rows.len() {
        return Err(EditError::TableRowIndexOutOfRange {
            block_id: block_id.clone(),
            row_index: end_row,
            row_count: t.rows.len(),
            step_index,
        });
    }
    if end_col >= width {
        return Err(EditError::TableColumnIndexOutOfRange {
            block_id: block_id.clone(),
            col_index: end_col,
            column_count: width,
            step_index,
        });
    }

    let span = (end_col - start_col + 1) as u32;
    for (row_offset, row_index) in (start_row..=end_row).enumerate() {
        let row = &mut t.rows[row_index];
        // Horizontal merge: leftmost cell of the region absorbs the span; the
        // cells start_col+1..=end_col are removed.
        let absorbed: Vec<TableCellNode> = row.cells.drain(start_col..=end_col).collect();
        let mut anchor = absorbed.into_iter().next().expect("region has >=1 cell");
        if span > 1 {
            anchor.grid_span = span;
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

/// Build a fresh row structurally like `template`: same number of cells and
/// gridSpans, fresh ids, Normal tracking. Each cell's paragraph carries the
/// corresponding entry of `cell_texts` (left-to-right); a cell past the end
/// of `cell_texts` (or when `cell_texts` is `None`) gets empty text. The
/// caller has already validated `cell_texts.len() <= template.cells.len()`.
fn fresh_row_like(
    template: &TableRowNode,
    table_id: &NodeId,
    cell_texts: Option<&[String]>,
) -> TableRowNode {
    let row_id = NodeId::from(format!("{}_insrow_{}", table_id.0, fresh_suffix()));
    let cells = template
        .cells
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let text = cell_texts
                .and_then(|texts| texts.get(i))
                .map_or("", |s| s.as_str());
            fresh_cell_like(c, &row_id, i, text)
        })
        .collect();
    TableRowNode {
        id: row_id,
        cells,
        grid_before: template.grid_before,
        grid_after: template.grid_after,
        tracking_status: None,
        is_header: false,
        height: None,
        height_rule: None,
        formatting_change: None,
        para_id: None,
        text_id: None,
        cant_split: template.cant_split,
        jc: template.jc.clone(),
        w_before: template.w_before.clone(),
        w_after: template.w_after.clone(),
        cnf_style: template.cnf_style.clone(),
        tbl_pr_ex: template.tbl_pr_ex.clone(),
        cell_spacing: template.cell_spacing,
        preserved: Vec::new(),
    }
}

/// Build a fresh cell structurally like `template` (gridSpan kept, vMerge
/// cleared), fresh id, one paragraph cloned from the template's first
/// paragraph (so the cell shape is honest) carrying `text`.
fn fresh_cell_like(
    template: &TableCellNode,
    row_id: &NodeId,
    col: usize,
    text: &str,
) -> TableCellNode {
    let cell_id = NodeId::from(format!("{}_c{col}_{}", row_id.0, fresh_suffix()));
    let para = template_paragraph(template, &cell_id, text);
    TableCellNode {
        id: cell_id,
        blocks: vec![BlockNode::from(para)],
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
    }
}

/// Build a paragraph carrying `text` using the cell's first paragraph as a
/// structural template (so its style/alignment survive), with a fresh-content
/// single Normal segment and a deterministic id derived from the cell.
fn template_paragraph(cell: &TableCellNode, cell_id: &NodeId, text: &str) -> ParagraphNode {
    let para_id = NodeId::from(format!("{}_p", cell_id.0));
    // Find a template paragraph in the cell; if none, this cell had no paragraph
    // (degenerate), so we cannot honestly synthesize all 39 ParagraphNode fields
    // — fall back to cloning is impossible. In practice every imported cell has
    // a paragraph; we assert that here by panicking only if truly absent, which
    // canonicalization would already have rejected upstream.
    let template = cell.blocks.iter().find_map(|b| match b {
        BlockNode::Paragraph(p) => Some(p),
        _ => None,
    });
    let inline = InlineNode::from(TextNode {
        id: NodeId::from(format!("{}_t", para_id.0)),
        text_role: None,
        text: text.to_string(),
        marks: Vec::new(),
        style_props: crate::domain::StyleProps::default(),
        rpr_authored: RunRprAuthored::default(),
        source_run_attrs: Vec::new(),
        formatting_change: None,
    });
    let segments: Vec<TrackedSegment> = normal_segment(vec![inline]);

    match template {
        Some(p) => {
            let mut np = p.as_ref().clone();
            np.id = para_id;
            np.segments = segments;
            np.block_text_hash = Some(crate::import::sha256_hex(text.as_bytes()));
            np.rendered_text = None;
            np.formatting_change = None;
            np.para_mark_status = None;
            np.para_split = false;
            np.section_property_change = None;
            np
        }
        None => minimal_paragraph(para_id, segments, text),
    }
}

/// A minimal paragraph carrying `segments`. Only used when a cell has no
/// paragraph template at all (canonicalization rejects such cells upstream, so
/// this is a defensive last resort, not a silent fallback for normal input).
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
        preserved_ppr: Vec::new(),
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
                cant_split: false,
                jc: None,
                w_before: None,
                w_after: None,
                cnf_style: None,
                tbl_pr_ex: None,
                cell_spacing: None,
                preserved: Vec::new(),
            });
        }
        let structure_hash = compute_table_structure_hash(&row_nodes);
        TableNode {
            id: NodeId::from(id.to_string()),
            rows: row_nodes,
            structure_hash,
            formatting: crate::domain::TableFormatting::default(),
            formatting_change: None,
        }
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
            },
            &base.id,
            0,
        )
        .unwrap();
        assert_eq!(target.rows.len(), 3);
        assert_eq!(target.rows[1].cells.len(), 3);
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
    fn column_op_on_merged_grid_refused() {
        let mut base = simple_table("t", 2, 2);
        base.rows[0].cells[0].grid_span = 2;
        base.rows[0].cells.pop(); // one cell spanning 2
        let err =
            build_target(&base, &TableOp::DeleteColumn { col_index: 0 }, &base.id, 0).unwrap_err();
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

    #[test]
    fn merge_vertical_sets_vmerge() {
        let base = simple_table("t", 2, 2);
        let target = build_target(
            &base,
            &TableOp::MergeCells {
                start_row: 0,
                start_col: 0,
                end_row: 1,
                end_col: 0,
            },
            &base.id,
            0,
        )
        .unwrap();
        assert_eq!(target.rows[0].cells[0].v_merge, VerticalMerge::Restart);
        assert_eq!(target.rows[1].cells[0].v_merge, VerticalMerge::Continue);
    }

    // SetCellText is now an in-place cell-paragraph-text edit (it never reaches
    // `build_target`); its range checks, formatting-preservation, and tracking
    // invariants are covered end-to-end in
    // `stemma-engine/tests/table_set_cell_text.rs`.
}
