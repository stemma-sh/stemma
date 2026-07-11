//! `ApplyStyle` per-verb integration invariants (daily tier).
//!
//! ApplyStyle authors a tracked paragraph-style change as a `w:pPrChange`
//! carrying the prior `w:pStyle` (§17.3.1.27 / §17.13.5.29). The verb's "done"
//! criterion (domain-model §11; `stemma-engine/src/edit/AGENTS.md`):
//!
//! - reject-all restores `previous_style_id` exactly (reversibility);
//! - accept-all keeps the new style and equals direct apply;
//! - the paragraph's segment text is byte-identical across the change (a style
//!   swap is a property delta, never a text edit);
//! - refuse a no-op (style already == target) and a stacked `pPrChange`.

use stemma::api::Document;
use stemma::domain::*;
use stemma::edit::*;
use stemma::{accept_all, reject_all_with_styles};

/// A plain-paragraph DOCX. Paragraphs carry no `w:pStyle`, so `style_id` parses
/// as `None` (the implicit Normal style) — the base state ApplyStyle changes
/// FROM.
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

fn txn(steps: Vec<EditStep>, mode: MaterializationMode) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        materialization_mode: mode,
        revision: RevisionInfo {
            revision_id: 1,
            author: Some("Styler".to_string()),
            date: Some("2026-06-01T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

fn first_para(canon: &CanonDoc) -> &ParagraphNode {
    match &canon.blocks[0].block {
        BlockNode::Paragraph(p) => p,
        _ => panic!("first block is not a paragraph"),
    }
}

/// Concatenated visible text of the first paragraph's segments.
fn first_para_text(canon: &CanonDoc) -> String {
    let mut s = String::new();
    for seg in &first_para(canon).segments {
        for inline in &seg.inlines {
            if let InlineNode::Text(t) = inline {
                s.push_str(&t.text);
            }
        }
    }
    s
}

#[test]
fn apply_style_reject_restores_previous_accept_keeps_new() {
    let (base, ids) = doc_and_ids(&["A clause that should become a heading."]);
    let block_id = NodeId::from(ids[0].as_str());
    let base_style = first_para(&base).style_id.clone();
    let base_text = first_para_text(&base);

    let steps = vec![EditStep::ApplyStyle {
        block_id: block_id.clone(),
        semantic_hash: None,
        style_id: "Heading2".to_string(),
        rationale: None,
    }];

    let tracked = apply_transaction(
        &base,
        &txn(steps.clone(), MaterializationMode::TrackedChange),
    )
    .expect("tracked apply")
    .0;

    // Tracked: new style live, pPrChange recorded, text unchanged.
    {
        let p = first_para(&tracked);
        assert_eq!(p.style_id.as_deref(), Some("Heading2"));
        assert!(
            p.formatting_change.is_some(),
            "tracked style change must record a pPrChange"
        );
        assert_eq!(
            p.formatting_change.as_ref().unwrap().previous_style_id,
            base_style,
            "pPrChange must record the prior style for reject-restore"
        );
        assert_eq!(
            first_para_text(&tracked),
            base_text,
            "a style swap must not touch the segment text"
        );
    }

    // Reject-all restores the previous style and drops the change.
    let mut rejected = tracked.clone();
    reject_all_with_styles(&mut rejected, None);
    {
        let p = first_para(&rejected);
        assert_eq!(
            p.style_id, base_style,
            "reject-all must restore previous style"
        );
        assert!(
            p.formatting_change.is_none(),
            "reject-all must drop the pPrChange record"
        );
        assert_eq!(
            first_para_text(&rejected),
            base_text,
            "text byte-identical after reject"
        );
    }

    // Accept-all keeps the new style and equals direct apply.
    let mut accepted = tracked;
    accept_all(&mut accepted);
    let direct = apply_transaction(&base, &txn(steps, MaterializationMode::Direct))
        .expect("direct apply")
        .0;
    {
        let a = first_para(&accepted);
        let d = first_para(&direct);
        assert_eq!(a.style_id.as_deref(), Some("Heading2"));
        assert_eq!(a.style_id, d.style_id, "accept-all style must equal direct");
        assert!(
            a.formatting_change.is_none() && d.formatting_change.is_none(),
            "neither accept-all nor direct leaves a pPrChange record"
        );
        assert_eq!(
            first_para_text(&accepted),
            base_text,
            "text byte-identical after accept"
        );
    }
}

#[test]
fn apply_style_refuses_noop() {
    // Re-apply the style the paragraph already has after a direct change.
    let (base, ids) = doc_and_ids(&["Already a heading."]);
    let block_id = NodeId::from(ids[0].as_str());
    let direct = apply_transaction(
        &base,
        &txn(
            vec![EditStep::ApplyStyle {
                block_id: block_id.clone(),
                semantic_hash: None,
                style_id: "Heading2".to_string(),
                rationale: None,
            }],
            MaterializationMode::Direct,
        ),
    )
    .expect("direct apply sets Heading2")
    .0;

    // Now Heading2 is the live style; requesting it again is a no-op.
    let err = apply_transaction(
        &direct,
        &txn(
            vec![EditStep::ApplyStyle {
                block_id,
                semantic_hash: None,
                style_id: "Heading2".to_string(),
                rationale: None,
            }],
            MaterializationMode::TrackedChange,
        ),
    );
    assert!(matches!(err, Err(EditError::NoStyleChangeRequested { .. })));
}

#[test]
fn apply_style_refuses_stacked_pprchange() {
    let (base, ids) = doc_and_ids(&["A clause."]);
    let block_id = NodeId::from(ids[0].as_str());

    // First tracked style change leaves a pPrChange.
    let tracked = apply_transaction(
        &base,
        &txn(
            vec![EditStep::ApplyStyle {
                block_id: block_id.clone(),
                semantic_hash: None,
                style_id: "Heading2".to_string(),
                rationale: None,
            }],
            MaterializationMode::TrackedChange,
        ),
    )
    .expect("first apply")
    .0;

    // Second tracked style change must refuse to stack onto the unresolved one.
    let err = apply_transaction(
        &tracked,
        &txn(
            vec![EditStep::ApplyStyle {
                block_id,
                semantic_hash: None,
                style_id: "Heading3".to_string(),
                rationale: None,
            }],
            MaterializationMode::TrackedChange,
        ),
    );
    assert!(matches!(
        err,
        Err(EditError::UnsupportedParagraphStructure { .. })
    ));
}

#[test]
fn apply_style_missing_block_is_error() {
    let (base, _ids) = doc_and_ids(&["A clause."]);
    let err = apply_transaction(
        &base,
        &txn(
            vec![EditStep::ApplyStyle {
                block_id: NodeId::from("does-not-exist"),
                semantic_hash: None,
                style_id: "Heading2".to_string(),
                rationale: None,
            }],
            MaterializationMode::TrackedChange,
        ),
    );
    assert!(matches!(err, Err(EditError::BlockNotFound { .. })));
}
