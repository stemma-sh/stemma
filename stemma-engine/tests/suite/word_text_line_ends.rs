//! White space written inside a text leaf, read as Word reads it.
//!
//! Observed by saving small documents through Word: inside `w:t` and
//! `w:delText`, with or without `xml:space="preserve"`, a line feed is one
//! space (not a line break), and so is a carriage return, written as such or
//! as `&#13;`; a carriage return before a line feed is one space with it; a
//! tab character is a tab. Word saves `one\ntwo` back as `one two`.
//!
//! An untouched run is written back as its source wrote it; a run an edit
//! rewrites is written as it reads.

use stemma::ExportOptions;
use stemma::api::Document;
use stemma::docx::DocxArchive;
use stemma::domain::{BlockNode, InlineNode, NodeId, RevisionInfo, TrackingStatus};
use stemma::edit::{EditStep, EditTransaction, InlineMarkSet, MaterializationMode, RunStyleEdit};

use crate::common::{docx_with_body_and_comments, paragraph_text};

fn parse(body: &str) -> Document {
    Document::parse(&docx_with_body_and_comments(body, &[])).expect("parse")
}

/// The first paragraph's text (a hard break read as a line feed), and
/// whether it holds a hard break.
fn first_paragraph(body: &str) -> (String, bool) {
    let document = parse(body);
    let snapshot = document.snapshot();
    let BlockNode::Paragraph(paragraph) = &snapshot.canonical.blocks[0].block else {
        panic!("a paragraph first");
    };
    let breaks = paragraph
        .all_inlines_owned()
        .iter()
        .any(|inline| matches!(inline, InlineNode::HardBreak(_)));
    (paragraph_text(paragraph), breaks)
}

fn reads_as(body: &str, expected: &str) {
    let (text, breaks) = first_paragraph(body);
    assert_eq!(text, expected, "{body:?}");
    assert!(!breaks, "no line break in {body:?}");
}

#[test]
fn a_line_feed_in_preserved_text_is_one_space() {
    reads_as(
        "<w:p><w:r><w:t xml:space=\"preserve\">one\ntwo</w:t></w:r></w:p>",
        "one two",
    );
}

#[test]
fn a_line_feed_in_plain_text_is_one_space() {
    reads_as("<w:p><w:r><w:t>one\ntwo</w:t></w:r></w:p>", "one two");
}

#[test]
fn a_written_carriage_return_is_one_space() {
    reads_as(
        "<w:p><w:r><w:t xml:space=\"preserve\">one\rtwo</w:t></w:r></w:p>",
        "one two",
    );
}

#[test]
fn a_carriage_return_and_line_feed_together_are_one_space() {
    reads_as(
        "<w:p><w:r><w:t xml:space=\"preserve\">one\r\ntwo</w:t></w:r></w:p>",
        "one two",
    );
}

#[test]
fn a_carriage_return_written_as_a_reference_is_one_space() {
    reads_as(
        r#"<w:p><w:r><w:t xml:space="preserve">one&#13;two</w:t></w:r></w:p>"#,
        "one two",
    );
}

#[test]
fn a_line_feed_written_as_a_reference_is_one_space() {
    reads_as(
        r#"<w:p><w:r><w:t xml:space="preserve">one&#10;two</w:t></w:r></w:p>"#,
        "one two",
    );
}

#[test]
fn two_line_feeds_are_two_spaces() {
    reads_as(
        "<w:p><w:r><w:t xml:space=\"preserve\">one\n\ntwo</w:t></w:r></w:p>",
        "one  two",
    );
}

#[test]
fn spaces_around_a_line_feed_are_kept() {
    reads_as(
        "<w:p><w:r><w:t xml:space=\"preserve\">one \n  two</w:t></w:r></w:p>",
        "one    two",
    );
}

#[test]
fn a_tab_character_is_a_tab() {
    reads_as(
        "<w:p><w:r><w:t xml:space=\"preserve\">one\ttwo</w:t></w:r></w:p>",
        "one\ttwo",
    );
}

