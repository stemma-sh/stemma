//! OPC main-document-part resolution (ECMA-376 Part 2 §9.3).
//!
//! The main document part is located by following the `officeDocument`
//! relationship in the package root-relationships part (`_rels/.rels`); its
//! part name is NOT fixed. Word writes `word/document.xml` by convention, but a
//! conformant package may name it anything — a real, Word-openable document
//! keeps its main part at `word/document2.xml`. The engine must:
//!
//!   - locate the main part through the relationship (not a hardcoded name),
//!   - derive the main part's own `.rels` path from the resolved name,
//!   - round-trip the ORIGINAL part name verbatim on export, with the
//!     content-type Override intact, and
//!   - validate clean and keep tracked accept/reject text identity.
//!
//! These witnesses are synthetic in-memory packages (the repo `pack()` idiom) —
//! no corpus dependency. The non-conventional part name is `word/document2.xml`.

use std::io::{Cursor, Write};

use stemma::{
    DocxRuntime, ExportMode, RevisionInfo, SimpleRuntime, accept_all, diff_documents, merge_diff,
    reject_all_with_styles,
};
use zip::{ZipWriter, write::FileOptions};

use crate::common;

const MAIN_CT: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml";
const STYLES_CT: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml";
const OFFICE_DOCUMENT_REL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument";
const STYLES_REL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles";

/// A minimal `word/styles.xml` with one built-in style, so import has a style
/// table to read.
const STYLES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>
</w:styles>"#;

/// Build a DOCX whose main document part is stored at `main_part` (e.g.
/// `word/document2.xml`), located via the officeDocument relationship. The
/// body text is a single paragraph containing `body_text`. A `styles.xml`
/// sibling is present and referenced from the main part's `.rels`.
fn docx_with_main_part(main_part: &str, body_text: &str) -> Vec<u8> {
    let file = main_part.rsplit('/').next().expect("part filename");
    let dir = &main_part[..main_part.len() - file.len()]; // e.g. "word/"
    let doc_rels_path = format!("{dir}_rels/{file}.rels");

    let content_types = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/{main_part}" ContentType="{MAIN_CT}"/>
  <Override PartName="/{dir}styles.xml" ContentType="{STYLES_CT}"/>
</Types>"#
    );

    let root_rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="{OFFICE_DOCUMENT_REL}" Target="/{main_part}"/>
</Relationships>"#
    );

    let doc_rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="{STYLES_REL}" Target="styles.xml"/>
</Relationships>"#
    );

    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:r><w:t xml:space="preserve">{body_text}</w:t></w:r></w:p>
    <w:sectPr><w:pgSz w:w="12240" w:h="15840"/></w:sectPr>
  </w:body>
</w:document>"#
    );

    zip_parts(&[
        ("[Content_Types].xml", &content_types),
        ("_rels/.rels", &root_rels),
        (&doc_rels_path, &doc_rels),
        (&format!("{dir}styles.xml"), STYLES_XML),
        (main_part, &document),
    ])
}

fn zip_parts(parts: &[(&str, &str)]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut zip = ZipWriter::new(Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        for (name, data) in parts {
            zip.start_file(*name, opts).expect("start_file");
            zip.write_all(data.as_bytes()).expect("write part");
        }
        zip.finish().expect("finish zip");
    }
    buf
}

fn revision() -> RevisionInfo {
    RevisionInfo {
        revision_id: 1,
        author: Some("opc-sentinel".to_string()),
        date: Some("2026-07-06T00:00:00Z".to_string()),
        apply_op_id: None,
    }
}

fn body_text(bytes: &[u8]) -> String {
    let rt = SimpleRuntime::new();
    let canon = std::sync::Arc::unwrap_or_clone(rt.import_docx(bytes).expect("import").canonical);
    common::all_paragraphs(&canon)
        .iter()
        .map(|p| common::paragraph_text(p))
        .collect::<Vec<_>>()
        .join(" ")
}

// ── the happy path ─────────────────────────────────────────────────────────

#[test]
fn imports_main_part_located_at_non_conventional_name() {
    let bytes = docx_with_main_part("word/document2.xml", "Hello world");
    let rt = SimpleRuntime::new();
    let import = rt
        .import_docx(&bytes)
        .expect("import must succeed when the main part is at word/document2.xml");
    let canon = std::sync::Arc::unwrap_or_clone(import.canonical);
    let text = common::all_paragraphs(&canon)
        .iter()
        .map(|p| common::paragraph_text(p))
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(text.trim(), "Hello world", "body text must import verbatim");
}

