//! Spec test: a structural op's destination anchor must never resolve
//! against a tracked-move SOURCE (a `w:moveFrom` shadow left behind at its
//! OLD position).
//!
//! Forensic background: an agent authoring several single-block `move` ops
//! in one transaction chained each hop's anchor onto the PREVIOUS hop's
//! source block id (`move p22 after p6`, then `move p23 after p22`). In
//! tracked mode, the first move flips `p22` to `Deleted` + `move_id` and
//! inserts a fresh-id copy at the destination — `p22` itself never moves,
//! it just becomes a shadow sitting where it always was. The second op's
//! anchor `"p22"` then silently resolved against that shadow's stale
//! position instead of refusing, and the transaction "succeeded" with
//! scattered block order. Per the no-silent-fallback rule, an ambiguous
//! anchor must refuse (`EditError::AmbiguousAnchorAfterMove`), not guess.
//!
//! The WORKING pattern — several moves all anchored on the SAME anchor that
//! is itself never moved (`move p22 after p6`, `move p23 after p6`, ...) —
//! must keep landing the moved blocks in sequence after that anchor, exactly
//! as before this guard existed.

use stemma::domain::*;
use stemma::edit::*;
use stemma::{accept_all, reject_all_with_styles};

// ─── Test helpers (mirrors stemma-engine/tests/edit_basic.rs) ──────────────

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
        author: Some("Test Author".to_string()),
        date: Some("2026-07-02T00:00:00Z".to_string()),
        apply_op_id: None,
    }
}

fn all_para_texts(doc: &CanonDoc) -> Vec<String> {
    doc.blocks
        .iter()
        .filter_map(|tb| match &tb.block {
            BlockNode::Paragraph(p) => Some(
                p.segments
                    .iter()
                    .flat_map(|seg| seg.inlines.iter())
                    .filter_map(|inline| match inline {
                        InlineNode::Text(t) => Some(t.text.as_str()),
                        _ => None,
                    })
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect()
}

/// Eight-paragraph synthetic doc: p1="one", p2="two", ..., p8="eight".
fn eight_paragraph_doc() -> CanonDoc {
    let words = [
        "one", "two", "three", "four", "five", "six", "seven", "eight",
    ];
    let blocks = words
        .iter()
        .enumerate()
        .map(|(i, word)| {
            let id = format!("p{}", i + 1);
            let text_id = format!("t{}", i + 1);
            let para = make_para(&id, normal_segment(vec![make_text(&text_id, word)]));
            normal_tracked_block(BlockNode::from(para))
        })
        .collect();
    make_doc(blocks)
}

fn move_step(from: &str, to: &str, dest_anchor: &str) -> EditStep {
    EditStep::MoveBlockRange {
        from_block_id: NodeId::from(from),
        to_block_id: NodeId::from(to),
        dest_anchor_id: NodeId::from(dest_anchor),
        dest_position: InsertPosition::After,
        rationale: None,
        expect: None,
        semantic_hash: None,
    }
}

fn transaction(steps: Vec<EditStep>) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    }
}

// ─── (a) Chained anchor across two ops in ONE transaction: refuse ──────────

#[test]
fn chained_move_anchor_on_just_moved_source_refuses() {
    // "Move a contiguous section" via successive single-block moves, each
    // anchored on the PREVIOUS op's source id — the exact pattern that
    // silently scattered blocks before this guard existed.
    let doc = eight_paragraph_doc();
    let original_order = all_para_texts(&doc);

    let tx = transaction(vec![
        move_step("p5", "p5", "p2"), // op 0: move p5 after p2
        move_step("p6", "p6", "p5"), // op 1: BUG SITE — p5 was just moved
    ]);

    let err = apply_transaction(&doc, &tx)
        .expect_err("anchoring on a block moved earlier in the same transaction must refuse");

    match &err {
        EditError::AmbiguousAnchorAfterMove {
            anchor_id,
            moved_by_step_index,
            moved_to_block_id,
            step_index,
        } => {
            assert_eq!(anchor_id, &NodeId::from("p5"));
            assert_eq!(
                *moved_by_step_index,
                Some(0),
                "must name the step index (within this transaction) that moved the anchor"
            );
            assert_eq!(
                moved_to_block_id,
                &Some(NodeId::from("p5__ins1")),
                "must name the moveTo copy holding p5's content now"
            );
            assert_eq!(*step_index, 1, "the refusal belongs to the second move op");
        }
        other => panic!("expected AmbiguousAnchorAfterMove, got: {other:?}"),
    }

    // The error message is what an LLM retry loop reads — it must be
    // actionable without inspecting the structured fields.
    let message = err.to_string();
    assert!(
        message.contains("p5") && message.contains("p5__ins1"),
        "error message must name both the stale anchor and its moved copy: {message}"
    );

    // Transaction atomicity: apply_transaction operates on a clone, so a
    // failure never touches the caller's document.
    assert_eq!(
        all_para_texts(&doc),
        original_order,
        "a failed transaction must leave the document exactly as it was"
    );
}

// ─── (b) Same pattern, but the WORKING fixed-anchor form: must still work ──

#[test]
fn successive_moves_to_the_same_fixed_anchor_still_land_in_order() {
    // Every move anchors on p2, which is NEVER itself moved — the
    // legitimate way to relocate a contiguous run one block at a time.
    // `InsertOrderState::by_anchor` chains p2 -> the last block landed
    // after it, so each successive move lands right after the previous
    // one, in the order the ops were issued.
    let doc = eight_paragraph_doc();

    let tx = transaction(vec![
        move_step("p5", "p5", "p2"),
        move_step("p6", "p6", "p2"),
        move_step("p7", "p7", "p2"),
    ]);

    let result = apply_transaction(&doc, &tx)
        .expect("moves anchored on the same untouched block must not be flagged ambiguous")
        .0;

    let mut accepted = result.clone();
    accept_all(&mut accepted);
    assert_eq!(
        all_para_texts(&accepted),
        vec![
            "one", "two", "five", "six", "seven", "three", "four", "eight"
        ],
        "accept-all must land p5, p6, p7 after p2 in issue order"
    );

    let mut rejected = result;
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        all_para_texts(&rejected),
        vec![
            "one", "two", "three", "four", "five", "six", "seven", "eight"
        ],
        "reject-all must restore the original order"
    );
}

