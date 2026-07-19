//! Fail-fast validation tests, driven END-TO-END from real DOCX bytes. The
//! engine must REFUSE edits addressed against blocks whose existence or content
//! is itself under tracked review, with the correct typed `EditError`. No silent
//! fallbacks (CLAUDE.md prime directive).
//!
//! DOMAIN RULE (shared by every case here): editing relative to a block whose
//! existence is itself under review — a tracked-inserted/deleted block — has no
//! well-defined accept/reject semantics. The engine must fail loudly rather than
//! guess which resolution the caller intended.
//!
//! Inputs are authored as body XML (tracked blocks via body-level `<w:ins>` /
//! `<w:del>`; tables via `<w:tbl>`; hyperlinks via `<w:hyperlink>`), zipped into
//! a minimal valid .docx, and imported with `Document::parse`. The precise
//! `EditError` variant is asserted via `apply_transaction` on the imported
//! CanonDoc — the same pattern `blocks_to_table.rs` uses for its refusal cases.
//! No hand-built IR. Corpus-free, daily tier.

use stemma::api::Document;
use stemma::domain::{BlockNode, CanonDoc, NodeId, RevisionInfo, TrackingStatus};
use stemma::edit::{
    ContentFragment, EditError, EditStep, EditTransaction, MaterializationMode, ParagraphContent,
    ResolvedSpanSelector, apply_transaction,
};

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

// ─── Synthetic-docx helper ───────────────────────────────────────────────────

fn make_docx(body_inner: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W_NS}" xmlns:r="{R_NS}"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
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

fn parse(body: &str) -> CanonDoc {
    (*Document::parse(&make_docx(body))
        .expect("parse")
        .snapshot()
        .canonical)
        .clone()
}

fn test_revision() -> RevisionInfo {
    RevisionInfo {
        revision_id: 100,
        identity: 0,
        author: Some("Test Author".to_string()),
        date: Some("2026-06-07T00:00:00Z".to_string()),
        apply_op_id: None,
    }
}

fn text_content(s: &str) -> ParagraphContent {
    ParagraphContent {
        fragments: vec![ContentFragment::Text(s.to_string())],
    }
}

fn one_step_tx(step: EditStep) -> EditTransaction {
    EditTransaction {
        steps: vec![step],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    }
}

/// The id of the i-th top-level block, as assigned by import.
fn block_id_at(canon: &CanonDoc, idx: usize) -> NodeId {
    match &canon.blocks[idx].block {
        BlockNode::Paragraph(p) => p.id.clone(),
        BlockNode::Table(t) => t.id.clone(),
        BlockNode::OpaqueBlock(o) => o.id.clone(),
    }
}

/// The first hyperlink-opaque id anywhere in the doc (import-assigned).
fn first_hyperlink_id(canon: &CanonDoc) -> NodeId {
    for tb in &canon.blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let stemma::domain::InlineNode::OpaqueInline(o) = inline
                        && matches!(o.kind, stemma::domain::OpaqueKind::Hyperlink(_))
                    {
                        return o.id.clone();
                    }
                }
            }
        }
    }
    panic!("no hyperlink opaque in doc");
}

