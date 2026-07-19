//! Re-editing a paragraph that is itself a pending BLOCK insertion.
//!
//! `InsertParagraphs` (tracked) records the new paragraph as a block-level
//! `TrackingStatus::Inserted` wrapping ordinary (`Normal`-status) segments — the
//! insertion lives on the block, not on each run. A later tracked
//! `ReplaceParagraphText` on that same paragraph diffs the new text against the
//! paragraph's segments; the removed span originated in a `Normal` segment, so
//! the reconstruction minted a plain base-class `Deleted` tombstone for it —
//! blind to the fact that the whole paragraph is a pending insertion that never
//! existed in the base.
//!
//! That is wrong: a plain deletion is "restored on reject" (§17.13.5.20), so
//! reject-all brought the removed span BACK as ordinary text while the rest of
//! the inserted paragraph was rejected away — text that never existed in the
//! base leaked into the "before" document. The invariant (the same one
//! `InsertedThenDeleted` already encodes elsewhere): content that exists ONLY by
//! virtue of a pending insertion, then removed by a later pending edit, must drop
//! in BOTH full resolutions — reject-all restores the pristine original, accept-all
//! shows only the re-edited content.
//!
//! For a SAME-author re-edit the removed span is un-proposed (it was the author's
//! own pending insertion — Word simply forgets it); for a CROSS-author re-edit it
//! becomes the stacked `InsertedThenDeleted` state. Both drop in full accept AND
//! full reject.
//!
//! Covered on the IR projection AND the wire (serialize → `reject_all_docx` /
//! `normalize_docx`), because the leak is observable only after the block
//! insertion is lowered to per-run `w:ins` + an inserted paragraph mark.
//!
//! Daily tier: synthesized in-memory DOCX, no corpus, no real-Word oracle.

use std::io::Write;

use stemma::api::{Document, validate};
use stemma::docx::DocxArchive;
use stemma::domain::{BlockNode, CanonDoc, NodeId, RevisionInfo, TrackingStatus};
use stemma::edit::{
    BlockSpec, ContentFragment, EditStep, EditTransaction, InsertPosition, MaterializationMode,
    ParagraphBlockSpec, ParagraphContent, parse_paragraph_markup,
};
use stemma::semantic_hash::block_guard;
use stemma::{ExportOptions, Resolution};

const AUTHOR: &str = "Corpus Mutation";
const INSERTED: &str = "Probe inserted paragraph one.";
const REEDITED: &str = "QQP1QQ5 probe inserted paragraph one.";

