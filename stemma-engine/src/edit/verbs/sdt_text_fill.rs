//! `sdt_text_fill` — set a content control's text value, tracked. RFC-0002
//! §Phase-2. The forms-natural operation ("set this field's value"): a
//! whole-value replace of the control's text, shown as a redline (old struck
//! through, new inserted) or applied directly.
//!
//! Three targets, one semantic:
//! INLINE control (`OpaqueInline{Sdt}`, addressed by host paragraph + sdt id):
//! its `raw_xml` is spliced in place with the shared
//! [`crate::opaque_splice::set_region_text`] core.
//!
//! OPEN BLOCK control (a `BlockSdtWrap` over ordinary blocks, addressed by its
//! first block id alone): lowered before the edit loop to the text replace of
//! its value paragraph ([`lower_open_block_fills`]).
//!
//! SEALED BLOCK (body-level) control (`OpaqueBlock{Sdt}`, addressed by the frozen
//! `body_index` discovery surfaced): its bytes live in the serialize scaffold,
//! unreachable from the pure edit core — so the fill is validated and its
//! tracked-change ids minted here, then STAGED into `PendingParts` for the save
//! path to apply against the scaffold node.
//!
//! Fail loud (CLAUDE.md "no silent fallbacks"): neither/both targets ⇒
//! `SdtFillAmbiguousTarget`; missing inline sdt / block ⇒ `OpaqueTextTargetNotFound`
//! / `SdtFillBlockNotFound`; an empty fill of an empty control ⇒ `SdtFillEmpty`.