// ─── span replace onto a tracked-DELETED block ───────────────────────────────
//
// Span replace never pre-flattens tracked changes (the splice resolves the
// handle against the same segment structure the read view minted it over,
// and carries out-of-range tracked segments through untouched — see
// `spec_span_splice.rs` for the range contract and layer-beside coverage).
// Block-LEVEL tracked status is still refused here: a block whose existence
// is itself under review (tracked-deleted/-inserted) is not a valid splice
// target.
#[test]
fn span_replace_on_deleted_block_fails_block_has_tracked_status() {
    // Body-level <w:del> wrapping the paragraph imports it as a Deleted block.
    let body = r#"<w:del w:id="5" w:author="a" w:date="2020-01-01T00:00:00Z"><w:p><w:pPr><w:rPr><w:del w:id="6" w:author="a" w:date="2020-01-01T00:00:00Z"/></w:rPr></w:pPr><w:r><w:t>hello world deleted block</w:t></w:r></w:p></w:del>"#;
    let canon = parse(body);
    assert!(
        matches!(canon.blocks[0].status, TrackingStatus::Deleted(_)),
        "fixture must import as a tracked-deleted block"
    );
    let target = block_id_at(&canon, 0);

    let step = EditStep::ReplaceSpanText {
        block_id: target,
        // The refusal under test fires on block tracking status, before the
        // guard is compared, so the guard value is irrelevant here.
        guard: "unused-refusal-fires-first".to_string(),
        expect: None,
        span: ResolvedSpanSelector::Whole,
        content: text_content("goodbye"),
        rationale: None,
    };
    let err = apply_transaction(&canon, &one_step_tx(step))
        .expect_err("span replace on a deleted block must fail");
    assert!(
        matches!(
            err,
            EditError::BlockHasTrackedStatus {
                status: "deleted",
                ..
            }
        ),
        "expected BlockHasTrackedStatus(deleted), got: {err:?}"
    );
}

// ─── span replace onto a table block ─────────────────────────────────────────

#[test]
fn span_replace_on_table_block_fails_not_a_paragraph() {
    // A table id resolves at the top level but is not a paragraph — span text
    // addressing is paragraph-only.
    let body = concat!(
        r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol/></w:tblGrid>"#,
        r#"<w:tr><w:tc><w:tcPr/><w:p><w:r><w:t>cell text</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
    );
    let canon = parse(body);
    assert!(
        matches!(canon.blocks[0].block, BlockNode::Table(_)),
        "fixture must import as a table block"
    );
    let target = block_id_at(&canon, 0);

    let step = EditStep::ReplaceSpanText {
        block_id: target,
        // The refusal under test fires on block kind, before the guard is
        // compared, so the guard value is irrelevant here.
        guard: "unused-refusal-fires-first".to_string(),
        expect: None,
        span: ResolvedSpanSelector::Whole,
        content: text_content("x"),
        rationale: None,
    };
    let err = apply_transaction(&canon, &one_step_tx(step))
        .expect_err("span replace on a table must fail");
    assert!(
        matches!(
            err,
            EditError::NotAParagraph {
                actual_kind: "table",
                ..
            }
        ),
        "expected NotAParagraph(table), got: {err:?}"
    );
}

// ─── delete(block_range) endpoint is an already-tracked block ────────────────
//
// NOTE on the task's "delete endpoint is a table -> NotAParagraph" case:
// `DeleteBlockRange`'s per-block gate (`validate_block_is_editable`) checks only
// block-level tracking status and paragraph-segment status; it deliberately does
// NOT reject a `BlockNode::Table`. Deleting a table (or a range ending on one) is
// a legitimate operation — the whole block is marked Deleted. So a table
// endpoint does NOT fail with `NotAParagraph`; that expectation is incorrect for
// delete, and the case is SKIPPED. The tracked-endpoint case below is the real
// "existence under review" fail-fast for delete.
#[test]
fn delete_block_range_with_tracked_endpoint_fails_block_has_tracked_status() {
    // [p0..=p1] where p1 is already tracked-deleted (body-level <w:del>). Its
    // existence is under review; re-deleting it has no well-defined semantics.
    let body = concat!(
        r#"<w:p><w:r><w:t xml:space="preserve">first para</w:t></w:r></w:p>"#,
        r#"<w:del w:id="5" w:author="a" w:date="2020-01-01T00:00:00Z"><w:p><w:pPr><w:rPr><w:del w:id="6" w:author="a" w:date="2020-01-01T00:00:00Z"/></w:rPr></w:pPr><w:r><w:t>second para</w:t></w:r></w:p></w:del>"#,
    );
    let canon = parse(body);
    assert!(
        matches!(canon.blocks[1].status, TrackingStatus::Deleted(_)),
        "second block must import as tracked-deleted"
    );
    let from = block_id_at(&canon, 0);
    let to = block_id_at(&canon, 1);

    let step = EditStep::DeleteBlockRange {
        from_block_id: from,
        to_block_id: to,
        rationale: None,
        expect: "first".to_string(),
        semantic_hash: None,
    };
    let err = apply_transaction(&canon, &one_step_tx(step))
        .expect_err("delete range ending on a tracked block must fail");
    assert!(
        matches!(
            err,
            EditError::BlockHasTrackedStatus {
                status: "deleted",
                ..
            }
        ),
        "expected BlockHasTrackedStatus(deleted), got: {err:?}"
    );
}

