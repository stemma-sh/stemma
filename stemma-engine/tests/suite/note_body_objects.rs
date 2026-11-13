//! `edit_note` edits a note that holds a hyperlink (or another inline object
//! or a break), and the notes read shows the body the way `edit_note` takes it.
//!
//! Domain rules:
//! - A note story (§17.11.10 `w:footnote`) holds ordinary paragraphs, and a
//!   link (§17.16.22) in one is part of the note, not a reason it cannot be
//!   edited. `edit_note` carries every inline object and break of a paragraph
//!   in place, never authoring or dropping one: in the body line each stands
//!   as one U+FFFC (the engine's no-text-object character), in order, and the
//!   text around them is word-diffed like a body paragraph's replace.
//! - The notes read (`note_body_text`) is that same text: one line per note
//!   paragraph, joined by `\n` (paragraphs are not fused), so a read row
//!   edited and handed back to `edit_note` is a valid body.
//! - Reject All restores the note; Accept All shows the edit with the link
//!   intact; save and reopen keeps both.
//!
//! Daily tier: synthetic in-memory DOCX.

use stemma::api::Document;
use stemma::edit::{EditError, note_body_text};
use stemma::{ExportOptions, Resolution};

use crate::common::read_zip_entry;

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn docx(footnote_body: &str) -> Vec<u8> {
    use std::io::Write;
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}"><w:body><w:p><w:r><w:t>The term is defined.</w:t></w:r><w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:footnoteReference w:id="1"/></w:r></w:p><w:sectPr/></w:body></w:document>"#
    );
    let footnotes_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:footnotes xmlns:w="{W}"><w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote><w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote><w:footnote w:id="1">{footnote_body}</w:footnote></w:footnotes>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/footnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdFn" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes" Target="footnotes.xml"/></Relationships>"#;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions = zip::write::FileOptions::default();
        for (name, data) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/document.xml", document_xml.as_str()),
            ("word/footnotes.xml", footnotes_xml.as_str()),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

/// A two-paragraph note whose first paragraph holds an internal link.
const LINKED_NOTE: &str = r#"<w:p><w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:footnoteRef/></w:r><w:r><w:t xml:space="preserve"> See the </w:t></w:r><w:hyperlink w:anchor="terms"><w:r><w:t>standard terms</w:t></w:r></w:hyperlink><w:r><w:t xml:space="preserve"> for detail.</w:t></w:r></w:p><w:p><w:r><w:t>Rates are annual.</w:t></w:r></w:p>"#;

fn note_text(doc: &Document) -> String {
    let snapshot = doc.snapshot();
    let note = snapshot
        .canonical
        .footnotes
        .iter()
        .find(|note| note.id == "1")
        .expect("note 1");
    note_body_text(&note.blocks)
}

fn edit_note(doc: &Document, body: &str, mode: &str) -> Result<Document, stemma::RuntimeError> {
    let json = serde_json::json!({
        "ops": [{"op": "edit_note", "note_id": "1", "note_kind": "footnote", "body": body}],
        "revision": {"author": "Reviewer", "date": "2026-11-05T19:00:00Z"},
        "materialization_mode": mode,
    })
    .to_string();
    doc.apply(
        &stemma::edit_v4::parse_transaction(&json)
            .unwrap()
            .into_edit_transaction()
            .unwrap(),
    )
}

