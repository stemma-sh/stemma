//! Table diffing: compares canonical tables and produces aligned diff results.
//!
//! Uses patience diff on row/column signatures to align rows between tables,
//! then performs cell-level diffing within aligned rows.
//!
//! Row alignment is comparison-specific on purpose. Comparison keeps an
//! aligned row whose content is unrelated as a `Replacement` and shows it as
//! one replaced row; the engine's explicit table edits instead split a
//! low-similarity match into a deleted and an inserted row. Cell text diffing
//! and everything after alignment is the engine's.

use similar::{Algorithm, DiffOp};
use stemma::diff_cell_text;

use crate::domain::{
    BlockNode, CanonicalTable, InlineChange, NestedTableDiff, RunRprAuthored, StyleProps, TableNode,
};
use crate::table::canonicalize_table;
// Shared with the engine: one implementation of the change pipeline.
pub(crate) use stemma::{
    CellDiff, CellDiffType, extract_tracked_table_new_text, extract_tracked_table_old_text,
    table_has_tracked_changes,
};
use stemma::{extract_paragraph_text, find_cell_index};
// Shared with the engine: one implementation of the change pipeline.

/// Result of diffing two canonical tables.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableDiff {
    /// The canonicalized old table.
    pub old_table: CanonicalTable,
    /// The canonicalized new table.
    pub new_table: CanonicalTable,
    /// Row-level alignment between old and new tables.
    pub row_alignment: Vec<RowAlignment>,
    /// Cell-level diffs (for matched rows).
    pub cell_diffs: Vec<CellDiff>,
}

/// Alignment of a single row between old and new tables.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowAlignment {
    /// Row exists in both tables at these indices.
    Matched { old_row: usize, new_row: usize },
    /// Source and target rows share a compatible physical row/cell shell, but
    /// their content has no proved semantic lineage. The materializer may
    /// reuse that shell only while deleting every source-owned cell payload
    /// and inserting every target-owned payload.
    Replacement { old_row: usize, new_row: usize },
    /// Row was deleted from old table.
    Deleted { old_row: usize },
    /// Row was inserted in new table.
    Inserted { new_row: usize },
}

/// Diff two tables and produce a structured TableDiff.
pub fn diff_tables(old_table: &TableNode, new_table: &TableNode) -> Result<TableDiff, String> {
    let old_canonical = canonicalize_table(old_table)?;
    let new_canonical = canonicalize_table(new_table)?;

    Ok(diff_canonical_tables(old_canonical, new_canonical))
}

/// Diff two already-canonicalized tables.
pub fn diff_canonical_tables(old_table: CanonicalTable, new_table: CanonicalTable) -> TableDiff {
    // Align rows using patience diff on row signatures
    let row_alignment = align_rows(&old_table, &new_table);

    // Compute cell-level diffs for matched rows
    let cell_diffs = compute_cell_diffs(&old_table, &new_table, &row_alignment);

    TableDiff {
        old_table,
        new_table,
        row_alignment,
        cell_diffs,
    }
}

