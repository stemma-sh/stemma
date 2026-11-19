//! `validate` and four faults Word does not open cleanly.
//!
//! Each was injected into Word-clean documents and checked against Word:
//!
//! - the main document part declared with a content type that is not a main
//!   document type (its Override removed, or pointing elsewhere): Word cannot
//!   open the file. A package that declares the main type as the `xml`
//!   Default, with no Override, opens without repair;
//! - one relationship id used twice in a relationships part: Word repairs;
//! - a run placed directly in the body: Word cannot open the file;
//! - inserted text spelled `w:delText`, or deleted text spelled `w:t`: Word
//!   repairs.

use std::io::{Cursor, Write};

use stemma::validate_docx_report;
use zip::{ZipWriter, write::FileOptions};

const MAIN_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml";
const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
const NO_DOCUMENT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
const PARAGRAPH: &str = r#"<w:p><w:r><w:t>The night ferry leaves from pier two.</w:t></w:r></w:p>"#;

fn content_types(xml_default: &str, main_override: Option<&str>) -> String {
    let main_override = main_override.map_or(String::new(), |content_type| {
        format!(r#"<Override PartName="/word/document.xml" ContentType="{content_type}"/>"#)
    });
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="{xml_default}"/>{main_override}</Types>"#
    )
}

fn package(content_types: &str, document_rels: &str, body: &str) -> Vec<u8> {
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr><w:pgSz w:w="12240" w:h="15840"/></w:sectPr></w:body></w:document>"#
    );
    let mut buf = Vec::new();
    {
        let mut zip = ZipWriter::new(Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        for (part, text) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", ROOT_RELS),
            ("word/_rels/document.xml.rels", document_rels),
            ("word/document.xml", document.as_str()),
        ] {
            zip.start_file(part, opts).expect("start_file");
            zip.write_all(text.as_bytes()).expect("write part");
        }
        zip.finish().expect("finish zip");
    }
    buf
}

fn with_body(body: &str) -> Vec<u8> {
    package(
        &content_types("application/xml", Some(MAIN_TYPE)),
        NO_DOCUMENT_RELS,
        body,
    )
}

fn messages(bytes: &[u8]) -> Vec<String> {
    validate_docx_report(bytes)
        .expect("validate")
        .issues
        .into_iter()
        .map(|issue| issue.message)
        .collect()
}

#[test]
fn the_plain_document_is_sound() {
    assert_eq!(messages(&with_body(PARAGRAPH)), Vec::<String>::new());
}

// --- main document content type ---------------------------------------------

#[test]
fn main_part_left_to_a_generic_xml_default_is_reported() {
    let bytes = package(
        &content_types("application/xml", None),
        NO_DOCUMENT_RELS,
        PARAGRAPH,
    );
    assert_eq!(
        messages(&bytes),
        vec![
            r#"main document part "word/document.xml" is declared with content type "application/xml", which is not a WordprocessingML main document type; Word cannot open such a file"#
                .to_string()
        ]
    );
}

#[test]
fn main_part_overridden_to_another_type_is_reported() {
    let styles_type = "application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml";
    let bytes = package(
        &content_types("application/xml", Some(styles_type)),
        NO_DOCUMENT_RELS,
        PARAGRAPH,
    );
    assert_eq!(messages(&bytes).len(), 1);
    assert!(messages(&bytes)[0].contains("not a WordprocessingML main document type"));
}

#[test]
fn main_type_given_as_the_xml_default_is_sound() {
    let bytes = package(&content_types(MAIN_TYPE, None), NO_DOCUMENT_RELS, PARAGRAPH);
    assert_eq!(messages(&bytes), Vec::<String>::new());
}

// --- relationship ids ---------------------------------------------------------

#[test]
fn a_relationship_id_used_twice_is_reported() {
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.invalid/timetable" TargetMode="External"/><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.invalid/fares" TargetMode="External"/></Relationships>"#;
    let bytes = package(
        &content_types("application/xml", Some(MAIN_TYPE)),
        rels,
        PARAGRAPH,
    );
    assert_eq!(
        messages(&bytes),
        vec![
            r#"relationship id "rId1" is used more than once in word/_rels/document.xml.rels; Word repairs such a file"#
                .to_string()
        ]
    );
}

#[test]
fn distinct_relationship_ids_are_sound() {
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.invalid/timetable" TargetMode="External"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.invalid/fares" TargetMode="External"/></Relationships>"#;
    let bytes = package(
        &content_types("application/xml", Some(MAIN_TYPE)),
        rels,
        PARAGRAPH,
    );
    assert_eq!(messages(&bytes), Vec::<String>::new());
}

// --- run directly in the body ---------------------------------------------------

