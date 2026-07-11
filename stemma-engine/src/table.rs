//! Table canonicalization: transforms DOCX physical cell list into logical grid.
//!
//! DOCX represents tables as a list of rows, each containing a list of cells.
//! Merged cells are represented via:
//! - `gridSpan`: horizontal merge (cell spans multiple columns)
//! - `vMerge`: vertical merge (cell spans multiple rows)
//! - `gridBefore/gridAfter`: empty columns at row start/end
//!
//! This module transforms that into a canonical grid model with explicit
//! owner mappings for O(1) lookup and proper anchor tracking for diffing.

use std::collections::HashMap;

use crate::domain::{
    BlockNode, CanonicalCell, CanonicalTable, InlineNode, TableNode, VerticalMerge,
};

/// Build canonical table from physical DOCX structure.
///
/// Transforms the physical cell list into a logical rectangular grid,
/// handling gridSpan (horizontal merge), vMerge (vertical merge),
/// and gridBefore/gridAfter (empty columns).
pub fn canonicalize_table(table: &TableNode) -> Result<CanonicalTable, String> {
    let n_rows = table.rows.len();
    let n_cols = compute_grid_width(table);

    // Collect per-row tracking status
    let row_tracking: Vec<Option<_>> = table
        .rows
        .iter()
        .map(|r| r.tracking_status.clone())
        .collect();

    if n_rows == 0 || n_cols == 0 {
        return Ok(CanonicalTable {
            id: table.id.clone(),
            n_rows,
            n_cols,
            cells: Vec::new(),
            owner_grid: Vec::new(),
            formatting: table.formatting.clone(),
            row_tracking,
        });
    }

    // Initialize owner grid
    let mut owner_grid: Vec<Vec<Option<usize>>> = vec![vec![None; n_cols]; n_rows];
    let mut cells: Vec<CanonicalCell> = Vec::new();

    // Track vMerge anchors: col -> cell_index
    // IMPORTANT: Register for EVERY column in a colspan, not just the first
    let mut v_merge_anchors: HashMap<usize, usize> = HashMap::new();

    for (row_idx, row) in table.rows.iter().enumerate() {
        let mut col = row.grid_before as usize;

        for cell in &row.cells {
            // Skip columns that are occupied by spans from above
            while col < n_cols && owner_grid[row_idx][col].is_some() {
                col += 1;
            }
            // Clamp to grid width (resilient to inconsistent docs)
            if col >= n_cols {
                break;
            }

            let colspan = (cell.grid_span as usize).min(n_cols - col);

            match cell.v_merge {
                VerticalMerge::Continue => {
                    // This cell continues a vertical merge from above.
                    // Look up the anchor cell for this column.
                    let anchor_idx = v_merge_anchors.get(&col).copied();

                    if let Some(cell_idx) = anchor_idx {
                        // Extend the anchor cell's rowspan
                        cells[cell_idx].rowspan += 1;
                        // Mark all positions as owned by the anchor
                        let end = (col + colspan).min(n_cols);
                        for slot in &mut owner_grid[row_idx][col..end] {
                            *slot = Some(cell_idx);
                        }
                    } else {
                        return Err(format!(
                            "Invalid vertical merge: <w:vMerge/> continue at row {row_idx}, \
                             column {col} in table '{}' has no preceding restart anchor",
                            table.id.0
                        ));
                    }
                }
                VerticalMerge::Restart | VerticalMerge::None => {
                    // New cell (anchor)
                    let cell_idx = cells.len();
                    cells.push(CanonicalCell {
                        id: cell.id.clone(),
                        row: row_idx,
                        col,
                        rowspan: 1,
                        colspan,
                        blocks: cell.blocks.clone(),
                        text: extract_cell_text(&cell.blocks),
                        formatting: cell.formatting.clone(),
                    });

                    // Mark owner_grid for all covered positions
                    let end = (col + colspan).min(n_cols);
                    for slot in &mut owner_grid[row_idx][col..end] {
                        *slot = Some(cell_idx);
                    }

                    // Track vMerge anchor for ALL columns in colspan
                    if cell.v_merge == VerticalMerge::Restart {
                        for c in col..col + colspan {
                            v_merge_anchors.insert(c, cell_idx);
                        }
                    } else {
                        // Clear any previous anchors in this span
                        for c in col..col + colspan {
                            v_merge_anchors.remove(&c);
                        }
                    }
                }
            }

            col += colspan;
        }

        // After processing each row, extend any active rowspans that weren't
        // covered by explicit cells (handles partial vMerge coverage)
    }

    Ok(CanonicalTable {
        id: table.id.clone(),
        n_rows,
        n_cols,
        cells,
        owner_grid,
        formatting: table.formatting.clone(),
        row_tracking,
    })
}

