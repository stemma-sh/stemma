//! `validate` and the ZIP items Word leaves under `[trash]/`.
//!
//! Word itself writes discarded space into a package as `[trash]/0000.dat`,
//! `[trash]/0001.dat`, ...: items that start with four `FF` bytes and have no
//! declared content type. They are not package parts, and Word opens a
//! document that carries them without repair, so `validate` must not report
//! them.
//!
//! The exemption is exactly as narrow as Word's. Checked against Word: the
//! same bytes under any other name, or a `[trash]/NNNN.dat` item that does
//! not start with the marker, make Word repair the document, so those stay
//! reported as parts with no declared content type.

use std::io::{Cursor, Write};

use stemma::validate_docx_report;
use zip::{ZipWriter, write::FileOptions};

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
const DOCUMENT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
const DOCUMENT: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>The harbour ledger opens here.</w:t></w:r></w:p><w:sectPr><w:pgSz w:w="12240" w:h="15840"/></w:sectPr></w:body></w:document>"#;

/// Word's trash marker followed by the zero fill Word writes after it.
fn trash_bytes() -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xFF, 0xFF, 0xFF];
    bytes.extend(std::iter::repeat_n(0u8, 60));
    bytes
}

fn docx_with_extra_item(name: &str, data: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut zip = ZipWriter::new(Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        for (part, text) in [
            ("[Content_Types].xml", CONTENT_TYPES),
            ("_rels/.rels", ROOT_RELS),
            ("word/_rels/document.xml.rels", DOCUMENT_RELS),
            ("word/document.xml", DOCUMENT),
        ] {
            zip.start_file(part, opts).expect("start_file");
            zip.write_all(text.as_bytes()).expect("write part");
        }
        zip.start_file(name, opts).expect("start_file");
        zip.write_all(data).expect("write item");
        zip.finish().expect("finish zip");
    }
    buf
}

fn undeclared_part_issues(docx: &[u8]) -> Vec<String> {
    validate_docx_report(docx)
        .expect("validate")
        .issues
        .into_iter()
        .filter(|issue| issue.message.contains("has no declared OPC content type"))
        .map(|issue| issue.message)
        .collect()
}

#[test]
fn word_trash_item_is_not_reported() {
    let docx = docx_with_extra_item("[trash]/0000.dat", &trash_bytes());
    let report = validate_docx_report(&docx).expect("validate");
    assert!(
        report.ok,
        "a document carrying Word's own trash item is sound; got: {:?}",
        report.issues
    );
}

#[test]
fn later_numbered_word_trash_item_is_not_reported() {
    let docx = docx_with_extra_item("[trash]/0007.dat", &trash_bytes());
    assert_eq!(undeclared_part_issues(&docx), Vec::<String>::new());
}

#[test]
fn trash_named_item_without_the_marker_is_reported() {
    let docx = docx_with_extra_item("[trash]/0000.dat", &[0u8; 64]);
    assert_eq!(
        undeclared_part_issues(&docx),
        vec![r#"package part "[trash]/0000.dat" has no declared OPC content type"#.to_string()]
    );
}

#[test]
fn marker_bytes_under_another_trash_name_are_reported() {
    let docx = docx_with_extra_item("[trash]/item.bin", &trash_bytes());
    assert_eq!(
        undeclared_part_issues(&docx),
        vec![r#"package part "[trash]/item.bin" has no declared OPC content type"#.to_string()]
    );
}

#[test]
fn marker_bytes_under_a_part_name_are_reported() {
    let docx = docx_with_extra_item("word/extra.dat", &trash_bytes());
    assert_eq!(
        undeclared_part_issues(&docx),
        vec![r#"package part "word/extra.dat" has no declared OPC content type"#.to_string()]
    );
}
