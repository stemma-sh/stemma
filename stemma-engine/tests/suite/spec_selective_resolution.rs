//! Spec compliance for SELECTIVE tracked-change resolution — the engine path
//! the agentic MCP surface (`accept_changes` / `reject_changes`) drives.
//!
//! ECMA-376 §17.13.5.x: `w:ins` / `w:del` revision marks are paired,
//! well-formed insertion/deletion containers. Accepting some revisions while
//! leaving others tracked must still emit a well-formed document: no orphan
//! `w:del` wrapper around live text, no dangling `vMerge` continuation, no
//! revision markup the accept/reject path failed to collapse. The post-
//! serialization validator (`stemma::api::validate`, the same
//! package/wordprocessing/schema gate the runtime uses) is the spec oracle here.
//!
//! Daily tier: synthesized in-memory DOCX, no corpus, no real-Word oracle.

use std::collections::HashSet;
use std::io::Write;

use stemma::api::{Document, validate};
use stemma::domain::*;
use stemma::edit::*;
use stemma::tracked_model::ResolveSelectionAction;
use stemma::view::{SegmentView, TrackStatus, build_document_view_from_canon};
use stemma::{ExportOptions, Resolution};

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

fn authored_replace(
    block_id: &str,
    expect: &str,
    replacement: &str,
    revision_id: u32,
    author: &str,
) -> EditTransaction {
    EditTransaction {
        steps: vec![EditStep::ReplaceParagraphText {
            block_id: NodeId::from(block_id),
            rationale: None,
            replacement_role: None,
            expect: expect.to_string(),
            semantic_hash: None,
            content: ParagraphContent {
                fragments: vec![ContentFragment::Text(replacement.to_string())],
            },
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id,
            identity: 0,
            author: Some(author.to_string()),
            date: Some("2026-06-01T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

fn revision_ids_by_author(canon: &CanonDoc, author: &str) -> HashSet<u32> {
    let view = build_document_view_from_canon(canon);
    let mut ids = HashSet::new();
    let mut push = |s: &TrackStatus| {
        if let TrackStatus::Inserted(r) | TrackStatus::Deleted(r) = s
            && r.author.as_deref() == Some(author)
        {
            ids.insert(r.revision_id);
        }
    };
    for b in &view.blocks {
        push(&b.block_status);
        push(&b.paragraph_mark_status);
        for seg in &b.segments {
            match seg {
                SegmentView::Text { status, .. } | SegmentView::Opaque { status, .. } => {
                    push(status)
                }
            }
        }
    }
    ids
}

/// Build a document with two independently-authored tracked changes.
fn two_authored() -> Document {
    let base = make_test_docx(&["First clause text", "Second clause text"]);
    let doc = Document::parse(&base).expect("parse");
    let ids: Vec<String> = doc.read().blocks.iter().map(|b| b.id.to_string()).collect();
    doc.apply(&authored_replace(
        &ids[0],
        "First clause text",
        "First AMENDED",
        1,
        "Alice",
    ))
    .expect("alice")
    .apply(&authored_replace(
        &ids[1],
        "Second clause text",
        "Second AMENDED",
        2,
        "Bob",
    ))
    .expect("bob")
}

/// §17.13.5: selectively accepting ONE author's revisions, leaving the other
/// tracked, must emit a well-formed DOCX that re-parses and validates clean —
/// no orphan w:ins/w:del/vMerge from the partial resolution.
#[test]
fn spec_selective_accept_emits_wellformed_docx() {
    let two = two_authored();
    let alice = revision_ids_by_author(&two.snapshot().canonical, "Alice");
    assert!(!alice.is_empty(), "Alice must have revisions to accept");

    let resolved = two
        .project(Resolution::Selective {
            ids: alice,
            action: ResolveSelectionAction::Accept,
        })
        .expect("selective accept Alice");

    let bytes = resolved
        .serialize(&ExportOptions::default())
        .expect("serialize partially-resolved doc");

    // Re-parses (structural well-formedness) and validates (spec gate).
    Document::parse(&bytes).expect("partially-resolved doc must re-parse");
    let report = validate(&bytes);
    assert!(
        report.ok,
        "selective-accept export must validate clean (no orphan revision markup): {:?}",
        report.issues
    );
}

/// §17.13.5: selectively REJECTING one author's revisions while leaving the
/// other tracked must likewise emit a well-formed, valid DOCX.
#[test]
fn spec_selective_reject_emits_wellformed_docx() {
    let two = two_authored();
    let bob = revision_ids_by_author(&two.snapshot().canonical, "Bob");
    assert!(!bob.is_empty(), "Bob must have revisions to reject");

    let resolved = two
        .project(Resolution::Selective {
            ids: bob,
            action: ResolveSelectionAction::Reject,
        })
        .expect("selective reject Bob");

    let bytes = resolved
        .serialize(&ExportOptions::default())
        .expect("serialize partially-resolved doc");

    Document::parse(&bytes).expect("partially-resolved doc must re-parse");
    let report = validate(&bytes);
    assert!(
        report.ok,
        "selective-reject export must validate clean (no orphan revision markup): {:?}",
        report.issues
    );
}
