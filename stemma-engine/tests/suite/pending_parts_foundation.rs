//! PendingParts foundation — inertness proof for the pure verb core.
//!
//! `apply_transaction` now returns `(CanonDoc, PendingParts)`. PendingParts is
//! the channel a future image-insert / style-create verb uses to stage OPC
//! parts for the save path. THIS COMMIT ships only the channel: no current verb
//! writes to it. These tests pin that invariant — every existing verb leaves
//! PendingParts empty — and re-assert the unchanged post-conditions
//! (reject-all == baseline, accept-all == target) under the new return type.
//!
//! The save-path consumer (`runtime::apply_pending_parts`) is exercised by the
//! private unit tests in `runtime.rs` (it operates on a `DocxPackage`, which is
//! not part of the public surface), including the fail-loud and
//! authored-style-wins-collision cases.

use stemma::domain::*;
use stemma::edit::*;
use stemma::{accept_all, reject_all_with_styles};

// ─── Minimal builders (mirror tests/edit_basic.rs) ───────────────────────────

fn make_text(id: &str, text: &str) -> InlineNode {
    InlineNode::from(TextNode {
        id: NodeId::from(id),
        text_role: None,
        text: text.to_string(),
        marks: Vec::new(),
        style_props: StyleProps::default(),
        rpr_authored: stemma::domain::RunRprAuthored::default(),
        formatting_change: None,
    })
}

fn make_para(id: &str, segments: Vec<TrackedSegment>) -> ParagraphNode {
    ParagraphNode {
        id: NodeId::from(id),
        style_id: None,
        align: None,
        has_direct_align: false,
        indent: None,
        has_direct_indent: false,
        authored_indent: None,
        spacing: None,
        has_direct_spacing: false,
        authored_spacing: None,
        borders: None,
        keep_next: None,
        keep_lines: None,
        page_break_before: false,
        widow_control: None,
        contextual_spacing: None,
        shading: None,
        has_direct_keep_next: true,
        has_direct_keep_lines: true,
        has_direct_page_break_before: true,
        has_direct_widow_control: true,
        has_direct_contextual_spacing: true,
        has_direct_shading: true,
        has_direct_borders: true,
        tab_stops: vec![],
        effective_tab_stops_rel: vec![],
        segments,
        block_text_hash: None,
        numbering: None,
        has_direct_numbering: true,
        numbering_suppressed: false,
        materialized_numbering: None,
        rendered_text: None,
        literal_prefix: None,
        literal_prefix_leading_rpr: None,
        literal_prefix_trailing_rpr: None,
        literal_prefix_marks: Vec::new(),
        literal_prefix_style_props: StyleProps::default(),
        literal_prefix_rpr_authored: stemma::domain::RunRprAuthored::default(),
        literal_prefix_leading_tab_twips: None,
        literal_prefix_leading_tab_count: 0,
        literal_prefix_leading_ws: String::new(),
        literal_prefix_trailing_ws: String::new(),
        literal_prefix_has_trailing_tab: false,
        literal_prefix_trailing_tab_stop_twips: None,
        outline_lvl: None,
        heading_level: None,
        para_mark_status: None,
        paragraph_mark_marks: vec![],
        paragraph_mark_style_props: StyleProps::default(),
        paragraph_mark_rpr_off: Default::default(),
        para_split: false,
        section_property_change: None,
        formatting_change: None,
        section_properties: None,
        mirror_indents: None,
        auto_space_de: None,
        auto_space_dn: None,
        bidi: None,
        text_alignment: None,
        suppress_auto_hyphens: None,
        snap_to_grid: None,
        overflow_punct: None,
        adjust_right_ind: None,
        word_wrap: None,
        frame_pr: None,
        para_id: None,
        text_id: None,
        text_direction: None,
        cnf_style: None,
        preserved_ppr: Vec::new(),
    }
}

fn make_doc(blocks: Vec<TrackedBlock>) -> CanonDoc {
    CanonDoc {
        id: NodeId::from("doc1"),
        blocks,
        meta: DocMeta {
            schema_version: SCHEMA_VERSION_V0.to_string(),
            docx_fingerprint: DocFingerprint("test".to_string()),
            internal_ids_version: INTERNAL_IDS_VERSION_V0.to_string(),
        },
        headers: vec![],
        footers: vec![],
        footnotes: vec![],
        endnotes: vec![],
        comments: vec![],
        comments_extended: vec![],
        body_section_properties: None,
        body_section_property_change: None,
        compat_settings: CompatSettings::default(),
        even_and_odd_headers: None,
        document_background: None,
        document_protection: None,
    }
}

fn simple_doc(para_id: &str, text: &str) -> CanonDoc {
    let para = make_para(
        para_id,
        normal_segment(vec![make_text(&format!("{para_id}_t1"), text)]),
    );
    make_doc(vec![normal_tracked_block(BlockNode::from(para))])
}

fn test_revision() -> RevisionInfo {
    RevisionInfo {
        revision_id: 100,
        author: Some("Test Author".to_string()),
        date: Some("2026-03-28T00:00:00Z".to_string()),
        apply_op_id: None,
    }
}

