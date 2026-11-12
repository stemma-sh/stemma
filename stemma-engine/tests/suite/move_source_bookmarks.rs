//! Accepting a move and the bookmarks of its source, on every resolution path.
//!
//! Word 16 (observed), accepting a tracked move (Accept All, or the move
//! alone):
//! - a bookmark whose whole span is moved-from text goes, both halves; it is
//!   neither collapsed to a point at the source nor carried to the
//!   destination;
//! - a bookmark that also spans text that stays — it starts in the moved
//!   text and ends after it, or starts before it and ends inside — stays,
//!   and its half that was in the moved text sits where that text was
//!   removed.
//!
//! Rejecting the move keeps every bookmark where it was. The model's
//! Accept/Reject All, the byte path's, and accepting or rejecting the move by
//! id all reach Word's document.

use std::collections::HashSet;

use stemma::api::Document;
use stemma::docx::DocxArchive;
use stemma::normalize::{normalize_docx, reject_all_docx};
use stemma::{ExportOptions, Resolution, ResolveSelectionAction};

use crate::common::{docx_with_body_and_comments, marker_stream, read_zip_entry};

const MOVE: &str = r#"w:author="Mover" w:date="2026-01-05T10:00:00Z""#;

fn run(text: &str) -> String {
    format!(r#"<w:r><w:t xml:space="preserve">{text}</w:t></w:r>"#)
}

fn para(inner: &str) -> String {
    format!("<w:p>{inner}</w:p>")
}

fn moved_from(id: u32, text: &str) -> String {
    format!(
        r#"<w:moveFrom w:id="{id}" {MOVE}>{}</w:moveFrom>"#,
        run(text)
    )
}

/// The whole moved-from paragraph; its range end sits at body level after
/// it, where Word writes it.
fn source_paragraph(inner: &str) -> String {
    format!(
        r#"<w:p><w:pPr><w:rPr><w:moveFrom w:id="20" {MOVE}/></w:rPr></w:pPr><w:moveFromRangeStart w:id="21" {MOVE} w:name="move1"/>{inner}</w:p><w:moveFromRangeEnd w:id="21"/>"#
    )
}

fn destination_paragraph() -> String {
    format!(
        r#"<w:p><w:pPr><w:rPr><w:moveTo w:id="30" {MOVE}/></w:rPr></w:pPr><w:moveToRangeStart w:id="31" {MOVE} w:name="move1"/><w:moveTo w:id="32" {MOVE}>{}</w:moveTo></w:p><w:moveToRangeEnd w:id="31"/>"#,
        run("Moving text.")
    )
}

fn bookmark_start(id: u32, name: &str) -> String {
    format!(r#"<w:bookmarkStart w:id="{id}" w:name="{name}"/>"#)
}

fn bookmark_end(id: u32) -> String {
    format!(r#"<w:bookmarkEnd w:id="{id}"/>"#)
}

fn stream_of(bytes: &[u8]) -> String {
    marker_stream(&read_zip_entry(bytes, "word/document.xml").expect("document.xml"))
}

fn saved(document: &Document) -> Vec<u8> {
    document
        .serialize(&ExportOptions::default())
        .expect("the resolved document saves")
}

fn by_id(document: &Document, action: ResolveSelectionAction) -> Document {
    let ids: HashSet<u32> = document
        .revisions()
        .into_iter()
        .filter(|r| r.author.as_deref() == Some("Mover"))
        .map(|r| r.revision_id)
        .collect();
    assert!(!ids.is_empty(), "the move is listed");
    document
        .project(Resolution::Selective { ids, action })
        .expect("resolve the move by id")
}

/// Every path, accepting and rejecting, reads `accepted` / `rejected`.
fn assert_every_path(body: &str, accepted: &str, rejected: &str) {
    let bytes = docx_with_body_and_comments(body, &[]);
    let document = Document::parse(&bytes).expect("parse");
    let archive = DocxArchive::read(&bytes).expect("read package");
    for (expected, byte_path, model_all, action) in [
        (
            accepted,
            normalize_docx(&archive).expect("byte Accept All").0,
            Resolution::AcceptAll,
            ResolveSelectionAction::Accept,
        ),
        (
            rejected,
            reject_all_docx(&archive).expect("byte Reject All").0,
            Resolution::RejectAll,
            ResolveSelectionAction::Reject,
        ),
    ] {
        let all = document.project(model_all).expect("resolve all");
        let readings = [
            (
                "byte path",
                stream_of(&byte_path.write().expect("write package")),
            ),
            ("model, all", stream_of(&saved(&all))),
            (
                "model, the move by id",
                stream_of(&saved(&by_id(&document, action))),
            ),
        ];
        let wrong: Vec<String> = readings
            .iter()
            .filter(|(_, reading)| reading != expected)
            .map(|(path, reading)| format!("{path}: {reading}"))
            .collect();
        assert!(
            wrong.is_empty(),
            "{action:?} should read {expected}\n{}",
            wrong.join("\n")
        );
    }
}

#[test]
fn a_bookmark_wholly_inside_the_moved_paragraph_goes_on_accept() {
    let body = [
        para(&run("Opening text.")),
        source_paragraph(&format!(
            "{}{}{}",
            bookmark_start(1, "Inside"),
            moved_from(22, "Moving text."),
            bookmark_end(1)
        )),
        para(&run("Staying text.")),
        destination_paragraph(),
        para(&run("Closing text.")),
    ]
    .concat();
    assert_every_path(
        &body,
        "Opening text.|Staying text.|Moving text.|Closing text.|",
        "Opening text.|{bs1}Moving text.{be1}|Staying text.|Closing text.|",
    );
}

#[test]
fn a_bookmark_starting_in_the_moved_paragraph_and_ending_later_stays() {
    let body = [
        para(&run("Opening text.")),
        source_paragraph(&format!(
            "{}{}",
            bookmark_start(2, "Leaving"),
            moved_from(22, "Moving text.")
        )),
        para(&format!(
            "{}{}{}",
            run("Staying "),
            bookmark_end(2),
            run("text.")
        )),
        destination_paragraph(),
        para(&run("Closing text.")),
    ]
    .concat();
    assert_every_path(
        &body,
        "Opening text.|{bs2}Staying {be2}text.|Moving text.|Closing text.|",
        "Opening text.|{bs2}Moving text.|Staying {be2}text.|Closing text.|",
    );
}

#[test]
fn a_bookmark_starting_before_and_ending_in_the_moved_paragraph_stays() {
    let body = [
        para(&format!(
            "{}{}{}",
            run("Opening "),
            bookmark_start(3, "Arriving"),
            run("text.")
        )),
        source_paragraph(&format!(
            "{}{}{}",
            moved_from(22, "Moving "),
            bookmark_end(3),
            moved_from(23, "text.")
        )),
        para(&run("Staying text.")),
        destination_paragraph(),
        para(&run("Closing text.")),
    ]
    .concat();
    assert_every_path(
        &body,
        "Opening {bs3}text.|{be3}Staying text.|Moving text.|Closing text.|",
        "Opening {bs3}text.|Moving {be3}text.|Staying text.|Closing text.|",
    );
}

#[test]
fn a_bookmark_starting_in_moved_text_and_ending_after_it_in_the_paragraph_stays() {
    let body = [
        para(&run("Opening text.")),
        para(&format!(
            r#"{}<w:moveFromRangeStart w:id="21" {MOVE} w:name="move1"/>{}{}<w:moveFromRangeEnd w:id="21"/>{}{}{}"#,
            run("Before. "),
            bookmark_start(4, "Tail"),
            moved_from(22, "Moving text. "),
            run("After "),
            bookmark_end(4),
            run("the move."),
        )),
        para(&run("Staying text.")),
        para(&format!(
            r#"{}<w:moveToRangeStart w:id="31" {MOVE} w:name="move1"/><w:moveTo w:id="32" {MOVE}>{}</w:moveTo><w:moveToRangeEnd w:id="31"/>{}"#,
            run("Here: "),
            run("Moving text. "),
            run("end."),
        )),
    ]
    .concat();
    assert_every_path(
        &body,
        "Opening text.|Before. {bs4}After {be4}the move.|Staying text.|Here: Moving text. end.|",
        "Opening text.|Before. {bs4}Moving text. After {be4}the move.|Staying text.|Here: end.|",
    );
}

/// Word 16 (observed): a bookmark that starts just before the moved text,
/// with nothing between, and ends inside it spans only moved text and goes.
#[test]
fn a_bookmark_spanning_only_moved_text_from_just_before_it_goes() {
    let body = [
        para(&format!(
            r#"{}{}<w:moveFromRangeStart w:id="21" {MOVE} w:name="move1"/>{}{}{}<w:moveFromRangeEnd w:id="21"/>{}"#,
            run("Before. "),
            bookmark_start(5, "Edge"),
            moved_from(22, "Moving "),
            bookmark_end(5),
            moved_from(23, "text. "),
            run("After."),
        )),
        para(&format!(
            r#"{}<w:moveToRangeStart w:id="31" {MOVE} w:name="move1"/><w:moveTo w:id="32" {MOVE}>{}</w:moveTo><w:moveToRangeEnd w:id="31"/>"#,
            run("Here: "),
            run("Moving text. "),
        )),
    ]
    .concat();
    assert_every_path(
        &body,
        "Before. After.|Here: Moving text. |",
        "Before. {bs5}Moving {be5}text. After.|Here: |",
    );
}

/// A half held at body level, between the moved paragraph and its range
/// end, is in the moved source like one inside the paragraph: the bookmark
/// spans only moved text and goes.
#[test]
fn a_bookmark_half_at_body_level_in_the_source_is_read() {
    let body = [
        para(&run("Opening text.")),
        format!(
            r#"<w:p><w:pPr><w:rPr><w:moveFrom w:id="20" {MOVE}/></w:rPr></w:pPr><w:moveFromRangeStart w:id="21" {MOVE} w:name="move1"/>{}{}</w:p>{}<w:moveFromRangeEnd w:id="21"/>"#,
            bookmark_start(6, "Body"),
            moved_from(22, "Moving text."),
            bookmark_end(6),
        ),
        para(&run("Staying text.")),
        destination_paragraph(),
        para(&run("Closing text.")),
    ]
    .concat();
    assert_every_path(
        &body,
        "Opening text.|Staying text.|Moving text.|Closing text.|",
        "Opening text.|{bs6}Moving text.|{be6}Staying text.|Closing text.|",
    );
}
