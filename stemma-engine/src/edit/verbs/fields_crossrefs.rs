//! `InsertCrossReference` — author a new REF / PAGEREF / NOREF cross-reference
//! field as a tracked insert (`w:fldSimple`, §17.16.5.45 REF / .39 PAGEREF /
//! .36 NOREF). "Insert a cross-reference to the Definitions bookmark, tracked."
//!
//! This verb is overwhelmingly a **lift**, not a build:
//!
//! - The IR already models the field fully: `OpaqueKind::Field(FieldData)` with
//!   `FieldKind::Simple` and `FieldSemantic::Ref(RefFieldSpec)`.
//! - The serializer already emits a tracked-capable `w:fldSimple` from a
//!   synthesized `Field` opaque (`build_simple_field_element` sets `w:instr`
//!   from `data.semantic.to_instruction_text()`); `RefFieldSpec::to_instruction_text()`
//!   already renders `"REF bookmark \\h…"`.
//! - The TOC-insert verb (`resolve_toc_spec`) and `NewHyperlink`
//!   (`synthesize_new_hyperlink_inline`) are the working precedent for
//!   synthesizing a fresh inline opaque with `raw_xml: None` and letting the
//!   serializer rebuild it.
//!
//! It does **not** touch the materializer (Invariant M). Like `run_formatting`,
//! it operates in-place on the target paragraph's segments: it locates the
//! `expect` anchor text inside one contiguous Normal segment and splices the
//! synthesized `Field` opaque in directly behind it, wrapped in its own
//! `Inserted` segment (TrackedChange mode) or left `Normal` (Direct mode). The
//! existing segment-level accept/reject projection then resolves the change:
//! accept keeps the inserted `fldSimple`; reject drops it.
//!
//! v1 scope (fail loud beyond it, per CLAUDE.md "start narrow"):
//! - top-level paragraphs only;
//! - authors only the self-contained `w:fldSimple` REF/PAGEREF/NOREF — the
//!   four-piece `fldChar` begin/instr/separate/end complex field is out of
//!   scope (roundtrip-only via `raw_xml`) and is never produced here;
//! - the bookmark name must be non-empty (an empty `\REF` instruction is
//!   meaningless and refused — it is NOT defaulted);
//! - the `expect` anchor text must lie within a single contiguous Normal
//!   segment of text;
//! - v1 inserts the field but does NOT compute or refresh `result_text` (Word
//!   recalculates the displayed reference text on open); `result_text` stays
//!   `None`.

use super::super::{EditError, MaterializationMode};
use super::super::{find_block_index, validate_block_is_editable};
use crate::domain::{
    BlockNode, CanonDoc, DocPart, FieldData, FieldKind, FieldSemantic, InlineNode, NodeId,
    OpaqueInlineNode, OpaqueKind, ParagraphNode, ProofRef, RefFieldSpec, RevisionInfo, StyleProps,
    TrackedSegment, TrackingStatus,
};
use crate::semantic_hash::check_block_guard;

