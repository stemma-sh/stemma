use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// Interned string type — `Arc<str>` is an atomically reference-counted, immutable
/// string slice. Cloning is O(1) (just increments the reference count), and it
/// shares one heap allocation across all clones. Used for frequently-duplicated
/// values like font names, colors, language tags, style IDs, and node IDs.
///
/// Uses `Arc` rather than `Rc` because `CanonDoc` (which contains these types)
/// must be `Send + Sync` for the server runtime and integration tests that
/// pass documents across thread boundaries.
pub type IStr = Arc<str>;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub struct NodeId(pub IStr);

impl NodeId {
    pub fn new(s: impl Into<IStr>) -> Self {
        NodeId(s.into())
    }
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for NodeId {
    fn from(s: String) -> Self {
        NodeId(IStr::from(s))
    }
}

impl From<&str> for NodeId {
    fn from(s: &str) -> Self {
        NodeId(IStr::from(s))
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaterializedPrefixKind {
    LiteralDeleted,
    LiteralInserted,
    Structural,
    StructuralInserted,
    StructuralDeleted,
}

impl MaterializedPrefixKind {
    pub fn suffix(self) -> &'static str {
        match self {
            Self::LiteralDeleted => "_pfx_del",
            Self::LiteralInserted => "_pfx_ins",
            Self::Structural => "_npfx",
            Self::StructuralInserted => "_npfx_ins",
            Self::StructuralDeleted => "_npfx_del",
        }
    }
}

pub fn materialized_prefix_node_id(paragraph_id: &NodeId, kind: MaterializedPrefixKind) -> NodeId {
    NodeId::from(format!("{}{}", paragraph_id.0, kind.suffix()))
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum TextRole {
    MaterializedPrefix(MaterializedPrefixKind),
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub struct DocHandle(pub String);

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub struct DocFingerprint(pub String);

pub const SCHEMA_VERSION_V0: &str = "0.1";
pub const INTERNAL_IDS_VERSION_V0: &str = "0.1";

/// Compatibility settings parsed from w:compat in settings.xml (MS-DOCX §2.3).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct CompatSettings {
    /// compatibilityMode value (MS-DOCX §2.3.5). E.g., 15 = Word 2013+.
    pub compatibility_mode: Option<u32>,
    /// overrideTableStyleFontSizeAndJustification (MS-DOCX §2.3.1).
    pub override_table_style_font_size_and_justification: Option<bool>,
    /// doNotFlipMirrorIndents (MS-DOCX §2.3.2).
    pub do_not_flip_mirror_indents: Option<bool>,
    /// enableOpenTypeFeatures (MS-DOCX §2.3.3).
    pub enable_open_type_features: Option<bool>,
    /// differentiateMultirowTableHeaders (MS-DOCX §2.3.4).
    pub differentiate_multirow_table_headers: Option<bool>,
    /// allowTextAfterFloatingTableBreak (MS-DOCX §2.3.6).
    pub allow_text_after_floating_table_break: Option<bool>,
}

/// The editing restriction a document declares via `w:documentProtection/@w:edit`
/// (`ST_DocProtect`, ISO/IEC 29500-1 §17.18.31). A closed set: an out-of-enum
/// value is a hard parse error, never coerced to a catch-all.
///
/// The engine records the declared mode; it does NOT enforce it. Word applies
/// these restrictions in its editor; stemma edits the underlying model
/// regardless (see [`DocumentProtection`]).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocProtectEdit {
    /// `none` — no editing is restricted.
    None,
    /// `readOnly` — the whole document is read-only.
    ReadOnly,
    /// `comments` — only comments may be added.
    Comments,
    /// `trackedChanges` — edits are permitted but forced to be tracked.
    TrackedChanges,
    /// `forms` — only form fields may be filled in.
    Forms,
}

/// `w:documentProtection` from `word/settings.xml` (`CT_DocProtect`,
/// ISO/IEC 29500-1 §17.15.1.29): a document-level declaration that Word should
/// restrict editing. The engine reports this fact honestly and leaves policy to
/// the host — it is NOT an import refusal, and engine edits do NOT honor it.
///
/// `None` on [`CanonDoc::document_protection`] means the element is absent (the
/// document declared no protection — distinct from a present element whose
/// `enforcement` is off). Every field here mirrors an attribute Word actually
/// writes; there is no silent default.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DocumentProtection {
    /// `w:edit` — the restriction mode. `None` when the attribute is absent (the
    /// element declares protection without naming an edit mode); `Some` carries
    /// the parsed [`DocProtectEdit`]. Absent-vs-present is kept honestly.
    pub edit: Option<DocProtectEdit>,

    /// `w:enforcement` (`ST_OnOff`) — whether the declared restriction is
    /// actually enforced. Three-state: `None` = attribute absent, `Some(true)` =
    /// enforced, `Some(false)` = present but explicitly off. A protection
    /// element with enforcement off is a declared-but-inert restriction.
    pub enforcement: Option<bool>,

    /// Whether the protection carries a password credential — `w:hash`/`w:salt`
    /// (legacy) or `w:hashValue`/`w:saltValue` (agile, ISO). We record ONLY
    /// presence; the credential material itself is never parsed, stored, or
    /// interpreted (the engine cannot and must not evaluate the password).
    pub has_credential: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CanonDoc {
    pub id: NodeId,
    pub blocks: Vec<TrackedBlock>,
    pub meta: DocMeta,

    // Story collections
    pub headers: Vec<HeaderStory>,
    pub footers: Vec<FooterStory>,
    pub footnotes: Vec<FootnoteStory>,
    pub endnotes: Vec<EndnoteStory>,
    pub comments: Vec<CommentStory>,

    /// `w15:commentEx` records from word/commentsExtended.xml (MS-DOCX §2.5.1):
    /// comment reply-threading + resolved state, keyed by the `w14:paraId` of a
    /// comment's first body paragraph. `#[serde(default)]` keeps deserialization
    /// of pre-existing snapshots (serialized before this field existed) stable.
    #[serde(default)]
    pub comments_extended: Vec<CommentExtended>,

    /// Parsed section properties from w:body/w:sectPr (the final/document-level section).
    pub body_section_properties: Option<SectionProperties>,

    /// Tracked change for body-level section properties (w:body/w:sectPr/w:sectPrChange).
    /// Present when a reviewer changed page layout for the final document section.
    pub body_section_property_change: Option<SectionPropertyChange>,

    /// Compatibility settings from w:compat in settings.xml (MS-DOCX §2.3).
    pub compat_settings: CompatSettings,

    /// `w:evenAndOddHeaders` from word/settings.xml (ISO 29500-1 §17.15.1.35).
    ///
    /// This is a `CT_OnOff` toggle whose *presence* (with no explicit `w:val`,
    /// or `w:val="1"`) means "use distinct even-page headers/footers". We carry
    /// three states honestly — `None` = element absent (NOT the same as off),
    /// `Some(true)` = present and on, `Some(false)` = present with `w:val="0"`
    /// (explicitly off). The settings.xml writer round-trips this distinction;
    /// there is no silent collapse of absent into off.
    ///
    /// `#[serde(default)]` keeps deserialization of pre-existing snapshots
    /// (serialized before this field existed) stable: they decode as `None`.
    #[serde(default)]
    pub even_and_odd_headers: Option<bool>,

    /// `w:background` from `word/document.xml` (ISO 29500-1 §17.2.1, CT_Background).
    ///
    /// A direct child of `<w:document>`, ordered BEFORE `<w:body>`. It carries the
    /// document's display background (color / theme color) plus an optional VML
    /// drawing child. `None` = the element is absent (which is NOT the same as
    /// "white": absence means no background is declared at all). `#[serde(default)]`
    /// keeps pre-existing snapshots decoding as `None`.
    #[serde(default)]
    pub document_background: Option<DocumentBackground>,

    /// `w:documentProtection` from `word/settings.xml` (ISO/IEC 29500-1
    /// §17.15.1.29). `None` = the element is absent (no protection declared).
    ///
    /// This is a *reported* fact, not an enforced one: the engine surfaces what
    /// the document declares (and emits an import diagnostic when enforcement is
    /// on) so a host can decide policy, but engine edits do not honor it. The
    /// element round-trips verbatim through `word/settings.xml` (the settings
    /// part is preserved, not rewritten, unless a settings verb touches it).
    ///
    /// `#[serde(default)]` keeps pre-existing snapshots (serialized before this
    /// field existed) decoding as `None`.
    #[serde(default)]
    pub document_protection: Option<DocumentProtection>,
}

/// `w:background` (ISO 29500-1 §17.2.1, CT_Background) — the document-level
/// display background.
///
/// The four `w:*` attributes are carried as typed optional strings (verbatim
/// token values; the display semantics are Word's to apply). The optional VML
/// drawing child (`v:background`, rendered only when `settings.xml`
/// `w:displayBackgroundShape` is set) is carried as an opaque, already-serialized
/// XML fragment so it round-trips verbatim — we do NOT silently drop the subtree.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct DocumentBackground {
    /// `w:color` — RGB hex (e.g. `FFFFFF`) or `auto`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// `w:themeColor` — ST_ThemeColor token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme_color: Option<String>,
    /// `w:themeTint` — two-hex-digit tint applied to the theme color.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme_tint: Option<String>,
    /// `w:themeShade` — two-hex-digit shade applied to the theme color.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme_shade: Option<String>,
    /// Serialized XML of the element's child nodes (the optional VML drawing),
    /// preserved verbatim. Empty when the background has no children.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drawing_xml: Vec<String>,
}

// Story-part domain types (HeaderStory, FooterStory, FootnoteStory,
// EndnoteStory, CommentStory, HeaderFooterKind, NoteType, StoryScope) were
// carved out into `domain/story.rs`. The glob re-export keeps every existing
// `crate::domain::FootnoteStory` (etc.) import path resolving unchanged.
mod story;
pub use story::*;

/// Reference from a section to a header or footer part (§17.10.4 / §17.10.5).
///
/// `part_path` is the relationship target (e.g. "header3.xml") — a
/// document-independent identifier resolved at import time from the raw rId.
/// rIds are serialization-layer concepts that are re-assigned at output time.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct StoryRef {
    pub kind: HeaderFooterKind,
    pub part_path: String,
    /// Parse-time provenance: this ref was INHERITED from a previous section
    /// (§17.10.2 resolution) or synthesized (§17.10.5 blank first-section
    /// header), not authored by this sectPr. Inherited refs must not be
    /// materialized as direct headerReference/footerReference markup —
    /// inheritance is Word's render-time rule, not a save rewrite.
    #[serde(default)]
    pub synthesized: bool,
}

/// Equality ignores `synthesized`: provenance is parse-time metadata, not ref
/// identity — a restored-from-history ref equals the ref it snapshotted.
impl PartialEq for StoryRef {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind && self.part_path == other.part_path
    }
}
impl Eq for StoryRef {}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DocMeta {
    pub schema_version: String,
    pub docx_fingerprint: DocFingerprint,
    pub internal_ids_version: String,
}

/// `Paragraph` and `Table` carry their large payloads behind a `Box` so that
/// `BlockNode` (and therefore `TrackedBlock`, stored by value in
/// `Vec<TrackedBlock>`) stays pointer-sized rather than ~3.8 KB. Without the
/// box, a `Vec` of N blocks reserves `N * sizeof(ParagraphNode)` contiguously
/// (the Rung-5 16 MB single allocation). `Box<T>` is serde/bincode-transparent:
/// it serializes exactly as the inner value, so persisted snapshot blobs keep
/// the same wire shape.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum BlockNode {
    Paragraph(Box<ParagraphNode>),
    Table(Box<TableNode>),
    OpaqueBlock(Box<OpaqueBlockNode>),
}

impl From<ParagraphNode> for BlockNode {
    fn from(p: ParagraphNode) -> Self {
        BlockNode::Paragraph(Box::new(p))
    }
}

impl From<TableNode> for BlockNode {
    fn from(t: TableNode) -> Self {
        BlockNode::Table(Box::new(t))
    }
}

impl From<OpaqueBlockNode> for BlockNode {
    fn from(o: OpaqueBlockNode) -> Self {
        BlockNode::OpaqueBlock(Box::new(o))
    }
}

/// Tracking status for tracked-change aware content.
///
/// The four variants are exactly the per-character state space of OOXML's
/// inline-text tracked-change grammar: base text, a pending insertion, a
/// pending deletion of base text, and a pending deletion OF pending-inserted
/// text (the stacked state, serialized as one tracked container nested in
/// another — `w:del` inside `w:ins` or the converse; both orders denote this
/// one state).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub enum TrackingStatus {
    #[default]
    Normal,
    Inserted(RevisionInfo),
    Deleted(RevisionInfo),
    /// Text inserted by one revision and then deleted by another, BOTH still
    /// pending — "a deletion remembers what it deletes". The
    /// four origin rules define its resolutions:
    ///   accept the insertion → `Deleted(deleted)` (the deletion now targets
    ///     base text); reject the insertion → dropped (the nested deletion
    ///     goes with it — the Word cascade);
    ///   accept the deletion → dropped; reject the deletion →
    ///     `Inserted(inserted)`.
    /// Accept-all and reject-all therefore BOTH drop it; only mixed
    /// resolutions distinguish it from a plain insertion of the kept text.
    /// Boxed: the enum's size is its largest variant, and this state is rare
    /// per document while `Normal` segments are cloned constantly (same
    /// rationale as `InlineNode`'s boxed payloads).
    ///
    /// NOTE: appended LAST — bincode snapshot blobs encode variant indices.
    InsertedThenDeleted(Box<StackedRevision>),
}

/// The two pending revisions of a [`TrackingStatus::InsertedThenDeleted`]
/// segment: `inserted` happened first (structurally — nesting order in the
/// markup is a serialization detail), `deleted` strikes that insertion.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct StackedRevision {
    pub inserted: RevisionInfo,
    pub deleted: RevisionInfo,
}

/// A contiguous paragraph segment sharing one tracking status.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TrackedSegment {
    pub status: TrackingStatus,
    pub inlines: Vec<InlineNode>,
}

/// A block paired with tracked-change status.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TrackedBlock {
    pub status: TrackingStatus,
    pub block: BlockNode,
    /// When set, this block is part of a move operation. The move_id links a
    /// deleted block (move source) with an inserted block (move destination).
    /// Used during serialization to emit `w:moveFrom`/`w:moveTo` instead of
    /// plain `w:del`/`w:ins`.
    pub move_id: Option<String>,
    /// When set, THIS block opens a block-level structured document tag
    /// (`w:sdt`, §17.5.2) whose `w:sdtContent` spans this block and the
    /// `span - 1` blocks immediately after it (authored by the
    /// `WrapBlocksInContentControl` verb). Only the FIRST block of the range
    /// carries the marker; serialization emits `<w:sdt><w:sdtPr>…</w:sdtPr>
    /// <w:sdtContent>` before this block and closes `</w:sdtContent></w:sdt>`
    /// after the `span`-th block.
    ///
    /// A block-level SDT is **structural / untracked**: OOXML has no
    /// `w:sdtChange` envelope (just like the inline `WrapInContentControl`), so
    /// the wrap survives both accept-all and reject-all unchanged. The wrapped
    /// blocks keep their own tracking status independently; this marker only
    /// describes the enclosing wrapper.
    ///
    /// `#[serde(default)]` keeps deserialization of pre-existing snapshots
    /// (serialized before this field existed) stable: they decode as `None`.
    #[serde(default)]
    pub block_sdt_wrap: Option<BlockSdtWrap>,
}

/// A block-level structured document tag wrapper opened on a [`TrackedBlock`]
/// (§17.5.2). Authored by `WrapBlocksInContentControl`. Carries the wrapper's
/// `w:sdtPr` XML (built deterministically from a typed [`SdtControl`] via
/// `serialize::sdt::build_sdt_pr`) plus an optional `w:sdtEndPr`, and the count
/// of consecutive body blocks enclosed by the wrapper's `w:sdtContent`.
///
/// `span` counts from (and includes) the marked block. It is always `>= 1` (a
/// content control wrapping zero blocks has no meaning and is refused at the
/// verb edge). The wrap is closed after the `span`-th block during
/// serialization.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct BlockSdtWrap {
    /// The wrapper's preserved `w:sdtPr` / `w:sdtEndPr` XML.
    pub wrapper: SdtWrapper,
    /// Number of consecutive blocks enclosed by the `w:sdtContent`, counting the
    /// marked block itself. Invariant: `span >= 1`.
    pub span: usize,
}

/// A block-level structured document tag (`w:sdt`, §17.5.2) wrapping a
/// contiguous range of a table cell's blocks. The cell-scoped analogue of
/// [`BlockSdtWrap`]: a body block carries its wrap inline on the owning
/// [`TrackedBlock`], but a cell holds a plain `Vec<BlockNode>`, so a cell's
/// wraps live beside the blocks as `(start, span)` ranges on the cell.
///
/// A cell may hold several independent block content controls interleaved with
/// unwrapped sibling blocks — e.g. a `w14:checkbox` control whose `w:sdtContent`
/// is a single glyph paragraph, immediately followed by a sibling label
/// paragraph. Recording only WHERE each wrap starts (and treating the wrap as
/// "the whole cell") re-nests that following sibling inside the control on
/// export; Word then repairs the file on open, because a content control binds
/// a fixed run count. Recording both `start` and `span` keeps every block
/// outside the range a sibling of the `w:sdt`.
///
/// Invariants (established at import, relied on at serialize): `span >= 1`;
/// `start + span <= cell.blocks.len()`; entries are in document order and their
/// ranges do not overlap.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CellSdtWrap {
    /// Index into the cell's `blocks` of the first enclosed block.
    pub start: usize,
    /// Number of consecutive blocks enclosed by the `w:sdtContent`, counting
    /// `start` itself. Invariant: `span >= 1`.
    pub span: usize,
    /// The wrapper's preserved `w:sdtPr` / `w:sdtEndPr` XML.
    pub wrapper: SdtWrapper,
}

fn serde_true() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ParagraphNode {
    pub id: NodeId,
    pub style_id: Option<IStr>,
    pub align: Option<Alignment>,
    /// True when alignment was set by direct w:jc in the paragraph's pPr
    /// (not inherited from styles or defaults). Used by conditional formatting
    /// post-processing to avoid overwriting direct alignment.
    pub has_direct_align: bool,
    /// RESOLVED EFFECTIVE indentation (numbering-level + style-chain + docDefaults
    /// cascade baked in, plus import-time rendering transforms like tab
    /// absorption). This is the FRONTEND/layout projection value — see
    /// [`Self::authored_indent`] for the serializer's value. The two differ
    /// exactly like [`Self::effective_tab_stops_rel`] differs from
    /// [`Self::tab_stops`].
    pub indent: Option<Indentation>,
    /// True when indentation was set by direct w:ind in the paragraph's pPr
    /// (not inherited from styles). Gates whether the serializer emits a `w:ind`.
    pub has_direct_indent: bool,
    /// AUTHORED-DIRECT indentation: the paragraph's OWN `w:pPr/w:ind`, verbatim
    /// (only the attributes the direct element carried — inherited numbering/
    /// style values are NEVER materialized here). The serializer re-emits exactly
    /// this, so an untouched paragraph round-trips its authored `w:ind` faithfully
    /// (an explicit `left="0"` is kept; an inherited numbering `left` is not
    /// injected). Mirrors the `tab_stops` (authored) vs `effective_tab_stops_rel`
    /// (derived) split. `#[serde(default)]` → None on snapshots predating this
    /// field; the serializer then falls back to the effective `indent` so those
    /// snapshots keep their historical emission (no silent drop).
    #[serde(default)]
    pub authored_indent: Option<Indentation>,
    /// RESOLVED EFFECTIVE spacing (style-chain + docDefaults cascade baked in).
    /// Frontend/layout projection value — see [`Self::authored_spacing`] for the
    /// serializer's value.
    pub spacing: Option<ParagraphSpacing>,
    /// True when spacing was set by direct w:spacing in the paragraph's pPr
    /// (not inherited from styles). Gates whether the serializer emits a `w:spacing`.
    pub has_direct_spacing: bool,
    /// AUTHORED-DIRECT spacing: the paragraph's OWN `w:pPr/w:spacing`, verbatim
    /// (inherited `after`/`line`/etc. are never materialized). Serializer re-emits
    /// exactly this; see [`Self::authored_indent`] for the round-trip rationale and
    /// the `#[serde(default)]` fallback.
    #[serde(default)]
    pub authored_spacing: Option<ParagraphSpacing>,
    pub borders: Option<ParagraphBorders>,
    /// Keep paragraph with next (w:keepNext, §17.3.1.14).
    /// None = absent (inherit from style), Some(false) = explicitly off, Some(true) = on.
    pub keep_next: Option<bool>,
    /// Keep all lines on same page (w:keepLines, §17.3.1.15).
    /// None = absent (inherit from style), Some(false) = explicitly off, Some(true) = on.
    pub keep_lines: Option<bool>,
    /// Force page break before paragraph (w:pageBreakBefore).
    pub page_break_before: bool,
    /// Widow/orphan control (w:widowControl). None = inherit default (true per spec),
    /// Some(false) = explicitly disabled via val="0".
    pub widow_control: Option<bool>,
    /// Suppress before/after spacing between adjacent paragraphs sharing the
    /// same style (w:contextualSpacing, §17.3.1.9).
    /// None = absent (inherit from style), Some(true) = on, Some(false) = explicitly off.
    pub contextual_spacing: Option<bool>,
    /// Paragraph shading from w:pPr/w:shd.
    pub shading: Option<Shading>,
    // Parse-time provenance flags for the remaining style-resolved pPr slots
    // (the paragraph-side twin of RunRprAuthored): the field holds the RESOLVED
    // effective value for projections; the serializer emits it as direct pPr
    // only when the paragraph's own pPr authored it. Serde-defaults to true so
    // snapshots from before these flags existed keep emitting (over-emission is
    // the historical behavior; silent dropping would be a regression).
    #[serde(default = "serde_true")]
    pub has_direct_keep_next: bool,
    #[serde(default = "serde_true")]
    pub has_direct_keep_lines: bool,
    #[serde(default = "serde_true")]
    pub has_direct_page_break_before: bool,
    #[serde(default = "serde_true")]
    pub has_direct_widow_control: bool,
    #[serde(default = "serde_true")]
    pub has_direct_contextual_spacing: bool,
    #[serde(default = "serde_true")]
    pub has_direct_shading: bool,
    #[serde(default = "serde_true")]
    pub has_direct_borders: bool,
    /// AUTHORED tab stops: the paragraph's own direct `w:pPr/w:tabs`, verbatim
    /// (page-absolute positions, `Clear` entries included). The serializer
    /// re-emits exactly this — style-inherited / default-grid stops are never
    /// materialized into pPr (they live in styles.xml / the implicit grid).
    /// Empty = the paragraph authors no stops of its own.
    pub tab_stops: Vec<crate::word_ir::TabStopDef>,
    /// DERIVED view value (never serialized to DOCX, never compared for
    /// roundtrip): effective tab stops after style resolution + default-grid
    /// synthesis, converted to body-left-relative positions for the frontend.
    /// Computed at import; goes stale on in-memory edits until the snapshot is
    /// rebuilt from serialized bytes (same lifecycle as other derived fields).
    #[serde(default)]
    pub effective_tab_stops_rel: Vec<crate::word_ir::TabStopDef>,
    pub segments: Vec<TrackedSegment>,
    pub block_text_hash: Option<String>,
    /// RESOLVED EFFECTIVE numbering: the paragraph's `w:numPr` after the
    /// direct-then-style (§17.7.4.14)-then-pStyle-reverse-binding (§17.9.23)
    /// cascade. This is the value the frontend renders and the counter machinery
    /// synthesizes from — it is NOT necessarily authored on the paragraph's own
    /// pPr. Gated for serialization by [`Self::has_direct_numbering`].
    pub numbering: Option<NumberingInfo>,
    /// True when the paragraph's OWN direct `w:pPr/w:numPr` authored the
    /// numbering (`DirectNumPr::Active`). Gates whether the serializer emits a
    /// direct `w:numPr` — the numbering analogue of [`Self::has_direct_indent`].
    ///
    /// Without this gate, numbering INHERITED from a paragraph style (or bound
    /// to the pStyle via the abstractNum's `<w:pStyle>` reverse link, §17.9.23)
    /// would be materialized as a direct `w:numPr` on every whole-document
    /// rebuild, changing an untouched paragraph's numbering-inherited indent
    /// with no `pPrChange`. `#[serde(default = "serde_true")]` → true on
    /// snapshots predating the field, preserving their historical (over-)
    /// emission rather than silently dropping numbering.
    #[serde(default = "serde_true")]
    pub has_direct_numbering: bool,
    /// True when w:numPr/w:numId=0 (§17.9.18) explicitly removes inherited
    /// (style/pStyle) numbering. MUTUALLY EXCLUSIVE with `numbering: Some(..)`:
    /// a paragraph is either actively numbered, suppressed, or
    /// inheriting-absent. Carried so the serializer can re-emit
    /// `<w:numPr><w:ilvl w:val="0"/><w:numId w:val="0"/></w:numPr>` instead of
    /// silently letting the paragraph re-inherit its style's numbering.
    #[serde(default)]
    pub numbering_suppressed: bool,
    /// Original numbering info saved before materialization into inline `_npfx`
    /// nodes. Set by `materialize_numbering_prefix` / `_in_place` when
    /// `numbering` is cleared to prevent the serializer from double-emitting
    /// `w:numPr`. After accept/reject, `normalize_paragraph_after_projection`
    /// restores `numbering` from this field so the projected document keeps
    /// structural numbering (matching the target) instead of degrading to
    /// `literal_prefix`.
    pub materialized_numbering: Option<NumberingInfo>,
    /// Pre-rendered text including synthesized number prefix.
    /// Used for diff comparison to avoid spurious diffs between
    /// manual numbering (text) and auto-numbering (numPr).
    pub rendered_text: Option<String>,
    /// Typed numbering prefix detected and stripped from inlines (e.g., "(a)", "1.").
    ///
    /// When present, the prefix text has been removed from `segments` and the
    /// tab that separated it from body text has been consumed.  The paragraph's
    /// `indent` is **unchanged** — `first_line` still reflects the OOXML cascade
    /// value.  The frontend renders the prefix inline (via `::before`) as part
    /// of the first line, positioned by `text-indent`.
    pub literal_prefix: Option<String>,
    /// Direct/effective formatting marks for the stripped literal prefix text.
    ///
    /// `literal_prefix` is serialized as its own run, so its formatting must be
    /// preserved independently from the first body run.
    pub literal_prefix_marks: Vec<Mark>,
    /// Value-carrying formatting properties for the stripped literal prefix text.
    pub literal_prefix_style_props: StyleProps,
    /// Per-slot run-rPr provenance for the stripped literal prefix run: which of
    /// `literal_prefix_style_props`' slots the original prefix run AUTHORED
    /// directly vs inherited through the cascade. The prefix is re-emitted as its
    /// own run, so — exactly like a `TextNode` — it must emit ONLY authored slots;
    /// otherwise an inherited theme font / themeColor gets baked onto the prefix
    /// run and (winning per §17.3.2.26) changes its rendering.
    #[serde(default)]
    pub literal_prefix_rpr_authored: RunRprAuthored,
    /// Formatting of the run(s) that carried the prefix's LEADING whitespace,
    /// when it differs from the label's formatting. A paragraph like
    /// `[Arial rPr + tab] (c) [tab] Body` authors the leading tab in its OWN
    /// run whose rPr the label-formatting slot cannot represent — without this
    /// carrier the leading tab re-emits wearing the label's formatting and the
    /// authored rPr silently vanishes (the SAFE-template w:b / rFonts loss).
    #[serde(default)]
    pub literal_prefix_leading_rpr: Option<Box<PrefixLeadingRpr>>,
    /// Formatting of the run that carried the prefix's TRAILING separator
    /// (label-to-body tab/spaces) when it differs from the label's — the
    /// trailing twin of `literal_prefix_leading_rpr` ("(a) [b+i tab] Body"
    /// authored the separator tab bold+italic in its own run).
    #[serde(default)]
    pub literal_prefix_trailing_rpr: Option<Box<PrefixLeadingRpr>>,
    /// Gap in twips from the paragraph's margin-left to the first tab stop,
    /// when the stripped prefix had a leading tab (e.g., `\t(a)\t`).
    ///
    /// This value tracks the stripped prefix's consumed tab geometry.
    /// The paragraph indent model remains intact, including any resolved
    /// `indent.effective_first_line_twips`, so accept/reject can faithfully
    /// reconstruct the original layout.
    ///
    /// The frontend uses this as `padding-left` on `::before` (via CSS
    /// variable `--prefix-leading-gap`) instead of a CSS tab character.
    pub literal_prefix_leading_tab_twips: Option<i32>,
    /// Number of leading tab characters stripped before the literal prefix label.
    ///
    /// This is distinct from `literal_prefix_leading_tab_twips`: multiple
    /// leading tabs can share the same first explicit tab stop, with later
    /// tabs advancing on Word's default grid.
    pub literal_prefix_leading_tab_count: u8,
    /// Verbatim whitespace (spaces and tabs, in source order) stripped from
    /// BEFORE the label. Significant whitespace (XML 1.0 §2.10): the serializer
    /// re-emits it verbatim so leading indentation survives round-trip. Empty
    /// when no whitespace preceded the label. Distinct from
    /// `literal_prefix_leading_tab_count` (a discretized tab count that drops
    /// interleaved spaces — kept for frontend geometry).
    #[serde(default)]
    pub literal_prefix_leading_ws: String,
    /// Verbatim whitespace (spaces and tabs, in source order) stripped from
    /// BETWEEN the label and the body text. The serializer re-emits it verbatim
    /// (the prior model collapsed it to a single space, losing the real
    /// run-length). Empty when the label abutted the body.
    #[serde(default)]
    pub literal_prefix_trailing_ws: String,
    /// Whether the stripped literal prefix was followed by a real tab
    /// character before the body text.
    pub literal_prefix_has_trailing_tab: bool,
    /// Gap in twips from the paragraph's margin-left to the explicit tab stop
    /// consumed by prefix tabs, when that stop came from `w:tabs` rather than
    /// Word's default tab grid.
    ///
    /// The serializer uses this to re-emit the explicit paragraph tab stop.
    pub literal_prefix_trailing_tab_stop_twips: Option<i32>,
    /// Outline level from a DIRECT w:pPr/w:outlineLvl (§17.3.1.20), 0-based wire
    /// value. None = absent (the paragraph did not directly author outlineLvl; it
    /// may still inherit one via the style cascade — that inherited value is NOT
    /// stored here and is NOT re-emitted, mirroring the has_direct_* discipline for
    /// jc/ind/spacing). `heading_level` is derived separately and keeps the
    /// resolved cascade for frontend semantics.
    pub outline_lvl: Option<u8>,
    /// Heading level if this paragraph is a heading (in DOCX, headings are paragraphs with heading styles).
    pub heading_level: Option<HeadingLevel>,
    /// Paragraph mark tracking status (e.g. deleted paragraph mark).
    pub para_mark_status: Option<TrackingStatus>,
    /// Direct formatting on the paragraph mark from w:pPr/w:rPr.
    /// This applies to the paragraph mark itself, not the text runs.
    pub paragraph_mark_marks: Vec<Mark>,
    /// Direct value-carrying style properties on the paragraph mark from w:pPr/w:rPr.
    pub paragraph_mark_style_props: StyleProps,
    /// AUTHORED OFF toggles on the paragraph mark's `w:pPr/w:rPr` (§17.3.1.29
    /// CT_ParaRPr) that the presence-only `paragraph_mark_marks: Vec<Mark>` cannot
    /// represent — the pilcrow analogue of `RunRprAuthored::{bold_off, italic_off,
    /// underline_off}`. See [`ParaMarkRprOff`].
    #[serde(default)]
    pub paragraph_mark_rpr_off: ParaMarkRprOff,
    /// True when this paragraph was created by a paragraph split (one base
    /// paragraph became two). Guards the merge logic: split paragraphs must
    /// NOT merge on reject because their inline changes already carry the
    /// full original text as Deleted segments.
    pub para_split: bool,
    /// Tracked change for section properties (w:sectPrChange inside w:sectPr).
    /// Present when a reviewer changed page layout and the change was tracked.
    pub section_property_change: Option<SectionPropertyChange>,
    /// Tracked paragraph formatting change from w:pPrChange (§17.13.5.29).
    /// Present when paragraph formatting was changed via Track Changes.
    pub formatting_change: Option<ParagraphFormattingChange>,
    /// Structured section properties parsed from w:sectPr (page size, columns).
    /// Mid-document section breaks live here.
    pub section_properties: Option<SectionProperties>,
    /// Mirror indents for facing pages (w:mirrorIndents, §17.3.1.18).
    /// None = absent (inherit from style), Some(true) = explicitly on,
    /// Some(false) = explicitly off (`w:val="0"`). An explicit OFF is an
    /// authored override — NOT the same as absent — and must round-trip
    /// (§17.17.4 ST_OnOff), so this is `Option<bool>` like its sibling flags
    /// (`overflow_punct`, `word_wrap`, …) rather than a lossy plain `bool`.
    /// Carries the paragraph's OWN direct value (never a style-resolved
    /// effective one), so the serializer emits both polarities without ever
    /// materializing an inherited flag. `#[serde(default)]` → None on snapshots
    /// predating the field (bincode blobs are version-gated separately).
    #[serde(default)]
    pub mirror_indents: Option<bool>,
    /// Automatically adjust spacing between Latin and East Asian text (w:autoSpaceDE).
    pub auto_space_de: Option<bool>,
    /// Automatically adjust spacing between East Asian and Latin text (w:autoSpaceDN).
    pub auto_space_dn: Option<bool>,
    /// Right-to-left paragraph layout (w:bidi, §17.3.1.6).
    /// None = absent (inherit from style/docDefaults), Some(true) = explicitly
    /// on, Some(false) = explicitly off (`w:val="0"`). An explicit OFF cancels
    /// an inherited RTL layout — a real override that flips left/right indent
    /// interpretation — so it must round-trip (§17.17.4 ST_OnOff); modelling it
    /// as a plain `bool` silently dropped it. Carries the paragraph's OWN direct
    /// value; the serializer emits both polarities and never materializes an
    /// inherited flag. `#[serde(default)]` → None on snapshots predating the
    /// field (bincode blobs are version-gated separately).
    #[serde(default)]
    pub bidi: Option<bool>,
    /// Vertical character alignment on each line (w:textAlignment, §17.3.1.39).
    pub text_alignment: Option<TextAlignment>,
    /// Text direction for paragraph (w:textDirection, §17.3.1.40).
    pub text_direction: Option<TextDirection>,
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
    /// Paragraph ID from w14:paraId (hex string, used by GDocs/Word for identity).
    pub para_id: Option<String>,
    /// Text ID from w14:textId (hex string).
    pub text_id: Option<String>,
    /// Conditional formatting flags (w:cnfStyle, §17.3.1.8).
    /// Present on paragraphs inside table cells to indicate which table
    /// conditional formats apply.
    pub cnf_style: Option<CnfStyle>,
    /// Unmodeled w:pPr children, captured verbatim at import and re-emitted
    /// at serialization so they survive round-trip (see [`PreservedProp`]).
    /// Mirrors `StyleProps::preserved` for the paragraph-properties container.
    #[serde(default)]
    pub preserved_ppr: Vec<PreservedProp>,
}

