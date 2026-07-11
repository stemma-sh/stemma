//! Spec compliance for resolving tracked changes inside COMMENT BODIES.
//!
//! A comment body is a story like any other: `w:comment` → `w:p` → runs, and
//! those runs can carry `w:ins`/`w:del`. Real Word's "Accept All" / "Reject
//! All" resolves revisions everywhere, including comment text. The archive-level
//! resolution paths (`normalize::normalize_docx` = accept-all,
//! `normalize::reject_all_docx`) must therefore resolve the comments part too,
//! not pass it through verbatim — otherwise a document still carries revision
//! markup after "accept all". The model path
//! (`Document::project` → `accept_all`/`reject_all`) already resolves comment
//! bodies, so this also pins wire/model equivalence: the comment TEXT after a
//! wire resolution must equal the comment text after the model resolution.
//!
//! Daily tier: synthesized in-memory DOCX, no corpus, no real-Word oracle.

use std::io::{Read, Write};

use stemma::api::{Document, validate};
use stemma::docx::DocxArchive;
use stemma::{ExportOptions, Resolution};

/// One comment whose body carries a base run, an inserted run, and a deleted
/// run — so accept keeps "Base kept" and reject restores "Base gone". The
/// comment is anchored in the body with a comment range + reference so the
/// package is a realistic, valid shape.
const COMMENT_BODY: &str = r#"<w:comment w:id="1" w:author="Reviewer" w:date="2024-01-01T00:00:00Z">
    <w:p>
      <w:r><w:t xml:space="preserve">Base </w:t></w:r>
      <w:ins w:id="10" w:author="Reviewer" w:date="2024-01-01T00:00:00Z"><w:r><w:t>kept</w:t></w:r></w:ins>
      <w:del w:id="11" w:author="Reviewer" w:date="2024-01-01T00:00:00Z"><w:r><w:delText>gone</w:delText></w:r></w:del>
    </w:p>
  </w:comment>"#;

