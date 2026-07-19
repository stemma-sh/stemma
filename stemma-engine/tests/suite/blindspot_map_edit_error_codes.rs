//! Blindspot regression: `map_edit_error` (runtime.rs:5123) ends with a
//! catch-all `_ => ErrorCode::InvalidRange` arm. Many `EditError` variants are
//! never named in the explicit arms above it and therefore collapse onto
//! `InvalidRange`, so the public `RuntimeError.code` misrepresents the failure
//! class.
//!
//! DOMAIN-CORRECT BEHAVIOR: distinct error CLASSES must surface as distinct,
//! semantically-correct public codes. `ErrorCode` (runtime.rs) defines a
//! dedicated `AnchorNotFound` code, and every other "the addressed thing does
//! not exist" `EditError` IS mapped to it explicitly:
//!   - `BlockNotFound`, `StoryNotFound`, `StoryBlockNotFound`,
//!     `BookmarkNotFound`, `StyleNotFound`, `DrawingNotFound`,
//!     `ContentControlNotFound`, `CommentTargetNotFound`,
//!     `CommentAnchorNotFound`, `NoteNotFound`, ... => `ErrorCode::AnchorNotFound`.
//!
//! `EditError::HyperlinkNotFound` (edit/mod.rs:1636) is the same class: it is
//! raised by the `ReplaceHyperlinkText` verb (edit/mod.rs:8461-8475) when there
//! is NO inline with the requested id anywhere in the document — i.e. a missing
//! anchor, exactly like `DrawingNotFound`/`ContentControlNotFound` which sit
//! right beside it in the verb family and DO map to `AnchorNotFound`. But
//! `HyperlinkNotFound` is not named in any explicit arm of `map_edit_error`, so
//! it falls through the `_` arm to `InvalidRange`.
//!
//! A "this hyperlink id does not exist" failure is unambiguously NOT an
//! invalid-range error. The post-condition of the public error mapping is that
//! a not-found failure reports the not-found code. This test asserts the
//! domain-correct code (`AnchorNotFound`), not whatever the catch-all currently
//! emits.
//!
//! Driven entirely through the public surface (`Document::parse` +
//! `Document::apply`), so the error travels the real path
//! `EditSnapshot::apply` -> `map_edit_error` -> `RuntimeError`.

use stemma::api::Document;
use stemma::domain::*;
use stemma::edit::*;
use stemma::runtime::ErrorCode;

// ─── Fixture (copied verbatim from edit_fidelity_invariants.rs:29-62) ────────

/// Minimal plain-paragraph DOCX.
fn make_test_docx(paragraphs: &[&str]) -> Vec<u8> {
    let mut document_xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>"#,
    );
    for para in paragraphs {
        document_xml.push_str(&format!(r#"<w:p><w:r><w:t>{para}</w:t></w:r></w:p>"#));
    }
    document_xml.push_str("<w:sectPr/></w:body></w:document>");

    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
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
        zip.finish().unwrap();
    }
    buf
}

/// Transaction wrapper (copied verbatim from edit_fidelity_invariants.rs:132-144).
fn txn(steps: Vec<EditStep>, mode: MaterializationMode) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        materialization_mode: mode,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("Gate".to_string()),
            date: Some("2026-06-01T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

#[test]
fn hyperlink_not_found_is_anchor_not_found_not_invalid_range() {
    // A document with a single plain paragraph — no hyperlinks at all.
    let doc = Document::parse(&make_test_docx(&["Hello world"])).expect("parse");

    // Target a hyperlink id that does not exist anywhere in the document. The
    // verb walks the whole doc, finds no inline with this id, and raises
    // `EditError::HyperlinkNotFound` (verb: edit/mod.rs:8461-8475).
    let step = EditStep::ReplaceHyperlinkText {
        hyperlink_id: NodeId::from("nonexistent-hyperlink-id"),
        rationale: None,
        expect: "anything".to_string(),
        new_text: "replacement".to_string(),
        expect_href: None,
        expect_anchor: None,
    };

    let err = match doc.apply(&txn(vec![step], MaterializationMode::TrackedChange)) {
        Ok(_) => panic!("applying a replace against a missing hyperlink id must fail"),
        Err(e) => e,
    };

    // Domain-correct post-condition: a "no such hyperlink" failure is a
    // not-found / missing-anchor error. Every sibling `*NotFound` variant maps
    // to `ErrorCode::AnchorNotFound`; this one must too.
    //
    // The load-bearing assertion is the negative one: this is clearly NOT an
    // invalid-range error. If the catch-all `_ => InvalidRange` arm is what
    // classifies it, that is the confirmed defect.
    assert_ne!(
        err.code,
        ErrorCode::InvalidRange,
        "HyperlinkNotFound (a missing-anchor failure) must NOT be reported as \
         InvalidRange — it fell through the `_ => ErrorCode::InvalidRange` \
         catch-all in map_edit_error (runtime.rs:5123). public message: {}",
        err.message
    );

    assert_eq!(
        err.code,
        ErrorCode::AnchorNotFound,
        "HyperlinkNotFound must surface as AnchorNotFound, consistent with every \
         other *NotFound EditError variant (BlockNotFound, DrawingNotFound, \
         ContentControlNotFound, ...). public message: {}",
        err.message
    );
}
