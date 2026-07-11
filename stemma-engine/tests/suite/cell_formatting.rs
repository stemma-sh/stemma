//! Integration tests for `set_cell_format` — the EXEMPLAR in-place cell
//! formatting verb (`w:tcPrChange`, §17.13.5.37).
//!
//! Domain rules encoded (OOXML §17.13 accept/reject + formatting preservation):
//!   - `apply` SUCCEEDS on a formatted base WITHOUT routing through the
//!     whole-table replace schema (no `TableHasFormattingNotInSpec`);
//!   - `accept_all` == the cell with the requested formatting applied (the
//!     tcPrChange is dropped, the new tcPr stays);
//!   - `reject_all` == the original cell formatting (the tcPrChange reverts it);
//!   - the change is a tracked `tcPrChange` (`cell.formatting_change`), NOT a
//!     segment ins/del — the cell's TEXT is byte-identical;
//!   - the table's `tblPr`, every OTHER cell, and the target cell's UNtouched
//!     properties are byte-identical before/after;
//!   - the stacking guard refuses a second format on an already-changed cell;
//!   - a no-op patch is refused (`NoCellFormattingRequested`);
//!   - opaque inlines in the cell are preserved (the verb never touches text);
//!   - the logical-grid `{row, col}` address resolves like the read view, and
//!     interior-of-a-span / vMerge-continuation targets fail loud.

use stemma::ExportOptions;
use stemma::accept_all;
use stemma::api::Document;
use stemma::docx::DocxArchive;
use stemma::domain::*;
use stemma::edit::{EditTransaction, apply_transaction};
use stemma::edit_v4::parse_transaction;
use stemma::reject_all_with_styles;

// ─── Doc-construction helpers (mirroring table_set_cell_text.rs) ─────────────