/// Align rows between two tables using patience diff on signatures.
pub fn align_rows(old: &CanonicalTable, new: &CanonicalTable) -> Vec<RowAlignment> {
    // Prefer a persisted row identity only when it is unambiguous and appears
    // on both sides. An identity present on just one side must not prevent a
    // normal content match (for example, a caller-authored replacement target
    // that omits imported paraIds). A shared identity, however, prevents a new
    // otherwise-identical blank row from stealing the match of the existing
    // row whose document index shifted.
    let mut old_id_counts = std::collections::HashMap::<&str, usize>::new();
    let mut new_id_counts = std::collections::HashMap::<&str, usize>::new();
    for id in old.row_para_ids.iter().filter_map(Option::as_deref) {
        *old_id_counts.entry(id).or_default() += 1;
    }
    for id in new.row_para_ids.iter().filter_map(Option::as_deref) {
        *new_id_counts.entry(id).or_default() += 1;
    }
    let signature = |table: &CanonicalTable,
                     row: usize,
                     own_counts: &std::collections::HashMap<&str, usize>,
                     other_counts: &std::collections::HashMap<&str, usize>| {
        table.row_para_ids[row]
            .as_deref()
            .filter(|id| own_counts.get(id) == Some(&1) && other_counts.get(id) == Some(&1))
            .map_or_else(
                || table.row_signature(row),
                |id| format!("\0row-identity:{id}"),
            )
    };
    let old_sigs: Vec<String> = (0..old.n_rows)
        .map(|row| signature(old, row, &old_id_counts, &new_id_counts))
        .collect();
    let new_sigs: Vec<String> = (0..new.n_rows)
        .map(|row| signature(new, row, &new_id_counts, &old_id_counts))
        .collect();

    // Use similar's diff with patience algorithm
    let old_refs: Vec<&str> = old_sigs.iter().map(|s| s.as_str()).collect();
    let new_refs: Vec<&str> = new_sigs.iter().map(|s| s.as_str()).collect();

    let diff = similar::capture_diff_slices_deadline(
        Algorithm::Patience,
        &old_refs,
        &new_refs,
        None, // No deadline
    );

    let mut alignments = Vec::new();

    for op in diff {
        match op {
            DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => {
                for i in 0..len {
                    alignments.push(RowAlignment::Matched {
                        old_row: old_index + i,
                        new_row: new_index + i,
                    });
                }
            }
            DiffOp::Delete {
                old_index, old_len, ..
            } => {
                for i in 0..old_len {
                    alignments.push(RowAlignment::Deleted {
                        old_row: old_index + i,
                    });
                }
            }
            DiffOp::Insert {
                new_index, new_len, ..
            } => {
                for i in 0..new_len {
                    alignments.push(RowAlignment::Inserted {
                        new_row: new_index + i,
                    });
                }
            }
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                align_replace_block(
                    old,
                    new,
                    old_index,
                    old_len,
                    new_index,
                    new_len,
                    &mut alignments,
                );
            }
        }
    }

    alignments
}

/// True when two canonical rows expose the same anchor and merge geometry.
/// This says nothing about semantic content identity; it only proves that one
/// physical row/cell shell can carry a complete source deletion plus target
/// insertion without changing the table grid in either terminal.
fn row_grid_shape_matches(
    old: &CanonicalTable,
    new: &CanonicalTable,
    old_row: usize,
    new_row: usize,
) -> bool {
    old.n_cols == new.n_cols
        && (0..old.n_cols).all(|col| {
            let old_anchor = old.is_anchor(old_row, col);
            let new_anchor = new.is_anchor(new_row, col);
            old_anchor == new_anchor
                && (!old_anchor
                    || matches!(
                        (old.cell_at(old_row, col), new.cell_at(new_row, col)),
                        (Some(old_cell), Some(new_cell))
                            if old_cell.rowspan == new_cell.rowspan
                                && old_cell.colspan == new_cell.colspan
                    ))
        })
}

/// Pair only positional rows whose physical grid is compatible. This is not a
/// correspondence heuristic: every paired row is explicitly classified as an
/// unrelated replacement and its content is fully revised on both sides.
fn align_replace_block(
    old: &CanonicalTable,
    new: &CanonicalTable,
    old_index: usize,
    old_len: usize,
    new_index: usize,
    new_len: usize,
    alignments: &mut Vec<RowAlignment>,
) {
    let paired_len = old_len.min(new_len);
    for offset in 0..paired_len {
        let old_row = old_index + offset;
        let new_row = new_index + offset;
        if row_grid_shape_matches(old, new, old_row, new_row) {
            alignments.push(RowAlignment::Replacement { old_row, new_row });
        } else {
            alignments.push(RowAlignment::Deleted { old_row });
            alignments.push(RowAlignment::Inserted { new_row });
        }
    }
    for offset in paired_len..old_len {
        alignments.push(RowAlignment::Deleted {
            old_row: old_index + offset,
        });
    }
    for offset in paired_len..new_len {
        alignments.push(RowAlignment::Inserted {
            new_row: new_index + offset,
        });
    }
}

