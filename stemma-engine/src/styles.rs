//! Style resolution for Word documents.
//!
//! Parses `word/styles.xml` and resolves the formatting inheritance chain:
//! - Run properties (rPr): direct → character style → paragraph style → document defaults.
//! - Paragraph properties (pPr): direct → paragraph style → basedOn chain (tab stops).
//! - Table properties (tblPr): direct → table style → basedOn chain.

use std::collections::{HashMap, HashSet};

use xmltree::{Element, XMLNode};

use crate::domain::{Alignment, Border, BorderSet, CellMargins, IStr, Shading};
use crate::numbering::LevelIndent;
use crate::word_ir::{
    BorderEdge, DirectNumPr, IndentProps, MarkValue, NumProps, ParagraphBorderProps, SpacingProps,
    TabStopDef, TextMarks,
};
use crate::xml_attrs::attr_get;

const WORD_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

/// Theme font definitions parsed from word/theme/theme1.xml.
///
/// Maps theme font references (e.g., "majorHAnsi", "minorEastAsia") to actual
/// font family names. Per ISO 29500-1 §17.3.2.26, rFonts can reference theme
/// fonts via asciiTheme/hAnsiTheme/eastAsiaTheme/csTheme attributes.
#[derive(Clone, Debug, Default)]
pub struct ThemeFonts {
    /// Major font latin typeface (for majorHAnsi/majorAscii).
    pub major_latin: Option<String>,
    /// Minor font latin typeface (for minorHAnsi/minorAscii).
    pub minor_latin: Option<String>,
    /// Major font east asian typeface (for majorEastAsia).
    pub major_east_asia: Option<String>,
    /// Minor font east asian typeface (for minorEastAsia).
    pub minor_east_asia: Option<String>,
    /// Major font complex script typeface (for majorBidi).
    pub major_cs: Option<String>,
    /// Minor font complex script typeface (for minorBidi).
    pub minor_cs: Option<String>,
}

impl ThemeFonts {
    /// Parse theme font definitions from word/theme/theme1.xml bytes.
    pub fn parse(xml_bytes: &[u8]) -> Result<Self, String> {
        if xml_bytes.is_empty() {
            return Err("word/theme/theme1.xml is empty".to_string());
        }
        let root = crate::word_xml::parse_document_xml(xml_bytes)
            .map_err(|err| format!("failed to parse word/theme/theme1.xml: {err:?}"))?;
        let theme_elements = root
            .children
            .iter()
            .find_map(|c| match c {
                XMLNode::Element(el) if el.name == "themeElements" => Some(el),
                _ => None,
            })
            .ok_or_else(|| "word/theme/theme1.xml missing themeElements".to_string())?;
        let font_scheme = theme_elements
            .children
            .iter()
            .find_map(|c| match c {
                XMLNode::Element(el) if el.name == "fontScheme" => Some(el),
                _ => None,
            })
            .ok_or_else(|| "word/theme/theme1.xml missing fontScheme".to_string())?;

        let mut fonts = ThemeFonts::default();
        for child in &font_scheme.children {
            let el = match child {
                XMLNode::Element(el) => el,
                _ => continue,
            };
            match el.name.as_str() {
                "majorFont" => {
                    fonts.major_latin = find_typeface(el, "latin");
                    fonts.major_east_asia = find_typeface(el, "ea");
                    fonts.major_cs = find_typeface(el, "cs");
                }
                "minorFont" => {
                    fonts.minor_latin = find_typeface(el, "latin");
                    fonts.minor_east_asia = find_typeface(el, "ea");
                    fonts.minor_cs = find_typeface(el, "cs");
                }
                _ => {}
            }
        }
        Ok(fonts)
    }

    /// Resolve a theme font reference to an actual font name.
    /// Returns None if the reference is unknown or the theme has no font for it.
    pub fn resolve(&self, theme_ref: &str) -> Option<&str> {
        match theme_ref {
            "majorHAnsi" | "majorAscii" => self.major_latin.as_deref(),
            "minorHAnsi" | "minorAscii" => self.minor_latin.as_deref(),
            "majorEastAsia" => self.major_east_asia.as_deref(),
            "minorEastAsia" => self.minor_east_asia.as_deref(),
            "majorBidi" => self.major_cs.as_deref(),
            "minorBidi" => self.minor_cs.as_deref(),
            _ => None,
        }
    }
}

/// Find a child element's `typeface` attribute by element name (e.g., "latin", "ea", "cs").
fn find_typeface(parent: &Element, child_name: &str) -> Option<String> {
    parent.children.iter().find_map(|c| match c {
        XMLNode::Element(el) if el.name == child_name => {
            let typeface = el
                .attributes
                .iter()
                .find(|(k, _)| k.local_name == "typeface")
                .map(|(_, v)| v)?;
            if typeface.is_empty() {
                None
            } else {
                Some(typeface.clone())
            }
        }
        _ => None,
    })
}

/// Opaque public handle to a document's resolved style table.
///
/// Needed to re-resolve a run's style-inherited marks when accepting/rejecting a
/// tracked paragraph-style change (`w:pPrChange`) OUTSIDE the runtime projection
/// — see [`crate::reject_all_with_styles`] /
/// [`crate::resolve_selected_revisions_with_styles`]. Obtain one from
/// [`crate::style_table_from_docx`] (or the runtime, which threads it in
/// automatically). The inner [`StyleDefinitions`] is intentionally not exposed:
/// callers only shuttle this token from parse to resolution.
#[derive(Clone, Debug)]
pub struct StyleTable(pub(crate) StyleDefinitions);

/// Pre-resolved style definitions from word/styles.xml.
///
/// Each style's TextMarks already incorporates basedOn chain inheritance and
/// document defaults, so callers only need a single lookup.
#[derive(Clone, Debug, Default)]
pub struct StyleDefinitions {
    /// Document-default run properties (from w:docDefaults/w:rPrDefault/w:rPr).
    pub doc_defaults: TextMarks,
    /// Document-default paragraph properties (from w:docDefaults/w:pPrDefault/w:pPr).
    pub(crate) ppr_defaults: RawParagraphProps,
    /// The default paragraph style ID (w:type="paragraph" w:default="1"), typically "Normal".
    /// Per ISO 29500-1 §17.7.4.17, unstyled paragraphs implicitly reference this style.
    default_para_style_id: Option<String>,
    /// The default character style ID (w:type="character" w:default="1").
    /// Per ISO 29500-1 §17.7.4.17, unstyled runs implicitly reference this style.
    default_char_style_id: Option<String>,
    /// Character styles: style_id → resolved TextMarks.
    char_styles: HashMap<String, TextMarks>,
    /// Paragraph styles' run properties: style_id → resolved TextMarks.
    para_styles: HashMap<String, TextMarks>,
    /// Paragraph-style chains whose highest authored Latin `w:rFonts` element
    /// contains literal ascii/hAnsi values and no Theme selector. Word treats
    /// that element as replacing a lower docDefaults Theme selector rather
    /// than attribute-merging the selector back into the style.
    para_literal_latin_over_doc_default_theme: HashSet<String>,
    /// Paragraph styles' resolved effective tab stops: style_id → `Vec<TabStopDef>`.
    /// Only non-"clear" stops, sorted ascending by position, de-duped.
    para_tab_stops: HashMap<String, Vec<TabStopDef>>,
    /// Paragraph styles' resolved paragraph properties (alignment, indent, spacing, borders).
    para_props: HashMap<String, RawParagraphProps>,
    /// Table styles: style_id → resolved table-level properties.
    table_styles: HashMap<String, TableStyleProps>,
    /// The default table style ID (`w:type="table" w:default="1"`), which an
    /// unstyled table uses (§17.7.4.17).
    default_table_style_id: Option<String>,
    /// Table styles whose chain (or a conditional part of it) declares a
    /// row's hidden state (`w:trPr/w:hidden`).
    table_styles_declaring_row_hidden: HashSet<String>,
    /// Table styles whose whole chain resolves, mapped to whether any style
    /// in it declares shading (`w:shd` in its `w:tblPr` or `w:tcPr`, also in
    /// a conditional `w:tblStylePr`). A style whose chain does not resolve
    /// (a missing or non-table `basedOn`, a cycle) is absent.
    table_style_chains_declaring_shading: HashMap<String, bool>,
    /// Table styles whose whole chain resolves, mapped to whether any style
    /// in it declares run properties (a `w:rPr`, also in a conditional
    /// `w:tblStylePr`). A style whose chain does not resolve is absent.
    table_style_chains_declaring_run_properties: HashMap<String, bool>,
    /// Theme font definitions for resolving asciiTheme/hAnsiTheme/etc. references.
    theme_fonts: ThemeFonts,
    /// Each style's own `w:rFonts`, as written, with its `basedOn` and
    /// whether it links a character style: the levels
    /// [`StyleDefinitions::effective_fonts`] reads.
    style_fonts: HashMap<String, StyleFonts>,
    /// The document defaults' `w:rFonts`, as written.
    doc_default_fonts: LevelFonts,
}

/// One style level's `w:rFonts` (§17.3.2.26) as written.
#[derive(Clone, Debug, Default)]
enum LevelFonts {
    /// The level names no font.
    #[default]
    Absent,
    Authored(crate::domain::AuthoredRFonts),
    /// The level's fonts are written in a form this model does not hold (an
    /// attribute outside the font attributes, or the element twice).
    Unmodelled,
}

#[derive(Clone, Debug)]
struct StyleFonts {
    fonts: LevelFonts,
    based_on: Option<String>,
    links_character_style: bool,
}

/// One script slot's font as the cascade selects it: a theme font, a named
/// font, or none named at any level.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FontSelection {
    Unset,
    Theme(IStr),
    Named(IStr),
}

/// A run's fonts per script slot (§17.3.2.26), and its font hint, as the
/// style cascade selects them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectiveFonts {
    pub ascii: FontSelection,
    pub h_ansi: FontSelection,
    pub east_asia: FontSelection,
    pub cs: FontSelection,
    pub hint: Option<IStr>,
}

/// The font attributes of one `w:rFonts` element (§17.3.2.26), or `None`
/// when it carries any other attribute.
pub(crate) fn authored_fonts_of(el: &Element) -> Option<crate::domain::AuthoredRFonts> {
    const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
    let mut fonts = crate::domain::AuthoredRFonts::default();
    for (name, value) in &el.attributes {
        if name.namespace.as_deref().is_some_and(|ns| ns != W_NS) {
            return None;
        }
        let value = Some(IStr::from(value.as_str()));
        let slot = match name.local_name.as_str() {
            "ascii" => &mut fonts.ascii,
            "hAnsi" => &mut fonts.h_ansi,
            "asciiTheme" => &mut fonts.ascii_theme,
            "hAnsiTheme" => &mut fonts.h_ansi_theme,
            "eastAsia" => &mut fonts.east_asia,
            "eastAsiaTheme" => &mut fonts.east_asia_theme,
            "cs" => &mut fonts.cs,
            "cstheme" => &mut fonts.cs_theme,
            "hint" => &mut fonts.hint,
            _ => return None,
        };
        *slot = value;
    }
    Some(fonts)
}

/// The `w:rFonts` of a `w:rPr` (or of none) as one style level.
fn level_fonts(rpr: Option<&Element>) -> LevelFonts {
    let Some(rpr) = rpr else {
        return LevelFonts::Absent;
    };
    let mut found = rpr.children.iter().filter_map(|child| match child {
        XMLNode::Element(el) if is_w_tag(el, "rFonts") => Some(el),
        _ => None,
    });
    match (found.next(), found.next()) {
        (None, _) => LevelFonts::Absent,
        (Some(el), None) => match authored_fonts_of(el) {
            Some(fonts) => LevelFonts::Authored(fonts),
            None => LevelFonts::Unmodelled,
        },
        (Some(_), Some(_)) => LevelFonts::Unmodelled,
    }
}

/// Resolved table style properties from a `<w:style w:type="table">` definition.
///
/// Contains the table-level formatting inherited through the basedOn chain.
/// Callers merge these as defaults underneath any direct formatting on the table element.
#[derive(Clone, Debug, Default)]
pub struct TableStyleProps {
    /// Table borders from w:tblPr/w:tblBorders in the style.
    pub borders: Option<BorderSet>,
    /// Default cell margins from w:tblPr/w:tblCellMar in the style.
    pub default_cell_margins: Option<CellMargins>,
    /// Default cell shading from w:tcPr/w:shd in the style's whole-table properties.
    pub default_cell_shading: Option<Shading>,
    /// Table alignment from w:tblPr/w:jc in the style (§17.4.28).
    pub alignment: Option<Alignment>,
    /// Table indent from w:tblPr/w:tblInd w:w in the style (§17.4.51), in twips.
    pub indent: Option<i32>,
    /// Conditional formatting overrides (tblStylePr) keyed by condition type.
    pub conditional: HashMap<TblStylePrType, ConditionalCellProps>,
    /// Row band size from w:tblPr/w:tblStyleRowBandSize (§17.4.79). MS defaults to 0 (no banding).
    pub row_band_size: u32,
    /// Column band size from w:tblPr/w:tblStyleColBandSize (§17.4.78). MS defaults to 0 (no banding).
    pub col_band_size: u32,
    /// Base paragraph alignment from the table style's root-level w:pPr/w:jc (§17.7.6).
    /// Used by the overrideTableStyleFontSizeAndJustification compat setting (MS-DOCX §2.3.1).
    pub base_para_alignment: Option<Alignment>,
    /// Base font size from the table style's root-level w:rPr/w:sz (§17.7.6), in half-points.
    /// Used by the overrideTableStyleFontSizeAndJustification compat setting (MS-DOCX §2.3.1).
    pub base_font_size: Option<u32>,
    /// Base bold from the table style's root-level w:rPr/w:b (§17.7.6).
    /// Always applied as a table-style default (not gated by compat setting).
    pub base_bold: Option<bool>,
    /// Base color from the table style's root-level w:rPr/w:color (§17.7.6).
    /// Always applied as a table-style default (not gated by compat setting).
    pub base_color: Option<IStr>,
    /// Base font family from the table style's root-level w:rPr/w:rFonts (§17.7.6).
    /// Always applied as a table-style default (not gated by compat setting).
    pub base_font_family: Option<IStr>,
}

/// Condition type for table conditional formatting (§17.7.6.1).
///
/// Each variant corresponds to a `w:type` attribute value on `w:tblStylePr`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TblStylePrType {
    FirstRow,
    LastRow,
    FirstCol,
    LastCol,
    Band1Horz,
    Band2Horz,
    Band1Vert,
    Band2Vert,
    WholeTable,
    /// Top-left corner cell (MS-OI29500 §17.4.54(a), §17.18.89).
    NwCell,
    /// Top-right corner cell.
    NeCell,
    /// Bottom-left corner cell.
    SwCell,
    /// Bottom-right corner cell.
    SeCell,
}

/// Cell-level properties that a conditional formatting override can set.
///
/// Per §17.7.6.1/§17.7.6.2, tblStylePr elements can also contain pPr and rPr
/// that apply to paragraphs and runs within matching table cells.
#[derive(Clone, Debug, Default)]
pub struct ConditionalCellProps {
    pub shading: Option<Shading>,
    pub borders: Option<BorderSet>,
    pub margins: Option<CellMargins>,
    /// Paragraph alignment from pPr/jc (§17.7.6.1).
    pub alignment: Option<Alignment>,
    /// Bold from rPr/b (§17.7.6.2).
    pub bold: Option<bool>,
    /// Font size from rPr/sz (§17.7.6.2), in half-points.
    pub font_size: Option<u32>,
    /// Font family from rPr/rFonts (§17.7.6.2).
    pub font_family: Option<IStr>,
    /// Color from rPr/color (§17.7.6.2).
    pub color: Option<IStr>,
}

/// Raw paragraph-level properties (alignment, indent, spacing, borders) parsed from a style definition.
/// Fields are individually optional: None means "inherit from parent".
#[derive(Clone, Debug, Default)]
pub(crate) struct RawParagraphProps {
    alignment: Option<Alignment>,
    indent_left: Option<i32>,
    indent_right: Option<i32>,
    indent_first_line: Option<i32>,
    /// Character-unit indents (MS-OI29500 2.1.44).
    indent_start_chars: Option<i32>,
    indent_end_chars: Option<i32>,
    indent_first_line_chars: Option<i32>,
    indent_hanging_chars: Option<i32>,
    spacing_before: Option<u32>,
    spacing_after: Option<u32>,
    spacing_before_lines: Option<u32>,
    spacing_after_lines: Option<u32>,
    spacing_before_autospacing: Option<bool>,
    spacing_after_autospacing: Option<bool>,
    spacing_line: Option<i32>,
    spacing_line_rule: Option<String>,
    /// Paragraph borders (whole-object replacement, not per-edge).
    borders: Option<ParagraphBorderProps>,
    /// Numbering properties from w:numPr in the style's pPr (§17.7.4.14).
    num_props: Option<DirectNumPr>,
    /// Raw layer: the `w:ilvl` the style's own `w:numPr` writes, whether or
    /// not it also names a list. Resolved (after `resolve_para_props_chain`):
    /// the level a paragraph whose direct `w:numPr` names a list but no level
    /// takes from this style (`omitted_level_along_chain`).
    num_ilvl: Option<u32>,
    /// Contextual spacing flag from w:contextualSpacing (§17.3.1.9).
    /// None = not specified (inherit from parent), Some(true/false) = explicit.
    contextual_spacing: Option<bool>,
    /// Widow/orphan control from w:widowControl (§17.3.1.44).
    /// None = not specified (inherit from parent), Some(true/false) = explicit.
    widow_control: Option<bool>,
    /// Keep paragraph with next from w:keepNext (§17.3.1.15).
    /// None = not specified (inherit from parent), Some(true/false) = explicit.
    keep_next: Option<bool>,
    /// Keep all lines on same page from w:keepLines (§17.3.1.14).
    /// None = not specified (inherit from parent), Some(true/false) = explicit.
    keep_lines: Option<bool>,
    /// Page break before paragraph from w:pageBreakBefore (§17.3.1.23).
    /// None = not specified (inherit from parent), Some(true/false) = explicit.
    pub page_break_before: Option<bool>,
    /// Outline level from w:outlineLvl (§17.3.1.20).
    /// None = not specified (inherit from parent), Some(0–8) = explicit.
    outline_lvl: Option<u8>,
    /// Paragraph shading from w:pPr/w:shd (§17.3.1.31).
    pub shading: Option<Shading>,
}

/// Raw (unresolved) style entry parsed from the XML.
struct RawStyle {
    style_type: String,
    marks: TextMarks,
    based_on: Option<String>,
    /// ISO 29500-1 §17.7.4.6: w:link creates a bidirectional link between
    /// a paragraph style and a character style. When present on a paragraph
    /// style, runs should inherit rPr from the linked character style.
    link: Option<String>,
    /// Raw tab stops from w:pPr/w:tabs. None = not specified (inherit from parent).
    /// Some = explicitly set (may include "clear" entries).
    raw_tab_stops: Option<Vec<TabStopDef>>,
    /// Raw paragraph properties from w:pPr (alignment, indent).
    raw_para_props: RawParagraphProps,
    /// Raw table style properties from w:tblPr + w:tcPr (table styles only).
    raw_table_props: RawTableStyleProps,
}

/// Raw (unresolved) table style properties parsed from a single style definition.
/// Fields are individually optional: None means "inherit from parent".
#[derive(Clone, Debug, Default)]
struct RawTableStyleProps {
    borders: Option<BorderSet>,
    default_cell_margins: Option<CellMargins>,
    default_cell_shading: Option<Shading>,
    alignment: Option<Alignment>,
    indent: Option<i32>,
    conditional: HashMap<TblStylePrType, ConditionalCellProps>,
    row_band_size: u32,
    col_band_size: u32,
    /// Base paragraph alignment from the table style's root-level w:pPr/w:jc.
    base_para_alignment: Option<Alignment>,
    /// Whether the style's w:trPr, or a w:tblStylePr's, holds w:hidden.
    declares_row_hidden: bool,
    /// Whether the style's w:tblPr or w:tcPr, or a w:tblStylePr's, holds
    /// w:shd.
    declares_shading: bool,
    /// Whether the style, or a w:tblStylePr of it, holds a w:rPr.
    declares_run_properties: bool,
    /// Base font size from the table style's root-level w:rPr/w:sz, in half-points.
    base_font_size: Option<u32>,
    /// Base bold from the table style's root-level w:rPr/w:b.
    base_bold: Option<bool>,
    /// Base color from the table style's root-level w:rPr/w:color.
    base_color: Option<IStr>,
    /// Base font family from the table style's root-level w:rPr/w:rFonts.
    base_font_family: Option<IStr>,
}

impl StyleDefinitions {
    /// Parse `word/styles.xml` bytes into resolved style definitions.
    pub fn parse(xml_bytes: &[u8]) -> Result<Self, String> {
        if xml_bytes.is_empty() {
            return Err("word/styles.xml is empty".to_string());
        }
        let root = crate::word_xml::parse_document_xml(xml_bytes)
            .map_err(|err| format!("failed to parse word/styles.xml: {err:?}"))?;

        // 1. Parse document defaults
        let doc_defaults = parse_doc_defaults(&root)?;
        let ppr_defaults = parse_ppr_defaults(&root)?;

        // 2. Parse all raw style entries
        let mut raw_styles: HashMap<String, RawStyle> = HashMap::new();
        let mut default_para_style_id: Option<String> = None;
        let mut default_char_style_id: Option<String> = None;
        let mut default_table_style_id: Option<String> = None;
        let mut style_fonts: HashMap<String, StyleFonts> = HashMap::new();
        for child in &root.children {
            let el = match child {
                XMLNode::Element(el) => el,
                _ => continue,
            };
            if !is_w_tag(el, "style") {
                continue;
            }
            let style_id = match attr_get(el, "w:styleId") {
                Some(id) => id.clone(),
                None => continue,
            };
            // ISO 29500-1 §17.7.4.17: "If this attribute is omitted, then the
            // default value shall be assumed to be paragraph." Defaulting to ""
            // meant a type-omitted w:default="1" style was never registered as the
            // default paragraph style (confirmed against real Word: Word applies it).
            let style_type = attr_get(el, "w:type")
                .cloned()
                .unwrap_or_else(|| "paragraph".to_string());

            // ISO 29500-1 §17.7.4.17: identify default styles.
            let is_default = matches!(
                attr_get(el, "w:default").map(|v| v.as_str()),
                Some("1") | Some("true")
            );
            if is_default && style_type == "paragraph" {
                default_para_style_id = Some(style_id.clone());
            }
            if is_default && style_type == "character" {
                default_char_style_id = Some(style_id.clone());
            }
            if is_default && style_type == "table" {
                default_table_style_id = Some(style_id.clone());
            }
            let based_on =
                find_w_child(el, "basedOn").and_then(|el| attr_get(el, "w:val").cloned());

            // ISO 29500-1 §17.7.4.6: w:link val — bidirectional link to
            // a character style (on paragraph styles) or paragraph style
            // (on character styles).
            let link = find_w_child(el, "link").and_then(|el| attr_get(el, "w:val").cloned());

            // MS-OI29500 §17.7.4.17a — Word ignores child elements
            // of styles with IDs "DefaultParagraphFont", "NoList", or "TableNormal".
            // Store empty/default properties so inheriting from these gives nothing.
            let is_special_ignored = matches!(
                style_id.as_str(),
                "DefaultParagraphFont" | "NoList" | "TableNormal"
            );

            // Extract run properties from the style.
            // For paragraph styles: w:style/w:rPr
            // For character styles: w:style/w:rPr
            let marks = if is_special_ignored {
                TextMarks::default()
            } else {
                match find_w_child(el, "rPr") {
                    Some(rpr) => parse_rpr_marks(rpr)
                        .map_err(|err| format!("word/styles.xml style '{style_id}': {err}"))?,
                    None => TextMarks::default(),
                }
            };

            style_fonts.insert(
                style_id.clone(),
                StyleFonts {
                    fonts: if is_special_ignored {
                        LevelFonts::Absent
                    } else {
                        level_fonts(find_w_child(el, "rPr"))
                    },
                    based_on: based_on.clone(),
                    links_character_style: style_type == "paragraph" && link.is_some(),
                },
            );

            // Extract paragraph-level properties from w:pPr.
            let (raw_tab_stops, raw_para_props) = if is_special_ignored {
                (None, RawParagraphProps::default())
            } else {
                let ppr = find_w_child(el, "pPr");
                let tabs = ppr
                    .map(parse_ppr_tab_stops)
                    .transpose()
                    .map_err(|err| format!("word/styles.xml style {style_id:?}: {err}"))?
                    .flatten();
                let props = ppr
                    .map(extract_raw_para_props)
                    .transpose()
                    .map_err(|err| format!("word/styles.xml style {style_id:?}: {err}"))?
                    .unwrap_or_default();
                (tabs, props)
            };

            // Extract table-level properties from w:tblPr and w:tcPr (table styles only).
            let raw_table_props = if is_special_ignored {
                RawTableStyleProps::default()
            } else if style_type == "table" {
                extract_raw_table_style_props(el)
                    .map_err(|err| format!("word/styles.xml style '{style_id}': {err}"))?
            } else {
                RawTableStyleProps::default()
            };

            raw_styles.insert(
                style_id,
                RawStyle {
                    style_type,
                    marks,
                    based_on,
                    link,
                    raw_tab_stops,
                    raw_para_props,
                    raw_table_props,
                },
            );
        }

        // 3. Resolve basedOn chains and fold in document defaults.
        let mut char_styles = HashMap::new();
        let mut para_styles = HashMap::new();
        let mut para_literal_latin_over_doc_default_theme = HashSet::new();
        let mut para_tab_stops = HashMap::new();
        let mut para_props = HashMap::new();
        let mut table_styles = HashMap::new();

        // Collect style IDs first to avoid borrow issues.
        let style_ids: Vec<String> = raw_styles.keys().cloned().collect();

        // Pass 1: Resolve character styles first (needed for w:link lookups).
        for id in &style_ids {
            let raw = &raw_styles[id];
            if raw.style_type == "character" {
                let resolved = resolve_chain(id, &raw_styles);
                char_styles.insert(id.clone(), resolved);
            }
        }

        // Pass 2: Resolve paragraph styles.
        // ISO 29500-1 §17.7.4.6: If a paragraph style has w:link pointing
        // to a character style, the paragraph's runs inherit the linked
        // character style's rPr (not the paragraph style's own rPr).
        for id in &style_ids {
            let raw = &raw_styles[id];
            if raw.style_type != "paragraph" {
                continue;
            }

            let resolved = match raw.link.as_deref() {
                Some(link_id) => {
                    match (raw_styles.get(link_id), char_styles.get(link_id)) {
                        (Some(linked_raw), Some(char_resolved)) => {
                            // ISO 29500-1 §17.7.4.6: Three-way merge —
                            // char style's explicit properties always win,
                            // para style's explicit properties win over
                            // char's merely-inherited properties, and
                            // char's inherited properties fill gaps where
                            // para doesn't set a value.
                            let mut base = resolve_chain(id, &raw_styles);
                            overlay_linked_char(
                                &mut base,
                                char_resolved,
                                &raw.marks,
                                &linked_raw.marks,
                            );
                            base
                        }
                        _ => resolve_chain(id, &raw_styles),
                    }
                }
                None => resolve_chain(id, &raw_styles),
            };
            para_styles.insert(id.clone(), resolved);
            if raw.link.is_none()
                && doc_defaults.font_family_theme.is_some()
                && highest_paragraph_style_latin_rfonts(id, &raw_styles)
                    == Some(LatinRFontsDisposition::Literal)
            {
                para_literal_latin_over_doc_default_theme.insert(id.clone());
            }

            let resolved_tabs = resolve_tab_stop_chain(id, &raw_styles);
            if !resolved_tabs.is_empty() {
                para_tab_stops.insert(id.clone(), resolved_tabs);
            }
            let resolved_pprops = resolve_para_props_chain(id, &raw_styles);
            if resolved_pprops.alignment.is_some()
                || resolved_pprops.indent_left.is_some()
                || resolved_pprops.indent_right.is_some()
                || resolved_pprops.indent_first_line.is_some()
                || resolved_pprops.indent_start_chars.is_some()
                || resolved_pprops.indent_end_chars.is_some()
                || resolved_pprops.indent_first_line_chars.is_some()
                || resolved_pprops.indent_hanging_chars.is_some()
                || resolved_pprops.spacing_before.is_some()
                || resolved_pprops.spacing_after.is_some()
                || resolved_pprops.spacing_before_lines.is_some()
                || resolved_pprops.spacing_after_lines.is_some()
                || resolved_pprops.spacing_before_autospacing.is_some()
                || resolved_pprops.spacing_after_autospacing.is_some()
                || resolved_pprops.spacing_line.is_some()
                || resolved_pprops.borders.is_some()
                || resolved_pprops.num_props.is_some()
                || resolved_pprops.num_ilvl.is_some()
                || resolved_pprops.contextual_spacing.is_some()
                || resolved_pprops.widow_control.is_some()
                || resolved_pprops.keep_next.is_some()
                || resolved_pprops.keep_lines.is_some()
                || resolved_pprops.page_break_before.is_some()
                || resolved_pprops.outline_lvl.is_some()
                || resolved_pprops.shading.is_some()
            {
                para_props.insert(id.clone(), resolved_pprops);
            }
        }

        // Pass 3: Resolve table styles.
        let mut table_styles_declaring_row_hidden = HashSet::new();
        let mut table_style_chains_declaring_shading = HashMap::new();
        let mut table_style_chains_declaring_run_properties = HashMap::new();
        for id in &style_ids {
            let raw = &raw_styles[id];
            if raw.style_type == "table" {
                if table_style_chain_declares_row_hidden(id, &raw_styles) {
                    table_styles_declaring_row_hidden.insert(id.clone());
                }
                if let Some(declares) =
                    table_style_chain_declares(id, &raw_styles, |p| p.declares_shading)
                {
                    table_style_chains_declaring_shading.insert(id.clone(), declares);
                }
                if let Some(declares) =
                    table_style_chain_declares(id, &raw_styles, |p| p.declares_run_properties)
                {
                    table_style_chains_declaring_run_properties.insert(id.clone(), declares);
                }
                let resolved_tprops = resolve_table_style_chain(id, &raw_styles);
                if resolved_tprops.borders.is_some()
                    || resolved_tprops.default_cell_margins.is_some()
                    || resolved_tprops.default_cell_shading.is_some()
                    || !resolved_tprops.conditional.is_empty()
                    || resolved_tprops.base_font_family.is_some()
                    || resolved_tprops.base_font_size.is_some()
                    || resolved_tprops.base_bold.is_some()
                    || resolved_tprops.base_color.is_some()
                    || resolved_tprops.base_para_alignment.is_some()
                {
                    table_styles.insert(id.clone(), resolved_tprops);
                }
            }
        }

        Ok(StyleDefinitions {
            doc_defaults,
            ppr_defaults,
            default_para_style_id,
            default_char_style_id,
            char_styles,
            para_styles,
            para_literal_latin_over_doc_default_theme,
            para_tab_stops,
            para_props,
            table_styles,
            default_table_style_id,
            table_styles_declaring_row_hidden,
            table_style_chains_declaring_shading,
            table_style_chains_declaring_run_properties,
            theme_fonts: ThemeFonts::default(),
            style_fonts,
            doc_default_fonts: level_fonts(
                find_w_child(&root, "docDefaults")
                    .and_then(|el| find_w_child(el, "rPrDefault"))
                    .and_then(|el| find_w_child(el, "rPr")),
            ),
        })
    }

