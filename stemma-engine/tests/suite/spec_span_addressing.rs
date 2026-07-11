//! Sub-stream B contract: **span-handle / anchor-relative replace ops**.
//!
//! Sub-block addressing: fine-grained in-block editing by span handle
//! (`s_<n>`) or anchor-relative range, never by substring. The block `guard`
//! makes the ephemeral handle safe.
//!
//! Invariants pinned here:
//!   - span handle round-trip: a handle from the detail view of block B resolves
//!     to the same inline range while B's guard is unchanged;
//!   - anchor-relative resolves by opaque id, NOT substring: with two identical
//!     phrases, `{after: anchor}` targets the position adjacent to the anchor;
//!   - fail-never opaque: a span replace dropping an in-span opaque fails
//!     `OpaqueDestroyed`; an opaque OUTSIDE the span survives;
//!   - Invariant M: `ReplaceSpanText` produces the same TrackedSegments as an
//!     equivalent whole-paragraph `ReplaceParagraphText` with the same net text
//!     (one materializer).
//!
//! Daily, corpus-free.

use stemma::api::Document;
use stemma::edit::{
    ContentFragment, EditError, EditStep, EditTransaction, MaterializationMode, ParagraphContent,
    ResolvedSpanSelector, apply_transaction,
};
use stemma::edit_v4::parse_transaction;
use stemma::{BlockNode, InlineNode, NodeId, RevisionInfo, TrackingStatus};

// ─── Fixtures ──────────────────────────────────────────────────────────────

fn make_docx_with_body(body_inner: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
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

fn plain_docx(text: &str) -> Vec<u8> {
    make_docx_with_body(&format!(
        r#"<w:p><w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p>"#
    ))
}

fn revision() -> RevisionInfo {
    RevisionInfo {
        revision_id: 9,
        author: Some("span-test".to_string()),
        date: Some("2026-06-05T00:00:00Z".to_string()),
        apply_op_id: None,
    }
}

fn text_content(s: &str) -> ParagraphContent {
    ParagraphContent {
        fragments: vec![ContentFragment::Text(s.to_string())],
    }
}

fn first_block_id_and_guard(doc: &Document) -> (NodeId, String) {
    let view = doc.read();
    (view.blocks[0].id.clone(), view.blocks[0].guard.clone())
}

/// All opaque-inline ids of the first paragraph, in document order.
fn opaque_ids(doc: &Document, block_id: &NodeId) -> Vec<String> {
    for tb in &doc.snapshot().canonical.blocks {
        if let BlockNode::Paragraph(p) = &tb.block
            && &p.id == block_id
        {
            let mut out = Vec::new();
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::OpaqueInline(o) = inline {
                        out.push(o.id.to_string());
                    }
                }
            }
            return out;
        }
    }
    panic!("block not found");
}

fn apply_step(doc: &Document, step: EditStep) -> Result<Document, stemma::RuntimeError> {
    doc.apply(&EditTransaction {
        steps: vec![step],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: revision(),
    })
}

// ─── Span handle round-trip ──────────────────────────────────────────────────

#[test]
fn span_handle_resolves_to_the_inline_range_the_reader_saw() {
    // A paragraph with multiple spans (a mark break makes >1 text span). The
    // handle the detail read minted must resolve, on write, to that exact span —
    // replacing only it.
    let body = r#"<w:p><w:r><w:t xml:space="preserve">The term is </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>30 days</w:t></w:r><w:r><w:t xml:space="preserve"> total.</w:t></w:r></w:p>"#;
    let doc = Document::parse(&make_docx_with_body(body)).expect("parse");
    let (block_id, guard) = first_block_id_and_guard(&doc);

    // The bold "30 days" is its own span (mark break). Find its handle.
    let view = doc.read();
    let segs = &view.blocks[0].segments;
    let bold_handle = segs
        .iter()
        .find_map(|s| match s {
            stemma::view::SegmentView::Text {
                text,
                marks,
                handle,
                ..
            } if text == "30 days" && marks.contains(&stemma::view::TextMark::Bold) => {
                handle.clone()
            }
            _ => None,
        })
        .expect("bold span has a handle")
        .0;

    let edited = apply_step(
        &doc,
        EditStep::ReplaceSpanText {
            block_id: block_id.clone(),
            guard,
            expect: None,
            span: ResolvedSpanSelector::Handle(bold_handle),
            content: text_content("60 days"),
            rationale: None,
        },
    )
    .expect("handle resolves and applies");

    // Accept-all reading: only the targeted span changed.
    let accepted = edited.read_accepted().expect("accept").read();
    assert_eq!(accepted.blocks[0].text, "The term is 60 days total.");
}