#[test]
fn a_tab_written_as_a_reference_is_a_tab() {
    reads_as(
        r#"<w:p><w:r><w:t xml:space="preserve">one&#9;two</w:t></w:r></w:p>"#,
        "one\ttwo",
    );
}

#[test]
fn a_line_feed_in_deleted_text_is_one_space() {
    let document = parse(concat!(
        r#"<w:p><w:r><w:t xml:space="preserve">Kept </w:t></w:r>"#,
        r#"<w:del w:id="3" w:author="Ines" w:date="2024-03-01T09:00:00Z"><w:r><w:delText xml:space="preserve">one"#,
        "\n",
        r#"two</w:delText></w:r></w:del></w:p>"#,
    ));
    let snapshot = document.snapshot();
    let BlockNode::Paragraph(paragraph) = &snapshot.canonical.blocks[0].block else {
        panic!("a paragraph first");
    };
    let deleted: String = paragraph
        .segments
        .iter()
        .filter(|segment| matches!(segment.status, TrackingStatus::Deleted(_)))
        .flat_map(|segment| &segment.inlines)
        .map(|inline| match inline {
            InlineNode::Text(text) => text.text.clone(),
            InlineNode::HardBreak(_) => "<br>".to_string(),
            _ => String::new(),
        })
        .collect();
    assert_eq!(deleted, "one two");
}

/// `document.xml` after making the word "two" of the first paragraph bold,
/// which rewrites that paragraph's run; the second paragraph is untouched.
fn saved_after_rewriting_the_first_run() -> String {
    let document = parse(concat!(
        "<w:p><w:r><w:t xml:space=\"preserve\">one\ntwo</w:t></w:r></w:p>",
        "<w:p><w:r><w:t xml:space=\"preserve\">three\nfour</w:t></w:r></w:p>",
        "<w:p><w:r><w:t xml:space=\"preserve\">five&#13;six</w:t></w:r></w:p>",
    ));
    let snapshot = document.snapshot();
    let BlockNode::Paragraph(paragraph) = &snapshot.canonical.blocks[0].block else {
        panic!("a paragraph first");
    };
    let block_id: NodeId = paragraph.id.clone();
    let transaction = EditTransaction {
        steps: vec![EditStep::SetRunFormatting {
            block_id,
            expect: "two".to_string(),
            semantic_hash: None,
            marks: InlineMarkSet {
                bold: true,
                ..Default::default()
            },
            style: RunStyleEdit::default(),
            rationale: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::Direct,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("Editor".to_string()),
            date: Some("2024-03-01T09:00:00Z".to_string()),
            apply_op_id: None,
            date_utc: None,
        },
    };
    let bytes = document
        .apply(&transaction)
        .expect("bold one word")
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let archive = DocxArchive::read(&bytes).expect("read saved document");
    String::from_utf8(
        archive
            .get("word/document.xml")
            .expect("document.xml")
            .to_vec(),
    )
    .expect("utf-8")
}

#[test]
fn a_rewritten_run_writes_the_space_it_reads_as() {
    let saved = saved_after_rewriting_the_first_run();
    assert!(
        saved.contains(">one </w:t>"),
        "the rewritten run holds the space: {saved}"
    );
    assert!(!saved.contains("one\ntwo"), "{saved}");
    assert!(
        !saved.contains("<w:br"),
        "no line break is written: {saved}"
    );
}

#[test]
fn an_untouched_run_keeps_its_written_line_feed() {
    let saved = saved_after_rewriting_the_first_run();
    assert!(
        saved.contains(">three\nfour</w:t>"),
        "the untouched run is written as it was: {saved}"
    );
    // A carriage return the text means is written as a reference again: a
    // literal one would be read back as a line feed (XML 1.0 §2.11).
    assert!(
        saved.contains(">five&#13;six</w:t>") || saved.contains(">five&#xD;six</w:t>"),
        "the written carriage return survives: {saved:?}"
    );
}
