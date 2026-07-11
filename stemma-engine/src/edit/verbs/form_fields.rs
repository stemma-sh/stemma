//! `SetFormFieldValue` — fill a legacy form field (FORMTEXT / FORMCHECKBOX /
//! FORMDROPDOWN: the `w:fldChar` + `w:ffData` complex-field carrier, §17.16).
//!
//! ## Model
//!
//! A complex form field is NOT one opaque. Import emits each `w:fldChar` /
//! `w:instrText` as its own `OpaqueInline{Field}` and the cached result run(s)
//! as ordinary `TextNode` inlines between the `separate` and `end` anchors
//! (`import.rs`). So this verb mutates **two disjoint sites**:
//!
//! - the `w:ffData` blob inside the *begin* anchor's `raw_xml`
//!   (textInput default / checkbox `w:checked` / ddList `w:result` index), and
//! - the cached **result run(s)** in the segment list (the value Word renders).
//!
//! A correct setter touches both, so the stored state and the displayed value
//! agree (the state⇄render invariant). The field is addressed by its **begin**
//! anchor id — the node that carries `ffData`.
//!
//! ## Tracking
//!
//! Untracked / in-place, like `SetImageAttributes` and `SetContentControlValue`:
//! Word fills a form field as a field-result update, not a tracked insertion
//! (§17.16.18; there is no revision envelope for "the field result changed").
//!
//! ## `w:enabled` / document protection — intentional non-consult
//!
//! `w:enabled` (§17.16.14) and `<w:documentProtection w:edit="forms"/>` gate
//! whether Word's *interactive UI* lets a human type in the field. stemma is a
//! programmatic editor, not Word's UI: the caller owns the policy of whether a
//! disabled field should be filled. We deliberately do NOT consult either —
//! honoring them would conflate "Word won't let a human type here" with "the API
//! may not set a value" (a documented decision, not a silent fallback).
//!
//! ## Fail loud (CLAUDE.md "no silent fallbacks")
//!
//! - id not a `Field` opaque, or not a `Begin` form-field anchor → `NotAFormField`
//! - no opaque inline carries the id                             → `FormFieldNotFound`
//! - begin anchor has no `raw_xml`                               → `FormFieldMissingRawXml`
//! - ffData fails to parse                                       → `FormFieldRawXmlParse`
//! - ffData has no textInput/checkBox/ddList child               → `MalformedFfData`
//! - value kind ≠ field type                                     → `FormFieldTypeMismatch`
//! - dropdown value not in `listEntry` set                       → `FormFieldValueNotInList`
//! - result region carries a tracked change                      → `FormFieldResultHasTrackedChanges`

use super::super::{EditError, block_at_mut, find_paragraph_path};
use crate::domain::{
    BlockNode, CanonDoc, FieldKind, InlineNode, NodeId, OpaqueKind, RunRprAuthored, TextNode,
    TrackingStatus,
};
use crate::import::sha256_hex;
use crate::word_xml::{parse_raw_fragment, serialize_raw_fragment};
use xmltree::{Element, XMLNode};

/// New value for a legacy form field. Each variant is valid only for a matching
/// field type — a mismatch is `FormFieldTypeMismatch`. Distinct from
/// `SdtValue` (content controls): legacy form fields and SDTs are different
/// domains and must not converge by accident.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormFieldValue {
    /// FORMTEXT: set the displayed text (the result run).
    Text(String),
    /// FORMCHECKBOX: set the checked state (ffData `w:checked`). No result run.
    Checked(bool),
    /// FORMDROPDOWN: select an entry by its `listEntry` value string.
    Selected(String),
}

impl FormFieldValue {
    fn kind_label(&self) -> &'static str {
        match self {
            FormFieldValue::Text(_) => "text",
            FormFieldValue::Checked(_) => "checked",
            FormFieldValue::Selected(_) => "selected",
        }
    }
}

// ─── Field-span locator ───────────────────────────────────────────────────────

/// A flat coordinate into a paragraph's inline flow: which segment, which inline
/// within that segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InlinePos {
    segment: usize,
    inline: usize,
}

/// The located span of one complex form field within a paragraph.
struct FieldSpan {
    /// The begin anchor (carries `ffData`).
    begin: InlinePos,
    /// The matching separate anchor, if any (FORMCHECKBOX has none).
    separate: Option<InlinePos>,
    /// The matching end anchor.
    #[allow(dead_code)]
    end: InlinePos,
    /// The result-run inline positions, in order: the inlines strictly between
    /// `separate` and `end` (empty when there is no separate). These are the
    /// cached displayed value Word renders.
    result: Vec<InlinePos>,
}