/// Conditional formatting flags from w:cnfStyle (§17.3.1.8).
///
/// Applied to paragraphs inside table cells to indicate which table
/// conditional formats apply. The 12 boolean attributes correspond to
/// the 12-bit `val` string (legacy format).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CnfStyle {
    /// Legacy 12-character binary string (e.g. "100000000000").
    pub val: Option<String>,
    pub first_row: bool,
    pub last_row: bool,
    pub first_column: bool,
    pub last_column: bool,
    pub odd_v_band: bool,
    pub even_v_band: bool,
    pub odd_h_band: bool,
    pub even_h_band: bool,
    pub first_row_first_column: bool,
    pub first_row_last_column: bool,
    pub last_row_first_column: bool,
    pub last_row_last_column: bool,
}

/// Vertical character alignment on each line per §17.3.1.39 `ST_TextAlignment`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub enum TextAlignment {
    Auto,
    Top,
    Center,
    Baseline,
    Bottom,
}

impl TextAlignment {
    pub fn from_xml_str(s: &str) -> Result<Self, String> {
        match s {
            "auto" => Ok(Self::Auto),
            "top" => Ok(Self::Top),
            "center" => Ok(Self::Center),
            "baseline" => Ok(Self::Baseline),
            "bottom" => Ok(Self::Bottom),
            other => Err(format!("unknown TextAlignment: {other:?}")),
        }
    }

    pub fn to_xml_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Top => "top",
            Self::Center => "center",
            Self::Baseline => "baseline",
            Self::Bottom => "bottom",
        }
    }
}

/// Table layout algorithm per §17.4.52 `ST_TblLayoutType`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub enum TableLayout {
    Fixed,
    Autofit,
}

impl TableLayout {
    pub fn from_xml_str(s: &str) -> Result<Self, String> {
        match s {
            "fixed" => Ok(Self::Fixed),
            "autofit" => Ok(Self::Autofit),
            other => Err(format!("unknown TableLayout: {other:?}")),
        }
    }

    pub fn to_xml_str(&self) -> &'static str {
        match self {
            Self::Fixed => "fixed",
            Self::Autofit => "autofit",
        }
    }
}

/// Table overlap setting per §17.4.55 `ST_TblOverlap`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub enum TableOverlap {
    Never,
    Overlap,
}

impl TableOverlap {
    pub fn from_xml_str(s: &str) -> Result<Self, String> {
        match s {
            "never" => Ok(Self::Never),
            "overlap" => Ok(Self::Overlap),
            other => Err(format!("unknown TableOverlap: {other:?}")),
        }
    }

    pub fn to_xml_str(&self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::Overlap => "overlap",
        }
    }
}

/// Vertical anchor per §17.18.100 `ST_VAnchor`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub enum VAnchor {
    Text,
    Margin,
    Page,
}

impl VAnchor {
    pub fn from_xml_str(s: &str) -> Result<Self, String> {
        match s {
            "text" => Ok(Self::Text),
            "margin" => Ok(Self::Margin),
            "page" => Ok(Self::Page),
            other => Err(format!("unknown VAnchor: {other:?}")),
        }
    }

    pub fn to_xml_str(&self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Margin => "margin",
            Self::Page => "page",
        }
    }
}

/// Horizontal anchor per §17.18.35 `ST_HAnchor`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub enum HAnchor {
    Text,
    Margin,
    Page,
}

impl HAnchor {
    pub fn from_xml_str(s: &str) -> Result<Self, String> {
        match s {
            "text" => Ok(Self::Text),
            "margin" => Ok(Self::Margin),
            "page" => Ok(Self::Page),
            other => Err(format!("unknown HAnchor: {other:?}")),
        }
    }

    pub fn to_xml_str(&self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Margin => "margin",
            Self::Page => "page",
        }
    }
}

/// Frame wrap type per §17.18.104 `ST_Wrap`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub enum FrameWrap {
    Auto,
    NotBeside,
    Around,
    Tight,
    Through,
    None,
}

impl FrameWrap {
    pub fn from_xml_str(s: &str) -> Result<Self, String> {
        match s {
            "auto" => Ok(Self::Auto),
            "notBeside" => Ok(Self::NotBeside),
            "around" => Ok(Self::Around),
            "tight" => Ok(Self::Tight),
            "through" => Ok(Self::Through),
            "none" => Ok(Self::None),
            other => Err(format!("unknown FrameWrap: {other:?}")),
        }
    }

    pub fn to_xml_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::NotBeside => "notBeside",
            Self::Around => "around",
            Self::Tight => "tight",
            Self::Through => "through",
            Self::None => "none",
        }
    }
}

/// Frame horizontal alignment per §17.18.105 `ST_XAlign`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub enum XAlign {
    Left,
    Center,
    Right,
    Inside,
    Outside,
}

impl XAlign {
    pub fn from_xml_str(s: &str) -> Result<Self, String> {
        match s {
            "left" => Ok(Self::Left),
            "center" => Ok(Self::Center),
            "right" => Ok(Self::Right),
            "inside" => Ok(Self::Inside),
            "outside" => Ok(Self::Outside),
            other => Err(format!("unknown XAlign: {other:?}")),
        }
    }

    pub fn to_xml_str(&self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Center => "center",
            Self::Right => "right",
            Self::Inside => "inside",
            Self::Outside => "outside",
        }
    }
}

/// Frame vertical alignment per §17.18.106 `ST_YAlign`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub enum YAlign {
    Inline,
    Top,
    Center,
    Bottom,
    Inside,
    Outside,
}

impl YAlign {
    pub fn from_xml_str(s: &str) -> Result<Self, String> {
        match s {
            "inline" => Ok(Self::Inline),
            "top" => Ok(Self::Top),
            "center" => Ok(Self::Center),
            "bottom" => Ok(Self::Bottom),
            "inside" => Ok(Self::Inside),
            "outside" => Ok(Self::Outside),
            other => Err(format!("unknown YAlign: {other:?}")),
        }
    }

    pub fn to_xml_str(&self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::Top => "top",
            Self::Center => "center",
            Self::Bottom => "bottom",
            Self::Inside => "inside",
            Self::Outside => "outside",
        }
    }
}

/// Text frame properties from w:framePr (§17.3.1.11, CT_FramePr).
///
/// The modeled fields below cover the geometry and anchoring attributes; any
/// CT_FramePr attribute not modeled here (`w:dropCap`, `w:lines`,
/// `w:anchorLock`, plus anything a future schema adds) is captured verbatim in
/// `extra_attrs` at the extraction edge and re-emitted unchanged (RFC-0003
/// attribute-level remainder). Nothing on `w:framePr` is ever silently dropped.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct FrameProperties {
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub h_rule: Option<HeightRule>,
    pub h_space: Option<i64>,
    /// Vertical spacing (`w:vSpace`, ST_TwipsMeasure). Paired with `h_space`.
    pub v_space: Option<i64>,
    pub wrap: Option<FrameWrap>,
    pub v_anchor: Option<VAnchor>,
    pub h_anchor: Option<HAnchor>,
    pub x: Option<i64>,
    /// Relative horizontal alignment (`w:xAlign`); alternative to absolute `x`.
    pub x_align: Option<XAlign>,
    /// Absolute vertical position (`w:y`, ST_SignedTwipsMeasure); the `y`
    /// counterpart to `x`.
    pub y: Option<i64>,
    /// Relative vertical alignment (`w:yAlign`); alternative to absolute `y`.
    pub y_align: Option<YAlign>,
    /// CT_FramePr attributes not modeled above, captured and re-emitted verbatim.
    pub extra_attrs: Vec<(String, String)>,
}

impl ParagraphNode {
    /// Flatten all paragraph inlines, ignoring tracking status.
    pub fn all_inlines(&self) -> impl Iterator<Item = &InlineNode> {
        self.segments
            .iter()
            .flat_map(|segment| segment.inlines.iter())
    }

    /// Flatten all paragraph inlines into an owned vec.
    pub fn all_inlines_owned(&self) -> Vec<InlineNode> {
        self.all_inlines().cloned().collect()
    }

    /// Return the first content `TextNode`, skipping materialized prefix nodes.
    pub fn first_content_text_node(&self) -> Option<&TextNode> {
        self.all_inlines().find_map(|inline| match inline {
            InlineNode::Text(t) if !is_materialized_prefix_text(t) => Some(t.as_ref()),
            _ => None,
        })
    }

    /// Build a minimal single-run paragraph for a synthesized story body (a
    /// comment, a reply, etc.). `para_id` becomes the `w14:paraId` — required
    /// for comments so the `commentsExtended` sidecar can reference it. All
    /// formatting fields take their defaults; this is plain authored text, not
    /// a clone of an existing styled paragraph.
    pub fn new_story_body(id: &str, text: &str, para_id: Option<String>) -> ParagraphNode {
        ParagraphNode {
            id: NodeId::from(id),
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
                inlines: vec![InlineNode::from(TextNode {
                    id: NodeId::from(format!("{id}_r0")),
                    text_role: None,
                    text: text.to_string(),
                    marks: vec![],
                    style_props: StyleProps::default(),
                    rpr_authored: RunRprAuthored::default(),
                    formatting_change: None,
                })],
            }],
            block_text_hash: None,
            numbering: None,
            has_direct_numbering: true,
            numbering_suppressed: false,
            materialized_numbering: None,
            rendered_text: None,
            literal_prefix: None,
            literal_prefix_marks: Vec::new(),
            literal_prefix_style_props: StyleProps::default(),
            literal_prefix_rpr_authored: RunRprAuthored::default(),
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
            paragraph_mark_rpr_off: ParaMarkRprOff::default(),
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
            para_id,
            text_id: None,
            text_direction: None,
            cnf_style: None,
            preserved_ppr: Vec::new(),
        }
    }
}

pub fn materialized_prefix_kind_for_text(text: &TextNode) -> Option<MaterializedPrefixKind> {
    text.text_role
        .as_ref()
        .map(|TextRole::MaterializedPrefix(kind)| *kind)
}

pub fn is_materialized_prefix_text(text: &TextNode) -> bool {
    materialized_prefix_kind_for_text(text).is_some()
}

pub fn normal_segment(inlines: Vec<InlineNode>) -> Vec<TrackedSegment> {
    if inlines.is_empty() {
        Vec::new()
    } else {
        vec![TrackedSegment {
            status: TrackingStatus::Normal,
            inlines,
        }]
    }
}

pub fn normal_tracked_block(block: BlockNode) -> TrackedBlock {
    TrackedBlock {
        status: TrackingStatus::Normal,
        block,
        move_id: None,
        block_sdt_wrap: None,
    }
}

/// Page orientation (§17.6.14).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum PageOrientation {
    Portrait,
    Landscape,
}

/// Section type per §17.6.22 `ST_SectionMark`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub enum SectionType {
    NextPage,
    Continuous,
    EvenPage,
    OddPage,
    /// Column section break — starts the new section in the next column (§17.6.22).
    NextColumn,
}

impl SectionType {
    pub fn from_xml_str(s: &str) -> Result<Self, String> {
        match s {
            "nextPage" => Ok(Self::NextPage),
            "continuous" => Ok(Self::Continuous),
            "evenPage" => Ok(Self::EvenPage),
            "oddPage" => Ok(Self::OddPage),
            "nextColumn" => Ok(Self::NextColumn),
            other => Err(format!("unknown SectionType: {other:?}")),
        }
    }

    pub fn to_xml_str(&self) -> &'static str {
        match self {
            Self::NextPage => "nextPage",
            Self::Continuous => "continuous",
            Self::EvenPage => "evenPage",
            Self::OddPage => "oddPage",
            Self::NextColumn => "nextColumn",
        }
    }
}

/// Vertical alignment for section content per §17.18.100 `ST_VerticalJc`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub enum SectionVAlign {
    Top,
    Center,
    Bottom,
    Both,
}

impl SectionVAlign {
    pub fn from_xml_str(s: &str) -> Result<Self, String> {
        match s {
            "top" => Ok(Self::Top),
            "center" => Ok(Self::Center),
            "bottom" => Ok(Self::Bottom),
            "both" => Ok(Self::Both),
            other => Err(format!("unknown SectionVAlign: {other:?}")),
        }
    }

    pub fn to_xml_str(&self) -> &'static str {
        match self {
            Self::Top => "top",
            Self::Center => "center",
            Self::Bottom => "bottom",
            Self::Both => "both",
        }
    }
}

/// Text direction per §17.18.93 `ST_TextDirection`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub enum TextDirection {
    LrTb,
    TbRl,
    BtLr,
    LrTbV,
    TbRlV,
}

impl TextDirection {
    pub fn from_xml_str(s: &str) -> Result<Self, String> {
        match s {
            "lrTb" => Ok(Self::LrTb),
            "tbRl" => Ok(Self::TbRl),
            "btLr" => Ok(Self::BtLr),
            "lrTbV" => Ok(Self::LrTbV),
            "tbRlV" => Ok(Self::TbRlV),
            other => Err(format!("unknown TextDirection: {other:?}")),
        }
    }

    pub fn to_xml_str(&self) -> &'static str {
        match self {
            Self::LrTb => "lrTb",
            Self::TbRl => "tbRl",
            Self::BtLr => "btLr",
            Self::LrTbV => "lrTbV",
            Self::TbRlV => "tbRlV",
        }
    }
}

/// Document grid type per §17.18.14 `ST_DocGrid`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub enum DocGridType {
    Default,
    Lines,
    LinesAndChars,
    SnapToChars,
}

impl DocGridType {
    pub fn from_xml_str(s: &str) -> Result<Self, String> {
        match s {
            "default" => Ok(Self::Default),
            "lines" => Ok(Self::Lines),
            "linesAndChars" => Ok(Self::LinesAndChars),
            "snapToChars" => Ok(Self::SnapToChars),
            other => Err(format!("unknown DocGridType: {other:?}")),
        }
    }

    pub fn to_xml_str(&self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Lines => "lines",
            Self::LinesAndChars => "linesAndChars",
            Self::SnapToChars => "snapToChars",
        }
    }
}

/// Height rule for table rows per §17.18.37 `ST_HeightRule`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub enum HeightRule {
    Exact,
    AtLeast,
    Auto,
}

impl HeightRule {
    pub fn from_xml_str(s: &str) -> Result<Self, String> {
        match s {
            "exact" => Ok(Self::Exact),
            "atLeast" => Ok(Self::AtLeast),
            "auto" => Ok(Self::Auto),
            other => Err(format!("unknown HeightRule: {other:?}")),
        }
    }

    pub fn to_xml_str(&self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::AtLeast => "atLeast",
            Self::Auto => "auto",
        }
    }
}

/// Tab stop alignment (ECMA-376 §17.18.81 ST_TabJc).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum TabAlignment {
    Bar,
    Center,
    Clear,
    Decimal,
    End,
    Left,
    Num,
    Right,
    Start,
}

impl TabAlignment {
    pub fn from_xml_str(s: &str) -> Result<Self, String> {
        match s {
            "bar" => Ok(Self::Bar),
            "center" => Ok(Self::Center),
            "clear" => Ok(Self::Clear),
            "decimal" => Ok(Self::Decimal),
            "end" => Ok(Self::End),
            "left" => Ok(Self::Left),
            "num" => Ok(Self::Num),
            "right" => Ok(Self::Right),
            "start" => Ok(Self::Start),
            other => Err(format!("unknown TabAlignment: {other:?}")),
        }
    }

    pub fn to_xml_str(&self) -> &'static str {
        match self {
            Self::Bar => "bar",
            Self::Center => "center",
            Self::Clear => "clear",
            Self::Decimal => "decimal",
            Self::End => "end",
            Self::Left => "left",
            Self::Num => "num",
            Self::Right => "right",
            Self::Start => "start",
        }
    }
}

/// Tab stop leader character (ECMA-376 §17.18.82 ST_TabTlc).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum TabLeader {
    Dot,
    Heavy,
    Hyphen,
    MiddleDot,
    None,
    Underscore,
}

impl TabLeader {
    pub fn from_xml_str(s: &str) -> Result<Self, String> {
        match s {
            "dot" => Ok(Self::Dot),
            "heavy" => Ok(Self::Heavy),
            "hyphen" => Ok(Self::Hyphen),
            "middleDot" => Ok(Self::MiddleDot),
            "none" => Ok(Self::None),
            "underscore" => Ok(Self::Underscore),
            other => Err(format!("unknown TabLeader: {other:?}")),
        }
    }

    pub fn to_xml_str(&self) -> &'static str {
        match self {
            Self::Dot => "dot",
            Self::Heavy => "heavy",
            Self::Hyphen => "hyphen",
            Self::MiddleDot => "middleDot",
            Self::None => "none",
            Self::Underscore => "underscore",
        }
    }
}

/// Footnote position (ECMA-376 §17.18.23 ST_FtnPos).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum FootnotePosition {
    PageBottom,
    BeneathText,
}

impl FootnotePosition {
    pub fn from_xml_str(s: &str) -> Result<Self, String> {
        match s {
            "pageBottom" => Ok(Self::PageBottom),
            "beneathText" => Ok(Self::BeneathText),
            other => Err(format!("unknown FootnotePosition: {other:?}")),
        }
    }

    pub fn to_xml_str(&self) -> &'static str {
        match self {
            Self::PageBottom => "pageBottom",
            Self::BeneathText => "beneathText",
        }
    }
}

/// Endnote position (ECMA-376 §17.18.14 ST_EdnPos).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum EndnotePosition {
    SectEnd,
    DocEnd,
}

impl EndnotePosition {
    pub fn from_xml_str(s: &str) -> Result<Self, String> {
        match s {
            "sectEnd" => Ok(Self::SectEnd),
            "docEnd" => Ok(Self::DocEnd),
            other => Err(format!("unknown EndnotePosition: {other:?}")),
        }
    }

    pub fn to_xml_str(&self) -> &'static str {
        match self {
            Self::SectEnd => "sectEnd",
            Self::DocEnd => "docEnd",
        }
    }
}

/// Note position — unified enum for footnote and endnote positions.
///
/// Footnotes use PageBottom/BeneathText (ST_FtnPos §17.18.23).
/// Endnotes use SectEnd/DocEnd (ST_EdnPos §17.18.14).
/// Both share the same NoteProperties struct, so this enum covers all values.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum NotePosition {
    PageBottom,
    BeneathText,
    SectEnd,
    DocEnd,
}

impl NotePosition {
    pub fn from_xml_str(s: &str) -> Result<Self, String> {
        match s {
            "pageBottom" => Ok(Self::PageBottom),
            "beneathText" => Ok(Self::BeneathText),
            "sectEnd" => Ok(Self::SectEnd),
            "docEnd" => Ok(Self::DocEnd),
            other => Err(format!("unknown NotePosition: {other:?}")),
        }
    }

    pub fn to_xml_str(&self) -> &'static str {
        match self {
            Self::PageBottom => "pageBottom",
            Self::BeneathText => "beneathText",
            Self::SectEnd => "sectEnd",
            Self::DocEnd => "docEnd",
        }
    }
}

/// Number restart rule (ECMA-376 §17.18.60 ST_RestartNumber).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum RestartRule {
    Continuous,
    EachSect,
    EachPage,
}

impl RestartRule {
    pub fn from_xml_str(s: &str) -> Result<Self, String> {
        match s {
            "continuous" => Ok(Self::Continuous),
            "eachSect" => Ok(Self::EachSect),
            "eachPage" => Ok(Self::EachPage),
            other => Err(format!("unknown RestartRule: {other:?}")),
        }
    }

    pub fn to_xml_str(&self) -> &'static str {
        match self {
            Self::Continuous => "continuous",
            Self::EachSect => "eachSect",
            Self::EachPage => "eachPage",
        }
    }
}

/// Number format (ECMA-376 §17.18.59 ST_NumberFormat).
///
/// Common values from the spec. This is a large enumeration; additional values
/// can be added as needed when encountered in real documents.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum NumberFormat {
    Decimal,
    UpperRoman,
    LowerRoman,
    UpperLetter,
    LowerLetter,
    Ordinal,
    CardinalText,
    OrdinalText,
    Chicago,
    Bullet,
    None,
    DecimalZero,
    DecimalEnclosedCircle,
    IdeographDigital,
    IdeographTraditional,
    IdeographLegalTraditional,
    IdeographZodiac,
    IdeographZodiacTraditional,
    JapaneseCounting,
    JapaneseDigitalTenThousand,
    JapaneseLegal,
    ChineseCounting,
    ChineseCountingThousand,
    ChineseLegalSimplified,
    KoreanCounting,
    KoreanDigital,
    KoreanDigital2,
    KoreanLegal,
    Hebrew1,
    Hebrew2,
    ArabicAlpha,
    ArabicAbjad,
    HindiVowels,
    HindiConsonants,
    HindiNumbers,
    HindiCounting,
    ThaiLetters,
    ThaiNumbers,
    ThaiCounting,
    TaiwaneseCountingThousand,
    TaiwaneseDigital,
    TaiwaneseCounting,
    VietnameseCounting,
    NumberInDash,
    RussianLower,
    RussianUpper,
}

