//! The designed public read projection — `DocumentView` and friends.
//!
//! This is the engine's query language (`docs/domain-model.md` §4, §9): a
//! *single-document*, stable surface for **targeting** and **inspecting** a
//! document — block ids to aim an [`EditTransaction`] at, role labels, the
//! visible text a step's `expect` matches against, the tracked status of each
//! block / paragraph-mark / inline segment, and opaque anchors.
//!
//! It is designed **independently of the IR** so the IR (`CanonDoc` and all of
//! `domain`) can keep moving underneath it. None of the following leak through
//! this surface: `CanonDoc`/`domain` IR types, the internal change vocabulary
//! (`InlineChange`/`DiffChange`), the pairwise-diff projection (`FullDocBlock`),
//! or any diff-only field (`doc1_block_id`, `change_type`, `move_id`, …).
//!
//! The one IR type intentionally re-used is [`NodeId`]: it is already public and
//! is exactly the handle an [`EditTransaction`] targets, so exposing it here
//! keeps targeting friction-free rather than forcing a translation step.
//!
//! The role / heading / paragraph-mark decisions mirror the established
//! single-document projection (`diff::project_tracked_document`,
//! `diff::block_metadata`) so this clean view agrees with the engine's rules.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::domain::{
    BlockNode, CanonDoc, HeadingLevel, InlineNode, NodeId, OpaqueInlineNode, OpaqueKind,
    TrackedBlock, TrackedSegment, TrackingStatus,
};
use crate::runtime::EditSnapshot;

/// The designed read projection of a single document.
///
/// A flat list of blocks in document order. Build it with [`build_document_view`]
/// (or [`crate::api::Document::read`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DocumentView {
    pub blocks: Vec<BlockView>,
}

/// One block in the read projection: enough to target it and inspect its
/// tracked structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct BlockView {
    /// Stable block id — the handle an [`crate::edit::EditTransaction`] targets.
    pub id: NodeId,
    /// What kind of block this is.
    pub role: BlockRole,
    /// Paragraph/table style id, when the IR carries one.
    pub style_id: Option<String>,
    /// The role token an `insert`/`replace` op accepts to author a NEW paragraph
    /// formatted like this block. This is the document's *private* role
    /// vocabulary (`vocabulary::paragraph_role_ids`) — the snake_case of the
    /// style id for styled paragraphs, a descriptive name (`body_text`,
    /// `heading_1`, `numbered_item`, …) otherwise — and is the SAME token the
    /// insert op validates against. `None` only for non-paragraph blocks (tables,
    /// opaque) which have no paragraph role. A cold agent reads this to obtain a
    /// role that `insert` accepts even for a `Normal`-styled doc, where
    /// `style_id` is `null`.
    pub role_token: Option<String>,
    /// Concatenated visible text (accept-all reading: what the block *says*).
    /// This is what a plain-paragraph [`crate::edit::EditStep`]'s `expect`
    /// matches against.
    pub text: String,
    /// Block-level tracked status (whole-block insert/delete), from
    /// `TrackedBlock.status`.
    pub block_status: TrackStatus,
    /// Tracked status of the trailing paragraph mark (`None` in the IR → Normal).
    pub paragraph_mark_status: TrackStatus,
    /// The block's **staleness guard**: its semantic hash
    /// (`semantic_hash::block_semantic_hash_for_block`) at read time. A write op
    /// that targets this block carries this value as its `guard`; if the block
    /// changed since the read, the op fails loud (`StaleEdit`). This is the
    /// single staleness mechanism: the precondition and the staleness check are
    /// the same object. Span handles are only safe *because* of this guard.
    pub guard: String,
    /// List/numbering membership, present only when this paragraph participates
    /// in Word auto-numbering (`w:numPr`). Carries the `numId` + `ilvl` + ordered
    /// /bullet kind the granular list ops (`SetType`/`Indent`/`Outdent`/`Restart`)
    /// target. `None` for non-list paragraphs and for literal-prefix "lists"
    /// (which are plain text with a typed-in prefix, not Word numbering).
    pub list: Option<ListMembership>,
    /// Per-cell addressing for a `Table` block: each cell's `{row, col}` grid
    /// position and its visible text, in row-major order. Empty for non-table
    /// blocks. A cold agent locates "the cell containing X" here and targets
    /// `table_op.set_cell_text` with the cell's `row`/`col`.
    pub cells: Vec<TableCellView>,
    /// Table-level render metadata (column widths, alignment, indent). `Some`
    /// only for a `Table` block.
    pub table: Option<TableMetaView>,
    /// The typed-in enumeration label (`"1."`, `"(a)"`) this paragraph carries
    /// in [`ParagraphNode::literal_prefix`], when it has one and no structural
    /// auto-numbering supersedes it (see [`literal_prefix_label`]). This is the
    /// same label already PREPENDED to `text` (as `"{label}\t{body}"`); the
    /// field is surfaced separately so a detail reader (`read_block`) can tell
    /// that the leading label is a structural enumeration marker, NOT a
    /// `segments` span it can target — body edits address the `segments`, the
    /// label is not span-addressable. `None` for paragraphs without a literal
    /// prefix and for auto-numbered paragraphs (whose marker is in `list`).
    pub literal_prefix: Option<String>,
    /// Inline structure for fine-grained targeting.
    pub segments: Vec<SegmentView>,
    /// For an `Opaque` block: what KIND of placeholder this is (e.g. `"sdt"`,
    /// `"quarantined_nested_tracked_changes"`). An opaque block has no
    /// readable text, so the label is the honest description the reader gets
    /// instead — load-bearing for quarantined items, whose contested tracked
    /// content must never be presented as ordinary text. `None` for
    /// non-opaque blocks.
    pub opaque_label: Option<String>,
}

/// A paragraph's Word-auto-numbering membership, surfaced so the granular list
/// verbs become targetable from a read. Mirrors the IR's `NumberingInfo`
/// (`num_id` + `ilvl`), plus the ordered-vs-bullet discriminant the agent needs
/// to decide whether a list is a numbered list or a bullet list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct ListMembership {
    /// The numbering instance id (`w:numId`) — the list this paragraph belongs
    /// to. Two paragraphs sharing a `num_id` are in the same list.
    pub num_id: u32,
    /// The indent level within the list (`w:ilvl`, 0-based). Outdent decreases
    /// it, Indent increases it.
    pub ilvl: u32,
    /// `true` for an ordered (counter) list, `false` for a bullet list. From the
    /// IR's `NumberingInfo.is_bullet`.
    pub ordered: bool,
    /// The synthesized marker the IR computed for this item (`"1."`, `"(a)"`,
    /// `"•"`). Empty when the IR has not materialized one.
    pub marker_text: String,
}

/// One cell of a `Table` block in the read view: its grid position and visible
/// text. The address a cold agent passes to `table_op.set_cell_text`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct TableCellView {
    /// 0-based row index in the table's grid.
    pub row: usize,
    /// 0-based column index in the table's grid (the logical grid column, after
    /// `gridBefore`; a horizontally-merged cell occupies its first column).
    pub col: usize,
    /// Concatenated visible text of every paragraph in the cell, blocks joined
    /// by a single space (the same accept-all reading `BlockView::text` uses).
    pub text: String,
    /// Horizontal span (gridSpan); 1 = no merge.
    pub col_span: usize,
    /// Vertical span (resolved vMerge); 1 = no merge. Continuation cells are
    /// folded into the anchor and not emitted.
    pub row_span: usize,
    /// The cell's four EFFECTIVE borders (cell override else table outer/inside).
    pub borders: CellBordersView,
    /// Background fill from cell shading (`w:shd`), as a hex color. None = none.
    pub shading: Option<String>,
    /// Vertical alignment ("top"/"center"/"bottom"); None = default (top).
    pub v_align: Option<String>,
    /// The cell's content as structured inline paragraphs, projected with the
    /// SAME [`InlineChange`](crate::domain::InlineChange) segment shape the body's
    /// rich view uses — so a render frontend reuses one segment→DOM path for both
    /// body and cell text and reaches the same fidelity (bold/italic/underline,
    /// colors, fonts, run shading/highlight, and hyperlinks). One entry per
    /// paragraph block in the cell, in document order; a multi-paragraph cell
    /// yields multiple entries, each rendered as its own line/`<p>`.
    ///
    /// These are single-document `Unchanged` segments (no tracked-change diff):
    /// the table block is read-only in the render surface, so no per-cell redline
    /// is projected. `text` stays the flat accept-all reading for cell ADDRESSING
    /// (`table_op.set_cell_text`); `paragraphs` is the render projection.
    pub paragraphs: Vec<CellParagraphView>,
}

/// One paragraph inside a table cell, as render-ready inline segments. The
/// segment shape is exactly the body's
/// [`InlineChange`](crate::domain::InlineChange) so a frontend reuses its
/// existing segment→DOM mapping (marks + `style_props` + hyperlinks) for cells.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct CellParagraphView {
    /// The paragraph's inline runs, each carrying its visible `text`, `marks`
    /// (bold/italic/…), and `style_props` (font/size/color/highlight/…); opaque
    /// runs (hyperlinks, fields, footnote markers, drawings) arrive as
    /// `InlineChange::Opaque`. Segments carry per-run tracking status — a tracked
    /// change inside the cell projects as `Inserted`/`Deleted` (a redline), the
    /// same vocabulary the body uses. Images inside cells surface as `Opaque`
    /// WITHOUT an
    /// `asset_ref` — the lean view carries no image-data lookup — so a frontend
    /// renders them as a labeled placeholder, not pixels (documented gap).
    pub segments: Vec<crate::domain::InlineChange>,
    /// The cell paragraph's own NodeId — the handle a `replace`/`set_format` op
    /// targets to edit this paragraph IN PLACE (the engine resolves it via
    /// `find_paragraph_path`, which already recurses into cells). This is what
    /// lets a frontend edit a cell like a body paragraph (with redline + marks)
    /// instead of whole-cell `set_cell_text`.
    pub block_id: String,
    /// This cell paragraph's staleness guard (its own semantic hash), pinned by a
    /// `replace` targeting it — the same guard mechanism body paragraphs use.
    pub guard: String,
}

/// A table cell's four resolved border edges for the render projection. Each is
/// the effective border after cell-override-else-table-outer/inside resolution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct CellBordersView {
    pub top: Option<crate::domain::Border>,
    pub bottom: Option<crate::domain::Border>,
    pub left: Option<crate::domain::Border>,
    pub right: Option<crate::domain::Border>,
}

/// Table-level render metadata (not per-cell): the grid column widths (twips),
/// table alignment, and table indent (twips).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TableMetaView {
    /// Column widths in twips, from `w:tblGrid` (`w:gridCol`).
    pub cols: Vec<u32>,
    /// Table alignment ("left"/"center"/"right"); None = left.
    pub align: Option<String>,
    /// Table indent from the leading margin, in twips.
    pub indent: Option<i32>,
}

/// The role of a block. Mirrors `diff::block_metadata`'s decision: a paragraph
/// with a heading level is a `Heading`, otherwise `Paragraph`; tables are
/// `Table`; opaque blocks are `Opaque`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum BlockRole {
    Paragraph,
    Heading { level: u8 },
    Table,
    Opaque,
}

/// An **ephemeral, block-local** handle for a text span in the detail view.
///
/// The detail read surface exposes one handle per emitted
/// [`SegmentView::Text`], named `s_<ordinal>` in document
/// order within its block. A handle is *not* a durable id (block ids are): it is
/// valid only against the exact block the detail read was taken from, and a write
/// that uses it is made safe by the block `guard` (see [`BlockView::guard`]).
///
/// The same enumeration that assigns these handles in the read view is reused by
/// the write path (`edit::resolve_span`) so a handle from a fresh read resolves
/// to the same inline range — see [`enumerate_text_spans`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SpanHandle(pub String);

