//! Integration tests for the DOCX post-serialization validator.
//!
//! Tests validate both known-good DOCX files and synthetically-constructed
//! bad packages with specific invariant violations.

use std::io::{Cursor, Write};

use zip::ZipWriter;
use zip::write::FileOptions;

use stemma::docx_validate::{ValidationSeverity, validate_docx};

// =============================================================================
// Helpers
// =============================================================================

/// Build a ZIP file from a list of (name, content) pairs.
fn build_zip(parts: &[(&str, &[u8])]) -> Vec<u8> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = FileOptions::default();
    for (name, data) in parts {
        writer.start_file(*name, options).unwrap();
        writer.write_all(data).unwrap();
    }
    let cursor = writer.finish().unwrap();
    cursor.into_inner()
}

/// A minimal valid DOCX package for testing.
fn minimal_valid_docx() -> Vec<u8> {
    let content_types = br#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

    let rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

    let doc_rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
</Relationships>"#;

    let document = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:r><w:t>Hello</w:t></w:r></w:p>
    <w:sectPr/>
  </w:body>
</w:document>"#;

    build_zip(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", rels),
        ("word/_rels/document.xml.rels", doc_rels),
        ("word/document.xml", document),
    ])
}

// =============================================================================
// Known-good DOCX from testdata
// =============================================================================

