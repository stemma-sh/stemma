//! Resolving a revision that holds ONE end of a bookmark, comment range or
//! permission range keeps that end where the removed text was.
//!
//! Domain rule (ECMA-376 §17.13.6): a range is a pair of POSITIONS in the
//! document, not a pair of characters. Accepting a deletion or rejecting an
//! insertion removes characters; a range marker that sat among them is not a
//! character, so it stays at the removal point, and the text between the
//! surviving end and the removal point stays inside the range. Only a range
//! that lies wholly within one removed stretch goes with it (deleting a
//! selection that contains a whole bookmark deletes the bookmark).
//!
//! Before the fix both resolution paths dropped the marker with its revision
//! and then re-inserted it next to the surviving end, shrinking the range to
//! nothing although text inside it survived.
//!
//! Daily tier: synthetic in-memory DOCX.

use std::collections::HashSet;

use stemma::api::Document;
use stemma::docx::DocxArchive;
use stemma::normalize::{normalize_docx, reject_all_docx};
use stemma::{ExportOptions, Resolution, ResolveSelectionAction};

use crate::common::{docx_with_body_and_comments, marker_stream, read_zip_entry};

const DATE: &str = r#"w:date="2026-10-01T09:00:00Z""#;

fn stream_of(bytes: &[u8]) -> String {
    marker_stream(&read_zip_entry(bytes, "word/document.xml").expect("document.xml"))
}

fn model_resolve(bytes: &[u8], resolution: Resolution) -> String {
    let doc = Document::parse(bytes).expect("parse");
    let out = doc
        .project(resolution)
        .expect("project")
        .serialize(&ExportOptions::default())
        .expect("serialize");
    stream_of(&out)
}

fn byte_resolve(bytes: &[u8], accept: bool) -> String {
    let archive = DocxArchive::read(bytes).expect("read");
    let (out, _) = if accept {
        normalize_docx(&archive).expect("accept")
    } else {
        reject_all_docx(&archive).expect("reject")
    };
    stream_of(&out.write().expect("write archive"))
}

fn all_revision_ids(bytes: &[u8]) -> HashSet<u32> {
    let doc = Document::parse(bytes).expect("parse");
    stemma::enumerate_revisions(&doc.snapshot().canonical)
        .into_iter()
        .map(|r| r.revision_id)
        .collect()
}

/// Assert both full-resolution paths (model and byte) and the selective path
/// over every revision produce `expected`.
fn assert_resolves_to(bytes: &[u8], accept: bool, expected: &str) {
    let (full, action) = if accept {
        (Resolution::AcceptAll, ResolveSelectionAction::Accept)
    } else {
        (Resolution::RejectAll, ResolveSelectionAction::Reject)
    };
    assert_eq!(
        model_resolve(bytes, full),
        expected,
        "model full resolution"
    );
    assert_eq!(
        byte_resolve(bytes, accept),
        expected,
        "byte full resolution"
    );
    let ids = all_revision_ids(bytes);
    assert_eq!(
        model_resolve(bytes, Resolution::Selective { ids, action }),
        expected,
        "selective resolution of every revision"
    );
}

#[test]
fn accept_keeps_a_bookmark_start_that_sat_in_deleted_text() {
    let body = format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Alpha </w:t></w:r><w:del w:id="10" w:author="Ann" {DATE}><w:r><w:delText>ol</w:delText></w:r><w:bookmarkStart w:id="1" w:name="bm1"/><w:r><w:delText>d</w:delText></w:r></w:del><w:r><w:t xml:space="preserve"> beta</w:t></w:r><w:bookmarkEnd w:id="1"/><w:r><w:t xml:space="preserve"> gamma.</w:t></w:r></w:p>"#
    );
    let bytes = docx_with_body_and_comments(&body, &[]);
    assert_eq!(stream_of(&bytes), "Alpha <-ol{bs1}d-> beta{be1} gamma.|");
    // " beta" was inside the bookmark and survives, so it stays inside.
    assert_resolves_to(&bytes, true, "Alpha {bs1} beta{be1} gamma.|");
}

#[test]
fn reject_keeps_a_comment_range_end_that_sat_in_inserted_text() {
    let body = format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Alpha </w:t></w:r><w:commentRangeStart w:id="0"/><w:r><w:t xml:space="preserve">beta </w:t></w:r><w:ins w:id="10" w:author="Ann" {DATE}><w:r><w:t>ne</w:t></w:r><w:commentRangeEnd w:id="0"/><w:r><w:t>w</w:t></w:r></w:ins><w:r><w:commentReference w:id="0"/></w:r><w:r><w:t xml:space="preserve"> gamma.</w:t></w:r></w:p>"#
    );
    let bytes = docx_with_body_and_comments(&body, &["0"]);
    assert_eq!(
        stream_of(&bytes),
        "Alpha {cs0}beta <+ne{ce0}w+>{cr0} gamma.|"
    );
    assert_resolves_to(&bytes, false, "Alpha {cs0}beta {ce0}{cr0} gamma.|");
}

