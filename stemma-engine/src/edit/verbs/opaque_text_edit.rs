//! `opaque_text_edit` — surgical tracked text replacement INSIDE an opaque
//! region (a textbox's `w:txbxContent` paragraph, or an inline content control's
//! `w:sdtContent`). RFC-0002 §Phase-1.
//!
//! Unlike `set_textbox_text` (whole-interior replace, untracked) this verb splices
//! the FIRST occurrence of `find` → `replacement` inside one addressed region,
//! producing real `w:ins`/`w:del` tracked markup (or a direct replace), and leaves
//! every other byte of the opaque fragment untouched. It rides the shared
//! fragment-splice core [`crate::opaque_splice::splice_region_text`].
//!
//! ## Addressing
//!
//! `(block_id, opaque_id)` locate the hosting paragraph and the opaque inline; the
//! `container_index`/`paragraph_index` (from `opaque_targets::opaque_text_targets`)
//! pick the text region. Textbox Choice/Fallback copies share one logical interior
//! — the edit mirrors across every byte-identical copy so no branch is left stale.
//!
//! ## Fail loud (CLAUDE.md "no silent fallbacks")
//!
//! - opaque id absent / not a textbox or inline SDT → `OpaqueTextTargetNotFound`
//! - the address does not resolve to a text region  → `OpaqueTextRegionNotFound`
//! - no `raw_xml` / unparseable fragment            → `OpaqueTextMissingRawXml` / `OpaqueTextRawXmlParse`
//! - `find` not present / region already tracked / span crosses a barrier →
//!   the mapped `OpaqueText*` splice errors (never a partial write)

