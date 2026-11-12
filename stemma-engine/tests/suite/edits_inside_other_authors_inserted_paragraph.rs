//! A second author edits a paragraph that is wholly another author's pending
//! insertion: a span replace, a formatting change, a deletion and a comment
//! stack on the insertion instead of being refused.
//!
//! Domain rules (ECMA-376 §17.13.5; Word keeps each reviewer's change as its
//! own proposal):
//! - Each author's change resolves on its own. Rejecting Ann's insertion
//!   removes her text and paragraph mark, and a change Ben stacked on that
//!   text goes with it (the cascade); text Ben inserted beside hers is his own
//!   proposal and stays pending, joined to the next paragraph once Ann's mark
//!   is gone. Reject All leaves the source without the paragraph.
//! - Rejecting only Ben's change restores Ann's pending insertion exactly;
//!   accepting only Ann's leaves Ben's change pending on what is now base
//!   text.
//! - Accept All shows Ben's edit applied to Ann's text (a deletion stacked on
//!   the insertion removes the paragraph in both full resolutions).
//! - One state, one spelling: the edited document and its save, reopened,
//!   read the same.
//!
//! Daily tier: synthetic in-memory DOCX.

use std::collections::HashSet;

use stemma::api::Document;
use stemma::{ExportOptions, Resolution, ResolveSelectionAction};

use crate::common::docx_with_body_and_comments;

const DATE: &str = r#"w:date="2026-10-01T09:00:00Z""#;
const INSERTED: &str = "Ann drafted this whole new clause.";

fn source() -> Document {
    let body = format!(
        r#"<w:p><w:r><w:t>Opening clause.</w:t></w:r></w:p><w:p><w:pPr><w:rPr><w:ins w:id="1" w:author="Ann" {DATE}/></w:rPr></w:pPr><w:ins w:id="2" w:author="Ann" {DATE}><w:r><w:t>{INSERTED}</w:t></w:r></w:ins></w:p><w:p><w:r><w:t>Closing clause.</w:t></w:r></w:p>"#
    );
    Document::parse(&docx_with_body_and_comments(&body, &[])).expect("parse")
}

fn apply(doc: &Document, op: serde_json::Value) -> Document {
    let json = serde_json::json!({
        "ops": [op],
        "revision": {"author": "Ben", "date": "2026-11-05T19:00:00Z"},
    })
    .to_string();
    doc.apply(
        &stemma::edit_v4::parse_transaction(&json)
            .unwrap()
            .into_edit_transaction()
            .unwrap(),
    )
    .expect("an edit inside another author's inserted paragraph applies")
}

fn texts(doc: &Document) -> Vec<String> {
    doc.read().blocks.iter().map(|b| b.text.clone()).collect()
}

fn ids_by(doc: &Document, author: &str) -> HashSet<u32> {
    doc.revisions()
        .into_iter()
        .filter(|r| r.author.as_deref() == Some(author))
        .map(|r| r.revision_id)
        .collect()
}

fn resolve(doc: &Document, author: &str, action: ResolveSelectionAction) -> Document {
    let ids = ids_by(doc, author);
    assert!(!ids.is_empty(), "{author} has a pending revision");
    doc.project(Resolution::Selective { ids, action })
        .expect("selective resolution")
}

fn reopen(doc: &Document) -> Document {
    Document::parse(&doc.serialize(&ExportOptions::default()).expect("save")).expect("reopen")
}

