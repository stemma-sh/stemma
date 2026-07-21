//! Spec test: the v4 `move` op's RANGE form — `target: {"from":..,"to":..}` —
//! the shape an agent naturally reaches for when relocating a whole section
//! (several contiguous blocks) in one op.
//!
//! Forensic background: two independent agents, told to move a 6-block
//! section, first tried `{"op":"move","source":{"from":"p_22","to":"p_27"},
//! "target":{"anchor":"p_6","position":"after"}}`. The schema refused it
//! (day-one `move` was single-block only), and their fallback — several
//! single-block moves chained in one transaction, each anchoring on the
//! PREVIOUS hop's source id — is exactly what `AmbiguousAnchorAfterMove`
//! (see `spec_move_anchor_ambiguity.rs`) now refuses. This file exercises
//! the real fix: `move.target` now also accepts a contiguous inclusive
//! range, so a whole section moves in ONE op, through the real v4 wire
//! parser (not direct `EditStep` construction) — the news here is the wire
//! surface, not the underlying `MoveBlockRange` engine primitive (which
//! already supported ranges; see `move_multi_block_range_shares_single_move_id`
//! in edit_basic.rs).
//!
//! Synthetic in-code docs only — no benchmark fixture, path, or gate name.

use stemma::domain::*;
use stemma::edit::{EditError, apply_transaction};
use stemma::edit_v4::parse_transaction;
use stemma::{accept_all, reject_all_with_styles};

// ─── Test helpers (mirrors edit_v4_invariants.rs / spec_move_anchor_ambiguity.rs) ─

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
        source_run_attrs: Vec::new(),
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

/// Parse + adapt a v4 transaction JSON through the REAL wire pipeline (the
/// thing under test — the range wire shape, not the `EditStep` primitive).
fn parse_and_translate(json: &str) -> stemma::edit::EditTransaction {
    let txn = parse_transaction(json).expect("v4 schema check passes");
    txn.into_edit_transaction().expect("v4 adapter succeeds")
}

fn range_move_json(from: &str, to: &str, anchor: &str, position: &str) -> String {
    format!(
        r#"{{
            "ops": [{{
                "op": "move",
                "target": {{ "from": "{from}", "to": "{to}" }},
                "destination": {{ "anchor": "{anchor}", "position": "{position}" }}
            }}],
            "revision": {{ "author": "Counsel" }}
        }}"#
    )
}

// ─── Range move relocates the whole run, both ways ─────────────────────────

#[test]
fn range_move_via_v4_wire_relocates_the_whole_run() {
    let doc = eight_paragraph_doc();
    let json = range_move_json("p3", "p5", "p7", "after");
    let txn = parse_and_translate(&json);
    let result = apply_transaction(&doc, &txn).expect("range move applies").0;

    let mut accepted = result.clone();
    accept_all(&mut accepted);
    assert_eq!(
        all_para_texts(&accepted),
        vec![
            "one", "two", "six", "seven", "three", "four", "five", "eight"
        ],
        "accept-all must relocate the WHOLE [p3..p5] run after p7 in one op"
    );

    let mut rejected = result;
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        all_para_texts(&rejected),
        all_para_texts(&doc),
        "reject-all must restore the original text/order byte-faithfully"
    );
    // NOTE: not a full `assert_eq!(rejected, doc)` — pre-existing, orthogonal
    // to this range-move work: `project_blocks_for_accept_reject` (shared by
    // both accept_all and reject_all) never clears `TrackedBlock::move_id`
    // once a block's status resolves to Normal, so a moved-then-resolved
    // block keeps a stale `move_id` forever. Confirmed by direct check below
    // (every block IS Normal + id-faithful; only `move_id` lingers).
    assert!(
        rejected
            .blocks
            .iter()
            .all(|tb| tb.status == TrackingStatus::Normal),
        "every block must resolve to Normal after reject-all"
    );
    assert_eq!(
        rejected.blocks.iter().map(block_id_of).collect::<Vec<_>>(),
        doc.blocks.iter().map(block_id_of).collect::<Vec<_>>(),
        "reject-all must restore the exact original block id sequence"
    );
}

