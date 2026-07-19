//! Literal-prefix separator whitespace is preserved VERBATIM (XML 1.0 §2.10;
//! ECMA-376 §17.3.3.1 w:tab as a real character).
//!
//! DOMAIN RULE: when import strips a literal enumeration label ("2.3", "(a)")
//! from a paragraph, everything it consumed — the label, the surrounding
//! whitespace, and the separator between label and body text — is part of the
//! document's visible character content and must re-emit byte-for-byte. The
//! model captures the separator verbatim (`literal_prefix_trailing_ws`,
//! e.g. "  \t\t"); the serializer must not discretize it to a lone `<w:tab/>`
//! (the old `pending_prefix_tab: bool` did exactly that, losing separator
//! spaces and collapsing multi-tab separators — the "inter-run whitespace
//! loss" class).
//!
//! Edit path exercised deliberately: un-edited export returns original bytes
//! and proves nothing (see roundtrip_fidelity.rs module docs).

use std::io::Write;

use stemma::ExportOptions;
use stemma::RevisionInfo;
use stemma::api::Document;
use stemma::edit::{EditStep, EditTransaction, MaterializationMode};
use zip::write::FileOptions;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn make_docx(body_inner: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W_NS}"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
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
            rationale: Some("separator-verbatim reserialize trigger".to_string()),
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

fn edited_document_xml(docx: &[u8]) -> String {
    let doc = Document::parse(docx).expect("parse");
    let edited = doc.apply(&reserialize_trigger()).expect("apply trigger");
    let out = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let archive = stemma::docx::DocxArchive::read(&out).expect("read docx archive");
    String::from_utf8(
        archive
            .get("word/document.xml")
            .expect("document.xml present")
            .to_vec(),
    )
    .expect("document.xml utf-8")
}

/// "2.3" label, two trailing spaces, then TWO tab elements: the separator
/// "  \t\t" must survive verbatim — spaces kept, both tabs kept.
#[test]
fn multi_tab_separator_with_spaces_survives_verbatim() {
    let body = r#"<w:p><w:r><w:t xml:space="preserve">2.3  </w:t></w:r><w:r><w:tab/><w:tab/><w:t>double tabs</w:t></w:r></w:p>"#;
    let xml = edited_document_xml(&make_docx(body));

    let para = &xml[xml.find("<w:p>").unwrap()..xml.find("</w:p>").unwrap()];
    assert_eq!(
        para.matches("<w:tab />").count() + para.matches("<w:tab/>").count(),
        2,
        "both separator tabs must survive (not collapse to one); paragraph: {para}"
    );
    assert!(
        para.contains(r#"<w:t xml:space="preserve">  </w:t>"#)
            || para.contains(r#"<w:t xml:space="preserve">2.3  </w:t>"#),
        "the two separator spaces after the label must survive; paragraph: {para}"
    );
}

/// Regression guard for the common single-tab shape: "1.1\tHeading" keeps
/// exactly one tab and the heading's internal multi-spaces.
#[test]
fn single_tab_separator_and_internal_spaces_survive() {
    let body =
        r#"<w:p><w:r><w:t xml:space="preserve">1.1&#9;Definitions   and  Terms</w:t></w:r></w:p>"#;
    let xml = edited_document_xml(&make_docx(body));

    let para = &xml[xml.find("<w:p>").unwrap()..xml.find("</w:p>").unwrap()];
    assert_eq!(
        para.matches("<w:tab />").count() + para.matches("<w:tab/>").count(),
        1,
        "exactly one separator tab; paragraph: {para}"
    );
    assert!(
        para.contains("Definitions   and  Terms"),
        "internal multi-spaces in body text must survive; paragraph: {para}"
    );
}

/// The projected text (what accept/reject and text extraction see) must be
/// stable across the edit-path rebuild for separator-bearing paragraphs.
#[test]
fn projected_text_is_stable_for_separator_paragraphs() {
    let body = concat!(
        r#"<w:p><w:r><w:t xml:space="preserve">2.3  </w:t></w:r><w:r><w:tab/><w:tab/><w:t>double tabs</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t xml:space="preserve">   (a) item with leading spaces</w:t></w:r></w:p>"#,
    );
    let docx = make_docx(body);
    let before = Document::parse(&docx).expect("parse").to_text();
    let doc = Document::parse(&docx).expect("parse");
    let edited = doc.apply(&reserialize_trigger()).expect("apply");
    let out = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let after = Document::parse(&out).expect("reparse").to_text();
    assert_eq!(
        before, after,
        "projected text must be identical across the edit-path rebuild"
    );
}
