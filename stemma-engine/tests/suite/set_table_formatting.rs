//! Integration tests for `set_table_format` — the in-place TABLE-level
//! formatting verb (`w:tblPrChange`, §17.13.5.34).
//!
//! Domain rules encoded (OOXML §17.13 accept/reject + formatting preservation):
//!   - `apply` SUCCEEDS on a formatted base WITHOUT routing through the
//!     whole-table replace schema (no `TableHasFormattingNotInSpec`);
//!   - `accept_all` == the table with the requested formatting applied (the
//!     tblPrChange is dropped, the new tblPr stays);
//!   - `reject_all` == the original table formatting (the tblPrChange reverts);
//!   - the change is a tracked `tblPrChange` (`table.formatting_change`), NOT a
//!     structural row/cell edit — every row and cell is byte-identical;
//!   - the table's UNtouched tblPr properties (e.g. an explicit width left out of
//!     the patch) are byte-identical before/after;
//!   - the stacking guard refuses a second format on an already-changed table;
//!   - a no-op patch is refused (`NoTableFormattingRequested`);
//!   - a paragraph target is refused as `NotATable`.

use stemma::ExportOptions;
use stemma::accept_all;
use stemma::api::Document;
use stemma::docx::DocxArchive;
use stemma::domain::*;
use stemma::edit::{EditTransaction, apply_transaction};
use stemma::edit_v4::parse_transaction;
use stemma::reject_all_with_styles;

// ─── Doc-construction helpers (mirroring cell_formatting.rs) ──────────────────

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

