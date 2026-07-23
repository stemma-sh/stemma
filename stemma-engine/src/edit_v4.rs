//! LLM Edit Schema v4: typed recursive grammar.
//!
//! This module defines the LLM-facing edit transaction format for schema v4.
//! The schema models the document as a tree of typed nodes, mirroring how the
//! IR, ProseMirror, and OOXML already represent it. The v3 schema
//! (`edit::EditTransactionRequest`) remains in place as a separate wire format
//! until clients migrate.
//!
//! Scope of this module:
//! - **Data types only** (this milestone). Pure serde-derived JSON wire shape
//!   for the v4 grammar. No validation, no engine application, no translation.
//! - Schema validation (parse step) and semantic checks live in companion
//!   functions added in later milestones.
//!
//! Grammar (the day-one node set):
//!
//! ```text
//! Block:
//!   paragraph    { role, content: Inline[], attrs }
//!   table        { role, content: TableRow[], attrs }
//!   toc          { levels? } — insert-only; see `Block::Toc`.
//!
//! TableRow / TableCell exist in the grammar so that a table payload can
//! express row/cell shape. They are NOT addressable day-one (the LLM cannot
//! `replace` a row or cell).
//!
//! Inline:
//!   text         { text, marks: Mark[] }
//!   hyperlink    { attrs: { href, title }, content: Inline[] }
//!   opaque_ref   { attrs: { id } }
//!
//! Marks (flat array on text nodes):
//!   bold, italic, underline, strike, subscript, superscript
//!   inline_role(id)
//! ```
//!
//! Verbs:
//!
//! ```text
//! replace(target, content)              replace one node with another of the same kind
//! insert(target, content)               insert new nodes before/after an anchor
//! delete(target)                        remove a node
//! move(target, destination)             relocate a node, or a contiguous range of nodes
//! set_attr(target, attrs)               update node attributes
//! ```

use serde::Deserialize;

use crate::domain::{
    Alignment, Border, BorderSet, BorderStyle, CellFormatting, CellMargins, FormatSwitches,
    HeaderFooterKind, HeightRule, HighlightColor, Indentation, LineSpacingRule, NodeId,
    PageOrientation, ParagraphBorders, ParagraphSpacing, RefFieldSpec, RefKind, RevisionInfo,
    SdtControl, SdtListItem, SectionType, Shading, ShadingPattern, TableFormatting,
    TableMeasurement, TocLevelsSpec, VerticalAlignment, WidthType,
};
use crate::edit::NoteKind;
use crate::edit::verbs::headers_footers::HeaderFooterLink;
use crate::edit::verbs::numbering::NumberingChange;
use crate::edit::verbs::page_setup::{
    ColumnLayout, PageMargins, PageSetupPatch, PageSize, SectionTarget,
};
use crate::edit::verbs::table_ops::{TableInsertPosition, TableOp};
use crate::edit::{
    BlockSpec, CellFormattingPatch, ContentFragment, DataBinding, EditStep, EditTransaction,
    EquationPlacement, FormFieldValue, ImageCrop, ImageFormat, ImageLayoutPatch, ImagePositionAxis,
    ImageResize, ImageSource, ImageWrapType, InlineMarkSet, InsertListSpec, InsertPosition,
    MaterializationMode, ParagraphBlockSpec, ParagraphContent, ParagraphFormattingPatch,
    ResolvedSpanEndpoint, ResolvedSpanSelector, RowFormattingPatch, RunStyleEdit, SdtSpec,
    SdtValue, StoryRef, StyleDefinition, StyleParaProps, StyleRunProps, StyleType, TableBlockSpec,
    TableCellSpec, TableFormattingPatch, TableRowSpec, TocBlockSpec, VerticalMergeSpec,
};

// ─── Block-level grammar ─────────────────────────────────────────────────────

/// A block-level node in the v4 grammar.
///
/// Day-one addressable kinds are `Paragraph` and `Table`. The kinds the LLM
/// can target with structural verbs (`replace`, `insert`, `delete`, `move`)
/// and the property verb (`set_attr`) are constrained by an op-kind-by-node-kind
/// matrix, enforced in validation.
// The Table variant carries optional formatting attrs (RFC-0003 Item 1); the
// size spread is fine for this transient wire type deserialized once per op.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Block {
    Paragraph {
        #[serde(default)]
        role: Option<String>,
        content: Vec<Inline>,
        #[serde(default)]
        attrs: Option<ParagraphAttrs>,
        /// Optional list membership for the inserted/replaced paragraph: attach
        /// the paragraph's `w:numPr` (numId + ilvl, §17.3.1.19) from the start,
        /// so an agent can author a list sub-point as a single tracked insert
        /// (no post-insert `set_numbering` needed). `num_id` MUST reference a
        /// numbering definition the document already uses — the engine resolves
        /// the level against existing list paragraphs and fails loud
        /// (`InsertListNumIdUnknown`) if no paragraph references that numId. It
        /// never fabricates a numbering definition.
        #[serde(default)]
        list: Option<ListSpecWire>,
    },
    Table {
        #[serde(default)]
        role: Option<String>,
        content: Vec<TableRow>,
        #[serde(default)]
        attrs: Option<TableAttrs>,
    },
    /// A native table-of-contents field, e.g. `{"type":"toc"}` or
    /// `{"type":"toc","levels":{"from":1,"to":2}}`. Insert-only (day one):
    /// `translate_replace`/`translate_span_replace` refuse it via
    /// `SchemaError::TocNotReplaceable` before translation, and it may not
    /// appear inside a table cell (`SchemaError::TocNotAllowedInTableCell`).
    ///
    /// `levels` is optional; when omitted the engine uses the documented
    /// product default 1–3 (Word's own "Automatic Table of Contents" range).
    /// No `role` field: unlike `Paragraph`/`Table`, a ToC insert never asks
    /// the caller for an internal role token — the engine always resolves
    /// the surrounding paragraph against the document's default body role
    /// (see `resolve_toc_spec`'s `"default"` alias fallback).
    Toc {
        #[serde(default)]
        levels: Option<TocLevelsWire>,
    },
}

/// Wire shape for `Block::Toc.levels`: the `\o "from-to"` heading-level range
/// (§17.16.5.68). Schema-validated at the wire edge (`validate_block`) to
/// `1 <= from <= to <= 9` — OOXML/Word support nine heading levels; an
/// out-of-range or inverted pair is refused, never clamped.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TocLevelsWire {
    pub from: u8,
    pub to: u8,
}

/// A table row. Grammar-only: not addressable day-one.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TableRow {
    pub content: Vec<TableCell>,
    #[serde(default)]
    pub attrs: Option<TableRowAttrs>,
}

/// A table cell. Grammar-only: not addressable day-one. Cell content is a
/// `Vec<Block>`, which proves the recursion shape — a cell can contain
/// paragraphs and nested tables.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TableCell {
    pub content: Vec<Block>,
    #[serde(default)]
    pub attrs: Option<TableCellAttrs>,
}

// ─── Inline-level grammar ────────────────────────────────────────────────────

/// An inline node inside a paragraph or hyperlink.
///
/// `OpaqueRef` is a pointer to an existing opaque inline node (footnote,
/// field, image, etc.). The LLM never authors opaque content; it references
/// an existing node by id. Per invariant 2 (opaque set-equality), the set of
/// opaque ids in a `replace` payload must equal the set in the target.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Inline {
    Text {
        text: String,
        #[serde(default)]
        marks: Vec<Mark>,
    },
    Hyperlink {
        attrs: HyperlinkAttrs,
        content: Vec<Inline>,
    },
    OpaqueRef {
        attrs: OpaqueRefAttrs,
    },
}

/// Inline marks on text nodes. A flat array (not wrapper nodes) keeps the
/// wire shape terse.
///
/// `inline_role` carries a vocabulary id (e.g. `defined_term`) that the engine
/// resolves against the document's role catalogue. The plain marks
/// (bold/italic/underline/strike/subscript/superscript) map to the universal
/// marks table applicable to any text span.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Mark {
    Bold,
    Italic,
    Underline,
    Strike,
    Subscript,
    Superscript,
    InlineRole { id: String },
}

// ─── Attrs (per-kind) ────────────────────────────────────────────────────────
//
// Each kind has its own attrs struct so a `set_attr` payload's allowed fields
// depend on the target kind. The semantic-check layer (M4+) validates that
// the attrs the caller supplied are legal for the resolved target kind.

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParagraphAttrs {
    /// Numbering restart. Currently the only paragraph-level attribute the
    /// engine routes through `set_attr` is `role`, which sits on the block
    /// directly. `restart_numbering` lives here to track the v3 field.
    #[serde(default)]
    pub restart_numbering: bool,
}

