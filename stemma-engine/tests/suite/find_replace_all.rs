//! Integration tests for the find-and-replace planner
//! (`stemma::edit::plan_find_replace_all`).
//!
//! The planner is PURE: it composes existing `EditStep::ReplaceParagraphText`
//! steps. These tests assert the standing per-verb invariants from
//! `stemma-engine/docs/testing_strategy.md`:
//!   - accept-all == all-replaced, reject-all == original (reversibility);
//!   - identity (needle == replacement, or absent) => empty plan, zero spans;
//!   - multi-occurrence in one block => exactly ONE step for that block;
//!   - a match spanning a bold->normal run break keeps surrounding marks;
//!   - a barrier straddle is either skipped or fails loud (never half-edited);
//!   - a stale plan (block mutated after planning) fails with a StaleEdit and
//!     leaves the document unmutated.
//!
//! Daily-tier and corpus-free: every fixture is synthesized in-memory IR.

use stemma::edit::*;
use stemma::{
    BlockNode, CanonDoc, HyperlinkData, HyperlinkRun, InlineNode, Mark, NodeId, OpaqueInlineNode,
    OpaqueKind, ParagraphNode, ProofRef, RevisionInfo, StyleProps, TextNode, TrackedSegment,
    TrackingStatus, accept_all, normal_segment, normal_tracked_block, reject_all_with_styles,
};

// ─── IR builders (synthesized, corpus-free) ──────────────────────────────────

fn make_text(id: &str, text: &str, marks: Vec<Mark>) -> InlineNode {
    InlineNode::from(TextNode {
        id: NodeId::from(id),
        text_role: None,
        text: text.to_string(),
        marks,
        style_props: StyleProps::default(),
        rpr_authored: stemma::domain::RunRprAuthored::default(),
        formatting_change: None,
    })
}

fn make_hyperlink(id: &str) -> InlineNode {
    InlineNode::from(OpaqueInlineNode {
        id: NodeId::from(id),
        kind: OpaqueKind::Hyperlink(HyperlinkData {
            url: Some("https://example.com".to_string()),
            anchor: None,
            text: "example".to_string(),
            r_id: Some("rId1".to_string()),
            runs: vec![HyperlinkRun {
                text: "example".to_string(),
                rpr_xml: None,
                status: TrackingStatus::Normal,
            }],
            extra_attrs: vec![],
        }),
        opaque_ref: format!("hyperlink_{id}"),
        proof_ref: ProofRef {
            part: stemma::DocPart::DocumentXml,
            block_id: NodeId::from("p1"),
            docx_anchor: String::new(),
        },
        wrapper_marks: Vec::new(),
        wrapper_style_props: StyleProps::default(),
        raw_xml: Some(
            b"<w:hyperlink r:id=\"rId1\"><w:r><w:t>example</w:t></w:r></w:hyperlink>".to_vec(),
        ),
        content_hash: None,
    })
}

fn build_paragraph(id: &str, segments: Vec<TrackedSegment>) -> ParagraphNode {
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
        literal_prefix_trailing_rpr: None,
        literal_prefix_marks: Vec::new(),
        literal_prefix_style_props: StyleProps::default(),
        literal_prefix_rpr_authored: stemma::domain::RunRprAuthored::default(),
        literal_prefix_leading_rpr: None,
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
        formatting_change: None,
        preserved_ppr: Vec::new(),
    }
}

fn doc_from_paragraphs(paras: Vec<ParagraphNode>) -> CanonDoc {
    CanonDoc {
        id: NodeId::from("doc1"),
        blocks: paras
            .into_iter()
            .map(|p| normal_tracked_block(BlockNode::from(p)))
            .collect(),
        meta: stemma::DocMeta {
            schema_version: stemma::SCHEMA_VERSION_V0.to_string(),
            docx_fingerprint: stemma::DocFingerprint("fr-test".to_string()),
            internal_ids_version: stemma::INTERNAL_IDS_VERSION_V0.to_string(),
        },
        headers: vec![],
        footers: vec![],
        footnotes: vec![],
        endnotes: vec![],
        comments: vec![],
        comments_extended: vec![],
        body_section_properties: None,
        body_section_property_change: None,
        compat_settings: stemma::CompatSettings::default(),
        even_and_odd_headers: None,
        document_background: None,
        document_protection: None,
    }
}