#[test]
fn validate_known_good_docx_simple_text() {
    let bytes = std::fs::read("testdata/simple-text/before.docx")
        .expect("read testdata/simple-text/before.docx");
    let result = validate_docx(&bytes);
    let errors: Vec<_> = result.errors().collect();
    assert!(
        errors.is_empty(),
        "known-good DOCX should have 0 errors, got {} errors:\n{}",
        errors.len(),
        errors
            .iter()
            .map(|f| f.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn validate_known_good_docx_paragraphs() {
    let bytes = std::fs::read("testdata/paragraphs/before.docx")
        .expect("read testdata/paragraphs/before.docx");
    let result = validate_docx(&bytes);
    let errors: Vec<_> = result.errors().collect();
    assert!(
        errors.is_empty(),
        "known-good DOCX should have 0 errors, got {} errors:\n{}",
        errors.len(),
        errors
            .iter()
            .map(|f| f.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn validate_minimal_valid_docx_has_no_errors() {
    let bytes = minimal_valid_docx();
    let result = validate_docx(&bytes);
    let errors: Vec<_> = result.errors().collect();
    assert!(
        errors.is_empty(),
        "minimal valid DOCX should have 0 errors, got {} errors:\n{}",
        errors.len(),
        errors
            .iter()
            .map(|f| f.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
}

// =============================================================================
// I-PKG-001: _rels/.rels must exist
// =============================================================================

#[test]
fn pkg_001_missing_rels() {
    let content_types = br#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="xml" ContentType="application/xml"/>
</Types>"#;

    let document = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body><w:p/></w:body>
</w:document>"#;

    let bytes = build_zip(&[
        ("[Content_Types].xml", content_types),
        ("word/document.xml", document),
    ]);
    let result = validate_docx(&bytes);
    let pkg_001: Vec<_> = result
        .findings
        .iter()
        .filter(|f| f.rule_id == "I-PKG-001")
        .collect();
    assert_eq!(pkg_001.len(), 1, "expected exactly 1 I-PKG-001 finding");
    assert_eq!(pkg_001[0].severity, ValidationSeverity::Error);
}

// =============================================================================
// I-PKG-002: word/document.xml must exist
// =============================================================================

#[test]
fn pkg_002_missing_document() {
    let content_types = br#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
</Types>"#;

    let rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

    let bytes = build_zip(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", rels),
    ]);
    let result = validate_docx(&bytes);
    let pkg_002: Vec<_> = result
        .findings
        .iter()
        .filter(|f| f.rule_id == "I-PKG-002")
        .collect();
    assert_eq!(pkg_002.len(), 1, "expected exactly 1 I-PKG-002 finding");
    assert_eq!(pkg_002[0].severity, ValidationSeverity::Error);
}

// =============================================================================
// I-CT-001: Every part must have a content type
// =============================================================================

#[test]
fn ct_001_missing_content_type() {
    // A package with a .png file but no Default for "png" and no Override.
    let content_types = br#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

    let rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

    let doc_rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png" />
</Relationships>"#;

    let document = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <w:body>
    <w:p><w:r><w:drawing r:embed="rId1"/></w:r></w:p>
  </w:body>
</w:document>"#;

    let bytes = build_zip(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", rels),
        ("word/_rels/document.xml.rels", doc_rels),
        ("word/document.xml", document),
        ("word/media/image1.png", b"fake png data"),
    ]);
    let result = validate_docx(&bytes);
    let ct_001: Vec<_> = result
        .findings
        .iter()
        .filter(|f| f.rule_id == "I-CT-001")
        .collect();
    assert_eq!(ct_001.len(), 1, "expected exactly 1 I-CT-001 finding");
    assert!(ct_001[0].message.contains("image1.png"));
    assert_eq!(ct_001[0].severity, ValidationSeverity::Error);
}

#[test]
fn ct_001_default_extension_covers_part() {
    // A package with a .png file AND a Default extension for "png" — should pass.
    let content_types = br#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Default Extension="png" ContentType="image/png"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

    let rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

    let doc_rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png" />
</Relationships>"#;

    let document = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <w:body>
    <w:p><w:r><w:drawing r:embed="rId1"/></w:r></w:p>
  </w:body>
</w:document>"#;

    let bytes = build_zip(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", rels),
        ("word/_rels/document.xml.rels", doc_rels),
        ("word/document.xml", document),
        ("word/media/image1.png", b"fake png data"),
    ]);
    let result = validate_docx(&bytes);
    let ct_001: Vec<_> = result
        .findings
        .iter()
        .filter(|f| f.rule_id == "I-CT-001")
        .collect();
    assert!(
        ct_001.is_empty(),
        "default extension should cover .png part"
    );
}

#[test]
fn ct_001_missing_content_types_xml() {
    // Package with no [Content_Types].xml at all.
    let rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

    let document = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body><w:p/></w:body>
</w:document>"#;

    let bytes = build_zip(&[("_rels/.rels", rels), ("word/document.xml", document)]);
    let result = validate_docx(&bytes);
    let ct_001: Vec<_> = result
        .findings
        .iter()
        .filter(|f| f.rule_id == "I-CT-001")
        .collect();
    assert_eq!(ct_001.len(), 1, "should report missing [Content_Types].xml");
    assert_eq!(ct_001[0].severity, ValidationSeverity::Error);
}

// =============================================================================
// I-REL-001: Every r:id/r:embed/r:link reference resolves
// =============================================================================

#[test]
fn rel_001_broken_rid_reference() {
    let content_types = br#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
</Types>"#;

    let rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

    // document.xml references rId99 which does not exist in document.xml.rels
    let document = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <w:body>
    <w:p><w:hyperlink r:id="rId99"><w:r><w:t>link</w:t></w:r></w:hyperlink></w:p>
  </w:body>
</w:document>"#;

    let doc_rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
</Relationships>"#;

    let bytes = build_zip(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", rels),
        ("word/_rels/document.xml.rels", doc_rels),
        ("word/document.xml", document),
    ]);
    let result = validate_docx(&bytes);
    let rel_001: Vec<_> = result
        .findings
        .iter()
        .filter(|f| f.rule_id == "I-REL-001")
        .collect();
    assert_eq!(rel_001.len(), 1, "expected exactly 1 I-REL-001 finding");
    assert!(rel_001[0].message.contains("rId99"));
    assert_eq!(rel_001[0].severity, ValidationSeverity::Error);
}

#[test]
fn rel_001_valid_rid_reference_no_error() {
    let content_types = br#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
</Types>"#;

    let rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

    let document = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <w:body>
    <w:p><w:hyperlink r:id="rId1"><w:r><w:t>link</w:t></w:r></w:hyperlink></w:p>
  </w:body>
</w:document>"#;

    let doc_rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.com" TargetMode="External"/>
</Relationships>"#;

    let bytes = build_zip(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", rels),
        ("word/_rels/document.xml.rels", doc_rels),
        ("word/document.xml", document),
    ]);
    let result = validate_docx(&bytes);
    let rel_001: Vec<_> = result
        .findings
        .iter()
        .filter(|f| f.rule_id == "I-REL-001")
        .collect();
    assert!(
        rel_001.is_empty(),
        "valid r:id reference should not produce I-REL-001"
    );
}

#[test]
fn rel_001_no_rels_file_for_part_with_references() {
    let content_types = br#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
</Types>"#;

    let rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

    // document.xml references rId1 but there is NO word/_rels/document.xml.rels
    let document = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <w:body>
    <w:p><w:hyperlink r:id="rId1"><w:r><w:t>link</w:t></w:r></w:hyperlink></w:p>
  </w:body>
</w:document>"#;

    let bytes = build_zip(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", rels),
        ("word/document.xml", document),
    ]);
    let result = validate_docx(&bytes);
    let rel_001: Vec<_> = result
        .findings
        .iter()
        .filter(|f| f.rule_id == "I-REL-001")
        .collect();
    assert_eq!(
        rel_001.len(),
        1,
        "expected I-REL-001 when .rels file is missing"
    );
    assert!(rel_001[0].message.contains("rId1"));
}

// =============================================================================
// I-REL-002: Relationship ID uniqueness
// =============================================================================

#[test]
fn rel_002_duplicate_ids() {
    let content_types = br#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
</Types>"#;

    let rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

    // Duplicate rId1 in document.xml.rels
    let doc_rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/>
</Relationships>"#;

    let document = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body><w:p/></w:body>
</w:document>"#;

    let bytes = build_zip(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", rels),
        ("word/_rels/document.xml.rels", doc_rels),
        ("word/document.xml", document),
    ]);
    let result = validate_docx(&bytes);
    let rel_002: Vec<_> = result
        .findings
        .iter()
        .filter(|f| f.rule_id == "I-REL-002")
        .collect();
    assert_eq!(rel_002.len(), 1, "expected exactly 1 I-REL-002 finding");
    assert!(rel_002[0].message.contains("rId1"));
    assert_eq!(rel_002[0].severity, ValidationSeverity::Error);
}

// =============================================================================
// I-REL-003: Internal relationship targets resolve
// =============================================================================

#[test]
fn rel_003_missing_internal_target() {
    let content_types = br#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
</Types>"#;

    let rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

    // Points to styles.xml which does not exist in the package
    let doc_rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
</Relationships>"#;

    let document = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body><w:p/></w:body>
</w:document>"#;

    let bytes = build_zip(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", rels),
        ("word/_rels/document.xml.rels", doc_rels),
        ("word/document.xml", document),
    ]);
    let result = validate_docx(&bytes);
    let rel_003: Vec<_> = result
        .findings
        .iter()
        .filter(|f| f.rule_id == "I-REL-003")
        .collect();
    assert_eq!(rel_003.len(), 1, "expected exactly 1 I-REL-003 finding");
    assert!(rel_003[0].message.contains("styles.xml"));
    assert_eq!(rel_003[0].severity, ValidationSeverity::Error);
}