/// Walk a paragraph's inlines in document order and return `(pos, status,
/// &inline)` triples. The flow crosses segment boundaries, so a field's
/// begin/separate/end and its result run can live in different segments (e.g. a
/// result run inside a tracked `w:ins` segment).
fn paragraph_inline_flow(
    para: &crate::domain::ParagraphNode,
) -> Vec<(InlinePos, TrackingStatus, &InlineNode)> {
    let mut flow = Vec::new();
    for (s, seg) in para.segments.iter().enumerate() {
        for (i, inline) in seg.inlines.iter().enumerate() {
            flow.push((
                InlinePos {
                    segment: s,
                    inline: i,
                },
                seg.status.clone(),
                inline,
            ));
        }
    }
    flow
}

/// Classify an inline as a form-field anchor part (or not). Only `Field` opaque
/// inlines are field parts; everything else (text, drawings, …) is content.
fn field_kind_of(inline: &InlineNode) -> Option<&FieldKind> {
    match inline {
        InlineNode::OpaqueInline(o) => match &o.kind {
            OpaqueKind::Field(data) => Some(&data.field_kind),
            _ => None,
        },
        _ => None,
    }
}

/// Locate the form-field span whose BEGIN anchor has id `field_id`, pairing
/// begin/separate/end by **nesting depth** (§17.16.18 permits nested fields).
///
/// Walks the inline flow once, maintaining a depth counter (begin `+1`, end
/// `-1`). The matching `separate`/`end` for our begin are the next ones that
/// return to the begin's depth at the same nesting level. The result region is
/// every inline strictly between the matching separate and end.
///
/// Refuses:
/// - `NotAFormField` if `field_id` resolves to a `Simple` field or a non-begin
///   field part (its `instr` may say FORMTEXT, but a `fldSimple` carries no
///   ffData to set), or to a non-`Field` opaque.
/// - `FormFieldNotFound` if no opaque inline carries `field_id`.
/// - `FormFieldResultHasTrackedChanges` if any result-region inline lives in a
///   non-`Normal` segment (we cannot safely overwrite a half-tracked result).
fn locate_form_field_span(
    para: &crate::domain::ParagraphNode,
    field_id: &NodeId,
    step_index: usize,
) -> Result<FieldSpan, EditError> {
    let flow = paragraph_inline_flow(para);

    // Find the begin anchor with our id, and confirm it is a Begin field part.
    let begin_idx = flow.iter().position(
        |(_, _, inline)| matches!(inline, InlineNode::OpaqueInline(o) if o.id == *field_id),
    );
    let Some(begin_idx) = begin_idx else {
        return Err(EditError::FormFieldNotFound {
            field_id: field_id.clone(),
            step_index,
        });
    };
    match field_kind_of(flow[begin_idx].2) {
        Some(FieldKind::Begin) => {}
        // A Simple field, an Instruction/Separate/End part, an Unknown
        // fldCharType, or a non-Field opaque are all "not a fillable form-field
        // begin anchor".
        _ => {
            return Err(EditError::NotAFormField {
                field_id: field_id.clone(),
                step_index,
            });
        }
    }

    // Walk forward from the begin, pairing by depth. Our begin sits at depth 1
    // relative to the point just before it; its matching separate/end are the
    // ones that bring the running depth back to 1 (separate) and 0 (end).
    let mut depth = 0i32;
    let mut separate: Option<InlinePos> = None;
    let mut end: Option<InlinePos> = None;
    for (pos, _status, inline) in &flow[begin_idx..] {
        match field_kind_of(inline) {
            Some(FieldKind::Begin) => depth += 1,
            Some(FieldKind::Separate) => {
                // The separate that belongs to OUR field is the one seen while
                // we are exactly one level deep (inside our begin, no nested
                // begin open).
                if depth == 1 && separate.is_none() {
                    separate = Some(*pos);
                }
            }
            Some(FieldKind::End) => {
                depth -= 1;
                if depth == 0 {
                    end = Some(*pos);
                    break;
                }
            }
            _ => {}
        }
    }

    let Some(end) = end else {
        // A begin with no matching end is a malformed field region.
        return Err(EditError::NotAFormField {
            field_id: field_id.clone(),
            step_index,
        });
    };

    // The result region is the inlines strictly between the matching separate
    // and end. If there is no separate (FORMCHECKBOX), there is no result run.
    let mut result = Vec::new();
    if let Some(sep) = separate {
        let sep_flow_idx = flow
            .iter()
            .position(|(p, _, _)| *p == sep)
            .expect("separate position is in the flow");
        let end_flow_idx = flow
            .iter()
            .position(|(p, _, _)| *p == end)
            .expect("end position is in the flow");
        for (pos, status, _inline) in &flow[sep_flow_idx + 1..end_flow_idx] {
            // A result region that crosses tracked changes cannot be safely
            // overwritten in v1 (we would lose redline integrity).
            if !matches!(status, TrackingStatus::Normal) {
                return Err(EditError::FormFieldResultHasTrackedChanges {
                    field_id: field_id.clone(),
                    step_index,
                });
            }
            result.push(*pos);
        }
    }

    Ok(FieldSpan {
        begin: flow[begin_idx].0,
        separate,
        end,
        result,
    })
}

