//! Named-invariant tests for LLM Edit Schema v4.
//!
//! One test per named v4 edit-schema invariant (enumerated below). Each test
//! pins the invariant by exercising a minimal failure case and asserting the
//! engine refuses with the right error.
//!
//! Invariants:
//! 1. Kind matching on replace
//! 2. Opaque set-equality
//! 3. Schema validity
//! 4. Diff output is always rehydratable
//! 5. Replacement-payload identity boundary
//! 6. Move well-formedness
//!
//! Invariants 1, 2 (document half), and 6 (document half) need a live
//! CanonDoc to exercise; invariants 3 and 5 are enforced by the v4
//! schema-check layer and the type system respectively; invariant 4 is a
//! property of the engine's diff machinery and is covered by the existing
//! diff and tracked-change test suites — we add a smoke test here that
//! routes through the v4 adapter.

use stemma::domain::*;
use stemma::edit::{EditError, EditStep, EditTransaction, MaterializationMode, apply_transaction};
use stemma::edit_v4::*;

// ─── Doc-construction helpers ────────────────────────────────────────────────
//
// Minimal-shape helpers focused on what the invariant tests need. We do not
// reuse `tests/edit_basic.rs` helpers because integration tests do not share
// modules — duplicating the few bits we need keeps each invariant test
// self-contained.

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
        source_run_attrs: Vec::new(),
        formatting_change: None,
    })
}