/// One inline span in a block. Either visible text (carrying its segment's
/// tracked status) or an opaque anchor (image, equation, field, …).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SegmentView {
    /// A contiguous run of visible text sharing one tracked status **and** one
    /// set of meaningful inline marks. A run breaks when either changes, so the
    /// comprehension view can render the marks that carry legal meaning.
    Text {
        text: String,
        status: TrackStatus,
        marks: Vec<TextMark>,
        /// Ephemeral block-local span handle (`s_<n>`), assigned in document
        /// order by the shared span enumeration. The write path resolves it
        /// back to this exact inline range while the block `guard` is unchanged.
        /// `Option` because constructors outside the IR walk (tests, slices)
        /// may not carry one; the IR walk always assigns it.
        #[serde(default)]
        handle: Option<SpanHandle>,
    },
    /// An opaque anchor occupying a position in the text. `id` is the anchor's
    /// stable id; `text` is its visible label when one is known (field result,
    /// hyperlink display text), else `None`.
    Opaque {
        id: NodeId,
        kind: OpaqueAnchorKind,
        status: TrackStatus,
        text: Option<String>,
        /// Ephemeral block-local span handle (`s_<n>`), assigned in document
        /// order alongside text spans. Anchors are *also* addressable by their
        /// durable `id` (and that is the preferred selector for anchor-relative
        /// ops), but they occupy a handle ordinal so the sequence is dense.
        #[serde(default)]
        handle: Option<SpanHandle>,
        /// Structured, kind-specific metadata projected from the anchor (its
        /// raw_xml or typed IR data). `None` when the kind carries nothing
        /// discoverable (documented bareness). `text` stays the cheap one-line
        /// label; `metadata` is the opt-in structure.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<OpaqueMetadata>,
    },
}

/// Tracked status of a span, carrying the revision metadata of a tracked change.
///
/// Mirrors the IR's own `TrackingStatus`: a `Normal` span has no revision; an
/// `Inserted` / `Deleted` span always carries the [`RevisionView`] that produced
/// it. Encoding it this way makes the invalid states unrepresentable (no
/// `Normal`-with-revision, no tracked-span-without-revision) rather than pairing a
/// bare discriminant with an `Option<RevisionView>` the caller must keep in sync.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TrackStatus {
    Normal,
    Inserted(RevisionView),
    Deleted(RevisionView),
    /// The stacked state: text inserted by one pending revision
    /// and deleted by another. ONE span with a compound status — two virtual
    /// spans would fork the read enumeration from the write resolution. The
    /// span is non-targetable for further text edits in v1 (the state is
    /// terminal in the inline grammar; you resolve it, you don't edit it).
    InsertedThenDeleted {
        inserted: RevisionView,
        deleted: RevisionView,
    },
}

/// The revision metadata of a tracked change: who, when, and which apply produced
/// it. This is the public projection of the IR's `RevisionInfo` — the
/// engine-minted revision IDENTITY is surfaced (it is the stable id a caller
/// groups adjacent spans by AND the handle accept/reject selectors address),
/// but no other IR internals — notably not the non-unique wire `w:id` — leak.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct RevisionView {
    /// Stable engine-minted identity of this revision within the document
    /// (RFC-0004 §H7). Adjacent spans sharing this id belong to the same tracked
    /// change, and it is the value `Resolution::Selective` resolves — NOT the
    /// wire `w:id`, which Word does not keep unique.
    pub revision_id: u32,
    /// Author of the change, when the source carried one. `None` when the DOCX
    /// had a `<w:ins>` / `<w:del>` with no `w:author` (Word anonymization,
    /// third-party tools). Callers that need a non-null author decide their own
    /// fallback — the view does not invent one.
    pub author: Option<String>,
    /// ISO-8601 timestamp of the change, when the source carried one.
    pub date: Option<String>,
    /// Group id of the `apply` call that created this revision. Every tracked
    /// change from a single authored apply shares one id; `None` for changes
    /// loaded from an imported DOCX.
    pub apply_op_id: Option<String>,
}

/// The meaningful inline marks the comprehension view surfaces. These are the
/// formatting marks that carry legal/semantic weight (bold a defined term,
/// strike a deleted figure). Low-signal run properties (kerning, exact font
/// metrics, East Asian layout) are deliberately not projected here; they live in
/// the detail view, reachable when the agent is actually editing formatting.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum TextMark {
    Bold,
    Italic,
    Underline,
    Strike,
    Subscript,
    Superscript,
}

/// The kind of an opaque inline anchor, mapped from the IR's `OpaqueKind` to a
/// small public vocabulary. Anything not in the explicit set maps to `Other`
/// (never dropped — `CLAUDE.md`: no silent fallbacks).
///
/// `#[non_exhaustive]`: future kinds (a SmartArt detail surface, etc.) can be
/// added without breaking external exhaustive matches — they degrade to a
/// fallback arm rather than failing to compile.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub enum OpaqueAnchorKind {
    Drawing,
    Equation,
    Hyperlink,
    Field,
    FootnoteRef,
    EndnoteRef,
    Comment,
    /// A content control (`w:sdt`) — surfaced as its own kind so an agent can
    /// filter anchors to content controls before reading metadata.
    ContentControl,
    /// An intra-paragraph hard line/page/column break (`InlineNode::HardBreak`
    /// — Word's `<w:br/>`). Occupies a stream position like any other opaque
    /// anchor so a text run never straddles it; `id` is the break's own
    /// stable id (already write-addressable via `AnchorAfter`/`AnchorBefore`).
    HardBreak,
    /// The start marker of an engine-native comment range
    /// (`InlineNode::CommentRangeStart`). See [`OpaqueAnchorKind::Comment`]
    /// for the visible reference marker.
    CommentRangeStart,
    /// The end marker of an engine-native comment range
    /// (`InlineNode::CommentRangeEnd`).
    CommentRangeEnd,
    Other,
}

/// Structured, kind-specific metadata projected from an opaque anchor's raw_xml
/// (or typed IR data) for the read surface. `None` on a `SegmentView::Opaque`
/// means the kind carries no discoverable metadata (Ruby, SmartTag, Ptab,
/// CustomXml, SmartArt, Unknown, Quarantined, OMML) — an explicit, documented
/// bareness, not a silent drop. See `crate::opaque_meta::project`.
///
/// `#[non_exhaustive]`: future metadata-bearing kinds (e.g. a MathML linearizer
/// adding a `Math` variant) can be added without breaking external matches.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "meta_kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum OpaqueMetadata {
    /// A structured document tag (content control). `tag` is the programmatic
    /// id ("TenantName"), `alias` the human title ("Tenant Name"),
    /// `display_text` the current value, `control` the kind.
    ContentControl {
        tag: Option<String>,
        alias: Option<String>,
        control: SdtControlKind,
        /// Current `sdtContent` text, `None` when the control is empty.
        display_text: Option<String>,
        /// Populated for Dropdown / ComboBox only.
        list_items: Vec<SdtListItemView>,
        /// Populated for Checkbox only.
        checked: Option<bool>,
    },
    /// An inline drawing. `extent_*` are in EMUs (914400 per inch); `alt_text`
    /// from `wp:docPr` @descr; `embed_rid` is the embed relationship id.
    Drawing {
        extent_cx_emu: Option<i64>,
        extent_cy_emu: Option<i64>,
        alt_text: Option<String>,
        embed_rid: Option<String>,
        /// The interior text of a textbox carried by this drawing (a
        /// `w:txbxContent` inside a DrawingML `wps:txbx` or a VML `v:textbox`),
        /// paragraphs joined by `\n`. `None` when the drawing carries no
        /// textbox. This is the read-side half of `set_textbox_text` (M3): the
        /// agent reads what a textbox currently says before replacing it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        textbox_text: Option<String>,
    },
    /// A field. Typed IR data plus — only on a Begin anchor carrying a
    /// `w:ffData` child — the legacy form-field identity.
    Field {
        /// Which part of a field this anchor is (begin / instruction /
        /// separate / end / simple / unknown).
        field_char: FieldCharRole,
        instruction: Option<String>,
        result: Option<String>,
        semantic: Option<String>,
        /// Present ONLY on a Begin anchor that carries a `w:ffData` child (a
        /// legacy form field). `None` for ordinary fields (TOC, REF, PAGE, …)
        /// and for every non-begin anchor.
        form: Option<FormFieldIdentity>,
    },
    /// A hyperlink. Adds the resolved target to the display text already in
    /// `SegmentView::Opaque.text`.
    Hyperlink {
        url: Option<String>,
        anchor: Option<String>,
    },
    /// A note/comment reference: the story id it points at.
    NoteReference { reference_id: String },
    /// A symbol glyph (`w:sym`): the decoded character + its font.
    Symbol { display_char: String, font: String },
    /// raw_xml was expected for this kind but was missing or failed to parse.
    /// NEVER a silent empty — the agent sees the failure and the document still
    /// renders (the anchor keeps its id + kind).
    Unparsed { reason: String },
}

/// Read mirror of the IR's `SdtControl` (a `w:sdt`'s control type). The view
/// keeps its own small read vocabulary rather than re-exporting the write type,
/// matching how `OpaqueAnchorKind` mirrors `OpaqueKind`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SdtControlKind {
    PlainText,
    RichText,
    Dropdown,
    ComboBox,
    Checkbox,
    Date,
    RepeatingSection,
}

/// One list item of a dropdown / combo content control.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SdtListItemView {
    pub display: String,
    pub value: String,
}

/// Read mirror of the IR's `FieldKind`: which part of a field an anchor is.
/// `FieldKind::Unknown(String)` collapses to `Unknown` here (the raw type string
/// is not action-relevant for read-surfacing and is preserved in raw_xml).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FieldCharRole {
    Begin,
    Instruction,
    Separate,
    End,
    Simple,
    Unknown,
}

/// The legacy form-field identity parsed from a Begin anchor's `w:ffData`. The
/// identity `SetFormFieldValue` targets. One variant per legacy form-field kind
/// so invalid combinations (a textInput with list entries) are unrepresentable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "form_kind", rename_all = "snake_case")]
pub enum FormFieldIdentity {
    /// FORMTEXT — `<w:ffData><w:textInput>`. (§17.16.23)
    TextInput {
        name: Option<String>,
        /// `textInput/default` @w:val — the stored default, NOT the live value
        /// (the live FORMTEXT value is the result run, surfaced as block text).
        default: Option<String>,
    },
    /// FORMCHECKBOX — `<w:checkBox>`. (§17.16.8) Legacy `w:checked`.
    Checkbox { name: Option<String>, checked: bool },
    /// FORMDROPDOWN — `<w:ddList>`. (§17.16.28)
    DropDown {
        name: Option<String>,
        /// `listEntry` @w:val, in order (display == value for legacy fields).
        entries: Vec<String>,
        /// `ddList/result` @w:val (zero-based selected index).
        selected_index: Option<usize>,
    },
}

impl From<&TrackingStatus> for TrackStatus {
    fn from(status: &TrackingStatus) -> Self {
        match status {
            TrackingStatus::Normal => TrackStatus::Normal,
            TrackingStatus::Inserted(rev) => TrackStatus::Inserted(RevisionView::from(rev)),
            TrackingStatus::Deleted(rev) => TrackStatus::Deleted(RevisionView::from(rev)),
            TrackingStatus::InsertedThenDeleted(sr) => TrackStatus::InsertedThenDeleted {
                inserted: RevisionView::from(&sr.inserted),
                deleted: RevisionView::from(&sr.deleted),
            },
        }
    }
}

impl From<&crate::domain::RevisionInfo> for RevisionView {
    fn from(rev: &crate::domain::RevisionInfo) -> Self {
        RevisionView {
            // Surface the engine-minted identity as the caller-facing revision
            // id (the resolve handle), NOT the non-unique wire `w:id`.
            revision_id: rev.identity,
            author: rev.author.clone(),
            date: rev.date.clone(),
            apply_op_id: rev.apply_op_id.clone(),
        }
    }
}

impl From<&OpaqueKind> for OpaqueAnchorKind {
    fn from(kind: &OpaqueKind) -> Self {
        match kind {
            OpaqueKind::Drawing => OpaqueAnchorKind::Drawing,
            OpaqueKind::OmmlBlock | OpaqueKind::OmmlInline => OpaqueAnchorKind::Equation,
            OpaqueKind::Hyperlink(_) => OpaqueAnchorKind::Hyperlink,
            OpaqueKind::Field(_) => OpaqueAnchorKind::Field,
            OpaqueKind::FootnoteReference(_) => OpaqueAnchorKind::FootnoteRef,
            OpaqueKind::EndnoteReference(_) => OpaqueAnchorKind::EndnoteRef,
            OpaqueKind::CommentReference(_) => OpaqueAnchorKind::Comment,
            // A content control is the discovery primitive: surfaced as its own
            // kind so an agent can filter to content controls by kind alone.
            OpaqueKind::Sdt => OpaqueAnchorKind::ContentControl,
            // SmartArt, Ruby, SmartTag, Sym, Ptab, CustomXml, Unknown(..) are
            // real anchors we don't give a dedicated label — preserved as
            // `Other` rather than dropped.
            OpaqueKind::SmartArt
            | OpaqueKind::Ruby
            | OpaqueKind::SmartTag
            | OpaqueKind::Sym(_)
            | OpaqueKind::Ptab
            | OpaqueKind::CustomXml
            | OpaqueKind::Unknown(_)
            | OpaqueKind::QuarantinedNestedTracking => OpaqueAnchorKind::Other,
        }
    }
}