// ─── ffData classification + mutation ─────────────────────────────────────────

/// Which form-field-type child the begin anchor's `ffData` carries. RichText is
/// not a thing here — a legacy form field is exactly one of these three.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FfKind {
    Text,
    Checkbox,
    Dropdown,
}

impl FfKind {
    fn label(self) -> &'static str {
        match self {
            FfKind::Text => "FORMTEXT",
            FfKind::Checkbox => "FORMCHECKBOX",
            FfKind::Dropdown => "FORMDROPDOWN",
        }
    }
}

/// Find the `w:ffData` element inside the parsed begin-anchor `w:fldChar`. In
/// `parse_raw_fragment` output `.name` is the local name.
fn ffdata_mut(fld_char: &mut Element) -> Option<&mut Element> {
    fld_char.children.iter_mut().find_map(|c| match c {
        XMLNode::Element(el) if el.name == "ffData" => Some(el),
        _ => None,
    })
}

fn child_by_local<'a>(parent: &'a Element, local: &str) -> Option<&'a Element> {
    parent.children.iter().find_map(|c| match c {
        XMLNode::Element(el) if el.name == local => Some(el),
        _ => None,
    })
}

fn child_by_local_mut<'a>(parent: &'a mut Element, local: &str) -> Option<&'a mut Element> {
    parent.children.iter_mut().find_map(|c| match c {
        XMLNode::Element(el) if el.name == local => Some(el),
        _ => None,
    })
}

/// Classify the ffData by which type child is present. Refuses
/// (`MalformedFfData`) when none of textInput/checkBox/ddList is there.
fn classify_ffdata(
    ffdata: &Element,
    field_id: &NodeId,
    step_index: usize,
) -> Result<FfKind, EditError> {
    if child_by_local(ffdata, "textInput").is_some() {
        Ok(FfKind::Text)
    } else if child_by_local(ffdata, "checkBox").is_some() {
        Ok(FfKind::Checkbox)
    } else if child_by_local(ffdata, "ddList").is_some() {
        Ok(FfKind::Dropdown)
    } else {
        Err(EditError::MalformedFfData {
            field_id: field_id.clone(),
            reason: "ffData has no textInput/checkBox/ddList child",
            step_index,
        })
    }
}

/// The ordered `listEntry` @val set of a `w:ddList`.
fn ddlist_entries(ddlist: &Element) -> Vec<String> {
    ddlist
        .children
        .iter()
        .filter_map(|c| match c {
            XMLNode::Element(el) if el.name == "listEntry" => {
                crate::xml_attrs::attr_get(el, "w:val").cloned()
            }
            _ => None,
        })
        .collect()
}

/// Where to insert a freshly-created `set_val_child` element so the parent's
/// children stay in schema order. Real fixtures almost always already carry the
/// element (so this only matters when it is absent).
#[derive(Clone, Copy)]
enum InsertAt {
    /// First child — `CT_FFDDList` is `result?, default?, listEntry*`, so a new
    /// `w:result` goes at the front.
    Front,
    /// Last child — `CT_FFCheckBox` is `(size|sizeAuto), default?, checked?`, so
    /// a new `w:checked` goes at the back.
    Back,
}

/// Set (or insert) a single `w:val`-keyed child element of `parent`, e.g.
/// `<w:checked w:val="true"/>` or `<w:result w:val="2"/>`. Updates the element
/// in place when present; otherwise creates it and inserts it at `at` so the
/// parent's children keep their schema order.
fn set_val_child(parent: &mut Element, local: &str, val: &str, at: InsertAt) {
    if let Some(existing) = child_by_local_mut(parent, local) {
        crate::xml_attrs::attr_set(existing, "w:val", val);
        return;
    }
    let mut el = crate::word_xml::w_el(local);
    crate::xml_attrs::attr_set(&mut el, "w:val", val);
    match at {
        InsertAt::Front => parent.children.insert(0, XMLNode::Element(el)),
        InsertAt::Back => parent.children.push(XMLNode::Element(el)),
    }
}

