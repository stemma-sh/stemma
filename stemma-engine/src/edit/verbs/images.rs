//! `SetImageAttributes` — author an in-place attribute edit on an existing
//! opaque drawing (`w:drawing`): resize it (`wp:extent` @cx/@cy, §20.4.2.7) and
//! /or set its alt text (`wp:docPr` @descr, §20.4.2.5).
//!
//! ## Model
//!
//! A drawing is an [`OpaqueInlineNode`] of kind [`OpaqueKind::Drawing`]. Its
//! display tree (the `wp:inline`/`wp:anchor` envelope, `wp:extent`, `wp:docPr`,
//! the `a:graphic` graphic-frame) lives serialized in `raw_xml`. The *binary
//! media* (the PNG/JPEG bytes) is a package part referenced by a relationship —
//! it is NEVER in the IR and this verb NEVER reads or touches it. We only mutate
//! the drawing's display attributes inside `raw_xml`.
//!
//! This is a **direct, untracked** attribute edit, exactly like
//! [`crate::edit::EditStep::SetHyperlinkAttr`]: OOXML has no tracked-change
//! envelope for opaque-drawing display attributes (there is no `w:drawingChange`
//! the way there is `w:rPrChange`), so we cannot author a reject-able tracked
//! delta here. Both materialization modes behave identically — the mutation is
//! silent in the tracked-change audit trail. Reversibility is therefore at the
//! transaction-rejection level (don't apply), not at segment-accept/reject.
//!
//! ## Fail loud (CLAUDE.md "no silent fallbacks")
//!
//! - missing drawing id           → `DrawingNotFound`
//! - id resolves to a non-drawing → `NotADrawing`
//! - drawing has no `raw_xml`     → `DrawingMissingRawXml`
//! - nothing to edit on the node  → `ImageAttributeTargetAbsent`
//!   (resize requested but no `wp:extent`; alt-text requested but no `wp:docPr`)
//! - empty request                → `NoImageAttributeRequested`
//!
//! ## Out of scope (cross-cutting foundation work, flagged not attempted)
//!
//! Image *insert*/*replace* needs a `DocxPackage` handle to add a media part and
//! a relationship; `apply_transaction` operates on the IR alone and has no such
//! handle. Adding a media part is a packaging change, not a verb.

use super::super::{
    EditError, MaterializationMode, apply_opaque_delete, block_at_mut,
    check_ancestor_table_tracking, find_block_index, find_opaque_flat_index, find_paragraph_path,
};
use crate::domain::{
    BlockNode, CanonDoc, InlineNode, NodeId, OpaqueInlineNode, OpaqueKind, RevisionInfo,
};
use crate::import::sha256_hex;
use crate::word_xml::{parse_raw_fragment, serialize_raw_fragment};
use xmltree::{Element, XMLNode};

/// New drawing dimensions in EMUs (English Metric Units, 914400 per inch) for
/// `wp:extent` @cx (width) / @cy (height). Both are required: OOXML's
/// `wp:extent` carries both, and resizing one without the other distorts the
/// aspect ratio silently — the caller states the full target box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageResize {
    /// New width in EMUs. Must be `>= 0` (validated at the wire edge).
    pub cx_emu: i64,
    /// New height in EMUs. Must be `>= 0` (validated at the wire edge).
    pub cy_emu: i64,
}