/// Best-effort visible label for an opaque anchor, when the IR carries one.
/// Fields expose their `result_text`; hyperlinks their display `text`. Anything
/// else has no inline text of its own (its visible text, if any, lives in
/// sibling `Text` inlines) → `None`.
fn opaque_visible_text(node: &OpaqueInlineNode) -> Option<String> {
    match &node.kind {
        OpaqueKind::Field(data) => data.result_text.clone(),
        OpaqueKind::Hyperlink(data) => {
            if data.text.is_empty() {
                None
            } else {
                Some(data.text.clone())
            }
        }
        _ => None,
    }
}

/// The meaningful inline marks of a text run, projected for the view. Boolean
/// marks come from `TextNode.marks`; strike is a tri-state style prop.
fn text_marks(node: &crate::domain::TextNode) -> Vec<TextMark> {
    use crate::domain::{Mark, MarkValue};
    let mut out = Vec::new();
    for mark in &node.marks {
        out.push(match mark {
            Mark::Bold => TextMark::Bold,
            Mark::Italic => TextMark::Italic,
            Mark::Underline => TextMark::Underline,
            Mark::Subscript => TextMark::Subscript,
            Mark::Superscript => TextMark::Superscript,
        });
    }
    if node.style_props.strike == MarkValue::On {
        out.push(TextMark::Strike);
    }
    out
}

/// Map a paragraph's heading level (if any) to the public role.
fn paragraph_role(heading_level: Option<&HeadingLevel>) -> BlockRole {
    match heading_level {
        Some(level) => BlockRole::Heading {
            level: heading_level_to_u8(level),
        },
        None => BlockRole::Paragraph,
    }
}

/// Mirror of `diff::heading_level_to_u8` (kept private there).
fn heading_level_to_u8(level: &HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
        HeadingLevel::H7 => 7,
        HeadingLevel::H8 => 8,
        HeadingLevel::H9 => 9,
    }
}

/// One enumerated span of a paragraph — the **single source of truth** the read
/// view and the write path (`edit::resolve_span`) both consume so a span handle
/// resolves deterministically.
///
/// Each variant carries the source inline range it was projected from
/// (`seg_idx` + half-open `[inline_start, inline_end)` within that segment's
/// `inlines`), so the write path can map a handle back to the exact inlines
/// without re-deriving the projection rule. The ordinal a handle reflects is the
/// span's index in the [`enumerate_text_spans`] output.
// The inline-range fields (`seg_idx`, `inline_start`, `inline_end`,
// `inline_idx`) are the handle→inlines mapping the WRITE path consumes
// (`edit::resolve_span`); the read view uses the projected content. Both are
// driven by THIS one enumeration so a handle resolves to the inlines the reader
// saw — that shared mapping is the whole point.
pub(crate) enum EnumeratedSpan {
    /// A contiguous run of `Text` inlines sharing one tracked status and one
    /// meaningful-mark set. `[inline_start, inline_end)` is the run inside
    /// `segments[seg_idx].inlines`.
    Text {
        seg_idx: usize,
        inline_start: usize,
        inline_end: usize,
        text: String,
        status: TrackStatus,
        marks: Vec<TextMark>,
    },
    /// A single `OpaqueInline` at `inlines[inline_idx]`.
    Opaque {
        seg_idx: usize,
        inline_idx: usize,
        id: NodeId,
        kind: OpaqueAnchorKind,
        status: TrackStatus,
        text: Option<String>,
        /// Projected read metadata (see `crate::opaque_meta::project`). Carried
        /// here so the read view copies it into `SegmentView::Opaque`; the write
        /// path (`edit::resolve_span`) keys on `inline_idx` and ignores it.
        metadata: Option<OpaqueMetadata>,
    },
}

/// Enumerate a paragraph's targetable spans in document order — the shared
/// projection rule behind both the detail view's span handles and the write
/// path's span resolution.
///
/// Rule (kept in ONE place on purpose): walk segments in order; within a
/// segment, contiguous `Text` inlines coalesce into one span, breaking when the
/// meaningful mark set changes; each `OpaqueInline` is its own span and flushes
/// any pending text first. A hard break and each of the three IR-native comment
/// markers (`CommentRangeStart`/`CommentRangeEnd`/`CommentReference` — what the
/// engine's own comment-authoring verb emits; an *imported* `commentReference`
/// arrives as an `OpaqueInline(OpaqueKind::CommentReference)` instead and
/// already took this path) likewise flush any pending text first and become
/// their OWN `EnumeratedSpan::Opaque` entry — same treatment as an
/// `OpaqueInline`, so a text run never again straddles one of these markers
/// (domain-model §4: the view is a COMPLETE list of spans — every text run,
/// every opaque anchor, every hard break). Only a zero-width `Decoration`
/// (bookmarks and the like — "doesn't affect text positions" per its own IR
/// doc comment) stays non-targetable and sits inside a coalesced text run's
/// `[start,end)` range when interleaved; the write path preserves it by
/// reference.
///
/// Invariant: `enumerate_text_spans(p)[n]` and the `n`-th `SegmentView` of
/// `block_to_view`'s paragraph projection describe the same content. The view
/// assigns that span the handle `s_<n>`; `resolve_span` resolves `s_<n>` to this
/// span's inline range. Divergence here would silently mis-resolve a write, so
/// the two MUST stay driven by this one function.
pub(crate) fn enumerate_text_spans(para: &crate::domain::ParagraphNode) -> Vec<EnumeratedSpan> {
    let mut out = Vec::new();
    for (seg_idx, segment) in para.segments.iter().enumerate() {
        let status = TrackStatus::from(&segment.status);
        // Pending coalesced text run: its inline range and accumulated content.
        let mut run_start: Option<usize> = None;
        let mut run_text = String::new();
        let mut run_marks: Vec<TextMark> = Vec::new();
        let mut run_last_end = 0usize; // exclusive end of the pending run

        let flush = |out: &mut Vec<EnumeratedSpan>,
                     run_start: &mut Option<usize>,
                     run_text: &mut String,
                     run_marks: &mut Vec<TextMark>,
                     run_last_end: usize| {
            if let Some(start) = run_start.take() {
                if !run_text.is_empty() {
                    out.push(EnumeratedSpan::Text {
                        seg_idx,
                        inline_start: start,
                        inline_end: run_last_end,
                        text: std::mem::take(run_text),
                        status: status.clone(),
                        marks: std::mem::take(run_marks),
                    });
                } else {
                    run_text.clear();
                    run_marks.clear();
                }
            }
        };

        for (inline_idx, inline) in segment.inlines.iter().enumerate() {
            match inline {
                InlineNode::Text(t) => {
                    let marks = text_marks(t);
                    // A run breaks when its meaningful marks change.
                    if !run_text.is_empty() && marks != run_marks {
                        flush(
                            &mut out,
                            &mut run_start,
                            &mut run_text,
                            &mut run_marks,
                            run_last_end,
                        );
                    }
                    if run_start.is_none() {
                        run_start = Some(inline_idx);
                    }
                    run_marks = marks;
                    run_text.push_str(&t.text);
                    run_last_end = inline_idx + 1;
                }
                InlineNode::OpaqueInline(o) => {
                    flush(
                        &mut out,
                        &mut run_start,
                        &mut run_text,
                        &mut run_marks,
                        run_last_end,
                    );
                    out.push(EnumeratedSpan::Opaque {
                        seg_idx,
                        inline_idx,
                        id: o.id.clone(),
                        kind: OpaqueAnchorKind::from(&o.kind),
                        status: status.clone(),
                        text: opaque_visible_text(o),
                        metadata: crate::opaque_meta::project(o),
                    });
                }
                InlineNode::HardBreak(hb) => {
                    flush(
                        &mut out,
                        &mut run_start,
                        &mut run_text,
                        &mut run_marks,
                        run_last_end,
                    );
                    out.push(EnumeratedSpan::Opaque {
                        seg_idx,
                        inline_idx,
                        id: hb.id.clone(),
                        kind: OpaqueAnchorKind::HardBreak,
                        status: status.clone(),
                        text: None,
                        metadata: None,
                    });
                }
                // The engine's own comment-authoring verb splices all three
                // markers as these native variants (never as an OpaqueInline);
                // an imported `commentReference` already arrives as an
                // OpaqueInline above. Surfacing all three here — not just the
                // reference — gives an agent editing near a comment the same
                // signal an imported document already carries for its anchor:
                // where the commented range starts and ends, not just that a
                // comment exists. `metadata` carries the comment id with the
                // SAME shape (`NoteReference`) the imported reference's
                // metadata already uses, so a caller correlates all three by
                // one field regardless of marker role.
                InlineNode::CommentRangeStart { id } => {
                    flush(
                        &mut out,
                        &mut run_start,
                        &mut run_text,
                        &mut run_marks,
                        run_last_end,
                    );
                    out.push(EnumeratedSpan::Opaque {
                        seg_idx,
                        inline_idx,
                        id: NodeId::from(id.clone()),
                        kind: OpaqueAnchorKind::CommentRangeStart,
                        status: status.clone(),
                        text: None,
                        metadata: Some(OpaqueMetadata::NoteReference {
                            reference_id: id.clone(),
                        }),
                    });
                }
                InlineNode::CommentRangeEnd { id } => {
                    flush(
                        &mut out,
                        &mut run_start,
                        &mut run_text,
                        &mut run_marks,
                        run_last_end,
                    );
                    out.push(EnumeratedSpan::Opaque {
                        seg_idx,
                        inline_idx,
                        id: NodeId::from(id.clone()),
                        kind: OpaqueAnchorKind::CommentRangeEnd,
                        status: status.clone(),
                        text: None,
                        metadata: Some(OpaqueMetadata::NoteReference {
                            reference_id: id.clone(),
                        }),
                    });
                }
                InlineNode::CommentReference { id } => {
                    flush(
                        &mut out,
                        &mut run_start,
                        &mut run_text,
                        &mut run_marks,
                        run_last_end,
                    );
                    out.push(EnumeratedSpan::Opaque {
                        seg_idx,
                        inline_idx,
                        id: NodeId::from(id.clone()),
                        // Same kind an IMPORTED commentReference maps to
                        // (`OpaqueKind::CommentReference` above) — parity.
                        kind: OpaqueAnchorKind::Comment,
                        status: status.clone(),
                        text: None,
                        metadata: Some(OpaqueMetadata::NoteReference {
                            reference_id: id.clone(),
                        }),
                    });
                }
                // Non-targetable: zero-width, doesn't affect text positions
                // (its own IR doc comment). Carries no ordinal of its own; it
                // may sit inside a coalesced text run's range, which the write
                // path preserves by reference.
                InlineNode::Decoration(_) => {
                    if run_start.is_some() {
                        run_last_end = inline_idx + 1;
                    }
                }
            }
        }
        flush(
            &mut out,
            &mut run_start,
            &mut run_text,
            &mut run_marks,
            run_last_end,
        );
    }
    out
}

/// Concatenate the visible text of a paragraph's tracked segments — the
/// targetable, accept-all reading of the block.
fn paragraph_text(segments: &[TrackedSegment]) -> String {
    let mut out = String::new();
    for segment in segments {
        for inline in &segment.inlines {
            if let InlineNode::Text(t) = inline {
                out.push_str(&t.text);
            }
        }
    }
    out
}

/// THE literal-prefix read rule, stated once (CLAUDE.md "name the invariant
/// once"). A `literal_prefix` is a typed-in enumeration label (`"1."`, `"(a)"`)
/// that the prefix detector stripped out of the paragraph's inline runs and
/// stashed in [`ParagraphNode::literal_prefix`]. It is REAL text: the serializer
/// re-emits it as a visible `<w:t>` run, so Word reads it as part of the
/// paragraph. The read surfaces must therefore show it too — its invisibility is
/// what once caused doubled-numbering corruption (a rejected label looked
/// un-restored, so the agent re-typed it).
///
/// The label is shown **iff** there is no structural auto-numbering on the
/// paragraph — the SAME gate the serializer uses (`serialize::mod`: when
/// `numbering.is_some()`, Word generates the label from the numbering
/// definition, so `literal_prefix` is NOT emitted as text). Auto-numbering
/// markers stay metadata (`ListMembership::marker_text`); a literal prefix is
/// text.
///
/// Returns the trimmed label (no separators). `None` when there is no label to
/// show (no prefix, or auto-numbering supersedes it).
pub(crate) fn literal_prefix_label(p: &crate::domain::ParagraphNode) -> Option<&str> {
    if p.numbering.is_some() {
        return None;
    }
    p.literal_prefix
        .as_deref()
        .map(str::trim)
        .filter(|label| !label.is_empty())
}

