//! A range marker standing between the paragraphs of a table cell.
//!
//! A cell holds block-level content like the body (§17.4.66 `w:tc`), and a
//! bookmark's start and end may sit there at block level, between two
//! paragraphs (§17.13.6.1). As in the body (body_level_range_marker_blocks),
//! a paragraph inserted after the last paragraph the bookmark covers, or
//! before the first, joins it. Word evidence: in Word 16, Enter at the end
//! of "Marked line" then "New line" gives Bookmarks("marked").Range.Text
//! "Marked line¶New line¶"; "New line" then Enter at its start gives
//! "New line¶Marked line¶", with Track Changes on and off.

use stemma::ExportOptions;
use stemma::api::Document;

fn docx() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:tbl><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="4000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>Opening line</w:t></w:r></w:p><w:bookmarkStart w:id="0" w:name="marked"/><w:p><w:r><w:t>Marked line</w:t></w:r></w:p><w:bookmarkEnd w:id="0"/><w:p><w:r><w:t>Closing line</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/><w:sectPr/></w:body></w:document>"#;
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    use std::io::Write;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions = zip::write::FileOptions::default();
        for (name, data) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/document.xml", document_xml),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

fn marked_line_id(doc: &Document) -> String {
    for tracked in &doc.snapshot().canonical.blocks {
        let stemma::BlockNode::Table(table) = &tracked.block else {
            continue;
        };
        for block in table
            .rows
            .iter()
            .flat_map(|row| &row.cells)
            .flat_map(|cell| &cell.blocks)
        {
            if let stemma::BlockNode::Paragraph(paragraph) = block
                && paragraph.all_inlines().any(
                    |inline| matches!(inline, stemma::InlineNode::Text(t) if t.text == "Marked line"),
                )
            {
                return paragraph.id.to_string();
            }
        }
    }
    panic!("no cell paragraph reads 'Marked line'")
}

fn insert(doc: &Document, position: &str, mode: &str) -> Document {
    let json = serde_json::json!({"ops":[{"op":"insert","target":{"anchor":marked_line_id(doc),"position":position},
        "content":[{"type":"paragraph","content":[{"type":"text","text":"New line"}]}]}],
        "materialization_mode":mode,"revision":{"author":"Editor"}})
    .to_string();
    doc.apply(
        &stemma::edit_v4::parse_transaction(&json)
            .unwrap()
            .into_edit_transaction()
            .unwrap(),
    )
    .unwrap_or_else(|e| panic!("{mode} insert {position}: {e}"))
}

fn document_xml(doc: &Document) -> String {
    let bytes = doc.serialize(&ExportOptions::default()).expect("save");
    String::from_utf8(
        stemma::docx::DocxArchive::read(&bytes)
            .unwrap()
            .get("word/document.xml")
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

#[test]
fn a_paragraph_inserted_beside_a_bookmarked_cell_paragraph_joins_the_bookmark() {
    let doc = Document::parse(&docx()).expect("parse");
    for mode in ["tracked_change", "direct"] {
        let after = document_xml(&insert(&doc, "after", mode));
        let end = after.find("<w:bookmarkEnd").expect("bookmark end kept");
        let new = after.find("New line").expect("new paragraph saved");
        assert!(
            new < end,
            "{mode}: before the bookmark's end, inside it: {after}"
        );

        let before = document_xml(&insert(&doc, "before", mode));
        let start = before
            .find("<w:bookmarkStart")
            .expect("bookmark start kept");
        let new = before.find("New line").expect("new paragraph saved");
        let marked = before.find("Marked line").expect("marked line kept");
        assert!(
            start < new && new < marked,
            "{mode}: the bookmark starts at the new first paragraph: {before}"
        );
    }
}