// ─── The verb ─────────────────────────────────────────────────────────────────

/// What the result run should display after the set, plus the resolved ffData
/// mutation outcome — the bridge between the type-check (which needs the parsed
/// ffData) and the two mutation sites (ffData raw_xml + result-run splice).
struct ResolvedSet {
    /// New display text for the result run, or `None` for FORMCHECKBOX (no run).
    display: Option<String>,
}

/// Apply a `SetFormFieldValue` step. See the module docs for the model and the
/// fail-loud table. Untracked / in-place.
pub(crate) fn apply_set_value(
    doc: &mut CanonDoc,
    block_id: &NodeId,
    field_id: &NodeId,
    value: &FormFieldValue,
    semantic_hash: Option<&str>,
    step_index: usize,
) -> Result<(), EditError> {
    // Locate the hosting paragraph — top-level or inside a (possibly nested)
    // table cell. The field's begin anchor lives in this paragraph's inline flow.
    let path = find_paragraph_path(doc, block_id).ok_or_else(|| EditError::BlockNotFound {
        block_id: block_id.clone(),
        step_index,
    })?;
    let block = block_at_mut(doc, &path);
    let BlockNode::Paragraph(para) = block else {
        // The block_id resolves to a non-paragraph (a table / opaque block); a
        // field begin anchor only ever lives in a paragraph's inline flow.
        return Err(EditError::FormFieldNotFound {
            field_id: field_id.clone(),
            step_index,
        });
    };

    // 1. Locate the field span (immutable borrow of the paragraph).
    let span = locate_form_field_span(para, field_id, step_index)?;

    // 2. Optional semantic-hash precondition on the begin anchor.
    if let Some(expected) = semantic_hash {
        let begin = &para.segments[span.begin.segment].inlines[span.begin.inline];
        let actual = match begin {
            InlineNode::OpaqueInline(o) => o.content_hash.as_deref().unwrap_or(""),
            _ => "",
        };
        if actual != expected {
            return Err(EditError::BlockSemanticHashMismatch {
                block_id: field_id.clone(),
                expected: expected.to_string(),
                actual: actual.to_string(),
                step_index,
            });
        }
    }

    // 3. Mutate the ffData inside the begin anchor's raw_xml; this also
    //    type-checks the value against the field kind and yields the resolved
    //    display text for the result run.
    let begin = &mut para.segments[span.begin.segment].inlines[span.begin.inline];
    let InlineNode::OpaqueInline(opaque) = begin else {
        return Err(EditError::NotAFormField {
            field_id: field_id.clone(),
            step_index,
        });
    };
    let resolved = mutate_ffdata(opaque, value, field_id, step_index)?;

    // 4. Splice the result run (FORMTEXT / FORMDROPDOWN). FORMCHECKBOX has no
    //    result run (`display` is None) and no separate, so nothing to splice.
    //
    //    DECISION — a nested field wholly inside the result region: the result
    //    span is `[separate+1, end)`, so a balanced nested field that sits inside
    //    it (begin..end both within the span) is part of the cached result and is
    //    replaced WHOLE by the new run — its begin AND end go together, so the
    //    markup stays balanced. This is the Word-like semantic: the cached field
    //    result is recomputed wholesale, not surgically edited around a nested
    //    field. (The locator's depth pairing guarantees we found the OUTER end,
    //    so the span is exactly the outer result and never clips a nested field
    //    in half. Tested by `set_outer_field_pairs_correct_end_across_nested_field`.)
    if let Some(display) = resolved.display {
        replace_result_run(para, &span, &display);
        // The paragraph's cached text/hash are now stale.
        para.block_text_hash = None;
        para.rendered_text = None;
    }

    Ok(())
}

