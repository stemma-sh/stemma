//! Comments are annotations, as in Word (a named departure from the laws).
//!
//! Word's own compare carries comments untracked, and Word does not honour a
//! tracked insertion of a comment anchored on text both documents keep: its
//! Reject All leaves such a comment anchored, emptied of its words. So a
//! comment the target adds on kept text is carried untracked, and both
//! readings keep it with its words. A comment added on inserted text rides
//! inside the insertion and goes with it on Reject All.

use std::io::Write;

use stemma::api::Document;
use stemma::domain::{BlockNode, InlineNode, OpaqueKind, TrackingStatus};
use stemma::{ExportOptions, Resolution};

const COMMENT_TEXT: &str = "Please confirm the delivery window.";

/// A package whose body is `body`, with a comments part holding comment 0
/// when `with_comment`.
fn docx(body: &str, with_comment: bool) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let comments_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:comment w:id="0" w:author="Reviewer" w:date="2026-03-02T09:00:00Z" w:initials="RV"><w:p><w:r><w:t>{COMMENT_TEXT}</w:t></w:r></w:p></w:comment></w:comments>"#
    );
    let comments_override = if with_comment {
        r#"<Override PartName="/word/comments.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"/>"#
    } else {
        ""
    };
    let content_types = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>{comments_override}</Types>"#
    );
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = if with_comment {
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="comments.xml"/></Relationships>"#
    } else {
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"></Relationships>"#
    };
    let mut parts = vec![
        ("[Content_Types].xml", content_types.as_str()),
        ("_rels/.rels", rels),
        ("word/_rels/document.xml.rels", doc_rels),
        ("word/document.xml", document_xml.as_str()),
    ];
    if with_comment {
        parts.push(("word/comments.xml", comments_xml.as_str()));
    }
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = zip::write::FileOptions::default();
        for (name, part) in parts {
            zip.start_file(name, opts).expect("start part");
            zip.write_all(part.as_bytes()).expect("write part");
        }
        zip.finish().expect("finish package");
    }
    buf
}

