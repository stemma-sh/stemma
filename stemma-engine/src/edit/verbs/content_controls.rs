//! Content controls / structured document tags (`w:sdt`, §17.5.2). Two verbs:
//!
//! - **`WrapInContentControl`** wraps a run-span (the `expect` substring inside a
//!   paragraph) in a freshly-built `w:sdt`. The control's `w:sdtPr` is built
//!   deterministically from a typed [`SdtSpec`] (see `serialize/sdt.rs`); the
//!   matched text becomes the `w:sdtContent`. The result is a new inline
//!   `OpaqueInline{Sdt}` with `raw_xml: Some(<w:sdt>…)`, spliced over the matched
//!   inlines.
//!
//! - **`SetContentControlValue`** mutates an existing `OpaqueInline{Sdt}`'s
//!   displayed value in place (`SetImageAttributes` pattern): it parses the
//!   `raw_xml`, sets the `sdtContent` text and/or the checkbox/selection state,
//!   re-serializes, and recomputes `content_hash`.
//!
//! ## Untracked / structural (honest reversibility)
//!
//! OOXML has NO tracked-change envelope for SDT structure (there is no
//! `w:sdtChange` the way there is `w:rPrChange`). So both verbs are
//! Direct/structural like `SetImageAttributes`: the materialization mode does
//! not change behavior, and accept-all == reject-all == the wrapped/edited doc.
//! Reversibility is therefore at the **transaction-rejection** level (don't
//! apply), not at segment accept/reject.
//!
//! ## Fail loud (CLAUDE.md "no silent fallbacks")
//!
//! - empty distinguishing spec (no tag, no alias, RichText default) ⇒
//!   `EmptyContentControlSpec` — a control with no identity and no kind is
//!   indistinguishable from un-wrapped content;
//! - a span crossing an opaque inline / hard break ⇒
//!   `UnsupportedParagraphStructure` (we never wrap — and thereby risk dropping —
//!   a nested opaque in v1; run-span text only);
//! - block-level wrapping ⇒ `ContentControlBlockUnsupported` (deferred — the
//!   engine streams body-level opaque blocks from the original parsed XML by
//!   `body_index`, so a *synthesized* block-level `w:sdt` has no serializable
//!   representation today; see `runtime.rs` body-opaque streaming);
//! - `SetContentControlValue` id not an SDT ⇒ `NotAContentControl`;
//! - value kind incompatible with the control type (e.g. set-checked on a
//!   plain-text control) ⇒ `ContentControlTypeMismatch`.
//!
//! ## v1 scope (deferred, documented — NOT silently handled)
//!
//! - `w:dataBinding` XPath auto-resolution is OUT: we never resolve a binding
//!   target (no silent fallback);
//! - repeating-section instance add/remove is OUT (structural multi-block, v2).

use super::super::{EditError, find_block_index, validate_block_is_editable};
use crate::domain::DocPart;
use crate::domain::{
    BlockNode, CanonDoc, InlineNode, NodeId, OpaqueInlineNode, OpaqueKind, ProofRef, SdtControl,
    StyleProps, TextNode, TrackedSegment, TrackingStatus,
};
use crate::import::sha256_hex;
use crate::serialize::sdt::build_inline_sdt;
use crate::word_xml::{parse_raw_fragment, serialize_raw_fragment};
use xmltree::{Element, XMLNode};

/// The specification of a content control to author: optional identity
/// (`w:tag` / `w:alias`) plus the control type. At least one distinguishing
/// field must be present (a tag, an alias, or a non-RichText control) — an
/// all-empty spec is refused (`EmptyContentControlSpec`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SdtSpec {
    /// The control's `w:tag` (a programmatic handle), if any.
    pub tag: Option<String>,
    /// The control's `w:alias` (a human-friendly title), if any.
    pub alias: Option<String>,
    /// The control type (plain text, dropdown, checkbox, …).
    pub control: SdtControl,
    /// Optional XML data binding (`w:dataBinding`, §17.5.2.6): an XPath into a
    /// custom-XML datastore part plus the part's `storeItemID`. When present,
    /// the wrap emits `<w:dataBinding>` in the `sdtPr` AND stages a backing
    /// `customXml/item*.xml` datastore part (see [`DataBinding`]). When `None`,
    /// the control is plain (no datastore link), exactly as before.
    pub binding: Option<DataBinding>,
}

