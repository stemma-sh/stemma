//! A resolution that takes a comment's anchor removes the comment whole, as
//! Word does (Word 16, verified on the oracle):
//! - rejecting the insertion that holds the comment's reference, or the one
//!   that holds all of its commented text, deletes the comment: its
//!   definition, its range start and end, and its reference, even when the
//!   reference is another author's insertion that is not rejected;
//! - text the range keeps stays as plain text;
//! - a comment stays while anything remains in its range: text inside a
//!   hyperlink or an inline content control, or an object such as a tab or a
//!   picture (Word 16, observed); only a range left with nothing loses it;
//! - a `w:ins` holding only the reference run is not a revision: none is
//!   listed, and Accept and Reject both keep the comment.
//!
//! Each case runs through Reject All (the model's and the byte path's) and
//! through rejecting by id.

use std::collections::HashSet;

use stemma::api::Document;
use stemma::docx::DocxArchive;
use stemma::normalize::reject_all_docx;
use stemma::{ExportOptions, Resolution, ResolveSelectionAction};

use crate::common::{docx_with_body_and_comments, marker_stream, read_zip_entry};

const DATE: &str = r#"w:date="2026-03-01T08:00:00Z""#;
const REFERENCE: &str = r#"<w:r><w:rPr><w:rStyle w:val="CommentReference"/></w:rPr><w:commentReference w:id="0"/></w:r>"#;

