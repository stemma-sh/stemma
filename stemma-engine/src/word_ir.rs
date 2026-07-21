use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use xmltree::{Element, Namespace, XMLNode};

use crate::domain::{
    FrameWrap, HAnchor, HeightRule, HyperlinkData, HyperlinkRun, IStr, PageOrientation,
    RevisionInfo, SectionProperties, SectionPropertyChange, TextAlignment, TrackingStatus, VAnchor,
    XAlign, YAlign,
};
use crate::xml_attrs::{attr_get, capture_extra_attrs};

const BARRIER_CHAR: char = '\u{FFFC}';
const WORD_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const MC_NS: &str = "http://schemas.openxmlformats.org/markup-compatibility/2006";

/// Recursively clear `namespaces` (the xmlns declaration map) from an element
/// tree so that serialized bytes don't carry redundant namespace declarations.
/// The element's own `namespace` (singular — its namespace URI) and `prefix`
/// are left intact.
fn strip_ns_decls(element: &mut Element) {
    element.namespaces = None;
    for child in &mut element.children {
        if let XMLNode::Element(child_el) = child {
            strip_ns_decls(child_el);
        }
    }
}

/// Walk an element tree and collect all `(prefix, namespace_uri)` pairs used by
/// elements and attributes.  This captures the *used* bindings so we can
/// re-declare exactly those prefixes on the root, making the serialized bytes
/// self-contained regardless of whether the prefix appears in
/// `KNOWN_OOXML_NAMESPACES`.
pub(crate) fn collect_prefix_uri_bindings(element: &Element) -> HashMap<String, String> {
    let mut bindings = HashMap::new();
    collect_prefix_uri_bindings_inner(element, &mut bindings);
    bindings
}

fn collect_prefix_uri_bindings_inner(element: &Element, bindings: &mut HashMap<String, String>) {
    // Element's own namespace URI. A prefixed element (e.g. `w:p`) binds its
    // prefix; an un-prefixed element carrying a namespace (e.g. a foreign
    // `<Insert xmlns="...">` placeholder) binds the DEFAULT namespace, keyed by
    // the empty prefix `""`. Dropping the default binding would strip the
    // `xmlns="..."` declaration and silently re-home the element into the
    // surrounding default namespace on round-trip — a corruption, not a no-op.
    if let Some(uri) = &element.namespace {
        let prefix = element.prefix.clone().unwrap_or_default();
        bindings.entry(prefix).or_insert_with(|| uri.clone());
    }

    // Attribute prefixes + namespace URIs (attributes never use the default
    // namespace, so an un-prefixed attribute carries no binding).
    for attr_name in element.attributes.keys() {
        if let (Some(prefix), Some(uri)) = (&attr_name.prefix, &attr_name.namespace) {
            bindings
                .entry(prefix.clone())
                .or_insert_with(|| uri.clone());
        }
    }

    // Recurse into child elements
    for child in &element.children {
        if let XMLNode::Element(child_el) = child {
            collect_prefix_uri_bindings_inner(child_el, bindings);
        }
    }
}

/// Serialize an XML element to bytes for roundtripping.
///
/// Before writing, all namespace *declarations* (`xmlns:*`) are stripped from
/// every node.  Then, the root element gets exactly the prefix→URI declarations
/// that are actually used within the subtree.  This makes the serialized bytes
/// self-contained — any prefix used inside the tree is declared at the root,
/// including prefixes not in `KNOWN_OOXML_NAMESPACES`.
fn serialize_element(element: &Element) -> Vec<u8> {
    use xmltree::EmitterConfig;

    let bindings = collect_prefix_uri_bindings(element);
    let mut stripped = element.clone();
    strip_ns_decls(&mut stripped);

    // Re-set root namespace declarations to exactly the prefixes used
    // within this subtree, making the raw bytes self-contained.
    if !bindings.is_empty() {
        let mut ns = Namespace::empty();
        for (prefix, uri) in &bindings {
            ns.put(prefix.as_str(), uri.as_str());
        }
        stripped.namespaces = Some(ns);
    }

    let mut buf = Vec::new();
    let config = EmitterConfig::new().write_document_declaration(false);
    // Writing to an in-memory Vec: the only failure mode is the emitter
    // refusing the tree itself (malformed names — a programmer bug, since this
    // tree was parsed from the document). Crash with the invariant named
    // rather than store silently truncated bytes as raw_xml.
    stripped
        .write_with_config(&mut buf, config)
        .expect("re-serializing a parsed element must not fail");
    buf
}

#[derive(Debug)]
pub enum WordIrError {
    UnknownParagraphElement(String),
    UnknownRunElement(String),
    MissingTrackedChangeAttribute(&'static str),
    MissingRequiredAttribute {
        element: String,
        attribute: &'static str,
    },
    /// A tracked-change container nested inside another tracked-change
    /// container reached atom extraction. Silently skipping it (the
    /// earlier behavior) lost the inner revision — e.g. B's pending
    /// deletion of A's pending insertion. Body-level import quarantines
    /// these shapes BEFORE atom extraction; reaching one here means a
    /// context with no quarantine machinery (a story part, a table cell of
    /// a non-quarantined construct) — fail loud, never drop.
    NestedTrackedChange {
        outer: String,
        inner: String,
    },
    /// An element reached the content loop of a tracked-change container
    /// (`w:ins`/`w:del`/`w:moveFrom`/`w:moveTo`) that matches none of its arms.
    /// The container's content model is `CT_RunTrackChange` = `EG_ContentRunContent`
    /// (runs, the transparent `customXml`/`smartTag`/`bdo`/`dir` wrappers, the
    /// `sdt`/`fldSimple`/math widgets, and the zero-width range & revision markup) —
    /// it admits NO property children. A stray element here is therefore unmodeled
    /// CONTENT that the old catch-all silently dropped, losing its text or its
    /// anchoring. Fail loud with the container kind and the element, mirroring the
    /// sibling `NestedTrackedChange` / wrapper-dispatch refusals (CLAUDE.md: no
    /// silent fallbacks).
    UnexpectedTrackedChangeChild {
        container: String,
        element: String,
    },
    /// An mc:Choice `Requires` token names a namespace prefix that has no
    /// in-scope xmlns binding. Per ISO/IEC 29500-3 §7.6 the Requires value
    /// "shall be a whitespace-delimited list of one or more namespace prefixes",
    /// so an unbound prefix is non-conformant markup — we cannot resolve it to a
    /// namespace name to compare against the understood set, and silently
    /// treating it as unsatisfiable would hide a malformed document. Fail loud
    /// with the offending prefix (no silent fallbacks).
    UnresolvableMcRequiresPrefix {
        prefix: String,
    },
    /// An `mc:MustUnderstand` attribute declares a namespace the consumer does
    /// not understand. ISO/IEC 29500-3 §9.4 (Step 3): "signal a mismatch" — the
    /// producer requires the consumer to understand this namespace, so we must
    /// refuse rather than silently drop or mis-render the content.
    McMustUnderstandUnsupported {
        namespace: String,
    },
}

impl fmt::Display for WordIrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WordIrError::UnknownParagraphElement(name) => {
                write!(f, "unknown paragraph-level element: {name}")
            }
            WordIrError::UnknownRunElement(name) => {
                write!(f, "unknown run-level element: {name}")
            }
            WordIrError::MissingTrackedChangeAttribute(attr) => {
                write!(f, "missing required tracked change attribute: {attr}")
            }
            WordIrError::MissingRequiredAttribute { element, attribute } => {
                write!(
                    f,
                    "missing required attribute {attribute} on element {element}"
                )
            }
            WordIrError::NestedTrackedChange { outer, inner } => {
                write!(
                    f,
                    "nested tracked change: <w:{inner}> inside <w:{outer}> is not \
                     representable in this context (stacked revisions); \
                     refusing rather than silently dropping the inner revision"
                )
            }
            WordIrError::UnexpectedTrackedChangeChild { container, element } => {
                write!(
                    f,
                    "unexpected element <{element}> inside tracked change <w:{container}>: \
                     not a known run-level content or revision-markup element \
                     (CT_RunTrackChange admits no property children); refusing rather \
                     than silently dropping it — its text or anchoring would be lost"
                )
            }
            WordIrError::UnresolvableMcRequiresPrefix { prefix } => {
                write!(
                    f,
                    "mc:Choice Requires references namespace prefix {prefix:?} which has no \
                     in-scope xmlns binding (ISO/IEC 29500-3 §7.6: Requires is a list of \
                     namespace prefixes); the document is non-conformant"
                )
            }
            WordIrError::McMustUnderstandUnsupported { namespace } => {
                write!(
                    f,
                    "mc:MustUnderstand requires namespace {namespace:?} which this consumer \
                     does not understand (ISO/IEC 29500-3 §9.4: signal a mismatch); refusing \
                     rather than silently dropping or mis-rendering required content"
                )
            }
        }
    }
}

impl std::error::Error for WordIrError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AtomKind {
    /// Actual text content.
    Text(String),
    /// Tab character (\t).
    Tab,
    /// Non-breaking hyphen (§17.3.3.18). A VISIBLE character displayed with the
    /// hyphen-minus glyph that forbids a line break — NOT a zero-width decoration.
    /// Projects to U+2011 (Unicode NON-BREAKING HYPHEN: faithful glyph + non-breaking
    /// semantics). Modeled like `Tab`: contributes one char and round-trips as the
    /// element (untouched) or as a literal U+2011 in `w:t` on rebuild (Word reads
    /// both identically). softHyphen stays a zero-width Decoration (§17.3.3.29).
    NoBreakHyphen,
    /// Line/page/column break per ISO 29500-1 §17.3.3.1.
    Break(crate::domain::BreakType),
    /// Widget that occupies space (images, embedded objects, etc.).
    /// Contributes U+FFFC to block_text().
    /// Stores the element name and raw XML bytes for roundtripping.
    Widget { name: String, raw_xml: Vec<u8> },
    /// Hyperlink element with embedded data for serialization.
    /// Contributes U+FFFC to block_text() (acts as a barrier).
    Hyperlink(HyperlinkData),
    /// Zero-width decoration (bookmarks, etc.).
    /// Does NOT contribute to block_text().
    /// Stores the element name and raw XML bytes for roundtripping.
    Decoration { name: String, raw_xml: Vec<u8> },
    /// Comment range start marker — preserves w:id for round-tripping.
    CommentRangeStart { id: String },
    /// Comment range end marker — preserves w:id for round-tripping.
    CommentRangeEnd { id: String },
    /// Start of a tracked move container (w:moveTo or w:moveFrom).
    /// The raw_xml stores the wrapper element (without children) for rebuild.
    TrackedMoveStart { raw_xml: Vec<u8> },
    /// End of a tracked move container.
    /// The raw_xml stores the (childless) wrapper element so the serializer
    /// can re-wrap the move content on the import round-trip path. Mirrors
    /// `TrackedMoveStart` — both markers carry the same wrapper bytes.
    TrackedMoveEnd { raw_xml: Vec<u8> },
    /// Start of a bidirectional display-only wrapper (`w:bdo` §17.3.2.3 /
    /// `w:dir` §17.3.2.8). These are TRANSPARENT containers: their inner runs
    /// are parsed as ordinary text atoms (logical order, no revision), so the
    /// wrapper itself is a zero-width marker carrying the childless wrapper
    /// bytes (`<w:bdo w:val="…"/>`/`<w:dir w:val="…"/>`) so the serializer can
    /// re-wrap the inner content on the import round-trip path. Mirrors
    /// `TrackedMoveStart`/`End` — both markers carry the same wrapper bytes.
    BidiWrapperStart { raw_xml: Vec<u8> },
    /// End of a bidirectional display-only wrapper. See `BidiWrapperStart`.
    BidiWrapperEnd { raw_xml: Vec<u8> },
    /// Start of an inline custom-XML / smart-tag wrapper (`w:customXml`
    /// §17.5.1.3 / `w:smartTag` §17.5.1.9). These are TRANSPARENT semantic
    /// containers: their inner runs are ordinary document text and any inner
    /// revisions (`w:del`, `w:moveFrom`/`w:moveTo`) are ordinary revisions, so
    /// the wrapper itself is a zero-width marker carrying the childless wrapper
    /// bytes (attributes + `customXmlPr`/`smartTagPr`, content children cleared)
    /// so the serializer can re-nest the inner content on round-trip. Mirrors
    /// `BidiWrapperStart`/`End` — both markers carry the same wrapper bytes.
    CustomXmlWrapperStart { raw_xml: Vec<u8> },
    /// End of an inline custom-XML / smart-tag wrapper. See
    /// `CustomXmlWrapperStart`.
    CustomXmlWrapperEnd { raw_xml: Vec<u8> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtomOrigin {
    pub run_index: Option<usize>,
    pub child_index: Option<usize>,
    pub paragraph_child_index: Option<usize>,
}

/// Tracking context from a w:ins or w:del container, attached to atoms
/// extracted from within that container.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtomTrackingContext {
    pub is_insertion: bool,   // true = w:ins, false = w:del
    pub revision_id: u32,     // w:id attribute
    pub author: String,       // w:author attribute
    pub date: Option<String>, // w:date attribute (optional per spec)
    /// The STACKED state (one nesting level): the atom sits inside both an
    /// insertion and a deletion, both pending.
    /// Import normalizes BOTH markup orders (`w:del`-in-`w:ins` and
    /// `w:ins`-in-`w:del`) to insertion-primary: when set, `is_insertion` is
    /// `true`, the fields above describe the INSERTION revision, and this
    /// layer carries the DELETION revision.
    pub stacked_deletion: Option<StackedDeletionLayer>,
}

/// The pending-deletion layer of a stacked atom (see
/// [`AtomTrackingContext::stacked_deletion`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StackedDeletionLayer {
    pub revision_id: u32,
    pub author: String,
    pub date: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Atom {
    pub kind: AtomKind,
    pub utf16_len: u32,
    /// Source w:r attributes whose `rsid*` provenance Word can consult during
    /// layout. Stored as qualified-name/value pairs for exact re-emission.
    pub source_run_attrs: Vec<(String, String)>,
    pub origin: AtomOrigin,
    /// Parsed formatting marks from w:rPr.
    pub marks: TextMarks,
    /// Tracking context if this atom came from a w:ins or w:del container.
    pub tracking: Option<AtomTrackingContext>,
}

/// Tri-state value for formatting properties.
/// OOXML formatting can be: inherit (absent), explicitly on, or explicitly off.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum MarkValue {
    /// Property absent - inherit from style.
    #[default]
    Inherit,
    /// Explicitly enabled (<w:b/> or <w:b w:val="1"/>).
    On,
    /// Explicitly disabled (<w:b w:val="0"/>).
    Off,
}

/// Tri-state formatting marks extracted from w:rPr.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct TextMarks {
    pub bold: MarkValue,
    pub italic: MarkValue,
    pub underline: MarkValue,
    pub strike: MarkValue,
    pub double_strike: MarkValue,
    pub subscript: MarkValue,
    pub superscript: MarkValue,
    pub caps: MarkValue,
    pub small_caps: MarkValue,
    pub vanish: MarkValue,
    /// Hidden when displayed as a web page from w:webHidden (§17.3.2.44).
    pub web_hidden: MarkValue,
    pub emboss: MarkValue,
    pub imprint: MarkValue,
    pub outline: MarkValue,
    pub shadow: MarkValue,
    /// Font family from w:rFonts (w:ascii or w:hAnsi attribute).
    pub font_family: Option<IStr>,
    /// Theme font reference for ascii/hAnsi slot (e.g., "majorHAnsi", "minorHAnsi").
    /// Resolved to an actual font name during style resolution via ThemeFonts.
    pub font_family_theme: Option<IStr>,
    /// Font size in half-points from w:sz (e.g., 24 = 12pt).
    pub font_size: Option<u32>,
    /// Text color from w:color w:val (e.g., "FF0000" or "auto").
    pub color: Option<IStr>,
    /// Theme color reference from w:color (themeColor/themeShade/themeTint).
    pub color_theme: Option<crate::domain::ThemeColorRef>,
    /// Highlight color as named color from w:highlight w:val (e.g., "yellow").
    pub highlight: Option<String>,
    /// Underline style from w:u w:val (e.g., "single", "double", "dotted").
    pub underline_style: Option<String>,
    /// East Asian font family from w:rFonts w:eastAsia.
    pub font_east_asia: Option<IStr>,
    /// Theme font reference for eastAsia slot (e.g., "majorEastAsia", "minorEastAsia").
    pub font_east_asia_theme: Option<IStr>,
    /// Complex script font family from w:rFonts w:cs.
    pub font_cs: Option<IStr>,
    /// Theme font reference for cs slot (e.g., "majorBidi", "minorBidi").
    pub font_cs_theme: Option<IStr>,
    /// Language tag from w:lang w:val (e.g., "en-US").
    pub lang: Option<IStr>,
    /// East Asian language tag from w:lang w:eastAsia (e.g., "ja-JP").
    pub lang_east_asia: Option<IStr>,
    /// Character spacing in twips from w:spacing w:val in rPr.
    pub char_spacing: Option<i32>,
    /// Font hint from w:rFonts w:hint (MS-OI29500 §17.3.2.26(b)).
    /// Controls per-character font selection for ambiguous Unicode ranges.
    /// When "eastAsia", characters in ambiguous ranges use the eastAsia font.
    pub font_hint: Option<IStr>,
    /// Complex script override from w:cs (CT_OnOff).
    /// When On, the cs font slot is used for ALL characters regardless of Unicode range.
    /// Per MS-OI29500 §17.3.2.26b.
    pub cs: MarkValue,
    /// Right-to-left override from w:rtl (CT_OnOff).
    /// When On, the cs font slot is used for ALL characters regardless of Unicode range.
    /// Per MS-OI29500 §17.3.2.26b.
    pub rtl: MarkValue,
    /// Complex script bold from w:bCs (MS-OI29500 §17.3.2.1).
    /// When cs/rtl is active, this determines bold instead of w:b.
    pub bold_cs: MarkValue,
    /// Complex script italic from w:iCs (MS-OI29500 §17.3.2.16).
    /// When cs/rtl is active, this determines italic instead of w:i.
    pub italic_cs: MarkValue,
    /// Complex script font size from w:szCs (MS-OI29500 §17.3.2.38), in half-points.
    /// When cs/rtl is active, this determines font size instead of w:sz.
    pub font_size_cs: Option<u32>,
    /// Character style ID from w:rStyle (e.g., "BoldChar", "Emphasis").
    /// Used for style inheritance resolution.
    pub char_style_id: Option<IStr>,
    /// Previous formatting from w:rPrChange (tracked formatting change).
    /// Present when Word tracked a formatting-only change on this run.
    pub rpr_change: Option<Box<RprChange>>,
    /// Run border style from w:bdr w:val (ISO 29500-1 §17.3.2.4).
    pub run_border_style: Option<IStr>,
    /// Run border size in eighth-points from w:bdr w:sz.
    pub run_border_size: Option<u32>,
    /// Run border spacing in points from w:bdr w:space.
    pub run_border_space: Option<u32>,
    /// Run border color as hex RGB from w:bdr w:color.
    pub run_border_color: Option<IStr>,
    /// Vertical position offset in half-points from w:position (ISO 29500-1 §17.3.2.19).
    pub position: Option<i64>,
    /// Kerning threshold in half-points from w:kern (ISO 29500-1 §17.3.2.19a).
    pub kern: Option<i64>,
    /// Character width scaling percentage from w:w (ISO 29500-1 §17.3.2.43).
    pub char_width_scaling: Option<i64>,
    /// Suppress proofing marks from w:noProof (§17.3.2.21).
    pub no_proof: MarkValue,
    /// Special vanish for style separator runs from w:specVanish (§17.3.2.36).
    pub spec_vanish: MarkValue,
    /// Math formatting context from w:oMath (§17.3.2.22).
    pub o_math: MarkValue,
    /// Snap to document grid from w:snapToGrid (§17.3.2.34).
    pub snap_to_grid: MarkValue,
    /// Run-level shading from w:shd (§17.3.2.32).
    /// Stored as (fill, val, color) matching the paragraph shading pattern.
    pub run_shading: Option<(Option<String>, Option<String>, Option<String>)>,
    /// East Asian emphasis mark from w:em w:val (§17.3.2.11).
    pub emphasis_mark: Option<String>,
    /// Animated text effect from w:effect w:val (§17.3.2.12).
    pub text_effect: Option<String>,
    /// Fit text width in twips from w:fitText w:val (§17.3.2.14).
    pub fit_text_width: Option<u32>,
    /// Fit text grouping ID from w:fitText w:id (§17.3.2.14).
    pub fit_text_id: Option<u32>,
    /// Unmodeled rPr children, captured verbatim (see
    /// `crate::domain::PreservedProp`) so they survive re-serialization even
    /// though this parser has no typed field for them.
    pub preserved: Vec<crate::domain::PreservedProp>,
}

/// Tracked formatting change metadata from w:rPrChange.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RprChange {
    /// The "before" formatting marks (from the inner w:rPr of w:rPrChange).
    pub previous_marks: TextMarks,
    /// Revision id (`w:id`); 0 when the markup carried none.
    pub revision_id: u32,
    /// Revision author.
    pub author: String,
    /// Revision date.
    pub date: Option<String>,
}

/// Tracked paragraph formatting change metadata from w:pPrChange (§17.13.5.29).
/// Contains the previous paragraph properties before a tracked formatting change.
///
/// `extract_ppr_change` parses the inner w:pPr with the same direct
/// `extract_*` calls the outer paragraph uses, one per inner-pPr child that
/// has a `previous_*` field on `domain::ParagraphFormattingChange` (see
/// `PPR_CHANGE_MODELED_CHILDREN`) — so reject can restore the snapshot as
/// MODEL state, not just as raw XML. Any other inner-pPr child (e.g.
/// w:suppressLineNumbers, w:numPr, w:outlineLvl) is captured verbatim into
/// `preserved` rather than dropped — the same discipline
/// `ParagraphView::preserved` applies to the outer paragraph's pPr.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PprChange {
    /// Revision id (`w:id`); 0 when the markup carried none.
    pub revision_id: u32,
    /// Previous alignment from the inner w:pPr.
    pub previous_alignment: Option<String>,
    /// Previous indentation from the inner w:pPr.
    pub previous_indentation: Option<IndentProps>,
    /// Previous spacing from the inner w:pPr.
    pub previous_spacing: Option<SpacingProps>,
    /// Previous style ID from the inner w:pPr/w:pStyle.
    pub previous_style_id: Option<IStr>,
    /// Previous paragraph borders from the inner w:pPr/w:pBdr.
    pub previous_borders: Option<ParagraphBorderProps>,
    /// Previous keepNext from the inner w:pPr.
    pub previous_keep_next: Option<bool>,
    /// Previous keepLines from the inner w:pPr.
    pub previous_keep_lines: Option<bool>,
    /// Previous pageBreakBefore from the inner w:pPr.
    pub previous_page_break_before: Option<bool>,
    /// Previous widowControl from the inner w:pPr.
    pub previous_widow_control: Option<bool>,
    /// Previous contextualSpacing from the inner w:pPr.
    pub previous_contextual_spacing: Option<bool>,
    /// Previous paragraph shading from the inner w:pPr/w:shd: (fill, val, color).
    pub previous_shading: Option<(Option<String>, Option<String>, Option<String>)>,
    /// Previous direct tab stops from the inner w:pPr/w:tabs.
    pub previous_tab_stops: Option<Vec<TabStopDef>>,
    /// Previous mirrorIndents from the inner w:pPr (three-state, matching
    /// `ParagraphNode::mirror_indents`).
    pub previous_mirror_indents: Option<bool>,
    /// Previous autoSpaceDE from the inner w:pPr.
    pub previous_auto_space_de: Option<bool>,
    /// Previous autoSpaceDN from the inner w:pPr.
    pub previous_auto_space_dn: Option<bool>,
    /// Previous bidi from the inner w:pPr (three-state, matching
    /// `ParagraphNode::bidi`).
    pub previous_bidi: Option<bool>,
    /// Previous textAlignment from the inner w:pPr.
    pub previous_text_alignment: Option<TextAlignment>,
    /// Previous textDirection from the inner w:pPr.
    pub previous_text_direction: Option<crate::domain::TextDirection>,
    /// Previous suppressAutoHyphens from the inner w:pPr.
    pub previous_suppress_auto_hyphens: Option<bool>,
    /// Previous snapToGrid from the inner w:pPr.
    pub previous_snap_to_grid: Option<bool>,
    /// Previous overflowPunct from the inner w:pPr.
    pub previous_overflow_punct: Option<bool>,
    /// Previous adjustRightInd from the inner w:pPr.
    pub previous_adjust_right_ind: Option<bool>,
    /// Previous wordWrap from the inner w:pPr.
    pub previous_word_wrap: Option<bool>,
    /// Previous framePr from the inner w:pPr.
    pub previous_frame_pr: Option<FrameProperties>,
    /// Previous paragraph mark run properties from the inner w:pPr/w:rPr.
    pub previous_paragraph_mark_rpr: TextMarks,
    /// Unmodeled children of the inner w:pPr, captured verbatim (see
    /// `crate::domain::PreservedProp`) so they survive re-serialization and
    /// are restored onto the paragraph's own pPr remainder when this change
    /// is rejected, even though this parser has no typed field for them.
    pub preserved: Vec<crate::domain::PreservedProp>,
    /// Revision author.
    pub author: String,
    /// Revision date.
    pub date: Option<String>,
}

/// Local names of every inner-pPr child `extract_ppr_change` extracts into a
/// typed `PprChange` field. Mirrors `MODELED_PPR_CHILDREN`'s role for the
/// outer paragraph: anything NOT in this list falls through to `preserved`.
///
/// Deliberately absent (they stay in `preserved` because
/// `ParagraphFormattingChange` has no `previous_*` field for them):
/// - `numPr` — `previous_numbering` needs the document-order numbering
///   counter state to synthesize `NumberingInfo`; import can't produce it
///   here, so the snapshot's numPr round-trips verbatim instead.
/// - `outlineLvl`, `cnfStyle` — no previous_* domain field (yet).
const PPR_CHANGE_MODELED_CHILDREN: &[&str] = &[
    "jc",
    "ind",
    "spacing",
    "rPr",
    "pStyle",
    "pBdr",
    "shd",
    "keepNext",
    "keepLines",
    "pageBreakBefore",
    "widowControl",
    "contextualSpacing",
    "tabs",
    "mirrorIndents",
    "autoSpaceDE",
    "autoSpaceDN",
    "bidi",
    "textAlignment",
    "textDirection",
    "suppressAutoHyphens",
    "snapToGrid",
    "overflowPunct",
    "adjustRightInd",
    "wordWrap",
    "framePr",
];

/// Numbering properties extracted from w:pPr/w:numPr.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NumProps {
    pub num_id: u32,
    pub ilvl: u32,
}

/// What the paragraph's direct w:numPr says about numbering.
///
/// Per §17.9.18, numId=0 explicitly removes inherited numbering —
/// it is NOT the same as "no numPr element at all." We model this
/// as a three-state enum so downstream code (pStyle reverse binding,
/// style resolution) can distinguish the cases.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DirectNumPr {
    /// No w:numPr element on this paragraph.
    Absent,
    /// w:numId val="0" — explicitly suppress inherited numbering (§17.9.18).
    Suppressed,
    /// Active numbering: numId > 0 with an ilvl.
    Active(NumProps),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParagraphView {
    pub atoms: Vec<Atom>,
    /// Numbering directive from w:pPr/w:numPr.
    pub num_props: DirectNumPr,
    /// Style ID from w:pPr/w:pStyle.
    pub style_id: Option<IStr>,
    /// Alignment from w:pPr/w:jc.
    pub alignment: Option<String>,
    /// Indentation from w:pPr/w:ind.
    pub indentation: Option<IndentProps>,
    /// Spacing from w:pPr/w:spacing.
    pub spacing: Option<SpacingProps>,
    /// Paragraph borders from w:pPr/w:pBdr.
    pub borders: Option<ParagraphBorderProps>,
    /// Keep paragraph with next (w:keepNext, §17.3.1.14).
    /// None = absent (inherit from style), Some(false) = explicitly off, Some(true) = on.
    pub keep_next: Option<bool>,
    /// Keep all lines on same page (w:keepLines, §17.3.1.15).
    /// None = absent (inherit from style), Some(false) = explicitly off, Some(true) = on.
    pub keep_lines: Option<bool>,
    /// Force page break before paragraph (w:pageBreakBefore).
    /// None = absent (inherit from style), Some(false) = explicitly off, Some(true) = on.
    pub page_break_before: Option<bool>,
    /// Widow/orphan control (w:widowControl). None = inherit, Some(false) = explicitly disabled.
    pub widow_control: Option<bool>,
    /// Suppress spacing between adjacent same-style paragraphs (w:contextualSpacing, §17.3.1.9).
    /// None = absent (inherit from style), Some(false) = explicitly off, Some(true) = on.
    pub contextual_spacing: Option<bool>,
    /// Paragraph shading from w:pPr/w:shd: (fill, val, color).
    pub paragraph_shading: Option<(Option<String>, Option<String>, Option<String>)>,
    /// Outline level from w:pPr/w:outlineLvl (0-based, 0 = level 1).
    pub outline_lvl: Option<u8>,
    /// Direct tab stops from w:pPr/w:tabs (before style resolution).
    /// None = not specified (inherit from style); Some = explicit (may include "clear" entries).
    pub tab_stops: Option<Vec<TabStopDef>>,
    /// Tracked change for section properties (w:sectPrChange inside w:sectPr).
    pub section_property_change: Option<SectionPropertyChange>,
    /// Tracked paragraph formatting change from w:pPrChange (§17.13.5.29).
    /// Present when Word tracked a paragraph-level formatting change.
    pub ppr_change: Option<PprChange>,
    /// Structured section properties parsed from w:sectPr.
    pub section_properties: Option<SectionProperties>,
    /// Paragraph mark tracking status from w:pPr/w:rPr (w:del or w:ins).
    /// Represents tracked paragraph joins/splits (§17.13.5.28).
    pub para_mark_status: Option<TrackingStatus>,
    /// Direct paragraph-mark run properties from w:pPr/w:rPr.
    /// These format the paragraph mark itself, not the text runs in the paragraph.
    pub paragraph_mark_rpr: TextMarks,
    /// Mirror indents for facing pages (w:mirrorIndents, §17.3.1.18).
    /// Mirror indents for facing pages (w:mirrorIndents, §17.3.1.18).
    /// None = absent (inherit), Some(false) = explicitly off, Some(true) = on.
    pub mirror_indents: Option<bool>,
    /// Automatically adjust spacing between Latin and East Asian text (w:autoSpaceDE).
    pub auto_space_de: Option<bool>,
    /// Automatically adjust spacing between East Asian and Latin text (w:autoSpaceDN).
    pub auto_space_dn: Option<bool>,
    /// Right-to-left paragraph layout (w:bidi, §17.3.1.6).
    /// None = absent (inherit), Some(false) = explicitly off, Some(true) = on.
    pub bidi: Option<bool>,
    /// Vertical character alignment on each line (w:textAlignment, §17.3.1.39).
    pub text_alignment: Option<TextAlignment>,
    /// Text direction for paragraph (w:textDirection, §17.3.1.40).
    pub text_direction: Option<crate::domain::TextDirection>,
    /// Suppress automatic hyphenation (w:suppressAutoHyphens, §17.3.1.34).
    pub suppress_auto_hyphens: Option<bool>,
    /// Use document grid settings (w:snapToGrid, §17.3.1.32).
    pub snap_to_grid: Option<bool>,
    /// Punctuation overflow (w:overflowPunct, §17.3.1.21).
    pub overflow_punct: Option<bool>,
    /// Auto-adjust right indent for document grid (w:adjustRightInd, §17.3.1.1).
    pub adjust_right_ind: Option<bool>,
    /// Character-level vs word-level line breaking (w:wordWrap, §17.3.1.45).
    pub word_wrap: Option<bool>,
    /// Text frame properties (w:framePr, §17.3.1.11).
    pub frame_pr: Option<FrameProperties>,
    /// Conditional formatting flags (w:cnfStyle, §17.3.1.8).
    pub cnf_style: Option<crate::domain::CnfStyle>,
    /// Unmodeled pPr children, captured verbatim (see
    /// `crate::domain::PreservedProp`) so they survive re-serialization even
    /// though this parser has no typed field for them.
    pub preserved: Vec<crate::domain::PreservedProp>,
}

