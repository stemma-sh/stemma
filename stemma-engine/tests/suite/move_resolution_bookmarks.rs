//! Bookmarks around a move when it is resolved (Word's ruling F11).
//!
//! Word, on the same shapes (resolving everything and resolving the move
//! alone give one result):
//! - the move range markers always go;
//! - Reject: bookmarks inside or straddling the moved-from side stay around
//!   the restored text; bookmarks inside the moved-to side go with it;
//! - Accept: bookmarks inside the moved-to side stay; a bookmark that starts
//!   or ends inside the moved-from side is removed whole (not collapsed at
//!   the source, not carried to the destination);
//! - another author's unrelated revision stays pending through a by-revision
//!   resolution.

use std::collections::HashSet;
use std::io::Write;

use stemma::api::Document;
use stemma::{ExportOptions, Resolution, ResolveSelectionAction, RevisionKind};

fn docx(body: &str) -> Vec<u8> {
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let document_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
    let mut bytes = Vec::new();
    {
        use zip::write::FileOptions;
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
        let options: FileOptions = FileOptions::default();
        for (name, data) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", root_rels),
            ("word/_rels/document.xml.rels", document_rels),
            ("word/document.xml", document.as_str()),
        ] {
            zip.start_file(name, options).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    bytes
}

const MOVE: &str = r#"w:author="Mover" w:date="2026-01-01T00:00:00Z""#;

/// A bookmark wholly inside the moved-from side, and another author's
/// insertion beside the destination.
fn bookmark_in_source() -> String {
    format!(
        concat!(
            r#"<w:p><w:r><w:t xml:space="preserve">Ahead. </w:t></w:r>"#,
            r#"<w:moveFromRangeStart w:id="1" {m} w:name="move1"/><w:bookmarkStart w:id="20" w:name="Kept"/>"#,
            r#"<w:moveFrom w:id="2" {m}><w:r><w:t xml:space="preserve">Roaming sentence. </w:t></w:r></w:moveFrom>"#,
            r#"<w:bookmarkEnd w:id="20"/><w:moveFromRangeEnd w:id="1"/><w:r><w:t>Behind.</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t xml:space="preserve">Landing. </w:t></w:r><w:moveToRangeStart w:id="3" {m} w:name="move1"/>"#,
            r#"<w:moveTo w:id="4" {m}><w:r><w:t xml:space="preserve">Roaming sentence. </w:t></w:r></w:moveTo><w:moveToRangeEnd w:id="3"/>"#,
            r#"<w:ins w:id="5" w:author="Other" w:date="2026-01-02T00:00:00Z"><w:r><w:t xml:space="preserve">Aside. </w:t></w:r></w:ins>"#,
            r#"<w:r><w:t>End.</w:t></w:r></w:p>"#,
        ),
        m = MOVE
    )
}

/// A bookmark starting before the move and ending inside the moved-from
/// side, and one inside the moved-to side.
fn straddling_and_destination_bookmarks() -> String {
    format!(
        concat!(
            r#"<w:p><w:r><w:t xml:space="preserve">Ahead. </w:t></w:r><w:bookmarkStart w:id="21" w:name="Straddle"/>"#,
            r#"<w:moveFromRangeStart w:id="1" {m} w:name="move1"/>"#,
            r#"<w:moveFrom w:id="2" {m}><w:r><w:t xml:space="preserve">Roaming </w:t></w:r></w:moveFrom><w:bookmarkEnd w:id="21"/>"#,
            r#"<w:moveFrom w:id="2" {m}><w:r><w:t xml:space="preserve">sentence. </w:t></w:r></w:moveFrom>"#,
            r#"<w:moveFromRangeEnd w:id="1"/><w:r><w:t>Behind.</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t xml:space="preserve">Landing. </w:t></w:r><w:moveToRangeStart w:id="3" {m} w:name="move1"/>"#,
            r#"<w:moveTo w:id="4" {m}><w:r><w:t xml:space="preserve">Roaming </w:t></w:r></w:moveTo>"#,
            r#"<w:bookmarkStart w:id="22" w:name="InLanding"/><w:bookmarkEnd w:id="22"/>"#,
            r#"<w:moveTo w:id="4" {m}><w:r><w:t xml:space="preserve">sentence. </w:t></w:r></w:moveTo>"#,
            r#"<w:moveToRangeEnd w:id="3"/><w:r><w:t>End.</w:t></w:r></w:p>"#,
        ),
        m = MOVE
    )
}

/// The body as text with `[name>` / `<]` for bookmark starts and ends, and
/// whether any move range marker is left.
fn reading(doc: &Document) -> (String, bool) {
    let bytes = doc.serialize(&ExportOptions::default()).expect("save");
    let xml = String::from_utf8(
        stemma::docx::DocxArchive::read(&bytes)
            .unwrap()
            .get("word/document.xml")
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    let body = &xml[xml.find("<w:body>").unwrap()..];
    let mut out = String::new();
    let mut rest = body;
    while let Some(open) = rest.find('<') {
        let close = open + rest[open..].find('>').unwrap();
        let tag = &rest[open + 1..close];
        if tag.starts_with("w:t>") || tag.starts_with("w:t ") || tag == "w:t" {
            let end = close + 1 + rest[close + 1..].find("</w:t>").unwrap();
            out.push_str(&rest[close + 1..end]);
            rest = &rest[end..];
            continue;
        }
        if let Some(attrs) = tag.strip_prefix("w:bookmarkStart ") {
            let name = attrs
                .split("w:name=\"")
                .nth(1)
                .unwrap()
                .split('"')
                .next()
                .unwrap();
            out.push_str(&format!("[{name}>"));
        } else if tag.starts_with("w:bookmarkEnd ") {
            out.push_str("<]");
        } else if tag.starts_with("/w:p>") || tag == "/w:p" {
            out.push('|');
        }
        rest = &rest[close + 1..];
    }
    (
        out,
        body.contains("RangeStart") || body.contains("RangeEnd"),
    )
}

fn move_id(doc: &Document) -> u32 {
    stemma::enumerate_revisions(&doc.snapshot().canonical)
        .iter()
        .find(|r| r.kind == RevisionKind::Move)
        .expect("a move")
        .revision_id
}

/// Both resolution paths, with Word's reading for each: `[accept all,
/// accept by id, reject all, reject by id]`. They differ only by the text of
/// an unrelated revision that resolving the move alone leaves pending.
fn assert_word_readings(body: &str, expected: [&str; 4]) {
    let doc = Document::parse(&docx(body)).expect("parse");
    let id = move_id(&doc);
    let mut expected = expected.into_iter();
    for (action, all) in [
        (ResolveSelectionAction::Accept, Resolution::AcceptAll),
        (ResolveSelectionAction::Reject, Resolution::RejectAll),
    ] {
        let by_id = doc
            .project(Resolution::Selective {
                ids: HashSet::from([id]),
                action,
            })
            .expect("resolve the move");
        for (label, resolved) in [
            ("all", doc.project(all).expect("resolve all")),
            ("by id", by_id),
        ] {
            let (text, range_markers_left) = reading(&resolved);
            assert_eq!(text, expected.next().unwrap(), "{action:?} {label}");
            assert!(
                !range_markers_left,
                "{action:?} {label}: the move range markers go"
            );
        }
    }
}

#[test]
fn a_bookmark_inside_the_moved_from_side_goes_on_accept_and_stays_on_reject() {
    assert_word_readings(
        &bookmark_in_source(),
        [
            "Ahead. Behind.|Landing. Roaming sentence. Aside. End.|",
            "Ahead. Behind.|Landing. Roaming sentence. Aside. End.|",
            "Ahead. [Kept>Roaming sentence. <]Behind.|Landing. End.|",
            "Ahead. [Kept>Roaming sentence. <]Behind.|Landing. Aside. End.|",
        ],
    );
}

#[test]
fn an_unrelated_revision_stays_pending_through_resolving_the_move() {
    let doc = Document::parse(&docx(&bookmark_in_source())).expect("parse");
    let id = move_id(&doc);
    for action in [
        ResolveSelectionAction::Accept,
        ResolveSelectionAction::Reject,
    ] {
        let resolved = doc
            .project(Resolution::Selective {
                ids: HashSet::from([id]),
                action,
            })
            .expect("resolve the move");
        let pending: Vec<_> = stemma::enumerate_revisions(&resolved.snapshot().canonical)
            .into_iter()
            .map(|r| (r.author, r.kind))
            .collect();
        assert_eq!(
            pending,
            vec![(Some("Other".to_string()), RevisionKind::Insert)],
            "{action:?}"
        );
    }
}

#[test]
fn a_straddling_source_bookmark_goes_on_accept_and_a_destination_bookmark_on_reject() {
    assert_word_readings(
        &straddling_and_destination_bookmarks(),
        [
            "Ahead. Behind.|Landing. Roaming [InLanding><]sentence. End.|",
            "Ahead. Behind.|Landing. Roaming [InLanding><]sentence. End.|",
            "Ahead. [Straddle>Roaming <]sentence. Behind.|Landing. End.|",
            "Ahead. [Straddle>Roaming <]sentence. Behind.|Landing. End.|",
        ],
    );
}
