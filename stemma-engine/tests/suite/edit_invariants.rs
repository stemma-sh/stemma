//! Edit-pipeline invariants (test tier mirror of the diff/redline
//! invariants documented in `docs/testing_strategy.md`).
//!
//! This file closes the gap between the mature diff/redline invariant
//! coverage and the edit pipeline. For the existing `ReplaceParagraphText`
//! op it proves:
//!
//! **Tier 1 (text identity)**
//! - `accept_all(apply_edit(doc, edit)) == text(doc) with edit applied`
//!   (`edit_tracked_accept_matches_new_text`)
//! - `reject_all(apply_edit(doc, edit)) == text(doc)` (unchanged base)
//!   (`edit_tracked_reject_matches_original_text`)
//! - Edit fixpoint: apply → serialize → re-parse → accept equals
//!   apply → accept with no XML drift
//!   (`edit_serialize_reparse_accept_equals_canonical_accept`)
//! - Identity replace is a true no-op (no phantom tracked spans)
//!   (`edit_identity_replacement_produces_no_tracked_spans`)
//! - Direct materialization parity: `direct` mode converges on the
//!   accept state of the tracked-change mode
//!   (`edit_direct_mode_matches_tracked_accept`)
//!
//! **Tier 2 (structural formatting)**
//! - Heading style survives an edit+accept round
//!   (`edit_preserves_heading_style_through_accept`)
//! - Bold/italic on unchanged spans survives accept
//!   (`edit_preserves_bold_on_unchanged_span_through_accept`)
//! - Numbering survives accept
//!   (`edit_preserves_numbering_through_accept`)
//!
//! **Word Oracle (nightly, held-out tier)**
//! - #14b and #14 / #20c applied to edit output are proven in the held-out
//!   real-Word conformance tier (see `docs/testing_strategy.md`); that tier
//!   does not run on a public clone.

use std::fs;

use stemma::edit::*;
use stemma::{
    BlockNode, CanonDoc, DocxRuntime, ExportMode, HeadingLevel, HyperlinkData, HyperlinkRun,
    InlineNode, Mark, MarkValue, NodeId, NumberingInfo, OpaqueInlineNode, OpaqueKind,
    ParagraphNode, ProofRef, RevisionInfo, SimpleRuntime, StyleProps, TextNode, TrackedSegment,
    TrackingStatus, accept_all, normal_segment, normal_tracked_block, reject_all_with_styles,
};

// ─── shared helpers ──────────────────────────────────────────────────────────

fn test_revision() -> RevisionInfo {
    RevisionInfo {
        revision_id: 42,
        identity: 0,
        author: Some("edit-invariants".to_string()),
        date: Some("2026-04-11T00:00:00Z".to_string()),
        apply_op_id: None,
    }
}

fn replace_tx(
    block_id: &str,
    expect: &str,
    content: ParagraphContent,
    mode: MaterializationMode,
) -> EditTransaction {
    EditTransaction {
        steps: vec![EditStep::ReplaceParagraphText {
            block_id: NodeId::from(block_id),
            rationale: None,
            replacement_role: None,
            expect: expect.to_string(),
            semantic_hash: None,
            content,
        }],
        summary: None,
        materialization_mode: mode,
        revision: test_revision(),
    }
}

fn text_content(text: &str) -> ParagraphContent {
    ParagraphContent {
        fragments: vec![ContentFragment::Text(text.to_string())],
    }
}