fn make_opaque(id: &str) -> InlineNode {
    InlineNode::from(OpaqueInlineNode {
        id: NodeId::from(id),
        kind: OpaqueKind::Drawing,
        opaque_ref: format!("opaque_{id}"),
        proof_ref: ProofRef {
            part: DocPart::DocumentXml,
            block_id: NodeId::from("p1"),
            docx_anchor: String::new(),
        },
        wrapper_marks: Vec::new(),
        wrapper_style_props: StyleProps::default(),
        raw_xml: Some(b"<w:drawing/>".to_vec()),
        content_hash: None,
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

fn simple_doc_with_text(para_id: &str, text: &str) -> CanonDoc {
    let para = make_para(
        para_id,
        normal_segment(vec![make_text(&format!("{para_id}_t1"), text)]),
    );
    make_doc(vec![normal_tracked_block(BlockNode::from(para))])
}

fn doc_with_opaque(para_id: &str, opaque_id: &str) -> CanonDoc {
    let para = make_para(
        para_id,
        normal_segment(vec![
            make_text(&format!("{para_id}_t1"), "before "),
            make_opaque(opaque_id),
            make_text(&format!("{para_id}_t2"), " after"),
        ]),
    );
    make_doc(vec![normal_tracked_block(BlockNode::from(para))])
}

fn parse_and_translate(json: &str) -> EditTransaction {
    let txn = parse_transaction(json).expect("v4 schema check passes");
    txn.into_edit_transaction().expect("v4 adapter succeeds")
}

// ─── Invariant 1: Kind matching on replace ───────────────────────────────────
//
// Spec: "A replace payload's root node must have the same type as the target
// node. A paragraph cannot be replaced by a hyperlink." Today the schema
// layer rejects unaddressable payloads (text, opaque_ref). The
// document-level check — that a hyperlink payload targets an actual
// hyperlink, not a paragraph — is enforced by the engine at apply time
// (HyperlinkNotFound when the target id is not a hyperlink).

#[test]
fn invariant_1_kind_matching_hyperlink_payload_against_paragraph_target() {
    // A v4 replace where content.type=hyperlink but target is a paragraph id.
    // The adapter routes this through ReplaceHyperlinkText with the
    // paragraph's id as hyperlink_id; the engine then fails to find a
    // hyperlink with that id and returns HyperlinkNotFound.
    let doc = simple_doc_with_text("p1", "Hello world.");
    let json = r#"
    {
      "ops": [{
        "op": "replace",
        "target": "p1",
        "expect": "Hello",
        "content": {
          "type": "hyperlink",
          "attrs": { "href": "https://example.com" },
          "content": [{ "type": "text", "text": "click here" }]
        }
      }],
      "revision": { "author": "Counsel" }
    }
    "#;
    let txn = parse_and_translate(json);
    let err = apply_transaction(&doc, &txn).expect_err("kind mismatch must fail");
    assert!(
        matches!(err, EditError::HyperlinkNotFound { .. }),
        "expected HyperlinkNotFound, got {err:?}"
    );
}

// ─── Invariant 2: Opaque set-equality ────────────────────────────────────────
//
// Spec: "The set of opaque ids in a replace payload's subtree must equal the
// set of opaque ids in the target's subtree. Neither dropped... nor
// foreign-added." Three angles:
//  a) Dropped from payload (target has op_2; payload doesn't)
//  b) Foreign added to payload (payload references op_X that's not in target)
//  c) Duplicated in payload (same id twice — schema layer)
//
// (c) is covered by edit_v4 unit tests; (a) and (b) by the engine, accessed
// via the v4 path here.

#[test]
fn invariant_2_opaque_set_equality_rejects_dropped_opaque() {
    let doc = doc_with_opaque("p1", "op_2");
    // Replace payload omits op_2.
    let json = r#"
    {
      "ops": [{
        "op": "replace",
        "target": "p1",
        "expect": "before",
        "content": {
          "type": "paragraph",
          "content": [{ "type": "text", "text": "completely new text" }]
        }
      }],
      "revision": { "author": "Counsel" }
    }
    "#;
    let txn = parse_and_translate(json);
    let err = apply_transaction(&doc, &txn).expect_err("missing opaque must fail");
    let EditError::OpaqueDestroyed {
        missing_opaque_ids, ..
    } = err
    else {
        panic!("expected OpaqueDestroyed, got something else");
    };
    assert_eq!(missing_opaque_ids, vec!["op_2".to_string()]);
}

#[test]
fn invariant_2_opaque_set_equality_rejects_foreign_opaque_ref() {
    let doc = doc_with_opaque("p1", "op_2");
    // Replace payload references op_99 which is not in the target paragraph.
    let json = r#"
    {
      "ops": [{
        "op": "replace",
        "target": "p1",
        "expect": "before",
        "content": {
          "type": "paragraph",
          "content": [
            { "type": "text", "text": "before " },
            { "type": "opaque_ref", "attrs": { "id": "op_99" } },
            { "type": "text", "text": " after" }
          ]
        }
      }],
      "revision": { "author": "Counsel" }
    }
    "#;
    let txn = parse_and_translate(json);
    let err = apply_transaction(&doc, &txn).expect_err("foreign opaque ref must fail");
    assert!(
        matches!(
            err,
            EditError::PreservedInlineNotFound { .. } | EditError::OpaqueDestroyed { .. }
        ),
        "expected PreservedInlineNotFound or OpaqueDestroyed, got {err:?}"
    );
}

// ─── Invariant 3: Schema validity ────────────────────────────────────────────
//
// Spec: "The replacement subtree must satisfy the content expression for its
// kind." Largely enforced by serde's typed parse. We add one test that pins
// the case of a paragraph with content that fails to deserialize as the
// content-expression of the inline grammar.

#[test]
fn invariant_3_schema_validity_rejects_block_inside_inline_position() {
    // `paragraph.content` is `Vec<Inline>`. A block-shaped object (table)
    // placed there fails the content expression. Serde rejects it because
    // the type tag `table` is not a valid Inline variant.
    let json = r#"
    {
      "ops": [{
        "op": "replace",
        "target": "p1",
        "expect": "x",
        "content": {
          "type": "paragraph",
          "content": [
            { "type": "table", "content": [] }
          ]
        }
      }],
      "revision": { "author": "Counsel" }
    }
    "#;
    let err = parse_transaction(json).expect_err("schema violation must fail at parse");
    assert!(
        matches!(err, SchemaError::JsonParseError { .. }),
        "expected JsonParseError from serde, got {err:?}"
    );
}

// ─── Invariant 4: Diff output is always rehydratable ─────────────────────────
//
// Spec: "Inline diff operates on a two-level flattening: structural
// segmentation first (hyperlink open/close and opaque_ref markers move as
// whole-span units, never split), word-level Myers within matched
// segments." This is a property of the engine's diff machinery. We add a
// smoke test that routes through the v4 adapter and confirms the diff
// output is a valid TrackedSegment sequence (no unbalanced markers).

#[test]
fn invariant_4_two_level_diff_keeps_opaque_anchor_intact() {
    let doc = doc_with_opaque("p1", "op_2");
    // Replace surrounding text but keep the opaque ref in place.
    let json = r#"
    {
      "ops": [{
        "op": "replace",
        "target": "p1",
        "expect": "before",
        "content": {
          "type": "paragraph",
          "content": [
            { "type": "text", "text": "still before " },
            { "type": "opaque_ref", "attrs": { "id": "op_2" } },
            { "type": "text", "text": " still after" }
          ]
        }
      }],
      "revision": { "author": "Counsel" }
    }
    "#;
    let txn = parse_and_translate(json);
    let doc = apply_transaction(&doc, &txn)
        .expect("two-level diff produces a valid result")
        .0;
    // Confirm the opaque inline still appears in the resulting paragraph.
    let mut found_opaque = false;
    for block in &doc.blocks {
        if let BlockNode::Paragraph(p) = &block.block {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::OpaqueInline(o) = inline
                        && o.id.0.as_ref() == "op_2"
                    {
                        found_opaque = true;
                    }
                }
            }
        }
    }
    assert!(found_opaque, "opaque inline must survive the diff intact");
}

// ─── Invariant 4b: replace(table) routes through the table diff ──────────────
//
// Pins that the v4 adapter's `replace(table)` path doesn't degrade to a
// block-level delete + insert (which would lose row/cell-level
// alignment). After applying a `replace(table)` that inserts one new
// row, the merged table must still be a *single* table block carrying
// the row-level Inserted marker on the new row — proof that the diff
// fed `apply_table_structure_changed` instead of falling back.

fn make_simple_table(id: &str, cell_text: &str) -> BlockNode {
    BlockNode::from(TableNode {
        id: NodeId::from(id),
        rows: vec![TableRowNode {
            id: NodeId::from(format!("{id}_r0")),
            cells: vec![TableCellNode {
                id: NodeId::from(format!("{id}_c0")),
                blocks: vec![BlockNode::from(make_para(
                    &format!("{id}_c0_p"),
                    normal_segment(vec![make_text(&format!("{id}_c0_t"), cell_text)]),
                ))],
                grid_span: 1,
                v_merge: VerticalMerge::None,
                formatting: CellFormatting::default(),
                formatting_change: None,
                tracking_status: None,
                row_sdt_wrapper: None,
                content_sdt_wraps: Vec::new(),
                cnf_style: None,
                hide_mark: false,
                preserved: Vec::new(),
            }],
            grid_before: 0,
            grid_after: 0,
            tracking_status: None,
            is_header: false,
            height: None,
            height_rule: None,
            formatting_change: None,
            para_id: None,
            text_id: None,
            cant_split: false,
            jc: None,
            w_before: None,
            w_after: None,
            cnf_style: None,
            tbl_pr_ex: None,
            cell_spacing: None,
            preserved: Vec::new(),
        }],
        structure_hash: String::new(),
        formatting: TableFormatting::default(),
        formatting_change: None,
    })
}

#[test]
fn invariant_4b_table_replace_routes_through_diff() {
    // Doc with an exemplar body paragraph + a 1-row table.
    let body = make_para(
        "body_exemplar",
        normal_segment(vec![make_text("body_t", "body")]),
    );
    let table = make_simple_table("t1", "row A");
    let doc = CanonDoc {
        id: NodeId::from("doc1"),
        blocks: vec![
            normal_tracked_block(BlockNode::from(body)),
            normal_tracked_block(table),
        ],
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
    };

    let json = r#"
    {
      "ops": [{
        "op": "replace",
        "target": "t1",
        "content": {
          "type": "table",
          "content": [
            { "content": [{ "content": [
              { "type": "paragraph", "role": "body_text", "content": [{ "type": "text", "text": "row A" }] }
            ] }] },
            { "content": [{ "content": [
              { "type": "paragraph", "role": "body_text", "content": [{ "type": "text", "text": "row B" }] }
            ] }] }
          ]
        }
      }],
      "revision": { "author": "Counsel" }
    }
    "#;
    let txn = parse_and_translate(json);
    let edited = apply_transaction(&doc, &txn).expect("apply succeeds").0;

    // The merged document must still have the table at id "t1" — not
    // replaced by a different block kind, not split into delete+insert.
    let nid = NodeId::from("t1");
    let tbl = edited
        .blocks
        .iter()
        .find_map(|tb| match &tb.block {
            BlockNode::Table(t) if t.id == nid => Some(t),
            _ => None,
        })
        .expect("merged doc must contain a table with id 't1'");
    // Two rows: the matched row A and the inserted row B with
    // row-level TrackingStatus::Inserted (drives w:trPr/w:ins).
    assert_eq!(
        tbl.rows.len(),
        2,
        "must produce one merged table with two rows"
    );
    assert!(tbl.rows[0].tracking_status.is_none(), "row 0 stays matched");
    assert!(
        matches!(
            tbl.rows[1].tracking_status,
            Some(TrackingStatus::Inserted(_))
        ),
        "row 1 must carry TrackingStatus::Inserted from the diff path"
    );
}

// ─── Invariant 5: Replacement-payload identity boundary ──────────────────────
//
// Spec: "Fresh nodes in insert / replace payloads carry no ids. The engine
// assigns ids on application. The LLM never invents ids; it only references
// existing ones (as anchor targets, or as opaque_ref operands)."
//
// This invariant is enforced two ways, jointly:
//
// 1. **Type-enforced for fresh-node kinds.** Inspect `src/edit_v4.rs`: the
//    `Block::Paragraph`, `Block::Table`, `Inline::Text`, and
//    `Inline::Hyperlink` variants have no `id` field. There is no path in
//    the grammar through which a caller can attach an id to a node that
//    will be newly created. Adding an `id` field to any of those would be
//    a visible source-code change touching the type definition.
//
// 2. **Reference-only for the one id-bearing variant.** `Inline::OpaqueRef`
//    carries an id, but it is a *reference* to an existing opaque node, not
//    a freshly-minted id. The engine resolves the id against the target
//    paragraph's existing opaque inlines and fails loudly when the
//    reference does not match — this is invariant 2's foreign-id case,
//    covered by `invariant_2_opaque_set_equality_rejects_foreign_opaque_ref`
//    above. An LLM trying to "mint" an id by writing
//    `opaque_ref { id: "fabricated" }` is caught there.
//
// We deliberately do not add a runtime test that asserts serde drops
// unknown fields named `id`: serde's default behavior of tolerating
// unknown fields is a forward-compatibility property we want to keep, and
// pinning it would be testing serde rather than the invariant. The
// structural guarantee comes from the type definitions, not from runtime
// rejection of unknown fields.
//
// To strengthen this protection later: tag the fresh-node types with
// `#[serde(deny_unknown_fields)]` so the wire format actively rejects
// `id`. That decision should be weighed against the forward-compat cost
// when v4 has real clients.

// ─── Invariant 6: Move well-formedness ───────────────────────────────────────
//
// Spec: "A move destination must not lie within the moved node's own
// subtree (no cycles) and must be a legal container for the moved kind."
// The engine's `MoveBlockRange` step enforces the cycle check.

#[test]
fn invariant_6_move_rejects_destination_inside_source() {
    // For the day-one move shape (single-block range), "destination inside
    // source" only occurs when the destination anchor IS the source itself.
    let doc = simple_doc_with_text("p1", "only paragraph");
    let json = r#"
    {
      "ops": [{
        "op": "move",
        "target": "p1",
        "destination": { "anchor": "p1", "position": "after" }
      }],
      "revision": { "author": "Counsel" }
    }
    "#;
    let txn = parse_and_translate(json);
    let err = apply_transaction(&doc, &txn).expect_err("destination inside source must fail");
    assert!(
        matches!(err, EditError::MoveDestinationInsideSource { .. }),
        "expected MoveDestinationInsideSource, got {err:?}"
    );
}

/// The RANGE move form (`move.target: {"from","to"}`) must enforce the same
/// invariant over the WHOLE range, not just the range's own endpoints: an
/// anchor on any INTERIOR block (not merely `from` or `to` themselves) is
/// still "destination inside source".
#[test]
fn invariant_6_move_range_rejects_destination_inside_source() {
    let p1 = make_para("p1", normal_segment(vec![make_text("t1", "alpha")]));
    let p2 = make_para("p2", normal_segment(vec![make_text("t2", "beta")]));
    let p3 = make_para("p3", normal_segment(vec![make_text("t3", "gamma")]));
    let doc = make_doc(vec![
        normal_tracked_block(BlockNode::from(p1)),
        normal_tracked_block(BlockNode::from(p2)),
        normal_tracked_block(BlockNode::from(p3)),
    ]);
    let json = r#"
    {
      "ops": [{
        "op": "move",
        "target": { "from": "p1", "to": "p3" },
        "destination": { "anchor": "p2", "position": "after" }
      }],
      "revision": { "author": "Counsel" }
    }
    "#;
    let txn = parse_and_translate(json);
    let err = apply_transaction(&doc, &txn)
        .expect_err("an interior-block destination must fail, not just the range's own ends");
    assert!(
        matches!(err, EditError::MoveDestinationInsideSource { .. }),
        "expected MoveDestinationInsideSource, got {err:?}"
    );
}

// ─── Hyperlink replace: no silent href drop ──────────────────────────────────
//
// `replace(hyperlink, ...)` preserves the URL/anchor by design; href changes
// belong on `set_attr`. The wire format requires hyperlinks to carry an
// href, so the adapter forwards it as a precondition the engine validates.
// If the caller supplies a different href (intending to change it via
// `replace`), the engine fails loudly with `HyperlinkAttrMismatch` instead
// of silently dropping the new href.

fn doc_with_hyperlink(para_id: &str, hyperlink_id: &str, url: &str, text: &str) -> CanonDoc {
    let hyp = InlineNode::from(OpaqueInlineNode {
        id: NodeId::from(hyperlink_id),
        kind: OpaqueKind::Hyperlink(HyperlinkData {
            url: Some(url.to_string()),
            anchor: None,
            text: text.to_string(),
            r_id: Some("rId1".to_string()),
            runs: vec![HyperlinkRun {
                text: text.to_string(),
                rpr_xml: None,
                source_run_attrs: Vec::new(),
                status: TrackingStatus::Normal,
            }],
            extra_attrs: vec![],
        }),
        opaque_ref: format!("hyperlink_{hyperlink_id}"),
        proof_ref: ProofRef {
            part: DocPart::DocumentXml,
            block_id: NodeId::from(para_id),
            docx_anchor: String::new(),
        },
        wrapper_marks: Vec::new(),
        wrapper_style_props: StyleProps::default(),
        raw_xml: None,
        content_hash: None,
    });
    let para = make_para(
        para_id,
        normal_segment(vec![make_text(&format!("{para_id}_t1"), "see "), hyp]),
    );
    make_doc(vec![normal_tracked_block(BlockNode::from(para))])
}

#[test]
fn replace_hyperlink_with_changed_href_fails_loudly() {
    let doc = doc_with_hyperlink("p1", "h1", "https://original.example.com", "click here");
    // The caller sends a *different* href on the replace payload. Without
    // the precondition this would silently rewrite display text only.
    // With it, the engine refuses and points the caller at set_attr.
    let json = r#"
    {
      "ops": [{
        "op": "replace",
        "target": "h1",
        "expect": "click",
        "content": {
          "type": "hyperlink",
          "attrs": { "href": "https://new.example.com" },
          "content": [{ "type": "text", "text": "click here updated" }]
        }
      }],
      "revision": { "author": "Counsel" }
    }
    "#;
    let txn = parse_and_translate(json);
    let err = apply_transaction(&doc, &txn).expect_err("href mismatch must fail loudly");
    let EditError::HyperlinkAttrMismatch {
        attr,
        expected,
        actual,
        ..
    } = err
    else {
        panic!("expected HyperlinkAttrMismatch, got {err:?}");
    };
    assert_eq!(attr, "href");
    assert_eq!(expected.as_deref(), Some("https://new.example.com"));
    assert_eq!(actual.as_deref(), Some("https://original.example.com"));
}

#[test]
fn replace_hyperlink_with_matching_href_succeeds() {
    let doc = doc_with_hyperlink("p1", "h1", "https://original.example.com", "click here");
    // Same href on the payload as the existing one — the precondition is
    // satisfied and the display text is rewritten.
    let json = r#"
    {
      "ops": [{
        "op": "replace",
        "target": "h1",
        "expect": "click",
        "content": {
          "type": "hyperlink",
          "attrs": { "href": "https://original.example.com" },
          "content": [{ "type": "text", "text": "click here updated" }]
        }
      }],
      "revision": { "author": "Counsel" }
    }
    "#;
    let txn = parse_and_translate(json);
    apply_transaction(&doc, &txn).expect("matching-href replace succeeds");
}

// ─── Sanity: end-to-end happy path through the v4 surface ────────────────────

#[test]
fn v4_paragraph_replace_happy_path_produces_tracked_changes() {
    let doc = simple_doc_with_text("p1", "Hello, world.");
    let json = r#"
    {
      "ops": [{
        "op": "replace",
        "target": "p1",
        "expect": "Hello",
        "content": {
          "type": "paragraph",
          "content": [{ "type": "text", "text": "Greetings, world." }]
        }
      }],
      "revision": { "author": "Counsel" }
    }
    "#;
    let mut txn = parse_and_translate(json);
    txn.materialization_mode = MaterializationMode::TrackedChange;
    let result = apply_transaction(&doc, &txn)
        .expect("happy path succeeds")
        .0;
    // The result must contain both a deleted segment (Hello) and an
    // inserted segment (Greetings) — the engine's word-level diff routed
    // through the v4 adapter end-to-end.
    let para = result
        .blocks
        .iter()
        .find_map(|tb| match &tb.block {
            BlockNode::Paragraph(p) if p.id.0.as_ref() == "p1" => Some(p),
            _ => None,
        })
        .expect("paragraph still present");
    let mut saw_deleted = false;
    let mut saw_inserted = false;
    for seg in &para.segments {
        if matches!(seg.status, TrackingStatus::Deleted(_)) {
            saw_deleted = true;
        }
        if matches!(seg.status, TrackingStatus::Inserted(_)) {
            saw_inserted = true;
        }
    }
    assert!(saw_deleted, "expected a Deleted segment");
    assert!(saw_inserted, "expected an Inserted segment");
}

// ─── set_attr(hyperlink, { href, anchor }) — option (A) direct mutation ───────
//
// The v4 adapter routes `set_attr` on a hyperlink to
// `EditStep::SetHyperlinkAttr`, which mutates `HyperlinkData.url` and/or
// `HyperlinkData.anchor` in place. OOXML provides no `w:hyperlinkChange`
// element; option (A) is the spec-correct, Word-supported choice (verified
// against the held-out real-Word oracle). The adapter
// requires `expect_href` / `expect_anchor` alongside the matching mutation
// field (optimistic-concurrency contract).

fn hyperlink_data_of<'a>(doc: &'a CanonDoc, id: &str) -> &'a HyperlinkData {
    for block in &doc.blocks {
        if let BlockNode::Paragraph(p) = &block.block {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::OpaqueInline(o) = inline
                        && o.id.0.as_ref() == id
                        && let OpaqueKind::Hyperlink(d) = &o.kind
                    {
                        return d;
                    }
                }
            }
        }
    }
    panic!("hyperlink {id} not found in doc");
}