fn double_borders() -> BorderSet {
    let edge = Border {
        style: BorderStyle::Double,
        color: Some("FF0000".to_string()),
        size: Some(12),
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

fn default_margins() -> CellMargins {
    CellMargins {
        top: Some(60),
        bottom: Some(60),
        left: Some(120),
        right: Some(120),
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

/// A 2×2 table with table-level borders + an explicit width — the kind of REAL
/// formatted table the whole-table v4-replace guard refuses. The explicit width
/// is the "untouched property" the borders-only patch must byte-preserve.
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

/// A `set_table_format` op that re-borders the target table (double, red) and
/// sets default cell margins, leaving the table's width untouched.
fn reborder_table_json(target: &str) -> String {
    format!(
        r#"{{ "ops": [{{ "op": "set_table_format", "target": "{target}",
            "borders": {{
                "top":    {{ "style": "double", "color": "FF0000", "size": 12, "space": 0 }},
                "bottom": {{ "style": "double", "color": "FF0000", "size": 12, "space": 0 }},
                "left":   {{ "style": "double", "color": "FF0000", "size": 12, "space": 0 }},
                "right":  {{ "style": "double", "color": "FF0000", "size": 12, "space": 0 }},
                "inside_h": {{ "style": "double", "color": "FF0000", "size": 12, "space": 0 }},
                "inside_v": {{ "style": "double", "color": "FF0000", "size": 12, "space": 0 }}
            }},
            "default_cell_margins": {{ "top": 60, "bottom": 60, "left": 120, "right": 120 }} }}],
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

// ─── Core: apply SUCCEEDS on a formatted table, produces a tblPrChange ─────────

#[test]
fn set_table_format_on_formatted_table_does_not_refuse() {
    let doc = doc_with(formatted_table());
    let txn = translate(&reborder_table_json("t1"));
    let edited = apply_transaction(&doc, &txn);
    assert!(
        edited.is_ok(),
        "set_table_format on a formatted table must NOT route through the \
         whole-table replace guard, got {:?}",
        edited.err()
    );
}

#[test]
fn set_table_format_records_a_tracked_tblprchange_not_a_structural_change() {
    let base = formatted_table();
    let doc = doc_with(base.clone());
    let txn = translate(&reborder_table_json("t1"));
    let edited = apply_transaction(&doc, &txn).expect("apply").0;
    let et = find_table(&edited, "t1");

    // The change is a tracked tblPrChange on the table.
    let fc = et
        .formatting_change
        .as_ref()
        .expect("table must carry a tblPrChange");
    // Its inner tblPr is the PREVIOUS state: single borders, no prior margins.
    assert_eq!(
        fc.previous_borders,
        Some(single_borders()),
        "tblPrChange inner tblPr must capture the prior borders"
    );
    assert_eq!(
        fc.previous_default_cell_margins, None,
        "the table had no default cell margins before"
    );
    assert_eq!(
        fc.previous_width,
        Some(TableMeasurement {
            w: 5000,
            width_type: WidthType::Pct,
            pct_literal: None,
        }),
        "the snapshot captures the full prior tblPr, incl. untouched width"
    );
    assert_eq!(fc.author, "Counsel");

    // The NEW live state carries the requested borders + margins.
    assert_eq!(et.formatting.borders, Some(double_borders()));
    assert_eq!(et.formatting.default_cell_margins, Some(default_margins()));

    // It is NOT a structural change: every row and cell is byte-identical
    // EXCEPT the companion no-op trPrChange on the first row (WORD RULE,
    // bisected against live Word: Word never registers a lone tblPrChange and
    // reject silently keeps the formatting; a row-level carrier is what
    // registers the coalesced table_property revision).
    let companion = et.rows[0]
        .formatting_change
        .as_ref()
        .expect("first row carries the companion no-op trPrChange");
    assert_ne!(
        companion.revision_id, fc.revision_id,
        "the companion carries its own annotation id (I-ANN-001 uniqueness, \
         mirroring Word's per-carrier ids)"
    );
    assert_eq!(
        companion.author, fc.author,
        "same author: Word coalesces them"
    );
    assert_eq!(
        (
            companion.previous_height,
            companion.previous_height_rule.clone()
        ),
        (base.rows[0].height, base.rows[0].height_rule.clone()),
        "the companion is a NO-OP snapshot of the row's current trPr"
    );
    let mut et_rows_sans_companion = et.rows.clone();
    et_rows_sans_companion[0].formatting_change = None;
    assert_eq!(
        et_rows_sans_companion, base.rows,
        "no row/cell edits beyond the companion carrier — a tblPr change only"
    );
}

#[test]
fn set_table_format_accept_keeps_reject_reverts() {
    let doc = doc_with(formatted_table());
    let txn = translate(&reborder_table_json("t1"));
    let edited = apply_transaction(&doc, &txn).expect("apply").0;

    // accept_all => the table keeps the requested formatting; tblPrChange is gone.
    let mut accepted = edited.clone();
    accept_all(&mut accepted);
    let at = find_table(&accepted, "t1");
    assert_eq!(
        at.formatting.borders,
        Some(double_borders()),
        "accept keeps the new borders"
    );
    assert_eq!(
        at.formatting.default_cell_margins,
        Some(default_margins()),
        "accept keeps the new margins"
    );
    assert!(
        at.formatting_change.is_none(),
        "accept clears the tblPrChange"
    );

    // reject_all => the table reverts to the original formatting.
    let mut rejected = edited.clone();
    reject_all_with_styles(&mut rejected, None);
    let rt = find_table(&rejected, "t1");
    assert_eq!(
        rt.formatting.borders,
        Some(single_borders()),
        "reject restores the prior borders"
    );
    assert_eq!(
        rt.formatting.default_cell_margins, None,
        "reject restores the prior (absent) margins"
    );
    assert!(
        rt.formatting_change.is_none(),
        "reject clears the tblPrChange"
    );
}

// ─── Byte-preservation: rows/cells + untouched tblPr props ────────────────────

#[test]
fn set_table_format_preserves_rows_cells_and_untouched_props() {
    let base = formatted_table();
    let doc = doc_with(base.clone());
    let txn = translate(&reborder_table_json("t1"));
    let edited = apply_transaction(&doc, &txn).expect("apply").0;
    let et = find_table(&edited, "t1");

    // Every row + cell is a byte-identical whole node, except the companion
    // no-op trPrChange on the first row (the Word-visibility carrier).
    let mut et_rows_sans_companion = et.rows.clone();
    et_rows_sans_companion[0].formatting_change = None;
    assert_eq!(
        et_rows_sans_companion, base.rows,
        "rows/cells must be byte-preserved (companion carrier aside)"
    );

    // The table's UNtouched tblPr property (its explicit width) is byte-identical;
    // only `borders` + `default_cell_margins` (the requested fields) and
    // `formatting_change` changed.
    assert_eq!(
        et.formatting.width, base.formatting.width,
        "the table's untouched width must be byte-preserved"
    );
    assert_eq!(et.id, base.id);
    assert_eq!(et.structure_hash, base.structure_hash);
}

// ─── No-op + stacking guards ──────────────────────────────────────────────────

#[test]
fn set_table_format_noop_is_refused() {
    // Requesting exactly the table's CURRENT formatting (same width) passes
    // schema (non-empty) but short-circuits in the verb to NO tblPrChange.
    let base = formatted_table();
    let doc = doc_with(base.clone());
    let txn = translate(
        r#"{ "ops": [{ "op": "set_table_format", "target": "t1",
            "width": { "w": 5000, "width_type": "pct" } }],
            "revision": { "author": "Counsel" } }"#,
    );
    let edited = apply_transaction(&doc, &txn).expect("apply").0;
    let et = find_table(&edited, "t1");
    assert!(
        et.formatting_change.is_none(),
        "setting the current value must short-circuit to NO tblPrChange"
    );
    assert_eq!(
        et.formatting, base.formatting,
        "a no-op set must leave the tblPr byte-identical"
    );
}

