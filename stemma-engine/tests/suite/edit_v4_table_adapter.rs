//! Adapter-layer tests for v4 `replace(table)` and `insert(table)` ops.
//!
//! Covers two distinct boundaries:
//!
//! 1. Schema layer — `parse_transaction` must reject structurally
//!    impossible tables (empty rows, empty cells, empty cell content)
//!    before they reach the engine.
//! 2. Adapter layer — `into_edit_transaction` must route well-formed
//!    payloads to the right engine step (`ReplaceTable`,
//!    `InsertParagraphs` carrying a `BlockSpec::Table`).
//!
//! Document-level fail-fast checks (merged cells, header rows, base table
//! formatting) live in the engine — covered separately via apply tests
//! that construct CanonDocs with the offending shapes.

use stemma::domain::*;
use stemma::edit::{
    BlockSpec, EditError, EditStep, EditTransaction, MaterializationMode, apply_transaction,
};
use stemma::edit_v4::*;

// ─── Doc-construction helpers ────────────────────────────────────────────────
//
// Minimal table builders. Each test constructs a doc containing a single
// 1×1 table by default; richer shapes are inlined per-test for clarity.

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
    // structure_hash left empty here — the engine's apply path recomputes
    // it on the freshly resolved target; the base table only needs an id
    // to be discoverable.
    TableNode {
        id: NodeId::from(id),
        rows,
        structure_hash: String::new(),
        formatting: TableFormatting::default(),
        formatting_change: None,
    }
}

fn doc_with_table(table: TableNode) -> CanonDoc {
    // Include a top-level paragraph alongside the table so the document
    // vocabulary has a "body_text" exemplar to resolve against when the
    // engine builds the target table's cell paragraphs.
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

fn find_table_by_id<'a>(doc: &'a CanonDoc, id: &str) -> &'a TrackedBlock {
    let nid = NodeId::from(id);
    doc.blocks
        .iter()
        .find(|tb| match &tb.block {
            BlockNode::Table(t) => t.id == nid,
            _ => false,
        })
        .expect("table not found")
}

// One-row table with a single cell containing the text "cell text".
fn simple_table_doc(table_id: &str) -> CanonDoc {
    let cell = make_cell(&format!("{table_id}_c0"), "cell text");
    let row = make_row(&format!("{table_id}_r0"), vec![cell]);
    let table = make_table(table_id, vec![row]);
    doc_with_table(table)
}

fn translate(json: &str) -> EditTransaction {
    parse_transaction(json)
        .expect("schema check passes")
        .into_edit_transaction()
        .expect("adapter succeeds")
}

// ─── Schema-layer tests ──────────────────────────────────────────────────────

#[test]
fn parse_rejects_empty_table_rows() {
    let json = r#"
    {
      "ops": [{
        "op": "replace",
        "target": "t1",
        "content": { "type": "table", "content": [] }
      }],
      "revision": { "author": "Counsel" }
    }
    "#;
    let err = parse_transaction(json).expect_err("empty rows must fail");
    assert!(
        matches!(err, SchemaError::EmptyTableRows { .. }),
        "expected EmptyTableRows, got {err:?}"
    );
}

#[test]
fn parse_rejects_empty_row_cells() {
    let json = r#"
    {
      "ops": [{
        "op": "replace",
        "target": "t1",
        "content": {
          "type": "table",
          "content": [{ "content": [] }]
        }
      }],
      "revision": { "author": "Counsel" }
    }
    "#;
    let err = parse_transaction(json).expect_err("empty row cells must fail");
    assert!(
        matches!(err, SchemaError::EmptyTableRowCells { .. }),
        "expected EmptyTableRowCells, got {err:?}"
    );
}

#[test]
fn parse_rejects_empty_cell_content() {
    let json = r#"
    {
      "ops": [{
        "op": "replace",
        "target": "t1",
        "content": {
          "type": "table",
          "content": [{ "content": [{ "content": [] }] }]
        }
      }],
      "revision": { "author": "Counsel" }
    }
    "#;
    let err = parse_transaction(json).expect_err("empty cell content must fail");
    assert!(
        matches!(err, SchemaError::EmptyTableCellBlocks { .. }),
        "expected EmptyTableCellBlocks, got {err:?}"
    );
}

