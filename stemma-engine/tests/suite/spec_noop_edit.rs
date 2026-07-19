//! Write-path truth: a content-bearing edit op that changes NOTHING must not
//! be reported as a successful application.
//!
//! Domain rule (CLAUDE.md "No silent fallbacks" / prime directive): an edit
//! transaction either changes the document or it fails loudly. "Applied a
//! change" and "changed nothing" are different outcomes and must be
//! distinguishable by the caller. Silently `continue`-ing past an op that had
//! no effect, then returning `applied: true`, is the same family as the old D5
//! ApplyStyle silent-ok defect.
//!
//! Two distinct defects this file pins, both seen when a cold agent issued
//! three whole-paragraph styled replaces that returned `{"applied": true}`
//! while changing nothing:
//!
//! 1. **Mark-blind identity check.** A whole-paragraph replace that changes
//!    ONLY run marks (e.g. "Events" → bold "Events") was classified as an
//!    identity replacement and silently dropped, because the identity check
//!    compared visible text + anchors but ignored marks. That is a real
//!    correctness bug: the mark change is a genuine edit and must apply.
//!
//! 2. **Genuine no-op reported as success.** A whole-paragraph replace whose
//!    content equals the paragraph in text AND marks is a true no-op. The
//!    engine swallowed it via `continue` and the caller could not tell the op
//!    had no effect. A true no-op must surface as an explicit error naming the
//!    op index, the block id, and why nothing changed.

use stemma::domain::*;
use stemma::edit::*;

// ─── helpers (mirrors edit_basic.rs construction) ────────────────────────────

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

fn make_text_with_marks(id: &str, text: &str, marks: Vec<Mark>) -> InlineNode {
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

fn test_revision() -> RevisionInfo {
    RevisionInfo {
        revision_id: 100,
        identity: 0,
        author: Some("Test Author".to_string()),
        date: Some("2026-03-28T00:00:00Z".to_string()),
        apply_op_id: None,
    }
}

fn replace_step(block_id: &str, expect: &str, content: ParagraphContent) -> EditStep {
    EditStep::ReplaceParagraphText {
        block_id: NodeId::from(block_id),
        rationale: None,
        replacement_role: None,
        expect: expect.to_string(),
        semantic_hash: None,
        content,
    }
}

fn transaction(steps: Vec<EditStep>, mode: MaterializationMode) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        materialization_mode: mode,
        revision: test_revision(),
    }
}

/// All marks present on the (single) text node of a paragraph after accept_all.
fn accepted_marks(doc: &CanonDoc, block_id: &str) -> Vec<Mark> {
    let mut doc = doc.clone();
    stemma::accept_all(&mut doc);
    let nid = NodeId::from(block_id);
    for tb in &doc.blocks {
        if let BlockNode::Paragraph(p) = &tb.block
            && p.id == nid
        {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::Text(t) = inline {
                        return t.marks.clone();
                    }
                }
            }
        }
    }
    panic!("paragraph '{block_id}' not found or has no text");
}

// ─── Defect 1: mark-only whole-paragraph replace must NOT be a no-op ─────────

/// Replacing "Events" (plain) with bold "Events" is a real edit — only the
/// marks differ. The engine must apply it, not classify it as an identity
/// replacement and drop it. (Tracked mode: the mark change is materialized;
/// after accept_all the run is bold.)
#[test]
fn mark_only_whole_paragraph_replace_is_not_identity() {
    let para = make_para("p1", normal_segment(vec![make_text("p1_t1", "Events")]));
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    // Same text, but now bold. This is the styled-content path
    // (apply_segment_replace_paragraph), the exact shape an agent sends when it
    // re-applies a mark to otherwise-unchanged text.
    let content = ParagraphContent {
        fragments: vec![ContentFragment::StyledText {
            text: "Events".to_string(),
            marks: InlineMarkSet {
                bold: true,
                ..Default::default()
            },
        }],
    };
    let tx = transaction(
        vec![replace_step("p1", "Events", content)],
        MaterializationMode::TrackedChange,
    );

    apply_transaction(&doc, &tx).expect("mark-only replace must apply, not no-op");
    // would-be result: the accepted run carries Bold. If the engine treated
    // this as identity, the run would still be plain (no Bold) — the assertion
    // catches the silent drop.
    let result = apply_transaction(&doc, &tx).unwrap().0;
    assert!(
        accepted_marks(&result, "p1").contains(&Mark::Bold),
        "mark-only whole-paragraph replace was silently dropped: accepted run is not bold"
    );
}

// ─── Defect 2: a true no-op must surface as an explicit error ────────────────

