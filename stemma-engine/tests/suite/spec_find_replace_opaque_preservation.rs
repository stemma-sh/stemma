//! OOXML spec-compliance (daily tier): a tracked find-replace must PRESERVE
//! run-level non-text content (fields, hyperlinks, images) it edits around.
//!
//! ECMA-376 / ISO 29500-1 §17.13.5 defines tracked insert/delete (`w:ins` /
//! `w:del`) over runs. A complex field (`w:fldSimple` / `w:fldChar` +
//! `w:instrText`, §17.16) or a hyperlink (`w:hyperlink`, §17.16.22) is run-level
//! content that text editing must not silently destroy — the engine models
//! these as opaque inline ANCHORS and enforces (domain-model §11) that every
//! anchor present before an edit is present after it, or the edit fails with
//! `OpaqueDestroyed`.
//!
//! The find-replace planner satisfies this by emitting a `PreservedInlineRef`
//! for every barrier anchor in original order. This test pins both halves of
//! the contract:
//!   1. A planned replace whose match sits in a text section ADJACENT to a field
//!      anchor keeps the field anchor in the output (accept and reject).
//!   2. A hand-built replace that OMITS the anchor (what a naive find-replace
//!      would do) is rejected with `OpaqueDestroyed` — the engine never lets the
//!      field be dropped.

use stemma::edit::*;
use stemma::{
    BlockNode, CanonDoc, InlineNode, NodeId, OpaqueInlineNode, OpaqueKind, ParagraphNode, ProofRef,
    RevisionInfo, StyleProps, TextNode, accept_all, normal_segment, normal_tracked_block,
    reject_all_with_styles,
};

fn make_text(id: &str, text: &str) -> InlineNode {
    InlineNode::from(TextNode {
        id: NodeId::from(id),
        text_role: None,
        text: text.to_string(),
        marks: vec![],
        style_props: StyleProps::default(),
        rpr_authored: stemma::domain::RunRprAuthored::default(),
        source_run_attrs: Vec::new(),
        formatting_change: None,
    })
}

/// A PAGE field (`w:fldSimple w:instr="PAGE"`), modeled as an opaque inline.
fn make_field(id: &str) -> InlineNode {
    InlineNode::from(OpaqueInlineNode {
        id: NodeId::from(id),
        kind: OpaqueKind::Field(stemma::FieldData {
            field_kind: stemma::FieldKind::Simple,
            instruction_text: Some("PAGE".to_string()),
            result_text: Some("1".to_string()),
            semantic: None,
        }),
        opaque_ref: format!("field_{id}"),
        proof_ref: ProofRef {
            part: stemma::DocPart::DocumentXml,
            block_id: NodeId::from("p1"),
            docx_anchor: String::new(),
        },
        wrapper_marks: Vec::new(),
        wrapper_style_props: StyleProps::default(),
        raw_xml: Some(
            br#"<w:fldSimple w:instr="PAGE"><w:r><w:t>1</w:t></w:r></w:fldSimple>"#.to_vec(),
        ),
        content_hash: None,
    })
}

fn build_para(id: &str, inlines: Vec<InlineNode>) -> ParagraphNode {
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
        segments: normal_segment(inlines),
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

fn doc(para: ParagraphNode) -> CanonDoc {
    CanonDoc {
        id: NodeId::from("doc1"),
        blocks: vec![normal_tracked_block(BlockNode::from(para))],
        meta: stemma::DocMeta {
            schema_version: stemma::SCHEMA_VERSION_V0.to_string(),
            docx_fingerprint: stemma::DocFingerprint("spec-fr".to_string()),
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

fn revision() -> RevisionInfo {
    RevisionInfo {
        revision_id: 1,
        identity: 0,
        author: Some("spec".to_string()),
        date: Some("2026-06-01T00:00:00Z".to_string()),
        apply_op_id: None,
    }
}

fn txn(steps: Vec<EditStep>, mode: MaterializationMode) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        materialization_mode: mode,
        revision: revision(),
    }
}

fn field_ids(d: &CanonDoc) -> Vec<String> {
    let mut ids = Vec::new();
    for tb in &d.blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::OpaqueInline(o) = inline {
                        ids.push(o.id.to_string());
                    }
                }
            }
        }
    }
    ids
}

/// "Page " <field PAGE> " of the report." — replace "report" with "document".
/// The field anchor must survive both accept and reject.
#[test]
fn spec_planned_replace_preserves_field_anchor() {
    let base = doc(build_para(
        "p1",
        vec![
            make_text("p1_a", "Page "),
            make_field("fld1"),
            make_text("p1_b", " of the report."),
        ],
    ));

    let plan = plan_find_replace_all(
        &base,
        &FindReplaceOptions {
            needle: "report".to_string(),
            replacement: "document".to_string(),
            scope: FindReplaceScope::BodyOnly,
            case_sensitive: true,
            whole_word: false,
            on_barrier_match: BarrierPolicy::Fail,
        },
    )
    .expect("plan");
    assert_eq!(plan.len(), 1, "the matching paragraph gets one step");

    let tracked = apply_transaction(&base, &txn(plan, MaterializationMode::TrackedChange))
        .expect("tracked apply")
        .0;

    // Field anchor survives the tracked edit (§17.13.5 / §17.16: run-level
    // content preserved across tracked text edits).
    assert!(
        field_ids(&tracked).contains(&"fld1".to_string()),
        "field anchor must survive the tracked replace"
    );

    let mut accepted = tracked.clone();
    accept_all(&mut accepted);
    assert!(
        field_ids(&accepted).contains(&"fld1".to_string()),
        "field anchor must survive accept-all"
    );

    let mut rejected = tracked;
    reject_all_with_styles(&mut rejected, None);
    assert!(
        field_ids(&rejected).contains(&"fld1".to_string()),
        "field anchor must survive reject-all"
    );
}

/// The negative half of the spec contract: a `ReplaceParagraphText` that OMITS
/// the field's `PreservedInlineRef` is rejected with `OpaqueDestroyed`. This is
/// what a naive find-replace (rebuild text only) would do — the engine refuses
/// it, which is WHY the planner must interleave the anchor refs.
#[test]
fn spec_replace_omitting_field_anchor_is_rejected() {
    let base = doc(build_para(
        "p1",
        vec![
            make_text("p1_a", "Page "),
            make_field("fld1"),
            make_text("p1_b", " of the report."),
        ],
    ));

    // Content rebuilds only text, dropping the field anchor entirely.
    let bad = vec![EditStep::ReplaceParagraphText {
        block_id: NodeId::from("p1"),
        rationale: None,
        replacement_role: None,
        expect: "Page ".to_string(),
        semantic_hash: None,
        content: ParagraphContent {
            fragments: vec![ContentFragment::Text("Page  of the document.".to_string())],
        },
    }];

    let err = apply_transaction(&base, &txn(bad, MaterializationMode::TrackedChange))
        .expect_err("dropping the field anchor must be rejected");
    assert!(
        matches!(err, EditError::OpaqueDestroyed { .. }),
        "expected OpaqueDestroyed, got {err:?}"
    );
}
