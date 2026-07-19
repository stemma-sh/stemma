//! An import refusal must name the PART it came from.
//!
//! The `word_ir` importer refuses out-of-schema shapes with a contextual
//! message (e.g. "unexpected element <w:hyperlink> inside tracked change
//! <w:ins>"), but that message describes only the offending element — not
//! whether it was read from `word/document.xml`, a header, footnotes, or
//! comments. The same refusing shape can appear in any story part, so without
//! the part name the error is not actionable (CLAUDE.md: errors must include
//! the identifiers needed to debug). The importer attaches the part name at
//! the per-part boundary (`with_part_context`), where the caller that selected
//! the part knows its name.
//!
//! These tests place ONE refusing shape (a `w:hyperlink` nested directly in a
//! `w:ins`, out-of-schema for `CT_RunTrackChange`) in (a) the main document
//! body and (b) a referenced header part, and assert the refusal names the
//! right part in each case — and that the two labels differ, so a body refusal
//! is never mistaken for a header one.
//!
//! Sentinel: without `with_part_context`, the mapped refusal carries no part
//! name — the "in part …" clause is absent and `details.context` is `None`, so
//! each `contains(part)` assertion fails. Captured before restoring the wrap.
//!
//! Daily tier: synthetic in-memory DOCX, no corpus.

use std::io::Write;

use stemma::api::Document;
use stemma::{DocFingerprint, build_canonical_from_docx_preserving_tracked};

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const HEADER_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header";

/// A `w:hyperlink` nested directly inside a `w:ins` is out-of-schema for
/// `CT_RunTrackChange` and matches none of the importer's tracked-change arms,
/// so importing it is refused rather than silently dropped. The refusal is
/// identical wherever the shape appears — which is exactly why the part name
/// matters.
const REFUSING_PARAGRAPH: &str = r#"<w:p>
  <w:r><w:t xml:space="preserve">keep </w:t></w:r>
  <w:ins w:id="7" w:author="A" w:date="2024-01-01T00:00:00Z">
    <w:hyperlink w:anchor="X"><w:r><w:t>linktext</w:t></w:r></w:hyperlink>
  </w:ins>
</w:p>"#;

fn zip_docx(parts: &[(&str, String)]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        use zip::write::FileOptions;
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        for (name, data) in parts {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

/// A minimal package whose main document body is `body_inner`. When
/// `header_body` is `Some`, a `header1.xml` part carrying it is added and
/// referenced from the body's `sectPr` (so the streaming importer actually
/// parses it).
fn docx(body_inner: &str, header_body: Option<&str>) -> Vec<u8> {
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;

    let (sect_pr, doc_rels_inner) = match header_body {
        Some(_) => (
            r#"<w:sectPr><w:headerReference w:type="default" r:id="rId2"/></w:sectPr>"#.to_string(),
            format!(r#"<Relationship Id="rId2" Type="{HEADER_REL_TYPE}" Target="header1.xml"/>"#),
        ),
        None => ("<w:sectPr/>".to_string(), String::new()),
    };

    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W_NS}" xmlns:r="{R_NS}"><w:body>{body_inner}{sect_pr}</w:body></w:document>"#
    );
    let doc_rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{doc_rels_inner}</Relationships>"#
    );

    let mut parts = vec![
        ("[Content_Types].xml", content_types.to_string()),
        ("_rels/.rels", root_rels.to_string()),
        ("word/_rels/document.xml.rels", doc_rels),
        ("word/document.xml", document_xml),
    ];
    if let Some(header_body) = header_body {
        let header_xml = format!(r#"<w:hdr xmlns:w="{W_NS}">{header_body}</w:hdr>"#);
        parts.push(("word/header1.xml", header_xml));
    }
    zip_docx(&parts)
}

/// The refusing shape in the main document body: the error names
/// `word/document.xml` (the resolved main part), keeps the underlying
/// contextual refusal, and carries the part in the structured `context` slot.
#[test]
fn body_refusal_names_the_document_part() {
    let bytes = docx(REFUSING_PARAGRAPH, None);
    let err = build_canonical_from_docx_preserving_tracked(&bytes, DocFingerprint("t".to_string()))
        .expect_err("an out-of-schema tracked-change child in the body must be refused");

    assert!(
        err.message.contains("word/document.xml"),
        "body refusal must name its part (word/document.xml): {}",
        err.message
    );
    // The underlying, part-agnostic refusal is preserved — the part name is
    // added to it, not substituted for it.
    assert!(
        err.message.to_lowercase().contains("hyperlink"),
        "the original refusal must survive the part-context wrap: {}",
        err.message
    );
    assert_eq!(
        err.details.context.as_deref(),
        Some("part: word/document.xml"),
        "the part name must also land in the structured context slot: {:?}",
        err.details.context
    );
}

/// The SAME refusing shape in a referenced header part: the error names
/// `word/header1.xml`, not the document body — so an identical refusal is
/// attributable to the header it actually came from.
#[test]
fn header_refusal_names_the_header_part() {
    let clean_body = r#"<w:p><w:r><w:t>body</w:t></w:r></w:p>"#;
    let bytes = docx(clean_body, Some(REFUSING_PARAGRAPH));
    let err = build_canonical_from_docx_preserving_tracked(&bytes, DocFingerprint("t".to_string()))
        .expect_err("an out-of-schema tracked-change child in a header must be refused");

    assert!(
        err.message.contains("word/header1.xml"),
        "header refusal must name its part (word/header1.xml): {}",
        err.message
    );
    // Disambiguation: the header refusal must NOT be attributed to the body.
    assert!(
        !err.message.contains("word/document.xml"),
        "a header refusal must not be labelled with the document part: {}",
        err.message
    );
    assert!(
        err.message.to_lowercase().contains("hyperlink"),
        "the original refusal must survive the part-context wrap: {}",
        err.message
    );
    assert_eq!(
        err.details.context.as_deref(),
        Some("part: word/header1.xml"),
        "the part name must also land in the structured context slot: {:?}",
        err.details.context
    );
}

/// The public parse path (`Document::parse`) imports the body through the
/// root-based importer, which is handed an already-parsed root and never
/// resolves the main part's package name. It still labels the body distinctly
/// (as the main document body) so a body refusal is not mistaken for a
/// header/footnote/comment one. Pins that the public path also carries a part
/// label; if the resolved name is later threaded through, tighten this to
/// `word/document.xml`.
#[test]
fn public_parse_body_refusal_is_labelled_the_body() {
    let bytes = docx(REFUSING_PARAGRAPH, None);
    let err = Document::parse(&bytes)
        .err()
        .expect("an out-of-schema tracked-change child in the body must be refused");

    let msg = err.message.to_lowercase();
    assert!(
        msg.contains("document"),
        "public-parse body refusal must identify the main document: {}",
        err.message
    );
    assert!(
        !msg.contains("header"),
        "a body refusal must not be attributed to a header: {}",
        err.message
    );
}
