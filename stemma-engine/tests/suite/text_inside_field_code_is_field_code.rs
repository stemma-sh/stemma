//! A `w:t` between a complex field's `begin` and its `separate` is part of
//! the field's code, as Word reads it: Word does not show it, and writes it
//! back inside the field's `w:instrText`. Some programs write such a run
//! (a stray character before the instruction, for one).
//!
//! Observed in Word by saving each shape and reading what it wrote back.

use std::io::{Cursor, Write};

use stemma::api::Document;
use zip::{ZipWriter, write::FileOptions};

const BEGIN: &str = r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r>"#;
const SEPARATE: &str = r#"<w:r><w:fldChar w:fldCharType="separate"/></w:r>"#;
const END: &str = r#"<w:r><w:fldChar w:fldCharType="end"/></w:r>"#;
const INSTRUCTION: &str = r#"<w:r><w:instrText xml:space="preserve"> AUTHOR </w:instrText></w:r>"#;
const RESULT: &str = r#"<w:r><w:t>Mira</w:t></w:r>"#;
const STRAY: &str = r#"<w:r><w:t>STRAY</w:t></w:r>"#;

fn docx(pieces: &[&str]) -> Vec<u8> {
    let body = format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Signed by </w:t></w:r>{}<w:r><w:t xml:space="preserve"> today.</w:t></w:r></w:p>"#,
        pieces.concat()
    );
    let files = [
        (
            "[Content_Types].xml",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#.to_string(),
        ),
        (
            "_rels/.rels",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#.to_string(),
        ),
        (
            "word/document.xml",
            format!(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
            ),
        ),
    ];
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, content) in files {
        zip.start_file(name, FileOptions::default()).unwrap();
        zip.write_all(content.as_bytes()).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn text(pieces: &[&str]) -> String {
    Document::parse(&docx(pieces)).expect("parse").to_text()
}

#[test]
fn text_before_or_after_the_instruction_is_not_document_text() {
    let plain = text(&[BEGIN, INSTRUCTION, SEPARATE, RESULT, END]);
    assert!(plain.contains("Mira"), "the result is text: {plain}");
    for pieces in [
        [BEGIN, STRAY, INSTRUCTION, SEPARATE, RESULT, END].as_slice(),
        [BEGIN, INSTRUCTION, STRAY, SEPARATE, RESULT, END].as_slice(),
        [BEGIN, STRAY, SEPARATE, RESULT, END].as_slice(),
    ] {
        assert_eq!(text(pieces), plain, "the stray run is field code");
    }
}

#[test]
fn text_in_a_field_with_no_separator_is_field_code() {
    let read = text(&[BEGIN, INSTRUCTION, STRAY, END]);
    assert!(!read.contains("STRAY"), "{read}");
    assert!(read.contains("Signed by"), "{read}");
}

#[test]
fn text_in_the_result_and_after_the_field_stays_text() {
    let read = text(&[BEGIN, INSTRUCTION, SEPARATE, STRAY, END, STRAY]);
    assert_eq!(read.matches("STRAY").count(), 2, "{read}");
}

/// An untouched save keeps the paragraph as it was written (Word opens it
/// without repair), and it still reads the same.
#[test]
fn a_saved_copy_reads_the_same() {
    let pieces = [BEGIN, STRAY, INSTRUCTION, SEPARATE, RESULT, END];
    let doc = Document::parse(&docx(&pieces)).expect("parse");
    let bytes = doc
        .serialize(&stemma::ExportOptions::default())
        .expect("serialize");
    assert!(stemma::api::validate(&bytes).ok);
    assert_eq!(
        Document::parse(&bytes).expect("reparse").to_text(),
        text(&pieces)
    );
}

/// The same holds for a field written inside a hyperlink element: the
/// link's text is the field's result only.
#[test]
fn text_inside_field_code_within_a_link_is_not_the_links_text() {
    let markdown = |inner: &[&str]| {
        let link = format!(
            r#"<w:hyperlink w:anchor="place">{}</w:hyperlink>"#,
            inner.concat()
        );
        Document::parse(&docx(&[&link]))
            .expect("parse")
            .to_markdown()
    };
    let plain = markdown(&[BEGIN, INSTRUCTION, SEPARATE, RESULT, END]);
    assert!(plain.contains("Mira"), "{plain}");
    let stray = markdown(&[BEGIN, STRAY, INSTRUCTION, SEPARATE, RESULT, END]);
    assert!(!stray.contains("STRAY"), "{stray}");
    assert!(stray.contains("Mira"), "{stray}");
    let in_result = markdown(&[BEGIN, INSTRUCTION, SEPARATE, STRAY, END]);
    assert!(in_result.contains("STRAY"), "{in_result}");
}