    /// Push style `id`'s fonts and its `basedOn` ancestors', nearest first;
    /// `None` for a missing style, a loop, or fonts the model does not hold.
    fn push_style_fonts<'a>(
        &'a self,
        id: &str,
        levels: &mut Vec<&'a crate::domain::AuthoredRFonts>,
    ) -> Option<()> {
        let mut seen = HashSet::new();
        let mut next = Some(id);
        while let Some(id) = next {
            if !seen.insert(id) {
                return None;
            }
            let style = self.style_fonts.get(id)?;
            match &style.fonts {
                LevelFonts::Absent => {}
                LevelFonts::Authored(fonts) => levels.push(fonts),
                LevelFonts::Unmodelled => return None,
            }
            next = style.based_on.as_deref();
        }
        Some(())
    }

    /// A run's fonts per script slot as the cascade selects them (§17.7.2:
    /// direct, character style, paragraph style, document defaults), or
    /// `None` where this model cannot prove the selection.
    ///
    /// For each slot the nearest level that names its font decides it: its
    /// theme selector, which within one `w:rFonts` takes precedence over
    /// the literal name (§17.3.2.26), else the literal. A literal over a
    /// lower level's theme selector for the same slot is not decided here
    /// (Word's precedence across levels is witnessed for some shapes only),
    /// nor is a level written in a form the model does not hold, a style
    /// that is missing or whose `basedOn` chain loops, or a paragraph style
    /// linked to a character style (§17.7.4.6). The hint is the nearest
    /// level's.
    pub fn effective_fonts(
        &self,
        direct: Option<&crate::domain::AuthoredRFonts>,
        char_style_id: Option<&str>,
        para_style_id: Option<&str>,
    ) -> Option<EffectiveFonts> {
        let mut levels: Vec<&crate::domain::AuthoredRFonts> = direct.into_iter().collect();
        let push_chain = |id, levels: &mut Vec<_>| self.push_style_fonts(id, levels);
        let default_char = self.default_char_style_id.as_deref();
        match char_style_id {
            Some(id) if Some(id) != default_char => {
                push_chain(id, &mut levels)?;
                if let Some(default) = default_char {
                    push_chain(default, &mut levels)?;
                }
            }
            Some(id) => push_chain(id, &mut levels)?,
            None => {
                if let Some(default) = default_char {
                    push_chain(default, &mut levels)?;
                }
            }
        }
        if let Some(id) = para_style_id.or(self.default_para_style_id.as_deref()) {
            if self.style_fonts.get(id)?.links_character_style {
                return None;
            }
            push_chain(id, &mut levels)?;
        }
        match &self.doc_default_fonts {
            LevelFonts::Absent => {}
            LevelFonts::Authored(fonts) => levels.push(fonts),
            LevelFonts::Unmodelled => return None,
        }
        type Slot = fn(&crate::domain::AuthoredRFonts) -> (&Option<IStr>, &Option<IStr>);
        let select = |slot: Slot| -> Option<FontSelection> {
            let Some(at) = levels.iter().position(|level| {
                let (named, theme) = slot(level);
                named.is_some() || theme.is_some()
            }) else {
                return Some(FontSelection::Unset);
            };
            match slot(levels[at]) {
                (_, Some(theme)) => Some(FontSelection::Theme(theme.clone())),
                (Some(named), None) => {
                    let theme_below = levels[at + 1..].iter().any(|level| slot(level).1.is_some());
                    (!theme_below).then(|| FontSelection::Named(named.clone()))
                }
                (None, None) => unreachable!("the deciding level names the slot"),
            }
        };
        Some(EffectiveFonts {
            ascii: select(|f| (&f.ascii, &f.ascii_theme))?,
            h_ansi: select(|f| (&f.h_ansi, &f.h_ansi_theme))?,
            east_asia: select(|f| (&f.east_asia, &f.east_asia_theme))?,
            cs: select(|f| (&f.cs, &f.cs_theme))?,
            hint: levels.iter().find_map(|level| level.hint.clone()),
        })
    }

    /// The default table style ID (`w:type="table" w:default="1"`), if any.
    pub fn default_table_style_id(&self) -> Option<&str> {
        self.default_table_style_id.as_deref()
    }

    /// Whether table style `style_id`'s chain declares a row's hidden state
    /// (`w:trPr/w:hidden`, also in a conditional `w:tblStylePr`). Word
    /// ignores the children of `TableNormal` (MS-OI29500 §17.7.4.17a), so
    /// it never declares one.
    pub fn table_style_declares_row_hidden(&self, style_id: &str) -> bool {
        self.table_styles_declaring_row_hidden.contains(style_id)
    }

    /// Whether table style `style_id`'s chain declares shading (`w:shd` in
    /// a style's `w:tblPr` or `w:tcPr`, also in a conditional
    /// `w:tblStylePr`): `None` when the chain does not resolve (an unknown
    /// style, a missing or non-table `basedOn`, a cycle).
    pub fn table_style_declares_shading(&self, style_id: &str) -> Option<bool> {
        self.table_style_chains_declaring_shading
            .get(style_id)
            .copied()
    }

    /// Whether table style `style_id`'s chain declares run properties (a
    /// `w:rPr` in a style, also in a conditional `w:tblStylePr`): `None`
    /// when the chain does not resolve.
    pub fn table_style_declares_run_properties(&self, style_id: &str) -> Option<bool> {
        self.table_style_chains_declaring_run_properties
            .get(style_id)
            .copied()
    }

    /// The default paragraph style ID (w:type="paragraph" w:default="1").
    /// Per ISO 29500-1 §17.7.4.17, unstyled paragraphs implicitly reference this style.
    pub fn default_para_style_id(&self) -> Option<&str> {
        self.default_para_style_id.as_deref()
    }

    /// The default character style ID (w:type="character" w:default="1").
    /// Per ISO 29500-1 §17.7.4.17, unstyled runs implicitly reference this style.
    pub fn default_char_style_id(&self) -> Option<&str> {
        self.default_char_style_id.as_deref()
    }

    /// Set theme font definitions for resolving asciiTheme/hAnsiTheme/etc. references.
    /// Should be called after parsing if theme1.xml is available in the DOCX package.
    pub fn set_theme_fonts(&mut self, theme_fonts: ThemeFonts) {
        self.theme_fonts = theme_fonts;
    }

    /// Resolve effective tab stops for a paragraph, merging direct paragraph
    /// tabs with the style hierarchy's resolved tabs.
    ///
    /// Returns the final list of non-"clear" tab stops, sorted ascending by position.
    pub fn resolve_effective_tabs(
        &self,
        style_id: Option<&str>,
        direct_tabs: Option<&[TabStopDef]>,
    ) -> Vec<TabStopDef> {
        // Start with style-resolved tabs.
        let style_tabs = style_id.and_then(|id| self.para_tab_stops.get(id));
        let mut effective: Vec<TabStopDef> = style_tabs.cloned().unwrap_or_default();

        // Overlay direct tabs (if explicitly specified on the paragraph).
        if let Some(direct) = direct_tabs {
            overlay_tab_stops(&mut effective, direct);
        }

        effective
    }

    /// Resolve effective indentation for a paragraph.
    ///
    /// Per ECMA-376 §17.3.1.12: indentation attributes are overridden on an
    /// individual (per-attribute) basis through the cascade:
    /// direct > numbering > style.  When a direct `w:ind` element is present,
    /// each specified attribute wins; unspecified attributes (None) fall through
    /// to numbering, then to the style chain.
    ///
    /// When no direct `w:ind` is present, numbering-level indent (§17.9.22)
    /// takes precedence over the paragraph style chain.
    pub fn resolve_effective_indent(
        &self,
        style_id: Option<&str>,
        direct: Option<&IndentProps>,
        numbering_indent: Option<&LevelIndent>,
    ) -> Option<IndentProps> {
        let style_props = style_id.and_then(|id| self.para_props.get(id));
        let defaults = &self.ppr_defaults;

        // ECMA-376 §17.3.1.12: "Indentation settings are overridden on an
        // individual basis" — per-attribute merge through the style chain.
        // When direct w:ind specifies firstLine/hanging (even as "0"), that
        // value wins.  When it omits firstLine/hanging entirely (None), we
        // fall through to numbering, then to the style chain.
        // Left/right follow the same per-attribute cascade.
        if let Some(d) = direct {
            let has_twip =
                d.left.is_some() || d.right.is_some() || d.effective_first_line_twips.is_some();
            let has_char = d.start_chars.is_some()
                || d.end_chars.is_some()
                || d.first_line_chars.is_some()
                || d.hanging_chars.is_some();
            if has_twip || has_char {
                // Left/right: direct > numbering > style > defaults (per-attribute).
                let left = d
                    .left
                    .or_else(|| numbering_indent.and_then(|n| n.left))
                    .or_else(|| style_props.and_then(|s| s.indent_left))
                    .or(defaults.indent_left);
                let right = d
                    .right
                    .or_else(|| numbering_indent.and_then(|n| n.right))
                    .or_else(|| style_props.and_then(|s| s.indent_right))
                    .or(defaults.indent_right);
                // firstLine/hanging: per-attribute cascade — direct > numbering > style > defaults.
                // Explicit Some(0) from direct w:ind wins (no fall-through).
                // None (absent firstLine/hanging) falls through to numbering, then style, then defaults.
                let first_line = d
                    .effective_first_line_twips
                    .or_else(|| numbering_indent.and_then(|n| n.effective_first_line_twips))
                    .or_else(|| style_props.and_then(|s| s.indent_first_line))
                    .or(defaults.indent_first_line);
                // MS-OI29500 2.1.44(b): non-zero char-unit values from style
                // override the twip values from direct formatting.
                let start_chars = d
                    .start_chars
                    .or_else(|| style_props.and_then(|s| s.indent_start_chars))
                    .or(defaults.indent_start_chars);
                let end_chars = d
                    .end_chars
                    .or_else(|| style_props.and_then(|s| s.indent_end_chars))
                    .or(defaults.indent_end_chars);
                let first_line_chars = d
                    .first_line_chars
                    .or_else(|| style_props.and_then(|s| s.indent_first_line_chars))
                    .or(defaults.indent_first_line_chars);
                let hanging_chars = d
                    .hanging_chars
                    .or_else(|| style_props.and_then(|s| s.indent_hanging_chars))
                    .or(defaults.indent_hanging_chars);
                return Some(IndentProps {
                    left,
                    right,
                    effective_first_line_twips: first_line,
                    start_chars,
                    end_chars,
                    first_line_chars,
                    hanging_chars,
                });
            }
            // Direct w:ind element was present but had no attributes —
            // all values default to 0, which means "no indentation".
            return None;
        }

        // No direct w:ind — check numbering level.
        // When numbering provides w:ind with any field set, it replaces
        // the style's w:ind as a whole element (element-level override), matching
        // how direct formatting already works (line 486). Per-field merge would
        // incorrectly leak style firstLine into the numbering indent.
        // MS-OI29500 2.1.44: character-unit indents from style can still apply.
        if let Some(num) = numbering_indent {
            let has_any = num.left.is_some()
                || num.right.is_some()
                || num.effective_first_line_twips.is_some();
            if has_any {
                let start_chars = style_props
                    .and_then(|s| s.indent_start_chars)
                    .or(defaults.indent_start_chars);
                let end_chars = style_props
                    .and_then(|s| s.indent_end_chars)
                    .or(defaults.indent_end_chars);
                let first_line_chars = style_props
                    .and_then(|s| s.indent_first_line_chars)
                    .or(defaults.indent_first_line_chars);
                let hanging_chars = style_props
                    .and_then(|s| s.indent_hanging_chars)
                    .or(defaults.indent_hanging_chars);
                return Some(IndentProps {
                    left: num.left,
                    right: num.right,
                    effective_first_line_twips: num.effective_first_line_twips,
                    start_chars,
                    end_chars,
                    first_line_chars,
                    hanging_chars,
                });
            }
        }

        // Fall through to style-only resolution.
        let left = style_props
            .and_then(|s| s.indent_left)
            .or(defaults.indent_left);
        let right = style_props
            .and_then(|s| s.indent_right)
            .or(defaults.indent_right);
        let first_line = style_props
            .and_then(|s| s.indent_first_line)
            .or(defaults.indent_first_line);
        let start_chars = style_props
            .and_then(|s| s.indent_start_chars)
            .or(defaults.indent_start_chars);
        let end_chars = style_props
            .and_then(|s| s.indent_end_chars)
            .or(defaults.indent_end_chars);
        let first_line_chars = style_props
            .and_then(|s| s.indent_first_line_chars)
            .or(defaults.indent_first_line_chars);
        let hanging_chars = style_props
            .and_then(|s| s.indent_hanging_chars)
            .or(defaults.indent_hanging_chars);

        if left.is_some()
            || right.is_some()
            || first_line.is_some()
            || start_chars.is_some()
            || end_chars.is_some()
            || first_line_chars.is_some()
            || hanging_chars.is_some()
        {
            Some(IndentProps {
                left,
                right,
                effective_first_line_twips: first_line,
                start_chars,
                end_chars,
                first_line_chars,
                hanging_chars,
            })
        } else {
            None
        }
    }

    /// Resolve effective spacing for a paragraph.
    ///
    /// Per ECMA-376 §17.3.1.33: when a paragraph has a direct `w:spacing`
    /// element, it replaces the style's `w:spacing` entirely.  Missing
    /// attributes on the direct element stay None — they do NOT fall through
    /// to the style chain or document defaults.
    ///
    /// When no direct `w:spacing` is present, the style chain and document
    /// defaults are merged per-field.
    pub fn resolve_effective_spacing(
        &self,
        style_id: Option<&str>,
        direct: Option<&SpacingProps>,
    ) -> Option<SpacingProps> {
        // §17.3.1.33: Per-attribute inheritance — each spacing attribute inherits
        // independently from the style hierarchy when omitted in direct formatting.
        // This matches §17.3.1.12 (ind) which explicitly says "overriden on an
        // individual basis". Word produces partial w:spacing in 99.7% of cases.
        let style_props = style_id.and_then(|id| self.para_props.get(id));
        let defaults = &self.ppr_defaults;

        // Resolve each attribute: direct > style > docDefaults
        let before = direct
            .and_then(|d| d.before)
            .or_else(|| style_props.and_then(|s| s.spacing_before))
            .or(defaults.spacing_before);
        let after = direct
            .and_then(|d| d.after)
            .or_else(|| style_props.and_then(|s| s.spacing_after))
            .or(defaults.spacing_after);
        let before_lines = direct
            .and_then(|d| d.before_lines)
            .or_else(|| style_props.and_then(|s| s.spacing_before_lines))
            .or(defaults.spacing_before_lines);
        let after_lines = direct
            .and_then(|d| d.after_lines)
            .or_else(|| style_props.and_then(|s| s.spacing_after_lines))
            .or(defaults.spacing_after_lines);
        let before_autospacing = direct
            .and_then(|d| d.before_autospacing)
            .or_else(|| style_props.and_then(|s| s.spacing_before_autospacing))
            .or(defaults.spacing_before_autospacing);
        let after_autospacing = direct
            .and_then(|d| d.after_autospacing)
            .or_else(|| style_props.and_then(|s| s.spacing_after_autospacing))
            .or(defaults.spacing_after_autospacing);
        let line = direct
            .and_then(|d| d.line)
            .or_else(|| style_props.and_then(|s| s.spacing_line))
            .or(defaults.spacing_line);
        let line_rule = direct
            .and_then(|d| d.line_rule.clone())
            .or_else(|| style_props.and_then(|s| s.spacing_line_rule.clone()))
            .or_else(|| defaults.spacing_line_rule.clone());

        // §17.3.1.33: "If [lineRule] is omitted, then it shall be assumed
        // to be of a value auto if a line attribute value is present."
        // Special rule: when direct sets line but omits lineRule, default to
        // auto rather than inheriting the style's lineRule.
        let line_rule = if direct.is_some_and(|d| d.line.is_some() && d.line_rule.is_none())
            || (line.is_some() && line_rule.is_none())
        {
            Some("auto".to_string())
        } else {
            line_rule
        };

        if before.is_some()
            || after.is_some()
            || before_lines.is_some()
            || after_lines.is_some()
            || before_autospacing.is_some()
            || after_autospacing.is_some()
            || line.is_some()
        {
            Some(SpacingProps {
                before,
                after,
                before_lines,
                after_lines,
                before_autospacing,
                after_autospacing,
                line,
                line_rule,
            })
        } else {
            None
        }
    }

    /// Resolve effective paragraph borders, merging direct with style chain.
    /// Whole-object replacement: direct wins if present, else style-resolved value.
    pub fn resolve_effective_borders(
        &self,
        style_id: Option<&str>,
        direct: Option<&ParagraphBorderProps>,
    ) -> Option<ParagraphBorderProps> {
        if let Some(d) = direct {
            return Some(d.clone());
        }
        style_id
            .and_then(|id| self.para_props.get(id))
            .and_then(|p| p.borders.clone())
            .or_else(|| self.ppr_defaults.borders.clone())
    }

    /// Resolve effective paragraph shading (§17.3.1.31).
    /// Direct wins if present, else style-resolved value from basedOn chain.
    pub fn resolve_effective_para_shading(
        &self,
        style_id: Option<&str>,
        direct: Option<&Shading>,
    ) -> Option<Shading> {
        if let Some(d) = direct {
            return Some(d.clone());
        }
        style_id
            .and_then(|id| self.para_props.get(id))
            .and_then(|p| p.shading.clone())
    }

    /// Resolve effective alignment for a paragraph, merging direct with style chain.
    /// Direct wins if present, else style-resolved value.
    pub fn resolve_effective_alignment(
        &self,
        style_id: Option<&str>,
        direct: Option<&Alignment>,
    ) -> Option<Alignment> {
        if let Some(a) = direct {
            return Some(a.clone());
        }
        style_id
            .and_then(|id| self.para_props.get(id))
            .and_then(|p| p.alignment.clone())
            .or_else(|| self.ppr_defaults.alignment.clone())
    }

    /// Resolve effective numbering properties for a paragraph (§17.7.4.14).
    ///
    /// - `DirectNumPr::Active` → direct wins, return the active props.
    /// - `DirectNumPr::Suppressed` → numId=0, explicitly no numbering (§17.9.18).
    /// - `DirectNumPr::Absent` → fall through to style-resolved numPr.
    /// - `DirectNumPr::ListWithoutLevel` → the direct list at the level
    ///   [`resolve_omitted_direct_level`] gives.
    pub fn resolve_effective_num_props(
        &self,
        style_id: Option<&str>,
        direct: &DirectNumPr,
    ) -> DirectNumPr {
        match direct {
            DirectNumPr::Active(_) | DirectNumPr::Suppressed => return direct.clone(),
            DirectNumPr::ListWithoutLevel { num_id } => {
                return DirectNumPr::Active(NumProps {
                    num_id: *num_id,
                    ilvl: resolve_omitted_direct_level(Some(self), style_id),
                });
            }
            DirectNumPr::Absent => {}
        }
        style_id
            .and_then(|id| self.para_props.get(id))
            .and_then(|p| p.num_props.clone())
            .or_else(|| self.ppr_defaults.num_props.clone())
            .unwrap_or(DirectNumPr::Absent)
    }

    /// Resolve effective contextual spacing for a paragraph (§17.3.1.9).
    /// Direct wins if present (Some), else style-resolved value, else None.
    pub fn resolve_effective_contextual_spacing(
        &self,
        style_id: Option<&str>,
        direct: Option<bool>,
    ) -> Option<bool> {
        if direct.is_some() {
            return direct;
        }
        style_id
            .and_then(|id| self.para_props.get(id))
            .and_then(|p| p.contextual_spacing)
            .or(self.ppr_defaults.contextual_spacing)
    }

    /// Resolve effective widowControl for a paragraph (§17.3.1.44).
    /// Direct wins if present, else style-resolved value, else None (spec default true).
    pub fn resolve_effective_widow_control(
        &self,
        style_id: Option<&str>,
        direct: Option<bool>,
    ) -> Option<bool> {
        if direct.is_some() {
            return direct;
        }
        // §17.3.1.44: the spec default is ON — widow control's is the one
        // pPr toggle whose absence means TRUE. The EFFECTIVE value is
        // therefore never "inherit nothing": an explicit `w:widowControl`
        // and an absent one with no chain contribution are the SAME state,
        // and Word freely rewrites one spelling into the other on resave.
        // Leaving the effective field None for the absent spelling gave that
        // one state two representations, and every comparison across a Word
        // save tripped on it (wave-8 real-word v6 reject-endpoint family).
        // `has_direct_widow_control` still gates wire emission.
        style_id
            .and_then(|id| self.para_props.get(id))
            .and_then(|p| p.widow_control)
            .or(self.ppr_defaults.widow_control)
            .or(Some(true))
    }

    /// Resolve effective keepNext for a paragraph (§17.3.1.14).
    /// Direct wins if present, else style-resolved value.
    pub fn resolve_effective_keep_next(
        &self,
        style_id: Option<&str>,
        direct: Option<bool>,
    ) -> Option<bool> {
        if direct.is_some() {
            return direct;
        }
        style_id
            .and_then(|id| self.para_props.get(id))
            .and_then(|p| p.keep_next)
            .or(self.ppr_defaults.keep_next)
    }

    /// Resolve effective keepLines for a paragraph (§17.3.1.15).
    /// Direct wins if present, else style-resolved value.
    pub fn resolve_effective_keep_lines(
        &self,
        style_id: Option<&str>,
        direct: Option<bool>,
    ) -> Option<bool> {
        if direct.is_some() {
            return direct;
        }
        style_id
            .and_then(|id| self.para_props.get(id))
            .and_then(|p| p.keep_lines)
            .or(self.ppr_defaults.keep_lines)
    }

    /// Resolve effective pageBreakBefore for a paragraph (§17.3.1.23).
    pub fn resolve_effective_page_break_before(
        &self,
        style_id: Option<&str>,
        direct: Option<bool>,
    ) -> Option<bool> {
        if direct.is_some() {
            return direct;
        }
        style_id
            .and_then(|id| self.para_props.get(id))
            .and_then(|p| p.page_break_before)
    }

    /// Resolve effective outlineLvl for a paragraph (§17.3.1.20).
    /// Direct wins if present, else style-resolved value, else None.
    pub fn resolve_effective_outline_lvl(
        &self,
        style_id: Option<&str>,
        direct: Option<u8>,
    ) -> Option<u8> {
        if direct.is_some() {
            return direct;
        }
        style_id
            .and_then(|id| self.para_props.get(id))
            .and_then(|p| p.outline_lvl)
            .or(self.ppr_defaults.outline_lvl)
    }

    /// Look up a resolved table style by style ID.
    ///
    /// Returns the pre-resolved table style properties (borders, cell margins, shading)
    /// from the style's basedOn chain. Returns None if the style ID doesn't exist
    /// or the style has no table-level properties.
    pub fn table_style(&self, style_id: &str) -> Option<&TableStyleProps> {
        self.table_styles.get(style_id)
    }

    /// Resolve a run's formatting through the style inheritance chain.
    ///
    /// For each property in `direct`:
    /// - If explicitly set (On/Off for marks, Some for style props), keep it.
    /// - If Inherit/None, look up the character style, then paragraph style, then doc defaults.
    ///
    /// Toggle properties (bold, italic, caps, small_caps, strike, vanish,
    /// emboss, imprint, outline, shadow) use XOR semantics across hierarchy
    /// levels per ISO 29500-1 §17.7.3.  Non-toggle marks use simple cascade.
    pub fn resolve(
        &self,
        direct: &TextMarks,
        char_style_id: Option<&str>,
        para_style_id: Option<&str>,
    ) -> TextMarks {
        // Per ISO 29500-1 §17.7.2, the cascade is:
        //   direct → explicit char style → default char style → para style → doc defaults
        // When char_style_id differs from the default char style, we merge the
        // default char style underneath the explicit one so that unset properties
        // in the explicit style fall through to the default char style before
        // reaching the paragraph style.
        let default_char_id = self.default_char_style_id.as_deref();
        let is_explicit_char_style = char_style_id.is_some() && char_style_id != default_char_id;

        let merged_char_marks;
        let char_marks = if is_explicit_char_style {
            let explicit = char_style_id.and_then(|id| self.char_styles.get(id));
            let default_char = default_char_id.and_then(|id| self.char_styles.get(id));
            match (explicit, default_char) {
                (Some(exp), Some(def)) => {
                    // Start with default char style, overlay explicit on top
                    let mut merged = def.clone();
                    overlay_marks(&mut merged, exp);
                    merged_char_marks = merged;
                    Some(&merged_char_marks)
                }
                (Some(exp), None) => Some(exp),
                (None, def) => def,
            }
        } else {
            char_style_id.and_then(|id| self.char_styles.get(id))
        };
        let para_marks = para_style_id.and_then(|id| self.para_styles.get(id));

        let mut result = TextMarks {
            rpr_after_content: direct.rpr_after_content,
            preserve_val_absent_underline: direct.preserve_val_absent_underline,
            authored_rfonts: direct.authored_rfonts.clone(),
            // Toggle properties — XOR across hierarchy levels (§17.7.3).
            bold: resolve_toggle_mark(
                direct.bold.clone(),
                char_marks.map(|m| &m.bold),
                para_marks.map(|m| &m.bold),
                &self.doc_defaults.bold,
            ),
            italic: resolve_toggle_mark(
                direct.italic.clone(),
                char_marks.map(|m| &m.italic),
                para_marks.map(|m| &m.italic),
                &self.doc_defaults.italic,
            ),
            caps: resolve_toggle_mark(
                direct.caps.clone(),
                char_marks.map(|m| &m.caps),
                para_marks.map(|m| &m.caps),
                &self.doc_defaults.caps,
            ),
            small_caps: resolve_toggle_mark(
                direct.small_caps.clone(),
                char_marks.map(|m| &m.small_caps),
                para_marks.map(|m| &m.small_caps),
                &self.doc_defaults.small_caps,
            ),
            strike: resolve_toggle_mark(
                direct.strike.clone(),
                char_marks.map(|m| &m.strike),
                para_marks.map(|m| &m.strike),
                &self.doc_defaults.strike,
            ),
            vanish: resolve_toggle_mark(
                direct.vanish.clone(),
                char_marks.map(|m| &m.vanish),
                para_marks.map(|m| &m.vanish),
                &self.doc_defaults.vanish,
            ),
            web_hidden: resolve_toggle_mark(
                direct.web_hidden.clone(),
                char_marks.map(|m| &m.web_hidden),
                para_marks.map(|m| &m.web_hidden),
                &self.doc_defaults.web_hidden,
            ),
            emboss: resolve_toggle_mark(
                direct.emboss.clone(),
                char_marks.map(|m| &m.emboss),
                para_marks.map(|m| &m.emboss),
                &self.doc_defaults.emboss,
            ),
            imprint: resolve_toggle_mark(
                direct.imprint.clone(),
                char_marks.map(|m| &m.imprint),
                para_marks.map(|m| &m.imprint),
                &self.doc_defaults.imprint,
            ),
            outline: resolve_toggle_mark(
                direct.outline.clone(),
                char_marks.map(|m| &m.outline),
                para_marks.map(|m| &m.outline),
                &self.doc_defaults.outline,
            ),
            shadow: resolve_toggle_mark(
                direct.shadow.clone(),
                char_marks.map(|m| &m.shadow),
                para_marks.map(|m| &m.shadow),
                &self.doc_defaults.shadow,
            ),
            // Non-toggle marks — simple cascade.
            underline: resolve_mark(
                direct.underline.clone(),
                char_marks.map(|m| &m.underline),
                para_marks.map(|m| &m.underline),
                &self.doc_defaults.underline,
            ),
            double_strike: resolve_mark(
                direct.double_strike.clone(),
                char_marks.map(|m| &m.double_strike),
                para_marks.map(|m| &m.double_strike),
                &self.doc_defaults.double_strike,
            ),
            subscript: resolve_mark(
                direct.subscript.clone(),
                char_marks.map(|m| &m.subscript),
                para_marks.map(|m| &m.subscript),
                &self.doc_defaults.subscript,
            ),
            superscript: resolve_mark(
                direct.superscript.clone(),
                char_marks.map(|m| &m.superscript),
                para_marks.map(|m| &m.superscript),
                &self.doc_defaults.superscript,
            ),
            font_family: resolve_option(
                &direct.font_family,
                char_marks.and_then(|m| m.font_family.as_ref()),
                para_marks.and_then(|m| m.font_family.as_ref()),
                self.doc_defaults.font_family.as_ref(),
            ),
            font_size: resolve_option(
                &direct.font_size,
                char_marks.and_then(|m| m.font_size.as_ref()),
                para_marks.and_then(|m| m.font_size.as_ref()),
                self.doc_defaults.font_size.as_ref(),
            ),
            color: resolve_option(
                &direct.color,
                char_marks.and_then(|m| m.color.as_ref()),
                para_marks.and_then(|m| m.color.as_ref()),
                self.doc_defaults.color.as_ref(),
            ),
            color_theme: resolve_option(
                &direct.color_theme,
                char_marks.and_then(|m| m.color_theme.as_ref()),
                para_marks.and_then(|m| m.color_theme.as_ref()),
                self.doc_defaults.color_theme.as_ref(),
            ),
            highlight: resolve_option(
                &direct.highlight,
                char_marks.and_then(|m| m.highlight.as_ref()),
                para_marks.and_then(|m| m.highlight.as_ref()),
                self.doc_defaults.highlight.as_ref(),
            ),
            underline_style: resolve_option(
                &direct.underline_style,
                char_marks.and_then(|m| m.underline_style.as_ref()),
                para_marks.and_then(|m| m.underline_style.as_ref()),
                self.doc_defaults.underline_style.as_ref(),
            ),
            // `Some` exactly where `underline_style` is, so this resolves from
            // the same level as the style it belongs to.
            underline_extra_attrs: resolve_option(
                &direct.underline_extra_attrs,
                char_marks.and_then(|m| m.underline_extra_attrs.as_ref()),
                para_marks.and_then(|m| m.underline_extra_attrs.as_ref()),
                self.doc_defaults.underline_extra_attrs.as_ref(),
            ),
            font_east_asia: resolve_option(
                &direct.font_east_asia,
                char_marks.and_then(|m| m.font_east_asia.as_ref()),
                para_marks.and_then(|m| m.font_east_asia.as_ref()),
                self.doc_defaults.font_east_asia.as_ref(),
            ),
            font_cs: resolve_option(
                &direct.font_cs,
                char_marks.and_then(|m| m.font_cs.as_ref()),
                para_marks.and_then(|m| m.font_cs.as_ref()),
                self.doc_defaults.font_cs.as_ref(),
            ),
            font_family_theme: resolve_option(
                &direct.font_family_theme,
                char_marks.and_then(|m| m.font_family_theme.as_ref()),
                para_marks.and_then(|m| m.font_family_theme.as_ref()),
                self.doc_defaults.font_family_theme.as_ref(),
            ),
            font_east_asia_theme: resolve_option(
                &direct.font_east_asia_theme,
                char_marks.and_then(|m| m.font_east_asia_theme.as_ref()),
                para_marks.and_then(|m| m.font_east_asia_theme.as_ref()),
                self.doc_defaults.font_east_asia_theme.as_ref(),
            ),
            font_cs_theme: resolve_option(
                &direct.font_cs_theme,
                char_marks.and_then(|m| m.font_cs_theme.as_ref()),
                para_marks.and_then(|m| m.font_cs_theme.as_ref()),
                self.doc_defaults.font_cs_theme.as_ref(),
            ),
            lang: resolve_option(
                &direct.lang,
                char_marks.and_then(|m| m.lang.as_ref()),
                para_marks.and_then(|m| m.lang.as_ref()),
                self.doc_defaults.lang.as_ref(),
            ),
            lang_east_asia: resolve_option(
                &direct.lang_east_asia,
                char_marks.and_then(|m| m.lang_east_asia.as_ref()),
                para_marks.and_then(|m| m.lang_east_asia.as_ref()),
                self.doc_defaults.lang_east_asia.as_ref(),
            ),
            lang_bidi: resolve_option(
                &direct.lang_bidi,
                char_marks.and_then(|m| m.lang_bidi.as_ref()),
                para_marks.and_then(|m| m.lang_bidi.as_ref()),
                self.doc_defaults.lang_bidi.as_ref(),
            ),
            char_spacing: resolve_option(
                &direct.char_spacing,
                char_marks.and_then(|m| m.char_spacing.as_ref()),
                para_marks.and_then(|m| m.char_spacing.as_ref()),
                self.doc_defaults.char_spacing.as_ref(),
            ),
            // cs/rtl — cascade through hierarchy (non-toggle).
            cs: resolve_mark(
                direct.cs.clone(),
                char_marks.map(|m| &m.cs),
                para_marks.map(|m| &m.cs),
                &self.doc_defaults.cs,
            ),
            rtl: resolve_mark(
                direct.rtl.clone(),
                char_marks.map(|m| &m.rtl),
                para_marks.map(|m| &m.rtl),
                &self.doc_defaults.rtl,
            ),
            // Complex script toggle properties (MS-OI29500 §17.3.2.1/§17.3.2.16/§17.3.2.38).
            bold_cs: resolve_toggle_mark(
                direct.bold_cs.clone(),
                char_marks.map(|m| &m.bold_cs),
                para_marks.map(|m| &m.bold_cs),
                &self.doc_defaults.bold_cs,
            ),
            italic_cs: resolve_toggle_mark(
                direct.italic_cs.clone(),
                char_marks.map(|m| &m.italic_cs),
                para_marks.map(|m| &m.italic_cs),
                &self.doc_defaults.italic_cs,
            ),
            font_size_cs: resolve_option(
                &direct.font_size_cs,
                char_marks.and_then(|m| m.font_size_cs.as_ref()),
                para_marks.and_then(|m| m.font_size_cs.as_ref()),
                self.doc_defaults.font_size_cs.as_ref(),
            ),
            // font_hint — cascade through hierarchy like other Option<String> fields.
            font_hint: resolve_option(
                &direct.font_hint,
                char_marks.and_then(|m| m.font_hint.as_ref()),
                para_marks.and_then(|m| m.font_hint.as_ref()),
                self.doc_defaults.font_hint.as_ref(),
            ),
            // Run border — cascade through hierarchy like other Option fields.
            run_border_style: resolve_option(
                &direct.run_border_style,
                char_marks.and_then(|m| m.run_border_style.as_ref()),
                para_marks.and_then(|m| m.run_border_style.as_ref()),
                self.doc_defaults.run_border_style.as_ref(),
            ),
            run_border_size: resolve_option(
                &direct.run_border_size,
                char_marks.and_then(|m| m.run_border_size.as_ref()),
                para_marks.and_then(|m| m.run_border_size.as_ref()),
                self.doc_defaults.run_border_size.as_ref(),
            ),
            run_border_space: resolve_option(
                &direct.run_border_space,
                char_marks.and_then(|m| m.run_border_space.as_ref()),
                para_marks.and_then(|m| m.run_border_space.as_ref()),
                self.doc_defaults.run_border_space.as_ref(),
            ),
            run_border_color: resolve_option(
                &direct.run_border_color,
                char_marks.and_then(|m| m.run_border_color.as_ref()),
                para_marks.and_then(|m| m.run_border_color.as_ref()),
                self.doc_defaults.run_border_color.as_ref(),
            ),
            run_border_extra_attrs: resolve_option(
                &direct.run_border_extra_attrs,
                char_marks.and_then(|m| m.run_border_extra_attrs.as_ref()),
                para_marks.and_then(|m| m.run_border_extra_attrs.as_ref()),
                self.doc_defaults.run_border_extra_attrs.as_ref(),
            ),
            // Vertical position, kerning, char width scaling — cascade.
            position: resolve_option(
                &direct.position,
                char_marks.and_then(|m| m.position.as_ref()),
                para_marks.and_then(|m| m.position.as_ref()),
                self.doc_defaults.position.as_ref(),
            ),
            kern: resolve_option(
                &direct.kern,
                char_marks.and_then(|m| m.kern.as_ref()),
                para_marks.and_then(|m| m.kern.as_ref()),
                self.doc_defaults.kern.as_ref(),
            ),
            char_width_scaling: resolve_option(
                &direct.char_width_scaling,
                char_marks.and_then(|m| m.char_width_scaling.as_ref()),
                para_marks.and_then(|m| m.char_width_scaling.as_ref()),
                self.doc_defaults.char_width_scaling.as_ref(),
            ),
            // Toggle properties for the 8 newly-modeled rPr children.
            no_proof: resolve_mark(
                direct.no_proof.clone(),
                char_marks.map(|m| &m.no_proof),
                para_marks.map(|m| &m.no_proof),
                &self.doc_defaults.no_proof,
            ),
            spec_vanish: resolve_mark(
                direct.spec_vanish.clone(),
                char_marks.map(|m| &m.spec_vanish),
                para_marks.map(|m| &m.spec_vanish),
                &self.doc_defaults.spec_vanish,
            ),
            o_math: resolve_mark(
                direct.o_math.clone(),
                char_marks.map(|m| &m.o_math),
                para_marks.map(|m| &m.o_math),
                &self.doc_defaults.o_math,
            ),
            snap_to_grid: resolve_mark(
                direct.snap_to_grid.clone(),
                char_marks.map(|m| &m.snap_to_grid),
                para_marks.map(|m| &m.snap_to_grid),
                &self.doc_defaults.snap_to_grid,
            ),
            // Value-carrying properties for the 8 newly-modeled rPr children.
            run_shading: resolve_option(
                &direct.run_shading,
                char_marks.and_then(|m| m.run_shading.as_ref()),
                para_marks.and_then(|m| m.run_shading.as_ref()),
                self.doc_defaults.run_shading.as_ref(),
            ),
            emphasis_mark: resolve_option(
                &direct.emphasis_mark,
                char_marks.and_then(|m| m.emphasis_mark.as_ref()),
                para_marks.and_then(|m| m.emphasis_mark.as_ref()),
                self.doc_defaults.emphasis_mark.as_ref(),
            ),
            text_effect: resolve_option(
                &direct.text_effect,
                char_marks.and_then(|m| m.text_effect.as_ref()),
                para_marks.and_then(|m| m.text_effect.as_ref()),
                self.doc_defaults.text_effect.as_ref(),
            ),
            fit_text_width: resolve_option(
                &direct.fit_text_width,
                char_marks.and_then(|m| m.fit_text_width.as_ref()),
                para_marks.and_then(|m| m.fit_text_width.as_ref()),
                self.doc_defaults.fit_text_width.as_ref(),
            ),
            fit_text_id: resolve_option(
                &direct.fit_text_id,
                char_marks.and_then(|m| m.fit_text_id.as_ref()),
                para_marks.and_then(|m| m.fit_text_id.as_ref()),
                self.doc_defaults.fit_text_id.as_ref(),
            ),
            // char_style_id, rpr_change, and preserved are direct-only, never
            // inherited from styles — an unmodeled rPr child captured on a
            // style definition's own rPr is a styles.xml-part concern (out of
            // scope here; see PreservedProp doc comment), not a per-run one.
            char_style_id: direct.char_style_id.clone(),
            rpr_change: direct.rpr_change.clone(),
            preserved: direct.preserved.clone(),
        };

        // ISO 29500-1 §17.3.2.26: `w:rFonts` is one cascade property, even
        // though its script slots are
        // represented independently in `TextMarks`. A direct run element that
        // authors a literal script slot replaces the corresponding inherited
        // Theme selector; Word does not attribute-merge that lower-level slot
        // back into the direct element. Keep a Theme selector authored on the
        // direct element itself: within one `w:rFonts`, Theme attributes still
        // take precedence over their corresponding literals.
        let direct_latin_literals_replace_inherited_theme = direct.authored_rfonts.ascii.is_some()
            && direct.authored_rfonts.h_ansi.is_some()
            && direct.authored_rfonts.ascii_theme.is_none()
            && direct.authored_rfonts.h_ansi_theme.is_none();
        if direct_latin_literals_replace_inherited_theme {
            result.font_family_theme = None;
        }
        let direct_latin_themes_replace_inherited_literals =
            direct.authored_rfonts.ascii_theme.is_some()
                && direct.authored_rfonts.h_ansi_theme.is_some();
        if direct_latin_themes_replace_inherited_literals {
            result.font_family = None;
        }
        if direct.authored_rfonts.east_asia.is_some()
            && direct.authored_rfonts.east_asia_theme.is_none()
        {
            result.font_east_asia_theme = None;
        }
        if direct.authored_rfonts.cs.is_some() && direct.authored_rfonts.cs_theme.is_none() {
            result.font_cs_theme = None;
        }
        // `w:color` is likewise one cascade property. A direct literal-only
        // element replaces a lower style/default Theme selector; Word does
        // not merge `themeColor` from the inherited element into the direct
        // `w:color`. A Theme selector authored on the direct element remains
        // authoritative over its same-element literal fallback.
        if direct.color.is_some() && direct.color_theme.is_none() {
            result.color_theme = None;
        }
        let direct_authors_latin_rfonts = direct.authored_rfonts.ascii.is_some()
            || direct.authored_rfonts.h_ansi.is_some()
            || direct.authored_rfonts.ascii_theme.is_some()
            || direct.authored_rfonts.h_ansi_theme.is_some();
        if !direct_authors_latin_rfonts
            && !char_marks.is_some_and(|marks| {
                marks.font_family.is_some() || marks.font_family_theme.is_some()
            })
            && para_style_id
                .or(self.default_para_style_id.as_deref())
                .is_some_and(|style_id| {
                    self.para_literal_latin_over_doc_default_theme
                        .contains(style_id)
                })
        {
            // W-STYLE-LITERAL-RFONTS-OVER-DEFAULT-THEME-01: the paragraph
            // style's literal rFonts element replaces the lower docDefaults
            // Theme selector for bounded Latin consumers. Linked/character
            // style cascades are deliberately excluded from this registry.
            result.font_family_theme = None;
        }

        // Post-processing: resolve theme font references to actual font names.
        // The element-level cascade rules above first remove a value inherited
        // from the replaced lower-level rFonts element. Within one rFonts
        // element the corresponding Theme selector is authoritative.
        if result.font_family.is_none()
            && let Some(ref theme_ref) = result.font_family_theme
        {
            result.font_family = self.theme_fonts.resolve(theme_ref).map(IStr::from);
        }
        if result.font_east_asia.is_none()
            && let Some(ref theme_ref) = result.font_east_asia_theme
        {
            result.font_east_asia = self.theme_fonts.resolve(theme_ref).map(IStr::from);
        }
        if result.font_cs.is_none()
            && let Some(ref theme_ref) = result.font_cs_theme
        {
            // MS-OI29500 §17.3.2.26c: when the cs theme slot resolves to an
            // empty or absent typeface, Word falls back to "Times New Roman".
            result.font_cs = self
                .theme_fonts
                .resolve(theme_ref)
                .map(IStr::from)
                .or_else(|| Some(IStr::from("Times New Roman")));
        }

        // rFonts hint=eastAsia selects eastAsia font slot.
        // Per MS-OI29500 §17.3.2.26(b): when hint="eastAsia", characters in
        // ambiguous Unicode ranges use the eastAsia font instead of hAnsi.
        // Simplified: when hint=eastAsia, override font_family with font_east_asia.
        if result.font_hint.as_deref() == Some("eastAsia")
            && let Some(ref ea_font) = result.font_east_asia
        {
            result.font_family = Some(ea_font.clone());
        }

        // cs/rtl forces cs font slot for ALL characters.
        // Per MS-OI29500 §17.3.2.26b: when w:cs or w:rtl is On, the cs font
        // is used for all characters regardless of Unicode range.
        let is_cs = result.cs == MarkValue::On || result.rtl == MarkValue::On;
        if is_cs && let Some(ref cs_font) = result.font_cs {
            result.font_family = Some(cs_font.clone());
        }

        // When cs/rtl is active, use bCs/iCs/szCs instead of b/i/sz.
        // Per ECMA-376 §17.3.2.1, §17.3.2.16, §17.3.2.38:
        // When cs/rtl is active, b/i/sz are IGNORED — only bCs/iCs/szCs apply.
        // If bCs/iCs is Inherit (absent), that means "not bold/italic" for CS text.
        // For szCs, only override sz if szCs is explicitly present.
        if is_cs {
            result.bold = result.bold_cs.clone();
            result.italic = result.italic_cs.clone();
            if result.font_size_cs.is_some() {
                result.font_size = result.font_size_cs;
            }
        }

        // eastAsia="Times New Roman" override rule.
        // Per MS-OI29500 §17.3.2.26d: when eastAsia is "Times New Roman"
        // and font_family is set, replace eastAsia with the ascii font.
        if result.font_east_asia.as_deref() == Some("Times New Roman")
            && let Some(ref ascii_font) = result.font_family
        {
            result.font_east_asia = Some(ascii_font.clone());
        }

        // Default font is Times New Roman.
        // Per MS-OI29500 §17.3.2.26c: when no font is resolved after the
        // entire chain (including theme fonts), default to Times New Roman.
        if result.font_family.is_none() {
            result.font_family = Some(IStr::from("Times New Roman"));
        }

        result
    }
}