/// Build a full DOCX (bytes) whose document body anchors a single comment, and
/// whose `word/comments.xml` part is `comment_body`. Includes the comments
/// content-type Override and the document→comments relationship so both the
/// model path (`Document::parse`) and the archive path (`DocxArchive::read`)
/// discover the part exactly as they would for a real Word file.
fn docx_with_comment(comment_body: &str) -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:commentRangeStart w:id="1"/><w:r><w:t>anchored</w:t></w:r><w:commentRangeEnd w:id="1"/><w:r><w:commentReference w:id="1"/></w:r></w:p><w:sectPr/></w:body></w:document>"#;
    let comments_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">{comment_body}</w:comments>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/comments.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId100" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="comments.xml"/></Relationships>"#;
    let mut buf = Vec::new();
    {
        use zip::write::FileOptions;
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
        zip.start_file("word/comments.xml", opts).unwrap();
        zip.write_all(comments_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

fn part_of(bytes: &[u8], part: &str) -> String {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut f = zip.by_name(part).unwrap();
    let mut s = String::new();
    f.read_to_string(&mut s).unwrap();
    s
}

/// Strip every XML tag, leaving only text content, then collapse whitespace.
/// Used to compare comment-body TEXT across the wire and model paths without
/// depending on element ordering or serialization details.
fn text_only(xml: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for ch in xml.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ── Archive path: accept-all resolves comment-body revisions ─────────────────

#[test]
fn archive_accept_resolves_comment_body_revisions() {
    let archive = DocxArchive::read(&docx_with_comment(COMMENT_BODY)).expect("read");
    let (out, result) = stemma::normalize::normalize_docx(&archive).expect("accept");
    let comments = String::from_utf8(out.get("word/comments.xml").unwrap().to_vec()).unwrap();

    // Zero revision markup left in the comments part.
    assert!(
        !comments.contains("<w:ins") && !comments.contains("<w:del"),
        "accept must strip revision wrappers from comments: {comments}"
    );
    assert!(
        !comments.contains("delText"),
        "accept must leave no delText in comments: {comments}"
    );
    // Inserted text kept (unwrapped); deleted text gone.
    assert!(
        comments.contains("kept"),
        "inserted comment text must survive accept: {comments}"
    );
    assert!(
        !comments.contains("gone"),
        "deleted comment text must be dropped on accept: {comments}"
    );
    // Comment identity is preserved (only body revisions resolved).
    assert!(
        comments.contains(r#"w:id="1""#) && comments.contains("Reviewer"),
        "comment identity (id, author) must survive: {comments}"
    );

    // Stats: the comments part is reported normalized and its revisions counted,
    // exactly as a header/footnote part would be.
    assert!(
        result
            .parts_normalized
            .iter()
            .any(|p| p == "word/comments.xml"),
        "comments part must be reported normalized: {:?}",
        result.parts_normalized
    );
    assert!(
        result.revisions_resolved >= 2,
        "the comment's ins+del must count into revisions_resolved: {result:?}"
    );

    // The document part is left semantically intact (it carried no revisions).
    let document = String::from_utf8(out.get("word/document.xml").unwrap().to_vec()).unwrap();
    assert!(
        document.contains("anchored") && document.contains("commentReference"),
        "document part must keep its anchor + reference: {document}"
    );
}

// ── Archive path: reject-all resolves comment-body revisions ─────────────────

#[test]
fn archive_reject_resolves_comment_body_revisions() {
    let archive = DocxArchive::read(&docx_with_comment(COMMENT_BODY)).expect("read");
    let (out, _result) = stemma::normalize::reject_all_docx(&archive).expect("reject");
    let comments = String::from_utf8(out.get("word/comments.xml").unwrap().to_vec()).unwrap();

    assert!(
        !comments.contains("<w:ins") && !comments.contains("<w:del"),
        "reject must strip revision wrappers from comments: {comments}"
    );
    assert!(
        !comments.contains("delText"),
        "reject must restore delText to t in comments: {comments}"
    );
    // Inverse of accept: inserted text dropped, deleted text restored.
    assert!(
        !comments.contains("kept"),
        "inserted comment text must be dropped on reject: {comments}"
    );
    assert!(
        comments.contains("gone"),
        "deleted comment text must be restored on reject: {comments}"
    );
}

// ── Wire/model equivalence on comment-body text ──────────────────────────────

#[test]
fn wire_accept_comment_text_matches_model_accept() {
    let bytes = docx_with_comment(COMMENT_BODY);

    // Wire (archive) accept.
    let archive = DocxArchive::read(&bytes).expect("read");
    let (out, _) = stemma::normalize::normalize_docx(&archive).expect("accept");
    let wire_comments = String::from_utf8(out.get("word/comments.xml").unwrap().to_vec()).unwrap();

    // Model accept.
    let doc = Document::parse(&bytes).expect("parse");
    let resolved = doc.project(Resolution::AcceptAll).expect("accept");
    let model_bytes = resolved
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let model_comments = part_of(&model_bytes, "word/comments.xml");

    assert_eq!(
        text_only(&wire_comments),
        text_only(&model_comments),
        "wire and model comment text must agree after accept"
    );
    // And it is the accepted text.
    assert_eq!(text_only(&wire_comments), "Base kept");
    assert!(
        validate(&model_bytes).ok,
        "model-accepted doc must validate"
    );
}

#[test]
fn wire_reject_comment_text_matches_model_reject() {
    let bytes = docx_with_comment(COMMENT_BODY);

    let archive = DocxArchive::read(&bytes).expect("read");
    let (out, _) = stemma::normalize::reject_all_docx(&archive).expect("reject");
    let wire_comments = String::from_utf8(out.get("word/comments.xml").unwrap().to_vec()).unwrap();

    let doc = Document::parse(&bytes).expect("parse");
    let resolved = doc.project(Resolution::RejectAll).expect("reject");
    let model_bytes = resolved
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let model_comments = part_of(&model_bytes, "word/comments.xml");

    assert_eq!(
        text_only(&wire_comments),
        text_only(&model_comments),
        "wire and model comment text must agree after reject"
    );
    assert_eq!(text_only(&wire_comments), "Base gone");
    assert!(
        validate(&model_bytes).ok,
        "model-rejected doc must validate"
    );
}