/// The laws every stacked edit keeps. `accepted` is the text Accept All
/// leaves, `None` when the paragraph is gone; `ben_left` is what remains of
/// Ben's own insertion once Ann's is rejected, joined to the next paragraph.
fn assert_laws(edited: &Document, accepted: Option<&str>, ben_left: &str) {
    let source = source();
    let with = |middle: Option<&str>| -> Vec<String> {
        let mut out = vec!["Opening clause.".to_string()];
        out.extend(middle.map(str::to_string));
        out.push("Closing clause.".to_string());
        out
    };

    assert_eq!(
        texts(&edited.project(Resolution::AcceptAll).unwrap()),
        with(accepted),
        "Accept All"
    );
    assert_eq!(
        texts(&edited.project(Resolution::RejectAll).unwrap()),
        with(None),
        "Reject All removes Ann's paragraph"
    );
    let without_ann = resolve(edited, "Ann", ResolveSelectionAction::Reject);
    assert_eq!(
        texts(&without_ann),
        vec![
            "Opening clause.".to_string(),
            format!("{ben_left}Closing clause.")
        ],
        "rejecting Ann's insertion takes what Ben stacked on it"
    );
    assert!(ids_by(&without_ann, "Ann").is_empty());
    assert_eq!(
        texts(&without_ann.project(Resolution::RejectAll).unwrap()),
        with(None)
    );

    // Rejecting only Ben's change leaves Ann's insertion as it was.
    let without_ben = resolve(edited, "Ben", ResolveSelectionAction::Reject);
    assert!(ids_by(&without_ben, "Ben").is_empty());
    assert_eq!(texts(&without_ben), texts(&source));
    assert_eq!(
        ids_by(&without_ben, "Ann"),
        ids_by(&source, "Ann"),
        "Ann's insertion is still pending, under its own id"
    );
    assert_eq!(
        texts(&without_ben.project(Resolution::RejectAll).unwrap()),
        with(None)
    );

    // Accepting only Ann's leaves Ben's change pending on base text.
    let with_ann = resolve(edited, "Ann", ResolveSelectionAction::Accept);
    assert!(ids_by(&with_ann, "Ann").is_empty());
    assert!(
        !ids_by(&with_ann, "Ben").is_empty(),
        "Ben's change is pending"
    );
    assert_eq!(
        texts(&with_ann.project(Resolution::RejectAll).unwrap()),
        with(Some(INSERTED)),
        "rejecting Ben's change then restores Ann's accepted text"
    );
    assert_eq!(
        texts(&with_ann.project(Resolution::AcceptAll).unwrap()),
        with(accepted)
    );

    // One state, one spelling.
    let reopened = reopen(edited);
    let differences = stemma::roundtrip_compare::compare_canon_docs(
        &edited.snapshot().canonical,
        &reopened.snapshot().canonical,
    );
    assert!(
        differences.is_empty(),
        "save and reopen reads the same: {:?}",
        &differences[..differences.len().min(3)]
    );
}

fn inserted_paragraph_guard(doc: &Document) -> String {
    doc.read().blocks[1].guard.clone()
}

#[test]
fn a_span_replace_stacks_on_the_insertion() {
    let doc = source();
    let edited = apply(
        &doc,
        serde_json::json!({"op": "replace", "target": "p_2", "span": "s_0",
            "guard": inserted_paragraph_guard(&doc),
            "content": {"type": "paragraph", "content": [
                {"type": "text", "text": "Ann drafted this short new clause."}]}}),
    );
    assert_laws(&edited, Some("Ann drafted this short new clause."), "short");
}

#[test]
fn a_whole_paragraph_replace_stacks_on_the_insertion() {
    let edited = apply(
        &source(),
        serde_json::json!({"op": "replace", "target": "p_2", "expect": INSERTED,
            "content": {"type": "paragraph", "content": [
                {"type": "text", "text": "Ann drafted this short new clause."}]}}),
    );
    assert_laws(&edited, Some("Ann drafted this short new clause."), "short");
}

#[test]
fn a_format_change_stacks_on_the_insertion() {
    let edited = apply(
        &source(),
        serde_json::json!({"op": "set_format", "target": "p_2", "expect": "whole",
            "marks": {"bold": true}}),
    );
    assert_laws(&edited, Some(INSERTED), "");
    let bold = |doc: &Document| doc.to_markdown().contains("<b>whole</b>");
    assert!(bold(&edited.project(Resolution::AcceptAll).unwrap()));
    assert!(!bold(&resolve(
        &edited,
        "Ben",
        ResolveSelectionAction::Reject
    )));
    assert!(bold(&resolve(
        &edited,
        "Ann",
        ResolveSelectionAction::Accept
    )));
}

#[test]
fn a_deletion_stacks_on_the_insertion() {
    let edited = apply(
        &source(),
        serde_json::json!({"op": "delete", "target": "p_2", "expect": INSERTED}),
    );
    assert_laws(&edited, None, "");
}

#[test]
fn a_comment_anchors_in_the_insertion() {
    let source = source();
    let edited = apply(
        &source,
        serde_json::json!({"op": "comment_create", "target": "p_2", "expect": "whole",
            "body": "Is this needed?", "author": "Ben"}),
    );
    // A comment is an annotation, not a revision: the texts and Ann's
    // insertion are unchanged, in every reading.
    assert_eq!(
        texts(&edited.project(Resolution::AcceptAll).unwrap())[1].trim_end_matches('\u{FFFC}'),
        INSERTED
    );
    assert_eq!(ids_by(&edited, "Ann"), ids_by(&source, "Ann"));
    assert!(ids_by(&edited, "Ben").is_empty());
    assert_eq!(edited.snapshot().canonical.comments.len(), 1);
    let reopened = reopen(&edited);
    let differences = stemma::roundtrip_compare::compare_canon_docs(
        &edited.snapshot().canonical,
        &reopened.snapshot().canonical,
    );
    assert!(
        differences.is_empty(),
        "{:?}",
        &differences[..differences.len().min(3)]
    );
}
