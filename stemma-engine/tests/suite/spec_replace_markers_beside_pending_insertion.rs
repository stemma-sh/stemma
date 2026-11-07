//! A whole-paragraph replace in a paragraph that already holds another
//! author's pending insertion keeps every bookmark, comment range and
//! permission range where it was.
//!
//! Domain rules: the edit changes only the text it rewrites, so a marker
//! outside the edited span keeps its position relative to the unchanged text
//! in both readings; and the edit adds a proposal on top of the existing ones,
//! so Reject All on the result reproduces Reject All on the source exactly,
//! markers included.
//!
//! Before the fix a marker's position was counted over the paragraph's text
//! including the pending insertion but re-placed in the original reading,
//! which leaves that insertion out, so every marker after the insertion slid
//! right by its length.
//!
//! Daily tier: synthetic in-memory DOCX.

use stemma::api::Document;
use stemma::domain::{BlockNode, NodeId, RevisionInfo};
use stemma::edit::{
    ContentFragment, EditStep, EditTransaction, MaterializationMode, ParagraphContent,
};
use stemma::{ExportOptions, Resolution};

use crate::common::{docx_with_body_and_comments, marker_stream, read_zip_entry};

const DATE: &str = r#"w:date="2026-10-01T09:00:00Z""#;

fn stream(doc: &Document) -> String {
    let bytes = doc.serialize(&ExportOptions::default()).expect("serialize");
    marker_stream(&read_zip_entry(&bytes, "word/document.xml").expect("document.xml"))
}

fn first_paragraph_id(doc: &Document) -> NodeId {
    doc.snapshot()
        .canonical
        .blocks
        .iter()
        .find_map(|tb| match &tb.block {
            BlockNode::Paragraph(p) => Some(p.id.clone()),
            _ => None,
        })
        .expect("a paragraph")
}

/// Replace the first paragraph's text with `new_text`, tracked.
fn replace_first_paragraph(doc: &Document, expect: &str, new_text: &str) -> Document {
    let txn = EditTransaction {
        steps: vec![EditStep::ReplaceParagraphText {
            block_id: first_paragraph_id(doc),
            rationale: None,
            replacement_role: None,
            expect: expect.to_string(),
            semantic_hash: None,
            content: ParagraphContent {
                fragments: vec![ContentFragment::Text(new_text.to_string())],
            },
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("Reviewer".to_string()),
            date: Some("2026-11-05T19:00:00Z".to_string()),
            apply_op_id: None,
        },
    };
    doc.apply(&txn).expect("apply replace")
}

/// Edit the paragraph and check both readings of the result: Accept All is
/// `accepted`, Reject All equals Reject All of the source and is `rejected`.
fn assert_edit_keeps_markers(
    body: &str,
    comment_ids: &[&str],
    expect: &str,
    new_text: &str,
    accepted: &str,
    rejected: &str,
) {
    let source = Document::parse(&docx_with_body_and_comments(body, comment_ids)).expect("parse");
    assert_eq!(
        stream(&source.project(Resolution::RejectAll).unwrap()),
        rejected,
        "the source's own Reject All"
    );
    let edited = replace_first_paragraph(&source, expect, new_text);
    let redline = stream(&edited);
    assert_eq!(
        stream(&edited.project(Resolution::AcceptAll).unwrap()),
        accepted,
        "Accept All of the edit; redline {redline}"
    );
    assert_eq!(
        stream(&edited.project(Resolution::RejectAll).unwrap()),
        rejected,
        "Reject All of the edit restores the source; redline {redline}"
    );
}

#[test]
fn bookmark_after_a_pending_insertion_stays_put() {
    let body = format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Alpha </w:t></w:r><w:ins w:id="10" w:author="Ann" {DATE}><w:r><w:t xml:space="preserve">new </w:t></w:r></w:ins><w:r><w:t xml:space="preserve">beta </w:t></w:r><w:bookmarkStart w:id="1" w:name="bm1"/><w:r><w:t>gamma</w:t></w:r><w:bookmarkEnd w:id="1"/><w:r><w:t xml:space="preserve"> delta.</w:t></w:r></w:p><w:p><w:r><w:t>Tail.</w:t></w:r></w:p>"#
    );
    assert_edit_keeps_markers(
        &body,
        &[],
        "Alpha",
        "ALPHA new beta gamma delta.",
        "ALPHA new beta {bs1}gamma{be1} delta.|Tail.|",
        "Alpha beta {bs1}gamma{be1} delta.|Tail.|",
    );
}

