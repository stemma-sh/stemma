//! Integration tests for `set_row_format` — the in-place row formatting verb
//! (`w:trPrChange`, §17.13.5.36), the row-level sibling of the `set_cell_format`
//! exemplar.
//!
//! Domain rules encoded (OOXML §17.13 accept/reject + formatting preservation):
//!   - `apply` SUCCEEDS on a formatted base WITHOUT routing through the
//!     whole-table replace schema (no `TableHasFormattingNotInSpec`);
//!   - `accept_all` == the row with the requested height applied (the trPrChange
//!     is dropped, the new trHeight stays);
//!   - `reject_all` == the original row height (the trPrChange reverts it);
//!   - the change is a tracked `trPrChange` (`row.formatting_change`), NOT a
//!     segment ins/del — the row's cell TEXT is byte-identical;
//!   - the table's `tblPr`, every OTHER row, and every cell of the target row
//!     are byte-identical before/after;
//!   - the stacking guard refuses a second format on an already-changed row;
//!   - a no-op patch is refused (`NoRowFormattingRequested`);
//!   - opaque inlines in the row's cells are preserved (the verb never touches text);
//!   - `row_index` out of range / a paragraph target fail loud.

use stemma::ExportOptions;
use stemma::accept_all;
use stemma::api::Document;
use stemma::docx::DocxArchive;
use stemma::domain::*;
use stemma::edit::{EditTransaction, apply_transaction};
use stemma::edit_v4::parse_transaction;
use stemma::reject_all_with_styles;

// ─── Doc-construction helpers (mirroring cell_formatting.rs) ─────────────────

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

/// A row carrying an EXPLICIT prior height (360 twips, atLeast) so reject must
/// restore that value (not just `None`).
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