fn run(text: &str) -> String {
    format!(r#"<w:r><w:t xml:space="preserve">{text}</w:t></w:r>"#)
}

const START: &str = r#"<w:commentRangeStart w:id="0"/>"#;
const END: &str = r#"<w:commentRangeEnd w:id="0"/>"#;
const REFERENCE: &str = r#"<w:r><w:commentReference w:id="0"/></w:r>"#;

fn reopened(document: &Document) -> Document {
    Document::parse(
        &document
            .serialize(&ExportOptions::default())
            .expect("serialize"),
    )
    .expect("reopen")
}

/// A reading: the body text, and each comment as (anchored, its words),
/// where anchored says its reference is in the body.
fn reading(document: &Document) -> (String, Vec<(bool, String)>) {
    let canonical = &document.snapshot().canonical;
    let mut referenced = Vec::new();
    for tracked in &canonical.blocks {
        if let BlockNode::Paragraph(p) = &tracked.block {
            for inline in p.all_inlines() {
                if let InlineNode::OpaqueInline(o) = inline
                    && let OpaqueKind::CommentReference(r) = &o.kind
                {
                    referenced.push(r.reference_id.clone());
                }
            }
        }
    }
    let comments = canonical
        .comments
        .iter()
        .map(|story| {
            let words: Vec<String> = story
                .blocks
                .iter()
                .filter_map(|tb| match &tb.block {
                    BlockNode::Paragraph(p) => Some(stemma::local_change::extract_inline_text(
                        &p.all_inlines_owned(),
                    )),
                    _ => None,
                })
                .collect();
            (referenced.contains(&story.id), words.join("\n"))
        })
        .collect();
    (document.to_text(), comments)
}

fn resolved(document: &Document, resolution: Resolution) -> (String, Vec<(bool, String)>) {
    let resolved = reopened(&document.project(resolution).expect("resolve"));
    assert!(resolved.revisions().is_empty(), "a resolved reading");
    reading(&resolved)
}

/// Whether any of the redline's comment markers sits in a tracked segment.
fn comment_markers_tracked(document: &Document) -> bool {
    document
        .snapshot()
        .canonical
        .blocks
        .iter()
        .filter_map(|tracked| match &tracked.block {
            BlockNode::Paragraph(p) => Some(p),
            _ => None,
        })
        .flat_map(|p| &p.segments)
        .filter(|segment| segment.status != TrackingStatus::Normal)
        .flat_map(|segment| &segment.inlines)
        .any(|inline| match inline {
            InlineNode::CommentRangeStart { .. } | InlineNode::CommentRangeEnd { .. } => true,
            InlineNode::OpaqueInline(o) => matches!(o.kind, OpaqueKind::CommentReference(_)),
            _ => false,
        })
}

/// The comment's first paragraph, plain or with comment 0 on it, then
/// `second`.
fn kept_text_body(annotated: bool, second: &str) -> Vec<u8> {
    let first = run("Deliveries arrive at the north gate.");
    let first = if annotated {
        format!("{START}{first}{END}{REFERENCE}")
    } else {
        first
    };
    docx(
        &format!("<w:p>{first}</w:p><w:p>{}</w:p>", run(second)),
        annotated,
    )
}

/// The target annotates kept text and edits another paragraph: Accept All
/// reads as the target, and Reject All as the base carrying the target's
/// comment, its words included.
#[test]
fn a_comment_added_on_kept_text_is_carried_untracked() {
    let parse = |bytes: Vec<u8>| Document::parse(&bytes).expect("parse");
    let base = parse(kept_text_body(
        false,
        "Invoices are due within thirty days.",
    ));
    let target = parse(kept_text_body(true, "Invoices are due within forty days."));
    let annotated_base = parse(kept_text_body(true, "Invoices are due within thirty days."));
    let redline = reopened(&stemma_diff::diff_as(&base, &target, "compare").expect("compare"));
    assert!(
        !comment_markers_tracked(&redline),
        "the comment's markers are plain"
    );
    assert!(!redline.revisions().is_empty(), "the edit is tracked");
    assert_eq!(resolved(&redline, Resolution::AcceptAll), reading(&target));
    assert_eq!(
        resolved(&redline, Resolution::RejectAll),
        reading(&annotated_base)
    );
    assert_eq!(
        reading(&annotated_base).1,
        vec![(true, COMMENT_TEXT.to_string())]
    );
}

#[test]
fn a_comment_added_on_inserted_text_goes_with_it_on_reject() {
    let base = Document::parse(&docx(
        &format!("<w:p>{}</w:p>", run("Deliveries arrive at the north gate.")),
        false,
    ))
    .expect("parse base");
    let target = Document::parse(&docx(
        &format!(
            "<w:p>{}</w:p><w:p>{START}{}{END}{REFERENCE}</w:p>",
            run("Deliveries arrive at the north gate."),
            run("Pallets over one tonne need a booked slot.")
        ),
        true,
    ))
    .expect("parse target");
    let redline = reopened(&stemma_diff::diff_as(&base, &target, "compare").expect("compare"));
    assert_eq!(resolved(&redline, Resolution::AcceptAll), reading(&target));
    assert_eq!(resolved(&redline, Resolution::RejectAll), reading(&base));
}

/// Whether the saved package's comments part holds a comment.
fn package_holds_a_comment(document: &Document) -> bool {
    use std::io::Read;
    let bytes = document
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("open package");
    let Ok(mut part) = archive.by_name("word/comments.xml") else {
        return false;
    };
    let mut xml = String::new();
    part.read_to_string(&mut xml).expect("read comments part");
    xml.contains("<w:comment ")
}

/// A comment inside a wholly inserted paragraph, between kept ones, is on
/// inserted text: the paragraph is one insertion (its own segments carry no
/// status of their own), and the comment rides inside it, its words
/// recorded as inserted. Reject All removes the paragraph and its comment, leaving no
/// comment in the comments part; Accept All reads as the target.
#[test]
fn a_comment_inside_an_inserted_paragraph_is_inserted_with_it() {
    let kept_first = run("Deliveries arrive at the north gate.");
    let kept_last = run("Invoices are due within thirty days.");
    let base = Document::parse(&docx(
        &format!("<w:p>{kept_first}</w:p><w:p>{kept_last}</w:p>"),
        false,
    ))
    .expect("parse base");
    let target = Document::parse(&docx(
        &format!(
            "<w:p>{kept_first}</w:p><w:p>{START}{}{END}{REFERENCE}</w:p><w:p>{kept_last}</w:p>",
            run("Pallets over one tonne need a booked slot.")
        ),
        true,
    ))
    .expect("parse target");
    let redline = stemma_diff::diff_as(&base, &target, "compare").expect("compare");
    for document in [&redline, &reopened(&redline)] {
        let comment = document
            .snapshot()
            .canonical
            .comments
            .iter()
            .find(|story| story.id == "0")
            .expect("the redline holds the comment");
        assert!(
            comment
                .blocks
                .iter()
                .all(|block| matches!(block.status, TrackingStatus::Inserted(_))),
            "the comment's words are inserted with its paragraph: {:?}",
            comment.blocks.iter().map(|b| &b.status).collect::<Vec<_>>()
        );
        assert_eq!(resolved(document, Resolution::AcceptAll), reading(&target));
        assert_eq!(resolved(document, Resolution::RejectAll), reading(&base));
        let rejected = reopened(&document.project(Resolution::RejectAll).expect("reject"));
        assert!(
            !package_holds_a_comment(&rejected),
            "Reject All leaves no comment in the comments part"
        );
    }
}