fn text_para(id: &str, text: &str) -> ParagraphNode {
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
        literal_prefix_trailing_rpr: None,
        literal_prefix_marks: Vec::new(),
        literal_prefix_style_props: StyleProps::default(),
        literal_prefix_rpr_authored: stemma::domain::RunRprAuthored::default(),
        literal_prefix_leading_rpr: None,
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

fn yellow_shading() -> Shading {
    Shading {
        fill: Some("FFFF00".to_string()),
        val: Some(ShadingPattern::Clear),
        color: Some("auto".to_string()),
        extra_attrs: Vec::new(),
    }
}

fn single_borders() -> BorderSet {
    let edge = Border {
        style: BorderStyle::Single,
        color: Some("000000".to_string()),
        size: Some(4),
        space: Some(0),
        extra_attrs: Vec::new(),
    };
    BorderSet {
        top: Some(edge.clone()),
        bottom: Some(edge.clone()),
        left: Some(edge.clone()),
        right: Some(edge.clone()),
        inside_h: Some(edge.clone()),
        inside_v: Some(edge),
    }
}

/// A cell carrying NON-DEFAULT formatting (an explicit width) so we can prove
/// the verb byte-preserves the untouched properties while changing others.
fn formatted_cell(id: &str, text: &str) -> TableCellNode {
    TableCellNode {
        id: NodeId::from(id),
        blocks: vec![BlockNode::from(text_para(&format!("{id}_p"), text))],
        grid_span: 1,
        v_merge: VerticalMerge::None,
        formatting: CellFormatting {
            width: Some(TableMeasurement {
                w: 2400,
                width_type: WidthType::Dxa,
                pct_literal: None,
            }),
            ..CellFormatting::default()
        },
        formatting_change: None,
        tracking_status: None,
        row_sdt_wrapper: None,
        content_sdt_wraps: Vec::new(),
        cnf_style: None,
        hide_mark: false,
        preserved: Vec::new(),
    }
}

fn plain_cell(id: &str, text: &str) -> TableCellNode {
    TableCellNode {
        id: NodeId::from(id),
        blocks: vec![BlockNode::from(text_para(&format!("{id}_p"), text))],
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

fn row(id: &str, cells: Vec<TableCellNode>) -> TableRowNode {
    TableRowNode {
        id: NodeId::from(id),
        cells,
        grid_before: 0,
        grid_after: 0,
        tracking_status: None,
        is_header: false,
        height: Some(360),
        height_rule: Some(HeightRule::AtLeast),
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

/// A 2×2 table with table-level borders + per-cell width — the kind of REAL
/// formatted table the whole-table v4-replace guard refuses.
fn formatted_table() -> TableNode {
    TableNode {
        id: NodeId::from("t1"),
        rows: vec![
            row(
                "t1_r0",
                vec![
                    formatted_cell("t1_r0c0", "r0c0"),
                    formatted_cell("t1_r0c1", "r0c1"),
                ],
            ),
            row(
                "t1_r1",
                vec![
                    formatted_cell("t1_r1c0", "r1c0"),
                    formatted_cell("t1_r1c1", "r1c1"),
                ],
            ),
        ],
        structure_hash: String::new(),
        formatting: TableFormatting {
            borders: Some(single_borders()),
            width: Some(TableMeasurement {
                w: 5000,
                width_type: WidthType::Pct,
                pct_literal: None,
            }),
            ..TableFormatting::default()
        },
        formatting_change: None,
    }
}

fn doc_with(table: TableNode) -> CanonDoc {
    CanonDoc {
        id: NodeId::from("doc1"),
        blocks: vec![
            normal_tracked_block(BlockNode::from(text_para("body", "body"))),
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

/// A `set_cell_format` op that shades the target cell yellow.
fn shade_cell_json(target: &str, row: usize, col: usize) -> String {
    format!(
        r#"{{ "ops": [{{ "op": "set_cell_format", "target": "{target}",
            "row_index": {row}, "col_index": {col},
            "shading": {{ "fill": "FFFF00", "pattern": "clear", "color": "auto" }} }}],
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
        .expect("table present")
}

fn cell_text(cell: &TableCellNode) -> String {
    let mut out = String::new();
    for block in &cell.blocks {
        if let BlockNode::Paragraph(p) = block {
            for seg in &p.segments {
                for i in &seg.inlines {
                    if let InlineNode::Text(t) = i {
                        out.push_str(&t.text);
                    }
                }
            }
        }
    }
    out
}

// ─── Core: apply SUCCEEDS on a formatted table, produces a tcPrChange ─────────

#[test]
fn set_cell_format_on_formatted_table_does_not_refuse() {
    let doc = doc_with(formatted_table());
    let txn = translate(&shade_cell_json("t1", 0, 1));
    let edited = apply_transaction(&doc, &txn);
    assert!(
        edited.is_ok(),
        "set_cell_format on a formatted table must NOT route through the \
         whole-table replace guard, got {:?}",
        edited.err()
    );
}

#[test]
fn set_cell_format_records_a_tracked_tcprchange_not_a_segment_change() {
    let base = formatted_table();
    let doc = doc_with(base.clone());
    let txn = translate(&shade_cell_json("t1", 0, 1));
    let edited = apply_transaction(&doc, &txn).expect("apply").0;
    let et = find_table(&edited, "t1");

    // The change is a tracked tcPrChange on the target cell.
    let target = &et.rows[0].cells[1];
    let fc = target
        .formatting_change
        .as_ref()
        .expect("target cell must carry a tcPrChange");
    // Its inner tcPr is the PREVIOUS state: the cell was unshaded before.
    assert_eq!(
        fc.previous_shading, None,
        "tcPrChange inner tcPr must capture the prior (unshaded) state"
    );
    assert_eq!(fc.author, "Counsel");
    // The NEW live state carries the requested shading.
    assert_eq!(target.formatting.shading, Some(yellow_shading()));

    // It is NOT a segment ins/del: the cell's paragraph text is byte-identical
    // (formatting changes never lower to the segment materializer).
    assert_eq!(cell_text(target), "r0c1", "cell text must be unchanged");
    let target_para = match &target.blocks[0] {
        BlockNode::Paragraph(p) => p,
        _ => panic!("cell holds a paragraph"),
    };
    let base_para = match &base.rows[0].cells[1].blocks[0] {
        BlockNode::Paragraph(p) => p,
        _ => panic!(),
    };
    assert_eq!(
        target_para.segments, base_para.segments,
        "no tracked segments — a tcPr change is not a text edit"
    );
}

#[test]
fn set_cell_format_accept_keeps_reject_reverts() {
    let doc = doc_with(formatted_table());
    let txn = translate(&shade_cell_json("t1", 0, 1));
    let edited = apply_transaction(&doc, &txn).expect("apply").0;

    // accept_all => the cell keeps the requested shading; the tcPrChange is gone.
    let mut accepted = edited.clone();
    accept_all(&mut accepted);
    let at = find_table(&accepted, "t1");
    assert_eq!(
        at.rows[0].cells[1].formatting.shading,
        Some(yellow_shading()),
        "accept keeps the new shading"
    );
    assert!(
        at.rows[0].cells[1].formatting_change.is_none(),
        "accept clears the tcPrChange"
    );

    // reject_all => the cell reverts to the original (unshaded) formatting.
    let mut rejected = edited.clone();
    reject_all_with_styles(&mut rejected, None);
    let rt = find_table(&rejected, "t1");
    assert_eq!(
        rt.rows[0].cells[1].formatting.shading, None,
        "reject restores the prior (unshaded) state"
    );
    assert!(
        rt.rows[0].cells[1].formatting_change.is_none(),
        "reject clears the tcPrChange"
    );
}

// ─── Byte-preservation: tblPr + every other cell + untouched props ───────────

#[test]
fn set_cell_format_preserves_tblpr_other_cells_and_untouched_props() {
    let base = formatted_table();
    let doc = doc_with(base.clone());
    let txn = translate(&shade_cell_json("t1", 0, 1));
    let edited = apply_transaction(&doc, &txn).expect("apply").0;
    let et = find_table(&edited, "t1");

    // Table-level formatting (tblPr) is byte-identical.
    assert_eq!(
        et.formatting, base.formatting,
        "tblPr (borders/width) must be byte-preserved"
    );

    // Every row's trPr is byte-identical; every cell EXCEPT the target is a
    // byte-identical whole node.
    for (r, brow) in base.rows.iter().enumerate() {
        assert_eq!(et.rows[r].height, brow.height, "row {r} trPr height");
        assert_eq!(
            et.rows[r].height_rule, brow.height_rule,
            "row {r} trPr rule"
        );
    }
    assert_eq!(
        et.rows[0].cells[0], base.rows[0].cells[0],
        "untouched cell node"
    );
    assert_eq!(
        et.rows[1].cells[0], base.rows[1].cells[0],
        "untouched cell node"
    );
    assert_eq!(
        et.rows[1].cells[1], base.rows[1].cells[1],
        "untouched cell node"
    );

    // The TARGET cell's UNtouched property (its explicit width) is byte-identical;
    // only `shading` (the requested field) and `formatting_change` changed.
    let target = &et.rows[0].cells[1];
    assert_eq!(
        target.formatting.width, base.rows[0].cells[1].formatting.width,
        "target cell's untouched width must be byte-preserved"
    );
    assert_eq!(target.id, base.rows[0].cells[1].id);
    assert_eq!(target.grid_span, base.rows[0].cells[1].grid_span);
    assert_eq!(target.v_merge, base.rows[0].cells[1].v_merge);
}

// ─── No-op + stacking guards ──────────────────────────────────────────────────

#[test]
fn set_cell_format_noop_is_refused() {
    // Requesting exactly the cell's CURRENT formatting (width unchanged) is a
    // no-op — but the schema layer refuses an EMPTY patch first. A patch that
    // sets the SAME width still passes schema (non-empty), then short-circuits
    // in the verb to NO tcPrChange. Assert the short-circuit: no change recorded.
    let base = formatted_table();
    let doc = doc_with(base.clone());
    let txn = translate(
        r#"{ "ops": [{ "op": "set_cell_format", "target": "t1",
            "row_index": 0, "col_index": 1,
            "width": { "w": 2400, "width_type": "dxa" } }],
            "revision": { "author": "Counsel" } }"#,
    );
    let edited = apply_transaction(&doc, &txn).expect("apply").0;
    let et = find_table(&edited, "t1");
    assert!(
        et.rows[0].cells[1].formatting_change.is_none(),
        "setting the current value must short-circuit to NO tcPrChange"
    );
    assert_eq!(
        et.rows[0].cells[1], base.rows[0].cells[1],
        "a no-op set must leave the cell byte-identical"
    );
}

#[test]
fn set_cell_format_empty_patch_refused_at_schema() {
    // An empty set_cell_format (no property at all) is refused at the wire edge.
    let err = parse_transaction(
        r#"{ "ops": [{ "op": "set_cell_format", "target": "t1",
            "row_index": 0, "col_index": 1 }],
            "revision": { "author": "Counsel" } }"#,
    )
    .unwrap_err();
    assert!(
        format!("{err:?}").contains("EmptyCellFormat"),
        "empty set_cell_format must be refused at schema, got {err:?}"
    );
}

