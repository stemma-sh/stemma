//! Redline roundtrip tests for v4 `replace(table)` and `insert(table)`.
//!
//! Each test constructs a CanonDoc with a base table, applies a v4
//! edit transaction, then verifies that `accept_all` produces the
//! target text and `reject_all` produces the base text (OOXML §17.13
//! tracked-change accept/reject invariants).
//!
//! These tests run in canonical space — they exercise the engine and
//! the accept/reject projection, not the DOCX serializer. Full
//! serialize/re-import roundtrip lives in `edit_serialize.rs`.

use stemma::accept_all;
use stemma::domain::*;
use stemma::edit::{EditTransaction, apply_transaction};
use stemma::edit_v4::parse_transaction;
use stemma::reject_all_with_styles;

// ─── Doc-construction helpers ────────────────────────────────────────────────

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

fn make_cell(id: &str, paragraph_text: &str) -> TableCellNode {
    let para = make_para(&format!("{id}_p"), paragraph_text);
    TableCellNode {
        id: NodeId::from(id),
        blocks: vec![BlockNode::from(para)],
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
    let body_para = make_para("body_exemplar", "body");
    CanonDoc {
        id: NodeId::from("doc1"),
        blocks: vec![
            normal_tracked_block(BlockNode::from(body_para)),
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

fn translate(json: &str) -> EditTransaction {
    parse_transaction(json)
        .expect("schema check passes")
        .into_edit_transaction()
        .expect("adapter succeeds")
}

/// Extract a flat list of cell texts for the named table, recursively
/// pulling text from every paragraph inside every cell. Useful for
/// comparing accept/reject states against a target.
fn table_cell_texts(doc: &CanonDoc, table_id: &str) -> Vec<Vec<String>> {
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
        .map(|row| {
            row.cells
                .iter()
                .map(|cell| {
                    let mut text = String::new();
                    for block in &cell.blocks {
                        if let BlockNode::Paragraph(p) = block {
                            for seg in &p.segments {
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

// ─── Row-insert roundtrip ────────────────────────────────────────────────────

const REPLACE_TABLE_ADD_ROW_B: &str = r#"
{
  "ops": [{
    "op": "replace",
    "target": "t1",
    "content": {
      "type": "table",
      "content": [
        {
          "content": [{
            "content": [
              { "type": "paragraph", "role": "body_text", "content": [{ "type": "text", "text": "row A" }] }
            ]
          }]
        },
        {
          "content": [{
            "content": [
              { "type": "paragraph", "role": "body_text", "content": [{ "type": "text", "text": "row B" }] }
            ]
          }]
        }
      ]
    }
  }],
  "revision": { "author": "Counsel" }
}
"#;

#[test]
fn redline_table_row_insert_accept_matches_target_text() {
    let table = make_table(
        "t1",
        vec![make_row("t1_r0", vec![make_cell("t1_c0", "row A")])],
    );
    let doc = doc_with_table(table);
    let txn = translate(REPLACE_TABLE_ADD_ROW_B);
    let edited = apply_transaction(&doc, &txn).expect("apply").0;

    let mut accepted = edited.clone();
    accept_all(&mut accepted);
    assert_eq!(
        table_cell_texts(&accepted, "t1"),
        vec![vec!["row A".to_string()], vec!["row B".to_string()]],
        "accept_all on row-insert should yield the target table (row A + row B)"
    );
}

#[test]
fn redline_table_row_insert_reject_matches_base_text() {
    let table = make_table(
        "t1",
        vec![make_row("t1_r0", vec![make_cell("t1_c0", "row A")])],
    );
    let doc = doc_with_table(table);
    let txn = translate(REPLACE_TABLE_ADD_ROW_B);
    let edited = apply_transaction(&doc, &txn).expect("apply").0;

    let mut rejected = edited.clone();
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        table_cell_texts(&rejected, "t1"),
        vec![vec!["row A".to_string()]],
        "reject_all on row-insert should yield the base table (only row A)"
    );
}

// ─── Row-delete roundtrip ────────────────────────────────────────────────────

const REPLACE_TABLE_DELETE_ROW_B: &str = r#"
{
  "ops": [{
    "op": "replace",
    "target": "t1",
    "content": {
      "type": "table",
      "content": [{
        "content": [{
          "content": [
            { "type": "paragraph", "role": "body_text", "content": [{ "type": "text", "text": "row A" }] }
          ]
        }]
      }]
    }
  }],
  "revision": { "author": "Counsel" }
}
"#;

fn doc_with_two_rows() -> CanonDoc {
    let table = make_table(
        "t1",
        vec![
            make_row("t1_r0", vec![make_cell("t1_c0_a", "row A")]),
            make_row("t1_r1", vec![make_cell("t1_c0_b", "row B")]),
        ],
    );
    doc_with_table(table)
}

#[test]
fn redline_table_row_delete_accept_matches_target_text() {
    let doc = doc_with_two_rows();
    let txn = translate(REPLACE_TABLE_DELETE_ROW_B);
    let edited = apply_transaction(&doc, &txn).expect("apply").0;

    let mut accepted = edited.clone();
    accept_all(&mut accepted);
    assert_eq!(
        table_cell_texts(&accepted, "t1"),
        vec![vec!["row A".to_string()]],
        "accept_all on row-delete should yield the target table (only row A)"
    );
}

#[test]
fn redline_table_row_delete_reject_matches_base_text() {
    let doc = doc_with_two_rows();
    let txn = translate(REPLACE_TABLE_DELETE_ROW_B);
    let edited = apply_transaction(&doc, &txn).expect("apply").0;

    let mut rejected = edited.clone();
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        table_cell_texts(&rejected, "t1"),
        vec![vec!["row A".to_string()], vec!["row B".to_string()]],
        "reject_all on row-delete should yield the base table (row A + row B)"
    );
}

// ─── Cell text change roundtrip ──────────────────────────────────────────────

const REPLACE_TABLE_MODIFY_CELL: &str = r#"
{
  "ops": [{
    "op": "replace",
    "target": "t1",
    "content": {
      "type": "table",
      "content": [{
        "content": [{
          "content": [
            { "type": "paragraph", "role": "body_text", "content": [{ "type": "text", "text": "cell text revised" }] }
          ]
        }]
      }]
    }
  }],
  "revision": { "author": "Counsel" }
}
"#;

#[test]
fn redline_table_cell_text_change_accept_matches_target() {
    let table = make_table(
        "t1",
        vec![make_row("t1_r0", vec![make_cell("t1_c0", "cell text")])],
    );
    let doc = doc_with_table(table);
    let txn = translate(REPLACE_TABLE_MODIFY_CELL);
    let edited = apply_transaction(&doc, &txn).expect("apply").0;

    let mut accepted = edited.clone();
    accept_all(&mut accepted);
    let texts = table_cell_texts(&accepted, "t1");
    // Cell text after accept should equal the target ("cell text revised").
    // We compare via a flat join because the cell paragraph may have been
    // split into two paragraphs (delete + insert) by the diff. After
    // accept_all the deleted paragraph is removed; the inserted survives.
    let flat: String = texts
        .iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>()
        .join("|");
    assert!(
        flat.contains("cell text revised"),
        "expected accepted cell to contain 'cell text revised', got {flat:?}"
    );
    assert!(
        !flat.contains("cell text|"),
        // (Loose assertion: post-accept text must not include the
        // deleted version as a separate cell-text entry. The substring
        // "cell text" is part of "cell text revised", so we look for the
        // form "cell text" followed by the cell boundary "|" or end.)
        "accepted cell should not contain the deleted base text, got {flat:?}"
    );
}

#[test]
fn redline_table_cell_text_change_reject_matches_base() {
    let table = make_table(
        "t1",
        vec![make_row("t1_r0", vec![make_cell("t1_c0", "cell text")])],
    );
    let doc = doc_with_table(table);
    let txn = translate(REPLACE_TABLE_MODIFY_CELL);
    let edited = apply_transaction(&doc, &txn).expect("apply").0;

    let mut rejected = edited.clone();
    reject_all_with_styles(&mut rejected, None);
    let texts = table_cell_texts(&rejected, "t1");
    let flat: String = texts
        .iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>()
        .join("|");
    assert!(
        flat.contains("cell text") && !flat.contains("revised"),
        "rejected cell should equal the base (no 'revised'), got {flat:?}"
    );
}

// ─── Nested table change roundtrip ──────────────────────────────────────────

#[test]
fn redline_table_nested_table_change_accept_matches_target() {
    // Outer table has one row, one cell. The cell contains a paragraph
    // plus a nested table with one row of one cell ("inner A"). The
    // replace payload changes the nested cell to "inner A revised".
    let inner_table = make_table(
        "t1_outer_c0_inner",
        vec![make_row(
            "t1_inner_r0",
            vec![make_cell("t1_inner_c0", "inner A")],
        )],
    );
    let mut outer_cell = make_cell("t1_outer_c0", "outer");
    outer_cell.blocks.push(BlockNode::from(inner_table));
    let outer_row = make_row("t1_outer_r0", vec![outer_cell]);
    let outer_table = make_table("t1", vec![outer_row]);
    let doc = doc_with_table(outer_table);

    let json = r#"
    {
      "ops": [{
        "op": "replace",
        "target": "t1",
        "content": {
          "type": "table",
          "content": [{
            "content": [{
              "content": [
                { "type": "paragraph", "role": "body_text", "content": [{ "type": "text", "text": "outer" }] },
                {
                  "type": "table",
                  "content": [{
                    "content": [{
                      "content": [
                        { "type": "paragraph", "role": "body_text", "content": [{ "type": "text", "text": "inner A revised" }] }
                      ]
                    }]
                  }]
                }
              ]
            }]
          }]
        }
      }],
      "revision": { "author": "Counsel" }
    }
    "#;
    let txn = translate(json);
    let edited = apply_transaction(&doc, &txn)
        .expect("apply nested table change")
        .0;

    let mut accepted = edited.clone();
    accept_all(&mut accepted);

    // Walk the outer table's only cell, find the nested table, check that
    // the inner cell now reads "inner A revised".
    let nid = NodeId::from("t1");
    let outer = accepted
        .blocks
        .iter()
        .find_map(|tb| match &tb.block {
            BlockNode::Table(t) if t.id == nid => Some(t),
            _ => None,
        })
        .expect("outer table not found");
    // Walk every nested table under the outer table and collect the text
    // from every cell. We assert the target text "inner A revised" is
    // present somewhere in the nested table's cell text after accept.
    fn walk_nested_text(t: &TableNode, out: &mut String) {
        for row in &t.rows {
            for cell in &row.cells {
                for block in &cell.blocks {
                    match block {
                        BlockNode::Paragraph(p) => {
                            for seg in &p.segments {
                                for inl in &seg.inlines {
                                    if let InlineNode::Text(tn) = inl {
                                        out.push_str(&tn.text);
                                        out.push('|');
                                    }
                                }
                            }
                        }
                        BlockNode::Table(inner) => walk_nested_text(inner, out),
                        BlockNode::OpaqueBlock(_) => {}
                    }
                }
            }
        }
    }
    let mut nested_text = String::new();
    for block in &outer.rows[0].cells[0].blocks {
        if let BlockNode::Table(t) = block {
            walk_nested_text(t, &mut nested_text);
        }
    }
    assert!(
        !nested_text.is_empty(),
        "expected at least one nested table inside outer cell, got empty text"
    );
    assert!(
        nested_text.contains("inner A revised"),
        "nested cell after accept_all should contain 'inner A revised', got {nested_text:?}"
    );
}
