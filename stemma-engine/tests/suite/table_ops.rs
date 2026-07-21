//! Integration tests for the granular `table_op` v4 ops (`verbs::table_ops`).
//!
//! Each op routes through the SAME table-diff machinery `replace(table)` uses,
//! so it materializes as row/cell-level tracked changes. The domain rule these
//! tests encode (OOXML §17.13 accept/reject invariants):
//!
//!   - `accept_all` on a tracked granular table op yields the TARGET table
//!     (the op's intended result), and
//!   - `reject_all` yields the BASE table (the op never happened).
//!
//! Plus the fail-loud contract: out-of-range indices, ragged/merged grids for
//! column ops, and non-rectangular merge regions are refused with a typed,
//! table-addressed error — never silently clamped or guessed.

use stemma::accept_all;
use stemma::domain::*;
use stemma::edit::{EditTransaction, apply_transaction};
use stemma::edit_v4::parse_transaction;
use stemma::reject_all_with_styles;

// ─── Doc-construction helpers (mirrors redline_table_replace.rs) ──────────────

fn make_para(id: &str, text: &str) -> ParagraphNode {
    ParagraphNode {
        id: NodeId::from(id),
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
            id: NodeId::from(format!("{id}_t")),
            text_role: None,
            text: text.to_string(),
            marks: vec![],
            style_props: StyleProps::default(),
            rpr_authored: stemma::domain::RunRprAuthored::default(),
            source_run_attrs: Vec::new(),
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
        literal_prefix_style_props: StyleProps::default(),
        literal_prefix_rpr_authored: stemma::domain::RunRprAuthored::default(),
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

fn make_cell(id: &str, text: &str) -> TableCellNode {
    TableCellNode {
        id: NodeId::from(id),
        blocks: vec![BlockNode::from(make_para(&format!("{id}_p"), text))],
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

fn make_row(id: &str, cells: Vec<TableCellNode>) -> TableRowNode {
    TableRowNode {
        id: NodeId::from(id),
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
    }
}

fn make_table(id: &str, rows: Vec<TableRowNode>) -> TableNode {
    TableNode {
        id: NodeId::from(id),
        rows,
        structure_hash: String::new(),
        formatting: TableFormatting::default(),
        formatting_change: None,
    }
}

fn doc_with_table(table: TableNode) -> CanonDoc {
    CanonDoc {
        id: NodeId::from("doc1"),
        blocks: vec![
            normal_tracked_block(BlockNode::from(make_para("body", "body"))),
            normal_tracked_block(BlockNode::from(table)),
        ],
        meta: DocMeta {
            schema_version: SCHEMA_VERSION_V0.to_string(),
            docx_fingerprint: DocFingerprint("test".to_string()),
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
    }
}

/// A 2×2 grid with cell texts r{r}c{c}.
fn grid_2x2() -> CanonDoc {
    let table = make_table(
        "t1",
        vec![
            make_row(
                "t1_r0",
                vec![make_cell("t1_r0c0", "r0c0"), make_cell("t1_r0c1", "r0c1")],
            ),
            make_row(
                "t1_r1",
                vec![make_cell("t1_r1c0", "r1c0"), make_cell("t1_r1c1", "r1c1")],
            ),
        ],
    );
    doc_with_table(table)
}

fn translate(json: &str) -> EditTransaction {
    parse_transaction(json)
        .expect("schema check passes")
        .into_edit_transaction()
        .expect("adapter succeeds")
}

fn try_translate(json: &str) -> Result<EditTransaction, String> {
    parse_transaction(json)
        .map_err(|e| e.to_string())?
        .into_edit_transaction()
        .map_err(|e| e.to_string())
}

/// rows × cells text matrix for the named table, dropping rows whose
/// `tracking_status` is Deleted and cells whose blocks are all Deleted segments
/// (the post-projection live tree). For accept/reject comparison we read live
/// text directly.
fn cell_texts(doc: &CanonDoc, table_id: &str) -> Vec<Vec<String>> {
    let nid = NodeId::from(table_id);
    let table = doc
        .blocks
        .iter()
        .find_map(|tb| match &tb.block {
            BlockNode::Table(t) if t.id == nid => Some(t),
            _ => None,
        })
        .expect("table not found");
    table
        .rows
        .iter()
        .filter(|r| !matches!(r.tracking_status, Some(TrackingStatus::Deleted(_))))
        .map(|row| {
            row.cells
                .iter()
                .filter(|c| !matches!(c.tracking_status, Some(TrackingStatus::Deleted(_))))
                .map(|cell| {
                    let mut text = String::new();
                    for block in &cell.blocks {
                        if let BlockNode::Paragraph(p) = block {
                            for seg in &p.segments {
                                if matches!(seg.status, TrackingStatus::Deleted(_)) {
                                    continue;
                                }
                                for i in &seg.inlines {
                                    if let InlineNode::Text(t) = i {
                                        text.push_str(&t.text);
                                    }
                                }
                            }
                        }
                    }
                    text
                })
                .collect()
        })
        .collect()
}

fn table_op_json(target: &str, op_body: &str) -> String {
    format!(
        r#"{{ "ops": [{{ "op": "table_op", "target": "{target}", "table_op": {op_body} }}],
            "revision": {{ "author": "Counsel" }} }}"#
    )
}

fn find_table<'a>(doc: &'a CanonDoc, id: &str) -> &'a TableNode {
    let nid = NodeId::from(id);
    doc.blocks
        .iter()
        .find_map(|tb| match &tb.block {
            BlockNode::Table(t) if t.id == nid => Some(t),
            _ => None,
        })
        .expect("table not found")
}

// ─── InsertTableRow ──────────────────────────────────────────────────────────

#[test]
fn insert_row_accept_is_target_reject_is_base() {
    let doc = grid_2x2();
    let txn = translate(&table_op_json(
        "t1",
        r#"{ "kind": "insert_row", "ref_row": 1, "position": "after" }"#,
    ));
    let edited = apply_transaction(&doc, &txn).expect("apply").0;

    let mut accepted = edited.clone();
    accept_all(&mut accepted);
    // accept => a new (empty) third row exists.
    let acc = cell_texts(&accepted, "t1");
    assert_eq!(
        acc.len(),
        3,
        "accept_all on insert_row yields a third row: {acc:?}"
    );
    assert_eq!(acc[0], vec!["r0c0", "r0c1"]);
    assert_eq!(acc[1], vec!["r1c0", "r1c1"]);
    assert_eq!(acc[2], vec!["", ""], "inserted row is empty");

    let mut rejected = edited.clone();
    reject_all_with_styles(&mut rejected, None);
    // reject => the base 2x2 grid (the inserted row never happened).
    assert_eq!(
        cell_texts(&rejected, "t1"),
        vec![vec!["r0c0", "r0c1"], vec!["r1c0", "r1c1"]],
        "reject_all on insert_row restores the base"
    );
}

#[test]
fn insert_row_out_of_range_refused() {
    let doc = grid_2x2();
    let txn = translate(&table_op_json(
        "t1",
        r#"{ "kind": "insert_row", "ref_row": 9, "position": "after" }"#,
    ));
    let err = apply_transaction(&doc, &txn).unwrap_err();
    assert!(
        format!("{err:?}").contains("TableRowIndexOutOfRange"),
        "expected TableRowIndexOutOfRange, got {err:?}"
    );
}

// ─── DeleteTableRow ──────────────────────────────────────────────────────────

#[test]
fn delete_row_accept_is_target_reject_is_base() {
    let doc = grid_2x2();
    let txn = translate(&table_op_json(
        "t1",
        r#"{ "kind": "delete_row", "row_index": 0 }"#,
    ));
    let edited = apply_transaction(&doc, &txn).expect("apply").0;

    let mut accepted = edited.clone();
    accept_all(&mut accepted);
    assert_eq!(
        cell_texts(&accepted, "t1"),
        vec![vec!["r1c0", "r1c1"]],
        "accept_all on delete_row drops row 0"
    );

    let mut rejected = edited.clone();
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        cell_texts(&rejected, "t1"),
        vec![vec!["r0c0", "r0c1"], vec!["r1c0", "r1c1"]],
        "reject_all on delete_row restores both rows"
    );
}

