//! Blindspot regression: a tracked paragraph-property change (`w:pPrChange`,
//! ECMA-376 §17.13.5.29) recorded in the SAME `TrackedChange` transaction as a
//! later text edit on the SAME paragraph must remain reversible.
//!
//! Domain rule (the tracked-change reversibility invariant, the standing gate
//! in `edit_fidelity_invariants.rs` §1): for any valid TrackedChange edit,
//! `reject_all` of the edited document reconstructs the original EXACTLY —
//! original text AND original paragraph formatting. ECMA-376 §17.13.5.29
//! `w:pPrChange` stores the prior `w:pPr`; rejecting the revision restores it.
//!
//! Suspected defect (edit/mod.rs:6918 + 3837 + tracked_model.rs:5113): the
//! `ReplaceParagraphText` step calls `prepare_paragraph_for_direct_edit`, which
//! runs `project_block_for_accept_reject(block, /*keep_inserted=*/true)` — an
//! ACCEPT of the block, regardless of the transaction's materialization mode.
//! Accept sets `p.formatting_change = None`, permanently discarding the
//! `pPrChange` recorded by the earlier `SetParagraphFormatting` step. After
//! that, `reject_all` can no longer restore the original alignment, so the
//! rejected document keeps the new alignment instead of the base one.
//!
//! If the shapes differ, the defect is real: the formatting change was silently
//! accepted and is no longer reversible.

use stemma::api::Document;
use stemma::domain::*;
use stemma::edit::*;
use stemma::reject_all_with_styles;
use stemma::view::{BlockRole, SegmentView, TextMark, TrackStatus, build_document_view_from_canon};

// ─── Fixtures (verbatim from edit_fidelity_invariants.rs) ────────────────────

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
            author: Some("Gate".to_string()),
            date: Some("2026-06-01T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

fn text_content(text: &str) -> ParagraphContent {
    ParagraphContent {
        fragments: vec![ContentFragment::Text(text.to_string())],
    }
}

// `shape` verbatim from edit_fidelity_invariants.rs:159-233. It folds in
// paragraph-level alignment/indent/spacing so a pure formatting flip that
// reject failed to restore is caught even though the visible text matches.
fn shape(canon: &CanonDoc) -> String {
    fn status_tag(s: &TrackStatus) -> &'static str {
        match s {
            TrackStatus::Normal => "=",
            TrackStatus::Inserted(_) => "+",
            TrackStatus::Deleted(_) => "-",
            TrackStatus::InsertedThenDeleted { .. } => "±",
        }
    }
    fn marks_tag(marks: &[TextMark]) -> String {
        let mut s = String::new();
        for (m, c) in [
            (TextMark::Bold, 'b'),
            (TextMark::Italic, 'i'),
            (TextMark::Underline, 'u'),
            (TextMark::Strike, 's'),
            (TextMark::Subscript, 'v'),
            (TextMark::Superscript, '^'),
        ] {
            if marks.contains(&m) {
                s.push(c);
            }
        }
        s
    }
    fn role_tag(role: &BlockRole) -> String {
        match role {
            BlockRole::Paragraph => "para".to_string(),
            BlockRole::Heading { level } => format!("h{level}"),
            BlockRole::Table => "table".to_string(),
            BlockRole::Opaque => "opaque".to_string(),
        }
    }

    let view = build_document_view_from_canon(canon);
    let mut out = String::new();
    for (tb, b) in canon.blocks.iter().zip(view.blocks.iter()) {
        let para_fmt = match &tb.block {
            BlockNode::Paragraph(p) => format!(
                "|align={:?},indent={:?},spacing={:?}",
                p.align, p.indent, p.spacing
            ),
            _ => String::new(),
        };
        out.push_str(&format!(
            "[{}|{}|{}{}]\n",
            role_tag(&b.role),
            b.style_id.as_deref().unwrap_or(""),
            status_tag(&b.block_status),
            para_fmt,
        ));
        for seg in &b.segments {
            match seg {
                SegmentView::Text {
                    text,
                    status,
                    marks,
                    ..
                } => out.push_str(&format!(
                    "  T{}{}:{text}\n",
                    status_tag(status),
                    marks_tag(marks)
                )),
                SegmentView::Opaque { kind, status, .. } => {
                    out.push_str(&format!("  A{}:{kind:?}\n", status_tag(status)))
                }
            }
        }
    }
    out
}