#[test]
fn export_round_trips_the_original_part_name_and_validates_clean() {
    let bytes = docx_with_main_part("word/document2.xml", "Hello world");
    let rt = SimpleRuntime::new();
    let import = rt.import_docx(&bytes).expect("import");
    let exported = rt
        .export_docx(&import.doc_handle, ExportMode::Redline)
        .expect("export");

    // The main part must round-trip under its ORIGINAL name (verbatim
    // principle) — never renamed to the conventional word/document.xml.
    assert!(
        common::read_zip_entry(&exported, "word/document2.xml").is_some(),
        "exported package must keep the main part at word/document2.xml"
    );
    assert!(
        common::read_zip_entry(&exported, "word/document.xml").is_none(),
        "exported package must NOT invent a word/document.xml"
    );
    // Its relationships part follows the resolved name.
    assert!(
        common::read_zip_entry(&exported, "word/_rels/document2.xml.rels").is_some(),
        "the main part's .rels must be derived from its name"
    );
    // Its content-type Override survives, for the resolved name.
    let ct = common::read_zip_entry(&exported, "[Content_Types].xml").expect("content types");
    assert!(
        ct.contains(r#"PartName="/word/document2.xml""#),
        "content-type Override must name the resolved main part; got: {ct}"
    );

    // Validator: zero blocking findings.
    let report = stemma::validate_docx_report(&exported).expect("validate");
    assert!(
        report.ok,
        "exported package must validate clean; issues: {:?}",
        report.issues
    );
}

#[test]
fn tracked_edit_accept_reject_text_identity_holds() {
    // A→B tracked change over a document whose main part is word/document2.xml:
    // accept_all yields B's text, reject_all yields A's text (invariants #6/#14).
    let a_bytes = docx_with_main_part("word/document2.xml", "Hello world");
    let b_bytes = docx_with_main_part("word/document2.xml", "Hello brave world");

    let rt = SimpleRuntime::new();
    let a = std::sync::Arc::unwrap_or_clone(rt.import_docx(&a_bytes).expect("import A").canonical);
    let b = std::sync::Arc::unwrap_or_clone(rt.import_docx(&b_bytes).expect("import B").canonical);

    let want_a = body_text(&a_bytes);
    let want_b = body_text(&b_bytes);
    assert_ne!(want_a, want_b, "fixture must actually differ");

    let diff = diff_documents(&a, &b).expect("diff A→B");
    let merged = merge_diff(&a, &b, &diff, &revision()).expect("merge").doc;

    let mut accepted = merged.clone();
    accept_all(&mut accepted);
    let accepted_text = common::all_paragraphs(&accepted)
        .iter()
        .map(|p| common::paragraph_text(p))
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(accepted_text, want_b, "accept_all must equal B text");

    let mut rejected = merged;
    reject_all_with_styles(&mut rejected, None);
    let rejected_text = common::all_paragraphs(&rejected)
        .iter()
        .map(|p| common::paragraph_text(p))
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(rejected_text, want_a, "reject_all must equal A text");
}

// ── negative cases: distinct, actionable errors ────────────────────────────

#[test]
fn rels_without_office_document_relationship_is_a_specific_error() {
    // _rels/.rels is present and well-formed but declares NO officeDocument
    // relationship: there is no discoverable main part.
    let content_types = format!(
        r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="{MAIN_CT}"/>
</Types>"#
    );
    // A root rels with only an unrelated (core-properties) relationship.
    let root_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/>
</Relationships>"#;
    let document = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p/></w:body></w:document>"#;
    let bytes = zip_parts(&[
        ("[Content_Types].xml", &content_types),
        ("_rels/.rels", root_rels),
        (
            "word/_rels/document.xml.rels",
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#,
        ),
        ("word/document.xml", document),
    ]);

    let rt = SimpleRuntime::new();
    let err = rt
        .import_docx(&bytes)
        .expect_err("a package with no officeDocument relationship must be rejected");
    assert!(
        err.message.contains("officeDocument relationship"),
        "error must name the missing officeDocument relationship; got: {}",
        err.message
    );
}

#[test]
fn office_document_relationship_to_missing_part_is_a_specific_error() {
    // The officeDocument relationship resolves to a part that is not in the
    // package: distinct from "no relationship at all".
    let content_types = format!(
        r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="{MAIN_CT}"/>
</Types>"#
    );
    let root_rels = format!(
        r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="{OFFICE_DOCUMENT_REL}" Target="word/missing.xml"/>
</Relationships>"#
    );
    // No word/missing.xml part present.
    let bytes = zip_parts(&[
        ("[Content_Types].xml", &content_types),
        ("_rels/.rels", &root_rels),
    ]);

    let rt = SimpleRuntime::new();
    let err = rt
        .import_docx(&bytes)
        .expect_err("officeDocument target absent from the package must be rejected");
    assert!(
        err.message.contains("not present in the package"),
        "error must say the resolved part is absent; got: {}",
        err.message
    );
    assert!(
        err.message.contains("word/missing.xml"),
        "error must name the resolved (missing) part; got: {}",
        err.message
    );
}