#[test]
fn stale_handle_out_of_range_fails_loud() {
    // A handle that no longer maps to a span (out of range) must fail, never
    // resolve against the wrong inlines.
    let doc = Document::parse(&plain_docx("One span only.")).expect("parse");
    let (block_id, guard) = first_block_id_and_guard(&doc);

    let err = match apply_step(
        &doc,
        EditStep::ReplaceSpanText {
            block_id,
            guard,
            expect: None,
            span: ResolvedSpanSelector::Handle("s_99".to_string()),
            content: text_content("x"),
            rationale: None,
        },
    ) {
        Ok(_) => panic!("a stale handle must fail"),
        Err(e) => e,
    };
    assert_eq!(err.code, stemma::ErrorCode::StaleEdit, "{err:?}");
}

// ─── Anchor-relative resolves by id, not substring ───────────────────────────

#[test]
fn anchor_after_targets_position_adjacent_to_anchor_not_first_substring() {
    // TWO identical phrases "see " around a field. `{after: field_id}` must
    // target the position adjacent to the FIELD, not the first "see " substring.
    // We replace the empty range right after the field with " inserted" and
    // assert the new text lands after the field, between the two phrases.
    let body = r#"<w:p><w:r><w:t xml:space="preserve">see </w:t></w:r><w:fldSimple w:instr=" REF A \h "><w:r><w:t>X</w:t></w:r></w:fldSimple><w:r><w:t xml:space="preserve"> see end</w:t></w:r></w:p>"#;
    let doc = Document::parse(&make_docx_with_body(body)).expect("parse");
    let (block_id, guard) = first_block_id_and_guard(&doc);
    let anchors = opaque_ids(&doc, &block_id);
    assert_eq!(anchors.len(), 1, "exactly one field anchor");
    let field_id = NodeId::from(anchors[0].clone());

    let edited = apply_step(
        &doc,
        EditStep::ReplaceSpanText {
            block_id: block_id.clone(),
            guard,
            expect: None,
            span: ResolvedSpanSelector::AnchorAfter(field_id.clone()),
            content: text_content("INSERTED"),
            rationale: None,
        },
    )
    .expect("anchor-after applies");

    // Inspect the IR inline order (the view's flattened `text` drops the opaque
    // label, so it cannot distinguish positions). The inserted "INSERTED" text
    // must sit IMMEDIATELY after the field opaque — proving the selector resolved
    // by the field's durable id, not the first "see " substring. If it had
    // targeted a substring, the inserted node would land elsewhere.
    let accepted = edited.read_accepted().expect("accept");
    let flat = flat_inline_labels(&accepted, &block_id);
    let field_pos = flat
        .iter()
        .position(|(kind, id)| kind == "opaque" && *id == field_id.to_string())
        .expect("field present after accept");
    assert!(
        flat.get(field_pos + 1)
            .map(|(k, t)| (k.as_str(), t.as_str()))
            == Some(("text", "INSERTED")),
        "inserted text must be immediately after the field anchor (by id): {flat:?}"
    );
}

/// Flat (kind, payload) labels of a paragraph's inlines: ("text", run text) or
/// ("opaque", opaque id), in document order, across accepted segments.
fn flat_inline_labels(doc: &Document, block_id: &NodeId) -> Vec<(String, String)> {
    for tb in &doc.snapshot().canonical.blocks {
        if let BlockNode::Paragraph(p) = &tb.block
            && &p.id == block_id
        {
            let mut out = Vec::new();
            for seg in &p.segments {
                for inline in &seg.inlines {
                    match inline {
                        InlineNode::Text(t) => out.push(("text".to_string(), t.text.clone())),
                        InlineNode::OpaqueInline(o) => {
                            out.push(("opaque".to_string(), o.id.to_string()))
                        }
                        _ => {}
                    }
                }
            }
            return out;
        }
    }
    panic!("block not found");
}