/// Local names of every w:pPr child this parser extracts into a typed
/// `ParagraphView` field. Kept in one place so the unknown-child scan in
/// `ParagraphView::from_paragraph` has a single source of truth: anything NOT
/// in this list falls through to the preserved remainder. A pPr containing
/// every element listed here must produce an EMPTY remainder — see
/// `ppr_walk_covers_every_modeled_child` — which guards this list against
/// drifting out of sync with the extract_* helpers above.
const MODELED_PPR_CHILDREN: &[&str] = &[
    "pStyle",
    "numPr",
    "jc",
    "ind",
    "spacing",
    "pBdr",
    "shd",
    "keepNext",
    "keepLines",
    "pageBreakBefore",
    "widowControl",
    "contextualSpacing",
    "outlineLvl",
    "tabs",
    "sectPr",
    "pPrChange",
    "rPr",
    "mirrorIndents",
    "autoSpaceDE",
    "autoSpaceDN",
    "bidi",
    "textAlignment",
    "textDirection",
    "suppressAutoHyphens",
    "snapToGrid",
    "overflowPunct",
    "adjustRightInd",
    "wordWrap",
    "framePr",
    "cnfStyle",
];

/// Text frame properties from w:framePr (§17.3.1.11, CT_FramePr).
///
/// Extraction-edge twin of [`crate::domain::FrameProperties`]; see that type
/// for the modeled-vs-`extra_attrs` split. Kept field-for-field in sync so the
/// import mapping is a straight copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameProperties {
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub h_rule: Option<HeightRule>,
    pub h_space: Option<i64>,
    pub v_space: Option<i64>,
    pub wrap: Option<FrameWrap>,
    pub v_anchor: Option<VAnchor>,
    pub h_anchor: Option<HAnchor>,
    pub x: Option<i64>,
    pub x_align: Option<XAlign>,
    pub y: Option<i64>,
    pub y_align: Option<YAlign>,
    /// CT_FramePr attributes not modeled above, captured and re-emitted verbatim.
    pub extra_attrs: Vec<(String, String)>,
}

/// Indentation properties extracted from w:pPr/w:ind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndentProps {
    pub left: Option<i32>,
    pub right: Option<i32>,
    pub effective_first_line_twips: Option<i32>,
    /// Left/start indent in character units (hundredths of a character width).
    /// Non-zero value takes precedence over twip `left` (MS-OI29500 2.1.44).
    pub start_chars: Option<i32>,
    /// Right/end indent in character units.
    pub end_chars: Option<i32>,
    /// First line indent in character units.
    pub first_line_chars: Option<i32>,
    /// Hanging indent in character units.
    pub hanging_chars: Option<i32>,
}

/// Spacing properties extracted from w:pPr/w:spacing (§17.3.1.33).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpacingProps {
    pub before: Option<u32>,
    pub after: Option<u32>,
    /// Space before in hundredths of a line (100 = one line).
    /// Per §17.3.1.33, takes precedence over `before` when both are present.
    pub before_lines: Option<u32>,
    /// Space after in hundredths of a line (100 = one line).
    /// Per §17.3.1.33, takes precedence over `after` when both are present.
    pub after_lines: Option<u32>,
    /// §17.3.1.33: when true, `before` and `before_lines` are ignored and spacing
    /// is automatically determined by the consumer (matching HTML default `<p>` margins).
    pub before_autospacing: Option<bool>,
    /// §17.3.1.33: when true, `after` and `after_lines` are ignored and spacing
    /// is automatically determined by the consumer (matching HTML default `<p>` margins).
    pub after_autospacing: Option<bool>,
    pub line: Option<u32>,
    pub line_rule: Option<String>,
}

/// Paragraph border properties extracted from w:pPr/w:pBdr (§17.3.1.24).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParagraphBorderProps {
    pub top: Option<BorderEdge>,
    pub bottom: Option<BorderEdge>,
    pub left: Option<BorderEdge>,
    pub right: Option<BorderEdge>,
    pub between: Option<BorderEdge>,
    pub bar: Option<BorderEdge>,
}

/// A single border edge extracted from XML.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BorderEdge {
    pub style: String,
    pub color: Option<String>,
    pub size: Option<u32>,
    /// w:space — border offset from the object edge, in points (§17.3.4).
    /// The pBdr path previously dropped this; cell/table borders kept it.
    pub space: Option<u32>,
}

/// A single tab stop definition from w:pPr/w:tabs/w:tab.
///
/// `position` is measured in twips relative to the paragraph's leading edge
/// (left edge for LTR text, right edge for RTL).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TabStopDef {
    pub position: i32,
    /// Tab stop alignment (ECMA-376 §17.18.81 ST_TabJc).
    pub alignment: crate::domain::TabAlignment,
    /// Tab stop leader character (ECMA-376 §17.18.82 ST_TabTlc).
    pub leader: Option<crate::domain::TabLeader>,
}

impl ParagraphView {
    pub fn from_paragraph(
        paragraph: &Element,
        rel_lookup: &std::collections::HashMap<String, String>,
    ) -> Result<Self, WordIrError> {
        let mut atoms = Vec::new();
        let mut num_props = DirectNumPr::Absent;
        let mut style_id = None;
        let mut alignment = None;
        let mut indentation = None;
        let mut spacing = None;
        let mut borders = None;
        let mut keep_next = None;
        let mut keep_lines = None;
        let mut page_break_before = None;
        let mut widow_control = None;
        let mut contextual_spacing = None;
        let mut paragraph_shading = None;
        let mut outline_lvl = None;
        let mut tab_stops = None;
        let mut section_property_change = None;
        let mut ppr_change = None;
        let mut section_properties = None;
        let mut para_mark_status = None;
        let mut paragraph_mark_rpr = TextMarks::default();
        let mut mirror_indents = None;
        let mut auto_space_de = None;
        let mut auto_space_dn = None;
        let mut bidi = None;
        let mut text_alignment = None;
        let mut text_direction = None;
        let mut suppress_auto_hyphens = None;
        let mut snap_to_grid = None;
        let mut overflow_punct = None;
        let mut adjust_right_ind = None;
        let mut word_wrap = None;
        let mut frame_pr = None;
        let mut cnf_style = None;
        let mut preserved = Vec::new();

        for (index, child) in paragraph.children.iter().enumerate() {
            let element = match child {
                XMLNode::Element(el) => el,
                _ => continue,
            };

            // Extract paragraph properties from pPr
            if is_w_tag(element, "pPr") {
                num_props = extract_num_props(element);
                style_id = extract_style_id(element);
                alignment = extract_alignment(element);
                indentation = extract_indentation(element);
                spacing = extract_spacing(element);
                borders = extract_paragraph_borders(element);
                keep_next = extract_keep_next(element);
                keep_lines = extract_keep_lines(element);
                page_break_before = extract_page_break_before(element);
                widow_control = extract_widow_control(element);
                contextual_spacing = extract_contextual_spacing(element);
                paragraph_shading = extract_paragraph_shading(element);
                outline_lvl = extract_outline_lvl(element);
                tab_stops = extract_tab_stops(element);
                section_property_change = extract_section_property_change(element);
                ppr_change = extract_ppr_change(element);
                section_properties = extract_section_properties(element, rel_lookup);
                para_mark_status = extract_para_mark_status(element);
                paragraph_mark_rpr = extract_paragraph_mark_rpr(element);
                mirror_indents = extract_optional_bool(element, "mirrorIndents");
                auto_space_de = extract_optional_bool(element, "autoSpaceDE");
                auto_space_dn = extract_optional_bool(element, "autoSpaceDN");
                bidi = extract_optional_bool(element, "bidi");
                text_alignment = extract_text_alignment(element);
                text_direction = find_w_child(element, "textDirection")
                    .and_then(|el| attr_value(el, "val"))
                    .and_then(|s| crate::domain::TextDirection::from_xml_str(s).ok());
                suppress_auto_hyphens = extract_optional_bool(element, "suppressAutoHyphens");
                snap_to_grid = extract_optional_bool(element, "snapToGrid");
                overflow_punct = extract_optional_bool(element, "overflowPunct");
                adjust_right_ind = extract_optional_bool(element, "adjustRightInd");
                word_wrap = extract_optional_bool(element, "wordWrap");
                frame_pr = extract_frame_pr(element);
                cnf_style = extract_cnf_style(element);

                // --- Preserved remainder: unmodeled pPr child ---
                //
                // A pPr child element this parser doesn't model (e.g.
                // w:suppressLineNumbers, w:kinsoku, or a foreign-namespace
                // extension) is captured verbatim here rather than dropped —
                // the same discipline `parse_rpr_element` applies to rPr.
                // `build_paragraph_properties`'s preserved-child post-pass
                // re-emits it at its Annex-A position (or at the end of pPr
                // for names outside the ordering table) on re-serialization.
                for ppr_child in &element.children {
                    let ppr_el = match ppr_child {
                        XMLNode::Element(el) => el,
                        _ => continue,
                    };
                    let ppr_local = local_element_name(ppr_el);
                    if MODELED_PPR_CHILDREN.contains(&ppr_local.as_str()) {
                        continue;
                    }
                    tracing::debug!(
                        element = %ppr_local,
                        "ParagraphView::from_paragraph: unmodeled pPr child element captured verbatim as a preserved remainder"
                    );
                    preserved.push(crate::domain::PreservedProp {
                        name: qualified_element_name(ppr_el),
                        raw_xml: String::from_utf8(serialize_element(ppr_el))
                            .expect("serialize_element always emits valid UTF-8 XML"),
                    });
                }

                continue;
            }

            // Handle runs - contain actual text content
            if is_w_tag(element, "r") {
                atoms.extend(run_atoms(element, index)?);
                continue;
            }

            // Handle tracked changes containers (del/ins) - recurse into their runs
            if is_w_tag(element, "del") || is_w_tag(element, "ins") {
                atoms.extend(tracked_change_atoms(element, index)?);
                continue;
            }

            // Get the local element name for categorization
            let local_name = local_element_name(element);

            // Check if it's a paragraph-level widget (container that we can't edit across)
            if is_paragraph_widget(&local_name) {
                atoms.push(Atom {
                    kind: AtomKind::Widget {
                        name: element.name.clone(),
                        raw_xml: serialize_element(element),
                    },
                    utf16_len: 1, // Occupies space as a single barrier
                    source_run_attrs: Vec::new(),
                    origin: AtomOrigin {
                        run_index: None,
                        child_index: None,
                        paragraph_child_index: Some(index),
                    },
                    marks: TextMarks::default(),
                    tracking: None,
                });
                continue;
            }

            // Handle hyperlinks specially - extract data for serialization.
            // Range markers nested inside the link are hoisted to its edges
            // (see hoisted_hyperlink_range_markers) so their pairs survive.
            if local_name == "hyperlink" {
                let (markers_before, markers_after) = hoisted_hyperlink_range_markers(element);
                push_hoisted_marker_atoms(markers_before, index, &mut atoms);
                let data = extract_hyperlink_data(element);
                atoms.push(Atom {
                    kind: AtomKind::Hyperlink(data),
                    utf16_len: 1, // Occupies space as a single barrier
                    source_run_attrs: Vec::new(),
                    origin: AtomOrigin {
                        run_index: None,
                        child_index: None,
                        paragraph_child_index: Some(index),
                    },
                    marks: TextMarks::default(),
                    tracking: None,
                });
                push_hoisted_marker_atoms(markers_after, index, &mut atoms);
                continue;
            }

            // Bidirectional display-only wrappers (w:bdo §17.3.2.3 / w:dir
            // §17.3.2.8). TRANSPARENT containers: descend into the inner runs so
            // their (logical-order) text reads through, carry NO revision, and
            // preserve the wrapper element for byte-verbatim round-trip. Mirrors
            // the moveFrom/moveTo start/end-marker shape below.
            if local_name == "bdo" || local_name == "dir" {
                atoms.extend(bidi_wrapper_atoms(element, index)?);
                continue;
            }

            // Inline custom-XML / smart-tag wrappers (w:customXml §17.5.1.3 /
            // w:smartTag §17.5.1.9). TRANSPARENT semantic containers: descend
            // into the inner runs (ordinary text) and inner revisions (ordinary
            // revisions), preserving the wrapper + its customXmlPr/smartTagPr
            // for byte-verbatim round-trip. Mirrors the bdo/dir shape above.
            if local_name == "customXml" || local_name == "smartTag" {
                atoms.extend(custom_xml_wrapper_atoms(element, index)?);
                continue;
            }

            // Handle tracked move containers (moveFrom/moveTo) - similar to del/ins
            // but wrapped with start/end markers for roundtrip fidelity.
            if is_w_tag(element, "moveFrom") || is_w_tag(element, "moveTo") {
                // Emit start marker (wrapper element without children)
                let mut template = element.clone();
                template.children.clear();
                atoms.push(Atom {
                    kind: AtomKind::TrackedMoveStart {
                        raw_xml: serialize_element(&template),
                    },
                    utf16_len: 0,
                    source_run_attrs: Vec::new(),
                    origin: AtomOrigin {
                        run_index: None,
                        child_index: None,
                        paragraph_child_index: Some(index),
                    },
                    marks: TextMarks::default(),
                    tracking: None,
                });
                // Flatten inner runs as normal atoms (text visible for diffing)
                atoms.extend(tracked_change_atoms(element, index)?);
                // Emit end marker carrying the same childless wrapper bytes,
                // so the serializer can re-wrap the move content on round-trip.
                atoms.push(Atom {
                    kind: AtomKind::TrackedMoveEnd {
                        raw_xml: serialize_element(&template),
                    },
                    utf16_len: 0,
                    source_run_attrs: Vec::new(),
                    origin: AtomOrigin {
                        run_index: None,
                        child_index: None,
                        paragraph_child_index: Some(index),
                    },
                    marks: TextMarks::default(),
                    tracking: None,
                });
                continue;
            }

            // Comment range markers — extract w:id for typed round-tripping
            if local_name == "commentRangeStart" || local_name == "commentRangeEnd" {
                let id = attr_get(element, "w:id").cloned().ok_or_else(|| {
                    WordIrError::MissingRequiredAttribute {
                        element: local_name.to_string(),
                        attribute: "w:id",
                    }
                })?;
                atoms.push(Atom {
                    kind: if local_name == "commentRangeStart" {
                        AtomKind::CommentRangeStart { id }
                    } else {
                        AtomKind::CommentRangeEnd { id }
                    },
                    utf16_len: 0,
                    source_run_attrs: Vec::new(),
                    origin: AtomOrigin {
                        run_index: None,
                        child_index: None,
                        paragraph_child_index: Some(index),
                    },
                    marks: TextMarks::default(),
                    tracking: None,
                });
                continue;
            }

            // Check if it's a known decoration (zero-width marker)
            if is_paragraph_decoration(&local_name) {
                atoms.push(Atom {
                    kind: AtomKind::Decoration {
                        name: element.name.clone(),
                        raw_xml: serialize_element(element),
                    },
                    utf16_len: 0, // Zero-width!
                    source_run_attrs: Vec::new(),
                    origin: AtomOrigin {
                        run_index: None,
                        child_index: None,
                        paragraph_child_index: Some(index),
                    },
                    marks: TextMarks::default(),
                    tracking: None,
                });
                continue;
            }

            // MC AlternateContent at paragraph level — select branch and
            // process its children as paragraph-level elements.
            if is_mc_alternate_content(element) {
                if let Some(branch) = select_mc_branch(element)? {
                    for branch_child in &branch.children {
                        let bel = match branch_child {
                            XMLNode::Element(el) => el,
                            _ => continue,
                        };
                        // Skip paragraph properties inside MC branch
                        if is_w_tag(bel, "pPr") {
                            continue;
                        }
                        if is_w_tag(bel, "r") {
                            atoms.extend(run_atoms(bel, index)?);
                        } else if is_w_tag(bel, "del") || is_w_tag(bel, "ins") {
                            atoms.extend(tracked_change_atoms(bel, index)?);
                        } else if is_w_tag(bel, "moveFrom") || is_w_tag(bel, "moveTo") {
                            let mut template = bel.clone();
                            template.children.clear();
                            atoms.push(Atom {
                                kind: AtomKind::TrackedMoveStart {
                                    raw_xml: serialize_element(&template),
                                },
                                utf16_len: 0,
                                source_run_attrs: Vec::new(),
                                origin: AtomOrigin {
                                    run_index: None,
                                    child_index: None,
                                    paragraph_child_index: Some(index),
                                },
                                marks: TextMarks::default(),
                                tracking: None,
                            });
                            atoms.extend(tracked_change_atoms(bel, index)?);
                            atoms.push(Atom {
                                kind: AtomKind::TrackedMoveEnd {
                                    raw_xml: serialize_element(&template),
                                },
                                utf16_len: 0,
                                source_run_attrs: Vec::new(),
                                origin: AtomOrigin {
                                    run_index: None,
                                    child_index: None,
                                    paragraph_child_index: Some(index),
                                },
                                marks: TextMarks::default(),
                                tracking: None,
                            });
                        } else {
                            let bl = local_element_name(bel);
                            if is_paragraph_widget(&bl) {
                                atoms.push(Atom {
                                    kind: AtomKind::Widget {
                                        name: bel.name.clone(),
                                        raw_xml: serialize_element(bel),
                                    },
                                    utf16_len: 1,
                                    source_run_attrs: Vec::new(),
                                    origin: AtomOrigin {
                                        run_index: None,
                                        child_index: None,
                                        paragraph_child_index: Some(index),
                                    },
                                    marks: TextMarks::default(),
                                    tracking: None,
                                });
                            } else if bl == "hyperlink" {
                                // Same edge-hoisting of nested range markers
                                // as the direct paragraph-child hyperlink path.
                                let (markers_before, markers_after) =
                                    hoisted_hyperlink_range_markers(bel);
                                push_hoisted_marker_atoms(markers_before, index, &mut atoms);
                                let data = extract_hyperlink_data(bel);
                                atoms.push(Atom {
                                    kind: AtomKind::Hyperlink(data),
                                    utf16_len: 1,
                                    source_run_attrs: Vec::new(),
                                    origin: AtomOrigin {
                                        run_index: None,
                                        child_index: None,
                                        paragraph_child_index: Some(index),
                                    },
                                    marks: TextMarks::default(),
                                    tracking: None,
                                });
                                push_hoisted_marker_atoms(markers_after, index, &mut atoms);
                            } else if bl == "commentRangeStart" || bl == "commentRangeEnd" {
                                let id = attr_get(bel, "w:id").cloned().ok_or_else(|| {
                                    WordIrError::MissingRequiredAttribute {
                                        element: bl.to_string(),
                                        attribute: "w:id",
                                    }
                                })?;
                                atoms.push(Atom {
                                    kind: if bl == "commentRangeStart" {
                                        AtomKind::CommentRangeStart { id }
                                    } else {
                                        AtomKind::CommentRangeEnd { id }
                                    },
                                    utf16_len: 0,
                                    source_run_attrs: Vec::new(),
                                    origin: AtomOrigin {
                                        run_index: None,
                                        child_index: None,
                                        paragraph_child_index: Some(index),
                                    },
                                    marks: TextMarks::default(),
                                    tracking: None,
                                });
                            } else if is_paragraph_decoration(&bl) || is_run_decoration(&bl) {
                                atoms.push(Atom {
                                    kind: AtomKind::Decoration {
                                        name: bel.name.clone(),
                                        raw_xml: serialize_element(bel),
                                    },
                                    utf16_len: 0,
                                    source_run_attrs: Vec::new(),
                                    origin: AtomOrigin {
                                        run_index: None,
                                        child_index: None,
                                        paragraph_child_index: Some(index),
                                    },
                                    marks: TextMarks::default(),
                                    tracking: None,
                                });
                            } else if bl == "tab" {
                                atoms.push(Atom {
                                    kind: AtomKind::Tab,
                                    utf16_len: 1,
                                    source_run_attrs: Vec::new(),
                                    origin: AtomOrigin {
                                        run_index: None,
                                        child_index: None,
                                        paragraph_child_index: Some(index),
                                    },
                                    marks: TextMarks::default(),
                                    tracking: None,
                                });
                            } else if bl == "br" || bl == "cr" {
                                let break_type = if bl == "cr" {
                                    crate::domain::BreakType::TextWrapping
                                } else {
                                    match attr_get(bel, "w:type").map(|s| s.as_str()) {
                                        Some("page") => crate::domain::BreakType::Page,
                                        Some("column") => crate::domain::BreakType::Column,
                                        _ => crate::domain::BreakType::TextWrapping,
                                    }
                                };
                                atoms.push(Atom {
                                    kind: AtomKind::Break(break_type),
                                    utf16_len: 1,
                                    source_run_attrs: Vec::new(),
                                    origin: AtomOrigin {
                                        run_index: None,
                                        child_index: None,
                                        paragraph_child_index: Some(index),
                                    },
                                    marks: TextMarks::default(),
                                    tracking: None,
                                });
                            } else if is_run_widget(&bl) {
                                // Run-level widget inside MC at paragraph level.
                                let raw_xml = serialize_element(bel);
                                let name = mc_inner_content_name(bel)?;
                                atoms.push(Atom {
                                    kind: AtomKind::Widget { name, raw_xml },
                                    utf16_len: 1,
                                    source_run_attrs: Vec::new(),
                                    origin: AtomOrigin {
                                        run_index: None,
                                        child_index: None,
                                        paragraph_child_index: Some(index),
                                    },
                                    marks: TextMarks::default(),
                                    tracking: None,
                                });
                            } else if is_foreign_namespace_element(bel) {
                                // Foreign-namespace element inside an MC branch —
                                // preserve verbatim as a zero-width decoration, same
                                // as the non-MC paragraph-level path.
                                atoms.push(Atom {
                                    kind: AtomKind::Decoration {
                                        name: bel.name.clone(),
                                        raw_xml: serialize_element(bel),
                                    },
                                    utf16_len: 0,
                                    source_run_attrs: Vec::new(),
                                    origin: AtomOrigin {
                                        run_index: None,
                                        child_index: None,
                                        paragraph_child_index: Some(index),
                                    },
                                    marks: TextMarks::default(),
                                    tracking: None,
                                });
                            } else {
                                // Unknown element inside MC branch — fail fast,
                                // same as the non-MC paragraph-level path.
                                return Err(WordIrError::UnknownParagraphElement(bel.name.clone()));
                            }
                        }
                    }
                }
                continue;
            }

            // Run-level decorations appearing bare at paragraph level (non-conformant
            // but produced by Word and other tools). Treat identically to paragraph
            // decorations — zero-width markers preserved for roundtrip fidelity.
            if is_run_decoration(&local_name) {
                atoms.push(Atom {
                    kind: AtomKind::Decoration {
                        name: element.name.clone(),
                        raw_xml: serialize_element(element),
                    },
                    utf16_len: 0,
                    source_run_attrs: Vec::new(),
                    origin: AtomOrigin {
                        run_index: None,
                        child_index: None,
                        paragraph_child_index: Some(index),
                    },
                    marks: TextMarks::default(),
                    tracking: None,
                });
                continue;
            }

            // Run-level content elements appearing bare at paragraph level.
            if local_name == "tab" {
                atoms.push(Atom {
                    kind: AtomKind::Tab,
                    utf16_len: 1,
                    source_run_attrs: Vec::new(),
                    origin: AtomOrigin {
                        run_index: None,
                        child_index: None,
                        paragraph_child_index: Some(index),
                    },
                    marks: TextMarks::default(),
                    tracking: None,
                });
                continue;
            }
            if local_name == "noBreakHyphen" {
                atoms.push(Atom {
                    kind: AtomKind::NoBreakHyphen,
                    utf16_len: 1,
                    source_run_attrs: Vec::new(),
                    origin: AtomOrigin {
                        run_index: None,
                        child_index: None,
                        paragraph_child_index: Some(index),
                    },
                    marks: TextMarks::default(),
                    tracking: None,
                });
                continue;
            }
            if local_name == "br" || local_name == "cr" {
                let break_type = if local_name == "cr" {
                    crate::domain::BreakType::TextWrapping
                } else {
                    match attr_get(element, "w:type").map(|s| s.as_str()) {
                        Some("page") => crate::domain::BreakType::Page,
                        Some("column") => crate::domain::BreakType::Column,
                        _ => crate::domain::BreakType::TextWrapping,
                    }
                };
                atoms.push(Atom {
                    kind: AtomKind::Break(break_type),
                    utf16_len: 1,
                    source_run_attrs: Vec::new(),
                    origin: AtomOrigin {
                        run_index: None,
                        child_index: None,
                        paragraph_child_index: Some(index),
                    },
                    marks: TextMarks::default(),
                    tracking: None,
                });
                continue;
            }

            // Run-level widget elements appearing bare at paragraph level
            // (non-conformant but produced by Word). Treat as opaque widgets,
            // same as when they appear inside a run.
            if is_run_widget(&local_name) {
                let raw_xml = serialize_element(element);
                let name = mc_inner_content_name(element)?;
                atoms.push(Atom {
                    kind: AtomKind::Widget { name, raw_xml },
                    utf16_len: 1,
                    source_run_attrs: Vec::new(),
                    origin: AtomOrigin {
                        run_index: None,
                        child_index: None,
                        paragraph_child_index: Some(index),
                    },
                    marks: TextMarks::default(),
                    tracking: None,
                });
                continue;
            }

            // Foreign-namespace element at paragraph level (third-party tool
            // extension not in any in-scope mc:Ignorable, e.g. PowerTools/Templafy
            // <Insert>). Preserve verbatim as a zero-width decoration rather than
            // refuse the document or drop the marker (see
            // `is_foreign_namespace_element`).
            if is_foreign_namespace_element(element) {
                atoms.push(Atom {
                    kind: AtomKind::Decoration {
                        name: element.name.clone(),
                        raw_xml: serialize_element(element),
                    },
                    utf16_len: 0,
                    source_run_attrs: Vec::new(),
                    origin: AtomOrigin {
                        run_index: None,
                        child_index: None,
                        paragraph_child_index: Some(index),
                    },
                    marks: TextMarks::default(),
                    tracking: None,
                });
                continue;
            }

            // Wild-input tolerance (Word-verified, wave campaign): Microsoft
            // Outlook emits a bare `w:rPr` as a direct `w:p` child — CT_P has
            // no such member, yet Word opens the package valid and unrepaired.
            // The stray styles no run and is NOT the paragraph-mark rPr (that
            // lives inside pPr); guessing a meaning would invent semantics and
            // dropping it would be a silent loss. Preserve it verbatim as a
            // zero-width marker, the same treatment as the foreign-namespace
            // arm above.
            if local_element_name(element) == "rPr" {
                tracing::debug!(
                    "preserving schema-invalid bare paragraph-level w:rPr verbatim \
                     (Microsoft Outlook emission)"
                );
                atoms.push(Atom {
                    kind: AtomKind::Decoration {
                        name: element.name.clone(),
                        raw_xml: serialize_element(element),
                    },
                    utf16_len: 0,
                    source_run_attrs: Vec::new(),
                    origin: AtomOrigin {
                        run_index: None,
                        child_index: None,
                        paragraph_child_index: Some(index),
                    },
                    marks: TextMarks::default(),
                    tracking: None,
                });
                continue;
            }

            // Unknown paragraph-level element - fail fast
            return Err(WordIrError::UnknownParagraphElement(element.name.clone()));
        }
        // §17.16.23: an instrText not inside a complex field's field codes is
        // regular text, not a field code. The per-run extraction can't see the
        // surrounding fldChar context, so reclassify orphans in a single
        // ordered pass over the paragraph's atoms now that they are all present.
        reclassify_orphan_instr_text(&mut atoms);

        Ok(ParagraphView {
            atoms,
            num_props,
            style_id,
            alignment,
            indentation,
            spacing,
            borders,
            keep_next,
            keep_lines,
            page_break_before,
            widow_control,
            contextual_spacing,
            paragraph_shading,
            outline_lvl,
            tab_stops,
            section_property_change,
            ppr_change,
            section_properties,
            para_mark_status,
            paragraph_mark_rpr,
            mirror_indents,
            auto_space_de,
            auto_space_dn,
            bidi,
            text_alignment,
            text_direction,
            suppress_auto_hyphens,
            snap_to_grid,
            overflow_punct,
            adjust_right_ind,
            word_wrap,
            frame_pr,
            cnf_style,
            preserved,
        })
    }

