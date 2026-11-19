//! `validate` and bytes outside the ZIP archive.
//!
//! A `.docx` is a ZIP archive and nothing else. Checked against Word: a
//! single byte after the end-of-central-directory record, or before the
//! first local file header, makes Word repair the document, even though ZIP
//! readers (ours included) still read every part. Such files turn up when a
//! download or a web archive leaves its own separator on the end, or writes
//! the document twice.
//!
//! A ZIP comment is declared by the end-of-central-directory record and is
//! part of the archive; Word opens a document that has one without repair.

use std::io::{Cursor, Write};

use stemma::validate_docx_report;
use zip::{ZipWriter, write::FileOptions};

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
const DOCUMENT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
const DOCUMENT: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>The lighthouse keeper signs the roster.</w:t></w:r></w:p><w:sectPr><w:pgSz w:w="12240" w:h="15840"/></w:sectPr></w:body></w:document>"#;

fn docx() -> Vec<u8> {
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
        zip.finish().expect("finish zip");
    }
    buf
}

/// The same archive with a ZIP comment declared in its end record.
fn docx_with_zip_comment(comment: &[u8]) -> Vec<u8> {
    let mut bytes = docx();
    let length = bytes.len();
    // The archive has no comment, so its end record is the last 22 bytes and
    // the comment length is the last two of them.
    assert_eq!(&bytes[length - 22..length - 18], b"PK\x05\x06");
    let declared = u16::try_from(comment.len()).expect("comment fits");
    bytes[length - 2..].copy_from_slice(&declared.to_le_bytes());
    bytes.extend_from_slice(comment);
    bytes
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
fn plain_archive_is_sound() {
    assert_eq!(messages(&docx()), Vec::<String>::new());
}

#[test]
fn bytes_after_the_archive_are_reported() {
    let mut bytes = docx();
    bytes.extend_from_slice(b"\r\n\r\n");
    assert_eq!(
        messages(&bytes),
        vec!["4 bytes follow the end of the ZIP archive; Word repairs such a file".to_string()]
    );
}

#[test]
fn a_single_byte_after_the_archive_is_reported() {
    let mut bytes = docx();
    bytes.push(b'\n');
    assert_eq!(
        messages(&bytes),
        vec!["1 byte follows the end of the ZIP archive; Word repairs such a file".to_string()]
    );
}

#[test]
fn bytes_before_the_archive_are_reported() {
    let mut bytes = b"\r\n\r\n".to_vec();
    bytes.extend_from_slice(&docx());
    assert_eq!(
        messages(&bytes),
        vec!["4 bytes precede the ZIP archive; Word repairs such a file".to_string()]
    );
}

/// A document followed by a second copy of itself, as a faulty download can
/// leave it. The file starts with a local file header and ends with an end
/// record, and ZIP readers read the second copy; the first copy is bytes in
/// front of the archive. Checked against Word: it repairs such a file.
#[test]
fn a_document_followed_by_a_copy_of_itself_is_reported() {
    let one = docx();
    let mut bytes = one.clone();
    bytes.extend_from_slice(&one);
    assert_eq!(
        messages(&bytes),
        vec![format!(
            "{} bytes precede the ZIP archive; Word repairs such a file",
            one.len()
        )]
    );
}

#[test]
fn a_declared_zip_comment_is_part_of_the_archive() {
    let bytes = docx_with_zip_comment(b"filed by the harbour office");
    assert_eq!(messages(&bytes), Vec::<String>::new());
}

#[test]
fn bytes_after_a_declared_zip_comment_are_reported() {
    let mut bytes = docx_with_zip_comment(b"filed by the harbour office");
    bytes.extend_from_slice(b"  ");
    assert_eq!(
        messages(&bytes),
        vec!["2 bytes follow the end of the ZIP archive; Word repairs such a file".to_string()]
    );
}