/// Replacing "Events" with identical content (same text, same marks) changes
/// nothing. The op must fail loudly with `NoOpEdit` naming the op index and
/// block id — not silently report success.
#[test]
fn true_identity_replace_is_an_explicit_error() {
    let para = make_para(
        "p1",
        normal_segment(vec![make_text_with_marks(
            "p1_t1",
            "Events",
            vec![Mark::Bold, Mark::Italic],
        )]),
    );
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    // Identical: same visible text, same marks.
    let content = ParagraphContent {
        fragments: vec![ContentFragment::StyledText {
            text: "Events".to_string(),
            marks: InlineMarkSet {
                bold: true,
                italic: true,
                ..Default::default()
            },
        }],
    };
    let tx = transaction(
        vec![replace_step("p1", "Events", content)],
        MaterializationMode::TrackedChange,
    );

    let err = apply_transaction(&doc, &tx).expect_err(
        "a content-bearing op that changes nothing must fail loudly, not report success",
    );
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

/// The same no-op rule holds for a plain-text whole-paragraph replace whose
/// text equals the original (the diff-path identity, not the styled path).
#[test]
fn true_identity_plain_replace_is_an_explicit_error() {
    let para = make_para("p1", normal_segment(vec![make_text("p1_t1", "Events")]));
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    let content = ParagraphContent {
        fragments: vec![ContentFragment::Text("Events".to_string())],
    };
    let tx = transaction(
        vec![replace_step("p1", "Events", content)],
        MaterializationMode::TrackedChange,
    );

    let err = apply_transaction(&doc, &tx).expect_err("plain identity replace must fail loudly");
    assert!(
        matches!(err, EditError::NoOpEdit { step_index: 0, .. }),
        "expected EditError::NoOpEdit at step 0, got {err:?}"
    );
}

// ─── Ground-truth invariant: success ⟺ the document actually changed ─────────
//
// A span-edit can "succeed" against a guard captured BEFORE several
// applied=true responses, leaving the paragraph byte-untouched through those
// "successful" writes. The invariant that makes that impossible: a content-
// bearing replace either CHANGES the document (its TrackedBlocks differ from the
// input) and returns Ok, or it changes nothing and returns Err(NoOpEdit). There
// is no third outcome (Ok + unchanged). We assert over the full TrackedBlock
// structural diff, NOT the guard semantic_hash — the guard deliberately skips
// pending-deleted text and ignores formatting, so it would wrongly call a
// tracked delete-only or format-only edit a no-op.

/// True when no block's `TrackedBlock` value differs between two docs (same ids,
/// same structure). The honest "did the document change?" predicate.
fn docs_structurally_equal(a: &CanonDoc, b: &CanonDoc) -> bool {
    a.blocks == b.blocks
}

#[test]
fn apply_success_iff_document_changed() {
    // A battery of (label, doc, transaction) cases spanning the agent-reachable
    // identity exits and their non-identity counterparts.
    struct Case {
        label: &'static str,
        doc: CanonDoc,
        tx: EditTransaction,
    }

    let plain_para = || {
        make_doc(vec![normal_tracked_block(BlockNode::from(make_para(
            "p1",
            normal_segment(vec![make_text("p1_t1", "Events")]),
        )))])
    };
    let bold_italic_para = || {
        make_doc(vec![normal_tracked_block(BlockNode::from(make_para(
            "p1",
            normal_segment(vec![make_text_with_marks(
                "p1_t1",
                "Events",
                vec![Mark::Bold, Mark::Italic],
            )]),
        )))])
    };

    let cases = vec![
        Case {
            label: "plain identity (text equal)",
            doc: plain_para(),
            tx: transaction(
                vec![replace_step(
                    "p1",
                    "Events",
                    ParagraphContent {
                        fragments: vec![ContentFragment::Text("Events".to_string())],
                    },
                )],
                MaterializationMode::TrackedChange,
            ),
        },
        Case {
            label: "real text change",
            doc: plain_para(),
            tx: transaction(
                vec![replace_step(
                    "p1",
                    "Events",
                    ParagraphContent {
                        fragments: vec![ContentFragment::Text("Events and More".to_string())],
                    },
                )],
                MaterializationMode::TrackedChange,
            ),
        },
        Case {
            label: "styled identity (same marks)",
            doc: bold_italic_para(),
            tx: transaction(
                vec![replace_step(
                    "p1",
                    "Events",
                    ParagraphContent {
                        fragments: vec![ContentFragment::StyledText {
                            text: "Events".to_string(),
                            marks: InlineMarkSet {
                                bold: true,
                                italic: true,
                                ..Default::default()
                            },
                        }],
                    },
                )],
                MaterializationMode::TrackedChange,
            ),
        },
        Case {
            label: "mark-only change (plain -> bold)",
            doc: plain_para(),
            tx: transaction(
                vec![replace_step(
                    "p1",
                    "Events",
                    ParagraphContent {
                        fragments: vec![ContentFragment::StyledText {
                            text: "Events".to_string(),
                            marks: InlineMarkSet {
                                bold: true,
                                ..Default::default()
                            },
                        }],
                    },
                )],
                MaterializationMode::TrackedChange,
            ),
        },
    ];

    for case in cases {
        match apply_transaction(&case.doc, &case.tx) {
            Ok((after, _)) => {
                assert!(
                    !docs_structurally_equal(&case.doc, &after),
                    "[{}] apply returned Ok but the document is unchanged — a no-op reported \
                     as success (the exact bug)",
                    case.label
                );
            }
            Err(EditError::NoOpEdit { .. }) => {
                // The only legitimate non-Ok outcome here: the op changed
                // nothing and said so loudly. (Other errors would be a
                // different failure and are surfaced by the panic below.)
            }
            Err(other) => panic!("[{}] unexpected error: {other:?}", case.label),
        }
    }
}

// ─── p_13-shaped fixtures: literal-prefix numbering, and the strip's legit case
//
// The p_13 shape has `list: null` and the LITERAL text "1.\tEvents" — its "1."
// was hoisted by import into `literal_prefix` (the read view re-prepends it).
// These pin the two outcomes around that shape that ARE in Task #1's scope; the
// non-identity prefix-strip contract (refuse-or-report) is Task #8.

/// A paragraph carrying a literal-prefix "1." (numbering materialized as the
/// `literal_prefix` field, NOT numPr). Body runs hold only "Events".
fn literal_prefix_para(id: &str) -> ParagraphNode {
    let mut para = make_para(
        id,
        normal_segment(vec![make_text(&format!("{id}_t1"), "Events")]),
    );
    para.literal_prefix = Some("1.".to_string());
    para
}

/// The exact p_13 outcome: a styled whole-paragraph replace whose content echoes
/// the literal "1.\t" prefix on a paragraph already carrying literal_prefix "1.".
/// The prefix-duplication guard (spec_prefix_duplication.rs) fires FIRST and
/// refuses with PrefixDuplicatesLabel — the agent learns the numbering is already
/// present, instead of a silent applied=true (or the now-superseded
/// silent strip that left a no-op). This is the single most valuable string in
/// the fix: it breaks the doubling chain at step one.
#[test]
fn literal_prefix_styled_replace_echoing_label_is_refused() {
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(
        literal_prefix_para("p1"),
    ))]);

    // Content that echoes the label: "1.\t" then "Events".
    let content = ParagraphContent {
        fragments: vec![
            ContentFragment::StyledText {
                text: "1.\t".to_string(),
                marks: InlineMarkSet::default(),
            },
            ContentFragment::Text("Events".to_string()),
        ],
    };
    let tx = transaction(
        vec![replace_step("p1", "Events", content)],
        MaterializationMode::TrackedChange,
    );

    let err = apply_transaction(&doc, &tx)
        .expect_err("echoing the literal_prefix label must fail loud, not silently strip/apply");
    assert!(
        matches!(err, EditError::PrefixDuplicatesLabel { ref label, .. } if label == "1."),
        "expected PrefixDuplicatesLabel for '1.', got {err:?}"
    );
}