#[test]
fn parse_accepts_nested_table_inside_cell() {
    // Cell.content is `Vec<Block>`, so a cell can carry a paragraph plus
    // a nested table. Proves the recursion shape: the schema admits it,
    // the adapter translates it, and the engine receives a
    // `BlockSpec::Table` containing a `BlockSpec::Table` inside one cell.
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
                { "type": "paragraph", "content": [{ "type": "text", "text": "outer cell" }] },
                {
                  "type": "table",
                  "content": [{
                    "content": [{
                      "content": [
                        { "type": "paragraph", "content": [{ "type": "text", "text": "nested" }] }
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
    let EditStep::ReplaceTable { replacement, .. } = &txn.steps[0] else {
        panic!("expected ReplaceTable");
    };
    let outer_cell = &replacement.rows[0].cells[0];
    assert_eq!(
        outer_cell.content.len(),
        2,
        "outer cell should hold paragraph + nested table"
    );
    assert!(matches!(outer_cell.content[0], BlockSpec::Paragraph(_)));
    let BlockSpec::Table(nested) = &outer_cell.content[1] else {
        panic!("expected nested BlockSpec::Table at cell index 1");
    };
    assert_eq!(nested.rows.len(), 1);
    assert_eq!(nested.rows[0].cells.len(), 1);
}

// ─── Adapter-layer tests ─────────────────────────────────────────────────────

#[test]
fn adapter_routes_replace_table_to_replace_table_step() {
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
                { "type": "paragraph", "content": [{ "type": "text", "text": "new cell" }] }
              ]
            }]
          }]
        }
      }],
      "revision": { "author": "Counsel" }
    }
    "#;
    let txn = translate(json);
    let EditStep::ReplaceTable {
        block_id,
        replacement,
        ..
    } = &txn.steps[0]
    else {
        panic!("expected ReplaceTable step");
    };
    assert_eq!(block_id.0.as_ref(), "t1");
    assert_eq!(replacement.rows.len(), 1);
    assert_eq!(replacement.rows[0].cells.len(), 1);
    assert_eq!(replacement.rows[0].cells[0].content.len(), 1);
}

#[test]
fn adapter_routes_insert_table_to_insert_step() {
    let json = r#"
    {
      "ops": [{
        "op": "insert",
        "target": { "anchor": "p1", "position": "after" },
        "content": [{
          "type": "table",
          "content": [{
            "content": [{
              "content": [
                { "type": "paragraph", "content": [{ "type": "text", "text": "inserted cell" }] }
              ]
            }]
          }]
        }]
      }],
      "revision": { "author": "Counsel" }
    }
    "#;
    let txn = translate(json);
    let EditStep::InsertParagraphs {
        anchor_block_id,
        blocks,
        ..
    } = &txn.steps[0]
    else {
        panic!("expected InsertParagraphs");
    };
    assert_eq!(anchor_block_id.0.as_ref(), "p1");
    assert_eq!(blocks.len(), 1);
    let BlockSpec::Table(spec) = &blocks[0] else {
        panic!("expected BlockSpec::Table in insert payload");
    };
    assert_eq!(spec.rows.len(), 1);
}

// ─── Engine fail-fast tests on the base table ────────────────────────────────
//
// These verify that the engine refuses to operate on tables whose shape the
// v4 grammar cannot faithfully round-trip (merged cells, header rows,
// non-default formatting). Each test constructs a base CanonDoc with the
// offending shape, points a `replace(table)` at it, and asserts the engine
// rejects with the right error variant.

fn replace_table_t1_with_single_cell_payload(text: &str) -> EditTransaction {
    let json = format!(
        r#"
    {{
      "ops": [{{
        "op": "replace",
        "target": "t1",
        "content": {{
          "type": "table",
          "content": [{{ "content": [{{ "content": [
            {{ "type": "paragraph", "role": "body_text", "content": [{{ "type": "text", "text": "{text}" }}] }}
          ] }}] }}]
        }}
      }}],
      "revision": {{ "author": "Counsel" }}
    }}
    "#
    );
    translate(&json)
}

// NOTE (tables-merged + RFC-0003): the former `adapter_rejects_replace_table_*`
// tests for merged cells / vMerge / header rows AND for table/cell formatting in
// the BASE encoded the pre-lift contract, where those shapes were refused. After
// the `tables-merged` lift the v4 grammar expresses gridSpan/vMerge/tblHeader,
// and after RFC-0003 a `replace(table)` CARRIES the base's table/row/cell
// formatting onto the resolved target (`carry_base_formatting_onto_target`)
// instead of dropping it. So a formatted base is now ACCEPTED and its formatting
// preserved (see `adapter_replace_table_preserves_*_formatting_in_base` below).
// The merge-grid refusals (`RaggedTableGrid`, `OrphanVMergeContinue`) target the
// REPLACEMENT spec, exercised in `tables_merged`'s own unit tests.