/// Resolve a single boolean mark through the chain (non-toggle properties).
/// If direct is Inherit, fall through char style → para style → doc defaults.
fn resolve_mark(
    direct: MarkValue,
    char_style: Option<&MarkValue>,
    para_style: Option<&MarkValue>,
    doc_default: &MarkValue,
) -> MarkValue {
    if direct != MarkValue::Inherit {
        return direct;
    }
    if let Some(cs) = char_style
        && *cs != MarkValue::Inherit
    {
        return cs.clone();
    }
    if let Some(ps) = para_style
        && *ps != MarkValue::Inherit
    {
        return ps.clone();
    }
    doc_default.clone()
}

/// Resolve a toggle property through the style hierarchy.
///
/// MS-OI29500 §2.1.258: Word uses RESET (simple override) semantics between
/// paragraph style and character style levels, not XOR. Within a single
/// basedOn chain the "first value encountered" rule already applies (handled
/// by `resolve_chain`), so by the time values reach this function each level
/// is already resolved to a single value.
///
/// MS-OI29500 §2.1.230a: When docDefaults sets a toggle property on, styles
/// can still turn it off — docDefaults is just the base, not an unconditional
/// override.
///
/// Rules:
/// 1. Direct formatting always wins — no XOR.
/// 2. Character style overrides paragraph style (simple override, not XOR).
/// 3. Paragraph style overrides doc defaults.
/// 4. If no level sets the property, fall back to document defaults.
fn resolve_toggle_mark(
    direct: MarkValue,
    char_style: Option<&MarkValue>,
    para_style: Option<&MarkValue>,
    doc_default: &MarkValue,
) -> MarkValue {
    // Direct formatting always wins (§17.7.3).
    if direct != MarkValue::Inherit {
        return direct;
    }

    // Character style overrides paragraph style (MS-OI29500 §2.1.258 reset semantics).
    if let Some(cs) = char_style
        && *cs != MarkValue::Inherit
    {
        return cs.clone();
    }

    // Paragraph style overrides doc defaults (MS-OI29500 §2.1.230a).
    if let Some(ps) = para_style
        && *ps != MarkValue::Inherit
    {
        return ps.clone();
    }

    // No level set this property — fall back to doc defaults.
    doc_default.clone()
}

/// Resolve a single optional value-carrying property through the chain.
fn resolve_option<T: Clone>(
    direct: &Option<T>,
    char_style: Option<&T>,
    para_style: Option<&T>,
    doc_default: Option<&T>,
) -> Option<T> {
    if direct.is_some() {
        return direct.clone();
    }
    if let Some(cs) = char_style {
        return Some(cs.clone());
    }
    if let Some(ps) = para_style {
        return Some(ps.clone());
    }
    doc_default.cloned()
}

/// Resolve a style's basedOn chain, folding each parent under the child.
///
/// Returns a "pure" TextMarks containing only what the style chain itself
/// sets.  Properties not explicitly set anywhere in the chain remain
/// `Inherit` / `None`, so the caller can distinguish "level contributed a
/// value" from "level inherited from doc defaults".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LatinRFontsDisposition {
    Literal,
    Theme,
    LiteralAndTheme,
}

/// Find the first Latin rFonts element in one same-type paragraph-style chain.
///
/// Child styles precede their `basedOn` parents. A literal-only child element
/// replaces a parent's Theme selector; a Theme-only child replaces a parent's
/// literal; an element authoring both retains both with Theme precedence.
fn highest_paragraph_style_latin_rfonts(
    style_id: &str,
    raw_styles: &HashMap<String, RawStyle>,
) -> Option<LatinRFontsDisposition> {
    let mut current = Some(style_id);
    let mut visited = HashSet::new();
    while let Some(id) = current {
        if !visited.insert(id.to_string()) {
            return None;
        }
        let style = raw_styles.get(id)?;
        if style.style_type != "paragraph" {
            return None;
        }
        match (
            style.marks.font_family.is_some(),
            style.marks.font_family_theme.is_some(),
        ) {
            (true, false) => return Some(LatinRFontsDisposition::Literal),
            (false, true) => return Some(LatinRFontsDisposition::Theme),
            (true, true) => return Some(LatinRFontsDisposition::LiteralAndTheme),
            (false, false) => {}
        }
        current = style.based_on.as_deref();
    }
    None
}

fn resolve_chain(style_id: &str, raw_styles: &HashMap<String, RawStyle>) -> TextMarks {
    // Walk the basedOn chain collecting layers (child first).
    // Per §17.7.4.3: basedOn must reference a style of the same type.
    // If a cross-type reference is encountered, stop walking — the
    // mismatched parent's properties must not leak into this style.
    let origin_type = raw_styles.get(style_id).map(|r| r.style_type.as_str());
    let mut chain = Vec::new();
    let mut current = Some(style_id.to_string());
    let mut visited = std::collections::HashSet::new();

    while let Some(ref id) = current {
        if !visited.insert(id.clone()) {
            // OBSERVABLE BOUNDARY: a basedOn cycle is malformed styles.xml
            // (Word itself won't have produced it), but refusing the whole
            // document over one cyclic style chain would violate parse
            // totality — we cut the cycle and keep going with what we've
            // collected so far. Behavior unchanged, now logged.
            tracing::warn!(
                style_id, parent_id = %id,
                "basedOn chain cycle detected; stopping walk at the repeated style"
            );
            break;
        }
        if let Some(raw) = raw_styles.get(id) {
            // First entry (the style itself) always included; subsequent entries
            // must match the origin style's type.
            if !chain.is_empty() && origin_type != Some(raw.style_type.as_str()) {
                // OBSERVABLE BOUNDARY: cross-type basedOn is malformed per
                // §17.7.4.3. We stop walking rather than leaking the
                // mismatched parent's properties in — correct containment,
                // now logged so the malformed reference isn't silent.
                tracing::warn!(
                    style_id,
                    parent_id = %id,
                    parent_type = %raw.style_type,
                    origin_type = ?origin_type,
                    "basedOn references a style of a different type; stopping walk per §17.7.4.3"
                );
                break;
            }
            chain.push(&raw.marks);
            current = raw.based_on.clone();
        } else {
            // OBSERVABLE BOUNDARY: basedOn references a style id that isn't
            // defined anywhere in styles.xml. Dangling references are real
            // producer output Word tolerates (it just stops resolving the
            // chain there); we do the same, now logged.
            tracing::warn!(
                style_id,
                parent_id = %id,
                "basedOn references an undefined style id; stopping walk"
            );
            break;
        }
    }

    // Start from a blank base so each level's resolved value reflects only
    // what the style chain explicitly sets.  Doc defaults are applied later
    // at resolution time — this is required for toggle-property XOR semantics
    // (ISO 29500-1 §17.7.3) where we need to know whether a level actually
    // contributed a value vs. inherited it from doc defaults.
    let mut resolved = TextMarks::default();

    for layer in chain.iter().rev() {
        overlay_marks(&mut resolved, layer);
    }

    resolved
}

/// Overlay `top` marks onto `base`, replacing only where `top` is explicitly set.
fn overlay_marks(base: &mut TextMarks, top: &TextMarks) {
    if top.bold != MarkValue::Inherit {
        base.bold = top.bold.clone();
    }
    if top.italic != MarkValue::Inherit {
        base.italic = top.italic.clone();
    }
    if top.underline != MarkValue::Inherit {
        base.underline = top.underline.clone();
    }
    if top.strike != MarkValue::Inherit {
        base.strike = top.strike.clone();
    }
    if top.double_strike != MarkValue::Inherit {
        base.double_strike = top.double_strike.clone();
    }
    if top.subscript != MarkValue::Inherit {
        base.subscript = top.subscript.clone();
    }
    if top.superscript != MarkValue::Inherit {
        base.superscript = top.superscript.clone();
    }
    if top.caps != MarkValue::Inherit {
        base.caps = top.caps.clone();
    }
    if top.small_caps != MarkValue::Inherit {
        base.small_caps = top.small_caps.clone();
    }
    if top.vanish != MarkValue::Inherit {
        base.vanish = top.vanish.clone();
    }
    if top.web_hidden != MarkValue::Inherit {
        base.web_hidden = top.web_hidden.clone();
    }
    if top.emboss != MarkValue::Inherit {
        base.emboss = top.emboss.clone();
    }
    if top.imprint != MarkValue::Inherit {
        base.imprint = top.imprint.clone();
    }
    if top.outline != MarkValue::Inherit {
        base.outline = top.outline.clone();
    }
    if top.shadow != MarkValue::Inherit {
        base.shadow = top.shadow.clone();
    }
    if top.font_family.is_some() {
        base.font_family = top.font_family.clone();
    }
    if top.font_family_theme.is_some() {
        base.font_family_theme = top.font_family_theme.clone();
    }
    if top.font_size.is_some() {
        base.font_size = top.font_size;
    }
    if top.color.is_some() {
        base.color = top.color.clone();
    }
    if top.color_theme.is_some() {
        base.color_theme = top.color_theme.clone();
    }
    if top.highlight.is_some() {
        base.highlight = top.highlight.clone();
    }
    if top.underline_style.is_some() {
        base.underline_style = top.underline_style.clone();
        base.underline_extra_attrs = top.underline_extra_attrs.clone();
    }
    if top.font_east_asia.is_some() {
        base.font_east_asia = top.font_east_asia.clone();
    }
    if top.font_east_asia_theme.is_some() {
        base.font_east_asia_theme = top.font_east_asia_theme.clone();
    }
    if top.font_cs.is_some() {
        base.font_cs = top.font_cs.clone();
    }
    if top.font_cs_theme.is_some() {
        base.font_cs_theme = top.font_cs_theme.clone();
    }
    if top.lang.is_some() {
        base.lang = top.lang.clone();
    }
    if top.lang_east_asia.is_some() {
        base.lang_east_asia = top.lang_east_asia.clone();
    }
    if top.lang_bidi.is_some() {
        base.lang_bidi = top.lang_bidi.clone();
    }
    if top.char_spacing.is_some() {
        base.char_spacing = top.char_spacing;
    }
    if top.font_hint.is_some() {
        base.font_hint = top.font_hint.clone();
    }
    if top.cs != MarkValue::Inherit {
        base.cs = top.cs.clone();
    }
    if top.rtl != MarkValue::Inherit {
        base.rtl = top.rtl.clone();
    }
    if top.bold_cs != MarkValue::Inherit {
        base.bold_cs = top.bold_cs.clone();
    }
    if top.italic_cs != MarkValue::Inherit {
        base.italic_cs = top.italic_cs.clone();
    }
    if top.font_size_cs.is_some() {
        base.font_size_cs = top.font_size_cs;
    }
    if top.run_border_style.is_some() {
        base.run_border_style = top.run_border_style.clone();
    }
    if top.run_border_size.is_some() {
        base.run_border_size = top.run_border_size;
    }
    if top.run_border_space.is_some() {
        base.run_border_space = top.run_border_space;
    }
    if top.run_border_color.is_some() {
        base.run_border_color = top.run_border_color.clone();
    }
    if top.run_border_extra_attrs.is_some() {
        base.run_border_extra_attrs = top.run_border_extra_attrs.clone();
    }
    if top.position.is_some() {
        base.position = top.position;
    }
    if top.kern.is_some() {
        base.kern = top.kern;
    }
    if top.char_width_scaling.is_some() {
        base.char_width_scaling = top.char_width_scaling;
    }
    if top.no_proof != MarkValue::Inherit {
        base.no_proof = top.no_proof.clone();
    }
    if top.spec_vanish != MarkValue::Inherit {
        base.spec_vanish = top.spec_vanish.clone();
    }
    if top.o_math != MarkValue::Inherit {
        base.o_math = top.o_math.clone();
    }
    if top.snap_to_grid != MarkValue::Inherit {
        base.snap_to_grid = top.snap_to_grid.clone();
    }
    if top.run_shading.is_some() {
        base.run_shading = top.run_shading.clone();
    }
    if top.emphasis_mark.is_some() {
        base.emphasis_mark = top.emphasis_mark.clone();
    }
    if top.text_effect.is_some() {
        base.text_effect = top.text_effect.clone();
    }
    if top.fit_text_width.is_some() {
        base.fit_text_width = top.fit_text_width;
    }
    if top.fit_text_id.is_some() {
        base.fit_text_id = top.fit_text_id;
    }
}

