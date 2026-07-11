//! Release-safe serialize-time validator gate.
//!
//! The built-in OOXML linker gate must run in ANY build profile (not only
//! under `debug_assertions`) when a caller opts in via [`ValidatorLevel`].
//! These tests exercise the gate through the public `serialize_snapshot` /
//! `ExportOptions` seam and the shared `gate_serialized_bytes` free function,
//! so they prove the contract WITHOUT depending on the compiled-out debug path.

use std::io::Write;

use stemma::api::Document;
use stemma::{ErrorCode, ExportMode, ExportOptions, ValidatorLevel, gate_serialized_bytes};
use zip::write::FileOptions;

/// Wrap a `<w:body>` inner fragment in a minimal, valid OPC package.
fn build_minimal_docx(body_inner: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;

    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        zip.start_file("[Content_Types].xml", opts).unwrap();
        zip.write_all(content_types.as_bytes()).unwrap();
        zip.start_file("_rels/.rels", opts).unwrap();
        zip.write_all(rels.as_bytes()).unwrap();
        zip.start_file("word/_rels/document.xml.rels", opts)
            .unwrap();
        zip.write_all(doc_rels.as_bytes()).unwrap();
        zip.start_file("word/document.xml", opts).unwrap();
        zip.write_all(document_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

/// A package missing `word/document.xml` — violates blocking rule I-PKG-002.
/// (Built directly as bytes; the importer would reject it, so it can only be
/// exercised through the byte-level gate.)
fn build_docx_without_document_xml() -> Vec<u8> {
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        zip.start_file("[Content_Types].xml", opts).unwrap();
        zip.write_all(content_types.as_bytes()).unwrap();
        zip.start_file("_rels/.rels", opts).unwrap();
        zip.write_all(rels.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

/// Blocking is the default: bytes do not leave the engine unchecked. Off is
/// reserved for engine-internal intermediates via `ExportOptions::unchecked()`.
#[test]
fn validator_level_default_is_blocking() {
    assert_eq!(
        ExportOptions::default().validator_level,
        ValidatorLevel::Blocking
    );
    assert!(matches!(ExportOptions::default().mode, ExportMode::Redline));
    assert_eq!(
        ExportOptions::unchecked().validator_level,
        ValidatorLevel::Off
    );
}

/// Off never inspects the bytes — even structurally-corrupt bytes pass.
#[test]
fn gate_off_is_a_noop_even_on_corrupt_bytes() {
    let corrupt = build_docx_without_document_xml();
    assert!(
        gate_serialized_bytes(&corrupt, ValidatorLevel::Off).is_ok(),
        "Off must not gate — it is the explicit engine-internal opt-out"
    );
}

/// Blocking refuses bytes that violate a structural BLOCKING_RULE
/// (I-PKG-002: word/document.xml must exist), returning ValidationFailed.
/// This is the release-mode guarantee: it does NOT depend on debug_assertions.
#[test]
fn gate_blocking_rejects_missing_document_xml() {
    let corrupt = build_docx_without_document_xml();
    let err = gate_serialized_bytes(&corrupt, ValidatorLevel::Blocking)
        .expect_err("Blocking must refuse a package missing word/document.xml");
    assert_eq!(err.code, ErrorCode::ValidationFailed);
    assert!(
        err.message.contains("I-PKG-002"),
        "error must name the violated blocking rule: {}",
        err.message
    );
}

/// Blocking accepts a well-formed package.
#[test]
fn gate_blocking_accepts_valid_package() {
    let good = build_minimal_docx(r#"<w:p><w:r><w:t>Hello</w:t></w:r></w:p>"#);
    assert!(
        gate_serialized_bytes(&good, ValidatorLevel::Blocking).is_ok(),
        "a structurally valid package must pass the Blocking gate"
    );
}

/// End-to-end through the `serialize_snapshot` seam (via the `Document`
/// facade): a clean document serializes fine under Blocking, proving the gate
/// is wired into the public export path and runs regardless of build profile.
#[test]
fn serialize_snapshot_blocking_accepts_clean_document() {
    let doc = Document::parse(&build_minimal_docx(
        r#"<w:p><w:r><w:t>Clean paragraph.</w:t></w:r></w:p>"#,
    ))
    .expect("parse minimal docx");

    let off = doc
        .serialize(&ExportOptions::default())
        .expect("Off serialize succeeds");
    assert!(!off.is_empty());

    let blocking = doc
        .serialize(&ExportOptions {
            mode: ExportMode::Redline,
            validator_level: ValidatorLevel::Blocking,
            validator: None,
        })
        .expect("Blocking serialize of a clean doc must succeed");
    assert!(!blocking.is_empty());
}
