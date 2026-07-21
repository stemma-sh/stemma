//! Edit engine: step application for CanonDoc.
//!
//! This module implements the edit step application engine. It covers:
//!
//! - `EditStep` / `EditTransaction` / `EditError` types
//! - `ParagraphContent` / `ContentFragment` input representation
//! - Paragraph flattening (segment/inline structure -> diffable tokens)
//! - Phase 1: Validation (resolve, check tracking, check expect, validate anchors)
//! - Phase 2: Inline diff (UAX #29 tokenization + Myers diff)
//! - Phase 3: Segment reconstruction (diff output -> TrackedSegments)
//! - Phase 4: Normalize and write back
//! - `apply_transaction()` entry point
//!
//! Scope: step application only. No LLM generation, no ProseMirror translation,
//! no HTTP endpoints.

use crate::domain::{
    Alignment, BlockNode, BorderSet, CanonDoc, CellFormatting, CellFormattingChange, CellMargins,
    FieldData, FieldKind, FieldSemantic, FormattingChange, HeaderFooterKind, HeightRule,
    HighlightColor, IStr, Indentation, InlineChange, InlineNode, Mark, MarkValue, NodeId,
    NumberingInfo, OpaqueKind, ParagraphFormattingChange, ParagraphNode, ParagraphSpacing,
    RefFieldSpec, RevisionInfo, RowFormattingChange, RunRprAuthored, Shading, StyleProps,
    TableCellNode, TableFormatting, TableFormattingChange, TableMeasurement, TableNode,
    TableRowNode, TextNode, TocFieldSpec, TocLevelsSpec, TrackedBlock, TrackedSegment,
    TrackingStatus, VerticalAlignment, VerticalMerge, normal_segment,
};
use crate::numbering::NumberingSource;
use crate::semantic_hash::check_block_guard;
use crate::tracked_model::{
    apply_table_structure_changed, project_block_for_accept_reject,
    project_block_for_text_edit_prep,
};
use crate::vocabulary::extract_vocabulary;

// ─── Core types ──────────────────────────────────────────────────────────────

/// A single editing operation on a CanonDoc.
///
/// Steps are block-level semantic operations. The engine resolves
/// inline details (diffing, formatting, tracked segments) internally.
// A few variants carry optional table formatting (RFC-0003 Item 1); the size
// spread doesn't matter for these transient per-op values (cf. diff.rs).
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum EditStep {
    /// Replace the text content of a single paragraph.
    ///
    /// The engine diffs old content against new content at word
    /// granularity and produces minimal tracked changes.
    ///
    /// Preconditions:
    /// - The target block must exist and be a paragraph
    /// - The block must have Normal tracking status
    /// - All segments must be Normal (no existing tracked changes)
    /// - The `expect` substring must appear in a single text section
    /// - All preserved inline anchors must be referenced exactly once,
    ///   in the same order as the original
    ReplaceParagraphText {
        /// Target paragraph, by stable block ID.
        block_id: NodeId,

        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,

        /// Optional role from the schema block envelope. The MVP runtime does
        /// not yet resolve or validate replacement roles, but we preserve the
        /// caller's intent so later pipeline stages can consume it.
        replacement_role: Option<String>,

        /// Substring that must appear in the paragraph's current
        /// visible text. Matched section-locally: the substring must
        /// appear within a single text section (between preserved
        /// inline anchors), not spanning across anchor boundaries.
        expect: String,

        /// Optional full-block semantic hash precondition.
        semantic_hash: Option<String>,

        /// The complete new content for the paragraph.
        content: ParagraphContent,
    },

    /// Insert one or more new paragraphs relative to an existing block.
    InsertParagraphs {
        /// Anchor block, by stable block ID.
        anchor_block_id: NodeId,
        /// Whether to insert before or after the anchor.
        position: InsertPosition,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
        /// Block specs to insert, in order.
        blocks: Vec<BlockSpec>,
    },

    /// Mark one or more existing blocks as deleted.
    DeleteBlockRange {
        /// Inclusive range start.
        from_block_id: NodeId,
        /// Inclusive range end.
        to_block_id: NodeId,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
        /// Human-auditable precondition evaluated against the `from` block.
        expect: String,
        /// Optional full-block semantic hash precondition evaluated against the
        /// `from` block.
        semantic_hash: Option<String>,
    },

    /// Schema-shaped replace. The engine uses inline diff when possible and
    /// otherwise falls back to block delete+insert.
    ReplaceBlockRange {
        /// Inclusive range start.
        from_block_id: NodeId,
        /// Inclusive range end.
        to_block_id: NodeId,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
        /// Human-auditable precondition evaluated against the `from` block.
        expect: String,
        /// Optional full-block semantic hash precondition evaluated against the
        /// `from` block.
        semantic_hash: Option<String>,
        /// Replacement blocks.
        blocks: Vec<BlockSpec>,
    },

    /// Move a contiguous range of blocks to a new position, producing
    /// paired `w:moveFrom` / `w:moveTo` tracked changes in the DOCX
    /// output. The source blocks are marked `Deleted` with a shared
    /// `move_id`; a deep clone of each is inserted at the destination
    /// marked `Inserted` with the same `move_id`, so the serializer
    /// emits the paired `w:moveFromRange` / `w:moveToRange` markers.
    ///
    /// A `move` op. The destination anchor must not fall inside the
    /// source range.
    MoveBlockRange {
        /// Inclusive source range start.
        from_block_id: NodeId,
        /// Inclusive source range end.
        to_block_id: NodeId,
        /// Destination anchor block id. Must be outside `[from..=to]`.
        dest_anchor_id: NodeId,
        /// Whether to insert before or after the destination anchor.
        dest_position: InsertPosition,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
        /// Optional human-auditable precondition, evaluated against the
        /// `from` block — same placement and unified-guard contract as
        /// `DeleteBlockRange.expect` (authoritative when `semantic_hash` is
        /// absent; advisory-only when it is present), but optional: a
        /// single-block move has never required a guard (the target id is
        /// the whole precondition), and a range move is not forced to
        /// retrofit one.
        expect: Option<String>,
        /// Optional full-block semantic hash precondition, evaluated
        /// against the `from` block. Same optionality reasoning as `expect`.
        semantic_hash: Option<String>,
    },

    /// Change a paragraph's role without touching its text content.
    /// Re-resolves the paragraph's pPr from the new role's exemplar
    /// and records the previous pPr in a `ParagraphFormattingChange`
    /// (`w:pPrChange`, §17.13.5.29) so accept/reject produces the
    /// right state and Word renders the change as a tracked format
    /// change.
    ///
    /// A `set_attr` op. Ranges are allowed — the same new role is
    /// applied to every block in `[from..=to]`.
    SetBlockRangeAttr {
        /// Inclusive range start.
        from_block_id: NodeId,
        /// Inclusive range end.
        to_block_id: NodeId,
        /// New role id from the document vocabulary.
        role: String,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },

    /// Replace the display text of a hyperlink, leaving its URL/anchor
    /// untouched.
    ///
    /// Hyperlinks are opaque inline anchors at the paragraph layer — their
    /// display text is unreachable via `ReplaceParagraphText` (which would
    /// raise `OpaqueDestroyed` if the LLM edits the visible text). This
    /// step targets the hyperlink directly: the existing run sequence is
    /// rewritten so old text is marked `Deleted` and new text `Inserted`
    /// inside the hyperlink envelope. The URL, anchor, r_id, rPr-bearing
    /// run formatting on kept runs, and any extra attributes are preserved.
    ///
    /// Preconditions:
    /// - The hyperlink with `hyperlink_id` exists as an `OpaqueInline` of
    ///   kind `Hyperlink` somewhere in the document (top-level or in a
    ///   table cell paragraph).
    /// - The enclosing paragraph's segments are all `Normal` and the
    ///   enclosing block is editable per the same rules as
    ///   `ReplaceParagraphText`.
    /// - The hyperlink's runs are all `Normal` — the MVP rejects edits
    ///   when the hyperlink already contains tracked changes.
    /// - The concatenated current display text contains `expect` as a
    ///   substring.
    ReplaceHyperlinkText {
        /// The `NodeId` of the target `OpaqueInline` (kind = `Hyperlink`).
        hyperlink_id: NodeId,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
        /// Substring that must appear in the hyperlink's current display
        /// text. Anchors the rewrite so a stale caller does not silently
        /// edit the wrong link.
        expect: String,
        /// The complete new display text. Empty string is allowed — the
        /// hyperlink envelope is preserved but all old text is marked
        /// deleted with no inserted replacement.
        new_text: String,
        /// Optional precondition on the hyperlink's existing `href`. When
        /// supplied, the engine compares against the target hyperlink's
        /// `OpaqueKind::Hyperlink.url` and fails with
        /// `HyperlinkAttrMismatch` on a mismatch. The v4 wire format
        /// surfaces this so a caller that supplies a different href on a
        /// `replace(hyperlink)` payload fails loudly rather than getting a
        /// silent display-text-only change; attr changes belong on
        /// `set_attr`.
        expect_href: Option<String>,
        /// Optional precondition on the hyperlink's existing internal
        /// anchor. Same loud-fail semantics as `expect_href`.
        expect_anchor: Option<String>,
    },

    /// Replace a whole table block by id with a fresh target table
    /// expressed in the v4 grammar. The engine builds a target `TableNode`
    /// from the spec, diffs base against target with
    /// `table_diff::diff_canonical_tables`, then applies the diff via
    /// `tracked_model::apply_table_structure_changed` — producing row-level
    /// `w:trPr/w:ins` and `w:trPr/w:del` tracked changes, cell-level
    /// `w:cellIns` / `w:cellDel`, and inline tracked changes inside
    /// modified cells (OOXML §17.13).
    ///
    /// Preconditions:
    /// - The target block must exist and be a `BlockNode::Table`.
    /// - Merged cells (`gridSpan`/`vMerge`) and header rows are now
    ///   expressible by the `tables-merged` lift, so they round-trip
    ///   faithfully. Only non-default table/row/cell *formatting* (borders,
    ///   shading, widths) is still unrepresentable; the engine fails loudly
    ///   with `TableHasFormattingNotInSpec` rather than silently drop it.
    /// - The replacement table must be a rectangular logical grid with no
    ///   orphan `vMerge=continue`; the engine fails with `RaggedTableGrid` /
    ///   `OrphanVMergeContinue` otherwise.
    ///
    /// Unlike `ReplaceParagraphText`, there is no `expect` substring
    /// precondition. A table has no single flat text section to anchor an
    /// `expect` to; stale-snapshot detection rides on `table_id` (the
    /// target must still exist as a table) and the optional `semantic_hash`.
    ReplaceTable {
        /// Target table, by stable block ID.
        block_id: NodeId,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
        /// Optional full-block semantic hash precondition.
        semantic_hash: Option<String>,
        /// The complete new table content.
        replacement: TableBlockSpec,
    },

    /// Mutate a hyperlink's `href` (URL) and/or `anchor` (internal bookmark)
    /// in place. The change is **not** wrapped in a tracked-change envelope:
    /// OOXML defines no `w:hyperlinkChange` element and the spec provides no
    /// shape for representing a hyperlink retarget as a tracked attribute
    /// change. Option (A) — direct, non-tracked mutation — is the
    /// spec-correct choice (verified against real Word).
    /// `MaterializationMode::Direct`
    /// and `MaterializationMode::TrackedChange` therefore behave identically
    /// for this step.
    ///
    /// The hyperlink's display text, run formatting, `r_id`, and any extra
    /// attributes are preserved. The serializer re-resolves `r_id` from the
    /// new `url` via the rel resolver at export time, so the engine does
    /// not need to manage relationships.
    ///
    /// Preconditions:
    /// - The hyperlink with `hyperlink_id` exists as an `OpaqueInline` of
    ///   kind `Hyperlink` somewhere in the document.
    /// - The enclosing paragraph is editable per the same block-tracking
    ///   rules as `ReplaceHyperlinkText`. (Inside-hyperlink tracked content
    ///   is *not* a precondition here — we are mutating the hyperlink's
    ///   target, not its visible runs.)
    /// - At least one of `new_href` / `new_anchor` is set
    ///   (`HyperlinkSetAttrNoOp` otherwise).
    /// - `expect_href`, when set, equals `data.url`
    ///   (`HyperlinkAttrMismatch` otherwise).
    /// - `expect_anchor`, when set, equals `data.anchor`
    ///   (`HyperlinkAttrMismatch` otherwise).
    SetHyperlinkAttr {
        /// The `NodeId` of the target `OpaqueInline` (kind = `Hyperlink`).
        hyperlink_id: NodeId,
        /// When `Some(new)`, set `data.url = Some(new)`. When `None`, the
        /// url is not touched. `Some("")` is a structurally legal
        /// representation of "clear" but is rejected by the upstream
        /// schema (a hyperlink with empty url and empty anchor is
        /// meaningless); the engine does not re-validate this here.
        new_href: Option<String>,
        /// When `Some(value)`, set `data.anchor = value`. The outer Some
        /// means "the field is being set"; the inner option lets the
        /// caller distinguish setting a new anchor (`Some(Some("name"))`)
        /// from clearing the anchor (`Some(None)`). When `None`, the
        /// anchor is not touched.
        new_anchor: Option<Option<String>>,
        /// Optimistic-concurrency precondition on the current `data.url`.
        /// Required by the v4 adapter when `new_href` is set; the engine
        /// itself treats it as optional so callers from other surfaces can
        /// skip the check at their own risk.
        expect_href: Option<String>,
        /// Optimistic-concurrency precondition on the current `data.anchor`.
        /// Same contract as `expect_href`.
        expect_anchor: Option<String>,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },

    /// Apply run-level formatting to the text matched by `expect`, as a tracked
    /// `w:rPrChange` (§17.13.5.31) — e.g. "bold this defined term, tracked".
    ///
    /// The previous run properties are recorded so accept-all keeps the new
    /// formatting and reject-all restores the original. This is a formatting
    /// delta, not a text insert/delete; it does not go through the segment
    /// materializer.
    ///
    /// Preconditions (fail loud otherwise):
    /// - the target is an existing top-level paragraph with Normal tracking and
    ///   no existing tracked segments;
    /// - `expect` appears within a single contiguous run of text (it may not
    ///   span an opaque inline or hard break);
    /// - no run in the matched span already carries a tracked formatting change.
    SetRunFormatting {
        /// Target paragraph, by stable block ID.
        block_id: NodeId,
        /// Substring of the paragraph's visible text to format. Anchors the
        /// edit so a stale caller fails rather than formatting the wrong text.
        expect: String,
        /// Optional full-block semantic hash precondition.
        semantic_hash: Option<String>,
        /// Marks to turn on over the matched span (additive).
        marks: InlineMarkSet,
        /// Value-bearing run-style properties to set over the matched span
        /// (color, highlight, font family, font size). Separate from `marks`
        /// because these carry values that the `Copy` boolean `InlineMarkSet`
        /// cannot express.
        style: RunStyleEdit,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },
    /// Set paragraph-level formatting (alignment / indentation / spacing) on a
    /// single paragraph **in place**, as a tracked `w:pPrChange` (§17.13.5.29) —
    /// e.g. "center this clause, tracked".
    ///
    /// Unlike `SetBlockRangeAttr` (which swaps the paragraph role and clones the
    /// whole pPr from an exemplar), this sets only the attributes named in
    /// `patch` and leaves the paragraph's role / style unchanged. The previous
    /// pPr is recorded so accept-all keeps the new formatting and reject-all
    /// restores the original. Like `SetRunFormatting`, it is a formatting delta,
    /// not a text insert/delete; it does not go through the segment materializer.
    ///
    /// Preconditions (fail loud otherwise):
    /// - the target is an existing top-level paragraph with Normal tracking and
    ///   no existing tracked segments;
    /// - the paragraph does not already carry a tracked `pPrChange`;
    /// - `patch` sets at least one of alignment / indentation / spacing.
    SetParagraphFormatting {
        /// Target paragraph, by stable block ID.
        block_id: NodeId,
        /// Optional full-block semantic hash precondition. A pPr change has no
        /// single text span to anchor (like `ReplaceTable`), so stale-snapshot
        /// detection rides on this hash.
        semantic_hash: Option<String>,
        /// The paragraph attributes to set (alignment / indentation / spacing /
        /// borders / shading). At least one must be `Some` (an empty patch is
        /// refused).
        patch: ParagraphFormattingPatch,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },

    /// Set cell-level formatting (borders / shading / width / vertical alignment
    /// / margins) on ONE table cell **in place**, as a tracked `w:tcPrChange`
    /// (§17.13.5.37) — e.g. "shade this cell yellow, tracked".
    ///
    /// The cell is addressed by LOGICAL grid position `{row_index, col_index}`
    /// (after `gridBefore`, advancing by each cell's `gridSpan`) — the same
    /// address the read view mints. Like `SetParagraphFormatting`, it is a
    /// property delta, not a text insert/delete; it does not go through the
    /// segment materializer. It byte-preserves the table's `tblPr`, every
    /// `trPr`, and all other cells, touching only the target cell's requested
    /// `tcPr` properties — so it bypasses the whole-table v4-replace formatting
    /// guard (the same precedent `SetCellText`'s in-place path set).
    ///
    /// Preconditions (fail loud otherwise):
    /// - the target is an existing top-level table;
    /// - `{row_index, col_index}` resolves to a physical cell start (not the
    ///   interior of a span) that is neither a vertical-merge continuation nor
    ///   tracked-inserted/deleted;
    /// - the cell does not already carry a tracked `tcPrChange`;
    /// - `patch` sets at least one property.
    SetCellFormatting {
        /// Target table, by stable block ID.
        block_id: NodeId,
        /// Zero-based row index into the table.
        row_index: usize,
        /// Zero-based LOGICAL column index (start column of the target cell).
        col_index: usize,
        /// Optional full-block semantic hash precondition (no `expect`: a tcPr
        /// change is a property change, not a text edit).
        semantic_hash: Option<String>,
        /// The cell properties to set. At least one must be `Some`.
        patch: CellFormattingPatch,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },

    /// Insert a new REF / PAGEREF / NOREF cross-reference field as a tracked
    /// insert (`w:fldSimple`, §17.16.5.45 / .39 / .36) — e.g. "insert a
    /// cross-reference to the Definitions bookmark, tracked".
    ///
    /// The verb synthesizes a fresh `OpaqueInline{Field}` (the same lift the
    /// TOC-insert and `NewHyperlink` verbs use) and splices it into the target
    /// paragraph right after the `expect` anchor text, wrapped in an `Inserted`
    /// segment. Accept-all keeps the field; reject-all drops it.
    ///
    /// Preconditions: top-level paragraph with Normal tracking and no existing
    /// tracked segments; `bookmark` non-empty; `expect` within a single
    /// contiguous run of text. v1 authors only the self-contained `w:fldSimple`.
    InsertCrossReference {
        /// Target paragraph, by stable block ID.
        block_id: NodeId,
        /// Substring of the paragraph's visible text the new field is inserted
        /// after.
        expect: String,
        /// Optional full-block semantic hash precondition.
        semantic_hash: Option<String>,
        /// The cross-reference specification: kind (REF/PAGEREF/NOREF),
        /// bookmark, and the `\h \n \r \w \t \p` switches.
        spec: RefFieldSpec,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },
    /// Set the paragraph's auto-numbering (attach/detach a list, change the
    /// indent level, restart the counter) as a tracked `w:pPrChange` carrying
    /// the previous `w:numPr` (§17.13.5.29). The previous numbering is recorded
    /// so accept-all keeps the new numbering and reject-all restores the
    /// original. A property delta, not a text edit; it does not go through the
    /// segment materializer.
    ///
    /// Preconditions: top-level paragraph, Normal tracking, no tracked segments,
    /// no existing tracked formatting change; `SetLevel` requires existing
    /// numbering; manual-numbering (literal-prefix) paragraphs are refused; a
    /// structurally-equal no-op is refused.
    SetParagraphNumbering {
        /// Target paragraph, by stable block ID.
        block_id: NodeId,
        /// Optional full-block semantic hash precondition (no `expect`: a
        /// numbering change is a property change, not a text edit).
        semantic_hash: Option<String>,
        /// The numbering operation to author.
        change: verbs::numbering::NumberingChange,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },
    /// Insert a WordprocessingML bookmark (`w:bookmarkStart` + `w:bookmarkEnd`,
    /// §17.13.6) wrapping the `expect` span in the target paragraph. A bookmark
    /// is a zero-width structural annotation, NOT a tracked content change — the
    /// markers are emitted with Normal status and do not change visible text on
    /// any projection. The verb synthesizes the start/end pair as
    /// `Decoration{Bookmark}` nodes sharing one authored-origin placeholder id;
    /// the serializer reassigns a fresh `w:id` at write time.
    ///
    /// Preconditions: top-level paragraph, Normal tracking, no tracked segments;
    /// `name` non-empty; `name` not already used in the same paragraph; `expect`
    /// within a single contiguous Normal run of text.
    InsertBookmark {
        /// Target paragraph, by stable block ID.
        block_id: NodeId,
        /// Substring of the paragraph's visible text the bookmark wraps.
        expect: String,
        /// Optional full-block semantic hash precondition.
        semantic_hash: Option<String>,
        /// Bookmark name (`w:name`). Must be non-empty.
        name: String,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },
    /// Rename an existing bookmark in the target paragraph, rewriting ONLY the
    /// `w:name` on its `w:bookmarkStart` (the `w:id` and paired `w:bookmarkEnd`
    /// are untouched). Refuses a missing `old_name` (`BookmarkNotFound`) or a
    /// `new_name` already in use (`BookmarkDuplicateName`).
    RenameBookmark {
        /// Target paragraph, by stable block ID.
        block_id: NodeId,
        /// Current bookmark name to find.
        old_name: String,
        /// New bookmark name. Must be non-empty.
        new_name: String,
        /// Optional full-block semantic hash precondition.
        semantic_hash: Option<String>,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },
    /// Remove a bookmark from the target paragraph: drop both the named
    /// `w:bookmarkStart` and the `w:bookmarkEnd` sharing its `w:id`. Refuses a
    /// missing bookmark (`BookmarkNotFound`) or a start whose paired end is not
    /// in the paragraph (`BookmarkOrphanEnd`) — no partial removal.
    RemoveBookmark {
        /// Target paragraph, by stable block ID.
        block_id: NodeId,
        /// Bookmark name to remove.
        name: String,
        /// Optional full-block semantic hash precondition.
        semantic_hash: Option<String>,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },
    /// Apply a named paragraph style (`w:pStyle`) as a tracked `w:pPrChange`
    /// (§17.3.1.27 / §17.13.5.29) — "make this clause a Heading 2, tracked".
    ///
    /// Same lift as `SetParagraphFormatting`: the paragraph's `style_id`
    /// already serializes as `w:pStyle` at pPr position 0, the previous style
    /// is recorded in the existing `ParagraphFormattingChange.previous_style_id`,
    /// and accept/reject already resolves it. A property delta, not a text
    /// edit; it does not go through the segment materializer.
    ///
    /// Preconditions (fail loud otherwise): top-level paragraph, Normal
    /// tracking, no tracked segments, no existing `pPrChange`; the style must
    /// differ from the current one (a no-op is refused). Style **existence**
    /// is validated by the package-aware caller (the runtime has the styles
    /// part); this step cannot see the style table on `&CanonDoc`.
    ApplyStyle {
        /// Target paragraph, by stable block ID.
        block_id: NodeId,
        /// Optional full-block semantic hash precondition (no `expect`: a style
        /// change is a property change, not a text edit).
        semantic_hash: Option<String>,
        /// The style ID to apply (the `w:val` of `w:pStyle`). Must be
        /// non-empty (validated at the wire edge) and differ from the current
        /// style.
        style_id: String,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },
    /// Set display attributes on an existing opaque drawing (`w:drawing`):
    /// resize it (`wp:extent` @cx/@cy, §20.4.2.7) and/or set its alt text
    /// (`wp:docPr` @descr, §20.4.2.5) — e.g. "shrink the logo and give it alt
    /// text". A **direct, untracked** in-place mutation of the drawing's
    /// `raw_xml`, like `SetHyperlinkAttr`: OOXML has no tracked-change envelope
    /// for opaque-drawing display attributes. The binary media part is never
    /// read or touched.
    ///
    /// Preconditions (fail loud otherwise): `drawing_id` resolves to an
    /// `OpaqueInline` of kind `Drawing` in `block_id`; the drawing has
    /// `raw_xml`; the requested target element (`wp:extent` for resize,
    /// `wp:docPr` for alt text) exists; at least one of `resize` / `alt_text`
    /// is requested.
    SetImageAttributes {
        /// The paragraph hosting the drawing.
        block_id: NodeId,
        /// The drawing's stable opaque-inline id.
        drawing_id: NodeId,
        /// Optional precondition on the drawing's current `content_hash`
        /// (SHA-256 of its `raw_xml`) — stale-snapshot detection for an opaque
        /// node that has no text span to anchor on.
        semantic_hash: Option<String>,
        /// New `wp:extent` dimensions in EMUs. `None` leaves the size unchanged.
        resize: Option<verbs::images::ImageResize>,
        /// Alt-text edit, three-state to avoid a silent fallback:
        /// `None` = leave `descr` untouched; `Some(None)` = clear `descr`;
        /// `Some(Some(text))` = set `descr` to `text`.
        alt_text: Option<Option<String>>,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },
    /// Author a DELETION of an existing inline Drawing opaque — the editor-intent
    /// counterpart to `InsertImage`/`SetImageAttributes` and the sibling of
    /// `DeleteNote`. Deliberately NOT routed through the text-replace path (whose
    /// `OpaqueDestroyed` guard correctly refuses to drop opaques for the LLM):
    /// this locates the drawing by id and flips its segment's tracking status.
    ///
    /// `TrackedChange`: the single segment holding the drawing becomes
    /// `Deleted(rev)` — isolated from surrounding Normal text, exactly like
    /// deleted text (accept-all → gone, reject-all → restored byte-identical).
    /// Deleting one's OWN pending inserted drawing un-proposes it; a cross-author
    /// deletion stacks as `InsertedThenDeleted`. `Direct`: the opaque is dropped
    /// and its Normal neighbours coalesce.
    ///
    /// Guard: the DRAWING's own `content_hash` (like `SetImageAttributes`), not
    /// the containing block's text guard.
    DeleteImage {
        /// The paragraph hosting the drawing (resolved cell-aware).
        block_id: NodeId,
        /// The drawing's stable opaque-inline id.
        drawing_id: NodeId,
        /// Optional precondition on the drawing's current `content_hash`.
        semantic_hash: Option<String>,
        /// Optional audit metadata.
        rationale: Option<String>,
    },
    /// Author a new comment anchored to a text span (§17.13.4). Locates
    /// `expect` in a single contiguous Normal segment of `block_id`, splices a
    /// `commentRangeStart` before / `commentRangeEnd` + `commentReference`
    /// after the matched span as **zero-width Normal decorations** (comments are
    /// annotations, NOT tracked changes — the markers are Normal even in
    /// TrackedChange mode), allocates a fresh comment id, and pushes a
    /// `CommentStory` into `doc.comments`.
    CommentCreate {
        /// Paragraph hosting the anchored text.
        block_id: NodeId,
        /// Substring that must appear in the paragraph; the comment range wraps
        /// exactly this span. Matched in a single contiguous Normal segment.
        expect: String,
        /// Optional full-block semantic-hash precondition.
        semantic_hash: Option<String>,
        /// The comment body text (must be non-empty).
        body: String,
        /// Comment author.
        author: Option<String>,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },

    /// Reply to an existing comment (MS-DOCX §2.5.1): create a new comment
    /// story plus a `commentsExtended` record whose `w15:paraIdParent` links to
    /// the parent comment's first-body-paragraph `w14:paraId`. The reply is NOT
    /// anchored to body text — it threads under the parent.
    CommentReply {
        /// The comment id (`w:id`) of the parent comment.
        parent_comment_id: String,
        /// Reply body text (must be non-empty).
        body: String,
        /// Reply author.
        author: Option<String>,
        /// Optional audit metadata.
        rationale: Option<String>,
    },

    /// Flip the `w15:done` resolved flag on a comment's `commentsExtended`
    /// record (MS-DOCX §2.5.1). Creates the record if the comment had none.
    CommentResolve {
        /// The comment id (`w:id`) to resolve / unresolve.
        comment_id: String,
        /// Target resolved state.
        done: bool,
        /// Optional audit metadata.
        rationale: Option<String>,
    },

    /// Delete a comment entirely (§17.13.4): remove the `CommentStory` AND all
    /// three anchor markers (`commentRangeStart` / `commentRangeEnd` /
    /// `commentReference`) for that id from every story's blocks. If the markers
    /// cannot all be located, fail `CommentRangeOrphaned` with the missing list
    /// (no half-delete).
    CommentDelete {
        /// The comment id (`w:id`) to delete.
        comment_id: String,
        /// Optional audit metadata.
        rationale: Option<String>,
    },
    /// Insert a footnote or endnote (§17.11). Locates `expect` in a single
    /// contiguous Normal segment of `block_id`, splices a synthesized
    /// `w:footnoteReference` / `w:endnoteReference` reference run after the
    /// matched span (an `Inserted` segment in TrackedChange mode, `Normal` in
    /// Direct), allocates the next sequential note id across BOTH note
    /// collections, and pushes a matching `FootnoteStory` / `EndnoteStory`
    /// whose first paragraph carries the `footnoteRef`/`endnoteRef` auto-number
    /// decoration + the body text. In TrackedChange mode the story's block
    /// status is ALSO `Inserted` (its own, separately stamped, revision) — so
    /// Word shows the footnote body as inserted text and reject-all removes
    /// the whole note, not just the reference run. Renumbering is positional
    /// (Word renumbers on open); no display number is stored.
    InsertNote {
        /// Paragraph hosting the anchored reference.
        block_id: NodeId,
        /// Substring after which the reference run is inserted (matched in a
        /// single contiguous Normal segment).
        expect: String,
        /// Optional full-block semantic-hash precondition.
        semantic_hash: Option<String>,
        /// Footnote vs endnote.
        note_kind: verbs::footnotes::NoteKind,
        /// The note body text (must be non-empty).
        body: String,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },

    /// Edit an existing note's body (§17.11). `Direct` mode wholesale-replaces
    /// the note story's body blocks from the provided text, preserving the
    /// leading `footnoteRef`/`endnoteRef` decoration. `TrackedChange` mode is
    /// a SURGICAL word-diff on the story's paragraph (the same engine
    /// `ReplaceParagraphText` uses on body paragraphs) — minimal `Deleted`/
    /// `Inserted` segments around just the changed words, not a whole-
    /// paragraph rebuild; refuses (`NoteBodyMultiParagraph`) beyond a
    /// single-paragraph body, and refuses (`BlockHasTrackedStatus`) a story
    /// that already carries a pending tracked change rather than stacking.
    /// `NoteNotFound` if the id has no story; `OpaqueDestroyed` if the
    /// existing story carried an opaque inline the flat-text replace/diff
    /// cannot reproduce.
    EditNote {
        /// The note id (`w:id`) to edit.
        note_id: String,
        /// Footnote vs endnote (selects the collection to address).
        note_kind: verbs::footnotes::NoteKind,
        /// The replacement body text (must be non-empty).
        body: String,
        /// Optional audit metadata.
        rationale: Option<String>,
    },

    /// Delete a note (§17.11): remove the story AND every matching reference run
    /// from body paragraphs. `Direct` mode removes both physically.
    /// `TrackedChange` mode marks both as a tracked deletion instead (the
    /// reference run(s) via the same status-flip engine opaque deletes use;
    /// the story's block status becomes `Deleted`) — accept-all then removes
    /// note + reference, reject-all restores both; refuses
    /// (`BlockHasTrackedStatus`) a story that already carries a pending
    /// tracked change rather than stacking. `NoteNotFound` if the id has no
    /// story; `NoteReferenceMissing` if the story exists but no body reference
    /// does (no half-delete). Relies on Word's open-time positional renumber.
    DeleteNote {
        /// The note id (`w:id`) to delete.
        note_id: String,
        /// Footnote vs endnote.
        note_kind: verbs::footnotes::NoteKind,
        /// Optional audit metadata.
        rationale: Option<String>,
    },
    /// Set page-setup properties (page size, orientation, margins, columns,
    /// gutter) on the body section or a paragraph's mid-document section break
    /// (§17.6). A `w:sectPr`-child property delta — NOT a text edit, so it does
    /// NOT route through the segment materializer. In `TrackedChange` mode the
    /// prior `w:sectPr` is recorded as a `w:sectPrChange` (§17.13.5.32) so
    /// accept-all keeps the new layout and reject-all restores the original.
    ///
    /// Preconditions (fail loud): the patch is non-empty
    /// (`NoPageSetupRequested`); the target section exists
    /// (`SectionPropertiesNotFound`); the section has no existing tracked
    /// `sectPrChange` (`SectionAlreadyHasTrackedChange`). A no-op (patch equals
    /// current) is silently skipped (no empty `sectPrChange`).
    SetPageSetup {
        /// Target section (body or a paragraph's section break).
        target: verbs::page_setup::SectionTarget,
        /// The page-setup attributes to set; at least one must be `Some`.
        patch: verbs::page_setup::PageSetupPatch,
        /// Optional full-block semantic-hash precondition (reserved for future
        /// stale-snapshot detection on the section; unused in v1).
        semantic_hash: Option<String>,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },

    /// Set the section type (`w:type`, §17.6.22) on the body section or a
    /// paragraph's section break. A `Continuous` type respects the
    /// preceding-section page-property inheritance the importer established;
    /// this step changes only the discriminant, never page geometry.
    SetSectionType {
        /// Target section.
        target: verbs::page_setup::SectionTarget,
        /// The new section type.
        section_type: crate::domain::SectionType,
        /// Optional full-block semantic-hash precondition (reserved; unused v1).
        semantic_hash: Option<String>,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },

    /// Insert a mid-document section break (§17.6) by attaching a fresh
    /// `w:sectPr` to the anchor paragraph's `w:pPr`. The anchor must be a
    /// top-level paragraph that does NOT already own a section break (we refuse
    /// to clobber one). A property delta, not a text edit.
    InsertSectionBreak {
        /// The paragraph that ends the new section (carries the `w:sectPr`).
        anchor_block_id: NodeId,
        /// The section break type.
        section_type: crate::domain::SectionType,
        /// Page-setup geometry for the new section (may be empty — then the
        /// break carries default/inherited geometry).
        properties: verbs::page_setup::PageSetupPatch,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },

    /// Edit a header story paragraph's text as a TRACKED change (§17.10). Routes
    /// through the SAME word-diff + ONE materializer body `ReplaceParagraphText`
    /// uses, over the resolved header story (Invariant M). The block is
    /// story-local; opaque anchors (a `PAGE` field run, etc.) must be preserved
    /// or the edit fails `OpaqueDestroyed`.
    EditHeader {
        /// The header story (addressed by part name).
        story: StoryRef,
        /// Story-local paragraph block id.
        block_id: NodeId,
        /// Substring that must appear in the paragraph's current visible text.
        expect: String,
        /// Optional full-block semantic-hash precondition.
        semantic_hash: Option<String>,
        /// The complete new content for the paragraph.
        content: ParagraphContent,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },

    /// Edit a footer story paragraph's text as a TRACKED change (§17.10). Same
    /// lift as `EditHeader` over a footer story.
    EditFooter {
        /// The footer story (addressed by part name).
        story: StoryRef,
        /// Story-local paragraph block id.
        block_id: NodeId,
        /// Substring that must appear in the paragraph's current visible text.
        expect: String,
        /// Optional full-block semantic-hash precondition.
        semantic_hash: Option<String>,
        /// The complete new content for the paragraph.
        content: ParagraphContent,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },

    /// Author a NET-NEW, blank header story (§17.10.2) and reference it from the
    /// body section. Pushes a blank `HeaderStory` into `doc.headers`, adds a
    /// `headerReference` of `kind` to the body section, and FORCES the modeled
    /// sectPr path by recording a tracked `w:sectPrChange` (§17.13.5.32): accept
    /// keeps the new running head, reject restores the original section (no
    /// header) and prunes the blank orphan story. Refuses to duplicate an
    /// existing header of the same kind (`HeaderFooterAlreadyExists` — edit it
    /// with `EditHeader` instead) and refuses to stack a second tracked sectPr
    /// change (`SectionAlreadyHasTrackedChange`). The save path synthesizes the
    /// OPC part/content-type/rel for the new story (no PendingParts needed).
    CreateHeader {
        /// The header kind (`Default` / `First` / `Even`) — selects the
        /// reference `w:type` and the story's kind.
        kind: HeaderFooterKind,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },

    /// Author a NET-NEW, blank footer story (§17.10.2). The footer twin of
    /// `CreateHeader` — same coordinated story + reference + tracked sectPrChange
    /// mutation, over `doc.footers` / `footerReference`.
    CreateFooter {
        /// The footer kind (`Default` / `First` / `Even`).
        kind: HeaderFooterKind,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },

    /// Toggle header/footer display mode (§17.6.18 `w:titlePg`, §17.15.1.35
    /// `w:evenAndOddHeaders`) and link/unlink a section's header/footer
    /// references by kind. v1 LINKs an EXISTING header/footer story; net-new
    /// story creation fails `HeaderFooterRefNotResolvable`. A fully-empty request
    /// is refused (`NoHeaderFooterModeRequested`).
    SetHeaderFooterMode {
        /// `Some(true/false)` sets `w:titlePg`; `None` leaves it untouched.
        title_page: Option<bool>,
        /// `Some(true/false)` sets the document-level `w:evenAndOddHeaders`
        /// (three-state honest: `Some(false)` = explicit off, distinct from
        /// absent); `None` leaves it untouched.
        even_and_odd: Option<bool>,
        /// Optional link/unlink of a header/footer reference by kind.
        link: Option<verbs::headers_footers::HeaderFooterLink>,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },
    /// Insert an Office MathML (OMML) equation as a tracked insert. Inline math
    /// (`m:oMath`, §22.1.2.77) splices into the run flow after `expect`; block
    /// math (`m:oMathPara`, §22.1.2.78) lands as a paragraph-direct opaque that
    /// the serializer already wraps in a tracked container. The `omml` fragment
    /// is validated at the edge (`EquationXmlInvalid` on parse failure,
    /// `EquationNotMath` when the root local name does not match the placement).
    /// The fragment IS the source of truth (`raw_xml: Some`); accept-all keeps
    /// the equation, reject-all restores the baseline.
    InsertEquation {
        /// Top-level paragraph to host the equation.
        block_id: NodeId,
        /// Substring that must appear in the paragraph's visible text; the
        /// equation is spliced in right after it.
        expect: String,
        /// Optional full-block semantic-hash precondition.
        semantic_hash: Option<String>,
        /// Caller-supplied `m:oMath` (Inline) / `m:oMathPara` (Block) fragment.
        omml: Vec<u8>,
        /// Inline vs block placement (must match the fragment's root element).
        placement: verbs::equations::EquationPlacement,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },

    /// Convert a contiguous run of paragraphs (e.g. a bullet list) into a TABLE
    /// as a single composed tracked change. The new table is staged as a
    /// tracked **insert** (placed immediately before the first source paragraph)
    /// and the source paragraph range is marked as a tracked **delete**, so the
    /// two projections are clean:
    /// - **accept-all** => the table only (the source paragraphs are gone);
    /// - **reject-all** => the original paragraphs only (the table is gone).
    ///
    /// This rides the ONE materializer (Invariant M): it builds a
    /// [`TableBlockSpec`] and reuses the exact tracked-insert path
    /// `InsertParagraphs` uses (inserted rows get `<w:trPr><w:ins/></w:trPr>`,
    /// inserted cell runs get `<w:ins>`) and the tracked-delete path
    /// `DeleteBlockRange` uses (run `<w:del>`/`<w:delText>` + paragraph-mark
    /// deletion). No new tracked primitive is introduced.
    ///
    /// Each source paragraph becomes one body row; the paragraph's visible text
    /// is split by `delimiter` into cells. An optional `header` adds a leading
    /// (tracked-inserted) header row AND fixes the column count at `header.len()`:
    /// each body row is split into at most that many cells (extra delimiters fold
    /// into the last cell) and short rows are padded with empty trailing cells —
    /// lossless, no text dropped. Without a header the first row's split count
    /// fixes the grid and every later row must match it, else the verb refuses
    /// with `BlocksToTableSplitMismatch` rather than emitting a ragged table.
    ///
    /// Preconditions (fail-fast — see CLAUDE.md "no silent fallbacks"):
    /// - every block in `[from..=to]` is a top-level paragraph with Normal
    ///   tracking (else `BlocksToTableNonParagraph` /
    ///   `ParagraphContainsTrackedSegments` / `BlockHasTrackedStatus`);
    /// - no source paragraph carries an opaque inline (drawing/field/hyperlink/
    ///   footnote/comment ref) — those would be lost on accept-all, so the verb
    ///   refuses with `BlocksToTableOpaqueInline` rather than destroying them.
    BlocksToTable {
        /// Inclusive source range start (first paragraph to convert).
        from_block_id: NodeId,
        /// Inclusive source range end (last paragraph to convert).
        to_block_id: NodeId,
        /// Delimiter used to split each source paragraph's visible text into
        /// cells. Must be non-empty.
        delimiter: String,
        /// Optional header row cell texts. When present, its length defines the
        /// column count; when absent, the column count is the cell count of the
        /// first source paragraph's split.
        header: Option<Vec<String>>,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },

    /// Wrap a run-span (anchored by `expect` inside `block_id`) in a structured
    /// document tag / content control (`w:sdt`, §17.5.2). Synthesizes a NEW
    /// inline `OpaqueInline{Sdt}` whose `raw_xml` is a deterministically-built
    /// `w:sdt` (sdtPr from the `spec`, sdtContent wrapping the matched run).
    /// SDT structure is NOT tracked (OOXML has no `w:sdtChange` envelope), so
    /// this is Direct/structural like `SetImageAttributes` — accept-all ==
    /// reject-all == the wrapped doc; reversibility is at the
    /// transaction-rejection level. Fails loud: empty distinguishing spec ⇒
    /// `EmptyContentControlSpec`; block-level wrapping ⇒
    /// `ContentControlBlockUnsupported` (deferred — see `verbs::content_controls`).
    WrapInContentControl {
        /// Paragraph hosting the run-span to wrap.
        block_id: NodeId,
        /// Substring identifying the run-span to wrap.
        expect: String,
        /// Optional full-block semantic-hash precondition.
        semantic_hash: Option<String>,
        /// The content-control specification (tag/alias/control type).
        spec: verbs::content_controls::SdtSpec,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },

    /// Set the displayed value of an existing content control (`w:sdt`,
    /// §17.5.2): the `sdtContent` text and/or the checkbox/selection state.
    /// Locates the `OpaqueInline{Sdt}` by id, mutates its `raw_xml` in place
    /// (`SetImageAttributes` pattern), recomputes `content_hash`. NOT tracked
    /// (no `w:sdtChange`). Fails loud: id not an SDT ⇒ `NotAContentControl`;
    /// value kind incompatible with the control type ⇒
    /// `ContentControlTypeMismatch`.
    SetContentControlValue {
        /// Paragraph hosting the content control.
        block_id: NodeId,
        /// The content control's stable opaque-inline id.
        sdt_id: NodeId,
        /// The new value to display (text / checked / selected).
        value: verbs::content_controls::SdtValue,
        /// When `true`, the value change is recorded as tracked `w:ins`/`w:del`
        /// inside `sdtContent`. This requires the accept/reject projector to
        /// descend into SDT `raw_xml` (the B1 feature), which is not implemented
        /// yet — so `tracked: true` is REFUSED with
        /// `TrackedContentControlSetUnsupported` rather than silently downgraded
        /// to an untracked set. `false` is the current in-place/untracked path.
        tracked: bool,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },

    /// Fill a legacy form field (FORMTEXT / FORMCHECKBOX / FORMDROPDOWN — the
    /// `w:fldChar` + `w:ffData` complex-field carrier, §17.16). Locates the field
    /// by its BEGIN anchor's opaque-inline id, mutates the `ffData` state inside
    /// that anchor's `raw_xml` AND the cached result run(s) between `separate` and
    /// `end`, recomputes `content_hash`. UNTRACKED / in-place: Word fills a form
    /// field as a field-result update, not a tracked edit (§17.16.18). Fails loud
    /// per the `FormField*` / `MalformedFfData` error table; does NOT consult
    /// `w:enabled` / document protection (documented decision — stemma is a
    /// programmatic editor, not Word's interactive UI).
    SetFormFieldValue {
        /// Paragraph hosting the form field.
        block_id: NodeId,
        /// The opaque-inline id of the field's BEGIN anchor (the `fldChar`
        /// carrying `ffData`).
        field_id: NodeId,
        /// The new value (text / checked / selected).
        value: verbs::form_fields::FormFieldValue,
        /// Optional precondition on the begin anchor's `content_hash`.
        semantic_hash: Option<String>,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },
    /// Insert a new inline image into a paragraph (§20.4 DrawingML). Synthesizes
    /// a `w:drawing` whose `a:blip r:embed` references a *logical rId* and stages
    /// the binary as a [`pending_parts::PendingMedia`]; the save path
    /// (`runtime::apply_pending_media`) writes the `word/media/*` part, registers
    /// the image relationship, and rewrites the logical rId to the real one.
    ///
    /// Tracked: the drawing rides in its own `Inserted` segment, so accept-all
    /// keeps the (now-registered) image and reject-all drops it. The image's
    /// format is validated against its magic bytes at the verb edge
    /// (`UnsupportedImageFormat` / `ImageBytesEmpty`).
    InsertImage {
        /// Paragraph to host the image.
        block_id: NodeId,
        /// Optional anchor: the image is appended after the segment containing
        /// this text. `None` appends at the end of the paragraph. A supplied
        /// anchor that is absent fails `ExpectMismatch` (no silent fallback).
        expect: Option<String>,
        /// Optional full-block semantic-hash precondition.
        semantic_hash: Option<String>,
        /// The validated image (bytes + format + extent + alt text).
        image: verbs::image_insert::ImageSource,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },

    /// Replace the binary media of an existing drawing (§20.4). Locates the
    /// drawing by id, rewrites its `a:blip r:embed` to a fresh logical rId, and
    /// stages the new binary as a [`pending_parts::PendingMedia`]. Direct /
    /// untracked, like `SetImageAttributes`: OOXML has no tracked-change envelope
    /// for swapping a drawing's media. The old media part is left unreferenced
    /// (harmless). Reversibility is at the transaction-rejection level.
    ReplaceImage {
        /// Paragraph hosting the drawing.
        block_id: NodeId,
        /// The drawing's stable opaque-inline id.
        drawing_id: NodeId,
        /// Optional precondition on the drawing's current `content_hash`.
        semantic_hash: Option<String>,
        /// The validated replacement image (carries the required display extent
        /// `cx`/`cy`, which is now applied to `wp:extent`).
        image: verbs::image_insert::ImageSource,
        /// Override the aspect-ratio guard: when `false` (default), a replacement
        /// whose intrinsic aspect ratio disagrees with the requested extent is
        /// refused (`ImageAspectMismatch`) rather than stretched; `true` permits
        /// a deliberate stretch.
        allow_stretch: bool,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },

    /// Replace the WHOLE interior of a textbox (`w:txbxContent`, §17.3.4.4) with
    /// caller-supplied paragraphs. Locates the drawing by id, mutates its
    /// `raw_xml` in place (`SetImageAttributes` pattern). Carrier-agnostic
    /// (DrawingML `wps:txbx` + VML `v:textbox`, located by local name).
    /// UNTRACKED / direct: there is no tracked-change envelope for rewriting a
    /// textbox's contents. Refuses loudly if the drawing has no `w:txbxContent`
    /// (`ImageAttributeTargetAbsent`) or the interior already carries tracked
    /// changes (`TextboxHasTrackedChanges` — resolve them first, don't flatten).
    SetTextboxText {
        /// Paragraph hosting the drawing.
        block_id: NodeId,
        /// The drawing's stable opaque-inline id.
        drawing_id: NodeId,
        /// The new interior, one string per paragraph. Empty ⇒ one empty
        /// paragraph (CT_TxbxContent requires a block-level child).
        paragraphs: Vec<String>,
        /// Optional precondition on the drawing's current `content_hash`.
        semantic_hash: Option<String>,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },

    /// Author a brand-new style into `word/styles.xml` (§17.7.4). Builds the
    /// `w:style` fragment deterministically from the [`verbs::style_defs::StyleDefinition`]
    /// and stages a [`pending_parts::StyleOp::Create`]; the save path splices it
    /// AFTER the base/target style merge, so an authored style wins a style-id
    /// collision. Does NOT mutate the body IR. Untracked (package-level):
    /// reversibility is at the transaction-rejection level. The runtime fails
    /// loud if the styleId already exists.
    CreateStyle {
        /// The full style definition to author.
        def: verbs::style_defs::StyleDefinition,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },

    /// Modify an existing style in `word/styles.xml` (§17.7.4). Rebuilds the
    /// `w:style` fragment from the [`verbs::style_defs::StyleDefinition`] and
    /// stages a [`pending_parts::StyleOp::Modify`] (MERGE-by-styleId: authored
    /// fields replace their counterparts on the existing style; everything the
    /// definition does not author is preserved — omitting a field never
    /// removes it). Does NOT
    /// mutate the body IR. Untracked. The runtime fails loud if the styleId is
    /// absent.
    ModifyStyle {
        /// The styleId to replace (must match `def.style_id`).
        style_id: String,
        /// The replacement style definition.
        def: verbs::style_defs::StyleDefinition,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },

    /// Set the document DEFAULT run properties
    /// (`w:docDefaults/w:rPrDefault/w:rPr`, §17.7.5.3). The one-edit body-text
    /// re-skin: text that inherits its font/size from the document defaults
    /// (the common case for unstyled body text) picks up the new values without
    /// touching any individual `w:style`. Stages a
    /// [`pending_parts::StyleOp::SetDocDefaults`]; the save path property-merges
    /// it into the docDefaults block (find-or-create, preserving every other
    /// rPrDefault child). Does NOT mutate the body IR. Untracked (package-level),
    /// like CreateStyle/ModifyStyle — OOXML has no change envelope for
    /// docDefaults. At least one of font_family / font_size must be present.
    SetDocDefaults {
        /// Literal font family for `w:rFonts` @ascii/@hAnsi/@cs.
        font_family: Option<String>,
        /// Font size in half-points for `w:sz`/`w:szCs` @val (24 = 12pt).
        font_size_half_points: Option<u32>,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },
    /// Replace the text of a **sub-range** of a paragraph (Phase 3 fine-grained
    /// addressing) via the **status-preserving splice**: an inline text edit
    /// layers a new revision beside existing tracked segments without
    /// disturbing them. The range is named by a resolved span selector — a
    /// span handle (`s_<n>` from a fresh detail read) or an anchor-relative
    /// range — never by substring.
    ///
    /// The splice splits the boundary segments at the range edges,
    /// materializes the change as new `Inserted`/`Deleted` segments inside
    /// the range, and carries every out-of-range segment through untouched —
    /// so a neighbouring tracked change survives structurally ("layer
    /// beside"). The range contract is enforced fail-loud: the targeted
    /// range must be all-`Normal`, must not split a bracket pair (bookmark /
    /// comment-range markers), and its wall inventory (opaques, hard breaks)
    /// must be carried by the replacement content.
    ReplaceSpanText {
        /// Target paragraph, by stable block id.
        block_id: NodeId,
        /// Block staleness guard (semantic hash). REQUIRED: it is both the
        /// freshness gate that makes the ephemeral span handle safe
        /// (optimistic concurrency) and the mechanism that refuses compound
        /// same-paragraph edits in one transaction — op 1 moves the
        /// hash, op 2's stale guard refuses. Distinct from the range-status
        /// predicate, which is a content check, not a freshness check.
        guard: String,
        /// Optional text-identity precondition: the resolved range's
        /// visible text must equal this exactly. Closes the cross-version
        /// hole the guard alone does not cover — the guard is deliberately
        /// segmentation-insensitive while a handle is an ordinal over the
        /// segmentation.
        expect: Option<String>,
        /// The resolved span selector (handle / anchor-relative range).
        span: ResolvedSpanSelector,
        /// The new text for the targeted span.
        content: ParagraphContent,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },
    /// Granular structural edit on an EXISTING table (insert/delete row or
    /// column, merge a rectangular cell region, set one cell's text). The op
    /// builds a modified target `TableNode` from the located base and lowers it
    /// through the SAME table-diff machinery `ReplaceTable` uses — producing
    /// row/cell-level tracked changes. See `verbs::table_ops`. Does NOT touch
    /// the materializer (Invariant M).
    TableStructureOp {
        /// Target table, by stable block id.
        block_id: NodeId,
        /// Optional full-block semantic-hash precondition (stale-snapshot guard).
        semantic_hash: Option<String>,
        /// The granular structural op.
        op: verbs::table_ops::TableOp,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },
    /// Set row-level formatting (height + height rule) on ONE table row **in
    /// place**, as a tracked `w:trPrChange` (§17.13.5.36). The row is addressed
    /// by `row_index` — the same address the read view mints. Like
    /// `SetCellFormatting`, this is an in-place property edit: it byte-preserves
    /// `tblPr`, every OTHER row, and every cell of the target row, so it bypasses
    /// the whole-table v4 replace guard (the same precedent `SetCellText`'s
    /// in-place path set).
    ///
    /// Preconditions (fail loud otherwise):
    /// - the target is an existing top-level table;
    /// - `row_index` is within range;
    /// - the row carries no tracked structural insert/delete;
    /// - the row does not already carry a tracked `trPrChange`;
    /// - `patch` sets at least one property.
    SetRowFormatting {
        /// Target table, by stable block ID.
        block_id: NodeId,
        /// Zero-based row index into the table.
        row_index: usize,
        /// Optional full-block semantic hash precondition (no `expect`: a trPr
        /// change is a property change, not a text edit).
        semantic_hash: Option<String>,
        /// The row properties to set. At least one must be `Some`.
        patch: RowFormattingPatch,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },
    /// Set TABLE-level formatting (borders / width / default cell margins) on a
    /// table **in place**, as a tracked `w:tblPrChange` (§17.13.5.34) — e.g.
    /// "box this table and widen it to full-width, tracked".
    ///
    /// The table is addressed by stable `block_id`. Like `SetCellFormatting`, it
    /// is a property delta on `tblPr`, not a structural row/cell edit: it does
    /// NOT go through the segment materializer. It byte-preserves every `w:tr`,
    /// every `w:tc`, and the table's untouched `tblPr` properties (style,
    /// alignment, grid, banding, …), touching only the requested `tblPr` fields —
    /// so it bypasses the whole-table v4-replace formatting guard (the same
    /// precedent `SetCellFormatting`'s in-place path set; nothing is dropped).
    ///
    /// Preconditions (fail loud otherwise):
    /// - the target is an existing top-level table;
    /// - the table does not already carry a tracked `tblPrChange`;
    /// - `patch` sets at least one property.
    SetTableFormatting {
        /// Target table, by stable block ID.
        block_id: NodeId,
        /// Optional full-block semantic hash precondition (no `expect`: a tblPr
        /// change is a property change, not a text edit).
        semantic_hash: Option<String>,
        /// The table properties to set. At least one must be `Some`.
        patch: TableFormattingPatch,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },
    /// Author the *layout* display attributes on an existing opaque drawing:
    /// its crop (`a:srcRect`), floating position (`wp:positionH`/`wp:positionV`),
    /// and text-wrap type (`wp:wrap*`). Sibling to `SetImageAttributes`
    /// (resize + alt-text) and likewise a **direct, untracked** in-place mutation
    /// of the drawing's `raw_xml` — OOXML has no tracked-change envelope for
    /// opaque-drawing display attributes, so accept/reject are no-ops on it.
    ///
    /// Crop is reachable on any drawing; position and wrap are anchor-only and
    /// fail loud (`ImageLayoutRequiresAnchor`) on an inline drawing. See
    /// `verbs::image_layout`.
    SetImageLayout {
        /// The paragraph hosting the drawing.
        block_id: NodeId,
        /// The drawing's stable opaque-inline id.
        drawing_id: NodeId,
        /// Optional precondition on the drawing's current `content_hash`.
        semantic_hash: Option<String>,
        /// The layout properties to set (crop / position / wrap). At least one
        /// must be present (the wire edge and `apply` both refuse an empty patch).
        patch: verbs::image_layout::ImageLayoutPatch,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },
    /// Wrap a contiguous, top-level RANGE of body blocks (paragraphs / tables)
    /// in a single block-level structured document tag / content control
    /// (`w:sdt`, §17.5.2) — the block-level sibling of `WrapInContentControl`.
    /// Records a `BlockSdtWrap` on the first block of the range; serialization
    /// emits `<w:sdt><w:sdtPr>…</w:sdtPr><w:sdtContent>` around the range. The
    /// wrapped blocks' content and opaques are preserved exactly. SDT structure
    /// is NOT tracked (OOXML has no `w:sdtChange`), so this is Direct/structural
    /// like `WrapInContentControl` — accept-all == reject-all == the wrapped
    /// doc; reversibility is at the transaction-rejection level. Fails loud:
    /// empty distinguishing spec ⇒ `EmptyContentControlSpec`; a non-forward /
    /// non-existent range ⇒ `BlockRangeInvalid`; a block already wrapped ⇒
    /// `BlockAlreadyWrapped`. See `verbs::block_content_controls`.
    WrapBlocksInContentControl {
        /// First block of the range (inclusive), by stable block id.
        start_block_id: NodeId,
        /// Last block of the range (inclusive), by stable block id.
        end_block_id: NodeId,
        /// The content-control specification (tag/alias/control type).
        spec: verbs::content_controls::SdtSpec,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },
    /// Surgical text replacement INSIDE an opaque region — the FIRST
    /// document-order occurrence of `find` → `replacement` inside one addressed
    /// textbox paragraph or inline content-control text region, as real
    /// `w:ins`/`w:del` tracked markup (or a direct replace). The rest of the
    /// opaque fragment is preserved Word-identically (structural
    /// re-serialization; not a byte-for-byte guarantee on sibling runs — see
    /// `opaque_splice`). The addressed textbox's text-identical
    /// AlternateContent copies are mirrored (all or refuse).
    /// Fails loud: absent/ineligible opaque ⇒ `OpaqueTextTargetNotFound`; bad
    /// address ⇒ `OpaqueTextRegionNotFound`; `find` missing / region already
    /// tracked / span crosses a barrier ⇒ the `OpaqueText*` splice errors. See
    /// `verbs::opaque_text_edit` and the shared `opaque_splice` core.
    OpaqueTextEdit {
        /// The paragraph hosting the opaque inline.
        block_id: NodeId,
        /// The opaque inline's stable id (a textbox drawing or inline `w:sdt`).
        opaque_id: NodeId,
        /// Which distinct interior container (textbox: the Nth deduped
        /// `txbxContent`; inline SDT: 0). From `opaque_text_targets`.
        container_index: usize,
        /// Which text-bearing paragraph within that container (inline SDT: 0).
        paragraph_index: usize,
        /// The exact current text to replace (first occurrence).
        find: String,
        /// Its replacement (empty ⇒ pure deletion).
        replacement: String,
        /// Optional precondition on the opaque's current `content_hash`.
        semantic_hash: Option<String>,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },
    /// Set a content control's text VALUE (whole-value replace), tracked. The
    /// forms-natural "fill this field" op — inline controls splice their raw_xml
    /// in place; body-level (block) controls are validated here and STAGED for the
    /// save path (their bytes live in the serialize scaffold). Exactly one target:
    /// `(block_id, sdt_id)` for an inline control, or `body_index` for a block
    /// control. Fails loud: neither/both ⇒ `SdtFillAmbiguousTarget`; missing ⇒
    /// `OpaqueTextTargetNotFound`/`SdtFillBlockNotFound`; empty fill of an empty
    /// control ⇒ `SdtFillEmpty`. See `verbs::sdt_text_fill`.
    SdtTextFill {
        /// Host paragraph of an INLINE content control (with `sdt_id`).
        block_id: Option<NodeId>,
        /// The inline content control's opaque id.
        sdt_id: Option<NodeId>,
        /// The body index of a BLOCK-level content control (from discovery).
        body_index: Option<usize>,
        /// The value to set.
        value: String,
        /// Optional precondition on an inline control's current `content_hash`.
        semantic_hash: Option<String>,
        /// Optional audit metadata from the schema envelope.
        rationale: Option<String>,
    },
    // ─── add new authoring verbs above ───────────────────────────────────────
    // Keep each verb's validate/apply logic in `edit/verbs/<verb>.rs` and make
    // the `apply_transaction` arm a one-line delegate. See `edit/AGENTS.md`.
}

/// A resolved sub-block span selector (the translated, engine-internal form of
/// the wire `edit_v4::SpanSelector`). Anchors are durable opaque node ids; a
/// handle is the ephemeral `s_<n>` ordinal keyed on `view::enumerate_text_spans`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolvedSpanSelector {
    /// The whole paragraph (back-compat with `ReplaceParagraphText`).
    Whole,
    /// A span handle `s_<n>` from a fresh detail read.
    Handle(String),
    /// An empty insertion range immediately after the given opaque anchor.
    AnchorAfter(NodeId),
    /// An empty insertion range immediately before the given opaque anchor.
    AnchorBefore(NodeId),
    /// A range delimited by two endpoints.
    Between {
        start: ResolvedSpanEndpoint,
        end: ResolvedSpanEndpoint,
    },
}

/// One endpoint of a [`ResolvedSpanSelector::Between`] range.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolvedSpanEndpoint {
    /// The start of the paragraph's inline content.
    Start,
    /// The end of the paragraph's inline content.
    End,
    /// The position adjacent to the given opaque anchor.
    Anchor(NodeId),
    /// A resolved boundary index into `flat_inlines(para)` — the half-open
    /// range edge, so the legal domain is `0..=flat_inlines(para).len()` (the
    /// length itself is the end-of-paragraph boundary). NOT a character offset:
    /// the unit is one whole inline (a text run, an opaque anchor, a hard
    /// break), counted in document order across all segments by the SAME
    /// flattening the resolver indexes.
    ///
    /// INTERNAL-ONLY: there is no wire `SpanSelector` shape that maps here — it
    /// exists so a server-side planner (e.g. `replace_text`) can target a Normal
    /// text region whose edge is a tracked-segment boundary (which has no opaque
    /// anchor to name). Because nothing outside the engine mints it, an
    /// out-of-domain index is a planner bug: `resolve_span` `debug_assert!`s the
    /// bound (caught at the planner's own test) and still refuses the apply in
    /// production. The all-Normal range predicate (`validate_range_status`)
    /// separately rejects a range that points into a tracked segment, so a bad
    /// boundary never splices across a tracked-change boundary.
    FlatIndex(usize),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterializationMode {
    #[default]
    TrackedChange,
    Direct,
}

/// Relative insertion position for block inserts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsertPosition {
    Before,
    After,
}

/// A schema block for insert/replace operations.
#[allow(clippy::large_enum_variant)] // Table variant carries optional formatting (RFC-0003 Item 1).
#[derive(Clone, Debug)]
pub enum BlockSpec {
    Paragraph(ParagraphBlockSpec),
    Toc(TocBlockSpec),
    Table(TableBlockSpec),
}

/// A schema table block for insert/replace operations.
///
/// Mirrors the v4 `Block::Table` grammar: a grid of rows and cells, each cell
/// containing block children. As of the `tables-merged` verb the row/cell
/// payloads carry merge state (`gridSpan`/`vMerge`) and the header-row flag, so
/// a `replace(table)` can author merged-cell tables as tracked changes. Table-,
/// row-, and cell-level *formatting* (borders, shading, widths) remain a
/// deliberate v4 gap and are still rejected at apply time
/// (`TableHasFormattingNotInSpec`) rather than silently best-effort-projected.
#[derive(Clone, Debug)]
pub struct TableBlockSpec {
    pub rows: Vec<TableRowSpec>,
    /// Optional caller-specified TABLE-level formatting (tblStyle / tblBorders /
    /// tblW / tblCellMar), parsed at the v4 edge (RFC-0003 Item 1). `None` on a
    /// bare content spec. On a `replace`, the fields SET here win over the base
    /// table's carried formatting; unset fields still inherit from the base
    /// (`carry_base_formatting_onto_target`). On an `insert`, they define the new
    /// table's look outright.
    pub formatting: Option<TableFormatting>,
}

/// A row of a `TableBlockSpec`.
#[derive(Clone, Debug)]
pub struct TableRowSpec {
    pub cells: Vec<TableCellSpec>,
    /// Whether this is a repeated header row (`w:tblHeader`, §17.4.49). Maps to
    /// `TableRowNode.is_header`.
    pub is_header: bool,
    /// Optional caller-specified row height (`w:trHeight w:val`, twips) and rule
    /// (RFC-0003 Item 1). `None` inherits the base row's height on a replace.
    pub height: Option<u32>,
    pub height_rule: Option<HeightRule>,
}

/// A cell of a `TableRowSpec`. Cells contain block children — paragraphs
/// or nested tables — which are recursively resolved through
/// `resolve_block_spec`.
#[derive(Clone, Debug)]
pub struct TableCellSpec {
    pub content: Vec<BlockSpec>,
    /// Horizontal merge: number of grid columns this cell spans (`w:gridSpan`,
    /// §17.4.17). `None` or `Some(1)` is a single-column cell. Maps to
    /// `TableCellNode.grid_span`.
    pub merge_h: Option<u32>,
    /// Vertical merge state (`w:vMerge`, §17.4.84). `None` is no vertical merge.
    /// Maps to `TableCellNode.v_merge`.
    pub merge_v: Option<VerticalMergeSpec>,
    /// Optional caller-specified CELL-level formatting (tcBorders / shd / tcW /
    /// vAlign / tcMar), parsed at the v4 edge (RFC-0003 Item 1). `None` leaves the
    /// cell unformatted (an `insert`) or inherits the aligned base cell's `tcPr`
    /// (a same-shape `replace`).
    pub formatting: Option<CellFormatting>,
}

/// Authoring-side vertical-merge state for a `TableCellSpec`. The adapter parses
/// the wire `"restart"`/`"continue"` strings into this typed enum at the edge;
/// `resolve_table_spec` maps it 1:1 onto the IR's `VerticalMerge`. We keep a
/// distinct spec enum (rather than reusing the IR enum) so the grammar layer
/// stays decoupled from `domain.rs` per `edit/AGENTS.md` (lift, don't reach into
/// IR types from the wire).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerticalMergeSpec {
    /// `<w:vMerge w:val="restart"/>` — the anchor cell of a vertical merge.
    Restart,
    /// `<w:vMerge/>` / `<w:vMerge w:val="continue"/>` — a continuation cell.
    Continue,
}

/// A schema paragraph block for insert/replace operations.
///
/// Content is `ParagraphContent` directly — both the v3 markup parser and the
/// v4 adapter populate this field at the edge. The engine core only sees the
/// typed content, never raw markup strings (CLAUDE.md "parse at the edges").
#[derive(Clone, Debug)]
pub struct ParagraphBlockSpec {
    /// Optional paragraph role from the document vocabulary.
    pub role: Option<String>,
    /// Paragraph content, already parsed.
    pub content: ParagraphContent,
    /// Whether this paragraph should restart numbering.
    pub restart_numbering: bool,
    /// Optional explicit list membership for the inserted paragraph: attach its
    /// `w:numPr` (numId + ilvl) from the start rather than inheriting the role
    /// exemplar's numbering. When present, `resolve_paragraph_spec` overrides
    /// the cloned exemplar's numbering with this `{num_id, ilvl}`. `num_id` MUST
    /// be a numId the document already uses (resolved against existing list
    /// paragraphs); an unknown numId is refused (`InsertListNumIdUnknown`) — the
    /// engine never fabricates a numbering definition.
    pub list: Option<InsertListSpec>,
}

/// Engine-side list membership for an inserted paragraph (`ParagraphBlockSpec.list`).
/// Carries only the structural numbering coordinates the live `w:numPr` needs.
/// The displayed label is re-derived by Word from `word/numbering.xml` at the
/// target level, so `synthesized_text`/`is_bullet` are NOT carried here — they
/// are derived diff hints, not load-bearing for serialization (the same reasoning
/// as the numbering verb's `Indent`).
#[derive(Clone, Copy, Debug)]
pub struct InsertListSpec {
    /// A numId the document already uses (resolved against existing list
    /// paragraphs at apply time).
    pub num_id: u32,
    /// Indent level (`w:ilvl`, 0..=8, §17.9.3).
    pub ilvl: u32,
}

/// A semantic table-of-contents block for insert operations (day-one scope:
/// insert-only — see `edit_v4::SchemaError::TocNotReplaceable`).
#[derive(Clone, Debug)]
pub struct TocBlockSpec {
    /// Paragraph role from the document vocabulary. The TOC field is inserted
    /// into a paragraph cloned from this exemplar. `None` (the v4 wire's only
    /// option — it never asks the caller for an internal role token) resolves
    /// to the document's default body role via the `"default"`/`"body"` alias
    /// (`resolve_toc_spec`, mirroring `resolve_role_entry`'s existing
    /// fallback for paragraph inserts).
    pub role: Option<String>,
    /// Heading levels to include via the `\\o` field switch.
    pub levels: TocLevelsSpec,
    /// Include hyperlinks in TOC entries (`\\h`).
    pub include_hyperlinks: bool,
    /// Hide page numbers in web layout (`\\z`).
    pub hide_page_numbers_in_web: bool,
    /// Use outline levels in addition to built-in heading styles (`\\u`).
    pub use_outline_levels: bool,
}

/// The complete content of a paragraph, expressed as a flat sequence
/// of text fragments and preserved inline references.
///
/// This is the engine's input representation — not the LLM-facing
/// markup language. A separate parser (future work) translates
/// `<bold>text</bold>` and `<opaque id="..."/>` into this type.
///
/// The preserved inline references must appear in the same order as
/// they appear in the original paragraph. They are fixed structural
/// anchors — they cannot be moved, removed, or reordered. Text may
/// change only in the text sections around them.
#[derive(Clone, Debug)]
pub struct ParagraphContent {
    pub fragments: Vec<ContentFragment>,
}

/// A fragment in the paragraph replacement content.
#[derive(Clone, Debug)]
pub enum ContentFragment {
    /// Plain text. Adjacent Text fragments are normalized (merged)
    /// before processing. Plain text inherits formatting from the
    /// paragraph's role exemplar.
    Text(String),

    /// Styled text carrying an explicit set of inline marks from
    /// the LLM-facing markup (`<bold>`, `<italic>`, etc.). Marks are
    /// applied ON TOP of the exemplar formatting at TextNode construction
    /// time — i.e. the fragment's marks are *additive*.
    ///
    /// Used by the insert path (`resolve_paragraph_spec`) to build
    /// TextNodes with the LLM-specified marks. The `replace` inline-diff
    /// path currently rejects StyledText fragments because threading
    /// marks through the Myers diff is future work; mark-bearing replaces
    /// fall back to whole-paragraph segment replace.
    StyledText { text: String, marks: InlineMarkSet },

    /// Reference to an existing preserved inline node (opaque inline
    /// or hard break) in the target paragraph, by its NodeId.
    ///
    /// The engine looks up the node in the original paragraph and
    /// preserves it exactly (raw_xml, marks, kind, everything).
    /// The node appears at this position in the new content.
    ///
    /// Only OpaqueInlineNode and HardBreakNode are valid referents.
    /// Referencing a TextNode or DecorationNode is an error.
    PreservedInlineRef(NodeId),

    /// A brand-new hyperlink the LLM wants to create at this position.
    /// Parsed from `<link href="...">display text</link>` (external link)
    /// or `<link anchor="bookmark_name">display text</link>` (internal
    /// cross-reference). At least one of `href`/`anchor` must be set.
    ///
    /// At apply time the engine synthesizes an `OpaqueInline{Hyperlink}`
    /// containing a single Normal `HyperlinkRun` with the display text.
    /// The rId for external URLs is allocated by the serializer's
    /// `resolve_rel_rid` callback at export time, so callers do not need
    /// to manage relationships.
    ///
    /// Routes to whole-paragraph segment replace (like StyledText), so
    /// the inline-diff path never sees a NewHyperlink fragment.
    NewHyperlink {
        href: Option<String>,
        anchor: Option<String>,
        text: String,
    },
}

impl ContentFragment {
    /// True when this fragment carries inline marks (`<bold>`, `<italic>`,
    /// etc.) that the word-level inline diff can't thread through its
    /// reconstruction. These fall back to whole-paragraph segment replace.
    ///
    /// `NewHyperlink` is NOT styled: the inline diff handles it directly
    /// by treating each link as an atomic insert (see
    /// `render_section_for_diff` and the placeholder-aware Insert
    /// handler in `reconstruct_section_segments`).
    pub fn is_styled(&self) -> bool {
        matches!(self, ContentFragment::StyledText { .. })
    }

    /// Return the fragment's plain text content, ignoring any marks.
    /// Preserved-inline references return `None` (they are anchors,
    /// not text). NewHyperlink returns its display text.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            ContentFragment::Text(t) => Some(t.as_str()),
            ContentFragment::StyledText { text, .. } => Some(text.as_str()),
            ContentFragment::NewHyperlink { text, .. } => Some(text.as_str()),
            ContentFragment::PreservedInlineRef(_) => None,
        }
    }
}

/// A new-hyperlink atom carried alongside the diff-friendly flat text
/// of a section. The diff sees a single private-use Unicode codepoint
/// in place of each link; this struct holds the link metadata so the
/// segment reconstructor can materialize the `OpaqueInline{Hyperlink}`
/// where the placeholder appears.
#[derive(Clone, Debug)]
struct NewHyperlinkAtom {
    href: Option<String>,
    anchor: Option<String>,
    display: String,
}

/// First codepoint in the Private Use Area block used for hyperlink
/// placeholders inside `render_section_for_diff`. The diff treats each
/// placeholder as a single character that has no match in the old text,
/// so it is always emitted as an Insert. The reconstructor then splits
/// the Insert token at each placeholder.
///
/// We use the start of the BMP PUA range (U+E000) and assign one
/// codepoint per atom (so a section with two links uses U+E000 and
/// U+E001). This caps the per-section link count at 0x1900 = 6,400,
/// far beyond any realistic legal-paragraph link density.
const HYPERLINK_PLACEHOLDER_BASE: u32 = 0xE000;
const HYPERLINK_PLACEHOLDER_MAX: u32 = 0xF8FF;

/// A set of inline marks from the LLM-facing markup — the universal marks
/// applicable to any text span. Five of these (bold/italic/underline/subscript/
/// superscript) map directly to `Mark` enum variants; `strike` maps to
/// `StyleProps.strike = MarkValue::On`. Kept as a plain struct of flags
/// to keep the parser small and to let the insert-path projector compose
/// marks cleanly with exemplar formatting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InlineMarkSet {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub subscript: bool,
    pub superscript: bool,
    /// All-caps display (`w:caps`, §17.3.2.5). A `StyleProps` tri-state, not a
    /// `Mark` enum variant — set turns it `MarkValue::On`.
    pub caps: bool,
    /// Small-caps display (`w:smallCaps`, §17.3.2.33). Same tri-state lift.
    pub small_caps: bool,
}

impl InlineMarkSet {
    pub fn is_empty(&self) -> bool {
        !(self.bold
            || self.italic
            || self.underline
            || self.strike
            || self.subscript
            || self.superscript
            || self.caps
            || self.small_caps)
    }
}

/// Value-bearing run-style properties an author can set as a tracked
/// `w:rPrChange` (§17.13.5.31), carried alongside the boolean `InlineMarkSet`
/// on `EditStep::SetRunFormatting`.
///
/// These map 1:1 onto existing `StyleProps` fields — the IR, serializer, and
/// accept/reject already handle them on the diff/merge side; this struct only
/// lets the *authoring* side request the same deltas. Kept separate from
/// `InlineMarkSet` because that type is `Copy` and shared with the StyledText
/// insert path, and these fields carry values (`String`/enum) the boolean set
/// cannot express.
///
/// v1 scope (fail loud beyond it):
/// - `color`: a literal 6-hex-digit RGB or the literal `"auto"` — theme color
///   references are out of scope (no silent fallback to a literal);
/// - `font_family`: the ascii/hAnsi font slot only — east-asia / complex-script
///   slots and theme fonts are out of scope;
/// - `font_size_half_points`: OOXML half-points; `0` is refused.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunStyleEdit {
    /// Literal text color (6-hex RGB, e.g. `"FF0000"`, or `"auto"`) for w:color.
    pub color: Option<IStr>,
    /// Highlight color per §17.18.40 `ST_HighlightColor` for w:highlight.
    pub highlight: Option<HighlightColor>,
    /// Font family for the w:rFonts ascii/hAnsi slot.
    pub font_family: Option<IStr>,
    /// Font size in half-points for w:sz (e.g. 24 = 12pt).
    pub font_size_half_points: Option<u32>,
    /// Character spacing in twips for w:spacing (§17.3.2.35). Positive expands,
    /// negative condenses, `0` is a legitimate "reset to default tracking".
    /// `None` leaves it untouched.
    pub char_spacing: Option<i32>,
}

impl RunStyleEdit {
    pub fn is_empty(&self) -> bool {
        self.color.is_none()
            && self.highlight.is_none()
            && self.font_family.is_none()
            && self.font_size_half_points.is_none()
            && self.char_spacing.is_none()
    }
}

/// The paragraph-level attributes an author can set as a tracked `w:pPrChange`
/// (§17.13.5.29), carried on `EditStep::SetParagraphFormatting`.
///
/// Each field maps 1:1 onto an existing `ParagraphNode` field — the IR,
/// serializer, and accept/reject already handle them on the diff/merge side;
/// this struct only lets the *authoring* side request the same deltas, exactly
/// as `RunStyleEdit` does for run properties. It is a typed, validated-at-edge
/// intent carrier (grammar), not a new IR shape.
///
/// v1 grammar is intentionally narrow: alignment, indentation, and spacing
/// only. Any other pPr attribute (keep options, borders, shading, numbering)
/// stays role-only via `SetBlockRangeAttr` and cannot be requested here.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParagraphFormattingPatch {
    /// Paragraph alignment (`w:jc`, §17.3.1.13).
    pub align: Option<Alignment>,
    /// Paragraph indentation (`w:ind`, §17.3.1.12).
    pub indent: Option<Indentation>,
    /// Paragraph spacing — line + before/after (`w:spacing`, §17.3.1.33).
    pub spacing: Option<ParagraphSpacing>,
    /// Paragraph borders (`w:pBdr`, §17.3.1.24).
    pub borders: Option<crate::domain::ParagraphBorders>,
    /// Paragraph shading (`w:shd`, §17.3.1.31).
    pub shading: Option<crate::domain::Shading>,
}

impl ParagraphFormattingPatch {
    pub fn is_empty(&self) -> bool {
        self.align.is_none()
            && self.indent.is_none()
            && self.spacing.is_none()
            && self.borders.is_none()
            && self.shading.is_none()
    }
}

/// The grammar for an in-place cell-formatting change, carried on
/// `EditStep::SetCellFormatting`. Like [`ParagraphFormattingPatch`], it is a
/// typed, validated-at-edge intent carrier (not a new IR shape): each field is
/// `Some` only when the caller asked to set it, so unrequested cell properties
/// are byte-preserved.
///
/// The grammar covers exactly the five `tcPr` properties the accept/reject
/// projection restores on a `w:tcPrChange` reject (`tracked_model.rs`: width,
/// borders, shading, vertical alignment, margins). The other `CellFormatting`
/// fields (`no_wrap`, `text_direction`, `tc_fit_text`) are intentionally NOT in
/// the grammar: reject does not restore them, so authoring them as a tracked
/// change would not round-trip. They stay out until the projection covers them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CellFormattingPatch {
    /// Cell borders (`w:tcBorders`).
    pub borders: Option<BorderSet>,
    /// Cell shading (`w:shd`, §17.4.33).
    pub shading: Option<Shading>,
    /// Cell width (`w:tcW`, §17.4.72).
    pub width: Option<TableMeasurement>,
    /// Vertical alignment (`w:vAlign`, §17.4.84).
    pub v_align: Option<VerticalAlignment>,
    /// Per-cell margin overrides (`w:tcMar`, §17.4.41), in twips.
    pub margins: Option<CellMargins>,
}

impl CellFormattingPatch {
    pub fn is_empty(&self) -> bool {
        self.borders.is_none()
            && self.shading.is_none()
            && self.width.is_none()
            && self.v_align.is_none()
            && self.margins.is_none()
    }
}

/// The row-level properties a `SetRowFormatting` step may set, each `Option`
/// (absent = leave unchanged). Scoped to exactly the two `trPr` properties the
/// accept/reject projection restores (`tracked_model.rs`: reject restores
/// `previous_height` / `previous_height_rule`) — authoring a field reject won't
/// revert would be a lie. Drives `EditStep::SetRowFormatting`; like
/// [`CellFormattingPatch`], it is an in-place property delta on ONE row that
/// bypasses the whole-table v4 replace schema.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RowFormattingPatch {
    /// Row height in twips (`w:trHeight w:val`, §17.4.81).
    pub height: Option<u32>,
    /// Row height rule (`w:trHeight w:hRule`, §17.18.37 `ST_HeightRule`).
    pub height_rule: Option<HeightRule>,
}

impl RowFormattingPatch {
    pub fn is_empty(&self) -> bool {
        self.height.is_none() && self.height_rule.is_none()
    }
}

/// The grammar for an in-place TABLE-level formatting change, carried on
/// `EditStep::SetTableFormatting`. Like [`CellFormattingPatch`], it is a typed,
/// validated-at-edge intent carrier (not a new IR shape): each field is `Some`
/// only when the caller asked to set it, so unrequested `tblPr` properties — and
/// every row/cell — are byte-preserved.
///
/// The grammar covers exactly the three `tblPr` properties the accept/reject
/// projection restores on a `w:tblPrChange` reject (`tracked_model.rs`:
/// `previous_width` / `previous_borders` / `previous_default_cell_margins`). The
/// other [`TableFormatting`] fields (style, alignment, indent, layout, cell
/// spacing, positioning, banding, …) are intentionally NOT in the grammar:
/// `TableFormattingChange` does not carry them, so reject would not restore them
/// — authoring them as a tracked change would not round-trip (CLAUDE.md "no
/// silent fallbacks"). In particular there is **no** table-level shading:
/// `TableFormatting` has no `shading` field and `tblPr` carries none (cell
/// shading lives on `w:tcPr` and is authored via `SetCellFormatting`), so a
/// table-level shading request has nothing to land on and is excluded by design.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TableFormattingPatch {
    /// Table borders (`w:tblBorders`, §17.4.39).
    pub borders: Option<BorderSet>,
    /// Table width (`w:tblW`, §17.4.64).
    pub width: Option<TableMeasurement>,
    /// Default cell margins (`w:tblCellMar`, §17.4.43), in twips.
    pub default_cell_margins: Option<CellMargins>,
}

impl TableFormattingPatch {
    pub fn is_empty(&self) -> bool {
        self.borders.is_none() && self.width.is_none() && self.default_cell_margins.is_none()
    }
}

/// An atomic batch of editing operations.
///
/// Either all steps apply successfully, or none do.
#[derive(Clone, Debug)]
pub struct EditTransaction {
    /// The steps to apply, in order.
    pub steps: Vec<EditStep>,

    /// Optional high-level description of the change set.
    pub summary: Option<String>,

    /// Whether the transaction should materialize as tracked revisions or as
    /// direct document mutations.
    pub materialization_mode: MaterializationMode,

    /// Revision metadata applied to all tracked changes
    /// generated by this transaction.
    pub revision: RevisionInfo,
}

/// Why a step failed. Each variant carries enough context to
/// produce an actionable error message.
///
/// `StoryNotFound` / `StoryBlockNotFound` carry the edit-layer
/// [`crate::edit::StoryRef`]. `StoryRef` is now `pub` (the header/footer
/// authoring verbs need callers to address a story by part name when building
/// `EditStep::EditHeader` / `EditFooter`), so this error type leaks no private
/// interface.
#[derive(Clone, Debug)]
pub enum EditError {
    /// The target block_id does not exist in the document.
    BlockNotFound { block_id: NodeId, step_index: usize },

    /// The target block exists but is not a paragraph.
    NotAParagraph {
        block_id: NodeId,
        actual_kind: &'static str,
        step_index: usize,
    },

    /// The target block has a non-Normal tracking status.
    BlockHasTrackedStatus {
        block_id: NodeId,
        status: &'static str,
        step_index: usize,
    },

    /// The target paragraph contains segments with non-Normal
    /// tracking status.
    ParagraphContainsTrackedSegments { block_id: NodeId, step_index: usize },

    /// The `expect` substring was not found in any single text
    /// section of the paragraph.
    ExpectMismatch {
        block_id: NodeId,
        expected: String,
        actual_text: String,
        step_index: usize,
    },

    /// The caller supplied a block semantic hash precondition that does not
    /// match the current target block.
    BlockSemanticHashMismatch {
        block_id: NodeId,
        expected: String,
        actual: String,
        step_index: usize,
    },

    /// An anchor-relative span selector referenced an opaque anchor id that does
    /// not exist in the target paragraph. (Span addressing never falls back to
    /// substring; an absent anchor fails loud.)
    AnchorNotFound {
        block_id: NodeId,
        anchor_id: NodeId,
        step_index: usize,
    },

    /// A span handle (`s_<n>`) is out of range for the target paragraph — the
    /// block changed since the read the handle came from. (The block guard
    /// normally catches this first; this is the defensive resolve-time check so
    /// a handle is never resolved against the wrong inlines.)
    SpanHandleStale {
        block_id: NodeId,
        handle: String,
        span_count: usize,
        step_index: usize,
    },

    /// A resolved span range overlaps a tracked (inserted/deleted) segment. The
    /// splice operates over Normal inline content only (the range-status
    /// predicate); tracked segments are walls — they live outside the range
    /// and are carried by reference. A range overlapping one is refused
    /// rather than silently rewriting across a tracked-change boundary.
    /// (Editing tracked content in place is delivery step 2/3.)
    SpanCrossesTrackedSegment { block_id: NodeId, step_index: usize },

    /// The resolved span's visible text does not equal the op's `expect`
    /// precondition (the text-identity check). The handle resolved, the guard may
    /// even have matched, but the range denotes different text than the
    /// reader saw — re-read the block and mint fresh handles.
    SpanTextMismatch {
        block_id: NodeId,
        expected: String,
        actual: String,
        step_index: usize,
    },

    /// The resolved span range would split a paired range marker (the
    /// brackets predicate): one member of the pair is inside the targeted
    /// range and its partner is outside. The load-bearing case is a bookmark
    /// — a `REF` field targets it by name, so detaching a `bookmarkStart`
    /// from the text it covers silently changes the cross-reference. Refused
    /// rather than auto-extending the range (which would edit more than
    /// asked).
    SpanSplitsBracketPair {
        block_id: NodeId,
        /// The marker-pair kind that blocked the edit, e.g. "bookmark",
        /// "commentRange", "perm" — doubles as refusal instrumentation.
        bracket_kind: String,
        /// The pair id shared by the two markers (e.g. the bookmark `w:id`).
        pair_id: String,
        step_index: usize,
    },

    /// Span replace does not support styled (`<bold>` etc.) replacement text
    /// yet. The whole-paragraph replace routes styled content to the segment
    ///-replace materializer; there is no equivalent range-scoped path, and
    /// silently dropping the mark intent is forbidden. Refused loud.
    SpanStyledContentUnsupported { block_id: NodeId, step_index: usize },

    /// One or more preserved inline nodes (opaque tokens, hard breaks)
    /// present in the original paragraph are missing from the replacement
    /// content. This maps to the "opaque_preservation" validation check
    /// in the typed validation-error wire shape.
    ///
    /// The engine groups ALL missing anchors into a single error so that
    /// the caller (LLM retry or end-user) can fix them in one pass instead
    /// of discovering them one at a time.
    OpaqueDestroyed {
        step_index: usize,
        target_block_id: NodeId,
        /// Stable IDs of the preserved inlines that would be destroyed by
        /// this replace step. The names mirror the spec's `<opaque id="…"/>`
        /// tokens — hard-break anchors are included here too because they
        /// are projected to the LLM as the same kind of preserved-inline
        /// reference.
        missing_opaque_ids: Vec<String>,
        /// Parallel vector: the engine-level kind label for each missing
        /// anchor, e.g. "opaque" or "hard_break". Aligned with
        /// `missing_opaque_ids` by index.
        missing_inline_kinds: Vec<&'static str>,
        /// Short preview of the original paragraph's visible text, suitable
        /// for display in a retry prompt or user-facing error.
        original_text_preview: String,
    },

    /// The replacement content references a preserved inline that
    /// does not exist in the original paragraph.
    PreservedInlineNotFound {
        block_id: NodeId,
        referenced_id: NodeId,
        step_index: usize,
    },

    /// A preserved inline appears more than once in the replacement.
    DuplicatePreservedInlineRef {
        block_id: NodeId,
        inline_id: NodeId,
        step_index: usize,
    },

    /// The preserved inlines in the replacement appear in a different
    /// order than in the original paragraph.
    PreservedInlineOrderChanged { block_id: NodeId, step_index: usize },

    /// The replacement content references a node that exists in the
    /// paragraph but is not a preserved inline type.
    NotAPreservedInline {
        block_id: NodeId,
        referenced_id: NodeId,
        actual_kind: &'static str,
        step_index: usize,
    },

    /// The paragraph contains a structure that the MVP engine cannot
    /// safely rewrite.
    UnsupportedParagraphStructure {
        block_id: NodeId,
        reason: String,
        step_index: usize,
    },

    /// Insert/replace referenced a paragraph role that doesn't exist.
    ParagraphRoleNotFound { role: String, step_index: usize },

    /// The requested role exists but can't be materialized honestly yet.
    UnsupportedParagraphRole {
        role: String,
        reason: String,
        step_index: usize,
    },

    /// The current engine only supports plain inserted text.
    UnsupportedInlineMarkup { step_index: usize, snippet: String },

    /// Numbering restart for inserted paragraphs is not implemented yet.
    UnsupportedNumberingRestart {
        role: Option<String>,
        step_index: usize,
    },

    /// A `move` step targeted a destination anchor that falls inside
    /// the source range. Moving a range into itself is undefined — the
    /// source blocks would be both deleted and inserted at overlapping
    /// positions.
    MoveDestinationInsideSource {
        from_block_id: NodeId,
        to_block_id: NodeId,
        dest_anchor_id: NodeId,
        step_index: usize,
    },

    /// A structural op's destination anchor (`move`'s `destination`,
    /// `insert`'s `target`) names a block that is a tracked-move SOURCE — a
    /// `w:moveFrom` shadow, still present at its ORIGINAL position with
    /// `TrackingStatus::Deleted` + `move_id` set. Anchoring there is
    /// ambiguous: the shadow no longer marks where that content lives (the
    /// moveTo copy does), so "insert after/before this anchor" would resolve
    /// against a position the content has already left. The engine never
    /// guesses which position the caller meant — refuse and name the moveTo
    /// copy to anchor on instead.
    ///
    /// `moved_by_step_index` is `Some` when an earlier step of THIS
    /// transaction performed the move (chained single-block moves anchoring
    /// each hop on the previous hop's source id are the common trigger);
    /// `None` when the anchor was already a moveFrom source in the document
    /// the transaction started from (a previously committed move, or one
    /// imported from a DOCX that already carried `w:moveFrom`/`w:moveTo`).
    AmbiguousAnchorAfterMove {
        anchor_id: NodeId,
        moved_by_step_index: Option<usize>,
        /// `None` when the moveTo copy cannot be located — a dirty import can
        /// carry an unpaired `w:moveFrom` (the importer tags blocks as
        /// encountered, it does not validate pairing), so the hint degrades
        /// rather than the refusal crashing.
        moved_to_block_id: Option<NodeId>,
        step_index: usize,
    },

    /// `ReplaceHyperlinkText` could not find an `OpaqueInline` with the
    /// given `hyperlink_id`, anywhere in the document.
    HyperlinkNotFound {
        hyperlink_id: NodeId,
        step_index: usize,
    },

    /// The targeted opaque inline is not a hyperlink (it's a field, a
    /// drawing, a footnote reference, etc.).
    NotAHyperlink {
        hyperlink_id: NodeId,
        actual_kind: &'static str,
        step_index: usize,
    },

    /// The hyperlink already contains tracked changes inside (one or more
    /// runs are `Inserted` or `Deleted`). The MVP refuses to edit such a
    /// hyperlink; callers must accept/reject the existing changes first.
    HyperlinkContainsTrackedChanges {
        hyperlink_id: NodeId,
        step_index: usize,
    },

    /// `ReplaceHyperlinkText` supplied an `expect_href` or `expect_anchor`
    /// that does not match the target hyperlink's existing value. The
    /// `replace` verb on a hyperlink preserves the URL/anchor; to change
    /// them, the caller must use `set_attr`. Failing loudly here prevents
    /// the silent-href-drop case where a caller thinks they are editing
    /// both display text and target. Also raised by `SetHyperlinkAttr`
    /// when its `expect_href` / `expect_anchor` preconditions fail.
    HyperlinkAttrMismatch {
        hyperlink_id: NodeId,
        attr: &'static str,
        expected: Option<String>,
        actual: Option<String>,
        step_index: usize,
    },

    /// The target block_id exists and is a block, but it is not a table —
    /// caller asked the engine to `replace(table)` against a paragraph or
    /// opaque block.
    NotATable {
        block_id: NodeId,
        actual_kind: &'static str,
        step_index: usize,
    },

    /// A `TableBlockSpec` carries an empty `rows` vector. v4 forbids
    /// zero-row tables (every table has at least one row of at least one
    /// cell), and the engine refuses to materialize one — a table with no
    /// rows is not a meaningful edit.
    EmptyTableStructure { step_index: usize },

    /// A `TableRowSpec` has an empty `cells` vector. Same reasoning as
    /// `EmptyTableStructure`.
    EmptyRowContent { step_index: usize, row_index: usize },

    /// A `TableCellSpec` has an empty `content` vector. v4 requires every
    /// cell to carry at least one block; the engine has no policy for
    /// materializing a literally-empty cell.
    EmptyCellContent {
        step_index: usize,
        row_index: usize,
        cell_index: usize,
    },

    /// (tables-merged) The replacement table is a **ragged grid**: a row's
    /// logical column count (the sum of its cells' `gridSpan`s) differs from the
    /// first row's. OOXML tables are rectangular (§17.4.17 / `w:tblGrid`); the
    /// positional matched-row cell alignment in `apply_table_structure_changed`
    /// relies on uniform width, so v1 refuses rather than guess column identity.
    /// Replaces the former `TableHasMergedCellsNotInSpec` base-table refusal:
    /// merged cells are now authorable, but the resulting grid must be valid.
    RaggedTableGrid {
        row_index: usize,
        expected_columns: u32,
        actual_columns: u32,
        step_index: usize,
    },

    /// (tables-merged) The replacement table has a `vMerge=continue` cell with
    /// no `vMerge=restart` anchor above it in the same logical column (§17.4.84).
    /// `canonicalize_table` rejects this downstream; we catch it at authoring
    /// time so the error names the offending row/cell rather than surfacing as
    /// an opaque canonicalization failure.
    OrphanVMergeContinue {
        row_index: usize,
        cell_index: usize,
        column: u32,
        step_index: usize,
    },

    /// (table_ops) A granular table op named a `row_index` past the table's
    /// last row. The engine refuses rather than clamp — a row index that
    /// doesn't exist is a caller bug, not a guessable default.
    TableRowIndexOutOfRange {
        block_id: NodeId,
        row_index: usize,
        row_count: usize,
        step_index: usize,
    },

    /// (table_ops) A granular table op named a `col_index` past the row's last
    /// column (or the table's uniform width). Same fail-loud reasoning as
    /// `TableRowIndexOutOfRange`.
    TableColumnIndexOutOfRange {
        block_id: NodeId,
        col_index: usize,
        column_count: usize,
        step_index: usize,
    },

    /// (table_ops) A column insert/delete (or merge) was requested on a table
    /// whose grid is not simple (some cell carries `gridSpan>1` or `vMerge`).
    /// Positional column identity is ambiguous on a merged grid, so v1 refuses
    /// rather than guess which logical column a spanning cell occupies (same
    /// reasoning as the `tables-merged` ragged refusal).
    TableColumnOpOnMergedGrid { block_id: NodeId, step_index: usize },

    /// (table_ops) A `MergeCells` op named a region that is not a clean
    /// rectangle (start index past end index, or a single cell). `reason`
    /// names the defect.
    MergeRegionNotRectangular {
        block_id: NodeId,
        reason: String,
        step_index: usize,
    },

    /// (table_ops) A row/column delete would leave the table with no rows or no
    /// columns. A zero-row / zero-column table is not a meaningful structure
    /// (and would fail canonicalization); the engine refuses rather than
    /// produce one. Delete the whole table block instead.
    TableWouldBeEmpty { block_id: NodeId, step_index: usize },

    /// (table_ops) `InsertRow`'s `cells` named more texts than the reference
    /// row has columns. The engine refuses rather than clamp the list or
    /// widen the grid — a caller that miscounted columns has a wrong request,
    /// not one to silently truncate.
    TableInsertRowCellCountExceedsColumns {
        block_id: NodeId,
        given: usize,
        columns: usize,
        step_index: usize,
    },

    /// (table_ops) `SetCellText` targeted a cell that is not a clean edit target
    /// (it already carries a tracked structural insert/delete). `reason` names
    /// the defect; resolve the structural change first.
    TableCellNotEditable {
        block_id: NodeId,
        reason: String,
        step_index: usize,
    },

    /// The base table carries non-default formatting at the table, row,
    /// or cell level. v4 cannot express table formatting today, so a
    /// replace would silently drop borders, shading, alignment, widths,
    /// etc. Fail-fast prevents that.
    TableHasFormattingNotInSpec {
        table_id: NodeId,
        /// Where the formatting was found: "table", "row[N]", or
        /// "row[N].cell[M]".
        location: String,
        step_index: usize,
    },

    /// A granular structural table op (insert/delete row/column, merge) was
    /// requested on a table that carries UNRESOLVED tracked changes — a row/cell
    /// tracked insert or delete, or a pending `tblPrChange`/`trPrChange`/
    /// `tcPrChange`. The structural diff assumes a clean base; layering a new
    /// revision over an in-flight one would interleave two change layers
    /// ambiguously (RFC-0003: row-level tracked-change markup belongs to the
    /// revision model, not the edit schema). Accept or reject the existing
    /// changes first. `location` names where the in-flight change sits.
    TableMidRedline {
        table_id: NodeId,
        location: String,
        step_index: usize,
    },

    /// (RFC-0003 Item 1) A TRACKED `replace(table)` carried caller-set table/row/
    /// cell formatting. That can't be represented as a reversible tracked change
    /// (table/row `*PrChange` doesn't cover style, and applying it directly would
    /// break reject-all == base), so it is refused. Use `materialization_mode:
    /// "direct"` to apply the new look wholesale, or the in-place
    /// `set_table_format` / `set_row_format` / `set_cell_format` verbs to author a
    /// tracked formatting change. Spec formatting on an `insert(table)` is fine.
    TableSpecFormattingRequiresDirect { block_id: NodeId, step_index: usize },

    /// `SetHyperlinkAttr` was dispatched with neither `new_href` nor
    /// `new_anchor` set. The v4 adapter rejects empty `set_attr.attrs` at
    /// the schema layer; this is the engine-side defence against direct
    /// callers (tests, future surfaces) that bypass the adapter and pass
    /// an empty mutation request.
    HyperlinkSetAttrNoOp {
        hyperlink_id: NodeId,
        step_index: usize,
    },

    /// `SetRunFormatting` was dispatched with no marks set — refusing a no-op
    /// formatting request.
    NoFormattingRequested { step_index: usize },

    /// `SetRunFormatting` was asked to set a color that is neither a 6-hex-digit
    /// RGB value nor the literal `"auto"`. We refuse rather than coerce.
    InvalidColorValue { value: String, step_index: usize },

    /// `SetRunFormatting` was asked to set a font size of zero half-points. A
    /// zero size is meaningless; we refuse rather than emit it.
    InvalidFontSize { step_index: usize },

    /// `SetParagraphFormatting` was dispatched with an empty patch (no
    /// alignment, indentation, or spacing set) — refusing a no-op pPrChange.
    NoParagraphFormattingRequested { step_index: usize },

    /// `SetCellFormatting` was dispatched with an empty patch (no borders,
    /// shading, width, vertical alignment, or margins set) — refusing a no-op
    /// tcPrChange.
    NoCellFormattingRequested { step_index: usize },

    /// `SetRowFormatting` was dispatched with an empty patch (no height or
    /// height rule set) — refusing a no-op trPrChange.
    NoRowFormattingRequested { step_index: usize },

    /// `SetRowFormatting` targeted a row that is not a clean format target (it
    /// carries a tracked structural insert/delete, or already carries a tracked
    /// `trPrChange`). `reason` names the defect; resolve it first.
    TableRowNotEditable {
        block_id: NodeId,
        reason: String,
        step_index: usize,
    },

    /// `SetTableFormatting` was dispatched with an empty patch (no borders,
    /// width, or default cell margins set) — refusing a no-op tblPrChange.
    NoTableFormattingRequested { step_index: usize },

    /// `SetTableFormatting` was asked to author a tblPrChange on a table that
    /// already carries one (a tracked tblPrChange). Accept or reject the existing
    /// change before formatting again — the engine refuses to stack.
    TableAlreadyHasFormattingChange { block_id: NodeId, step_index: usize },

    /// `InsertCrossReference` was given an empty bookmark name. A REF / PAGEREF
    /// field with no bookmark target is meaningless; we refuse rather than
    /// default it.
    CrossRefEmptyBookmark { step_index: usize },

    /// `SetParagraphNumbering` was asked to set numbering structurally equal to
    /// the paragraph's current numbering — refusing a no-op change.
    NoNumberingChangeRequested { block_id: NodeId, step_index: usize },

    /// `SetParagraphNumbering::SetLevel` was applied to a paragraph with no
    /// current structural numbering. Attach a list first.
    NumberingLevelOnUnnumbered { block_id: NodeId, step_index: usize },

    /// An `Indent` / `Outdent` would move the list level outside the OOXML
    /// 0..=8 range (`w:ilvl`, §17.9.3). The engine refuses rather than clamp:
    /// "indent past the deepest level" is a caller bug, not a guessable no-op.
    NumberingLevelOutOfBounds {
        block_id: NodeId,
        requested: i64,
        step_index: usize,
    },

    /// An inserted paragraph's `list.num_id` does not match any numbering
    /// definition the document already uses. The engine resolves an insert's
    /// numId against existing list paragraphs — it never fabricates a new
    /// `w:num`/`w:abstractNum` definition (`word/numbering.xml` is not on the
    /// `CanonDoc` value). Read a sibling list item's `list.num_id` from the read
    /// surface and reuse it. `available` lists the numIds the document does use.
    InsertListNumIdUnknown {
        requested: u32,
        available: Vec<u32>,
        step_index: usize,
    },

    /// `SetParagraphNumbering` targeted a manual-numbering (literal-prefix)
    /// paragraph; converting manual prefixes to structural numbering is not
    /// supported in v1.
    NumberingManualPrefixUnsupported { block_id: NodeId, step_index: usize },

    /// `SetParagraphNumbering::Split` targeted a paragraph with no structural
    /// numbering. A split divides an existing numbered/bulleted list at a list
    /// item; an unnumbered paragraph is not a list item, so there is nothing to
    /// split. (Mirrors `NumberingLevelOnUnnumbered` but is a distinct verb so
    /// the message is precise.)
    NumberingSplitOnUnnumbered { block_id: NodeId, step_index: usize },

    /// A story-targeted edit named a footnote/endnote/comment story that does
    /// not exist in the document. No silent fallback to the body or to the
    /// first story — the missing story id is surfaced verbatim.
    StoryNotFound { story: StoryRef, step_index: usize },

    /// A story-targeted edit resolved the story but the block id does not
    /// exist within it. Story block ids restart per story, so this carries
    /// the `(StoryRef, NodeId)` pair that failed to resolve.
    StoryBlockNotFound {
        story: StoryRef,
        block_id: NodeId,
        step_index: usize,
    },

    /// A find-replace needle straddles a barrier anchor (opaque inline, field,
    /// hyperlink, hard break) in this paragraph, and the caller chose
    /// `BarrierPolicy::Fail`. We never half-edit across a barrier — the planner
    /// surfaces the straddle instead of silently skipping it.
    FindReplaceBarrierStraddle {
        block_id: NodeId,
        needle: String,
        step_index: usize,
    },
    /// `InsertBookmark` / `RenameBookmark` was given an empty or whitespace-only
    /// bookmark name. A nameless bookmark is unreferenceable; we refuse rather
    /// than default it.
    BookmarkEmptyName { step_index: usize },

    /// `InsertBookmark` / `RenameBookmark` requested a bookmark name that is
    /// already in use in the target paragraph. Carries the offending name.
    BookmarkDuplicateName { name: String, step_index: usize },

    /// `RenameBookmark` / `RemoveBookmark` named a bookmark that does not exist
    /// in the target paragraph. Carries the missing name.
    BookmarkNotFound { name: String, step_index: usize },

    /// `RemoveBookmark` found the named `w:bookmarkStart` but no paired
    /// `w:bookmarkEnd` (sharing its `w:id`) in the same paragraph. A
    /// multi-paragraph bookmark is out of v1 scope; we refuse rather than
    /// partially remove. Carries the bookmark name.
    BookmarkOrphanEnd { name: String, step_index: usize },

    /// A bookmark decoration's `raw_xml` failed to parse. These bytes were
    /// synthesized here or by the serializer, so this signals a programmer bug,
    /// surfaced loudly rather than swallowed.
    BookmarkRawXmlUnparsable,
    /// `ApplyStyle` named a style ID that does not exist in the document's
    /// style table (`word/styles.xml`). Emitted by the **package-aware caller**
    /// (the runtime), not by the body-content `apply_transaction` path, which
    /// has no style table on `&CanonDoc`. No silent acceptance of a dangling
    /// style — the missing id is surfaced verbatim.
    StyleNotFound {
        block_id: NodeId,
        style_id: String,
        step_index: usize,
    },

    /// `ApplyStyle` requested the style the paragraph already carries — a
    /// visually-empty `pPrChange`. We refuse rather than author a no-op change.
    NoStyleChangeRequested {
        block_id: NodeId,
        style_id: String,
        step_index: usize,
    },
    /// `SetImageAttributes` could not find an `OpaqueInline` with the given
    /// `drawing_id` in the target block.
    DrawingNotFound {
        drawing_id: NodeId,
        step_index: usize,
    },

    /// `SetImageAttributes` resolved `drawing_id` to an opaque inline that is
    /// not a drawing (a field, a hyperlink, a footnote reference, etc.).
    NotADrawing {
        drawing_id: NodeId,
        step_index: usize,
    },

    /// `SetImageAttributes` targeted a drawing whose `raw_xml` is absent, so
    /// there is no display XML to mutate. The IR cannot have lost the drawing's
    /// markup honestly; we refuse rather than fabricate it.
    DrawingMissingRawXml {
        drawing_id: NodeId,
        step_index: usize,
    },

    /// `SetImageAttributes` could not re-parse the drawing's `raw_xml`. Surfaces
    /// the parse error so the corruption is visible, not swallowed.
    DrawingRawXmlParse {
        drawing_id: NodeId,
        reason: String,
        step_index: usize,
    },

    /// `SetImageAttributes` requested an edit whose target element is absent
    /// from the drawing's XML (resize but no `wp:extent`, or alt-text but no
    /// `wp:docPr`). We fail rather than silently skip the requested change.
    ImageAttributeTargetAbsent {
        drawing_id: NodeId,
        attribute: &'static str,
        step_index: usize,
    },

    /// `SetImageAttributes` was dispatched with neither a resize nor an
    /// alt-text edit — refusing a no-op mutation.
    NoImageAttributeRequested { step_index: usize },

    /// `SetImageLayout` requested a position or wrap edit on a drawing that is
    /// `wp:inline` (not floating). Position and wrap exist only on a `wp:anchor`;
    /// converting inline⇄anchor is a structural transform, out of scope for this
    /// attribute verb. We fail loud rather than silently skip.
    ImageLayoutRequiresAnchor {
        drawing_id: NodeId,
        step_index: usize,
    },

    /// `SetImageLayout` requested a crop but the drawing has no `pic:blipFill`
    /// (e.g. it is a shape or chart, not a raster picture) — the `a:srcRect`
    /// target is absent. We fail rather than silently skip.
    ImageLayoutTargetAbsent {
        drawing_id: NodeId,
        target: &'static str,
        step_index: usize,
    },

    /// `SetImageLayout` was dispatched with an empty patch (no crop/position/
    /// wrap) — refusing a no-op mutation.
    NoImageLayoutRequested { step_index: usize },

    /// `CommentReply` / `CommentResolve` / `CommentDelete` named a comment id
    /// (`w:id`) that does not exist in `doc.comments`. No silent fallback — the
    /// missing id is surfaced verbatim.
    CommentTargetNotFound {
        comment_id: String,
        step_index: usize,
    },

    /// `CommentCreate`'s `expect` anchor text was not found in the target
    /// paragraph's visible text (the `Normal` + `Inserted` segments).
    CommentAnchorNotFound {
        block_id: NodeId,
        expected: String,
        actual_text: String,
        step_index: usize,
    },

    /// `CommentCreate`'s `expect` anchor resolves onto DELETED (`w:del` /
    /// inserted-then-deleted) content. A comment on struck text is genuinely
    /// ambiguous (it vanishes on accept-all), so it is refused rather than
    /// anchored on content that is going away.
    CommentAnchorOverlapsDeleted {
        block_id: NodeId,
        expected: String,
        step_index: usize,
    },

    /// `CommentCreate` targeted a whole block whose tracking status is not
    /// `Normal` (an inserted / deleted / moved paragraph). Commenting a paragraph
    /// that merely carries tracked *segments* is allowed; commenting a paragraph
    /// whose existence is itself contested is not (yet).
    CommentOnTrackedBlock {
        block_id: NodeId,
        status: &'static str,
        step_index: usize,
    },

    /// `CommentCreate` / `CommentReply` was given an empty (or whitespace-only)
    /// body. A comment with no text is meaningless; refused rather than
    /// defaulted.
    CommentEmptyBody { step_index: usize },

    /// `CommentDelete` could not locate all three anchor markers
    /// (`commentRangeStart` / `commentRangeEnd` / `commentReference`) for the
    /// comment id. Carries the list of marker kinds that were missing — we
    /// never half-delete a comment range.
    CommentRangeOrphaned {
        comment_id: String,
        missing_markers: Vec<&'static str>,
        step_index: usize,
    },

    /// `CommentReply` named a parent comment that carries no anchor markers in
    /// the document (`commentRangeStart` / `commentRangeEnd` /
    /// `commentReference` for the parent id). A reply is only visible in Word
    /// when it authors its OWN markers beside the parent's span (MS-DOCX
    /// §2.5.1); with no parent anchor to place them against, we refuse rather
    /// than author a reply Word's Comments collection can never surface —
    /// mirroring `CommentDelete`'s no-half-delete discipline. Carries the list
    /// of marker kinds that were missing on the parent.
    CommentParentUnanchored {
        parent_comment_id: String,
        missing_markers: Vec<&'static str>,
        step_index: usize,
    },

    /// `EditNote` / `DeleteNote` named a note id that has no story in the
    /// addressed collection (`doc.footnotes` for a footnote, `doc.endnotes` for
    /// an endnote). No silent fallback — the missing id + kind are surfaced.
    NoteNotFound {
        note_id: String,
        note_kind: &'static str,
        step_index: usize,
    },

    /// `DeleteNote` found the story but no matching reference run in any body
    /// paragraph (or, symmetrically, a reference with no story). Refused as a
    /// half-delete rather than left dangling.
    NoteReferenceMissing {
        note_id: String,
        note_kind: &'static str,
        step_index: usize,
    },

    /// `InsertNote`'s `block_id` resolved to a non-paragraph block (a table or
    /// opaque block). A footnote reference run can only be spliced into a
    /// paragraph.
    NoteAnchorNotAParagraph {
        block_id: NodeId,
        actual_kind: &'static str,
        step_index: usize,
    },

    /// `InsertNote` / `EditNote` was given an empty (or whitespace-only) body.
    /// A note with no text is meaningless; refused rather than defaulted.
    NoteEmptyBody { step_index: usize },

    /// `InsertNote` could not allocate a sequential note id because an existing
    /// footnote/endnote id is non-numeric (a malformed import). Refused with the
    /// offending id rather than guessing.
    NoteIdNotNumeric { note_id: String, step_index: usize },

    /// `EditNote` in `TrackedChange` mode targeted a story whose body is more
    /// than one paragraph. The surgical word-diff (`apply_replace_paragraph_text`)
    /// operates on a single `ParagraphNode`; a multi-paragraph body is out of
    /// v1's "single-paragraph body" scope (module doc), so this refuses rather
    /// than silently diffing only the first paragraph and discarding the rest.
    /// `Direct` mode is unaffected (its wholesale rebuild already handles this
    /// shape, at the cost of dropping extra paragraphs — a pre-existing,
    /// unchanged Direct-mode contract, not something this variant governs).
    NoteBodyMultiParagraph {
        note_id: String,
        note_kind: &'static str,
        paragraph_count: usize,
        step_index: usize,
    },

    /// `SetPageSetup` was dispatched with an empty patch (no page size,
    /// orientation, margins, columns, or gutter). Refusing a no-op mutation
    /// rather than authoring an empty `w:sectPrChange`.
    NoPageSetupRequested { step_index: usize },

    /// A page-setup verb targeted a section that does not exist: the body has no
    /// `w:sectPr`, or the addressed paragraph has no mid-document section break.
    /// `block_id` is `None` for the body section.
    SectionPropertiesNotFound {
        block_id: Option<NodeId>,
        step_index: usize,
    },

    /// `SetPageSetup` targeted a section that already carries a tracked
    /// `w:sectPrChange` (or `InsertSectionBreak` targeted a paragraph that
    /// already owns a `w:sectPr`). Accept or reject the existing change first;
    /// we refuse to stack or clobber. `block_id` is `None` for the body section.
    SectionAlreadyHasTrackedChange {
        block_id: Option<NodeId>,
        step_index: usize,
    },

    /// `SetHeaderFooterMode` was dispatched with neither a `title_page` nor an
    /// `even_and_odd` toggle nor a link op — refusing a no-op mutation.
    NoHeaderFooterModeRequested { step_index: usize },

    /// `SetHeaderFooterMode` LINK named a header/footer kind that has no existing
    /// story to link. v1 links existing stories only; net-new-story creation is
    /// out of scope, so we fail loud rather than synthesize an empty story.
    HeaderFooterRefNotResolvable {
        is_header: bool,
        kind: &'static str,
        step_index: usize,
    },

    /// `CreateHeader` / `CreateFooter` was asked to author a NET-NEW story of a
    /// kind that already exists on the body section. We refuse rather than
    /// silently duplicate the story (which would leave two parts of the same
    /// `w:type` and an ambiguous running head) — the caller should `EditHeader`
    /// / `EditFooter` the existing story instead.
    HeaderFooterAlreadyExists {
        is_header: bool,
        kind: &'static str,
        step_index: usize,
    },

    /// `InsertEquation` was given an OMML fragment that failed to parse. The
    /// parse error is surfaced verbatim rather than swallowed.
    EquationXmlInvalid { reason: String, step_index: usize },

    /// `InsertEquation`'s fragment root local name did not match the requested
    /// placement: `Inline` requires `m:oMath`, `Block` requires `m:oMathPara`.
    /// We never silently re-wrap one form into the other.
    EquationNotMath {
        actual_root: String,
        expected_root: &'static str,
        step_index: usize,
    },

    /// `WrapInContentControl` was given a spec with no distinguishing data (no
    /// tag, no alias, default RichText control). Such a control is
    /// indistinguishable from un-wrapped content; refused rather than authored.
    EmptyContentControlSpec { step_index: usize },

    /// `WrapInContentControl` was given a `w:dataBinding` with an empty `xpath`
    /// or empty `storeItemID`. A binding with no target is unresolvable and
    /// would silently degrade to a plain control in Word; refused at the verb
    /// edge rather than authored.
    MalformedDataBinding {
        reason: &'static str,
        step_index: usize,
    },

    /// `WrapInContentControl` was asked to wrap a whole block in a `w:sdt`.
    /// Deferred (v1): the engine streams body-level opaque blocks from the
    /// original parsed XML by `body_index`, so a synthesized block-level SDT has
    /// no serializable representation today. Run-span wrapping is supported.
    ContentControlBlockUnsupported { step_index: usize },

    /// `WrapBlocksInContentControl` was given a `start`/`end` pair that is not a
    /// valid forward, top-level range (end precedes start). The missing-block
    /// case surfaces as `BlockNotFound`.
    BlockRangeInvalid {
        start_block_id: NodeId,
        end_block_id: NodeId,
        reason: &'static str,
        step_index: usize,
    },

    /// `WrapBlocksInContentControl` found a block in the requested range that is
    /// already enclosed by an authored block-level content control. We never nest
    /// an authored wrap inside an authored one (no serializable representation;
    /// risks an unbalanced `w:sdt`).
    BlockAlreadyWrapped { block_id: NodeId, step_index: usize },

    /// `SetContentControlValue` could not find an `OpaqueInline` with the given
    /// `sdt_id` in the target block.
    ContentControlNotFound { sdt_id: NodeId, step_index: usize },

    /// `SetContentControlValue` resolved `sdt_id` to an opaque inline that is not
    /// a content control (a drawing, a field, an equation, …).
    NotAContentControl { sdt_id: NodeId, step_index: usize },

    /// `SetContentControlValue` targeted an SDT whose `raw_xml` is absent — the
    /// IR cannot have lost the control's markup honestly; refused.
    ContentControlMissingRawXml { sdt_id: NodeId, step_index: usize },

    /// `SetContentControlValue` could not re-parse the SDT's `raw_xml`. The parse
    /// error is surfaced rather than swallowed.
    ContentControlRawXmlParse {
        sdt_id: NodeId,
        reason: String,
        step_index: usize,
    },

    /// `SetContentControlValue`'s value kind is incompatible with the control
    /// type (e.g. set-checked on a plain-text control, or a `selected` value not
    /// in the control's list). The requested value kind and the actual control
    /// kind are surfaced; the value is never coerced or defaulted.
    ContentControlTypeMismatch {
        sdt_id: NodeId,
        requested: &'static str,
        actual: &'static str,
        step_index: usize,
    },

    /// `SetFormFieldValue` could not find an `OpaqueInline` with the given
    /// `field_id` in the target block.
    FormFieldNotFound { field_id: NodeId, step_index: usize },

    /// `SetFormFieldValue` resolved `field_id` to something that is not a fillable
    /// legacy form-field BEGIN anchor: a non-`Field` opaque, a `fldSimple` (which
    /// carries no ffData), an Instruction/Separate/End part, or an unknown
    /// fldCharType. Its `instr` may say FORMTEXT, but there is no ffData to set.
    NotAFormField { field_id: NodeId, step_index: usize },

    /// `SetFormFieldValue`'s begin anchor has `raw_xml: None` — the IR cannot have
    /// lost the field markup honestly; refused.
    FormFieldMissingRawXml { field_id: NodeId, step_index: usize },

    /// `SetFormFieldValue` could not re-parse the begin anchor's `raw_xml` /
    /// `ffData`. The parse error is surfaced rather than swallowed.
    FormFieldRawXmlParse {
        field_id: NodeId,
        reason: String,
        step_index: usize,
    },

    /// `SetFormFieldValue`'s value kind is incompatible with the field type (e.g.
    /// `Checked` on a FORMTEXT). The requested value kind and the actual field
    /// kind are surfaced; the value is never coerced.
    FormFieldTypeMismatch {
        field_id: NodeId,
        requested: &'static str,
        actual: &'static str,
        step_index: usize,
    },

    /// `SetFormFieldValue` with a `Selected` value that is not among the
    /// dropdown's `listEntry` set (§17.16.28). We refuse rather than silently
    /// clamp to an in-range index.
    FormFieldValueNotInList {
        field_id: NodeId,
        value: String,
        step_index: usize,
    },

    /// `SetFormFieldValue`'s begin anchor carries a `ffData` with no
    /// `textInput`/`checkBox`/`ddList` child (§17.16.17) — there is no field-type
    /// state to set. Fail loud rather than guess.
    MalformedFfData {
        field_id: NodeId,
        reason: &'static str,
        step_index: usize,
    },

    /// `SetFormFieldValue`'s result region (between `separate` and `end`) carries
    /// a tracked change (a non-`Normal` segment). We cannot overwrite a
    /// half-tracked result and preserve redline integrity in v1; refused.
    FormFieldResultHasTrackedChanges { field_id: NodeId, step_index: usize },

    /// `SetContentControlValue` was asked for a TRACKED set (`tracked: true`), but
    /// the accept/reject projector does not yet descend into SDT `raw_xml` to
    /// resolve revisions placed inside `sdtContent` (the B1 feature). We refuse
    /// rather than silently downgrade to an untracked set.
    TrackedContentControlSetUnsupported { sdt_id: NodeId, step_index: usize },

    /// `SetTextboxText`'s target `w:txbxContent` already carries a tracked change
    /// (`w:ins`/`w:del`/`w:moveFrom`/`w:moveTo`). A whole-interior replace would
    /// silently flatten those redlines; we refuse so the caller resolves them
    /// first (the M0 accept/reject descent now reaches inside textboxes).
    TextboxHasTrackedChanges {
        drawing_id: NodeId,
        step_index: usize,
    },

    /// `SetTextboxText`'s drawing contains MULTIPLE `w:txbxContent` whose
    /// interiors are NOT identical (a true multi-textbox group shape, not the
    /// `mc:AlternateContent` Choice/Fallback duplicate of one textbox). Replacing
    /// them all with one caller text would be wrong, and silently picking one is
    /// the fallback pattern we kill — refuse. (When the copies ARE identical, as
    /// in the standard AlternateContent emission, all are replaced together.)
    MultipleDistinctTextboxes {
        drawing_id: NodeId,
        count: usize,
        step_index: usize,
    },

    /// `OpaqueTextEdit`: the addressed opaque inline is absent from the host
    /// paragraph, or is not a textbox / inline content control (nothing to
    /// text-edit — body-level content controls use `sdt_text_fill`).
    OpaqueTextTargetNotFound {
        opaque_id: NodeId,
        step_index: usize,
    },

    /// `OpaqueTextEdit`: the `(container_index, paragraph_index)` address does not
    /// resolve to a text-bearing region inside the opaque fragment (stale or
    /// out-of-range — re-run `opaque_text_targets`).
    OpaqueTextRegionNotFound {
        opaque_id: NodeId,
        container_index: usize,
        paragraph_index: usize,
        step_index: usize,
    },

    /// `OpaqueTextEdit`: the opaque inline carries no `raw_xml` to descend into.
    OpaqueTextMissingRawXml {
        opaque_id: NodeId,
        step_index: usize,
    },

    /// `OpaqueTextEdit`: the opaque fragment failed to parse.
    OpaqueTextRawXmlParse {
        opaque_id: NodeId,
        reason: String,
        step_index: usize,
    },

    /// `OpaqueTextEdit`: `find` does not occur in the addressed text region — we
    /// never splice a guessed location.
    OpaqueTextNotFound {
        opaque_id: NodeId,
        find: String,
        step_index: usize,
    },

    /// `OpaqueTextEdit`: the addressed region already carries tracked changes;
    /// resolve them first rather than splicing into an ambiguous base.
    OpaqueTextRegionHasTrackedChanges {
        opaque_id: NodeId,
        step_index: usize,
    },

    /// `OpaqueTextEdit`: the matched span crosses a non-text element (tab / break
    /// / drawing / nested control) — out of the v1 surgical-splice scope.
    OpaqueTextUnsupportedShape {
        opaque_id: NodeId,
        step_index: usize,
    },

    /// `SdtTextFill`: neither or both of an inline target (`sdt_id`) and a block
    /// target (`body_index`) were named — exactly one is required.
    SdtFillAmbiguousTarget { step_index: usize },

    /// `SdtTextFill`: no body-level content control exists at the addressed
    /// `body_index` (stale or wrong index — re-run discovery).
    SdtFillBlockNotFound {
        body_index: usize,
        step_index: usize,
    },

    /// `SdtTextFill`: an empty fill of an already-empty (or bytes-unseen block)
    /// control — a meaningless no-op we refuse rather than silently accept.
    SdtFillEmpty { step_index: usize },

    /// `OpaqueTextEdit`: a Choice/Fallback copy of the addressed textbox
    /// matched by visible text, but its paragraph shape failed to resolve —
    /// splicing the copies that did resolve would leave the others silently
    /// stale (a partial mirror). Refused; nothing was applied.
    OpaqueTextMirrorDivergence {
        opaque_id: NodeId,
        matched: usize,
        spliced: usize,
        step_index: usize,
    },

    /// `SdtTextFill`: a `semantic_hash` precondition was supplied for a BLOCK
    /// (body-level) target. Block-control bytes live in the serialize scaffold,
    /// unreachable from the pure edit core, and block discovery surfaces no
    /// hash to pin against — so the precondition cannot be honored. Refused
    /// loudly rather than silently ignored (the caller believes they have a
    /// stale-edit guard they do not have).
    SdtFillBlockHashUnsupported {
        body_index: usize,
        step_index: usize,
    },

    /// `SdtTextFill`: two fills of the SAME block `body_index` in one
    /// transaction. The second would clobber the first (direct) or splice into
    /// already-tracked bytes (tracked) at save time — refused at the verb edge
    /// where the step index is still known.
    SdtFillDuplicateBlockTarget {
        body_index: usize,
        step_index: usize,
    },

    /// `SdtTextFill`: the control's text is not entirely in direct simple runs —
    /// some hides in a hyperlink / field / nested control. A whole-value set would
    /// relocate that text, so we refuse: a rich content region is not a cleanly
    /// fillable value.
    SdtFillComplexContent { sdt_id: NodeId, step_index: usize },

    /// `InsertImage` / `ReplaceImage` was given empty image bytes. An image part
    /// with no bytes is meaningless and would be a corrupt media part; refused at
    /// the verb edge rather than written.
    ImageBytesEmpty { step_index: usize },

    /// `InsertImage` / `ReplaceImage` was given bytes whose magic signature does
    /// not match the declared format. We never infer or default the format from a
    /// mismatch — a mislabeled blob is refused. `declared` is the content type the
    /// caller asserted.
    UnsupportedImageFormat {
        declared: &'static str,
        step_index: usize,
    },

    /// `ReplaceImage` would stretch: the replacement bytes' intrinsic pixel
    /// aspect ratio disagrees with the requested display extent's aspect ratio
    /// beyond the tolerance. Refused rather than silently distorting the image;
    /// the caller can pass an extent matching the image aspect, or set
    /// `allow_stretch: true` to deliberately override.
    ImageAspectMismatch {
        drawing_id: NodeId,
        intrinsic_w: u32,
        intrinsic_h: u32,
        requested_cx: i64,
        requested_cy: i64,
        step_index: usize,
    },

    /// `ReplaceImage`'s bytes pass the magic-byte check for `format` but their
    /// header could not be decoded to intrinsic pixel dimensions (truncated /
    /// malformed). We cannot validate the aspect against a size we can't read,
    /// and a header we can't decode is a corrupt image we're about to embed —
    /// refused. NOT bypassable by `allow_stretch` (that opts into stretching,
    /// not into corrupt bytes).
    ImageHeaderUndecodable {
        drawing_id: NodeId,
        format: &'static str,
        len: usize,
        step_index: usize,
    },

    /// `CreateStyle` / `ModifyStyle` carried an empty `style_id`. A `w:style` with
    /// no `w:styleId` is unaddressable; refused rather than defaulted.
    StyleDefEmptyId { step_index: usize },

    /// `CreateStyle` / `ModifyStyle` carried an empty `name`. Word requires a
    /// `w:name` for a usable style; refused rather than defaulted.
    StyleDefEmptyName { style_id: String, step_index: usize },

    /// `ModifyStyle`'s addressed `style_id` disagrees with `def.style_id`. The two
    /// must match or the verb would splice a style under the wrong id; refused.
    StyleDefIdMismatch {
        addressed: String,
        definition: String,
        step_index: usize,
    },

    /// `SetDocDefaults` carried neither `font_family` nor `font_size_half_points`.
    /// An op that would set nothing is a no-op the caller did not mean; refused
    /// rather than silently doing nothing.
    DocDefaultsEmpty { step_index: usize },

    /// `BlocksToTable` addressed a source-range block that is not a paragraph
    /// (e.g. a nested table or an opaque block). The verb converts a run of
    /// paragraphs into table rows; a non-paragraph in the range has no text to
    /// project into cells, so the conversion is refused rather than dropping it.
    BlocksToTableNonParagraph {
        block_id: NodeId,
        actual_kind: &'static str,
        step_index: usize,
    },

    /// A `BlocksToTable` source paragraph carried an opaque inline (a drawing,
    /// field, hyperlink, footnote/comment reference, etc.). The conversion
    /// projects only the paragraph's **visible text** into table cells, so an
    /// opaque inline would be silently lost on accept-all. We refuse rather than
    /// destroy it (the standing opaque-preservation invariant — see CLAUDE.md
    /// "no silent fallbacks"). Split the opaque-bearing paragraph out of the
    /// range first.
    BlocksToTableOpaqueInline { block_id: NodeId, step_index: usize },

    /// A header-less `BlocksToTable` source paragraph's text did not split into
    /// the same number of columns as the first row. Without a header there is no
    /// authoritative column count, so the first row's split count fixes the grid
    /// and every subsequent row must match it; a row that does not is refused
    /// with the offending paragraph and its visible text rather than emitting a
    /// ragged table. (With a header the column count is fixed by the header and
    /// short rows are padded — see `BlocksToTable`.)
    BlocksToTableSplitMismatch {
        block_id: NodeId,
        expected_columns: usize,
        actual_columns: usize,
        text: String,
        step_index: usize,
    },

    /// `BlocksToTable` was given an empty `header` cell list, or a `delimiter`
    /// that is the empty string. Neither can produce a well-formed grid; refused
    /// at the verb edge rather than defaulted.
    BlocksToTableEmptySpec {
        reason: &'static str,
        step_index: usize,
    },

    /// A content-bearing replace/splice op resolved to **no change**: the
    /// replacement content equals the target in visible text, marks, and
    /// preserved-inline anchors, so applying it would produce byte-identical
    /// output. Per CLAUDE.md ("no silent fallbacks") the engine refuses to
    /// report a no-op as a successful application — "applied a change" and
    /// "changed nothing" are different outcomes and the caller must be able to
    /// tell them apart. The transaction is atomic, so a no-op op aborts the
    /// whole transaction rather than half-applying its siblings.
    ///
    /// `reason` names *why* nothing changed (e.g. "replacement text and marks
    /// equal the target paragraph") so the caller can fix the op or drop it.
    NoOpEdit {
        block_id: NodeId,
        step_index: usize,
        reason: &'static str,
    },

    /// A write's replacement content begins with an enumeration label that would
    /// STACK onto the paragraph's existing numbering label.
    ///
    /// Model — label vs body. A numbered paragraph has two separable parts: the
    /// **label** ("1.", "(a)") and the **body** ("Events"). After import (and
    /// after every accept, via post-projection re-hoist) the label's canonical
    /// home is the `literal_prefix` FIELD, never the body runs — the serializer
    /// re-emits it from there, and every read surface re-prepends it. So an edit
    /// addresses the **body**; the label is managed separately (by the
    /// `set_numbering` renumber path). When replacement content puts a label at
    /// the head of the *body*, the serializer renders BOTH — the field label and
    /// the body label — producing doubled numbering:
    ///   - the SAME label echoed → `"1.1.\tEvents"` (the agent copied its read);
    ///   - a DIFFERENT label (an attempted in-text renumber) → `"1.2.\tEvents"`
    ///     (the existing field label survives untouched; a literal label in the
    ///     body never supersedes it).
    ///
    /// Both are refused, identically on the whole-paragraph replace and the span
    /// splice (the whole-paragraph path once stripped the label silently; the
    /// span path once let the doubling through — a `"1.1.Events"`
    /// corruption). The agent fixes the op by editing the BODY only and omitting
    /// the label; to change the number, use the renumber verb.
    ///
    /// `current_text` is what the paragraph already reads (label + body) so the
    /// refusal is self-explanatory.
    PrefixDuplicatesLabel {
        block_id: NodeId,
        step_index: usize,
        /// The enumeration label the replacement content's BODY begins with.
        label: String,
        /// The paragraph's own numbering label (its `literal_prefix`, re-emitted
        /// by the serializer). Equal to `label` for a verbatim echo; different
        /// for an attempted in-text renumber — the two cases need different
        /// guidance.
        paragraph_label: String,
        current_text: String,
    },
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EditError::BlockNotFound {
                block_id,
                step_index,
            } => write!(f, "step {step_index}: block '{block_id}' not found"),
            EditError::NotAParagraph {
                block_id,
                actual_kind,
                step_index,
            } => write!(
                f,
                "step {step_index}: block '{block_id}' is a {actual_kind}, not a paragraph"
            ),
            EditError::BlockHasTrackedStatus {
                block_id,
                status,
                step_index,
            } => write!(
                f,
                "step {step_index}: block '{block_id}' has tracked status '{status}'"
            ),
            EditError::ParagraphContainsTrackedSegments {
                block_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: paragraph '{block_id}' contains existing tracked changes"
            ),
            EditError::ExpectMismatch {
                block_id,
                expected,
                step_index,
                ..
            } => write!(
                f,
                "step {step_index}: expect substring not found in paragraph '{block_id}': \
                 expected '{expected}'"
            ),
            EditError::BlockSemanticHashMismatch {
                block_id,
                expected,
                step_index,
                ..
            } => write!(
                f,
                "step {step_index}: semantic hash mismatch for paragraph '{block_id}': \
                 expected '{expected}'"
            ),
            EditError::AnchorNotFound {
                block_id,
                anchor_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: anchor '{anchor_id}' not found in paragraph '{block_id}' \
                 (span addressing never falls back to substring)"
            ),
            EditError::SpanHandleStale {
                block_id,
                handle,
                span_count,
                step_index,
            } => write!(
                f,
                "step {step_index}: span handle '{handle}' is out of range for paragraph \
                 '{block_id}' ({span_count} spans); the block changed since the read"
            ),
            EditError::SpanCrossesTrackedSegment {
                block_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: span range overlaps a tracked segment in paragraph \
                 '{block_id}'; the splice operates over Normal content only — narrow the \
                 range to untracked text, or resolve the existing change first"
            ),
            EditError::SpanTextMismatch {
                block_id,
                expected,
                actual,
                step_index,
            } => write!(
                f,
                "step {step_index}: span text precondition failed in paragraph '{block_id}': \
                 expected {expected:?}, the range resolves to {actual:?}; the block changed \
                 since the read — re-read it and mint fresh handles"
            ),
            EditError::SpanSplitsBracketPair {
                block_id,
                bracket_kind,
                pair_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: span range would split a {bracket_kind} pair \
                 (id '{pair_id}') in paragraph '{block_id}': one marker of the pair is \
                 inside the targeted range and its partner is outside; narrow or move the \
                 range so the pair stays on one side"
            ),
            EditError::SpanStyledContentUnsupported {
                block_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: span replace on paragraph '{block_id}' does not support \
                 styled replacement text yet; use plain text, or a whole-paragraph replace \
                 for mark changes"
            ),
            EditError::OpaqueDestroyed {
                step_index,
                target_block_id,
                missing_opaque_ids,
                missing_inline_kinds,
                ..
            } => {
                // Render the first missing anchor in the display message
                // (matches the spec's example: "opaque node op_2 (footnote_ref)
                // would be destroyed by this replace on p_7"). Additional
                // missing anchors are counted in the trailing summary.
                let first_id = missing_opaque_ids
                    .first()
                    .map(String::as_str)
                    .unwrap_or("?");
                let first_kind = missing_inline_kinds.first().copied().unwrap_or("opaque");
                let extras = missing_opaque_ids.len().saturating_sub(1);
                if extras == 0 {
                    write!(
                        f,
                        "step {step_index}: opaque node '{first_id}' ({first_kind}) \
                         would be destroyed by this replace on '{target_block_id}'"
                    )
                } else {
                    write!(
                        f,
                        "step {step_index}: opaque node '{first_id}' ({first_kind}) and \
                         {extras} other preserved inline(s) would be destroyed by this \
                         replace on '{target_block_id}'"
                    )
                }
            }
            EditError::PreservedInlineNotFound {
                block_id,
                referenced_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: preserved inline '{referenced_id}' not found in \
                 paragraph '{block_id}'"
            ),
            EditError::DuplicatePreservedInlineRef {
                block_id,
                inline_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: preserved inline '{inline_id}' referenced twice \
                 in paragraph '{block_id}'"
            ),
            EditError::PreservedInlineOrderChanged {
                block_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: preserved inlines reordered in paragraph '{block_id}'"
            ),
            EditError::NotAPreservedInline {
                block_id,
                referenced_id,
                actual_kind,
                step_index,
            } => write!(
                f,
                "step {step_index}: '{referenced_id}' in paragraph '{block_id}' is a \
                 {actual_kind}, not a preserved inline"
            ),
            EditError::UnsupportedParagraphStructure {
                block_id,
                reason,
                step_index,
            } => write!(
                f,
                "step {step_index}: unsupported structure in paragraph '{block_id}': {reason}"
            ),
            EditError::ParagraphRoleNotFound { role, step_index } => write!(
                f,
                "step {step_index}: paragraph role '{role}' not found in document vocabulary"
            ),
            EditError::UnsupportedParagraphRole {
                role,
                reason,
                step_index,
            } => write!(
                f,
                "step {step_index}: paragraph role '{role}' is unsupported here: {reason}"
            ),
            EditError::UnsupportedInlineMarkup {
                step_index,
                snippet,
            } => write!(
                f,
                "step {step_index}: inserted inline markup is not supported yet: {snippet}"
            ),
            EditError::UnsupportedNumberingRestart { role, step_index } => {
                if let Some(role) = role {
                    write!(
                        f,
                        "step {step_index}: restart_numbering is not supported yet for role '{role}'"
                    )
                } else {
                    write!(
                        f,
                        "step {step_index}: restart_numbering is not supported yet"
                    )
                }
            }
            EditError::MoveDestinationInsideSource {
                from_block_id,
                to_block_id,
                dest_anchor_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: move destination '{dest_anchor_id}' falls inside \
                 source range '{from_block_id}'..='{to_block_id}'"
            ),
            EditError::AmbiguousAnchorAfterMove {
                anchor_id,
                moved_by_step_index,
                moved_to_block_id,
                step_index,
            } => {
                let cause = match moved_by_step_index {
                    Some(i) => format!("step {i} already moved it"),
                    None => "it is already a moveFrom shadow in the document (an earlier \
                             committed move or an imported one)"
                        .to_string(),
                };
                let remedy = match moved_to_block_id {
                    Some(copy) => {
                        format!("Anchor on '{copy}' (the moved copy) or a stable neighbor instead.")
                    }
                    // Unpaired moveFrom (dirty import): there is no copy to point at.
                    None => "Its moveTo copy could not be located (the document carries an \
                             unpaired moveFrom); anchor on a stable neighbor instead."
                        .to_string(),
                };
                write!(
                    f,
                    "step {step_index}: destination anchor '{anchor_id}' is ambiguous — {cause}, \
                     so it is a moveFrom shadow at its OLD position, not the new one. {remedy}"
                )
            }
            EditError::HyperlinkNotFound {
                hyperlink_id,
                step_index,
            } => write!(f, "step {step_index}: hyperlink '{hyperlink_id}' not found"),
            EditError::NotAHyperlink {
                hyperlink_id,
                actual_kind,
                step_index,
            } => write!(
                f,
                "step {step_index}: inline '{hyperlink_id}' is a {actual_kind}, \
                 not a hyperlink"
            ),
            EditError::HyperlinkContainsTrackedChanges {
                hyperlink_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: hyperlink '{hyperlink_id}' already contains \
                 tracked changes; accept/reject before editing"
            ),
            EditError::HyperlinkAttrMismatch {
                hyperlink_id,
                attr,
                expected,
                actual,
                step_index,
            } => {
                let expected = expected.as_deref().unwrap_or("<none>");
                let actual = actual.as_deref().unwrap_or("<none>");
                write!(
                    f,
                    "step {step_index}: hyperlink '{hyperlink_id}' {attr} mismatch: \
                     payload expected '{expected}', target has '{actual}'. \
                     Use `set_attr` to change a hyperlink's {attr}; `replace` preserves it."
                )
            }
            EditError::NotATable {
                block_id,
                actual_kind,
                step_index,
            } => write!(
                f,
                "step {step_index}: block '{block_id}' is a {actual_kind}, not a table"
            ),
            EditError::EmptyTableStructure { step_index } => write!(
                f,
                "step {step_index}: table payload has no rows; v4 requires at least one row"
            ),
            EditError::EmptyRowContent {
                step_index,
                row_index,
            } => write!(
                f,
                "step {step_index}: table row[{row_index}] has no cells; v4 requires at least one cell per row"
            ),
            EditError::EmptyCellContent {
                step_index,
                row_index,
                cell_index,
            } => write!(
                f,
                "step {step_index}: table row[{row_index}].cell[{cell_index}] has no content; v4 requires at least one block per cell"
            ),
            EditError::RaggedTableGrid {
                row_index,
                expected_columns,
                actual_columns,
                step_index,
            } => write!(
                f,
                "step {step_index}: replacement table row[{row_index}] has {actual_columns} logical \
                 columns (sum of gridSpans) but the first row has {expected_columns}; OOXML tables \
                 must be rectangular. Make every row span the same number of columns."
            ),
            EditError::OrphanVMergeContinue {
                row_index,
                cell_index,
                column,
                step_index,
            } => write!(
                f,
                "step {step_index}: replacement table row[{row_index}].cell[{cell_index}] (logical \
                 column {column}) is a vMerge=continue with no vMerge=restart anchor above it in the \
                 same column; a vertical-merge continuation must extend an open merge (§17.4.84)."
            ),
            EditError::TableRowIndexOutOfRange {
                block_id,
                row_index,
                row_count,
                step_index,
            } => write!(
                f,
                "step {step_index}: table '{block_id}' has {row_count} row(s); row index \
                 {row_index} is out of range"
            ),
            EditError::TableColumnIndexOutOfRange {
                block_id,
                col_index,
                column_count,
                step_index,
            } => write!(
                f,
                "step {step_index}: table '{block_id}' has {column_count} column(s); column index \
                 {col_index} is out of range"
            ),
            EditError::TableColumnOpOnMergedGrid {
                block_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: table '{block_id}' has merged cells (gridSpan/vMerge); column \
                 insert/delete/merge requires a simple grid because column identity is ambiguous \
                 across spanning cells. Refusing rather than guess."
            ),
            EditError::MergeRegionNotRectangular {
                block_id,
                reason,
                step_index,
            } => write!(
                f,
                "step {step_index}: merge region on table '{block_id}' is not a clean rectangle: \
                 {reason}"
            ),
            EditError::TableWouldBeEmpty {
                block_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: deleting that row/column would leave table '{block_id}' with no \
                 rows or columns; delete the whole table block instead"
            ),
            EditError::TableInsertRowCellCountExceedsColumns {
                block_id,
                given,
                columns,
                step_index,
            } => write!(
                f,
                "step {step_index}: insert_row on table '{block_id}' was given {given} cell text(s) \
                 but the reference row has {columns} column(s); give at most {columns} entries \
                 (fewer is fine — the rest are left empty)"
            ),
            EditError::TableCellNotEditable {
                block_id,
                reason,
                step_index,
            } => write!(
                f,
                "step {step_index}: cell in table '{block_id}' is not a clean SetCellText target: \
                 {reason}"
            ),
            EditError::TableHasFormattingNotInSpec {
                table_id,
                location,
                step_index,
            } => write!(
                f,
                "step {step_index}: table '{table_id}' carries non-default formatting at {location}; \
                 v4 schema cannot express table/row/cell formatting, so `replace(table)` would silently drop it. \
                 Use a future formatting-aware op once available."
            ),
            EditError::TableMidRedline {
                table_id,
                location,
                step_index,
            } => write!(
                f,
                "step {step_index}: table '{table_id}' carries an unresolved tracked change at {location}; \
                 a granular structural edit can't be layered over an in-flight revision. \
                 Accept or reject the existing change first."
            ),
            EditError::TableSpecFormattingRequiresDirect {
                block_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: replace(table '{block_id}') carried caller-set formatting on a \
                 TRACKED edit, which can't be a reversible tracked change. Use materialization_mode \
                 'direct', or author the formatting with set_table_format / set_row_format / \
                 set_cell_format."
            ),
            EditError::HyperlinkSetAttrNoOp {
                hyperlink_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: set_attr on hyperlink '{hyperlink_id}' \
                 sets neither href nor anchor; refusing a no-op mutation"
            ),
            EditError::NoFormattingRequested { step_index } => write!(
                f,
                "step {step_index}: set_run_formatting requested no marks; \
                 refusing a no-op formatting change"
            ),
            EditError::InvalidColorValue { value, step_index } => write!(
                f,
                "step {step_index}: set_run_formatting color '{value}' is not a \
                 6-hex-digit RGB value or 'auto'; refusing to coerce"
            ),
            EditError::InvalidFontSize { step_index } => write!(
                f,
                "step {step_index}: set_run_formatting font size of 0 half-points \
                 is meaningless; refusing"
            ),
            EditError::NoParagraphFormattingRequested { step_index } => write!(
                f,
                "step {step_index}: set_paragraph_formatting requested no alignment, \
                 indentation, or spacing; refusing a no-op pPrChange"
            ),
            EditError::NoCellFormattingRequested { step_index } => write!(
                f,
                "step {step_index}: set_cell_formatting requested no borders, shading, \
                 width, vertical alignment, or margins; refusing a no-op tcPrChange"
            ),
            EditError::NoRowFormattingRequested { step_index } => write!(
                f,
                "step {step_index}: set_row_formatting requested no height or height \
                 rule; refusing a no-op trPrChange"
            ),
            EditError::TableRowNotEditable {
                block_id,
                reason,
                step_index,
            } => write!(
                f,
                "step {step_index}: row in table '{block_id}' is not a clean \
                 SetRowFormatting target: {reason}"
            ),
            EditError::NoTableFormattingRequested { step_index } => write!(
                f,
                "step {step_index}: set_table_formatting requested no borders, width, \
                 or default cell margins; refusing a no-op tblPrChange"
            ),
            EditError::TableAlreadyHasFormattingChange {
                block_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: table '{block_id}' already has a tracked formatting \
                 change (tblPrChange); accept or reject it before formatting again"
            ),
            EditError::CrossRefEmptyBookmark { step_index } => write!(
                f,
                "step {step_index}: insert_cross_reference bookmark is empty; \
                 a REF/PAGEREF field needs a bookmark target. Refusing to default it."
            ),
            EditError::NoNumberingChangeRequested {
                block_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: set_paragraph_numbering on '{block_id}' requests \
                 the numbering the paragraph already has; refusing a no-op change"
            ),
            EditError::NumberingLevelOnUnnumbered {
                block_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: set_paragraph_numbering set_level on '{block_id}' \
                 has no list to re-level; attach a list first"
            ),
            EditError::NumberingLevelOutOfBounds {
                block_id,
                requested,
                step_index,
            } => write!(
                f,
                "step {step_index}: indent/outdent on '{block_id}' would move the list level to \
                 {requested}, outside the valid range 0..=8 (w:ilvl, §17.9.3)"
            ),
            EditError::NumberingManualPrefixUnsupported {
                block_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: set_paragraph_numbering on '{block_id}' targets a \
                 manual-numbering (literal-prefix) paragraph; converting manual prefixes \
                 to structural numbering is not supported in v1"
            ),
            EditError::NumberingSplitOnUnnumbered {
                block_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: set_paragraph_numbering split on '{block_id}' has no \
                 list to split; the split point must be a list item (it carries no numbering)"
            ),
            EditError::InsertListNumIdUnknown {
                requested,
                available,
                step_index,
            } => {
                let avail = available
                    .iter()
                    .map(|n| n.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(
                    f,
                    "step {step_index}: inserted paragraph's list.num_id {requested} is not used \
                     by any existing list paragraph (the engine never fabricates a numbering \
                     definition); reuse a sibling list item's num_id (in-use num_ids: [{avail}])"
                )
            }
            EditError::StoryNotFound { story, step_index } => {
                write!(f, "step {step_index}: {story} not found")
            }
            EditError::StoryBlockNotFound {
                story,
                block_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: block '{block_id}' not found in {story}"
            ),
            EditError::FindReplaceBarrierStraddle {
                block_id,
                needle,
                step_index,
            } => write!(
                f,
                "step {step_index}: find-replace needle '{needle}' straddles a barrier anchor \
                 in paragraph '{block_id}'; cannot replace across an opaque/field/hyperlink/break"
            ),
            EditError::BookmarkEmptyName { step_index } => write!(
                f,
                "step {step_index}: bookmark name is empty; a bookmark needs a \
                 non-empty name. Refusing to default it."
            ),
            EditError::BookmarkDuplicateName { name, step_index } => write!(
                f,
                "step {step_index}: bookmark name '{name}' is already used in the \
                 target paragraph; names must be unique"
            ),
            EditError::BookmarkNotFound { name, step_index } => write!(
                f,
                "step {step_index}: bookmark '{name}' not found in the target paragraph"
            ),
            EditError::BookmarkOrphanEnd { name, step_index } => write!(
                f,
                "step {step_index}: bookmark '{name}' has no paired bookmarkEnd in \
                 the target paragraph (multi-paragraph bookmarks are out of v1 scope); \
                 refusing partial removal"
            ),
            EditError::BookmarkRawXmlUnparsable => {
                write!(f, "bookmark decoration raw XML failed to parse")
            }
            EditError::StyleNotFound {
                block_id,
                style_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: style '{style_id}' for block '{block_id}' \
                 does not exist in word/styles.xml"
            ),
            EditError::NoStyleChangeRequested {
                block_id,
                style_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: block '{block_id}' already has style '{style_id}'; \
                 no style change to author"
            ),
            EditError::DrawingNotFound {
                drawing_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: drawing '{drawing_id}' not found in the target block"
            ),
            EditError::NotADrawing {
                drawing_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: inline '{drawing_id}' is not a drawing"
            ),
            EditError::DrawingMissingRawXml {
                drawing_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: drawing '{drawing_id}' has no raw_xml to edit"
            ),
            EditError::DrawingRawXmlParse {
                drawing_id,
                reason,
                step_index,
            } => write!(
                f,
                "step {step_index}: drawing '{drawing_id}' raw_xml failed to parse: {reason}"
            ),
            EditError::ImageAttributeTargetAbsent {
                drawing_id,
                attribute,
                step_index,
            } => write!(
                f,
                "step {step_index}: drawing '{drawing_id}' has no {attribute} element to edit; \
                 refusing to silently skip the requested change"
            ),
            EditError::NoImageAttributeRequested { step_index } => write!(
                f,
                "step {step_index}: set_image_attributes requested neither a resize nor an \
                 alt-text edit; refusing a no-op mutation"
            ),
            EditError::ImageLayoutRequiresAnchor {
                drawing_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: drawing '{drawing_id}' is inline; position/wrap require a \
                 floating (wp:anchor) drawing — inline⇄anchor conversion is out of scope"
            ),
            EditError::ImageLayoutTargetAbsent {
                drawing_id,
                target,
                step_index,
            } => write!(
                f,
                "step {step_index}: drawing '{drawing_id}' has no {target} element to edit; \
                 refusing to silently skip the requested change"
            ),
            EditError::NoImageLayoutRequested { step_index } => write!(
                f,
                "step {step_index}: set_image_layout requested no crop/position/wrap; \
                 refusing a no-op mutation"
            ),
            EditError::CommentTargetNotFound {
                comment_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: comment '{comment_id}' not found in the document"
            ),
            EditError::CommentAnchorNotFound {
                block_id,
                expected,
                actual_text,
                step_index,
            } => write!(
                f,
                "step {step_index}: comment anchor '{expected}' not found in the visible text \
                 of paragraph '{block_id}' (actual: '{actual_text}')"
            ),
            EditError::CommentAnchorOverlapsDeleted {
                block_id,
                expected,
                step_index,
            } => write!(
                f,
                "step {step_index}: comment anchor '{expected}' falls on text marked for \
                 deletion in paragraph '{block_id}'; a comment on struck text is ambiguous — \
                 target text that stays, comment before deleting, or resolve the deletion first"
            ),
            EditError::CommentOnTrackedBlock {
                block_id,
                status,
                step_index,
            } => write!(
                f,
                "step {step_index}: cannot comment on block '{block_id}': the whole block is \
                 tracked ('{status}') — comment on a paragraph without a pending block-level \
                 change, or resolve the change first (commenting a paragraph that only carries \
                 tracked segments IS allowed)"
            ),
            EditError::CommentEmptyBody { step_index } => write!(
                f,
                "step {step_index}: comment body is empty; a comment needs non-empty text. \
                 Refusing to default it."
            ),
            EditError::CommentRangeOrphaned {
                comment_id,
                missing_markers,
                step_index,
            } => write!(
                f,
                "step {step_index}: cannot delete comment '{comment_id}': missing anchor \
                 markers {missing_markers:?}; refusing a half-delete"
            ),
            EditError::CommentParentUnanchored {
                parent_comment_id,
                missing_markers,
                step_index,
            } => write!(
                f,
                "step {step_index}: cannot reply to comment '{parent_comment_id}': its anchor \
                 markers {missing_markers:?} are not present in the document, so the reply would \
                 have no span to attach to and would be invisible in Word — anchor the parent \
                 first, or reply to a comment that is anchored"
            ),
            EditError::NoteNotFound {
                note_id,
                note_kind,
                step_index,
            } => write!(
                f,
                "step {step_index}: {note_kind} '{note_id}' has no story in the document"
            ),
            EditError::NoteReferenceMissing {
                note_id,
                note_kind,
                step_index,
            } => write!(
                f,
                "step {step_index}: cannot delete {note_kind} '{note_id}': it has a story but no \
                 body reference run; refusing a half-delete"
            ),
            EditError::NoteAnchorNotAParagraph {
                block_id,
                actual_kind,
                step_index,
            } => write!(
                f,
                "step {step_index}: cannot insert a note into block '{block_id}': it is a \
                 {actual_kind}, not a paragraph"
            ),
            EditError::NoteEmptyBody { step_index } => write!(
                f,
                "step {step_index}: note body is empty; a note needs non-empty text. \
                 Refusing to default it."
            ),
            EditError::NoteIdNotNumeric {
                note_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: cannot allocate a sequential note id: existing note id \
                 '{note_id}' is non-numeric"
            ),
            EditError::NoteBodyMultiParagraph {
                note_id,
                note_kind,
                paragraph_count,
                step_index,
            } => write!(
                f,
                "step {step_index}: cannot tracked-edit {note_kind} '{note_id}': its body has \
                 {paragraph_count} paragraphs, but a tracked edit_note only supports a \
                 single-paragraph body; use mode:\"direct\" or edit the story manually"
            ),
            EditError::NoPageSetupRequested { step_index } => write!(
                f,
                "step {step_index}: set_page_setup requested no page-setup change \
                 (empty patch); refusing a no-op mutation"
            ),
            EditError::SectionPropertiesNotFound {
                block_id,
                step_index,
            } => match block_id {
                Some(id) => write!(
                    f,
                    "step {step_index}: paragraph '{id}' has no section break \
                     (w:sectPr) to edit"
                ),
                None => write!(
                    f,
                    "step {step_index}: the document has no body section properties \
                     (w:sectPr) to edit"
                ),
            },
            EditError::SectionAlreadyHasTrackedChange {
                block_id,
                step_index,
            } => match block_id {
                Some(id) => write!(
                    f,
                    "step {step_index}: section at '{id}' already carries a tracked \
                     change or section break; accept/reject it before editing"
                ),
                None => write!(
                    f,
                    "step {step_index}: the body section already carries a tracked \
                     w:sectPrChange; accept or reject it before editing"
                ),
            },
            EditError::NoHeaderFooterModeRequested { step_index } => write!(
                f,
                "step {step_index}: set_header_footer_mode requested no change \
                 (no title_page, even_and_odd, or link); refusing a no-op"
            ),
            EditError::HeaderFooterRefNotResolvable {
                is_header,
                kind,
                step_index,
            } => write!(
                f,
                "step {step_index}: cannot link a {} reference of kind '{kind}': no existing \
                 story to link (net-new-story creation is out of v1 scope)",
                if *is_header { "header" } else { "footer" }
            ),
            EditError::HeaderFooterAlreadyExists {
                is_header,
                kind,
                step_index,
            } => write!(
                f,
                "step {step_index}: a {} of kind '{kind}' already exists on this section; \
                 edit it (EditHeader/EditFooter) instead of creating a duplicate",
                if *is_header { "header" } else { "footer" }
            ),
            EditError::EquationXmlInvalid { reason, step_index } => write!(
                f,
                "step {step_index}: equation OMML fragment failed to parse: {reason}"
            ),
            EditError::EquationNotMath {
                actual_root,
                expected_root,
                step_index,
            } => write!(
                f,
                "step {step_index}: equation fragment root is '{actual_root}', \
                 expected '{expected_root}' for the requested placement"
            ),
            EditError::EmptyContentControlSpec { step_index } => write!(
                f,
                "step {step_index}: content-control spec has no distinguishing data \
                 (no tag, no alias, default rich-text control); refusing to author it"
            ),
            EditError::MalformedDataBinding { reason, step_index } => write!(
                f,
                "step {step_index}: content-control data binding is malformed ({reason}); \
                 refusing to author an unresolvable w:dataBinding"
            ),
            EditError::ContentControlBlockUnsupported { step_index } => write!(
                f,
                "step {step_index}: block-level content-control wrapping is deferred \
                 (v1 supports run-span wrapping only)"
            ),
            EditError::BlockRangeInvalid {
                start_block_id,
                end_block_id,
                reason,
                step_index,
            } => write!(
                f,
                "step {step_index}: invalid block range [{start_block_id} .. {end_block_id}]: {reason}"
            ),
            EditError::BlockAlreadyWrapped {
                block_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: block '{block_id}' is already wrapped in a block-level \
                 content control; refusing to nest an authored wrap"
            ),
            EditError::ContentControlNotFound { sdt_id, step_index } => write!(
                f,
                "step {step_index}: no content control with id '{sdt_id}' in the target block"
            ),
            EditError::NotAContentControl { sdt_id, step_index } => write!(
                f,
                "step {step_index}: inline '{sdt_id}' is not a content control (w:sdt)"
            ),
            EditError::ContentControlMissingRawXml { sdt_id, step_index } => write!(
                f,
                "step {step_index}: content control '{sdt_id}' has no raw XML to mutate"
            ),
            EditError::ContentControlRawXmlParse {
                sdt_id,
                reason,
                step_index,
            } => write!(
                f,
                "step {step_index}: content control '{sdt_id}' raw XML failed to parse: {reason}"
            ),
            EditError::ContentControlTypeMismatch {
                sdt_id,
                requested,
                actual,
                step_index,
            } => write!(
                f,
                "step {step_index}: content control '{sdt_id}' is a '{actual}' control; \
                 cannot set a '{requested}' value on it"
            ),
            EditError::FormFieldNotFound {
                field_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: no form-field opaque inline with id '{field_id}' in the target block"
            ),
            EditError::NotAFormField {
                field_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: '{field_id}' is not a fillable legacy form-field begin anchor \
                 (a fldSimple, a non-begin field part, or a non-field opaque has no ffData to set)"
            ),
            EditError::FormFieldMissingRawXml {
                field_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: form field '{field_id}' has no raw_xml — the field markup is missing"
            ),
            EditError::FormFieldRawXmlParse {
                field_id,
                reason,
                step_index,
            } => write!(
                f,
                "step {step_index}: could not parse form field '{field_id}' ffData: {reason}"
            ),
            EditError::FormFieldTypeMismatch {
                field_id,
                requested,
                actual,
                step_index,
            } => write!(
                f,
                "step {step_index}: form field '{field_id}' is a '{actual}' field; \
                 cannot set a '{requested}' value on it"
            ),
            EditError::FormFieldValueNotInList {
                field_id,
                value,
                step_index,
            } => write!(
                f,
                "step {step_index}: form field '{field_id}' has no list entry '{value}' \
                 — refusing to select a value that is not in the dropdown"
            ),
            EditError::MalformedFfData {
                field_id,
                reason,
                step_index,
            } => write!(
                f,
                "step {step_index}: form field '{field_id}' has malformed ffData: {reason}"
            ),
            EditError::FormFieldResultHasTrackedChanges {
                field_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: form field '{field_id}' result region carries a tracked change \
                 — refusing to overwrite a half-tracked result (resolve the revisions first)"
            ),
            EditError::TrackedContentControlSetUnsupported { sdt_id, step_index } => write!(
                f,
                "step {step_index}: tracked content-control set on '{sdt_id}' is not supported yet \
                 (the projector does not resolve revisions inside sdtContent) — use tracked: false"
            ),
            EditError::TextboxHasTrackedChanges {
                drawing_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: textbox '{drawing_id}' interior already carries tracked changes \
                 — resolve them first (accept/reject) before replacing the textbox text, rather \
                 than silently flattening the redlines"
            ),
            EditError::MultipleDistinctTextboxes {
                drawing_id,
                count,
                step_index,
            } => write!(
                f,
                "step {step_index}: drawing '{drawing_id}' has {count} txbxContent with different \
                 interiors (a multi-textbox group shape) — refusing to replace them all with one \
                 text; set_textbox_text v1 targets a single textbox (or its identical \
                 AlternateContent copies)"
            ),
            EditError::OpaqueTextTargetNotFound {
                opaque_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: opaque '{opaque_id}' is not a textbox or inline content control \
                 with editable text (body-level content controls use sdt_text_fill)"
            ),
            EditError::OpaqueTextRegionNotFound {
                opaque_id,
                container_index,
                paragraph_index,
                step_index,
            } => write!(
                f,
                "step {step_index}: opaque '{opaque_id}' has no text region at container \
                 {container_index}, paragraph {paragraph_index} — re-run opaque_text_targets"
            ),
            EditError::OpaqueTextMissingRawXml {
                opaque_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: opaque '{opaque_id}' carries no raw_xml to descend into"
            ),
            EditError::OpaqueTextRawXmlParse {
                opaque_id,
                reason,
                step_index,
            } => write!(
                f,
                "step {step_index}: opaque '{opaque_id}' raw_xml failed to parse: {reason}"
            ),
            EditError::OpaqueTextNotFound {
                opaque_id,
                find,
                step_index,
            } => write!(
                f,
                "step {step_index}: text '{find}' not found in opaque '{opaque_id}' region — \
                 refusing to splice a guessed location"
            ),
            EditError::OpaqueTextRegionHasTrackedChanges {
                opaque_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: opaque '{opaque_id}' region already carries tracked changes \
                 — resolve them first (accept/reject) before editing its text"
            ),
            EditError::OpaqueTextUnsupportedShape {
                opaque_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: the matched span in opaque '{opaque_id}' crosses a non-text \
                 element (tab/break/drawing/nested control) — out of the v1 surgical-splice scope"
            ),
            EditError::SdtFillAmbiguousTarget { step_index } => write!(
                f,
                "step {step_index}: sdt_text_fill needs exactly one target — an inline control \
                 (block_id + sdt_id) OR a block control (body_index), not neither/both"
            ),
            EditError::SdtFillBlockNotFound {
                body_index,
                step_index,
            } => write!(
                f,
                "step {step_index}: no body-level content control at body_index {body_index} \
                 — re-run opaque_text_targets"
            ),
            EditError::SdtFillEmpty { step_index } => write!(
                f,
                "step {step_index}: refusing an empty content-control fill (no value and nothing \
                 to replace) — a meaningless no-op"
            ),
            EditError::OpaqueTextMirrorDivergence {
                opaque_id,
                matched,
                spliced,
                step_index,
            } => write!(
                f,
                "step {step_index}: textbox '{opaque_id}' has {matched} Choice/Fallback copies \
                 matching the addressed interior but only {spliced} could be edited — refusing a \
                 partial mirror (the copies would diverge silently)"
            ),
            EditError::SdtFillBlockHashUnsupported {
                body_index,
                step_index,
            } => write!(
                f,
                "step {step_index}: semantic_hash is not supported for a BLOCK content-control \
                 fill (body_index {body_index}) — block discovery surfaces no hash to pin \
                 against; drop the field (refusing rather than silently ignoring a stale-edit \
                 guard)"
            ),
            EditError::SdtFillDuplicateBlockTarget {
                body_index,
                step_index,
            } => write!(
                f,
                "step {step_index}: a fill for body_index {body_index} is already staged in this \
                 transaction — one fill per block control per transaction"
            ),
            EditError::SdtFillComplexContent { sdt_id, step_index } => write!(
                f,
                "step {step_index}: content control '{sdt_id}' has text inside a hyperlink/field/\
                 nested control — not a cleanly fillable value; refusing rather than relocating it"
            ),
            EditError::ImageBytesEmpty { step_index } => write!(
                f,
                "step {step_index}: image has empty bytes — refusing to author an empty media part"
            ),
            EditError::UnsupportedImageFormat {
                declared,
                step_index,
            } => write!(
                f,
                "step {step_index}: image bytes do not match the declared format '{declared}' \
                 (magic-byte mismatch) — refusing to author a mislabeled media part"
            ),
            EditError::ImageAspectMismatch {
                drawing_id,
                intrinsic_w,
                intrinsic_h,
                requested_cx,
                requested_cy,
                step_index,
            } => write!(
                f,
                "step {step_index}: replace_image would stretch drawing '{drawing_id}': bytes are \
                 {intrinsic_w}x{intrinsic_h} but the requested extent {requested_cx}x{requested_cy} \
                 has a different aspect ratio — pass an extent matching the image aspect, or set \
                 allow_stretch: true to override"
            ),
            EditError::ImageHeaderUndecodable {
                drawing_id,
                format,
                len,
                step_index,
            } => write!(
                f,
                "step {step_index}: replace_image on drawing '{drawing_id}': the {format} bytes \
                 ({len} bytes) pass the magic check but their header could not be decoded to pixel \
                 dimensions — refusing to embed a corrupt/truncated image (not bypassable by \
                 allow_stretch)"
            ),
            EditError::StyleDefEmptyId { step_index } => write!(
                f,
                "step {step_index}: style definition has an empty styleId — a w:style needs a w:styleId"
            ),
            EditError::StyleDefEmptyName {
                style_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: style '{style_id}' has an empty name — a usable w:style needs a w:name"
            ),
            EditError::StyleDefIdMismatch {
                addressed,
                definition,
                step_index,
            } => write!(
                f,
                "step {step_index}: ModifyStyle addresses '{addressed}' but the definition's styleId \
                 is '{definition}' — refusing to splice a style under a mismatched id"
            ),
            EditError::DocDefaultsEmpty { step_index } => write!(
                f,
                "step {step_index}: set_doc_defaults set neither font_family nor \
                 font_size_half_points — refusing a no-op docDefaults edit"
            ),
            EditError::BlocksToTableNonParagraph {
                block_id,
                actual_kind,
                step_index,
            } => write!(
                f,
                "step {step_index}: blocks_to_table source block '{block_id}' is a {actual_kind}, \
                 not a paragraph — only a contiguous run of paragraphs can be converted to a table"
            ),
            EditError::BlocksToTableOpaqueInline {
                block_id,
                step_index,
            } => write!(
                f,
                "step {step_index}: blocks_to_table source paragraph '{block_id}' contains an opaque \
                 inline (drawing/field/hyperlink/etc.); the conversion projects only visible text \
                 into cells and would lose it — split that paragraph out of the range first"
            ),
            EditError::BlocksToTableSplitMismatch {
                block_id,
                expected_columns,
                actual_columns,
                text,
                step_index,
            } => write!(
                f,
                "step {step_index}: blocks_to_table source paragraph '{block_id}' split into \
                 {actual_columns} cell(s) but the table has {expected_columns} column(s) \
                 (text: {text:?}) — a ragged grid is refused"
            ),
            EditError::BlocksToTableEmptySpec { reason, step_index } => write!(
                f,
                "step {step_index}: blocks_to_table spec is invalid: {reason}"
            ),
            EditError::NoOpEdit {
                block_id,
                step_index,
                reason,
            } => write!(
                f,
                "step {step_index}: edit on paragraph '{}' has no effect ({reason}); \
                 an op that changes nothing is not applied — drop it or correct the content",
                block_id.0
            ),
            EditError::PrefixDuplicatesLabel {
                block_id,
                step_index,
                label,
                paragraph_label,
                current_text,
            } => {
                if label == paragraph_label {
                    write!(
                        f,
                        "step {step_index}: replacement content for paragraph '{}' begins \
                         with '{label}', which duplicates this paragraph's numbering label; \
                         it already reads '{current_text}' — omit the leading '{label}' (the \
                         numbering is already present)",
                        block_id.0
                    )
                } else {
                    write!(
                        f,
                        "step {step_index}: replacement content for paragraph '{}' begins \
                         with the label '{label}', but this paragraph's own numbering label \
                         is '{paragraph_label}' and is re-emitted automatically, so the \
                         result would read '{paragraph_label}{label}…'. Changing a \
                         paragraph's numbering label via text replace is not supported — \
                         omit the label and edit the body text only (it already reads \
                         '{current_text}')",
                        block_id.0
                    )
                }
            }
        }
    }
}

impl std::error::Error for EditError {}

// Markup parser (test builder) lives in `edit/markup.rs`.
mod markup;
pub use markup::{MarkupParseError, parse_paragraph_markup};

// Per-verb authoring logic. See `edit/AGENTS.md`.
pub(crate) mod verbs;

// Per-apply transient: OPC parts (media binaries, styles.xml fragments) a verb
// wants staged into the save path alongside its typed-IR mutation. Derived
// entirely from the transaction, never persisted (honors the EditSnapshot
// "do not persist" contract).
pub mod pending_parts;
pub use pending_parts::{CustomXmlPart, NumberingOp, PendingMedia, PendingParts, StyleOp};

// Story-targeting addressing grammar (body + footnote/endnote/comment stories).
pub(crate) mod story_addr;
pub use story_addr::StoryRef;

// Re-export the numbering grammar so callers can build
// `EditStep::SetParagraphNumbering` without reaching into the verb module path.
pub use verbs::numbering::NumberingChange;
pub use verbs::table_ops::{TableInsertPosition, TableOp};

// Re-export the note-family selector so callers can build
// `EditStep::InsertNote`/`EditNote`/`DeleteNote` without reaching into the verb
// module path (mirrors `NumberingChange`).
pub use verbs::footnotes::NoteKind;

// Re-export the page-setup grammar so callers can build `EditStep::SetPageSetup`
// / `SetSectionType` / `InsertSectionBreak` without reaching into the verb
// module path (mirrors `NumberingChange`).
pub use verbs::page_setup::{ColumnLayout, PageMargins, PageSetupPatch, PageSize, SectionTarget};

// Re-export the header/footer link grammar for `EditStep::SetHeaderFooterMode`.
pub use verbs::headers_footers::HeaderFooterLink;

// Re-export the find-replace planner. It is a PURE planner: it composes
// existing `EditStep::ReplaceParagraphText` steps (no new EditStep / Op /
// materializer change), so callers build a transaction from its output.
pub use verbs::find_replace::ReplaceTextError;
pub use verbs::find_replace::{
    BarrierPolicy, ExpectedMatches, FindReplaceOptions, FindReplaceScope, MatchMode, MatchSite,
    NormalizationClass, ReplaceTextOptions, ReplaceTextPlan, ReplaceTextScope, SkippedStraddle,
    UnreachedCellMatch, plan_find_replace_all, plan_replace_text, unreached_cell_matches,
};
// Re-export the image-resize grammar so callers can build
// `EditStep::SetImageAttributes` without reaching into the verb module path.
pub use verbs::content_controls::{DataBinding, SdtSpec, SdtValue};
pub use verbs::equations::EquationPlacement;
pub use verbs::form_fields::FormFieldValue;
pub use verbs::images::ImageResize;
// Re-export the image-layout grammar (crop / position / wrap) so callers can
// build `EditStep::SetImageLayout` without reaching into the verb module path.
pub use verbs::image_layout::{ImageCrop, ImageLayoutPatch, ImagePositionAxis, ImageWrapType};

// Re-export the image-insert grammar so callers can build
// `EditStep::InsertImage`/`ReplaceImage` without reaching into the verb module
// path (mirrors `ImageResize`).
pub use verbs::image_insert::{ImageFormat, ImageSource};

// Re-export the style-definition grammar so callers can build
// `EditStep::CreateStyle`/`ModifyStyle` without reaching into the verb module
// path (mirrors `NumberingChange`).
pub use verbs::style_defs::{StyleDefinition, StyleParaProps, StyleRunProps, StyleType};

// ─── Paragraph flattening ────────────────────────────────────────────────────

/// Information about a preserved inline anchor in the original paragraph.
#[derive(Clone, Debug)]
struct AnchorInfo {
    id: NodeId,
    kind: &'static str,
    /// Position in the original anchor sequence (0-based).
    order_index: usize,
}

/// Map an `OpaqueKind` to a stable, short, human-readable label. The
/// labels are used in the `OpaqueDestroyed` error's `missing_inline_kinds`
/// list and in the paragraph preview placeholder (e.g. `[footnote_ref]`).
/// Preserved as `&'static str` so the preview and the error both key off
/// the same constants — the LLM's retry prompt reads them as tokens.
pub(crate) fn opaque_kind_label(kind: &OpaqueKind) -> &'static str {
    match kind {
        OpaqueKind::Drawing => "drawing",
        OpaqueKind::SmartArt => "smart_art",
        OpaqueKind::Sdt => "sdt",
        OpaqueKind::Field(_) => "field",
        OpaqueKind::OmmlBlock => "math_block",
        OpaqueKind::OmmlInline => "math_inline",
        OpaqueKind::Ruby => "ruby",
        OpaqueKind::Hyperlink(_) => "hyperlink",
        OpaqueKind::CommentReference(_) => "comment_ref",
        OpaqueKind::FootnoteReference(_) => "footnote_ref",
        OpaqueKind::EndnoteReference(_) => "endnote_ref",
        OpaqueKind::SmartTag => "smart_tag",
        OpaqueKind::Sym(_) => "sym",
        OpaqueKind::Ptab => "ptab",
        OpaqueKind::CustomXml => "custom_xml",
        OpaqueKind::Unknown(_) => "unknown_opaque",
        OpaqueKind::QuarantinedNestedTracking => "quarantined_nested_tracked_changes",
    }
}

/// Collect the preserved inline anchors from a paragraph, in order.
fn collect_anchor_inventory(para: &ParagraphNode) -> Vec<AnchorInfo> {
    let mut anchors = Vec::new();
    for segment in &para.segments {
        for inline in &segment.inlines {
            match inline {
                InlineNode::OpaqueInline(opaque) => {
                    anchors.push(AnchorInfo {
                        id: opaque.id.clone(),
                        kind: opaque_kind_label(&opaque.kind),
                        order_index: anchors.len(),
                    });
                }
                InlineNode::HardBreak(hb) => {
                    anchors.push(AnchorInfo {
                        id: hb.id.clone(),
                        kind: "hard_break",
                        order_index: anchors.len(),
                    });
                }
                _ => {}
            }
        }
    }
    anchors
}

/// A paragraph segment contributes to the VISIBLE pending text — the projection
/// an `expect` precondition is checked against — iff it is not struck: `Normal`
/// or `Inserted`. `Deleted` and `InsertedThenDeleted` segments hold text that is
/// struck through in the pending state; it is not part of what the caller is
/// editing. A stale re-edit whose `expect` lands ONLY inside a `Deleted` segment
/// must refuse, not silently overwrite the pending change that struck it. Mirrors
/// `comments::segment_is_visible`.
fn segment_is_visible_pending(status: &TrackingStatus) -> bool {
    matches!(status, TrackingStatus::Normal | TrackingStatus::Inserted(_))
}

/// Extract the text of a paragraph, split at anchor boundaries into sections,
/// keeping only the segments `include` accepts. Returns one text section per gap
/// between anchors: for a paragraph with N (kept) anchors, N+1 sections
/// `[text before first anchor, ..., text after last anchor]`. Segments the
/// filter rejects contribute neither text nor anchor splits (a struck opaque
/// inline does not fragment the visible sections).
fn extract_text_sections_filtered(
    para: &ParagraphNode,
    include: impl Fn(&TrackingStatus) -> bool,
) -> Vec<String> {
    let mut sections = Vec::new();
    let mut current = String::new();

    for segment in &para.segments {
        if !include(&segment.status) {
            continue;
        }
        for inline in &segment.inlines {
            match inline {
                InlineNode::Text(t) => {
                    current.push_str(&t.text);
                }
                InlineNode::OpaqueInline(_) | InlineNode::HardBreak(_) => {
                    sections.push(std::mem::take(&mut current));
                }
                // Decorations and comment markers are zero-width; skip.
                _ => {}
            }
        }
    }
    sections.push(current);
    sections
}

/// Extract the text of a paragraph across ALL segments (Normal, Inserted,
/// Deleted, InsertedThenDeleted), split at anchor boundaries into sections. This
/// is the accept-all/redline reading (what the runs hold), used by no-op
/// identity detection and paths that have already refused pending tracked
/// segments. For the `expect`-precondition search space, use
/// `extract_visible_text_sections` — struck text is not part of the pending
/// state a caller edits.
fn extract_text_sections(para: &ParagraphNode) -> Vec<String> {
    extract_text_sections_filtered(para, |_| true)
}

/// Extract the VISIBLE PENDING text of a paragraph (Normal ∪ Inserted, never
/// struck), split at anchor boundaries. This is the search space every `expect`
/// precondition matches against.
fn extract_visible_text_sections(para: &ParagraphNode) -> Vec<String> {
    extract_text_sections_filtered(para, segment_is_visible_pending)
}

/// Get the full text of a paragraph across ALL segments (ignoring anchors). This
/// is the accept-all/redline reading; it includes struck (`Deleted`) text and is
/// used for user-facing "what the paragraph says" displays.
pub(crate) fn paragraph_visible_text(para: &ParagraphNode) -> String {
    let mut text = String::new();
    for segment in &para.segments {
        for inline in &segment.inlines {
            if let InlineNode::Text(t) = inline {
                text.push_str(&t.text);
            }
        }
    }
    text
}

/// Get the VISIBLE PENDING text of a paragraph (Normal ∪ Inserted, never struck;
/// ignoring anchors). The `expect`-precondition search view: struck text is not
/// part of the pending state a caller edits.
fn paragraph_visible_pending_text(para: &ParagraphNode) -> String {
    let mut text = String::new();
    for segment in &para.segments {
        if !segment_is_visible_pending(&segment.status) {
            continue;
        }
        for inline in &segment.inlines {
            if let InlineNode::Text(t) = inline {
                text.push_str(&t.text);
            }
        }
    }
    text
}

/// Build a short preview of the paragraph's visible text, with preserved
/// inlines rendered as kind-labeled placeholders (e.g. `[footnote_ref]`,
/// `[hyperlink]`) so the retry prompt and user-facing messages can see
/// what kind of anchor was there. The specific node IDs travel separately
/// in `missing_opaque_ids` on the `OpaqueDestroyed` error — the preview
/// is for orientation, not for addressing. This kind-labeled preview is part
/// of the typed validation-error wire shape. Capped at ~200 chars.
fn original_text_preview(para: &ParagraphNode) -> String {
    const PREVIEW_CAP: usize = 200;
    let mut out = String::new();
    for segment in &para.segments {
        for inline in &segment.inlines {
            match inline {
                InlineNode::Text(t) => out.push_str(&t.text),
                InlineNode::OpaqueInline(o) => {
                    out.push('[');
                    out.push_str(opaque_kind_label(&o.kind));
                    out.push(']');
                }
                InlineNode::HardBreak(_) => {
                    out.push_str("[hard_break]");
                }
                _ => {}
            }
            if out.len() >= PREVIEW_CAP {
                break;
            }
        }
        if out.len() >= PREVIEW_CAP {
            break;
        }
    }
    if out.len() > PREVIEW_CAP {
        // truncate on a char boundary
        let mut end = PREVIEW_CAP;
        while end > 0 && !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
        out.push('…');
    }
    out
}

/// Canonicalize punctuation that commonly differs between LLM-authored `expect`
/// strings and Word-authored document text.
///
/// LLMs tend to emit ASCII quotes, hyphens, three-dot ellipses; Word users
/// paste smart quotes, en/em dashes, and the single ellipsis glyph. A byte-exact
/// `contains` check fails on documents that round-tripped through Word even when
/// the human-visible text matches. We fold both sides to a canonical ASCII form
/// before comparing.
///
/// `expect` is a precondition-only check — the replacement uses the LLM's
/// full paragraph content, not a byte-range substitution. So the normalized
/// forms are only compared for containment; no byte-range mapping is needed.
fn normalize_expect_punctuation(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            // Single curly quotes, modifier apostrophe, reversed single quote,
            // backtick → ASCII apostrophe.
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' | '\u{02BC}' | '\u{0060}' => {
                out.push('\'');
            }
            // Double curly/low/high-reversed quotes → ASCII double quote.
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => out.push('"'),
            // Dashes/minus/horizontal bar → ASCII hyphen-minus.
            '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}' | '\u{2212}' => out.push('-'),
            // Single-glyph ellipsis → three ASCII dots.
            '\u{2026}' => out.push_str("..."),
            // Non-breaking, narrow no-break, and figure spaces → ASCII space.
            '\u{00A0}' | '\u{202F}' | '\u{2007}' => out.push(' '),
            other => out.push(other),
        }
    }
    out
}

/// THE expect-matching rule, stated once (CLAUDE.md "name the invariant once").
///
/// THE SEARCH SPACE IS THE VISIBLE PENDING TEXT, NOT THE REDLINE: `expect` is a
/// staleness guard over the state a caller is editing. That state is the pending
/// projection — `Normal` ∪ `Inserted` segments (see `segment_is_visible_pending`)
/// — never `Deleted`/`InsertedThenDeleted` text, which is already struck. Matching
/// inside `Inserted` segments is intentional: it lets a caller re-edit its own
/// pending insertion. Matching inside `Deleted` segments is a bug — a genuinely
/// stale re-edit whose `expect` survives only in the struck original would apply
/// and silently overwrite the pending change. So both the body-section
/// and the label-inclusive branches below read the VISIBLE projection
/// (`extract_visible_text_sections` / `paragraph_visible_pending_text`).
///
/// THE LABEL BELONGS TO THE PARAGRAPH, NOT TO CONTENT: a numbered paragraph's
/// enumeration label ("1.", "(a)") lives in the `literal_prefix` FIELD (its
/// canonical post-normalization home — import hoists it there, post-projection
/// re-hoist puts it back after every accept, the serializer re-emits it from
/// there). Every read surface re-prepends it, so an agent reads "1.\tEvents" and
/// may legitimately copy that into `expect`. The body runs, however, hold only
/// "Events". So an `expect` precondition matches when the (punctuation-
/// normalized) needle is a substring of EITHER:
///   - any body text section (the legacy form — what the runs hold), OR
///   - the label-inclusive read text "{label}\t{body}" (what the reader saw),
///     using the read view's literal_prefix-only label logic
///     (`crate::view::literal_prefix_label`; numPr numbering is Word-generated
///     and shown body-only, so it is NOT prepended here).
///
/// This accepts BOTH the label-inclusive and body-only forms with NO refusal on
/// the read-comparison side — it never adds a false match for a paragraph
/// without a label (the label-inclusive branch only runs when one exists), so
/// nothing regresses. (Writing the label as CONTENT is a different rule —
/// `PrefixDuplicatesLabel` — and is refused; this is purely the read/precondition
/// side.) Used by EVERY path that takes an `expect`: replace and delete.
fn expect_matches_paragraph(para: &ParagraphNode, expect: &str) -> bool {
    if expect.is_empty() {
        return true;
    }
    let expect_norm = normalize_expect_punctuation(expect);
    if extract_visible_text_sections(para)
        .iter()
        .any(|section| normalize_expect_punctuation(section).contains(&expect_norm))
    {
        return true;
    }
    if let Some(label) = crate::view::literal_prefix_label(para) {
        let with_label = format!("{label}\t{}", paragraph_visible_pending_text(para));
        return normalize_expect_punctuation(&with_label).contains(&expect_norm);
    }
    false
}

/// Char-count-preserving normalization of typographic punctuation.
///
/// Folds the curly/typographic variants Word produces (smart quotes, en/em
/// dashes, NBSP) to their ASCII canonical form so the structural diff inside
/// `apply_replace_paragraph_text` does not flag style-only differences when
/// the LLM emits ASCII versions of the same characters. The downstream
/// segment-reconstruction consumer reads `Equal` and `Delete` token chars by
/// count from the ORIGINAL text node, so unchanged typographic chars survive
/// in the kept output. Only `Insert` chunks (genuinely new content) carry the
/// normalized form forward — that is acceptable because there is no original
/// glyph to preserve at insertion sites.
///
/// Invariant: every substitution must be exactly one char in, one char out,
/// so `chars().count()` is unchanged. That is what makes
/// `OldTextCursor::consume_chars` safe to drive off the normalized diff.
/// Variants that would change char count (ellipsis U+2026 → "...") are NOT
/// included here even though they appear in `normalize_expect_punctuation` —
/// that function targets containment checks where length need not be preserved.
///
/// Backtick (U+0060) is also excluded: it is rare in legal source documents
/// and the existing fixture
/// `replace_accepts_ascii_apostrophe_against_backtick_in_doc` asserts the
/// LLM's ASCII apostrophe wins. Modifier-letter apostrophe (U+02BC) is
/// likewise excluded — both go through the `expect` precondition normalizer
/// for forgiveness but are not treated as preservation-worthy typography.
fn normalize_diff_punctuation(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        let mapped = match ch {
            // Single curly quotes, reversed single quote → ASCII apostrophe.
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            // Double curly/low/high-reversed quotes → ASCII double quote.
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            // Dashes/minus/horizontal bar → ASCII hyphen-minus.
            '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}' | '\u{2212}' => '-',
            // Non-breaking, narrow no-break, and figure spaces → ASCII space.
            '\u{00A0}' | '\u{202F}' | '\u{2007}' => ' ',
            other => other,
        };
        out.push(mapped);
    }
    debug_assert_eq!(
        out.chars().count(),
        s.chars().count(),
        "normalize_diff_punctuation must preserve char count"
    );
    out
}

// ─── Validation (Phase 1) ────────────────────────────────────────────────────

/// Find a block's index in `doc.blocks` by its block_id.
///
/// Top-level only. For paragraphs nested in table cells, use
/// `find_paragraph_path` instead.
pub(crate) fn find_block_index(blocks: &[TrackedBlock], block_id: &NodeId) -> Option<usize> {
    blocks
        .iter()
        .position(|tb| block_id_of(&tb.block) == block_id)
}

/// Get the block_id of a BlockNode.
fn block_id_of(block: &BlockNode) -> &NodeId {
    match block {
        BlockNode::Paragraph(p) => &p.id,
        BlockNode::Table(t) => &t.id,
        BlockNode::OpaqueBlock(o) => &o.id,
    }
}

/// Location of a paragraph that the edit engine can address.
///
/// Paragraphs may live directly in `doc.blocks` (top-level) or inside one or
/// more nested table cells. The `descent` is a sequence of (row, cell, block)
/// indices that, applied in order, navigates from `doc.blocks[top_block]`
/// into the target paragraph. An empty `descent` means the paragraph is the
/// top-level block itself.
#[derive(Clone, Debug)]
pub(crate) struct ParagraphPath {
    pub top_block: usize,
    pub descent: Vec<CellStep>,
}

/// One descent step into a table cell.
#[derive(Clone, Debug)]
pub(crate) struct CellStep {
    pub row_idx: usize,
    pub cell_idx: usize,
    pub block_in_cell_idx: usize,
}

impl ParagraphPath {
    fn is_top_level(&self) -> bool {
        self.descent.is_empty()
    }
}

/// Find the path to a paragraph anywhere in the document (top-level or in a
/// table cell, including nested tables).
///
/// Returns `None` if the block_id is not a paragraph anywhere in the doc, or
/// if it resolves to a non-paragraph (a table or opaque block) — callers
/// receive that distinction by reading the block at the path themselves.
///
/// Search strategy: walk `doc.blocks`. If a top-level block matches, return
/// it as `descent: []`. Otherwise, for each top-level `Table`, recurse into
/// every cell looking for the paragraph (or a nested table containing it).
pub(crate) fn find_paragraph_path(doc: &CanonDoc, block_id: &NodeId) -> Option<ParagraphPath> {
    for (top_idx, tb) in doc.blocks.iter().enumerate() {
        if block_id_of(&tb.block) == block_id {
            return Some(ParagraphPath {
                top_block: top_idx,
                descent: Vec::new(),
            });
        }
        if let BlockNode::Table(table) = &tb.block
            && let Some(descent) = find_paragraph_in_table(table, block_id)
        {
            return Some(ParagraphPath {
                top_block: top_idx,
                descent,
            });
        }
    }
    None
}

fn find_paragraph_in_table(
    table: &crate::domain::TableNode,
    block_id: &NodeId,
) -> Option<Vec<CellStep>> {
    for (row_idx, row) in table.rows.iter().enumerate() {
        for (cell_idx, cell) in row.cells.iter().enumerate() {
            for (block_in_cell_idx, block) in cell.blocks.iter().enumerate() {
                if block_id_of(block) == block_id {
                    return Some(vec![CellStep {
                        row_idx,
                        cell_idx,
                        block_in_cell_idx,
                    }]);
                }
                if let BlockNode::Table(nested) = block
                    && let Some(mut deeper) = find_paragraph_in_table(nested, block_id)
                {
                    deeper.insert(
                        0,
                        CellStep {
                            row_idx,
                            cell_idx,
                            block_in_cell_idx,
                        },
                    );
                    return Some(deeper);
                }
            }
        }
    }
    None
}

/// Resolve a `ParagraphPath` to the BlockNode it points at (read-only).
pub(crate) fn block_at<'a>(doc: &'a CanonDoc, path: &ParagraphPath) -> &'a BlockNode {
    let mut block = &doc.blocks[path.top_block].block;
    for step in &path.descent {
        let table = match block {
            BlockNode::Table(t) => t,
            _ => panic!("ParagraphPath descent through non-table block (invariant violation)"),
        };
        block = &table.rows[step.row_idx].cells[step.cell_idx].blocks[step.block_in_cell_idx];
    }
    block
}

/// Resolve a `ParagraphPath` to the BlockNode it points at (mutable).
pub(crate) fn block_at_mut<'a>(doc: &'a mut CanonDoc, path: &ParagraphPath) -> &'a mut BlockNode {
    let mut block = &mut doc.blocks[path.top_block].block;
    for step in &path.descent {
        let table = match block {
            BlockNode::Table(t) => t,
            _ => panic!("ParagraphPath descent through non-table block (invariant violation)"),
        };
        block = &mut table.rows[step.row_idx].cells[step.cell_idx].blocks[step.block_in_cell_idx];
    }
    block
}

/// Check that the path leads through table rows and cells with no
/// (non-Normal) tracking status. The MVP edit engine refuses to edit a
/// paragraph inside a tracked-inserted or tracked-deleted row or cell — the
/// user must accept or reject the surrounding row/cell change first.
///
/// Returns `BlockHasTrackedStatus` keyed to `block_id` on the first
/// non-Normal ancestor encountered.
fn check_ancestor_table_tracking(
    doc: &CanonDoc,
    path: &ParagraphPath,
    block_id: &NodeId,
    step_index: usize,
) -> Result<(), EditError> {
    // The enclosing top-level block (the table holding this cell paragraph) may
    // itself be tracked-inserted/deleted — e.g. the Deleted shadow left by a
    // tracked table move. The top-level status is checked directly only for
    // top-level paragraphs; for a cell paragraph it is reached solely here, so
    // editing content inside a deleted/inserted table would otherwise slip
    // through and (for the deleted shadow) vanish on accept (P0 #3).
    if let Some(label) = tracking_status_label(&doc.blocks[path.top_block].status) {
        return Err(EditError::BlockHasTrackedStatus {
            block_id: block_id.clone(),
            status: label,
            step_index,
        });
    }

    let mut block = &doc.blocks[path.top_block].block;
    for step in &path.descent {
        let table = match block {
            BlockNode::Table(t) => t,
            _ => panic!("ParagraphPath descent through non-table block (invariant violation)"),
        };
        let row = &table.rows[step.row_idx];
        if let Some(status) = &row.tracking_status
            && let Some(label) = tracking_status_label(status)
        {
            return Err(EditError::BlockHasTrackedStatus {
                block_id: block_id.clone(),
                status: label,
                step_index,
            });
        }
        let cell = &row.cells[step.cell_idx];
        if let Some(status) = &cell.tracking_status
            && let Some(label) = tracking_status_label(status)
        {
            return Err(EditError::BlockHasTrackedStatus {
                block_id: block_id.clone(),
                status: label,
                step_index,
            });
        }
        block = &cell.blocks[step.block_in_cell_idx];
    }
    Ok(())
}

fn tracking_status_label(status: &TrackingStatus) -> Option<&'static str> {
    match status {
        TrackingStatus::Normal => None,
        TrackingStatus::Inserted(_) => Some("inserted"),
        TrackingStatus::Deleted(_) => Some("deleted"),
        TrackingStatus::InsertedThenDeleted(_) => Some("inserted_then_deleted"),
    }
}

/// Record that a guarded step's advisory `expect` substring did not match the
/// current block, after the authoritative block guard already proved freshness.
///
/// This is a DEPRECATION marker: under the unified-guard contract the `expect`
/// substring is advisory (the guard is the staleness check), so a miss must NOT
/// fail the step. We emit a `tracing` event so a caller wiring up diagnostics can
/// see the drift, but the flow continues — the block guard matched, which is the
/// real precondition.
fn advisory_expect_miss(block_id: &NodeId, expect: &str, step_index: usize) {
    tracing::debug!(
        target: "stemma::edit::guard",
        %block_id,
        step_index,
        expect,
        "advisory `expect` substring did not match, but the block guard matched; \
         applying anyway (expect is advisory under the unified-guard contract; \
         the literal-substring expect mechanism is deprecated)"
    );
}

/// Phase 1: Validate preconditions for a ReplaceParagraphText step.
///
/// Returns a `ParagraphPath` to the target paragraph on success. The path
/// may point at a top-level block or at a paragraph inside one or more
/// nested table cells.
fn validate_replace_step(
    doc: &CanonDoc,
    block_id: &NodeId,
    expect: &str,
    semantic_hash: Option<&str>,
    content: &ParagraphContent,
    step_index: usize,
) -> Result<ParagraphPath, EditError> {
    // 1. Find the paragraph (top-level or in a table cell).
    let path = find_paragraph_path(doc, block_id).ok_or_else(|| EditError::BlockNotFound {
        block_id: block_id.clone(),
        step_index,
    })?;

    // 2. Check tracking status of the block itself (top-level) and of any
    //    enclosing table rows/cells. The MVP rule is the same in either
    //    location: the block being edited must be Normal, and no ancestor
    //    row or cell may be tracked-inserted/deleted.
    if path.is_top_level() {
        let tracked_block = &doc.blocks[path.top_block];
        match &tracked_block.status {
            // Normal and Inserted are editable: prep (project_tracked_block_for_
            // direct_edit) resolves an Inserted block to Normal right after this
            // validation, so re-editing a freshly-inserted paragraph is allowed —
            // and validation now runs BEFORE prep (so the guard checks the redlined
            // block the client saw). Deleted / InsertedThenDeleted stay rejected,
            // matching prep, which refuses them.
            TrackingStatus::Normal | TrackingStatus::Inserted(_) => {}
            TrackingStatus::Deleted(_) => {
                return Err(EditError::BlockHasTrackedStatus {
                    block_id: block_id.clone(),
                    status: "deleted",
                    step_index,
                });
            }
            TrackingStatus::InsertedThenDeleted(_) => {
                return Err(EditError::BlockHasTrackedStatus {
                    block_id: block_id.clone(),
                    status: "inserted_then_deleted",
                    step_index,
                });
            }
        }
    } else {
        check_ancestor_table_tracking(doc, &path, block_id, step_index)?;
    }

    // 3. Check block kind
    let block = block_at(doc, &path);
    let para = match block {
        BlockNode::Paragraph(p) => p,
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
    };

    // 4. Existing tracked segments are NOT rejected here: every caller runs
    //    `prepare_paragraph_for_direct_edit` (flatten tracked ins/del to a Normal
    //    base) before the diff, so re-editing a paragraph that already carries a
    //    tracked change is allowed (B1) — the new change layers via flatten-then-
    //    diff. The staleness guard below still authoritatively gates the edit
    //    against the block the client actually saw (projected, pre-flatten).

    // ── Staleness gate (unified-guard contract) ──
    //
    // The block `guard` (semantic hash) is the AUTHORITATIVE staleness check —
    // the block-level staleness guard: the precondition and the staleness check
    // are the same object (a hash of the target block, so a stale edit against
    // changed content is refused).
    //
    // - guard present + matches  → the block is fresh; the op applies. A stale
    //   or changed `expect` substring does NOT fail — it is downgraded to an
    //   advisory diagnostic only (the guard already proved freshness).
    // - guard present + mismatches → fail loud (`BlockSemanticHashMismatch`).
    // - guard absent → fall back to the legacy `expect` gate (so pre-Phase-3
    //   callers that only sent `expect` keep working byte-identically).
    //
    // This is the contract change: with a matching guard, `expect` is advisory;
    // with a mismatching guard, the op fails; with neither guard nor a found
    // `expect`, today's expect-gated behavior is preserved.
    // `expect` is matched per the shared prefix-aware rule (see
    // `expect_matches_paragraph`): the punctuation-normalized needle must be a
    // substring of a body section OR the label-inclusive read text, so an agent's
    // faithful copy of "1.\tEvents" is accepted.
    let expect_section_match =
        |para: &ParagraphNode| -> bool { expect_matches_paragraph(para, expect) };

    match semantic_hash {
        Some(expected_hash) => {
            if let Err(actual) = check_block_guard(block, expected_hash) {
                return Err(EditError::BlockSemanticHashMismatch {
                    block_id: block_id.clone(),
                    expected: expected_hash.to_string(),
                    actual,
                    step_index,
                });
            }
            // Guard passed: `expect` is advisory. A miss is recorded as a
            // diagnostic but never fails the step (DEPRECATED dual-gate).
            if !expect_section_match(para) {
                advisory_expect_miss(block_id, expect, step_index);
            }
        }
        None => {
            // No guard supplied — legacy expect gate is authoritative.
            if !expect_section_match(para) {
                return Err(EditError::ExpectMismatch {
                    block_id: block_id.clone(),
                    expected: expect.to_string(),
                    actual_text: paragraph_visible_text(para),
                    step_index,
                });
            }
        }
    }

    // 5. Collect preserved inline inventory
    let anchors = collect_anchor_inventory(para);

    // 7. Validate preserved inline references
    validate_preserved_inlines(para, block_id, content, &anchors, step_index)?;

    Ok(path)
}

/// Validate that the replacement content correctly references all preserved
/// inlines from the original paragraph.
fn validate_preserved_inlines(
    para: &ParagraphNode,
    block_id: &NodeId,
    content: &ParagraphContent,
    anchors: &[AnchorInfo],
    step_index: usize,
) -> Result<(), EditError> {
    // Build a lookup from NodeId -> AnchorInfo for the original paragraph
    let anchor_map: std::collections::HashMap<&NodeId, &AnchorInfo> =
        anchors.iter().map(|a| (&a.id, a)).collect();

    // Also build a set of all inline NodeIds in the paragraph (for the
    // "not a preserved inline" error — references to text/decoration nodes)
    let all_inline_ids: std::collections::HashMap<&NodeId, &'static str> = para
        .segments
        .iter()
        .flat_map(|s| s.inlines.iter())
        .filter_map(|inline| match inline {
            InlineNode::Text(t) => Some((&t.id, "text")),
            InlineNode::Decoration(d) => Some((&d.id, "decoration")),
            _ => None, // anchors are already in anchor_map
        })
        .collect();

    // Walk the replacement content and collect referenced anchor IDs
    let mut referenced_ids: Vec<&NodeId> = Vec::new();
    let mut seen: std::collections::HashSet<&NodeId> = std::collections::HashSet::new();

    for fragment in &content.fragments {
        if let ContentFragment::PreservedInlineRef(id) = fragment {
            // Check: does this ID exist as a preserved inline?
            if !anchor_map.contains_key(id) {
                // Maybe it's a text or decoration node?
                if let Some(kind) = all_inline_ids.get(id) {
                    return Err(EditError::NotAPreservedInline {
                        block_id: block_id.clone(),
                        referenced_id: id.clone(),
                        actual_kind: kind,
                        step_index,
                    });
                }
                return Err(EditError::PreservedInlineNotFound {
                    block_id: block_id.clone(),
                    referenced_id: id.clone(),
                    step_index,
                });
            }

            // Check: duplicate?
            if !seen.insert(id) {
                return Err(EditError::DuplicatePreservedInlineRef {
                    block_id: block_id.clone(),
                    inline_id: id.clone(),
                    step_index,
                });
            }

            referenced_ids.push(id);
        }
    }

    // Check: every anchor from the inventory must be present. We collect
    // ALL missing anchors (not just the first) so retry flows can fix
    // everything in one pass. The spec's `missing_opaque_ids` is plural
    // for exactly this reason.
    let missing: Vec<&AnchorInfo> = anchors.iter().filter(|a| !seen.contains(&a.id)).collect();
    if !missing.is_empty() {
        let missing_opaque_ids: Vec<String> = missing.iter().map(|a| a.id.to_string()).collect();
        let missing_inline_kinds: Vec<&'static str> = missing.iter().map(|a| a.kind).collect();
        return Err(EditError::OpaqueDestroyed {
            step_index,
            target_block_id: block_id.clone(),
            missing_opaque_ids,
            missing_inline_kinds,
            original_text_preview: original_text_preview(para),
        });
    }

    // Check: order must match original
    let referenced_order: Vec<usize> = referenced_ids
        .iter()
        .map(|id| anchor_map[id].order_index)
        .collect();
    for pair in referenced_order.windows(2) {
        if pair[0] >= pair[1] {
            return Err(EditError::PreservedInlineOrderChanged {
                block_id: block_id.clone(),
                step_index,
            });
        }
    }

    Ok(())
}

fn normalize_block_range_indices(
    doc: &CanonDoc,
    from_block_id: &NodeId,
    to_block_id: &NodeId,
    step_index: usize,
) -> Result<(usize, usize), EditError> {
    let from_idx =
        find_block_index(&doc.blocks, from_block_id).ok_or_else(|| EditError::BlockNotFound {
            block_id: from_block_id.clone(),
            step_index,
        })?;
    let to_idx =
        find_block_index(&doc.blocks, to_block_id).ok_or_else(|| EditError::BlockNotFound {
            block_id: to_block_id.clone(),
            step_index,
        })?;

    if from_idx <= to_idx {
        Ok((from_idx, to_idx))
    } else {
        Ok((to_idx, from_idx))
    }
}

pub(crate) fn validate_block_is_editable(
    block: &TrackedBlock,
    step_index: usize,
) -> Result<(), EditError> {
    let block_id = block_id_of(&block.block).clone();
    match &block.status {
        TrackingStatus::Normal => {}
        TrackingStatus::Inserted(_) => {
            return Err(EditError::BlockHasTrackedStatus {
                block_id,
                status: "inserted",
                step_index,
            });
        }
        TrackingStatus::Deleted(_) => {
            return Err(EditError::BlockHasTrackedStatus {
                block_id,
                status: "deleted",
                step_index,
            });
        }
        TrackingStatus::InsertedThenDeleted(_) => {
            return Err(EditError::BlockHasTrackedStatus {
                block_id,
                status: "inserted_then_deleted",
                step_index,
            });
        }
    }

    if let BlockNode::Paragraph(p) = &block.block {
        for segment in &p.segments {
            if segment.status != TrackingStatus::Normal {
                return Err(EditError::ParagraphContainsTrackedSegments {
                    block_id,
                    step_index,
                });
            }
        }
    }

    Ok(())
}

fn project_tracked_block_for_direct_edit(
    tracked_block: &mut TrackedBlock,
    step_index: usize,
) -> Result<(), EditError> {
    let block_id = block_id_of(&tracked_block.block).clone();

    match &tracked_block.status {
        TrackingStatus::Normal | TrackingStatus::Inserted(_) => {}
        TrackingStatus::InsertedThenDeleted(_) => {
            // Pending-deleted: cannot re-base a direct edit on a block whose
            // existence is itself contested.
            return Err(EditError::BlockHasTrackedStatus {
                block_id,
                status: "inserted_then_deleted",
                step_index,
            });
        }
        TrackingStatus::Deleted(_) => {
            return Err(EditError::BlockHasTrackedStatus {
                block_id,
                status: "deleted",
                step_index,
            });
        }
    }

    tracked_block.status = TrackingStatus::Normal;
    tracked_block.move_id = None;
    // Flatten pre-existing tracked ins/del TEXT so the upcoming text edit diffs
    // against a clean base, but PRESERVE any recorded `*PrChange` formatting
    // change: accepting it here would break tracked-change reversibility
    // (ECMA-376 §17.13.5.29) — `reject_all` could no longer restore the prior
    // formatting. See `project_block_for_text_edit_prep`.
    project_block_for_text_edit_prep(&mut tracked_block.block);
    Ok(())
}

/// Resolves any existing tracked changes on the target paragraph (in-place
/// accept) so the edit engine runs on a Normal slate.
///
/// For top-level blocks: clears block-level `TrackingStatus::Inserted` and
/// projects segments via `project_tracked_block_for_direct_edit`.
///
/// For paragraphs inside table cells: only the paragraph's segments are
/// resolved. Enclosing row/cell tracking status is validated as Normal
/// first (see `check_ancestor_table_tracking`); editing inside a tracked
/// row/cell is rejected.
fn prepare_paragraph_for_direct_edit(
    doc: &mut CanonDoc,
    block_id: &NodeId,
    step_index: usize,
) -> Result<ParagraphPath, EditError> {
    let path = find_paragraph_path(doc, block_id).ok_or_else(|| EditError::BlockNotFound {
        block_id: block_id.clone(),
        step_index,
    })?;
    if path.is_top_level() {
        project_tracked_block_for_direct_edit(&mut doc.blocks[path.top_block], step_index)?;
    } else {
        check_ancestor_table_tracking(doc, &path, block_id, step_index)?;
        let block = block_at_mut(doc, &path);
        // Same as the top-level path: flatten tracked text, preserve the
        // paragraph's `formatting_change` so a prior pPrChange stays reversible.
        project_block_for_text_edit_prep(block);
    }
    Ok(path)
}

fn prepare_block_range_for_direct_edit(
    doc: &mut CanonDoc,
    from_block_id: &NodeId,
    to_block_id: &NodeId,
    step_index: usize,
) -> Result<(usize, usize), EditError> {
    let (start, end) = normalize_block_range_indices(doc, from_block_id, to_block_id, step_index)?;
    for tracked_block in &mut doc.blocks[start..=end] {
        project_tracked_block_for_direct_edit(tracked_block, step_index)?;
    }
    Ok((start, end))
}

fn validate_expect_on_block(
    doc: &CanonDoc,
    block_id: &NodeId,
    expect: &str,
    semantic_hash: Option<&str>,
    step_index: usize,
) -> Result<(), EditError> {
    let block_idx =
        find_block_index(&doc.blocks, block_id).ok_or_else(|| EditError::BlockNotFound {
            block_id: block_id.clone(),
            step_index,
        })?;
    let tracked_block = &doc.blocks[block_idx];
    validate_block_is_editable(tracked_block, step_index)?;

    let para = match &tracked_block.block {
        BlockNode::Paragraph(p) => p,
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
    };

    // Unified-guard contract (same as `validate_replace_step`): the block guard
    // is the authoritative staleness check; `expect` is advisory when a guard is
    // supplied, and the legacy gate only when no guard is supplied. `expect` is
    // matched by the SAME prefix-aware rule as replace (`expect_matches_paragraph`)
    // so a delete that copies a numbered paragraph's read text ("1.\tEvents") into
    // `expect` is accepted — the label lives in `literal_prefix`, not the runs.
    let expect_found = || expect_matches_paragraph(para, expect);

    match semantic_hash {
        Some(expected_hash) => {
            if let Err(actual) = check_block_guard(&tracked_block.block, expected_hash) {
                return Err(EditError::BlockSemanticHashMismatch {
                    block_id: block_id.clone(),
                    expected: expected_hash.to_string(),
                    actual,
                    step_index,
                });
            }
            if !expect_found() {
                advisory_expect_miss(block_id, expect, step_index);
            }
        }
        None => {
            if !expect_found() {
                return Err(EditError::ExpectMismatch {
                    block_id: block_id.clone(),
                    expected: expect.to_string(),
                    actual_text: paragraph_visible_text(para),
                    step_index,
                });
            }
        }
    }

    Ok(())
}

/// Validate `MoveBlockRange`'s OPTIONAL `expect`/`semantic_hash` precondition
/// against the `from` block — same target as `DeleteBlockRange`'s (always
/// required) precondition, via `validate_expect_on_block`, but move's guard
/// is opt-in so a caller with neither field set gets today's ungated move.
///
/// `semantic_hash` with no `expect` is a real combination `validate_expect_on_block`
/// does not support (it always needs an `expect` string, even to demote it to
/// advisory) — handled here directly via `check_block_guard`, mirroring the
/// hash-only precondition pattern used elsewhere (e.g. table-replace).
fn validate_move_expect(
    doc: &CanonDoc,
    from_block_id: &NodeId,
    expect: Option<&str>,
    semantic_hash: Option<&str>,
    step_index: usize,
) -> Result<(), EditError> {
    let Some(expect) = expect else {
        let Some(expected) = semantic_hash else {
            return Ok(());
        };
        let idx = find_block_index(&doc.blocks, from_block_id).ok_or_else(|| {
            EditError::BlockNotFound {
                block_id: from_block_id.clone(),
                step_index,
            }
        })?;
        return match check_block_guard(&doc.blocks[idx].block, expected) {
            Ok(()) => Ok(()),
            Err(actual) => Err(EditError::BlockSemanticHashMismatch {
                block_id: from_block_id.clone(),
                expected: expected.to_string(),
                actual,
                step_index,
            }),
        };
    };
    validate_expect_on_block(doc, from_block_id, expect, semantic_hash, step_index)
}

fn validate_delete_step(
    doc: &CanonDoc,
    from_block_id: &NodeId,
    to_block_id: &NodeId,
    expect: &str,
    semantic_hash: Option<&str>,
    step_index: usize,
) -> Result<(usize, usize), EditError> {
    validate_expect_on_block(doc, from_block_id, expect, semantic_hash, step_index)?;
    let (start, end) = normalize_block_range_indices(doc, from_block_id, to_block_id, step_index)?;
    for tracked_block in &doc.blocks[start..=end] {
        validate_block_is_editable(tracked_block, step_index)?;
    }
    Ok((start, end))
}

// ─── Phase 0.5: Defensive numbering-prefix strip ────────────────────────────

/// Return the "materialized numbering prefix" string for a paragraph — the
/// exact byte sequence that currently appears at the start of the paragraph's
/// visible text because the import path materialized the list-generated
/// numbering into the run text.
///
/// Source of truth is the paragraph's `literal_prefix` (when the typed prefix
/// detector stripped a prefix from the inlines and stashed it here), or the
/// `synthesized_text` on `numbering` / `materialized_numbering` (when the
/// prefix came from structural list numbering).
fn materialized_numbering_prefix(para: &ParagraphNode) -> Option<&str> {
    if let Some(lp) = para.literal_prefix.as_deref() {
        return Some(lp);
    }
    if let Some(n) = &para.numbering {
        return Some(&n.synthesized_text);
    }
    if let Some(n) = &para.materialized_numbering {
        return Some(&n.synthesized_text);
    }
    None
}

/// Detect when a replacement's leading text would DOUBLE the target paragraph's
/// numbering label. Returns the leading label the content carries (the one that
/// would be doubled) when the guard should fire, else `None`.
///
/// Scenario: the target paragraph carries a numbering label — hoisted into
/// `literal_prefix` (a typed-in `"1."`, `"(a)"` the serializer re-emits) or
/// generated from `numbering`/`materialized_numbering`. The read view presents
/// the label re-prepended to the body, so the agent sees `"1.\tEvents"`. The
/// serializer re-emits the existing label, so ANY enumeration label at the head
/// of the replacement content renders the number twice:
///   - the SAME label → `"1.1.\tEvents"` (the literal echo case);
///   - a DIFFERENT label (e.g. `"2.\t…"` to renumber) → `"1.2.\tEvents"` — also
///     corruption, because the paragraph's own `"1."` label is untouched and
///     re-emitted (changing the number is the `set_numbering` renumber verb's
///     job, not a literal label typed into the body).
///
/// So the guard fires whenever the paragraph HAS a non-empty label AND the
/// leading content text begins with an enumeration label — its exact own label,
/// OR any label the prefix detector recognizes (`match_prefix_pattern`, the same
/// recognizer import uses to hoist labels). Format-agnostic: Arabic, Roman,
/// alpha, `(a)`, `§2.1` are all recognized as strings, never parsed for meaning.
/// The caller refuses with `EditError::PrefixDuplicatesLabel` rather than
/// silently stripping (a no-trace partial modification). Returns `None` when the
/// paragraph has no label, or the content carries no leading label (the agent
/// omitted it — the desired case).
/// Returns `(content_label, paragraph_label)` when the guard should fire: the
/// label the content begins with, and the paragraph's own label it would stack
/// onto. Equal for a verbatim echo; different for an attempted in-text renumber.
fn numbering_label_duplicated_by(
    para: &ParagraphNode,
    leading_text: &str,
) -> Option<(String, String)> {
    let prefix = materialized_numbering_prefix(para)?;
    if prefix.is_empty() {
        return None;
    }
    // Same label echoed verbatim.
    if leading_text.starts_with(prefix) {
        return Some((prefix.to_string(), prefix.to_string()));
    }
    // A DIFFERENT enumeration label at the head — also doubles, because the
    // paragraph's own label survives and is re-emitted. Detect it with the same
    // recognizer import uses (after any leading whitespace, matching the
    // detector's behavior).
    let trimmed = leading_text.trim_start_matches([' ', '\t']);
    crate::import::match_prefix_pattern(trimmed).map(|(label, _)| (label, prefix.to_string()))
}

/// The paragraph's current visible text WITH its numbering label re-prepended,
/// exactly as a reader sees it — used in the `PrefixDuplicatesLabel` message so
/// the refusal shows what the paragraph already reads (e.g. `"1.\tEvents"`). A
/// single tab joins the label and body, matching the read-view re-prepend.
fn paragraph_text_with_label(para: &ParagraphNode) -> String {
    let body = paragraph_visible_text(para);
    match materialized_numbering_prefix(para) {
        Some(label) if !label.is_empty() => format!("{label}\t{body}"),
        _ => body,
    }
}

// ─── Phase 0: Identity check ────────────────────────────────────────────────

/// Check whether the replacement content is semantically identical to the
/// original paragraph content. If so, the paragraph would be left untouched
/// and the caller surfaces an [`EditError::NoOpEdit`] rather than reporting a
/// no-op as success.
///
/// Identity means the rebuilt paragraph would be byte-identical: same visible
/// text per section, same preserved-inline anchors in the same order, AND —
/// for styled (`<bold>` etc.) replacements — the same run **marks**. The mark
/// check matters because a `StyledText` fragment can carry mark intent that the
/// plain text comparison alone would miss (e.g. replacing plain "Events" with
/// bold "Events" is a real edit, not a no-op). A plain `Text` fragment carries
/// no mark intent — it inherits the original run's formatting (see
/// [`build_text_node_from_exemplar`]) — so text+anchor identity is sufficient
/// there.
fn is_identity_replacement(para: &ParagraphNode, content: &ParagraphContent) -> bool {
    // Build what the "new" text sections and anchor sequence look like
    let mut new_sections: Vec<String> = Vec::new();
    let mut current_text = String::new();
    let mut new_anchor_ids: Vec<&NodeId> = Vec::new();

    for fragment in &content.fragments {
        match fragment {
            ContentFragment::Text(t) => current_text.push_str(t),
            ContentFragment::StyledText { text, .. } => current_text.push_str(text),
            ContentFragment::PreservedInlineRef(id) => {
                new_sections.push(std::mem::take(&mut current_text));
                new_anchor_ids.push(id);
            }
            // A NewHyperlink fragment means "create something the old
            // paragraph didn't have" — by construction it's not an
            // identity replacement. Bail out early.
            ContentFragment::NewHyperlink { .. } => return false,
        }
    }
    new_sections.push(current_text);

    // Compare with original
    let old_sections = extract_text_sections(para);
    let old_anchors = collect_anchor_inventory(para);

    if new_sections.len() != old_sections.len() {
        return false;
    }
    if new_anchor_ids.len() != old_anchors.len() {
        return false;
    }

    for (old, new) in old_sections.iter().zip(new_sections.iter()) {
        if old != new {
            return false;
        }
    }
    for (old, new_id) in old_anchors.iter().zip(new_anchor_ids.iter()) {
        if &old.id != *new_id {
            return false;
        }
    }

    // Text and anchors match (so this is a SAME-TEXT replace). An all-plain
    // replacement (no styled fragment) over a paragraph carrying surface marks is an
    // editor UN-FORMAT — the content specifies no marks for text that is bold/etc.
    // — NOT an identity. (Only same-text, where inherit-vs-set is ambiguous; a
    // genuine text edit never reaches here. The styled branch below covers the ADD
    // direction.) A genuinely unmarked paragraph stays an identity → NoOpEdit.
    if !content.fragments.iter().any(ContentFragment::is_styled)
        && para.all_inlines().any(|i| {
            matches!(i, InlineNode::Text(t) if !marks_set_of(&t.marks, &t.style_props).is_empty())
        })
    {
        return false;
    }

    // Text and anchors match. If the replacement carries mark intent (any
    // `StyledText` fragment), it is only an identity when those marks are
    // already present on the target — otherwise applying it changes formatting
    // and is a genuine edit. The styled materializer rebuilds every run from
    // one exemplar (`first_text_formatting`) unioned with the fragment's
    // overrides, so identity requires (a) the target's runs to be homogeneous
    // at that exemplar, and (b) each fragment's overrides to already be present
    // in the exemplar (so the union is a no-op).
    if content.fragments.iter().any(ContentFragment::is_styled) {
        if !paragraph_marks_are_homogeneous(para) {
            return false;
        }
        let exemplar = first_text_formatting(para);
        for fragment in &content.fragments {
            if let ContentFragment::StyledText { marks, .. } = fragment
                && !overrides_already_present(&exemplar.marks, &exemplar.style_props, marks)
            {
                return false;
            }
        }
    }

    true
}

/// True when every visible text run in the paragraph carries the same marks and
/// strike state as the first run — i.e. the paragraph is mark-homogeneous, the
/// precondition for the styled materializer's one-exemplar rebuild to be a
/// faithful no-op. A paragraph with mixed bold/non-bold runs is not homogeneous,
/// so a styled replace over it is never an identity (it would flatten the mix).
fn paragraph_marks_are_homogeneous(para: &ParagraphNode) -> bool {
    let mut first: Option<(&Vec<Mark>, &MarkValue)> = None;
    for seg in &para.segments {
        for inline in &seg.inlines {
            if let InlineNode::Text(t) = inline {
                match first {
                    None => first = Some((&t.marks, &t.style_props.strike)),
                    Some((marks, strike)) => {
                        if !marks_eq_as_set(marks, &t.marks) || strike != &t.style_props.strike {
                            return false;
                        }
                    }
                }
            }
        }
    }
    true
}

/// True when a `StyledText` fragment's mark overrides are all already present
/// in the exemplar formatting — applying them would not add or change any mark.
fn overrides_already_present(
    exemplar_marks: &[Mark],
    exemplar_style: &StyleProps,
    overrides: &InlineMarkSet,
) -> bool {
    let has = |m: Mark| exemplar_marks.contains(&m);
    (!overrides.bold || has(Mark::Bold))
        && (!overrides.italic || has(Mark::Italic))
        && (!overrides.underline || has(Mark::Underline))
        && (!overrides.subscript || has(Mark::Subscript))
        && (!overrides.superscript || has(Mark::Superscript))
        && (!overrides.strike || exemplar_style.strike == MarkValue::On)
}

/// Compare two mark lists as sets (order-insensitive, no duplicates expected).
fn marks_eq_as_set(a: &[Mark], b: &[Mark]) -> bool {
    a.len() == b.len() && a.iter().all(|m| b.contains(m))
}

// ─── Phase 2: Inline diff ───────────────────────────────────────────────────

/// A diff operation tag at the token level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DiffOp {
    Equal,
    Delete,
    Insert,
}

/// A tagged token from the diff result.
#[derive(Clone, Debug)]
struct DiffToken {
    op: DiffOp,
    text: String,
}

/// Diff two text sections by delegating to the shared token-diff pipeline in
/// `crate::diff`. This runs Patience over `diff::tokenize` and then applies
/// `cleanup_inline_changes_with_config` (adjacent-run merge, zipper-region
/// collapse, character-level affix factoring), matching the comparison
/// pipeline so LLM-driven paragraph replaces don't produce per-word zippers
/// on heavy rewrites.
///
/// The downstream consumer (`reconstruct_section_segments`) walks the tokens
/// by `chars().count()`, so merged `Equal` runs from cleanup are safe: the
/// total character count on each side is preserved by construction
/// (`collapse_region` concatenates `Unchanged` text into both the del and ins
/// sides, and `factor_char_level_affixes` only moves text between adjacent
/// runs of the same pair).
///
/// Both inputs are run through `normalize_diff_punctuation` first so that
/// LLM-emitted ASCII variants of curly quotes, en/em dashes, and NBSP do not
/// show up as spurious `Delete`/`Insert` pairs against Word-authored
/// documents that use the typographic glyphs. Char counts are preserved by
/// the normalization, and the consumer pulls kept/deleted chars from the
/// ORIGINAL text node — so the document's typographic glyphs survive in the
/// unchanged regions of the output.
///
/// Plain `&str` input cannot produce `InlineChange::Opaque`, so that variant
/// is an invariant violation if observed.
fn diff_text_sections(old_text: &str, new_text: &str) -> Vec<DiffToken> {
    let old_norm = normalize_diff_punctuation(old_text);
    let new_norm = normalize_diff_punctuation(new_text);

    // The diff runs over the normalized (ASCII-folded) strings so curly-vs-ASCII
    // variants don't manufacture spurious Delete/Insert pairs. Equal/Delete output
    // text is consumed downstream only by `chars().count()` (kept/deleted chars are
    // pulled from the ORIGINAL old TextNodes), so the normalized form is harmless
    // for those. INSERT tokens are different — see the glyph cursor below.
    //
    // ── Common whole-token affix anchoring ──
    // The word-level Patience diff can shift a boundary when an ambiguous token
    // (e.g. a single space) sits next to the changed region: a pure insertion of
    // " (as amended)" before " for details." can come back as
    // `Delete(" ") + Insert(" (as amended) ") + Equal("for details.")` instead of
    // `Insert(" (as amended)") + Equal(" for details.")`. The boundary space then
    // lands inside a `<w:del>` — and Word's reject, which restores ONLY del text
    // and drops ins text, no longer reproduces the original verbatim.
    //
    // The common leading and trailing *whole tokens* of the two strings are
    // unambiguously unchanged: no correct diff may place them in a del/ins. We
    // factor those off here, emit them as `Equal`, and word-diff only the
    // differing middle. Token granularity is used (not char) so we never split a
    // word mid-glyph: "Investor's" vs "Purchaser's" keeps the whole word as the
    // delta and leaves the shared " duty" tail Equal — matching the readability
    // contract the word tokenizer already enforces. This is the structural
    // invariant Word's reject relies on: unchanged boundary text stays OUTSIDE
    // any tracked envelope.
    let old_tokens = crate::diff::tokenize(&old_norm);
    let new_tokens = crate::diff::tokenize(&new_norm);
    let max_pre = old_tokens.len().min(new_tokens.len());
    let mut pre_tok = 0usize;
    while pre_tok < max_pre && old_tokens[pre_tok] == new_tokens[pre_tok] {
        pre_tok += 1;
    }
    let max_suf = max_pre - pre_tok;
    let mut suf_tok = 0usize;
    while suf_tok < max_suf
        && old_tokens[old_tokens.len() - 1 - suf_tok] == new_tokens[new_tokens.len() - 1 - suf_tok]
    {
        suf_tok += 1;
    }
    let prefix_str: String = old_tokens[..pre_tok].concat();
    let suffix_str: String = old_tokens[old_tokens.len() - suf_tok..].concat();
    let old_mid: String = old_tokens[pre_tok..old_tokens.len() - suf_tok].concat();
    let new_mid: String = new_tokens[pre_tok..new_tokens.len() - suf_tok].concat();

    // BUG-2 glyph cursor: the *inserted* text is genuinely new content the caller
    // supplied — its original glyphs (e.g. a curly apostrophe U+2019) must survive
    // into the `<w:ins>` run rather than being downgraded to the ASCII fold that
    // the diff matched against. Normalization is char-count-preserving, so we
    // restore each Insert token's text from the ORIGINAL `new_text` by char
    // offset: a cursor over `new_text` advances on the leading common run and on
    // every Equal/Insert token of the middle diff (both present on the new side)
    // and is idle on Delete tokens (old side only).
    let new_chars: Vec<char> = new_text.chars().collect();
    let mut new_cursor = prefix_str.chars().count();

    let mut out: Vec<DiffToken> = Vec::new();

    // Leading common run: unchanged, outside any envelope.
    if !prefix_str.is_empty() {
        out.push(DiffToken {
            op: DiffOp::Equal,
            text: prefix_str,
        });
    }

    // Word-diff only the genuinely-differing middle.
    for change in crate::diff::diff_block_content(&old_mid, &new_mid) {
        match change {
            InlineChange::Unchanged { text, .. } => {
                new_cursor += text.chars().count();
                out.push(DiffToken {
                    op: DiffOp::Equal,
                    text,
                });
            }
            InlineChange::Deleted { text, .. } => out.push(DiffToken {
                op: DiffOp::Delete,
                text,
            }),
            InlineChange::Inserted { text, .. } => {
                let len = text.chars().count();
                let original: String = new_chars[new_cursor..new_cursor + len].iter().collect();
                new_cursor += len;
                out.push(DiffToken {
                    op: DiffOp::Insert,
                    text: original,
                });
            }
            InlineChange::Opaque { .. } => {
                unreachable!("diff_block_content on plain text cannot emit InlineChange::Opaque")
            }
        }
    }

    // Trailing common run: unchanged, outside any envelope.
    if !suffix_str.is_empty() {
        out.push(DiffToken {
            op: DiffOp::Equal,
            text: suffix_str,
        });
    }

    out
}

// ─── Phase 3: Segment reconstruction ────────────────────────────────────────

/// Context for formatting inheritance during segment reconstruction.
/// Tracks the most recently seen kept/deleted text node's formatting.
#[derive(Clone, Debug)]
struct FormattingContext {
    marks: Vec<Mark>,
    style_props: StyleProps,
    rpr_authored: RunRprAuthored,
}

/// Resolve formatting for newly inserted text using left-sibling priority.
///
/// 1. If there's a preceding kept/deleted token in the section, clone its formatting.
/// 2. Else if there's a following kept/deleted token, clone its formatting.
/// 3. Else use empty marks and default style_props (paragraph cascade).
fn resolve_formatting_for_insert(
    left_context: &Option<FormattingContext>,
    right_context: &Option<FormattingContext>,
) -> FormattingContext {
    if let Some(ctx) = left_context {
        return ctx.clone();
    }
    if let Some(ctx) = right_context {
        return ctx.clone();
    }
    FormattingContext {
        marks: Vec::new(),
        style_props: StyleProps::default(),
        rpr_authored: RunRprAuthored::default(),
    }
}

// ─── Phase 3: sub-block span addressing ──────────────────────────────────────

/// The flat, document-order list of a paragraph's inlines across all its
/// segments. Span ranges are expressed over this flat index space. Handles
/// are minted by `view::enumerate_text_spans` over the same segment
/// structure, so a flat range maps deterministically back to the inlines the
/// reader saw — including on paragraphs that carry tracked segments (the
/// splice's status predicate separately requires the targeted range itself
/// to be all-Normal).
///
/// `pub(crate)` so the `verbs` planners (e.g. `replace_text`) build their
/// region/coordinate model over the SAME flattening this resolver indexes —
/// the flat index space is true by construction, not by two iterations that
/// happen to agree.
pub(crate) fn flat_inlines(para: &ParagraphNode) -> Vec<&InlineNode> {
    para.segments
        .iter()
        .flat_map(|seg| seg.inlines.iter())
        .collect()
}

/// The same flat, document-order inline list as [`flat_inlines`], but each
/// inline paired with its owning segment status. A planner that needs
/// per-inline editability MUST derive it here rather than re-walking
/// `para.segments` with its own counter — that second walk is what silently
/// drifts from the resolver's index space. One flattening, one coordinate
/// system.
///
/// Invariant: `flat_inlines_with_status(para).len() == flat_inlines(para).len()`
/// and `flat_inlines_with_status(para)[i].0` is the same `&InlineNode` as
/// `flat_inlines(para)[i]`, for every `i` (same iteration order).
pub(crate) fn flat_inlines_with_status(
    para: &ParagraphNode,
) -> Vec<(&InlineNode, &TrackingStatus)> {
    para.segments
        .iter()
        .flat_map(|seg| seg.inlines.iter().map(move |inline| (inline, &seg.status)))
        .collect()
}

/// The flat index of the first inline of `segments[target_seg].inlines[local]`.
fn flat_offset_of(para: &ParagraphNode, target_seg: usize, local: usize) -> usize {
    let mut offset = 0usize;
    for (seg_idx, seg) in para.segments.iter().enumerate() {
        if seg_idx == target_seg {
            return offset + local;
        }
        offset += seg.inlines.len();
    }
    // target_seg past the end ⇒ end-of-paragraph.
    offset
}

/// Flat index immediately AFTER the opaque inline whose id is `anchor_id`, and
/// the index OF it, if present. Searches the flat inline list.
fn find_opaque_flat_index(para: &ParagraphNode, anchor_id: &NodeId) -> Option<usize> {
    flat_inlines(para).iter().position(|inline| match inline {
        InlineNode::OpaqueInline(o) => &o.id == anchor_id,
        InlineNode::HardBreak(hb) => &hb.id == anchor_id,
        _ => false,
    })
}

/// Delete a single opaque inline at flat index `idx` from `para` — a status flip
/// on exactly that inline, NOT a text-content diff (there is no replacement).
///
/// `TrackedChange`: isolate the opaque into its own tracked segment — `Deleted`
/// if it was Normal, un-proposed (dropped, no tombstone) if it was the caller's
/// OWN pending insertion, `InsertedThenDeleted` if a cross-author pending
/// insertion, preserved verbatim if it was already a tombstone. `Direct`: drop
/// it and let its Normal neighbours coalesce. Head/tail segments keep their
/// original status (`split_segments_at_flat_range` clones per-piece status; only
/// identical-status neighbours re-merge in `normalize_segments`).
fn apply_opaque_delete(
    para: &mut ParagraphNode,
    idx: usize,
    mode: MaterializationMode,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
) {
    let (head, mid, tail) = split_segments_at_flat_range(&para.segments, idx, idx + 1);
    let mut segments = head;
    let mid_status = mid
        .first()
        .map(|s| s.status.clone())
        .unwrap_or(TrackingStatus::Normal);
    let inlines: Vec<InlineNode> = mid.into_iter().flat_map(|s| s.inlines).collect();
    match mode {
        MaterializationMode::TrackedChange => match mid_status {
            TrackingStatus::Inserted(ins_rev) => {
                // Author identity is exact byte equality (an anonymous revision
                // never matches — never un-propose what you can't prove is yours).
                let own = matches!(
                    (revision.author.as_deref(), ins_rev.author.as_deref()),
                    (Some(editing), Some(owner)) if editing == owner
                );
                if own {
                    // Deleting one's OWN pending insertion un-proposes it — the
                    // drawing never existed in the base, so no tombstone.
                } else {
                    segments.push(TrackedSegment {
                        status: TrackingStatus::InsertedThenDeleted(Box::new(
                            crate::domain::StackedRevision {
                                inserted: ins_rev,
                                deleted: next_revision(revision, rev_counter),
                            },
                        )),
                        inlines,
                    });
                }
            }
            TrackingStatus::Normal => segments.push(TrackedSegment {
                status: TrackingStatus::Deleted(next_revision(revision, rev_counter)),
                inlines,
            }),
            // Already a tombstone — re-deleting is a no-op; preserve verbatim.
            other => segments.push(TrackedSegment {
                status: other,
                inlines,
            }),
        },
        // Physically drop the opaque; neighbours coalesce below.
        MaterializationMode::Direct => {}
    }
    segments.extend(tail);
    normalize_segments(&mut segments);
    para.segments = segments;
    para.block_text_hash = None;
    para.rendered_text = None;
}

/// Resolve a [`ResolvedSpanSelector`] to a half-open flat inline range
/// `[start, end)` over [`flat_inlines`]. Generalizes `find_anchor`
/// (`edit/verbs/fields_crossrefs.rs`) — but resolves by handle ordinal and by
/// durable opaque id, NEVER by substring.
///
/// CRITICAL determinism contract: a `Handle("s_n")` is resolved through the
/// SAME enumeration the read view used to mint it (`view::enumerate_text_spans`),
/// so a handle from a fresh detail read maps to exactly the inlines the reader
/// saw. Divergence between the view enumeration and this resolver would silently
/// mis-target an edit — hence one shared helper.
///
/// Fails loud: `SpanHandleStale` (handle out of range), `AnchorNotFound`
/// (anchor id absent). The caller separately enforces the range
/// contract on the resolved range (`SpanCrossesTrackedSegment` etc.).
fn resolve_span(
    para: &ParagraphNode,
    block_id: &NodeId,
    sel: &ResolvedSpanSelector,
    step_index: usize,
) -> Result<(usize, usize), EditError> {
    let total = flat_inlines(para).len();

    let anchor_index = |anchor_id: &NodeId| -> Result<usize, EditError> {
        find_opaque_flat_index(para, anchor_id).ok_or_else(|| EditError::AnchorNotFound {
            block_id: block_id.clone(),
            anchor_id: anchor_id.clone(),
            step_index,
        })
    };

    let range = match sel {
        ResolvedSpanSelector::Whole => (0, total),
        ResolvedSpanSelector::Handle(handle) => {
            // The shared enumeration is the single source of truth for `s_<n>`.
            let spans = crate::view::enumerate_text_spans(para);
            let ordinal = parse_span_ordinal(handle).ok_or_else(|| EditError::SpanHandleStale {
                block_id: block_id.clone(),
                handle: handle.clone(),
                span_count: spans.len(),
                step_index,
            })?;
            let span = spans
                .get(ordinal)
                .ok_or_else(|| EditError::SpanHandleStale {
                    block_id: block_id.clone(),
                    handle: handle.clone(),
                    span_count: spans.len(),
                    step_index,
                })?;
            match span {
                crate::view::EnumeratedSpan::Text {
                    seg_idx,
                    inline_start,
                    inline_end,
                    ..
                } => (
                    flat_offset_of(para, *seg_idx, *inline_start),
                    flat_offset_of(para, *seg_idx, *inline_end),
                ),
                crate::view::EnumeratedSpan::Opaque {
                    seg_idx,
                    inline_idx,
                    ..
                } => (
                    flat_offset_of(para, *seg_idx, *inline_idx),
                    flat_offset_of(para, *seg_idx, *inline_idx + 1),
                ),
            }
        }
        ResolvedSpanSelector::AnchorAfter(anchor_id) => {
            let idx = anchor_index(anchor_id)?;
            // Empty insertion range immediately after the anchor.
            (idx + 1, idx + 1)
        }
        ResolvedSpanSelector::AnchorBefore(anchor_id) => {
            let idx = anchor_index(anchor_id)?;
            (idx, idx)
        }
        ResolvedSpanSelector::Between { start, end } => {
            // A `FlatIndex(i)` is a boundary index into `flat_inlines(para)` —
            // the half-open range edge, so the valid domain is `0..=total`
            // (`total` itself is a legal end-of-paragraph boundary). It is
            // INTERNAL-ONLY (no wire shape mints it), so an out-of-domain index
            // is a planner bug, not stale user input. Assert loudly in debug so
            // the offending planner is caught at its test, and still fail the
            // apply (never splice on a bad boundary) via the `end > total` check
            // below in production.
            let flat_boundary = |i: usize| -> usize {
                debug_assert!(
                    i <= total,
                    "ResolvedSpanEndpoint::FlatIndex({i}) out of bounds for \
                     flat_inlines len {total} in block {block_id} — planner bug \
                     (a FlatIndex must be a boundary index 0..={total})",
                );
                i
            };
            let start_idx = match start {
                ResolvedSpanEndpoint::Start => 0,
                ResolvedSpanEndpoint::End => total,
                // `between` deletes/replaces the content BETWEEN the anchors:
                // start just after the start anchor.
                ResolvedSpanEndpoint::Anchor(id) => anchor_index(id)? + 1,
                // A pre-resolved flat-inline boundary (internal planner use).
                ResolvedSpanEndpoint::FlatIndex(i) => flat_boundary(*i),
            };
            let end_idx = match end {
                ResolvedSpanEndpoint::Start => 0,
                ResolvedSpanEndpoint::End => total,
                // ...and up to (not including) the end anchor.
                ResolvedSpanEndpoint::Anchor(id) => anchor_index(id)?,
                ResolvedSpanEndpoint::FlatIndex(i) => flat_boundary(*i),
            };
            (start_idx, end_idx)
        }
    };

    let (start, end) = range;
    if start > end || end > total {
        return Err(EditError::SpanHandleStale {
            block_id: block_id.clone(),
            handle: format!("{sel:?}"),
            span_count: total,
            step_index,
        });
    }
    Ok((start, end))
}

/// Parse the ordinal `n` out of a span handle of the form `s_<n>`.
fn parse_span_ordinal(handle: &str) -> Option<usize> {
    handle.strip_prefix("s_")?.parse::<usize>().ok()
}

/// Split a segment list at a flat inline range into (head, mid, tail).
/// Boundary segments are divided at the range edges; each non-empty piece
/// keeps the segment's status (and revision) — splitting a segment changes
/// segmentation topology, never bytes or status.
fn split_segments_at_flat_range(
    segments: &[TrackedSegment],
    start: usize,
    end: usize,
) -> (
    Vec<TrackedSegment>,
    Vec<TrackedSegment>,
    Vec<TrackedSegment>,
) {
    let mut head: Vec<TrackedSegment> = Vec::new();
    let mut mid: Vec<TrackedSegment> = Vec::new();
    let mut tail: Vec<TrackedSegment> = Vec::new();
    let mut offset = 0usize;
    for segment in segments {
        let seg_start = offset;
        let seg_end = offset + segment.inlines.len();
        offset = seg_end;
        let cut_a = start.clamp(seg_start, seg_end) - seg_start;
        let cut_b = end.clamp(seg_start, seg_end) - seg_start;
        for (piece, bucket) in [
            (&segment.inlines[..cut_a], &mut head),
            (&segment.inlines[cut_a..cut_b], &mut mid),
            (&segment.inlines[cut_b..], &mut tail),
        ] {
            if !piece.is_empty() {
                bucket.push(TrackedSegment {
                    status: segment.status.clone(),
                    inlines: piece.to_vec(),
                });
            }
        }
    }
    (head, mid, tail)
}

/// The wall inventory of the flat inline range `[start, end)` — the anchors
/// (opaques, hard breaks) the replacement content must carry by reference
/// (the in-range walls). Out-of-range walls are NOT in this inventory: the splice
/// carries them itself, untouched.
fn collect_anchor_inventory_in_range(
    para: &ParagraphNode,
    (start, end): (usize, usize),
) -> Vec<AnchorInfo> {
    let mut anchors = Vec::new();
    for inline in &flat_inlines(para)[start..end] {
        match inline {
            InlineNode::OpaqueInline(opaque) => {
                anchors.push(AnchorInfo {
                    id: opaque.id.clone(),
                    kind: opaque_kind_label(&opaque.kind),
                    order_index: anchors.len(),
                });
            }
            InlineNode::HardBreak(hb) => {
                anchors.push(AnchorInfo {
                    id: hb.id.clone(),
                    kind: "hard_break",
                    order_index: anchors.len(),
                });
            }
            _ => {}
        }
    }
    anchors
}

/// Range-scoped identity check: the replacement content equals the targeted
/// range's text sections and wall sequence exactly, so the splice would be a
/// no-op. Mirrors `is_identity_replacement` over a sub-range.
fn is_identity_splice(
    para: &ParagraphNode,
    (start, end): (usize, usize),
    content: &ParagraphContent,
) -> bool {
    let mut new_sections: Vec<String> = Vec::new();
    let mut current_text = String::new();
    let mut new_anchor_ids: Vec<&NodeId> = Vec::new();
    for fragment in &content.fragments {
        match fragment {
            ContentFragment::Text(t) => current_text.push_str(t),
            ContentFragment::StyledText { text, .. } => current_text.push_str(text),
            ContentFragment::PreservedInlineRef(id) => {
                new_sections.push(std::mem::take(&mut current_text));
                new_anchor_ids.push(id);
            }
            ContentFragment::NewHyperlink { .. } => return false,
        }
    }
    new_sections.push(current_text);

    let mut old_sections: Vec<String> = vec![String::new()];
    let mut old_anchor_ids: Vec<&NodeId> = Vec::new();
    for inline in &flat_inlines(para)[start..end] {
        match inline {
            InlineNode::Text(t) => old_sections
                .last_mut()
                .expect("old_sections starts non-empty")
                .push_str(&t.text),
            InlineNode::OpaqueInline(o) => {
                old_anchor_ids.push(&o.id);
                old_sections.push(String::new());
            }
            InlineNode::HardBreak(hb) => {
                old_anchor_ids.push(&hb.id);
                old_sections.push(String::new());
            }
            _ => {}
        }
    }

    new_sections == old_sections && new_anchor_ids == old_anchor_ids
}

/// The status-preserving splice: replace the flat inline range `[start, end)`
/// of `para` with `content`, materializing the change as tracked
/// `Deleted`/`Inserted` segments WITHIN the range and carrying every
/// out-of-range segment through untouched (clone-the-rest discipline). A
/// neighbouring tracked change survives structurally — nothing ever
/// reconstructs it.
///
/// The range differ and segment reconstructor are the SAME engine the
/// whole-paragraph replace uses ([`diff_and_reconstruct_segments`]); the
/// splice feeds them only the targeted range. Formatting context for
/// insertions at the range edges comes from the carried neighbours, which the
/// whole-paragraph diff used to see as part of one input.
///
/// When `resolve_directly` is set (direct materialization mode), the freshly
/// produced del/ins segments are immediately resolved — and ONLY those: a
/// direct edit must never accept or reject someone else's pending tracked
/// change in the head/tail as a side effect.
fn apply_span_splice(
    para: &mut ParagraphNode,
    (start, end): (usize, usize),
    content: &ParagraphContent,
    revision: &RevisionInfo,
    enclosing_insertion: Option<&RevisionInfo>,
    rev_counter: &mut u32,
    resolve_directly: bool,
) {
    let (head, mid, tail) = split_segments_at_flat_range(&para.segments, start, end);

    let outer_left = head
        .iter()
        .rev()
        .flat_map(|seg| seg.inlines.iter().rev())
        .find_map(text_formatting_context);
    let outer_right = tail
        .iter()
        .flat_map(|seg| seg.inlines.iter())
        .find_map(text_formatting_context);

    let mut mid_out = diff_and_reconstruct_segments(
        &mid,
        content,
        revision,
        rev_counter,
        outer_left,
        outer_right,
        enclosing_insertion,
    );
    if resolve_directly {
        mid_out = resolve_fresh_segments(mid_out);
    }

    let mut segments = head;
    segments.extend(mid_out);
    segments.extend(tail);

    // Invariant M (domain-model §6): the same pass set in the same fixed
    // order as the whole-paragraph materializer. All three preserve the
    // concatenated (byte, status) stream and the wall/bracket inventory, so
    // the carried head/tail stay identical under the structural-invariant
    // comparison even when segment boundaries re-draw.
    let segments = crate::tracked_model::coalesce_split_field_sequences(segments);
    let mut segments = crate::tracked_model::normalize_paragraph_opaque_reading_order(segments);
    normalize_segments(&mut segments);
    para.segments = segments;

    // Invalidate caches derived from the previous text.
    para.block_text_hash = None;
    para.rendered_text = None;
}

/// The formatting context of a Text inline, if it is one.
fn text_formatting_context(inline: &InlineNode) -> Option<FormattingContext> {
    match inline {
        InlineNode::Text(t) => Some(FormattingContext {
            marks: t.marks.clone(),
            style_props: t.style_props.clone(),
            rpr_authored: t.rpr_authored,
        }),
        _ => None,
    }
}

/// Resolve (accept) ONLY freshly produced tracked segments: Inserted becomes
/// Normal, Deleted drops. Used by the direct-mode splice on the mid output
/// before stitching, so pre-existing tracked changes in the head/tail are
/// never resolved as a side effect.
fn resolve_fresh_segments(segments: Vec<TrackedSegment>) -> Vec<TrackedSegment> {
    segments
        .into_iter()
        .filter_map(|seg| match seg.status {
            TrackingStatus::Deleted(_) => None,
            TrackingStatus::Inserted(_) => Some(TrackedSegment {
                status: TrackingStatus::Normal,
                inlines: seg.inlines,
            }),
            TrackingStatus::Normal => Some(seg),
            TrackingStatus::InsertedThenDeleted(_) => unreachable!(
                "direct-mode splices admit all-Normal ranges only; the differ \
                 emits no stacked segments"
            ),
        })
        .collect()
}

/// Phase 1 validation for a `ReplaceSpanText` step: locate + editability +
/// the mandatory staleness guard, then resolve the span to a flat inline
/// range and enforce the range contract (status, text identity,
/// brackets — the walls half is the caller's range-scoped
/// `validate_preserved_inlines`). Returns the path and the resolved range.
fn validate_span_replace_step(
    doc: &CanonDoc,
    block_id: &NodeId,
    guard: &str,
    expect: Option<&str>,
    span: &ResolvedSpanSelector,
    in_place_author: Option<&str>,
    step_index: usize,
) -> Result<(ParagraphPath, (usize, usize)), EditError> {
    let path = find_paragraph_path(doc, block_id).ok_or_else(|| EditError::BlockNotFound {
        block_id: block_id.clone(),
        step_index,
    })?;

    if path.is_top_level() {
        let tracked_block = &doc.blocks[path.top_block];
        match &tracked_block.status {
            TrackingStatus::Normal => {}
            TrackingStatus::Inserted(_) => {
                return Err(EditError::BlockHasTrackedStatus {
                    block_id: block_id.clone(),
                    status: "inserted",
                    step_index,
                });
            }
            TrackingStatus::Deleted(_) => {
                return Err(EditError::BlockHasTrackedStatus {
                    block_id: block_id.clone(),
                    status: "deleted",
                    step_index,
                });
            }
            TrackingStatus::InsertedThenDeleted(_) => {
                return Err(EditError::BlockHasTrackedStatus {
                    block_id: block_id.clone(),
                    status: "inserted_then_deleted",
                    step_index,
                });
            }
        }
    } else {
        check_ancestor_table_tracking(doc, &path, block_id, step_index)?;
    }

    let block = block_at(doc, &path);
    let para = match block {
        BlockNode::Paragraph(p) => p,
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
    };

    // The block guard is the authoritative staleness gate (unified-guard
    // contract) and is checked BEFORE resolving the span: a stale block means
    // the handle ordinals are meaningless. It is mandatory by type:
    // the same guard is what refuses the second of two same-paragraph ops in
    // one transaction — op 1's splice moves the hash. This is a FRESHNESS
    // check; the range-status predicate below is a CONTENT check. They are
    // different guards and narrowing one does not weaken the other.
    if let Err(actual) = check_block_guard(block, guard) {
        return Err(EditError::BlockSemanticHashMismatch {
            block_id: block_id.clone(),
            expected: guard.to_string(),
            actual,
            step_index,
        });
    }

    let range = resolve_span(para, block_id, span, step_index)?;

    // Status: the targeted range must be all-Normal — or, in tracked
    // mode, inside the editing author's own pending insertion (same-author
    // in-place editing). Tracked segments outside the range are walls,
    // carried by reference.
    validate_range_status(para, block_id, range, in_place_author, step_index)?;

    // Text identity: when the op carries `expect`, the resolved range's
    // visible text must equal it exactly. The guard is deliberately
    // segmentation-insensitive while a handle is an ordinal over the
    // segmentation; this comparison is what makes a handle safe across
    // engine-version changes to segmentation normalization.
    if let Some(expected) = expect {
        let actual = range_visible_text(para, range);
        if actual != expected {
            return Err(EditError::SpanTextMismatch {
                block_id: block_id.clone(),
                expected: expected.to_string(),
                actual,
                step_index,
            });
        }
    }

    // Brackets: the splice boundary must not fall between a paired
    // range marker and its partner.
    validate_range_brackets(para, block_id, range, step_index)?;

    Ok((path, range))
}

/// The range-status predicate: every segment overlapping the flat inline range
/// `[start, end)` must be `Normal` — or, when `in_place_author` is given
/// (tracked-change mode, same-author in-place editing), an `Inserted` segment whose revision
/// author equals the editing author: an author may edit THEIR OWN pending
/// insertion in place. Another author's insertion (stacking, step 3) and any
/// pending deletion (its text is already struck; rewriting it has no tracked
/// semantics) still refuse. An empty range at a segment boundary overlaps
/// nothing, so a pure insertion beside a tracked segment is always admitted
/// (it moves no tracked content).
fn validate_range_status(
    para: &ParagraphNode,
    block_id: &NodeId,
    (start, end): (usize, usize),
    in_place_author: Option<&str>,
    step_index: usize,
) -> Result<(), EditError> {
    let mut offset = 0usize;
    for segment in &para.segments {
        let seg_start = offset;
        let seg_end = offset + segment.inlines.len();
        offset = seg_end;
        let overlaps = seg_start < end && seg_end > start;
        if !overlaps {
            continue;
        }
        let admitted = match &segment.status {
            TrackingStatus::Normal => true,
            // Any pending insertion is editable in tracked mode (step 3a):
            // the in-place-vs-stack decision is made per character at the
            // Delete arm of the reconstruction — text removed from one's OWN
            // insertion is un-proposed (dropped), text removed from another
            // author's becomes the stacked state. Direct mode (`editing
            // author` None) admits all-Normal only.
            TrackingStatus::Inserted(_) => in_place_author.is_some(),
            TrackingStatus::Deleted(_) => false,
            // The stacked state is TERMINAL in the inline text grammar:
            // there is no fifth state for a further edit to
            // map to. You resolve it; you don't edit it.
            TrackingStatus::InsertedThenDeleted(_) => false,
        };
        if !admitted {
            return Err(EditError::SpanCrossesTrackedSegment {
                block_id: block_id.clone(),
                step_index,
            });
        }
    }
    Ok(())
}

/// Visible text of the flat inline range `[start, end)` — Text inlines only,
/// matching what the read view projects for a span.
fn range_visible_text(para: &ParagraphNode, (start, end): (usize, usize)) -> String {
    flat_inlines(para)[start..end]
        .iter()
        .filter_map(|inline| match inline {
            InlineNode::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect()
}

/// A paired range marker (a "bracket") at its flat inline index.
struct BracketMarker {
    flat_idx: usize,
    /// Pair kind, e.g. "bookmark", "commentRange", "perm", "moveFromRange".
    kind: String,
    /// Pair identity within the kind (the marker's `w:id` / comment id).
    pair_id: String,
}

/// Collect the paragraph's pairable bracket markers in flat order.
///
/// Markers that carry no pair identity are deliberately not collected:
/// `proofErr` has no id (Word regenerates proofing state), and a decoration
/// whose raw bytes don't reparse cannot be classified. The splice preserves
/// every marker it encounters either way — the bracket predicate exists only
/// to protect pair EXTENT, which is meaningless for unpairable markers.
fn collect_bracket_markers(para: &ParagraphNode) -> Vec<BracketMarker> {
    let mut out = Vec::new();
    // CustomXmlWrapper markers carry no w:id and both halves share the same
    // childless bytes, so they pair by STACK ORDER (like the serializer's
    // renest pass), not by id. Walk them with a stack of in-flight open pair
    // ids and assign each matched open/close pair the same synthetic pair_id so
    // the bracket guard protects the wrapper's extent (task #6, Rider 2: a
    // splice must not tear a wrapper).
    let mut wrapper_stack: Vec<String> = Vec::new();
    let mut next_wrapper_pair = 0usize;

    for (flat_idx, inline) in flat_inlines(para).iter().enumerate() {
        match inline {
            InlineNode::CommentRangeStart { id } | InlineNode::CommentRangeEnd { id } => {
                out.push(BracketMarker {
                    flat_idx,
                    kind: "commentRange".to_string(),
                    pair_id: id.clone(),
                });
            }
            InlineNode::Decoration(deco)
                if deco.kind == crate::domain::DecorationType::CustomXmlWrapper =>
            {
                // A pending open on the stack closes here; otherwise this opens a
                // new wrapper. Both halves share the same synthetic pair_id.
                let pair_id = match wrapper_stack.pop() {
                    Some(open_pair_id) => open_pair_id,
                    None => {
                        let id = format!("w{next_wrapper_pair}");
                        next_wrapper_pair += 1;
                        wrapper_stack.push(id.clone());
                        id
                    }
                };
                out.push(BracketMarker {
                    flat_idx,
                    kind: "customXmlWrapper".to_string(),
                    pair_id,
                });
            }
            InlineNode::Decoration(deco) => {
                if let Some((kind, pair_id)) = decoration_pair_key(deco) {
                    out.push(BracketMarker {
                        flat_idx,
                        kind,
                        pair_id,
                    });
                }
            }
            _ => {}
        }
    }
    out
}

/// The (kind, pair-id) key that pairs a decoration marker with its partner:
/// `bookmarkStart`/`bookmarkEnd` share a `w:id`, as do `permStart`/`permEnd`,
/// `customXml*RangeStart`/`End` and `move*RangeStart`/`End`. Read from the
/// marker's own raw bytes (the importer stores the original element).
fn decoration_pair_key(deco: &crate::domain::DecorationNode) -> Option<(String, String)> {
    let raw = deco.raw_xml.as_deref()?;
    let el = match crate::word_xml::parse_raw_fragment(raw) {
        Ok(el) => el,
        Err(e) => {
            // Decoration raw bytes are written by our own importer; failing to
            // reparse them is a programmer bug, not a document state.
            debug_assert!(false, "decoration raw_xml failed to reparse: {e}");
            return None;
        }
    };
    let stem = el
        .name
        .strip_suffix("Start")
        .or_else(|| el.name.strip_suffix("End"))?;
    let id = crate::xml_attrs::attr_get(&el, "id")?;
    Some((stem.to_string(), id.clone()))
}

/// The brackets predicate: refuse a non-empty range that contains one
/// member of a marker pair but not its partner (whether the partner sits
/// elsewhere in this paragraph or in another block entirely). A pure
/// insertion point moves no markers and cannot split a pair.
fn validate_range_brackets(
    para: &ParagraphNode,
    block_id: &NodeId,
    (start, end): (usize, usize),
    step_index: usize,
) -> Result<(), EditError> {
    if start == end {
        return Ok(());
    }
    let markers = collect_bracket_markers(para);
    for marker in &markers {
        if marker.flat_idx < start || marker.flat_idx >= end {
            continue;
        }
        let partner_inside = markers.iter().any(|other| {
            other.flat_idx != marker.flat_idx
                && other.kind == marker.kind
                && other.pair_id == marker.pair_id
                && other.flat_idx >= start
                && other.flat_idx < end
        });
        if !partner_inside {
            return Err(EditError::SpanSplitsBracketPair {
                block_id: block_id.clone(),
                bracket_kind: marker.kind.clone(),
                pair_id: marker.pair_id.clone(),
                step_index,
            });
        }
    }
    Ok(())
}

/// Apply a `ReplaceParagraphText` step using "whole-paragraph segment
/// replace": wrap the original inlines in a single Deleted segment and
/// the freshly-built inlines in a single Inserted segment. Used for
/// content fragments the inline-diff path can't handle today —
/// `StyledText` (marks on new text) and `NewHyperlink` (creating an
/// `<link>` opaque from markup).
///
/// Block id stays unchanged. Preserved-inline references in the new
/// content reference cloned copies of the original opaques (which are
/// themselves still present in the Deleted segment). On accept the
/// Deleted segment drops and only the inserted clones remain; on reject
/// the Inserted segment drops and the originals come back.
fn apply_segment_replace_paragraph(
    para: &mut ParagraphNode,
    content: &ParagraphContent,
    revision: &RevisionInfo,
    enclosing_insertion: Option<&RevisionInfo>,
    rev_counter: &mut u32,
) {
    let fmt = first_text_formatting(para);
    let stamped = stamp_revision(revision, rev_counter);

    // Collect old inlines: every inline from every Normal segment becomes
    // the body of the single Deleted segment. (The caller has already
    // validated that all existing segments are Normal — that's a
    // precondition of ReplaceParagraphText.)
    let mut old_inlines: Vec<InlineNode> = Vec::new();
    for seg in &para.segments {
        for inline in &seg.inlines {
            old_inlines.push(inline.clone());
        }
    }

    // Build a lookup of preserved inlines so `PreservedInlineRef` can
    // resolve to a clone of the original.
    let mut preserved_lookup: std::collections::HashMap<NodeId, InlineNode> =
        std::collections::HashMap::new();
    for inline in &old_inlines {
        match inline {
            InlineNode::OpaqueInline(o) => {
                preserved_lookup.insert(o.id.clone(), inline.clone());
            }
            InlineNode::HardBreak(hb) => {
                preserved_lookup.insert(hb.id.clone(), inline.clone());
            }
            _ => {}
        }
    }

    // Build new inlines from the content fragments.
    let mut new_inlines: Vec<InlineNode> = Vec::new();
    for (idx, fragment) in content.fragments.iter().enumerate() {
        let node_id = NodeId::from(format!("{}_seg_t{idx}", para.id.0));
        match fragment {
            ContentFragment::Text(t) => {
                new_inlines.push(InlineNode::from(build_text_node_from_exemplar(
                    node_id,
                    &fmt,
                    t.clone(),
                    None,
                )));
            }
            ContentFragment::StyledText { text, marks } => {
                new_inlines.push(InlineNode::from(build_text_node_from_exemplar(
                    node_id,
                    &fmt,
                    text.clone(),
                    Some(*marks),
                )));
            }
            ContentFragment::PreservedInlineRef(id) => {
                // Validation guaranteed the id resolves to an opaque or
                // hard break in the original paragraph.
                let original = preserved_lookup
                    .get(id)
                    .cloned()
                    .expect("validate_preserved_inlines ensured the id is present");
                new_inlines.push(original);
            }
            ContentFragment::NewHyperlink { href, anchor, text } => {
                new_inlines.push(synthesize_new_hyperlink_inline(node_id, href, anchor, text));
            }
        }
    }

    // Build the new segment list: the old content as a tombstone + Inserted(new).
    // Empty segments are filtered (an empty new_inlines means the LLM
    // produced no replacement content — the paragraph reads as a pure
    // deletion, which is also what `delete` would do).
    //
    // The old content's tombstone status follows the SAME origin rule the
    // word-diff reconstruction uses: if the whole paragraph is a pending
    // block insertion, its old content is not base content — removing it
    // un-proposes it (own author) or stacks it (cross author), never a plain
    // `Deleted` (which reject-all would restore, leaking text that never existed).
    let mut new_segments: Vec<TrackedSegment> = Vec::new();
    if !old_inlines.is_empty() {
        let own = enclosing_insertion.is_some_and(|ins| {
            matches!(
                (revision.author.as_deref(), ins.author.as_deref()),
                (Some(editing), Some(owner)) if editing == owner
            )
        });
        match enclosing_insertion {
            // Own pending block insertion → un-propose the old content: emit no
            // tombstone at all (the block insertion still carries the paragraph).
            Some(_) if own => {}
            // Cross-author pending block insertion → the stacked state.
            Some(block_ins) => new_segments.push(TrackedSegment {
                status: TrackingStatus::InsertedThenDeleted(Box::new(
                    crate::domain::StackedRevision {
                        inserted: block_ins.clone(),
                        deleted: stamped.clone(),
                    },
                )),
                inlines: old_inlines,
            }),
            // Genuine base content → a deletion reject restores.
            None => new_segments.push(TrackedSegment {
                status: TrackingStatus::Deleted(stamped.clone()),
                inlines: old_inlines,
            }),
        }
    }
    if !new_inlines.is_empty() {
        new_segments.push(TrackedSegment {
            status: TrackingStatus::Inserted(stamped),
            inlines: new_inlines,
        });
    }
    para.segments = new_segments;

    // Invalidate caches that depended on the previous text.
    para.block_text_hash = None;
    para.rendered_text = None;
}

/// The authored-mark set an original run carries (the inverse of
/// `apply_mark_overrides`): the boolean marks present plus `strike` lifted from
/// the style props. `caps`/`small_caps` are ignored — the surgical insert path
/// can't author them, so they never participate in a kept-text reformat.
fn marks_set_of(marks: &[Mark], style_props: &StyleProps) -> InlineMarkSet {
    InlineMarkSet {
        bold: marks.contains(&Mark::Bold),
        italic: marks.contains(&Mark::Italic),
        underline: marks.contains(&Mark::Underline),
        subscript: marks.contains(&Mark::Subscript),
        superscript: marks.contains(&Mark::Superscript),
        strike: style_props.strike == MarkValue::On,
        ..InlineMarkSet::default()
    }
}

/// How a styled replacement relates to the paragraph's KEPT (Equal) text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeptTextReformat {
    /// No kept char changes marks — a pure text edit; stay surgical.
    None,
    /// The text is UNCHANGED (all-Equal) but some run's marks change — a pure
    /// formatting change, expressible as a surgical per-run tracked rPrChange.
    FormattingOnly,
    /// Text changes AND some kept text is reformatted in the same edit — the
    /// surgical word-diff can't carry the reformat, so fall back to a whole-
    /// paragraph segment replace (a reversible delete+insert).
    Mixed,
}

/// Classify how applying `content` as a surgical word-diff against `para` would
/// treat KEPT (Equal) text's formatting. The text-keyed word-diff carries kept
/// text's ORIGINAL marks, so it cannot express a reformat of text it doesn't
/// change; the caller routes around that: `FormattingOnly` (text identical, marks
/// changed) becomes a surgical per-run rPrChange; `Mixed` (text changed AND kept
/// text reformatted) falls back to the whole-paragraph segment replace; `None`
/// (no kept-text reformat) stays surgical.
fn classify_kept_text_reformat(
    para: &ParagraphNode,
    content: &ParagraphContent,
) -> KeptTextReformat {
    // Per-char authored marks of the NEW content (flat text only).
    let mut new_text = String::new();
    let mut new_marks: Vec<InlineMarkSet> = Vec::new();
    for frag in &content.fragments {
        match frag {
            ContentFragment::Text(t) => {
                for ch in t.chars() {
                    new_text.push(ch);
                    new_marks.push(InlineMarkSet::default());
                }
            }
            ContentFragment::StyledText { text, marks } => {
                for ch in text.chars() {
                    new_text.push(ch);
                    new_marks.push(*marks);
                }
            }
            // Links / preserved refs carry no text marks — skip (the detection is
            // about reformatting kept TEXT; biasing toward whole-para is safe).
            _ => {}
        }
    }
    // Per-char marks of the ORIGINAL paragraph (flat text runs only).
    let mut old_marks: Vec<InlineMarkSet> = Vec::new();
    let mut old_text = String::new();
    for inline in para.all_inlines() {
        if let InlineNode::Text(t) = inline {
            let ms = marks_set_of(&t.marks, &t.style_props);
            for ch in t.text.chars() {
                old_text.push(ch);
                old_marks.push(ms);
            }
        }
    }
    // A kept (Equal) char whose authored marks changed is a reformat; any Delete/
    // Insert token is a real text change.
    let mut saw_changed_equal_mark = false;
    let mut saw_indel = false;
    let mut oi = 0usize;
    let mut ni = 0usize;
    for tok in diff_text_sections(&old_text, &new_text) {
        let n = tok.text.chars().count();
        match tok.op {
            DiffOp::Equal => {
                for k in 0..n {
                    if old_marks.get(oi + k) != new_marks.get(ni + k) {
                        saw_changed_equal_mark = true;
                    }
                }
                oi += n;
                ni += n;
            }
            DiffOp::Delete => {
                saw_indel = true;
                oi += n;
            }
            DiffOp::Insert => {
                saw_indel = true;
                ni += n;
            }
        }
    }
    if !saw_changed_equal_mark {
        return KeptTextReformat::None;
    }
    if saw_indel {
        return KeptTextReformat::Mixed;
    }
    // Formatting-only (all-Equal, a mark changed): take the surgical per-run
    // rPrChange path ONLY on an all-Normal paragraph, where the new content's flat
    // text aligns 1:1 with the paragraph's runs. A paragraph that already carries a
    // pending tracked change doesn't align that way, so fall back to whole-para.
    if para
        .segments
        .iter()
        .all(|s| s.status == TrackingStatus::Normal)
    {
        KeptTextReformat::FormattingOnly
    } else {
        KeptTextReformat::Mixed
    }
}

/// Overwrite a paragraph's content with a single PLAIN (untracked, `Normal`)
/// run of `text` — no diff, no `w:ins` layer. Used ONLY when the paragraph's
/// enclosing table cell is ITSELF a pending insertion authored earlier in the
/// current transaction (`apply_set_cell_text_in_place`'s `own_pending_insert`
/// case): the row/cell already carries the tracked-insert envelope, so the
/// runs inside it need no per-run tracking of their own — the whole cell (a
/// fresh row/cell built by `fresh_row_like`, always all-`Normal` segments) is
/// already the thing being inserted.
fn set_paragraph_plain_text(para: &mut ParagraphNode, text: &str) {
    let inline = InlineNode::from(TextNode {
        id: NodeId::from(format!("{}_t", para.id.0)),
        text_role: None,
        text: text.to_string(),
        marks: Vec::new(),
        style_props: StyleProps::default(),
        rpr_authored: RunRprAuthored::default(),
        source_run_attrs: Vec::new(),
        formatting_change: None,
    });
    para.segments = normal_segment(vec![inline]);
    para.block_text_hash = Some(crate::import::sha256_hex(text.as_bytes()));
    para.rendered_text = None;
}

/// The pending block insertion the paragraph at `path` is ITSELF part of, if the
/// edit is tracked and the top-level block carries `TrackingStatus::Inserted`
/// (e.g. a paragraph added by a prior tracked `InsertParagraphs`). The
/// reconstruction uses it so that removing text from such a paragraph is treated
/// as editing pending-inserted content — un-proposed (same author) or stacked
/// (cross author) — never a plain `Deleted` tombstone that reject-all restores.
///
/// Returns `None` in Direct mode (prep flattens the block to a Normal base
/// first), and for a paragraph inside a table cell (whose insertion axis is the
/// cell/row tracked status, resolved on the cell path, not a top-level block).
fn enclosing_block_insertion(
    doc: &CanonDoc,
    path: &ParagraphPath,
    mode: MaterializationMode,
) -> Option<RevisionInfo> {
    if mode != MaterializationMode::TrackedChange || !path.is_top_level() {
        return None;
    }
    match &doc.blocks[path.top_block].status {
        TrackingStatus::Inserted(rev) => Some(rev.clone()),
        _ => None,
    }
}

/// Apply a ReplaceParagraphText step to a single paragraph: a splice over the
/// whole content range — the whole-paragraph replace and the span replace are
/// the same primitive, fed different ranges.
fn apply_replace_paragraph_text(
    para: &mut ParagraphNode,
    content: &ParagraphContent,
    revision: &RevisionInfo,
    enclosing_insertion: Option<&RevisionInfo>,
    rev_counter: &mut u32,
) {
    // The original segments still carry comment-range / decoration markers at
    // their true text offsets; the diff flushes them as trailing decorations
    // (position lost), so capture them now to re-place afterward (B4).
    let original = para.segments.clone();

    let segments = diff_and_reconstruct_segments(
        &para.segments,
        content,
        revision,
        rev_counter,
        None,
        None,
        enclosing_insertion,
    );

    // ── Phase 4: Normalize ──
    // Invariant M (domain-model §6): both materializers run the SAME pass set in
    // the SAME fixed order — field-coalescing, then opaque reading-order, then
    // segment normalization. The first two are guarded no-ops on ordinary edit
    // output (see their doc comments); running them here makes the edit and
    // merge paths converge on one lowering rather than two with divergent
    // coverage.
    let segments = crate::tracked_model::coalesce_split_field_sequences(segments);
    let mut segments = crate::tracked_model::normalize_paragraph_opaque_reading_order(segments);
    normalize_segments(&mut segments);

    // ── Re-place structural markers at their text offsets ──
    // A paragraph edit must not orphan a comment: drop the diff's mis-positioned
    // trailing markers and reposition every structural marker (comment ranges,
    // decorations) at its ORIGINAL offset, exactly as the merge path does — so a
    // comment range survives an edit instead of collapsing to the paragraph end
    // (B4). For a marker-free paragraph this is a no-op (strip removes nothing;
    // inject returns early on empty markers), so non-commented edits are
    // unaffected.
    if has_structural_markers(&original) {
        strip_structural_markers(&mut segments);
        crate::tracked_model::inject_structural_markers_at_offsets(&mut segments, &original, None);
        normalize_segments(&mut segments);
    }
    para.segments = segments;
}

/// Apply a FORMATTING-ONLY replace: the new content has the SAME flat text as the
/// paragraph (the `KeptTextReformat::FormattingOnly` precondition) but reformats
/// some runs. Rather than a whole-paragraph delete+insert, this rewrites only the
/// reformatted runs in place and records a tracked per-run `FormattingChange`
/// (rPrChange) on each — exactly the representation `set_format` produces for
/// font/color. The text stays unchanged (every run is Normal); accept keeps the new
/// marks, reject restores `previous_*`. Because it carries the FULL new mark set per
/// char (via `set_mark_surface`), it expresses mark REMOVAL too (un-bolding a word),
/// which the add-only formatting paths cannot.
fn apply_formatting_only_replace(
    para: &mut ParagraphNode,
    content: &ParagraphContent,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
) {
    // Per-char target marks (plain Text → none, StyledText → its marks), aligned
    // 1:1 to the paragraph's flat Text chars — guaranteed by the FormattingOnly
    // precondition (new flat text == old flat text, all runs Normal). Links /
    // preserved refs carry no text marks (same skip as the classifier).
    let mut target: Vec<InlineMarkSet> = Vec::new();
    for frag in &content.fragments {
        match frag {
            ContentFragment::Text(t) => target.extend(std::iter::repeat_n(
                InlineMarkSet::default(),
                t.chars().count(),
            )),
            ContentFragment::StyledText { text, marks } => {
                target.extend(std::iter::repeat_n(*marks, text.chars().count()))
            }
            _ => {}
        }
    }

    let original = para.segments.clone();
    let rev = next_revision(revision, rev_counter);
    let mut ti = 0usize; // flat Text-char cursor into `target`
    for seg in &mut para.segments {
        let mut rebuilt: Vec<InlineNode> = Vec::new();
        for inline in std::mem::take(&mut seg.inlines) {
            let InlineNode::Text(node) = inline else {
                // Comment markers / decorations / opaques pass through in place —
                // the text is unchanged, so their offsets are still correct.
                rebuilt.push(inline);
                continue;
            };
            let chars: Vec<char> = node.text.chars().collect();
            let mut start = 0usize;
            while start < chars.len() {
                // Split the run at boundaries where the target mark-set changes, so
                // exactly the reformatted span carries a FormattingChange.
                let cur = target.get(ti + start).copied().unwrap_or_default();
                let mut end = start + 1;
                while end < chars.len() && target.get(ti + end).copied().unwrap_or_default() == cur
                {
                    end += 1;
                }
                let slice: String = chars[start..end].iter().collect();
                let id = if start == 0 {
                    node.id.clone()
                } else {
                    NodeId::new(format!("{}_split_{}", node.id.0, start))
                };
                let mut new_marks = node.marks.clone();
                let mut new_style_props = node.style_props.clone();
                set_mark_surface(&mut new_marks, &mut new_style_props, cur);
                // No-op run (marks unchanged) carries NO rPrChange — an empty
                // formatting change would be a spurious tracked edit.
                let formatting_change =
                    if new_marks == node.marks && new_style_props == node.style_props {
                        None
                    } else {
                        // Baseline rule (B6): if the run already carries a tracked
                        // formatting change, keep THAT change's original rPr (and its
                        // rpr_authored — same class-audit fix as run_formatting.rs's
                        // apply_marks: previous_marks/previous_style_props alone are
                        // not enough, the serializer separately consults rpr_authored
                        // to decide what to emit) so reject restores the true origin;
                        // else the live run's current rPr/authored state.
                        let (previous_marks, previous_style_props, previous_rpr_authored) =
                            match &node.formatting_change {
                                Some(fc) => (
                                    fc.previous_marks.clone(),
                                    fc.previous_style_props.clone(),
                                    fc.previous_rpr_authored,
                                ),
                                None => (
                                    node.marks.clone(),
                                    node.style_props.clone(),
                                    node.rpr_authored,
                                ),
                            };
                        Some(FormattingChange {
                            revision_id: rev.revision_id,
                            identity: 0,
                            previous_marks,
                            previous_style_props,
                            previous_rpr_authored,
                            author: rev.author.clone().unwrap_or_default(),
                            date: rev.date.clone(),
                        })
                    };
                let mut rpr_authored = node.rpr_authored;
                claim_authored_marks(&mut rpr_authored, cur);
                rebuilt.push(InlineNode::from(TextNode {
                    id,
                    text_role: node.text_role.clone(),
                    text: slice,
                    marks: new_marks,
                    style_props: new_style_props,
                    rpr_authored,
                    source_run_attrs: node.source_run_attrs.clone(),
                    formatting_change,
                }));
                start = end;
            }
            ti += chars.len();
        }
        seg.inlines = rebuilt;
    }

    // Same normalize pass set as apply_replace_paragraph_text (no-ops on the
    // unchanged opaque/field structure here; merges adjacent untouched runs while
    // keeping a reformatted run — its differing formatting_change blocks the merge).
    let segments = std::mem::take(&mut para.segments);
    let segments = crate::tracked_model::coalesce_split_field_sequences(segments);
    let mut segments = crate::tracked_model::normalize_paragraph_opaque_reading_order(segments);
    normalize_segments(&mut segments);
    if has_structural_markers(&original) {
        strip_structural_markers(&mut segments);
        crate::tracked_model::inject_structural_markers_at_offsets(&mut segments, &original, None);
        normalize_segments(&mut segments);
    }
    para.segments = segments;
    para.block_text_hash = None;
    para.rendered_text = None;
}

/// True if any segment carries a comment-range / decoration marker.
fn has_structural_markers(segments: &[TrackedSegment]) -> bool {
    segments.iter().any(|seg| {
        seg.inlines.iter().any(|i| {
            matches!(
                i,
                InlineNode::Decoration(_)
                    | InlineNode::CommentRangeStart { .. }
                    | InlineNode::CommentRangeEnd { .. }
                    | InlineNode::CommentReference { .. }
            )
        })
    })
}

/// Remove comment-range / decoration markers from each segment's inline list
/// (their replacement at correct offsets is done by
/// `inject_structural_markers_at_offsets`).
fn strip_structural_markers(segments: &mut [TrackedSegment]) {
    for seg in segments.iter_mut() {
        seg.inlines.retain(|i| {
            !matches!(
                i,
                InlineNode::Decoration(_)
                    | InlineNode::CommentRangeStart { .. }
                    | InlineNode::CommentRangeEnd { .. }
                    | InlineNode::CommentReference { .. }
            )
        });
    }
}

/// The shared diff-and-reconstruct engine behind both splice entry points:
/// section the old `segments` at wall boundaries, word-diff each section's
/// text against the replacement content, and rebuild tracked segments —
/// original TextNodes carried/split for kept and deleted text, fresh nodes
/// for inserted text.
///
/// `outer_left`/`outer_right` supply formatting context from text that sits
/// OUTSIDE `segments` (the splice's carried head/tail), used only when an
/// insertion has no kept/deleted neighbour inside the range to inherit from.
/// The whole-paragraph caller passes `None` — its input is the whole content,
/// so there is no outside.
///
/// Does NOT run the normalize pass set; callers stitch first (the splice) and
/// then run the shared passes over the full segment list (Invariant M).
fn diff_and_reconstruct_segments(
    segments: &[TrackedSegment],
    content: &ParagraphContent,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    outer_left: Option<FormattingContext>,
    outer_right: Option<FormattingContext>,
    // The pending block insertion the target paragraph is ITSELF part of, if any.
    // A `Normal` segment inside such a paragraph exists only by virtue of that
    // insertion (never in the base), so a delete of it un-proposes/stacks rather
    // than minting a plain `Deleted` tombstone (which reject-all would restore).
    enclosing_insertion: Option<&RevisionInfo>,
) -> Vec<TrackedSegment> {
    // ── Split old content at anchor boundaries ──
    // Build a map of the old paragraph's inline structure.
    //
    // We need to:
    // 1. Extract old text sections (text between anchors)
    // 2. Extract new text sections (text between PreservedInlineRefs)
    // 3. Diff each corresponding pair
    // 4. Reconstruct segments from diff output, preserving original TextNodes

    // Collect the inline nodes in order, grouped by text section.
    // A "section" is delimited by anchors (opaques/hard breaks).
    let mut old_sections: Vec<OldSection> = Vec::new();
    let mut current_inlines: Vec<(usize, usize, &InlineNode)> = Vec::new();
    let mut anchors_between: Vec<(usize, usize, &InlineNode)> = Vec::new();
    let mut decorations_between: Vec<(usize, usize, &InlineNode)> = Vec::new();

    for (seg_idx, segment) in segments.iter().enumerate() {
        for (inline_idx, inline) in segment.inlines.iter().enumerate() {
            match inline {
                InlineNode::Text(_) => {
                    current_inlines.push((seg_idx, inline_idx, inline));
                }
                InlineNode::OpaqueInline(_) | InlineNode::HardBreak(_) => {
                    old_sections.push(OldSection {
                        text_inlines: std::mem::take(&mut current_inlines),
                        trailing_decorations: std::mem::take(&mut decorations_between),
                    });
                    anchors_between.push((seg_idx, inline_idx, inline));
                }
                InlineNode::Decoration(_)
                | InlineNode::CommentRangeStart { .. }
                | InlineNode::CommentRangeEnd { .. }
                | InlineNode::CommentReference { .. } => {
                    decorations_between.push((seg_idx, inline_idx, inline));
                }
            }
        }
    }
    // Final section (after last anchor or all content if no anchors)
    old_sections.push(OldSection {
        text_inlines: std::mem::take(&mut current_inlines),
        trailing_decorations: std::mem::take(&mut decorations_between),
    });

    // Extract new sections from the replacement content. Each section
    // owns its sequence of Text/Link parts so the diff stage can render
    // a flat string with private-use placeholders for the links — the
    // diff treats each link as a single inserted character, and the
    // reconstructor maps the placeholder back to a synthesized
    // `OpaqueInline{Hyperlink}` in the inserted segment.
    //
    // StyledText fragments still route to the whole-paragraph segment-
    // replace path (the caller in `apply_transaction` checks
    // `ContentFragment::is_styled` and dispatches there). Reaching this
    // function with a StyledText fragment would silently drop the LLM's
    // mark intent — forbidden by CLAUDE.md — so we assert here.
    let mut new_sections: Vec<NewSectionParts> = Vec::new();
    let mut current_parts: Vec<NewSectionPart> = Vec::new();
    for fragment in &content.fragments {
        match fragment {
            ContentFragment::Text(t) => current_parts.push(NewSectionPart::Text(t.clone())),
            ContentFragment::StyledText { text, marks } => {
                current_parts.push(NewSectionPart::StyledText {
                    text: text.clone(),
                    marks: *marks,
                })
            }
            ContentFragment::NewHyperlink { href, anchor, text } => {
                current_parts.push(NewSectionPart::Link(NewHyperlinkAtom {
                    href: href.clone(),
                    anchor: anchor.clone(),
                    display: text.clone(),
                }));
            }
            ContentFragment::PreservedInlineRef(_) => {
                new_sections.push(NewSectionParts {
                    parts: std::mem::take(&mut current_parts),
                });
            }
        }
    }
    new_sections.push(NewSectionParts {
        parts: current_parts,
    });

    // Now build the new segments.
    let mut new_segments: Vec<TrackedSegment> = Vec::new();
    let mut anchor_idx = 0;

    let last_section_idx = old_sections.len() - 1;
    for (section_idx, old_section) in old_sections.iter().enumerate() {
        let old_text = old_section.flat_text(segments);
        let (new_text, link_atoms, char_marks) = new_sections[section_idx].render_for_diff();

        // Diff this section
        let diff_tokens = diff_text_sections(&old_text, &new_text);

        // Outside formatting context applies only at the outer edges: the
        // first section's left, the last section's right.
        let initial_left = if section_idx == 0 {
            outer_left.clone()
        } else {
            None
        };
        let fallback_right = if section_idx == last_section_idx {
            outer_right.clone()
        } else {
            None
        };

        // Reconstruct segments from diff output for this section.
        // `link_atoms` is consulted by the Insert handler: any
        // placeholder codepoint inside an Insert token splits into
        // [pre-text TextNode, link OpaqueInline, post-text TextNode]
        // sharing the same revision.
        reconstruct_section_segments(
            &diff_tokens,
            old_section,
            segments,
            &link_atoms,
            &char_marks,
            initial_left,
            fallback_right,
            revision,
            enclosing_insertion,
            rev_counter,
            &mut new_segments,
        );

        // Emit trailing decorations from this old section
        for &(_seg_idx, _inline_idx, inline) in &old_section.trailing_decorations {
            append_inline_to_segments(&mut new_segments, inline.clone(), TrackingStatus::Normal);
        }

        // Emit the anchor between this section and the next (if any)
        if anchor_idx < anchors_between.len() {
            let (_, _, anchor_inline) = anchors_between[anchor_idx];
            append_inline_to_segments(
                &mut new_segments,
                anchor_inline.clone(),
                TrackingStatus::Normal,
            );
            anchor_idx += 1;
        }
    }

    new_segments
}

/// A new-section made up of plain text runs and atomic hyperlink
/// insertions. Built from the LLM's replacement content fragments
/// between preserved-inline anchors.
#[derive(Clone, Debug)]
struct NewSectionParts {
    parts: Vec<NewSectionPart>,
}

#[derive(Clone, Debug)]
enum NewSectionPart {
    Text(String),
    /// Authored text carrying inline marks (bold/italic/…). The marks ride the
    /// word-diff via a per-char map aligned to the rendered diff string, so a
    /// surgical edit to a formatted run keeps a minimal redline AND its marks.
    StyledText {
        text: String,
        marks: InlineMarkSet,
    },
    Link(NewHyperlinkAtom),
}

impl NewSectionParts {
    /// Render to flat text for diffing, replacing each link with a
    /// single private-use codepoint. The diff algorithm sees the
    /// placeholder as a character that does not appear in the old text,
    /// so it always emits the placeholder inside an Insert token. The
    /// reconstructor maps each placeholder back to the corresponding
    /// `NewHyperlinkAtom`.
    ///
    /// The returned `Vec<NewHyperlinkAtom>` is indexed by
    /// `placeholder_codepoint - HYPERLINK_PLACEHOLDER_BASE`.
    /// Also returns a per-CHAR authored-mark map aligned to the rendered string
    /// (`None` for plain text and link placeholders, `Some` for styled runs), so
    /// the reconstructor can stamp inserted text with its authored marks.
    fn render_for_diff(&self) -> (String, Vec<NewHyperlinkAtom>, Vec<Option<InlineMarkSet>>) {
        let mut text = String::new();
        let mut atoms: Vec<NewHyperlinkAtom> = Vec::new();
        let mut char_marks: Vec<Option<InlineMarkSet>> = Vec::new();
        for part in &self.parts {
            match part {
                NewSectionPart::Text(t) => {
                    for ch in t.chars() {
                        text.push(ch);
                        char_marks.push(None);
                    }
                }
                NewSectionPart::StyledText { text: t, marks } => {
                    for ch in t.chars() {
                        text.push(ch);
                        char_marks.push(Some(*marks));
                    }
                }
                NewSectionPart::Link(atom) => {
                    let code = HYPERLINK_PLACEHOLDER_BASE + atoms.len() as u32;
                    assert!(
                        code <= HYPERLINK_PLACEHOLDER_MAX,
                        "more than {} hyperlinks in a single text section — \
                         exceeds private-use codepoint allocation",
                        HYPERLINK_PLACEHOLDER_MAX - HYPERLINK_PLACEHOLDER_BASE + 1
                    );
                    text.push(
                        char::from_u32(code)
                            .expect("placeholder codepoint is in the BMP private-use area"),
                    );
                    char_marks.push(None);
                    atoms.push(atom.clone());
                }
            }
        }
        (text, atoms, char_marks)
    }
}

/// Look up a placeholder codepoint's atom index. Returns `None` for
/// codepoints outside the placeholder range so the caller can treat
/// them as ordinary inserted characters.
fn placeholder_atom_index(ch: char, atom_count: usize) -> Option<usize> {
    let code = ch as u32;
    if (HYPERLINK_PLACEHOLDER_BASE..HYPERLINK_PLACEHOLDER_BASE + atom_count as u32).contains(&code)
    {
        Some((code - HYPERLINK_PLACEHOLDER_BASE) as usize)
    } else {
        None
    }
}

/// A section of old paragraph content between two anchor boundaries.
struct OldSection<'a> {
    /// The text inlines in this section: (segment_idx, inline_idx, &InlineNode)
    text_inlines: Vec<(usize, usize, &'a InlineNode)>,
    /// Decorations that appeared between the last text and the following anchor
    trailing_decorations: Vec<(usize, usize, &'a InlineNode)>,
}

impl<'a> OldSection<'a> {
    /// Get the concatenated ACCEPT-ALL text of this section — Normal + Inserted
    /// runs only. Prior tombstones (Deleted/stacked) are skipped so the diff runs
    /// against the accept-all view (matching what the client sent); the cursor
    /// skips them in lockstep and passes them through verbatim.
    fn flat_text(&self, original_segments: &[TrackedSegment]) -> String {
        let mut s = String::new();
        for &(seg_idx, _, inline) in &self.text_inlines {
            if matches!(
                original_segments[seg_idx].status,
                TrackingStatus::Deleted(_) | TrackingStatus::InsertedThenDeleted(_)
            ) {
                continue;
            }
            if let InlineNode::Text(t) = inline {
                s.push_str(&t.text);
            }
        }
        s
    }
}

/// Reconstruct TrackedSegments for one text section from diff output.
///
/// Walks the diff tokens and the original inline structure in parallel,
/// splitting original TextNodes at token boundaries as needed.
///
/// `link_atoms` carries the per-section hyperlink atoms whose display
/// text was replaced with private-use placeholders before diffing.
/// When an Insert token contains a placeholder codepoint, this function
/// splits it into surrounding TextNodes plus a freshly-synthesized
/// `OpaqueInline{Hyperlink}` at the placeholder's position. Old-side
/// tokens (Equal/Delete) never contain placeholders by construction.
///
/// `initial_left` / `fallback_right` are formatting contexts from text
/// OUTSIDE this section's input (a splice's carried head/tail); they apply
/// only when the section itself offers no kept/deleted neighbour to inherit
/// from.
#[allow(clippy::too_many_arguments)]
fn reconstruct_section_segments(
    diff_tokens: &[DiffToken],
    old_section: &OldSection,
    original_segments: &[TrackedSegment],
    link_atoms: &[NewHyperlinkAtom],
    // Per-char authored marks aligned to the rendered NEW text (the same string
    // the diff ran on). Inserted/Equal tokens advance `new_cursor` over it so an
    // Insert token can be stamped with the marks the author put on that text.
    char_marks: &[Option<InlineMarkSet>],
    initial_left: Option<FormattingContext>,
    fallback_right: Option<FormattingContext>,
    revision: &RevisionInfo,
    // The pending block insertion the target paragraph is ITSELF part of, if any
    // (same-transaction or a prior tracked `InsertParagraphs`). Its `Normal`
    // segments are not base content — see the `Delete` arm.
    enclosing_insertion: Option<&RevisionInfo>,
    rev_counter: &mut u32,
    output: &mut Vec<TrackedSegment>,
) {
    // Build a cursor into the old text nodes
    let mut old_cursor = OldTextCursor::new(old_section);
    // Char cursor into the NEW text (advances on Equal + Insert, like the diff's).
    let mut new_cursor = 0usize;

    // Scan for right-side formatting context (first kept/deleted token),
    // falling back to context carried in from outside the section.
    let right_context = find_right_formatting_context(diff_tokens, old_section, original_segments)
        .or(fallback_right);
    let mut left_context: Option<FormattingContext> = initial_left;

    // Walk diff tokens and build segments
    for token in diff_tokens {
        match token.op {
            DiffOp::Equal => {
                // Kept text: extract from original TextNodes, preserving
                // formatting. Output status follows the ORIGIN:
                // Normal text stays Normal; text inside the author's own
                // pending insertion stays Inserted under its ORIGINAL
                // revision — the pending change keeps its identity.
                let inlines =
                    old_cursor.consume_chars(token.text.chars().count(), original_segments);

                // Update left formatting context from the last kept node
                if let Some((InlineNode::Text(t), _)) = inlines.last() {
                    left_context = Some(FormattingContext {
                        marks: t.marks.clone(),
                        style_props: t.style_props.clone(),
                        rpr_authored: t.rpr_authored,
                    });
                }

                for (inline, origin) in inlines {
                    // The origin status is preserved verbatim — including a
                    // prior tombstone the cursor passed through: the new edit
                    // keeps this region, so the pending change stays pending
                    // (never accepted for the user).
                    append_inline_to_segments(output, inline, origin);
                }
                // Equal text is present in the NEW string — advance the new cursor.
                new_cursor += token.text.chars().count();
            }
            DiffOp::Delete => {
                // Deleted text: extract from original TextNodes. Origin decides
                // the outcome: Normal text gets a Deleted
                // tombstone (Word's reject restores it); text removed from the
                // author's own pending insertion is DROPPED outright — it never
                // existed in the base, so a tombstone would corrupt reject-all.
                let inlines =
                    old_cursor.consume_chars(token.text.chars().count(), original_segments);
                // Minted lazily: a delete that only trims own-insertion text
                // produces no tombstone and should burn no revision id.
                let mut del_rev: Option<RevisionInfo> = None;

                // Update left formatting context
                if let Some((InlineNode::Text(t), _)) = inlines.last() {
                    left_context = Some(FormattingContext {
                        marks: t.marks.clone(),
                        style_props: t.style_props.clone(),
                        rpr_authored: t.rpr_authored,
                    });
                }

                for (inline, origin) in inlines {
                    // The pending-insertion revision this deleted text belongs to,
                    // if any: an explicitly `Inserted` segment carries its own; a
                    // `Normal` segment inside a paragraph that is ITSELF a pending
                    // block insertion (`enclosing_insertion`) belongs to that block
                    // insertion — it exists ONLY by virtue of it and was never in
                    // the base. Both delete the same way (own → un-propose, cross →
                    // stacked); a plain `Deleted` tombstone would be "restored" on
                    // reject-all (§17.13.5.20), leaking text that never existed.
                    let insertion_origin = match &origin {
                        TrackingStatus::Inserted(ins_rev) => Some(ins_rev.clone()),
                        TrackingStatus::Normal => enclosing_insertion.cloned(),
                        // A prior tombstone the cursor passed through, sitting
                        // inside the range this edit deletes — preserve it verbatim,
                        // untouched by the new deletion.
                        TrackingStatus::Deleted(_) | TrackingStatus::InsertedThenDeleted(_) => {
                            append_inline_to_segments(output, inline, origin);
                            continue;
                        }
                    };
                    match insertion_origin {
                        // Genuine base content: a deletion Word's reject restores.
                        None => {
                            let rev = del_rev
                                .get_or_insert_with(|| next_revision(revision, rev_counter))
                                .clone();
                            append_inline_to_segments(output, inline, TrackingStatus::Deleted(rev));
                        }
                        // Text that exists only by a pending insertion (an explicit
                        // Inserted segment, or Normal content inside a pending block
                        // insertion) being removed by this edit.
                        Some(ins_rev) => {
                            // Author identity is exact byte equality; an
                            // anonymous revision never matches — never
                            // un-propose what you cannot prove is
                            // yours.
                            let own = matches!(
                                (revision.author.as_deref(), ins_rev.author.as_deref()),
                                (Some(editing), Some(owner)) if editing == owner
                            );
                            if own {
                                // Removing text from one's OWN pending
                                // insertion un-proposes it: no tombstone —
                                // the text never existed in the base
                                // (step 2 rule).
                            } else {
                                // Cross-author: B deleting A's pending
                                // insertion produces the STACKED state —
                                // "a deletion remembers what it deletes".
                                // Both revisions stay
                                // pending; resolution follows the four
                                // origin rules.
                                let del = del_rev
                                    .get_or_insert_with(|| next_revision(revision, rev_counter))
                                    .clone();
                                append_inline_to_segments(
                                    output,
                                    inline,
                                    TrackingStatus::InsertedThenDeleted(Box::new(
                                        crate::domain::StackedRevision {
                                            inserted: ins_rev,
                                            deleted: del,
                                        },
                                    )),
                                );
                            }
                        }
                    }
                }
            }
            DiffOp::Insert => {
                // Inserted text: create new TextNode(s) with inherited
                // formatting. If the inserted text contains hyperlink
                // placeholder codepoints, split at each placeholder and
                // emit a synthesized `OpaqueInline{Hyperlink}` in place.
                // All emitted inlines share the same revision (one
                // user-visible insert action).
                let fmt = resolve_formatting_for_insert(&left_context, &right_context);
                let rev = next_revision(revision, rev_counter);
                let status = TrackingStatus::Inserted(rev);
                let n = token.text.chars().count();
                let lo = new_cursor.min(char_marks.len());
                let hi = (new_cursor + n).min(char_marks.len());
                emit_insert_token(
                    &token.text,
                    &char_marks[lo..hi],
                    link_atoms,
                    &fmt,
                    &status,
                    rev_counter,
                    output,
                );
                new_cursor += n;
            }
        }
    }

    // Emit any prior tombstones that trail the last consumed accept-all char
    // (e.g. a deletion at the very end of the paragraph) — they're preserved, not
    // accepted.
    for (inline, status) in old_cursor.flush_tombstones(original_segments) {
        append_inline_to_segments(output, inline, status);
    }

    // Any decorations interspersed with text that weren't picked up by
    // the cursor should be emitted. The cursor handles decorations within
    // text nodes, but decorations at the boundaries (before first text /
    // after last text) are in the OldSection's trailing_decorations and
    // handled by the caller.
}

/// Find the formatting context from the first kept/deleted token in a section.
/// Used as fallback when there's no left context (insertion at section start).
fn find_right_formatting_context(
    diff_tokens: &[DiffToken],
    old_section: &OldSection,
    original_segments: &[TrackedSegment],
) -> Option<FormattingContext> {
    // Find the first Equal or Delete token — that's from old content
    for token in diff_tokens {
        if token.op == DiffOp::Equal || token.op == DiffOp::Delete {
            // The first old-content token's formatting comes from the first
            // text node in the section
            if let Some(&(seg_idx, inline_idx, _)) = old_section.text_inlines.first()
                && let InlineNode::Text(t) = &original_segments[seg_idx].inlines[inline_idx]
            {
                return Some(FormattingContext {
                    marks: t.marks.clone(),
                    style_props: t.style_props.clone(),
                    rpr_authored: t.rpr_authored,
                });
            }
            break;
        }
    }
    None
}

/// Cursor into the old text nodes of a section.
/// Tracks current position as (index into text_inlines, char offset within that TextNode).
struct OldTextCursor<'a> {
    text_inlines: &'a [(usize, usize, &'a InlineNode)],
    /// Current index into text_inlines.
    node_idx: usize,
    /// Current char offset within the current TextNode.
    char_offset: usize,
}

impl<'a> OldTextCursor<'a> {
    fn new(section: &'a OldSection<'a>) -> Self {
        OldTextCursor {
            text_inlines: &section.text_inlines,
            node_idx: 0,
            char_offset: 0,
        }
    }

    /// Consume `num_chars` characters from the old text, returning new InlineNodes
    /// that represent exactly those characters with original formatting, each
    /// paired with the tracking status of the segment it ORIGINATED in. The
    /// origin drives the splice's status mapping: kept/deleted
    /// text from a Normal segment and from the author's own pending insertion
    /// resolve differently.
    ///
    /// When a TextNode must be split, the resulting halves each get a NodeId:
    /// - the first fragment of a TextNode keeps the original NodeId
    /// - subsequent fragments get a derived NodeId
    fn consume_chars(
        &mut self,
        mut num_chars: usize,
        original_segments: &[TrackedSegment],
    ) -> Vec<(InlineNode, TrackingStatus)> {
        let mut result = Vec::new();

        while num_chars > 0 && self.node_idx < self.text_inlines.len() {
            let (seg_idx, inline_idx, _) = self.text_inlines[self.node_idx];
            let origin_status = original_segments[seg_idx].status.clone();
            let original_text_node = match &original_segments[seg_idx].inlines[inline_idx] {
                InlineNode::Text(t) => t,
                _ => unreachable!("text_inlines should only contain Text nodes"),
            };

            // A prior tombstone (Deleted/stacked) is NOT part of the accept-all
            // text the diff aligned against — pass it through verbatim at its
            // boundary, consuming no diff char, so re-editing the paragraph never
            // accepts a pending change. (flat_text skips these too, so the diff
            // string and this walk stay in lockstep on the accept-all text.)
            if matches!(
                origin_status,
                TrackingStatus::Deleted(_) | TrackingStatus::InsertedThenDeleted(_)
            ) {
                result.push((InlineNode::Text(original_text_node.clone()), origin_status));
                self.node_idx += 1;
                self.char_offset = 0;
                continue;
            }

            let node_chars: Vec<char> = original_text_node.text.chars().collect();
            let remaining_in_node = node_chars.len() - self.char_offset;
            let take = num_chars.min(remaining_in_node);

            let slice: String = node_chars[self.char_offset..self.char_offset + take]
                .iter()
                .collect();

            // Determine NodeId: if we're taking a prefix starting from 0 and
            // consuming the full node, keep original ID. If splitting, derive.
            let id = if self.char_offset == 0 && take == node_chars.len() {
                // Taking the whole node
                original_text_node.id.clone()
            } else if self.char_offset == 0 {
                // Taking the first portion — keep original ID
                original_text_node.id.clone()
            } else {
                // Taking a middle or tail portion — derive ID
                NodeId::new(format!(
                    "{}_split_{}",
                    original_text_node.id.0, self.char_offset
                ))
            };

            // A WHOLE kept run preserves its tracked formatting change (rPrChange):
            // an unrelated edit elsewhere in the paragraph must not silently ACCEPT
            // the pending format change (the same principle as preserving prior
            // del/ins on a re-edit). A split portion drops it (which sub-run owns the
            // run-level change is ambiguous; first-ship keeps it simple).
            let formatting_change = if self.char_offset == 0 && take == node_chars.len() {
                original_text_node.formatting_change.clone()
            } else {
                None
            };
            result.push((
                InlineNode::from(TextNode {
                    id,
                    text_role: None,
                    text: slice,
                    marks: original_text_node.marks.clone(),
                    style_props: original_text_node.style_props.clone(),
                    rpr_authored: original_text_node.rpr_authored,
                    source_run_attrs: original_text_node.source_run_attrs.clone(),
                    // A whole kept run preserves its tracked formatting change; a
                    // split portion drops it (computed above).
                    formatting_change,
                }),
                origin_status.clone(),
            ));

            self.char_offset += take;
            num_chars -= take;

            if self.char_offset >= node_chars.len() {
                self.node_idx += 1;
                self.char_offset = 0;
            }
        }

        result
    }

    /// Emit any prior tombstones (Deleted/stacked) that trail the last consumed
    /// accept-all char (e.g. a deletion at the very end of the paragraph, which no
    /// diff token reaches). Returns them verbatim with their original status so
    /// the pending change survives. Stops at the first non-tombstone (which would
    /// mean the diff failed to consume all accept-all text — never expected).
    fn flush_tombstones(
        &mut self,
        original_segments: &[TrackedSegment],
    ) -> Vec<(InlineNode, TrackingStatus)> {
        let mut result = Vec::new();
        while self.node_idx < self.text_inlines.len() {
            let (seg_idx, inline_idx, _) = self.text_inlines[self.node_idx];
            let status = original_segments[seg_idx].status.clone();
            if !matches!(
                status,
                TrackingStatus::Deleted(_) | TrackingStatus::InsertedThenDeleted(_)
            ) {
                break;
            }
            if let InlineNode::Text(t) = &original_segments[seg_idx].inlines[inline_idx] {
                result.push((InlineNode::Text(t.clone()), status));
            }
            self.node_idx += 1;
            self.char_offset = 0;
        }
        result
    }
}

/// Append an inline node to the output segments, merging into the last
/// segment if the tracking status matches.
fn append_inline_to_segments(
    segments: &mut Vec<TrackedSegment>,
    inline: InlineNode,
    status: TrackingStatus,
) {
    if let Some(last) = segments.last_mut()
        && last.status == status
    {
        last.inlines.push(inline);
        return;
    }
    segments.push(TrackedSegment {
        status,
        inlines: vec![inline],
    });
}

/// Create the next revision, incrementing the counter.
///
/// The apply_op_id is propagated from the base so every tracked change a
/// single `apply_edit` call produces shares the same group identifier.
/// Emit one diff-Insert token's inlines into `output`, splitting at
/// hyperlink placeholder codepoints. All emitted inlines share the
/// provided `status` (and thus the same revision) so the resulting
/// segments merge into one Inserted block.
#[allow(clippy::too_many_arguments)]
fn emit_insert_token(
    token_text: &str,
    char_marks: &[Option<InlineMarkSet>],
    link_atoms: &[NewHyperlinkAtom],
    fmt: &FormattingContext,
    status: &TrackingStatus,
    rev_counter: &mut u32,
    output: &mut Vec<TrackedSegment>,
) {
    let mut buffer = String::new();
    // The authored marks for the chars currently buffered (homogeneous): we flush
    // and start a fresh run whenever the per-char authored marks change, so each
    // inserted run carries exactly its own marks.
    let mut buffer_marks: Option<InlineMarkSet> = None;
    for (i, ch) in token_text.chars().enumerate() {
        let marks = char_marks.get(i).copied().flatten();
        if let Some(idx) = placeholder_atom_index(ch, link_atoms.len()) {
            // Flush any buffered plain text as a TextNode insert first.
            if !buffer.is_empty() {
                output_inserted_text_node(
                    std::mem::take(&mut buffer),
                    fmt,
                    buffer_marks,
                    status.clone(),
                    rev_counter,
                    output,
                );
            }
            buffer_marks = None;
            // Synthesize the hyperlink at the placeholder position.
            let atom = &link_atoms[idx];
            let link_node_id = NodeId::new(format!("edit_link_{}", *rev_counter));
            *rev_counter += 1;
            let link_inline = synthesize_new_hyperlink_inline(
                link_node_id,
                &atom.href,
                &atom.anchor,
                &atom.display,
            );
            append_inline_to_segments(output, link_inline, status.clone());
        } else {
            // Mark boundary → flush the homogeneous run before starting the next.
            if !buffer.is_empty() && marks != buffer_marks {
                output_inserted_text_node(
                    std::mem::take(&mut buffer),
                    fmt,
                    buffer_marks,
                    status.clone(),
                    rev_counter,
                    output,
                );
            }
            if buffer.is_empty() {
                buffer_marks = marks;
            }
            buffer.push(ch);
        }
    }
    if !buffer.is_empty() {
        output_inserted_text_node(
            buffer,
            fmt,
            buffer_marks,
            status.clone(),
            rev_counter,
            output,
        );
    }
}

fn output_inserted_text_node(
    text: String,
    fmt: &FormattingContext,
    authored_marks: Option<InlineMarkSet>,
    status: TrackingStatus,
    rev_counter: &mut u32,
    output: &mut Vec<TrackedSegment>,
) {
    // Start from the inherited formatting context, then layer the author's own
    // marks on top (the same union the segment-replace exemplar path uses), so a
    // surgically-inserted run keeps both its context and its authored marks.
    let mut marks = fmt.marks.clone();
    let mut style_props = fmt.style_props.clone();
    let mut rpr_authored = fmt.rpr_authored;
    if let Some(ov) = authored_marks {
        apply_mark_overrides(&mut marks, &mut style_props, ov);
        claim_authored_marks(&mut rpr_authored, ov);
    }
    let text_node = InlineNode::from(TextNode {
        id: NodeId::new(format!("edit_{}", *rev_counter)),
        text_role: None,
        text,
        // Keep the override-applied local marks/style_props (authored_marks path),
        // and carry main's per-slot run-rPr provenance widened by the marks this
        // edit itself authored.
        marks,
        style_props,
        rpr_authored,
        source_run_attrs: Vec::new(),
        formatting_change: None,
    });
    *rev_counter += 1;
    append_inline_to_segments(output, text_node, status);
}

/// Layer an authored `InlineMarkSet` on top of an existing mark/style set: add
/// the boolean marks if absent and lift `strike` to a style-prop. Shared by the
/// surgical insert path (`output_inserted_text_node`) and the segment-replace
/// exemplar builder so the two can never drift in what a mark intent produces.
fn apply_mark_overrides(marks: &mut Vec<Mark>, style_props: &mut StyleProps, ov: InlineMarkSet) {
    for (enabled, mark) in [
        (ov.bold, Mark::Bold),
        (ov.italic, Mark::Italic),
        (ov.underline, Mark::Underline),
        (ov.subscript, Mark::Subscript),
        (ov.superscript, Mark::Superscript),
    ] {
        if enabled && !marks.contains(&mark) {
            marks.push(mark);
        }
    }
    if ov.strike {
        style_props.strike = MarkValue::On;
    }
}

/// Claim per-slot run-rPr provenance for the marks an edit just authored via an
/// [`InlineMarkSet`]. Without this the serializer's directness filter
/// (`direct_marks`) treats the added mark as style-inherited and never emits it
/// — the run silently loses its bold/italic/etc. on export. Add-only by
/// construction: an `InlineMarkSet` cannot express authored-OFF (`w:b w:val="0"`
/// is the documented presence-only `Vec<Mark>` residue), so this only ever
/// widens the claim.
fn claim_authored_marks(rpr_authored: &mut crate::domain::RunRprAuthored, ov: InlineMarkSet) {
    rpr_authored.bold |= ov.bold;
    rpr_authored.italic |= ov.italic;
    rpr_authored.underline |= ov.underline;
    rpr_authored.vert_align |= ov.subscript || ov.superscript;
    rpr_authored.strike |= ov.strike;
    rpr_authored.caps |= ov.caps;
    rpr_authored.small_caps |= ov.small_caps;
}

/// SET (not merge) the kept-text-reformat mark surface — bold/italic/underline/
/// subscript/superscript (boolean Marks) and strike (a style prop) — to exactly
/// `target`: ADD a mark the target wants, REMOVE one it doesn't. This is the only
/// path in the engine that can CLEAR a mark (`apply_marks`/`apply_mark_overrides`
/// are add-only), so it is what lets a formatting-only replace express un-bolding a
/// word. It rewrites EXACTLY the six properties `marks_set_of` lifts and nothing
/// else — caps/small_caps/color/font/highlight/spacing are outside the detector's
/// surface, so touching them would diverge the applier from the detector. Strike is
/// only forced off when it was explicitly On, so an inherited (Inherit) strike on an
/// untouched run is preserved (no spurious rPrChange).
fn set_mark_surface(marks: &mut Vec<Mark>, style_props: &mut StyleProps, target: InlineMarkSet) {
    for (enabled, mark) in [
        (target.bold, Mark::Bold),
        (target.italic, Mark::Italic),
        (target.underline, Mark::Underline),
        (target.subscript, Mark::Subscript),
        (target.superscript, Mark::Superscript),
    ] {
        if enabled {
            if !marks.contains(&mark) {
                marks.push(mark);
            }
        } else {
            marks.retain(|m| *m != mark);
        }
    }
    if target.strike {
        style_props.strike = MarkValue::On;
    } else if style_props.strike == MarkValue::On {
        style_props.strike = MarkValue::Off;
    }
}

fn next_revision(base: &RevisionInfo, counter: &mut u32) -> RevisionInfo {
    let rev = RevisionInfo {
        revision_id: *counter,
        identity: 0,
        author: base.author.clone(),
        date: base.date.clone(),
        apply_op_id: base.apply_op_id.clone(),
    };
    *counter += 1;
    rev
}

// ─── Phase 4: Normalize ─────────────────────────────────────────────────────

/// Normalize the segment list:
///
/// 1. Merge adjacent segments with identical TrackingStatus (including RevisionInfo).
/// 2. Drop empty segments (no inlines).
///
/// Text-node boundaries are deliberately preserved. They correspond to source
/// `w:r` boundaries, which can affect Word's line layout even when neighbouring
/// runs have identical modeled formatting.
///
/// This segment-normalization pass is shared by both materializers (Invariant M,
/// domain-model §6). The edit path has always run it; the merge path now runs it
/// too, as the final pass, after field-coalescing and opaque reading-order
/// normalization, so those passes still see the unmerged segment boundaries
/// they rely on.
pub(crate) fn normalize_segments(segments: &mut Vec<TrackedSegment>) {
    // Merge adjacent segments with identical status while retaining every
    // inline boundary in order.
    let mut merged: Vec<TrackedSegment> = Vec::new();
    for segment in segments.drain(..) {
        if segment.inlines.is_empty() {
            continue; // Drop empty segments (I7)
        }
        if let Some(last) = merged.last_mut()
            && last.status == segment.status
        {
            last.inlines.extend(segment.inlines);
            continue;
        }
        merged.push(segment);
    }

    *segments = merged;
}

#[derive(Default)]
struct InsertOrderState {
    by_anchor: std::collections::HashMap<NodeId, NodeId>,
    /// Blocks turned into a moveFrom source by an earlier step of the
    /// CURRENT transaction, keyed by their (still-live) original id. A later
    /// step's destination anchor resolving to one of these keys is the
    /// chained-anchor bug (`EditError::AmbiguousAnchorAfterMove`) — see
    /// `check_destination_anchor_not_moved`.
    moved_this_transaction: std::collections::HashMap<NodeId, MovedSourceInfo>,
}

/// Where a moveFrom source's content actually landed, recorded when the
/// step that moved it ran. See `InsertOrderState::moved_this_transaction`.
struct MovedSourceInfo {
    step_index: usize,
    copy_block_id: NodeId,
}

fn unique_inserted_block_id(blocks: &[TrackedBlock], original_id: &NodeId) -> NodeId {
    if find_block_index(blocks, original_id).is_none() {
        return original_id.clone();
    }
    let mut suffix = 1usize;
    loop {
        let candidate = NodeId::from(format!("{}__ins{}", original_id.0, suffix));
        if find_block_index(blocks, &candidate).is_none() {
            return candidate;
        }
        suffix += 1;
    }
}

/// Collect every block-addressable `NodeId` in `blocks` — top-level blocks plus
/// all table-nested blocks, rows, and cells — into `used`. Block ids must be
/// unique across the whole document (`find_paragraph_path` resolves an id to the
/// first match in document order), so a freshly cloned subtree has to avoid
/// colliding with any of these.
fn collect_block_node_ids(block: &BlockNode, used: &mut std::collections::HashSet<NodeId>) {
    match block {
        BlockNode::Paragraph(p) => {
            used.insert(p.id.clone());
        }
        BlockNode::OpaqueBlock(o) => {
            used.insert(o.id.clone());
        }
        BlockNode::Table(t) => {
            used.insert(t.id.clone());
            for row in &t.rows {
                used.insert(row.id.clone());
                for cell in &row.cells {
                    used.insert(cell.id.clone());
                    for b in &cell.blocks {
                        collect_block_node_ids(b, used);
                    }
                }
            }
        }
    }
}

/// Pick an id not already in `used`, derived from `original` (verbatim if free,
/// else `original__ins1`, `__ins2`, …), and register it.
fn fresh_unique_id(original: &NodeId, used: &mut std::collections::HashSet<NodeId>) -> NodeId {
    let chosen = if used.contains(original) {
        let mut suffix = 1usize;
        loop {
            let candidate = NodeId::from(format!("{}__ins{}", original.0, suffix));
            if !used.contains(&candidate) {
                break candidate;
            }
            suffix += 1;
        }
    } else {
        original.clone()
    };
    used.insert(chosen.clone());
    chosen
}

/// Rewrite EVERY block-addressable id in a freshly cloned block — its own id and,
/// for a table, all nested row/cell/cell-block ids recursively — to values not in
/// `used`, registering each. Returns the block's new top-level id.
///
/// Renaming only the top-level id (the old behavior) left a moved table's nested
/// cell-paragraph ids identical to the still-present Deleted source, so an edit
/// targeting a cell resolved to the shadow and vanished on accept (P0 #3).
fn reassign_cloned_block_ids(
    block: &mut BlockNode,
    used: &mut std::collections::HashSet<NodeId>,
) -> NodeId {
    let new_top = fresh_unique_id(block_id_of(block), used);
    match block {
        BlockNode::Paragraph(p) => p.id = new_top.clone(),
        BlockNode::OpaqueBlock(o) => o.id = new_top.clone(),
        BlockNode::Table(t) => {
            t.id = new_top.clone();
            for row in &mut t.rows {
                row.id = fresh_unique_id(&row.id, used);
                for cell in &mut row.cells {
                    cell.id = fresh_unique_id(&cell.id, used);
                    for b in &mut cell.blocks {
                        reassign_cloned_block_ids(b, used);
                    }
                }
            }
        }
    }
    new_top
}

fn resolve_after_anchor(anchor_id: &NodeId, order_state: &InsertOrderState) -> NodeId {
    order_state
        .by_anchor
        .get(anchor_id)
        .cloned()
        .unwrap_or_else(|| anchor_id.clone())
}

fn note_after_insert(anchor_id: &NodeId, inserted_id: NodeId, order_state: &mut InsertOrderState) {
    order_state.by_anchor.insert(anchor_id.clone(), inserted_id);
}

fn first_text_formatting(para: &ParagraphNode) -> FormattingContext {
    if let Some(text) = para.first_content_text_node() {
        return FormattingContext {
            marks: text.marks.clone(),
            style_props: text.style_props.clone(),
            rpr_authored: text.rpr_authored,
        };
    }
    FormattingContext {
        marks: Vec::new(),
        style_props: StyleProps::default(),
        rpr_authored: RunRprAuthored::default(),
    }
}

fn find_paragraph_in_block<'a>(
    block: &'a BlockNode,
    paragraph_id: &NodeId,
) -> Option<&'a ParagraphNode> {
    match block {
        BlockNode::Paragraph(p) if &p.id == paragraph_id => Some(p),
        BlockNode::Paragraph(_) => None,
        BlockNode::Table(t) => {
            for row in &t.rows {
                for cell in &row.cells {
                    for child in &cell.blocks {
                        if let Some(p) = find_paragraph_in_block(child, paragraph_id) {
                            return Some(p);
                        }
                    }
                }
            }
            None
        }
        BlockNode::OpaqueBlock(_) => None,
    }
}

fn find_paragraph_anywhere<'a>(
    doc: &'a CanonDoc,
    paragraph_id: &NodeId,
) -> Option<&'a ParagraphNode> {
    for tracked in &doc.blocks {
        if let Some(p) = find_paragraph_in_block(&tracked.block, paragraph_id) {
            return Some(p);
        }
    }
    None
}

/// Collect the `NumberingInfo` of every list paragraph in the document (body +
/// table cells). Used to resolve an inserted paragraph's `list.num_id` against
/// numbering the document ALREADY uses: the `CanonDoc` value does not carry the
/// parsed `word/numbering.xml`, so "this numId exists" means "some existing
/// paragraph references it". This also lets us reuse that sibling's `is_bullet`
/// rather than fabricating it (CLAUDE.md "no silent fallbacks").
fn collect_doc_numbering(doc: &CanonDoc) -> Vec<NumberingInfo> {
    fn walk(block: &BlockNode, out: &mut Vec<NumberingInfo>) {
        match block {
            BlockNode::Paragraph(p) => {
                if let Some(n) = &p.numbering {
                    out.push(n.clone());
                }
            }
            BlockNode::Table(t) => {
                for row in &t.rows {
                    for cell in &row.cells {
                        for child in &cell.blocks {
                            walk(child, out);
                        }
                    }
                }
            }
            BlockNode::OpaqueBlock(_) => {}
        }
    }
    let mut out = Vec::new();
    for tracked in &doc.blocks {
        walk(&tracked.block, &mut out);
    }
    out
}

/// Synthesize a brand-new hyperlink `InlineNode` from the LLM's
/// `<link href=".../anchor=...">text</link>` markup. The hyperlink has
/// a single Normal run carrying the display text and no per-run rPr
/// (formatting inherits from the surrounding paragraph). The rId for
/// external URLs is left as `None` and gets allocated by the serializer's
/// `resolve_rel_rid` callback at export time, so relationship management
/// is invisible to the LLM and to this code path.
///
/// The opaque carries an empty `raw_xml` (the serializer rebuilds the
/// element from `HyperlinkData`) and a generated `opaque_ref` based on
/// the node id so downstream code can address it.
fn synthesize_new_hyperlink_inline(
    id: NodeId,
    href: &Option<String>,
    anchor: &Option<String>,
    text: &str,
) -> InlineNode {
    use crate::domain::{
        DocPart, HyperlinkData, HyperlinkRun, OpaqueInlineNode, ProofRef, StyleProps,
    };
    let data = HyperlinkData {
        url: href.clone(),
        anchor: anchor.clone(),
        text: text.to_string(),
        // rId is allocated at export time by the serializer's
        // resolve_rel_rid callback. We leave None so the serializer
        // knows this is a fresh hyperlink needing a new relationship.
        r_id: None,
        runs: vec![HyperlinkRun {
            text: text.to_string(),
            rpr_xml: None,
            source_run_attrs: Vec::new(),
            status: TrackingStatus::Normal,
        }],
        extra_attrs: vec![],
    };
    InlineNode::from(OpaqueInlineNode {
        id: id.clone(),
        kind: OpaqueKind::Hyperlink(data),
        opaque_ref: format!("hyperlink_{}", id.0),
        proof_ref: ProofRef {
            part: DocPart::DocumentXml,
            block_id: id.clone(),
            docx_anchor: String::new(),
        },
        wrapper_marks: Vec::new(),
        wrapper_style_props: StyleProps::default(),
        raw_xml: None,
        content_hash: None,
    })
}

/// Build a TextNode from an exemplar's formatting plus an optional
/// LLM-specified mark override. `id` is unique per-node; `base_fmt` is
/// the exemplar's run formatting (marks + StyleProps + direct-flag
/// hints); `text` is the literal text content; `overrides` is the
/// LLM-provided universal marks to union onto the exemplar marks.
fn build_text_node_from_exemplar(
    id: NodeId,
    base_fmt: &FormattingContext,
    text: String,
    overrides: Option<InlineMarkSet>,
) -> TextNode {
    let mut marks = base_fmt.marks.clone();
    let mut style_props = base_fmt.style_props.clone();
    let mut rpr_authored = base_fmt.rpr_authored;
    if let Some(ov) = overrides {
        apply_mark_overrides(&mut marks, &mut style_props, ov);
        // An overridden mark is DIRECT formatting on the synthesized run:
        // promote its provenance or the serializer's authored-mark filter
        // (direct_marks) would strip what the caller explicitly authored.
        claim_authored_marks(&mut rpr_authored, ov);
    }
    TextNode {
        id,
        text_role: None,
        text,
        marks,
        style_props,
        rpr_authored,
        source_run_attrs: Vec::new(),
        formatting_change: None,
    }
}

/// Aliases a cold agent can pass instead of a document-specific role id when it
/// just wants "a normal body paragraph". These resolve to the document's default
/// body role (`vocabulary::default_body_role_id`) — the most frequent
/// non-numbered role. This is a *documented, visible* default (CLAUDE.md: a
/// product-approved default must be intentional and named), not a silent
/// fallback: if the document has no body role to map to, resolution still fails
/// loud.
const DEFAULT_ROLE_ALIASES: &[&str] = &["default", "body"];

/// Resolve a role string against a document's vocabulary into the `ParagraphRole`
/// an insert/replace will clone formatting from. Accepts either an exact role id
/// (the token the read view surfaces as `role_token`) or one of
/// [`DEFAULT_ROLE_ALIASES`]. On an unknown role, fails loud with the available
/// role ids so the retry loop can correct.
fn resolve_role_entry<'v>(
    doc: &CanonDoc,
    vocab: &'v crate::vocabulary::DocumentVocabulary,
    role: &str,
    step_index: usize,
) -> Result<&'v crate::vocabulary::ParagraphRole, EditError> {
    let target_id: String = if DEFAULT_ROLE_ALIASES.contains(&role) {
        crate::vocabulary::default_body_role_id(doc).ok_or_else(|| {
            EditError::ParagraphRoleNotFound {
                role: format!(
                    "{role} (no default body role: document has no projectable paragraph role)"
                ),
                step_index,
            }
        })?
    } else {
        role.to_string()
    };
    vocab
        .paragraph_roles
        .iter()
        .find(|r| r.id == target_id)
        .ok_or_else(|| {
            let available: Vec<&str> =
                vocab.paragraph_roles.iter().map(|r| r.id.as_str()).collect();
            EditError::ParagraphRoleNotFound {
                role: format!(
                    "{role} (available roles: {}; or pass \"default\"/\"body\" for the document's body role)",
                    available.join(", ")
                ),
                step_index,
            }
        })
}

fn resolve_paragraph_spec(
    doc: &CanonDoc,
    spec: &ParagraphBlockSpec,
    step_index: usize,
) -> Result<BlockNode, EditError> {
    // Content is already parsed by the wire-format edge (v3 markup parser or
    // v4 adapter). Inserts produce brand-new content so `<opaque>` / `<anchor>`
    // references are invalid here (they point at existing nodes, and inserts
    // have no source paragraph).
    let content = &spec.content;
    for fragment in &content.fragments {
        if matches!(fragment, ContentFragment::PreservedInlineRef(_)) {
            return Err(EditError::UnsupportedParagraphStructure {
                block_id: NodeId::from("<insert>".to_string()),
                reason: "inserted paragraphs cannot reference existing preserved inlines via \
                     <opaque .../> — insert operations create new paragraphs with no source"
                    .to_string(),
                step_index,
            });
        }
    }

    let role = spec
        .role
        .as_ref()
        .ok_or_else(|| EditError::UnsupportedParagraphRole {
            role: "<none>".to_string(),
            reason: "inserted paragraphs currently require an explicit role".to_string(),
            step_index,
        })?;

    let vocab = extract_vocabulary(doc);
    let role_entry = resolve_role_entry(doc, &vocab, role, step_index)?;

    // Literal-prefix roles ARE supported for insertion: the inserted
    // paragraph inherits the exemplar's prefix (e.g., "1.") as a
    // placeholder, and `adjust_literal_prefixes_after_insert` reassigns
    // it based on the insert position and renumbers downstream
    // siblings. If the exemplar's prefix is in an unsupported format
    // (roman numerals, hierarchical "1.1", etc.) the adjust pass emits
    // an actionable error so the LLM retry loop can correct.
    //
    // `restart_numbering` still only applies to Word auto-numbering —
    // literal-prefix roles encode their position textually, so there
    // is no counter to restart.
    if spec.restart_numbering
        && !(role_entry.has_numbering && role_entry.numbering_source == Some(NumberingSource::Auto))
    {
        return Err(EditError::UnsupportedNumberingRestart {
            role: Some(role.clone()),
            step_index,
        });
    }

    let exemplar = find_paragraph_anywhere(doc, &role_entry.exemplar).ok_or_else(|| {
        EditError::UnsupportedParagraphRole {
            role: role.clone(),
            reason: format!("exemplar paragraph '{}' not found", role_entry.exemplar),
            step_index,
        }
    })?;

    let fmt = first_text_formatting(exemplar);
    let mut para = exemplar.clone();
    para.segments = if content.fragments.is_empty() {
        Vec::new()
    } else {
        // Walk the parsed fragments and build one TextNode per Text /
        // StyledText fragment. Each StyledText fragment carries an
        // `InlineMarkSet` we union onto the exemplar's base formatting.
        let mut inlines: Vec<InlineNode> = Vec::with_capacity(content.fragments.len());
        for (idx, fragment) in content.fragments.iter().enumerate() {
            let node_id = NodeId::from(format!("{}_t{idx}", para.id.0));
            match fragment {
                ContentFragment::Text(t) => {
                    inlines.push(InlineNode::from(build_text_node_from_exemplar(
                        node_id,
                        &fmt,
                        t.clone(),
                        None,
                    )));
                }
                ContentFragment::StyledText { text, marks } => {
                    inlines.push(InlineNode::from(build_text_node_from_exemplar(
                        node_id,
                        &fmt,
                        text.clone(),
                        Some(*marks),
                    )));
                }
                ContentFragment::PreservedInlineRef(_) => {
                    // Already rejected above — unreachable here.
                    unreachable!(
                        "PreservedInlineRef in insert content should have been rejected earlier"
                    );
                }
                ContentFragment::NewHyperlink { href, anchor, text } => {
                    inlines.push(synthesize_new_hyperlink_inline(node_id, href, anchor, text));
                }
            }
        }
        if inlines.is_empty() {
            Vec::new()
        } else {
            normal_segment(inlines)
        }
    };
    para.block_text_hash = None;
    para.rendered_text = None;
    para.para_mark_status = None;
    para.para_split = false;
    para.section_property_change = None;
    para.formatting_change = None;
    strip_position_bound_state(&mut para);

    if role_entry.has_numbering
        && role_entry.numbering_source == Some(NumberingSource::Auto)
        && let Some(ref mut numbering) = para.numbering
    {
        numbering.synthesized_text.clear();
        // Reject restart on bullet numbering: bullets have no counter, so
        // "restart" is meaningless. This mirrors the "no silent fallback"
        // rule from CLAUDE.md — surface the mismatch rather than quietly
        // dropping the flag.
        if spec.restart_numbering && numbering.is_bullet {
            return Err(EditError::UnsupportedNumberingRestart {
                role: Some(role.clone()),
                step_index,
            });
        }
        numbering.restart_numbering = spec.restart_numbering;
    }

    // Explicit list membership: when the caller supplied `list: {num_id, ilvl}`,
    // author the paragraph's `w:numPr` directly at that level instead of
    // inheriting the role exemplar's numbering. This is what lets an agent
    // create a list sub-point as a SINGLE tracked insert (no follow-up
    // `set_numbering`, which is refused on a freshly-inserted paragraph).
    //
    // `num_id` must reference numbering the document already uses — the engine
    // never fabricates a `word/numbering.xml` definition. We resolve it against
    // existing list paragraphs and fail loud on an unknown numId, reusing a
    // sibling's `is_bullet` rather than guessing it. The displayed label is
    // re-derived by Word from numbering.xml at the target level, so
    // `synthesized_text` is left empty (the live numPr carries only numId/ilvl).
    if let Some(list) = &spec.list {
        let doc_numbering = collect_doc_numbering(doc);
        let sibling = doc_numbering.iter().find(|n| n.num_id == list.num_id);
        let Some(sibling) = sibling else {
            let mut available: Vec<u32> = doc_numbering.iter().map(|n| n.num_id).collect();
            available.sort_unstable();
            available.dedup();
            return Err(EditError::InsertListNumIdUnknown {
                requested: list.num_id,
                available,
                step_index,
            });
        };
        para.numbering = Some(NumberingInfo {
            num_id: list.num_id,
            ilvl: list.ilvl,
            synthesized_text: String::new(),
            is_bullet: sibling.is_bullet,
            restart_numbering: false,
        });
        // An inserted list item authors its own direct numPr.
        para.has_direct_numbering = true;
    }

    Ok(BlockNode::from(para))
}

fn toc_field_spec(spec: &TocBlockSpec) -> TocFieldSpec {
    TocFieldSpec {
        levels: spec.levels,
        include_hyperlinks: spec.include_hyperlinks,
        hide_page_numbers_in_web: spec.hide_page_numbers_in_web,
        use_outline_levels: spec.use_outline_levels,
    }
}

fn resolve_toc_spec(
    doc: &CanonDoc,
    spec: &TocBlockSpec,
    step_index: usize,
) -> Result<BlockNode, EditError> {
    // Product default: a ToC insert with no explicit `role` resolves against
    // the document's default body paragraph role, via the same `"default"`
    // alias `resolve_role_entry` already accepts for a paragraph insert
    // (`DEFAULT_ROLE_ALIASES`). The v4 wire never asks the caller for an
    // internal role token to insert a ToC (see `edit_v4::translate_insert_blocks`'s
    // `Block::Toc` arm) — an explicit role stays available for callers that
    // construct a `TocBlockSpec` directly and want a specific exemplar.
    let role = spec.role.as_deref().unwrap_or("default");

    let vocab = extract_vocabulary(doc);
    let role_entry = resolve_role_entry(doc, &vocab, role, step_index)?;
    let exemplar = find_paragraph_anywhere(doc, &role_entry.exemplar).ok_or_else(|| {
        EditError::UnsupportedParagraphRole {
            role: role.to_string(),
            reason: format!("exemplar paragraph '{}' not found", role_entry.exemplar),
            step_index,
        }
    })?;

    let mut para = exemplar.clone();
    let field_id = NodeId::from(format!("{}_toc0", para.id.0));
    let semantic = toc_field_spec(spec);
    let instruction_text = semantic.instruction_text();
    let field = InlineNode::from(crate::domain::OpaqueInlineNode {
        id: field_id,
        kind: OpaqueKind::Field(FieldData {
            field_kind: FieldKind::Simple,
            instruction_text: Some(instruction_text),
            result_text: None,
            semantic: Some(FieldSemantic::Toc(semantic)),
        }),
        opaque_ref: format!("generated_toc_{}", para.id.0),
        proof_ref: crate::domain::ProofRef {
            part: crate::domain::DocPart::DocumentXml,
            block_id: para.id.clone(),
            docx_anchor: String::new(),
        },
        wrapper_marks: Vec::new(),
        wrapper_style_props: StyleProps::default(),
        raw_xml: None,
        content_hash: None,
    });
    para.segments = normal_segment(vec![field]);
    para.block_text_hash = None;
    para.rendered_text = None;
    para.para_mark_status = None;
    para.para_split = false;
    para.section_property_change = None;
    para.formatting_change = None;
    strip_position_bound_state(&mut para);
    Ok(BlockNode::from(para))
}

/// Strip position-bound, one-place-only state from a paragraph cloned off a
/// role exemplar so it can be inserted elsewhere.
///
/// An inserted paragraph inherits the exemplar's FORMATTING, never its
/// position-bound state. The clone copies every pPr field; these identify the
/// exemplar's SLOT in the document rather than its look, so a new paragraph
/// must never carry them:
///
/// * `section_properties` — a mid-document `w:sectPr`. When the chosen exemplar
///   is a section-final paragraph it carries the break that ends its section
///   (pgSz/pgMar/headerReference/footerReference). Cloning it makes every
///   inserted paragraph a NEW section break — observed as duplicated
///   `<w:sectPr>` blocks on a plain "insert two list items". On the body path
///   this ships silently (the referenced rels resolve); inside a table cell it
///   fails loud at serialize with I-REL-001 because cell-level footerReference
///   rels are unregistered. A new paragraph is never a section boundary.
/// * `para_id` / `text_id` — `w14:paraId` / `w14:textId` are meant to be
///   document-unique identities (commentsExtended threading, editor identity).
///   A clone would emit a duplicate id; drop them so the inserted paragraph has
///   no stale identity (the attribute is optional; Word regenerates one on next
///   save).
///
/// Deliberately KEPT (formatting the insert is meant to inherit): style_id,
/// align/indent/spacing/borders/shading, numbering (may be overridden by
/// `spec.list`), outline_lvl/heading_level, the literal-prefix placeholder
/// fields (`adjust_literal_prefixes_after_insert` reassigns them), and the
/// paragraph-mark formatting. Inline decorations (bookmark/comment ranges) live
/// in `segments`, which the caller rebuilds, so they never survive the clone.
fn strip_position_bound_state(para: &mut ParagraphNode) {
    para.section_properties = None;
    para.para_id = None;
    para.text_id = None;
}

fn resolve_block_spec(
    doc: &CanonDoc,
    spec: &BlockSpec,
    step_index: usize,
) -> Result<BlockNode, EditError> {
    match spec {
        BlockSpec::Paragraph(paragraph) => resolve_paragraph_spec(doc, paragraph, step_index),
        BlockSpec::Toc(toc) => resolve_toc_spec(doc, toc, step_index),
        BlockSpec::Table(table) => resolve_table_spec(doc, table, step_index),
    }
}

/// Resolve a `TableBlockSpec` into a fresh `BlockNode::Table`.
///
/// Pre-conditions (fail-fast — see CLAUDE.md "no silent fallbacks"):
/// - `spec.rows` non-empty
/// - every row has at least one cell
/// - every cell has at least one block
///
/// All rows, cells, and the table itself get fresh IDs derived from a
/// per-call counter. The caller (`apply_replace_table` /
/// `apply_insert_paragraphs`) may rename the root table id to avoid
/// collisions; row and cell ids inherit from that root via the
/// `{root}_r{N}_c{M}` pattern, so renaming the root is enough — descendant
/// ids stay unique because the root prefix changed.
///
/// The resulting `TableNode` uses default `TableFormatting` (no style,
/// default `tbl_look`, empty borders) and default `CellFormatting` — the v4
/// spec carries content + merge structure but no formatting. For a
/// `replace(table)`, `apply_replace_table` then overlays the BASE table's
/// formatting onto this target (`carry_base_formatting_onto_target`) so the
/// replace round-trips it (RFC-0003); for a bare `insert(table)` there is no
/// base to carry from, so the new table simply has no direct formatting.
fn resolve_table_spec(
    doc: &CanonDoc,
    spec: &TableBlockSpec,
    step_index: usize,
) -> Result<BlockNode, EditError> {
    if spec.rows.is_empty() {
        return Err(EditError::EmptyTableStructure { step_index });
    }
    // Merge-grid validity (rectangular logical grid, no orphan vMerge
    // continue). Runs before we build the IR so a malformed merge spec fails
    // loudly with a row/cell-addressed error rather than reaching the diff /
    // materializer (where the failure would be an opaque canonicalization
    // error). See `verbs::tables_merged`.
    verbs::tables_merged::validate_merge_spec(spec, step_index)?;
    // ID base: a stable placeholder. `apply_insert_paragraphs` /
    // `apply_replace_table` will reassign the root id at insert time so
    // distinct inserted tables don't collide; the descendants keep their
    // structural suffixes.
    let root_id = NodeId::from("__edit_table".to_string());

    let mut rows: Vec<TableRowNode> = Vec::with_capacity(spec.rows.len());
    for (row_index, row_spec) in spec.rows.iter().enumerate() {
        if row_spec.cells.is_empty() {
            return Err(EditError::EmptyRowContent {
                step_index,
                row_index,
            });
        }
        let row_id = NodeId::from(format!("{}_r{row_index}", root_id.0));
        let mut cells: Vec<TableCellNode> = Vec::with_capacity(row_spec.cells.len());
        for (cell_index, cell_spec) in row_spec.cells.iter().enumerate() {
            if cell_spec.content.is_empty() {
                return Err(EditError::EmptyCellContent {
                    step_index,
                    row_index,
                    cell_index,
                });
            }
            let cell_id = NodeId::from(format!("{}_r{row_index}_c{cell_index}", root_id.0));
            // Recursively resolve the cell's block children. Renames each
            // resolved child's id to a deterministic path so nested tables
            // and paragraphs don't collide across cells.
            let mut blocks: Vec<BlockNode> = Vec::with_capacity(cell_spec.content.len());
            for (block_in_cell_idx, child_spec) in cell_spec.content.iter().enumerate() {
                let mut child = resolve_block_spec(doc, child_spec, step_index)?;
                let child_id = NodeId::from(format!("{}_b{block_in_cell_idx}", cell_id.0));
                match &mut child {
                    BlockNode::Paragraph(p) => p.id = child_id,
                    BlockNode::Table(t) => t.id = child_id,
                    BlockNode::OpaqueBlock(o) => o.id = child_id,
                }
                blocks.push(child);
            }
            // Populate merge state from the spec. `merge_h` (gridSpan) defaults
            // to 1 (single column); `merge_v` maps 1:1 onto the IR's
            // `VerticalMerge`. Both are emitted by the serializer as-is and
            // adopted on matched cells by `apply_table_structure_changed`.
            let grid_span = cell_spec.merge_h.unwrap_or(1).max(1);
            let v_merge = match cell_spec.merge_v {
                None => VerticalMerge::None,
                Some(VerticalMergeSpec::Restart) => VerticalMerge::Restart,
                Some(VerticalMergeSpec::Continue) => VerticalMerge::Continue,
            };
            cells.push(TableCellNode {
                id: cell_id,
                blocks,
                grid_span,
                v_merge,
                // Caller-specified cell tcPr (RFC-0003 Item 1) or unformatted.
                formatting: cell_spec.formatting.clone().unwrap_or_default(),
                formatting_change: None,
                tracking_status: None,
                row_sdt_wrapper: None,
                content_sdt_wraps: Vec::new(),
                cnf_style: None,
                hide_mark: false,
                preserved: Vec::new(),
            });
        }
        rows.push(TableRowNode {
            id: row_id,
            cells,
            grid_before: 0,
            grid_after: 0,
            tracking_status: None,
            is_header: row_spec.is_header,
            // Caller-specified row height (RFC-0003 Item 1).
            height: row_spec.height,
            height_rule: row_spec.height_rule.clone(),
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
        });
    }

    let structure_hash = crate::import::compute_table_structure_hash(&rows);
    Ok(BlockNode::from(TableNode {
        id: root_id,
        rows,
        structure_hash,
        // Caller-specified table-level tblPr (RFC-0003 Item 1) or default.
        formatting: spec.formatting.clone().unwrap_or_default(),
        formatting_change: None,
    }))
}

// ─── Literal-prefix label assignment ────────────────────────────────────────
//
// When inserting a paragraph whose role uses literal-prefix numbering
// (the number is baked into the paragraph text, e.g. "1.", "(a)"),
// `resolve_paragraph_spec` clones the exemplar's prefix as a
// placeholder. After insertion we run `adjust_literal_prefixes_after_insert`
// to reassign that placeholder to a label that matches the insert
// position — the nearest preceding same-format sibling's number + 1.
//
// We intentionally DO NOT renumber subsequent siblings. Literal
// prefixes are plain paragraph text: mutating them in place would be
// an untracked change that reject-all could not undo. If the caller
// wants a cascading renumber, they can chain `ReplaceParagraphText`
// steps for each affected paragraph — each becomes its own tracked
// change and participates properly in accept/reject. This matches
// Word's own behavior: literal-numbered lists don't auto-renumber
// when you type a new item mid-list.
//
// Supported formats (parsed from the exemplar's `literal_prefix`):
// - Arabic integers with optional bracketing punctuation: "1.", "1)",
//   "(1)", "[1]".
// - Single ASCII letter with optional bracketing: "a.", "a)", "(a)",
//   "A.", "A)", "(A)".
//
// Hierarchical ("1.1", "1.1.2"), roman ("i.", "II)"), and custom
// formats fail with a clear error that tells the caller/LLM what's
// supported — the retry loop (or user) can rephrase toward a
// recognized format or a non-numbered edit.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LabelKind {
    Arabic,
    LowerLetter,
    UpperLetter,
}

#[derive(Clone, PartialEq, Eq, Debug)]
struct LabelFormat {
    kind: LabelKind,
    prefix_chars: String,
    suffix_chars: String,
}

fn parse_literal_prefix_label(raw: &str) -> Option<(LabelFormat, u32)> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    let chars: Vec<char> = s.chars().collect();
    // Eat a leading run of non-alphanumeric "decoration" chars (e.g. "(", "[").
    let mut i = 0;
    while i < chars.len() && !chars[i].is_ascii_alphanumeric() {
        i += 1;
    }
    let prefix_chars: String = chars[..i].iter().collect();
    if i >= chars.len() {
        return None;
    }
    let counter_start = i;
    if chars[i].is_ascii_digit() {
        while i < chars.len() && chars[i].is_ascii_digit() {
            i += 1;
        }
        // Reject hybrids like "1a." or hierarchical "1.1" — the suffix
        // run must not contain another alphanumeric.
        let suffix: String = chars[i..].iter().collect();
        if suffix.chars().any(|c| c.is_ascii_alphanumeric()) {
            return None;
        }
        let number: u32 = chars[counter_start..i]
            .iter()
            .collect::<String>()
            .parse()
            .ok()?;
        Some((
            LabelFormat {
                kind: LabelKind::Arabic,
                prefix_chars,
                suffix_chars: suffix,
            },
            number,
        ))
    } else if chars[i].is_ascii_alphabetic() {
        // Single letter only — reject multi-char letters (roman "ii",
        // abbreviations "ab.") because there's no unambiguous increment
        // rule.
        let letter = chars[i];
        i += 1;
        let suffix: String = chars[i..].iter().collect();
        if suffix.chars().any(|c| c.is_ascii_alphanumeric()) {
            return None;
        }
        let kind = if letter.is_ascii_lowercase() {
            LabelKind::LowerLetter
        } else {
            LabelKind::UpperLetter
        };
        let number = letter_to_number(letter)?;
        Some((
            LabelFormat {
                kind,
                prefix_chars,
                suffix_chars: suffix,
            },
            number,
        ))
    } else {
        None
    }
}

fn letter_to_number(c: char) -> Option<u32> {
    let lower = c.to_ascii_lowercase();
    if lower.is_ascii_lowercase() {
        Some((lower as u32) - ('a' as u32) + 1)
    } else {
        None
    }
}

fn number_to_letter(n: u32, kind: LabelKind) -> Option<char> {
    if n == 0 || n > 26 {
        return None;
    }
    let base = match kind {
        LabelKind::LowerLetter => 'a',
        LabelKind::UpperLetter => 'A',
        LabelKind::Arabic => return None,
    };
    char::from_u32((base as u32) + n - 1)
}

fn render_literal_prefix_label(format: &LabelFormat, number: u32) -> Option<String> {
    match format.kind {
        LabelKind::Arabic => Some(format!(
            "{}{}{}",
            format.prefix_chars, number, format.suffix_chars
        )),
        LabelKind::LowerLetter | LabelKind::UpperLetter => {
            let letter = number_to_letter(number, format.kind)?;
            Some(format!(
                "{}{}{}",
                format.prefix_chars, letter, format.suffix_chars
            ))
        }
    }
}

/// After `apply_insert_paragraphs[_direct]` has placed every new
/// paragraph into the document, reassign labels on literal-prefix
/// paragraphs and renumber subsequent siblings in the same sequence.
///
/// `inserted_indices` are the final document-order indices of the
/// newly inserted paragraphs. They must be in ascending order so that
/// each pass operates on a doc whose earlier inserts already carry
/// their final labels.
fn adjust_literal_prefixes_after_insert(
    doc: &mut CanonDoc,
    inserted_indices: &[usize],
    step_index: usize,
) -> Result<(), EditError> {
    let pending: std::collections::HashSet<usize> = inserted_indices.iter().copied().collect();
    let mut processed: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for &idx in inserted_indices {
        adjust_one_literal_prefix_insert(doc, idx, &pending, &processed, step_index)?;
        processed.insert(idx);
    }
    Ok(())
}

fn adjust_one_literal_prefix_insert(
    doc: &mut CanonDoc,
    idx: usize,
    pending: &std::collections::HashSet<usize>,
    processed: &std::collections::HashSet<usize>,
    step_index: usize,
) -> Result<(), EditError> {
    // Only literal-prefix paragraphs need adjustment — other inserts
    // were already finalized by `resolve_paragraph_spec`.
    let exemplar_prefix = match &doc.blocks[idx].block {
        BlockNode::Paragraph(p) => p.literal_prefix.clone(),
        _ => return Ok(()),
    };
    let Some(exemplar_prefix) = exemplar_prefix else {
        return Ok(());
    };
    let Some((format, _)) = parse_literal_prefix_label(&exemplar_prefix) else {
        return Err(EditError::UnsupportedParagraphRole {
            role: "<insert>".to_string(),
            reason: format!(
                "cannot parse literal-prefix label {exemplar_prefix:?} — supported formats are \
                 Arabic ('1.', '(1)', '1)') and single ASCII letter ('a.', 'a)', '(a)')"
            ),
            step_index,
        });
    };

    // Find the anchor: the nearest preceding paragraph in the same
    // format that is NOT a pending unprocessed insert (those are
    // placeholder-labeled until we process them) and NOT a deleted
    // tracked block (which wouldn't be visible in the final doc).
    let anchor_number = nearest_preceding_same_format_number(doc, idx, &format, pending, processed);

    // The inserted paragraph's number is anchor+1, or 1 if there is no
    // preceding sibling in this format (we're starting a new sequence).
    let new_number = anchor_number.map(|n| n + 1).unwrap_or(1);
    let new_label = render_literal_prefix_label(&format, new_number).ok_or_else(|| {
        EditError::UnsupportedParagraphRole {
            role: "<insert>".to_string(),
            reason: format!(
                "cannot render label for number {new_number} in this format (out of range for \
                 single-letter sequences)"
            ),
            step_index,
        }
    })?;
    if let BlockNode::Paragraph(p) = &mut doc.blocks[idx].block {
        p.literal_prefix = Some(new_label);
    }

    Ok(())
}

fn nearest_preceding_same_format_number(
    doc: &CanonDoc,
    idx: usize,
    format: &LabelFormat,
    pending: &std::collections::HashSet<usize>,
    processed: &std::collections::HashSet<usize>,
) -> Option<u32> {
    for i in (0..idx).rev() {
        // Skip pending inserts that haven't been processed yet — their
        // label is still the exemplar placeholder, not a real position.
        if pending.contains(&i) && !processed.contains(&i) {
            continue;
        }
        if matches!(doc.blocks[i].status, TrackingStatus::Deleted(_)) {
            continue;
        }
        let BlockNode::Paragraph(p) = &doc.blocks[i].block else {
            continue;
        };
        let Some(raw) = &p.literal_prefix else {
            continue;
        };
        let Some((prev_format, number)) = parse_literal_prefix_label(raw) else {
            continue;
        };
        if prev_format == *format {
            return Some(number);
        }
    }
    None
}

fn apply_delete_block_range(
    doc: &mut CanonDoc,
    start: usize,
    end: usize,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
) {
    for tracked_block in &mut doc.blocks[start..=end] {
        tracked_block.status = TrackingStatus::Deleted(next_revision(revision, rev_counter));
        if let BlockNode::Paragraph(p) = &mut tracked_block.block {
            p.para_mark_status = Some(TrackingStatus::Deleted(next_revision(
                revision,
                rev_counter,
            )));
        }
    }
}

fn apply_delete_block_range_direct(doc: &mut CanonDoc, start: usize, end: usize) {
    doc.blocks.drain(start..=end);
}

struct ApplyBlockContext<'a> {
    step_index: usize,
    revision: &'a RevisionInfo,
    rev_counter: &'a mut u32,
    order_state: &'a mut InsertOrderState,
}

/// Find the moveTo copy in `doc` paired with the moveFrom source
/// `source_block_id`, which shares `move_id` with it.
///
/// `apply_move_block_range` moves a CONTIGUOUS run of source blocks to a
/// contiguous run of destination clones, inserted in the same relative
/// order and all sharing one `move_id` (see its doc comment). So the Nth
/// `Deleted` block carrying `move_id` pairs with the Nth `Inserted` block
/// carrying `move_id`. `doc` must already reflect the move (both halves
/// present).
fn move_destination_copy_id(
    doc: &CanonDoc,
    move_id: &str,
    source_block_id: &NodeId,
) -> Option<NodeId> {
    let position = doc
        .blocks
        .iter()
        .filter(|tb| {
            tb.move_id.as_deref() == Some(move_id)
                && matches!(tb.status, TrackingStatus::Deleted(_))
        })
        .position(|tb| block_id_of(&tb.block) == source_block_id)?;
    doc.blocks
        .iter()
        .filter(|tb| {
            tb.move_id.as_deref() == Some(move_id)
                && matches!(tb.status, TrackingStatus::Inserted(_))
        })
        .nth(position)
        .map(|tb| block_id_of(&tb.block).clone())
}

/// Refuse a structural op's destination anchor (`move`'s `destination`,
/// `insert`'s `target`) when it names a block that is a tracked-move
/// SOURCE — see `EditError::AmbiguousAnchorAfterMove`. Two ways an anchor
/// ends up in that state:
///
/// 1. An earlier step of THIS transaction moved it — recorded in
///    `ctx.order_state.moved_this_transaction` by `apply_move_block_range`
///    when the move happens, so the step index and the copy id are known
///    directly.
/// 2. It is already a `Deleted` + `move_id` shadow in `doc` (a previously
///    committed move, or one imported from DOCX) — checked by looking the
///    anchor up directly; the copy id comes from `move_destination_copy_id`.
///
/// Checks (1) before falling back to (2): the registry check must win when
/// both would match, otherwise a same-transaction move would be misreported
/// as "already a moveFrom shadow in the document" and lose its step index.
///
/// Does NOT flag anchors that are merely `Deleted` (a plain tracked
/// delete/replace has no `move_id`) — the anchor-after-just-deleted-range
/// pattern `apply_structural_replace` relies on stays untouched.
fn check_destination_anchor_not_moved(
    doc: &CanonDoc,
    anchor_id: &NodeId,
    ctx: &ApplyBlockContext<'_>,
) -> Result<(), EditError> {
    if let Some(info) = ctx.order_state.moved_this_transaction.get(anchor_id) {
        return Err(EditError::AmbiguousAnchorAfterMove {
            anchor_id: anchor_id.clone(),
            moved_by_step_index: Some(info.step_index),
            moved_to_block_id: Some(info.copy_block_id.clone()),
            step_index: ctx.step_index,
        });
    }
    let Some(idx) = find_block_index(&doc.blocks, anchor_id) else {
        return Ok(());
    };
    let tracked_block = &doc.blocks[idx];
    if !matches!(tracked_block.status, TrackingStatus::Deleted(_)) {
        return Ok(());
    }
    let Some(move_id) = tracked_block.move_id.clone() else {
        return Ok(());
    };
    // A move authored by this engine always pairs source and copy, but a
    // dirty IMPORT can carry an unpaired `w:moveFrom` (the importer tags
    // blocks as encountered; it does not validate pairing) — so the copy
    // hint is best-effort and the refusal must not depend on it.
    let moved_to_block_id = move_destination_copy_id(doc, &move_id, anchor_id);
    Err(EditError::AmbiguousAnchorAfterMove {
        anchor_id: anchor_id.clone(),
        moved_by_step_index: None,
        moved_to_block_id,
        step_index: ctx.step_index,
    })
}

fn apply_insert_paragraphs(
    doc: &mut CanonDoc,
    anchor_block_id: &NodeId,
    position: InsertPosition,
    blocks: &[BlockSpec],
    ctx: &mut ApplyBlockContext<'_>,
) -> Result<(), EditError> {
    check_destination_anchor_not_moved(doc, anchor_block_id, ctx)?;

    let mut resolved_blocks = Vec::with_capacity(blocks.len());
    for block in blocks {
        resolved_blocks.push(resolve_block_spec(doc, block, ctx.step_index)?);
    }

    let effective_anchor = match position {
        InsertPosition::After => resolve_after_anchor(anchor_block_id, ctx.order_state),
        InsertPosition::Before => anchor_block_id.clone(),
    };
    let anchor_idx = find_block_index(&doc.blocks, &effective_anchor).ok_or_else(|| {
        EditError::BlockNotFound {
            block_id: effective_anchor.clone(),
            step_index: ctx.step_index,
        }
    })?;

    let start_idx = match position {
        InsertPosition::Before => anchor_idx,
        InsertPosition::After => anchor_idx + 1,
    };

    let mut inserted_indices = Vec::with_capacity(resolved_blocks.len());
    let mut last_inserted_id = None;
    for (insert_idx, mut block) in (start_idx..).zip(resolved_blocks) {
        let block_id = unique_inserted_block_id(&doc.blocks, block_id_of(&block));
        match &mut block {
            BlockNode::Paragraph(p) => p.id = block_id.clone(),
            BlockNode::Table(t) => t.id = block_id.clone(),
            BlockNode::OpaqueBlock(o) => o.id = block_id.clone(),
        }
        doc.blocks.insert(
            insert_idx,
            TrackedBlock {
                status: TrackingStatus::Inserted(next_revision(ctx.revision, ctx.rev_counter)),
                block,
                move_id: None,
                block_sdt_wrap: None,
            },
        );
        inserted_indices.push(insert_idx);
        last_inserted_id = Some(block_id);
    }

    adjust_literal_prefixes_after_insert(doc, &inserted_indices, ctx.step_index)?;

    if position == InsertPosition::After
        && let Some(last_inserted_id) = last_inserted_id
    {
        note_after_insert(anchor_block_id, last_inserted_id, ctx.order_state);
    }

    Ok(())
}

fn apply_insert_paragraphs_direct(
    doc: &mut CanonDoc,
    anchor_block_id: &NodeId,
    position: InsertPosition,
    blocks: &[BlockSpec],
    order_state: &mut InsertOrderState,
    step_index: usize,
) -> Result<(), EditError> {
    let mut resolved_blocks = Vec::with_capacity(blocks.len());
    for block in blocks {
        resolved_blocks.push(resolve_block_spec(doc, block, step_index)?);
    }

    let effective_anchor = match position {
        InsertPosition::After => resolve_after_anchor(anchor_block_id, order_state),
        InsertPosition::Before => anchor_block_id.clone(),
    };
    let anchor_idx = find_block_index(&doc.blocks, &effective_anchor).ok_or_else(|| {
        EditError::BlockNotFound {
            block_id: effective_anchor.clone(),
            step_index,
        }
    })?;

    let start_idx = match position {
        InsertPosition::Before => anchor_idx,
        InsertPosition::After => anchor_idx + 1,
    };

    let mut inserted_indices = Vec::with_capacity(resolved_blocks.len());
    let mut last_inserted_id = None;
    for (insert_idx, mut block) in (start_idx..).zip(resolved_blocks) {
        let block_id = unique_inserted_block_id(&doc.blocks, block_id_of(&block));
        match &mut block {
            BlockNode::Paragraph(p) => p.id = block_id.clone(),
            BlockNode::Table(t) => t.id = block_id.clone(),
            BlockNode::OpaqueBlock(o) => o.id = block_id.clone(),
        }
        doc.blocks.insert(
            insert_idx,
            TrackedBlock {
                status: TrackingStatus::Normal,
                block,
                move_id: None,
                block_sdt_wrap: None,
            },
        );
        inserted_indices.push(insert_idx);
        last_inserted_id = Some(block_id);
    }

    adjust_literal_prefixes_after_insert(doc, &inserted_indices, step_index)?;

    if position == InsertPosition::After
        && let Some(last_inserted_id) = last_inserted_id
    {
        note_after_insert(anchor_block_id, last_inserted_id, order_state);
    }

    Ok(())
}

fn apply_structural_replace(
    doc: &mut CanonDoc,
    from_block_id: &NodeId,
    to_block_id: &NodeId,
    blocks: &[BlockSpec],
    ctx: &mut ApplyBlockContext<'_>,
) -> Result<(), EditError> {
    let (start, end) =
        normalize_block_range_indices(doc, from_block_id, to_block_id, ctx.step_index)?;
    let insert_after_id = block_id_of(&doc.blocks[end].block).clone();
    apply_delete_block_range(doc, start, end, ctx.revision, ctx.rev_counter);
    apply_insert_paragraphs(doc, &insert_after_id, InsertPosition::After, blocks, ctx)
}

fn apply_structural_replace_direct(
    doc: &mut CanonDoc,
    from_block_id: &NodeId,
    to_block_id: &NodeId,
    blocks: &[BlockSpec],
    order_state: &mut InsertOrderState,
    step_index: usize,
) -> Result<(), EditError> {
    let (start, end) = normalize_block_range_indices(doc, from_block_id, to_block_id, step_index)?;
    apply_delete_block_range_direct(doc, start, end);

    let mut resolved_blocks = Vec::with_capacity(blocks.len());
    for block in blocks {
        resolved_blocks.push(resolve_block_spec(doc, block, step_index)?);
    }

    for (insert_idx, mut block) in (start..).zip(resolved_blocks) {
        let block_id = unique_inserted_block_id(&doc.blocks, block_id_of(&block));
        match &mut block {
            BlockNode::Paragraph(p) => p.id = block_id.clone(),
            BlockNode::Table(t) => t.id = block_id.clone(),
            BlockNode::OpaqueBlock(o) => o.id = block_id.clone(),
        }
        doc.blocks.insert(
            insert_idx,
            TrackedBlock {
                status: TrackingStatus::Normal,
                block,
                move_id: None,
                block_sdt_wrap: None,
            },
        );
    }
    order_state.by_anchor.clear();
    Ok(())
}

/// Generate a fresh `move_id` for this step. Scoped per-step — two move
/// steps in the same transaction get distinct ids — because OOXML pairs
/// source and destination via matching `w:id` attributes on
/// `w:moveFromRangeStart` / `w:moveToRangeStart`, and collisions would
/// leave Word unable to reconcile the paired halves.
fn next_move_id(step_index: usize, rev_counter: u32) -> String {
    // Include both the step index and the revision counter so the id
    // is unique within the transaction even if the caller inspects
    // counters mid-apply.
    format!("mv_s{step_index}_r{rev_counter}")
}

/// Apply a `MoveBlockRange` step as paired tracked changes.
///
/// Source blocks are marked `Deleted` and carry `move_id`; deep clones
/// of each source block are inserted at the destination, marked
/// `Inserted` and carrying the same `move_id`. The serializer already
/// knows how to emit `w:moveFromRangeStart/End` around blocks tagged as
/// Deleted-with-move_id and `w:moveToRangeStart/End` around blocks
/// tagged as Inserted-with-move_id (see `serialize_canonical_docx` in
/// `runtime.rs`).
///
/// The destination is remapped to the correct block id after the source
/// range delete (source ids don't shift because we only change their
/// status, not their positions), but the destination-index bookkeeping
/// must account for the destination sitting BELOW the source range —
/// the clones are inserted at `dest_idx` in the current doc order, not
/// at some post-delete-compacted index.
#[allow(clippy::too_many_arguments)]
fn apply_move_block_range(
    doc: &mut CanonDoc,
    from_block_id: &NodeId,
    to_block_id: &NodeId,
    dest_anchor_id: &NodeId,
    dest_position: InsertPosition,
    expect: Option<&str>,
    semantic_hash: Option<&str>,
    ctx: &mut ApplyBlockContext<'_>,
) -> Result<(), EditError> {
    validate_move_expect(doc, from_block_id, expect, semantic_hash, ctx.step_index)?;
    check_destination_anchor_not_moved(doc, dest_anchor_id, ctx)?;

    let (start, end) =
        normalize_block_range_indices(doc, from_block_id, to_block_id, ctx.step_index)?;
    let effective_anchor = match dest_position {
        InsertPosition::After => resolve_after_anchor(dest_anchor_id, ctx.order_state),
        InsertPosition::Before => dest_anchor_id.clone(),
    };
    let dest_idx = find_block_index(&doc.blocks, &effective_anchor).ok_or_else(|| {
        EditError::BlockNotFound {
            block_id: effective_anchor.clone(),
            step_index: ctx.step_index,
        }
    })?;

    // Reject destinations that fall inside the source range. Moving a
    // range into itself is undefined in OOXML's paired-move markup
    // (`w:moveFromRange` and `w:moveToRange` would overlap) and the
    // result would corrupt the tracked-change graph.
    if dest_idx >= start && dest_idx <= end {
        return Err(EditError::MoveDestinationInsideSource {
            from_block_id: from_block_id.clone(),
            to_block_id: to_block_id.clone(),
            dest_anchor_id: dest_anchor_id.clone(),
            step_index: ctx.step_index,
        });
    }

    // Validate every source block is editable (Normal status, etc.).
    // This mirrors the checks `validate_delete_step` runs for delete;
    // move has the same constraint because it marks source blocks as
    // Deleted internally.
    for block in &doc.blocks[start..=end] {
        validate_block_is_editable(block, ctx.step_index)?;
    }

    // Allocate one move_id for the pair.
    let move_id = next_move_id(ctx.step_index, *ctx.rev_counter);

    // Clone source blocks BEFORE we mutate them so the destination
    // clones carry the current content, not the soon-to-be-Deleted
    // shadow. Each clone gets a fresh NodeId so it doesn't collide with
    // its source (block ids must be unique in the doc).
    //
    // `source_ids` is captured in the same order so the "Insert the
    // clones" loop below can pair each source's ORIGINAL id with its
    // clone's fresh id — that pairing is what lets a later step's
    // destination anchor resolution refuse an ambiguous anchor
    // (`check_destination_anchor_not_moved`) instead of silently
    // resolving against the source's stale, pre-move position.
    let mut cloned_destinations: Vec<BlockNode> = Vec::with_capacity(end - start + 1);
    let mut source_ids: Vec<NodeId> = Vec::with_capacity(end - start + 1);
    for block in &doc.blocks[start..=end] {
        cloned_destinations.push(block.block.clone());
        source_ids.push(block_id_of(&block.block).clone());
    }

    // Mark source blocks as Deleted with move_id.
    for tracked_block in &mut doc.blocks[start..=end] {
        tracked_block.status =
            TrackingStatus::Deleted(next_revision(ctx.revision, ctx.rev_counter));
        tracked_block.move_id = Some(move_id.clone());
        if let BlockNode::Paragraph(p) = &mut tracked_block.block {
            p.para_mark_status = Some(TrackingStatus::Deleted(next_revision(
                ctx.revision,
                ctx.rev_counter,
            )));
        }
    }

    // Compute destination insert index. Because the source blocks are
    // still present (we only flipped their status), indices don't
    // shift. `dest_idx` is still valid.
    let mut insert_idx = match dest_position {
        InsertPosition::Before => dest_idx,
        InsertPosition::After => dest_idx + 1,
    };

    // Collect every id already live in the doc (top-level AND table-nested) —
    // the Deleted source shadows are still present — so the clones can be
    // rewritten to ids that collide with none of them.
    let mut used_ids: std::collections::HashSet<NodeId> = std::collections::HashSet::new();
    for tb in &doc.blocks {
        collect_block_node_ids(&tb.block, &mut used_ids);
    }

    // Insert the clones at the destination, tagging them Inserted with
    // the same move_id.
    let mut last_inserted_id = None;
    for (mut block, source_id) in cloned_destinations.into_iter().zip(source_ids.iter()) {
        // Rename the clone AND all its nested ids so nothing clashes with the
        // source (still present as a Deleted shadow) or with an earlier clone.
        // Renaming only the top-level id left a moved table's cell-paragraph ids
        // duplicated, so a later edit resolved to the shadow and was lost on
        // accept (P0 #3). `used_ids` accumulates across clones too.
        let new_id = reassign_cloned_block_ids(&mut block, &mut used_ids);
        doc.blocks.insert(
            insert_idx,
            TrackedBlock {
                status: TrackingStatus::Inserted(next_revision(ctx.revision, ctx.rev_counter)),
                block,
                move_id: Some(move_id.clone()),
                block_sdt_wrap: None,
            },
        );
        insert_idx += 1;
        // Record the pairing so a later step in THIS transaction that
        // anchors on `source_id` (now a moveFrom shadow) refuses instead
        // of resolving against the shadow's stale position.
        ctx.order_state.moved_this_transaction.insert(
            source_id.clone(),
            MovedSourceInfo {
                step_index: ctx.step_index,
                copy_block_id: new_id.clone(),
            },
        );
        last_inserted_id = Some(new_id);
    }

    if dest_position == InsertPosition::After
        && let Some(last_inserted_id) = last_inserted_id
    {
        note_after_insert(dest_anchor_id, last_inserted_id, ctx.order_state);
    }

    Ok(())
}

/// Capture the paragraph's current pPr state into a
/// `ParagraphFormattingChange` suitable for serialization as
/// `w:pPrChange` (§17.13.5.29). The child pPr inside a pPrChange
/// element must be a COMPLETE snapshot of the previous state, not a
/// diff — that's the OOXML contract.
///
/// `numbering_explicitly_absent` is true when the paragraph had no
/// numPr at all before the change (vs. having numbering that was
/// then swapped for different numbering). The serializer uses this
/// to emit `numId=0` in the inner pPr so the reject-view numbering
/// state machine correctly skips the paragraph.
fn snapshot_paragraph_formatting(
    p: &ParagraphNode,
    revision: &RevisionInfo,
) -> ParagraphFormattingChange {
    let numbering_explicitly_absent = p.numbering.is_none() && p.literal_prefix.is_none();
    ParagraphFormattingChange {
        revision_id: revision.revision_id,
        identity: 0,
        previous_alignment: p.align.clone(),
        // The pPrChange inner pPr is the previous DIRECT formatting (§17.13.5.29),
        // so snapshot the AUTHORED-direct indent/spacing — not the resolved
        // effective value (which would bake inherited numbering/style into the
        // "before" state and un-round-trip on reject). Fall back to the effective
        // value only for synthesized paragraphs that never populated the authored
        // field.
        previous_indentation: p.authored_indent.clone().or_else(|| p.indent.clone()),
        previous_spacing: p.authored_spacing.clone().or_else(|| p.spacing.clone()),
        previous_numbering: p.numbering.clone(),
        previous_numbering_explicitly_absent: numbering_explicitly_absent,
        previous_style_id: p.style_id.clone(),
        previous_keep_next: p.keep_next,
        previous_keep_lines: p.keep_lines,
        previous_page_break_before: p.page_break_before,
        previous_widow_control: p.widow_control,
        previous_contextual_spacing: p.contextual_spacing,
        previous_shading: p.shading.clone(),
        previous_borders: p.borders.clone(),
        previous_tab_stops: p.tab_stops.clone(),
        previous_literal_prefix_leading_tab_twips: p.literal_prefix_leading_tab_twips,
        previous_literal_prefix_trailing_tab_stop_twips: p.literal_prefix_trailing_tab_stop_twips,
        previous_paragraph_mark_marks: p.paragraph_mark_marks.clone(),
        previous_paragraph_mark_style_props: p.paragraph_mark_style_props.clone(),
        previous_paragraph_mark_rpr_off: p.paragraph_mark_rpr_off,
        previous_text_direction: p.text_direction.clone(),
        previous_text_alignment: p.text_alignment.clone(),
        previous_mirror_indents: p.mirror_indents,
        previous_auto_space_de: p.auto_space_de,
        previous_auto_space_dn: p.auto_space_dn,
        previous_bidi: p.bidi,
        previous_suppress_auto_hyphens: p.suppress_auto_hyphens,
        previous_snap_to_grid: p.snap_to_grid,
        previous_overflow_punct: p.overflow_punct,
        previous_adjust_right_ind: p.adjust_right_ind,
        previous_word_wrap: p.word_wrap,
        previous_frame_pr: p.frame_pr.clone(),
        previous_preserved_ppr: p.preserved_ppr.clone(),
        author: revision.author.clone().unwrap_or_default(),
        date: revision.date.clone(),
    }
}

/// Snapshot a cell's CURRENT formatting into a `CellFormattingChange` — the
/// "before" state of a `w:tcPrChange` (§17.13.5.37). Mirrors the field mapping
/// the diff classifier uses (`tracked_model.rs::apply_cell_formatting_change`),
/// so an authored change is indistinguishable from one Word produced. Called
/// BEFORE mutating the cell so the snapshot captures the complete prior `tcPr`.
fn snapshot_cell_formatting(cell: &TableCellNode, revision: &RevisionInfo) -> CellFormattingChange {
    CellFormattingChange {
        revision_id: revision.revision_id,
        identity: 0,
        previous_width: cell.formatting.width.clone(),
        previous_borders: cell.formatting.borders.clone(),
        previous_shading: cell.formatting.shading.clone(),
        previous_v_align: cell.formatting.v_align.clone(),
        previous_margins: cell.formatting.margins.clone(),
        previous_no_wrap: cell.formatting.no_wrap,
        previous_text_direction: cell.formatting.text_direction.clone(),
        previous_tc_fit_text: cell.formatting.tc_fit_text,
        author: revision.author.clone().unwrap_or_default(),
        date: revision.date.clone(),
    }
}

/// Snapshot a row's CURRENT formatting into a `RowFormattingChange` — the
/// "before" state of a `w:trPrChange` (§17.13.5.36). Mirrors the field mapping
/// the accept/reject projection restores (`tracked_model.rs`: reject restores
/// `previous_height` / `previous_height_rule`), so an authored change is
/// indistinguishable from one Word produced. Called BEFORE mutating the row so
/// the snapshot captures the complete prior `trPr`.
pub(crate) fn snapshot_row_formatting(
    row: &TableRowNode,
    revision: &RevisionInfo,
) -> RowFormattingChange {
    RowFormattingChange {
        revision_id: revision.revision_id,
        identity: 0,
        previous_height: row.height,
        previous_height_rule: row.height_rule.clone(),
        author: revision.author.clone().unwrap_or_default(),
        date: revision.date.clone(),
    }
}

/// Capture a table's CURRENT `tblPr` formatting as the "before" state of a
/// `w:tblPrChange` (§17.13.5.34). Mirrors the field mapping the tracked-model
/// classifier and the reject projection use (`tracked_model.rs`: reject restores
/// `width` / `borders` / `default_cell_margins` from these fields), so a verb-
/// authored change is byte-identical to one Word produced. Call BEFORE mutating
/// `table.formatting` so the inner `tblPr` is the complete previous state.
fn snapshot_table_formatting(table: &TableNode, revision: &RevisionInfo) -> TableFormattingChange {
    TableFormattingChange {
        revision_id: revision.revision_id,
        identity: 0,
        previous_width: table.formatting.width.clone(),
        previous_borders: table.formatting.borders.clone(),
        previous_default_cell_margins: table.formatting.default_cell_margins.clone(),
        author: revision.author.clone().unwrap_or_default(),
        date: revision.date.clone(),
    }
}

/// Copy every tracked pPr-level field from `exemplar` into `target`,
/// leaving text content (segments, numbering text, id, etc.)
/// untouched. This is the "clone formatting from exemplar" primitive
/// the `set_attr` step uses — it mirrors what the tracked-model
/// merge pipeline does when applying a new paragraph's pPr over an
/// old one.
fn copy_paragraph_formatting_from_exemplar(target: &mut ParagraphNode, exemplar: &ParagraphNode) {
    target.style_id = exemplar.style_id.clone();
    target.align = exemplar.align.clone();
    target.has_direct_align = exemplar.has_direct_align;
    target.indent = exemplar.indent.clone();
    target.has_direct_indent = exemplar.has_direct_indent;
    target.authored_indent = exemplar.authored_indent.clone();
    target.spacing = exemplar.spacing.clone();
    target.has_direct_spacing = exemplar.has_direct_spacing;
    target.authored_spacing = exemplar.authored_spacing.clone();
    target.borders = exemplar.borders.clone();
    target.keep_next = exemplar.keep_next;
    target.keep_lines = exemplar.keep_lines;
    target.page_break_before = exemplar.page_break_before;
    target.widow_control = exemplar.widow_control;
    target.contextual_spacing = exemplar.contextual_spacing;
    target.shading = exemplar.shading.clone();
    target.tab_stops = exemplar.tab_stops.clone();
    target.numbering = exemplar.numbering.clone();
    target.has_direct_numbering = exemplar.has_direct_numbering;
    target.heading_level = exemplar.heading_level.clone();
    target.paragraph_mark_marks = exemplar.paragraph_mark_marks.clone();
    target.paragraph_mark_style_props = exemplar.paragraph_mark_style_props.clone();
    target.paragraph_mark_rpr_off = exemplar.paragraph_mark_rpr_off;
    target.mirror_indents = exemplar.mirror_indents;
    target.auto_space_de = exemplar.auto_space_de;
    target.auto_space_dn = exemplar.auto_space_dn;
    target.bidi = exemplar.bidi;
    target.text_alignment = exemplar.text_alignment.clone();
    target.suppress_auto_hyphens = exemplar.suppress_auto_hyphens;
    target.snap_to_grid = exemplar.snap_to_grid;
    target.overflow_punct = exemplar.overflow_punct;
    target.adjust_right_ind = exemplar.adjust_right_ind;
    target.word_wrap = exemplar.word_wrap;
    target.frame_pr = exemplar.frame_pr.clone();
    target.text_direction = exemplar.text_direction.clone();
    // cnf_style is table-cell conditional formatting; not part of the
    // exemplar's paragraph-role cascade, leave target's value alone.
    // literal_prefix is text content, not pPr — leave it alone too.
}

/// Apply a `SetBlockRangeAttr` step: for each paragraph in the range,
/// clone formatting from the new role's exemplar into the paragraph
/// and record the previous pPr in a `ParagraphFormattingChange`.
///
/// Numbering continuation is automatic: when multiple paragraphs in a
/// range are switched to the same numbered role, they all reference
/// the same `num_id` / `ilvl` that was cloned from the exemplar, and
/// Word synthesizes sequential counters at render time from document
/// order. No per-block numbering state needs to be threaded.
///
/// No-op when the target's current formatting already matches the
/// exemplar's (the role is effectively unchanged); emitting a
/// `pPrChange` in that case would produce a visually-empty tracked
/// change, confusing reviewers.
fn apply_set_block_range_attr(
    doc: &mut CanonDoc,
    from_block_id: &NodeId,
    to_block_id: &NodeId,
    role: &str,
    ctx: &mut ApplyBlockContext<'_>,
) -> Result<(), EditError> {
    let (start, end) =
        normalize_block_range_indices(doc, from_block_id, to_block_id, ctx.step_index)?;

    // Every block in the range must be a Normal-status paragraph with
    // no existing tracked segments. Tables and opaque blocks reject:
    // set_attr only makes sense on paragraph roles.
    for tb in &doc.blocks[start..=end] {
        validate_block_is_editable(tb, ctx.step_index)?;
        match &tb.block {
            BlockNode::Paragraph(_) => {}
            BlockNode::Table(_) => {
                return Err(EditError::NotAParagraph {
                    block_id: block_id_of(&tb.block).clone(),
                    actual_kind: "table",
                    step_index: ctx.step_index,
                });
            }
            BlockNode::OpaqueBlock(_) => {
                return Err(EditError::NotAParagraph {
                    block_id: block_id_of(&tb.block).clone(),
                    actual_kind: "opaque_block",
                    step_index: ctx.step_index,
                });
            }
        }
    }

    // Resolve the role and find its exemplar once for the whole range.
    // `resolve_paragraph_spec` builds a fresh paragraph from the role's
    // exemplar; we only need the pPr fields from it, so we reuse the
    // same helper and then copy the formatting fields over each target
    // paragraph in turn. The spec passes empty text + no restart so
    // the resulting paragraph carries only the exemplar's pPr.
    let spec = ParagraphBlockSpec {
        role: Some(role.to_string()),
        content: ParagraphContent {
            fragments: Vec::new(),
        },
        restart_numbering: false,
        list: None,
    };
    let resolved = resolve_paragraph_spec(doc, &spec, ctx.step_index)?;
    let exemplar_para = match resolved {
        BlockNode::Paragraph(p) => p,
        _ => unreachable!("resolve_paragraph_spec returns BlockNode::Paragraph"),
    };

    for idx in start..=end {
        let tb = &mut doc.blocks[idx];
        let para = match &mut tb.block {
            BlockNode::Paragraph(p) => p,
            _ => unreachable!("validated above"),
        };

        // No-op when the target already has the exemplar's formatting.
        // We compare only the fields that `copy_paragraph_formatting_
        // from_exemplar` would overwrite — a surgical equality check.
        let already_matches = para.style_id == exemplar_para.style_id
            && para.align == exemplar_para.align
            && para.indent == exemplar_para.indent
            && para.spacing == exemplar_para.spacing
            && para.numbering.as_ref().map(|n| (n.num_id, n.ilvl))
                == exemplar_para.numbering.as_ref().map(|n| (n.num_id, n.ilvl))
            && para.heading_level == exemplar_para.heading_level
            && para.borders == exemplar_para.borders
            && para.shading == exemplar_para.shading;
        if already_matches {
            continue;
        }

        // Snapshot current pPr → attach to paragraph as pPrChange.
        // Bump the revision counter so this edit's pPrChange is
        // distinguishable from other tracked changes in the transaction.
        let rev_for_change = next_revision(ctx.revision, ctx.rev_counter);
        let fc = snapshot_paragraph_formatting(para, &rev_for_change);
        copy_paragraph_formatting_from_exemplar(para, &exemplar_para);
        para.formatting_change = Some(fc);
        // Clear cached text hash / rendered text so any downstream
        // consumer that compares against a hash recomputes.
        para.block_text_hash = None;
        para.rendered_text = None;
    }

    Ok(())
}

// ─── Entry point ─────────────────────────────────────────────────────────────

/// Find the maximum revision_id in a document's tracked changes.
/// Used to determine the starting counter for new revision IDs.
fn max_revision_id(doc: &CanonDoc) -> u32 {
    // One id namespace, one walker: the runtime's allocator walk covers
    // everything enumerable (statuses incl. table rows/cells and stories,
    // plus all five formatting-change kinds). A weaker duplicate here once
    // missed formatting-change ids and minted a colliding id.
    crate::runtime::max_revision_id(doc)
}

/// Apply an edit transaction to a CanonDoc.
///
/// Returns a new CanonDoc with the edits applied as tracked changes, plus a
/// [`PendingParts`] carrying any OPC parts (media binaries, styles.xml
/// fragments) the verbs want staged into the save path. The input document is
/// not modified.
///
/// `PendingParts` is a per-apply transient derived entirely from the
/// transaction (never persisted). All current verbs leave it **empty** — the
/// channel exists but no shipped verb writes to it yet; the save path treats an
/// empty `PendingParts` as a no-op.
///
/// If any step fails, returns the error immediately — no partial application.
///
/// Id allocation caveat: the counter is seeded from the ids visible in the
/// `CanonDoc`. Body-level (block) opaque interiors keep their bytes in the
/// runtime's serialize scaffold, INVISIBLE here — a caller that holds those
/// bytes (the runtime — `EditSnapshot::apply`) must use
/// [`apply_transaction_with_id_floor`] with their max id, or a minted id can
/// collide with a pre-existing block-interior id.
pub fn apply_transaction(
    doc: &CanonDoc,
    transaction: &EditTransaction,
) -> Result<(CanonDoc, PendingParts), EditError> {
    apply_transaction_with_id_floor(doc, transaction, 0)
}

/// [`apply_transaction`] with an EXTERNAL id floor: `external_max_revision_id`
/// is the highest revision id the caller can see that this pure core cannot
/// (block-opaque interiors living in the serialize scaffold). Minted ids are
/// strictly above both.
pub fn apply_transaction_with_id_floor(
    doc: &CanonDoc,
    transaction: &EditTransaction,
    external_max_revision_id: u32,
) -> Result<(CanonDoc, PendingParts), EditError> {
    // Every guard supplied in one atomic transaction describes the snapshot
    // the caller inspected before submitting that transaction. Keep that
    // immutable snapshot available while the mutable clone advances through
    // its ordered steps. A later step targeting the same block must not become
    // spuriously stale merely because an earlier step in this transaction
    // changed it.
    let transaction_base = doc;
    let mut doc = doc.clone();
    // Fixed for the whole call: any revision_id >= this floor was minted by
    // THIS transaction (see `apply_set_cell_text_in_place`'s "own pending
    // insert" check). `rev_counter` itself advances as steps run, so it must
    // NOT be reused for that comparison.
    let transaction_floor = max_revision_id(&doc).max(external_max_revision_id) + 1;
    let mut rev_counter = transaction_floor;
    let mut insert_order_state = InsertOrderState::default();
    // OPC parts staged by verbs (media/styles). Empty for all current verbs.
    let mut pending = PendingParts::default();

    for (step_index, step) in transaction.steps.iter().enumerate() {
        // DIRECT-mode physical-removal edits (table row/column delete,
        // block-range delete, block-range replace's delete leg, blocks-to-table's
        // source drain) remove content outright rather than marking it deleted.
        // If the removed content held one half of a range-marker pair
        // (bookmark/comment/permission) whose other half survives, the pair tears
        // (ECMA-376 §17.13.6) and the post-serialization pairing guard refuses the
        // document. Snapshot before / repair after collapses any such tear to a
        // point at the survivor — the SAME rule the accept/reject resolution path
        // applies (`tracked_model::collapse_resolution_torn_range_markers`) and
        // what Word does when a bookmarked range's interior is deleted. Tracked
        // mode only MARKS deletions (both halves stay in the tree), so it never
        // tears — hence this is gated to Direct, which also skips the snapshot
        // walk on the common tracked path.
        let range_marker_snapshot = (transaction.materialization_mode
            == MaterializationMode::Direct)
            .then(|| crate::tracked_model::snapshot_range_markers(&doc.blocks));
        match step {
            EditStep::ReplaceParagraphText {
                block_id,
                rationale: _,
                replacement_role: _,
                expect,
                semantic_hash,
                content,
            } => {
                // Every replacement — plain text, styled runs, PreservedInlineRef
                // anchors, NewHyperlink atoms — routes to the word-level inline-diff
                // (apply_replace_paragraph_text). Links ride as placeholder
                // codepoints; authored marks ride a per-char map; both surface in
                // the reconstructed segments, so edits stay minimal redlines.

                // Phase 1: Validate FIRST, against the block as the client saw it
                // (with any pre-existing tracked changes). The staleness guard the
                // client holds is the hash of the PROJECTED block, so it must be
                // checked before prep flattens tracked ins/del — otherwise a re-edit
                // of a paragraph that already carries a tracked change is rejected
                // with a spurious BlockSemanticHashMismatch (B1).
                let path = validate_replace_step(
                    &doc,
                    block_id,
                    expect,
                    semantic_hash.as_deref(),
                    content,
                    step_index,
                )?;

                // DIRECT mode only: accept existing tracked changes into a clean
                // Normal base so the word-level diff runs on final content
                // (flatten-then-diff). In TRACKED mode we must NOT flatten — that
                // would silently ACCEPT the paragraph's prior pending changes for
                // the user. Instead the materializer below diffs against the
                // accept-all view and carries prior tombstones through verbatim, so
                // a re-edit preserves both the prior and the new pending changes.
                if transaction.materialization_mode == MaterializationMode::Direct {
                    let _ = prepare_paragraph_for_direct_edit(&mut doc, block_id, step_index)?;
                }

                // Phase 0.5: Numbering-prefix duplication guard — if the target
                // paragraph carries a numbering label (in literal_prefix or
                // generated from numbering) and the replacement's leading text
                // echoes that label, refuse rather than silently strip it. The
                // label is re-emitted by the serializer, so applying the content
                // verbatim would double it ("1.1.Events"); silently stripping it
                // dropped the agent's text with no trace. Same guard runs on the
                // span path (ReplaceSpanText) so both write paths agree.
                let para = match block_at(&doc, &path) {
                    BlockNode::Paragraph(p) => p,
                    _ => unreachable!("validated as paragraph"),
                };
                if let Some(leading) = content.fragments.first().and_then(ContentFragment::as_text)
                    && let Some((label, paragraph_label)) =
                        numbering_label_duplicated_by(para, leading)
                {
                    return Err(EditError::PrefixDuplicatesLabel {
                        block_id: block_id.clone(),
                        step_index,
                        label,
                        paragraph_label,
                        current_text: paragraph_text_with_label(para),
                    });
                }

                // Phase 0: Identity check — a content-bearing replace that would
                // change nothing is not silently swallowed: it fails loud so the
                // caller never sees a no-op reported as a successful edit
                // (CLAUDE.md "no silent fallbacks").
                if is_identity_replacement(para, content) {
                    return Err(EditError::NoOpEdit {
                        block_id: block_id.clone(),
                        step_index,
                        reason: "replacement text, marks, and anchors equal the target paragraph",
                    });
                }

                // Phase 2-4: Diff/segment replace, reconstruct, normalize.
                // Capture (clone) the enclosing block insertion BEFORE the mutable
                // borrow: a paragraph that is itself a pending block insertion has
                // no base content, so a delete of its text must un-propose/stack,
                // never mint a plain Deleted tombstone.
                let enclosing =
                    enclosing_block_insertion(&doc, &path, transaction.materialization_mode);
                let block = block_at_mut(&mut doc, &path);
                let para = match block {
                    BlockNode::Paragraph(p) => p,
                    _ => unreachable!("validated as paragraph"),
                };
                // A TEXT edit — even on a formatted paragraph — goes through the
                // surgical word-diff: authored marks ride the diff via a per-char
                // map (see render_for_diff / emit_insert_token), so changing one
                // word stays a minimal redline instead of a whole-paragraph
                // delete+insert.
                //
                // BUT the word-diff is text-keyed: text it finds in BOTH the old
                // and new content is kept (Equal) carrying its ORIGINAL marks, so it
                // cannot express a reformat of kept text. We classify and route:
                //  - FormattingOnly (same text, marks changed — e.g. bold one word):
                //    a surgical per-run tracked rPrChange (apply_formatting_only_
                //    replace), the same representation set_format produces.
                //  - Mixed (text changed AND kept text reformatted in one edit): the
                //    surgical diff can't carry the reformat, so fall back to the
                //    whole-paragraph segment replace (a reversible delete+insert).
                //  - None (pure text edit): stays surgical.
                // The cell `set_cell_text` path only ever sends plain Text (never
                // StyledText), so it never reaches the rPrChange branch.
                if content.fragments.iter().any(ContentFragment::is_styled) {
                    match classify_kept_text_reformat(para, content) {
                        KeptTextReformat::FormattingOnly => apply_formatting_only_replace(
                            para,
                            content,
                            &transaction.revision,
                            &mut rev_counter,
                        ),
                        KeptTextReformat::Mixed => apply_segment_replace_paragraph(
                            para,
                            content,
                            &transaction.revision,
                            enclosing.as_ref(),
                            &mut rev_counter,
                        ),
                        KeptTextReformat::None => apply_replace_paragraph_text(
                            para,
                            content,
                            &transaction.revision,
                            enclosing.as_ref(),
                            &mut rev_counter,
                        ),
                    }
                } else if para
                    .all_inlines()
                    .any(|i| matches!(i, InlineNode::Text(t) if !marks_set_of(&t.marks, &t.style_props).is_empty()))
                    && classify_kept_text_reformat(para, content) == KeptTextReformat::FormattingOnly
                {
                    // All-plain content. A SAME-TEXT replace over a paragraph carrying
                    // surface marks is an editor UN-FORMAT (the content specifies no
                    // marks where the run is bold/italic/underline) → a surgical per-
                    // run rPrChange that REMOVES the mark. Any other all-plain replace
                    // — a genuine text edit — INHERITS the run formatting (the LLM
                    // contract, I4), so it stays on the surgical word-diff below.
                    apply_formatting_only_replace(
                        para,
                        content,
                        &transaction.revision,
                        &mut rev_counter,
                    );
                } else {
                    apply_replace_paragraph_text(
                        para,
                        content,
                        &transaction.revision,
                        enclosing.as_ref(),
                        &mut rev_counter,
                    );
                }
                // For direct-mode edits, immediately resolve the just-created
                // tracked changes so the paragraph returns to all-Normal.
                if transaction.materialization_mode == MaterializationMode::Direct {
                    let block = block_at_mut(&mut doc, &path);
                    project_block_for_accept_reject(block, true);
                }
            }
            EditStep::ReplaceSpanText {
                block_id,
                guard,
                expect,
                span,
                content,
                rationale: _,
            } => {
                // The status-preserving splice. The span is resolved against the
                // SAME segment structure the read view minted the handle over
                // (the determinism contract in `resolve_span`) — never
                // pre-flattened: flattening would re-map the handle onto a merged
                // structure and silently target the wrong inlines. The range
                // contract (guard, status, text identity, brackets) is enforced
                // in validation; the splice
                // then replaces only the targeted range and carries every
                // out-of-range segment through untouched, so a neighbouring
                // tracked change survives structurally ("layer beside").
                //
                // Phase 1: validate + resolve the span to a flat inline range.
                // In tracked mode the author may splice inside their OWN
                // pending insertion (same-author in-place editing); in direct
                // mode the range must be all-Normal — an untracked edit must
                // never silently
                // resolve or rewrite a pending change.
                let in_place_author = match transaction.materialization_mode {
                    MaterializationMode::TrackedChange => transaction.revision.author.as_deref(),
                    MaterializationMode::Direct => None,
                };
                let (path, range) = validate_span_replace_step(
                    &doc,
                    block_id,
                    guard,
                    expect.as_deref(),
                    span,
                    in_place_author,
                    step_index,
                )?;

                let para = match block_at(&doc, &path) {
                    BlockNode::Paragraph(p) => p,
                    _ => unreachable!("validated as paragraph"),
                };

                // Styled replacement text has no range-scoped materializer yet
                // (whole-paragraph replace routes it to segment replace);
                // silently dropping the mark intent is forbidden — refuse loud.
                if content.fragments.iter().any(ContentFragment::is_styled) {
                    return Err(EditError::SpanStyledContentUnsupported {
                        block_id: block_id.clone(),
                        step_index,
                    });
                }

                // Numbering-prefix duplication guard — identical contract to the
                // whole-paragraph path. A splice that begins at the paragraph
                // HEAD (flat-inline index 0, where the label conceptually sits)
                // whose inserted text echoes the paragraph's numbering label
                // would render the label twice: this is the exact path that
                // once shipped "1.1.Events". Refuse rather than corrupt.
                // A splice that starts mid-body (range.0 > 0) is unaffected — a
                // label there is the agent's real content, not a duplication.
                if range.0 == 0
                    && let Some(leading) =
                        content.fragments.first().and_then(ContentFragment::as_text)
                    && let Some((label, paragraph_label)) =
                        numbering_label_duplicated_by(para, leading)
                {
                    return Err(EditError::PrefixDuplicatesLabel {
                        block_id: block_id.clone(),
                        step_index,
                        label,
                        paragraph_label,
                        current_text: paragraph_text_with_label(para),
                    });
                }

                // Walls: the in-range wall inventory is re-asserted
                // against the replacement content — an opaque/hard-break inside
                // the targeted range must be carried by reference
                // (OpaqueDestroyed otherwise). Out-of-range walls are not in
                // the inventory; the splice carries them itself, untouched.
                let range_anchors = collect_anchor_inventory_in_range(para, range);
                validate_preserved_inlines(para, block_id, content, &range_anchors, step_index)?;

                // Identity splice: the replacement equals the range. A
                // content-bearing splice that changes nothing fails loud rather
                // than being silently dropped (CLAUDE.md "no silent fallbacks").
                if is_identity_splice(para, range, content) {
                    return Err(EditError::NoOpEdit {
                        block_id: block_id.clone(),
                        step_index,
                        reason: "replacement text and anchors equal the targeted span",
                    });
                }

                // Phase 2: the splice (same diff/reconstruct engine as the
                // whole-paragraph replace, fed only the targeted range).
                // Capture the enclosing block insertion before the mutable borrow
                // a splice inside a paragraph that is itself a pending
                // block insertion must un-propose/stack removed text, not tombstone
                // it.
                let enclosing =
                    enclosing_block_insertion(&doc, &path, transaction.materialization_mode);
                let block = block_at_mut(&mut doc, &path);
                let para = match block {
                    BlockNode::Paragraph(p) => p,
                    _ => unreachable!("validated as paragraph"),
                };
                apply_span_splice(
                    para,
                    range,
                    content,
                    &transaction.revision,
                    enclosing.as_ref(),
                    &mut rev_counter,
                    transaction.materialization_mode == MaterializationMode::Direct,
                );
            }
            EditStep::InsertParagraphs {
                anchor_block_id,
                position,
                rationale: _,
                blocks,
            } => {
                if transaction.materialization_mode == MaterializationMode::TrackedChange {
                    let mut ctx = ApplyBlockContext {
                        step_index,
                        revision: &transaction.revision,
                        rev_counter: &mut rev_counter,
                        order_state: &mut insert_order_state,
                    };
                    apply_insert_paragraphs(
                        &mut doc,
                        anchor_block_id,
                        *position,
                        blocks,
                        &mut ctx,
                    )?;
                    normalize_final_mark(&mut doc, &transaction.revision, &mut rev_counter);
                } else {
                    apply_insert_paragraphs_direct(
                        &mut doc,
                        anchor_block_id,
                        *position,
                        blocks,
                        &mut insert_order_state,
                        step_index,
                    )?;
                }
            }
            EditStep::DeleteBlockRange {
                from_block_id,
                to_block_id,
                rationale: _,
                expect,
                semantic_hash,
            } => {
                if transaction.materialization_mode == MaterializationMode::Direct {
                    let _ = prepare_block_range_for_direct_edit(
                        &mut doc,
                        from_block_id,
                        to_block_id,
                        step_index,
                    )?;
                }
                let (start, end) = validate_delete_step(
                    &doc,
                    from_block_id,
                    to_block_id,
                    expect,
                    semantic_hash.as_deref(),
                    step_index,
                )?;
                if transaction.materialization_mode == MaterializationMode::TrackedChange {
                    apply_delete_block_range(
                        &mut doc,
                        start,
                        end,
                        &transaction.revision,
                        &mut rev_counter,
                    );
                    normalize_final_mark(&mut doc, &transaction.revision, &mut rev_counter);
                } else {
                    apply_delete_block_range_direct(&mut doc, start, end);
                }
            }
            EditStep::ReplaceBlockRange {
                from_block_id,
                to_block_id,
                rationale: _,
                expect,
                semantic_hash,
                blocks,
            } => {
                // Route to inline-diff when: single-block target, single-block
                // replacement, no restart_numbering, and no StyledText marks
                // in the replacement. Mark-bearing replaces fall back to
                // structural replace (block delete + insert) which routes
                // through `resolve_paragraph_spec` — that path applies LLM
                // marks to the new TextNodes. Threading marks through the
                // inline-diff reconstruction is deferred work.
                let inline_diff_ok = from_block_id == to_block_id
                    && blocks.len() == 1
                    && blocks.first().is_some_and(|block| match block {
                        BlockSpec::Paragraph(paragraph) => {
                            // StyledText marks fall back to the structural
                            // path (resolve_paragraph_spec) so marks get
                            // applied to TextNodes. NewHyperlink is handled
                            // directly by the inline diff via placeholder
                            // codepoints and does NOT need the fallback.
                            !paragraph.restart_numbering
                                && !paragraph
                                    .content
                                    .fragments
                                    .iter()
                                    .any(ContentFragment::is_styled)
                        }
                        BlockSpec::Toc(_) => false,
                        // Tables never use the paragraph inline-diff path;
                        // the engine routes `replace(table)` through the
                        // dedicated `EditStep::ReplaceTable` arm. Falling
                        // back to structural replace here would block-delete
                        // and insert the whole table, defeating the
                        // row/cell-aligned tracked-change shape — the
                        // dedicated step is the only correct path.
                        BlockSpec::Table(_) => false,
                    });
                if inline_diff_ok && let Some(BlockSpec::Paragraph(block)) = blocks.first() {
                    let content = block.content.clone();
                    if transaction.materialization_mode == MaterializationMode::Direct {
                        let _ =
                            prepare_paragraph_for_direct_edit(&mut doc, from_block_id, step_index)?;
                    }
                    let path = validate_replace_step(
                        &doc,
                        from_block_id,
                        expect,
                        semantic_hash.as_deref(),
                        &content,
                        step_index,
                    )?;
                    let para = match block_at(&doc, &path) {
                        BlockNode::Paragraph(p) => p,
                        _ => unreachable!("validated as paragraph"),
                    };
                    // A single-paragraph replace that would change nothing fails
                    // loud rather than silently skipping (CLAUDE.md "no silent
                    // fallbacks").
                    if is_identity_replacement(para, &content) {
                        return Err(EditError::NoOpEdit {
                            block_id: from_block_id.clone(),
                            step_index,
                            reason: "replacement text, marks, and anchors equal the target paragraph",
                        });
                    }
                    // a single-paragraph range replace on a paragraph that
                    // is itself a pending block insertion must un-propose/stack, not
                    // tombstone. Capture the enclosing insertion before the mut borrow.
                    let enclosing =
                        enclosing_block_insertion(&doc, &path, transaction.materialization_mode);
                    let para = match block_at_mut(&mut doc, &path) {
                        BlockNode::Paragraph(p) => p,
                        _ => unreachable!("validated as paragraph"),
                    };
                    apply_replace_paragraph_text(
                        para,
                        &content,
                        &transaction.revision,
                        enclosing.as_ref(),
                        &mut rev_counter,
                    );
                    if transaction.materialization_mode == MaterializationMode::Direct {
                        let block = block_at_mut(&mut doc, &path);
                        project_block_for_accept_reject(block, true);
                    }
                } else {
                    if transaction.materialization_mode == MaterializationMode::Direct {
                        let _ = prepare_block_range_for_direct_edit(
                            &mut doc,
                            from_block_id,
                            to_block_id,
                            step_index,
                        )?;
                    }
                    let _ = validate_delete_step(
                        &doc,
                        from_block_id,
                        to_block_id,
                        expect,
                        semantic_hash.as_deref(),
                        step_index,
                    )?;
                    if transaction.materialization_mode == MaterializationMode::TrackedChange {
                        let mut ctx = ApplyBlockContext {
                            step_index,
                            revision: &transaction.revision,
                            rev_counter: &mut rev_counter,
                            order_state: &mut insert_order_state,
                        };
                        apply_structural_replace(
                            &mut doc,
                            from_block_id,
                            to_block_id,
                            blocks,
                            &mut ctx,
                        )?;
                        normalize_final_mark(&mut doc, &transaction.revision, &mut rev_counter);
                    } else {
                        apply_structural_replace_direct(
                            &mut doc,
                            from_block_id,
                            to_block_id,
                            blocks,
                            &mut insert_order_state,
                            step_index,
                        )?;
                    }
                }
            }
            EditStep::MoveBlockRange {
                from_block_id,
                to_block_id,
                dest_anchor_id,
                dest_position,
                rationale: _,
                expect,
                semantic_hash,
            } => {
                // Direct mode is not implemented for moves: the
                // paired `w:moveFrom`/`w:moveTo` semantics require the
                // tracked-change model. Direct-mode callers should
                // fall back to delete+insert.
                if transaction.materialization_mode != MaterializationMode::TrackedChange {
                    return Err(EditError::UnsupportedParagraphStructure {
                        block_id: from_block_id.clone(),
                        reason: "move is only supported in tracked_change mode \
                                 (direct mode must decompose to delete+insert)"
                            .to_string(),
                        step_index,
                    });
                }
                let mut ctx = ApplyBlockContext {
                    step_index,
                    revision: &transaction.revision,
                    rev_counter: &mut rev_counter,
                    order_state: &mut insert_order_state,
                };
                apply_move_block_range(
                    &mut doc,
                    from_block_id,
                    to_block_id,
                    dest_anchor_id,
                    *dest_position,
                    expect.as_deref(),
                    semantic_hash.as_deref(),
                    &mut ctx,
                )?;
                // A move whose destination ends the document leaves the moved-in
                // final pilcrow carrying an unresolvable insertion-class mark;
                // re-attribute it to the anchor (the move-aware tail rule).
                normalize_final_mark(&mut doc, &transaction.revision, &mut rev_counter);
            }
            EditStep::SetBlockRangeAttr {
                from_block_id,
                to_block_id,
                role,
                rationale: _,
            } => {
                // Direct mode: immediately project the pPrChange so
                // the paragraph lands with the new formatting and no
                // tracked-change shadow. Tracked mode: record the
                // formatting change so accept/reject produces the
                // right state.
                let mut ctx = ApplyBlockContext {
                    step_index,
                    revision: &transaction.revision,
                    rev_counter: &mut rev_counter,
                    order_state: &mut insert_order_state,
                };
                apply_set_block_range_attr(&mut doc, from_block_id, to_block_id, role, &mut ctx)?;
                if transaction.materialization_mode == MaterializationMode::Direct {
                    // Project immediately: keep the new pPr, drop the
                    // formatting_change record so the block reads as
                    // "normal" post-edit.
                    let (start, end) = normalize_block_range_indices(
                        &doc,
                        from_block_id,
                        to_block_id,
                        step_index,
                    )?;
                    for tb in &mut doc.blocks[start..=end] {
                        project_block_for_accept_reject(&mut tb.block, true);
                    }
                }
            }
            EditStep::ReplaceHyperlinkText {
                hyperlink_id,
                rationale: _,
                expect,
                new_text,
                expect_href,
                expect_anchor,
            } => {
                let revision = stamp_revision(&transaction.revision, &mut rev_counter);
                apply_replace_hyperlink_text(
                    &mut doc,
                    hyperlink_id,
                    expect,
                    new_text,
                    expect_href.as_deref(),
                    expect_anchor.as_deref(),
                    &revision,
                    step_index,
                )?;
                if transaction.materialization_mode == MaterializationMode::Direct {
                    // Resolve the just-created tracked changes inside the
                    // hyperlink so the paragraph reads as all-Normal after
                    // a direct-mode edit.
                    if let Some(path) = find_hyperlink_path(&doc, hyperlink_id) {
                        let block = block_at_mut(&mut doc, &path);
                        project_block_for_accept_reject(block, true);
                    }
                }
            }
            EditStep::ReplaceTable {
                block_id,
                rationale: _,
                semantic_hash,
                replacement,
            } => {
                apply_replace_table(
                    &mut doc,
                    block_id,
                    semantic_hash.as_deref(),
                    replacement,
                    &transaction.revision,
                    &mut rev_counter,
                    transaction.materialization_mode,
                    step_index,
                )?;
            }
            EditStep::TableStructureOp {
                block_id,
                semantic_hash,
                op,
                rationale: _,
            } => {
                // Existing tables validate against the immutable transaction
                // base. A table created by an earlier step has no base entry,
                // so retain the ordinary current-state guard for that case.
                let guard_for_current = if let Some(expected) = semantic_hash.as_deref()
                    && let Some(base_idx) = find_block_index(&transaction_base.blocks, block_id)
                {
                    if let Err(actual) =
                        check_block_guard(&transaction_base.blocks[base_idx].block, expected)
                    {
                        return Err(EditError::BlockSemanticHashMismatch {
                            block_id: block_id.clone(),
                            expected: expected.to_string(),
                            actual,
                            step_index,
                        });
                    }
                    None
                } else {
                    semantic_hash.as_deref()
                };
                verbs::table_ops::apply(
                    &mut doc,
                    block_id,
                    guard_for_current,
                    op,
                    &transaction.revision,
                    &mut rev_counter,
                    transaction_floor,
                    transaction.materialization_mode,
                    step_index,
                )?;
            }
            EditStep::SetHyperlinkAttr {
                hyperlink_id,
                new_href,
                new_anchor,
                expect_href,
                expect_anchor,
                rationale: _,
            } => {
                // No `stamp_revision` here: option (A) is by design not
                // tracked (OOXML has no `w:hyperlinkChange`).
                // `MaterializationMode::Direct` and
                // `MaterializationMode::TrackedChange` are structurally
                // identical because we never create a tracked envelope —
                // the mutation is silent in the tracked-change audit
                // trail. The materialization mode is therefore ignored
                // here.
                apply_set_hyperlink_attr(
                    &mut doc,
                    hyperlink_id,
                    new_href.as_deref(),
                    new_anchor.as_ref(),
                    expect_href.as_deref(),
                    expect_anchor.as_deref(),
                    step_index,
                )?;
            }
            EditStep::SetRunFormatting {
                block_id,
                expect,
                semantic_hash,
                marks,
                style,
                rationale: _,
            } => {
                // Revision-id stamping (class audit in
                // spec_selective_formatting_resolution.rs): every sibling
                // formatting verb (cell/row/table below, paragraph via its
                // own internal rev_counter use) gets a freshly-stamped,
                // never-zero revision id. This arm must not pass the
                // transaction's raw, un-stamped `revision` straight through, or
                // every run-level w:rPrChange is born as the `0` legacy sentinel
                // — invisible to selective accept_changes/reject_changes,
                // which gate on revision_id != 0.
                let revision = stamp_revision(&transaction.revision, &mut rev_counter);
                verbs::run_formatting::apply(
                    &mut doc,
                    block_id,
                    expect,
                    semantic_hash.as_deref(),
                    *marks,
                    style,
                    &revision,
                    transaction.materialization_mode,
                    step_index,
                )?;
            }
            EditStep::SetParagraphFormatting {
                block_id,
                semantic_hash,
                patch,
                rationale: _,
            } => {
                verbs::paragraph_formatting::apply(
                    &mut doc,
                    block_id,
                    semantic_hash.as_deref(),
                    patch,
                    &transaction.revision,
                    &mut rev_counter,
                    transaction.materialization_mode,
                    step_index,
                )?;
            }
            EditStep::SetCellFormatting {
                block_id,
                row_index,
                col_index,
                semantic_hash,
                patch,
                rationale: _,
            } => {
                let revision = stamp_revision(&transaction.revision, &mut rev_counter);
                verbs::cell_formatting::apply(
                    &mut doc,
                    block_id,
                    *row_index,
                    *col_index,
                    semantic_hash.as_deref(),
                    patch,
                    &revision,
                    transaction.materialization_mode,
                    step_index,
                )?;
            }
            EditStep::SetRowFormatting {
                block_id,
                row_index,
                semantic_hash,
                patch,
                rationale: _,
            } => {
                let revision = stamp_revision(&transaction.revision, &mut rev_counter);
                verbs::row_formatting::apply(
                    &mut doc,
                    block_id,
                    *row_index,
                    semantic_hash.as_deref(),
                    patch,
                    &revision,
                    transaction.materialization_mode,
                    step_index,
                )?;
            }
            EditStep::SetTableFormatting {
                block_id,
                semantic_hash,
                patch,
                rationale: _,
            } => {
                let revision = stamp_revision(&transaction.revision, &mut rev_counter);
                verbs::table_formatting::apply(
                    &mut doc,
                    block_id,
                    semantic_hash.as_deref(),
                    patch,
                    &revision,
                    &mut rev_counter,
                    transaction.materialization_mode,
                    step_index,
                )?;
            }
            EditStep::InsertCrossReference {
                block_id,
                expect,
                semantic_hash,
                spec,
                rationale: _,
            } => {
                let revision = stamp_revision(&transaction.revision, &mut rev_counter);
                verbs::fields_crossrefs::apply(
                    &mut doc,
                    block_id,
                    expect,
                    semantic_hash.as_deref(),
                    spec,
                    &revision,
                    transaction.materialization_mode,
                    step_index,
                )?;
            }
            EditStep::SetParagraphNumbering {
                block_id,
                semantic_hash,
                change,
                rationale: _,
            } => {
                verbs::numbering::apply(
                    &mut doc,
                    block_id,
                    semantic_hash.as_deref(),
                    change,
                    &transaction.revision,
                    transaction.materialization_mode,
                    step_index,
                    &mut pending.numbering_ops,
                )?;
            }
            EditStep::InsertBookmark {
                block_id,
                expect,
                semantic_hash,
                name,
                rationale: _,
            } => {
                verbs::bookmarks::apply_insert(
                    &mut doc,
                    block_id,
                    expect,
                    semantic_hash.as_deref(),
                    name,
                    step_index,
                )?;
            }
            EditStep::RenameBookmark {
                block_id,
                old_name,
                new_name,
                semantic_hash,
                rationale: _,
            } => {
                verbs::bookmarks::apply_rename(
                    &mut doc,
                    block_id,
                    old_name,
                    new_name,
                    semantic_hash.as_deref(),
                    step_index,
                )?;
            }
            EditStep::RemoveBookmark {
                block_id,
                name,
                semantic_hash,
                rationale: _,
            } => {
                verbs::bookmarks::apply_remove(
                    &mut doc,
                    block_id,
                    name,
                    semantic_hash.as_deref(),
                    step_index,
                )?;
            }
            EditStep::ApplyStyle {
                block_id,
                semantic_hash,
                style_id,
                rationale: _,
            } => {
                verbs::styles::apply(
                    &mut doc,
                    block_id,
                    semantic_hash.as_deref(),
                    style_id,
                    &transaction.revision,
                    &mut rev_counter,
                    transaction.materialization_mode,
                    step_index,
                )?;
            }
            EditStep::SetImageAttributes {
                block_id,
                drawing_id,
                semantic_hash,
                resize,
                alt_text,
                rationale: _,
            } => {
                // Untracked, direct in-place mutation (see verbs::images): the
                // materialization mode does not change behavior — there is no
                // tracked-change envelope for opaque-drawing attributes.
                verbs::images::apply(
                    &mut doc,
                    block_id,
                    drawing_id,
                    semantic_hash.as_deref(),
                    *resize,
                    alt_text.clone(),
                    step_index,
                )?;
            }
            EditStep::DeleteImage {
                block_id,
                drawing_id,
                semantic_hash,
                rationale: _,
            } => {
                // Unlike SetImageAttributes, a DELETION genuinely forks on mode
                // (tracked → Deleted segment; direct → dropped), so thread the
                // revision + counter + materialization mode.
                verbs::images::apply_delete_image(
                    &mut doc,
                    block_id,
                    drawing_id,
                    semantic_hash.as_deref(),
                    &transaction.revision,
                    &mut rev_counter,
                    transaction.materialization_mode,
                    step_index,
                )?;
            }
            EditStep::SetImageLayout {
                block_id,
                drawing_id,
                semantic_hash,
                patch,
                rationale: _,
            } => {
                // Untracked, direct in-place mutation (see verbs::image_layout):
                // the materialization mode does not change behavior — there is no
                // tracked-change envelope for opaque-drawing layout attributes.
                verbs::image_layout::apply(
                    &mut doc,
                    block_id,
                    drawing_id,
                    semantic_hash.as_deref(),
                    patch,
                    step_index,
                )?;
            }
            EditStep::CommentCreate {
                block_id,
                expect,
                semantic_hash,
                body,
                author,
                rationale: _,
            } => {
                verbs::comments::apply_create(
                    &mut doc,
                    block_id,
                    expect,
                    semantic_hash.as_deref(),
                    body,
                    author.clone(),
                    &transaction.revision,
                    step_index,
                )?;
            }
            EditStep::CommentReply {
                parent_comment_id,
                body,
                author,
                rationale: _,
            } => {
                verbs::comments::apply_reply(
                    &mut doc,
                    parent_comment_id,
                    body,
                    author.clone(),
                    &transaction.revision,
                    step_index,
                )?;
            }
            EditStep::CommentResolve {
                comment_id,
                done,
                rationale: _,
            } => {
                verbs::comments::apply_resolve(&mut doc, comment_id, *done, step_index)?;
            }
            EditStep::CommentDelete {
                comment_id,
                rationale: _,
            } => {
                verbs::comments::apply_delete(&mut doc, comment_id, step_index)?;
            }
            EditStep::InsertNote {
                block_id,
                expect,
                semantic_hash,
                note_kind,
                body,
                rationale: _,
            } => {
                verbs::footnotes::apply_insert(
                    &mut doc,
                    block_id,
                    expect,
                    semantic_hash.as_deref(),
                    *note_kind,
                    body,
                    &transaction.revision,
                    transaction.materialization_mode,
                    &mut rev_counter,
                    step_index,
                )?;
            }
            EditStep::EditNote {
                note_id,
                note_kind,
                body,
                rationale: _,
            } => {
                verbs::footnotes::apply_edit(
                    &mut doc,
                    note_id,
                    *note_kind,
                    body,
                    &transaction.revision,
                    transaction.materialization_mode,
                    &mut rev_counter,
                    step_index,
                )?;
            }
            EditStep::DeleteNote {
                note_id,
                note_kind,
                rationale: _,
            } => {
                verbs::footnotes::apply_delete(
                    &mut doc,
                    note_id,
                    *note_kind,
                    &transaction.revision,
                    transaction.materialization_mode,
                    &mut rev_counter,
                    step_index,
                )?;
            }
            EditStep::SetPageSetup {
                target,
                patch,
                semantic_hash: _,
                rationale: _,
            } => {
                verbs::page_setup::apply_set_page_setup(
                    &mut doc,
                    target,
                    patch,
                    &transaction.revision,
                    &mut rev_counter,
                    transaction.materialization_mode,
                    step_index,
                )?;
            }
            EditStep::SetSectionType {
                target,
                section_type,
                semantic_hash: _,
                rationale: _,
            } => {
                verbs::page_setup::apply_set_section_type(
                    &mut doc,
                    target,
                    section_type.clone(),
                    step_index,
                )?;
            }
            EditStep::InsertSectionBreak {
                anchor_block_id,
                section_type,
                properties,
                rationale: _,
            } => {
                verbs::page_setup::apply_insert_section_break(
                    &mut doc,
                    anchor_block_id,
                    section_type.clone(),
                    properties,
                    step_index,
                )?;
            }
            EditStep::EditHeader {
                story,
                block_id,
                expect,
                semantic_hash,
                content,
                rationale: _,
            }
            | EditStep::EditFooter {
                story,
                block_id,
                expect,
                semantic_hash,
                content,
                rationale: _,
            } => {
                verbs::headers_footers::apply_edit(
                    &mut doc,
                    story,
                    block_id,
                    expect,
                    semantic_hash.as_deref(),
                    content,
                    &transaction.revision,
                    &mut rev_counter,
                    transaction.materialization_mode,
                    step_index,
                )?;
            }
            EditStep::CreateHeader { kind, rationale: _ } => {
                verbs::headers_footers::apply_create(
                    &mut doc,
                    true,
                    kind,
                    &transaction.revision,
                    &mut rev_counter,
                    transaction.materialization_mode,
                    step_index,
                )?;
            }
            EditStep::CreateFooter { kind, rationale: _ } => {
                verbs::headers_footers::apply_create(
                    &mut doc,
                    false,
                    kind,
                    &transaction.revision,
                    &mut rev_counter,
                    transaction.materialization_mode,
                    step_index,
                )?;
            }
            EditStep::SetHeaderFooterMode {
                title_page,
                even_and_odd,
                link,
                rationale: _,
            } => {
                verbs::headers_footers::apply_set_mode(
                    &mut doc,
                    *title_page,
                    *even_and_odd,
                    link.clone(),
                    step_index,
                )?;
            }
            EditStep::InsertEquation {
                block_id,
                expect,
                semantic_hash,
                omml,
                placement,
                rationale: _,
            } => {
                let revision = stamp_revision(&transaction.revision, &mut rev_counter);
                verbs::equations::apply(
                    &mut doc,
                    block_id,
                    expect,
                    semantic_hash.as_deref(),
                    omml,
                    *placement,
                    &revision,
                    transaction.materialization_mode,
                    step_index,
                )?;
            }
            EditStep::BlocksToTable {
                from_block_id,
                to_block_id,
                delimiter,
                header,
                rationale: _,
            } => {
                // Validate + build the table spec (pure; no mutation). Every
                // failure is an explicit, addressed EditError.
                let plan = verbs::blocks_to_table::plan(
                    &doc,
                    from_block_id,
                    to_block_id,
                    delimiter,
                    header.as_deref(),
                    step_index,
                )?;

                // Resolve the table spec into a fresh BlockNode::Table. This is
                // the SAME resolver `insert(table)` / `replace(table)` use, so
                // the inserted table rides the one materializer.
                let mut table_block = resolve_table_spec(&doc, &plan.table, step_index)?;
                let base_id = block_id_of(&table_block).clone();
                let table_id = unique_inserted_block_id(&doc.blocks, &base_id);
                if let BlockNode::Table(t) = &mut table_block {
                    t.id = table_id;
                }

                match transaction.materialization_mode {
                    MaterializationMode::TrackedChange => {
                        // Tracked DELETE of the source paragraph range (run
                        // <w:del>/<w:delText> + paragraph-mark deletion) — the
                        // SAME path DeleteBlockRange uses.
                        apply_delete_block_range(
                            &mut doc,
                            plan.start,
                            plan.end,
                            &transaction.revision,
                            &mut rev_counter,
                        );
                        // Stamp the whole table as a tracked INSERT: rows
                        // (<w:trPr><w:ins/></w:trPr>), cells (<w:cellIns>), AND
                        // cell runs/paragraph marks (<w:ins>). One shared
                        // insert revision for the table.
                        let insert_rev = TrackingStatus::Inserted(next_revision(
                            &transaction.revision,
                            &mut rev_counter,
                        ));
                        if let BlockNode::Table(t) = &mut table_block {
                            verbs::blocks_to_table::stamp_table_inserted(t, insert_rev.clone());
                        }
                        // Insert the table immediately before the (now-deleted)
                        // source range. The delete only changed statuses, so
                        // indices did not shift — insert at `plan.start`.
                        doc.blocks.insert(
                            plan.start,
                            TrackedBlock {
                                status: insert_rev,
                                block: table_block,
                                move_id: None,
                                block_sdt_wrap: None,
                            },
                        );
                        // A source range that ends the document would leave the
                        // final paragraph mark tracked-deleted — the shape Word
                        // can never resolve. Same tail rule as DeleteBlockRange.
                        normalize_final_mark(&mut doc, &transaction.revision, &mut rev_counter);
                    }
                    MaterializationMode::Direct => {
                        // Direct == the accepted state: source paragraphs gone,
                        // table in their place. When the source range ENDS the
                        // document, the last source paragraph's mark survives
                        // as an empty final paragraph — a body must end with a
                        // paragraph mark (Word leaves exactly this survivor
                        // when converting trailing text to a table), and it
                        // keeps accept-all(tracked) == direct.
                        let survivor = if plan.end + 1 == doc.blocks.len() {
                            match &doc.blocks[plan.end].block {
                                BlockNode::Paragraph(last_src) => {
                                    let mut survivor = last_src.clone();
                                    survivor.segments = Vec::new();
                                    survivor.para_mark_status = None;
                                    Some(survivor)
                                }
                                _ => None,
                            }
                        } else {
                            None
                        };
                        doc.blocks.drain(plan.start..=plan.end);
                        doc.blocks.insert(
                            plan.start,
                            TrackedBlock {
                                status: TrackingStatus::Normal,
                                block: table_block,
                                move_id: None,
                                block_sdt_wrap: None,
                            },
                        );
                        if let Some(survivor) = survivor {
                            doc.blocks.insert(
                                plan.start + 1,
                                TrackedBlock {
                                    status: TrackingStatus::Normal,
                                    block: BlockNode::Paragraph(survivor),
                                    move_id: None,
                                    block_sdt_wrap: None,
                                },
                            );
                        }
                    }
                }
                // The inserted/deleted blocks shifted positions; any cached
                // after-anchor bookkeeping is stale for subsequent steps.
                insert_order_state.by_anchor.clear();
            }
            EditStep::WrapInContentControl {
                block_id,
                expect,
                semantic_hash,
                spec,
                rationale: _,
            } => {
                // Untracked/structural (no w:sdtChange envelope): materialization
                // mode does not change behavior. A data-bound control stages a
                // CustomXmlPart into `pending.custom_xml`; the save path
                // (runtime::apply_pending_custom_xml) authors the datastore part.
                verbs::content_controls::apply_wrap(
                    &mut doc,
                    block_id,
                    expect,
                    semantic_hash.as_deref(),
                    spec,
                    &mut pending.custom_xml,
                    step_index,
                )?;
            }
            EditStep::WrapBlocksInContentControl {
                start_block_id,
                end_block_id,
                spec,
                rationale: _,
            } => {
                // Untracked/structural (no w:sdtChange envelope): materialization
                // mode does not change behavior, exactly like the inline wrap.
                verbs::block_content_controls::apply_wrap_blocks(
                    &mut doc,
                    start_block_id,
                    end_block_id,
                    spec,
                    step_index,
                )?;
            }
            EditStep::SetContentControlValue {
                block_id,
                sdt_id,
                value,
                tracked,
                rationale: _,
            } => {
                // A tracked SDT set needs the projector to descend into
                // sdtContent revisions (B1), which is not implemented — refuse
                // rather than silently downgrade to untracked.
                if *tracked {
                    return Err(EditError::TrackedContentControlSetUnsupported {
                        sdt_id: sdt_id.clone(),
                        step_index,
                    });
                }
                verbs::content_controls::apply_set_value(
                    &mut doc, block_id, sdt_id, value, step_index,
                )?;
            }
            EditStep::SetFormFieldValue {
                block_id,
                field_id,
                value,
                semantic_hash,
                rationale: _,
            } => {
                verbs::form_fields::apply_set_value(
                    &mut doc,
                    block_id,
                    field_id,
                    value,
                    semantic_hash.as_deref(),
                    step_index,
                )?;
            }
            EditStep::InsertImage {
                block_id,
                expect,
                semantic_hash,
                image,
                rationale: _,
            } => {
                // Stages a PendingMedia into `pending.media`; the save path
                // (runtime::apply_pending_media) writes the binary, registers the
                // image rel, and rewrites the logical rId to the real one.
                verbs::image_insert::apply_insert(
                    &mut doc,
                    block_id,
                    expect.as_deref(),
                    semantic_hash.as_deref(),
                    image,
                    &transaction.revision,
                    transaction.materialization_mode,
                    step_index,
                    &mut pending.media,
                )?;
            }
            EditStep::ReplaceImage {
                block_id,
                drawing_id,
                semantic_hash,
                image,
                allow_stretch,
                rationale: _,
            } => {
                // Direct/untracked: rewrites the drawing's blip rId to a new
                // logical rId, applies the requested display extent, and stages
                // the new binary. Old media left unreferenced.
                verbs::image_insert::apply_replace(
                    &mut doc,
                    block_id,
                    drawing_id,
                    semantic_hash.as_deref(),
                    &verbs::image_insert::ReplaceRequest {
                        image,
                        allow_stretch: *allow_stretch,
                    },
                    step_index,
                    &mut pending.media,
                )?;
            }
            EditStep::SetTextboxText {
                block_id,
                drawing_id,
                paragraphs,
                semantic_hash,
                rationale: _,
            } => {
                // Direct/untracked: replaces the txbxContent children in the
                // drawing's raw_xml. Refuses if the interior already has tracked
                // changes (don't flatten).
                verbs::textbox::apply_set_text(
                    &mut doc,
                    block_id,
                    drawing_id,
                    paragraphs,
                    semantic_hash.as_deref(),
                    step_index,
                )?;
            }
            EditStep::OpaqueTextEdit {
                block_id,
                opaque_id,
                container_index,
                paragraph_index,
                find,
                replacement,
                semantic_hash,
                rationale: _,
            } => {
                // Surgical interior text splice. Tracked/direct follows the
                // transaction mode; mints fresh unique revision ids from
                // `rev_counter` for the w:ins/w:del (Phase-3-resolvable).
                verbs::opaque_text_edit::apply(
                    &mut doc,
                    block_id,
                    opaque_id,
                    *container_index,
                    *paragraph_index,
                    find,
                    replacement,
                    semantic_hash.as_deref(),
                    &transaction.revision,
                    &mut rev_counter,
                    transaction.materialization_mode == MaterializationMode::TrackedChange,
                    step_index,
                )?;
            }
            EditStep::SdtTextFill {
                block_id,
                sdt_id,
                body_index,
                value,
                semantic_hash,
                rationale: _,
            } => {
                // Inline: splices the control's raw_xml in place. Block: validates
                // and stages an OpaqueChildTextSet for the save path (the scaffold
                // holds the bytes). Tracked/direct follows the transaction mode.
                verbs::sdt_text_fill::apply(
                    &mut doc,
                    block_id.as_ref(),
                    sdt_id.as_ref(),
                    *body_index,
                    value,
                    semantic_hash.as_deref(),
                    &transaction.revision,
                    &mut rev_counter,
                    transaction.materialization_mode == MaterializationMode::TrackedChange,
                    &mut pending,
                    step_index,
                )?;
            }
            EditStep::CreateStyle { def, rationale: _ } => {
                // Package-level, untracked: stages a StyleOp::Create. Does not
                // mutate the body IR. The save path splices it after the style
                // merge and fails loud if the styleId already exists.
                verbs::style_defs::apply_create(def, step_index, &mut pending.style_ops)?;
            }
            EditStep::ModifyStyle {
                style_id,
                def,
                rationale: _,
            } => {
                verbs::style_defs::apply_modify(style_id, def, step_index, &mut pending.style_ops)?;
            }
            EditStep::SetDocDefaults {
                font_family,
                font_size_half_points,
                rationale: _,
            } => {
                // Package-level, untracked: stages a StyleOp::SetDocDefaults. Does
                // not mutate the body IR. The save path property-merges it into
                // w:docDefaults/w:rPrDefault/w:rPr.
                verbs::style_defs::apply_set_doc_defaults(
                    font_family.as_deref(),
                    *font_size_half_points,
                    step_index,
                    &mut pending.style_ops,
                )?;
            }
        }
        if let Some(snapshot) = &range_marker_snapshot {
            crate::tracked_model::repair_torn_range_markers(&mut doc.blocks, snapshot);
        }
    }

    // H7: authoring creates new revisions with identity 0; mint stable
    // identities for them (existing identities are preserved) before the doc
    // leaves the producer, so enumerate/Selective can address them.
    crate::import::mint_identities(&mut doc);
    // H2: one unified body-state validator after this producer's normalizers.
    crate::tracked_model::debug_assert_body_invariants(&doc, "apply_transaction");
    Ok((doc, pending))
}

/// Re-attribute a tracked mark that a paragraph-append/delete/move step placed on
/// the document-final paragraph mark to the preceding mark (see
/// [`crate::tracked_model::normalize_final_mark_attribution`]). Called from the
/// paragraph-structural step handlers and `MoveBlockRange` (whose moveTo
/// destination can end the document, leaving an unresolvable mark on the final
/// pilcrow just as an append does — the move-aware branch handles it while
/// leaving the moveFrom shadow's own pairing intact). NOT called from
/// `BlocksToTable` (its trailing deleted source paragraphs are a table
/// conversion, not a tail delete).
fn normalize_final_mark(doc: &mut CanonDoc, revision: &RevisionInfo, rev_counter: &mut u32) {
    crate::tracked_model::normalize_final_mark_attribution(&mut doc.blocks, revision, rev_counter);
}

/// Apply a `ReplaceTable` step.
///
/// Pipeline (mirrors the merge-diff path at `diff.rs::compute_table_diff_result`
/// + `tracked_model::apply_table_structure_changed`):
///
/// 1. Locate the target table by id; fail with `BlockNotFound` or
///    `NotATable` if the address doesn't resolve to a top-level table block.
/// 2. Fail-fast on the one remaining v4-schema gap: non-default table/row/cell
///    formatting (merged cells and header rows are now expressible). The error
///    names the location so the LLM (or human reviewer) sees the source.
/// 3. Optionally check the caller-supplied `semantic_hash` against the
///    base table; mismatch means a stale snapshot — fail loudly.
/// 4. Resolve the spec into a fresh `TableNode`. Rename the root id to
///    match the base so the diff aligns on identity.
/// 5. Run `compute_table_diff_result` to align rows and cells.
/// 6. Short-circuit (invariant I9): if the diff says nothing changed, do
///    not mutate the block — preserves "edits with no net effect are
///    no-ops".
/// 7. `Direct` mode: replace the table outright with the target. The
///    fail-fast checks already guarantee the base table has nothing we'd
///    silently lose by overwriting.
/// 8. `TrackedChange` mode: feed the diff into
///    `apply_table_structure_changed`, which emits the row/cell-level
///    tracked changes (`w:trPr/w:ins`, `w:trPr/w:del`, `w:cellIns`,
///    `w:cellDel`, and inline `w:ins`/`w:del` inside modified cells per
///    OOXML §17.13.5).
#[allow(clippy::too_many_arguments)]
fn apply_replace_table(
    doc: &mut CanonDoc,
    block_id: &NodeId,
    semantic_hash: Option<&str>,
    replacement: &TableBlockSpec,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    materialization_mode: MaterializationMode,
    step_index: usize,
) -> Result<(), EditError> {
    let idx = find_block_index(&doc.blocks, block_id).ok_or_else(|| EditError::BlockNotFound {
        block_id: block_id.clone(),
        step_index,
    })?;

    let base_table = match &doc.blocks[idx].block {
        BlockNode::Table(t) => t.clone(),
        BlockNode::Paragraph(_) => {
            return Err(EditError::NotATable {
                block_id: block_id.clone(),
                actual_kind: "paragraph",
                step_index,
            });
        }
        BlockNode::OpaqueBlock(_) => {
            return Err(EditError::NotATable {
                block_id: block_id.clone(),
                actual_kind: "opaque_block",
                step_index,
            });
        }
    };

    // RFC-0003: `replace(table)` no longer refuses a formatted base. We carry
    // the base's formatting onto the resolved target below, so a whole-table
    // replace round-trips borders/shading/widths instead of dropping them. The
    // only base state still refused is an UNRESOLVED tracked change (the
    // structural diff can't layer a fresh revision over an in-flight one) — the
    // same narrow guard the granular ops use.
    validate_table_not_mid_redline(&base_table, step_index, None)?;

    // RFC-0003 Item 1: caller-SET formatting on a TRACKED replace can't be a
    // reversible tracked change (table/row `*PrChange` doesn't cover style, and
    // applying it directly would break the reject-all == base invariant). Refuse
    // and point to direct mode (which applies the spec wholesale) or the in-place
    // `Set*Formatting` verbs (which track a formatting change properly). Spec
    // formatting on a DIRECT replace, and on an `insert(table)` (where the whole
    // new table is the tracked insert), is fine.
    if materialization_mode == MaterializationMode::TrackedChange
        && table_spec_has_formatting(replacement)
    {
        return Err(EditError::TableSpecFormattingRequiresDirect {
            block_id: block_id.clone(),
            step_index,
        });
    }

    if let Some(expected) = semantic_hash
        && let Err(actual) = check_block_guard(&doc.blocks[idx].block, expected)
    {
        return Err(EditError::BlockSemanticHashMismatch {
            block_id: block_id.clone(),
            expected: expected.to_string(),
            actual,
            step_index,
        });
    }

    // Resolve the spec. The freshly-resolved table gets a placeholder id;
    // we rename it to match the base so diffing aligns on identity.
    let mut target_block = resolve_table_spec(doc, replacement, step_index)?;
    let target_table_owned = match &mut target_block {
        BlockNode::Table(t) => {
            t.id = base_table.id.clone();
            t
        }
        _ => unreachable!("resolve_table_spec returns BlockNode::Table"),
    };
    // Overlay the base table's formatting onto the resolved (formatting-free)
    // target so the replace preserves it (RFC-0003).
    carry_base_formatting_onto_target(&base_table, target_table_owned);
    let target_table: TableNode = (**target_table_owned).clone();

    lower_table_target(
        doc,
        idx,
        block_id,
        &base_table,
        &target_table,
        revision,
        rev_counter,
        materialization_mode,
        "edit_replace_table",
        step_index,
    )
}

/// Lower a (base, target) table pair into the document at `idx` through the
/// SAME table-diff machinery `ReplaceTable` uses. This is the shared tail for
/// every whole-table and granular table op: compute the structural diff, honor
/// the I9 no-op short-circuit, and either overwrite (Direct) or emit row/cell
/// tracked changes via `apply_table_structure_changed` (TrackedChange). It is
/// NOT the materializer (Invariant M) — it builds the materializer's input
/// (`target_table` + the diff) and calls it.
///
/// `block_id` must equal `base_table.id`, and `doc.blocks[idx].block` must be
/// the same table (the caller located it). The target must already be a valid
/// table whose id matches the base (so the diff aligns on identity).
#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_table_target(
    doc: &mut CanonDoc,
    idx: usize,
    block_id: &NodeId,
    base_table: &TableNode,
    target_table: &TableNode,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    materialization_mode: MaterializationMode,
    context: &'static str,
    step_index: usize,
) -> Result<(), EditError> {
    // Compute the diff. compute_table_diff_result returns an error if
    // canonicalization fails (e.g., malformed table). The fail-fast checks
    // upstream prevent the known causes of malformedness, so map any
    // remaining failure to UnsupportedParagraphStructure with context.
    let diff = crate::diff::compute_table_diff_result(base_table, target_table).map_err(|e| {
        EditError::UnsupportedParagraphStructure {
            block_id: block_id.clone(),
            reason: format!("table canonicalization failed: {e}"),
            step_index,
        }
    })?;

    // I9 short-circuit: if the diff reports no structural row changes AND
    // no modified cells, the op is a no-op. Leave the block untouched.
    if is_table_diff_identity(&diff) {
        return Ok(());
    }

    if materialization_mode == MaterializationMode::Direct {
        // Direct mode: overwrite the base table with the freshly-resolved
        // target. The fail-fast checks guarantee we don't drop anything
        // we'd otherwise preserve. structure_hash gets updated to the
        // target's so subsequent diffs see the new shape.
        doc.blocks[idx].block = BlockNode::from(target_table.clone());
        return Ok(());
    }

    // Tracked-change mode: feed the diff to apply_table_structure_changed,
    // which is the same function the merge-diff path uses. It produces
    // a merged TableNode in place with the right row/cell tracking flags.
    apply_table_structure_changed(
        &mut doc.blocks,
        block_id,
        target_table,
        &diff,
        revision,
        rev_counter,
        context,
    )
    .map_err(|e| EditError::UnsupportedParagraphStructure {
        block_id: block_id.clone(),
        reason: format!(
            "apply_table_structure_changed failed: {} ({})",
            e.message, e.context
        ),
        step_index,
    })?;
    Ok(())
}

/// Return true when a table diff has no structural changes (every row is
/// Matched) and no cell content changes (no Modified or MergeChanged cell
/// diffs and no nested-table diffs).
///
/// Used as the I9 short-circuit in `apply_replace_table`: a replace that
/// resolves to the same table must leave the block untouched (no spurious
/// tracked changes).
fn is_table_diff_identity(diff: &crate::domain::TableDiffResult) -> bool {
    let all_matched = diff
        .row_alignment
        .iter()
        .all(|a| matches!(a, crate::domain::TableRowAlignment::Matched { .. }));
    if !all_matched {
        return false;
    }
    diff.cell_diffs.iter().all(|c| {
        matches!(c.diff_type, crate::domain::TableCellDiffType::Unchanged)
            && c.text_diff.is_none()
            && c.nested_table_diffs.is_empty()
    })
}

/// Narrow structural guard for the granular table ops (insert/delete row/
/// column, merge). Unlike the whole-table REPLACE path, these ops build their
/// target by CLONING the base `TableNode` and mutating the clone, so every
/// table/row/cell property the IR models is carried through byte-identically —
/// there is nothing for the v4 grammar to drop, and the old blanket
/// `validate_base_table_v4_compatible` formatting refusal (which refused every
/// table carrying formatting the v4 grammar could not express) no longer applies.
///
/// What DOES still have to be refused is a base carrying a PRE-EXISTING
/// unresolved tracked change — a row/cell tracked insert or delete, or a pending `tblPrChange`/
/// `trPrChange`/`tcPrChange`. The structural diff (`compute_table_diff_result`
/// → `apply_table_structure_changed`) assumes a clean base; layering a fresh
/// revision over an in-flight one would interleave two change layers ambiguously
/// (RFC-0003 keeps row-level tracked-change markup out of the edit schema — it
/// belongs to the revision model). Fail loud and point at the in-flight change
/// so the caller accepts/rejects it first. Granular table ops may, however,
/// compose over row/cell statuses minted earlier in the SAME atomic
/// transaction; `transaction_floor` identifies those engine-minted ids. This
/// is how one plan can insert one row and delete another without resolving its
/// own first step. Whole-table replacement passes no floor and retains the
/// strict clean-base rule.
pub(crate) fn validate_table_not_mid_redline(
    base: &TableNode,
    step_index: usize,
    transaction_floor: Option<u32>,
) -> Result<(), EditError> {
    if base.formatting_change.is_some() {
        return Err(EditError::TableMidRedline {
            table_id: base.id.clone(),
            location: "table (tblPrChange)".to_string(),
            step_index,
        });
    }
    for (row_index, row) in base.rows.iter().enumerate() {
        if row.tracking_status.as_ref().is_some_and(|status| {
            !tracking_status_belongs_to_transaction(status, transaction_floor)
        }) {
            return Err(EditError::TableMidRedline {
                table_id: base.id.clone(),
                location: format!("row[{row_index}] (tracked row ins/del)"),
                step_index,
            });
        }
        if row.formatting_change.is_some() {
            return Err(EditError::TableMidRedline {
                table_id: base.id.clone(),
                location: format!("row[{row_index}] (trPrChange)"),
                step_index,
            });
        }
        for (cell_index, cell) in row.cells.iter().enumerate() {
            if cell.tracking_status.as_ref().is_some_and(|status| {
                !tracking_status_belongs_to_transaction(status, transaction_floor)
            }) {
                return Err(EditError::TableMidRedline {
                    table_id: base.id.clone(),
                    location: format!("row[{row_index}].cell[{cell_index}] (tracked cell ins/del)"),
                    step_index,
                });
            }
            if cell.formatting_change.is_some() {
                return Err(EditError::TableMidRedline {
                    table_id: base.id.clone(),
                    location: format!("row[{row_index}].cell[{cell_index}] (tcPrChange)"),
                    step_index,
                });
            }
        }
    }
    Ok(())
}

fn tracking_status_belongs_to_transaction(
    status: &TrackingStatus,
    transaction_floor: Option<u32>,
) -> bool {
    let Some(floor) = transaction_floor else {
        return false;
    };
    match status {
        TrackingStatus::Normal => true,
        TrackingStatus::Inserted(revision) | TrackingStatus::Deleted(revision) => {
            revision.revision_id >= floor
        }
        TrackingStatus::InsertedThenDeleted(stacked) => {
            stacked.inserted.revision_id >= floor && stacked.deleted.revision_id >= floor
        }
    }
}

/// The logical column count of a table's first row (`gridBefore` + Σ gridSpan +
/// `gridAfter`). Used to decide whether a `replace(table)` keeps the same column
/// structure (so per-column `tblGrid` widths can carry across).
fn table_logical_width(t: &TableNode) -> u32 {
    t.rows
        .first()
        .map(|r| {
            r.grid_before + r.cells.iter().map(|c| c.grid_span.max(1)).sum::<u32>() + r.grid_after
        })
        .unwrap_or(0)
}

/// RFC-0003: carry the BASE table's formatting onto the freshly-resolved REPLACE
/// target so `replace(table)` round-trips formatting instead of silently
/// dropping it. The v4 table spec (`TableBlockSpec`) carries content + merge
/// structure but no formatting, so `resolve_table_spec` produces a target with
/// default `tblPr`/`trPr`/`tcPr`. We overlay the base's formatting:
///
/// - **Table-level** (`tblPr`) always carries. `tblGrid` (per-column widths)
///   carries only when the column count is unchanged; a replace that changes the
///   column count can't map old widths onto new columns, so it keeps the
///   resolved target's grid (same as an unformatted replace produces today).
/// - **Row-level** (`trPr`: height, cantSplit, jc, cnfStyle, tblPrEx,
///   cellSpacing, wBefore/After, preserved) carries onto the row at the same
///   index. `is_header`, `gridBefore/After`, and the cell list stay
///   spec-controlled (the caller may restructure them).
/// - **Cell-level** (`tcPr`: the full `CellFormatting`, cnfStyle, hideMark,
///   preserved) carries onto the cell at the same `{row, col}` index ONLY when
///   that row's cell count is unchanged. A cell the replace adds/removes has no
///   source formatting to carry — that is a legitimate new/dropped cell, not a
///   silent drop of a surviving one.
///
/// Because the carried target formatting then equals the base's for every
/// matched cell, `apply_table_structure_changed`'s `apply_cell_formatting_change`
/// is a no-op there (it only records a `tcPrChange` when they differ), and
/// Direct-mode replace (which overwrites with the target) now keeps the
/// formatting too.
///
/// **Fill-if-default, not overwrite** (RFC-0003 Item 1): the target may already
/// carry caller-SET formatting from the spec (`TableBlockSpec.formatting`,
/// `TableRowSpec.height`, `TableCellSpec.formatting`). The base is carried only
/// into slots the spec left at their default, so a caller-specified value ALWAYS
/// wins and unset slots still inherit the base — a replace can rewrite content,
/// restyle some cells, and preserve the rest in one op. When the spec carries no
/// formatting (the common case) every target slot is default, so this is
/// equivalent to the old whole-node carry.
/// Whether a `TableBlockSpec` carries any caller-set formatting (RFC-0003
/// Item&nbsp;1): table-level `tblPr`, any row height, or any cell `tcPr`. Used to
/// refuse spec formatting on a TRACKED replace (see `apply_replace_table`).
fn table_spec_has_formatting(spec: &TableBlockSpec) -> bool {
    spec.formatting.is_some()
        || spec.rows.iter().any(|r| {
            r.height.is_some()
                || r.height_rule.is_some()
                || r.cells.iter().any(|c| c.formatting.is_some())
        })
}

fn carry_base_formatting_onto_target(base: &TableNode, target: &mut TableNode) {
    let same_columns = table_logical_width(base) == table_logical_width(target);
    merge_table_formatting_from_base(&mut target.formatting, &base.formatting, same_columns);

    for (row_idx, trow) in target.rows.iter_mut().enumerate() {
        let Some(brow) = base.rows.get(row_idx) else {
            continue;
        };
        // Row-level trPr: fill each slot the spec left unset from the base row
        // (NOT is_header / gridBefore/After / cells — spec-controlled structure).
        if trow.height.is_none() {
            trow.height = brow.height;
            trow.height_rule = brow.height_rule.clone();
        }
        if !trow.cant_split {
            trow.cant_split = brow.cant_split;
        }
        if trow.jc.is_none() {
            trow.jc = brow.jc.clone();
        }
        if trow.cnf_style.is_none() {
            trow.cnf_style = brow.cnf_style.clone();
        }
        if trow.tbl_pr_ex.is_none() {
            trow.tbl_pr_ex = brow.tbl_pr_ex.clone();
        }
        if trow.cell_spacing.is_none() {
            trow.cell_spacing = brow.cell_spacing;
        }
        if trow.w_before.is_none() {
            trow.w_before = brow.w_before.clone();
        }
        if trow.w_after.is_none() {
            trow.w_after = brow.w_after.clone();
        }
        if trow.preserved.is_empty() {
            trow.preserved = brow.preserved.clone();
        }
        // Cell-level tcPr (whole-cell granularity): fill only cells the spec left
        // unformatted, and only when the row's cell count is unchanged.
        if trow.cells.len() == brow.cells.len() {
            for (tcell, bcell) in trow.cells.iter_mut().zip(brow.cells.iter()) {
                if tcell.formatting == CellFormatting::default() {
                    tcell.formatting = bcell.formatting.clone();
                }
                if tcell.cnf_style.is_none() {
                    tcell.cnf_style = bcell.cnf_style.clone();
                }
                if !tcell.hide_mark {
                    tcell.hide_mark = bcell.hide_mark;
                }
                if tcell.preserved.is_empty() {
                    tcell.preserved = bcell.preserved.clone();
                }
            }
        }
    }
}

/// Fill each table-level `tblPr` slot the target left at its default from the
/// base, carrying the base's `has_direct_*` provenance with the value (so a
/// style-inherited base value is not re-materialized as direct markup). A slot
/// the target already set (from `TableBlockSpec.formatting`) is kept. `tblGrid`
/// carries only when the column count is unchanged.
fn merge_table_formatting_from_base(
    target: &mut TableFormatting,
    base: &TableFormatting,
    same_columns: bool,
) {
    if target.style_id.is_none() {
        target.style_id = base.style_id.clone();
    }
    if target.tbl_look.is_none() {
        target.tbl_look = base.tbl_look.clone();
        target.has_direct_tbl_look = base.has_direct_tbl_look;
    }
    if target.borders.is_none() {
        target.borders = base.borders.clone();
        target.has_direct_borders = base.has_direct_borders;
    }
    if target.width.is_none() {
        target.width = base.width.clone();
    }
    if target.default_cell_margins.is_none() {
        target.default_cell_margins = base.default_cell_margins.clone();
        target.has_direct_cell_margins = base.has_direct_cell_margins;
    }
    if target.alignment.is_none() {
        target.alignment = base.alignment.clone();
        target.has_direct_alignment = base.has_direct_alignment;
    }
    if target.indent.is_none() {
        target.indent = base.indent;
        target.has_direct_indent = base.has_direct_indent;
    }
    if target.layout.is_none() {
        target.layout = base.layout.clone();
    }
    if target.cell_spacing.is_none() {
        target.cell_spacing = base.cell_spacing;
    }
    if target.positioning.is_none() {
        target.positioning = base.positioning.clone();
    }
    if target.overlap.is_none() {
        target.overlap = base.overlap.clone();
    }
    if target.row_band_size.is_none() {
        target.row_band_size = base.row_band_size;
    }
    if target.col_band_size.is_none() {
        target.col_band_size = base.col_band_size;
    }
    if target.shading.is_none() {
        target.shading = base.shading.clone();
    }
    if !target.bidi_visual {
        target.bidi_visual = base.bidi_visual;
    }
    if target.caption.is_none() {
        target.caption = base.caption.clone();
    }
    if target.description.is_none() {
        target.description = base.description.clone();
    }
    if target.preserved.is_empty() {
        target.preserved = base.preserved.clone();
    }
    // tblGrid: fill from the base only when the column count is unchanged (per-
    // column widths can't map onto a changed column count).
    if target.grid_cols.iter().all(|&w| w == 0) && same_columns {
        target.grid_cols = base.grid_cols.clone();
    }
}

/// In-place cell-text edit: replace the TEXT of the cell at logical grid
/// position `{row_index, col_index}` by routing the cell's paragraph through
/// the SAME paragraph-text materializer `ReplaceParagraphText` uses
/// (`apply_replace_paragraph_text`). This deliberately does NOT route through
/// the whole-table replace schema (`lower_table_target`): editing one cell's text
/// touches neither the table's `tblPr`, the row's `trPr`, the cell's `tcPr`, nor
/// any other cell, so it needs none of the whole-table machinery.
/// Everything except the target cell's paragraph segments is byte-preserved.
///
/// Addressing matches the read view (`view::table_cell_views`): `col_index` is
/// the LOGICAL grid column (after `gridBefore`, advancing by each cell's
/// `gridSpan`), so the address a cold agent read off `read_block.cells` resolves
/// to the same cell here.
///
/// Fail loud (no silent fallback):
/// - `row_index` past the last row → `TableRowIndexOutOfRange`;
/// - `col_index` not the start column of any physical cell in that row →
///   `TableColumnIndexOutOfRange` (e.g. addressing the interior of a
///   horizontally-merged cell);
/// - a vertical-merge CONTINUE cell → `TableCellNotEditable`: its text lives in
///   the merge anchor (a higher row), so editing the continuation is ambiguous
///   — target the anchor cell instead;
/// - a cell carrying a tracked structural insert/delete → `TableCellNotEditable`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_set_cell_text_in_place(
    doc: &mut CanonDoc,
    idx: usize,
    block_id: &NodeId,
    row_index: usize,
    col_index: usize,
    text: &str,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    transaction_floor: u32,
    materialization_mode: MaterializationMode,
    step_index: usize,
) -> Result<(), EditError> {
    let table = match &mut doc.blocks[idx].block {
        BlockNode::Table(t) => t,
        _ => unreachable!("caller verified the block at idx is a table"),
    };

    if row_index >= table.rows.len() {
        return Err(EditError::TableRowIndexOutOfRange {
            block_id: block_id.clone(),
            row_index,
            row_count: table.rows.len(),
            step_index,
        });
    }
    let row = &mut table.rows[row_index];

    // Resolve `col_index` (a LOGICAL grid column) to the physical cell that
    // STARTS at that column, advancing by each cell's gridSpan after the row's
    // gridBefore — exactly how `view::table_cell_views` mints the address. We
    // require an exact start match: addressing the interior of a spanning cell
    // is out of range rather than silently snapped to the anchor.
    let mut col = row.grid_before as usize;
    let mut found: Option<usize> = None;
    let mut logical_width = col;
    for (phys_idx, cell) in row.cells.iter().enumerate() {
        if col == col_index {
            found = Some(phys_idx);
        }
        col += cell.grid_span.max(1) as usize;
        logical_width = col;
    }
    logical_width += row.grid_after as usize;
    let phys_idx = found.ok_or_else(|| EditError::TableColumnIndexOutOfRange {
        block_id: block_id.clone(),
        col_index,
        column_count: logical_width,
        step_index,
    })?;

    // A whole-row insert/delete carries its tracked-structural status on the
    // ROW (`w:trPr/w:ins`|`w:del`), leaving the cells markerless (see
    // `tracked_model::mark_whole_row_inserted` / `_deleted`); a cell-level op
    // (column insert/delete, cell merge) carries it on the CELL (`w:cellIns`|
    // `w:cellDel`). The two are mutually exclusive, so a cell's EFFECTIVE
    // tracked-structural status is its own if present, else the enclosing row's.
    // Editability must consult both, or a cell in a foreign inserted/deleted row
    // would read as clean.
    let row_status = row.tracking_status.clone();
    let cell = &mut row.cells[phys_idx];

    // A vertical-merge CONTINUE cell holds no content of its own — the text
    // belongs to the merge anchor in a higher row. Editing the continuation is
    // ambiguous, so refuse and point at the anchor (no silent retarget).
    if cell.v_merge == VerticalMerge::Continue {
        return Err(EditError::TableCellNotEditable {
            block_id: block_id.clone(),
            reason: format!(
                "cell at row {row_index}, col {col_index} is a vertical-merge \
                 continuation; its text lives in the merge anchor — edit the anchor cell"
            ),
            step_index,
        });
    }

    // A cell carrying a tracked structural insert/delete is not automatically a
    // clean text target — UNLESS the insert is this cell's OWN pending
    // insertion, authored earlier in THIS SAME transaction (e.g. an
    // `insert_row` step with no `cells`, followed by `set_cell_text` on the
    // new row, both in one `apply_edit` call). Writing content into your own
    // pending insertion is part of authoring the insertion, not a second
    // revision layered on top — the row/cell already carries `w:trPr/w:ins` /
    // `w:cellIns`, so the text belongs to that same insertion.
    //
    // "This same transaction" is detected honestly, not via caller-supplied
    // metadata: `transaction_floor` is the revision-id counter's value
    // BEFORE this transaction minted anything (see `apply_transaction`), and
    // every stamp this transaction produces (via `next_revision`) has
    // `revision_id >= transaction_floor`. A `Deleted` or
    // `InsertedThenDeleted` status, or an `Inserted` status predating this
    // transaction, is always foreign.
    let effective_status = cell.tracking_status.clone().or(row_status);
    let own_pending_insert = matches!(
        &effective_status,
        Some(TrackingStatus::Inserted(rev)) if rev.revision_id >= transaction_floor
    );
    if let Some(status) = &effective_status
        && !own_pending_insert
    {
        let revision_ids = match status {
            TrackingStatus::Normal => Vec::new(),
            TrackingStatus::Inserted(rev) | TrackingStatus::Deleted(rev) => vec![rev.revision_id],
            TrackingStatus::InsertedThenDeleted(stacked) => {
                vec![stacked.inserted.revision_id, stacked.deleted.revision_id]
            }
        };
        let ids = revision_ids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let cell_para_id = cell.blocks.iter().find_map(|b| match b {
            BlockNode::Paragraph(p) => Some(p.id.clone()),
            _ => None,
        });
        let alternative = match cell_para_id {
            Some(id) => {
                format!("or address the cell paragraph block '{id}' with a tracked replace")
            }
            None => {
                "or address the cell's content block directly with a tracked replace".to_string()
            }
        };
        return Err(EditError::TableCellNotEditable {
            block_id: block_id.clone(),
            reason: format!(
                "cell at row {row_index}, col {col_index} carries a pre-existing tracked \
                 revision (id {ids}); accept or reject revision {ids} first, {alternative}"
            ),
            step_index,
        });
    }

    // Locate the cell's editable paragraph. A cell normally holds exactly one
    // paragraph; if it holds several, we edit the FIRST (the read view's
    // `cell_text` joins them, but a one-run set targets the first paragraph,
    // matching the previous SetCellText shape). A cell with no paragraph at all
    // is degenerate (canonicalization rejects it upstream) — refuse loudly.
    let para_pos = cell
        .blocks
        .iter()
        .position(|b| matches!(b, BlockNode::Paragraph(_)))
        .ok_or_else(|| EditError::TableCellNotEditable {
            block_id: block_id.clone(),
            reason: format!("cell at row {row_index}, col {col_index} has no paragraph to edit"),
            step_index,
        })?;
    // DIRECT mode only: flatten pre-existing tracked ins/del in the cell paragraph
    // to a Normal base BEFORE the diff (flatten-then-diff), as the body
    // `ReplaceParagraphText` path does. In TRACKED mode we must NOT flatten — that
    // would silently ACCEPT the cell's prior pending changes; the materializer
    // instead diffs against the accept-all view and carries prior tombstones
    // through, so a second tracked edit preserves both changes (not "oldNEW").
    if materialization_mode == MaterializationMode::Direct {
        project_block_for_text_edit_prep(&mut cell.blocks[para_pos]);
    }
    let para = match &mut cell.blocks[para_pos] {
        BlockNode::Paragraph(p) => p,
        _ => unreachable!("para_pos points at a paragraph"),
    };

    // Build a WHOLE-paragraph replacement content: the cell's opaque inlines
    // (hyperlinks, fields, …) are carried by-ref so they survive; the visible
    // text is replaced by the single new run. This is the SAME shape
    // `ReplaceSpanText` builds for the materializer (Invariant M: we build the
    // input, we do not modify the materializer).
    let new_content = ParagraphContent {
        fragments: vec![ContentFragment::Text(text.to_string())],
    };
    // Validate opaque preservation against the new content: a `set_cell_text`
    // that would drop an opaque inline in the cell is refused, not silently
    // applied. (Plain-text replacement keeps no opaques, so this fires when the
    // cell carried one — the caller must use a span/replace verb to preserve it.)
    let anchors = collect_anchor_inventory(para);
    validate_preserved_inlines(para, block_id, &new_content, &anchors, step_index)?;

    // Setting the cell to the text it already holds changes nothing; fail loud
    // rather than reporting a no-op as a successful edit (CLAUDE.md "no silent
    // fallbacks").
    if is_identity_replacement(para, &new_content) {
        return Err(EditError::NoOpEdit {
            block_id: block_id.clone(),
            step_index,
            reason: "cell text equals the requested replacement",
        });
    }

    if own_pending_insert {
        // The paragraph's enclosing cell is ITSELF a pending insertion
        // authored earlier in this transaction: the text is part of that one
        // insertion, so write it as plain (Normal) content rather than
        // diffing — diffing against the fresh row's empty base would wrap the
        // new text in its OWN inline `w:ins`, nested inside the cell's
        // `w:cellIns` / row's `w:trPr/w:ins`, misrepresenting one insertion as
        // two stacked tracked changes.
        set_paragraph_plain_text(para, text);
    } else {
        // No enclosing block insertion: a cell paragraph's insertion axis is the
        // cell/row tracked status, and editing a FOREIGN (cross-transaction)
        // pending-inserted cell is refused above — so the only insertion reaching
        // here is `own_pending_insert`, handled by the plain-text branch.
        apply_replace_paragraph_text(para, &new_content, revision, None, rev_counter);
    }

    if materialization_mode == MaterializationMode::Direct {
        // Direct mode: immediately resolve the just-created tracked changes so
        // the cell returns to all-Normal (matching the paragraph-text path).
        project_block_for_accept_reject(&mut doc.blocks[idx].block, true);
    }

    Ok(())
}

/// Locate a hyperlink opaque inline anywhere in the document and return the
/// path to its enclosing paragraph block. Recursively descends through
/// table cells (and nested tables) so hyperlinks in any table cell are
/// reachable.
fn find_hyperlink_path(doc: &CanonDoc, hyperlink_id: &NodeId) -> Option<ParagraphPath> {
    for (top_idx, tb) in doc.blocks.iter().enumerate() {
        if let Some(descent) = find_hyperlink_in_block(&tb.block, hyperlink_id) {
            return Some(ParagraphPath {
                top_block: top_idx,
                descent,
            });
        }
    }
    None
}

/// Search a single `BlockNode` (top-level or in-cell) and any tables it
/// contains for a hyperlink with the given id. Returns the descent path
/// from the search root (empty if the hyperlink is directly inside this
/// block's paragraph).
fn find_hyperlink_in_block(block: &BlockNode, hyperlink_id: &NodeId) -> Option<Vec<CellStep>> {
    match block {
        BlockNode::Paragraph(p) => {
            if paragraph_contains_hyperlink(p, hyperlink_id) {
                Some(Vec::new())
            } else {
                None
            }
        }
        BlockNode::Table(t) => find_hyperlink_in_table(t, hyperlink_id),
        BlockNode::OpaqueBlock(_) => None,
    }
}

fn find_hyperlink_in_table(
    table: &crate::domain::TableNode,
    hyperlink_id: &NodeId,
) -> Option<Vec<CellStep>> {
    for (row_idx, row) in table.rows.iter().enumerate() {
        for (cell_idx, cell) in row.cells.iter().enumerate() {
            for (block_in_cell_idx, block) in cell.blocks.iter().enumerate() {
                if let Some(mut deeper) = find_hyperlink_in_block(block, hyperlink_id) {
                    deeper.insert(
                        0,
                        CellStep {
                            row_idx,
                            cell_idx,
                            block_in_cell_idx,
                        },
                    );
                    return Some(deeper);
                }
            }
        }
    }
    None
}

fn paragraph_contains_hyperlink(p: &ParagraphNode, hyperlink_id: &NodeId) -> bool {
    p.segments.iter().any(|seg| {
        seg.inlines.iter().any(|inline| {
            matches!(inline, InlineNode::OpaqueInline(o)
                if &o.id == hyperlink_id
                && matches!(o.kind, OpaqueKind::Hyperlink(_)))
        })
    })
}

/// Stamp a `RevisionInfo` from the transaction's base revision plus the
/// next allocated revision id. Mirrors how the inline-diff path manages
/// per-segment revision ids inside a transaction.
fn stamp_revision(base: &RevisionInfo, rev_counter: &mut u32) -> RevisionInfo {
    let revision_id = *rev_counter;
    *rev_counter += 1;
    RevisionInfo {
        revision_id,
        identity: 0,
        author: base.author.clone(),
        date: base.date.clone(),
        apply_op_id: base.apply_op_id.clone(),
    }
}

/// Apply a `ReplaceHyperlinkText` step. Locates the hyperlink, validates
/// preconditions, and rewrites the hyperlink's runs so that the matched
/// substring is wrapped in a `Deleted` envelope followed by an `Inserted`
/// run carrying the new text. The hyperlink's URL/anchor/r_id are
/// untouched.
#[allow(clippy::too_many_arguments)]
fn apply_replace_hyperlink_text(
    doc: &mut CanonDoc,
    hyperlink_id: &NodeId,
    expect: &str,
    new_text: &str,
    expect_href: Option<&str>,
    expect_anchor: Option<&str>,
    revision: &RevisionInfo,
    step_index: usize,
) -> Result<(), EditError> {
    // 1. Locate the enclosing paragraph.
    let path = find_hyperlink_path(doc, hyperlink_id).ok_or_else(|| {
        // Distinguish "no inline with that id at all" from "inline exists
        // but isn't a hyperlink" — walk once more to check.
        if let Some(actual_kind) = locate_inline_kind(doc, hyperlink_id) {
            EditError::NotAHyperlink {
                hyperlink_id: hyperlink_id.clone(),
                actual_kind,
                step_index,
            }
        } else {
            EditError::HyperlinkNotFound {
                hyperlink_id: hyperlink_id.clone(),
                step_index,
            }
        }
    })?;

    // 2. Validate the enclosing paragraph's block-level tracking status.
    if path.is_top_level() {
        match &doc.blocks[path.top_block].status {
            TrackingStatus::Normal => {}
            TrackingStatus::Inserted(_) => {
                return Err(EditError::BlockHasTrackedStatus {
                    block_id: block_id_of(&doc.blocks[path.top_block].block).clone(),
                    status: "inserted",
                    step_index,
                });
            }
            TrackingStatus::Deleted(_) => {
                return Err(EditError::BlockHasTrackedStatus {
                    block_id: block_id_of(&doc.blocks[path.top_block].block).clone(),
                    status: "deleted",
                    step_index,
                });
            }
            TrackingStatus::InsertedThenDeleted(_) => {
                return Err(EditError::BlockHasTrackedStatus {
                    block_id: block_id_of(&doc.blocks[path.top_block].block).clone(),
                    status: "inserted_then_deleted",
                    step_index,
                });
            }
        }
    } else {
        let para_id = match block_at(doc, &path) {
            BlockNode::Paragraph(p) => p.id.clone(),
            _ => unreachable!("hyperlink path resolves to a paragraph"),
        };
        check_ancestor_table_tracking(doc, &path, &para_id, step_index)?;
    }

    // 3. Validate paragraph segments are all Normal — the MVP rule.
    let para_id = match block_at(doc, &path) {
        BlockNode::Paragraph(p) => p.id.clone(),
        _ => unreachable!("hyperlink path resolves to a paragraph"),
    };
    {
        let para = match block_at(doc, &path) {
            BlockNode::Paragraph(p) => p,
            _ => unreachable!(),
        };
        for segment in &para.segments {
            if segment.status != TrackingStatus::Normal {
                return Err(EditError::ParagraphContainsTrackedSegments {
                    block_id: para_id,
                    step_index,
                });
            }
        }
    }

    // 4. Get a mutable reference to the hyperlink opaque and rewrite its
    //    runs in place.
    let para = match block_at_mut(doc, &path) {
        BlockNode::Paragraph(p) => p,
        _ => unreachable!(),
    };
    let opaque = para
        .segments
        .iter_mut()
        .flat_map(|seg| seg.inlines.iter_mut())
        .find_map(|inline| match inline {
            InlineNode::OpaqueInline(o)
                if &o.id == hyperlink_id && matches!(o.kind, OpaqueKind::Hyperlink(_)) =>
            {
                Some(o)
            }
            _ => None,
        })
        .expect("hyperlink path resolved to a paragraph containing the hyperlink");

    let data = match &mut opaque.kind {
        OpaqueKind::Hyperlink(data) => data,
        _ => unreachable!(),
    };

    // Hyperlink attr-preservation precondition. `replace(hyperlink, ...)`
    // preserves URL and anchor; if the caller supplies a different value
    // they almost certainly meant to use `set_attr`. Fail loudly so a
    // mis-routed attr change does not silently become a no-op on the
    // attr-change side.
    if let Some(expected) = expect_href {
        let actual = data.url.as_deref().unwrap_or("");
        if expected != actual {
            return Err(EditError::HyperlinkAttrMismatch {
                hyperlink_id: hyperlink_id.clone(),
                attr: "href",
                expected: Some(expected.to_string()),
                actual: data.url.clone(),
                step_index,
            });
        }
    }
    if let Some(expected) = expect_anchor {
        let actual = data.anchor.as_deref().unwrap_or("");
        if expected != actual {
            return Err(EditError::HyperlinkAttrMismatch {
                hyperlink_id: hyperlink_id.clone(),
                attr: "anchor",
                expected: Some(expected.to_string()),
                actual: data.anchor.clone(),
                step_index,
            });
        }
    }

    rewrite_hyperlink_runs(data, hyperlink_id, expect, new_text, revision, step_index)?;

    // Invalidate the cached raw_xml so the next serialize rebuilds from
    // the freshly edited `runs`.
    opaque.raw_xml = None;
    opaque.content_hash = None;

    Ok(())
}

/// Apply a `SetHyperlinkAttr` step. Locates the hyperlink, validates
/// `expect_href` / `expect_anchor` preconditions, and mutates `data.url`
/// and/or `data.anchor` in place. No tracked-change envelope is produced
/// (OOXML has no `w:hyperlinkChange`).
#[allow(clippy::too_many_arguments)]
fn apply_set_hyperlink_attr(
    doc: &mut CanonDoc,
    hyperlink_id: &NodeId,
    new_href: Option<&str>,
    new_anchor: Option<&Option<String>>,
    expect_href: Option<&str>,
    expect_anchor: Option<&str>,
    step_index: usize,
) -> Result<(), EditError> {
    // 0. Defensive no-op rejection. The v4 adapter enforces this at the
    //    schema layer (empty AttrPatch), but other callers (tests, future
    //    surfaces) might dispatch directly.
    if new_href.is_none() && new_anchor.is_none() {
        return Err(EditError::HyperlinkSetAttrNoOp {
            hyperlink_id: hyperlink_id.clone(),
            step_index,
        });
    }

    // 1. Locate the enclosing paragraph.
    let path = find_hyperlink_path(doc, hyperlink_id).ok_or_else(|| {
        if let Some(actual_kind) = locate_inline_kind(doc, hyperlink_id) {
            EditError::NotAHyperlink {
                hyperlink_id: hyperlink_id.clone(),
                actual_kind,
                step_index,
            }
        } else {
            EditError::HyperlinkNotFound {
                hyperlink_id: hyperlink_id.clone(),
                step_index,
            }
        }
    })?;

    // 2. Validate the enclosing paragraph's block-level tracking status.
    //    Same rules as `ReplaceHyperlinkText`: we refuse to edit a
    //    hyperlink that sits inside an Inserted/Deleted block, or inside a
    //    cell of a tracked table row. (We do NOT require the hyperlink's
    //    own runs to be Normal — we are mutating the hyperlink's target,
    //    not its runs.)
    if path.is_top_level() {
        match &doc.blocks[path.top_block].status {
            TrackingStatus::Normal => {}
            TrackingStatus::Inserted(_) => {
                return Err(EditError::BlockHasTrackedStatus {
                    block_id: block_id_of(&doc.blocks[path.top_block].block).clone(),
                    status: "inserted",
                    step_index,
                });
            }
            TrackingStatus::Deleted(_) => {
                return Err(EditError::BlockHasTrackedStatus {
                    block_id: block_id_of(&doc.blocks[path.top_block].block).clone(),
                    status: "deleted",
                    step_index,
                });
            }
            TrackingStatus::InsertedThenDeleted(_) => {
                return Err(EditError::BlockHasTrackedStatus {
                    block_id: block_id_of(&doc.blocks[path.top_block].block).clone(),
                    status: "inserted_then_deleted",
                    step_index,
                });
            }
        }
    } else {
        let para_id = match block_at(doc, &path) {
            BlockNode::Paragraph(p) => p.id.clone(),
            _ => unreachable!("hyperlink path resolves to a paragraph"),
        };
        check_ancestor_table_tracking(doc, &path, &para_id, step_index)?;
    }

    // 3. Get a mutable reference to the hyperlink opaque.
    let para = match block_at_mut(doc, &path) {
        BlockNode::Paragraph(p) => p,
        _ => unreachable!(),
    };
    let opaque = para
        .segments
        .iter_mut()
        .flat_map(|seg| seg.inlines.iter_mut())
        .find_map(|inline| match inline {
            InlineNode::OpaqueInline(o)
                if &o.id == hyperlink_id && matches!(o.kind, OpaqueKind::Hyperlink(_)) =>
            {
                Some(o)
            }
            _ => None,
        })
        .expect("hyperlink path resolved to a paragraph containing the hyperlink");

    let data = match &mut opaque.kind {
        OpaqueKind::Hyperlink(data) => data,
        _ => unreachable!(),
    };

    // 4. Optimistic-concurrency preconditions. Compare against the
    //    canonical Option<String> shape (not the empty-string surrogate),
    //    because `None` and `Some("")` mean different things at the OOXML
    //    layer (no `Target` rels entry vs. an explicit empty target).
    if let Some(expected) = expect_href {
        let actual = data.url.as_deref().unwrap_or("");
        if expected != actual {
            return Err(EditError::HyperlinkAttrMismatch {
                hyperlink_id: hyperlink_id.clone(),
                attr: "href",
                expected: Some(expected.to_string()),
                actual: data.url.clone(),
                step_index,
            });
        }
    }
    if let Some(expected) = expect_anchor {
        let actual = data.anchor.as_deref().unwrap_or("");
        if expected != actual {
            return Err(EditError::HyperlinkAttrMismatch {
                hyperlink_id: hyperlink_id.clone(),
                attr: "anchor",
                expected: Some(expected.to_string()),
                actual: data.anchor.clone(),
                step_index,
            });
        }
    }

    // 5. Mutate. `r_id` is intentionally left alone: the serializer
    //    re-resolves it from `url` via the rel resolver at export time
    //    (see `src/serialize.rs` near the OpaqueKind::Hyperlink arm).
    //    Setting it here would race the resolver and create an
    //    inconsistency between `data.url` and the rels file.
    if let Some(new_url) = new_href {
        data.url = Some(new_url.to_string());
    }
    if let Some(new_anchor_value) = new_anchor {
        data.anchor = new_anchor_value.clone();
    }

    // 6. Invalidate cached XML. Mirrors `apply_replace_hyperlink_text`:
    //    the next serialize must rebuild the hyperlink element from
    //    `data`, not from the stale captured XML.
    opaque.raw_xml = None;
    opaque.content_hash = None;

    Ok(())
}

/// Return a kind label if any inline anywhere in `doc` matches the given
/// id (regardless of kind). Used to distinguish `NotAHyperlink` from
/// `HyperlinkNotFound` in error reporting.
fn locate_inline_kind(doc: &CanonDoc, inline_id: &NodeId) -> Option<&'static str> {
    fn scan(blocks: &[BlockNode], inline_id: &NodeId) -> Option<&'static str> {
        for block in blocks {
            match block {
                BlockNode::Paragraph(p) => {
                    for seg in &p.segments {
                        for inline in &seg.inlines {
                            match inline {
                                InlineNode::Text(t) if &t.id == inline_id => return Some("text"),
                                InlineNode::OpaqueInline(o) if &o.id == inline_id => {
                                    return Some(opaque_kind_label(&o.kind));
                                }
                                InlineNode::HardBreak(hb) if &hb.id == inline_id => {
                                    return Some("hard_break");
                                }
                                InlineNode::Decoration(d) if &d.id == inline_id => {
                                    return Some("decoration");
                                }
                                _ => {}
                            }
                        }
                    }
                }
                BlockNode::Table(t) => {
                    for row in &t.rows {
                        for cell in &row.cells {
                            if let Some(k) = scan(&cell.blocks, inline_id) {
                                return Some(k);
                            }
                        }
                    }
                }
                BlockNode::OpaqueBlock(_) => {}
            }
        }
        None
    }
    let top: Vec<BlockNode> = doc.blocks.iter().map(|tb| tb.block.clone()).collect();
    scan(&top, inline_id)
}

/// Rewrite a hyperlink's runs so that the matched substring is wrapped in
/// `Deleted` runs followed by an `Inserted` run carrying `new_text`. Runs
/// straddling a match boundary are split at the byte offset (always a UTF-8
/// char boundary because `expect` is a contiguous substring of the
/// concatenated text).
///
/// Preconditions:
/// - All existing runs must be `Normal` (`HyperlinkContainsTrackedChanges`
///   otherwise).
/// - The concatenated text must contain `expect` (`ExpectMismatch` otherwise).
fn rewrite_hyperlink_runs(
    data: &mut crate::domain::HyperlinkData,
    hyperlink_id: &NodeId,
    expect: &str,
    new_text: &str,
    revision: &RevisionInfo,
    step_index: usize,
) -> Result<(), EditError> {
    use crate::domain::HyperlinkRun;

    if data.runs.iter().any(|r| r.status != TrackingStatus::Normal) {
        return Err(EditError::HyperlinkContainsTrackedChanges {
            hyperlink_id: hyperlink_id.clone(),
            step_index,
        });
    }

    // Empty `expect` is meaningless — we can't anchor a rewrite at a
    // zero-length string. Reject loudly rather than silently no-op'ing
    // or inserting at position 0. The Python adapter rejects empty
    // expect at the boundary; this is the engine's matching check for
    // direct callers (the Rust JSON adapter doesn't enforce minLength).
    if expect.is_empty() {
        return Err(EditError::ExpectMismatch {
            block_id: hyperlink_id.clone(),
            expected: expect.to_string(),
            actual_text: data.runs.iter().map(|r| r.text.as_str()).collect(),
            step_index,
        });
    }

    let full_text: String = data.runs.iter().map(|r| r.text.as_str()).collect();
    let match_start = full_text
        .find(expect)
        .ok_or_else(|| EditError::ExpectMismatch {
            block_id: hyperlink_id.clone(),
            expected: expect.to_string(),
            actual_text: full_text.clone(),
            step_index,
        })?;
    let match_end = match_start + expect.len();

    let mut new_runs: Vec<HyperlinkRun> = Vec::new();
    let mut cursor: usize = 0;
    let mut nearest_rpr: Option<Vec<u8>> = None;
    let mut inserted_emitted = false;

    for run in &data.runs {
        let run_start = cursor;
        let run_end = cursor + run.text.len();

        // Region 1: prefix portion (Normal).
        if run_start < match_start {
            let prefix_end = run_end.min(match_start);
            let slice = &run.text[..(prefix_end - run_start)];
            if !slice.is_empty() {
                new_runs.push(HyperlinkRun {
                    text: slice.to_string(),
                    rpr_xml: run.rpr_xml.clone(),
                    source_run_attrs: run.source_run_attrs.clone(),
                    status: TrackingStatus::Normal,
                });
                nearest_rpr = run.rpr_xml.clone();
            }
        }

        // Region 2: matched portion (Deleted).
        let match_lo = run_start.max(match_start);
        let match_hi = run_end.min(match_end);
        if match_lo < match_hi {
            let lo = match_lo - run_start;
            let hi = match_hi - run_start;
            let slice = &run.text[lo..hi];
            if !slice.is_empty() {
                new_runs.push(HyperlinkRun {
                    text: slice.to_string(),
                    rpr_xml: run.rpr_xml.clone(),
                    source_run_attrs: run.source_run_attrs.clone(),
                    status: TrackingStatus::Deleted(revision.clone()),
                });
                nearest_rpr = run.rpr_xml.clone();
            }
        }

        // Emit the inserted run exactly once, immediately after the
        // matched portion ends in this run (or after the last matched run
        // if the match boundary aligned with the run end).
        if !inserted_emitted && run_end >= match_end && match_start < match_end {
            if !new_text.is_empty() {
                new_runs.push(HyperlinkRun {
                    text: new_text.to_string(),
                    rpr_xml: nearest_rpr.clone(),
                    source_run_attrs: Vec::new(),
                    status: TrackingStatus::Inserted(revision.clone()),
                });
            }
            inserted_emitted = true;
        }

        // Region 3: suffix portion (Normal).
        if run_end > match_end {
            let lo = run_start.max(match_end) - run_start;
            let slice = &run.text[lo..];
            if !slice.is_empty() {
                new_runs.push(HyperlinkRun {
                    text: slice.to_string(),
                    rpr_xml: run.rpr_xml.clone(),
                    source_run_attrs: run.source_run_attrs.clone(),
                    status: TrackingStatus::Normal,
                });
            }
        }

        cursor = run_end;
    }

    data.runs = new_runs;
    // Keep `text` in sync with the visible (non-deleted) run text so it
    // still reflects the post-accept display string.
    data.text = data
        .runs
        .iter()
        .filter(|r| !matches!(r.status, TrackingStatus::Deleted(_)))
        .map(|r| r.text.as_str())
        .collect();

    Ok(())
}

// ─── Unit tests ────────────────────────────────────────────────────────────
//
// These cover `apply_transaction`'s error arms as no-silent-fallback
// contracts (a failed precondition must produce a TYPED error, never a
// best-effort partial apply) and `normalize_segments`'s structural invariants
// (adjacent same-status merge, empty-segment drop). Pure in-memory CanonDoc —
// no DOCX bytes, no runtime, no corpus.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        DocFingerprint, DocMeta, DocPart, FieldData, FieldKind, INTERNAL_IDS_VERSION_V0,
        OpaqueInlineNode, ProofRef, SCHEMA_VERSION_V0, TextNode, normal_tracked_block,
    };

    fn meta() -> DocMeta {
        DocMeta {
            schema_version: SCHEMA_VERSION_V0.to_string(),
            docx_fingerprint: DocFingerprint("fp".to_string()),
            internal_ids_version: INTERNAL_IDS_VERSION_V0.to_string(),
        }
    }

    fn empty_doc(blocks: Vec<TrackedBlock>) -> CanonDoc {
        CanonDoc {
            id: NodeId::from("doc"),
            blocks,
            meta: meta(),
            headers: Vec::new(),
            footers: Vec::new(),
            footnotes: Vec::new(),
            endnotes: Vec::new(),
            comments: Vec::new(),
            comments_extended: Vec::new(),
            body_section_properties: None,
            body_section_property_change: None,
            compat_settings: crate::domain::CompatSettings::default(),
            even_and_odd_headers: None,
            document_background: None,
            document_protection: None,
        }
    }

    fn text_inline(id: &str, text: &str) -> InlineNode {
        InlineNode::from(TextNode {
            id: NodeId::from(id),
            text_role: None,
            text: text.to_string(),
            marks: Vec::new(),
            style_props: StyleProps::default(),
            rpr_authored: RunRprAuthored::default(),
            source_run_attrs: Vec::new(),
            formatting_change: None,
        })
    }

    fn field_inline(id: &str) -> InlineNode {
        InlineNode::from(OpaqueInlineNode {
            id: NodeId::from(id),
            kind: OpaqueKind::Field(FieldData {
                field_kind: FieldKind::Simple,
                instruction_text: Some(" DATE ".to_string()),
                result_text: Some("2025".to_string()),
                semantic: None,
            }),
            opaque_ref: id.to_string(),
            proof_ref: ProofRef {
                part: DocPart::DocumentXml,
                block_id: NodeId::from(id),
                docx_anchor: id.to_string(),
            },
            wrapper_marks: Vec::new(),
            wrapper_style_props: StyleProps::default(),
            raw_xml: None,
            content_hash: None,
        })
    }

    fn paragraph_block(id: &str, segments: Vec<TrackedSegment>) -> ParagraphNode {
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
            segments,
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
        }
    }

    fn revision() -> RevisionInfo {
        RevisionInfo {
            revision_id: 7,
            identity: 0,
            author: Some("unit-test".to_string()),
            date: Some("2026-06-03T00:00:00Z".to_string()),
            apply_op_id: None,
        }
    }

    fn replace_tx(block_id: &str, expect: &str, new_text: &str) -> EditTransaction {
        EditTransaction {
            steps: vec![EditStep::ReplaceParagraphText {
                block_id: NodeId::from(block_id),
                rationale: None,
                replacement_role: None,
                expect: expect.to_string(),
                semantic_hash: None,
                content: ParagraphContent {
                    fragments: vec![ContentFragment::Text(new_text.to_string())],
                },
            }],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: revision(),
        }
    }

    // ── apply_transaction error arms ─────────────────────────────────────────

    /// A replace targeting a non-existent block must fail BlockNotFound with
    /// the offending id and step index — never silently skip the step.
    #[test]
    fn apply_replace_unknown_block_is_block_not_found() {
        let doc = empty_doc(vec![normal_tracked_block(BlockNode::from(
            paragraph_block("p1", normal_segment(vec![text_inline("t1", "Hello world")])),
        ))]);
        let tx = replace_tx("does-not-exist", "Hello", "Goodbye");
        let err = apply_transaction(&doc, &tx).expect_err("missing block must fail");
        match err {
            EditError::BlockNotFound {
                block_id,
                step_index,
            } => {
                assert_eq!(block_id, NodeId::from("does-not-exist"));
                assert_eq!(step_index, 0);
            }
            other => panic!("expected BlockNotFound, got {other:?}"),
        }
    }

    /// `expect` is a precondition: if the substring is not present the edit is
    /// stale and must fail ExpectMismatch carrying both expected and actual —
    /// the engine must not apply against text it cannot locate.
    #[test]
    fn apply_replace_expect_not_found_is_expect_mismatch() {
        let doc = empty_doc(vec![normal_tracked_block(BlockNode::from(
            paragraph_block("p1", normal_segment(vec![text_inline("t1", "Hello world")])),
        ))]);
        let tx = replace_tx("p1", "Nonexistent phrase", "Whatever");
        let err = apply_transaction(&doc, &tx).expect_err("absent expect must fail");
        match err {
            EditError::ExpectMismatch {
                block_id,
                expected,
                actual_text,
                step_index,
            } => {
                assert_eq!(block_id, NodeId::from("p1"));
                assert_eq!(expected, "Nonexistent phrase");
                assert!(
                    actual_text.contains("Hello world"),
                    "actual_text must surface what was really there: {actual_text:?}"
                );
                assert_eq!(step_index, 0);
            }
            other => panic!("expected ExpectMismatch, got {other:?}"),
        }
    }

    /// Replacing the text around an opaque inline without preserving it must
    /// fail OpaqueDestroyed, listing the destroyed anchor id. Opaque content
    /// is never silently dropped.
    #[test]
    fn apply_replace_dropping_opaque_is_opaque_destroyed() {
        let segments = normal_segment(vec![
            text_inline("t1", "Dated "),
            field_inline("f1"),
            text_inline("t2", " today"),
        ]);
        let doc = empty_doc(vec![normal_tracked_block(BlockNode::from(
            paragraph_block("p1", segments),
        ))]);
        // Replacement is plain text that omits the field anchor entirely.
        let tx = replace_tx("p1", "Dated", "Completely rewritten with no field");
        let err = apply_transaction(&doc, &tx).expect_err("dropping the field must fail");
        match err {
            EditError::OpaqueDestroyed {
                target_block_id,
                missing_opaque_ids,
                step_index,
                ..
            } => {
                assert_eq!(target_block_id, NodeId::from("p1"));
                assert!(
                    missing_opaque_ids.contains(&"f1".to_string()),
                    "the destroyed field id must be reported: {missing_opaque_ids:?}"
                );
                assert_eq!(step_index, 0);
            }
            other => panic!("expected OpaqueDestroyed, got {other:?}"),
        }
    }

    /// A paragraph block already marked for DELETION cannot be edited: the
    /// document says this content is going away, so a rewrite of it is
    /// incoherent and must fail BlockHasTrackedStatus("deleted").
    ///
    /// (Domain note: a block marked INSERTED is the opposite case — it is
    /// in-flight new content the author may still rewrite, so the replace path
    /// auto-accepts it rather than refusing. Only the deleted side is a hard
    /// stop. See `project_tracked_block_for_direct_edit`.)
    #[test]
    fn apply_replace_on_deleted_block_is_block_has_tracked_status() {
        let para = paragraph_block("p1", normal_segment(vec![text_inline("t1", "Hello world")]));
        let tracked_block = TrackedBlock {
            status: TrackingStatus::Deleted(revision()),
            block: BlockNode::from(para),
            move_id: None,
            block_sdt_wrap: None,
        };
        let doc = empty_doc(vec![tracked_block]);
        let tx = replace_tx("p1", "Hello", "Goodbye");
        let err = apply_transaction(&doc, &tx).expect_err("deleted block must be refused");
        match err {
            EditError::BlockHasTrackedStatus {
                block_id,
                status,
                step_index,
            } => {
                assert_eq!(block_id, NodeId::from("p1"));
                assert_eq!(status, "deleted");
                assert_eq!(step_index, 0);
            }
            other => panic!("expected BlockHasTrackedStatus, got {other:?}"),
        }
    }

    // ── normalize_segments edge cases ────────────────────────────────────────

    /// Adjacent segments with identical status are merged into one (the
    /// canonical-form invariant the reconstruction relies on).
    #[test]
    fn normalize_merges_adjacent_same_status_segments() {
        let mut segments = vec![
            TrackedSegment {
                status: TrackingStatus::Normal,
                inlines: vec![text_inline("a", "Hello ")],
            },
            TrackedSegment {
                status: TrackingStatus::Normal,
                inlines: vec![text_inline("b", "world")],
            },
        ];
        normalize_segments(&mut segments);
        assert_eq!(segments.len(), 1, "adjacent Normal segments must merge");
        let texts: Vec<&str> = segments[0]
            .inlines
            .iter()
            .map(|inline| match inline {
                InlineNode::Text(text) => text.text.as_str(),
                other => panic!("expected text node, got {other:?}"),
            })
            .collect();
        assert_eq!(
            texts,
            vec!["Hello ", "world"],
            "segment normalization must preserve source run boundaries"
        );
    }

    /// Two adjacent Normal text nodes that are identical in every OTHER
    /// respect but carry different preserved-rPr remainders (one run kept an
    /// unmodeled `w:eastAsianLayout`, its neighbor didn't) are format-distinct
    /// and must remain separate — merging would silently drop
    /// whichever run's preserved content lost the coin flip.
    #[test]
    fn normalize_does_not_merge_text_nodes_with_differing_preserved_props() {
        let preserved_a = crate::domain::PreservedProp {
            name: "w:eastAsianLayout".to_string(),
            raw_xml: r#"<w:eastAsianLayout w:combine="1"/>"#.to_string(),
        };
        let mut a = match text_inline("a", "same text") {
            InlineNode::Text(t) => *t,
            _ => unreachable!(),
        };
        a.style_props.preserved = vec![preserved_a];
        let b = match text_inline("b", "same text") {
            InlineNode::Text(t) => *t,
            _ => unreachable!(),
        };
        // `b` has no preserved props — style_props otherwise identical.
        assert_ne!(a.style_props, b.style_props);

        let mut segments = vec![TrackedSegment {
            status: TrackingStatus::Normal,
            inlines: vec![InlineNode::from(a), InlineNode::from(b)],
        }];
        normalize_segments(&mut segments);

        assert_eq!(
            segments[0].inlines.len(),
            2,
            "runs with differing preserved rPr remainders must remain separate"
        );
    }

    /// Empty segments (no inlines) are dropped — they carry no content and
    /// would otherwise be phantom tracked spans (invariant I7).
    #[test]
    fn normalize_drops_empty_segments() {
        let mut segments = vec![
            TrackedSegment {
                status: TrackingStatus::Normal,
                inlines: vec![text_inline("a", "kept")],
            },
            TrackedSegment {
                status: TrackingStatus::Inserted(revision()),
                inlines: vec![],
            },
        ];
        normalize_segments(&mut segments);
        assert_eq!(
            segments.len(),
            1,
            "the empty Inserted segment must be dropped"
        );
        assert!(matches!(segments[0].status, TrackingStatus::Normal));
    }

    /// Distinct statuses must NOT be merged: a delete followed by an insert is
    /// two separate tracked spans, and normalization preserves that boundary.
    #[test]
    fn normalize_keeps_distinct_status_segments_separate() {
        let mut segments = vec![
            TrackedSegment {
                status: TrackingStatus::Deleted(revision()),
                inlines: vec![text_inline("a", "old")],
            },
            TrackedSegment {
                status: TrackingStatus::Inserted(revision()),
                inlines: vec![text_inline("b", "new")],
            },
        ];
        normalize_segments(&mut segments);
        assert_eq!(segments.len(), 2, "delete and insert must stay distinct");
        assert!(matches!(segments[0].status, TrackingStatus::Deleted(_)));
        assert!(matches!(segments[1].status, TrackingStatus::Inserted(_)));
    }
}