/// Overlay linked char style's resolved properties onto para style's resolved base.
///
/// Per ISO 29500-1 §17.7.4.6:
/// - Char style's explicit properties always win
/// - Para style's explicit properties win over char's merely-inherited properties
/// - Char's inherited properties fill gaps where para doesn't set a value
fn overlay_linked_char(
    base: &mut TextMarks,
    char_resolved: &TextMarks,
    para_raw: &TextMarks,
    char_raw: &TextMarks,
) {
    // MarkValue fields: apply if char resolved has a value AND
    // either char explicitly sets it OR para doesn't explicitly set it
    if char_resolved.bold != MarkValue::Inherit
        && (char_raw.bold != MarkValue::Inherit || para_raw.bold == MarkValue::Inherit)
    {
        base.bold = char_resolved.bold.clone();
    }
    if char_resolved.italic != MarkValue::Inherit
        && (char_raw.italic != MarkValue::Inherit || para_raw.italic == MarkValue::Inherit)
    {
        base.italic = char_resolved.italic.clone();
    }
    if char_resolved.underline != MarkValue::Inherit
        && (char_raw.underline != MarkValue::Inherit || para_raw.underline == MarkValue::Inherit)
    {
        base.underline = char_resolved.underline.clone();
    }
    if char_resolved.strike != MarkValue::Inherit
        && (char_raw.strike != MarkValue::Inherit || para_raw.strike == MarkValue::Inherit)
    {
        base.strike = char_resolved.strike.clone();
    }
    if char_resolved.double_strike != MarkValue::Inherit
        && (char_raw.double_strike != MarkValue::Inherit
            || para_raw.double_strike == MarkValue::Inherit)
    {
        base.double_strike = char_resolved.double_strike.clone();
    }
    if char_resolved.subscript != MarkValue::Inherit
        && (char_raw.subscript != MarkValue::Inherit || para_raw.subscript == MarkValue::Inherit)
    {
        base.subscript = char_resolved.subscript.clone();
    }
    if char_resolved.superscript != MarkValue::Inherit
        && (char_raw.superscript != MarkValue::Inherit
            || para_raw.superscript == MarkValue::Inherit)
    {
        base.superscript = char_resolved.superscript.clone();
    }
    if char_resolved.caps != MarkValue::Inherit
        && (char_raw.caps != MarkValue::Inherit || para_raw.caps == MarkValue::Inherit)
    {
        base.caps = char_resolved.caps.clone();
    }
    if char_resolved.small_caps != MarkValue::Inherit
        && (char_raw.small_caps != MarkValue::Inherit || para_raw.small_caps == MarkValue::Inherit)
    {
        base.small_caps = char_resolved.small_caps.clone();
    }
    if char_resolved.vanish != MarkValue::Inherit
        && (char_raw.vanish != MarkValue::Inherit || para_raw.vanish == MarkValue::Inherit)
    {
        base.vanish = char_resolved.vanish.clone();
    }
    if char_resolved.web_hidden != MarkValue::Inherit
        && (char_raw.web_hidden != MarkValue::Inherit || para_raw.web_hidden == MarkValue::Inherit)
    {
        base.web_hidden = char_resolved.web_hidden.clone();
    }
    if char_resolved.emboss != MarkValue::Inherit
        && (char_raw.emboss != MarkValue::Inherit || para_raw.emboss == MarkValue::Inherit)
    {
        base.emboss = char_resolved.emboss.clone();
    }
    if char_resolved.imprint != MarkValue::Inherit
        && (char_raw.imprint != MarkValue::Inherit || para_raw.imprint == MarkValue::Inherit)
    {
        base.imprint = char_resolved.imprint.clone();
    }
    if char_resolved.outline != MarkValue::Inherit
        && (char_raw.outline != MarkValue::Inherit || para_raw.outline == MarkValue::Inherit)
    {
        base.outline = char_resolved.outline.clone();
    }
    if char_resolved.shadow != MarkValue::Inherit
        && (char_raw.shadow != MarkValue::Inherit || para_raw.shadow == MarkValue::Inherit)
    {
        base.shadow = char_resolved.shadow.clone();
    }
    if char_resolved.cs != MarkValue::Inherit
        && (char_raw.cs != MarkValue::Inherit || para_raw.cs == MarkValue::Inherit)
    {
        base.cs = char_resolved.cs.clone();
    }
    if char_resolved.rtl != MarkValue::Inherit
        && (char_raw.rtl != MarkValue::Inherit || para_raw.rtl == MarkValue::Inherit)
    {
        base.rtl = char_resolved.rtl.clone();
    }
    if char_resolved.bold_cs != MarkValue::Inherit
        && (char_raw.bold_cs != MarkValue::Inherit || para_raw.bold_cs == MarkValue::Inherit)
    {
        base.bold_cs = char_resolved.bold_cs.clone();
    }
    if char_resolved.italic_cs != MarkValue::Inherit
        && (char_raw.italic_cs != MarkValue::Inherit || para_raw.italic_cs == MarkValue::Inherit)
    {
        base.italic_cs = char_resolved.italic_cs.clone();
    }

    // Option<T> fields: apply if char resolved has a value AND
    // either char explicitly sets it OR para doesn't explicitly set it
    if char_resolved.font_family.is_some()
        && (char_raw.font_family.is_some() || para_raw.font_family.is_none())
    {
        base.font_family = char_resolved.font_family.clone();
    }
    if char_resolved.font_family_theme.is_some()
        && (char_raw.font_family_theme.is_some() || para_raw.font_family_theme.is_none())
    {
        base.font_family_theme = char_resolved.font_family_theme.clone();
    }
    if char_resolved.font_size.is_some()
        && (char_raw.font_size.is_some() || para_raw.font_size.is_none())
    {
        base.font_size = char_resolved.font_size;
    }
    if char_resolved.color.is_some() && (char_raw.color.is_some() || para_raw.color.is_none()) {
        base.color = char_resolved.color.clone();
    }
    if char_resolved.color_theme.is_some()
        && (char_raw.color_theme.is_some() || para_raw.color_theme.is_none())
    {
        base.color_theme = char_resolved.color_theme.clone();
    }
    if char_resolved.highlight.is_some()
        && (char_raw.highlight.is_some() || para_raw.highlight.is_none())
    {
        base.highlight = char_resolved.highlight.clone();
    }
    if char_resolved.underline_style.is_some()
        && (char_raw.underline_style.is_some() || para_raw.underline_style.is_none())
    {
        base.underline_style = char_resolved.underline_style.clone();
        base.underline_extra_attrs = char_resolved.underline_extra_attrs.clone();
    }
    if char_resolved.font_east_asia.is_some()
        && (char_raw.font_east_asia.is_some() || para_raw.font_east_asia.is_none())
    {
        base.font_east_asia = char_resolved.font_east_asia.clone();
    }
    if char_resolved.font_east_asia_theme.is_some()
        && (char_raw.font_east_asia_theme.is_some() || para_raw.font_east_asia_theme.is_none())
    {
        base.font_east_asia_theme = char_resolved.font_east_asia_theme.clone();
    }
    if char_resolved.font_cs.is_some() && (char_raw.font_cs.is_some() || para_raw.font_cs.is_none())
    {
        base.font_cs = char_resolved.font_cs.clone();
    }
    if char_resolved.font_cs_theme.is_some()
        && (char_raw.font_cs_theme.is_some() || para_raw.font_cs_theme.is_none())
    {
        base.font_cs_theme = char_resolved.font_cs_theme.clone();
    }
    if char_resolved.lang.is_some() && (char_raw.lang.is_some() || para_raw.lang.is_none()) {
        base.lang = char_resolved.lang.clone();
    }
    if char_resolved.lang_east_asia.is_some()
        && (char_raw.lang_east_asia.is_some() || para_raw.lang_east_asia.is_none())
    {
        base.lang_east_asia = char_resolved.lang_east_asia.clone();
    }
    if char_resolved.lang_bidi.is_some()
        && (char_raw.lang_bidi.is_some() || para_raw.lang_bidi.is_none())
    {
        base.lang_bidi = char_resolved.lang_bidi.clone();
    }
    if char_resolved.char_spacing.is_some()
        && (char_raw.char_spacing.is_some() || para_raw.char_spacing.is_none())
    {
        base.char_spacing = char_resolved.char_spacing;
    }
    if char_resolved.font_hint.is_some()
        && (char_raw.font_hint.is_some() || para_raw.font_hint.is_none())
    {
        base.font_hint = char_resolved.font_hint.clone();
    }
    if char_resolved.font_size_cs.is_some()
        && (char_raw.font_size_cs.is_some() || para_raw.font_size_cs.is_none())
    {
        base.font_size_cs = char_resolved.font_size_cs;
    }
    if char_resolved.run_border_style.is_some()
        && (char_raw.run_border_style.is_some() || para_raw.run_border_style.is_none())
    {
        base.run_border_style = char_resolved.run_border_style.clone();
    }
    if char_resolved.run_border_size.is_some()
        && (char_raw.run_border_size.is_some() || para_raw.run_border_size.is_none())
    {
        base.run_border_size = char_resolved.run_border_size;
    }
    if char_resolved.run_border_space.is_some()
        && (char_raw.run_border_space.is_some() || para_raw.run_border_space.is_none())
    {
        base.run_border_space = char_resolved.run_border_space;
    }
    if char_resolved.run_border_color.is_some()
        && (char_raw.run_border_color.is_some() || para_raw.run_border_color.is_none())
    {
        base.run_border_color = char_resolved.run_border_color.clone();
    }
    if char_resolved.run_border_extra_attrs.is_some()
        && (char_raw.run_border_extra_attrs.is_some() || para_raw.run_border_extra_attrs.is_none())
    {
        base.run_border_extra_attrs = char_resolved.run_border_extra_attrs.clone();
    }
    if char_resolved.position.is_some()
        && (char_raw.position.is_some() || para_raw.position.is_none())
    {
        base.position = char_resolved.position;
    }
    if char_resolved.kern.is_some() && (char_raw.kern.is_some() || para_raw.kern.is_none()) {
        base.kern = char_resolved.kern;
    }
    if char_resolved.char_width_scaling.is_some()
        && (char_raw.char_width_scaling.is_some() || para_raw.char_width_scaling.is_none())
    {
        base.char_width_scaling = char_resolved.char_width_scaling;
    }
    if char_resolved.no_proof != MarkValue::Inherit
        && (char_raw.no_proof != MarkValue::Inherit || para_raw.no_proof == MarkValue::Inherit)
    {
        base.no_proof = char_resolved.no_proof.clone();
    }
    if char_resolved.spec_vanish != MarkValue::Inherit
        && (char_raw.spec_vanish != MarkValue::Inherit
            || para_raw.spec_vanish == MarkValue::Inherit)
    {
        base.spec_vanish = char_resolved.spec_vanish.clone();
    }
    if char_resolved.o_math != MarkValue::Inherit
        && (char_raw.o_math != MarkValue::Inherit || para_raw.o_math == MarkValue::Inherit)
    {
        base.o_math = char_resolved.o_math.clone();
    }
    if char_resolved.snap_to_grid != MarkValue::Inherit
        && (char_raw.snap_to_grid != MarkValue::Inherit
            || para_raw.snap_to_grid == MarkValue::Inherit)
    {
        base.snap_to_grid = char_resolved.snap_to_grid.clone();
    }
    if char_resolved.run_shading.is_some()
        && (char_raw.run_shading.is_some() || para_raw.run_shading.is_none())
    {
        base.run_shading = char_resolved.run_shading.clone();
    }
    if char_resolved.emphasis_mark.is_some()
        && (char_raw.emphasis_mark.is_some() || para_raw.emphasis_mark.is_none())
    {
        base.emphasis_mark = char_resolved.emphasis_mark.clone();
    }
    if char_resolved.text_effect.is_some()
        && (char_raw.text_effect.is_some() || para_raw.text_effect.is_none())
    {
        base.text_effect = char_resolved.text_effect.clone();
    }
    if char_resolved.fit_text_width.is_some()
        && (char_raw.fit_text_width.is_some() || para_raw.fit_text_width.is_none())
    {
        base.fit_text_width = char_resolved.fit_text_width;
    }
    if char_resolved.fit_text_id.is_some()
        && (char_raw.fit_text_id.is_some() || para_raw.fit_text_id.is_none())
    {
        base.fit_text_id = char_resolved.fit_text_id;
    }
}

/// Parse document defaults: w:docDefaults/w:rPrDefault/w:rPr.
fn parse_doc_defaults(root: &Element) -> Result<TextMarks, String> {
    let doc_defaults = match find_w_child(root, "docDefaults") {
        Some(el) => el,
        None => return Ok(TextMarks::default()),
    };
    let rpr_default = match find_w_child(doc_defaults, "rPrDefault") {
        Some(el) => el,
        None => return Ok(TextMarks::default()),
    };
    let rpr = match find_w_child(rpr_default, "rPr") {
        Some(el) => el,
        None => return Ok(TextMarks::default()),
    };
    parse_rpr_marks(rpr).map_err(|err| format!("word/styles.xml docDefaults: {err}"))
}

/// Parse document paragraph defaults: w:docDefaults/w:pPrDefault/w:pPr.
fn parse_ppr_defaults(root: &Element) -> Result<RawParagraphProps, String> {
    let doc_defaults = match find_w_child(root, "docDefaults") {
        Some(el) => el,
        None => return Ok(RawParagraphProps::default()),
    };
    let ppr_default = match find_w_child(doc_defaults, "pPrDefault") {
        Some(el) => el,
        None => return Ok(RawParagraphProps::default()),
    };
    let ppr = match find_w_child(ppr_default, "pPr") {
        Some(el) => el,
        None => return Ok(RawParagraphProps::default()),
    };
    extract_raw_para_props(ppr).map_err(|err| format!("word/styles.xml docDefaults: {err}"))
}

/// Parse formatting marks from a w:rPr element.
/// Shared between style definitions and document defaults. A font size is
/// read by the same ST_HpsMeasure parser as a run's
/// (`word_ir::font_size_half_points`), so styles and runs agree, and a value is
/// never dropped.
fn parse_rpr_marks(rpr: &Element) -> Result<TextMarks, String> {
    let mut marks = TextMarks::default();

    for child in &rpr.children {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };

        // An extension element is never a modeled style property, even when it
        // shares a core local name (`w14:shadow` is not `w:shadow`).
        if !crate::word_ir::is_word_core_element(el) {
            continue;
        }
        let local_name = local_element_name(el);

        match local_name.as_str() {
            "shd" => marks.run_shading = Some(crate::word_ir::parse_run_shading(el)),
            "b" => marks.bold = parse_toggle_value(el),
            "i" => marks.italic = parse_toggle_value(el),
            "u" => {
                marks.underline = parse_underline_value(el);
                marks.underline_style = attr_get(el, "w:val").cloned();
                marks.underline_extra_attrs = marks
                    .underline_style
                    .is_some()
                    .then(|| crate::xml_attrs::capture_extra_attrs(el, &["val"]));
            }
            "strike" => marks.strike = parse_toggle_value(el),
            "dstrike" => marks.double_strike = parse_toggle_value(el),
            "vanish" => marks.vanish = parse_toggle_value(el),
            "webHidden" => marks.web_hidden = parse_toggle_value(el),
            "emboss" => marks.emboss = parse_toggle_value(el),
            "imprint" => marks.imprint = parse_toggle_value(el),
            "outline" => marks.outline = parse_toggle_value(el),
            "shadow" => marks.shadow = parse_toggle_value(el),
            "vertAlign" => {
                if let Some(val) = attr_get(el, "w:val") {
                    match val.as_str() {
                        "subscript" => marks.subscript = MarkValue::On,
                        "superscript" => marks.superscript = MarkValue::On,
                        "baseline" => {
                            marks.subscript = MarkValue::Off;
                            marks.superscript = MarkValue::Off;
                        }
                        _ => {}
                    }
                }
            }
            "caps" => marks.caps = parse_toggle_value(el),
            "smallCaps" => marks.small_caps = parse_toggle_value(el),
            "rFonts" => {
                marks.font_family = attr_get(el, "w:ascii")
                    .or_else(|| attr_get(el, "w:hAnsi"))
                    .map(|s| IStr::from(s.as_str()));
                marks.font_family_theme = attr_get(el, "w:asciiTheme")
                    .or_else(|| attr_get(el, "w:hAnsiTheme"))
                    .map(|s| IStr::from(s.as_str()));
                marks.font_east_asia = attr_get(el, "w:eastAsia").map(|s| IStr::from(s.as_str()));
                marks.font_east_asia_theme =
                    attr_get(el, "w:eastAsiaTheme").map(|s| IStr::from(s.as_str()));
                marks.font_cs = attr_get(el, "w:cs").map(|s| IStr::from(s.as_str()));
                // The normative attribute is lowercase-t "cstheme" — the lone
                // exception among asciiTheme/hAnsiTheme/eastAsiaTheme
                // (§17.3.2.26). Reading only "csTheme" silently dropped the
                // docDefaults cs slot, so every run's effective font_cs lost
                // the theme contribution real Word resolves (wave-8 real-word
                // v5 lifecycle 0). word_ir's run parser was fixed for this
                // same trap earlier; this is the styles-part twin.
                marks.font_cs_theme = attr_get(el, "w:cstheme")
                    .or_else(|| attr_get(el, "w:csTheme"))
                    .map(|s| IStr::from(s.as_str()));
            }
            "sz" => marks.font_size = style_half_points(el, "sz")?,
            "color" => {
                if let Some(val) = attr_get(el, "w:val") {
                    marks.color = Some(IStr::from(val.as_str()));
                }
                if let Some(tc) = attr_get(el, "w:themeColor") {
                    marks.color_theme = Some(crate::domain::ThemeColorRef {
                        theme_color: IStr::from(tc.as_str()),
                        theme_shade: attr_get(el, "w:themeShade").map(|s| IStr::from(s.as_str())),
                        theme_tint: attr_get(el, "w:themeTint").map(|s| IStr::from(s.as_str())),
                    });
                }
            }
            "highlight" => {
                marks.highlight = attr_get(el, "w:val").cloned();
            }
            "lang" => {
                marks.lang = attr_get(el, "w:val").map(|s| IStr::from(s.as_str()));
                marks.lang_east_asia = attr_get(el, "w:eastAsia").map(|s| IStr::from(s.as_str()));
                marks.lang_bidi = attr_get(el, "w:bidi").map(|s| IStr::from(s.as_str()));
            }
            "spacing" => {
                // Read as a run's character spacing is read.
                marks.char_spacing = crate::word_ir::char_spacing_twips(el)
                    .map_err(|err| format!("rPr/spacing: {err}"))?;
            }
            "position" => {
                // w:position w:val="6" — vertical displacement in half-points (ISO 29500-1 §17.3.2.19)
                marks.position = attr_get(el, "w:val").and_then(|v| v.parse::<i64>().ok());
            }
            "kern" => {
                // w:kern w:val="28" — kerning threshold in half-points (ISO 29500-1 §17.3.2.19a)
                marks.kern = attr_get(el, "w:val").and_then(|v| v.parse::<i64>().ok());
            }
            "w" => {
                // w:w w:val="150" — character width scaling percentage (ISO 29500-1 §17.3.2.43)
                marks.char_width_scaling =
                    attr_get(el, "w:val").and_then(|v| v.parse::<i64>().ok());
            }
            "cs" => marks.cs = parse_toggle_value(el),
            "rtl" => marks.rtl = parse_toggle_value(el),
            "bCs" => marks.bold_cs = parse_toggle_value(el),
            "iCs" => marks.italic_cs = parse_toggle_value(el),
            "szCs" => marks.font_size_cs = style_half_points(el, "szCs")?,
            _ => {}
        }
    }

    Ok(marks)
}

/// A style's `w:sz` / `w:szCs` value in half-points, read as a run's is
/// (`word_ir::font_size_half_points`).
fn style_half_points(element: &Element, name: &'static str) -> Result<Option<u32>, String> {
    crate::word_ir::font_size_half_points(element, name).map_err(|err| err.to_string())
}

/// Parse a toggle property value (CT_OnOff, e.g. `w:b`, `w:i`).
///
/// - Absent: On (element presence implies true for a toggle property).
/// - `val` recognized false-like ("0"/"false"/"off"): Off.
/// - `val` recognized true-like ("1"/"true"/"on"): On.
/// - `val` anything else (schema-invalid — ST_OnOff only permits the above):
///   PRODUCT-APPROVED DEFAULT, On. This matches Word's own tolerant boolean
///   parsing (anything not recognized as false is treated as true) — see
///   `spec_toggle_properties_xor_hierarchy_word_compliance.rs`. Logged since
///   the value is out-of-spec; the fallback itself is intentional, not silent
///   data loss.
fn parse_toggle_value(el: &Element) -> MarkValue {
    match attr_get(el, "w:val") {
        None => MarkValue::On,
        Some(val) => match val.as_str() {
            "0" | "false" | "off" => MarkValue::Off,
            "1" | "true" | "on" => MarkValue::On,
            other => {
                tracing::warn!(
                    value = %other,
                    "unrecognized ST_OnOff value on a toggle property; treating as On (product-approved default)"
                );
                MarkValue::On
            }
        },
    }
}

/// Parse underline value.
fn parse_underline_value(el: &Element) -> MarkValue {
    match attr_get(el, "w:val") {
        None => MarkValue::On,
        Some(val) => {
            if val == "none" {
                MarkValue::Off
            } else {
                MarkValue::On
            }
        }
    }
}

fn is_w_tag(element: &Element, local: &str) -> bool {
    if element.name == local {
        if element.prefix.as_deref() == Some("w") {
            return true;
        }
        return element.namespace.as_deref() == Some(WORD_NS);
    }
    element.name == format!("w:{local}")
}

fn find_w_child<'a>(parent: &'a Element, local: &str) -> Option<&'a Element> {
    for child in &parent.children {
        if let XMLNode::Element(el) = child
            && is_w_tag(el, local)
        {
            return Some(el);
        }
    }
    None
}

fn local_element_name(element: &Element) -> String {
    if let Some(pos) = element.name.find(':') {
        element.name[pos + 1..].to_string()
    } else {
        element.name.clone()
    }
}

// =============================================================================
// Paragraph property (alignment, indent) resolution
// =============================================================================

/// Extract raw paragraph properties (alignment, indent, spacing, borders) from a w:pPr element.
fn extract_raw_para_props(ppr: &Element) -> Result<RawParagraphProps, String> {
    let alignment = find_w_child(ppr, "jc")
        .and_then(|jc| attr_get(jc, "w:val"))
        .map(|value| Alignment::from_xml_str(value).map_err(|err| format!("pPr/jc: {err}")))
        .transpose()?;

    let (
        indent_left,
        indent_right,
        indent_first_line,
        indent_start_chars,
        indent_end_chars,
        indent_first_line_chars,
        indent_hanging_chars,
    ) = match find_w_child(ppr, "ind") {
        Some(ind) => {
            // Read as a paragraph's own w:ind is read (one twips reading).
            let twips = |attribute: &'static str, allows_negative: bool| {
                crate::word_ir::parse_paragraph_twip_measure(ind, "ind", attribute, allows_negative)
                    .map_err(|err| format!("pPr/ind: {err}"))?
                    .map(|twips| {
                        i32::try_from(twips).map_err(|_| {
                            format!("pPr/ind: w:{attribute} value {twips} is outside i32 twips")
                        })
                    })
                    .transpose()
            };
            let left = if attr_get(ind, "w:left").is_some() {
                twips("left", true)?
            } else {
                twips("start", true)?
            };
            let right = if attr_get(ind, "w:right").is_some() {
                twips("right", true)?
            } else {
                twips("end", true)?
            };
            // §17.3.1.12: "firstLine and hanging are mutually exclusive,
            // if both are specified, the firstLine value is ignored" — hanging wins.
            let first_line = match twips("hanging", false)? {
                Some(hanging) => Some(-hanging),
                None => twips("firstLine", true)?,
            };
            // Character-unit indents (MS-OI29500 2.1.44, §17.3.1.12): stored raw,
            // including an explicit 0 (a real override that cancels an inherited
            // character indent). Precedence — a non-zero chars value wins over its
            // twips sibling — is applied by resolve_effective_indent, not here, so
            // zeros are not filtered. leftChars/rightChars are the transitional
            // aliases of startChars/endChars.
            let start_chars = attr_get(ind, "w:startChars")
                .or_else(|| attr_get(ind, "w:leftChars"))
                .and_then(|v| v.parse().ok());
            let end_chars = attr_get(ind, "w:endChars")
                .or_else(|| attr_get(ind, "w:rightChars"))
                .and_then(|v| v.parse().ok());
            let first_line_chars = attr_get(ind, "w:firstLineChars").and_then(|v| v.parse().ok());
            let hanging_chars = attr_get(ind, "w:hangingChars").and_then(|v| v.parse().ok());
            (
                left,
                right,
                first_line,
                start_chars,
                end_chars,
                first_line_chars,
                hanging_chars,
            )
        }
        None => (None, None, None, None, None, None, None),
    };

    let (
        spacing_before,
        spacing_after,
        spacing_before_lines,
        spacing_after_lines,
        spacing_before_autospacing,
        spacing_after_autospacing,
        spacing_line,
        spacing_line_rule,
    ) = match find_w_child(ppr, "spacing") {
        Some(sp) => {
            // Read as a paragraph's own w:spacing is read (one twips reading).
            let twips = |attribute: &'static str, allows_negative: bool| {
                crate::word_ir::parse_paragraph_twip_measure(
                    sp,
                    "spacing",
                    attribute,
                    allows_negative,
                )
                .map_err(|err| format!("pPr/spacing: {err}"))
            };
            let before = twips("before", false)?
                .map(|v| {
                    u32::try_from(v).map_err(|_| format!("pPr/spacing: w:before {v} out of range"))
                })
                .transpose()?;
            let after = twips("after", false)?
                .map(|v| {
                    u32::try_from(v).map_err(|_| format!("pPr/spacing: w:after {v} out of range"))
                })
                .transpose()?;
            let before_lines = attr_get(sp, "w:beforeLines").and_then(|v| v.parse().ok());
            let after_lines = attr_get(sp, "w:afterLines").and_then(|v| v.parse().ok());
            // §17.3.1.33: beforeAutospacing/afterAutospacing — ST_OnOff.
            let before_autospacing = attr_get(sp, "w:beforeAutospacing")
                .map(|v| !matches!(v.as_str(), "0" | "false" | "off"));
            let after_autospacing = attr_get(sp, "w:afterAutospacing")
                .map(|v| !matches!(v.as_str(), "0" | "false" | "off"));
            let line = twips("line", true)?
                .map(|v| {
                    i32::try_from(v).map_err(|_| format!("pPr/spacing: w:line {v} out of range"))
                })
                .transpose()?;
            let line_rule = attr_get(sp, "w:lineRule").cloned();
            (
                before,
                after,
                before_lines,
                after_lines,
                before_autospacing,
                after_autospacing,
                line,
                line_rule,
            )
        }
        None => (None, None, None, None, None, None, None, None),
    };

    let borders = extract_ppr_borders(ppr);

    // Extract numPr (numId + ilvl) from w:pPr/w:numPr (§17.7.4.14).
    let num_props = extract_style_num_props(ppr);
    let num_ilvl = extract_style_num_ilvl(ppr)?;

    // Extract contextualSpacing (CT_OnOff, §17.3.1.9).
    let contextual_spacing = find_w_child(ppr, "contextualSpacing").map(
        |el| !matches!(attr_get(el, "w:val"), Some(v) if v == "0" || v == "false" || v == "off"),
    );

    // Extract widowControl (CT_OnOff, §17.3.1.44).
    let widow_control = find_w_child(ppr, "widowControl").map(
        |el| !matches!(attr_get(el, "w:val"), Some(v) if v == "0" || v == "false" || v == "off"),
    );

    // Extract keepNext (CT_OnOff, §17.3.1.15).
    let keep_next = find_w_child(ppr, "keepNext").map(
        |el| !matches!(attr_get(el, "w:val"), Some(v) if v == "0" || v == "false" || v == "off"),
    );

    // Extract keepLines (CT_OnOff, §17.3.1.14).
    let keep_lines = find_w_child(ppr, "keepLines").map(
        |el| !matches!(attr_get(el, "w:val"), Some(v) if v == "0" || v == "false" || v == "off"),
    );

    // Extract pageBreakBefore (CT_OnOff, §17.3.1.23).
    let page_break_before = find_w_child(ppr, "pageBreakBefore").map(
        |el| !matches!(attr_get(el, "w:val"), Some(v) if v == "0" || v == "false" || v == "off"),
    );

    // Extract outlineLvl (§17.3.1.20).
    let outline_lvl = find_w_child(ppr, "outlineLvl")
        .and_then(|el| attr_get(el, "w:val"))
        .and_then(|v| v.parse::<u8>().ok());

    let shading = extract_shading(ppr);

    Ok(RawParagraphProps {
        alignment,
        indent_left,
        indent_right,
        indent_first_line,
        indent_start_chars,
        indent_end_chars,
        indent_first_line_chars,
        indent_hanging_chars,
        spacing_before,
        spacing_after,
        spacing_before_lines,
        spacing_after_lines,
        spacing_before_autospacing,
        spacing_after_autospacing,
        spacing_line,
        spacing_line_rule,
        borders,
        num_props,
        num_ilvl,
        contextual_spacing,
        widow_control,
        keep_next,
        keep_lines,
        page_break_before,
        outline_lvl,
        shading,
    })
}

/// The level of a paragraph whose direct `w:numPr` names a list but writes no
/// `w:ilvl` — the one place this rule lives.
///
/// Word (16, observed): the level the paragraph style's basedOn chain gives
/// (`omitted_level_along_chain`), even when the style names a different list
/// or none; else the docDefaults level; else level 0. Word renders that level
/// and writes it into the live `w:numPr` on save.
pub fn resolve_omitted_direct_level(
    style_defs: Option<&StyleDefinitions>,
    style_id: Option<&str>,
) -> u32 {
    style_defs
        .and_then(|defs| {
            style_id
                .and_then(|id| defs.para_props.get(id))
                .and_then(|props| props.num_ilvl)
                .or(defs.ppr_defaults.num_ilvl)
        })
        .unwrap_or(0)
}

/// Resolve a paragraph's effective numbering from its direct `w:numPr` when
/// the document may have no styles part. Without styles there is no style
/// level, so a list without a level is at level 0.
pub fn resolve_effective_direct_num_props(
    style_defs: Option<&StyleDefinitions>,
    style_id: Option<&str>,
    direct: &DirectNumPr,
) -> DirectNumPr {
    match style_defs {
        Some(defs) => defs.resolve_effective_num_props(style_id, direct),
        None => match direct {
            DirectNumPr::ListWithoutLevel { num_id } => DirectNumPr::Active(NumProps {
                num_id: *num_id,
                ilvl: resolve_omitted_direct_level(None, style_id),
            }),
            DirectNumPr::Absent | DirectNumPr::Suppressed | DirectNumPr::Active(_) => {
                direct.clone()
            }
        },
    }
}

/// The `w:ilvl` written in a style's `w:numPr`, with or without a `w:numId`.
fn extract_style_num_ilvl(ppr: &Element) -> Result<Option<u32>, String> {
    let Some(num_pr) = find_w_child(ppr, "numPr") else {
        return Ok(None);
    };
    let Some(ilvl) = find_w_child(num_pr, "ilvl") else {
        return Ok(None);
    };
    let raw = attr_get(ilvl, "w:val").ok_or("pPr/numPr/ilvl has no w:val")?;
    raw.parse::<u32>()
        .map(Some)
        .map_err(|_| format!("pPr/numPr/ilvl w:val {raw:?} is not a level number"))
}

/// Extract numbering properties (numId + ilvl) from a w:pPr/w:numPr element in a style definition.
fn extract_style_num_props(ppr: &Element) -> Option<DirectNumPr> {
    let num_pr = find_w_child(ppr, "numPr")?;
    let num_id_elem = find_w_child(num_pr, "numId")?;
    let num_id: u32 = attr_get(num_id_elem, "w:val")?.parse().ok()?;

    if num_id == 0 {
        return Some(DirectNumPr::Suppressed);
    }

    let ilvl = find_w_child(num_pr, "ilvl")
        .and_then(|el| attr_get(el, "w:val"))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    Some(DirectNumPr::Active(NumProps { num_id, ilvl }))
}

/// Extract paragraph borders from a w:pPr/w:pBdr element in a style definition.
fn extract_ppr_borders(ppr: &Element) -> Option<ParagraphBorderProps> {
    let pbdr = find_w_child(ppr, "pBdr")?;

    let top = extract_style_border_edge(pbdr, "top");
    let bottom = extract_style_border_edge(pbdr, "bottom");
    let left = extract_style_border_edge(pbdr, "left")
        .or_else(|| extract_style_border_edge(pbdr, "start"));
    let right =
        extract_style_border_edge(pbdr, "right").or_else(|| extract_style_border_edge(pbdr, "end"));
    let between = extract_style_border_edge(pbdr, "between");
    let bar = extract_style_border_edge(pbdr, "bar");

    if top.is_some()
        || bottom.is_some()
        || left.is_some()
        || right.is_some()
        || between.is_some()
        || bar.is_some()
    {
        Some(ParagraphBorderProps {
            top,
            bottom,
            left,
            right,
            between,
            bar,
        })
    } else {
        None
    }
}

/// Extract a single border edge from a style border container.
fn extract_style_border_edge(container: &Element, edge_name: &str) -> Option<BorderEdge> {
    let edge = find_w_child(container, edge_name)?;
    let style = attr_get(edge, "w:val")
        .cloned()
        .unwrap_or_else(|| "none".to_string());
    let color = attr_get(edge, "w:color").cloned();
    let size = attr_get(edge, "w:sz").and_then(|v| v.parse().ok());
    let space = attr_get(edge, "w:space").and_then(|v| v.parse().ok());
    Some(BorderEdge {
        style,
        color,
        size,
        space,
        extra_attrs: crate::xml_attrs::capture_extra_attrs(edge, &["val", "color", "sz", "space"]),
    })
}