    pub fn block_text(&self) -> String {
        let mut out = String::new();
        for atom in &self.atoms {
            match &atom.kind {
                AtomKind::Text(text) => out.push_str(text),
                AtomKind::Tab => out.push('\t'),
                AtomKind::NoBreakHyphen => out.push('\u{2011}'),
                AtomKind::Break(_) => out.push('\n'),
                AtomKind::Widget { .. } => out.push(BARRIER_CHAR),
                AtomKind::Hyperlink(_) => out.push(BARRIER_CHAR),
                AtomKind::Decoration { .. }
                | AtomKind::CommentRangeStart { .. }
                | AtomKind::CommentRangeEnd { .. } => {} // Zero-width, no contribution
                AtomKind::TrackedMoveStart { .. } | AtomKind::TrackedMoveEnd { .. } => {} // Zero-width
                AtomKind::BidiWrapperStart { .. } | AtomKind::BidiWrapperEnd { .. } => {} // Zero-width: bdo/dir are display-only wrappers; inner text reads through its own atoms
                AtomKind::CustomXmlWrapperStart { .. } | AtomKind::CustomXmlWrapperEnd { .. } => {} // Zero-width: customXml/smartTag are transparent wrappers; inner text reads through its own atoms
            }
        }
        out
    }
}

/// Paragraph-level elements that are zero-width decorations.
/// These are stored for roundtripping but don't contribute to text offsets.
fn is_paragraph_decoration(local_name: &str) -> bool {
    matches!(
        local_name,
        "bookmarkStart"
            | "bookmarkEnd"
            | "permStart"
            | "permEnd"
            | "proofErr"
            | "customXmlInsRangeStart"
            | "customXmlInsRangeEnd"
            | "customXmlDelRangeStart"
            | "customXmlDelRangeEnd"
            | "customXmlMoveFromRangeStart"
            | "customXmlMoveFromRangeEnd"
            | "customXmlMoveToRangeStart"
            | "customXmlMoveToRangeEnd"
            | "moveFromRangeStart"
            | "moveFromRangeEnd"
            | "moveToRangeStart"
            | "moveToRangeEnd"
            | "lastRenderedPageBreak"
            | "fldChar"
            | "instrText"
            | "delInstrText"
            | "footnoteReference"
            | "endnoteReference"
    )
}

/// Paragraph-level elements that are widgets (occupy space, block editing).
/// These are complex structures that we can't edit across.
/// Note: "hyperlink" is handled separately with HyperlinkData for serialization.
fn is_paragraph_widget(local_name: &str) -> bool {
    matches!(
        local_name,
        "sdt" | "fldSimple"
            // Office Math paragraph container (m:oMathPara) or inline (m:oMath)
            | "oMathPara"
            | "oMath"
    )
    // NOTE: "customXml" and "smartTag" are NOT widgets — they are TRANSPARENT
    // wrappers handled by `custom_xml_wrapper_atoms` (the inner runs are
    // ordinary document text; §17.5.1.3 / §17.5.1.9).
}

/// Local names of run-level widget elements (members of EG_RunInnerContent that
/// occupy space and block editing). This is the **single source of truth** for
/// "this element is only legal inside `w:r`": both the serializer's re-wrap
/// predicate (`opaque_raw_element_requires_run_wrapper`) and the redline-extract
/// opaque projection (`is_opaque_run_element`) derive from it, so the three
/// whitelists cannot drift apart (the class of bug where a widget like `w:pgNum`
/// gets emitted bare at paragraph level and makes Word refuse the file).
pub(crate) const RUN_WIDGET_NAMES: &[&str] = &[
    "drawing",
    "object",
    "pict",
    "sym",
    "fldChar",
    "instrText",
    "delInstrText",
    // Positioned tab stop (w:ptab) can carry alignment/leader attributes.
    // Preserve as opaque widget to keep roundtrip fidelity.
    "ptab",
    // Current-page-number placeholder (w:pgNum, §17.3.3.22, CT_Empty).
    // A field-like marker rendered as the current page number; preserve
    // as an opaque single-position barrier (like sym/fldChar).
    "pgNum",
    // Footnote/endnote references in main document
    "footnoteReference",
    "endnoteReference",
    // Comment reference marker
    "commentReference",
    // Inline math equation (m:oMath)
    "oMath",
    // East Asian ruby text annotations
    "ruby",
    // Run-level reference to an external content part (e.g. embedded
    // ink / content part) via r:id (§17.3.3.2, MS-OI29500 §2.1.102).
    // No inner text — opaque is honest here. Preserved via raw_xml.
    "contentPart",
];

/// Run-level elements that are widgets (occupy space).
pub(crate) fn is_run_widget(local_name: &str) -> bool {
    RUN_WIDGET_NAMES.contains(&local_name)
}

/// Run-level elements that are decorations (zero-width markers).
fn is_run_decoration(local_name: &str) -> bool {
    matches!(
        local_name,
        "lastRenderedPageBreak"
            // noBreakHyphen is NOT a decoration — it is a visible character
            // (§17.3.3.18); handled as AtomKind::NoBreakHyphen.
            | "softHyphen"
            | "annotationRef"
            | "footnoteRef"
            | "endnoteRef"
            | "separator"
            | "continuationSeparator"
    )
}

fn run_atoms(run: &Element, run_index: usize) -> Result<Vec<Atom>, WordIrError> {
    let mut atoms = Vec::new();
    let source_run_attrs = source_run_attrs(run);
    let marks = extract_text_marks(run);

    // MCE resolution (§9.3) precedes content classification: resolve any
    // `mc:AlternateContent` among the run's children to its selected branch, so
    // an AC wrapping run content is modeled identically to that content held
    // bare — and identically whether or not the run sits inside a tracked-change
    // container. Only clone the child list when an AC is actually present (the
    // overwhelmingly common case is none). rPr is untouched: an AC INSIDE run
    // properties is not run content and is not resolved here.
    let resolved_children;
    let run_children: &[XMLNode] = if run
        .children
        .iter()
        .any(|c| matches!(c, XMLNode::Element(e) if is_mc_alternate_content(e)))
    {
        resolved_children = resolve_run_alternate_content(&run.children)?;
        &resolved_children
    } else {
        &run.children
    };

    for (child_index, child) in run_children.iter().enumerate() {
        let element = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };

        // Skip run properties
        if is_w_tag(element, "rPr") {
            continue;
        }

        let local_name = local_element_name(element);

        // Text content (t = normal text, delText = deleted text in tracked changes)
        if local_name == "t" || local_name == "delText" {
            let text = text_from_element(element);
            if text.is_empty() {
                continue;
            }
            atoms.push(Atom {
                utf16_len: utf16_len(&text),
                kind: AtomKind::Text(text),
                source_run_attrs: source_run_attrs.clone(),
                origin: AtomOrigin {
                    run_index: Some(run_index),
                    child_index: Some(child_index),
                    paragraph_child_index: None,
                },
                marks: marks.clone(),
                tracking: None,
            });
            continue;
        }

        // Tab character
        if local_name == "tab" {
            atoms.push(Atom {
                kind: AtomKind::Tab,
                utf16_len: 1,
                source_run_attrs: source_run_attrs.clone(),
                origin: AtomOrigin {
                    run_index: Some(run_index),
                    child_index: Some(child_index),
                    paragraph_child_index: None,
                },
                marks: marks.clone(),
                tracking: None,
            });
            continue;
        }

        // Non-breaking hyphen — a visible character (§17.3.3.18), like Tab.
        if local_name == "noBreakHyphen" {
            atoms.push(Atom {
                kind: AtomKind::NoBreakHyphen,
                utf16_len: 1,
                source_run_attrs: source_run_attrs.clone(),
                origin: AtomOrigin {
                    run_index: Some(run_index),
                    child_index: Some(child_index),
                    paragraph_child_index: None,
                },
                marks: marks.clone(),
                tracking: None,
            });
            continue;
        }

        // Breaks (line, page, column) per ISO 29500-1 §17.3.3.1
        if local_name == "br" || local_name == "cr" {
            let break_type = if local_name == "cr" {
                crate::domain::BreakType::TextWrapping
            } else {
                match attr_get(element, "w:type").map(|s| s.as_str()) {
                    Some("page") => crate::domain::BreakType::Page,
                    Some("column") => crate::domain::BreakType::Column,
                    _ => crate::domain::BreakType::TextWrapping,
                }
            };
            atoms.push(Atom {
                kind: AtomKind::Break(break_type),
                utf16_len: 1,
                source_run_attrs: source_run_attrs.clone(),
                origin: AtomOrigin {
                    run_index: Some(run_index),
                    child_index: Some(child_index),
                    paragraph_child_index: None,
                },
                marks: marks.clone(),
                tracking: None,
            });
            continue;
        }

        // Widgets (occupy space)
        if is_run_widget(&local_name) {
            atoms.push(Atom {
                kind: AtomKind::Widget {
                    name: element.name.clone(),
                    raw_xml: serialize_element(element),
                },
                utf16_len: 1,
                source_run_attrs: source_run_attrs.clone(),
                origin: AtomOrigin {
                    run_index: Some(run_index),
                    child_index: Some(child_index),
                    paragraph_child_index: None,
                },
                marks: marks.clone(),
                tracking: None,
            });
            continue;
        }

        // Decorations (zero-width markers)
        if is_run_decoration(&local_name) {
            atoms.push(Atom {
                kind: AtomKind::Decoration {
                    name: element.name.clone(),
                    raw_xml: serialize_element(element),
                },
                utf16_len: 0, // Zero-width
                source_run_attrs: source_run_attrs.clone(),
                origin: AtomOrigin {
                    run_index: Some(run_index),
                    child_index: Some(child_index),
                    paragraph_child_index: None,
                },
                marks: marks.clone(),
                tracking: None,
            });
            continue;
        }

        // `mc:AlternateContent` never reaches here: it is resolved to its
        // selected branch's content above (see `resolve_run_alternate_content`),
        // so the loop only ever sees the resolved content, never the wrapper.

        // Wild-input tolerance (Word-verified, wave campaign): LibreOffice
        // 24.2 emits a stray childless COPY of the run's own rPr/rPrChange as
        // a direct run child when round-tripping a formatting revision.
        // Schema-invalid (CT_R has no rPrChange member), but Word opens the
        // package valid and unrepaired and the revision is fully carried by
        // the in-rPr element. Drop ONLY an exact duplicate (same id, author,
        // date as the change already parsed into `marks`) — observable via
        // the debug diagnostic. Anything else keeps the fail-loud arm below:
        // a non-duplicate would carry a revision this run does not otherwise
        // hold, and dropping it would be a silent revision loss.
        if local_name == "rPrChange"
            && let Some(rc) = marks.rpr_change.as_deref()
        {
            let dup_id = attr_value(element, "id")
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(0);
            let dup_author = attr_value(element, "author").cloned().unwrap_or_default();
            let dup_date = attr_value(element, "date").cloned();
            if rc.revision_id == dup_id && rc.author == dup_author && rc.date == dup_date {
                tracing::debug!(
                    "dropping stray duplicate run-level w:rPrChange (LibreOffice emission; \
                     revision carried by the run's rPr)"
                );
                continue;
            }
        }

        // Unknown run-level element - fail fast
        return Err(WordIrError::UnknownRunElement(element.name.clone()));
    }
    // Text and tab children from one source w:r are one formatting/layout
    // carrier. Keep them in one atom when they are contiguous so the canonical
    // model cannot later serialize `<w:tab/><w:t>…</w:t>` as two separate
    // runs. Word's justification can observe that split even though the text
    // and direct rPr are identical. Decorations/widgets/breaks remain explicit
    // barriers and therefore prevent coalescing.
    let mut coalesced: Vec<Atom> = Vec::with_capacity(atoms.len());
    for atom in atoms {
        let current_fragment = match &atom.kind {
            AtomKind::Text(text) => Some(text.as_str()),
            AtomKind::Tab => Some("\t"),
            _ => None,
        };
        if let Some(fragment) = current_fragment
            && let Some(previous) = coalesced.last_mut()
        {
            match &mut previous.kind {
                AtomKind::Text(text) => {
                    text.push_str(fragment);
                    previous.utf16_len += atom.utf16_len;
                    continue;
                }
                AtomKind::Tab => {
                    previous.kind = AtomKind::Text(format!("\t{fragment}"));
                    previous.utf16_len += atom.utf16_len;
                    continue;
                }
                _ => {}
            }
        }
        coalesced.push(atom);
    }
    Ok(coalesced)
}

/// Extract atoms for the STACKED state: a supported `ins`/`del` pair nested
/// one level deep, in either markup order. The runs of the INNER container
/// produce atoms whose tracking context is normalized to insertion-primary
/// (`is_insertion: true`, base fields = the insertion revision,
/// `stacked_deletion` = the deletion revision), so downstream code sees ONE
/// state regardless of the markup order it came from. A further tracked
/// container inside the inner one (depth 2) fails loud — only one nesting
/// level exists in the inline text grammar.
fn stacked_atoms(
    outer: &Element,
    inner: &Element,
    container_index: usize,
) -> Result<Vec<Atom>, WordIrError> {
    fn rev_fields(el: &Element) -> Result<(u32, String, Option<String>), WordIrError> {
        let revision_id: u32 = attr_value(el, "id")
            .ok_or(WordIrError::MissingTrackedChangeAttribute("id"))?
            .parse()
            .map_err(|_| WordIrError::MissingTrackedChangeAttribute("id"))?;
        let author = attr_value(el, "author")
            .ok_or(WordIrError::MissingTrackedChangeAttribute("author"))?
            .to_string();
        let date = attr_value(el, "date").map(|s| s.to_string());
        Ok((revision_id, author, date))
    }
    let (ins_el, del_el) = if is_w_tag(outer, "ins") {
        (outer, inner)
    } else {
        (inner, outer)
    };
    let (ins_id, ins_author, ins_date) = rev_fields(ins_el)?;
    let (del_id, del_author, del_date) = rev_fields(del_el)?;
    let ctx = AtomTrackingContext {
        is_insertion: true,
        revision_id: ins_id,
        author: ins_author,
        date: ins_date,
        stacked_deletion: Some(StackedDeletionLayer {
            revision_id: del_id,
            author: del_author,
            date: del_date,
        }),
    };

    let mut atoms = Vec::new();
    for child in &inner.children {
        let element = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };
        if is_w_tag(element, "r") {
            atoms.extend(run_atoms(element, container_index)?);
            continue;
        }
        if is_w_tag(element, "ins")
            || is_w_tag(element, "del")
            || is_w_tag(element, "moveFrom")
            || is_w_tag(element, "moveTo")
        {
            // Depth 2: no third state exists; never drop, never guess.
            return Err(WordIrError::NestedTrackedChange {
                outer: inner.name.clone(),
                inner: element.name.clone(),
            });
        }
    }
    for atom in &mut atoms {
        atom.tracking = Some(ctx.clone());
    }
    Ok(atoms)
}

/// Resolve every `mc:AlternateContent` directly inside a tracked-change container
/// (or inside one of its direct-child runs) to its selected branch's content,
/// returning a clone of `container` with the AC wrappers replaced in place.
///
/// MCE preprocessing precedes revision semantics (ISO/IEC 29500-3 §9.4 + ECMA-376
/// §17.13.5.18, confirmed against real Word): an AlternateContent inside
/// `w:ins`/`w:del` resolves to the selected branch's runs, which then become
/// ordinary inserted/deleted content. Without this, the AC block has no arm in
/// the tracked-content loop and vanishes from BOTH the accepted and rejected
/// readings — silent content loss. We reuse [`select_mc_branch`] (the shared
/// resolution helper) so the branch choice matches the standalone path exactly.
///
/// Scope: only AC that is a direct child of the container, or a child of a
/// direct-child `w:r`, is resolved here — the shapes Word emits for a tracked
/// shape/extension insertion. Standalone (non-tracked) AC is untouched and stays
/// opaque (preserving the wrapper for rebuild fidelity).
fn resolve_alternate_content_in_tracked(container: &Element) -> Result<Element, WordIrError> {
    /// Selected-branch child elements of an AC, as cloned `XMLNode`s spliced into
    /// the parent in place of the AC wrapper. A non-selected AC (no Choice
    /// satisfiable, no Fallback) contributes nothing (§9.4: removed with its
    /// contents).
    fn branch_nodes(ac: &Element) -> Result<Vec<XMLNode>, WordIrError> {
        match select_mc_branch(ac)? {
            Some(branch) => Ok(branch.children.clone()),
            None => Ok(Vec::new()),
        }
    }

    let mut out = container.clone();
    let mut new_children: Vec<XMLNode> = Vec::with_capacity(out.children.len());
    for child in container.children.iter() {
        match child {
            XMLNode::Element(el) if is_mc_alternate_content(el) => {
                // AC directly inside the tracked container -> splice the selected
                // branch's content (runs, widgets, …) in place.
                new_children.extend(branch_nodes(el)?);
            }
            XMLNode::Element(el) if is_w_tag(el, "r") => {
                // A run that may itself contain an AC (e.g. an inserted shape run).
                // Resolve any AC among its children in place; leave everything
                // else untouched.
                let has_ac = el
                    .children
                    .iter()
                    .any(|c| matches!(c, XMLNode::Element(e) if is_mc_alternate_content(e)));
                if !has_ac {
                    new_children.push(child.clone());
                    continue;
                }
                let mut new_run = el.clone();
                let mut run_children: Vec<XMLNode> = Vec::with_capacity(el.children.len());
                for run_child in el.children.iter() {
                    match run_child {
                        XMLNode::Element(e) if is_mc_alternate_content(e) => {
                            run_children.extend(branch_nodes(e)?);
                        }
                        other => run_children.push(other.clone()),
                    }
                }
                new_run.children = run_children;
                new_children.push(XMLNode::Element(new_run));
            }
            other => new_children.push(other.clone()),
        }
    }
    out.children = new_children;
    Ok(out)
}

/// Resolve every `mc:AlternateContent` among a run's children to its selected
/// branch's content (ISO/IEC 29500-3 §9.3), returning the run's child list with
/// each AC wrapper replaced in place by the branch's children.
///
/// An AC carrying run-level content — a `w:drawing` shape in its `wps` Choice, a
/// plain `w:t`/`w:sym` in a Fallback — thereby models EXACTLY as the same content
/// would if the run held it directly: an AC-wrapped drawing and a bare drawing
/// produce the identical `Drawing` widget, and the SAME AC resolves the SAME way
/// whether or not its run sits inside a tracked-change container. This closes a
/// positional inconsistency: previously an AC inside a `w:ins`/`w:del` run was
/// resolved (via [`resolve_alternate_content_in_tracked`]) while a structurally
/// identical AC in an untracked run was kept verbatim as one opaque widget, so
/// two identical constructs in one document resolved differently.
///
/// Branch selection is the shared [`select_mc_branch`] (Choice whose `Requires`
/// namespaces we understand, else the Fallback), matching the tracked and
/// paragraph-level paths exactly. A non-selected AC (no satisfiable Choice, no
/// Fallback) contributes nothing (§9.4: removed with its contents). Nested ACs
/// are resolved recursively.
fn resolve_run_alternate_content(children: &[XMLNode]) -> Result<Vec<XMLNode>, WordIrError> {
    let mut out = Vec::with_capacity(children.len());
    for child in children {
        match child {
            XMLNode::Element(el) if is_mc_alternate_content(el) => {
                if let Some(branch) = select_mc_branch(el)? {
                    out.extend(resolve_run_alternate_content(&branch.children)?);
                }
            }
            other => out.push(other.clone()),
        }
    }
    Ok(out)
}

/// Process a tracked change container (w:del, w:ins, w:moveFrom, w:moveTo) which contains runs.
/// Extracts revision attributes and tags all produced atoms with tracking context.
/// moveFrom → Deleted, moveTo → Inserted (same semantics as del/ins).
fn tracked_change_atoms(
    container: &Element,
    container_index: usize,
) -> Result<Vec<Atom>, WordIrError> {
    let tracking = if is_w_tag(container, "ins")
        || is_w_tag(container, "del")
        || is_w_tag(container, "moveFrom")
        || is_w_tag(container, "moveTo")
    {
        let is_insertion = is_w_tag(container, "ins") || is_w_tag(container, "moveTo");
        let revision_id: u32 = attr_value(container, "id")
            .ok_or(WordIrError::MissingTrackedChangeAttribute("id"))?
            .parse()
            .map_err(|_| WordIrError::MissingTrackedChangeAttribute("id"))?;
        let author = attr_value(container, "author")
            .ok_or(WordIrError::MissingTrackedChangeAttribute("author"))?
            .to_string();
        let date = attr_value(container, "date").map(|s| s.to_string());
        Some(AtomTrackingContext {
            is_insertion,
            revision_id,
            author,
            date,
            stacked_deletion: None,
        })
    } else {
        None
    };

    // MCE preprocessing precedes revision semantics (§9.4 + §17.13.5.18): resolve
    // any AlternateContent directly inside this container (or inside its
    // direct-child runs) to its selected branch's content BEFORE the loop, so the
    // resolved runs are modeled as ordinary tracked content rather than vanishing.
    // Only clone when an AC is actually present (the common case is none).
    let resolved_owned;
    let container = if container.children.iter().any(|c| match c {
        XMLNode::Element(el) => {
            is_mc_alternate_content(el)
                || (is_w_tag(el, "r")
                    && el
                        .children
                        .iter()
                        .any(|cc| matches!(cc, XMLNode::Element(e) if is_mc_alternate_content(e))))
        }
        _ => false,
    }) {
        resolved_owned = resolve_alternate_content_in_tracked(container)?;
        &resolved_owned
    } else {
        container
    };

    let mut atoms = Vec::new();
    for child in &container.children {
        let element = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };
        // Tracked changes contain runs (w:r)
        if is_w_tag(element, "r") {
            atoms.extend(run_atoms(element, container_index)?);
            continue;
        }
        // A tracked container nested inside this one: parse one of the three
        // modeled one-level states:
        //
        // - ins/del (either wire order): an inserted-then-deleted segment;
        // - moveTo/del: a later deletion in the move destination;
        // - moveFrom/ins: the insertion from which a later move originated.
        //
        // The two move shapes are emitted by Word itself. Their inner carrier
        // remains an ordinary independent status; the enclosing move context
        // is supplied by the linked TrackedBlock move pair. Everything else
        // still fails loud (same-type nesting is invalid OOXML per I-TC-003,
        // and no deeper nesting is modeled).
        if tracking.is_some()
            && (is_w_tag(element, "ins")
                || is_w_tag(element, "del")
                || is_w_tag(element, "moveFrom")
                || is_w_tag(element, "moveTo"))
        {
            let outer_is_ins = is_w_tag(container, "ins");
            let outer_is_del = is_w_tag(container, "del");
            let inner_is_ins = is_w_tag(element, "ins");
            let inner_is_del = is_w_tag(element, "del");
            let stacked_pair = (outer_is_ins && inner_is_del) || (outer_is_del && inner_is_ins);
            let move_destination_deletion = is_w_tag(container, "moveTo") && inner_is_del;
            let inserted_move_source = is_w_tag(container, "moveFrom") && inner_is_ins;
            if !stacked_pair && !move_destination_deletion && !inserted_move_source {
                return Err(WordIrError::NestedTrackedChange {
                    outer: container.name.clone(),
                    inner: element.name.clone(),
                });
            }
            if stacked_pair {
                atoms.extend(stacked_atoms(container, element, container_index)?);
            } else {
                // Parse the inner revision normally. The final tagging loop
                // deliberately does not overwrite its context with the outer
                // move context.
                atoms.extend(tracked_change_atoms(element, container_index)?);
            }
            continue;
        }
        let local_name = local_element_name(element);
        // Transparent wrappers inside a tracked change (CT_RunTrackChange
        // carries EG_ContentRunContent, which includes w:customXml/w:smartTag
        // §17.5.1.3/.9 and w:bdo/w:dir §17.3.2.3/.8). Descend transparently so
        // the inner runs and revisions are modeled; the tagging loop below
        // attaches the container's revision context to the produced atoms.
        // Silently skipping (the pre-customXml-transparent behavior) dropped
        // the wrapped text from the IR entirely.
        if local_name == "customXml" || local_name == "smartTag" {
            atoms.extend(custom_xml_wrapper_atoms(element, container_index)?);
            continue;
        }
        if local_name == "bdo" || local_name == "dir" {
            atoms.extend(bidi_wrapper_atoms(element, container_index)?);
            continue;
        }
        // Paragraph-level widgets are legal tracked content (CT_RunTrackChange
        // carries EG_ContentRunContent + math: an inserted/deleted block
        // equation is `w:ins > m:oMathPara`). Mirror the direct-paragraph-child
        // handling — the tagging loop below attaches the revision context.
        // Silently skipping (the earlier behavior) dropped the math from
        // the IR entirely.
        if is_paragraph_widget(&local_name) {
            atoms.push(Atom {
                kind: AtomKind::Widget {
                    name: element.name.clone(),
                    raw_xml: serialize_element(element),
                },
                utf16_len: 1, // Occupies space as a single barrier
                source_run_attrs: Vec::new(),
                origin: AtomOrigin {
                    run_index: None,
                    child_index: None,
                    paragraph_child_index: Some(container_index),
                },
                marks: TextMarks::default(),
                tracking: None,
            });
            continue;
        }
        // Comment-range markers are legal tracked content too (the same
        // EG_RangeMarkupElements group, §17.13.5.18), but unlike bookmarks and
        // permissions they are TYPED atoms rather than decorations, so they need
        // their own arm — `is_paragraph_decoration`/`is_run_decoration` below do
        // not match them. Without this arm a comment range with one half inside a
        // tracked change was silently dropped here, orphaning the surviving half:
        // the accept/reject collapse never saw the removed half, so it could not
        // re-pair the torn range (the byte path already models and collapses
        // these; this brings the model path to parity). The tagging loop below
        // attaches the container's revision context, so the marker resolves with
        // the surrounding change exactly like the bookmark/permission arm.
        if local_name == "commentRangeStart" || local_name == "commentRangeEnd" {
            let id = attr_get(element, "w:id").cloned().ok_or_else(|| {
                WordIrError::MissingRequiredAttribute {
                    element: local_name.to_string(),
                    attribute: "w:id",
                }
            })?;
            atoms.push(Atom {
                kind: if local_name == "commentRangeStart" {
                    AtomKind::CommentRangeStart { id }
                } else {
                    AtomKind::CommentRangeEnd { id }
                },
                utf16_len: 0,
                source_run_attrs: Vec::new(),
                origin: AtomOrigin {
                    run_index: None,
                    child_index: None,
                    paragraph_child_index: Some(container_index),
                },
                marks: TextMarks::default(),
                tracking: None,
            });
            continue;
        }
        // Zero-width range markers are legal tracked content (CT_RunTrackChange
        // includes EG_RangeMarkupElements: bookmarkStart/bookmarkEnd, move
        // range markers, …; ECMA-376 §17.13.5.18). Preserve them as decoration
        // atoms — silently skipping them (the earlier behavior) tore
        // bookmark pairs whose other half sat outside the container, and the
        // tagging loop below attaches the revision context so accept/reject
        // resolves the marker with the surrounding revision (rejecting an
        // insertion removes a bookmark created inside it, matching Word).
        if is_paragraph_decoration(&local_name) || is_run_decoration(&local_name) {
            atoms.push(Atom {
                kind: AtomKind::Decoration {
                    name: element.name.clone(),
                    raw_xml: serialize_element(element),
                },
                utf16_len: 0, // Zero-width
                source_run_attrs: Vec::new(),
                origin: AtomOrigin {
                    run_index: None,
                    child_index: None,
                    paragraph_child_index: Some(container_index),
                },
                marks: TextMarks::default(),
                tracking: None,
            });
            continue;
        }
        // Anything else is unmodeled content. A tracked container's schema
        // (CT_RunTrackChange = EG_ContentRunContent) admits only the runs,
        // transparent wrappers, widgets, and zero-width range/revision markup the
        // arms above handle — there are NO property children to legitimately skip
        // here (a paragraph/run-mark revision lives inside pPr/rPr, not as a
        // tracked container's child, and is parsed elsewhere). A stray element is
        // therefore content we would otherwise silently drop, losing its text or
        // anchoring, so refuse — mirroring the nested-revision and wrapper-dispatch
        // arms that already fail loud (CLAUDE.md: no silent fallbacks). Verified
        // against the wild corpus: every direct child of a tracked container is a
        // handled content kind (zero reach this point), so this never spuriously
        // refuses a real document.
        return Err(WordIrError::UnexpectedTrackedChangeChild {
            container: local_element_name(container).to_string(),
            element: element.name.clone(),
        });
    }

    // Tag all atoms with tracking context. Atoms that already carry one came
    // from a nested stacked pair (`stacked_atoms`) — overwriting it with the
    // outer container's plain context would silently degrade the stacked
    // state back to a plain insertion (losing the inner deletion).
    if let Some(ctx) = tracking {
        for atom in &mut atoms {
            if atom.tracking.is_none() {
                atom.tracking = Some(ctx.clone());
            }
        }
    }

    Ok(atoms)
}