impl NumberFormat {
    pub fn from_xml_str(s: &str) -> Result<Self, String> {
        match s {
            "decimal" => Ok(Self::Decimal),
            "upperRoman" => Ok(Self::UpperRoman),
            "lowerRoman" => Ok(Self::LowerRoman),
            "upperLetter" => Ok(Self::UpperLetter),
            "lowerLetter" => Ok(Self::LowerLetter),
            "ordinal" => Ok(Self::Ordinal),
            "cardinalText" => Ok(Self::CardinalText),
            "ordinalText" => Ok(Self::OrdinalText),
            "chicago" => Ok(Self::Chicago),
            "bullet" => Ok(Self::Bullet),
            "none" => Ok(Self::None),
            "decimalZero" => Ok(Self::DecimalZero),
            "decimalEnclosedCircle" => Ok(Self::DecimalEnclosedCircle),
            "ideographDigital" => Ok(Self::IdeographDigital),
            "ideographTraditional" => Ok(Self::IdeographTraditional),
            "ideographLegalTraditional" => Ok(Self::IdeographLegalTraditional),
            "ideographZodiac" => Ok(Self::IdeographZodiac),
            "ideographZodiacTraditional" => Ok(Self::IdeographZodiacTraditional),
            "japaneseCounting" => Ok(Self::JapaneseCounting),
            "japaneseDigitalTenThousand" => Ok(Self::JapaneseDigitalTenThousand),
            "japaneseLegal" => Ok(Self::JapaneseLegal),
            "chineseCounting" => Ok(Self::ChineseCounting),
            "chineseCountingThousand" => Ok(Self::ChineseCountingThousand),
            "chineseLegalSimplified" => Ok(Self::ChineseLegalSimplified),
            "koreanCounting" => Ok(Self::KoreanCounting),
            "koreanDigital" => Ok(Self::KoreanDigital),
            "koreanDigital2" => Ok(Self::KoreanDigital2),
            "koreanLegal" => Ok(Self::KoreanLegal),
            "hebrew1" => Ok(Self::Hebrew1),
            "hebrew2" => Ok(Self::Hebrew2),
            "arabicAlpha" => Ok(Self::ArabicAlpha),
            "arabicAbjad" => Ok(Self::ArabicAbjad),
            "hindiVowels" => Ok(Self::HindiVowels),
            "hindiConsonants" => Ok(Self::HindiConsonants),
            "hindiNumbers" => Ok(Self::HindiNumbers),
            "hindiCounting" => Ok(Self::HindiCounting),
            "thaiLetters" => Ok(Self::ThaiLetters),
            "thaiNumbers" => Ok(Self::ThaiNumbers),
            "thaiCounting" => Ok(Self::ThaiCounting),
            "taiwaneseCountingThousand" => Ok(Self::TaiwaneseCountingThousand),
            "taiwaneseDigital" => Ok(Self::TaiwaneseDigital),
            "taiwaneseCounting" => Ok(Self::TaiwaneseCounting),
            "vietnameseCounting" => Ok(Self::VietnameseCounting),
            "numberInDash" => Ok(Self::NumberInDash),
            "russianLower" => Ok(Self::RussianLower),
            "russianUpper" => Ok(Self::RussianUpper),
            other => Err(format!("unknown NumberFormat: {other:?}")),
        }
    }

    pub fn to_xml_str(&self) -> &'static str {
        match self {
            Self::Decimal => "decimal",
            Self::UpperRoman => "upperRoman",
            Self::LowerRoman => "lowerRoman",
            Self::UpperLetter => "upperLetter",
            Self::LowerLetter => "lowerLetter",
            Self::Ordinal => "ordinal",
            Self::CardinalText => "cardinalText",
            Self::OrdinalText => "ordinalText",
            Self::Chicago => "chicago",
            Self::Bullet => "bullet",
            Self::None => "none",
            Self::DecimalZero => "decimalZero",
            Self::DecimalEnclosedCircle => "decimalEnclosedCircle",
            Self::IdeographDigital => "ideographDigital",
            Self::IdeographTraditional => "ideographTraditional",
            Self::IdeographLegalTraditional => "ideographLegalTraditional",
            Self::IdeographZodiac => "ideographZodiac",
            Self::IdeographZodiacTraditional => "ideographZodiacTraditional",
            Self::JapaneseCounting => "japaneseCounting",
            Self::JapaneseDigitalTenThousand => "japaneseDigitalTenThousand",
            Self::JapaneseLegal => "japaneseLegal",
            Self::ChineseCounting => "chineseCounting",
            Self::ChineseCountingThousand => "chineseCountingThousand",
            Self::ChineseLegalSimplified => "chineseLegalSimplified",
            Self::KoreanCounting => "koreanCounting",
            Self::KoreanDigital => "koreanDigital",
            Self::KoreanDigital2 => "koreanDigital2",
            Self::KoreanLegal => "koreanLegal",
            Self::Hebrew1 => "hebrew1",
            Self::Hebrew2 => "hebrew2",
            Self::ArabicAlpha => "arabicAlpha",
            Self::ArabicAbjad => "arabicAbjad",
            Self::HindiVowels => "hindiVowels",
            Self::HindiConsonants => "hindiConsonants",
            Self::HindiNumbers => "hindiNumbers",
            Self::HindiCounting => "hindiCounting",
            Self::ThaiLetters => "thaiLetters",
            Self::ThaiNumbers => "thaiNumbers",
            Self::ThaiCounting => "thaiCounting",
            Self::TaiwaneseCountingThousand => "taiwaneseCountingThousand",
            Self::TaiwaneseDigital => "taiwaneseDigital",
            Self::TaiwaneseCounting => "taiwaneseCounting",
            Self::VietnameseCounting => "vietnameseCounting",
            Self::NumberInDash => "numberInDash",
            Self::RussianLower => "russianLower",
            Self::RussianUpper => "russianUpper",
        }
    }
}

/// Section-level footnote or endnote properties (§17.11.3 / §17.11.2).
///
/// Overrides document-level defaults for note positioning, numbering format,
/// starting number, and restart behavior within a section.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct NoteProperties {
    /// Position (ST_FtnPos / ST_EdnPos).
    pub position: Option<NotePosition>,
    /// Number format (ST_NumberFormat §17.18.59).
    pub num_fmt: Option<NumberFormat>,
    /// Starting number (w:numStart w:val).
    pub num_start: Option<u32>,
    /// Restart behavior (ST_RestartNumber §17.18.60).
    pub num_restart: Option<RestartRule>,
}

/// Structured section properties parsed from w:sectPr (§17.6).
///
/// Page size comes from w:pgSz (§17.6.14), columns from w:cols (§17.6.4).
/// All dimension values are in twips (1/1440 inch).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct SectionProperties {
    /// Page width from w:pgSz w:w.
    pub page_width: Option<u32>,
    /// Page height from w:pgSz w:h.
    pub page_height: Option<u32>,
    /// Page orientation from w:pgSz w:orient.
    pub orientation: Option<PageOrientation>,
    /// Number of columns from w:cols w:num.
    pub columns: Option<u32>,
    /// Space between columns from w:cols w:space (twips).
    pub column_space: Option<u32>,
    /// Individual column definitions from w:cols/w:col (MS-OI29500 §17.6.3/§17.6.4).
    /// Present when equalWidth=false and individual col elements specify per-column widths.
    pub column_defs: Vec<ColumnDef>,

    // ── Page margins (§17.6.11) ──
    /// Top margin from w:pgMar w:top (twips).
    pub margin_top: Option<i32>,
    /// Bottom margin from w:pgMar w:bottom (twips).
    pub margin_bottom: Option<i32>,
    /// Left margin from w:pgMar w:left (twips).
    pub margin_left: Option<i32>,
    /// Right margin from w:pgMar w:right (twips).
    pub margin_right: Option<i32>,
    /// Header distance from w:pgMar w:header (twips).
    pub header_distance: Option<u32>,
    /// Footer distance from w:pgMar w:footer (twips).
    pub footer_distance: Option<u32>,
    /// Gutter from w:pgMar w:gutter (twips).
    pub gutter: Option<u32>,
    /// RTL gutter flag from w:rtlGutter (§17.6.15).
    pub rtl_gutter: Option<bool>,

    // ── Section type (§17.6.17) ──
    /// Section type from w:type w:val.
    pub section_type: Option<SectionType>,

    // ── Page borders (§17.6.7) ──
    /// Page borders from w:pgBorders.
    pub page_borders: Option<PageBorders>,

    // ── Line numbering (§17.6.8) ──
    /// Line numbering from w:lnNumType.
    pub line_numbering: Option<LineNumbering>,

    // ── Vertical alignment (§17.6.20) ──
    /// Vertical alignment from w:vAlign w:val.
    pub v_align: Option<SectionVAlign>,

    /// Text direction from w:textDirection w:val (§17.6.19).
    pub text_direction: Option<TextDirection>,

    // ── Page number type (§17.6.12) ──
    /// Page number type from w:pgNumType.
    pub page_number_type: Option<PageNumberType>,

    // ── Document grid (§17.6.5) ──
    /// Document grid type from w:docGrid w:type.
    pub doc_grid_type: Option<DocGridType>,
    /// Line pitch from w:docGrid w:linePitch (twips).
    pub doc_grid_line_pitch: Option<u32>,
    /// Character space from w:docGrid w:charSpace (twips).
    pub doc_grid_char_space: Option<u32>,

    // ── Boolean section flags ──
    /// Distinct first-page header/footer from w:titlePg (§17.6.18).
    pub title_page: Option<bool>,
    /// Right-to-left section layout from w:bidi (§17.6.1).
    pub bidi: Option<bool>,
    /// Section-level form protection from w:formProt (§17.6.6).
    pub form_prot: Option<bool>,
    /// Suppress endnotes in this section from w:noEndnote (§17.6.9).
    pub no_endnote: Option<bool>,

    // ── Paper size code (§17.6.14) ──
    /// Standard paper size code from w:pgSz w:code.
    pub paper_size_code: Option<i64>,

    // ── Column separator (§17.6.4) ──
    /// Draw vertical separator between columns from w:cols w:sep.
    pub column_separator: Option<bool>,

    /// w:cols w:equalWidth (§17.6.4). Word defaults this to true, so when an
    /// authored doc sets equalWidth="0" (unequal columns defined by per-col
    /// widths) it MUST be re-emitted on a sectPr rebuild — otherwise Word reverts
    /// the section to equal-width columns (MS-OI29500 §2.1.213).
    pub equal_width: Option<bool>,

    // ── Note properties (§17.11.3 / §17.11.2) ──
    /// Section-level footnote properties from w:footnotePr.
    pub footnote_pr: Option<NoteProperties>,
    /// Section-level endnote properties from w:endnotePr.
    pub endnote_pr: Option<NoteProperties>,

    // ── Header/footer references (§17.10.2 / §17.10.5) ──
    /// Effective header references for this section (own declarations merged
    /// with inherited refs from the preceding section). Empty when the section
    /// has no effective headers.
    pub header_refs: Vec<StoryRef>,
    /// Effective footer references (same inheritance semantics as headers).
    pub footer_refs: Vec<StoryRef>,

    // ── Paper source (§17.6.9) ──
    /// Printer tray codes from w:paperSrc. None = element absent.
    pub paper_source: Option<PaperSource>,

    // ── Printer settings reference (§17.6.14) ──
    /// rId of the printer settings part from w:printerSettings r:id.
    /// Stored verbatim — references survive package roundtrip via the
    /// untyped relationship carry-over.
    pub printer_settings_rid: Option<String>,
}

/// Printer tray codes for first vs subsequent pages (§17.6.9).
/// Both attributes are ST_DecimalNumber and default to 1 (auto-select)
/// when omitted; we preserve the omitted/present distinction for fidelity.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PaperSource {
    /// First-page printer tray code (w:first).
    pub first: Option<i64>,
    /// Non-first-page printer tray code (w:other).
    pub other: Option<i64>,
}

/// Individual column definition from w:cols/w:col (MS-OI29500 §17.6.3).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ColumnDef {
    /// Column width in twips.
    pub width: u32,
    /// Space after this column in twips (defaults to 0 per MS-OI29500 §17.6.3).
    pub space: u32,
}

/// Page border properties from w:pgBorders (§17.6.7).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PageBorders {
    pub top: Option<Border>,
    pub bottom: Option<Border>,
    pub left: Option<Border>,
    pub right: Option<Border>,
    /// Z-order of borders relative to text. Defaults to "front" per MS-OI29500 §17.6.10.
    pub z_order: String,
    /// Whether border offset is measured from text or page edge. Defaults to "text".
    pub offset_from: String,
}

/// Line numbering properties from w:lnNumType (§17.6.8).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct LineNumbering {
    pub count_by: Option<u32>,
    /// Raw `start` attribute value from the XML.
    /// MS-OI29500 §17.6.8: Word interprets this as a skip count, not a starting
    /// number. Use `display_start()` for the MS-compatible rendering value.
    pub start: Option<u32>,
    pub restart: Option<String>,
    /// Distance between text margin and line numbers from w:lnNumType w:distance (§17.6.8), in twips.
    pub distance: Option<u32>,
}

impl LineNumbering {
    /// MS-compatible display start value.
    ///
    /// Per MS-OI29500 §17.6.8, Word interprets the `start` attribute as a skip
    /// count: `start=3` means skip the first 3 lines and begin numbering at line 4.
    /// This returns `start + 1` when set, or `1` when absent.
    pub fn display_start(&self) -> u32 {
        self.start.map(|s| s + 1).unwrap_or(1)
    }
}

/// Page number type properties from w:pgNumType (§17.6.12).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PageNumberType {
    pub fmt: Option<String>,
    pub start: Option<u32>,
    /// Heading style level for chapter numbering from w:chapStyle (§17.6.12).
    pub chap_style: Option<i64>,
    /// Chapter-page separator from w:chapSep (§17.6.12). E.g., "hyphen", "period", "colon".
    pub chap_sep: Option<String>,
}

/// Paragraph indentation values in twips (1/1440 inch).
///
/// These values represent the **OOXML style cascade result** (§17.3.1.12),
/// resolved via direct > numbering > style per-attribute merge.
///
/// On the edit/round-trip model, `effective_first_line_twips` is always the raw
/// cascade value — prefix stripping does not change it. In the **render
/// projection** ([`FullDocBlock::indent`]) it is the *resolved first-line
/// origin*: when a literal-prefix marker is positioned by a leading tab, that
/// tab's resolved landing is folded in here, so a render consumer applies a
/// single `text-indent` (from `effective_first_line_twips`) to position the
/// whole first line (prefix in `::before` + body) — no separate leading-tab
/// field to combine, and nothing to double-count.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Indentation {
    /// Left indentation in twips (§17.3.1.12 w:left).
    /// Continuation lines start at this position.
    pub left: Option<i32>,
    /// Right indentation in twips (§17.3.1.12 w:right).
    pub right: Option<i32>,
    /// First-line indent in twips relative to `left` (§17.3.1.12 w:firstLine / w:hanging).
    /// Positive = indent right of `left`, negative = hanging (first line left of `left`).
    /// The first line of text starts at `left + effective_first_line_twips`.
    pub effective_first_line_twips: Option<i32>,
    /// Left/start indent in character units (hundredths of a character width).
    /// A non-zero value takes precedence over twip `left` (MS-OI29500 2.1.44).
    /// `Some(0)` is distinct from `None`: an explicit `leftChars="0"` is a real
    /// override that cancels a character indent inherited from a style or
    /// numbering (2.1.44a), so it is preserved and re-emitted verbatim.
    pub start_chars: Option<i32>,
    /// Right/end indent in character units. Same precedence and explicit-zero
    /// semantics as `start_chars` (MS-OI29500 2.1.44).
    pub end_chars: Option<i32>,
    /// First line indent in character units.
    /// Non-zero value takes precedence over twip `first_line` (MS-OI29500 2.1.44).
    pub first_line_chars: Option<i32>,
    /// Hanging indent in character units.
    /// Non-zero value takes precedence over twip hanging (MS-OI29500 2.1.44).
    pub hanging_chars: Option<i32>,
}

/// Paragraph spacing values (§17.3.1.33).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ParagraphSpacing {
    /// Space before paragraph in twips.
    pub before: Option<u32>,
    /// Space after paragraph in twips.
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
    /// Line spacing value (interpretation depends on `line_rule`).
    pub line: Option<u32>,
    /// How to interpret the `line` value.
    pub line_rule: Option<LineSpacingRule>,
}

/// How to interpret the w:line value in paragraph spacing (§17.3.1.33).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum LineSpacingRule {
    /// Line value is in 240ths of a line (240 = single, 480 = double).
    Auto,
    /// Line value is in twips (exact height — text may be clipped).
    Exact,
    /// Line value is in twips (minimum height — expands if needed).
    AtLeast,
}

/// Paragraph borders from w:pBdr (§17.3.1.24).
///
/// Has up to 6 edges: top, bottom, left, right, between (between adjacent
/// paragraphs with same border settings), and bar (vertical bar to the side).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ParagraphBorders {
    pub top: Option<Border>,
    pub bottom: Option<Border>,
    pub left: Option<Border>,
    pub right: Option<Border>,
    /// Border between adjacent paragraphs with the same border set.
    pub between: Option<Border>,
    /// Vertical bar border drawn to the side of the paragraph.
    pub bar: Option<Border>,
}

/// Numbering information for a paragraph using Word auto-numbering.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct NumberingInfo {
    pub num_id: u32,
    pub ilvl: u32,
    /// The synthesized number text (e.g., "1.", "(a)").
    pub synthesized_text: String,
    /// True when the numbering format is bullet (numFmt="bullet").
    /// Bullet numbering has no counter, so its prefix never shifts
    /// when paragraphs are inserted/deleted.
    pub is_bullet: bool,
    /// Pending numbering restart request: at serialize time, a new `w:num`
    /// instance with `w:lvlOverride/w:startOverride val="1"` is allocated
    /// and `num_id` is remapped to the fresh instance. Implements the
    /// `restart_numbering` field of the LLM edit schema's insert op kind.
    /// The serializer clears this flag once the override has been
    /// materialized.
    /// NOTE: no `#[serde(skip_serializing_if)]` here — runtime snapshots use
    /// bincode, which serializes structs positionally. Omitting a field on the
    /// false case corrupts snapshot roundtrips for numbered paragraphs.
    #[serde(default)]
    pub restart_numbering: bool,
}

impl NumberingInfo {
    /// Whether two `NumberingInfo` reference the same structural numbering
    /// (same `num_id` and `ilvl`).  Ignores `synthesized_text`, which is a
    /// derived counter value that drifts when list items are added/removed.
    pub fn structurally_eq(&self, other: &Self) -> bool {
        self.num_id == other.num_id && self.ilvl == other.ilvl
    }
}

/// Compare two `Option<NumberingInfo>` structurally (num_id + ilvl only).
pub fn numbering_structurally_eq(a: &Option<NumberingInfo>, b: &Option<NumberingInfo>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.structurally_eq(b),
        (None, None) => true,
        _ => false,
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum HeadingLevel {
    H1,
    H2,
    H3,
    H4,
    H5,
    H6,
    H7,
    H8,
    H9,
}

impl HeadingLevel {
    /// Convert a numeric heading level (1-9) to HeadingLevel.
    /// Levels outside 1-9 are clamped to the nearest valid level.
    pub fn from_number(n: u8) -> Self {
        match n {
            0 | 1 => HeadingLevel::H1,
            2 => HeadingLevel::H2,
            3 => HeadingLevel::H3,
            4 => HeadingLevel::H4,
            5 => HeadingLevel::H5,
            6 => HeadingLevel::H6,
            7 => HeadingLevel::H7,
            8 => HeadingLevel::H8,
            _ => HeadingLevel::H9,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TableNode {
    pub id: NodeId,
    pub rows: Vec<TableRowNode>,
    /// Hash of table structure (row count, column layout, merge info).
    /// Used for quick structure comparison during diffing.
    pub structure_hash: String,
    /// Table-level formatting (borders, widths, grid columns).
    pub formatting: TableFormatting,
    /// Tracked table formatting change from w:tblPrChange (§17.13.5.34).
    pub formatting_change: Option<TableFormattingChange>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TableRowNode {
    pub id: NodeId,
    pub cells: Vec<TableCellNode>,
    /// Number of empty grid columns before the first cell (from w:gridBefore).
    pub grid_before: u32,
    /// Number of empty grid columns after the last cell (from w:gridAfter).
    pub grid_after: u32,
    /// Tracked change status from w:trPr (w:ins or w:del).
    /// Indicates if this row was natively tracked as inserted/deleted in Word.
    pub tracking_status: Option<TrackingStatus>,
    /// Whether this row repeats as a header on each page (from w:tblHeader).
    pub is_header: bool,
    /// Row height in twips (from w:trHeight w:val).
    pub height: Option<u32>,
    /// Row height rule (from w:trHeight w:hRule) per §17.18.37 `ST_HeightRule`.
    pub height_rule: Option<HeightRule>,
    /// Tracked row formatting change from w:trPrChange (§17.13.5.36).
    pub formatting_change: Option<RowFormattingChange>,
    /// Row ID from w14:paraId (hex string, used by GDocs/Word for identity).
    /// MS-DOCX §2.2.4: applies to both w:p and w:tr elements.
    pub para_id: Option<String>,
    /// Text ID from w14:textId (hex string).
    pub text_id: Option<String>,
    /// Whether this row may not be split across pages (from w:cantSplit, §17.4.6).
    #[serde(default)]
    pub cant_split: bool,
    /// Row-level table justification (from w:jc in trPr, §17.4.28).
    #[serde(default)]
    pub jc: Option<Alignment>,
    /// Preferred width of the gridBefore empty span (from w:wBefore, §17.4.86).
    #[serde(default)]
    pub w_before: Option<TableMeasurement>,
    /// Preferred width of the gridAfter empty span (from w:wAfter, §17.4.85).
    #[serde(default)]
    pub w_after: Option<TableMeasurement>,
    /// Conditional formatting flags for this row (from w:cnfStyle in trPr, §17.4.7).
    #[serde(default)]
    pub cnf_style: Option<CnfStyle>,
    /// Row-level table property exceptions (from w:tblPrEx, §17.4.61).
    /// Per-row overrides of table-level properties (borders, shading, cell
    /// margins, justification, layout). Only the CT_TblPrEx subset of
    /// TableFormatting is meaningful here.
    #[serde(default)]
    pub tbl_pr_ex: Option<TableFormatting>,
    /// Row-level cell spacing (from w:tblCellSpacing w:w in trPr, §17.4.44),
    /// in twips. Mirrors `TableFormatting::cell_spacing` (type assumed dxa).
    /// Without this a row whose only trPr child is tblCellSpacing loses its
    /// entire trPr on reserialization (state-3 loss).
    #[serde(default)]
    pub cell_spacing: Option<i64>,
    /// Verbatim round-trip of any trPr child the typed fields above do not
    /// consume — `w:divId`, `w:hidden`, and vendor/foreign extensions. RFC-0003
    /// "never silently drop" catch-all (captured via `TRPR_CONSUMED`).
    #[serde(default)]
    pub preserved: Vec<PreservedProp>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TableCellNode {
    pub id: NodeId,
    pub blocks: Vec<BlockNode>,
    /// Horizontal span (from w:gridSpan), default 1.
    pub grid_span: u32,
    /// Vertical merge state (from w:vMerge).
    pub v_merge: VerticalMerge,
    /// Cell-level formatting (borders, shading, width, alignment).
    pub formatting: CellFormatting,
    /// Tracked cell formatting change from w:tcPrChange (§17.13.5.37).
    pub formatting_change: Option<CellFormattingChange>,
    /// Tracked change status (from w:cellIns / w:cellDel in tcPr).
    pub tracking_status: Option<TrackingStatus>,
    /// SDT wrapper around this cell at the row level (§17.5.2).
    /// When present, the cell was originally wrapped in `w:sdt` inside the row.
    pub row_sdt_wrapper: Option<SdtWrapper>,
    /// Block-level `w:sdt` wrappers around ranges of this cell's blocks
    /// (§17.5.2), in document order. Each entry records WHERE its wrap starts in
    /// `blocks` and HOW MANY consecutive blocks its `w:sdtContent` encloses, so
    /// a following sibling block is never re-nested inside the control on export
    /// (which made Word repair the file). Replaces an earlier single whole-cell
    /// wrapper that could not express a sub-cell span. See [`CellSdtWrap`].
    #[serde(default)]
    pub content_sdt_wraps: Vec<CellSdtWrap>,
    /// Conditional formatting flags for this cell (from w:cnfStyle in tcPr, §17.4.7).
    #[serde(default)]
    pub cnf_style: Option<CnfStyle>,
    /// Whether the end-of-cell mark in this cell is hidden (from w:hideMark, §17.4.10).
    /// hideMark is a valid child of both CT_TrPr and CT_TcPr; this is the cell case.
    #[serde(default)]
    pub hide_mark: bool,
    /// Verbatim round-trip of any tcPr child the typed fields above do not
    /// consume — legacy `w:hMerge`, and vendor/foreign extensions (e.g.
    /// `tm:tmTcPr`). RFC-0003 "never silently drop" catch-all (captured via
    /// `TCPR_CONSUMED`).
    #[serde(default)]
    pub preserved: Vec<PreservedProp>,
}

/// Preserved SDT (structured document tag / content control) wrapper.
/// Stores the raw XML of `w:sdtPr` and `w:sdtEndPr` so the wrapper can be
/// reconstructed during serialization without understanding every property type.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SdtWrapper {
    /// Raw XML bytes of the `w:sdtPr` element.
    pub sdt_pr_xml: Vec<u8>,
    /// Raw XML bytes of the `w:sdtEndPr` element, if present.
    pub sdt_end_pr_xml: Option<Vec<u8>>,
}

/// The control type of a structured document tag (`w:sdt`, §17.5.2). This is the
/// kind discriminator inside `w:sdtPr` that tells Word which content-control UI
/// to present. Authored deterministically by the `WrapInContentControl` verb;
/// an unknown control is never defaulted (CLAUDE.md "no silent fallbacks").
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum SdtControl {
    /// Plain-text control (`w:text`, §17.5.2.36).
    PlainText,
    /// Rich-text control (the absence of a specific control kind in `w:sdtPr`).
    RichText,
    /// Drop-down list (`w:dropDownList`, §17.5.2.15) — selection only.
    Dropdown { items: Vec<SdtListItem> },
    /// Combo box (`w:comboBox`, §17.5.2.6) — selection or free text.
    ComboBox { items: Vec<SdtListItem> },
    /// Checkbox (`w14:checkbox`, MS-DOCX §2.5.2.4) — a Word 2010 extension.
    Checkbox { checked: bool },
    /// Date picker (`w:date`, §17.5.2.7).
    Date,
    /// Repeating section (`w15:repeatingSection`, MS-DOCX) — a Word 2013 extension.
    RepeatingSection,
}

/// One entry in a drop-down list / combo box content control
/// (`w:listItem` @displayText/@value, §17.5.2.20).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SdtListItem {
    /// The text shown in the control's drop-down (`w:displayText`).
    pub display: String,
    /// The stored value selected (`w:value`).
    pub value: String,
}

/// Vertical merge state for a table cell.
///
/// In DOCX, vertical merging is represented by:
/// - `<w:vMerge w:val="restart"/>`: Start of a vertical merge (anchor cell)
/// - `<w:vMerge/>` or `<w:vMerge w:val="continue"/>`: Continuation of merge
/// - No vMerge element: No vertical merge
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub enum VerticalMerge {
    /// No vertical merge (default).
    #[default]
    None,
    /// Start of a vertical merge (this cell is the anchor).
    Restart,
    /// Continuation of a vertical merge from the cell above.
    Continue,
}

// =============================================================================
// Table Formatting Properties
// =============================================================================

/// Table-level formatting properties from w:tblPr.
///
/// The `has_direct_*` flags are parse-time provenance (same doctrine as
/// RunRprAuthored / the paragraph flags): the VALUE fields hold the RESOLVED
/// effective formatting for projections, while the serializer emits a slot as
/// direct tblPr only when the table's own tblPr authored it. `Default` sets
/// the flags TRUE (present == authored) so authoring paths (edit verbs,
/// tblPrEx, tests) emit what they construct; the importer overrides them with
/// real provenance.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TableFormatting {
    /// Table style reference (from w:tblStyle w:val, §17.4.60).
    pub style_id: Option<IStr>,
    /// Table look flags for conditional formatting (from w:tblLook, §17.4.56).
    pub tbl_look: Option<TblLook>,
    /// Table borders (from w:tblBorders).
    pub borders: Option<BorderSet>,
    /// Table width (from w:tblW).
    pub width: Option<TableMeasurement>,
    /// Column widths from w:tblGrid/w:gridCol (in twips).
    pub grid_cols: Vec<u32>,
    /// Default cell margins from w:tblCellMar (twips).
    pub default_cell_margins: Option<CellMargins>,
    /// Table alignment from w:jc (§17.4.28).
    pub alignment: Option<Alignment>,
    /// Table indent from w:tblInd w:w (§17.4.51), in twips.
    pub indent: Option<i32>,
    /// Table layout algorithm from w:tblLayout w:type (§17.4.52).
    pub layout: Option<TableLayout>,
    /// Cell spacing from w:tblCellSpacing w:w (§17.4.44), in twips.
    pub cell_spacing: Option<i64>,
    /// Floating table positioning from w:tblpPr (§17.4.57).
    pub positioning: Option<TablePositioning>,
    /// Table overlap setting from w:tblOverlap w:val (§17.4.55).
    pub overlap: Option<TableOverlap>,
    /// Row band size from w:tblStyleRowBandSize w:val (§17.4.79).
    pub row_band_size: Option<u32>,
    /// Column band size from w:tblStyleColBandSize w:val (§17.4.78).
    pub col_band_size: Option<u32>,
    /// Parse-time provenance: the table's own tblPr carried w:tblBorders.
    #[serde(default = "serde_true")]
    pub has_direct_borders: bool,
    /// w:tblCellMar authored on the table's own tblPr.
    #[serde(default = "serde_true")]
    pub has_direct_cell_margins: bool,
    /// w:jc authored on the table's own tblPr.
    #[serde(default = "serde_true")]
    pub has_direct_alignment: bool,
    /// w:tblInd authored on the table's own tblPr.
    #[serde(default = "serde_true")]
    pub has_direct_indent: bool,
    /// w:tblLook authored on the table's own tblPr (parse_tbl_look returns the
    /// MS 0x04A0 default for ABSENT elements, which must not be injected).
    #[serde(default = "serde_true")]
    pub has_direct_tbl_look: bool,
    /// Table-level shading (from w:shd, §17.4.32). Distinct from cell shading;
    /// paints the whole table background. RFC-0003 EDIT property.
    #[serde(default)]
    pub shading: Option<Shading>,
    /// Right-to-left visual column order (from w:bidiVisual, §17.4.1). When set,
    /// Word lays the columns out right-to-left. RFC-0003 KEEP property.
    #[serde(default)]
    pub bidi_visual: bool,
    /// Accessibility caption (from w:tblCaption, §17.4.42). RFC-0003 KEEP.
    #[serde(default)]
    pub caption: Option<String>,
    /// Accessibility description (from w:tblDescription, §17.4.46). RFC-0003 KEEP.
    #[serde(default)]
    pub description: Option<String>,
    /// Verbatim round-trip of any tblPr child the typed fields above do not
    /// model — vendor extensions (o:*, tm:*) and future OOXML additions. The
    /// RFC-0003 "never silently drop" catch-all: an unmodeled child is captured
    /// as a [`PreservedProp`] and re-emitted, not dropped. Reused for tblPrEx
    /// (which shares this struct).
    #[serde(default)]
    pub preserved: Vec<PreservedProp>,
}

impl Default for TableFormatting {
    fn default() -> Self {
        Self {
            style_id: None,
            tbl_look: None,
            borders: None,
            width: None,
            grid_cols: Vec::new(),
            default_cell_margins: None,
            alignment: None,
            indent: None,
            layout: None,
            cell_spacing: None,
            positioning: None,
            overlap: None,
            row_band_size: None,
            col_band_size: None,
            // Present == authored for non-import construction (see struct docs).
            has_direct_borders: true,
            has_direct_cell_margins: true,
            has_direct_alignment: true,
            has_direct_indent: true,
            has_direct_tbl_look: true,
            shading: None,
            bidi_visual: false,
            caption: None,
            description: None,
            preserved: Vec::new(),
        }
    }
}

/// Floating table positioning from w:tblpPr (§17.4.58, CT_TblPPr).
///
/// The modeled fields below cover the anchors, the absolute offsets, the
/// distance-from-text clearances, and the relative alignment specs; any
/// CT_TblPPr attribute not modeled here (plus anything a future schema adds) is
/// captured verbatim in `extra_attrs` at the extraction edge and re-emitted
/// unchanged (RFC-0003 attribute-level remainder). Nothing on `w:tblpPr` is
/// ever silently dropped.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TablePositioning {
    /// Vertical anchor per §17.18.100 ST_VAnchor.
    pub vert_anchor: Option<VAnchor>,
    /// Horizontal anchor per §17.18.35 ST_HAnchor.
    pub horz_anchor: Option<HAnchor>,
    /// Vertical offset in twips (`w:tblpY`).
    pub tblp_y: Option<i64>,
    /// Horizontal offset in twips (`w:tblpX`).
    pub tblp_x: Option<i64>,
    /// Distance from surrounding text to the left of the table, in twips
    /// (`w:leftFromText`, ST_TwipsMeasure).
    #[serde(default)]
    pub left_from_text: Option<i64>,
    /// Distance from surrounding text to the right of the table, in twips
    /// (`w:rightFromText`, ST_TwipsMeasure).
    #[serde(default)]
    pub right_from_text: Option<i64>,
    /// Distance from surrounding text above the table, in twips
    /// (`w:topFromText`, ST_TwipsMeasure).
    #[serde(default)]
    pub top_from_text: Option<i64>,
    /// Distance from surrounding text below the table, in twips
    /// (`w:bottomFromText`, ST_TwipsMeasure).
    #[serde(default)]
    pub bottom_from_text: Option<i64>,
    /// Relative horizontal alignment (`w:tblpXSpec`, ST_XAlign); alternative to
    /// the absolute `tblp_x`.
    #[serde(default)]
    pub tblp_x_spec: Option<XAlign>,
    /// Relative vertical alignment (`w:tblpYSpec`, ST_YAlign); alternative to
    /// the absolute `tblp_y`.
    #[serde(default)]
    pub tblp_y_spec: Option<YAlign>,
    /// CT_TblPPr attributes not modeled above, captured and re-emitted verbatim.
    #[serde(default)]
    pub extra_attrs: Vec<(String, String)>,
}

/// Flags from w:tblLook controlling which conditional formatting conditions apply (§17.4.56).
///
/// MS-OI29500 §17.4.55(a): When tblLook is omitted, Word defaults to 0x04A0:
/// firstRow=true, firstColumn=true, noVBand=true (all others false).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TblLook {
    pub first_row: bool,
    pub last_row: bool,
    pub first_column: bool,
    pub last_column: bool,
    pub no_h_band: bool,
    pub no_v_band: bool,
    /// Raw w:val hex string for roundtrip fidelity.
    pub val: Option<String>,
}

impl Default for TblLook {
    /// MS-OI29500 §17.4.55(a): When tblLook is omitted, Word defaults to 0x04A0.
    /// 0x04A0 = firstRow(0x0020) | firstColumn(0x0080) | noVBand(0x0400).
    fn default() -> Self {
        TblLook {
            first_row: true,
            last_row: false,
            first_column: true,
            last_column: false,
            no_h_band: false,
            no_v_band: true,
            val: None,
        }
    }
}

/// Cell-level formatting properties from w:tcPr (beyond merge attributes).
///
/// `has_direct_*` flags: parse-time provenance (see TableFormatting docs) —
/// values hold the RESOLVED effective formatting (table-style conditional
/// banding, border-conflict resolution) for projections; the serializer emits
/// direct tcPr only for authored slots. Default = true (present == authored).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CellFormatting {
    /// Cell borders (from w:tcBorders).
    pub borders: Option<BorderSet>,
    /// Cell shading (from w:shd).
    pub shading: Option<Shading>,
    /// Cell width (from w:tcW).
    pub width: Option<TableMeasurement>,
    /// Vertical alignment (from w:vAlign).
    pub v_align: Option<VerticalAlignment>,
    /// Per-cell margin overrides from w:tcMar (twips).
    pub margins: Option<CellMargins>,
    /// No text wrapping in cell (from w:noWrap, §17.4.30).
    pub no_wrap: Option<bool>,
    /// Text direction in cell (from w:textDirection, §17.4.72).
    pub text_direction: Option<TextDirection>,
    /// Fit text to cell width (from w:tcFitText, §17.4.63).
    pub tc_fit_text: Option<bool>,
    /// Parse-time provenance: the cell's own tcPr carried w:tcBorders.
    #[serde(default = "serde_true")]
    pub has_direct_borders: bool,
    /// The w:tcBorders exactly as authored on this cell's own tcPr, captured
    /// before any resolution (§17.4.39). `borders` above holds the RESOLVED
    /// effective set (table cascade + adjacent-cell conflicts) for projections;
    /// `authored_borders` is what the serializer emits, so an edge the author
    /// deliberately omitted (deferring to table/neighbor resolution) stays
    /// absent on round-trip instead of being synthesized as a visible line.
    /// `None` ⇒ not captured separately, i.e. `borders` is itself the authored
    /// set (non-import construction, where present == authored). Only meaningful
    /// when `has_direct_borders` is true; ignored otherwise.
    #[serde(default)]
    pub authored_borders: Option<BorderSet>,
    /// w:shd authored on the cell's own tcPr.
    #[serde(default = "serde_true")]
    pub has_direct_shading: bool,
}