#[test]
fn set_attr_hyperlink_changes_href_with_matching_expect_succeeds() {
    let doc = doc_with_hyperlink("p1", "h1", "https://original.example.com", "click here");
    let json = r#"
    {
      "ops": [{
        "op": "set_attr",
        "target": "h1",
        "attrs": { "href": "https://updated.example.com" },
        "expect_href": "https://original.example.com"
      }],
      "revision": { "author": "Counsel" }
    }
    "#;
    let txn = parse_and_translate(json);
    let result = apply_transaction(&doc, &txn)
        .expect("href change succeeds")
        .0;
    let data = hyperlink_data_of(&result, "h1");
    assert_eq!(data.url.as_deref(), Some("https://updated.example.com"));
    // anchor was not touched.
    assert!(data.anchor.is_none());
    // Cached raw_xml is invalidated so the serializer rebuilds from `data`.
    for block in &result.blocks {
        if let BlockNode::Paragraph(p) = &block.block {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::OpaqueInline(o) = inline
                        && o.id.0.as_ref() == "h1"
                    {
                        assert!(o.raw_xml.is_none(), "raw_xml must be invalidated");
                        assert!(o.content_hash.is_none(), "content_hash must be invalidated");
                    }
                }
            }
        }
    }
}

#[test]
fn set_attr_hyperlink_changes_href_with_mismatched_expect_fails_loudly() {
    let doc = doc_with_hyperlink("p1", "h1", "https://original.example.com", "click here");
    let json = r#"
    {
      "ops": [{
        "op": "set_attr",
        "target": "h1",
        "attrs": { "href": "https://updated.example.com" },
        "expect_href": "https://STALE.example.com"
      }],
      "revision": { "author": "Counsel" }
    }
    "#;
    let txn = parse_and_translate(json);
    let err = apply_transaction(&doc, &txn).expect_err("stale expect_href must fail");
    let EditError::HyperlinkAttrMismatch {
        attr,
        expected,
        actual,
        ..
    } = err
    else {
        panic!("expected HyperlinkAttrMismatch, got {err:?}");
    };
    assert_eq!(attr, "href");
    assert_eq!(expected.as_deref(), Some("https://STALE.example.com"));
    assert_eq!(actual.as_deref(), Some("https://original.example.com"));
    // Document itself must be unchanged (apply_transaction works on a clone,
    // and on error returns the error; the caller's doc is untouched).
    let data = hyperlink_data_of(&doc, "h1");
    assert_eq!(data.url.as_deref(), Some("https://original.example.com"));
}