// ─── (c) A SECOND transaction anchoring on an already-committed move source ─

#[test]
fn second_transaction_anchoring_on_a_committed_move_source_refuses() {
    let doc = eight_paragraph_doc();

    // Transaction 1: move p5 after p2. Commit it (take the .0 as the new
    // live document), exactly as a caller would between two apply_edit
    // calls.
    let tx1 = transaction(vec![move_step("p5", "p5", "p2")]);
    let committed = apply_transaction(&doc, &tx1)
        .expect("first transaction should apply cleanly")
        .0;

    // Sanity: p5 is now a moveFrom shadow sitting at its old position.
    let p5_shadow = committed
        .blocks
        .iter()
        .find(|tb| matches!(&tb.block, BlockNode::Paragraph(p) if p.id == NodeId::from("p5")))
        .expect("p5 shadow still present");
    assert!(matches!(p5_shadow.status, TrackingStatus::Deleted(_)));
    assert!(p5_shadow.move_id.is_some());

    // Transaction 2: move p7 anchored on p5 — a moveFrom shadow from an
    // ALREADY-COMMITTED transaction, not this one.
    let tx2 = transaction(vec![move_step("p7", "p7", "p5")]);
    let err = apply_transaction(&committed, &tx2)
        .expect_err("anchoring on a moveFrom shadow from a prior transaction must refuse");

    match &err {
        EditError::AmbiguousAnchorAfterMove {
            anchor_id,
            moved_by_step_index,
            moved_to_block_id,
            step_index,
        } => {
            assert_eq!(anchor_id, &NodeId::from("p5"));
            assert_eq!(
                *moved_by_step_index, None,
                "the move happened in a PRIOR transaction, so there is no same-transaction \
                 step index to name"
            );
            assert_eq!(moved_to_block_id, &Some(NodeId::from("p5__ins1")));
            assert_eq!(*step_index, 0);
        }
        other => panic!("expected AmbiguousAnchorAfterMove, got: {other:?}"),
    }
}

