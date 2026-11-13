//! No Stemma producer writes deleted text inside a move's source.
//!
//! ECMA-376 §17.13.5.22: `w:moveFrom` holds runs of plain text (`w:t`); Word
//! reports a file whose moveFrom runs carry `w:delText` as corrupted and
//! repairs it (checked in Word). Every way the engine writes a move must spell
//! moved-from text as `w:t`: the move verb (one paragraph, a range of
//! paragraphs), a compare that detects a move, and a pending move written
//! back after an unrelated resolution, inline and whole-paragraph, in the
//! body and in a footnote.

use std::collections::HashSet;
use std::io::Write;

use stemma::api::Document;
use stemma::{ExportOptions, Resolution, ResolveSelectionAction};
#[allow(unused_imports)]
use stemma_diff::test_support::DocumentComparisonExt as _;

fn docx(body: &str, footnote_body: Option<&str>) -> Vec<u8> {
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let mut parts: Vec<(&str, String)> = vec![
        (
            "_rels/.rels",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#.to_string(),
        ),
        ("word/document.xml", document),
    ];
    let mut overrides = String::from(
        r#"<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>"#,
    );
    if let Some(note) = footnote_body {
        overrides.push_str(r#"<Override PartName="/word/footnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"/>"#);
        parts.push((
            "word/_rels/document.xml.rels",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes" Target="footnotes.xml"/></Relationships>"#.to_string(),
        ));
        parts.push((
            "word/footnotes.xml",
            format!(
                concat!(
                    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
                    r#"<w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote>"#,
                    r#"<w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote>"#,
                    r#"<w:footnote w:id="1">{note}</w:footnote></w:footnotes>"#
                ),
                note = note
            ),
        ));
    }
    parts.push((
        "[Content_Types].xml",
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/>{overrides}</Types>"#
        ),
    ));
    let mut bytes = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
        let options: zip::write::FileOptions = zip::write::FileOptions::default();
        for (name, data) in &parts {
            zip.start_file(*name, options).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    bytes
}

/// Every `w:moveFrom` in every XML part of `bytes` holds `w:t`, never
/// `w:delText` outside a nested `w:del`.
fn assert_move_from_text_is_plain(bytes: &[u8], label: &str) {
    let archive = stemma::docx::DocxArchive::read(bytes).expect("package");
    let mut saw_move_from = false;
    for name in archive.list().map(str::to_string).collect::<Vec<_>>() {
        if !name.ends_with(".xml") {
            continue;
        }
        let xml = String::from_utf8_lossy(archive.get(&name).unwrap()).to_string();
        let mut rest = xml.as_str();
        while let Some(start) = rest.find("<w:moveFrom ") {
            saw_move_from = true;
            let after = &rest[start..];
            let end = after.find("</w:moveFrom>").unwrap_or(after.len());
            let inside = &after[..end];
            let outside_del = inside
                .split("<w:del ")
                .enumerate()
                .map(|(i, piece)| {
                    if i == 0 {
                        piece
                    } else {
                        piece.split("</w:del>").nth(1).unwrap_or("")
                    }
                })
                .collect::<String>();
            assert!(
                !outside_del.contains("<w:delText"),
                "{label}: {name} writes w:delText inside w:moveFrom: {inside}"
            );
            rest = &after[end.min(after.len())..];
            if end == after.len() {
                break;
            }
        }
    }
    assert!(saw_move_from, "{label}: the output still holds the move");
    assert!(
        stemma::api::validate(bytes).ok,
        "{label}: {:?}",
        stemma::api::validate(bytes)
    );
}

fn save(doc: &Document) -> Vec<u8> {
    doc.serialize(&ExportOptions::default()).expect("save")
}

fn apply(doc: &Document, ops: serde_json::Value) -> Document {
    let json = serde_json::json!({"ops": ops, "revision": {"author": "Mover"}}).to_string();
    doc.apply(
        &stemma::edit_v4::parse_transaction(&json)
            .unwrap()
            .into_edit_transaction()
            .unwrap(),
    )
    .expect("apply")
}

fn reject_author(doc: &Document, author: &str) -> Document {
    let ids: HashSet<u32> = stemma::enumerate_revisions(&doc.snapshot().canonical)
        .into_iter()
        .filter(|r| r.author.as_deref() == Some(author))
        .map(|r| r.revision_id)
        .collect();
    doc.project(Resolution::Selective {
        ids,
        action: ResolveSelectionAction::Reject,
    })
    .expect("resolve")
}

const FIVE: &str = concat!(
    r#"<w:p><w:r><w:t>Alpha line one.</w:t></w:r></w:p>"#,
    r#"<w:p><w:r><w:t>Bravo line two.</w:t></w:r></w:p>"#,
    r#"<w:p><w:r><w:t>Charlie line three.</w:t></w:r></w:p>"#,
    r#"<w:p><w:r><w:t>Delta line four.</w:t></w:r></w:p>"#,
    r#"<w:p><w:r><w:t xml:space="preserve">Echo line five, </w:t></w:r><w:ins w:id="90" w:author="Other" w:date="2026-01-02T00:00:00Z"><w:r><w:t>amended</w:t></w:r></w:ins></w:p>"#,
);

#[test]
fn the_move_verb_writes_plain_moved_from_text() {
    let doc = Document::parse(&docx(FIVE, None)).expect("parse");
    for (label, ops) in [
        (
            "one paragraph",
            serde_json::json!([{"op":"move","target":"p_2","destination":{"anchor":"p_4","position":"after"}}]),
        ),
        (
            "a range of paragraphs",
            serde_json::json!([{"op":"move","target":{"from":"p_2","to":"p_3"},"destination":{"anchor":"p_4","position":"after"}}]),
        ),
    ] {
        let moved = apply(&doc, ops);
        assert_move_from_text_is_plain(&save(&moved), label);
        // The pending move written back after an unrelated resolution.
        let other_rejected = reject_author(&moved, "Other");
        assert_move_from_text_is_plain(
            &save(&other_rejected),
            &format!("{label}, after a resolution"),
        );
    }
}

#[test]
fn a_compare_that_detects_a_move_writes_plain_moved_from_text() {
    let base = Document::parse(&docx(
        concat!(
            r#"<w:p><w:r><w:t>The first clause stays exactly where it is.</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t>The travelling clause moves below the anchor clause.</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t>The anchor clause is the one it moves below.</w:t></w:r></w:p>"#,
        ),
        None,
    ))
    .unwrap();
    let target = Document::parse(&docx(
        concat!(
            r#"<w:p><w:r><w:t>The first clause stays exactly where it is.</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t>The anchor clause is the one it moves below.</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t>The travelling clause moves below the anchor clause.</w:t></w:r></w:p>"#,
        ),
        None,
    ))
    .unwrap();
    let redline = base.diff(&target).expect("compare");
    let bytes = save(&redline);
    let xml = String::from_utf8_lossy(
        stemma::docx::DocxArchive::read(&bytes)
            .unwrap()
            .get("word/document.xml")
            .unwrap(),
    )
    .to_string();
    assert!(
        xml.contains("<w:moveFrom "),
        "the compare detects the move: {xml}"
    );
    assert_move_from_text_is_plain(&bytes, "compare");
}

/// An inline move and a whole-paragraph move read from a file, each written
/// back after another author's revision is resolved: in the body, and the
/// inline one in a footnote.
#[test]
fn a_pending_move_written_back_after_a_resolution_keeps_plain_moved_from_text() {
    let m = r#"w:author="Mover" w:date="2026-01-01T00:00:00Z""#;
    let inline = format!(
        concat!(
            r#"<w:p><w:r><w:t xml:space="preserve">Front. </w:t></w:r><w:moveFromRangeStart w:id="10" {m} w:name="move1"/>"#,
            r#"<w:moveFrom w:id="11" {m}><w:r><w:t>travelling words</w:t></w:r></w:moveFrom><w:moveFromRangeEnd w:id="10"/>"#,
            r#"<w:r><w:t xml:space="preserve"> Centre. </w:t></w:r><w:moveToRangeStart w:id="12" {m} w:name="move1"/>"#,
            r#"<w:moveTo w:id="13" {m}><w:r><w:t>travelling words</w:t></w:r></w:moveTo><w:moveToRangeEnd w:id="12"/>"#,
            r#"<w:ins w:id="14" w:author="Other" w:date="2026-01-02T00:00:00Z"><w:r><w:t xml:space="preserve"> Aside.</w:t></w:r></w:ins></w:p>"#,
        ),
        m = m
    );
    let whole = format!(
        concat!(
            r#"<w:p><w:r><w:t>Opening.</w:t></w:r></w:p>"#,
            r#"<w:p><w:pPr><w:rPr><w:moveFrom w:id="20" {m}/></w:rPr></w:pPr><w:moveFromRangeStart w:id="21" {m} w:name="move2"/>"#,
            r#"<w:moveFrom w:id="22" {m}><w:r><w:t>Travelling paragraph.</w:t></w:r></w:moveFrom></w:p><w:moveFromRangeEnd w:id="21"/>"#,
            r#"<w:p><w:r><w:t xml:space="preserve">Middle, </w:t></w:r><w:ins w:id="23" w:author="Other" w:date="2026-01-02T00:00:00Z"><w:r><w:t>amended</w:t></w:r></w:ins></w:p>"#,
            r#"<w:p><w:pPr><w:rPr><w:moveTo w:id="24" {m}/></w:rPr></w:pPr><w:moveToRangeStart w:id="25" {m} w:name="move2"/>"#,
            r#"<w:moveTo w:id="26" {m}><w:r><w:t>Travelling paragraph.</w:t></w:r></w:moveTo></w:p><w:moveToRangeEnd w:id="25"/>"#,
            r#"<w:p><w:r><w:t>Closing.</w:t></w:r></w:p>"#,
        ),
        m = m
    );
    let with_note = concat!(
        r#"<w:p><w:r><w:t xml:space="preserve">Body text</w:t></w:r><w:r><w:footnoteReference w:id="1"/></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>Closing.</w:t></w:r></w:p>"#,
    );
    for (label, body, note) in [
        ("inline, body", inline.as_str(), None),
        ("whole paragraph, body", whole.as_str(), None),
        ("inline, footnote", with_note, Some(inline.as_str())),
    ] {
        let doc = Document::parse(&docx(body, note)).expect("parse");
        let resolved = reject_author(&doc, "Other");
        assert_move_from_text_is_plain(&save(&resolved), label);
    }
}

/// The guard itself catches the shape it guards against.
#[test]
fn the_guard_rejects_deleted_text_inside_move_from() {
    let m = r#"w:author="Mover" w:date="2026-01-01T00:00:00Z""#;
    let bad = format!(
        concat!(
            r#"<w:p><w:r><w:t xml:space="preserve">Front. </w:t></w:r><w:moveFromRangeStart w:id="10" {m} w:name="move1"/>"#,
            r#"<w:moveFrom w:id="11" {m}><w:r><w:delText>travelling words</w:delText></w:r></w:moveFrom><w:moveFromRangeEnd w:id="10"/>"#,
            r#"<w:r><w:t xml:space="preserve"> Centre. </w:t></w:r><w:moveToRangeStart w:id="12" {m} w:name="move1"/>"#,
            r#"<w:moveTo w:id="13" {m}><w:r><w:t>travelling words</w:t></w:r></w:moveTo><w:moveToRangeEnd w:id="12"/></w:p>"#,
        ),
        m = m
    );
    let bytes = docx(&bad, None);
    let caught = std::panic::catch_unwind(|| assert_move_from_text_is_plain(&bytes, "bad"));
    assert!(caught.is_err());
}