fn base_docx() -> Vec<u8> {
    let body_inner = r#"<w:p><w:r><w:t xml:space="preserve">Anchor paragraph.</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve">Trailing paragraph.</w:t></w:r></w:p>"#;
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
    let mut buf = Vec::new();
    {
        use zip::write::FileOptions;
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

fn txn(steps: Vec<EditStep>, author: &str, revision_id: u32) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id,
            identity: 0,
            author: Some(author.to_string()),
            date: Some("2026-07-10T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

fn first_para_id(canon: &CanonDoc) -> NodeId {
    canon
        .blocks
        .iter()
        .find_map(|tb| match &tb.block {
            BlockNode::Paragraph(p) => Some(p.id.clone()),
            _ => None,
        })
        .expect("a paragraph")
}

/// The id + projected block-guard of the pending-inserted paragraph whose visible
/// text is `INSERTED`.
fn inserted_para(canon: &CanonDoc) -> (NodeId, String) {
    let tb = canon
        .blocks
        .iter()
        .find(|tb| {
            matches!(tb.status, TrackingStatus::Inserted(_))
                && matches!(&tb.block, BlockNode::Paragraph(p)
                    if paragraph_text(p) == INSERTED)
        })
        .expect("the pending-inserted paragraph is present");
    let BlockNode::Paragraph(p) = &tb.block else {
        unreachable!()
    };
    (p.id.clone(), block_guard(&tb.block))
}

fn paragraph_text(p: &stemma::domain::ParagraphNode) -> String {
    use stemma::domain::InlineNode;
    let mut s = String::new();
    for seg in &p.segments {
        for inline in &seg.inlines {
            if let InlineNode::Text(t) = inline {
                s.push_str(&t.text);
            }
        }
    }
    s
}

/// The full body text stream of a serialized docx after resolving all revisions
/// on the WIRE with `resolve` (reject_all_docx / normalize_docx).
fn wire_resolved_text(
    bytes: &[u8],
    resolve: fn(
        &DocxArchive,
    ) -> Result<
        (DocxArchive, stemma::normalize::NormalizationResult),
        stemma::normalize::NormalizeError,
    >,
) -> String {
    let archive = DocxArchive::read(bytes).expect("read docx");
    let (out, _) = resolve(&archive).expect("resolve wire");
    let xml = String::from_utf8(out.get("word/document.xml").unwrap().to_vec()).unwrap();
    // Concatenate the text of every w:t / w:delText-restored run. After a full
    // reject there are no w:delText left; a crude tag-strip of <w:t>…</w:t> is
    // enough for a text-stream comparison on these synthetic bodies.
    strip_run_text(&xml)
}

/// Extract the concatenation of all `<w:t …>text</w:t>` payloads in document
/// order — the visible text stream of a fully-resolved (no tracked-change)
/// document.
fn strip_run_text(xml: &str) -> String {
    let mut out = String::new();
    let mut rest = xml;
    while let Some(open) = rest.find("<w:t") {
        // Only match the <w:t> / <w:t …> run-text element, not <w:tbl>, <w:tc>, …
        let after = &rest[open + 4..];
        let boundary = after.as_bytes().first().copied();
        if !matches!(boundary, Some(b'>') | Some(b' ') | Some(b'/')) {
            rest = after;
            continue;
        }
        rest = after;
        // Skip to the end of the opening tag.
        let Some(gt) = rest.find('>') else { break };
        // Self-closing <w:t/> carries no text (the char before '>' is '/').
        if gt > 0 && rest.as_bytes()[gt - 1] == b'/' {
            rest = &rest[gt + 1..];
            continue;
        }
        rest = &rest[gt + 1..];
        let Some(close) = rest.find("</w:t>") else {
            break;
        };
        out.push_str(&rest[..close]);
        rest = &rest[close + 6..];
    }
    out
}

/// Apply insert-then-reedit and return the edited `Document`. `reedit_author` is
/// the author of the SECOND (re-edit) revision; the insertion is always by
/// `AUTHOR`. Same-author → the removed span is un-proposed; cross-author → it
/// becomes the stacked `InsertedThenDeleted` state. Both drop in full accept AND
/// full reject.
fn edited_document_by(reedit_author: &str) -> Document {
    let doc0 = Document::parse(&base_docx()).expect("parse base");
    let anchor = first_para_id(&doc0.snapshot().canonical);

    let insert = EditStep::InsertParagraphs {
        anchor_block_id: anchor,
        position: InsertPosition::After,
        rationale: None,
        blocks: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
            role: Some("body".to_string()),
            content: parse_paragraph_markup(INSERTED).unwrap(),
            restart_numbering: false,
            list: None,
        })],
    };
    let doc1 = doc0
        .apply(&txn(vec![insert], AUTHOR, 1))
        .expect("insert applies");

    let (para_id, guard) = inserted_para(&doc1.snapshot().canonical);
    let reedit = EditStep::ReplaceParagraphText {
        block_id: para_id,
        rationale: None,
        replacement_role: None,
        expect: INSERTED.to_string(),
        semantic_hash: Some(guard),
        content: ParagraphContent {
            fragments: vec![ContentFragment::Text(REEDITED.to_string())],
        },
    };
    // A SECOND revision id, `reedit_author` as the author.
    doc1.apply(&txn(vec![reedit], reedit_author, 2))
        .expect("re-edit of the inserted paragraph applies")
}

/// The reported repro: same author inserts then re-edits.
fn edited_document() -> Document {
    edited_document_by(AUTHOR)
}

// ── IR projection ───────────────────────────────────────────────────────────