// ─── hyperlink replace inside an Inserted paragraph ──────────────────────────

#[test]
fn replace_hyperlink_inside_inserted_paragraph_fails_block_has_tracked_status() {
    // Body-level <w:ins> wraps a paragraph that contains a hyperlink. The
    // paragraph's existence is under review → editing the link text is undefined.
    let body = r#"<w:ins w:id="7" w:author="a" w:date="2020-01-01T00:00:00Z"><w:p><w:pPr><w:rPr><w:ins w:id="8" w:author="a" w:date="2020-01-01T00:00:00Z"/></w:rPr></w:pPr><w:r><w:t xml:space="preserve">see </w:t></w:r><w:hyperlink r:id="rIdX"><w:r><w:t>here</w:t></w:r></w:hyperlink></w:p></w:ins>"#;
    let canon = parse(body);
    assert!(
        matches!(canon.blocks[0].status, TrackingStatus::Inserted(_)),
        "fixture must import as a tracked-inserted block"
    );
    let hyperlink_id = first_hyperlink_id(&canon);

    let step = EditStep::ReplaceHyperlinkText {
        hyperlink_id,
        rationale: None,
        expect: "here".to_string(),
        new_text: "there".to_string(),
        expect_href: None,
        expect_anchor: None,
    };
    let err = apply_transaction(&canon, &one_step_tx(step))
        .expect_err("replace hyperlink in an inserted paragraph must fail");
    assert!(
        matches!(
            err,
            EditError::BlockHasTrackedStatus {
                status: "inserted",
                ..
            }
        ),
        "expected BlockHasTrackedStatus(inserted), got: {err:?}"
    );
}

// ─── hyperlink set_attr inside a Deleted paragraph ───────────────────────────

#[test]
fn set_hyperlink_attr_inside_deleted_paragraph_fails_block_has_tracked_status() {
    // Body-level <w:del> wraps a paragraph that contains a hyperlink. Retargeting
    // its href is undefined while the enclosing block is under deletion review.
    let body = r#"<w:del w:id="7" w:author="a" w:date="2020-01-01T00:00:00Z"><w:p><w:pPr><w:rPr><w:del w:id="8" w:author="a" w:date="2020-01-01T00:00:00Z"/></w:rPr></w:pPr><w:r><w:delText xml:space="preserve">see </w:delText></w:r><w:hyperlink r:id="rIdX"><w:r><w:delText>here</w:delText></w:r></w:hyperlink></w:p></w:del>"#;
    let canon = parse(body);
    assert!(
        matches!(canon.blocks[0].status, TrackingStatus::Deleted(_)),
        "fixture must import as a tracked-deleted block"
    );
    let hyperlink_id = first_hyperlink_id(&canon);

    let step = EditStep::SetHyperlinkAttr {
        hyperlink_id,
        new_href: Some("https://new.example.com/".to_string()),
        new_anchor: None,
        expect_href: None,
        expect_anchor: None,
        rationale: None,
    };
    let err = apply_transaction(&canon, &one_step_tx(step))
        .expect_err("set_attr on a hyperlink in a deleted paragraph must fail");
    assert!(
        matches!(
            err,
            EditError::BlockHasTrackedStatus {
                status: "deleted",
                ..
            }
        ),
        "expected BlockHasTrackedStatus(deleted), got: {err:?}"
    );
}