/// Compute the grid width (number of columns) for a table.
///
/// Tries `tblGrid` element first (not available in our model), then infers
/// from rows: max(gridBefore + sum(gridSpan) + gridAfter).
fn compute_grid_width(table: &TableNode) -> usize {
    // Infer from rows: max(gridBefore + sum(gridSpan) + gridAfter)
    table
        .rows
        .iter()
        .map(|row| {
            let cell_span: u32 = row.cells.iter().map(|c| c.grid_span).sum();
            (row.grid_before + cell_span + row.grid_after) as usize
        })
        .max()
        .unwrap_or(0)
}

/// Extract text from cell blocks for diffing.
///
/// Concatenates text from all paragraphs, headings, and nested tables.
pub fn extract_cell_text(blocks: &[BlockNode]) -> String {
    let mut text = String::new();
    for block in blocks {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&extract_block_text(block));
    }
    text
}

/// Extract text from a single block.
fn extract_block_text(block: &BlockNode) -> String {
    match block {
        BlockNode::Paragraph(p) => {
            let inlines = p.all_inlines_owned();
            let inline_text = extract_inlines_text(&inlines);
            // Fall back to rendered_text for paragraphs with no inline text
            // but visible rendered content (e.g. bullet-only numbering).
            // Matches the fallback pattern in diff.rs block-level hashing.
            if inline_text.trim().is_empty()
                && let Some(rendered) = &p.rendered_text
                && !rendered.trim().is_empty()
            {
                return rendered.clone();
            }
            inline_text
        }
        BlockNode::Table(t) => extract_table_text(t),
        BlockNode::OpaqueBlock(_) => String::new(),
    }
}

/// Extract text from inline nodes.
pub(crate) fn extract_inlines_text(inlines: &[InlineNode]) -> String {
    let mut text = String::new();
    for inline in inlines {
        match inline {
            InlineNode::Text(t) => text.push_str(&t.text),
            InlineNode::HardBreak(_) => text.push('\n'),
            InlineNode::OpaqueInline(_) => text.push('\u{FFFC}'), // Include placeholder so row signatures distinguish rows with drawings from rows without
            InlineNode::Decoration(_) => {}
            InlineNode::CommentRangeStart { .. } => {}
            InlineNode::CommentRangeEnd { .. } => {}
            InlineNode::CommentReference { .. } => {}
        }
    }
    text
}

