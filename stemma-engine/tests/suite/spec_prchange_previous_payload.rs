//! Previous-properties payload fidelity for `w:rPrChange` and `w:pPrChange`.
//!
//! A property-change record is reversible history: its child properties are
//! the complete prior state. Importing a document, editing an unrelated run,
//! and serializing the whole body must not filter that history through the
//! current property's provenance.

use std::io::{Cursor, Read, Write};

use stemma::api::Document;
use stemma::edit::{EditStep, EditTransaction, InlineMarkSet, MaterializationMode, RunStyleEdit};
use stemma::{Alignment, ExportOptions, Mark, Resolution, RevisionInfo};
use zip::ZipWriter;
use zip::write::FileOptions;

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

const DOCUMENT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;

fn synthetic_docx() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:r>
        <w:rPr>
          <w:rPrChange w:id="41" w:author="Prior Author" w:date="2026-07-01T00:00:00Z">
            <w:rPr><w:b/><w:color w:val="112233"/></w:rPr>
          </w:rPrChange>
        </w:rPr>
        <w:t>Run changed away from bold color.</w:t>
      </w:r>
    </w:p>
    <w:p>
      <w:pPr>
        <w:jc w:val="center"/>
        <w:pPrChange w:id="42" w:author="Prior Author" w:date="2026-07-01T00:00:00Z">
          <w:pPr><w:spacing w:before="240"/><w:jc w:val="right"/></w:pPr>
        </w:pPrChange>
      </w:pPr>
      <w:r><w:t>Paragraph changed away from right alignment.</w:t></w:r>
    </w:p>
    <w:p><w:r><w:t>Unrelated edit target.</w:t></w:r></w:p>
    <w:sectPr/>
  </w:body>
</w:document>"#;

    let cursor = Cursor::new(Vec::new());
    let mut zip = ZipWriter::new(cursor);
    let options = FileOptions::default();
    for (path, bytes) in [
        ("[Content_Types].xml", CONTENT_TYPES.as_bytes()),
        ("_rels/.rels", ROOT_RELS.as_bytes()),
        ("word/_rels/document.xml.rels", DOCUMENT_RELS.as_bytes()),
        ("word/document.xml", document_xml.as_bytes()),
    ] {
        zip.start_file(path, options).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn unrelated_edit(doc: &Document) -> Document {
    let target = doc.read().blocks[2].id.clone();
    doc.apply(&EditTransaction {
        steps: vec![EditStep::SetRunFormatting {
            block_id: target,
            expect: "Unrelated edit target".to_string(),
            semantic_hash: None,
            marks: InlineMarkSet {
                italic: true,
                ..InlineMarkSet::default()
            },
            style: RunStyleEdit::default(),
            rationale: Some("force full body reserialization".to_string()),
        }],
        summary: Some("unrelated edit".to_string()),
        materialization_mode: MaterializationMode::Direct,
        revision: RevisionInfo {
            revision_id: 100,
            identity: 0,
            author: Some("Current Author".to_string()),
            date: Some("2026-07-16T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    })
    .expect("apply unrelated edit")
}

fn serialize(doc: &Document) -> Vec<u8> {
    doc.serialize(&ExportOptions::default()).expect("serialize")
}

fn document_xml(docx: &[u8]) -> String {
    let mut zip = zip::ZipArchive::new(Cursor::new(docx)).expect("zip");
    let mut part = zip.by_name("word/document.xml").expect("document.xml");
    let mut xml = String::new();
    part.read_to_string(&mut xml).expect("utf-8 document.xml");
    xml
}

#[test]
fn rpr_change_nonempty_previous_payload_survives_roundtrip() {
    let original = Document::parse(&synthetic_docx()).expect("parse synthetic docx");
    let bytes = serialize(&unrelated_edit(&original));
    let reopened = Document::parse(&bytes).expect("reimport serialized docx");

    let snapshot = reopened.snapshot();
    let canonical = &snapshot.canonical;
    let paragraph = match &canonical.blocks[0].block {
        stemma::BlockNode::Paragraph(paragraph) => paragraph,
        other => panic!("first block should be a paragraph, got {other:?}"),
    };
    let run = paragraph
        .all_inlines()
        .find_map(|inline| match inline {
            stemma::InlineNode::Text(text) => Some(text),
            _ => None,
        })
        .expect("text run");
    let change = run.formatting_change.as_ref().expect("rPrChange");
    assert!(change.previous_marks.contains(&Mark::Bold));
    assert_eq!(change.previous_style_props.color.as_deref(), Some("112233"));
}

#[test]
fn ppr_change_nonempty_previous_payload_survives_roundtrip() {
    let original = Document::parse(&synthetic_docx()).expect("parse synthetic docx");
    let bytes = serialize(&unrelated_edit(&original));
    let reopened = Document::parse(&bytes).expect("reimport serialized docx");

    let snapshot = reopened.snapshot();
    let canonical = &snapshot.canonical;
    let paragraph = match &canonical.blocks[1].block {
        stemma::BlockNode::Paragraph(paragraph) => paragraph,
        other => panic!("second block should be a paragraph, got {other:?}"),
    };
    let change = paragraph.formatting_change.as_ref().expect("pPrChange");
    assert_eq!(change.previous_alignment, Some(Alignment::Right));
    assert_eq!(
        change
            .previous_spacing
            .as_ref()
            .and_then(|spacing| spacing.before),
        Some(240)
    );
}

#[test]
fn rejecting_property_changes_restores_previous_payloads() {
    let original = Document::parse(&synthetic_docx()).expect("parse synthetic docx");
    let rejected = original
        .project(Resolution::RejectAll)
        .expect("reject property changes");
    let xml = document_xml(&serialize(&rejected));

    assert!(!xml.contains("rPrChange") && !xml.contains("pPrChange"));
    assert!(xml.contains("<w:b"));
    assert!(xml.contains(r#"w:color w:val="112233""#));
    assert!(xml.contains(r#"w:spacing w:before="240""#));
    assert!(xml.contains(r#"w:jc w:val="right""#));
}
