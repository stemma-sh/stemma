//! Typed labels read as written, and an inserted item is labelled once.
//!
//! Domain rule: a typed label ("(c)") is the paragraph's own text, and so is
//! the whitespace after it. A quotation whose items read "(c) the design…"
//! (label, space, text) must read that way: reading it as "(c)\tthe design…"
//! led an agent to write tabs the items never had. A new item modelled on
//! such a paragraph is labelled by its position ("(b)" after "(a)"), so its
//! content must not repeat the label.

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

/// Items (a) and (c) of a list, each "label, space, text".
const ITEMS: &str = r#"<w:p><w:r><w:t xml:space="preserve">(a) the address of the site where work is planned; </w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve">(c) the date on which the work begins; </w:t></w:r></w:p>"#;

fn saved_text(doc: &Document) -> String {
    let bytes = doc.serialize(&ExportOptions::default()).expect("serialize");
    let reopened = Document::parse(&bytes).expect("reopen");
    reopened
        .read()
        .blocks
        .iter()
        .map(|b| b.text.clone())
        .collect::<Vec<_>>()
        .join("\n")
}

fn insert_after_first(doc: &Document, text: &str) -> Result<Document, stemma::RuntimeError> {
    let view = doc.read();
    let (anchor, template) = (view.blocks[0].id.to_string(), view.blocks[1].id.to_string());
    let json = serde_json::json!({"ops":[{"op":"insert","target":{"anchor":anchor,"position":"after"},"content":[{"type":"paragraph","attrs":{"format_from":template},"content":[{"type":"text","text":text}]}]}],"revision":{"author":"Editor"}}).to_string();
    doc.apply(
        &stemma::edit_v4::parse_transaction(&json)
            .expect("parse")
            .into_edit_transaction()
            .expect("adapt"),
    )
}

#[test]
fn a_typed_label_reads_with_its_own_separator() {
    let doc = Document::parse(&docx(ITEMS)).expect("parse");
    let view = doc.read();
    assert_eq!(view.blocks[1].literal_prefix.as_deref(), Some("(c)"));
    assert_eq!(
        view.blocks[1].text,
        "(c) the date on which the work begins; "
    );
}

#[test]
fn an_inserted_item_takes_its_label_from_its_position() {
    let doc = Document::parse(&docx(ITEMS)).expect("parse");
    let inserted = insert_after_first(&doc, "the name of the contractor doing the work; ")
        .expect("the item is inserted");
    let accepted = inserted.project(Resolution::AcceptAll).expect("accept");
    assert_eq!(
        saved_text(&accepted),
        "(a) the address of the site where work is planned; \n\
         (b) the name of the contractor doing the work; \n\
         (c) the date on which the work begins; "
    );
}

#[test]
fn an_inserted_item_that_repeats_its_label_is_refused_with_the_fix() {
    let doc = Document::parse(&docx(ITEMS)).expect("parse");
    let err = match insert_after_first(&doc, "(b) the name of the contractor doing the work; ") {
        Ok(_) => panic!("the label would be written twice"),
        Err(err) => err,
    };
    assert_eq!(format!("{:?}", err.code), "PrefixDuplicatesLabel");
    assert!(
        err.message.contains("'(b)'")
            && err
                .message
                .contains("'the name of the contractor doing the work; '"),
        "the refusal names the label and the content to send: {}",
        err.message
    );
}

/// A typed label's own formatting is readable: in a heading "X.  Schwab v.
/// Oscar" whose label is bold and whose case name is bold and underlined, the
/// label reads as bold only. It is not a span, so a reader rewriting the
/// heading with styled content would otherwise copy the underline onto it.
#[test]
fn a_typed_label_reports_its_own_marks() {
    let doc = Document::parse(&docx(
        r#"<w:p><w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve">X.  </w:t></w:r><w:r><w:rPr><w:b/><w:u w:val="single"/></w:rPr><w:t>Schwab v. Oscar</w:t></w:r></w:p>"#,
    ))
    .expect("parse");
    let block = &doc.read().blocks[0];
    assert_eq!(
        block.literal_prefix.as_deref(),
        Some("X."),
        "fixture: a typed label"
    );
    assert_eq!(
        block.literal_prefix_marks,
        vec![stemma::view::TextMark::Bold]
    );
}