#[test]
fn rel_003_external_target_not_checked() {
    let content_types = br#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
</Types>"#;

    let rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

    // External hyperlink — should NOT be checked for existence in ZIP
    let doc_rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.com" TargetMode="External"/>
</Relationships>"#;

    let document = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body><w:p/></w:body>
</w:document>"#;

    let bytes = build_zip(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", rels),
        ("word/_rels/document.xml.rels", doc_rels),
        ("word/document.xml", document),
    ]);
    let result = validate_docx(&bytes);
    let rel_003: Vec<_> = result
        .findings
        .iter()
        .filter(|f| f.rule_id == "I-REL-003")
        .collect();
    assert!(
        rel_003.is_empty(),
        "external targets should not be checked for existence"
    );
}

// =============================================================================
// I-STORY-001: Story parts have corresponding relationships
// =============================================================================

#[test]
fn story_001_orphan_header() {
    let content_types = br#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
</Types>"#;

    let rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

    // document.xml.rels has NO header relationship, but header1.xml exists
    let doc_rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
</Relationships>"#;

    let document = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body><w:p/></w:body>
</w:document>"#;

    let header = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:p><w:r><w:t>Header</w:t></w:r></w:p>
</w:hdr>"#;

    let bytes = build_zip(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", rels),
        ("word/_rels/document.xml.rels", doc_rels),
        ("word/document.xml", document),
        ("word/header1.xml", header),
    ]);
    let result = validate_docx(&bytes);
    let story_001: Vec<_> = result
        .findings
        .iter()
        .filter(|f| f.rule_id == "I-STORY-001")
        .collect();
    assert_eq!(story_001.len(), 1, "expected exactly 1 I-STORY-001 finding");
    assert!(story_001[0].message.contains("header1.xml"));
    assert_eq!(story_001[0].severity, ValidationSeverity::Warning);
}