fn ins(id: u32, author: &str, content: &str) -> String {
    format!(r#"<w:ins w:id="{id}" w:author="{author}" {DATE}>{content}</w:ins>"#)
}

fn run(text: &str) -> String {
    format!(r#"<w:r><w:t xml:space="preserve">{text}</w:t></w:r>"#)
}

fn package(paragraph: &str) -> Vec<u8> {
    let body = format!("<w:p>{}</w:p><w:p>{paragraph}</w:p>", run("A first line."));
    docx_with_body_and_comments(&body, &["0"])
}

fn document(paragraph: &str) -> Document {
    Document::parse(&package(paragraph)).expect("parse")
}

/// The byte path's Reject All: the document stream and the comment count.
fn byte_reject(paragraph: &str) -> (String, usize) {
    let (archive, _) =
        reject_all_docx(&DocxArchive::read(&package(paragraph)).unwrap()).expect("byte reject");
    let bytes = archive.write().unwrap();
    let comments = read_zip_entry(&bytes, "word/comments.xml")
        .map_or(0, |xml| xml.matches("<w:comment ").count());
    (
        marker_stream(&read_zip_entry(&bytes, "word/document.xml").unwrap()),
        comments,
    )
}

fn stream(doc: &Document) -> String {
    let bytes = doc.serialize(&ExportOptions::default()).expect("save");
    marker_stream(&read_zip_entry(&bytes, "word/document.xml").expect("document.xml"))
}

fn comment_count(doc: &Document) -> usize {
    doc.snapshot().canonical.comments.len()
}

fn reject_by(doc: &Document, author: &str) -> Document {
    let ids: HashSet<u32> = doc
        .revisions()
        .into_iter()
        .filter(|r| r.author.as_deref() == Some(author))
        .map(|r| r.revision_id)
        .collect();
    assert!(!ids.is_empty(), "{author} has revisions");
    doc.project(Resolution::Selective {
        ids,
        action: ResolveSelectionAction::Reject,
    })
    .expect("reject by id")
}

/// After rejecting, the paragraph reads `expected` and the comment is gone.
fn assert_comment_removed(rejected: &Document, expected: &str) {
    assert_eq!(stream(rejected), format!("A first line.|{expected}|"));
    assert_eq!(comment_count(rejected), 0, "the comment goes whole");
}

#[test]
fn rejecting_the_insertion_holding_the_commented_text_removes_the_comment() {
    let paragraph = format!(
        r#"{}<w:commentRangeStart w:id="0"/>{}<w:commentRangeEnd w:id="0"/>{}{}"#,
        run("The gate is "),
        ins(1, "Ana", &run("painted green")),
        ins(2, "Ana", REFERENCE),
        run(" each spring."),
    );
    let doc = document(&paragraph);
    assert_eq!(
        byte_reject(&paragraph),
        (format!("A first line.|{}|", "The gate is  each spring."), 0),
        "byte Reject All"
    );
    assert_comment_removed(
        &doc.project(Resolution::RejectAll).unwrap(),
        "The gate is  each spring.",
    );
    assert_comment_removed(&reject_by(&doc, "Ana"), "The gate is  each spring.");
}

#[test]
fn kept_text_inside_the_range_stays_plain_when_the_reference_goes() {
    let paragraph = format!(
        r#"{}<w:commentRangeStart w:id="0"/>{}{}<w:commentRangeEnd w:id="0"/>{}{}"#,
        run("The gate is "),
        run("painted "),
        ins(1, "Ana", &run("green")),
        ins(2, "Ana", REFERENCE),
        run(" each spring."),
    );
    let doc = document(&paragraph);
    assert_eq!(
        byte_reject(&paragraph),
        (
            format!("A first line.|{}|", "The gate is painted  each spring."),
            0
        ),
        "byte Reject All"
    );
    assert_comment_removed(
        &doc.project(Resolution::RejectAll).unwrap(),
        "The gate is painted  each spring.",
    );
    assert_comment_removed(&reject_by(&doc, "Ana"), "The gate is painted  each spring.");
}

#[test]
fn rejecting_the_reference_with_inserted_text_keeps_the_commented_text_plain() {
    let paragraph = format!(
        r#"{}<w:commentRangeStart w:id="0"/>{}<w:commentRangeEnd w:id="0"/>{}{}"#,
        run("The gate is "),
        run("painted green"),
        ins(1, "Ana", &format!("{REFERENCE}{}", run(" and oiled"))),
        run(" each spring."),
    );
    let doc = document(&paragraph);
    assert_eq!(
        byte_reject(&paragraph),
        (
            format!(
                "A first line.|{}|",
                "The gate is painted green each spring."
            ),
            0
        ),
        "byte Reject All"
    );
    assert_comment_removed(
        &doc.project(Resolution::RejectAll).unwrap(),
        "The gate is painted green each spring.",
    );
    assert_comment_removed(
        &reject_by(&doc, "Ana"),
        "The gate is painted green each spring.",
    );
}

/// The commented text is Ana's insertion, the reference Ben's: rejecting
/// either removes the comment, and rejecting Ana's drops Ben's pending
/// reference insertion with it.
#[test]
fn either_of_two_authors_insertions_takes_the_comment() {
    let paragraph = format!(
        r#"{}<w:commentRangeStart w:id="0"/>{}<w:commentRangeEnd w:id="0"/>{}{}"#,
        run("The gate is "),
        ins(1, "Ana", &run("painted green")),
        ins(2, "Ben", REFERENCE),
        run(" each spring."),
    );
    let doc = document(&paragraph);
    assert_eq!(
        byte_reject(&paragraph),
        (format!("A first line.|{}|", "The gate is  each spring."), 0),
        "byte Reject All"
    );
    assert_comment_removed(&reject_by(&doc, "Ana"), "The gate is  each spring.");
    assert_comment_removed(
        &reject_by(&doc, "Ben"),
        "The gate is <+painted green+> each spring.",
    );
    assert_comment_removed(
        &doc.project(Resolution::RejectAll).unwrap(),
        "The gate is  each spring.",
    );
}

#[test]
fn an_insertion_of_only_the_reference_is_not_a_revision() {
    let paragraph = format!(
        r#"{}<w:commentRangeStart w:id="0"/>{}<w:commentRangeEnd w:id="0"/>{}{}"#,
        run("The gate is "),
        run("painted green"),
        ins(1, "Ana", REFERENCE),
        run(" each spring."),
    );
    let doc = document(&paragraph);
    assert_eq!(
        byte_reject(&paragraph),
        (
            "A first line.|The gate is {cs0}painted green{ce0}{cr0} each spring.|".to_string(),
            1
        ),
        "byte Reject All"
    );
    assert!(doc.revisions().is_empty(), "{:?}", doc.revisions());
    for resolution in [Resolution::AcceptAll, Resolution::RejectAll] {
        let resolved = doc.project(resolution).unwrap();
        assert_eq!(
            stream(&resolved),
            "A first line.|The gate is {cs0}painted green{ce0}{cr0} each spring.|"
        );
        assert_eq!(comment_count(&resolved), 1, "the comment stays");
    }
}

/// Rejecting the insertion that held the start of the commented text, when
/// `remainder` is what the range keeps: Word 16 (observed) keeps the comment
/// and its markers around the remainder, on every path.
fn assert_comment_kept_around(remainder: &str, expected: &str) {
    let paragraph = format!(
        r#"{}<w:commentRangeStart w:id="0"/>{}{remainder}<w:commentRangeEnd w:id="0"/>{REFERENCE}{}"#,
        run("Read "),
        ins(1, "Ana", &run("see ")),
        run(" for details."),
    );
    let doc = document(&paragraph);
    let expected = format!("A first line.|Read {{cs0}}{expected}{{ce0}}{{cr0}} for details.|");
    assert_eq!(
        byte_reject(&paragraph),
        (expected.clone(), 1),
        "byte Reject All"
    );
    for (label, rejected) in [
        ("Reject All", doc.project(Resolution::RejectAll).unwrap()),
        ("reject by id", reject_by(&doc, "Ana")),
    ] {
        assert_eq!(stream(&rejected), expected, "{label}");
        assert_eq!(comment_count(&rejected), 1, "{label}: the comment stays");
    }
}

#[test]
fn text_left_inside_a_hyperlink_keeps_the_comment() {
    assert_comment_kept_around(
        r#"<w:hyperlink w:anchor="policy"><w:r><w:t>the policy</w:t></w:r></w:hyperlink>"#,
        "the policy",
    );
}

#[test]
fn text_left_inside_an_inline_content_control_keeps_the_comment() {
    assert_comment_kept_around(
        r#"<w:sdt><w:sdtPr><w:id w:val="4711"/></w:sdtPr><w:sdtContent><w:r><w:t>the policy</w:t></w:r></w:sdtContent></w:sdt>"#,
        "the policy",
    );
}

#[test]
fn a_tab_left_in_the_range_keeps_the_comment() {
    assert_comment_kept_around("<w:r><w:tab/></w:r>", "");
}
