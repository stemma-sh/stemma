//! A paragraph that is wholly another author's pending insertion, and whose
//! inserted content holds a range marker (a bookmark end, or a proofing-error
//! pair), is untouched by a tracked edit elsewhere in the document.
//!
//! Domain rules:
//! - An edit changes what it targets and nothing else. Ann's pending
//!   insertion is not part of Ben's edit, so after it the insertion is the
//!   same revision with the same content: in the edited session, in the
//!   saved file, and in the audit of either.
//! - A range marker written inside the `w:ins` (ECMA-376 §17.13.5.18) is part
//!   of the inserted content. Word writes bookmark ends and `w:proofErr`
//!   there; it does not make the paragraph anything but one whole insertion.
//! - One state, one spelling: the edited document and its save, reopened,
//!   read the same.
//!
//! Daily tier: synthetic in-memory DOCX.

use stemma::api::Document;
use stemma::audit::{AuditReport, RevisionDisposition};
use stemma::{ExportOptions, RevisionKind};

use crate::common::docx_with_body_and_comments;

const DATE: &str = r#"w:date="2026-10-01T09:00:00Z""#;
const EDITED: &str = "The ferry leaves the harbour at noon.";

/// Opening paragraph (which may open a bookmark), the paragraph Ben edits,
/// Ann's inserted paragraph with `inserted_content` inside her `w:ins`, and
/// a closing paragraph, so the insertion is not the story's last paragraph.
fn source(opening_marker: &str, inserted_content: &str) -> Vec<u8> {
    let body = format!(
        concat!(
            r#"<w:p><w:r><w:t>Opening paragraph.</w:t></w:r>{opening_marker}</w:p>"#,
            r#"<w:p><w:r><w:t>{edited}</w:t></w:r></w:p>"#,
            r#"<w:p><w:pPr><w:rPr><w:ins w:id="1" w:author="Ann" {date}/></w:rPr></w:pPr>"#,
            r#"<w:ins w:id="2" w:author="Ann" {date}>{inserted_content}</w:ins></w:p>"#,
            r#"<w:p><w:r><w:t>Closing paragraph.</w:t></w:r></w:p>"#,
        ),
        opening_marker = opening_marker,
        edited = EDITED,
        date = DATE,
        inserted_content = inserted_content,
    );
    docx_with_body_and_comments(&body, &[])
}

/// Ben replaces one word of the second paragraph, tracked.
fn replace_one_word(doc: &Document) -> Document {
    let json = serde_json::json!({
        "ops": [{"op": "replace", "target": "p_2", "expect": EDITED,
            "content": {"type": "paragraph", "content": [
                {"type": "text", "text": "The ferry leaves the harbour at dusk."}]}}],
        "revision": {"author": "Ben", "date": "2026-11-05T19:00:00Z"},
    })
    .to_string();
    doc.apply(
        &stemma::edit_v4::parse_transaction(&json)
            .unwrap()
            .into_edit_transaction()
            .unwrap(),
    )
    .expect("the replacement applies")
}

/// Ann's census rows: what a reader of the pending revisions sees.
fn anns_rows(doc: &Document) -> Vec<(u32, RevisionKind, String)> {
    doc.revisions()
        .into_iter()
        .filter(|r| r.author.as_deref() == Some("Ann"))
        .map(|r| (r.revision_id, r.kind, r.excerpt))
        .collect()
}

fn assert_prior_revisions_untouched(report: &AuditReport, context: &str) {
    let changed: Vec<_> = report
        .preexisting_revisions
        .iter()
        .filter(|prior| !matches!(prior.disposition, RevisionDisposition::Untouched))
        .collect();
    assert!(
        changed.is_empty(),
        "{context}: Ann's insertion is not part of Ben's edit: {changed:?}"
    );
    assert!(
        report
            .new_revisions
            .iter()
            .all(|r| r.author.as_deref() == Some("Ben")),
        "{context}: only Ben's edit is new: {:?}",
        report.new_revisions
    );
    assert!(
        !report.new_revisions.is_empty(),
        "{context}: Ben's edit is new"
    );
}

fn assert_unrelated_edit_leaves_insertion(original: &[u8]) {
    let source = Document::parse(original).expect("parse");
    let before = anns_rows(&source);
    assert!(
        !before.is_empty(),
        "Ann's insertion is pending in the source"
    );

    let edited = replace_one_word(&source);
    assert_eq!(anns_rows(&edited), before, "the edited session");
    assert_prior_revisions_untouched(&edited.review().expect("review"), "session review");

    let saved = edited.serialize(&ExportOptions::default()).expect("save");
    let reopened = Document::parse(&saved).expect("reopen");
    assert_eq!(anns_rows(&reopened), before, "the saved file");
    assert_prior_revisions_untouched(
        &stemma::audit(original, &saved).expect("audit"),
        "audit of the saved file",
    );

    let differences = stemma::roundtrip_compare::compare_canon_docs(
        &edited.snapshot().canonical,
        &reopened.snapshot().canonical,
    );
    assert!(
        differences.is_empty(),
        "the edited document and its save read the same: {differences:?}"
    );
}

#[test]
fn a_bookmark_end_inside_a_pending_insertion_survives_an_unrelated_edit() {
    // The bookmark opens in the first paragraph and closes inside Ann's
    // insertion.
    assert_unrelated_edit_leaves_insertion(&source(
        r#"<w:bookmarkStart w:id="7" w:name="Harbour"/>"#,
        r#"<w:r><w:t>Ann added a clause about the harbour.</w:t></w:r><w:bookmarkEnd w:id="7"/>"#,
    ));
}

#[test]
fn a_whole_bookmark_inside_a_pending_insertion_survives_an_unrelated_edit() {
    assert_unrelated_edit_leaves_insertion(&source(
        "",
        r#"<w:bookmarkStart w:id="7" w:name="Harbour"/><w:r><w:t>Ann added a clause about the harbour.</w:t></w:r><w:bookmarkEnd w:id="7"/>"#,
    ));
}

#[test]
fn a_proofing_mark_inside_a_pending_insertion_survives_an_unrelated_edit() {
    assert_unrelated_edit_leaves_insertion(&source(
        "",
        r#"<w:r><w:t xml:space="preserve">Ann added a clause about the </w:t></w:r><w:proofErr w:type="spellStart"/><w:r><w:t>harbr</w:t></w:r><w:proofErr w:type="spellEnd"/><w:r><w:t>.</w:t></w:r>"#,
    ));
}
