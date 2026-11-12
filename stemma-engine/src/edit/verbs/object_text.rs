//! Text replacement inside an inline object's own text: a hyperlink's display
//! text (§17.16.22 `w:hyperlink`) or an inline content control's value
//! (§17.5.2.36 `w:sdtContent`).
//!
//! The find/replace planners (the replace-text worklist and replace-all) reach
//! a paragraph's own runs with a paragraph splice; an occurrence that stands
//! inside one of these objects is edited here, through the same cores the
//! single-object routes use: the hyperlink run splice of `ReplaceHyperlinkText`
//! and the fragment splice of `opaque_text_edit` / `sdt_text_fill`. Each
//! occurrence becomes its own change (its own deletion and insertion ids), and
//! the words an occurrence and its replacement share at either end stay in
//! place, as the single-object routes keep them.
//!
//! A data-bound control needs nothing extra: after the transaction the runtime
//! writes every bound control's new text into its store
//! (`data_binding::sync_bound_stores`), whichever route changed it.
//!
//! ## What refuses (no silent fallbacks)
//!
//! [`ObjectTextBlock`] names each reason an object's text is not editable this
//! way: a control whose content is locked (`w:lock`, §17.5.2.23), a control
//! kind whose value is not free text, a control showing its placeholder, pending
//! changes inside the object, text the object shows outside its editable runs,
//! an object inside a pending insertion, and a rewrite that would leave the
//! object without text. The planners refuse those before emitting a step; the
//! step re-checks them, so a step built elsewhere cannot bypass them.

use super::super::{
    EditError, ParagraphPath, block_at, block_at_mut, check_host_paragraph_editable,
    find_paragraph_path, shared_token_trim, splice_hyperlink_runs, stamp_revision,
};
use super::find_replace::InlineObjectText;
use crate::domain::{
    BlockNode, CanonDoc, InlineNode, NodeId, OpaqueInlineNode, OpaqueKind, RevisionInfo,
    TrackingStatus,
};
use crate::opaque_splice::{SpliceError, region_has_tracked_containers, region_text};
use crate::word_xml::OpaqueFragment;
use xmltree::{Element, XMLNode};

/// One occurrence to replace inside an object's text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectTextEdit {
    /// Where the occurrence starts, in characters of the object's text.
    pub start: usize,
    /// The text that stands there now.
    pub old: String,
    /// Its replacement.
    pub new: String,
}

/// Why an inline object's text cannot be edited by text replacement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObjectTextBlock {
    /// The control's `w:lock` (§17.5.2.23) forbids editing its contents
    /// (`contentLocked`, `sdtContentLocked`), or is a value this engine does
    /// not know; carries the attribute value.
    Locked(String),
    /// The control's value is not free text (a drop-down, date, checkbox,
    /// picture, citation, …); carries the `w:sdtPr` kind element's name.
    ControlKind(String),
    /// The control shows its placeholder (§17.5.2.39), which is not a value.
    ShowingPlaceholder,
    /// The object already holds pending tracked changes.
    PendingChanges,
    /// Some of the text the object shows lies outside its editable runs (in
    /// a field or nested control inside it).
    TextOutsideEditableRuns,
    /// The object stands inside another pending insertion.
    InPendingInsertion,
    /// The object is in a story the replacement does not edit objects in
    /// (a footnote, endnote or comment).
    OutsideBody,
    /// The replacement would leave the object with no text.
    WouldBeEmpty,
    /// The control's XML cannot be read; carries the parse failure.
    MalformedControl(String),
    /// Part of the link's text is inside a wrapper element within the link
    /// (a simple field, a content control, a smart tag, a custom XML element,
    /// a bidirectional embedding or override, a nested link) that the link's
    /// rebuilt runs would not keep; carries the element's name.
    WrappedText(String),
}