#[test]
fn adapter_accepts_replace_table_with_merged_cells_in_base() {
    // Two cells, second has grid_span=2 (horizontal merge). Logical width 3.
    let mut cell1 = make_cell("t1_c0", "cell A");
    let mut cell2 = make_cell("t1_c1", "cell B");
    cell2.grid_span = 2;
    cell1.grid_span = 1;
    let row = make_row("t1_r0", vec![cell1, cell2]);
    let table = make_table("t1", vec![row]);
    let doc = doc_with_table(table);

    let txn = replace_table_t1_with_single_cell_payload("rewrite");
    apply_transaction(&doc, &txn)
        .expect("merged cells in base are now expressible and must be accepted");
}

#[test]
fn adapter_accepts_replace_table_with_vmerge_in_base() {
    // Two-row table whose first column is a vertical merge (restart/continue).
    let mut top = make_cell("t1_r0_c0", "cell A");
    top.v_merge = VerticalMerge::Restart;
    let mut bottom = make_cell("t1_r1_c0", "cell A");
    bottom.v_merge = VerticalMerge::Continue;
    let row0 = make_row("t1_r0", vec![top]);
    let row1 = make_row("t1_r1", vec![bottom]);
    let table = make_table("t1", vec![row0, row1]);
    let doc = doc_with_table(table);

    let txn = replace_table_t1_with_single_cell_payload("rewrite");
    apply_transaction(&doc, &txn).expect("vMerge in base is now expressible and must be accepted");
}

#[test]
fn adapter_accepts_replace_table_with_header_rows_in_base() {
    let cell = make_cell("t1_c0", "header text");
    let mut row = make_row("t1_r0", vec![cell]);
    row.is_header = true;
    let table = make_table("t1", vec![row]);
    let doc = doc_with_table(table);

    let txn = replace_table_t1_with_single_cell_payload("rewrite");
    apply_transaction(&doc, &txn)
        .expect("header rows in base are now expressible and must be accepted");
}

#[test]
fn adapter_replace_table_preserves_table_formatting_in_base() {
    // RFC-0003: a formatted base is no longer refused; the replace carries the
    // base's table-level formatting (tblStyle) onto the resolved target.
    let cell = make_cell("t1_c0", "cell text");
    let row = make_row("t1_r0", vec![cell]);
    let mut table = make_table("t1", vec![row]);
    table.formatting.style_id = Some("TableGrid".into());
    let doc = doc_with_table(table);

    let txn = replace_table_t1_with_single_cell_payload("rewrite");
    let new_doc = apply_transaction(&doc, &txn)
        .expect("a formatted base must be accepted and its formatting carried (RFC-0003)")
        .0;
    let BlockNode::Table(tbl) = &find_table_by_id(&new_doc, "t1").block else {
        panic!("t1 should still be a table");
    };
    assert_eq!(
        tbl.formatting.style_id.as_deref(),
        Some("TableGrid"),
        "tblStyle must be preserved across the replace"
    );
}

#[test]
fn adapter_replace_table_preserves_cell_formatting_in_base() {
    // RFC-0003: same-shape replace carries the base cell's tcPr (width) onto the
    // matched target cell.
    let mut cell = make_cell("t1_c0", "cell text");
    cell.formatting.width = Some(TableMeasurement {
        width_type: WidthType::Dxa,
        w: 1440,
        pct_literal: None,
    });
    let row = make_row("t1_r0", vec![cell]);
    let table = make_table("t1", vec![row]);
    let doc = doc_with_table(table);

    let txn = replace_table_t1_with_single_cell_payload("rewrite");
    let new_doc = apply_transaction(&doc, &txn)
        .expect("cell formatting in base must be accepted and carried (RFC-0003)")
        .0;
    let BlockNode::Table(tbl) = &find_table_by_id(&new_doc, "t1").block else {
        panic!("t1 should still be a table");
    };
    let matched_cell = &tbl.rows[0].cells[0];
    assert_eq!(
        matched_cell.formatting.width.as_ref().map(|w| w.w),
        Some(1440),
        "matched cell tcW must be preserved across the replace"
    );
}

#[test]
fn adapter_rejects_replace_table_when_target_is_paragraph() {
    // Target id resolves to a paragraph, not a table — NotATable.
    let para = make_para("p1", "I am a paragraph");
    let doc = CanonDoc {
        id: NodeId::from("doc1"),
        blocks: vec![normal_tracked_block(BlockNode::from(para))],
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
    };
    let json = r#"
    {
      "ops": [{
        "op": "replace",
        "target": "p1",
        "content": {
          "type": "table",
          "content": [{ "content": [{ "content": [
            { "type": "paragraph", "content": [{ "type": "text", "text": "x" }] }
          ] }] }]
        }
      }],
      "revision": { "author": "Counsel" }
    }
    "#;
    let txn = translate(json);
    let err = apply_transaction(&doc, &txn).expect_err("paragraph target must fail NotATable");
    assert!(
        matches!(err, EditError::NotATable { .. }),
        "expected NotATable, got {err:?}"
    );
}