/// Build a minimal paragraph with all the bookkeeping fields populated
/// so tests can focus on the interesting attributes (marks, style_id,
/// numbering) without dragging in a 60-line literal.
fn build_paragraph(
    id: &str,
    segments: Vec<TrackedSegment>,
    style_id: Option<&str>,
    heading_level: Option<HeadingLevel>,
    numbering: Option<NumberingInfo>,
) -> ParagraphNode {
    ParagraphNode {
        id: NodeId::from(id),
        style_id: style_id.map(|s| s.to_string().into()),
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
        numbering,
        has_direct_numbering: true,
        numbering_suppressed: false,
        materialized_numbering: None,
        rendered_text: None,
        literal_prefix: None,
        literal_prefix_marks: Vec::new(),
        literal_prefix_style_props: StyleProps::default(),
        literal_prefix_rpr_authored: stemma::domain::RunRprAuthored::default(),
        literal_prefix_leading_rpr: None,
        literal_prefix_trailing_rpr: None,
        literal_prefix_leading_tab_twips: None,
        literal_prefix_leading_tab_count: 0,
        literal_prefix_leading_ws: String::new(),
        literal_prefix_trailing_ws: String::new(),
        literal_prefix_has_trailing_tab: false,
        literal_prefix_trailing_tab_stop_twips: None,
        outline_lvl: None,
        heading_level,
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

fn simple_doc(para: ParagraphNode) -> CanonDoc {
    CanonDoc {
        id: NodeId::from("doc1"),
        blocks: vec![normal_tracked_block(BlockNode::from(para))],
        meta: stemma::DocMeta {
            schema_version: stemma::SCHEMA_VERSION_V0.to_string(),
            docx_fingerprint: stemma::DocFingerprint("inv-test".to_string()),
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

/// Extract visible text from a paragraph (text nodes only).
fn para_text(doc: &CanonDoc, block_id: &str) -> String {
    let nid = NodeId::from(block_id);
    for tb in &doc.blocks {
        if let BlockNode::Paragraph(p) = &tb.block
            && p.id == nid
        {
            let mut out = String::new();
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::Text(t) = inline {
                        if t.style_props.caps == MarkValue::On {
                            out.push_str(&t.text.to_uppercase());
                        } else {
                            out.push_str(&t.text);
                        }
                    }
                }
            }
            return out;
        }
    }
    panic!("paragraph '{block_id}' not found");
}

fn find_para<'a>(doc: &'a CanonDoc, block_id: &str) -> &'a ParagraphNode {
    let nid = NodeId::from(block_id);
    doc.blocks
        .iter()
        .find_map(|tb| {
            if let BlockNode::Paragraph(p) = &tb.block
                && p.id == nid
            {
                Some(p)
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("paragraph '{block_id}' not found"))
}

fn count_tracked_spans(doc: &CanonDoc, block_id: &str) -> (usize, usize) {
    let para = find_para(doc, block_id);
    let mut inserted = 0;
    let mut deleted = 0;
    for seg in &para.segments {
        match &seg.status {
            TrackingStatus::Inserted(_) => inserted += 1,
            TrackingStatus::Deleted(_) => deleted += 1,
            // A stacked segment carries one pending insertion AND one
            // pending deletion.
            TrackingStatus::InsertedThenDeleted(_) => {
                inserted += 1;
                deleted += 1;
            }
            TrackingStatus::Normal => {}
        }
    }
    (inserted, deleted)
}

// ─── Tier 1: text identity ─────────────────────────────────────────────────

/// Invariant (edit version of #6 / #14):
///   accept_all(apply_edit(doc, edit)) == text(doc) with the edit applied
#[test]
fn edit_tracked_accept_matches_new_text() {
    let para = build_paragraph(
        "p1",
        normal_segment(vec![make_text("t1", "The quick brown fox.", vec![])]),
        None,
        None,
        None,
    );
    let doc = simple_doc(para);

    let tx = replace_tx(
        "p1",
        "quick",
        text_content("The slow brown fox."),
        MaterializationMode::TrackedChange,
    );
    let mut edited = apply_transaction(&doc, &tx)
        .expect("apply_transaction succeeds")
        .0;
    accept_all(&mut edited);

    assert_eq!(
        para_text(&edited, "p1"),
        "The slow brown fox.",
        "accept_all on an edited doc must produce the edit's new text"
    );
}

/// Invariant: rejecting an edit returns the original text verbatim.
/// The pre-condition is that the original doc was Normal (no existing
/// tracked changes); the post-condition is that rejecting the edit is
/// the identity over original text.
#[test]
fn edit_tracked_reject_matches_original_text() {
    let para = build_paragraph(
        "p1",
        normal_segment(vec![make_text("t1", "The quick brown fox.", vec![])]),
        None,
        None,
        None,
    );
    let doc = simple_doc(para);

    let tx = replace_tx(
        "p1",
        "quick",
        text_content("The slow brown fox."),
        MaterializationMode::TrackedChange,
    );
    let mut edited = apply_transaction(&doc, &tx)
        .expect("apply_transaction succeeds")
        .0;
    reject_all_with_styles(&mut edited, None);

    assert_eq!(
        para_text(&edited, "p1"),
        "The quick brown fox.",
        "reject_all on an edited doc must return the original text (no drift)"
    );
}

/// Edit fixpoint (edit version of #7 / #12):
///   apply_edit → serialize → re-parse → accept_all
/// must produce the same visible text as
///   apply_edit → accept_all (canonical-only)
///
/// This is the edit-pipeline equivalent of the diff fixpoint invariant:
/// it proves the serializer and the canonical apply path agree.
#[test]
fn edit_serialize_reparse_accept_equals_canonical_accept() {
    // Drive this through the SimpleRuntime so the serializer + reimport path is
    // exercised. The `safe-us-vs-singapore` before.docx is committed under
    // `stemma-engine/testdata/`, so the fixpoint invariant runs UNCONDITIONALLY in the
    // daily gate — no corpus dependency, no silent SKIP fallback.
    drive_fixpoint_check("testdata/safe-us-vs-singapore/before.docx");
}

fn drive_fixpoint_check(fixture_path: &str) {
    let bytes = fs::read(fixture_path).unwrap_or_else(|e| panic!("read {fixture_path}: {e}"));
    let runtime = SimpleRuntime::new();
    let import = runtime
        .import_docx(&bytes)
        .unwrap_or_else(|e| panic!("import {fixture_path}: {e:?}"));

    // Find the first editable paragraph (Normal status, no tracked segments,
    // at least 2 words, no preserved inlines). This mirrors the helper in
    // edit_serialize.rs so this invariant uses the same surface the existing
    // sweep exercises.
    let Some((block_id, original_text, first_word)) = find_editable_paragraph(&import.canonical)
    else {
        panic!(
            "fixture {fixture_path} has no editable paragraph — the fixpoint invariant needs a \
             plain-text paragraph with at least two words; the committed fixture must provide one"
        );
    };

    let new_text = original_text.replacen(&first_word, "EDITED", 1);
    let tx = replace_tx(
        &block_id.to_string(),
        &first_word,
        text_content(&new_text),
        MaterializationMode::TrackedChange,
    );

    // Canonical path: apply → accept
    let canonical_edited = apply_transaction(&import.canonical, &tx)
        .unwrap_or_else(|e| panic!("apply_transaction on {fixture_path}: {e}"))
        .0;
    let mut canonical_accepted = canonical_edited.clone();
    accept_all(&mut canonical_accepted);
    let canonical_accepted_text = extract_para_text(&canonical_accepted, &block_id);

    // Serialized path: apply via runtime → export → reimport → accept
    let apply_result = runtime
        .apply_edit(&import.doc_handle, &tx)
        .unwrap_or_else(|e| panic!("runtime.apply_edit on {fixture_path}: {e:?}"));
    let exported = runtime
        .export_docx(&import.doc_handle, ExportMode::Redline)
        .unwrap_or_else(|e| panic!("export_docx on {fixture_path}: {e:?}"));
    let reimport_runtime = SimpleRuntime::new();
    let reimport = reimport_runtime
        .import_docx(&exported)
        .unwrap_or_else(|e| panic!("reimport on {fixture_path}: {e:?}"));

    let mut serialized_accepted = (*reimport.canonical).clone();
    accept_all(&mut serialized_accepted);
    let serialized_accepted_text = extract_para_text(&serialized_accepted, &block_id);

    // Post-condition: both paths converge on the same text for the target
    // paragraph. The block_id is stable across import/export by contract,
    // so the paragraph can be located by the same id.
    assert_eq!(
        canonical_accepted_text, serialized_accepted_text,
        "edit fixpoint violated for {fixture_path} block {block_id}:\n  \
         canonical accept:  {canonical_accepted_text:?}\n  \
         serialized accept: {serialized_accepted_text:?}"
    );
    // Sanity check: the accept text must match the declared new text.
    assert_eq!(
        canonical_accepted_text, new_text,
        "canonical accept text diverged from the declared replacement in {fixture_path}"
    );
    // Keep the apply_result alive so the export step uses its side effects.
    let _ = apply_result;
}

fn find_editable_paragraph(doc: &CanonDoc) -> Option<(NodeId, String, String)> {
    doc.blocks.iter().find_map(|tb| {
        if !matches!(tb.status, TrackingStatus::Normal) {
            return None;
        }
        let BlockNode::Paragraph(p) = &tb.block else {
            return None;
        };
        if p.segments
            .iter()
            .any(|s| !matches!(s.status, TrackingStatus::Normal))
        {
            return None;
        }
        if p.segments.iter().any(|s| {
            s.inlines
                .iter()
                .any(|i| matches!(i, InlineNode::OpaqueInline(_) | InlineNode::HardBreak(_)))
        }) {
            return None;
        }
        let text: String = p
            .segments
            .iter()
            .flat_map(|s| s.inlines.iter())
            .filter_map(|i| match i {
                InlineNode::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect();
        let first_word = text.split_whitespace().next().unwrap_or("").to_string();
        if text.split_whitespace().count() >= 2 && !first_word.is_empty() {
            Some((p.id.clone(), text, first_word))
        } else {
            None
        }
    })
}

fn extract_para_text(doc: &CanonDoc, block_id: &NodeId) -> String {
    for tb in &doc.blocks {
        if let BlockNode::Paragraph(p) = &tb.block
            && &p.id == block_id
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
    panic!("paragraph {block_id} not found after reimport");
}

/// Identity edit contract: applying a replace whose new content is
/// **textually identical** to the old content (same text, marks, anchors)
/// changes nothing. The domain rule (CLAUDE.md "no silent fallbacks") is that
/// an op which would change nothing must FAIL LOUD — a no-op reported as a
/// successful application is the bug. The engine must therefore refuse the
/// identity replace with `EditError::NoOpEdit` naming the op index and block id,
/// rather than silently emitting zero tracked spans and reporting success.
///
/// (The separate diff/redline path keeps `diff(A, A)` empty — see
/// `identity_invariant.rs`; that pipeline does not route through the
/// `apply_transaction` edit verbs and so is unaffected.)
#[test]
fn edit_identity_replacement_fails_loud_as_no_op() {
    let para = build_paragraph(
        "p1",
        normal_segment(vec![make_text(
            "t1",
            "Confidential Information means information.",
            vec![],
        )]),
        None,
        None,
        None,
    );
    let doc = simple_doc(para);

    let tx = replace_tx(
        "p1",
        "Confidential",
        text_content("Confidential Information means information."),
        MaterializationMode::TrackedChange,
    );
    let err = apply_transaction(&doc, &tx)
        .expect_err("an identity replace changes nothing and must fail loud, not report success");
    match err {
        EditError::NoOpEdit {
            block_id,
            step_index,
            ..
        } => {
            assert_eq!(block_id, NodeId::from("p1"));
            assert_eq!(step_index, 0);
        }
        other => panic!("expected EditError::NoOpEdit, got {other:?}"),
    }
}

/// Direct materialization parity: for the same logical edit,
/// `MaterializationMode::Direct` must produce a document whose visible
/// text matches `accept_all(tracked_mode)`. This proves the two
/// materialization modes converge on the same end state — the only
/// difference is whether the intermediate tracked-change markup is
/// produced.
///
/// Domain rule: direct mode is "apply + accept in one step", so its
/// output is the fixpoint of the tracked-change mode.
#[test]
fn edit_direct_mode_matches_tracked_accept() {
    let make_base = || {
        build_paragraph(
            "p1",
            normal_segment(vec![make_text("t1", "The original sentence here.", vec![])]),
            None,
            None,
            None,
        )
    };

    let tx_tracked = replace_tx(
        "p1",
        "original",
        text_content("The revised sentence here."),
        MaterializationMode::TrackedChange,
    );
    let tx_direct = replace_tx(
        "p1",
        "original",
        text_content("The revised sentence here."),
        MaterializationMode::Direct,
    );

    let base = simple_doc(make_base());

    let mut tracked_edited = apply_transaction(&base, &tx_tracked).unwrap().0;
    accept_all(&mut tracked_edited);
    let tracked_text = para_text(&tracked_edited, "p1");

    let direct_edited = apply_transaction(&base, &tx_direct).unwrap().0;
    let direct_text = para_text(&direct_edited, "p1");

    assert_eq!(
        tracked_text, direct_text,
        "direct mode must match accept_all(tracked mode); got tracked={tracked_text:?} direct={direct_text:?}"
    );
    assert_eq!(
        direct_text, "The revised sentence here.",
        "direct mode must produce the replacement text"
    );

    // Direct mode must leave the block in Normal state with zero tracked
    // spans — it isn't "accept after the fact", it's "write directly".
    let (inserted, deleted) = count_tracked_spans(&direct_edited, "p1");
    assert_eq!(
        (inserted, deleted),
        (0, 0),
        "direct edit must leave no tracked spans; got ins={inserted} del={deleted}"
    );
    assert!(
        matches!(direct_edited.blocks[0].status, TrackingStatus::Normal),
        "direct edit must leave block status Normal"
    );
}

// ─── Tier 2: structural formatting ─────────────────────────────────────────

/// Heading style must survive an edit + accept round. Domain rule: the
/// edit engine never touches paragraph-level metadata (style_id,
/// numbering, heading_level), so after
/// accepting the edit the paragraph must still declare the same
/// heading style it started with.
#[test]
fn edit_preserves_heading_style_through_accept() {
    let para = build_paragraph(
        "p1",
        normal_segment(vec![make_text(
            "t1",
            "Section 1: Definitions",
            vec![Mark::Bold],
        )]),
        Some("Heading1"),
        Some(HeadingLevel::H1),
        None,
    );
    let doc = simple_doc(para);

    let tx = replace_tx(
        "p1",
        "Definitions",
        text_content("Section 1: Updated Definitions"),
        MaterializationMode::TrackedChange,
    );
    let mut edited = apply_transaction(&doc, &tx).expect("apply succeeds").0;
    accept_all(&mut edited);

    let para = find_para(&edited, "p1");
    assert_eq!(
        para.style_id.as_deref(),
        Some("Heading1"),
        "heading style_id must survive accept"
    );
    assert_eq!(
        para.heading_level,
        Some(HeadingLevel::H1),
        "heading_level must survive accept"
    );
    assert_eq!(
        para_text(&edited, "p1"),
        "Section 1: Updated Definitions",
        "accept text must match the replacement"
    );
}

/// Bold (or any mark) on an *unchanged* span must survive an edit +
/// accept. Domain rule: unchanged tokens are moved verbatim from the
/// original paragraph (invariant I4), so their marks are preserved by
/// construction. If any accepted-state run that overlaps the kept
/// region loses Bold, the edit engine broke I4.
#[test]
fn edit_preserves_bold_on_unchanged_span_through_accept() {
    // "Confidential Information means [plain]information[/plain]."
    // where "Confidential Information" is bold.
    let para = build_paragraph(
        "p1",
        normal_segment(vec![
            make_text("t1", "Confidential Information", vec![Mark::Bold]),
            make_text("t2", " means information.", vec![]),
        ]),
        None,
        None,
        None,
    );
    let doc = simple_doc(para);

    // Edit only the plain part — "information." → "classified data."
    let tx = replace_tx(
        "p1",
        "means information.",
        text_content("Confidential Information means classified data."),
        MaterializationMode::TrackedChange,
    );
    let mut edited = apply_transaction(&doc, &tx).expect("apply succeeds").0;
    accept_all(&mut edited);

    let para = find_para(&edited, "p1");

    // Find the run that contains "Confidential Information" and assert
    // it retains the Bold mark. We look at all Normal+Inserted inlines
    // (Deleted ones are gone after accept, but we already accepted).
    let mut saw_bold_confidential = false;
    for seg in &para.segments {
        for inline in &seg.inlines {
            if let InlineNode::Text(t) = inline
                && t.text.contains("Confidential Information")
            {
                assert!(
                    t.marks.contains(&Mark::Bold),
                    "unchanged 'Confidential Information' run lost Bold after accept; marks={:?}",
                    t.marks
                );
                saw_bold_confidential = true;
            }
        }
    }
    assert!(
        saw_bold_confidential,
        "expected to find the bolded 'Confidential Information' run in accepted output"
    );
    assert_eq!(
        para_text(&edited, "p1"),
        "Confidential Information means classified data."
    );
}

/// Numbering survives edit + accept. Domain rule: the edit engine
/// never rewrites paragraph numbering (it edits inline content inside
/// the paragraph). So after accept, the paragraph must still reference
/// the same (num_id, ilvl).
#[test]
fn edit_preserves_numbering_through_accept() {
    let numbering = NumberingInfo {
        num_id: 3,
        ilvl: 0,
        synthesized_text: "1.".to_string(),
        is_bullet: false,
        restart_numbering: false,
    };
    let para = build_paragraph(
        "p1",
        normal_segment(vec![make_text("t1", "First item in the list", vec![])]),
        Some("ListParagraph"),
        None,
        Some(numbering.clone()),
    );
    let doc = simple_doc(para);

    let tx = replace_tx(
        "p1",
        "First",
        text_content("First rewritten item in the list"),
        MaterializationMode::TrackedChange,
    );
    let mut edited = apply_transaction(&doc, &tx).expect("apply succeeds").0;
    accept_all(&mut edited);

    let para = find_para(&edited, "p1");
    let got = para
        .numbering
        .as_ref()
        .expect("numbering must survive accept");
    assert!(
        got.structurally_eq(&numbering),
        "numbering (num_id, ilvl) must survive accept: expected num_id={} ilvl={} got num_id={} ilvl={}",
        numbering.num_id,
        numbering.ilvl,
        got.num_id,
        got.ilvl
    );
    assert_eq!(
        para.style_id.as_deref(),
        Some("ListParagraph"),
        "list paragraph style must survive"
    );
    assert_eq!(para_text(&edited, "p1"), "First rewritten item in the list");
}

// ─── Hyperlink (opaque inline) preservation through accept ─────────────────

/// A hyperlink (opaque inline) must survive an edit + accept. The
/// domain rule is: preserved inlines are immovable anchors — when the
/// edit references the hyperlink by its NodeId, the original node is
/// reinserted at the new position and survives accept unchanged.
///
/// This is the "strikethrough leaking" / "opaque gone after accept"
/// check for the edit pipeline — the edit analogue of the Tier 2
/// formatting fidelity gate in invariant #18.
#[test]
fn edit_preserves_hyperlink_through_accept() {
    let hyperlink = make_hyperlink("link1");
    let para = build_paragraph(
        "p1",
        normal_segment(vec![
            make_text("t1", "See ", vec![]),
            hyperlink,
            make_text("t2", " for more details.", vec![]),
        ]),
        None,
        None,
        None,
    );
    let doc = simple_doc(para);

    let content = ParagraphContent {
        fragments: vec![
            ContentFragment::Text("Refer to ".to_string()),
            ContentFragment::PreservedInlineRef(NodeId::from("link1")),
            ContentFragment::Text(" for complete details.".to_string()),
        ],
    };
    let tx = replace_tx("p1", "See", content, MaterializationMode::TrackedChange);
    let mut edited = apply_transaction(&doc, &tx).expect("apply succeeds").0;
    accept_all(&mut edited);

    let para = find_para(&edited, "p1");
    let has_hyperlink = para.segments.iter().any(|s| {
        s.inlines.iter().any(|i| {
            matches!(
                i,
                InlineNode::OpaqueInline(o)
                    if o.id == NodeId::from("link1")
                        && matches!(&o.kind, OpaqueKind::Hyperlink(_))
            )
        })
    });
    assert!(
        has_hyperlink,
        "hyperlink 'link1' must survive accept unchanged"
    );
}
