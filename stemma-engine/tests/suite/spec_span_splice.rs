//! The status-preserving splice: range contract + layer-beside behavior.
//!
//! The status-preserving splice: every inline text edit is a **local,
//! attributed splice** against stable identities. A splice over `[start, end)`
//! splits the boundary segments, materializes the change as new
//! `Inserted`/`Deleted` segments inside the range, and carries every other
//! segment through untouched — a neighbour's tracked change survives
//! *structurally*, not by policy.
//!
//! The range contract is pinned here as refusal tests:
//!   - **Status**: the targeted range must be all-`Normal`; a range overlapping
//!     a tracked segment refuses (`SpanCrossesTrackedSegment`).
//!   - **Walls**: opaques/hard-breaks in range are carried by reference
//!     (covered by `spec_span_addressing.rs`'s `OpaqueDestroyed` tests).
//!   - **Brackets**: the splice boundary must not split a paired range marker
//!     (bookmarkStart/End etc.); refusals name the bracket kind.
//!   - **Text identity**: when the op carries `expect`, the resolved range's
//!     visible text must equal it (`SpanTextMismatch` → stale_edit).
//!   - The block guard is **mandatory** on the wire (`SpanRequiresGuard`).
//!
//! Plus the structural invariant: a splice leaves the concatenated
//! `(char, status)` stream and wall inventory of the non-targeted regions
//! byte-identical.
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

/// "Preferred Stock means the <ins by Stemma>standard</ins> shares." — the
/// hard case: a paragraph already carrying a reviewer's tracked insertion.
fn tracked_ins_body() -> &'static str {
    r#"<w:p><w:r><w:t xml:space="preserve">Preferred Stock means the </w:t></w:r><w:ins w:id="1" w:author="Stemma" w:date="2026-01-01T00:00:00Z"><w:r><w:t>standard</w:t></w:r></w:ins><w:r><w:t xml:space="preserve"> shares.</w:t></w:r></w:p>"#
}