/// A 2×2 table with table-level borders — the kind of REAL formatted table the
/// whole-table v4-replace guard refuses.
fn formatted_table() -> TableNode {
    TableNode {
        id: NodeId::from("t1"),
        rows: vec![
            row(
                "t1_r0",
                vec![plain_cell("t1_r0c0", "r0c0"), plain_cell("t1_r0c1", "r0c1")],
            ),
            row(
                "t1_r1",
                vec![plain_cell("t1_r1c0", "r1c0"), plain_cell("t1_r1c1", "r1c1")],
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

/// A `set_row_format` op that re-heights the target row to 720 twips, exact.
fn reheight_row_json(target: &str, row: usize) -> String {
    format!(
        r#"{{ "ops": [{{ "op": "set_row_format", "target": "{target}",
            "row_index": {row}, "height": 720, "height_rule": "exact" }}],
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

// ─── Core: apply SUCCEEDS on a formatted table, produces a trPrChange ─────────

#[test]
fn set_row_format_on_formatted_table_does_not_refuse() {
    let doc = doc_with(formatted_table());
    let txn = translate(&reheight_row_json("t1", 0));
    let edited = apply_transaction(&doc, &txn);
    assert!(
        edited.is_ok(),
        "set_row_format on a formatted table must NOT route through the \
         whole-table replace guard, got {:?}",
        edited.err()
    );
}

#[test]
fn set_row_format_records_a_tracked_trprchange_not_a_segment_change() {
    let base = formatted_table();
    let doc = doc_with(base.clone());
    let txn = translate(&reheight_row_json("t1", 0));
    let edited = apply_transaction(&doc, &txn).expect("apply").0;
    let et = find_table(&edited, "t1");

    // The change is a tracked trPrChange on the target row.
    let target = &et.rows[0];
    let fc = target
        .formatting_change
        .as_ref()
        .expect("target row must carry a trPrChange");
    // Its inner trPr is the PREVIOUS state: the row was 360/atLeast before.
    assert_eq!(
        fc.previous_height,
        Some(360),
        "trPrChange inner trPr must capture the prior height"
    );
    assert_eq!(fc.previous_height_rule, Some(HeightRule::AtLeast));
    assert_eq!(fc.author, "Counsel");
    // The NEW live state carries the requested height.
    assert_eq!(target.height, Some(720));
    assert_eq!(target.height_rule, Some(HeightRule::Exact));

    // It is NOT a segment ins/del: the row's cells are byte-identical to base.
    assert_eq!(
        target.cells, base.rows[0].cells,
        "no tracked segments — a trPr change is not a text edit"
    );
}

#[test]
fn set_row_format_accept_keeps_reject_reverts() {
    let doc = doc_with(formatted_table());
    let txn = translate(&reheight_row_json("t1", 0));
    let edited = apply_transaction(&doc, &txn).expect("apply").0;

    // accept_all => the row keeps the requested height; the trPrChange is gone.
    let mut accepted = edited.clone();
    accept_all(&mut accepted);
    let at = find_table(&accepted, "t1");
    assert_eq!(at.rows[0].height, Some(720), "accept keeps the new height");
    assert_eq!(at.rows[0].height_rule, Some(HeightRule::Exact));
    assert!(
        at.rows[0].formatting_change.is_none(),
        "accept clears the trPrChange"
    );

    // reject_all => the row reverts to the original height.
    let mut rejected = edited.clone();
    reject_all_with_styles(&mut rejected, None);
    let rt = find_table(&rejected, "t1");
    assert_eq!(
        rt.rows[0].height,
        Some(360),
        "reject restores the prior height"
    );
    assert_eq!(rt.rows[0].height_rule, Some(HeightRule::AtLeast));
    assert!(
        rt.rows[0].formatting_change.is_none(),
        "reject clears the trPrChange"
    );
}

// ─── Byte-preservation: tblPr + every other row + the target's cells ─────────

#[test]
fn set_row_format_preserves_tblpr_other_rows_and_target_cells() {
    let base = formatted_table();
    let doc = doc_with(base.clone());
    let txn = translate(&reheight_row_json("t1", 0));
    let edited = apply_transaction(&doc, &txn).expect("apply").0;
    let et = find_table(&edited, "t1");

    // Table-level formatting (tblPr) is byte-identical.
    assert_eq!(
        et.formatting, base.formatting,
        "tblPr (borders/width) must be byte-preserved"
    );

    // Every OTHER row is a byte-identical whole node.
    assert_eq!(et.rows[1], base.rows[1], "untouched row node");

    // The TARGET row's cells are byte-identical; only height/height_rule and
    // formatting_change changed.
    assert_eq!(
        et.rows[0].cells, base.rows[0].cells,
        "target row's cells must be byte-preserved"
    );
    assert_eq!(et.rows[0].id, base.rows[0].id);
    assert_eq!(et.rows[0].is_header, base.rows[0].is_header);
    assert_eq!(et.rows[0].grid_before, base.rows[0].grid_before);
    assert_eq!(et.rows[0].grid_after, base.rows[0].grid_after);
}

// ─── No-op + stacking guards ──────────────────────────────────────────────────

#[test]
fn set_row_format_noop_is_refused() {
    // Requesting exactly the row's CURRENT height (360/atLeast) passes schema
    // (non-empty), then short-circuits in the verb to NO trPrChange.
    let base = formatted_table();
    let doc = doc_with(base.clone());
    let txn = translate(
        r#"{ "ops": [{ "op": "set_row_format", "target": "t1",
            "row_index": 0, "height": 360, "height_rule": "atLeast" }],
            "revision": { "author": "Counsel" } }"#,
    );
    let edited = apply_transaction(&doc, &txn).expect("apply").0;
    let et = find_table(&edited, "t1");
    assert!(
        et.rows[0].formatting_change.is_none(),
        "setting the current value must short-circuit to NO trPrChange"
    );
    assert_eq!(
        et.rows[0], base.rows[0],
        "a no-op set must leave the row byte-identical"
    );
}

#[test]
fn set_row_format_empty_patch_refused_at_schema() {
    // An empty set_row_format (no property at all) is refused at the wire edge.
    let err = parse_transaction(
        r#"{ "ops": [{ "op": "set_row_format", "target": "t1",
            "row_index": 0 }],
            "revision": { "author": "Counsel" } }"#,
    )
    .unwrap_err();
    assert!(
        format!("{err:?}").contains("EmptyRowFormat"),
        "empty set_row_format must be refused at schema, got {err:?}"
    );
}

#[test]
fn set_row_format_unknown_height_rule_refused_at_adapter() {
    // A bad height_rule token fails loud at the wire edge (no silent coerce).
    let err = parse_transaction(
        r#"{ "ops": [{ "op": "set_row_format", "target": "t1",
            "row_index": 0, "height_rule": "tallish" }],
            "revision": { "author": "Counsel" } }"#,
    )
    .expect("schema passes (non-empty)")
    .into_edit_transaction()
    .unwrap_err();
    assert!(
        format!("{err:?}").contains("UnknownHeightRule"),
        "an unknown height_rule must be refused at the adapter, got {err:?}"
    );
}