#[test]
fn reject_keeps_a_permission_start_that_sat_in_inserted_text() {
    let body = format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Alpha </w:t></w:r><w:ins w:id="10" w:author="Ann" {DATE}><w:r><w:t xml:space="preserve">new </w:t></w:r><w:permStart w:id="2" w:edGrp="everyone"/></w:ins><w:r><w:t>beta</w:t></w:r><w:permEnd w:id="2"/><w:r><w:t xml:space="preserve"> gamma.</w:t></w:r></w:p>"#
    );
    let bytes = docx_with_body_and_comments(&body, &[]);
    assert_eq!(stream_of(&bytes), "Alpha <+new {ps2}+>beta{pe2} gamma.|");
    assert_resolves_to(&bytes, false, "Alpha {ps2}beta{pe2} gamma.|");
}

/// Two separate deletions each hold one end; the text between them survives,
/// so the range survives around it.
#[test]
fn accept_keeps_a_range_whose_two_ends_sat_in_separate_deletions() {
    let body = format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Alpha </w:t></w:r><w:del w:id="10" w:author="Ann" {DATE}><w:r><w:delText>x</w:delText></w:r><w:bookmarkStart w:id="1" w:name="bm1"/></w:del><w:r><w:t xml:space="preserve"> mid </w:t></w:r><w:del w:id="11" w:author="Ann" {DATE}><w:bookmarkEnd w:id="1"/><w:r><w:delText>y</w:delText></w:r></w:del><w:r><w:t xml:space="preserve"> gamma.</w:t></w:r></w:p>"#
    );
    let bytes = docx_with_body_and_comments(&body, &[]);
    assert_eq!(stream_of(&bytes), "Alpha <-x{bs1}-> mid <-{be1}y-> gamma.|");
    assert_resolves_to(&bytes, true, "Alpha {bs1} mid {be1} gamma.|");
}

/// A range wholly inside one removed stretch is removed with it.
#[test]
fn reject_removes_a_range_that_lies_wholly_in_one_insertion() {
    let body = format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Alpha </w:t></w:r><w:ins w:id="10" w:author="Ann" {DATE}><w:bookmarkStart w:id="1" w:name="bm1"/><w:r><w:t>new</w:t></w:r><w:bookmarkEnd w:id="1"/></w:ins><w:r><w:t xml:space="preserve"> gamma.</w:t></w:r></w:p>"#
    );
    let bytes = docx_with_body_and_comments(&body, &[]);
    assert_resolves_to(&bytes, false, "Alpha  gamma.|");
}

/// A whole deleted paragraph (its text and its mark) holding a bookmark's
/// end: accepting removes "Second para here.¶", so the end lands where that
/// removal leaves the reading position, the start of the next paragraph, and
/// "gamma." stays inside the bookmark.
#[test]
fn accept_keeps_a_bookmark_end_that_sat_in_a_deleted_paragraph() {
    let body = format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Alpha beta </w:t></w:r><w:bookmarkStart w:id="1" w:name="bm1"/><w:r><w:t>gamma.</w:t></w:r></w:p><w:p><w:pPr><w:rPr><w:del w:id="3" w:author="Ann" {DATE}/></w:rPr></w:pPr><w:del w:id="4" w:author="Ann" {DATE}><w:r><w:delText>Second</w:delText></w:r><w:bookmarkEnd w:id="1"/><w:r><w:delText xml:space="preserve"> para here.</w:delText></w:r></w:del></w:p><w:p><w:r><w:t>Third para.</w:t></w:r></w:p>"#
    );
    let bytes = docx_with_body_and_comments(&body, &[]);
    assert_eq!(
        stream_of(&bytes),
        "Alpha beta {bs1}gamma.|<-Second{be1} para here.->|Third para.|"
    );
    assert_resolves_to(&bytes, true, "Alpha beta {bs1}gamma.|{be1}Third para.|");
}

/// Rejecting an inserted paragraph (its text and its mark) holding a
/// bookmark's start: the start lands at the start of the next paragraph and
/// "Third" stays inside the bookmark.
#[test]
fn reject_keeps_a_bookmark_start_that_sat_in_an_inserted_paragraph() {
    let body = format!(
        r#"<w:p><w:r><w:t>Alpha.</w:t></w:r></w:p><w:p><w:pPr><w:rPr><w:ins w:id="3" w:author="Ann" {DATE}/></w:rPr></w:pPr><w:ins w:id="4" w:author="Ann" {DATE}><w:r><w:t xml:space="preserve">New </w:t></w:r><w:bookmarkStart w:id="1" w:name="bm1"/><w:r><w:t>para.</w:t></w:r></w:ins></w:p><w:p><w:r><w:t>Third</w:t></w:r><w:bookmarkEnd w:id="1"/><w:r><w:t xml:space="preserve"> para.</w:t></w:r></w:p>"#
    );
    let bytes = docx_with_body_and_comments(&body, &[]);
    assert_eq!(
        stream_of(&bytes),
        "Alpha.|<+New {bs1}para.+>|Third{be1} para.|"
    );
    assert_resolves_to(&bytes, false, "Alpha.|{bs1}Third{be1} para.|");
}