/// A located insertion point: segment `seg_idx`, after inline index
/// `inline_idx` (which is the `Text` node whose tail contains the end of the
/// `expect` match). `char_end` is the char offset *inside that text node* at
/// which the match ends; the field opaque is spliced in right after it.
struct InsertPlan {
    seg_idx: usize,
    inline_idx: usize,
    char_end: usize,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply(
    doc: &mut CanonDoc,
    block_id: &NodeId,
    expect: &str,
    semantic_hash: Option<&str>,
    spec: &RefFieldSpec,
    revision: &RevisionInfo,
    mode: MaterializationMode,
    step_index: usize,
) -> Result<(), EditError> {
    // Validate the bookmark at the verb edge, before mutating anything — no
    // best-effort default (CLAUDE.md "no silent fallbacks"). An empty bookmark
    // would render `REF ` with no target, which is meaningless.
    if spec.bookmark.trim().is_empty() {
        return Err(EditError::CrossRefEmptyBookmark { step_index });
    }

    // v1: top-level paragraphs only. A nested (table-cell) paragraph is not
    // found here and surfaces as BlockNotFound — extend when authoring demand
    // for in-cell cross-references appears.
    let idx = find_block_index(&doc.blocks, block_id).ok_or_else(|| EditError::BlockNotFound {
        block_id: block_id.clone(),
        step_index,
    })?;

    // Block status + segment-Normal preconditions (same gate as a text replace).
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
    apply_to_paragraph(para, block_id, expect, spec, revision, mode, step_index)
}

fn apply_to_paragraph(
    para: &mut ParagraphNode,
    block_id: &NodeId,
    expect: &str,
    spec: &RefFieldSpec,
    revision: &RevisionInfo,
    mode: MaterializationMode,
    step_index: usize,
) -> Result<(), EditError> {
    let plan = find_anchor(&para.segments, expect).ok_or_else(|| {
        let visible: String = para
            .segments
            .iter()
            .flat_map(|s| s.inlines.iter())
            .filter_map(|i| match i {
                InlineNode::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect();
        EditError::ExpectMismatch {
            block_id: block_id.clone(),
            expected: expect.to_string(),
            actual_text: visible,
            step_index,
        }
    })?;

    // Synthesize the new field opaque. Its id is derived from the paragraph id
    // so it is stable and unique within the block.
    let field_id = NodeId::from(format!("{}_xref0", para.id.0));
    let field = synthesize_new_field_inline(&field_id, spec);

    splice_field(&mut para.segments, plan, field, revision, mode);

    // Invalidate caches that depended on the previous inline layout.
    para.block_text_hash = None;
    para.rendered_text = None;
    Ok(())
}

/// Locate `expect` inside a single contiguous run of `Text` nodes within one
/// Normal segment. Returns the segment, the inline index of the text node whose
/// tail contains the end of the match, and the char offset where the match ends
/// inside that node. The cross-reference is inserted at that boundary.
fn find_anchor(segments: &[TrackedSegment], expect: &str) -> Option<InsertPlan> {
    if expect.is_empty() {
        return None;
    }
    for (seg_idx, seg) in segments.iter().enumerate() {
        // Defensive: we only ever splice into Normal segments (the editable
        // precondition guarantees all segments are Normal, but stay honest).
        if seg.status != TrackingStatus::Normal {
            continue;
        }
        let inlines = &seg.inlines;
        let mut i = 0;
        while i < inlines.len() {
            if !matches!(inlines[i], InlineNode::Text(_)) {
                i += 1;
                continue;
            }
            // Contiguous TextNode run [i, j).
            let mut j = i;
            let mut concat = String::new();
            while j < inlines.len() {
                match &inlines[j] {
                    InlineNode::Text(t) => {
                        concat.push_str(&t.text);
                        j += 1;
                    }
                    _ => break,
                }
            }
            if let Some(match_end_chars) = char_find_end(&concat, expect) {
                // Map the run-relative end offset back to the specific text
                // node and its local char offset.
                let mut consumed = 0usize;
                for (k, inline) in inlines.iter().enumerate().take(j).skip(i) {
                    let InlineNode::Text(t) = inline else {
                        unreachable!("run is all TextNodes");
                    };
                    let len = t.text.chars().count();
                    if match_end_chars <= consumed + len {
                        return Some(InsertPlan {
                            seg_idx,
                            inline_idx: k,
                            char_end: match_end_chars - consumed,
                        });
                    }
                    consumed += len;
                }
            }
            i = j.max(i + 1);
        }
    }
    None
}

/// Char offset (1-based count of chars from the start) at which `needle` ends
/// inside `haystack`, or `None` if not found.
fn char_find_end(haystack: &str, needle: &str) -> Option<usize> {
    let byte = haystack.find(needle)?;
    let start = haystack[..byte].chars().count();
    Some(start + needle.chars().count())
}

/// Splice the synthesized `field` opaque into the located segment right after
/// the anchor text. The host segment is split into up to three segments:
///
/// 1. `Normal` — everything up to and including the anchor-bearing text (the
///    text node is split at `char_end` so the field lands exactly after the
///    matched span);
/// 2. the inserted field (its own `Inserted` segment in TrackedChange mode, or
///    folded back into Normal in Direct mode);
/// 3. `Normal` — the remainder of the host segment.
///
/// Accept-all keeps the inserted field (its segment becomes Normal); reject-all
/// drops the Inserted segment, leaving the two Normal halves which re-coalesce
/// to the original — that is the reversibility invariant, handled entirely by
/// the existing segment-level accept/reject projection (no new code here).
fn splice_field(
    segments: &mut Vec<TrackedSegment>,
    plan: InsertPlan,
    field: InlineNode,
    revision: &RevisionInfo,
    mode: MaterializationMode,
) {
    let host = segments.remove(plan.seg_idx);
    let mut inlines = host.inlines;

    // Split the anchor-bearing text node at `char_end` so the field lands right
    // after the matched span (not at the end of the whole text node).
    if let InlineNode::Text(node) = &inlines[plan.inline_idx] {
        let chars: Vec<char> = node.text.chars().collect();
        if plan.char_end < chars.len() {
            let before: String = chars[..plan.char_end].iter().collect();
            let after: String = chars[plan.char_end..].iter().collect();
            let mut head = node.clone();
            head.text = before;
            let mut tail = node.clone();
            tail.id = NodeId::new(format!("{}_xtail", node.id.0));
            tail.text = after;
            inlines.splice(
                plan.inline_idx..=plan.inline_idx,
                [InlineNode::Text(head), InlineNode::Text(tail)],
            );
        }
    }

    // Everything up to and including the (possibly split) anchor text node.
    let split_at = plan.inline_idx + 1;
    let head_inlines: Vec<InlineNode> = inlines.drain(..split_at).collect();
    let tail_inlines: Vec<InlineNode> = inlines; // remainder

    let field_status = match mode {
        MaterializationMode::TrackedChange => TrackingStatus::Inserted(revision.clone()),
        MaterializationMode::Direct => TrackingStatus::Normal,
    };

    let mut rebuilt: Vec<TrackedSegment> = Vec::new();
    if !head_inlines.is_empty() {
        rebuilt.push(TrackedSegment {
            status: TrackingStatus::Normal,
            inlines: head_inlines,
        });
    }
    rebuilt.push(TrackedSegment {
        status: field_status,
        inlines: vec![field],
    });
    if !tail_inlines.is_empty() {
        rebuilt.push(TrackedSegment {
            status: TrackingStatus::Normal,
            inlines: tail_inlines,
        });
    }

    // Splice the rebuilt segments back where the host segment was.
    segments.splice(plan.seg_idx..plan.seg_idx, rebuilt);
}

/// Build a fresh `OpaqueInline{Field}` for a REF/PAGEREF/NOREF cross-reference,
/// modeled on `synthesize_new_hyperlink_inline` and `resolve_toc_spec`:
/// `raw_xml: None` (so the serializer rebuilds the `w:fldSimple` from the
/// semantic spec), default wrapper marks/props, instruction text rendered from
/// the spec. `result_text` is `None` — Word recalculates the displayed
/// reference on open (v1 does not compute page numbers / cross-ref text).
fn synthesize_new_field_inline(id: &NodeId, spec: &RefFieldSpec) -> InlineNode {
    let instruction_text = spec.to_instruction_text();
    let data = FieldData {
        field_kind: FieldKind::Simple,
        instruction_text: Some(instruction_text),
        result_text: None,
        semantic: Some(FieldSemantic::Ref(spec.clone())),
    };
    InlineNode::from(OpaqueInlineNode {
        id: id.clone(),
        kind: OpaqueKind::Field(data),
        opaque_ref: format!("fieldref_{}", id.0),
        proof_ref: ProofRef {
            part: DocPart::DocumentXml,
            block_id: id.clone(),
            docx_anchor: String::new(),
        },
        wrapper_marks: Vec::new(),
        wrapper_style_props: StyleProps::default(),
        raw_xml: None,
        content_hash: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{FormatSwitches, RefKind};

    fn ref_spec(bookmark: &str) -> RefFieldSpec {
        RefFieldSpec {
            kind: RefKind::Ref,
            bookmark: bookmark.to_string(),
            insert_hyperlink: false,
            no_paragraph_number: false,
            paragraph_number_relative: false,
            paragraph_number_full: false,
            suppress_non_delimiter: false,
            above_below: false,
            format: FormatSwitches::default(),
        }
    }

    #[test]
    fn synthesized_field_carries_ref_instruction() {
        let spec = ref_spec("Definitions");
        let inline = synthesize_new_field_inline(&NodeId::from("p_1_xref0"), &spec);
        let InlineNode::OpaqueInline(o) = inline else {
            panic!("expected opaque inline");
        };
        let OpaqueKind::Field(data) = o.kind else {
            panic!("expected a field opaque");
        };
        assert_eq!(data.field_kind, FieldKind::Simple);
        // §17.16.5.45 — a REF field's instruction is `REF <bookmark>`.
        assert_eq!(data.instruction_text.as_deref(), Some("REF Definitions"));
        // v1 does not compute the displayed reference; Word recalculates on open.
        assert_eq!(data.result_text, None);
        assert!(o.raw_xml.is_none());
        assert!(matches!(data.semantic, Some(FieldSemantic::Ref(_))));
    }

    #[test]
    fn synthesized_pageref_with_hyperlink_switch() {
        let mut spec = ref_spec("Section2");
        spec.kind = RefKind::PageRef;
        spec.insert_hyperlink = true;
        let inline = synthesize_new_field_inline(&NodeId::from("p_2_xref0"), &spec);
        let InlineNode::OpaqueInline(o) = inline else {
            panic!("expected opaque inline");
        };
        let OpaqueKind::Field(data) = o.kind else {
            panic!("expected a field opaque");
        };
        // §17.16.5.39 PAGEREF + `\h` hyperlink switch.
        assert_eq!(
            data.instruction_text.as_deref(),
            Some("PAGEREF Section2 \\h")
        );
    }

    #[test]
    fn char_find_end_maps_to_char_offset() {
        assert_eq!(char_find_end("Hello world", "Hello"), Some(5));
        assert_eq!(char_find_end("Hello world", "world"), Some(11));
        assert_eq!(char_find_end("café au lait", "café"), Some(4));
        assert_eq!(char_find_end("Hello", "absent"), None);
    }
}
