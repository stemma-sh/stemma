//! Pagination properties through `set_para_format`.
//!
//! Domain rule: a heading is kept on the page of the paragraph it introduces
//! with `w:keepNext` (§17.3.1.14); `w:keepLines` (§17.3.1.15) keeps a
//! paragraph's lines together and `w:pageBreakBefore` (§17.3.1.23) starts it on
//! a new page. They are paragraph properties like alignment, so setting one is
//! a tracked paragraph-property change: accept keeps it, reject restores the
//! paragraph as it was.

use stemma::api::Document;
use stemma::{ExportOptions, Resolution};

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

const BODY: &str = r#"<w:p><w:r><w:t>Questions and contacts</w:t></w:r></w:p><w:p><w:r><w:t>If you have questions, call the office.</w:t></w:r></w:p>"#;

fn first_paragraph_ppr(doc: &Document) -> String {
    let bytes = doc.serialize(&ExportOptions::default()).expect("serialize");
    let xml = String::from_utf8(
        stemma::docx::DocxArchive::read(&bytes)
            .expect("read")
            .get("word/document.xml")
            .expect("document part")
            .to_vec(),
    )
    .expect("utf-8");
    let paragraph = &xml[xml.find("<w:p").expect("a paragraph")..];
    let paragraph = &paragraph[..paragraph.find("</w:p>").expect("closed")];
    paragraph
        .find("<w:pPr>")
        .map(|start| paragraph[start..paragraph.find("</w:pPr>").expect("closed") + 8].to_string())
        .unwrap_or_default()
}

fn set(doc: &Document, fields: &str) -> Document {
    let id = doc.read().blocks[0].id.to_string();
    let json = format!(
        r#"{{"ops":[{{"op":"set_para_format","target":"{id}",{fields}}}],"revision":{{"author":"Editor"}}}}"#
    );
    doc.apply(
        &stemma::edit_v4::parse_transaction(&json)
            .expect("parse")
            .into_edit_transaction()
            .expect("adapt"),
    )
    .expect("apply")
}

#[test]
fn keep_with_next_is_a_tracked_paragraph_property() {
    let doc = Document::parse(&docx(BODY)).expect("parse");
    let edited = set(&doc, r#""keep_with_next":true"#);
    let ppr = first_paragraph_ppr(&edited);
    assert!(
        ppr.contains("<w:keepNext") && ppr.contains("<w:pPrChange"),
        "keepNext set as a tracked change: {ppr}"
    );
    let accepted = edited.project(Resolution::AcceptAll).expect("accept");
    let accepted_ppr = first_paragraph_ppr(&accepted);
    assert!(
        accepted_ppr.contains("<w:keepNext") && !accepted_ppr.contains("pPrChange"),
        "{accepted_ppr}"
    );
    let rejected = edited.project(Resolution::RejectAll).expect("reject");
    assert!(
        !first_paragraph_ppr(&rejected).contains("keepNext"),
        "reject restores the paragraph"
    );
}

#[test]
fn keep_lines_and_page_break_before_are_set_and_cleared() {
    let doc = Document::parse(&docx(BODY)).expect("parse");
    let edited = set(
        &doc,
        r#""keep_lines_together":true,"page_break_before":true"#,
    );
    let accepted = edited.project(Resolution::AcceptAll).expect("accept");
    let ppr = first_paragraph_ppr(&accepted);
    assert!(
        ppr.contains("<w:keepLines") && ppr.contains("<w:pageBreakBefore"),
        "{ppr}"
    );
    let cleared = set(&accepted, r#""page_break_before":false"#)
        .project(Resolution::AcceptAll)
        .expect("accept");
    let ppr = first_paragraph_ppr(&cleared);
    assert!(
        ppr.contains("<w:keepLines") && !ppr.contains(r#"<w:pageBreakBefore/>"#),
        "the page break is turned off, the lines stay together: {ppr}"
    );
}

#[test]
fn setting_the_current_value_changes_nothing() {
    let doc = Document::parse(&docx(BODY)).expect("parse");
    let kept = set(&doc, r#""keep_with_next":true"#)
        .project(Resolution::AcceptAll)
        .expect("accept");
    let again = set(&kept, r#""keep_with_next":true"#);
    assert!(again.revisions().is_empty(), "no empty tracked change");
}

/// A paragraph's properties change independently of the text in it: Word
/// records a tracked paragraph-format change (pPrChange) on a paragraph whose
/// runs carry tracked insertions and deletions. Editing a paragraph's words and
/// then its indent, in two calls, is the common sequence (joining a broken
/// paragraph, then giving it the body indent); the second call must apply.
#[test]
fn a_paragraph_with_tracked_text_changes_can_be_reformatted() {
    let doc = Document::parse(&docx(BODY)).expect("parse");
    let id = doc.read().blocks[0].id.to_string();
    let text = doc.read().blocks[0].text.clone();
    let first_word = text.split_whitespace().next().expect("a word").to_string();
    let json = serde_json::json!({"ops":[{"op":"replace","target":id,"expect":first_word,
        "content":{"type":"paragraph","content":[{"type":"text","text":text.replacen(&first_word, "Revised", 1)}]}}],
        "revision":{"author":"Editor"}})
    .to_string();
    let worded = doc
        .apply(
            &stemma::edit_v4::parse_transaction(&json)
                .unwrap()
                .into_edit_transaction()
                .unwrap(),
        )
        .expect("text edit applies");
    let edited = set(&worded, r#""indent":{"left":0,"first_line":720}"#);
    let ppr = first_paragraph_ppr(&edited);
    assert!(
        ppr.contains(r#"w:firstLine="720""#) && ppr.contains("<w:pPrChange"),
        "the indent is a tracked paragraph change: {ppr}"
    );
    let accepted = edited.project(Resolution::AcceptAll).expect("accept");
    assert!(first_paragraph_ppr(&accepted).contains(r#"w:firstLine="720""#));
    assert!(accepted.read().blocks[0].text.starts_with("Revised"));
    let rejected = edited.project(Resolution::RejectAll).expect("reject");
    assert!(!first_paragraph_ppr(&rejected).contains("firstLine"));
    assert_eq!(rejected.read().blocks[0].text, text);
    let reopened = Document::parse(&edited.serialize(&ExportOptions::default()).unwrap()).unwrap();
    let differences = stemma::roundtrip_compare::compare_canon_docs(
        &edited.snapshot().canonical,
        &reopened.snapshot().canonical,
    );
    assert!(
        differences.is_empty(),
        "{:?}",
        &differences[..differences.len().min(3)]
    );
}