/// The paragraph's comprehension text: the literal-prefix label (when
/// [`literal_prefix_label`] yields one) followed by a tab, then the body text.
/// `"{label}\t{body}"` mirrors the canonical `rendered_text` the import builds
/// and the run sequence the serializer emits, so a read agrees byte-for-byte
/// with what Word reads. When there is no label, this is exactly the body text.
fn paragraph_comprehension_text(p: &crate::domain::ParagraphNode) -> String {
    let body = paragraph_text(&p.segments);
    match literal_prefix_label(p) {
        Some(label) => format!("{label}\t{body}"),
        None => body,
    }
}

/// Build a `BlockView` for one tracked block. `role_ids` maps every paragraph
/// `NodeId` to the role token an `insert` op accepts for a paragraph of that
/// formatting (`vocabulary::paragraph_role_ids`), so the view surfaces a role a
/// cold agent can author with.
fn block_to_view(tracked: &TrackedBlock, role_ids: &HashMap<NodeId, String>) -> BlockView {
    let block_status = TrackStatus::from(&tracked.status);
    // When the whole block is tracked-inserted/deleted, the paragraph mark's
    // own status is subsumed by the block status (mirrors
    // `project_tracked_document`, which only surfaces para_mark_status when the
    // block itself is Normal).
    let block_is_tracked = !matches!(tracked.status, TrackingStatus::Normal);

    // The block's staleness guard is its semantic hash at read time — the SAME
    // value a write op carries as `guard`. Computed for every block kind so the
    // single-staleness-mechanism contract holds uniformly.
    let guard = crate::semantic_hash::block_guard(&tracked.block);

    match &tracked.block {
        BlockNode::Paragraph(p) => {
            // Build the inline projection from the SHARED span enumeration so the
            // `s_<n>` handles here are exactly what `edit::resolve_span` resolves.
            let segments = segment_views_with_handles(p);
            let paragraph_mark_status = if block_is_tracked {
                TrackStatus::Normal
            } else {
                p.para_mark_status
                    .as_ref()
                    .map(TrackStatus::from)
                    .unwrap_or(TrackStatus::Normal)
            };
            BlockView {
                id: p.id.clone(),
                role: paragraph_role(p.heading_level.as_ref()),
                style_id: p.style_id.as_ref().map(|s| s.to_string()),
                role_token: role_ids.get(&p.id).cloned(),
                list: list_membership(p),
                cells: Vec::new(),
                table: None,
                text: paragraph_comprehension_text(p),
                literal_prefix: literal_prefix_label(p).map(str::to_string),
                block_status,
                paragraph_mark_status,
                guard,
                segments,
                opaque_label: None,
            }
        }
        BlockNode::Table(t) => BlockView {
            id: t.id.clone(),
            role: BlockRole::Table,
            style_id: None,
            role_token: None,
            list: None,
            cells: table_cell_views(t),
            table: Some(table_meta_view(t)),
            text: crate::diff::extract_table_text(t),
            // A table is not a paragraph — no literal-prefix enumeration label.
            literal_prefix: None,
            block_status,
            paragraph_mark_status: TrackStatus::Normal,
            guard,
            segments: Vec::new(),
            opaque_label: None,
        },
        BlockNode::OpaqueBlock(o) => BlockView {
            id: o.id.clone(),
            role: BlockRole::Opaque,
            style_id: None,
            role_token: None,
            list: None,
            cells: Vec::new(),
            table: None,
            text: String::new(),
            literal_prefix: None,
            block_status,
            paragraph_mark_status: TrackStatus::Normal,
            guard,
            segments: Vec::new(),
            opaque_label: Some(crate::edit::opaque_kind_label(&o.kind).to_string()),
        },
    }
}

/// Project a paragraph's Word-auto-numbering into [`ListMembership`]. `None` when
/// the paragraph carries no `w:numPr` (plain text, or a literal-prefix "list"
/// which is not Word numbering and has no `numId`/`ilvl` to target).
fn list_membership(p: &crate::domain::ParagraphNode) -> Option<ListMembership> {
    let num = p.numbering.as_ref()?;
    Some(ListMembership {
        num_id: num.num_id,
        ilvl: num.ilvl,
        ordered: !num.is_bullet,
        marker_text: num.synthesized_text.clone(),
    })
}

/// Project a table's logical grid into per-cell [`TableCellView`]s in row-major
/// order. The `col` is the logical grid column: it advances by each cell's
/// `grid_span`, and `grid_before` empty columns are skipped, so a cell's `col`
/// is its true grid position (what `table_op.set_cell_text` addresses). Vertical
/// merge continuations are surfaced too (they carry the merged cell's text in
/// the IR); the anchor cell holds the content.
fn table_cell_views(t: &crate::domain::TableNode) -> Vec<TableCellView> {
    // Reuse the engine's resolved grid: anchor cells only, with rowspan/colspan
    // (vMerge continuations folded in) and per-cell EFFECTIVE formatting (borders
    // + shading already resolved at import). On a malformed table (vMerge with no
    // anchor) fall back to a flat walk so text still renders.
    match crate::table::canonicalize_table(t) {
        Ok(canonical) => canonical
            .cells
            .iter()
            .map(|c| {
                let f = &c.formatting;
                TableCellView {
                    row: c.row,
                    col: c.col,
                    text: cell_text_from_blocks(&c.blocks),
                    col_span: c.colspan.max(1),
                    row_span: c.rowspan.max(1),
                    borders: project_cell_borders(f.borders.as_ref()),
                    shading: f
                        .shading
                        .as_ref()
                        .and_then(|s| s.fill.clone())
                        .filter(|h| h != "auto"),
                    v_align: f.v_align.as_ref().map(|v| {
                        match v {
                            crate::domain::VerticalAlignment::Top => "top",
                            crate::domain::VerticalAlignment::Center => "center",
                            crate::domain::VerticalAlignment::Bottom => "bottom",
                        }
                        .to_string()
                    }),
                    paragraphs: cell_paragraph_views(&c.blocks),
                }
            })
            .collect(),
        Err(_) => t
            .rows
            .iter()
            .enumerate()
            .flat_map(|(row_idx, row)| {
                let mut col = row.grid_before as usize;
                row.cells
                    .iter()
                    .map(|cell| {
                        let v = TableCellView {
                            row: row_idx,
                            col,
                            text: cell_text(cell),
                            col_span: cell.grid_span.max(1) as usize,
                            row_span: 1,
                            borders: project_cell_borders(cell.formatting.borders.as_ref()),
                            shading: cell
                                .formatting
                                .shading
                                .as_ref()
                                .and_then(|s| s.fill.clone())
                                .filter(|h| h != "auto"),
                            v_align: None,
                            paragraphs: cell_paragraph_views(&cell.blocks),
                        };
                        col += cell.grid_span.max(1) as usize;
                        v
                    })
                    .collect::<Vec<_>>()
            })
            .collect(),
    }
}

/// Project a cell's resolved `BorderSet` to the 4 render edges. The engine has
/// already resolved cell-vs-table-vs-style precedence into these edges, so we
/// just carry the 4 outer ones (inside_h/inside_v are not used for rendering).
fn project_cell_borders(b: Option<&crate::domain::BorderSet>) -> CellBordersView {
    match b {
        Some(b) => CellBordersView {
            top: b.top.clone(),
            bottom: b.bottom.clone(),
            left: b.left.clone(),
            right: b.right.clone(),
        },
        None => CellBordersView::default(),
    }
}

/// Table-level render metadata (column widths, alignment, indent).
fn table_meta_view(t: &crate::domain::TableNode) -> TableMetaView {
    let f = &t.formatting;
    TableMetaView {
        cols: f.grid_cols.clone(),
        align: f.alignment.as_ref().map(|a| {
            match a {
                crate::domain::Alignment::Center => "center",
                crate::domain::Alignment::Right => "right",
                _ => "left",
            }
            .to_string()
        }),
        indent: f.indent,
    }
}

/// Project a table cell's paragraph blocks into render-ready inline segments,
/// one [`CellParagraphView`] per paragraph in document order.
///
/// Uses the SAME projection the body's single-document rich view uses for an
/// unchanged paragraph (`diff::inlines_to_segments` over the paragraph's owned
/// inlines), so a cell run carries the identical `marks` + `style_props` +
/// hyperlink shape the body does and a frontend renders both through one path.
///
/// Two documented boundaries, both consistent with the lean view's contract of
/// operating on a bare `CanonDoc` with no media/notes side-channel:
/// - **No image data.** `enrich_segments_with_assets` is not run (the lean view
///   has no image-data lookup), so a drawing inside a cell stays an `Opaque`
///   with no `asset_ref` — a frontend shows its fallback label, not pixels.
/// - **No footnote/endnote ordinals.** Note markers are looked up empty, so a
///   footnote reference inside a cell carries no synthesized "1"/"2" marker.
///   (Footnotes inside table cells are vanishingly rare; the body path threads
///   the doc-level lookup, which is not reachable here without widening the
///   block-walk signature.)
fn cell_paragraph_views(blocks: &[BlockNode]) -> Vec<CellParagraphView> {
    let note_markers = HashMap::new();
    blocks
        .iter()
        .filter_map(|block| match block {
            BlockNode::Paragraph(p) => {
                // Per-segment projection (the same vocabulary the body uses) so a
                // tracked change INSIDE a cell surfaces as Inserted/Deleted
                // segments — a redline — not a flat all-Unchanged run.
                let mut segments = Vec::new();
                for tracked_seg in &p.segments {
                    let change_type_str = match &tracked_seg.status {
                        crate::domain::TrackingStatus::Normal => "equal",
                        crate::domain::TrackingStatus::Inserted(_) => "insert",
                        crate::domain::TrackingStatus::Deleted(_)
                        | crate::domain::TrackingStatus::InsertedThenDeleted(_) => "delete",
                    };
                    let mut seg_parts = crate::diff::inlines_to_segments(
                        &tracked_seg.inlines,
                        change_type_str,
                        &note_markers,
                    );
                    let seg_rev = match &tracked_seg.status {
                        crate::domain::TrackingStatus::Inserted(r)
                        | crate::domain::TrackingStatus::Deleted(r) => r.identity,
                        _ => 0,
                    };
                    if seg_rev != 0 {
                        for part in &mut seg_parts {
                            match part {
                                crate::domain::InlineChange::Inserted { rev_id, .. }
                                | crate::domain::InlineChange::Deleted { rev_id, .. } => {
                                    *rev_id = seg_rev
                                }
                                _ => {}
                            }
                        }
                    }
                    segments.extend(seg_parts);
                }
                Some(CellParagraphView {
                    segments,
                    block_id: p.id.to_string(),
                    guard: crate::semantic_hash::block_semantic_hash_for_paragraph(p),
                })
            }
            // Nested tables / opaque blocks inside a cell are not projected as
            // inline paragraphs here (the render frontend does not recurse into
            // them yet); their text still contributes to the flat `text`.
            _ => None,
        })
        .collect()
}

