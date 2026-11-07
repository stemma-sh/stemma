//! Bounded proof for the direct `SetImageAttributes` operation.
//!
//! Only wp:extent cx/cy, wp:docPr descr, and the framed object's own size
//! (its top-level `a:xfrm/a:ext`, when it is exactly the old size scaled by
//! the frame's change, as the resize writes it) are accounted for. The
//! remaining drawing XML and every other paragraph field must still compare
//! equal.
//! This is not a blanket exemption for an image-bearing paragraph.

use crate::domain::{BlockNode, InlineNode, OpaqueKind, TrackedBlock};
use crate::runtime::{RuntimeError, invalid_docx};
use crate::word_xml::{parse_raw_fragment, serialize_raw_fragment};
use crate::xml_attrs::attr_get;
use xmltree::{Element, XMLNode};

const WP: &str = "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing";

#[derive(PartialEq, Eq)]
enum ImageAttribute {
    Extent {
        cx: u64,
        cy: u64,
    },
    Description(Option<String>),
    /// The framed object's own size (`a:xfrm/a:ext`).
    Frame {
        cx: Option<i64>,
        cy: Option<i64>,
    },
}

pub(crate) fn only_image_attributes_changed(
    before: &TrackedBlock,
    after: &TrackedBlock,
) -> Result<bool, RuntimeError> {
    // The authoring operation addresses a body paragraph. Other block
    // families keep their existing audit behavior, with no exemption here.
    if !matches!(
        (&before.block, &after.block),
        (BlockNode::Paragraph(_), BlockNode::Paragraph(_))
    ) {
        return Ok(false);
    }
    if crate::roundtrip_compare::compare_tracked_block_pair(before, after).is_empty() {
        return Ok(false);
    }
    let mut old = before.clone();
    let mut new = after.clone();
    let old_attributes = remove_accounted_attributes(&mut old)?;
    let new_attributes = remove_accounted_attributes(&mut new)?;
    Ok(old_attributes != new_attributes
        && frames_follow_their_extents(&old_attributes, &new_attributes)
        && crate::roundtrip_compare::compare_tracked_block_pair(&old, &new).is_empty())
}

/// Every framed object's new size is its old size scaled by its drawing's
/// extent change (each Frame follows its drawing's Extent in the list).
fn frames_follow_their_extents(old: &[ImageAttribute], new: &[ImageAttribute]) -> bool {
    use crate::edit::verbs::images::scale_frame_dimension as scale;
    if old.len() != new.len() {
        return false;
    }
    let mut extents = None;
    old.iter().zip(new).all(|pair| match pair {
        (
            ImageAttribute::Extent { cx: ocx, cy: ocy },
            ImageAttribute::Extent { cx: ncx, cy: ncy },
        ) => {
            extents = Some(((*ocx as i64, *ocy as i64), (*ncx as i64, *ncy as i64)));
            true
        }
        (
            ImageAttribute::Frame { cx: ocx, cy: ocy },
            ImageAttribute::Frame { cx: ncx, cy: ncy },
        ) => match extents {
            Some(((old_cx, old_cy), (new_cx, new_cy))) if (old_cx, old_cy) != (new_cx, new_cy) => {
                *ncx == Some(scale(*ocx, Some(old_cx), new_cx))
                    && *ncy == Some(scale(*ocy, Some(old_cy), new_cy))
            }
            _ => ocx == ncx && ocy == ncy,
        },
        (ImageAttribute::Description(_), ImageAttribute::Description(_)) => true,
        _ => false,
    })
}

fn remove_accounted_attributes(
    block: &mut TrackedBlock,
) -> Result<Vec<ImageAttribute>, RuntimeError> {
    let BlockNode::Paragraph(paragraph) = &mut block.block else {
        unreachable!("image accounting is paragraph-scoped")
    };
    let mut attributes = Vec::new();
    for inline in paragraph
        .segments
        .iter_mut()
        .flat_map(|segment| &mut segment.inlines)
    {
        let InlineNode::OpaqueInline(drawing) = inline else {
            continue;
        };
        if !matches!(drawing.kind, OpaqueKind::Drawing) {
            continue;
        }
        let context = format!(
            "image audit, paragraph {}, drawing {}",
            paragraph.id, drawing.id
        );
        let raw = drawing.raw_xml.as_deref().ok_or_else(|| {
            invalid_docx(&format!(
                "{context}: missing drawing XML; cannot prove an attribute-only change"
            ))
        })?;
        let mut root = parse_raw_fragment(raw)
            .map_err(|error| invalid_docx(&format!("{context}: malformed drawing XML: {error}")))?;
        remove_from_tree(&mut root, &mut attributes, &context)?;
        if let Some(data) =
            crate::edit::verbs::images::find_descendant_by_local_mut(&mut root, "graphicData")
        {
            for ext in crate::edit::verbs::images::graphic_frame_extents(data) {
                let read = |name| attr_get(ext, name).and_then(|v| v.parse::<i64>().ok());
                attributes.push(ImageAttribute::Frame {
                    cx: read("cx"),
                    cy: read("cy"),
                });
                ext.attributes.retain(|name, _| {
                    name.namespace.is_some() || !matches!(name.local_name.as_str(), "cx" | "cy")
                });
            }
        }
        let residual = serialize_raw_fragment(&root);
        drawing.content_hash = Some(crate::import::canonical_raw_hash(&residual));
        drawing.raw_xml = Some(residual);
    }
    Ok(attributes)
}

fn remove_from_tree(
    element: &mut Element,
    attributes: &mut Vec<ImageAttribute>,
    context: &str,
) -> Result<(), RuntimeError> {
    if element.namespace.as_deref() == Some(WP) {
        match element.name.as_str() {
            "extent" => {
                let coordinate = |name| -> Result<u64, RuntimeError> {
                    let value = attr_get(element, name).ok_or_else(|| {
                        invalid_docx(&format!("{context}: wp:extent missing {name}"))
                    })?;
                    value.parse().map_err(|error| {
                        invalid_docx(&format!(
                            "{context}: invalid wp:extent {name}={value:?}: {error}"
                        ))
                    })
                };
                attributes.push(ImageAttribute::Extent {
                    cx: coordinate("cx")?,
                    cy: coordinate("cy")?,
                });
                element.attributes.retain(|name, _| {
                    name.namespace.is_some() || !matches!(name.local_name.as_str(), "cx" | "cy")
                });
            }
            "docPr" => {
                attributes.push(ImageAttribute::Description(
                    attr_get(element, "descr").cloned(),
                ));
                element
                    .attributes
                    .retain(|name, _| name.namespace.is_some() || name.local_name != "descr");
            }
            _ => {}
        }
    }
    for child in &mut element.children {
        if let XMLNode::Element(child) = child {
            remove_from_tree(child, attributes, context)?;
        }
    }
    Ok(())
}
