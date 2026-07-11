//! `InsertEquation` — author a new Office MathML (OMML) equation as a tracked
//! insert. Inline math is `m:oMath` (§22.1.2.77, run-level); display/block math
//! is `m:oMathPara` (§22.1.2.78, a direct child of `w:p`).
//!
//! This verb is a **lift**, not a build: the IR already models math as
//! `OpaqueKind::OmmlInline` / `OpaqueKind::OmmlBlock`, the importer maps both
//! (`import.rs` ~4674), and the serializer already emits them — including a
//! tracked container for the block form (`append_tracked_omml_paragraph_opaque`
//! wraps `m:oMathPara` inside `w:ins`/`w:del`). So block equations are
//! tracked-change-capable for free, and inline equations ride the same
//! segment-splice the cross-reference verb uses.
//!
//! Unlike the field verb (`raw_xml: None`, serializer rebuilds), an equation's
//! OMML fragment **is** the source of truth — there is no semantic OMML builder.
//! We keep `raw_xml: Some(fragment)` and a `content_hash` over it.
//!
//! ## Validate at the edge (CLAUDE.md "no silent fallbacks")
//!
//! - the fragment must parse (`word_xml::parse_raw_fragment`) ⇒ else
//!   `EquationXmlInvalid`;
//! - the root element's local name must be `oMath` (inline) or `oMathPara`
//!   (block) ⇒ else `EquationNotMath`;
//! - the root must MATCH the requested placement: `Inline` expects `oMath`,
//!   `Block` expects `oMathPara`. We do NOT silently re-wrap one into the other —
//!   a mismatch is `EquationNotMath` with the offending root name.
//!
//! ## Invariant M untouched
//!
//! We only ADD an opaque (never destroy one) and produce input segments for the
//! one materializer. Inline splices into the anchor segment exactly like
//! `splice_field`; block inserts the `m:oMathPara` opaque as its own segment so
//! the existing serializer path gives it a tracked container.

use super::super::{EditError, MaterializationMode};
use super::super::{find_block_index, validate_block_is_editable};
use crate::domain::{
    BlockNode, CanonDoc, DocPart, InlineNode, NodeId, OpaqueInlineNode, OpaqueKind, ProofRef,
    RevisionInfo, StyleProps, TrackedSegment, TrackingStatus,
};
use crate::import::sha256_hex;
use crate::semantic_hash::check_block_guard;
use crate::word_xml::parse_raw_fragment;

/// Where the equation is placed relative to the paragraph text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EquationPlacement {
    /// Inline math (`m:oMath`), spliced after the `expect` anchor in the run flow.
    Inline,
    /// Display/block math (`m:oMathPara`), a paragraph-direct opaque.
    Block,
}

/// A located insertion point inside a paragraph's segments — the same shape the
/// cross-reference verb uses. `seg_idx`/`inline_idx`/`char_end` pin the split.
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
    omml: &[u8],
    placement: EquationPlacement,
    revision: &RevisionInfo,
    mode: MaterializationMode,
    step_index: usize,
) -> Result<(), EditError> {
    // Validate the OMML fragment at the verb edge, before mutating anything.
    let element = parse_raw_fragment(omml).map_err(|e| EditError::EquationXmlInvalid {
        reason: e.to_string(),
        step_index,
    })?;
    let root_local = element
        .name
        .rsplit_once(':')
        .map(|(_, local)| local)
        .unwrap_or(&element.name)
        .to_string();
    // Enforce placement ⇄ root consistency. We never silently wrap `oMath` into
    // an `oMathPara` (or vice versa); a mismatch is surfaced loudly.
    let expected_root = match placement {
        EquationPlacement::Inline => "oMath",
        EquationPlacement::Block => "oMathPara",
    };
    if root_local != expected_root {
        return Err(EditError::EquationNotMath {
            actual_root: root_local,
            expected_root,
            step_index,
        });
    }

    let idx = find_block_index(&doc.blocks, block_id).ok_or_else(|| EditError::BlockNotFound {
        block_id: block_id.clone(),
        step_index,
    })?;
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

    let kind = match placement {
        EquationPlacement::Inline => OpaqueKind::OmmlInline,
        EquationPlacement::Block => OpaqueKind::OmmlBlock,
    };
    let eq_id = NodeId::from(format!("{}_eq0", para.id.0));
    let equation = synthesize_equation_inline(&eq_id, kind, omml);

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

    splice_equation(&mut para.segments, plan, equation, revision, mode);

    para.block_text_hash = None;
    para.rendered_text = None;
    Ok(())
}

