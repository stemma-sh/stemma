//! `sdt_text_fill` — set a content control's text value, tracked. RFC-0002
//! §Phase-2. The forms-natural operation ("set this field's value"): a
//! whole-value replace of the control's text, shown as a redline (old struck
//! through, new inserted) or applied directly.
//!
//! Two targets, one semantic:
//! INLINE control (`OpaqueInline{Sdt}`, addressed by host paragraph + sdt id):
//! its `raw_xml` is spliced in place with the shared
//! [`crate::opaque_splice::set_region_text`] core.
//!
//! BLOCK (body-level) control (`OpaqueBlock{Sdt}`, addressed by the frozen
//! `body_index` discovery surfaced): its bytes live in the serialize scaffold,
//! unreachable from the pure edit core — so the fill is validated and its
//! tracked-change ids minted here, then STAGED into `PendingParts` for the save
//! path to apply against the scaffold node.
//!
//! Fail loud (CLAUDE.md "no silent fallbacks"): neither/both targets ⇒
//! `SdtFillAmbiguousTarget`; missing inline sdt / block ⇒ `OpaqueTextTargetNotFound`
//! / `SdtFillBlockNotFound`; an empty fill of an empty control ⇒ `SdtFillEmpty`.

use super::super::EditError;
use super::super::{block_at_mut, find_paragraph_path};
use crate::domain::{BlockNode, CanonDoc, InlineNode, NodeId, OpaqueKind, RevisionInfo};
use crate::edit::pending_parts::{OpaqueChildTextSet, PendingParts};
use crate::import::sha256_hex;
use crate::opaque_splice::{SpliceError, first_descendant_mut, set_region_text};
use crate::word_xml::{parse_raw_fragment, serialize_raw_fragment};

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply(
    doc: &mut CanonDoc,
    block_id: Option<&NodeId>,
    sdt_id: Option<&NodeId>,
    body_index: Option<usize>,
    value: &str,
    semantic_hash: Option<&str>,
    base: &RevisionInfo,
    rev_counter: &mut u32,
    tracked: bool,
    pending: &mut PendingParts,
    step_index: usize,
) -> Result<(), EditError> {
    match (sdt_id, body_index) {
        (Some(sdt_id), None) => fill_inline(
            doc,
            block_id,
            sdt_id,
            value,
            semantic_hash,
            base,
            rev_counter,
            tracked,
            step_index,
        ),
        (None, Some(body_index)) => stage_block(
            doc,
            body_index,
            value,
            semantic_hash,
            base,
            rev_counter,
            tracked,
            pending,
            step_index,
        ),
        // Exactly one target must be named.
        _ => Err(EditError::SdtFillAmbiguousTarget { step_index }),
    }
}

