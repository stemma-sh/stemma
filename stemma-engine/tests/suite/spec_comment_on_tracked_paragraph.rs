//! Spec: `comment_create` on a paragraph that already carries tracked changes.
//!
//! Domain rule (ECMA-376 §17.13.4): a comment is an ANNOTATION, not a tracked
//! change. Word anchors comments on redlined clauses constantly, and the
//! canonical negotiation order is "make a tracked counter-edit on a clause, THEN
//! attach a comment to that clause". The engine must therefore let a comment
//! anchor on a paragraph carrying tracked *segments* — refusing only when the
//! anchor is genuinely ambiguous (unlocatable, or landing on deleted content).
//!
//! Marker placement is bounded by two invariants these tests pin:
//!   - a range may ENCLOSE a whole `w:ins` / `w:del` container that sits between
//!     its endpoints, but a marker must never split a tracked container mid-run;
//!   - when an endpoint falls inside an insertion the range WIDENS outward to the
//!     container boundary (encloses the whole `w:ins`).
//!
//! Every serialized result must pass the annotation validators (comment-range
//! pairing I-ANN-005, tracked-change content model I-TC-001, dangling reference
//! I-XREF-003), including after accept-all and reject-all.
//!
//! Daily tier, corpus-free (tracked-changes DOCX synthesized in-memory).

use stemma::ExportOptions;
use stemma::api::Document;
use stemma::docx::DocxArchive;
use stemma::domain::{BlockNode, CanonDoc, NodeId, RevisionInfo};
use stemma::edit::{EditError, EditStep, EditTransaction, MaterializationMode, apply_transaction};
use stemma::runtime::Resolution;

