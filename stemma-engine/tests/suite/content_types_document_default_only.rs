//! Regression: a package whose `word/document.xml` content type is supplied
//! ONLY by `Default Extension="xml"` (with the WML main type), and carries no
//! `Override PartName="/word/document.xml"`, must still export a package that
//! the post-serialization validator accepts.
//!
//! Real-world repro: OpenXmlPowerTools `HtmlConverter01` Test-08 ships this
//! shape. The import scaffold adds the canonical Override, but the cached
//! anchored bytes (the cold `get_doc_bytes` export path) were a verbatim re-zip
//! of the input and shipped the original defect, so `save_docx` failed with
//! `[I-CT-002] WML part "word/document.xml" has no content-type Override`.
//!
//! The fix re-emits the corrected `[Content_Types].xml` into the cached anchored
//! bytes so the exported package and the scaffold package agree. This test
//! drives the full import -> export -> validate path on a self-contained
//! fixture with exactly that Content-Types shape and asserts a clean save.

use std::io::{Cursor, Write};

use stemma::{DocxRuntime, ExportMode, SimpleRuntime, validate_docx_report};
use zip::{ZipWriter, write::FileOptions};

/// Build a minimal, valid DOCX whose `word/document.xml` is content-typed only
/// via `Default Extension="xml"` (pointed at the WML main type) — no Override.
fn fixture_docx_document_default_only() -> Vec<u8> {
    let content_types = r#"<?xml version="1.0" encoding="utf-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml" /><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml" /></Types>"#;

    let root_rels = r#"<?xml version="1.0" encoding="utf-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml" /></Relationships>"#;

    let document_rels = r#"<?xml version="1.0" encoding="utf-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"></Relationships>"#;

    let document = r#"<?xml version="1.0" encoding="utf-8"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Hello</w:t></w:r></w:p><w:sectPr><w:pgSz w:w="12240" w:h="15840"/></w:sectPr></w:body></w:document>"#;

    let mut buf = Vec::new();
    {
        let mut zip = ZipWriter::new(Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        for (name, data) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", root_rels),
            ("word/_rels/document.xml.rels", document_rels),
            ("word/document.xml", document),
        ] {
            zip.start_file(name, opts).expect("start_file");
            zip.write_all(data.as_bytes()).expect("write part");
        }
        zip.finish().expect("finish zip");
    }
    buf
}

#[test]
fn export_adds_document_override_when_only_xml_default_covers_it() {
    let bytes = fixture_docx_document_default_only();

    let runtime = SimpleRuntime::new();
    let import = runtime.import_docx(&bytes).expect("import fixture");

    // This is the path that was broken: the cold export re-zips the cached
    // anchored bytes. Before the fix this produced a package whose
    // `word/document.xml` had no Override.
    let exported = runtime
        .export_docx(&import.doc_handle, ExportMode::Redline)
        .expect("export fixture");

    let report = validate_docx_report(&exported).expect("validate exported bytes");
    let ct_errors: Vec<&str> = report
        .issues
        .iter()
        .filter(|i| i.message.contains("word/document.xml") && i.message.contains("content-type"))
        .map(|i| i.message.as_str())
        .collect();
    assert!(
        ct_errors.is_empty(),
        "exported package must declare a content-type Override for word/document.xml; got: {ct_errors:?}"
    );
    assert!(
        report.ok,
        "exported package must validate clean; issues: {:?}",
        report.issues
    );

    // And the emitted Content-Types must actually carry the Override.
    let emitted_ct = extract_part(&exported, "[Content_Types].xml");
    assert!(
        emitted_ct.contains(r#"PartName="/word/document.xml""#),
        "emitted [Content_Types].xml must add the document.xml Override; got: {emitted_ct}"
    );
}

fn extract_part(docx_bytes: &[u8], part: &str) -> String {
    let mut zip = zip::ZipArchive::new(Cursor::new(docx_bytes)).expect("open exported zip");
    let mut file = zip.by_name(part).expect("part present");
    let mut out = String::new();
    std::io::Read::read_to_string(&mut file, &mut out).expect("read part");
    out
}
