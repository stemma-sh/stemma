//! A tracked `SetTableFormatting` is Word-visible and Word-rejectable.
//!
//! WORD RULE (bisected against live Word): Word NEVER
//! registers a lone `w:tblPrChange` — the revision is invisible AND reject-all
//! silently KEEPS the new table formatting. Any row/cell-level change carrier
//! (trPrChange/tcPrChange/tblPrEx change) makes Word register the coalesced
//! `table_property` revision and honor reject. Word's own writer emits no-op
//! trPrChange snapshots for exactly this reason; the verb mirrors it.

use std::io::Write;

use stemma::ExportOptions;
use stemma::api::Document;
use stemma::domain::*;
use stemma::edit::*;
use stemma::{Resolution, RevisionInfo};
use zip::write::FileOptions;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn make_table_docx() -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W_NS}"><w:body><w:tbl><w:tblPr/><w:tblGrid><w:gridCol/><w:gridCol/></w:tblGrid><w:tr><w:tc><w:tcPr/><w:p><w:r><w:t xml:space="preserve">A</w:t></w:r></w:p></w:tc><w:tc><w:tcPr/><w:p><w:r><w:t xml:space="preserve">B</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:tcPr/><w:p><w:r><w:t xml:space="preserve">C</w:t></w:r></w:p></w:tc><w:tc><w:tcPr/><w:p><w:r><w:t xml:space="preserve">D</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/><w:sectPr/></w:body></w:document>"#
    );
    let ct = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let o: FileOptions = FileOptions::default();
        zip.start_file("[Content_Types].xml", o).unwrap();
        zip.write_all(ct.as_bytes()).unwrap();
        zip.start_file("_rels/.rels", o).unwrap();
        zip.write_all(rels.as_bytes()).unwrap();
        zip.start_file("word/_rels/document.xml.rels", o).unwrap();
        zip.write_all(br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#).unwrap();
        zip.start_file("word/document.xml", o).unwrap();
        zip.write_all(document_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

fn formatting_txn(block_id: NodeId) -> EditTransaction {
    let edge = Border {
        style: BorderStyle::Single,
        color: Some("000000".to_string()),
        size: Some(4),
        space: Some(0),
        extra_attrs: Vec::new(),
    };
    EditTransaction {
        steps: vec![EditStep::SetTableFormatting {
            block_id,
            semantic_hash: None,
            patch: TableFormattingPatch {
                borders: Some(BorderSet {
                    top: Some(edge.clone()),
                    bottom: Some(edge.clone()),
                    left: Some(edge.clone()),
                    right: Some(edge.clone()),
                    inside_h: Some(edge.clone()),
                    inside_v: Some(edge),
                }),
                width: Some(TableMeasurement {
                    w: 5000,
                    width_type: WidthType::Pct,
                    pct_literal: None,
                }),
                default_cell_margins: Some(CellMargins {
                    top: Some(60),
                    bottom: Some(60),
                    left: Some(120),
                    right: Some(120),
                }),
            },
            rationale: None,
        }],
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 7,
            author: Some("fid".into()),
            date: Some("2026-07-02T00:00:00Z".into()),
            apply_op_id: None,
        },
        summary: None,
    }
}

fn table_xml(out: &[u8]) -> String {
    let a = stemma::docx::DocxArchive::read(out).expect("archive");
    let xml = String::from_utf8(a.get("word/document.xml").unwrap().to_vec()).unwrap();
    let i = xml.find("<w:tbl>").unwrap();
    let j = xml.find("</w:tbl>").unwrap();
    xml[i..j + 8].to_string()
}

fn apply_formatting(base: &[u8]) -> stemma::api::Document {
    let doc = Document::parse(base).expect("parse");
    let id = doc.read().blocks[0].id.clone();
    doc.apply(&formatting_txn(id)).expect("apply")
}

/// The tracked change carries a companion no-op trPrChange on the first row
/// (with its own annotation id, like Word's per-carrier ids), and the
/// requested tblCellMar is in the live tblPr.
#[test]
fn tracked_table_format_emits_companion_row_carrier() {
    let out = apply_formatting(&make_table_docx())
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let tbl = table_xml(&out);
    assert!(
        tbl.contains("<w:tblPrChange"),
        "the table-level change record must be present; table: {tbl}"
    );
    assert!(
        tbl.contains("<w:trPrChange"),
        "Word never honors a lone tblPrChange (invisible + reject keeps the \
         formatting); a companion row carrier is required; table: {tbl}"
    );
    assert_eq!(
        tbl.matches("<w:trPrChange").count(),
        1,
        "exactly ONE companion carrier — not one per row; table: {tbl}"
    );
    // Each carrier gets its own annotation id (Word's writer does the same;
    // the validator enforces uniqueness via I-ANN-001).
    let id_of = |marker: &str| -> String {
        let i = tbl.find(marker).unwrap();
        let rest = &tbl[i + marker.len()..];
        rest[..rest.find('"').unwrap()].to_string()
    };
    assert_ne!(
        id_of(r#"<w:tblPrChange w:id=""#),
        id_of(r#"<w:trPrChange w:id=""#),
        "annotation ids are document-unique (I-ANN-001); table: {tbl}"
    );
    // The authored tblCellMar survives serialization (provenance claimed).
    assert!(
        tbl.contains("<w:tblCellMar"),
        "the requested default cell margins must serialize; table: {tbl}"
    );
}

/// stemma's own accept/reject resolves the companion together with the
/// tblPrChange: accept keeps the new tblPr and clears BOTH markers; reject
/// restores the original (unboxed, auto-width) table and clears BOTH markers.
#[test]
fn companion_carrier_resolves_atomically() {
    let edited = apply_formatting(&make_table_docx());

    let accepted = edited
        .project(Resolution::AcceptAll)
        .expect("accept")
        .serialize(&ExportOptions::default())
        .expect("serialize accepted");
    let tbl = table_xml(&accepted);
    assert!(
        tbl.contains("<w:tblBorders") && !tbl.contains("Change"),
        "accept keeps the formatting and clears both markers; table: {tbl}"
    );

    let rejected = edited
        .project(Resolution::RejectAll)
        .expect("reject")
        .serialize(&ExportOptions::default())
        .expect("serialize rejected");
    let tbl = table_xml(&rejected);
    assert!(
        !tbl.contains("<w:tblBorders") && !tbl.contains("Change"),
        "reject restores the unformatted table and clears both markers; table: {tbl}"
    );
}
