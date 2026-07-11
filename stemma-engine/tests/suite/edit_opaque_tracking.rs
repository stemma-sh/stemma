//! Sibling-opaque tracking audit.
//!
//! The serializer's tracked-emit path (`emit_tracked_chunks`) wraps PARAGRAPH-
//! LEVEL opaques (hyperlink / fldSimple / oMathPara — see `is_paragraph_level_opaque`)
//! specially, because they cannot be a direct child of `<w:ins>`/`<w:del>`
//! (validator rule I-TC-001 / CT_RunTrackChange = EG_ContentRunContent). Math and
//! hyperlinks are handled; this audits that a `fldSimple` field which lands in a
//! tracked (Inserted/Deleted) segment is ALSO emitted as a tracked change rather
//! than untracked permanent content.
//!
//! Bytes-in; public `Document` API.

use std::io::{Cursor, Read, Write};

use stemma::api::Document;
use stemma::edit_v4::parse_transaction;
use stemma::{ExportMode, ExportOptions, ValidatorLevel};
use zip::ZipWriter;
use zip::write::FileOptions;

const CONTENT_TYPES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
const PACKAGE_RELS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
const DOC_RELS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;

fn make_docx(body_inner: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
    );
    let mut buf = Vec::new();
    {
        let mut zip = ZipWriter::new(Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        zip.start_file("[Content_Types].xml", opts).unwrap();
        zip.write_all(CONTENT_TYPES_XML.as_bytes()).unwrap();
        zip.start_file("_rels/.rels", opts).unwrap();
        zip.write_all(PACKAGE_RELS_XML.as_bytes()).unwrap();
        zip.start_file("word/_rels/document.xml.rels", opts)
            .unwrap();
        zip.write_all(DOC_RELS_XML.as_bytes()).unwrap();
        zip.start_file("word/document.xml", opts).unwrap();
        zip.write_all(document_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

fn redline_document_xml(doc: &Document) -> String {
    let bytes = doc
        .serialize(&ExportOptions {
            mode: ExportMode::Redline,
            validator_level: ValidatorLevel::Off,
            validator: None,
        })
        .expect("serialize redline");
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).expect("zip");
    let mut xml = String::new();
    zip.by_name("word/document.xml")
        .expect("document.xml")
        .read_to_string(&mut xml)
        .expect("read");
    xml
}

fn apply_v4(doc: &Document, json: &str) -> Document {
    let txn = parse_transaction(json)
        .expect("v4 schema")
        .into_edit_transaction()
        .expect("v4 adapt");
    doc.apply(&txn).expect("apply")
}

/// DOMAIN RULE: deleting a paragraph that contains a `fldSimple` field must emit
/// the field as a TRACKED deletion — the field's result runs wrapped in `<w:del>`
/// (the field element itself stays paragraph-level; per I-TC-001 it cannot be a
/// child of `<w:del>`). Otherwise Word reads the field as permanent content and
/// accept-all fails to remove it (a deleted field survives) — the same class of
/// bug fixed for synthesized hyperlinks.
#[test]
fn deleted_paragraph_with_a_field_serializes_the_field_as_tracked() {
    // Two paragraphs; the first carries a fldSimple DATE field. Delete it.
    let body = r#"<w:p><w:r><w:t xml:space="preserve">before </w:t></w:r><w:fldSimple w:instr=" DATE \@ &quot;yyyy&quot; "><w:r><w:t>2026</w:t></w:r></w:fldSimple><w:r><w:t xml:space="preserve"> after</w:t></w:r></w:p><w:p><w:r><w:t>keep me</w:t></w:r></w:p>"#;
    let doc = Document::parse(&make_docx(body)).expect("parse");

    let view = doc.read();
    let first = view.blocks.first().expect("block 0");
    let (id, guard) = (first.id.to_string(), first.guard.clone());
    let json = format!(
        r#"{{ "ops": [{{ "op": "delete", "target": "{id}", "guard": "{guard}", "expect": "before" }}],
             "revision": {{ "author": "audit" }} }}"#
    );
    let edited = apply_v4(&doc, &json);

    let xml = redline_document_xml(&edited);
    let f_start = xml
        .find("<w:fldSimple")
        .expect("redline still emits the deleted field");
    let f_end = xml[f_start..]
        .find("</w:fldSimple>")
        .map(|e| f_start + e)
        .expect("fldSimple closes");
    let field_inner = &xml[f_start..f_end];
    assert!(
        field_inner.contains("<w:del"),
        "a fldSimple in a Deleted segment must carry an in-field <w:del> envelope \
         so Word treats the field as a tracked deletion and accept-all removes it; \
         got:\n{field_inner}"
    );
}
