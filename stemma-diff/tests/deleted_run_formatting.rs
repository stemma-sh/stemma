//! Comparison keeps each deleted run's own formatting.
//!
//! A word made of differently formatted runs (bold "abc" then red "123") whose
//! digits change ("456") is deleted and reinserted by the word-level compare.
//! The deletion records the text as it was: Reject All restores bold "abc"
//! and red "123" — the base — never formatting neither input had; Accept All
//! gives the target. The in-memory redline and its saved and reopened form
//! agree.

use std::io::Write;

use stemma::api::Document;
use stemma::domain::{BlockNode, Mark};
use stemma::{ExportOptions, InlineNode, Resolution};

fn docx(body: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
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

fn char_formatting(document: &Document) -> Vec<(char, bool, Option<String>)> {
    let BlockNode::Paragraph(paragraph) = &document.snapshot().canonical.blocks[0].block else {
        panic!("the document starts with a paragraph");
    };
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

const BASE: &str = r#"<w:p><w:r><w:t xml:space="preserve">Total </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>abc</w:t></w:r><w:r><w:rPr><w:color w:val="FF0000"/></w:rPr><w:t>123</w:t></w:r><w:r><w:t xml:space="preserve"> units.</w:t></w:r></w:p>"#;
const TARGET: &str = r#"<w:p><w:r><w:t xml:space="preserve">Total </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>abc</w:t></w:r><w:r><w:rPr><w:color w:val="FF0000"/></w:rPr><w:t>456</w:t></w:r><w:r><w:t xml:space="preserve"> units.</w:t></w:r></w:p>"#;

#[test]
fn a_compared_word_spanning_formatted_runs_rejects_to_the_base_formatting() {
    let base = Document::parse(&docx(BASE)).expect("parse base");
    let target = Document::parse(&docx(TARGET)).expect("parse target");
    let redline = stemma_diff::diff_as(&base, &target, "compare").expect("compare");
    let reopened_redline = reopened(&redline);
    for (reading, document) in [
        ("in-memory redline", &redline),
        ("reopened redline", &reopened_redline),
    ] {
        let rejected = document.project(Resolution::RejectAll).expect("reject all");
        assert_eq!(
            char_formatting(&reopened(&rejected)),
            char_formatting(&base),
            "{reading}: Reject All restores each base run's formatting"
        );
        let accepted = document.project(Resolution::AcceptAll).expect("accept all");
        assert_eq!(
            char_formatting(&reopened(&accepted)),
            char_formatting(&target),
            "{reading}: Accept All gives the target's formatting"
        );
    }
}
