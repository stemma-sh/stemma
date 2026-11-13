//! A typed list label ("1." then a tab, no `w:numPr`) is text the author
//! wrote at the start of a paragraph. When comparison replaces a table
//! cell's paragraphs whole, each paragraph's label is part of the deleted
//! or inserted paragraph: Reject All restores the base cell and Accept All
//! gives the target cell, labels included, in memory and after save and
//! reopen. A label left untracked would survive into both readings and,
//! once its paragraph mark is resolved away, land in a neighbouring
//! paragraph.

use std::io::Write;

use stemma::api::Document;
use stemma::domain::{BlockNode, InlineNode};
use stemma::{ExportOptions, Resolution};

fn cell_docx(cell_paragraphs: &[&str]) -> Vec<u8> {
    let cell: String = cell_paragraphs
        .iter()
        .map(|text| {
            let runs: String = text
                .split('\t')
                .enumerate()
                .map(|(i, part)| {
                    let tab = if i > 0 { "<w:tab/>" } else { "" };
                    format!(r#"<w:r>{tab}<w:t xml:space="preserve">{part}</w:t></w:r>"#)
                })
                .collect();
            format!("<w:p>{runs}</w:p>")
        })
        .collect();
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Meeting notes</w:t></w:r></w:p><w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="8000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="8000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>Minutes of the quarterly review</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:tcPr><w:tcW w:w="8000" w:type="dxa"/></w:tcPr>{cell}</w:tc></w:tr></w:tbl><w:p/><w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"></Relationships>"#;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = zip::write::FileOptions::default();
        for (name, part) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/document.xml", document_xml.as_str()),
        ] {
            zip.start_file(name, opts).expect("start part");
            zip.write_all(part.as_bytes()).expect("write part");
        }
        zip.finish().expect("finish package");
    }
    buf
}

fn reopened(document: &Document) -> Document {
    let bytes = document
        .serialize(&ExportOptions::default())
        .expect("serialize");
    Document::parse(&bytes).expect("reopen")
}

/// The cell's paragraphs as written: typed label (with its whitespace) and
/// text.
fn cell_reading(document: &Document) -> Vec<String> {
    let blocks = &document.snapshot().canonical.blocks;
    let BlockNode::Table(table) = &blocks[1].block else {
        panic!("the second block is the table: {:?}", blocks[1].block);
    };
    table.rows[1].cells[0]
        .blocks
        .iter()
        .map(|block| {
            let BlockNode::Paragraph(p) = block else {
                panic!("a cell block that is not a paragraph");
            };
            let mut out = String::new();
            if let Some(label) = &p.literal_prefix {
                out.push_str(&p.literal_prefix_leading_ws);
                out.push_str(label);
                out.push_str(&p.literal_prefix_trailing_ws);
            }
            for inline in p.all_inlines() {
                match inline {
                    InlineNode::Text(t) => out.push_str(&t.text),
                    InlineNode::HardBreak(_) => out.push('\n'),
                    _ => {}
                }
            }
            out
        })
        .collect()
}

fn reading(document: &Document, resolution: Resolution) -> Vec<String> {
    let resolved = reopened(&document.project(resolution).expect("resolve"));
    assert!(resolved.revisions().is_empty(), "a resolved reading");
    cell_reading(&resolved)
}

fn assert_laws(base_cell: &[&str], target_cell: &[&str], redline: Document) {
    let base = Document::parse(&cell_docx(base_cell)).expect("parse base");
    let target = Document::parse(&cell_docx(target_cell)).expect("parse target");
    let want_base = cell_reading(&reopened(&base));
    let want_target = cell_reading(&reopened(&target));
    let reopened_redline = reopened(&redline);
    for (name, doc) in [("in memory", &redline), ("reopened", &reopened_redline)] {
        assert_eq!(
            reading(doc, Resolution::RejectAll),
            want_base,
            "{name}: Reject All reads as the base cell"
        );
        assert_eq!(
            reading(doc, Resolution::AcceptAll),
            want_target,
            "{name}: Accept All reads as the target cell"
        );
    }
}

const BASE_CELL: &[&str] = &[
    "1.\tUpdate from the regional office",
    "2.\tStaffing over the winter months",
    "Closing remarks by the chair",
];
const TARGET_CELL: &[&str] = &[
    "Agenda moved to a later date entirely",
    "Closing remarks by the chair",
];

#[test]
fn classic_comparison_tracks_typed_labels_of_replaced_cell_paragraphs() {
    let base = Document::parse(&cell_docx(BASE_CELL)).expect("parse base");
    let target = Document::parse(&cell_docx(TARGET_CELL)).expect("parse target");
    let redline = stemma_diff::diff_as(&base, &target, "compare").expect("compare");
    assert_laws(BASE_CELL, TARGET_CELL, redline);
}

#[test]
fn classic_comparison_tracks_typed_labels_of_inserted_cell_paragraphs() {
    let base = Document::parse(&cell_docx(TARGET_CELL)).expect("parse base");
    let target = Document::parse(&cell_docx(BASE_CELL)).expect("parse target");
    let redline = stemma_diff::diff_as(&base, &target, "compare").expect("compare");
    assert_laws(TARGET_CELL, BASE_CELL, redline);
}

/// A cell whose outer paragraphs stay while the labelled paragraphs between
/// them are deleted or inserted.
const KEPT_CELL_FULL: &[&str] = &[
    "Opening words from the chair of the meeting",
    "1.\tUpdate from the regional office",
    "2.\tStaffing over the winter months",
    "Closing remarks by the chair of the meeting",
];
const KEPT_CELL_SHORT: &[&str] = &[
    "Opening words from the chair of the meeting",
    "Closing remarks by the chair of the meeting",
];

#[test]
fn classic_comparison_tracks_typed_labels_of_deleted_and_inserted_cell_paragraphs() {
    for (from, to) in [
        (KEPT_CELL_FULL, KEPT_CELL_SHORT),
        (KEPT_CELL_SHORT, KEPT_CELL_FULL),
    ] {
        let base = Document::parse(&cell_docx(from)).expect("parse base");
        let target = Document::parse(&cell_docx(to)).expect("parse target");
        let redline = stemma_diff::diff_as(&base, &target, "compare").expect("compare");
        assert_laws(from, to, redline);
    }
}
