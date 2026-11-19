//! Accept All and Reject All where Word's result is not the sum of the
//! single changes: a move whose two copies differ, and a field whose
//! markers are tracked.
//!
//! Each rule was observed by having Word accept all and reject all in small
//! documents that differ in one piece:
//!
//! - a move paired by range markers, source "Quay seven. ", destination
//!   "Pier nine now.": Reject All leaves "Pier nine now." at the source. The
//!   destination's content returns; the source's own text does not. The same
//!   holds when the two copies differ by one space. Without range markers the
//!   two halves are a plain deletion and insertion, and the source's own text
//!   is restored;
//! - a field (a HYPERLINK and an AUTHOR field alike) whose begin marker, end
//!   marker or both are a tracked insertion, with a result that carries no
//!   change: Reject All removes the field and its result. With a marker a
//!   tracked deletion: Accept All removes the field and its result.

use std::io::{Cursor, Write};

use stemma::Resolution;
use stemma::api::Document;
use zip::ZipWriter;
use zip::write::FileOptions;

const WHO: &str = r#"w:author="Ingrid" w:date="2024-03-02T09:10:00Z""#;

fn docx(body: &str) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let options = FileOptions::default();
    zip.start_file("[Content_Types].xml", options).unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#).unwrap();
    zip.start_file("_rels/.rels", options).unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#).unwrap();
    zip.start_file("word/_rels/document.xml.rels", options)
        .unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#).unwrap();
    zip.start_file("word/document.xml", options).unwrap();
    zip.write_all(
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
        )
        .as_bytes(),
    )
    .unwrap();
    zip.finish().unwrap().into_inner()
}

fn resolved(body: &str, resolution: Resolution) -> String {
    Document::parse(&docx(body))
        .expect("the document loads")
        .project(resolution)
        .expect("the document resolves")
        .to_text()
}

