//! A bookmark marker that sits directly in the body inside a `w:ins` or
//! `w:del`.
//!
//! The schema allows run-level elements, among them `w:ins`/`w:del` and the
//! range markers, directly in `w:body` (ECMA-376 §17.2.2 `CT_Body`,
//! `EG_RunLevelElements`). Word does not track bookmarks: observed in Word,
//! such a document opens without repair, lists no revision for the wrapper,
//! and keeps the bookmark on Accept All and on Reject All alike. The engine
//! reads it the same way: the bookmark is part of the document, carries no
//! revision, and survives saving and both resolutions with its start before
//! its end. (Word itself saves such a marker at the start of the next
//! paragraph, the same position.)

use std::io::Write;

use stemma::api::Document;
use stemma::{ExportOptions, Resolution};

fn docx(body: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"></Relationships>"#;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = zip::write::FileOptions::default();
        for (name, part) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/document.xml", document_xml.as_str()),
        ] {
            zip.start_file(name, opts).expect("start part");
            zip.write_all(part.as_bytes()).expect("write part");
        }
        zip.finish().expect("finish package");
    }
    buf
}

fn saved(document: &Document) -> Vec<u8> {
    document
        .serialize(&ExportOptions::default())
        .expect("serialize")
}

/// The saved document's bookmarks, by name, in document order of their
/// starts, after checking each start has one end with its id, after it.
fn bookmarks(document: &Document) -> Vec<String> {
    let bytes = saved(document);
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("saved package");
    let mut xml = String::new();
    std::io::Read::read_to_string(
        &mut archive.by_name("word/document.xml").expect("main part"),
        &mut xml,
    )
    .expect("read main part");
    let attribute = |tag: &str, name: &str| -> String {
        let key = format!("w:{name}=\"");
        let at = tag.find(&key).expect("attribute") + key.len();
        tag[at..at + tag[at..].find('"').expect("quote")].to_string()
    };
    let mut events = Vec::new();
    let mut rest = xml.as_str();
    while let Some(at) = rest.find("<w:bookmark") {
        let end = rest[at..].find('>').expect("tag closes") + at;
        let tag = &rest[at..=end];
        if tag.starts_with("<w:bookmarkStart") {
            events.push((true, attribute(tag, "id"), attribute(tag, "name")));
        } else if tag.starts_with("<w:bookmarkEnd") {
            events.push((false, attribute(tag, "id"), String::new()));
        }
        rest = &rest[end..];
    }
    let mut names = Vec::new();
    for (index, (is_start, id, name)) in events.iter().enumerate() {
        if !is_start {
            continue;
        }
        let ends: Vec<usize> = events
            .iter()
            .enumerate()
            .filter(|(_, (start, other, _))| !start && other == id)
            .map(|(i, _)| i)
            .collect();
        assert!(
            ends.len() == 1 && ends[0] > index,
            "bookmark {name:?} has one end, after its start: {events:?}"
        );
        names.push(name.clone());
    }
    names
}

fn reopened(document: &Document) -> Document {
    Document::parse(&saved(document)).expect("reopen")
}

#[test]
fn a_bookmark_in_a_body_level_insertion_is_kept_untracked() {
    let document = Document::parse(&docx(
        r#"<w:p><w:r><w:t>Opening hours</w:t></w:r></w:p><w:ins w:id="20" w:author="Ann"><w:bookmarkStart w:id="7" w:name="hours"/></w:ins><w:ins w:id="21" w:author="Ann"><w:bookmarkEnd w:id="7"/></w:ins><w:p><w:r><w:t>The library opens at nine.</w:t></w:r></w:p>"#,
    ))
    .expect("parse");
    assert!(document.revisions().is_empty(), "Word lists no revision");
    assert_eq!(bookmarks(&reopened(&document)), ["hours"], "saved as read");
    for resolution in [Resolution::AcceptAll, Resolution::RejectAll] {
        let resolved = reopened(&document.project(resolution).expect("resolve"));
        assert_eq!(bookmarks(&resolved), ["hours"], "kept by either resolution");
    }
}

#[test]
fn a_bookmark_in_a_body_level_deletion_is_kept_untracked() {
    let document = Document::parse(&docx(
        r#"<w:p><w:r><w:t>Opening hours</w:t></w:r></w:p><w:del w:id="20" w:author="Ann"><w:bookmarkStart w:id="7" w:name="hours"/><w:bookmarkEnd w:id="7"/></w:del><w:p><w:r><w:t>The library opens at nine.</w:t></w:r></w:p>"#,
    ))
    .expect("parse");
    assert!(document.revisions().is_empty(), "Word lists no revision");
    assert_eq!(bookmarks(&reopened(&document)), ["hours"], "saved as read");
    for resolution in [Resolution::AcceptAll, Resolution::RejectAll] {
        let resolved = reopened(&document.project(resolution).expect("resolve"));
        assert_eq!(bookmarks(&resolved), ["hours"], "kept by either resolution");
    }
}
