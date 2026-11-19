//! Paragraph marks resolved before a table, and range markers inside stacked
//! changes and cell content controls.
//!
//! - Observed in Word 16: a paragraph of untracked text whose mark is a
//!   tracked insertion, then an empty paragraph whose mark is a tracked
//!   insertion, then a table. Reject All joins the text into the first
//!   paragraph of the table's first cell, in that paragraph's properties;
//!   the table stays.
//! - A bookmark start inside text inserted and then deleted by another
//!   author (`w:ins` holding `w:del`), its end outside. Both resolutions
//!   remove the text; the bookmark must survive as a pair (ECMA-376
//!   §17.13.6), so the document saves.
//! - A bookmark started between a cell's paragraphs and ended inside the
//!   cell's content control: the pair survives a resolution and the
//!   document saves.

use std::io::{Cursor, Write};

use stemma::api::Document;
use stemma::docx::DocxArchive;
use stemma::{ExportOptions, Resolution};
use zip::ZipWriter;
use zip::write::FileOptions;

const WHO: &str = r#"w:author="Ingrid" w:date="2024-03-02T09:10:00Z""#;
const OTHER: &str = r#"w:author="Nils" w:date="2024-03-03T09:10:00Z""#;

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

fn resolve(body: &str, resolution: Resolution) -> String {
    let bytes = Document::parse(&docx(body))
        .expect("the document loads")
        .project(resolution)
        .expect("the document resolves")
        .serialize(&ExportOptions::default())
        .expect("the resolved document saves");
    let archive = DocxArchive::read(&bytes).expect("the package reads");
    String::from_utf8_lossy(archive.get("word/document.xml").expect("document part")).to_string()
}

/// Each paragraph's text, in order (cells included).
fn paragraphs(xml: &str) -> Vec<String> {
    fn walk(element: &xmltree::Element, out: &mut Vec<String>) {
        if element.name == "p" {
            fn text(element: &xmltree::Element, into: &mut String) {
                for child in element.children.iter().filter_map(|c| c.as_element()) {
                    if child.name == "t" {
                        into.push_str(&child.get_text().unwrap_or_default());
                    } else {
                        text(child, into);
                    }
                }
            }
            let mut line = String::new();
            text(element, &mut line);
            out.push(line);
            return;
        }
        for child in element.children.iter().filter_map(|c| c.as_element()) {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(
        &xmltree::Element::parse(xml.as_bytes()).expect("parses"),
        &mut out,
    );
    out
}

#[test]
fn reject_all_joins_text_before_a_table_into_its_first_cell() {
    let body = format!(
        concat!(
            r#"<w:p><w:pPr><w:jc w:val="center"/><w:rPr><w:ins w:id="1" {who}/></w:rPr></w:pPr>"#,
            r#"<w:r><w:t>Schedule A</w:t></w:r></w:p>"#,
            r#"<w:p><w:pPr><w:rPr><w:ins w:id="2" {who}/></w:rPr></w:pPr></w:p>"#,
            r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="3000"/><w:gridCol w:w="3000"/></w:tblGrid>"#,
            r#"<w:tr><w:tc><w:p><w:pPr><w:spacing w:after="0"/></w:pPr><w:r><w:t>Tariffs</w:t></w:r></w:p></w:tc>"#,
            r#"<w:tc><w:p><w:r><w:t>Page 1</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
            r#"<w:p><w:r><w:t>Closing line.</w:t></w:r></w:p>"#,
        ),
        who = WHO
    );
    let rejected = resolve(&body, Resolution::RejectAll);
    assert_eq!(
        paragraphs(&rejected),
        ["Schedule ATariffs", "Page 1", "Closing line."]
    );
    let cell = &rejected[rejected.find("<w:tc>").expect("a cell")..];
    assert!(
        cell.starts_with("<w:tc><w:p><w:pPr><w:spacing"),
        "the joined paragraph keeps the cell paragraph's properties: {rejected}"
    );
    assert_eq!(
        paragraphs(&resolve(&body, Resolution::AcceptAll)),
        ["Schedule A", "", "Tariffs", "Page 1", "Closing line."]
    );
}

#[test]
fn a_bookmark_inside_inserted_then_deleted_text_survives_both_resolutions() {
    let body = format!(
        concat!(
            r#"<w:p><w:r><w:t xml:space="preserve">Keep </w:t></w:r>"#,
            r#"<w:ins w:id="1" {who}><w:del w:id="2" {other}>"#,
            r#"<w:r><w:delText xml:space="preserve">this </w:delText></w:r>"#,
            r#"<w:bookmarkStart w:id="61" w:name="_Ref1"/>"#,
            r#"<w:r><w:delText xml:space="preserve">too </w:delText></w:r></w:del></w:ins></w:p>"#,
            r#"<w:bookmarkEnd w:id="61"/><w:p><w:r><w:t>Next.</w:t></w:r></w:p>"#,
        ),
        who = WHO,
        other = OTHER
    );
    for resolution in [Resolution::AcceptAll, Resolution::RejectAll] {
        let xml = resolve(&body, resolution);
        assert_eq!(paragraphs(&xml), ["Keep ", "Next."]);
        assert_eq!(xml.matches("<w:bookmarkStart").count(), 1, "{xml}");
        assert_eq!(xml.matches("<w:bookmarkEnd").count(), 1, "{xml}");
    }
}

#[test]
fn a_bookmark_ending_inside_a_cell_content_control_survives_a_resolution() {
    let body = format!(
        concat!(
            r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid><w:tr><w:tc>"#,
            r#"<w:p><w:r><w:t>Contents</w:t></w:r></w:p>"#,
            r#"<w:bookmarkStart w:id="4" w:name="_Toc1"/>"#,
            r#"<w:sdt><w:sdtPr><w:id w:val="5"/></w:sdtPr><w:sdtContent>"#,
            r#"<w:bookmarkEnd w:id="4"/><w:p><w:r><w:t>Berths</w:t></w:r></w:p>"#,
            r#"</w:sdtContent></w:sdt><w:p/></w:tc></w:tr></w:tbl>"#,
            r#"<w:p><w:ins w:id="9" {who}><w:r><w:t>New.</w:t></w:r></w:ins></w:p>"#,
        ),
        who = WHO
    );
    let xml = resolve(&body, Resolution::AcceptAll);
    assert_eq!(paragraphs(&xml), ["Contents", "Berths", "", "New."]);
    assert_eq!(xml.matches("<w:bookmarkStart").count(), 1, "{xml}");
    assert_eq!(xml.matches("<w:bookmarkEnd").count(), 1, "{xml}");
}