#[test]
fn story_001_linked_header_no_warning() {
    let content_types = br#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
</Types>"#;

    let rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

    let doc_rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/>
</Relationships>"#;

    let document = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body><w:p/></w:body>
</w:document>"#;

    let header = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:p><w:r><w:t>Header</w:t></w:r></w:p>
</w:hdr>"#;

    let bytes = build_zip(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", rels),
        ("word/_rels/document.xml.rels", doc_rels),
        ("word/document.xml", document),
        ("word/header1.xml", header),
    ]);
    let result = validate_docx(&bytes);
    let story_001: Vec<_> = result
        .findings
        .iter()
        .filter(|f| f.rule_id == "I-STORY-001")
        .collect();
    assert!(
        story_001.is_empty(),
        "header with matching relationship should not produce I-STORY-001"
    );
}

// =============================================================================
// I-PEOPLE-001: people.xml has relationship if present
// =============================================================================

#[test]
fn people_001_orphan_people_xml() {
    let content_types = br#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
</Types>"#;

    let rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

    let doc_rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
</Relationships>"#;

    let document = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body><w:p/></w:body>
</w:document>"#;

    let people = br#"<?xml version="1.0" encoding="UTF-8"?>
<w15:people xmlns:w15="http://schemas.microsoft.com/office/word/2012/wordml">
  <w15:person w15:author="Test Author"/>
</w15:people>"#;

    let bytes = build_zip(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", rels),
        ("word/_rels/document.xml.rels", doc_rels),
        ("word/document.xml", document),
        ("word/people.xml", people),
    ]);
    let result = validate_docx(&bytes);
    let people_001: Vec<_> = result
        .findings
        .iter()
        .filter(|f| f.rule_id == "I-PEOPLE-001")
        .collect();
    assert_eq!(
        people_001.len(),
        1,
        "expected exactly 1 I-PEOPLE-001 finding"
    );
    assert_eq!(people_001[0].severity, ValidationSeverity::Warning);
}

#[test]
fn people_001_linked_people_no_warning() {
    let content_types = br#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
</Types>"#;

    let rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

    let doc_rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId5" Type="http://schemas.microsoft.com/office/2011/relationships/people" Target="people.xml"/>
</Relationships>"#;

    let document = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body><w:p/></w:body>
</w:document>"#;

    let people = br#"<?xml version="1.0" encoding="UTF-8"?>
<w15:people xmlns:w15="http://schemas.microsoft.com/office/word/2012/wordml">
  <w15:person w15:author="Test Author"/>
</w15:people>"#;

    let bytes = build_zip(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", rels),
        ("word/_rels/document.xml.rels", doc_rels),
        ("word/document.xml", document),
        ("word/people.xml", people),
    ]);
    let result = validate_docx(&bytes);
    let people_001: Vec<_> = result
        .findings
        .iter()
        .filter(|f| f.rule_id == "I-PEOPLE-001")
        .collect();
    assert!(
        people_001.is_empty(),
        "people.xml with matching relationship should not produce I-PEOPLE-001"
    );
}

// =============================================================================
// Edge cases
// =============================================================================

#[test]
fn invalid_zip_produces_error() {
    let result = validate_docx(b"this is not a zip file");
    assert!(result.has_errors());
    let pkg_000: Vec<_> = result
        .findings
        .iter()
        .filter(|f| f.rule_id == "I-PKG-000")
        .collect();
    assert_eq!(pkg_000.len(), 1, "should report ZIP open failure");
}

#[test]
fn multiple_violations_all_reported() {
    // A package with multiple problems: no _rels/.rels, no document.xml, no content types.
    let bytes = build_zip(&[("word/styles.xml", b"<styles/>")]);
    let result = validate_docx(&bytes);

    let rule_ids: Vec<&str> = result.findings.iter().map(|f| f.rule_id).collect();
    assert!(
        rule_ids.contains(&"I-PKG-001"),
        "should report missing _rels/.rels"
    );
    assert!(
        rule_ids.contains(&"I-PKG-002"),
        "should report missing word/document.xml"
    );
    assert!(
        rule_ids.contains(&"I-CT-001"),
        "should report missing [Content_Types].xml"
    );
}

