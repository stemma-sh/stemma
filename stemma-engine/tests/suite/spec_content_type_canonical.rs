//! I-CT-002: WordprocessingML parts must carry their canonical content type.
//!
//! ECMA-376 Part 1 §15.2 fixes the content type of each WML part, and Word
//! locates parts such as `word/comments.xml` *by content type*. A part covered
//! only by the generic `Default Extension="xml"` (→ `application/xml`) is not
//! recognized as its WML role: Word reports "unreadable content" and drops the
//! part on repair. So:
//!
//! 1. The post-serialization validator flags such a package with an
//!    **Error**-severity `I-CT-002` finding (and it is a `BLOCKING_RULE`).
//! 2. The engine *repairs* the defect: parsing a package whose comments part has
//!    no Override and re-serializing it emits the canonical override, so the
//!    output passes the Blocking gate.
//!
//! This is the unit-level sentinel behind the corpus-wide
//! `spec_validator_clean_sweep` (#13 stand-in): the sweep proves we never emit
//! this defect over real fixtures; these tests pin *why* the rule exists and
//! that the engine actively fixes the input class that produced it.

use std::io::Write;

use stemma::api::Document;
use stemma::docx::DocxArchive;
use stemma::docx_validate::{ValidationSeverity, validate_docx};
use stemma::{ExportMode, ExportOptions, ValidatorLevel};
use zip::write::FileOptions;

const COMMENTS_CT: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml";
const COMMENTS_REL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments";

/// Build a minimal DOCX that has a real `word/comments.xml` part and a comments
/// relationship, but whose `[Content_Types].xml` declares an Override for the
/// comments part only when `comments_override` is true. When false, the comments
/// part is covered solely by the `xml` Default — the I-CT-002 defect.
fn make_docx_with_comments(comments_override: bool) -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Body text with a comment anchor.</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;

    let comments_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:comment w:id="1" w:author="A" w:date="2026-01-01T00:00:00Z"><w:p><w:r><w:t>note</w:t></w:r></w:p></w:comment></w:comments>"#;

    let comments_override_xml = if comments_override {
        format!(r#"<Override PartName="/word/comments.xml" ContentType="{COMMENTS_CT}"/>"#)
    } else {
        String::new()
    };
    let content_types = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>{comments_override_xml}</Types>"#
    );

    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId10" Type="{COMMENTS_REL}" Target="comments.xml"/></Relationships>"#
    );

    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        let mut put = |name: &str, data: &str| {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        };
        put("[Content_Types].xml", &content_types);
        put("_rels/.rels", rels);
        put("word/_rels/document.xml.rels", &doc_rels);
        put("word/document.xml", document_xml);
        put("word/comments.xml", comments_xml);
        zip.finish().unwrap();
    }
    buf
}

/// Extract the content type declared for `/word/comments.xml`, if any Override
/// exists. Returns `None` when the part is covered only by a Default.
fn comments_override(bytes: &[u8]) -> Option<String> {
    let archive = DocxArchive::read(bytes).expect("read docx");
    let ct = String::from_utf8(
        archive
            .get("[Content_Types].xml")
            .expect("content types")
            .to_vec(),
    )
    .expect("utf8");
    // Find the Override whose PartName is the comments part and return its type.
    for chunk in ct.split("<Override").skip(1) {
        let part = chunk
            .split("PartName=\"")
            .nth(1)
            .and_then(|s| s.split('"').next());
        if part == Some("/word/comments.xml") {
            return chunk
                .split("ContentType=\"")
                .nth(1)
                .and_then(|s| s.split('"').next())
                .map(|s| s.to_string());
        }
    }
    None
}

/// A package whose comments part has no Override (only the `xml` Default) is
/// flagged by the validator with an Error-severity I-CT-002 finding naming the
/// expected canonical content type.
#[test]
fn comments_part_without_override_is_flagged_error() {
    let bytes = make_docx_with_comments(false);
    // Sanity: the defective package really lacks the comments Override.
    assert_eq!(
        comments_override(&bytes),
        None,
        "test fixture should NOT declare a comments Override"
    );

    let v = validate_docx(&bytes);
    let ct_findings: Vec<&str> = v
        .findings
        .iter()
        .filter(|f| f.rule_id == "I-CT-002")
        .map(|f| f.message.as_str())
        .collect();
    assert_eq!(
        ct_findings.len(),
        1,
        "expected exactly one I-CT-002 finding, got: {:?}",
        v.findings.iter().map(|f| f.to_string()).collect::<Vec<_>>()
    );
    assert!(
        ct_findings[0].contains(COMMENTS_CT),
        "finding should name the canonical comments content type; got {}",
        ct_findings[0]
    );
    // It is an Error (so it gates as a blocking rule).
    assert!(
        v.findings
            .iter()
            .any(|f| f.rule_id == "I-CT-002" && f.severity == ValidationSeverity::Error),
        "I-CT-002 must be Error severity"
    );
}

/// A package that declares the canonical comments Override produces no I-CT-002.
#[test]
fn comments_part_with_canonical_override_is_clean() {
    let bytes = make_docx_with_comments(true);
    assert_eq!(comments_override(&bytes).as_deref(), Some(COMMENTS_CT));

    let v = validate_docx(&bytes);
    assert!(
        v.findings.iter().all(|f| f.rule_id != "I-CT-002"),
        "expected no I-CT-002, got: {:?}",
        v.findings.iter().map(|f| f.to_string()).collect::<Vec<_>>()
    );
}

/// The engine REPAIRS the defect at the source: parsing a package whose comments
/// part lacks an Override and re-serializing it emits the canonical override, so
/// the Blocking gate passes. This is the fix-at-source guarantee — the engine
/// authors the correct content type rather than round-tripping the input defect.
#[test]
fn engine_repairs_missing_comments_override_on_serialize() {
    let defective = make_docx_with_comments(false);
    assert_eq!(comments_override(&defective), None, "precondition");

    let doc = Document::parse(&defective).expect("parse defective package");
    let out = doc
        .serialize(&ExportOptions {
            mode: ExportMode::Redline,
            validator_level: ValidatorLevel::Blocking,
            validator: None,
        })
        .expect("serialize must pass Blocking gate after repair");

    // The re-serialized package now declares the canonical comments content type.
    assert_eq!(
        comments_override(&out).as_deref(),
        Some(COMMENTS_CT),
        "engine must emit the canonical comments Override on output"
    );

    // And the validator agrees the output is clean of I-CT-002.
    let v = validate_docx(&out);
    assert!(
        v.findings.iter().all(|f| f.rule_id != "I-CT-002"),
        "repaired output still has I-CT-002: {:?}",
        v.findings.iter().map(|f| f.to_string()).collect::<Vec<_>>()
    );
}