/// Mutate the begin anchor's `ffData` per `value`, type-checking against the
/// field kind. Re-serializes the anchor's `raw_xml` and recomputes its
/// `content_hash`. Returns the resolved display text for the result run.
fn mutate_ffdata(
    opaque: &mut crate::domain::OpaqueInlineNode,
    value: &FormFieldValue,
    field_id: &NodeId,
    step_index: usize,
) -> Result<ResolvedSet, EditError> {
    let raw = opaque
        .raw_xml
        .as_deref()
        .ok_or_else(|| EditError::FormFieldMissingRawXml {
            field_id: field_id.clone(),
            step_index,
        })?;
    let mut fld_char = parse_raw_fragment(raw).map_err(|e| EditError::FormFieldRawXmlParse {
        field_id: field_id.clone(),
        reason: e.to_string(),
        step_index,
    })?;
    let ffdata = ffdata_mut(&mut fld_char).ok_or_else(|| EditError::MalformedFfData {
        field_id: field_id.clone(),
        reason: "begin anchor fldChar has no ffData",
        step_index,
    })?;
    let kind = classify_ffdata(ffdata, field_id, step_index)?;

    let display = match (value, kind) {
        (FormFieldValue::Text(text), FfKind::Text) => {
            // FORMTEXT: the live value is the result run; the textInput/default
            // is the fallback when empty and is NOT the live value, so we leave
            // it. The displayed value is the supplied text.
            Some(text.clone())
        }
        (FormFieldValue::Checked(checked), FfKind::Checkbox) => {
            // FORMCHECKBOX: set the legacy `w:checked` (§17.16.8) — NOT the SDT
            // `w14:checked`. No result run.
            let checkbox = child_by_local_mut(ffdata, "checkBox").ok_or_else(|| {
                EditError::MalformedFfData {
                    field_id: field_id.clone(),
                    reason: "checkBox child vanished",
                    step_index,
                }
            })?;
            set_val_child(
                checkbox,
                "checked",
                if *checked { "true" } else { "false" },
                InsertAt::Back,
            );
            None
        }
        (FormFieldValue::Selected(sel), FfKind::Dropdown) => {
            // FORMDROPDOWN: resolve the value to its zero-based index in the
            // ordered listEntry set and write `<w:result w:val="{idx}">`
            // (§17.16.28). Refuse a value not in the list (no silent clamp).
            let ddlist =
                child_by_local_mut(ffdata, "ddList").ok_or_else(|| EditError::MalformedFfData {
                    field_id: field_id.clone(),
                    reason: "ddList child vanished",
                    step_index,
                })?;
            let entries = ddlist_entries(ddlist);
            let index = entries.iter().position(|e| e == sel).ok_or_else(|| {
                EditError::FormFieldValueNotInList {
                    field_id: field_id.clone(),
                    value: sel.clone(),
                    step_index,
                }
            })?;
            set_val_child(ddlist, "result", &index.to_string(), InsertAt::Front);
            // The displayed value is the selected entry's text (legacy listEntry
            // has only a w:val, so display == value).
            Some(sel.clone())
        }
        // Any other (value, kind) pairing is a type mismatch.
        (value, kind) => {
            return Err(EditError::FormFieldTypeMismatch {
                field_id: field_id.clone(),
                requested: value.kind_label(),
                actual: kind.label(),
                step_index,
            });
        }
    };

    let new_raw = serialize_raw_fragment(&fld_char);
    opaque.content_hash = Some(sha256_hex(&new_raw));
    opaque.raw_xml = Some(new_raw);

    Ok(ResolvedSet { display })
}

/// Replace the result-region inlines (`span.result`, all in `Normal` segments)
/// with a single text node carrying `display`. The result positions are
/// strictly between `separate` and `end`; we drop them and insert the new run
/// at the separate-anchor's segment, right after the separate.
fn replace_result_run(para: &mut crate::domain::ParagraphNode, span: &FieldSpan, display: &str) {
    // Remove the existing result inlines. Delete from the highest (segment,
    // inline) first so earlier positions stay valid.
    let mut to_remove = span.result.clone();
    to_remove.sort_by(|a, b| b.segment.cmp(&a.segment).then(b.inline.cmp(&a.inline)));
    for pos in &to_remove {
        para.segments[pos.segment].inlines.remove(pos.inline);
    }

    // Insert the new single result run immediately after the separate anchor.
    // The separate is in a Normal segment (the field anchors are Normal), and
    // removing the result inlines after it did not shift the separate's own
    // index. When there is no separate (checkbox), `display` is None and this
    // function is not called, so `separate` is always Some here.
    let new_run = InlineNode::from(TextNode {
        id: NodeId::from(format!("{}_result", para.id.0)),
        text_role: None,
        text: display.to_string(),
        marks: Vec::new(),
        style_props: crate::domain::StyleProps::default(),
        rpr_authored: RunRprAuthored::default(),
        formatting_change: None,
    });
    if let Some(sep) = span.separate {
        let seg = &mut para.segments[sep.segment];
        let insert_at = (sep.inline + 1).min(seg.inlines.len());
        seg.inlines.insert(insert_at, new_run);
    }
}
