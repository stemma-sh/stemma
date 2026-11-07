//! A range marker standing between paragraphs is zero-width.
//!
//! Word writes the end of its `_GoBack` bookmark as a `w:bookmarkEnd` directly
//! under `w:body`, between the paragraph holding the start and the next one
//! (ECMA-376 §17.13.6.1: a bookmark's start and end may sit at block level).
//! Such a marker is not content: it closes the range around the paragraph
//! before it. So, by the editing contract (docs/guide/editing.md):
//!
//! - an insert `after` a paragraph without a role takes the formatting of the
//!   following PARAGRAPH, which is the one after the marker, and lands after
//!   the marker: content added after the bookmarked paragraph is not inside
//!   its bookmark;
//! - deleting the paragraph that holds the start keeps the bookmark a pair in
//!   every reading: the redline, Accept All and Reject All all save (an
//!   orphaned `bookmarkEnd` is invalid, §17.13.6.2).

use stemma::ExportOptions;
use stemma::api::Document;
use stemma::domain::{Alignment, BlockNode, Mark, ParagraphNode};
use stemma::runtime::Resolution;

fn docx() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:pPr><w:jc w:val="center"/></w:pPr><w:bookmarkStart w:id="0" w:name="_GoBack"/><w:r><w:rPr><w:b/></w:rPr><w:t>Title paragraph</w:t></w:r></w:p><w:bookmarkEnd w:id="0"/><w:p><w:pPr><w:jc w:val="right"/></w:pPr><w:r><w:rPr><w:i/></w:rPr><w:t>Second paragraph</w:t></w:r></w:p><w:p><w:r><w:t>Third paragraph</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;
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

fn apply(doc: &Document, op: serde_json::Value, mode: &str) -> Document {
    let json = serde_json::json!({"ops":[op],"materialization_mode":mode,
        "revision":{"author":"Editor"}})
    .to_string();
    doc.apply(
        &stemma::edit_v4::parse_transaction(&json)
            .unwrap()
            .into_edit_transaction()
            .unwrap(),
    )
    .unwrap_or_else(|e| panic!("{mode}: {e}"))
}

fn document_xml(doc: &Document) -> String {
    let bytes = doc
        .serialize(&ExportOptions::default())
        .unwrap_or_else(|e| panic!("save: {e}"));
    String::from_utf8(
        stemma::docx::DocxArchive::read(&bytes)
            .unwrap()
            .get("word/document.xml")
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

fn paragraph_with_text<'a>(doc: &'a Document, text: &str) -> &'a ParagraphNode {
    doc.snapshot()
        .canonical
        .blocks
        .iter()
        .find_map(|tracked| match &tracked.block {
            BlockNode::Paragraph(p)
                if p.segments
                    .iter()
                    .flat_map(|s| &s.inlines)
                    .any(|i| matches!(i, stemma::InlineNode::Text(t) if t.text == text)) =>
            {
                Some(&**p)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("no paragraph reads {text:?}"))
}

fn text_marks(paragraph: &ParagraphNode, text: &str) -> Vec<Mark> {
    paragraph
        .segments
        .iter()
        .flat_map(|s| &s.inlines)
        .find_map(|i| match i {
            stemma::InlineNode::Text(t) if t.text == text => Some(t.marks.clone()),
            _ => None,
        })
        .unwrap()
}

#[test]
fn an_insert_after_a_bookmarked_paragraph_takes_the_next_paragraphs_formatting_outside_the_bookmark()
 {
    let doc = Document::parse(&docx()).expect("parse");
    let first = doc.read().blocks[0].id.to_string();
    for mode in ["tracked_change", "direct"] {
        let edited = apply(
            &doc,
            serde_json::json!({"op":"insert","target":{"anchor":first,"position":"after"},
                "content":[{"type":"paragraph","content":[{"type":"text","text":"Inserted paragraph"}]}]}),
            mode,
        );
        let inserted = paragraph_with_text(&edited, "Inserted paragraph");
        assert_eq!(
            inserted.align,
            Some(Alignment::Right),
            "{mode}: the new paragraph is formatted like the following paragraph, not the title"
        );
        assert_eq!(
            text_marks(inserted, "Inserted paragraph"),
            vec![Mark::Italic],
            "{mode}: the new text takes the following paragraph's text formatting"
        );
        let xml = document_xml(&edited);
        let end = xml
            .find("<w:bookmarkEnd")
            .expect("the bookmark end is kept");
        let new_text = xml
            .find("Inserted paragraph")
            .expect("the new paragraph is saved");
        assert!(
            end < new_text,
            "{mode}: the new paragraph lands after the bookmark's end, outside the bookmark: {xml}"
        );
    }
}

#[test]
fn deleting_the_paragraph_holding_a_bookmark_start_keeps_the_pair_valid_in_every_reading() {
    let doc = Document::parse(&docx()).expect("parse");
    let first = doc.read().blocks[0].id.to_string();
    let edited = apply(
        &doc,
        serde_json::json!({"op":"delete","target":first,"expect":"Title paragraph"}),
        "tracked_change",
    );
    document_xml(&edited);
    for (reading, resolution, title_kept) in [
        ("accept all", Resolution::AcceptAll, false),
        ("reject all", Resolution::RejectAll, true),
    ] {
        let projected = edited
            .project(resolution)
            .unwrap_or_else(|e| panic!("{reading}: {e}"));
        let xml = document_xml(&projected);
        assert_eq!(
            xml.contains("Title paragraph"),
            title_kept,
            "{reading}: {xml}"
        );
        assert_eq!(
            xml.matches("<w:bookmarkStart").count(),
            xml.matches("<w:bookmarkEnd").count(),
            "{reading}: every bookmark is a pair: {xml}"
        );
        if title_kept {
            assert!(
                xml.contains("_GoBack"),
                "{reading}: the rejected deletion restores the bookmark: {xml}"
            );
        }
    }
}
