//! A bookmark moved with edited text goes on the target's copy, as in Word's
//! Compare.
//!
//! The base's bookmarked paragraph is deleted in one place and the target's
//! edited copy, carrying the same bookmark, is inserted in another. Word's
//! Compare puts the bookmark on the target's copy: after Accept All every
//! bookmark of the target is there, so a REF field to it still resolves;
//! after Reject All the base's text is back without that one bookmark name,
//! which is what Word's own result gives too.

use std::collections::BTreeSet;
use std::io::{Read, Write};

use stemma::api::Document;
use stemma::{ExportOptions, Resolution};

fn docx(body: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = zip::write::FileOptions::default();
        for (name, part) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/document.xml", document_xml.as_str()),
        ] {
            zip.start_file(name, opts).expect("start part");
            zip.write_all(part.as_bytes()).expect("write part");
        }
        zip.finish().expect("finish package");
    }
    buf
}

fn paragraph(text: &str) -> String {
    format!(r#"<w:p><w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p>"#)
}

fn bookmarked(name: &str, text: &str) -> String {
    format!(
        r#"<w:p><w:bookmarkStart w:id="4" w:name="{name}"/><w:r><w:t xml:space="preserve">{text}</w:t></w:r><w:bookmarkEnd w:id="4"/></w:p>"#
    )
}

/// A paragraph holding a REF field to `name`.
fn reference(name: &str) -> String {
    format!(
        r#"<w:p><w:r><w:t xml:space="preserve">See </w:t></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> REF {name} \h </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>the confidentiality clause</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:t>.</w:t></w:r></w:p>"#
    )
}

/// The saved main document part.
fn document_xml(document: &Document) -> String {
    let bytes = document
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("open package");
    let mut xml = String::new();
    archive
        .by_name("word/document.xml")
        .expect("main part")
        .read_to_string(&mut xml)
        .expect("read main part");
    xml
}

/// Names of the bookmarks in the saved main document part.
fn bookmark_names(document: &Document) -> BTreeSet<String> {
    let xml = document_xml(document);
    xml.split("<w:bookmarkStart")
        .skip(1)
        .filter_map(|tail| {
            let tail = &tail[..tail.find('>')?];
            let at = tail.find("w:name=\"")? + "w:name=\"".len();
            Some(tail[at..at + tail[at..].find('"')?].to_string())
        })
        .collect()
}

/// Names that a REF field in the saved main document part refers to.
fn referenced_names(document: &Document) -> BTreeSet<String> {
    document_xml(document)
        .split(" REF ")
        .skip(1)
        .filter_map(|tail| tail.split_whitespace().next().map(str::to_string))
        .collect()
}

fn reopened(document: &Document) -> Document {
    Document::parse(
        &document
            .serialize(&ExportOptions::default())
            .expect("serialize"),
    )
    .expect("reopen")
}

const CLAUSE: &str = "Each party shall keep the terms of this agreement confidential.";
const EDITED: &str = "Each party must keep the terms of this agreement strictly confidential.";
const DELIVERY: &str = "The supplier delivers the goods to the north gate.";
const PAYMENT: &str = "Invoices are payable within thirty days.";

#[test]
fn a_bookmark_moved_with_edited_text_is_on_the_target_copy() {
    let base = Document::parse(&docx(&format!(
        "{}{}{}{}",
        bookmarked("Confidentiality", CLAUSE),
        paragraph(DELIVERY),
        paragraph(PAYMENT),
        reference("Confidentiality")
    )))
    .expect("parse base");
    let target = Document::parse(&docx(&format!(
        "{}{}{}{}",
        paragraph(DELIVERY),
        paragraph(PAYMENT),
        bookmarked("Confidentiality", EDITED),
        reference("Confidentiality")
    )))
    .expect("parse target");

    let redline = reopened(&stemma_diff::diff_as(&base, &target, "compare").expect("compare"));
    let accepted = reopened(&redline.project(Resolution::AcceptAll).expect("accept"));
    let rejected = reopened(&redline.project(Resolution::RejectAll).expect("reject"));

    assert_eq!(
        accepted.to_text(),
        target.to_text(),
        "Accept All reads as the target"
    );
    assert_eq!(
        rejected.to_text(),
        base.to_text(),
        "Reject All reads as the base"
    );
    assert_eq!(
        bookmark_names(&accepted),
        bookmark_names(&target),
        "Accept All holds every bookmark of the target"
    );
    assert!(
        referenced_names(&accepted).is_subset(&bookmark_names(&accepted)),
        "every REF field still has its bookmark after Accept All"
    );
    assert_eq!(
        bookmark_names(&rejected),
        BTreeSet::new(),
        "Reject All keeps the base's text without the moved bookmark, as Word's result does"
    );
}

/// A bookmark on text both documents keep stays where it is.
#[test]
fn a_bookmark_on_kept_text_is_kept_in_both_readings() {
    let base = Document::parse(&docx(&format!(
        "{}{}",
        bookmarked("Confidentiality", CLAUSE),
        paragraph(PAYMENT)
    )))
    .expect("parse base");
    let target = Document::parse(&docx(&format!(
        "{}{}",
        bookmarked("Confidentiality", CLAUSE),
        paragraph("Invoices are payable within forty days.")
    )))
    .expect("parse target");

    let redline = reopened(&stemma_diff::diff_as(&base, &target, "compare").expect("compare"));
    for resolution in [Resolution::AcceptAll, Resolution::RejectAll] {
        let reading = reopened(&redline.project(resolution).expect("resolve"));
        assert_eq!(
            bookmark_names(&reading),
            BTreeSet::from(["Confidentiality".to_string()])
        );
    }
}
