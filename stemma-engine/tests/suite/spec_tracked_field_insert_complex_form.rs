//! A tracked field INSERT lowers to the complex form inside `w:ins`.
//!
//! `w:fldSimple` legally cannot ride inside `w:ins` (EG_PContent is
//! paragraph-level only), and the old emission — field direct on the paragraph
//! with only the result run tracked — read to Word as PERMANENT content:
//! with no cached result there was no revision at all, and reject-all left
//! the field in the document (verified against live Word). Word's own writer
//! lowers tracked field inserts to
//! `w:ins > w:r(fldChar begin) + w:r(instrText) + w:r(fldChar end)`.

use std::io::Write;

use stemma::ExportOptions;
use stemma::api::Document;
use stemma::domain::{FormatSwitches, RefFieldSpec, RefKind};
use stemma::edit::*;
use stemma::{Resolution, RevisionInfo};
use zip::write::FileOptions;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn make_docx() -> Vec<u8> {
    let doc = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W_NS}"><w:body><w:p><w:r><w:t>See the Definitions section for details.</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#
    );
    let ct = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let o: FileOptions = FileOptions::default();
        zip.start_file("[Content_Types].xml", o).unwrap();
        zip.write_all(ct.as_bytes()).unwrap();
        zip.start_file("_rels/.rels", o).unwrap();
        zip.write_all(rels.as_bytes()).unwrap();
        zip.start_file("word/_rels/document.xml.rels", o).unwrap();
        zip.write_all(br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#).unwrap();
        zip.start_file("word/document.xml", o).unwrap();
        zip.write_all(doc.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

fn crossref_txn(block_id: stemma::domain::NodeId) -> EditTransaction {
    EditTransaction {
        steps: vec![EditStep::InsertCrossReference {
            block_id,
            expect: "Definitions".to_string(),
            semantic_hash: None,
            spec: RefFieldSpec {
                kind: RefKind::Ref,
                bookmark: "Definitions".to_string(),
                insert_hyperlink: true,
                no_paragraph_number: false,
                paragraph_number_relative: false,
                paragraph_number_full: false,
                suppress_non_delimiter: false,
                above_below: false,
                format: FormatSwitches::default(),
            },
            rationale: None,
        }],
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("fid".into()),
            date: Some("2026-07-02T00:00:00Z".into()),
            apply_op_id: None,
        },
        summary: None,
    }
}

fn body_paragraph(out: &[u8]) -> String {
    let a = stemma::docx::DocxArchive::read(out).expect("archive");
    let xml = String::from_utf8(a.get("word/document.xml").unwrap().to_vec()).unwrap();
    let i = xml.find("<w:p>").unwrap();
    let j = xml.find("</w:p>").unwrap();
    xml[i..j + 6].to_string()
}

fn edited() -> stemma::api::Document {
    let base = make_docx();
    let doc = Document::parse(&base).expect("parse");
    let id = doc.read().blocks[0].id.clone();
    doc.apply(&crossref_txn(id)).expect("apply")
}

#[test]
fn tracked_field_insert_lowers_to_complex_form_inside_ins() {
    let out = edited()
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let para = body_paragraph(&out);

    // The whole field rides inside ONE w:ins as complex-field runs.
    let ins_start = para.find("<w:ins ").expect("w:ins present");
    let ins_end = para[ins_start..].find("</w:ins>").expect("closed") + ins_start;
    let ins = &para[ins_start..ins_end];
    assert!(
        ins.contains(r#"<w:fldChar w:fldCharType="begin""#)
            && ins.contains("<w:instrText")
            && ins.contains("REF Definitions")
            && ins.contains(r#"<w:fldChar w:fldCharType="end""#),
        "the tracked field must be complex-form runs inside w:ins; ins: {ins}"
    );
    assert!(
        !para.contains("<w:fldSimple"),
        "no fldSimple — it cannot ride inside w:ins and reads as permanent \
         content outside it; paragraph: {para}"
    );
}

/// stemma's own resolution semantics survive the lowering: reject drops the
/// field entirely; accept keeps it (as a normal, untracked field).
#[test]
fn tracked_field_insert_accept_reject() {
    let doc = edited();

    let rejected = doc
        .project(Resolution::RejectAll)
        .expect("reject")
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let para = body_paragraph(&rejected);
    assert!(
        !para.contains("fldChar") && !para.contains("fldSimple") && !para.contains("REF "),
        "reject removes the inserted field entirely; paragraph: {para}"
    );

    let accepted = doc
        .project(Resolution::AcceptAll)
        .expect("accept")
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let para = body_paragraph(&accepted);
    assert!(
        para.contains("REF Definitions") && !para.contains("<w:ins"),
        "accept keeps the field, untracked; paragraph: {para}"
    );
}