/// A single-segment, single-text-run paragraph from a string.
fn plain_para(id: &str, text: &str) -> ParagraphNode {
    build_paragraph(
        id,
        normal_segment(vec![make_text(&format!("{id}_t"), text, vec![])]),
    )
}

fn test_revision() -> RevisionInfo {
    RevisionInfo {
        revision_id: 7,
        author: Some("find-replace".to_string()),
        date: Some("2026-06-01T00:00:00Z".to_string()),
        apply_op_id: None,
    }
}

fn txn(steps: Vec<EditStep>, mode: MaterializationMode) -> EditTransaction {
    EditTransaction {
        steps,
        summary: Some("find-replace".to_string()),
        materialization_mode: mode,
        revision: test_revision(),
    }
}

fn opts(needle: &str, replacement: &str) -> FindReplaceOptions {
    FindReplaceOptions {
        needle: needle.to_string(),
        replacement: replacement.to_string(),
        scope: FindReplaceScope::BodyOnly,
        case_sensitive: true,
        whole_word: false,
        on_barrier_match: BarrierPolicy::Skip,
    }
}

/// Visible text of a paragraph by id (text nodes only).
fn para_text(doc: &CanonDoc, id: &str) -> String {
    let nid = NodeId::from(id);
    for tb in &doc.blocks {
        if let BlockNode::Paragraph(p) = &tb.block
            && p.id == nid
        {
            let mut out = String::new();
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::Text(t) = inline {
                        out.push_str(&t.text);
                    }
                }
            }
            return out;
        }
    }
    panic!("paragraph '{id}' not found");
}

/// Count text marks of a given kind that survive on a span containing `needle`.
fn span_has_mark(doc: &CanonDoc, id: &str, span: &str, mark: &Mark) -> bool {
    let nid = NodeId::from(id);
    for tb in &doc.blocks {
        if let BlockNode::Paragraph(p) = &tb.block
            && p.id == nid
        {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::Text(t) = inline
                        && t.text.contains(span)
                        && t.marks.contains(mark)
                    {
                        return true;
                    }
                }
            }
        }
    }
    false
}

// ─── T1: reversibility + accept==all-replaced ─────────────────────────────────

#[test]
fn t1_accept_all_replaced_reject_all_original() {
    let base = doc_from_paragraphs(vec![
        plain_para("p1", "The cat sat. The cat ran."),
        plain_para("p2", "A dog. A cat. A bird."),
        plain_para("p3", "No match here."),
    ]);

    let plan = plan_find_replace_all(&base, &opts("cat", "lion")).expect("plan");
    // Two paragraphs match (p1, p2); p3 produces no step.
    assert_eq!(plan.len(), 2, "exactly the matching paragraphs get a step");

    // Tracked apply, then accept-all == all replaced.
    let tracked = apply_transaction(
        &base,
        &txn(plan.clone(), MaterializationMode::TrackedChange),
    )
    .expect("tracked apply")
    .0;
    let mut accepted = tracked.clone();
    accept_all(&mut accepted);
    assert_eq!(para_text(&accepted, "p1"), "The lion sat. The lion ran.");
    assert_eq!(para_text(&accepted, "p2"), "A dog. A lion. A bird.");
    assert_eq!(para_text(&accepted, "p3"), "No match here.");

    // reject-all == original text in every paragraph.
    let mut rejected = tracked;
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(para_text(&rejected, "p1"), "The cat sat. The cat ran.");
    assert_eq!(para_text(&rejected, "p2"), "A dog. A cat. A bird.");
    assert_eq!(para_text(&rejected, "p3"), "No match here.");

    // Accept-all == direct apply.
    let direct = apply_transaction(&base, &txn(plan, MaterializationMode::Direct))
        .expect("direct apply")
        .0;
    assert_eq!(para_text(&accepted, "p1"), para_text(&direct, "p1"));
    assert_eq!(para_text(&accepted, "p2"), para_text(&direct, "p2"));
}

// ─── Identity / absent => empty plan ──────────────────────────────────────────

#[test]
fn identity_needle_equals_replacement_is_empty_plan() {
    let base = doc_from_paragraphs(vec![plain_para("p1", "The cat sat.")]);
    let plan = plan_find_replace_all(&base, &opts("cat", "cat")).expect("plan");
    assert!(plan.is_empty(), "needle == replacement is a no-op");
}