/// Classify ONE content child of a transparent inline wrapper — `w:bdo`/`w:dir`
/// (§17.3.2.3/.8) or `w:customXml`/`w:smartTag` (§17.5.1.3/.9) — and produce its
/// atoms, mirroring exactly how the paragraph-level dispatcher classifies the
/// SAME element.
///
/// There is one content model behind all four wrappers: `CT_BdoContentRun` /
/// `CT_DirContentRun` / `CT_CustomXmlRun` / `CT_SmartTagRun` each contain
/// `EG_PContent`, so a single shared dispatcher is correct — the alternative
/// (a per-wrapper child whitelist) drifts, which is exactly how a legal
/// `proofErr` between runs inside a `w:bdo` came to fail import.
///
/// `EG_PContent` = `EG_ContentRunContent` (`customXml` | `smartTag` | `sdt` |
/// `dir` | `bdo` | `r` | `EG_RunLevelElts`) + `fldSimple` | `hyperlink` |
/// `subDoc`. `EG_RunLevelElts` is the tracked-change envelopes
/// (`ins`/`del`/`moveFrom`/`moveTo`) plus the zero-width range/proofing markup
/// (`proofErr`, bookmarks, `permStart`/`permEnd`, comment ranges, move ranges,
/// customXml ranges).
///
/// Arms resolved here (same atoms the paragraph path produces):
///  - `customXmlPr`/`smartTagPr` — the wrapper's property child; already carried
///    in the marker bytes, so skip it (never occurs inside `bdo`/`dir`).
///  - `r` — ordinary run text.
///  - `ins`/`del`/`moveFrom`/`moveTo` — tracked revision, descended transparently.
///  - `customXml`/`smartTag`, `bdo`/`dir` — nested transparent wrapper.
///  - `commentRangeStart`/`commentRangeEnd` — typed comment-range atoms.
///  - any `is_paragraph_decoration`/`is_run_decoration` element — a zero-width
///    range/proofing marker (`proofErr`, bookmarks, perms, move ranges, …),
///    preserved as a `Decoration` atom so the serializer's `renest_inline_*`
///    pass folds it back inside the wrapper in place.
///
/// Arms deliberately kept FAIL-LOUD (`UnknownRunElement`): `hyperlink`,
/// `fldSimple`, `sdt`, `subDoc`, and the paragraph-level math widgets. The
/// paragraph path models these as *space-occupying* barrier/widget atoms, but
/// the wrapper round-trip reconstructs the wrapper by folding the intervening
/// zero-width-or-text siblings back inside it (`renest_inline_bidi_wrappers` /
/// `renest_inline_custom_xml_wrappers`); a barrier/widget atom between the
/// wrapper markers is not something that fold is built to re-nest. Descending
/// would require restructuring that pass, so we refuse loudly rather than
/// silently drop the content (CLAUDE.md: no silent fallbacks).
fn wrapper_content_child_atoms(
    element: &Element,
    container_index: usize,
) -> Result<Vec<Atom>, WordIrError> {
    // Property child of a customXml/smartTag wrapper — carried in the marker
    // bytes as metadata, not document content. (Never appears inside bdo/dir.)
    if is_w_tag(element, "customXmlPr") || is_w_tag(element, "smartTagPr") {
        return Ok(Vec::new());
    }
    if is_w_tag(element, "r") {
        return run_atoms(element, container_index);
    }
    // Tracked-change envelopes (EG_RunLevelElts). `tracked_change_atoms` reads
    // the revision context off the container and handles all four verbs.
    if is_w_tag(element, "ins")
        || is_w_tag(element, "del")
        || is_w_tag(element, "moveFrom")
        || is_w_tag(element, "moveTo")
    {
        return tracked_change_atoms(element, container_index);
    }
    if is_w_tag(element, "customXml") || is_w_tag(element, "smartTag") {
        return custom_xml_wrapper_atoms(element, container_index);
    }
    if is_w_tag(element, "bdo") || is_w_tag(element, "dir") {
        return bidi_wrapper_atoms(element, container_index);
    }
    let local_name = local_element_name(element);
    // Comment range markers — extract w:id for typed round-tripping, exactly as
    // the paragraph path does (they are not in the decoration set below).
    if local_name == "commentRangeStart" || local_name == "commentRangeEnd" {
        let id = attr_get(element, "w:id").cloned().ok_or_else(|| {
            WordIrError::MissingRequiredAttribute {
                element: local_name.to_string(),
                attribute: "w:id",
            }
        })?;
        return Ok(vec![Atom {
            kind: if local_name == "commentRangeStart" {
                AtomKind::CommentRangeStart { id }
            } else {
                AtomKind::CommentRangeEnd { id }
            },
            utf16_len: 0,
            source_run_attrs: Vec::new(),
            origin: AtomOrigin {
                run_index: None,
                child_index: None,
                paragraph_child_index: Some(container_index),
            },
            marks: TextMarks::default(),
            tracking: None,
        }]);
    }
    // Zero-width range/proofing markup (EG_RunLevelElts): proofErr, bookmarks,
    // perms, move ranges, customXml ranges, … Preserve as a Decoration atom so
    // the pair survives round-trip in place. THIS is the arm the buggy
    // per-wrapper whitelist lacked, which made `proofErr` inside `w:bdo` fail.
    if is_paragraph_decoration(&local_name) || is_run_decoration(&local_name) {
        return Ok(vec![Atom {
            kind: AtomKind::Decoration {
                name: element.name.clone(),
                raw_xml: serialize_element(element),
            },
            utf16_len: 0,
            source_run_attrs: Vec::new(),
            origin: AtomOrigin {
                run_index: None,
                child_index: None,
                paragraph_child_index: Some(container_index),
            },
            marks: TextMarks::default(),
            tracking: None,
        }]);
    }
    // Space-occupying content (hyperlink/fldSimple/sdt/subDoc/math) and any
    // genuinely unknown element: fail loud rather than silently drop. See the
    // fn-doc for why descending is not attempted inside a wrapper.
    Err(WordIrError::UnknownRunElement(element.name.clone()))
}

/// Parse a bidirectional display-only wrapper (`w:bdo` §17.3.2.3 / `w:dir`
/// §17.3.2.8) as a TRANSPARENT container, mirroring how `tracked_change_atoms`
/// descends into `w:ins`/`w:del` but WITHOUT any revision semantics.
///
/// bdo/dir affect visual direction only; the runs inside them hold ordinary
/// logical-order document text. So we:
///  - emit a zero-width `BidiWrapperStart` marker carrying the CHILDLESS wrapper
///    bytes (`<w:bdo w:val="…"/>`), so the serializer can re-wrap on round-trip;
///  - descend into each content child via the shared `wrapper_content_child_atoms`
///    dispatcher (runs → text, revisions → tracked, zero-width proofing/range
///    markup like `proofErr` → decoration atoms, nested wrappers transparently);
///  - emit a matching `BidiWrapperEnd` marker carrying the same wrapper bytes.
///
/// The serializer's `renest_inline_bidi_wrappers` pass folds the intervening
/// atoms back into the wrapper, reconstructing `<w:bdo>…children…</w:bdo>`
/// verbatim, so a decoration between two runs keeps its position.
fn bidi_wrapper_atoms(wrapper: &Element, container_index: usize) -> Result<Vec<Atom>, WordIrError> {
    let mut template = wrapper.clone();
    template.children.clear();
    let marker_bytes = serialize_element(&template);

    let mut atoms = Vec::new();
    atoms.push(Atom {
        kind: AtomKind::BidiWrapperStart {
            raw_xml: marker_bytes.clone(),
        },
        utf16_len: 0,
        source_run_attrs: Vec::new(),
        origin: AtomOrigin {
            run_index: None,
            child_index: None,
            paragraph_child_index: Some(container_index),
        },
        marks: TextMarks::default(),
        tracking: None,
    });

    for child in &wrapper.children {
        let element = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };
        atoms.extend(wrapper_content_child_atoms(element, container_index)?);
    }

    atoms.push(Atom {
        kind: AtomKind::BidiWrapperEnd {
            raw_xml: marker_bytes,
        },
        utf16_len: 0,
        source_run_attrs: Vec::new(),
        origin: AtomOrigin {
            run_index: None,
            child_index: None,
            paragraph_child_index: Some(container_index),
        },
        marks: TextMarks::default(),
        tracking: None,
    });

    Ok(atoms)
}

/// Parse an inline custom-XML (`w:customXml` §17.5.1.3) or smart-tag
/// (`w:smartTag` §17.5.1.9) wrapper as a TRANSPARENT container, mirroring
/// `bidi_wrapper_atoms` (and the moveFrom/moveTo start/end-marker shape).
///
/// These wrappers carry semantic identity (uri/element + a `customXmlPr` /
/// `smartTagPr` property child) but their content runs are ORDINARY document
/// text and any inner revisions are ORDINARY revisions. So we:
///  - emit a zero-width `CustomXmlWrapperStart` marker carrying the wrapper
///    bytes with its CONTENT children cleared but its property child
///    (`customXmlPr`/`smartTagPr`) PRESERVED, so the serializer can re-wrap on
///    round-trip without losing identity;
///  - descend into the content children: runs as ordinary text atoms,
///    `w:ins`/`w:del`/`w:moveFrom`/`w:moveTo` via `tracked_change_atoms` (so the
///    revision resolves on accept/reject), nested customXml/smartTag/bdo/dir
///    transparently;
///  - emit a matching `CustomXmlWrapperEnd` marker carrying the same bytes.
///
/// `renest_inline_custom_xml_wrappers` (serializer) folds the intervening
/// atoms back into the wrapper, reconstructing
/// `<w:customXml><w:customXmlPr/>…runs…</w:customXml>` verbatim.
fn custom_xml_wrapper_atoms(
    wrapper: &Element,
    container_index: usize,
) -> Result<Vec<Atom>, WordIrError> {
    // Template = the wrapper with its CONTENT children removed but its property
    // child (customXmlPr/smartTagPr) kept. The Pr child is metadata, not
    // document content, so it travels with the marker bytes.
    let mut template = wrapper.clone();
    template.children.retain(|c| {
        matches!(
            c,
            XMLNode::Element(e)
                if is_w_tag(e, "customXmlPr") || is_w_tag(e, "smartTagPr")
        )
    });
    let marker_bytes = serialize_element(&template);

    let mut atoms = Vec::new();
    atoms.push(Atom {
        kind: AtomKind::CustomXmlWrapperStart {
            raw_xml: marker_bytes.clone(),
        },
        utf16_len: 0,
        source_run_attrs: Vec::new(),
        origin: AtomOrigin {
            run_index: None,
            child_index: None,
            paragraph_child_index: Some(container_index),
        },
        marks: TextMarks::default(),
        tracking: None,
    });

    for child in &wrapper.children {
        let element = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };
        // Same shared dispatcher as `bidi_wrapper_atoms` — one content model
        // (EG_PContent), one classification. The property child
        // (customXmlPr/smartTagPr) is skipped there (already in the template).
        atoms.extend(wrapper_content_child_atoms(element, container_index)?);
    }

    atoms.push(Atom {
        kind: AtomKind::CustomXmlWrapperEnd {
            raw_xml: marker_bytes,
        },
        utf16_len: 0,
        source_run_attrs: Vec::new(),
        origin: AtomOrigin {
            run_index: None,
            child_index: None,
            paragraph_child_index: Some(container_index),
        },
        marks: TextMarks::default(),
        tracking: None,
    });

    Ok(atoms)
}

/// Read the `w:fldCharType` of a `fldChar` widget atom from its raw bytes.
/// Returns the type string ("begin" / "separate" / "end") or `None` if the
/// atom is not a fldChar or the attribute is absent/unparseable.
fn fld_char_type(atom: &Atom) -> Option<String> {
    let AtomKind::Widget { name, raw_xml } = &atom.kind else {
        return None;
    };
    if local_element_name_str(name) != "fldChar" {
        return None;
    }
    let el = crate::word_xml::parse_raw_fragment(raw_xml).ok()?;
    attr_value(&el, "fldCharType").cloned()
}

/// Local name of a possibly-prefixed element name string ("w:fldChar" -> "fldChar").
fn local_element_name_str(name: &str) -> &str {
    name.rsplit_once(':').map(|(_, l)| l).unwrap_or(name)
}

/// §17.16.23: reclassify each `instrText`/`delInstrText` atom that is NOT inside
/// a complex field's field codes into a plain `Text` atom.
///
/// `instrText` is field-code text only when it sits inside a complex field —
/// between a `begin` fldChar and its matching `end`. An *orphan* instrText (no
/// enclosing complex field) "should be treated as regular text" per the spec, so
/// importing it as an opaque field marker drops its visible content.
///
/// We track complex-field nesting depth across the ordered atom stream: each
/// `begin` fldChar opens a field, each `end` closes one. An instrText seen at
/// depth 0 is an orphan and becomes a `Text` atom carrying its inner text.
/// An unknown/absent fldCharType does not change the depth (it parses per the
/// unknown-fldCharType leniency and stays an opaque widget).
///
/// Scope note: a complex field that is split across paragraph boundaries is not
/// tracked here (depth resets per paragraph) — Word keeps a field's codes within
/// one paragraph in practice, and the conformant shapes this guards against are
/// single-paragraph.
fn reclassify_orphan_instr_text(atoms: &mut [Atom]) {
    // Conservative guard: a fldChar whose fldCharType is missing/unreadable
    // makes the paragraph's field structure ambiguous (CT_FldChar requires the
    // attribute). Refuse to guess: leave every instrText opaque rather than
    // risk surfacing genuine field code as visible text.
    let ambiguous_field_structure = atoms.iter().any(|a| {
        matches!(&a.kind, AtomKind::Widget { name, .. }
            if local_element_name_str(name) == "fldChar")
            && fld_char_type(a).is_none()
    });
    if ambiguous_field_structure {
        return;
    }

    let mut field_depth: u32 = 0;
    for atom in atoms.iter_mut() {
        // Update field nesting depth from fldChar markers BEFORE deciding about
        // a co-located instrText (they are distinct atoms, so order is by their
        // position in the stream).
        if let Some(fld_type) = fld_char_type(atom) {
            match fld_type.as_str() {
                "begin" => field_depth += 1,
                "end" => field_depth = field_depth.saturating_sub(1),
                _ => {} // "separate" (and unknown types) do not change nesting depth
            }
            continue;
        }

        let AtomKind::Widget { name, raw_xml } = &atom.kind else {
            continue;
        };
        let local = local_element_name_str(name);
        if local != "instrText" && local != "delInstrText" {
            continue;
        }
        if field_depth > 0 {
            // Genuine field code inside a complex field — leave opaque.
            continue;
        }
        // Orphan: re-read the inner text and demote to a plain Text atom.
        // An EMPTY orphan (or one whose bytes do not re-parse) is left as an
        // opaque widget: "regular text that is empty" contributes nothing, and
        // an empty Text atom would emit no inline, breaking the atoms↔inlines
        // 1:1 import invariant (segments_from_tracked_atoms).
        let Ok(el) = crate::word_xml::parse_raw_fragment(raw_xml) else {
            continue;
        };
        let text = text_from_element(&el);
        if text.is_empty() {
            continue;
        }
        atom.kind = AtomKind::Text(text.clone());
        atom.utf16_len = utf16_len(&text);
    }
}

fn text_from_element(element: &Element) -> String {
    let mut out = String::new();
    for node in &element.children {
        if let XMLNode::Text(text) = node {
            out.push_str(text);
        }
    }
    out
}

fn source_run_attrs(run: &Element) -> Vec<(String, String)> {
    let mut attrs: Vec<_> = run
        .attributes
        .iter()
        .filter(|(name, _)| name.local_name.starts_with("rsid"))
        .map(|(name, value)| {
            let qualified = name.prefix.as_deref().map_or_else(
                || name.local_name.clone(),
                |prefix| format!("{prefix}:{}", name.local_name),
            );
            (qualified, value.clone())
        })
        .collect();
    attrs.sort();
    attrs
}

/// Extract formatting marks from w:rPr child of a w:r element.
/// Delegates to `parse_rpr_element` — the single canonical rPr parser.
fn extract_text_marks(run: &Element) -> TextMarks {
    let Some(rpr) = find_w_child(run, "rPr") else {
        return TextMarks::default();
    };
    parse_rpr_element(rpr)
}

/// Canonical parser for a w:rPr element. All rPr parsing goes through here.
/// Handles every CT_RPr / EG_RPrBase child element per ECMA-376 §17.3.2.
pub(crate) fn parse_rpr_element(rpr: &Element) -> TextMarks {
    let mut marks = TextMarks::default();

    for child in &rpr.children {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };

        let local_name = local_element_name(el);

        match local_name.as_str() {
            // --- Toggle properties ---
            "b" => marks.bold = parse_toggle_value(el),
            "bCs" => marks.bold_cs = parse_toggle_value(el),
            "i" => marks.italic = parse_toggle_value(el),
            "iCs" => marks.italic_cs = parse_toggle_value(el),
            "caps" => marks.caps = parse_toggle_value(el),
            "smallCaps" => marks.small_caps = parse_toggle_value(el),
            "strike" => marks.strike = parse_toggle_value(el),
            "dstrike" => marks.double_strike = parse_toggle_value(el),
            "outline" => marks.outline = parse_toggle_value(el),
            "shadow" => marks.shadow = parse_toggle_value(el),
            "emboss" => marks.emboss = parse_toggle_value(el),
            "imprint" => marks.imprint = parse_toggle_value(el),
            "vanish" => marks.vanish = parse_toggle_value(el),
            "webHidden" => marks.web_hidden = parse_toggle_value(el),
            "cs" => marks.cs = parse_toggle_value(el),
            "rtl" => marks.rtl = parse_toggle_value(el),

            // --- Underline ---
            "u" => {
                marks.underline = parse_underline_value(el);
                marks.underline_style = attr_value(el, "val").cloned();
            }

            // --- Vertical alignment (superscript/subscript) ---
            "vertAlign" => {
                // w:vertAlign w:val="subscript" | "superscript"
                if let Some(val) = attr_value(el, "val") {
                    match val.as_str() {
                        "subscript" => marks.subscript = MarkValue::On,
                        "superscript" => marks.superscript = MarkValue::On,
                        _ => {}
                    }
                }
            }

            // --- Fonts ---
            "rFonts" => {
                // w:rFonts — prefer w:ascii, fall back to w:hAnsi
                marks.font_family = attr_value(el, "ascii")
                    .or_else(|| attr_value(el, "hAnsi"))
                    .map(|s| IStr::from(s.as_str()));
                marks.font_family_theme = attr_value(el, "asciiTheme")
                    .or_else(|| attr_value(el, "hAnsiTheme"))
                    .map(|s| IStr::from(s.as_str()));
                marks.font_east_asia = attr_value(el, "eastAsia").map(|s| IStr::from(s.as_str()));
                marks.font_east_asia_theme =
                    attr_value(el, "eastAsiaTheme").map(|s| IStr::from(s.as_str()));
                marks.font_cs = attr_value(el, "cs").map(|s| IStr::from(s.as_str()));
                // The normative attribute is lowercase-t "cstheme" (the lone
                // lowercase theme attr in CT_Fonts; ascii/hAnsi/eastAsiaTheme are
                // camelCase). Reading "csTheme" silently dropped it. (§17.3.2.26)
                marks.font_cs_theme = attr_value(el, "cstheme").map(|s| IStr::from(s.as_str()));
                // w:hint attribute for per-character font selection.
                marks.font_hint = attr_value(el, "hint").map(|s| IStr::from(s.as_str()));
            }

            // --- Font sizes ---
            "sz" => {
                // w:sz w:val="24" — half-points (24 = 12pt)
                marks.font_size = attr_value(el, "val").and_then(|v| v.parse().ok());
            }
            "szCs" => {
                marks.font_size_cs = attr_value(el, "val").and_then(|v| v.parse::<u32>().ok());
            }

            // --- Color & highlight ---
            "color" => {
                // w:color w:val="FF0000" or "auto"
                if let Some(val) = attr_value(el, "val") {
                    marks.color = Some(IStr::from(val.as_str()));
                }
                // w:color w:themeColor="accent1" w:themeShade="BF" w:themeTint="99"
                if let Some(tc) = attr_value(el, "themeColor") {
                    marks.color_theme = Some(crate::domain::ThemeColorRef {
                        theme_color: IStr::from(tc.as_str()),
                        theme_shade: attr_value(el, "themeShade").map(|s| IStr::from(s.as_str())),
                        theme_tint: attr_value(el, "themeTint").map(|s| IStr::from(s.as_str())),
                    });
                }
            }
            "highlight" => {
                // w:highlight w:val="yellow" — named color
                marks.highlight = attr_value(el, "val").cloned();
            }

            // --- Language ---
            "lang" => {
                marks.lang = attr_value(el, "val").map(|s| IStr::from(s.as_str()));
                marks.lang_east_asia = attr_value(el, "eastAsia").map(|s| IStr::from(s.as_str()));
            }

            // --- Spacing / positioning / scaling ---
            "spacing" => {
                marks.char_spacing = attr_value(el, "val").and_then(|v| v.parse::<i32>().ok());
            }
            "position" => {
                // w:position w:val="6" — vertical displacement in half-points (ISO 29500-1 §17.3.2.19)
                marks.position = attr_value(el, "val").and_then(|v| v.parse::<i64>().ok());
            }
            "kern" => {
                // w:kern w:val="28" — kerning threshold in half-points (ISO 29500-1 §17.3.2.19a)
                marks.kern = attr_value(el, "val").and_then(|v| v.parse::<i64>().ok());
            }
            "w" => {
                // w:w w:val="150" — character width scaling percentage (ISO 29500-1 §17.3.2.43)
                marks.char_width_scaling =
                    attr_value(el, "val").and_then(|v| v.parse::<i64>().ok());
            }

            // --- Character style reference ---
            "rStyle" => {
                // w:rStyle w:val="Emphasis" — character style reference
                marks.char_style_id = attr_value(el, "val").map(|s| IStr::from(s.as_str()));
            }

            // --- Run border ---
            "bdr" => {
                // w:bdr — run border (ISO 29500-1 §17.3.2.4)
                marks.run_border_style = attr_value(el, "val").map(|s| IStr::from(s.as_str()));
                marks.run_border_size = attr_value(el, "sz").and_then(|v| v.parse::<u32>().ok());
                marks.run_border_space =
                    attr_value(el, "space").and_then(|v| v.parse::<u32>().ok());
                marks.run_border_color = attr_value(el, "color").map(|s| IStr::from(s.as_str()));
            }

            // --- Tracked formatting change ---
            "rPrChange" => {
                // w:rPrChange — tracked formatting change.
                // Contains an inner w:rPr with the "before" formatting snapshot.
                let author = attr_value(el, "author").cloned().unwrap_or_default();
                let date = attr_value(el, "date").cloned();
                let revision_id = attr_value(el, "id")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                // Word always writes the previous-state child, even when empty
                // (`<w:rPr/>`); LibreOffice omits it entirely when the prior
                // run had no direct formatting. Same meaning — an absent child
                // is an EMPTY previous state, never a reason to drop the
                // tracked change (mirrors parse_tbl_pr_change's missing-inner
                // handling).
                let previous = match find_w_child(el, "rPr") {
                    Some(inner_rpr) => parse_rpr_element(inner_rpr),
                    None => TextMarks::default(),
                };
                marks.rpr_change = Some(Box::new(RprChange {
                    previous_marks: previous,
                    revision_id,
                    author,
                    date,
                }));
            }

            // --- Toggle rPr children (CT_OnOff) ---
            "noProof" => marks.no_proof = parse_toggle_value(el),
            "specVanish" => marks.spec_vanish = parse_toggle_value(el),
            "oMath" => marks.o_math = parse_toggle_value(el),
            "snapToGrid" => marks.snap_to_grid = parse_toggle_value(el),

            // --- Value-carrying rPr children ---
            "shd" => {
                // §17.3.2.32 — run shading (same CT_Shd as paragraph/cell shading).
                let fill = attr_value(el, "fill").cloned();
                let val = attr_value(el, "val").cloned();
                let color = attr_value(el, "color").cloned();
                if fill.is_some() || val.is_some() || color.is_some() {
                    marks.run_shading = Some((fill, val, color));
                }
            }
            "em" => {
                // §17.3.2.11 — east asian emphasis mark.
                marks.emphasis_mark = attr_value(el, "val").cloned();
            }
            "effect" => {
                // §17.3.2.12 — animated text effect.
                marks.text_effect = attr_value(el, "val").cloned();
            }
            "fitText" => {
                // §17.3.2.14 — fit text within specified width.
                marks.fit_text_width = attr_value(el, "val").and_then(|v| v.parse::<u32>().ok());
                marks.fit_text_id = attr_value(el, "id").and_then(|v| v.parse::<u32>().ok());
            }

            // Paragraph-mark revision markers (§17.13.5.19/.28) — parsed by
            // extract_para_mark_status, not text marks. No-op here.
            "ins" | "del" => {}

            // --- Preserved remainder: unmodeled rPr child ---
            //
            // An rPr child element this parser doesn't model (e.g.
            // w:eastAsianLayout, or a foreign-namespace extension like
            // w14:glow) is captured verbatim here rather than dropped —
            // disciplined preservation, the same guarantee structural content
            // already has via `AtomKind::Widget { raw_xml }`. `build_rpr`
            // re-emits it at its Annex-A position (or at the end of rPr for
            // names outside the ordering table) on re-serialization, so an
            // untouched run's unmodeled formatting survives.
            unknown => {
                tracing::debug!(
                    element = %unknown,
                    "parse_rpr_element: unmodeled rPr child element captured verbatim as a preserved remainder"
                );
                marks.preserved.push(crate::domain::PreservedProp {
                    name: qualified_element_name(el),
                    raw_xml: String::from_utf8(serialize_element(el))
                        .expect("serialize_element always emits valid UTF-8 XML"),
                });
            }
        }
    }

    marks
}