impl Default for CellFormatting {
    fn default() -> Self {
        Self {
            borders: None,
            shading: None,
            width: None,
            v_align: None,
            margins: None,
            no_wrap: None,
            text_direction: None,
            tc_fit_text: None,
            // Present == authored for non-import construction (see struct docs).
            has_direct_borders: true,
            authored_borders: None,
            has_direct_shading: true,
        }
    }
}

/// Cell margins in twips (1/1440 inch).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct CellMargins {
    pub top: Option<u32>,
    pub bottom: Option<u32>,
    pub left: Option<u32>,
    pub right: Option<u32>,
}

/// A set of border edges.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct BorderSet {
    pub top: Option<Border>,
    pub bottom: Option<Border>,
    pub left: Option<Border>,
    pub right: Option<Border>,
    /// Horizontal inside border (table-level only).
    pub inside_h: Option<Border>,
    /// Vertical inside border (table-level only).
    pub inside_v: Option<Border>,
}

/// Border style values per OOXML §17.18.2 `ST_Border`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub enum BorderStyle {
    None,
    Single,
    Thick,
    Double,
    Dashed,
    Dotted,
    DotDash,
    DotDotDash,
    Triple,
    ThinThickSmallGap,
    ThickThinSmallGap,
    ThinThickThinSmallGap,
    ThinThickMediumGap,
    ThickThinMediumGap,
    ThinThickThinMediumGap,
    ThinThickLargeGap,
    ThickThinLargeGap,
    ThinThickThinLargeGap,
    Wave,
    DoubleWave,
    DashSmallGap,
    DashDotStroked,
    ThreeDEmboss,
    ThreeDEngrave,
    Outset,
    Inset,
    Nil,
}

impl BorderStyle {
    pub fn from_xml_str(s: &str) -> Result<Self, String> {
        match s {
            "none" => Ok(Self::None),
            "single" => Ok(Self::Single),
            "thick" => Ok(Self::Thick),
            "double" => Ok(Self::Double),
            "dashed" => Ok(Self::Dashed),
            "dotted" => Ok(Self::Dotted),
            "dotDash" => Ok(Self::DotDash),
            "dotDotDash" => Ok(Self::DotDotDash),
            "triple" => Ok(Self::Triple),
            "thinThickSmallGap" => Ok(Self::ThinThickSmallGap),
            "thickThinSmallGap" => Ok(Self::ThickThinSmallGap),
            "thinThickThinSmallGap" => Ok(Self::ThinThickThinSmallGap),
            "thinThickMediumGap" => Ok(Self::ThinThickMediumGap),
            "thickThinMediumGap" => Ok(Self::ThickThinMediumGap),
            "thinThickThinMediumGap" => Ok(Self::ThinThickThinMediumGap),
            "thinThickLargeGap" => Ok(Self::ThinThickLargeGap),
            "thickThinLargeGap" => Ok(Self::ThickThinLargeGap),
            "thinThickThinLargeGap" => Ok(Self::ThinThickThinLargeGap),
            "wave" => Ok(Self::Wave),
            "doubleWave" => Ok(Self::DoubleWave),
            "dashSmallGap" => Ok(Self::DashSmallGap),
            "dashDotStroked" => Ok(Self::DashDotStroked),
            "threeDEmboss" => Ok(Self::ThreeDEmboss),
            "threeDEngrave" => Ok(Self::ThreeDEngrave),
            "outset" => Ok(Self::Outset),
            "inset" => Ok(Self::Inset),
            "nil" => Ok(Self::Nil),
            other => Err(format!("unknown BorderStyle: {other:?}")),
        }
    }

    pub fn to_xml_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Single => "single",
            Self::Thick => "thick",
            Self::Double => "double",
            Self::Dashed => "dashed",
            Self::Dotted => "dotted",
            Self::DotDash => "dotDash",
            Self::DotDotDash => "dotDotDash",
            Self::Triple => "triple",
            Self::ThinThickSmallGap => "thinThickSmallGap",
            Self::ThickThinSmallGap => "thickThinSmallGap",
            Self::ThinThickThinSmallGap => "thinThickThinSmallGap",
            Self::ThinThickMediumGap => "thinThickMediumGap",
            Self::ThickThinMediumGap => "thickThinMediumGap",
            Self::ThinThickThinMediumGap => "thinThickThinMediumGap",
            Self::ThinThickLargeGap => "thinThickLargeGap",
            Self::ThickThinLargeGap => "thickThinLargeGap",
            Self::ThinThickThinLargeGap => "thinThickThinLargeGap",
            Self::Wave => "wave",
            Self::DoubleWave => "doubleWave",
            Self::DashSmallGap => "dashSmallGap",
            Self::DashDotStroked => "dashDotStroked",
            Self::ThreeDEmboss => "threeDEmboss",
            Self::ThreeDEngrave => "threeDEngrave",
            Self::Outset => "outset",
            Self::Inset => "inset",
            Self::Nil => "nil",
        }
    }
}

/// A single border edge.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Border {
    /// Border style per §17.18.2 `ST_Border`.
    pub style: BorderStyle,
    /// Border color as hex (e.g., "000000", "auto").
    pub color: Option<String>,
    /// Border width in eighths of a point.
    pub size: Option<u32>,
    /// Border offset from page/text margin in points (§17.6.7).
    pub space: Option<u32>,
    /// Verbatim round-trip of any CT_Border attribute the typed fields above
    /// don't model — theme colors (themeColor/themeTint/themeShade), frame,
    /// shadow. RFC-0003 "never silently drop"; `(qualified_name, value)` pairs
    /// captured at the edge and re-emitted.
    #[serde(default)]
    pub extra_attrs: Vec<(String, String)>,
}

/// Underline style per §17.18.99 `ST_Underline`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub enum UnderlineStyle {
    Single,
    Words,
    Double,
    Thick,
    Dotted,
    DottedHeavy,
    Dash,
    DashedHeavy,
    DashLong,
    DashLongHeavy,
    DotDash,
    DashDotHeavy,
    DotDotDash,
    DashDotDotHeavy,
    Wave,
    WavyHeavy,
    WavyDouble,
    None,
}

impl UnderlineStyle {
    pub fn from_xml_str(s: &str) -> Result<Self, String> {
        match s {
            "single" => Ok(Self::Single),
            "words" => Ok(Self::Words),
            "double" => Ok(Self::Double),
            "thick" => Ok(Self::Thick),
            "dotted" => Ok(Self::Dotted),
            "dottedHeavy" => Ok(Self::DottedHeavy),
            "dash" => Ok(Self::Dash),
            "dashedHeavy" => Ok(Self::DashedHeavy),
            "dashLong" => Ok(Self::DashLong),
            "dashLongHeavy" => Ok(Self::DashLongHeavy),
            "dotDash" => Ok(Self::DotDash),
            "dashDotHeavy" => Ok(Self::DashDotHeavy),
            "dotDotDash" => Ok(Self::DotDotDash),
            "dashDotDotHeavy" => Ok(Self::DashDotDotHeavy),
            "wave" => Ok(Self::Wave),
            "wavyHeavy" => Ok(Self::WavyHeavy),
            "wavyDouble" => Ok(Self::WavyDouble),
            "none" => Ok(Self::None),
            other => Err(format!("unknown UnderlineStyle: {other:?}")),
        }
    }

    pub fn to_xml_str(&self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::Words => "words",
            Self::Double => "double",
            Self::Thick => "thick",
            Self::Dotted => "dotted",
            Self::DottedHeavy => "dottedHeavy",
            Self::Dash => "dash",
            Self::DashedHeavy => "dashedHeavy",
            Self::DashLong => "dashLong",
            Self::DashLongHeavy => "dashLongHeavy",
            Self::DotDash => "dotDash",
            Self::DashDotHeavy => "dashDotHeavy",
            Self::DotDotDash => "dotDotDash",
            Self::DashDotDotHeavy => "dashDotDotHeavy",
            Self::Wave => "wave",
            Self::WavyHeavy => "wavyHeavy",
            Self::WavyDouble => "wavyDouble",
            Self::None => "none",
        }
    }
}

/// Highlight color per §17.18.40 `ST_HighlightColor`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub enum HighlightColor {
    Black,
    Blue,
    Cyan,
    Green,
    Magenta,
    Red,
    Yellow,
    White,
    DarkBlue,
    DarkCyan,
    DarkGreen,
    DarkMagenta,
    DarkRed,
    DarkYellow,
    DarkGray,
    LightGray,
    None,
}

impl HighlightColor {
    pub fn from_xml_str(s: &str) -> Result<Self, String> {
        match s {
            "black" => Ok(Self::Black),
            "blue" => Ok(Self::Blue),
            "cyan" => Ok(Self::Cyan),
            "green" => Ok(Self::Green),
            "magenta" => Ok(Self::Magenta),
            "red" => Ok(Self::Red),
            "yellow" => Ok(Self::Yellow),
            "white" => Ok(Self::White),
            "darkBlue" => Ok(Self::DarkBlue),
            "darkCyan" => Ok(Self::DarkCyan),
            "darkGreen" => Ok(Self::DarkGreen),
            "darkMagenta" => Ok(Self::DarkMagenta),
            "darkRed" => Ok(Self::DarkRed),
            "darkYellow" => Ok(Self::DarkYellow),
            "darkGray" => Ok(Self::DarkGray),
            "lightGray" => Ok(Self::LightGray),
            "none" => Ok(Self::None),
            other => Err(format!("unknown HighlightColor: {other:?}")),
        }
    }

    pub fn to_xml_str(&self) -> &'static str {
        match self {
            Self::Black => "black",
            Self::Blue => "blue",
            Self::Cyan => "cyan",
            Self::Green => "green",
            Self::Magenta => "magenta",
            Self::Red => "red",
            Self::Yellow => "yellow",
            Self::White => "white",
            Self::DarkBlue => "darkBlue",
            Self::DarkCyan => "darkCyan",
            Self::DarkGreen => "darkGreen",
            Self::DarkMagenta => "darkMagenta",
            Self::DarkRed => "darkRed",
            Self::DarkYellow => "darkYellow",
            Self::DarkGray => "darkGray",
            Self::LightGray => "lightGray",
            Self::None => "none",
        }
    }
}

/// Shading pattern per §17.18.78 `ST_Shd`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ShadingPattern {
    Nil,
    Clear,
    Solid,
    HorzStripe,
    VertStripe,
    ReverseDiagStripe,
    DiagStripe,
    HorzCross,
    DiagCross,
    ThinHorzStripe,
    ThinVertStripe,
    ThinReverseDiagStripe,
    ThinDiagStripe,
    ThinHorzCross,
    ThinDiagCross,
    Pct5,
    Pct10,
    Pct12,
    Pct15,
    Pct20,
    Pct25,
    Pct30,
    Pct35,
    Pct37,
    Pct40,
    Pct45,
    Pct50,
    Pct55,
    Pct60,
    Pct62,
    Pct65,
    Pct70,
    Pct75,
    Pct80,
    Pct85,
    Pct87,
    Pct90,
    Pct95,
}

impl ShadingPattern {
    pub fn from_xml_str(s: &str) -> Result<Self, String> {
        match s {
            "nil" => Ok(Self::Nil),
            "clear" => Ok(Self::Clear),
            "solid" => Ok(Self::Solid),
            "horzStripe" => Ok(Self::HorzStripe),
            "vertStripe" => Ok(Self::VertStripe),
            "reverseDiagStripe" => Ok(Self::ReverseDiagStripe),
            "diagStripe" => Ok(Self::DiagStripe),
            "horzCross" => Ok(Self::HorzCross),
            "diagCross" => Ok(Self::DiagCross),
            "thinHorzStripe" => Ok(Self::ThinHorzStripe),
            "thinVertStripe" => Ok(Self::ThinVertStripe),
            "thinReverseDiagStripe" => Ok(Self::ThinReverseDiagStripe),
            "thinDiagStripe" => Ok(Self::ThinDiagStripe),
            "thinHorzCross" => Ok(Self::ThinHorzCross),
            "thinDiagCross" => Ok(Self::ThinDiagCross),
            "pct5" => Ok(Self::Pct5),
            "pct10" => Ok(Self::Pct10),
            "pct12" => Ok(Self::Pct12),
            "pct15" => Ok(Self::Pct15),
            "pct20" => Ok(Self::Pct20),
            "pct25" => Ok(Self::Pct25),
            "pct30" => Ok(Self::Pct30),
            "pct35" => Ok(Self::Pct35),
            "pct37" => Ok(Self::Pct37),
            "pct40" => Ok(Self::Pct40),
            "pct45" => Ok(Self::Pct45),
            "pct50" => Ok(Self::Pct50),
            "pct55" => Ok(Self::Pct55),
            "pct60" => Ok(Self::Pct60),
            "pct62" => Ok(Self::Pct62),
            "pct65" => Ok(Self::Pct65),
            "pct70" => Ok(Self::Pct70),
            "pct75" => Ok(Self::Pct75),
            "pct80" => Ok(Self::Pct80),
            "pct85" => Ok(Self::Pct85),
            "pct87" => Ok(Self::Pct87),
            "pct90" => Ok(Self::Pct90),
            "pct95" => Ok(Self::Pct95),
            other => Err(format!("unknown ShadingPattern: {other:?}")),
        }
    }

    pub fn to_xml_str(&self) -> &'static str {
        match self {
            Self::Nil => "nil",
            Self::Clear => "clear",
            Self::Solid => "solid",
            Self::HorzStripe => "horzStripe",
            Self::VertStripe => "vertStripe",
            Self::ReverseDiagStripe => "reverseDiagStripe",
            Self::DiagStripe => "diagStripe",
            Self::HorzCross => "horzCross",
            Self::DiagCross => "diagCross",
            Self::ThinHorzStripe => "thinHorzStripe",
            Self::ThinVertStripe => "thinVertStripe",
            Self::ThinReverseDiagStripe => "thinReverseDiagStripe",
            Self::ThinDiagStripe => "thinDiagStripe",
            Self::ThinHorzCross => "thinHorzCross",
            Self::ThinDiagCross => "thinDiagCross",
            Self::Pct5 => "pct5",
            Self::Pct10 => "pct10",
            Self::Pct12 => "pct12",
            Self::Pct15 => "pct15",
            Self::Pct20 => "pct20",
            Self::Pct25 => "pct25",
            Self::Pct30 => "pct30",
            Self::Pct35 => "pct35",
            Self::Pct37 => "pct37",
            Self::Pct40 => "pct40",
            Self::Pct45 => "pct45",
            Self::Pct50 => "pct50",
            Self::Pct55 => "pct55",
            Self::Pct60 => "pct60",
            Self::Pct62 => "pct62",
            Self::Pct65 => "pct65",
            Self::Pct70 => "pct70",
            Self::Pct75 => "pct75",
            Self::Pct80 => "pct80",
            Self::Pct85 => "pct85",
            Self::Pct87 => "pct87",
            Self::Pct90 => "pct90",
            Self::Pct95 => "pct95",
        }
    }
}

/// Shading/fill for a cell or table.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Shading {
    /// Fill color as hex (e.g., "FFFF00", "auto").
    pub fill: Option<String>,
    /// Pattern value per §17.18.78 `ST_Shd`.
    pub val: Option<ShadingPattern>,
    /// Pattern color.
    pub color: Option<String>,
    /// Verbatim round-trip of any CT_Shd attribute the typed fields above don't
    /// model — theme fills/colors (themeFill/themeFillTint/themeFillShade/
    /// themeColor/themeTint/themeShade). RFC-0003 "never silently drop".
    #[serde(default)]
    pub extra_attrs: Vec<(String, String)>,
}

/// East Asian emphasis mark per §17.18.24 `ST_Em`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum EmphasisMark {
    None,
    Dot,
    Comma,
    Circle,
    UnderDot,
}

impl EmphasisMark {
    pub fn from_xml_str(s: &str) -> Result<Self, String> {
        match s {
            "none" => Ok(Self::None),
            "dot" => Ok(Self::Dot),
            "comma" => Ok(Self::Comma),
            "circle" => Ok(Self::Circle),
            "underDot" => Ok(Self::UnderDot),
            other => Err(format!("unknown EmphasisMark: {other:?}")),
        }
    }

    pub fn to_xml_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Dot => "dot",
            Self::Comma => "comma",
            Self::Circle => "circle",
            Self::UnderDot => "underDot",
        }
    }
}

/// Animated text effect per §17.18.95 `ST_TextEffect`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum TextEffect {
    BlinkBackground,
    Lights,
    AntsBlack,
    AntsRed,
    Shimmer,
    Sparkle,
    None,
}

impl TextEffect {
    pub fn from_xml_str(s: &str) -> Result<Self, String> {
        match s {
            "blinkBackground" => Ok(Self::BlinkBackground),
            "lights" => Ok(Self::Lights),
            "antsBlack" => Ok(Self::AntsBlack),
            "antsRed" => Ok(Self::AntsRed),
            "shimmer" => Ok(Self::Shimmer),
            "sparkle" => Ok(Self::Sparkle),
            "none" => Ok(Self::None),
            other => Err(format!("unknown TextEffect: {other:?}")),
        }
    }

    pub fn to_xml_str(&self) -> &'static str {
        match self {
            Self::BlinkBackground => "blinkBackground",
            Self::Lights => "lights",
            Self::AntsBlack => "antsBlack",
            Self::AntsRed => "antsRed",
            Self::Shimmer => "shimmer",
            Self::Sparkle => "sparkle",
            Self::None => "none",
        }
    }
}

/// Fit text constraint per §17.3.2.14 `CT_FitText`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct FitText {
    /// Width in twips that the text should be compressed/expanded to fit.
    pub width: u32,
    /// Grouping ID — runs with the same ID are fitted together.
    pub id: Option<u32>,
}

/// Width type for table/cell measurements per §17.18.90 `ST_TblWidth`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub enum WidthType {
    /// Fixed width in twips (twentieths of a point).
    Dxa,
    /// Percentage in fiftieths of a percent (e.g., 5000 = 100%).
    Pct,
    /// Automatic width.
    Auto,
    /// No width (nil).
    Nil,
}

impl WidthType {
    pub fn from_xml_str(s: &str) -> Result<Self, String> {
        match s {
            "dxa" => Ok(Self::Dxa),
            "pct" => Ok(Self::Pct),
            "auto" => Ok(Self::Auto),
            "nil" => Ok(Self::Nil),
            other => Err(format!("unknown WidthType: {other:?}")),
        }
    }

    pub fn to_xml_str(&self) -> &'static str {
        match self {
            Self::Dxa => "dxa",
            Self::Pct => "pct",
            Self::Auto => "auto",
            Self::Nil => "nil",
        }
    }
}

