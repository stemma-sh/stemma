//! Serialization / code-generation: IR domain types → Word XML elements.
//!
//! Extracted from `runtime.rs`. These functions convert canonical document
//! types into XML nodes for DOCX output.

use std::collections::{HashMap, HashSet};

use xmltree::{Element, XMLNode};

use crate::domain::{
    Alignment, BlockNode, Border, BorderSet, CanonDoc, CellMargins, CellSdtWrap, CnfStyle,
    FieldData, FieldKind, FormattingChange, FrameProperties, HyperlinkData, HyperlinkRun,
    InlineNode, LineSpacingRule, Mark, MarkValue, NodeId, OpaqueInlineNode, OpaqueKind,
    ParagraphNode, RevisionInfo, Shading, StyleProps, TableCellNode, TableFormatting,
    TableMeasurement, TableNode, TableRowNode, TrackedBlock, TrackedSegment, TrackingStatus,
    VerticalAlignment, VerticalMerge,
};
use crate::runtime::{
    ErrorCode, ErrorDetails, HYPERLINK_REL_TYPE, RuntimeError, build_sdt_wrapper,
    local_element_name, next_annotation_id,
};
use crate::word_xml::{self, w_cell_del, w_cell_ins, w_del, w_del_text, w_el, w_ins};
use crate::xml_attrs::{attr_get, attr_set};

/// Build a `w:framePr` element (§17.3.1.11 CT_FramePr) from `FrameProperties`.
///
/// Emits every modeled attribute that is present, then re-emits the captured
/// `extra_attrs` remainder verbatim. Shared by the live-paragraph pPr and the
/// `w:pPrChange` previous-pPr emit sites so the two never drift.
fn build_frame_pr(fp: &FrameProperties) -> Element {
    let mut frame = w_el("framePr");
    if let Some(w) = fp.width {
        attr_set(&mut frame, "w:w", w.to_string());
    }
    if let Some(h) = fp.height {
        attr_set(&mut frame, "w:h", h.to_string());
    }
    if let Some(ref rule) = fp.h_rule {
        attr_set(&mut frame, "w:hRule", rule.to_xml_str());
    }
    if let Some(hs) = fp.h_space {
        attr_set(&mut frame, "w:hSpace", hs.to_string());
    }
    if let Some(vs) = fp.v_space {
        attr_set(&mut frame, "w:vSpace", vs.to_string());
    }
    if let Some(ref wrap) = fp.wrap {
        attr_set(&mut frame, "w:wrap", wrap.to_xml_str());
    }
    if let Some(ref va) = fp.v_anchor {
        attr_set(&mut frame, "w:vAnchor", va.to_xml_str());
    }
    if let Some(ref ha) = fp.h_anchor {
        attr_set(&mut frame, "w:hAnchor", ha.to_xml_str());
    }
    if let Some(x) = fp.x {
        attr_set(&mut frame, "w:x", x.to_string());
    }
    if let Some(ref xa) = fp.x_align {
        attr_set(&mut frame, "w:xAlign", xa.to_xml_str());
    }
    if let Some(y) = fp.y {
        attr_set(&mut frame, "w:y", y.to_string());
    }
    if let Some(ref ya) = fp.y_align {
        attr_set(&mut frame, "w:yAlign", ya.to_xml_str());
    }
    for (qname, value) in &fp.extra_attrs {
        attr_set(&mut frame, qname, value);
    }
    frame
}

/// AUTHORED stops re-emit verbatim (page-absolute, `Clear` entries included).
/// No synthetic prefix stop and no dedup: the stop a literal-prefix tab landed
/// on is still IN `tab_stops` when the document authored it, and when the tab
/// landed on a style/default-grid stop the pPr must stay silent about it —
/// materializing it would freeze inheritance and churn untouched markup. (The
/// old body-left-relative model re-added a synthetic absolute stop next to
/// relative values and deduped the collisions — the tab-stop loss family.)
fn serialized_tab_stops_for_paragraph(
    paragraph: &ParagraphNode,
) -> Vec<crate::word_ir::TabStopDef> {
    paragraph.tab_stops.clone()
}

fn serialized_tab_stops_for_previous_formatting(
    fc: &crate::domain::ParagraphFormattingChange,
) -> Vec<crate::word_ir::TabStopDef> {
    fc.previous_tab_stops.clone()
}

/// A bare run carrying a deferred literal-prefix separator VERBATIM (spaces and
/// tabs in source order). Used when the run the separator would normally attach
/// to is not a text run (hard break, opaque inline).
fn build_separator_run(separator: &str) -> Element {
    let mut run = w_el("r");
    tabbed_text_children(&mut run, "", false, Some(separator));
    run
}

#[allow(clippy::too_many_arguments)]
fn append_literal_prefix_runs(
    parent: &mut Element,
    prefix: &str,
    leading_ws: &str,
    trailing_ws: &str,
    has_trailing_tab: bool,
    embed_trailing_tab_in_prefix_run: bool,
    leading_rpr: Option<&crate::domain::PrefixLeadingRpr>,
    trailing_rpr: Option<&crate::domain::PrefixLeadingRpr>,
    marks: &[Mark],
    style_props: &StyleProps,
    directness: RunDirectness,
    deleted_text: bool,
    next_id: &mut u32,
) {
    // The leading whitespace/tab run authored its OWN rPr (a tab-only run
    // preceding the label): emit it as its own run wearing that formatting —
    // folding it into the label run would silently swap its authored rPr for
    // the label's (the SAFE-template w:b / rFonts loss).
    let (leading_ws, lead_emitted) = match leading_rpr {
        Some(lead) if !leading_ws.is_empty() => {
            parent
                .children
                .push(XMLNode::Element(build_text_run_with_leading_tabs(
                    leading_ws,
                    &lead.marks,
                    &lead.style_props,
                    lead.rpr_authored,
                    deleted_text,
                    None,
                    next_id,
                    None,
                )));
            ("", true)
        }
        _ => (leading_ws, false),
    };
    let _ = lead_emitted;
    // Re-emit the stripped whitespace VERBATIM (XML 1.0 §2.10): the leading
    // whitespace (spaces and tabs, in source order) goes inside the prefix run
    // before the label, and `build_text_run_with_leading_tabs` splits it on `\t`
    // (each tab → `<w:tab/>`, spaces → `<w:t xml:space="preserve">`). When the
    // trailing tab is embedded, append the (tab-bearing) trailing whitespace too.
    // A trailing separator with its OWN authored rPr never embeds or defers —
    // it re-emits as its own run below (the trailing twin of leading_rpr).
    let embed_trailing_tab_in_prefix_run =
        embed_trailing_tab_in_prefix_run && trailing_rpr.is_none();
    let prefix_text = if has_trailing_tab && embed_trailing_tab_in_prefix_run {
        // Use the captured trailing whitespace verbatim if it carries the tab;
        // fall back to a lone tab for legacy models without it.
        let trailing = if trailing_ws.contains('\t') {
            trailing_ws.to_string()
        } else {
            "\t".to_string()
        };
        format!("{leading_ws}{prefix}{trailing}")
    } else {
        format!("{leading_ws}{prefix}")
    };
    parent
        .children
        .push(XMLNode::Element(build_text_run_with_leading_tabs(
            &prefix_text,
            marks,
            style_props,
            // The stripped prefix run is a real run: emit ONLY the slots it
            // authored (per its captured provenance), like any TextNode. Emitting
            // an inherited theme font / themeColor here would bake the cascade
            // onto the prefix and (winning per §17.3.2.26) change its rendering.
            directness,
            deleted_text,
            None,
            next_id,
            None,
        )));
    if let Some(tr) = trailing_rpr {
        if !trailing_ws.is_empty() {
            parent
                .children
                .push(XMLNode::Element(build_text_run_with_leading_tabs(
                    trailing_ws,
                    &tr.marks,
                    &tr.style_props,
                    tr.rpr_authored,
                    deleted_text,
                    None,
                    next_id,
                    None,
                )));
        }
        return;
    }
    if !has_trailing_tab {
        // The separator whitespace is part of the SAME prefix run formatting:
        // project it through the prefix's provenance too, or it would
        // re-introduce the inherited theme font / themeColor the prefix run
        // itself suppresses. Emit it VERBATIM (the prior model collapsed any run
        // of separator spaces to a single space, losing significant whitespace).
        // A legacy model with no captured separator falls back to one space, the
        // historical behavior.
        let separator = if trailing_ws.is_empty() {
            " ".to_string()
        } else {
            trailing_ws.to_string()
        };
        parent
            .children
            .push(XMLNode::Element(build_text_run_with_leading_tabs(
                &separator,
                marks,
                style_props,
                directness,
                deleted_text,
                None,
                next_id,
                None,
            )));
    }
}

fn ensure_prefix_trailing_tab_consumed(
    pending_prefix_tab: bool,
    block_id: &crate::domain::NodeId,
) -> Result<(), RuntimeError> {
    if pending_prefix_tab {
        return Err(RuntimeError {
            code: ErrorCode::InvalidDocx,
            message: "literal prefix trailing tab had no body run to attach to".to_string(),
            details: ErrorDetails {
                block_id: Some(block_id.clone()),
                context: Some(
                    "paragraph.literal_prefix_has_trailing_tab=true but no following text run was serialized"
                        .to_string(),
                ),
                ..ErrorDetails::default()
            },
        });
    }
    Ok(())
}

fn tabbed_text_children(
    parent: &mut Element,
    text: &str,
    deleted_text: bool,
    leading_separator: Option<&str>,
) {
    // A deferred literal-prefix separator is re-emitted VERBATIM ahead of the
    // text (spaces and tabs in source order — discretizing it to a lone
    // `<w:tab/>` loses separator spaces and extra tabs). It flows through the
    // same `split('\t')` emission as the text itself.
    let combined;
    let text = match leading_separator {
        Some(sep) => {
            combined = format!("{sep}{text}");
            combined.as_str()
        }
        None => text,
    };

    let mut first = true;
    for part in text.split('\t') {
        if !first {
            parent.children.push(XMLNode::Element(w_el("tab")));
        }
        if !part.is_empty() {
            if deleted_text {
                parent.children.push(XMLNode::Element(w_del_text(part)));
            } else {
                let mut t = w_el("t");
                if part.starts_with(' ') || part.ends_with(' ') {
                    attr_set(&mut t, "xml:space", "preserve");
                }
                t.children.push(XMLNode::Text(part.to_string()));
                parent.children.push(XMLNode::Element(t));
            }
        }
        first = false;
    }
}

/// The serializer's view of run-rPr provenance is exactly the model's per-slot
/// [`crate::domain::RunRprAuthored`]: was each property set by DIRECT run rPr (`true`) or merely
/// INHERITED through the style cascade (`false`)?
///
/// `TextNode.style_props` holds the fully-RESOLVED effective props (direct →
/// char-style → para-style → docDefaults, collapsed at import). The serializer
/// must only emit a property as DIRECT `<w:rPr>` when it was authored directly;
/// emitting an inherited value as direct rPr would bake the cascade into the run
/// and (for theme attrs / themeColor, which WIN per §17.3.2.26) change rendering.
type RunDirectness = crate::domain::RunRprAuthored;

/// Project a run's resolved `style_props` down to the props that should be
/// emitted as DIRECT `<w:rPr>`, nulling EACH slot the run merely inherited (per
/// its [`crate::domain::RunRprAuthored`]). Props with no separate provenance flag (bold, run
/// shading, …) pass through unchanged — the bug is scoped to the cascade-injected
/// slots: fonts (literal + theme, all four script slots + hint), sizes (sz/szCs),
/// color (literal + theme), and lang (val + eastAsia).
fn direct_run_style_props(style_props: &StyleProps, directness: RunDirectness) -> StyleProps {
    let mut props = style_props.clone();
    // ascii/hAnsi font slot: literal (w:ascii) and theme (w:asciiTheme) are
    // independent — a run authoring a literal font must NOT get a theme font
    // injected (the theme attr would WIN and change the rendered font).
    if !directness.font_family {
        props.font_family = None;
    }
    if !directness.font_family_theme {
        props.font_family_theme = None;
    }
    if !directness.font_east_asia {
        props.font_east_asia = None;
    }
    if !directness.font_east_asia_theme {
        props.font_east_asia_theme = None;
    }
    if !directness.font_cs {
        props.font_cs = None;
    }
    if !directness.font_cs_theme {
        props.font_cs_theme = None;
    }
    if !directness.font_hint {
        props.font_hint = None;
    }
    if !directness.font_size {
        props.font_size = None;
    }
    if !directness.font_size_cs {
        props.font_size_cs = None;
    }
    // color: literal/auto (w:val) vs theme (w:themeColor) tracked separately —
    // an authored `w:val="auto"` must not get a themeColor injected (themeColor
    // would WIN and render e.g. dark blue instead of black).
    if !directness.color {
        props.color = None;
    }
    if !directness.color_theme {
        props.color_theme = None;
    }
    if !directness.lang {
        props.lang = None;
    }
    if !directness.lang_east_asia {
        props.lang_east_asia = None;
    }
    // kern/spacing carry no precedence inversion; stripping unauthored values is
    // churn hygiene — an inherited kerning threshold or character spacing must
    // not be materialized as direct rPr on untouched runs.
    if !directness.kern {
        props.kern = None;
    }
    if !directness.char_spacing {
        props.char_spacing = None;
    }
    // Toggle marks (H8 directness gap): an inherited toggle re-emitted as
    // direct rPr both churns untouched markup AND flips rendering when the
    // style itself toggles it (§17.7.3 toggle semantics). Tri-states reset to
    // Inherit; value props to None.
    if !directness.strike {
        props.strike = crate::domain::MarkValue::Inherit;
    }
    if !directness.double_strike {
        props.double_strike = crate::domain::MarkValue::Inherit;
    }
    if !directness.caps {
        props.caps = crate::domain::MarkValue::Inherit;
    }
    if !directness.small_caps {
        props.small_caps = crate::domain::MarkValue::Inherit;
    }
    if !directness.vanish {
        props.vanish = crate::domain::MarkValue::Inherit;
    }
    if !directness.web_hidden {
        props.web_hidden = crate::domain::MarkValue::Inherit;
    }
    if !directness.emboss {
        props.emboss = crate::domain::MarkValue::Inherit;
    }
    if !directness.imprint {
        props.imprint = crate::domain::MarkValue::Inherit;
    }
    if !directness.outline {
        props.outline = crate::domain::MarkValue::Inherit;
    }
    if !directness.shadow {
        props.shadow = crate::domain::MarkValue::Inherit;
    }
    if !directness.bold_cs {
        props.bold_cs = crate::domain::MarkValue::Inherit;
    }
    if !directness.italic_cs {
        props.italic_cs = crate::domain::MarkValue::Inherit;
    }
    if !directness.rtl {
        props.rtl = crate::domain::MarkValue::Inherit;
    }
    if !directness.cs {
        props.cs = crate::domain::MarkValue::Inherit;
    }
    if !directness.no_proof {
        props.no_proof = crate::domain::MarkValue::Inherit;
    }
    if !directness.spec_vanish {
        props.spec_vanish = crate::domain::MarkValue::Inherit;
    }
    if !directness.o_math {
        props.o_math = crate::domain::MarkValue::Inherit;
    }
    if !directness.snap_to_grid {
        props.snap_to_grid = crate::domain::MarkValue::Inherit;
    }
    if !directness.highlight {
        props.highlight = None;
    }
    if !directness.underline_style {
        props.underline_style = None;
    }
    if !directness.position {
        props.position = None;
    }
    if !directness.char_width_scaling {
        props.char_width_scaling = None;
    }
    if !directness.char_style_id {
        props.char_style_id = None;
    }
    if !directness.run_border {
        props.run_border = None;
    }
    if !directness.run_shading {
        props.run_shading = None;
    }
    if !directness.emphasis_mark {
        props.emphasis_mark = None;
    }
    if !directness.text_effect {
        props.text_effect = None;
    }
    if !directness.fit_text {
        props.fit_text = None;
    }
    props
}

/// Emit AUTHORED-OFF forms: a run whose own rPr carried `<w:b w:val="0"/>`
/// resolves to not-bold, so `Vec<Mark>` (presence-only) cannot carry it — the
/// explicit `bold_off`/`italic_off`/`underline_off` provenance flags carry the
/// authored OFF (absence of the resolved mark is NOT a sound signal: complex-
/// script runs resolve b/i via bCs/iCs, and an unset underline is
/// indistinguishable from an explicit none). Dropping the off-form would let a
/// style-level toggle or underline bleed back in (§17.7.3, §17.3.2.40).
/// Inserted at the Annex-A position via the validator's RPR_ORDER.
fn append_authored_off_toggles(rpr: &mut Element, marks: &[Mark], directness: RunDirectness) {
    if directness == RunDirectness::ALL {
        // ALL emits what is present; it never fabricates ON or OFF forms.
        return;
    }
    let order = crate::docx_validate_ordering::RPR_ORDER;
    let insert_ordered = |rpr: &mut Element, el: Element, name: &str| {
        let idx = order.iter().position(|n| *n == name);
        let insert_at = match idx {
            Some(idx) => rpr
                .children
                .iter()
                .position(|c| match c {
                    XMLNode::Element(existing) => {
                        order
                            .iter()
                            .position(|n| *n == existing.name.as_str())
                            .unwrap_or(usize::MAX)
                            > idx
                    }
                    _ => false,
                })
                .unwrap_or(rpr.children.len()),
            None => rpr.children.len(),
        };
        rpr.children.insert(insert_at, XMLNode::Element(el));
    };
    for (authored, authored_off, mark, name) in [
        (directness.bold, directness.bold_off, Mark::Bold, "b"),
        (directness.italic, directness.italic_off, Mark::Italic, "i"),
    ] {
        // Authored ON whose resolved mark is absent (complex-script runs
        // resolve b/i via bCs/iCs): restore the bare ON form.
        let missing_on = authored && !authored_off && !marks.contains(&mark);
        if authored_off || missing_on {
            let mut el = w_el(name);
            if authored_off {
                attr_set(&mut el, "w:val", "0");
            }
            insert_ordered(rpr, el, name);
        }
    }
    // Underline is a simple override (CT_Underline, §17.3.2.40), not a XOR
    // toggle: an authored `<w:u w:val="none"/>` cancels an inherited underline
    // and re-emits verbatim. The ON form (`marks` carries Mark::Underline) is
    // written by build_rpr itself, so only the OFF form is synthesized here.
    if directness.underline_off {
        let mut u = w_el("u");
        attr_set(&mut u, "w:val", "none");
        insert_ordered(rpr, u, "u");
    }
}

