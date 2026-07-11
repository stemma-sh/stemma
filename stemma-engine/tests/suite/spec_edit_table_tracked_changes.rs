//! OOXML §17.13 spec compliance tests for v4 `replace(table)`.
//!
//! Verifies that the engine emits the canonical-space `TrackingStatus`
//! markers the serializer translates into `w:trPr/w:ins`,
//! `w:trPr/w:del`, `w:cellIns`, `w:cellDel`, and inline `w:ins`/`w:del`
//! per OOXML §17.13.5.
//!
//! These tests sit in canonical space because:
//!  1. The `TrackingStatus` field is the contract between the engine
//!     and the serializer. The serializer's faithfulness to OOXML is
//!     already verified by the `spec_tables_*` suite that constructs
//!     hand-built canonical docs.
//!  2. End-to-end DOCX serialization requires base/target byte
//!     templates, which means a fixture, which means a slower test
//!     loop. The `edit_serialize.rs` sweep covers the byte-level
//!     roundtrip across all fixtures; here we pin the per-row /
//!     per-cell markers an `EditStep::ReplaceTable` produces.

use stemma::domain::*;
use stemma::edit::{EditTransaction, apply_transaction};
use stemma::edit_v4::parse_transaction;

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

// ─── Row insert produces w:trPr/w:ins (TrackingStatus::Inserted on row) ──────
//
// Spec: ECMA-376 §17.13.5.16 `w:ins` element inside `w:trPr` marks an
// inserted table row. The engine encodes this as
// `TableRowNode.tracking_status = Some(TrackingStatus::Inserted(_))`.
// The serializer (covered by `spec_table_*` suite) translates that field
// to `<w:trPr><w:ins .../></w:trPr>` at the right position in the trPr
// element ordering.

#[test]
fn spec_table_row_insert_marks_tracking_status_inserted() {
    let table = make_table(
        "t1",
        vec![make_row("t1_r0", vec![make_cell("t1_c0", "row A")])],
    );
    let doc = doc_with_table(table);
    let json = r#"
    {
      "ops": [{
        "op": "replace",
        "target": "t1",
        "content": {
          "type": "table",
          "content": [
            { "content": [{ "content": [
              { "type": "paragraph", "role": "body_text", "content": [{ "type": "text", "text": "row A" }] }
            ] }] },
            { "content": [{ "content": [
              { "type": "paragraph", "role": "body_text", "content": [{ "type": "text", "text": "row B" }] }
            ] }] }
          ]
        }
      }],
      "revision": { "author": "Counsel" }
    }
    "#;
    let txn = translate(json);
    let edited = apply_transaction(&doc, &txn).expect("apply").0;
    let tbl = find_table(&edited, "t1");

    assert_eq!(tbl.rows.len(), 2);
    // Row 0 (existing) keeps tracking_status = None.
    assert!(
        tbl.rows[0].tracking_status.is_none(),
        "existing row should not be marked"
    );
    // Row 1 (new) carries Inserted — serializer emits w:trPr/w:ins.
    assert!(
        matches!(
            tbl.rows[1].tracking_status,
            Some(TrackingStatus::Inserted(_))
        ),
        "inserted row must carry TrackingStatus::Inserted (drives w:trPr/w:ins per §17.13.5.16)"
    );
}

// ─── Row delete produces w:trPr/w:del (TrackingStatus::Deleted on row) ───────
//
// Spec: ECMA-376 §17.13.5.13 `w:del` inside `w:trPr` marks a deleted row.

#[test]
fn spec_table_row_delete_marks_tracking_status_deleted() {
    let table = make_table(
        "t1",
        vec![
            make_row("t1_r0", vec![make_cell("t1_c0_a", "row A")]),
            make_row("t1_r1", vec![make_cell("t1_c0_b", "row B")]),
        ],
    );
    let doc = doc_with_table(table);
    let json = r#"
    {
      "ops": [{
        "op": "replace",
        "target": "t1",
        "content": {
          "type": "table",
          "content": [{
            "content": [{ "content": [
              { "type": "paragraph", "role": "body_text", "content": [{ "type": "text", "text": "row A" }] }
            ] }]
          }]
        }
      }],
      "revision": { "author": "Counsel" }
    }
    "#;
    let txn = translate(json);
    let edited = apply_transaction(&doc, &txn).expect("apply").0;
    let tbl = find_table(&edited, "t1");

    assert_eq!(tbl.rows.len(), 2);
    assert!(tbl.rows[0].tracking_status.is_none());
    assert!(
        matches!(
            tbl.rows[1].tracking_status,
            Some(TrackingStatus::Deleted(_))
        ),
        "deleted row must carry TrackingStatus::Deleted (drives w:trPr/w:del per §17.13.5.13)"
    );
}