/// Cell text from already-extracted canonical blocks (mirrors [`cell_text`]).
fn cell_text_from_blocks(blocks: &[BlockNode]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            BlockNode::Paragraph(p) => Some(paragraph_text(&p.segments)),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Concatenate the visible text of every paragraph in a table cell, blocks
/// joined by a single space — the accept-all reading used for cell addressing.
fn cell_text(cell: &crate::domain::TableCellNode) -> String {
    let mut parts: Vec<String> = Vec::new();
    for block in &cell.blocks {
        if let BlockNode::Paragraph(p) = block {
            parts.push(paragraph_text(&p.segments));
        }
    }
    parts.join(" ")
}

/// Project a paragraph's enumerated spans into `SegmentView`s, assigning each its
/// document-order handle `s_<n>`. The ordinal is the span's index in
/// [`enumerate_text_spans`] — the same index `edit::resolve_span` keys on — so a
/// handle read here resolves to the same inlines on write.
fn segment_views_with_handles(p: &crate::domain::ParagraphNode) -> Vec<SegmentView> {
    enumerate_text_spans(p)
        .into_iter()
        .enumerate()
        .map(|(ordinal, span)| {
            let handle = Some(SpanHandle(format!("s_{ordinal}")));
            match span {
                EnumeratedSpan::Text {
                    text,
                    status,
                    marks,
                    ..
                } => SegmentView::Text {
                    text,
                    status,
                    marks,
                    handle,
                },
                EnumeratedSpan::Opaque {
                    id,
                    kind,
                    status,
                    text,
                    metadata,
                    ..
                } => SegmentView::Opaque {
                    id,
                    kind,
                    status,
                    text,
                    handle,
                    metadata,
                },
            }
        })
        .collect()
}

/// Build the designed read projection from a snapshot's canonical IR.
///
/// Single document, no diff: walks `snapshot.canonical.blocks` in order and
/// projects each into a clean [`BlockView`].
pub fn build_document_view(snapshot: &EditSnapshot) -> DocumentView {
    build_document_view_from_canon(&snapshot.canonical)
}

/// Build the designed read projection from a bare canonical IR.
///
/// Same projection as [`build_document_view`], but takes a `&CanonDoc`
/// directly rather than an [`EditSnapshot`]. The merged document produced by
/// `merge_diff` is a bare `CanonDoc` (not a snapshot), and atom extraction
/// needs to read it through this clean view — so the canon-level entry point is
/// public.
pub fn build_document_view_from_canon(canonical: &CanonDoc) -> DocumentView {
    // The per-block role token is the SAME role vocabulary the insert op
    // validates against (`vocabulary::paragraph_role_ids`), built in one walk so
    // a surfaced token always resolves on write.
    let role_ids = crate::vocabulary::paragraph_role_ids(canonical);
    let blocks = canonical
        .blocks
        .iter()
        .map(|b| block_to_view(b, &role_ids))
        .collect();
    DocumentView { blocks }
}

/// Render a [`DocumentView`] as a single plain-text string.
///
/// This is the **human-readable consumption surface** (what a reader, or Word,
/// would see). It is *not* the same string as the engine's block-identity surface
/// ([`crate::import::extract_block_text`]); the two diverge on fields, by design
/// (see below).
///
/// **Text definition:**
/// - each visible [`SegmentView::Text`] contributes its `text` verbatim;
/// - a [`SegmentView::Opaque`] anchor of kind [`OpaqueAnchorKind::Field`]
///   contributes its **cached result text** — the displayed field result Word
///   shows (e.g. a `PAGE` field reads as `7`, a `FILENAME` field as its file
///   name). A complex field's structural markers (begin / instrText / separate /
///   end) carry no result, so they contribute nothing — matching Word, which
///   displays nothing for them;
/// - a [`SegmentView::Opaque`] anchor of kind [`OpaqueAnchorKind::HardBreak`]
///   contributes **nothing** — a break (line/page/column, `<w:br/>`) is
///   layout-only (ECMA-376 §17.3.3.1/§17.18.3/§17.18.4): Word concatenates the
///   surrounding text directly across it, no character, not even a space
///   (`spec_runcontent_breaks_tabs_word_compliance` pins this: `"A"<w:br/>"B"`
///   reads as `"AB"`). It still occupies its own span position in the
///   structured view (see [`enumerate_text_spans`]) and a visual line
///   separation in the html/markdown renders — only the FLAT text string is
///   silent about it, matching Word's own reading;
/// - a [`SegmentView::Opaque`] anchor of kind [`OpaqueAnchorKind::CommentRangeStart`]
///   or [`OpaqueAnchorKind::CommentRangeEnd`] contributes nothing — these
///   range boundaries are genuinely zero-width in Word (ECMA-376 §17.13.2),
///   unlike the comment reference below;
/// - every other [`SegmentView::Opaque`] anchor (drawing, equation,
///   footnote/endnote/comment reference, hyperlink anchor, …) is a true
///   no-text object and contributes **exactly one** U+FFFC (OBJECT
///   REPLACEMENT CHARACTER, Unicode 5.4.6);
/// - blocks are joined by a blank line (`"\n\n"`).
///
/// **Why this diverges from the block-identity surface.**
/// [`crate::import::extract_block_text`] (and the story content hash built on it)
/// surfaces *every* opaque anchor — fields included — as one U+FFFC, so the
/// opaque inventory is countable from that text and block identity stays stable
/// against volatile field results (a `PAGE` field whose result flips from `7` to
/// `8` must not change the block's hash). That is the diff/identity contract.
/// The human-readable surface here answers a different question — "what does a
/// reader see?" — and for a field the answer is its displayed result, not a
/// placeholder. The two are intentionally different functions with different
/// contracts; do not collapse them.
///
/// This reads from the *projected view* (accept-all reading of tracked status:
/// the view surfaces both Deleted and Inserted spans, so the string is the
/// union of visible text — call [`crate::api::Document::read_accepted`] /
/// [`read_rejected`](crate::api::Document::read_rejected) first to pick a
/// resolution). It deliberately does **not** reuse [`paragraph_text`] (which
/// drops opaque anchors), because that would drop the field results and U+FFFC
/// objects this surface must render.
///
/// `HardBreak` diverges, the same intentional way fields do (see above):
/// `extract_block_text`'s block-identity surface renders it as `\n` (a
/// content-hash concern — two paragraphs split across a break must not
/// collapse to one for identity purposes), while this human-readable surface
/// renders it as nothing (a layout-only mark Word itself doesn't read as a
/// character; see above). The comment range boundaries agree between the two
/// surfaces (both render nothing — genuinely zero-width, not merely
/// block-identity-stable). Only `CommentReference` diverges the OTHER way:
/// `extract_block_text`'s
/// block-identity surface renders it as nothing (a comment must not perturb
/// block identity — the same reason a field's volatile result doesn't), while
/// this human-readable surface renders it as one U+FFFC (a reader sees
/// SOMETHING marks the comment, even though the comment's own text lives in
/// the story, not inline). Tables flatten to their cell text in row-major
/// order with a trailing space per block, matching `extract_block_text`'s table handling
/// only at the opaque-count level (the view models a table block with empty
/// `segments`, so a table currently contributes no per-cell text to the plain
/// string; its block text lives in `BlockView::text`). These are limits of the
/// view, not silent fallbacks: the function never invents content and never
/// drops an opaque anchor.
pub fn to_plain_text(view: &DocumentView) -> String {
    to_plain_text_blocks(&view.blocks)
}

// ─── Structural index + windowing (read-surface scale tier) ───────────────────
//
// Pure read-only projections of `DocumentView`. The structural index is a
// one-walk summary (one entry per block, in document order) for navigating a
// large document without rendering its full text; the windowing tier resolves a
// pair of block ids to a sub-slice that the EXISTING slice renderers
// (`to_plain_text_blocks`, `to_extended_markdown_blocks`, `to_html_blocks`)
// render — so a windowed read is, by construction, a slice of the full read.

/// The maximum number of characters in an [`OutlineEntry::text_preview`].
///
/// A deliberate constant (not a tuning knob): a structural index entry shows
/// *enough* of a block to recognize it, never its whole body. Cut on a UTF-8
/// char boundary (`char_indices`), so the preview is always valid UTF-8 and
/// never splits a multi-byte character.
pub const OUTLINE_PREVIEW_CHARS: usize = 120;

/// A structural index of a [`DocumentView`]: one entry per block in document
/// order, plus document-level totals. The navigation tier for a large document
/// — read this to find the block ids worth windowing into, without paying to
/// render the whole body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct DocumentOutline {
    /// One entry per block, in document order. `entries[i]` describes
    /// `blocks[i]` (the index is faithful: `entries[i].index == i` and
    /// `entries[i].id == blocks[i].id`).
    pub entries: Vec<OutlineEntry>,
    /// Total number of blocks (== `entries.len()`).
    pub total_blocks: usize,
    /// Total visible characters across all blocks (`sum of entry.char_len`),
    /// counted as Unicode scalar values, not bytes.
    pub total_chars: usize,
}

/// One block's row in a [`DocumentOutline`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct OutlineEntry {
    /// Stable block id — identical to `blocks[index].id`. The handle a window
    /// or an [`crate::edit::EditTransaction`] targets.
    pub id: NodeId,
    /// Document-order position of this block (`0`-based). Faithful: equals the
    /// block's index in [`DocumentView::blocks`].
    pub index: usize,
    /// The block's role (paragraph / heading / table / opaque).
    pub role: BlockRole,
    /// Nesting depth: the level of the nearest preceding heading (a `Heading`
    /// itself carries its own level). Body before any heading is depth `0`.
    pub depth: u8,
    /// The first [`OUTLINE_PREVIEW_CHARS`] characters of the block's visible
    /// text, cut on a char boundary (never mid-codepoint).
    pub text_preview: String,
    /// Visible-text length in Unicode scalar values (`text.chars().count()`).
    pub char_len: usize,
    /// Visible-text length in UTF-8 bytes (`text.len()`).
    pub byte_len: usize,
    /// The block-level tracked status (whole-block insert/delete).
    pub block_status: TrackStatus,
    /// The insert-acceptable role token for this block (mirrors
    /// [`BlockView::role_token`]) — surfaced in the index so a cold agent can
    /// pick an authoring role while navigating, without a detail read. `None`
    /// for non-paragraph blocks.
    pub role_token: Option<String>,
    /// List/numbering membership (mirrors [`BlockView::list`]) — surfaced in the
    /// index so list paragraphs are discoverable (and the granular list ops
    /// targetable) from the navigation tier. `None` for non-list paragraphs.
    pub list: Option<ListMembership>,
}

/// Build the [`DocumentOutline`] structural index of a view in a single walk.
///
/// Faithfulness invariants (named once, here): for the returned outline,
/// `entries.len() == view.blocks.len()`, and for every `i`,
/// `entries[i].id == view.blocks[i].id` and `entries[i].index == i`. `depth`
/// tracks the running nearest-preceding-heading level: a `Heading{level}` sets
/// the running depth to `level` and takes it as its own depth; any other block
/// takes the running depth; body before the first heading is depth `0`.
pub fn build_outline(view: &DocumentView) -> DocumentOutline {
    let mut entries = Vec::with_capacity(view.blocks.len());
    let mut total_chars = 0usize;
    let mut running_depth: u8 = 0;
    for (index, block) in view.blocks.iter().enumerate() {
        if let BlockRole::Heading { level } = block.role {
            running_depth = level;
        }
        let char_len = block.text.chars().count();
        total_chars += char_len;
        entries.push(OutlineEntry {
            id: block.id.clone(),
            index,
            role: block.role.clone(),
            depth: running_depth,
            text_preview: preview(&block.text),
            char_len,
            byte_len: block.text.len(),
            block_status: block.block_status.clone(),
            role_token: block.role_token.clone(),
            list: block.list.clone(),
        });
    }
    DocumentOutline {
        total_blocks: entries.len(),
        total_chars,
        entries,
    }
}

/// The first [`OUTLINE_PREVIEW_CHARS`] characters of `text`, cut on a UTF-8 char
/// boundary so the result is always valid (never splits a codepoint).
fn preview(text: &str) -> String {
    match text.char_indices().nth(OUTLINE_PREVIEW_CHARS) {
        Some((byte_idx, _)) => text[..byte_idx].to_string(),
        None => text.to_string(),
    }
}

/// Failure resolving a block-id window. Fail-loud (CLAUDE.md — no silent
/// fallbacks): an unknown id is never treated as "start of doc", and an
/// out-of-order pair is never silently swapped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowError {
    /// A `from`/`to` id resolved to no block in the view. Carries the offending
    /// id string.
    AnchorNotFound(String),
    /// `from` resolves to a position after `to` in document order.
    OutOfOrder { from: usize, to: usize },
}

impl std::fmt::Display for WindowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WindowError::AnchorNotFound(id) => {
                write!(
                    f,
                    "window anchor not found: block id '{id}' is not in the document"
                )
            }
            WindowError::OutOfOrder { from, to } => write!(
                f,
                "window endpoints out of document order: from (#{from}) comes after to (#{to})"
            ),
        }
    }
}

impl std::error::Error for WindowError {}