/// Parse a toggle property value (CT_OnOff, like w:b, w:i).
/// - Absent: On (element presence implies true for a toggle property).
/// - Present with no val, val="1", or val="true": On.
/// - val="0" or val="false": Off.
/// - Anything else (schema-invalid — ST_OnOff only permits the above):
///   PRODUCT-APPROVED DEFAULT, On. Matches Word's own tolerant boolean
///   parsing (same decision as the sibling in styles.rs's
///   `parse_toggle_value`, kept identical so styles.xml and rPr toggle
///   properties degrade the same way). Logged since the value is
///   out-of-spec even though the fallback is intentional.
fn parse_toggle_value(el: &Element) -> MarkValue {
    match attr_value(el, "val") {
        None => MarkValue::On, // <w:b/> without val means on
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
/// - Absent: Inherit (from outer style)
/// - w:u w:val="none": Off
/// - w:u w:val="single" | "double" | etc.: On
fn parse_underline_value(el: &Element) -> MarkValue {
    match attr_value(el, "val") {
        None => MarkValue::On, // <w:u/> without val defaults to single (on)
        Some(val) => match val.as_str() {
            "none" => MarkValue::Off,
            _ => MarkValue::On,
        },
    }
}

fn utf16_len(text: &str) -> u32 {
    text.encode_utf16().count() as u32
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

/// True iff `element` lives in a FOREIGN namespace: it has an explicit
/// namespace URI that is neither the MCE namespace nor one this consumer
/// understands (WML core + every known OOXML/Microsoft extension —
/// [`mce_namespace_understood`]).
///
/// Third-party tools inject such elements directly into the WML stream outside
/// the schema — e.g. OpenXML PowerTools / Templafy DocumentBuilder emits
/// `<Insert Id="..." xmlns="http://powertools.codeplex.com/documentbuilder/2011/insert"/>`
/// as a direct child of `w:p` in a header story. The element is NOT declared in
/// any in-scope `mc:Ignorable` (so the MCE Step-1 transform leaves it in place),
/// yet it is not WML markup we can model. The honest move is to preserve it
/// verbatim as a zero-width decoration (raw_xml round-trips byte-for-byte) rather
/// than refuse the whole document or silently drop the marker.
///
/// An element in the WML/OOXML namespaces is NEVER foreign: an unknown element
/// there is a genuine spec gap that must still fail loud (e.g. `<w:bogusElement/>`).
fn is_foreign_namespace_element(element: &Element) -> bool {
    match element.namespace.as_deref() {
        Some(uri) => uri != MC_NS && !mce_namespace_understood(uri),
        // No namespace URI at all (bare/un-prefixed in the default WML scope):
        // treat as WML-local and let the unknown-element refusal handle it.
        None => false,
    }
}

/// Extract the local name from an element, stripping any namespace prefix.
/// e.g., "w:bookmarkStart" -> "bookmarkStart", "bookmarkStart" -> "bookmarkStart"
fn local_element_name(element: &Element) -> String {
    if let Some(pos) = element.name.find(':') {
        element.name[pos + 1..].to_string()
    } else {
        element.name.clone()
    }
}

/// Reconstruct the qualified element name (`prefix:local`) from a parsed
/// element's separate `prefix` / `name` fields — the production document
/// parser (`parse_document_xml_quick`) splits the qname on read, so `.name`
/// alone is local-only for a prefixed element (e.g. "glow", not "w14:glow").
/// An element bound via a default `xmlns=` declaration (no prefix) keeps its
/// bare name.
fn qualified_element_name(element: &Element) -> String {
    match &element.prefix {
        Some(prefix) => format!("{prefix}:{}", element.name),
        None => element.name.clone(),
    }
}

/// Check if an element is mc:AlternateContent.
pub fn is_mc_alternate_content(element: &Element) -> bool {
    let local = local_element_name(element);
    if local != "AlternateContent" {
        return false;
    }
    element.prefix.as_deref() == Some("mc")
        || element.namespace.as_deref() == Some(MC_NS)
        || element.name == "mc:AlternateContent"
}

/// The namespace URIs in our MCE "application configuration" (ISO/IEC 29500-3
/// §9.3): an mc:Choice is selected when every namespace its `Requires` prefixes
/// resolve to is in this set. Real Word confirms selection is
/// by resolved namespace NAME, not by the literal prefix token.
///
/// Members:
/// - The WordprocessingML main namespace (ISO/IEC 29500-1 §10: all MCE features
///   are available to WML, and WML markup is what we fully parse) — confirmed
///   against real Word (`Requires="w"`/`"word"` Choices are selected).
/// - The Word 2010 wordml namespace `w14` (MS-DOCX §2.6) — confirmed against real Word
///   (`Requires="w14"` Choices are selected); its rPr/run extensions are either
///   baseline WML we parse or opaque markup we preserve.
/// - The drawing namespaces wps/wpg/wpc/wpi (MS-DOCX §2.2 extension mechanism):
///   their content lives inside w:drawing/w:pict, which we preserve as opaque
///   widgets.
///
/// A `Requires` token whose namespace is NOT in this set means the Choice is not
/// selectable (skip it, try the next Choice or the Fallback). A token with NO
/// in-scope binding is non-conformant per §7.6 and is a hard error
/// ([`WordIrError::UnresolvableMcRequiresPrefix`]), never a silent skip.
const UNDERSTOOD_MC_NAMESPACES: &[&str] = &[
    "http://schemas.openxmlformats.org/wordprocessingml/2006/main", // WML main
    "http://schemas.microsoft.com/office/word/2010/wordml",         // w14 (MS-DOCX §2.6)
    "http://schemas.microsoft.com/office/word/2010/wordprocessingShape", // wps
    "http://schemas.microsoft.com/office/word/2010/wordprocessingGroup", // wpg
    "http://schemas.microsoft.com/office/word/2010/wordprocessingCanvas", // wpc
    "http://schemas.microsoft.com/office/word/2010/wordprocessingInk", // wpi
];

/// Select the preferred MC branch per ISO/IEC 29500-3 §9.3: the first Choice
/// whose `Requires` namespaces are all in our application configuration, else
/// the (single) Fallback, else None.
///
/// Returns `Err` only when a Choice's `Requires` token cannot be resolved to a
/// namespace through the element's in-scope xmlns bindings (non-conformant per
/// §7.6) — see [`WordIrError::UnresolvableMcRequiresPrefix`].
pub fn select_mc_branch(mc_element: &Element) -> Result<Option<&Element>, WordIrError> {
    let mut fallback: Option<&Element> = None;
    for child in &mc_element.children {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };
        let local = local_element_name(el);
        if local == "Choice" && mc_choice_is_selectable(el)? {
            return Ok(Some(el));
        }
        if local == "Fallback" && fallback.is_none() {
            fallback = Some(el);
        }
    }
    Ok(fallback)
}

/// Check if an mc:Choice element's `Requires` attribute is satisfied.
///
/// ISO/IEC 29500-3 §7.6: `Requires` is a whitespace-delimited list of namespace
/// PREFIXES. §9.3: the Choice is selected when each namespace those prefixes
/// resolve to is in the application configuration. So we resolve every prefix
/// through the element's in-scope xmlns bindings (xmltree-ns populates
/// `Element::namespaces` with the FULL in-scope map, ancestors included) and
/// compare the resulting URIs against [`UNDERSTOOD_MC_NAMESPACES`].
///
/// An absent `Requires` makes the Choice always selectable (§7.6 requires it,
/// but we tolerate its absence as "no requirement"). An unbound prefix is
/// non-conformant (§7.6) and returns an error — not a silent "unsatisfiable".
fn mc_choice_is_selectable(choice: &Element) -> Result<bool, WordIrError> {
    let Some(requires) = attr_value(choice, "Requires") else {
        return Ok(true);
    };
    for prefix in requires.split_whitespace() {
        let Some(ns_uri) = choice
            .namespaces
            .as_ref()
            .and_then(|ns| ns.get(prefix))
            .filter(|uri| !uri.is_empty())
        else {
            return Err(WordIrError::UnresolvableMcRequiresPrefix {
                prefix: prefix.to_string(),
            });
        };
        if !UNDERSTOOD_MC_NAMESPACES.contains(&ns_uri) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Is this namespace URI one this consumer understands for MCE Step-1 purposes?
///
/// An element in an ignorable namespace is dropped (§9.2) ONLY when its namespace
/// is NOT in the application configuration. For stemma the configuration is the
/// set of OOXML namespaces it can faithfully round-trip — the WML core plus every
/// Microsoft/OOXML extension namespace it preserves (`KNOWN_OOXML_NAMESPACES` +
/// the core list). This is deliberately BROADER than [`UNDERSTOOD_MC_NAMESPACES`]
/// (the Choice-selection set): w15/w16/etc. content is not *selected* by a
/// Requires, but it is preserved opaquely, so it must NEVER be dropped as
/// "ignored". Only genuinely-foreign namespaces (unknown extensions) are dropped.
fn mce_namespace_understood(ns_uri: &str) -> bool {
    crate::word_xml::CORE_NAMESPACE_URIS.contains(&ns_uri)
        || crate::word_xml::KNOWN_OOXML_NAMESPACES
            .iter()
            .any(|(_, uri)| *uri == ns_uri)
}

/// Accumulated MCE declarations in scope at a given point in the tree (§7.2/§7.3/
/// §7.4): the set of ignorable namespace URIs, the process-content name pairs
/// (`(namespace_uri, local_or_star)`), and the prefix→URI bindings needed to
/// resolve the prefix tokens those attributes carry. Cloned and extended on
/// descent so each element sees its ancestors' declarations.
#[derive(Clone, Default)]
pub(crate) struct MceScope {
    ignorable: Vec<String>,
    process_content: Vec<(String, String)>,
    ns_bindings: HashMap<String, String>,
}

impl MceScope {
    /// Build the seed scope from a top-down list of ANCESTOR elements (e.g.
    /// `[w:document, w:body]`) whose `mc:Ignorable`/`mc:ProcessContent` and xmlns
    /// declarations govern every descendant (§9.2: "an Ignorable attribute of
    /// this element or of an ancestor element"). Pass childless elements — only
    /// the attributes and namespace declarations matter.
    ///
    /// Also evaluates `mc:MustUnderstand` on these ancestors (§7.4/§9.4): the
    /// document root is the natural producer placement for it, just like
    /// mc:Ignorable, and the per-element MustUnderstand check in
    /// [`mce_step1_transform_node`] only ever visits body CHILDREN — so a
    /// root-level MustUnderstand would otherwise be silently ignored. A required
    /// namespace outside the understood set is a mismatch and fails loud here
    /// ([`WordIrError::McMustUnderstandUnsupported`]), never a silent continue.
    pub(crate) fn from_ancestors(ancestors: &[&Element]) -> Result<MceScope, WordIrError> {
        let mut scope = MceScope::default();
        for el in ancestors {
            scope = scope.extended_with(el);
            // Resolve this ancestor's MustUnderstand prefixes against the scope
            // accumulated so far (its own + outer ancestors' xmlns bindings).
            if let Some(val) = attr_value(el, "MustUnderstand") {
                for prefix in val.split_whitespace() {
                    if let Some(uri) = scope.ns_bindings.get(prefix)
                        && !mce_namespace_understood(uri)
                    {
                        return Err(WordIrError::McMustUnderstandUnsupported {
                            namespace: uri.clone(),
                        });
                    }
                }
            }
        }
        Ok(scope)
    }

    /// Whether this scope carries any ignorable namespace or process-content pair.
    /// An empty seed means no ancestor MCE directive is in force, so the cheap
    /// gate can skip the transform unless a subtree-local directive is present.
    pub(crate) fn has_directives(&self) -> bool {
        !self.ignorable.is_empty() || !self.process_content.is_empty()
    }

    /// Extend the scope with the xmlns declarations and the
    /// Ignorable/ProcessContent attributes that appear on `element`.
    /// Returns the extended scope. MustUnderstand is NOT folded into the scope
    /// (it triggers a refusal, not a scope change) — it is evaluated per-element
    /// in [`mce_step1_transform_node`] for body descendants and in
    /// [`MceScope::from_ancestors`] for the document-root ancestors.
    fn extended_with(&self, element: &Element) -> MceScope {
        let mut next = self.clone();
        if let Some(ns) = &element.namespaces {
            for (prefix, uri) in ns.iter() {
                if !prefix.is_empty() {
                    next.ns_bindings.insert(prefix.to_string(), uri.to_string());
                }
            }
        }
        // Ignorable: whitespace-delimited prefixes -> namespace URIs (§7.2).
        if let Some(val) = attr_value(element, "Ignorable") {
            for prefix in val.split_whitespace() {
                if let Some(uri) = next.ns_bindings.get(prefix)
                    && !next.ignorable.iter().any(|u| u == uri)
                {
                    next.ignorable.push(uri.clone());
                }
            }
        }
        // ProcessContent: whitespace-delimited `prefix:local` or `prefix:*`
        // tokens -> (namespace URI, local-or-'*') pairs (§7.3).
        if let Some(val) = attr_value(element, "ProcessContent") {
            for token in val.split_whitespace() {
                if let Some((prefix, local)) = token.split_once(':')
                    && let Some(uri) = next.ns_bindings.get(prefix)
                {
                    let pair = (uri.clone(), local.to_string());
                    if !next.process_content.contains(&pair) {
                        next.process_content.push(pair);
                    }
                }
            }
        }
        next
    }

    fn is_ignorable(&self, ns_uri: &str) -> bool {
        self.ignorable.iter().any(|u| u == ns_uri)
    }

    /// Does an element with `(ns_uri, local)` match a process-content pair (§7.3:
    /// local matches exactly or the pair's local part is '*')?
    fn matches_process_content(&self, ns_uri: &str, local: &str) -> bool {
        self.process_content
            .iter()
            .any(|(n, l)| n == ns_uri && (l == "*" || l == local))
    }
}

/// MCE Step-1 (§9.2) + the ignored/unwrapped/MustUnderstand arms of Step-3 (§9.4)
/// applied to a subtree on the CONSUMPTION (model) path. Returns the transformed
/// nodes that replace `node` in its parent's child list:
/// - an ignored element (ignorable namespace, not understood, no ProcessContent
///   match) yields NO nodes — it is removed with its attributes and contents;
/// - an unwrapped element (ProcessContent match) yields its transformed children,
///   spliced in place (the wrapper is discarded);
/// - any other node is kept, with its element children transformed in place.
///
/// `mc:MustUnderstand` naming an unsupported namespace is a hard refusal
/// ([`WordIrError::McMustUnderstandUnsupported`]). mc:* elements (AlternateContent
/// /Choice/Fallback) are NOT touched here — branch resolution stays in
/// [`select_mc_branch`]; §7.2 forbids the MC namespace from being ignorable.
///
/// This runs over an owned subtree fed to import; it does NOT alter the verbatim
/// bytes stemma re-zips for an unedited document (those come from the package
/// scaffold, not this tree).
///
/// Scope: `scope` is seeded with the declarations in force from the ancestors
/// ABOVE this subtree (e.g. `w:document` + `w:body`, via
/// [`MceScope::from_ancestors`]) and extended with each element's own
/// declarations on descent. So §9.2's "this element or an ANCESTOR element" is
/// honored whether the `mc:Ignorable`/`mc:ProcessContent` lives on the document
/// root, an intermediate ancestor, or the element itself. A foreign element NOT
/// covered by any in-scope Ignorable still surfaces and, if it is an unknown
/// element, fails loud downstream as before (no silent swallow).
fn mce_step1_transform_node(node: &XMLNode, scope: &MceScope) -> Result<Vec<XMLNode>, WordIrError> {
    let XMLNode::Element(element) = node else {
        // Non-element nodes (text, comments, …) pass through unchanged.
        return Ok(vec![node.clone()]);
    };

    // MustUnderstand is evaluated on every element (§9.4): a producer demanding a
    // namespace we lack is a mismatch we must signal, regardless of this
    // element's own fate.
    if let Some(val) = attr_value(element, "MustUnderstand") {
        let local_scope = scope.extended_with(element);
        for prefix in val.split_whitespace() {
            if let Some(uri) = local_scope.ns_bindings.get(prefix)
                && !mce_namespace_understood(uri)
            {
                return Err(WordIrError::McMustUnderstandUnsupported {
                    namespace: uri.clone(),
                });
            }
        }
    }

    // The scope governing THIS element's fate includes its own
    // Ignorable/ProcessContent declarations: §9.2 says an element is ignorable if
    // its namespace is declared ignorable "by an Ignorable attribute of this
    // element or of an ancestor element". The same extended scope is what the
    // element's children inherit.
    let child_scope = scope.extended_with(element);

    // This element's own namespace decides ignored/unwrapped/kept. An element with
    // no namespace (or in the MC namespace) is never ignorable.
    let elem_ns = element.namespace.as_deref();
    if let Some(ns_uri) = elem_ns
        && ns_uri != MC_NS
        && child_scope.is_ignorable(ns_uri)
        && !mce_namespace_understood(ns_uri)
    {
        if child_scope.matches_process_content(ns_uri, &element.name) {
            // Unwrapped: discard the wrapper, splice its transformed children in
            // place (§9.2/§9.4). Attributes of the wrapper are lost (§9.4).
            let mut out = Vec::new();
            for child in &element.children {
                out.extend(mce_step1_transform_node(child, &child_scope)?);
            }
            return Ok(out);
        }
        // Ignored: remove the element with its attributes and contents.
        return Ok(Vec::new());
    }

    // Kept element: transform its children in place.
    let mut new_children = Vec::with_capacity(element.children.len());
    for child in &element.children {
        new_children.extend(mce_step1_transform_node(child, &child_scope)?);
    }
    let mut kept = element.clone();
    kept.children = new_children;
    Ok(vec![XMLNode::Element(kept)])
}

/// Apply [`mce_step1_transform_node`] to a single element subtree, returning the
/// transformed element, or `None` if the element itself is ignored/unwrapped away
/// (an unwrapped or ignored root collapses to zero-or-many nodes — at the body/
/// story child level we only expect a single kept element, so a multi-node or
/// empty result means the whole child vanished from the model).
///
/// `seed` carries the MCE declarations in force from the ancestors ABOVE this
/// subtree (typically `w:document` + `w:body`), built by [`MceScope::from_ancestors`].
/// §9.2 makes an element ignorable when the declaration is on the element OR any
/// ancestor, so a document-root `mc:Ignorable` governs a foreign element deep in
/// the body even though that element carries no mc:* attribute itself.
pub fn mce_preprocess_element(
    element: &Element,
    seed: &MceScope,
) -> Result<Option<Element>, WordIrError> {
    let mut out = mce_step1_transform_node(&XMLNode::Element(element.clone()), seed)?;
    match out.len() {
        1 => match out.pop() {
            Some(XMLNode::Element(el)) => Ok(Some(el)),
            _ => Ok(None),
        },
        _ => Ok(None),
    }
}

/// Cheap pre-check: does any element in this subtree carry an
/// `mc:Ignorable`, `mc:ProcessContent`, or `mc:MustUnderstand` attribute?
/// Used to skip the (cloning) MCE Step-1 transform entirely for the common case
/// of a subtree with no MCE directives — the vast majority of paragraphs.
pub fn subtree_has_mce_directives(element: &Element) -> bool {
    if attr_value(element, "Ignorable").is_some()
        || attr_value(element, "ProcessContent").is_some()
        || attr_value(element, "MustUnderstand").is_some()
    {
        return true;
    }
    element
        .children
        .iter()
        .any(|child| matches!(child, XMLNode::Element(el) if subtree_has_mce_directives(el)))
}

/// Cheap pre-check: does any element in this subtree sit in a namespace this
/// consumer does NOT understand (a foreign extension namespace)? When an
/// ancestor `mc:Ignorable`/`mc:ProcessContent` is in force (a non-empty seed
/// scope) but the foreign element carries no mc:* attribute of its own,
/// [`subtree_has_mce_directives`] returns false — so we additionally gate on the
/// presence of a foreign-namespace element to decide whether the Step-1 transform
/// must run. Elements in understood namespaces never become ignored/unwrapped, so
/// a subtree of pure WML stays clone-free even under a document-root mc:Ignorable.
pub fn subtree_has_foreign_namespace_element(element: &Element) -> bool {
    if let Some(ns) = element.namespace.as_deref()
        && ns != MC_NS
        && !mce_namespace_understood(ns)
    {
        return true;
    }
    element.children.iter().any(
        |child| matches!(child, XMLNode::Element(el) if subtree_has_foreign_namespace_element(el)),
    )
}

/// Determine the inner content element name from an MC block's selected branch.
/// Used to classify the widget (e.g., "w:drawing" → Drawing) without substring
/// searching serialized XML. Returns the MC element's own name as fallback.
///
/// Propagates the branch-selection error (a non-conformant `Requires` prefix):
/// even though the MC block is preserved verbatim as an opaque widget, the same
/// malformed-markup invariant holds here as on the content path — fail loud
/// rather than classify a document we have already determined is non-conformant.
pub fn mc_inner_content_name(mc_element: &Element) -> Result<String, WordIrError> {
    if let Some(branch) = select_mc_branch(mc_element)? {
        for child in &branch.children {
            if let XMLNode::Element(el) = child {
                let local = local_element_name(el);
                // Skip property elements — we want the actual content
                if local != "rPr" && local != "pPr" {
                    return Ok(el.name.clone());
                }
            }
        }
    }
    Ok(mc_element.name.clone())
}

fn find_w_child<'a>(element: &'a Element, local: &str) -> Option<&'a Element> {
    element.children.iter().find_map(|child| {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => return None,
        };
        if is_w_tag(el, local) { Some(el) } else { None }
    })
}

fn attr_value<'a>(element: &'a Element, local: &str) -> Option<&'a String> {
    attr_get(element, local)
}

/// Extract numbering directive from w:pPr/w:numPr.
///
/// Returns `DirectNumPr::Absent` when no w:numPr is present,
/// `DirectNumPr::Suppressed` when numId=0 (§17.9.18: remove inherited numbering),
/// or `DirectNumPr::Active(NumProps)` for an active numbering reference.
fn extract_num_props(p_pr: &Element) -> DirectNumPr {
    let Some(num_pr) = find_w_child(p_pr, "numPr") else {
        return DirectNumPr::Absent;
    };
    let Some(num_id_elem) = find_w_child(num_pr, "numId") else {
        return DirectNumPr::Absent;
    };
    let Some(num_id) = attr_value(num_id_elem, "val").and_then(|v| v.parse::<u32>().ok()) else {
        return DirectNumPr::Absent;
    };

    // §17.9.18: numId=0 means "remove inherited numbering from this paragraph."
    if num_id == 0 {
        return DirectNumPr::Suppressed;
    }

    // ilvl defaults to 0 if not specified
    let ilvl = find_w_child(num_pr, "ilvl")
        .and_then(|el| attr_value(el, "val"))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    DirectNumPr::Active(NumProps { num_id, ilvl })
}

/// Extract style ID from w:pPr/w:pStyle element.
fn extract_style_id(p_pr: &Element) -> Option<IStr> {
    let p_style = find_w_child(p_pr, "pStyle")?;
    attr_value(p_style, "val").map(|s| IStr::from(s.as_str()))
}

/// Extract outline level from w:pPr/w:outlineLvl element.
/// Returns 0-based level (0 = heading level 1, 8 = heading level 9).
fn extract_outline_lvl(p_pr: &Element) -> Option<u8> {
    let outline = find_w_child(p_pr, "outlineLvl")?;
    let val = attr_value(outline, "val")?;
    let n: u8 = val.parse().ok()?;
    // ST_DecimalNumber per §17.3.1.20: 0-8 map to heading levels 1-9, and 9
    // means "body text" (explicitly NOT a heading). All ten are valid authored
    // values that must round-trip; we carry the directly-authored level verbatim
    // for fidelity. The heading-level/body-text DISTINCTION is made downstream
    // (derive_heading_level_number excludes 9) — dropping the element here would
    // be a silent state-3 loss of an explicitly-authored body-text marker.
    if n <= 9 { Some(n) } else { None }
}

/// Extract alignment from w:pPr/w:jc element.
fn extract_alignment(p_pr: &Element) -> Option<String> {
    let jc = find_w_child(p_pr, "jc")?;
    attr_value(jc, "val").cloned()
}