/// Apply a `SetImageAttributes` step: locate the drawing by `drawing_id`,
/// optionally check `semantic_hash`, mutate `wp:extent` and/or `wp:docPr`
/// inside `raw_xml`, re-serialize, and recompute `content_hash`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply(
    doc: &mut CanonDoc,
    block_id: &NodeId,
    drawing_id: &NodeId,
    semantic_hash: Option<&str>,
    resize: Option<ImageResize>,
    alt_text: Option<Option<String>>,
    step_index: usize,
) -> Result<(), EditError> {
    // Empty request is a no-op we refuse rather than silently accept.
    if resize.is_none() && alt_text.is_none() {
        return Err(EditError::NoImageAttributeRequested { step_index });
    }

    let idx = find_block_index(&doc.blocks, block_id).ok_or_else(|| EditError::BlockNotFound {
        block_id: block_id.clone(),
        step_index,
    })?;

    let node = locate_drawing_mut(&mut doc.blocks[idx].block, drawing_id, step_index)?;

    if let Some(expected) = semantic_hash {
        let actual = node.content_hash.as_deref().unwrap_or("");
        if actual != expected {
            return Err(EditError::BlockSemanticHashMismatch {
                block_id: drawing_id.clone(),
                expected: expected.to_string(),
                actual: actual.to_string(),
                step_index,
            });
        }
    }

    let raw = node
        .raw_xml
        .as_deref()
        .ok_or_else(|| EditError::DrawingMissingRawXml {
            drawing_id: drawing_id.clone(),
            step_index,
        })?;

    let mut element = parse_raw_fragment(raw).map_err(|e| EditError::DrawingRawXmlParse {
        drawing_id: drawing_id.clone(),
        reason: e.to_string(),
        step_index,
    })?;

    // Mutate the requested attributes; fail loud if the requested target node is
    // not present (no silent skip).
    if let Some(r) = resize {
        let extent = find_descendant_by_local_mut(&mut element, "extent").ok_or_else(|| {
            EditError::ImageAttributeTargetAbsent {
                drawing_id: drawing_id.clone(),
                attribute: "wp:extent",
                step_index,
            }
        })?;
        // wp:extent @cx/@cy are bare (un-prefixed) attributes per the DrawingML
        // schema. We write the inline-display extent specifically — NOT the
        // inner a:ext on the graphic frame, which is the picture's own size box
        // and is keyed separately.
        crate::xml_attrs::attr_set(extent, "cx", r.cx_emu.to_string());
        crate::xml_attrs::attr_set(extent, "cy", r.cy_emu.to_string());
    }

    if let Some(descr) = alt_text {
        let doc_pr = find_descendant_by_local_mut(&mut element, "docPr").ok_or_else(|| {
            EditError::ImageAttributeTargetAbsent {
                drawing_id: drawing_id.clone(),
                attribute: "wp:docPr",
                step_index,
            }
        })?;
        match descr {
            // Set/replace the alt text.
            Some(text) => crate::xml_attrs::attr_set(doc_pr, "descr", text),
            // Clear it: remove the @descr attribute entirely (absent == no alt
            // text), rather than writing an empty string.
            None => remove_bare_attr(doc_pr, "descr"),
        }
    }

    let new_raw = serialize_raw_fragment(&element);
    node.content_hash = Some(sha256_hex(&new_raw));
    node.raw_xml = Some(new_raw);
    Ok(())
}

/// Delete an existing inline Drawing opaque by id. Resolves the host paragraph
/// cell-aware (`find_paragraph_path`, NOT `find_block_index` which is top-level
/// only), guards on the DRAWING's own `content_hash` (like [`apply`]), then flips
/// the opaque's segment status via [`apply_opaque_delete`] (tracked → `Deleted`,
/// direct → dropped). Never routes through the text-replace path, so it neither
/// touches nor weakens the `OpaqueDestroyed` guard.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_delete_image(
    doc: &mut CanonDoc,
    block_id: &NodeId,
    drawing_id: &NodeId,
    semantic_hash: Option<&str>,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    mode: MaterializationMode,
    step_index: usize,
) -> Result<(), EditError> {
    let path = find_paragraph_path(doc, block_id).ok_or_else(|| EditError::BlockNotFound {
        block_id: block_id.clone(),
        step_index,
    })?;
    // A cell-hosted drawing: refuse if an enclosing row/cell/table is itself
    // tracked-inserted/deleted (same gate as an in-cell text edit).
    if !path.is_top_level() {
        check_ancestor_table_tracking(doc, &path, block_id, step_index)?;
    }
    // Guard + drawing-kind check on the drawing's own content_hash (scoped so the
    // node borrow ends before we re-borrow the block to edit its segments).
    {
        let block = block_at_mut(doc, &path);
        let node = locate_drawing_mut(block, drawing_id, step_index)?;
        if let Some(expected) = semantic_hash {
            let actual = node.content_hash.as_deref().unwrap_or("");
            if actual != expected {
                return Err(EditError::BlockSemanticHashMismatch {
                    block_id: drawing_id.clone(),
                    expected: expected.to_string(),
                    actual: actual.to_string(),
                    step_index,
                });
            }
        }
    }
    let BlockNode::Paragraph(para) = block_at_mut(doc, &path) else {
        return Err(EditError::DrawingNotFound {
            drawing_id: drawing_id.clone(),
            step_index,
        });
    };
    let idx =
        find_opaque_flat_index(para, drawing_id).ok_or_else(|| EditError::DrawingNotFound {
            drawing_id: drawing_id.clone(),
            step_index,
        })?;
    apply_opaque_delete(para, idx, mode, revision, rev_counter);
    Ok(())
}