// ─── Happy-path replace test ────────────────────────────────────────────────

#[test]
fn replace_table_modifies_cell_text_with_inline_tracked_changes() {
    // Base table: one row, one cell, paragraph "cell text".
    // Replace with a payload whose cell paragraph says "modified text".
    // The engine should:
    //  - keep the row Matched (same structure)
    //  - mark the cell as Modified
    //  - produce inline ins/del segments inside the cell paragraph
    let doc = simple_table_doc("t1");
    // Small delta keeps the row alignment Matched (high row_cell_similarity)
    // so the cell change shows up as Modified with inline tracked changes,
    // not as row Delete + row Insert.
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
                { "type": "paragraph", "role": "body_text", "content": [{ "type": "text", "text": "cell text revised" }] }
              ]
            }]
          }]
        }
      }],
      "revision": { "author": "Counsel" }
    }
    "#;
    let txn = translate(json);
    let new_doc = apply_transaction(&doc, &txn)
        .expect("replace_table happy path")
        .0;

    // After: same number of top-level blocks, still a table at t1.
    // Index 0 = body exemplar paragraph, index 1 = our table.
    assert_eq!(new_doc.blocks.len(), 2);
    let BlockNode::Table(tbl) = &find_table_by_id(&new_doc, "t1").block else {
        panic!("expected table block");
    };
    assert_eq!(
        tbl.rows.len(),
        1,
        "small-delta cell change should keep the row Matched; got {} rows",
        tbl.rows.len()
    );
    assert_eq!(tbl.rows[0].cells.len(), 1);
    let cell = &tbl.rows[0].cells[0];
    // Cell may carry one or two blocks depending on how the diff
    // classified the paragraph change (inline-merged into one paragraph
    // with tracked segments, or split into a Deleted paragraph + an
    // Inserted paragraph). Both shapes are valid per OOXML §17.13 — the
    // tracked-change information is identical when serialized. Check
    // across all blocks.
    let mut has_deleted_cell_text = false;
    let mut has_inserted_revised = false;
    for block in &cell.blocks {
        if let BlockNode::Paragraph(para) = block {
            for seg in &para.segments {
                let text: String = seg
                    .inlines
                    .iter()
                    .filter_map(|i| match i {
                        InlineNode::Text(t) => Some(t.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                match seg.status {
                    TrackingStatus::Deleted(_) if text.contains("cell text") => {
                        has_deleted_cell_text = true;
                    }
                    TrackingStatus::Inserted(_) if text.contains("revised") => {
                        has_inserted_revised = true;
                    }
                    _ => {}
                }
            }
        }
    }
    assert!(
        has_deleted_cell_text,
        "expected a Deleted segment containing 'cell text' somewhere in the cell, cell blocks: {:?}",
        cell.blocks
    );
    assert!(
        has_inserted_revised,
        "expected an Inserted segment containing 'revised' somewhere in the cell, cell blocks: {:?}",
        cell.blocks
    );
}

#[test]
fn replace_table_identity_is_a_noop() {
    // Identity replace (target matches base): no tracked changes should
    // be emitted (I9 — invariant: edits with no net effect are no-ops).
    let doc = simple_table_doc("t1");
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
                { "type": "paragraph", "role": "body_text", "content": [{ "type": "text", "text": "cell text" }] }
              ]
            }]
          }]
        }
      }],
      "revision": { "author": "Counsel" }
    }
    "#;
    let txn = translate(json);
    let new_doc = apply_transaction(&doc, &txn).expect("identity replace").0;

    // Compare structurally that nothing got marked Deleted or Inserted.
    let BlockNode::Table(tbl) = &find_table_by_id(&new_doc, "t1").block else {
        panic!("expected table block");
    };
    for row in &tbl.rows {
        assert!(
            row.tracking_status.is_none(),
            "row should not have tracking status after identity replace"
        );
        for cell in &row.cells {
            assert!(
                cell.tracking_status.is_none(),
                "cell should not have tracking status after identity replace"
            );
            for block in &cell.blocks {
                if let BlockNode::Paragraph(p) = block {
                    for seg in &p.segments {
                        assert!(
                            matches!(seg.status, TrackingStatus::Normal),
                            "all segments should be Normal after identity replace, got {:?}",
                            seg.status
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn replace_table_row_insertion_marks_new_row_inserted() {
    // Base: one row "A". Target: two rows "A", "B".
    let cell_a = make_cell("t1_c0", "row A");
    let row_a = make_row("t1_r0", vec![cell_a]);
    let table = make_table("t1", vec![row_a]);
    let doc = doc_with_table(table);

    let json = r#"
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
    let txn = translate(json);
    let new_doc = apply_transaction(&doc, &txn).expect("row insertion").0;

    let BlockNode::Table(tbl) = &find_table_by_id(&new_doc, "t1").block else {
        panic!("expected table block");
    };
    assert_eq!(tbl.rows.len(), 2, "merged table should have both rows");
    // Row 0 (existing) stays Normal-status (no tracking_status set).
    assert!(tbl.rows[0].tracking_status.is_none());
    // Row 1 (new) must be Inserted.
    assert!(
        matches!(
            tbl.rows[1].tracking_status,
            Some(TrackingStatus::Inserted(_))
        ),
        "inserted row should carry Inserted tracking_status, got {:?}",
        tbl.rows[1].tracking_status
    );
    // The cells of the inserted row carry NO per-cell marker: a whole-row
    // insertion is the row-level `w:trPr/w:ins` alone (Word parity — real Word
    // never emits `w:cellIns` on a wholly-inserted row's cells). The row marker
    // covers them; a redundant per-cell marker would let selective resolution
    // strip a cell out of a still-inserted row (a cell-less `<w:tr>`).
    for cell in &tbl.rows[1].cells {
        assert!(
            cell.tracking_status.is_none(),
            "a wholly-inserted row's cells must carry no cellIns, got {:?}",
            cell.tracking_status
        );
    }
}

#[test]
fn replace_table_row_deletion_marks_old_row_deleted() {
    // Base: two rows "A", "B". Target: one row "A".
    let cell_a = make_cell("t1_c0_a", "row A");
    let row_a = make_row("t1_r0", vec![cell_a]);
    let cell_b = make_cell("t1_c0_b", "row B");
    let row_b = make_row("t1_r1", vec![cell_b]);
    let table = make_table("t1", vec![row_a, row_b]);
    let doc = doc_with_table(table);

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
                { "type": "paragraph", "role": "body_text", "content": [{ "type": "text", "text": "row A" }] }
              ]
            }]
          }]
        }
      }],
      "revision": { "author": "Counsel" }
    }
    "#;
    let txn = translate(json);
    let new_doc = apply_transaction(&doc, &txn).expect("row deletion").0;

    let BlockNode::Table(tbl) = &find_table_by_id(&new_doc, "t1").block else {
        panic!("expected table block");
    };
    assert_eq!(
        tbl.rows.len(),
        2,
        "merged table keeps deleted row visible with tracked-delete markers"
    );
    // Row 0 stays normal.
    assert!(tbl.rows[0].tracking_status.is_none());
    // Row 1 is marked Deleted.
    assert!(
        matches!(
            tbl.rows[1].tracking_status,
            Some(TrackingStatus::Deleted(_))
        ),
        "deleted row should carry Deleted tracking_status, got {:?}",
        tbl.rows[1].tracking_status
    );
}