#[test]
fn set_row_format_stacking_on_changed_row_refused() {
    let doc = doc_with(formatted_table());
    // First format succeeds.
    let txn1 = translate(&reheight_row_json("t1", 0));
    let once = apply_transaction(&doc, &txn1).expect("first apply").0;
    // A second format on the same (already-changed) row must be refused.
    let txn2 = translate(
        r#"{ "ops": [{ "op": "set_row_format", "target": "t1",
            "row_index": 0, "height": 900 }],
            "revision": { "author": "Counsel" } }"#,
    );
    let err = apply_transaction(&once, &txn2).unwrap_err();
    assert!(
        format!("{err:?}").contains("TableRowNotEditable"),
        "stacking a second trPrChange must be refused, got {err:?}"
    );
}

// ─── Addressing: out-of-range, wrong target kind ─────────────────────────────

#[test]
fn set_row_format_row_out_of_range_refused() {
    let doc = doc_with(formatted_table());
    let txn = translate(&reheight_row_json("t1", 9));
    let err = apply_transaction(&doc, &txn).unwrap_err();
    assert!(
        format!("{err:?}").contains("TableRowIndexOutOfRange"),
        "expected TableRowIndexOutOfRange, got {err:?}"
    );
}

#[test]
fn set_row_format_on_a_paragraph_target_refused() {
    let doc = doc_with(formatted_table());
    // "body" is a paragraph, not a table.
    let txn = translate(&reheight_row_json("body", 0));
    let err = apply_transaction(&doc, &txn).unwrap_err();
    assert!(
        format!("{err:?}").contains("NotATable"),
        "a paragraph target must be refused as NotATable, got {err:?}"
    );
}

// ─── Opaque preservation (the verb never touches text/inlines) ────────────────

#[test]
fn set_row_format_preserves_a_cell_opaque_inline() {
    // A row whose cell holds an opaque inline (a field). A trPr change must
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
    let base_cells = vec![cell.clone()];
    let table = TableNode {
        id: NodeId::from("t1"),
        rows: vec![row("t1_r0", vec![cell])],
        structure_hash: String::new(),
        formatting: TableFormatting::default(),
        formatting_change: None,
    };
    let doc = doc_with(table);
    let txn = translate(&reheight_row_json("t1", 0));
    let edited = apply_transaction(&doc, &txn).expect("apply").0;
    let et = find_table(&edited, "t1");
    // Cells (including the opaque inline) are byte-identical; only the trPr
    // (height + formatting_change) changed.
    assert_eq!(
        et.rows[0].cells, base_cells,
        "row cells (incl. opaque) must be byte-preserved across a trPr change"
    );
    assert_eq!(et.rows[0].height, Some(720));
}

// ─── End-to-end: serialize a tracked trPrChange and validate the package ──────

/// A hermetic 1×1 table DOCX whose single row carries an explicit height.
fn make_table_docx() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:tbl><w:tblPr><w:tblW w:w="5000" w:type="pct"/></w:tblPr><w:tblGrid><w:gridCol w:w="2400"/></w:tblGrid><w:tr><w:trPr><w:trHeight w:val="360" w:hRule="atLeast"/></w:trPr><w:tc><w:tcPr/><w:p><w:r><w:t xml:space="preserve">r0c0</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/><w:sectPr/></w:body></w:document>"#;
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
fn set_row_format_serializes_clean_trprchange_and_round_trips() {
    let base = Document::parse(&make_table_docx()).expect("parse");
    let table_id = first_table_id(&base.snapshot().canonical);

    let txn = translate(&reheight_row_json(&table_id, 0));
    let edited = base.apply(&txn).expect("apply tracked SetRowFormatting");

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
        "tracked set_row_format must serialize validator-clean, got: {errors:#?}"
    );

    // The change is a real tracked trPrChange in the XML — not a segment ins/del.
    let archive = DocxArchive::read(&bytes).expect("read");
    let doc_xml = String::from_utf8_lossy(archive.get("word/document.xml").expect("document.xml"))
        .to_string();
    assert!(
        doc_xml.contains("trPrChange"),
        "serialized doc must carry a w:trPrChange, xml: {doc_xml}"
    );
    assert!(
        doc_xml.contains(r#"w:val="720""#),
        "the new height must be present in the live trPr, xml: {doc_xml}"
    );

    // Reject-all => the prior (360) row; accept-all => the 720 row.
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
        !rej_xml.contains("trPrChange") && !rej_xml.contains(r#"w:val="720""#),
        "reject-all must revert to the 360 row with no trPrChange, xml: {rej_xml}"
    );
    assert!(
        rej_xml.contains(r#"w:val="360""#),
        "reject-all must restore the prior 360 height, xml: {rej_xml}"
    );
}