#[test]
fn set_cell_format_stacking_on_changed_cell_refused() {
    let doc = doc_with(formatted_table());
    // First format succeeds.
    let txn1 = translate(&shade_cell_json("t1", 0, 1));
    let once = apply_transaction(&doc, &txn1).expect("first apply").0;
    // A second format on the same (already-changed) cell must be refused —
    // accept/reject the first one before formatting again.
    let txn2 = translate(
        r#"{ "ops": [{ "op": "set_cell_format", "target": "t1",
            "row_index": 0, "col_index": 1, "v_align": "center" }],
            "revision": { "author": "Counsel" } }"#,
    );
    let err = apply_transaction(&once, &txn2).unwrap_err();
    assert!(
        format!("{err:?}").contains("TableCellNotEditable"),
        "stacking a second tcPrChange must be refused, got {err:?}"
    );
}

// ─── Addressing: logical grid, out-of-range, vMerge ──────────────────────────

#[test]
fn set_cell_format_addresses_logical_grid_column_across_a_span() {
    // Row 0: a span-2 anchor at logical col 0, then a plain cell at logical col 2.
    let mut wide = formatted_cell("t1_r0c0", "WIDE");
    wide.grid_span = 2;
    let table = TableNode {
        id: NodeId::from("t1"),
        rows: vec![
            row("t1_r0", vec![wide, plain_cell("t1_r0c2", "right")]),
            row(
                "t1_r1",
                vec![
                    plain_cell("t1_r1c0", "a"),
                    plain_cell("t1_r1c1", "b"),
                    plain_cell("t1_r1c2", "c"),
                ],
            ),
        ],
        structure_hash: String::new(),
        formatting: TableFormatting {
            borders: Some(single_borders()),
            ..TableFormatting::default()
        },
        formatting_change: None,
    };
    let doc = doc_with(table);

    // Logical col 2 in row 0 is the cell AFTER the span-2 anchor; shading it by
    // logical column (not physical index 1) must hit the post-span cell.
    let txn = translate(&shade_cell_json("t1", 0, 2));
    let edited = apply_transaction(&doc, &txn).expect("apply").0;
    let et = find_table(&edited, "t1");
    assert!(
        et.rows[0].cells[0].formatting.shading.is_none(),
        "the span-2 anchor must be untouched"
    );
    assert_eq!(
        et.rows[0].cells[1].formatting.shading,
        Some(yellow_shading()),
        "logical col 2 resolved to the post-span cell"
    );
}