/// Extract indentation from w:pPr/w:ind element.
fn extract_indentation(p_pr: &Element) -> Option<IndentProps> {
    let ind = find_w_child(p_pr, "ind")?;

    let left = attr_value(ind, "left")
        .or_else(|| attr_value(ind, "start"))
        .and_then(|v| v.parse().ok());

    let right = attr_value(ind, "right")
        .or_else(|| attr_value(ind, "end"))
        .and_then(|v| v.parse().ok());

    // §17.3.1.12: "The firstLine and hanging attributes are mutually
    // exclusive, if both are specified, then the firstLine value is ignored."
    // hanging wins when both are present.
    let first_line = if let Some(hanging) = attr_value(ind, "hanging") {
        hanging.parse::<i32>().ok().map(|v| -v)
    } else if let Some(first) = attr_value(ind, "firstLine") {
        first.parse().ok()
    } else {
        None
    };

    // Character-unit indents (MS-OI29500 2.1.44, §17.3.1.12): ST_DecimalNumber
    // in hundredths of a character — the East Asian layout variant. Stored raw,
    // INCLUDING an explicit 0: `leftChars="0"` is a real override that cancels a
    // character indent inherited from a style or numbering (2.1.44a), so it is
    // NOT equivalent to "absent" and must be preserved and re-emitted. Precedence
    // (a NON-ZERO chars value wins over its twips sibling) is applied downstream
    // by resolve_effective_indent, not at parse time — so do not filter zeros
    // here. leftChars/rightChars are the transitional-schema aliases of
    // startChars/endChars.
    let start_chars = attr_value(ind, "startChars")
        .or_else(|| attr_value(ind, "leftChars"))
        .and_then(|v| v.parse().ok());
    let end_chars = attr_value(ind, "endChars")
        .or_else(|| attr_value(ind, "rightChars"))
        .and_then(|v| v.parse().ok());
    // §17.3.1.12: "The firstLineChars and hangingChars attributes are mutually
    // exclusive, if both are specified, then the firstLineChars value is
    // ignored." A NON-ZERO hangingChars wins; hangingChars="0" is not a real
    // hanging indent and does not suppress firstLineChars.
    let raw_first_line_chars: Option<i32> =
        attr_value(ind, "firstLineChars").and_then(|v| v.parse().ok());
    let hanging_chars: Option<i32> = attr_value(ind, "hangingChars").and_then(|v| v.parse().ok());
    let first_line_chars = if hanging_chars.is_some_and(|h| h != 0) {
        None // non-zero hangingChars wins, firstLineChars is ignored
    } else {
        raw_first_line_chars
    };

    // Only return Some if at least one property is set
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

/// Extract w:textAlignment from w:pPr (§17.3.1.39).
fn extract_text_alignment(p_pr: &Element) -> Option<TextAlignment> {
    let el = find_w_child(p_pr, "textAlignment")?;
    let val = attr_value(el, "val")?;
    match TextAlignment::from_xml_str(val) {
        Ok(ta) => Some(ta),
        Err(e) => {
            eprintln!("warning: {e}");
            None
        }
    }
}

/// Extract an optional boolean pPr child element (CT_OnOff pattern).
/// Returns Some(true/false) if the element is present, None if absent.
fn extract_optional_bool(p_pr: &Element, tag: &str) -> Option<bool> {
    let el = find_w_child(p_pr, tag)?;
    match attr_value(el, "val") {
        Some(v) if v == "0" || v == "false" || v == "off" => Some(false),
        _ => Some(true),
    }
}

/// Extract w:framePr from w:pPr (§17.3.1.11).
fn extract_frame_pr(p_pr: &Element) -> Option<FrameProperties> {
    let el = find_w_child(p_pr, "framePr")?;
    Some(FrameProperties {
        width: attr_value(el, "w").and_then(|v| v.parse().ok()),
        height: attr_value(el, "h").and_then(|v| v.parse().ok()),
        h_rule: attr_value(el, "hRule").and_then(|v| match HeightRule::from_xml_str(v) {
            Ok(hr) => Some(hr),
            Err(e) => {
                eprintln!("warning: framePr {e}");
                None
            }
        }),
        h_space: attr_value(el, "hSpace").and_then(|v| v.parse().ok()),
        wrap: attr_value(el, "wrap").and_then(|v| match FrameWrap::from_xml_str(v) {
            Ok(fw) => Some(fw),
            Err(e) => {
                eprintln!("warning: framePr {e}");
                None
            }
        }),
        v_anchor: attr_value(el, "vAnchor").and_then(|v| match VAnchor::from_xml_str(v) {
            Ok(va) => Some(va),
            Err(e) => {
                eprintln!("warning: framePr {e}");
                None
            }
        }),
        h_anchor: attr_value(el, "hAnchor").and_then(|v| match HAnchor::from_xml_str(v) {
            Ok(ha) => Some(ha),
            Err(e) => {
                eprintln!("warning: framePr {e}");
                None
            }
        }),
        x: attr_value(el, "x").and_then(|v| v.parse().ok()),
        x_align: attr_value(el, "xAlign").and_then(|v| match XAlign::from_xml_str(v) {
            Ok(xa) => Some(xa),
            Err(e) => {
                eprintln!("warning: framePr {e}");
                None
            }
        }),
        y: attr_value(el, "y").and_then(|v| v.parse().ok()),
        y_align: attr_value(el, "yAlign").and_then(|v| match YAlign::from_xml_str(v) {
            Ok(ya) => Some(ya),
            Err(e) => {
                eprintln!("warning: framePr {e}");
                None
            }
        }),
        v_space: attr_value(el, "vSpace").and_then(|v| v.parse().ok()),
        // Everything not modeled above (dropCap, lines, anchorLock, unknowns)
        // is preserved verbatim (§17.3.1.11 CT_FramePr remainder).
        extra_attrs: capture_extra_attrs(
            el,
            &[
                "w", "h", "hRule", "hSpace", "vSpace", "wrap", "vAnchor", "hAnchor", "x", "xAlign",
                "y", "yAlign",
            ],
        ),
    })
}

/// Extract conditional formatting flags from w:pPr/w:cnfStyle (§17.3.1.8).
fn extract_cnf_style(p_pr: &Element) -> Option<crate::domain::CnfStyle> {
    let el = find_w_child(p_pr, "cnfStyle")?;
    let bool_attr =
        |name: &str| -> bool { attr_value(el, name).is_some_and(|v| v == "1" || v == "true") };
    Some(crate::domain::CnfStyle {
        val: attr_value(el, "val").cloned(),
        first_row: bool_attr("firstRow"),
        last_row: bool_attr("lastRow"),
        first_column: bool_attr("firstColumn"),
        last_column: bool_attr("lastColumn"),
        odd_v_band: bool_attr("oddVBand"),
        even_v_band: bool_attr("evenVBand"),
        odd_h_band: bool_attr("oddHBand"),
        even_h_band: bool_attr("evenHBand"),
        first_row_first_column: bool_attr("firstRowFirstColumn"),
        first_row_last_column: bool_attr("firstRowLastColumn"),
        last_row_first_column: bool_attr("lastRowFirstColumn"),
        last_row_last_column: bool_attr("lastRowLastColumn"),
    })
}

/// Parse section-level footnote or endnote properties (§17.11.3 / §17.11.2).
fn parse_note_properties(el: &Element) -> Result<crate::domain::NoteProperties, String> {
    let position = find_w_child(el, "pos")
        .and_then(|e| attr_value(e, "val"))
        .map(|v| crate::domain::NotePosition::from_xml_str(v))
        .transpose()?;
    let num_fmt = find_w_child(el, "numFmt")
        .and_then(|e| attr_value(e, "val"))
        .map(|v| crate::domain::NumberFormat::from_xml_str(v))
        .transpose()?;
    let num_start = find_w_child(el, "numStart")
        .and_then(|e| attr_value(e, "val"))
        .and_then(|v| v.parse().ok());
    let num_restart = find_w_child(el, "numRestart")
        .and_then(|e| attr_value(e, "val"))
        .map(|v| crate::domain::RestartRule::from_xml_str(v))
        .transpose()?;
    Ok(crate::domain::NoteProperties {
        position,
        num_fmt,
        num_start,
        num_restart,
    })
}

/// Extract spacing from w:pPr/w:spacing element (§17.3.1.33).
fn extract_spacing(p_pr: &Element) -> Option<SpacingProps> {
    let sp = find_w_child(p_pr, "spacing")?;

    let before = attr_value(sp, "before").and_then(|v| v.parse().ok());
    let after = attr_value(sp, "after").and_then(|v| v.parse().ok());
    let before_lines = attr_value(sp, "beforeLines").and_then(|v| v.parse().ok());
    let after_lines = attr_value(sp, "afterLines").and_then(|v| v.parse().ok());
    // §17.3.1.33: beforeAutospacing/afterAutospacing — ST_OnOff attribute.
    // When true, before/beforeLines (or after/afterLines) are ignored and spacing
    // is automatically determined by the consumer.
    let before_autospacing =
        attr_value(sp, "beforeAutospacing").map(|v| !matches!(v.as_str(), "0" | "false" | "off"));
    let after_autospacing =
        attr_value(sp, "afterAutospacing").map(|v| !matches!(v.as_str(), "0" | "false" | "off"));
    let line = attr_value(sp, "line").and_then(|v| v.parse().ok());
    // Per §17.3.1.33: "If [lineRule] is omitted, then it shall be assumed
    // to be of a value auto if a line attribute value is present."
    let line_rule = attr_value(sp, "lineRule")
        .cloned()
        .or_else(|| line.map(|_| "auto".to_string()));

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

/// Extract paragraph borders from w:pPr/w:pBdr element (§17.3.1.24).
fn extract_paragraph_borders(p_pr: &Element) -> Option<ParagraphBorderProps> {
    let pbdr = find_w_child(p_pr, "pBdr")?;

    let top = extract_border_edge(pbdr, "top");
    let bottom = extract_border_edge(pbdr, "bottom");
    let left = extract_border_edge(pbdr, "left").or_else(|| extract_border_edge(pbdr, "start"));
    let right = extract_border_edge(pbdr, "right").or_else(|| extract_border_edge(pbdr, "end"));
    let between = extract_border_edge(pbdr, "between");
    let bar = extract_border_edge(pbdr, "bar");

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

/// Extract w:keepNext from pPr (§17.3.1.14).
fn extract_keep_next(p_pr: &Element) -> Option<bool> {
    let el = find_w_child(p_pr, "keepNext")?;
    match attr_value(el, "val") {
        Some(v) if v == "0" || v == "false" || v == "off" => Some(false),
        _ => Some(true),
    }
}

/// Extract w:keepLines from pPr (§17.3.1.15).
fn extract_keep_lines(p_pr: &Element) -> Option<bool> {
    let el = find_w_child(p_pr, "keepLines")?;
    match attr_value(el, "val") {
        Some(v) if v == "0" || v == "false" || v == "off" => Some(false),
        _ => Some(true),
    }
}

/// Extract w:pageBreakBefore from pPr (§17.3.1.23).
/// CT_OnOff: None=absent, Some(true)=present/val=1, Some(false)=val=0.
fn extract_page_break_before(p_pr: &Element) -> Option<bool> {
    let el = find_w_child(p_pr, "pageBreakBefore")?;
    match attr_value(el, "val") {
        Some(v) if v == "0" || v == "false" || v == "off" => Some(false),
        _ => Some(true),
    }
}

/// Extract w:widowControl from pPr (§17.3.1.44).
/// Returns None if absent (inherit default = true per spec).
/// Returns Some(false) if val="0" or val="false", Some(true) otherwise.
fn extract_widow_control(p_pr: &Element) -> Option<bool> {
    let el = find_w_child(p_pr, "widowControl")?;
    match attr_value(el, "val") {
        Some(v) if v == "0" || v == "false" => Some(false),
        Some(_) => Some(true),
        None => Some(true), // <w:widowControl/> without val means true
    }
}

/// Extract w:contextualSpacing from pPr (§17.3.1.9).
/// CT_OnOff: absent = None (inherit), present without val or val="1"/"true" = Some(true),
/// val="0"/"false" = Some(false) (explicitly off).
fn extract_contextual_spacing(p_pr: &Element) -> Option<bool> {
    let el = find_w_child(p_pr, "contextualSpacing")?;
    match attr_value(el, "val") {
        Some(v) if v == "0" || v == "false" || v == "off" => Some(false),
        _ => Some(true), // no val attribute, or val="1"/"true"
    }
}

/// Extract w:shd from pPr (§17.3.1.31) — paragraph shading.
fn extract_paragraph_shading(
    p_pr: &Element,
) -> Option<(Option<String>, Option<String>, Option<String>)> {
    let shd = find_w_child(p_pr, "shd")?;
    let fill = attr_value(shd, "fill").cloned();
    let val = attr_value(shd, "val").cloned();
    let color = attr_value(shd, "color").cloned();
    Some((fill, val, color))
}

/// Extract a single border edge element from a border container.
fn extract_border_edge(container: &Element, edge_name: &str) -> Option<BorderEdge> {
    let edge = find_w_child(container, edge_name)?;
    let style = attr_value(edge, "val")
        .cloned()
        .unwrap_or_else(|| "none".to_string());
    let color = attr_value(edge, "color").cloned();
    let size = attr_value(edge, "sz").and_then(|v| v.parse().ok());
    let space = attr_value(edge, "space").and_then(|v| v.parse().ok());
    Some(BorderEdge {
        style,
        color,
        size,
        space,
    })
}

/// Extract w:sectPrChange from w:pPr > w:sectPr.
///
/// Navigates pPr -> sectPr -> sectPrChange, extracting the revision metadata
/// (id, author, date) and the previous section properties as raw XML bytes.
fn extract_section_property_change(p_pr: &Element) -> Option<SectionPropertyChange> {
    let sect_pr = find_w_child(p_pr, "sectPr")?;
    let change_el = find_w_child(sect_pr, "sectPrChange")?;
    // A missing or unparseable w:id takes 0 and gets a minted identity from
    // the wire-zero pass (mint_wire_zero_revision_ids) — the same handling as
    // the rPr/pPr parsers; silently dropping the record here would hide a
    // pending revision. Likewise an absent previous-sectPr child is an EMPTY
    // previous state (LibreOffice omits it; Word writes `<w:sectPr/>`).
    let revision_id = attr_value(change_el, "id")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let author = attr_value(change_el, "author").cloned();
    let date = attr_value(change_el, "date").cloned();
    let empty_previous = Element::new("sectPr");
    let prev_sect_pr = find_w_child(change_el, "sectPr").unwrap_or(&empty_previous);
    let previous_properties_raw = serialize_element(prev_sect_pr);
    Some(SectionPropertyChange {
        revision: RevisionInfo {
            revision_id,
            identity: 0,
            author,
            date,
            apply_op_id: None,
        },
        previous_properties_raw,
    })
}

/// Extract tracked paragraph property change from w:pPr/w:pPrChange (§17.13.5.29).
/// The inner w:pPr contains the previous paragraph properties before the change.
fn extract_ppr_change(p_pr: &Element) -> Option<PprChange> {
    let ppr_change_el = find_w_child(p_pr, "pPrChange")?;
    let author = attr_value(ppr_change_el, "author")
        .cloned()
        .unwrap_or_default();
    let date = attr_value(ppr_change_el, "date").cloned();
    // Word always writes the previous-state child, even when empty
    // (`<w:pPr/>`); LibreOffice omits it when the prior paragraph had no
    // direct properties. An absent child is an EMPTY previous state, never a
    // reason to drop the tracked change (same rule as the rPrChange parse).
    let empty_previous = Element::new("pPr");
    let inner_ppr = find_w_child(ppr_change_el, "pPr").unwrap_or(&empty_previous);
    let revision_id = attr_value(ppr_change_el, "id")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    // --- Preserved remainder: unmodeled inner-pPr child ---
    //
    // Mirrors the outer paragraph's preserved-remainder walk in
    // `ParagraphView::from_paragraph`: a child of this pPrChange's previous
    // pPr without a typed field (see `PPR_CHANGE_MODELED_CHILDREN`) is
    // captured verbatim rather than dropped.
    let mut preserved = Vec::new();
    for inner_child in &inner_ppr.children {
        let inner_el = match inner_child {
            XMLNode::Element(el) => el,
            _ => continue,
        };
        let inner_local = local_element_name(inner_el);
        if PPR_CHANGE_MODELED_CHILDREN.contains(&inner_local.as_str()) {
            continue;
        }
        tracing::debug!(
            element = %inner_local,
            "extract_ppr_change: unmodeled pPrChange previous-pPr child element captured verbatim as a preserved remainder"
        );
        preserved.push(crate::domain::PreservedProp {
            name: qualified_element_name(inner_el),
            raw_xml: String::from_utf8(serialize_element(inner_el))
                .expect("serialize_element always emits valid UTF-8 XML"),
        });
    }

    Some(PprChange {
        revision_id,
        previous_alignment: extract_alignment(inner_ppr),
        previous_indentation: extract_indentation(inner_ppr),
        previous_spacing: extract_spacing(inner_ppr),
        previous_style_id: extract_style_id(inner_ppr),
        previous_borders: extract_paragraph_borders(inner_ppr),
        previous_keep_next: extract_keep_next(inner_ppr),
        previous_keep_lines: extract_keep_lines(inner_ppr),
        previous_page_break_before: extract_page_break_before(inner_ppr),
        previous_widow_control: extract_widow_control(inner_ppr),
        previous_contextual_spacing: extract_contextual_spacing(inner_ppr),
        previous_shading: extract_paragraph_shading(inner_ppr),
        previous_tab_stops: extract_tab_stops(inner_ppr),
        previous_mirror_indents: extract_optional_bool(inner_ppr, "mirrorIndents"),
        previous_auto_space_de: extract_optional_bool(inner_ppr, "autoSpaceDE"),
        previous_auto_space_dn: extract_optional_bool(inner_ppr, "autoSpaceDN"),
        previous_bidi: extract_optional_bool(inner_ppr, "bidi"),
        previous_text_alignment: extract_text_alignment(inner_ppr),
        previous_text_direction: find_w_child(inner_ppr, "textDirection")
            .and_then(|el| attr_value(el, "val"))
            .and_then(|s| crate::domain::TextDirection::from_xml_str(s).ok()),
        previous_suppress_auto_hyphens: extract_optional_bool(inner_ppr, "suppressAutoHyphens"),
        previous_snap_to_grid: extract_optional_bool(inner_ppr, "snapToGrid"),
        previous_overflow_punct: extract_optional_bool(inner_ppr, "overflowPunct"),
        previous_adjust_right_ind: extract_optional_bool(inner_ppr, "adjustRightInd"),
        previous_word_wrap: extract_optional_bool(inner_ppr, "wordWrap"),
        previous_frame_pr: extract_frame_pr(inner_ppr),
        previous_paragraph_mark_rpr: extract_paragraph_mark_rpr(inner_ppr),
        preserved,
        author,
        date,
    })
}

fn extract_paragraph_mark_rpr(p_pr: &Element) -> TextMarks {
    find_w_child(p_pr, "rPr")
        .map(parse_rpr_element)
        .unwrap_or_default()
}

/// Extract paragraph mark tracking status from w:pPr/w:rPr (§17.13.5.28).
///
/// A `w:del` inside `w:pPr/w:rPr` means the paragraph mark was deleted (paragraph join).
/// A `w:ins` inside `w:pPr/w:rPr` means the paragraph mark was inserted (paragraph split).
fn extract_para_mark_status(p_pr: &Element) -> Option<TrackingStatus> {
    let rpr = find_w_child(p_pr, "rPr")?;
    let mut mark_ins: Option<RevisionInfo> = None;
    let mut mark_del: Option<RevisionInfo> = None;
    for child in &rpr.children {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };
        // A paragraph-mark `moveFrom` means the paragraph break moved away — Word
        // normalizes this to a DELETE of the mark (confirmed against real Word:
        // accept of a paragraph-mark moveFrom MERGES the paragraphs, identical to a
        // paragraph-mark `del`). Symmetrically `moveTo` is an insert (§17.13.5.21/.26
        // vs §17.13.5.15).
        if is_w_tag(el, "del") || is_w_tag(el, "moveFrom") {
            let revision_id = attr_value(el, "id")?.parse().ok()?;
            mark_del = Some(RevisionInfo {
                revision_id,
                identity: 0,
                author: attr_value(el, "author").cloned(),
                date: attr_value(el, "date").cloned(),
                apply_op_id: None,
            });
        }
        if is_w_tag(el, "ins") || is_w_tag(el, "moveTo") {
            let revision_id = attr_value(el, "id")?.parse().ok()?;
            mark_ins = Some(RevisionInfo {
                revision_id,
                identity: 0,
                author: attr_value(el, "author").cloned(),
                date: attr_value(el, "date").cloned(),
                apply_op_id: None,
            });
        }
    }
    // BOTH markers = the stacked paragraph mark: the break was inserted by
    // one pending revision and deleted by another (real EBA/EMA corpus
    // documents carry this). The break survives only the mixed
    // accept-insert + reject-delete resolution — same origin rules as
    // inline text and rows.
    match (mark_ins, mark_del) {
        (Some(inserted), Some(deleted)) => Some(TrackingStatus::InsertedThenDeleted(Box::new(
            crate::domain::StackedRevision { inserted, deleted },
        ))),
        (Some(rev), None) => Some(TrackingStatus::Inserted(rev)),
        (None, Some(rev)) => Some(TrackingStatus::Deleted(rev)),
        (None, None) => None,
    }
}

/// Parse structured section properties from a w:sectPr element directly.
///
/// Extracts page size (w:pgSz), column layout (w:cols), margins (w:pgMar),
/// and other section-level properties into typed fields.
///
/// `rel_lookup` maps relationship IDs (e.g. "rId17") to their targets
/// (e.g. "header3.xml"). Header/footer references are resolved at parse time
/// so the domain model stores part paths, not rIds.
/// Resolve a `w:headerReference`/`w:footerReference` `w:type` attribute to a
/// [`crate::domain::HeaderFooterKind`] (§17.10.4 `ST_HdrFtr`: default | even
/// | first).
///
/// Single source of truth for this mapping — previously import.rs's
/// `parse_header_footer_ref` (rId-based header/footer refs) and this file's
/// inline sectPr headerReference/footerReference parsing each had their own
/// copy, and they disagreed: import.rs hard-errored on an unrecognized
/// `w:type`, this file silently defaulted. Unified here on the lenient side
/// — `parse_section_properties` is infallible and called from runtime.rs and
/// tracked_model.rs, so making this fail-fast would require threading a
/// `Result` through call sites outside this sweep's scope. It's also the
/// more defensible choice on the merits: a single unrecognized header/footer
/// reference is not worth refusing an otherwise well-formed document over
/// (invariant #1, parse totality) — Word itself degrades gracefully. The
/// degradation is now observable instead of silent.
///
/// "odd" is a non-standard synonym for "default" emitted by some producers
/// (e.g. Apache POI); OOXML's "default" already means "odd pages" when
/// `evenAndOddHeaders` is enabled, so the mapping is semantically exact.
pub(crate) fn parse_header_footer_kind(type_attr: Option<&str>) -> crate::domain::HeaderFooterKind {
    match type_attr {
        Some("first") => crate::domain::HeaderFooterKind::First,
        Some("even") => crate::domain::HeaderFooterKind::Even,
        Some("default") | Some("odd") | None => crate::domain::HeaderFooterKind::Default,
        Some(other) => {
            tracing::warn!(
                w_type = %other,
                "headerReference/footerReference: unrecognized w:type, treating as default"
            );
            crate::domain::HeaderFooterKind::Default
        }
    }
}

pub(crate) fn parse_section_properties(
    sect_pr: &Element,
    rel_lookup: &std::collections::HashMap<String, String>,
) -> SectionProperties {
    // w:pgSz -- page size, orientation, and paper size code (§17.6.14)
    let (page_width, page_height, orientation, paper_size_code) =
        if let Some(pg_sz) = find_w_child(sect_pr, "pgSz") {
            let w = attr_value(pg_sz, "w").and_then(|v| v.parse::<u32>().ok());
            let h = attr_value(pg_sz, "h").and_then(|v| v.parse::<u32>().ok());
            let orient = attr_value(pg_sz, "orient").and_then(|v| match v.as_str() {
                "landscape" => Some(PageOrientation::Landscape),
                "portrait" => Some(PageOrientation::Portrait),
                _ => None,
            });
            let code = attr_value(pg_sz, "code").and_then(|v| v.parse::<i64>().ok());
            (w, h, orient, code)
        } else {
            (None, None, None, None)
        };

    // w:cols -- column layout (§17.6.4)
    let (columns, column_space, column_separator, equal_width) =
        if let Some(cols) = find_w_child(sect_pr, "cols") {
            let num = attr_value(cols, "num").and_then(|v| v.parse::<u32>().ok());
            let space = attr_value(cols, "space").and_then(|v| v.parse::<u32>().ok());
            let sep = attr_value(cols, "sep").map(|v| v == "1" || v == "true");
            // §17.6.4 equalWidth (CT_OnOff). Word defaults to true; capture an
            // explicit value so a sectPr rebuild preserves unequal columns.
            let equal = attr_value(cols, "equalWidth").map(|v| v == "1" || v == "true");
            (num, space, sep, equal)
        } else {
            (None, None, None, None)
        };

    // w:pgMar -- page margins (§17.6.11)
    let (
        margin_top,
        margin_bottom,
        margin_left,
        margin_right,
        header_distance,
        footer_distance,
        gutter,
    ) = if let Some(pg_mar) = find_w_child(sect_pr, "pgMar") {
        (
            attr_value(pg_mar, "top").and_then(|v| v.parse::<i32>().ok()),
            attr_value(pg_mar, "bottom").and_then(|v| v.parse::<i32>().ok()),
            attr_value(pg_mar, "left").and_then(|v| v.parse::<i32>().ok()),
            attr_value(pg_mar, "right").and_then(|v| v.parse::<i32>().ok()),
            attr_value(pg_mar, "header").and_then(|v| v.parse::<u32>().ok()),
            attr_value(pg_mar, "footer").and_then(|v| v.parse::<u32>().ok()),
            attr_value(pg_mar, "gutter").and_then(|v| v.parse::<u32>().ok()),
        )
    } else {
        (None, None, None, None, None, None, None)
    };

    // w:type -- section type (§17.6.17)
    let section_type = find_w_child(sect_pr, "type")
        .and_then(|el| attr_value(el, "val"))
        .and_then(|s| crate::domain::SectionType::from_xml_str(s).ok());

    // w:pgBorders -- page borders (§17.6.7)
    let page_borders = find_w_child(sect_pr, "pgBorders").and_then(|pg_borders| {
        use crate::domain::{Border, BorderStyle, PageBorders};
        let parse_edge = |name: &str| -> Option<Border> {
            let el = find_w_child(pg_borders, name)?;
            let style_str = attr_value(el, "val").map(|s| s.as_str()).unwrap_or("none");
            let style = match BorderStyle::from_xml_str(style_str) {
                Ok(s) => s,
                Err(e) => {
                    if crate::runtime::runtime_timing_logs_enabled() {
                        eprintln!("parse section pgBorders edge: {e}, defaulting to None");
                    }
                    BorderStyle::None
                }
            };
            Some(Border {
                style,
                size: attr_value(el, "sz").and_then(|v| v.parse::<u32>().ok()),
                color: attr_value(el, "color").map(|s| s.to_string()),
                space: attr_value(el, "space").and_then(|v| v.parse::<u32>().ok()),
                extra_attrs: Vec::new(),
            })
        };
        let top = parse_edge("top");
        let bottom = parse_edge("bottom");
        let left = parse_edge("left");
        let right = parse_edge("right");
        if top.is_some() || bottom.is_some() || left.is_some() || right.is_some() {
            // MS-OI29500 §17.6.10: defaults are zOrder="front", offsetFrom="text"
            let z_order = attr_value(pg_borders, "zOrder")
                .map(|s| s.to_string())
                .unwrap_or_else(|| "front".to_string());
            let offset_from = attr_value(pg_borders, "offsetFrom")
                .map(|s| s.to_string())
                .unwrap_or_else(|| "text".to_string());
            Some(PageBorders {
                top,
                bottom,
                left,
                right,
                z_order,
                offset_from,
            })
        } else {
            None
        }
    });

    // w:lnNumType -- line numbering (§17.6.8)
    let line_numbering =
        find_w_child(sect_pr, "lnNumType").map(|ln| crate::domain::LineNumbering {
            count_by: attr_value(ln, "countBy").and_then(|v| v.parse::<u32>().ok()),
            start: attr_value(ln, "start").and_then(|v| v.parse::<u32>().ok()),
            restart: attr_value(ln, "restart").map(|s| s.to_string()),
            distance: attr_value(ln, "distance").and_then(|v| v.parse::<u32>().ok()),
        });

    // w:vAlign -- vertical alignment (§17.6.20)
    let v_align = find_w_child(sect_pr, "vAlign")
        .and_then(|el| attr_value(el, "val"))
        .and_then(|s| crate::domain::SectionVAlign::from_xml_str(s).ok());

    // w:pgNumType -- page number type (§17.6.12)
    let page_number_type =
        find_w_child(sect_pr, "pgNumType").map(|pn| crate::domain::PageNumberType {
            fmt: attr_value(pn, "fmt").map(|s| s.to_string()),
            start: attr_value(pn, "start").and_then(|v| v.parse::<u32>().ok()),
            chap_style: attr_value(pn, "chapStyle").and_then(|v| v.parse::<i64>().ok()),
            chap_sep: attr_value(pn, "chapSep").map(|s| s.to_string()),
        });

    // w:cols/w:col -- individual column definitions (MS-OI29500 §17.6.3/§17.6.4)
    let column_defs = if let Some(cols) = find_w_child(sect_pr, "cols") {
        cols.children
            .iter()
            .filter_map(|child| {
                if let xmltree::XMLNode::Element(el) = child
                    && is_w_tag(el, "col")
                {
                    let width = attr_value(el, "w")
                        .and_then(|v| v.parse::<u32>().ok())
                        .unwrap_or(0);
                    // MS-OI29500 §17.6.3: missing space attr defaults to 0
                    let space = attr_value(el, "space")
                        .and_then(|v| v.parse::<u32>().ok())
                        .unwrap_or(0);
                    return Some(crate::domain::ColumnDef { width, space });
                }
                None
            })
            .collect()
    } else {
        Vec::new()
    };

    // w:rtlGutter -- RTL gutter (§17.6.15)
    let rtl_gutter = find_w_child(sect_pr, "rtlGutter").map(|el| {
        // If present with no val attribute, it means "true" (toggle element)
        attr_value(el, "val").is_none_or(|v| v == "1" || v == "true")
    });

    // w:textDirection -- text direction (§17.6.19)
    let text_direction = find_w_child(sect_pr, "textDirection")
        .and_then(|el| attr_value(el, "val"))
        .and_then(|s| crate::domain::TextDirection::from_xml_str(s).ok());

    // w:docGrid -- document grid (§17.6.5)
    let (doc_grid_type, doc_grid_line_pitch, doc_grid_char_space) =
        if let Some(doc_grid) = find_w_child(sect_pr, "docGrid") {
            (
                attr_value(doc_grid, "type")
                    .and_then(|s| crate::domain::DocGridType::from_xml_str(s).ok()),
                attr_value(doc_grid, "linePitch").and_then(|v| v.parse::<u32>().ok()),
                attr_value(doc_grid, "charSpace").and_then(|v| v.parse::<u32>().ok()),
            )
        } else {
            (None, None, None)
        };

    // Boolean section flags: present-means-true pattern (§17.6.18, §17.6.1, §17.6.6, §17.6.9)
    let parse_bool_element = |name: &str| -> Option<bool> {
        find_w_child(sect_pr, name)
            .map(|el| attr_value(el, "val").is_none_or(|v| v == "1" || v == "true"))
    };
    let title_page = parse_bool_element("titlePg");
    let bidi = parse_bool_element("bidi");
    let form_prot = parse_bool_element("formProt");
    let no_endnote = parse_bool_element("noEndnote");

    // w:footnotePr / w:endnotePr -- note properties (§17.11.3 / §17.11.2)
    let footnote_pr = find_w_child(sect_pr, "footnotePr")
        .map(parse_note_properties)
        .transpose()
        .unwrap_or(None);
    let endnote_pr = find_w_child(sect_pr, "endnotePr")
        .map(parse_note_properties)
        .transpose()
        .unwrap_or(None);

    // w:headerReference / w:footerReference — section story references (§17.10.4)
    // Resolve rId → part_path at the parse boundary so the domain model
    // stores document-independent part paths, never raw rIds.
    let mut header_refs = Vec::new();
    let mut footer_refs = Vec::new();
    for child in &sect_pr.children {
        if let xmltree::XMLNode::Element(el) = child {
            if is_w_tag(el, "headerReference") {
                if let Some(rel_id) = attr_value(el, "id") {
                    let kind = parse_header_footer_kind(attr_value(el, "type").map(|s| s.as_str()));
                    if let Some(part_path) = rel_lookup.get(rel_id.as_str()) {
                        header_refs.push(crate::domain::StoryRef {
                            kind,
                            part_path: part_path.clone(),
                            synthesized: false,
                        });
                    } else {
                        tracing::warn!(rel_id = %rel_id, "sectPr headerReference: rId not found in relationships, skipping");
                    }
                }
            } else if is_w_tag(el, "footerReference")
                && let Some(rel_id) = attr_value(el, "id")
            {
                let kind = parse_header_footer_kind(attr_value(el, "type").map(|s| s.as_str()));
                if let Some(part_path) = rel_lookup.get(rel_id.as_str()) {
                    footer_refs.push(crate::domain::StoryRef {
                        kind,
                        part_path: part_path.clone(),
                        synthesized: false,
                    });
                } else {
                    tracing::warn!(rel_id = %rel_id, "sectPr footerReference: rId not found in relationships, skipping");
                }
            }
        }
    }

    // w:paperSrc — printer tray codes (§17.6.9). Both attributes are
    // ST_DecimalNumber, defaulting to 1 (auto). We preserve the
    // omitted/present distinction.
    let paper_source = find_w_child(sect_pr, "paperSrc").map(|el| crate::domain::PaperSource {
        first: attr_value(el, "first").and_then(|v| v.parse::<i64>().ok()),
        other: attr_value(el, "other").and_then(|v| v.parse::<i64>().ok()),
    });

    // w:printerSettings — relationship to printer settings part (§17.6.14).
    // Stored as the raw rId; the underlying part survives package roundtrip
    // via the untyped relationship carry-over.
    let printer_settings_rid = find_w_child(sect_pr, "printerSettings")
        .and_then(|el| attr_value(el, "id"))
        .map(|s| s.to_string());

    SectionProperties {
        page_width,
        page_height,
        orientation,
        columns,
        column_space,
        column_defs,
        margin_top,
        margin_bottom,
        margin_left,
        margin_right,
        header_distance,
        footer_distance,
        gutter,
        rtl_gutter,
        section_type,
        page_borders,
        line_numbering,
        v_align,
        text_direction,
        page_number_type,
        doc_grid_type,
        doc_grid_line_pitch,
        doc_grid_char_space,
        title_page,
        bidi,
        form_prot,
        no_endnote,
        paper_size_code,
        column_separator,
        equal_width,
        footnote_pr,
        endnote_pr,
        header_refs,
        footer_refs,
        paper_source,
        printer_settings_rid,
    }
}

/// Extract section properties from a pPr element's sectPr child.
/// Returns None if the pPr has no sectPr child.
fn extract_section_properties(
    p_pr: &Element,
    rel_lookup: &std::collections::HashMap<String, String>,
) -> Option<SectionProperties> {
    let sect_pr = find_w_child(p_pr, "sectPr")?;
    Some(parse_section_properties(sect_pr, rel_lookup))
}

/// Extract tab stops from w:pPr/w:tabs element.
///
/// Parses `<w:tab>` children, extracting position (`w:pos`), alignment (`w:val`),
/// and leader (`w:leader`). Includes `clear` stops — they're needed for
/// inheritance resolution in the style chain.
fn extract_tab_stops(p_pr: &Element) -> Option<Vec<TabStopDef>> {
    let tabs_el = find_w_child(p_pr, "tabs")?;
    let mut stops = Vec::new();
    for child in &tabs_el.children {
        let el = match child {
            XMLNode::Element(el) if is_w_tag(el, "tab") => el,
            _ => continue,
        };
        let position = match attr_value(el, "pos").and_then(|v| v.parse::<i32>().ok()) {
            // MS-OI29500 2.1.95 §17.3.1.37 describes Word's LOAD-time clamp to
            // ±31680 — a consumption rule, NOT a save rewrite: Word preserves an
            // out-of-range w:pos verbatim in the markup. Clamping here rewrote
            // authored positions and collided them with real stops at the
            // boundary (silent value corruption). Keep the authored value; a
            // consumer that needs Word's effective behavior clamps at read.
            Some(pos) => pos,
            None => continue, // position is required
        };
        // w:val — parse tab alignment (ST_TabJc §17.18.81).
        // MS-OI29500 §17.18.84: "start" is an alias for "left", "end" for "right".
        let alignment = match attr_value(el, "val") {
            Some(v) => match v.as_str() {
                "start" => crate::domain::TabAlignment::Left,
                "end" => crate::domain::TabAlignment::Right,
                other => match crate::domain::TabAlignment::from_xml_str(other) {
                    Ok(a) => a,
                    Err(_) => crate::domain::TabAlignment::Left, // spec: default to left
                },
            },
            None => crate::domain::TabAlignment::Left,
        };
        // w:leader — parse tab leader (ST_TabTlc §17.18.82).
        // "none" means no leader (same as absent).
        let leader = attr_value(el, "leader").and_then(|v| match v.as_str() {
            "none" => None,
            other => crate::domain::TabLeader::from_xml_str(other).ok(),
        });
        stops.push(TabStopDef {
            position,
            alignment,
            leader,
        });
    }
    if stops.is_empty() { None } else { Some(stops) }
}

/// If a paragraph has `\t` characters but fewer explicit stops than tabs,
/// synthesize stops at `default_interval` increments beyond the last explicit stop.
///
/// `left_indent_twips` is the paragraph's left indent in twips — default tab stops
/// at or before this position are skipped (Word never advances to a stop the cursor
/// has already passed).
///
/// Precondition: `explicit_stops` must already be normalized — sorted ascending
/// by position, "clear" entries removed, de-duped (`resolve_effective_tabs` handles this).
pub fn synthesize_default_tab_stops(
    explicit_stops: &[TabStopDef],
    tab_count: usize,
    default_interval: i32,
    left_indent_twips: i32,
) -> Vec<TabStopDef> {
    if tab_count == 0 {
        return explicit_stops.to_vec();
    }
    let needed = tab_count.min(100); // safety guard against runaway generation
    if explicit_stops.len() >= needed {
        return explicit_stops.to_vec();
    }

    // A zero or negative default tab interval is invalid per ECMA-376
    // (w:defaultTabStop val must be positive). Fall back to the Word
    // default of 720 twips (0.5 inch).
    let default_interval = if default_interval <= 0 {
        720
    } else {
        default_interval
    };

    let mut stops = explicit_stops.to_vec();
    // Start position: max of last explicit stop and paragraph indent (both clamped to 0).
    let last_pos = stops
        .last()
        .map(|s| s.position)
        .unwrap_or(0)
        .max(left_indent_twips.max(0));
    let mut pos = ((last_pos / default_interval) + 1) * default_interval;
    while stops.len() < needed {
        stops.push(TabStopDef {
            position: pos,
            alignment: crate::domain::TabAlignment::Left,
            leader: None,
        });
        pos += default_interval;
    }
    stops
}

/// Extracts hyperlink data from a w:hyperlink element.
/// The URL is resolved later via relationship lookup if r:id is present.
fn extract_hyperlink_data(element: &Element) -> HyperlinkData {
    // Get anchor attribute (for internal document links)
    let anchor = attr_value(element, "anchor").cloned();

    // Get r:id attribute (for external URL resolution via relationships)
    let r_id = attr_get(element, "r:id").cloned();

    // Collect extra attributes (anything that is not r:id or w:anchor).
    // These include w:history, w:tgtFrame, w:tooltip, w:docLocation, etc.
    let known_local_names: &[&str] = &["id", "anchor"];
    let mut extra_attrs: Vec<(String, String)> = Vec::new();
    for (attr_name, attr_value) in &element.attributes {
        // Skip r:id (local "id" with relationships namespace or prefix "r")
        if attr_name.prefix.as_deref() == Some("r") && attr_name.local_name == "id" {
            continue;
        }
        if attr_name.namespace.as_deref()
            == Some("http://schemas.openxmlformats.org/officeDocument/2006/relationships")
            && attr_name.local_name == "id"
        {
            continue;
        }
        // Skip w:anchor
        if known_local_names.contains(&&*attr_name.local_name)
            && attr_name.prefix.as_deref().is_none_or(|p| p == "w")
        {
            continue;
        }
        let qname = match &attr_name.prefix {
            Some(p) => format!("{}:{}", p, attr_name.local_name),
            None => attr_name.local_name.clone(),
        };
        extra_attrs.push((qname, attr_value.clone()));
    }

    // Extract per-run data (text + rPr) from direct and container-nested w:r children.
    let runs = extract_runs_from_hyperlink(element);

    // Concatenate all run texts for the backward-compatible `text` field.
    let text = runs
        .iter()
        .map(|r| r.text.as_str())
        .collect::<Vec<_>>()
        .join("");

    HyperlinkData {
        url: None, // Resolved later via relationship lookup using r_id
        anchor,
        text,
        r_id,
        runs,
        extra_attrs,
    }
}

/// A hoisted range marker: (element name, raw bytes).
type HoistedMarker = (String, Vec<u8>);

/// Bookmark/move-range markers inside a `w:hyperlink`, hoisted to the link's
/// edges as `(element name, raw bytes)`, split into (before, after).
///
/// The hyperlink IR (`HyperlinkData.runs`) models display text only, so a
/// zero-width range marker inside the element has no slot — dropping it (the
/// earlier behavior) tore the pair whenever the other half sat outside
/// the link (Word nests `_Toc*` bookmarks inside TOC-heading hyperlinks
/// routinely). Hoisting keeps the pair intact: markers seen before any run
/// content anchor BEFORE the hyperlink, the rest AFTER, so the marked span
/// still covers the hyperlink text; only the boundary's position relative to
/// the link's inner runs is approximated (§17.13.2 allows markers at any
/// location; zero content sits between the original and hoisted positions on
/// the respective side).
fn hoisted_hyperlink_range_markers(element: &Element) -> (Vec<HoistedMarker>, Vec<HoistedMarker>) {
    fn walk(
        el: &Element,
        seen_content: &mut bool,
        before: &mut Vec<HoistedMarker>,
        after: &mut Vec<HoistedMarker>,
    ) {
        for child in &el.children {
            let XMLNode::Element(c) = child else { continue };
            let local = local_element_name(c);
            match local.as_str() {
                "bookmarkStart" | "bookmarkEnd" | "moveFromRangeStart" | "moveFromRangeEnd"
                | "moveToRangeStart" | "moveToRangeEnd" => {
                    let entry = (c.name.clone(), serialize_element(c));
                    if *seen_content {
                        after.push(entry);
                    } else {
                        before.push(entry);
                    }
                }
                "r" | "fldSimple" => *seen_content = true,
                // Transparent containers the run collector also descends into.
                "ins" | "del" | "moveFrom" | "moveTo" | "smartTag" | "sdt" | "customXml"
                | "dir" | "bdo" | "hyperlink" => {
                    walk(c, seen_content, before, after);
                }
                _ => {}
            }
        }
    }
    let mut seen_content = false;
    let mut before = Vec::new();
    let mut after = Vec::new();
    walk(element, &mut seen_content, &mut before, &mut after);
    (before, after)
}

/// Push hoisted hyperlink range markers as zero-width decoration atoms.
fn push_hoisted_marker_atoms(
    markers: Vec<HoistedMarker>,
    paragraph_child_index: usize,
    atoms: &mut Vec<Atom>,
) {
    for (name, raw_xml) in markers {
        atoms.push(Atom {
            kind: AtomKind::Decoration { name, raw_xml },
            utf16_len: 0, // Zero-width
            source_run_attrs: Vec::new(),
            origin: AtomOrigin {
                run_index: None,
                child_index: None,
                paragraph_child_index: Some(paragraph_child_index),
            },
            marks: TextMarks::default(),
            tracking: None,
        });
    }
}

/// Collects `HyperlinkRun` entries from a hyperlink element.
///
/// Handles direct `<w:r>` children as well as runs nested inside transparent
/// containers such as `<w:ins>`, `<w:del>`, and `<w:smartTag>`. Tracked-
/// change envelopes inside the hyperlink propagate their `Inserted`/
/// `Deleted` status to the runs they contain so the IR can represent the
/// hyperlink display text being edited via tracked changes.
fn extract_runs_from_hyperlink(element: &Element) -> Vec<HyperlinkRun> {
    let mut runs = Vec::new();
    collect_hyperlink_runs(element, &mut runs);
    runs
}

/// Parse `w:id`/`w:author`/`w:date` attributes off a `<w:ins>`/`<w:del>`
/// (or `<w:moveTo>`/`<w:moveFrom>`) element and return the corresponding
/// `TrackingStatus`. Returns `None` if the required `w:id` is missing or
/// unparseable — callers fall back to the ambient status in that case.
fn parse_hyperlink_revision_status(el: &Element, inserted: bool) -> Option<TrackingStatus> {
    let revision_id = attr_value(el, "id")?.parse().ok()?;
    let author = attr_value(el, "author").cloned();
    let date = attr_value(el, "date").cloned();
    let info = RevisionInfo {
        revision_id,
        identity: 0,
        author,
        date,
        apply_op_id: None,
    };
    Some(if inserted {
        TrackingStatus::Inserted(info)
    } else {
        TrackingStatus::Deleted(info)
    })
}

fn collect_hyperlink_runs(element: &Element, out: &mut Vec<HyperlinkRun>) {
    collect_hyperlink_runs_with_status(element, TrackingStatus::Normal, out);
}

fn collect_hyperlink_runs_with_status(
    element: &Element,
    status: TrackingStatus,
    out: &mut Vec<HyperlinkRun>,
) {
    for child in &element.children {
        let XMLNode::Element(el) = child else {
            continue;
        };
        let local = local_element_name(el);
        match local.as_str() {
            "r" => {
                // Direct run: extract rPr and text.
                let rpr_xml = find_child_element(el, "rPr").map(serialize_element);
                let mut text = String::new();
                extract_text_recursive(el, &mut text);
                out.push(HyperlinkRun {
                    text,
                    rpr_xml,
                    source_run_attrs: source_run_attrs(el),
                    status: status.clone(),
                });
            }
            // Tracked-change envelopes inside a hyperlink: capture revision
            // metadata so child runs inherit the surrounding status. Per
            // ECMA-376 §17.13.5, CT_Hyperlink permits EG_PContent (which
            // includes `w:ins`/`w:del`/`w:moveFrom`/`w:moveTo`).
            "ins" | "moveTo" => {
                let new_status = parse_hyperlink_revision_status(el, /* inserted */ true)
                    .unwrap_or_else(|| status.clone());
                collect_hyperlink_runs_with_status(el, new_status, out);
            }
            "del" | "moveFrom" => {
                let new_status = parse_hyperlink_revision_status(el, /* inserted */ false)
                    .unwrap_or_else(|| status.clone());
                collect_hyperlink_runs_with_status(el, new_status, out);
            }
            // Transparent containers from EG_ContentRunContent (§17.16.5) and
            // EG_RunLevelElts that may wrap runs. fldSimple is EG_PContent-only
            // per the schema but handled defensively for non-conformant docs.
            "smartTag" | "sdt" | "customXml" | "dir" | "bdo" | "fldSimple" | "hyperlink" => {
                collect_hyperlink_runs_with_status(el, status.clone(), out);
            }
            // EG_RunLevelElts range markers — no text content, safe to skip.
            "bookmarkStart" | "bookmarkEnd" | "commentRangeStart" | "commentRangeEnd"
            | "proofErr" | "permStart" | "permEnd" | "moveFromRangeStart" | "moveFromRangeEnd"
            | "moveToRangeStart" | "moveToRangeEnd" => {}
            _ => {
                if crate::runtime::runtime_timing_logs_enabled() {
                    eprintln!("collect_hyperlink_runs: unhandled child <w:{local}>");
                }
            }
        }
    }
}

/// Returns the first child element with the given local name, if any.
fn find_child_element<'a>(element: &'a Element, local: &str) -> Option<&'a Element> {
    element.children.iter().find_map(|c| {
        if let XMLNode::Element(el) = c
            && local_element_name(el) == local
        {
            return Some(el);
        }
        None
    })
}

