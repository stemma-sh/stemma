//! Reject All removes an inserted (or moved-in) paragraph that stands before a
//! table even when it carries a bookmark or a comment range, and keeps those
//! markers in the paragraph before it.
//!
//! Domain rules: rejecting a paragraph's inserted mark removes the paragraph
//! break (ECMA-376 §17.13.5.20); with a table next there is no paragraph to
//! join, and a paragraph left with no text is not kept as an empty husk. The
//! bookmark / comment markers are zero-width positions of the original
//! document, not content: they survive the removal, at the removal point,
//! which is the end of the paragraph before (the table cannot hold them).
//!
//! Before the fix the zero-width markers counted as content that kept the
//! emptied paragraph alive. Commit-history twin: the move-range brackets of a
//! rejected move, which were already excluded.
//!
//! Daily tier: synthetic in-memory DOCX.

use std::collections::HashSet;

use stemma::api::Document;
use stemma::docx::DocxArchive;
use stemma::normalize::reject_all_docx;
use stemma::{ExportOptions, Resolution, ResolveSelectionAction};

use crate::common::{docx_with_body_and_comments, marker_stream, read_zip_entry};

const DATE: &str = r#"w:date="2026-10-01T09:00:00Z""#;
const TABLE: &str = r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="4000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>Cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#;

fn stream_of(bytes: &[u8]) -> String {
    marker_stream(&read_zip_entry(bytes, "word/document.xml").expect("document.xml"))
}

/// Reject All through the model (full and every-revision selective) and
/// the byte path.
fn assert_rejects_to(bytes: &[u8], expected: &str) {
    assert_full_rejects_to(bytes, expected);
    let doc = Document::parse(bytes).expect("parse");
    let ids: HashSet<u32> = stemma::enumerate_revisions(&doc.snapshot().canonical)
        .into_iter()
        .map(|r| r.revision_id)
        .collect();
    let selective = doc
        .project(Resolution::Selective {
            ids,
            action: ResolveSelectionAction::Reject,
        })
        .expect("selective reject")
        .serialize(&ExportOptions::default())
        .expect("serialize");
    assert_eq!(
        stream_of(&selective),
        expected,
        "selective reject of every revision"
    );
}

/// Reject All through the model and the byte path.
fn assert_full_rejects_to(bytes: &[u8], expected: &str) {
    let model = Document::parse(bytes)
        .expect("parse")
        .project(Resolution::RejectAll)
        .expect("reject")
        .serialize(&ExportOptions::default())
        .expect("serialize");
    assert_eq!(stream_of(&model), expected, "model Reject All");
    let (archive, _) = reject_all_docx(&DocxArchive::read(bytes).unwrap()).expect("byte reject");
    assert_eq!(
        stream_of(&archive.write().unwrap()),
        expected,
        "byte Reject All"
    );
}

#[test]
fn inserted_paragraph_with_a_bookmark_before_a_table_goes_on_reject() {
    let body = format!(
        r#"<w:p><w:r><w:t>Alpha.</w:t></w:r></w:p><w:p><w:pPr><w:rPr><w:ins w:id="11" w:author="Ann" {DATE}/></w:rPr></w:pPr><w:bookmarkStart w:id="1" w:name="bm1"/><w:ins w:id="10" w:author="Ann" {DATE}><w:r><w:t>Inserted para.</w:t></w:r></w:ins><w:bookmarkEnd w:id="1"/></w:p>{TABLE}<w:p><w:r><w:t>Tail.</w:t></w:r></w:p>"#
    );
    let bytes = docx_with_body_and_comments(&body, &[]);
    assert_rejects_to(&bytes, "Alpha.{bs1}{be1}|[tblCell|tbl]Tail.|");
}

#[test]
fn inserted_paragraph_with_a_comment_before_a_table_goes_on_reject() {
    let body = format!(
        r#"<w:p><w:r><w:t>Alpha.</w:t></w:r></w:p><w:p><w:pPr><w:rPr><w:ins w:id="11" w:author="Ann" {DATE}/></w:rPr></w:pPr><w:commentRangeStart w:id="0"/><w:ins w:id="10" w:author="Ann" {DATE}><w:r><w:t>Inserted para.</w:t></w:r></w:ins><w:commentRangeEnd w:id="0"/><w:r><w:commentReference w:id="0"/></w:r></w:p>{TABLE}<w:p><w:r><w:t>Tail.</w:t></w:r></w:p>"#
    );
    let bytes = docx_with_body_and_comments(&body, &["0"]);
    assert_rejects_to(&bytes, "Alpha.{cs0}{ce0}{cr0}|[tblCell|tbl]Tail.|");
}

#[test]
fn moved_in_paragraph_with_a_bookmark_before_a_table_goes_on_reject() {
    let body = format!(
        r#"<w:p><w:pPr><w:rPr><w:moveFrom w:id="27" w:author="Mia" {DATE}/></w:rPr></w:pPr><w:moveFromRangeStart w:id="20" w:author="Mia" {DATE} w:name="mv1"/><w:moveFrom w:id="21" w:author="Mia" {DATE}><w:r><w:t>Moved text.</w:t></w:r></w:moveFrom><w:moveFromRangeEnd w:id="20"/></w:p><w:p><w:r><w:t>Middle.</w:t></w:r></w:p><w:p><w:pPr><w:rPr><w:moveTo w:id="28" w:author="Mia" {DATE}/></w:rPr></w:pPr><w:bookmarkStart w:id="1" w:name="bm1"/><w:moveToRangeStart w:id="22" w:author="Mia" {DATE} w:name="mv1"/><w:moveTo w:id="23" w:author="Mia" {DATE}><w:r><w:t>Moved text.</w:t></w:r></w:moveTo><w:moveToRangeEnd w:id="22"/><w:bookmarkEnd w:id="1"/></w:p>{TABLE}<w:p><w:r><w:t>Tail.</w:t></w:r></w:p>"#
    );
    let bytes = docx_with_body_and_comments(&body, &[]);
    // Full resolution only: a selective reject lays a move out by its own
    // rule (it restores the source with the destination's markers).
    assert_full_rejects_to(&bytes, "Moved text.|Middle.{bs1}{be1}|[tblCell|tbl]Tail.|");
}

/// The same inside a table cell, before a nested table.
#[test]
fn inserted_cell_paragraph_with_a_bookmark_before_a_nested_table_goes_on_reject() {
    let body = format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="6000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="6000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>Alpha.</w:t></w:r></w:p><w:p><w:pPr><w:rPr><w:ins w:id="11" w:author="Ann" {DATE}/></w:rPr></w:pPr><w:bookmarkStart w:id="1" w:name="bm1"/><w:ins w:id="10" w:author="Ann" {DATE}><w:r><w:t>Inserted para.</w:t></w:r></w:ins><w:bookmarkEnd w:id="1"/></w:p>{TABLE}<w:p><w:r><w:t>Cell tail.</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:r><w:t>Tail.</w:t></w:r></w:p>"#
    );
    let bytes = docx_with_body_and_comments(&body, &[]);
    assert_rejects_to(
        &bytes,
        "[tblAlpha.{bs1}{be1}|[tblCell|tbl]Cell tail.|tbl]Tail.|",
    );
}