#[test]
fn set_attr_hyperlink_changes_anchor_only() {
    // The hyperlink starts with a URL and no anchor. The caller adds an
    // anchor without touching the URL. The required precondition is
    // `expect_anchor` (which is the empty string, matching the missing
    // anchor's empty-string surrogate). The URL must survive intact.
    let doc = doc_with_hyperlink("p1", "h1", "https://example.com/page", "click here");
    let json = r#"
    {
      "ops": [{
        "op": "set_attr",
        "target": "h1",
        "attrs": { "anchor": "section_2" },
        "expect_anchor": ""
      }],
      "revision": { "author": "Counsel" }
    }
    "#;
    let txn = parse_and_translate(json);
    let result = apply_transaction(&doc, &txn)
        .expect("anchor change succeeds")
        .0;
    let data = hyperlink_data_of(&result, "h1");
    assert_eq!(data.anchor.as_deref(), Some("section_2"));
    // URL is intact.
    assert_eq!(data.url.as_deref(), Some("https://example.com/page"));
    // r_id is also intact — the serializer will re-resolve it from URL at
    // export time, but the engine itself does not touch r_id.
    assert_eq!(data.r_id.as_deref(), Some("rId1"));
}

#[test]
fn set_attr_hyperlink_rejects_when_block_is_tracked() {
    // The block is in Inserted status (e.g. an LLM inserted the paragraph
    // earlier in the same review). Editing its hyperlink is refused, just
    // like editing its text would be.
    let hyp = InlineNode::from(OpaqueInlineNode {
        id: NodeId::from("h1"),
        kind: OpaqueKind::Hyperlink(HyperlinkData {
            url: Some("https://example.com".to_string()),
            anchor: None,
            text: "click".to_string(),
            r_id: Some("rId1".to_string()),
            runs: vec![HyperlinkRun {
                text: "click".to_string(),
                rpr_xml: None,
                source_run_attrs: Vec::new(),
                status: TrackingStatus::Normal,
            }],
            extra_attrs: vec![],
        }),
        opaque_ref: "hyperlink_h1".to_string(),
        proof_ref: ProofRef {
            part: DocPart::DocumentXml,
            block_id: NodeId::from("p1"),
            docx_anchor: String::new(),
        },
        wrapper_marks: Vec::new(),
        wrapper_style_props: StyleProps::default(),
        raw_xml: None,
        content_hash: None,
    });
    let para = make_para("p1", normal_segment(vec![hyp]));
    let doc = make_doc(vec![TrackedBlock {
        block: BlockNode::from(para),
        status: TrackingStatus::Inserted(RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("LLM".to_string()),
            date: None,
            apply_op_id: None,
        }),
        move_id: None,
        block_sdt_wrap: None,
    }]);
    let json = r#"
    {
      "ops": [{
        "op": "set_attr",
        "target": "h1",
        "attrs": { "href": "https://updated.example.com" },
        "expect_href": "https://example.com"
      }],
      "revision": { "author": "Counsel" }
    }
    "#;
    let txn = parse_and_translate(json);
    let err = apply_transaction(&doc, &txn).expect_err("tracked block must reject set_attr");
    let EditError::BlockHasTrackedStatus { status, .. } = err else {
        panic!("expected BlockHasTrackedStatus, got {err:?}");
    };
    assert_eq!(status, "inserted");
}