/// Locate the [`OpaqueInlineNode`] with `drawing_id` inside `block`, requiring it
/// to be a paragraph-hosted drawing. Mirrors the hyperlink-by-id lookup:
/// `DrawingNotFound` when no opaque inline carries the id, `NotADrawing` when one
/// does but is some other opaque kind.
pub(crate) fn locate_drawing_mut<'a>(
    block: &'a mut BlockNode,
    drawing_id: &NodeId,
    step_index: usize,
) -> Result<&'a mut OpaqueInlineNode, EditError> {
    let BlockNode::Paragraph(para) = block else {
        return Err(EditError::DrawingNotFound {
            drawing_id: drawing_id.clone(),
            step_index,
        });
    };
    // Two passes to keep the borrow checker happy: first decide the outcome by
    // an immutable scan, then take the mutable handle. The drawing id is unique
    // per document, so the first match is the only match.
    let mut found_kind: Option<bool> = None; // Some(true) == is a drawing
    for seg in &para.segments {
        for inline in &seg.inlines {
            if let InlineNode::OpaqueInline(o) = inline
                && o.id == *drawing_id
            {
                found_kind = Some(matches!(o.kind, OpaqueKind::Drawing));
                break;
            }
        }
        if found_kind.is_some() {
            break;
        }
    }
    match found_kind {
        None => Err(EditError::DrawingNotFound {
            drawing_id: drawing_id.clone(),
            step_index,
        }),
        Some(false) => Err(EditError::NotADrawing {
            drawing_id: drawing_id.clone(),
            step_index,
        }),
        Some(true) => {
            for seg in &mut para.segments {
                for inline in &mut seg.inlines {
                    if let InlineNode::OpaqueInline(o) = inline
                        && o.id == *drawing_id
                    {
                        return Ok(o);
                    }
                }
            }
            unreachable!("drawing located in the immutable scan above");
        }
    }
}

/// Depth-first search for the first descendant element (or `root` itself) whose
/// local name equals `local`, ignoring namespace prefix (so `wp:extent` matches
/// on `"extent"`). Returns a mutable handle to mutate its attributes in place.
pub(crate) fn find_descendant_by_local_mut<'a>(
    root: &'a mut Element,
    local: &str,
) -> Option<&'a mut Element> {
    if root.name == local {
        return Some(root);
    }
    for child in &mut root.children {
        if let XMLNode::Element(el) = child
            && let Some(hit) = find_descendant_by_local_mut(el, local)
        {
            return Some(hit);
        }
    }
    None
}

/// Remove a bare (un-prefixed) attribute by local name if present.
fn remove_bare_attr(element: &mut Element, local: &str) {
    let keys: Vec<_> = element
        .attributes
        .keys()
        .filter(|k| k.local_name == local)
        .cloned()
        .collect();
    for k in keys {
        element.attributes.shift_remove(&k);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal inline-drawing fragment with a `wp:extent` and a `wp:docPr`.
    fn drawing_fragment(cx: i64, cy: i64, descr: Option<&str>) -> Vec<u8> {
        let descr_attr = descr
            .map(|d| format!(r#" descr="{d}""#))
            .unwrap_or_default();
        format!(
            r#"<w:drawing xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><wp:inline><wp:extent cx="{cx}" cy="{cy}"/><wp:docPr id="1" name="Picture 1"{descr_attr}/><a:graphic><a:graphicData><a:ext cx="999" cy="999"/></a:graphicData></a:graphic></wp:inline></w:drawing>"#
        )
        .into_bytes()
    }

    fn parse(raw: &[u8]) -> Element {
        parse_raw_fragment(raw).expect("parse fragment")
    }

    #[test]
    fn resize_targets_wp_extent_not_inner_a_ext() {
        let mut el = parse(&drawing_fragment(100, 200, None));
        let extent = find_descendant_by_local_mut(&mut el, "extent").unwrap();
        crate::xml_attrs::attr_set(extent, "cx", "777");
        crate::xml_attrs::attr_set(extent, "cy", "888");
        let out = String::from_utf8(serialize_raw_fragment(&el)).unwrap();
        // wp:extent updated...
        assert!(out.contains(r#"cx="777""#));
        assert!(out.contains(r#"cy="888""#));
        // ...but the inner a:ext (the graphic frame's own box) is untouched.
        assert!(out.contains(r#"cx="999""#));
    }

    #[test]
    fn clear_descr_removes_the_attribute() {
        let mut el = parse(&drawing_fragment(1, 1, Some("old alt")));
        let doc_pr = find_descendant_by_local_mut(&mut el, "docPr").unwrap();
        remove_bare_attr(doc_pr, "descr");
        let out = String::from_utf8(serialize_raw_fragment(&el)).unwrap();
        assert!(!out.contains("descr="));
        assert!(!out.contains("old alt"));
    }

    #[test]
    fn set_descr_writes_the_attribute() {
        let mut el = parse(&drawing_fragment(1, 1, None));
        let doc_pr = find_descendant_by_local_mut(&mut el, "docPr").unwrap();
        crate::xml_attrs::attr_set(doc_pr, "descr", "new alt");
        let out = String::from_utf8(serialize_raw_fragment(&el)).unwrap();
        assert!(out.contains(r#"descr="new alt""#));
    }

    #[test]
    fn missing_target_is_detectable() {
        // A drawing with no wp:extent — the verb must be able to detect absence.
        let raw = br#"<w:drawing xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"><wp:inline><wp:docPr id="1" name="P"/></wp:inline></w:drawing>"#;
        let mut el = parse(raw);
        assert!(find_descendant_by_local_mut(&mut el, "extent").is_none());
        assert!(find_descendant_by_local_mut(&mut el, "docPr").is_some());
    }
}