/// The level a direct list reference without a level takes from its style's
/// basedOn chain (`chain` is child first); `None` when no style in the chain
/// writes a `w:ilvl`.
///
/// Word 16, observed: the nearest `w:ilvl` along the chain applies, whether
/// the style writing it names a list (`w:numId`) or not, and a style whose
/// numPr writes only `w:numId` passes the level through from its parents.
/// One exception was observed and is kept as observed: when the nearest
/// `w:ilvl` sits in a style that names no list while a style above it does,
/// Word ignores it and the paragraph is at level 0.
fn omitted_level_along_chain(chain: &[&RawParagraphProps]) -> Option<u32> {
    let (index, layer) = chain
        .iter()
        .enumerate()
        .find(|(_, layer)| layer.num_ilvl.is_some())?;
    let names_no_list = layer.num_props.is_none();
    let list_named_above = chain[index + 1..]
        .iter()
        .any(|ancestor| ancestor.num_props.is_some());
    if names_no_list && list_named_above {
        Some(0)
    } else {
        layer.num_ilvl
    }
}

/// Resolve paragraph properties through a style's basedOn chain.
/// Per-field most-specific-wins (first non-None walking child → parent).
fn resolve_para_props_chain(
    style_id: &str,
    raw_styles: &HashMap<String, RawStyle>,
) -> RawParagraphProps {
    let origin_type = raw_styles.get(style_id).map(|r| r.style_type.as_str());
    let mut chain: Vec<&RawParagraphProps> = Vec::new();
    let mut current = Some(style_id.to_string());
    let mut visited = std::collections::HashSet::new();

    while let Some(ref id) = current {
        if !visited.insert(id.clone()) {
            break; // cycle guard
        }
        if let Some(raw) = raw_styles.get(id) {
            if !chain.is_empty() && origin_type != Some(raw.style_type.as_str()) {
                break; // cross-type basedOn — stop walking per §17.7.4.3
            }
            chain.push(&raw.raw_para_props);
            current = raw.based_on.clone();
        } else {
            break;
        }
    }

    // Walk child-first: first non-None wins for each field.
    let mut resolved = RawParagraphProps {
        num_ilvl: omitted_level_along_chain(&chain),
        ..Default::default()
    };
    for layer in &chain {
        if resolved.alignment.is_none() {
            resolved.alignment = layer.alignment.clone();
        }
        if resolved.indent_left.is_none() {
            resolved.indent_left = layer.indent_left;
        }
        if resolved.indent_right.is_none() {
            resolved.indent_right = layer.indent_right;
        }
        if resolved.indent_first_line.is_none() {
            resolved.indent_first_line = layer.indent_first_line;
        }
        if resolved.indent_start_chars.is_none() {
            resolved.indent_start_chars = layer.indent_start_chars;
        }
        if resolved.indent_end_chars.is_none() {
            resolved.indent_end_chars = layer.indent_end_chars;
        }
        if resolved.indent_first_line_chars.is_none() {
            resolved.indent_first_line_chars = layer.indent_first_line_chars;
        }
        if resolved.indent_hanging_chars.is_none() {
            resolved.indent_hanging_chars = layer.indent_hanging_chars;
        }
        // Spacing: per-field inheritance (same as indentation).
        if resolved.spacing_before.is_none() {
            resolved.spacing_before = layer.spacing_before;
        }
        if resolved.spacing_after.is_none() {
            resolved.spacing_after = layer.spacing_after;
        }
        if resolved.spacing_before_lines.is_none() {
            resolved.spacing_before_lines = layer.spacing_before_lines;
        }
        if resolved.spacing_after_lines.is_none() {
            resolved.spacing_after_lines = layer.spacing_after_lines;
        }
        if resolved.spacing_before_autospacing.is_none() {
            resolved.spacing_before_autospacing = layer.spacing_before_autospacing;
        }
        if resolved.spacing_after_autospacing.is_none() {
            resolved.spacing_after_autospacing = layer.spacing_after_autospacing;
        }
        if resolved.spacing_line.is_none() {
            if let Some(line) = layer.spacing_line {
                resolved.spacing_line = Some(line);
                // §17.3.1.33: "If [lineRule] is omitted, then it shall be
                // assumed to be of a value auto if a line attribute value is
                // present."  When this layer sets `line` but omits `lineRule`,
                // default to "auto" immediately so that a parent layer's
                // lineRule does not leak through the per-field merge.
                if resolved.spacing_line_rule.is_none() {
                    resolved.spacing_line_rule = Some(
                        layer
                            .spacing_line_rule
                            .clone()
                            .unwrap_or_else(|| "auto".to_string()),
                    );
                }
            }
        } else if resolved.spacing_line_rule.is_none() {
            resolved.spacing_line_rule = layer.spacing_line_rule.clone();
        }
        // Borders: whole-object replacement (first non-None wins).
        if resolved.borders.is_none() {
            resolved.borders = layer.borders.clone();
        }
        // numPr: whole-object replacement (first non-None wins).
        if resolved.num_props.is_none() {
            resolved.num_props = layer.num_props.clone();
        }
        // contextualSpacing: first non-None wins.
        if resolved.contextual_spacing.is_none() {
            resolved.contextual_spacing = layer.contextual_spacing;
        }
        // widowControl: first non-None wins (§17.3.1.44).
        if resolved.widow_control.is_none() {
            resolved.widow_control = layer.widow_control;
        }
        // keepNext: first non-None wins (§17.3.1.15).
        if resolved.keep_next.is_none() {
            resolved.keep_next = layer.keep_next;
        }
        // keepLines: first non-None wins (§17.3.1.14).
        if resolved.keep_lines.is_none() {
            resolved.keep_lines = layer.keep_lines;
        }
        // pageBreakBefore: first non-None wins (§17.3.1.23).
        if resolved.page_break_before.is_none() {
            resolved.page_break_before = layer.page_break_before;
        }
        // outlineLvl: first non-None wins (§17.3.1.20).
        if resolved.outline_lvl.is_none() {
            resolved.outline_lvl = layer.outline_lvl;
        }
        // shading: first non-None wins (§17.3.1.31).
        if resolved.shading.is_none() {
            resolved.shading = layer.shading.clone();
        }
    }

    resolved
}

// =============================================================================
// Paragraph property (tab stop) resolution
// =============================================================================

/// Parse tab stops from a w:pPr element within a style definition.
/// Returns None if no w:tabs element is present (meaning "inherit from parent").
/// Returns Some (possibly empty) if w:tabs is present (explicit override).
///
/// A position is read as a paragraph's own tab position is
/// (`word_ir::tab_position_twips`); one with no position is refused, as Word
/// refuses it on a paragraph.
fn parse_ppr_tab_stops(ppr: &Element) -> Result<Option<Vec<TabStopDef>>, String> {
    let Some(tabs_el) = find_w_child(ppr, "tabs") else {
        return Ok(None);
    };
    let mut stops = Vec::new();
    for child in &tabs_el.children {
        let el = match child {
            XMLNode::Element(el) if is_w_tag(el, "tab") => el,
            _ => continue,
        };
        let raw_position = attr_get(el, "w:pos").ok_or("pPr/tabs: w:tab without w:pos")?;
        let position = crate::word_ir::tab_position_twips(raw_position)
            .ok_or_else(|| format!("pPr/tabs: invalid w:tab/@w:pos value {raw_position:?}"))?;
        // w:val — parse tab alignment (ST_TabJc §17.18.81).
        // MS-OI29500 §17.18.84: "start" is an alias for "left", "end" for "right".
        let alignment = match attr_get(el, "w:val") {
            Some(v) => match v.as_str() {
                "start" => crate::domain::TabAlignment::Left,
                "end" => crate::domain::TabAlignment::Right,
                other => match crate::domain::TabAlignment::from_xml_str(other) {
                    Ok(a) => a,
                    Err(_) => crate::domain::TabAlignment::Left,
                },
            },
            None => crate::domain::TabAlignment::Left,
        };
        let leader = attr_get(el, "w:leader").and_then(|v| match v.as_str() {
            "none" => None,
            other => crate::domain::TabLeader::from_xml_str(other).ok(),
        });
        stops.push(TabStopDef {
            position,
            alignment,
            leader,
        });
    }
    Ok(Some(stops))
}

/// Resolve tab stops through a style's basedOn chain.
///
/// Per OOXML: if a child style's `<w:tabs>` is **present** (even empty), it's an explicit
/// override — "clear" entries remove inherited stops, non-"clear" entries add/replace.
/// If `<w:tabs>` is **absent** (None), the child inherits the parent's tabs unchanged.
///
/// Returns the final effective list: no "clear" entries, de-duped by position,
/// sorted ascending.
fn resolve_tab_stop_chain(
    style_id: &str,
    raw_styles: &HashMap<String, RawStyle>,
) -> Vec<TabStopDef> {
    // Walk the basedOn chain collecting raw pPr tab layers (child first).
    let origin_type = raw_styles.get(style_id).map(|r| r.style_type.as_str());
    let mut chain: Vec<&Option<Vec<TabStopDef>>> = Vec::new();
    let mut current = Some(style_id.to_string());
    let mut visited = std::collections::HashSet::new();

    while let Some(ref id) = current {
        if !visited.insert(id.clone()) {
            break; // cycle guard
        }
        if let Some(raw) = raw_styles.get(id) {
            if !chain.is_empty() && origin_type != Some(raw.style_type.as_str()) {
                break; // cross-type basedOn — stop walking per §17.7.4.3
            }
            chain.push(&raw.raw_tab_stops);
            current = raw.based_on.clone();
        } else {
            break;
        }
    }

    // Start from empty (document defaults have no tab stops in this model)
    // and overlay each layer parent-first.
    let mut effective: Vec<TabStopDef> = Vec::new();
    for stops in chain.iter().rev().filter_map(|layer| layer.as_ref()) {
        overlay_tab_stops(&mut effective, stops);
    }

    effective
}

/// Overlay a layer of tab stops onto the effective list.
///
/// - "clear" entries remove any stop at the matching position.
/// - Non-"clear" entries add or replace at their position.
/// - Final result is de-duped by position and sorted ascending.
fn overlay_tab_stops(effective: &mut Vec<TabStopDef>, layer: &[TabStopDef]) {
    for stop in layer {
        if stop.alignment == crate::domain::TabAlignment::Clear {
            effective.retain(|s| s.position != stop.position);
        } else {
            // Replace if same position exists, otherwise add.
            if let Some(existing) = effective.iter_mut().find(|s| s.position == stop.position) {
                *existing = stop.clone();
            } else {
                effective.push(stop.clone());
            }
        }
    }
    // Sort ascending by position.
    effective.sort_by_key(|s| s.position);
}

// =============================================================================
// Table style property parsing + resolution
// =============================================================================

/// Extract raw table style properties from a `<w:style w:type="table">` element.
///
/// Reads:
/// - `w:tblPr/w:tblBorders` → borders
/// - `w:tblPr/w:tblCellMar` → default cell margins
/// - `w:tcPr/w:shd` → default cell shading
/// - `w:tblStylePr` → conditional formatting overrides
fn extract_raw_table_style_props(style_el: &Element) -> Result<RawTableStyleProps, String> {
    let mut props = RawTableStyleProps::default();
    let tr_pr_hidden = |el: &Element| {
        find_w_child(el, "trPr").is_some_and(|tr_pr| find_w_child(tr_pr, "hidden").is_some())
    };
    props.declares_row_hidden = tr_pr_hidden(style_el)
        || style_el.children.iter().any(|child| {
            matches!(child, XMLNode::Element(e) if e.name == "tblStylePr" && tr_pr_hidden(e))
        });
    let shaded = |el: &Element| {
        ["tblPr", "tcPr"].iter().any(|props| {
            find_w_child(el, props).is_some_and(|props| find_w_child(props, "shd").is_some())
        })
    };
    props.declares_shading = shaded(style_el)
        || style_el.children.iter().any(
            |child| matches!(child, XMLNode::Element(e) if e.name == "tblStylePr" && shaded(e)),
        );
    let has_rpr = |el: &Element| find_w_child(el, "rPr").is_some();
    props.declares_run_properties = has_rpr(style_el)
        || style_el.children.iter().any(
            |child| matches!(child, XMLNode::Element(e) if e.name == "tblStylePr" && has_rpr(e)),
        );

    // Table-level properties from w:tblPr.
    if let Some(tbl_pr) = find_w_child(style_el, "tblPr") {
        props.borders = extract_table_borders(tbl_pr);
        // Read as a table's own w:tblCellMar is read.
        props.default_cell_margins = crate::import::parse_cell_margins(tbl_pr, "tblCellMar");
        // Table alignment from w:jc (§17.4.28).
        props.alignment = find_w_child(tbl_pr, "jc")
            .and_then(|jc| attr_get(jc, "w:val"))
            .and_then(|v| match v.as_str() {
                "left" | "start" => Some(Alignment::Left),
                "center" => Some(Alignment::Center),
                "right" | "end" => Some(Alignment::Right),
                _ => None,
            });
        // Table indent from w:tblInd (§17.4.51). w:w is ST_MeasurementOrPercent
        // via CT_TblWidth — plain numbers and universal measures resolve to
        // twips; percent/invalid forms have no twips meaning and are dropped
        // (same observable boundary as the document-level tblInd parse).
        props.indent = find_w_child(tbl_pr, "tblInd")
            .and_then(|ind| attr_get(ind, "w:w"))
            .and_then(|v| {
                use crate::import::MeasurementOrPercent;
                let twips =
                    match crate::import::parse_measurement_or_percent(v, "style tblInd element") {
                        Ok(MeasurementOrPercent::Number(n)) => n,
                        Ok(MeasurementOrPercent::UniversalTwips(t)) => t,
                        // Word's reading of a tblInd with no number (verified
                        // on a table's own tblInd): no indent.
                        Ok(MeasurementOrPercent::NotANumber) => return None,
                        Ok(MeasurementOrPercent::Percent { .. }) | Err(_) => {
                            tracing::warn!(
                                value = %v,
                                "style tblInd w:w is not storable as twips (percent or invalid \
                                 form); dropping the style indent"
                            );
                            return None;
                        }
                    };
                i32::try_from(twips).ok()
            });
        // Band sizes from w:tblStyleRowBandSize (§17.4.79) and w:tblStyleColBandSize (§17.4.78).
        if let Some(rbs) = find_w_child(tbl_pr, "tblStyleRowBandSize")
            && let Some(val) = attr_get(rbs, "w:val").and_then(|v| v.parse::<u32>().ok())
        {
            props.row_band_size = val;
        }
        if let Some(cbs) = find_w_child(tbl_pr, "tblStyleColBandSize")
            && let Some(val) = attr_get(cbs, "w:val").and_then(|v| v.parse::<u32>().ok())
        {
            props.col_band_size = val;
        }
    }

    // Default cell properties from w:tcPr (style-level, applies to all cells).
    if let Some(tc_pr) = find_w_child(style_el, "tcPr") {
        props.default_cell_shading = extract_shading(tc_pr);
    }

    // Base paragraph alignment from root-level w:pPr/w:jc (MS-DOCX §2.3.1).
    // This is separate from tblStylePr conditionals — it sets the table style's
    // default paragraph alignment for the compat setting gate.
    if let Some(ppr) = find_w_child(style_el, "pPr") {
        props.base_para_alignment = find_w_child(ppr, "jc")
            .and_then(|jc| attr_get(jc, "w:val"))
            .and_then(|v| match v.as_str() {
                "left" | "start" => Some(Alignment::Left),
                "center" => Some(Alignment::Center),
                "right" | "end" => Some(Alignment::Right),
                "both" | "justify" => Some(Alignment::Justify),
                _ => None,
            });
    }

    // Base run properties from root-level w:rPr (MS-DOCX §2.3.1).
    // font_size and alignment are gated by overrideTableStyleFontSizeAndJustification;
    // bold, color, and font_family always apply as table-style defaults.
    if let Some(rpr) = find_w_child(style_el, "rPr") {
        if let Some(sz_el) = find_w_child(rpr, "sz") {
            props.base_font_size = style_half_points(sz_el, "sz")?;
        }
        if let Some(b_el) = find_w_child(rpr, "b") {
            props.base_bold = Some(parse_toggle_value(b_el) == MarkValue::On);
        }
        if let Some(rfonts) = find_w_child(rpr, "rFonts") {
            props.base_font_family = attr_get(rfonts, "w:ascii")
                .or_else(|| attr_get(rfonts, "w:hAnsi"))
                .map(|s| IStr::from(s.as_str()));
        }
        if let Some(color_el) = find_w_child(rpr, "color") {
            props.base_color = attr_get(color_el, "w:val").map(|s| IStr::from(s.as_str()));
        }
    }

    // Conditional formatting overrides from w:tblStylePr children.
    for child in &style_el.children {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };
        if !is_w_tag(el, "tblStylePr") {
            continue;
        }
        let type_str = match attr_get(el, "w:type") {
            Some(t) => t.as_str(),
            None => continue,
        };
        let cond_type = match type_str {
            "firstRow" => TblStylePrType::FirstRow,
            "lastRow" => TblStylePrType::LastRow,
            "firstCol" => TblStylePrType::FirstCol,
            "lastCol" => TblStylePrType::LastCol,
            "band1Horz" => TblStylePrType::Band1Horz,
            "band2Horz" => TblStylePrType::Band2Horz,
            "band1Vert" => TblStylePrType::Band1Vert,
            "band2Vert" => TblStylePrType::Band2Vert,
            "wholeTable" => TblStylePrType::WholeTable,
            "nwCell" => TblStylePrType::NwCell,
            "neCell" => TblStylePrType::NeCell,
            "swCell" => TblStylePrType::SwCell,
            "seCell" => TblStylePrType::SeCell,
            _ => continue,
        };
        let mut cond_props = ConditionalCellProps::default();
        if let Some(tc_pr) = find_w_child(el, "tcPr") {
            cond_props.shading = extract_shading(tc_pr);
            cond_props.borders = extract_conditional_cell_borders(tc_pr);
            cond_props.margins = crate::import::parse_cell_margins(tc_pr, "tblCellMar");
        }
        // §17.7.6.1: paragraph properties from pPr
        if let Some(ppr) = find_w_child(el, "pPr") {
            cond_props.alignment = find_w_child(ppr, "jc")
                .and_then(|jc| attr_get(jc, "w:val"))
                .and_then(|v| match v.as_str() {
                    "left" | "start" => Some(Alignment::Left),
                    "center" => Some(Alignment::Center),
                    "right" | "end" => Some(Alignment::Right),
                    "both" | "justify" => Some(Alignment::Justify),
                    _ => None,
                });
        }
        // §17.7.6.2: run properties from rPr
        if let Some(rpr) = find_w_child(el, "rPr") {
            if let Some(b_el) = find_w_child(rpr, "b") {
                cond_props.bold = Some(parse_toggle_value(b_el) == MarkValue::On);
            }
            if let Some(sz_el) = find_w_child(rpr, "sz") {
                cond_props.font_size = style_half_points(sz_el, "sz")?;
            }
            if let Some(rfonts) = find_w_child(rpr, "rFonts") {
                // Per §17.3.2.26: ascii/hAnsi slot determines the base font family.
                cond_props.font_family = attr_get(rfonts, "w:ascii")
                    .or_else(|| attr_get(rfonts, "w:hAnsi"))
                    .map(|s| IStr::from(s.as_str()));
            }
            if let Some(color_el) = find_w_child(rpr, "color") {
                cond_props.color = attr_get(color_el, "w:val").map(|s| IStr::from(s.as_str()));
            }
        }
        let has_any = cond_props.shading.is_some()
            || cond_props.borders.is_some()
            || cond_props.margins.is_some()
            || cond_props.alignment.is_some()
            || cond_props.bold.is_some()
            || cond_props.font_size.is_some()
            || cond_props.font_family.is_some()
            || cond_props.color.is_some();
        if has_any {
            props.conditional.insert(cond_type, cond_props);
        }
    }

    Ok(props)
}

/// Extract cell borders from a w:tcPr element's w:tcBorders child (for conditional formatting).
fn extract_conditional_cell_borders(tc_pr: &Element) -> Option<BorderSet> {
    let tc_borders = find_w_child(tc_pr, "tcBorders")?;

    let top = extract_domain_border_edge(tc_borders, "top");
    let bottom = extract_domain_border_edge(tc_borders, "bottom");
    let left = extract_domain_border_edge(tc_borders, "left")
        .or_else(|| extract_domain_border_edge(tc_borders, "start"));
    let right = extract_domain_border_edge(tc_borders, "right")
        .or_else(|| extract_domain_border_edge(tc_borders, "end"));
    let inside_h = extract_domain_border_edge(tc_borders, "insideH");
    let inside_v = extract_domain_border_edge(tc_borders, "insideV");
    let tl2br = extract_domain_border_edge(tc_borders, "tl2br");
    let tr2bl = extract_domain_border_edge(tc_borders, "tr2bl");

    if top.is_some()
        || bottom.is_some()
        || left.is_some()
        || right.is_some()
        || inside_h.is_some()
        || inside_v.is_some()
        || tl2br.is_some()
        || tr2bl.is_some()
    {
        Some(BorderSet {
            top,
            bottom,
            left,
            right,
            inside_h,
            inside_v,
            tl2br,
            tr2bl,
        })
    } else {
        None
    }
}

/// Extract table borders from a w:tblPr element's w:tblBorders child.
fn extract_table_borders(tbl_pr: &Element) -> Option<BorderSet> {
    let tbl_borders = find_w_child(tbl_pr, "tblBorders")?;

    let top = extract_domain_border_edge(tbl_borders, "top");
    let bottom = extract_domain_border_edge(tbl_borders, "bottom");
    let left = extract_domain_border_edge(tbl_borders, "left")
        .or_else(|| extract_domain_border_edge(tbl_borders, "start"));
    let right = extract_domain_border_edge(tbl_borders, "right")
        .or_else(|| extract_domain_border_edge(tbl_borders, "end"));
    let inside_h = extract_domain_border_edge(tbl_borders, "insideH");
    let inside_v = extract_domain_border_edge(tbl_borders, "insideV");

    if top.is_some()
        || bottom.is_some()
        || left.is_some()
        || right.is_some()
        || inside_h.is_some()
        || inside_v.is_some()
    {
        Some(BorderSet {
            top,
            bottom,
            left,
            right,
            inside_h,
            inside_v,
            tl2br: None,
            tr2bl: None,
        })
    } else {
        None
    }
}

/// Extract a single border edge as a domain `Border`.
fn extract_domain_border_edge(container: &Element, edge_name: &str) -> Option<Border> {
    let edge = find_w_child(container, edge_name)?;
    let style_str = attr_get(edge, "w:val")
        .map(|s| s.as_str())
        .unwrap_or("none");
    let style = match crate::domain::BorderStyle::from_xml_str(style_str) {
        Ok(s) => s,
        Err(e) => {
            if crate::runtime::runtime_timing_logs_enabled() {
                eprintln!("extract_domain_border_edge: {e}, defaulting to None");
            }
            crate::domain::BorderStyle::None
        }
    };
    let color = attr_get(edge, "w:color").cloned();
    let size = attr_get(edge, "w:sz").and_then(|v| v.parse().ok());
    let space = attr_get(edge, "w:space").and_then(|v| v.parse().ok());
    Some(Border {
        style,
        color,
        size,
        space,
        extra_attrs: Vec::new(),
    })
}

/// Extract shading from a w:shd element inside a properties element.
fn extract_shading(props: &Element) -> Option<Shading> {
    let shd = find_w_child(props, "shd")?;
    let fill = attr_get(shd, "w:fill").cloned();
    let val =
        attr_get(shd, "w:val").and_then(|v| crate::domain::ShadingPattern::from_xml_str(v).ok());
    let color = attr_get(shd, "w:color").cloned();
    if fill.is_some() || val.is_some() || color.is_some() {
        Some(Shading {
            fill,
            val,
            color,
            extra_attrs: Vec::new(),
        })
    } else {
        None
    }
}

/// Whether any style in `style_id`'s table-style chain declares a row's
/// hidden state.
fn table_style_chain_declares_row_hidden(
    style_id: &str,
    raw_styles: &HashMap<String, RawStyle>,
) -> bool {
    let mut current = Some(style_id.to_string());
    let mut visited = std::collections::HashSet::new();
    while let Some(id) = current {
        if !visited.insert(id.clone()) {
            return false;
        }
        let Some(raw) = raw_styles.get(&id) else {
            return false;
        };
        if raw.style_type != "table" {
            return false;
        }
        if raw.raw_table_props.declares_row_hidden {
            return true;
        }
        current = raw.based_on.clone();
    }
    false
}

/// Whether any style in `style_id`'s table-style chain declares what
/// `declares_in` reads off its properties; `None` when the chain does not
/// resolve (a missing or non-table style, a cycle).
fn table_style_chain_declares(
    style_id: &str,
    raw_styles: &HashMap<String, RawStyle>,
    declares_in: impl Fn(&RawTableStyleProps) -> bool,
) -> Option<bool> {
    let mut current = Some(style_id.to_string());
    let mut visited = std::collections::HashSet::new();
    let mut declares = false;
    while let Some(id) = current {
        if !visited.insert(id.clone()) {
            return None;
        }
        let raw = raw_styles.get(&id)?;
        if raw.style_type != "table" {
            return None;
        }
        declares |= declares_in(&raw.raw_table_props);
        current = raw.based_on.clone();
    }
    Some(declares)
}

/// Resolve table style properties through a style's basedOn chain.
/// Per-field most-specific-wins (first non-None walking child → parent).
fn resolve_table_style_chain(
    style_id: &str,
    raw_styles: &HashMap<String, RawStyle>,
) -> TableStyleProps {
    let origin_type = raw_styles.get(style_id).map(|r| r.style_type.as_str());
    let mut chain: Vec<&RawTableStyleProps> = Vec::new();
    let mut current = Some(style_id.to_string());
    let mut visited = std::collections::HashSet::new();

    while let Some(ref id) = current {
        if !visited.insert(id.clone()) {
            break; // cycle guard
        }
        if let Some(raw) = raw_styles.get(id) {
            if !chain.is_empty() && origin_type != Some(raw.style_type.as_str()) {
                break; // cross-type basedOn — stop walking per §17.7.4.3
            }
            chain.push(&raw.raw_table_props);
            current = raw.based_on.clone();
        } else {
            break;
        }
    }

    // Walk child-first: first non-None wins for each field.
    let mut resolved = TableStyleProps::default();
    for layer in &chain {
        if resolved.borders.is_none() {
            resolved.borders = layer.borders.clone();
        }
        if resolved.default_cell_margins.is_none() {
            resolved.default_cell_margins = layer.default_cell_margins.clone();
        }
        if resolved.default_cell_shading.is_none() {
            resolved.default_cell_shading = layer.default_cell_shading.clone();
        }
        if resolved.alignment.is_none() {
            resolved.alignment = layer.alignment.clone();
        }
        if resolved.indent.is_none() {
            resolved.indent = layer.indent;
        }
        // Merge conditional map: child-first, so only insert if key not already present.
        for (cond_type, cond_props) in &layer.conditional {
            resolved
                .conditional
                .entry(cond_type.clone())
                .or_insert_with(|| cond_props.clone());
        }
        if resolved.row_band_size == 0 && layer.row_band_size > 0 {
            resolved.row_band_size = layer.row_band_size;
        }
        if resolved.col_band_size == 0 && layer.col_band_size > 0 {
            resolved.col_band_size = layer.col_band_size;
        }
        if resolved.base_para_alignment.is_none() {
            resolved.base_para_alignment = layer.base_para_alignment.clone();
        }
        if resolved.base_font_size.is_none() {
            resolved.base_font_size = layer.base_font_size;
        }
        if resolved.base_bold.is_none() {
            resolved.base_bold = layer.base_bold;
        }
        if resolved.base_color.is_none() {
            resolved.base_color = layer.base_color.clone();
        }
        if resolved.base_font_family.is_none() {
            resolved.base_font_family = layer.base_font_family.clone();
        }
    }

    // MS-OI29500 §17.7.6.5/§17.7.6.7: band size defaults to 0 (no banding)
    // when absent. The base spec says 1, but Word uses 0.
    // resolved.row_band_size and resolved.col_band_size stay 0 if never set.

    resolved
}

// =============================================================================
// Style collision detection
// =============================================================================

/// A style ID that exists in both base and target with different definitions.
///
/// Used during merge to warn about potential formatting mismatches when the
/// same style ID has diverging XML content across the two documents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StyleCollision {
    /// The w:styleId shared by both documents.
    pub style_id: String,
    /// The w:type of the style (e.g. "paragraph", "character", "table").
    pub style_type: String,
    /// The human-readable style name from `w:name w:val="..."`, if present.
    pub style_name: Option<String>,
}

/// Compare base and target `word/styles.xml` for style ID collisions.
///
/// Returns a list of [`StyleCollision`]s for style IDs that:
/// 1. Appear in both base and target styles.xml
/// 2. Have different XML definitions (compared by serialized string)
/// 3. Are active in either reading: explicitly referenced, declared as a
///    default style, or inherited through an active style's `w:basedOn` chain
///
/// Detection does not choose a winner. A package-composition caller must either
/// provide a native carrier for the differing active definition or refuse the
/// composition; silently choosing one side makes the other terminal wrong.
pub fn detect_style_collisions(
    base_styles_xml: &[u8],
    target_styles_xml: &[u8],
    referenced_style_ids: &HashSet<IStr>,
) -> Result<Vec<StyleCollision>, String> {
    if base_styles_xml.is_empty() || target_styles_xml.is_empty() {
        return Ok(Vec::new());
    }

    let base_root = crate::word_xml::parse_document_xml(base_styles_xml)
        .map_err(|error| format!("failed to parse base word/styles.xml: {error:?}"))?;
    let target_root = crate::word_xml::parse_document_xml(target_styles_xml)
        .map_err(|error| format!("failed to parse target word/styles.xml: {error:?}"))?;

    // Extract style elements from each document, keyed by styleId.
    let base_styles = extract_style_elements(&base_root);
    let target_styles = extract_style_elements(&target_root);

    let mut active_style_ids: HashSet<String> = referenced_style_ids
        .iter()
        .map(|style_id| style_id.to_string())
        .collect();

    // A styleless paragraph still consumes the default paragraph style. The
    // same is true for the default character and table styles on their
    // corresponding objects, so a differing default definition cannot be
    // dismissed merely because no explicit pStyle/rStyle/tblStyle names it.
    for styles in [&base_styles, &target_styles] {
        for (style_id, style) in styles {
            if attr_get(style, "w:default")
                .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "on"))
            {
                active_style_ids.insert(style_id.clone());
            }
        }
    }

    // A referenced/default style observes every definition in its basedOn
    // closure. Follow both input tables: the parent can itself differ between
    // the accepted readings even when the child style is byte-identical.
    let mut pending: Vec<String> = active_style_ids.iter().cloned().collect();
    while let Some(style_id) = pending.pop() {
        for styles in [&base_styles, &target_styles] {
            let Some(style) = styles.get(&style_id) else {
                continue;
            };
            let Some(parent) =
                find_w_child(style, "basedOn").and_then(|based_on| attr_get(based_on, "w:val"))
            else {
                continue;
            };
            if active_style_ids.insert(parent.clone()) {
                pending.push(parent.clone());
            }
        }
    }

    let mut collisions = Vec::new();

    for (style_id, base_el) in &base_styles {
        // Only check styles actually referenced by the document.
        if !active_style_ids.contains(style_id) {
            continue;
        }

        let target_el = match target_styles.get(style_id) {
            Some(el) => el,
            None => continue,
        };

        if !same_active_style_definition(base_el, target_el) {
            let style_type = attr_get(base_el, "w:type")
                .cloned()
                .unwrap_or_else(|| "unknown".to_string());
            let style_name = find_w_child(base_el, "name")
                .and_then(|name_el| attr_get(name_el, "w:val").cloned());
            collisions.push(StyleCollision {
                style_id: style_id.clone(),
                style_type,
                style_name,
            });
        }
    }

    // Sort for deterministic output.
    collisions.sort_by(|a, b| a.style_id.cmp(&b.style_id));
    Ok(collisions)
}

