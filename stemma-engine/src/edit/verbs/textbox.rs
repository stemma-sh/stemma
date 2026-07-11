//! `SetTextboxText` — whole-interior replace of a textbox's `w:txbxContent`.
//!
//! ## Model
//!
//! A textbox imports as a single opaque `OpaqueInline{Drawing}` whose entire
//! serialized `w:drawing` — including `w:txbxContent` and every paragraph inside
//! it — is frozen in `raw_xml` (`import.rs`; the interior never reaches the atom
//! layer). This v1 verb replaces the WHOLE interior with caller-supplied
//! paragraphs, mutating the opaque's `raw_xml` in place (the `SetImageAttributes`
//! pattern: `parse_raw_fragment` → swap the `txbxContent` children →
//! `serialize_raw_fragment` → recompute `content_hash`).
//!
//! Carrier-agnostic: `w:txbxContent` is located by **local name**, so both the
//! modern DrawingML carrier (`wps:txbx > w:txbxContent`) and the legacy VML
//! carrier (`v:textbox > w:txbxContent`) are handled by the same code.
//!
//! ## Untracked
//!
//! Direct/in-place, like `SetImageAttributes` / `ReplaceImage`: OOXML has no
//! tracked-change envelope for "rewrite a textbox's contents", and a
//! whole-interior replace is not a redline-shaped operation.
//!
//! ## Fail loud (CLAUDE.md "no silent fallbacks")
//!
//! - id not a drawing            → `NotADrawing` / `DrawingNotFound`
//! - drawing has no `raw_xml`    → `DrawingMissingRawXml`
//! - raw_xml fails to parse      → `DrawingRawXmlParse`
//! - drawing has no txbxContent  → `ImageAttributeTargetAbsent("w:txbxContent")`
//! - txbxContent ALREADY carries `w:ins`/`w:del` → `TextboxHasTrackedChanges`
//!   (do NOT silently flatten existing redlines — the agent's path is
//!   "resolve first (M0 accept/reject), then set")

use super::super::{EditError, find_block_index};
use super::images::locate_drawing_mut;
use crate::domain::{CanonDoc, NodeId};
use crate::import::sha256_hex;
use crate::word_xml::{parse_raw_fragment, serialize_raw_fragment, w_el};
use xmltree::{Element, XMLNode};

/// Apply a `SetTextboxText` step: replace the located drawing's `w:txbxContent`
/// children with one `w:p` per entry in `paragraphs`.
pub(crate) fn apply_set_text(
    doc: &mut CanonDoc,
    block_id: &NodeId,
    drawing_id: &NodeId,
    paragraphs: &[String],
    semantic_hash: Option<&str>,
    step_index: usize,
) -> Result<(), EditError> {
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

    // Collect ALL txbxContent copies. Word's standard textbox emission wraps the
    // shape in `mc:AlternateContent` with a DrawingML Choice (wps:txbx) AND a VML
    // Fallback (v:textbox), each carrying a DUPLICATE of the same interior — so a
    // single textbox commonly appears as TWO txbxContent. Replacing only the
    // first would leave the fallback copy stale (a consumer taking the fallback
    // branch sees the OLD text).
    //
    // Shared read primitive (`opaque_meta::collect_descendants_by_local`, one
    // helper / two callers — the M3-read interior-text projection locates the
    // same copies): its no-recurse-into-a-found-match rule is the load-bearing
    // invariant here — a `txbxContent`'s own paragraphs may host a NESTED drawing
    // with its own textbox, which belongs to that nested anchor, not this one, so
    // it must NOT be miscounted as a copy of ours.
    let mut copies = Vec::new();
    crate::opaque_meta::collect_descendants_by_local(&element, "txbxContent", &mut copies);
    if copies.is_empty() {
        return Err(EditError::ImageAttributeTargetAbsent {
            drawing_id: drawing_id.clone(),
            attribute: "w:txbxContent",
            step_index,
        });
    }

    // Refuse if ANY copy already carries tracked changes — a whole-interior
    // replace would silently flatten them (the M0 accept/reject descent is the
    // agent's "resolve first" path). Don't flatten redlines (the A-2 lesson).
    if copies.iter().any(|c| contains_revision(c)) {
        return Err(EditError::TextboxHasTrackedChanges {
            drawing_id: drawing_id.clone(),
            step_index,
        });
    }

    // If there are several copies, they must be IDENTICAL (the AlternateContent
    // Choice/Fallback duplicate of ONE textbox). Several DISTINCT interiors is a
    // true multi-textbox group shape: replacing them all with one text would be
    // wrong, and silently picking one is the fallback we kill — refuse.
    if copies.len() > 1 {
        let first = serialize_raw_fragment(copies[0]);
        if copies
            .iter()
            .skip(1)
            .any(|c| serialize_raw_fragment(c) != first)
        {
            return Err(EditError::MultipleDistinctTextboxes {
                drawing_id: drawing_id.clone(),
                count: copies.len(),
                step_index,
            });
        }
    }

    // Build the new interior (CT_TxbxContent requires a block-level child, so an
    // empty `paragraphs` yields one empty paragraph) and apply it to EVERY copy.
    let new_children: Vec<XMLNode> = if paragraphs.is_empty() {
        vec![XMLNode::Element(build_paragraph(""))]
    } else {
        paragraphs
            .iter()
            .map(|p| XMLNode::Element(build_paragraph(p)))
            .collect()
    };
    replace_all_txbxcontent(&mut element, &new_children);

    let new_raw = serialize_raw_fragment(&element);
    node.content_hash = Some(sha256_hex(&new_raw));
    node.raw_xml = Some(new_raw);
    Ok(())
}

/// Replace the children of EVERY `w:txbxContent` descendant with a clone of
/// `new_children` (so identical copies stay byte-identical after the edit).
fn replace_all_txbxcontent(root: &mut Element, new_children: &[XMLNode]) {
    if root.name == "txbxContent" {
        root.children = new_children.to_vec();
        // Do NOT recurse into the children we just replaced (matching the
        // no-recurse rule of `collect_descendants_by_local`): a nested drawing's
        // textbox belonged to that nested anchor and is gone now that we replaced
        // this interior wholesale — exactly the whole-interior-replace semantic.
        return;
    }
    for child in &mut root.children {
        if let XMLNode::Element(c) = child {
            replace_all_txbxcontent(c, new_children);
        }
    }
}

/// Build `<w:p><w:r><w:t xml:space="preserve">text</w:t></w:r></w:p>`. An empty
/// `text` yields a bare `<w:p/>` (a valid empty paragraph).
fn build_paragraph(text: &str) -> Element {
    let mut p = w_el("p");
    if !text.is_empty() {
        let mut t = w_el("t");
        crate::xml_attrs::attr_set(&mut t, "xml:space", "preserve");
        t.children.push(XMLNode::Text(text.to_string()));
        let mut r = w_el("r");
        r.children.push(XMLNode::Element(t));
        p.children.push(XMLNode::Element(r));
    }
    p
}

/// Whether `element` (or any descendant) is or contains a tracked-change element
/// (`w:ins` / `w:del` / `w:moveFrom` / `w:moveTo`). Used to refuse a
/// whole-interior replace that would flatten existing redlines.
fn contains_revision(element: &Element) -> bool {
    for child in &element.children {
        if let XMLNode::Element(el) = child {
            if matches!(el.name.as_str(), "ins" | "del" | "moveFrom" | "moveTo") {
                return true;
            }
            if contains_revision(el) {
                return true;
            }
        }
    }
    false
}
