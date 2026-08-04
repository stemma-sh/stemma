//! Same-transaction run-formatting composition.
//!
//! Several format steps in one atomic transaction are one logical proposal and
//! compose into one rPrChange. A later transaction is a distinct proposal and
//! must refuse instead of absorbing the pending revision.
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
fn same_transaction_formats_compose_and_reject_restores_original() {
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

    let (e2, _) = apply_transaction(
        &base,
        &txn(vec![
            fmt_step(&id, bold, RunStyleEdit::default()),
            fmt_step(&id, InlineMarkSet::default(), red),
        ]),
    )
    .expect("format steps in one transaction compose");

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

#[test]
fn later_transaction_refuses_to_absorb_pending_format_revision() {
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
    let (first, _) = apply_transaction(
        &base,
        &txn(vec![fmt_step(&id, bold, RunStyleEdit::default())]),
    )
    .expect("first proposal applies");

    let before_refusal = first.clone();
    let error = apply_transaction(
        &first,
        &txn(vec![fmt_step(&id, InlineMarkSet::default(), red)]),
    )
    .expect_err("a later transaction is an independent proposal");
    assert!(
        matches!(
            error,
            stemma::edit::EditError::FormatRevisionConflict { .. }
        ),
        "refusal names the existing format revision: {error:?}"
    );
    assert_eq!(first, before_refusal, "a refusal does not mutate its input");
}

#[test]
fn direct_mode_refuses_to_flatten_pending_format_revision() {
    let (base, ids) = doc_and_ids(&["Format me"]);
    let id = ids[0].clone();
    let bold = InlineMarkSet {
        bold: true,
        ..InlineMarkSet::default()
    };
    let (pending, _) = apply_transaction(
        &base,
        &txn(vec![fmt_step(&id, bold, RunStyleEdit::default())]),
    )
    .expect("tracked proposal applies");
    let before = pending.clone();
    let mut direct = txn(vec![fmt_step(
        &id,
        InlineMarkSet {
            italic: true,
            ..InlineMarkSet::default()
        },
        RunStyleEdit::default(),
    )]);
    direct.materialization_mode = MaterializationMode::Direct;

    let error = apply_transaction(&pending, &direct)
        .expect_err("direct mode cannot erase pending revision metadata");
    assert!(matches!(
        error,
        stemma::edit::EditError::FormatRevisionConflict { .. }
    ));
    assert_eq!(
        pending, before,
        "direct refusal leaves the document unchanged"
    );
}