#[test]
fn set_cell_format_interior_of_merged_cell_refused() {
    let mut wide = plain_cell("t1_r0c0", "WIDE");
    wide.grid_span = 2;
    let table = TableNode {
        id: NodeId::from("t1"),
        rows: vec![
            row("t1_r0", vec![wide]),
            row(
                "t1_r1",
                vec![plain_cell("t1_r1c0", "a"), plain_cell("t1_r1c1", "b")],
            ),
        ],
        structure_hash: String::new(),
        formatting: TableFormatting::default(),
        formatting_change: None,
    };
    let doc = doc_with(table);
    // Logical col 1 is the INTERIOR of the span-2 cell — out of range.
    let txn = translate(&shade_cell_json("t1", 0, 1));
    let err = apply_transaction(&doc, &txn).unwrap_err();
    assert!(
        format!("{err:?}").contains("TableColumnIndexOutOfRange"),
        "interior of a merged cell must be out-of-range, got {err:?}"
    );
}

#[test]
fn set_cell_format_on_vmerge_continuation_refused() {
    let mut anchor = plain_cell("t1_r0c0", "MERGED");
    anchor.v_merge = VerticalMerge::Restart;
    let mut cont = plain_cell("t1_r1c0", "");
    cont.v_merge = VerticalMerge::Continue;
    let table = TableNode {
        id: NodeId::from("t1"),
        rows: vec![
            row("t1_r0", vec![anchor, plain_cell("t1_r0c1", "x")]),
            row("t1_r1", vec![cont, plain_cell("t1_r1c1", "y")]),
        ],
        structure_hash: String::new(),
        formatting: TableFormatting::default(),
        formatting_change: None,
    };
    let doc = doc_with(table);
    let txn = translate(&shade_cell_json("t1", 1, 0));
    let err = apply_transaction(&doc, &txn).unwrap_err();
    assert!(
        format!("{err:?}").contains("TableCellNotEditable"),
        "vMerge continuation must be refused, got {err:?}"
    );
}