#[test]
fn set_table_format_empty_patch_refused_at_schema() {
    let err = parse_transaction(
        r#"{ "ops": [{ "op": "set_table_format", "target": "t1" }],
            "revision": { "author": "Counsel" } }"#,
    )
    .unwrap_err();
    assert!(
        format!("{err:?}").contains("EmptyTableFormat"),
        "empty set_table_format must be refused at schema, got {err:?}"
    );
}

#[test]
fn set_table_format_stacking_on_changed_table_refused() {
    let doc = doc_with(formatted_table());
    // First format succeeds.
    let txn1 = translate(&reborder_table_json("t1"));
    let once = apply_transaction(&doc, &txn1).expect("first apply").0;
    // A second format on the same (already-changed) table must be refused.
    let txn2 = translate(
        r#"{ "ops": [{ "op": "set_table_format", "target": "t1",
            "width": { "w": 3000, "width_type": "dxa" } }],
            "revision": { "author": "Counsel" } }"#,
    );
    let err = apply_transaction(&once, &txn2).unwrap_err();
    assert!(
        format!("{err:?}").contains("TableAlreadyHasFormattingChange"),
        "stacking a second tblPrChange must be refused, got {err:?}"
    );
}

#[test]
fn set_table_format_on_a_paragraph_target_refused() {
    let doc = doc_with(formatted_table());
    let txn = translate(&reborder_table_json("body"));
    let err = apply_transaction(&doc, &txn).unwrap_err();
    assert!(
        format!("{err:?}").contains("NotATable"),
        "a paragraph target must be refused as NotATable, got {err:?}"
    );
}

// ─── End-to-end: serialize a tracked tblPrChange and validate the package ─────

fn make_table_docx() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:tbl><w:tblPr><w:tblW w:w="5000" w:type="pct"/><w:tblBorders><w:top w:val="single" w:sz="4" w:space="0" w:color="000000"/><w:left w:val="single" w:sz="4" w:space="0" w:color="000000"/><w:bottom w:val="single" w:sz="4" w:space="0" w:color="000000"/><w:right w:val="single" w:sz="4" w:space="0" w:color="000000"/></w:tblBorders></w:tblPr><w:tblGrid><w:gridCol w:w="2400"/><w:gridCol w:w="2400"/></w:tblGrid><w:tr><w:tc><w:tcPr/><w:p><w:r><w:t xml:space="preserve">A</w:t></w:r></w:p></w:tc><w:tc><w:tcPr/><w:p><w:r><w:t xml:space="preserve">B</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/><w:sectPr/></w:body></w:document>"#;
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
fn set_table_format_serializes_clean_tblprchange_and_round_trips() {
    let base = Document::parse(&make_table_docx()).expect("parse");
    let table_id = first_table_id(&base.snapshot().canonical);

    let txn = translate(&reborder_table_json(&table_id));
    let edited = base.apply(&txn).expect("apply tracked SetTableFormatting");

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
        "tracked set_table_format must serialize validator-clean, got: {errors:#?}"
    );

    // The change is a real tracked tblPrChange in the XML.
    let archive = DocxArchive::read(&bytes).expect("read");
    let doc_xml = String::from_utf8_lossy(archive.get("word/document.xml").expect("document.xml"))
        .to_string();
    assert!(
        doc_xml.contains("tblPrChange"),
        "serialized doc must carry a w:tblPrChange, xml: {doc_xml}"
    );
    assert!(
        doc_xml.contains("double"),
        "the new (double) borders must be present in the live tblPr"
    );

    // Reject-all => the prior (single-border, no-margin) table; accept-all => the
    // re-bordered table.
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
        !rej_xml.contains("double") && !rej_xml.contains("tblPrChange"),
        "reject-all must revert to the single-border table with no tblPrChange, xml: {rej_xml}"
    );
}