fn revision() -> RevisionInfo {
    RevisionInfo {
        revision_id: 9,
        identity: 0,
        author: Some("span-test".to_string()),
        date: Some("2026-06-09T00:00:00Z".to_string()),
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

fn apply_step(doc: &Document, step: EditStep) -> Result<Document, stemma::RuntimeError> {
    apply_steps(doc, vec![step], MaterializationMode::TrackedChange)
}

/// `expect_err` needs `Document: Debug`; this is the same assertion without it.
fn expect_refusal(
    result: Result<Document, stemma::RuntimeError>,
    what: &str,
) -> stemma::RuntimeError {
    match result {
        Ok(_) => panic!("{what}: expected a refusal but the edit applied"),
        Err(e) => e,
    }
}

fn apply_steps(
    doc: &Document,
    steps: Vec<EditStep>,
    mode: MaterializationMode,
) -> Result<Document, stemma::RuntimeError> {
    doc.apply(&EditTransaction {
        steps,
        summary: None,
        materialization_mode: mode,
        revision: revision(),
    })
}

/// The handle of the first span whose text equals `text` (any status).
fn handle_of_span(doc: &Document, text: &str) -> String {
    let view = doc.read();
    view.blocks[0]
        .segments
        .iter()
        .find_map(|s| match s {
            stemma::view::SegmentView::Text {
                text: t, handle, ..
            } if t == text => handle.clone(),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no span with text {text:?}"))
        .0
}

/// The paragraph's segments as `(status, author-or-"", text)` triples — the
/// domain-meaningful fingerprint (visible bytes + per-region status +
/// attribution), deliberately NOT node ids or segment boundaries.
fn segment_fingerprint(doc: &Document, block_id: &NodeId) -> Vec<(String, String, String)> {
    let para = find_paragraph(doc, block_id);
    para.segments
        .iter()
        .map(|seg| {
            let (status, author) = match &seg.status {
                TrackingStatus::Normal => ("normal".to_string(), String::new()),
                TrackingStatus::Inserted(r) => {
                    ("inserted".to_string(), r.author.clone().unwrap_or_default())
                }
                TrackingStatus::Deleted(r) => {
                    ("deleted".to_string(), r.author.clone().unwrap_or_default())
                }
                TrackingStatus::InsertedThenDeleted(sr) => (
                    format!(
                        "inserted_then_deleted(by {})",
                        sr.deleted.author.clone().unwrap_or_default()
                    ),
                    sr.inserted.author.clone().unwrap_or_default(),
                ),
            };
            let mut text = String::new();
            for inline in &seg.inlines {
                if let InlineNode::Text(t) = inline {
                    text.push_str(&t.text);
                }
            }
            (status, author, text)
        })
        .collect()
}

fn find_paragraph<'a>(doc: &'a Document, block_id: &NodeId) -> &'a stemma::ParagraphNode {
    for tb in &doc.snapshot().canonical.blocks {
        if let BlockNode::Paragraph(p) = &tb.block
            && &p.id == block_id
        {
            return p;
        }
    }
    panic!("block not found");
}

/// The concatenated `(char, status-label)` stream of a paragraph's visible
/// text, in document order — the structural-invariant comparison basis
/// (segmentation-insensitive by construction).
fn char_status_stream(doc: &Document, block_id: &NodeId) -> Vec<(char, String)> {
    let para = find_paragraph(doc, block_id);
    let mut out = Vec::new();
    for seg in &para.segments {
        let status = match &seg.status {
            TrackingStatus::Normal => "normal".to_string(),
            TrackingStatus::Inserted(r) => {
                format!("inserted:{}", r.author.clone().unwrap_or_default())
            }
            TrackingStatus::Deleted(r) => {
                format!("deleted:{}", r.author.clone().unwrap_or_default())
            }
            TrackingStatus::InsertedThenDeleted(sr) => format!(
                "stacked:{}+{}",
                sr.inserted.author.clone().unwrap_or_default(),
                sr.deleted.author.clone().unwrap_or_default()
            ),
        };
        for inline in &seg.inlines {
            if let InlineNode::Text(t) = inline {
                for ch in t.text.chars() {
                    out.push((ch, status.clone()));
                }
            }
        }
    }
    out
}

/// Opaque-inline ids in document order (the wall inventory).
fn wall_inventory(doc: &Document, block_id: &NodeId) -> Vec<String> {
    let para = find_paragraph(doc, block_id);
    let mut out = Vec::new();
    for seg in &para.segments {
        for inline in &seg.inlines {
            match inline {
                InlineNode::OpaqueInline(o) => out.push(o.id.to_string()),
                InlineNode::HardBreak(hb) => out.push(hb.id.to_string()),
                _ => {}
            }
        }
    }
    out
}

/// Count of zero-width decoration markers (the bracket inventory size).
fn decoration_count(doc: &Document, block_id: &NodeId) -> usize {
    let para = find_paragraph(doc, block_id);
    para.segments
        .iter()
        .flat_map(|s| s.inlines.iter())
        .filter(|i| matches!(i, InlineNode::Decoration(_)))
        .count()
}

// ─── Layer beside: the core workflow the splice unblocks ────────────────────

#[test]
fn splice_layers_a_tracked_change_beside_an_existing_insertion() {
    // The hard case, now the POSITIVE case: replace the leading Normal span of
    // a paragraph that already carries Stemma's tracked insertion. The splice
    // must (a) apply the edit as del/ins inside the targeted range only, and
    // (b) carry Stemma's insertion through untouched — same status, same
    // author, same text.
    let doc = Document::parse(&make_docx_with_body(tracked_ins_body())).expect("parse");
    let (block_id, guard) = first_block_id_and_guard(&doc);
    let lead_handle = handle_of_span(&doc, "Preferred Stock means the ");

    let edited = apply_step(
        &doc,
        EditStep::ReplaceSpanText {
            block_id: block_id.clone(),
            guard,
            expect: None,
            span: ResolvedSpanSelector::Handle(lead_handle),
            content: text_content("Standard Preferred Stock means the "),
            rationale: None,
        },
    )
    .expect("the splice layers beside the existing tracked change");

    // Stemma's insertion survives untouched.
    let fp = segment_fingerprint(&edited, &block_id);
    assert!(
        fp.iter().any(|(status, author, text)| status == "inserted"
            && author == "Stemma"
            && text == "standard"),
        "the pre-existing tracked insertion must survive untouched: {fp:?}"
    );
    // The new change is attributed to the editing revision's author.
    assert!(
        fp.iter()
            .any(|(status, author, _)| status == "inserted" && author == "span-test"),
        "the new edit must be tracked under the editing author: {fp:?}"
    );

    // Accept-all reading: both changes land.
    let accepted = edited.read_accepted().expect("accept").read();
    assert_eq!(
        accepted.blocks[0].text,
        "Standard Preferred Stock means the standard shares."
    );
    // Reject-all reading: the document returns to its pre-anyone state.
    let rejected = edited.read_rejected().expect("reject").read();
    assert_eq!(
        rejected.blocks[0].text,
        "Preferred Stock means the  shares."
    );
}

#[test]
fn splice_layers_beside_an_existing_deletion() {
    // Same contract with a deletion tombstone as the neighbour: the Deleted
    // segment (and its attribution) must be carried through by reference.
    let body = r#"<w:p><w:r><w:t xml:space="preserve">Payment due in </w:t></w:r><w:del w:id="2" w:author="Reviewer" w:date="2026-01-01T00:00:00Z"><w:r><w:delText xml:space="preserve">thirty </w:delText></w:r></w:del><w:r><w:t xml:space="preserve">days.</w:t></w:r></w:p>"#;
    let doc = Document::parse(&make_docx_with_body(body)).expect("parse");
    let (block_id, guard) = first_block_id_and_guard(&doc);
    let tail_handle = handle_of_span(&doc, "days.");

    let edited = apply_step(
        &doc,
        EditStep::ReplaceSpanText {
            block_id: block_id.clone(),
            guard,
            expect: None,
            span: ResolvedSpanSelector::Handle(tail_handle),
            content: text_content("business days."),
            rationale: None,
        },
    )
    .expect("the splice layers beside the existing deletion");

    let fp = segment_fingerprint(&edited, &block_id);
    assert!(
        fp.iter().any(|(status, author, text)| status == "deleted"
            && author == "Reviewer"
            && text == "thirty "),
        "the pre-existing tracked deletion must survive untouched: {fp:?}"
    );
    let accepted = edited.read_accepted().expect("accept").read();
    assert_eq!(accepted.blocks[0].text, "Payment due in business days.");
    let rejected = edited.read_rejected().expect("reject").read();
    assert_eq!(rejected.blocks[0].text, "Payment due in thirty days.");
}

#[test]
fn splice_inserts_at_an_anchor_boundary_next_to_a_tracked_segment() {
    // An empty-range insertion (anchor-after) whose insertion point touches a
    // tracked segment's boundary overlaps nothing, so the status predicate
    // admits it: the new Inserted segment lands between the anchor and the
    // existing insertion, which is carried through untouched.
    let body = r#"<w:p><w:r><w:t xml:space="preserve">see </w:t></w:r><w:fldSimple w:instr=" REF A \h "><w:r><w:t>X</w:t></w:r></w:fldSimple><w:ins w:id="1" w:author="Stemma" w:date="2026-01-01T00:00:00Z"><w:r><w:t xml:space="preserve"> appendix</w:t></w:r></w:ins><w:r><w:t xml:space="preserve"> end.</w:t></w:r></w:p>"#;
    let doc = Document::parse(&make_docx_with_body(body)).expect("parse");
    let (block_id, guard) = first_block_id_and_guard(&doc);
    let field_id = NodeId::from(wall_inventory(&doc, &block_id)[0].clone());

    let edited = apply_step(
        &doc,
        EditStep::ReplaceSpanText {
            block_id: block_id.clone(),
            guard,
            expect: None,
            span: ResolvedSpanSelector::AnchorAfter(field_id),
            content: text_content(" (as defined)"),
            rationale: None,
        },
    )
    .expect("empty-range insertion at a tracked-segment boundary applies");

    let fp = segment_fingerprint(&edited, &block_id);
    assert!(
        fp.iter().any(|(status, author, text)| status == "inserted"
            && author == "Stemma"
            && text == " appendix"),
        "the pre-existing insertion must survive untouched: {fp:?}"
    );
    let accepted = edited.read_accepted().expect("accept").read();
    assert_eq!(accepted.blocks[0].text, "see  (as defined) appendix end.");
}

#[test]
fn direct_mode_splice_does_not_resolve_neighbouring_tracked_changes() {
    // Direct mode materializes ONLY the new edit as resolved (Normal) content.
    // It must not accept/reject anyone else's pending tracked changes as a
    // side effect — that would silently resolve a reviewer's proposal.
    let doc = Document::parse(&make_docx_with_body(tracked_ins_body())).expect("parse");
    let (block_id, guard) = first_block_id_and_guard(&doc);
    let lead_handle = handle_of_span(&doc, "Preferred Stock means the ");

    let edited = apply_steps(
        &doc,
        vec![EditStep::ReplaceSpanText {
            block_id: block_id.clone(),
            guard,
            expect: None,
            span: ResolvedSpanSelector::Handle(lead_handle),
            content: text_content("Standard Preferred Stock means the "),
            rationale: None,
        }],
        MaterializationMode::Direct,
    )
    .expect("direct-mode splice applies");

    let fp = segment_fingerprint(&edited, &block_id);
    // The new text is resolved (Normal), the neighbour's insertion still tracked.
    assert!(
        fp.iter().any(|(status, author, text)| status == "inserted"
            && author == "Stemma"
            && text == "standard"),
        "direct mode must not resolve the neighbour's tracked change: {fp:?}"
    );
    assert!(
        !fp.iter().any(|(_, author, _)| author == "span-test"),
        "direct mode leaves no tracked change of its own: {fp:?}"
    );
    let accepted = edited.read_accepted().expect("accept").read();
    assert_eq!(
        accepted.blocks[0].text,
        "Standard Preferred Stock means the standard shares."
    );
}

// ─── Status: the targeted range must be all-Normal ──────────────────────────

#[test]
fn splice_refuses_a_range_overlapping_a_pending_deletion() {
    // A range overlapping a Deleted segment still refuses: the text is
    // already struck, and rewriting it has no tracked semantics (Word
    // refuses typing into deleted text too). Pending INSERTIONS, by
    // contrast, are editable — same-author edits in place, cross-author
    // edits stack (see the stacking tests below).
    let body = r#"<w:p><w:r><w:t xml:space="preserve">Payment due in </w:t></w:r><w:del w:id="2" w:author="Reviewer" w:date="2026-01-01T00:00:00Z"><w:r><w:delText xml:space="preserve">thirty </w:delText></w:r></w:del><w:r><w:t xml:space="preserve">days.</w:t></w:r></w:p>"#;
    let doc = Document::parse(&make_docx_with_body(body)).expect("parse");
    let (block_id, guard) = first_block_id_and_guard(&doc);

    let step = EditStep::ReplaceSpanText {
        block_id,
        guard,
        expect: None,
        span: ResolvedSpanSelector::Whole,
        content: text_content("flattened rewrite"),
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
    .expect_err("a range overlapping a pending deletion must refuse");
    assert!(
        matches!(err, EditError::SpanCrossesTrackedSegment { .. }),
        "expected SpanCrossesTrackedSegment, got {err:?}"
    );
}

#[test]
fn cross_author_replace_inside_anothers_insertion_stacks() {
    // Step 3a, the edit that PRODUCES the stacked state: span-test replaces
    // the text of Stemma's pending insertion. The removed text is NOT
    // un-proposed (it is not span-test's to retract) and NOT tombstoned (it
    // never existed in the base): it becomes InsertedThenDeleted — both
    // revisions pending, resolution by the four origin rules.
    let doc = Document::parse(&make_docx_with_body(tracked_ins_body())).expect("parse");
    let (block_id, guard) = first_block_id_and_guard(&doc);
    let ins_handle = handle_of_span(&doc, "standard");

    let edited = apply_step(
        &doc,
        EditStep::ReplaceSpanText {
            block_id: block_id.clone(),
            guard,
            expect: Some("standard".to_string()),
            span: ResolvedSpanSelector::Handle(ins_handle),
            content: text_content("bespoke"),
            rationale: None,
        },
    )
    .expect("a cross-author edit inside a pending insertion stacks");

    let para = find_paragraph(&edited, &block_id);
    let mut saw_stacked = false;
    let mut saw_new_ins = false;
    for seg in &para.segments {
        match &seg.status {
            stemma::TrackingStatus::InsertedThenDeleted(sr) => {
                assert_eq!(sr.inserted.author.as_deref(), Some("Stemma"));
                assert_eq!(sr.deleted.author.as_deref(), Some("span-test"));
                saw_stacked = true;
            }
            stemma::TrackingStatus::Inserted(r) if r.author.as_deref() == Some("span-test") => {
                saw_new_ins = true;
            }
            _ => {}
        }
    }
    assert!(
        saw_stacked,
        "the removed text stacks with both attributions"
    );
    assert!(saw_new_ins, "the replacement text is span-test's insertion");

    // Accept-all: the new wording lands; the stacked text drops.
    let accepted = edited.read_accepted().expect("accept").read();
    assert_eq!(
        accepted.blocks[0].text,
        "Preferred Stock means the bespoke shares."
    );
    // Reject-all: the pre-anyone base.
    let rejected = edited.read_rejected().expect("reject").read();
    assert_eq!(
        rejected.blocks[0].text,
        "Preferred Stock means the  shares."
    );
}

// ─── Text identity: expect re-asserted against the resolved span ────────────

#[test]
fn splice_reasserts_the_resolved_span_text_when_expect_is_present() {
    // The guard is deliberately segmentation-insensitive while a handle is an
    // ordinal over the segmentation — `expect` closes that hole by re-asserting
    // the resolved range's visible text. Mismatch refuses as stale_edit.
    let doc = Document::parse(&make_docx_with_body(tracked_ins_body())).expect("parse");
    let (block_id, guard) = first_block_id_and_guard(&doc);
    let lead_handle = handle_of_span(&doc, "Preferred Stock means the ");

    let err = apply_step(
        &doc,
        EditStep::ReplaceSpanText {
            block_id: block_id.clone(),
            guard: guard.clone(),
            expect: Some("some other text the reader never saw".to_string()),
            span: ResolvedSpanSelector::Handle(lead_handle.clone()),
            content: text_content("Standard Preferred Stock means the "),
            rationale: None,
        },
    );
    let err = expect_refusal(err, "an expect mismatch must refuse");
    assert_eq!(err.code, stemma::ErrorCode::StaleEdit, "{err:?}");

    // With the text the reader actually saw, the same edit applies.
    apply_step(
        &doc,
        EditStep::ReplaceSpanText {
            block_id,
            guard,
            expect: Some("Preferred Stock means the ".to_string()),
            span: ResolvedSpanSelector::Handle(lead_handle),
            content: text_content("Standard Preferred Stock means the "),
            rationale: None,
        },
    )
    .expect("a matching expect applies");
}

// ─── Brackets: never split a paired range marker ────────────────────────────

/// alpha [bookmarkStart] **beta** gamma [bookmarkEnd] — the bold run breaks the
/// text into three spans, so s_0 contains the start marker and s_2 the end.
fn bookmark_pair_body() -> &'static str {
    r#"<w:p><w:r><w:t xml:space="preserve">alpha </w:t></w:r><w:bookmarkStart w:id="3" w:name="ref_target"/><w:r><w:rPr><w:b/></w:rPr><w:t>beta</w:t></w:r><w:r><w:t xml:space="preserve"> gamma</w:t></w:r><w:bookmarkEnd w:id="3"/></w:p>"#
}

#[test]
fn splice_refuses_to_split_a_bookmark_pair() {
    // s_0 ("alpha ") contains the bookmarkStart; its bookmarkEnd lies outside
    // the range. Replacing s_0 would detach the start marker from the text the
    // bookmark covers — a REF field pointing at it would silently change
    // meaning. Refuse, naming the bracket kind.
    let doc = Document::parse(&make_docx_with_body(bookmark_pair_body())).expect("parse");
    let (block_id, guard) = first_block_id_and_guard(&doc);
    let lead_handle = handle_of_span(&doc, "alpha ");

    let err = apply_step(
        &doc,
        EditStep::ReplaceSpanText {
            block_id,
            guard,
            expect: None,
            span: ResolvedSpanSelector::Handle(lead_handle),
            content: text_content("ALPHA "),
            rationale: None,
        },
    );
    let err = expect_refusal(
        err,
        "a range containing one member of a bookmark pair must refuse",
    );
    assert_eq!(err.code, stemma::ErrorCode::UnsupportedEdit, "{err:?}");
    assert!(
        err.message.contains("bookmark"),
        "the refusal must name the bracket kind that blocked it: {}",
        err.message
    );
}

#[test]
fn splice_between_a_bracket_pair_applies_and_preserves_both_markers() {
    // s_1 ("beta") lies strictly between the bookmark markers — no pair member
    // is in range, so the splice applies and both markers survive.
    let doc = Document::parse(&make_docx_with_body(bookmark_pair_body())).expect("parse");
    let (block_id, guard) = first_block_id_and_guard(&doc);
    assert_eq!(decoration_count(&doc, &block_id), 2, "fixture sanity");
    let mid_handle = handle_of_span(&doc, "beta");

    let edited = apply_step(
        &doc,
        EditStep::ReplaceSpanText {
            block_id: block_id.clone(),
            guard,
            expect: None,
            span: ResolvedSpanSelector::Handle(mid_handle),
            content: text_content("BETA"),
            rationale: None,
        },
    )
    .expect("a range between a bracket pair applies");

    assert_eq!(
        decoration_count(&edited, &block_id),
        2,
        "both bookmark markers must survive the splice"
    );
    let accepted = edited.read_accepted().expect("accept").read();
    assert_eq!(accepted.blocks[0].text, "alpha BETA gamma");
}

/// alpha [customXml> **beta** gamma </customXml] delta — the transparent
/// customXml wrapper brackets the bold "beta" run AND the " gamma" run, so its
/// start marker sits after "alpha " and its end marker after " gamma" (mirroring
/// the bookmark fixture, whose pair brackets "beta gamma"). The wrapper is
/// imported as a CustomXmlWrapper Decoration pair (task #6), so the bracket
/// guard must protect its extent like a bookmark. The leading "alpha " span
/// captures the wrapper START (partner outside → split → refuse); the inner
/// "beta" span lies strictly between the markers (no pair member in range →
/// applies).
fn customxml_wrapper_pair_body() -> &'static str {
    r#"<w:p><w:r><w:t xml:space="preserve">alpha </w:t></w:r><w:customXml w:uri="urn:x" w:element="e"><w:r><w:rPr><w:b/></w:rPr><w:t>beta</w:t></w:r><w:r><w:t xml:space="preserve"> gamma</w:t></w:r></w:customXml><w:r><w:t xml:space="preserve"> delta</w:t></w:r></w:p>"#
}

#[test]
fn splice_refuses_to_split_a_custom_xml_wrapper_pair() {
    // s_0 ("alpha ") captures the customXml wrapper START marker; its END marker
    // lies outside the range (after "beta"). Replacing s_0 would detach the
    // wrapper's open marker from the content it wraps — a torn wrapper. The
    // marker-pair model makes that constructible, so the bracket guard must
    // refuse, naming the bracket kind (task #6, Rider 2).
    let doc = Document::parse(&make_docx_with_body(customxml_wrapper_pair_body())).expect("parse");
    let (block_id, guard) = first_block_id_and_guard(&doc);
    let lead_handle = handle_of_span(&doc, "alpha ");

    let err = apply_step(
        &doc,
        EditStep::ReplaceSpanText {
            block_id,
            guard,
            expect: None,
            span: ResolvedSpanSelector::Handle(lead_handle),
            content: text_content("ALPHA "),
            rationale: None,
        },
    );
    let err = expect_refusal(
        err,
        "a range containing one member of a customXml wrapper pair must refuse",
    );
    assert_eq!(err.code, stemma::ErrorCode::UnsupportedEdit, "{err:?}");
    assert!(
        err.message.contains("customXml"),
        "the refusal must name the bracket kind that blocked it: {}",
        err.message
    );
}

#[test]
fn splice_between_a_custom_xml_wrapper_pair_applies_and_preserves_both_markers() {
    // s_1 ("beta") lies strictly between the wrapper markers — no pair member is
    // in range, so the splice applies and both markers (the wrapper) survive.
    let doc = Document::parse(&make_docx_with_body(customxml_wrapper_pair_body())).expect("parse");
    let (block_id, guard) = first_block_id_and_guard(&doc);
    assert_eq!(decoration_count(&doc, &block_id), 2, "fixture sanity");
    let mid_handle = handle_of_span(&doc, "beta");

    let edited = apply_step(
        &doc,
        EditStep::ReplaceSpanText {
            block_id: block_id.clone(),
            guard,
            expect: None,
            span: ResolvedSpanSelector::Handle(mid_handle),
            content: text_content("BETA"),
            rationale: None,
        },
    )
    .expect("a range between a wrapper pair applies");

    assert_eq!(
        decoration_count(&edited, &block_id),
        2,
        "both customXml wrapper markers must survive the splice"
    );
    let accepted = edited.read_accepted().expect("accept").read();
    assert_eq!(accepted.blocks[0].text, "alpha BETA gamma delta");
}

// ─── Wire contract: the guard is mandatory for span ops ─────────────────────

#[test]
fn span_op_without_a_guard_is_rejected_at_schema() {
    // The splice op REQUIRES the block guard — it is both the staleness
    // gate that makes the ephemeral handle safe and the mechanism that refuses
    // compound same-paragraph edits. A span op without one is malformed.
    let json = r#"{
      "ops": [
        { "op": "replace", "target": "p_1", "span": "s_0",
          "content": { "type": "paragraph",
                       "content": [ { "type": "text", "text": "x" } ] } }
      ],
      "revision": { "author": "Counsel" }
    }"#;
    let err = parse_transaction(json).expect_err("span op without guard must be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("guard"),
        "the schema error must point at the missing guard: {msg}"
    );

    // The same op WITH a guard passes the schema layer.
    let json_with_guard = r#"{
      "ops": [
        { "op": "replace", "target": "p_1", "span": "s_0", "guard": "abc123",
          "content": { "type": "paragraph",
                       "content": [ { "type": "text", "text": "x" } ] } }
      ],
      "revision": { "author": "Counsel" }
    }"#;
    parse_transaction(json_with_guard).expect("span op with guard parses");
}