/// Resolve an inclusive `from_id..=to_id` window to the block sub-slice it names.
///
/// Both ids must resolve to a block (else [`WindowError::AnchorNotFound`]) and
/// `from` must not come after `to` (else [`WindowError::OutOfOrder`]). The
/// returned slice borrows the view's blocks, so rendering it with one of the
/// slice renderers (`to_plain_text_blocks` / `to_extended_markdown_blocks` /
/// `to_html_blocks`) yields exactly the slice of the corresponding full-document
/// render — the windowed-read == slice-of-full-read invariant, by construction.
pub fn block_range<'a>(
    view: &'a DocumentView,
    from_id: &str,
    to_id: &str,
) -> Result<&'a [BlockView], WindowError> {
    let pos = |id: &str| view.blocks.iter().position(|b| b.id.to_string() == id);
    let from = pos(from_id).ok_or_else(|| WindowError::AnchorNotFound(from_id.to_string()))?;
    let to = pos(to_id).ok_or_else(|| WindowError::AnchorNotFound(to_id.to_string()))?;
    if from > to {
        return Err(WindowError::OutOfOrder { from, to });
    }
    Ok(&view.blocks[from..=to])
}

/// Render a slice of [`BlockView`]s to plain text under the same definition as
/// [`to_plain_text`]. Exposed so windowed callers (a section, a single block)
/// can render a sub-range without rebuilding a [`DocumentView`].
pub fn to_plain_text_blocks(blocks: &[BlockView]) -> String {
    let mut rendered: Vec<String> = Vec::with_capacity(blocks.len());
    for block in blocks {
        rendered.push(block_plain_text(block));
    }
    rendered.join("\n\n")
}

/// The plain text of one block: concatenate visible text segments, surfacing
/// each opaque anchor as what a human reader sees there (see the rule on
/// [`to_plain_text`]) — a field's cached result text, one U+FFFC for any other
/// opaque object.
///
/// A `Table` / `Opaque` block has no inline `segments` in the view; its text
/// lives in [`BlockView::text`], which we fall back to *only* for those roles
/// (a paragraph always builds from `segments` so opaque anchors render through
/// the rule below rather than being dropped by `text`'s paragraph_text
/// concatenation).
fn block_plain_text(block: &BlockView) -> String {
    if block.segments.is_empty() {
        // Table / opaque / empty paragraph: no inline structure to walk. The
        // table block's flattened cell text is the only place a Table's text
        // is available in the view. (A literal prefix only rides on a paragraph,
        // which always has at least the body segment, so it is handled below.)
        return block.text.clone();
    }
    let mut out = String::new();
    // The literal-prefix enumeration label is real text Word reads (see
    // `literal_prefix_label`); surface it ahead of the body, as `"{label}\t"`,
    // the same rendering `BlockView::text` carries.
    if let Some(label) = &block.literal_prefix {
        out.push_str(label);
        out.push('\t');
    }
    for seg in &block.segments {
        match seg {
            SegmentView::Text { text, .. } => out.push_str(text),
            SegmentView::Opaque { kind, text, .. } => out.push_str(&opaque_read_text(*kind, text)),
        }
    }
    out
}