use super::super::{EditError, EditStep};
use super::super::{block_at_mut, block_id_of, find_paragraph_path};
use crate::domain::{
    BlockNode, CanonDoc, InlineNode, NodeId, OpaqueKind, RevisionInfo, TrackingStatus,
};
use crate::edit::pending_parts::{OpaqueChildTextSet, PendingParts};
use crate::opaque_splice::{SpliceError, ValueRpr, first_descendant_mut, set_region_text};
use crate::word_xml::{is_w_tag, parse_raw_fragment, serialize_raw_fragment};
use xmltree::{Element, XMLNode};

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
    let value_rpr =
        leave_placeholder(&mut element).map_err(|reason| EditError::OpaqueTextRawXmlParse {
            opaque_id: sdt_id.clone(),
            reason,
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
    set_region_text(content, value, &value_rpr, base, rev_counter, tracked).map_err(
        |e| match e {
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
        },
    )?;

    let new_raw = serialize_raw_fragment(&element);
    node.content_hash = Some(crate::import::canonical_raw_hash(&new_raw));
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
        || pending
            .block_form_text_sets
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

/// Lower every `SdtTextFill` that names a block content control by its first
/// block (`block_id` alone) to the text replace it is.
///
/// A body control whose content is ordinary blocks imports OPEN (a
/// `BlockSdtWrap` over the blocks, the same model
/// `wrap_blocks_content_control` authors), so its value is model content, not
/// scaffold bytes: the fill sets the whole text of the control's first
/// paragraph with text, tracked or direct like any replace. A `semantic_hash`
/// is that paragraph's guard (the block hash the read view shows). The value
/// paragraph must hold only plain text runs, with no pending revision; no
/// table or preserved block may come before it, and no content after it (a
/// control with more is a passage, not a field); anything else is refused
/// rather than relocated. A filled control that showed its placeholder no
/// longer does ([`clear_placeholder_flags`]). A sealed control
/// (`OpaqueBlock{Sdt}`) is still addressed by `body_index` and staged for the
/// save path.
pub(crate) fn lower_open_block_fills(
    doc: &CanonDoc,
    steps: &[EditStep],
) -> Result<Vec<EditStep>, EditError> {
    steps
        .iter()
        .enumerate()
        .map(|(step_index, step)| match step {
            EditStep::SdtTextFill {
                block_id: Some(block_id),
                sdt_id: None,
                body_index: None,
                value,
                semantic_hash,
                rationale,
            } => lower_open_block_fill(
                doc,
                block_id,
                value,
                semantic_hash.as_deref(),
                rationale,
                step_index,
            ),
            other => Ok(other.clone()),
        })
        .collect()
}

pub(crate) fn lower_open_block_fill(
    doc: &CanonDoc,
    block_id: &NodeId,
    value: &str,
    semantic_hash: Option<&str>,
    rationale: &Option<String>,
    step_index: usize,
) -> Result<EditStep, EditError> {
    let not_fillable = |reason: &str| EditError::SdtFillControlNotFillable {
        block_id: block_id.clone(),
        reason: reason.to_string(),
        step_index,
    };
    let (start, wrap) = doc
        .blocks
        .iter()
        .enumerate()
        .find_map(|(index, tracked)| {
            let wrap = tracked.block_sdt_wrap.as_ref()?;
            (block_id_of(&tracked.block) == block_id).then_some((index, wrap))
        })
        .ok_or_else(|| EditError::SdtFillControlNotFound {
            block_id: block_id.clone(),
            step_index,
        })?;
    let mut value_paragraph = None;
    for tracked in &doc.blocks[start..start + wrap.span] {
        match &tracked.block {
            BlockNode::Paragraph(paragraph) => {
                let has_text = paragraph.all_inlines().any(
                    |inline| matches!(inline, InlineNode::Text(text) if !text.text.is_empty()),
                );
                if has_text {
                    value_paragraph = Some((tracked, paragraph));
                    break;
                }
            }
            BlockNode::Table(_) | BlockNode::OpaqueBlock(_) => {
                return Err(not_fillable(
                    "a table or preserved block comes before its first paragraph with text",
                ));
            }
        }
    }
    let (tracked, paragraph) =
        value_paragraph.ok_or_else(|| not_fillable("no paragraph in the control has text"))?;
    // One value: a control with text in more than one block is a passage,
    // not a field, and setting its first paragraph would keep the rest.
    let value_index = start
        + doc.blocks[start..start + wrap.span]
            .iter()
            .position(|other| block_id_of(&other.block) == &paragraph.id)
            .expect("the value paragraph is in the control");
    let more_content = doc.blocks[value_index + 1..start + wrap.span]
        .iter()
        .any(|other| match &other.block {
            BlockNode::Paragraph(other) => other
                .all_inlines()
                .any(|inline| matches!(inline, InlineNode::Text(text) if !text.text.is_empty())),
            BlockNode::Table(_) | BlockNode::OpaqueBlock(_) => true,
        });
    if more_content {
        return Err(not_fillable(
            "it holds content after its first paragraph with text; edit its paragraphs directly",
        ));
    }
    if tracked.status != TrackingStatus::Normal
        || paragraph
            .segments
            .iter()
            .any(|segment| segment.status != TrackingStatus::Normal)
    {
        return Err(not_fillable(
            "its value paragraph has pending tracked changes",
        ));
    }
    // The value is the paragraph's text runs, contiguous; zero-width markers
    // (bookmarks, proofing marks) may sit before or after them.
    let inlines = crate::edit::flat_inlines(paragraph);
    let is_text = |inline: &&InlineNode| matches!(inline, InlineNode::Text(_));
    let first = inlines
        .iter()
        .position(is_text)
        .expect("the paragraph has text");
    let end = inlines
        .iter()
        .rposition(is_text)
        .expect("the paragraph has text")
        + 1;
    if !inlines[first..end].iter().all(is_text)
        || inlines[..first]
            .iter()
            .chain(&inlines[end..])
            .any(|inline| !matches!(inline, InlineNode::Decoration(_)))
    {
        return Err(not_fillable(
            "its value paragraph holds a hyperlink, field or other object; edit it directly",
        ));
    }
    if value.is_empty() {
        return Err(EditError::SdtFillEmpty { step_index });
    }
    let current: String = inlines[first..end]
        .iter()
        .map(|inline| match inline {
            InlineNode::Text(text) => text.text.as_str(),
            _ => unreachable!("checked above"),
        })
        .collect();
    Ok(EditStep::ReplaceSpanText {
        block_id: paragraph.id.clone(),
        guard: semantic_hash
            .map(str::to_string)
            .unwrap_or_else(|| crate::semantic_hash::block_semantic_hash_for_block(&tracked.block)),
        expect: Some(current),
        span: super::find_replace::flat_range_to_selector(paragraph, first, end),
        content: crate::edit::ParagraphContent {
            fragments: vec![crate::edit::ContentFragment::Text(value.to_string())],
        },
        rationale: rationale.clone(),
    })
}

/// An open block control a transaction fills while it shows its placeholder
/// (`w:showingPlcHdr`, §17.5.2.39), read before the edit.
pub(crate) struct PlaceholderFill {
    /// The control's first block.
    pub(crate) control: NodeId,
    /// The run properties the control keeps for entered text (`sdtPr/w:rPr`,
    /// §17.5.2.27); none when it keeps none.
    pub(crate) entered_text_rpr: crate::word_ir::TextMarks,
}

/// The open controls `steps` fill that show their placeholder.
pub(crate) fn filled_placeholder_controls(
    doc: &CanonDoc,
    steps: &[EditStep],
) -> Result<Vec<PlaceholderFill>, EditError> {
    let mut fills = Vec::new();
    for (step_index, step) in steps.iter().enumerate() {
        let EditStep::SdtTextFill {
            block_id: Some(block_id),
            sdt_id: None,
            body_index: None,
            ..
        } = step
        else {
            continue;
        };
        let Some(wrap) = doc
            .blocks
            .iter()
            .find(|tracked| block_id_of(&tracked.block) == block_id)
            .and_then(|tracked| tracked.block_sdt_wrap.as_ref())
        else {
            // Not an open control: the fill refuses it when it is lowered.
            continue;
        };
        let sdt_pr = parse_raw_fragment(&wrap.wrapper.sdt_pr_xml)
            .expect("w:sdtPr bytes are an element written by import or an authoring verb");
        let not_fillable = |reason: String| EditError::SdtFillControlNotFillable {
            block_id: block_id.clone(),
            reason,
            step_index,
        };
        if !shows_placeholder(&sdt_pr).map_err(not_fillable)? {
            continue;
        }
        let entered_text_rpr = sdt_pr
            .children
            .iter()
            .find_map(|child| match child {
                XMLNode::Element(element) if is_w_tag(element, "rPr") => {
                    Some(crate::word_ir::parse_rpr_element(element))
                }
                _ => None,
            })
            .transpose()
            .map_err(|error| not_fillable(format!("its entered-text formatting: {error}")))?
            .unwrap_or_default();
        fills.push(PlaceholderFill {
            control: block_id.clone(),
            entered_text_rpr,
        });
    }
    Ok(fills)
}

/// A filled control no longer shows its placeholder: clear the flag on each
/// control [`filled_placeholder_controls`] named. The flag is a property of
/// the control, which Word does not track, so this is untracked in both
/// modes, as when Word fills a placeholder.
pub(crate) fn clear_placeholder_flags(doc: &mut CanonDoc, fills: &[PlaceholderFill]) {
    for fill in fills {
        // A later step of the transaction may have removed the control.
        let Some(wrap) = doc
            .blocks
            .iter_mut()
            .find(|tracked| block_id_of(&tracked.block) == &fill.control)
            .and_then(|tracked| tracked.block_sdt_wrap.as_mut())
        else {
            continue;
        };
        if let Some(cleared) = without_placeholder_flag(&wrap.wrapper) {
            wrap.wrapper = cleared;
        }
    }
}

/// The value that replaced a placeholder is formatted as entered text
/// (§17.5.2.27): the control's entered-text run properties, resolved through
/// the style cascade as import resolves any run, and never the placeholder's
/// formatting. The fill is a text replace, which keeps the replaced text's
/// formatting, and the pure edit core has no style table, so the caller that
/// holds the document's styles formats the value after the edit. The value is
/// every text of the control that is not deleted: a fillable control holds no
/// other text.
pub(crate) fn format_placeholder_values(
    doc: &mut CanonDoc,
    fills: &[PlaceholderFill],
    style_defs: Option<&crate::styles::StyleDefinitions>,
) -> Result<(), crate::runtime::RuntimeError> {
    for fill in fills {
        // A later step of the transaction may have removed the control.
        let Some((start, span)) = doc.blocks.iter().enumerate().find_map(|(index, tracked)| {
            let wrap = tracked.block_sdt_wrap.as_ref()?;
            (block_id_of(&tracked.block) == &fill.control).then_some((index, wrap.span))
        }) else {
            continue;
        };
        for tracked in &mut doc.blocks[start..start + span] {
            let BlockNode::Paragraph(paragraph) = &mut tracked.block else {
                continue;
            };
            let para_style_id = paragraph.style_id.as_deref().or_else(|| {
                style_defs.and_then(crate::styles::StyleDefinitions::default_para_style_id)
            });
            let (marks, style_props, rpr_authored) = crate::import::text_run_formatting(
                &fill.entered_text_rpr,
                style_defs,
                para_style_id,
            )?;
            for segment in &mut paragraph.segments {
                if !matches!(
                    segment.status,
                    TrackingStatus::Normal | TrackingStatus::Inserted(_)
                ) {
                    continue;
                }
                for inline in &mut segment.inlines {
                    if let InlineNode::Text(text) = inline {
                        text.marks = marks.clone();
                        text.style_props = style_props.clone();
                        text.rpr_authored = rpr_authored;
                    }
                }
            }
        }
    }
    Ok(())
}

/// `wrapper` without `w:showingPlcHdr`, or `None` when it has none.
fn without_placeholder_flag(
    wrapper: &crate::domain::SdtWrapper,
) -> Option<crate::domain::SdtWrapper> {
    let mut sdt_pr = parse_raw_fragment(&wrapper.sdt_pr_xml)
        .expect("w:sdtPr bytes are an element written by import or an authoring verb");
    let before = sdt_pr.children.len();
    sdt_pr.children.retain(|child| {
        !matches!(child, xmltree::XMLNode::Element(element)
            if crate::word_xml::is_w_tag(element, "showingPlcHdr"))
    });
    (sdt_pr.children.len() != before).then(|| crate::domain::SdtWrapper {
        sdt_pr_xml: serialize_raw_fragment(&sdt_pr),
        sdt_end_pr_xml: wrapper.sdt_end_pr_xml.clone(),
    })
}

/// Whether a control's properties say it shows its placeholder: the
/// `w:showingPlcHdr` toggle (§17.5.2.39), present and not turned off.
pub(crate) fn shows_placeholder(sdt_pr: &Element) -> Result<bool, String> {
    let Some(flag) = sdt_pr.children.iter().find_map(|child| match child {
        XMLNode::Element(element) if is_w_tag(element, "showingPlcHdr") => Some(element),
        _ => None,
    }) else {
        return Ok(false);
    };
    match crate::xml_attrs::attr_get(flag, "w:val") {
        None => Ok(true),
        Some(value) => crate::settings::parse_on_off_value(value, "w:showingPlcHdr"),
    }
}

/// Prepare a raw `w:sdt` for a new value. A control that shows its
/// placeholder (§17.5.2.39) is filled, not edited: the flag goes, and the value
/// takes the run properties the control keeps for entered text
/// (`sdtPr/w:rPr`, §17.5.2.27), or none — never the placeholder's. A control
/// that shows its value is left as it is, and the value keeps the formatting
/// of the text it replaces.
pub(crate) fn leave_placeholder(sdt: &mut Element) -> Result<ValueRpr, String> {
    let Some(sdt_pr) = sdt.children.iter_mut().find_map(|child| match child {
        XMLNode::Element(element) if is_w_tag(element, "sdtPr") => Some(element),
        _ => None,
    }) else {
        return Ok(ValueRpr::OfReplacedText);
    };
    if !shows_placeholder(sdt_pr)? {
        return Ok(ValueRpr::OfReplacedText);
    }
    sdt_pr.children.retain(
        |child| !matches!(child, XMLNode::Element(element) if is_w_tag(element, "showingPlcHdr")),
    );
    let entered_text_rpr = sdt_pr.children.iter().find_map(|child| match child {
        XMLNode::Element(element) if is_w_tag(element, "rPr") => Some(element.clone()),
        _ => None,
    });
    Ok(ValueRpr::Given(entered_text_rpr))
}