#[test]
fn delete_last_remaining_row_refused() {
    let table = make_table(
        "t1",
        vec![make_row("t1_r0", vec![make_cell("t1_r0c0", "x")])],
    );
    let doc = doc_with_table(table);
    let txn = translate(&table_op_json(
        "t1",
        r#"{ "kind": "delete_row", "row_index": 0 }"#,
    ));
    let err = apply_transaction(&doc, &txn).unwrap_err();
    assert!(
        format!("{err:?}").contains("TableWouldBeEmpty"),
        "expected TableWouldBeEmpty, got {err:?}"
    );
}

// ─── InsertTableColumn ───────────────────────────────────────────────────────

#[test]
fn insert_column_accept_is_target_reject_is_base() {
    let doc = grid_2x2();
    let txn = translate(&table_op_json(
        "t1",
        r#"{ "kind": "insert_column", "ref_col": 1, "position": "after" }"#,
    ));
    let edited = apply_transaction(&doc, &txn).expect("apply").0;

    let mut accepted = edited.clone();
    accept_all(&mut accepted);
    let acc = cell_texts(&accepted, "t1");
    assert_eq!(
        acc[0],
        vec!["r0c0", "r0c1", ""],
        "new empty 3rd column on row 0"
    );
    assert_eq!(
        acc[1],
        vec!["r1c0", "r1c1", ""],
        "new empty 3rd column on row 1"
    );

    let mut rejected = edited.clone();
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        cell_texts(&rejected, "t1"),
        vec![vec!["r0c0", "r0c1"], vec!["r1c0", "r1c1"]],
        "reject_all on insert_column restores the 2-column base"
    );
}