/// Project a run's resolved `marks` down to the AUTHORED ones (per the same
/// [`RunRprAuthored`] provenance): a style-inherited Bold/Italic/Underline/
/// vertAlign must not re-emit as direct rPr — see `direct_run_style_props`.
fn direct_marks(marks: &[Mark], directness: RunDirectness) -> Vec<Mark> {
    marks
        .iter()
        .filter(|m| match m {
            // Authored-driven: a mark whose presence came from resolution
            // (e.g. iCs governing a complex-script run) must not emit when the
            // run's own rPr said OFF.
            Mark::Bold => directness.bold && !directness.bold_off,
            Mark::Italic => directness.italic && !directness.italic_off,
            Mark::Underline => directness.underline,
            Mark::Subscript | Mark::Superscript => directness.vert_align,
        })
        .cloned()
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn build_text_run_with_leading_tabs(
    text: &str,
    marks: &[Mark],
    style_props: &StyleProps,
    directness: RunDirectness,
    deleted_text: bool,
    formatting_change: Option<&FormattingChange>,
    next_id: &mut u32,
    leading_separator: Option<&str>,
) -> Element {
    let mut run = w_el("r");
    let has_rpr_change = formatting_change.is_some();
    let direct_props = direct_run_style_props(style_props, directness);
    let resolved_marks = marks;
    let marks = direct_marks(marks, directness);
    let has_off_toggles = (directness.bold
        && (directness.bold_off || !resolved_marks.contains(&Mark::Bold)))
        || (directness.italic
            && (directness.italic_off || !resolved_marks.contains(&Mark::Italic)))
        || directness.underline_off;
    let has_style_props = !direct_props.is_empty();
    if !marks.is_empty() || has_rpr_change || has_style_props || has_off_toggles {
        let mut rpr = build_rpr(&marks, &direct_props);
        append_authored_off_toggles(&mut rpr, resolved_marks, directness);
        if let Some(fc) = formatting_change {
            let mut rpr_change = w_el("rPrChange");
            attr_set(
                &mut rpr_change,
                "w:id",
                if fc.revision_id != 0 {
                    fc.revision_id.to_string()
                } else {
                    next_annotation_id(next_id).to_string()
                },
            );
            attr_set(&mut rpr_change, "w:author", fc.author.clone());
            if let Some(ref date) = fc.date {
                attr_set(&mut rpr_change, "w:date", date.clone());
            }
            // The previous-props snapshot is a run rPr too: apply the same
            // direct-ness projection so the "before" state is recorded with the
            // same inherited-vs-direct semantics as the current props.
            // Previous-state snapshot: same directness PROJECTION as the live
            // rPr, but NO toggle synthesis — bold_off/missing-ON describe the
            // CURRENT run's authored state, not the pre-change state, and
            // synthesizing them here would fabricate history (a phantom
            // <w:b/> in the rPrChange baseline).
            let prev_rpr = build_rpr(
                &direct_marks(&fc.previous_marks, directness),
                &direct_run_style_props(&fc.previous_style_props, directness),
            );
            rpr_change.children.push(XMLNode::Element(prev_rpr));
            rpr.children.push(XMLNode::Element(rpr_change));
        }
        run.children.push(XMLNode::Element(rpr));
    }
    tabbed_text_children(&mut run, text, deleted_text, leading_separator);
    run
}

/// Build a run's DIRECT `<w:rPr>`: the run properties Word would serialize for
/// this run, carrying ONLY the slots it authored (per `directness`) with the
/// style-inherited cascade values suppressed, plus any authored-OFF toggles
/// (`<w:b w:val="0"/>`) re-synthesized. No `rPrChange` child is emitted.
///
/// Besides being the natural factoring of the run serializer's own rPr step,
/// this is consumed by accept/reject style re-resolution
/// (`crate::import::reresolve_run_style_props`): parsing the element back into
/// `word_ir::TextMarks` recovers the run's DIRECT marks, which are then run
/// through the SAME `StyleDefinitions::resolve` cascade import uses against a
/// reverted paragraph style — so the two paths cannot drift apart.
pub(crate) fn build_run_direct_rpr(
    marks: &[Mark],
    style_props: &StyleProps,
    directness: RunDirectness,
) -> Element {
    let mut rpr = build_rpr(
        &direct_marks(marks, directness),
        &direct_run_style_props(style_props, directness),
    );
    append_authored_off_toggles(&mut rpr, marks, directness);
    rpr
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn append_inline_refs_to_container(
    parent: &mut Element,
    inlines: &[&InlineNode],
    deleted_text: bool,
    next_id: &mut u32,
    bookmark_policy: &BookmarkIdPolicy,
    origin: &str,
    mut resolve_rel_rid: Option<&mut dyn FnMut(&str, &str) -> String>,
    pending_prefix_sep: &mut Option<String>,
) -> Result<(), RuntimeError> {
    for &inline in inlines {
        let resolver = resolve_rel_rid
            .as_mut()
            .map(|resolver| &mut **resolver as &mut dyn FnMut(&str, &str) -> String);
        append_single_inline(
            parent,
            inline,
            deleted_text,
            next_id,
            bookmark_policy,
            origin,
            resolver,
            pending_prefix_sep,
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn append_inlines_to_container(
    parent: &mut Element,
    inlines: &[InlineNode],
    deleted_text: bool,
    next_id: &mut u32,
    bookmark_policy: &BookmarkIdPolicy,
    origin: &str,
    mut resolve_rel_rid: Option<&mut dyn FnMut(&str, &str) -> String>,
    pending_prefix_sep: &mut Option<String>,
) -> Result<(), RuntimeError> {
    for inline in inlines {
        let resolver = resolve_rel_rid
            .as_mut()
            .map(|resolver| &mut **resolver as &mut dyn FnMut(&str, &str) -> String);
        append_single_inline(
            parent,
            inline,
            deleted_text,
            next_id,
            bookmark_policy,
            origin,
            resolver,
            pending_prefix_sep,
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn append_single_inline(
    parent: &mut Element,
    inline: &InlineNode,
    deleted_text: bool,
    next_id: &mut u32,
    bookmark_policy: &BookmarkIdPolicy,
    origin: &str,
    resolve_rel_rid: Option<&mut dyn FnMut(&str, &str) -> String>,
    pending_prefix_sep: &mut Option<String>,
) -> Result<(), RuntimeError> {
    match inline {
        InlineNode::Text(text) => {
            let sep = pending_prefix_sep.take();
            let run = build_text_run_with_leading_tabs(
                &text.text,
                &text.marks,
                &text.style_props,
                text.rpr_authored,
                deleted_text,
                text.formatting_change.as_ref(),
                next_id,
                sep.as_deref(),
            );
            parent.children.push(XMLNode::Element(run));
        }
        InlineNode::HardBreak(hb) => {
            if let Some(sep) = pending_prefix_sep.take() {
                parent
                    .children
                    .push(XMLNode::Element(build_separator_run(&sep)));
            }
            let mut run = w_el("r");
            let mut br = w_el("br");
            match hb.break_type {
                crate::domain::BreakType::Page => {
                    attr_set(&mut br, "w:type", "page");
                }
                crate::domain::BreakType::Column => {
                    attr_set(&mut br, "w:type", "column");
                }
                crate::domain::BreakType::TextWrapping => {}
            }
            run.children.push(XMLNode::Element(br));
            parent.children.push(XMLNode::Element(run));
        }
        InlineNode::OpaqueInline(opaque) => {
            if let Some(sep) = pending_prefix_sep.take() {
                parent
                    .children
                    .push(XMLNode::Element(build_separator_run(&sep)));
            }
            if let Some(raw_xml) = &opaque.raw_xml {
                let mut element =
                    crate::word_xml::parse_raw_fragment(raw_xml.as_slice()).map_err(|source| {
                        RuntimeError {
                            code: ErrorCode::InvalidDocx,
                            message: "failed to parse opaque inline XML".to_string(),
                            details: ErrorDetails {
                                context: Some(format!(
                                    "opaque_ref={} err={source}",
                                    opaque.opaque_ref
                                )),
                                ..ErrorDetails::default()
                            },
                        }
                    })?;
                // Coerce the opaque's own descendant run content to match the
                // container it is emitted into: inside a w:del it must be
                // w:delText / w:delInstrText (I-TC-001), and as plain content
                // (a restored/rejected opaque, or a w:moveFrom) it must be
                // w:t / w:instrText. `deleted_text` is true only for the w:del
                // container, not w:moveFrom. Running BOTH directions keeps a
                // w:delInstrText captured from a since-rejected w:del from
                // leaking into a non-deleted run (schema-invalid; Word repairs).
                coerce_opaque_run_text(&mut element, deleted_text);
                if opaque_raw_element_requires_run_wrapper(&element) {
                    let mut run = w_el("r");
                    if let Some(rpr) = build_wrapper_rpr(
                        &opaque.wrapper_marks,
                        &opaque.wrapper_style_props,
                        &opaque.kind,
                    ) {
                        run.children.push(XMLNode::Element(rpr));
                    }
                    run.children.push(XMLNode::Element(element));
                    parent.children.push(XMLNode::Element(run));
                } else {
                    parent.children.push(XMLNode::Element(element));
                }
            } else if let OpaqueKind::Hyperlink(data) = &opaque.kind {
                let mut resolved = data.clone();
                if let (Some(url), Some(resolver)) = (&data.url, resolve_rel_rid) {
                    resolved.r_id = Some(resolver(url, HYPERLINK_REL_TYPE));
                }
                parent
                    .children
                    .push(XMLNode::Element(build_hyperlink_element(&resolved)));
            } else if let OpaqueKind::Field(data) = &opaque.kind {
                if data.field_kind == FieldKind::Simple {
                    parent
                        .children
                        .push(XMLNode::Element(build_simple_field_element(
                            data,
                            &opaque.wrapper_marks,
                            &opaque.wrapper_style_props,
                            next_id,
                        )));
                } else {
                    return Err(RuntimeError {
                        code: ErrorCode::UnsupportedEdit,
                        message: "field opaque without raw XML cannot be serialized".to_string(),
                        details: ErrorDetails {
                            block_id: Some(opaque.id.clone()),
                            context: Some(format!("opaque_ref={}", opaque.opaque_ref)),
                            ..ErrorDetails::default()
                        },
                    });
                }
            } else if let OpaqueKind::FootnoteReference(data) | OpaqueKind::EndnoteReference(data) =
                &opaque.kind
            {
                // Authored note reference (raw_xml: None). Rebuild
                // `<w:footnoteReference w:id="N"/>` / `<w:endnoteReference
                // w:id="N"/>` from the link id, wrapped in `w:r` with the
                // FootnoteReference/EndnoteReference rStyle — same pattern as
                // build_simple_field_element. (§17.11.3 / §17.11.7.)
                let tag = match &opaque.kind {
                    OpaqueKind::FootnoteReference(_) => "footnoteReference",
                    OpaqueKind::EndnoteReference(_) => "endnoteReference",
                    _ => unreachable!("matched a note reference above"),
                };
                let mut reference = w_el(tag);
                attr_set(&mut reference, "w:id", data.reference_id.clone());
                let mut run = w_el("r");
                if let Some(rpr) = build_wrapper_rpr(
                    &opaque.wrapper_marks,
                    &opaque.wrapper_style_props,
                    &opaque.kind,
                ) {
                    run.children.push(XMLNode::Element(rpr));
                }
                run.children.push(XMLNode::Element(reference));
                parent.children.push(XMLNode::Element(run));
            } else {
                return Err(RuntimeError {
                    code: ErrorCode::UnsupportedEdit,
                    message: "opaque inline without raw XML cannot be serialized".to_string(),
                    details: ErrorDetails {
                        block_id: Some(opaque.id.clone()),
                        context: Some(format!("opaque_ref={}", opaque.opaque_ref)),
                        ..ErrorDetails::default()
                    },
                });
            }
        }
        InlineNode::Decoration(deco) => {
            if let Some(raw_xml) = &deco.raw_xml {
                let mut element =
                    crate::word_xml::parse_raw_fragment(raw_xml.as_slice()).map_err(|source| {
                        RuntimeError {
                            code: ErrorCode::InvalidDocx,
                            message: "failed to parse decoration raw XML".to_string(),
                            details: ErrorDetails {
                                context: Some(format!(
                                    "opaque_ref={} err={source}",
                                    deco.opaque_ref
                                )),
                                ..ErrorDetails::default()
                            },
                        }
                    })?;
                let effective_origin = deco.origin.as_deref().unwrap_or(origin);
                if apply_decoration_id_policy(&mut element, bookmark_policy, effective_origin)?
                    == DecorationEmit::Skip
                {
                    return Ok(());
                }
                if decoration_requires_run_wrapper(&element) {
                    let mut run = w_el("r");
                    // Restore the host run's rPr (note-reference character style,
                    // fonts, size) captured at import. Without it the synthesized
                    // wrapper collapses to a bare `<w:r>` and the footnote/endnote
                    // auto-number reverts to the default style on every rebuild of
                    // the story — a silent, text-invisible fidelity loss.
                    if !deco.wrapper_marks.is_empty() || !deco.wrapper_style_props.is_empty() {
                        run.children.push(XMLNode::Element(build_rpr(
                            &deco.wrapper_marks,
                            &deco.wrapper_style_props,
                        )));
                    }
                    run.children.push(XMLNode::Element(element));
                    parent.children.push(XMLNode::Element(run));
                } else {
                    parent.children.push(XMLNode::Element(element));
                }
            } else {
                return Err(RuntimeError {
                    code: ErrorCode::UnsupportedEdit,
                    message: "decoration without raw XML cannot be serialized".to_string(),
                    details: ErrorDetails {
                        context: Some(format!("opaque_ref={}", deco.opaque_ref)),
                        ..ErrorDetails::default()
                    },
                });
            }
        }
        InlineNode::CommentRangeStart { id } => {
            let mut el = w_el("commentRangeStart");
            attr_set(&mut el, "w:id", id.clone());
            parent.children.push(XMLNode::Element(el));
        }
        InlineNode::CommentRangeEnd { id } => {
            let mut el = w_el("commentRangeEnd");
            attr_set(&mut el, "w:id", id.clone());
            parent.children.push(XMLNode::Element(el));
        }
        InlineNode::CommentReference { id } => {
            let mut run = w_el("r");
            let mut reference = w_el("commentReference");
            attr_set(&mut reference, "w:id", id.clone());
            run.children.push(XMLNode::Element(reference));
            parent.children.push(XMLNode::Element(run));
        }
    }
    Ok(())
}

fn build_simple_field_element(
    data: &FieldData,
    wrapper_marks: &[Mark],
    wrapper_style_props: &StyleProps,
    next_id: &mut u32,
) -> Element {
    let mut field = w_el("fldSimple");
    // Prefer the canonical instruction text reconstructed from the typed
    // semantic — whitespace gets normalized and the on-the-wire form is
    // stable across imports. Fall back to the raw fragment for fields the
    // parser couldn't classify.
    let instruction_text = data
        .semantic
        .as_ref()
        .map(|s| s.to_instruction_text())
        .or_else(|| data.instruction_text.clone());
    if let Some(text) = instruction_text {
        attr_set(&mut field, "w:instr", text);
    }
    if let Some(result_text) = &data.result_text
        && !result_text.is_empty()
    {
        field.children.push(XMLNode::Element(build_text_run(
            result_text,
            wrapper_marks,
            wrapper_style_props,
            false,
            None,
            next_id,
        )));
    }
    field
}

fn paragraph_mark_formatting_changed(
    paragraph: &ParagraphNode,
    fc: &crate::domain::ParagraphFormattingChange,
) -> bool {
    paragraph.paragraph_mark_marks != fc.previous_paragraph_mark_marks
        || paragraph.paragraph_mark_style_props != fc.previous_paragraph_mark_style_props
        || paragraph.paragraph_mark_rpr_off != fc.previous_paragraph_mark_rpr_off
}

/// Translate a paragraph mark's authored OFF toggles into the `RunRprAuthored`
/// shape `append_authored_off_toggles` consumes. Only the OFF flags are set (the
/// ON forms already live in `paragraph_mark_marks` and are emitted by `build_rpr`);
/// the result is deliberately NOT `RunRprAuthored::ALL`, so the emitter runs.
fn para_mark_off_directness(off: crate::domain::ParaMarkRprOff) -> RunDirectness {
    RunDirectness {
        bold_off: off.bold_off,
        italic_off: off.italic_off,
        underline_off: off.underline_off,
        ..RunDirectness::default()
    }
}

fn build_paragraph_mark_rpr(paragraph: &ParagraphNode, next_id: &mut u32) -> Option<Element> {
    let off = paragraph.paragraph_mark_rpr_off;
    let has_current = !paragraph.paragraph_mark_marks.is_empty()
        || !paragraph.paragraph_mark_style_props.is_empty()
        || off != crate::domain::ParaMarkRprOff::default();
    let formatting_change = paragraph
        .formatting_change
        .as_ref()
        .filter(|fc| paragraph_mark_formatting_changed(paragraph, fc));

    if !has_current && formatting_change.is_none() {
        return None;
    }

    let mut rpr = build_rpr(
        &paragraph.paragraph_mark_marks,
        &paragraph.paragraph_mark_style_props,
    );
    // The pilcrow's authored OFF toggles (`<w:b w:val="0"/>`, `<w:i w:val="0"/>`,
    // `<w:u w:val="none"/>`) that the presence-only `paragraph_mark_marks` cannot
    // carry — re-emitted at their Annex-A position, the same path runs use.
    append_authored_off_toggles(
        &mut rpr,
        &paragraph.paragraph_mark_marks,
        para_mark_off_directness(off),
    );
    if let Some(fc) = formatting_change {
        let mut rpr_change = w_el("rPrChange");
        attr_set(
            &mut rpr_change,
            "w:id",
            if fc.revision_id != 0 {
                fc.revision_id.to_string()
            } else {
                next_annotation_id(next_id).to_string()
            },
        );
        attr_set(&mut rpr_change, "w:author", fc.author.clone());
        if let Some(ref date) = fc.date {
            attr_set(&mut rpr_change, "w:date", date.clone());
        }
        let mut prev_rpr = build_rpr(
            &fc.previous_paragraph_mark_marks,
            &fc.previous_paragraph_mark_style_props,
        );
        append_authored_off_toggles(
            &mut prev_rpr,
            &fc.previous_paragraph_mark_marks,
            para_mark_off_directness(fc.previous_paragraph_mark_rpr_off),
        );
        rpr_change.children.push(XMLNode::Element(prev_rpr));
        rpr.children.push(XMLNode::Element(rpr_change));
    }

    Some(rpr)
}

// =============================================================================
// Block-level serialization entry points
// =============================================================================

pub(crate) fn collect_tracked_change_authors(doc: &CanonDoc) -> Vec<String> {
    let mut authors = HashSet::new();

    fn authors_from_status(status: &TrackingStatus, out: &mut HashSet<String>) {
        match status {
            TrackingStatus::Inserted(rev) | TrackingStatus::Deleted(rev) => {
                if let Some(ref author) = rev.author {
                    out.insert(author.clone());
                }
            }
            // Both revisions of a stacked segment carry visible attribution.
            TrackingStatus::InsertedThenDeleted(sr) => {
                if let Some(ref author) = sr.inserted.author {
                    out.insert(author.clone());
                }
                if let Some(ref author) = sr.deleted.author {
                    out.insert(author.clone());
                }
            }
            TrackingStatus::Normal => {}
        }
    }

    fn authors_from_paragraph(p: &ParagraphNode, out: &mut HashSet<String>) {
        if let Some(ref status) = p.para_mark_status {
            authors_from_status(status, out);
        }
        for seg in &p.segments {
            authors_from_status(&seg.status, out);
        }
        if let Some(ref fc) = p.formatting_change {
            out.insert(fc.author.clone());
        }
    }

    fn authors_from_block(block: &BlockNode, out: &mut HashSet<String>) {
        match block {
            BlockNode::Paragraph(p) => authors_from_paragraph(p, out),
            BlockNode::Table(t) => {
                for row in &t.rows {
                    if let Some(ref ts) = row.tracking_status {
                        authors_from_status(ts, out);
                    }
                    for cell in &row.cells {
                        for b in &cell.blocks {
                            authors_from_block(b, out);
                        }
                    }
                }
            }
            BlockNode::OpaqueBlock(_) => {}
        }
    }

    fn authors_from_tracked_blocks(blocks: &[TrackedBlock], out: &mut HashSet<String>) {
        for tb in blocks {
            authors_from_status(&tb.status, out);
            authors_from_block(&tb.block, out);
        }
    }

    authors_from_tracked_blocks(&doc.blocks, &mut authors);
    for h in &doc.headers {
        authors_from_tracked_blocks(&h.blocks, &mut authors);
    }
    for f in &doc.footers {
        authors_from_tracked_blocks(&f.blocks, &mut authors);
    }
    for n in &doc.footnotes {
        authors_from_tracked_blocks(&n.blocks, &mut authors);
    }
    for n in &doc.endnotes {
        authors_from_tracked_blocks(&n.blocks, &mut authors);
    }
    for c in &doc.comments {
        authors_from_tracked_blocks(&c.blocks, &mut authors);
    }

    let mut sorted: Vec<String> = authors.into_iter().collect();
    sorted.sort();
    sorted
}

/// Build the XML content for word/people.xml.
pub(crate) fn build_people_xml(authors: &[String]) -> String {
    let mut xml = String::from(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#);
    xml.push_str(
        r#"<w15:people xmlns:wpc="http://schemas.microsoft.com/office/word/2010/wordprocessingCanvas" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:o="urn:schemas-microsoft-com:office:office" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math" xmlns:v="urn:schemas-microsoft-com:vml" xmlns:wp14="http://schemas.microsoft.com/office/word/2010/wordprocessingDrawing" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:w10="urn:schemas-microsoft-com:office:word" xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" xmlns:w15="http://schemas.microsoft.com/office/word/2012/wordml" xmlns:wpg="http://schemas.microsoft.com/office/word/2010/wordprocessingGroup" xmlns:wpi="http://schemas.microsoft.com/office/word/2010/wordprocessingInk" xmlns:wne="http://schemas.microsoft.com/office/word/2006/wordml" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape" mc:Ignorable="w14 w15 wp14">"#,
    );
    for author in authors {
        // Escape XML special chars in author name
        let escaped = author
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;");
        xml.push_str(&format!(
            r#"<w15:person w15:author="{escaped}"><w15:presenceInfo w15:providerId="None" w15:userId="{escaped}"/></w15:person>"#,
        ));
    }
    xml.push_str("</w15:people>");
    xml
}

#[allow(clippy::type_complexity)]
pub(crate) fn serialize_tracked_blocks(
    blocks: &[crate::domain::TrackedBlock],
    next_id: &mut u32,
    bookmark_policy: &BookmarkIdPolicy,
    mut resolve_rel_rid: Option<&mut dyn FnMut(&str, &str) -> String>,
) -> Result<Vec<XMLNode>, RuntimeError> {
    let mut out = Vec::new();
    for tracked in blocks {
        let resolver = resolve_rel_rid
            .as_mut()
            .map(|resolver| &mut **resolver as &mut dyn FnMut(&str, &str) -> String);
        out.push(XMLNode::Element(serialize_tracked_block(
            tracked,
            next_id,
            bookmark_policy,
            resolver,
        )?));
    }
    Ok(out)
}

#[allow(clippy::type_complexity)]
pub(crate) fn serialize_tracked_block(
    tracked: &crate::domain::TrackedBlock,
    next_id: &mut u32,
    bookmark_policy: &BookmarkIdPolicy,
    resolve_rel_rid: Option<&mut dyn FnMut(&str, &str) -> String>,
) -> Result<Element, RuntimeError> {
    let origin = match &tracked.status {
        TrackingStatus::Inserted(_) => "target",
        TrackingStatus::Normal
        | TrackingStatus::Deleted(_)
        | TrackingStatus::InsertedThenDeleted(_) => "base",
    };
    match &tracked.block {
        BlockNode::Paragraph(p) => serialize_paragraph_node(
            p,
            Some(&tracked.status),
            tracked.move_id.is_some(),
            next_id,
            bookmark_policy,
            origin,
            resolve_rel_rid,
        ),
        BlockNode::Table(t) => {
            serialize_table_node(t, &tracked.status, next_id, bookmark_policy, origin)
        }
        BlockNode::OpaqueBlock(o) => Err(RuntimeError {
            code: ErrorCode::UnsupportedEdit,
            message: "cannot serialize OpaqueBlock from canonical model".to_string(),
            details: ErrorDetails {
                block_id: Some(o.id.clone()),
                context: Some(format!("opaque_ref={}", o.opaque_ref)),
                ..ErrorDetails::default()
            },
        }),
    }
}

#[allow(clippy::type_complexity)]
pub(crate) fn serialize_untracked_block(
    block: &BlockNode,
    next_id: &mut u32,
    bookmark_policy: &BookmarkIdPolicy,
    origin: &str,
    resolve_rel_rid: Option<&mut dyn FnMut(&str, &str) -> String>,
) -> Result<Element, RuntimeError> {
    match block {
        BlockNode::Paragraph(p) => serialize_paragraph_node(
            p,
            None,
            false,
            next_id,
            bookmark_policy,
            origin,
            resolve_rel_rid,
        ),
        BlockNode::Table(t) => {
            serialize_table_node(t, &TrackingStatus::Normal, next_id, bookmark_policy, origin)
        }
        BlockNode::OpaqueBlock(o) => Err(RuntimeError {
            code: ErrorCode::UnsupportedEdit,
            message: "cannot serialize OpaqueBlock from canonical model".to_string(),
            details: ErrorDetails {
                block_id: Some(o.id.clone()),
                context: Some(format!("opaque_ref={}", o.opaque_ref)),
                ..ErrorDetails::default()
            },
        }),
    }
}

// =============================================================================
// Paragraph serialization
// =============================================================================

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(crate) fn serialize_paragraph_node(
    paragraph: &ParagraphNode,
    block_status: Option<&TrackingStatus>,
    is_move: bool,
    next_id: &mut u32,
    bookmark_policy: &BookmarkIdPolicy,
    origin: &str,
    mut resolve_rel_rid: Option<&mut dyn FnMut(&str, &str) -> String>,
) -> Result<Element, RuntimeError> {
    // ── Exhaustive field witness ────────────────────────────────────────
    // Every ParagraphNode field MUST be listed here WITHOUT `..`.
    // Adding a field to ParagraphNode without listing it = compile error.
    // Fields are annotated with where they're handled:
    //   → pPr       = serialized in build_paragraph_properties() below
    //   → here      = serialized in this function's body
    //   → derived   = computed value not written to XML
    //   → frontend  = used by frontend only, not part of OOXML output
    let ParagraphNode {
        id: _,                 // NodeId — internal, not serialized
        style_id: _,           // → pPr (pStyle)
        align: _,              // → pPr (jc)
        has_direct_align: _,   // → pPr (gate for jc emission)
        indent: _,             // frontend — resolved effective indent (not serialized)
        has_direct_indent: _,  // → pPr (gate for ind emission)
        authored_indent: _,    // → pPr (ind) — the verbatim authored w:ind
        spacing: _,            // frontend — resolved effective spacing (not serialized)
        has_direct_spacing: _, // → pPr (gate for spacing emission)
        authored_spacing: _,   // → pPr (spacing) — the verbatim authored w:spacing
        borders: _,            // → pPr (pBdr)
        keep_next: _,          // → pPr (keepNext)
        keep_lines: _,         // → pPr (keepLines)
        page_break_before: _,  // → pPr (pageBreakBefore)
        widow_control: _,      // → pPr (widowControl)
        contextual_spacing: _, // → pPr (contextualSpacing)
        shading: _,            // → pPr (shd)
        has_direct_keep_next: _,
        has_direct_keep_lines: _,
        has_direct_page_break_before: _,
        has_direct_widow_control: _,
        has_direct_contextual_spacing: _,
        has_direct_shading: _,
        has_direct_borders: _,
        tab_stops: _,
        effective_tab_stops_rel: _, // derived view value — never serialized              // → pPr (tabs)
        segments: _,                // → here (run content)
        block_text_hash: _,         // derived — diff fingerprint, not serialized
        numbering: _,               // → pPr (numPr) — resolved effective, gated below
        has_direct_numbering: _,    // → pPr (numPr) — gate: emit only paragraph-authored numPr
        numbering_suppressed: _,    // → pPr (numPr w:numId=0, §17.9.18)
        materialized_numbering: _,  // internal — projection roundtrip, not serialized
        rendered_text: _,           // derived — diff comparison, not serialized
        literal_prefix: _,          // → here (prefix text run)
        literal_prefix_marks: _,
        literal_prefix_style_props: _,
        literal_prefix_rpr_authored: _, // read via `paragraph.` at the prefix-run call site
        // mixed content/provenance; emitted XML is compared by the fidelity gate
        literal_prefix_leading_rpr: _,
        literal_prefix_trailing_rpr: _,
        literal_prefix_leading_tab_twips: _, // frontend — CSS --prefix-leading-gap
        literal_prefix_leading_tab_count: _,
        literal_prefix_leading_ws: _, // → here (verbatim leading ws, read via `paragraph.`)
        literal_prefix_trailing_ws: _, // → here (verbatim separator ws, read via `paragraph.`)
        literal_prefix_has_trailing_tab: _, // whether stripped prefix ended with a tab
        literal_prefix_trailing_tab_stop_twips: _, // explicit consumed prefix tab stop
        outline_lvl: _,               // → pPr (outlineLvl)
        heading_level: _,             // frontend — heading semantic level
        para_mark_status: _,          // → here (rPr ins/del marker)
        paragraph_mark_marks: _,      // → pPr (rPr)
        paragraph_mark_style_props: _, // → pPr (rPr)
        paragraph_mark_rpr_off: _,    // → pPr (rPr) — authored OFF toggles

        para_split: _,              // internal — merge guard, not serialized directly
        section_property_change: _, // → pPr (sectPrChange built from canonical change)
        formatting_change: _,       // → pPr (pPrChange)
        section_properties: _,      // → pPr (sectPr built from canonical model)
        mirror_indents: _,          // → pPr (mirrorIndents)
        auto_space_de: _,           // → pPr (autoSpaceDE)
        auto_space_dn: _,           // → pPr (autoSpaceDN)
        bidi: _,                    // → pPr (bidi)
        text_alignment: _,          // → pPr (textAlignment)
        text_direction: _,          // → pPr (textDirection)
        suppress_auto_hyphens: _,   // → pPr (suppressAutoHyphens)
        snap_to_grid: _,            // → pPr (snapToGrid)
        overflow_punct: _,          // → pPr (overflowPunct)
        adjust_right_ind: _,        // → pPr (adjustRightInd)
        word_wrap: _,               // → pPr (wordWrap)
        frame_pr: _,                // → pPr (framePr)
        para_id: _,                 // → here (w14:paraId attribute)
        text_id: _,                 // → here (w14:textId attribute)
        cnf_style: _,               // → pPr (cnfStyle)
        preserved_ppr: _,           // → pPr (post-pass: PPR_ORDER position, or end of pPr)
    } = paragraph;

    let mut p = w_el("p");
    if let Some(ref id) = paragraph.para_id {
        attr_set(&mut p, "w14:paraId", id.clone());
    }
    if let Some(ref id) = paragraph.text_id {
        attr_set(&mut p, "w14:textId", id.clone());
    }
    // A hoisted `literal_prefix` is ALWAYS re-materialized into the text stream —
    // it is real body text the author typed and stripped only for inline rendering.
    //
    // Historically this was suppressed when the paragraph carried structural
    // numbering, on the theory that Word regenerates the label from the numbering
    // definition (so emitting both would double it). Since 93c9ae4 the importer no
    // longer hoists a leading label on a NUMBERED paragraph (there the label run is
    // kept in the body verbatim, never becoming `literal_prefix`), so import never
    // produces the `literal_prefix + numbering` combination. The ONLY remaining
    // producer is an EDIT that adds numbering to a paragraph that already carried a
    // hoisted prefix — `copy_paragraph_formatting_from_exemplar` (SetBlockRangeAttr)
    // copies the exemplar's `numbering` while deliberately leaving `literal_prefix`
    // (text content) intact. In that case the prefix is genuine body text Word shows
    // IN ADDITION to the structural number (93c9ae4's model rule); suppressing it
    // here silently dropped lawyer-visible bytes from BOTH accept and reject of the
    // redline. Re-materialize it — the numbering renders its own label separately.
    let literal_prefix = paragraph
        .literal_prefix
        .as_deref()
        .map(str::trim)
        .filter(|prefix| !prefix.is_empty())
        .map(|prefix| format!("{prefix} "));

    let resolver = resolve_rel_rid
        .as_mut()
        .map(|resolver| &mut **resolver as &mut dyn FnMut(&str, &str) -> String);
    if let Some(ppr) = build_paragraph_properties(paragraph, next_id, resolver) {
        p.children.push(XMLNode::Element(ppr));
    }

    let para_mark_status = paragraph
        .para_mark_status
        .as_ref()
        .or(block_status)
        .filter(|status| !matches!(status, TrackingStatus::Normal));
    // The pilcrow mark is part of the MOVE only when the BLOCK itself is the
    // matching move half — a moveTo DESTINATION (block Inserted + move_id) whose
    // mark is Inserted, or a moveFrom SOURCE (block Deleted + move_id) whose mark
    // is Deleted. A stale `move_id` can outlive the move on a paragraph whose
    // block-level move half was resolved away (e.g. selectively rejecting the
    // block-deletion id of a moveFrom source restores the block to Normal but
    // leaves its mark-deletion pending): that pilcrow mark is now an INDEPENDENT
    // change and must emit plain `w:ins`/`w:del`, matching the model's projection
    // (accept MERGES the paragraph with its neighbour, it does not move-remove it).
    let block_move_to = is_move && matches!(block_status, Some(TrackingStatus::Inserted(_)));
    let block_move_from = is_move && matches!(block_status, Some(TrackingStatus::Deleted(_)));
    if let Some(status) = para_mark_status {
        match status {
            // A genuine move half's pilcrow mark is part of the move: emit
            // `w:moveTo`/`w:moveFrom` — the moved-paragraph twin of `w:ins`/`w:del`
            // — so Word resolves the whole moved paragraph as ONE move (matching
            // real Word's markup). See `ensure_ppr_rpr_move_to`.
            TrackingStatus::Inserted(rev) if block_move_to => {
                crate::word_xml::ensure_ppr_rpr_move_to(
                    &mut p,
                    next_annotation_id(next_id),
                    rev.author.as_deref().unwrap_or(""),
                    rev.date.as_deref().unwrap_or(""),
                )
            }
            TrackingStatus::Deleted(rev) if block_move_from => {
                crate::word_xml::ensure_ppr_rpr_move_from(
                    &mut p,
                    next_annotation_id(next_id),
                    rev.author.as_deref().unwrap_or(""),
                    rev.date.as_deref().unwrap_or(""),
                )
            }
            TrackingStatus::Inserted(rev) => crate::word_xml::ensure_ppr_rpr_ins(
                &mut p,
                next_annotation_id(next_id),
                rev.author.as_deref().unwrap_or(""),
                rev.date.as_deref().unwrap_or(""),
            ),
            TrackingStatus::Deleted(rev) => crate::word_xml::ensure_ppr_rpr_del(
                &mut p,
                next_annotation_id(next_id),
                rev.author.as_deref().unwrap_or(""),
                rev.date.as_deref().unwrap_or(""),
            ),
            TrackingStatus::InsertedThenDeleted(sr) => {
                // The stacked paragraph mark: both markers, ins before del
                // (CT_ParaRPr order; the same shape Word and the EBA corpus
                // produce).
                crate::word_xml::ensure_ppr_rpr_ins(
                    &mut p,
                    next_annotation_id(next_id),
                    sr.inserted.author.as_deref().unwrap_or(""),
                    sr.inserted.date.as_deref().unwrap_or(""),
                );
                crate::word_xml::ensure_ppr_rpr_del(
                    &mut p,
                    next_annotation_id(next_id),
                    sr.deleted.author.as_deref().unwrap_or(""),
                    sr.deleted.date.as_deref().unwrap_or(""),
                );
            }
            TrackingStatus::Normal => {}
        }
    }

    // Stripped literal prefixes serialize as their own run, so use the
    // prefix's stored formatting rather than borrowing the first body run.
    let (pfx_marks, pfx_style_props, pfx_directness) = if paragraph.literal_prefix.is_some() {
        (
            paragraph.literal_prefix_marks.clone(),
            paragraph.literal_prefix_style_props.clone(),
            paragraph.literal_prefix_rpr_authored,
        )
    } else {
        match paragraph.first_content_text_node() {
            Some(t) => (t.marks.clone(), t.style_props.clone(), t.rpr_authored),
            None => (vec![], StyleProps::default(), RunDirectness::default()),
        }
    };
    // The deferred separator between a literal prefix and the body text,
    // VERBATIM (e.g. "  \t\t"). Carried as the captured string — discretizing it
    // to a lone tab (the old bool) lost separator spaces and extra tabs. Legacy
    // models that recorded only the tab flag fall back to the historical "\t".
    let mut pending_prefix_sep: Option<String> = if literal_prefix.is_some()
        && paragraph.literal_prefix_has_trailing_tab
        // A separator with its own authored rPr is emitted by
        // append_literal_prefix_runs as its own run — never deferred.
        && paragraph.literal_prefix_trailing_rpr.is_none()
    {
        Some(if paragraph.literal_prefix_trailing_ws.contains('\t') {
            paragraph.literal_prefix_trailing_ws.clone()
        } else {
            "\t".to_string()
        })
    } else {
        None
    };

    if let Some(TrackingStatus::Deleted(rev)) = block_status {
        // Use w:moveFrom instead of w:del when this block is a move source.
        let author = rev.author.as_deref().unwrap_or("");
        let date = rev.date.as_deref().unwrap_or("");
        let container_kind = if is_move {
            TrackedContainer::MoveFrom
        } else {
            TrackedContainer::Del
        };
        if let Some(prefix) = &literal_prefix {
            let mut prefix_container = match &container_kind {
                TrackedContainer::MoveFrom => {
                    word_xml::w_move_from(next_annotation_id(next_id), author, date)
                }
                _ => w_del(next_annotation_id(next_id), author, date),
            };
            append_literal_prefix_runs(
                &mut prefix_container,
                prefix.trim_end(),
                &paragraph.literal_prefix_leading_ws,
                &paragraph.literal_prefix_trailing_ws,
                paragraph.literal_prefix_has_trailing_tab,
                false,
                paragraph.literal_prefix_leading_rpr.as_deref(),
                paragraph.literal_prefix_trailing_rpr.as_deref(),
                &pfx_marks,
                &pfx_style_props,
                pfx_directness,
                !is_move, // w:delText only valid inside w:del, not w:moveFrom
                next_id,
            );
            p.children.push(XMLNode::Element(prefix_container));
        }
        let all: Vec<&InlineNode> = paragraph.all_inlines().collect();
        let chunks = split_tracked_container_chunks(&all);
        emit_tracked_chunks(
            &mut p,
            &chunks,
            &container_kind,
            author,
            date,
            next_id,
            None,
            bookmark_policy,
            origin,
            match resolve_rel_rid.as_mut() {
                Some(resolver) => Some(&mut **resolver as &mut dyn FnMut(&str, &str) -> String),
                None => None,
            },
            &mut pending_prefix_sep,
        )?;
        ensure_prefix_trailing_tab_consumed(pending_prefix_sep.is_some(), &paragraph.id)?;
        return Ok(p);
    }

    // Block-level insertion: wrap all content runs in w:ins (or w:moveTo for moves)
    // so Word treats them as inserted/moved text.
    if let Some(TrackingStatus::Inserted(rev)) = block_status {
        let rev_author = rev.author.as_deref().unwrap_or("");
        let rev_date = rev.date.as_deref().unwrap_or("");
        let block_container_kind = if is_move {
            TrackedContainer::MoveTo
        } else {
            TrackedContainer::Ins
        };
        if let Some(prefix) = &literal_prefix {
            let mut wrapper = match &block_container_kind {
                TrackedContainer::MoveTo => {
                    word_xml::w_move_to(next_annotation_id(next_id), rev_author, rev_date)
                }
                _ => w_ins(next_annotation_id(next_id), rev_author, rev_date),
            };
            append_literal_prefix_runs(
                &mut wrapper,
                prefix.trim_end(),
                &paragraph.literal_prefix_leading_ws,
                &paragraph.literal_prefix_trailing_ws,
                paragraph.literal_prefix_has_trailing_tab,
                false,
                paragraph.literal_prefix_leading_rpr.as_deref(),
                paragraph.literal_prefix_trailing_rpr.as_deref(),
                &pfx_marks,
                &pfx_style_props,
                pfx_directness,
                false,
                next_id,
            );
            p.children.push(XMLNode::Element(wrapper));
        }
        let mut emitted_opaques = HashSet::new();
        for segment in &paragraph.segments {
            let segment_refs: Vec<&InlineNode> = segment.inlines.iter().collect();
            let chunks = split_tracked_container_chunks(&segment_refs);
            match &segment.status {
                TrackingStatus::Normal => {
                    let seg_container = if is_move {
                        TrackedContainer::MoveTo
                    } else {
                        TrackedContainer::Ins
                    };
                    emit_tracked_chunks(
                        &mut p,
                        &chunks,
                        &seg_container,
                        rev_author,
                        rev_date,
                        next_id,
                        Some(&mut emitted_opaques),
                        bookmark_policy,
                        origin,
                        match resolve_rel_rid.as_mut() {
                            Some(resolver) => {
                                Some(&mut **resolver as &mut dyn FnMut(&str, &str) -> String)
                            }
                            None => None,
                        },
                        &mut pending_prefix_sep,
                    )?;
                }
                TrackingStatus::Inserted(seg_rev) => {
                    let seg_author = seg_rev.author.as_deref().unwrap_or("");
                    let seg_date = seg_rev.date.as_deref().unwrap_or("");
                    let seg_container = if is_move {
                        TrackedContainer::MoveTo
                    } else {
                        TrackedContainer::Ins
                    };
                    emit_tracked_chunks(
                        &mut p,
                        &chunks,
                        &seg_container,
                        seg_author,
                        seg_date,
                        next_id,
                        Some(&mut emitted_opaques),
                        bookmark_policy,
                        origin,
                        match resolve_rel_rid.as_mut() {
                            Some(resolver) => {
                                Some(&mut **resolver as &mut dyn FnMut(&str, &str) -> String)
                            }
                            None => None,
                        },
                        &mut pending_prefix_sep,
                    )?;
                }
                TrackingStatus::Deleted(seg_rev) => {
                    let seg_author = seg_rev.author.as_deref().unwrap_or("");
                    let seg_date = seg_rev.date.as_deref().unwrap_or("");
                    emit_tracked_chunks(
                        &mut p,
                        &chunks,
                        &TrackedContainer::Del,
                        seg_author,
                        seg_date,
                        next_id,
                        Some(&mut emitted_opaques),
                        bookmark_policy,
                        origin,
                        match resolve_rel_rid.as_mut() {
                            Some(resolver) => {
                                Some(&mut **resolver as &mut dyn FnMut(&str, &str) -> String)
                            }
                            None => None,
                        },
                        &mut pending_prefix_sep,
                    )?;
                }
                TrackingStatus::InsertedThenDeleted(sr) => {
                    // Same canonical nested emission as emit_segment; never
                    // reached today (splice refuses move-wrapped paragraphs,
                    // import quarantines stacked shapes inside containers)
                    // but total rather than a panic.
                    let mut ins_wrapper = w_ins(
                        next_annotation_id(next_id),
                        sr.inserted.author.as_deref().unwrap_or(""),
                        sr.inserted.date.as_deref().unwrap_or(""),
                    );
                    emit_tracked_chunks(
                        &mut ins_wrapper,
                        &chunks,
                        &TrackedContainer::Del,
                        sr.deleted.author.as_deref().unwrap_or(""),
                        sr.deleted.date.as_deref().unwrap_or(""),
                        next_id,
                        Some(&mut emitted_opaques),
                        bookmark_policy,
                        origin,
                        match resolve_rel_rid.as_mut() {
                            Some(resolver) => {
                                Some(&mut **resolver as &mut dyn FnMut(&str, &str) -> String)
                            }
                            None => None,
                        },
                        &mut pending_prefix_sep,
                    )?;
                    if !ins_wrapper.children.is_empty() {
                        p.children.push(XMLNode::Element(ins_wrapper));
                    }
                }
            }
        }
        ensure_prefix_trailing_tab_consumed(pending_prefix_sep.is_some(), &paragraph.id)?;
        return Ok(p);
    }

    let embed_prefix_trailing_tab = pending_prefix_sep.is_some()
        && matches!(block_status, None | Some(TrackingStatus::Normal))
        && matches!(
            paragraph.segments.first().map(|segment| &segment.status),
            Some(TrackingStatus::Inserted(_)) | Some(TrackingStatus::Deleted(_))
        );
    if let Some(prefix) = &literal_prefix {
        append_literal_prefix_runs(
            &mut p,
            prefix.trim_end(),
            &paragraph.literal_prefix_leading_ws,
            &paragraph.literal_prefix_trailing_ws,
            paragraph.literal_prefix_has_trailing_tab,
            embed_prefix_trailing_tab,
            paragraph.literal_prefix_leading_rpr.as_deref(),
            paragraph.literal_prefix_trailing_rpr.as_deref(),
            &pfx_marks,
            &pfx_style_props,
            pfx_directness,
            false,
            next_id,
        );
    }
    if embed_prefix_trailing_tab {
        pending_prefix_sep = None;
    }

    let mut emitted_opaques = HashSet::new();
    let segments = &paragraph.segments;
    let mut seg_idx = 0;
    while seg_idx < segments.len() {
        let segment = &segments[seg_idx];

        // When a Del segment is followed by an Ins segment (or vice versa) and
        // both contain the same paragraph-level opaques, interleave their
        // emission so opaques appear between the corresponding tracked-change
        // containers rather than inside the first one processed.
        if seg_idx + 1 < segments.len() {
            let next_seg = &segments[seg_idx + 1];
            let is_del_ins = matches!(segment.status, TrackingStatus::Deleted(_))
                && matches!(next_seg.status, TrackingStatus::Inserted(_));
            if is_del_ins
                && segments_share_opaques(segment, next_seg)
                && !segment_contains_tracked_container_direct_markers(segment)
                && !segment_contains_tracked_container_direct_markers(next_seg)
            {
                emit_interleaved_del_ins(
                    &mut p,
                    segment,
                    next_seg,
                    &mut emitted_opaques,
                    next_id,
                    bookmark_policy,
                    origin,
                    match resolve_rel_rid.as_mut() {
                        Some(resolver) => {
                            Some(&mut **resolver as &mut dyn FnMut(&str, &str) -> String)
                        }
                        None => None,
                    },
                )?;
                seg_idx += 2;
                continue;
            }
        }

        emit_segment(
            &mut p,
            segment,
            &mut emitted_opaques,
            next_id,
            bookmark_policy,
            origin,
            match resolve_rel_rid.as_mut() {
                Some(resolver) => Some(&mut **resolver as &mut dyn FnMut(&str, &str) -> String),
                None => None,
            },
            &mut pending_prefix_sep,
        )?;
        seg_idx += 1;
    }

    ensure_prefix_trailing_tab_consumed(pending_prefix_sep.is_some(), &paragraph.id)?;
    renest_inline_move_containers(&mut p);
    renest_inline_bidi_wrappers(&mut p);
    renest_inline_custom_xml_wrappers(&mut p);
    Ok(p)
}

/// Re-nest inline `w:moveFrom`/`w:moveTo` containers that the import round-trip
/// path emits as a *pair* of empty bracket elements (a `MoveRange` start
/// decoration and the matching end decoration, see `import.rs` /
/// `word_ir::AtomKind::TrackedMove*`).
///
/// On import, an inline move container is decomposed into:
///   `[empty <w:moveFrom> open marker] … move content runs … [empty <w:moveFrom> close marker]`
/// where both markers carry the SAME childless wrapper bytes (same `w:id`).
/// Emitting them verbatim would leave the content as flat siblings *between*
/// two empty containers — structurally invalid OOXML. This pass folds the
/// intervening siblings back INTO the open marker and drops the close marker,
/// reconstructing the original `<w:moveFrom>…content…</w:moveFrom>` nesting.
///
/// Only operates on *empty* `moveFrom`/`moveTo` elements that appear as a
/// same-`w:id` pair; a `moveFrom`/`moveTo` the diff path already built with its
/// content nested (non-empty) is left untouched (no double-wrapping).
fn renest_inline_move_containers(p: &mut Element) {
    fn empty_move_marker_id(node: &XMLNode) -> Option<(String, String)> {
        let XMLNode::Element(el) = node else {
            return None;
        };
        let local = local_element_name(el);
        if local != "moveFrom" && local != "moveTo" {
            return None;
        }
        // Only an *empty* container is a bracket marker. A populated
        // moveFrom/moveTo (built by the diff path) already nests its content.
        if el.children.iter().any(|c| matches!(c, XMLNode::Element(_))) {
            return None;
        }
        let id = attr_get(el, "w:id")?.clone();
        Some((local.to_string(), id))
    }

    let mut i = 0;
    while i < p.children.len() {
        let Some((open_local, open_id)) = empty_move_marker_id(&p.children[i]) else {
            i += 1;
            continue;
        };
        // Find the matching close: the next empty move marker with the same
        // local name and w:id.
        let close = ((i + 1)..p.children.len()).find(|&j| {
            empty_move_marker_id(&p.children[j])
                .is_some_and(|(l, id)| l == open_local && id == open_id)
        });
        let Some(close_idx) = close else {
            // Unpaired marker — leave as-is rather than guessing (no silent
            // fallback). A balanced document always pairs them.
            i += 1;
            continue;
        };
        // Drain the siblings strictly between open and close into the open
        // container, then remove the (now redundant) close marker.
        let between: Vec<XMLNode> = p.children.drain((i + 1)..close_idx).collect();
        // After draining, the close marker has shifted to index i + 1.
        p.children.remove(i + 1);
        if let XMLNode::Element(open_el) = &mut p.children[i] {
            open_el.children.extend(between);
        }
        i += 1;
    }
}

/// Re-nest inline `w:bdo` / `w:dir` display-only wrappers that the import
/// round-trip path emits as a *pair* of empty bracket elements (a `BidiWrapper`
/// start decoration and the matching end decoration, see `import.rs` /
/// `word_ir::AtomKind::BidiWrapper*`).
///
/// On import a bdo/dir wrapper is decomposed into:
///   `[empty <w:bdo w:val="…"/> open marker] … inner runs … [empty <w:bdo …/> close marker]`
/// where both markers carry the SAME childless wrapper bytes. This pass folds
/// the intervening siblings back INTO the open marker and drops the close
/// marker, reconstructing `<w:bdo>…runs…</w:bdo>` verbatim.
///
/// Unlike `moveFrom`/`moveTo`, bdo/dir carry no `w:id`, so the open/close pairs
/// are matched by STACK ORDER over empty markers of the same local name: an
/// incoming empty marker that matches the open marker currently on top of the
/// stack closes it; otherwise it opens a new wrapper. This handles both adjacent
/// (`bdo…bdo bdo…bdo`) and nested (`bdo … dir … /dir … /bdo`) shapes.
fn renest_inline_bidi_wrappers(p: &mut Element) {
    fn empty_bidi_marker_name(node: &XMLNode) -> Option<String> {
        let XMLNode::Element(el) = node else {
            return None;
        };
        let local = local_element_name(el);
        if local != "bdo" && local != "dir" {
            return None;
        }
        // Only an *empty* container is a bracket marker. A populated bdo/dir
        // (already re-nested by a prior fold) keeps its content.
        if el.children.iter().any(|c| matches!(c, XMLNode::Element(_))) {
            return None;
        }
        Some(local.to_string())
    }

    // Iterate to a fixed point: each pass folds the INNERMOST balanced pair
    // (an open immediately followed, at this level, by its close once any inner
    // wrappers have already been folded). Folding changes indices, so restart.
    loop {
        // Find an open marker whose matching close (by stack order) has no
        // intervening *unmatched* marker of the same name — i.e. the innermost
        // pair. We locate it by walking and tracking a per-name open stack.
        let mut stack: Vec<(String, usize)> = Vec::new();
        let mut fold: Option<(usize, usize)> = None;
        for i in 0..p.children.len() {
            let Some(name) = empty_bidi_marker_name(&p.children[i]) else {
                continue;
            };
            if let Some((top_name, open_idx)) = stack.last().cloned()
                && top_name == name
            {
                // This marker closes the open one on top of the stack, and
                // since it is the top, nothing unmatched lies between them:
                // the innermost pair. Fold it.
                stack.pop();
                fold = Some((open_idx, i));
                break;
            }
            stack.push((name, i));
        }
        let Some((open_idx, close_idx)) = fold else {
            break;
        };
        // Drain the siblings strictly between open and close into the open
        // container, then remove the (now redundant) close marker.
        let between: Vec<XMLNode> = p.children.drain((open_idx + 1)..close_idx).collect();
        // After draining, the close marker shifted to open_idx + 1.
        p.children.remove(open_idx + 1);
        if let XMLNode::Element(open_el) = &mut p.children[open_idx] {
            open_el.children.extend(between);
        }
    }
}

/// Re-nest inline `w:customXml` / `w:smartTag` transparent wrappers that the
/// import round-trip path emits as a *pair* of empty bracket elements (a
/// `CustomXmlWrapper` start decoration and the matching end decoration, see
/// `import.rs` / `word_ir::AtomKind::CustomXmlWrapper*`).
///
/// On import a customXml/smartTag wrapper is decomposed into:
///   `[empty <w:customXml …/> open marker] … inner runs/revisions … [empty <w:customXml …/> close marker]`
/// where both markers carry the SAME childless wrapper bytes (attributes +
/// `customXmlPr`/`smartTagPr` preserved, content children cleared). This pass
/// folds the intervening siblings back INTO the open marker and drops the
/// close marker, reconstructing `<w:customXml>…content…</w:customXml>` verbatim.
///
/// Like `w:bdo`/`w:dir` (and unlike id-paired `moveFrom`/`moveTo`), these
/// wrappers carry no `w:id`, so open/close pairs are matched by STACK ORDER
/// over empty markers of the same local name — handling adjacent and nested
/// (`customXml … smartTag … /smartTag … /customXml`) shapes. The `customXmlPr`
/// / `smartTagPr` child is part of the (childless-content) template, so a
/// marker carrying ONLY a property child still counts as an empty bracket.
fn renest_inline_custom_xml_wrappers(p: &mut Element) {
    fn empty_wrapper_marker_name(node: &XMLNode) -> Option<String> {
        let XMLNode::Element(el) = node else {
            return None;
        };
        let local = local_element_name(el);
        if local != "customXml" && local != "smartTag" {
            return None;
        }
        // Only an *empty-of-content* container is a bracket marker. The
        // wrapper's own property child (`customXmlPr`/`smartTagPr`) is part of
        // the template and does NOT count as content; any OTHER element child
        // means this wrapper was already re-nested (a prior fold) and keeps its
        // content.
        let has_content_child = el.children.iter().any(|c| match c {
            XMLNode::Element(e) => {
                let l = local_element_name(e);
                l != "customXmlPr" && l != "smartTagPr"
            }
            _ => false,
        });
        if has_content_child {
            return None;
        }
        Some(local.to_string())
    }

    // Iterate to a fixed point: each pass folds the INNERMOST balanced pair
    // (an open immediately followed, at this level, by its close once any inner
    // wrappers have already been folded). Folding changes indices, so restart.
    loop {
        let mut stack: Vec<(String, usize)> = Vec::new();
        let mut fold: Option<(usize, usize)> = None;
        for i in 0..p.children.len() {
            let Some(name) = empty_wrapper_marker_name(&p.children[i]) else {
                continue;
            };
            if let Some((top_name, open_idx)) = stack.last().cloned()
                && top_name == name
            {
                // This marker closes the open one on top of the stack, and
                // since it is the top, nothing unmatched lies between them:
                // the innermost pair. Fold it.
                stack.pop();
                fold = Some((open_idx, i));
                break;
            }
            stack.push((name, i));
        }
        let Some((open_idx, close_idx)) = fold else {
            break;
        };
        // Drain the siblings strictly between open and close into the open
        // container, then remove the (now redundant) close marker.
        let between: Vec<XMLNode> = p.children.drain((open_idx + 1)..close_idx).collect();
        // After draining, the close marker shifted to open_idx + 1.
        p.children.remove(open_idx + 1);
        if let XMLNode::Element(open_el) = &mut p.children[open_idx] {
            open_el.children.extend(between);
        }
    }
}

#[allow(clippy::type_complexity)]
fn build_paragraph_sect_pr(
    paragraph: &ParagraphNode,
    resolve_section_rid: Option<&mut dyn FnMut(&str, &str) -> String>,
) -> Option<Element> {
    let section_properties = paragraph.section_properties.as_ref()?;

    let mut resolve_section_rid = resolve_section_rid;

    let sect_pr_change = paragraph.section_property_change.as_ref().map(|change| {
        let mut change_el = w_el("sectPrChange");
        attr_set(
            &mut change_el,
            "w:id",
            change.revision.revision_id.to_string(),
        );
        if let Some(ref author) = change.revision.author {
            attr_set(&mut change_el, "w:author", author.clone());
        }
        if let Some(ref date) = change.revision.date {
            attr_set(&mut change_el, "w:date", date.clone());
        }
        if let Ok(mut previous) =
            crate::word_xml::parse_raw_fragment(&change.previous_properties_raw)
        {
            crate::runtime::materialize_empty_sect_pr_snapshot(&mut previous);
            change_el.children.push(XMLNode::Element(previous));
        }
        // The previous sectPr carries placeholder (part_path) header/footer
        // r:id values; resolve them through the same package resolver so they
        // become real, registered relationship rIds (else Word needs repair).
        if let Some(resolve) = resolve_section_rid.as_deref_mut() {
            crate::runtime::resolve_sect_pr_change_story_refs(&mut change_el, resolve);
        }
        change_el
    });

    Some(crate::runtime::section_properties_to_element(
        section_properties,
        None,
        sect_pr_change,
        resolve_section_rid,
    ))
}

/// Emit a CT_OnOff pPr flag faithfully (ST_OnOff §17.17.4): `Some(true)` →
/// `<w:name/>`, `Some(false)` → `<w:name w:val="0"/>`, `None` → nothing.
/// Returns whether an element was pushed.
///
/// An explicit OFF is an AUTHORED override, NOT the same as absent — dropping it
/// (or dropping an explicit ON, the symmetric case) silently lets the paragraph
/// re-inherit its style's value on round-trip. These flag fields all carry the
/// paragraph's OWN direct value (they are stored straight from the parsed pPr,
/// never through the style cascade), so emitting BOTH polarities never
/// materializes an inherited flag onto an untouched paragraph. This is the
/// paragraph analogue of the run `bold_off`/`underline_off` off-forms.
fn push_onoff_flag(ppr: &mut Element, name: &str, val: Option<bool>) -> bool {
    let Some(on) = val else { return false };
    let mut el = w_el(name);
    if !on {
        attr_set(&mut el, "w:val", "0");
    }
    ppr.children.push(XMLNode::Element(el));
    true
}

#[allow(clippy::type_complexity)]
pub(crate) fn build_paragraph_properties(
    paragraph: &ParagraphNode,
    next_id: &mut u32,
    resolve_section_rid: Option<&mut dyn FnMut(&str, &str) -> String>,
) -> Option<Element> {
    let mut ppr = w_el("pPr");
    let mut has_any = false;

    // CT_PPrBase + CT_PPr sequence (ECMA-376 Annex A, §17.3.1.26).
    // Children MUST appear in this order.

    // --- Position 0: pStyle ---
    if let Some(style_id) = &paragraph.style_id {
        let mut pstyle = w_el("pStyle");
        attr_set(&mut pstyle, "w:val", style_id.clone());
        ppr.children.push(XMLNode::Element(pstyle));
        has_any = true;
    }

    // --- Position 1: keepNext ---
    // Style-resolved slots emit as direct pPr only when the paragraph's own
    // pPr authored them (same provenance rule as direct_run_style_props).
    if paragraph.has_direct_keep_next
        && let Some(kn) = paragraph.keep_next
    {
        let mut el = w_el("keepNext");
        if !kn {
            attr_set(&mut el, "w:val", "0");
        }
        ppr.children.push(XMLNode::Element(el));
        has_any = true;
    }

    // --- Position 2: keepLines ---
    if paragraph.has_direct_keep_lines
        && let Some(kl) = paragraph.keep_lines
    {
        let mut el = w_el("keepLines");
        if !kl {
            attr_set(&mut el, "w:val", "0");
        }
        ppr.children.push(XMLNode::Element(el));
        has_any = true;
    }

    // --- Position 3: pageBreakBefore ---
    // Gate on has_direct so a style-inherited pageBreakBefore is not
    // materialized; within a direct authoring, emit BOTH polarities — an
    // explicit `<w:pageBreakBefore w:val="0"/>` cancels a style's page break and
    // must round-trip, exactly like widowControl below. (`page_break_before` is
    // the resolved value, but direct wins in the cascade so it equals the
    // authored value whenever has_direct is set.)
    if paragraph.has_direct_page_break_before {
        let mut el = w_el("pageBreakBefore");
        if !paragraph.page_break_before {
            attr_set(&mut el, "w:val", "0");
        }
        ppr.children.push(XMLNode::Element(el));
        has_any = true;
    }

    // --- Position 4: framePr ---
    if let Some(ref fp) = paragraph.frame_pr {
        ppr.children.push(XMLNode::Element(build_frame_pr(fp)));
        has_any = true;
    }

    // --- Position 5: widowControl ---
    if paragraph.has_direct_widow_control
        && let Some(wc) = paragraph.widow_control
    {
        let mut el = w_el("widowControl");
        if !wc {
            attr_set(&mut el, "w:val", "0");
        }
        ppr.children.push(XMLNode::Element(el));
        has_any = true;
    }

    // --- Position 6: numPr ---
    // Emit a direct w:numPr ONLY when the paragraph's own pPr authored it
    // (has_direct_numbering). `numbering` is the RESOLVED EFFECTIVE value, which
    // for a paragraph that inherits its numbering from a style (§17.7.4.14) or
    // via the abstractNum's pStyle reverse binding (§17.9.23) holds a numId/ilvl
    // the paragraph never wrote directly. Materializing that as a direct numPr
    // on an untouched paragraph shifts its numbering-inherited indent with no
    // pPrChange — the numbering analogue of emitting resolved-effective w:ind.
    if paragraph.has_direct_numbering
        && let Some(numbering) = &paragraph.numbering
    {
        let mut num_pr = w_el("numPr");
        let mut ilvl = w_el("ilvl");
        attr_set(&mut ilvl, "w:val", numbering.ilvl.to_string());
        let mut num_id = w_el("numId");
        attr_set(&mut num_id, "w:val", numbering.num_id.to_string());
        num_pr.children.push(XMLNode::Element(ilvl));
        num_pr.children.push(XMLNode::Element(num_id));
        ppr.children.push(XMLNode::Element(num_pr));
        has_any = true;
    } else if paragraph.numbering_suppressed {
        // §17.9.18: w:numId=0 explicitly removes inherited (style/pStyle)
        // numbering. Re-emit the suppression marker so the paragraph does not
        // silently re-inherit its style's numId on round-trip.
        let mut num_pr = w_el("numPr");
        let mut ilvl = w_el("ilvl");
        attr_set(&mut ilvl, "w:val", "0");
        let mut num_id = w_el("numId");
        attr_set(&mut num_id, "w:val", "0");
        num_pr.children.push(XMLNode::Element(ilvl));
        num_pr.children.push(XMLNode::Element(num_id));
        ppr.children.push(XMLNode::Element(num_pr));
        has_any = true;
    }

    // --- Position 8: pBdr ---
    if paragraph.has_direct_borders
        && let Some(borders) = &paragraph.borders
    {
        let mut pbdr = w_el("pBdr");
        if let Some(top) = &borders.top {
            pbdr.children
                .push(XMLNode::Element(build_border_edge("top", top)));
        }
        if let Some(left) = &borders.left {
            pbdr.children
                .push(XMLNode::Element(build_border_edge("left", left)));
        }
        if let Some(bottom) = &borders.bottom {
            pbdr.children
                .push(XMLNode::Element(build_border_edge("bottom", bottom)));
        }
        if let Some(right) = &borders.right {
            pbdr.children
                .push(XMLNode::Element(build_border_edge("right", right)));
        }
        if let Some(between) = &borders.between {
            pbdr.children
                .push(XMLNode::Element(build_border_edge("between", between)));
        }
        if let Some(bar) = &borders.bar {
            pbdr.children
                .push(XMLNode::Element(build_border_edge("bar", bar)));
        }
        ppr.children.push(XMLNode::Element(pbdr));
        has_any = true;
    }

    // --- Position 9: shd ---
    if paragraph.has_direct_shading
        && let Some(shading) = &paragraph.shading
    {
        let mut shd = w_el("shd");
        if let Some(ref fill) = shading.fill {
            attr_set(&mut shd, "w:fill", fill.clone());
        }
        if let Some(ref val) = shading.val {
            attr_set(&mut shd, "w:val", val.to_xml_str());
        }
        if let Some(ref color) = shading.color {
            attr_set(&mut shd, "w:color", color.clone());
        }
        ppr.children.push(XMLNode::Element(shd));
        has_any = true;
    }

    // --- Position 10: tabs ---
    let paragraph_tab_stops = serialized_tab_stops_for_paragraph(paragraph);
    if !paragraph_tab_stops.is_empty() {
        let mut tabs = w_el("tabs");
        for tab in &paragraph_tab_stops {
            let mut tab_el = w_el("tab");
            attr_set(&mut tab_el, "w:val", tab.alignment.to_xml_str());
            attr_set(&mut tab_el, "w:pos", tab.position.to_string());
            if let Some(ref leader) = tab.leader {
                attr_set(&mut tab_el, "w:leader", leader.to_xml_str());
            }
            tabs.children.push(XMLNode::Element(tab_el));
        }
        ppr.children.push(XMLNode::Element(tabs));
        has_any = true;
    }

    // --- Positions 11-20: CT_OnOff pPr flags ---
    // Each carries the paragraph's OWN direct value (Option<bool>: None absent,
    // Some(true) on, Some(false) off) and re-emits both polarities faithfully.
    // Previously several were one-armed (suppressAutoHyphens emitted only ON;
    // wordWrap/overflowPunct/adjustRightInd/snapToGrid only OFF; bidi was a lossy
    // plain bool that could not carry an explicit OFF at all), silently dropping
    // the other polarity on untouched paragraphs.
    has_any |= push_onoff_flag(
        &mut ppr,
        "suppressAutoHyphens",
        paragraph.suppress_auto_hyphens,
    );
    has_any |= push_onoff_flag(&mut ppr, "wordWrap", paragraph.word_wrap);
    has_any |= push_onoff_flag(&mut ppr, "overflowPunct", paragraph.overflow_punct);
    has_any |= push_onoff_flag(&mut ppr, "autoSpaceDE", paragraph.auto_space_de);
    has_any |= push_onoff_flag(&mut ppr, "autoSpaceDN", paragraph.auto_space_dn);
    has_any |= push_onoff_flag(&mut ppr, "bidi", paragraph.bidi);
    has_any |= push_onoff_flag(&mut ppr, "adjustRightInd", paragraph.adjust_right_ind);
    has_any |= push_onoff_flag(&mut ppr, "snapToGrid", paragraph.snap_to_grid);

    // --- Position 21: spacing ---
    // Only emit w:spacing when spacing was directly set on the paragraph.
    // Emit the AUTHORED-direct spacing verbatim — NOT the resolved effective
    // value, which has the style/docDefaults cascade baked in (e.g. an inherited
    // `w:after`) and would materialize inherited spacing onto the direct pPr.
    // Fall back to the effective `spacing` only for snapshots predating
    // `authored_spacing` (serde-default None) so they keep their emission.
    // CT_Spacing attribute order per ECMA-376 Annex A:
    // before, beforeLines, beforeAutospacing, after, afterLines, afterAutospacing, line, lineRule
    if paragraph.has_direct_spacing
        && let Some(spacing) = paragraph
            .authored_spacing
            .as_ref()
            .or(paragraph.spacing.as_ref())
    {
        let mut sp = w_el("spacing");
        if let Some(before) = spacing.before {
            attr_set(&mut sp, "w:before", before.to_string());
        }
        if let Some(before_lines) = spacing.before_lines {
            attr_set(&mut sp, "w:beforeLines", before_lines.to_string());
        }
        if let Some(true) = spacing.before_autospacing {
            attr_set(&mut sp, "w:beforeAutospacing", "1");
        }
        if let Some(after) = spacing.after {
            attr_set(&mut sp, "w:after", after.to_string());
        }
        if let Some(after_lines) = spacing.after_lines {
            attr_set(&mut sp, "w:afterLines", after_lines.to_string());
        }
        if let Some(true) = spacing.after_autospacing {
            attr_set(&mut sp, "w:afterAutospacing", "1");
        }
        if let Some(line) = spacing.line {
            attr_set(&mut sp, "w:line", line.to_string());
        }
        if let Some(rule) = &spacing.line_rule {
            let val = match rule {
                LineSpacingRule::Auto => "auto",
                LineSpacingRule::Exact => "exact",
                LineSpacingRule::AtLeast => "atLeast",
            };
            attr_set(&mut sp, "w:lineRule", val);
        }
        ppr.children.push(XMLNode::Element(sp));
        has_any = true;
    }

    // --- Position 22: ind ---
    // Only emit w:ind when indentation was directly set on the paragraph.
    // Emit the AUTHORED-direct indent verbatim — NOT the resolved effective
    // value, which bakes in the numbering-level / style cascade (materializing an
    // inherited `w:left`, dropping an authored hanging via tab absorption, etc.).
    // Fall back to the effective `indent` only for snapshots predating
    // `authored_indent` (serde-default None) so they keep their emission.
    if paragraph.has_direct_indent
        && let Some(indent) = paragraph
            .authored_indent
            .as_ref()
            .or(paragraph.indent.as_ref())
    {
        let mut ind = w_el("ind");
        if let Some(left) = indent.left {
            attr_set(&mut ind, "w:left", left.to_string());
        }
        if let Some(right) = indent.right {
            attr_set(&mut ind, "w:right", right.to_string());
        }
        if let Some(first_line) = indent.effective_first_line_twips {
            if first_line >= 0 {
                attr_set(&mut ind, "w:firstLine", first_line.to_string());
            } else {
                attr_set(&mut ind, "w:hanging", (-first_line).to_string());
            }
        }
        // Character-unit indents: emit the transitional-schema names
        // (leftChars/rightChars) to match the w:left/w:right twips this block
        // already writes. An explicit "0" is meaningful (it overrides an
        // inherited character indent) and must be emitted, not skipped.
        if let Some(sc) = indent.start_chars {
            attr_set(&mut ind, "w:leftChars", sc.to_string());
        }
        if let Some(ec) = indent.end_chars {
            attr_set(&mut ind, "w:rightChars", ec.to_string());
        }
        if let Some(flc) = indent.first_line_chars {
            attr_set(&mut ind, "w:firstLineChars", flc.to_string());
        }
        if let Some(hc) = indent.hanging_chars {
            attr_set(&mut ind, "w:hangingChars", hc.to_string());
        }
        ppr.children.push(XMLNode::Element(ind));
        has_any = true;
    }

    // --- Position 23: contextualSpacing ---
    if paragraph.has_direct_contextual_spacing
        && let Some(cs) = paragraph.contextual_spacing
    {
        let mut el = w_el("contextualSpacing");
        if !cs {
            attr_set(&mut el, "w:val", "0");
        }
        ppr.children.push(XMLNode::Element(el));
        has_any = true;
    }

    // --- Position 24: mirrorIndents ---
    has_any |= push_onoff_flag(&mut ppr, "mirrorIndents", paragraph.mirror_indents);

    // --- Position 26: jc ---
    // Only emit w:jc when alignment was directly set on the paragraph
    // (not inherited from styles). Word relies on style inheritance.
    if paragraph.has_direct_align
        && let Some(align) = &paragraph.align
    {
        let mut jc = w_el("jc");
        attr_set(&mut jc, "w:val", alignment_to_string(align));
        ppr.children.push(XMLNode::Element(jc));
        has_any = true;
    }

    // --- Position 27: textDirection ---
    if let Some(ref td) = paragraph.text_direction {
        let mut td_el = w_el("textDirection");
        attr_set(&mut td_el, "w:val", td.to_xml_str());
        ppr.children.push(XMLNode::Element(td_el));
        has_any = true;
    }

    // --- Position 28: textAlignment ---
    if let Some(ref ta) = paragraph.text_alignment {
        let mut ta_el = w_el("textAlignment");
        attr_set(&mut ta_el, "w:val", ta.to_xml_str());
        ppr.children.push(XMLNode::Element(ta_el));
        has_any = true;
    }

    // --- Position 29: outlineLvl (§17.3.1.20) ---
    // Emit only when the paragraph DIRECTLY authored w:outlineLvl (carried on
    // outline_lvl). Inherited cascade values are derived into heading_level and
    // are NOT re-emitted, mirroring the has_direct_* discipline for jc/ind/spacing.
    if let Some(lvl) = paragraph.outline_lvl {
        let mut ol_el = w_el("outlineLvl");
        attr_set(&mut ol_el, "w:val", lvl.to_string());
        ppr.children.push(XMLNode::Element(ol_el));
        has_any = true;
    }

    // --- Position 31: cnfStyle ---
    if let Some(ref cnf) = paragraph.cnf_style {
        ppr.children
            .push(XMLNode::Element(serialize_cnf_style(cnf)));
        has_any = true;
    }

    // --- Position 33: rPr ---
    if let Some(rpr) = build_paragraph_mark_rpr(paragraph, next_id) {
        ppr.children.push(XMLNode::Element(rpr));
        has_any = true;
    }

    // --- Position 34: sectPr ---
    if let Some(sect_el) = build_paragraph_sect_pr(paragraph, resolve_section_rid) {
        ppr.children.push(XMLNode::Element(sect_el));
        has_any = true;
    }

    // --- Preserved remainder: unmodeled pPr children ---
    //
    // Content this parser doesn't model (e.g. w:suppressLineNumbers,
    // w:kinsoku, or a foreign-namespace extension) was captured verbatim at
    // import (`ParagraphView::from_paragraph`) rather than dropped. Re-insert
    // it here at its Annex-A position (`docx_validate_ordering::PPR_ORDER`)
    // so an untouched paragraph round-trips byte-faithful for content this
    // engine has no typed model for. Run BEFORE pPrChange (position 35, below)
    // is appended, but AFTER rPr/sectPr (positions 33/34, above) so a
    // preserved child orders correctly relative to that tail.
    if !paragraph.preserved_ppr.is_empty() {
        insert_preserved_children(
            &mut ppr,
            &paragraph.preserved_ppr,
            crate::docx_validate_ordering::PPR_ORDER,
        );
        has_any = true;
    }

    // --- Position 35: pPrChange ---
    // Serialize w:pPrChange (§17.13.5.29) — tracked paragraph formatting change.
    // The inner pPr is a COMPLETE snapshot of the previous state per the spec.
    if let Some(ref fc) = paragraph.formatting_change {
        let mut ppr_change = w_el("pPrChange");
        attr_set(
            &mut ppr_change,
            "w:id",
            if fc.revision_id != 0 {
                fc.revision_id.to_string()
            } else {
                next_annotation_id(next_id).to_string()
            },
        );
        attr_set(&mut ppr_change, "w:author", fc.author.clone());
        if let Some(ref date) = fc.date {
            attr_set(&mut ppr_change, "w:date", date.clone());
        }
        let mut prev_ppr = w_el("pPr");
        // Inner pPr inside pPrChange follows same CT_PPrBase order as build_paragraph_properties.

        // --- Position 0: pStyle ---
        if let Some(ref style_id) = fc.previous_style_id {
            let mut pstyle = w_el("pStyle");
            attr_set(&mut pstyle, "w:val", style_id.clone());
            prev_ppr.children.push(XMLNode::Element(pstyle));
        }

        // --- Position 1: keepNext ---
        if let Some(kn) = fc.previous_keep_next {
            let mut el = w_el("keepNext");
            if !kn {
                attr_set(&mut el, "w:val", "0");
            }
            prev_ppr.children.push(XMLNode::Element(el));
        }

        // --- Position 2: keepLines ---
        if let Some(kl) = fc.previous_keep_lines {
            let mut el = w_el("keepLines");
            if !kl {
                attr_set(&mut el, "w:val", "0");
            }
            prev_ppr.children.push(XMLNode::Element(el));
        }

        // --- Position 3: pageBreakBefore ---
        if fc.previous_page_break_before {
            prev_ppr
                .children
                .push(XMLNode::Element(w_el("pageBreakBefore")));
        }

        // --- Position 4: framePr ---
        if let Some(ref fp) = fc.previous_frame_pr {
            prev_ppr.children.push(XMLNode::Element(build_frame_pr(fp)));
        }

        // --- Position 5: widowControl ---
        if let Some(wc) = fc.previous_widow_control {
            let mut el = w_el("widowControl");
            if !wc {
                attr_set(&mut el, "w:val", "0");
            }
            prev_ppr.children.push(XMLNode::Element(el));
        }

        // --- Position 6: numPr ---
        if let Some(ref numbering) = fc.previous_numbering {
            let mut num_pr = w_el("numPr");
            let mut ilvl_el = w_el("ilvl");
            attr_set(&mut ilvl_el, "w:val", numbering.ilvl.to_string());
            let mut num_id_el = w_el("numId");
            attr_set(&mut num_id_el, "w:val", numbering.num_id.to_string());
            num_pr.children.push(XMLNode::Element(ilvl_el));
            num_pr.children.push(XMLNode::Element(num_id_el));
            prev_ppr.children.push(XMLNode::Element(num_pr));
        } else if fc.previous_numbering_explicitly_absent {
            // Emit numId=0 to signal "base had no numbering at all" (§17.9.18).
            // The extraction's reject-view state machine uses this to skip the
            // paragraph — without it, the current numPr would be synthesized in
            // the reject view, producing a prefix that didn't exist in the base.
            let mut num_pr = w_el("numPr");
            let mut ilvl_el = w_el("ilvl");
            attr_set(&mut ilvl_el, "w:val", "0");
            let mut num_id_el = w_el("numId");
            attr_set(&mut num_id_el, "w:val", "0");
            num_pr.children.push(XMLNode::Element(ilvl_el));
            num_pr.children.push(XMLNode::Element(num_id_el));
            prev_ppr.children.push(XMLNode::Element(num_pr));
        }

        // --- Position 8: pBdr ---
        if let Some(ref borders) = fc.previous_borders {
            let mut pbdr = w_el("pBdr");
            if let Some(ref top) = borders.top {
                pbdr.children
                    .push(XMLNode::Element(build_border_edge("top", top)));
            }
            if let Some(ref left) = borders.left {
                pbdr.children
                    .push(XMLNode::Element(build_border_edge("left", left)));
            }
            if let Some(ref bottom) = borders.bottom {
                pbdr.children
                    .push(XMLNode::Element(build_border_edge("bottom", bottom)));
            }
            if let Some(ref right) = borders.right {
                pbdr.children
                    .push(XMLNode::Element(build_border_edge("right", right)));
            }
            if let Some(ref between) = borders.between {
                pbdr.children
                    .push(XMLNode::Element(build_border_edge("between", between)));
            }
            if let Some(ref bar) = borders.bar {
                pbdr.children
                    .push(XMLNode::Element(build_border_edge("bar", bar)));
            }
            prev_ppr.children.push(XMLNode::Element(pbdr));
        }

        // --- Position 9: shd ---
        if let Some(ref shading) = fc.previous_shading {
            let mut shd = w_el("shd");
            if let Some(ref fill) = shading.fill {
                attr_set(&mut shd, "w:fill", fill.clone());
            }
            if let Some(ref val) = shading.val {
                attr_set(&mut shd, "w:val", val.to_xml_str());
            }
            if let Some(ref color) = shading.color {
                attr_set(&mut shd, "w:color", color.clone());
            }
            prev_ppr.children.push(XMLNode::Element(shd));
        }

        // --- Position 10: tabs ---
        let previous_tab_stops = serialized_tab_stops_for_previous_formatting(fc);
        if !previous_tab_stops.is_empty() {
            let mut tabs = w_el("tabs");
            for tab in &previous_tab_stops {
                let mut tab_el = w_el("tab");
                attr_set(&mut tab_el, "w:val", tab.alignment.to_xml_str());
                attr_set(&mut tab_el, "w:pos", tab.position.to_string());
                if let Some(ref leader) = tab.leader {
                    attr_set(&mut tab_el, "w:leader", leader.to_xml_str());
                }
                tabs.children.push(XMLNode::Element(tab_el));
            }
            prev_ppr.children.push(XMLNode::Element(tabs));
        }

        // --- Positions 11-20: CT_OnOff pPr flags (previous DIRECT pPr) ---
        // Same faithful both-polarity emission as the live pPr block above.
        push_onoff_flag(
            &mut prev_ppr,
            "suppressAutoHyphens",
            fc.previous_suppress_auto_hyphens,
        );
        push_onoff_flag(&mut prev_ppr, "wordWrap", fc.previous_word_wrap);
        push_onoff_flag(&mut prev_ppr, "overflowPunct", fc.previous_overflow_punct);
        push_onoff_flag(&mut prev_ppr, "autoSpaceDE", fc.previous_auto_space_de);
        push_onoff_flag(&mut prev_ppr, "autoSpaceDN", fc.previous_auto_space_dn);
        push_onoff_flag(&mut prev_ppr, "bidi", fc.previous_bidi);
        push_onoff_flag(
            &mut prev_ppr,
            "adjustRightInd",
            fc.previous_adjust_right_ind,
        );
        push_onoff_flag(&mut prev_ppr, "snapToGrid", fc.previous_snap_to_grid);

        // --- Position 21: spacing ---
        if let Some(ref spacing) = fc.previous_spacing {
            let mut sp = w_el("spacing");
            if let Some(before) = spacing.before {
                attr_set(&mut sp, "w:before", before.to_string());
            }
            if let Some(before_lines) = spacing.before_lines {
                attr_set(&mut sp, "w:beforeLines", before_lines.to_string());
            }
            if let Some(true) = spacing.before_autospacing {
                attr_set(&mut sp, "w:beforeAutospacing", "1");
            }
            if let Some(after) = spacing.after {
                attr_set(&mut sp, "w:after", after.to_string());
            }
            if let Some(after_lines) = spacing.after_lines {
                attr_set(&mut sp, "w:afterLines", after_lines.to_string());
            }
            if let Some(true) = spacing.after_autospacing {
                attr_set(&mut sp, "w:afterAutospacing", "1");
            }
            if let Some(line) = spacing.line {
                attr_set(&mut sp, "w:line", line.to_string());
            }
            if let Some(ref rule) = spacing.line_rule {
                let val = match rule {
                    LineSpacingRule::Auto => "auto",
                    LineSpacingRule::Exact => "exact",
                    LineSpacingRule::AtLeast => "atLeast",
                };
                attr_set(&mut sp, "w:lineRule", val);
            }
            prev_ppr.children.push(XMLNode::Element(sp));
        }

        // --- Position 22: ind ---
        if let Some(ref indent) = fc.previous_indentation {
            let mut ind = w_el("ind");
            if let Some(left) = indent.left {
                attr_set(&mut ind, "w:left", left.to_string());
            }
            if let Some(right) = indent.right {
                attr_set(&mut ind, "w:right", right.to_string());
            }
            if let Some(first_line) = indent.effective_first_line_twips {
                if first_line >= 0 {
                    attr_set(&mut ind, "w:firstLine", first_line.to_string());
                } else {
                    attr_set(&mut ind, "w:hanging", (-first_line).to_string());
                }
            }
            // Transitional-schema names to match w:left/w:right (see above);
            // explicit "0" overrides an inherited char indent and must be emitted.
            if let Some(sc) = indent.start_chars {
                attr_set(&mut ind, "w:leftChars", sc.to_string());
            }
            if let Some(ec) = indent.end_chars {
                attr_set(&mut ind, "w:rightChars", ec.to_string());
            }
            if let Some(flc) = indent.first_line_chars {
                attr_set(&mut ind, "w:firstLineChars", flc.to_string());
            }
            if let Some(hc) = indent.hanging_chars {
                attr_set(&mut ind, "w:hangingChars", hc.to_string());
            }
            prev_ppr.children.push(XMLNode::Element(ind));
        }

        // --- Position 23: contextualSpacing ---
        if let Some(cs) = fc.previous_contextual_spacing {
            let mut el = w_el("contextualSpacing");
            if !cs {
                attr_set(&mut el, "w:val", "0");
            }
            prev_ppr.children.push(XMLNode::Element(el));
        }

        // --- Position 24: mirrorIndents ---
        push_onoff_flag(&mut prev_ppr, "mirrorIndents", fc.previous_mirror_indents);

        // --- Position 26: jc ---
        if let Some(ref align) = fc.previous_alignment {
            let mut jc = w_el("jc");
            attr_set(&mut jc, "w:val", alignment_to_string(align));
            prev_ppr.children.push(XMLNode::Element(jc));
        }

        // --- Position 27: textDirection ---
        if let Some(ref td) = fc.previous_text_direction {
            let mut td_el = w_el("textDirection");
            attr_set(&mut td_el, "w:val", td.to_xml_str());
            prev_ppr.children.push(XMLNode::Element(td_el));
        }

        // --- Position 28: textAlignment ---
        if let Some(ref ta) = fc.previous_text_alignment {
            let mut ta_el = w_el("textAlignment");
            attr_set(&mut ta_el, "w:val", ta.to_xml_str());
            prev_ppr.children.push(XMLNode::Element(ta_el));
        }

        // --- Position 33: rPr ---
        if !paragraph_mark_formatting_changed(paragraph, fc)
            && (!fc.previous_paragraph_mark_marks.is_empty()
                || !fc.previous_paragraph_mark_style_props.is_empty())
        {
            prev_ppr.children.push(XMLNode::Element(build_rpr(
                &fc.previous_paragraph_mark_marks,
                &fc.previous_paragraph_mark_style_props,
            )));
        }

        // --- Preserved remainder: unmodeled inner-pPr children ---
        //
        // Content the pPrChange parser doesn't model (e.g.
        // w:suppressLineNumbers, w:keepNext, w:pBdr) was captured verbatim at
        // import (`extract_ppr_change`) rather than dropped. Re-insert it
        // here at its Annex-A position (same CT_PPrBase order as the outer
        // pPr) so a pre-existing pPrChange round-trips byte-faithful for
        // content this engine has no typed model for.
        if !fc.previous_preserved_ppr.is_empty() {
            insert_preserved_children(
                &mut prev_ppr,
                &fc.previous_preserved_ppr,
                crate::docx_validate_ordering::PPR_ORDER,
            );
        }

        ppr_change.children.push(XMLNode::Element(prev_ppr));
        ppr.children.push(XMLNode::Element(ppr_change));
        has_any = true;
    }

    if has_any { Some(ppr) } else { None }
}

pub(crate) fn alignment_to_string(align: &Alignment) -> &'static str {
    match align {
        Alignment::Left => "left",
        Alignment::Center => "center",
        Alignment::Right => "right",
        Alignment::Justify => "both",
        Alignment::Distribute => "distribute",
        Alignment::HighKashida => "highKashida",
        Alignment::LowKashida => "lowKashida",
        Alignment::MediumKashida => "mediumKashida",
        Alignment::NumTab => "numTab",
        Alignment::ThaiDistribute => "thaiDistribute",
    }
}

/// Build a border edge XML element for serialization.
fn build_border_edge(name: &str, border: &Border) -> Element {
    let mut el = w_el(name);
    attr_set(&mut el, "w:val", border.style.to_xml_str());
    if let Some(ref color) = border.color {
        attr_set(&mut el, "w:color", color.clone());
    }
    if let Some(size) = border.size {
        attr_set(&mut el, "w:sz", size.to_string());
    }
    if let Some(space) = border.space {
        attr_set(&mut el, "w:space", space.to_string());
    }
    for (qname, value) in &border.extra_attrs {
        attr_set(&mut el, qname, value);
    }
    el
}

// =============================================================================
// Inline-level helpers
// =============================================================================

/// Check whether an inline node is a paragraph-level opaque that must NOT
/// appear inside w:del/w:ins/w:moveFrom/w:moveTo containers.
///
/// Per OOXML (ECMA-376 Annex A, CT_RunTrackChange), tracked-change containers
/// can only hold EG_ContentRunContent (runs, smartTag, sdt, etc.).
/// w:hyperlink, w:fldSimple, and m:oMathPara are paragraph-level elements
/// that must be direct children of w:p.
fn is_paragraph_level_opaque(inline: &InlineNode) -> bool {
    match inline {
        InlineNode::OpaqueInline(opaque) => matches!(
            &opaque.kind,
            OpaqueKind::Hyperlink(_)
                | OpaqueKind::Field(FieldData {
                    field_kind: FieldKind::Simple,
                    ..
                })
                | OpaqueKind::OmmlBlock
        ),
        _ => false,
    }
}

fn is_tracked_container_direct_inline(inline: &InlineNode) -> bool {
    is_paragraph_level_opaque(inline)
        || matches!(
            inline,
            InlineNode::Decoration(_)
                | InlineNode::CommentRangeStart { .. }
                | InlineNode::CommentRangeEnd { .. }
                | InlineNode::CommentReference { .. }
        )
}

fn segment_contains_tracked_container_direct_markers(segment: &TrackedSegment) -> bool {
    segment.inlines.iter().any(|inline| {
        is_tracked_container_direct_inline(inline) && !is_paragraph_level_opaque(inline)
    })
}

enum TrackedContentChunk<'a> {
    RunContent(Vec<&'a InlineNode>),
    DirectInline(&'a InlineNode),
}

fn element_local_name(element: &Element) -> &str {
    element
        .name
        .rsplit_once(':')
        .map(|(_, local)| local)
        .unwrap_or(&element.name)
}

fn is_math_tag(element: &Element, local: &str) -> bool {
    element_local_name(element) == local
        && (element.prefix.as_deref() == Some("m")
            || element.namespace.as_deref()
                == Some("http://schemas.openxmlformats.org/officeDocument/2006/math"))
}

fn has_math_track_change_child(element: &Element) -> bool {
    element.children.iter().any(|child| match child {
        XMLNode::Element(el) => {
            matches!(
                element_local_name(el),
                "del" | "ins" | "moveFrom" | "moveTo"
            ) && (el.prefix.as_deref() == Some("w")
                || el.namespace.as_deref()
                    == Some("http://schemas.openxmlformats.org/wordprocessingml/2006/main"))
        }
        _ => false,
    })
}

fn build_math_track_change(
    container_kind: &TrackedContainer,
    author: &str,
    date: &str,
    next_id: &mut u32,
) -> Element {
    match container_kind {
        TrackedContainer::Del => w_del(next_annotation_id(next_id), author, date),
        TrackedContainer::Ins => w_ins(next_annotation_id(next_id), author, date),
        TrackedContainer::MoveFrom => {
            word_xml::w_move_from(next_annotation_id(next_id), author, date)
        }
        TrackedContainer::MoveTo => word_xml::w_move_to(next_annotation_id(next_id), author, date),
    }
}

fn wrap_math_track_changes_in_place(
    element: &mut Element,
    container_kind: &TrackedContainer,
    author: &str,
    date: &str,
    next_id: &mut u32,
) {
    for child in &mut element.children {
        if let XMLNode::Element(child_el) = child {
            wrap_math_track_changes_in_place(child_el, container_kind, author, date, next_id);
        }
    }

    if !(is_math_tag(element, "r") || is_math_tag(element, "ctrlPr")) {
        return;
    }
    if element.children.is_empty() || has_math_track_change_child(element) {
        return;
    }

    let mut track = build_math_track_change(container_kind, author, date, next_id);
    track.children = std::mem::take(&mut element.children);
    element.children.push(XMLNode::Element(track));
}

fn append_tracked_omml_paragraph_opaque(
    parent: &mut Element,
    opaque: &OpaqueInlineNode,
    container_kind: &TrackedContainer,
    author: &str,
    date: &str,
    next_id: &mut u32,
) -> Result<(), RuntimeError> {
    let Some(raw_xml) = &opaque.raw_xml else {
        return Err(RuntimeError {
            code: ErrorCode::UnsupportedEdit,
            message: "math opaque inline without raw XML cannot be serialized".to_string(),
            details: ErrorDetails {
                block_id: Some(opaque.id.clone()),
                context: Some(format!("opaque_ref={}", opaque.opaque_ref)),
                ..ErrorDetails::default()
            },
        });
    };

    let mut element =
        crate::word_xml::parse_raw_fragment(raw_xml.as_slice()).map_err(|source| RuntimeError {
            code: ErrorCode::InvalidDocx,
            message: "failed to parse math opaque inline XML".to_string(),
            details: ErrorDetails {
                context: Some(format!("opaque_ref={} err={source}", opaque.opaque_ref)),
                ..ErrorDetails::default()
            },
        })?;
    wrap_math_track_changes_in_place(&mut element, container_kind, author, date, next_id);
    parent.children.push(XMLNode::Element(element));
    Ok(())
}

/// Emit a paragraph-level hyperlink opaque whose insertion/deletion is tracked
/// at the SEGMENT level.
///
/// Per the layering invariant documented on `HyperlinkRun::status`, when the
/// parent `TrackedSegment` is `Inserted`/`Deleted` the hyperlink's runs are
/// `Normal` — the tracking lives on the segment, not the runs. A `w:hyperlink`
/// is paragraph-level (EG_PContent) and cannot be a child of `w:ins`/`w:del`,
/// but ECMA-376 §17.13.5 permits wrapping the hyperlink's INNER runs, which
/// `build_hyperlink_element` already does per run status. So we stamp the
/// segment's status onto the runs and reuse that builder. Without this, an
/// inserted link serializes as untracked permanent content and `reject_all`
/// (in Word, on the bytes) would not revert it.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn append_tracked_hyperlink_paragraph_opaque(
    parent: &mut Element,
    data: &HyperlinkData,
    container_kind: &TrackedContainer,
    author: &str,
    date: &str,
    next_id: &mut u32,
    resolve_rel_rid: Option<&mut dyn FnMut(&str, &str) -> String>,
) {
    let rev = RevisionInfo {
        revision_id: next_annotation_id(next_id),
        author: Some(author.to_string()),
        date: Some(date.to_string()),
        apply_op_id: None,
    };
    // Caller restricts this to `Ins`/`Del` containers.
    let status = match container_kind {
        TrackedContainer::Del => TrackingStatus::Deleted(rev),
        _ => TrackingStatus::Inserted(rev),
    };

    let mut tracked = data.clone();
    // Register the external relationship and bind r:id, exactly as the untracked
    // hyperlink path does (see append_single_inline). A synthesized link carries
    // `r_id: None` until export; without this the inserted <w:hyperlink> serializes
    // with no backing relationship and loses its URL on the next save/reopen.
    if let (Some(url), Some(resolver)) = (&data.url, resolve_rel_rid) {
        tracked.r_id = Some(resolver(url, HYPERLINK_REL_TYPE));
    }
    if tracked.runs.is_empty() {
        // Synthesized links may carry only `text` with no run data; materialize a
        // single run so the status applies (build_hyperlink_element's empty-runs
        // fallback emits an untracked bare run otherwise).
        tracked.runs = vec![HyperlinkRun {
            text: tracked.text.clone(),
            rpr_xml: None,
            status: status.clone(),
        }];
    } else {
        for run in &mut tracked.runs {
            run.status = status.clone();
        }
    }
    parent
        .children
        .push(XMLNode::Element(build_hyperlink_element(&tracked)));
}

/// Lower a tracked-INSERTED simple field to the complex form, entirely inside
/// one `w:ins` (§17.16.18 fldChar / §17.16.23 instrText):
/// `begin` + `instrText` [+ `separate` + cached result] + `end`, each in its
/// own run wearing the field's wrapper rPr.
///
/// WHY (verified against real Word): `w:fldSimple`
/// legally cannot ride inside `w:ins` (EG_PContent is paragraph-level only;
/// I-TC-001), and the previous emission — field direct on the paragraph with
/// only the result run tracked — reads to Word as PERMANENT content: with no
/// cached result there is no revision at all, and reject-all leaves the field
/// in the document. Word's own writer lowers tracked field inserts to exactly
/// this complex form.
#[allow(clippy::too_many_arguments)]
fn append_tracked_inserted_complex_field(
    parent: &mut Element,
    data: &FieldData,
    wrapper_marks: &[Mark],
    wrapper_style_props: &StyleProps,
    kind: &OpaqueKind,
    author: &str,
    date: &str,
    next_id: &mut u32,
) {
    let rpr = build_wrapper_rpr(wrapper_marks, wrapper_style_props, kind);
    let field_run = |child: Element, rpr: &Option<Element>| -> Element {
        let mut run = w_el("r");
        if let Some(rpr) = rpr {
            run.children.push(XMLNode::Element(rpr.clone()));
        }
        run.children.push(XMLNode::Element(child));
        run
    };
    let fld_char = |char_type: &str| -> Element {
        let mut c = w_el("fldChar");
        attr_set(&mut c, "w:fldCharType", char_type);
        c
    };

    let mut ins = w_ins(next_annotation_id(next_id), author, date);
    ins.children
        .push(XMLNode::Element(field_run(fld_char("begin"), &rpr)));

    let instruction_text = data
        .semantic
        .as_ref()
        .map(|s| s.to_instruction_text())
        .or_else(|| data.instruction_text.clone());
    if let Some(text) = instruction_text {
        let mut instr = w_el("instrText");
        attr_set(&mut instr, "xml:space", "preserve");
        instr.children.push(XMLNode::Text(text));
        ins.children.push(XMLNode::Element(field_run(instr, &rpr)));
    }

    if let Some(result_text) = &data.result_text
        && !result_text.is_empty()
    {
        ins.children
            .push(XMLNode::Element(field_run(fld_char("separate"), &rpr)));
        ins.children.push(XMLNode::Element(build_text_run(
            result_text,
            wrapper_marks,
            wrapper_style_props,
            false,
            None,
            next_id,
        )));
    }

    ins.children
        .push(XMLNode::Element(field_run(fld_char("end"), &rpr)));
    parent.children.push(XMLNode::Element(ins));
}

/// Emit a paragraph-level `fldSimple` opaque whose DELETION is tracked at the
/// SEGMENT level, by wrapping the field's RESULT run in the tracked container.
/// The `<w:fldSimple>` element stays paragraph-level; per I-TC-001 it cannot
/// be a child of `<w:ins>`/`<w:del>`, so the run goes inside the field (the
/// same form as the hyperlink). Known gap: Word reads the field shell as
/// permanent content on reject; lowering the delete path (as the INSERT path
/// now does via `append_tracked_inserted_complex_field`) needs
/// `w:delInstrText` and its own oracle pass.
#[allow(clippy::too_many_arguments)]
fn append_tracked_simple_field_paragraph_opaque(
    parent: &mut Element,
    data: &FieldData,
    wrapper_marks: &[Mark],
    wrapper_style_props: &StyleProps,
    container_kind: &TrackedContainer,
    author: &str,
    date: &str,
    next_id: &mut u32,
) {
    let mut field = w_el("fldSimple");
    let instruction_text = data
        .semantic
        .as_ref()
        .map(|s| s.to_instruction_text())
        .or_else(|| data.instruction_text.clone());
    if let Some(text) = instruction_text {
        attr_set(&mut field, "w:instr", text);
    }
    if let Some(result_text) = &data.result_text
        && !result_text.is_empty()
    {
        let deleted = matches!(container_kind, TrackedContainer::Del);
        let mut container = match container_kind {
            TrackedContainer::Del => w_del(next_annotation_id(next_id), author, date),
            _ => w_ins(next_annotation_id(next_id), author, date),
        };
        container.children.push(XMLNode::Element(build_text_run(
            result_text,
            wrapper_marks,
            wrapper_style_props,
            deleted,
            None,
            next_id,
        )));
        field.children.push(XMLNode::Element(container));
    }
    parent.children.push(XMLNode::Element(field));
}

/// Split a tracked-container content sequence around inline nodes that are not
/// allowed inside `w:ins`/`w:del`/`w:moveFrom`/`w:moveTo`.
///
/// Run-level content is grouped into `RunContent` chunks; paragraph-level
/// structural markers and paragraph-level opaque elements become `DirectInline`
/// chunks that must be emitted directly on the paragraph to preserve a valid
/// WordprocessingML content model.
fn split_tracked_container_chunks<'a>(inlines: &[&'a InlineNode]) -> Vec<TrackedContentChunk<'a>> {
    let mut chunks = Vec::new();
    let mut acc: Vec<&'a InlineNode> = Vec::new();
    for &inline in inlines {
        if is_tracked_container_direct_inline(inline) {
            if !acc.is_empty() {
                chunks.push(TrackedContentChunk::RunContent(std::mem::take(&mut acc)));
            }
            chunks.push(TrackedContentChunk::DirectInline(inline));
        } else {
            acc.push(inline);
        }
    }
    if !acc.is_empty() {
        chunks.push(TrackedContentChunk::RunContent(acc));
    }
    chunks
}

enum TrackedContainer {
    Del,
    Ins,
    MoveFrom,
    MoveTo,
}

/// Compute a dedup key for a paragraph-level opaque inline.
///
/// When a paragraph-level opaque appears in both Deleted and Inserted segments
/// (because the diff couldn't match them as equal), we must emit it only once.
/// This returns a stable key based on semantic identity, excluding transport
/// details like r:id that differ between documents.
fn paragraph_opaque_dedup_key(inline: &InlineNode) -> Option<String> {
    match inline {
        InlineNode::OpaqueInline(opaque) => match &opaque.kind {
            OpaqueKind::Hyperlink(data) => Some(format!(
                "hyperlink:{:?}:{:?}:{:?}",
                data.url, data.anchor, data.text
            )),
            OpaqueKind::Field(data) if data.field_kind == FieldKind::Simple => Some(format!(
                "fldSimple:{:?}:{:?}",
                data.instruction_text, data.result_text
            )),
            _ => None,
        },
        _ => None,
    }
}

/// Emit tracked-change chunks onto a paragraph element.
///
/// RunContent chunks get wrapped in the appropriate container (del/ins/moveFrom/moveTo).
/// ParagraphOpaque chunks are emitted directly as w:p children.
///
/// If `emitted_opaques` is provided, paragraph-level opaques are deduplicated
/// across segments — this prevents the same hyperlink from appearing twice when
/// it exists in both Deleted and Inserted segments.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn emit_tracked_chunks(
    p: &mut Element,
    chunks: &[TrackedContentChunk<'_>],
    container_kind: &TrackedContainer,
    author: &str,
    date: &str,
    next_id: &mut u32,
    mut emitted_opaques: Option<&mut HashSet<String>>,
    bookmark_policy: &BookmarkIdPolicy,
    origin: &str,
    mut resolve_rel_rid: Option<&mut dyn FnMut(&str, &str) -> String>,
    pending_prefix_sep: &mut Option<String>,
) -> Result<(), RuntimeError> {
    for chunk in chunks {
        match chunk {
            TrackedContentChunk::RunContent(inlines) => {
                // w:delText is only valid inside w:del. Word requires w:t
                // inside w:moveFrom (confirmed by Word's own output and
                // empirical validation against real Word).
                let deleted_text = matches!(container_kind, TrackedContainer::Del);
                let mut container = match container_kind {
                    TrackedContainer::Del => w_del(next_annotation_id(next_id), author, date),
                    TrackedContainer::Ins => w_ins(next_annotation_id(next_id), author, date),
                    TrackedContainer::MoveFrom => {
                        word_xml::w_move_from(next_annotation_id(next_id), author, date)
                    }
                    TrackedContainer::MoveTo => {
                        word_xml::w_move_to(next_annotation_id(next_id), author, date)
                    }
                };
                append_inline_refs_to_container(
                    &mut container,
                    inlines,
                    deleted_text,
                    next_id,
                    bookmark_policy,
                    origin,
                    match resolve_rel_rid.as_mut() {
                        Some(resolver) => {
                            Some(&mut **resolver as &mut dyn FnMut(&str, &str) -> String)
                        }
                        None => None,
                    },
                    pending_prefix_sep,
                )?;
                // Insertion side of a tracked replace: an inline content control
                // cloned into both copies of the redline must not share its
                // `w:sdtPr > w:id` with the deleted original (§17.5.2.18). The
                // deleted/moveFrom copy keeps the source id; re-id the copy here.
                if matches!(
                    container_kind,
                    TrackedContainer::Ins | TrackedContainer::MoveTo
                ) {
                    reassign_inserted_sdt_ids(&mut container, next_id);
                }
                if !container.children.is_empty() {
                    p.children.push(XMLNode::Element(container));
                }
            }
            TrackedContentChunk::DirectInline(inline) => {
                if let InlineNode::OpaqueInline(opaque) = inline
                    && matches!(opaque.kind, OpaqueKind::OmmlBlock)
                    && matches!(
                        container_kind,
                        TrackedContainer::Del
                            | TrackedContainer::Ins
                            | TrackedContainer::MoveFrom
                            | TrackedContainer::MoveTo
                    )
                {
                    append_tracked_omml_paragraph_opaque(
                        p,
                        opaque,
                        container_kind,
                        author,
                        date,
                        next_id,
                    )?;
                    continue;
                }
                if let Some(set) = emitted_opaques.as_deref_mut()
                    && let Some(key) = paragraph_opaque_dedup_key(inline)
                    && !set.insert(key)
                {
                    continue; // already emitted from another segment
                }
                // A paragraph-level hyperlink whose insertion/deletion is tracked
                // at the segment level must wrap its inner runs in the tracked
                // container (ECMA-376 §17.13.5), or Word reads the link as
                // permanent content and accept/reject won't revert it.
                if let InlineNode::OpaqueInline(opaque) = inline
                    && let OpaqueKind::Hyperlink(data) = &opaque.kind
                    && matches!(
                        container_kind,
                        TrackedContainer::Ins | TrackedContainer::Del
                    )
                {
                    append_tracked_hyperlink_paragraph_opaque(
                        p,
                        data,
                        container_kind,
                        author,
                        date,
                        next_id,
                        match resolve_rel_rid.as_mut() {
                            Some(resolver) => {
                                Some(&mut **resolver as &mut dyn FnMut(&str, &str) -> String)
                            }
                            None => None,
                        },
                    );
                    continue;
                }
                // A paragraph-level `fldSimple` whose insertion/deletion is tracked
                // at the segment level must likewise wrap its result run in the
                // tracked container; `<w:fldSimple>` cannot be a child of
                // `<w:ins>`/`<w:del>` (I-TC-001), so the run goes inside the field.
                // Without this Word reads the field as permanent content and
                // accept/reject of a deleted/inserted field would not revert.
                if let InlineNode::OpaqueInline(opaque) = inline
                    && let OpaqueKind::Field(data) = &opaque.kind
                    && data.field_kind == FieldKind::Simple
                    && matches!(
                        container_kind,
                        TrackedContainer::Ins | TrackedContainer::Del
                    )
                {
                    match container_kind {
                        // An INSERTED field lowers to the complex form inside
                        // w:ins — the only shape Word registers as a revision
                        // and reverts on reject (see the helper's WHY).
                        TrackedContainer::Ins => append_tracked_inserted_complex_field(
                            p,
                            data,
                            &opaque.wrapper_marks,
                            &opaque.wrapper_style_props,
                            &opaque.kind,
                            author,
                            date,
                            next_id,
                        ),
                        // A DELETED pre-existing field keeps the historical
                        // shape (field direct on the paragraph, result run
                        // wrapped in w:del). Known gap: Word reads the field
                        // shell as permanent content on reject; lowering the
                        // delete path needs w:delInstrText and its own oracle
                        // pass.
                        _ => append_tracked_simple_field_paragraph_opaque(
                            p,
                            data,
                            &opaque.wrapper_marks,
                            &opaque.wrapper_style_props,
                            container_kind,
                            author,
                            date,
                            next_id,
                        ),
                    }
                    continue;
                }
                append_single_inline(
                    p,
                    inline,
                    false,
                    next_id,
                    bookmark_policy,
                    origin,
                    match resolve_rel_rid.as_mut() {
                        Some(resolver) => {
                            Some(&mut **resolver as &mut dyn FnMut(&str, &str) -> String)
                        }
                        None => None,
                    },
                    pending_prefix_sep,
                )?;
            }
        }
    }
    Ok(())
}

/// Extract the ordered dedup keys of paragraph-level opaques in an inline list.
fn paragraph_opaque_keys(inlines: &[InlineNode]) -> Vec<String> {
    inlines
        .iter()
        .filter(|i| is_paragraph_level_opaque(i))
        .filter_map(paragraph_opaque_dedup_key)
        .collect()
}

/// Check whether two segments share the same paragraph-level opaques (by dedup key, in order).
fn segments_share_opaques(a: &TrackedSegment, b: &TrackedSegment) -> bool {
    let a_keys = paragraph_opaque_keys(&a.inlines);
    if a_keys.is_empty() {
        return false;
    }
    a_keys == paragraph_opaque_keys(&b.inlines)
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn emit_segment(
    p: &mut Element,
    segment: &TrackedSegment,
    emitted_opaques: &mut HashSet<String>,
    next_id: &mut u32,
    bookmark_policy: &BookmarkIdPolicy,
    origin: &str,
    mut resolve_rel_rid: Option<&mut dyn FnMut(&str, &str) -> String>,
    pending_prefix_sep: &mut Option<String>,
) -> Result<(), RuntimeError> {
    match &segment.status {
        TrackingStatus::Normal => {
            append_inlines_to_container(
                p,
                &segment.inlines,
                false,
                next_id,
                bookmark_policy,
                origin,
                match resolve_rel_rid.as_mut() {
                    Some(resolver) => Some(&mut **resolver as &mut dyn FnMut(&str, &str) -> String),
                    None => None,
                },
                pending_prefix_sep,
            )?;
        }
        TrackingStatus::Inserted(rev) => {
            let segment_refs: Vec<&InlineNode> = segment.inlines.iter().collect();
            let chunks = split_tracked_container_chunks(&segment_refs);
            let author = rev.author.as_deref().unwrap_or("");
            let date = rev.date.as_deref().unwrap_or("");
            emit_tracked_chunks(
                p,
                &chunks,
                &TrackedContainer::Ins,
                author,
                date,
                next_id,
                Some(emitted_opaques),
                bookmark_policy,
                origin,
                match resolve_rel_rid.as_mut() {
                    Some(resolver) => Some(&mut **resolver as &mut dyn FnMut(&str, &str) -> String),
                    None => None,
                },
                pending_prefix_sep,
            )?;
        }
        TrackingStatus::Deleted(rev) => {
            let segment_refs: Vec<&InlineNode> = segment.inlines.iter().collect();
            let chunks = split_tracked_container_chunks(&segment_refs);
            let author = rev.author.as_deref().unwrap_or("");
            let date = rev.date.as_deref().unwrap_or("");
            emit_tracked_chunks(
                p,
                &chunks,
                &TrackedContainer::Del,
                author,
                date,
                next_id,
                Some(emitted_opaques),
                bookmark_policy,
                origin,
                match resolve_rel_rid.as_mut() {
                    Some(resolver) => Some(&mut **resolver as &mut dyn FnMut(&str, &str) -> String),
                    None => None,
                },
                pending_prefix_sep,
            )?;
        }
        TrackingStatus::InsertedThenDeleted(sr) => {
            // Canonical stacked emission: ONE
            // nesting order on output — `<w:ins author=A><w:del author=B>
            // <w:r><w:delText>…` — regardless of which legal order the input
            // markup used (del-in-ins or ins-in-del both denote this state).
            // The del chunks are emitted INTO the ins wrapper: Word reads the
            // text as pending-inserted and pending-deleted, resolving per the
            // origin rules (verified against real Word).
            let segment_refs: Vec<&InlineNode> = segment.inlines.iter().collect();
            let chunks = split_tracked_container_chunks(&segment_refs);
            let mut ins_wrapper = w_ins(
                next_annotation_id(next_id),
                sr.inserted.author.as_deref().unwrap_or(""),
                sr.inserted.date.as_deref().unwrap_or(""),
            );
            emit_tracked_chunks(
                &mut ins_wrapper,
                &chunks,
                &TrackedContainer::Del,
                sr.deleted.author.as_deref().unwrap_or(""),
                sr.deleted.date.as_deref().unwrap_or(""),
                next_id,
                Some(emitted_opaques),
                bookmark_policy,
                origin,
                match resolve_rel_rid.as_mut() {
                    Some(resolver) => Some(&mut **resolver as &mut dyn FnMut(&str, &str) -> String),
                    None => None,
                },
                pending_prefix_sep,
            )?;
            if !ins_wrapper.children.is_empty() {
                p.children.push(XMLNode::Element(ins_wrapper));
            }
        }
    }
    Ok(())
}

/// Emit a Del+Ins segment pair interleaved at paragraph-level opaque boundaries.
///
/// When `collapse_zipper_regions` merges interleaved del/ins/equal text into
/// flat del+ins segments, paragraph-level opaques (fldSimple, hyperlink) end up
/// inside both segments at the same positions. Emitting each segment
/// independently places the opaque inside the first segment's output, breaking
/// reading order. This function walks both chunk lists in parallel, emitting
/// del RunContent, then ins RunContent, then the shared opaque at each boundary.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn emit_interleaved_del_ins(
    p: &mut Element,
    del_segment: &TrackedSegment,
    ins_segment: &TrackedSegment,
    emitted_opaques: &mut HashSet<String>,
    next_id: &mut u32,
    bookmark_policy: &BookmarkIdPolicy,
    origin: &str,
    mut resolve_rel_rid: Option<&mut dyn FnMut(&str, &str) -> String>,
) -> Result<(), RuntimeError> {
    let (del_rev, ins_rev) = match (&del_segment.status, &ins_segment.status) {
        (TrackingStatus::Deleted(d), TrackingStatus::Inserted(i)) => (d, i),
        _ => unreachable!("emit_interleaved_del_ins called with non-Del+Ins pair"),
    };
    let del_author = del_rev.author.as_deref().unwrap_or("");
    let del_date = del_rev.date.as_deref().unwrap_or("");
    let ins_author = ins_rev.author.as_deref().unwrap_or("");
    let ins_date = ins_rev.date.as_deref().unwrap_or("");

    let del_refs: Vec<&InlineNode> = del_segment.inlines.iter().collect();
    let ins_refs: Vec<&InlineNode> = ins_segment.inlines.iter().collect();
    let del_chunks = split_tracked_container_chunks(&del_refs);
    let ins_chunks = split_tracked_container_chunks(&ins_refs);

    // Walk both chunk lists. At each opaque boundary:
    // 1. Emit del RunContent in <w:del>
    // 2. Emit ins RunContent in <w:ins>
    // 3. Emit the shared opaque directly on <w:p>
    let mut del_iter = del_chunks.iter().peekable();
    let mut ins_iter = ins_chunks.iter().peekable();

    loop {
        // Emit del RunContent chunks until we hit a ParagraphOpaque or exhaust del
        while let Some(TrackedContentChunk::RunContent(inlines)) = del_iter.peek() {
            let mut container = w_del(next_annotation_id(next_id), del_author, del_date);
            let mut no_prefix_sep: Option<String> = None;
            append_inline_refs_to_container(
                &mut container,
                inlines,
                true,
                next_id,
                bookmark_policy,
                origin,
                match resolve_rel_rid.as_mut() {
                    Some(resolver) => Some(&mut **resolver as &mut dyn FnMut(&str, &str) -> String),
                    None => None,
                },
                &mut no_prefix_sep,
            )?;
            if !container.children.is_empty() {
                p.children.push(XMLNode::Element(container));
            }
            del_iter.next();
        }

        // Emit ins RunContent chunks until we hit a ParagraphOpaque or exhaust ins
        while let Some(TrackedContentChunk::RunContent(inlines)) = ins_iter.peek() {
            let mut container = w_ins(next_annotation_id(next_id), ins_author, ins_date);
            let mut no_prefix_sep: Option<String> = None;
            append_inline_refs_to_container(
                &mut container,
                inlines,
                false,
                next_id,
                bookmark_policy,
                origin,
                match resolve_rel_rid.as_mut() {
                    Some(resolver) => Some(&mut **resolver as &mut dyn FnMut(&str, &str) -> String),
                    None => None,
                },
                &mut no_prefix_sep,
            )?;
            if !container.children.is_empty() {
                p.children.push(XMLNode::Element(container));
            }
            ins_iter.next();
        }

        // Both iterators should now point to direct paragraph children (or be exhausted).
        match (del_iter.peek(), ins_iter.peek()) {
            (
                Some(TrackedContentChunk::DirectInline(del_inline)),
                Some(TrackedContentChunk::DirectInline(ins_inline)),
            ) => {
                if let (Some(del_key), Some(ins_key)) = (
                    paragraph_opaque_dedup_key(del_inline),
                    paragraph_opaque_dedup_key(ins_inline),
                ) && del_key == ins_key
                {
                    emitted_opaques.insert(del_key);
                    let mut no_prefix_sep: Option<String> = None;
                    append_single_inline(
                        p,
                        del_inline,
                        false,
                        next_id,
                        bookmark_policy,
                        origin,
                        match resolve_rel_rid.as_mut() {
                            Some(resolver) => {
                                Some(&mut **resolver as &mut dyn FnMut(&str, &str) -> String)
                            }
                            None => None,
                        },
                        &mut no_prefix_sep,
                    )?;
                    del_iter.next();
                    ins_iter.next();
                    continue;
                }

                let mut no_prefix_sep: Option<String> = None;
                append_single_inline(
                    p,
                    del_inline,
                    false,
                    next_id,
                    bookmark_policy,
                    origin,
                    match resolve_rel_rid.as_mut() {
                        Some(resolver) => {
                            Some(&mut **resolver as &mut dyn FnMut(&str, &str) -> String)
                        }
                        None => None,
                    },
                    &mut no_prefix_sep,
                )?;
                del_iter.next();

                let mut no_prefix_sep: Option<String> = None;
                append_single_inline(
                    p,
                    ins_inline,
                    false,
                    next_id,
                    bookmark_policy,
                    origin,
                    match resolve_rel_rid.as_mut() {
                        Some(resolver) => {
                            Some(&mut **resolver as &mut dyn FnMut(&str, &str) -> String)
                        }
                        None => None,
                    },
                    &mut no_prefix_sep,
                )?;
                ins_iter.next();
            }
            (Some(TrackedContentChunk::DirectInline(inline)), _) => {
                if let Some(key) = paragraph_opaque_dedup_key(inline) {
                    emitted_opaques.insert(key);
                }
                let mut no_prefix_sep: Option<String> = None;
                append_single_inline(
                    p,
                    inline,
                    false,
                    next_id,
                    bookmark_policy,
                    origin,
                    match resolve_rel_rid.as_mut() {
                        Some(resolver) => {
                            Some(&mut **resolver as &mut dyn FnMut(&str, &str) -> String)
                        }
                        None => None,
                    },
                    &mut no_prefix_sep,
                )?;
                del_iter.next();
            }
            (None, Some(TrackedContentChunk::DirectInline(inline))) => {
                if let Some(key) = paragraph_opaque_dedup_key(inline) {
                    emitted_opaques.insert(key);
                }
                let mut no_prefix_sep: Option<String> = None;
                append_single_inline(
                    p,
                    inline,
                    false,
                    next_id,
                    bookmark_policy,
                    origin,
                    match resolve_rel_rid.as_mut() {
                        Some(resolver) => {
                            Some(&mut **resolver as &mut dyn FnMut(&str, &str) -> String)
                        }
                        None => None,
                    },
                    &mut no_prefix_sep,
                )?;
                ins_iter.next();
            }
            _ => break, // Both exhausted
        }
    }

    Ok(())
}

fn opaque_raw_element_requires_run_wrapper(element: &Element) -> bool {
    let name = local_element_name(element);
    // Every run-level widget (word_ir's single classification) is a member of
    // EG_RunInnerContent and is only legal inside `w:r` — it must be re-wrapped
    // on emission. Deriving from `RUN_WIDGET_NAMES` (rather than re-listing the
    // names here) is what kills the drift class: previously `pgNum`/`contentPart`
    // were run widgets on import but missing from this list, so they were emitted
    // bare at paragraph level and Word refused the file. See the invariant test
    // `every_run_widget_requires_a_run_wrapper`.
    crate::word_ir::is_run_widget(name)
        // Serializer-only run-inner content that word_ir models as atoms (text,
        // breaks, tabs) rather than opaque widgets, plus the mc:AlternateContent
        // choice wrapper — none are in RUN_WIDGET_NAMES but all are still only
        // legal inside `w:r`.
        || matches!(name, "t" | "delText" | "br" | "tab" | "AlternateContent")
}

/// Rename a Word element's local name in place, preserving whichever name form
/// the parser produced (embedded `w:foo` vs a bare local with a separate `w`
/// prefix). Both forms occur in parsed raw fragments (see `is_w_tag`).
fn rename_w_local(element: &mut Element, new_local: &str) {
    if element.name.contains(':') {
        element.name = format!("w:{new_local}");
    } else {
        element.name = new_local.to_string();
    }
}

/// Coerce run-content text throughout an opaque raw-XML subtree to match the
/// tracked-change container it is being emitted into.
///
/// The deleted↔plain run-content pairs are `w:t`↔`w:delText` (ECMA-376
/// §17.4.20) and `w:instrText`↔`w:delInstrText` (§17.16.13) — the I-TC-001
/// tracked-change content model. The mapping is shared with the reject-side
/// restore in `normalize` (see [`crate::normalize::DELETED_RUN_CONTENT_PAIRS`])
/// so the two directions cannot drift apart.
///
/// - `deleted = true`: the opaque is emitted inside `w:del`, so its own
///   descendant runs must use the deleted forms (`w:t`→`w:delText`,
///   `w:instrText`→`w:delInstrText`). Opaque raw XML such as an inline `w:sdt`
///   carries descendant runs; leaving their `w:t`/`w:instrText` plain inside the
///   delete makes Word open the file only after repair and crashes a
///   programmatic accept-all.
/// - `deleted = false`: the opaque is emitted as PLAIN content — either never
///   deleted, restored by a reject, or a `w:moveFrom` (which keeps `w:t`). Its
///   descendant runs must use the plain forms (`w:delText`→`w:t`,
///   `w:delInstrText`→`w:instrText`). Without this inverse, a `w:delInstrText`
///   captured into the opaque's `raw_xml` from a since-rejected `w:del` would
///   leak into a non-deleted run — schema-invalid, and Word repairs the file.
///
/// `w:txbxContent` is deliberately NOT descended into: a textbox is a separate
/// story whose runs stay `w:t` even when the drawing that holds it is deleted
/// (Word accepts that — verified against real Word).
fn coerce_opaque_run_text(element: &mut Element, deleted: bool) {
    let local = local_element_name(element);
    // Boundaries where descent stops because the run-content text form is
    // governed by the nested container, not the one being emitted:
    // - `w:txbxContent` is a separate story (its runs stay `w:t`), both ways.
    // - `w:del` (inverse only): a PRESERVED nested deletion keeps its own
    //   `w:delText`/`w:delInstrText`; converting them to plain here would
    //   corrupt an unresolved nested tracked deletion. (Forward keeps the prior
    //   behavior of descending — a nested del's content is already deleted-form.)
    let is_boundary = local == "txbxContent" || (!deleted && local == "del");
    if is_boundary {
        return;
    }
    // Coerce this element if it IS a run-content element. A bare run-widget
    // opaque (e.g. a `w:delInstrText`) is its own `raw_xml` root, so the element
    // handed in — not only its descendants — may be the one to rewrite. A
    // matched element is a text leaf; there is nothing further to descend into.
    let mapped = if deleted {
        crate::normalize::deleted_run_content_name(local)
    } else {
        crate::normalize::plain_run_content_name(local)
    };
    if let Some(new_local) = mapped {
        rename_w_local(element, new_local);
        return;
    }
    for child in element.children.iter_mut() {
        if let XMLNode::Element(child_el) = child {
            coerce_opaque_run_text(child_el, deleted);
        }
    }
}

/// Reassign every `w:sdt` descendant's `w:sdtPr > w:id` (§17.5.2.18) to a fresh
/// document-unique value. The mirror of [`convert_deleted_opaque_run_text`] for
/// the insertion side: applied to opaque content emitted inside a
/// `w:ins`/`w:moveTo` container.
///
/// A tracked *replace* of an inline content control clones the source SDT into
/// BOTH copies of the redline — the original inside `w:del`/`w:moveFrom` and the
/// replacement inside `w:ins`/`w:moveTo` — so both carry the SAME id. Two live
/// structured-document tags claiming one identity is incoherent while both exist
/// (before accept/reject resolves the redline), and Word restructures the SDT on
/// save, a text-identical but structure-observable drift. The deleted copy keeps
/// the source id (it survives reject and must restore the source byte-shape); the
/// inserted copy — which survives accept — takes a new id here.
///
/// `next_id` is the document-wide annotation-id counter, seeded above every
/// existing annotation `w:id` AND SDT id in the base/target parts (see
/// `runtime::max_sdt_id_in_archive`), so every value allocated here is unique
/// across the whole output. Nested SDTs are re-id'd too — each is its own
/// control and is duplicated the same way.
fn reassign_inserted_sdt_ids(element: &mut Element, next_id: &mut u32) {
    if local_element_name(element) == "sdt"
        && let Some(sdt_pr) = element.children.iter_mut().find_map(|child| match child {
            XMLNode::Element(el) if local_element_name(el) == "sdtPr" => Some(el),
            _ => None,
        })
        && let Some(id_el) = sdt_pr.children.iter_mut().find_map(|child| match child {
            XMLNode::Element(el) if local_element_name(el) == "id" => Some(el),
            _ => None,
        })
    {
        attr_set(id_el, "w:val", next_annotation_id(next_id).to_string());
    }
    for child in element.children.iter_mut() {
        if let XMLNode::Element(child_el) = child {
            reassign_inserted_sdt_ids(child_el, next_id);
        }
    }
}

/// Decoration elements that are run-level content (children of CT_R) and
/// need a `w:r` wrapper when emitted. Without it they'd be invalid direct
/// children of w:p or w:ins/w:del.
fn decoration_requires_run_wrapper(element: &Element) -> bool {
    matches!(
        local_element_name(element),
        "footnoteRef"
            | "endnoteRef"
            | "separator"
            | "continuationSeparator"
            | "annotationRef"
            | "lastRenderedPageBreak"
            | "noBreakHyphen"
            | "softHyphen"
    )
}

// =============================================================================
// Bookmark / move-range id policy
// =============================================================================
//
// Domain model (ECMA-376 Part 1 §17.13.6): a bookmark's IDENTITY is its
// `w:name` — unique per document. The `w:id` is only a part-local pairing key
// linking one `bookmarkStart` to one `bookmarkEnd` (§17.13.2 cross-structure
// annotations); the halves may sit at run level or between paragraphs and
// take DIFFERENT emission paths (rebuilt paragraphs vs raw-preserved body
// children). The invariant the policy preserves:
//
//   I1 (no torn pairs): any id rewrite or drop applies to BOTH halves of a
//   pair, on every emission path.
//
// Rules (each per serialized part):
// - Base-origin markers keep their original ids, verbatim, everywhere. This
//   keeps pairs intact across mixed emission paths by construction and keeps
//   pure roundtrips byte-stable. (Raw-preserved body children and opaque
//   inline fragments are base content, so they never need rewriting.)
// - Target-origin markers exist only inside merged output (inserted blocks /
//   injected markers). Their ids come from a DIFFERENT document's id space,
//   so a kept pair gets ONE fresh id applied to both halves.
// - A target-origin bookmark whose NAME matches a base-emitted bookmark name
//   is the SAME bookmark (name = identity; §17.13.6.2 name attribute:
//   duplicate names — first maintained, subsequent ignored). Both halves are
//   dropped: we do not emit markup Word is required to ignore.
// - A target-origin half whose other half does not materialize in this part
//   (merge alignment carried only one side) is dropped: a lone half can only
//   corrupt the part-local id pairing. This is a defined, test-covered rule
//   (see tests/redline_bookmark_identity.rs T2), not a fallback.
// - Authored markers (edit verbs, placeholder ids) are always remapped to a
//   fresh pair id; an unbalanced authored pair is a verb bug and refuses.
//
// `commentRangeStart`/`commentRangeEnd` are deliberately NOT covered — their
// ids must match `w:comment` elements in comments.xml.

/// Paired-range element families. Pairing is per family: a `bookmarkStart`
/// never pairs with a `moveFromRangeEnd`, so the policy keys include the
/// family (the old single map keyed only by id and could cross-contaminate).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum RangeFamily {
    Bookmark,
    MoveFrom,
    MoveTo,
}

fn range_family_of(local_name: &str) -> Option<(RangeFamily, bool)> {
    match local_name {
        "bookmarkStart" => Some((RangeFamily::Bookmark, true)),
        "bookmarkEnd" => Some((RangeFamily::Bookmark, false)),
        "moveFromRangeStart" => Some((RangeFamily::MoveFrom, true)),
        "moveFromRangeEnd" => Some((RangeFamily::MoveFrom, false)),
        "moveToRangeStart" => Some((RangeFamily::MoveTo, true)),
        "moveToRangeEnd" => Some((RangeFamily::MoveTo, false)),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RangeIdAction {
    /// Rewrite the id on both halves to this fresh, part-unique value.
    Remap(u32),
    /// Do not emit either half.
    Drop,
    /// Emit verbatim, keeping the original id. Used for a target-origin half
    /// that pairs with a BASE half of the same id (a range straddling an origin
    /// boundary): the base half keeps its id, so the target half must too, or
    /// the pair tears.
    Keep,
    /// Refuse the serialization: an AUTHORED half-pair means an edit verb
    /// produced an unbalanced bookmark — engine bug, fail loud.
    Refuse,
}

/// Non-base origin classes whose ids the policy rewrites. Part of the policy
/// key so a target id and an authored placeholder id with the same numeric
/// value stay independent pairings.
///
/// - `Target`: decorations carried from the merge's target document
///   (inserted blocks, injected markers). Ids come from a foreign id space;
///   bookmark names dedup against base (same name = same bookmark).
/// - `Authored`: decorations synthesized by edit verbs
///   (`edit/verbs/bookmarks.rs`) under a deliberate placeholder id with the
///   documented contract "the serializer reassigns the real id at write
///   time". Always remapped, never name-deduped (the verb already enforces
///   name uniqueness; authored content is not "the other side's copy" of an
///   existing bookmark).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum OriginClass {
    Target,
    Authored,
}

fn origin_class_of(origin: &str) -> Option<OriginClass> {
    match origin {
        "target" => Some(OriginClass::Target),
        "authored" => Some(OriginClass::Authored),
        // "base" (the only other origin the pipeline produces) — preserved
        // verbatim.
        _ => None,
    }
}

/// Pre-scan of everything a part's serialization will emit. Collects the
/// base-emitted bookmark names (the part's name identity set) and the
/// target/authored range halves, so `into_policy` can decide each pair once
/// and apply it consistently to both halves.
#[derive(Default)]
pub(crate) struct BookmarkScan {
    base_bookmark_names: HashSet<String>,
    /// Base-emitted range halves as `(family, is_start, id)`. Base ids are kept
    /// verbatim, so these are the part's live pairing keys. A target-origin half
    /// whose id matches a base half of the COMPLEMENTARY role is not lone — it is
    /// the target-side copy of a pair whose range STRADDLES an origin boundary
    /// (e.g. a bookmark whose start is live/base while its end rides a `w:moveTo`
    /// clone that kept the base id). Dropping it as "lone" orphans the base half
    /// (orphaning the base half). Keyed on `(family, is_start, id)`.
    base_range_halves: HashSet<(RangeFamily, bool, String)>,
    /// (class, family, old id, name) per non-base start, in scan order.
    remap_starts: Vec<(OriginClass, RangeFamily, String, Option<String>)>,
    remap_end_ids: HashSet<(OriginClass, RangeFamily, String)>,
}

impl BookmarkScan {
    /// Scan tracked blocks exactly as `serialize_tracked_block` will emit
    /// them: block origin is "target" for Inserted blocks, "base" otherwise,
    /// with the per-decoration `DecorationNode::origin` override.
    pub(crate) fn scan_tracked_blocks(&mut self, blocks: &[crate::domain::TrackedBlock]) {
        for tracked in blocks {
            let origin = match &tracked.status {
                TrackingStatus::Inserted(_) => "target",
                TrackingStatus::Normal
                | TrackingStatus::Deleted(_)
                | TrackingStatus::InsertedThenDeleted(_) => "base",
            };
            self.scan_block(&tracked.block, origin);
        }
    }

    fn scan_block(&mut self, block: &BlockNode, origin: &str) {
        match block {
            BlockNode::Paragraph(p) => {
                for segment in &p.segments {
                    for inline in &segment.inlines {
                        self.scan_inline(inline, origin);
                    }
                }
            }
            BlockNode::Table(t) => {
                for row in &t.rows {
                    for cell in &row.cells {
                        for cell_block in &cell.blocks {
                            self.scan_block(cell_block, origin);
                        }
                    }
                }
            }
            // Raw-preserved body children are scanned by the caller via
            // `scan_raw_node` (they are emitted from the body template, not
            // from the block model).
            BlockNode::OpaqueBlock(_) => {}
        }
    }

    fn scan_inline(&mut self, inline: &InlineNode, block_origin: &str) {
        match inline {
            InlineNode::Decoration(deco) => {
                let Some(raw_xml) = &deco.raw_xml else { return };
                let effective_origin = deco.origin.as_deref().unwrap_or(block_origin);
                if let Ok(el) = crate::word_xml::parse_raw_fragment(raw_xml.as_slice()) {
                    self.record_element_tree(&el, effective_origin);
                }
            }
            InlineNode::OpaqueInline(opaque) => {
                // Opaque inline fragments are emitted verbatim (ids
                // preserved), but bookmarks buried inside BASE fragments
                // still claim their names in this part's identity set so a
                // same-named target bookmark dedups against them. Target
                // fragments are skipped: we never rewrite inside them, so
                // they contribute nothing the policy can act on.
                if block_origin != "target"
                    && let Some(raw_xml) = &opaque.raw_xml
                    && bytes_contains(raw_xml, b"bookmarkStart")
                    && let Ok(el) = crate::word_xml::parse_raw_fragment(raw_xml.as_slice())
                {
                    self.record_element_tree(&el, "base");
                }
            }
            _ => {}
        }
    }

    /// Scan a raw body-template node (raw-preserved body children are always
    /// base content: target-origin opaque blocks are never emitted).
    pub(crate) fn scan_raw_node(&mut self, node: &XMLNode) {
        if let XMLNode::Element(el) = node {
            self.record_element_tree(el, "base");
        }
    }

    fn record_element_tree(&mut self, el: &Element, origin: &str) {
        self.record_element(el, origin);
        for child in &el.children {
            if let XMLNode::Element(child_el) = child {
                self.record_element_tree(child_el, origin);
            }
        }
    }

    fn record_element(&mut self, el: &Element, origin: &str) {
        let Some((family, is_start)) = range_family_of(local_element_name(el)) else {
            return;
        };
        let Some(id) = attr_get(el, "w:id") else {
            return;
        };
        let Some(class) = origin_class_of(origin) else {
            // Base ids are preserved verbatim. The bookmark NAME defines the
            // part's name identity set; the (family, role, id) is a live pairing
            // key a straddling target half can pair with (see `base_range_halves`).
            if is_start
                && family == RangeFamily::Bookmark
                && let Some(name) = attr_get(el, "w:name")
            {
                self.base_bookmark_names.insert(name.clone());
            }
            self.base_range_halves
                .insert((family, is_start, id.clone()));
            return;
        };
        if is_start {
            self.remap_starts
                .push((class, family, id.clone(), attr_get(el, "w:name").cloned()));
        } else {
            self.remap_end_ids.insert((class, family, id.clone()));
        }
    }

    /// True when a target-origin bookmark half of `id` is the ONLY half on its
    /// side and pairs with a lone BASE half of the complementary role — i.e. the
    /// range straddles an origin boundary and the target half must be kept
    /// verbatim (not dropped as lone). Requires the base to carry the
    /// COMPLEMENTARY half but NOT the matching one: if the base already has BOTH
    /// halves the pair is intact in base and this target half is a redundant
    /// duplicate that must still be dropped (the `t2` rule — a pair spanning an
    /// unchanged + edited paragraph carries the end in both copies).
    ///
    /// Scoped to `Bookmark`: move-range halves carry their own materialization
    /// (`move_bookmark_ids`), and `Authored` halves are always verb-balanced.
    fn straddles_base_half(
        &self,
        class: OriginClass,
        family: RangeFamily,
        is_start: bool,
        id: &str,
    ) -> bool {
        class == OriginClass::Target
            && family == RangeFamily::Bookmark
            && self
                .base_range_halves
                .contains(&(family, !is_start, id.to_string()))
            && !self
                .base_range_halves
                .contains(&(family, is_start, id.to_string()))
    }

    /// Decide every target/authored pair once. Fresh ids come from the shared
    /// annotation counter, which the caller seeded above every w:id in both
    /// input archives — so a remapped id can never collide with a kept one.
    pub(crate) fn into_policy(self, next_id: &mut u32) -> BookmarkIdPolicy {
        let mut actions: HashMap<(OriginClass, RangeFamily, String), RangeIdAction> =
            HashMap::new();
        for (class, family, old_id, name) in &self.remap_starts {
            let key = (*class, *family, old_id.clone());
            if actions.contains_key(&key) {
                // Duplicate start ids within one class (the SOURCE document
                // was already ambiguous about this pairing). First decision
                // wins so the ambiguity is mirrored 1:1 instead of torn.
                continue;
            }
            let same_name_as_base = *class == OriginClass::Target
                && *family == RangeFamily::Bookmark
                && name
                    .as_ref()
                    .is_some_and(|n| self.base_bookmark_names.contains(n));
            let action = if same_name_as_base {
                // Same NAME as a base-emitted bookmark: same identity, the
                // base pair already represents it (§17.13.6.2 name attr).
                RangeIdAction::Drop
            } else if self.remap_end_ids.contains(&key) {
                // Complete pair: keep it under one fresh part-unique id.
                RangeIdAction::Remap(next_annotation_id(next_id))
            } else if self.straddles_base_half(*class, *family, /*is_start=*/ true, old_id) {
                // Its end lives in a BASE half of the same id — the range
                // straddles an origin boundary (e.g. a moved bookmark whose end
                // kept the base id). Keep this half verbatim so it pairs with the
                // base end.
                RangeIdAction::Keep
            } else {
                // Lone start — its end did not materialize in this part.
                // Merge alignment can legitimately carry one target half;
                // an unbalanced AUTHORED pair is a verb bug.
                match class {
                    OriginClass::Target => RangeIdAction::Drop,
                    OriginClass::Authored => RangeIdAction::Refuse,
                }
            };
            actions.insert(key, action);
        }
        // Ends whose start never materialized: lone halves.
        for key in &self.remap_end_ids {
            let (class, family, id) = key;
            let action = if self.straddles_base_half(*class, *family, /*is_start=*/ false, id) {
                // Its start lives in a BASE half of the same id — a straddling
                // range (the move-clone case: base start + moveTo-clone
                // end). Keep verbatim to pair with the base start.
                RangeIdAction::Keep
            } else {
                match class {
                    OriginClass::Target => RangeIdAction::Drop,
                    OriginClass::Authored => RangeIdAction::Refuse,
                }
            };
            actions.entry(key.clone()).or_insert(action);
        }
        BookmarkIdPolicy { actions }
    }
}

/// The per-part id policy consulted at emission time. Default (empty) policy
/// = "no target/authored markers in this part": base markers always pass
/// through untouched.
#[derive(Default)]
pub(crate) struct BookmarkIdPolicy {
    actions: HashMap<(OriginClass, RangeFamily, String), RangeIdAction>,
}

fn bytes_contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Whether to emit a decoration element after applying the id policy.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DecorationEmit {
    Emit,
    Skip,
}

/// Apply the part's bookmark/move-range id policy to a decoration element.
///
/// Base-origin markers are emitted verbatim (original ids — pairs stay intact
/// across emission paths by construction). Target-origin markers follow the
/// pre-scanned policy: remap both halves to the same fresh id, or drop both
/// halves. A target-origin marker the pre-scan never saw is a programmer bug
/// (scan/emission drift) and fails loudly rather than emitting a half-pair.
fn apply_decoration_id_policy(
    element: &mut Element,
    policy: &BookmarkIdPolicy,
    origin: &str,
) -> Result<DecorationEmit, RuntimeError> {
    let local_name = local_element_name(element);
    // Comment ranges (commentRangeStart/End) are NOT touched here — their
    // IDs must match w:comment elements in comments.xml.
    let Some((family, _is_start)) = range_family_of(local_name) else {
        return Ok(DecorationEmit::Emit);
    };
    let Some(old_id) = attr_get(element, "w:id").cloned() else {
        return Ok(DecorationEmit::Emit);
    };
    let Some(class) = origin_class_of(origin) else {
        return Ok(DecorationEmit::Emit);
    };
    match policy.actions.get(&(class, family, old_id.clone())) {
        Some(RangeIdAction::Remap(new_id)) => {
            attr_set(element, "w:id", new_id.to_string());
            Ok(DecorationEmit::Emit)
        }
        // Straddling pair: emit verbatim so the target half keeps the base id it
        // pairs with.
        Some(RangeIdAction::Keep) => Ok(DecorationEmit::Emit),
        Some(RangeIdAction::Drop) => Ok(DecorationEmit::Skip),
        Some(RangeIdAction::Refuse) => Err(RuntimeError {
            code: ErrorCode::InternalError,
            message: format!(
                "authored bookmark pair is unbalanced: <w:{local_name}> w:id={old_id} \
                 has no matching half in this part — edit-verb bug, refusing to emit \
                 a torn pair (ECMA-376 §17.13.6 id pairing)"
            ),
            details: ErrorDetails::default(),
        }),
        None => Err(RuntimeError {
            code: ErrorCode::InternalError,
            message: format!(
                "bookmark id policy has no entry for {class:?}-origin <w:{local_name}> \
                 w:id={old_id} — pre-scan/emission drift (engine bug)"
            ),
            details: ErrorDetails::default(),
        }),
    }
}

/// Return the character style name for footnote/endnote reference runs.
/// Word expects these references to have w:rStyle so the superscript
/// formatting is applied.
fn note_reference_style_name(kind: &OpaqueKind) -> Option<&'static str> {
    match kind {
        OpaqueKind::FootnoteReference(_) => Some("FootnoteReference"),
        OpaqueKind::EndnoteReference(_) => Some("EndnoteReference"),
        _ => None,
    }
}

fn build_wrapper_rpr(
    marks: &[Mark],
    style_props: &StyleProps,
    kind: &OpaqueKind,
) -> Option<Element> {
    let mut rpr = if !marks.is_empty() || !style_props.is_empty() {
        build_rpr(marks, style_props)
    } else {
        w_el("rPr")
    };

    if let Some(style_val) = note_reference_style_name(kind)
        && style_props.char_style_id.is_none()
    {
        let mut rstyle = w_el("rStyle");
        attr_set(&mut rstyle, "w:val", style_val);
        rpr.children.insert(0, XMLNode::Element(rstyle));
    }

    (!rpr.children.is_empty()).then_some(rpr)
}

// =============================================================================
// Run-level serialization
// =============================================================================

pub(crate) fn build_text_run(
    text: &str,
    marks: &[Mark],
    style_props: &StyleProps,
    deleted_text: bool,
    formatting_change: Option<&FormattingChange>,
    next_id: &mut u32,
) -> Element {
    // Callers of this helper serialize non-run-provenance text (opaque field
    // result text, etc.): no `has_direct_*` flags exist, so emit props as-is.
    build_text_run_with_leading_tabs(
        text,
        marks,
        style_props,
        RunDirectness::ALL,
        deleted_text,
        formatting_change,
        next_id,
        None,
    )
}

/// Build a w:rPr element with children in ECMA-376 §17.3.2.28 conventional order.
///
/// Emit a MarkValue tri-state field as an XML element.
/// On → `<w:name/>`, Off → `<w:name w:val="0"/>`, Inherit → nothing.
fn emit_mark_value(parent: &mut Element, name: &str, value: &MarkValue) {
    match value {
        MarkValue::On => {
            parent.children.push(XMLNode::Element(w_el(name)));
        }
        MarkValue::Off => {
            let mut el = w_el(name);
            attr_set(&mut el, "w:val", "0");
            parent.children.push(XMLNode::Element(el));
        }
        MarkValue::Inherit => {}
    }
}

/// CT_RPr references EG_RPrBase as xsd:choice maxOccurs="unbounded" — technically
/// any order is schema-valid. However, Word always emits elements in a conventional
/// order and some consumers depend on it.
///
/// Order: rStyle(1) → rFonts(2) → b(3) → bCs(4) → i(5) → iCs(6) → caps(7) → smallCaps(8) →
///        strike(9) → dstrike(10) → outline(11) → shadow(12) → emboss(13) →
///        imprint(14) → vanish(17) → color(19) → spacing(20) → sz(24) → szCs(25) →
///        highlight(26) → u(27) → vertAlign(31) → rtl(32) → cs(33) → bdr(28) → lang(35) →
///        [rPrChange(40)]
/// Re-insert preserved (unmodeled) property-container children — rPr's
/// `StyleProps::preserved` and pPr's `ParagraphNode::preserved_ppr` both use
/// this — at their schema-correct position in `parent`, per `order_table`
/// (`docx_validate_ordering::RPR_ORDER` / `PPR_ORDER`).
///
/// Each preserved child is placed just before the first existing child whose
/// order-table index is greater than its own; a child already present but
/// absent from the table (e.g. an earlier-inserted foreign-namespace
/// preserved child) is treated as ordering after every table entry, so a
/// later known-index insert still lands before it. A preserved child whose
/// own name isn't in the table either is appended at the end of `parent`.
fn insert_preserved_children(
    parent: &mut Element,
    preserved: &[crate::domain::PreservedProp],
    order_table: &[&str],
) {
    for prop in preserved {
        let el = crate::word_xml::parse_raw_fragment(prop.raw_xml.as_bytes()).unwrap_or_else(|e| {
            panic!(
                "preserved child {:?} failed to reparse its own captured bytes: {e:?}",
                prop.name
            )
        });
        let local = prop
            .name
            .rsplit_once(':')
            .map_or(prop.name.as_str(), |(_, l)| l);
        let order_idx = order_table.iter().position(|n| *n == local);
        let insert_at = match order_idx {
            Some(idx) => parent
                .children
                .iter()
                .position(|c| match c {
                    XMLNode::Element(existing) => {
                        let existing_idx = order_table
                            .iter()
                            .position(|n| *n == existing.name.as_str())
                            .unwrap_or(usize::MAX);
                        existing_idx > idx
                    }
                    _ => false,
                })
                .unwrap_or(parent.children.len()),
            None => parent.children.len(),
        };
        parent.children.insert(insert_at, XMLNode::Element(el));
    }
}

pub(crate) fn build_rpr(marks: &[Mark], style_props: &StyleProps) -> Element {
    let mut rpr = w_el("rPr");

    // ── Exhaustive field witness ────────────────────────────────────────
    // Every StyleProps field MUST be listed here WITHOUT `..`.
    // Adding a field to StyleProps without listing it = compile error.
    let StyleProps {
        font_family: _,          // → rFonts (pos 2)
        font_family_theme: _,    // → rFonts (pos 2) asciiTheme/hAnsiTheme
        font_size: _,            // → sz (pos 24)
        color: _,                // → color (pos 19)
        color_theme: _,          // → color (pos 19) themeColor/themeShade/themeTint
        highlight: _,            // → highlight (pos 26)
        underline_style: _,      // → u (pos 27) — style for w:val attr
        font_east_asia: _,       // → rFonts (pos 2)
        font_east_asia_theme: _, // → rFonts (pos 2) eastAsiaTheme
        font_cs: _,              // → rFonts (pos 2)
        font_cs_theme: _,        // → rFonts (pos 2) csTheme
        lang: _,                 // → lang (pos 35)
        lang_east_asia: _,       // → lang (pos 35)
        char_spacing: _,         // → spacing (pos 20)
        char_style_id: _,        // → rStyle (pos 1)
        run_border: _,           // → bdr (pos 28)
        position: _,             // → position (pos 23)
        kern: _,                 // → kern (pos 22)
        char_width_scaling: _,   // → w (pos 21)
        bold_cs: _,              // → bCs (pos 4)
        italic_cs: _,            // → iCs (pos 6)
        strike: _,               // → strike (pos 9)
        double_strike: _,        // → dstrike (pos 10)
        caps: _,                 // → caps (pos 7)
        small_caps: _,           // → smallCaps (pos 8)
        vanish: _,               // → vanish (pos 17)
        web_hidden: _,           // → webHidden (pos 18)
        emboss: _,               // → emboss (pos 13)
        imprint: _,              // → imprint (pos 14)
        outline: _,              // → outline (pos 11)
        shadow: _,               // → shadow (pos 12)
        font_size_cs: _,         // → szCs (pos 25)
        rtl: _,                  // → rtl (pos 32)
        cs: _,                   // → cs (pos 33)
        font_hint: _,            // → rFonts (pos 2) hint
        no_proof: _,             // → noProof (pos 15)
        spec_vanish: _,          // → specVanish (pos 38)
        o_math: _,               // → oMath (pos 39)
        snap_to_grid: _,         // → snapToGrid (pos 16)
        run_shading: _,          // → shd (pos 30)
        emphasis_mark: _,        // → em (pos 35)
        text_effect: _,          // → effect (pos 28)
        fit_text: _,             // → fitText (pos 31)
        preserved: _,            // → post-pass below: RPR_ORDER position, or end of rPr
    } = style_props;

    // --- Position 1: rStyle ---
    if let Some(ref style_id) = style_props.char_style_id {
        let mut el = w_el("rStyle");
        attr_set(&mut el, "w:val", style_id.clone());
        rpr.children.push(XMLNode::Element(el));
    }

    // --- Position 2: rFonts ---
    // Per §17.3.2.26, theme attributes take precedence over direct names.
    // When both are present, we emit both: the theme attr for theme-aware readers,
    // and the direct attr as fallback for non-theme-aware readers.
    if style_props.font_family.is_some()
        || style_props.font_family_theme.is_some()
        || style_props.font_east_asia.is_some()
        || style_props.font_east_asia_theme.is_some()
        || style_props.font_cs.is_some()
        || style_props.font_cs_theme.is_some()
        || style_props.font_hint.is_some()
    {
        let mut el = w_el("rFonts");
        if let Some(ref t) = style_props.font_family_theme {
            attr_set(&mut el, "w:asciiTheme", t.clone());
            attr_set(&mut el, "w:hAnsiTheme", t.clone());
        }
        if let Some(ref f) = style_props.font_family {
            attr_set(&mut el, "w:ascii", f.clone());
            attr_set(&mut el, "w:hAnsi", f.clone());
        }
        if let Some(ref t) = style_props.font_east_asia_theme {
            attr_set(&mut el, "w:eastAsiaTheme", t.clone());
        }
        if let Some(ref f) = style_props.font_east_asia {
            attr_set(&mut el, "w:eastAsia", f.clone());
        }
        if let Some(ref t) = style_props.font_cs_theme {
            // §17.3.2.26: the normative attribute is lowercase-t `cstheme` (the
            // lone exception among the *Theme font attrs). Import reads `cstheme`
            // (word_ir.rs), so writing camelCase `csTheme` would round-trip-drop
            // it on the next read and churn ~1k attrs per save. Match the reader.
            attr_set(&mut el, "w:cstheme", t.clone());
        }
        if let Some(ref f) = style_props.font_cs {
            attr_set(&mut el, "w:cs", f.clone());
        }
        if let Some(ref h) = style_props.font_hint {
            attr_set(&mut el, "w:hint", h.clone());
        }
        rpr.children.push(XMLNode::Element(el));
    }

    // --- Positions 3–17: boolean marks in conventional order ---
    // Iterate in spec order, not input order, to guarantee correct output.
    // bCs (pos 4) and iCs (pos 6) are MarkValue tri-state fields on StyleProps,
    // inserted after their non-CS counterparts.

    // 3. b
    if marks.contains(&Mark::Bold) {
        rpr.children.push(XMLNode::Element(w_el("b")));
    }
    // 4. bCs
    emit_mark_value(&mut rpr, "bCs", &style_props.bold_cs);
    // 5. i
    if marks.contains(&Mark::Italic) {
        rpr.children.push(XMLNode::Element(w_el("i")));
    }
    // 6. iCs
    emit_mark_value(&mut rpr, "iCs", &style_props.italic_cs);

    // 7. caps
    emit_mark_value(&mut rpr, "caps", &style_props.caps);
    // 8. smallCaps
    emit_mark_value(&mut rpr, "smallCaps", &style_props.small_caps);
    // 9. strike
    emit_mark_value(&mut rpr, "strike", &style_props.strike);
    // 10. dstrike
    emit_mark_value(&mut rpr, "dstrike", &style_props.double_strike);
    // 11. outline
    emit_mark_value(&mut rpr, "outline", &style_props.outline);
    // 12. shadow
    emit_mark_value(&mut rpr, "shadow", &style_props.shadow);
    // 13. emboss
    emit_mark_value(&mut rpr, "emboss", &style_props.emboss);
    // 14. imprint
    emit_mark_value(&mut rpr, "imprint", &style_props.imprint);

    // --- Position 15: noProof ---
    emit_mark_value(&mut rpr, "noProof", &style_props.no_proof);

    // --- Position 16: snapToGrid ---
    emit_mark_value(&mut rpr, "snapToGrid", &style_props.snap_to_grid);

    // 17. vanish
    emit_mark_value(&mut rpr, "vanish", &style_props.vanish);

    // 18. webHidden
    emit_mark_value(&mut rpr, "webHidden", &style_props.web_hidden);

    // --- Position 19: color ---
    // Emit w:color when we have a literal color value OR theme color attributes.
    // Per §17.3.2.6, themeColor takes precedence; w:val is a pre-resolved fallback.
    if style_props.color.is_some() || style_props.color_theme.is_some() {
        let mut el = w_el("color");
        if let Some(ref color) = style_props.color {
            attr_set(&mut el, "w:val", color.clone());
        }
        if let Some(ref tc) = style_props.color_theme {
            attr_set(&mut el, "w:themeColor", tc.theme_color.clone());
            if let Some(ref shade) = tc.theme_shade {
                attr_set(&mut el, "w:themeShade", shade.clone());
            }
            if let Some(ref tint) = tc.theme_tint {
                attr_set(&mut el, "w:themeTint", tint.clone());
            }
        }
        rpr.children.push(XMLNode::Element(el));
    }

    // --- Position 20: spacing (character spacing) ---
    if let Some(spacing) = style_props.char_spacing {
        let mut el = w_el("spacing");
        attr_set(&mut el, "w:val", spacing.to_string());
        rpr.children.push(XMLNode::Element(el));
    }

    // --- Position 21: w (character width scaling) ---
    if let Some(scaling) = style_props.char_width_scaling {
        let mut el = w_el("w");
        attr_set(&mut el, "w:val", scaling.to_string());
        rpr.children.push(XMLNode::Element(el));
    }

    // --- Position 22: kern (kerning threshold) ---
    if let Some(kern) = style_props.kern {
        let mut el = w_el("kern");
        attr_set(&mut el, "w:val", kern.to_string());
        rpr.children.push(XMLNode::Element(el));
    }

    // --- Position 23: position (vertical displacement) ---
    if let Some(pos) = style_props.position {
        let mut el = w_el("position");
        attr_set(&mut el, "w:val", pos.to_string());
        rpr.children.push(XMLNode::Element(el));
    }

    // --- Position 24: sz ---
    if let Some(size) = style_props.font_size {
        let mut el = w_el("sz");
        attr_set(&mut el, "w:val", size.to_string());
        rpr.children.push(XMLNode::Element(el));
    }

    // --- Position 25: szCs ---
    if let Some(size_cs) = style_props.font_size_cs {
        let mut el = w_el("szCs");
        attr_set(&mut el, "w:val", size_cs.to_string());
        rpr.children.push(XMLNode::Element(el));
    }

    // --- Position 26: highlight ---
    if let Some(ref hl) = style_props.highlight {
        let mut el = w_el("highlight");
        attr_set(&mut el, "w:val", hl.to_xml_str());
        rpr.children.push(XMLNode::Element(el));
    }

    // --- Position 27: u (underline) ---
    if marks.contains(&Mark::Underline) {
        let mut u = w_el("u");
        let val = style_props
            .underline_style
            .as_ref()
            .map(|s| s.to_xml_str())
            .unwrap_or("single");
        attr_set(&mut u, "w:val", val);
        rpr.children.push(XMLNode::Element(u));
    }

    // --- Position 28: effect ---
    if let Some(ref eff) = style_props.text_effect {
        let mut el = w_el("effect");
        attr_set(&mut el, "w:val", eff.to_xml_str());
        rpr.children.push(XMLNode::Element(el));
    }

    // --- Position 29: bdr (run border) ---
    if let Some(ref border) = style_props.run_border {
        let mut el = w_el("bdr");
        attr_set(&mut el, "w:val", border.style.clone());
        attr_set(&mut el, "w:sz", border.size.to_string());
        attr_set(&mut el, "w:space", border.space.to_string());
        attr_set(&mut el, "w:color", border.color.clone());
        rpr.children.push(XMLNode::Element(el));
    }

    // --- Position 30: shd (run shading) ---
    if let Some(ref shading) = style_props.run_shading {
        let mut shd = w_el("shd");
        if let Some(ref fill) = shading.fill {
            attr_set(&mut shd, "w:fill", fill.clone());
        }
        if let Some(ref val) = shading.val {
            attr_set(&mut shd, "w:val", val.to_xml_str());
        }
        if let Some(ref color) = shading.color {
            attr_set(&mut shd, "w:color", color.clone());
        }
        rpr.children.push(XMLNode::Element(shd));
    }

    // --- Position 31: fitText ---
    if let Some(ref ft) = style_props.fit_text {
        let mut el = w_el("fitText");
        attr_set(&mut el, "w:val", ft.width.to_string());
        if let Some(id) = ft.id {
            attr_set(&mut el, "w:id", id.to_string());
        }
        rpr.children.push(XMLNode::Element(el));
    }

    // --- Position 32: vertAlign (subscript/superscript) ---
    for mark in marks {
        match mark {
            Mark::Subscript => {
                let mut va = w_el("vertAlign");
                attr_set(&mut va, "w:val", "subscript");
                rpr.children.push(XMLNode::Element(va));
            }
            Mark::Superscript => {
                let mut va = w_el("vertAlign");
                attr_set(&mut va, "w:val", "superscript");
                rpr.children.push(XMLNode::Element(va));
            }
            _ => {}
        }
    }

    // --- Position 32: rtl ---
    emit_mark_value(&mut rpr, "rtl", &style_props.rtl);

    // --- Position 34: cs ---
    emit_mark_value(&mut rpr, "cs", &style_props.cs);

    // --- Position 35: em ---
    if let Some(ref em) = style_props.emphasis_mark {
        let mut el = w_el("em");
        attr_set(&mut el, "w:val", em.to_xml_str());
        rpr.children.push(XMLNode::Element(el));
    }

    // --- Position 36: lang ---
    if style_props.lang.is_some() || style_props.lang_east_asia.is_some() {
        let mut el = w_el("lang");
        if let Some(ref l) = style_props.lang {
            attr_set(&mut el, "w:val", l.clone());
        }
        if let Some(ref l) = style_props.lang_east_asia {
            attr_set(&mut el, "w:eastAsia", l.clone());
        }
        rpr.children.push(XMLNode::Element(el));
    }

    // --- Position 38: specVanish ---
    emit_mark_value(&mut rpr, "specVanish", &style_props.spec_vanish);

    // --- Position 39: oMath ---
    emit_mark_value(&mut rpr, "oMath", &style_props.o_math);

    // --- Preserved remainder: unmodeled rPr children ---
    //
    // Content this parser doesn't model (e.g. w:eastAsianLayout, or a
    // foreign-namespace extension like w14:glow) was captured verbatim at
    // import (`word_ir::parse_rpr_element`) rather than dropped. Re-insert it
    // here at its Annex-A position (`docx_validate_ordering::RPR_ORDER`) so an
    // untouched run round-trips byte-faithful for content this engine has no
    // typed model for — the `Atom::Widget` precedent, applied to rPr
    // children. A name outside the ordering table (unrecognized even to
    // Annex A) is appended at the end of rPr.
    insert_preserved_children(
        &mut rpr,
        &style_props.preserved,
        crate::docx_validate_ordering::RPR_ORDER,
    );

    // Note: rPrChange (position 40) is added separately by the caller.
    rpr
}

// =============================================================================
// Table serialization
// =============================================================================

/// Serialize a single border edge as a Word XML element.
fn serialize_border_edge(edge_name: &str, border: &Border) -> Element {
    let mut el = w_el(edge_name);
    attr_set(&mut el, "w:val", border.style.to_xml_str());
    if let Some(ref color) = border.color {
        attr_set(&mut el, "w:color", color.clone());
    }
    if let Some(size) = border.size {
        attr_set(&mut el, "w:sz", size.to_string());
    }
    if let Some(space) = border.space {
        attr_set(&mut el, "w:space", space.to_string());
    }
    // Re-emit preserved theme-color / frame / shadow attributes (RFC-0003).
    for (qname, value) in &border.extra_attrs {
        attr_set(&mut el, qname, value);
    }
    el
}

/// Serialize a border set (e.g., w:tblBorders or w:tcBorders) into the given container element name.
/// Serialize a `w:cnfStyle` element (§17.3.1.8 / §17.4.7) from a CnfStyle.
///
/// Shared by paragraph (pPr), row (trPr), and cell (tcPr) conditional
/// formatting — all three emit the identical element shape.
fn serialize_cnf_style(cnf: &CnfStyle) -> Element {
    let mut cnf_el = w_el("cnfStyle");
    if let Some(ref val) = cnf.val {
        attr_set(&mut cnf_el, "w:val", val.clone());
    }
    let set_bool = |el: &mut Element, name: &str, val: bool| {
        if val {
            attr_set(el, &format!("w:{name}"), "1");
        }
    };
    set_bool(&mut cnf_el, "firstRow", cnf.first_row);
    set_bool(&mut cnf_el, "lastRow", cnf.last_row);
    set_bool(&mut cnf_el, "firstColumn", cnf.first_column);
    set_bool(&mut cnf_el, "lastColumn", cnf.last_column);
    set_bool(&mut cnf_el, "oddVBand", cnf.odd_v_band);
    set_bool(&mut cnf_el, "evenVBand", cnf.even_v_band);
    set_bool(&mut cnf_el, "oddHBand", cnf.odd_h_band);
    set_bool(&mut cnf_el, "evenHBand", cnf.even_h_band);
    set_bool(
        &mut cnf_el,
        "firstRowFirstColumn",
        cnf.first_row_first_column,
    );
    set_bool(&mut cnf_el, "firstRowLastColumn", cnf.first_row_last_column);
    set_bool(&mut cnf_el, "lastRowFirstColumn", cnf.last_row_first_column);
    set_bool(&mut cnf_el, "lastRowLastColumn", cnf.last_row_last_column);
    cnf_el
}

/// Serialize a `w:tblPrEx` element (§17.4.61, CT_TblPrEx) from a TableFormatting.
///
/// Emits only the CT_TblPrEx-valid children in schema order:
/// tblW, jc, tblCellSpacing, tblInd, tblBorders, shd, tblLayout, tblCellMar,
/// tblLook. The same per-property serializers used for w:tblPr are reused.
fn serialize_tbl_pr_ex(fmt: &TableFormatting) -> Element {
    let mut el = w_el("tblPrEx");
    // tblW
    if let Some(ref width) = fmt.width {
        el.children
            .push(XMLNode::Element(serialize_table_measurement("tblW", width)));
    }
    // jc
    if let Some(ref alignment) = fmt.alignment {
        let mut jc = w_el("jc");
        let val = match alignment {
            Alignment::Left => "left",
            Alignment::Center => "center",
            Alignment::Right => "right",
            Alignment::Justify => "left",
            Alignment::Distribute => "distribute",
            Alignment::HighKashida => "highKashida",
            Alignment::LowKashida => "lowKashida",
            Alignment::MediumKashida => "mediumKashida",
            Alignment::NumTab => "numTab",
            Alignment::ThaiDistribute => "thaiDistribute",
        };
        attr_set(&mut jc, "w:val", val);
        el.children.push(XMLNode::Element(jc));
    }
    // tblCellSpacing
    if let Some(cell_spacing) = fmt.cell_spacing {
        let mut tbl_cs = w_el("tblCellSpacing");
        attr_set(&mut tbl_cs, "w:w", cell_spacing.to_string());
        attr_set(&mut tbl_cs, "w:type", "dxa");
        el.children.push(XMLNode::Element(tbl_cs));
    }
    // tblInd
    if let Some(indent) = fmt.indent {
        let mut tbl_ind = w_el("tblInd");
        attr_set(&mut tbl_ind, "w:w", indent.to_string());
        attr_set(&mut tbl_ind, "w:type", "dxa");
        el.children.push(XMLNode::Element(tbl_ind));
    }
    // tblBorders (carries insideH/insideV)
    if let Some(ref borders) = fmt.borders {
        el.children.push(XMLNode::Element(serialize_border_set(
            "tblBorders",
            borders,
        )));
    }
    // tblLayout
    if let Some(ref layout) = fmt.layout {
        let mut tbl_layout = w_el("tblLayout");
        attr_set(&mut tbl_layout, "w:type", layout.to_xml_str());
        el.children.push(XMLNode::Element(tbl_layout));
    }
    // tblCellMar
    if let Some(ref margins) = fmt.default_cell_margins {
        el.children.push(XMLNode::Element(serialize_cell_margins(
            "tblCellMar",
            margins,
        )));
    }
    // tblLook
    if let Some(ref tbl_look) = fmt.tbl_look {
        let mut look_el = w_el("tblLook");
        if let Some(ref v) = tbl_look.val {
            attr_set(&mut look_el, "w:val", v.clone());
        }
        attr_set(
            &mut look_el,
            "w:firstRow",
            if tbl_look.first_row { "1" } else { "0" },
        );
        attr_set(
            &mut look_el,
            "w:lastRow",
            if tbl_look.last_row { "1" } else { "0" },
        );
        attr_set(
            &mut look_el,
            "w:firstColumn",
            if tbl_look.first_column { "1" } else { "0" },
        );
        attr_set(
            &mut look_el,
            "w:lastColumn",
            if tbl_look.last_column { "1" } else { "0" },
        );
        attr_set(
            &mut look_el,
            "w:noHBand",
            if tbl_look.no_h_band { "1" } else { "0" },
        );
        attr_set(
            &mut look_el,
            "w:noVBand",
            if tbl_look.no_v_band { "1" } else { "0" },
        );
        el.children.push(XMLNode::Element(look_el));
    }
    el
}

fn serialize_border_set(container_name: &str, borders: &BorderSet) -> Element {
    let mut el = w_el(container_name);
    if let Some(ref b) = borders.top {
        el.children
            .push(XMLNode::Element(serialize_border_edge("top", b)));
    }
    if let Some(ref b) = borders.left {
        el.children
            .push(XMLNode::Element(serialize_border_edge("left", b)));
    }
    if let Some(ref b) = borders.bottom {
        el.children
            .push(XMLNode::Element(serialize_border_edge("bottom", b)));
    }
    if let Some(ref b) = borders.right {
        el.children
            .push(XMLNode::Element(serialize_border_edge("right", b)));
    }
    if let Some(ref b) = borders.inside_h {
        el.children
            .push(XMLNode::Element(serialize_border_edge("insideH", b)));
    }
    if let Some(ref b) = borders.inside_v {
        el.children
            .push(XMLNode::Element(serialize_border_edge("insideV", b)));
    }
    el
}

/// Serialize a shading element (w:shd).
fn serialize_shading(shading: &Shading) -> Element {
    let mut el = w_el("shd");
    if let Some(ref val) = shading.val {
        attr_set(&mut el, "w:val", val.to_xml_str());
    }
    if let Some(ref color) = shading.color {
        attr_set(&mut el, "w:color", color.clone());
    }
    if let Some(ref fill) = shading.fill {
        attr_set(&mut el, "w:fill", fill.clone());
    }
    // Re-emit preserved theme fills/colors verbatim (RFC-0003).
    for (qname, value) in &shading.extra_attrs {
        attr_set(&mut el, qname, value);
    }
    el
}

/// Serialize a table measurement element (e.g., w:tblW, w:tcW).
///
/// A width imported from an ST_Percentage literal ("33.3%", ECMA-376
/// §17.18.107) re-emits that exact spelling: tables are rebuilt from the
/// typed model on save, so without this the source form would churn to the
/// fiftieths-of-a-percent integer spelling.
fn serialize_table_measurement(element_name: &str, measurement: &TableMeasurement) -> Element {
    let mut el = w_el(element_name);
    match &measurement.pct_literal {
        // Invariant on TableMeasurement: pct_literal implies width_type Pct.
        Some(literal) => attr_set(&mut el, "w:w", literal.clone()),
        None => attr_set(&mut el, "w:w", measurement.w.to_string()),
    }
    attr_set(&mut el, "w:type", measurement.width_type.to_xml_str());
    el
}

/// Serialize cell margins into a container element (e.g., w:tblCellMar or w:tcMar).
fn serialize_cell_margins(container_name: &str, margins: &CellMargins) -> Element {
    let mut el = w_el(container_name);
    if let Some(top) = margins.top {
        let mut m = w_el("top");
        attr_set(&mut m, "w:w", top.to_string());
        attr_set(&mut m, "w:type", "dxa");
        el.children.push(XMLNode::Element(m));
    }
    if let Some(left) = margins.left {
        let mut m = w_el("left");
        attr_set(&mut m, "w:w", left.to_string());
        attr_set(&mut m, "w:type", "dxa");
        el.children.push(XMLNode::Element(m));
    }
    if let Some(bottom) = margins.bottom {
        let mut m = w_el("bottom");
        attr_set(&mut m, "w:w", bottom.to_string());
        attr_set(&mut m, "w:type", "dxa");
        el.children.push(XMLNode::Element(m));
    }
    if let Some(right) = margins.right {
        let mut m = w_el("right");
        attr_set(&mut m, "w:w", right.to_string());
        attr_set(&mut m, "w:type", "dxa");
        el.children.push(XMLNode::Element(m));
    }
    el
}

fn serialize_table_node(
    table: &TableNode,
    table_status: &TrackingStatus,
    next_id: &mut u32,
    bookmark_policy: &BookmarkIdPolicy,
    origin: &str,
) -> Result<Element, RuntimeError> {
    // ── Exhaustive field witnesses ──────────────────────────────────────
    // Every field MUST be listed WITHOUT `..`. Adding a field without
    // listing it here = compile error.
    let TableNode {
        id: _,                // NodeId — internal, not serialized
        rows: _,              // → serialized as w:tr children below
        structure_hash: _,    // derived — diff fingerprint, not serialized
        formatting: _,        // → serialized as w:tblPr below
        formatting_change: _, // → serialized as w:tblPrChange inside tblPr
    } = table;
    // Per-row witness checked inside the row loop.
    // Per-cell witness checked inside the cell loop.

    let mut tbl = w_el("tbl");

    // Serialize w:tblPr (table properties)
    let fmt = &table.formatting;
    let has_tbl_pr = fmt.style_id.is_some()
        || fmt.borders.is_some()
        || fmt.width.is_some()
        || fmt.default_cell_margins.is_some()
        || fmt.alignment.is_some()
        || fmt.indent.is_some()
        || fmt.tbl_look.is_some()
        || fmt.layout.is_some()
        || fmt.cell_spacing.is_some()
        || fmt.positioning.is_some()
        || fmt.overlap.is_some()
        || fmt.row_band_size.is_some()
        || fmt.col_band_size.is_some()
        || fmt.shading.is_some()
        || fmt.bidi_visual
        || fmt.caption.is_some()
        || fmt.description.is_some()
        || !fmt.preserved.is_empty()
        || table.formatting_change.is_some();
    if has_tbl_pr {
        let mut tbl_pr = w_el("tblPr");
        // CT_TblPr sequence (ECMA-376 Annex A, §17.4.60).
        // Children MUST appear in this order:
        // tblStyle(0) → tblpPr(1) → tblOverlap(2) → bidiVisual(3) →
        // tblStyleRowBandSize(4) → tblStyleColBandSize(5) → tblW(6) → jc(7) →
        // tblCellSpacing(8) → tblInd(9) → tblBorders(10) → shd(11) →
        // tblLayout(12) → tblCellMar(13) → tblLook(14) → tblCaption(15) →
        // tblDescription(16) → tblPrChange(17)

        // --- Position 0: tblStyle ---
        if let Some(ref style_id) = fmt.style_id {
            let mut tbl_style = w_el("tblStyle");
            attr_set(&mut tbl_style, "w:val", style_id.clone());
            tbl_pr.children.push(XMLNode::Element(tbl_style));
        }
        // --- Position 1: tblpPr ---
        if let Some(ref pos) = fmt.positioning {
            // §17.4.58 CT_TblPPr. Emit every modeled attribute that is present,
            // then re-emit the captured extra_attrs remainder verbatim so no
            // authored attribute is silently dropped on rebuild.
            let mut tblp_pr = w_el("tblpPr");
            if let Some(lft) = pos.left_from_text {
                attr_set(&mut tblp_pr, "w:leftFromText", lft.to_string());
            }
            if let Some(rgt) = pos.right_from_text {
                attr_set(&mut tblp_pr, "w:rightFromText", rgt.to_string());
            }
            if let Some(top) = pos.top_from_text {
                attr_set(&mut tblp_pr, "w:topFromText", top.to_string());
            }
            if let Some(bot) = pos.bottom_from_text {
                attr_set(&mut tblp_pr, "w:bottomFromText", bot.to_string());
            }
            if let Some(ref v) = pos.vert_anchor {
                attr_set(&mut tblp_pr, "w:vertAnchor", v.to_xml_str());
            }
            if let Some(ref v) = pos.horz_anchor {
                attr_set(&mut tblp_pr, "w:horzAnchor", v.to_xml_str());
            }
            if let Some(x) = pos.tblp_x {
                attr_set(&mut tblp_pr, "w:tblpX", x.to_string());
            }
            if let Some(ref xs) = pos.tblp_x_spec {
                attr_set(&mut tblp_pr, "w:tblpXSpec", xs.to_xml_str());
            }
            if let Some(y) = pos.tblp_y {
                attr_set(&mut tblp_pr, "w:tblpY", y.to_string());
            }
            if let Some(ref ys) = pos.tblp_y_spec {
                attr_set(&mut tblp_pr, "w:tblpYSpec", ys.to_xml_str());
            }
            for (qname, value) in &pos.extra_attrs {
                attr_set(&mut tblp_pr, qname, value);
            }
            tbl_pr.children.push(XMLNode::Element(tblp_pr));
        }
        // --- Position 2: tblOverlap ---
        if let Some(ref overlap) = fmt.overlap {
            let mut tbl_overlap = w_el("tblOverlap");
            attr_set(&mut tbl_overlap, "w:val", overlap.to_xml_str());
            tbl_pr.children.push(XMLNode::Element(tbl_overlap));
        }
        // --- Position 3: bidiVisual (§17.4.1) — RTL visual column order ---
        if fmt.bidi_visual {
            tbl_pr.children.push(XMLNode::Element(w_el("bidiVisual")));
        }
        // --- Position 4: tblStyleRowBandSize ---
        if let Some(rbs) = fmt.row_band_size {
            let mut el = w_el("tblStyleRowBandSize");
            attr_set(&mut el, "w:val", rbs.to_string());
            tbl_pr.children.push(XMLNode::Element(el));
        }
        // --- Position 5: tblStyleColBandSize ---
        if let Some(cbs) = fmt.col_band_size {
            let mut el = w_el("tblStyleColBandSize");
            attr_set(&mut el, "w:val", cbs.to_string());
            tbl_pr.children.push(XMLNode::Element(el));
        }
        // --- Position 6: tblW ---
        if let Some(ref width) = fmt.width {
            tbl_pr
                .children
                .push(XMLNode::Element(serialize_table_measurement("tblW", width)));
        }
        // --- Position 7: jc ---
        // Style-resolved slots emit as direct tblPr only when the table's own
        // tblPr authored them (parse-time provenance; see TableFormatting).
        if fmt.has_direct_alignment
            && let Some(ref alignment) = fmt.alignment
        {
            let mut jc = w_el("jc");
            let val = match alignment {
                Alignment::Left => "left",
                Alignment::Center => "center",
                Alignment::Right => "right",
                Alignment::Justify => "left",
                Alignment::Distribute => "distribute",
                Alignment::HighKashida => "highKashida",
                Alignment::LowKashida => "lowKashida",
                Alignment::MediumKashida => "mediumKashida",
                Alignment::NumTab => "numTab",
                Alignment::ThaiDistribute => "thaiDistribute",
            };
            attr_set(&mut jc, "w:val", val);
            tbl_pr.children.push(XMLNode::Element(jc));
        }
        // --- Position 8: tblCellSpacing ---
        if let Some(cell_spacing) = fmt.cell_spacing {
            let mut tbl_cs = w_el("tblCellSpacing");
            attr_set(&mut tbl_cs, "w:w", cell_spacing.to_string());
            attr_set(&mut tbl_cs, "w:type", "dxa");
            tbl_pr.children.push(XMLNode::Element(tbl_cs));
        }
        // --- Position 9: tblInd ---
        if fmt.has_direct_indent
            && let Some(indent) = fmt.indent
        {
            let mut tbl_ind = w_el("tblInd");
            attr_set(&mut tbl_ind, "w:w", indent.to_string());
            attr_set(&mut tbl_ind, "w:type", "dxa");
            tbl_pr.children.push(XMLNode::Element(tbl_ind));
        }
        // --- Position 10: tblBorders ---
        if fmt.has_direct_borders
            && let Some(ref borders) = fmt.borders
        {
            tbl_pr.children.push(XMLNode::Element(serialize_border_set(
                "tblBorders",
                borders,
            )));
        }
        // --- Position 11: shd (§17.4.32) — table-level shading ---
        if let Some(ref shading) = fmt.shading {
            tbl_pr
                .children
                .push(XMLNode::Element(serialize_shading(shading)));
        }
        // --- Position 12: tblLayout ---
        if let Some(ref layout) = fmt.layout {
            let mut tbl_layout = w_el("tblLayout");
            attr_set(&mut tbl_layout, "w:type", layout.to_xml_str());
            tbl_pr.children.push(XMLNode::Element(tbl_layout));
        }
        // --- Position 13: tblCellMar ---
        if fmt.has_direct_cell_margins
            && let Some(ref margins) = fmt.default_cell_margins
        {
            tbl_pr
                .children
                .push(XMLNode::Element(serialize_cell_margins(
                    "tblCellMar",
                    margins,
                )));
        }
        // --- Position 14: tblLook ---
        if fmt.has_direct_tbl_look
            && let Some(ref tbl_look) = fmt.tbl_look
        {
            let mut look_el = w_el("tblLook");
            // Emit the raw w:val hex for roundtrip fidelity if available.
            if let Some(ref v) = tbl_look.val {
                attr_set(&mut look_el, "w:val", v.clone());
            }
            // Always emit individual boolean attributes for clarity.
            attr_set(
                &mut look_el,
                "w:firstRow",
                if tbl_look.first_row { "1" } else { "0" },
            );
            attr_set(
                &mut look_el,
                "w:lastRow",
                if tbl_look.last_row { "1" } else { "0" },
            );
            attr_set(
                &mut look_el,
                "w:firstColumn",
                if tbl_look.first_column { "1" } else { "0" },
            );
            attr_set(
                &mut look_el,
                "w:lastColumn",
                if tbl_look.last_column { "1" } else { "0" },
            );
            attr_set(
                &mut look_el,
                "w:noHBand",
                if tbl_look.no_h_band { "1" } else { "0" },
            );
            attr_set(
                &mut look_el,
                "w:noVBand",
                if tbl_look.no_v_band { "1" } else { "0" },
            );
            tbl_pr.children.push(XMLNode::Element(look_el));
        }
        // --- Position 15: tblCaption (§17.4.42) — a11y caption ---
        if let Some(ref caption) = fmt.caption {
            let mut el = w_el("tblCaption");
            attr_set(&mut el, "w:val", caption.clone());
            tbl_pr.children.push(XMLNode::Element(el));
        }
        // --- Position 16: tblDescription (§17.4.46) — a11y description ---
        if let Some(ref description) = fmt.description {
            let mut el = w_el("tblDescription");
            attr_set(&mut el, "w:val", description.clone());
            tbl_pr.children.push(XMLNode::Element(el));
        }
        // tblPrChange — tracked table formatting change (§17.13.5.34).
        if let Some(ref fc) = table.formatting_change {
            let mut tbl_pr_change = w_el("tblPrChange");
            attr_set(
                &mut tbl_pr_change,
                "w:id",
                if fc.revision_id != 0 {
                    fc.revision_id.to_string()
                } else {
                    next_annotation_id(next_id).to_string()
                },
            );
            attr_set(&mut tbl_pr_change, "w:author", fc.author.clone());
            if let Some(ref date) = fc.date {
                attr_set(&mut tbl_pr_change, "w:date", date.clone());
            }
            let mut prev_tbl_pr = w_el("tblPr");
            if let Some(ref width) = fc.previous_width {
                prev_tbl_pr
                    .children
                    .push(XMLNode::Element(serialize_table_measurement("tblW", width)));
            }
            if let Some(ref borders) = fc.previous_borders {
                prev_tbl_pr
                    .children
                    .push(XMLNode::Element(serialize_border_set(
                        "tblBorders",
                        borders,
                    )));
            }
            if let Some(ref margins) = fc.previous_default_cell_margins {
                prev_tbl_pr
                    .children
                    .push(XMLNode::Element(serialize_cell_margins(
                        "tblCellMar",
                        margins,
                    )));
            }
            tbl_pr_change.children.push(XMLNode::Element(prev_tbl_pr));
            tbl_pr.children.push(XMLNode::Element(tbl_pr_change));
        }
        // Preserved remainder: any tblPr child the typed fields above don't model
        // (vendor extensions o:*/tm:*, future OOXML additions). Placed at their
        // schema position by `TBLPR_ORDER`; unknown/foreign names append at the
        // end. RFC-0003 "never silently drop".
        if !fmt.preserved.is_empty() {
            insert_preserved_children(
                &mut tbl_pr,
                &fmt.preserved,
                crate::docx_validate_ordering::TBLPR_ORDER,
            );
        }
        tbl.children.push(XMLNode::Element(tbl_pr));
    }

    // Serialize w:tblGrid (column widths)
    if !fmt.grid_cols.is_empty() {
        let mut tbl_grid = w_el("tblGrid");
        for &col_w in &fmt.grid_cols {
            let mut grid_col = w_el("gridCol");
            attr_set(&mut grid_col, "w:w", col_w.to_string());
            tbl_grid.children.push(XMLNode::Element(grid_col));
        }
        tbl.children.push(XMLNode::Element(tbl_grid));
    }

    for row in &table.rows {
        // Invariant backstop (§17.4.72 `CT_Row` requires `tc+`): a `<w:tr>` must
        // carry at least one `<w:tc>`. A cell-less row is never a legal wire
        // shape — the engine's own importer refuses it — so emitting one would
        // write bytes we cannot read back. Reaching here means an upstream
        // resolution/merge stripped a row's last cell (the class of bug a
        // per-cell `cellDel`/`cellIns` resolved without its row-level marker
        // produced); fail loud rather than serialize invalid OOXML. This is a
        // backstop, not the fix — whole-row insert/delete no longer mint
        // per-cell markers (see `mark_whole_row_deleted` / `_inserted`).
        if row.cells.is_empty() {
            return Err(RuntimeError {
                code: ErrorCode::InternalError,
                message:
                    "serializer refused a table row with zero cells (OOXML §17.4.72 CT_Row requires tc+)"
                        .to_string(),
                details: ErrorDetails {
                    block_id: Some(table.id.clone()),
                    context: Some(format!("table {} produced a cell-less <w:tr>", table.id.0)),
                    ..ErrorDetails::default()
                },
            });
        }
        let mut tr = w_el("tr");
        // Exhaustive row witness — compile error if TableRowNode gains a field.
        let TableRowNode {
            id: _,                // NodeId — internal, not serialized
            cells: _,             // → serialized as w:tc children below
            grid_before: _,       // → trPr (gridBefore)
            grid_after: _,        // → trPr (gridAfter)
            tracking_status: _,   // → trPr (ins/del)
            is_header: _,         // → trPr (tblHeader)
            height: _,            // → trPr (trHeight)
            height_rule: _,       // → trPr (trHeight hRule)
            formatting_change: _, // → trPr (trPrChange)
            para_id: _,           // → here (w14:paraId attribute)
            text_id: _,           // → here (w14:textId attribute)
            cant_split: _,        // → trPr (cantSplit)
            jc: _,                // → trPr (jc)
            w_before: _,          // → trPr (wBefore)
            w_after: _,           // → trPr (wAfter)
            cnf_style: _,         // → trPr (cnfStyle)
            tbl_pr_ex: _,         // → tblPrEx (row's first child, before trPr)
            cell_spacing: _,      // → trPr (tblCellSpacing)
            preserved: _,         // → trPr (preserved remainder: divId/hidden/vendor)
        } = row;
        // MS-DOCX §2.2.4: w14:paraId and w14:textId apply to w:tr elements.
        if let Some(ref id) = row.para_id {
            attr_set(&mut tr, "w14:paraId", id.clone());
        }
        if let Some(ref id) = row.text_id {
            attr_set(&mut tr, "w14:textId", id.clone());
        }
        // w:tblPrEx is the row's FIRST child (before w:trPr), §17.4.61.
        if let Some(ref ex) = row.tbl_pr_ex {
            tr.children.push(XMLNode::Element(serialize_tbl_pr_ex(ex)));
        }
        let mut tr_pr = w_el("trPr");
        let mut has_tr_pr = false;

        // CT_TrPr pos 0: cnfStyle.
        if let Some(ref cnf) = row.cnf_style {
            tr_pr
                .children
                .push(XMLNode::Element(serialize_cnf_style(cnf)));
            has_tr_pr = true;
        }

        if row.grid_before > 0 {
            let mut grid_before = w_el("gridBefore");
            attr_set(&mut grid_before, "w:val", row.grid_before.to_string());
            tr_pr.children.push(XMLNode::Element(grid_before));
            has_tr_pr = true;
        }
        if row.grid_after > 0 {
            let mut grid_after = w_el("gridAfter");
            attr_set(&mut grid_after, "w:val", row.grid_after.to_string());
            tr_pr.children.push(XMLNode::Element(grid_after));
            has_tr_pr = true;
        }
        // CT_TrPr pos 4: wBefore — preferred width of the gridBefore span.
        if let Some(ref m) = row.w_before {
            tr_pr
                .children
                .push(XMLNode::Element(serialize_table_measurement("wBefore", m)));
            has_tr_pr = true;
        }
        // CT_TrPr pos 5: wAfter — preferred width of the gridAfter span.
        if let Some(ref m) = row.w_after {
            tr_pr
                .children
                .push(XMLNode::Element(serialize_table_measurement("wAfter", m)));
            has_tr_pr = true;
        }
        // CT_TrPr pos 6: cantSplit — row may not be split across pages.
        if row.cant_split {
            tr_pr.children.push(XMLNode::Element(w_el("cantSplit")));
            has_tr_pr = true;
        }
        // CT_TrPr ordering: trHeight (pos 7) before tblHeader (pos 8)
        if let Some(height) = row.height {
            let mut tr_height = w_el("trHeight");
            attr_set(&mut tr_height, "w:val", height.to_string());
            if let Some(ref rule) = row.height_rule {
                attr_set(&mut tr_height, "w:hRule", rule.to_xml_str());
            }
            tr_pr.children.push(XMLNode::Element(tr_height));
            has_tr_pr = true;
        }
        if row.is_header {
            tr_pr.children.push(XMLNode::Element(w_el("tblHeader")));
            has_tr_pr = true;
        }
        // CT_TrPr pos 9: tblCellSpacing — row-level cell spacing (§17.4.44).
        // Type is dxa (mirrors the table-level tblCellSpacing serializer).
        if let Some(spacing) = row.cell_spacing {
            let mut tbl_cs = w_el("tblCellSpacing");
            attr_set(&mut tbl_cs, "w:w", spacing.to_string());
            attr_set(&mut tbl_cs, "w:type", "dxa");
            tr_pr.children.push(XMLNode::Element(tbl_cs));
            has_tr_pr = true;
        }
        // CT_TrPr pos 11: jc — row-level table justification (§17.4.28).
        if let Some(ref alignment) = row.jc {
            let mut jc = w_el("jc");
            let val = match alignment {
                Alignment::Left => "left",
                Alignment::Center => "center",
                Alignment::Right => "right",
                Alignment::Justify => "left",
                Alignment::Distribute => "distribute",
                Alignment::HighKashida => "highKashida",
                Alignment::LowKashida => "lowKashida",
                Alignment::MediumKashida => "mediumKashida",
                Alignment::NumTab => "numTab",
                Alignment::ThaiDistribute => "thaiDistribute",
            };
            attr_set(&mut jc, "w:val", val);
            tr_pr.children.push(XMLNode::Element(jc));
            has_tr_pr = true;
        }
        // Row-level tracking: prefer the row's own tracking_status (set by
        // row-level merge in apply_table_structure_changed), falling back to
        // the block-level table_status (set when the entire table is inserted
        // or deleted as a block).
        let effective_row_status = row.tracking_status.as_ref().unwrap_or(table_status);
        match effective_row_status {
            TrackingStatus::Inserted(rev) => {
                tr_pr.children.push(XMLNode::Element(w_ins(
                    next_annotation_id(next_id),
                    rev.author.as_deref().unwrap_or(""),
                    rev.date.as_deref().unwrap_or(""),
                )));
                has_tr_pr = true;
            }
            TrackingStatus::Deleted(rev) => {
                tr_pr.children.push(XMLNode::Element(w_del(
                    next_annotation_id(next_id),
                    rev.author.as_deref().unwrap_or(""),
                    rev.date.as_deref().unwrap_or(""),
                )));
                has_tr_pr = true;
            }
            TrackingStatus::InsertedThenDeleted(sr) => {
                tr_pr.children.push(XMLNode::Element(w_ins(
                    next_annotation_id(next_id),
                    sr.inserted.author.as_deref().unwrap_or(""),
                    sr.inserted.date.as_deref().unwrap_or(""),
                )));
                tr_pr.children.push(XMLNode::Element(w_del(
                    next_annotation_id(next_id),
                    sr.deleted.author.as_deref().unwrap_or(""),
                    sr.deleted.date.as_deref().unwrap_or(""),
                )));
                has_tr_pr = true;
            }
            TrackingStatus::Normal => {}
        }
        // trPrChange — tracked row formatting change (§17.13.5.36).
        if let Some(ref fc) = row.formatting_change {
            let mut tr_pr_change = w_el("trPrChange");
            attr_set(
                &mut tr_pr_change,
                "w:id",
                if fc.revision_id != 0 {
                    fc.revision_id.to_string()
                } else {
                    next_annotation_id(next_id).to_string()
                },
            );
            attr_set(&mut tr_pr_change, "w:author", fc.author.clone());
            if let Some(ref date) = fc.date {
                attr_set(&mut tr_pr_change, "w:date", date.clone());
            }
            let mut prev_tr_pr = w_el("trPr");
            if let Some(height) = fc.previous_height {
                let mut tr_height = w_el("trHeight");
                attr_set(&mut tr_height, "w:val", height.to_string());
                if let Some(ref rule) = fc.previous_height_rule {
                    attr_set(&mut tr_height, "w:hRule", rule.to_xml_str());
                }
                prev_tr_pr.children.push(XMLNode::Element(tr_height));
            }
            tr_pr_change.children.push(XMLNode::Element(prev_tr_pr));
            tr_pr.children.push(XMLNode::Element(tr_pr_change));
            has_tr_pr = true;
        }
        // Preserved remainder: any trPr child the typed fields don't model
        // (w:divId, w:hidden, vendor). Placed at its schema position by
        // TRPR_ORDER. RFC-0003 "never silently drop".
        if !row.preserved.is_empty() {
            insert_preserved_children(
                &mut tr_pr,
                &row.preserved,
                crate::docx_validate_ordering::TRPR_ORDER,
            );
            has_tr_pr = true;
        }
        if has_tr_pr {
            tr.children.push(XMLNode::Element(tr_pr));
        }

        for cell in &row.cells {
            let mut tc = w_el("tc");
            // Exhaustive cell witness — compile error if TableCellNode gains a field.
            let TableCellNode {
                id: _,                // NodeId — internal, not serialized
                blocks: _,            // → serialized as cell block content below
                grid_span: _,         // → tcPr (gridSpan)
                v_merge: _,           // → tcPr (vMerge)
                formatting: _,        // → tcPr (borders, shd, width, vAlign, margins)
                formatting_change: _, // → tcPr (tcPrChange)
                tracking_status: _,   // → tcPr (cellIns/cellDel)
                row_sdt_wrapper: _,   // → here (SDT re-wrapping)
                content_sdt_wraps: _, // → here (per-range content SDT wrapping)
                cnf_style: _,         // → tcPr (cnfStyle)
                hide_mark: _,         // → tcPr (hideMark)
                preserved: _,         // → tcPr (preserved remainder: hMerge/vendor)
            } = cell;
            let mut tc_pr = w_el("tcPr");
            let mut has_tc_pr = false;
            // CT_TcPrBase pos 0: cnfStyle.
            if let Some(ref cnf) = cell.cnf_style {
                tc_pr
                    .children
                    .push(XMLNode::Element(serialize_cnf_style(cnf)));
                has_tc_pr = true;
            }
            // CT_TcPrBase sequence order (ECMA-376 Annex A):
            // cnfStyle(0), tcW(1), gridSpan(2), hMerge(3), vMerge(4),
            // tcBorders(5), shd(6), noWrap(7), tcMar(8), textDirection(9),
            // tcFitText(10), vAlign(11), ...
            if let Some(ref width) = cell.formatting.width {
                tc_pr
                    .children
                    .push(XMLNode::Element(serialize_table_measurement("tcW", width)));
                has_tc_pr = true;
            }
            if cell.grid_span > 1 {
                let mut grid_span = w_el("gridSpan");
                attr_set(&mut grid_span, "w:val", cell.grid_span.to_string());
                tc_pr.children.push(XMLNode::Element(grid_span));
                has_tc_pr = true;
            }
            match cell.v_merge {
                VerticalMerge::Restart => {
                    let mut vmerge = w_el("vMerge");
                    attr_set(&mut vmerge, "w:val", "restart");
                    tc_pr.children.push(XMLNode::Element(vmerge));
                    has_tc_pr = true;
                }
                VerticalMerge::Continue => {
                    let vmerge = w_el("vMerge");
                    tc_pr.children.push(XMLNode::Element(vmerge));
                    has_tc_pr = true;
                }
                VerticalMerge::None => {}
            }
            // Conditional-banding / border-conflict resolution mutate `borders`
            // into the effective set for projections; re-emit the AUTHORED set
            // (`authored_borders`, falling back to `borders` for non-import
            // construction) so an edge the author omitted stays absent rather
            // than being synthesized as a visible line (§17.4.39).
            if cell.formatting.has_direct_borders
                && let Some(borders) = cell
                    .formatting
                    .authored_borders
                    .as_ref()
                    .or(cell.formatting.borders.as_ref())
            {
                tc_pr
                    .children
                    .push(XMLNode::Element(serialize_border_set("tcBorders", borders)));
                has_tc_pr = true;
            }
            if cell.formatting.has_direct_shading
                && let Some(ref shading) = cell.formatting.shading
            {
                tc_pr
                    .children
                    .push(XMLNode::Element(serialize_shading(shading)));
                has_tc_pr = true;
            }
            // Position 7: noWrap
            if let Some(no_wrap) = cell.formatting.no_wrap {
                let mut el = w_el("noWrap");
                if !no_wrap {
                    attr_set(&mut el, "w:val", "0");
                }
                tc_pr.children.push(XMLNode::Element(el));
                has_tc_pr = true;
            }
            if let Some(ref margins) = cell.formatting.margins {
                tc_pr
                    .children
                    .push(XMLNode::Element(serialize_cell_margins("tcMar", margins)));
                has_tc_pr = true;
            }
            // Position 9: textDirection
            if let Some(ref td) = cell.formatting.text_direction {
                let mut td_el = w_el("textDirection");
                attr_set(&mut td_el, "w:val", td.to_xml_str());
                tc_pr.children.push(XMLNode::Element(td_el));
                has_tc_pr = true;
            }
            // Position 10: tcFitText
            if let Some(tc_fit_text) = cell.formatting.tc_fit_text {
                let mut el = w_el("tcFitText");
                if !tc_fit_text {
                    attr_set(&mut el, "w:val", "0");
                }
                tc_pr.children.push(XMLNode::Element(el));
                has_tc_pr = true;
            }
            if let Some(ref v_align) = cell.formatting.v_align {
                let mut val_el = w_el("vAlign");
                let val = match v_align {
                    VerticalAlignment::Top => "top",
                    VerticalAlignment::Center => "center",
                    VerticalAlignment::Bottom => "bottom",
                };
                attr_set(&mut val_el, "w:val", val);
                tc_pr.children.push(XMLNode::Element(val_el));
                has_tc_pr = true;
            }
            // CT_TcPrBase pos 12: hideMark (after vAlign).
            if cell.hide_mark {
                tc_pr.children.push(XMLNode::Element(w_el("hideMark")));
                has_tc_pr = true;
            }

            match &cell.tracking_status {
                Some(TrackingStatus::Inserted(rev)) => {
                    tc_pr.children.push(XMLNode::Element(w_cell_ins(
                        next_annotation_id(next_id),
                        rev.author.as_deref().unwrap_or(""),
                        rev.date.as_deref().unwrap_or(""),
                    )));
                    has_tc_pr = true;
                }
                Some(TrackingStatus::Deleted(rev)) => {
                    tc_pr.children.push(XMLNode::Element(w_cell_del(
                        next_annotation_id(next_id),
                        rev.author.as_deref().unwrap_or(""),
                        rev.date.as_deref().unwrap_or(""),
                    )));
                    has_tc_pr = true;
                }
                Some(TrackingStatus::InsertedThenDeleted(sr)) => {
                    tc_pr.children.push(XMLNode::Element(w_cell_ins(
                        next_annotation_id(next_id),
                        sr.inserted.author.as_deref().unwrap_or(""),
                        sr.inserted.date.as_deref().unwrap_or(""),
                    )));
                    tc_pr.children.push(XMLNode::Element(w_cell_del(
                        next_annotation_id(next_id),
                        sr.deleted.author.as_deref().unwrap_or(""),
                        sr.deleted.date.as_deref().unwrap_or(""),
                    )));
                    has_tc_pr = true;
                }
                Some(TrackingStatus::Normal) | None => {}
            }
            // tcPrChange — tracked cell formatting change (§17.13.5.37).
            if let Some(ref fc) = cell.formatting_change {
                let mut tc_pr_change = w_el("tcPrChange");
                attr_set(
                    &mut tc_pr_change,
                    "w:id",
                    if fc.revision_id != 0 {
                        fc.revision_id.to_string()
                    } else {
                        next_annotation_id(next_id).to_string()
                    },
                );
                attr_set(&mut tc_pr_change, "w:author", fc.author.clone());
                if let Some(ref date) = fc.date {
                    attr_set(&mut tc_pr_change, "w:date", date.clone());
                }
                let mut prev_tc_pr = w_el("tcPr");
                if let Some(ref width) = fc.previous_width {
                    prev_tc_pr
                        .children
                        .push(XMLNode::Element(serialize_table_measurement("tcW", width)));
                }
                if let Some(ref borders) = fc.previous_borders {
                    prev_tc_pr
                        .children
                        .push(XMLNode::Element(serialize_border_set("tcBorders", borders)));
                }
                if let Some(ref shading) = fc.previous_shading {
                    prev_tc_pr
                        .children
                        .push(XMLNode::Element(serialize_shading(shading)));
                }
                if let Some(no_wrap) = fc.previous_no_wrap {
                    let mut el = w_el("noWrap");
                    if !no_wrap {
                        attr_set(&mut el, "w:val", "0");
                    }
                    prev_tc_pr.children.push(XMLNode::Element(el));
                }
                if let Some(ref margins) = fc.previous_margins {
                    prev_tc_pr
                        .children
                        .push(XMLNode::Element(serialize_cell_margins("tcMar", margins)));
                }
                if let Some(ref td) = fc.previous_text_direction {
                    let mut td_el = w_el("textDirection");
                    attr_set(&mut td_el, "w:val", td.to_xml_str());
                    prev_tc_pr.children.push(XMLNode::Element(td_el));
                }
                if let Some(tc_fit_text) = fc.previous_tc_fit_text {
                    let mut el = w_el("tcFitText");
                    if !tc_fit_text {
                        attr_set(&mut el, "w:val", "0");
                    }
                    prev_tc_pr.children.push(XMLNode::Element(el));
                }
                if let Some(ref v_align) = fc.previous_v_align {
                    let mut val_el = w_el("vAlign");
                    let val = match v_align {
                        VerticalAlignment::Top => "top",
                        VerticalAlignment::Center => "center",
                        VerticalAlignment::Bottom => "bottom",
                    };
                    attr_set(&mut val_el, "w:val", val);
                    prev_tc_pr.children.push(XMLNode::Element(val_el));
                }
                tc_pr_change.children.push(XMLNode::Element(prev_tc_pr));
                tc_pr.children.push(XMLNode::Element(tc_pr_change));
                has_tc_pr = true;
            }
            // Preserved remainder: any tcPr child the typed fields don't model
            // (legacy w:hMerge, vendor tm:tmTcPr). Placed at its schema position
            // by TCPR_ORDER. RFC-0003 "never silently drop".
            if !cell.preserved.is_empty() {
                insert_preserved_children(
                    &mut tc_pr,
                    &cell.preserved,
                    crate::docx_validate_ordering::TCPR_ORDER,
                );
                has_tc_pr = true;
            }
            if has_tc_pr {
                tc.children.push(XMLNode::Element(tc_pr));
            }

            // Serialize cell block content, re-wrapping each preserved
            // block-level content control around EXACTLY the blocks it enclosed
            // on import (`content_sdt_wraps`). §17.4.65: a w:tc must end with a
            // w:p. If the last block is a nested table we append a bare <w:p/>
            // at the tc level to satisfy this constraint.
            let last_block_is_table = cell
                .blocks
                .last()
                .is_some_and(|b| matches!(b, BlockNode::Table(_)));

            if cell.blocks.is_empty() {
                // A wrap needs >= 1 block, so an empty cell carries none; emit
                // the bare paragraph OOXML requires (e.g. a vMerge-continue
                // cell).
                tc.children.push(XMLNode::Element(w_el("p")));
            } else {
                let mut block_nodes = Vec::with_capacity(cell.blocks.len());
                for block in &cell.blocks {
                    block_nodes.push(serialize_untracked_block(
                        block,
                        next_id,
                        bookmark_policy,
                        origin,
                        None,
                    )?);
                }
                let mut content = wrap_cell_blocks_in_content_sdts(
                    block_nodes,
                    &cell.content_sdt_wraps,
                    &cell.id,
                )?;
                if last_block_is_table {
                    content.push(XMLNode::Element(w_el("p")));
                }
                tc.children.extend(content);
            }

            // If the cell was wrapped in a row-level SDT, re-wrap the tc element.
            if let Some(ref wrapper) = cell.row_sdt_wrapper {
                let sdt = build_sdt_wrapper(wrapper, vec![XMLNode::Element(tc)])?;
                tr.children.push(XMLNode::Element(sdt));
            } else {
                tr.children.push(XMLNode::Element(tc));
            }
        }
        tbl.children.push(XMLNode::Element(tr));
    }
    Ok(tbl)
}

/// Re-wrap a table cell's already-serialized blocks in their preserved
/// block-level content controls (`content_sdt_wraps`), returning the cell's
/// content nodes in document order: each wrap range becomes one
/// `<w:sdt>…<w:sdtContent>{its span blocks}</w:sdtContent></w:sdt>`, and every
/// block outside all ranges stays a bare sibling of the `w:sdt`.
///
/// This is also the export-time ratchet for the swallowed-sibling class:
/// before emitting, it fails loud if the wrap ranges are not a valid partition
/// of a prefix-disjoint set of the cell's blocks — out of
/// order, overlapping, zero-span, or running past the block list. That makes it
/// structurally impossible to re-nest a following sibling inside a control
/// again: a swallowed sibling would require a `span` larger than the SDT's
/// imported block count, and the count is fixed in the model at import. We check
/// the model invariant here (cheap, local, exact) rather than re-diff the
/// serialized archive against the input — the invariant "an SDT that wrapped N
/// blocks re-emits exactly N blocks" is enforced at the point of emission.
fn wrap_cell_blocks_in_content_sdts(
    blocks: Vec<Element>,
    wraps: &[CellSdtWrap],
    cell_id: &NodeId,
) -> Result<Vec<XMLNode>, RuntimeError> {
    if wraps.is_empty() {
        return Ok(blocks.into_iter().map(XMLNode::Element).collect());
    }
    let n = blocks.len();
    // Validate the ranges: document order, in-bounds, span >= 1, no overlap.
    let mut prev_end = 0usize;
    for w in wraps {
        if w.span == 0 {
            return Err(cell_sdt_wrap_error(
                cell_id,
                "a cell content control wraps zero blocks",
                w,
                n,
            ));
        }
        if w.start < prev_end {
            return Err(cell_sdt_wrap_error(
                cell_id,
                "cell content controls overlap or are out of document order",
                w,
                n,
            ));
        }
        let end = match w.start.checked_add(w.span) {
            Some(end) if end <= n => end,
            _ => {
                return Err(cell_sdt_wrap_error(
                    cell_id,
                    "a cell content control runs past the cell's blocks",
                    w,
                    n,
                ));
            }
        };
        prev_end = end;
    }
    // Interleave: unwrapped blocks pass through; each range becomes one w:sdt
    // holding exactly its `span` blocks. Draining in order is sound because the
    // ranges are ordered and cover increasing, non-overlapping indices.
    let mut drain = blocks.into_iter();
    let mut wrap_iter = wraps.iter().peekable();
    let mut out: Vec<XMLNode> = Vec::new();
    let mut idx = 0usize;
    while idx < n {
        if let Some(w) = wrap_iter.peek().copied()
            && w.start == idx
        {
            let mut content = Vec::with_capacity(w.span);
            for _ in 0..w.span {
                content.push(XMLNode::Element(
                    drain.next().expect("wrap range validated in-bounds"),
                ));
            }
            out.push(XMLNode::Element(build_sdt_wrapper(&w.wrapper, content)?));
            idx += w.span;
            wrap_iter.next();
        } else {
            out.push(XMLNode::Element(drain.next().expect("idx < blocks.len()")));
            idx += 1;
        }
    }
    Ok(out)
}

/// Fail-loud error for an invalid `content_sdt_wraps` range — an engine bug
/// (import/edit must produce ordered, in-bounds, non-overlapping ranges).
fn cell_sdt_wrap_error(cell_id: &NodeId, why: &str, w: &CellSdtWrap, n: usize) -> RuntimeError {
    RuntimeError {
        code: ErrorCode::ValidationFailed,
        message: format!(
            "invalid cell content-control span in {cell_id}: {why} \
             (wrap start={} span={}, cell has {n} block(s)) — engine bug; \
             refusing to emit an SDT that would swallow a sibling block \
             (ISO 29500-1 §17.5.2)",
            w.start, w.span,
        ),
        details: ErrorDetails {
            block_id: Some(cell_id.clone()),
            ..ErrorDetails::default()
        },
    }
}

// =============================================================================
// Story-part serialization (footnotes, endnotes, comments)
// =============================================================================
//
// Carved into `serialize/notes.rs`. Re-exported `pub(crate)` so existing
// `crate::serialize::serialize_*_part` / `sync_note_like_part` call sites
// resolve unchanged.
mod notes;
pub(crate) use notes::{
    serialize_comments_extended_part, serialize_comments_ids_part, serialize_comments_part,
    serialize_endnotes_part, serialize_footnotes_part,
};

// SDT (content control) `w:sdtPr` builder — owned by the WrapInContentControl
// verb (`edit/verbs/content_controls.rs`). See `edit/AGENTS.md`.
pub(crate) mod sdt;

// `w:style` definition builder — owned by the CreateStyle/ModifyStyle verb
// (`edit/verbs/style_defs.rs`). Reuses `build_rpr` for the style's rPr and a
// focused pPr builder. See `edit/AGENTS.md`.
pub(crate) mod style_def;

// =============================================================================
// Hyperlink serialization
// =============================================================================

pub(crate) fn build_hyperlink_element(data: &HyperlinkData) -> Element {
    let mut hyperlink = w_el("hyperlink");

    // Add anchor attribute for internal links
    if let Some(anchor) = &data.anchor {
        attr_set(&mut hyperlink, "w:anchor", anchor.clone());
    }

    // Add r:id attribute for external hyperlinks
    if let Some(r_id) = &data.r_id {
        attr_set(&mut hyperlink, "r:id", r_id.clone());
    }

    // Restore any extra hyperlink attributes (w:history, w:tgtFrame, etc.)
    for (qname, value) in &data.extra_attrs {
        attr_set(&mut hyperlink, qname, value.clone());
    }

    if !data.runs.is_empty() {
        // Emit runs. Adjacent runs with the same tracking status are
        // grouped into a single `<w:ins>`/`<w:del>` envelope inside the
        // hyperlink. Normal runs are emitted as bare `<w:r>` children.
        // Per ECMA-376 §17.13.5, CT_Hyperlink accepts EG_PContent which
        // includes `w:ins`/`w:del`, so Word renders these correctly.
        let mut i = 0;
        while i < data.runs.len() {
            let status = data.runs[i].status.clone();
            let mut j = i + 1;
            while j < data.runs.len() && data.runs[j].status == status {
                j += 1;
            }
            match &status {
                TrackingStatus::Normal => {
                    for run in &data.runs[i..j] {
                        hyperlink
                            .children
                            .push(XMLNode::Element(build_hyperlink_run(run, false)));
                    }
                }
                TrackingStatus::Inserted(rev) => {
                    let author = rev.author.as_deref().unwrap_or("");
                    let date = rev.date.as_deref().unwrap_or("");
                    let mut ins = crate::word_xml::w_ins(rev.revision_id, author, date);
                    for run in &data.runs[i..j] {
                        ins.children
                            .push(XMLNode::Element(build_hyperlink_run(run, false)));
                    }
                    hyperlink.children.push(XMLNode::Element(ins));
                }
                TrackingStatus::Deleted(rev) => {
                    let author = rev.author.as_deref().unwrap_or("");
                    let date = rev.date.as_deref().unwrap_or("");
                    let mut del = crate::word_xml::w_del(rev.revision_id, author, date);
                    for run in &data.runs[i..j] {
                        del.children
                            .push(XMLNode::Element(build_hyperlink_run(run, true)));
                    }
                    hyperlink.children.push(XMLNode::Element(del));
                }
                TrackingStatus::InsertedThenDeleted(sr) => {
                    // Canonical nested emission inside the hyperlink, same
                    // shape as emit_segment's. Never constructed today (the
                    // splice does not edit hyperlink display runs), but total.
                    let mut ins = crate::word_xml::w_ins(
                        sr.inserted.revision_id,
                        sr.inserted.author.as_deref().unwrap_or(""),
                        sr.inserted.date.as_deref().unwrap_or(""),
                    );
                    let mut del = crate::word_xml::w_del(
                        sr.deleted.revision_id,
                        sr.deleted.author.as_deref().unwrap_or(""),
                        sr.deleted.date.as_deref().unwrap_or(""),
                    );
                    for run in &data.runs[i..j] {
                        del.children
                            .push(XMLNode::Element(build_hyperlink_run(run, true)));
                    }
                    ins.children.push(XMLNode::Element(del));
                    hyperlink.children.push(XMLNode::Element(ins));
                }
            }
            i = j;
        }
    } else {
        // Fallback for synthetically constructed hyperlinks with no run data:
        // emit a single bare run with the concatenated text.
        let mut run = w_el("r");
        let mut text_el = w_el("t");
        if data.text.starts_with(' ') || data.text.ends_with(' ') {
            attr_set(&mut text_el, "xml:space", "preserve");
        }
        text_el.children.push(XMLNode::Text(data.text.clone()));
        run.children.push(XMLNode::Element(text_el));
        hyperlink.children.push(XMLNode::Element(run));
    }

    hyperlink
}

/// Serialize a single `HyperlinkRun` back to a `<w:r>` element.
///
/// When `deleted` is true, the text is emitted inside `<w:delText>` instead
/// of `<w:t>` (per ECMA-376 §17.4.20). Used when the surrounding container
/// is `<w:del>`.
fn build_hyperlink_run(run: &HyperlinkRun, deleted: bool) -> Element {
    let mut r = w_el("r");

    // Restore rPr if present. These bytes were serialized by us during import
    // via serialize_element(), so a parse failure is a programmer bug.
    if let Some(rpr_bytes) = &run.rpr_xml {
        let rpr_el = crate::word_xml::parse_raw_fragment(rpr_bytes.as_slice())
            .expect("hyperlink rPr bytes we serialized during import must parse back");
        r.children.push(XMLNode::Element(rpr_el));
    }

    let text_local = if deleted { "delText" } else { "t" };
    let mut text_el = w_el(text_local);
    if run.text.starts_with(' ') || run.text.ends_with(' ') {
        attr_set(&mut text_el, "xml:space", "preserve");
    }
    text_el.children.push(XMLNode::Text(run.text.clone()));
    r.children.push(XMLNode::Element(text_el));

    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{Cursor, Read};

    use zip::ZipArchive;

    use crate::domain::NoteType;
    use crate::{DocxRuntime, ExportMode, SimpleRuntime, TransactionMeta};

    // ── bookmark id policy ───────────────────────────────────────────────
    //
    // These replace the old `remap_decoration_id` tests, which encoded the
    // wrong domain model (remap EVERY origin to fresh ids). Per ECMA-376
    // §17.13.6 the bookmark's identity is its NAME and the id is only a
    // part-local pairing key — base ids must be preserved verbatim (pairs
    // can span emission paths the remap never saw), and only target-origin
    // pairs get rewritten, both halves consistently.

    fn marker(tag: &str, id: &str, name: Option<&str>) -> Element {
        let mut el = w_el(tag);
        attr_set(&mut el, "w:id", id);
        if let Some(name) = name {
            attr_set(&mut el, "w:name", name);
        }
        el
    }

    // ── backstop: a cell-less row is refused, not serialized ─────────────
    //
    // §17.4.72 `CT_Row` requires `tc+`. A `<w:tr>` with zero `<w:tc>` is not a
    // legal wire shape (the engine's own importer refuses it), so if a
    // resolution/merge ever strips a row's last cell the serializer must fail
    // loud rather than emit unreadable bytes. This pins the backstop directly on
    // a hand-built cell-less row.
    #[test]
    fn serialize_refuses_cell_less_row() {
        use crate::domain::{TableFormatting, TableNode, TableRowNode};

        let cell_less_row = TableRowNode {
            id: NodeId::from("tbl_x_r0"),
            cells: Vec::new(),
            grid_before: 0,
            grid_after: 0,
            tracking_status: None,
            is_header: false,
            height: None,
            height_rule: None,
            formatting_change: None,
            para_id: None,
            text_id: None,
            cant_split: false,
            jc: None,
            w_before: None,
            w_after: None,
            cnf_style: None,
            tbl_pr_ex: None,
            cell_spacing: None,
            preserved: Vec::new(),
        };
        let table = TableNode {
            id: NodeId::from("tbl_x"),
            rows: vec![cell_less_row],
            structure_hash: "x".to_string(),
            formatting: TableFormatting::default(),
            formatting_change: None,
        };
        let mut next_id = 1u32;
        let err = serialize_table_node(
            &table,
            &TrackingStatus::Normal,
            &mut next_id,
            &BookmarkIdPolicy::default(),
            "test",
        )
        .expect_err("serializer must refuse a cell-less row");
        assert_eq!(err.code, ErrorCode::InternalError);
        assert!(
            err.message.contains("zero cells"),
            "unexpected message: {}",
            err.message
        );
    }

    // ── table measurements: percent-literal source form re-emits verbatim ─
    //
    // ST_MeasurementOrPercent (§17.18.107) admits percent literals ("33.3%").
    // Tables are rebuilt from the typed model on save, so the serializer —
    // not a verbatim copy — is what preserves the source spelling.
    #[test]
    fn table_measurement_percent_literal_reemits_source_form() {
        let m = crate::domain::TableMeasurement {
            w: 1665,
            width_type: crate::domain::WidthType::Pct,
            pct_literal: Some("33.3%".to_string()),
        };
        let el = serialize_table_measurement("tcW", &m);
        assert_eq!(attr_get(&el, "w:w").map(|s| s.as_str()), Some("33.3%"));
        assert_eq!(attr_get(&el, "w:type").map(|s| s.as_str()), Some("pct"));
    }

    #[test]
    fn table_measurement_numeric_widths_emit_number() {
        let m = crate::domain::TableMeasurement {
            w: 5000,
            width_type: crate::domain::WidthType::Pct,
            pct_literal: None,
        };
        let el = serialize_table_measurement("tblW", &m);
        assert_eq!(attr_get(&el, "w:w").map(|s| s.as_str()), Some("5000"));
        assert_eq!(attr_get(&el, "w:type").map(|s| s.as_str()), Some("pct"));
    }

    // ── run-wrapper predicate cannot drift from word_ir's widget list ────
    //
    // Every element word_ir classifies as a run widget is a member of
    // EG_RunInnerContent and is only legal inside `w:r`. If the serializer
    // emits such an element bare at paragraph level, Word refuses the file
    // entirely. This test pins the invariant at the whitelist level so the
    // whole drift class (not just the pgNum/contentPart instance that first
    // exposed it) can never regress silently.
    #[test]
    fn every_run_widget_requires_a_run_wrapper() {
        for name in crate::word_ir::RUN_WIDGET_NAMES {
            let element = w_el(name);
            assert!(
                opaque_raw_element_requires_run_wrapper(&element),
                "run widget `{name}` (EG_RunInnerContent) must be re-wrapped in \
                 w:r on emission, but the serializer predicate would emit it bare"
            );
        }
    }

    // ── deleted-opaque run text: w:t → w:delText, except in a textbox ─────
    //
    // When an opaque (e.g. an inline w:sdt) is wrapped in w:del, its descendant
    // run text must become w:delText (I-TC-001) — but a w:txbxContent behind a
    // deleted drawing is a separate story where w:t stays legal.
    #[test]
    fn coerce_opaque_run_text_deleted_skips_textbox() {
        let raw = br#"<w:sdt xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:sdtContent><w:r><w:instrText>FIELD</w:instrText></w:r><w:r><w:t>body</w:t></w:r><w:r><w:drawing><w:txbxContent><w:p><w:r><w:t>caption</w:t></w:r></w:p></w:txbxContent></w:drawing></w:r></w:sdtContent></w:sdt>"#;
        let mut element =
            crate::word_xml::parse_raw_fragment(raw).expect("witness sdt fragment parses");
        coerce_opaque_run_text(&mut element, /*deleted=*/ true);
        let out = String::from_utf8(crate::word_xml::serialize_raw_fragment(&element))
            .expect("serialized fragment is utf-8");

        // The sdt's own run text is converted to the deleted-text content model.
        assert!(
            out.contains("<w:delText>body</w:delText>"),
            "sdt run text must become w:delText inside a deletion: {out}"
        );
        assert!(
            out.contains("<w:delInstrText>FIELD</w:delInstrText>"),
            "sdt instruction text must become w:delInstrText inside a deletion: {out}"
        );
        // The textbox is a separate story — its run text stays w:t.
        assert!(
            out.contains("<w:t>caption</w:t>"),
            "textbox (w:txbxContent) run text must stay w:t under a deletion: {out}"
        );
        assert!(
            !out.contains("<w:delText>caption</w:delText>"),
            "textbox run text must NOT be converted to w:delText: {out}"
        );
    }

    // ── restored-opaque run text: w:delText → w:t, w:delInstrText → w:instrText ─
    //
    // The inverse of the deleted coercion. When a w:del wrapping an opaque is
    // rejected, the opaque is emitted as plain content; any w:delText /
    // w:delInstrText captured into its raw_xml from the deletion must restore to
    // w:t / w:instrText, or a bare w:delInstrText leaks into a non-deleted run
    // (schema-invalid; Word repairs the file). The textbox story is unaffected.
    #[test]
    fn coerce_opaque_run_text_restored_inverts_deleted_forms() {
        let raw = br#"<w:sdt xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:sdtContent><w:r><w:delInstrText>FIELD</w:delInstrText></w:r><w:r><w:delText>body</w:delText></w:r><w:r><w:drawing><w:txbxContent><w:p><w:r><w:t>caption</w:t></w:r></w:p></w:txbxContent></w:drawing></w:r></w:sdtContent></w:sdt>"#;
        let mut element =
            crate::word_xml::parse_raw_fragment(raw).expect("witness sdt fragment parses");
        coerce_opaque_run_text(&mut element, /*deleted=*/ false);
        let out = String::from_utf8(crate::word_xml::serialize_raw_fragment(&element))
            .expect("serialized fragment is utf-8");

        assert!(
            out.contains("<w:t>body</w:t>") && !out.contains("delText"),
            "restored opaque run text must become w:t: {out}"
        );
        assert!(
            out.contains("<w:instrText>FIELD</w:instrText>") && !out.contains("delInstrText"),
            "restored opaque instruction text must become w:instrText: {out}"
        );
        assert!(
            out.contains("<w:t>caption</w:t>"),
            "textbox run text is unaffected by the restore coercion: {out}"
        );
    }

    // A PRESERVED nested deletion inside a plain opaque must keep its deleted
    // run-content forms — the restore coercion must not descend into a nested
    // w:del and convert its w:delText/w:delInstrText, which would corrupt an
    // unresolved tracked deletion.
    #[test]
    fn coerce_opaque_run_text_restored_preserves_nested_deletion() {
        let raw = br#"<w:sdt xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:sdtContent><w:r><w:t>kept</w:t></w:r><w:del w:id="3" w:author="A"><w:r><w:delText>gone</w:delText></w:r><w:r><w:delInstrText>CODE</w:delInstrText></w:r></w:del></w:sdtContent></w:sdt>"#;
        let mut element =
            crate::word_xml::parse_raw_fragment(raw).expect("witness sdt fragment parses");
        coerce_opaque_run_text(&mut element, /*deleted=*/ false);
        let out = String::from_utf8(crate::word_xml::serialize_raw_fragment(&element))
            .expect("serialized fragment is utf-8");
        assert!(
            out.contains("<w:delText>gone</w:delText>"),
            "nested preserved deletion keeps its w:delText: {out}"
        );
        assert!(
            out.contains("<w:delInstrText>CODE</w:delInstrText>"),
            "nested preserved deletion keeps its w:delInstrText: {out}"
        );
    }

    // ── build_rpr: preserved-remainder emission ordering ─────────────────

    /// Every element `build_rpr` writes into its own bytes, in document order
    /// — used to assert the built `rPr`'s child ORDER without depending on
    /// how `xmltree::Element::name`/`prefix` happen to be split.
    fn child_xml_strings(el: &Element) -> Vec<String> {
        el.children
            .iter()
            .filter_map(|c| match c {
                XMLNode::Element(child) => {
                    let mut buf = Vec::new();
                    child.write(&mut buf).ok()?;
                    Some(String::from_utf8_lossy(&buf).into_owned())
                }
                _ => None,
            })
            .collect()
    }

    /// A preserved `w:eastAsianLayout` (Annex A position between `lang` and
    /// `specVanish`) must be re-inserted at that exact position, not appended
    /// at the end — `build_rpr`'s post-pass looks it up in `RPR_ORDER`
    /// rather than treating every preserved child as a trailing extension.
    #[test]
    fn build_rpr_places_known_preserved_child_at_annex_a_position() {
        let style_props = StyleProps {
            lang: Some("en-US".into()),
            spec_vanish: MarkValue::On,
            preserved: vec![crate::domain::PreservedProp {
                name: "w:eastAsianLayout".to_string(),
                raw_xml: r#"<w:eastAsianLayout w:combine="1"/>"#.to_string(),
            }],
            ..StyleProps::default()
        };
        let rpr = build_rpr(&[], &style_props);
        let names = child_xml_strings(&rpr);

        let lang_idx = names
            .iter()
            .position(|s| s.contains("lang"))
            .expect("w:lang emitted");
        let ea_idx = names
            .iter()
            .position(|s| s.contains("eastAsianLayout"))
            .expect("preserved w:eastAsianLayout emitted");
        let spec_idx = names
            .iter()
            .position(|s| s.contains("specVanish"))
            .expect("w:specVanish emitted");

        assert!(
            lang_idx < ea_idx,
            "w:lang must precede preserved w:eastAsianLayout (Annex A): {names:?}"
        );
        assert!(
            ea_idx < spec_idx,
            "preserved w:eastAsianLayout must precede w:specVanish (Annex A): {names:?}"
        );
    }

    /// A preserved child whose local name isn't in `RPR_ORDER` at all (a
    /// foreign-namespace extension, e.g. `w14:glow`) has no Annex A slot to
    /// target — it must land at the END of `rPr`, after every modeled child.
    #[test]
    fn build_rpr_appends_unrecognized_preserved_child_at_end() {
        let style_props = StyleProps {
            lang: Some("en-US".into()),
            preserved: vec![crate::domain::PreservedProp {
                name: "w14:glow".to_string(),
                raw_xml: r#"<w14:glow xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" w14:rad="63500"><w14:srgbClr w14:val="4F81BD"/></w14:glow>"#.to_string(),
            }],
            ..StyleProps::default()
        };
        let rpr = build_rpr(&[], &style_props);
        let names = child_xml_strings(&rpr);
        assert_eq!(
            names.last().map(|s| s.contains("glow")),
            Some(true),
            "unrecognized preserved child must be the last rPr child: {names:?}"
        );
    }

    /// A foreign-namespace preserved child captured BEFORE a table-listed one
    /// (their source-document order) must not derail the known child's
    /// Annex A placement: the insertion scan encounters the already-appended
    /// foreign element, which is outside `RPR_ORDER`, and must treat it as
    /// ordering after every table entry rather than panicking.
    #[test]
    fn build_rpr_orders_known_preserved_child_past_an_earlier_foreign_one() {
        let style_props = StyleProps {
            lang: Some("en-US".into()),
            spec_vanish: MarkValue::On,
            preserved: vec![
                crate::domain::PreservedProp {
                    name: "w14:glow".to_string(),
                    raw_xml: r#"<w14:glow xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" w14:rad="63500"/>"#.to_string(),
                },
                crate::domain::PreservedProp {
                    name: "w:eastAsianLayout".to_string(),
                    raw_xml: r#"<w:eastAsianLayout w:combine="1"/>"#.to_string(),
                },
            ],
            ..StyleProps::default()
        };
        let rpr = build_rpr(&[], &style_props);
        let names = child_xml_strings(&rpr);

        let lang_idx = names
            .iter()
            .position(|s| s.contains("lang"))
            .expect("w:lang emitted");
        let ea_idx = names
            .iter()
            .position(|s| s.contains("eastAsianLayout"))
            .expect("preserved w:eastAsianLayout emitted");
        let spec_idx = names
            .iter()
            .position(|s| s.contains("specVanish"))
            .expect("w:specVanish emitted");
        let glow_idx = names
            .iter()
            .position(|s| s.contains("glow"))
            .expect("w14:glow emitted");

        assert!(
            lang_idx < ea_idx && ea_idx < spec_idx,
            "Annex A placement held: {names:?}"
        );
        assert!(
            spec_idx < glow_idx,
            "foreign child stays after modeled children: {names:?}"
        );
    }

    // ── build_paragraph_properties: preserved-remainder emission ordering ─

    /// A minimal, all-defaults `ParagraphNode`. Individual pPr ordering tests
    /// clone this and set the handful of fields they need.
    fn minimal_paragraph_for_ppr_test() -> ParagraphNode {
        ParagraphNode {
            id: crate::domain::NodeId::from("p1"),
            style_id: None,
            align: None,
            has_direct_align: false,
            indent: None,
            has_direct_indent: false,
            authored_indent: None,
            spacing: None,
            has_direct_spacing: false,
            authored_spacing: None,
            borders: None,
            has_direct_borders: false,
            keep_next: None,
            has_direct_keep_next: false,
            keep_lines: None,
            has_direct_keep_lines: false,
            page_break_before: false,
            has_direct_page_break_before: false,
            widow_control: None,
            has_direct_widow_control: false,
            contextual_spacing: None,
            has_direct_contextual_spacing: false,
            shading: None,
            has_direct_shading: false,
            tab_stops: vec![],
            effective_tab_stops_rel: vec![],
            segments: vec![],
            block_text_hash: None,
            numbering: None,
            has_direct_numbering: true,
            numbering_suppressed: false,
            materialized_numbering: None,
            rendered_text: None,
            literal_prefix: None,
            literal_prefix_marks: Vec::new(),
            literal_prefix_style_props: StyleProps::default(),
            literal_prefix_rpr_authored: crate::domain::RunRprAuthored::default(),
            literal_prefix_leading_rpr: None,
            literal_prefix_trailing_rpr: None,
            literal_prefix_leading_tab_twips: None,
            literal_prefix_leading_tab_count: 0,
            literal_prefix_leading_ws: String::new(),
            literal_prefix_trailing_ws: String::new(),
            literal_prefix_has_trailing_tab: false,
            literal_prefix_trailing_tab_stop_twips: None,
            outline_lvl: None,
            heading_level: None,
            para_mark_status: None,
            paragraph_mark_marks: vec![],
            paragraph_mark_style_props: StyleProps::default(),
            paragraph_mark_rpr_off: Default::default(),
            para_split: false,
            section_property_change: None,
            formatting_change: None,
            section_properties: None,
            mirror_indents: None,
            auto_space_de: None,
            auto_space_dn: None,
            bidi: None,
            text_alignment: None,
            text_direction: None,
            suppress_auto_hyphens: None,
            snap_to_grid: None,
            overflow_punct: None,
            adjust_right_ind: None,
            word_wrap: None,
            frame_pr: None,
            para_id: None,
            text_id: None,
            cnf_style: None,
            preserved_ppr: Vec::new(),
        }
    }

    /// A preserved `w:suppressLineNumbers` (Annex A position between
    /// `numPr`/`widowControl` and `pBdr`) must be re-inserted at that exact
    /// position, not appended at the end — mirrors
    /// `build_rpr_places_known_preserved_child_at_annex_a_position`.
    #[test]
    fn build_paragraph_properties_places_known_preserved_child_at_annex_a_position() {
        let paragraph = ParagraphNode {
            widow_control: Some(false),
            has_direct_widow_control: true,
            has_direct_borders: true,
            borders: Some(crate::domain::ParagraphBorders {
                top: None,
                bottom: None,
                left: None,
                right: None,
                between: None,
                bar: None,
            }),
            preserved_ppr: vec![crate::domain::PreservedProp {
                name: "w:suppressLineNumbers".to_string(),
                raw_xml: "<w:suppressLineNumbers/>".to_string(),
            }],
            ..minimal_paragraph_for_ppr_test()
        };
        let mut next_id = 1;
        let ppr = build_paragraph_properties(&paragraph, &mut next_id, None)
            .expect("pPr should be emitted");
        let names = child_xml_strings(&ppr);

        let widow_idx = names
            .iter()
            .position(|s| s.contains("widowControl"))
            .expect("w:widowControl emitted");
        let sln_idx = names
            .iter()
            .position(|s| s.contains("suppressLineNumbers"))
            .expect("preserved w:suppressLineNumbers emitted");
        let pbdr_idx = names
            .iter()
            .position(|s| s.contains("pBdr"))
            .expect("w:pBdr emitted");

        assert!(
            widow_idx < sln_idx,
            "w:widowControl must precede preserved w:suppressLineNumbers (Annex A): {names:?}"
        );
        assert!(
            sln_idx < pbdr_idx,
            "preserved w:suppressLineNumbers must precede w:pBdr (Annex A): {names:?}"
        );
    }

    /// A preserved child whose local name isn't in `PPR_ORDER` at all (a
    /// foreign-namespace extension) has no Annex A slot to target — it must
    /// land at the end of pPr, BEFORE the caller-appended `w:pPrChange` (this
    /// paragraph has no formatting_change, so `w:pPrChange` isn't in play,
    /// but the preserved child is still last among what's actually emitted).
    #[test]
    fn build_paragraph_properties_appends_unrecognized_preserved_child_at_end() {
        let paragraph = ParagraphNode {
            widow_control: Some(false),
            preserved_ppr: vec![crate::domain::PreservedProp {
                name: "foo:bar".to_string(),
                raw_xml: r#"<foo:bar xmlns:foo="http://example.com/foo" val="1"/>"#.to_string(),
            }],
            ..minimal_paragraph_for_ppr_test()
        };
        let mut next_id = 1;
        let ppr = build_paragraph_properties(&paragraph, &mut next_id, None)
            .expect("pPr should be emitted");
        let names = child_xml_strings(&ppr);
        assert_eq!(
            names.last().map(|s| s.contains("bar")),
            Some(true),
            "unrecognized preserved child must be the last pPr child: {names:?}"
        );
    }

    /// A foreign-namespace preserved child captured BEFORE a table-listed one
    /// (their source-document order) must not derail the known child's
    /// Annex A placement — mirrors
    /// `build_rpr_orders_known_preserved_child_past_an_earlier_foreign_one`.
    #[test]
    fn build_paragraph_properties_orders_known_preserved_child_past_an_earlier_foreign_one() {
        let paragraph = ParagraphNode {
            widow_control: Some(false),
            has_direct_widow_control: true,
            has_direct_borders: true,
            borders: Some(crate::domain::ParagraphBorders {
                top: None,
                bottom: None,
                left: None,
                right: None,
                between: None,
                bar: None,
            }),
            preserved_ppr: vec![
                crate::domain::PreservedProp {
                    name: "foo:bar".to_string(),
                    raw_xml: r#"<foo:bar xmlns:foo="http://example.com/foo" val="1"/>"#.to_string(),
                },
                crate::domain::PreservedProp {
                    name: "w:suppressLineNumbers".to_string(),
                    raw_xml: "<w:suppressLineNumbers/>".to_string(),
                },
            ],
            ..minimal_paragraph_for_ppr_test()
        };
        let mut next_id = 1;
        let ppr = build_paragraph_properties(&paragraph, &mut next_id, None)
            .expect("pPr should be emitted");
        let names = child_xml_strings(&ppr);

        let widow_idx = names
            .iter()
            .position(|s| s.contains("widowControl"))
            .expect("w:widowControl emitted");
        let sln_idx = names
            .iter()
            .position(|s| s.contains("suppressLineNumbers"))
            .expect("preserved w:suppressLineNumbers emitted");
        let pbdr_idx = names
            .iter()
            .position(|s| s.contains("pBdr"))
            .expect("w:pBdr emitted");
        let bar_idx = names
            .iter()
            .position(|s| s.contains("bar"))
            .expect("foo:bar emitted");

        assert!(
            widow_idx < sln_idx && sln_idx < pbdr_idx,
            "Annex A placement held: {names:?}"
        );
        assert!(
            pbdr_idx < bar_idx,
            "foreign child stays after modeled children: {names:?}"
        );
    }

    /// Base-origin markers keep their ORIGINAL ids (I2/I3): a base pair can
    /// span a rebuilt paragraph and a raw-preserved body child, so any
    /// rewrite of one half would tear the pair (§17.13.6.1 id pairing).
    #[test]
    fn policy_base_origin_markers_keep_original_ids() {
        let policy = BookmarkIdPolicy::default();
        for tag in [
            "bookmarkStart",
            "bookmarkEnd",
            "moveFromRangeStart",
            "moveFromRangeEnd",
            "moveToRangeStart",
            "moveToRangeEnd",
        ] {
            let mut el = marker(tag, "42", Some("kept"));
            let emit = apply_decoration_id_policy(&mut el, &policy, "base").unwrap();
            assert_eq!(emit, DecorationEmit::Emit);
            assert_eq!(
                attr_get(&el, "w:id").unwrap(),
                "42",
                "base-origin <w:{tag}> must keep its original id"
            );
        }
    }

    /// A complete target-origin pair is remapped to ONE fresh id on both
    /// halves (I4): target ids come from a different document's id space and
    /// may collide with kept base ids; the fresh id must be applied
    /// consistently so the pair stays intact.
    #[test]
    fn policy_target_pair_remapped_consistently() {
        let mut scan = BookmarkScan::default();
        scan.record_element_tree(&marker("bookmarkStart", "5", Some("target_bm")), "target");
        scan.record_element_tree(&marker("bookmarkEnd", "5", None), "target");
        // Move ranges share the pairing mechanics (same tear risk), keyed by
        // family so a bookmark id=5 cannot cross-pair with a move range id=5.
        scan.record_element_tree(&marker("moveFromRangeStart", "5", Some("move1")), "target");
        scan.record_element_tree(&marker("moveFromRangeEnd", "5", None), "target");
        let mut next_id = 100;
        let policy = scan.into_policy(&mut next_id);

        let mut bm_start = marker("bookmarkStart", "5", Some("target_bm"));
        let mut bm_end = marker("bookmarkEnd", "5", None);
        assert_eq!(
            apply_decoration_id_policy(&mut bm_start, &policy, "target").unwrap(),
            DecorationEmit::Emit
        );
        assert_eq!(
            apply_decoration_id_policy(&mut bm_end, &policy, "target").unwrap(),
            DecorationEmit::Emit
        );
        let new_id = attr_get(&bm_start, "w:id").unwrap().clone();
        assert_ne!(new_id, "5", "target pair must get a fresh id");
        assert_eq!(
            attr_get(&bm_end, "w:id").unwrap(),
            &new_id,
            "both halves must carry the SAME fresh id"
        );

        let mut mv_start = marker("moveFromRangeStart", "5", Some("move1"));
        let mut mv_end = marker("moveFromRangeEnd", "5", None);
        apply_decoration_id_policy(&mut mv_start, &policy, "target").unwrap();
        apply_decoration_id_policy(&mut mv_end, &policy, "target").unwrap();
        let mv_id = attr_get(&mv_start, "w:id").unwrap().clone();
        assert_eq!(attr_get(&mv_end, "w:id").unwrap(), &mv_id);
        assert_ne!(
            mv_id, new_id,
            "bookmark and move-range pairs with the same old id are distinct \
             pairings (different families) and must not share a fresh id"
        );
    }

    /// A target-origin bookmark whose NAME matches a base-emitted bookmark is
    /// the SAME bookmark (I5, §17.13.6.2 name attribute: duplicate names —
    /// first maintained, subsequent ignored): both halves are dropped.
    #[test]
    fn policy_target_pair_with_base_name_dropped() {
        let mut scan = BookmarkScan::default();
        scan.record_element_tree(&marker("bookmarkStart", "1", Some("_GoBack")), "base");
        scan.record_element_tree(&marker("bookmarkStart", "9", Some("_GoBack")), "target");
        scan.record_element_tree(&marker("bookmarkEnd", "9", None), "target");
        let mut next_id = 100;
        let policy = scan.into_policy(&mut next_id);

        let mut start = marker("bookmarkStart", "9", Some("_GoBack"));
        let mut end = marker("bookmarkEnd", "9", None);
        assert_eq!(
            apply_decoration_id_policy(&mut start, &policy, "target").unwrap(),
            DecorationEmit::Skip,
            "duplicate-name target start must be dropped"
        );
        assert_eq!(
            apply_decoration_id_policy(&mut end, &policy, "target").unwrap(),
            DecorationEmit::Skip,
            "its end must be dropped too (both halves, never one)"
        );
    }

    /// A lone target-origin half (the other half never materialized in this
    /// part) is dropped — emitting half a pair can only corrupt the
    /// part-local pairing (I1). Covers both the lone-end and lone-start
    /// shapes.
    #[test]
    fn policy_lone_target_half_dropped() {
        let mut scan = BookmarkScan::default();
        scan.record_element_tree(&marker("bookmarkEnd", "7", None), "target");
        scan.record_element_tree(&marker("bookmarkStart", "8", Some("lonely")), "target");
        let mut next_id = 100;
        let policy = scan.into_policy(&mut next_id);

        let mut end = marker("bookmarkEnd", "7", None);
        assert_eq!(
            apply_decoration_id_policy(&mut end, &policy, "target").unwrap(),
            DecorationEmit::Skip,
            "target end without a start must be dropped"
        );
        let mut start = marker("bookmarkStart", "8", Some("lonely"));
        assert_eq!(
            apply_decoration_id_policy(&mut start, &policy, "target").unwrap(),
            DecorationEmit::Skip,
            "target start without an end must be dropped"
        );
    }

    /// A bookmark whose START is base-origin (live) and whose END rides
    /// a target-origin block (a `w:moveTo` clone that kept the base id, or an
    /// inserted block) is a pair STRADDLING the origin boundary. The lone target
    /// END is NOT a corrupting half — its partner is the base START — so it must
    /// be KEPT verbatim (base id preserved) to pair with it, not dropped. Before
    /// the fix it was dropped as a lone target half, orphaning the base start.
    #[test]
    fn policy_target_half_straddling_base_partner_kept() {
        // base START id 0, no base END; target END id 0 (the moveTo-clone half).
        let mut scan = BookmarkScan::default();
        scan.record_element_tree(&marker("bookmarkStart", "0", Some("_GoBack")), "base");
        scan.record_element_tree(&marker("bookmarkEnd", "0", None), "target");
        let mut next_id = 100;
        let policy = scan.into_policy(&mut next_id);

        let mut end = marker("bookmarkEnd", "0", None);
        assert_eq!(
            apply_decoration_id_policy(&mut end, &policy, "target").unwrap(),
            DecorationEmit::Emit,
            "a target end whose only partner is a base start must be kept, not dropped"
        );
        assert_eq!(
            attr_get(&end, "w:id").unwrap(),
            "0",
            "the kept straddling half keeps the base id it pairs with"
        );

        // Mirror: base END + target START straddle.
        let mut scan = BookmarkScan::default();
        scan.record_element_tree(&marker("bookmarkEnd", "0", None), "base");
        scan.record_element_tree(&marker("bookmarkStart", "0", Some("b")), "target");
        let mut next_id = 100;
        let policy = scan.into_policy(&mut next_id);
        let mut start = marker("bookmarkStart", "0", Some("b"));
        assert_eq!(
            apply_decoration_id_policy(&mut start, &policy, "target").unwrap(),
            DecorationEmit::Emit,
            "a target start whose only partner is a base end must be kept"
        );
        assert_eq!(attr_get(&start, "w:id").unwrap(), "0");
    }

    /// The keep rule must NOT over-fire: when the BASE already carries the
    /// COMPLETE pair (start AND end), a redundant target END duplicate must
    /// still be DROPPED — keeping it would emit a duplicated `bookmarkEnd`
    /// (the `t2` shape: a pair spanning an unchanged + edited paragraph carries
    /// the end in both the base and target copies).
    #[test]
    fn policy_target_duplicate_of_complete_base_pair_dropped() {
        let mut scan = BookmarkScan::default();
        scan.record_element_tree(&marker("bookmarkStart", "0", Some("b")), "base");
        scan.record_element_tree(&marker("bookmarkEnd", "0", None), "base");
        scan.record_element_tree(&marker("bookmarkEnd", "0", None), "target");
        let mut next_id = 100;
        let policy = scan.into_policy(&mut next_id);

        let mut end = marker("bookmarkEnd", "0", None);
        assert_eq!(
            apply_decoration_id_policy(&mut end, &policy, "target").unwrap(),
            DecorationEmit::Skip,
            "a target end duplicating a COMPLETE base pair must be dropped (no duplicate)"
        );
    }

    /// A target-origin marker the pre-scan never saw is an engine bug
    /// (scan/emission drift) and must fail loudly, not emit a half-pair.
    #[test]
    fn policy_unscanned_target_marker_fails_loudly() {
        let policy = BookmarkIdPolicy::default();
        let mut el = marker("bookmarkStart", "3", Some("ghost"));
        let err = apply_decoration_id_policy(&mut el, &policy, "target")
            .expect_err("unscanned target marker must error");
        assert_eq!(err.code, ErrorCode::InternalError);
        assert!(
            err.message.contains("bookmarkStart") && err.message.contains("3"),
            "error must carry the element and id for debugging: {}",
            err.message
        );
    }

    #[test]
    fn serialize_separator_decoration_note_paragraph_wraps_in_run() {
        let before = fs::read("testdata/safe-us-vs-singapore/before.docx").expect("read fixture");
        let runtime = SimpleRuntime::new();
        let import = runtime.import_docx(&before).expect("import");
        let view = runtime.view(&import.doc_handle).expect("view");
        let footnote = view
            .canonical
            .footnotes
            .iter()
            .find(|note| note.note_type == NoteType::Separator)
            .expect("separator footnote");
        let tracked = footnote.blocks.first().expect("separator block");

        let mut next_id = 1u32;
        let bookmark_policy = BookmarkIdPolicy::default();
        let paragraph = serialize_tracked_block(tracked, &mut next_id, &bookmark_policy, None)
            .expect("serialize separator paragraph");

        let run = paragraph
            .children
            .iter()
            .find_map(|child| match child {
                XMLNode::Element(el) if local_element_name(el) == "r" => Some(el),
                _ => None,
            })
            .expect("separator paragraph should contain a run");
        assert!(
            run.children.iter().any(|child| matches!(
                child,
                XMLNode::Element(el) if local_element_name(el) == "separator"
            )),
            "separator paragraph run should contain w:separator: {paragraph:?}"
        );
    }

    #[test]
    fn singapore_redline_note_separators_stay_wrapped_in_runs() {
        let before = fs::read("testdata/safe-us-vs-singapore/before.docx").expect("read before");
        let after = fs::read("testdata/safe-us-vs-singapore/after.docx").expect("read after");
        let runtime = SimpleRuntime::new();
        let import_before = runtime.import_docx(&before).expect("import before");
        let import_after = runtime.import_docx(&after).expect("import after");
        runtime
            .diff_and_redline(
                &import_before.doc_handle,
                &import_after.doc_handle,
                TransactionMeta {
                    author: "serialize_test".to_string(),
                    reason: Some("separator note serialization regression".to_string()),
                    timestamp_utc: Some("2026-03-26T00:00:00Z".to_string()),
                },
            )
            .expect("diff_and_redline");
        let redline = runtime
            .export_docx(&import_before.doc_handle, ExportMode::Redline)
            .expect("export redline");
        let mut zip = ZipArchive::new(Cursor::new(redline)).expect("open redline zip");

        for part_name in ["word/footnotes.xml", "word/endnotes.xml"] {
            let mut file = zip
                .by_name(part_name)
                .unwrap_or_else(|e| panic!("{part_name}: {e}"));
            let mut xml = String::new();
            file.read_to_string(&mut xml)
                .unwrap_or_else(|e| panic!("read {part_name}: {e}"));
            assert!(
                xml.contains("<w:r><w:separator"),
                "{part_name} should wrap separator notes in w:r, xml={xml}"
            );
            assert!(
                xml.contains("<w:r><w:continuationSeparator"),
                "{part_name} should wrap continuationSeparator notes in w:r, xml={xml}"
            );
        }
    }

    #[test]
    fn append_literal_prefix_runs_emits_space_separator_with_leading_tab() {
        fn collect_run_text(run: &Element) -> String {
            let mut out = String::new();
            for child in &run.children {
                match child {
                    XMLNode::Element(el) if local_element_name(el) == "tab" => out.push('\t'),
                    XMLNode::Element(el)
                        if local_element_name(el) == "t" || local_element_name(el) == "delText" =>
                    {
                        if let Some(text) = el.get_text() {
                            out.push_str(&text);
                        }
                    }
                    _ => {}
                }
            }
            out
        }

        let mut paragraph = w_el("p");
        let mut next_id = 1u32;
        append_literal_prefix_runs(
            &mut paragraph,
            "(f)",
            "\t",
            "",
            false,
            false,
            None,
            None,
            &[],
            &StyleProps::default(),
            RunDirectness::ALL,
            false,
            &mut next_id,
        );

        let runs: Vec<&Element> = paragraph
            .children
            .iter()
            .filter_map(|child| match child {
                XMLNode::Element(el) if local_element_name(el) == "r" => Some(el),
                _ => None,
            })
            .collect();

        assert_eq!(
            runs.len(),
            2,
            "prefix without trailing tab should emit a separator run"
        );
        assert_eq!(collect_run_text(runs[0]), "\t(f)");
        assert_eq!(
            collect_run_text(runs[1]),
            " ",
            "empty captured separator falls back to one space (legacy behavior)"
        );
    }

    /// When a target-origin bookmarkStart is collected into a Normal block
    /// during merge and its paired bookmarkEnd lives in an Inserted block,
    /// the decoration's `origin` field must ensure both halves use the same
    /// remap key ("target:7") so they get matching IDs.
    ///
    /// Without the `origin` field, the Normal block would use "base" origin
    /// for the bookmarkStart remap key ("base:7") while the Inserted block
    /// uses "target" origin for bookmarkEnd ("target:7") — producing
    /// mismatched IDs and orphaned bookmarks.
    #[test]
    fn target_decoration_in_normal_block_pairs_with_inserted_block() {
        use crate::domain::{
            DecorationNode, DecorationType, DocPart, NodeId, ParagraphNode, ProofRef, RevisionInfo,
            TextNode, TrackedBlock, TrackedSegment, TrackingStatus,
        };

        let bm_start_raw = b"<w:bookmarkStart xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" w:id=\"7\" w:name=\"test_bm\"/>";
        let bm_end_raw = b"<w:bookmarkEnd xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" w:id=\"7\"/>";

        // Block 1: Normal block with a target-origin bookmarkStart.
        // This simulates what happens when apply_block_modified collects a
        // target decoration into the first Normal segment.
        let normal_block = TrackedBlock {
            status: TrackingStatus::Normal,
            block: crate::domain::BlockNode::from(ParagraphNode {
                id: NodeId::from("p1"),
                style_id: None,
                align: None,
                has_direct_align: false,
                indent: None,
                has_direct_indent: false,
                authored_indent: None,
                spacing: None,
                has_direct_spacing: false,
                authored_spacing: None,
                borders: None,
                keep_next: None,
                keep_lines: None,
                page_break_before: false,
                widow_control: None,
                contextual_spacing: None,
                shading: None,
                has_direct_keep_next: true,
                has_direct_keep_lines: true,
                has_direct_page_break_before: true,
                has_direct_widow_control: true,
                has_direct_contextual_spacing: true,
                has_direct_shading: true,
                has_direct_borders: true,
                tab_stops: vec![],
                effective_tab_stops_rel: vec![],
                segments: vec![TrackedSegment {
                    status: TrackingStatus::Normal,
                    inlines: vec![
                        InlineNode::from(DecorationNode {
                            id: NodeId::from("d1"),
                            kind: DecorationType::Bookmark,
                            opaque_ref: "p1:deco:1".to_string(),
                            proof_ref: ProofRef {
                                part: DocPart::DocumentXml,
                                block_id: NodeId::from("d1"),
                                docx_anchor: "p1:deco:1".to_string(),
                            },
                            wrapper_marks: Vec::new(),
                            wrapper_style_props: StyleProps::default(),
                            raw_xml: Some(bm_start_raw.to_vec()),
                            // Key: this decoration came from target during merge
                            origin: Some("target".to_string()),
                        }),
                        InlineNode::from(TextNode {
                            id: NodeId::from("t1"),
                            text_role: None,
                            text: "hello".to_string(),
                            marks: vec![],
                            style_props: StyleProps::default(),
                            rpr_authored: crate::domain::RunRprAuthored::default(),
                            formatting_change: None,
                        }),
                    ],
                }],
                block_text_hash: None,
                numbering: None,
                has_direct_numbering: true,
                numbering_suppressed: false,
                materialized_numbering: None,
                rendered_text: None,
                literal_prefix: None,
                literal_prefix_marks: Vec::new(),
                literal_prefix_style_props: crate::domain::StyleProps::default(),
                literal_prefix_rpr_authored: crate::domain::RunRprAuthored::default(),
                literal_prefix_leading_rpr: None,
                literal_prefix_trailing_rpr: None,
                literal_prefix_leading_tab_twips: None,
                literal_prefix_leading_tab_count: 0,
                literal_prefix_leading_ws: String::new(),
                literal_prefix_trailing_ws: String::new(),
                literal_prefix_has_trailing_tab: false,
                literal_prefix_trailing_tab_stop_twips: None,
                outline_lvl: None,
                heading_level: None,
                para_mark_status: None,
                paragraph_mark_marks: vec![],
                paragraph_mark_style_props: StyleProps::default(),
                paragraph_mark_rpr_off: Default::default(),
                para_split: false,
                section_property_change: None,
                formatting_change: None,
                section_properties: None,
                mirror_indents: None,
                auto_space_de: None,
                auto_space_dn: None,
                bidi: None,
                text_alignment: None,
                suppress_auto_hyphens: None,
                snap_to_grid: None,
                overflow_punct: None,
                adjust_right_ind: None,
                word_wrap: None,
                frame_pr: None,
                para_id: None,
                text_id: None,
                text_direction: None,
                cnf_style: None,
                preserved_ppr: Vec::new(),
            }),
            move_id: None,
            block_sdt_wrap: None,
        };

        // Block 2: Inserted block with the paired bookmarkEnd.
        // The Inserted status gives this block origin="target" automatically.
        let inserted_block = TrackedBlock {
            status: TrackingStatus::Inserted(RevisionInfo {
                revision_id: 1,
                author: Some("test".to_string()),
                date: Some("2026-01-01T00:00:00Z".to_string()),
                apply_op_id: None,
            }),
            block: crate::domain::BlockNode::from(ParagraphNode {
                id: NodeId::from("p2"),
                style_id: None,
                align: None,
                has_direct_align: false,
                indent: None,
                has_direct_indent: false,
                authored_indent: None,
                spacing: None,
                has_direct_spacing: false,
                authored_spacing: None,
                borders: None,
                keep_next: None,
                keep_lines: None,
                page_break_before: false,
                widow_control: None,
                contextual_spacing: None,
                shading: None,
                has_direct_keep_next: true,
                has_direct_keep_lines: true,
                has_direct_page_break_before: true,
                has_direct_widow_control: true,
                has_direct_contextual_spacing: true,
                has_direct_shading: true,
                has_direct_borders: true,
                tab_stops: vec![],
                effective_tab_stops_rel: vec![],
                segments: vec![TrackedSegment {
                    status: TrackingStatus::Inserted(RevisionInfo {
                        revision_id: 1,
                        author: Some("test".to_string()),
                        date: Some("2026-01-01T00:00:00Z".to_string()),
                        apply_op_id: None,
                    }),
                    inlines: vec![
                        InlineNode::from(DecorationNode {
                            id: NodeId::from("d2"),
                            kind: DecorationType::Bookmark,
                            opaque_ref: "p2:deco:1".to_string(),
                            proof_ref: ProofRef {
                                part: DocPart::DocumentXml,
                                block_id: NodeId::from("d2"),
                                docx_anchor: "p2:deco:1".to_string(),
                            },
                            wrapper_marks: Vec::new(),
                            wrapper_style_props: StyleProps::default(),
                            raw_xml: Some(bm_end_raw.to_vec()),
                            // No origin override needed — block is Inserted,
                            // so it naturally gets origin="target"
                            origin: None,
                        }),
                        InlineNode::from(TextNode {
                            id: NodeId::from("t2"),
                            text_role: None,
                            text: "world".to_string(),
                            marks: vec![],
                            style_props: StyleProps::default(),
                            rpr_authored: crate::domain::RunRprAuthored::default(),
                            formatting_change: None,
                        }),
                    ],
                }],
                block_text_hash: None,
                numbering: None,
                has_direct_numbering: true,
                numbering_suppressed: false,
                materialized_numbering: None,
                rendered_text: None,
                literal_prefix: None,
                literal_prefix_marks: Vec::new(),
                literal_prefix_style_props: crate::domain::StyleProps::default(),
                literal_prefix_rpr_authored: crate::domain::RunRprAuthored::default(),
                literal_prefix_leading_rpr: None,
                literal_prefix_trailing_rpr: None,
                literal_prefix_leading_tab_twips: None,
                literal_prefix_leading_tab_count: 0,
                literal_prefix_leading_ws: String::new(),
                literal_prefix_trailing_ws: String::new(),
                literal_prefix_has_trailing_tab: false,
                literal_prefix_trailing_tab_stop_twips: None,
                outline_lvl: None,
                heading_level: None,
                para_mark_status: None,
                paragraph_mark_marks: vec![],
                paragraph_mark_style_props: StyleProps::default(),
                paragraph_mark_rpr_off: Default::default(),
                para_split: false,
                section_property_change: None,
                formatting_change: None,
                section_properties: None,
                mirror_indents: None,
                auto_space_de: None,
                auto_space_dn: None,
                bidi: None,
                text_alignment: None,
                suppress_auto_hyphens: None,
                snap_to_grid: None,
                overflow_punct: None,
                adjust_right_ind: None,
                word_wrap: None,
                frame_pr: None,
                para_id: None,
                text_id: None,
                text_direction: None,
                cnf_style: None,
                preserved_ppr: Vec::new(),
            }),
            move_id: None,
            block_sdt_wrap: None,
        };

        // The per-part policy is built from ONE pre-scan over every block the
        // part will emit (mirrors serialize_canonical_docx), then shared by
        // both serialize calls — that is what keeps the cross-block pair
        // consistent.
        let mut next_id: u32 = 100;
        let bookmark_policy = {
            let mut scan = BookmarkScan::default();
            scan.scan_tracked_blocks(std::slice::from_ref(&normal_block));
            scan.scan_tracked_blocks(std::slice::from_ref(&inserted_block));
            scan.into_policy(&mut next_id)
        };
        let el1 =
            serialize_tracked_block(&normal_block, &mut next_id, &bookmark_policy, None).unwrap();
        let el2 =
            serialize_tracked_block(&inserted_block, &mut next_id, &bookmark_policy, None).unwrap();

        fn find_bookmark_start_id(el: &Element) -> Option<String> {
            if local_element_name(el) == "bookmarkStart" {
                return attr_get(el, "w:id").cloned();
            }
            for child in &el.children {
                if let XMLNode::Element(child_el) = child
                    && let Some(id) = find_bookmark_start_id(child_el)
                {
                    return Some(id);
                }
            }
            None
        }

        fn find_bookmark_end_id(el: &Element) -> Option<String> {
            if local_element_name(el) == "bookmarkEnd" {
                return attr_get(el, "w:id").cloned();
            }
            for child in &el.children {
                if let XMLNode::Element(child_el) = child
                    && let Some(id) = find_bookmark_end_id(child_el)
                {
                    return Some(id);
                }
            }
            None
        }

        let start_id = find_bookmark_start_id(&el1)
            .expect("bookmarkStart must be present in serialized Normal block");
        let end_id = find_bookmark_end_id(&el2)
            .expect("bookmarkEnd must be present in serialized Inserted block");

        assert_eq!(
            start_id, end_id,
            "target bookmarkStart in Normal block and bookmarkEnd in Inserted block must get the same remapped ID \
             (start={start_id}, end={end_id})"
        );
    }

    #[test]
    fn serialize_empty_paragraph_preserves_paragraph_mark_rpr() {
        use crate::domain::{NodeId, ParagraphNode};

        let paragraph = ParagraphNode {
            id: NodeId::from("p1"),
            style_id: None,
            align: None,
            has_direct_align: false,
            indent: None,
            has_direct_indent: false,
            authored_indent: None,
            spacing: None,
            has_direct_spacing: false,
            authored_spacing: None,
            borders: None,
            keep_next: None,
            keep_lines: None,
            page_break_before: false,
            widow_control: None,
            contextual_spacing: None,
            shading: None,
            has_direct_keep_next: true,
            has_direct_keep_lines: true,
            has_direct_page_break_before: true,
            has_direct_widow_control: true,
            has_direct_contextual_spacing: true,
            has_direct_shading: true,
            has_direct_borders: true,
            tab_stops: vec![],
            effective_tab_stops_rel: vec![],
            segments: vec![],
            block_text_hash: None,
            numbering: None,
            has_direct_numbering: true,
            numbering_suppressed: false,
            materialized_numbering: None,
            rendered_text: None,
            literal_prefix: None,
            literal_prefix_marks: Vec::new(),
            literal_prefix_style_props: crate::domain::StyleProps::default(),
            literal_prefix_rpr_authored: crate::domain::RunRprAuthored::default(),
            literal_prefix_leading_rpr: None,
            literal_prefix_trailing_rpr: None,
            literal_prefix_leading_tab_twips: None,
            literal_prefix_leading_tab_count: 0,
            literal_prefix_leading_ws: String::new(),
            literal_prefix_trailing_ws: String::new(),
            literal_prefix_has_trailing_tab: false,
            literal_prefix_trailing_tab_stop_twips: None,
            outline_lvl: None,
            heading_level: None,
            para_mark_status: None,
            paragraph_mark_marks: vec![Mark::Bold],
            paragraph_mark_style_props: StyleProps {
                font_size: Some(22),
                color: Some("FF0000".into()),
                ..StyleProps::default()
            },
            paragraph_mark_rpr_off: Default::default(),
            para_split: false,
            section_property_change: None,
            formatting_change: None,
            section_properties: None,
            mirror_indents: None,
            auto_space_de: None,
            auto_space_dn: None,
            bidi: None,
            text_alignment: None,
            text_direction: None,
            suppress_auto_hyphens: None,
            snap_to_grid: None,
            overflow_punct: None,
            adjust_right_ind: None,
            word_wrap: None,
            frame_pr: None,
            para_id: None,
            text_id: None,
            cnf_style: None,
            preserved_ppr: Vec::new(),
        };

        let mut next_id = 1;
        let ppr = build_paragraph_properties(&paragraph, &mut next_id, None)
            .expect("pPr should be emitted for paragraph-mark rPr");
        let rpr = ppr
            .children
            .iter()
            .find_map(|child| match child {
                XMLNode::Element(el) if local_element_name(el) == "rPr" => Some(el),
                _ => None,
            })
            .expect("paragraph properties should contain rPr");

        assert!(
            rpr.children.iter().any(|child| matches!(
                child,
                XMLNode::Element(el) if local_element_name(el) == "b"
            )),
            "paragraph mark rPr should preserve bold"
        );
        assert!(
            rpr.children.iter().any(|child| matches!(
                child,
                XMLNode::Element(el)
                    if local_element_name(el) == "sz"
                        && attr_get(el, "w:val") == Some(&"22".to_string())
            )),
            "paragraph mark rPr should preserve font size"
        );
        assert!(
            rpr.children.iter().any(|child| matches!(
                child,
                XMLNode::Element(el)
                    if local_element_name(el) == "color"
                        && attr_get(el, "w:val") == Some(&"FF0000".to_string())
            )),
            "paragraph mark rPr should preserve color"
        );
    }

    /// §17.9.18: a paragraph carrying `numbering_suppressed` (parsed from
    /// `w:numPr/w:numId=0`) must re-emit the suppression marker
    /// `<w:numPr><w:ilvl w:val="0"/><w:numId w:val="0"/></w:numPr>` so it does
    /// NOT silently re-inherit its pStyle's numbering on round-trip. Mirrors
    /// the corpus regression where Sub-Header (numId=3) paragraphs that
    /// suppressed via numId=0 re-grew a list label after reserialization.
    #[test]
    fn suppressed_numbering_reserializes_numid_zero_not_style_numid() {
        use crate::domain::{NodeId, ParagraphNode};

        let mut paragraph = ParagraphNode {
            id: NodeId::from("p1"),
            style_id: Some("Sub-Header".into()),
            align: None,
            has_direct_align: false,
            indent: None,
            has_direct_indent: false,
            authored_indent: None,
            spacing: None,
            has_direct_spacing: false,
            authored_spacing: None,
            borders: None,
            keep_next: None,
            keep_lines: None,
            page_break_before: false,
            widow_control: None,
            contextual_spacing: None,
            shading: None,
            has_direct_keep_next: true,
            has_direct_keep_lines: true,
            has_direct_page_break_before: true,
            has_direct_widow_control: true,
            has_direct_contextual_spacing: true,
            has_direct_shading: true,
            has_direct_borders: true,
            tab_stops: vec![],
            effective_tab_stops_rel: vec![],
            segments: vec![],
            block_text_hash: None,
            numbering: None,
            has_direct_numbering: true,
            // Mutually exclusive with numbering: Some(..). This paragraph
            // explicitly removes inherited numbering.
            numbering_suppressed: true,
            materialized_numbering: None,
            rendered_text: None,
            literal_prefix: None,
            literal_prefix_marks: Vec::new(),
            literal_prefix_style_props: crate::domain::StyleProps::default(),
            literal_prefix_rpr_authored: crate::domain::RunRprAuthored::default(),
            literal_prefix_leading_rpr: None,
            literal_prefix_trailing_rpr: None,
            literal_prefix_leading_tab_twips: None,
            literal_prefix_leading_tab_count: 0,
            literal_prefix_leading_ws: String::new(),
            literal_prefix_trailing_ws: String::new(),
            literal_prefix_has_trailing_tab: false,
            literal_prefix_trailing_tab_stop_twips: None,
            outline_lvl: None,
            heading_level: None,
            para_mark_status: None,
            paragraph_mark_marks: vec![],
            paragraph_mark_style_props: StyleProps::default(),
            paragraph_mark_rpr_off: Default::default(),
            para_split: false,
            section_property_change: None,
            formatting_change: None,
            section_properties: None,
            mirror_indents: None,
            auto_space_de: None,
            auto_space_dn: None,
            bidi: None,
            text_alignment: None,
            text_direction: None,
            suppress_auto_hyphens: None,
            snap_to_grid: None,
            overflow_punct: None,
            adjust_right_ind: None,
            word_wrap: None,
            frame_pr: None,
            para_id: None,
            text_id: None,
            cnf_style: None,
            preserved_ppr: Vec::new(),
        };

        let mut next_id = 1;
        let ppr = build_paragraph_properties(&paragraph, &mut next_id, None)
            .expect("pPr should be emitted for a suppressed paragraph");

        let num_pr = ppr
            .children
            .iter()
            .find_map(|child| match child {
                XMLNode::Element(el) if local_element_name(el) == "numPr" => Some(el),
                _ => None,
            })
            .expect("suppressed paragraph must emit a w:numPr");

        let num_id = num_pr
            .children
            .iter()
            .find_map(|child| match child {
                XMLNode::Element(el) if local_element_name(el) == "numId" => Some(el),
                _ => None,
            })
            .expect("w:numPr must contain a w:numId");
        assert_eq!(
            attr_get(num_id, "w:val"),
            Some(&"0".to_string()),
            "§17.9.18: suppressed paragraph must re-emit numId=0, NOT re-inherit \
             its style's numId"
        );

        let ilvl = num_pr
            .children
            .iter()
            .find_map(|child| match child {
                XMLNode::Element(el) if local_element_name(el) == "ilvl" => Some(el),
                _ => None,
            })
            .expect("w:numPr must contain a w:ilvl");
        assert_eq!(attr_get(ilvl, "w:val"), Some(&"0".to_string()));

        // Negative control: with suppression cleared and no active numbering,
        // NO numPr is emitted (the paragraph would then inherit from its style).
        paragraph.numbering_suppressed = false;
        let ppr2 = build_paragraph_properties(&paragraph, &mut next_id, None);
        let has_num_pr = ppr2.as_ref().is_some_and(|p| {
            p.children.iter().any(
                |child| matches!(child, XMLNode::Element(el) if local_element_name(el) == "numPr"),
            )
        });
        assert!(
            !has_num_pr,
            "a non-suppressed paragraph with no active numbering must NOT emit w:numPr"
        );
    }

    /// A hyperlink with mixed Normal / Inserted / Deleted runs must
    /// serialize each tracked-status group inside a `<w:ins>` or `<w:del>`
    /// envelope, while Normal runs appear as bare `<w:r>` children. This
    /// is the structure Word expects (ECMA-376 §17.13.5: CT_Hyperlink
    /// accepts EG_PContent).
    #[test]
    fn hyperlink_with_tracked_runs_emits_ins_and_del_envelopes() {
        use crate::domain::RevisionInfo;
        let rev = RevisionInfo {
            revision_id: 42,
            author: Some("Test".to_string()),
            date: Some("2026-05-19T10:00:00Z".to_string()),
            apply_op_id: None,
        };
        let data = HyperlinkData {
            url: Some("https://example.com".to_string()),
            anchor: None,
            text: "before middle after".to_string(),
            r_id: Some("rId1".to_string()),
            runs: vec![
                HyperlinkRun {
                    text: "before ".to_string(),
                    rpr_xml: None,
                    status: TrackingStatus::Normal,
                },
                HyperlinkRun {
                    text: "old".to_string(),
                    rpr_xml: None,
                    status: TrackingStatus::Deleted(rev.clone()),
                },
                HyperlinkRun {
                    text: "new".to_string(),
                    rpr_xml: None,
                    status: TrackingStatus::Inserted(rev.clone()),
                },
                HyperlinkRun {
                    text: " after".to_string(),
                    rpr_xml: None,
                    status: TrackingStatus::Normal,
                },
            ],
            extra_attrs: vec![],
        };
        let el = build_hyperlink_element(&data);
        let local_names: Vec<&str> = el
            .children
            .iter()
            .filter_map(|c| match c {
                XMLNode::Element(e) => Some(local_element_name(e)),
                _ => None,
            })
            .collect();
        assert_eq!(
            local_names,
            vec!["r", "del", "ins", "r"],
            "expected bare-r + del-envelope + ins-envelope + bare-r"
        );

        // The del envelope must carry the revision id/author/date and
        // contain a run whose text is emitted as <w:delText>.
        let del = el
            .children
            .iter()
            .find_map(|c| match c {
                XMLNode::Element(e) if local_element_name(e) == "del" => Some(e),
                _ => None,
            })
            .unwrap();
        assert_eq!(attr_get(del, "w:id"), Some(&"42".to_string()));
        assert_eq!(attr_get(del, "w:author"), Some(&"Test".to_string()));
        let r_in_del = del
            .children
            .iter()
            .find_map(|c| match c {
                XMLNode::Element(e) if local_element_name(e) == "r" => Some(e),
                _ => None,
            })
            .unwrap();
        let del_text_local = r_in_del
            .children
            .iter()
            .find_map(|c| match c {
                XMLNode::Element(e) => Some(local_element_name(e).to_string()),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            del_text_local, "delText",
            "runs inside <w:del> must use <w:delText>, not <w:t>"
        );
    }

    /// A hyperlink with all-Normal runs (the typical case after import or
    /// after accept/reject projection) emits only bare `<w:r>` children —
    /// no `<w:ins>` / `<w:del>` envelopes, no extra wrappers.
    #[test]
    fn hyperlink_with_all_normal_runs_emits_bare_r_only() {
        let data = HyperlinkData {
            url: Some("https://example.com".to_string()),
            anchor: None,
            text: "click here".to_string(),
            r_id: Some("rId1".to_string()),
            runs: vec![HyperlinkRun {
                text: "click here".to_string(),
                rpr_xml: None,
                status: TrackingStatus::Normal,
            }],
            extra_attrs: vec![],
        };
        let el = build_hyperlink_element(&data);
        let names: Vec<String> = el
            .children
            .iter()
            .filter_map(|c| match c {
                XMLNode::Element(e) => Some(local_element_name(e).to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(names, vec!["r".to_string()]);
    }

    // ── cell content-control span ratchet ────────────────────────────────
    //
    // `wrap_cell_blocks_in_content_sdts` re-wraps a cell's serialized blocks in
    // their preserved block-level content controls. It must emit each control
    // around EXACTLY the blocks its range names — and refuse (rather than
    // silently swallow a sibling) when the ranges are malformed. Import
    // establishes the range invariants; these tests pin the export-side guard.

    fn wrap_at(start: usize, span: usize) -> crate::domain::CellSdtWrap {
        crate::domain::CellSdtWrap {
            start,
            span,
            wrapper: crate::domain::SdtWrapper {
                // Empty sdtPr keeps the fixture minimal; build_sdt_wrapper skips
                // an empty property fragment, so the emitted shape is
                // <w:sdt><w:sdtContent>…</w:sdtContent></w:sdt>.
                sdt_pr_xml: Vec::new(),
                sdt_end_pr_xml: None,
            },
        }
    }

    fn labeled_paragraph(text: &str) -> Element {
        let mut p = w_el("p");
        let mut r = w_el("r");
        let mut t = w_el("t");
        t.children.push(xmltree::XMLNode::Text(text.to_string()));
        r.children.push(xmltree::XMLNode::Element(t));
        p.children.push(xmltree::XMLNode::Element(r));
        p
    }

    /// Count `w:sdtContent` elements and, for the first, its direct block
    /// children — enough to assert the span partition.
    fn sdt_and_first_content_blocks(nodes: &[XMLNode]) -> (usize, usize) {
        fn walk(node: &XMLNode, sdt_count: &mut usize, first_blocks: &mut Option<usize>) {
            if let XMLNode::Element(el) = node {
                if local_element_name(el) == "sdtContent" {
                    *sdt_count += 1;
                    if first_blocks.is_none() {
                        *first_blocks = Some(
                            el.children
                                .iter()
                                .filter(|c| {
                                    matches!(c, XMLNode::Element(e)
                                        if local_element_name(e) == "p"
                                            || local_element_name(e) == "tbl")
                                })
                                .count(),
                        );
                    }
                }
                for c in &el.children {
                    walk(c, sdt_count, first_blocks);
                }
            }
        }
        let mut sdt_count = 0;
        let mut first_blocks = None;
        for n in nodes {
            walk(n, &mut sdt_count, &mut first_blocks);
        }
        (sdt_count, first_blocks.unwrap_or(0))
    }

    #[test]
    fn cell_sdt_wrap_emits_exactly_span_blocks_and_keeps_siblings() {
        // blocks [glyph, label]; a span-1 wrap over block 0 must leave the label
        // a sibling of the w:sdt, not inside its sdtContent.
        let blocks = vec![labeled_paragraph("Glyph"), labeled_paragraph("Label")];
        let id = crate::domain::NodeId::from("cell".to_string());
        let out = wrap_cell_blocks_in_content_sdts(blocks, &[wrap_at(0, 1)], &id)
            .expect("valid single-block wrap");
        // One sdtContent, holding exactly one block; two top-level nodes (the
        // w:sdt then the bare label paragraph).
        let (sdt_count, first_blocks) = sdt_and_first_content_blocks(&out);
        assert_eq!(sdt_count, 1, "exactly one content control emitted");
        assert_eq!(first_blocks, 1, "the control wraps exactly its one block");
        assert_eq!(out.len(), 2, "the label stays a sibling of the w:sdt");
    }

    #[test]
    fn cell_sdt_wrap_rejects_span_past_blocks() {
        let blocks = vec![labeled_paragraph("only")];
        let id = crate::domain::NodeId::from("cell".to_string());
        let err = wrap_cell_blocks_in_content_sdts(blocks, &[wrap_at(0, 2)], &id)
            .expect_err("span past the cell's blocks must fail loud");
        assert_eq!(err.code, ErrorCode::ValidationFailed);
    }

    #[test]
    fn cell_sdt_wrap_rejects_overlapping_ranges() {
        let blocks = vec![
            labeled_paragraph("a"),
            labeled_paragraph("b"),
            labeled_paragraph("c"),
        ];
        let id = crate::domain::NodeId::from("cell".to_string());
        // [0,2) then [1,3) overlap.
        let err = wrap_cell_blocks_in_content_sdts(blocks, &[wrap_at(0, 2), wrap_at(1, 2)], &id)
            .expect_err("overlapping wraps must fail loud");
        assert_eq!(err.code, ErrorCode::ValidationFailed);
    }

    #[test]
    fn cell_sdt_wrap_rejects_zero_span() {
        let blocks = vec![labeled_paragraph("a")];
        let id = crate::domain::NodeId::from("cell".to_string());
        let err = wrap_cell_blocks_in_content_sdts(blocks, &[wrap_at(0, 0)], &id)
            .expect_err("a zero-span wrap must fail loud");
        assert_eq!(err.code, ErrorCode::ValidationFailed);
    }
}