// =============================================================================
// I-XML-001: a part that fails to parse is a hard Error, never a silent skip
// =============================================================================
//
// The validator's story/aux checks all operate on parsed trees. If a part is
// not well-formed XML, the part-level checks cannot run — so the parse failure
// itself MUST be an Error finding (and a blocking one), otherwise a corrupt
// part validates "clean" precisely because it is corrupt. That is the
// "continuing in an unknown state" failure mode the validator exists to stop.

/// Build a minimal package where one named part's bytes are overridden.
fn minimal_docx_with_part(part_name: &str, content: &[u8]) -> Vec<u8> {
    let content_types = br#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
  <Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/>
  <Override PartName="/word/footnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"/>
</Types>"#;
    let rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;
    let doc_rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes" Target="footnotes.xml"/>
  <Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
</Relationships>"#;
    let document: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:r><w:t>Hello</w:t></w:r></w:p>
    <w:sectPr/>
  </w:body>
</w:document>"#;
    let styles: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"/>"#;
    let footnotes: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"/>"#;

    let mut parts: Vec<(&str, &[u8])> = vec![
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", rels),
        ("word/_rels/document.xml.rels", doc_rels),
        ("word/document.xml", document),
        ("word/styles.xml", styles),
        ("word/footnotes.xml", footnotes),
    ];
    for (name, data) in &mut parts {
        if *name == part_name {
            *data = content;
        }
    }
    build_zip(&parts)
}

fn assert_xml_001_error(bytes: &[u8], part: &str) {
    let result = validate_docx(bytes);
    let xml_001: Vec<_> = result
        .findings
        .iter()
        .filter(|f| f.rule_id == "I-XML-001")
        .collect();
    assert!(
        xml_001
            .iter()
            .any(|f| f.location == part && f.severity == ValidationSeverity::Error),
        "corrupt {part} must produce an I-XML-001 Error finding at that location, got: {:?}",
        result.findings
    );
    assert!(result.has_errors());
}

#[test]
fn corrupt_document_xml_is_an_error_finding() {
    // Truncated mid-element: the exact shape a partial write produces.
    let bytes = minimal_docx_with_part(
        "word/document.xml",
        b"<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body><w:p>",
    );
    assert_xml_001_error(&bytes, "word/document.xml");
}

#[test]
fn corrupt_footnotes_xml_is_an_error_finding() {
    let bytes = minimal_docx_with_part("word/footnotes.xml", b"<w:footnotes><not-closed");
    assert_xml_001_error(&bytes, "word/footnotes.xml");
}

#[test]
fn corrupt_styles_xml_is_an_error_finding() {
    let bytes = minimal_docx_with_part("word/styles.xml", b"\x00\x01 not xml at all");
    assert_xml_001_error(&bytes, "word/styles.xml");
}

// =============================================================================
// The Blocking gate refuses structurally unusable output
// =============================================================================

#[test]
fn blocking_gate_refuses_corrupt_story_part() {
    use stemma::runtime::{ValidatorLevel, gate_serialized_bytes};
    let bytes = minimal_docx_with_part(
        "word/document.xml",
        b"<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body><w:p>",
    );
    let err = gate_serialized_bytes(&bytes, ValidatorLevel::Blocking)
        .expect_err("a package whose document.xml does not parse must not pass the Blocking gate");
    assert!(
        err.message.contains("I-XML-001"),
        "gate error should name the rule, got: {}",
        err.message
    );
}

#[test]
fn blocking_gate_refuses_dangling_relationship_reference() {
    use stemma::runtime::{ValidatorLevel, gate_serialized_bytes};
    // A hyperlink pointing at rId99 with no such relationship: Word repairs the
    // file and drops content — a data-loss class, so Blocking must refuse.
    let document: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <w:body>
    <w:p><w:hyperlink r:id="rId99"><w:r><w:t>link</w:t></w:r></w:hyperlink></w:p>
    <w:sectPr/>
  </w:body>
</w:document>"#;
    let bytes = minimal_docx_with_part("word/document.xml", document);
    let err = gate_serialized_bytes(&bytes, ValidatorLevel::Blocking)
        .expect_err("a dangling r:id must not pass the Blocking gate");
    assert!(
        err.message.contains("I-REL-001"),
        "gate error should name the rule, got: {}",
        err.message
    );
}

