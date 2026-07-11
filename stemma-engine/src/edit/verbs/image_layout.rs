//! `SetImageLayout` — author the *layout* display attributes on an existing
//! opaque drawing (`w:drawing`): its **crop** (`a:srcRect` l/t/r/b, §20.1.8.55),
//! its floating **position** (`wp:positionH`/`wp:positionV` offset or alignment,
//! §20.4.2.10/§20.4.2.11), and its **text-wrap type** (`wp:wrapNone` /
//! `wp:wrapSquare` / `wp:wrapTight` / `wp:wrapThrough` / `wp:wrapTopAndBottom`,
//! §20.4.2.15–20.4.2.19).
//!
//! Sibling to [`crate::edit::verbs::images`] (`SetImageAttributes`, which does
//! resize + alt-text). Both mutate the drawing's display tree inside `raw_xml`
//! and never touch the binary media part. This verb is likewise a **direct,
//! untracked** attribute edit: OOXML has no tracked-change envelope for
//! opaque-drawing display attributes (there is no `w:drawingChange`), so both
//! materialization modes behave identically and reversibility is at the
//! transaction-rejection level, not segment accept/reject.
//!
//! ## What is reachable, and what is honestly out of scope
//!
//! The drawing envelope is one of two shapes (§20.4.2.8 / §20.4.2.20):
//!
//! - `wp:inline` — flows in the text line. Has **no** position and **no** wrap
//!   element (being inline *is* its wrap mode). It still has a `pic:blipFill`,
//!   so **crop is reachable** on an inline drawing.
//! - `wp:anchor` — floats. Carries `wp:positionH`/`wp:positionV` and exactly one
//!   wrap element. So **position and wrap-type are reachable only here.**
//!
//! Position and wrap therefore require an *already-anchored* drawing; we fail
//! loud (`ImageLayoutRequiresAnchor`) on an inline drawing rather than silently
//! skip. Converting `wp:inline` ⇄ `wp:anchor` (synthesizing a full anchor
//! envelope: `@simplePos`/`@behindDoc`/`@relativeHeight`/`wp:simplePos`/
//! `wp:effectExtent`/default position+wrap, with the strict CT_Anchor child
//! order) is a large structural transform, not an attribute edit — it is
//! **deliberately out of scope** here (recorded as the
//! honesty-escaped sub-property). Crop, plus position/wrap on a drawing that is
//! already floating, are the reachable surface and are implemented in full.
//!
//! ## Fail loud (CLAUDE.md "no silent fallbacks")
//!
//! - missing / non-drawing / raw-xml-less drawing → reuses `images`' errors
//!   (`DrawingNotFound` / `NotADrawing` / `DrawingMissingRawXml`).
//! - position or wrap requested on an inline drawing → `ImageLayoutRequiresAnchor`.
//! - the `pic:blipFill` a crop needs is absent → `ImageLayoutTargetAbsent`.
//! - empty request (nothing set) → `NoImageLayoutRequested`.

use super::super::{EditError, find_block_index};
use super::images::locate_drawing_mut;
use crate::domain::{NodeId, OpaqueInlineNode};
use crate::import::sha256_hex;
use crate::word_xml::{parse_raw_fragment, serialize_raw_fragment};
use xmltree::{Element, XMLNode};

const A_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const WP_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing";

/// One axis of a floating drawing's position (`wp:positionH` or `wp:positionV`).
/// Exactly one of offset/alignment is authored; the wire edge rejects both-set
/// and neither-set so this domain type is never ambiguous.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImagePositionAxis {
    /// `<wp:posOffset>` — an absolute EMU offset from `relative_from`.
    Offset {
        /// The `@relativeFrom` frame token (e.g. `"column"`, `"page"`,
        /// `"margin"`). Validated against the per-axis vocabulary at the edge.
        relative_from: String,
        /// Offset in EMUs. May be negative (a drawing can sit left of its frame).
        offset_emu: i64,
    },
    /// `<wp:align>` — a relative alignment keyword (e.g. `"left"`, `"center"`).
    Align {
        /// The `@relativeFrom` frame token.
        relative_from: String,
        /// The alignment keyword. Validated against the per-axis vocabulary at
        /// the edge.
        align: String,
    },
}