#[test]
fn replace_table_direct_mode_skips_tracked_changes() {
    // In Direct mode, the replace should swap the table contents without
    // marking any tracked changes.
    let doc = simple_table_doc("t1");
    let mut txn = translate(
        r#"
    {
      "ops": [{
        "op": "replace",
        "target": "t1",
        "content": {
          "type": "table",
          "content": [{
            "content": [{
              "content": [
                { "type": "paragraph", "role": "body_text", "content": [{ "type": "text", "text": "direct text" }] }
              ]
            }]
          }]
        }
      }],
      "revision": { "author": "Counsel" }
    }
    "#,
    );
    txn.materialization_mode = MaterializationMode::Direct;
    let new_doc = apply_transaction(&doc, &txn).expect("direct replace").0;
    let BlockNode::Table(tbl) = &find_table_by_id(&new_doc, "t1").block else {
        panic!("expected table");
    };
    let para = match &tbl.rows[0].cells[0].blocks[0] {
        BlockNode::Paragraph(p) => p,
        _ => panic!("expected paragraph"),
    };
    // Should read "direct text" with no tracked-change shadow.
    for seg in &para.segments {
        assert!(
            matches!(seg.status, TrackingStatus::Normal),
            "direct mode must not leave tracked changes, got {:?}",
            seg.status
        );
    }
}