// ─── DeleteTableColumn ───────────────────────────────────────────────────────

#[test]
fn delete_column_accept_is_target_reject_is_base() {
    let doc = grid_2x2();
    let txn = translate(&table_op_json(
        "t1",
        r#"{ "kind": "delete_column", "col_index": 0 }"#,
    ));
    let edited = apply_transaction(&doc, &txn).expect("apply").0;

    let mut accepted = edited.clone();
    accept_all(&mut accepted);
    assert_eq!(
        cell_texts(&accepted, "t1"),
        vec![vec!["r0c1"], vec!["r1c1"]],
        "accept_all on delete_column drops column 0"
    );

    let mut rejected = edited.clone();
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        cell_texts(&rejected, "t1"),
        vec![vec!["r0c0", "r0c1"], vec!["r1c0", "r1c1"]],
        "reject_all on delete_column restores both columns"
    );
}

#[test]
fn column_op_on_merged_grid_refused() {
    // Build a table whose first row has a span-2 cell -> merged grid.
    let mut merged = make_cell("t1_r0c0", "wide");
    merged.grid_span = 2;
    let table = make_table(
        "t1",
        vec![
            make_row("t1_r0", vec![merged]),
            make_row(
                "t1_r1",
                vec![make_cell("t1_r1c0", "a"), make_cell("t1_r1c1", "b")],
            ),
        ],
    );
    let doc = doc_with_table(table);
    let txn = translate(&table_op_json(
        "t1",
        r#"{ "kind": "delete_column", "col_index": 0 }"#,
    ));
    let err = apply_transaction(&doc, &txn).unwrap_err();
    assert!(
        format!("{err:?}").contains("TableColumnOpOnMergedGrid"),
        "expected TableColumnOpOnMergedGrid, got {err:?}"
    );
}

// ─── MergeCells ──────────────────────────────────────────────────────────────