/// Compute cell-level diffs for aligned rows.
fn compute_cell_diffs(
    old: &CanonicalTable,
    new: &CanonicalTable,
    row_alignment: &[RowAlignment],
) -> Vec<CellDiff> {
    let mut cell_diffs = Vec::new();

    for alignment in row_alignment {
        match alignment {
            RowAlignment::Matched { old_row, new_row } => {
                // Diff cells in matched rows
                let row_diffs = diff_row_cells(old, new, *old_row, *new_row, true);
                cell_diffs.extend(row_diffs);
            }
            RowAlignment::Replacement { old_row, new_row } => {
                let row_diffs = diff_row_cells(old, new, *old_row, *new_row, false);
                cell_diffs.extend(row_diffs);
            }
            RowAlignment::Deleted { old_row } => {
                // All cells in deleted row are deleted
                for col in 0..old.n_cols {
                    if old.is_anchor(*old_row, col)
                        && let Some(cell) = old.cell_at(*old_row, col)
                    {
                        let cell_idx = find_cell_index(old, cell);
                        cell_diffs.push(CellDiff {
                            old_cell_idx: cell_idx,
                            new_cell_idx: None,
                            diff_type: CellDiffType::Deleted,
                            text_diff: None,
                            nested_table_diffs: Vec::new(),
                        });
                    }
                }
            }
            RowAlignment::Inserted { new_row } => {
                // All cells in inserted row are inserted
                for col in 0..new.n_cols {
                    if new.is_anchor(*new_row, col)
                        && let Some(cell) = new.cell_at(*new_row, col)
                    {
                        let cell_idx = find_cell_index(new, cell);
                        cell_diffs.push(CellDiff {
                            old_cell_idx: None,
                            new_cell_idx: cell_idx,
                            diff_type: CellDiffType::Inserted,
                            text_diff: None,
                            nested_table_diffs: Vec::new(),
                        });
                    }
                }
            }
        }
    }

    cell_diffs
}

/// Diff cells within matched rows.
fn diff_row_cells(
    old: &CanonicalTable,
    new: &CanonicalTable,
    old_row: usize,
    new_row: usize,
    content_related: bool,
) -> Vec<CellDiff> {
    let mut diffs = Vec::new();
    let max_cols = old.n_cols.max(new.n_cols);

    for col in 0..max_cols {
        let old_cell = if col < old.n_cols {
            old.cell_at(old_row, col)
                .filter(|_| old.is_anchor(old_row, col))
        } else {
            None
        };

        let new_cell = if col < new.n_cols {
            new.cell_at(new_row, col)
                .filter(|_| new.is_anchor(new_row, col))
        } else {
            None
        };

        match (old_cell, new_cell) {
            (Some(old_c), Some(new_c)) => {
                let old_idx = find_cell_index(old, old_c);
                let new_idx = find_cell_index(new, new_c);

                if !content_related {
                    let para_text_old = extract_paragraph_text(&old_c.blocks);
                    let para_text_new = extract_paragraph_text(&new_c.blocks);
                    diffs.push(CellDiff {
                        old_cell_idx: old_idx,
                        new_cell_idx: new_idx,
                        diff_type: CellDiffType::Modified,
                        text_diff: Some(diff_cell_text_unrelated(&para_text_old, &para_text_new)),
                        nested_table_diffs: Vec::new(),
                    });
                    continue;
                }

                // Check for merge changes
                let merge_changed =
                    old_c.rowspan != new_c.rowspan || old_c.colspan != new_c.colspan;

                // Check for text changes
                let text_changed = old_c.text != new_c.text;

                if text_changed {
                    // Compute paragraph-level text diff (only for paragraph text).
                    let para_text_old = extract_paragraph_text(&old_c.blocks);
                    let para_text_new = extract_paragraph_text(&new_c.blocks);
                    let text_diff = if para_text_old != para_text_new {
                        Some(diff_cell_text(&para_text_old, &para_text_new))
                    } else {
                        None
                    };

                    // Compute nested table diffs.
                    let nested_table_diffs = diff_cell_nested_tables(&old_c.blocks, &new_c.blocks);

                    diffs.push(CellDiff {
                        old_cell_idx: old_idx,
                        new_cell_idx: new_idx,
                        diff_type: CellDiffType::Modified,
                        text_diff,
                        nested_table_diffs,
                    });
                } else if merge_changed {
                    diffs.push(CellDiff {
                        old_cell_idx: old_idx,
                        new_cell_idx: new_idx,
                        diff_type: CellDiffType::MergeChanged,
                        text_diff: None,
                        nested_table_diffs: Vec::new(),
                    });
                } else {
                    diffs.push(CellDiff {
                        old_cell_idx: old_idx,
                        new_cell_idx: new_idx,
                        diff_type: CellDiffType::Unchanged,
                        text_diff: None,
                        nested_table_diffs: Vec::new(),
                    });
                }
            }
            (Some(old_c), None) => {
                let old_idx = find_cell_index(old, old_c);
                diffs.push(CellDiff {
                    old_cell_idx: old_idx,
                    new_cell_idx: None,
                    diff_type: CellDiffType::Deleted,
                    text_diff: None,
                    nested_table_diffs: Vec::new(),
                });
            }
            (None, Some(new_c)) => {
                let new_idx = find_cell_index(new, new_c);
                diffs.push(CellDiff {
                    old_cell_idx: None,
                    new_cell_idx: new_idx,
                    diff_type: CellDiffType::Inserted,
                    text_diff: None,
                    nested_table_diffs: Vec::new(),
                });
            }
            (None, None) => {
                // Both empty, skip
            }
        }
    }

    diffs
}