/// Table or cell width measurement.
///
/// The carrier attribute (`w:w` on `w:tblW`, `w:tcW`, `w:wBefore`, `w:wAfter`)
/// is typed ST_MeasurementOrPercent (ECMA-376 §17.18.107): a plain decimal
/// number, an ST_Percentage literal (`"33.3%"`), or an ST_UniversalMeasure
/// (`"1.5in"`). All forms are normalized into `w` at the import edge
/// (`parse_table_measurement`); `pct_literal` additionally keeps the exact
/// percent spelling for source-form-faithful re-emission, because tables are
/// rebuilt (not copied verbatim) on serialization.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TableMeasurement {
    /// Width value, normalized: twips when `width_type` is `Dxa`, fiftieths
    /// of a percent when `Pct` (e.g. 5000 = 100%).
    pub w: u32,
    /// Width type per §17.18.90 `ST_TblWidth`.
    pub width_type: WidthType,
    /// Exact source spelling when the width arrived as an ST_Percentage
    /// literal (e.g. `"33.3%"`, `"100%"`); re-emitted verbatim on save so a
    /// rebuilt table does not churn the width form.
    ///
    /// INVARIANT: `Some` implies `width_type == WidthType::Pct` and
    /// `w == round(percent_value * 50)`. Established only at the import edge;
    /// programmatic widths (edit verbs, wire patches) always carry `None`.
    ///
    /// NOTE: no `#[serde(skip_serializing_if)]` — runtime snapshots use
    /// bincode, which serializes structs positionally.
    #[serde(default)]
    pub pct_literal: Option<String>,
}

/// Vertical alignment for a table cell.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum VerticalAlignment {
    Top,
    Center,
    Bottom,
}

// =============================================================================
// Canonical Table Model
// =============================================================================

/// A canonical table representation with explicit grid structure.
///
/// Transforms DOCX's physical cell list into a logical rectangular grid.
/// This model is used for accurate table diffing and rendering.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CanonicalTable {
    /// Original table ID for traceability.
    pub id: NodeId,
    /// Number of logical rows in the grid.
    pub n_rows: usize,
    /// Number of logical columns in the grid.
    pub n_cols: usize,
    /// All unique cells (each appears once, at its anchor position).
    /// Ordered top-to-bottom, left-to-right by anchor.
    pub cells: Vec<CanonicalCell>,
    /// OWNER grid: owner_grid[row][col] -> Option<cell index>.
    /// - Some(i) = cell i owns this position (whether anchor or spanned)
    /// - None = position is empty/invalid
    ///   Every covered position points to its owning cell for O(1) lookup.
    pub owner_grid: Vec<Vec<Option<usize>>>,
    /// Table-level formatting (borders, widths, grid columns).
    pub formatting: TableFormatting,
    /// Per-row tracked change status from w:trPr (w:ins or w:del).
    /// Indexed by row number. Only present for rows with native tracking.
    pub row_tracking: Vec<Option<TrackingStatus>>,
}

impl CanonicalTable {
    /// Get the cell that owns a grid position (O(1) via owner_grid).
    pub fn cell_at(&self, row: usize, col: usize) -> Option<&CanonicalCell> {
        self.owner_grid.get(row)?.get(col)?.map(|i| &self.cells[i])
    }

    /// Check if position is an anchor (top-left of a cell).
    pub fn is_anchor(&self, row: usize, col: usize) -> bool {
        self.cell_at(row, col)
            .map(|cell| cell.row == row && cell.col == col)
            .unwrap_or(false)
    }

    /// Compute row signature for alignment (anchor-only to avoid repeated span text).
    ///
    /// For each column in the row:
    /// - If there's an anchor at (row, col): use its text
    /// - If covered by rowspan from above: use "↕" marker (not the actual text)
    /// - If covered by colspan from left: use "↔" marker
    /// - If empty: use ""
    pub fn row_signature(&self, row: usize) -> String {
        let mut parts: Vec<String> = Vec::with_capacity(self.n_cols);

        for col in 0..self.n_cols {
            if self.is_anchor(row, col) {
                // Anchor at this position: use cell text
                if let Some(cell) = self.cell_at(row, col) {
                    parts.push(cell.text.clone());
                }
            } else if let Some(cell) = self.cell_at(row, col) {
                // Covered by span: check if this is a rowspan continuation
                if cell.row < row {
                    // Rowspan from above: use marker instead of text
                    parts.push("↕".to_string());
                } else {
                    // Colspan from left: use marker
                    parts.push("↔".to_string());
                }
            } else {
                // Empty position
                parts.push(String::new());
            }
        }

        parts.join(" | ")
    }
}

/// A single cell in the canonical table.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CanonicalCell {
    /// Original cell ID for traceability.
    pub id: NodeId,
    /// Anchor position row (top row for merged cells).
    pub row: usize,
    /// Anchor position column (left column for merged cells).
    pub col: usize,
    /// Vertical span (1 = no merge).
    pub rowspan: usize,
    /// Horizontal span (1 = no merge).
    pub colspan: usize,
    /// Cell content (paragraphs, nested tables, etc.).
    pub blocks: Vec<BlockNode>,
    /// Pre-computed normalized text for diffing.
    pub text: String,
    /// Cell-level formatting (borders, shading, width, alignment).
    pub formatting: CellFormatting,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct OpaqueBlockNode {
    pub id: NodeId,
    pub kind: OpaqueKind,
    pub opaque_ref: String,
    pub proof_ref: ProofRef,
    /// Set when this opaque block IS one half of a paired range marker that
    /// stood as a DIRECT child of `w:body` (a `bookmarkStart`/`End`,
    /// `commentRangeStart`/`End`, or `permStart`/`End` between paragraphs, §17.13).
    /// The block still serializes verbatim from its raw bytes — this is metadata,
    /// not a new emitter, so byte fidelity is unchanged. It exists so the
    /// tracked-change torn-range repair can SEE the marker's identity (family +
    /// id + role): without it a body-level half is invisible, and a projection
    /// that removes the paragraph holding its inline partner orphans the pair
    /// (ECMA-376 §17.13.6). See `tracked_model::collapse_resolution_torn_range_markers`.
    #[serde(default)]
    pub range_marker: Option<RangeMarkerMeta>,
}

/// Identity of a paired range marker carried on an [`OpaqueBlockNode`] (a
/// body-level bookmark / comment-range / permission half). Lets the torn-range
/// repair pair a block-level half with its inline partner by `(family, id)` and
/// know which end it is.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct RangeMarkerMeta {
    pub family: RangeMarkerFamily,
    pub id: String,
    pub role: RangeMarkerRole,
}

/// The paired-range families the torn-range repair understands. `customXml` and
/// move ranges are deliberately excluded (they are the markup of a wrapper/move
/// revision and resolve as a unit, not collapsed to a point).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeMarkerFamily {
    Bookmark,
    CommentRange,
    Permission,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeMarkerRole {
    Start,
    End,
}

/// The `Text` and `OpaqueInline` variants box their payloads so `InlineNode`
/// stays small (one machine word + discriminant) instead of ~1 KB. A
/// paragraph's `Vec<InlineNode>` reserves `len * sizeof(InlineNode)`
/// contiguously, so an unboxed 1 KB variant made each paragraph's inline buffer
/// ~21 KB. `Box<T>` is serde/bincode-transparent (serializes as the inner
/// value), so snapshot blobs keep the same wire shape.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum InlineNode {
    Text(Box<TextNode>),
    HardBreak(HardBreakNode),
    /// Widget that occupies space (images, embedded objects, etc.).
    OpaqueInline(Box<OpaqueInlineNode>),
    /// Zero-width decoration (bookmarks, comments, etc.).
    /// Stored for roundtripping but doesn't affect text positions.
    /// Boxed (like `Text`/`OpaqueInline`) because run-level decorations carry a
    /// `wrapper_style_props: StyleProps` that would otherwise bloat the enum.
    Decoration(Box<DecorationNode>),
    /// Comment range start marker (links to CommentStory by id).
    CommentRangeStart {
        id: String,
    },
    /// Comment range end marker.
    CommentRangeEnd {
        id: String,
    },
    /// Comment reference marker (visible indicator in document).
    CommentReference {
        id: String,
    },
}

impl From<TextNode> for InlineNode {
    fn from(t: TextNode) -> Self {
        InlineNode::Text(Box::new(t))
    }
}

impl From<OpaqueInlineNode> for InlineNode {
    fn from(o: OpaqueInlineNode) -> Self {
        InlineNode::OpaqueInline(Box::new(o))
    }
}

impl From<DecorationNode> for InlineNode {
    fn from(d: DecorationNode) -> Self {
        InlineNode::Decoration(Box::new(d))
    }
}

/// Run-level border properties from w:bdr (ISO 29500-1 §17.3.2.4).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct RunBorder {
    /// Border style (e.g., "single", "double", "dotted").
    pub style: String,
    /// Border width in eighth-points.
    pub size: u32,
    /// Spacing offset in points.
    pub space: u32,
    /// Border color as hex RGB (e.g., "FF0000").
    pub color: String,
}

/// Theme color reference from w:color attributes (§17.3.2.6).
/// Groups the themeColor / themeShade / themeTint attributes that travel together.
/// When present on a `<w:color>` element, the theme color takes precedence over
/// the literal hex RGB in w:val — the val is a pre-resolved fallback for readers
/// that don't support themes.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ThemeColorRef {
    /// Theme color name (e.g., "accent1", "dark1", "text1").
    pub theme_color: IStr,
    /// Optional shade modifier as hex byte (e.g., "BF" = 75% shade).
    pub theme_shade: Option<IStr>,
    /// Optional tint modifier as hex byte (e.g., "99" = 60% tint).
    pub theme_tint: Option<IStr>,
}

/// A preserved, unmodeled property child: the verbatim XML of an rPr child
/// element the engine does not model, captured at import and re-emitted at
/// serialization so it survives round-trip.
/// Invariant: never synthesized by the engine — only carried from a parsed
/// source part. Two runs whose preserved sets differ are format-distinct
/// (they must not coalesce), which the derived PartialEq provides.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PreservedProp {
    /// Qualified element name as parsed (e.g. "w:eastAsianLayout", "w14:glow").
    pub name: String,
    /// Verbatim serialized element subtree.
    pub raw_xml: String,
}

/// Value-carrying style properties for text runs.
/// These are non-boolean formatting properties that carry a value
/// (unlike marks which are just on/off).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct StyleProps {
    /// Font family from w:rFonts (w:ascii / w:hAnsi).
    pub font_family: Option<IStr>,
    /// Theme font reference for ascii/hAnsi slot (e.g., "minorHAnsi").
    /// When present, serialized as w:asciiTheme / w:hAnsiTheme on w:rFonts.
    /// Per §17.3.2.26, theme attributes take precedence over direct font names.
    pub font_family_theme: Option<IStr>,
    /// Font size in half-points from w:sz (e.g., 24 = 12pt).
    pub font_size: Option<u32>,
    /// Text color from w:color (e.g., "FF0000" or "auto").
    pub color: Option<IStr>,
    /// Theme color reference from w:color (themeColor/themeShade/themeTint).
    /// When present, the theme color takes precedence; `color` is the pre-resolved fallback.
    pub color_theme: Option<ThemeColorRef>,
    /// Highlight color per §17.18.40 `ST_HighlightColor`.
    pub highlight: Option<HighlightColor>,
    /// Underline style from w:u w:val per §17.18.99 `ST_Underline`.
    pub underline_style: Option<UnderlineStyle>,
    /// East Asian font family from w:rFonts w:eastAsia.
    pub font_east_asia: Option<IStr>,
    /// Theme font reference for eastAsia slot (e.g., "minorEastAsia").
    pub font_east_asia_theme: Option<IStr>,
    /// Complex script font family from w:rFonts w:cs.
    pub font_cs: Option<IStr>,
    /// Theme font reference for cs slot (e.g., "minorBidi").
    pub font_cs_theme: Option<IStr>,
    /// Language tag from w:lang w:val (e.g., "en-US").
    pub lang: Option<IStr>,
    /// East Asian language tag from w:lang w:eastAsia (e.g., "ja-JP").
    pub lang_east_asia: Option<IStr>,
    /// Character spacing in twips from w:spacing w:val in rPr.
    pub char_spacing: Option<i32>,
    /// Character style ID from w:rStyle (e.g., "BoldChar", "Emphasis").
    pub char_style_id: Option<IStr>,
    /// Run-level border from w:bdr (ISO 29500-1 §17.3.2.4).
    pub run_border: Option<RunBorder>,
    /// Vertical position offset in half-points from w:position (ISO 29500-1 §17.3.2.19).
    /// Positive = raised, negative = lowered.
    pub position: Option<i64>,
    /// Kerning threshold in half-points from w:kern (ISO 29500-1 §17.3.2.19a).
    pub kern: Option<i64>,
    /// Character width scaling percentage from w:w (ISO 29500-1 §17.3.2.43).
    /// 100 = normal, 200 = double width, 50 = half width.
    pub char_width_scaling: Option<i64>,
    /// Complex script bold from w:bCs (MS-OI29500 §17.3.2.1).
    pub bold_cs: MarkValue,
    /// Complex script italic from w:iCs (MS-OI29500 §17.3.2.16).
    pub italic_cs: MarkValue,
    /// Strikethrough from w:strike (§17.3.2.37). Tri-state: On/Off/Inherit.
    pub strike: MarkValue,
    /// Double strikethrough from w:dstrike (§17.3.2.9). Tri-state: On/Off/Inherit.
    pub double_strike: MarkValue,
    /// All caps from w:caps (§17.3.2.5). Tri-state: On/Off/Inherit.
    pub caps: MarkValue,
    /// Small caps from w:smallCaps (§17.3.2.33). Tri-state: On/Off/Inherit.
    pub small_caps: MarkValue,
    /// Hidden text from w:vanish (§17.3.2.41). Tri-state: On/Off/Inherit.
    pub vanish: MarkValue,
    /// Hidden when displayed as a web page from w:webHidden (§17.3.2.44).
    /// Distinct from vanish (which hides in all views). Tri-state: On/Off/Inherit.
    pub web_hidden: MarkValue,
    /// Embossed text from w:emboss (§17.3.2.13). Tri-state: On/Off/Inherit.
    pub emboss: MarkValue,
    /// Imprinted text from w:imprint (§17.3.2.18). Tri-state: On/Off/Inherit.
    pub imprint: MarkValue,
    /// Outline text from w:outline (§17.3.2.23). Tri-state: On/Off/Inherit.
    pub outline: MarkValue,
    /// Shadow text from w:shadow (§17.3.2.31). Tri-state: On/Off/Inherit.
    pub shadow: MarkValue,
    /// Complex script font size in half-points from w:szCs (MS-OI29500 §17.3.2.38).
    pub font_size_cs: Option<u32>,
    /// Right-to-left run flag from w:rtl (CT_OnOff).
    pub rtl: MarkValue,
    /// Complex script flag from w:cs (CT_OnOff).
    pub cs: MarkValue,
    /// Font hint from w:rFonts w:hint (MS-OI29500 §17.3.2.26(b)).
    /// Controls per-character font selection for ambiguous Unicode ranges.
    pub font_hint: Option<IStr>,
    /// Suppress proofing marks from w:noProof (§17.3.2.21).
    pub no_proof: MarkValue,
    /// Special vanish for style separator runs from w:specVanish (§17.3.2.36).
    pub spec_vanish: MarkValue,
    /// Math formatting context from w:oMath (§17.3.2.22).
    pub o_math: MarkValue,
    /// Snap to document grid from w:snapToGrid (§17.3.2.34).
    pub snap_to_grid: MarkValue,
    /// Run-level shading from w:shd (§17.3.2.32).
    pub run_shading: Option<Shading>,
    /// East Asian emphasis mark from w:em (§17.3.2.11).
    pub emphasis_mark: Option<EmphasisMark>,
    /// Animated text effect from w:effect (§17.3.2.12).
    pub text_effect: Option<TextEffect>,
    /// Fit text constraint from w:fitText (§17.3.2.14).
    pub fit_text: Option<FitText>,
    /// Unmodeled rPr children captured verbatim at import and re-emitted at
    /// their Annex-A position on serialization (e.g. w:eastAsianLayout, or a
    /// foreign-namespace extension like w14:glow) — the disciplined-
    /// preservation remainder for run properties this engine does not model.
    /// Never synthesized by the engine; only ever carried through from a
    /// parsed source part.
    #[serde(default)]
    pub preserved: Vec<PreservedProp>,
}

impl StyleProps {
    pub fn is_empty(&self) -> bool {
        self.font_family.is_none()
            && self.font_family_theme.is_none()
            && self.font_size.is_none()
            && self.color.is_none()
            && self.color_theme.is_none()
            && self.highlight.is_none()
            && self.underline_style.is_none()
            && self.font_east_asia.is_none()
            && self.font_east_asia_theme.is_none()
            && self.font_cs.is_none()
            && self.font_cs_theme.is_none()
            && self.lang.is_none()
            && self.lang_east_asia.is_none()
            && self.char_spacing.is_none()
            && self.char_style_id.is_none()
            && self.run_border.is_none()
            && self.position.is_none()
            && self.kern.is_none()
            && self.char_width_scaling.is_none()
            && self.bold_cs == MarkValue::Inherit
            && self.italic_cs == MarkValue::Inherit
            && self.strike == MarkValue::Inherit
            && self.double_strike == MarkValue::Inherit
            && self.caps == MarkValue::Inherit
            && self.small_caps == MarkValue::Inherit
            && self.vanish == MarkValue::Inherit
            && self.web_hidden == MarkValue::Inherit
            && self.emboss == MarkValue::Inherit
            && self.imprint == MarkValue::Inherit
            && self.outline == MarkValue::Inherit
            && self.shadow == MarkValue::Inherit
            && self.font_size_cs.is_none()
            && self.rtl == MarkValue::Inherit
            && self.cs == MarkValue::Inherit
            && self.font_hint.is_none()
            && self.no_proof == MarkValue::Inherit
            && self.spec_vanish == MarkValue::Inherit
            && self.o_math == MarkValue::Inherit
            && self.snap_to_grid == MarkValue::Inherit
            && self.strike == MarkValue::Inherit
            && self.double_strike == MarkValue::Inherit
            && self.caps == MarkValue::Inherit
            && self.small_caps == MarkValue::Inherit
            && self.outline == MarkValue::Inherit
            && self.shadow == MarkValue::Inherit
            && self.emboss == MarkValue::Inherit
            && self.imprint == MarkValue::Inherit
            && self.vanish == MarkValue::Inherit
            && self.run_shading.is_none()
            && self.emphasis_mark.is_none()
            && self.text_effect.is_none()
            && self.fit_text.is_none()
            && self.preserved.is_empty()
    }
}

/// Tracked paragraph formatting change: the "before" state from w:pPrChange (§17.13.5.29).
/// Present on paragraphs whose formatting was changed via Word's Track Changes.
///
/// Per the spec, the child pPr inside pPrChange must be a COMPLETE snapshot of the
/// previous paragraph properties — not just the properties that changed.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ParagraphFormattingChange {
    /// Previous alignment (before the change).
    pub previous_alignment: Option<Alignment>,
    /// Previous indentation (before the change).
    pub previous_indentation: Option<Indentation>,
    /// Previous spacing (before the change).
    pub previous_spacing: Option<ParagraphSpacing>,
    /// Previous numbering (before the change).
    pub previous_numbering: Option<NumberingInfo>,
    /// When true, the base paragraph had no numbering at all (no numPr AND no
    /// literal prefix). The serializer emits `numId=0` in pPrChange's inner pPr
    /// to signal this to the extraction, which skips the paragraph in the
    /// reject-view numbering state machine. Without this flag, `previous_numbering:
    /// None` is ambiguous — it could mean "had no numPr but had a literal prefix"
    /// (where the current numPr replaced the prefix and should still be counted).
    pub previous_numbering_explicitly_absent: bool,
    /// Previous style ID from w:pStyle (before the change).
    pub previous_style_id: Option<IStr>,
    /// Previous keepNext (before the change).
    pub previous_keep_next: Option<bool>,
    /// Previous keepLines (before the change).
    pub previous_keep_lines: Option<bool>,
    /// Previous pageBreakBefore (before the change).
    pub previous_page_break_before: bool,
    /// Previous widowControl (before the change).
    pub previous_widow_control: Option<bool>,
    /// Previous contextualSpacing (before the change).
    pub previous_contextual_spacing: Option<bool>,
    /// Previous paragraph shading (before the change).
    pub previous_shading: Option<Shading>,
    /// Previous paragraph borders (before the change).
    pub previous_borders: Option<ParagraphBorders>,
    /// Previous tab stops (before the change).
    pub previous_tab_stops: Vec<crate::word_ir::TabStopDef>,
    /// Previous gap from margin-left to the consumed leading tab stop for a
    /// stripped literal prefix like `\t(c)\t`.
    pub previous_literal_prefix_leading_tab_twips: Option<i32>,
    /// Previous gap from margin-left to the tab stop reached by a stripped
    /// trailing tab after the literal prefix.
    pub previous_literal_prefix_trailing_tab_stop_twips: Option<i32>,
    /// Previous direct paragraph-mark formatting marks from w:pPr/w:rPr.
    pub previous_paragraph_mark_marks: Vec<Mark>,
    /// Previous direct paragraph-mark value-carrying style props from w:pPr/w:rPr.
    pub previous_paragraph_mark_style_props: StyleProps,
    /// Previous authored OFF toggles on the paragraph mark's w:pPr/w:rPr (the
    /// pilcrow analogue that `previous_paragraph_mark_marks` cannot carry).
    #[serde(default)]
    pub previous_paragraph_mark_rpr_off: ParaMarkRprOff,
    /// Previous text direction (before the change).
    pub previous_text_direction: Option<TextDirection>,
    /// Previous text alignment (before the change).
    pub previous_text_alignment: Option<TextAlignment>,
    /// Previous mirrorIndents (before the change). Three-state, matching
    /// `ParagraphNode::mirror_indents`.
    #[serde(default)]
    pub previous_mirror_indents: Option<bool>,
    /// Previous autoSpaceDE (before the change).
    pub previous_auto_space_de: Option<bool>,
    /// Previous autoSpaceDN (before the change).
    pub previous_auto_space_dn: Option<bool>,
    /// Previous bidi (before the change). Three-state, matching
    /// `ParagraphNode::bidi`.
    #[serde(default)]
    pub previous_bidi: Option<bool>,
    /// Previous suppressAutoHyphens (before the change).
    pub previous_suppress_auto_hyphens: Option<bool>,
    /// Previous snapToGrid (before the change).
    pub previous_snap_to_grid: Option<bool>,
    /// Previous overflowPunct (before the change).
    pub previous_overflow_punct: Option<bool>,
    /// Previous adjustRightInd (before the change).
    pub previous_adjust_right_ind: Option<bool>,
    /// Previous wordWrap (before the change).
    pub previous_word_wrap: Option<bool>,
    /// Previous framePr (before the change).
    pub previous_frame_pr: Option<FrameProperties>,
    /// Unmodeled children of the pPrChange's previous pPr, captured verbatim
    /// at import and re-emitted inside the serialized w:pPrChange's inner
    /// w:pPr. On reject, these REPLACE the restored paragraph's own
    /// `ParagraphNode::preserved_ppr`: the snapshot IS the complete previous
    /// pPr per §17.13.5.29, so its preserved remainder is the paragraph's
    /// entire unmodeled remainder after reject, not a merge with whatever the
    /// (about-to-be-discarded) current state happened to carry.
    #[serde(default)]
    pub previous_preserved_ppr: Vec<PreservedProp>,
    /// Stable revision id (`w:id`) of this tracked formatting change — the
    /// SAME identity the accept/reject selectors address. `0` is the legacy
    /// sentinel for snapshots serialized before identity existed (such a
    /// change cannot be selected by id until the doc is re-imported; the
    /// serializer mints a fresh id for it on output, preserving old behavior).
    #[serde(default)]
    pub revision_id: u32,
    /// Revision author.
    pub author: String,
    /// Revision date.
    pub date: Option<String>,
    /// ENGINE-MINTED revision identity (RFC-0004 §H7). The stable,
    /// document-unique handle the resolution surface addresses, distinct from
    /// the wire `revision_id` (which Word does not keep unique). `0` is the
    /// pre-identity sentinel. Appended LAST for the bincode-positional reason
    /// on `revision_id`.
    #[serde(default)]
    pub identity: u32,
}

/// Tracked table formatting change: the "before" state from w:tblPrChange (§17.13.5.34).
/// Present on tables whose formatting was changed via Word's Track Changes.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TableFormattingChange {
    /// Previous table width (before the change).
    pub previous_width: Option<TableMeasurement>,
    /// Previous table borders (before the change).
    pub previous_borders: Option<BorderSet>,
    /// Previous default cell margins (before the change).
    pub previous_default_cell_margins: Option<CellMargins>,
    /// Stable revision id (`w:id`) of this tracked formatting change — the
    /// SAME identity the accept/reject selectors address. `0` is the legacy
    /// sentinel for snapshots serialized before identity existed (such a
    /// change cannot be selected by id until the doc is re-imported; the
    /// serializer mints a fresh id for it on output, preserving old behavior).
    #[serde(default)]
    pub revision_id: u32,
    /// Revision author.
    pub author: String,
    /// Revision date.
    pub date: Option<String>,
    /// ENGINE-MINTED revision identity (RFC-0004 §H7). The stable,
    /// document-unique handle the resolution surface addresses, distinct from
    /// the wire `revision_id` (which Word does not keep unique). `0` is the
    /// pre-identity sentinel. Appended LAST for the bincode-positional reason
    /// on `revision_id`.
    #[serde(default)]
    pub identity: u32,
}

/// Tracked row formatting change: the "before" state from w:trPrChange (§17.13.5.36).
/// Present on table rows whose formatting was changed via Word's Track Changes.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct RowFormattingChange {
    /// Previous row height in twips (before the change).
    pub previous_height: Option<u32>,
    /// Previous row height rule (before the change).
    pub previous_height_rule: Option<HeightRule>,
    /// Stable revision id (`w:id`) of this tracked formatting change — the
    /// SAME identity the accept/reject selectors address. `0` is the legacy
    /// sentinel for snapshots serialized before identity existed (such a
    /// change cannot be selected by id until the doc is re-imported; the
    /// serializer mints a fresh id for it on output, preserving old behavior).
    #[serde(default)]
    pub revision_id: u32,
    /// Revision author.
    pub author: String,
    /// Revision date.
    pub date: Option<String>,
    /// ENGINE-MINTED revision identity (RFC-0004 §H7). The stable,
    /// document-unique handle the resolution surface addresses, distinct from
    /// the wire `revision_id` (which Word does not keep unique). `0` is the
    /// pre-identity sentinel. Appended LAST for the bincode-positional reason
    /// on `revision_id`.
    #[serde(default)]
    pub identity: u32,
}

/// Tracked cell formatting change: the "before" state from w:tcPrChange (§17.13.5.37).
/// Present on table cells whose formatting was changed via Word's Track Changes.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CellFormattingChange {
    /// Previous cell width (before the change).
    pub previous_width: Option<TableMeasurement>,
    /// Previous cell borders (before the change).
    pub previous_borders: Option<BorderSet>,
    /// Previous cell shading (before the change).
    pub previous_shading: Option<Shading>,
    /// Previous vertical alignment (before the change).
    pub previous_v_align: Option<VerticalAlignment>,
    /// Previous cell margins (before the change).
    pub previous_margins: Option<CellMargins>,
    /// Previous no-wrap setting (before the change).
    pub previous_no_wrap: Option<bool>,
    /// Previous text direction (before the change).
    pub previous_text_direction: Option<TextDirection>,
    /// Previous fit-text setting (before the change).
    pub previous_tc_fit_text: Option<bool>,
    /// Stable revision id (`w:id`) of this tracked formatting change — the
    /// SAME identity the accept/reject selectors address. `0` is the legacy
    /// sentinel for snapshots serialized before identity existed (such a
    /// change cannot be selected by id until the doc is re-imported; the
    /// serializer mints a fresh id for it on output, preserving old behavior).
    #[serde(default)]
    pub revision_id: u32,
    /// Revision author.
    pub author: String,
    /// Revision date.
    pub date: Option<String>,
    /// ENGINE-MINTED revision identity (RFC-0004 §H7). The stable,
    /// document-unique handle the resolution surface addresses, distinct from
    /// the wire `revision_id` (which Word does not keep unique). `0` is the
    /// pre-identity sentinel. Appended LAST for the bincode-positional reason
    /// on `revision_id`.
    #[serde(default)]
    pub identity: u32,
}