#[test]
fn anchor_not_found_fails_loud_never_substring_fallback() {
    let doc = Document::parse(&plain_docx("plain text, no anchors")).expect("parse");
    let (block_id, guard) = first_block_id_and_guard(&doc);

    let step = EditStep::ReplaceSpanText {
        block_id: block_id.clone(),
        guard,
        expect: None,
        span: ResolvedSpanSelector::AnchorAfter(NodeId::from("nonexistent_anchor")),
        content: text_content("x"),
        rationale: None,
    };
    // White-box at the engine level to assert the precise error variant.
    let err = apply_transaction(
        &doc.snapshot().canonical,
        &EditTransaction {
            steps: vec![step],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: revision(),
        },
    )
    .expect_err("absent anchor must fail");
    assert!(
        matches!(err, EditError::AnchorNotFound { .. }),
        "expected AnchorNotFound, got {err:?}"
    );
}

// ─── Fail-never opaque preservation ──────────────────────────────────────────

#[test]
fn dropping_an_in_span_opaque_fails_opaque_destroyed() {
    // A `between(start, end)` span that covers the whole paragraph including the
    // field, replaced by plain text that does NOT reference the field, must fail
    // OpaqueDestroyed — the in-span opaque cannot be silently dropped.
    let body = r#"<w:p><w:r><w:t xml:space="preserve">before </w:t></w:r><w:fldSimple w:instr=" REF A \h "><w:r><w:t>X</w:t></w:r></w:fldSimple><w:r><w:t xml:space="preserve"> after</w:t></w:r></w:p>"#;
    let doc = Document::parse(&make_docx_with_body(body)).expect("parse");
    let (block_id, guard) = first_block_id_and_guard(&doc);

    let step = EditStep::ReplaceSpanText {
        block_id: block_id.clone(),
        guard,
        expect: None,
        span: ResolvedSpanSelector::Whole, // whole paragraph incl. the field
        content: text_content("totally new text with no field"),
        rationale: None,
    };
    let err = apply_transaction(
        &doc.snapshot().canonical,
        &EditTransaction {
            steps: vec![step],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: revision(),
        },
    )
    .expect_err("dropping an in-span opaque must fail");
    assert!(
        matches!(err, EditError::OpaqueDestroyed { .. }),
        "expected OpaqueDestroyed, got {err:?}"
    );
}

#[test]
fn opaque_outside_the_targeted_span_survives() {
    // Target only the leading text span; the field (outside the span) is carried
    // by-ref and survives — never dropped.
    let body = r#"<w:p><w:r><w:t xml:space="preserve">before </w:t></w:r><w:fldSimple w:instr=" REF A \h "><w:r><w:t>X</w:t></w:r></w:fldSimple><w:r><w:t xml:space="preserve"> after</w:t></w:r></w:p>"#;
    let doc = Document::parse(&make_docx_with_body(body)).expect("parse");
    let (block_id, guard) = first_block_id_and_guard(&doc);
    let field_id = NodeId::from(opaque_ids(&doc, &block_id)[0].clone());

    // Span s_0 is the leading "before " text run; replace only it.
    let edited = apply_step(
        &doc,
        EditStep::ReplaceSpanText {
            block_id: block_id.clone(),
            guard,
            expect: None,
            span: ResolvedSpanSelector::Handle("s_0".to_string()),
            content: text_content("AFTER-EDIT "),
            rationale: None,
        },
    )
    .expect("editing a text span preserves the out-of-span field");

    // The field must still be present after the edit (accept-all).
    let accepted = edited.read_accepted().expect("accept");
    let survivors = opaque_ids(&accepted, &block_id);
    assert!(
        survivors.contains(&field_id.to_string()),
        "the out-of-span field must survive: {survivors:?}"
    );
}

