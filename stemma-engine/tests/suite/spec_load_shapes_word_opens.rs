//! Shapes the schema does not allow that Word opens without repair, and
//! how it reads them. Each was checked against Word on these documents.
//!
//! - A tracked insertion or deletion with no `w:id`, or no `w:author`
//!   (both required by the schema): shown as a tracked change. On save Word
//!   numbers the change itself, and a change that had no author is written
//!   back with none.
//! - An insertion inside an insertion, or a deletion inside a deletion: one
//!   change, the inner one.
//! - An empty element Word does not know, in a paragraph: contributes
//!   nothing.

use std::io::{Cursor, Write};

use stemma::api::Document;
use stemma::{ExportOptions, Resolution};
use zip::{ZipWriter, write::FileOptions};

fn docx(body: &str) -> Vec<u8> {
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
    let mut buf = Vec::new();
    {
        let mut zip = ZipWriter::new(Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        for (name, text) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/document.xml", document.as_str()),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(text.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

const DATE: &str = r#"w:date="2026-01-05T10:00:00Z""#;

fn inserted(attributes: &str) -> Vec<u8> {
    docx(&format!(
        r#"<w:p><w:r><w:t xml:space="preserve">The pilot boat leaves </w:t></w:r><w:ins {attributes}><w:r><w:t>early</w:t></w:r></w:ins></w:p>"#
    ))
}

fn deleted(attributes: &str) -> Vec<u8> {
    docx(&format!(
        r#"<w:p><w:r><w:t xml:space="preserve">The pilot boat leaves </w:t></w:r><w:del {attributes}><w:r><w:delText>early</w:delText></w:r></w:del></w:p>"#
    ))
}

fn text(doc: &Document, resolution: Resolution) -> String {
    doc.project(resolution)
        .expect("project")
        .read()
        .blocks
        .iter()
        .map(|block| block.text.clone())
        .collect::<Vec<_>>()
        .join("\n")
}

fn cases() -> Vec<(&'static str, Vec<u8>, &'static str, &'static str)> {
    let with = "The pilot boat leaves early";
    let without = "The pilot boat leaves ";
    vec![
        (
            "insertion, no id",
            inserted(&format!(r#"w:author="Harbour Office" {DATE}"#)),
            with,
            without,
        ),
        (
            "insertion, no author",
            inserted(&format!(r#"w:id="5" {DATE}"#)),
            with,
            without,
        ),
        ("insertion, neither", inserted(""), with, without),
        (
            "deletion, no id",
            deleted(&format!(r#"w:author="Harbour Office" {DATE}"#)),
            without,
            with,
        ),
        (
            "deletion, no author",
            deleted(&format!(r#"w:id="5" {DATE}"#)),
            without,
            with,
        ),
    ]
}

#[test]
fn the_change_loads_as_a_tracked_change() {
    for (name, bytes, accepted, rejected) in cases() {
        let doc = Document::parse(&bytes).unwrap_or_else(|error| panic!("{name}: {error:?}"));
        assert_eq!(text(&doc, Resolution::AcceptAll), accepted, "{name}");
        assert_eq!(text(&doc, Resolution::RejectAll), rejected, "{name}");
    }
}

#[test]
fn the_document_is_sound() {
    for (name, bytes, _, _) in cases() {
        let report = stemma::api::validate(&bytes);
        assert!(report.ok, "{name}: {:?}", report.issues);
    }
}

/// Checked against Word: the untouched save of each of these opens without
/// repair.
#[test]
fn an_untouched_document_saves_and_reopens_to_the_same_readings() {
    for (name, bytes, accepted, rejected) in cases() {
        let saved = Document::parse(&bytes)
            .unwrap()
            .serialize(&ExportOptions::default())
            .unwrap_or_else(|error| panic!("{name}: {error:?}"));
        let reopened = Document::parse(&saved).unwrap();
        assert_eq!(text(&reopened, Resolution::AcceptAll), accepted, "{name}");
        assert_eq!(text(&reopened, Resolution::RejectAll), rejected, "{name}");
    }
}

// --- same-type nesting ----------------------------------------------------------

/// An insertion inside an insertion, a deletion inside a deletion. Checked
/// against Word: it opens the document without repair and reads the content
/// as one change, the inner one; on save it writes only the inner wrapper,
/// with the inner author and date.
fn nested(container: &str, text_element: &str) -> Vec<u8> {
    docx(&format!(
        r#"<w:p><w:r><w:t xml:space="preserve">The pilot boat leaves </w:t></w:r><w:{container} w:id="1" w:author="Harbour Office" w:date="2026-01-05T10:00:00Z"><w:{container} w:id="2" w:author="Pilot Station" w:date="2026-01-06T10:00:00Z"><w:r><w:{text_element}>early</w:{text_element}></w:r></w:{container}></w:{container}></w:p>"#
    ))
}

#[test]
fn same_type_nesting_is_one_change_by_the_inner_author() {
    for (container, text_element, kind) in [("ins", "t", "insert"), ("del", "delText", "delete")] {
        let bytes = nested(container, text_element);
        let doc = Document::parse(&bytes).unwrap_or_else(|error| panic!("{container}: {error:?}"));
        let revisions = doc.revisions();
        assert_eq!(revisions.len(), 1, "{container}: {revisions:?}");
        let described = format!("{:?}", revisions[0]);
        assert!(
            described.contains("Pilot Station"),
            "{container}: {described}"
        );
        assert!(
            !described.contains("Harbour Office"),
            "{container}: {described}"
        );
        assert!(
            described.to_lowercase().contains(kind),
            "{container}: {described}"
        );
        let report = stemma::api::validate(&bytes);
        assert!(report.ok, "{container}: {:?}", report.issues);
        let saved = doc
            .serialize(&ExportOptions::default())
            .unwrap_or_else(|error| panic!("{container}: {error:?}"));
        Document::parse(&saved).expect("the untouched save reopens");
    }
}

// --- an unknown element in a paragraph ----------------------------------------

/// Checked against Word: an empty element it does not know, in the
/// WordprocessingML namespace, opens without repair and contributes nothing.
#[test]
fn an_empty_unknown_element_in_a_paragraph_loads_and_is_kept() {
    let bytes = docx(
        r#"<w:p><w:notAnElement w:val="1"/><w:r><w:t>The pilot boat leaves early.</w:t></w:r></w:p>"#,
    );
    let doc = Document::parse(&bytes).expect("an empty unknown element loads");
    assert_eq!(
        text(&doc, Resolution::AcceptAll),
        "The pilot boat leaves early."
    );
    assert!(stemma::api::validate(&bytes).ok);
    let saved = doc.serialize(&ExportOptions::default()).expect("save");
    Document::parse(&saved).expect("the untouched save reopens");
}

/// Word unwraps an unknown element that holds runs and shows their text.
/// That reading is not modeled, so the document is refused, not loaded with
/// the text hidden.
#[test]
fn an_unknown_element_wrapping_runs_is_refused() {
    let bytes = docx(
        r#"<w:p><w:notAnElement><w:r><w:t>inner </w:t></w:r></w:notAnElement><w:r><w:t>outer</w:t></w:r></w:p>"#,
    );
    let error = Document::parse(&bytes).err().expect("not modeled");
    assert!(format!("{error:?}").contains("notAnElement"), "{error:?}");
}
