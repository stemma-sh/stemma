//! A replacement keeps the formatting of the characters it does not change.
//!
//! Domain rule (docs/reference/operations.md: a local text replacement
//! "inherits its mixed character formatting"): rewriting a paragraph's text
//! changes only what differs. A word made of differently formatted runs
//! (bold "abc" then red "123") that loses "123" and gains "456" keeps "abc"
//! bold, and the new "456" takes the formatting of the text it replaces — as
//! typing over a selection does in Word, where the new text takes the first
//! selected character's formatting. Reject All restores every run as it was.
//! The same holds for a table cell's text (`set_cell_text`).

use stemma::api::Document;
use stemma::domain::{BlockNode, Mark, ParagraphNode};
use stemma::{InlineNode, Resolution};

fn docx(body: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
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
            ("word/document.xml", document_xml.as_str()),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

const MIXED_WORD: &str = r#"<w:p><w:r><w:t xml:space="preserve">Total </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>abc</w:t></w:r><w:r><w:rPr><w:color w:val="FF0000"/></w:rPr><w:t>123</w:t></w:r><w:r><w:t xml:space="preserve"> units.</w:t></w:r></w:p>"#;

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

fn replace(doc: &Document, text: &str, mode: &str) -> Document {
    let target = doc.read().blocks[0].id.to_string();
    apply(
        doc,
        serde_json::json!({"op":"replace","target":target,"expect":"Total abc123 units.",
            "content":{"type":"paragraph","content":[{"type":"text","text":text}]}}),
        mode,
    )
}

/// Each character of `paragraph`, with the formatting of the run holding it:
/// (char, bold, colour).
fn char_formatting(paragraph: &ParagraphNode) -> Vec<(char, bool, Option<String>)> {
    paragraph
        .segments
        .iter()
        .flat_map(|s| &s.inlines)
        .filter_map(|inline| match inline {
            InlineNode::Text(t) => Some(t),
            _ => None,
        })
        .flat_map(|t| {
            let bold = t.marks.contains(&Mark::Bold);
            let color = t.style_props.color.as_deref().map(str::to_string);
            t.text.chars().map(move |c| (c, bold, color.clone()))
        })
        .collect()
}

fn first_paragraph(doc: &Document) -> ParagraphNode {
    match &doc.snapshot().canonical.blocks[0].block {
        BlockNode::Paragraph(p) => (**p).clone(),
        other => panic!("expected a paragraph, got {other:?}"),
    }
}

fn reading_of(doc: &Document, resolution: Resolution) -> Vec<(char, bool, Option<String>)> {
    char_formatting(&first_paragraph(&doc.project(resolution).expect("project")))
}

fn expected(spans: &[(&str, bool, Option<&str>)]) -> Vec<(char, bool, Option<String>)> {
    spans
        .iter()
        .flat_map(|(text, bold, color)| {
            text.chars()
                .map(move |c| (c, *bold, color.map(str::to_string)))
        })
        .collect()
}

#[test]
fn replacing_part_of_a_mixed_word_keeps_the_unchanged_characters_formatting() {
    let doc = Document::parse(&docx(MIXED_WORD)).expect("parse");
    let base = reading_of(&doc, Resolution::AcceptAll);
    for mode in ["tracked_change", "direct"] {
        let edited = replace(&doc, "Total abc456 units.", mode);
        assert_eq!(
            reading_of(&edited, Resolution::AcceptAll),
            expected(&[
                ("Total ", false, None),
                ("abc", true, None),
                ("456", false, Some("FF0000")),
                (" units.", false, None),
            ]),
            "{mode}: \"abc\" stays bold and the new digits take the replaced digits' red"
        );
        if mode == "tracked_change" {
            assert_eq!(
                reading_of(&edited, Resolution::RejectAll),
                base,
                "Reject All restores every run as it was"
            );
        }
    }
}

#[test]
fn text_typed_over_a_mixed_word_takes_the_first_replaced_characters_formatting() {
    let doc = Document::parse(&docx(MIXED_WORD)).expect("parse");
    let base = reading_of(&doc, Resolution::AcceptAll);
    let edited = replace(&doc, "Total xyz789 units.", "tracked_change");
    assert_eq!(
        reading_of(&edited, Resolution::AcceptAll),
        expected(&[
            ("Total ", false, None),
            ("xyz789", true, None),
            (" units.", false, None),
        ]),
        "the new word takes the formatting of the first character it replaces"
    );
    assert_eq!(reading_of(&edited, Resolution::RejectAll), base);
}

#[test]
fn set_cell_text_keeps_the_formatting_of_characters_it_keeps() {
    let doc = Document::parse(&docx(
        r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="4000" w:type="dxa"/></w:tcPr><w:p><w:r><w:rPr><w:b/></w:rPr><w:t>Components</w:t></w:r><w:r><w:rPr><w:b/><w:vertAlign w:val="superscript"/></w:rPr><w:t>1</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/>"#,
    ))
    .expect("parse");
    let table = doc.read().blocks[0].id.to_string();
    let edited = apply(
        &doc,
        serde_json::json!({"op":"table_op","target":table,"table_op":{"kind":"set_cell_text",
            "row_index":0,"col_index":0,"text":"Widgets1"}}),
        "tracked_change",
    );
    let accepted = edited.project(Resolution::AcceptAll).expect("accept");
    let BlockNode::Table(table) = &accepted.snapshot().canonical.blocks[0].block else {
        panic!("the document starts with a table");
    };
    let BlockNode::Paragraph(cell) = &table.rows[0].cells[0].blocks[0] else {
        panic!("the cell holds a paragraph");
    };
    let runs: Vec<(String, Vec<Mark>)> = cell
        .segments
        .iter()
        .flat_map(|s| &s.inlines)
        .filter_map(|inline| match inline {
            InlineNode::Text(t) => Some((t.text.clone(), t.marks.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        runs,
        vec![
            ("Widgets".to_string(), vec![Mark::Bold]),
            ("1".to_string(), vec![Mark::Bold, Mark::Superscript]),
        ],
        "the new word is bold like the word it replaces; the kept footnote digit stays superscript"
    );
}