impl std::fmt::Display for ObjectTextBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ObjectTextBlock::Locked(lock) => write!(
                f,
                "the control is locked (w:lock=\"{lock}\"): its contents cannot be edited"
            ),
            ObjectTextBlock::ControlKind(kind) => write!(
                f,
                "the control is a '{kind}' control, whose value is not free text"
            ),
            ObjectTextBlock::ShowingPlaceholder => {
                write!(f, "the control shows its placeholder, not a value")
            }
            ObjectTextBlock::PendingChanges => {
                write!(
                    f,
                    "it already holds pending tracked changes; resolve them first"
                )
            }
            ObjectTextBlock::TextOutsideEditableRuns => write!(
                f,
                "part of its text is inside a field or nested control within it"
            ),
            ObjectTextBlock::InPendingInsertion => {
                write!(f, "it stands inside another pending insertion")
            }
            ObjectTextBlock::OutsideBody => write!(
                f,
                "it is in a footnote, endnote or comment, where objects are not edited by \
                 replacement"
            ),
            ObjectTextBlock::WouldBeEmpty => {
                write!(f, "the replacement would leave it with no text")
            }
            ObjectTextBlock::MalformedControl(reason) => {
                write!(f, "the control's XML cannot be read: {reason}")
            }
            ObjectTextBlock::WrappedText(element) => write!(
                f,
                "part of its text is inside a w:{element} within it, which editing the \
                 link's runs would not keep"
            ),
        }
    }
}

/// An inline object's text as replacement sees it.
#[derive(Clone, Debug)]
pub(crate) struct ObjectTextView {
    pub object: InlineObjectText,
    /// The text the reader sees: a link's display text, a control's value.
    pub shown: String,
    /// Why that text cannot be edited, if it cannot.
    pub blocked: Option<ObjectTextBlock>,
}

/// The text-holding view of an inline object in a segment of `status`, or
/// `None` for an object without text of its own (a drawing, a field, an empty
/// control) or one in deleted text.
pub(crate) fn object_text_view(
    opaque: &OpaqueInlineNode,
    status: &TrackingStatus,
) -> Option<ObjectTextView> {
    let in_insertion = match status {
        TrackingStatus::Normal => false,
        TrackingStatus::Inserted(_) => true,
        TrackingStatus::Deleted(_) | TrackingStatus::InsertedThenDeleted(_) => return None,
    };
    let (object, shown, blocked) = match &opaque.kind {
        OpaqueKind::Hyperlink(link) => {
            let pending = link.runs.iter().any(|r| r.status != TrackingStatus::Normal);
            let blocked = if pending {
                Some(ObjectTextBlock::PendingChanges)
            } else {
                link.flattened_wrappers
                    .first()
                    .map(|wrapper| ObjectTextBlock::WrappedText(wrapper.clone()))
            };
            (InlineObjectText::HyperlinkText, link.text.clone(), blocked)
        }
        OpaqueKind::Sdt => {
            let Some(crate::view::OpaqueMetadata::ContentControl {
                display_text: Some(shown),
                ..
            }) = crate::opaque_meta::project(opaque)
            else {
                return None;
            };
            let blocked = match parse_sdt(opaque) {
                Ok(sdt) => control_block(&sdt, &shown),
                Err(reason) => Some(ObjectTextBlock::MalformedControl(reason)),
            };
            (InlineObjectText::ContentControlValue, shown, blocked)
        }
        _ => return None,
    };
    let blocked = if in_insertion {
        Some(ObjectTextBlock::InPendingInsertion)
    } else {
        blocked
    };
    Some(ObjectTextView {
        object,
        shown,
        blocked,
    })
}

fn parse_sdt(opaque: &OpaqueInlineNode) -> Result<OpaqueFragment, String> {
    let raw = opaque
        .raw_xml
        .as_deref()
        .ok_or_else(|| "content control has no raw_xml".to_string())?;
    OpaqueFragment::parse(raw)
}