// ─── A wholly-inserted row carries no per-cell cellIns ──────────────────────
//
// Spec (ECMA-376 §17.13.5): a whole-row insertion is the row-level marker
// `w:trPr/w:ins` (§17.13.5.17) — real Word does NOT emit `w:cellIns`
// (§17.13.5.2) on the cells of a wholly-inserted row (cellIns is for a cell
// inserted WITHIN a surviving row: a column op or a cell merge). Consumers read
// the row marker as authoritative. A redundant per-cell marker also makes an
// invalid state representable: selective resolution of one cell's cellIns would
// strip that cell out of a still-inserted row, yielding a cell-less `<w:tr>`
// (invalid per §17.4.72 CT_Row).

#[test]
fn spec_table_inserted_row_cells_carry_no_cell_marker() {
    let table = make_table(
        "t1",
        vec![make_row("t1_r0", vec![make_cell("t1_c0", "row A")])],
    );
    let doc = doc_with_table(table);
    let json = r#"
    {
      "ops": [{
        "op": "replace",
        "target": "t1",
        "content": {
          "type": "table",
          "content": [
            { "content": [{ "content": [
              { "type": "paragraph", "role": "body_text", "content": [{ "type": "text", "text": "row A" }] }
            ] }] },
            { "content": [{ "content": [
              { "type": "paragraph", "role": "body_text", "content": [{ "type": "text", "text": "row B" }] }
            ] }] }
          ]
        }
      }],
      "revision": { "author": "Counsel" }
    }
    "#;
    let txn = translate(json);
    let edited = apply_transaction(&doc, &txn).expect("apply").0;
    let tbl = find_table(&edited, "t1");

    // The inserted row itself carries the row-level marker …
    assert!(
        matches!(
            tbl.rows[1].tracking_status,
            Some(TrackingStatus::Inserted(_))
        ),
        "inserted row must carry the row-level Inserted marker, got {:?}",
        tbl.rows[1].tracking_status
    );
    // … and its cells carry NO per-cell marker (Word parity).
    for cell in &tbl.rows[1].cells {
        assert!(
            cell.tracking_status.is_none(),
            "a wholly-inserted row's cells must carry no cellIns, got {:?}",
            cell.tracking_status
        );
    }
}

// ─── Matched-row cell text change produces inline tracked changes ───────────
//
// Spec: ECMA-376 §17.13.5.16/§17.13.5.13 — inline-level `w:ins` and
// `w:del` wrap the run-level inserts/deletes inside paragraphs. When a
// cell's row is structurally Matched but text changed, we expect the
// cell's paragraph to carry a mix of Inserted and Deleted tracked
// segments (or, in the cross-paragraph reconcile path, a Deleted paragraph
// followed by an Inserted paragraph).