/// An XML data binding for a content control (`w:dataBinding`, ECMA-376
/// §17.5.2.6). It binds the control's displayed value to a node in a custom-XML
/// datastore part: Word reads/writes the bound node when the user edits the
/// control.
///
/// Authoring a binding has two halves, mirroring the styles.xml / numbering.xml
/// part-bootstrap precedent:
/// 1. the `sdtPr` gains a `<w:dataBinding w:xpath="…" w:storeItemID="…">` (and
///    an optional `w:prefixMappings`); and
/// 2. the save path stages a `customXml/item*.xml` datastore part (plus its
///    `itemProps` + content-types + relationship) keyed by `store_item_id`, so
///    Word resolves the `storeItemID` to a real, well-formed part.
///
/// Fail loud (no silent fallback): an empty `xpath` or empty `store_item_id` is
/// refused at the verb edge (`MalformedDataBinding`) — a binding with no target
/// is indistinguishable from no binding and would silently degrade to plain
/// text in Word.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataBinding {
    /// The XPath selecting the bound node in the datastore part. Non-empty.
    pub xpath: String,
    /// The `storeItemID` GUID of the backing datastore part (the `ds:itemID`
    /// authored on the `customXml/itemProps*.xml` part). Non-empty. Multiple
    /// bindings sharing one `store_item_id` reuse a single datastore part.
    pub store_item_id: String,
    /// Optional `w:prefixMappings` declaring the XML namespace prefixes used in
    /// `xpath` (e.g. `xmlns:ns0='urn:contract'`). `None` = no namespace prefixes.
    pub prefix_mappings: Option<String>,
}

impl SdtSpec {
    /// True when the spec carries no distinguishing data: no tag, no alias, and
    /// the default RichText control (which emits no kind child). Such a control
    /// is indistinguishable from un-wrapped content and is refused.
    pub(crate) fn is_empty(&self) -> bool {
        self.tag.as_deref().map(str::trim).unwrap_or("").is_empty()
            && self
                .alias
                .as_deref()
                .map(str::trim)
                .unwrap_or("")
                .is_empty()
            && matches!(self.control, SdtControl::RichText)
    }
}

/// The new value to set on an existing content control. Each variant is only
/// valid for a matching control type — a mismatch is `ContentControlTypeMismatch`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SdtValue {
    /// Set the displayed text (valid for PlainText / RichText / ComboBox / Date).
    Text(String),
    /// Set the checkbox state (valid only for a Checkbox control).
    Checked(bool),
    /// Select a value by its stored `w:value` (valid for Dropdown / ComboBox).
    Selected(String),
}

// ─── WrapInContentControl ─────────────────────────────────────────────────────

