//! Whitespace-only text nodes inside OPAQUE content survive the edit-path
//! rebuild (found by untouched_block_fidelity.rs on image-math-combined).
//!
//! DOMAIN RULE: opaque constructs (OMML math, drawings, fields) round-trip
//! byte-faithfully — that is the opaque contract. A whitespace-only
//! `<m:t xml:space="preserve">  </m:t>` between math runs is VISIBLE CHARACTER
//! CONTENT (XML 1.0 §2.10); re-emitting it as an empty self-closing `<m:t/>`
//! silently deletes rendered characters from content the engine promised not
//! to touch. Root cause was the raw-fragment re-parse using a parser config
//! that drops whitespace-only text nodes.

use std::io::Write;

use stemma::ExportOptions;
use stemma::RevisionInfo;
use stemma::api::Document;
use stemma::edit::{EditStep, EditTransaction, MaterializationMode};
use zip::write::FileOptions;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const M_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/math";

fn make_docx(body_inner: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W_NS}" xmlns:m="{M_NS}"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        zip.start_file("[Content_Types].xml", opts).unwrap();
        zip.write_all(content_types.as_bytes()).unwrap();
        zip.start_file("_rels/.rels", opts).unwrap();
        zip.write_all(rels.as_bytes()).unwrap();
        zip.start_file("word/_rels/document.xml.rels", opts)
            .unwrap();
        zip.write_all(doc_rels.as_bytes()).unwrap();
        zip.start_file("word/document.xml", opts).unwrap();
        zip.write_all(document_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

fn reserialize_trigger() -> EditTransaction {
    EditTransaction {
        steps: vec![EditStep::SetDocDefaults {
            font_family: Some("Calibri".to_string()),
            font_size_half_points: None,
            rationale: Some("opaque-whitespace reserialize trigger".to_string()),
        }],
        materialization_mode: MaterializationMode::Direct,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("fid".into()),
            date: Some("2026-07-02T00:00:00Z".into()),
            apply_op_id: None,
        },
        summary: Some("reserialize trigger".to_string()),
    }
}

/// An inline OMML run whose middle math-run is a whitespace-only
/// `<m:t xml:space="preserve">  </m:t>` must keep those two spaces through the
/// full-body rebuild.
#[test]
fn whitespace_only_math_text_survives_edit_rebuild() {
    let body = r#"<w:p><w:r><w:t>Equation: </w:t></w:r><m:oMath><m:r><m:t>+…,</m:t></m:r><m:r><m:t xml:space="preserve">  </m:t></m:r><m:r><m:t>-x</m:t></m:r></m:oMath></w:p>"#;
    let docx = make_docx(body);

    let doc = Document::parse(&docx).expect("parse");
    let edited = doc.apply(&reserialize_trigger()).expect("apply trigger");
    let out = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");

    let archive = stemma::docx::DocxArchive::read(&out).expect("read output");
    let xml = String::from_utf8(
        archive
            .get("word/document.xml")
            .expect("document.xml")
            .to_vec(),
    )
    .expect("utf-8");

    assert!(
        xml.contains(r#"<m:t xml:space="preserve">  </m:t>"#),
        "whitespace-only m:t inside opaque math must survive verbatim \
         (opaque contract; a self-closing <m:t /> deletes rendered spaces); \
         document.xml: {xml}"
    );
}