#[test]
fn default_export_options_validate_at_blocking() {
    use stemma::runtime::{ExportOptions, ValidatorLevel};
    // The default path is the contract: bytes do not leave the engine unchecked.
    // Skipping validation (ValidatorLevel::Off) is an explicit caller decision.
    assert_eq!(
        ExportOptions::default().validator_level,
        ValidatorLevel::Blocking
    );
}

// =============================================================================
// I-ORD-*: Annex A ordering violations are Error severity (Full gate refuses)
// =============================================================================

#[test]
fn ordering_violation_is_error_severity_and_full_gate_refuses() {
    use stemma::runtime::{ValidatorLevel, gate_serialized_bytes};
    // w:jc before w:pStyle violates the CT_PPr xsd:sequence — invalid OOXML.
    let document: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:pPr><w:jc w:val="center"/><w:pStyle w:val="Heading1"/></w:pPr>
      <w:r><w:t>Hello</w:t></w:r>
    </w:p>
    <w:sectPr/>
  </w:body>
</w:document>"#;
    let bytes = minimal_docx_with_part("word/document.xml", document);

    let result = validate_docx(&bytes);
    let ord: Vec<_> = result
        .findings
        .iter()
        .filter(|f| f.rule_id.starts_with("I-ORD"))
        .collect();
    assert!(
        !ord.is_empty(),
        "mis-ordered pPr must produce an I-ORD finding"
    );
    for f in &ord {
        assert_eq!(
            f.severity,
            ValidationSeverity::Error,
            "Annex A ordering is normative (xsd:sequence) — Error, not advisory: {f}"
        );
    }

    // Blocking tolerates it (Word repairs ordering quietly); Full must refuse.
    gate_serialized_bytes(&bytes, ValidatorLevel::Blocking)
        .expect("ordering alone is not a Blocking-class defect");
    gate_serialized_bytes(&bytes, ValidatorLevel::Full)
        .expect_err("Full gate must refuse on any error-severity finding");
}

// =============================================================================
// I-REL-003: fragment-only targets are same-part references, not part names
// =============================================================================

#[test]
fn rel_003_fragment_target_is_not_dangling() {
    // Old Word files emit bookmark hyperlinks as relationships whose Target is
    // a bare fragment ("#BookmarkName"). Per RFC 3986 a fragment-only reference
    // resolves to the source part itself — it never names a package part, so
    // I-REL-003 must not try to resolve it as a path.
    let content_types = br#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;
    let rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;
    let doc_rels = br##"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="#_SomeBookmark"/>
</Relationships>"##;
    let document = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <w:body>
    <w:p><w:hyperlink r:id="rId9"><w:r><w:t>go</w:t></w:r></w:hyperlink></w:p>
    <w:sectPr/>
  </w:body>
</w:document>"#;
    let bytes = build_zip(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", rels),
        ("word/_rels/document.xml.rels", doc_rels),
        ("word/document.xml", document),
    ]);
    let result = validate_docx(&bytes);
    let rel_003: Vec<_> = result
        .findings
        .iter()
        .filter(|f| f.rule_id == "I-REL-003")
        .collect();
    assert!(
        rel_003.is_empty(),
        "fragment-only target must not be flagged as dangling: {rel_003:?}"
    );
}

// =============================================================================
// I-CT-002: all four legitimate WML main-part content types are canonical
// =============================================================================

#[test]
fn ct_002_template_main_content_type_is_accepted() {
    // A .docx whose main part is content-typed as a template (or macro-enabled
    // variant) is a legitimate Word document — Word opens it as a template.
    // Only a content type OUTSIDE the WML main-part family is a defect.
    let content_types = br#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.template.main+xml"/>
</Types>"#;
    let rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;
    let doc_rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
</Relationships>"#;
    let document = br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body><w:p><w:r><w:t>Hello</w:t></w:r></w:p><w:sectPr/></w:body>
</w:document>"#;
    let bytes = build_zip(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", rels),
        ("word/_rels/document.xml.rels", doc_rels),
        ("word/document.xml", document),
    ]);
    let result = validate_docx(&bytes);
    let ct_002: Vec<_> = result
        .findings
        .iter()
        .filter(|f| f.rule_id == "I-CT-002")
        .collect();
    assert!(
        ct_002.is_empty(),
        "template.main+xml is a legitimate main-part content type: {ct_002:?}"
    );
}