/// Whether the two style tables author the same document defaults.
///
/// `w:docDefaults` is document-global active state. Word has no tracked-change
/// carrier for switching the styles part between Accept and Reject, so package
/// composition uses this structural comparison before selecting one physical
/// styles part. Malformed inputs fail loudly instead of being treated as equal.
pub fn authored_doc_defaults_equal(
    base_styles_xml: &[u8],
    target_styles_xml: &[u8],
) -> Result<bool, String> {
    let base_root = crate::word_xml::parse_document_xml(base_styles_xml)
        .map_err(|error| format!("failed to parse base word/styles.xml: {error:?}"))?;
    let target_root = crate::word_xml::parse_document_xml(target_styles_xml)
        .map_err(|error| format!("failed to parse target word/styles.xml: {error:?}"))?;
    Ok(
        match (
            find_w_child(&base_root, "docDefaults"),
            find_w_child(&target_root, "docDefaults"),
        ) {
            (None, None) => true,
            (Some(base), Some(target)) => same_authored_xml(base, target),
            _ => false,
        },
    )
}

/// Extract `w:style` elements from a parsed styles.xml root, keyed by `w:styleId`.
fn extract_style_elements(root: &Element) -> HashMap<String, &Element> {
    let mut map = HashMap::new();
    for child in &root.children {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };
        if !is_w_tag(el, "style") {
            continue;
        }
        if let Some(style_id) = attr_get(el, "w:styleId") {
            map.insert(style_id.clone(), el);
        }
    }
    map
}

/// Whether two definitions of one style id mean the same style.
///
/// A direct `w:rsid` child records the editing session that last touched the
/// style. It does not participate in the style cascade and Word may rewrite it
/// on save, so it is source-owned provenance, not an active difference. Every
/// other authored child is compared by [`same_authored_xml`].
fn same_active_style_definition(base: &Element, target: &Element) -> bool {
    let without_rsid = |style: &Element| {
        let mut active = style.clone();
        active.children.retain(
            |child| !matches!(child, XMLNode::Element(element) if is_w_tag(element, "rsid")),
        );
        active
    };
    same_authored_xml(&without_rsid(base), &without_rsid(target))
}

/// Children of an element-only element, without ignorable whitespace text.
fn element_content(element: &Element) -> Vec<&XMLNode> {
    element
        .children
        .iter()
        .filter(
            |child| !matches!(child, XMLNode::Text(text) if text.chars().all(char::is_whitespace)),
        )
        .collect()
}

/// Whether two element-only style-table subtrees (`w:style`, `w:docDefaults`)
/// author the same content.
///
/// One spelling: these differences are XML spelling, not meaning, and compare
/// equal:
/// - attribute order (XML 1.0 §3.1: attributes are unordered);
/// - namespace prefixes and where `xmlns` is declared (Namespaces in XML §6:
///   names are identified by namespace URI and local name);
/// - whitespace-only text between children (`CT_Style` / `CT_DocDefaults` and
///   their descendants have element-only content, so it is ignorable).
///
/// Everything else is content and stays exact: element sequence, attribute
/// values, non-whitespace text, comments, CDATA, processing instructions.
fn same_authored_xml(base: &Element, target: &Element) -> bool {
    if base.namespace != target.namespace || base.name != target.name {
        return false;
    }
    if base.attributes.len() != target.attributes.len() {
        return false;
    }
    let attributes_match = base.attributes.iter().all(|(name, value)| {
        target.attributes.iter().any(|(other, other_value)| {
            other.namespace == name.namespace
                && other.local_name == name.local_name
                && other_value == value
        })
    });
    if !attributes_match {
        return false;
    }
    let base_children = element_content(base);
    let target_children = element_content(target);
    base_children.len() == target_children.len()
        && base_children
            .iter()
            .zip(&target_children)
            .all(|(left, right)| match (left, right) {
                (XMLNode::Element(left), XMLNode::Element(right)) => same_authored_xml(left, right),
                (left, right) => left == right,
            })
}

/// Merge two `word/styles.xml` parts, preferring target definitions on
/// collisions while retaining base-only styles still needed for deleted/base
/// content in the redline.
///
/// Strategy:
/// - Keep the target root-level metadata (`docDefaults`, `latentStyles`, etc.)
/// - Keep all target `w:style` definitions as-is
/// - Append any base-only `w:style` elements missing from the target
pub fn merge_styles_xml_preferring_target(
    base_styles_xml: &[u8],
    target_styles_xml: &[u8],
) -> Option<Vec<u8>> {
    let base_root = crate::word_xml::parse_document_xml(base_styles_xml).ok()?;
    let target_root = crate::word_xml::parse_document_xml(target_styles_xml).ok()?;

    let mut merged_root = target_root.clone();
    let target_style_ids: HashSet<String> =
        extract_style_elements(&target_root).into_keys().collect();

    for child in &base_root.children {
        let XMLNode::Element(el) = child else {
            continue;
        };
        if !is_w_tag(el, "style") {
            continue;
        }
        let Some(style_id) = attr_get(el, "w:styleId") else {
            continue;
        };
        if target_style_ids.contains(style_id) {
            continue;
        }
        merged_root.children.push(XMLNode::Element(el.clone()));
    }

    crate::word_xml::ensure_all_used_namespaces(&mut merged_root);
    let mut out = Vec::new();
    merged_root.write(&mut out).ok()?;
    Some(out)
}

// =============================================================================
// Faithful (UN-resolved) style-table projection
// =============================================================================
//
// `StyleDefinitions` (above) is the *resolved* model: every style's properties
// already incorporate its basedOn chain and the document defaults, because that
// is what the layout/rendering path needs. A reader that wants to know "what is
// authored where" — e.g. an agent about to globally re-skin body text — needs
// the OPPOSITE: each `w:style` exactly as it appears in `word/styles.xml`, with
// NO chain resolution, so it can see which style literally sets `w:ascii` vs
// inherits it. That is what this projection provides. It deliberately does NOT
// reuse `StyleDefinitions::parse`.

/// Document-default run properties (`w:docDefaults/w:rPrDefault/w:rPr`), as
/// authored — the formatting body text inherits when no style sets it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocDefaultRun {
    /// `w:rFonts` font family. When `font_family_is_theme` is true this is a
    /// theme reference token (e.g. `minorHAnsi`); otherwise a literal typeface.
    pub font_family: Option<String>,
    /// True when `font_family` came from a `*Theme` attribute
    /// (`w:asciiTheme`/`w:hAnsiTheme`) rather than a literal `w:ascii`/`w:hAnsi`.
    pub font_family_is_theme: bool,
    /// `w:sz` @val in half-points (24 = 12pt).
    pub font_size_half_points: Option<u32>,
}

/// One `w:style` as AUTHORED — no basedOn-chain resolution, no doc-default fold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StyleRow {
    /// `w:styleId` — the programmatic id.
    pub style_id: String,
    /// `w:name` @val — the human-visible name. `None` if the style omits it.
    pub name: Option<String>,
    /// Style family: "para", "char", "table", or "num" (`w:type`, defaulting to
    /// "para" per ISO 29500-1 §17.7.4.17 when the attribute is omitted).
    pub style_type: String,
    /// `w:basedOn` @val — the parent style id, if any.
    pub based_on: Option<String>,
    /// `w:rPr/w:rFonts` font family, as authored. Theme vs literal disambiguated
    /// by `font_family_is_theme`.
    pub font_family: Option<String>,
    /// True when `font_family` is a theme reference (`*Theme` attr).
    pub font_family_is_theme: bool,
    /// `w:rPr/w:sz` @val in half-points.
    pub font_size_half_points: Option<u32>,
    /// `w:rPr/w:color` @val (hex RGB, no leading '#').
    pub color: Option<String>,
    /// `w:rPr/w:b` toggle, as authored: `Some(true/false)` if present, `None` if
    /// absent (inherits).
    pub bold: Option<bool>,
    /// `w:default="1"` — this style is the family default.
    pub is_default: bool,
}

/// A faithful projection of `word/styles.xml` for reading the style table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StyleTableProjection {
    /// Document-default run properties (`w:docDefaults/w:rPrDefault/w:rPr`).
    pub doc_default: DocDefaultRun,
    /// The default paragraph style id (`w:type="paragraph" w:default="1"`).
    pub default_para_style_id: Option<String>,
    /// The default character style id (`w:type="character" w:default="1"`).
    pub default_char_style_id: Option<String>,
    /// Every `w:style`, in document order, as authored.
    pub styles: Vec<StyleRow>,
}

/// Project `word/styles.xml` into a faithful, UN-resolved view of the style
/// table (each `w:style` exactly as authored; doc defaults read straight from
/// `w:docDefaults`).
///
/// Fails loud (CLAUDE.md "no silent fallbacks"):
/// - malformed XML → `Err`
/// - a `w:style` lacking `w:styleId` (unaddressable) → `Err`
///
/// An ABSENT styles part is NOT this function's concern — the runtime maps an
/// absent `word/styles.xml` to `StyleTableProjection::default()` (the empty
/// table). Passing empty bytes here therefore errors, matching the rest of the
/// module: emptiness at this layer means "I was handed a broken part".
pub fn style_table_projection(xml_bytes: &[u8]) -> Result<StyleTableProjection, String> {
    if xml_bytes.is_empty() {
        return Err("word/styles.xml is empty".to_string());
    }
    let root = crate::word_xml::parse_document_xml(xml_bytes)
        .map_err(|err| format!("failed to parse word/styles.xml: {err:?}"))?;

    let doc_default = project_doc_default(&root)?;

    let mut styles = Vec::new();
    let mut default_para_style_id: Option<String> = None;
    let mut default_char_style_id: Option<String> = None;

    for child in &root.children {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };
        if !is_w_tag(el, "style") {
            continue;
        }
        // Fail loud: a w:style with no w:styleId is unaddressable.
        let style_id = attr_get(el, "w:styleId").cloned().ok_or_else(|| {
            "word/styles.xml contains a <w:style> with no w:styleId — refusing to project an \
             unaddressable style"
                .to_string()
        })?;

        // ISO 29500-1 §17.7.4.17: a type-omitted w:style defaults to paragraph.
        let style_type = match attr_get(el, "w:type").map(String::as_str) {
            Some("paragraph") | None => "para".to_string(),
            Some("character") => "char".to_string(),
            Some("table") => "table".to_string(),
            Some("numbering") => "num".to_string(),
            Some(other) => other.to_string(),
        };

        let is_default = matches!(
            attr_get(el, "w:default").map(String::as_str),
            Some("1") | Some("true")
        );
        // The *family* default tracks the raw w:type, not our projected token.
        let raw_type = attr_get(el, "w:type")
            .map(String::as_str)
            .unwrap_or("paragraph");
        if is_default && raw_type == "paragraph" && default_para_style_id.is_none() {
            default_para_style_id = Some(style_id.clone());
        }
        if is_default && raw_type == "character" && default_char_style_id.is_none() {
            default_char_style_id = Some(style_id.clone());
        }

        let name = find_w_child(el, "name").and_then(|el| attr_get(el, "w:val").cloned());
        let based_on = find_w_child(el, "basedOn").and_then(|el| attr_get(el, "w:val").cloned());

        let rpr = find_w_child(el, "rPr");
        let (font_family, font_family_is_theme) =
            rpr.and_then(find_rfonts_family).unwrap_or((None, false));
        let font_size_half_points = match rpr.and_then(|rpr| find_w_child(rpr, "sz")) {
            Some(sz) => style_half_points(sz, "sz")
                .map_err(|err| format!("word/styles.xml style '{style_id}': {err}"))?,
            None => None,
        };
        let color = rpr
            .and_then(|rpr| find_w_child(rpr, "color"))
            .and_then(|c| attr_get(c, "w:val").cloned());
        let bold = rpr
            .and_then(|rpr| find_w_child(rpr, "b"))
            .map(|b| matches!(parse_toggle_value(b), MarkValue::On));

        styles.push(StyleRow {
            style_id,
            name,
            style_type,
            based_on,
            font_family,
            font_family_is_theme,
            font_size_half_points,
            color,
            bold,
            is_default,
        });
    }

    Ok(StyleTableProjection {
        doc_default,
        default_para_style_id,
        default_char_style_id,
        styles,
    })
}

/// Read the doc-default run props straight from `w:docDefaults/w:rPrDefault/w:rPr`
/// (no resolution). Absent at any level → `DocDefaultRun::default()`.
fn project_doc_default(root: &Element) -> Result<DocDefaultRun, String> {
    let rpr = find_w_child(root, "docDefaults")
        .and_then(|dd| find_w_child(dd, "rPrDefault"))
        .and_then(|rd| find_w_child(rd, "rPr"));
    let rpr = match rpr {
        Some(rpr) => rpr,
        None => return Ok(DocDefaultRun::default()),
    };
    let (font_family, font_family_is_theme) = find_rfonts_family(rpr).unwrap_or((None, false));
    let font_size_half_points = match find_w_child(rpr, "sz") {
        Some(sz) => style_half_points(sz, "sz")
            .map_err(|err| format!("word/styles.xml docDefaults: {err}"))?,
        None => None,
    };
    Ok(DocDefaultRun {
        font_family,
        font_family_is_theme,
        font_size_half_points,
    })
}