#[test]
fn spec_table_matched_row_cell_change_produces_inline_tracked_changes() {
    // Two rows so the row signatures differ between rows; we change the
    // content of row 1 only. Row 0 is the same across base and target so
    // the diff matches it; row 1's signature is similar enough to be
    // matched too (cell text differs only by a suffix).
    let table = make_table(
        "t1",
        vec![
            make_row("t1_r0", vec![make_cell("t1_c0_a", "header")]),
            make_row("t1_r1", vec![make_cell("t1_c0_b", "data row text")]),
        ],
    );
    let doc = doc_with_table(table);
    let json = r#"
    {
      "ops": [{
        "op": "replace",
        "target": "t1",
        "content": {
          "type": "table",
          "content": [
            { "content": [{ "content": [
              { "type": "paragraph", "role": "body_text", "content": [{ "type": "text", "text": "header" }] }
            ] }] },
            { "content": [{ "content": [
              { "type": "paragraph", "role": "body_text", "content": [{ "type": "text", "text": "data row text revised" }] }
            ] }] }
          ]
        }
      }],
      "revision": { "author": "Counsel" }
    }
    "#;
    let txn = translate(json);
    let edited = apply_transaction(&doc, &txn).expect("apply").0;
    let tbl = find_table(&edited, "t1");

    // Row count unchanged — both rows Matched.
    assert_eq!(tbl.rows.len(), 2);
    assert!(tbl.rows[0].tracking_status.is_none());
    assert!(tbl.rows[1].tracking_status.is_none());
    // Cells in matched rows must also be unmarked at the row/cell layer;
    // the change rides inside the cell paragraph(s).
    for cell in &tbl.rows[1].cells {
        assert!(
            cell.tracking_status.is_none(),
            "matched-row cell should not carry row-level tracking; tracked content rides inside the cell paragraph"
        );
    }

    // Walk the cell's blocks and confirm there's at least one Deleted
    // and one Inserted somewhere in the cell — that's the inline
    // tracked-change shape OOXML demands.
    let cell = &tbl.rows[1].cells[0];
    let mut has_deleted = false;
    let mut has_inserted = false;
    for block in &cell.blocks {
        if let BlockNode::Paragraph(p) = block {
            // Per-paragraph mark status set by reconcile_cell_blocks
            // when the paragraph is replaced wholesale.
            if matches!(p.para_mark_status, Some(TrackingStatus::Deleted(_))) {
                has_deleted = true;
            }
            if matches!(p.para_mark_status, Some(TrackingStatus::Inserted(_))) {
                has_inserted = true;
            }
            for seg in &p.segments {
                if matches!(seg.status, TrackingStatus::Deleted(_)) {
                    has_deleted = true;
                }
                if matches!(seg.status, TrackingStatus::Inserted(_)) {
                    has_inserted = true;
                }
            }
        }
    }
    assert!(
        has_deleted,
        "expected at least one Deleted tracked segment or paragraph inside the modified cell; got cell.blocks: {:?}",
        cell.blocks
    );
    assert!(
        has_inserted,
        "expected at least one Inserted tracked segment or paragraph inside the modified cell; got cell.blocks: {:?}",
        cell.blocks
    );
}

// ─── Identity replace produces zero tracked changes (I9 invariant) ──────────
//
// Spec invariant I9 (engine): edits with no net effect must produce no
// tracked changes. For tables, that means a `replace(table)` whose target
// structure matches the base must leave the block byte-identical.

#[test]
fn spec_table_identity_replace_emits_no_tracked_changes() {
    let table = make_table(
        "t1",
        vec![make_row("t1_r0", vec![make_cell("t1_c0", "cell text")])],
    );
    let doc = doc_with_table(table);
    let original_table = match &doc.blocks[1].block {
        BlockNode::Table(t) => t.clone(),
        _ => panic!("expected table"),
    };

    let json = r#"
    {
      "ops": [{
        "op": "replace",
        "target": "t1",
        "content": {
          "type": "table",
          "content": [{
            "content": [{ "content": [
              { "type": "paragraph", "role": "body_text", "content": [{ "type": "text", "text": "cell text" }] }
            ] }]
          }]
        }
      }],
      "revision": { "author": "Counsel" }
    }
    "#;
    let txn = translate(json);
    let edited = apply_transaction(&doc, &txn).expect("identity replace").0;
    let tbl = find_table(&edited, "t1");

    // Same row count, no tracking_status anywhere.
    assert_eq!(tbl.rows.len(), original_table.rows.len());
    for row in &tbl.rows {
        assert!(
            row.tracking_status.is_none(),
            "identity replace must leave row tracking_status unset"
        );
        for cell in &row.cells {
            assert!(
                cell.tracking_status.is_none(),
                "identity replace must leave cell tracking_status unset"
            );
            for block in &cell.blocks {
                if let BlockNode::Paragraph(p) = block {
                    assert!(
                        p.para_mark_status.is_none(),
                        "identity replace must leave para_mark_status unset"
                    );
                    for seg in &p.segments {
                        assert!(
                            matches!(seg.status, TrackingStatus::Normal),
                            "identity replace must leave all segments Normal, got {:?}",
                            seg.status
                        );
                    }
                }
            }
        }
    }
}
