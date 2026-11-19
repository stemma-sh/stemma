//! Rejecting an inserted paragraph mark inside a table cell keeps the
//! paragraph's typed label.
//!
//! Observed in Word 16: in a cell, a lettered item ("(c)", a tab, then its
//! text) whose mark is a tracked insertion, followed in the same cell by a
//! paragraph whose text is all inserted and whose mark is untracked. Reject
//! All joins the item into that paragraph, as a removed mark joins two body
//! paragraphs: one paragraph holding the item's label and text, with the
//! properties of the paragraph it joined.

use std::collections::HashSet;
use std::io::{Cursor, Write};

use stemma::api::Document;
use stemma::docx::DocxArchive;
use stemma::{ExportOptions, Resolution, ResolveSelectionAction};
use zip::ZipWriter;
use zip::write::FileOptions;

const WHO: &str = r#"w:author="Ingrid" w:date="2024-03-02T09:10:00Z""#;

fn docx(body: &str) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let options = FileOptions::default();
    zip.start_file("[Content_Types].xml", options).unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#).unwrap();
    zip.start_file("_rels/.rels", options).unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#).unwrap();
    zip.start_file("word/_rels/document.xml.rels", options)
        .unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#).unwrap();
    zip.start_file("word/document.xml", options).unwrap();
    zip.write_all(
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
        )
        .as_bytes(),
    )
    .unwrap();
    zip.finish().unwrap().into_inner()
}

fn body() -> String {
    let cell = format!(
        concat!(
            r#"<w:p><w:r><w:t>(b)</w:t></w:r><w:r><w:tab/><w:t>the first berth; or</w:t></w:r></w:p>"#,
            r#"<w:p><w:pPr><w:rPr><w:ins w:id="1" {who}/></w:rPr></w:pPr>"#,
            r#"<w:r><w:t>(c)</w:t></w:r><w:r><w:tab/><w:t>the second berth; or</w:t></w:r></w:p>"#,
            r#"<w:p><w:pPr><w:spacing w:before="100"/></w:pPr>"#,
            r#"<w:ins w:id="2" {who}><w:r><w:t>a pilot is aboard</w:t></w:r></w:ins></w:p>"#,
        ),
        who = WHO
    );
    format!(
        r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid><w:tr><w:tc>{cell}</w:tc></w:tr></w:tbl><w:p><w:r><w:t>Closing line.</w:t></w:r></w:p>"#
    )
}

/// Each paragraph's text (tabs as `\t`) and whether it has the joined
/// paragraph's spacing.
fn paragraphs(bytes: &[u8]) -> Vec<(String, bool)> {
    fn walk(element: &xmltree::Element, out: &mut Vec<(String, bool)>) {
        let local = element.name.rsplit(':').next().unwrap();
        if local == "p" {
            let mut text = String::new();
            let mut spaced = false;
            collect(element, &mut text, &mut spaced);
            out.push((text, spaced));
            return;
        }
        for child in element.children.iter().filter_map(|c| c.as_element()) {
            walk(child, out);
        }
    }
    fn collect(element: &xmltree::Element, text: &mut String, spaced: &mut bool) {
        for child in element.children.iter().filter_map(|c| c.as_element()) {
            match child.name.rsplit(':').next().unwrap() {
                "t" => text.push_str(&child.get_text().unwrap_or_default()),
                "tab" if element.name.ends_with('r') => text.push('\t'),
                "spacing" => *spaced = true,
                _ => collect(child, text, spaced),
            }
        }
    }
    let archive = DocxArchive::read(bytes).expect("the package reads");
    let root = xmltree::Element::parse(archive.get("word/document.xml").expect("document part"))
        .expect("document.xml parses");
    let mut out = Vec::new();
    walk(&root, &mut out);
    out
}

fn expected() -> Vec<(String, bool)> {
    vec![
        ("(b)\tthe first berth; or".to_string(), false),
        ("(c)\tthe second berth; or".to_string(), true),
        ("Closing line.".to_string(), false),
    ]
}

fn save(doc: Document) -> Vec<u8> {
    doc.serialize(&ExportOptions::default())
        .expect("the resolved document saves")
}

#[test]
fn reject_all_keeps_the_label_of_a_cell_paragraph_whose_inserted_mark_goes() {
    let doc = Document::parse(&docx(&body())).expect("the document loads");
    let rejected = doc.project(Resolution::RejectAll).expect("rejects");
    assert_eq!(paragraphs(&save(rejected)), expected());
}

#[test]
fn rejecting_every_revision_by_id_keeps_the_label_the_same() {
    let doc = Document::parse(&docx(&body())).expect("the document loads");
    let ids: HashSet<u32> = doc
        .revisions()
        .into_iter()
        .map(|record| record.revision_id)
        .collect();
    let rejected = doc
        .project(Resolution::Selective {
            ids,
            action: ResolveSelectionAction::Reject,
        })
        .expect("rejects");
    assert_eq!(paragraphs(&save(rejected)), expected());
}