#[test]
fn set_cell_format_row_out_of_range_refused() {
    let doc = doc_with(formatted_table());
    let txn = translate(&shade_cell_json("t1", 9, 0));
    let err = apply_transaction(&doc, &txn).unwrap_err();
    assert!(
        format!("{err:?}").contains("TableRowIndexOutOfRange"),
        "expected TableRowIndexOutOfRange, got {err:?}"
    );
}

#[test]
fn set_cell_format_on_a_paragraph_target_refused() {
    let doc = doc_with(formatted_table());
    // "body" is a paragraph, not a table.
    let txn = translate(&shade_cell_json("body", 0, 0));
    let err = apply_transaction(&doc, &txn).unwrap_err();
    assert!(
        format!("{err:?}").contains("NotATable"),
        "a paragraph target must be refused as NotATable, got {err:?}"
    );
}

// ─── Opaque preservation (the verb never touches text/inlines) ────────────────

#[test]
fn set_cell_format_preserves_a_cell_opaque_inline() {
    // A cell whose paragraph holds an opaque inline (a field). A tcPr change must
    // leave the cell's content — including the opaque — byte-identical.
    let opaque = InlineNode::from(OpaqueInlineNode {
        id: NodeId::from("t1_r0c0_field"),
        kind: OpaqueKind::Field(stemma::FieldData {
            field_kind: stemma::FieldKind::Simple,
            instruction_text: Some("PAGE".to_string()),
            result_text: Some("1".to_string()),
            semantic: None,
        }),
        opaque_ref: "field_t1_r0c0".to_string(),
        proof_ref: ProofRef {
            part: stemma::DocPart::DocumentXml,
            block_id: NodeId::from("t1_r0c0_p"),
            docx_anchor: String::new(),
        },
        wrapper_marks: Vec::new(),
        wrapper_style_props: StyleProps::default(),
        raw_xml: Some(
            br#"<w:fldSimple w:instr="PAGE"><w:r><w:t>1</w:t></w:r></w:fldSimple>"#.to_vec(),
        ),
        content_hash: None,
    });
    let mut para = text_para("t1_r0c0_p", "before ");
    if let Some(seg) = para.segments.first_mut() {
        seg.inlines.push(opaque);
    }
    let cell = TableCellNode {
        id: NodeId::from("t1_r0c0"),
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
    };
    let base_cell = cell.clone();
    let table = TableNode {
        id: NodeId::from("t1"),
        rows: vec![row("t1_r0", vec![cell, plain_cell("t1_r0c1", "x")])],
        structure_hash: String::new(),
        formatting: TableFormatting::default(),
        formatting_change: None,
    };
    let doc = doc_with(table);
    let txn = translate(&shade_cell_json("t1", 0, 0));
    let edited = apply_transaction(&doc, &txn).expect("apply").0;
    let et = find_table(&edited, "t1");
    // Content (blocks, including the opaque inline) is byte-identical; only the
    // tcPr (shading + formatting_change) changed.
    assert_eq!(
        et.rows[0].cells[0].blocks, base_cell.blocks,
        "cell content (incl. opaque) must be byte-preserved across a tcPr change"
    );
    assert_eq!(
        et.rows[0].cells[0].formatting.shading,
        Some(yellow_shading())
    );
}