/// The `w:sdtPr` kind elements whose control value is not free text.
const NON_TEXT_KINDS: &[&str] = &[
    "dropDownList",
    "comboBox",
    "date",
    "checkbox",
    "picture",
    "group",
    "citation",
    "bibliography",
    "docPartObj",
    "docPartList",
    "equation",
    "repeatingSection",
    "repeatingSectionItem",
    "entityPicker",
];

/// Why a control's value cannot be edited as text, if it cannot. `shown` is
/// the value the read model reports.
fn control_block(fragment: &OpaqueFragment, shown: &str) -> Option<ObjectTextBlock> {
    let sdt = fragment.active();
    if let Some(pr) = child(sdt, "sdtPr") {
        // §17.5.2.23, read as the control fill verbs read it.
        if let Some(lock) = super::content_controls::content_lock(pr) {
            return Some(ObjectTextBlock::Locked(lock));
        }
        if let Some(kind) = pr.children.iter().find_map(|c| match c {
            XMLNode::Element(e) if NON_TEXT_KINDS.contains(&e.name.as_str()) => Some(e),
            _ => None,
        }) {
            return Some(ObjectTextBlock::ControlKind(kind.name.clone()));
        }
        match crate::edit::verbs::sdt_text_fill::shows_placeholder(pr) {
            Ok(false) => {}
            Ok(true) => return Some(ObjectTextBlock::ShowingPlaceholder),
            Err(reason) => return Some(ObjectTextBlock::MalformedControl(reason)),
        }
    }
    let content = child(sdt, "sdtContent")?;
    if region_has_tracked_containers(content) {
        return Some(ObjectTextBlock::PendingChanges);
    }
    (region_text(content) != shown).then_some(ObjectTextBlock::TextOutsideEditableRuns)
}

fn child<'a>(parent: &'a Element, local: &str) -> Option<&'a Element> {
    parent.children.iter().find_map(|c| match c {
        XMLNode::Element(e) if e.name == local => Some(e),
        _ => None,
    })
}

/// `text` with every edit applied — what the object reads after Accept.
pub(crate) fn rewritten(text: &str, edits: &[ObjectTextEdit]) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut at = 0usize;
    for edit in edits {
        out.extend(&chars[at..edit.start]);
        out.push_str(&edit.new);
        at = edit.start + edit.old.chars().count();
    }
    out.extend(&chars[at..]);
    out
}

/// Byte ranges of `text` and their replacements for `edits`, each narrowed to
/// what it changes (shared leading and trailing words stay in place). An edit
/// whose text is not at its position refuses (a stale plan); one that changes
/// nothing is dropped.
fn changed_byte_spans(
    text: &str,
    edits: &[ObjectTextEdit],
    object_id: &NodeId,
    step_index: usize,
) -> Result<Vec<(usize, usize, String)>, EditError> {
    let mut spans = Vec::new();
    let mut previous_end = 0usize;
    for edit in edits {
        let start = text
            .char_indices()
            .map(|(byte, _)| byte)
            .chain(std::iter::once(text.len()))
            .nth(edit.start);
        let present = start.filter(|&start| {
            start >= previous_end
                && !edit.old.is_empty()
                && text[start..].starts_with(edit.old.as_str())
        });
        let Some(start) = present else {
            return Err(EditError::ExpectMismatch {
                block_id: object_id.clone(),
                expected: edit.old.clone(),
                actual_text: text.to_string(),
                step_index,
            });
        };
        previous_end = start + edit.old.len();
        let (prefix, suffix, inserted) = shared_token_trim(&edit.old, &edit.new);
        let (from, to) = (start + prefix, previous_end - suffix);
        if from == to && inserted.is_empty() {
            continue;
        }
        spans.push((from, to, inserted));
    }
    Ok(spans)
}