/// Diff nested tables within a cell's blocks.
///
/// Walks block pairs positionally and produces `NestedTableDiff` entries
/// for any `BlockNode::Table` pairs that differ.
fn diff_cell_nested_tables(
    old_blocks: &[BlockNode],
    new_blocks: &[BlockNode],
) -> Vec<NestedTableDiff> {
    use crate::compiler::diff_nested_tables;
    old_blocks
        .iter()
        .zip(new_blocks.iter())
        .enumerate()
        .filter_map(|(idx, (old_b, new_b))| {
            if let (BlockNode::Table(old_t), BlockNode::Table(new_t)) = (old_b, new_b) {
                match diff_nested_tables(old_t, new_t, idx) {
                    Ok(Some(diff)) => Some(diff),
                    Ok(None) => None,
                    Err(e) => {
                        tracing::warn!("failed to diff nested table at block index {idx}: {e}");
                        None
                    }
                }
            } else {
                None
            }
        })
        .collect()
}

/// Describe an unrelated physical replacement without preserving coincidental
/// common tokens as semantic content lineage.
fn diff_cell_text_unrelated(old_text: &str, new_text: &str) -> Vec<InlineChange> {
    let mut changes = Vec::with_capacity(2);
    if !old_text.is_empty() {
        changes.push(InlineChange::Deleted {
            text: old_text.to_string(),
            marks: Vec::new(),
            style_props: StyleProps::default(),
            rpr_authored: RunRprAuthored::default(),
            formatting_change: None,
            rev_id: 0,
        });
    }
    if !new_text.is_empty() {
        changes.push(InlineChange::Inserted {
            text: new_text.to_string(),
            marks: Vec::new(),
            style_props: StyleProps::default(),
            rpr_authored: RunRprAuthored::default(),
            formatting_change: None,
            rev_id: 0,
        });
    }
    changes
}