/// Wire shape for an inserted/replaced paragraph's list membership
/// (`Block::Paragraph.list`). Carries the two structural numbering coordinates
/// the live `w:numPr` needs: the list instance (`numId`) and the indent level
/// (`ilvl`, 0..=8, §17.9.3). The engine resolves the rest (the displayed label)
/// the same way Word does — from `word/numbering.xml` at render time — so the
/// caller does NOT supply `synthesized_text`/`is_bullet` here (unlike
/// `set_numbering`'s `SetList`): an inserted paragraph's live numPr only carries
/// numId/ilvl, and those derived fields are non-serialized diff hints.
///
/// `num_id` must be a numId the document ALREADY uses (read it from a sibling
/// list item via `read_block`/`read_outline`'s `list.num_id`). The engine never
/// mints a new numbering definition; an unknown numId fails loud.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListSpecWire {
    pub num_id: u32,
    pub ilvl: u32,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TableAttrs {
    /// Table style reference (`w:tblStyle w:val`, §17.4.63). RFC-0003 Item 1.
    #[serde(default)]
    pub style: Option<String>,
    /// Table borders (`w:tblBorders`, §17.4.38).
    #[serde(default)]
    pub borders: Option<BorderSetPatch>,
    /// Preferred table width (`w:tblW`, §17.4.64).
    #[serde(default)]
    pub width: Option<MeasurementPatch>,
    /// Default cell margins (`w:tblCellMar`, §17.4.43), in twips.
    #[serde(default)]
    pub cell_margins: Option<CellMarginsPatch>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TableRowAttrs {
    /// Repeated header row (`w:tblHeader`, §17.4.49). Maps to
    /// `TableRowSpec.is_header`.
    #[serde(default)]
    pub header: bool,
    /// Row height in twips (`w:trHeight w:val`, §17.4.81). RFC-0003 Item 1.
    #[serde(default)]
    pub height: Option<u32>,
    /// Row height rule (`w:trHeight w:hRule`, §17.18.37): `exact`|`atLeast`|`auto`.
    #[serde(default)]
    pub height_rule: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TableCellAttrs {
    /// Horizontal merge: number of grid columns this cell spans (`w:gridSpan`,
    /// §17.4.17). Absent or `1` is a single-column cell. Maps to
    /// `TableCellSpec.merge_h`.
    #[serde(default)]
    pub grid_span: Option<u32>,
    /// Vertical merge (`w:vMerge`, §17.4.84): `"restart"` (anchor) or
    /// `"continue"` (continuation). Maps to `TableCellSpec.merge_v`.
    #[serde(default)]
    pub v_merge: Option<VMergeWire>,
    /// Cell borders (`w:tcBorders`). RFC-0003 Item 1.
    #[serde(default)]
    pub borders: Option<BorderSetPatch>,
    /// Cell shading (`w:shd`, §17.4.33).
    #[serde(default)]
    pub shading: Option<ShadingWire>,
    /// Cell width (`w:tcW`, §17.4.72).
    #[serde(default)]
    pub width: Option<MeasurementPatch>,
    /// Vertical alignment (`w:vAlign`, §17.4.84): `top`|`center`|`bottom`.
    #[serde(default)]
    pub v_align: Option<String>,
    /// Per-cell margins (`w:tcMar`, §17.4.41), in twips.
    #[serde(default)]
    pub margins: Option<CellMarginsPatch>,
}

/// Wire form of vertical-merge state. Parsed at the edge into the typed
/// `VerticalMergeSpec` by `convert` — unknown strings are rejected by serde
/// (no silent fallback).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VMergeWire {
    Restart,
    Continue,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HyperlinkAttrs {
    /// External URL. Mutually-non-exclusive with `anchor`: a hyperlink may
    /// carry both an `href` and an internal bookmark anchor in OOXML
    /// (`w:hyperlink/@w:anchor`). At least one of `href`/`anchor` must be
    /// supplied; enforced at the schema-check layer.
    #[serde(default)]
    pub href: Option<String>,
    /// Internal bookmark anchor.
    #[serde(default)]
    pub anchor: Option<String>,
    /// Tooltip (`w:hyperlink/@w:tooltip`).
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpaqueRefAttrs {
    /// Stable id of the opaque inline node in the target paragraph.
    pub id: NodeId,
}

// ─── Sub-block span addressing (Phase 3) ─────────────────────────────────────

/// Wire form of a sub-block span selector on `replace` — sub-block addressing,
/// a span/range inside a block.
///
/// Deserializes from any of:
/// - `"whole"` — the whole block (back-compat; identical to no `span`);
/// - `"s_3"` — a span **handle** from a fresh detail read;
/// - `{ "after": "x_12" }` / `{ "before": "x_12" }` — an empty range adjacent to
///   an opaque anchor (an insertion point), by anchor id;
/// - `{ "between": [<endpoint>, <endpoint>] }` — a range delimited by two
///   endpoints, each `"start"`, `"end"`, or an anchor id string.
///
/// Anchors are ALWAYS referenced by their durable opaque id, never by substring.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum SpanSelector {
    /// A bare string: either the literal `"whole"`, or a span handle (`s_<n>`).
    Token(String),
    /// `{ "after": <anchor_id> }`.
    After { after: NodeId },
    /// `{ "before": <anchor_id> }`.
    Before { before: NodeId },
    /// `{ "between": [<endpoint>, <endpoint>] }`.
    Between { between: [SpanEndpoint; 2] },
}

/// One endpoint of a `between` span selector. A bare string that is `"start"` or
/// `"end"` selects a block boundary; any other string is an opaque anchor id.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum SpanEndpoint {
    /// `"start"` / `"end"` / an anchor id, disambiguated at translation time.
    Token(String),
}

// ─── Op envelope ─────────────────────────────────────────────────────────────

/// A single v4 op.
///
/// `replace`, `insert`, `delete`, `move` are the four structural verbs.
/// `set_attr` is the property verb (separate because OOXML emits property
/// changes through a different element class: `pPrChange`/`rPrChange` vs
/// `w:ins`/`w:del`/`w:moveFrom`/`w:moveTo`).
///
/// `expect` (substring precondition) is preserved on `replace` and `delete`
/// for the same reason it exists in v3: an audit trail that fails loudly if
/// the LLM addressed a stale snapshot of the document.
/// Payload of a `replace` op. The wire format uses the inner `type` tag to
/// discriminate. Addressable kinds for `replace` are
/// paragraph, table, and hyperlink. Schema validation rejects text and
/// opaque_ref payloads (they are not addressable for `replace`).
#[allow(clippy::large_enum_variant)] // Block carries optional table formatting (RFC-0003 Item 1).
#[derive(Clone, Debug)]
pub enum ReplaceContent {
    Block(Block),
    Inline(Inline),
}

/// `ReplaceContent` decodes by the inner `"type"` discriminator, NOT serde's
/// `untagged` fallback. Untagged decoding tries `Block` then `Inline` and, on a
/// malformed payload, reports only "data did not match any variant of untagged
/// enum ReplaceContent" — a black box that names neither the bad field nor the
/// valid shapes. This hand-written
/// decode reads `type` first and dispatches to the matching arm, so a wrong or
/// missing `type` yields a field-level, actionable message listing the valid
/// kinds, and a structurally-wrong payload of a KNOWN kind surfaces that kind's
/// own decode error (e.g. a paragraph missing `content`) rather than the
/// catch-all. The accepted wire shapes are unchanged: `Block` is
/// `type ∈ {paragraph, table}`, `Inline` is `type ∈ {text, hyperlink,
/// opaque_ref}` — both already `#[serde(tag = "type")]`.
impl<'de> Deserialize<'de> for ReplaceContent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error as _;
        // Buffer the payload so we can peek at `type` and then re-decode the
        // whole object into the chosen variant (serde_json::Value preserves it).
        let value = serde_json::Value::deserialize(deserializer)?;
        let type_tag = value.get("type").and_then(serde_json::Value::as_str);
        match type_tag {
            Some("paragraph") | Some("table") => serde_json::from_value::<Block>(value)
                .map(ReplaceContent::Block)
                .map_err(D::Error::custom),
            Some("text") | Some("hyperlink") | Some("opaque_ref") => {
                serde_json::from_value::<Inline>(value)
                    .map(ReplaceContent::Inline)
                    .map_err(D::Error::custom)
            }
            // `toc` IS a recognized content kind — just not a replaceable one
            // (day-one scope: insert-only, `SchemaError::TocNotReplaceable`).
            // Named separately from the `Some(other)` catch-all below so the
            // error says why, not just "unrecognized type".
            Some("toc") => Err(D::Error::custom(
                "replace content.type \"toc\": a toc block can only be inserted \
                 (op: \"insert\"), not replaced",
            )),
            Some(other) => Err(D::Error::custom(format!(
                "replace `content.type` must be one of \
                 {{paragraph, table, text, hyperlink, opaque_ref}}, got {other:?}"
            ))),
            None => Err(D::Error::custom(
                "replace `content` is missing the `type` field; it must be one of \
                 {paragraph, table, text, hyperlink, opaque_ref}",
            )),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    Replace {
        target: NodeId,
        content: ReplaceContent,
        /// Optional sub-block span selector (Phase 3). `None` or `"whole"` is
        /// byte-identical to the pre-Phase-3 whole-block replace. A handle or an
        /// anchor-relative selector targets a sub-range of the paragraph
        /// (applied as the status-preserving splice — existing tracked changes
        /// outside the range are carried through untouched). A span on a
        /// non-paragraph target is rejected (`SpanOnNonParagraph`), and a span
        /// op without a `guard` is rejected (`SpanRequiresGuard`).
        #[serde(default)]
        span: Option<SpanSelector>,
        /// Precondition text. On a WHOLE-block replace this is the advisory
        /// human-readable substring (back-compat; authoritative only when no
        /// guard is supplied — see `validate_replace_step`). On a SPAN replace
        /// it is the resolved range's exact visible text, re-asserted at
        /// resolution (the text-identity check; mismatch => stale_edit).
        #[serde(default)]
        expect: Option<String>,
        /// The block staleness guard (block semantic hash). `guard` is the
        /// spec-named alias for `semantic_hash` — supply EITHER. If both are
        /// present and disagree, the op is rejected (`ConflictingGuard`): we
        /// never silently pick one.
        #[serde(default)]
        guard: Option<String>,
        #[serde(default)]
        semantic_hash: Option<String>,
        #[serde(default)]
        rationale: Option<String>,
    },
    Insert {
        target: AnchorTarget,
        content: Vec<Block>,
        #[serde(default)]
        rationale: Option<String>,
    },
    Delete {
        target: NodeId,
        #[serde(default)]
        expect: Option<String>,
        /// Block staleness guard; spec-named alias for `semantic_hash` (supply
        /// either; both-and-disagree => `ConflictingGuard`).
        #[serde(default)]
        guard: Option<String>,
        #[serde(default)]
        semantic_hash: Option<String>,
        #[serde(default)]
        rationale: Option<String>,
    },
    #[serde(rename = "move")]
    Move {
        /// A single block (`"<block_id>"`) or a contiguous inclusive range
        /// (`{"from":"<block_id>","to":"<block_id>"}`, either doc order —
        /// the engine reorders, never refuses). See [`MoveTarget`].
        target: MoveTarget,
        destination: AnchorTarget,
        /// Optional precondition, checked against the FROM block — same
        /// placement as `delete`'s `expect`, but optional (see
        /// `EditStep::MoveBlockRange.expect`).
        #[serde(default)]
        expect: Option<String>,
        /// Block staleness guard; spec-named alias for `semantic_hash` (supply
        /// either; both-and-disagree => `ConflictingGuard`).
        #[serde(default)]
        guard: Option<String>,
        #[serde(default)]
        semantic_hash: Option<String>,
        #[serde(default)]
        rationale: Option<String>,
    },
    SetAttr {
        target: NodeId,
        attrs: AttrPatch,
        /// Optimistic-concurrency precondition on the hyperlink's current
        /// `href`. Required by the adapter whenever `attrs.href` is set
        /// (so a stale caller cannot silently retarget the wrong URL).
        /// Ignored for paragraph-role mutations.
        #[serde(default)]
        expect_href: Option<String>,
        /// Optimistic-concurrency precondition on the hyperlink's current
        /// `anchor`. Required by the adapter whenever `attrs.anchor` is
        /// set. Ignored for paragraph-role mutations.
        #[serde(default)]
        expect_anchor: Option<String>,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Apply run-level formatting to the text matched by `expect`, as a tracked
    /// `w:rPrChange`. `marks` are the universal marks to turn on; `inline_role`
    /// is not a formatting toggle and is rejected here.
    SetFormat {
        target: NodeId,
        expect: String,
        #[serde(default)]
        semantic_hash: Option<String>,
        marks: Vec<Mark>,
        /// Literal text color: 6-hex-digit RGB (e.g. `"FF0000"`) or `"auto"`.
        #[serde(default)]
        color: Option<String>,
        /// Highlight color name per §17.18.40 `ST_HighlightColor`.
        #[serde(default)]
        highlight: Option<String>,
        /// Font family for the ascii/hAnsi slot.
        #[serde(default)]
        font_family: Option<String>,
        /// Font size in half-points (e.g. 24 = 12pt).
        #[serde(default)]
        font_size_half_points: Option<u32>,
        /// Turn on all-caps display (`w:caps`, §17.3.2.5). A `StyleProps`
        /// tri-state, not a universal `Mark`, so it rides here, not in `marks`.
        #[serde(default)]
        caps: bool,
        /// Turn on small-caps display (`w:smallCaps`, §17.3.2.33).
        #[serde(default)]
        small_caps: bool,
        /// Character spacing in twips (`w:spacing` @w:val, §17.3.2.35).
        /// Positive expands, negative condenses; `0` resets to default tracking.
        #[serde(default)]
        char_spacing: Option<i32>,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Set paragraph-level formatting (alignment / indentation / spacing) on a
    /// single paragraph in place, as a tracked `w:pPrChange`. Unlike `set_attr`
    /// (which swaps the paragraph role), this sets only the named attributes and
    /// leaves the role unchanged. At least one of `align` / `indent` / `spacing`
    /// must be present.
    SetParaFormat {
        target: NodeId,
        #[serde(default)]
        semantic_hash: Option<String>,
        /// Alignment token: `left` | `center` | `right` | `both` | `distribute`.
        /// An unknown token is rejected at the wire edge (no silent `Other`).
        #[serde(default)]
        align: Option<String>,
        /// Indentation in twips. Any present sub-field sets the indent.
        #[serde(default)]
        indent: Option<IndentPatch>,
        /// Spacing in twips / line units. Any present sub-field sets the spacing.
        #[serde(default)]
        spacing: Option<SpacingPatch>,
        /// Paragraph borders (`w:pBdr`, §17.3.1.24). Any present edge sets the
        /// border set. A border edge with an unknown `style` is rejected at the
        /// wire edge (no silent fallback).
        #[serde(default)]
        borders: Option<ParaBordersPatch>,
        /// Paragraph shading (`w:shd`, §17.3.1.31). An unknown `pattern` token
        /// is rejected at the wire edge.
        #[serde(default)]
        shading: Option<ShadingWire>,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Set cell-level formatting (borders / shading / width / vertical alignment
    /// / margins) on ONE table cell **in place**, as a tracked `w:tcPrChange`
    /// (§17.13.5.37). The cell is addressed by LOGICAL grid position
    /// `{row_index, col_index}` (after `gridBefore`, advancing by each cell's
    /// `gridSpan`) — the same address the read view mints. Like `set_cell_text`,
    /// this is an in-place property edit: it byte-preserves `tblPr`, every
    /// `trPr`, and all other cells, so it bypasses the whole-table v4 replace
    /// schema (and its formatting refusal) entirely. At least one of the
    /// property fields must be present.
    SetCellFormat {
        target: NodeId,
        #[serde(default)]
        semantic_hash: Option<String>,
        /// Zero-based row index into the table.
        row_index: usize,
        /// Zero-based LOGICAL column index (start column of the target cell).
        col_index: usize,
        /// Cell borders (`w:tcBorders`).
        #[serde(default)]
        borders: Option<BorderSetPatch>,
        /// Cell shading (`w:shd`, §17.4.33).
        #[serde(default)]
        shading: Option<ShadingWire>,
        /// Cell width (`w:tcW`, §17.4.72).
        #[serde(default)]
        width: Option<MeasurementPatch>,
        /// Vertical alignment (`w:vAlign`, §17.4.84): `top` | `center` | `bottom`.
        #[serde(default)]
        v_align: Option<String>,
        /// Per-cell margin overrides (`w:tcMar`, §17.4.41), in twips.
        #[serde(default)]
        margins: Option<CellMarginsPatch>,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Set row-level formatting (height + height rule) on ONE table row **in
    /// place**, as a tracked `w:trPrChange` (§17.13.5.36). The row is addressed
    /// by `row_index` — the same address the read view mints. Like
    /// `set_cell_format`, this is an in-place property edit: it byte-preserves
    /// `tblPr`, every OTHER row, and every cell of the target row, so it bypasses
    /// the whole-table v4 replace schema (and its formatting refusal) entirely.
    /// At least one property field must be present.
    SetRowFormat {
        target: NodeId,
        #[serde(default)]
        semantic_hash: Option<String>,
        /// Zero-based row index into the table.
        row_index: usize,
        /// Row height in twips (`w:trHeight w:val`, §17.4.81).
        #[serde(default)]
        height: Option<u32>,
        /// Row height rule (`w:trHeight w:hRule`, §17.18.37): `exact` |
        /// `atLeast` | `auto`.
        #[serde(default)]
        height_rule: Option<String>,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Insert a REF / PAGEREF / NOREF cross-reference field after `expect`, as
    /// a tracked `w:fldSimple` insert (§17.16.5.45 / .39 / .36). Mirrors
    /// `EditStep::InsertCrossReference`.
    InsertCrossRef {
        target: NodeId,
        expect: String,
        #[serde(default)]
        semantic_hash: Option<String>,
        /// Bookmark name the cross-reference points at. Must be non-empty
        /// (validated in `validate_schema`).
        bookmark: String,
        /// `"ref"` | `"pageref"` | `"noref"` — validated and mapped to `RefKind`.
        ref_kind: String,
        /// `\h` — render the cross-reference as a hyperlink to the bookmark.
        #[serde(default)]
        as_hyperlink: bool,
        /// `\n` — insert the bookmark's paragraph number, no trailing context.
        #[serde(default)]
        no_paragraph_number: bool,
        /// `\r` — insert the bookmark's relative paragraph number.
        #[serde(default)]
        paragraph_number_relative: bool,
        /// `\w` — insert the bookmark's full-context paragraph number.
        #[serde(default)]
        paragraph_number_full: bool,
        /// `\p` — append the relative position ("above" / "below").
        #[serde(default)]
        above_below: bool,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Set the paragraph's auto-numbering (attach/detach a list, change level,
    /// restart the counter) as a tracked `w:pPrChange`. Mirrors
    /// `EditStep::SetParagraphNumbering`. No `expect` substring: a numbering
    /// change is a property change, so staleness rides on the target id +
    /// optional `semantic_hash`.
    SetNumbering {
        target: NodeId,
        change: NumberingChangeWire,
        #[serde(default)]
        semantic_hash: Option<String>,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Insert a bookmark (`w:bookmarkStart` + `w:bookmarkEnd`, §17.13.6) around
    /// `expect`. Mirrors `EditStep::InsertBookmark`. `name` must be non-empty
    /// (validated in `validate_schema` => `SchemaError::BookmarkEmptyName`).
    InsertBookmark {
        target: NodeId,
        expect: String,
        /// Bookmark name. Must be non-empty.
        name: String,
        #[serde(default)]
        semantic_hash: Option<String>,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Rename a bookmark's `w:name`. Mirrors `EditStep::RenameBookmark`.
    /// `new_name` must be non-empty.
    RenameBookmark {
        target: NodeId,
        old_name: String,
        new_name: String,
        #[serde(default)]
        semantic_hash: Option<String>,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Remove a bookmark's start+end pair. Mirrors `EditStep::RemoveBookmark`.
    RemoveBookmark {
        target: NodeId,
        name: String,
        #[serde(default)]
        semantic_hash: Option<String>,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Apply a named paragraph style as a tracked `w:pPrChange`. Mirrors
    /// `EditStep::ApplyStyle`. No `expect` substring: a style change is a
    /// property change, so staleness rides on the target id + optional
    /// `semantic_hash`. `style_id` must be non-empty (validated in
    /// `validate_schema`).
    ApplyStyle {
        target: NodeId,
        /// The style ID to apply (the `w:val` of `w:pStyle`).
        style_id: String,
        #[serde(default)]
        semantic_hash: Option<String>,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Set display attributes on an existing opaque drawing: resize its
    /// `wp:extent` and/or set its `wp:docPr` alt text. Mirrors
    /// `EditStep::SetImageAttributes`. A direct, untracked in-place mutation;
    /// the binary media part is never touched. At least one of `resize` /
    /// `alt_text` must be present (validated in `validate_schema`).
    SetImageAttrs {
        /// The paragraph hosting the drawing.
        target: NodeId,
        /// The drawing's stable opaque-inline id.
        drawing_id: NodeId,
        #[serde(default)]
        semantic_hash: Option<String>,
        /// New `wp:extent` dimensions in EMUs. Both `cx`/`cy` required when
        /// present; each must be `>= 0` (validated at the wire edge).
        #[serde(default)]
        resize: Option<ImageResizeWire>,
        /// Alt-text edit, three-state: omitted (`None`) leaves `descr`
        /// untouched; an explicit JSON `null` (`Some(None)`) clears it; a
        /// string (`Some(Some(s))`) sets it. Mirrors `Option<Option<String>>`.
        #[serde(default, deserialize_with = "deserialize_double_option")]
        alt_text: Option<Option<String>>,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// `EditStep::DeleteImage`. Tracked deletion of an existing inline drawing,
    /// addressed by drawing id. Guard is the drawing's own `content_hash` (like
    /// `SetImageAttrs`), not the block's text guard.
    DeleteImage {
        /// The paragraph hosting the drawing.
        target: NodeId,
        /// The drawing's stable opaque-inline id.
        drawing_id: NodeId,
        #[serde(default)]
        semantic_hash: Option<String>,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Author a new comment anchored to `expect` in `target` (§17.13.4).
    /// Mirrors `EditStep::CommentCreate`. `body` must be non-empty (validated in
    /// `validate_schema`). Comments are annotations, not tracked changes.
    CommentCreate {
        target: NodeId,
        expect: String,
        /// Comment body text. Must be non-empty.
        body: String,
        #[serde(default)]
        author: Option<String>,
        #[serde(default)]
        semantic_hash: Option<String>,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Reply to an existing comment (MS-DOCX §2.5.1). Mirrors
    /// `EditStep::CommentReply`. `parent_comment_id` and `body` must be
    /// non-empty (validated in `validate_schema`).
    CommentReply {
        /// The parent comment's `w:id`.
        parent_comment_id: String,
        /// Reply body text. Must be non-empty.
        body: String,
        #[serde(default)]
        author: Option<String>,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Set a comment's resolved (`w15:done`) flag. Mirrors
    /// `EditStep::CommentResolve`. `comment_id` must be non-empty.
    CommentResolve {
        comment_id: String,
        done: bool,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Delete a comment and all its anchor markers (§17.13.4). Mirrors
    /// `EditStep::CommentDelete`. `comment_id` must be non-empty.
    CommentDelete {
        comment_id: String,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Insert a footnote or endnote (§17.11). Mirrors `EditStep::InsertNote`.
    /// `note_kind` is the wire string `"footnote"` | `"endnote"` (validated and
    /// mapped to `NoteKind`; an unknown value is refused, never defaulted).
    /// `expect` and `body` must be non-empty (validated in `validate_schema`).
    InsertNote {
        target: NodeId,
        expect: String,
        /// `"footnote"` | `"endnote"`.
        note_kind: String,
        /// The note body text. Must be non-empty.
        body: String,
        #[serde(default)]
        semantic_hash: Option<String>,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Edit an existing note's body (§17.11). Mirrors `EditStep::EditNote`.
    /// `note_id` and `body` must be non-empty; `note_kind` is `"footnote"` |
    /// `"endnote"`.
    EditNote {
        note_id: String,
        /// `"footnote"` | `"endnote"`.
        note_kind: String,
        /// The replacement body text. Must be non-empty.
        body: String,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Delete a note and its reference runs (§17.11). Mirrors
    /// `EditStep::DeleteNote`. `note_id` must be non-empty; `note_kind` is
    /// `"footnote"` | `"endnote"`.
    DeleteNote {
        note_id: String,
        /// `"footnote"` | `"endnote"`.
        note_kind: String,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Set page-setup properties on a section (§17.6). Mirrors
    /// `EditStep::SetPageSetup`. `target` is the wire section target
    /// (`{"section":"body"}` or `{"paragraph":"<block_id>"}`); the patch fields
    /// are all optional but at least one must be present (validated in
    /// `validate_schema`). `orientation` is `"portrait"` | `"landscape"` — an
    /// unknown token is refused, never defaulted.
    SetPageSetup {
        target: SectionTargetWire,
        #[serde(default)]
        page_size: Option<PageSizeWire>,
        /// `"portrait"` | `"landscape"`.
        #[serde(default)]
        orientation: Option<String>,
        #[serde(default)]
        margins: Option<PageMarginsWire>,
        #[serde(default)]
        columns: Option<ColumnLayoutWire>,
        #[serde(default)]
        gutter: Option<u32>,
        #[serde(default)]
        semantic_hash: Option<String>,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Set the section type (`w:type`, §17.6.22). Mirrors
    /// `EditStep::SetSectionType`. `section_type` is `"next_page"` |
    /// `"continuous"` | `"even_page"` | `"odd_page"` | `"next_column"` — an
    /// unknown token is refused at the wire edge, NEVER defaulted.
    SetSectionType {
        target: SectionTargetWire,
        section_type: String,
        #[serde(default)]
        semantic_hash: Option<String>,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Insert a mid-document section break (§17.6). Mirrors
    /// `EditStep::InsertSectionBreak`. `section_type` is the same token set as
    /// `SetSectionType`; the geometry fields mirror `SetPageSetup`'s patch.
    InsertSectionBreak {
        anchor: NodeId,
        section_type: String,
        #[serde(default)]
        page_size: Option<PageSizeWire>,
        #[serde(default)]
        orientation: Option<String>,
        #[serde(default)]
        margins: Option<PageMarginsWire>,
        #[serde(default)]
        columns: Option<ColumnLayoutWire>,
        #[serde(default)]
        gutter: Option<u32>,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Edit a header story paragraph's text, tracked (§17.10). Mirrors
    /// `EditStep::EditHeader`. `header_part` is the header story's part name
    /// (e.g. "header1.xml"); `target` is the story-local paragraph id. `content`
    /// is the v4 inline list (same shape as a paragraph replace).
    EditHeader {
        header_part: String,
        target: NodeId,
        expect: String,
        content: Vec<Inline>,
        #[serde(default)]
        semantic_hash: Option<String>,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Edit a footer story paragraph's text, tracked (§17.10). Mirrors
    /// `EditStep::EditFooter`.
    EditFooter {
        footer_part: String,
        target: NodeId,
        expect: String,
        content: Vec<Inline>,
        #[serde(default)]
        semantic_hash: Option<String>,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Author a NET-NEW, blank header story + body-section reference, tracked as
    /// a `w:sectPrChange` (§17.10.2 / §17.13.5.32). Mirrors
    /// `EditStep::CreateHeader`. `kind` is `"default"` | `"first"` | `"even"`
    /// (validated and mapped in `translate_op`; an unknown token is refused).
    CreateHeader {
        kind: String,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Author a NET-NEW, blank footer story. Footer twin of `CreateHeader`;
    /// mirrors `EditStep::CreateFooter`.
    CreateFooter {
        kind: String,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Toggle header/footer display mode and link/unlink references (§17.6.18 /
    /// §17.15.1.35). Mirrors `EditStep::SetHeaderFooterMode`. At least one of
    /// `title_page` / `even_and_odd` / `link` must be present (validated in
    /// `validate_schema`).
    SetHeaderFooterMode {
        #[serde(default)]
        title_page: Option<bool>,
        #[serde(default)]
        even_and_odd: Option<bool>,
        #[serde(default)]
        link: Option<HeaderFooterLinkWire>,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Insert an OMML equation after `expect`, tracked. Mirrors
    /// `EditStep::InsertEquation`. `omml` is the caller-supplied math fragment
    /// (`m:oMath` for inline, `m:oMathPara` for block); `placement` is `inline`
    /// or `block` (validated and mapped in `translate_op`). The fragment must be
    /// non-empty (validated in `validate_schema`).
    InsertEquation {
        target: NodeId,
        expect: String,
        #[serde(default)]
        semantic_hash: Option<String>,
        /// The OMML fragment as a UTF-8 string.
        omml: String,
        /// `"inline"` | `"block"` — validated and mapped to `EquationPlacement`.
        placement: String,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Convert a contiguous run of paragraphs (`from`..=`to`, by block id) into a
    /// TABLE, as a single composed tracked change. Mirrors
    /// `EditStep::BlocksToTable`. Each source paragraph's visible text is split by
    /// `delimiter` into cells (one body row per paragraph); an optional `header`
    /// adds a leading header row and fixes the column count. The new table is a
    /// tracked insert and the source paragraphs a tracked delete, so accept-all =>
    /// the table, reject-all => the original paragraphs. `delimiter` must be
    /// non-empty and `header` (if present) non-empty (validated in
    /// `validate_schema`).
    BlocksToTable {
        /// First source paragraph (inclusive).
        from: NodeId,
        /// Last source paragraph (inclusive).
        to: NodeId,
        /// Delimiter splitting each paragraph's text into cells. Non-empty.
        delimiter: String,
        /// Optional header row cell texts; its length fixes the column count.
        #[serde(default)]
        header: Option<Vec<String>>,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Wrap a run-span (`expect` inside `target`) in a content control (`w:sdt`,
    /// §17.5.2). Mirrors `EditStep::WrapInContentControl`. `control` is the
    /// typed control spec; at least one distinguishing field (tag/alias/
    /// non-rich-text control) must be present (validated in `validate_schema`).
    WrapContentControl {
        target: NodeId,
        expect: String,
        #[serde(default)]
        semantic_hash: Option<String>,
        /// `w:tag` programmatic handle, if any.
        #[serde(default)]
        tag: Option<String>,
        /// `w:alias` human title, if any.
        #[serde(default)]
        alias: Option<String>,
        /// The control type and its parameters.
        control: SdtControlWire,
        /// Optional XML data binding (`w:dataBinding`, §17.5.2.6). When present,
        /// the control is bound to a node in a custom-XML datastore part; the
        /// save path authors the backing part keyed by `store_item_id`. A
        /// binding with an empty `xpath`/`store_item_id` is refused in
        /// `validate_schema` (`MalformedDataBinding`).
        #[serde(default)]
        data_binding: Option<DataBindingWire>,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Set the displayed value of an existing content control. Mirrors
    /// `EditStep::SetContentControlValue`. Exactly one of `text` / `checked` /
    /// `selected` must be present (validated in `validate_schema`).
    SetContentControlValue {
        target: NodeId,
        sdt_id: NodeId,
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        checked: Option<bool>,
        #[serde(default)]
        selected: Option<String>,
        /// When true, record the change as tracked w:ins/w:del inside sdtContent.
        /// NOT yet supported — refused with `TrackedContentControlSetUnsupported`
        /// rather than silently downgraded. Default false (untracked, in-place).
        #[serde(default)]
        tracked: bool,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Fill a legacy form field (FORMTEXT / FORMCHECKBOX / FORMDROPDOWN — the
    /// `w:fldChar`/`w:ffData` carrier). Mirrors `EditStep::SetFormFieldValue`.
    /// `field_id` is the BEGIN anchor's opaque id. Exactly one of
    /// `text`/`checked`/`selected` must be present (validated in `validate_schema`).
    SetFormFieldValue {
        target: NodeId,
        field_id: NodeId,
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        checked: Option<bool>,
        #[serde(default)]
        selected: Option<String>,
        #[serde(default)]
        semantic_hash: Option<String>,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Insert a new inline image into a paragraph. Mirrors
    /// `EditStep::InsertImage`. `bytes_base64` is the base64-encoded binary;
    /// `format` is `"png"`|`"jpeg"`|`"gif"`. The format is validated against the
    /// bytes' magic at the verb edge.
    ///
    /// `cx`/`cy` are the display box in EMUs and are OPTIONAL: omit BOTH to use
    /// the image's intrinsic pixel dimensions at 96 DPI (1 px = 9525 EMU); supply
    /// exactly ONE to derive the other from the intrinsic aspect ratio; supply
    /// BOTH to set the box explicitly. A header we cannot decode is refused
    /// (never a default size) — CLAUDE.md "no silent fallbacks".
    InsertImage {
        target: NodeId,
        /// Base64-encoded image bytes. Decoded + magic-checked at translation.
        bytes_base64: String,
        /// `"png"` | `"jpeg"` | `"gif"` — an unknown value is refused.
        format: String,
        /// Display width in EMUs (`wp:extent` @cx). `None` => derive (see above).
        #[serde(default)]
        cx: Option<i64>,
        /// Display height in EMUs (`wp:extent` @cy). `None` => derive (see above).
        #[serde(default)]
        cy: Option<i64>,
        #[serde(default)]
        alt_text: Option<String>,
        /// Optional anchor: append after the segment containing this text.
        #[serde(default)]
        expect: Option<String>,
        #[serde(default)]
        semantic_hash: Option<String>,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Replace the binary media of an existing drawing. Mirrors
    /// `EditStep::ReplaceImage`. Direct/untracked.
    ReplaceImage {
        target: NodeId,
        drawing_id: NodeId,
        bytes_base64: String,
        /// `"png"` | `"jpeg"` | `"gif"`.
        format: String,
        /// Display width in EMUs. `None` => derive from intrinsic dimensions (see
        /// `InsertImage`); the aspect guard then trivially matches.
        #[serde(default)]
        cx: Option<i64>,
        /// Display height in EMUs. `None` => derive from intrinsic dimensions.
        #[serde(default)]
        cy: Option<i64>,
        #[serde(default)]
        alt_text: Option<String>,
        /// Override the aspect-ratio guard (default `false`): permit a deliberate
        /// stretch when the requested extent's aspect ratio disagrees with the
        /// replacement image's intrinsic aspect.
        #[serde(default)]
        allow_stretch: bool,
        #[serde(default)]
        semantic_hash: Option<String>,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Replace the whole interior of a textbox (`w:txbxContent`). Mirrors
    /// `EditStep::SetTextboxText`. `paragraphs` is one string per paragraph.
    /// Direct/untracked; refuses if the interior already has tracked changes.
    SetTextboxText {
        target: NodeId,
        drawing_id: NodeId,
        paragraphs: Vec<String>,
        #[serde(default)]
        semantic_hash: Option<String>,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Surgical text replacement INSIDE an opaque region — replace the first
    /// occurrence of `find` with `replacement` in one addressed textbox paragraph
    /// or inline content-control text region. Mirrors `EditStep::OpaqueTextEdit`.
    /// Tracked when the transaction is in tracked mode; the addressed textbox's
    /// identical AlternateContent copies are mirrored. `container_index` /
    /// `paragraph_index` come from `opaque_text_targets` (both default 0).
    OpaqueTextEdit {
        target: NodeId,
        opaque_id: NodeId,
        find: String,
        replacement: String,
        #[serde(default)]
        container_index: usize,
        #[serde(default)]
        paragraph_index: usize,
        #[serde(default)]
        semantic_hash: Option<String>,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Set a content control's text VALUE (whole-value replace), tracked. Mirrors
    /// `EditStep::SdtTextFill`. Exactly one target: an INLINE control via
    /// `block_id` + `sdt_id`, or a BLOCK-level control via `body_index` (both from
    /// `opaque_text_targets`). The forms "fill this field" op.
    SdtTextFill {
        #[serde(default)]
        block_id: Option<NodeId>,
        #[serde(default)]
        sdt_id: Option<NodeId>,
        #[serde(default)]
        body_index: Option<usize>,
        value: String,
        #[serde(default)]
        semantic_hash: Option<String>,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Author a new style into `word/styles.xml`. Mirrors
    /// `EditStep::CreateStyle`. `style_id` and `name` must be non-empty.
    CreateStyle {
        #[serde(flatten)]
        def: StyleDefinitionWire,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Modify an existing style. Mirrors `EditStep::ModifyStyle`. The style to
    /// modify is addressed by `def.style_id` (the flattened definition carries the
    /// id); there is no separate outer `style_id` field.
    ModifyStyle {
        #[serde(flatten)]
        def: StyleDefinitionWire,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Set the document DEFAULT run font/size
    /// (`w:docDefaults/w:rPrDefault/w:rPr`). Mirrors `EditStep::SetDocDefaults`.
    /// The one-edit body-text re-skin: unstyled body text that inherits its font
    /// from the document defaults picks up the new values without editing any
    /// individual style. At least one of `font_family` / `font_size_half_points`
    /// must be present (validated in `validate_schema`).
    SetDocDefaults {
        /// Literal font family for `w:rFonts` @ascii/@hAnsi/@cs.
        #[serde(default)]
        font_family: Option<String>,
        /// Font size in half-points for `w:sz`/`w:szCs` @val (24 = 12pt).
        #[serde(default)]
        font_size_half_points: Option<u32>,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Granular structural edit on an EXISTING table: insert/delete a row or
    /// column, merge a rectangular cell region, or set one cell's text. Mirrors
    /// `EditStep::TableStructureOp`; routes through the same table-diff machinery
    /// `replace(table)` uses, producing row/cell-level tracked changes.
    TableOp {
        target: NodeId,
        #[serde(default)]
        semantic_hash: Option<String>,
        /// The granular structural op. Named `table_op` (not `op`) because the
        /// outer `Op` enum is serde-tagged on the `op` field.
        table_op: TableOpWire,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Set TABLE-level formatting (borders / width / default cell margins) on a
    /// table **in place**, as a tracked `w:tblPrChange` (§17.13.5.34). The table
    /// is addressed by `target` block id. Like `set_cell_format`, this is an
    /// in-place property edit: it byte-preserves every `w:tr`, every `w:tc`, and
    /// all other `tblPr` properties, so it bypasses the whole-table v4 replace
    /// schema (and its formatting refusal) entirely. At least one of the property
    /// fields must be present. Mirrors `EditStep::SetTableFormatting`.
    ///
    /// There is intentionally NO shading field: `tblPr` carries no shading (cell
    /// shading lives on `w:tcPr`, authored via `set_cell_format`), so a
    /// table-level shading request would have nothing to land on.
    SetTableFormat {
        target: NodeId,
        #[serde(default)]
        semantic_hash: Option<String>,
        /// Table borders (`w:tblBorders`, §17.4.39).
        #[serde(default)]
        borders: Option<BorderSetPatch>,
        /// Table width (`w:tblW`, §17.4.64).
        #[serde(default)]
        width: Option<MeasurementPatch>,
        /// Default cell margins (`w:tblCellMar`, §17.4.43), in twips.
        #[serde(default)]
        default_cell_margins: Option<CellMarginsPatch>,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Set the *layout* display attributes on an existing opaque drawing: crop
    /// (`a:srcRect`), floating position (`wp:positionH`/`wp:positionV`), and
    /// text-wrap type. Mirrors `EditStep::SetImageLayout`. Direct, untracked;
    /// the binary media is never touched. At least one of crop/position/wrap
    /// must be present (validated in `validate_schema`). Position and wrap
    /// require an already-floating (anchored) drawing.
    SetImageLayout {
        /// The paragraph hosting the drawing.
        target: NodeId,
        /// The drawing's stable opaque-inline id.
        drawing_id: NodeId,
        #[serde(default)]
        semantic_hash: Option<String>,
        /// Horizontal position (`wp:positionH`). Anchor-only.
        #[serde(default)]
        position_h: Option<ImagePositionWire>,
        /// Vertical position (`wp:positionV`). Anchor-only.
        #[serde(default)]
        position_v: Option<ImagePositionWire>,
        /// Text-wrap type token: `none` | `square` | `tight` | `through` |
        /// `top_and_bottom`. Anchor-only. Unknown tokens are rejected at the edge.
        #[serde(default)]
        wrap: Option<String>,
        /// Crop rectangle (`a:srcRect` insets, 1000ths of a percent). Reachable
        /// on inline and anchor drawings alike.
        #[serde(default)]
        crop: Option<ImageCropWire>,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Wrap a contiguous, top-level RANGE of body blocks
    /// (`[start_block, end_block]`, inclusive) in a block-level content control
    /// (`w:sdt`, §17.5.2). Mirrors `EditStep::WrapBlocksInContentControl` — the
    /// block-level sibling of `wrap_content_control`. `control` is the typed
    /// control spec; at least one distinguishing field (tag/alias/non-rich-text
    /// control) must be present (validated in `validate_schema`).
    WrapBlocksContentControl {
        /// First block of the range (inclusive), by stable block id.
        start_block: NodeId,
        /// Last block of the range (inclusive), by stable block id.
        end_block: NodeId,
        /// `w:tag` programmatic handle, if any.
        #[serde(default)]
        tag: Option<String>,
        /// `w:alias` human title, if any.
        #[serde(default)]
        alias: Option<String>,
        /// The control type and its parameters.
        control: SdtControlWire,
        #[serde(default)]
        rationale: Option<String>,
    },
    // ─── add new wire ops above ──────────────────────────────────────────────
    // Mirror a new `EditStep`; wire it in `translate_op` + `validate_schema`.
    // ALSO add the tag + field list to `OP_FIELDS` (below `EditTransactionV4`)
    // — that table is what rejects a misnamed field on this variant. Forgetting
    // this step is NOT silent: `check_op_fields` rejects any `op` tag that
    // isn't in `OP_FIELDS`, so the new variant fails loudly on its very first
    // parse (every op, not just misnamed-field ones) until its entry is added.
    // See `edit/AGENTS.md`.
}

/// Wire form of a granular table structural op (`Op::TableOp.op`). Tagged by
/// `kind`. Indices are 0-based. `position` is `"before"` | `"after"`. The
/// adapter maps this 1:1 onto `verbs::table_ops::TableOp`; unknown enum values
/// fail at the wire edge (no silent fallback).
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TableOpWire {
    /// Insert a row before/after `ref_row`, copying that row's columns.
    /// `cells` gives each new cell's plain text, left-to-right; omit (or give
    /// fewer than the column count) for a blank/partially-blank row. More
    /// entries than columns is refused at apply time (the schema doesn't know
    /// the table's column count).
    InsertRow {
        ref_row: usize,
        #[serde(default = "default_after")]
        position: String,
        #[serde(default)]
        cells: Option<Vec<String>>,
    },
    /// Delete the row at `row_index`.
    DeleteRow { row_index: usize },
    /// Insert an empty column before/after `ref_col` (simple-grid only).
    InsertColumn {
        ref_col: usize,
        #[serde(default = "default_after")]
        position: String,
    },
    /// Delete the column at `col_index` (simple-grid only).
    DeleteColumn { col_index: usize },
    /// Merge the rectangular region [start_row..=end_row] × [start_col..=end_col].
    MergeCells {
        start_row: usize,
        start_col: usize,
        end_row: usize,
        end_col: usize,
    },
    /// Replace the text of the cell at (row, col) with `text`.
    SetCellText {
        row_index: usize,
        col_index: usize,
        text: String,
    },
}

fn default_after() -> String {
    "after".to_string()
}

/// Wire shape for a style definition (`create_style`/`modify_style`). Mirrors
/// [`StyleDefinition`] with string-tagged enums so callers write
/// `{"style_id":"H1","style_type":"para","name":"Heading 1", ...}`. Unknown
/// `style_type` / `alignment` tokens are refused (NEVER defaulted).
///
/// This is the ONE wire struct flattened into the internally-tagged [`Op`] enum
/// (`#[serde(flatten)]` on `Op::CreateStyle`/`Op::ModifyStyle`). serde's
/// `#[serde(deny_unknown_fields)]` is a documented no-op under `flatten`, so a
/// misnamed top-level key (e.g. `run_format` instead of `run_props`) would be
/// SILENTLY DROPPED — the style would author with no font and the op would
/// report success (a "no silent fallback" violation). The hand-written
/// `Deserialize` below restores fail-loud: it buffers the def's own keys (the
/// flatten machinery only hands it the leftover keys, NOT the enum's `op` /
/// `rationale`), rejects any key outside [`STYLE_DEF_FIELDS`], and suggests the
/// nearest valid field. Misnamed NESTED keys (`font` inside `run_props`) are
/// caught separately by `deny_unknown_fields` on the leaf prop structs below,
/// which are plain nested fields (never flattened) and so accept the attribute.
#[derive(Clone, Debug)]
pub struct StyleDefinitionWire {
    pub style_id: String,
    /// `"para"` | `"char"` | `"table"` | `"numbering"`.
    pub style_type: String,
    pub based_on: Option<String>,
    pub name: String,
    pub run_props: StyleRunPropsWire,
    pub para_props: StyleParaPropsWire,
}

/// The set of top-level keys a flattened [`StyleDefinitionWire`] accepts. Used
/// by the hand-written `Deserialize` to reject (and suggest a fix for) a
/// misnamed key that `deny_unknown_fields` cannot catch under `#[serde(flatten)]`.
const STYLE_DEF_FIELDS: &[&str] = &[
    "style_id",
    "style_type",
    "based_on",
    "name",
    "run_props",
    "para_props",
];

impl<'de> Deserialize<'de> for StyleDefinitionWire {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error as _;
        // The leaf struct does the real field extraction (and, via its leaf
        // `deny_unknown_fields`, rejects misnamed NESTED keys). We buffer the
        // object first so we can reject misnamed TOP-LEVEL keys, which the
        // flatten machinery would otherwise silently swallow.
        #[derive(Deserialize)]
        struct Inner {
            style_id: String,
            style_type: String,
            #[serde(default)]
            based_on: Option<String>,
            name: String,
            #[serde(default)]
            run_props: StyleRunPropsWire,
            #[serde(default)]
            para_props: StyleParaPropsWire,
        }

        let value = serde_json::Value::deserialize(deserializer)?;
        if let Some(obj) = value.as_object() {
            for key in obj.keys() {
                if STYLE_DEF_FIELDS.contains(&key.as_str()) {
                    continue;
                }
                // Suggest the nearest known field by a cheap shared-prefix probe
                // (catches run_format->run_props, font->font_family at this level,
                // align->alignment, etc.), so the error names the likely fix.
                let probe = &key[..key.len().min(4)];
                let suggestion = STYLE_DEF_FIELDS.iter().find(|field| {
                    field.starts_with(probe) || key.starts_with(&field[..field.len().min(4)])
                });
                let hint = match suggestion {
                    Some(field) => format!(" (did you mean `{field}`?)"),
                    None => String::new(),
                };
                return Err(D::Error::custom(format!(
                    "unknown style definition field `{key}`{hint}; expected one of \
                     style_id, style_type, based_on, name, run_props, para_props"
                )));
            }
        }
        let inner = Inner::deserialize(value).map_err(D::Error::custom)?;
        Ok(StyleDefinitionWire {
            style_id: inner.style_id,
            style_type: inner.style_type,
            based_on: inner.based_on,
            name: inner.name,
            run_props: inner.run_props,
            para_props: inner.para_props,
        })
    }
}

/// Wire shape for a style's run-property subset (`create_style.run_props`).
/// `deny_unknown_fields`: a misnamed key (e.g. `font` for `font_family`, `size`
/// for `font_size_half_points`) is rejected at the wire edge rather than
/// silently dropped — the font/size would otherwise vanish and the op would
/// report success (a "no silent fallback" violation).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StyleRunPropsWire {
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
    #[serde(default)]
    pub underline: bool,
    #[serde(default)]
    pub font_size_half_points: Option<u32>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub font_family: Option<String>,
}

/// Wire shape for a style's paragraph-property subset
/// (`create_style.para_props`). `deny_unknown_fields`: a misnamed key (e.g.
/// `align` for `alignment`) is rejected at the wire edge rather than silently
/// dropped.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StyleParaPropsWire {
    /// `"left"`|`"center"`|`"right"`|`"justify"` etc. — an unknown token is refused.
    #[serde(default)]
    pub alignment: Option<String>,
    #[serde(default)]
    pub spacing_before: Option<i32>,
    #[serde(default)]
    pub spacing_after: Option<i32>,
    #[serde(default)]
    pub line_spacing: Option<i32>,
    #[serde(default)]
    pub indent_left: Option<i32>,
    #[serde(default)]
    pub indent_right: Option<i32>,
    #[serde(default)]
    pub indent_first_line: Option<i32>,
}

/// Wire shape for a section target: the body section or a paragraph's section
/// break. Untagged so callers write `{"section":"body"}` or
/// `{"paragraph":"p_7"}`.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SectionTargetWire {
    /// `{"section":"body"}` — the document-level section.
    Section(String),
    /// `{"paragraph":"<block_id>"}` — a mid-document section break.
    Paragraph(NodeId),
}

/// Wire shape for `set_page_setup.page_size` — page dimensions in twips
/// (`w:pgSz`, §17.6.13).
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageSizeWire {
    pub width: u32,
    pub height: u32,
}

/// Wire shape for `set_page_setup.margins` — all four edges + header/footer
/// distance in twips (`w:pgMar`, §17.6.11). All required together: a partial
/// margin box is ambiguous.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageMarginsWire {
    pub top: i32,
    pub bottom: i32,
    pub left: i32,
    pub right: i32,
    pub header: u32,
    pub footer: u32,
}

/// Wire shape for `set_page_setup.columns` — equal-width columns + gutter
/// (`w:cols`, §17.6.4).
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColumnLayoutWire {
    pub count: u32,
    pub space: u32,
}

/// Wire shape for `set_header_footer_mode.link` — link/unlink a header/footer
/// reference by kind.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeaderFooterLinkWire {
    /// `true` = header (`headerReference`), `false` = footer.
    pub is_header: bool,
    /// `"default"` | `"first"` | `"even"` — an unknown token is refused.
    pub kind: String,
    /// `true` = link, `false` = unlink.
    pub link: bool,
}

/// Wire shape for `set_image_attrs.resize` — drawing dimensions in EMUs
/// (`wp:extent` @cx/@cy, §20.4.2.7). Both required: resizing one axis without
/// the other silently distorts the aspect ratio.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageResizeWire {
    pub cx: i64,
    pub cy: i64,
}

/// Wire shape for `set_image_layout.position_{h,v}` — one positioning axis of a
/// floating drawing (`wp:positionH`/`wp:positionV`, §20.4.2.10/§20.4.2.11).
/// `relative_from` is the `@relativeFrom` frame token. Exactly one of
/// `offset`/`align` must be set (enforced in `validate_schema`): `offset` emits
/// `<wp:posOffset>` (EMU, may be negative); `align` emits `<wp:align>` (a
/// keyword like `left`/`center`/`right`/`top`/`bottom`).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImagePositionWire {
    pub relative_from: String,
    #[serde(default)]
    pub offset: Option<i64>,
    #[serde(default)]
    pub align: Option<String>,
}

/// Wire shape for `set_image_layout.crop` — `a:srcRect` edge insets in 1000ths
/// of a percent (`ST_Percentage`, 0..=100000). Each edge is optional so a caller
/// can adjust one without disturbing the others.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageCropWire {
    #[serde(default)]
    pub left: Option<i32>,
    #[serde(default)]
    pub top: Option<i32>,
    #[serde(default)]
    pub right: Option<i32>,
    #[serde(default)]
    pub bottom: Option<i32>,
}

/// Wire shape for a content-control type (`wrap_content_control.control`).
/// Internally tagged on `kind`: `{"kind":"plain_text"}`,
/// `{"kind":"dropdown","items":[…]}`, `{"kind":"checkbox","checked":true}`, etc.
/// Unknown kinds are rejected by serde (NEVER defaulted).
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SdtControlWire {
    PlainText,
    RichText,
    Dropdown {
        #[serde(default)]
        items: Vec<SdtListItemWire>,
    },
    ComboBox {
        #[serde(default)]
        items: Vec<SdtListItemWire>,
    },
    Checkbox {
        checked: bool,
    },
    Date,
    RepeatingSection,
}

/// Wire shape for one drop-down / combo box list item.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SdtListItemWire {
    pub display: String,
    pub value: String,
}

/// Wire shape for a content-control XML data binding
/// (`wrap_content_control.data_binding`, `w:dataBinding`, §17.5.2.6).
///
/// `xpath` selects the bound node in the datastore part; `store_item_id` is the
/// backing part's `storeItemID` GUID. Both must be non-empty (enforced in
/// `validate_schema` -> `MalformedDataBinding`). `prefix_mappings` declares the
/// XML namespace prefixes used in `xpath`, when any.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataBindingWire {
    pub xpath: String,
    pub store_item_id: String,
    #[serde(default)]
    pub prefix_mappings: Option<String>,
}

/// Deserialize `Option<Option<T>>` distinguishing "field omitted" (`None`) from
/// "field present and null" (`Some(None)`). serde's default `Option` collapses
/// both to `None`, which would erase the alt-text three-state; this helper
/// preserves it so a `null` means "clear", not "leave unchanged".
fn deserialize_double_option<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // Reached only when the field is present (serde skips this for an omitted
    // field because of `#[serde(default)]`), so an inner `Option` is enough:
    // present-and-null -> Some(None); present-and-string -> Some(Some(s)).
    Ok(Some(Option::<String>::deserialize(deserializer)?))
}

/// Wire shape for `set_para_format.indent` — twip/character indentation
/// (§17.3.1.12). Each field is optional; the adapter maps present fields onto
/// the domain `Indentation`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndentPatch {
    #[serde(default)]
    pub left: Option<i32>,
    #[serde(default)]
    pub right: Option<i32>,
    /// First-line indent in twips relative to `left` (positive = indent,
    /// negative = hanging).
    #[serde(default)]
    pub first_line: Option<i32>,
}

impl IndentPatch {
    fn is_empty(&self) -> bool {
        self.left.is_none() && self.right.is_none() && self.first_line.is_none()
    }
}

/// Wire shape for `set_para_format.spacing` — line + before/after spacing
/// (§17.3.1.33). Each field is optional.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpacingPatch {
    /// Space before the paragraph, in twips.
    #[serde(default)]
    pub before: Option<u32>,
    /// Space after the paragraph, in twips.
    #[serde(default)]
    pub after: Option<u32>,
    /// Line spacing value (interpreted per `line_rule`).
    #[serde(default)]
    pub line: Option<u32>,
    /// Line rule: `auto` | `exact` | `at_least`. Required when `line` is set;
    /// an unknown rule is rejected at the wire edge.
    #[serde(default)]
    pub line_rule: Option<String>,
}

/// Wire shape for one border edge (`w:top`/`w:bottom`/… inside `w:pBdr`,
/// `w:tcBorders`, or `w:tblBorders`). Maps onto the domain [`Border`]. `style`
/// is required (a border with no style is meaningless); `color` is `auto` or
/// six hex digits; `size`/`space` are the OOXML units (size in eighths of a
/// point, space in points).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BorderPatch {
    /// Border style token per §17.18.2 `ST_Border` (e.g. `single`, `double`).
    pub style: String,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub size: Option<u32>,
    #[serde(default)]
    pub space: Option<u32>,
}

/// Wire shape for `set_para_format.borders` — paragraph borders (`w:pBdr`,
/// §17.3.1.24). Each edge is optional; the adapter maps present edges onto the
/// domain [`ParagraphBorders`]. An empty patch (no edge) is treated as absent.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParaBordersPatch {
    #[serde(default)]
    pub top: Option<BorderPatch>,
    #[serde(default)]
    pub bottom: Option<BorderPatch>,
    #[serde(default)]
    pub left: Option<BorderPatch>,
    #[serde(default)]
    pub right: Option<BorderPatch>,
    #[serde(default)]
    pub between: Option<BorderPatch>,
    #[serde(default)]
    pub bar: Option<BorderPatch>,
}

impl ParaBordersPatch {
    fn is_empty(&self) -> bool {
        self.top.is_none()
            && self.bottom.is_none()
            && self.left.is_none()
            && self.right.is_none()
            && self.between.is_none()
            && self.bar.is_none()
    }
}

/// Wire shape for `set_cell_format.borders` — cell borders (`w:tcBorders`).
/// Maps onto the domain [`BorderSet`] (the table/cell border container, which
/// adds inside-horizontal/-vertical edges). An empty patch is treated as absent.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BorderSetPatch {
    #[serde(default)]
    pub top: Option<BorderPatch>,
    #[serde(default)]
    pub bottom: Option<BorderPatch>,
    #[serde(default)]
    pub left: Option<BorderPatch>,
    #[serde(default)]
    pub right: Option<BorderPatch>,
    #[serde(default)]
    pub inside_h: Option<BorderPatch>,
    #[serde(default)]
    pub inside_v: Option<BorderPatch>,
}

impl BorderSetPatch {
    fn is_empty(&self) -> bool {
        self.top.is_none()
            && self.bottom.is_none()
            && self.left.is_none()
            && self.right.is_none()
            && self.inside_h.is_none()
            && self.inside_v.is_none()
    }
}

/// Wire shape for a shading element (`w:shd`, §17.3.1.31 / §17.4.33). Maps onto
/// the domain [`Shading`]: `fill` is the background hex (or `auto`), `pattern`
/// is a `ST_Shd` token, `color` is the pattern hex.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShadingWire {
    #[serde(default)]
    pub fill: Option<String>,
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
}

impl ShadingWire {
    fn is_empty(&self) -> bool {
        self.fill.is_none() && self.pattern.is_none() && self.color.is_none()
    }
}

/// Wire shape for a table/cell width measurement (`w:tcW`/`w:tblW`). Maps onto
/// the domain [`TableMeasurement`]. `width_type` is a `ST_TblWidth` token
/// (`dxa` | `pct` | `auto` | `nil`).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasurementPatch {
    pub w: u32,
    pub width_type: String,
}

/// Wire shape for `set_cell_format.margins` — per-cell margin overrides
/// (`w:tcMar`, §17.4.41). Maps onto the domain [`CellMargins`] (twips).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CellMarginsPatch {
    #[serde(default)]
    pub top: Option<u32>,
    #[serde(default)]
    pub bottom: Option<u32>,
    #[serde(default)]
    pub left: Option<u32>,
    #[serde(default)]
    pub right: Option<u32>,
}

impl CellMarginsPatch {
    fn is_empty(&self) -> bool {
        self.top.is_none() && self.bottom.is_none() && self.left.is_none() && self.right.is_none()
    }
}

/// Wire shape of a paragraph-numbering operation, tagged by `kind`.
///
/// `synthesized_text` / `is_bullet` are caller-resolved derived values from the
/// document's numbering definitions; the engine does not carry those on the
/// `CanonDoc` value, so the caller supplies them rather than the engine
/// fabricating them (CLAUDE.md "no silent fallbacks").
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NumberingChangeWire {
    /// Attach/replace the list: set num_id + ilvl, optionally restart.
    SetList {
        num_id: u32,
        ilvl: u32,
        #[serde(default)]
        restart: bool,
        synthesized_text: String,
        #[serde(default)]
        is_bullet: bool,
    },
    /// Change only the indent level, keeping the current num_id.
    SetLevel {
        ilvl: u32,
        synthesized_text: String,
        #[serde(default)]
        is_bullet: bool,
    },
    /// Detach the list (set numbering to none).
    Remove,
    /// Indent one level (ilvl + 1). No caller-supplied fields: the engine keeps
    /// the current num_id/is_bullet and Word re-derives the label.
    Indent,
    /// Outdent one level (ilvl - 1). Same as `Indent`.
    Outdent,
    /// Restart the list counter here (restart_numbering = true).
    Restart,
    /// Continue the previous list run here (restart_numbering = false).
    Continue,
    /// Swap the list kind (bullet <-> numbered) by re-pointing at an EXISTING
    /// `num_id` of the target kind. The caller resolves `num_id` (+ derived
    /// `synthesized_text`/`is_bullet`) from the document's numbering.xml; if no
    /// list of the target kind exists, the caller fails loud rather than
    /// fabricating a definition (create-new-list-definition is deferred).
    SetType {
        num_id: u32,
        synthesized_text: String,
        #[serde(default)]
        is_bullet: bool,
    },
    /// Split the list at this item: the item and the contiguous following items
    /// at the same num_id/ilvl are re-pointed at a NEW num_id (whose definition
    /// the engine authors by cloning the source list's levels), so the tail
    /// renumbers from 1 independently. No caller-supplied fields: the engine
    /// allocates the new num_id and clones the definition itself; the caller
    /// drives it by reading the split item's `list.num_id` from the read surface
    /// and targeting that block. Refused on an unnumbered paragraph.
    Split,
}

impl SpacingPatch {
    fn is_empty(&self) -> bool {
        self.before.is_none() && self.after.is_none() && self.line.is_none()
    }
}

/// A positional anchor: a node id and a side (`before` or `after`). Used by
/// `insert.target` and `move.destination`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnchorTarget {
    pub anchor: NodeId,
    pub position: AnchorPosition,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnchorPosition {
    Before,
    After,
}

/// `move.target`: the content being relocated, either one block or a
/// contiguous inclusive range. An agent moving several blocks at once
/// (a section, a run of paragraphs) reaches for this shape naturally; before
/// it existed, the only way to move a range was several single-block moves
/// chained in one transaction, anchoring each hop on the previous hop's
/// source id — which is ambiguous (`AmbiguousAnchorAfterMove`) once the
/// first hop turns that id into a moveFrom shadow.
///
/// Untagged: a bare string is `Single`, an object with `from`/`to` is
/// `Range`. The two shapes are structurally disjoint, so there is no
/// ambiguity in which variant a given payload means.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum MoveTarget {
    Single(NodeId),
    /// Inclusive, either doc order — the engine reorders `from`/`to` by
    /// position, never refuses an inverted pair (same as `delete`'s range
    /// normalization).
    Range {
        from: NodeId,
        to: NodeId,
    },
}

/// `set_attr` payload. Parsed shape is permissive: each field is optional, and
/// the semantic-check layer verifies which fields are legal for the resolved
/// target kind (e.g. `href` is rejected on a paragraph target, `role` is
/// rejected on a hyperlink target).
///
/// This is the one place where we trade strict type discrimination for
/// wire-format clarity. The discriminator (target kind) is only known after
/// anchor resolution, which is a semantic-check concern by design (schema
/// validation and semantic validation are two distinct layers).
/// `deny_unknown_fields` still applies: it is
/// deliberately loose about which of `role`/`href`/`anchor`/`title` is legal
/// for a given target kind (that's the semantic-check layer's job), but a key
/// that is not ANY of them (e.g. a typo'd `titel`) is not a legality question —
/// it is a misnamed field, and must be rejected the same as anywhere else.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttrPatch {
    /// Paragraph role from the document vocabulary, e.g. `section_heading_h2`.
    /// Only legal on paragraph targets.
    #[serde(default)]
    pub role: Option<String>,
    /// Hyperlink href. Only legal on hyperlink targets.
    #[serde(default)]
    pub href: Option<String>,
    /// Hyperlink internal anchor. Only legal on hyperlink targets.
    #[serde(default)]
    pub anchor: Option<String>,
    /// Hyperlink tooltip. Only legal on hyperlink targets.
    #[serde(default)]
    pub title: Option<String>,
}

impl AttrPatch {
    /// True when no field is set. Empty `set_attr` payloads are rejected by
    /// the schema-check layer.
    pub fn is_empty(&self) -> bool {
        self.role.is_none() && self.href.is_none() && self.anchor.is_none() && self.title.is_none()
    }
}

// ─── Transaction envelope ────────────────────────────────────────────────────

/// The wire-level v4 edit transaction.
///
/// Same atomic semantics as v3: either every op applies or none do. The
/// `revision` block carries the user identity stamped onto every tracked
/// change the transaction produces; we deliberately do not default `author`.
/// `deny_unknown_fields`: a misnamed top-level key (e.g. `smmary`) is rejected
/// at the wire edge rather than silently dropped.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditTransactionV4 {
    #[serde(deserialize_with = "deserialize_ops_strict")]
    pub ops: Vec<Op>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub materialization_mode: MaterializationMode,
    pub revision: RevisionInfoV4,
}

/// Per-transaction revision metadata. Same shape as v3's
/// `edit::RevisionInfoRequest`; duplicated here so the v4 schema is
/// self-contained and the v3 surface can be removed without disturbing it.
/// `deny_unknown_fields`: a misnamed key (e.g. `authr`) is rejected at the wire
/// edge rather than silently dropped (which would leave `author` empty and, per
/// `parse_transaction`, still report success).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionInfoV4 {
    pub author: String,
    #[serde(default)]
    pub date: Option<String>,
    /// Group id stamped on every tracked change produced by this transaction.
    #[serde(default)]
    pub apply_op_id: Option<String>,
}

/// Strict per-op unknown-field guard for `EditTransactionV4.ops`.
///
/// `Op` is `#[serde(tag = "op")]` (internally tagged): serde's derive silently
/// ignores unknown keys inside a variant body under internal tagging (unlike
/// plain structs, where `#[serde(deny_unknown_fields)]` rejects them). A
/// container-level `deny_unknown_fields` on `Op` itself is not usable either:
/// two variants (`create_style`/`modify_style`) flatten [`StyleDefinitionWire`],
/// whose `Deserialize` is hand-written (see its doc comment) rather than
/// derived, so serde cannot statically enumerate its fields to reconcile with
/// an enum-level deny — doing so rejects every legitimate style field as
/// "unknown" (verified: this is a real regression, not a hypothetical).
///
/// So the guard lives here instead, at `Op`'s one and only deserialization
/// site: buffer each op to a `Value`, check its `op` tag and keys against
/// [`OP_FIELDS`] (rejecting both a misnamed field AND an `op` tag that isn't
/// in the table at all — see `check_op_fields`), and only then hand the
/// now-known-clean value to `Op`'s ordinary (lenient) derived `Deserialize`.
/// `Op` is not deserialized anywhere else in this codebase — if a future call
/// site deserializes `Op` directly, route it through this function too.
fn deserialize_ops_strict<'de, D>(deserializer: D) -> Result<Vec<Op>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;
    let raw = Vec::<serde_json::Value>::deserialize(deserializer)?;
    raw.into_iter()
        .map(|value| {
            check_op_fields(&value).map_err(D::Error::custom)?;
            serde_json::from_value::<Op>(value).map_err(D::Error::custom)
        })
        .collect()
}

/// Reject a key on a buffered op `Value` that isn't in [`OP_FIELDS`]'s
/// allowlist for that op's tag, AND reject an `op` tag that isn't in
/// [`OP_FIELDS`] at all. The latter is the drift guard: without it, an `Op`
/// variant added to the enum but forgotten in `OP_FIELDS` would silently
/// bypass this whole function (an unrecognized tag would just fall through to
/// `Op`'s own derived `Deserialize`, which is correct today but means the
/// unknown-field guard quietly stops applying to that variant — the exact
/// silent-drop shape this module exists to prevent). So an unrecognized
/// STRING tag is a hard error here, not a pass-through.
///
/// Still deliberately permissive about the cases that aren't a table-drift
/// question at all: a non-object value, or a missing/non-string `op` key.
/// Those are left for `Op`'s own derived `Deserialize` to report, which
/// already produces a fine actionable message for them (e.g. "missing field
/// `op`").
fn check_op_fields(value: &serde_json::Value) -> Result<(), String> {
    let Some(obj) = value.as_object() else {
        return Ok(());
    };
    let Some(tag) = obj.get("op").and_then(serde_json::Value::as_str) else {
        return Ok(());
    };
    let Some(&(_, allowed)) = OP_FIELDS.iter().find(|(t, _)| *t == tag) else {
        let known: Vec<&str> = OP_FIELDS.iter().map(|(t, _)| *t).collect();
        return Err(format!(
            "unknown op `{tag}`; expected one of: {}",
            known.join(", ")
        ));
    };
    for key in obj.keys() {
        if key == "op" || allowed.contains(&key.as_str()) {
            continue;
        }
        return Err(format!(
            "op `{tag}`: unknown field `{key}`; expected one of: op, {}",
            allowed.join(", ")
        ));
    }
    Ok(())
}

/// The set of top-level keys each `op` tag value accepts, keyed by the wire tag
/// string (`#[serde(tag = "op", rename_all = "snake_case")]`, with `move`'s
/// explicit `#[serde(rename = "move")]`). `"op"` itself is always implicitly
/// allowed (checked separately in [`check_op_fields`]) and omitted here. Kept
/// in the same order as the `Op` enum so a new variant is easy to place.
///
/// `create_style`/`modify_style` list [`StyleDefinitionWire`]'s flattened
/// fields directly (`style_id`, `style_type`, `based_on`, `name`, `run_props`,
/// `para_props`) plus `rationale` — those two ops have no fields of their own
/// beyond the flattened definition.
const OP_FIELDS: &[(&str, &[&str])] = &[
    (
        "replace",
        &[
            "target",
            "content",
            "span",
            "expect",
            "guard",
            "semantic_hash",
            "rationale",
        ],
    ),
    ("insert", &["target", "content", "rationale"]),
    (
        "delete",
        &["target", "expect", "guard", "semantic_hash", "rationale"],
    ),
    (
        "move",
        &[
            "target",
            "destination",
            "expect",
            "guard",
            "semantic_hash",
            "rationale",
        ],
    ),
    (
        "set_attr",
        &[
            "target",
            "attrs",
            "expect_href",
            "expect_anchor",
            "rationale",
        ],
    ),
    (
        "set_format",
        &[
            "target",
            "expect",
            "semantic_hash",
            "marks",
            "color",
            "highlight",
            "font_family",
            "font_size_half_points",
            "caps",
            "small_caps",
            "char_spacing",
            "rationale",
        ],
    ),
    (
        "set_para_format",
        &[
            "target",
            "semantic_hash",
            "align",
            "indent",
            "spacing",
            "borders",
            "shading",
            "rationale",
        ],
    ),
    (
        "set_cell_format",
        &[
            "target",
            "semantic_hash",
            "row_index",
            "col_index",
            "borders",
            "shading",
            "width",
            "v_align",
            "margins",
            "rationale",
        ],
    ),
    (
        "set_row_format",
        &[
            "target",
            "semantic_hash",
            "row_index",
            "height",
            "height_rule",
            "rationale",
        ],
    ),
    (
        "insert_cross_ref",
        &[
            "target",
            "expect",
            "semantic_hash",
            "bookmark",
            "ref_kind",
            "as_hyperlink",
            "no_paragraph_number",
            "paragraph_number_relative",
            "paragraph_number_full",
            "above_below",
            "rationale",
        ],
    ),
    (
        "set_numbering",
        &["target", "change", "semantic_hash", "rationale"],
    ),
    (
        "insert_bookmark",
        &["target", "expect", "name", "semantic_hash", "rationale"],
    ),
    (
        "rename_bookmark",
        &[
            "target",
            "old_name",
            "new_name",
            "semantic_hash",
            "rationale",
        ],
    ),
    (
        "remove_bookmark",
        &["target", "name", "semantic_hash", "rationale"],
    ),
    (
        "apply_style",
        &["target", "style_id", "semantic_hash", "rationale"],
    ),
    (
        "set_image_attrs",
        &[
            "target",
            "drawing_id",
            "semantic_hash",
            "resize",
            "alt_text",
            "rationale",
        ],
    ),
    (
        "delete_image",
        &["target", "drawing_id", "semantic_hash", "rationale"],
    ),
    (
        "comment_create",
        &[
            "target",
            "expect",
            "body",
            "author",
            "semantic_hash",
            "rationale",
        ],
    ),
    (
        "comment_reply",
        &["parent_comment_id", "body", "author", "rationale"],
    ),
    ("comment_resolve", &["comment_id", "done", "rationale"]),
    ("comment_delete", &["comment_id", "rationale"]),
    (
        "insert_note",
        &[
            "target",
            "expect",
            "note_kind",
            "body",
            "semantic_hash",
            "rationale",
        ],
    ),
    ("edit_note", &["note_id", "note_kind", "body", "rationale"]),
    ("delete_note", &["note_id", "note_kind", "rationale"]),
    (
        "set_page_setup",
        &[
            "target",
            "page_size",
            "orientation",
            "margins",
            "columns",
            "gutter",
            "semantic_hash",
            "rationale",
        ],
    ),
    (
        "set_section_type",
        &["target", "section_type", "semantic_hash", "rationale"],
    ),
    (
        "insert_section_break",
        &[
            "anchor",
            "section_type",
            "page_size",
            "orientation",
            "margins",
            "columns",
            "gutter",
            "rationale",
        ],
    ),
    (
        "edit_header",
        &[
            "header_part",
            "target",
            "expect",
            "content",
            "semantic_hash",
            "rationale",
        ],
    ),
    (
        "edit_footer",
        &[
            "footer_part",
            "target",
            "expect",
            "content",
            "semantic_hash",
            "rationale",
        ],
    ),
    ("create_header", &["kind", "rationale"]),
    ("create_footer", &["kind", "rationale"]),
    (
        "set_header_footer_mode",
        &["title_page", "even_and_odd", "link", "rationale"],
    ),
    (
        "insert_equation",
        &[
            "target",
            "expect",
            "semantic_hash",
            "omml",
            "placement",
            "rationale",
        ],
    ),
    (
        "blocks_to_table",
        &["from", "to", "delimiter", "header", "rationale"],
    ),
    (
        "wrap_content_control",
        &[
            "target",
            "expect",
            "semantic_hash",
            "tag",
            "alias",
            "control",
            "data_binding",
            "rationale",
        ],
    ),
    (
        "set_content_control_value",
        &[
            "target",
            "sdt_id",
            "text",
            "checked",
            "selected",
            "tracked",
            "rationale",
        ],
    ),
    (
        "set_form_field_value",
        &[
            "target",
            "field_id",
            "text",
            "checked",
            "selected",
            "semantic_hash",
            "rationale",
        ],
    ),
    (
        "insert_image",
        &[
            "target",
            "bytes_base64",
            "format",
            "cx",
            "cy",
            "alt_text",
            "expect",
            "semantic_hash",
            "rationale",
        ],
    ),
    (
        "replace_image",
        &[
            "target",
            "drawing_id",
            "bytes_base64",
            "format",
            "cx",
            "cy",
            "alt_text",
            "allow_stretch",
            "semantic_hash",
            "rationale",
        ],
    ),
    (
        "set_textbox_text",
        &[
            "target",
            "drawing_id",
            "paragraphs",
            "semantic_hash",
            "rationale",
        ],
    ),
    (
        "opaque_text_edit",
        &[
            "target",
            "opaque_id",
            "find",
            "replacement",
            "container_index",
            "paragraph_index",
            "semantic_hash",
            "rationale",
        ],
    ),
    (
        "sdt_text_fill",
        &[
            "block_id",
            "sdt_id",
            "body_index",
            "value",
            "semantic_hash",
            "rationale",
        ],
    ),
    (
        "create_style",
        &[
            "style_id",
            "style_type",
            "based_on",
            "name",
            "run_props",
            "para_props",
            "rationale",
        ],
    ),
    (
        "modify_style",
        &[
            "style_id",
            "style_type",
            "based_on",
            "name",
            "run_props",
            "para_props",
            "rationale",
        ],
    ),
    (
        "set_doc_defaults",
        &["font_family", "font_size_half_points", "rationale"],
    ),
    (
        "table_op",
        &["target", "semantic_hash", "table_op", "rationale"],
    ),
    (
        "set_table_format",
        &[
            "target",
            "semantic_hash",
            "borders",
            "width",
            "default_cell_margins",
            "rationale",
        ],
    ),
    (
        "set_image_layout",
        &[
            "target",
            "drawing_id",
            "semantic_hash",
            "position_h",
            "position_v",
            "wrap",
            "crop",
            "rationale",
        ],
    ),
    (
        "wrap_blocks_content_control",
        &[
            "start_block",
            "end_block",
            "tag",
            "alias",
            "control",
            "rationale",
        ],
    ),
    // ─── add new wire ops above ──────────────────────────────────────────────
    // Mirror a new `Op` variant's field list here. See `edit/AGENTS.md`.
];

/// The complete wire vocabulary accepted by [`parse_transaction`].
///
/// This is intentionally a read-only view over the same table that enforces
/// fail-loud unknown-field rejection. MCP and other typed edges use it for
/// capability discovery, so the advertised operation names and fields cannot
/// drift from the parser's authoritative vocabulary.
pub fn operation_vocabulary() -> &'static [(&'static str, &'static [&'static str])] {
    OP_FIELDS
}

/// The engine-owned operation catalog: [`OP_FIELDS`] decorated with groups,
/// cues, and canonical parse-valid shapes. Every transport and the generated
/// reference page project this one catalog.
pub mod catalog;

// ─── Schema validation (parse step) ──────────────────────────────────────────
//
// The schema check verifies structural well-formedness of an already-deserialized
// transaction. Serde's typed parse covers content-expression conformance
// (invariant 3): a `Vec<Inline>` only accepts `Inline`-shaped objects, so a
// paragraph cannot contain a table at parse time. The checks below cover the
// well-formedness gaps that the type system can't express directly:
//
// - ops list is non-empty
// - insert.content is non-empty
// - hyperlinks carry at least one of `href` / `anchor`
// - set_attr.attrs sets at least one field
//
// Invariants that require resolving anchors against the live document live in
// the semantic-check layer (invariants 1, 2, 6). Invariant 5 (no
// LLM-minted ids) is enforced by construction: no fresh-node type in this
// module carries an `id` field. The only id-bearing node is `OpaqueRef`,
// which is a reference, not a fresh node.

/// A schema-level validation failure. Carries a `path` so the caller knows
/// where in the transaction the problem is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchemaError {
    /// JSON parse failure surfaced from serde (line/col in `message`).
    JsonParseError { message: String },

    /// The transaction has no ops.
    EmptyOps,

    /// `insert.content` is an empty list. Per the schema, an insert with no
    /// blocks has no effect and is almost certainly a programmer error;
    /// failing loudly preserves "no silent fallbacks".
    EmptyInsertContent { op_index: usize },

    /// A hyperlink carries neither `href` nor `anchor`. At least one must be
    /// set — a hyperlink with no target is meaningless.
    HyperlinkHasNoTarget { path: NodePath },

    /// `set_attr.attrs` sets no fields. An empty attribute patch has no
    /// effect; failing loudly catches the case where a caller forgot to
    /// include the change.
    EmptyAttrPatch { op_index: usize },
    /// A `set_format` op supplied no marks.
    EmptyFormatMarks { op_index: usize },
    /// A `set_para_format` op set no alignment, indentation, spacing, borders,
    /// or shading.
    EmptyParaFormat { op_index: usize },
    /// A `set_cell_format` op set no borders, shading, width, vertical
    /// alignment, or margins.
    EmptyCellFormat { op_index: usize },
    /// A `set_row_format` op set no height or height rule.
    EmptyRowFormat { op_index: usize },
    /// A `set_table_format` op set no borders, width, or default cell margins.
    EmptyTableFormat { op_index: usize },
    /// An `insert_cross_ref` op carried an empty `bookmark`.
    CrossRefEmptyBookmark { op_index: usize },
    /// An `insert_cross_ref` op carried a `ref_kind` not in `ref`/`pageref`/`noref`.
    UnknownRefKind { op_index: usize, value: String },
    /// A `set_numbering` op requested a non-bullet level with empty
    /// `synthesized_text` (the caller failed to resolve the counter text).
    EmptyNumberingText { op_index: usize },
    /// An `insert_bookmark` / `rename_bookmark` op carried an empty bookmark
    /// name. A nameless bookmark is unreferenceable; refused rather than
    /// defaulted.
    BookmarkEmptyName { op_index: usize },
    /// An `apply_style` op carried an empty `style_id`. A `w:pStyle` with no
    /// `w:val` is meaningless; we refuse rather than default it.
    EmptyStyleId { op_index: usize },

    /// A `replace` payload's root node is a kind that is not addressable.
    /// `replace` accepts paragraph, table, and hyperlink payloads. `text` and
    /// `opaque_ref` payloads are rejected by this schema check.
    UnaddressableReplaceContent { op_index: usize, kind: &'static str },

    /// The same `opaque_ref` id appears more than once in a single payload's
    /// subtree. The engine's set-equality invariant (named invariant 2) is
    /// over a *set*; structurally, a duplicate would mean the same opaque
    /// node was placed in two positions, which is meaningless and almost
    /// certainly a caller bug.
    DuplicateOpaqueRefInPayload { op_index: usize, opaque_id: String },

    /// A `Block::Table` carries an empty `content` (no rows). A table
    /// with no rows is structurally meaningless and would crash the
    /// engine's diff machinery, which assumes at least one row. Failing
    /// at the schema layer keeps the engine's invariants honest.
    EmptyTableRows { path: NodePath },

    /// A `TableRow` carries an empty `content` (no cells). Same
    /// reasoning as `EmptyTableRows` — a row with no cells is not a
    /// valid OOXML row.
    EmptyTableRowCells { path: NodePath },

    /// A `TableCell` carries an empty `content` (no blocks). OOXML
    /// requires every cell to contain at least one paragraph; an empty
    /// cell would fail validation.
    EmptyTableCellBlocks { path: NodePath },

    /// A `TableCell` carries `attrs.grid_span = Some(0)`. A horizontal merge
    /// must span at least one grid column (`w:gridSpan` ≥ 1, §17.4.17); zero is
    /// structurally meaningless. Caught at the schema layer so the engine never
    /// sees a degenerate gridSpan. (Ragged grids and orphan `vMerge=continue`
    /// are deeper, cross-row constraints validated by the engine's
    /// `validate_merge_spec`, which emits `RaggedTableGrid` /
    /// `OrphanVMergeContinue`.)
    ZeroGridSpan { path: NodePath },

    /// An inserted/replaced paragraph's `list.ilvl` is outside 0..=8 (`w:ilvl`,
    /// §17.9.3). OOXML defines nine list levels; a level above 8 is structurally
    /// meaningless. Refused at the wire edge rather than clamped.
    InsertListLevelOutOfBounds { path: NodePath, ilvl: u32 },

    /// A `Block::Toc.levels` pair violates `1 <= from <= to <= 9`. Word's TOC
    /// field switch `\o "from-to"` (§17.16.5.68) addresses nine heading
    /// levels; an inverted or out-of-range pair is structurally meaningless.
    /// Refused at the wire edge rather than clamped.
    TocLevelsOutOfBounds { path: NodePath, from: u8, to: u8 },

    /// A `replace` op's content was a `toc` block. Day-one scope: a ToC can
    /// only be authored via `insert` (it has no prior content to replace
    /// against — there is no meaningful "replace this ToC's text").
    TocNotReplaceable { op_index: usize },

    /// A `toc` block appeared inside a table cell's content. Day-one scope:
    /// ToC insertion is top-level only.
    TocNotAllowedInTableCell { path: NodePath },

    /// A `set_image_attrs` op requested neither a resize nor an alt-text edit.
    EmptyImageAttrs { op_index: usize },

    /// A `set_image_attrs` resize carried a negative `cx` or `cy`. EMU
    /// dimensions are non-negative (`wp:extent` is a `ST_PositiveCoordinate`,
    /// §20.4.3.6); a negative box is structurally meaningless.
    NegativeImageDimension {
        op_index: usize,
        axis: &'static str,
        value: i64,
    },

    /// A `set_image_layout` op requested no crop/position/wrap — a no-op.
    EmptyImageLayout { op_index: usize },

    /// A `set_image_layout` position axis set neither `offset` nor `align`, or
    /// set both. Exactly one is required (`wp:posOffset` XOR `wp:align`).
    ImageLayoutPositionAmbiguous { op_index: usize, axis: &'static str },

    /// A `set_image_layout` crop edge inset is outside `0..=100000` (1000ths of
    /// a percent, `ST_Percentage`). A crop beyond 100% is meaningless.
    ImageLayoutCropOutOfRange {
        op_index: usize,
        edge: &'static str,
        value: i32,
    },

    /// A `set_image_layout` `wrap` token is not one of `none` / `square` /
    /// `tight` / `through` / `top_and_bottom`. Refused rather than defaulted.
    ImageLayoutUnknownWrap { op_index: usize, token: String },

    /// A `comment_create` / `comment_reply` op carried an empty `body`. A
    /// comment with no text is meaningless; refused rather than defaulted.
    CommentEmptyBody { op_index: usize },

    /// A `comment_reply` op carried an empty `parent_comment_id`. Without a
    /// parent there is nothing to thread under.
    CommentMissingParent { op_index: usize },

    /// A `comment_resolve` / `comment_delete` op carried an empty `comment_id`.
    CommentMissingId { op_index: usize },

    /// An `insert_note` op carried an empty `expect` anchor. Without it the
    /// reference run has no insertion point.
    NoteEmptyExpect { op_index: usize },

    /// An `insert_note` / `edit_note` op carried an empty `body`. A note with no
    /// text is meaningless; refused rather than defaulted.
    NoteEmptyBody { op_index: usize },

    /// An `edit_note` / `delete_note` op carried an empty `note_id`.
    NoteMissingId { op_index: usize },

    /// A `set_page_setup` op carried no page-setup fields (empty patch).
    EmptyPageSetup { op_index: usize },

    /// An `edit_header` / `edit_footer` op carried an empty `header_part` /
    /// `footer_part` or `expect`. Without the part name the story is
    /// unaddressable; without `expect` the edit has no anchor.
    HeaderFooterEmptyField {
        op_index: usize,
        field: &'static str,
    },

    /// A `set_header_footer_mode` op requested no change (no `title_page`,
    /// `even_and_odd`, or `link`).
    EmptyHeaderFooterMode { op_index: usize },

    /// An `insert_equation` op carried an empty `expect` anchor.
    EquationEmptyExpect { op_index: usize },

    /// An `insert_equation` op carried an empty `omml` fragment. An equation
    /// with no math markup is meaningless; refused rather than defaulted.
    EquationEmptyOmml { op_index: usize },

    /// An `insert_equation` op carried a `placement` not in `inline`/`block`.
    UnknownEquationPlacement { op_index: usize, value: String },

    /// A `blocks_to_table` op carried an empty `delimiter`. Cells can't be split
    /// without one; refused rather than defaulted.
    BlocksToTableEmptyDelimiter { op_index: usize },

    /// A `blocks_to_table` op supplied a `header` with no cells. An empty header
    /// can't fix a column count; refused rather than defaulted.
    BlocksToTableEmptyHeader { op_index: usize },

    /// A `wrap_content_control` op carried an empty `expect` anchor.
    ContentControlEmptyExpect { op_index: usize },

    /// A `wrap_content_control` op carried no distinguishing data (no tag, no
    /// alias, default rich-text control). Refused rather than authored.
    EmptyContentControlSpec { op_index: usize },

    /// A `wrap_content_control` op carried a `data_binding` with an empty
    /// `xpath` or empty `store_item_id`. A binding with no target is
    /// unresolvable; refused at the wire edge rather than authored.
    MalformedDataBinding {
        op_index: usize,
        reason: &'static str,
    },

    /// A `set_content_control_value` op set other than exactly one of
    /// `text` / `checked` / `selected`. `present` is how many were set.
    ContentControlValueArity { op_index: usize, present: u8 },

    /// A `set_form_field_value` op set other than exactly one of
    /// `text` / `checked` / `selected`. `present` is how many were set.
    FormFieldValueArity { op_index: usize, present: u8 },

    /// An `insert_image` / `replace_image` op carried empty `bytes_base64`. An
    /// image with no bytes is meaningless; refused rather than authored.
    ImageBytesEmpty { op_index: usize },

    /// A `create_style` / `modify_style` op carried an empty `style_id`. A
    /// `w:style` with no `w:styleId` is unaddressable; refused.
    StyleDefEmptyId { op_index: usize },

    /// A `create_style` / `modify_style` op carried an empty `name`. A usable
    /// `w:style` needs a `w:name`; refused rather than defaulted.
    StyleDefEmptyName { op_index: usize },

    /// A `set_doc_defaults` op carried neither `font_family` nor
    /// `font_size_half_points`. An op that would set nothing is refused rather
    /// than authored as a no-op.
    DocDefaultsEmpty { op_index: usize },

    /// A `replace` op carried a sub-block `span` selector but the payload is not
    /// a paragraph (table / hyperlink). Span addressing is paragraph-only; a
    /// span on another target is meaningless and refused rather than ignored.
    SpanOnNonParagraph { op_index: usize, kind: &'static str },

    /// A `replace` op carried a sub-block `span` selector without a block
    /// guard (`guard`/`semantic_hash`). The guard is REQUIRED for span ops:
    /// it is what makes the ephemeral span handle safe, and what refuses the
    /// second of two same-paragraph ops in one transaction.
    SpanRequiresGuard { op_index: usize },

    /// An op supplied BOTH `guard` and `semantic_hash`, and they disagree.
    /// `guard` is the spec alias for `semantic_hash`; when both are present they
    /// must be equal. We reject rather than silently choosing one (CLAUDE.md:
    /// no silent fallbacks).
    ConflictingGuard {
        op_index: usize,
        guard: String,
        semantic_hash: String,
    },
}

impl std::fmt::Display for SchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SchemaError::JsonParseError { message } => write!(f, "json parse error: {message}"),
            SchemaError::EmptyOps => write!(f, "transaction has no ops"),
            SchemaError::EmptyInsertContent { op_index } => {
                write!(f, "ops[{op_index}]: insert.content is empty")
            }
            SchemaError::HyperlinkHasNoTarget { path } => write!(
                f,
                "{path}: hyperlink must carry at least one of `href` or `anchor`"
            ),
            SchemaError::EmptyAttrPatch { op_index } => {
                write!(f, "ops[{op_index}]: set_attr.attrs sets no fields")
            }
            SchemaError::EmptyFormatMarks { op_index } => {
                write!(f, "ops[{op_index}]: set_format.marks is empty")
            }
            SchemaError::EmptyParaFormat { op_index } => write!(
                f,
                "ops[{op_index}]: set_para_format sets no alignment, indentation, \
                 spacing, borders, or shading"
            ),
            SchemaError::EmptyCellFormat { op_index } => write!(
                f,
                "ops[{op_index}]: set_cell_format sets no borders, shading, width, \
                 vertical alignment, or margins"
            ),
            SchemaError::EmptyRowFormat { op_index } => write!(
                f,
                "ops[{op_index}]: set_row_format sets no height or height rule"
            ),
            SchemaError::EmptyTableFormat { op_index } => write!(
                f,
                "ops[{op_index}]: set_table_format sets no borders, width, or default \
                 cell margins"
            ),
            SchemaError::CrossRefEmptyBookmark { op_index } => write!(
                f,
                "ops[{op_index}]: insert_cross_ref.bookmark is empty; a REF/PAGEREF \
                 field needs a bookmark target"
            ),
            SchemaError::UnknownRefKind { op_index, value } => write!(
                f,
                "ops[{op_index}]: insert_cross_ref.ref_kind '{value}' is not one of \
                 `ref`, `pageref`, `noref`"
            ),
            SchemaError::EmptyNumberingText { op_index } => write!(
                f,
                "ops[{op_index}]: set_numbering requested a non-bullet level with empty \
                 synthesized_text; resolve the counter text against the numbering definitions"
            ),
            SchemaError::BookmarkEmptyName { op_index } => write!(
                f,
                "ops[{op_index}]: bookmark name is empty; a bookmark needs a non-empty name"
            ),
            SchemaError::EmptyStyleId { op_index } => write!(
                f,
                "ops[{op_index}]: apply_style.style_id is empty; a w:pStyle needs a style id"
            ),
            SchemaError::UnaddressableReplaceContent { op_index, kind } => write!(
                f,
                "ops[{op_index}]: replace payload of kind `{kind}` is not addressable \
                 (paragraph, table, or hyperlink only)"
            ),
            SchemaError::DuplicateOpaqueRefInPayload {
                op_index,
                opaque_id,
            } => write!(
                f,
                "ops[{op_index}]: opaque_ref id `{opaque_id}` appears more than once in payload"
            ),
            SchemaError::EmptyTableRows { path } => write!(
                f,
                "{path}: table has no rows; every table must have at least one row"
            ),
            SchemaError::EmptyTableRowCells { path } => write!(
                f,
                "{path}: table row has no cells; every row must have at least one cell"
            ),
            SchemaError::EmptyTableCellBlocks { path } => write!(
                f,
                "{path}: table cell has no content; every cell must contain at least one block"
            ),
            SchemaError::ZeroGridSpan { path } => write!(
                f,
                "{path}: table cell grid_span is 0; a horizontal merge must span at least one column"
            ),
            SchemaError::InsertListLevelOutOfBounds { path, ilvl } => write!(
                f,
                "{path}: paragraph list.ilvl is {ilvl}; OOXML list levels are 0..=8"
            ),
            SchemaError::TocLevelsOutOfBounds { path, from, to } => write!(
                f,
                "{path}: toc levels {{from:{from},to:{to}}} invalid; require 1 <= from <= to <= 9"
            ),
            SchemaError::TocNotReplaceable { op_index } => write!(
                f,
                "ops[{op_index}]: a toc block can only be inserted (op: \"insert\"), not replaced"
            ),
            SchemaError::TocNotAllowedInTableCell { path } => write!(
                f,
                "{path}: a toc block cannot appear inside a table cell (top-level insert only)"
            ),
            SchemaError::EmptyImageAttrs { op_index } => write!(
                f,
                "ops[{op_index}]: set_image_attrs requested neither a resize nor an alt-text edit"
            ),
            SchemaError::NegativeImageDimension {
                op_index,
                axis,
                value,
            } => write!(
                f,
                "ops[{op_index}]: set_image_attrs resize {axis} is {value}; \
                 EMU dimensions must be non-negative"
            ),
            SchemaError::EmptyImageLayout { op_index } => write!(
                f,
                "ops[{op_index}]: set_image_layout requested no crop/position/wrap"
            ),
            SchemaError::ImageLayoutPositionAmbiguous { op_index, axis } => write!(
                f,
                "ops[{op_index}]: set_image_layout position {axis} must set exactly one of \
                 offset / align (got both or neither)"
            ),
            SchemaError::ImageLayoutCropOutOfRange {
                op_index,
                edge,
                value,
            } => write!(
                f,
                "ops[{op_index}]: set_image_layout crop {edge} is {value}; \
                 srcRect insets are 1000ths of a percent in 0..=100000"
            ),
            SchemaError::ImageLayoutUnknownWrap { op_index, token } => write!(
                f,
                "ops[{op_index}]: set_image_layout wrap '{token}' is not one of \
                 none/square/tight/through/top_and_bottom"
            ),
            SchemaError::CommentEmptyBody { op_index } => write!(
                f,
                "ops[{op_index}]: comment body is empty; a comment needs non-empty text"
            ),
            SchemaError::CommentMissingParent { op_index } => write!(
                f,
                "ops[{op_index}]: comment_reply has an empty parent_comment_id"
            ),
            SchemaError::CommentMissingId { op_index } => {
                write!(f, "ops[{op_index}]: comment op has an empty comment_id")
            }
            SchemaError::NoteEmptyExpect { op_index } => {
                write!(f, "ops[{op_index}]: insert_note has an empty expect anchor")
            }
            SchemaError::NoteEmptyBody { op_index } => write!(
                f,
                "ops[{op_index}]: note body is empty; a note needs non-empty text"
            ),
            SchemaError::NoteMissingId { op_index } => {
                write!(f, "ops[{op_index}]: note op has an empty note_id")
            }
            SchemaError::EmptyPageSetup { op_index } => write!(
                f,
                "ops[{op_index}]: set_page_setup sets no page size, orientation, \
                 margins, columns, or gutter"
            ),
            SchemaError::HeaderFooterEmptyField { op_index, field } => write!(
                f,
                "ops[{op_index}]: edit_header/edit_footer has an empty {field}"
            ),
            SchemaError::EmptyHeaderFooterMode { op_index } => write!(
                f,
                "ops[{op_index}]: set_header_footer_mode requests no change \
                 (no title_page, even_and_odd, or link)"
            ),
            SchemaError::EquationEmptyExpect { op_index } => write!(
                f,
                "ops[{op_index}]: insert_equation has an empty expect anchor"
            ),
            SchemaError::EquationEmptyOmml { op_index } => write!(
                f,
                "ops[{op_index}]: insert_equation has an empty omml fragment"
            ),
            SchemaError::UnknownEquationPlacement { op_index, value } => write!(
                f,
                "ops[{op_index}]: insert_equation.placement '{value}' is not `inline` or `block`"
            ),
            SchemaError::BlocksToTableEmptyDelimiter { op_index } => {
                write!(f, "ops[{op_index}]: blocks_to_table has an empty delimiter")
            }
            SchemaError::BlocksToTableEmptyHeader { op_index } => write!(
                f,
                "ops[{op_index}]: blocks_to_table has a header with no cells"
            ),
            SchemaError::ContentControlEmptyExpect { op_index } => write!(
                f,
                "ops[{op_index}]: wrap_content_control has an empty expect anchor"
            ),
            SchemaError::EmptyContentControlSpec { op_index } => write!(
                f,
                "ops[{op_index}]: wrap_content_control has no distinguishing data \
                 (no tag, no alias, default rich-text control)"
            ),
            SchemaError::MalformedDataBinding { op_index, reason } => write!(
                f,
                "ops[{op_index}]: wrap_content_control.data_binding is malformed ({reason})"
            ),
            SchemaError::ContentControlValueArity { op_index, present } => write!(
                f,
                "ops[{op_index}]: set_content_control_value must set exactly one of \
                 text/checked/selected (got {present})"
            ),
            SchemaError::FormFieldValueArity { op_index, present } => write!(
                f,
                "ops[{op_index}]: set_form_field_value must set exactly one of \
                 text/checked/selected (got {present})"
            ),
            SchemaError::ImageBytesEmpty { op_index } => write!(
                f,
                "ops[{op_index}]: image bytes_base64 is empty; an image needs binary bytes"
            ),
            SchemaError::StyleDefEmptyId { op_index } => write!(
                f,
                "ops[{op_index}]: style definition style_id is empty; a w:style needs a w:styleId"
            ),
            SchemaError::StyleDefEmptyName { op_index } => write!(
                f,
                "ops[{op_index}]: style definition name is empty; a usable w:style needs a w:name"
            ),
            SchemaError::DocDefaultsEmpty { op_index } => write!(
                f,
                "ops[{op_index}]: set_doc_defaults set neither font_family nor \
                 font_size_half_points; refusing a no-op docDefaults edit"
            ),
            SchemaError::SpanOnNonParagraph { op_index, kind } => write!(
                f,
                "ops[{op_index}]: a sub-block `span` selector is paragraph-only; \
                 the replace payload is `{kind}`"
            ),
            SchemaError::SpanRequiresGuard { op_index } => write!(
                f,
                "ops[{op_index}]: a sub-block `span` replace requires the block `guard` \
                 (the guard from the latest read of the block); the guard is what makes \
                 an ephemeral span handle safe"
            ),
            SchemaError::ConflictingGuard {
                op_index,
                guard,
                semantic_hash,
            } => write!(
                f,
                "ops[{op_index}]: `guard` ({guard}) and `semantic_hash` ({semantic_hash}) \
                 disagree; they are aliases and must be equal (supply only one)"
            ),
        }
    }
}

/// Reconcile the `guard` alias with `semantic_hash` on a guarded op.
///
/// `guard` (spec name) and `semantic_hash` (legacy name) are the same staleness
/// token. A caller supplies at most one; supplying both is allowed only when they
/// are byte-equal. Returns the single effective guard, or `ConflictingGuard` when
/// both are present and differ — never silently picks one.
fn reconcile_guard(
    op_index: usize,
    guard: Option<String>,
    semantic_hash: Option<String>,
) -> Result<Option<String>, SchemaError> {
    match (guard, semantic_hash) {
        (Some(g), Some(h)) if g != h => Err(SchemaError::ConflictingGuard {
            op_index,
            guard: g,
            semantic_hash: h,
        }),
        (Some(g), Some(_)) => Ok(Some(g)),
        (Some(g), None) => Ok(Some(g)),
        (None, Some(h)) => Ok(Some(h)),
        (None, None) => Ok(None),
    }
}

impl std::error::Error for SchemaError {}

/// A path through a transaction, accumulated during the validation walk.
/// Renders as e.g. `ops[0].content.paragraph.content[3].hyperlink`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodePath(Vec<NodePathSegment>);

#[derive(Clone, Debug, PartialEq, Eq)]
enum NodePathSegment {
    OpIndex(usize),
    Field(&'static str),
    Index(usize),
}

impl NodePath {
    fn pushed(&self, segment: NodePathSegment) -> Self {
        let mut next = self.0.clone();
        next.push(segment);
        NodePath(next)
    }
    fn op(idx: usize) -> Self {
        NodePath(vec![NodePathSegment::OpIndex(idx)])
    }
}

impl std::fmt::Display for NodePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, seg) in self.0.iter().enumerate() {
            match seg {
                NodePathSegment::OpIndex(idx) => write!(f, "ops[{idx}]")?,
                NodePathSegment::Field(name) => {
                    if i == 0 {
                        write!(f, "{name}")?;
                    } else {
                        write!(f, ".{name}")?;
                    }
                }
                NodePathSegment::Index(idx) => write!(f, "[{idx}]")?,
            }
        }
        Ok(())
    }
}

/// Parse a JSON transaction and run the schema-check layer.
///
/// This is the entry point for the v4 wire format: it produces a fully
/// validated (at the schema layer) `EditTransactionV4`. The semantic checks
/// (invariants 1, 2, 6 and the target-resolution stages) run later in the
/// pipeline, once the live document is available.
pub fn parse_transaction(json: &str) -> Result<EditTransactionV4, SchemaError> {
    let txn: EditTransactionV4 =
        serde_json::from_str(json).map_err(|e| SchemaError::JsonParseError {
            message: e.to_string(),
        })?;
    validate_schema(&txn)?;
    Ok(txn)
}

/// Walk an already-parsed transaction and enforce the schema-layer invariants
/// that the type system can't express directly.
pub fn validate_schema(txn: &EditTransactionV4) -> Result<(), SchemaError> {
    if txn.ops.is_empty() {
        return Err(SchemaError::EmptyOps);
    }
    for (op_index, op) in txn.ops.iter().enumerate() {
        match op {
            Op::Replace {
                content,
                span,
                guard,
                semantic_hash,
                ..
            } => {
                // `guard`/`semantic_hash` are aliases: reject a conflicting pair
                // at the schema edge before translation.
                reconcile_guard(op_index, guard.clone(), semantic_hash.clone())?;
                // Day-one scope: a toc block has no prior content to replace
                // against — it is insert-only. Reject before the span check
                // below so a span-on-toc gets this message, not the generic
                // `SpanOnNonParagraph`.
                if matches!(content, ReplaceContent::Block(Block::Toc { .. })) {
                    return Err(SchemaError::TocNotReplaceable { op_index });
                }
                // A sub-block span is paragraph-only. Reject it on a
                // table/hyperlink/text/opaque payload before translation.
                if let Some(sel) = span {
                    let is_whole = matches!(sel, SpanSelector::Token(t) if t == "whole");
                    if !is_whole {
                        let kind = match content {
                            ReplaceContent::Block(Block::Paragraph { .. }) => None,
                            ReplaceContent::Block(Block::Table { .. }) => Some("table"),
                            // Unreachable: the `TocNotReplaceable` bail above
                            // already returned for this content. Kept as an
                            // explicit arm for match exhaustiveness.
                            ReplaceContent::Block(Block::Toc { .. }) => Some("toc"),
                            ReplaceContent::Inline(Inline::Hyperlink { .. }) => Some("hyperlink"),
                            ReplaceContent::Inline(Inline::Text { .. }) => Some("text"),
                            ReplaceContent::Inline(Inline::OpaqueRef { .. }) => Some("opaque_ref"),
                        };
                        if let Some(kind) = kind {
                            return Err(SchemaError::SpanOnNonParagraph { op_index, kind });
                        }
                        // A span op REQUIRES the block guard:
                        // the guard makes the ephemeral handle safe and
                        // enforces the no-compound-same-paragraph contract.
                        if guard.is_none() && semantic_hash.is_none() {
                            return Err(SchemaError::SpanRequiresGuard { op_index });
                        }
                    }
                }
                let path = NodePath::op(op_index).pushed(NodePathSegment::Field("content"));
                match content {
                    ReplaceContent::Block(block) => validate_block(block, &path)?,
                    ReplaceContent::Inline(Inline::Hyperlink { attrs, content }) => {
                        let path = path.pushed(NodePathSegment::Field("hyperlink"));
                        if attrs.href.is_none() && attrs.anchor.is_none() {
                            return Err(SchemaError::HyperlinkHasNoTarget { path });
                        }
                        let content_path = path.pushed(NodePathSegment::Field("content"));
                        for (i, child) in content.iter().enumerate() {
                            let child_path = content_path.pushed(NodePathSegment::Index(i));
                            validate_inline(child, &child_path)?;
                        }
                    }
                    ReplaceContent::Inline(Inline::Text { .. }) => {
                        return Err(SchemaError::UnaddressableReplaceContent {
                            op_index,
                            kind: "text",
                        });
                    }
                    ReplaceContent::Inline(Inline::OpaqueRef { .. }) => {
                        return Err(SchemaError::UnaddressableReplaceContent {
                            op_index,
                            kind: "opaque_ref",
                        });
                    }
                }
                // Invariant 2 (opaque set-equality), payload half: a single
                // payload cannot reference the same opaque id twice. The
                // engine performs the *set-equality* comparison against the
                // target paragraph later; this check is the local structural
                // half that does not need the document.
                check_unique_opaque_ids_in_replace_content(op_index, content)?;
            }
            Op::Insert { content, .. } => {
                if content.is_empty() {
                    return Err(SchemaError::EmptyInsertContent { op_index });
                }
                let path = NodePath::op(op_index).pushed(NodePathSegment::Field("content"));
                for (i, block) in content.iter().enumerate() {
                    let path = path.pushed(NodePathSegment::Index(i));
                    validate_block(block, &path)?;
                }
                // Insert payloads create brand-new content; they have no
                // source paragraph to inherit opaque ids from. The engine
                // rejects opaque_refs in insert content at resolve time;
                // the duplicate-id check here gives an earlier, clearer
                // error path when the LLM repeats the same id.
                let mut ids: Vec<String> = Vec::new();
                for block in content {
                    collect_opaque_ids_in_block(block, &mut ids);
                }
                if let Some(dup) = find_duplicate(&ids) {
                    return Err(SchemaError::DuplicateOpaqueRefInPayload {
                        op_index,
                        opaque_id: dup,
                    });
                }
            }
            Op::Delete {
                guard,
                semantic_hash,
                ..
            } => {
                reconcile_guard(op_index, guard.clone(), semantic_hash.clone())?;
            }
            Op::Move {
                guard,
                semantic_hash,
                ..
            } => {
                reconcile_guard(op_index, guard.clone(), semantic_hash.clone())?;
            }
            Op::SetAttr { attrs, .. } => {
                if attrs.is_empty() {
                    return Err(SchemaError::EmptyAttrPatch { op_index });
                }
            }
            Op::SetFormat {
                marks,
                color,
                highlight,
                font_family,
                font_size_half_points,
                caps,
                small_caps,
                char_spacing,
                ..
            } => {
                if marks.is_empty()
                    && color.is_none()
                    && highlight.is_none()
                    && font_family.is_none()
                    && font_size_half_points.is_none()
                    && !*caps
                    && !*small_caps
                    && char_spacing.is_none()
                {
                    return Err(SchemaError::EmptyFormatMarks { op_index });
                }
            }
            Op::SetParaFormat {
                align,
                indent,
                spacing,
                borders,
                shading,
                ..
            } => {
                let has_indent = indent.as_ref().is_some_and(|p| !p.is_empty());
                let has_spacing = spacing.as_ref().is_some_and(|p| !p.is_empty());
                if align.is_none() && !has_indent && !has_spacing {
                    // borders/shading still make the op non-empty.
                    let has_borders = borders.as_ref().is_some_and(|p| !p.is_empty());
                    let has_shading = shading.as_ref().is_some_and(|p| !p.is_empty());
                    if !has_borders && !has_shading {
                        return Err(SchemaError::EmptyParaFormat { op_index });
                    }
                }
            }
            Op::SetCellFormat {
                borders,
                shading,
                width,
                v_align,
                margins,
                ..
            } => {
                let has_borders = borders.as_ref().is_some_and(|p| !p.is_empty());
                let has_shading = shading.as_ref().is_some_and(|p| !p.is_empty());
                let has_margins = margins.as_ref().is_some_and(|p| !p.is_empty());
                if !has_borders
                    && !has_shading
                    && width.is_none()
                    && v_align.is_none()
                    && !has_margins
                {
                    return Err(SchemaError::EmptyCellFormat { op_index });
                }
            }
            Op::SetRowFormat {
                height,
                height_rule,
                ..
            } => {
                if height.is_none() && height_rule.is_none() {
                    return Err(SchemaError::EmptyRowFormat { op_index });
                }
            }
            Op::SetTableFormat {
                borders,
                width,
                default_cell_margins,
                ..
            } => {
                let has_borders = borders.as_ref().is_some_and(|p| !p.is_empty());
                let has_margins = default_cell_margins.as_ref().is_some_and(|p| !p.is_empty());
                if !has_borders && width.is_none() && !has_margins {
                    return Err(SchemaError::EmptyTableFormat { op_index });
                }
            }
            Op::InsertCrossRef {
                bookmark, ref_kind, ..
            } => {
                if bookmark.trim().is_empty() {
                    return Err(SchemaError::CrossRefEmptyBookmark { op_index });
                }
                if !matches!(ref_kind.as_str(), "ref" | "pageref" | "noref") {
                    return Err(SchemaError::UnknownRefKind {
                        op_index,
                        value: ref_kind.clone(),
                    });
                }
            }
            Op::SetNumbering { change, .. } => {
                // A non-bullet numbering level always renders visible counter
                // text; an empty `synthesized_text` on a numbered level is a
                // malformed request. Bullets carry empty text legitimately.
                let bad = match change {
                    NumberingChangeWire::SetList {
                        synthesized_text,
                        is_bullet,
                        ..
                    }
                    | NumberingChangeWire::SetLevel {
                        synthesized_text,
                        is_bullet,
                        ..
                    }
                    | NumberingChangeWire::SetType {
                        synthesized_text,
                        is_bullet,
                        ..
                    } => !*is_bullet && synthesized_text.is_empty(),
                    // Indent/Outdent/Restart/Continue/Remove/Split carry no
                    // caller-supplied label, so there is nothing to validate.
                    NumberingChangeWire::Remove
                    | NumberingChangeWire::Indent
                    | NumberingChangeWire::Outdent
                    | NumberingChangeWire::Restart
                    | NumberingChangeWire::Continue
                    | NumberingChangeWire::Split => false,
                };
                if bad {
                    return Err(SchemaError::EmptyNumberingText { op_index });
                }
            }
            Op::InsertBookmark { name, .. } => {
                if name.trim().is_empty() {
                    return Err(SchemaError::BookmarkEmptyName { op_index });
                }
            }
            Op::RenameBookmark { new_name, .. } => {
                if new_name.trim().is_empty() {
                    return Err(SchemaError::BookmarkEmptyName { op_index });
                }
            }
            Op::RemoveBookmark { .. } => {}
            Op::ApplyStyle { style_id, .. } => {
                if style_id.trim().is_empty() {
                    return Err(SchemaError::EmptyStyleId { op_index });
                }
            }
            Op::SetImageAttrs {
                resize, alt_text, ..
            } => {
                if resize.is_none() && alt_text.is_none() {
                    return Err(SchemaError::EmptyImageAttrs { op_index });
                }
                if let Some(r) = resize {
                    if r.cx < 0 {
                        return Err(SchemaError::NegativeImageDimension {
                            op_index,
                            axis: "cx",
                            value: r.cx,
                        });
                    }
                    if r.cy < 0 {
                        return Err(SchemaError::NegativeImageDimension {
                            op_index,
                            axis: "cy",
                            value: r.cy,
                        });
                    }
                }
            }
            Op::SetImageLayout {
                position_h,
                position_v,
                wrap,
                crop,
                ..
            } => {
                let crop_empty = crop.is_none_or(|c| {
                    c.left.is_none() && c.top.is_none() && c.right.is_none() && c.bottom.is_none()
                });
                if position_h.is_none() && position_v.is_none() && wrap.is_none() && crop_empty {
                    return Err(SchemaError::EmptyImageLayout { op_index });
                }
                // Each position axis must set exactly one of offset / align.
                for (axis, p) in [("position_h", position_h), ("position_v", position_v)] {
                    if let Some(p) = p
                        && p.offset.is_some() == p.align.is_some()
                    {
                        return Err(SchemaError::ImageLayoutPositionAmbiguous { op_index, axis });
                    }
                }
                // Wrap token must be a known keyword.
                if let Some(w) = wrap
                    && parse_wrap_token(w).is_none()
                {
                    return Err(SchemaError::ImageLayoutUnknownWrap {
                        op_index,
                        token: w.clone(),
                    });
                }
                // Crop insets are 1000ths of a percent in 0..=100000.
                if let Some(c) = crop {
                    for (edge, v) in [
                        ("left", c.left),
                        ("top", c.top),
                        ("right", c.right),
                        ("bottom", c.bottom),
                    ] {
                        if let Some(v) = v
                            && !(0..=100_000).contains(&v)
                        {
                            return Err(SchemaError::ImageLayoutCropOutOfRange {
                                op_index,
                                edge,
                                value: v,
                            });
                        }
                    }
                }
            }
            Op::CommentCreate { body, .. } => {
                if body.trim().is_empty() {
                    return Err(SchemaError::CommentEmptyBody { op_index });
                }
            }
            Op::CommentReply {
                parent_comment_id,
                body,
                ..
            } => {
                if parent_comment_id.trim().is_empty() {
                    return Err(SchemaError::CommentMissingParent { op_index });
                }
                if body.trim().is_empty() {
                    return Err(SchemaError::CommentEmptyBody { op_index });
                }
            }
            Op::CommentResolve { comment_id, .. } | Op::CommentDelete { comment_id, .. } => {
                if comment_id.trim().is_empty() {
                    return Err(SchemaError::CommentMissingId { op_index });
                }
            }
            Op::InsertNote { expect, body, .. } => {
                if expect.trim().is_empty() {
                    return Err(SchemaError::NoteEmptyExpect { op_index });
                }
                if body.trim().is_empty() {
                    return Err(SchemaError::NoteEmptyBody { op_index });
                }
            }
            Op::EditNote { note_id, body, .. } => {
                if note_id.trim().is_empty() {
                    return Err(SchemaError::NoteMissingId { op_index });
                }
                if body.trim().is_empty() {
                    return Err(SchemaError::NoteEmptyBody { op_index });
                }
            }
            Op::DeleteNote { note_id, .. } => {
                if note_id.trim().is_empty() {
                    return Err(SchemaError::NoteMissingId { op_index });
                }
            }
            Op::DeleteImage { .. } => {
                // No schema-layer checks: target/drawing_id come from the read
                // view; drawing existence + guard are enforced against the live
                // document at apply time.
            }
            Op::SetPageSetup {
                page_size,
                orientation,
                margins,
                columns,
                gutter,
                ..
            } => {
                if page_size.is_none()
                    && orientation.is_none()
                    && margins.is_none()
                    && columns.is_none()
                    && gutter.is_none()
                {
                    return Err(SchemaError::EmptyPageSetup { op_index });
                }
            }
            // SetSectionType / InsertSectionBreak carry a required section_type
            // token validated (and rejected if unknown) in `translate_op`.
            Op::SetSectionType { .. } | Op::InsertSectionBreak { .. } => {}
            // CreateHeader / CreateFooter carry a required header/footer `kind`
            // token validated (and rejected if unknown) in `translate_op`.
            Op::CreateHeader { .. } | Op::CreateFooter { .. } => {}
            Op::EditHeader {
                header_part,
                expect,
                ..
            } => {
                if header_part.trim().is_empty() {
                    return Err(SchemaError::HeaderFooterEmptyField {
                        op_index,
                        field: "header_part",
                    });
                }
                if expect.trim().is_empty() {
                    return Err(SchemaError::HeaderFooterEmptyField {
                        op_index,
                        field: "expect",
                    });
                }
            }
            Op::EditFooter {
                footer_part,
                expect,
                ..
            } => {
                if footer_part.trim().is_empty() {
                    return Err(SchemaError::HeaderFooterEmptyField {
                        op_index,
                        field: "footer_part",
                    });
                }
                if expect.trim().is_empty() {
                    return Err(SchemaError::HeaderFooterEmptyField {
                        op_index,
                        field: "expect",
                    });
                }
            }
            Op::SetHeaderFooterMode {
                title_page,
                even_and_odd,
                link,
                ..
            } => {
                if title_page.is_none() && even_and_odd.is_none() && link.is_none() {
                    return Err(SchemaError::EmptyHeaderFooterMode { op_index });
                }
            }
            Op::InsertEquation {
                expect,
                omml,
                placement,
                ..
            } => {
                if expect.trim().is_empty() {
                    return Err(SchemaError::EquationEmptyExpect { op_index });
                }
                if omml.trim().is_empty() {
                    return Err(SchemaError::EquationEmptyOmml { op_index });
                }
                if !matches!(placement.as_str(), "inline" | "block") {
                    return Err(SchemaError::UnknownEquationPlacement {
                        op_index,
                        value: placement.clone(),
                    });
                }
            }
            Op::BlocksToTable {
                delimiter, header, ..
            } => {
                if delimiter.is_empty() {
                    return Err(SchemaError::BlocksToTableEmptyDelimiter { op_index });
                }
                if let Some(cells) = header
                    && cells.is_empty()
                {
                    return Err(SchemaError::BlocksToTableEmptyHeader { op_index });
                }
            }
            Op::WrapContentControl {
                expect,
                tag,
                alias,
                control,
                data_binding,
                ..
            } => {
                if expect.trim().is_empty() {
                    return Err(SchemaError::ContentControlEmptyExpect { op_index });
                }
                // An all-empty spec (no tag, no alias, default rich-text) is
                // indistinguishable from un-wrapped content — refuse it here.
                let tag_empty = tag.as_deref().map(str::trim).unwrap_or("").is_empty();
                let alias_empty = alias.as_deref().map(str::trim).unwrap_or("").is_empty();
                if tag_empty && alias_empty && matches!(control, SdtControlWire::RichText) {
                    return Err(SchemaError::EmptyContentControlSpec { op_index });
                }
                // A data binding must carry a target: empty xpath / storeItemID
                // is unresolvable and would silently degrade to a plain control.
                if let Some(b) = data_binding {
                    if b.xpath.trim().is_empty() {
                        return Err(SchemaError::MalformedDataBinding {
                            op_index,
                            reason: "empty xpath",
                        });
                    }
                    if b.store_item_id.trim().is_empty() {
                        return Err(SchemaError::MalformedDataBinding {
                            op_index,
                            reason: "empty store_item_id",
                        });
                    }
                }
            }
            Op::WrapBlocksContentControl {
                tag,
                alias,
                control,
                ..
            } => {
                // Same all-empty-spec guard as the inline wrap: a no-tag /
                // no-alias / default rich-text control carries no identity.
                let tag_empty = tag.as_deref().map(str::trim).unwrap_or("").is_empty();
                let alias_empty = alias.as_deref().map(str::trim).unwrap_or("").is_empty();
                if tag_empty && alias_empty && matches!(control, SdtControlWire::RichText) {
                    return Err(SchemaError::EmptyContentControlSpec { op_index });
                }
            }
            Op::SetContentControlValue {
                text,
                checked,
                selected,
                ..
            } => {
                let present =
                    text.is_some() as u8 + checked.is_some() as u8 + selected.is_some() as u8;
                if present != 1 {
                    return Err(SchemaError::ContentControlValueArity { op_index, present });
                }
            }
            Op::SetFormFieldValue {
                text,
                checked,
                selected,
                ..
            } => {
                let present =
                    text.is_some() as u8 + checked.is_some() as u8 + selected.is_some() as u8;
                if present != 1 {
                    return Err(SchemaError::FormFieldValueArity { op_index, present });
                }
            }
            Op::InsertImage {
                bytes_base64,
                cx,
                cy,
                ..
            }
            | Op::ReplaceImage {
                bytes_base64,
                cx,
                cy,
                ..
            } => {
                if bytes_base64.trim().is_empty() {
                    return Err(SchemaError::ImageBytesEmpty { op_index });
                }
                // cx/cy are optional (omit to derive from intrinsic dimensions);
                // a supplied value must still be non-negative.
                if let Some(cx) = cx
                    && *cx < 0
                {
                    return Err(SchemaError::NegativeImageDimension {
                        op_index,
                        axis: "cx",
                        value: *cx,
                    });
                }
                if let Some(cy) = cy
                    && *cy < 0
                {
                    return Err(SchemaError::NegativeImageDimension {
                        op_index,
                        axis: "cy",
                        value: *cy,
                    });
                }
            }
            Op::CreateStyle { def, .. } => {
                validate_style_def_wire(op_index, def)?;
            }
            Op::ModifyStyle { def, .. } => {
                validate_style_def_wire(op_index, def)?;
            }
            Op::SetDocDefaults {
                font_family,
                font_size_half_points,
                ..
            } => {
                // Fail loud: an op that sets neither field is a no-op the caller
                // did not mean (CLAUDE.md "no silent fallbacks").
                if font_family.is_none() && font_size_half_points.is_none() {
                    return Err(SchemaError::DocDefaultsEmpty { op_index });
                }
            }
            // A granular table op carries only indices + (for set_cell_text) a
            // string. All structural validity (index ranges, simple-grid
            // requirement, rectangular merge region) depends on the LIVE table,
            // so it is enforced in the verb where the resolved table is in scope
            // — the verb returns actionable, table-addressed errors. There is
            // no transaction-local schema invariant to check here.
            Op::TableOp { .. } => {}
            // SetTextboxText needs no schema-layer validation: any paragraph
            // strings are acceptable (an empty list becomes one empty paragraph
            // in the verb), and the txbxContent/tracked-interior checks are
            // structural and live in the verb.
            Op::SetTextboxText { .. } => {}
            // OpaqueTextEdit needs no schema-layer validation: an absent/empty
            // `find` fails loud in the splice core (`OpaqueTextNotFound`), and the
            // container/paragraph address is validated structurally in the verb.
            Op::OpaqueTextEdit { .. } => {}
            // SdtTextFill's exactly-one-target and empty-fill rules are enforced
            // in the verb where the resolved control is in scope.
            Op::SdtTextFill { .. } => {}
        }
    }
    Ok(())
}

/// Wire-edge validation for a style definition: non-empty `style_id` + `name`.
fn validate_style_def_wire(op_index: usize, def: &StyleDefinitionWire) -> Result<(), SchemaError> {
    if def.style_id.trim().is_empty() {
        return Err(SchemaError::StyleDefEmptyId { op_index });
    }
    if def.name.trim().is_empty() {
        return Err(SchemaError::StyleDefEmptyName { op_index });
    }
    Ok(())
}

/// OOXML list levels are 0..=8 (`w:ilvl`, §17.9.3). An inserted paragraph's
/// `list.ilvl` is bounds-checked at the wire edge against this maximum.
const MAX_INSERT_ILVL: u32 = 8;

/// Word/OOXML TOC heading levels are 1..=9 (`\o "from-to"`, §17.16.5.68). A
/// `Block::Toc.levels` pair is bounds-checked at the wire edge against this
/// range (and `from <= to`).
const MIN_TOC_LEVEL: u8 = 1;
const MAX_TOC_LEVEL: u8 = 9;

fn validate_block(block: &Block, path: &NodePath) -> Result<(), SchemaError> {
    match block {
        Block::Paragraph { content, list, .. } => {
            let path = path.pushed(NodePathSegment::Field("paragraph"));
            // List level bounds (`w:ilvl`, §17.9.3): OOXML defines 9 levels,
            // 0..=8. A level outside that range is structurally invalid; refused
            // at the wire edge rather than clamped (no silent fallback). The
            // numId is NOT checked here — its existence is a document-relative
            // fact resolved at apply time (`InsertListNumIdUnknown`).
            if let Some(list) = list
                && list.ilvl > MAX_INSERT_ILVL
            {
                return Err(SchemaError::InsertListLevelOutOfBounds {
                    path: path.clone(),
                    ilvl: list.ilvl,
                });
            }
            let content_path = path.pushed(NodePathSegment::Field("content"));
            for (i, inline) in content.iter().enumerate() {
                let path = content_path.pushed(NodePathSegment::Index(i));
                validate_inline(inline, &path)?;
            }
        }
        Block::Table { content, .. } => {
            let path = path.pushed(NodePathSegment::Field("table"));
            if content.is_empty() {
                return Err(SchemaError::EmptyTableRows { path });
            }
            let rows_path = path.pushed(NodePathSegment::Field("content"));
            for (i, row) in content.iter().enumerate() {
                let row_path = rows_path.pushed(NodePathSegment::Index(i));
                if row.content.is_empty() {
                    return Err(SchemaError::EmptyTableRowCells { path: row_path });
                }
                let cells_path = row_path.pushed(NodePathSegment::Field("content"));
                for (j, cell) in row.content.iter().enumerate() {
                    let cell_path = cells_path.pushed(NodePathSegment::Index(j));
                    if cell.content.is_empty() {
                        return Err(SchemaError::EmptyTableCellBlocks { path: cell_path });
                    }
                    // gridSpan bounds (§17.4.17): a horizontal merge spans ≥ 1
                    // column. Cross-row constraints (ragged grid, orphan vMerge)
                    // are validated deeper, by the engine's `validate_merge_spec`.
                    if let Some(attrs) = &cell.attrs
                        && attrs.grid_span == Some(0)
                    {
                        return Err(SchemaError::ZeroGridSpan { path: cell_path });
                    }
                    let blocks_path = cell_path.pushed(NodePathSegment::Field("content"));
                    for (k, block) in cell.content.iter().enumerate() {
                        let block_path = blocks_path.pushed(NodePathSegment::Index(k));
                        // Day-one scope: a ToC is top-level only. Caught here,
                        // at the cell-recursion call site, rather than inside
                        // the `Block::Toc` arm below — that arm also runs for
                        // top-level insert content, where a ToC IS allowed.
                        if matches!(block, Block::Toc { .. }) {
                            return Err(SchemaError::TocNotAllowedInTableCell { path: block_path });
                        }
                        validate_block(block, &block_path)?;
                    }
                }
            }
        }
        Block::Toc { levels } => {
            if let Some(levels) = levels {
                let path = path.pushed(NodePathSegment::Field("levels"));
                if levels.from < MIN_TOC_LEVEL
                    || levels.from > levels.to
                    || levels.to > MAX_TOC_LEVEL
                {
                    return Err(SchemaError::TocLevelsOutOfBounds {
                        path,
                        from: levels.from,
                        to: levels.to,
                    });
                }
            }
        }
    }
    Ok(())
}

// ─── Adapter: v4 -> internal EditStep ────────────────────────────────────────
//
// The engine's apply pipeline consumes `EditStep` (the v3 internal type). The
// adapter translates v4 ops into `EditStep`s without going through any wire
// format — the typed v4 tree is converted directly to `ParagraphContent` and
// the matching `EditStep` variant.
//
// Mappings (v4 op -> internal EditStep):
//   v4 replace(paragraph)  -> EditStep::ReplaceParagraphText
//   v4 replace(hyperlink)  -> EditStep::ReplaceHyperlinkText
//   v4 replace(table)      -> EditStep::ReplaceTable
//   v4 insert(paragraph)   -> EditStep::InsertParagraphs
//   v4 insert(table)       -> EditStep::InsertParagraphs (table BlockSpec)
//   v4 delete              -> EditStep::DeleteBlockRange (single-block range)
//   v4 move                -> EditStep::MoveBlockRange (single-block range)
//   v4 set_attr(role)      -> EditStep::SetBlockRangeAttr
//   v4 set_attr(href/...)  -> not yet supported by the engine (M7+)
//
// Adapter failures are explicit `AdapterError` variants. We do not fall back
// to lossy translations (CLAUDE.md "no silent fallbacks") — if a v4 surface
// is not yet routed by the engine, the adapter says so and stops.

/// A translation failure when adapting a v4 op into the engine's internal
/// `EditStep`. These errors signal that the v4 wire format expressed
/// something the engine does not yet support. The translation is **not**
/// best-effort; an unrouted surface stops the transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdapterError {
    /// `insert` with a non-paragraph block. The engine's insert path only
    /// resolves paragraph blocks today.
    InsertNonParagraphNotSupported { op_index: usize, kind: &'static str },

    /// `set_attr` carries fields that don't apply to a single resolved kind.
    /// Either a paragraph-only field is mixed with a hyperlink-only field,
    /// or no fields disambiguate the target kind.
    AmbiguousSetAttrAttrs { op_index: usize },

    /// `set_attr` on a hyperlink set `attrs.href` without supplying the
    /// matching `expect_href`, or set `attrs.anchor` without `expect_anchor`.
    /// The optimistic-concurrency precondition is required for hyperlink
    /// attribute mutations: without it a stale caller could retarget the
    /// wrong link entirely.
    MissingHyperlinkAttrExpect { op_index: usize, attr: &'static str },

    /// A v4 `Mark::InlineRole` appeared on a text node. The engine's
    /// `InlineMarkSet` does not yet carry document-vocabulary inline roles.
    InlineRoleMarkNotSupported { op_index: usize, role_id: String },

    /// A hyperlink's `title` attribute was set. The engine does not yet
    /// thread hyperlink tooltips through the IR.
    HyperlinkTitleNotSupported { op_index: usize },

    /// A hyperlink contained a non-text inline (a nested hyperlink, an
    /// opaque_ref, or marked text). OOXML does not support nested
    /// hyperlinks; marked content inside hyperlinks is deferred.
    UnsupportedInlineInsideHyperlink {
        op_index: usize,
        reason: &'static str,
    },

    /// `replace(paragraph)` was supplied without an `expect` precondition.
    /// A `guard`/`semantic_hash` alias conflict surfaced during translation.
    /// `validate_schema` normally catches this first; this is the local
    /// re-check so translation never silently picks one of two disagreeing
    /// guards. Carries the rendered `SchemaError::ConflictingGuard` message.
    SchemaConflict { message: String },

    /// The engine requires a staleness precondition for paragraph-replace to
    /// anchor the rewrite against a known snapshot. Under the unified-guard
    /// contract this means a `guard` (block hash) **or** a legacy `expect`
    /// substring; an op with neither is refused. (When a guard is present,
    /// `expect` is advisory and may be absent.)
    MissingExpect { op_index: usize },

    /// `delete` was supplied without an `expect` precondition. Same
    /// reasoning as `MissingExpect` above.
    MissingDeleteExpect { op_index: usize },

    /// `set_format.color` was neither a 6-hex-digit RGB value nor `auto`.
    /// Refused at the wire edge rather than coerced.
    InvalidColorValue { op_index: usize, value: String },

    /// `set_format.highlight` was not a known `ST_HighlightColor` name.
    UnsupportedHighlightColor { op_index: usize, value: String },

    /// `set_para_format.align` was not one of the accepted alignment tokens
    /// (`left` | `center` | `right` | `both` | `distribute`). Refused at the
    /// wire edge rather than coerced.
    UnknownAlignment { op_index: usize, value: String },

    /// `set_para_format.spacing.line_rule` was not one of `auto` | `exact` |
    /// `at_least`. Refused at the wire edge rather than coerced.
    UnknownLineRule { op_index: usize, value: String },

    /// A border `style` token (in `set_para_format.borders` /
    /// `set_cell_format.borders`) was not a known `ST_Border` value. Refused at
    /// the wire edge rather than coerced.
    UnknownBorderStyle { op_index: usize, value: String },
    /// A border `color` was neither `auto` nor six hex digits.
    InvalidBorderColor { op_index: usize, value: String },
    /// A shading `fill`/`color` was neither `auto` nor six hex digits.
    InvalidShadingColor { op_index: usize, value: String },
    /// A shading `pattern` token was not a known `ST_Shd` value.
    UnknownShadingPattern { op_index: usize, value: String },
    /// A width `width_type` token was not a known `ST_TblWidth` value.
    UnknownWidthType { op_index: usize, value: String },
    /// `set_cell_format.v_align` was not `top` | `center` | `bottom`.
    UnknownVerticalAlignment { op_index: usize, value: String },
    /// `set_row_format.height_rule` was not a known `ST_HeightRule` value
    /// (`exact` | `atLeast` | `auto`).
    UnknownHeightRule { op_index: usize, value: String },
    /// `insert_cross_ref.ref_kind` was not `ref`/`pageref`/`noref` (adapter-side
    /// defence for direct callers that bypass `validate_schema`).
    UnknownRefKind { op_index: usize, value: String },

    /// `insert_note`/`edit_note`/`delete_note`'s `note_kind` was not `footnote`
    /// or `endnote`. Refused at the wire edge — NEVER defaulted to footnote.
    UnknownNoteKind { op_index: usize, value: String },

    /// `set_section_type` / `insert_section_break`'s `section_type` was not one
    /// of `next_page` | `continuous` | `even_page` | `odd_page` | `next_column`.
    /// Refused at the wire edge — NEVER mapped to a default `Other`.
    UnknownSectionType { op_index: usize, value: String },

    /// `set_page_setup` / `insert_section_break`'s `orientation` was not
    /// `portrait` | `landscape`. Refused at the wire edge, never coerced.
    UnknownOrientation { op_index: usize, value: String },

    /// `set_header_footer_mode.link.kind` was not `default` | `first` | `even`.
    /// Refused at the wire edge.
    UnknownHeaderFooterKind { op_index: usize, value: String },

    /// `insert_equation.placement` was not `inline` | `block` (adapter-side
    /// defence for direct callers that bypass `validate_schema`).
    UnknownEquationPlacement { op_index: usize, value: String },

    /// `set_content_control_value` did not set exactly one value kind (adapter-
    /// side defence for direct callers that bypass `validate_schema`).
    AmbiguousContentControlValue { op_index: usize },

    /// `set_form_field_value` did not set exactly one value kind (adapter-side
    /// defence for direct callers that bypass `validate_schema`).
    AmbiguousFormFieldValue { op_index: usize },

    /// `insert_image` / `replace_image`'s `format` was not `png` | `jpeg` | `gif`.
    /// Refused at the wire edge — NEVER mapped to a default.
    UnknownImageFormat { op_index: usize, value: String },

    /// `insert_image` / `replace_image`'s `bytes_base64` failed to decode. The
    /// decode error is surfaced rather than swallowed.
    InvalidImageBase64 { op_index: usize, reason: String },

    /// `insert_image` / `replace_image` omitted `cx`/`cy` (asking for the
    /// intrinsic-size default) but the image header could not be decoded to
    /// positive pixel dimensions. Refused rather than guessing a size — the
    /// caller can pass `cx`/`cy` explicitly.
    ImageDimensionsUndecodable {
        op_index: usize,
        format: &'static str,
        len: usize,
    },

    /// `create_style` / `modify_style`'s `style_type` was not
    /// `para` | `char` | `table` | `numbering`. Refused at the wire edge.
    UnknownStyleType { op_index: usize, value: String },

    /// `create_style.para_props.alignment` was not a recognized justification
    /// token. Refused at the wire edge, never coerced.
    UnknownStyleAlignment { op_index: usize, value: String },

    /// A `table_op` insert's `position` was not `before` | `after`. Refused at
    /// the wire edge — never coerced to a default.
    UnknownTablePosition { op_index: usize, value: String },
}

impl std::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdapterError::InsertNonParagraphNotSupported { op_index, kind } => write!(
                f,
                "ops[{op_index}]: insert of a `{kind}` block is not yet supported"
            ),
            AdapterError::AmbiguousSetAttrAttrs { op_index } => write!(
                f,
                "ops[{op_index}]: set_attr.attrs mixes fields from multiple target kinds"
            ),
            AdapterError::MissingHyperlinkAttrExpect { op_index, attr } => write!(
                f,
                "ops[{op_index}]: set_attr on hyperlink requires `expect_{attr}` \
                 when `attrs.{attr}` is set (optimistic-concurrency precondition)"
            ),
            AdapterError::InlineRoleMarkNotSupported { op_index, role_id } => write!(
                f,
                "ops[{op_index}]: inline_role mark `{role_id}` is not yet supported by the engine"
            ),
            AdapterError::HyperlinkTitleNotSupported { op_index } => write!(
                f,
                "ops[{op_index}]: hyperlink `title` attribute is not yet threaded through the IR"
            ),
            AdapterError::UnsupportedInlineInsideHyperlink { op_index, reason } => write!(
                f,
                "ops[{op_index}]: hyperlink content must be plain text: {reason}"
            ),
            AdapterError::SchemaConflict { message } => write!(f, "{message}"),
            AdapterError::MissingExpect { op_index } => write!(
                f,
                "ops[{op_index}]: replace(paragraph) requires a staleness precondition \
                 (`guard` block hash or legacy `expect` substring)"
            ),
            AdapterError::MissingDeleteExpect { op_index } => write!(
                f,
                "ops[{op_index}]: delete requires an `expect` precondition"
            ),
            AdapterError::InvalidColorValue { op_index, value } => write!(
                f,
                "ops[{op_index}]: set_format.color '{value}' is not a 6-hex-digit \
                 RGB value or 'auto'"
            ),
            AdapterError::UnsupportedHighlightColor { op_index, value } => write!(
                f,
                "ops[{op_index}]: set_format.highlight '{value}' is not a known \
                 ST_HighlightColor name"
            ),
            AdapterError::UnknownAlignment { op_index, value } => write!(
                f,
                "ops[{op_index}]: set_para_format.align '{value}' is not one of \
                 left | center | right | both | distribute"
            ),
            AdapterError::UnknownLineRule { op_index, value } => write!(
                f,
                "ops[{op_index}]: set_para_format.spacing.line_rule '{value}' is not \
                 one of auto | exact | at_least"
            ),
            AdapterError::UnknownBorderStyle { op_index, value } => write!(
                f,
                "ops[{op_index}]: border style '{value}' is not a known ST_Border value"
            ),
            AdapterError::InvalidBorderColor { op_index, value } => write!(
                f,
                "ops[{op_index}]: border color '{value}' is not a 6-hex-digit RGB value or 'auto'"
            ),
            AdapterError::InvalidShadingColor { op_index, value } => write!(
                f,
                "ops[{op_index}]: shading color '{value}' is not a 6-hex-digit RGB value or 'auto'"
            ),
            AdapterError::UnknownShadingPattern { op_index, value } => write!(
                f,
                "ops[{op_index}]: shading pattern '{value}' is not a known ST_Shd value"
            ),
            AdapterError::UnknownWidthType { op_index, value } => write!(
                f,
                "ops[{op_index}]: width type '{value}' is not a known ST_TblWidth value \
                 (dxa | pct | auto | nil)"
            ),
            AdapterError::UnknownVerticalAlignment { op_index, value } => write!(
                f,
                "ops[{op_index}]: set_cell_format.v_align '{value}' is not one of \
                 top | center | bottom"
            ),
            AdapterError::UnknownHeightRule { op_index, value } => write!(
                f,
                "ops[{op_index}]: set_row_format.height_rule '{value}' is not one of \
                 exact | atLeast | auto"
            ),
            AdapterError::UnknownRefKind { op_index, value } => write!(
                f,
                "ops[{op_index}]: insert_cross_ref.ref_kind '{value}' is not one of \
                 `ref`, `pageref`, `noref`"
            ),
            AdapterError::UnknownNoteKind { op_index, value } => write!(
                f,
                "ops[{op_index}]: note_kind '{value}' is not one of `footnote`, `endnote`"
            ),
            AdapterError::UnknownSectionType { op_index, value } => write!(
                f,
                "ops[{op_index}]: section_type '{value}' is not one of `next_page`, \
                 `continuous`, `even_page`, `odd_page`, `next_column`"
            ),
            AdapterError::UnknownOrientation { op_index, value } => write!(
                f,
                "ops[{op_index}]: orientation '{value}' is not `portrait` or `landscape`"
            ),
            AdapterError::UnknownHeaderFooterKind { op_index, value } => write!(
                f,
                "ops[{op_index}]: header/footer kind '{value}' is not one of `default`, \
                 `first`, `even`"
            ),
            AdapterError::UnknownEquationPlacement { op_index, value } => write!(
                f,
                "ops[{op_index}]: insert_equation.placement '{value}' is not `inline` or `block`"
            ),
            AdapterError::AmbiguousContentControlValue { op_index } => write!(
                f,
                "ops[{op_index}]: set_content_control_value must set exactly one of \
                 text/checked/selected"
            ),
            AdapterError::AmbiguousFormFieldValue { op_index } => write!(
                f,
                "ops[{op_index}]: set_form_field_value must set exactly one of \
                 text/checked/selected"
            ),
            AdapterError::UnknownImageFormat { op_index, value } => write!(
                f,
                "ops[{op_index}]: image format '{value}' is not `png`, `jpeg`, or `gif`"
            ),
            AdapterError::InvalidImageBase64 { op_index, reason } => write!(
                f,
                "ops[{op_index}]: image bytes_base64 failed to decode: {reason}"
            ),
            AdapterError::ImageDimensionsUndecodable {
                op_index,
                format,
                len,
            } => write!(
                f,
                "ops[{op_index}]: cannot decode intrinsic pixel dimensions from the \
                 {format} image ({len} bytes) to default the display size; pass cx and cy \
                 explicitly (EMUs; 1 px = 9525 at 96 DPI)"
            ),
            AdapterError::UnknownStyleType { op_index, value } => write!(
                f,
                "ops[{op_index}]: style_type '{value}' is not `para`, `char`, `table`, or `numbering`"
            ),
            AdapterError::UnknownStyleAlignment { op_index, value } => write!(
                f,
                "ops[{op_index}]: style alignment '{value}' is not a recognized justification token"
            ),
            AdapterError::UnknownTablePosition { op_index, value } => write!(
                f,
                "ops[{op_index}]: table insert position '{value}' is not 'before' or 'after'"
            ),
        }
    }
}

impl std::error::Error for AdapterError {}

impl EditTransactionV4 {
    /// Translate this v4 transaction into the engine's internal
    /// `EditTransaction`. Schema-level invariants must have been enforced
    /// by `validate_schema` (or `parse_transaction`) before calling this.
    pub fn into_edit_transaction(self) -> Result<EditTransaction, AdapterError> {
        let mut steps = Vec::with_capacity(self.ops.len());
        for (op_index, op) in self.ops.into_iter().enumerate() {
            steps.push(translate_op(op_index, op)?);
        }
        let date = self
            .revision
            .date
            .or_else(|| Some(chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()));
        Ok(EditTransaction {
            steps,
            summary: self.summary,
            materialization_mode: self.materialization_mode,
            revision: RevisionInfo {
                revision_id: 0,
                identity: 0,
                author: Some(self.revision.author),
                date,
                apply_op_id: self.revision.apply_op_id,
            },
        })
    }
}

fn translate_op(op_index: usize, op: Op) -> Result<EditStep, AdapterError> {
    match op {
        Op::Replace {
            target,
            content,
            span,
            expect,
            guard,
            semantic_hash,
            rationale,
        } => {
            // `guard`/`semantic_hash` are aliases; `validate_schema` already
            // rejected a conflicting pair, so collapsing to whichever is present
            // is exact (not a fallback). Re-check here keeps the invariant local.
            let semantic_hash = reconcile_guard(op_index, guard, semantic_hash).map_err(|e| {
                AdapterError::SchemaConflict {
                    message: e.to_string(),
                }
            })?;
            match resolve_span_selector(op_index, span)? {
                // `None`/`whole` => byte-identical to the pre-Phase-3 path.
                None | Some(ResolvedSpanSelector::Whole) => {
                    translate_replace(op_index, target, content, expect, semantic_hash, rationale)
                }
                Some(resolved) => translate_span_replace(
                    op_index,
                    target,
                    content,
                    resolved,
                    expect,
                    semantic_hash,
                    rationale,
                ),
            }
        }
        Op::Insert {
            target,
            content,
            rationale,
        } => {
            let blocks = translate_insert_blocks(op_index, &content)?;
            Ok(EditStep::InsertParagraphs {
                anchor_block_id: target.anchor,
                position: convert_anchor_position(target.position),
                rationale,
                blocks,
            })
        }
        Op::Delete {
            target,
            expect,
            guard,
            semantic_hash,
            rationale,
        } => {
            let semantic_hash = reconcile_guard(op_index, guard, semantic_hash).map_err(|e| {
                AdapterError::SchemaConflict {
                    message: e.to_string(),
                }
            })?;
            let expect = expect.ok_or(AdapterError::MissingDeleteExpect { op_index })?;
            Ok(EditStep::DeleteBlockRange {
                from_block_id: target.clone(),
                to_block_id: target,
                rationale,
                expect,
                semantic_hash,
            })
        }
        Op::Move {
            target,
            destination,
            expect,
            guard,
            semantic_hash,
            rationale,
        } => {
            let semantic_hash = reconcile_guard(op_index, guard, semantic_hash).map_err(|e| {
                AdapterError::SchemaConflict {
                    message: e.to_string(),
                }
            })?;
            let (from_block_id, to_block_id) = match target {
                MoveTarget::Single(id) => (id.clone(), id),
                MoveTarget::Range { from, to } => (from, to),
            };
            Ok(EditStep::MoveBlockRange {
                from_block_id,
                to_block_id,
                dest_anchor_id: destination.anchor,
                dest_position: convert_anchor_position(destination.position),
                rationale,
                expect,
                semantic_hash,
            })
        }
        Op::SetAttr {
            target,
            attrs,
            expect_href,
            expect_anchor,
            rationale,
        } => translate_set_attr(
            op_index,
            target,
            attrs,
            expect_href,
            expect_anchor,
            rationale,
        ),
        Op::SetFormat {
            target,
            expect,
            semantic_hash,
            marks,
            color,
            highlight,
            font_family,
            font_size_half_points,
            caps,
            small_caps,
            char_spacing,
            rationale,
        } => {
            let mut set = InlineMarkSet::default();
            for mark in marks {
                match mark {
                    Mark::Bold => set.bold = true,
                    Mark::Italic => set.italic = true,
                    Mark::Underline => set.underline = true,
                    Mark::Strike => set.strike = true,
                    Mark::Subscript => set.subscript = true,
                    Mark::Superscript => set.superscript = true,
                    Mark::InlineRole { id } => {
                        return Err(AdapterError::InlineRoleMarkNotSupported {
                            op_index,
                            role_id: id,
                        });
                    }
                }
            }
            // caps / smallCaps are tri-state StyleProps, carried as their own
            // wire booleans (not in `marks`); fold them onto the same set.
            set.caps = caps;
            set.small_caps = small_caps;

            // Validate/map the value-bearing properties at the wire edge so a
            // bad request fails loud here rather than being coerced downstream.
            if let Some(c) = &color
                && !is_valid_color(c)
            {
                return Err(AdapterError::InvalidColorValue {
                    op_index,
                    value: c.clone(),
                });
            }
            let highlight = match highlight {
                Some(name) => Some(HighlightColor::from_xml_str(&name).map_err(|_| {
                    AdapterError::UnsupportedHighlightColor {
                        op_index,
                        value: name,
                    }
                })?),
                None => None,
            };
            let style = RunStyleEdit {
                color: color.map(Into::into),
                highlight,
                font_family: font_family.map(Into::into),
                font_size_half_points,
                char_spacing,
            };

            Ok(EditStep::SetRunFormatting {
                block_id: target,
                expect,
                semantic_hash,
                marks: set,
                style,
                rationale,
            })
        }
        Op::SetParaFormat {
            target,
            semantic_hash,
            align,
            indent,
            spacing,
            borders,
            shading,
            rationale,
        } => {
            // Parse/validate every value-bearing field at the wire edge so a bad
            // request fails loud here rather than being coerced downstream
            // (CLAUDE.md "no silent fallbacks").
            let align = match align {
                Some(token) => Some(parse_alignment(op_index, &token)?),
                None => None,
            };
            let indent = match indent {
                Some(p) if !p.is_empty() => Some(Indentation {
                    left: p.left,
                    right: p.right,
                    effective_first_line_twips: p.first_line,
                    start_chars: None,
                    end_chars: None,
                    first_line_chars: None,
                    hanging_chars: None,
                }),
                _ => None,
            };
            let spacing = match spacing {
                Some(p) if !p.is_empty() => {
                    let line_rule = match p.line_rule {
                        Some(rule) => Some(parse_line_rule(op_index, &rule)?),
                        None => None,
                    };
                    Some(ParagraphSpacing {
                        before: p.before,
                        after: p.after,
                        before_lines: None,
                        after_lines: None,
                        before_autospacing: None,
                        after_autospacing: None,
                        line: p.line,
                        line_rule,
                    })
                }
                _ => None,
            };

            let borders = match borders {
                Some(p) => parse_para_borders(op_index, &p)?,
                None => None,
            };
            let shading = match shading {
                Some(p) => parse_shading(op_index, &p)?,
                None => None,
            };

            let patch = ParagraphFormattingPatch {
                align,
                indent,
                spacing,
                borders,
                shading,
            };

            Ok(EditStep::SetParagraphFormatting {
                block_id: target,
                semantic_hash,
                patch,
                rationale,
            })
        }
        Op::SetCellFormat {
            target,
            semantic_hash,
            row_index,
            col_index,
            borders,
            shading,
            width,
            v_align,
            margins,
            rationale,
        } => {
            // Parse/validate every value-bearing field at the wire edge so a bad
            // request fails loud here rather than being coerced downstream
            // (CLAUDE.md "no silent fallbacks").
            let borders = match borders {
                Some(p) => parse_border_set(op_index, &p)?,
                None => None,
            };
            let shading = match shading {
                Some(p) => parse_shading(op_index, &p)?,
                None => None,
            };
            let width = match width {
                Some(p) => Some(parse_measurement(op_index, &p)?),
                None => None,
            };
            let v_align = match v_align {
                Some(token) => Some(parse_v_align(op_index, &token)?),
                None => None,
            };
            let margins = margins.and_then(|p| {
                if p.is_empty() {
                    None
                } else {
                    Some(CellMargins {
                        top: p.top,
                        bottom: p.bottom,
                        left: p.left,
                        right: p.right,
                    })
                }
            });

            let patch = CellFormattingPatch {
                borders,
                shading,
                width,
                v_align,
                margins,
            };

            Ok(EditStep::SetCellFormatting {
                block_id: target,
                row_index,
                col_index,
                semantic_hash,
                patch,
                rationale,
            })
        }
        Op::SetRowFormat {
            target,
            semantic_hash,
            row_index,
            height,
            height_rule,
            rationale,
        } => {
            // Parse/validate the height rule token at the wire edge so a bad
            // request fails loud here rather than being coerced downstream
            // (CLAUDE.md "no silent fallbacks").
            let height_rule = match height_rule {
                Some(token) => Some(parse_height_rule(op_index, &token)?),
                None => None,
            };

            let patch = RowFormattingPatch {
                height,
                height_rule,
            };

            Ok(EditStep::SetRowFormatting {
                block_id: target,
                row_index,
                semantic_hash,
                patch,
                rationale,
            })
        }
        Op::SetTableFormat {
            target,
            semantic_hash,
            borders,
            width,
            default_cell_margins,
            rationale,
        } => {
            // Parse/validate every value-bearing field at the wire edge so a bad
            // request fails loud here rather than being coerced downstream
            // (CLAUDE.md "no silent fallbacks").
            let borders = match borders {
                Some(p) => parse_border_set(op_index, &p)?,
                None => None,
            };
            let width = match width {
                Some(p) => Some(parse_measurement(op_index, &p)?),
                None => None,
            };
            let default_cell_margins = default_cell_margins.and_then(|p| {
                if p.is_empty() {
                    None
                } else {
                    Some(CellMargins {
                        top: p.top,
                        bottom: p.bottom,
                        left: p.left,
                        right: p.right,
                    })
                }
            });

            let patch = TableFormattingPatch {
                borders,
                width,
                default_cell_margins,
            };

            Ok(EditStep::SetTableFormatting {
                block_id: target,
                semantic_hash,
                patch,
                rationale,
            })
        }
        Op::InsertCrossRef {
            target,
            expect,
            semantic_hash,
            bookmark,
            ref_kind,
            as_hyperlink,
            no_paragraph_number,
            paragraph_number_relative,
            paragraph_number_full,
            above_below,
            rationale,
        } => {
            let kind = match ref_kind.as_str() {
                "ref" => RefKind::Ref,
                "pageref" => RefKind::PageRef,
                "noref" => RefKind::NoRef,
                _ => {
                    return Err(AdapterError::UnknownRefKind {
                        op_index,
                        value: ref_kind,
                    });
                }
            };
            let spec = RefFieldSpec {
                kind,
                bookmark,
                insert_hyperlink: as_hyperlink,
                no_paragraph_number,
                paragraph_number_relative,
                paragraph_number_full,
                suppress_non_delimiter: false,
                above_below,
                format: FormatSwitches::default(),
            };
            Ok(EditStep::InsertCrossReference {
                block_id: target,
                expect,
                semantic_hash,
                spec,
                rationale,
            })
        }
        Op::SetNumbering {
            target,
            change,
            semantic_hash,
            rationale,
        } => {
            let change = match change {
                NumberingChangeWire::SetList {
                    num_id,
                    ilvl,
                    restart,
                    synthesized_text,
                    is_bullet,
                } => NumberingChange::SetList {
                    num_id,
                    ilvl,
                    restart,
                    synthesized_text,
                    is_bullet,
                },
                NumberingChangeWire::SetLevel {
                    ilvl,
                    synthesized_text,
                    is_bullet,
                } => NumberingChange::SetLevel {
                    ilvl,
                    synthesized_text,
                    is_bullet,
                },
                NumberingChangeWire::Remove => NumberingChange::Remove,
                NumberingChangeWire::Indent => NumberingChange::Indent,
                NumberingChangeWire::Outdent => NumberingChange::Outdent,
                NumberingChangeWire::Restart => NumberingChange::Restart,
                NumberingChangeWire::Continue => NumberingChange::Continue,
                NumberingChangeWire::SetType {
                    num_id,
                    synthesized_text,
                    is_bullet,
                } => NumberingChange::SetType {
                    num_id,
                    synthesized_text,
                    is_bullet,
                },
                NumberingChangeWire::Split => NumberingChange::Split,
            };
            Ok(EditStep::SetParagraphNumbering {
                block_id: target,
                semantic_hash,
                change,
                rationale,
            })
        }
        Op::TableOp {
            target,
            semantic_hash,
            table_op,
            rationale,
        } => {
            let op = translate_table_op(op_index, table_op)?;
            Ok(EditStep::TableStructureOp {
                block_id: target,
                semantic_hash,
                op,
                rationale,
            })
        }
        Op::InsertBookmark {
            target,
            expect,
            name,
            semantic_hash,
            rationale,
        } => Ok(EditStep::InsertBookmark {
            block_id: target,
            expect,
            semantic_hash,
            name,
            rationale,
        }),
        Op::RenameBookmark {
            target,
            old_name,
            new_name,
            semantic_hash,
            rationale,
        } => Ok(EditStep::RenameBookmark {
            block_id: target,
            old_name,
            new_name,
            semantic_hash,
            rationale,
        }),
        Op::RemoveBookmark {
            target,
            name,
            semantic_hash,
            rationale,
        } => Ok(EditStep::RemoveBookmark {
            block_id: target,
            name,
            semantic_hash,
            rationale,
        }),
        Op::ApplyStyle {
            target,
            style_id,
            semantic_hash,
            rationale,
        } => Ok(EditStep::ApplyStyle {
            block_id: target,
            semantic_hash,
            style_id,
            rationale,
        }),
        Op::SetImageAttrs {
            target,
            drawing_id,
            semantic_hash,
            resize,
            alt_text,
            rationale,
        } => {
            // Non-negativity is enforced in `validate_schema` (which runs before
            // translation); map the wire shape onto the domain type.
            let resize = resize.map(|r| ImageResize {
                cx_emu: r.cx,
                cy_emu: r.cy,
            });
            Ok(EditStep::SetImageAttributes {
                block_id: target,
                drawing_id,
                semantic_hash,
                resize,
                alt_text,
                rationale,
            })
        }
        Op::DeleteImage {
            target,
            drawing_id,
            semantic_hash,
            rationale,
        } => Ok(EditStep::DeleteImage {
            block_id: target,
            drawing_id,
            semantic_hash,
            rationale,
        }),
        Op::SetImageLayout {
            target,
            drawing_id,
            semantic_hash,
            position_h,
            position_v,
            wrap,
            crop,
            rationale,
        } => {
            // Tokens were validated in `validate_schema` (runs before
            // translation); map the wire shapes onto the domain types.
            let patch = ImageLayoutPatch {
                position_h: position_h.map(translate_position_axis),
                position_v: position_v.map(translate_position_axis),
                wrap: wrap.as_deref().map(translate_wrap_token),
                crop: crop.map(|c| ImageCrop {
                    left: c.left,
                    top: c.top,
                    right: c.right,
                    bottom: c.bottom,
                }),
            };
            Ok(EditStep::SetImageLayout {
                block_id: target,
                drawing_id,
                semantic_hash,
                patch,
                rationale,
            })
        }
        Op::CommentCreate {
            target,
            expect,
            body,
            author,
            semantic_hash,
            rationale,
        } => Ok(EditStep::CommentCreate {
            block_id: target,
            expect,
            semantic_hash,
            body,
            author,
            rationale,
        }),
        Op::CommentReply {
            parent_comment_id,
            body,
            author,
            rationale,
        } => Ok(EditStep::CommentReply {
            parent_comment_id,
            body,
            author,
            rationale,
        }),
        Op::CommentResolve {
            comment_id,
            done,
            rationale,
        } => Ok(EditStep::CommentResolve {
            comment_id,
            done,
            rationale,
        }),
        Op::CommentDelete {
            comment_id,
            rationale,
        } => Ok(EditStep::CommentDelete {
            comment_id,
            rationale,
        }),
        Op::InsertNote {
            target,
            expect,
            note_kind,
            body,
            semantic_hash,
            rationale,
        } => Ok(EditStep::InsertNote {
            block_id: target,
            expect,
            semantic_hash,
            note_kind: parse_note_kind(op_index, &note_kind)?,
            body,
            rationale,
        }),
        Op::EditNote {
            note_id,
            note_kind,
            body,
            rationale,
        } => Ok(EditStep::EditNote {
            note_id,
            note_kind: parse_note_kind(op_index, &note_kind)?,
            body,
            rationale,
        }),
        Op::DeleteNote {
            note_id,
            note_kind,
            rationale,
        } => Ok(EditStep::DeleteNote {
            note_id,
            note_kind: parse_note_kind(op_index, &note_kind)?,
            rationale,
        }),
        Op::SetPageSetup {
            target,
            page_size,
            orientation,
            margins,
            columns,
            gutter,
            semantic_hash,
            rationale,
        } => Ok(EditStep::SetPageSetup {
            target: translate_section_target(target),
            patch: translate_page_setup_patch(
                op_index,
                page_size,
                orientation,
                margins,
                columns,
                gutter,
            )?,
            semantic_hash,
            rationale,
        }),
        Op::SetSectionType {
            target,
            section_type,
            semantic_hash,
            rationale,
        } => Ok(EditStep::SetSectionType {
            target: translate_section_target(target),
            section_type: parse_section_type(op_index, &section_type)?,
            semantic_hash,
            rationale,
        }),
        Op::InsertSectionBreak {
            anchor,
            section_type,
            page_size,
            orientation,
            margins,
            columns,
            gutter,
            rationale,
        } => Ok(EditStep::InsertSectionBreak {
            anchor_block_id: anchor,
            section_type: parse_section_type(op_index, &section_type)?,
            properties: translate_page_setup_patch(
                op_index,
                page_size,
                orientation,
                margins,
                columns,
                gutter,
            )?,
            rationale,
        }),
        Op::EditHeader {
            header_part,
            target,
            expect,
            content,
            semantic_hash,
            rationale,
        } => Ok(EditStep::EditHeader {
            story: StoryRef::Header(header_part),
            block_id: target,
            expect,
            semantic_hash,
            content: v4_inlines_to_paragraph_content(op_index, &content)?,
            rationale,
        }),
        Op::EditFooter {
            footer_part,
            target,
            expect,
            content,
            semantic_hash,
            rationale,
        } => Ok(EditStep::EditFooter {
            story: StoryRef::Footer(footer_part),
            block_id: target,
            expect,
            semantic_hash,
            content: v4_inlines_to_paragraph_content(op_index, &content)?,
            rationale,
        }),
        Op::CreateHeader { kind, rationale } => Ok(EditStep::CreateHeader {
            kind: parse_header_footer_kind(op_index, &kind)?,
            rationale,
        }),
        Op::CreateFooter { kind, rationale } => Ok(EditStep::CreateFooter {
            kind: parse_header_footer_kind(op_index, &kind)?,
            rationale,
        }),
        Op::SetHeaderFooterMode {
            title_page,
            even_and_odd,
            link,
            rationale,
        } => Ok(EditStep::SetHeaderFooterMode {
            title_page,
            even_and_odd,
            link: link
                .map(|l| translate_header_footer_link(op_index, l))
                .transpose()?,
            rationale,
        }),
        Op::InsertEquation {
            target,
            expect,
            semantic_hash,
            omml,
            placement,
            rationale,
        } => {
            let placement = match placement.as_str() {
                "inline" => EquationPlacement::Inline,
                "block" => EquationPlacement::Block,
                _ => {
                    return Err(AdapterError::UnknownEquationPlacement {
                        op_index,
                        value: placement,
                    });
                }
            };
            Ok(EditStep::InsertEquation {
                block_id: target,
                expect,
                semantic_hash,
                omml: omml.into_bytes(),
                placement,
                rationale,
            })
        }
        Op::BlocksToTable {
            from,
            to,
            delimiter,
            header,
            rationale,
        } => Ok(EditStep::BlocksToTable {
            from_block_id: from,
            to_block_id: to,
            delimiter,
            header,
            rationale,
        }),
        Op::WrapContentControl {
            target,
            expect,
            semantic_hash,
            tag,
            alias,
            control,
            data_binding,
            rationale,
        } => Ok(EditStep::WrapInContentControl {
            block_id: target,
            expect,
            semantic_hash,
            spec: SdtSpec {
                tag,
                alias,
                control: translate_sdt_control(control),
                binding: data_binding.map(translate_data_binding),
            },
            rationale,
        }),
        Op::WrapBlocksContentControl {
            start_block,
            end_block,
            tag,
            alias,
            control,
            rationale,
        } => Ok(EditStep::WrapBlocksInContentControl {
            start_block_id: start_block,
            end_block_id: end_block,
            spec: SdtSpec {
                tag,
                alias,
                control: translate_sdt_control(control),
                // Block-level wraps never carry a data binding (the inline wrap
                // owns the data-binding path); refused in the verb if supplied.
                binding: None,
            },
            rationale,
        }),
        Op::SetContentControlValue {
            target,
            sdt_id,
            text,
            checked,
            selected,
            tracked,
            rationale,
        } => {
            // Exactly one value kind must be present — enforced in
            // `validate_schema`; map it here.
            let value = match (text, checked, selected) {
                (Some(t), None, None) => SdtValue::Text(t),
                (None, Some(c), None) => SdtValue::Checked(c),
                (None, None, Some(s)) => SdtValue::Selected(s),
                _ => {
                    return Err(AdapterError::AmbiguousContentControlValue { op_index });
                }
            };
            Ok(EditStep::SetContentControlValue {
                block_id: target,
                sdt_id,
                value,
                tracked,
                rationale,
            })
        }
        Op::SetFormFieldValue {
            target,
            field_id,
            text,
            checked,
            selected,
            semantic_hash,
            rationale,
        } => {
            // Exactly one value kind must be present (validated in
            // `validate_schema`); map it onto the distinct FormFieldValue type.
            let value = match (text, checked, selected) {
                (Some(t), None, None) => FormFieldValue::Text(t),
                (None, Some(c), None) => FormFieldValue::Checked(c),
                (None, None, Some(s)) => FormFieldValue::Selected(s),
                _ => {
                    return Err(AdapterError::AmbiguousFormFieldValue { op_index });
                }
            };
            Ok(EditStep::SetFormFieldValue {
                block_id: target,
                field_id,
                value,
                semantic_hash,
                rationale,
            })
        }
        Op::InsertImage {
            target,
            bytes_base64,
            format,
            cx,
            cy,
            alt_text,
            expect,
            semantic_hash,
            rationale,
        } => {
            let image = translate_image_source(op_index, &bytes_base64, &format, cx, cy, alt_text)?;
            Ok(EditStep::InsertImage {
                block_id: target,
                expect,
                semantic_hash,
                image,
                rationale,
            })
        }
        Op::ReplaceImage {
            target,
            drawing_id,
            bytes_base64,
            format,
            cx,
            cy,
            alt_text,
            allow_stretch,
            semantic_hash,
            rationale,
        } => {
            let image = translate_image_source(op_index, &bytes_base64, &format, cx, cy, alt_text)?;
            Ok(EditStep::ReplaceImage {
                block_id: target,
                drawing_id,
                semantic_hash,
                image,
                allow_stretch,
                rationale,
            })
        }
        Op::SetTextboxText {
            target,
            drawing_id,
            paragraphs,
            semantic_hash,
            rationale,
        } => Ok(EditStep::SetTextboxText {
            block_id: target,
            drawing_id,
            paragraphs,
            semantic_hash,
            rationale,
        }),
        Op::OpaqueTextEdit {
            target,
            opaque_id,
            find,
            replacement,
            container_index,
            paragraph_index,
            semantic_hash,
            rationale,
        } => Ok(EditStep::OpaqueTextEdit {
            block_id: target,
            opaque_id,
            container_index,
            paragraph_index,
            find,
            replacement,
            semantic_hash,
            rationale,
        }),
        Op::SdtTextFill {
            block_id,
            sdt_id,
            body_index,
            value,
            semantic_hash,
            rationale,
        } => Ok(EditStep::SdtTextFill {
            block_id,
            sdt_id,
            body_index,
            value,
            semantic_hash,
            rationale,
        }),
        Op::CreateStyle { def, rationale } => Ok(EditStep::CreateStyle {
            def: translate_style_def(op_index, def)?,
            rationale,
        }),
        Op::ModifyStyle { def, rationale } => {
            let style_id = def.style_id.clone();
            Ok(EditStep::ModifyStyle {
                style_id,
                def: translate_style_def(op_index, def)?,
                rationale,
            })
        }
        Op::SetDocDefaults {
            font_family,
            font_size_half_points,
            rationale,
        } => Ok(EditStep::SetDocDefaults {
            font_family,
            font_size_half_points,
            rationale,
        }),
    }
}

/// Decode the base64 bytes, map the format token, and build a validated
/// [`ImageSource`]. Non-negativity of `cx`/`cy` is enforced in `validate_schema`;
/// the magic-byte check happens inside `ImageSource::new`, surfaced here as
/// `UnsupportedImageFormat` (mapped from the engine error) — but a wholly unknown
/// `format` token is refused here before any decode.
/// Map a wrap token to its domain [`ImageWrapType`]; `None` for an unknown
/// token. Used by both `validate_schema` (to reject unknowns) and
/// `translate_wrap_token` (which trusts the validated input).
fn parse_wrap_token(token: &str) -> Option<ImageWrapType> {
    match token {
        "none" => Some(ImageWrapType::None),
        "square" => Some(ImageWrapType::Square),
        "tight" => Some(ImageWrapType::Tight),
        "through" => Some(ImageWrapType::Through),
        "top_and_bottom" => Some(ImageWrapType::TopAndBottom),
        _ => None,
    }
}

/// Translate a validated wrap token. Panics on an unknown token — `validate_schema`
/// guarantees this never happens (it rejects unknowns before translation).
fn translate_wrap_token(token: &str) -> ImageWrapType {
    parse_wrap_token(token).expect("wrap token validated in validate_schema before translation")
}

/// Translate a validated position-axis wire shape. `validate_schema` guarantees
/// exactly one of offset/align is set before translation, so the `else` branch
/// (align) is reached only when `offset` is absent.
fn translate_position_axis(p: ImagePositionWire) -> ImagePositionAxis {
    match p.offset {
        Some(offset_emu) => ImagePositionAxis::Offset {
            relative_from: p.relative_from,
            offset_emu,
        },
        None => ImagePositionAxis::Align {
            relative_from: p.relative_from,
            // `validate_schema` rejected the both-absent case, so align is Some.
            align: p
                .align
                .expect("position axis validated to have offset xor align"),
        },
    }
}

/// EMUs per pixel at 96 DPI: 914400 EMU/inch ÷ 96 px/inch. The intrinsic-size
/// default (caller omits cx/cy) converts pixel dimensions to a display box at
/// this density, matching Word's own 96-DPI assumption for screen images.
const EMU_PER_PIXEL_96DPI: i64 = 9525;

/// Resolve the display extent for an image whose caller may have omitted one or
/// both of `cx`/`cy`. Both present → used verbatim (historical behavior). Both
/// omitted → the intrinsic pixel dimensions at 96 DPI. Exactly one present → the
/// other is derived from the intrinsic aspect ratio. A header we cannot decode
/// to positive pixel dimensions is refused (never a default size) — CLAUDE.md
/// "no silent fallbacks".
fn resolve_image_extent(
    op_index: usize,
    fmt: ImageFormat,
    bytes: &[u8],
    cx: Option<i64>,
    cy: Option<i64>,
) -> Result<(i64, i64), AdapterError> {
    if let (Some(cx), Some(cy)) = (cx, cy) {
        return Ok((cx, cy));
    }
    let (iw, ih) = match fmt.intrinsic_dimensions(bytes) {
        Some((w, h)) if w > 0 && h > 0 => (w as i64, h as i64),
        _ => {
            return Err(AdapterError::ImageDimensionsUndecodable {
                op_index,
                format: fmt.content_type(),
                len: bytes.len(),
            });
        }
    };
    let extent = match (cx, cy) {
        (None, None) => (iw * EMU_PER_PIXEL_96DPI, ih * EMU_PER_PIXEL_96DPI),
        // One side fixed: preserve the intrinsic aspect ratio for the other.
        (Some(cx), None) => (cx, cx * ih / iw),
        (None, Some(cy)) => (cy * iw / ih, cy),
        (Some(_), Some(_)) => unreachable!("both-present handled above"),
    };
    Ok(extent)
}

fn translate_image_source(
    op_index: usize,
    bytes_base64: &str,
    format: &str,
    cx: Option<i64>,
    cy: Option<i64>,
    alt_text: Option<String>,
) -> Result<ImageSource, AdapterError> {
    use base64::Engine as _;
    let fmt = match format {
        "png" => ImageFormat::Png,
        "jpeg" | "jpg" => ImageFormat::Jpeg,
        "gif" => ImageFormat::Gif,
        other => {
            return Err(AdapterError::UnknownImageFormat {
                op_index,
                value: other.to_string(),
            });
        }
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(bytes_base64.as_bytes())
        .map_err(|e| AdapterError::InvalidImageBase64 {
            op_index,
            reason: e.to_string(),
        })?;
    // Resolve the display box (intrinsic-size default / one-sided derivation)
    // before building the source — a decode failure is a loud refusal, not a
    // guessed size.
    let (cx, cy) = resolve_image_extent(op_index, fmt, &bytes, cx, cy)?;
    // `ImageSource::new` fails loud on empty bytes / magic mismatch. Those are
    // engine `EditError`s; surface them as adapter errors with the op index so a
    // wire caller gets a uniform error type.
    ImageSource::new(bytes, fmt, cx, cy, alt_text, op_index).map_err(|e| {
        AdapterError::InvalidImageBase64 {
            op_index,
            reason: e.to_string(),
        }
    })
}

/// Map the wire style definition onto the domain [`StyleDefinition`], refusing
/// unknown `style_type` / `alignment` tokens (NEVER defaulted).
fn translate_style_def(
    op_index: usize,
    def: StyleDefinitionWire,
) -> Result<StyleDefinition, AdapterError> {
    let style_type = match def.style_type.as_str() {
        "para" | "paragraph" => StyleType::Para,
        "char" | "character" => StyleType::Char,
        "table" => StyleType::Table,
        "numbering" => StyleType::Numbering,
        other => {
            return Err(AdapterError::UnknownStyleType {
                op_index,
                value: other.to_string(),
            });
        }
    };
    let alignment = match def.para_props.alignment {
        Some(token) => Some(parse_style_alignment(op_index, &token)?),
        None => None,
    };
    Ok(StyleDefinition {
        style_id: def.style_id,
        style_type,
        based_on: def.based_on,
        name: def.name,
        run_props: StyleRunProps {
            bold: def.run_props.bold,
            italic: def.run_props.italic,
            underline: def.run_props.underline,
            font_size_half_points: def.run_props.font_size_half_points,
            color: def.run_props.color,
            font_family: def.run_props.font_family,
        },
        para_props: StyleParaProps {
            alignment,
            spacing_before: def.para_props.spacing_before,
            spacing_after: def.para_props.spacing_after,
            line_spacing: def.para_props.line_spacing,
            indent_left: def.para_props.indent_left,
            indent_right: def.para_props.indent_right,
            indent_first_line: def.para_props.indent_first_line,
        },
    })
}

/// Parse a style-definition alignment token to [`Alignment`], reusing the same
/// vocabulary as paragraph-format alignment. Unknown tokens are refused.
fn parse_style_alignment(op_index: usize, token: &str) -> Result<Alignment, AdapterError> {
    match token {
        "left" | "start" => Ok(Alignment::Left),
        "center" => Ok(Alignment::Center),
        "right" | "end" => Ok(Alignment::Right),
        "justify" | "both" => Ok(Alignment::Justify),
        "distribute" => Ok(Alignment::Distribute),
        other => Err(AdapterError::UnknownStyleAlignment {
            op_index,
            value: other.to_string(),
        }),
    }
}

/// Map the wire content-control type onto the domain [`SdtControl`].
fn translate_sdt_control(control: SdtControlWire) -> SdtControl {
    match control {
        SdtControlWire::PlainText => SdtControl::PlainText,
        SdtControlWire::RichText => SdtControl::RichText,
        SdtControlWire::Dropdown { items } => SdtControl::Dropdown {
            items: items.into_iter().map(translate_list_item).collect(),
        },
        SdtControlWire::ComboBox { items } => SdtControl::ComboBox {
            items: items.into_iter().map(translate_list_item).collect(),
        },
        SdtControlWire::Checkbox { checked } => SdtControl::Checkbox { checked },
        SdtControlWire::Date => SdtControl::Date,
        SdtControlWire::RepeatingSection => SdtControl::RepeatingSection,
    }
}

fn translate_list_item(item: SdtListItemWire) -> SdtListItem {
    SdtListItem {
        display: item.display,
        value: item.value,
    }
}

/// Map the wire data binding onto the domain [`DataBinding`]. Emptiness was
/// already rejected in `validate_schema` (`MalformedDataBinding`).
fn translate_data_binding(b: DataBindingWire) -> DataBinding {
    DataBinding {
        xpath: b.xpath,
        store_item_id: b.store_item_id,
        prefix_mappings: b.prefix_mappings,
    }
}

/// Map the wire section target to the engine [`SectionTarget`].
fn translate_section_target(target: SectionTargetWire) -> SectionTarget {
    match target {
        // The only legal `section` value is "body" today; any other string is
        // still treated as the body section (the wire enum only carries the
        // discriminant, and there is no other document-level section).
        SectionTargetWire::Section(_) => SectionTarget::Body,
        SectionTargetWire::Paragraph(id) => SectionTarget::Paragraph(id),
    }
}

/// Assemble a [`PageSetupPatch`] from the wire fields, rejecting an unknown
/// orientation token (NEVER defaulted).
fn translate_page_setup_patch(
    op_index: usize,
    page_size: Option<PageSizeWire>,
    orientation: Option<String>,
    margins: Option<PageMarginsWire>,
    columns: Option<ColumnLayoutWire>,
    gutter: Option<u32>,
) -> Result<PageSetupPatch, AdapterError> {
    let orientation = match orientation {
        Some(token) => Some(parse_orientation(op_index, &token)?),
        None => None,
    };
    Ok(PageSetupPatch {
        page_size: page_size.map(|s| PageSize {
            width: s.width,
            height: s.height,
        }),
        orientation,
        margins: margins.map(|m| PageMargins {
            top: m.top,
            bottom: m.bottom,
            left: m.left,
            right: m.right,
            header: m.header,
            footer: m.footer,
        }),
        columns: columns.map(|c| ColumnLayout {
            count: c.count,
            space: c.space,
        }),
        gutter,
    })
}

/// Map a wire section-type token to the domain [`SectionType`]. Unknown tokens
/// are refused (`UnknownSectionType`) — NEVER mapped to a default.
fn parse_section_type(op_index: usize, token: &str) -> Result<SectionType, AdapterError> {
    match token {
        "next_page" => Ok(SectionType::NextPage),
        "continuous" => Ok(SectionType::Continuous),
        "even_page" => Ok(SectionType::EvenPage),
        "odd_page" => Ok(SectionType::OddPage),
        "next_column" => Ok(SectionType::NextColumn),
        _ => Err(AdapterError::UnknownSectionType {
            op_index,
            value: token.to_string(),
        }),
    }
}

/// Map a wire orientation token to the domain [`PageOrientation`]. Unknown
/// tokens are refused (`UnknownOrientation`).
fn parse_orientation(op_index: usize, token: &str) -> Result<PageOrientation, AdapterError> {
    match token {
        "portrait" => Ok(PageOrientation::Portrait),
        "landscape" => Ok(PageOrientation::Landscape),
        _ => Err(AdapterError::UnknownOrientation {
            op_index,
            value: token.to_string(),
        }),
    }
}

/// Map a wire header/footer-kind token to the domain [`HeaderFooterKind`].
fn parse_header_footer_kind(
    op_index: usize,
    token: &str,
) -> Result<HeaderFooterKind, AdapterError> {
    match token {
        "default" => Ok(HeaderFooterKind::Default),
        "first" => Ok(HeaderFooterKind::First),
        "even" => Ok(HeaderFooterKind::Even),
        _ => Err(AdapterError::UnknownHeaderFooterKind {
            op_index,
            value: token.to_string(),
        }),
    }
}

fn translate_header_footer_link(
    op_index: usize,
    link: HeaderFooterLinkWire,
) -> Result<HeaderFooterLink, AdapterError> {
    Ok(HeaderFooterLink {
        is_header: link.is_header,
        kind: parse_header_footer_kind(op_index, &link.kind)?,
        link: link.link,
    })
}

/// Map the wire `note_kind` token to [`NoteKind`]. Unknown values are refused
/// (`UnknownNoteKind`) — NEVER silently defaulted to footnote.
fn parse_note_kind(op_index: usize, token: &str) -> Result<NoteKind, AdapterError> {
    match token {
        "footnote" => Ok(NoteKind::Footnote),
        "endnote" => Ok(NoteKind::Endnote),
        _ => Err(AdapterError::UnknownNoteKind {
            op_index,
            value: token.to_string(),
        }),
    }
}

/// Map an alignment token to the domain `Alignment`. v1 accepts the five common
/// values; an unknown token is refused (no silent `Other`).
fn parse_alignment(op_index: usize, token: &str) -> Result<Alignment, AdapterError> {
    match token {
        "left" => Ok(Alignment::Left),
        "center" => Ok(Alignment::Center),
        "right" => Ok(Alignment::Right),
        "both" => Ok(Alignment::Justify),
        "distribute" => Ok(Alignment::Distribute),
        _ => Err(AdapterError::UnknownAlignment {
            op_index,
            value: token.to_string(),
        }),
    }
}

/// Map a line-rule token to the domain `LineSpacingRule`. An unknown token is
/// refused rather than coerced.
fn parse_line_rule(op_index: usize, token: &str) -> Result<LineSpacingRule, AdapterError> {
    match token {
        "auto" => Ok(LineSpacingRule::Auto),
        "exact" => Ok(LineSpacingRule::Exact),
        "at_least" => Ok(LineSpacingRule::AtLeast),
        _ => Err(AdapterError::UnknownLineRule {
            op_index,
            value: token.to_string(),
        }),
    }
}

/// A valid wire `color` is `auto` or six hex digits (§17.18.79). Mirrors the
/// verb-edge check in `verbs::run_formatting` so the wire fails loud too.
fn is_valid_color(value: &str) -> bool {
    value == "auto" || (value.len() == 6 && value.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Parse one wire [`BorderPatch`] into a domain [`Border`]. `style` is mapped
/// through `BorderStyle::from_xml_str` (unknown token rejected); `color` is
/// validated as `auto`/hex. Refuse rather than coerce (no silent fallback).
fn parse_border(op_index: usize, p: &BorderPatch) -> Result<Border, AdapterError> {
    let style =
        BorderStyle::from_xml_str(&p.style).map_err(|_| AdapterError::UnknownBorderStyle {
            op_index,
            value: p.style.clone(),
        })?;
    if let Some(c) = &p.color
        && !is_valid_color(c)
    {
        return Err(AdapterError::InvalidBorderColor {
            op_index,
            value: c.clone(),
        });
    }
    Ok(Border {
        style,
        color: p.color.clone(),
        size: p.size,
        space: p.space,
        extra_attrs: Vec::new(),
    })
}

/// Parse a wire [`ParaBordersPatch`] into a domain [`ParagraphBorders`]. Returns
/// `None` when the patch is empty (treated as "no borders requested").
fn parse_para_borders(
    op_index: usize,
    p: &ParaBordersPatch,
) -> Result<Option<ParagraphBorders>, AdapterError> {
    if p.is_empty() {
        return Ok(None);
    }
    let edge = |b: &Option<BorderPatch>| -> Result<Option<Border>, AdapterError> {
        b.as_ref().map(|bp| parse_border(op_index, bp)).transpose()
    };
    Ok(Some(ParagraphBorders {
        top: edge(&p.top)?,
        bottom: edge(&p.bottom)?,
        left: edge(&p.left)?,
        right: edge(&p.right)?,
        between: edge(&p.between)?,
        bar: edge(&p.bar)?,
    }))
}

/// Parse a wire [`BorderSetPatch`] into a domain [`BorderSet`] (cell/table
/// borders). Returns `None` when the patch is empty.
fn parse_border_set(
    op_index: usize,
    p: &BorderSetPatch,
) -> Result<Option<BorderSet>, AdapterError> {
    if p.is_empty() {
        return Ok(None);
    }
    let edge = |b: &Option<BorderPatch>| -> Result<Option<Border>, AdapterError> {
        b.as_ref().map(|bp| parse_border(op_index, bp)).transpose()
    };
    Ok(Some(BorderSet {
        top: edge(&p.top)?,
        bottom: edge(&p.bottom)?,
        left: edge(&p.left)?,
        right: edge(&p.right)?,
        inside_h: edge(&p.inside_h)?,
        inside_v: edge(&p.inside_v)?,
    }))
}

/// Parse a wire [`ShadingWire`] into a domain [`Shading`]. `fill`/`color` are
/// validated as `auto`/hex; `pattern` is mapped through
/// `ShadingPattern::from_xml_str` (unknown token rejected). Returns `None` when
/// the patch is empty.
fn parse_shading(op_index: usize, p: &ShadingWire) -> Result<Option<Shading>, AdapterError> {
    if p.is_empty() {
        return Ok(None);
    }
    if let Some(c) = &p.fill
        && !is_valid_color(c)
    {
        return Err(AdapterError::InvalidShadingColor {
            op_index,
            value: c.clone(),
        });
    }
    if let Some(c) = &p.color
        && !is_valid_color(c)
    {
        return Err(AdapterError::InvalidShadingColor {
            op_index,
            value: c.clone(),
        });
    }
    let val = match &p.pattern {
        Some(t) => Some(ShadingPattern::from_xml_str(t).map_err(|_| {
            AdapterError::UnknownShadingPattern {
                op_index,
                value: t.clone(),
            }
        })?),
        None => None,
    };
    Ok(Some(Shading {
        fill: p.fill.clone(),
        val,
        color: p.color.clone(),
        extra_attrs: Vec::new(),
    }))
}

/// Parse a wire [`MeasurementPatch`] into a domain [`TableMeasurement`]. The
/// `width_type` token is mapped through `WidthType::from_xml_str` (unknown
/// token rejected).
fn parse_measurement(
    op_index: usize,
    p: &MeasurementPatch,
) -> Result<TableMeasurement, AdapterError> {
    let width_type =
        WidthType::from_xml_str(&p.width_type).map_err(|_| AdapterError::UnknownWidthType {
            op_index,
            value: p.width_type.clone(),
        })?;
    Ok(TableMeasurement {
        w: p.w,
        width_type,
        // Programmatic widths are canonical-form: no source percent literal.
        pct_literal: None,
    })
}

/// Map a vertical-alignment token to the domain [`VerticalAlignment`]. Refuse
/// an unknown token rather than coerce.
fn parse_v_align(op_index: usize, token: &str) -> Result<VerticalAlignment, AdapterError> {
    match token {
        "top" => Ok(VerticalAlignment::Top),
        "center" => Ok(VerticalAlignment::Center),
        "bottom" => Ok(VerticalAlignment::Bottom),
        _ => Err(AdapterError::UnknownVerticalAlignment {
            op_index,
            value: token.to_string(),
        }),
    }
}

/// Map a height-rule token to the domain [`HeightRule`] (`ST_HeightRule`,
/// §17.18.37). Refuse an unknown token rather than coerce.
fn parse_height_rule(op_index: usize, token: &str) -> Result<HeightRule, AdapterError> {
    HeightRule::from_xml_str(token).map_err(|_| AdapterError::UnknownHeightRule {
        op_index,
        value: token.to_string(),
    })
}

/// Translate a wire [`SpanSelector`] into the engine's [`ResolvedSpanSelector`].
/// `None` (no `span`) stays `None`; `"whole"` maps to `Whole`; a `s_<n>` token
/// maps to `Handle`. Anchors are resolved by durable id (never substring).
/// `"start"`/`"end"` endpoints map to the block boundaries.
fn resolve_span_selector(
    _op_index: usize,
    span: Option<SpanSelector>,
) -> Result<Option<ResolvedSpanSelector>, AdapterError> {
    let Some(span) = span else {
        return Ok(None);
    };
    let resolved = match span {
        SpanSelector::Token(t) if t == "whole" => ResolvedSpanSelector::Whole,
        SpanSelector::Token(handle) => ResolvedSpanSelector::Handle(handle),
        SpanSelector::After { after } => ResolvedSpanSelector::AnchorAfter(after),
        SpanSelector::Before { before } => ResolvedSpanSelector::AnchorBefore(before),
        SpanSelector::Between { between } => {
            let [start, end] = between;
            ResolvedSpanSelector::Between {
                start: resolve_endpoint(start),
                end: resolve_endpoint(end),
            }
        }
    };
    Ok(Some(resolved))
}

/// Map a wire [`SpanEndpoint`] token to a [`ResolvedSpanEndpoint`]: `"start"` /
/// `"end"` are block boundaries; any other string is an opaque anchor id.
fn resolve_endpoint(endpoint: SpanEndpoint) -> ResolvedSpanEndpoint {
    let SpanEndpoint::Token(t) = endpoint;
    match t.as_str() {
        "start" => ResolvedSpanEndpoint::Start,
        "end" => ResolvedSpanEndpoint::End,
        _ => ResolvedSpanEndpoint::Anchor(NodeId::from(t)),
    }
}

/// Translate a `replace` op carrying a sub-block span into `ReplaceSpanText`.
/// Only paragraph payloads are addressable by span (`SpanOnNonParagraph` is
/// enforced at the schema layer); a hyperlink/table payload here is a bug.
/// Likewise the guard: `validate_schema` requires it on span ops
/// (`SpanRequiresGuard`), so its absence here is a bug. The op's `expect` is
/// span-scoped on this path: the resolved range's exact visible text (the
/// text-identity check), not the whole-paragraph advisory substring.
fn translate_span_replace(
    op_index: usize,
    target: NodeId,
    content: ReplaceContent,
    span: ResolvedSpanSelector,
    expect: Option<String>,
    semantic_hash: Option<String>,
    rationale: Option<String>,
) -> Result<EditStep, AdapterError> {
    let Some(guard) = semantic_hash else {
        unreachable!("validate_schema requires a guard on span replace ops")
    };
    match content {
        ReplaceContent::Block(Block::Paragraph { content, .. }) => {
            let content = v4_inlines_to_paragraph_content(op_index, &content)?;
            Ok(EditStep::ReplaceSpanText {
                block_id: target,
                guard,
                expect,
                span,
                content,
                rationale,
            })
        }
        // `validate_schema` rejects a span on a non-paragraph payload before
        // translation, so these are unreachable on the validated path.
        ReplaceContent::Block(Block::Table { .. }) => {
            unreachable!("validate_schema rejects a span on a table replace payload")
        }
        ReplaceContent::Block(Block::Toc { .. }) => {
            unreachable!("validate_schema rejects a toc replace payload (TocNotReplaceable)")
        }
        ReplaceContent::Inline(_) => {
            unreachable!("validate_schema rejects a span on a hyperlink/inline replace payload")
        }
    }
}

fn translate_replace(
    op_index: usize,
    target: NodeId,
    content: ReplaceContent,
    expect: Option<String>,
    semantic_hash: Option<String>,
    rationale: Option<String>,
) -> Result<EditStep, AdapterError> {
    match content {
        ReplaceContent::Block(Block::Paragraph { role, content, .. }) => {
            // Unified-guard contract: a staleness precondition is required, but
            // it may be EITHER the block `guard`/`semantic_hash` OR the legacy
            // `expect` substring. When a guard is present `expect` is advisory
            // and may be absent (passed through as empty). When NO guard is
            // present, `expect` is the authoritative gate and is required.
            let expect = match (expect, semantic_hash.is_some()) {
                (Some(e), _) => e,
                (None, true) => String::new(),
                (None, false) => return Err(AdapterError::MissingExpect { op_index }),
            };
            let content = v4_inlines_to_paragraph_content(op_index, &content)?;
            Ok(EditStep::ReplaceParagraphText {
                block_id: target,
                rationale,
                replacement_role: role,
                expect,
                semantic_hash,
                content,
            })
        }
        ReplaceContent::Block(Block::Table { content, attrs, .. }) => {
            // No `expect` precondition on table replace: a table has no
            // single flat text section to anchor an expect substring
            // against. The engine relies on `table_id` (must still
            // resolve to a table) and the optional `semantic_hash` for
            // stale-snapshot detection.
            let table_spec = v4_table_rows_to_table_spec(op_index, &content, attrs.as_ref())?;
            Ok(EditStep::ReplaceTable {
                block_id: target,
                rationale,
                semantic_hash,
                replacement: table_spec,
            })
        }
        ReplaceContent::Inline(Inline::Hyperlink { attrs, content }) => {
            // `replace(hyperlink, ...)` preserves the URL/anchor; only the
            // display text changes. The payload's href/anchor become
            // preconditions the engine validates against the target — if
            // they differ, the engine fails with HyperlinkAttrMismatch and
            // the caller is told to use `set_attr` instead. This makes the
            // wire-format trade-off (Hyperlink type carries attrs because
            // hyperlinks always do) honest: the attrs are preconditions,
            // not edits.
            if attrs.title.is_some() {
                return Err(AdapterError::HyperlinkTitleNotSupported { op_index });
            }
            let new_text = flatten_hyperlink_content_to_text(op_index, &content)?;
            let expect = expect.ok_or(AdapterError::MissingExpect { op_index })?;
            Ok(EditStep::ReplaceHyperlinkText {
                hyperlink_id: target,
                rationale,
                expect,
                new_text,
                expect_href: attrs.href,
                expect_anchor: attrs.anchor,
            })
        }
        // Unaddressable variants were rejected by `validate_schema`. The
        // compiler can prove these are unreachable once we exhaust the
        // ReplaceContent enum, so the explicit match keeps the surface
        // exhaustive without a catch-all.
        ReplaceContent::Block(Block::Toc { .. })
        | ReplaceContent::Inline(Inline::Text { .. })
        | ReplaceContent::Inline(Inline::OpaqueRef { .. }) => {
            unreachable!(
                "schema validation rejects toc/text/opaque_ref replace payloads before translation"
            )
        }
    }
}

/// Lift a wire `ListSpecWire` onto the engine's `InsertListSpec`. A pure
/// coordinate copy: the engine resolves the numId against the document and
/// fails loud on an unknown one at apply time (`InsertListNumIdUnknown`).
fn insert_list_spec_from_wire(w: ListSpecWire) -> InsertListSpec {
    InsertListSpec {
        num_id: w.num_id,
        ilvl: w.ilvl,
    }
}

fn translate_insert_blocks(
    op_index: usize,
    content: &[Block],
) -> Result<Vec<BlockSpec>, AdapterError> {
    let mut out = Vec::with_capacity(content.len());
    for block in content {
        match block {
            Block::Paragraph {
                role,
                content,
                attrs,
                list,
            } => {
                let restart_numbering =
                    attrs.as_ref().map(|a| a.restart_numbering).unwrap_or(false);
                let content = v4_inlines_to_paragraph_content(op_index, content)?;
                out.push(BlockSpec::Paragraph(ParagraphBlockSpec {
                    role: role.clone(),
                    content,
                    restart_numbering,
                    list: list.map(insert_list_spec_from_wire),
                }));
            }
            Block::Table { content, attrs, .. } => {
                let table_spec = v4_table_rows_to_table_spec(op_index, content, attrs.as_ref())?;
                out.push(BlockSpec::Table(table_spec));
            }
            Block::Toc { levels } => {
                // Product default levels: Word's own "Automatic Table of
                // Contents" range (1-3), used whenever the caller omits
                // `levels`. Test-covered by
                // `toc_insert_omitted_levels_uses_default_range`.
                let levels = levels
                    .map(|l| TocLevelsSpec {
                        from: l.from,
                        to: l.to,
                    })
                    .unwrap_or(TocLevelsSpec { from: 1, to: 3 });
                out.push(BlockSpec::Toc(TocBlockSpec {
                    // Product default role: the wire never asks the caller
                    // for an internal role token for a ToC (unlike
                    // `Paragraph`/`Table`). `None` here resolves, at apply
                    // time, against the document's default body role via the
                    // same `"default"` alias `resolve_role_entry` already
                    // accepts for paragraph inserts (see
                    // `resolve_toc_spec`). Test-covered by
                    // `toc_insert_without_role_uses_default_body_role`.
                    role: None,
                    levels,
                    // Word's default TOC field switches (`TOC \o "1-3" \h \z
                    // \u`): hyperlinked entries, page numbers hidden in web
                    // layout, outline levels included in addition to the
                    // built-in heading styles. Documented product default;
                    // test-covered by
                    // `toc_insert_default_instruction_text`. The wire does
                    // not expose these three switches individually — day-one
                    // scope ships Word's own default, not a configurable
                    // surface with no current caller demand.
                    include_hyperlinks: true,
                    hide_page_numbers_in_web: true,
                    use_outline_levels: true,
                }));
            }
        }
    }
    Ok(out)
}

fn translate_set_attr(
    op_index: usize,
    target: NodeId,
    attrs: AttrPatch,
    expect_href: Option<String>,
    expect_anchor: Option<String>,
    rationale: Option<String>,
) -> Result<EditStep, AdapterError> {
    let has_paragraph_field = attrs.role.is_some();
    let has_hyperlink_field =
        attrs.href.is_some() || attrs.anchor.is_some() || attrs.title.is_some();

    if has_paragraph_field && has_hyperlink_field {
        return Err(AdapterError::AmbiguousSetAttrAttrs { op_index });
    }
    if has_hyperlink_field {
        // `title` is out of scope (the IR does not carry it). Reject loudly
        // rather than ignoring the field.
        if attrs.title.is_some() {
            return Err(AdapterError::HyperlinkTitleNotSupported { op_index });
        }
        // Optimistic-concurrency contract: when the caller mutates a
        // hyperlink attribute, they must also assert the current value.
        // Without this, a stale caller could retarget the wrong link
        // entirely. CLAUDE.md "invalid states hard."
        if attrs.href.is_some() && expect_href.is_none() {
            return Err(AdapterError::MissingHyperlinkAttrExpect {
                op_index,
                attr: "href",
            });
        }
        if attrs.anchor.is_some() && expect_anchor.is_none() {
            return Err(AdapterError::MissingHyperlinkAttrExpect {
                op_index,
                attr: "anchor",
            });
        }
        // The v4 wire shape lets the LLM omit `anchor` entirely (field
        // absent) or set it to a value. There is currently no surface for
        // "clear the anchor" — the LLM cannot send `anchor: null` because
        // `Option<String>` does not distinguish absent from null in this
        // grammar. We forward `Some(Some(s))` for the set case; clearing
        // would require a schema-layer addition later.
        let new_anchor = attrs.anchor.map(Some);
        return Ok(EditStep::SetHyperlinkAttr {
            hyperlink_id: target,
            new_href: attrs.href,
            new_anchor,
            expect_href,
            expect_anchor,
            rationale,
        });
    }
    // Paragraph case.
    let role = attrs
        .role
        .expect("schema check guarantees at least one field set");
    Ok(EditStep::SetBlockRangeAttr {
        from_block_id: target.clone(),
        to_block_id: target,
        role,
        rationale,
    })
}

/// Translate a v4 `Vec<TableRow>` payload into the engine's
/// `TableBlockSpec`. Recursively converts every cell's `Vec<Block>` so
/// nested tables and paragraphs are handled by the same `BlockSpec`
/// pipeline as a top-level insert.
///
/// Schema-level emptiness (no rows / no cells / no blocks) was rejected
/// earlier by `validate_schema`, so these structural invariants are
/// guaranteed at this point; the function does not re-check them.
/// Map a wire `TableOpWire` onto the engine's `TableOp`. `position` strings are
/// parsed with no silent fallback (`UnknownTablePosition` on anything other than
/// `before` / `after`).
fn translate_table_op(op_index: usize, op: TableOpWire) -> Result<TableOp, AdapterError> {
    let parse_pos = |value: String| -> Result<TableInsertPosition, AdapterError> {
        match value.as_str() {
            "before" => Ok(TableInsertPosition::Before),
            "after" => Ok(TableInsertPosition::After),
            _ => Err(AdapterError::UnknownTablePosition { op_index, value }),
        }
    };
    match op {
        TableOpWire::InsertRow {
            ref_row,
            position,
            cells,
        } => Ok(TableOp::InsertRow {
            ref_row,
            position: parse_pos(position)?,
            cells,
        }),
        TableOpWire::DeleteRow { row_index } => Ok(TableOp::DeleteRow { row_index }),
        TableOpWire::InsertColumn { ref_col, position } => Ok(TableOp::InsertColumn {
            ref_col,
            position: parse_pos(position)?,
        }),
        TableOpWire::DeleteColumn { col_index } => Ok(TableOp::DeleteColumn { col_index }),
        TableOpWire::MergeCells {
            start_row,
            start_col,
            end_row,
            end_col,
        } => Ok(TableOp::MergeCells {
            start_row,
            start_col,
            end_row,
            end_col,
        }),
        TableOpWire::SetCellText {
            row_index,
            col_index,
            text,
        } => Ok(TableOp::SetCellText {
            row_index,
            col_index,
            text,
        }),
    }
}

fn v4_table_rows_to_table_spec(
    op_index: usize,
    rows: &[TableRow],
    table_attrs: Option<&TableAttrs>,
) -> Result<TableBlockSpec, AdapterError> {
    let mut row_specs: Vec<TableRowSpec> = Vec::with_capacity(rows.len());
    for row in rows {
        let mut cell_specs: Vec<TableCellSpec> = Vec::with_capacity(row.content.len());
        for cell in &row.content {
            let mut block_specs: Vec<BlockSpec> = Vec::with_capacity(cell.content.len());
            for block in &cell.content {
                match block {
                    Block::Paragraph {
                        role,
                        content,
                        attrs,
                        list,
                    } => {
                        let restart_numbering =
                            attrs.as_ref().map(|a| a.restart_numbering).unwrap_or(false);
                        let content = v4_inlines_to_paragraph_content(op_index, content)?;
                        block_specs.push(BlockSpec::Paragraph(ParagraphBlockSpec {
                            role: role.clone(),
                            content,
                            restart_numbering,
                            list: list.map(insert_list_spec_from_wire),
                        }));
                    }
                    Block::Table { content, attrs, .. } => {
                        let nested =
                            v4_table_rows_to_table_spec(op_index, content, attrs.as_ref())?;
                        block_specs.push(BlockSpec::Table(nested));
                    }
                    Block::Toc { .. } => {
                        unreachable!(
                            "validate_schema rejects a toc block nested in a table cell \
                             (TocNotAllowedInTableCell)"
                        )
                    }
                }
            }
            // Merge state + caller-set cell formatting from the cell attrs
            // (RFC-0003 Item 1). Absent attrs mean a single, unformatted cell.
            let (merge_h, merge_v, formatting) = match &cell.attrs {
                None => (None, None, None),
                Some(attrs) => {
                    let merge_v = attrs.v_merge.map(|w| match w {
                        VMergeWire::Restart => VerticalMergeSpec::Restart,
                        VMergeWire::Continue => VerticalMergeSpec::Continue,
                    });
                    let formatting = cell_formatting_from_attrs(op_index, attrs)?;
                    (attrs.grid_span, merge_v, formatting)
                }
            };
            cell_specs.push(TableCellSpec {
                content: block_specs,
                merge_h,
                merge_v,
                formatting,
            });
        }
        let (is_header, height, height_rule) = match &row.attrs {
            None => (false, None, None),
            Some(a) => {
                let height_rule = match &a.height_rule {
                    Some(t) => Some(parse_height_rule(op_index, t)?),
                    None => None,
                };
                (a.header, a.height, height_rule)
            }
        };
        row_specs.push(TableRowSpec {
            cells: cell_specs,
            is_header,
            height,
            height_rule,
        });
    }
    let formatting = match table_attrs {
        Some(a) => table_formatting_from_attrs(op_index, a)?,
        None => None,
    };
    Ok(TableBlockSpec {
        rows: row_specs,
        formatting,
    })
}

/// Build caller-set CELL formatting (`tcPr`) from wire cell attrs (RFC-0003
/// Item 1). Returns `None` when no formatting field is present. Each value is
/// parsed/validated at the edge (fail-loud on a bad token).
fn cell_formatting_from_attrs(
    op_index: usize,
    attrs: &TableCellAttrs,
) -> Result<Option<CellFormatting>, AdapterError> {
    let borders = match &attrs.borders {
        Some(p) => parse_border_set(op_index, p)?,
        None => None,
    };
    let shading = match &attrs.shading {
        Some(p) => parse_shading(op_index, p)?,
        None => None,
    };
    let width = match &attrs.width {
        Some(p) => Some(parse_measurement(op_index, p)?),
        None => None,
    };
    let v_align = match &attrs.v_align {
        Some(t) => Some(parse_v_align(op_index, t)?),
        None => None,
    };
    let margins = attrs.margins.as_ref().and_then(|p| {
        if p.is_empty() {
            None
        } else {
            Some(CellMargins {
                top: p.top,
                bottom: p.bottom,
                left: p.left,
                right: p.right,
            })
        }
    });
    if borders.is_none()
        && shading.is_none()
        && width.is_none()
        && v_align.is_none()
        && margins.is_none()
    {
        return Ok(None);
    }
    Ok(Some(CellFormatting {
        borders,
        shading,
        width,
        v_align,
        margins,
        ..CellFormatting::default()
    }))
}

/// Build caller-set TABLE formatting (`tblPr`) from wire table attrs (RFC-0003
/// Item 1). Returns `None` when no formatting field is present.
fn table_formatting_from_attrs(
    op_index: usize,
    attrs: &TableAttrs,
) -> Result<Option<TableFormatting>, AdapterError> {
    let style_id = attrs.style.clone().map(crate::domain::IStr::from);
    let borders = match &attrs.borders {
        Some(p) => parse_border_set(op_index, p)?,
        None => None,
    };
    let width = match &attrs.width {
        Some(p) => Some(parse_measurement(op_index, p)?),
        None => None,
    };
    let default_cell_margins = attrs.cell_margins.as_ref().and_then(|p| {
        if p.is_empty() {
            None
        } else {
            Some(CellMargins {
                top: p.top,
                bottom: p.bottom,
                left: p.left,
                right: p.right,
            })
        }
    });
    if style_id.is_none() && borders.is_none() && width.is_none() && default_cell_margins.is_none()
    {
        return Ok(None);
    }
    Ok(Some(TableFormatting {
        style_id,
        borders,
        width,
        default_cell_margins,
        ..TableFormatting::default()
    }))
}

fn v4_inlines_to_paragraph_content(
    op_index: usize,
    inlines: &[Inline],
) -> Result<ParagraphContent, AdapterError> {
    let mut fragments: Vec<ContentFragment> = Vec::with_capacity(inlines.len());
    for inline in inlines {
        match inline {
            Inline::Text { text, marks } => {
                if marks.is_empty() {
                    fragments.push(ContentFragment::Text(text.clone()));
                } else {
                    let mark_set = v4_marks_to_inline_mark_set(op_index, marks)?;
                    fragments.push(ContentFragment::StyledText {
                        text: text.clone(),
                        marks: mark_set,
                    });
                }
            }
            Inline::Hyperlink { attrs, content } => {
                if attrs.title.is_some() {
                    return Err(AdapterError::HyperlinkTitleNotSupported { op_index });
                }
                let text = flatten_hyperlink_content_to_text(op_index, content)?;
                fragments.push(ContentFragment::NewHyperlink {
                    href: attrs.href.clone(),
                    anchor: attrs.anchor.clone(),
                    text,
                });
            }
            Inline::OpaqueRef { attrs } => {
                fragments.push(ContentFragment::PreservedInlineRef(attrs.id.clone()));
            }
        }
    }
    Ok(ParagraphContent { fragments })
}

fn v4_marks_to_inline_mark_set(
    op_index: usize,
    marks: &[Mark],
) -> Result<InlineMarkSet, AdapterError> {
    let mut set = InlineMarkSet::default();
    for m in marks {
        match m {
            Mark::Bold => set.bold = true,
            Mark::Italic => set.italic = true,
            Mark::Underline => set.underline = true,
            Mark::Strike => set.strike = true,
            Mark::Subscript => set.subscript = true,
            Mark::Superscript => set.superscript = true,
            Mark::InlineRole { id } => {
                return Err(AdapterError::InlineRoleMarkNotSupported {
                    op_index,
                    role_id: id.clone(),
                });
            }
        }
    }
    Ok(set)
}

fn flatten_hyperlink_content_to_text(
    op_index: usize,
    content: &[Inline],
) -> Result<String, AdapterError> {
    let mut out = String::new();
    for inline in content {
        match inline {
            Inline::Text { text, marks } => {
                if !marks.is_empty() {
                    return Err(AdapterError::UnsupportedInlineInsideHyperlink {
                        op_index,
                        reason: "marks on text inside a hyperlink are not yet supported",
                    });
                }
                out.push_str(text);
            }
            Inline::Hyperlink { .. } => {
                return Err(AdapterError::UnsupportedInlineInsideHyperlink {
                    op_index,
                    reason: "nested hyperlinks are not allowed by OOXML",
                });
            }
            Inline::OpaqueRef { .. } => {
                return Err(AdapterError::UnsupportedInlineInsideHyperlink {
                    op_index,
                    reason: "opaque_ref nodes are not allowed inside a hyperlink",
                });
            }
        }
    }
    Ok(out)
}

fn convert_anchor_position(p: AnchorPosition) -> InsertPosition {
    match p {
        AnchorPosition::Before => InsertPosition::Before,
        AnchorPosition::After => InsertPosition::After,
    }
}

fn validate_inline(inline: &Inline, path: &NodePath) -> Result<(), SchemaError> {
    match inline {
        Inline::Text { .. } => Ok(()),
        Inline::Hyperlink { attrs, content } => {
            let path = path.pushed(NodePathSegment::Field("hyperlink"));
            if attrs.href.is_none() && attrs.anchor.is_none() {
                return Err(SchemaError::HyperlinkHasNoTarget { path });
            }
            let content_path = path.pushed(NodePathSegment::Field("content"));
            for (i, child) in content.iter().enumerate() {
                let child_path = content_path.pushed(NodePathSegment::Index(i));
                validate_inline(child, &child_path)?;
            }
            Ok(())
        }
        Inline::OpaqueRef { .. } => Ok(()),
    }
}

/// Walk a replace content tree and reject any payload that references the
/// same opaque id twice. This is the local structural half of invariant 2
/// (opaque set-equality). The full set-equality comparison against the
/// target paragraph is the engine's responsibility (it needs the document).
fn check_unique_opaque_ids_in_replace_content(
    op_index: usize,
    content: &ReplaceContent,
) -> Result<(), SchemaError> {
    let mut ids: Vec<String> = Vec::new();
    match content {
        ReplaceContent::Block(block) => collect_opaque_ids_in_block(block, &mut ids),
        ReplaceContent::Inline(inline) => collect_opaque_ids_in_inline(inline, &mut ids),
    }
    if let Some(dup) = find_duplicate(&ids) {
        return Err(SchemaError::DuplicateOpaqueRefInPayload {
            op_index,
            opaque_id: dup,
        });
    }
    Ok(())
}

fn collect_opaque_ids_in_block(block: &Block, out: &mut Vec<String>) {
    match block {
        Block::Paragraph { content, .. } => {
            for inline in content {
                collect_opaque_ids_in_inline(inline, out);
            }
        }
        Block::Table { content, .. } => {
            for row in content {
                for cell in &row.content {
                    for child in &cell.content {
                        collect_opaque_ids_in_block(child, out);
                    }
                }
            }
        }
        // A synthesized toc field carries no LLM-authored opaque_ref ids —
        // the engine builds its content entirely server-side.
        Block::Toc { .. } => {}
    }
}

fn collect_opaque_ids_in_inline(inline: &Inline, out: &mut Vec<String>) {
    match inline {
        Inline::Text { .. } => {}
        Inline::Hyperlink { content, .. } => {
            for child in content {
                collect_opaque_ids_in_inline(child, out);
            }
        }
        Inline::OpaqueRef { attrs } => {
            out.push(attrs.id.0.to_string());
        }
    }
}

fn find_duplicate(ids: &[String]) -> Option<String> {
    for (i, id) in ids.iter().enumerate() {
        if ids[i + 1..].iter().any(|x| x == id) {
            return Some(id.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Domain rule: a style op whose property fields are MISNAMED must be
    /// REJECTED at the wire edge, never silently applied. A caller that writes
    /// `run_format` instead of `run_props` (or `font`/`size` for
    /// `font_family`/`font_size_half_points`) has expressed an intent the engine
    /// cannot honor; under "no silent fallbacks" the op must fail loud with an
    /// actionable error, not author a fontless style and report success.
    ///
    /// Two distinct silent-drop sites are covered:
    /// 1. a misnamed TOP-LEVEL def key (`run_format`), which serde's `flatten`
    ///    machinery would otherwise swallow; and
    /// 2. a misnamed NESTED prop key (`run_props.font`), guarded by
    ///    `deny_unknown_fields` on the leaf prop struct.
    #[test]
    fn style_op_with_misnamed_prop_field_is_rejected() {
        // (1) Misnamed top-level key: `run_format` instead of `run_props`.
        let top = r#"{
            "ops": [
                { "op": "create_style", "style_id": "H1", "style_type": "para",
                  "name": "Heading 1", "run_format": { "font": "Georgia" } }
            ],
            "revision": { "author": "Styler" }
        }"#;
        let err = parse_transaction(top).expect_err("misnamed `run_format` must be rejected");
        let SchemaError::JsonParseError { message } = err else {
            panic!("expected JsonParseError, got {err:?}");
        };
        assert!(
            message.contains("run_format"),
            "error must name the bad field; got: {message}"
        );
        assert!(
            message.contains("run_props"),
            "error must suggest the expected field; got: {message}"
        );

        // (2) Misnamed nested key: `font` instead of `font_family` inside the
        // valid `run_props` object.
        let nested = r#"{
            "ops": [
                { "op": "create_style", "style_id": "H1", "style_type": "para",
                  "name": "Heading 1", "run_props": { "font": "Georgia" } }
            ],
            "revision": { "author": "Styler" }
        }"#;
        let err = parse_transaction(nested).expect_err("misnamed `font` must be rejected");
        let SchemaError::JsonParseError { message } = err else {
            panic!("expected JsonParseError, got {err:?}");
        };
        assert!(
            message.contains("font") && message.contains("font_family"),
            "error must name the bad field and the valid one; got: {message}"
        );

        // Sanity: the CORRECTLY-named payload still parses.
        let good = r#"{
            "ops": [
                { "op": "create_style", "style_id": "H1", "style_type": "para",
                  "name": "Heading 1",
                  "run_props": { "font_family": "Georgia", "font_size_half_points": 24 } }
            ],
            "revision": { "author": "Styler" }
        }"#;
        parse_transaction(good).expect("correctly-named style op still parses");
    }

    /// Domain rule: `Op` is `#[serde(tag = "op")]` (internally tagged), and
    /// serde's derive silently ignores unknown keys inside a variant body under
    /// internal tagging — a plain `#[serde(deny_unknown_fields)]` on the enum is
    /// also unusable here because two variants (`create_style`/`modify_style`)
    /// flatten a hand-Deserialize'd struct, and enum-level deny can't know that
    /// struct's fields (see `deserialize_ops_strict`). A typo'd `guard` on
    /// `replace` must still fail loud rather than silently degrade the
    /// staleness check to the advisory `expect` substring.
    #[test]
    fn op_with_misnamed_guard_field_is_rejected() {
        let json = r#"{
            "ops": [
                { "op": "replace", "target": "p_1", "guaard": "abc123",
                  "content": { "type": "text", "text": "x" } }
            ],
            "revision": { "author": "Reviewer" }
        }"#;
        let err = parse_transaction(json).expect_err("misnamed `guard` must be rejected");
        let SchemaError::JsonParseError { message } = err else {
            panic!("expected JsonParseError, got {err:?}");
        };
        assert!(
            message.contains("guaard"),
            "error must name the bad field; got: {message}"
        );
        assert!(
            message.contains("replace"),
            "error must name the op the field appeared on; got: {message}"
        );
    }

    /// Same domain rule as above, for the `semantic_hash` alias and a
    /// camelCase typo, on `delete`.
    #[test]
    fn op_with_misnamed_semantic_hash_field_is_rejected() {
        let json = r#"{
            "ops": [
                { "op": "delete", "target": "p_1", "semanticHash": "abc123" }
            ],
            "revision": { "author": "Reviewer" }
        }"#;
        let err = parse_transaction(json).expect_err("misnamed `semantic_hash` must be rejected");
        let SchemaError::JsonParseError { message } = err else {
            panic!("expected JsonParseError, got {err:?}");
        };
        assert!(
            message.contains("semanticHash"),
            "error must name the bad field; got: {message}"
        );
        assert!(
            message.contains("delete"),
            "error must name the op the field appeared on; got: {message}"
        );
    }

    /// `set_format`'s scalar font fields (`font_family`, `color`, etc.) live
    /// directly on the `Op` variant, so they reproduce the original
    /// misnamed-style-field bug exactly if `Op` isn't hardened. `fontFamily`
    /// instead of `font_family` must be rejected, not silently drop the font.
    #[test]
    fn set_format_with_misnamed_font_field_is_rejected() {
        let json = r#"{
            "ops": [
                { "op": "set_format", "target": "p_1", "expect": "hello",
                  "marks": [], "fontFamily": "Georgia" }
            ],
            "revision": { "author": "Reviewer" }
        }"#;
        let err = parse_transaction(json).expect_err("misnamed `fontFamily` must be rejected");
        let SchemaError::JsonParseError { message } = err else {
            panic!("expected JsonParseError, got {err:?}");
        };
        assert!(
            message.contains("fontFamily") && message.contains("font_family"),
            "error must name the bad field and the valid one; got: {message}"
        );
    }

    /// Nested patch structs (`set_para_format.spacing` etc.) reject a misnamed
    /// nested key via plain `#[serde(deny_unknown_fields)]` on the leaf patch
    /// struct (`SpacingPatch`) — verified independent of the outer `Op` guard.
    #[test]
    fn set_para_format_with_misnamed_nested_spacing_field_is_rejected() {
        let json = r#"{
            "ops": [
                { "op": "set_para_format", "target": "p_1",
                  "spacing": { "befor": 240 } }
            ],
            "revision": { "author": "Reviewer" }
        }"#;
        let err = parse_transaction(json).expect_err("misnamed `befor` must be rejected");
        let SchemaError::JsonParseError { message } = err else {
            panic!("expected JsonParseError, got {err:?}");
        };
        assert!(
            message.contains("befor"),
            "error must name the bad nested field; got: {message}"
        );
    }

    /// The transaction envelope (`EditTransactionV4`) and its `revision` block
    /// (`RevisionInfoV4`) are plain, non-flattened structs — a misnamed
    /// optional key must be rejected, not silently ignored.
    #[test]
    fn envelope_with_misnamed_field_is_rejected() {
        let json = r#"{
            "ops": [
                { "op": "delete", "target": "p_1" }
            ],
            "revision": { "author": "Reviewer" },
            "smmary": "typo'd summary"
        }"#;
        let err = parse_transaction(json).expect_err("misnamed `summary` must be rejected");
        let SchemaError::JsonParseError { message } = err else {
            panic!("expected JsonParseError, got {err:?}");
        };
        assert!(
            message.contains("smmary"),
            "error must name the bad field; got: {message}"
        );

        let json_revision = r#"{
            "ops": [
                { "op": "delete", "target": "p_1" }
            ],
            "revision": { "authr": "Reviewer" }
        }"#;
        let err = parse_transaction(json_revision).expect_err("misnamed `author` must be rejected");
        let SchemaError::JsonParseError { message } = err else {
            panic!("expected JsonParseError, got {err:?}");
        };
        assert!(
            message.contains("authr"),
            "error must name the bad field; got: {message}"
        );
    }

    /// Positive control: every legitimate field on a representative sample of
    /// ops (a struct-heavy one and a flatten-based one) still parses once the
    /// unknown-field guards are in place.
    #[test]
    fn op_with_every_legitimate_field_still_parses() {
        let json = r#"{
            "ops": [
                { "op": "replace", "target": "p_1", "guard": "abc123",
                  "expect": "hello", "rationale": "why",
                  "content": { "type": "paragraph",
                    "content": [{ "type": "text", "text": "x" }] } },
                { "op": "set_format", "target": "p_1", "expect": "hello",
                  "semantic_hash": "abc123", "marks": [{"type": "bold"}],
                  "color": "FF0000", "highlight": "yellow",
                  "font_family": "Georgia", "font_size_half_points": 24,
                  "caps": true, "small_caps": false, "char_spacing": 10,
                  "rationale": "why" },
                { "op": "set_para_format", "target": "p_1",
                  "spacing": { "before": 240, "after": 120 },
                  "indent": { "left": 720 } },
                { "op": "create_style", "style_id": "H1", "style_type": "para",
                  "based_on": "Normal", "name": "Heading 1",
                  "run_props": { "bold": true }, "para_props": { "alignment": "center" },
                  "rationale": "why" }
            ],
            "revision": { "author": "Reviewer", "date": "2026-07-02", "apply_op_id": "grp1" },
            "summary": "a summary",
            "materialization_mode": "direct"
        }"#;
        parse_transaction(json).expect("every legitimately-named field must still parse");
    }

    /// Domain rule (drift guard): a future `Op` variant added to the enum but
    /// NOT to `OP_FIELDS` must fail loudly (on its very first parse), not
    /// silently lose the unknown-field guard — the exact quiet-gap shape this
    /// whole hardening pass exists to eliminate. `check_op_fields` enforces
    /// this by rejecting any `op` tag it doesn't recognize, rather than
    /// deferring silently to `Op`'s own (still-correct, but bypassable-by-a-
    /// missing-table-entry) derived error.
    #[test]
    fn op_with_unrecognized_tag_is_rejected_naming_the_tag() {
        let json = r#"{
            "ops": [ { "op": "repalce", "target": "p_1" } ],
            "revision": { "author": "Reviewer" }
        }"#;
        let err = parse_transaction(json).expect_err("unrecognized op tag must be rejected");
        let SchemaError::JsonParseError { message } = err else {
            panic!("expected JsonParseError, got {err:?}");
        };
        assert!(
            message.contains("repalce"),
            "error must name the bad tag; got: {message}"
        );
        assert!(
            message.contains("replace"),
            "error must list (or otherwise suggest) a valid tag; got: {message}"
        );
    }

    #[test]
    fn paragraph_replace_with_hyperlink_and_opaque_ref_roundtrips() {
        // Worked example: a paragraph replace carrying a hyperlink and an opaque_ref.
        let json = r#"
        {
          "ops": [
            {
              "op": "replace",
              "target": "p_7",
              "content": {
                "type": "paragraph",
                "role": "body_text",
                "content": [
                  { "type": "text", "text": "\"" },
                  { "type": "text", "text": "Confidential Information",
                    "marks": [{ "type": "inline_role", "id": "defined_term" }] },
                  { "type": "text", "text": "\" means information disclosed under " },
                  { "type": "hyperlink",
                    "attrs": { "href": "https://example.com/refs" },
                    "content": [{ "type": "text", "text": "Section 5" }] },
                  { "type": "text", "text": " or via the procedures " },
                  { "type": "opaque_ref", "attrs": { "id": "op_2" } },
                  { "type": "text", "text": "." }
                ]
              }
            }
          ],
          "revision": { "author": "Counsel" }
        }
        "#;
        let txn: EditTransactionV4 = serde_json::from_str(json).expect("parses");
        assert_eq!(txn.ops.len(), 1);
        let Op::Replace {
            target, content, ..
        } = &txn.ops[0]
        else {
            panic!("expected replace");
        };
        assert_eq!(target.0.as_ref(), "p_7");
        let ReplaceContent::Block(Block::Paragraph { role, content, .. }) = content else {
            panic!("expected paragraph block content");
        };
        assert_eq!(role.as_deref(), Some("body_text"));
        assert_eq!(content.len(), 7);
        // The opaque_ref carries the existing op_2 id.
        let Inline::OpaqueRef { attrs } = &content[5] else {
            panic!("expected opaque_ref at index 5");
        };
        assert_eq!(attrs.id.0.as_ref(), "op_2");
        // The defined_term mark is parsed as an InlineRole.
        let Inline::Text { marks, .. } = &content[1] else {
            panic!("expected text at index 1");
        };
        assert_eq!(
            marks,
            &vec![Mark::InlineRole {
                id: "defined_term".into()
            }]
        );
    }

    #[test]
    fn set_attr_promotes_paragraph_role() {
        let json = r#"
        {
          "ops": [{ "op": "set_attr", "target": "p_42", "attrs": { "role": "section_heading_h2" } }],
          "revision": { "author": "Counsel" }
        }
        "#;
        let txn: EditTransactionV4 = serde_json::from_str(json).unwrap();
        let Op::SetAttr { target, attrs, .. } = &txn.ops[0] else {
            panic!("expected set_attr");
        };
        assert_eq!(target.0.as_ref(), "p_42");
        assert_eq!(attrs.role.as_deref(), Some("section_heading_h2"));
        assert!(attrs.href.is_none());
    }

    #[test]
    fn set_attr_retargets_hyperlink_href() {
        let json = r#"
        {
          "ops": [{ "op": "set_attr", "target": "hyp_3", "attrs": { "href": "https://example.com/new-target" } }],
          "revision": { "author": "Counsel" }
        }
        "#;
        let txn: EditTransactionV4 = serde_json::from_str(json).unwrap();
        let Op::SetAttr { target, attrs, .. } = &txn.ops[0] else {
            panic!("expected set_attr");
        };
        assert_eq!(target.0.as_ref(), "hyp_3");
        assert_eq!(
            attrs.href.as_deref(),
            Some("https://example.com/new-target")
        );
        assert!(attrs.role.is_none());
    }

    #[test]
    fn insert_carries_anchor_and_position() {
        let json = r#"
        {
          "ops": [{
            "op": "insert",
            "target": { "anchor": "p_7", "position": "after" },
            "content": [
              { "type": "paragraph", "content": [{ "type": "text", "text": "New clause." }] }
            ]
          }],
          "revision": { "author": "Counsel" }
        }
        "#;
        let txn: EditTransactionV4 = serde_json::from_str(json).unwrap();
        let Op::Insert {
            target, content, ..
        } = &txn.ops[0]
        else {
            panic!("expected insert");
        };
        assert_eq!(target.anchor.0.as_ref(), "p_7");
        assert_eq!(target.position, AnchorPosition::After);
        assert_eq!(content.len(), 1);
    }

    #[test]
    fn move_carries_target_and_destination() {
        let json = r#"
        {
          "ops": [{
            "op": "move",
            "target": "p_12",
            "destination": { "anchor": "p_3", "position": "before" }
          }],
          "revision": { "author": "Counsel" }
        }
        "#;
        let txn: EditTransactionV4 = serde_json::from_str(json).unwrap();
        let Op::Move {
            target,
            destination,
            ..
        } = &txn.ops[0]
        else {
            panic!("expected move");
        };
        let MoveTarget::Single(id) = target else {
            panic!("expected a single-block target, got {target:?}");
        };
        assert_eq!(id.0.as_ref(), "p_12");
        assert_eq!(destination.anchor.0.as_ref(), "p_3");
        assert_eq!(destination.position, AnchorPosition::Before);
    }

    #[test]
    fn move_range_target_carries_from_and_to() {
        let json = r#"
        {
          "ops": [{
            "op": "move",
            "target": { "from": "p_22", "to": "p_27" },
            "destination": { "anchor": "p_6", "position": "after" }
          }],
          "revision": { "author": "Counsel" }
        }
        "#;
        let txn: EditTransactionV4 = serde_json::from_str(json).unwrap();
        let Op::Move { target, .. } = &txn.ops[0] else {
            panic!("expected move");
        };
        let MoveTarget::Range { from, to } = target else {
            panic!("expected a range target, got {target:?}");
        };
        assert_eq!(from.0.as_ref(), "p_22");
        assert_eq!(to.0.as_ref(), "p_27");
    }

    #[test]
    fn delete_round_trips() {
        let json = r#"
        {
          "ops": [{ "op": "delete", "target": "p_9", "expect": "shall be void" }],
          "revision": { "author": "Counsel" }
        }
        "#;
        let txn: EditTransactionV4 = serde_json::from_str(json).unwrap();
        let Op::Delete { target, expect, .. } = &txn.ops[0] else {
            panic!("expected delete");
        };
        assert_eq!(target.0.as_ref(), "p_9");
        assert_eq!(expect.as_deref(), Some("shall be void"));
    }

    #[test]
    fn schema_check_rejects_empty_ops() {
        let json = r#"{ "ops": [], "revision": { "author": "Counsel" } }"#;
        let err = parse_transaction(json).unwrap_err();
        assert_eq!(err, SchemaError::EmptyOps);
    }

    #[test]
    fn schema_check_rejects_empty_insert_content() {
        let json = r#"
        {
          "ops": [{
            "op": "insert",
            "target": { "anchor": "p_7", "position": "after" },
            "content": []
          }],
          "revision": { "author": "Counsel" }
        }
        "#;
        let err = parse_transaction(json).unwrap_err();
        assert_eq!(err, SchemaError::EmptyInsertContent { op_index: 0 });
    }

    #[test]
    fn schema_check_rejects_hyperlink_with_no_target() {
        let json = r#"
        {
          "ops": [{
            "op": "replace",
            "target": "p_7",
            "content": {
              "type": "paragraph",
              "content": [
                { "type": "text", "text": "see " },
                { "type": "hyperlink", "attrs": {}, "content": [{ "type": "text", "text": "this" }] }
              ]
            }
          }],
          "revision": { "author": "Counsel" }
        }
        "#;
        let err = parse_transaction(json).unwrap_err();
        match err {
            SchemaError::HyperlinkHasNoTarget { path } => {
                assert_eq!(
                    path.to_string(),
                    "ops[0].content.paragraph.content[1].hyperlink"
                );
            }
            other => panic!("expected HyperlinkHasNoTarget, got {other:?}"),
        }
    }

    #[test]
    fn schema_check_rejects_empty_attr_patch() {
        let json = r#"
        {
          "ops": [{ "op": "set_attr", "target": "p_1", "attrs": {} }],
          "revision": { "author": "Counsel" }
        }
        "#;
        let err = parse_transaction(json).unwrap_err();
        assert_eq!(err, SchemaError::EmptyAttrPatch { op_index: 0 });
    }

    #[test]
    fn schema_check_reports_path_into_nested_table_cell() {
        // Hyperlink with no target buried inside a table cell. The error path
        // must descend through the table > row > cell > block > paragraph
        // structure to the offending hyperlink.
        let json = r#"
        {
          "ops": [{
            "op": "replace",
            "target": "t_2",
            "content": {
              "type": "table",
              "content": [{
                "content": [{
                  "content": [{
                    "type": "paragraph",
                    "content": [
                      { "type": "hyperlink", "attrs": {}, "content": [{ "type": "text", "text": "x" }] }
                    ]
                  }]
                }]
              }]
            }
          }],
          "revision": { "author": "Counsel" }
        }
        "#;
        let err = parse_transaction(json).unwrap_err();
        match err {
            SchemaError::HyperlinkHasNoTarget { path } => {
                assert_eq!(
                    path.to_string(),
                    "ops[0].content.table.content[0].content[0].content[0].paragraph.content[0].hyperlink"
                );
            }
            other => panic!("expected HyperlinkHasNoTarget, got {other:?}"),
        }
    }

    // ─── Adapter tests ───────────────────────────────────────────────────────

    fn parse(json: &str) -> EditTransactionV4 {
        parse_transaction(json).expect("schema check passes")
    }

    fn translate(json: &str) -> EditTransaction {
        parse(json)
            .into_edit_transaction()
            .expect("adapter succeeds")
    }

    fn translate_err(json: &str) -> AdapterError {
        parse(json)
            .into_edit_transaction()
            .expect_err("adapter rejects")
    }

    #[test]
    fn adapter_translates_paragraph_replace_with_styled_text_and_opaque_ref() {
        let json = r#"
        {
          "ops": [{
            "op": "replace",
            "target": "p_7",
            "expect": "Confidential",
            "content": {
              "type": "paragraph",
              "role": "body_text",
              "content": [
                { "type": "text", "text": "Hello, " },
                { "type": "text", "text": "world", "marks": [{ "type": "bold" }] },
                { "type": "text", "text": " — see " },
                { "type": "opaque_ref", "attrs": { "id": "op_2" } },
                { "type": "text", "text": "." }
              ]
            }
          }],
          "revision": { "author": "Counsel" }
        }
        "#;
        let txn = translate(json);
        assert_eq!(txn.steps.len(), 1);
        let EditStep::ReplaceParagraphText {
            block_id,
            replacement_role,
            expect,
            content,
            ..
        } = &txn.steps[0]
        else {
            panic!("expected ReplaceParagraphText, got {:?}", txn.steps[0]);
        };
        assert_eq!(block_id.0.as_ref(), "p_7");
        assert_eq!(replacement_role.as_deref(), Some("body_text"));
        assert_eq!(expect, "Confidential");
        assert_eq!(content.fragments.len(), 5);
        let ContentFragment::Text(t) = &content.fragments[0] else {
            panic!()
        };
        assert_eq!(t, "Hello, ");
        let ContentFragment::StyledText { text, marks } = &content.fragments[1] else {
            panic!()
        };
        assert_eq!(text, "world");
        assert!(marks.bold && !marks.italic);
        let ContentFragment::PreservedInlineRef(id) = &content.fragments[3] else {
            panic!()
        };
        assert_eq!(id.0.as_ref(), "op_2");
    }

    #[test]
    fn adapter_translates_paragraph_replace_with_new_hyperlink() {
        let json = r#"
        {
          "ops": [{
            "op": "replace",
            "target": "p_7",
            "expect": "see",
            "content": {
              "type": "paragraph",
              "role": "body_text",
              "content": [
                { "type": "text", "text": "see " },
                { "type": "hyperlink",
                  "attrs": { "href": "https://example.com/x" },
                  "content": [{ "type": "text", "text": "Section 5" }] }
              ]
            }
          }],
          "revision": { "author": "Counsel" }
        }
        "#;
        let txn = translate(json);
        let EditStep::ReplaceParagraphText { content, .. } = &txn.steps[0] else {
            panic!()
        };
        let ContentFragment::NewHyperlink { href, text, .. } = &content.fragments[1] else {
            panic!(
                "expected NewHyperlink at index 1, got {:?}",
                content.fragments[1]
            )
        };
        assert_eq!(href.as_deref(), Some("https://example.com/x"));
        assert_eq!(text, "Section 5");
    }

    #[test]
    fn adapter_translates_hyperlink_replace_to_replace_hyperlink_text() {
        let json = r#"
        {
          "ops": [{
            "op": "replace",
            "target": "hyp_3",
            "expect": "old text",
            "content": {
              "type": "hyperlink",
              "attrs": { "href": "https://example.com/x" },
              "content": [{ "type": "text", "text": "new display text" }]
            }
          }],
          "revision": { "author": "Counsel" }
        }
        "#;
        let txn = translate(json);
        let EditStep::ReplaceHyperlinkText {
            hyperlink_id,
            expect,
            new_text,
            expect_href,
            expect_anchor,
            ..
        } = &txn.steps[0]
        else {
            panic!("expected ReplaceHyperlinkText");
        };
        assert_eq!(hyperlink_id.0.as_ref(), "hyp_3");
        assert_eq!(expect, "old text");
        assert_eq!(new_text, "new display text");
        // The payload's href is forwarded as a precondition the engine
        // validates. If the caller wants to *change* the href, they must
        // use set_attr; a `replace` that supplies a different href will
        // fail at apply time with HyperlinkAttrMismatch.
        assert_eq!(expect_href.as_deref(), Some("https://example.com/x"));
        assert!(expect_anchor.is_none());
    }

    #[test]
    fn adapter_translates_insert_with_paragraph_content() {
        let json = r#"
        {
          "ops": [{
            "op": "insert",
            "target": { "anchor": "p_7", "position": "after" },
            "content": [
              { "type": "paragraph", "role": "body_text",
                "content": [{ "type": "text", "text": "A brand-new clause." }] }
            ]
          }],
          "revision": { "author": "Counsel" }
        }
        "#;
        let txn = translate(json);
        let EditStep::InsertParagraphs {
            anchor_block_id,
            position,
            blocks,
            ..
        } = &txn.steps[0]
        else {
            panic!("expected InsertParagraphs");
        };
        assert_eq!(anchor_block_id.0.as_ref(), "p_7");
        assert!(matches!(position, InsertPosition::After));
        assert_eq!(blocks.len(), 1);
        let BlockSpec::Paragraph(spec) = &blocks[0] else {
            panic!()
        };
        assert_eq!(spec.role.as_deref(), Some("body_text"));
        assert_eq!(spec.content.fragments.len(), 1);
        let ContentFragment::Text(t) = &spec.content.fragments[0] else {
            panic!()
        };
        assert_eq!(t, "A brand-new clause.");
    }

    #[test]
    fn adapter_translates_delete_to_single_block_range() {
        let json = r#"
        {
          "ops": [{ "op": "delete", "target": "p_9", "expect": "shall be void" }],
          "revision": { "author": "Counsel" }
        }
        "#;
        let txn = translate(json);
        let EditStep::DeleteBlockRange {
            from_block_id,
            to_block_id,
            expect,
            ..
        } = &txn.steps[0]
        else {
            panic!("expected DeleteBlockRange");
        };
        assert_eq!(from_block_id.0.as_ref(), "p_9");
        assert_eq!(to_block_id.0.as_ref(), "p_9");
        assert_eq!(expect, "shall be void");
    }

    #[test]
    fn adapter_translates_move_to_single_block_range() {
        let json = r#"
        {
          "ops": [{
            "op": "move",
            "target": "p_12",
            "destination": { "anchor": "p_3", "position": "before" }
          }],
          "revision": { "author": "Counsel" }
        }
        "#;
        let txn = translate(json);
        let EditStep::MoveBlockRange {
            from_block_id,
            to_block_id,
            dest_anchor_id,
            dest_position,
            ..
        } = &txn.steps[0]
        else {
            panic!("expected MoveBlockRange");
        };
        assert_eq!(from_block_id.0.as_ref(), "p_12");
        assert_eq!(to_block_id.0.as_ref(), "p_12");
        assert_eq!(dest_anchor_id.0.as_ref(), "p_3");
        assert!(matches!(dest_position, InsertPosition::Before));
    }

    #[test]
    fn adapter_translates_set_attr_role_to_set_block_range_attr() {
        let json = r#"
        {
          "ops": [{ "op": "set_attr", "target": "p_42", "attrs": { "role": "section_heading_h2" } }],
          "revision": { "author": "Counsel" }
        }
        "#;
        let txn = translate(json);
        let EditStep::SetBlockRangeAttr {
            from_block_id,
            to_block_id,
            role,
            ..
        } = &txn.steps[0]
        else {
            panic!("expected SetBlockRangeAttr");
        };
        assert_eq!(from_block_id.0.as_ref(), "p_42");
        assert_eq!(to_block_id.0.as_ref(), "p_42");
        assert_eq!(role, "section_heading_h2");
    }

    #[test]
    fn adapter_requires_expect_href_for_hyperlink_href_change() {
        // `set_attr(hyperlink, { href })` without `expect_href` is rejected:
        // the optimistic-concurrency precondition is mandatory.
        let json = r#"
        {
          "ops": [{ "op": "set_attr", "target": "hyp_3", "attrs": { "href": "https://x" } }],
          "revision": { "author": "Counsel" }
        }
        "#;
        let err = translate_err(json);
        assert_eq!(
            err,
            AdapterError::MissingHyperlinkAttrExpect {
                op_index: 0,
                attr: "href",
            },
        );
    }

    #[test]
    fn adapter_requires_expect_anchor_for_hyperlink_anchor_change() {
        let json = r#"
        {
          "ops": [{ "op": "set_attr", "target": "hyp_3", "attrs": { "anchor": "bookmark_b" } }],
          "revision": { "author": "Counsel" }
        }
        "#;
        let err = translate_err(json);
        assert_eq!(
            err,
            AdapterError::MissingHyperlinkAttrExpect {
                op_index: 0,
                attr: "anchor",
            },
        );
    }

    #[test]
    fn adapter_translates_set_attr_href_with_expect_to_set_hyperlink_attr() {
        let json = r#"
        {
          "ops": [{
            "op": "set_attr",
            "target": "hyp_3",
            "attrs": { "href": "https://example.com/new-target" },
            "expect_href": "https://example.com/old-target"
          }],
          "revision": { "author": "Counsel" }
        }
        "#;
        let txn = translate(json);
        let EditStep::SetHyperlinkAttr {
            hyperlink_id,
            new_href,
            new_anchor,
            expect_href,
            expect_anchor,
            ..
        } = &txn.steps[0]
        else {
            panic!("expected SetHyperlinkAttr");
        };
        assert_eq!(hyperlink_id.0.as_ref(), "hyp_3");
        assert_eq!(new_href.as_deref(), Some("https://example.com/new-target"));
        assert!(new_anchor.is_none());
        assert_eq!(
            expect_href.as_deref(),
            Some("https://example.com/old-target")
        );
        assert!(expect_anchor.is_none());
    }

    #[test]
    fn adapter_rejects_set_attr_with_hyperlink_title() {
        // `title` is out of scope (the IR does not carry it). Even with
        // both expects supplied, the adapter must reject.
        let json = r#"
        {
          "ops": [{
            "op": "set_attr",
            "target": "hyp_3",
            "attrs": { "title": "tooltip" }
          }],
          "revision": { "author": "Counsel" }
        }
        "#;
        let err = translate_err(json);
        assert_eq!(
            err,
            AdapterError::HyperlinkTitleNotSupported { op_index: 0 }
        );
    }

    #[test]
    fn adapter_translates_table_replace_to_replace_table_step() {
        // A valid, non-empty table payload now routes through the
        // engine's ReplaceTable step. Empty-shape rejection is covered
        // by the schema-layer test below.
        let json = r#"
        {
          "ops": [{
            "op": "replace",
            "target": "t_2",
            "content": {
              "type": "table",
              "content": [{
                "content": [{
                  "content": [{
                    "type": "paragraph",
                    "role": "table_body",
                    "content": [{ "type": "text", "text": "cell text" }]
                  }]
                }]
              }]
            }
          }],
          "revision": { "author": "Counsel" }
        }
        "#;
        let txn = translate(json);
        let EditStep::ReplaceTable {
            block_id,
            replacement,
            ..
        } = &txn.steps[0]
        else {
            panic!("expected ReplaceTable");
        };
        assert_eq!(block_id.0.as_ref(), "t_2");
        assert_eq!(replacement.rows.len(), 1);
        assert_eq!(replacement.rows[0].cells.len(), 1);
        assert_eq!(replacement.rows[0].cells[0].content.len(), 1);
    }

    #[test]
    fn schema_rejects_empty_table_rows() {
        let json = r#"
        {
          "ops": [{
            "op": "replace",
            "target": "t_2",
            "content": { "type": "table", "content": [] }
          }],
          "revision": { "author": "Counsel" }
        }
        "#;
        let err = parse_transaction(json).expect_err("empty table rows must fail");
        assert!(
            matches!(err, SchemaError::EmptyTableRows { .. }),
            "expected EmptyTableRows, got {err:?}"
        );
    }

    #[test]
    fn adapter_rejects_inline_role_mark() {
        let json = r#"
        {
          "ops": [{
            "op": "replace",
            "target": "p_7",
            "expect": "x",
            "content": {
              "type": "paragraph",
              "role": "body_text",
              "content": [{ "type": "text", "text": "x",
                            "marks": [{ "type": "inline_role", "id": "defined_term" }] }]
            }
          }],
          "revision": { "author": "Counsel" }
        }
        "#;
        let err = translate_err(json);
        assert_eq!(
            err,
            AdapterError::InlineRoleMarkNotSupported {
                op_index: 0,
                role_id: "defined_term".into()
            }
        );
    }

    #[test]
    fn adapter_rejects_hyperlink_title_attribute() {
        let json = r#"
        {
          "ops": [{
            "op": "replace",
            "target": "p_7",
            "expect": "x",
            "content": {
              "type": "paragraph",
              "role": "body_text",
              "content": [
                { "type": "hyperlink",
                  "attrs": { "href": "https://x", "title": "tooltip" },
                  "content": [{ "type": "text", "text": "go" }] }
              ]
            }
          }],
          "revision": { "author": "Counsel" }
        }
        "#;
        let err = translate_err(json);
        assert_eq!(
            err,
            AdapterError::HyperlinkTitleNotSupported { op_index: 0 }
        );
    }

    #[test]
    fn adapter_rejects_paragraph_replace_without_expect() {
        let json = r#"
        {
          "ops": [{
            "op": "replace",
            "target": "p_7",
            "content": {
              "type": "paragraph",
              "content": [{ "type": "text", "text": "x" }]
            }
          }],
          "revision": { "author": "Counsel" }
        }
        "#;
        let err = translate_err(json);
        assert_eq!(err, AdapterError::MissingExpect { op_index: 0 });
    }

    #[test]
    fn schema_check_rejects_duplicate_opaque_ref_in_replace() {
        let json = r#"
        {
          "ops": [{
            "op": "replace",
            "target": "p_7",
            "expect": "x",
            "content": {
              "type": "paragraph",
              "content": [
                { "type": "opaque_ref", "attrs": { "id": "op_2" } },
                { "type": "text", "text": " and " },
                { "type": "opaque_ref", "attrs": { "id": "op_2" } }
              ]
            }
          }],
          "revision": { "author": "Counsel" }
        }
        "#;
        let err = parse_transaction(json).unwrap_err();
        assert_eq!(
            err,
            SchemaError::DuplicateOpaqueRefInPayload {
                op_index: 0,
                opaque_id: "op_2".into()
            }
        );
    }

    #[test]
    fn schema_check_finds_duplicate_opaque_ref_across_nested_hyperlink() {
        let json = r#"
        {
          "ops": [{
            "op": "replace",
            "target": "p_7",
            "expect": "x",
            "content": {
              "type": "paragraph",
              "content": [
                { "type": "opaque_ref", "attrs": { "id": "op_2" } },
                { "type": "hyperlink", "attrs": { "href": "https://x" },
                  "content": [
                    { "type": "text", "text": "see " },
                    { "type": "opaque_ref", "attrs": { "id": "op_2" } }
                  ] }
              ]
            }
          }],
          "revision": { "author": "Counsel" }
        }
        "#;
        let err = parse_transaction(json).unwrap_err();
        assert!(matches!(
            err,
            SchemaError::DuplicateOpaqueRefInPayload { op_index: 0, .. }
        ));
    }

    #[test]
    fn schema_check_passes_on_valid_paragraph_replace() {
        let json = r#"
        {
          "ops": [{
            "op": "replace",
            "target": "p_7",
            "content": {
              "type": "paragraph",
              "content": [{ "type": "text", "text": "hello" }]
            }
          }],
          "revision": { "author": "Counsel" }
        }
        "#;
        let txn = parse_transaction(json).expect("valid transaction parses");
        assert_eq!(txn.ops.len(), 1);
    }

    #[test]
    fn nested_table_grammar_parses() {
        // Proves the recursion shape: table_cell.content is Vec<Block>, so a
        // cell can contain a nested table. Tables are not addressable day-one
        // (no LLM-visible row/cell ids), but the grammar admits them.
        let json = r#"
        {
          "ops": [{
            "op": "replace",
            "target": "t_2",
            "content": {
              "type": "table",
              "content": [
                {
                  "content": [
                    {
                      "content": [
                        { "type": "paragraph", "content": [{ "type": "text", "text": "outer cell" }] },
                        {
                          "type": "table",
                          "content": [
                            { "content": [{ "content": [{ "type": "paragraph", "content": [{ "type": "text", "text": "nested" }] }] }] }
                          ]
                        }
                      ]
                    }
                  ]
                }
              ]
            }
          }],
          "revision": { "author": "Counsel" }
        }
        "#;
        let txn: EditTransactionV4 = serde_json::from_str(json).unwrap();
        let Op::Replace {
            content: ReplaceContent::Block(Block::Table { content: rows, .. }),
            ..
        } = &txn.ops[0]
        else {
            panic!("expected table replace");
        };
        let cell = &rows[0].content[0];
        assert_eq!(cell.content.len(), 2);
        assert!(matches!(cell.content[0], Block::Paragraph { .. }));
        assert!(matches!(cell.content[1], Block::Table { .. }));
    }

    // ─── Tagged ReplaceContent: actionable decode errors ─────────────────────
    //
    // The decode dispatches on the inner `type` discriminator, not serde's
    // untagged fallback. A wrong/missing `type` names the valid kinds; a
    // structurally-wrong payload of a KNOWN kind surfaces that kind's own error.
    // The documented wire shapes still decode unchanged.

    /// Helper: decode just the `content` of a one-op replace transaction.
    fn parse_replace_content(content_json: &str) -> Result<ReplaceContent, SchemaError> {
        let json = format!(
            r#"{{"ops":[{{"op":"replace","target":"p_1","content":{content_json}}}],
                "revision":{{"author":"t"}}}}"#
        );
        parse_transaction(&json).map(|txn| match txn.ops.into_iter().next().unwrap() {
            Op::Replace { content, .. } => content,
            _ => unreachable!("replace op"),
        })
    }

    #[test]
    fn replace_content_documented_shapes_still_decode() {
        // paragraph (Block)
        assert!(matches!(
            parse_replace_content(r#"{"type":"paragraph","content":[{"type":"text","text":"x"}]}"#)
                .expect("paragraph decodes"),
            ReplaceContent::Block(Block::Paragraph { .. })
        ));
        // hyperlink (Inline)
        assert!(matches!(
            parse_replace_content(
                r#"{"type":"hyperlink","attrs":{"href":"https://x"},"content":[{"type":"text","text":"y"}]}"#
            )
            .expect("hyperlink decodes"),
            ReplaceContent::Inline(Inline::Hyperlink { .. })
        ));
    }

    #[test]
    fn replace_content_unknown_type_names_the_valid_kinds() {
        // A slightly-wrong shape now yields a
        // message that lists the valid discriminators instead of the opaque
        // "did not match any variant of untagged enum ReplaceContent".
        let err = parse_replace_content(r#"{"type":"para","content":[]}"#)
            .expect_err("unknown content type is rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("paragraph")
                && msg.contains("hyperlink")
                && msg.contains("opaque_ref")
                && msg.contains("\"para\""),
            "error must name the valid kinds and the bad value, got: {msg}"
        );
        assert!(
            !msg.contains("untagged"),
            "error must not be the serde untagged black box, got: {msg}"
        );
    }

    #[test]
    fn replace_content_missing_type_is_actionable() {
        let err = parse_replace_content(r#"{"content":[{"type":"text","text":"x"}]}"#)
            .expect_err("missing content type is rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("missing the `type` field") && msg.contains("paragraph"),
            "error must name the missing field and valid kinds, got: {msg}"
        );
    }

    #[test]
    fn replace_content_known_kind_wrong_shape_surfaces_that_kinds_error() {
        // type=paragraph but no `content` array: the error is about the
        // paragraph's own decode, NOT the untagged catch-all.
        let err = parse_replace_content(r#"{"type":"paragraph","role":"body_text"}"#)
            .expect_err("a paragraph missing content is rejected");
        let msg = err.to_string();
        assert!(
            !msg.contains("untagged"),
            "a known-kind decode error must not fall back to the untagged message, got: {msg}"
        );
        assert!(
            msg.contains("content"),
            "the error should mention the missing `content` field, got: {msg}"
        );
    }

    // ─── Block::Toc wire tests ───────────────────────────────────────────────

    fn insert_toc_txn(content_json: &str) -> Result<EditTransaction, String> {
        let json = format!(
            r#"{{
              "ops": [{{ "op": "insert",
                         "target": {{ "anchor": "p_1", "position": "after" }},
                         "content": [{content_json}] }}],
              "revision": {{ "author": "Agent" }}
            }}"#
        );
        parse_transaction(&json)
            .map_err(|e| e.to_string())?
            .into_edit_transaction()
            .map_err(|e| e.to_string())
    }

    fn toc_spec_from(txn: &EditTransaction) -> &TocBlockSpec {
        assert_eq!(txn.steps.len(), 1);
        let EditStep::InsertParagraphs { blocks, .. } = &txn.steps[0] else {
            panic!("expected InsertParagraphs, got {:?}", txn.steps[0]);
        };
        assert_eq!(blocks.len(), 1);
        let BlockSpec::Toc(spec) = &blocks[0] else {
            panic!("expected BlockSpec::Toc, got {:?}", blocks[0]);
        };
        spec
    }

    /// Domain rule: `{"type":"toc"}` with `levels` omitted resolves to the
    /// documented product defaults — Word's own "Automatic Table of Contents"
    /// range (1-3) and its three field switches (`\h \z \u`), and no explicit
    /// role (the wire never asks for one; `resolve_toc_spec` resolves the
    /// document's default body role at apply time).
    #[test]
    fn toc_block_omitted_levels_uses_default_range_and_switches() {
        let txn = insert_toc_txn(r#"{"type":"toc"}"#).expect("toc block translates");
        let spec = toc_spec_from(&txn);
        assert_eq!(spec.role, None, "toc insert never carries an explicit role");
        assert_eq!(spec.levels.from, 1);
        assert_eq!(spec.levels.to, 3);
        assert!(spec.include_hyperlinks);
        assert!(spec.hide_page_numbers_in_web);
        assert!(spec.use_outline_levels);
    }

    /// Explicit `levels` flow through unchanged.
    #[test]
    fn toc_block_explicit_levels_flow_through() {
        let txn =
            insert_toc_txn(r#"{"type":"toc","levels":{"from":2,"to":4}}"#).expect("translates");
        let spec = toc_spec_from(&txn);
        assert_eq!(spec.levels.from, 2);
        assert_eq!(spec.levels.to, 4);
    }

    /// `deny_unknown_fields`: a typo'd/extra field on a `toc` block must fail
    /// loud at the wire edge, not be silently ignored.
    #[test]
    fn toc_block_unknown_field_is_rejected() {
        let err = insert_toc_txn(r#"{"type":"toc","level":{"from":1,"to":3}}"#)
            .expect_err("misnamed `level` (not `levels`) must be rejected");
        assert!(
            err.contains("level"),
            "error must name the bad field; got: {err}"
        );
    }

    /// `1 <= from <= to <= 9` is enforced at the schema edge — refused, never
    /// clamped (CLAUDE.md "no silent fallbacks").
    #[test]
    fn toc_levels_out_of_bounds_is_rejected() {
        for bad in [
            r#"{"from":0,"to":3}"#,  // from below the 1-9 range
            r#"{"from":5,"to":2}"#,  // inverted (from > to)
            r#"{"from":1,"to":10}"#, // to above the 1-9 range
        ] {
            let content = format!(r#"{{"type":"toc","levels":{bad}}}"#);
            let err = insert_toc_txn(&content)
                .expect_err(&format!("out-of-bounds levels {bad} must be rejected"));
            assert!(
                err.contains("1 <= from <= to <= 9"),
                "error must name the constraint; got: {err}"
            );
        }
    }

    /// Day-one scope: a toc block is insert-only. `replace` with toc content
    /// is refused with an actionable error, not silently accepted or a panic.
    /// `ReplaceContent`'s hand-written `Deserialize` recognizes `"toc"` as a
    /// real (but non-replaceable) kind, so the rejection happens at the JSON
    /// edge with a message naming `insert` as the fix — before the
    /// `SchemaError::TocNotReplaceable` defense-in-depth check even runs
    /// (that check guards direct `EditTransactionV4` construction, which
    /// skips JSON parsing entirely; see `toc_not_replaceable_schema_check`).
    #[test]
    fn toc_replace_is_rejected() {
        let json = r#"{
            "ops": [
                { "op": "replace", "target": "p_1", "expect": "x",
                  "content": { "type": "toc" } }
            ],
            "revision": { "author": "Agent" }
        }"#;
        let err = parse_transaction(json).expect_err("replacing with a toc block is rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("toc") && msg.contains("insert"),
            "error must name the kind and point at insert; got: {msg}"
        );
    }

    /// Defense-in-depth: `validate_schema` itself also rejects a toc replace
    /// payload, for callers who build `EditTransactionV4` directly (bypassing
    /// JSON, and therefore `ReplaceContent`'s custom `Deserialize`).
    #[test]
    fn toc_not_replaceable_schema_check() {
        let txn = EditTransactionV4 {
            ops: vec![Op::Replace {
                target: NodeId::from("p_1"),
                content: ReplaceContent::Block(Block::Toc { levels: None }),
                span: None,
                expect: Some("x".to_string()),
                guard: None,
                semantic_hash: None,
                rationale: None,
            }],
            summary: None,
            materialization_mode: MaterializationMode::default(),
            revision: RevisionInfoV4 {
                author: "Agent".to_string(),
                date: None,
                apply_op_id: None,
            },
        };
        let err = validate_schema(&txn).expect_err("toc replace payload is rejected");
        let SchemaError::TocNotReplaceable { op_index } = err else {
            panic!("expected TocNotReplaceable, got {err:?}");
        };
        assert_eq!(op_index, 0);
    }

    /// Day-one scope: a toc block is top-level only — nested inside a table
    /// cell it is refused rather than silently accepted (the engine has no
    /// cell-level ToC support to route it to).
    #[test]
    fn toc_inside_table_cell_is_rejected() {
        let json = r#"{
            "ops": [{
                "op": "insert",
                "target": { "anchor": "p_1", "position": "after" },
                "content": [{
                    "type": "table",
                    "content": [{
                        "content": [{ "content": [{ "type": "toc" }] }]
                    }]
                }]
            }],
            "revision": { "author": "Agent" }
        }"#;
        let err = parse_transaction(json).expect_err("a toc block inside a table cell is rejected");
        assert!(
            matches!(err, SchemaError::TocNotAllowedInTableCell { .. }),
            "expected TocNotAllowedInTableCell, got {err:?}"
        );
    }

    // ── insert_image intrinsic-size default (cx/cy optional) ─────────────────

    /// A magic-valid PNG whose IHDR width/height are `w`/`h` (big-endian u32 at
    /// offsets 16/20). Only the dimensions are read.
    fn png_wh(w: u32, h: u32) -> Vec<u8> {
        let mut v = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        v.extend_from_slice(&[0, 0, 0, 13]); // IHDR length
        v.extend_from_slice(b"IHDR");
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&[8, 2, 0, 0, 0]);
        v
    }

    fn b64(bytes: &[u8]) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    /// Build an `insert_image` transaction JSON, with `cx`/`cy` lines injected
    /// only when `Some`. Returns the resolved [`ImageSource`] (the display extent
    /// after intrinsic-size resolution) or the adapter error.
    fn insert_image_source(
        bytes: &[u8],
        cx: Option<i64>,
        cy: Option<i64>,
    ) -> Result<ImageSource, AdapterError> {
        let cx_line = cx.map(|v| format!(r#""cx": {v},"#)).unwrap_or_default();
        let cy_line = cy.map(|v| format!(r#""cy": {v},"#)).unwrap_or_default();
        let json = format!(
            r#"{{
                "ops": [
                    {{ "op": "insert_image", "target": "p_1",
                       "bytes_base64": "{}", "format": "png", {cx_line} {cy_line}
                       "alt_text": "logo" }}
                ],
                "revision": {{ "author": "Imager" }}
            }}"#,
            b64(bytes)
        );
        let v4 = parse_transaction(&json).expect("schema valid");
        let txn = v4.into_edit_transaction()?;
        for step in txn.steps {
            if let EditStep::InsertImage { image, .. } = step {
                return Ok(image);
            }
        }
        panic!("no InsertImage step produced");
    }

    /// Both omitted → intrinsic pixel size at 96 DPI (1 px = 9525 EMU).
    /// 100×50 px → 952500 × 476250 EMU exactly.
    #[test]
    fn insert_image_intrinsic_dims_default_png() {
        let img = insert_image_source(&png_wh(100, 50), None, None).expect("resolves");
        assert_eq!(img.cx_emu, 952_500);
        assert_eq!(img.cy_emu, 476_250);
    }

    /// Exactly one supplied → the other follows the intrinsic aspect ratio.
    #[test]
    fn insert_image_one_sided_derivation_preserves_aspect() {
        // width fixed at the intrinsic EMU width → height derives to intrinsic.
        let by_w = insert_image_source(&png_wh(100, 50), Some(952_500), None).expect("resolves");
        assert_eq!(by_w.cx_emu, 952_500);
        assert_eq!(by_w.cy_emu, 476_250);
        // height fixed → width derives.
        let by_h = insert_image_source(&png_wh(100, 50), None, Some(476_250)).expect("resolves");
        assert_eq!(by_h.cx_emu, 952_500);
        assert_eq!(by_h.cy_emu, 476_250);
    }

    /// Both supplied → used verbatim (historical behavior; no decode needed).
    #[test]
    fn insert_image_both_supplied_used_verbatim() {
        let img = insert_image_source(&png_wh(100, 50), Some(11), Some(22)).expect("resolves");
        assert_eq!((img.cx_emu, img.cy_emu), (11, 22));
    }

    /// Magic-valid but header-undecodable bytes with cx/cy omitted → loud refusal
    /// (never a default size), and the message redirects to passing cx/cy.
    #[test]
    fn insert_image_undecodable_header_refuses_when_defaulting() {
        // PNG signature with no IHDR payload: magic passes, dimensions cannot be
        // read.
        let mut truncated = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        truncated.extend_from_slice(b"payload");
        let err = insert_image_source(&truncated, None, None).expect_err("must refuse");
        match &err {
            AdapterError::ImageDimensionsUndecodable { format, .. } => {
                assert_eq!(*format, "image/png");
            }
            other => panic!("expected ImageDimensionsUndecodable, got {other:?}"),
        }
        assert!(
            err.to_string().contains("pass cx and cy"),
            "message must redirect to explicit cx/cy, got: {err}"
        );
    }

    /// A supplied-both path never decodes the header, so even undecodable bytes
    /// are accepted at this stage (the size is explicit). Guards against
    /// over-eager decoding.
    #[test]
    fn insert_image_both_supplied_skips_decode() {
        let mut truncated = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        truncated.extend_from_slice(b"payload");
        let img = insert_image_source(&truncated, Some(5), Some(7)).expect("resolves");
        assert_eq!((img.cx_emu, img.cy_emu), (5, 7));
    }
}