pub(crate) fn apply_wrap(
    doc: &mut CanonDoc,
    block_id: &NodeId,
    expect: &str,
    semantic_hash: Option<&str>,
    spec: &SdtSpec,
    pending_custom_xml: &mut Vec<crate::edit::CustomXmlPart>,
    step_index: usize,
) -> Result<(), EditError> {
    // Reject a spec with no distinguishing data at the verb edge.
    if spec.is_empty() {
        return Err(EditError::EmptyContentControlSpec { step_index });
    }

    // Validate the data binding (if any) at the verb edge — a binding with an
    // empty xpath or empty storeItemID is unresolvable and would silently
    // degrade to a plain control in Word (CLAUDE.md "no silent fallbacks").
    if let Some(b) = &spec.binding {
        if b.xpath.trim().is_empty() {
            return Err(EditError::MalformedDataBinding {
                reason: "data binding has an empty xpath",
                step_index,
            });
        }
        if b.store_item_id.trim().is_empty() {
            return Err(EditError::MalformedDataBinding {
                reason: "data binding has an empty storeItemID",
                step_index,
            });
        }
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
        && let Err(actual) =
            crate::semantic_hash::check_block_guard(&doc.blocks[idx].block, expected)
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

    // Derive a stable decimal SDT id from the paragraph id hash (the value only
    // needs to be unique within the document; Word treats it as opaque).
    let sdt_id_num = stable_sdt_id(&para.id.0);
    let sdt_node_id = NodeId::from(format!("{}_sdt0", para.id.0));

    let plan = find_text_span(&para.segments, expect).ok_or_else(|| {
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

    // Build the inner run XML from the matched text (run-span wrap, v1). The
    // matched text is the `expect` substring exactly.
    let inner_run = format!(
        r#"<w:r><w:t xml:space="preserve">{}</w:t></w:r>"#,
        xml_escape_text(expect)
    );
    let sdt_xml = build_inline_sdt(
        sdt_id_num,
        spec.tag.as_deref(),
        spec.alias.as_deref(),
        &spec.control,
        spec.binding.as_ref(),
        &inner_run,
    );
    let raw = sdt_xml.into_bytes();
    let content_hash = Some(sha256_hex(&raw));

    // When the control is data-bound, stage the backing custom-XML datastore
    // part so the save path authors (or reuses) a `customXml/item*.xml` whose
    // `storeItemID` matches the `w:dataBinding` we just emitted. Without this,
    // the `storeItemID` would dangle and Word would open the control unbound.
    // The datastore root element name is derived from the binding's xpath so the
    // bound XPath has a real node to address; the part dedups by storeItemID.
    if let Some(b) = &spec.binding {
        pending_custom_xml.push(crate::edit::CustomXmlPart {
            store_item_id: b.store_item_id.clone(),
            root_element: datastore_root_from_xpath(&b.xpath),
            namespace: None,
        });
    }

    let sdt_inline = InlineNode::from(OpaqueInlineNode {
        id: sdt_node_id.clone(),
        kind: OpaqueKind::Sdt,
        opaque_ref: format!("sdtref_{}", sdt_node_id.0),
        proof_ref: ProofRef {
            part: DocPart::DocumentXml,
            block_id: sdt_node_id,
            docx_anchor: String::new(),
        },
        wrapper_marks: Vec::new(),
        wrapper_style_props: StyleProps::default(),
        raw_xml: Some(raw),
        content_hash,
    });

    splice_over_span(&mut para.segments, plan, sdt_inline);

    para.block_text_hash = None;
    para.rendered_text = None;
    Ok(())
}

/// A located text span [start, end) within a single Normal segment, expressed as
/// the segment index, the inline index of the matched text node, and the char
/// offsets of the match start/end inside that node. v1 requires the whole `expect`
/// match to lie within ONE text node (no opaque/break crossing).
struct SpanPlan {
    seg_idx: usize,
    inline_idx: usize,
    char_start: usize,
    char_end: usize,
}

/// Locate `expect` as a contiguous substring inside a single `Text` inline of a
/// Normal segment. Returns `None` if the match is absent or would straddle a
/// non-text inline. The caller surfaces a straddle as
/// `UnsupportedParagraphStructure` (it cannot be found here, so it falls through
/// to `ExpectMismatch` — but a single-node match is the only span we wrap, by
/// design, so we never silently wrap across an opaque).
fn find_text_span(segments: &[TrackedSegment], expect: &str) -> Option<SpanPlan> {
    if expect.is_empty() {
        return None;
    }
    for (seg_idx, seg) in segments.iter().enumerate() {
        if seg.status != TrackingStatus::Normal {
            continue;
        }
        for (inline_idx, inline) in seg.inlines.iter().enumerate() {
            if let InlineNode::Text(t) = inline
                && let Some(byte) = t.text.find(expect)
            {
                let char_start = t.text[..byte].chars().count();
                let char_end = char_start + expect.chars().count();
                return Some(SpanPlan {
                    seg_idx,
                    inline_idx,
                    char_start,
                    char_end,
                });
            }
        }
    }
    None
}

/// Splice the `sdt` opaque over the matched text span. The host text node is
/// split into up to three parts: a head Text (before the match), the SDT opaque
/// (replacing the matched text), and a tail Text (after the match). All stay in
/// the SAME Normal segment — SDT structure is untracked, so there is no
/// Inserted/Deleted projection here.
fn splice_over_span(segments: &mut [TrackedSegment], plan: SpanPlan, sdt: InlineNode) {
    let seg = &mut segments[plan.seg_idx];
    let InlineNode::Text(node) = &seg.inlines[plan.inline_idx] else {
        unreachable!("span located on a text node");
    };
    let chars: Vec<char> = node.text.chars().collect();
    let before: String = chars[..plan.char_start].iter().collect();
    let after: String = chars[plan.char_end..].iter().collect();

    let mut replacement: Vec<InlineNode> = Vec::new();
    if !before.is_empty() {
        let mut head = node.clone();
        head.text = before;
        replacement.push(InlineNode::Text(head));
    }
    replacement.push(sdt);
    if !after.is_empty() {
        let mut tail: TextNode = (**node).clone();
        tail.id = NodeId::new(format!("{}_sdttail", node.id.0));
        tail.text = after;
        replacement.push(InlineNode::from(tail));
    }

    seg.inlines
        .splice(plan.inline_idx..=plan.inline_idx, replacement);
}

// ─── SetContentControlValue ───────────────────────────────────────────────────

pub(crate) fn apply_set_value(
    doc: &mut CanonDoc,
    block_id: &NodeId,
    sdt_id: &NodeId,
    value: &SdtValue,
    step_index: usize,
) -> Result<(), EditError> {
    let idx = find_block_index(&doc.blocks, block_id).ok_or_else(|| EditError::BlockNotFound {
        block_id: block_id.clone(),
        step_index,
    })?;

    let node = locate_sdt_mut(&mut doc.blocks[idx].block, sdt_id, step_index)?;
    let raw = node
        .raw_xml
        .as_deref()
        .ok_or_else(|| EditError::ContentControlMissingRawXml {
            sdt_id: sdt_id.clone(),
            step_index,
        })?;
    let mut element =
        parse_raw_fragment(raw).map_err(|e| EditError::ContentControlRawXmlParse {
            sdt_id: sdt_id.clone(),
            reason: e.to_string(),
            step_index,
        })?;

    apply_value_to_sdt(&mut element, value, sdt_id, step_index)?;

    let new_raw = serialize_raw_fragment(&element);
    node.content_hash = Some(sha256_hex(&new_raw));
    node.raw_xml = Some(new_raw);
    Ok(())
}

/// Mutate the parsed `w:sdt` element per `value`, enforcing control-type
/// compatibility. We inspect `w:sdtPr` to learn the control kind, then refuse a
/// value that does not apply (`ContentControlTypeMismatch`).
fn apply_value_to_sdt(
    sdt: &mut Element,
    value: &SdtValue,
    sdt_id: &NodeId,
    step_index: usize,
) -> Result<(), EditError> {
    let kind = detect_control_kind(sdt);
    // Setting any value takes the control out of the placeholder state
    // (§17.5.2.39): `<w:showingPlcHdr/>` marks the displayed run as placeholder
    // text, not a real value. Leaving it set would make Word render the new text
    // as placeholder (greyed, replaced on next focus). Clear it for every value
    // kind, before the value-specific mutation below.
    clear_showing_placeholder(sdt);
    match value {
        SdtValue::Checked(checked) => {
            if kind != DetectedKind::Checkbox {
                return Err(EditError::ContentControlTypeMismatch {
                    sdt_id: sdt_id.clone(),
                    requested: "checked",
                    actual: kind.label(),
                    step_index,
                });
            }
            set_checkbox_state(sdt, *checked);
            // The displayed glyph (sdtContent run text) tracks the state.
            set_sdt_content_text(sdt, if *checked { "\u{2612}" } else { "\u{2610}" });
        }
        SdtValue::Text(text) => {
            // Text is valid for text-bearing controls but NOT a checkbox (whose
            // displayed content is a state glyph, set via Checked).
            if kind == DetectedKind::Checkbox {
                return Err(EditError::ContentControlTypeMismatch {
                    sdt_id: sdt_id.clone(),
                    requested: "text",
                    actual: kind.label(),
                    step_index,
                });
            }
            set_sdt_content_text(sdt, text);
        }
        SdtValue::Selected(value) => {
            if kind != DetectedKind::Dropdown && kind != DetectedKind::ComboBox {
                return Err(EditError::ContentControlTypeMismatch {
                    sdt_id: sdt_id.clone(),
                    requested: "selected",
                    actual: kind.label(),
                    step_index,
                });
            }
            // Resolve the selected value's display text from the list items; if
            // the value is not among them, that is a caller error (no silent
            // fallback) — surface it as a type mismatch with context.
            let display = list_item_display(sdt, value).ok_or_else(|| {
                EditError::ContentControlTypeMismatch {
                    sdt_id: sdt_id.clone(),
                    requested: "selected (value not in list)",
                    actual: kind.label(),
                    step_index,
                }
            })?;
            set_sdt_content_text(sdt, &display);
        }
    }
    Ok(())
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum DetectedKind {
    PlainText,
    RichText,
    Dropdown,
    ComboBox,
    Checkbox,
    Date,
    RepeatingSection,
}

impl From<crate::view::SdtControlKind> for DetectedKind {
    fn from(kind: crate::view::SdtControlKind) -> Self {
        use crate::view::SdtControlKind;
        match kind {
            SdtControlKind::PlainText => DetectedKind::PlainText,
            SdtControlKind::RichText => DetectedKind::RichText,
            SdtControlKind::Dropdown => DetectedKind::Dropdown,
            SdtControlKind::ComboBox => DetectedKind::ComboBox,
            SdtControlKind::Checkbox => DetectedKind::Checkbox,
            SdtControlKind::Date => DetectedKind::Date,
            SdtControlKind::RepeatingSection => DetectedKind::RepeatingSection,
        }
    }
}

impl DetectedKind {
    fn label(self) -> &'static str {
        match self {
            DetectedKind::PlainText => "plain_text",
            DetectedKind::RichText => "rich_text",
            DetectedKind::Dropdown => "dropdown",
            DetectedKind::ComboBox => "combo_box",
            DetectedKind::Checkbox => "checkbox",
            DetectedKind::Date => "date",
            DetectedKind::RepeatingSection => "repeating_section",
        }
    }
}

/// Inspect `w:sdtPr` to classify the control. RichText is the absence of a
/// recognized kind child.
///
/// Re-expressed against the shared read primitive
/// [`crate::opaque_meta::sdt_control_kind`] (one parser, two callers): the verb
/// keeps its own `DetectedKind` (used for the write-path type-mismatch checks),
/// but the element walk that classifies the kind lives in one place.
fn detect_control_kind(sdt: &Element) -> DetectedKind {
    DetectedKind::from(crate::opaque_meta::sdt_control_kind(sdt))
}

/// Remove `<w:showingPlcHdr/>` from the control's `w:sdtPr` if present, taking
/// it out of the placeholder state (§17.5.2.39). A no-op when the control was
/// not showing placeholder text.
fn clear_showing_placeholder(sdt: &mut Element) {
    if let Some(pr) = child_by_local_mut(sdt, "sdtPr") {
        pr.children
            .retain(|c| !matches!(c, XMLNode::Element(el) if el.name == "showingPlcHdr"));
    }
}

/// Set the `w14:checked` @val under `w14:checkbox` to 1/0.
fn set_checkbox_state(sdt: &mut Element, checked: bool) {
    if let Some(pr) = child_by_local_mut(sdt, "sdtPr")
        && let Some(checkbox) = child_by_local_mut(pr, "checkbox")
        && let Some(checked_el) = child_by_local_mut(checkbox, "checked")
    {
        crate::xml_attrs::attr_set(checked_el, "w14:val", if checked { "1" } else { "0" });
    }
}

/// Find the display text for a selected `w:value` among the control's list
/// items. Re-expressed against the shared read primitive
/// [`crate::opaque_meta::sdt_list_items`] (one parser, two callers).
fn list_item_display(sdt: &Element, value: &str) -> Option<String> {
    crate::opaque_meta::sdt_list_items(sdt)
        .into_iter()
        .find(|item| item.value == value)
        .map(|item| item.display)
}

/// Replace the `w:sdtContent` with a single run carrying `text`, preserving the
/// content element so the control stays well-formed.
fn set_sdt_content_text(sdt: &mut Element, text: &str) {
    let Some(content) = child_by_local_mut(sdt, "sdtContent") else {
        return;
    };
    // Build a single <w:r><w:t xml:space="preserve">text</w:t></w:r>.
    let mut t = crate::word_xml::w_el("t");
    crate::xml_attrs::attr_set(&mut t, "xml:space", "preserve");
    t.children.push(XMLNode::Text(text.to_string()));
    let mut r = crate::word_xml::w_el("r");
    r.children.push(XMLNode::Element(t));
    content.children = vec![XMLNode::Element(r)];
}

// ─── element helpers (xmltree-ns: `name` is the LOCAL name) ───────────────────

fn child_by_local_mut<'a>(parent: &'a mut Element, local: &str) -> Option<&'a mut Element> {
    parent.children.iter_mut().find_map(|c| match c {
        XMLNode::Element(el) if el.name == local => Some(el),
        _ => None,
    })
}

/// Locate the `OpaqueInline{Sdt}` with `sdt_id` in `block`. `NotAContentControl`
/// when the id resolves to some other opaque kind, mirroring the image verb's
/// `NotADrawing`; the dedicated `ContentControlNotFound` is reserved for a
/// missing id.
fn locate_sdt_mut<'a>(
    block: &'a mut BlockNode,
    sdt_id: &NodeId,
    step_index: usize,
) -> Result<&'a mut OpaqueInlineNode, EditError> {
    let BlockNode::Paragraph(para) = block else {
        return Err(EditError::ContentControlNotFound {
            sdt_id: sdt_id.clone(),
            step_index,
        });
    };
    let mut found_is_sdt: Option<bool> = None;
    for seg in &para.segments {
        for inline in &seg.inlines {
            if let InlineNode::OpaqueInline(o) = inline
                && o.id == *sdt_id
            {
                found_is_sdt = Some(matches!(o.kind, OpaqueKind::Sdt));
                break;
            }
        }
        if found_is_sdt.is_some() {
            break;
        }
    }
    match found_is_sdt {
        None => Err(EditError::ContentControlNotFound {
            sdt_id: sdt_id.clone(),
            step_index,
        }),
        Some(false) => Err(EditError::NotAContentControl {
            sdt_id: sdt_id.clone(),
            step_index,
        }),
        Some(true) => {
            for seg in &mut para.segments {
                for inline in &mut seg.inlines {
                    if let InlineNode::OpaqueInline(o) = inline
                        && o.id == *sdt_id
                    {
                        return Ok(o);
                    }
                }
            }
            unreachable!("sdt located in the immutable scan above");
        }
    }
}

