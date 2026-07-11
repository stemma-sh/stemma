//! OOXML rPr child-order spec compliance for the run-formatting remainder
//! (Part A): `w:caps` (§17.3.2.5), `w:smallCaps` (§17.3.2.33), and character
//! spacing `w:spacing` (§17.3.2.35).
//!
//! ECMA-376 Annex A (`CT_RPr`) fixes the child order: ... caps(7) → smallCaps(8)
//! → ... → spacing(20) → ... . The serializer's `build_rpr()` already emits at
//! these positions; this test pins that an authored `SetRunFormatting` carrying
//! all three produces an rPr whose children are in that order, and that the
//! character-spacing value is written verbatim as `w:spacing w:val="<twips>"`.
//!
//! Runs daily (corpus-free, in-memory DOCX).

use stemma::docx::DocxArchive;
use stemma::edit::*;
use stemma::{DocxRuntime, ExportMode, NodeId, RevisionInfo, SimpleRuntime};

/// Minimal one-paragraph DOCX with a single run of `text`.
fn make_docx(text: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;

    use std::io::Write;
    use zip::write::FileOptions;
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

fn export_document_xml(steps: Vec<EditStep>) -> String {
    let runtime = SimpleRuntime::new();
    let import = runtime
        .import_docx(&make_docx("The Confidential term."))
        .expect("import");
    let block_id = {
        let view = runtime.view(&import.doc_handle).expect("view");
        use stemma::BlockNode;
        view.canonical
            .blocks
            .iter()
            .find_map(|tb| match &tb.block {
                BlockNode::Paragraph(p) => Some(p.id.to_string()),
                _ => None,
            })
            .expect("a paragraph block")
    };
    let steps = steps
        .into_iter()
        .map(|s| match s {
            EditStep::SetRunFormatting {
                expect,
                semantic_hash,
                marks,
                style,
                rationale,
                ..
            } => EditStep::SetRunFormatting {
                block_id: NodeId::from(block_id.as_str()),
                expect,
                semantic_hash,
                marks,
                style,
                rationale,
            },
            other => other,
        })
        .collect();
    let txn = EditTransaction {
        steps,
        summary: None,
        // Direct mode keeps the new marks without an rPrChange wrapper, so the
        // emitted rPr is exactly the new run properties — the cleanest surface
        // for an ordering assertion.
        materialization_mode: MaterializationMode::Direct,
        revision: RevisionInfo {
            revision_id: 1,
            author: Some("Spec".to_string()),
            date: Some("2026-06-01T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    };
    runtime.apply_edit(&import.doc_handle, &txn).expect("apply");
    let bytes = runtime
        .export_docx(&import.doc_handle, ExportMode::Redline)
        .expect("export");
    let archive = DocxArchive::read(&bytes).expect("read docx");
    String::from_utf8(
        archive
            .get("word/document.xml")
            .expect("document.xml")
            .to_vec(),
    )
    .expect("utf-8")
}

/// Byte offset of the first occurrence of a tag's local name within an rPr.
fn local_pos(xml: &str, local: &str) -> usize {
    // The serializer emits prefixed tags (`<w:caps`). Match on `:local` so we
    // don't accidentally match a substring of a longer local name.
    xml.find(&format!(":{local}"))
        .unwrap_or_else(|| panic!("element '{local}' not found in: {xml}"))
}

#[test]
fn caps_smallcaps_spacing_emit_in_annex_a_order() {
    let xml = export_document_xml(vec![EditStep::SetRunFormatting {
        block_id: NodeId::from("placeholder"),
        expect: "Confidential".to_string(),
        semantic_hash: None,
        marks: InlineMarkSet {
            caps: true,
            small_caps: true,
            ..Default::default()
        },
        style: RunStyleEdit {
            char_spacing: Some(40),
            ..Default::default()
        },
        rationale: None,
    }]);

    // All three present.
    assert!(xml.contains(":caps"), "w:caps must be emitted");
    assert!(xml.contains(":smallCaps"), "w:smallCaps must be emitted");
    assert!(xml.contains(":spacing"), "w:spacing must be emitted");

    // CT_RPr order: caps(7) < smallCaps(8) < spacing(20).
    let caps = local_pos(&xml, "caps");
    let small = local_pos(&xml, "smallCaps");
    let spacing = local_pos(&xml, "spacing");
    assert!(caps < small, "w:caps must precede w:smallCaps (Annex A)");
    assert!(
        small < spacing,
        "w:smallCaps must precede w:spacing (Annex A)"
    );
}

#[test]
fn char_spacing_writes_twips_to_w_val() {
    let xml = export_document_xml(vec![EditStep::SetRunFormatting {
        block_id: NodeId::from("placeholder"),
        expect: "Confidential".to_string(),
        semantic_hash: None,
        marks: InlineMarkSet::default(),
        style: RunStyleEdit {
            char_spacing: Some(-25),
            ..Default::default()
        },
        rationale: None,
    }]);
    // w:spacing carries the twip value verbatim on @w:val (negative = condensed).
    assert!(
        xml.contains(r#"w:val="-25""#),
        "character spacing must be written as w:spacing w:val=\"-25\"; got: {xml}"
    );
}