#[test]
fn a_run_directly_in_the_body_is_reported() {
    let body = format!(r#"<w:r><w:t>Stray words.</w:t></w:r>{PARAGRAPH}"#);
    let found = messages(&with_body(&body));
    assert!(
        found.iter().any(|message| message.contains("I-DOC-004")),
        "{found:?}"
    );
}

// --- tracked text form -----------------------------------------------------------

const INS: &str = r#"w:id="1" w:author="Harbour Office" w:date="2026-01-05T10:00:00Z""#;

fn tracked(container: &str, text_element: &str) -> String {
    format!(
        r#"<w:p><w:r><w:t xml:space="preserve">The ferry leaves </w:t></w:r><w:{container} {INS}><w:r><w:{text_element}>late</w:{text_element}></w:r></w:{container}></w:p>"#
    )
}

#[test]
fn inserted_plain_text_and_deleted_deleted_text_are_sound() {
    for (container, text_element) in [("ins", "t"), ("del", "delText")] {
        assert_eq!(
            messages(&with_body(&tracked(container, text_element))),
            Vec::<String>::new(),
            "{container}/{text_element}"
        );
    }
}

#[test]
fn inserted_text_spelled_as_deleted_text_is_reported() {
    let found = messages(&with_body(&tracked("ins", "delText")));
    assert!(
        found.iter().any(|message| message.contains("I-TC-005")),
        "{found:?}"
    );
}

#[test]
fn deleted_text_spelled_as_plain_text_is_reported() {
    let found = messages(&with_body(&tracked("del", "t")));
    assert!(
        found.iter().any(|message| message.contains("I-TC-005")),
        "{found:?}"
    );
}

/// Text inserted by one reviewer and then deleted by another: the deletion
/// is the nearest change, so `w:delText` is the right spelling.
#[test]
fn a_deletion_inside_an_insertion_carries_deleted_text() {
    let body = format!(
        r#"<w:p><w:ins {INS}><w:del w:id="2" w:author="Pilot Station" w:date="2026-01-06T10:00:00Z"><w:r><w:delText>late</w:delText></w:r></w:del></w:ins><w:r><w:t>The ferry leaves.</w:t></w:r></w:p>"#
    );
    assert_eq!(messages(&with_body(&body)), Vec::<String>::new());
}

#[test]
fn deleted_text_outside_any_tracked_change_is_reported() {
    let body = r#"<w:p><w:r><w:t xml:space="preserve">The ferry leaves </w:t></w:r><w:r><w:delText>late</w:delText></w:r></w:p>"#;
    let found = messages(&with_body(body));
    assert!(
        found.iter().any(|message| message.contains("I-TC-005")),
        "{found:?}"
    );
}

#[test]
fn text_directly_in_a_paragraph_is_reported() {
    let body = r#"<w:p><w:t>Stray words.</w:t><w:r><w:t>The ferry leaves.</w:t></w:r></w:p>"#;
    let found = messages(&with_body(body));
    assert!(
        found.iter().any(|message| message.contains("I-DOC-005")),
        "{found:?}"
    );
}

// --- bidirectional override and embedding ---------------------------------------

#[test]
fn a_bidi_override_or_embedding_with_a_direction_is_sound() {
    for body in [
        r#"<w:p><w:bdo w:val="ltr"><w:r><w:t>B7</w:t></w:r></w:bdo></w:p>"#,
        r#"<w:p><w:bdo w:val="rtl"><w:r><w:t>B7</w:t></w:r></w:bdo></w:p>"#,
        r#"<w:p><w:dir w:val="rtl"><w:r><w:t>B7</w:t></w:r></w:dir></w:p>"#,
    ] {
        assert_eq!(messages(&with_body(body)), Vec::<String>::new(), "{body}");
    }
}

#[test]
fn a_bidi_override_or_embedding_without_a_direction_is_reported() {
    for body in [
        r#"<w:p><w:bdo><w:r><w:t>B7</w:t></w:r></w:bdo></w:p>"#,
        r#"<w:p><w:dir><w:r><w:t>B7</w:t></w:r></w:dir></w:p>"#,
        r#"<w:p><w:bdo w:val="sideways"><w:r><w:t>B7</w:t></w:r></w:bdo></w:p>"#,
    ] {
        let found = messages(&with_body(body));
        assert!(
            found.iter().any(|message| message.contains("I-RUN-001")),
            "{body}: {found:?}"
        );
    }
}

// --- section break in a table cell ----------------------------------------------

const SECTION_BREAK: &str = r#"<w:pPr><w:sectPr><w:type w:val="continuous"/><w:pgSz w:w="12240" w:h="15840"/></w:sectPr></w:pPr>"#;

#[test]
fn a_section_break_in_a_body_paragraph_is_sound() {
    let body =
        format!(r#"<w:p>{SECTION_BREAK}<w:r><w:t>The first notice.</w:t></w:r></w:p>{PARAGRAPH}"#);
    assert_eq!(messages(&with_body(&body)), Vec::<String>::new());
}

#[test]
fn a_section_break_in_a_table_cell_is_reported() {
    let body = format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="5000" w:type="dxa"/></w:tcPr><w:p>{SECTION_BREAK}<w:r><w:t>Berth list.</w:t></w:r></w:p></w:tc></w:tr></w:tbl>{PARAGRAPH}"#
    );
    let found = messages(&with_body(&body));
    assert!(
        found.iter().any(|message| message.contains("I-DOC-006")),
        "{found:?}"
    );
}