// ---------------------------------------------------------------------------
// Tracked-table text extraction
//
// Reads the "before" or "after" text view of a table that carries tracked
// segments and row/cell tracking statuses. Used by the diff pipeline to
// decide whether a tracked table needs a diff entry at all, and to align
// its tracked content with adjacent atoms.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        BlockNode, CellFormatting, InlineNode, NodeId, ParagraphNode, StyleProps, TableCellNode,
        TableFormatting, TableRowNode, TextNode, VerticalMerge, normal_segment,
    };

    fn make_text_cell(id: &str, text: &str) -> TableCellNode {
        TableCellNode {
            cell_merge: None,
            id: NodeId::from(id.to_string()),
            blocks: vec![BlockNode::from(ParagraphNode {
                id: NodeId::from(format!("{}_p", id)),
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
                segments: normal_segment(vec![InlineNode::from(TextNode {
                    id: NodeId::from(format!("{}_t", id)),
                    text_role: None,
                    text: text.to_string(),
                    marks: vec![],
                    style_props: StyleProps::default(),
                    rpr_authored: crate::domain::RunRprAuthored::default(),
                    source_run_attrs: Vec::new(),
                    formatting_change: None,
                })]),
                block_text_hash: None,
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
                paragraph_mark_style_props: StyleProps::default(),
                paragraph_mark_rfonts: Default::default(),
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
                source_paragraph_attrs: Vec::new(),
                preserved_ppr: Vec::new(),
                cell_move_id: None,
            })],
            grid_span: 1,
            v_merge: VerticalMerge::None,
            formatting: CellFormatting::default(),
            formatting_change: None,
            tracking_status: None,
            row_sdt_wrapper: None,
            content_sdt_wraps: Vec::new(),
            cnf_style: None,
            hide_mark: false,
            preserved: Vec::new(),
        }
    }

    fn make_table(rows: Vec<Vec<&str>>) -> TableNode {
        TableNode {
            range_markers: Vec::new(),
            id: NodeId::from("tbl_0"),
            rows: rows
                .into_iter()
                .enumerate()
                .map(|(r, cells)| TableRowNode {
                    source_row_attrs: Vec::new(),
                    id: NodeId::from(format!("tbl_0_r{}", r)),
                    cells: cells
                        .into_iter()
                        .enumerate()
                        .map(|(c, text)| make_text_cell(&format!("c{}_{}", r, c), text))
                        .collect(),
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
                })
                .collect(),
            structure_hash: String::new(),
            formatting: TableFormatting::default(),
            formatting_change: None,
        }
    }

    #[test]
    fn test_identical_tables() {
        let old = make_table(vec![vec!["A", "B"], vec!["C", "D"]]);
        let new = make_table(vec![vec!["A", "B"], vec!["C", "D"]]);

        let diff = diff_tables(&old, &new).expect("diff should succeed");

        // All rows should be matched
        assert_eq!(diff.row_alignment.len(), 2);
        for alignment in &diff.row_alignment {
            assert!(matches!(alignment, RowAlignment::Matched { .. }));
        }

        // All cells should be unchanged
        for cell_diff in &diff.cell_diffs {
            assert_eq!(cell_diff.diff_type, CellDiffType::Unchanged);
        }
    }

    #[test]
    fn test_row_insertion() {
        let old = make_table(vec![vec!["A", "B"], vec!["C", "D"]]);
        let new = make_table(vec![
            vec!["A", "B"],
            vec!["X", "Y"], // Inserted
            vec!["C", "D"],
        ]);

        let diff = diff_tables(&old, &new).expect("diff should succeed");

        // Should have: Matched(0,0), Inserted(1), Matched(1,2)
        let matched_count = diff
            .row_alignment
            .iter()
            .filter(|a| matches!(a, RowAlignment::Matched { .. }))
            .count();
        let inserted_count = diff
            .row_alignment
            .iter()
            .filter(|a| matches!(a, RowAlignment::Inserted { .. }))
            .count();

        assert_eq!(matched_count, 2);
        assert_eq!(inserted_count, 1);
    }

    #[test]
    fn test_row_deletion() {
        let old = make_table(vec![
            vec!["A", "B"],
            vec!["X", "Y"], // Will be deleted
            vec!["C", "D"],
        ]);
        let new = make_table(vec![vec!["A", "B"], vec!["C", "D"]]);

        let diff = diff_tables(&old, &new).expect("diff should succeed");

        // Should have: Matched(0,0), Deleted(1), Matched(2,1)
        let matched_count = diff
            .row_alignment
            .iter()
            .filter(|a| matches!(a, RowAlignment::Matched { .. }))
            .count();
        let deleted_count = diff
            .row_alignment
            .iter()
            .filter(|a| matches!(a, RowAlignment::Deleted { .. }))
            .count();

        assert_eq!(matched_count, 2);
        assert_eq!(deleted_count, 1);
    }

    #[test]
    fn test_cell_text_change() {
        let old = make_table(vec![
            vec!["Header1", "Header2", "Header3"],
            vec!["Data A", "Data B", "Data C"],
        ]);
        let new = make_table(vec![
            vec!["Header1", "Header2", "Header3"],
            vec!["Data A", "Modified B", "Data C"],
        ]);

        let diff = diff_tables(&old, &new).expect("diff should succeed");

        // Exact rows retain semantic identity. A changed row shares only its
        // compatible physical shell and retains no inferred content lineage.
        assert_eq!(diff.row_alignment.len(), 2);
        assert!(
            matches!(
                diff.row_alignment[0],
                RowAlignment::Matched {
                    old_row: 0,
                    new_row: 0
                }
            ),
            "First row (headers) should be matched"
        );
        assert!(
            matches!(
                diff.row_alignment[1],
                RowAlignment::Replacement {
                    old_row: 1,
                    new_row: 1
                }
            ),
            "changed row should be a physical replacement"
        );

        // Every cell in an unrelated replacement row is revised completely,
        // including cells whose text happens to be equal.
        let modified_cells: Vec<_> = diff
            .cell_diffs
            .iter()
            .filter(|c| c.diff_type == CellDiffType::Modified)
            .collect();
        assert_eq!(
            modified_cells.len(),
            3,
            "every replacement-row cell should be Modified"
        );
        assert!(modified_cells.iter().all(|cell| {
            cell.text_diff.as_ref().is_some_and(|changes| {
                changes
                    .iter()
                    .all(|change| !matches!(change, InlineChange::Unchanged { .. }))
            })
        }));
    }

    #[test]
    fn test_word_level_diff() {
        let changes = diff_cell_text("hello world", "hello universe");

        // Should have: "hello " unchanged, "world" deleted, "universe" inserted
        let has_unchanged = changes
            .iter()
            .any(|c| matches!(c, InlineChange::Unchanged { text, .. } if text.contains("hello")));
        let has_deleted = changes
            .iter()
            .any(|c| matches!(c, InlineChange::Deleted { text, .. } if text.contains("world")));
        let has_inserted = changes
            .iter()
            .any(|c| matches!(c, InlineChange::Inserted { text, .. } if text.contains("universe")));

        assert!(has_unchanged, "Should have unchanged 'hello'");
        assert!(has_deleted, "Should have deleted 'world'");
        assert!(has_inserted, "Should have inserted 'universe'");
    }

    #[test]
    fn unrelated_three_by_three_to_four_by_three_reuses_three_shells() {
        let old = make_table(vec![
            vec!["A1", "A2", "A3"],
            vec!["B1", "B2", "B3"],
            vec!["C1", "C2", "C3"],
        ]);
        let new = make_table(vec![
            vec!["X1", "X2", "X3"],
            vec!["Y1", "Y2", "Y3"],
            vec!["Z1", "Z2", "Z3"],
            vec!["W1", "W2", "W3"],
        ]);

        let diff = diff_tables(&old, &new).expect("diff should succeed");
        assert_eq!(
            diff.row_alignment,
            vec![
                RowAlignment::Replacement {
                    old_row: 0,
                    new_row: 0,
                },
                RowAlignment::Replacement {
                    old_row: 1,
                    new_row: 1,
                },
                RowAlignment::Replacement {
                    old_row: 2,
                    new_row: 2,
                },
                RowAlignment::Inserted { new_row: 3 },
            ]
        );
    }

    #[test]
    fn incompatible_row_grid_is_not_shared() {
        let old = make_table(vec![vec!["source"]]);
        let new = make_table(vec![vec!["target-a", "target-b"]]);

        let diff = diff_tables(&old, &new).expect("diff should succeed");
        assert_eq!(
            diff.row_alignment,
            vec![
                RowAlignment::Deleted { old_row: 0 },
                RowAlignment::Inserted { new_row: 0 },
            ]
        );
    }

    /// Simultaneous insert + delete: one row inserted, one row deleted.
    /// Row count stays the same but content shifts. Patience diff should
    /// detect the shift and produce Inserted + Deleted alignments, not
    /// treat all rows as Matched with modified content.
    #[test]
    fn test_simultaneous_insert_and_delete() {
        // Before: rows "111", "222", "333", "444", "555", "666"
        // After:  rows "111", "1a",  "222", "333", "444", "555"
        // "1a" inserted, "666" deleted
        let old = make_table(vec![
            vec!["111"],
            vec!["222"],
            vec!["333"],
            vec!["444"],
            vec!["555"],
            vec!["666"],
        ]);
        let new = make_table(vec![
            vec!["111"],
            vec!["1a"],
            vec!["222"],
            vec!["333"],
            vec!["444"],
            vec!["555"],
        ]);

        let diff = diff_tables(&old, &new).expect("diff should succeed");

        // Count alignment types
        let matched = diff
            .row_alignment
            .iter()
            .filter(|a| matches!(a, RowAlignment::Matched { .. }))
            .count();
        let inserted = diff
            .row_alignment
            .iter()
            .filter(|a| matches!(a, RowAlignment::Inserted { .. }))
            .count();
        let deleted = diff
            .row_alignment
            .iter()
            .filter(|a| matches!(a, RowAlignment::Deleted { .. }))
            .count();

        assert_eq!(
            inserted, 1,
            "should have 1 inserted row ('1a'), got {inserted}"
        );
        assert_eq!(
            deleted, 1,
            "should have 1 deleted row ('666'), got {deleted}"
        );
        assert_eq!(matched, 5, "should have 5 matched rows, got {matched}");
    }

    /// Helper to create a cell that contains a nested table (plus a paragraph).
    fn make_nested_table_cell(
        id: &str,
        para_text: &str,
        inner_rows: Vec<Vec<&str>>,
    ) -> TableCellNode {
        let inner_table = make_table(inner_rows);
        // Adjust inner table ID to be unique based on cell
        let inner_table = TableNode {
            range_markers: Vec::new(),
            id: NodeId::from(format!("{}_inner_tbl", id)),
            ..inner_table
        };
        let para = BlockNode::from(ParagraphNode {
            id: NodeId::from(format!("{}_p", id)),
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
            segments: normal_segment(vec![InlineNode::from(TextNode {
                id: NodeId::from(format!("{}_t", id)),
                text_role: None,
                text: para_text.to_string(),
                marks: vec![],
                style_props: StyleProps::default(),
                rpr_authored: crate::domain::RunRprAuthored::default(),
                source_run_attrs: Vec::new(),
                formatting_change: None,
            })]),
            block_text_hash: None,
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
            paragraph_mark_style_props: StyleProps::default(),
            paragraph_mark_rfonts: Default::default(),
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
            source_paragraph_attrs: Vec::new(),
            preserved_ppr: Vec::new(),
            cell_move_id: None,
        });

        TableCellNode {
            cell_merge: None,
            id: NodeId::from(id.to_string()),
            blocks: vec![para, BlockNode::from(inner_table)],
            grid_span: 1,
            v_merge: VerticalMerge::None,
            formatting: CellFormatting::default(),
            formatting_change: None,
            tracking_status: None,
            row_sdt_wrapper: None,
            content_sdt_wraps: Vec::new(),
            cnf_style: None,
            hide_mark: false,
            preserved: Vec::new(),
        }
    }

    #[test]
    fn test_nested_table_text_change_detected() {
        // Outer table has one row with one cell containing a nested table.
        // The nested table's inner cell text changes.
        let old = TableNode {
            range_markers: Vec::new(),
            id: NodeId::from("outer"),
            rows: vec![TableRowNode {
                source_row_attrs: Vec::new(),
                id: NodeId::from("outer_r0"),
                cells: vec![make_nested_table_cell(
                    "outer_c0",
                    "Header",
                    vec![vec!["Inner A", "Inner B"]],
                )],
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
            }],
            structure_hash: String::new(),
            formatting: TableFormatting::default(),
            formatting_change: None,
        };

        let new = TableNode {
            range_markers: Vec::new(),
            id: NodeId::from("outer"),
            rows: vec![TableRowNode {
                source_row_attrs: Vec::new(),
                id: NodeId::from("outer_r0"),
                cells: vec![make_nested_table_cell(
                    "outer_c0",
                    "Header",
                    vec![vec!["Inner A", "Modified B"]],
                )],
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
            }],
            structure_hash: String::new(),
            formatting: TableFormatting::default(),
            formatting_change: None,
        };

        let diff = diff_tables(&old, &new).expect("diff should succeed");

        // A content change inside a nested table does not prove semantic row
        // lineage. The shell is a physical replacement candidate; the compiler
        // later refuses it because nested payload replacement is not qualified.
        assert_eq!(diff.row_alignment.len(), 1);
        assert!(matches!(
            diff.row_alignment[0],
            RowAlignment::Replacement {
                old_row: 0,
                new_row: 0
            }
        ));

        // The cell should be Modified because inner table text changed.
        let modified: Vec<_> = diff
            .cell_diffs
            .iter()
            .filter(|c| c.diff_type == CellDiffType::Modified)
            .collect();
        assert_eq!(modified.len(), 1, "should have 1 modified cell");

        // No nested semantic relation is inferred inside an unrelated row.
        let cell_diff = modified[0];
        assert!(
            cell_diff.nested_table_diffs.is_empty(),
            "replacement row must not infer nested table lineage"
        );

        // A replacement row never retains coincidental paragraph content as
        // unchanged lineage, even when only the nested payload triggered it.
        assert!(
            cell_diff.text_diff.as_ref().is_some_and(|changes| changes
                .iter()
                .all(|change| !matches!(change, InlineChange::Unchanged { .. }))),
            "replacement row must contain no unchanged text span"
        );
    }

    #[test]
    fn test_nested_table_both_para_and_table_change() {
        // Both the paragraph text and nested table content change.
        let old = TableNode {
            range_markers: Vec::new(),
            id: NodeId::from("outer"),
            rows: vec![TableRowNode {
                source_row_attrs: Vec::new(),
                id: NodeId::from("outer_r0"),
                cells: vec![make_nested_table_cell(
                    "outer_c0",
                    "Old Header",
                    vec![vec!["Inner A"]],
                )],
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
            }],
            structure_hash: String::new(),
            formatting: TableFormatting::default(),
            formatting_change: None,
        };

        let new = TableNode {
            range_markers: Vec::new(),
            id: NodeId::from("outer"),
            rows: vec![TableRowNode {
                source_row_attrs: Vec::new(),
                id: NodeId::from("outer_r0"),
                cells: vec![make_nested_table_cell(
                    "outer_c0",
                    "New Header",
                    vec![vec!["Inner Z"]],
                )],
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
            }],
            structure_hash: String::new(),
            formatting: TableFormatting::default(),
            formatting_change: None,
        };

        let diff = diff_tables(&old, &new).expect("diff should succeed");

        let modified: Vec<_> = diff
            .cell_diffs
            .iter()
            .filter(|c| c.diff_type == CellDiffType::Modified)
            .collect();
        assert_eq!(modified.len(), 1);

        let cell_diff = modified[0];
        // The replacement projection describes the cell text without trying
        // to infer a nested-table relation.
        assert!(
            cell_diff.text_diff.is_some(),
            "paragraph text changed so text_diff should be Some"
        );
        assert!(
            cell_diff.nested_table_diffs.is_empty(),
            "replacement row must not infer nested table lineage"
        );
    }

    #[test]
    fn test_nested_table_unchanged() {
        // Nested table content is the same — no diffs should be generated.
        let old = TableNode {
            range_markers: Vec::new(),
            id: NodeId::from("outer"),
            rows: vec![TableRowNode {
                source_row_attrs: Vec::new(),
                id: NodeId::from("outer_r0"),
                cells: vec![make_nested_table_cell(
                    "outer_c0",
                    "Header",
                    vec![vec!["Inner A", "Inner B"]],
                )],
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
            }],
            structure_hash: String::new(),
            formatting: TableFormatting::default(),
            formatting_change: None,
        };

        // Same content
        let new = old.clone();

        let diff = diff_tables(&old, &new).expect("diff should succeed");

        // All cells should be unchanged.
        for cell_diff in &diff.cell_diffs {
            assert_eq!(
                cell_diff.diff_type,
                CellDiffType::Unchanged,
                "cells should be unchanged when nested table is identical"
            );
            assert!(cell_diff.nested_table_diffs.is_empty());
        }
    }
}