/// Tracked formatting change: the "before" state from w:rPrChange.
/// Present on text nodes whose formatting was changed via Word's Track Changes.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct FormattingChange {
    /// The previous boolean marks (before the change).
    pub previous_marks: Vec<Mark>,
    /// The previous style properties (before the change).
    pub previous_style_props: StyleProps,
    /// The previous per-slot rPr authoring provenance (before the change).
    /// Without this, rejecting the change can restore `previous_marks`/
    /// `previous_style_props` correctly in the typed model while the
    /// SERIALIZER still emits the now-reverted properties anyway — it reads
    /// `rpr_authored`, a separate provenance bitset, to decide which `<w:rPr>`
    /// children to write, and `previous_marks`/`previous_style_props` alone
    /// say nothing about it. `#[serde(default)]`: a snapshot written before
    /// this field existed defaults to "nothing was authored", which only
    /// matters for a REJECT of such a legacy change — see reject_text_
    /// formatting's doc comment.
    #[serde(default)]
    pub previous_rpr_authored: RunRprAuthored,
    /// Stable revision id (`w:id`) of this tracked formatting change — the
    /// SAME identity the accept/reject selectors address. `0` is the legacy
    /// sentinel for snapshots serialized before identity existed (such a
    /// change cannot be selected by id until the doc is re-imported; the
    /// serializer mints a fresh id for it on output, preserving old behavior).
    #[serde(default)]
    pub revision_id: u32,
    /// Revision author.
    pub author: String,
    /// Revision date.
    pub date: Option<String>,
    /// ENGINE-MINTED revision identity (RFC-0004 §H7). The stable,
    /// document-unique handle the resolution surface addresses, distinct from
    /// the wire `revision_id` (which Word does not keep unique). `0` is the
    /// pre-identity sentinel. Appended LAST for the bincode-positional reason
    /// on `revision_id`.
    #[serde(default)]
    pub identity: u32,
}

/// Per-slot provenance for a text run's `<w:rPr>`: was each property AUTHORED
/// DIRECTLY on the run (`true`) or merely INHERITED through the style cascade
/// (direct → char style → para style → docDefaults, collapsed into
/// `TextNode.style_props` at import) (`false`)?
///
/// `style_props` holds the fully-RESOLVED effective values. The serializer must
/// emit a property as DIRECT `<w:rPr>` ONLY when it was authored directly;
/// emitting an inherited value as direct rPr bakes the cascade into the run and
/// (for theme attributes / themeColor, which WIN per §17.3.2.26) actually CHANGES
/// rendering. These flags carry the run's authored-vs-inherited provenance into
/// serialization so inherited props are suppressed and re-resolve from the style
/// at render time (faithful round-trip).
///
/// One flag per independently-injectable rPr slot. `font_family` (literal
/// ascii/hAnsi) and `font_family_theme` (asciiTheme/hAnsiTheme) are SEPARATE: a
/// run authoring a literal font must not have a theme font injected, and vice
/// versa. Likewise `color` (literal/auto) vs `color_theme` (themeColor).
/// See [`ParagraphNode::literal_prefix_leading_rpr`].
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PrefixLeadingRpr {
    pub marks: Vec<Mark>,
    pub style_props: StyleProps,
    pub rpr_authored: RunRprAuthored,
}

/// The paragraph MARK's authored OFF toggles from `w:pPr/w:rPr` (§17.3.1.29
/// CT_ParaRPr): an explicit `<w:b w:val="0"/>`, `<w:i w:val="0"/>`, or
/// `<w:u w:val="none"/>` on the pilcrow that cancels a toggle (bold/italic,
/// §17.7.3) or underline (§17.3.2.40) the mark would otherwise inherit from the
/// paragraph/linked character style.
///
/// The pilcrow's OWN formatting is captured DIRECT-only (no cascade resolution),
/// split as `paragraph_mark_marks` (presence) + `paragraph_mark_style_props`
/// (values). A presence-only `Vec<Mark>` cannot carry an OFF, so — exactly like
/// runs, whose `RunRprAuthored::{bold_off, italic_off, underline_off}` solved the
/// identical gap — the OFF forms need explicit representation or they drop
/// silently on every rebuild (`build_rpr` only emits `<w:u>` when `Mark::Underline`
/// is present, and never `<w:b w:val="0"/>`). Unlike runs there is no provenance
/// ambiguity here: these are exactly the direct OFF forms the pilcrow authored.
/// The serializer re-emits them via `append_authored_off_toggles`.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct ParaMarkRprOff {
    /// `<w:b w:val="0"/>` on the paragraph mark.
    pub bold_off: bool,
    /// `<w:i w:val="0"/>` on the paragraph mark.
    pub italic_off: bool,
    /// `<w:u w:val="none"/>` on the paragraph mark.
    pub underline_off: bool,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct RunRprAuthored {
    /// w:rFonts w:ascii / w:hAnsi (literal font name).
    pub font_family: bool,
    /// w:rFonts w:asciiTheme / w:hAnsiTheme.
    pub font_family_theme: bool,
    /// w:rFonts w:eastAsia (literal).
    pub font_east_asia: bool,
    /// w:rFonts w:eastAsiaTheme.
    pub font_east_asia_theme: bool,
    /// w:rFonts w:cs (literal).
    pub font_cs: bool,
    /// w:rFonts w:cstheme.
    pub font_cs_theme: bool,
    /// w:rFonts w:hint.
    pub font_hint: bool,
    /// w:sz.
    pub font_size: bool,
    /// w:szCs.
    pub font_size_cs: bool,
    /// w:color w:val (literal or "auto").
    pub color: bool,
    /// w:color w:themeColor / themeShade / themeTint.
    pub color_theme: bool,
    /// w:lang w:val.
    pub lang: bool,
    /// w:lang w:eastAsia.
    pub lang_east_asia: bool,
    /// w:kern (kerning threshold, §17.3.2.19a). No theme/literal precedence
    /// inversion here — gating is churn hygiene: an inherited kerning threshold
    /// must not be re-emitted as direct rPr on every run.
    pub kern: bool,
    /// w:spacing (character spacing, §17.3.2.35). Same churn-hygiene rationale
    /// as `kern`.
    pub char_spacing: bool,
    // ── Toggle marks (the blindspot-audit H8 "directness gap"). A toggle the
    // run merely inherits from its style must NOT re-emit as direct rPr:
    // besides churning untouched markup, a materialized toggle FLIPS rendering
    // when the style itself toggles it (§17.7.3 toggle-property semantics).
    /// w:b / Mark::Bold.
    pub bold: bool,
    /// w:i / Mark::Italic.
    pub italic: bool,
    /// The run authored an explicit OFF toggle (`<w:b w:val="0"/>`). Carried
    /// explicitly because `Vec<Mark>` is presence-only and absence of the
    /// resolved mark is NOT a sound signal (complex-script runs resolve
    /// b/i via bCs/iCs, so an authored-ON can be resolved-absent).
    #[serde(default)]
    pub bold_off: bool,
    /// `<w:i w:val="0"/>` — see `bold_off`.
    #[serde(default)]
    pub italic_off: bool,
    /// w:u presence via Mark::Underline (see also `underline_style`).
    pub underline: bool,
    /// The run authored an explicit underline OFF (`<w:u w:val="none"/>`),
    /// cancelling an underline it would otherwise inherit from its style or the
    /// linked character style (§17.3.2.40). Carried explicitly because the
    /// presence-only `Vec<Mark>` cannot represent an OFF: a resolved-absent
    /// underline is indistinguishable from "never set". Dropping it would let a
    /// style-level underline bleed back onto the run — the same class of defect
    /// as `bold_off`, and the underline analogue of the CT_Ind explicit-zero
    /// override. Underline is a simple override (not a XOR toggle), so this
    /// re-emits verbatim as `<w:u w:val="none"/>`.
    #[serde(default)]
    pub underline_off: bool,
    /// w:vertAlign via Mark::Subscript / Mark::Superscript.
    pub vert_align: bool,
    /// w:strike (§17.3.2.37).
    pub strike: bool,
    /// w:dstrike (§17.3.2.9).
    pub double_strike: bool,
    /// w:caps (§17.3.2.5).
    pub caps: bool,
    /// w:smallCaps (§17.3.2.33).
    pub small_caps: bool,
    /// w:vanish (§17.3.2.41).
    pub vanish: bool,
    /// w:webHidden (§17.3.2.44).
    pub web_hidden: bool,
    /// w:emboss (§17.3.2.13).
    pub emboss: bool,
    /// w:imprint (§17.3.2.18).
    pub imprint: bool,
    /// w:outline (§17.3.2.23).
    pub outline: bool,
    /// w:shadow (§17.3.2.31).
    pub shadow: bool,
    /// w:bCs (MS-OI29500 §17.3.2.1).
    pub bold_cs: bool,
    /// w:iCs (MS-OI29500 §17.3.2.16).
    pub italic_cs: bool,
    /// w:rtl (CT_OnOff).
    pub rtl: bool,
    /// w:cs (CT_OnOff).
    pub cs: bool,
    /// w:noProof (§17.3.2.21).
    pub no_proof: bool,
    /// w:specVanish (§17.3.2.36).
    pub spec_vanish: bool,
    /// w:oMath (§17.3.2.22).
    pub o_math: bool,
    /// w:snapToGrid (§17.3.2.34).
    pub snap_to_grid: bool,
    // ── Value props resolved into style_props that lacked provenance gating.
    /// w:highlight (§17.18.40).
    pub highlight: bool,
    /// w:u w:val (§17.18.99) — the underline STYLE value.
    pub underline_style: bool,
    /// w:position (§17.3.2.19).
    pub position: bool,
    /// w:w (character width scaling, §17.3.2.43).
    pub char_width_scaling: bool,
    /// w:rStyle (§17.7.4.17) — the default character style is substituted at
    /// import resolution; only a run-authored reference re-emits.
    pub char_style_id: bool,
    /// w:bdr (§17.3.2.4).
    pub run_border: bool,
    /// w:shd run shading (§17.3.2.32).
    pub run_shading: bool,
    /// w:em (§17.3.2.11).
    pub emphasis_mark: bool,
    /// w:effect (§17.3.2.12).
    pub text_effect: bool,
    /// w:fitText (§17.3.2.14).
    pub fit_text: bool,
}

impl RunRprAuthored {
    /// Non-run contexts (style definitions, opaque field wrappers, literal
    /// prefixes) and synthesized runs that author every prop they carry: emit
    /// every prop as-is.
    pub const ALL: Self = Self {
        font_family: true,
        font_family_theme: true,
        font_east_asia: true,
        font_east_asia_theme: true,
        font_cs: true,
        font_cs_theme: true,
        font_hint: true,
        font_size: true,
        font_size_cs: true,
        color: true,
        color_theme: true,
        lang: true,
        lang_east_asia: true,
        kern: true,
        char_spacing: true,
        bold: true,
        italic: true,
        // ALL means "emit what is present"; it never fabricates an OFF.
        bold_off: false,
        italic_off: false,
        underline: true,
        // ALL emits what is present; it never fabricates an OFF form.
        underline_off: false,
        vert_align: true,
        strike: true,
        double_strike: true,
        caps: true,
        small_caps: true,
        vanish: true,
        web_hidden: true,
        emboss: true,
        imprint: true,
        outline: true,
        shadow: true,
        bold_cs: true,
        italic_cs: true,
        rtl: true,
        cs: true,
        no_proof: true,
        spec_vanish: true,
        o_math: true,
        snap_to_grid: true,
        highlight: true,
        underline_style: true,
        position: true,
        char_width_scaling: true,
        char_style_id: true,
        run_border: true,
        run_shading: true,
        emphasis_mark: true,
        text_effect: true,
        fit_text: true,
    };

    /// True when the run authored the ascii/hAnsi font slot directly (literal OR
    /// theme). Conditional-formatting and table-style propagation use this to
    /// avoid overwriting a directly-authored font family.
    pub fn font_family_any(&self) -> bool {
        self.font_family || self.font_family_theme
    }

    /// True when the run authored the color slot directly (literal/auto OR theme).
    pub fn color_any(&self) -> bool {
        self.color || self.color_theme
    }

    /// Presence-derived provenance for SYNTHESIZED runs (edit materializers,
    /// diff-built content): everything the run carries is treated as authored,
    /// so it all re-emits. Import-parsed runs must NOT use this — their
    /// `style_props`/`marks` hold the RESOLVED cascade, and presence-deriving
    /// would re-introduce the materialization churn this struct exists to stop.
    pub fn from_effective(marks: &[Mark], props: &StyleProps) -> Self {
        let mv = |v: &MarkValue| *v != MarkValue::Inherit;
        Self {
            font_family: props.font_family.is_some(),
            font_family_theme: props.font_family_theme.is_some(),
            font_east_asia: props.font_east_asia.is_some(),
            font_east_asia_theme: props.font_east_asia_theme.is_some(),
            font_cs: props.font_cs.is_some(),
            font_cs_theme: props.font_cs_theme.is_some(),
            font_hint: props.font_hint.is_some(),
            font_size: props.font_size.is_some(),
            font_size_cs: props.font_size_cs.is_some(),
            color: props.color.is_some(),
            color_theme: props.color_theme.is_some(),
            lang: props.lang.is_some(),
            lang_east_asia: props.lang_east_asia.is_some(),
            kern: props.kern.is_some(),
            char_spacing: props.char_spacing.is_some(),
            bold: marks.contains(&Mark::Bold),
            italic: marks.contains(&Mark::Italic),
            bold_off: false,
            italic_off: false,
            underline: marks.contains(&Mark::Underline),
            // Synthesized runs carry no inherited-underline to cancel; a
            // not-underlined run simply omits Mark::Underline.
            underline_off: false,
            vert_align: marks.contains(&Mark::Subscript) || marks.contains(&Mark::Superscript),
            strike: mv(&props.strike),
            double_strike: mv(&props.double_strike),
            caps: mv(&props.caps),
            small_caps: mv(&props.small_caps),
            vanish: mv(&props.vanish),
            web_hidden: mv(&props.web_hidden),
            emboss: mv(&props.emboss),
            imprint: mv(&props.imprint),
            outline: mv(&props.outline),
            shadow: mv(&props.shadow),
            bold_cs: mv(&props.bold_cs),
            italic_cs: mv(&props.italic_cs),
            rtl: mv(&props.rtl),
            cs: mv(&props.cs),
            no_proof: mv(&props.no_proof),
            spec_vanish: mv(&props.spec_vanish),
            o_math: mv(&props.o_math),
            snap_to_grid: mv(&props.snap_to_grid),
            highlight: props.highlight.is_some(),
            underline_style: props.underline_style.is_some(),
            position: props.position.is_some(),
            char_width_scaling: props.char_width_scaling.is_some(),
            char_style_id: props.char_style_id.is_some(),
            run_border: props.run_border.is_some(),
            run_shading: props.run_shading.is_some(),
            emphasis_mark: props.emphasis_mark.is_some(),
            text_effect: props.text_effect.is_some(),
            fit_text: props.fit_text.is_some(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TextNode {
    pub id: NodeId,
    pub text_role: Option<TextRole>,
    pub text: String,
    pub marks: Vec<Mark>,
    pub style_props: StyleProps,
    /// Per-slot run-rPr provenance: which properties were authored DIRECTLY on
    /// this run vs inherited through the style cascade. Drives faithful
    /// serialization (emit only authored slots) and is consulted by
    /// conditional-formatting / table-style propagation to avoid overwriting
    /// directly-authored props. Ephemeral (non-content): ignored by
    /// content-equality comparisons.
    #[serde(default)]
    pub rpr_authored: RunRprAuthored,
    /// Tracked formatting change from w:rPrChange, if present.
    pub formatting_change: Option<FormattingChange>,
}

/// Break type per ISO 29500-1 §17.3.3.1 (ST_BrType).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum BreakType {
    /// Line break (textWrapping) — default when type attribute is absent.
    TextWrapping,
    /// Page break.
    Page,
    /// Column break.
    Column,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct HardBreakNode {
    pub id: NodeId,
    pub break_type: BreakType,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct OpaqueInlineNode {
    pub id: NodeId,
    pub kind: OpaqueKind,
    pub opaque_ref: String,
    pub proof_ref: ProofRef,
    /// Formatting of the run wrapper that originally contained this opaque inline.
    /// Required when serialization must synthesize a `w:r` wrapper around
    /// run-level opaque elements like `w:fldChar` or `w:instrText`.
    pub wrapper_marks: Vec<Mark>,
    pub wrapper_style_props: StyleProps,
    /// Raw XML bytes for roundtripping opaque elements.
    /// Used to reconstruct the element during redline generation.
    pub raw_xml: Option<Vec<u8>>,
    /// SHA-256 of raw_xml bytes — enables comparing image/object identity.
    pub content_hash: Option<String>,
}

/// Zero-width decoration node (bookmarks, comments, etc.).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DecorationNode {
    pub id: NodeId,
    pub kind: DecorationType,
    pub opaque_ref: String,
    pub proof_ref: ProofRef,
    /// Formatting of the run wrapper that originally contained this decoration,
    /// for the glyph-rendering note marks (`w:footnoteRef`/`w:endnoteRef`
    /// §17.11.6/§17.11.1, `w:separator`/`w:continuationSeparator`,
    /// `w:annotationRef`). Each of these lives alone in a `w:r` whose `w:rPr`
    /// carries the character style, fonts and size Word renders the auto-number
    /// / separator / ref mark in; the decoration's `raw_xml` holds only the bare
    /// marker element, so the wrapper `rPr` MUST be captured here and
    /// re-synthesized around the marker on serialization — otherwise the wrapper
    /// collapses to a bare `<w:r>` and the rPr is silently dropped on every
    /// rebuild of the story. Empty for decorations with no independent host-run
    /// rPr: paragraph-level markers (bookmarks, comment/move/customXml ranges)
    /// and the non-glyph run decorations (`w:lastRenderedPageBreak`,
    /// `w:softHyphen`) that share a run with adjacent text (see
    /// `import::decoration_wrapper_rpr_is_load_bearing`). Mirrors
    /// [`OpaqueInlineNode::wrapper_marks`]/`wrapper_style_props`.
    #[serde(default)]
    pub wrapper_marks: Vec<Mark>,
    #[serde(default)]
    pub wrapper_style_props: StyleProps,
    /// Raw XML bytes for roundtripping the original element.
    pub raw_xml: Option<Vec<u8>>,
    /// Document origin override for the serializer's bookmark id policy
    /// (`serialize::BookmarkIdPolicy`). `None` means inherit from the block
    /// (Inserted block → "target", otherwise "base"). Values the pipeline
    /// produces:
    /// - "target": a target-document decoration collected into a base
    ///   (Normal/Deleted) block during merge — its id belongs to the target's
    ///   id space and the pair is remapped/deduped as a unit.
    /// - "authored": synthesized by edit verbs under a placeholder id; the
    ///   serializer assigns the real part-unique id at write time.
    ///
    /// Base-origin decorations always keep their original ids verbatim.
    pub origin: Option<String>,
}

/// Type of decoration element.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum DecorationType {
    Bookmark,
    CommentRange,
    PermissionRange,
    ProofError,
    CustomXmlRange,
    MoveRange,
    /// A bidirectional display-only wrapper (`w:bdo` §17.3.2.3 / `w:dir`
    /// §17.3.2.8) decomposed into a start/end marker pair. The inner runs are
    /// ordinary text nodes between the markers; the serializer re-nests them
    /// back into the wrapper on round-trip (see `renest_inline_*`).
    BidiWrapper,
    /// An inline custom-XML / smart-tag wrapper (`w:customXml` §17.5.1.3 /
    /// `w:smartTag` §17.5.1.9) decomposed into a start/end marker pair. These
    /// are TRANSPARENT semantic containers: their inner runs are ordinary
    /// document text and any inner revisions (`w:del`, `w:moveFrom`/`w:moveTo`)
    /// are ordinary revisions that resolve on accept/reject. The wrapper itself
    /// carries no text; the start/end markers carry the childless wrapper bytes
    /// (attributes + `customXmlPr`/`smartTagPr`, content children cleared) so
    /// the serializer can re-nest the intervening content on round-trip (see
    /// `renest_inline_custom_xml_wrappers`). One variant covers both elements;
    /// the marker's raw XML distinguishes customXml from smartTag — mirroring
    /// how `BidiWrapper` covers both `w:bdo` and `w:dir`.
    ///
    /// NOTE: appended LAST so serialized snapshots that encode the enum by
    /// variant index stay readable.
    CustomXmlWrapper,
    /// A zero-width element in a FOREIGN namespace (not WML core, not a known
    /// OOXML/Microsoft extension, not the MCE namespace) appearing at paragraph
    /// level. Third-party tools (e.g. OpenXML PowerTools / Templafy
    /// DocumentBuilder, namespace
    /// `http://powertools.codeplex.com/documentbuilder/2011/insert`) inject such
    /// placeholder markers as direct children of `w:p`, outside the `CT_P` schema.
    /// We do not model the extension's semantics, but we MUST NOT drop it (no
    /// silent fallback) nor guess its meaning: it is preserved verbatim via
    /// `raw_xml` and occupies zero logical width, round-tripping byte-for-byte.
    /// Distinct from `Bookmark`/etc. so the model never mislabels foreign markup
    /// as a WML construct.
    ///
    /// NOTE: appended LAST so serialized snapshots that encode the enum by
    /// variant index stay readable.
    ForeignElement,
    /// The CLOSE marker of a [`DecorationType::CustomXmlWrapper`] pair. The
    /// open/close polarity is known at import (the atoms are
    /// `CustomXmlWrapperStart`/`End`) and MUST be carried in the model:
    /// nested SAME-name wrappers (`smartTag` inside `smartTag` — Word's own
    /// `place` > `PlaceName` emission) are indistinguishable by name alone,
    /// and the serializer's re-nesting pass would otherwise pair an outer
    /// open with an inner open, flattening the structure and stranding the
    /// outer close as an empty element. Pre-polarity snapshots decode both
    /// markers as `CustomXmlWrapper`; the re-nesting pass keeps its
    /// name-stack pairing as the fallback for exactly that case.
    ///
    /// NOTE: appended LAST so serialized snapshots that encode the enum by
    /// variant index stay readable.
    CustomXmlWrapperEnd,
}

/// A formatting mark (kept for backwards compatibility).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum Mark {
    Bold,
    Italic,
    Underline,
    Subscript,
    Superscript,
}

/// Tri-state value for formatting properties.
/// OOXML formatting can be: inherit (absent), explicitly on, or explicitly off.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub enum MarkValue {
    /// Property absent - inherit from style.
    #[default]
    Inherit,
    /// Explicitly enabled (<w:b/> or <w:b w:val="1"/>).
    On,
    /// Explicitly disabled (<w:b w:val="0"/>).
    Off,
}

/// Tri-state formatting marks for text runs.
/// Captures the explicit/inherit state to avoid noisy diffs.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
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
    pub emboss: MarkValue,
    pub imprint: MarkValue,
    pub outline: MarkValue,
    pub shadow: MarkValue,
    /// Font family from w:rFonts (w:ascii or w:hAnsi attribute).
    pub font_family: Option<IStr>,
    /// Font size in half-points from w:sz (e.g., 24 = 12pt).
    pub font_size: Option<u32>,
    /// Text color as hex RGB from w:color w:val (e.g., "FF0000").
    pub color: Option<IStr>,
    /// Theme color reference from w:color (themeColor/themeShade/themeTint).
    pub color_theme: Option<ThemeColorRef>,
    /// Highlight color per §17.18.40 `ST_HighlightColor`.
    pub highlight: Option<HighlightColor>,
    /// Underline style from w:u w:val per §17.18.99 `ST_Underline`.
    pub underline_style: Option<UnderlineStyle>,
    /// East Asian font family from w:rFonts w:eastAsia.
    pub font_east_asia: Option<IStr>,
    /// Complex script font family from w:rFonts w:cs.
    pub font_cs: Option<IStr>,
    /// Language tag from w:lang w:val (e.g., "en-US").
    pub lang: Option<IStr>,
    /// East Asian language tag from w:lang w:eastAsia (e.g., "ja-JP").
    pub lang_east_asia: Option<IStr>,
    /// Character spacing in twips from w:spacing w:val in rPr.
    pub char_spacing: Option<i32>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum Alignment {
    Left,
    Center,
    Right,
    Justify,
    Distribute,
    HighKashida,
    LowKashida,
    MediumKashida,
    NumTab,
    ThaiDistribute,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum OpaqueKind {
    Drawing,
    SmartArt,
    Sdt,
    Field(FieldData),
    /// Block-level display math (`m:oMathPara`) — must be a direct child of `w:p`.
    OmmlBlock,
    /// Inline math (`m:oMath`) — run-level, can appear inside `w:del`/`w:ins`.
    OmmlInline,
    Ruby,
    Hyperlink(HyperlinkData),
    CommentReference(NoteReferenceData),
    FootnoteReference(NoteReferenceData),
    EndnoteReference(NoteReferenceData),
    SmartTag,
    /// Symbol character (`w:sym`) — a character from a specific font, not the run font.
    Sym(SymData),
    /// Absolute position tab (`w:ptab`) — advances to a calculated position.
    Ptab,
    /// Custom XML wrapper (`w:customXml`) — transparent container around inline content.
    CustomXml,
    Unknown(String),
    /// A body item quarantined at import because it contains a tracked-change
    /// container nested inside another (`w:del` inside `w:ins` — another
    /// author's pending deletion of pending-inserted text), which the IR
    /// cannot represent until stacked revisions land. The item is preserved
    /// byte-faithfully via the body-template opaque machinery and is
    /// structurally uneditable; the read view shows an explicit placeholder,
    /// never the inner revisions as uncontested content.
    ///
    /// NOTE: deliberately appended LAST — bincode snapshot blobs encode
    /// variant indices, so inserting mid-enum would break old blobs.
    QuarantinedNestedTracking,
}

/// Metadata for a symbol character element (`w:sym`).
///
/// Per ECMA-376 §17.3.3.30, a sym element specifies a character from a named font,
/// overriding the run's rFonts. The `char` attribute is a hex codepoint, possibly
/// shifted into the Private Use Area (U+F000..U+F0FF) for legacy font compatibility.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SymData {
    /// Font name from `w:font` attribute (e.g., "Symbol", "Wingdings").
    pub font: String,
    /// Raw hex codepoint from `w:char` attribute (e.g., "F03A").
    pub char_code: String,
    /// The decoded Unicode character, with F000 PUA offset stripped when present.
    /// This is the display character that should be rendered.
    pub display_char: char,
}

/// Metadata for footnote/endnote/comment reference elements.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct NoteReferenceData {
    /// The w:id attribute linking this reference to its story.
    pub reference_id: String,
}

/// Metadata for field elements (w:fldChar, w:instrText, w:fldSimple).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct FieldData {
    /// Kind of field element.
    pub field_kind: FieldKind,
    /// Instruction text (from w:instr attribute on fldSimple, or from instrText content).
    pub instruction_text: Option<String>,
    /// Display/result text (nested content of fldSimple, or content between separate and end).
    pub result_text: Option<String>,
    /// Typed semantic classification for field kinds we intentionally model.
    pub semantic: Option<FieldSemantic>,
}

/// Discriminant for which part of a field this element represents.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum FieldKind {
    /// w:fldChar with fldCharType="begin"
    Begin,
    /// w:instrText or w:delInstrText content
    Instruction,
    /// w:fldChar with fldCharType="separate"
    Separate,
    /// w:fldChar with fldCharType="end"
    End,
    /// w:fldSimple (self-contained field)
    Simple,
    /// w:fldChar with an `fldCharType` outside the ST_FldCharType value domain
    /// (begin|separate|end), §17.18.29. Carries the raw type string verbatim.
    ///
    /// This is an EXPLICIT, honest representation of an unrecognized field-char
    /// type — NOT a silent fallback. Word (the consumption oracle) opens such a
    /// document clean without repair, so refusing the whole file would be the
    /// wrong granularity of "fail fast". We therefore model the unknown state in
    /// the type system (rather than coercing it to begin/separate/end or dropping
    /// it), preserve the element byte-verbatim via the opaque anchor's `raw_xml`,
    /// and NEVER treat it as a begin/separate/end field boundary.
    Unknown(String),
}

/// Typed semantic classification for known field instructions (ECMA-376 §17.16).
///
/// Tier 1 (fully structured): Hyperlink, MergeField, Ref, DateTime — the most
/// common fields in legal templates. Tier 2 (structure-only): If, Formula, Toc —
/// we model the shape but don't interpret expressions. Tier 3 (`Other`):
/// preserves field name + tokenized args without per-field parsing.
///
/// `Other` is NOT a fallback for malformed Tier 1/2 instructions — those produce
/// a `FieldParseError` at the import boundary.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum FieldSemantic {
    Toc(TocFieldSpec),
    Hyperlink(HyperlinkFieldSpec),
    MergeField(MergeFieldSpec),
    Ref(RefFieldSpec),
    DateTime(DateTimeFieldSpec),
    If(IfFieldSpec),
    Formula(FormulaFieldSpec),
    /// Tier 3: known-field-name catch-all that preserves args. Never used as
    /// a fallback for Tier 1/2 parse failures — those return `Err`.
    Other {
        field_name: String,
        raw_args: Vec<FieldArg>,
    },
}