// ─── The blindspot ───────────────────────────────────────────────────────────

/// CASE 1 — single TrackedChange transaction: [SetParagraphFormatting(Center),
/// ReplaceParagraphText]. reject_all must restore BOTH original text and
/// original (default/left) alignment.
#[test]
fn single_txn_fmt_then_text_reject_restores_formatting_and_text() {
    let (base, ids) = doc_and_ids(&["Hello world"]);
    let base_shape = shape(&base);
    let block_id = NodeId::from(ids[0].clone());

    let steps = vec![
        EditStep::SetParagraphFormatting {
            block_id: block_id.clone(),
            semantic_hash: None,
            patch: ParagraphFormattingPatch {
                align: Some(Alignment::Center),
                indent: None,
                spacing: None,
                borders: None,
                shading: None,
            },
            rationale: None,
        },
        EditStep::ReplaceParagraphText {
            block_id: block_id.clone(),
            rationale: None,
            replacement_role: None,
            expect: "Hello world".to_string(),
            semantic_hash: None,
            content: text_content("Goodbye world"),
        },
    ];

    let (edited, _) =
        apply_transaction(&base, &txn(steps, MaterializationMode::TrackedChange)).unwrap();

    let mut rejected = edited.clone();
    reject_all_with_styles(&mut rejected, None);

    let rejected_shape = shape(&rejected);
    assert_eq!(
        rejected_shape, base_shape,
        "reject_all after a single TrackedChange txn [pPrChange, text-replace] on the same \
         paragraph must restore the original text AND the original alignment \
         (ECMA-376 §17.13.5.29). Got:\n--- rejected ---\n{rejected_shape}\n--- base ---\n{base_shape}"
    );
}

/// CASE 2 — two sequential TrackedChange transactions on the same paragraph:
/// txn A records the pPrChange (Center), txn B replaces the text. Rejecting the
/// combined result must still restore the base. This separates "step ordering
/// within one txn" from "a later text edit clobbers an already-committed
/// pPrChange".
#[test]
fn sequential_txns_text_after_fmt_reject_restores_formatting() {
    let (base, ids) = doc_and_ids(&["Hello world"]);
    let base_shape = shape(&base);
    let block_id = NodeId::from(ids[0].clone());

    let (after_fmt, _) = apply_transaction(
        &base,
        &txn(
            vec![EditStep::SetParagraphFormatting {
                block_id: block_id.clone(),
                semantic_hash: None,
                patch: ParagraphFormattingPatch {
                    align: Some(Alignment::Center),
                    indent: None,
                    spacing: None,
                    borders: None,
                    shading: None,
                },
                rationale: None,
            }],
            MaterializationMode::TrackedChange,
        ),
    )
    .unwrap();

    let (after_text, _) = apply_transaction(
        &after_fmt,
        &txn(
            vec![EditStep::ReplaceParagraphText {
                block_id: block_id.clone(),
                rationale: None,
                replacement_role: None,
                expect: "Hello world".to_string(),
                semantic_hash: None,
                content: text_content("Goodbye world"),
            }],
            MaterializationMode::TrackedChange,
        ),
    )
    .unwrap();

    let mut rejected = after_text.clone();
    reject_all_with_styles(&mut rejected, None);

    let rejected_shape = shape(&rejected);
    assert_eq!(
        rejected_shape, base_shape,
        "A text replace in a later TrackedChange txn must not silently accept the pPrChange \
         committed by an earlier txn. reject_all_with_styles must restore the base alignment \
         (ECMA-376 §17.13.5.29). Got:\n--- rejected ---\n{rejected_shape}\n--- base ---\n{base_shape}"
    );
}