// ─── Invariant M: one materializer ───────────────────────────────────────────

#[test]
fn span_replace_matches_equivalent_whole_paragraph_replace() {
    // A whole-span ReplaceSpanText and an equivalent ReplaceParagraphText that
    // produce the same NET text must yield byte-identical TrackedSegments — they
    // route through the SAME materializer (Invariant M).
    let doc = Document::parse(&plain_docx("The quick brown fox.")).expect("parse");
    let (block_id, guard) = first_block_id_and_guard(&doc);

    // Path A: whole-paragraph ReplaceParagraphText.
    let via_paragraph = apply_step(
        &doc,
        EditStep::ReplaceParagraphText {
            block_id: block_id.clone(),
            rationale: None,
            replacement_role: None,
            expect: "quick".to_string(),
            semantic_hash: Some(guard.clone()),
            content: text_content("The slow brown fox."),
        },
    )
    .expect("paragraph replace");

    // Path B: ReplaceSpanText over the WHOLE span, same net text.
    let via_span = apply_step(
        &doc,
        EditStep::ReplaceSpanText {
            block_id: block_id.clone(),
            guard,
            expect: None,
            span: ResolvedSpanSelector::Whole,
            content: text_content("The slow brown fox."),
            rationale: None,
        },
    )
    .expect("span replace");

    let seg_a = paragraph_segments_debug(&via_paragraph, &block_id);
    let seg_b = paragraph_segments_debug(&via_span, &block_id);
    assert_eq!(
        seg_a, seg_b,
        "span replace and whole-paragraph replace must produce identical tracked segments"
    );
}

// NOTE: span replace on a paragraph with existing tracked changes is no longer
// refused wholesale — the status-preserving splice layers a new tracked change
// beside existing ones (carrying them through untouched) and refuses only when
// the targeted RANGE overlaps a tracked segment. That contract — including the
// flipped layer-beside regression that replaced the old fails-loud test here —
// lives in `spec_span_splice.rs`.

/// A structural fingerprint of a paragraph's tracked segments (status + text),
/// for the Invariant-M equality check.
fn paragraph_segments_debug(doc: &Document, block_id: &NodeId) -> Vec<(String, String)> {
    for tb in &doc.snapshot().canonical.blocks {
        if let BlockNode::Paragraph(p) = &tb.block
            && &p.id == block_id
        {
            return p
                .segments
                .iter()
                .map(|seg| {
                    let status = match &seg.status {
                        TrackingStatus::Normal => "normal".to_string(),
                        TrackingStatus::Inserted(_) => "inserted".to_string(),
                        TrackingStatus::Deleted(_) => "deleted".to_string(),
                        TrackingStatus::InsertedThenDeleted(_) => {
                            "inserted_then_deleted".to_string()
                        }
                    };
                    let mut text = String::new();
                    for inline in &seg.inlines {
                        if let InlineNode::Text(t) = inline {
                            text.push_str(&t.text);
                        }
                    }
                    (status, text)
                })
                .collect();
        }
    }
    panic!("block not found");
}

// ─── Wire path: span on a non-paragraph payload is rejected ──────────────────

#[test]
fn span_on_non_paragraph_payload_is_rejected_at_schema() {
    // A span selector on a table replace payload must be rejected by the schema
    // layer (SpanOnNonParagraph), never silently ignored.
    let json = r#"{
      "ops": [
        { "op": "replace", "target": "t_1", "span": "s_0",
          "content": { "type": "table", "content": [
            { "content": [ { "content": [
              { "type": "paragraph", "content": [ { "type": "text", "text": "x" } ] }
            ] } ] }
          ] } }
      ],
      "revision": { "author": "Counsel" }
    }"#;
    let err = parse_transaction(json).expect_err("span on table must be rejected");
    assert!(
        matches!(err, stemma::edit_v4::SchemaError::SpanOnNonParagraph { .. }),
        "expected SpanOnNonParagraph, got {err:?}"
    );
}
