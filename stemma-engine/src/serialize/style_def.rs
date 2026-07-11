//! Deterministic `w:style` definition builder (§17.7.4) for the
//! CreateStyle/ModifyStyle verb.
//!
//! Given a [`crate::edit::verbs::style_defs::StyleDefinition`], build the full
//! `<w:style …>…</w:style>` fragment as self-contained bytes. The save path
//! (`runtime::apply_pending_style_ops`) splices these bytes into
//! `word/styles.xml` (Create = append, Modify = merge-by-styleId — authored fields replace
//! their counterparts; unauthored fields of the existing style survive).
//!
//! Child order follows CT_Style (ECMA-376 Annex A): `name` → `basedOn` → `pPr` →
//! `rPr`. We reuse [`crate::serialize::build_rpr`] for the run properties so the
//! style's `rPr` is byte-identical to a direct-formatting `rPr` (same field
//! witness, same element order), and build a focused `pPr` (alignment, spacing,
//! indentation) inline — the style-definition subset, not a whole paragraph.

use xmltree::{Element, XMLNode};

use crate::domain::{Mark, StyleProps};
use crate::edit::verbs::style_defs::{StyleDefinition, StyleType};
use crate::serialize::{alignment_to_string, build_rpr};
use crate::word_xml::{serialize_raw_fragment, w_el};
use crate::xml_attrs::attr_set;

/// Serialize a [`StyleDefinition`] to a self-contained `w:style` fragment.
///
/// The fragment declares the `w:` (and, via `build_rpr`, any other) namespaces
/// it uses on the root so it round-trips through `Element::parse` in the save
/// path. The `w:styleId` attribute equals `def.style_id` exactly — the save path
/// asserts this match before splicing.
pub(crate) fn build_style_fragment(def: &StyleDefinition) -> Vec<u8> {
    let mut style = w_el("style");
    attr_set(&mut style, "w:type", def.style_type.to_xml_str());
    attr_set(&mut style, "w:styleId", def.style_id.clone());

    // --- w:name (required by Word for a usable style) ---
    let mut name = w_el("name");
    attr_set(&mut name, "w:val", def.name.clone());
    style.children.push(XMLNode::Element(name));

    // --- w:basedOn (optional) ---
    if let Some(based_on) = &def.based_on {
        let mut bo = w_el("basedOn");
        attr_set(&mut bo, "w:val", based_on.clone());
        style.children.push(XMLNode::Element(bo));
    }

    // --- w:pPr (paragraph/numbering/table styles may carry paragraph props) ---
    if let Some(ppr) = build_style_ppr(def) {
        style.children.push(XMLNode::Element(ppr));
    }

    // --- w:rPr (reuse the direct-formatting builder) ---
    if let Some(rpr) = build_style_rpr(def) {
        style.children.push(XMLNode::Element(rpr));
    }

    serialize_raw_fragment(&style)
}

/// Build the style's `w:rPr` by mapping the run-props subset onto a
/// [`StyleProps`] + `Vec<Mark>` and delegating to [`build_rpr`]. Returns `None`
/// when the subset is entirely empty (no `w:rPr` child emitted).
fn build_style_rpr(def: &StyleDefinition) -> Option<Element> {
    let rp = &def.run_props;
    let mut marks = Vec::new();
    if rp.bold {
        marks.push(Mark::Bold);
    }
    if rp.italic {
        marks.push(Mark::Italic);
    }
    if rp.underline {
        marks.push(Mark::Underline);
    }

    let mut props = StyleProps::default();
    if let Some(size) = rp.font_size_half_points {
        props.font_size = Some(size);
    }
    if let Some(color) = &rp.color {
        props.color = Some(color.as_str().into());
    }
    if let Some(font) = &rp.font_family {
        props.font_family = Some(font.as_str().into());
    }

    let nothing = marks.is_empty()
        && props.font_size.is_none()
        && props.color.is_none()
        && props.font_family.is_none();
    if nothing {
        return None;
    }

    let rpr = build_rpr(&marks, &props);
    // `build_rpr` always returns a `w:rPr` element (possibly empty); we only
    // reach here when at least one child will be present.
    Some(rpr)
}

/// Build a focused style `w:pPr` (alignment, spacing, indentation) in CT_PPrBase
/// order. Returns `None` when no paragraph property is set. Numbering/table
/// styles share the paragraph-props subset; the OOXML schema permits `w:pPr` on
/// all four style types.
fn build_style_ppr(def: &StyleDefinition) -> Option<Element> {
    let pp = &def.para_props;
    let mut ppr = w_el("pPr");
    let mut has_any = false;

    // CT_PPrBase order: … spacing (pos 21) … ind (pos 22) … jc (pos 26).

    // --- w:spacing ---
    if pp.spacing_before.is_some() || pp.spacing_after.is_some() || pp.line_spacing.is_some() {
        let mut sp = w_el("spacing");
        if let Some(before) = pp.spacing_before {
            attr_set(&mut sp, "w:before", before.to_string());
        }
        if let Some(after) = pp.spacing_after {
            attr_set(&mut sp, "w:after", after.to_string());
        }
        if let Some(line) = pp.line_spacing {
            attr_set(&mut sp, "w:line", line.to_string());
            // A bare w:line with no rule means "auto" multiples of 240ths per
            // §17.3.1.33; we state it explicitly rather than relying on default.
            attr_set(&mut sp, "w:lineRule", "auto");
        }
        ppr.children.push(XMLNode::Element(sp));
        has_any = true;
    }

    // --- w:ind ---
    if pp.indent_left.is_some() || pp.indent_right.is_some() || pp.indent_first_line.is_some() {
        let mut ind = w_el("ind");
        if let Some(left) = pp.indent_left {
            attr_set(&mut ind, "w:left", left.to_string());
        }
        if let Some(right) = pp.indent_right {
            attr_set(&mut ind, "w:right", right.to_string());
        }
        if let Some(first) = pp.indent_first_line {
            if first >= 0 {
                attr_set(&mut ind, "w:firstLine", first.to_string());
            } else {
                attr_set(&mut ind, "w:hanging", (-first).to_string());
            }
        }
        ppr.children.push(XMLNode::Element(ind));
        has_any = true;
    }

    // --- w:jc ---
    if let Some(align) = &pp.alignment {
        let mut jc = w_el("jc");
        attr_set(&mut jc, "w:val", alignment_to_string(align));
        ppr.children.push(XMLNode::Element(jc));
        has_any = true;
    }

    if has_any { Some(ppr) } else { None }
}

impl StyleType {
    /// The `w:type` attribute value (§17.7.4.18 ST_StyleType).
    pub fn to_xml_str(self) -> &'static str {
        match self {
            StyleType::Para => "paragraph",
            StyleType::Char => "character",
            StyleType::Table => "table",
            StyleType::Numbering => "numbering",
        }
    }
}