use super::super::{EditError, block_at_mut, find_paragraph_path};
use crate::domain::{BlockNode, CanonDoc, InlineNode, NodeId, OpaqueKind, RevisionInfo};
use crate::import::sha256_hex;
use crate::opaque_splice::{SpliceError, splice_region_text};
use crate::opaque_targets::textbox_paragraph_texts;
use crate::word_xml::{is_w_tag, parse_raw_fragment, serialize_raw_fragment};
use xmltree::{Element, XMLNode};

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply(
    doc: &mut CanonDoc,
    block_id: &NodeId,
    opaque_id: &NodeId,
    container_index: usize,
    paragraph_index: usize,
    find: &str,
    replacement: &str,
    semantic_hash: Option<&str>,
    base: &RevisionInfo,
    rev_counter: &mut u32,
    tracked: bool,
    step_index: usize,
) -> Result<(), EditError> {
    // Host paragraph — `find_paragraph_path` reaches into table cells.
    let path = find_paragraph_path(doc, block_id).ok_or_else(|| EditError::BlockNotFound {
        block_id: block_id.clone(),
        step_index,
    })?;
    let BlockNode::Paragraph(para) = block_at_mut(doc, &path) else {
        return Err(EditError::OpaqueTextTargetNotFound {
            opaque_id: opaque_id.clone(),
            step_index,
        });
    };

    let node = para
        .segments
        .iter_mut()
        .flat_map(|s| s.inlines.iter_mut())
        .find_map(|inline| match inline {
            InlineNode::OpaqueInline(o) if o.id == *opaque_id => Some(o),
            _ => None,
        })
        .ok_or_else(|| EditError::OpaqueTextTargetNotFound {
            opaque_id: opaque_id.clone(),
            step_index,
        })?;

    if let Some(expected) = semantic_hash {
        let actual = node.content_hash.as_deref().unwrap_or("");
        if actual != expected {
            return Err(EditError::BlockSemanticHashMismatch {
                block_id: opaque_id.clone(),
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
            opaque_id: opaque_id.clone(),
            step_index,
        })?;
    let mut element = parse_raw_fragment(raw).map_err(|e| EditError::OpaqueTextRawXmlParse {
        opaque_id: opaque_id.clone(),
        reason: e.to_string(),
        step_index,
    })?;

    match &node.kind {
        OpaqueKind::Drawing => edit_textbox(
            &mut element,
            opaque_id,
            container_index,
            paragraph_index,
            find,
            replacement,
            base,
            rev_counter,
            tracked,
            step_index,
        )?,
        OpaqueKind::Sdt => edit_inline_sdt(
            &mut element,
            opaque_id,
            container_index,
            paragraph_index,
            find,
            replacement,
            base,
            rev_counter,
            tracked,
            step_index,
        )?,
        _ => {
            return Err(EditError::OpaqueTextTargetNotFound {
                opaque_id: opaque_id.clone(),
                step_index,
            });
        }
    }

    let new_raw = serialize_raw_fragment(&element);
    node.content_hash = Some(sha256_hex(&new_raw));
    node.raw_xml = Some(new_raw);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn edit_textbox(
    element: &mut Element,
    opaque_id: &NodeId,
    container_index: usize,
    paragraph_index: usize,
    find: &str,
    replacement: &str,
    base: &RevisionInfo,
    rev_counter: &mut u32,
    tracked: bool,
    step_index: usize,
) -> Result<(), EditError> {
    // Immutable pass: the DISTINCT textbox interiors (byte-deduped Choice/Fallback
    // copies collapse to one), in document order — the same enumeration discovery
    // used to mint the address. Validate the (container, paragraph) address here.
    let mut copies = Vec::new();
    crate::opaque_meta::collect_descendants_by_local(element, "txbxContent", &mut copies);
    let mut distinct: Vec<Vec<String>> = Vec::new();
    for c in &copies {
        let paras = textbox_paragraph_texts(c);
        if paras.is_empty() {
            continue;
        }
        if !distinct.contains(&paras) {
            distinct.push(paras);
        }
    }
    let region_not_found = || EditError::OpaqueTextRegionNotFound {
        opaque_id: opaque_id.clone(),
        container_index,
        paragraph_index,
        step_index,
    };
    let target = distinct.get(container_index).ok_or_else(region_not_found)?;
    if paragraph_index >= target.len() {
        return Err(region_not_found());
    }
    let target: Vec<String> = target.clone();

    // Mutable pass: splice the addressed paragraph in EVERY copy of this distinct
    // interior (keeping Choice/Fallback consistent). `for_each_txbxcontent_mut`
    // does not recurse into a matched `txbxContent`, so a nested textbox belonging
    // to a nested anchor is never miscounted as a copy of ours.
    //
    // Known v1 limitation: each mirrored copy's splice draws its OWN revision-id
    // pair from the counter, so a selective accept/reject of one id resolves one
    // copy and leaves its siblings pending until their ids are resolved too
    // (accept-all/reject-all keep copies consistent). Minting one SHARED pair
    // would instead demote the ids to census-only under the duplicate-id rule
    // (`tracked_model::classify_interior_ids`) — strictly less capable. Revisit
    // if selective resolution inside mirrored textboxes becomes a workflow.
    let mut matched = 0usize;
    let mut spliced = 0usize;
    for_each_txbxcontent_mut(element, &mut |content| {
        if textbox_paragraph_texts(content) != target {
            return Ok(());
        }
        matched += 1;
        let Some(p) = nth_text_paragraph_mut(content, paragraph_index) else {
            // Counted but not spliced — refused as a partial mirror below.
            return Ok(());
        };
        splice_region_text(p, find, replacement, base, rev_counter, tracked)?;
        spliced += 1;
        Ok(())
    })
    .map_err(|e| map_splice_error(e, opaque_id, find, step_index))?;

    if spliced == 0 {
        return Err(region_not_found());
    }
    // The copies matched by visible text, so the paragraph index must resolve
    // in every one of them. A copy that matched but failed to resolve would be
    // left silently stale while its siblings were edited — a partial mirror is
    // corruption, not a best-effort success.
    if spliced != matched {
        return Err(EditError::OpaqueTextMirrorDivergence {
            opaque_id: opaque_id.clone(),
            matched,
            spliced,
            step_index,
        });
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn edit_inline_sdt(
    element: &mut Element,
    opaque_id: &NodeId,
    container_index: usize,
    paragraph_index: usize,
    find: &str,
    replacement: &str,
    base: &RevisionInfo,
    rev_counter: &mut u32,
    tracked: bool,
    step_index: usize,
) -> Result<(), EditError> {
    // An inline content control's text region is its single `w:sdtContent`.
    if container_index != 0 || paragraph_index != 0 {
        return Err(EditError::OpaqueTextRegionNotFound {
            opaque_id: opaque_id.clone(),
            container_index,
            paragraph_index,
            step_index,
        });
    }
    let content =
        crate::opaque_splice::first_descendant_mut(element, "sdtContent").ok_or_else(|| {
            EditError::OpaqueTextRegionNotFound {
                opaque_id: opaque_id.clone(),
                container_index,
                paragraph_index,
                step_index,
            }
        })?;
    splice_region_text(content, find, replacement, base, rev_counter, tracked)
        .map_err(|e| map_splice_error(e, opaque_id, find, step_index))
}

fn map_splice_error(
    e: SpliceError,
    opaque_id: &NodeId,
    find: &str,
    step_index: usize,
) -> EditError {
    match e {
        SpliceError::TextNotFound => EditError::OpaqueTextNotFound {
            opaque_id: opaque_id.clone(),
            find: find.to_string(),
            step_index,
        },
        SpliceError::RegionHasTrackedChanges => EditError::OpaqueTextRegionHasTrackedChanges {
            opaque_id: opaque_id.clone(),
            step_index,
        },
        // The find-based splice never sets a whole value, so it cannot produce a
        // complex-content refusal; treat it as the same unsupported-shape class.
        SpliceError::UnsupportedRegionShape | SpliceError::RegionHasComplexContent => {
            EditError::OpaqueTextUnsupportedShape {
                opaque_id: opaque_id.clone(),
                step_index,
            }
        }
    }
}

/// Apply `f` to each `w:txbxContent` in `el`, without recursing INTO a matched
/// one (a `txbxContent`'s own nested drawing/textbox belongs to that nested
/// anchor, not this one — mirrors `opaque_meta::collect_descendants_by_local`).
fn for_each_txbxcontent_mut(
    el: &mut Element,
    f: &mut impl FnMut(&mut Element) -> Result<(), SpliceError>,
) -> Result<(), SpliceError> {
    if is_w_tag(el, "txbxContent") {
        return f(el);
    }
    for child in &mut el.children {
        if let XMLNode::Element(c) = child {
            for_each_txbxcontent_mut(c, f)?;
        }
    }
    Ok(())
}

/// The `index`-th DIRECT text-bearing `w:p` child of a `w:txbxContent` (matching
/// `textbox_paragraph_texts`' ordering — the address discovery minted against).
fn nth_text_paragraph_mut(content: &mut Element, index: usize) -> Option<&mut Element> {
    let mut seen = 0;
    for child in &mut content.children {
        if let XMLNode::Element(el) = child
            && is_w_tag(el, "p")
        {
            let mut text = String::new();
            crate::opaque_targets::collect_wt_text(el, &mut text);
            if !text.is_empty() {
                if seen == index {
                    return Some(el);
                }
                seen += 1;
            }
        }
    }
    None
}