fn text_run(text: &str) -> String {
    format!(r#"<w:r><w:t xml:space="preserve">{text}</w:t></w:r>"#)
}

/// Two paragraphs: the first receives the moved text between "First berth. "
/// and " After it.", the second gives it up in front of "Second berth.".
/// `destination` is the content of the paragraph's `w:moveTo` region.
fn moved(source: &str, destination: &str, paired: bool) -> String {
    let mut to = destination.to_string();
    let mut from = format!(
        r#"<w:moveFrom w:id="40" {WHO}>{}</w:moveFrom>"#,
        text_run(source)
    );
    if paired {
        to = format!(
            r#"<w:moveToRangeStart w:id="1" {WHO} w:name="move1"/>{to}<w:moveToRangeEnd w:id="1"/>"#
        );
        from = format!(
            r#"<w:moveFromRangeStart w:id="3" {WHO} w:name="move1"/>{from}<w:moveFromRangeEnd w:id="3"/>"#
        );
    }
    format!(
        "<w:p>{}{to}{}</w:p><w:p>{from}{}</w:p>",
        text_run("First berth. "),
        text_run(" After it."),
        text_run("Second berth.")
    )
}

fn move_to(id: u32, content: &str) -> String {
    format!(r#"<w:moveTo w:id="{id}" {WHO}>{content}</w:moveTo>"#)
}

#[test]
fn rejecting_a_paired_move_returns_the_destination_text_to_the_source() {
    for (source, destination, rejected_source) in [
        ("Quay seven. ", "Quay seven. ", "Quay seven. Second berth."),
        ("Quay seven. ", "Quay seven.", "Quay seven.Second berth."),
        ("Quay seven.", "Quay seven. ", "Quay seven. Second berth."),
        (
            "Quay seven. ",
            "Pier nine now.",
            "Pier nine now.Second berth.",
        ),
    ] {
        let body = moved(source, &move_to(2, &text_run(destination)), true);
        assert_eq!(
            resolved(&body, Resolution::RejectAll),
            format!("First berth.  After it.\n\n{rejected_source}"),
            "reject, source {source:?} destination {destination:?}"
        );
        assert_eq!(
            resolved(&body, Resolution::AcceptAll),
            format!("First berth. {destination} After it.\n\nSecond berth."),
            "accept, source {source:?} destination {destination:?}"
        );
    }
}

#[test]
fn rejecting_move_halves_with_no_range_markers_restores_the_source_text() {
    let body = moved(
        "Quay seven. ",
        &move_to(2, &text_run("Pier nine now.")),
        false,
    );
    assert_eq!(
        resolved(&body, Resolution::RejectAll),
        "First berth.  After it.\n\nQuay seven. Second berth."
    );
    assert_eq!(
        resolved(&body, Resolution::AcceptAll),
        "First berth. Pier nine now. After it.\n\nSecond berth."
    );
}

/// Two deletions made at the destination after the move. Rejecting
/// everything returns the destination's content, deletions undone, to the
/// source; accepting everything keeps the destination without them.
#[test]
fn several_deletions_inside_a_move_destination_resolve_with_the_move() {
    let deleted = |id: u32, text: &str| {
        format!(
            r#"<w:del w:id="{id}" w:author="Oskar" w:date="2024-03-03T08:00:00Z"><w:r><w:delText xml:space="preserve">{text}</w:delText></w:r></w:del>"#
        )
    };
    let destination = move_to(
        2,
        &format!(
            "{}{}{}{}{}",
            text_run("Quay "),
            deleted(21, "number "),
            text_run("seven "),
            deleted(22, "is "),
            text_run("open. ")
        ),
    );
    let body = moved("Quay number seven is open. ", &destination, true);
    assert_eq!(
        resolved(&body, Resolution::RejectAll),
        "First berth.  After it.\n\nQuay number seven is open. Second berth."
    );
    assert_eq!(
        resolved(&body, Resolution::AcceptAll),
        "First berth. Quay seven open.  After it.\n\nSecond berth."
    );

    // The same with a destination that also differs from the source outside
    // any tracked change ("berth" for "quay"): the destination's content
    // returns.
    let destination = move_to(
        2,
        &format!(
            "{}{}{}{}{}",
            text_run("Berth "),
            deleted(21, "number "),
            text_run("seven "),
            deleted(22, "is "),
            text_run("open. ")
        ),
    );
    let body = moved("Quay number seven is open. ", &destination, true);
    assert_eq!(
        resolved(&body, Resolution::RejectAll),
        "First berth.  After it.\n\nBerth number seven is open. Second berth."
    );
    assert_eq!(
        resolved(&body, Resolution::AcceptAll),
        "First berth. Berth seven open.  After it.\n\nSecond berth."
    );
}

/// "Quay [field showing seven] is open." with the field's begin part (begin
/// marker, instruction, separator) and its end marker wrapped as given.
fn field(
    instruction: &str,
    begin_wrap: (&str, &str),
    end_wrap: (&str, &str),
    deleted: bool,
) -> String {
    let instruction_tag = if deleted { "delInstrText" } else { "instrText" };
    format!(
        concat!(
            "<w:p>{quay}{begin_open}",
            r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r>"#,
            r#"<w:r><w:{tag} xml:space="preserve">{instruction}</w:{tag}></w:r>"#,
            r#"<w:r><w:fldChar w:fldCharType="separate"/></w:r>"#,
            "{begin_close}{result}{end_open}",
            r#"<w:r><w:fldChar w:fldCharType="end"/></w:r>"#,
            "{end_close}{rest}</w:p>"
        ),
        quay = text_run("Quay "),
        begin_open = begin_wrap.0,
        begin_close = begin_wrap.1,
        tag = instruction_tag,
        instruction = instruction,
        result = text_run("seven"),
        end_open = end_wrap.0,
        end_close = end_wrap.1,
        rest = text_run(" is open."),
    )
}

#[test]
fn a_field_with_a_tracked_marker_is_resolved_whole() {
    let inserted_a = format!(r#"<w:ins w:id="5" {WHO}>"#);
    let inserted_b = format!(r#"<w:ins w:id="6" {WHO}>"#);
    let deleted_a = format!(r#"<w:del w:id="5" {WHO}>"#);
    let deleted_b = format!(r#"<w:del w:id="6" {WHO}>"#);
    let none = ("", "");
    let with = "Quay seven is open.";
    let without = "Quay  is open.";
    for instruction in [r#" HYPERLINK "https://example.org/berths" "#, " AUTHOR "] {
        // A marker inserted: rejecting removes the field with its result.
        for (begin, end) in [
            (
                (inserted_a.as_str(), "</w:ins>"),
                (inserted_b.as_str(), "</w:ins>"),
            ),
            ((inserted_a.as_str(), "</w:ins>"), none),
            (none, (inserted_b.as_str(), "</w:ins>")),
        ] {
            let body = field(instruction, begin, end, false);
            assert_eq!(resolved(&body, Resolution::AcceptAll), with, "{body}");
            assert_eq!(resolved(&body, Resolution::RejectAll), without, "{body}");
        }
        // A marker deleted: accepting removes the field with its result.
        for (begin, end, begin_deleted) in [
            (
                (deleted_a.as_str(), "</w:del>"),
                (deleted_b.as_str(), "</w:del>"),
                true,
            ),
            ((deleted_a.as_str(), "</w:del>"), none, true),
            (none, (deleted_b.as_str(), "</w:del>"), false),
        ] {
            let body = field(instruction, begin, end, begin_deleted);
            assert_eq!(resolved(&body, Resolution::AcceptAll), without, "{body}");
            assert_eq!(resolved(&body, Resolution::RejectAll), with, "{body}");
        }
    }
}

/// Word writes the end marker of a move range at the start of the paragraph
/// after the last moved one. That following paragraph holds none of the
/// moved content: an insertion of its mark is an insertion. (It was read as
/// part of the move, and its inserted mark written back as a move
/// destination, which lost the insertion and added half a move.)
#[test]
fn a_paragraph_after_a_move_range_keeps_its_own_inserted_mark() {
    let who = WHO;
    let body = format!(
        concat!(
            r#"<w:p><w:pPr><w:rPr><w:moveTo w:id="2" {who}/></w:rPr></w:pPr>"#,
            r#"<w:moveToRangeStart w:id="1" {who} w:name="move1"/>"#,
            r#"<w:moveTo w:id="3" {who}><w:r><w:t>The pilot boards at dawn.</w:t></w:r></w:moveTo></w:p>"#,
            r#"<w:p><w:pPr><w:rPr><w:ins w:id="4" {who}/></w:rPr></w:pPr><w:moveToRangeEnd w:id="1"/>"#,
            r#"<w:ins w:id="5" {who}><w:r><w:t>A tug stands by.</w:t></w:r></w:ins></w:p>"#,
            r#"<w:p><w:r><w:t>The berth is clear.</w:t></w:r></w:p>"#,
            r#"<w:p><w:pPr><w:rPr><w:moveFrom w:id="7" {who}/></w:rPr></w:pPr>"#,
            r#"<w:moveFromRangeStart w:id="6" {who} w:name="move1"/>"#,
            r#"<w:moveFrom w:id="8" {who}><w:r><w:t>The pilot boards at dawn.</w:t></w:r></w:moveFrom></w:p>"#,
            r#"<w:p><w:moveFromRangeEnd w:id="6"/><w:r><w:t>The harbour closes at dusk.</w:t></w:r></w:p>"#,
        ),
        who = who
    );
    let document = Document::parse(&docx(&body)).expect("the document loads");
    let saved = document
        .serialize(&stemma::ExportOptions::default())
        .expect("save");
    let mut archive = zip::ZipArchive::new(Cursor::new(saved.clone())).expect("zip");
    let mut xml = String::new();
    std::io::Read::read_to_string(
        &mut archive.by_name("word/document.xml").expect("document"),
        &mut xml,
    )
    .expect("read");
    let marks_of = |tag: &str| {
        xml.split("<w:pPr>")
            .skip(1)
            .filter(|rest| {
                rest.split("</w:pPr>")
                    .next()
                    .is_some_and(|properties| properties.contains(&format!("<w:{tag} ")))
            })
            .count()
    };
    assert_eq!(marks_of("moveTo"), 1, "one moved-to mark: {xml}");
    assert_eq!(
        marks_of("ins"),
        1,
        "the following paragraph's inserted mark: {xml}"
    );
    assert_eq!(marks_of("moveFrom"), 1, "{xml}");

    // The same revisions before and after a save.
    let reopened = Document::parse(&saved).expect("the saved document loads");
    let census = |document: &Document| {
        let mut records: Vec<u32> =
            stemma::tracked_model::enumerate_revisions(&document.snapshot().canonical)
                .iter()
                .map(|record| record.revision_id)
                .collect();
        records.sort_unstable();
        records
    };
    assert_eq!(census(&document), census(&reopened));

    assert_eq!(
        resolved(&body, Resolution::AcceptAll),
        "The pilot boards at dawn.\n\nA tug stands by.\n\nThe berth is clear.\n\nThe harbour closes at dusk."
    );
    assert_eq!(
        resolved(&body, Resolution::RejectAll),
        "The berth is clear.\n\nThe pilot boards at dawn.\n\nThe harbour closes at dusk."
    );
}

/// A bibliography field whose markers are inserted and whose result, of
/// several paragraphs, carries no change (Word 16, checked): Reject All
/// removes the field with every paragraph mark inside it, leaving one
/// paragraph with the properties of the paragraph holding the end marker;
/// Accept All keeps it all.
#[test]
fn a_field_spanning_paragraphs_with_inserted_markers_is_rejected_whole() {
    let body = format!(
        concat!(
            "<w:p>{heading}</w:p>",
            r#"<w:p><w:ins w:id="5" {who}>"#,
            r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r>"#,
            r#"<w:r><w:instrText xml:space="preserve"> BIBLIOGRAPHY </w:instrText></w:r>"#,
            r#"<w:r><w:fldChar w:fldCharType="separate"/></w:r>"#,
            "</w:ins>{first}</w:p>",
            "<w:p>{second}</w:p>",
            "<w:p/>",
            r#"<w:p><w:pPr><w:jc w:val="center"/></w:pPr><w:ins w:id="6" {who}>"#,
            r#"<w:r><w:fldChar w:fldCharType="end"/></w:r></w:ins></w:p>"#,
            "<w:p>{closing}</w:p>",
        ),
        who = WHO,
        heading = text_run("Sources"),
        first = text_run("1. Harbour charts."),
        second = text_run("2. Tide tables."),
        closing = text_run("Closing line."),
    );
    let paragraphs = |resolution| {
        Document::parse(&docx(&body))
            .expect("the document loads")
            .project(resolution)
            .expect("the document resolves")
            .read()
            .blocks
            .iter()
            .map(|block| block.text.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        paragraphs(Resolution::AcceptAll),
        [
            "Sources",
            "1. Harbour charts.",
            "2. Tide tables.",
            "",
            "",
            "Closing line."
        ]
    );
    assert_eq!(
        paragraphs(Resolution::RejectAll),
        ["Sources", "", "Closing line."]
    );
    let rejected = Document::parse(&docx(&body))
        .expect("the document loads")
        .project(Resolution::RejectAll)
        .expect("the document resolves")
        .serialize(&stemma::ExportOptions::default())
        .expect("the resolved document saves");
    let archive = stemma::docx::DocxArchive::read(&rejected).expect("the package reads");
    let xml = String::from_utf8_lossy(archive.get("word/document.xml").expect("document part"));
    assert!(
        xml.contains(r#"<w:jc w:val="center""#),
        "the paragraph left keeps the end marker's paragraph properties: {xml}"
    );
}

/// The paragraphs of a document.xml, as text (`w:t` only).
fn package_paragraphs(bytes: &[u8]) -> Vec<String> {
    let archive = stemma::docx::DocxArchive::read(bytes).expect("the package reads");
    let xml = String::from_utf8_lossy(archive.get("word/document.xml").expect("document part"))
        .to_string();
    let root = xmltree::Element::parse(xml.as_bytes()).expect("document.xml parses");
    fn walk(element: &xmltree::Element, out: &mut Vec<String>) {
        if element.name == "p" {
            fn text(element: &xmltree::Element, into: &mut String) {
                for child in element.children.iter().filter_map(|c| c.as_element()) {
                    if child.name == "t" {
                        into.push_str(&child.get_text().unwrap_or_default());
                    } else {
                        text(child, into);
                    }
                }
            }
            let mut line = String::new();
            text(element, &mut line);
            out.push(line);
            return;
        }
        for child in element.children.iter().filter_map(|c| c.as_element()) {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(&root, &mut out);
    out
}

/// A table of contents whose field markers are inserted, of several result
/// paragraphs with no change, in a body content control that also carries
/// the insertion of its own tags (`w:customXmlInsRangeStart`/`End`, which
/// the engine keeps as a preserved control). Word 16 Reject All removes the
/// field with its whole result; the package resolver reads it the same.
fn table_of_contents_in_inserted_control() -> String {
    format!(
        concat!(
            "<w:p>{intro}</w:p>",
            r#"<w:customXmlInsRangeStart w:id="1" {who}/><w:sdt><w:sdtPr><w:id w:val="7"/>"#,
            r#"<w:docPartObj><w:docPartGallery w:val="Table of Contents"/><w:docPartUnique/></w:docPartObj>"#,
            r#"</w:sdtPr><w:sdtContent><w:customXmlInsRangeEnd w:id="1"/>"#,
            r#"<w:p><w:pPr><w:rPr><w:ins w:id="2" {who}/></w:rPr></w:pPr>"#,
            r#"<w:ins w:id="3" {who}>{contents}</w:ins></w:p>"#,
            r#"<w:p><w:ins w:id="4" {who}><w:r><w:fldChar w:fldCharType="begin"/></w:r>"#,
            r#"<w:r><w:instrText xml:space="preserve"> TOC \o "1-3" </w:instrText></w:r>"#,
            r#"<w:r><w:fldChar w:fldCharType="separate"/></w:r></w:ins>{entry_one}</w:p>"#,
            "<w:p>{entry_two}</w:p>",
            r#"<w:p><w:pPr><w:rPr><w:ins w:id="5" {who}/></w:rPr></w:pPr>"#,
            r#"<w:ins w:id="6" {who}><w:r><w:fldChar w:fldCharType="end"/></w:r></w:ins></w:p>"#,
            r#"<w:customXmlInsRangeStart w:id="8" {who}/></w:sdtContent></w:sdt>"#,
            r#"<w:customXmlInsRangeEnd w:id="8"/>"#,
            "<w:p>{heading}</w:p>",
        ),
        who = WHO,
        intro = text_run("Harbour rules."),
        contents = text_run("Contents"),
        entry_one = text_run("1. Berths 3"),
        entry_two = text_run("2. Tides 5"),
        heading = text_run("1. Berths"),
    )
}

#[test]
fn a_table_of_contents_with_inserted_markers_in_a_preserved_control_is_rejected_whole() {
    let body = table_of_contents_in_inserted_control();
    let rejected = Document::parse(&docx(&body))
        .expect("the document loads")
        .project(Resolution::RejectAll)
        .expect("the document resolves")
        .serialize(&stemma::ExportOptions::default())
        .expect("the resolved document saves");
    let kept: Vec<String> = package_paragraphs(&rejected)
        .into_iter()
        .filter(|text| !text.is_empty())
        .collect();
    assert_eq!(kept, ["Harbour rules.", "1. Berths"]);
}

#[test]
fn the_package_resolver_rejects_a_field_spanning_paragraphs_whole() {
    let body = format!(
        concat!(
            "<w:p>{heading}</w:p>",
            r#"<w:p><w:ins w:id="5" {who}>"#,
            r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r>"#,
            r#"<w:r><w:instrText xml:space="preserve"> BIBLIOGRAPHY </w:instrText></w:r>"#,
            r#"<w:r><w:fldChar w:fldCharType="separate"/></w:r>"#,
            "</w:ins>{first}</w:p>",
            "<w:p>{second}</w:p>",
            r#"<w:p><w:pPr><w:jc w:val="center"/></w:pPr><w:ins w:id="6" {who}>"#,
            r#"<w:r><w:fldChar w:fldCharType="end"/></w:r></w:ins></w:p>"#,
            "<w:p>{closing}</w:p>",
        ),
        who = WHO,
        heading = text_run("Sources"),
        first = text_run("1. Harbour charts."),
        second = text_run("2. Tide tables."),
        closing = text_run("Closing line."),
    );
    let archive = stemma::docx::DocxArchive::read(&docx(&body)).expect("the package reads");
    let (rejected, _) = stemma::normalize::reject_all_docx(&archive).expect("reject");
    let bytes = rejected.write().expect("the resolved package writes");
    assert_eq!(package_paragraphs(&bytes), ["Sources", "", "Closing line."]);
    let xml = String::from_utf8_lossy(
        stemma::docx::DocxArchive::read(&bytes)
            .unwrap()
            .get("word/document.xml")
            .unwrap(),
    )
    .to_string();
    assert!(
        xml.contains(r#"<w:jc w:val="center""#),
        "the paragraph left keeps the end marker's paragraph properties: {xml}"
    );
}

/// A heading and its paragraph moved (text in `w:moveFrom`/`w:moveTo`, marks
/// untracked, one range around both paragraphs), then reworded at the
/// destination: the heading's last word deleted there and a new one
/// inserted after the move's text. Word 16 Reject All: each source paragraph
/// comes back holding its destination counterpart's moved text (the deleted
/// word restored), and nothing is left at the destination.
#[test]
fn rejecting_a_reworded_move_of_several_paragraphs_returns_each_destination_paragraph() {
    let body = format!(
        concat!(
            r#"<w:p><w:moveToRangeStart w:id="1" {who} w:name="move1"/>"#,
            r#"<w:moveTo w:id="2" {who}>{to_heading}"#,
            r#"<w:del w:id="3" {who}><w:r><w:delText>berths</w:delText></w:r></w:del></w:moveTo>"#,
            r#"<w:ins w:id="4" {who}>{inserted}</w:ins></w:p>"#,
            r#"<w:p><w:moveTo w:id="5" {who}>{to_body}</w:moveTo></w:p>"#,
            r#"<w:moveToRangeEnd w:id="1"/>"#,
            "<w:p>{middle}</w:p>",
            r#"<w:p><w:moveFromRangeStart w:id="6" {who} w:name="move1"/>"#,
            r#"<w:moveFrom w:id="7" {who}>{from_heading}</w:moveFrom></w:p>"#,
            r#"<w:p><w:moveFrom w:id="8" {who}>{from_body}</w:moveFrom></w:p>"#,
            r#"<w:moveFromRangeEnd w:id="6"/>"#,
            "<w:p>{closing}</w:p>",
        ),
        who = WHO,
        to_heading = text_run("Quays and "),
        inserted = text_run("piers"),
        to_body = text_run("Ships moor at the quays."),
        middle = text_run("Middle."),
        from_heading = text_run("QUAYS AND BERTHS"),
        from_body = text_run("Ships moor at the quays"),
        closing = text_run("Closing line."),
    );
    assert_eq!(
        resolved(&body, Resolution::RejectAll),
        resolved(
            &format!(
                "<w:p/><w:p/><w:p>{}</w:p><w:p>{}</w:p><w:p>{}</w:p><w:p>{}</w:p>",
                text_run("Middle."),
                text_run("Quays and berths"),
                text_run("Ships moor at the quays."),
                text_run("Closing line."),
            ),
            Resolution::RejectAll
        )
    );
}

/// Two whole fields inside one tracked deletion: accepting removes both with
/// their results (one stretch, not two overlapping removals).
#[test]
fn two_fields_inside_one_deletion_are_accepted_away_together() {
    let field = |instruction: &str, result: &str| {
        format!(
            concat!(
                r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r>"#,
                r#"<w:r><w:delInstrText xml:space="preserve">{}</w:delInstrText></w:r>"#,
                r#"<w:r><w:fldChar w:fldCharType="separate"/></w:r>"#,
                r#"<w:r><w:delText>{}</w:delText></w:r>"#,
                r#"<w:r><w:fldChar w:fldCharType="end"/></w:r>"#,
            ),
            instruction, result
        )
    };
    let body = format!(
        r#"<w:p>{}<w:del w:id="5" {WHO}>{}{}</w:del>{}</w:p>"#,
        text_run("Quay "),
        field(" AUTHOR ", "Ingrid"),
        field(" PAGE ", "4"),
        text_run(" is open."),
    );
    assert_eq!(resolved(&body, Resolution::AcceptAll), "Quay  is open.");
    let archive = stemma::docx::DocxArchive::read(&docx(&body)).expect("the package reads");
    let (accepted, _) = stemma::normalize::normalize_docx(&archive).expect("accept");
    assert_eq!(
        package_paragraphs(&accepted.write().expect("writes")),
        ["Quay  is open."]
    );
}