fn extract_text_recursive(element: &Element, out: &mut String) {
    for child in &element.children {
        if let XMLNode::Element(el) = child {
            let local_name = local_element_name(el);
            // w:t is the text element
            if local_name == "t" || local_name == "delText" {
                for text_child in &el.children {
                    if let XMLNode::Text(text) = text_child {
                        out.push_str(text);
                    }
                }
            } else if local_name == "noBreakHyphen" {
                // Non-text run leaf: project to its Unicode character so it is
                // not silently dropped. U+2011 NON-BREAKING HYPHEN is the exact
                // character <w:noBreakHyphen/> denotes (ISO 29500-1 §17.3.3.18),
                // mirroring the plain-run path (import.rs AtomKind::NoBreakHyphen).
                out.push('\u{2011}');
            } else {
                // Recurse into runs and other elements
                extract_text_recursive(el, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_foreign_namespace_paragraph_element_preserved_as_decoration() {
        // A third-party tool (OpenXML PowerTools / Templafy DocumentBuilder)
        // injects a foreign-namespace placeholder as a DIRECT child of w:p inside
        // a header story (outside the CT_P schema). It is NOT in any in-scope
        // mc:Ignorable, so the MCE transform leaves it in place. We must NOT refuse
        // the document and must NOT drop the marker: model it as a zero-width
        // decoration whose raw_xml round-trips the element verbatim.
        let xml = br#"<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:r><w:t>x</w:t></w:r><Insert Id="Templafy" xmlns="http://powertools.codeplex.com/documentbuilder/2011/insert" /></w:p></w:hdr>"#;
        let root = crate::word_xml::parse_document_xml(xml).unwrap();
        let para = root
            .children
            .iter()
            .find_map(|c| match c {
                XMLNode::Element(e) if e.name == "p" || e.name == "w:p" => Some(e),
                _ => None,
            })
            .expect("header has a w:p");

        // The parser must classify the default-xmlns child as foreign.
        let foreign = para
            .children
            .iter()
            .find_map(|c| match c {
                XMLNode::Element(e) if local_element_name(e) == "Insert" => Some(e),
                _ => None,
            })
            .expect("Insert element present");
        assert!(
            is_foreign_namespace_element(foreign),
            "Insert in the PowerTools namespace must be classified foreign (ns={:?})",
            foreign.namespace
        );

        let view = ParagraphView::from_paragraph(para, &Default::default())
            .expect("foreign-namespace paragraph element must import cleanly, not refuse");

        // Exactly one Decoration atom, zero-width, carrying the verbatim element.
        let deco = view
            .atoms
            .iter()
            .find(|a| matches!(a.kind, AtomKind::Decoration { .. }))
            .expect("foreign element becomes a Decoration atom");
        assert_eq!(deco.utf16_len, 0, "foreign placeholder occupies zero width");
        match &deco.kind {
            AtomKind::Decoration { name, raw_xml } => {
                assert_eq!(name, "Insert");
                let raw = String::from_utf8(raw_xml.clone()).unwrap();
                assert!(
                    raw.contains("Insert") && raw.contains("Templafy"),
                    "raw_xml must preserve the foreign element verbatim: {raw}"
                );
                assert!(
                    raw.contains("powertools.codeplex.com"),
                    "raw_xml must preserve the foreign namespace: {raw}"
                );
            }
            other => panic!("expected Decoration, got {other:?}"),
        }
    }

    #[test]
    fn test_unknown_wml_namespace_element_still_refused() {
        // GUARD: the foreign-namespace leniency must NOT swallow an unknown element
        // in the WML namespace. A w:-prefixed element we do not model is a genuine
        // spec gap and must still fail loud (no silent fallback).
        let xml = br#"<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:bogusElement/></w:p></w:hdr>"#;
        let root = crate::word_xml::parse_document_xml(xml).unwrap();
        let para = root
            .children
            .iter()
            .find_map(|c| match c {
                XMLNode::Element(e) if e.name == "p" || e.name == "w:p" => Some(e),
                _ => None,
            })
            .expect("header has a w:p");
        let result = ParagraphView::from_paragraph(para, &Default::default());
        assert!(
            matches!(&result, Err(WordIrError::UnknownParagraphElement(n)) if n.contains("bogusElement")),
            "unknown WML-namespace element must still fail loud, got: {result:?}"
        );
    }

    use std::io::Cursor;
    use xmltree::ParserConfig;

    /// Helper to parse XML with whitespace preservation (same config as production)
    fn parse_with_whitespace(xml: &str) -> Element {
        let config = ParserConfig::new()
            .ignore_comments(false)
            .whitespace_to_characters(true);
        Element::parse_with_config(Cursor::new(xml), config).unwrap()
    }

    /// Verify xmltree preserves whitespace-only text nodes with proper config
    #[test]
    fn test_xmltree_whitespace_preservation() {
        // XML with whitespace-only text node (xml:space="preserve")
        let xml = r#"<w:t xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xml:space="preserve"> </w:t>"#;
        let element = parse_with_whitespace(xml);
        let text = text_from_element(&element);

        assert_eq!(text, " ", "whitespace-only text should be preserved");
    }

    /// An unmodeled/unknown rPr child element is dropped in isolation — it
    /// must not abort parsing or corrupt sibling properties that parse fine.
    /// (The drop itself, and its non-round-tripping on re-serialization, is
    /// a documented, separate limitation — see the OBSERVABLE BOUNDARY
    /// comment on `parse_rpr_element`'s unknown-element arm.)
    #[test]
    fn parse_rpr_element_unknown_child_is_dropped_without_disrupting_siblings() {
        let rpr = parse_with_whitespace(
            r#"<w:rPr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:b/><w:totallyMadeUpElement w:val="x"/><w:i/></w:rPr>"#,
        );
        let marks = parse_rpr_element(&rpr);
        assert_eq!(
            marks.bold,
            MarkValue::On,
            "known sibling before the unknown element still parses"
        );
        assert_eq!(
            marks.italic,
            MarkValue::On,
            "known sibling after the unknown element still parses"
        );
    }

    /// An unrecognized ST_OnOff value on a run toggle property is
    /// schema-invalid (ST_OnOff only permits 0/1/true/false/on/off), but
    /// Word's own tolerant boolean parsing treats it as On — the same
    /// product-approved default as the styles.rs sibling parser.
    #[test]
    fn parse_toggle_value_unrecognized_val_defaults_to_on() {
        let rpr = parse_with_whitespace(
            r#"<w:rPr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:b w:val="maybe"/></w:rPr>"#,
        );
        let marks = parse_rpr_element(&rpr);
        assert_eq!(
            marks.bold,
            MarkValue::On,
            "unrecognized w:val on w:b should default to On, not Off or Inherit"
        );
    }

    /// Test multiple text elements simulating the problematic pattern
    #[test]
    fn test_xmltree_multiple_text_elements() {
        // Simulating: <w:t>THAT</w:t><w:t xml:space="preserve"> </w:t><w:t>in</w:t>
        let xml = r#"<w:r xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:t>THAT</w:t>
            <w:t xml:space="preserve"> </w:t>
            <w:t>in</w:t>
        </w:r>"#;

        let element = parse_with_whitespace(xml);

        // Extract text from all w:t children
        let mut texts = Vec::new();
        for child in &element.children {
            if let XMLNode::Element(el) = child
                && local_element_name(el) == "t"
            {
                texts.push(text_from_element(el));
            }
        }

        let combined = texts.join("");

        assert_eq!(
            texts,
            vec!["THAT", " ", "in"],
            "should preserve all text including whitespace-only"
        );
        assert_eq!(
            combined, "THAT in",
            "combined text should have space between THAT and in"
        );
    }

    #[test]
    fn test_select_mc_branch_prefers_choice() {
        let xml = r#"<mc:AlternateContent xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape">
            <mc:Choice Requires="wps">
                <w:drawing xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"/>
            </mc:Choice>
            <mc:Fallback>
                <w:pict xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"/>
            </mc:Fallback>
        </mc:AlternateContent>"#;
        let element = parse_with_whitespace(xml);
        let branch = select_mc_branch(&element).unwrap().unwrap();
        let local = local_element_name(branch);
        assert_eq!(local, "Choice", "should select Choice when both exist");
    }

    #[test]
    fn test_select_mc_branch_falls_back_to_fallback() {
        // No Choice element — only Fallback
        let xml = r#"<mc:AlternateContent xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006">
            <mc:Fallback>
                <w:pict xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"/>
            </mc:Fallback>
        </mc:AlternateContent>"#;
        let element = parse_with_whitespace(xml);
        let branch = select_mc_branch(&element).unwrap().unwrap();
        let local = local_element_name(branch);
        assert_eq!(
            local, "Fallback",
            "should fall back to Fallback when no Choice"
        );
    }

    #[test]
    fn test_select_mc_branch_ununderstood_namespace_falls_to_fallback() {
        // §9.3: a Choice whose Requires prefix resolves to a namespace NOT in the
        // application configuration is not selected → fall to Fallback. The prefix
        // must be BOUND (here to a fictional future namespace) so this exercises
        // "bound-but-not-understood", distinct from the unbound (non-conformant)
        // case covered by test_select_mc_branch_unbound_requires_prefix_errors.
        let xml = r#"<mc:AlternateContent xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:fut="http://example.com/2099/future">
            <mc:Choice Requires="fut">
                <w:r xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                    <w:t>future content</w:t>
                </w:r>
            </mc:Choice>
            <mc:Fallback>
                <w:r xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                    <w:t>fallback content</w:t>
                </w:r>
            </mc:Fallback>
        </mc:AlternateContent>"#;
        let element = parse_with_whitespace(xml);
        let branch = select_mc_branch(&element).unwrap().unwrap();
        let local = local_element_name(branch);
        assert_eq!(
            local, "Fallback",
            "a bound-but-not-understood Requires namespace must fall to Fallback (§9.3)"
        );
    }

    #[test]
    fn test_select_mc_branch_w14_requires_selects_choice() {
        // Confirmed against real Word: w14 (http://.../2010/wordml, MS-DOCX §2.6) IS in
        // Word's application configuration, so a Requires="w14" Choice is SELECTED,
        // never the Fallback. The prefix is bound on the AlternateContent.
        let xml = r#"<mc:AlternateContent xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml">
            <mc:Choice Requires="w14">
                <w:r xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                    <w:t>w14 content</w:t>
                </w:r>
            </mc:Choice>
            <mc:Fallback>
                <w:r xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                    <w:t>fallback content</w:t>
                </w:r>
            </mc:Fallback>
        </mc:AlternateContent>"#;
        let element = parse_with_whitespace(xml);
        let branch = select_mc_branch(&element).unwrap().unwrap();
        let local = local_element_name(branch);
        assert_eq!(
            local, "Choice",
            "w14 is in Word's MCE configuration, so the Choice wins over the Fallback"
        );
    }

    #[test]
    fn test_select_mc_branch_requires_resolves_prefix_not_literal() {
        // §7.6/§9.3: selection is by the namespace a Requires prefix is BOUND to,
        // not the literal prefix token. Here a nonstandard prefix `n1` is bound to
        // the WML main namespace (understood), so the Choice is selected.
        let xml = r#"<mc:AlternateContent xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:n1="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <mc:Choice Requires="n1">
                <w:r xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                    <w:t>choice content</w:t>
                </w:r>
            </mc:Choice>
            <mc:Fallback>
                <w:r xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                    <w:t>fallback content</w:t>
                </w:r>
            </mc:Fallback>
        </mc:AlternateContent>"#;
        let element = parse_with_whitespace(xml);
        let branch = select_mc_branch(&element).unwrap().unwrap();
        let local = local_element_name(branch);
        assert_eq!(
            local, "Choice",
            "the prefix n1 resolves to the understood WML main namespace, so the Choice wins"
        );
    }

    #[test]
    fn test_select_mc_branch_unbound_requires_prefix_errors() {
        // §7.6: an unbound Requires prefix is non-conformant — we cannot resolve it
        // to a namespace name, so selection fails loud rather than silently
        // treating it as unsatisfiable (no silent fallbacks).
        let xml = r#"<mc:AlternateContent xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006">
            <mc:Choice Requires="ghost">
                <w:r xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                    <w:t>content</w:t>
                </w:r>
            </mc:Choice>
            <mc:Fallback>
                <w:r xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                    <w:t>fallback content</w:t>
                </w:r>
            </mc:Fallback>
        </mc:AlternateContent>"#;
        let element = parse_with_whitespace(xml);
        let err = select_mc_branch(&element).expect_err("unbound Requires prefix must error");
        assert!(
            matches!(err, WordIrError::UnresolvableMcRequiresPrefix { ref prefix } if prefix == "ghost"),
            "expected UnresolvableMcRequiresPrefix for 'ghost', got {err:?}"
        );
    }

    #[test]
    fn test_select_mc_branch_no_requires_selects_choice() {
        // Choice without Requires attribute → always selectable
        let xml = r#"<mc:AlternateContent xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006">
            <mc:Choice>
                <w:drawing xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"/>
            </mc:Choice>
            <mc:Fallback>
                <w:pict xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"/>
            </mc:Fallback>
        </mc:AlternateContent>"#;
        let element = parse_with_whitespace(xml);
        let branch = select_mc_branch(&element).unwrap().unwrap();
        let local = local_element_name(branch);
        assert_eq!(
            local, "Choice",
            "Choice without Requires should always be selected"
        );
    }

    #[test]
    fn test_mc_inner_content_name_drawing() {
        let xml = r#"<mc:AlternateContent xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape">
            <mc:Choice Requires="wps">
                <w:drawing xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"/>
            </mc:Choice>
            <mc:Fallback>
                <w:pict xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"/>
            </mc:Fallback>
        </mc:AlternateContent>"#;
        let element = parse_with_whitespace(xml);
        let name = mc_inner_content_name(&element).unwrap();
        assert_eq!(
            name, "drawing",
            "should use inner element name from selected Choice branch"
        );
    }

    #[test]
    fn test_delinstrtext_produces_widget_atom() {
        let xml = r#"<w:r xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:delInstrText xml:space="preserve"> TOC \o "1-3" </w:delInstrText>
        </w:r>"#;
        let element = parse_with_whitespace(xml);
        let atoms = run_atoms(&element, 0).unwrap();
        assert_eq!(
            atoms.len(),
            1,
            "delInstrText should produce exactly one atom"
        );
        assert!(
            matches!(&atoms[0].kind, AtomKind::Widget { name, .. } if name.contains("delInstrText")),
            "expected Widget with delInstrText, got {:?}",
            atoms[0].kind
        );
        assert_eq!(atoms[0].utf16_len, 1, "widget should occupy 1 UTF-16 unit");
    }

    #[test]
    fn run_atoms_keep_contiguous_text_and_tabs_in_one_run_atom() {
        let xml = r#"<w:r xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:t>left</w:t><w:tab/><w:t xml:space="preserve"> right</w:t>
        </w:r>"#;
        let element = parse_with_whitespace(xml);
        let atoms = run_atoms(&element, 7).expect("parse run");

        assert_eq!(atoms.len(), 1, "one source run must remain one text atom");
        assert!(
            matches!(&atoms[0].kind, AtomKind::Text(text) if text == "left\t right"),
            "text and tab children must retain their order in one carrier: {:?}",
            atoms[0].kind
        );
        assert_eq!(atoms[0].utf16_len, 11);
        assert_eq!(atoms[0].origin.run_index, Some(7));
    }

    #[test]
    fn run_atoms_preserve_only_sorted_source_rsid_attrs() {
        let xml = r#"<w:r xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            w:rsidRPr="00BB" w:rsidR="00AA" w:custom="discard-me">
            <w:t>text</w:t>
        </w:r>"#;
        let element = parse_with_whitespace(xml);
        let atoms = run_atoms(&element, 0).expect("parse run");

        assert_eq!(
            atoms[0].source_run_attrs,
            vec![
                ("w:rsidR".to_string(), "00AA".to_string()),
                ("w:rsidRPr".to_string(), "00BB".to_string()),
            ],
            "editing-session provenance is retained deterministically, while unrelated attrs are rejected at the import edge"
        );
    }

    #[test]
    fn run_atoms_do_not_coalesce_across_a_run_decoration() {
        let xml = r#"<w:r xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:t>before</w:t><w:lastRenderedPageBreak/><w:t>after</w:t>
        </w:r>"#;
        let element = parse_with_whitespace(xml);
        let atoms = run_atoms(&element, 0).expect("parse run");

        assert_eq!(atoms.len(), 3, "the decoration remains an explicit barrier");
        assert!(matches!(&atoms[0].kind, AtomKind::Text(text) if text == "before"));
        assert!(matches!(&atoms[1].kind, AtomKind::Decoration { .. }));
        assert!(matches!(&atoms[2].kind, AtomKind::Text(text) if text == "after"));
    }

    #[test]
    fn test_ruby_produces_widget_atom() {
        let xml = r#"<w:r xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:ruby>
                <w:rubyPr>
                    <w:rubyAlign w:val="distributeSpace"/>
                </w:rubyPr>
                <w:rt>
                    <w:r><w:t>かん</w:t></w:r>
                </w:rt>
                <w:rubyBase>
                    <w:r><w:t>漢</w:t></w:r>
                </w:rubyBase>
            </w:ruby>
        </w:r>"#;
        let element = parse_with_whitespace(xml);
        let atoms = run_atoms(&element, 0).unwrap();
        assert_eq!(atoms.len(), 1, "ruby should produce exactly one atom");
        assert!(
            matches!(&atoms[0].kind, AtomKind::Widget { name, .. } if name.contains("ruby")),
            "expected Widget with ruby, got {:?}",
            atoms[0].kind
        );
        assert_eq!(atoms[0].utf16_len, 1, "widget should occupy 1 UTF-16 unit");
    }

    #[test]
    fn test_ptab_produces_widget_atom() {
        let xml = r#"<w:r xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:ptab w:alignment="right" w:relativeTo="margin" w:leader="none"/>
        </w:r>"#;
        let element = parse_with_whitespace(xml);
        let atoms = run_atoms(&element, 0).unwrap();
        assert_eq!(atoms.len(), 1, "ptab should produce exactly one atom");
        assert!(
            matches!(&atoms[0].kind, AtomKind::Widget { name, .. } if name.contains("ptab")),
            "expected Widget with ptab, got {:?}",
            atoms[0].kind
        );
        assert_eq!(atoms[0].utf16_len, 1, "widget should occupy 1 UTF-16 unit");
    }

    #[test]
    fn test_omath_paragraph_level_produces_widget() {
        let xml = r#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
                         xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math">
            <m:oMath>
                <m:r>
                    <w:rPr><w:rFonts w:ascii="Cambria Math"/></w:rPr>
                    <m:t>x+1</m:t>
                </m:r>
            </m:oMath>
        </w:p>"#;
        let element = parse_with_whitespace(xml);
        let view = ParagraphView::from_paragraph(&element, &Default::default()).unwrap();
        assert_eq!(
            view.atoms.len(),
            1,
            "paragraph-level oMath should produce exactly one atom"
        );
        assert!(
            matches!(&view.atoms[0].kind, AtomKind::Widget { name, .. } if name.contains("oMath")),
            "expected Widget with oMath, got {:?}",
            view.atoms[0].kind
        );
        assert_eq!(
            view.atoms[0].utf16_len, 1,
            "widget should occupy 1 UTF-16 unit"
        );
    }

    #[test]
    fn test_mc_alternate_content_run_level() {
        // A run-level AlternateContent whose Choice requires an understood
        // namespace (wps) RESOLVES to that Choice (§9.3): the widget is the
        // Choice's `w:drawing`, exactly as if the run held the drawing bare. The
        // non-selected Fallback (legacy VML) is dropped, and the wrapper is not
        // carried — an AC-wrapped drawing and a bare drawing are indistinguishable
        // in the model, and the SAME construct resolves the SAME way regardless of
        // where it sits (matching the tracked-container and paragraph-level paths).
        let xml = r#"<w:r xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
                         xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006"
                         xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape">
            <mc:AlternateContent>
                <mc:Choice Requires="wps">
                    <w:drawing>
                        <shape>modern shape</shape>
                    </w:drawing>
                </mc:Choice>
                <mc:Fallback>
                    <w:pict>
                        <shape>legacy shape</shape>
                    </w:pict>
                </mc:Fallback>
            </mc:AlternateContent>
        </w:r>"#;
        let element = parse_with_whitespace(xml);
        let atoms = run_atoms(&element, 0).unwrap();
        assert_eq!(
            atoms.len(),
            1,
            "the resolved Choice drawing should produce exactly one atom"
        );
        match &atoms[0].kind {
            AtomKind::Widget { name, raw_xml } => {
                assert_eq!(
                    name.split(':').next_back().unwrap_or(name),
                    "drawing",
                    "widget should be the Choice's drawing"
                );
                let raw_str = String::from_utf8_lossy(raw_xml);
                assert!(
                    raw_str.contains("modern shape"),
                    "raw_xml should be the resolved Choice content"
                );
                assert!(
                    !raw_str.contains("AlternateContent"),
                    "the AC wrapper must NOT survive resolution"
                );
                assert!(
                    !raw_str.contains("legacy shape"),
                    "the non-selected Fallback must be dropped"
                );
            }
            other => panic!("expected Widget, got {:?}", other),
        }
    }

    #[test]
    fn test_mc_alternate_content_run_level_falls_back_to_text() {
        // A run-level AlternateContent whose only Choice requires a namespace we
        // do NOT understand (w16se) resolves to its Fallback (§9.3). Here the
        // Fallback is plain text, so the construct becomes ordinary run text
        // carrying the run's formatting — NOT an opaque widget named after a
        // descendant tag. This is the shape that previously produced a widget
        // mislabeled `unknown:t` (named after the Fallback's `w:t`).
        let xml = r#"<w:r xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
                         xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006"
                         xmlns:w16se="http://schemas.microsoft.com/office/word/2015/wordml/symex">
            <w:rPr><w:b/></w:rPr>
            <mc:AlternateContent>
                <mc:Choice Requires="w16se">
                    <w16se:symEx w16se:font="Segoe UI Emoji" w16se:char="1F4CD"/>
                </mc:Choice>
                <mc:Fallback>
                    <w:t>P</w:t>
                </mc:Fallback>
            </mc:AlternateContent>
        </w:r>"#;
        let element = parse_with_whitespace(xml);
        let atoms = run_atoms(&element, 0).unwrap();
        assert_eq!(atoms.len(), 1, "the resolved Fallback text is one atom");
        match &atoms[0].kind {
            AtomKind::Text(text) => assert_eq!(text, "P", "resolves to the Fallback text"),
            other => panic!("expected Text, got {:?}", other),
        }
    }

    #[test]
    fn test_mc_paragraph_level_unknown_element_errors() {
        // An MC:Choice branch at paragraph level containing an unknown element
        // should produce UnknownParagraphElement, not silently drop it.
        let xml = r#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
                         xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006"
                         xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape">
            <mc:AlternateContent>
                <mc:Choice Requires="wps">
                    <w:r><w:t>text</w:t></w:r>
                    <w:bogusElement/>
                </mc:Choice>
                <mc:Fallback>
                    <w:r><w:t>fallback</w:t></w:r>
                </mc:Fallback>
            </mc:AlternateContent>
        </w:p>"#;
        let element = parse_with_whitespace(xml);
        let result = ParagraphView::from_paragraph(&element, &Default::default());
        assert!(result.is_err(), "unknown element in MC branch should error");
        let err = result.unwrap_err();
        assert!(
            matches!(&err, WordIrError::UnknownParagraphElement(name) if name.contains("bogusElement")),
            "expected UnknownParagraphElement for bogusElement, got: {err}"
        );
    }

    #[test]
    fn test_mc_paragraph_level_known_elements_ok() {
        // MC:Choice branch containing known paragraph-level elements (run, bookmark)
        // should parse successfully without error.
        let xml = r#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
                         xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006"
                         xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape">
            <mc:AlternateContent>
                <mc:Choice Requires="wps">
                    <w:r><w:t>hello</w:t></w:r>
                    <w:bookmarkStart w:id="1" w:name="test"/>
                    <w:bookmarkEnd w:id="1"/>
                </mc:Choice>
                <mc:Fallback>
                    <w:r><w:t>fallback</w:t></w:r>
                </mc:Fallback>
            </mc:AlternateContent>
        </w:p>"#;
        let element = parse_with_whitespace(xml);
        let result = ParagraphView::from_paragraph(&element, &Default::default());
        assert!(
            result.is_ok(),
            "known elements in MC branch should succeed: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_mc_paragraph_level_hyperlink_ok() {
        // MC:Choice branch containing a hyperlink should parse successfully.
        let xml = r#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
                         xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006"
                         xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape">
            <mc:AlternateContent>
                <mc:Choice Requires="wps">
                    <w:hyperlink w:anchor="top">
                        <w:r><w:t>link text</w:t></w:r>
                    </w:hyperlink>
                </mc:Choice>
                <mc:Fallback>
                    <w:r><w:t>fallback</w:t></w:r>
                </mc:Fallback>
            </mc:AlternateContent>
        </w:p>"#;
        let element = parse_with_whitespace(xml);
        let result = ParagraphView::from_paragraph(&element, &Default::default());
        assert!(
            result.is_ok(),
            "hyperlink in MC branch should succeed: {:?}",
            result.err()
        );
        let view = result.unwrap();
        assert!(
            view.atoms
                .iter()
                .any(|a| matches!(&a.kind, AtomKind::Hyperlink(_))),
            "should produce a Hyperlink atom from MC branch"
        );
    }

    #[test]
    fn test_synthesize_no_tabs_is_noop() {
        use crate::domain::TabAlignment;
        let explicit = vec![TabStopDef {
            position: 1440,
            alignment: TabAlignment::Left,
            leader: None,
        }];
        let result = synthesize_default_tab_stops(&explicit, 0, 720, 0);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].position, 1440);
    }

    #[test]
    fn test_synthesize_sufficient_explicit_stops() {
        use crate::domain::TabAlignment;
        let explicit = vec![
            TabStopDef {
                position: 720,
                alignment: TabAlignment::Left,
                leader: None,
            },
            TabStopDef {
                position: 1440,
                alignment: TabAlignment::Center,
                leader: None,
            },
        ];
        // 2 tabs, 2 explicit stops — no synthesis needed
        let result = synthesize_default_tab_stops(&explicit, 2, 720, 0);
        assert_eq!(result.len(), 2);
        assert_eq!(result[1].alignment, TabAlignment::Center);
    }

    #[test]
    fn test_synthesize_all_from_zero_indent() {
        use crate::domain::TabAlignment;
        // No explicit stops, 3 tabs, default interval 720, no indent
        let result = synthesize_default_tab_stops(&[], 3, 720, 0);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].position, 720);
        assert_eq!(result[1].position, 1440);
        assert_eq!(result[2].position, 2160);
        for stop in &result {
            assert_eq!(stop.alignment, TabAlignment::Left);
            assert_eq!(stop.leader, None);
        }
    }

    #[test]
    fn test_synthesize_fills_remaining() {
        use crate::domain::{TabAlignment, TabLeader};
        // 1 explicit stop at 720, 3 tabs needed, no indent
        let explicit = vec![TabStopDef {
            position: 720,
            alignment: TabAlignment::Right,
            leader: Some(TabLeader::Dot),
        }];
        let result = synthesize_default_tab_stops(&explicit, 3, 720, 0);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].position, 720);
        assert_eq!(result[0].alignment, TabAlignment::Right);
        assert_eq!(result[0].leader, Some(TabLeader::Dot));
        assert_eq!(result[1].position, 1440);
        assert_eq!(result[2].position, 2160);
    }

    #[test]
    fn test_synthesize_aligns_to_interval_grid() {
        use crate::domain::TabAlignment;
        // Explicit stop at 1000 (not on 720 grid), interval 720, no indent
        // Next grid position after 1000: ((1000 / 720) + 1) * 720 = 2 * 720 = 1440
        let explicit = vec![TabStopDef {
            position: 1000,
            alignment: TabAlignment::Left,
            leader: None,
        }];
        let result = synthesize_default_tab_stops(&explicit, 3, 720, 0);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].position, 1000);
        assert_eq!(result[1].position, 1440);
        assert_eq!(result[2].position, 2160);
    }

    #[test]
    fn test_synthesize_skips_stops_before_left_indent() {
        // Paragraph indented at 4320 twips (216pt), 3 tabs, interval 720.
        // Stops at 720..4320 are at or before the indent — skip them.
        // First useful stop: ((4320 / 720) + 1) * 720 = 7 * 720 = 5040.
        let result = synthesize_default_tab_stops(&[], 3, 720, 4320);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].position, 5040);
        assert_eq!(result[1].position, 5760);
        assert_eq!(result[2].position, 6480);
    }

    #[test]
    fn test_synthesize_explicit_stop_past_indent_preserved() {
        use crate::domain::TabAlignment;
        // Explicit stop at 5000 (past indent of 4320), 2 tabs needed.
        // Explicit stop covers tab 1. Synthesized stop for tab 2: next grid after 5000.
        let explicit = vec![TabStopDef {
            position: 5000,
            alignment: TabAlignment::Center,
            leader: None,
        }];
        let result = synthesize_default_tab_stops(&explicit, 2, 720, 4320);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].position, 5000);
        assert_eq!(result[0].alignment, TabAlignment::Center);
        // ((5000 / 720) + 1) * 720 = (6 + 1) * 720 = 5040
        assert_eq!(result[1].position, 5040);
    }

    // --- Spacing extraction tests ---

    #[test]
    fn extract_spacing_full() {
        let xml = r#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:pPr>
                <w:spacing w:before="120" w:after="240" w:line="360" w:lineRule="auto"/>
            </w:pPr>
            <w:r><w:t>text</w:t></w:r>
        </w:p>"#;
        let el = parse_with_whitespace(xml);
        let view = ParagraphView::from_paragraph(&el, &Default::default()).unwrap();
        let sp = view.spacing.expect("spacing should be present");
        assert_eq!(sp.before, Some(120));
        assert_eq!(sp.after, Some(240));
        assert_eq!(sp.line, Some(360));
        assert_eq!(sp.line_rule.as_deref(), Some("auto"));
    }

    #[test]
    fn extract_spacing_partial_before_only() {
        let xml = r#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:pPr>
                <w:spacing w:before="200"/>
            </w:pPr>
            <w:r><w:t>text</w:t></w:r>
        </w:p>"#;
        let el = parse_with_whitespace(xml);
        let view = ParagraphView::from_paragraph(&el, &Default::default()).unwrap();
        let sp = view.spacing.expect("spacing should be present");
        assert_eq!(sp.before, Some(200));
        assert_eq!(sp.after, None);
        assert_eq!(sp.line, None);
        assert_eq!(sp.line_rule, None);
    }

    #[test]
    fn extract_spacing_exact_line_rule() {
        let xml = r#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:pPr>
                <w:spacing w:line="240" w:lineRule="exact"/>
            </w:pPr>
            <w:r><w:t>text</w:t></w:r>
        </w:p>"#;
        let el = parse_with_whitespace(xml);
        let view = ParagraphView::from_paragraph(&el, &Default::default()).unwrap();
        let sp = view.spacing.expect("spacing should be present");
        assert_eq!(sp.line, Some(240));
        assert_eq!(sp.line_rule.as_deref(), Some("exact"));
    }

    #[test]
    fn extract_spacing_absent_returns_none() {
        let xml = r#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:pPr>
                <w:jc w:val="center"/>
            </w:pPr>
            <w:r><w:t>text</w:t></w:r>
        </w:p>"#;
        let el = parse_with_whitespace(xml);
        let view = ParagraphView::from_paragraph(&el, &Default::default()).unwrap();
        assert!(view.spacing.is_none());
    }

    // --- pPr preserved-remainder tests ---

    /// Drift guard for `MODELED_PPR_CHILDREN`: a pPr containing every element
    /// name on that list must produce an EMPTY preserved remainder. If a
    /// name is added to the extract_* helpers above without being added to
    /// the const (or vice versa), this test catches the mismatch — the
    /// unknown-child scan would otherwise silently start (or stop) capturing
    /// a modeled element.
    #[test]
    fn ppr_walk_covers_every_modeled_child() {
        let xml = r#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:pPr>
                <w:pStyle w:val="Normal"/>
                <w:keepNext/>
                <w:keepLines/>
                <w:pageBreakBefore/>
                <w:framePr w:w="1000"/>
                <w:widowControl/>
                <w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr>
                <w:pBdr><w:top w:val="single" w:sz="4" w:space="1" w:color="auto"/></w:pBdr>
                <w:shd w:val="clear" w:fill="FFFFFF"/>
                <w:tabs><w:tab w:val="left" w:pos="720"/></w:tabs>
                <w:suppressAutoHyphens/>
                <w:wordWrap/>
                <w:overflowPunct/>
                <w:autoSpaceDE/>
                <w:autoSpaceDN/>
                <w:bidi/>
                <w:adjustRightInd/>
                <w:snapToGrid/>
                <w:spacing w:before="120"/>
                <w:ind w:left="720"/>
                <w:contextualSpacing/>
                <w:mirrorIndents/>
                <w:jc w:val="center"/>
                <w:textDirection w:val="lrTb"/>
                <w:textAlignment w:val="auto"/>
                <w:outlineLvl w:val="0"/>
                <w:cnfStyle w:val="100000000000"/>
                <w:rPr><w:b/></w:rPr>
                <w:sectPr/>
                <w:pPrChange w:id="1" w:author="Test"><w:pPr/></w:pPrChange>
            </w:pPr>
            <w:r><w:t>text</w:t></w:r>
        </w:p>"#;
        let el = parse_with_whitespace(xml);
        let view = ParagraphView::from_paragraph(&el, &Default::default()).unwrap();
        assert!(
            view.preserved.is_empty(),
            "every child here is on MODELED_PPR_CHILDREN; the remainder must be empty, got: {:?}",
            view.preserved
        );
    }

    /// A pPr child this parser doesn't model (`w:kinsoku`, Annex A but no
    /// extract_* helper) and a foreign-namespace extension are both captured
    /// verbatim as preserved remainder, alongside normal parsing of a
    /// sibling modeled child.
    #[test]
    fn ppr_walk_captures_unmodeled_child_as_preserved() {
        let xml = r#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
                         xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml">
            <w:pPr>
                <w:keepNext/>
                <w:kinsoku w:val="0"/>
                <w14:customPPr w14:val="1"/>
            </w:pPr>
            <w:r><w:t>text</w:t></w:r>
        </w:p>"#;
        let el = parse_with_whitespace(xml);
        let view = ParagraphView::from_paragraph(&el, &Default::default()).unwrap();
        assert_eq!(
            view.keep_next,
            Some(true),
            "sibling modeled child still parses"
        );
        assert_eq!(
            view.preserved.len(),
            2,
            "both the unmodeled w:kinsoku and the foreign w14:customPPr must be captured: {:?}",
            view.preserved
        );
        assert!(
            view.preserved.iter().any(|p| p.name == "w:kinsoku"),
            "expected w:kinsoku in preserved: {:?}",
            view.preserved
        );
        assert!(
            view.preserved.iter().any(|p| p.name == "w14:customPPr"),
            "expected w14:customPPr in preserved: {:?}",
            view.preserved
        );
    }

    #[test]
    fn extract_run_color_auto_preserved_as_explicit_value() {
        let xml = r#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:r>
                <w:rPr><w:color w:val="auto"/></w:rPr>
                <w:t>text</w:t>
            </w:r>
        </w:p>"#;
        let el = parse_with_whitespace(xml);
        let view = ParagraphView::from_paragraph(&el, &Default::default()).unwrap();
        let atom = view
            .atoms
            .iter()
            .find(|atom| matches!(atom.kind, AtomKind::Text(_)))
            .expect("text atom should exist");

        assert_eq!(
            atom.marks.color.as_deref(),
            Some("auto"),
            "direct w:color w:val=\"auto\" is an explicit run property and must survive import",
        );
    }

    // --- Paragraph border extraction tests ---

    #[test]
    fn extract_borders_top_and_bottom() {
        let xml = r#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:pPr>
                <w:pBdr>
                    <w:top w:val="single" w:color="FF0000" w:sz="4"/>
                    <w:bottom w:val="double" w:sz="8"/>
                </w:pBdr>
            </w:pPr>
            <w:r><w:t>text</w:t></w:r>
        </w:p>"#;
        let el = parse_with_whitespace(xml);
        let view = ParagraphView::from_paragraph(&el, &Default::default()).unwrap();
        let bdr = view.borders.expect("borders should be present");

        let top = bdr.top.expect("top border should be present");
        assert_eq!(top.style, "single");
        assert_eq!(top.color.as_deref(), Some("FF0000"));
        assert_eq!(top.size, Some(4));

        let bottom = bdr.bottom.expect("bottom border should be present");
        assert_eq!(bottom.style, "double");
        assert_eq!(bottom.color, None);
        assert_eq!(bottom.size, Some(8));

        assert!(bdr.left.is_none());
        assert!(bdr.right.is_none());
        assert!(bdr.between.is_none());
        assert!(bdr.bar.is_none());
    }

    #[test]
    fn extract_borders_start_end_aliases() {
        // w:start and w:end are aliases for w:left and w:right in newer OOXML.
        let xml = r#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:pPr>
                <w:pBdr>
                    <w:start w:val="single" w:sz="4"/>
                    <w:end w:val="single" w:sz="4"/>
                </w:pBdr>
            </w:pPr>
            <w:r><w:t>text</w:t></w:r>
        </w:p>"#;
        let el = parse_with_whitespace(xml);
        let view = ParagraphView::from_paragraph(&el, &Default::default()).unwrap();
        let bdr = view.borders.expect("borders should be present");

        // w:start maps to left, w:end maps to right
        assert!(bdr.left.is_some(), "start should map to left");
        assert!(bdr.right.is_some(), "end should map to right");
    }

    #[test]
    fn extract_borders_none_style_still_parses() {
        // A border with val="none" is still a valid border definition (explicitly no border).
        let xml = r#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:pPr>
                <w:pBdr>
                    <w:top w:val="none"/>
                </w:pBdr>
            </w:pPr>
            <w:r><w:t>text</w:t></w:r>
        </w:p>"#;
        let el = parse_with_whitespace(xml);
        let view = ParagraphView::from_paragraph(&el, &Default::default()).unwrap();
        let bdr = view.borders.expect("borders should be present");
        let top = bdr
            .top
            .expect("top border should be present even with none style");
        assert_eq!(top.style, "none");
    }

    #[test]
    fn extract_borders_absent_returns_none() {
        let xml = r#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:pPr>
                <w:jc w:val="center"/>
            </w:pPr>
            <w:r><w:t>text</w:t></w:r>
        </w:p>"#;
        let el = parse_with_whitespace(xml);
        let view = ParagraphView::from_paragraph(&el, &Default::default()).unwrap();
        assert!(view.borders.is_none());
    }

    // --- sectPr extraction tests ---

    #[test]
    fn extract_sect_pr_populates_typed_section_properties() {
        let xml = r#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:pPr>
                <w:sectPr>
                    <w:pgSz w:w="12240" w:h="15840"/>
                    <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
                </w:sectPr>
            </w:pPr>
            <w:r><w:t>text</w:t></w:r>
        </w:p>"#;
        let el = parse_with_whitespace(xml);
        let view = ParagraphView::from_paragraph(&el, &Default::default()).unwrap();
        let sp = view.section_properties.expect("sectPr should be extracted");

        assert_eq!(sp.page_width, Some(12240));
        assert_eq!(sp.page_height, Some(15840));
        assert_eq!(sp.margin_top, Some(1440));
        assert_eq!(sp.margin_right, Some(1440));
        assert_eq!(sp.margin_bottom, Some(1440));
        assert_eq!(sp.margin_left, Some(1440));
    }

    #[test]
    fn extract_sect_pr_absent_returns_none() {
        let xml = r#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:pPr>
                <w:jc w:val="center"/>
            </w:pPr>
            <w:r><w:t>text</w:t></w:r>
        </w:p>"#;
        let el = parse_with_whitespace(xml);
        let view = ParagraphView::from_paragraph(&el, &Default::default()).unwrap();
        assert!(view.section_properties.is_none());
    }

    /// MS-OI29500 §17.18.84: "start" and "end" are bidi-aware aliases for
    /// "left" and "right". Our parser normalizes them so downstream code only
    /// needs to handle the canonical forms.
    #[test]
    fn tab_stop_start_end_aliases() {
        let xml = r#"<w:pPr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:tabs>
                <w:tab w:val="start" w:pos="720"/>
                <w:tab w:val="end"   w:pos="9360"/>
                <w:tab w:val="left"  w:pos="1440"/>
                <w:tab w:val="right" w:pos="7200"/>
            </w:tabs>
        </w:pPr>"#;
        let el = parse_with_whitespace(xml);
        let stops = extract_tab_stops(&el).expect("should parse tab stops");
        assert_eq!(stops.len(), 4);
        assert_eq!(
            stops[0].alignment,
            crate::domain::TabAlignment::Left,
            "start should normalize to left"
        );
        assert_eq!(
            stops[1].alignment,
            crate::domain::TabAlignment::Right,
            "end should normalize to right"
        );
        assert_eq!(
            stops[2].alignment,
            crate::domain::TabAlignment::Left,
            "left stays left"
        );
        assert_eq!(
            stops[3].alignment,
            crate::domain::TabAlignment::Right,
            "right stays right"
        );
    }

    /// Verify that `serialize_element` produces self-contained fragments:
    /// the root element declares exactly the namespace prefixes used within
    /// the subtree — no more, no less. These fragments can then be re-parsed
    /// via `parse_raw_fragment`.
    #[test]
    fn test_serialize_element_declares_used_namespace_prefixes() {
        use crate::word_xml::parse_raw_fragment;

        // Simulate a document with w16du declared at the root, and an oMath child
        // that uses w16du:dateUtc on a tracked change element.
        // Also declare xmlns:a (unused in this subtree) to verify it is NOT emitted.
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
                                 xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math"
                                 xmlns:w16du="http://schemas.microsoft.com/office/word/2023/wordml/word16du"
                                 xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
            <w:body>
                <m:oMath>
                    <w:ins w:id="1" w:author="test" w:date="2024-01-01T00:00:00Z" w16du:dateUtc="2024-01-01T00:00:00Z">
                        <m:r><m:t>x</m:t></m:r>
                    </w:ins>
                </m:oMath>
            </w:body>
        </w:document>"#;

        let doc = parse_with_whitespace(xml);
        let body = doc
            .children
            .iter()
            .find_map(|c| {
                if let XMLNode::Element(e) = c {
                    Some(e)
                } else {
                    None
                }
            })
            .expect("should have body");
        let omath = body
            .children
            .iter()
            .find_map(|c| {
                if let XMLNode::Element(e) = c {
                    Some(e)
                } else {
                    None
                }
            })
            .expect("should have oMath");

        let raw = serialize_element(omath);
        let raw_str = String::from_utf8_lossy(&raw);

        // Used prefixes MUST have xmlns declarations.
        assert!(
            raw_str.contains("xmlns:m="),
            "used element prefix 'm' must be declared: {raw_str}"
        );
        assert!(
            raw_str.contains("xmlns:w="),
            "used element prefix 'w' must be declared: {raw_str}"
        );
        assert!(
            raw_str.contains("xmlns:w16du="),
            "used attribute prefix 'w16du' must be declared: {raw_str}"
        );

        // Unused prefixes must NOT be declared.
        assert!(
            !raw_str.contains("xmlns:a="),
            "unused prefix 'a' must not be declared: {raw_str}"
        );

        // Prefixed element/attribute names must still be present.
        assert!(
            raw_str.contains("m:oMath"),
            "element prefix preserved: {raw_str}"
        );
        assert!(
            raw_str.contains("w16du:dateUtc"),
            "attribute prefix preserved: {raw_str}"
        );

        // parse_raw_fragment must successfully re-parse the self-contained bytes.
        let el = parse_raw_fragment(&raw).expect("parse_raw_fragment should succeed");
        assert_eq!(el.name, "oMath");
        assert_eq!(el.prefix.as_deref(), Some("m"));
        // parse_raw_fragment keeps declarations for used prefixes so the element
        // is self-contained when embedded in a document.
        assert!(
            el.namespaces.is_some(),
            "used namespace declarations should be preserved"
        );
    }

    /// Verify that a prefix NOT in `KNOWN_OOXML_NAMESPACES` roundtrips through
    /// `serialize_element` -> `parse_raw_fragment`.  Before this fix, unknown
    /// prefixes would fail to re-parse because they had no xmlns declaration.
    #[test]
    fn test_unknown_prefix_roundtrips_through_serialize_and_parse() {
        use crate::word_xml::parse_raw_fragment;

        // "adec" is not in KNOWN_OOXML_NAMESPACES — this is the exact scenario
        // that triggered 19 stress test failures.
        let xml = r#"<w:drawing xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
                               xmlns:adec="http://schemas.microsoft.com/office/drawing/2017/decorative">
            <adec:decorative adec:val="1"/>
        </w:drawing>"#;

        let doc = parse_with_whitespace(xml);
        let raw = serialize_element(&doc);
        let raw_str = String::from_utf8_lossy(&raw);

        // The unknown prefix must be declared at the root.
        assert!(
            raw_str.contains("xmlns:adec="),
            "unknown prefix 'adec' must be declared in serialized bytes: {raw_str}"
        );

        // Must roundtrip successfully through parse_raw_fragment.
        let el = parse_raw_fragment(&raw).expect(
            "parse_raw_fragment must handle unknown prefixes when they are declared in the raw bytes"
        );
        assert_eq!(el.name, "drawing");
        assert_eq!(el.prefix.as_deref(), Some("w"));

        // The child element with the unknown prefix must survive.
        let child = el
            .children
            .iter()
            .find_map(|c| {
                if let XMLNode::Element(e) = c {
                    Some(e)
                } else {
                    None
                }
            })
            .expect("should have adec:decorative child");
        assert_eq!(child.name, "decorative");
        assert_eq!(child.prefix.as_deref(), Some("adec"));
    }

    #[test]
    fn parse_cnf_style_from_ppr() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
            <w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                <w:pPr>
                    <w:cnfStyle w:val="100000000000" w:firstRow="1" w:lastRow="0"
                        w:firstColumn="0" w:lastColumn="0"
                        w:oddVBand="0" w:evenVBand="0"
                        w:oddHBand="0" w:evenHBand="0"
                        w:firstRowFirstColumn="0" w:firstRowLastColumn="0"
                        w:lastRowFirstColumn="0" w:lastRowLastColumn="0"/>
                </w:pPr>
                <w:r><w:t>Test</w:t></w:r>
            </w:p>"#;
        let el = Element::parse(xml.as_bytes()).unwrap();
        let view = ParagraphView::from_paragraph(&el, &Default::default()).unwrap();

        let cnf = view.cnf_style.expect("cnfStyle should be parsed");
        assert_eq!(cnf.val.as_deref(), Some("100000000000"));
        assert!(cnf.first_row);
        assert!(!cnf.last_row);
        assert!(!cnf.first_column);
    }

    #[test]
    fn parse_note_properties_from_sect_pr() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
            <w:sectPr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                <w:footnotePr>
                    <w:pos w:val="beneathText"/>
                    <w:numFmt w:val="lowerRoman"/>
                    <w:numStart w:val="5"/>
                    <w:numRestart w:val="eachSect"/>
                </w:footnotePr>
                <w:endnotePr>
                    <w:numFmt w:val="upperRoman"/>
                </w:endnotePr>
                <w:pgSz w:w="12240" w:h="15840"/>
            </w:sectPr>"#;
        let el = Element::parse(xml.as_bytes()).unwrap();
        let sp = super::parse_section_properties(&el, &Default::default());

        let fp = sp.footnote_pr.expect("footnote_pr should be parsed");
        assert_eq!(fp.position, Some(crate::domain::NotePosition::BeneathText));
        assert_eq!(fp.num_fmt, Some(crate::domain::NumberFormat::LowerRoman));
        assert_eq!(fp.num_start, Some(5));
        assert_eq!(fp.num_restart, Some(crate::domain::RestartRule::EachSect));

        let ep = sp.endnote_pr.expect("endnote_pr should be parsed");
        assert_eq!(ep.num_fmt, Some(crate::domain::NumberFormat::UpperRoman));
        assert_eq!(ep.position, None);
        assert_eq!(ep.num_start, None);
    }

    // ── hyperlink run extraction ──────────────────────────────────────────

    /// A hyperlink with two runs — one bold, one plain — should produce two
    /// HyperlinkRun entries. The bold run must carry a non-empty rpr_xml.
    /// Domain rule: per-run formatting must be preserved across the
    /// extract → HyperlinkData boundary so that serialization can restore it.
    #[test]
    fn test_extract_hyperlink_data_two_runs_with_rpr() {
        let xml = r#"<w:hyperlink xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" w:anchor="target">
            <w:r w:rsidR="00112233">
                <w:rPr><w:b/></w:rPr>
                <w:t>bold part</w:t>
            </w:r>
            <w:r>
                <w:t xml:space="preserve"> and normal part</w:t>
            </w:r>
        </w:hyperlink>"#;

        let element = parse_with_whitespace(xml);
        let data = extract_hyperlink_data(&element);

        assert_eq!(
            data.runs.len(),
            2,
            "should extract one HyperlinkRun per w:r"
        );
        assert_eq!(data.runs[0].text, "bold part");
        assert_eq!(
            data.runs[0].source_run_attrs,
            vec![("w:rsidR".to_string(), "00112233".to_string())]
        );
        assert!(
            data.runs[0].rpr_xml.is_some(),
            "first run (bold) must have rpr_xml"
        );
        let rpr_bytes = data.runs[0].rpr_xml.as_ref().unwrap();
        let rpr_str = String::from_utf8_lossy(rpr_bytes);
        assert!(
            rpr_str.contains("<w:b"),
            "rpr_xml must contain the <w:b> element; got: {rpr_str}"
        );

        assert_eq!(data.runs[1].text, " and normal part");
        assert!(
            data.runs[1].rpr_xml.is_none(),
            "second run (no rPr) must have rpr_xml = None"
        );

        // Backward-compat: text field is concatenation.
        assert_eq!(data.text, "bold part and normal part");
    }

    /// A hyperlink with no runs should produce an empty runs vec and empty text.
    #[test]
    fn test_extract_hyperlink_data_no_runs() {
        let xml = r#"<w:hyperlink xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" w:anchor="empty"/>"#;

        let element = parse_with_whitespace(xml);
        let data = extract_hyperlink_data(&element);

        assert!(data.runs.is_empty(), "no w:r children → empty runs");
        assert!(data.text.is_empty(), "no runs → empty text");
        assert_eq!(data.anchor, Some("empty".to_string()));
    }

    /// w:history and other extra attrs beyond r:id / w:anchor must be captured
    /// in extra_attrs so they can be round-tripped.
    #[test]
    fn test_extract_hyperlink_data_extra_attrs() {
        let xml = r#"<w:hyperlink xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            w:anchor="target"
            w:history="1"
            w:tgtFrame="_blank">
            <w:r><w:t>link</w:t></w:r>
        </w:hyperlink>"#;

        let element = parse_with_whitespace(xml);
        let data = extract_hyperlink_data(&element);

        // history and tgtFrame must appear in extra_attrs; anchor must not.
        let extra_keys: Vec<&str> = data.extra_attrs.iter().map(|(k, _)| k.as_str()).collect();
        assert!(
            extra_keys.contains(&"w:history"),
            "w:history must be in extra_attrs; got: {extra_keys:?}"
        );
        assert!(
            extra_keys.contains(&"w:tgtFrame"),
            "w:tgtFrame must be in extra_attrs; got: {extra_keys:?}"
        );
        assert!(
            !extra_keys.contains(&"w:anchor"),
            "w:anchor must NOT be in extra_attrs; got: {extra_keys:?}"
        );
    }

    /// Runs inside `<w:ins>` and `<w:del>` envelopes within a hyperlink are
    /// imported with the corresponding `Inserted` / `Deleted` status (with
    /// revision metadata captured from the envelope attributes), so the IR
    /// represents tracked-change edits to the hyperlink display text.
    #[test]
    fn hyperlink_runs_capture_ins_del_status_from_envelopes() {
        let xml = r#"<w:hyperlink xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
                                   xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"
                                   r:id="rId1">
            <w:r><w:t>before </w:t></w:r>
            <w:del w:id="42" w:author="Test" w:date="2026-05-19T10:00:00Z">
                <w:r><w:delText>old</w:delText></w:r>
            </w:del>
            <w:ins w:id="43" w:author="Test" w:date="2026-05-19T10:00:00Z">
                <w:r><w:t>new</w:t></w:r>
            </w:ins>
            <w:r><w:t> after</w:t></w:r>
        </w:hyperlink>"#;
        let element = parse_with_whitespace(xml);
        let data = extract_hyperlink_data(&element);

        assert_eq!(
            data.runs.len(),
            4,
            "expected four runs (Normal/Deleted/Inserted/Normal), got {}",
            data.runs.len()
        );
        assert!(matches!(data.runs[0].status, TrackingStatus::Normal));
        assert_eq!(data.runs[0].text, "before ");
        match &data.runs[1].status {
            TrackingStatus::Deleted(rev) => {
                assert_eq!(rev.revision_id, 42);
                assert_eq!(rev.author.as_deref(), Some("Test"));
            }
            other => panic!("expected Deleted, got {other:?}"),
        }
        assert_eq!(data.runs[1].text, "old");
        match &data.runs[2].status {
            TrackingStatus::Inserted(rev) => {
                assert_eq!(rev.revision_id, 43);
                assert_eq!(rev.author.as_deref(), Some("Test"));
            }
            other => panic!("expected Inserted, got {other:?}"),
        }
        assert_eq!(data.runs[2].text, "new");
        assert!(matches!(data.runs[3].status, TrackingStatus::Normal));
        assert_eq!(data.runs[3].text, " after");
    }

    /// When `<w:ins>` / `<w:del>` inside a hyperlink omit the `w:id`
    /// attribute (out-of-spec but defensive), the runs fall back to the
    /// ambient status (Normal) rather than crashing or silently losing
    /// the text.
    #[test]
    fn hyperlink_runs_with_missing_revision_id_fall_back_to_normal() {
        let xml = r#"<w:hyperlink xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:ins><w:r><w:t>untracked-ins</w:t></w:r></w:ins>
        </w:hyperlink>"#;
        let element = parse_with_whitespace(xml);
        let data = extract_hyperlink_data(&element);

        assert_eq!(data.runs.len(), 1);
        assert!(
            matches!(data.runs[0].status, TrackingStatus::Normal),
            "missing w:id must fall back to Normal, got {:?}",
            data.runs[0].status
        );
        assert_eq!(data.runs[0].text, "untracked-ins");
    }
}