fn footnote_xml(doc: &Document) -> String {
    let bytes = doc.serialize(&ExportOptions::default()).expect("serialize");
    let xml = read_zip_entry(&bytes, "word/footnotes.xml").expect("footnotes.xml");
    let start = xml.find(r#"<w:footnote w:id="1""#).expect("note 1");
    xml[start..].to_string()
}

#[test]
fn the_notes_read_shows_one_line_per_paragraph_and_a_placeholder_per_object() {
    let doc = Document::parse(&docx(LINKED_NOTE)).unwrap();
    assert_eq!(
        note_text(&doc),
        " See the \u{FFFC} for detail.\nRates are annual."
    );
}

#[test]
fn edit_note_edits_around_a_link_and_keeps_it() {
    let source = Document::parse(&docx(LINKED_NOTE)).unwrap();
    let edited = edit_note(
        &source,
        " See the \u{FFFC} for full detail.\nRates are annual and fixed.",
        "tracked_change",
    )
    .expect("a note holding a link is editable");

    let accepted = edited.project(Resolution::AcceptAll).unwrap();
    assert_eq!(
        note_text(&accepted),
        " See the \u{FFFC} for full detail.\nRates are annual and fixed."
    );
    let rejected = edited.project(Resolution::RejectAll).unwrap();
    assert_eq!(note_text(&rejected), note_text(&source));

    // The link is the source's own, with its text, in both readings.
    for reading in [&accepted, &rejected, &edited] {
        let xml = footnote_xml(reading);
        assert!(
            xml.contains(r#"<w:hyperlink w:anchor="terms"><w:r><w:t>standard terms</w:t></w:r></w:hyperlink>"#),
            "the link survives untouched: {xml}"
        );
    }

    let reopened =
        Document::parse(&edited.serialize(&ExportOptions::default()).unwrap()).expect("reopen");
    assert_eq!(footnote_xml(&reopened), footnote_xml(&edited));
    assert_eq!(
        note_text(&reopened.project(Resolution::AcceptAll).unwrap()),
        note_text(&accepted)
    );
}

#[test]
fn a_direct_edit_note_keeps_the_link() {
    let source = Document::parse(&docx(LINKED_NOTE)).unwrap();
    let edited = edit_note(
        &source,
        " Read the \u{FFFC} first.\nRates are annual.",
        "direct",
    )
    .expect("direct edit");
    assert_eq!(
        note_text(&edited),
        " Read the \u{FFFC} first.\nRates are annual."
    );
    assert!(
        edited.revisions().is_empty(),
        "a direct edit leaves no revision"
    );
    assert!(footnote_xml(&edited).contains("<w:hyperlink w:anchor=\"terms\">"));
}

#[test]
fn a_line_without_the_links_placeholder_is_refused() {
    let source = Document::parse(&docx(LINKED_NOTE)).unwrap();
    let transaction = stemma::edit_v4::parse_transaction(
        &serde_json::json!({
            "ops": [{"op": "edit_note", "note_id": "1", "note_kind": "footnote",
                "body": " See the standard terms for detail.\nRates are annual."}],
            "revision": {"author": "Reviewer"},
        })
        .to_string(),
    )
    .unwrap()
    .into_edit_transaction()
    .unwrap();
    let error = stemma::edit::apply_transaction(&source.snapshot().canonical, &transaction)
        .expect_err("leaving out the link would destroy it");
    assert!(
        matches!(
            error,
            EditError::NoteBodyObjectCountMismatch {
                paragraph_index: 0,
                object_count: 1,
                placeholder_count: 0,
                ..
            }
        ),
        "{error:?}"
    );
}

// ---------------------------------------------------------------------------
// A note with pending tracked changes.
//
// Domain rules:
// - The notes read is plain text, so it cannot show a segment's status. It is
//   the text `edit_note` takes back, and the only mode that edits such a note
//   is direct, which works on the note's accepted content (as a direct body
//   paragraph edit does). So the read is the note's ACCEPTED reading:
//   deleted text, breaks and objects are absent, inserted ones present.
// - Direct `edit_note` accepts the pending changes and applies the edit,
//   leaving no revision; a read row handed back unchanged is a valid body
//   whose result reads the same.
// - Tracked `edit_note` does not stack a change on pending changes: it is
//   refused (`ParagraphContainsTrackedSegments`), never a panic or a
//   placeholder-count error.
// ---------------------------------------------------------------------------

const REF_RUN: &str =
    r#"<w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:footnoteRef/></w:r>"#;

fn pending_note(middle: &str) -> String {
    format!(
        r#"<w:p>{REF_RUN}<w:r><w:t xml:space="preserve"> See the </w:t></w:r>{middle}<w:r><w:t xml:space="preserve">terms for detail.</w:t></w:r></w:p>"#
    )
}

const DELETED_BREAK: &str =
    r#"<w:del w:id="5" w:author="Mira" w:date="2026-11-01T19:00:00Z"><w:r><w:br/></w:r></w:del>"#;
const DELETED_TEXT: &str = r#"<w:del w:id="5" w:author="Mira" w:date="2026-11-01T19:00:00Z"><w:r><w:delText xml:space="preserve">old </w:delText></w:r></w:del>"#;
const INSERTED_TEXT: &str = r#"<w:ins w:id="5" w:author="Mira" w:date="2026-11-01T19:00:00Z"><w:r><w:t xml:space="preserve">standard </w:t></w:r></w:ins>"#;
const INSERTED_LINK: &str = r#"<w:hyperlink w:anchor="glossary"><w:ins w:id="5" w:author="Mira" w:date="2026-11-01T19:00:00Z"><w:r><w:t>glossary</w:t></w:r></w:ins></w:hyperlink><w:ins w:id="6" w:author="Mira" w:date="2026-11-01T19:00:00Z"><w:r><w:t xml:space="preserve"> </w:t></w:r></w:ins>"#;

/// (pending change, accepted reading as the notes read shows it)
fn pending_cases() -> [(&'static str, &'static str, &'static str); 4] {
    [
        ("deleted break", DELETED_BREAK, " See the terms for detail."),
        ("deleted text", DELETED_TEXT, " See the terms for detail."),
        (
            "inserted text",
            INSERTED_TEXT,
            " See the standard terms for detail.",
        ),
        (
            "inserted link",
            INSERTED_LINK,
            " See the \u{FFFC} terms for detail.",
        ),
    ]
}

#[test]
fn the_notes_read_shows_a_pending_note_as_its_accepted_text() {
    for (label, middle, accepted) in pending_cases() {
        let doc = Document::parse(&docx(&pending_note(middle))).unwrap();
        assert_eq!(note_text(&doc), accepted, "{label}");
        let accepted_doc = doc.project(Resolution::AcceptAll).unwrap();
        assert_eq!(note_text(&accepted_doc), accepted, "{label}: accept all");
    }
}

#[test]
fn a_direct_edit_note_on_a_pending_note_accepts_and_edits() {
    for (label, middle, accepted) in pending_cases() {
        let source = Document::parse(&docx(&pending_note(middle))).unwrap();
        let edited_line = accepted.replace("detail", "full detail");
        let edited = edit_note(&source, &edited_line, "direct")
            .unwrap_or_else(|error| panic!("{label}: direct edit refused: {error:?}"));
        assert_eq!(note_text(&edited), edited_line, "{label}");
        assert!(
            edited.revisions().is_empty(),
            "{label}: a direct edit leaves no revision"
        );
        let xml = footnote_xml(&edited);
        assert!(!xml.contains("<w:br/>"), "{label}: {xml}");
        assert!(!xml.contains("old "), "{label}: {xml}");
        if middle == INSERTED_LINK {
            assert!(
                xml.contains(r#"<w:hyperlink w:anchor="glossary">"#)
                    && xml.contains("glossary</w:t>"),
                "{label}: the link and its text survive: {xml}"
            );
        }
        let reopened =
            Document::parse(&edited.serialize(&ExportOptions::default()).unwrap()).expect("reopen");
        assert_eq!(note_text(&reopened), edited_line, "{label}: reopen");
    }
}

#[test]
fn a_pending_notes_read_row_handed_back_unchanged_is_a_valid_direct_body() {
    for (label, middle, accepted) in pending_cases() {
        let source = Document::parse(&docx(&pending_note(middle))).unwrap();
        let read = note_text(&source);
        let edited = edit_note(&source, &read, "direct")
            .unwrap_or_else(|error| panic!("{label}: unchanged row refused: {error:?}"));
        assert_eq!(note_text(&edited), accepted, "{label}");
        assert!(edited.revisions().is_empty(), "{label}");
    }
}

#[test]
fn a_tracked_edit_note_on_a_pending_note_is_refused() {
    for (label, middle, accepted) in pending_cases() {
        let source = Document::parse(&docx(&pending_note(middle))).unwrap();
        let body = accepted.replace("detail", "full detail");
        let transaction = stemma::edit_v4::parse_transaction(
            &serde_json::json!({
                "ops": [{"op": "edit_note", "note_id": "1", "note_kind": "footnote", "body": body}],
                "revision": {"author": "Reviewer"},
            })
            .to_string(),
        )
        .unwrap()
        .into_edit_transaction()
        .unwrap();
        let error = stemma::edit::apply_transaction(&source.snapshot().canonical, &transaction)
            .expect_err("no stacking a change on pending changes");
        assert!(
            matches!(error, EditError::ParagraphContainsTrackedSegments { .. }),
            "{label}: {error:?}"
        );
    }
}