/// One token from a field instruction lex pass. Preserves quoting so
/// roundtrip is faithful (`MERGEFIELD "Company Name"` differs from
/// `MERGEFIELD CompanyName` for whitespace-containing names).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum FieldArg {
    /// `\X` switch where X is one ASCII letter. `escape` is normally `\\`.
    Switch { letter: char, escape: char },
    /// Bare unquoted token.
    Bare(String),
    /// `"..."` quoted token (content without the surrounding quotes).
    Quoted(String),
}

/// Common formatting switches that recur across field types
/// (`\* MERGEFORMAT`, `\# "0.00"`, `\@ "yyyy-MM-dd"`).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct FormatSwitches {
    /// `\*` general format (MERGEFORMAT, Upper, Lower, FirstCap, Caps, ...).
    pub general: Option<String>,
    /// `\#` numeric picture.
    pub numeric: Option<String>,
    /// `\@` date-time picture.
    pub date_time: Option<String>,
    /// Order in which the switches appeared (rare, but preserved for fidelity).
    pub order: Vec<FormatSwitchKind>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatSwitchKind {
    General,
    Numeric,
    DateTime,
}

/// Where a hyperlink points. Shared between `<w:hyperlink>` and HYPERLINK
/// field representations (the same domain concept expressed via two
/// unrelated XML mechanisms in the spec).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum HyperlinkTarget {
    /// External URL (`HYPERLINK "url"` or `<w:hyperlink r:id=...>`).
    Url { url: String },
    /// Internal bookmark (`HYPERLINK \l "name"` or `<w:hyperlink w:anchor="name">`).
    Bookmark { anchor: String },
    /// URL plus a sub-anchor inside it (`HYPERLINK "url" \l "frag"`).
    UrlWithBookmark { url: String, anchor: String },
}

/// HYPERLINK field (§17.16.5.25).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct HyperlinkFieldSpec {
    pub target: HyperlinkTarget,
    /// `\o "tooltip"` — screen tip displayed on hover.
    pub tooltip: Option<String>,
    /// `\t "frame"` — target HTML frame.
    pub target_frame: Option<String>,
    /// `\n` — open in new browser window / suppress history.
    pub no_history: bool,
    /// `\m` — image map / coordinates.
    pub image_map: bool,
    pub format: FormatSwitches,
}

/// MERGEFIELD field (§17.16.5.35) — mail-merge data substitution.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct MergeFieldSpec {
    /// Data field name to substitute.
    pub field_name: String,
    /// `\b "before"` — text inserted before non-empty value.
    pub before_text: Option<String>,
    /// `\f "after"` — text inserted after non-empty value.
    pub after_text: Option<String>,
    /// `\m` — map via merge database.
    pub map_via_db: bool,
    /// `\v` — vertical format (East Asian).
    pub vertical_format: bool,
    pub format: FormatSwitches,
}

/// Discriminator for REF / PAGEREF / NOREF (§17.16.5.45 / .39 / .36).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefKind {
    Ref,
    PageRef,
    NoRef,
}

/// REF / PAGEREF / NOREF field — cross-reference to a bookmark.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct RefFieldSpec {
    pub kind: RefKind,
    pub bookmark: String,
    /// `\h` — insert as hyperlink.
    pub insert_hyperlink: bool,
    /// `\n` — paragraph number suppression.
    pub no_paragraph_number: bool,
    /// `\r` — relative paragraph number.
    pub paragraph_number_relative: bool,
    /// `\w` — full-context paragraph number.
    pub paragraph_number_full: bool,
    /// `\t` — suppress non-delimiter (e.g. preceding text).
    pub suppress_non_delimiter: bool,
    /// `\p` — above/below position.
    pub above_below: bool,
    pub format: FormatSwitches,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum DateTimeKind {
    Date,
    Time,
}

/// DATE / TIME field (§17.16.5.16 / .51).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DateTimeFieldSpec {
    pub kind: DateTimeKind,
    /// `\l` — use last-used format.
    pub use_last_format: bool,
    /// `\s` — Saka era (Indian calendar).
    pub use_saka_era: bool,
    /// `\h` — Hijri/Lunar calendar.
    pub use_hijri: bool,
    /// `\@ "format"` lives inside `format.date_time`.
    pub format: FormatSwitches,
}

/// IF field (§17.16.5.27). Tier 2: we tokenize the expression but do not
/// evaluate it. `expression_text` is the raw string between `IF` and the
/// first quoted alternative.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct IfFieldSpec {
    pub expression_text: String,
    pub true_text: String,
    pub false_text: String,
    pub format: FormatSwitches,
}

/// FORMULA / `=` field (§17.16.5.20). Tier 2: structure-only.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct FormulaFieldSpec {
    pub expression_text: String,
    pub format: FormatSwitches,
}

/// Semantic configuration for a TOC field.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TocFieldSpec {
    /// Heading levels to include via the `\o` field switch.
    pub levels: TocLevelsSpec,
    /// Include hyperlinks in TOC entries (`\h`).
    pub include_hyperlinks: bool,
    /// Hide page numbers in web layout (`\z`).
    pub hide_page_numbers_in_web: bool,
    /// Use outline levels in addition to built-in heading styles (`\u`).
    pub use_outline_levels: bool,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct TocLevelsSpec {
    pub from: u8,
    pub to: u8,
}

/// Reasons a Tier 1/2 field instruction failed to parse. Caller should
/// surface these as `Diagnostic::Error` at the import boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldParseError {
    /// Field name was missing — instruction was empty or whitespace-only.
    EmptyInstruction,
    /// HYPERLINK with no URL or bookmark.
    HyperlinkMissingTarget,
    /// MERGEFIELD with no field name.
    MergeFieldMissingName,
    /// REF/PAGEREF/NOREF with no bookmark argument.
    RefMissingBookmark { field_name: String },
    /// IF without enough arguments to satisfy `IF expr "true" "false"`.
    IfMissingArgs,
    /// FORMULA with no expression body.
    FormulaMissingExpression,
    /// TOC's `\o "from-to"` argument is missing or malformed.
    TocLevelsInvalid,
}

impl std::fmt::Display for FieldParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyInstruction => write!(f, "field instruction is empty"),
            Self::HyperlinkMissingTarget => write!(f, "HYPERLINK is missing a URL or bookmark"),
            Self::MergeFieldMissingName => write!(f, "MERGEFIELD is missing a field name"),
            Self::RefMissingBookmark { field_name } => {
                write!(f, "{field_name} is missing a bookmark argument")
            }
            Self::IfMissingArgs => {
                write!(f, "IF requires expression and true/false text arguments")
            }
            Self::FormulaMissingExpression => write!(f, "= (FORMULA) is missing an expression"),
            Self::TocLevelsInvalid => {
                write!(f, "TOC \\o levels argument is missing or malformed")
            }
        }
    }
}

impl std::error::Error for FieldParseError {}

impl FieldData {
    pub fn semantic_toc_spec(&self) -> Option<&TocFieldSpec> {
        match &self.semantic {
            Some(FieldSemantic::Toc(spec)) => Some(spec),
            _ => None,
        }
    }
}

impl FieldSemantic {
    /// Reconstruct the canonical instruction text from a typed semantic.
    /// Used by the serializer when emitting `<w:fldSimple w:instr="...">`.
    pub fn to_instruction_text(&self) -> String {
        match self {
            Self::Toc(spec) => spec.instruction_text(),
            Self::Hyperlink(spec) => spec.to_instruction_text(),
            Self::MergeField(spec) => spec.to_instruction_text(),
            Self::Ref(spec) => spec.to_instruction_text(),
            Self::DateTime(spec) => spec.to_instruction_text(),
            Self::If(spec) => spec.to_instruction_text(),
            Self::Formula(spec) => spec.to_instruction_text(),
            Self::Other {
                field_name,
                raw_args,
            } => write_field_instruction(field_name, raw_args.iter()),
        }
    }
}

impl TocFieldSpec {
    pub fn instruction_text(&self) -> String {
        let mut instruction = format!("TOC \\o \"{}-{}\"", self.levels.from, self.levels.to);
        if self.include_hyperlinks {
            instruction.push_str(" \\h");
        }
        if self.hide_page_numbers_in_web {
            instruction.push_str(" \\z");
        }
        if self.use_outline_levels {
            instruction.push_str(" \\u");
        }
        instruction
    }
}

impl HyperlinkFieldSpec {
    pub fn to_instruction_text(&self) -> String {
        let mut s = String::from("HYPERLINK");
        match &self.target {
            HyperlinkTarget::Url { url } => {
                push_quoted(&mut s, url);
            }
            HyperlinkTarget::Bookmark { anchor } => {
                s.push_str(" \\l");
                push_quoted(&mut s, anchor);
            }
            HyperlinkTarget::UrlWithBookmark { url, anchor } => {
                push_quoted(&mut s, url);
                s.push_str(" \\l");
                push_quoted(&mut s, anchor);
            }
        }
        if let Some(tooltip) = &self.tooltip {
            s.push_str(" \\o");
            push_quoted(&mut s, tooltip);
        }
        if let Some(frame) = &self.target_frame {
            s.push_str(" \\t");
            push_quoted(&mut s, frame);
        }
        if self.no_history {
            s.push_str(" \\n");
        }
        if self.image_map {
            s.push_str(" \\m");
        }
        push_format_switches(&mut s, &self.format);
        s
    }
}

impl MergeFieldSpec {
    pub fn to_instruction_text(&self) -> String {
        let mut s = String::from("MERGEFIELD");
        if needs_quoting(&self.field_name) {
            push_quoted(&mut s, &self.field_name);
        } else {
            s.push(' ');
            s.push_str(&self.field_name);
        }
        if let Some(before) = &self.before_text {
            s.push_str(" \\b");
            push_quoted(&mut s, before);
        }
        if let Some(after) = &self.after_text {
            s.push_str(" \\f");
            push_quoted(&mut s, after);
        }
        if self.map_via_db {
            s.push_str(" \\m");
        }
        if self.vertical_format {
            s.push_str(" \\v");
        }
        push_format_switches(&mut s, &self.format);
        s
    }
}

impl RefFieldSpec {
    pub fn to_instruction_text(&self) -> String {
        let mut s = match self.kind {
            RefKind::Ref => String::from("REF"),
            RefKind::PageRef => String::from("PAGEREF"),
            RefKind::NoRef => String::from("NOREF"),
        };
        if needs_quoting(&self.bookmark) {
            push_quoted(&mut s, &self.bookmark);
        } else {
            s.push(' ');
            s.push_str(&self.bookmark);
        }
        if self.insert_hyperlink {
            s.push_str(" \\h");
        }
        if self.no_paragraph_number {
            s.push_str(" \\n");
        }
        if self.paragraph_number_relative {
            s.push_str(" \\r");
        }
        if self.paragraph_number_full {
            s.push_str(" \\w");
        }
        if self.suppress_non_delimiter {
            s.push_str(" \\t");
        }
        if self.above_below {
            s.push_str(" \\p");
        }
        push_format_switches(&mut s, &self.format);
        s
    }
}

impl DateTimeFieldSpec {
    pub fn to_instruction_text(&self) -> String {
        let mut s = match self.kind {
            DateTimeKind::Date => String::from("DATE"),
            DateTimeKind::Time => String::from("TIME"),
        };
        if self.use_last_format {
            s.push_str(" \\l");
        }
        if self.use_saka_era {
            s.push_str(" \\s");
        }
        if self.use_hijri {
            s.push_str(" \\h");
        }
        push_format_switches(&mut s, &self.format);
        s
    }
}

impl IfFieldSpec {
    pub fn to_instruction_text(&self) -> String {
        let mut s = String::from("IF ");
        s.push_str(&self.expression_text);
        push_quoted(&mut s, &self.true_text);
        push_quoted(&mut s, &self.false_text);
        push_format_switches(&mut s, &self.format);
        s
    }
}

impl FormulaFieldSpec {
    pub fn to_instruction_text(&self) -> String {
        let mut s = String::from("= ");
        s.push_str(&self.expression_text);
        push_format_switches(&mut s, &self.format);
        s
    }
}

fn needs_quoting(text: &str) -> bool {
    text.is_empty() || text.contains(|c: char| c.is_whitespace() || c == '"' || c == '\\')
}

fn push_quoted(out: &mut String, text: &str) {
    out.push_str(" \"");
    for c in text.chars() {
        if c == '"' {
            out.push('"');
            out.push('"');
        } else {
            out.push(c);
        }
    }
    out.push('"');
}

fn push_format_switches(out: &mut String, fmt: &FormatSwitches) {
    let order = if fmt.order.is_empty() {
        let mut o = Vec::new();
        if fmt.general.is_some() {
            o.push(FormatSwitchKind::General);
        }
        if fmt.numeric.is_some() {
            o.push(FormatSwitchKind::Numeric);
        }
        if fmt.date_time.is_some() {
            o.push(FormatSwitchKind::DateTime);
        }
        o
    } else {
        fmt.order.clone()
    };
    for kind in order {
        match kind {
            FormatSwitchKind::General => {
                if let Some(v) = &fmt.general {
                    out.push_str(" \\*");
                    push_quoted_or_bare(out, v);
                }
            }
            FormatSwitchKind::Numeric => {
                if let Some(v) = &fmt.numeric {
                    out.push_str(" \\#");
                    push_quoted_or_bare(out, v);
                }
            }
            FormatSwitchKind::DateTime => {
                if let Some(v) = &fmt.date_time {
                    out.push_str(" \\@");
                    push_quoted_or_bare(out, v);
                }
            }
        }
    }
}

fn push_quoted_or_bare(out: &mut String, text: &str) {
    if needs_quoting(text) {
        push_quoted(out, text);
    } else {
        out.push(' ');
        out.push_str(text);
    }
}

fn write_field_instruction<'a>(name: &str, args: impl Iterator<Item = &'a FieldArg>) -> String {
    let mut s = String::from(name);
    for arg in args {
        match arg {
            FieldArg::Switch { letter, escape } => {
                s.push(' ');
                s.push(*escape);
                s.push(*letter);
            }
            FieldArg::Bare(text) => {
                s.push(' ');
                s.push_str(text);
            }
            FieldArg::Quoted(text) => {
                push_quoted(&mut s, text);
            }
        }
    }
    s
}

// ── Field instruction lexer ───────────────────────────────────────────────
//
// `lex_field` produces a `Vec<FieldArg>` from an instruction string. It
// recognises `\X` switches (single-letter, ASCII), bare tokens, and
// `"..."` quoted strings with `""` (Word's escaping convention) treated
// as a literal `"`. Whitespace separates tokens; whitespace inside a
// quoted string is preserved verbatim.

/// Lex a field instruction into typed args. Always returns Ok input;
/// per-field validation happens in `parse_*` functions downstream.
pub fn lex_field(instruction: &str) -> Vec<FieldArg> {
    let mut out = Vec::new();
    let mut chars = instruction.chars().peekable();

    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        if c == '\\' {
            chars.next(); // consume the escape char
            if let Some(&letter) = chars.peek()
                && (letter.is_ascii_alphabetic() || matches!(letter, '*' | '#' | '@'))
            {
                chars.next();
                out.push(FieldArg::Switch {
                    letter: if letter.is_ascii_alphabetic() {
                        letter.to_ascii_lowercase()
                    } else {
                        letter
                    },
                    escape: '\\',
                });
                continue;
            }
            // Lone backslash — treat as bare token starting with '\'.
            let mut bare = String::from('\\');
            while let Some(&nc) = chars.peek() {
                if nc.is_whitespace() {
                    break;
                }
                bare.push(nc);
                chars.next();
            }
            out.push(FieldArg::Bare(bare));
            continue;
        }
        if c == '"' {
            chars.next();
            let mut quoted = String::new();
            while let Some(&nc) = chars.peek() {
                if nc == '"' {
                    chars.next();
                    if chars.peek() == Some(&'"') {
                        // Doubled quote = literal '"' inside a quoted string.
                        chars.next();
                        quoted.push('"');
                        continue;
                    }
                    break;
                }
                quoted.push(nc);
                chars.next();
            }
            out.push(FieldArg::Quoted(quoted));
            continue;
        }
        // Bare token.
        let mut bare = String::new();
        while let Some(&nc) = chars.peek() {
            if nc.is_whitespace() {
                break;
            }
            bare.push(nc);
            chars.next();
        }
        out.push(FieldArg::Bare(bare));
    }

    out
}

/// Pull the field name (first bare token) off the front. Returns None
/// when the instruction is empty or the first token isn't a bare word.
pub fn split_field_name(args: &[FieldArg]) -> Option<(&str, &[FieldArg])> {
    let (head, rest) = args.split_first()?;
    if let FieldArg::Bare(name) = head {
        Some((name.as_str(), rest))
    } else {
        None
    }
}

/// Consume `\* ...`, `\# ...`, `\@ ...` switches off the front of `rest`,
/// in any interleaving. Returns the parsed switches plus the remaining
/// args (non-format switches preserved in order).
fn extract_format_switches(rest: &[FieldArg]) -> (FormatSwitches, Vec<FieldArg>) {
    let mut fmt = FormatSwitches::default();
    let mut leftover = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        if let FieldArg::Switch { letter, .. } = &rest[i]
            && matches!(*letter, '*' | '#' | '@')
        {
            // Need an argument value.
            let value = match rest.get(i + 1) {
                Some(FieldArg::Bare(s)) => Some(s.clone()),
                Some(FieldArg::Quoted(s)) => Some(s.clone()),
                _ => None,
            };
            if let Some(v) = value {
                match *letter {
                    '*' => {
                        fmt.general = Some(v);
                        fmt.order.push(FormatSwitchKind::General);
                    }
                    '#' => {
                        fmt.numeric = Some(v);
                        fmt.order.push(FormatSwitchKind::Numeric);
                    }
                    '@' => {
                        fmt.date_time = Some(v);
                        fmt.order.push(FormatSwitchKind::DateTime);
                    }
                    _ => unreachable!(),
                }
                i += 2;
                continue;
            }
            // Format switch with no argument → drop into leftover for later
            // visibility (parse_field_instruction will not consider it part
            // of the typed format set).
        }
        leftover.push(rest[i].clone());
        i += 1;
    }
    (fmt, leftover)
}

/// Parse a field instruction string into a typed `FieldSemantic`.
///
/// Returns `Err` for malformed Tier 1/2 fields. Unknown field names land
/// in `FieldSemantic::Other` so they roundtrip without losing the field
/// name or args.
pub fn parse_field_instruction(instruction: &str) -> Result<FieldSemantic, FieldParseError> {
    let args = lex_field(instruction);
    if args.is_empty() {
        return Err(FieldParseError::EmptyInstruction);
    }
    // FORMULA's `=` shorthand: the field name is `=` and the rest is the expression.
    if let FieldArg::Bare(name) = &args[0]
        && name == "="
    {
        return parse_formula(&args[1..]);
    }
    let (name, rest) = split_field_name(&args).ok_or(FieldParseError::EmptyInstruction)?;
    let upper = name.to_ascii_uppercase();
    match upper.as_str() {
        "TOC" => parse_toc(rest),
        "HYPERLINK" => parse_hyperlink(rest),
        "MERGEFIELD" => parse_mergefield(rest),
        "REF" => parse_ref(RefKind::Ref, "REF", rest),
        "PAGEREF" => parse_ref(RefKind::PageRef, "PAGEREF", rest),
        "NOREF" => parse_ref(RefKind::NoRef, "NOREF", rest),
        "DATE" => Ok(parse_date_time(DateTimeKind::Date, rest)),
        "TIME" => Ok(parse_date_time(DateTimeKind::Time, rest)),
        "IF" => parse_if(rest),
        "FORMULA" => parse_formula(rest),
        _ => Ok(FieldSemantic::Other {
            field_name: name.to_string(),
            raw_args: rest.to_vec(),
        }),
    }
}

fn first_non_switch_value(args: &[FieldArg]) -> Option<(usize, String)> {
    for (i, a) in args.iter().enumerate() {
        match a {
            FieldArg::Bare(s) | FieldArg::Quoted(s) => return Some((i, s.clone())),
            FieldArg::Switch { .. } => continue,
        }
    }
    None
}

fn parse_toc(rest: &[FieldArg]) -> Result<FieldSemantic, FieldParseError> {
    let mut levels = None;
    let mut include_hyperlinks = false;
    let mut hide_page_numbers_in_web = false;
    let mut use_outline_levels = false;
    let mut i = 0;
    while i < rest.len() {
        if let FieldArg::Switch { letter, .. } = &rest[i] {
            match *letter {
                'o' => {
                    let next = rest.get(i + 1);
                    if let Some(FieldArg::Bare(s) | FieldArg::Quoted(s)) = next {
                        levels = parse_toc_levels(s);
                        i += 2;
                        continue;
                    }
                    return Err(FieldParseError::TocLevelsInvalid);
                }
                'h' => include_hyperlinks = true,
                'z' => hide_page_numbers_in_web = true,
                'u' => use_outline_levels = true,
                _ => {}
            }
        }
        i += 1;
    }
    let levels = levels.ok_or(FieldParseError::TocLevelsInvalid)?;
    Ok(FieldSemantic::Toc(TocFieldSpec {
        levels,
        include_hyperlinks,
        hide_page_numbers_in_web,
        use_outline_levels,
    }))
}

fn parse_toc_levels(token: &str) -> Option<TocLevelsSpec> {
    let trimmed = token.trim_matches('"');
    let (from, to) = trimmed.split_once('-')?;
    // Word tolerates whitespace around the bounds (`\o "1 - 9"` occurs in
    // real documents) — trim before parsing.
    let from = from.trim().parse().ok()?;
    let to = to.trim().parse().ok()?;
    if from == 0 || from > to {
        return None;
    }
    Some(TocLevelsSpec { from, to })
}

fn parse_hyperlink(rest: &[FieldArg]) -> Result<FieldSemantic, FieldParseError> {
    let (format, rest) = extract_format_switches(rest);
    // Identify the URL value (first bare/quoted not preceded by an unrelated switch)
    // and the bookmark anchor (value following \l).
    let mut url: Option<String> = None;
    let mut anchor: Option<String> = None;
    let mut tooltip: Option<String> = None;
    let mut target_frame: Option<String> = None;
    let mut no_history = false;
    let mut image_map = false;

    let mut i = 0;
    while i < rest.len() {
        match &rest[i] {
            FieldArg::Switch { letter, .. } => match *letter {
                'l' => {
                    if let Some(FieldArg::Bare(s) | FieldArg::Quoted(s)) = rest.get(i + 1) {
                        anchor = Some(s.clone());
                        i += 2;
                        continue;
                    }
                }
                'o' => {
                    if let Some(FieldArg::Bare(s) | FieldArg::Quoted(s)) = rest.get(i + 1) {
                        tooltip = Some(s.clone());
                        i += 2;
                        continue;
                    }
                }
                't' => {
                    if let Some(FieldArg::Bare(s) | FieldArg::Quoted(s)) = rest.get(i + 1) {
                        target_frame = Some(s.clone());
                        i += 2;
                        continue;
                    }
                }
                'n' => no_history = true,
                'm' => image_map = true,
                _ => {}
            },
            FieldArg::Bare(s) | FieldArg::Quoted(s) => {
                if url.is_none() {
                    url = Some(s.clone());
                }
            }
        }
        i += 1;
    }

    let target = match (url, anchor) {
        (Some(url), Some(anchor)) => HyperlinkTarget::UrlWithBookmark { url, anchor },
        (Some(url), None) => HyperlinkTarget::Url { url },
        (None, Some(anchor)) => HyperlinkTarget::Bookmark { anchor },
        (None, None) => return Err(FieldParseError::HyperlinkMissingTarget),
    };

    Ok(FieldSemantic::Hyperlink(HyperlinkFieldSpec {
        target,
        tooltip,
        target_frame,
        no_history,
        image_map,
        format,
    }))
}

fn parse_mergefield(rest: &[FieldArg]) -> Result<FieldSemantic, FieldParseError> {
    let (format, rest) = extract_format_switches(rest);
    let (name_idx, name) =
        first_non_switch_value(&rest).ok_or(FieldParseError::MergeFieldMissingName)?;

    let mut before_text: Option<String> = None;
    let mut after_text: Option<String> = None;
    let mut map_via_db = false;
    let mut vertical_format = false;

    let mut i = 0;
    while i < rest.len() {
        if i == name_idx {
            i += 1;
            continue;
        }
        if let FieldArg::Switch { letter, .. } = &rest[i] {
            match *letter {
                'b' => {
                    if let Some(FieldArg::Bare(s) | FieldArg::Quoted(s)) = rest.get(i + 1) {
                        before_text = Some(s.clone());
                        i += 2;
                        continue;
                    }
                }
                'f' => {
                    if let Some(FieldArg::Bare(s) | FieldArg::Quoted(s)) = rest.get(i + 1) {
                        after_text = Some(s.clone());
                        i += 2;
                        continue;
                    }
                }
                'm' => map_via_db = true,
                'v' => vertical_format = true,
                _ => {}
            }
        }
        i += 1;
    }

    Ok(FieldSemantic::MergeField(MergeFieldSpec {
        field_name: name,
        before_text,
        after_text,
        map_via_db,
        vertical_format,
        format,
    }))
}

fn parse_ref(
    kind: RefKind,
    field_name: &str,
    rest: &[FieldArg],
) -> Result<FieldSemantic, FieldParseError> {
    let (format, rest) = extract_format_switches(rest);
    let (bookmark_idx, bookmark) =
        first_non_switch_value(&rest).ok_or_else(|| FieldParseError::RefMissingBookmark {
            field_name: field_name.to_string(),
        })?;

    let mut insert_hyperlink = false;
    let mut no_paragraph_number = false;
    let mut paragraph_number_relative = false;
    let mut paragraph_number_full = false;
    let mut suppress_non_delimiter = false;
    let mut above_below = false;

    for (i, a) in rest.iter().enumerate() {
        if i == bookmark_idx {
            continue;
        }
        if let FieldArg::Switch { letter, .. } = a {
            match *letter {
                'h' => insert_hyperlink = true,
                'n' => no_paragraph_number = true,
                'r' => paragraph_number_relative = true,
                'w' => paragraph_number_full = true,
                't' => suppress_non_delimiter = true,
                'p' => above_below = true,
                _ => {}
            }
        }
    }

    Ok(FieldSemantic::Ref(RefFieldSpec {
        kind,
        bookmark,
        insert_hyperlink,
        no_paragraph_number,
        paragraph_number_relative,
        paragraph_number_full,
        suppress_non_delimiter,
        above_below,
        format,
    }))
}

fn parse_date_time(kind: DateTimeKind, rest: &[FieldArg]) -> FieldSemantic {
    let (format, rest) = extract_format_switches(rest);
    let mut use_last_format = false;
    let mut use_saka_era = false;
    let mut use_hijri = false;
    for a in &rest {
        if let FieldArg::Switch { letter, .. } = a {
            match *letter {
                'l' => use_last_format = true,
                's' => use_saka_era = true,
                'h' => use_hijri = true,
                _ => {}
            }
        }
    }
    FieldSemantic::DateTime(DateTimeFieldSpec {
        kind,
        use_last_format,
        use_saka_era,
        use_hijri,
        format,
    })
}

fn parse_if(rest: &[FieldArg]) -> Result<FieldSemantic, FieldParseError> {
    let (format, rest) = extract_format_switches(rest);
    // Find the first quoted argument — everything before it is the expression.
    let first_quoted = rest.iter().position(|a| matches!(a, FieldArg::Quoted(_)));
    let split = first_quoted.ok_or(FieldParseError::IfMissingArgs)?;
    let expression_args = &rest[..split];
    let tail = &rest[split..];
    let true_text = match tail.first() {
        Some(FieldArg::Quoted(s)) => s.clone(),
        _ => return Err(FieldParseError::IfMissingArgs),
    };
    let false_text = match tail.get(1) {
        Some(FieldArg::Quoted(s)) => s.clone(),
        _ => return Err(FieldParseError::IfMissingArgs),
    };
    let expression_text = write_field_instruction("", expression_args.iter())
        .trim()
        .to_string();
    Ok(FieldSemantic::If(IfFieldSpec {
        expression_text,
        true_text,
        false_text,
        format,
    }))
}