// ─── Compound edits to the same paragraph: refused by the guard ─────────────

#[test]
fn compound_edits_to_the_same_paragraph_are_refused_by_the_stale_guard() {
    // Two span ops on the SAME paragraph in one transaction: op 1's splice
    // moves the block's semantic hash, so op 2's guard (minted from the same
    // pre-transaction read) mismatches and the WHOLE transaction refuses —
    // atomicity means no partial application.
    let body = r#"<w:p><w:r><w:t xml:space="preserve">The term is </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>30 days</w:t></w:r><w:r><w:t xml:space="preserve"> total.</w:t></w:r></w:p>"#;
    let doc = Document::parse(&make_docx_with_body(body)).expect("parse");
    let (block_id, guard) = first_block_id_and_guard(&doc);
    let lead = handle_of_span(&doc, "The term is ");
    let bold = handle_of_span(&doc, "30 days");

    let err = apply_steps(
        &doc,
        vec![
            EditStep::ReplaceSpanText {
                block_id: block_id.clone(),
                guard: guard.clone(),
                expect: None,
                span: ResolvedSpanSelector::Handle(lead),
                content: text_content("The notice period is "),
                rationale: None,
            },
            EditStep::ReplaceSpanText {
                block_id: block_id.clone(),
                guard,
                expect: None,
                span: ResolvedSpanSelector::Handle(bold),
                content: text_content("60 days"),
                rationale: None,
            },
        ],
        MaterializationMode::TrackedChange,
    );
    let err = expect_refusal(err, "the second same-paragraph op must hit the stale guard");
    assert_eq!(err.code, stemma::ErrorCode::StaleEdit, "{err:?}");

    // Atomicity: the document is unchanged (op 1 did not partially apply).
    let unchanged = doc.read();
    assert_eq!(unchanged.blocks[0].text, "The term is 30 days total.");
}

