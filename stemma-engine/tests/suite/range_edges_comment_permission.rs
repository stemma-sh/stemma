//! Comment and permission ranges at the edge of a paragraph a new paragraph
//! is inserted beside.
//!
//! Word evidence (Word 16, Track Changes off and on, Enter at the end of the
//! covered paragraph then typing, or typing then Enter at its start):
//! - a comment range does not grow: Comments(1).Scope.Text stays "Marked
//!   line" and its markers stay around that text, so the new paragraph is
//!   outside the comment;
//! - a permission range grows, as a bookmark does (bookmarks:
//!   body_level_range_marker_blocks): the new paragraph joins it. Written at
//!   block level, the range's end stays after the last covered paragraph;
//!   written inside the paragraph, its end follows the typed text into the
//!   new paragraph, and its start moves onto a new first paragraph.

use stemma::ExportOptions;
use stemma::api::Document;
use stemma::runtime::Resolution;

fn docx(body: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/comments.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let document_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="comments.xml"/></Relationships>"#;
    let comments = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:comment w:id="0" w:author="Reviewer" w:date="2026-01-01T00:00:00Z" w:initials="R"><w:p><w:r><w:t>About this line.</w:t></w:r></w:p></w:comment></w:comments>"#;
    use std::io::Write;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions = zip::write::FileOptions::default();
        for (name, data) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/_rels/document.xml.rels", document_rels),
            ("word/comments.xml", comments),
            ("word/document.xml", document_xml.as_str()),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

const OPENING: &str = r#"<w:p><w:r><w:t>Opening line</w:t></w:r></w:p>"#;
const CLOSING: &str = r#"<w:p><w:r><w:t>Closing line</w:t></w:r></w:p>"#;

fn body(marked: &str) -> String {
    format!("{OPENING}{marked}{CLOSING}")
}

const COMMENTED: &str = r#"<w:p><w:commentRangeStart w:id="0"/><w:r><w:t>Marked line</w:t></w:r><w:commentRangeEnd w:id="0"/><w:r><w:commentReference w:id="0"/></w:r></w:p>"#;
const PERMISSION_BLOCK: &str = r#"<w:permStart w:id="1" w:edGrp="everyone"/><w:p><w:r><w:t>Marked line</w:t></w:r></w:p><w:permEnd w:id="1"/>"#;
const PERMISSION_RUN: &str = r#"<w:p><w:permStart w:id="1" w:edGrp="everyone"/><w:r><w:t>Marked line</w:t></w:r><w:permEnd w:id="1"/></w:p>"#;

fn marked_id(doc: &Document) -> String {
    doc.read()
        .blocks
        .iter()
        .find(|block| block.text == "Marked line")
        .expect("the marked paragraph")
        .id
        .to_string()
}

fn insert(doc: &Document, position: &str, mode: &str) -> String {
    let json = serde_json::json!({"ops":[{"op":"insert","target":{"anchor":marked_id(doc),"position":position},
        "content":[{"type":"paragraph","content":[{"type":"text","text":"New line"}]}]}],
        "materialization_mode":mode,"revision":{"author":"Editor"}})
    .to_string();
    let edited = doc
        .apply(
            &stemma::edit_v4::parse_transaction(&json)
                .unwrap()
                .into_edit_transaction()
                .unwrap(),
        )
        .unwrap_or_else(|e| panic!("{mode} insert {position}: {e}"));
    for resolution in [Resolution::AcceptAll, Resolution::RejectAll] {
        let resolved = xml(&edited.project(resolution).expect("resolve"));
        for (start, end) in [
            ("<w:permStart", "<w:permEnd"),
            ("<w:commentRangeStart", "<w:commentRangeEnd"),
        ] {
            assert_eq!(
                resolved.matches(start).count(),
                resolved.matches(end).count(),
                "{mode} {position}: every range stays a pair: {resolved}"
            );
        }
    }
    xml(&edited)
}

fn xml(doc: &Document) -> String {
    let bytes = doc.serialize(&ExportOptions::default()).expect("save");
    let xml = String::from_utf8(
        stemma::docx::DocxArchive::read(&bytes)
            .unwrap()
            .get("word/document.xml")
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    xml[xml.find("<w:body>").unwrap()..].to_string()
}

fn at(xml: &str, needle: &str) -> usize {
    xml.find(needle)
        .unwrap_or_else(|| panic!("{needle} in {xml}"))
}

#[test]
fn a_paragraph_inserted_beside_a_commented_paragraph_stays_outside_the_comment() {
    let doc = Document::parse(&docx(&body(COMMENTED))).expect("parse");
    for mode in ["tracked_change", "direct"] {
        let after = insert(&doc, "after", mode);
        assert!(
            at(&after, "<w:commentRangeEnd") < at(&after, "New line"),
            "{mode} after: {after}"
        );
        let before = insert(&doc, "before", mode);
        assert!(
            at(&before, "New line") < at(&before, "<w:commentRangeStart"),
            "{mode} before: {before}"
        );
    }
}

#[test]
fn a_paragraph_inserted_beside_a_permission_range_joins_it() {
    for (spelling, marked) in [("block", PERMISSION_BLOCK), ("run", PERMISSION_RUN)] {
        let doc = Document::parse(&docx(&body(marked))).expect("parse");
        for mode in ["tracked_change", "direct"] {
            let after = insert(&doc, "after", mode);
            assert!(
                at(&after, "<w:permStart") < at(&after, "Marked line")
                    && at(&after, "New line") < at(&after, "<w:permEnd"),
                "{spelling} {mode} after: {after}"
            );
            let before = insert(&doc, "before", mode);
            assert!(
                at(&before, "<w:permStart") < at(&before, "New line")
                    && at(&before, "Marked line") < at(&before, "<w:permEnd"),
                "{spelling} {mode} before: {before}"
            );
        }
    }
}