#[test]
fn set_attr_hyperlink_target_not_a_hyperlink() {
    // The targeted inline exists but is not a hyperlink (e.g. a footnote
    // ref). The engine reports NotAHyperlink with the actual kind so the
    // caller can route correctly.
    let footnote_ref = InlineNode::from(OpaqueInlineNode {
        id: NodeId::from("fn_3"),
        kind: OpaqueKind::FootnoteReference(NoteReferenceData {
            reference_id: "7".to_string(),
        }),
        opaque_ref: "footnote_ref_fn_3".to_string(),
        proof_ref: ProofRef {
            part: DocPart::DocumentXml,
            block_id: NodeId::from("p1"),
            docx_anchor: String::new(),
        },
        wrapper_marks: Vec::new(),
        wrapper_style_props: StyleProps::default(),
        raw_xml: Some(b"<w:footnoteReference w:id=\"7\"/>".to_vec()),
        content_hash: None,
    });
    let para = make_para(
        "p1",
        normal_segment(vec![
            make_text("p1_t1", "see "),
            footnote_ref,
            make_text("p1_t2", " for details"),
        ]),
    );
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);
    let json = r#"
    {
      "ops": [{
        "op": "set_attr",
        "target": "fn_3",
        "attrs": { "href": "https://updated.example.com" },
        "expect_href": "https://example.com"
      }],
      "revision": { "author": "Counsel" }
    }
    "#;
    let txn = parse_and_translate(json);
    let err = apply_transaction(&doc, &txn).expect_err("non-hyperlink target must fail");
    let EditError::NotAHyperlink { actual_kind, .. } = err else {
        panic!("expected NotAHyperlink, got {err:?}");
    };
    // The exact label is whatever `opaque_kind_label` reports for
    // FootnoteRef; the test asserts it's not "hyperlink".
    assert_ne!(actual_kind, "hyperlink");
}