/// Apply a `ReplaceInlineObjectText` step: the edits, in order and disjoint,
/// to the text of the link or control `object_id` in paragraph `block_id`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply(
    doc: &mut CanonDoc,
    block_id: &NodeId,
    object_id: &NodeId,
    edits: &[ObjectTextEdit],
    base: &RevisionInfo,
    rev_counter: &mut u32,
    tracked: bool,
    step_index: usize,
) -> Result<(), EditError> {
    let path: ParagraphPath =
        find_paragraph_path(doc, block_id).ok_or_else(|| EditError::BlockNotFound {
            block_id: block_id.clone(),
            step_index,
        })?;
    check_host_paragraph_editable(doc, &path, step_index)?;
    let not_found = || EditError::InlineObjectTextTargetNotFound {
        block_id: block_id.clone(),
        object_id: object_id.clone(),
        step_index,
    };
    let view = match block_at(doc, &path) {
        BlockNode::Paragraph(para) => para
            .segments
            .iter()
            .find_map(|segment| {
                segment.inlines.iter().find_map(|inline| match inline {
                    InlineNode::OpaqueInline(o) if o.id == *object_id => {
                        Some(object_text_view(o, &segment.status))
                    }
                    _ => None,
                })
            })
            .flatten()
            .ok_or_else(not_found)?,
        _ => return Err(not_found()),
    };
    let refuse = |reason: ObjectTextBlock| EditError::InlineObjectTextNotEditable {
        block_id: block_id.clone(),
        object_id: object_id.clone(),
        reason,
        step_index,
    };
    if let Some(reason) = view.blocked {
        return Err(refuse(reason));
    }
    let spans = changed_byte_spans(&view.shown, edits, object_id, step_index)?;
    if rewritten(&view.shown, edits).is_empty() {
        return Err(refuse(ObjectTextBlock::WouldBeEmpty));
    }
    if spans.is_empty() {
        return Ok(());
    }

    let BlockNode::Paragraph(para) = block_at_mut(doc, &path) else {
        unreachable!("the path resolved to a paragraph above");
    };
    let opaque = para
        .segments
        .iter_mut()
        .flat_map(|s| s.inlines.iter_mut())
        .find_map(|inline| match inline {
            InlineNode::OpaqueInline(o) if o.id == *object_id => Some(o),
            _ => None,
        })
        .expect("the object was found above");
    match &mut opaque.kind {
        OpaqueKind::Hyperlink(link) => {
            // Last span first: each splice leaves the offsets before it intact.
            for (from, to, inserted) in spans.iter().rev() {
                let deletion = stamp_revision(base, rev_counter);
                let insertion = stamp_revision(base, rev_counter);
                splice_hyperlink_runs(
                    link,
                    object_id,
                    (*from, *to),
                    inserted,
                    (&deletion, &insertion),
                    step_index,
                )?;
            }
            if !tracked {
                crate::tracked_model::project_hyperlink_runs(link, true);
            }
            // The next serialize rebuilds the link from its edited runs.
            opaque.raw_xml = None;
            opaque.content_hash = None;
        }
        OpaqueKind::Sdt => {
            let mut fragment =
                parse_sdt(opaque).map_err(|reason| EditError::OpaqueTextRawXmlParse {
                    opaque_id: object_id.clone(),
                    reason,
                    step_index,
                })?;
            let content =
                crate::opaque_splice::first_descendant_mut(fragment.active_mut(), "sdtContent")
                    .ok_or_else(not_found)?;
            crate::opaque_splice::splice_region_spans(content, &spans, base, rev_counter, tracked)
                .map_err(|error| match error {
                    SpliceError::RegionHasTrackedChanges => refuse(ObjectTextBlock::PendingChanges),
                    SpliceError::TextNotFound
                    | SpliceError::UnsupportedRegionShape
                    | SpliceError::RegionHasComplexContent => {
                        EditError::OpaqueTextUnsupportedShape {
                            opaque_id: object_id.clone(),
                            step_index,
                        }
                    }
                })?;
            let raw = fragment.serialize();
            opaque.content_hash = Some(crate::import::canonical_raw_hash(&raw));
            opaque.raw_xml = Some(raw);
        }
        _ => unreachable!("only links and controls have a text view"),
    }
    Ok(())
}
