//! `ApplyStyle` — author a tracked **named-style** change on a paragraph
//! (`w:pStyle` inside `w:pPrChange`, §17.3.1.27 + §17.13.5.29). "Make this
//! paragraph a Heading 2, tracked."
//!
//! This is the SAME lift as `paragraph_formatting.rs`: a paragraph's
//! `style_id` already serializes as `w:pStyle` at pPr position 0, the existing
//! `ParagraphFormattingChange.previous_style_id` already records the prior
//! style, `snapshot_paragraph_formatting` already snapshots it, and the
//! accept/reject projection already resolves it (`tracked_model.rs`: reject
//! restores `previous_style_id`, accept clears the change keeping the new
//! style). So this verb is a **pure authoring-side lift** — ZERO domain or
//! serialize change — exactly like the paragraph-formatting verb. It does not
//! go through the segment materializer (Invariant M): a style swap is an
//! in-place property delta, not a text insert/delete.
//!
//! v1 scope (fail loud beyond it, per CLAUDE.md "start narrow"):
//! - **paragraph style only** (the `w:pStyle` on a top-level paragraph). A
//!   nested in-cell paragraph surfaces as `BlockNotFound`. Character / table /
//!   numbering styles are out of scope.
//! - the paragraph must be Normal with no existing tracked segments;
//! - the paragraph must not already carry a `pPrChange` (accept/reject it
//!   first) — mirrors the run/paragraph-formatting guards;
//! - a no-op (the style is already the target) is refused
//!   (`NoStyleChangeRequested`);
//! - **style existence**: `apply_transaction` holds only a `&CanonDoc`, which
//!   carries NO style table (styles.xml is a package part, unmodeled in the
//!   IR). So this verb CANNOT validate that `style_id` exists — that
//!   validation defers to the package-aware caller (the runtime, which has the
//!   `DocxPackage` and its `word/styles.xml`). The `StyleNotFound` variant is
//!   the error that caller emits. This verb NEVER silently accepts an unknown
//!   style as valid output: it sets exactly the requested id and records the
//!   prior one, and the caller is responsible for surfacing a dangling style.
//!
//! Out of scope (flagged as cross-cutting): `CreateStyle` / `ModifyStyle`
//! (mutating `word/styles.xml`) collide with the merge path
//! (`merge_styles_xml_preferring_target`) and are deferred.

use super::super::{
    EditError, MaterializationMode, find_block_index, next_revision, snapshot_paragraph_formatting,
    validate_block_is_editable,
};
use crate::domain::{BlockNode, CanonDoc, IStr, NodeId, ParagraphNode, RevisionInfo};
use crate::semantic_hash::check_block_guard;

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply(
    doc: &mut CanonDoc,
    block_id: &NodeId,
    semantic_hash: Option<&str>,
    style_id: &str,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    mode: MaterializationMode,
    step_index: usize,
) -> Result<(), EditError> {
    // v1: top-level paragraphs only. A nested (table-cell) paragraph is not
    // found here and surfaces as BlockNotFound.
    let idx = find_block_index(&doc.blocks, block_id).ok_or_else(|| EditError::BlockNotFound {
        block_id: block_id.clone(),
        step_index,
    })?;

    // Block status + segment-Normal preconditions (same gate as a text replace
    // / paragraph-formatting change).
    validate_block_is_editable(&doc.blocks[idx], step_index)?;

    match &doc.blocks[idx].block {
        BlockNode::Paragraph(_) => {}
        BlockNode::Table(_) => {
            return Err(EditError::NotAParagraph {
                block_id: block_id.clone(),
                actual_kind: "table",
                step_index,
            });
        }
        BlockNode::OpaqueBlock(_) => {
            return Err(EditError::NotAParagraph {
                block_id: block_id.clone(),
                actual_kind: "opaque_block",
                step_index,
            });
        }
    }

    if let Some(expected) = semantic_hash
        && let Err(actual) = check_block_guard(&doc.blocks[idx].block, expected)
    {
        return Err(EditError::BlockSemanticHashMismatch {
            block_id: block_id.clone(),
            expected: expected.to_string(),
            actual,
            step_index,
        });
    }

    let BlockNode::Paragraph(para) = &mut doc.blocks[idx].block else {
        unreachable!("checked paragraph above");
    };

    apply_to_paragraph(
        para,
        block_id,
        style_id,
        revision,
        rev_counter,
        mode,
        step_index,
    )
}