#[test]
fn merge_horizontal_accept_sets_gridspan() {
    // Merge row 0's two cells into one span-2 cell.
    let doc = grid_2x2();
    let txn = translate(&table_op_json(
        "t1",
        r#"{ "kind": "merge_cells", "start_row": 0, "start_col": 0, "end_row": 0, "end_col": 1 }"#,
    ));
    let edited = apply_transaction(&doc, &txn).expect("apply").0;

    let mut accepted = edited.clone();
    accept_all(&mut accepted);
    let nid = NodeId::from("t1");
    let t = accepted
        .blocks
        .iter()
        .find_map(|tb| match &tb.block {
            BlockNode::Table(t) if t.id == nid => Some(t),
            _ => None,
        })
        .expect("table");
    // Row 0 now has one cell spanning 2 columns.
    let live_cells: Vec<&TableCellNode> = t.rows[0]
        .cells
        .iter()
        .filter(|c| !matches!(c.tracking_status, Some(TrackingStatus::Deleted(_))))
        .collect();
    assert_eq!(live_cells.len(), 1, "merged row 0 has a single live cell");
    assert_eq!(live_cells[0].grid_span, 2, "merged cell spans 2 columns");
}

#[test]
fn merge_single_cell_refused() {
    let doc = grid_2x2();
    let txn = translate(&table_op_json(
        "t1",
        r#"{ "kind": "merge_cells", "start_row": 0, "start_col": 0, "end_row": 0, "end_col": 0 }"#,
    ));
    let err = apply_transaction(&doc, &txn).unwrap_err();
    assert!(
        format!("{err:?}").contains("MergeRegionNotRectangular"),
        "expected MergeRegionNotRectangular, got {err:?}"
    );
}

// ─── SetCellText ─────────────────────────────────────────────────────────────

#[test]
fn set_cell_text_accept_is_target_reject_is_base() {
    let doc = grid_2x2();
    let txn = translate(&table_op_json(
        "t1",
        r#"{ "kind": "set_cell_text", "row_index": 0, "col_index": 1, "text": "REVISED" }"#,
    ));
    let edited = apply_transaction(&doc, &txn).expect("apply").0;

    let mut accepted = edited.clone();
    accept_all(&mut accepted);
    let acc = cell_texts(&accepted, "t1");
    let flat: String = acc.iter().flatten().cloned().collect::<Vec<_>>().join("|");
    assert!(
        flat.contains("REVISED") && !flat.contains("r0c1"),
        "accept_all on set_cell_text yields the revised cell: {flat:?}"
    );

    let mut rejected = edited.clone();
    reject_all_with_styles(&mut rejected, None);
    let rej = cell_texts(&rejected, "t1");
    let rflat: String = rej.iter().flatten().cloned().collect::<Vec<_>>().join("|");
    assert!(
        rflat.contains("r0c1") && !rflat.contains("REVISED"),
        "reject_all on set_cell_text restores the base cell: {rflat:?}"
    );
}

#[test]
fn set_cell_text_out_of_range_refused() {
    let doc = grid_2x2();
    let txn = translate(&table_op_json(
        "t1",
        r#"{ "kind": "set_cell_text", "row_index": 0, "col_index": 9, "text": "x" }"#,
    ));
    let err = apply_transaction(&doc, &txn).unwrap_err();
    assert!(
        format!("{err:?}").contains("TableColumnIndexOutOfRange"),
        "expected TableColumnIndexOutOfRange, got {err:?}"
    );
}

// ─── InsertRow with content ─────────────────────────────
//
// A tracked `insert_row` must be able to carry its own content in the SAME
// op: one tracked row insertion, not a blank-insert-then-fill dead end.

#[test]
fn insert_row_with_cells_carries_content() {
    let doc = grid_2x2();
    let txn = translate(&table_op_json(
        "t1",
        r#"{ "kind": "insert_row", "ref_row": 1, "position": "after", "cells": ["NEW0", "NEW1"] }"#,
    ));
    let edited = apply_transaction(&doc, &txn).expect("apply").0;

    let mut accepted = edited.clone();
    accept_all(&mut accepted);
    let acc = cell_texts(&accepted, "t1");
    assert_eq!(acc.len(), 3, "accept_all yields a third row: {acc:?}");
    assert_eq!(
        acc[2],
        vec!["NEW0", "NEW1"],
        "the inserted row carries the content given in the SAME op"
    );

    let mut rejected = edited.clone();
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        cell_texts(&rejected, "t1"),
        vec![vec!["r0c0", "r0c1"], vec!["r1c0", "r1c1"]],
        "reject_all restores exactly the base grid"
    );
}