// ─── End-to-end: serialize a tracked tcPrChange and validate the package ──────

/// A hermetic 1×2 table DOCX with a non-default cell width on cell (0,0). This
/// goes through the full `Document::parse` → `apply` → `serialize` round-trip so
/// the post-serialization validator and the real XML can be inspected.
fn make_table_docx() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:tbl><w:tblPr><w:tblW w:w="5000" w:type="pct"/><w:tblBorders><w:top w:val="single" w:sz="4" w:space="0" w:color="000000"/><w:left w:val="single" w:sz="4" w:space="0" w:color="000000"/><w:bottom w:val="single" w:sz="4" w:space="0" w:color="000000"/><w:right w:val="single" w:sz="4" w:space="0" w:color="000000"/></w:tblBorders></w:tblPr><w:tblGrid><w:gridCol w:w="2400"/><w:gridCol w:w="2400"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="2400" w:type="dxa"/></w:tcPr><w:p><w:r><w:t xml:space="preserve">r0c0</w:t></w:r></w:p></w:tc><w:tc><w:tcPr><w:tcW w:w="2400" w:type="dxa"/></w:tcPr><w:p><w:r><w:t xml:space="preserve">r0c1</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/><w:sectPr/></w:body></w:document>"#;
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"></Relationships>"#;

    use std::io::Write;
    use zip::write::FileOptions;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        zip.start_file("[Content_Types].xml", opts).unwrap();
        zip.write_all(content_types.as_bytes()).unwrap();
        zip.start_file("_rels/.rels", opts).unwrap();
        zip.write_all(rels.as_bytes()).unwrap();
        zip.start_file("word/_rels/document.xml.rels", opts)
            .unwrap();
        zip.write_all(doc_rels.as_bytes()).unwrap();
        zip.start_file("word/document.xml", opts).unwrap();
        zip.write_all(document_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

fn first_table_id(doc: &CanonDoc) -> String {
    for tb in &doc.blocks {
        if let BlockNode::Table(t) = &tb.block {
            return t.id.0.to_string();
        }
    }
    panic!("no table block");
}

#[test]
fn set_cell_format_serializes_clean_tcprchange_and_round_trips() {
    let base = Document::parse(&make_table_docx()).expect("parse");
    let table_id = first_table_id(&base.snapshot().canonical);

    let txn = translate(&shade_cell_json(&table_id, 0, 0));
    let edited = base.apply(&txn).expect("apply tracked SetCellFormatting");

    // Serialize the tracked redline; the validator gate runs inside serialize.
    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");

    // Validator-clean: no error-severity findings.
    let validation = stemma::docx_validate::validate_docx(&bytes);
    let errors: Vec<String> = validation
        .findings
        .iter()
        .filter(|f| matches!(f.severity, stemma::docx_validate::ValidationSeverity::Error))
        .map(|f| f.to_string())
        .collect();
    assert!(
        errors.is_empty(),
        "tracked set_cell_format must serialize validator-clean, got: {errors:#?}"
    );

    // The change is a real tracked tcPrChange in the XML — not a segment ins/del.
    let archive = DocxArchive::read(&bytes).expect("read");
    let doc_xml = String::from_utf8_lossy(archive.get("word/document.xml").expect("document.xml"))
        .to_string();
    assert!(
        doc_xml.contains("tcPrChange"),
        "serialized doc must carry a w:tcPrChange, xml: {doc_xml}"
    );
    assert!(
        doc_xml.contains("FFFF00"),
        "the new shading must be present in the live tcPr"
    );

    // Reject-all => the prior (unshaded) cell; accept-all => the shaded cell.
    let rejected = edited
        .project(stemma::Resolution::RejectAll)
        .expect("reject");
    let rej_xml = String::from_utf8_lossy(
        DocxArchive::read(
            &rejected
                .serialize(&ExportOptions::default())
                .expect("serialize reject"),
        )
        .expect("read")
        .get("word/document.xml")
        .expect("document.xml"),
    )
    .to_string();
    assert!(
        !rej_xml.contains("FFFF00") && !rej_xml.contains("tcPrChange"),
        "reject-all must revert to the unshaded cell with no tcPrChange, xml: {rej_xml}"
    );
}