#[allow(clippy::too_many_arguments)]
fn fill_inline(
    doc: &mut CanonDoc,
    block_id: Option<&NodeId>,
    sdt_id: &NodeId,
    value: &str,
    semantic_hash: Option<&str>,
    base: &RevisionInfo,
    rev_counter: &mut u32,
    tracked: bool,
    step_index: usize,
) -> Result<(), EditError> {
    let block_id = block_id.ok_or(EditError::SdtFillAmbiguousTarget { step_index })?;
    let path = find_paragraph_path(doc, block_id).ok_or_else(|| EditError::BlockNotFound {
        block_id: block_id.clone(),
        step_index,
    })?;
    let BlockNode::Paragraph(para) = block_at_mut(doc, &path) else {
        return Err(EditError::OpaqueTextTargetNotFound {
            opaque_id: sdt_id.clone(),
            step_index,
        });
    };
    let node = para
        .segments
        .iter_mut()
        .flat_map(|s| s.inlines.iter_mut())
        .find_map(|inline| match inline {
            InlineNode::OpaqueInline(o) if o.id == *sdt_id && matches!(o.kind, OpaqueKind::Sdt) => {
                Some(o)
            }
            _ => None,
        })
        .ok_or_else(|| EditError::OpaqueTextTargetNotFound {
            opaque_id: sdt_id.clone(),
            step_index,
        })?;

    if let Some(expected) = semantic_hash {
        let actual = node.content_hash.as_deref().unwrap_or("");
        if actual != expected {
            return Err(EditError::BlockSemanticHashMismatch {
                block_id: sdt_id.clone(),
                expected: expected.to_string(),
                actual: actual.to_string(),
                step_index,
            });
        }
    }

    let raw = node
        .raw_xml
        .as_deref()
        .ok_or_else(|| EditError::OpaqueTextMissingRawXml {
            opaque_id: sdt_id.clone(),
            step_index,
        })?;
    let mut element = parse_raw_fragment(raw).map_err(|e| EditError::OpaqueTextRawXmlParse {
        opaque_id: sdt_id.clone(),
        reason: e.to_string(),
        step_index,
    })?;
    let content = first_descendant_mut(&mut element, "sdtContent").ok_or_else(|| {
        EditError::OpaqueTextRegionNotFound {
            opaque_id: sdt_id.clone(),
            container_index: 0,
            paragraph_index: 0,
            step_index,
        }
    })?;
    set_region_text(content, value, base, rev_counter, tracked).map_err(|e| match e {
        SpliceError::RegionHasTrackedChanges => EditError::OpaqueTextRegionHasTrackedChanges {
            opaque_id: sdt_id.clone(),
            step_index,
        },
        // Text hidden in a hyperlink/field/nested control — not cleanly fillable.
        SpliceError::RegionHasComplexContent => EditError::SdtFillComplexContent {
            sdt_id: sdt_id.clone(),
            step_index,
        },
        // A whole-value SET never crosses a barrier; TextNotFound here means an
        // empty fill of an already-empty control (nothing to do).
        SpliceError::TextNotFound | SpliceError::UnsupportedRegionShape => {
            EditError::SdtFillEmpty { step_index }
        }
    })?;

    let new_raw = serialize_raw_fragment(&element);
    node.content_hash = Some(sha256_hex(&new_raw));
    node.raw_xml = Some(new_raw);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn stage_block(
    doc: &CanonDoc,
    body_index: usize,
    value: &str,
    semantic_hash: Option<&str>,
    base: &RevisionInfo,
    rev_counter: &mut u32,
    tracked: bool,
    pending: &mut PendingParts,
    step_index: usize,
) -> Result<(), EditError> {
    // A block control's bytes live in the serialize scaffold, unreachable from
    // this pure core, and block discovery surfaces no hash — a supplied
    // `semantic_hash` precondition CANNOT be honored here. Refuse rather than
    // silently ignore a stale-edit guard the caller is counting on.
    if semantic_hash.is_some() {
        return Err(EditError::SdtFillBlockHashUnsupported {
            body_index,
            step_index,
        });
    }
    // One fill per block control per transaction: a second staged fill of the
    // same body_index would clobber the first (direct) or splice into
    // already-tracked bytes (tracked) only at SAVE time — refuse here, where
    // the step index still names the offending step.
    if pending
        .opaque_child_text_sets
        .iter()
        .any(|s| s.body_index == body_index)
    {
        return Err(EditError::SdtFillDuplicateBlockTarget {
            body_index,
            step_index,
        });
    }
    // Validate the target block-level content control exists (structural — the
    // bytes are in the scaffold, applied at save time). Refuse a no-op empty fill.
    let exists = doc.blocks.iter().any(|tb| {
        matches!(&tb.block, BlockNode::OpaqueBlock(o)
            if matches!(o.kind, OpaqueKind::Sdt)
                && o.proof_ref
                    .docx_anchor
                    .strip_prefix("body_index:")
                    .and_then(|n| n.parse::<usize>().ok())
                    == Some(body_index))
    });
    if !exists {
        return Err(EditError::SdtFillBlockNotFound {
            body_index,
            step_index,
        });
    }
    if value.is_empty() {
        // A block fill has no current-text visibility here; an empty value is a
        // meaningless request (clearing a form value is not a v1 operation).
        return Err(EditError::SdtFillEmpty { step_index });
    }
    // Mint the tracked-change ids now (unique across the whole document, from the
    // transaction counter) so the save-time application stamps consistent ids.
    let revision_ids = if tracked {
        let del = *rev_counter;
        let ins = *rev_counter + 1;
        *rev_counter += 2;
        [del, ins]
    } else {
        [0, 0]
    };
    pending.opaque_child_text_sets.push(OpaqueChildTextSet {
        body_index,
        value: value.to_string(),
        tracked,
        author: base.author.clone().unwrap_or_default(),
        date: base.date.clone(),
        revision_ids,
    });
    Ok(())
}