/// Wrap arbitrary paragraph XML (which may carry `w:ins` / `w:del`) into a
/// minimal, parseable DOCX with no pre-existing comments.
fn make_docx_with_body(paragraph_xml: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{paragraph_xml}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"></Relationships>"#;

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

/// A clause "The term is <ins>three</ins> years." — a paragraph carrying a
/// tracked counter-edit (the inserted word) flanked by Normal text. Visible text
/// is "The term is three years.".
fn tracked_insertion_clause() -> Vec<u8> {
    make_docx_with_body(
        r#"<w:p><w:r><w:t xml:space="preserve">The term is </w:t></w:r><w:ins w:id="1" w:author="A" w:date="2026-01-01T00:00:00Z"><w:r><w:t>three</w:t></w:r></w:ins><w:r><w:t xml:space="preserve"> years.</w:t></w:r></w:p>"#,
    )
}

/// A clause "Keep <del>remove this</del> tail." — visible text "Keep  tail.";
/// "remove this" is struck (deleted).
fn tracked_deletion_clause() -> Vec<u8> {
    make_docx_with_body(
        r#"<w:p><w:r><w:t xml:space="preserve">Keep </w:t></w:r><w:del w:id="2" w:author="A" w:date="2026-01-01T00:00:00Z"><w:r><w:delText>remove this</w:delText></w:r></w:del><w:r><w:t xml:space="preserve"> tail.</w:t></w:r></w:p>"#,
    )
}

fn first_block_id(doc: &CanonDoc) -> NodeId {
    match &doc.blocks[0].block {
        BlockNode::Paragraph(p) => p.id.clone(),
        other => panic!("expected paragraph, got {other:?}"),
    }
}

fn txn(steps: Vec<EditStep>) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 1,
            author: Some("Reviewer".to_string()),
            date: Some("2026-06-01T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

fn create_step(block_id: NodeId, expect: &str, body: &str) -> EditStep {
    EditStep::CommentCreate {
        block_id,
        expect: expect.to_string(),
        semantic_hash: None,
        body: body.to_string(),
        author: Some("Reviewer".to_string()),
        rationale: None,
    }
}

/// Serialize and assert the comment/tracked-change annotation validators are
/// clean: no error-severity findings for comment-range pairing (I-ANN-005),
/// tracked-change content model (I-TC-001), or dangling references (I-XREF-003).
fn assert_annotations_clean(bytes: &[u8], ctx: &str) {
    use stemma::docx_validate::ValidationSeverity;
    let validation = stemma::docx_validate::validate_docx(bytes);
    let offenders: Vec<String> = validation
        .findings
        .iter()
        .filter(|f| f.severity == ValidationSeverity::Error)
        .filter(|f| {
            f.rule_id.starts_with("I-ANN")
                || f.rule_id.starts_with("I-TC")
                || f.rule_id.starts_with("I-XREF")
        })
        .map(|f| f.to_string())
        .collect();
    assert!(
        offenders.is_empty(),
        "{ctx}: expected clean annotation validation, got: {offenders:?}"
    );
}

fn document_xml(bytes: &[u8]) -> String {
    let archive = DocxArchive::read(bytes).expect("read");
    String::from_utf8_lossy(archive.get("word/document.xml").expect("document.xml")).into_owned()
}

/// A comment fully anchored: exactly one balanced range pair plus a reference.
fn assert_fully_anchored(doc_xml: &str) {
    assert_eq!(
        doc_xml.matches("commentRangeStart").count(),
        1,
        "exactly one commentRangeStart"
    );
    assert_eq!(
        doc_xml.matches("commentRangeEnd").count(),
        1,
        "exactly one commentRangeEnd"
    );
    assert!(
        doc_xml.contains("commentReference"),
        "commentReference present"
    );
}

// ── Test 1: anchor in a Normal segment of a redlined paragraph ───────────────

/// The headline flow that failed live: a paragraph carries a tracked
/// counter-edit (the `w:ins`), and the reviewer comments on a stable Normal part
/// of the same clause. Domain rule: allowed — a comment is not a tracked change.
#[test]
fn comment_on_normal_segment_of_redlined_paragraph_succeeds() {
    let base = Document::parse(&tracked_insertion_clause()).expect("parse");
    let block_id = first_block_id(&base.snapshot().canonical);

    let edited = base
        .apply(&txn(vec![create_step(
            block_id,
            "The term is",
            "Confirm the revised term.",
        )]))
        .expect("comment on a redlined paragraph must be allowed");

    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let doc_xml = document_xml(&bytes);
    assert_fully_anchored(&doc_xml);
    // The tracked insertion is untouched by the comment.
    assert!(doc_xml.contains("<w:ins"), "w:ins preserved: {doc_xml}");
    assert!(doc_xml.contains(">three<"), "inserted word preserved");
    assert_annotations_clean(&bytes, "comment on normal segment");
}

// ── Test 2: endpoint inside an insertion widens to enclose the whole w:ins ────

/// `expect` = "hre" lies strictly inside the inserted word "three". A marker
/// must not split the `w:ins` mid-run, so the range widens outward to enclose
/// the whole insertion: commentRangeStart lands before `<w:ins>` and
/// commentRangeEnd after `</w:ins>`.
#[test]
fn comment_endpoint_inside_insertion_widens_to_enclose_whole_ins() {
    let base = Document::parse(&tracked_insertion_clause()).expect("parse");
    let block_id = first_block_id(&base.snapshot().canonical);

    let edited = base
        .apply(&txn(vec![create_step(
            block_id,
            "hre",
            "Comment landing inside the insertion.",
        )]))
        .expect("widening anchor must be allowed");

    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let doc_xml = document_xml(&bytes);
    assert_fully_anchored(&doc_xml);

    let start = doc_xml.find("commentRangeStart").expect("start present");
    let ins_open = doc_xml.find("<w:ins").expect("ins open");
    let ins_close = doc_xml.find("</w:ins>").expect("ins close");
    let end = doc_xml.find("commentRangeEnd").expect("end present");
    assert!(
        start < ins_open,
        "commentRangeStart widened to BEFORE the w:ins (no mid-run split): {doc_xml}"
    );
    assert!(
        ins_close < end,
        "commentRangeEnd widened to AFTER the w:ins (no mid-run split): {doc_xml}"
    );
    assert_annotations_clean(&bytes, "widened enclosing insertion");
}

// ── Test 3: a span crossing Normal → insertion → Normal encloses the w:ins ────

/// `expect` = "is three years" begins and ends in Normal text but crosses the
/// insertion. The range splits the two Normal segments and encloses the whole
/// `w:ins` in between.
#[test]
fn comment_spanning_into_insertion_encloses_it() {
    let base = Document::parse(&tracked_insertion_clause()).expect("parse");
    let block_id = first_block_id(&base.snapshot().canonical);

    let edited = base
        .apply(&txn(vec![create_step(
            block_id,
            "is three years",
            "Comment across the insertion.",
        )]))
        .expect("span crossing the insertion must be allowed");

    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let doc_xml = document_xml(&bytes);
    assert_fully_anchored(&doc_xml);
    let start = doc_xml.find("commentRangeStart").expect("start");
    let ins_open = doc_xml.find("<w:ins").expect("ins open");
    let ins_close = doc_xml.find("</w:ins>").expect("ins close");
    let end = doc_xml.find("commentRangeEnd").expect("end");
    assert!(
        start < ins_open && ins_close < end,
        "w:ins enclosed: {doc_xml}"
    );
    assert_annotations_clean(&bytes, "span enclosing insertion");
}

// ── Test 4: an anchor on deleted content is refused, actionably ──────────────

/// `expect` = "remove this" resolves onto struck (`w:del`) content. That comment
/// would vanish on accept-all, so it is refused — and the message must offer an
/// actionable alternative, not a bare "not found".
#[test]
fn comment_anchor_on_deleted_content_is_refused_with_alternative() {
    let canon = Document::parse(&tracked_deletion_clause())
        .expect("parse")
        .snapshot()
        .canonical
        .clone();
    let block_id = first_block_id(&canon);

    let err = apply_transaction(
        &canon,
        &txn(vec![create_step(
            block_id,
            "remove this",
            "on deleted text",
        )]),
    )
    .expect_err("comment on deleted text must fail");

    match &err {
        EditError::CommentAnchorOverlapsDeleted { expected, .. } => {
            assert_eq!(expected, "remove this");
        }
        other => panic!("expected CommentAnchorOverlapsDeleted, got {other:?}"),
    }
    let msg = err.to_string();
    assert!(
        msg.contains("target text that stays")
            || msg.contains("resolve the deletion")
            || msg.contains("comment before deleting"),
        "error must name an actionable alternative, got: {msg}"
    );
}

// ── Test 5: comment survives accept-all AND reject-all ───────────────────────

/// A comment enclosing the insertion must stay well-formed under BOTH
/// resolutions. On accept-all the insertion becomes permanent; on reject-all it
/// is removed and the range collapses to the remaining anchor — but the markers
/// stay PAIRED (they are Normal decorations, never inside the `w:ins`), so the
/// comment is retained, never orphaned. This pins the domain-correct reject-all
/// behavior: shrink the range, keep the comment.
#[test]
fn comment_enclosing_insertion_survives_accept_and_reject() {
    let base = Document::parse(&tracked_insertion_clause()).expect("parse");
    let block_id = first_block_id(&base.snapshot().canonical);

    let edited = base
        .apply(&txn(vec![create_step(
            block_id,
            "hre",
            "Survives both resolutions.",
        )]))
        .expect("comment applies");

    let accepted = edited.project(Resolution::AcceptAll).expect("accept");
    let rejected = edited.project(Resolution::RejectAll).expect("reject");

    for (doc, label) in [(&accepted, "accept-all"), (&rejected, "reject-all")] {
        let bytes = doc.serialize(&ExportOptions::default()).expect("serialize");
        let doc_xml = document_xml(&bytes);
        // Markers not orphaned: the range pair and the reference all survive.
        assert_fully_anchored(&doc_xml);
        assert_annotations_clean(&bytes, label);
    }

    // Accept-all keeps the inserted word; reject-all removes it (range collapses
    // but the comment is retained).
    let accepted_xml = document_xml(&accepted.serialize(&ExportOptions::default()).unwrap());
    let rejected_xml = document_xml(&rejected.serialize(&ExportOptions::default()).unwrap());
    assert!(accepted_xml.contains("three"), "accept keeps insertion");
    assert!(
        !rejected_xml.contains(">three<"),
        "reject removes the inserted word: {rejected_xml}"
    );
}

// ── Test 6: an unlocatable anchor is still CommentAnchorNotFound ──────────────

/// A phrase absent from the visible text (and not merely struck) still fails
/// with `CommentAnchorNotFound` — the tracked-anchor relaxation did not weaken
/// the not-found refusal.
#[test]
fn comment_absent_anchor_is_still_not_found() {
    let canon = Document::parse(&tracked_insertion_clause())
        .expect("parse")
        .snapshot()
        .canonical
        .clone();
    let block_id = first_block_id(&canon);

    let err = apply_transaction(
        &canon,
        &txn(vec![create_step(block_id, "NONEXISTENT PHRASE", "x")]),
    )
    .expect_err("absent anchor must fail");
    assert!(
        matches!(err, EditError::CommentAnchorNotFound { .. }),
        "expected CommentAnchorNotFound, got {err:?}"
    );
}
