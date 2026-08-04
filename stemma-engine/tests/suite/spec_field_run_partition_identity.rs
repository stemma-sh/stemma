//! Spec-compliance: sibling runs inside one tracked container keep their
//! wire-run identity.
//!
//! A `w:del` (or `w:ins`) container holds a SEQUENCE of runs, each with its
//! own `w:rPr` (§17.13.5.14 — CT_RunTrackChange reuses EG_ContentRunContent).
//! Two sibling runs with different properties are distinct wire state: a bare
//! `separate` fldChar run followed by a field-result run carrying an
//! `w:rStyle` must never be re-emitted as ONE run wearing the result's style —
//! that would apply the character style to the field boundary and, on reopen,
//! read back as different formatting than the source document had.
//!
//! Regression shape (observed on a wild document): rejecting a deleted HYPERLINK
//! field restored the field, and the save merged the bare `separate` run into
//! the styled result run, so `reject → save → reopen` disagreed with
//! `reject` on the wrapper's char style.

use std::io::Write as _;

use stemma::api::Document;
use stemma::domain::{BlockNode, InlineNode};
use stemma::{ExportOptions, Resolution};
use zip::write::FileOptions;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

/// A paragraph holding a DELETED complex field where the `separate` fldChar
/// sits in its own bare run and the cached result run carries an rStyle —
/// exactly Word's shape for a hyperlink field with a styled result.
fn deleted_styled_field_docx() -> Vec<u8> {
    let doc = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W_NS}"><w:body><w:p><w:del w:id="10" w:author="A" w:date="2023-01-26T15:51:00Z"><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:delInstrText xml:space="preserve"> HYPERLINK "https://example.test" \h </w:delInstrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:rPr><w:rStyle w:val="InlineResourceReference"/></w:rPr><w:delText>Result Text</w:delText></w:r><w:r><w:rPr><w:rStyle w:val="InlineResourceReference"/></w:rPr><w:fldChar w:fldCharType="end"/></w:r></w:del></w:p><w:p><w:r><w:t>Body stays.</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#
    );
    let ct = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let options: FileOptions = FileOptions::default();
        zip.start_file("[Content_Types].xml", options).unwrap();
        zip.write_all(ct.as_bytes()).unwrap();
        zip.start_file("_rels/.rels", options).unwrap();
        zip.write_all(rels.as_bytes()).unwrap();
        zip.start_file("word/_rels/document.xml.rels", options)
            .unwrap();
        zip.write_all(br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#).unwrap();
        zip.start_file("word/document.xml", options).unwrap();
        zip.write_all(doc.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

fn field_paragraph_wrapper_styles(doc: &Document) -> Vec<(bool, Option<String>)> {
    let snapshot = doc.snapshot();
    let BlockNode::Paragraph(paragraph) = &snapshot.canonical.blocks[0].block else {
        panic!("first block is the field paragraph");
    };
    let mut out = Vec::new();
    for segment in &paragraph.segments {
        for inline in &segment.inlines {
            if let InlineNode::OpaqueInline(opaque) = inline {
                out.push((
                    opaque.joins_following_text_run,
                    opaque
                        .wrapper_style_props
                        .char_style_id
                        .as_deref()
                        .map(str::to_string),
                ));
            }
        }
    }
    out
}

/// Import must not claim two sibling container runs shared one wire run: the
/// bare `separate` fldChar is its OWN run, so it never joins the styled
/// result text that follows it.
#[test]
fn spec_sibling_container_runs_keep_distinct_run_identity() {
    let doc = Document::parse(&deleted_styled_field_docx()).expect("parse fixture");
    let wrappers = field_paragraph_wrapper_styles(&doc);
    assert_eq!(wrappers.len(), 4, "begin, instr, separate, end widgets");
    for (index, (joins, _)) in wrappers.iter().enumerate() {
        assert!(
            !joins,
            "widget {index} sat in its own wire run and must not join the following text run"
        );
    }
    // The bare runs carry no character style; only the end fldChar's run did.
    assert_eq!(wrappers[2].1, None, "separate fldChar run had no rStyle");
    assert_eq!(
        wrappers[3].1.as_deref(),
        Some("InlineResourceReference"),
        "end fldChar run carried the rStyle on the wire"
    );
}

/// Rejecting the deletion restores the field; saving and reopening that
/// projection must agree with the in-memory projection on every wrapper's
/// character style (the bare `separate` run must not come back styled).
#[test]
fn spec_rejected_field_save_reopen_preserves_wrapper_styles() {
    let doc = Document::parse(&deleted_styled_field_docx()).expect("parse fixture");
    let rejected = doc
        .project(Resolution::RejectAll)
        .expect("project reject-all");
    let in_memory = field_paragraph_wrapper_styles(&rejected);
    let bytes = rejected
        .serialize(&ExportOptions::default())
        .expect("serialize rejected projection");
    let reopened = Document::parse(&bytes).expect("reopen rejected projection");
    let persisted = field_paragraph_wrapper_styles(&reopened);
    assert_eq!(
        in_memory, persisted,
        "wrapper run identity and char styles survive save/reopen of the restored field"
    );
}