fn parse_formula(rest: &[FieldArg]) -> Result<FieldSemantic, FieldParseError> {
    let (format, rest) = extract_format_switches(rest);
    if rest.is_empty() {
        return Err(FieldParseError::FormulaMissingExpression);
    }
    let expression_text = write_field_instruction("", rest.iter()).trim().to_string();
    Ok(FieldSemantic::Formula(FormulaFieldSpec {
        expression_text,
        format,
    }))
}

/// A single run inside a hyperlink, preserving per-run formatting.
///
/// `status` carries the run's tracked-change state. `Normal` runs serialize
/// as bare `<w:r>` children of `<w:hyperlink>`; `Inserted`/`Deleted` runs
/// serialize wrapped in `<w:ins>`/`<w:del>` envelopes inside the hyperlink
/// per ECMA-376 §17.13.5 (CT_Hyperlink permits EG_PContent which includes
/// `w:ins`/`w:del`). Adjacent same-status runs are grouped into a single
/// envelope at serialization time.
///
/// # Layering invariant: segment-level vs run-level tracking
///
/// A hyperlink can be tracked at one of two layers, never both:
///
/// - **Segment-level**: the whole hyperlink opaque sits inside a
///   `TrackedSegment` with `status: Inserted(_)` or `Deleted(_)`. The
///   semantics is "the entire hyperlink was added/removed". In this
///   case every `HyperlinkRun.status` MUST be `Normal` — the segment's
///   status is the authoritative tracking state and the runs inherit
///   it implicitly.
///
/// - **Run-level**: the enclosing `TrackedSegment.status` is `Normal`
///   and individual runs carry their own `Inserted`/`Deleted` status.
///   The semantics is "the hyperlink exists; its display text is being
///   edited". This is what `EditStep::ReplaceHyperlinkText` produces.
///
/// Mixing the two — e.g. an `Inserted` segment containing a hyperlink
/// with `Deleted` runs — is undefined and breaks accept/reject
/// projection (the segment-level filter drops the whole opaque, so
/// run-level state is unreachable on accept). The construction sites
/// in `edit.rs` (`synthesize_new_hyperlink_inline` for new links;
/// `rewrite_hyperlink_runs` for edits) each pick one layer and stick
/// to it: new-link synthesis emits a `Normal` hyperlink whose entire
/// opaque is wrapped in an `Inserted` parent segment; hyperlink-text
/// edits leave the opaque in a `Normal` segment and edit runs in place.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct HyperlinkRun {
    /// Text content of this run (concatenation of all w:t / w:delText children).
    pub text: String,
    /// Serialized `<w:rPr>` bytes, if the run had a rPr element.
    /// Stored as self-contained XML so it can be round-tripped without
    /// holding a reference to the original parse tree.
    pub rpr_xml: Option<Vec<u8>>,
    /// Tracking status for this run. Defaults to `Normal` for backward
    /// compatibility with snapshots written before in-hyperlink edits
    /// became representable. See the type-level docs for the layering
    /// invariant: run-level tracking is only used when the parent
    /// `TrackedSegment` is `Normal`.
    #[serde(default)]
    pub status: TrackingStatus,
}

/// Hyperlink metadata for roundtripping.
/// Stores the data needed to serialize a hyperlink back to Word XML.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct HyperlinkData {
    /// External URL target (via r:id relationship).
    pub url: Option<String>,
    /// Internal bookmark anchor.
    pub anchor: Option<String>,
    /// Display text of the hyperlink (concatenation of all runs).
    /// Kept for backward-compatibility; prefer `runs` for formatting-aware access.
    pub text: String,
    /// Relationship ID for external URLs (r:id attribute).
    /// Used for roundtripping: preserved from import so the same rId
    /// can be written back on export.
    pub r_id: Option<String>,
    /// Per-run data preserving individual run formatting.
    /// Non-empty when the hyperlink was parsed from a document with w:r children.
    /// Empty only for synthetically constructed hyperlinks.
    pub runs: Vec<HyperlinkRun>,
    /// Extra hyperlink element attributes beyond `r:id` and `w:anchor`
    /// (e.g. `w:history`, `w:tgtFrame`, `w:tooltip`, `w:docLocation`).
    /// Stored as `(qname, value)` pairs for roundtripping.
    pub extra_attrs: Vec<(String, String)>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ProofRef {
    pub part: DocPart,
    pub block_id: NodeId,
    pub docx_anchor: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum DocPart {
    DocumentXml,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TransactionMeta {
    /// Display name for the `w:author` attribute on every emitted
    /// `w:ins`/`w:del`. Required; never defaulted. The export endpoint
    /// rejects calls that don't supply this explicitly.
    pub author: String,
    pub reason: Option<String>,
    pub timestamp_utc: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct RevisionInfo {
    /// The original wire `w:id` this revision imported with (or the id minted
    /// for a wire-`w:id="0"` carrier / an authoring transaction). This is a
    /// per-element OOXML annotation, NOT a document-wide address: Word reuses
    /// `w:id` across unrelated elements, so it is NOT unique. It survives ONLY
    /// as the round-trip/serialization pairing key and as the seed floor for
    /// `runtime::max_revision_id` → the annotation counter. NEVER address a
    /// revision by this value; use [`RevisionInfo::identity`].
    pub revision_id: u32,
    pub author: Option<String>,
    pub date: Option<String>,
    /// Identifier of the `apply_edit` call that created this revision.
    /// Generated server-side when the call is received and stamped on every
    /// new RevisionInfo the call produces, so all tracked changes from a
    /// single LLM rewrite (or any single apply) share a stable group id.
    /// Pre-existing tracked changes (loaded from import) have `None`.
    ///
    /// NOTE: no `#[serde(skip_serializing_if)]` here — bincode serializes by
    /// position, not by name, so any "skip" attribute desyncs the reader from
    /// the writer. The field must always be present in the binary form.
    #[serde(default)]
    pub apply_op_id: Option<String>,
    /// ENGINE-MINTED revision identity (RFC-0004 §H7). Unique within a
    /// `Document` instance and STABLE across projections within that instance's
    /// lineage: a still-pending revision keeps this value through partial
    /// resolution and re-projection, because it rides forward structurally on
    /// the cloned `RevisionInfo` rather than being re-derived. Imported
    /// identities are deterministically derived from the canonical revision
    /// record, so an unchanged revision also keeps this value across save and
    /// reopen even when serialization replaces its raw `w:id`. This — NOT
    /// `revision_id` — is the address `Resolution::Selective`, `enumerate_
    /// revisions`, `resolvable_revision_ids`, and the cascade set use. All the
    /// carriers of one user intention share ONE identity: a MOVE's source
    /// content + source pilcrow + destination clone(s) carry the move group's
    /// identity, so selecting it resolves the whole move atomically and the
    /// group enumerates as one record.
    ///
    /// `0` is the pre-identity sentinel: a snapshot serialized before H7, or a
    /// carrier not yet passed through the import mint walk, decodes as `0` and
    /// is not individually addressable until (re-)minted. Appended LAST for the
    /// bincode-positional reason above.
    #[serde(default)]
    pub identity: u32,
}

/// Tracked change for section properties (w:sectPrChange).
/// Stores the revision metadata and the previous section properties as raw XML.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SectionPropertyChange {
    pub revision: RevisionInfo,
    /// Raw XML bytes of the previous w:sectPr element inside sectPrChange.
    pub previous_properties_raw: Vec<u8>,
}

// =============================================================================
// Table Diff Types
// =============================================================================

/// Result of diffing two tables with detailed row/cell alignment.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TableDiffResult {
    /// Canonicalized old table.
    pub old_table: CanonicalTable,
    /// Canonicalized new table.
    pub new_table: CanonicalTable,
    /// Row-level alignment.
    pub row_alignment: Vec<TableRowAlignment>,
    /// Cell-level diffs.
    pub cell_diffs: Vec<TableCellDiff>,
}

/// Row alignment in table diff.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum TableRowAlignment {
    /// Row exists in both tables.
    Matched { old_row: usize, new_row: usize },
    /// Row was deleted from old table.
    Deleted { old_row: usize },
    /// Row was inserted in new table.
    Inserted { new_row: usize },
}

/// Cell-level diff in table diff.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TableCellDiff {
    /// Index in old_table.cells (None if inserted).
    pub old_cell_idx: Option<usize>,
    /// Index in new_table.cells (None if deleted).
    pub new_cell_idx: Option<usize>,
    /// Type of change.
    pub diff_type: TableCellDiffType,
    /// Word-level text diff (for Modified cells with paragraph content).
    pub text_diff: Option<Vec<InlineChange>>,
    /// Diffs for nested tables within this cell (for Modified cells).
    pub nested_table_diffs: Vec<NestedTableDiff>,
}

/// Type of cell change in table diff.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum TableCellDiffType {
    /// Cell content unchanged.
    Unchanged,
    /// Cell text modified.
    Modified,
    /// Cell inserted in new table.
    Inserted,
    /// Cell deleted from old table.
    Deleted,
    /// Cell merge (rowspan/colspan) changed.
    MergeChanged,
}

/// Source-document provenance for a merged block.
///
/// Each merged block may originate from the base document, the target document,
/// or both (modified). This provenance is emitted by `merge_diff` alongside the
/// merged CanonDoc, enabling downstream consumers (atom extraction, UI anchoring)
/// to use the correct source-document identity without reverse-engineering
/// merge-internal renaming.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct BlockProvenance {
    /// Block ID in the base (original) document. Present for deleted and modified blocks.
    pub base_block_id: Option<NodeId>,
    /// Block ID in the target (modified) document. Present for inserted and modified blocks.
    pub target_block_id: Option<NodeId>,
}

/// Result of diffing two documents.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DocumentDiff {
    pub base_fingerprint: DocFingerprint,
    pub target_fingerprint: DocFingerprint,
    pub changes: Vec<DiffChange>,
}

/// A single change in the diff.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
pub enum DiffChange {
    /// Block was deleted from base.
    BlockDeleted {
        block_id: NodeId,
        old_text: String,
        /// The deleted block (for metadata extraction).
        old_block: BlockNode,
        /// When set, this deletion is the source of a move operation.
        move_id: Option<String>,
    },
    /// Block was inserted (not in base).
    BlockInserted {
        after_block_id: Option<NodeId>,
        block: BlockNode,
        /// When set, this insertion is the destination of a move operation.
        move_id: Option<String>,
    },
    /// Block text was modified (inline changes).
    BlockModified {
        block_id: NodeId,
        old_text: String,
        new_text: String,
        inline_changes: Vec<InlineChange>,
        /// The old block (for metadata extraction).
        old_block: BlockNode,
        /// The new block (for metadata extraction).
        new_block: BlockNode,
        /// True when the diff detected that this modification is the first
        /// half of a paragraph split (one base paragraph → two target paragraphs).
        /// The following `BlockInserted` carries the second half.
        para_split: bool,
    },
    /// Table structure changed (different row/column layout).
    /// Includes canonicalized tables and detailed diff for rendering.
    TableStructureChanged {
        table_id: NodeId,
        target_table_id: NodeId,
        old_hash: String,
        new_hash: String,
        /// Coarse old table text (for backwards compatibility / fallback).
        old_text: String,
        /// Coarse new table text (for backwards compatibility / fallback).
        new_text: String,
        /// Detailed table diff with row alignment and cell changes.
        /// Some when detailed diffing succeeds, None for fallback.
        table_diff: Option<Box<TableDiffResult>>,
    },
    /// Table cell content changed but table structure (rows, columns, merges) is identical.
    /// Carries per-cell inline changes so the merge can apply them within the existing table
    /// instead of replacing the entire table with a deleted + inserted copy.
    TableCellsModified {
        table_id: NodeId,
        target_table_id: NodeId,
        cell_changes: Vec<TableCellChange>,
        /// Coarse old table text (for the comparison pipeline / atom assignment).
        old_text: String,
        /// Coarse new table text (for the comparison pipeline / atom assignment).
        new_text: String,
    },

    // Story-level changes
    /// Header was modified (content changed but kind matches).
    HeaderModified {
        kind: HeaderFooterKind,
        /// Base document part name carrying the pre-change content.
        base_part_name: String,
        /// Target document part name for the same logical story slot.
        target_part_name: String,
        old_hash: String,
        new_hash: String,
        block_changes: Vec<DiffChange>,
    },
    /// Header was deleted (exists in base but not target).
    HeaderDeleted {
        kind: HeaderFooterKind,
        /// Resolved part name (e.g., "header2.xml") for targeting the base part.
        part_name: String,
        content_hash: String,
        blocks: Vec<BlockNode>,
    },
    /// Header was inserted (exists in target but not base).
    HeaderInserted {
        kind: HeaderFooterKind,
        /// Resolved part name (e.g., "header3.xml") in target.
        part_name: String,
        content_hash: String,
        blocks: Vec<BlockNode>,
    },

    /// Footer was modified (content changed but kind matches).
    FooterModified {
        kind: HeaderFooterKind,
        /// Base document part name carrying the pre-change content.
        base_part_name: String,
        /// Target document part name for the same logical story slot.
        target_part_name: String,
        old_hash: String,
        new_hash: String,
        block_changes: Vec<DiffChange>,
    },
    /// Footer was deleted (exists in base but not target).
    FooterDeleted {
        kind: HeaderFooterKind,
        /// Resolved part name (e.g., "footer2.xml") for targeting the base part.
        part_name: String,
        content_hash: String,
        blocks: Vec<BlockNode>,
    },
    /// Footer was inserted (exists in target but not base).
    FooterInserted {
        kind: HeaderFooterKind,
        /// Resolved part name (e.g., "footer3.xml") in target.
        part_name: String,
        content_hash: String,
        blocks: Vec<BlockNode>,
    },

    /// Footnote was modified (content changed but id matches by content alignment).
    FootnoteModified {
        id: String,
        old_hash: String,
        new_hash: String,
        block_changes: Vec<DiffChange>,
    },
    /// Footnote was deleted.
    FootnoteDeleted {
        id: String,
        content_hash: String,
        blocks: Vec<BlockNode>,
    },
    /// Footnote was inserted.
    FootnoteInserted {
        id: String,
        content_hash: String,
        blocks: Vec<BlockNode>,
    },

    /// Endnote was modified.
    EndnoteModified {
        id: String,
        old_hash: String,
        new_hash: String,
        block_changes: Vec<DiffChange>,
    },
    /// Endnote was deleted.
    EndnoteDeleted {
        id: String,
        content_hash: String,
        blocks: Vec<BlockNode>,
    },
    /// Endnote was inserted.
    EndnoteInserted {
        id: String,
        content_hash: String,
        blocks: Vec<BlockNode>,
    },

    /// Comment was modified.
    CommentModified {
        id: String,
        old_hash: String,
        new_hash: String,
        block_changes: Vec<DiffChange>,
    },
    /// Comment was deleted.
    CommentDeleted {
        id: String,
        content_hash: String,
        blocks: Vec<BlockNode>,
    },
    /// Comment was inserted.
    CommentInserted {
        id: String,
        content_hash: String,
        blocks: Vec<BlockNode>,
    },
}

/// A single cell's content change within a same-structure table.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TableCellChange {
    /// Row index in the table.
    pub row_index: usize,
    /// Cell index within the row.
    pub cell_index: usize,
    /// Per-paragraph changes within this cell.
    pub paragraph_changes: Vec<CellParagraphChange>,
    /// Per-nested-table changes within this cell.
    pub nested_table_diffs: Vec<NestedTableDiff>,
    /// Target cell formatting when it differs from the base cell (for tcPrChange generation).
    pub new_cell_formatting: Option<CellFormatting>,
}

/// A nested table diff within a cell.
///
/// When a cell contains `BlockNode::Table` elements and the inner table content
/// changed, this carries the diff result so the tracked model can apply
/// row/cell-level tracked changes recursively.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct NestedTableDiff {
    /// Index of the table block within the cell's blocks.
    pub block_index: usize,
    /// The diff result for the nested table (structure or cell-level changes).
    pub diff: NestedTableDiffKind,
}

/// Kind of diff for a nested table within a cell.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum NestedTableDiffKind {
    /// Row/column structure changed — carries the full table diff.
    StructureChanged {
        table_diff: Box<TableDiffResult>,
        new_table: Box<TableNode>,
    },
    /// Same structure, cell content changed — carries per-cell changes.
    CellsModified { cell_changes: Vec<TableCellChange> },
}

/// A single paragraph change within a table cell.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CellParagraphChange {
    /// Index of the paragraph within the cell's blocks.
    pub block_index: usize,
    /// Inline changes for this paragraph.
    pub inline_changes: Vec<InlineChange>,
    /// The new (target) block for metadata and opaque fallback.
    pub new_block: BlockNode,
}

/// Character-level change within a block.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum InlineChange {
    Unchanged {
        text: String,
        marks: Vec<Mark>,
        style_props: StyleProps,
        formatting_change: Option<FormattingChange>,
    },
    Inserted {
        text: String,
        marks: Vec<Mark>,
        style_props: StyleProps,
        formatting_change: Option<FormattingChange>,
        /// Revision id of the tracked change this span belongs to (0 when there is
        /// no selectable revision — a pairwise-diff projection or a legacy
        /// pre-identity change). Lets a review UI map a redline span to its
        /// `revisions` entry for selective accept/reject.
        #[serde(default)]
        rev_id: u32,
    },
    Deleted {
        text: String,
        marks: Vec<Mark>,
        style_props: StyleProps,
        formatting_change: Option<FormattingChange>,
        #[serde(default)]
        rev_id: u32,
    },
    Opaque {
        #[allow(clippy::struct_field_names)]
        segment_type: InlineChangeSegmentType,
        kind: OpaqueSegmentKind,
        opaque_id: String,
        inline_index: usize,
        text: Option<String>,
        /// Reference ID (w:id) for footnote/endnote/comment references.
        reference_id: Option<String>,
        /// Field kind for field elements.
        field_kind: Option<FieldKind>,
        /// Field instruction text.
        field_instruction: Option<String>,
        /// Asset data for media/math opaques: image data URI or equation XML.
        asset_ref: Option<String>,
        /// Drawing display width in EMUs (from wp:extent cx). None for non-drawing opaques.
        asset_width_emu: Option<i64>,
        /// Drawing display height in EMUs (from wp:extent cy). None for non-drawing opaques.
        asset_height_emu: Option<i64>,
        /// Alt text from wp:docPr descr. None when not present.
        alt_text: Option<String>,
        /// Hyperlink target: the external URL, or `#anchor` for an internal
        /// bookmark link. None for non-hyperlink opaques.
        url: Option<String>,
        /// The opaque inline node's stable content hash — the guard that
        /// `set_image_attrs` (and other drawing-targeted ops) validate against
        /// via `node.content_hash`. Surfaced so an editor can pin a resize to the
        /// exact drawing it read (rather than the containing block's guard, which
        /// is a different hash). None when the node carries no hash.
        content_hash: Option<String>,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum InlineChangeSegmentType {
    Equal,
    Insert,
    Delete,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum OpaqueSegmentKind {
    Drawing,
    Omml,
    Hyperlink,
    Field,
    Sdt,
    Ruby,
    SmartArt,
    CommentReference,
    FootnoteReference,
    EndnoteReference,
    SmartTag,
    /// Symbol character — displays a single character from a named font.
    Sym,
    /// Absolute position tab — renders as whitespace advancing to a calculated position.
    Ptab,
    /// Custom XML wrapper — transparent container, content shows through.
    CustomXml,
    Unknown(String),
}

/// A specific metadata property that changed on an image while the pixels stayed identical.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum ImageMetadataChange {
    /// Image dimensions (wp:extent cx/cy) changed.
    Size,
    /// Cropping rectangle (a:srcRect) changed.
    Cropping,
    /// Alt text (wp:docPr descr) changed.
    AltText,
}

/// The structural kind of a block in the full document view.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum BlockType {
    Paragraph,
    Heading,
    Table,
    Opaque,
}

impl BlockType {
    /// Serialize to the wire-format string ("paragraph", "heading", "table", "opaque").
    pub fn as_str(&self) -> &'static str {
        match self {
            BlockType::Paragraph => "paragraph",
            BlockType::Heading => "heading",
            BlockType::Table => "table",
            BlockType::Opaque => "opaque",
        }
    }
}

impl std::fmt::Display for BlockType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The change status of a block in the full document diff view.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum ChangeType {
    Unchanged,
    Modified,
    Inserted,
    Deleted,
}

impl ChangeType {
    /// Serialize to the wire-format string ("unchanged", "modified", "inserted", "deleted").
    pub fn as_str(&self) -> &'static str {
        match self {
            ChangeType::Unchanged => "unchanged",
            ChangeType::Modified => "modified",
            ChangeType::Inserted => "inserted",
            ChangeType::Deleted => "deleted",
        }
    }
}

impl std::fmt::Display for ChangeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A block in the full document view with inline diff segments.
///
/// Represents every block in document order, where each block carries
/// its inline diff segments (equal/insert/delete with marks).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct FullDocBlock {
    /// Stable projection block identity.
    ///
    /// For blocks present in the target document, this reuses the target-side
    /// canonical block ID. For deleted-only blocks, this is a stable tombstone
    /// ID derived from the base-side canonical block ID.
    pub block_id: NodeId,
    /// Original block ID from the base document. Set for unchanged/modified/deleted, None for inserted.
    pub doc1_block_id: Option<NodeId>,
    /// Original block ID from the target document. Set for unchanged/modified/inserted, None for deleted.
    pub doc2_block_id: Option<NodeId>,
    pub block_type: BlockType,
    pub heading_level: Option<u8>,
    pub style_id: Option<IStr>,
    pub change_type: ChangeType,
    pub align: Option<Alignment>,
    /// Render-resolved indentation: `effective_first_line_twips` already folds in
    /// a literal-prefix marker's leading-tab landing, so it is the single
    /// first-line origin to apply as `text-indent`. See [`Indentation`].
    pub indent: Option<Indentation>,
    pub spacing: Option<ParagraphSpacing>,
    pub borders: Option<ParagraphBorders>,
    /// Effective tab stops for this paragraph (empty = no custom stops).
    pub tab_stops: Vec<crate::word_ir::TabStopDef>,
    pub numbering_text: Option<String>,
    pub numbering_ilvl: Option<u32>,
    /// The paragraph's numbering `num_id` (Word auto-numbering only; None for a
    /// literal-prefix "list"). Lets a list-editing consumer target an existing
    /// list (e.g. join a paragraph to it via `set_numbering`).
    pub numbering_num_id: Option<u32>,
    pub segments: Vec<InlineChange>,
    /// Optional table diff for table blocks (only when structure changed).
    pub table_diff: Option<TableDiffResult>,
    /// Content types present in this block, e.g. ["text"], ["image"], ["text", "image"].
    pub content_types: Vec<String>,
    /// Raw OMML XML strings for equations in this block (for LLM context).
    pub equation_xmls: Vec<String>,
    /// Number of doc1 equations in equation_xmls (the rest are doc2).
    pub equation_doc1_count: usize,
    /// Base64 data URIs for images in this block (e.g. "data:image/png;base64,...").
    pub image_data_uris: Vec<String>,
    /// Number of doc1 images in image_data_uris (the rest are doc2).
    pub image_doc1_count: usize,
    /// Metadata properties that changed on an image while the pixels stayed identical.
    pub image_metadata_changes: Vec<ImageMetadataChange>,
    /// Shared move identifier linking a "moved from" block to its "moved to" counterpart.
    /// Set when the diff detects that a deleted block's content reappears as an insertion elsewhere.
    pub move_id: Option<String>,
    /// Direction of the move: "from" (content was here, moved away) or "to" (content arrived here).
    pub move_direction: Option<MoveDirection>,
    /// Structural change annotation: paragraph was joined into or split from an adjacent block.
    pub structural_change: Option<StructuralChange>,
    /// Border group identifier for consecutive paragraphs with identical border settings.
    /// Paragraphs in the same group share a visual border box per OOXML §17.3.1.24.
    pub border_group_id: Option<String>,
    /// Tracked-change status of this paragraph's paragraph mark, when the
    /// paragraph mark itself is tracked-inserted or tracked-deleted (ParagraphNode::para_mark_status).
    /// When set, the last entry in `segments` is the synthesized `\n` segment
    /// representing that paragraph-mark change, and projection-side ID computation
    /// must tag it with `{block_id}_para_mark` to match the atom side.
    pub paragraph_mark_status: Option<TrackingStatus>,
}

/// Direction of a move operation.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum MoveDirection {
    /// Content was moved away from this location (appears as deleted).
    From,
    /// Content was moved to this location (appears as inserted).
    To,
}

/// Structural change annotation for paragraph join/split detection.
///
/// When paragraphs are joined (two become one) or split (one becomes two),
/// the diff shows modified + deleted/inserted blocks. This annotation
/// makes the structural change explicit.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum StructuralChange {
    /// This block was joined into an adjacent modified block.
    /// `into_block_id` is the block_id of the modified block that absorbed this content.
    Join { into_block_id: NodeId },
    /// This block was split from an adjacent modified block.
    /// `from_block_id` is the block_id of the modified block that lost this content.
    Split { from_block_id: NodeId },
}

/// Full document view result including body blocks and referenced story content.
pub struct FullDocViewResult {
    pub blocks: Vec<FullDocBlock>,
    /// Footnote stories from both documents (target takes precedence).
    pub footnotes: Vec<StoryPayload>,
    /// Endnote stories from both documents (target takes precedence).
    pub endnotes: Vec<StoryPayload>,
    /// Comment stories from both documents (target takes precedence).
    pub comments: Vec<CommentPayload>,
    /// Header stories referenced by the body section (§17.10.2), projected to
    /// inline segments. One entry per `w:headerReference` the body section binds,
    /// carrying its `kind` (default/first/even) so the frontend can pick the
    /// applicable band. Empty when the section declares no headers.
    pub headers: Vec<HeaderFooterPayload>,
    /// Footer stories referenced by the body section (§17.10.5), same shape and
    /// semantics as `headers`.
    pub footers: Vec<HeaderFooterPayload>,
    /// Body-level section properties from the target document.
    pub body_section_properties: Option<SectionProperties>,
}

/// Rendered note story content for the frontend.
pub struct StoryPayload {
    pub id: String,
    pub segments: Vec<InlineChange>,
}

/// One paragraph of a header/footer story, carrying the paragraph-level
/// properties a faithful band render needs alongside its inline content. Word
/// centers/right-aligns footer paragraphs (`w:jc`) and positions tabbed content
/// at real stops (`w:tabs`); a flat inline stream cannot express either, so the
/// projection keeps the story's paragraph structure here instead of flattening
/// it (the discarded `pPr` is exactly why a centered footer rendered left).
pub struct HeaderFooterParagraph {
    /// Paragraph alignment (`w:jc`, §17.3.1.13). `None` = inherited (left).
    pub align: Option<Alignment>,
    /// Explicit tab stops (`w:tabs`, §17.3.1.38) — the classic left/center/right
    /// footer. Empty = Word's default 0.5in grid (synthesized by the renderer).
    pub tab_stops: Vec<crate::word_ir::TabStopDef>,
    /// The paragraph's inline content (the same segment shape body blocks use:
    /// marks, tabs, fields).
    pub segments: Vec<InlineChange>,
}

/// Rendered header/footer story content for the frontend (read-only band).
///
/// `kind` is the `w:type` of the section's reference to this story
/// (`"default"`, `"first"`, or `"even"`), so the frontend renders the band that
/// applies to the page it is showing. `paragraphs` keeps the story's paragraph
/// structure (one per `w:p`) so each line's alignment and tab stops survive —
/// faithful to how Word lays out headers/footers.
pub struct HeaderFooterPayload {
    pub kind: String,
    pub paragraphs: Vec<HeaderFooterParagraph>,
}

/// Rendered comment story content for the frontend.
pub struct CommentPayload {
    pub id: String,
    pub author: Option<String>,
    pub date: Option<String>,
    pub segments: Vec<InlineChange>,
    /// Resolved state from the matching `w15:commentEx` record (MS-DOCX §2.5.1),
    /// linked by the comment's first body paragraph `w14:paraId`. `false` when
    /// there is no commentsExtended record for this comment.
    pub resolved: bool,
    /// The `w14:paraId` of the parent comment when this is a reply thread child
    /// (`w15:paraIdParent`); `None` for a top-level comment or when there is no
    /// commentsExtended record.
    pub parent_para_id: Option<String>,
}