/// Apply the style swap to a single (already located + kind-checked)
/// paragraph. Carries the no-op and stacked-pPrChange guards so they are
/// unit-testable without a full `CanonDoc` (mirrors `numbering.rs`).
#[allow(clippy::too_many_arguments)]
fn apply_to_paragraph(
    para: &mut ParagraphNode,
    block_id: &NodeId,
    style_id: &str,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    mode: MaterializationMode,
    step_index: usize,
) -> Result<(), EditError> {
    // Refuse to stack a new tracked pPrChange on a paragraph that already
    // carries one — accept or reject the existing change first. Mirrors the
    // run/paragraph-formatting guards.
    if para.formatting_change.is_some() {
        return Err(EditError::UnsupportedParagraphStructure {
            block_id: block_id.clone(),
            reason: "the paragraph already has a tracked formatting change (pPrChange); \
                     accept or reject it before applying a style"
                .to_string(),
            step_index,
        });
    }

    // Refuse a no-op: the style is already the requested target. Setting it
    // again would author a visually-empty pPrChange.
    if para.style_id.as_deref() == Some(style_id) {
        return Err(EditError::NoStyleChangeRequested {
            block_id: block_id.clone(),
            style_id: style_id.to_string(),
            step_index,
        });
    }

    // Snapshot BEFORE mutating so the inner pPr of the pPrChange is the
    // complete previous state (§17.13.5.29), with the prior pStyle.
    let rev_for_change = next_revision(revision, rev_counter);
    let snapshot = snapshot_paragraph_formatting(para, &rev_for_change);

    para.style_id = Some(IStr::from(style_id));

    // Clear cached text hash / rendered text so any downstream consumer that
    // compares against a hash recomputes (mirrors paragraph_formatting.rs).
    para.block_text_hash = None;
    para.rendered_text = None;

    match mode {
        MaterializationMode::TrackedChange => {
            para.formatting_change = Some(snapshot);
        }
        MaterializationMode::Direct => {
            para.formatting_change = None;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{StyleProps, TrackedSegment, TrackingStatus};

    /// A minimal Normal paragraph with every field defaulted, parameterized
    /// only by id + style — the fields the style verb reads (mirrors the
    /// `bare_para` helper in `numbering.rs`).
    fn bare_para(id: &str, style: Option<&str>) -> ParagraphNode {
        ParagraphNode {
            id: NodeId::new(id.to_string()),
            style_id: style.map(IStr::from),
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
            segments: vec![TrackedSegment {
                status: TrackingStatus::Normal,
                inlines: vec![],
            }],
            block_text_hash: None,
            numbering: None,
            has_direct_numbering: true,
            numbering_suppressed: false,
            materialized_numbering: None,
            rendered_text: None,
            literal_prefix: None,
            literal_prefix_marks: Vec::new(),
            literal_prefix_style_props: StyleProps::default(),
            literal_prefix_rpr_authored: crate::domain::RunRprAuthored::default(),
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

    fn rev() -> RevisionInfo {
        RevisionInfo {
            revision_id: 1,
            author: Some("Test".to_string()),
            date: Some("2026-06-01T00:00:00Z".to_string()),
            apply_op_id: None,
        }
    }

    #[test]
    fn applies_style_and_records_previous() {
        let mut p = bare_para("p1", None);
        let mut ctr = 1;
        apply_to_paragraph(
            &mut p,
            &NodeId::new("p1".to_string()),
            "Heading2",
            &rev(),
            &mut ctr,
            MaterializationMode::TrackedChange,
            0,
        )
        .expect("apply");
        assert_eq!(p.style_id.as_deref(), Some("Heading2"));
        let fc = p.formatting_change.as_ref().expect("pPrChange recorded");
        assert_eq!(fc.previous_style_id, None, "prior style was Normal/None");
    }

    #[test]
    fn records_prior_named_style() {
        let mut p = bare_para("p1", Some("Heading1"));
        let mut ctr = 1;
        apply_to_paragraph(
            &mut p,
            &NodeId::new("p1".to_string()),
            "Heading2",
            &rev(),
            &mut ctr,
            MaterializationMode::TrackedChange,
            0,
        )
        .expect("apply");
        assert_eq!(p.style_id.as_deref(), Some("Heading2"));
        assert_eq!(
            p.formatting_change
                .as_ref()
                .unwrap()
                .previous_style_id
                .as_deref(),
            Some("Heading1"),
            "prior named style must be recorded for reject-restore"
        );
    }

    #[test]
    fn refuses_noop_when_style_already_target() {
        let mut p = bare_para("p1", Some("Heading2"));
        let mut ctr = 1;
        let err = apply_to_paragraph(
            &mut p,
            &NodeId::new("p1".to_string()),
            "Heading2",
            &rev(),
            &mut ctr,
            MaterializationMode::TrackedChange,
            0,
        );
        assert!(matches!(err, Err(EditError::NoStyleChangeRequested { .. })));
    }

    #[test]
    fn refuses_stacked_pprchange() {
        let mut p = bare_para("p1", None);
        let mut ctr = 1;
        apply_to_paragraph(
            &mut p,
            &NodeId::new("p1".to_string()),
            "Heading2",
            &rev(),
            &mut ctr,
            MaterializationMode::TrackedChange,
            0,
        )
        .expect("first apply");
        let err = apply_to_paragraph(
            &mut p,
            &NodeId::new("p1".to_string()),
            "Heading3",
            &rev(),
            &mut ctr,
            MaterializationMode::TrackedChange,
            1,
        );
        assert!(matches!(
            err,
            Err(EditError::UnsupportedParagraphStructure { .. })
        ));
    }

    #[test]
    fn direct_mode_leaves_no_change_record() {
        let mut p = bare_para("p1", Some("Normal"));
        let mut ctr = 1;
        apply_to_paragraph(
            &mut p,
            &NodeId::new("p1".to_string()),
            "Heading2",
            &rev(),
            &mut ctr,
            MaterializationMode::Direct,
            0,
        )
        .expect("apply direct");
        assert_eq!(p.style_id.as_deref(), Some("Heading2"));
        assert!(
            p.formatting_change.is_none(),
            "direct mode records no pPrChange"
        );
    }
}