/// How a single opaque anchor reads in the human-readable plain-text projection.
///
/// A **field** displays its cached result to the reader — Word shows that text,
/// not a placeholder glyph — so a `Field` anchor surfaces its captured
/// `result_text` (`text`) when it has one. A field *structural marker* (a
/// complex field's begin / instrText / separate / end fldChar) has no result and
/// no textual representation of its own; it contributes nothing, exactly as Word
/// displays nothing for it. A **hard break** (line/page/column — `<w:br/>`) and
/// **comment range boundaries** (`CommentRangeStart`/`CommentRangeEnd`) are
/// likewise genuinely zero-width: `spec_runcontent_breaks_tabs_word_compliance`
/// pins a break as "layout-only" per ECMA-376 §17.3.3.1/§17.18.3/§17.18.4 — Word
/// concatenates the surrounding text directly across it, no character, not even
/// a space — and `spec_comments_bookmarks_permissions_word_compliance` pins the
/// same for a comment range boundary per §17.13.2 (it only brackets which text
/// is annotated). Both contribute nothing, exactly like the field structural
/// markers above. The comment **reference** (`Comment`) is the one part of a
/// comment with any visible trace — Word renders a balloon/indicator at that
/// position — so it (like every other true no-text object: drawing, equation,
/// footnote/endnote reference, hyperlink anchor, …) reads as one U+FFFC
/// (Unicode 5.4.6).
///
/// This is the consumption surface only; [`to_plain_text`] documents where it
/// intentionally diverges from the anchor-counting / block-identity surface
/// ([`crate::import::extract_block_text`] and the story content hash).
fn opaque_read_text(kind: OpaqueAnchorKind, text: &Option<String>) -> String {
    match kind {
        OpaqueAnchorKind::Field => text.clone().unwrap_or_default(),
        OpaqueAnchorKind::HardBreak
        | OpaqueAnchorKind::CommentRangeStart
        | OpaqueAnchorKind::CommentRangeEnd => String::new(),
        _ => "\u{FFFC}".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Document;
    use crate::domain::{NodeId, RevisionInfo};
    use crate::edit::{
        ContentFragment, EditStep, EditTransaction, MaterializationMode, ParagraphContent,
    };

    // Compile-time property (domain-model §9): `DocumentView` exposes NO
    // `InlineChange`, `FullDocBlock`, or `CanonDoc`/`domain` IR type in its
    // public surface. The only `domain` type that appears is `NodeId`, the
    // intentional targeting handle. This is enforced by the signatures above,
    // not by a runtime assertion.

    /// Build a minimal valid DOCX byte stream (copied from `api.rs` tests).
    fn make_test_docx(paragraphs: &[&str]) -> Vec<u8> {
        let mut document_xml = String::from(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>"#,
        );
        for para in paragraphs {
            document_xml.push_str(&format!(r#"<w:p><w:r><w:t>{para}</w:t></w:r></w:p>"#));
        }
        document_xml.push_str("<w:sectPr/></w:body></w:document>");

        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
        let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
        let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;

        use std::io::Write;
        use zip::write::FileOptions;
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts: FileOptions = FileOptions::default();
            zip.start_file("[Content_Types].xml", opts).unwrap();
            zip.write_all(content_types.as_bytes()).unwrap();
            zip.start_file("_rels/.rels", opts).unwrap();
            zip.write_all(rels.as_bytes()).unwrap();
            zip.start_file("word/_rels/document.xml.rels", opts)
                .unwrap();
            zip.write_all(doc_rels.as_bytes()).unwrap();
            zip.start_file("word/document.xml", opts).unwrap();
            zip.write_all(document_xml.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        buf
    }

    fn replace_paragraph_txn(block_id: &str, expect: &str, replacement: &str) -> EditTransaction {
        EditTransaction {
            steps: vec![EditStep::ReplaceParagraphText {
                block_id: NodeId::from(block_id),
                rationale: None,
                replacement_role: None,
                expect: expect.to_string(),
                semantic_hash: None,
                content: ParagraphContent {
                    fragments: vec![ContentFragment::Text(replacement.to_string())],
                },
            }],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: RevisionInfo {
                revision_id: 1,
                identity: 0,
                author: Some("Test".to_string()),
                date: Some("2026-05-31T00:00:00Z".to_string()),
                apply_op_id: None,
            },
        }
    }

    #[test]
    fn plain_two_paragraph_doc_reads_as_two_normal_paragraphs() {
        // Domain rule: a pristine document has zero deltas — every block, every
        // segment, and the paragraph mark are Normal. Two plain `<w:p>`s →
        // two Paragraph blocks whose text is exactly the run text, each with a
        // single Normal Text segment.
        let docx = make_test_docx(&["Hello world", "Second paragraph"]);
        let doc = Document::parse(&docx).expect("parse");
        let view = doc.read();

        assert_eq!(view.blocks.len(), 2, "two paragraphs → two blocks");

        let expected_text = ["Hello world", "Second paragraph"];
        for (block, expected) in view.blocks.iter().zip(expected_text) {
            assert_eq!(block.role, BlockRole::Paragraph);
            assert_eq!(block.text, expected);
            assert_eq!(block.block_status, TrackStatus::Normal);
            assert_eq!(block.paragraph_mark_status, TrackStatus::Normal);
            assert_eq!(block.segments.len(), 1, "one coalesced text segment");
            match &block.segments[0] {
                SegmentView::Text {
                    text,
                    status,
                    marks,
                    handle,
                } => {
                    assert_eq!(text, expected);
                    assert_eq!(*status, TrackStatus::Normal);
                    assert!(marks.is_empty(), "plain text carries no marks");
                    assert_eq!(
                        handle.as_ref().map(|h| h.0.as_str()),
                        Some("s_0"),
                        "the first (and only) span gets handle s_0"
                    );
                }
                SegmentView::Opaque { .. } => panic!("plain paragraph has no opaque anchor"),
            }
        }
    }

    #[test]
    fn tracked_word_replacement_is_visible_as_deleted_and_inserted_segments() {
        // Domain rule: a tracked word-level replacement is an attributed delta —
        // the old word survives as a Deleted text span and the new word as an
        // Inserted text span, both inside an otherwise-Normal block (only the
        // word changed, not the whole block, and not the paragraph mark).
        let docx = make_test_docx(&["Hello world"]);
        let doc = Document::parse(&docx).expect("parse");
        let block_id = doc.read().blocks[0].id.to_string();

        let txn = replace_paragraph_txn(&block_id, "Hello world", "Goodbye world");
        let edited = doc.apply(&txn).expect("apply");
        let view = edited.read();

        assert_eq!(view.blocks.len(), 1);
        let block = &view.blocks[0];
        assert_eq!(
            block.block_status,
            TrackStatus::Normal,
            "only a word changed; the block itself is not whole-tracked"
        );

        // A tracked span must carry the revision metadata that produced it: the
        // view projects `RevisionInfo`, so a Deleted/Inserted span exposes the
        // revision id and (here) the author the edit was stamped with.
        let has_deleted = block.segments.iter().any(|s| {
            matches!(
                s,
                SegmentView::Text {
                    status: TrackStatus::Deleted(rev),
                    ..
                } if rev.revision_id > 0 && rev.author.is_some()
            )
        });
        let has_inserted = block.segments.iter().any(|s| {
            matches!(
                s,
                SegmentView::Text {
                    status: TrackStatus::Inserted(rev),
                    ..
                } if rev.revision_id > 0 && rev.author.is_some()
            )
        });
        assert!(
            has_deleted,
            "the replaced word must appear as a Deleted span carrying its revision"
        );
        assert!(
            has_inserted,
            "the replacement word must appear as an Inserted span carrying its revision"
        );

        // `text` concatenates every Text inline (the targetable visible text),
        // so both the surviving and the inserted run text are present.
        assert!(
            block.text.contains("Goodbye") && block.text.contains("world"),
            "view text carries the visible run text"
        );
    }

    /// Build a minimal DOCX whose body is `document_xml_body` (the inner-of-body
    /// XML), so a test can inject opaque inlines (`<w:fldSimple>`, drawings).
    fn make_docx_with_body(body_inner: &str) -> Vec<u8> {
        let document_xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
        );
        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
        let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
        let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
        use std::io::Write;
        use zip::write::FileOptions;
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts: FileOptions = FileOptions::default();
            zip.start_file("[Content_Types].xml", opts).unwrap();
            zip.write_all(content_types.as_bytes()).unwrap();
            zip.start_file("_rels/.rels", opts).unwrap();
            zip.write_all(rels.as_bytes()).unwrap();
            zip.start_file("word/_rels/document.xml.rels", opts)
                .unwrap();
            zip.write_all(doc_rels.as_bytes()).unwrap();
            zip.start_file("word/document.xml", opts).unwrap();
            zip.write_all(document_xml.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        buf
    }

    #[test]
    fn to_plain_text_joins_blocks_by_blank_line_and_equals_per_block_text() {
        // Domain rule: the plain-text projection joins blocks by a blank line,
        // and for plain paragraphs each block's contribution equals the
        // per-block visible text (BlockView::text). This pins the join contract
        // against an independent oracle (the view's own per-block text).
        let docx = make_test_docx(&["First paragraph", "Second paragraph"]);
        let doc = Document::parse(&docx).expect("parse");
        let view = doc.read();

        let expected = view
            .blocks
            .iter()
            .map(|b| b.text.clone())
            .collect::<Vec<_>>()
            .join("\n\n");
        assert_eq!(
            to_plain_text(&view),
            expected,
            "pristine doc plain text == per-block BlockView.text joined by \\n\\n"
        );
        assert_eq!(to_plain_text(&view), "First paragraph\n\nSecond paragraph");
    }

    #[test]
    fn literal_prefix_label_reads_as_text_and_is_not_a_span() {
        // A typed-in enumeration label ("A.\t") is stripped into literal_prefix
        // at import, but Word reads it as text (the serializer re-emits it). The
        // read view must therefore lead `text` (and plain text) with the label,
        // surface it as `literal_prefix`, yet keep `segments` body-only — the
        // label is structural, not a targetable span. This is the read that was
        // previously blind.
        let body = r#"<w:p><w:r><w:t xml:space="preserve">A.&#9;First item body</w:t></w:r></w:p>"#;
        let doc = Document::parse(&make_docx_with_body(body)).expect("parse");
        let view = doc.read();
        let block = &view.blocks[0];

        assert_eq!(
            block.text, "A.\tFirst item body",
            "text reads label + tab + body"
        );
        assert_eq!(
            block.literal_prefix.as_deref(),
            Some("A."),
            "label surfaced as metadata"
        );
        // Plain text agrees (it is the same comprehension reading).
        assert_eq!(to_plain_text(&view), "A.\tFirst item body");
        // Segments carry only the body — the label is not a span.
        let span_text: String = block
            .segments
            .iter()
            .filter_map(|s| match s {
                SegmentView::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(span_text, "First item body", "segments are body-only");
    }

    #[test]
    fn to_plain_text_surfaces_field_result_and_one_fffc_for_other_objects() {
        // Domain rule: the human-readable plain-text surface shows a field's
        // cached result (what Word displays), while a no-text object (here a
        // drawing) reads as one U+FFFC. The field's body run "Section 2" is its
        // cached result and must read as text; the surrounding runs are verbatim.
        let body = r#"<w:p><w:r><w:t>See </w:t></w:r><w:fldSimple w:instr=" REF Defs \h "><w:r><w:t>Section 2</w:t></w:r></w:fldSimple><w:r><w:t> and </w:t></w:r><w:r><w:drawing/></w:r><w:r><w:t> now</w:t></w:r></w:p>"#;
        let doc = Document::parse(&make_docx_with_body(body)).expect("parse");
        let view = doc.read();

        // The view still carries exactly two opaque anchors (field + drawing):
        // the anchor inventory is unchanged — only how each *reads* differs.
        let opaque_count = view.blocks[0]
            .segments
            .iter()
            .filter(|s| matches!(s, SegmentView::Opaque { .. }))
            .count();
        assert_eq!(
            opaque_count, 2,
            "fldSimple + drawing are two opaque anchors"
        );

        let text = to_plain_text(&view);
        // The field reads as its cached result; the drawing reads as one U+FFFC.
        assert_eq!(text, "See Section 2 and \u{FFFC} now");
        assert_eq!(
            text.matches('\u{FFFC}').count(),
            1,
            "only the no-text object (drawing) contributes a U+FFFC; the field reads as text"
        );
    }

    #[test]
    fn hard_break_terminates_the_span_but_reads_as_no_character_in_plain_text() {
        // Domain rule (domain-model §4): the view is a COMPLETE list of spans —
        // every text run, every opaque anchor, every hard break. "A"<w:br/>"B"
        // is TWO lines, not one merged span "AB": a hard break must terminate
        // the pending text run and occupy a span position of its own, so an
        // `expect` match against the right line targets it precisely, not a
        // merged "AB" no `expect` could ever equal.
        //
        // The FLAT plain-text string, though, stays silent about the break: a
        // `<w:br/>` is layout-only per ECMA-376 §17.3.3.1/§17.18.3/§17.18.4 —
        // Word reads "A"<w:br/>"B" as the two characters "AB", not "A B" or
        // "A\nB" (`spec_runcontent_breaks_tabs_word_compliance` pins this).
        // The break is still fully honest in the STRUCTURED view (its own span,
        // its own id) and in html/markdown (a real line separation); only the
        // single-string reading matches Word's own silence about it.
        let body = r#"<w:p><w:r><w:t>A</w:t><w:br/><w:t>B</w:t></w:r></w:p>"#;
        let doc = Document::parse(&make_docx_with_body(body)).expect("parse");
        let view = doc.read();
        let block = &view.blocks[0];

        assert_eq!(
            block.segments.len(),
            3,
            "text A, the break, text B are three distinct spans, not one merged \"AB\": {:?}",
            block.segments
        );
        match &block.segments[0] {
            SegmentView::Text { text, handle, .. } => {
                assert_eq!(text, "A", "the run before the break stops AT the break");
                assert_eq!(handle.as_ref().map(|h| h.0.as_str()), Some("s_0"));
            }
            other => panic!("expected Text \"A\", got {other:?}"),
        }
        match &block.segments[1] {
            SegmentView::Opaque {
                kind, text, handle, ..
            } => {
                assert_eq!(*kind, OpaqueAnchorKind::HardBreak);
                assert_eq!(*text, None, "a break carries no visible label");
                assert_eq!(
                    handle.as_ref().map(|h| h.0.as_str()),
                    Some("s_1"),
                    "the break occupies its own span ordinal, between the two text runs"
                );
            }
            other => panic!("expected an Opaque HardBreak span, got {other:?}"),
        }
        match &block.segments[2] {
            SegmentView::Text { text, handle, .. } => {
                assert_eq!(text, "B", "the run after the break starts fresh, AFTER it");
                assert_eq!(handle.as_ref().map(|h| h.0.as_str()), Some("s_2"));
            }
            other => panic!("expected Text \"B\", got {other:?}"),
        }

        // The human-readable surface matches Word: no character for the break.
        // (Distinct from the pre-fix BUG, which merged the spans into one
        // string "AB" via a coalesced two-sided span — this is the SAME string
        // for a different, honest reason: two separate spans, zero-width break.)
        assert_eq!(to_plain_text(&view), "AB");
    }

    #[test]
    fn hard_break_renders_as_br_in_html_and_markdown() {
        let body = r#"<w:p><w:r><w:t>A</w:t><w:br/><w:t>B</w:t></w:r></w:p>"#;
        let doc = Document::parse(&make_docx_with_body(body)).expect("parse");
        let view = doc.read();

        let html = crate::html::to_html(&view);
        assert!(
            html.contains("data-kind=\"hard_break\""),
            "the break is an addressable, labeled anchor: {html}"
        );
        assert!(
            html.contains("<br/>"),
            "the break actually separates the line for an HTML consumer: {html}"
        );

        let markdown = crate::extended_markdown::to_extended_markdown(&view);
        assert!(
            markdown.contains("<br id="),
            "the break is represented in the comprehension surface: {markdown}"
        );
    }

    #[test]
    fn native_comment_markers_surface_with_parity_to_imported_reference() {
        // Domain rule: the engine's OWN comment-authoring verb (`CommentCreate`)
        // splices `commentRangeStart` / `commentRangeEnd` / `commentReference`
        // as IR-native markers (never as an `OpaqueInline`) — so before this
        // fix, an agent that adds a comment and re-reads the block sees NO
        // trace of it. The fix surfaces all three with the SAME shape
        // (`OpaqueAnchorKind::Comment` + `OpaqueMetadata::NoteReference`) an
        // IMPORTED `commentReference` already projects to (see
        // `OpaqueAnchorKind::from(&OpaqueKind::CommentReference(..))` and
        // `opaque_meta::project`'s `NoteReference` arm) — true parity, not a
        // second, divergent representation.
        let docx = make_test_docx(&["The scope of work shall be defined in Exhibit A."]);
        let doc = Document::parse(&docx).expect("parse");
        let block_id = doc.read().blocks[0].id.to_string();

        let txn = EditTransaction {
            steps: vec![EditStep::CommentCreate {
                block_id: NodeId::from(block_id.as_str()),
                expect: "scope of work".to_string(),
                semantic_hash: None,
                body: "Please clarify.".to_string(),
                author: Some("Reviewer".to_string()),
                rationale: None,
            }],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: RevisionInfo {
                revision_id: 1,
                identity: 0,
                author: Some("Reviewer".to_string()),
                date: Some("2026-06-01T00:00:00Z".to_string()),
                apply_op_id: None,
            },
        };
        let edited = doc.apply(&txn).expect("CommentCreate applies");
        let view = edited.read();
        let block = &view.blocks[0];

        let comment_id = edited
            .snapshot()
            .canonical
            .comments
            .first()
            .expect("CommentCreate pushed a CommentStory")
            .id
            .clone();

        let opaques: Vec<&SegmentView> = block
            .segments
            .iter()
            .filter(|s| matches!(s, SegmentView::Opaque { .. }))
            .collect();
        assert_eq!(
            opaques.len(),
            3,
            "range start + range end + reference are three anchors, not zero: {:?}",
            block.segments
        );

        let kinds_and_ids: Vec<(OpaqueAnchorKind, Option<String>)> = opaques
            .iter()
            .map(|s| match s {
                SegmentView::Opaque { kind, metadata, .. } => (
                    *kind,
                    match metadata {
                        Some(OpaqueMetadata::NoteReference { reference_id }) => {
                            Some(reference_id.clone())
                        }
                        _ => None,
                    },
                ),
                _ => unreachable!(),
            })
            .collect();

        assert_eq!(
            kinds_and_ids,
            vec![
                (
                    OpaqueAnchorKind::CommentRangeStart,
                    Some(comment_id.clone())
                ),
                (OpaqueAnchorKind::CommentRangeEnd, Some(comment_id.clone())),
                // Parity: the SAME kind + metadata shape an imported
                // `commentReference` already projects to.
                (OpaqueAnchorKind::Comment, Some(comment_id.clone())),
            ],
            "range start, range end, reference — in that order, all carrying the comment id"
        );

        // The read text now shows something marks the comment (never invented
        // before the fix) — but only the reference glyph, exactly one U+FFFC:
        // the range boundaries are genuinely zero-width in Word (ECMA-376
        // §17.13.2), matching `spec_comments_bookmarks_permissions_word_
        // compliance.rs`'s `*_zero_width` gates.
        let text = to_plain_text(&view);
        assert_eq!(
            text.matches('\u{FFFC}').count(),
            1,
            "only the reference marker is visible; the range boundaries are zero-width: {text:?}"
        );

        let markdown = crate::extended_markdown::to_extended_markdown(&view);
        assert!(
            markdown.contains(&format!("<comment_start id={comment_id}/>")),
            "{markdown}"
        );
        assert!(
            markdown.contains(&format!("<comment_end id={comment_id}/>")),
            "{markdown}"
        );
        assert!(
            markdown.contains(&format!("<comment id={comment_id}/>")),
            "the reference marker uses the SAME tag an imported comment does: {markdown}"
        );
    }

    // ─── Structural index + windowing ────────────────────────────────────────

    /// A view with `n` plain paragraphs whose text and id we control, plus a
    /// heading at `heading_at` of `heading_level`, for exercising the index.
    fn synth_view() -> DocumentView {
        DocumentView {
            blocks: vec![
                BlockView {
                    id: NodeId::from("p_0"),
                    role: BlockRole::Paragraph,
                    style_id: None,
                    role_token: None,
                    list: None,
                    cells: Vec::new(),
                    table: None,
                    text: "Preamble before any heading.".to_string(),
                    block_status: TrackStatus::Normal,
                    paragraph_mark_status: TrackStatus::Normal,
                    guard: String::new(),
                    literal_prefix: None,
                    segments: vec![SegmentView::Text {
                        text: "Preamble before any heading.".to_string(),
                        status: TrackStatus::Normal,
                        marks: vec![],
                        handle: None,
                    }],
                    opaque_label: None,
                },
                BlockView {
                    id: NodeId::from("h_1"),
                    role: BlockRole::Heading { level: 1 },
                    style_id: Some("Heading1".to_string()),
                    role_token: None,
                    list: None,
                    cells: Vec::new(),
                    table: None,
                    text: "Article One".to_string(),
                    block_status: TrackStatus::Normal,
                    paragraph_mark_status: TrackStatus::Normal,
                    guard: String::new(),
                    literal_prefix: None,
                    segments: vec![SegmentView::Text {
                        text: "Article One".to_string(),
                        status: TrackStatus::Normal,
                        marks: vec![],
                        handle: None,
                    }],
                    opaque_label: None,
                },
                BlockView {
                    id: NodeId::from("p_2"),
                    role: BlockRole::Paragraph,
                    style_id: None,
                    role_token: None,
                    list: None,
                    cells: Vec::new(),
                    table: None,
                    text: "Body under article one.".to_string(),
                    block_status: TrackStatus::Normal,
                    paragraph_mark_status: TrackStatus::Normal,
                    guard: String::new(),
                    literal_prefix: None,
                    segments: vec![SegmentView::Text {
                        text: "Body under article one.".to_string(),
                        status: TrackStatus::Normal,
                        marks: vec![],
                        handle: None,
                    }],
                    opaque_label: None,
                },
                BlockView {
                    id: NodeId::from("h_3"),
                    role: BlockRole::Heading { level: 2 },
                    style_id: None,
                    role_token: None,
                    list: None,
                    cells: Vec::new(),
                    table: None,
                    text: "Section 1.1".to_string(),
                    block_status: TrackStatus::Normal,
                    paragraph_mark_status: TrackStatus::Normal,
                    guard: String::new(),
                    literal_prefix: None,
                    segments: vec![SegmentView::Text {
                        text: "Section 1.1".to_string(),
                        status: TrackStatus::Normal,
                        marks: vec![],
                        handle: None,
                    }],
                    opaque_label: None,
                },
            ],
        }
    }

    #[test]
    fn outline_is_a_one_to_one_shadow_of_the_blocks() {
        // Faithfulness invariant (named in build_outline's doc): entries.len()
        // == blocks.len(), and for every i, entries[i].id == blocks[i].id and
        // entries[i].index == i. The index never reorders or drops a block.
        let view = synth_view();
        let outline = build_outline(&view);
        assert_eq!(outline.entries.len(), view.blocks.len());
        assert_eq!(outline.total_blocks, view.blocks.len());
        for (i, (entry, block)) in outline.entries.iter().zip(&view.blocks).enumerate() {
            assert_eq!(entry.index, i, "index is the document-order position");
            assert_eq!(entry.id, block.id, "entry id shadows block id");
            assert_eq!(entry.role, block.role, "entry role shadows block role");
        }
    }

    #[test]
    fn outline_depth_tracks_nearest_preceding_heading() {
        // Domain rule: depth is the running nearest-preceding-heading level. A
        // Heading takes its own level; body before any heading is depth 0; body
        // after a heading inherits that heading's level.
        let view = synth_view();
        let outline = build_outline(&view);
        let depths: Vec<u8> = outline.entries.iter().map(|e| e.depth).collect();
        // p_0 (body, no heading yet) = 0; h_1 = 1; p_2 (under h_1) = 1; h_3 = 2.
        assert_eq!(depths, vec![0, 1, 1, 2]);
    }

    #[test]
    fn outline_char_and_byte_len_are_faithful() {
        // Domain rule: char_len == text.chars().count() (scalar values),
        // byte_len == text.len() (UTF-8 bytes), and total_chars is their sum.
        // Use a block with a multi-byte char so char_len != byte_len.
        let view = DocumentView {
            blocks: vec![BlockView {
                id: NodeId::from("p_0"),
                role: BlockRole::Paragraph,
                style_id: None,
                role_token: None,
                list: None,
                cells: Vec::new(),
                table: None,
                text: "café €".to_string(), // 6 chars, é=2 bytes, €=3 bytes
                block_status: TrackStatus::Normal,
                paragraph_mark_status: TrackStatus::Normal,
                guard: String::new(),
                literal_prefix: None,
                segments: vec![],
                opaque_label: None,
            }],
        };
        let outline = build_outline(&view);
        let e = &outline.entries[0];
        assert_eq!(e.char_len, "café €".chars().count());
        assert_eq!(e.char_len, 6);
        assert_eq!(e.byte_len, "café €".len());
        assert!(
            e.byte_len > e.char_len,
            "multi-byte text: bytes exceed chars"
        );
        assert_eq!(outline.total_chars, e.char_len);
    }

    #[test]
    fn outline_preview_is_a_char_boundary_prefix_of_exactly_120_chars() {
        // Domain rule: the preview is the first OUTLINE_PREVIEW_CHARS (=120)
        // characters of the block text, cut on a char boundary — never longer,
        // never splitting a codepoint. Use a 4-byte emoji at position 120 so a
        // byte-based cut would split it; the char-based cut must not.
        assert_eq!(
            OUTLINE_PREVIEW_CHARS, 120,
            "preview length is a fixed constant"
        );
        let mut text = "x".repeat(OUTLINE_PREVIEW_CHARS);
        text.push('😀'); // the 121st char, multi-byte
        text.push_str("tail");
        let view = DocumentView {
            blocks: vec![BlockView {
                id: NodeId::from("p_0"),
                role: BlockRole::Paragraph,
                style_id: None,
                role_token: None,
                list: None,
                cells: Vec::new(),
                table: None,
                text: text.clone(),
                block_status: TrackStatus::Normal,
                paragraph_mark_status: TrackStatus::Normal,
                guard: String::new(),
                literal_prefix: None,
                segments: vec![],
                opaque_label: None,
            }],
        };
        let outline = build_outline(&view);
        let preview = &outline.entries[0].text_preview;
        assert_eq!(
            preview.chars().count(),
            OUTLINE_PREVIEW_CHARS,
            "exactly 120 chars"
        );
        assert_eq!(
            *preview,
            "x".repeat(OUTLINE_PREVIEW_CHARS),
            "prefix, char-cut"
        );
        assert!(
            text.starts_with(preview.as_str()),
            "preview is a real prefix"
        );
        // A shorter-than-limit text previews in full.
        let short = DocumentView {
            blocks: vec![BlockView {
                id: NodeId::from("p_1"),
                role: BlockRole::Paragraph,
                style_id: None,
                role_token: None,
                list: None,
                cells: Vec::new(),
                table: None,
                text: "short".to_string(),
                block_status: TrackStatus::Normal,
                paragraph_mark_status: TrackStatus::Normal,
                guard: String::new(),
                literal_prefix: None,
                segments: vec![],
                opaque_label: None,
            }],
        };
        assert_eq!(build_outline(&short).entries[0].text_preview, "short");
    }

    #[test]
    fn block_range_resolves_inclusive_window() {
        let view = synth_view();
        let slice = block_range(&view, "h_1", "p_2").expect("valid window");
        assert_eq!(slice.len(), 2);
        assert_eq!(slice[0].id.to_string(), "h_1");
        assert_eq!(slice[1].id.to_string(), "p_2");
        // A single-block window (from == to) is the one-element slice.
        let one = block_range(&view, "p_0", "p_0").expect("single-block window");
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].id.to_string(), "p_0");
    }

    #[test]
    fn block_range_fails_loud_on_unknown_and_out_of_order() {
        // Fail-loud (CLAUDE.md): an unknown id is AnchorNotFound carrying the
        // id; an out-of-order pair is OutOfOrder, never silently swapped.
        let view = synth_view();
        assert_eq!(
            block_range(&view, "nope", "p_2").err(),
            Some(WindowError::AnchorNotFound("nope".to_string()))
        );
        assert_eq!(
            block_range(&view, "p_0", "nope").err(),
            Some(WindowError::AnchorNotFound("nope".to_string()))
        );
        match block_range(&view, "p_2", "h_1") {
            Err(WindowError::OutOfOrder { from, to }) => {
                assert!(from > to, "from must be after to");
            }
            other => panic!("expected OutOfOrder, got {other:?}"),
        }
    }

    #[test]
    fn to_plain_text_round_trips_through_the_view() {
        // to_plain_text is a pure function of the DocumentView: rebuilding the
        // view and re-rendering yields the identical string (no hidden state).
        let docx = make_test_docx(&["Alpha", "Beta", "Gamma"]);
        let doc = Document::parse(&docx).expect("parse");
        let once = to_plain_text(&doc.read());
        let twice = to_plain_text(&doc.read());
        assert_eq!(once, twice);
        assert_eq!(once, "Alpha\n\nBeta\n\nGamma");
    }

    // ─── Opaque metadata read-surfacing (M1) ─────────────────────────────────

    /// A paragraph body carrying one inline `w:sdt` with the given tag/value.
    fn sdt_body(tag: &str, value: &str) -> String {
        format!(
            r#"<w:p><w:r><w:t>Tenant: </w:t></w:r><w:sdt><w:sdtPr><w:alias w:val="Tenant Name"/><w:tag w:val="{tag}"/><w:text/></w:sdtPr><w:sdtContent><w:r><w:t xml:space="preserve">{value}</w:t></w:r></w:sdtContent></w:sdt></w:p>"#
        )
    }

    /// The first `SegmentView::Opaque` of the first block, or panic.
    fn first_opaque(view: &DocumentView) -> &SegmentView {
        view.blocks[0]
            .segments
            .iter()
            .find(|s| matches!(s, SegmentView::Opaque { .. }))
            .expect("an opaque anchor")
    }

    #[test]
    fn segment_view_opaque_carries_metadata() {
        // An injected inline SDT projects ContentControl metadata onto its
        // SegmentView::Opaque, through the real parse → view path.
        let doc = Document::parse(&make_docx_with_body(&sdt_body(
            "TenantName",
            "Acme Corporation",
        )))
        .expect("parse");
        let view = doc.read();
        let SegmentView::Opaque { metadata, .. } = first_opaque(&view) else {
            unreachable!("filtered to Opaque");
        };
        match metadata.as_ref().expect("sdt carries metadata") {
            OpaqueMetadata::ContentControl {
                tag,
                alias,
                control,
                display_text,
                ..
            } => {
                assert_eq!(tag.as_deref(), Some("TenantName"));
                assert_eq!(alias.as_deref(), Some("Tenant Name"));
                assert_eq!(*control, SdtControlKind::PlainText);
                assert_eq!(display_text.as_deref(), Some("Acme Corporation"));
            }
            other => panic!("expected ContentControl, got {other:?}"),
        }
    }

    #[test]
    fn opaque_anchor_kind_sdt_is_content_control_not_other() {
        // §2.4: an SDT no longer collapses to Other — it is its own kind, the
        // discovery primitive an agent filters on.
        let doc = Document::parse(&make_docx_with_body(&sdt_body("X", "v"))).expect("parse");
        let view = doc.read();
        let SegmentView::Opaque { kind, .. } = first_opaque(&view) else {
            unreachable!("filtered to Opaque");
        };
        assert_eq!(*kind, OpaqueAnchorKind::ContentControl);
        // Guard the IR-level mapping directly too.
        assert_eq!(
            OpaqueAnchorKind::from(&crate::domain::OpaqueKind::Sdt),
            OpaqueAnchorKind::ContentControl
        );
    }

    #[test]
    fn metadata_enrichment_is_hash_neutral() {
        // THE load-bearing test (§1.5): the SDT's displayed value lives only in
        // metadata, never in the guard. Two documents whose ONLY difference is
        // the SDT's sdtContent text must produce:
        //   (a) DIFFERENT projected metadata (the read enrichment is real), and
        //   (b) the SAME block guard (the enrichment does not move any hash).
        // If (b) ever fails, a metadata-only read has leaked into the guard —
        // which would silently invalidate write preconditions.
        let doc_a = Document::parse(&make_docx_with_body(&sdt_body("Name", "Acme Corporation")))
            .expect("parse a");
        let doc_b = Document::parse(&make_docx_with_body(&sdt_body("Name", "Globex Limited")))
            .expect("parse b");

        // (a) the projected display_text differs.
        let display = |view: &DocumentView| -> Option<String> {
            let SegmentView::Opaque { metadata, .. } = first_opaque(view) else {
                unreachable!()
            };
            match metadata.as_ref().expect("metadata") {
                OpaqueMetadata::ContentControl { display_text, .. } => display_text.clone(),
                other => panic!("expected ContentControl, got {other:?}"),
            }
        };
        let view_a = doc_a.read();
        let view_b = doc_b.read();
        assert_eq!(display(&view_a).as_deref(), Some("Acme Corporation"));
        assert_eq!(display(&view_b).as_deref(), Some("Globex Limited"));
        assert_ne!(
            display(&view_a),
            display(&view_b),
            "the metadata enrichment must actually surface the differing value"
        );

        // (b) the block guard is identical: the guard hashes the SDT's id + kind
        // name, never its displayed/inner text. Read the guard off the projected
        // BlockView (the same `block_guard` the write path checks).
        assert_eq!(
            view_a.blocks[0].guard, view_b.blocks[0].guard,
            "differing SDT inner text must NOT move the block guard (hash-neutrality)"
        );
        assert!(
            !view_a.blocks[0].guard.is_empty(),
            "the guard is actually computed (a real value, not the empty string)"
        );
    }
}