#[test]
fn absent_needle_is_empty_plan() {
    let base = doc_from_paragraphs(vec![plain_para("p1", "The cat sat.")]);
    let plan = plan_find_replace_all(&base, &opts("zebra", "lion")).expect("plan");
    assert!(plan.is_empty(), "absent needle is a no-op");
}

#[test]
fn empty_needle_is_empty_plan() {
    let base = doc_from_paragraphs(vec![plain_para("p1", "The cat sat.")]);
    let plan = plan_find_replace_all(&base, &opts("", "lion")).expect("plan");
    assert!(plan.is_empty(), "empty needle is a no-op");
}

// ─── Multi-occurrence in one block => exactly ONE step ────────────────────────

#[test]
fn multi_occurrence_in_one_block_emits_one_step() {
    let base = doc_from_paragraphs(vec![plain_para("p1", "cat cat cat eats cat food with cat")]);
    let plan = plan_find_replace_all(&base, &opts("cat", "dog")).expect("plan");
    assert_eq!(
        plan.len(),
        1,
        "one block with N occurrences => exactly one step"
    );

    let direct = apply_transaction(&base, &txn(plan, MaterializationMode::Direct))
        .expect("direct apply")
        .0;
    assert_eq!(
        para_text(&direct, "p1"),
        "dog dog dog eats dog food with dog"
    );
}

// ─── Match spanning a bold->normal run break keeps surrounding marks ──────────

#[test]
fn match_spanning_bold_to_normal_run_break_keeps_surrounding_marks() {
    // "Confi" is bold, "dential clause" is normal; the needle "Confidential"
    // straddles the bold->normal break inside ONE text section (no anchor).
    let para = build_paragraph(
        "p1",
        normal_segment(vec![
            make_text("p1_a", "Confi", vec![Mark::Bold]),
            make_text("p1_b", "dential clause", vec![]),
        ]),
    );
    let base = doc_from_paragraphs(vec![para]);

    let plan = plan_find_replace_all(&base, &opts("Confidential", "Secret")).expect("plan");
    assert_eq!(plan.len(), 1);

    let tracked = apply_transaction(
        &base,
        &txn(plan.clone(), MaterializationMode::TrackedChange),
    )
    .expect("tracked apply")
    .0;

    // reject-all reconstructs the original surrounding marks: "Confi" stays bold.
    let mut rejected = tracked.clone();
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(para_text(&rejected, "p1"), "Confidential clause");
    assert!(
        span_has_mark(&rejected, "p1", "Confi", &Mark::Bold),
        "reject-all must restore the original bold run"
    );

    // accept-all yields the replaced text.
    let mut accepted = tracked;
    accept_all(&mut accepted);
    assert_eq!(para_text(&accepted, "p1"), "Secret clause");
}

// ─── Barrier straddle: skipped vs fails loud ──────────────────────────────────

/// Build a paragraph "click [hyperlink] here" where the needle "here" lives
/// AFTER the barrier (matchable) and "clickhere" would straddle it.
fn doc_with_barrier() -> CanonDoc {
    let para = build_paragraph(
        "p1",
        normal_segment(vec![
            make_text("p1_a", "click ", vec![]),
            make_hyperlink("link1"),
            make_text("p1_b", " then click here", vec![]),
        ]),
    );
    doc_from_paragraphs(vec![para])
}

#[test]
fn barrier_straddle_skip_leaves_paragraph_untouched() {
    let base = doc_with_barrier();
    // "click then click" straddles the hyperlink barrier (it spans the anchor):
    // it is present in the full visible text but in no single section.
    let mut o = opts("click  then click", "X"); // visible text = "click  then click here"
    o.on_barrier_match = BarrierPolicy::Skip;
    let plan = plan_find_replace_all(&base, &o).expect("plan");
    assert!(
        plan.is_empty(),
        "a straddling needle under Skip produces no step"
    );
}

#[test]
fn barrier_straddle_fail_errors_loud() {
    let base = doc_with_barrier();
    let mut o = opts("click  then click", "X");
    o.on_barrier_match = BarrierPolicy::Fail;
    let err = plan_find_replace_all(&base, &o).expect_err("must fail loud");
    assert!(
        matches!(err, EditError::FindReplaceBarrierStraddle { .. }),
        "expected FindReplaceBarrierStraddle, got {err:?}"
    );
}