#[test]
fn insert_row_with_fewer_cells_than_columns_pads_the_rest_empty() {
    let doc = grid_2x2();
    let txn = translate(&table_op_json(
        "t1",
        r#"{ "kind": "insert_row", "ref_row": 1, "position": "after", "cells": ["ONLY0"] }"#,
    ));
    let edited = apply_transaction(&doc, &txn).expect("apply").0;

    let mut accepted = edited.clone();
    accept_all(&mut accepted);
    assert_eq!(
        cell_texts(&accepted, "t1")[2],
        vec!["ONLY0", ""],
        "fewer texts than columns pads the remaining cells empty, no clamping of the grid"
    );
}

#[test]
fn insert_row_with_more_cells_than_columns_refused() {
    let doc = grid_2x2();
    let txn = translate(&table_op_json(
        "t1",
        r#"{ "kind": "insert_row", "ref_row": 1, "position": "after", "cells": ["a", "b", "c"] }"#,
    ));
    let err = apply_transaction(&doc, &txn).unwrap_err();
    let msg = format!("{err:?}");
    assert!(
        msg.contains("TableInsertRowCellCountExceedsColumns"),
        "expected TableInsertRowCellCountExceedsColumns, got {msg}"
    );
    let display = format!("{err}");
    assert!(
        display.contains('2') && display.contains('3'),
        "the error must name both the given count and the actual column count, got: {display}"
    );
}

#[test]
fn insert_row_blank_then_set_cell_text_in_same_transaction_matches_atomic_form() {
    // The two-op shape a cold agent falls back to: insert a blank tracked row,
    // then fill it with set_cell_text, in the SAME transaction. This must now
    // succeed (not dead-end into TableCellNotEditable) and must be
    // EQUIVALENT to the atomic `insert_row` + `cells` form: same final text,
    // and reject-all restores the original grid exactly (no extra tracked
    // layer from the fill).
    let doc = grid_2x2();
    let json = r#"{
      "ops": [
        { "op": "table_op", "target": "t1", "table_op": { "kind": "insert_row", "ref_row": 1, "position": "after" } },
        { "op": "table_op", "target": "t1", "table_op": { "kind": "set_cell_text", "row_index": 2, "col_index": 0, "text": "NEW0" } },
        { "op": "table_op", "target": "t1", "table_op": { "kind": "set_cell_text", "row_index": 2, "col_index": 1, "text": "NEW1" } }
      ],
      "revision": { "author": "Counsel" }
    }"#;
    let txn = translate(json);
    let edited = apply_transaction(&doc, &txn)
        .expect("tracked insert_row (blank) + set_cell_text on the SAME new row, in one transaction, must succeed")
        .0;

    let mut accepted = edited.clone();
    accept_all(&mut accepted);
    assert_eq!(
        cell_texts(&accepted, "t1")[2],
        vec!["NEW0", "NEW1"],
        "accept_all yields the same final content as the atomic insert_row+cells form"
    );

    let mut rejected = edited.clone();
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        cell_texts(&rejected, "t1"),
        vec![vec!["r0c0", "r0c1"], vec!["r1c0", "r1c1"]],
        "reject_all still restores exactly the base grid"
    );

    // No double-tracking: the new ROW is Inserted (a whole-row insertion is the
    // row-level `w:trPr/w:ins`; the cells carry no per-cell marker), and the text
    // WRITTEN into a cell by set_cell_text carries no INNER tracked-insert layer
    // of its own — the enclosing row's Inserted status already covers it.
    let tbl = find_table(&edited, "t1");
    assert!(
        matches!(
            tbl.rows[2].tracking_status,
            Some(TrackingStatus::Inserted(_))
        ),
        "inserted row carries the row-level marker, got {:?}",
        tbl.rows[2].tracking_status
    );
    let cell = &tbl.rows[2].cells[0];
    assert!(
        cell.tracking_status.is_none(),
        "a wholly-inserted row's cell carries no cellIns, got {:?}",
        cell.tracking_status
    );
    for block in &cell.blocks {
        if let BlockNode::Paragraph(p) = block {
            for seg in &p.segments {
                assert!(
                    matches!(seg.status, TrackingStatus::Normal),
                    "writing into your own pending insertion must not add a second tracked \
                     layer inside an already-Inserted cell, got {:?}",
                    seg.status
                );
            }
        }
    }
}

