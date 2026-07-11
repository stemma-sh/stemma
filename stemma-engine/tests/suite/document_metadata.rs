//! Package-level document-metadata invariants (daily tier).
//!
//! `set_core_property` / `set_custom_property` mutate `docProps/core.xml` /
//! `docProps/custom.xml` (§15.2.12). They are **untracked, package-level**
//! operations — NOT edit transactions. The contract this file pins:
//!
//! 1. setting title/author/custom, reserializing, and reparsing surfaces the
//!    value;
//! 2. the body (`word/document.xml`) is byte-unchanged and carries NO
//!    `w:ins`/`w:del`/`w:pPrChange` — metadata is not a tracked change;
//! 3. `docprops` parse -> serialize -> parse is an identity;
//! 4. a malformed part is a hard `DocPropsError`, never an empty-object
//!    fallback.

use stemma::api::Document;
use stemma::docprops::{CoreProperties, CustomProperties, DocPropsError};
use stemma::{ExportOptions, RuntimeError};

/// A DOCX carrying a `docProps/core.xml` core-properties part plus a body.
fn make_docx_with_core(core_xml: &str) -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Body text.</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;

    use std::io::Write;
    use zip::write::FileOptions;
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
        zip.start_file("docProps/core.xml", opts).unwrap();
        zip.write_all(core_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

const SAMPLE_CORE: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:dcmitype="http://purl.org/dc/dcmitype/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><dc:creator>Original Author</dc:creator></cp:coreProperties>"#;

/// Read one part's bytes from a serialized DOCX.
fn part_bytes(docx: &[u8], name: &str) -> Option<Vec<u8>> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(docx)).expect("zip");
    let mut file = zip.by_name(name).ok()?;
    use std::io::Read;
    let mut data = Vec::new();
    file.read_to_end(&mut data).expect("read part");
    Some(data)
}

#[test]
fn set_title_and_author_visible_after_round_trip() {
    let base = Document::parse(&make_docx_with_core(SAMPLE_CORE)).expect("parse");

    let edited = base
        .set_core_property("title", "Master Services Agreement")
        .expect("set title")
        .set_core_property("author", "Jane Counsel")
        .expect("set author");

    // Read back through the typed API.
    assert_eq!(
        edited.core_property("title").unwrap().as_deref(),
        Some("Master Services Agreement")
    );
    assert_eq!(
        edited.core_property("creator").unwrap().as_deref(),
        Some("Jane Counsel")
    );

    // Serialize and reparse a fresh Document: the value survives the package.
    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let reparsed = Document::parse(&bytes).expect("reparse");
    assert_eq!(
        reparsed.core_property("title").unwrap().as_deref(),
        Some("Master Services Agreement")
    );
    assert_eq!(
        reparsed.core_property("creator").unwrap().as_deref(),
        Some("Jane Counsel")
    );
}

#[test]
fn set_custom_property_visible_after_round_trip() {
    let base = Document::parse(&make_docx_with_core(SAMPLE_CORE)).expect("parse");
    let edited = base
        .set_custom_property("MatterNumber", "M-2026-0042")
        .expect("set custom");
    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let reparsed = Document::parse(&bytes).expect("reparse");
    assert_eq!(
        reparsed.custom_property("MatterNumber").unwrap().as_deref(),
        Some("M-2026-0042")
    );
}

#[test]
fn metadata_edit_leaves_body_unchanged_and_untracked() {
    let base = Document::parse(&make_docx_with_core(SAMPLE_CORE)).expect("parse");
    let base_bytes = base
        .serialize(&ExportOptions::default())
        .expect("serialize base");
    let base_doc_xml = part_bytes(&base_bytes, "word/document.xml").expect("base document.xml");

    let edited = base
        .set_core_property("title", "New Title")
        .expect("set title");
    let edited_bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize edited");
    let edited_doc_xml =
        part_bytes(&edited_bytes, "word/document.xml").expect("edited document.xml");

    // The body part is byte-for-byte identical: a metadata edit does not touch
    // word/document.xml.
    assert_eq!(
        base_doc_xml, edited_doc_xml,
        "metadata edit must leave word/document.xml byte-unchanged"
    );

    // And it introduces no tracked-change markup anywhere in the body.
    let body = String::from_utf8(edited_doc_xml).expect("utf8");
    assert!(!body.contains("w:ins"), "metadata is not a tracked insert");
    assert!(!body.contains("w:del"), "metadata is not a tracked delete");
    assert!(
        !body.contains("pPrChange"),
        "metadata is not a tracked formatting change"
    );

    // The core part itself carries the new title.
    let reparsed = Document::parse(&edited_bytes).expect("reparse");
    assert_eq!(
        reparsed.core_property("title").unwrap().as_deref(),
        Some("New Title")
    );
}

#[test]
fn core_props_parse_serialize_parse_identity() {
    let p = CoreProperties::parse(SAMPLE_CORE.as_bytes()).expect("parse");
    let bytes = p.serialize().expect("serialize");
    let p2 = CoreProperties::parse(&bytes).expect("reparse");
    assert_eq!(
        p, p2,
        "core docprops parse->serialize->parse must be identity"
    );
}

#[test]
fn custom_props_parse_serialize_parse_identity() {
    let custom = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/custom-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes"><property fmtid="{D5CDD505-2E9C-101B-9397-08002B2CF9AE}" pid="2" name="Reviewer"><vt:lpwstr>Carol</vt:lpwstr></property></Properties>"#;
    let c = CustomProperties::parse(custom.as_bytes()).expect("parse");
    let bytes = c.serialize().expect("serialize");
    let c2 = CustomProperties::parse(&bytes).expect("reparse");
    assert_eq!(
        c, c2,
        "custom docprops parse->serialize->parse must be identity"
    );
}

#[test]
fn malformed_core_part_is_error_not_fallback() {
    let err = CoreProperties::parse(b"<cp:coreProperties><dc:title>oops");
    assert!(
        matches!(err, Err(DocPropsError::MalformedXml { .. })),
        "malformed core.xml must be a hard error, got {err:?}"
    );
}

#[test]
fn unknown_core_field_is_rejected() {
    let base = Document::parse(&make_docx_with_core(SAMPLE_CORE)).expect("parse");
    let err: Result<Document, RuntimeError> = base.set_core_property("totalTime", "5");
    assert!(
        err.is_err(),
        "an unknown core field must be rejected, not defaulted"
    );
}