// ─── The structural invariant ────────────────────────────────────────────────

#[test]
fn splice_leaves_non_targeted_regions_byte_and_status_identical() {
    // A paragraph with an existing insertion, a field, and an existing deletion.
    // Splicing the middle Normal span must leave the concatenated (char, status)
    // stream of everything outside the targeted range — and the wall inventory —
    // exactly as it was. Compared as streams, NOT segment-by-segment: the
    // normalize pass may legitimately re-draw segment boundaries.
    let body = r#"<w:p><w:r><w:t xml:space="preserve">Alpha </w:t></w:r><w:ins w:id="1" w:author="Stemma" w:date="2026-01-01T00:00:00Z"><w:r><w:t xml:space="preserve">NEW </w:t></w:r></w:ins><w:r><w:t xml:space="preserve">bravo charlie</w:t></w:r><w:fldSimple w:instr=" REF A \h "><w:r><w:t>X</w:t></w:r></w:fldSimple><w:r><w:t xml:space="preserve"> delta</w:t></w:r><w:del w:id="2" w:author="Reviewer" w:date="2026-01-01T00:00:00Z"><w:r><w:delText>OLD</w:delText></w:r></w:del></w:p>"#;
    let doc = Document::parse(&make_docx_with_body(body)).expect("parse");
    let (block_id, guard) = first_block_id_and_guard(&doc);

    let before_stream = char_status_stream(&doc, &block_id);
    let before_walls = wall_inventory(&doc, &block_id);
    let target = "bravo charlie";
    let target_handle = handle_of_span(&doc, target);

    // The targeted region's chars, located inside the before-stream.
    let target_start = {
        let chars: Vec<char> = before_stream.iter().map(|(c, _)| *c).collect();
        let text: String = chars.iter().collect();
        let byte_pos = text.find(target).expect("target text present");
        text[..byte_pos].chars().count()
    };
    let head = &before_stream[..target_start];
    let tail = &before_stream[target_start + target.chars().count()..];

    let edited = apply_step(
        &doc,
        EditStep::ReplaceSpanText {
            block_id: block_id.clone(),
            guard,
            expect: Some(target.to_string()),
            span: ResolvedSpanSelector::Handle(target_handle),
            content: text_content("BRAVO"),
            rationale: None,
        },
    )
    .expect("splice applies");

    let after_stream = char_status_stream(&edited, &block_id);
    assert!(
        after_stream.starts_with(head),
        "the (char, status) stream BEFORE the targeted range must be untouched\nbefore: {head:?}\nafter head: {:?}",
        &after_stream[..head.len().min(after_stream.len())]
    );
    assert!(
        after_stream.ends_with(tail),
        "the (char, status) stream AFTER the targeted range must be untouched\nbefore: {tail:?}\nafter tail: {:?}",
        &after_stream[after_stream.len().saturating_sub(tail.len())..]
    );
    assert_eq!(
        wall_inventory(&edited, &block_id),
        before_walls,
        "the wall inventory must be unchanged by a splice that doesn't target it"
    );
}