/// The text-wrap *type* of a floating drawing — exactly one wrap element is
/// present on a `wp:anchor` (§20.4.2.15–20.4.2.19). `Inline` is intentionally
/// **not** a variant: that would be the inline⇄anchor structural conversion,
/// which is out of scope (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageWrapType {
    /// `<wp:wrapNone/>` — text does not wrap; the drawing floats over/under it.
    None,
    /// `<wp:wrapSquare wrapText="bothSides"/>` — text wraps a bounding box.
    Square,
    /// `<wp:wrapTight wrapText="bothSides">` — text wraps the wrap polygon.
    Tight,
    /// `<wp:wrapThrough wrapText="bothSides">` — text flows through the polygon.
    Through,
    /// `<wp:wrapTopAndBottom/>` — text wraps above and below only.
    TopAndBottom,
}

impl ImageWrapType {
    fn local_name(self) -> &'static str {
        match self {
            ImageWrapType::None => "wrapNone",
            ImageWrapType::Square => "wrapSquare",
            ImageWrapType::Tight => "wrapTight",
            ImageWrapType::Through => "wrapThrough",
            ImageWrapType::TopAndBottom => "wrapTopAndBottom",
        }
    }
    /// All five wrap element local names — used to delete any pre-existing wrap
    /// before inserting the requested one (exactly-one invariant).
    const ALL_LOCAL: [&'static str; 5] = [
        "wrapNone",
        "wrapSquare",
        "wrapTight",
        "wrapThrough",
        "wrapTopAndBottom",
    ];
    /// Whether this wrap element carries a `wrapText="bothSides"` attribute.
    /// `wrapNone` and `wrapTopAndBottom` do not take `@wrapText`.
    fn takes_wrap_text(self) -> bool {
        matches!(
            self,
            ImageWrapType::Square | ImageWrapType::Tight | ImageWrapType::Through
        )
    }
}

/// A cropping rectangle (`a:srcRect`, §20.1.8.55). Each edge is an inset given
/// in 1000ths of a percent of the source image (`ST_Percentage` integer form):
/// `50000` == crop 50% off that edge. Each is `Option` so a caller can adjust
/// one edge without disturbing the others; absent edges keep their current value
/// (Word treats an absent `srcRect` edge as 0).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImageCrop {
    pub left: Option<i32>,
    pub top: Option<i32>,
    pub right: Option<i32>,
    pub bottom: Option<i32>,
}

impl ImageCrop {
    fn is_empty(&self) -> bool {
        self.left.is_none() && self.top.is_none() && self.right.is_none() && self.bottom.is_none()
    }
}

/// The full layout request. Each field is `Option`; at least one must be present
/// (the wire edge + [`apply`] both reject the empty request).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImageLayoutPatch {
    /// Horizontal position (`wp:positionH`). Anchor-only.
    pub position_h: Option<ImagePositionAxis>,
    /// Vertical position (`wp:positionV`). Anchor-only.
    pub position_v: Option<ImagePositionAxis>,
    /// Text-wrap type. Anchor-only.
    pub wrap: Option<ImageWrapType>,
    /// Crop rectangle. Reachable on inline and anchor drawings alike.
    pub crop: Option<ImageCrop>,
}

impl ImageLayoutPatch {
    pub fn is_empty(&self) -> bool {
        self.position_h.is_none()
            && self.position_v.is_none()
            && self.wrap.is_none()
            && self.crop.as_ref().is_none_or(ImageCrop::is_empty)
    }
    /// Whether any anchor-only property (position/wrap) is requested.
    fn needs_anchor(&self) -> bool {
        self.position_h.is_some() || self.position_v.is_some() || self.wrap.is_some()
    }
}