#[test]
fn set_attr_hyperlink_unknown_id_fails() {
    // The targeted id is not present in the document. The engine returns
    // either HyperlinkNotFound (no inline with that id) or NotAHyperlink
    // (id exists but is not a hyperlink). For an unknown id, we expect
    // HyperlinkNotFound.
    let doc = doc_with_hyperlink("p1", "h1", "https://example.com", "click");
    let json = r#"
    {
      "ops": [{
        "op": "set_attr",
        "target": "h_NONEXISTENT",
        "attrs": { "href": "https://x" },
        "expect_href": "https://example.com"
      }],
      "revision": { "author": "Counsel" }
    }
    "#;
    let txn = parse_and_translate(json);
    let err = apply_transaction(&doc, &txn).expect_err("unknown id must fail");
    assert!(
        matches!(err, EditError::HyperlinkNotFound { .. }),
        "expected HyperlinkNotFound, got {err:?}"
    );
}

#[test]
fn set_attr_hyperlink_rejects_when_href_provided_without_expect_href() {
    // Adapter-level rejection: `attrs.href` requires `expect_href`.
    let json = r#"
    {
      "ops": [{
        "op": "set_attr",
        "target": "h1",
        "attrs": { "href": "https://updated.example.com" }
      }],
      "revision": { "author": "Counsel" }
    }
    "#;
    let txn = parse_transaction(json).expect("schema accepts");
    let err = txn.into_edit_transaction().expect_err("adapter rejects");
    assert_eq!(
        err,
        AdapterError::MissingHyperlinkAttrExpect {
            op_index: 0,
            attr: "href",
        },
    );
}