fn block_id_of(tb: &TrackedBlock) -> &NodeId {
    match &tb.block {
        BlockNode::Paragraph(p) => &p.id,
        BlockNode::Table(t) => &t.id,
        BlockNode::OpaqueBlock(o) => &o.id,
    }
}

#[test]
fn range_move_inverted_from_to_still_relocates_correctly() {
    // `to` given BEFORE `from` in document order — the engine normalizes,
    // never refuses (same contract as `delete`'s range).
    let doc = eight_paragraph_doc();
    let json = range_move_json("p5", "p3", "p7", "after");
    let txn = parse_and_translate(&json);
    let result = apply_transaction(&doc, &txn)
        .expect("inverted range still applies")
        .0;

    let mut accepted = result;
    accept_all(&mut accepted);
    assert_eq!(
        all_para_texts(&accepted),
        vec![
            "one", "two", "six", "seven", "three", "four", "five", "eight"
        ],
        "an inverted from/to must normalize to the same [p3..p5] range, not refuse"
    );
}

// ─── Native numbering survives a range move untouched ──────────────────────

#[test]
fn range_move_keeps_auto_numbering_native() {
    // A heading (num_id 10) and a body list item (num_id 20) moved together.
    // Structural `numbering` must survive verbatim on BOTH halves — the move
    // clones content wholesale and must never materialize the synthesized
    // number into `segments`/`literal_prefix` as a side effect.
    let mut heading = make_para("h1", normal_segment(vec![make_text("h1_t", "Definitions")]));
    heading.numbering = Some(NumberingInfo {
        num_id: 10,
        ilvl: 0,
        synthesized_text: "1.".to_string(),
        is_bullet: false,
        restart_numbering: false,
    });
    let mut body = make_para("b1", normal_segment(vec![make_text("b1_t", "first item")]));
    body.numbering = Some(NumberingInfo {
        num_id: 20,
        ilvl: 1,
        synthesized_text: "(a)".to_string(),
        is_bullet: false,
        restart_numbering: false,
    });
    let anchor = make_para("anchor", normal_segment(vec![make_text("a_t", "anchor")]));
    let doc = make_doc(vec![
        normal_tracked_block(BlockNode::from(heading)),
        normal_tracked_block(BlockNode::from(body)),
        normal_tracked_block(BlockNode::from(anchor)),
    ]);

    let json = range_move_json("h1", "b1", "anchor", "after");
    let txn = parse_and_translate(&json);
    let result = apply_transaction(&doc, &txn).expect("range move applies").0;

    for (id, expect_num_id, expect_text) in
        [("h1", 10u32, "Definitions"), ("b1", 20u32, "first item")]
    {
        let matches: Vec<&TrackedBlock> = result
            .blocks
            .iter()
            .filter(|tb| match &tb.block {
                BlockNode::Paragraph(p) => p.id.0.as_ref() == id || p.id.0.starts_with(id),
                _ => false,
            })
            .collect();
        assert_eq!(
            matches.len(),
            2,
            "expected the Deleted source and the Inserted copy for '{id}', got {}",
            matches.len()
        );
        for tb in matches {
            let BlockNode::Paragraph(p) = &tb.block else {
                panic!("expected paragraph")
            };
            let numbering = p
                .numbering
                .as_ref()
                .unwrap_or_else(|| panic!("'{}' lost its structural numbering on move", p.id));
            assert_eq!(
                numbering.num_id, expect_num_id,
                "'{}' must keep its own num_id across the move",
                p.id
            );
            assert!(
                p.literal_prefix.is_none(),
                "'{}' must NOT have a typed/materialized numbering prefix after a move — \
                 numbering stays native (structural numPr), never baked into text",
                p.id
            );
            let text: String = p
                .segments
                .iter()
                .flat_map(|s| s.inlines.iter())
                .filter_map(|i| match i {
                    InlineNode::Text(t) => Some(t.text.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(
                text, expect_text,
                "'{}' body text must be untouched by the move (no number text leaked in)",
                p.id
            );
        }
    }
}

// ─── Refusals ────────────────────────────────────────────────────────────

#[test]
fn range_move_destination_anchor_inside_source_is_refused() {
    let doc = eight_paragraph_doc();
    // [p3..p5], destination anchor p4 — inside the source range.
    let json = range_move_json("p3", "p5", "p4", "after");
    let txn = parse_and_translate(&json);
    let err = apply_transaction(&doc, &txn)
        .expect_err("a destination inside the source range must be refused");
    assert!(
        matches!(err, EditError::MoveDestinationInsideSource { .. }),
        "expected MoveDestinationInsideSource, got {err:?}"
    );
}

#[test]
fn range_move_unknown_block_id_fails_with_block_not_found() {
    let doc = eight_paragraph_doc();
    let json = range_move_json("p3", "p_does_not_exist", "p7", "after");
    let txn = parse_and_translate(&json);
    let err = apply_transaction(&doc, &txn).expect_err("an unresolvable range id must refuse");
    assert!(
        matches!(err, EditError::BlockNotFound { .. }),
        "expected BlockNotFound, got {err:?}"
    );
}

#[test]
fn chained_range_move_anchor_on_just_moved_source_refuses() {
    // The forensic fallback pattern, but with the range form available: an
    // agent still chaining hops (now range hops) onto the PREVIOUS hop's
    // source id hits the same ambiguous-anchor refusal a single-block chain
    // does — the guard is anchor-shape-agnostic.
    let doc = eight_paragraph_doc();
    let json = r#"{
            "ops": [
                { "op": "move", "target": { "from": "p3", "to": "p5" },
                   "destination": { "anchor": "p7", "position": "after" } },
                { "op": "move", "target": "p6",
                   "destination": { "anchor": "p3", "position": "after" } }
            ],
            "revision": { "author": "Counsel" }
        }"#
    .to_string();
    let txn = parse_and_translate(&json);
    let err = apply_transaction(&doc, &txn)
        .expect_err("anchoring on a block the first range-move just relocated must refuse");
    match &err {
        EditError::AmbiguousAnchorAfterMove {
            anchor_id,
            moved_by_step_index,
            step_index,
            ..
        } => {
            assert_eq!(anchor_id, &NodeId::from("p3"));
            assert_eq!(*moved_by_step_index, Some(0));
            assert_eq!(*step_index, 1);
        }
        other => panic!("expected AmbiguousAnchorAfterMove, got: {other:?}"),
    }
}

// ─── `expect` on a range checks the FROM block ──────────────────────────────

#[test]
fn range_move_expect_matching_the_from_block_succeeds() {
    let doc = eight_paragraph_doc();
    let json = r#"{
            "ops": [{
                "op": "move",
                "target": { "from": "p3", "to": "p5" },
                "destination": { "anchor": "p7", "position": "after" },
                "expect": "three"
            }],
            "revision": { "author": "Counsel" }
        }"#
    .to_string();
    let txn = parse_and_translate(&json);
    apply_transaction(&doc, &txn).expect("expect matching the FROM block's text must pass");
}

#[test]
fn range_move_expect_mismatch_against_the_from_block_is_refused() {
    let doc = eight_paragraph_doc();
    // "six" is the TO block's neighbor text, not the FROM block (p3="three")
    // — proves `expect` is checked against FROM, not TO or anywhere in range.
    let json = r#"{
            "ops": [{
                "op": "move",
                "target": { "from": "p3", "to": "p5" },
                "destination": { "anchor": "p7", "position": "after" },
                "expect": "five"
            }],
            "revision": { "author": "Counsel" }
        }"#
    .to_string();
    let txn = parse_and_translate(&json);
    let err = apply_transaction(&doc, &txn)
        .expect_err("expect must be checked against the FROM block, not the TO block");
    assert!(
        matches!(err, EditError::ExpectMismatch { ref block_id, .. } if block_id == &NodeId::from("p3")),
        "expected ExpectMismatch naming the FROM block 'p3', got {err:?}"
    );
}