/// The strip's legitimate-NO-overreach case: a paragraph WITHOUT any numbering
/// (no literal_prefix, no numPr) keeps a literal "1.\t" in the replacement as
/// REAL content — there is nothing to duplicate, so the strip must not fire and
/// the edit applies (the "1." becomes visible body text).
#[test]
fn replace_with_literal_number_on_unnumbered_paragraph_is_a_real_edit() {
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(make_para(
        "p1",
        normal_segment(vec![make_text("p1_t1", "Events")]),
    )))]);

    let content = ParagraphContent {
        fragments: vec![ContentFragment::Text("1.\tEvents".to_string())],
    };
    let tx = transaction(
        vec![replace_step("p1", "Events", content)],
        MaterializationMode::TrackedChange,
    );

    let (after, _) = apply_transaction(&doc, &tx)
        .expect("a literal '1.' on an unnumbered paragraph is a real edit, not a no-op");
    // accept_all, then read the FULL visible content. The "1." the agent added
    // survives: after accept, post-projection normalization re-hoists a leading
    // literal enumeration label out of the runs into `literal_prefix` (which the
    // serializer re-emits as a real run), so the "1." may live there rather than
    // in a TextNode. Both placements are "real content" — read both.
    let mut accepted = after;
    stemma::accept_all(&mut accepted);
    let para = accepted
        .blocks
        .iter()
        .find_map(|tb| match &tb.block {
            BlockNode::Paragraph(p) if p.id == NodeId::from("p1") => Some(p),
            _ => None,
        })
        .expect("p1");
    let mut text = para.literal_prefix.clone().unwrap_or_default();
    for seg in &para.segments {
        for inl in &seg.inlines {
            if let InlineNode::Text(t) = inl {
                text.push_str(&t.text);
            }
        }
    }
    assert!(
        text.contains("1.") && text.contains("Events"),
        "the literal '1.\\tEvents' must be applied as real content (in runs or literal_prefix), \
         got {text:?}"
    );
}