/// Read a `w:rFonts`'s authored font family from an `rPr`, preferring a literal
/// `w:ascii`/`w:hAnsi` typeface over a `w:asciiTheme`/`w:hAnsiTheme` reference.
///
/// Returns `(family, is_theme)`. A literal typeface wins when both are present
/// (the literal is what actually renders). Returns `None` when there is no
/// `w:rFonts` or it carries no ascii/hAnsi family at all.
fn find_rfonts_family(rpr: &Element) -> Option<(Option<String>, bool)> {
    let rfonts = find_w_child(rpr, "rFonts")?;
    if let Some(literal) = attr_get(rfonts, "w:ascii").or_else(|| attr_get(rfonts, "w:hAnsi")) {
        return Some((Some(literal.clone()), false));
    }
    if let Some(theme) =
        attr_get(rfonts, "w:asciiTheme").or_else(|| attr_get(rfonts, "w:hAnsiTheme"))
    {
        return Some((Some(theme.clone()), true));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::AuthoredRFonts;
    use std::io::Cursor;

    fn serialize_element(el: &Element) -> String {
        let mut buf = Vec::new();
        el.write(&mut buf).expect("serialize test element");
        String::from_utf8(buf).expect("serialized XML is UTF-8")
    }

    fn make_styles_xml(content: &str) -> Vec<u8> {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
            <w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            {content}
            </w:styles>"#,
        )
        .into_bytes()
    }

    /// A style's font size is an ST_HpsMeasure (§17.3.2.38), read as a run's
    /// is (Word 16, verified): a decimal truncates toward zero ("21.6" is 21),
    /// a non-numeric value is 0 (Word saves 0 and renders its minimum size),
    /// never dropped so the size silently inherits. A negative size is refused
    /// naming the style.
    #[test]
    fn a_style_font_size_reads_as_word_reads_it() {
        let xml = make_styles_xml(
            r#"<w:docDefaults><w:rPrDefault><w:rPr><w:sz w:val="large"/></w:rPr></w:rPrDefault></w:docDefaults>
            <w:style w:type="paragraph" w:styleId="Body"><w:rPr><w:sz w:val="21.6"/><w:szCs w:val="x"/></w:rPr></w:style>
            <w:style w:type="table" w:styleId="Grid"><w:rPr><w:sz w:val="19.2"/></w:rPr><w:tblStylePr w:type="firstRow"><w:rPr><w:sz w:val="big"/></w:rPr></w:tblStylePr></w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).expect("Word opens these sizes");
        assert_eq!(defs.doc_defaults.font_size, Some(0));
        let body = defs.resolve(&TextMarks::default(), None, Some("Body"));
        assert_eq!((body.font_size, body.font_size_cs), (Some(21), Some(0)));
        let grid = defs.table_style("Grid").expect("table style");
        assert_eq!(grid.base_font_size, Some(19));
        let projection = style_table_projection(&xml).expect("projection");
        assert_eq!(projection.doc_default.font_size_half_points, Some(0));

        let negative = make_styles_xml(
            r#"<w:style w:type="character" w:styleId="Note"><w:rPr><w:sz w:val="-4"/></w:rPr></w:style>"#,
        );
        let error = match StyleDefinitions::parse(&negative) {
            Ok(_) => panic!("a negative size is refused"),
            Err(error) => error,
        };
        assert!(
            error.contains("style 'Note'") && error.contains("sz"),
            "{error}"
        );
    }

    /// A universal measure is a valid ST_HpsMeasure: "10.5pt" is 21
    /// half-points in a style as in a run.
    #[test]
    fn a_style_font_size_in_points_reads_as_half_points() {
        let xml = make_styles_xml(
            r#"<w:docDefaults><w:rPrDefault><w:rPr><w:sz w:val="10.5pt"/></w:rPr></w:rPrDefault></w:docDefaults>
            <w:style w:type="table" w:styleId="Grid"><w:rPr><w:sz w:val="9pt"/></w:rPr><w:tblStylePr w:type="firstRow"><w:rPr><w:sz w:val="12pt"/></w:rPr></w:tblStylePr></w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).expect("valid sizes");
        assert_eq!(defs.doc_defaults.font_size, Some(21));
        let grid = defs.table_style("Grid").expect("table style");
        assert_eq!(grid.base_font_size, Some(18));
        let projection = style_table_projection(&xml).expect("projection");
        assert_eq!(projection.doc_default.font_size_half_points, Some(21));
        let grid_row = projection
            .styles
            .iter()
            .find(|row| row.style_id == "Grid")
            .expect("Grid row");
        assert_eq!(grid_row.font_size_half_points, Some(18));
    }

    #[test]
    fn complex_script_language_cascades_independently_and_retains_directness() {
        let xml = make_styles_xml(
            r#"
            <w:docDefaults><w:rPrDefault><w:rPr><w:lang w:val="en-GB" w:eastAsia="ja-JP" w:bidi="ar-SA"/></w:rPr></w:rPrDefault></w:docDefaults>
            <w:style w:type="paragraph" w:styleId="P"><w:rPr><w:lang w:bidi="ar-EG"/></w:rPr></w:style>
            <w:style w:type="character" w:styleId="Base"><w:rPr><w:lang w:bidi="fa-IR"/></w:rPr></w:style>
            <w:style w:type="character" w:styleId="C"><w:basedOn w:val="Base"/><w:rPr><w:lang w:val="de-DE"/></w:rPr></w:style>
        "#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        let direct = TextMarks {
            lang: Some(IStr::from("fr-FR")),
            ..TextMarks::default()
        };
        assert_eq!(
            defs.resolve(&direct, None, None).lang_bidi.as_deref(),
            Some("ar-SA")
        );
        assert_eq!(
            defs.resolve(&direct, None, Some("P")).lang_bidi.as_deref(),
            Some("ar-EG")
        );
        let inherited = defs.resolve(&direct, Some("C"), Some("P"));
        assert_eq!(inherited.lang_bidi.as_deref(), Some("fa-IR"));
        assert_eq!(inherited.lang.as_deref(), Some("fr-FR"));
        assert_eq!(inherited.lang_east_asia.as_deref(), Some("ja-JP"));
        let direct = TextMarks {
            lang_bidi: Some(IStr::from("ur-PK")),
            ..direct
        };
        assert_eq!(
            defs.resolve(&direct, Some("C"), Some("P"))
                .lang_bidi
                .as_deref(),
            Some("ur-PK")
        );
        let props = crate::domain::StyleProps {
            lang_bidi: Some(IStr::from("ur-PK")),
            ..Default::default()
        };
        assert!(
            !props.is_empty(),
            "complex-script language is a real formatting property"
        );
        let inherited = crate::domain::RunRprAuthored::default().direct_style_props(&props);
        assert!(
            inherited.lang_bidi.is_none(),
            "inherited language must not become a direct run override"
        );
        let authored = crate::domain::RunRprAuthored {
            lang_bidi: true,
            ..Default::default()
        }
        .direct_style_props(&props);
        assert_eq!(authored.lang_bidi.as_deref(), Some("ur-PK"));
    }

    #[test]
    fn style_num_id_zero_preserves_suppression() {
        let xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="SuppressedList">
                <w:pPr><w:numPr><w:numId w:val="0"/></w:numPr></w:pPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).expect("parse styles");

        assert_eq!(
            defs.resolve_effective_num_props(Some("SuppressedList"), &DirectNumPr::Absent),
            DirectNumPr::Suppressed
        );
    }

    #[test]
    fn style_suppression_blocks_numbering_from_document_defaults() {
        let xml = make_styles_xml(
            r#"<w:docDefaults><w:pPrDefault><w:pPr>
                <w:numPr><w:ilvl w:val="2"/><w:numId w:val="3"/></w:numPr>
            </w:pPr></w:pPrDefault></w:docDefaults>
            <w:style w:type="paragraph" w:styleId="SuppressedList">
                <w:pPr><w:numPr><w:numId w:val="0"/></w:numPr></w:pPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).expect("parse styles");

        assert_eq!(
            defs.resolve_effective_num_props(None, &DirectNumPr::Absent),
            DirectNumPr::Active(NumProps { num_id: 3, ilvl: 2 })
        );
        assert_eq!(
            defs.resolve_effective_num_props(Some("SuppressedList"), &DirectNumPr::Absent),
            DirectNumPr::Suppressed
        );
    }

    #[test]
    fn doc_defaults_cstheme_reaches_the_effective_cs_font() {
        // The normative attribute is lowercase-t "cstheme" (§17.3.2.26) — the
        // lone exception among the theme slots. With no theme part loaded the
        // resolver applies the MS-OI29500 §17.3.2.26c fallback, so the theme
        // reference surviving the PARSE is observable as font_cs becoming
        // "Times New Roman" rather than staying empty.
        let xml = make_styles_xml(
            r#"<w:docDefaults><w:rPrDefault><w:rPr>
                <w:rFonts w:asciiTheme="minorHAnsi" w:cstheme="minorBidi"/>
            </w:rPr></w:rPrDefault></w:docDefaults>"#,
        );
        let defs = StyleDefinitions::parse(&xml).expect("parse styles");
        let resolved = defs.resolve(&TextMarks::default(), None, None);
        assert_eq!(
            resolved.font_cs_theme.as_deref(),
            Some("minorBidi"),
            "docDefaults' lowercase cstheme attribute must survive the parse"
        );
        assert_eq!(
            resolved.font_cs.as_deref(),
            Some("Times New Roman"),
            "and resolve into the effective cs slot (empty-theme fallback)"
        );
    }

    #[test]
    fn direct_latin_rfonts_replaces_inherited_doc_default_theme_selector() {
        let xml = make_styles_xml(
            r#"<w:docDefaults><w:rPrDefault><w:rPr>
                <w:rFonts w:asciiTheme="minorHAnsi" w:hAnsiTheme="minorHAnsi"/>
            </w:rPr></w:rPrDefault></w:docDefaults>"#,
        );
        let defs = StyleDefinitions::parse(&xml).expect("parse styles");
        let direct = TextMarks {
            font_family: Some(IStr::from("Courier New")),
            authored_rfonts: AuthoredRFonts {
                ascii: Some(IStr::from("Courier New")),
                h_ansi: Some(IStr::from("Courier New")),
                ..AuthoredRFonts::default()
            },
            ..TextMarks::default()
        };

        let resolved = defs.resolve(&direct, None, None);

        assert_eq!(resolved.font_family.as_deref(), Some("Courier New"));
        assert_eq!(resolved.font_family_theme, None);
    }

    #[test]
    fn paragraph_style_latin_rfonts_replaces_doc_default_theme_selector() {
        let xml = make_styles_xml(
            r#"<w:docDefaults><w:rPrDefault><w:rPr>
                <w:rFonts w:asciiTheme="minorHAnsi" w:hAnsiTheme="minorHAnsi"/>
            </w:rPr></w:rPrDefault></w:docDefaults>
            <w:style w:type="paragraph" w:default="1" w:styleId="Normal">
                <w:rPr><w:rFonts w:ascii="Courier New" w:hAnsi="Courier New"/></w:rPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).expect("parse styles");

        let resolved = defs.resolve(&TextMarks::default(), None, Some("Normal"));

        assert_eq!(resolved.font_family.as_deref(), Some("Courier New"));
        assert_eq!(resolved.font_family_theme, None);
    }

    #[test]
    fn direct_latin_theme_selector_is_retained_when_same_rfonts_authors_it() {
        let xml = make_styles_xml("");
        let mut defs = StyleDefinitions::parse(&xml).expect("parse styles");
        defs.set_theme_fonts(ThemeFonts {
            minor_latin: Some("Aptos".to_string()),
            ..ThemeFonts::default()
        });
        let direct = TextMarks {
            font_family: Some(IStr::from("Courier New")),
            font_family_theme: Some(IStr::from("minorHAnsi")),
            authored_rfonts: AuthoredRFonts {
                ascii: Some(IStr::from("Courier New")),
                h_ansi: Some(IStr::from("Courier New")),
                ascii_theme: Some(IStr::from("minorHAnsi")),
                h_ansi_theme: Some(IStr::from("minorHAnsi")),
                ..AuthoredRFonts::default()
            },
            ..TextMarks::default()
        };

        let resolved = defs.resolve(&direct, None, None);

        assert_eq!(resolved.font_family.as_deref(), Some("Aptos"));
        assert_eq!(resolved.font_family_theme.as_deref(), Some("minorHAnsi"));
    }

    #[test]
    fn direct_latin_theme_rfonts_replaces_inherited_doc_default_literals() {
        let xml = make_styles_xml(
            r#"<w:docDefaults><w:rPrDefault><w:rPr>
                <w:rFonts w:ascii="Arial" w:hAnsi="Arial"/>
            </w:rPr></w:rPrDefault></w:docDefaults>"#,
        );
        let mut defs = StyleDefinitions::parse(&xml).expect("parse styles");
        defs.set_theme_fonts(ThemeFonts {
            major_latin: Some("Courier New".to_string()),
            ..ThemeFonts::default()
        });
        let direct = TextMarks {
            font_family_theme: Some(IStr::from("majorHAnsi")),
            authored_rfonts: AuthoredRFonts {
                ascii_theme: Some(IStr::from("majorHAnsi")),
                h_ansi_theme: Some(IStr::from("majorHAnsi")),
                ..AuthoredRFonts::default()
            },
            ..TextMarks::default()
        };

        let resolved = defs.resolve(&direct, None, None);

        assert_eq!(resolved.font_family.as_deref(), Some("Courier New"));
        assert_eq!(resolved.font_family_theme.as_deref(), Some("majorHAnsi"));
    }

    #[test]
    fn direct_script_rfonts_replace_corresponding_inherited_theme_selectors() {
        let xml = make_styles_xml(
            r#"<w:docDefaults><w:rPrDefault><w:rPr>
                <w:rFonts w:eastAsiaTheme="majorEastAsia" w:cstheme="majorBidi"/>
            </w:rPr></w:rPrDefault></w:docDefaults>"#,
        );
        let defs = StyleDefinitions::parse(&xml).expect("parse styles");
        let direct = TextMarks {
            font_east_asia: Some(IStr::from("MS Gothic")),
            font_cs: Some(IStr::from("Arial")),
            authored_rfonts: AuthoredRFonts {
                east_asia: Some(IStr::from("MS Gothic")),
                cs: Some(IStr::from("Arial")),
                ..AuthoredRFonts::default()
            },
            ..TextMarks::default()
        };

        let resolved = defs.resolve(&direct, None, None);

        assert_eq!(resolved.font_east_asia.as_deref(), Some("MS Gothic"));
        assert_eq!(resolved.font_east_asia_theme, None);
        assert_eq!(resolved.font_cs.as_deref(), Some("Arial"));
        assert_eq!(resolved.font_cs_theme, None);
    }

    #[test]
    fn direct_literal_color_replaces_inherited_theme_selector() {
        let xml = make_styles_xml(
            r#"<w:docDefaults><w:rPrDefault><w:rPr>
                <w:color w:val="365F91" w:themeColor="accent1" w:themeShade="BF"/>
            </w:rPr></w:rPrDefault></w:docDefaults>"#,
        );
        let defs = StyleDefinitions::parse(&xml).expect("parse styles");
        let direct = TextMarks {
            color: Some(IStr::from("366091")),
            ..TextMarks::default()
        };

        let resolved = defs.resolve(&direct, None, None);

        assert_eq!(resolved.color.as_deref(), Some("366091"));
        assert_eq!(resolved.color_theme, None);
    }

    #[test]
    fn direct_script_theme_selectors_are_retained_when_same_rfonts_authors_them() {
        let xml = make_styles_xml("");
        let direct = TextMarks {
            font_east_asia: Some(IStr::from("MS Gothic")),
            font_east_asia_theme: Some(IStr::from("majorEastAsia")),
            font_cs: Some(IStr::from("Arial")),
            font_cs_theme: Some(IStr::from("majorBidi")),
            authored_rfonts: AuthoredRFonts {
                east_asia: Some(IStr::from("MS Gothic")),
                east_asia_theme: Some(IStr::from("majorEastAsia")),
                cs: Some(IStr::from("Arial")),
                cs_theme: Some(IStr::from("majorBidi")),
                ..AuthoredRFonts::default()
            },
            ..TextMarks::default()
        };

        let resolved = StyleDefinitions::parse(&xml)
            .expect("parse styles")
            .resolve(&direct, None, None);

        assert_eq!(
            resolved.font_east_asia_theme.as_deref(),
            Some("majorEastAsia")
        );
        assert_eq!(resolved.font_cs_theme.as_deref(), Some("majorBidi"));
    }

    #[test]
    fn merge_styles_xml_prefers_target_and_keeps_base_only_styles() {
        let base_xml = make_styles_xml(
            r#"
            <w:style w:type="paragraph" w:styleId="Normal"><w:name w:val="Normal"/><w:pPr><w:spacing w:before="240"/></w:pPr></w:style>
            <w:style w:type="paragraph" w:styleId="BaseOnly"><w:name w:val="BaseOnly"/></w:style>
            "#,
        );
        let target_xml = make_styles_xml(
            r#"
            <w:docDefaults><w:rPrDefault><w:rPr><w:sz w:val="24"/></w:rPr></w:rPrDefault></w:docDefaults>
            <w:style w:type="paragraph" w:styleId="Normal"><w:name w:val="Normal"/><w:pPr><w:spacing w:before="0"/></w:pPr></w:style>
            <w:style w:type="paragraph" w:styleId="Body"><w:name w:val="Body"/></w:style>
            "#,
        );

        let merged =
            merge_styles_xml_preferring_target(&base_xml, &target_xml).expect("merge styles.xml");
        let merged_root = Element::parse(Cursor::new(merged)).expect("parse merged styles");
        let merged_styles = extract_style_elements(&merged_root);

        assert!(merged_styles.contains_key("Body"));
        assert!(merged_styles.contains_key("BaseOnly"));

        let normal = merged_styles.get("Normal").expect("merged Normal");
        let normal_xml = serialize_element(normal);
        assert!(
            normal_xml.contains("w:before=\"0\""),
            "merged styles should keep the target Normal definition: {normal_xml}"
        );
        assert!(
            !normal_xml.contains("w:before=\"240\""),
            "merged styles should not keep the base Normal definition on collision: {normal_xml}"
        );
    }

    #[test]
    fn merge_styles_xml_declares_extensions_used_by_base_only_styles() {
        let base_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
            <w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
                      xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml">
              <w:style w:type="character" w:styleId="BaseOnly">
                <w:rPr><w14:ligatures w14:val="standardContextual"/></w:rPr>
              </w:style>
            </w:styles>"#;
        let target_xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="Normal"><w:name w:val="Normal"/></w:style>"#,
        );

        let merged = merge_styles_xml_preferring_target(base_xml, &target_xml)
            .expect("merge styles with extension markup");
        let merged_text = String::from_utf8(merged.clone()).expect("UTF-8 styles.xml");
        assert!(merged_text.contains("xmlns:w14="), "{merged_text}");
        assert!(
            merged_text.contains("mc:Ignorable=\"w14\""),
            "{merged_text}"
        );
        Element::parse(Cursor::new(merged)).expect("merged styles.xml must be well-formed");
    }

    #[test]
    fn parse_empty_returns_error() {
        let err = StyleDefinitions::parse(&[]).expect_err("empty styles.xml must error");
        assert!(err.contains("word/styles.xml"));
    }

    #[test]
    fn parse_doc_defaults_font() {
        let xml = make_styles_xml(
            r#"<w:docDefaults>
                <w:rPrDefault>
                    <w:rPr>
                        <w:rFonts w:ascii="Times New Roman"/>
                        <w:sz w:val="24"/>
                    </w:rPr>
                </w:rPrDefault>
            </w:docDefaults>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        assert_eq!(
            defs.doc_defaults.font_family.as_deref(),
            Some("Times New Roman")
        );
        assert_eq!(defs.doc_defaults.font_size, Some(24));
    }

    /// An unrecognized ST_OnOff value on a toggle property (schema-invalid —
    /// ST_OnOff only permits 0/1/true/false/on/off) is not the same as an
    /// absent or "0"-valued attribute. Per Word's own tolerant boolean
    /// parsing, stemma treats it as On (product-approved default), not Off
    /// and not a parse failure.
    #[test]
    fn parse_toggle_value_unrecognized_val_defaults_to_on() {
        let rpr = Element::parse(Cursor::new(
            br#"<w:rPr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:b w:val="maybe"/></w:rPr>"#.as_slice(),
        ))
        .unwrap();
        let marks = parse_rpr_marks(&rpr).unwrap();
        assert_eq!(
            marks.bold,
            MarkValue::On,
            "unrecognized w:val on w:b should default to On, not Off or Inherit"
        );
    }

    #[test]
    fn resolve_inherits_from_doc_defaults() {
        let xml = make_styles_xml(
            r#"<w:docDefaults>
                <w:rPrDefault>
                    <w:rPr>
                        <w:b/>
                        <w:rFonts w:ascii="Arial"/>
                    </w:rPr>
                </w:rPrDefault>
            </w:docDefaults>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        let direct = TextMarks::default(); // all Inherit
        let resolved = defs.resolve(&direct, None, None);
        assert_eq!(resolved.bold, MarkValue::On);
        assert_eq!(resolved.font_family.as_deref(), Some("Arial"));
    }

    #[test]
    fn resolve_toggle_xor_across_levels() {
        // MS-OI29500 §2.1.258: Word uses RESET (simple override) semantics
        // between paragraph style and character style levels, not XOR.
        // Para=On, Char=Off → char style overrides → Off.
        let xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="Normal">
                <w:rPr><w:b/></w:rPr>
            </w:style>
            <w:style w:type="character" w:styleId="BoldChar">
                <w:rPr><w:b w:val="0"/></w:rPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        let direct = TextMarks::default();
        let resolved = defs.resolve(&direct, Some("BoldChar"), Some("Normal"));
        assert_eq!(resolved.bold, MarkValue::Off);
    }

    #[test]
    fn resolve_direct_overrides_all() {
        let xml = make_styles_xml(
            r#"<w:docDefaults>
                <w:rPrDefault>
                    <w:rPr><w:rFonts w:ascii="Arial"/></w:rPr>
                </w:rPrDefault>
            </w:docDefaults>
            <w:style w:type="paragraph" w:styleId="Normal">
                <w:rPr><w:rFonts w:ascii="Verdana"/></w:rPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        let direct = TextMarks {
            font_family: Some(IStr::from("Courier")),
            ..Default::default()
        };
        let resolved = defs.resolve(&direct, None, Some("Normal"));
        assert_eq!(resolved.font_family.as_deref(), Some("Courier"));
    }

    #[test]
    fn based_on_chain_resolution() {
        let xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="Normal">
                <w:rPr>
                    <w:rFonts w:ascii="Times New Roman"/>
                    <w:sz w:val="24"/>
                </w:rPr>
            </w:style>
            <w:style w:type="paragraph" w:styleId="Heading1">
                <w:basedOn w:val="Normal"/>
                <w:rPr>
                    <w:b/>
                    <w:sz w:val="32"/>
                </w:rPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        let direct = TextMarks::default();
        let resolved = defs.resolve(&direct, None, Some("Heading1"));
        // Heading1 inherits font from Normal, overrides size, adds bold
        assert_eq!(resolved.bold, MarkValue::On);
        assert_eq!(resolved.font_family.as_deref(), Some("Times New Roman"));
        assert_eq!(resolved.font_size, Some(32));
    }

    #[test]
    fn cycle_in_based_on_does_not_loop() {
        let xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="A">
                <w:basedOn w:val="B"/>
                <w:rPr><w:b/></w:rPr>
            </w:style>
            <w:style w:type="paragraph" w:styleId="B">
                <w:basedOn w:val="A"/>
                <w:rPr><w:i/></w:rPr>
            </w:style>"#,
        );
        // Should not hang — cycle guard breaks the loop
        let defs = StyleDefinitions::parse(&xml).unwrap();
        let direct = TextMarks::default();
        let resolved = defs.resolve(&direct, None, Some("A"));
        assert_eq!(resolved.bold, MarkValue::On);
    }

    // ══════════════════════════════════════════════════════════════════════
    // Tab stop resolution tests
    // ══════════════════════════════════════════════════════════════════════

    #[test]
    fn tab_stops_simple_style() {
        let xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="SigLine">
                <w:pPr>
                    <w:tabs>
                        <w:tab w:val="left" w:pos="432"/>
                        <w:tab w:val="left" w:pos="4320"/>
                    </w:tabs>
                </w:pPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        let tabs = defs.resolve_effective_tabs(Some("SigLine"), None);
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs[0].position, 432);
        assert_eq!(tabs[1].position, 4320);
    }

    #[test]
    fn tab_stops_based_on_inheritance() {
        let xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="Parent">
                <w:pPr>
                    <w:tabs>
                        <w:tab w:val="left" w:pos="720"/>
                        <w:tab w:val="center" w:pos="4320"/>
                    </w:tabs>
                </w:pPr>
            </w:style>
            <w:style w:type="paragraph" w:styleId="Child">
                <w:basedOn w:val="Parent"/>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        // Child has no w:tabs → inherits from Parent
        let tabs = defs.resolve_effective_tabs(Some("Child"), None);
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs[0].position, 720);
        assert_eq!(tabs[1].position, 4320);
        assert_eq!(tabs[1].alignment, crate::domain::TabAlignment::Center);
    }

    #[test]
    fn tab_stops_clear_semantics() {
        let xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="Parent">
                <w:pPr>
                    <w:tabs>
                        <w:tab w:val="left" w:pos="720"/>
                        <w:tab w:val="center" w:pos="4320"/>
                    </w:tabs>
                </w:pPr>
            </w:style>
            <w:style w:type="paragraph" w:styleId="Child">
                <w:basedOn w:val="Parent"/>
                <w:pPr>
                    <w:tabs>
                        <w:tab w:val="clear" w:pos="720"/>
                        <w:tab w:val="right" w:pos="9000"/>
                    </w:tabs>
                </w:pPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        let tabs = defs.resolve_effective_tabs(Some("Child"), None);
        // 720 cleared, 4320 inherited, 9000 added
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs[0].position, 4320);
        assert_eq!(tabs[1].position, 9000);
        assert_eq!(tabs[1].alignment, crate::domain::TabAlignment::Right);
    }

    #[test]
    fn tab_stops_direct_overrides_style() {
        use crate::domain::{TabAlignment, TabLeader};
        let xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="SigLine">
                <w:pPr>
                    <w:tabs>
                        <w:tab w:val="left" w:pos="720"/>
                    </w:tabs>
                </w:pPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        let direct = vec![
            TabStopDef {
                position: 720,
                alignment: TabAlignment::Clear,
                leader: None,
            },
            TabStopDef {
                position: 1440,
                alignment: TabAlignment::Center,
                leader: Some(TabLeader::Dot),
            },
        ];
        let tabs = defs.resolve_effective_tabs(Some("SigLine"), Some(&direct));
        // 720 cleared by direct, 1440 added
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].position, 1440);
        assert_eq!(tabs[0].alignment, TabAlignment::Center);
        assert_eq!(tabs[0].leader, Some(TabLeader::Dot));
    }

    #[test]
    fn tab_stops_unknown_style_returns_empty() {
        let xml = make_styles_xml("");
        let defs = StyleDefinitions::parse(&xml).unwrap();
        let tabs = defs.resolve_effective_tabs(Some("NonExistent"), None);
        assert!(tabs.is_empty());
    }

    // ══════════════════════════════════════════════════════════════════════
    // Paragraph property (indent, alignment) resolution tests
    // ══════════════════════════════════════════════════════════════════════

    #[test]
    fn para_props_simple_style_indent() {
        let xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="Normal">
                <w:pPr>
                    <w:ind w:firstLine="720"/>
                </w:pPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        let indent = defs.resolve_effective_indent(Some("Normal"), None, None);
        assert!(indent.is_some());
        let indent = indent.unwrap();
        assert_eq!(indent.effective_first_line_twips, Some(720));
        assert_eq!(indent.left, None);
        assert_eq!(indent.right, None);
    }

    #[test]
    fn para_props_based_on_inherits_indent() {
        let xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="Normal">
                <w:pPr>
                    <w:ind w:firstLine="720"/>
                </w:pPr>
            </w:style>
            <w:style w:type="paragraph" w:styleId="Child">
                <w:basedOn w:val="Normal"/>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        // Child inherits firstLine=720 from Normal
        let indent = defs
            .resolve_effective_indent(Some("Child"), None, None)
            .unwrap();
        assert_eq!(indent.effective_first_line_twips, Some(720));
    }

    #[test]
    fn para_props_child_overrides_parent_per_field() {
        let xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="Normal">
                <w:pPr>
                    <w:ind w:left="360" w:firstLine="720"/>
                </w:pPr>
            </w:style>
            <w:style w:type="paragraph" w:styleId="Child">
                <w:basedOn w:val="Normal"/>
                <w:pPr>
                    <w:ind w:left="1440"/>
                </w:pPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        let indent = defs
            .resolve_effective_indent(Some("Child"), None, None)
            .unwrap();
        // Child overrides left, inherits firstLine from Normal
        assert_eq!(indent.left, Some(1440));
        assert_eq!(indent.effective_first_line_twips, Some(720));
    }

    #[test]
    fn para_props_direct_left_inherits_first_line_from_style() {
        let xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="Normal">
                <w:pPr>
                    <w:ind w:left="360" w:firstLine="720"/>
                </w:pPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        let direct = IndentProps {
            left: Some(4320),
            right: None,
            effective_first_line_twips: None,
            start_chars: None,
            end_chars: None,
            first_line_chars: None,
            hanging_chars: None,
        };
        let indent = defs
            .resolve_effective_indent(Some("Normal"), Some(&direct), None)
            .unwrap();
        // Per-attribute cascade: direct overrides left, firstLine absent → inherits from style.
        assert_eq!(indent.left, Some(4320));
        assert_eq!(indent.effective_first_line_twips, Some(720));
    }

    #[test]
    fn para_props_no_style_returns_direct_only() {
        let xml = make_styles_xml("");
        let defs = StyleDefinitions::parse(&xml).unwrap();
        let direct = IndentProps {
            left: Some(4320),
            right: None,
            effective_first_line_twips: None,
            start_chars: None,
            end_chars: None,
            first_line_chars: None,
            hanging_chars: None,
        };
        let indent = defs
            .resolve_effective_indent(None, Some(&direct), None)
            .unwrap();
        assert_eq!(indent.left, Some(4320));
        assert_eq!(indent.effective_first_line_twips, None);
    }

    #[test]
    fn para_props_no_style_no_direct_returns_none() {
        let xml = make_styles_xml("");
        let defs = StyleDefinitions::parse(&xml).unwrap();
        assert!(defs.resolve_effective_indent(None, None, None).is_none());
    }

    /// SAFE US vs Singapore: negative left indent with Normal style's
    /// firstLine=720.  Per-attribute cascade (§17.3.1.12): absent firstLine
    /// in direct w:ind inherits from style chain.
    #[test]
    fn para_props_direct_negative_left_inherits_first_line_from_style() {
        let xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal">
                <w:pPr>
                    <w:ind w:firstLine="720"/>
                </w:pPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        let direct = IndentProps {
            left: Some(-720),
            right: None,
            effective_first_line_twips: None,
            start_chars: None,
            end_chars: None,
            first_line_chars: None,
            hanging_chars: None,
        };
        let indent = defs
            .resolve_effective_indent(Some("Normal"), Some(&direct), None)
            .unwrap();
        assert_eq!(indent.left, Some(-720));
        // Per-attribute cascade: firstLine absent → inherits 720 from Normal style.
        assert_eq!(
            indent.effective_first_line_twips,
            Some(720),
            "firstLine should inherit from Normal style when direct w:ind omits it"
        );
        assert_eq!(indent.right, None);
    }

    /// Direct w:ind has left only, numbering level has hanging, style has firstLine.
    /// Per-attribute cascade: numbering firstLine wins over style firstLine.
    #[test]
    fn para_props_direct_with_numbering_uses_numbering_first_line() {
        let xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal">
                <w:pPr>
                    <w:ind w:firstLine="720"/>
                </w:pPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        let direct = IndentProps {
            left: Some(2160),
            right: None,
            effective_first_line_twips: None,
            start_chars: None,
            end_chars: None,
            first_line_chars: None,
            hanging_chars: None,
        };
        let num_indent = crate::numbering::LevelIndent {
            left: Some(2160),
            right: None,
            effective_first_line_twips: Some(-720),
        };
        let indent = defs
            .resolve_effective_indent(Some("Normal"), Some(&direct), Some(&num_indent))
            .unwrap();
        assert_eq!(indent.left, Some(2160));
        // Per-attribute cascade: direct (None) → numbering (-720) → style (720).
        // Numbering wins since it's checked before style.
        assert_eq!(
            indent.effective_first_line_twips,
            Some(-720),
            "numbering firstLine should win over style firstLine"
        );
    }

    #[test]
    fn para_props_alignment_from_style() {
        let xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="Normal">
                <w:pPr>
                    <w:jc w:val="center"/>
                </w:pPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        assert_eq!(
            defs.resolve_effective_alignment(Some("Normal"), None),
            Some(Alignment::Center)
        );
    }

    #[test]
    fn para_props_alignment_direct_overrides_style() {
        let xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="Normal">
                <w:pPr>
                    <w:jc w:val="center"/>
                </w:pPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        assert_eq!(
            defs.resolve_effective_alignment(Some("Normal"), Some(&Alignment::Right)),
            Some(Alignment::Right)
        );
    }

    #[test]
    fn para_props_alignment_inherits_through_chain() {
        let xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="Normal">
                <w:pPr>
                    <w:jc w:val="both"/>
                </w:pPr>
            </w:style>
            <w:style w:type="paragraph" w:styleId="Child">
                <w:basedOn w:val="Normal"/>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        assert_eq!(
            defs.resolve_effective_alignment(Some("Child"), None),
            Some(Alignment::Justify)
        );
    }

    /// ST_Jc (§17.18.44) is a closed set: a style whose `w:jc` holds anything
    /// else has no alignment a reader can honour, so the table refuses to load
    /// and names the style, as import refuses such a paragraph.
    #[test]
    fn para_props_unrecognized_alignment_is_refused() {
        let xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="Odd">
                <w:pPr>
                    <w:jc w:val="sideways"/>
                </w:pPr>
            </w:style>"#,
        );
        let err = StyleDefinitions::parse(&xml).expect_err("unknown jc must refuse");
        assert!(err.contains("\"Odd\"") && err.contains("sideways"), "{err}");
    }

    #[test]
    fn para_props_hanging_indent_as_negative() {
        let xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="Normal">
                <w:pPr>
                    <w:ind w:left="720" w:hanging="360"/>
                </w:pPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        let indent = defs
            .resolve_effective_indent(Some("Normal"), None, None)
            .unwrap();
        assert_eq!(indent.left, Some(720));
        assert_eq!(indent.effective_first_line_twips, Some(-360));
    }

    // --- Spacing style inheritance tests ---

    #[test]
    fn spacing_direct_overrides_style() {
        let xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="Normal">
                <w:pPr>
                    <w:spacing w:before="100" w:after="200" w:line="360"/>
                </w:pPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        let direct = SpacingProps {
            before: Some(50),
            after: None,
            before_lines: None,
            after_lines: None,
            before_autospacing: None,
            after_autospacing: None,
            line: None,
            line_rule: None,
        };
        let resolved = defs
            .resolve_effective_spacing(Some("Normal"), Some(&direct))
            .unwrap();
        // Direct before=50 overrides style before=100
        assert_eq!(resolved.before, Some(50));
        // Per-attribute: absent after and line inherit from style (§17.3.1.33)
        assert_eq!(resolved.after, Some(200));
        assert_eq!(resolved.line, Some(360));
    }

    #[test]
    fn spacing_inherits_from_style() {
        let xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="Normal">
                <w:pPr>
                    <w:spacing w:before="120" w:after="240"/>
                </w:pPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        let resolved = defs
            .resolve_effective_spacing(Some("Normal"), None)
            .unwrap();
        assert_eq!(resolved.before, Some(120));
        assert_eq!(resolved.after, Some(240));
    }

    #[test]
    fn spacing_inherits_from_ppr_defaults() {
        let xml = make_styles_xml(
            r#"<w:docDefaults>
                <w:pPrDefault>
                    <w:pPr>
                        <w:spacing w:after="160" w:line="259" w:lineRule="auto"/>
                    </w:pPr>
                </w:pPrDefault>
            </w:docDefaults>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        // No style, no direct — falls through to doc paragraph defaults
        let resolved = defs.resolve_effective_spacing(None, None).unwrap();
        assert_eq!(resolved.after, Some(160));
        assert_eq!(resolved.line, Some(259));
        assert_eq!(resolved.line_rule.as_deref(), Some("auto"));
    }

    #[test]
    fn spacing_per_field_inheritance_across_layers() {
        // doc defaults: after=160, line=259
        // style Normal: before=120 (doesn't set after or line → inherit from defaults)
        // direct: line=360 (overrides both style and defaults)
        let xml = make_styles_xml(
            r#"<w:docDefaults>
                <w:pPrDefault>
                    <w:pPr>
                        <w:spacing w:after="160" w:line="259"/>
                    </w:pPr>
                </w:pPrDefault>
            </w:docDefaults>
            <w:style w:type="paragraph" w:styleId="Normal">
                <w:pPr>
                    <w:spacing w:before="120"/>
                </w:pPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        let direct = SpacingProps {
            before: None,
            after: None,
            before_lines: None,
            after_lines: None,
            before_autospacing: None,
            after_autospacing: None,
            line: Some(360),
            line_rule: None,
        };
        let resolved = defs
            .resolve_effective_spacing(Some("Normal"), Some(&direct))
            .unwrap();
        // Per-attribute: before inherits from Normal (120), after from defaults (160)
        assert_eq!(resolved.before, Some(120));
        assert_eq!(resolved.after, Some(160));
        // line: direct=360 (wins)
        assert_eq!(resolved.line, Some(360));
    }

    #[test]
    fn spacing_no_values_returns_none() {
        let xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="Normal">
                <w:pPr>
                    <w:jc w:val="center"/>
                </w:pPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        assert!(
            defs.resolve_effective_spacing(Some("Normal"), None)
                .is_none()
        );
    }

    // --- Spec tests: spacing per-attribute inheritance (§17.3.1.33) ---
    //
    // ECMA-376 §17.3.1.33 says each spacing attribute inherits independently
    // from the style hierarchy when omitted — identical wording to §17.3.1.12
    // (ind). Word produces partial w:spacing in 99.7% of direct formatting
    // cases (e.g., only w:before="0" with no after/line). Per-attribute
    // inheritance is required for correct resolution.

    /// §17.3.1.33: Direct w:spacing with only before=50 should inherit
    /// after=200 and line=360 from the style, not drop them to None.
    #[test]
    fn spec_spacing_partial_direct_inherits_missing_from_style() {
        let xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="Normal">
                <w:pPr>
                    <w:spacing w:before="100" w:after="200" w:line="360"/>
                </w:pPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        let direct = SpacingProps {
            before: Some(50),
            after: None,
            before_lines: None,
            after_lines: None,
            before_autospacing: None,
            after_autospacing: None,
            line: None,
            line_rule: None,
        };
        let resolved = defs
            .resolve_effective_spacing(Some("Normal"), Some(&direct))
            .unwrap();
        // Direct before=50 overrides style before=100
        assert_eq!(resolved.before, Some(50));
        // Per-attribute: absent after inherits from style
        assert_eq!(
            resolved.after,
            Some(200),
            "after should inherit 200 from style (per-attribute, §17.3.1.33)"
        );
        // Per-attribute: absent line inherits from style
        assert_eq!(
            resolved.line,
            Some(360),
            "line should inherit 360 from style (per-attribute, §17.3.1.33)"
        );
    }

    /// §17.3.1.33: Direct w:spacing with only line=360 should inherit
    /// before and after from the style chain (style + docDefaults).
    #[test]
    fn spec_spacing_partial_direct_line_inherits_before_after() {
        let xml = make_styles_xml(
            r#"<w:docDefaults>
                <w:pPrDefault>
                    <w:pPr>
                        <w:spacing w:after="160" w:line="259"/>
                    </w:pPr>
                </w:pPrDefault>
            </w:docDefaults>
            <w:style w:type="paragraph" w:styleId="Normal">
                <w:pPr>
                    <w:spacing w:before="120"/>
                </w:pPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        let direct = SpacingProps {
            before: None,
            after: None,
            before_lines: None,
            after_lines: None,
            before_autospacing: None,
            after_autospacing: None,
            line: Some(360),
            line_rule: None,
        };
        let resolved = defs
            .resolve_effective_spacing(Some("Normal"), Some(&direct))
            .unwrap();
        // line: direct=360 wins
        assert_eq!(resolved.line, Some(360));
        // Per-attribute: before inherits from style Normal
        assert_eq!(
            resolved.before,
            Some(120),
            "before should inherit 120 from Normal style (per-attribute, §17.3.1.33)"
        );
        // Per-attribute: after inherits from docDefaults (Normal doesn't set it)
        assert_eq!(
            resolved.after,
            Some(160),
            "after should inherit 160 from docDefaults (per-attribute, §17.3.1.33)"
        );
    }

    /// §17.3.1.33: Direct before=0 should NOT wipe out style's after=240.
    /// This is the exact pattern found in 99.7% of Word-produced SAFE agreements.
    #[test]
    fn spec_spacing_direct_before_zero_preserves_style_after() {
        let xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="Normal">
                <w:pPr>
                    <w:spacing w:after="240" w:line="276" w:lineRule="auto"/>
                </w:pPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        let direct = SpacingProps {
            before: Some(0),
            after: None,
            before_lines: None,
            after_lines: None,
            before_autospacing: None,
            after_autospacing: None,
            line: None,
            line_rule: None,
        };
        let resolved = defs
            .resolve_effective_spacing(Some("Normal"), Some(&direct))
            .unwrap();
        assert_eq!(resolved.before, Some(0), "direct before=0 should win");
        assert_eq!(
            resolved.after,
            Some(240),
            "after should inherit 240 from style — direct before=0 must not wipe it"
        );
        assert_eq!(
            resolved.line,
            Some(276),
            "line should inherit 276 from style — direct before=0 must not wipe it"
        );
        assert_eq!(
            resolved.line_rule.as_deref(),
            Some("auto"),
            "lineRule should inherit from style"
        );
    }

    /// §17.3.1.33: lineRule defaults to "auto" when line is present in direct
    /// formatting, even if the style has lineRule=exact. This is a special rule
    /// that overrides normal per-attribute inheritance.
    #[test]
    fn spec_spacing_line_rule_defaults_to_auto_overrides_style_exact() {
        let xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="ExactStyle">
                <w:pPr>
                    <w:spacing w:before="100" w:after="200" w:line="240" w:lineRule="exact"/>
                </w:pPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        let direct = SpacingProps {
            before: None,
            after: None,
            before_lines: None,
            after_lines: None,
            before_autospacing: None,
            after_autospacing: None,
            line: Some(480),
            line_rule: None,
        };
        let resolved = defs
            .resolve_effective_spacing(Some("ExactStyle"), Some(&direct))
            .unwrap();
        assert_eq!(resolved.line, Some(480), "direct line=480 wins");
        // Special rule: lineRule defaults to auto when line is present,
        // NOT inherited from style's "exact"
        assert_eq!(
            resolved.line_rule.as_deref(),
            Some("auto"),
            "lineRule should default to auto (§17.3.1.33 special rule), not inherit exact from style"
        );
        // Per-attribute: before and after inherit from style
        assert_eq!(
            resolved.before,
            Some(100),
            "before should inherit from style"
        );
        assert_eq!(resolved.after, Some(200), "after should inherit from style");
    }

    /// §17.3.1.33 + MS-OI29500 2.1.60: beforeLines from style chain should
    /// still merge in even with per-attribute spacing inheritance.
    #[test]
    fn spec_spacing_before_lines_merges_with_per_attribute() {
        let xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="Normal">
                <w:pPr>
                    <w:spacing w:before="100" w:after="200" w:beforeLines="50"/>
                </w:pPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        let direct = SpacingProps {
            before: Some(0),
            after: None,
            before_lines: None,
            after_lines: None,
            before_autospacing: None,
            after_autospacing: None,
            line: None,
            line_rule: None,
        };
        let resolved = defs
            .resolve_effective_spacing(Some("Normal"), Some(&direct))
            .unwrap();
        assert_eq!(resolved.before, Some(0), "direct before=0 wins");
        assert_eq!(resolved.after, Some(200), "after should inherit from style");
        assert_eq!(
            resolved.before_lines,
            Some(50),
            "beforeLines should merge from style (MS-OI29500 2.1.60)"
        );
    }

    // --- Border style inheritance tests ---

    #[test]
    fn borders_direct_overrides_style() {
        let xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="Normal">
                <w:pPr>
                    <w:pBdr>
                        <w:top w:val="single" w:color="FF0000" w:sz="4"/>
                    </w:pBdr>
                </w:pPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        // Direct borders with a different top — whole-object replacement means style is ignored.
        let direct = ParagraphBorderProps {
            top: Some(BorderEdge {
                style: "double".into(),
                color: Some("0000FF".into()),
                size: Some(8),
                space: None,
                extra_attrs: Vec::new(),
            }),
            bottom: None,
            left: None,
            right: None,
            between: None,
            bar: None,
        };
        let resolved = defs
            .resolve_effective_borders(Some("Normal"), Some(&direct))
            .unwrap();
        let top = resolved.top.expect("top should be present");
        assert_eq!(top.style, "double");
        assert_eq!(top.color.as_deref(), Some("0000FF"));
        // Style's top border is NOT merged — whole-object replacement.
    }

    #[test]
    fn borders_inherit_from_style_when_no_direct() {
        let xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="Bordered">
                <w:pPr>
                    <w:pBdr>
                        <w:bottom w:val="single" w:color="000000" w:sz="4"/>
                    </w:pBdr>
                </w:pPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        let resolved = defs
            .resolve_effective_borders(Some("Bordered"), None)
            .unwrap();
        let bottom = resolved.bottom.expect("bottom should be present");
        assert_eq!(bottom.style, "single");
        assert_eq!(bottom.color.as_deref(), Some("000000"));
    }

    #[test]
    fn borders_no_values_returns_none() {
        let xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="Normal">
                <w:pPr>
                    <w:jc w:val="center"/>
                </w:pPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        assert!(
            defs.resolve_effective_borders(Some("Normal"), None)
                .is_none()
        );
    }

    #[test]
    fn borders_inherit_through_based_on_chain() {
        let xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="Normal">
                <w:pPr>
                    <w:pBdr>
                        <w:top w:val="single" w:sz="4"/>
                    </w:pBdr>
                </w:pPr>
            </w:style>
            <w:style w:type="paragraph" w:styleId="Child">
                <w:basedOn w:val="Normal"/>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        // Child has no direct borders, should inherit from Normal via basedOn.
        let resolved = defs.resolve_effective_borders(Some("Child"), None).unwrap();
        assert!(
            resolved.top.is_some(),
            "should inherit top border from parent style"
        );
    }

    // ══════════════════════════════════════════════════════════════════════
    // Table style resolution tests
    // ══════════════════════════════════════════════════════════════════════

    #[test]
    fn table_style_parses_borders() {
        let xml = make_styles_xml(
            r#"<w:style w:type="table" w:styleId="LegalTable">
                <w:name w:val="Legal Table"/>
                <w:tblPr>
                    <w:tblBorders>
                        <w:top w:val="single" w:sz="4" w:color="000000"/>
                        <w:bottom w:val="single" w:sz="4" w:color="000000"/>
                        <w:left w:val="single" w:sz="4" w:color="000000"/>
                        <w:right w:val="single" w:sz="4" w:color="000000"/>
                        <w:insideH w:val="single" w:sz="4" w:color="000000"/>
                        <w:insideV w:val="single" w:sz="4" w:color="000000"/>
                    </w:tblBorders>
                </w:tblPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        let style = defs
            .table_style("LegalTable")
            .expect("LegalTable should be parsed");
        let borders = style.borders.as_ref().expect("should have borders");
        assert_eq!(
            borders.top.as_ref().unwrap().style,
            crate::domain::BorderStyle::Single
        );
        assert_eq!(borders.inside_h.as_ref().unwrap().size, Some(4));
        assert_eq!(
            borders.inside_v.as_ref().unwrap().color.as_deref(),
            Some("000000")
        );
    }

    #[test]
    fn table_style_inherits_through_based_on() {
        let xml = make_styles_xml(
            r#"<w:style w:type="table" w:styleId="BaseTable">
                <w:tblPr>
                    <w:tblBorders>
                        <w:top w:val="single" w:sz="4" w:color="000000"/>
                        <w:bottom w:val="single" w:sz="4" w:color="000000"/>
                    </w:tblBorders>
                </w:tblPr>
            </w:style>
            <w:style w:type="table" w:styleId="ChildTable">
                <w:basedOn w:val="BaseTable"/>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        let style = defs
            .table_style("ChildTable")
            .expect("ChildTable should inherit from BaseTable");
        let borders = style.borders.as_ref().expect("should inherit borders");
        assert!(borders.top.is_some(), "should inherit top border");
        assert!(borders.bottom.is_some(), "should inherit bottom border");
    }

    #[test]
    fn table_style_unknown_returns_none() {
        let xml = make_styles_xml("");
        let defs = StyleDefinitions::parse(&xml).unwrap();
        assert!(defs.table_style("NonExistent").is_none());
    }

    #[test]
    fn table_style_parses_cell_margins() {
        let xml = make_styles_xml(
            r#"<w:style w:type="table" w:styleId="MarginsTable">
                <w:tblPr>
                    <w:tblCellMar>
                        <w:top w:w="100" w:type="dxa"/>
                        <w:left w:w="200" w:type="dxa"/>
                        <w:bottom w:w="100" w:type="dxa"/>
                        <w:right w:w="200" w:type="dxa"/>
                    </w:tblCellMar>
                </w:tblPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        let style = defs
            .table_style("MarginsTable")
            .expect("should parse MarginsTable");
        let margins = style
            .default_cell_margins
            .as_ref()
            .expect("should have cell margins");
        assert_eq!(margins.top, Some(100));
        assert_eq!(margins.left, Some(200));
    }

    /// A pct margin edge is not specified (Word renders no margin from it),
    /// so a style's effective margins leave it out.
    #[test]
    fn table_style_pct_cell_margin_is_not_specified() {
        let xml = make_styles_xml(
            r#"<w:style w:type="table" w:styleId="PctMargins">
                <w:tblPr>
                    <w:tblCellMar>
                        <w:top w:w="100" w:type="dxa"/>
                        <w:left w:w="500" w:type="pct"/>
                    </w:tblCellMar>
                </w:tblPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        let margins = defs
            .table_style("PctMargins")
            .and_then(|style| style.default_cell_margins.as_ref())
            .expect("the dxa edge is a margin");
        assert_eq!(margins.top, Some(100));
        assert_eq!(margins.left, None);
    }

    #[test]
    fn table_style_parses_cell_shading() {
        let xml = make_styles_xml(
            r#"<w:style w:type="table" w:styleId="ShadedTable">
                <w:tcPr>
                    <w:shd w:val="clear" w:fill="FFFF00"/>
                </w:tcPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).unwrap();
        let style = defs
            .table_style("ShadedTable")
            .expect("should parse ShadedTable");
        let shading = style
            .default_cell_shading
            .as_ref()
            .expect("should have shading");
        assert_eq!(shading.fill.as_deref(), Some("FFFF00"));
        assert_eq!(shading.val, Some(crate::domain::ShadingPattern::Clear));
    }

    // =========================================================================
    // Style collision detection
    // =========================================================================

    #[test]
    fn collision_detected_when_same_style_id_different_definition() {
        let base_xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="CustomA">
                <w:rPr><w:b/></w:rPr>
            </w:style>"#,
        );
        let target_xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="CustomA">
                <w:rPr><w:i/></w:rPr>
            </w:style>"#,
        );
        let referenced: HashSet<IStr> = ["CustomA"].iter().map(|s| IStr::from(*s)).collect();
        let collisions = detect_style_collisions(&base_xml, &target_xml, &referenced).unwrap();
        assert_eq!(collisions.len(), 1);
        assert_eq!(collisions[0].style_id, "CustomA");
        assert_eq!(collisions[0].style_type, "paragraph");
    }

    #[test]
    fn no_collision_when_definitions_identical() {
        let base_xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="Normal">
                <w:rPr><w:sz w:val="24"/></w:rPr>
            </w:style>"#,
        );
        let target_xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="Normal">
                <w:rPr><w:sz w:val="24"/></w:rPr>
            </w:style>"#,
        );
        let referenced: HashSet<IStr> = ["Normal"].iter().map(|s| IStr::from(*s)).collect();
        let collisions = detect_style_collisions(&base_xml, &target_xml, &referenced).unwrap();
        assert!(collisions.is_empty());
    }

    #[test]
    fn collision_skipped_when_style_not_referenced() {
        let base_xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="UnusedStyle">
                <w:rPr><w:b/></w:rPr>
            </w:style>"#,
        );
        let target_xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="UnusedStyle">
                <w:rPr><w:i/></w:rPr>
            </w:style>"#,
        );
        // UnusedStyle is not in the referenced set.
        let referenced: HashSet<IStr> = ["Normal"].iter().map(|s| IStr::from(*s)).collect();
        let collisions = detect_style_collisions(&base_xml, &target_xml, &referenced).unwrap();
        assert!(collisions.is_empty());
    }

    #[test]
    fn collision_detected_for_differing_default_style_without_explicit_reference() {
        let base_xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal">
                <w:rPr><w:b/></w:rPr>
            </w:style>"#,
        );
        let target_xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal">
                <w:rPr><w:i/></w:rPr>
            </w:style>"#,
        );
        let collisions = detect_style_collisions(&base_xml, &target_xml, &HashSet::new()).unwrap();
        assert_eq!(collisions.len(), 1);
        assert_eq!(collisions[0].style_id, "Normal");
    }

    #[test]
    fn collision_detected_in_active_based_on_closure() {
        let base_xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="Parent"><w:rPr><w:b/></w:rPr></w:style>
               <w:style w:type="paragraph" w:styleId="Child"><w:basedOn w:val="Parent"/></w:style>"#,
        );
        let target_xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="Parent"><w:rPr><w:i/></w:rPr></w:style>
               <w:style w:type="paragraph" w:styleId="Child"><w:basedOn w:val="Parent"/></w:style>"#,
        );
        let referenced: HashSet<IStr> = ["Child"].iter().map(|s| IStr::from(*s)).collect();
        let collisions = detect_style_collisions(&base_xml, &target_xml, &referenced).unwrap();
        assert_eq!(collisions.len(), 1);
        assert_eq!(collisions[0].style_id, "Parent");
    }

    #[test]
    fn style_level_rsid_alone_is_not_an_active_collision() {
        let base_xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal">
                <w:name w:val="Normal"/><w:rsid w:val="00112233"/>
            </w:style>"#,
        );
        let target_xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal">
                <w:name w:val="Normal"/><w:rsid w:val="AABBCCDD"/>
            </w:style>"#,
        );
        assert!(
            detect_style_collisions(&base_xml, &target_xml, &HashSet::new())
                .unwrap()
                .is_empty()
        );
    }

    /// XML attribute order, namespace prefix choice, and whitespace between
    /// the children of an element-only style element carry no meaning (XML
    /// 1.0 §3.1, Namespaces in XML §6; `CT_Style` has element-only content).
    /// Two packages that spell one style definition differently define the
    /// same style, so they are not an active collision.
    #[test]
    fn style_definition_spelling_alone_is_not_an_active_collision() {
        let base_xml = make_styles_xml(
            r#"<w:style w:type="character" w:default="1" w:styleId="InventedFont"><w:name w:val="Invented Font"/><w:uiPriority w:val="1"/><w:semiHidden/><w:rPr><w:rFonts w:ascii="Alpha" w:hAnsi="Beta"/><w:b/></w:rPr></w:style>"#,
        );
        let target_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
            <q:styles xmlns:q="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
              <q:style q:styleId="InventedFont" q:default="1" q:type="character">
                <q:name q:val="Invented Font" />
                <q:uiPriority q:val="1" />
                <q:semiHidden />
                <q:rPr>
                  <q:rFonts q:hAnsi="Beta" q:ascii="Alpha" />
                  <q:b />
                </q:rPr>
              </q:style>
            </q:styles>"#
            .to_vec();
        let referenced: HashSet<IStr> = ["InventedFont"].iter().map(|s| IStr::from(*s)).collect();
        assert!(
            detect_style_collisions(&base_xml, &target_xml, &referenced)
                .unwrap()
                .is_empty()
        );
    }

    /// The spelling rule above must not hide a real value difference: the same
    /// reordering with one changed attribute value is still a collision.
    #[test]
    fn reordered_style_definition_with_a_changed_value_still_collides() {
        let base_xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="InventedHead"><w:rPr><w:rFonts w:ascii="Alpha" w:hAnsi="Beta"/></w:rPr></w:style>"#,
        );
        let target_xml = make_styles_xml(
            r#"<w:style w:styleId="InventedHead" w:type="paragraph">
                <w:rPr><w:rFonts w:hAnsi="Gamma" w:ascii="Alpha"/></w:rPr>
            </w:style>"#,
        );
        let referenced: HashSet<IStr> = ["InventedHead"].iter().map(|s| IStr::from(*s)).collect();
        let collisions = detect_style_collisions(&base_xml, &target_xml, &referenced).unwrap();
        assert_eq!(collisions.len(), 1);
        assert_eq!(collisions[0].style_id, "InventedHead");
    }

    /// Element children are content, not spelling: an added property is a
    /// different authored definition and stays a collision.
    #[test]
    fn style_definition_with_extra_child_still_collides() {
        let base_xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="InventedHead"><w:rPr><w:b/></w:rPr></w:style>"#,
        );
        let target_xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="InventedHead"><w:rPr><w:b/><w:i/></w:rPr></w:style>"#,
        );
        let referenced: HashSet<IStr> = ["InventedHead"].iter().map(|s| IStr::from(*s)).collect();
        assert_eq!(
            detect_style_collisions(&base_xml, &target_xml, &referenced)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn style_collision_detection_fails_loud_on_malformed_xml() {
        let valid = make_styles_xml("");
        let referenced: HashSet<IStr> = HashSet::new();
        let error = detect_style_collisions(b"<w:styles", &valid, &referenced).unwrap_err();
        assert!(error.contains("base word/styles.xml"), "{error}");
    }

    #[test]
    fn doc_defaults_spelling_alone_compares_equal() {
        let base = make_styles_xml(
            r#"<w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Alpha" w:hAnsi="Beta"/></w:rPr></w:rPrDefault></w:docDefaults>"#,
        );
        let target = br#"<?xml version="1.0" encoding="UTF-8"?>
            <q:styles xmlns:q="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
              <q:docDefaults>
                <q:rPrDefault><q:rPr><q:rFonts q:hAnsi="Beta" q:ascii="Alpha" /></q:rPr></q:rPrDefault>
              </q:docDefaults>
            </q:styles>"#;
        assert!(authored_doc_defaults_equal(&base, target).unwrap());
    }

    #[test]
    fn collision_empty_when_no_shared_styles() {
        let base_xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="StyleA">
                <w:rPr><w:b/></w:rPr>
            </w:style>"#,
        );
        let target_xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="StyleB">
                <w:rPr><w:i/></w:rPr>
            </w:style>"#,
        );
        let referenced: HashSet<IStr> = ["StyleA", "StyleB"]
            .iter()
            .map(|s| IStr::from(*s))
            .collect();
        let collisions = detect_style_collisions(&base_xml, &target_xml, &referenced).unwrap();
        assert!(collisions.is_empty());
    }

    #[test]
    fn collision_empty_inputs() {
        let referenced: HashSet<IStr> = ["Normal"].iter().map(|s| IStr::from(*s)).collect();
        assert!(
            detect_style_collisions(&[], &[], &referenced)
                .unwrap()
                .is_empty()
        );
        assert!(
            detect_style_collisions(b"<w:styles/>", &[], &referenced)
                .unwrap()
                .is_empty()
        );
        assert!(
            detect_style_collisions(&[], b"<w:styles/>", &referenced)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn authored_doc_defaults_compare_structurally() {
        let base = make_styles_xml(
            r#"<w:docDefaults><w:rPrDefault><w:rPr><w:lang w:val="en-AU"/></w:rPr></w:rPrDefault></w:docDefaults>"#,
        );
        let same = make_styles_xml(
            r#"<w:docDefaults><w:rPrDefault><w:rPr><w:lang w:val="en-AU"/></w:rPr></w:rPrDefault></w:docDefaults>"#,
        );
        let different = make_styles_xml(
            r#"<w:docDefaults><w:rPrDefault><w:rPr><w:lang w:val="en-US"/></w:rPr></w:rPrDefault></w:docDefaults>"#,
        );

        assert!(authored_doc_defaults_equal(&base, &same).unwrap());
        assert!(!authored_doc_defaults_equal(&base, &different).unwrap());
    }

    #[test]
    fn authored_doc_defaults_comparison_fails_loud_on_malformed_xml() {
        let valid = make_styles_xml("");
        let error = authored_doc_defaults_equal(b"<w:styles", &valid).unwrap_err();
        assert!(error.contains("base word/styles.xml"), "{error}");
    }

    #[test]
    fn collision_multiple_styles_only_divergent_reported() {
        let base_xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="Normal">
                <w:rPr><w:sz w:val="24"/></w:rPr>
            </w:style>
            <w:style w:type="paragraph" w:styleId="Heading1">
                <w:rPr><w:b/><w:sz w:val="28"/></w:rPr>
            </w:style>
            <w:style w:type="character" w:styleId="BoldChar">
                <w:rPr><w:b/></w:rPr>
            </w:style>"#,
        );
        let target_xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="Normal">
                <w:rPr><w:sz w:val="24"/></w:rPr>
            </w:style>
            <w:style w:type="paragraph" w:styleId="Heading1">
                <w:rPr><w:b/><w:sz w:val="32"/></w:rPr>
            </w:style>
            <w:style w:type="character" w:styleId="BoldChar">
                <w:rPr><w:b/><w:i/></w:rPr>
            </w:style>"#,
        );
        let referenced: HashSet<IStr> = ["Normal", "Heading1", "BoldChar"]
            .iter()
            .map(|s| IStr::from(*s))
            .collect();
        let collisions = detect_style_collisions(&base_xml, &target_xml, &referenced).unwrap();
        // Normal is identical, Heading1 and BoldChar differ.
        assert_eq!(collisions.len(), 2);
        assert_eq!(collisions[0].style_id, "BoldChar");
        assert_eq!(collisions[0].style_type, "character");
        assert_eq!(collisions[1].style_id, "Heading1");
        assert_eq!(collisions[1].style_type, "paragraph");
    }

    #[test]
    fn collision_includes_style_name_when_present() {
        let base_xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="Heading1">
                <w:name w:val="heading 1"/>
                <w:rPr><w:b/><w:sz w:val="28"/></w:rPr>
            </w:style>"#,
        );
        let target_xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="Heading1">
                <w:name w:val="heading 1"/>
                <w:rPr><w:b/><w:sz w:val="32"/></w:rPr>
            </w:style>"#,
        );
        let referenced: HashSet<IStr> = ["Heading1"].iter().map(|s| IStr::from(*s)).collect();
        let collisions = detect_style_collisions(&base_xml, &target_xml, &referenced).unwrap();
        assert_eq!(collisions.len(), 1);
        assert_eq!(collisions[0].style_id, "Heading1");
        assert_eq!(collisions[0].style_name.as_deref(), Some("heading 1"));
    }

    #[test]
    fn collision_style_name_none_when_absent() {
        let base_xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="CustomA">
                <w:rPr><w:b/></w:rPr>
            </w:style>"#,
        );
        let target_xml = make_styles_xml(
            r#"<w:style w:type="paragraph" w:styleId="CustomA">
                <w:rPr><w:i/></w:rPr>
            </w:style>"#,
        );
        let referenced: HashSet<IStr> = ["CustomA"].iter().map(|s| IStr::from(*s)).collect();
        let collisions = detect_style_collisions(&base_xml, &target_xml, &referenced).unwrap();
        assert_eq!(collisions.len(), 1);
        assert_eq!(collisions[0].style_name, None);
    }

    /// Bug 2: When a paragraph style is linked to a character style, the linked
    /// style's **inherited** (basedOn) properties should NOT override properties
    /// that the paragraph style explicitly sets.
    ///
    /// Scenario:
    ///   BaseChar (character): font_size=24
    ///   HeadingChar (character, basedOn=BaseChar): bold only (no font_size)
    ///   HeadingPara (paragraph, link=HeadingChar): font_size=32
    ///
    /// Expected: HeadingPara resolves font_size=32 (from its own rPr).
    /// Bug: HeadingChar's fully-resolved chain gives font_size=24 (from BaseChar),
    ///       which incorrectly overwrites HeadingPara's 32.
    ///
    /// Spec ref: ECMA-376 §17.7.4.6
    #[test]
    fn bug2_linked_style_preserves_para_font_size() {
        let xml = make_styles_xml(
            r#"<w:style w:type="character" w:styleId="BaseChar">
                <w:rPr>
                    <w:sz w:val="24"/>
                </w:rPr>
            </w:style>
            <w:style w:type="character" w:styleId="HeadingChar">
                <w:basedOn w:val="BaseChar"/>
                <w:rPr>
                    <w:b/>
                </w:rPr>
            </w:style>
            <w:style w:type="paragraph" w:styleId="HeadingPara">
                <w:link w:val="HeadingChar"/>
                <w:rPr>
                    <w:sz w:val="32"/>
                </w:rPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).expect("should parse styles XML");

        // Resolve a run with no direct formatting, no char style, para style = HeadingPara.
        let direct = TextMarks::default();
        let resolved = defs.resolve(&direct, None, Some("HeadingPara"));

        // The para style's own font_size=32 should win over BaseChar's inherited 24.
        assert_eq!(
            resolved.font_size,
            Some(32),
            "HeadingPara sets font_size=32; HeadingChar does NOT set font_size \
             (it only inherits 24 from BaseChar). The para style's explicit value must win."
        );

        // The linked char style's own bold=On should still apply.
        assert_eq!(
            resolved.bold,
            MarkValue::On,
            "HeadingChar explicitly sets bold — this should still apply via the link."
        );
    }

    /// Verify that a linked char style's own explicit properties DO override
    /// the para style's properties (the link is not completely ignored).
    #[test]
    fn bug2_linked_style_explicit_property_wins() {
        let xml = make_styles_xml(
            r#"<w:style w:type="character" w:styleId="AccentChar">
                <w:rPr>
                    <w:color w:val="FF0000"/>
                    <w:sz w:val="28"/>
                </w:rPr>
            </w:style>
            <w:style w:type="paragraph" w:styleId="AccentPara">
                <w:link w:val="AccentChar"/>
                <w:rPr>
                    <w:sz w:val="24"/>
                    <w:b/>
                </w:rPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).expect("should parse styles XML");

        let direct = TextMarks::default();
        let resolved = defs.resolve(&direct, None, Some("AccentPara"));

        // AccentChar explicitly sets font_size=28, which should override AccentPara's 24.
        assert_eq!(
            resolved.font_size,
            Some(28),
            "AccentChar explicitly sets font_size=28 — linked char style's explicit property wins."
        );

        // AccentChar explicitly sets color=FF0000. Para doesn't set it.
        assert_eq!(
            resolved.color,
            Some(IStr::from("FF0000")),
            "AccentChar explicitly sets color — should be present."
        );

        // AccentPara sets bold=On; AccentChar doesn't set bold, so para's bold should persist.
        assert_eq!(
            resolved.bold,
            MarkValue::On,
            "AccentPara sets bold; AccentChar is silent on bold — para's bold should persist."
        );
    }

    #[test]
    fn style_explicit_auto_color_is_preserved() {
        let xml = make_styles_xml(
            r#"<w:style w:type="character" w:styleId="AutoColorChar">
                <w:rPr>
                    <w:color w:val="auto"/>
                </w:rPr>
            </w:style>"#,
        );
        let defs = StyleDefinitions::parse(&xml).expect("should parse styles XML");

        let resolved = defs.resolve(&TextMarks::default(), Some("AutoColorChar"), None);
        assert_eq!(
            resolved.color.as_deref(),
            Some("auto"),
            "explicit style-level w:color w:val=\"auto\" must survive resolution",
        );
    }

    // ── Faithful (UN-resolved) style-table projection ────────────────────────

    #[test]
    fn projection_reports_authored_props_without_resolution() {
        // docDefaults uses a THEME font (asciiTheme) + size 22; Normal is the
        // default para style; Body is basedOn Normal and LITERALLY sets Arial 20
        // bold red. The projection must report each w:style AS AUTHORED — Body's
        // font must NOT be folded with docDefaults, and the theme-vs-literal
        // distinction must be preserved.
        let xml = make_styles_xml(
            r#"
            <w:docDefaults>
                <w:rPrDefault><w:rPr>
                    <w:rFonts w:asciiTheme="minorHAnsi" w:hAnsiTheme="minorHAnsi"/>
                    <w:sz w:val="22"/>
                    <w:lang w:val="en-US"/>
                </w:rPr></w:rPrDefault>
            </w:docDefaults>
            <w:style w:type="paragraph" w:default="1" w:styleId="Normal">
                <w:name w:val="Normal"/>
            </w:style>
            <w:style w:type="paragraph" w:styleId="Body">
                <w:name w:val="Body Text"/>
                <w:basedOn w:val="Normal"/>
                <w:rPr>
                    <w:rFonts w:ascii="Arial" w:hAnsi="Arial"/>
                    <w:sz w:val="20"/>
                    <w:b/>
                    <w:color w:val="FF0000"/>
                </w:rPr>
            </w:style>
            "#,
        );

        let proj = style_table_projection(&xml).expect("projection should parse");

        // docDefaults: theme font + is_theme=true + sz 22.
        assert_eq!(proj.doc_default.font_family.as_deref(), Some("minorHAnsi"));
        assert!(proj.doc_default.font_family_is_theme);
        assert_eq!(proj.doc_default.font_size_half_points, Some(22));
        assert_eq!(proj.default_para_style_id.as_deref(), Some("Normal"));

        // Body: authored props, no resolution.
        let body = proj
            .styles
            .iter()
            .find(|s| s.style_id == "Body")
            .expect("Body row present");
        assert_eq!(body.name.as_deref(), Some("Body Text"));
        assert_eq!(body.style_type, "para");
        assert_eq!(body.based_on.as_deref(), Some("Normal"));
        assert_eq!(body.font_family.as_deref(), Some("Arial"));
        assert!(!body.font_family_is_theme, "Arial is a literal typeface");
        assert_eq!(body.font_size_half_points, Some(20));
        assert_eq!(body.color.as_deref(), Some("FF0000"));
        assert_eq!(body.bold, Some(true));
        assert!(!body.is_default);

        // Normal is the default para style and sets no font of its own.
        let normal = proj
            .styles
            .iter()
            .find(|s| s.style_id == "Normal")
            .expect("Normal row present");
        assert!(normal.is_default);
        assert_eq!(normal.font_family, None, "Normal authors no rFonts");
    }

    #[test]
    fn projection_fails_loud_on_malformed_xml() {
        let err =
            style_table_projection(b"<w:styles><w:style>").expect_err("malformed XML must error");
        assert!(err.contains("word/styles.xml"), "{err}");
    }

    #[test]
    fn projection_fails_loud_on_style_without_style_id() {
        let xml = make_styles_xml(r#"<w:style w:type="paragraph"><w:name w:val="X"/></w:style>"#);
        let err = style_table_projection(&xml).expect_err("missing styleId must error");
        assert!(err.contains("w:styleId"), "{err}");
    }

    #[test]
    fn projection_against_real_bold_normal_docx() {
        // The styles.xml inside the real testdata docx must project without error
        // and expose its Normal default + at least one authored style row.
        let docx = include_bytes!("../testdata/style/bold-normal.docx");
        let archive = crate::docx::DocxArchive::read(&docx[..]).expect("open docx");
        let styles_bytes = archive
            .get("word/styles.xml")
            .expect("styles.xml present in bold-normal.docx");
        let proj = style_table_projection(styles_bytes).expect("real styles.xml projects");
        assert!(
            !proj.styles.is_empty(),
            "real document should expose authored styles"
        );
        // Every row is addressable (the fail-loud styleId invariant held).
        assert!(proj.styles.iter().all(|s| !s.style_id.is_empty()));
    }
    fn theme(name: &str) -> FontSelection {
        FontSelection::Theme(IStr::from(name))
    }

    fn named(name: &str) -> FontSelection {
        FontSelection::Named(IStr::from(name))
    }

    /// §17.3.2.26 / §17.7.2: each script slot takes its font from the
    /// nearest level that names it, and within one `w:rFonts` a theme
    /// selector takes precedence over the literal name. A run that writes
    /// out the theme fonts its defaults select selects the same fonts.
    #[test]
    fn a_run_restating_inherited_theme_fonts_selects_the_same_fonts() {
        let xml = make_styles_xml(
            r#"
            <w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:asciiTheme="minorHAnsi" w:hAnsiTheme="minorHAnsi" w:eastAsiaTheme="minorEastAsia" w:cstheme="minorBidi"/></w:rPr></w:rPrDefault></w:docDefaults>
            <w:style w:type="paragraph" w:default="1" w:styleId="Normal"/>
            <w:style w:type="character" w:default="1" w:styleId="DefaultParagraphFont"/>
            <w:style w:type="character" w:styleId="Quote"><w:basedOn w:val="DefaultParagraphFont"/><w:rPr><w:rFonts w:cs="Arial"/></w:rPr></w:style>
        "#,
        );
        let defs = StyleDefinitions::parse(&xml).expect("parse styles");
        let inherited = defs.effective_fonts(None, None, None).expect("provable");
        assert_eq!(inherited.ascii, theme("minorHAnsi"));
        assert_eq!(inherited.east_asia, theme("minorEastAsia"));
        assert_eq!(inherited.cs, theme("minorBidi"));
        let restated = AuthoredRFonts {
            ascii_theme: Some(IStr::from("minorHAnsi")),
            h_ansi_theme: Some(IStr::from("minorHAnsi")),
            east_asia_theme: Some(IStr::from("minorEastAsia")),
            ..AuthoredRFonts::default()
        };
        assert_eq!(
            defs.effective_fonts(Some(&restated), None, None),
            Some(inherited)
        );
        // A theme selector beats the literal written beside it.
        let both = AuthoredRFonts {
            ascii: Some(IStr::from("Arial")),
            ascii_theme: Some(IStr::from("majorHAnsi")),
            ..AuthoredRFonts::default()
        };
        let fonts = defs
            .effective_fonts(Some(&both), None, None)
            .expect("provable");
        assert_eq!(fonts.ascii, theme("majorHAnsi"));
        assert_eq!(fonts.h_ansi, theme("minorHAnsi"));
        // A character style's literal, over no theme for that slot below
        // it... except the defaults' cs theme: not decided.
        assert_eq!(defs.effective_fonts(None, Some("Quote"), None), None);
    }

    /// A literal named where no lower level names a theme for the slot
    /// decides it; one over a lower theme selector is not decided (Word's
    /// precedence across levels is not modelled for every shape).
    #[test]
    fn a_literal_font_over_a_lower_theme_selector_is_not_decided() {
        let xml = make_styles_xml(
            r#"
            <w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Calibri" w:hAnsi="Calibri" w:eastAsiaTheme="minorEastAsia"/></w:rPr></w:rPrDefault></w:docDefaults>
            <w:style w:type="paragraph" w:default="1" w:styleId="Normal"/>
        "#,
        );
        let defs = StyleDefinitions::parse(&xml).expect("parse styles");
        let latin = AuthoredRFonts {
            ascii: Some(IStr::from("Arial")),
            ..AuthoredRFonts::default()
        };
        let fonts = defs
            .effective_fonts(Some(&latin), None, None)
            .expect("provable");
        assert_eq!(fonts.ascii, named("Arial"));
        assert_eq!(fonts.h_ansi, named("Calibri"));
        let east_asian = AuthoredRFonts {
            east_asia: Some(IStr::from("SimSun")),
            ..AuthoredRFonts::default()
        };
        assert_eq!(defs.effective_fonts(Some(&east_asian), None, None), None);
    }

    /// A paragraph style linked to a character style takes its run
    /// properties by a three-way merge (§17.7.4.6) this cascade does not
    /// read; a style missing from the part is not resolved either.
    #[test]
    fn fonts_through_a_linked_or_missing_style_are_not_decided() {
        let xml = make_styles_xml(
            r#"
            <w:style w:type="paragraph" w:styleId="Heading"><w:link w:val="HeadingChar"/></w:style>
            <w:style w:type="character" w:styleId="HeadingChar"><w:link w:val="Heading"/></w:style>
        "#,
        );
        let defs = StyleDefinitions::parse(&xml).expect("parse styles");
        assert_eq!(defs.effective_fonts(None, None, Some("Heading")), None);
        assert_eq!(defs.effective_fonts(None, Some("Missing"), None), None);
        assert!(defs.effective_fonts(None, None, None).is_some());
    }
}
