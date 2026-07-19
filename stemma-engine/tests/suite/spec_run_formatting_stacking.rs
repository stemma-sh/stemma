//! B6: stacking two tracked formatting changes on the same run.
//!
//! Before the fix, a SECOND value-format on a run that already carried a tracked
//! formatting change (e.g. "make it bigger" then "recolor it") was rejected with
//! UnsupportedParagraphStructure. Now apply_marks merges the new properties onto
//! the live run while keeping the FIRST change's snapshot as the reject-all
//! baseline, so the two compose into one w:rPrChange.
//!
//! The load-bearing post-condition (per house style: assert the domain rule, not
//! the current output): reject-all of a stacked format must restore the ORIGINAL
//! run, not the intermediate (first-format) state. This is the test the trace's
//! rejected "snapshot the current live state" approach would have failed.
//!
//! Daily tier, corpus-free.

use stemma::api::Document;
use stemma::domain::{BlockNode, CanonDoc, InlineNode, Mark, NodeId, RevisionInfo, TextNode};
use stemma::edit::{
    EditStep, EditTransaction, InlineMarkSet, MaterializationMode, RunStyleEdit, apply_transaction,
};
use stemma::{accept_all, reject_all_with_styles};

fn make_test_docx(paragraphs: &[&str]) -> Vec<u8> {
    let mut document_xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>"#,
    );
    for para in paragraphs {
        document_xml.push_str(&format!(r#"<w:p><w:r><w:t>{para}</w:t></w:r></w:p>"#));
    }
    document_xml.push_str("<w:sectPr/></w:body></w:document>");
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

fn doc_and_ids(paragraphs: &[&str]) -> (CanonDoc, Vec<String>) {
    let doc = Document::parse(&make_test_docx(paragraphs)).expect("parse");
    let ids = doc.read().blocks.iter().map(|b| b.id.to_string()).collect();
    ((*doc.snapshot().canonical).clone(), ids)
}

fn txn(steps: Vec<EditStep>) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("Reviewer".to_string()),
            date: Some("2026-06-29T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

fn fmt_step(id: &str, marks: InlineMarkSet, style: RunStyleEdit) -> EditStep {
    EditStep::SetRunFormatting {
        block_id: NodeId::from(id),
        expect: "Format".to_string(),
        semantic_hash: None,
        marks,
        style,
        rationale: None,
    }
}

/// The TextNode whose text contains `needle`, if any.
fn run_containing(canon: &CanonDoc, needle: &str) -> Option<TextNode> {
    for tb in &canon.blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::Text(t) = inline
                        && t.text.contains(needle)
                    {
                        return Some((**t).clone());
                    }
                }
            }
        }
    }
    None
}

fn any_bold_or_color(canon: &CanonDoc) -> bool {
    for tb in &canon.blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::Text(t) = inline
                        && (t.marks.contains(&Mark::Bold) || t.style_props.color.is_some())
                    {
                        return true;
                    }
                }
            }
        }
    }
    false
}

#[test]
fn stack_two_tracked_formats_then_reject_restores_original() {
    let (base, ids) = doc_and_ids(&["Format me"]);
    let id = ids[0].clone();
    let bold = InlineMarkSet {
        bold: true,
        ..InlineMarkSet::default()
    };
    let red = RunStyleEdit {
        color: Some("FF0000".into()),
        ..RunStyleEdit::default()
    };

    // Edit 1 (tracked): bold "Format".
    let (e1, _) = apply_transaction(
        &base,
        &txn(vec![fmt_step(&id, bold, RunStyleEdit::default())]),
    )
    .expect("first tracked format (bold) applies");

    // Edit 2 (tracked): recolor the SAME (now-tracked-formatted) run red. Before
    // the fix this was rejected with UnsupportedParagraphStructure (B6).
    let (e2, _) = apply_transaction(
        &e1,
        &txn(vec![fmt_step(&id, InlineMarkSet::default(), red)]),
    )
    .expect("second tracked format on an already-formatted run must apply (B6)");

    // accept-all: the run carries BOTH bold and red.
    let mut acc = e2.clone();
    accept_all(&mut acc);
    let r = run_containing(&acc, "Format").expect("accept-all run");
    assert!(r.marks.contains(&Mark::Bold), "accept-all keeps bold");
    assert_eq!(
        r.style_props.color.as_deref(),
        Some("FF0000"),
        "accept-all keeps red"
    );

    // reject-all: restores the ORIGINAL run — neither bold nor color. (The trace's
    // "snapshot current live state" approach would leave bold here.)
    let mut rej = e2;
    reject_all_with_styles(&mut rej, None);
    assert!(
        !any_bold_or_color(&rej),
        "reject-all restores the original run (no bold, no color), not the intermediate"
    );
}