#[test]
fn non_straddling_match_after_barrier_still_replaces() {
    // "here" lives after the hyperlink — a clean section-local match. The
    // hyperlink anchor must survive (non-shrinking opaque inventory).
    let base = doc_with_barrier();
    let plan = plan_find_replace_all(&base, &opts("here", "now")).expect("plan");
    assert_eq!(plan.len(), 1);

    let direct = apply_transaction(&base, &txn(plan, MaterializationMode::Direct))
        .expect("direct apply")
        .0;
    assert_eq!(para_text(&direct, "p1"), "click  then click now");

    // The hyperlink anchor is still present.
    let mut anchors = Vec::new();
    for tb in &direct.blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::OpaqueInline(o) = inline {
                        anchors.push(o.id.to_string());
                    }
                }
            }
        }
    }
    assert!(
        anchors.contains(&"link1".to_string()),
        "hyperlink anchor must survive the replace"
    );
}

// ─── Already-tracked paragraph is refused (no silent history fold) ────────────

#[test]
fn paragraph_with_tracked_segment_is_refused() {
    // One Normal segment + one Inserted segment, both containing the needle.
    let para = build_paragraph(
        "p1",
        vec![
            TrackedSegment {
                status: TrackingStatus::Normal,
                inlines: vec![make_text("p1_a", "cat ", vec![])],
            },
            TrackedSegment {
                status: TrackingStatus::Inserted(test_revision()),
                inlines: vec![make_text("p1_b", "cat", vec![])],
            },
        ],
    );
    let base = doc_from_paragraphs(vec![para]);

    let err = plan_find_replace_all(&base, &opts("cat", "dog")).expect_err("must refuse");
    assert!(
        matches!(err, EditError::ParagraphContainsTrackedSegments { .. }),
        "expected ParagraphContainsTrackedSegments, got {err:?}"
    );
}

// ─── Stale plan => StaleEdit + no mutation ────────────────────────────────────

#[test]
fn stale_plan_fails_and_does_not_mutate() {
    // Plan against v1.
    let v1 = doc_from_paragraphs(vec![plain_para("p1", "The cat sat on the mat.")]);
    let plan = plan_find_replace_all(&v1, &opts("cat", "dog")).expect("plan");
    assert_eq!(plan.len(), 1);

    // Mutate the block (different text) to produce v2 — the planned semantic_hash
    // now mismatches.
    let v2 = doc_from_paragraphs(vec![plain_para("p1", "The CAT sat on the rug.")]);

    let result = apply_transaction(&v2, &txn(plan, MaterializationMode::TrackedChange));
    let err = result.expect_err("stale plan must fail");
    assert!(
        matches!(err, EditError::BlockSemanticHashMismatch { .. }),
        "expected BlockSemanticHashMismatch (StaleEdit), got {err:?}"
    );

    // v2 is untouched (apply_transaction clones; we asserted the original is
    // still the rug text).
    assert_eq!(para_text(&v2, "p1"), "The CAT sat on the rug.");
}

// ─── Case-insensitive writes literal replacement casing ───────────────────────

#[test]
fn case_insensitive_writes_literal_replacement() {
    let base = doc_from_paragraphs(vec![plain_para("p1", "Color, COLOR, color.")]);
    let mut o = opts("color", "colour");
    o.case_sensitive = false;
    let plan = plan_find_replace_all(&base, &o).expect("plan");
    assert_eq!(plan.len(), 1);

    let direct = apply_transaction(&base, &txn(plan, MaterializationMode::Direct))
        .expect("direct apply")
        .0;
    assert_eq!(para_text(&direct, "p1"), "colour, colour, colour.");
}

// ─── whole_word boundary ──────────────────────────────────────────────────────

#[test]
fn whole_word_does_not_match_inside_a_word() {
    let base = doc_from_paragraphs(vec![plain_para("p1", "category cat cats (cat)")]);
    let mut o = opts("cat", "dog");
    o.whole_word = true;
    let plan = plan_find_replace_all(&base, &o).expect("plan");
    assert_eq!(plan.len(), 1);

    let direct = apply_transaction(&base, &txn(plan, MaterializationMode::Direct))
        .expect("direct apply")
        .0;
    assert_eq!(para_text(&direct, "p1"), "category dog cats (dog)");
}