#[test]
fn reject_all_restores_pristine_original_ir() {
    let doc = edited_document();
    let rej = doc.project(Resolution::RejectAll).expect("reject-all");
    let text: String = rej
        .snapshot()
        .canonical
        .blocks
        .iter()
        .filter_map(|tb| match &tb.block {
            BlockNode::Paragraph(p) => Some(paragraph_text(p)),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !text.contains("Wave"),
        "reject-all must not restore 'Wave' — it never existed in the base: {text:?}"
    );
    assert!(
        !text.contains("QQP1QQ"),
        "reject-all must drop the re-edit insertion too: {text:?}"
    );
    assert_eq!(
        text, "Anchor paragraph.\nTrailing paragraph.",
        "reject-all is the pristine two-paragraph original"
    );
}

#[test]
fn accept_all_shows_only_reedited_content_ir() {
    let doc = edited_document();
    let acc = doc.project(Resolution::AcceptAll).expect("accept-all");
    let text: String = acc
        .snapshot()
        .canonical
        .blocks
        .iter()
        .filter_map(|tb| match &tb.block {
            BlockNode::Paragraph(p) => Some(paragraph_text(p)),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains(REEDITED),
        "accept-all shows the re-edited paragraph: {text:?}"
    );
    assert!(
        !text.contains("Wave"),
        "accept-all must not show the discarded 'Wave' span: {text:?}"
    );
}

// ── Wire (serialize → reparse → resolve) ─────────────────────────────────────

#[test]
fn reject_all_restores_pristine_original_wire() {
    let doc = edited_document();
    let bytes = doc.serialize(&ExportOptions::default()).expect("serialize");
    assert!(
        validate(&bytes).ok,
        "redline serializes clean: {:?}",
        validate(&bytes).issues
    );

    let got = wire_resolved_text(&bytes, stemma::normalize::reject_all_docx);
    let base_reject = wire_resolved_text(&base_docx(), stemma::normalize::reject_all_docx);
    assert!(
        !got.contains("Wave"),
        "wire reject-all leaked 'Wave' (a plain Deleted tombstone was restored): {got:?}"
    );
    assert!(
        !got.contains("QQP1QQ"),
        "wire reject-all must drop the re-edit insertion: {got:?}"
    );
    assert_eq!(
        got, base_reject,
        "wire reject-all equals the pristine original's text stream"
    );
}

#[test]
fn accept_all_shows_only_reedited_content_wire() {
    let doc = edited_document();
    let bytes = doc.serialize(&ExportOptions::default()).expect("serialize");
    let got = wire_resolved_text(&bytes, stemma::normalize::normalize_docx);
    assert!(
        got.contains(REEDITED),
        "wire accept-all shows the re-edited paragraph: {got:?}"
    );
    assert!(
        !got.contains("Wave"),
        "wire accept-all must not show the discarded 'Wave' span: {got:?}"
    );
}

// ── Cross-author re-edit: the stacked InsertedThenDeleted branch ─────────────

#[test]
fn cross_author_reedit_drops_in_both_resolutions_wire() {
    // Author A inserts the paragraph; a DIFFERENT author B re-edits it. B cannot
    // un-propose A's insertion, so the removed span becomes the stacked
    // `InsertedThenDeleted` state (nested w:ins>w:del) — which drops in BOTH full
    // resolutions. reject-all restores the pristine original (A's whole block
    // insertion is rejected); accept-all shows only the re-edited content.
    let doc = edited_document_by("Reviewer B");
    let bytes = doc.serialize(&ExportOptions::default()).expect("serialize");
    assert!(
        validate(&bytes).ok,
        "cross-author redline serializes clean: {:?}",
        validate(&bytes).issues
    );

    let rejected = wire_resolved_text(&bytes, stemma::normalize::reject_all_docx);
    let base_reject = wire_resolved_text(&base_docx(), stemma::normalize::reject_all_docx);
    assert!(
        !rejected.contains("Wave"),
        "cross-author reject-all must not restore 'Wave': {rejected:?}"
    );
    assert_eq!(
        rejected, base_reject,
        "cross-author reject-all equals the pristine original's text stream"
    );

    let accepted = wire_resolved_text(&bytes, stemma::normalize::normalize_docx);
    assert!(
        accepted.contains(REEDITED),
        "cross-author accept-all shows the re-edited paragraph: {accepted:?}"
    );
    assert!(
        !accepted.contains("Wave"),
        "cross-author accept-all must not show the discarded 'Wave' span: {accepted:?}"
    );
}
