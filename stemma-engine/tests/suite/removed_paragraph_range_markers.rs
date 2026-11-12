//! Where the range markers of a paragraph removed whole (its text and its
//! mark deleted) go on Accept, as Word places them (Word 16, verified on the
//! oracle):
//! - a range lying wholly inside the paragraph (bookmark, point bookmark,
//!   comment range) is dropped, and a comment goes with its range;
//! - a surviving range START is re-anchored at the start of the next
//!   surviving paragraph, before its first run;
//! - a surviving range END is re-anchored right after the previous surviving
//!   paragraph, as a body-level marker, so the range still covers that
//!   paragraph's mark;
//! - Reject puts everything back where it was.
//!
//! Accept All and accepting by id agree.

use std::collections::HashSet;

use stemma::api::Document;
use stemma::{ExportOptions, Resolution, ResolveSelectionAction};

use crate::common::{docx_with_body_and_comments, read_zip_entry};

const DATE: &str = r#"w:date="2026-04-02T07:00:00Z""#;

fn run(text: &str) -> String {
    format!(r#"<w:r><w:t xml:space="preserve">{text}</w:t></w:r>"#)
}

fn deleted(id: u32, content: &str) -> String {
    format!(r#"<w:del w:id="{id}" w:author="Ann" {DATE}>{content}</w:del>"#)
}

fn deleted_text(id: u32, text: &str) -> String {
    deleted(
        id,
        &format!(r#"<w:r><w:delText xml:space="preserve">{text}</w:delText></w:r>"#),
    )
}

const REFERENCE: &str = r#"<w:r><w:rPr><w:rStyle w:val="CommentReference"/></w:rPr><w:commentReference w:id="5"/></w:r>"#;

/// "Upper line." / the removed paragraph `removed` / "Lower line." with an
/// optional tail inside the lower paragraph.
fn document(upper_tail: &str, removed: &str, lower: &str, comments: &[&str]) -> Document {
    let body = format!(
        r#"<w:p>{}{upper_tail}</w:p><w:p><w:pPr><w:rPr><w:del w:id="1" w:author="Ann" {DATE}/></w:rPr></w:pPr>{removed}</w:p><w:p>{lower}</w:p>"#,
        run("Upper line.")
    );
    Document::parse(&docx_with_body_and_comments(&body, comments)).expect("parse")
}

/// The saved body, prefix-free, as compact XML: paragraphs and the markers
/// between them, runs reduced to their text.
fn body(doc: &Document) -> String {
    let bytes = doc.serialize(&ExportOptions::default()).expect("save");
    let xml = read_zip_entry(&bytes, "word/document.xml").expect("document.xml");
    let body = xml
        .split("<w:body>")
        .nth(1)
        .unwrap()
        .split("<w:sectPr")
        .next()
        .unwrap();
    let mut out = String::new();
    let mut rest = body;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let end = rest[start..].find('>').unwrap() + start;
        let tag = &rest[start + 1..end];
        let name = tag
            .trim_start_matches('/')
            .split([' ', '/'])
            .next()
            .unwrap();
        let id = tag
            .split("w:id=\"")
            .nth(1)
            .map(|v| v.split('"').next().unwrap())
            .unwrap_or("");
        match name {
            "w:p" if tag.starts_with('/') => out.push_str("</p>"),
            "w:p" if !tag.ends_with('/') => out.push_str("<p>"),
            "w:bookmarkStart" => out.push_str(&format!("[bs{id}]")),
            "w:bookmarkEnd" => out.push_str(&format!("[be{id}]")),
            "w:commentRangeStart" => out.push_str(&format!("[cs{id}]")),
            "w:commentRangeEnd" => out.push_str(&format!("[ce{id}]")),
            "w:commentReference" => out.push_str(&format!("[cr{id}]")),
            _ => {}
        }
        rest = &rest[end + 1..];
    }
    out
}

fn accept_both_ways(doc: &Document) -> Vec<Document> {
    let ids: HashSet<u32> = doc.revisions().iter().map(|r| r.revision_id).collect();
    vec![
        doc.project(Resolution::AcceptAll).expect("accept all"),
        doc.project(Resolution::Selective {
            ids,
            action: ResolveSelectionAction::Accept,
        })
        .expect("accept by id"),
    ]
}

fn assert_accepts_to(doc: &Document, expected: &str) {
    for accepted in accept_both_ways(doc) {
        assert_eq!(body(&accepted), expected);
    }
}

#[test]
fn a_bookmark_wholly_inside_the_removed_paragraph_is_dropped() {
    let doc = document(
        "",
        &format!(
            r#"<w:bookmarkStart w:id="31" w:name="Inside"/>{}<w:bookmarkEnd w:id="31"/>"#,
            deleted_text(2, "Removed words.")
        ),
        &run("Lower line."),
        &[],
    );
    assert_accepts_to(&doc, "<p>Upper line.</p><p>Lower line.</p>");
}

#[test]
fn a_point_bookmark_inside_the_removed_paragraph_is_dropped() {
    let doc = document(
        "",
        &format!(
            r#"{}<w:bookmarkStart w:id="32" w:name="Point"/><w:bookmarkEnd w:id="32"/>{}"#,
            deleted_text(2, "Removed "),
            deleted_text(3, "words.")
        ),
        &run("Lower line."),
        &[],
    );
    assert_accepts_to(&doc, "<p>Upper line.</p><p>Lower line.</p>");
}

#[test]
fn a_comment_and_bookmark_wholly_inside_go_with_the_paragraph() {
    let doc = document(
        "",
        &format!(
            r#"<w:bookmarkStart w:id="30" w:name="Inside"/><w:commentRangeStart w:id="5"/>{}<w:commentRangeEnd w:id="5"/><w:bookmarkEnd w:id="30"/>{}"#,
            deleted_text(2, "Removed words."),
            deleted(3, REFERENCE)
        ),
        &run("Lower line."),
        &["5"],
    );
    assert_accepts_to(&doc, "<p>Upper line.</p><p>Lower line.</p>");
    for accepted in accept_both_ways(&doc) {
        assert!(accepted.snapshot().canonical.comments.is_empty());
    }
}

/// The range starts in the paragraph above: its end goes right after that
/// paragraph, as a body-level marker, still covering its mark.
#[test]
fn a_range_end_goes_after_the_previous_paragraph() {
    let doc = document(
        r#"<w:bookmarkStart w:id="34" w:name="FromAbove"/>"#,
        &format!(
            r#"{}<w:bookmarkEnd w:id="34"/>"#,
            deleted_text(2, "Removed words.")
        ),
        &run("Lower line."),
        &[],
    );
    assert_accepts_to(&doc, "<p>Upper line.[bs34]</p>[be34]<p>Lower line.</p>");
}

/// The range ends in the paragraph below: its start goes to the start of
/// that paragraph.
#[test]
fn a_range_start_goes_to_the_start_of_the_next_paragraph() {
    let doc = document(
        "",
        &format!(
            r#"<w:bookmarkStart w:id="33" w:name="IntoBelow"/><w:commentRangeStart w:id="5"/>{}"#,
            deleted_text(2, "Removed words.")
        ),
        &format!(
            r#"{}<w:bookmarkEnd w:id="33"/><w:commentRangeEnd w:id="5"/>{REFERENCE}{}"#,
            run("Lower "),
            run("line.")
        ),
        &["5"],
    );
    assert_accepts_to(
        &doc,
        "<p>Upper line.</p><p>[bs33][cs5]Lower [be33][ce5][cr5]line.</p>",
    );
}

#[test]
fn reject_puts_the_markers_back() {
    let removed = format!(
        r#"{}<w:bookmarkEnd w:id="34"/>"#,
        deleted_text(2, "Removed words.")
    );
    let doc = document(
        r#"<w:bookmarkStart w:id="34" w:name="FromAbove"/>"#,
        &removed,
        &run("Lower line."),
        &[],
    );
    assert_eq!(
        body(&doc.project(Resolution::RejectAll).unwrap()),
        "<p>Upper line.[bs34]</p><p>Removed words.[be34]</p><p>Lower line.</p>"
    );
}