// ─── (d) The guard also covers `insert`'s destination anchor, not just `move` ─

#[test]
fn insert_destination_anchor_on_just_moved_source_refuses() {
    let doc = eight_paragraph_doc();

    let tx = transaction(vec![
        move_step("p5", "p5", "p2"), // op 0: move p5 after p2
        EditStep::InsertParagraphs {
            anchor_block_id: NodeId::from("p5"), // op 1: BUG SITE
            position: InsertPosition::After,
            rationale: None,
            blocks: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
                role: Some("body_text".to_string()),
                content: parse_paragraph_markup("nine").unwrap(),
                restart_numbering: false,
                list: None,
            })],
        },
    ]);

    let err = apply_transaction(&doc, &tx)
        .expect_err("insert anchored on a just-moved source must refuse, same as move");

    match &err {
        EditError::AmbiguousAnchorAfterMove {
            anchor_id,
            moved_by_step_index,
            moved_to_block_id,
            step_index,
        } => {
            assert_eq!(anchor_id, &NodeId::from("p5"));
            assert_eq!(*moved_by_step_index, Some(0));
            assert_eq!(moved_to_block_id, &Some(NodeId::from("p5__ins1")));
            assert_eq!(*step_index, 1);
        }
        other => panic!("expected AmbiguousAnchorAfterMove, got: {other:?}"),
    }
}

// ─── (e) The refusal must never PANIC when the moveTo copy is unlocatable.
// The importer tags blocks between move-range markers as encountered and does
// NOT validate pairing, so a dirty DOCX can import an unpaired `w:moveFrom`
// shadow. Anchoring on it is still ambiguous — refuse — but the copy hint is
// unavailable and must degrade to None, not crash. (The other conceivable
// route to an unpaired shadow — tracked-deleting the moveTo copy — is
// unreachable: `DeleteBlockRange` refuses Inserted blocks with
// `BlockHasTrackedStatus`.) ─────────────────────────────────────────────────

fn insert_after(anchor: &str) -> EditStep {
    EditStep::InsertParagraphs {
        anchor_block_id: NodeId::from(anchor),
        position: InsertPosition::After,
        rationale: None,
        blocks: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
            role: Some("body_text".to_string()),
            content: parse_paragraph_markup("nine").unwrap(),
            restart_numbering: false,
            list: None,
        })],
    }
}

#[test]
fn anchor_on_unpaired_move_source_refuses_without_panicking() {
    // Simulate a dirty import: p5 is a moveFrom shadow with NO moveTo half.
    let mut doc = eight_paragraph_doc();
    let idx = doc
        .blocks
        .iter()
        .position(|tb| matches!(&tb.block, BlockNode::Paragraph(p) if p.id == NodeId::from("p5")))
        .unwrap();
    doc.blocks[idx].status = TrackingStatus::Deleted(test_revision());
    doc.blocks[idx].move_id = Some("mv-unpaired".to_string());

    let tx = transaction(vec![insert_after("p5")]);
    let err = apply_transaction(&doc, &tx)
        .expect_err("anchoring on an unpaired moveFrom shadow must refuse");
    match &err {
        EditError::AmbiguousAnchorAfterMove {
            anchor_id,
            moved_to_block_id,
            ..
        } => {
            assert_eq!(anchor_id, &NodeId::from("p5"));
            assert_eq!(
                *moved_to_block_id, None,
                "no moveTo half exists, so the hint must degrade, not invent an id"
            );
        }
        other => panic!("expected AmbiguousAnchorAfterMove, got: {other:?}"),
    }
    let message = err.to_string();
    assert!(
        message.contains("stable neighbor"),
        "hint-less refusal must still tell the caller what to do: {message}"
    );
}
