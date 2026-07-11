//! Block-level content controls (`w:sdt`, §17.5.2) — the **`WrapBlocksInContentControl`**
//! verb. It is the block-level sibling of the inline run-span wrap
//! (`WrapInContentControl`, see `verbs::content_controls`): instead of wrapping a
//! run-span inside one paragraph, it wraps a contiguous RANGE of body blocks
//! (paragraphs / tables) in a single block-level `w:sdt` whose `w:sdtContent`
//! encloses the whole range.
//!
//! ## Model
//!
//! The wrap is recorded as a [`BlockSdtWrap`] on the FIRST [`TrackedBlock`] of
//! the range (`block_sdt_wrap`), carrying the deterministically-built `w:sdtPr`
//! (from a typed [`SdtSpec`], via `serialize::sdt::build_sdt_pr`) and the `span`
//! = number of enclosed blocks. The wrapped blocks themselves are NOT cloned or
//! mutated — their content and opaques are preserved exactly; only the enclosing
//! wrapper is added. Serialization (`runtime::serialize_canonical_docx`) emits
//! `<w:sdt><w:sdtPr>…</w:sdtPr><w:sdtContent>` before the first block and closes
//! `</w:sdtContent></w:sdt>` after the `span`-th block.
//!
//! ## Untracked / structural (honest reversibility)
//!
//! OOXML has NO tracked-change envelope for SDT structure (there is no
//! `w:sdtChange` the way there is `w:rPrChange`), exactly as for the inline wrap.
//! So this verb is Direct/structural: the materialization mode does not change
//! its behavior, and **accept-all == reject-all == the wrapped doc**. The wrap
//! marker lives on a `Normal` block, so the accept/reject projection (which only
//! filters Inserted/Deleted blocks and never touches `Normal` ones) carries it
//! through unchanged. Reversibility is at the **transaction-rejection** level
//! (don't apply), not at segment accept/reject.
//!
//! ## Fail loud (CLAUDE.md "no silent fallbacks")
//!
//! - empty distinguishing spec (no tag, no alias, RichText default) ⇒
//!   [`EditError::EmptyContentControlSpec`] — a control with no identity and no
//!   kind is indistinguishable from un-wrapped content;
//! - a `start`/`end` pair that is not a valid forward, contiguous, top-level
//!   range ⇒ [`EditError::BlockRangeInvalid`];
//! - any block in the range that is not editable (tracked-inserted/deleted, or a
//!   paragraph with tracked segments) ⇒ the usual editability errors;
//! - any block in the range already carrying a block-level wrap ⇒
//!   [`EditError::BlockAlreadyWrapped`] (we never nest an authored wrap inside an
//!   authored one — there is no serializable representation and it risks an
//!   unbalanced `w:sdt`).

use super::super::{EditError, find_block_index, validate_block_is_editable};
use super::content_controls::SdtSpec;
use crate::domain::{BlockSdtWrap, CanonDoc, NodeId, SdtWrapper};
use crate::serialize::sdt::build_sdt_pr;