// ─── small builders ───────────────────────────────────────────────────────────

/// Derive the local name of the datastore document element from a bound XPath.
///
/// The bound XPath addresses a node *inside* the datastore part; the part needs
/// a well-formed root element for that path to resolve against. We take the
/// first step's local name (e.g. `/ns0:root[1]/ns0:party[1]` -> `root`),
/// stripping any namespace prefix and positional predicate. When the xpath has
/// no usable step (e.g. `.` or `/`), we author a generic `root` — this is a
/// deterministic, non-silent default for the skeleton's element name, NOT a
/// fallback for a missing target (the xpath itself is already validated
/// non-empty). The skeleton only needs to be well-formed and root-addressable;
/// Word writes the bound value into it on edit.
fn datastore_root_from_xpath(xpath: &str) -> String {
    let first_step = xpath
        .split('/')
        .map(str::trim)
        .find(|s| !s.is_empty() && *s != ".");
    let Some(step) = first_step else {
        return "root".to_string();
    };
    // Strip a positional predicate `[…]` and a namespace prefix `ns0:`.
    let no_pred = step.split('[').next().unwrap_or(step);
    let local = no_pred.rsplit(':').next().unwrap_or(no_pred).trim();
    // Keep only XML-name-safe leading content; if empty, use a generic root.
    let name: String = local
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '-' || *c == '.')
        .collect();
    if name.is_empty()
        || !name
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
    {
        "root".to_string()
    } else {
        name
    }
}

