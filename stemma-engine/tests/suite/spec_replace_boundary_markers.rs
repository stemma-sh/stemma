//! A bookmark or other zero-width marker sitting exactly at the edge of the
//! text a whole-paragraph replace or `set_cell_text` rewrites stays with the
//! kept text beside it, outside the edit's deletion, and is never written
//! into another author's revision.
//!
//! Domain rules: an edit changes only the text it rewrites, so a marker at
//! its edge keeps its position relative to the unchanged neighbour in both
//! readings (Accept All shows the replacement inside a range that bracketed
//! the replaced text); and Reject All of the edit reproduces the source.
//!
//! Before the fix such a marker took the status of the text after it, so a
//! marker before a rewritten word went into the new `w:del` (Accept All then
//! moved it, or dropped a point bookmark), and a marker before another
//! author's pending deletion went into that deletion.
//!
//! Daily tier: synthetic in-memory DOCX.

use stemma::api::Document;
use stemma::{ExportOptions, Resolution};

use crate::common::{docx_with_body_and_comments, marker_stream, read_zip_entry};

const DATE: &str = r#"w:date="2026-10-01T09:00:00Z""#;

fn stream(doc: &Document) -> String {
    let bytes = doc.serialize(&ExportOptions::default()).expect("serialize");
    marker_stream(&read_zip_entry(&bytes, "word/document.xml").expect("document.xml"))
}

fn apply_v4(doc: &Document, ops: serde_json::Value) -> Document {
    let json = serde_json::json!({
        "ops": ops,
        "revision": {"author": "Reviewer", "date": "2026-11-05T19:00:00Z"},
    })
    .to_string();
    doc.apply(
        &stemma::edit_v4::parse_transaction(&json)
            .unwrap()
            .into_edit_transaction()
            .unwrap(),
    )
    .expect("apply")
}

/// Check the redline, its Accept All, and that its Reject All equals the
/// source's own.
fn assert_edit(source: &Document, edited: &Document, redline: &str, accepted: &str) {
    assert_eq!(stream(edited), redline, "redline");
    assert_eq!(
        stream(&edited.project(Resolution::AcceptAll).unwrap()),
        accepted,
        "Accept All"
    );
    assert_eq!(
        stream(&edited.project(Resolution::RejectAll).unwrap()),
        stream(&source.project(Resolution::RejectAll).unwrap()),
        "Reject All restores the source"
    );
}

fn replace_first_paragraph(doc: &Document, expect: &str, text: &str) -> Document {
    let target = doc.read().blocks[0].id.to_string();
    apply_v4(
        doc,
        serde_json::json!([{"op": "replace", "target": target, "expect": expect,
            "content": {"type": "paragraph", "content": [{"type": "text", "text": text}]}}]),
    )
}

#[test]
fn bookmark_end_before_a_rewritten_word_stays_outside_its_deletion() {
    let body = r#"<w:p><w:r><w:t xml:space="preserve">Alpha </w:t></w:r><w:bookmarkStart w:id="50" w:name="bm0"/><w:r><w:t xml:space="preserve">beta </w:t></w:r><w:bookmarkEnd w:id="50"/><w:r><w:t>gamma delta.</w:t></w:r></w:p>"#;
    let source = Document::parse(&docx_with_body_and_comments(body, &[])).unwrap();
    let edited = replace_first_paragraph(&source, "gamma", "Alpha beta GAMMA delta.");
    assert_edit(
        &source,
        &edited,
        "Alpha {bs50}beta {be50}<-gamma-><+GAMMA+> delta.|",
        "Alpha {bs50}beta {be50}GAMMA delta.|",
    );
}

#[test]
fn point_bookmark_before_a_rewritten_word_survives_accept() {
    let body = r#"<w:p><w:r><w:t xml:space="preserve">Alpha beta </w:t></w:r><w:bookmarkStart w:id="50" w:name="pt"/><w:bookmarkEnd w:id="50"/><w:r><w:t>gamma delta.</w:t></w:r></w:p>"#;
    let source = Document::parse(&docx_with_body_and_comments(body, &[])).unwrap();
    let edited = replace_first_paragraph(&source, "gamma", "Alpha beta GAMMA delta.");
    assert_edit(
        &source,
        &edited,
        "Alpha beta {bs50}{be50}<-gamma-><+GAMMA+> delta.|",
        "Alpha beta {bs50}{be50}GAMMA delta.|",
    );
}

/// The bookmark start sits right after the rewritten word: it stays with the
/// kept " beta" after the replacement.
#[test]
fn bookmark_start_after_a_rewritten_word_stays_with_the_kept_text() {
    let body = r#"<w:p><w:r><w:t>Alpha</w:t></w:r><w:bookmarkStart w:id="1" w:name="bm1"/><w:r><w:t xml:space="preserve"> beta</w:t></w:r><w:bookmarkEnd w:id="1"/><w:r><w:t xml:space="preserve"> gamma.</w:t></w:r></w:p>"#;
    let source = Document::parse(&docx_with_body_and_comments(body, &[])).unwrap();
    let edited = replace_first_paragraph(&source, "Alpha", "ALPHA beta gamma.");
    assert_edit(
        &source,
        &edited,
        "<-Alpha-><+ALPHA+>{bs1} beta{be1} gamma.|",
        "ALPHA{bs1} beta{be1} gamma.|",
    );
}

/// An edit elsewhere must not move a bookmark end into another author's
/// pending deletion that follows it.
#[test]
fn bookmark_end_before_another_authors_deletion_stays_outside_it() {
    let body = format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Alpha </w:t></w:r><w:bookmarkStart w:id="1" w:name="bm1"/><w:r><w:t>beta</w:t></w:r><w:bookmarkEnd w:id="1"/><w:del w:id="10" w:author="Ann" {DATE}><w:r><w:delText xml:space="preserve"> old</w:delText></w:r></w:del><w:r><w:t xml:space="preserve"> gamma delta.</w:t></w:r></w:p>"#
    );
    let source = Document::parse(&docx_with_body_and_comments(&body, &[])).unwrap();
    let edited = replace_first_paragraph(&source, "delta", "Alpha beta gamma DELTA.");
    let redline = stream(&edited);
    assert!(
        redline.starts_with("Alpha {bs1}beta{be1}<- old->"),
        "the end stays before Ann's deletion: {redline}"
    );
    assert_edit(
        &source,
        &edited,
        &redline,
        "Alpha {bs1}beta{be1} gamma DELTA.|",
    );
}

/// `set_cell_text` rewrites the cell paragraph through the same splice: the
/// bookmark that brackets "30" brackets its replacement.
#[test]
fn set_cell_text_keeps_edge_bookmarks_outside_the_deletion() {
    let body = r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="4000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t xml:space="preserve">Net </w:t></w:r><w:bookmarkStart w:id="1" w:name="bm1"/><w:r><w:t>30</w:t></w:r><w:bookmarkEnd w:id="1"/><w:r><w:t xml:space="preserve"> days</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:r><w:t>After.</w:t></w:r></w:p>"#;
    let source = Document::parse(&docx_with_body_and_comments(body, &[])).unwrap();
    let table = source.read().blocks[0].id.to_string();
    let edited = apply_v4(
        &source,
        serde_json::json!([{"op": "table_op", "target": table, "table_op":
            {"kind": "set_cell_text", "row_index": 0, "col_index": 0, "text": "Net 45 days"}}]),
    );
    assert_edit(
        &source,
        &edited,
        "[tblNet {bs1}<-30-><+45+>{be1} days|tbl]After.|",
        "[tblNet {bs1}45{be1} days|tbl]After.|",
    );
}