/// Wrap the contiguous, top-level block range `[start_block_id, end_block_id]`
/// (inclusive) in a single block-level `w:sdt`. The wrapper is recorded on the
/// first block of the range; serialization emits the enclosing envelope.
pub(crate) fn apply_wrap_blocks(
    doc: &mut CanonDoc,
    start_block_id: &NodeId,
    end_block_id: &NodeId,
    spec: &SdtSpec,
    step_index: usize,
) -> Result<(), EditError> {
    // Reject a spec with no distinguishing data at the verb edge (same rule as
    // the inline wrap): a control with no tag/alias and the default rich-text
    // kind is indistinguishable from un-wrapped content.
    if spec.is_empty() {
        return Err(EditError::EmptyContentControlSpec { step_index });
    }

    // Data binding on a block-level wrap is out of scope (v1): the inline wrap
    // is the data-binding path (it stages the backing datastore part). A block
    // wrap that carried a binding would emit a dangling `storeItemID` with no
    // authored part. Refuse loudly rather than silently dropping the binding.
    if spec.binding.is_some() {
        return Err(EditError::MalformedDataBinding {
            reason: "data binding is not supported on a block-level content-control wrap (use the inline wrap)",
            step_index,
        });
    }

    let start =
        find_block_index(&doc.blocks, start_block_id).ok_or_else(|| EditError::BlockNotFound {
            block_id: start_block_id.clone(),
            step_index,
        })?;
    let end =
        find_block_index(&doc.blocks, end_block_id).ok_or_else(|| EditError::BlockNotFound {
            block_id: end_block_id.clone(),
            step_index,
        })?;

    // The range must run forward (start <= end). A backward pair is a caller
    // error — never silently swap the ends.
    if end < start {
        return Err(EditError::BlockRangeInvalid {
            start_block_id: start_block_id.clone(),
            end_block_id: end_block_id.clone(),
            reason: "end block precedes start block",
            step_index,
        });
    }

    let span = end - start + 1;

    // No block in the new range may already be enclosed by an authored
    // block-level wrap. Because the marker lives only on the FIRST block of an
    // existing wrap (its `w:sdtContent` then spans `span` blocks), we cannot rely
    // on a per-block `block_sdt_wrap.is_some()` check — an *enclosed* block does
    // not carry the marker. Compute every existing wrap's covered interval
    // `[first, first + wrap.span)` and refuse if the new range `[start, end]`
    // intersects any of them. This catches the start/middle/end overlap cases and
    // a new range that would straddle (split) an existing wrap. Done BEFORE any
    // mutation so a failed call never leaves a partial wrap behind.
    for (i, tracked) in doc.blocks.iter().enumerate() {
        if let Some(existing) = &tracked.block_sdt_wrap {
            let ex_start = i;
            let ex_end = i + existing.span - 1; // inclusive
            // Two inclusive ranges [start,end] and [ex_start,ex_end] overlap iff
            // start <= ex_end && ex_start <= end.
            if start <= ex_end && ex_start <= end {
                return Err(EditError::BlockAlreadyWrapped {
                    block_id: block_id_of_block(&doc.blocks[ex_start].block).clone(),
                    step_index,
                });
            }
        }
    }

    // Every block in the range must be editable, BEFORE we mutate anything.
    for tracked in &doc.blocks[start..=end] {
        validate_block_is_editable(tracked, step_index)?;
    }

    // Build the wrapper's `w:sdtPr` deterministically from the typed spec. The
    // `w:id` is derived from the start block id (opaque to Word; uniqueness is
    // all that matters). Reuse the same builder the inline wrap uses, so an
    // authored block wrap is byte-consistent with an authored inline wrap.
    let sdt_id = stable_sdt_id(&start_block_id.0);
    let sdt_pr_xml = build_sdt_pr(
        sdt_id,
        spec.tag.as_deref(),
        spec.alias.as_deref(),
        &spec.control,
        // Block-level wraps never carry a data binding (refused above).
        None,
    )
    .into_bytes();

    doc.blocks[start].block_sdt_wrap = Some(BlockSdtWrap {
        wrapper: SdtWrapper {
            sdt_pr_xml,
            sdt_end_pr_xml: None,
        },
        span,
    });

    Ok(())
}

/// The stable node id of a block, for error context / id derivation.
fn block_id_of_block(block: &crate::domain::BlockNode) -> &NodeId {
    use crate::domain::BlockNode;
    match block {
        BlockNode::Paragraph(p) => &p.id,
        BlockNode::Table(t) => &t.id,
        BlockNode::OpaqueBlock(o) => &o.id,
    }
}

/// A stable, document-unique positive decimal id for `w:id`, derived from the
/// host id text (mirrors the inline wrap's `stable_sdt_id`). The value is opaque
/// to Word — uniqueness is all that matters.
fn stable_sdt_id(seed: &str) -> i32 {
    let hex = crate::import::sha256_hex(seed.as_bytes());
    let n = i32::from_str_radix(&hex[..7], 16).unwrap_or(1);
    n.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::SdtControl;

    #[test]
    fn stable_id_is_positive_and_deterministic() {
        let a = stable_sdt_id("p_1");
        let b = stable_sdt_id("p_1");
        assert_eq!(a, b);
        assert!(a >= 1);
        assert_ne!(stable_sdt_id("p_1"), stable_sdt_id("p_2"));
    }

    #[test]
    fn empty_spec_is_refused_label() {
        // A no-tag / no-alias / rich-text spec carries no identity.
        let spec = SdtSpec {
            tag: None,
            alias: None,
            control: SdtControl::RichText,
            binding: None,
        };
        assert!(spec.is_empty());
    }
}
