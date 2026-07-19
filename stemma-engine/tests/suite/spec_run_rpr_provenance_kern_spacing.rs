//! Run-rPr provenance for `w:kern` / `w:spacing` (ISO 29500-1 §17.3.2.19a,
//! §17.3.2.35; docDefaults cascade §17.7.5.1).
//!
//! DOMAIN RULE: a run's direct `w:rPr` states what the run AUTHORED; values the
//! run merely inherits (docDefaults / styles) are resolved at consumption time,
//! not baked into markup. The serializer must therefore emit `w:kern` /
//! `w:spacing` only on runs whose own rPr carried them. Re-emitting an
//! inherited kerning threshold or character spacing as direct rPr on every run
//! is render-neutral (baked value == inherited value; neither prop has a
//! theme-vs-literal precedence inversion) but rewrites markup provenance across
//! the whole body — the "inherited-value-as-direct" churn class that
//! `RunRprAuthored` per-slot provenance exists to prevent.
//!
//! The edit path is exercised deliberately: an un-edited export returns the
//! original scaffold bytes and proves nothing (see roundtrip_fidelity.rs module
//! docs); `SetDocDefaults` forces the full-body `serialize_canonical_docx` pass
//! where the baking would occur.

use std::io::Write;

use stemma::ExportOptions;
use stemma::RevisionInfo;
use stemma::api::Document;
use stemma::edit::{EditStep, EditTransaction, MaterializationMode};
use zip::write::FileOptions;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

/// Minimal valid .docx with a styles part (mirrors
/// spec_page_geometry_pgsz_pgmar_word_compliance.rs).
fn make_docx(body_xml: &str, styles_xml: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W_NS}"><w:body>{body_xml}</w:body></w:document>"#
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

/// Styles-only edit forcing the full-body reserialize (mirrors
/// roundtrip_fidelity.rs::reserialize_trigger).
fn reserialize_trigger() -> EditTransaction {
    EditTransaction {
        steps: vec![EditStep::SetDocDefaults {
            font_family: Some("Calibri".to_string()),
            font_size_half_points: None,
            rationale: Some("kern/spacing provenance reserialize trigger".to_string()),
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

fn part_of(docx: &[u8], part: &str) -> String {
    let archive = stemma::docx::DocxArchive::read(docx).expect("read docx archive");
    let bytes = archive
        .get(part)
        .unwrap_or_else(|| panic!("{part} present"));
    String::from_utf8(bytes.to_vec()).expect("part utf-8")
}

/// docDefaults carry kern=32 / spacing=10; one run authors nothing, one run
/// authors kern=28 / spacing=20. After an edit-path reserialize, only the
/// authored values may appear as direct rPr in document.xml.
#[test]
fn inherited_kern_and_spacing_are_not_materialized_onto_runs() {
    let styles = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="{W_NS}"><w:docDefaults><w:rPrDefault><w:rPr><w:kern w:val="32"/><w:spacing w:val="10"/></w:rPr></w:rPrDefault></w:docDefaults></w:styles>"#
    );
    let body = r#"<w:p><w:r><w:t>plain run inherits kern and spacing</w:t></w:r></w:p><w:p><w:r><w:rPr><w:spacing w:val="20"/><w:kern w:val="28"/></w:rPr><w:t>authored run keeps its own</w:t></w:r></w:p><w:sectPr/>"#;

    let docx = make_docx(body, &styles);
    let doc = Document::parse(&docx).expect("parse");
    let edited = doc.apply(&reserialize_trigger()).expect("apply trigger");
    let out = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");

    let document_xml = part_of(&out, "word/document.xml");

    // The authored run keeps exactly its own values… (no closing bracket in the
    // needle: the writer's self-close spacing is not part of the contract)
    assert!(
        document_xml.contains(r#"<w:kern w:val="28""#),
        "authored w:kern val=28 must survive the reserialize; document.xml: {document_xml}"
    );
    assert!(
        document_xml.contains(r#"<w:spacing w:val="20""#),
        "authored w:spacing val=20 must survive the reserialize; document.xml: {document_xml}"
    );
    // …and no run gains an inherited value as direct rPr. With one authoring
    // run in the fixture, exactly one w:kern / one w:spacing may exist in the
    // body. (The fixture has no pPr spacing, so every w:spacing is run-level.)
    let kern_count = document_xml.matches("<w:kern ").count();
    let spacing_count = document_xml.matches("<w:spacing ").count();
    assert_eq!(
        kern_count, 1,
        "inherited docDefaults kern must not be materialized onto runs \
         (inherited-value-as-direct churn); document.xml: {document_xml}"
    );
    assert_eq!(
        spacing_count, 1,
        "inherited docDefaults spacing must not be materialized onto runs \
         (inherited-value-as-direct churn); document.xml: {document_xml}"
    );

    // The inherited values still live where they were authored: docDefaults.
    let styles_xml = part_of(&out, "word/styles.xml");
    assert!(
        styles_xml.contains(r#"<w:kern w:val="32""#),
        "docDefaults kern must survive a SetDocDefaults(font) edit; styles.xml: {styles_xml}"
    );
    assert!(
        styles_xml.contains(r#"<w:spacing w:val="10""#),
        "docDefaults spacing must survive a SetDocDefaults(font) edit; styles.xml: {styles_xml}"
    );
}