#[test]
fn set_cell_text_on_a_pending_insert_from_an_earlier_transaction_is_still_refused() {
    // The "own pending insert" allowance is scoped to THIS transaction. A row
    // inserted by an EARLIER, separate `apply_transaction` call (still
    // pending, not yet accepted/rejected) is a FOREIGN pending change from
    // the point of view of a later transaction, exactly like an
    // imported/native Word tracked insert — it must still be refused.
    let doc = grid_2x2();
    let txn1 = translate(&table_op_json(
        "t1",
        r#"{ "kind": "insert_row", "ref_row": 1, "position": "after" }"#,
    ));
    let after_txn1 = apply_transaction(&doc, &txn1).expect("insert row").0;

    let txn2 = translate(&table_op_json(
        "t1",
        r#"{ "kind": "set_cell_text", "row_index": 2, "col_index": 0, "text": "NEW0" }"#,
    ));
    let err = apply_transaction(&after_txn1, &txn2).unwrap_err();
    assert!(
        format!("{err:?}").contains("TableCellNotEditable"),
        "a pending insert from an EARLIER transaction is foreign, not this transaction's own \
         pending insert; expected TableCellNotEditable, got {err:?}"
    );
}

#[test]
fn set_cell_text_on_foreign_pending_insert_names_actionable_options() {
    // A cell already carrying a pre-existing (e.g. imported) pending tracked
    // insert must still be refused, and the refusal must name the actual
    // recovery options rather than the old vague "resolve it first".
    let mut doc = grid_2x2();
    {
        let nid = NodeId::from("t1");
        let tbl = doc
            .blocks
            .iter_mut()
            .find_map(|tb| match &mut tb.block {
                BlockNode::Table(t) if t.id == nid => Some(t),
                _ => None,
            })
            .expect("table");
        tbl.rows[1].cells[0].tracking_status = Some(TrackingStatus::Inserted(RevisionInfo {
            revision_id: 0,
            identity: 0,
            author: Some("Other Author".to_string()),
            date: None,
            apply_op_id: None,
        }));
    }
    let txn = translate(&table_op_json(
        "t1",
        r#"{ "kind": "set_cell_text", "row_index": 1, "col_index": 0, "text": "x" }"#,
    ));
    let err = apply_transaction(&doc, &txn).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.to_lowercase().contains("revision")
            && (msg.to_lowercase().contains("accept") || msg.to_lowercase().contains("reject")),
        "error must name accept/reject-the-revision as an option, got: {msg}"
    );
    assert!(
        msg.contains("t1_r1c0_p"),
        "error should point at the cell paragraph's block id as the alternative \
         (a tracked replace on that block), got: {msg}"
    );
}

// ─── Fail-loud at the wire edge ──────────────────────────────────────────────

#[test]
fn unknown_position_refused_at_adapter() {
    let err = try_translate(&table_op_json(
        "t1",
        r#"{ "kind": "insert_row", "ref_row": 0, "position": "sideways" }"#,
    ))
    .unwrap_err();
    assert!(
        err.contains("before") && err.contains("after"),
        "unknown position must be refused at the wire edge: {err}"
    );
}

#[test]
fn table_op_on_non_table_refused() {
    let doc = grid_2x2();
    // "body" is the paragraph block id.
    let txn = translate(&table_op_json(
        "body",
        r#"{ "kind": "delete_row", "row_index": 0 }"#,
    ));
    let err = apply_transaction(&doc, &txn).unwrap_err();
    assert!(
        format!("{err:?}").contains("NotATable"),
        "expected NotATable, got {err:?}"
    );
}