/// Build a fresh `OpaqueInline` carrying the OMML fragment. `raw_xml: Some` (the
/// fragment is the source of truth); `content_hash` over the fragment bytes.
fn synthesize_equation_inline(id: &NodeId, kind: OpaqueKind, omml: &[u8]) -> InlineNode {
    InlineNode::from(OpaqueInlineNode {
        id: id.clone(),
        kind,
        opaque_ref: format!("equationref_{}", id.0),
        proof_ref: ProofRef {
            part: DocPart::DocumentXml,
            block_id: id.clone(),
            docx_anchor: String::new(),
        },
        wrapper_marks: Vec::new(),
        wrapper_style_props: StyleProps::default(),
        raw_xml: Some(omml.to_vec()),
        content_hash: Some(sha256_hex(omml)),
    })
}

/// Locate `expect` inside a single contiguous run of `Text` nodes within one
/// Normal segment — identical anchoring to the cross-reference verb. Returns the
/// segment, the inline index of the text node whose tail contains the match end,
/// and the char offset of the match end inside that node.
fn find_anchor(segments: &[TrackedSegment], expect: &str) -> Option<InsertPlan> {
    if expect.is_empty() {
        return None;
    }
    for (seg_idx, seg) in segments.iter().enumerate() {
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

/// Char offset at which `needle` ends inside `haystack`, or `None`.
fn char_find_end(haystack: &str, needle: &str) -> Option<usize> {
    let byte = haystack.find(needle)?;
    let start = haystack[..byte].chars().count();
    Some(start + needle.chars().count())
}

/// Splice the equation opaque in right after the anchor text. Identical
/// mechanics to `splice_field`: the host segment is split into up to three
/// segments, the equation getting its own `Inserted` segment in TrackedChange
/// mode (folded to Normal in Direct). Accept keeps it; reject drops the Inserted
/// segment and the two Normal halves re-coalesce — reversibility for free.
fn splice_equation(
    segments: &mut Vec<TrackedSegment>,
    plan: InsertPlan,
    equation: InlineNode,
    revision: &RevisionInfo,
    mode: MaterializationMode,
) {
    let host = segments.remove(plan.seg_idx);
    let mut inlines = host.inlines;

    if let InlineNode::Text(node) = &inlines[plan.inline_idx] {
        let chars: Vec<char> = node.text.chars().collect();
        if plan.char_end < chars.len() {
            let before: String = chars[..plan.char_end].iter().collect();
            let after: String = chars[plan.char_end..].iter().collect();
            let mut head = node.clone();
            head.text = before;
            let mut tail = node.clone();
            tail.id = NodeId::new(format!("{}_eqtail", node.id.0));
            tail.text = after;
            inlines.splice(
                plan.inline_idx..=plan.inline_idx,
                [InlineNode::Text(head), InlineNode::Text(tail)],
            );
        }
    }

    let split_at = plan.inline_idx + 1;
    let head_inlines: Vec<InlineNode> = inlines.drain(..split_at).collect();
    let tail_inlines: Vec<InlineNode> = inlines;

    let eq_status = match mode {
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
        status: eq_status,
        inlines: vec![equation],
    });
    if !tail_inlines.is_empty() {
        rebuilt.push(TrackedSegment {
            status: TrackingStatus::Normal,
            inlines: tail_inlines,
        });
    }

    segments.splice(plan.seg_idx..plan.seg_idx, rebuilt);
}

#[cfg(test)]
mod tests {
    use super::*;

    const M_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/math";

    fn omath_inline() -> Vec<u8> {
        format!(r#"<m:oMath xmlns:m="{M_NS}"><m:r><m:t>x</m:t></m:r></m:oMath>"#).into_bytes()
    }

    fn omath_para() -> Vec<u8> {
        format!(
            r#"<m:oMathPara xmlns:m="{M_NS}"><m:oMath><m:r><m:t>E=mc^2</m:t></m:r></m:oMath></m:oMathPara>"#
        )
        .into_bytes()
    }

    #[test]
    fn synthesized_inline_keeps_raw_xml_and_hash() {
        let raw = omath_inline();
        let inline =
            synthesize_equation_inline(&NodeId::from("p_1_eq0"), OpaqueKind::OmmlInline, &raw);
        let InlineNode::OpaqueInline(o) = inline else {
            panic!("expected opaque inline");
        };
        assert!(matches!(o.kind, OpaqueKind::OmmlInline));
        // The fragment IS the source of truth — raw_xml is retained.
        assert_eq!(o.raw_xml.as_deref(), Some(raw.as_slice()));
        assert_eq!(o.content_hash, Some(sha256_hex(&raw)));
    }

    #[test]
    fn block_root_is_omathpara() {
        let raw = omath_para();
        let el = parse_raw_fragment(&raw).unwrap();
        let local = el.name.rsplit_once(':').map(|(_, l)| l).unwrap_or(&el.name);
        assert_eq!(local, "oMathPara");
    }

    #[test]
    fn char_find_end_maps_to_char_offset() {
        assert_eq!(char_find_end("let x be", "let"), Some(3));
        assert_eq!(char_find_end("café au", "café"), Some(4));
        assert_eq!(char_find_end("none", "absent"), None);
    }
}