#[test]
fn comment_range_after_a_pending_insertion_stays_put() {
    let body = format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Alpha </w:t></w:r><w:ins w:id="10" w:author="Ann" {DATE}><w:r><w:t xml:space="preserve">new </w:t></w:r></w:ins><w:r><w:t xml:space="preserve">beta </w:t></w:r><w:commentRangeStart w:id="0"/><w:r><w:t>gamma</w:t></w:r><w:commentRangeEnd w:id="0"/><w:r><w:commentReference w:id="0"/></w:r><w:r><w:t xml:space="preserve"> delta.</w:t></w:r></w:p>"#
    );
    assert_edit_keeps_markers(
        &body,
        &["0"],
        "Alpha",
        "ALPHA new beta gamma delta.",
        "ALPHA new beta {cs0}gamma{ce0}{cr0} delta.|",
        "Alpha beta {cs0}gamma{ce0}{cr0} delta.|",
    );
}

#[test]
fn permission_range_after_a_pending_insertion_stays_put() {
    let body = format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Alpha </w:t></w:r><w:ins w:id="10" w:author="Ann" {DATE}><w:r><w:t xml:space="preserve">new </w:t></w:r></w:ins><w:r><w:t xml:space="preserve">beta </w:t></w:r><w:permStart w:id="5" w:edGrp="everyone"/><w:r><w:t>gamma</w:t></w:r><w:permEnd w:id="5"/><w:r><w:t xml:space="preserve"> delta.</w:t></w:r></w:p>"#
    );
    assert_edit_keeps_markers(
        &body,
        &[],
        "Alpha",
        "ALPHA new beta gamma delta.",
        "ALPHA new beta {ps5}gamma{pe5} delta.|",
        "Alpha beta {ps5}gamma{pe5} delta.|",
    );
}

/// The bookmark brackets exactly the pending insertion; an edit after it
/// leaves the bookmark around "new " in the accepted reading.
#[test]
fn bookmark_around_a_pending_insertion_stays_around_it() {
    let body = format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Alpha </w:t></w:r><w:bookmarkStart w:id="1" w:name="bm1"/><w:ins w:id="10" w:author="Ann" {DATE}><w:r><w:t xml:space="preserve">new </w:t></w:r></w:ins><w:bookmarkEnd w:id="1"/><w:r><w:t xml:space="preserve">beta gamma.</w:t></w:r></w:p>"#
    );
    assert_edit_keeps_markers(
        &body,
        &[],
        "beta",
        "Alpha new BETA gamma.",
        "Alpha {bs1}new {be1}BETA gamma.|",
        "Alpha {bs1}{be1}beta gamma.|",
    );
}

/// The bookmark's end sits inside the pending insertion; an edit before it
/// keeps the end between "ne" and "w".
#[test]
fn bookmark_end_inside_a_pending_insertion_stays_inside_it() {
    let body = format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Alpha </w:t></w:r><w:bookmarkStart w:id="1" w:name="bm1"/><w:r><w:t xml:space="preserve">beta </w:t></w:r><w:ins w:id="10" w:author="Ann" {DATE}><w:r><w:t>ne</w:t></w:r><w:bookmarkEnd w:id="1"/><w:r><w:t>w</w:t></w:r></w:ins><w:r><w:t xml:space="preserve"> gamma.</w:t></w:r></w:p>"#
    );
    assert_edit_keeps_markers(
        &body,
        &[],
        "Alpha",
        "ALPHA beta new gamma.",
        "ALPHA {bs1}beta ne{be1}w gamma.|",
        "Alpha {bs1}beta {be1} gamma.|",
    );
}

/// A bookmark wholly inside the pending insertion stays inside it: Reject
/// All removes it with the insertion, exactly as on the source.
#[test]
fn bookmark_inside_a_pending_insertion_stays_inside_it() {
    let body = format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Alpha </w:t></w:r><w:ins w:id="10" w:author="Ann" {DATE}><w:r><w:t xml:space="preserve">brand </w:t></w:r><w:bookmarkStart w:id="1" w:name="bm1"/><w:r><w:t>new</w:t></w:r><w:bookmarkEnd w:id="1"/><w:r><w:t xml:space="preserve"> text </w:t></w:r></w:ins><w:r><w:t>beta.</w:t></w:r></w:p>"#
    );
    assert_edit_keeps_markers(
        &body,
        &[],
        "beta",
        "Alpha brand new text BETA.",
        "Alpha brand {bs1}new{be1} text BETA.|",
        "Alpha beta.|",
    );
}
