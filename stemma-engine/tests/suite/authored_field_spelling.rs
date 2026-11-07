//! An inserted field has one spelling. `w:fldSimple` cannot sit inside
//! `w:ins`, so a tracked inserted simple field is written as a complex field
//! (begin, instruction, end) and import reads that exact lowering back as the
//! simple field. A complex field with anything more, such as Word's
//! `w:dirty`, is a different document and stays as written.
//!
//! The same holds for a whole hyperlink in a revision: `w:hyperlink` cannot sit
//! inside `w:ins`/`w:del`, so it is written as a complete HYPERLINK field and
//! that exact lowering reads back as the link.

use std::io::Write as _;

use stemma::api::Document;
use stemma::domain::{BlockNode, FieldKind, InlineNode, OpaqueKind, TrackingStatus};

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn docx(paragraph_inner: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="{W}"><w:body><w:p><w:r><w:t xml:space="preserve">See </w:t></w:r>{paragraph_inner}</w:p><w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = zip::write::FileOptions::default();
        for (name, body) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/document.xml", document_xml.as_str()),
        ] {
            zip.start_file(name, opts).expect("start part");
            zip.write_all(body.as_bytes()).expect("write part");
        }
        zip.finish().expect("finish package");
    }
    buf
}

fn field_kinds(bytes: &[u8]) -> Vec<FieldKind> {
    let document = Document::parse(bytes).expect("parse");
    let BlockNode::Paragraph(paragraph) = &document.snapshot().canonical.blocks[0].block else {
        panic!("the fixture is one paragraph");
    };
    paragraph
        .segments
        .iter()
        .flat_map(|segment| &segment.inlines)
        .filter_map(|inline| match inline {
            InlineNode::OpaqueInline(opaque) => match &opaque.kind {
                OpaqueKind::Field(field) => Some(field.field_kind.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn inserted_complex_field(begin_attrs: &str) -> String {
    format!(
        r#"<w:ins w:id="1" w:author="A" w:date="2026-01-01T00:00:00Z"><w:r><w:fldChar w:fldCharType="begin"{begin_attrs}/></w:r><w:r><w:instrText xml:space="preserve">PAGE</w:instrText></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:ins>"#
    )
}

#[test]
fn the_serializers_own_lowering_reads_back_as_the_simple_field() {
    assert_eq!(
        field_kinds(&docx(&inserted_complex_field(""))),
        vec![FieldKind::Simple]
    );
}

#[test]
fn a_complex_field_with_more_than_the_lowering_stays_complex() {
    assert_eq!(
        field_kinds(&docx(&inserted_complex_field(r#" w:dirty="true""#))),
        vec![FieldKind::Begin, FieldKind::Instruction, FieldKind::End]
    );
}

fn deleted_hyperlink_field(instruction: &str) -> String {
    format!(
        r#"<w:del w:id="1" w:author="A" w:date="2026-01-01T00:00:00Z"><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:delInstrText xml:space="preserve">{instruction}</w:delInstrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:rPr><w:rStyle w:val="Hyperlink"/></w:rPr><w:delText>Click here</w:delText></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:del>"#
    )
}

/// (segment status is Deleted, url, display text, extra attributes).
type Link = (bool, Option<String>, String, Vec<(String, String)>);

/// Every hyperlink in the fixture paragraph.
fn deleted_links(bytes: &[u8]) -> Vec<Link> {
    let document = Document::parse(bytes).expect("parse");
    let BlockNode::Paragraph(paragraph) = &document.snapshot().canonical.blocks[0].block else {
        panic!("the fixture is one paragraph");
    };
    paragraph
        .segments
        .iter()
        .flat_map(|segment| {
            segment
                .inlines
                .iter()
                .map(move |inline| (&segment.status, inline))
        })
        .filter_map(|(status, inline)| match inline {
            InlineNode::OpaqueInline(opaque) => match &opaque.kind {
                OpaqueKind::Hyperlink(link) => Some((
                    matches!(status, TrackingStatus::Deleted(_)),
                    link.url.clone(),
                    link.text.clone(),
                    link.extra_attrs.clone(),
                )),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[test]
fn a_lowered_deleted_hyperlink_reads_back_as_the_deleted_link() {
    // `\h` is the lowering of a link without visited history (§17.16.22
    // default); the link records no history attribute.
    let bytes = docx(&deleted_hyperlink_field(
        r#"HYPERLINK "https://example.com" \h"#,
    ));
    assert_eq!(
        deleted_links(&bytes),
        vec![(
            true,
            Some("https://example.com".to_string()),
            "Click here".to_string(),
            Vec::new()
        )]
    );
    assert!(field_kinds(&bytes).is_empty());
}

#[test]
fn a_hyperlink_field_the_lowering_cannot_write_stays_a_field() {
    // A screen tip (`\o`) has no slot in the lowering, so this is Word's field,
    // not a saved link.
    let bytes = docx(&deleted_hyperlink_field(
        r#"HYPERLINK "https://example.com" \o "tip" \h"#,
    ));
    assert!(deleted_links(&bytes).is_empty());
    assert_eq!(
        field_kinds(&bytes),
        vec![
            FieldKind::Begin,
            FieldKind::Instruction,
            FieldKind::Separate,
            FieldKind::End
        ]
    );
}