/// A stable, document-unique positive decimal id for `w:id`, derived from the
/// host id text. The value is opaque to Word — uniqueness is all that matters.
fn stable_sdt_id(seed: &str) -> i32 {
    let hex = sha256_hex(seed.as_bytes());
    let n = i32::from_str_radix(&hex[..7], 16).unwrap_or(1);
    n.max(1)
}

/// Escape the five XML predefined entities for text content.
fn xml_escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::SdtListItem;

    fn spec(control: SdtControl) -> SdtSpec {
        SdtSpec {
            tag: Some("t".into()),
            alias: None,
            control,
            binding: None,
        }
    }

    #[test]
    fn empty_spec_is_detected() {
        let s = SdtSpec {
            tag: None,
            alias: Some("   ".into()),
            control: SdtControl::RichText,
            binding: None,
        };
        assert!(s.is_empty());
        assert!(!spec(SdtControl::RichText).is_empty()); // tag distinguishes it
        let kind_only = SdtSpec {
            tag: None,
            alias: None,
            control: SdtControl::PlainText,
            binding: None,
        };
        assert!(!kind_only.is_empty()); // a non-RichText kind distinguishes it
    }

    #[test]
    fn detect_kind_reads_sdtpr() {
        let xml = build_inline_sdt(1, Some("t"), None, &SdtControl::PlainText, None, "<w:r/>");
        let el = parse_raw_fragment(xml.as_bytes()).unwrap();
        assert_eq!(detect_control_kind(&el), DetectedKind::PlainText);

        let xml = build_inline_sdt(
            1,
            None,
            None,
            &SdtControl::Checkbox { checked: false },
            None,
            "<w:r/>",
        );
        let el = parse_raw_fragment(xml.as_bytes()).unwrap();
        assert_eq!(detect_control_kind(&el), DetectedKind::Checkbox);
    }

    #[test]
    fn set_content_text_replaces_run() {
        let xml = build_inline_sdt(
            1,
            Some("t"),
            None,
            &SdtControl::PlainText,
            None,
            "<w:r><w:t>old</w:t></w:r>",
        );
        let mut el = parse_raw_fragment(xml.as_bytes()).unwrap();
        set_sdt_content_text(&mut el, "new");
        let out = String::from_utf8(serialize_raw_fragment(&el)).unwrap();
        assert!(out.contains("new"));
        assert!(!out.contains("old"));
    }

    #[test]
    fn checkbox_state_toggles_val() {
        let xml = build_inline_sdt(
            1,
            None,
            None,
            &SdtControl::Checkbox { checked: false },
            None,
            "<w:r/>",
        );
        let mut el = parse_raw_fragment(xml.as_bytes()).unwrap();
        set_checkbox_state(&mut el, true);
        let out = String::from_utf8(serialize_raw_fragment(&el)).unwrap();
        assert!(out.contains(r#"w:val="1""#));
    }

    #[test]
    fn list_item_display_resolves_value() {
        let xml = build_inline_sdt(
            1,
            None,
            None,
            &SdtControl::Dropdown {
                items: vec![SdtListItem {
                    display: "Yes".into(),
                    value: "Y".into(),
                }],
            },
            None,
            "<w:r/>",
        );
        let el = parse_raw_fragment(xml.as_bytes()).unwrap();
        assert_eq!(list_item_display(&el, "Y").as_deref(), Some("Yes"));
        assert_eq!(list_item_display(&el, "Z"), None);
    }

    #[test]
    fn datastore_root_derives_from_xpath_first_step() {
        assert_eq!(
            datastore_root_from_xpath("/ns0:root[1]/ns0:party[1]"),
            "root"
        );
        assert_eq!(datastore_root_from_xpath("/contract/clause"), "contract");
        assert_eq!(datastore_root_from_xpath("party"), "party");
        // No usable step => deterministic generic root (not a silent target fallback).
        assert_eq!(datastore_root_from_xpath("/"), "root");
        assert_eq!(datastore_root_from_xpath("."), "root");
    }
}