fn replace_txn(block_id: &str, expect: &str, replacement: &str) -> EditTransaction {
    EditTransaction {
        steps: vec![EditStep::ReplaceParagraphText {
            block_id: NodeId::from(block_id),
            rationale: None,
            replacement_role: None,
            expect: expect.to_string(),
            semantic_hash: None,
            content: ParagraphContent {
                fragments: vec![ContentFragment::Text(replacement.to_string())],
            },
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    }
}

fn format_txn(block_id: &str, expect: &str) -> EditTransaction {
    let marks = InlineMarkSet {
        bold: true,
        ..Default::default()
    };
    EditTransaction {
        steps: vec![EditStep::SetRunFormatting {
            block_id: NodeId::from(block_id),
            expect: expect.to_string(),
            semantic_hash: None,
            marks,
            style: RunStyleEdit::default(),
            rationale: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    }
}

fn para_text(doc: &CanonDoc, block_id: &str) -> String {
    let block = doc
        .blocks
        .iter()
        .find(|tb| matches!(&tb.block, BlockNode::Paragraph(p) if p.id == NodeId::from(block_id)))
        .expect("block present");
    match &block.block {
        BlockNode::Paragraph(p) => p
            .segments
            .iter()
            .flat_map(|s| s.inlines.iter())
            .filter_map(|i| match i {
                InlineNode::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect(),
        _ => unreachable!(),
    }
}

fn accepted_text(doc: &CanonDoc, block_id: &str) -> String {
    let mut d = doc.clone();
    accept_all(&mut d);
    para_text(&d, block_id)
}

fn rejected_text(doc: &CanonDoc, block_id: &str) -> String {
    let mut d = doc.clone();
    reject_all_with_styles(&mut d, None);
    para_text(&d, block_id)
}

// ─── (a) Foundation inertness: existing verbs stage no pending parts ─────────

#[test]
fn replace_paragraph_text_yields_empty_pending_and_correct_projection() {
    let doc = simple_doc("p1", "The quick brown fox.");
    // `content` is the full replacement paragraph; `expect` is the precondition
    // anchor (a substring that must be present in the original).
    let txn = replace_txn("p1", "quick brown", "The slow grey fox.");

    let (edited, pending) = apply_transaction(&doc, &txn).expect("replace applies");

    // The channel exists but no verb writes to it yet.
    assert!(
        pending.media.is_empty() && pending.style_ops.is_empty(),
        "ReplaceParagraphText must stage no OPC parts (foundation is inert)"
    );

    // Post-conditions unchanged under the new return type.
    assert_eq!(
        rejected_text(&edited, "p1"),
        "The quick brown fox.",
        "reject-all must reconstruct the baseline"
    );
    assert_eq!(
        accepted_text(&edited, "p1"),
        "The slow grey fox.",
        "accept-all must yield the target text"
    );
}

#[test]
fn set_run_formatting_yields_empty_pending_and_text_invariant() {
    let doc = simple_doc("p1", "Confidential terms apply.");
    let txn = format_txn("p1", "Confidential");

    let (edited, pending) = apply_transaction(&doc, &txn).expect("formatting applies");

    assert!(
        pending.media.is_empty() && pending.style_ops.is_empty(),
        "SetRunFormatting must stage no OPC parts (foundation is inert)"
    );

    // Formatting is not a text edit: both projections preserve visible text.
    assert_eq!(
        rejected_text(&edited, "p1"),
        "Confidential terms apply.",
        "reject-all preserves text"
    );
    assert_eq!(
        accepted_text(&edited, "p1"),
        "Confidential terms apply.",
        "accept-all preserves text"
    );
}

#[test]
fn multi_step_transaction_yields_empty_pending() {
    // Two paragraphs, two verbs in one transaction. Still no staged parts.
    let p1 = make_para(
        "p1",
        normal_segment(vec![make_text("p1_t1", "Alpha beta gamma.")]),
    );
    let p2 = make_para(
        "p2",
        normal_segment(vec![make_text("p2_t1", "Delta epsilon zeta.")]),
    );
    let doc = make_doc(vec![
        normal_tracked_block(BlockNode::from(p1)),
        normal_tracked_block(BlockNode::from(p2)),
    ]);

    let txn = EditTransaction {
        steps: vec![
            EditStep::ReplaceParagraphText {
                block_id: NodeId::from("p1"),
                rationale: None,
                replacement_role: None,
                expect: "beta".to_string(),
                semantic_hash: None,
                content: ParagraphContent {
                    fragments: vec![ContentFragment::Text("Alpha BETA gamma.".to_string())],
                },
            },
            EditStep::SetRunFormatting {
                block_id: NodeId::from("p2"),
                expect: "epsilon".to_string(),
                semantic_hash: None,
                marks: InlineMarkSet {
                    italic: true,
                    ..Default::default()
                },
                style: RunStyleEdit::default(),
                rationale: None,
            },
        ],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    let (edited, pending) = apply_transaction(&doc, &txn).expect("multi-step applies");
    assert!(
        pending.media.is_empty() && pending.style_ops.is_empty(),
        "a multi-verb transaction must still stage no OPC parts in the foundation commit"
    );
    assert_eq!(rejected_text(&edited, "p1"), "Alpha beta gamma.");
    assert_eq!(accepted_text(&edited, "p1"), "Alpha BETA gamma.");
}

#[test]
fn direct_mode_replace_also_yields_empty_pending() {
    let doc = simple_doc("p1", "One two three.");
    let mut txn = replace_txn("p1", "two", "One TWO three.");
    txn.materialization_mode = MaterializationMode::Direct;

    let (edited, pending) = apply_transaction(&doc, &txn).expect("direct replace applies");
    assert!(
        pending.media.is_empty() && pending.style_ops.is_empty(),
        "Direct-mode apply must also stage no OPC parts"
    );
    // Direct mode resolves immediately to the target.
    assert_eq!(para_text(&edited, "p1"), "One TWO three.");
}