#[test]
fn set_attr_hyperlink_rejects_when_anchor_provided_without_expect_anchor() {
    let json = r#"
    {
      "ops": [{
        "op": "set_attr",
        "target": "h1",
        "attrs": { "anchor": "section_2" }
      }],
      "revision": { "author": "Counsel" }
    }
    "#;
    let txn = parse_transaction(json).expect("schema accepts");
    let err = txn.into_edit_transaction().expect_err("adapter rejects");
    assert_eq!(
        err,
        AdapterError::MissingHyperlinkAttrExpect {
            op_index: 0,
            attr: "anchor",
        },
    );
}

#[test]
fn set_attr_hyperlink_no_op_when_neither_field_set_after_adapter() {
    // The v4 adapter rejects empty AttrPatch at the schema layer
    // (EmptyAttrPatch). The engine-side defensive check (HyperlinkSetAttrNoOp)
    // is for direct EditStep construction without going through the adapter.
    // This test exercises the engine path directly.
    let doc = doc_with_hyperlink("p1", "h1", "https://example.com", "click");
    let txn = EditTransaction {
        steps: vec![EditStep::SetHyperlinkAttr {
            hyperlink_id: NodeId::from("h1"),
            new_href: None,
            new_anchor: None,
            expect_href: None,
            expect_anchor: None,
            rationale: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 0,
            identity: 0,
            author: Some("Counsel".to_string()),
            date: None,
            apply_op_id: None,
        },
    };
    let err = apply_transaction(&doc, &txn).expect_err("empty set_attr must fail");
    assert!(
        matches!(err, EditError::HyperlinkSetAttrNoOp { .. }),
        "expected HyperlinkSetAttrNoOp, got {err:?}"
    );
}

#[test]
fn set_attr_hyperlink_changes_both_href_and_anchor_in_one_step() {
    // Both fields can be mutated in a single set_attr op, with both
    // preconditions supplied.
    let doc = doc_with_hyperlink("p1", "h1", "https://example.com/page", "click here");
    let json = r#"
    {
      "ops": [{
        "op": "set_attr",
        "target": "h1",
        "attrs": { "href": "https://example.com/new-page", "anchor": "intro" },
        "expect_href": "https://example.com/page",
        "expect_anchor": ""
      }],
      "revision": { "author": "Counsel" }
    }
    "#;
    let txn = parse_and_translate(json);
    let result = apply_transaction(&doc, &txn)
        .expect("dual change succeeds")
        .0;
    let data = hyperlink_data_of(&result, "h1");
    assert_eq!(data.url.as_deref(), Some("https://example.com/new-page"));
    assert_eq!(data.anchor.as_deref(), Some("intro"));
}