/// Apply a `SetImageLayout` step: locate the drawing, optionally check
/// `semantic_hash`, mutate the requested layout properties inside `raw_xml`,
/// re-serialize, and recompute `content_hash`.
pub(crate) fn apply(
    doc: &mut crate::domain::CanonDoc,
    block_id: &NodeId,
    drawing_id: &NodeId,
    semantic_hash: Option<&str>,
    patch: &ImageLayoutPatch,
    step_index: usize,
) -> Result<(), EditError> {
    if patch.is_empty() {
        return Err(EditError::NoImageLayoutRequested { step_index });
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

    apply_to_element(&mut element, drawing_id, patch, step_index)?;

    let new_raw = serialize_raw_fragment(&element);
    set_raw(node, new_raw);
    Ok(())
}

fn set_raw(node: &mut OpaqueInlineNode, new_raw: Vec<u8>) {
    node.content_hash = Some(sha256_hex(&new_raw));
    node.raw_xml = Some(new_raw);
}

/// Pure mutation on a parsed drawing fragment — the testable core.
fn apply_to_element(
    element: &mut Element,
    drawing_id: &NodeId,
    patch: &ImageLayoutPatch,
    step_index: usize,
) -> Result<(), EditError> {
    // Crop is reachable on both envelopes; do it first so the anchor-gate below
    // does not block a crop-only edit on an inline drawing.
    if let Some(crop) = patch.crop
        && !crop.is_empty()
    {
        apply_crop(element, drawing_id, crop, step_index)?;
    }

    if patch.needs_anchor() {
        let anchor = find_descendant_by_local_mut(element, "anchor").ok_or_else(|| {
            EditError::ImageLayoutRequiresAnchor {
                drawing_id: drawing_id.clone(),
                step_index,
            }
        })?;
        if let Some(h) = &patch.position_h {
            set_position(anchor, "positionH", h);
        }
        if let Some(v) = &patch.position_v {
            set_position(anchor, "positionV", v);
        }
        if let Some(w) = patch.wrap {
            set_wrap(anchor, w);
        }
        reorder_anchor_children(anchor);
    }
    Ok(())
}

/// Set the crop rectangle: locate (or create) the `a:srcRect` inside the
/// drawing's `pic:blipFill` (or `a:blipFill`) and set the requested edge insets.
/// Absent edges on the patch keep whatever the existing `srcRect` carried.
fn apply_crop(
    element: &mut Element,
    drawing_id: &NodeId,
    crop: ImageCrop,
    step_index: usize,
) -> Result<(), EditError> {
    let blip_fill = find_descendant_by_local_mut(element, "blipFill").ok_or_else(|| {
        EditError::ImageLayoutTargetAbsent {
            drawing_id: drawing_id.clone(),
            target: "blipFill",
            step_index,
        }
    })?;

    // Find or create the a:srcRect child.
    let has_src_rect = blip_fill
        .children
        .iter()
        .any(|c| matches!(c, XMLNode::Element(el) if el.name == "srcRect"));
    if !has_src_rect {
        // Insert directly after a:blip if present, else at the front — srcRect
        // precedes the fill mode (a:stretch/a:tile) in CT_BlipFillProperties.
        let pos = blip_fill
            .children
            .iter()
            .position(|c| matches!(c, XMLNode::Element(el) if el.name == "blip"))
            .map(|p| p + 1)
            .unwrap_or(0);
        blip_fill
            .children
            .insert(pos, XMLNode::Element(a_el("srcRect")));
    }
    let src_rect = blip_fill
        .children
        .iter_mut()
        .find_map(|c| match c {
            XMLNode::Element(el) if el.name == "srcRect" => Some(el),
            _ => None,
        })
        .expect("srcRect present after the insert above");

    // l/t/r/b are bare (un-prefixed) attributes per the DrawingML schema.
    if let Some(l) = crop.left {
        crate::xml_attrs::attr_set(src_rect, "l", l.to_string());
    }
    if let Some(t) = crop.top {
        crate::xml_attrs::attr_set(src_rect, "t", t.to_string());
    }
    if let Some(r) = crop.right {
        crate::xml_attrs::attr_set(src_rect, "r", r.to_string());
    }
    if let Some(b) = crop.bottom {
        crate::xml_attrs::attr_set(src_rect, "b", b.to_string());
    }
    Ok(())
}

/// Replace (or create) the `wp:positionH`/`wp:positionV` child of the anchor with
/// the requested offset/alignment. There is exactly one of each on an anchor.
fn set_position(anchor: &mut Element, local: &str, axis: &ImagePositionAxis) {
    // Drop any existing position element for this axis.
    anchor
        .children
        .retain(|c| !matches!(c, XMLNode::Element(el) if el.name == local));

    let mut pos = wp_el(local);
    let inner = match axis {
        ImagePositionAxis::Offset {
            relative_from,
            offset_emu,
        } => {
            crate::xml_attrs::attr_set(&mut pos, "relativeFrom", relative_from);
            let mut off = wp_el("posOffset");
            off.children.push(XMLNode::Text(offset_emu.to_string()));
            off
        }
        ImagePositionAxis::Align {
            relative_from,
            align,
        } => {
            crate::xml_attrs::attr_set(&mut pos, "relativeFrom", relative_from);
            let mut al = wp_el("align");
            al.children.push(XMLNode::Text(align.clone()));
            al
        }
    };
    pos.children.push(XMLNode::Element(inner));
    anchor.children.push(XMLNode::Element(pos));
}

/// Replace whatever wrap element the anchor has with the requested one
/// (exactly-one invariant). `reorder_anchor_children` then places it correctly.
fn set_wrap(anchor: &mut Element, wrap: ImageWrapType) {
    anchor.children.retain(|c| {
        !matches!(c, XMLNode::Element(el) if ImageWrapType::ALL_LOCAL.contains(&el.name.as_str()))
    });
    let mut el = wp_el(wrap.local_name());
    if wrap.takes_wrap_text() {
        crate::xml_attrs::attr_set(&mut el, "wrapText", "bothSides");
    }
    anchor.children.push(XMLNode::Element(el));
}

/// Re-order the anchor's *direct element children* into the CT_Anchor sequence
/// (§20.4.2.3): simplePos, positionH, positionV, extent, effectExtent, <wrap>,
/// docPr, cNvGraphicFramePr, graphic. Children we just appended go to the tail;
/// this restores the schema order without disturbing unknown/extension elements
/// (they sort to the end, after `graphic`, stably). Word rejects out-of-order
/// anchor children with a repair prompt, so this is load-bearing.
fn reorder_anchor_children(anchor: &mut Element) {
    fn rank(name: &str) -> usize {
        match name {
            "simplePos" => 0,
            "positionH" => 1,
            "positionV" => 2,
            "extent" => 3,
            "effectExtent" => 4,
            "wrapNone" | "wrapSquare" | "wrapTight" | "wrapThrough" | "wrapTopAndBottom" => 5,
            "docPr" => 6,
            "cNvGraphicFramePr" => 7,
            "graphic" => 8,
            _ => 9, // unknown / extensions: keep at the tail, stably
        }
    }
    // Stable sort over element children only; non-element nodes (whitespace
    // text) on an anchor are not expected, but if present they are left in place
    // relative to elements by sorting the whole child vector with text ranked 9.
    anchor.children.sort_by_key(|c| match c {
        XMLNode::Element(el) => rank(&el.name),
        _ => 9,
    });
}

/// Build a DrawingML-main (`a:`) element with the right prefix + namespace so it
/// round-trips through `serialize_raw_fragment` (which re-declares used prefixes).
fn a_el(local: &str) -> Element {
    let mut el = Element::new(local);
    el.prefix = Some("a".to_string());
    el.namespace = Some(A_NS.to_string());
    el
}

/// Build a wordprocessingDrawing (`wp:`) element.
fn wp_el(local: &str) -> Element {
    let mut el = Element::new(local);
    el.prefix = Some("wp".to_string());
    el.namespace = Some(WP_NS.to_string());
    el
}

/// Depth-first search for the first descendant element (or `root` itself) whose
/// local name equals `local`, ignoring namespace prefix.
fn find_descendant_by_local_mut<'a>(root: &'a mut Element, local: &str) -> Option<&'a mut Element> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn inline_fragment() -> Element {
        let raw = br#"<w:drawing xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><wp:inline><wp:extent cx="100" cy="200"/><wp:docPr id="1" name="P"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic><pic:blipFill><a:blip r:embed="rId1"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr/></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing>"#;
        parse_raw_fragment(raw).expect("parse inline")
    }

    fn anchor_fragment() -> Element {
        let raw = br#"<w:drawing xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><wp:anchor distT="0" distB="0" distL="0" distR="0" simplePos="0" relativeHeight="0" behindDoc="0" locked="0" layoutInCell="1" allowOverlap="1"><wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="column"><wp:posOffset>0</wp:posOffset></wp:positionH><wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV><wp:extent cx="100" cy="200"/><wp:effectExtent l="0" t="0" r="0" b="0"/><wp:wrapNone/><wp:docPr id="1" name="P"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic><pic:blipFill><a:blip r:embed="rId1"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr/></pic:pic></a:graphicData></a:graphic></wp:anchor></w:drawing>"#;
        parse_raw_fragment(raw).expect("parse anchor")
    }

    fn render(el: &Element) -> String {
        String::from_utf8(serialize_raw_fragment(el)).unwrap()
    }

    fn id() -> NodeId {
        NodeId::from("d1")
    }

    #[test]
    fn crop_inserts_src_rect_between_blip_and_stretch_on_inline() {
        let mut el = inline_fragment();
        let patch = ImageLayoutPatch {
            crop: Some(ImageCrop {
                left: Some(10000),
                top: Some(20000),
                right: Some(30000),
                bottom: Some(40000),
            }),
            ..Default::default()
        };
        apply_to_element(&mut el, &id(), &patch, 0).expect("crop on inline");
        let out = render(&el);
        assert!(out.contains(r#"l="10000""#), "{out}");
        assert!(out.contains(r#"t="20000""#));
        assert!(out.contains(r#"r="30000""#));
        assert!(out.contains(r#"b="40000""#));
        // srcRect sits after blip, before stretch (schema order).
        let src = out.find("srcRect").unwrap();
        let blip = out.find("blip").unwrap();
        let stretch = out.find("stretch").unwrap();
        assert!(
            blip < src && src < stretch,
            "srcRect must be between blip and stretch: {out}"
        );
    }

    #[test]
    fn crop_updates_one_edge_keeping_others() {
        let mut el = inline_fragment();
        apply_to_element(
            &mut el,
            &id(),
            &ImageLayoutPatch {
                crop: Some(ImageCrop {
                    left: Some(5000),
                    ..Default::default()
                }),
                ..Default::default()
            },
            0,
        )
        .unwrap();
        // Second edit touches only the top edge; left must survive.
        apply_to_element(
            &mut el,
            &id(),
            &ImageLayoutPatch {
                crop: Some(ImageCrop {
                    top: Some(7000),
                    ..Default::default()
                }),
                ..Default::default()
            },
            0,
        )
        .unwrap();
        let out = render(&el);
        assert!(out.contains(r#"l="5000""#), "left preserved: {out}");
        assert!(out.contains(r#"t="7000""#), "top set: {out}");
    }

    #[test]
    fn position_and_wrap_on_inline_fail_loud() {
        let mut el = inline_fragment();
        let err = apply_to_element(
            &mut el,
            &id(),
            &ImageLayoutPatch {
                wrap: Some(ImageWrapType::Square),
                ..Default::default()
            },
            0,
        )
        .unwrap_err();
        assert!(matches!(err, EditError::ImageLayoutRequiresAnchor { .. }));
    }

    #[test]
    fn wrap_replaces_existing_and_stays_ordered() {
        let mut el = anchor_fragment();
        apply_to_element(
            &mut el,
            &id(),
            &ImageLayoutPatch {
                wrap: Some(ImageWrapType::Square),
                ..Default::default()
            },
            0,
        )
        .unwrap();
        let out = render(&el);
        // Exactly one wrap element, the new one.
        assert!(out.contains("wrapSquare"), "{out}");
        assert!(!out.contains("wrapNone"), "old wrap removed: {out}");
        assert!(out.contains(r#"wrapText="bothSides""#));
        // Order: docPr must come AFTER the wrap element.
        let wrap_pos = out.find("wrapSquare").unwrap();
        let docpr_pos = out.find("docPr").unwrap();
        assert!(wrap_pos < docpr_pos, "wrap must precede docPr: {out}");
    }

    #[test]
    fn position_offset_replaces_existing_axis() {
        let mut el = anchor_fragment();
        apply_to_element(
            &mut el,
            &id(),
            &ImageLayoutPatch {
                position_h: Some(ImagePositionAxis::Offset {
                    relative_from: "page".to_string(),
                    offset_emu: 914400,
                }),
                ..Default::default()
            },
            0,
        )
        .unwrap();
        let out = render(&el);
        assert!(out.contains(r#"relativeFrom="page""#), "{out}");
        assert!(out.contains("914400"), "{out}");
        // Still exactly one positionH (replaced, not duplicated): the column one
        // is gone.
        assert!(
            !out.contains(r#"relativeFrom="column""#),
            "old positionH replaced: {out}"
        );
        // positionH precedes extent.
        assert!(out.find("positionH").unwrap() < out.find("extent").unwrap());
    }

    #[test]
    fn position_align_emits_align_keyword() {
        let mut el = anchor_fragment();
        apply_to_element(
            &mut el,
            &id(),
            &ImageLayoutPatch {
                position_v: Some(ImagePositionAxis::Align {
                    relative_from: "margin".to_string(),
                    align: "center".to_string(),
                }),
                ..Default::default()
            },
            0,
        )
        .unwrap();
        let out = render(&el);
        assert!(out.contains("center"), "{out}");
        assert!(out.contains(r#"relativeFrom="margin""#), "{out}");
    }

    #[test]
    fn empty_patch_is_empty() {
        assert!(ImageLayoutPatch::default().is_empty());
        assert!(
            ImageLayoutPatch {
                crop: Some(ImageCrop::default()),
                ..Default::default()
            }
            .is_empty()
        );
        assert!(
            !ImageLayoutPatch {
                wrap: Some(ImageWrapType::None),
                ..Default::default()
            }
            .is_empty()
        );
    }
}