/// Extract text from a nested table.
fn extract_table_text(table: &TableNode) -> String {
    let mut text = String::new();
    for row in &table.rows {
        for cell in &row.cells {
            if !text.is_empty() {
                text.push('\t');
            }
            text.push_str(&extract_cell_text(&cell.blocks));
        }
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        CellFormatting, NodeId, ParagraphNode, StyleProps, TableCellNode, TableFormatting,
        TableRowNode, TextNode, normal_segment,
    };

    fn make_text_cell(id: &str, text: &str) -> TableCellNode {
        TableCellNode {
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
                    formatting_change: None,
                })]),
                block_text_hash: None,
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

    fn make_merged_cell(
        id: &str,
        text: &str,
        grid_span: u32,
        v_merge: VerticalMerge,
    ) -> TableCellNode {
        let mut cell = make_text_cell(id, text);
        cell.grid_span = grid_span;
        cell.v_merge = v_merge;
        cell
    }

    #[test]
    fn test_simple_3x3_table() {
        let table = TableNode {
            id: NodeId::from("tbl_0"),
            rows: vec![
                TableRowNode {
                    id: NodeId::from("tbl_0_r0"),
                    cells: vec![
                        make_text_cell("c00", "A"),
                        make_text_cell("c01", "B"),
                        make_text_cell("c02", "C"),
                    ],
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
                },
                TableRowNode {
                    id: NodeId::from("tbl_0_r1"),
                    cells: vec![
                        make_text_cell("c10", "D"),
                        make_text_cell("c11", "E"),
                        make_text_cell("c12", "F"),
                    ],
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
                },
                TableRowNode {
                    id: NodeId::from("tbl_0_r2"),
                    cells: vec![
                        make_text_cell("c20", "G"),
                        make_text_cell("c21", "H"),
                        make_text_cell("c22", "I"),
                    ],
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
                },
            ],
            structure_hash: String::new(),
            formatting: TableFormatting::default(),
            formatting_change: None,
        };

        let canonical = canonicalize_table(&table).expect("canonicalize should succeed");

        assert_eq!(canonical.n_rows, 3);
        assert_eq!(canonical.n_cols, 3);
        assert_eq!(canonical.cells.len(), 9);

        // Check all cells are properly positioned
        for row in 0..3 {
            for col in 0..3 {
                let cell = canonical.cell_at(row, col).unwrap();
                assert_eq!(cell.row, row);
                assert_eq!(cell.col, col);
                assert_eq!(cell.rowspan, 1);
                assert_eq!(cell.colspan, 1);
            }
        }

        // Check specific content
        assert_eq!(canonical.cell_at(0, 0).unwrap().text, "A");
        assert_eq!(canonical.cell_at(1, 1).unwrap().text, "E");
        assert_eq!(canonical.cell_at(2, 2).unwrap().text, "I");
    }

    #[test]
    fn test_horizontal_merge() {
        // Row 0: [A spans 2 cols] [C]
        // Row 1: [D] [E] [F]
        let table = TableNode {
            id: NodeId::from("tbl_0"),
            rows: vec![
                TableRowNode {
                    id: NodeId::from("tbl_0_r0"),
                    cells: vec![
                        make_merged_cell("c00", "A", 2, VerticalMerge::None),
                        make_text_cell("c02", "C"),
                    ],
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
                },
                TableRowNode {
                    id: NodeId::from("tbl_0_r1"),
                    cells: vec![
                        make_text_cell("c10", "D"),
                        make_text_cell("c11", "E"),
                        make_text_cell("c12", "F"),
                    ],
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
                },
            ],
            structure_hash: String::new(),
            formatting: TableFormatting::default(),
            formatting_change: None,
        };

        let canonical = canonicalize_table(&table).expect("canonicalize should succeed");

        assert_eq!(canonical.n_rows, 2);
        assert_eq!(canonical.n_cols, 3);
        assert_eq!(canonical.cells.len(), 5); // A, C, D, E, F

        // Cell A spans columns 0-1
        let cell_a = canonical.cell_at(0, 0).unwrap();
        assert_eq!(cell_a.text, "A");
        assert_eq!(cell_a.colspan, 2);

        // Position (0, 1) should point to the same cell A
        let cell_01 = canonical.cell_at(0, 1).unwrap();
        assert_eq!(cell_01.text, "A");
        assert_eq!(cell_01.col, 0); // Anchor is at column 0

        // Cell C is at column 2
        let cell_c = canonical.cell_at(0, 2).unwrap();
        assert_eq!(cell_c.text, "C");
        assert_eq!(cell_c.col, 2);

        // Check anchor status
        assert!(canonical.is_anchor(0, 0)); // A's anchor
        assert!(!canonical.is_anchor(0, 1)); // A's span
        assert!(canonical.is_anchor(0, 2)); // C's anchor
    }

    #[test]
    fn test_vertical_merge() {
        // Col 0: [A spans 2 rows] Col 1: [B]
        //                         Col 1: [D]
        let table = TableNode {
            id: NodeId::from("tbl_0"),
            rows: vec![
                TableRowNode {
                    id: NodeId::from("tbl_0_r0"),
                    cells: vec![
                        make_merged_cell("c00", "A", 1, VerticalMerge::Restart),
                        make_text_cell("c01", "B"),
                    ],
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
                },
                TableRowNode {
                    id: NodeId::from("tbl_0_r1"),
                    cells: vec![
                        make_merged_cell("c10", "", 1, VerticalMerge::Continue),
                        make_text_cell("c11", "D"),
                    ],
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
                },
            ],
            structure_hash: String::new(),
            formatting: TableFormatting::default(),
            formatting_change: None,
        };

        let canonical = canonicalize_table(&table).expect("canonicalize should succeed");

        assert_eq!(canonical.n_rows, 2);
        assert_eq!(canonical.n_cols, 2);
        assert_eq!(canonical.cells.len(), 3); // A (spanning), B, D

        // Cell A spans rows 0-1
        let cell_a = canonical.cell_at(0, 0).unwrap();
        assert_eq!(cell_a.text, "A");
        assert_eq!(cell_a.rowspan, 2);

        // Position (1, 0) should point to the same cell A
        let cell_10 = canonical.cell_at(1, 0).unwrap();
        assert_eq!(cell_10.text, "A");
        assert_eq!(cell_10.row, 0); // Anchor is at row 0

        // Check anchor status
        assert!(canonical.is_anchor(0, 0)); // A's anchor
        assert!(!canonical.is_anchor(1, 0)); // A's span
        assert!(canonical.is_anchor(0, 1)); // B's anchor
        assert!(canonical.is_anchor(1, 1)); // D's anchor
    }

    #[test]
    fn test_block_merge() {
        // Combined 2x2 merge:
        // [A spans 2 cols, 2 rows] [C]
        //                          [F]
        let table = TableNode {
            id: NodeId::from("tbl_0"),
            rows: vec![
                TableRowNode {
                    id: NodeId::from("tbl_0_r0"),
                    cells: vec![
                        make_merged_cell("c00", "A", 2, VerticalMerge::Restart),
                        make_text_cell("c02", "C"),
                    ],
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
                },
                TableRowNode {
                    id: NodeId::from("tbl_0_r1"),
                    cells: vec![
                        make_merged_cell("c10", "", 2, VerticalMerge::Continue),
                        make_text_cell("c12", "F"),
                    ],
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
                },
            ],
            structure_hash: String::new(),
            formatting: TableFormatting::default(),
            formatting_change: None,
        };

        let canonical = canonicalize_table(&table).expect("canonicalize should succeed");

        assert_eq!(canonical.n_rows, 2);
        assert_eq!(canonical.n_cols, 3);
        assert_eq!(canonical.cells.len(), 3); // A, C, F

        // Cell A spans 2x2
        let cell_a = canonical.cell_at(0, 0).unwrap();
        assert_eq!(cell_a.text, "A");
        assert_eq!(cell_a.rowspan, 2);
        assert_eq!(cell_a.colspan, 2);

        // All four positions (0,0), (0,1), (1,0), (1,1) should point to A
        for (r, c) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
            let cell = canonical.cell_at(r, c).unwrap();
            assert_eq!(cell.text, "A", "Position ({}, {}) should be A", r, c);
        }

        // Only (0, 0) should be an anchor
        assert!(canonical.is_anchor(0, 0));
        assert!(!canonical.is_anchor(0, 1));
        assert!(!canonical.is_anchor(1, 0));
        assert!(!canonical.is_anchor(1, 1));
    }

    #[test]
    fn test_grid_before_after() {
        // Row 0: [empty] [A] [B]
        // Row 1: [C] [D] [empty]
        let table = TableNode {
            id: NodeId::from("tbl_0"),
            rows: vec![
                TableRowNode {
                    id: NodeId::from("tbl_0_r0"),
                    cells: vec![make_text_cell("c01", "A"), make_text_cell("c02", "B")],
                    grid_before: 1,
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
                },
                TableRowNode {
                    id: NodeId::from("tbl_0_r1"),
                    cells: vec![make_text_cell("c10", "C"), make_text_cell("c11", "D")],
                    grid_before: 0,
                    grid_after: 1,
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
                },
            ],
            structure_hash: String::new(),
            formatting: TableFormatting::default(),
            formatting_change: None,
        };

        let canonical = canonicalize_table(&table).expect("canonicalize should succeed");

        assert_eq!(canonical.n_rows, 2);
        assert_eq!(canonical.n_cols, 3);

        // Row 0: empty at 0, A at 1, B at 2
        assert!(canonical.cell_at(0, 0).is_none());
        assert_eq!(canonical.cell_at(0, 1).unwrap().text, "A");
        assert_eq!(canonical.cell_at(0, 2).unwrap().text, "B");

        // Row 1: C at 0, D at 1, empty at 2
        assert_eq!(canonical.cell_at(1, 0).unwrap().text, "C");
        assert_eq!(canonical.cell_at(1, 1).unwrap().text, "D");
        assert!(canonical.cell_at(1, 2).is_none());
    }

    #[test]
    fn test_row_signature() {
        // Row 0: [A spans 2 cols] [C]
        // Row 1: [D] [E] [F]
        let table = TableNode {
            id: NodeId::from("tbl_0"),
            rows: vec![
                TableRowNode {
                    id: NodeId::from("tbl_0_r0"),
                    cells: vec![
                        make_merged_cell("c00", "A", 2, VerticalMerge::None),
                        make_text_cell("c02", "C"),
                    ],
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
                },
                TableRowNode {
                    id: NodeId::from("tbl_0_r1"),
                    cells: vec![
                        make_text_cell("c10", "D"),
                        make_text_cell("c11", "E"),
                        make_text_cell("c12", "F"),
                    ],
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
                },
            ],
            structure_hash: String::new(),
            formatting: TableFormatting::default(),
            formatting_change: None,
        };

        let canonical = canonicalize_table(&table).expect("canonicalize should succeed");

        // Row 0: A (anchor), ↔ (colspan), C
        let sig0 = canonical.row_signature(0);
        assert_eq!(sig0, "A | ↔ | C");

        // Row 1: D, E, F (all anchors)
        let sig1 = canonical.row_signature(1);
        assert_eq!(sig1, "D | E | F");
    }

    #[test]
    fn test_row_signature_with_vertical_merge() {
        // Col 0: [A spans 2 rows]
        // Col 1: [B], [D]
        let table = TableNode {
            id: NodeId::from("tbl_0"),
            rows: vec![
                TableRowNode {
                    id: NodeId::from("tbl_0_r0"),
                    cells: vec![
                        make_merged_cell("c00", "A", 1, VerticalMerge::Restart),
                        make_text_cell("c01", "B"),
                    ],
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
                },
                TableRowNode {
                    id: NodeId::from("tbl_0_r1"),
                    cells: vec![
                        make_merged_cell("c10", "", 1, VerticalMerge::Continue),
                        make_text_cell("c11", "D"),
                    ],
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
                },
            ],
            structure_hash: String::new(),
            formatting: TableFormatting::default(),
            formatting_change: None,
        };

        let canonical = canonicalize_table(&table).expect("canonicalize should succeed");

        // Row 0: A (anchor), B
        let sig0 = canonical.row_signature(0);
        assert_eq!(sig0, "A | B");

        // Row 1: ↕ (rowspan from above), D
        let sig1 = canonical.row_signature(1);
        assert_eq!(sig1, "↕ | D");
    }

    #[test]
    fn test_continue_without_restart() {
        // Lenient handling: Continue without prior Restart
        let table = TableNode {
            id: NodeId::from("tbl_0"),
            rows: vec![
                TableRowNode {
                    id: NodeId::from("tbl_0_r0"),
                    cells: vec![
                        make_text_cell("c00", "A"), // No Restart
                        make_text_cell("c01", "B"),
                    ],
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
                },
                TableRowNode {
                    id: NodeId::from("tbl_0_r1"),
                    cells: vec![
                        make_merged_cell("c10", "X", 1, VerticalMerge::Continue), // Continue without Restart
                        make_text_cell("c11", "D"),
                    ],
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
                },
            ],
            structure_hash: String::new(),
            formatting: TableFormatting::default(),
            formatting_change: None,
        };

        let result = canonicalize_table(&table);

        // Should return an error for vMerge continue without restart
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("Invalid vertical merge"),
            "Error should describe invalid vMerge: {err}"
        );
        assert!(
            err.contains("row 1") && err.contains("column 0"),
            "Error should include row/column context: {err}"
        );
    }
}
