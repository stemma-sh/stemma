//! Authored-OFF toggles round-trip (§17.7.3 toggle semantics; the last piece
//! of the blindspot-audit H8 "directness gap").
//!
//! DOMAIN RULE: a run whose own rPr carries `<w:b w:val="0"/>` under a bold
//! style authored an OVERRIDE — direct formatting wins outright over the
//! style-layer toggle XOR. Dropping the off-form on re-serialize lets the
//! style's bold bleed back in: a RENDER-VISIBLE loss, worse than churn.
//! `Vec<Mark>` is presence-only, so the serializer reconstructs the off-form
//! from provenance: authored slot + absent resolved mark ⟹ authored OFF.

use std::io::Write;

use stemma::ExportOptions;
use stemma::RevisionInfo;
use stemma::api::Document;
use stemma::edit::{EditStep, EditTransaction, MaterializationMode};
use zip::write::FileOptions;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn make_docx(body_xml: &str, styles_xml: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W_NS}"><w:body>{body_xml}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#;
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
        zip.start_file("word/styles.xml", opts).unwrap();
        zip.write_all(styles_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

fn reserialize_trigger() -> EditTransaction {
    EditTransaction {
        steps: vec![EditStep::SetDocDefaults {
            font_family: Some("Calibri".to_string()),
            font_size_half_points: None,
            rationale: Some("authored-off reserialize trigger".to_string()),
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

/// Bold paragraph style; run A authors `<w:b w:val="0"/>` (must survive), run
/// B authors nothing (must NOT gain a bold or an off-bold — inheritance stays
/// in the style layer).
#[test]
fn authored_off_bold_survives_and_inherited_stays_unmaterialized() {
    let styles = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="{W_NS}"><w:style w:type="paragraph" w:styleId="BoldPara"><w:name w:val="Bold Para"/><w:rPr><w:b/><w:i/></w:rPr></w:style></w:styles>"#
    );
    let body = r#"<w:p><w:pPr><w:pStyle w:val="BoldPara"/></w:pPr><w:r><w:rPr><w:b w:val="0"/><w:i w:val="0"/></w:rPr><w:t>overridden off</w:t></w:r><w:r><w:t>inherits bold</w:t></w:r></w:p>"#;

    let docx = make_docx(body, &styles);
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

    let first_run = &xml[xml.find("<w:r>").unwrap()..];
    let first_run = &first_run[..first_run.find("</w:r>").unwrap()];
    assert!(
        first_run.contains(r#"<w:b w:val="0""#) && first_run.contains(r#"<w:i w:val="0""#),
        "the authored off-toggles must survive (dropping them lets the style's \
         bold/italic bleed back in — render-visible); first run: {first_run}"
    );

    let second_run_start = xml.find("inherits bold").expect("second run text");
    let second_run = &xml[..second_run_start];
    let second_run = &second_run[second_run.rfind("<w:r>").unwrap()..];
    assert!(
        !second_run.contains("<w:b") && !second_run.contains("<w:i"),
        "the inheriting run must not gain materialized toggles (on OR off); \
         second run region: {second_run}"
    );
}