// ─── Same-author in-place editing of a pending insertion ────────────────────
//
// Domain rule: an author may edit THEIR OWN pending insertion through the same
// splice. Output status is a function of each character's ORIGIN:
//   - kept text originating in the own insertion stays Inserted under the
//     ORIGINAL revision (the pending change keeps its identity);
//   - text removed from the own insertion is dropped outright — NO tombstone,
//     because it never existed in the base, and reject-all must reproduce the
//     base exactly;
//   - new text is Inserted under the editing revision.
// Anyone else's tracked content, and any pending deletion (own or not), still
// refuses. Direct mode keeps the step-1 rule (all-Normal ranges only): an
// untracked edit must never silently resolve a pending insertion.

/// "Base " + <ins by span-test (the test author)>draft text here</ins> + " end."
/// The insertion's date differs from the editing revision's date so kept text
/// is distinguishable from newly inserted text.
fn own_ins_body() -> &'static str {
    r#"<w:p><w:r><w:t xml:space="preserve">Base </w:t></w:r><w:ins w:id="1" w:author="span-test" w:date="2026-01-01T00:00:00Z"><w:r><w:t>draft text here</w:t></w:r></w:ins><w:r><w:t xml:space="preserve"> end.</w:t></w:r></w:p>"#
}

#[test]
fn same_author_edits_their_own_pending_insertion_in_place() {
    // Replace "draft text here" -> "draft wording here" inside one's own
    // pending insertion: "draft " and " here" stay Inserted under the ORIGINAL
    // revision date, "text" vanishes without a tombstone, "wording" arrives
    // Inserted under the editing revision. No nested markup, no w:del.
    let doc = Document::parse(&make_docx_with_body(own_ins_body())).expect("parse");
    let (block_id, guard) = first_block_id_and_guard(&doc);
    let ins_handle = handle_of_span(&doc, "draft text here");

    let edited = apply_step(
        &doc,
        EditStep::ReplaceSpanText {
            block_id: block_id.clone(),
            guard,
            expect: Some("draft text here".to_string()),
            span: ResolvedSpanSelector::Handle(ins_handle),
            content: text_content("draft wording here"),
            rationale: None,
        },
    )
    .expect("an author may edit their own pending insertion");

    // No deletion tombstone anywhere: the removed word never existed in the base.
    let fp = segment_fingerprint(&edited, &block_id);
    assert!(
        !fp.iter().any(|(status, _, _)| status == "deleted"),
        "editing one's own insertion must not produce a w:del tombstone: {fp:?}"
    );
    // Kept insertion text retains the ORIGINAL revision date; new text carries
    // the editing revision's date.
    let para = find_paragraph(&edited, &block_id);
    let mut kept_date = None;
    let mut new_date = None;
    for seg in &para.segments {
        if let TrackingStatus::Inserted(rev) = &seg.status {
            let text: String = seg
                .inlines
                .iter()
                .filter_map(|i| match i {
                    InlineNode::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
                .collect();
            if text.contains("draft") || text.contains("here") {
                kept_date = rev.date.clone();
            }
            if text.contains("wording") {
                new_date = rev.date.clone();
            }
        }
    }
    assert_eq!(
        kept_date.as_deref(),
        Some("2026-01-01T00:00:00Z"),
        "kept own-insertion text must retain the original revision identity"
    );
    assert_eq!(
        new_date.as_deref(),
        Some("2026-06-09T00:00:00Z"),
        "new text must carry the editing revision"
    );

    // Accept-all: the edited insertion lands. Reject-all: the base, exactly —
    // including no trace of either "text" or "wording".
    let accepted = edited.read_accepted().expect("accept").read();
    assert_eq!(accepted.blocks[0].text, "Base draft wording here end.");
    let rejected = edited.read_rejected().expect("reject").read();
    assert_eq!(rejected.blocks[0].text, "Base  end.");
}

#[test]
fn mixed_range_maps_status_by_origin() {
    // A Whole-paragraph range spanning Normal text AND one's own insertion:
    // deleted Normal-origin text gets a tombstone, deleted own-ins text is
    // dropped outright, and both resolutions stay exact.
    let body = r#"<w:p><w:r><w:t xml:space="preserve">Alpha </w:t></w:r><w:ins w:id="1" w:author="span-test" w:date="2026-01-01T00:00:00Z"><w:r><w:t xml:space="preserve">beta </w:t></w:r></w:ins><w:r><w:t xml:space="preserve">gamma.</w:t></w:r></w:p>"#;
    let doc = Document::parse(&make_docx_with_body(body)).expect("parse");
    let (block_id, guard) = first_block_id_and_guard(&doc);

    let edited = apply_step(
        &doc,
        EditStep::ReplaceSpanText {
            block_id: block_id.clone(),
            guard,
            expect: Some("Alpha beta gamma.".to_string()),
            span: ResolvedSpanSelector::Whole,
            content: text_content("delta gamma."),
            rationale: None,
        },
    )
    .expect("a mixed Normal + own-insertion range applies");

    let fp = segment_fingerprint(&edited, &block_id);
    // The deleted Normal-origin text ("Alpha ") gets a tombstone…
    assert!(
        fp.iter()
            .any(|(status, _, text)| status == "deleted" && text.contains("Alpha")),
        "deleted Normal-origin text needs a w:del tombstone: {fp:?}"
    );
    // …while the deleted own-insertion text ("beta ") vanishes entirely.
    assert!(
        !fp.iter().any(|(_, _, text)| text.contains("beta")),
        "deleted own-insertion text must vanish without a tombstone: {fp:?}"
    );

    let accepted = edited.read_accepted().expect("accept").read();
    assert_eq!(accepted.blocks[0].text, "delta gamma.");
    let rejected = edited.read_rejected().expect("reject").read();
    assert_eq!(rejected.blocks[0].text, "Alpha gamma.");
}

#[test]
fn anonymous_insertions_stack_rather_than_unpropose() {
    // D7: author identity is exact byte equality and an anonymous revision
    // never matches — the engine must never un-propose text it cannot prove
    // is the editing author's. Deleting text inside an anonymous insertion
    // therefore STACKS (conservative), never silently drops.
    let body = r#"<w:p><w:r><w:t xml:space="preserve">Lead </w:t></w:r><w:ins w:id="1" w:author="" w:date="2026-01-01T00:00:00Z"><w:r><w:t>anon</w:t></w:r></w:ins><w:r><w:t xml:space="preserve"> tail.</w:t></w:r></w:p>"#;
    let doc = Document::parse(&make_docx_with_body(body)).expect("parse");
    let (block_id, guard) = first_block_id_and_guard(&doc);
    let ins_handle = handle_of_span(&doc, "anon");

    let edited = apply_step(
        &doc,
        EditStep::ReplaceSpanText {
            block_id: block_id.clone(),
            guard,
            expect: None,
            span: ResolvedSpanSelector::Handle(ins_handle),
            content: ParagraphContent { fragments: vec![] },
            rationale: None,
        },
    )
    .expect("deleting inside an anonymous insertion stacks");
    let para = find_paragraph(&edited, &block_id);
    assert!(
        para.segments.iter().any(|seg| matches!(
            &seg.status,
            stemma::TrackingStatus::InsertedThenDeleted(sr)
                if sr.deleted.author.as_deref() == Some("span-test")
        )),
        "anonymous-author insertions stack rather than silently un-propose"
    );
}

#[test]
fn own_pending_deletion_still_refuses() {
    // A pending deletion's text is already struck; "editing" it has no
    // well-defined tracked semantics even for its own author (Word refuses
    // typing into deleted text too). Fail loud.
    let body = r#"<w:p><w:r><w:t xml:space="preserve">Keep </w:t></w:r><w:del w:id="2" w:author="span-test" w:date="2026-01-01T00:00:00Z"><w:r><w:delText>gone</w:delText></w:r></w:del><w:r><w:t xml:space="preserve"> tail.</w:t></w:r></w:p>"#;
    let doc = Document::parse(&make_docx_with_body(body)).expect("parse");
    let (block_id, guard) = first_block_id_and_guard(&doc);
    let del_handle = handle_of_span(&doc, "gone");

    let err = apply_step(
        &doc,
        EditStep::ReplaceSpanText {
            block_id,
            guard,
            expect: None,
            span: ResolvedSpanSelector::Handle(del_handle),
            content: text_content("changed"),
            rationale: None,
        },
    );
    let err = expect_refusal(err, "editing one's own pending deletion is refused");
    assert_eq!(err.code, stemma::ErrorCode::UnsupportedEdit, "{err:?}");
}

#[test]
fn direct_mode_still_refuses_a_range_over_own_insertion() {
    // Direct (untracked) materialization over one's own pending insertion
    // would have to either resolve it or rewrite it untracked — both silently
    // change the pending-change ledger. Step-1 rule stands in direct mode:
    // the range must be all-Normal.
    let doc = Document::parse(&make_docx_with_body(own_ins_body())).expect("parse");
    let (block_id, guard) = first_block_id_and_guard(&doc);
    let ins_handle = handle_of_span(&doc, "draft text here");

    let err = apply_steps(
        &doc,
        vec![EditStep::ReplaceSpanText {
            block_id,
            guard,
            expect: None,
            span: ResolvedSpanSelector::Handle(ins_handle),
            content: text_content("rewritten"),
            rationale: None,
        }],
        MaterializationMode::Direct,
    );
    let err = expect_refusal(err, "direct mode over an own insertion is refused");
    assert_eq!(err.code, stemma::ErrorCode::UnsupportedEdit, "{err:?}");
}
