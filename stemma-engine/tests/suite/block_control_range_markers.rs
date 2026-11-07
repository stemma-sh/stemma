//! Range markers between the blocks of a body content control.
//!
//! Domain rule: a bookmark, comment-range or permission boundary can sit
//! directly inside a control's `w:sdtContent`, between its paragraphs. Word
//! itself writes them there (a start at the control's first position, an end
//! between two of its paragraphs), so such a control is an ordinary open
//! control: its paragraphs are document content an agent reads and edits, and
//! the markers keep their place. One stray marker used to seal the whole
//! control, hiding every paragraph in it.

use stemma::api::Document;
use stemma::domain::BlockNode;
use stemma::{ExportOptions, Resolution};

fn docx(body: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    use std::io::Write;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions = zip::write::FileOptions::default();
        for (name, data) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/document.xml", document_xml.as_str()),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

/// A control holding two paragraphs and the end of a bookmark that starts
/// before it, as in a court ruling whose bookmark spans the whole control.
const END_INSIDE: &str = r#"<w:bookmarkStart w:id="7" w:name="Ruling"/><w:sdt><w:sdtPr><w:alias w:val="Ruling"/></w:sdtPr><w:sdtContent><w:p><w:r><w:t>First paragraph.</w:t></w:r></w:p><w:p><w:r><w:t>Second paragraph.</w:t></w:r></w:p><w:bookmarkEnd w:id="7"/></w:sdtContent></w:sdt><w:p><w:r><w:t>After.</w:t></w:r></w:p>"#;

/// Markers where Word keeps them inside a control: a start before its first
/// paragraph, and a whole bookmark between two paragraphs.
const MARKERS_BETWEEN: &str = r#"<w:sdt><w:sdtPr><w:alias w:val="Ruling"/></w:sdtPr><w:sdtContent><w:bookmarkStart w:id="3" w:name="Head"/><w:p><w:r><w:t>First paragraph.</w:t></w:r></w:p><w:bookmarkStart w:id="5" w:name="Mid"/><w:p><w:r><w:t>Second paragraph.</w:t></w:r></w:p><w:bookmarkEnd w:id="5"/><w:p><w:r><w:t>Third paragraph.</w:t></w:r></w:p></w:sdtContent></w:sdt><w:bookmarkEnd w:id="3"/><w:p><w:r><w:t>After.</w:t></w:r></w:p>"#;

fn paragraph_id(doc: &Document, text: &str) -> String {
    doc.read()
        .blocks
        .iter()
        .find(|block| block.text.contains(text))
        .unwrap_or_else(|| panic!("{text:?} is readable document content"))
        .id
        .to_string()
}

fn document_xml(doc: &Document) -> String {
    let bytes = doc
        .serialize(&ExportOptions::default())
        .expect("saves as a valid package");
    String::from_utf8(
        stemma::docx::DocxArchive::read(&bytes)
            .expect("read")
            .get("word/document.xml")
            .expect("document part")
            .to_vec(),
    )
    .expect("utf-8")
}

/// The part of `xml` inside the (first) control's content.
fn control_content(xml: &str) -> &str {
    let start = xml.find("<w:sdtContent>").expect("the control is written") + 14;
    let end = xml.find("</w:sdtContent>").expect("the control is closed");
    &xml[start..end]
}

/// The text of the `w:t` runs in `xml`, joined.
fn visible_text(xml: &str) -> String {
    xml.split("<w:t")
        .skip(1)
        .filter_map(|piece| {
            let start = piece.find('>')? + 1;
            let end = piece.find("</w:t>")?;
            Some(&piece[start..end])
        })
        .collect()
}

fn reopens_as_itself(doc: &Document, label: &str) {
    let reopened = Document::parse(&doc.serialize(&ExportOptions::default()).expect("serialize"))
        .expect("reopen");
    let differences = stemma::roundtrip_compare::compare_canon_docs(
        &doc.snapshot().canonical,
        &reopened.snapshot().canonical,
    );
    assert!(
        differences.is_empty(),
        "{label}: {:?}",
        differences
            .iter()
            .take(3)
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_controls_paragraphs_beside_a_range_marker_are_document_content() {
    for (label, body) in [
        ("end inside", END_INSIDE),
        ("markers between", MARKERS_BETWEEN),
    ] {
        let doc = Document::parse(&docx(body)).expect("parse");
        // The paragraphs read as blocks, not as one sealed opaque.
        assert!(
            doc.snapshot()
                .canonical
                .blocks
                .iter()
                .any(|tracked| tracked.block_sdt_wrap.is_some()),
            "{label}: the control is open"
        );
        for text in ["First paragraph.", "Second paragraph."] {
            paragraph_id(&doc, text);
        }
        assert!(
            doc.snapshot()
                .canonical
                .blocks
                .iter()
                .all(|tracked| !matches!(&tracked.block, BlockNode::OpaqueBlock(o) if o.range_marker.is_none())),
            "{label}: nothing in it is sealed"
        );
        reopens_as_itself(&doc, label);
    }
}

#[test]
fn the_markers_stay_where_they_were() {
    let doc = Document::parse(&docx(MARKERS_BETWEEN)).expect("parse");
    let xml = document_xml(&doc);
    let inside = control_content(&xml);
    let head = inside
        .find(r#"w:name="Head""#)
        .expect("the start stays inside");
    let first = inside.find("First paragraph.").expect("first paragraph");
    let mid = inside
        .find(r#"w:name="Mid""#)
        .expect("the mid start stays inside");
    let second = inside.find("Second paragraph.").expect("second paragraph");
    let third = inside.find("Third paragraph.").expect("third paragraph");
    assert!(
        head < first && first < mid && mid < second && second < third,
        "{inside}"
    );
    assert_eq!(inside.matches("<w:bookmarkEnd").count(), 1, "{inside}");
    // The end of the bookmark that spans the whole control stays after it.
    assert!(
        xml.find("</w:sdtContent>") < xml.rfind("<w:bookmarkEnd"),
        "{xml}"
    );
}

#[test]
fn a_tracked_edit_inside_the_control_resolves_and_keeps_its_markers() {
    let doc = Document::parse(&docx(END_INSIDE)).expect("parse");
    let second = paragraph_id(&doc, "Second paragraph.");
    let edit = format!(
        r#"{{"ops":[{{"op":"replace","target":"{second}","expect":"Second paragraph.","content":{{"type":"paragraph","content":[{{"type":"text","text":"Second paragraph, revised."}}]}}}}],"revision":{{"author":"Counsel"}}}}"#
    );
    let edited = doc
        .apply(
            &stemma::edit_v4::parse_transaction(&edit)
                .expect("parse edit")
                .into_edit_transaction()
                .expect("adapt edit"),
        )
        .expect("the paragraph is editable");
    reopens_as_itself(&edited, "edited");
    for (resolution, text) in [
        (Resolution::AcceptAll, "Second paragraph, revised."),
        (Resolution::RejectAll, "Second paragraph."),
    ] {
        let projected = edited.project(resolution).expect("project");
        let xml = document_xml(&projected);
        let inside = control_content(&xml);
        assert!(visible_text(inside).contains(text), "{inside}");
        let last_paragraph_end = inside.rfind("</w:p>").expect("paragraphs");
        let marker = inside
            .rfind("<w:bookmarkEnd")
            .expect("the marker stays inside");
        assert!(
            last_paragraph_end < marker && inside[marker..].contains(r#"w:id="7""#),
            "the bookmark still ends after the control's last paragraph: {inside}"
        );
        reopens_as_itself(&projected, "projected");
    }
}

#[test]
fn deleting_the_paragraph_before_an_end_marker_keeps_the_marker_in_the_control() {
    let doc = Document::parse(&docx(END_INSIDE)).expect("parse");
    let second = paragraph_id(&doc, "Second paragraph.");
    let edit = format!(
        r#"{{"ops":[{{"op":"delete","target":"{second}","expect":"Second paragraph."}}],"revision":{{"author":"Counsel"}}}}"#
    );
    let edited = doc
        .apply(
            &stemma::edit_v4::parse_transaction(&edit)
                .expect("parse edit")
                .into_edit_transaction()
                .expect("adapt edit"),
        )
        .expect("delete");
    reopens_as_itself(&edited, "deleted");
    let rejected = edited.project(Resolution::RejectAll).expect("reject");
    assert!(visible_text(control_content(&document_xml(&rejected))).contains("Second paragraph."));
    for projected in [&edited, &rejected] {
        let xml = document_xml(projected);
        assert_eq!(
            control_content(&xml).matches("<w:bookmarkEnd").count(),
            1,
            "{xml}"
        );
    }
}
