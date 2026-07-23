//! `docs/reference/read-model.md` is GENERATED from live engine values. This
//! test is both the generator and the drift guard:
//!
//! - `read_model_reference_is_current` (runs in the gate) renders the page
//!   from a live exemplar document and fails if the checked-in file differs,
//!   naming the fix.
//! - `regenerate_read_model_reference` (ignored) writes the rendered page;
//!   run it via `just regen-read-model-reference`.
//!
//! Drift guarding, per shape class:
//! - documented STRUCT field tables are asserted against the key set of a
//!   value the engine actually serialized (a struct field added, removed, or
//!   renamed fails this test until the table and page are regenerated);
//! - documented ENUM vocabularies are pinned by wildcard-free `match` arms
//!   (a new variant fails compilation in this file first);
//! - the facade method list is pinned by function-pointer casts (a signature
//!   change fails compilation in this file first);
//! - every embedded example is `serde_json::to_string_pretty` of a value the
//!   engine produced for the exemplar document, never hand-written JSON.
//!
//! The rendered Markdown must satisfy `scripts/check-docs.py` (no em/en
//! dashes, no spaced dash punctuation, resolvable links); the style assertions
//! here mirror those rules so an edit that would redden the docs gate fails
//! loudly in this crate first.

use std::fmt::Write as _;
use std::path::PathBuf;

use serde_json::Value;
use stemma::api::{
    BlockView, Document, DocumentOutline, DocumentView, SegmentView, TrackStatus, WindowError,
    WindowFormat,
};
use stemma::domain::{
    Alignment, BlockType, ChangeType, ImageMetadataChange, InlineChange, InlineChangeSegmentType,
    LineSpacingRule, Mark, MarkValue, MoveDirection, OpaqueSegmentKind, StructuralChange,
    TrackingStatus,
};
use stemma::edit_v4::parse_transaction;
use stemma::runtime::build_tracked_document_view_from_snapshot;
use stemma::tracked_model::{RevisionKind, RevisionRecord};

fn doc_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../docs/reference/read-model.md")
}

// ─── The exemplar document ──────────────────────────────────────────────────
//
// A minimal synthesized DOCX (corpus-free) carrying one hyperlink, one table,
// one tracked replacement, and one tracked format change, so every documented
// shape is observed on a value the engine actually produced.

fn make_docx() -> Vec<u8> {
    let body = concat!(
        r#"<w:p><w:r><w:t xml:space="preserve">See the standard terms for definitions.</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t xml:space="preserve">Liability is limited to direct damages.</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t xml:space="preserve">Fee schedule placeholder.</w:t></w:r></w:p>"#,
        r#"<w:p><w:pPr><w:tabs><w:tab w:val="center" w:pos="4536"/></w:tabs></w:pPr><w:r><w:t xml:space="preserve">Contact the Supplier for details.</w:t></w:r></w:p>"#,
        r#"<w:sectPr><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="708" w:footer="708" w:gutter="0"/></w:sectPr>"#,
    );
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}</w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"></Relationships>"#;

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

fn apply_v4(doc: &Document, json: &str) -> Document {
    let txn = parse_transaction(json)
        .unwrap_or_else(|e| panic!("exemplar transaction failed v4 parse: {e}"))
        .into_edit_transaction()
        .unwrap_or_else(|e| panic!("exemplar transaction failed v4 lowering: {e}"));
    doc.apply(&txn)
        .unwrap_or_else(|e| panic!("exemplar transaction failed to apply: {e:?}"))
}

fn find_block<'a>(view: &'a DocumentView, needle: &str) -> &'a BlockView {
    view.blocks
        .iter()
        .find(|b| b.text.contains(needle))
        .unwrap_or_else(|| panic!("exemplar has no block containing {needle:?}"))
}

/// Parse, scaffold in direct mode (hyperlink + table), then author tracked
/// edits (a redlined replacement and a run-format change) through the same v4
/// wire path every transport uses.
fn exemplar() -> Document {
    let doc = Document::parse(&make_docx()).expect("parse exemplar docx");
    let v = doc.read();
    let p1 = find_block(&v, "standard terms").id.to_string();
    let p3 = find_block(&v, "Fee schedule").id.to_string();

    let setup = format!(
        r#"{{"ops":[
            {{"op":"replace","target":"{p1}","expect":"See the standard terms for definitions.","content":{{"type":"paragraph","content":[
                {{"type":"text","text":"See "}},
                {{"type":"hyperlink","attrs":{{"href":"https://example.com/terms"}},"content":[{{"type":"text","text":"the standard terms"}}]}},
                {{"type":"text","text":" for definitions."}}]}}}},
            {{"op":"insert","target":{{"anchor":"{p3}","position":"after"}},"content":[
                {{"type":"table","content":[{{"content":[
                    {{"content":[{{"type":"paragraph","role":"body_text","content":[{{"type":"text","text":"Fee"}}]}}]}},
                    {{"content":[{{"type":"paragraph","role":"body_text","content":[{{"type":"text","text":"EUR 40"}}]}}]}}]}}]}}]}},
            {{"op":"delete","target":"{p3}","expect":"Fee schedule placeholder."}}
        ],"revision":{{"author":"Setup","date":"2026-07-01T09:00:00Z"}},
        "materialization_mode":"direct","summary":"Exemplar scaffolding"}}"#
    );
    let doc = apply_v4(&doc, &setup);

    let v = doc.read();
    let p2 = find_block(&v, "Liability").id.to_string();
    let p4 = find_block(&v, "Contact").id.to_string();
    let tracked = format!(
        r#"{{"ops":[
            {{"op":"replace","target":"{p2}","expect":"Liability is limited to direct damages.","content":{{"type":"paragraph","content":[
                {{"type":"text","text":"Liability is capped at "}},
                {{"type":"text","text":"twice the fees paid","marks":[{{"type":"bold"}}]}},
                {{"type":"text","text":"."}}]}}}},
            {{"op":"set_format","target":"{p4}","expect":"the Supplier","marks":[{{"type":"italic"}}]}}
        ],"revision":{{"author":"J. Osei","date":"2026-07-06T10:30:00Z"}},
        "summary":"Tracked exemplar edits"}}"#
    );
    apply_v4(&doc, &tracked)
}

// ─── Rendering helpers ──────────────────────────────────────────────────────

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).expect("Value renders")
}

/// Assert that a documented field table matches the observed key set of a
/// serialized value. `conditional` names fields serde omits when absent
/// (`skip_serializing_if`): they must be documented but may be unobserved.
fn assert_fields(what: &str, observed: &Value, rows: &[(&str, &str)], conditional: &[&str]) {
    let obj = observed
        .as_object()
        .unwrap_or_else(|| panic!("{what}: expected a JSON object, got {observed}"));
    let documented: Vec<&str> = rows.iter().map(|(k, _)| *k).collect();
    for key in obj.keys() {
        assert!(
            documented.contains(&key.as_str()),
            "{what}: engine serializes field `{key}` but the reference page does not document it; \
             add a row and regenerate"
        );
    }
    for key in &documented {
        assert!(
            obj.contains_key(*key) || conditional.contains(key),
            "{what}: the reference page documents field `{key}` but the engine no longer \
             serializes it; drop the row and regenerate"
        );
    }
}

fn field_table(rows: &[(&str, &str)]) -> String {
    let mut out = String::from("| Field | Meaning |\n|---|---|\n");
    for (name, meaning) in rows {
        let _ = writeln!(out, "| `{name}` | {meaning} |");
    }
    out
}

// ─── Documented field tables ────────────────────────────────────────────────
//
// One row per serialized field. `assert_fields` keeps every table exactly in
// step with what the engine emits.

const BLOCK_VIEW_FIELDS: &[(&str, &str)] = &[
    (
        "id",
        "Stable block id, the handle an edit operation targets.",
    ),
    (
        "role",
        r#""Paragraph", {"Heading":{"level":n}}, "Table", or "Opaque"."#,
    ),
    (
        "style_id",
        "Paragraph or table style id, when the document carries one; else null.",
    ),
    (
        "role_token",
        "The role token `insert` accepts to author a NEW paragraph formatted like this block; null for tables and opaque blocks.",
    ),
    (
        "text",
        "Concatenated visible text: the CURRENT redline reading, pending insertions and not-yet-accepted deletions both present (project the document for the accept-all or reject-all reading). What a plain `expect` matches against.",
    ),
    (
        "block_status",
        "Whole-block tracked status (a block-level insert or delete), a [track status](#track-status).",
    ),
    (
        "paragraph_mark_status",
        "Tracked status of the trailing paragraph mark, a [track status](#track-status).",
    ),
    (
        "guard",
        "The staleness guard: the block's semantic hash at read time. A write op carries it back, and a block that changed since the read fails loud with `StaleEdit`.",
    ),
    (
        "list",
        "Word auto-numbering membership (`num_id`, `ilvl`, `ordered`, `marker_text`); null for non-list paragraphs and for literal-prefix lists.",
    ),
    (
        "cells",
        "Per-cell grid addressing for a table block, row-major (see [tables](#tables-in-the-lean-view)); empty for non-table blocks.",
    ),
    (
        "table",
        "Table-level metadata: `cols` (grid column widths, twips), `align`, `indent` (twips); null for non-table blocks.",
    ),
    (
        "literal_prefix",
        r#"The typed-in enumeration label ("1.", "(a)") already prepended to `text`; surfaced separately because it is a structural marker, not a span an edit can address."#,
    ),
    (
        "segments",
        "Inline structure for fine-grained targeting; see below.",
    ),
    (
        "opaque_label",
        "For an opaque block, the honest description of what kind of placeholder it is; null otherwise.",
    ),
];

const SEGMENT_TEXT_FIELDS: &[(&str, &str)] = &[
    ("text", "The run's visible text."),
    (
        "status",
        "The span's [track status](#track-status). A run breaks where status or marks change.",
    ),
    (
        "marks",
        r#"Meaningful inline marks: "Bold", "Italic", "Underline", "Strike", "Subscript", "Superscript". Value-carrying formatting (fonts, sizes, colors) lives in the full render view, not here."#,
    ),
    (
        "handle",
        "Ephemeral block-local span handle (`s_0`, `s_1`, ...), valid only while the block `guard` is unchanged.",
    ),
];

const SEGMENT_OPAQUE_FIELDS: &[(&str, &str)] = &[
    (
        "id",
        "The anchor's durable id, the preferred selector for anchor-relative operations.",
    ),
    (
        "kind",
        r#"Anchor kind from a small public vocabulary: "Drawing", "Equation", "Hyperlink", "Field", "FootnoteRef", "EndnoteRef", "Comment", "ContentControl", "HardBreak", "CommentRangeStart", "CommentRangeEnd", and future kinds (the vocabulary is non-exhaustive by design)."#,
    ),
    ("status", "The anchor's [track status](#track-status)."),
    (
        "text",
        "Visible label when one is known (field result, hyperlink display text); else null.",
    ),
    (
        "handle",
        "Span handle ordinal, shared with text spans so the sequence is dense.",
    ),
    (
        "metadata",
        "Kind-specific structure (a content control's tag and value, a drawing's EMU extent and alt text, ...), omitted when the kind carries nothing discoverable.",
    ),
];

const REVISION_VIEW_FIELDS: &[(&str, &str)] = &[
    (
        "revision_id",
        "Stable engine-minted identity, unique within the document; the value selective accept and reject address. NOT the wire `w:id`, which Word does not keep unique.",
    ),
    (
        "author",
        "Author of the change, when the source carried one; the view never invents one.",
    ),
    ("date", "ISO-8601 timestamp, when the source carried one."),
    (
        "apply_op_id",
        "Group id of the `apply` call that created this revision; null for changes loaded from an imported DOCX.",
    ),
];

const LIST_MEMBERSHIP_FIELDS: &[(&str, &str)] = &[
    (
        "num_id",
        "The numbering instance (`w:numId`); paragraphs sharing it are in the same list.",
    ),
    (
        "ilvl",
        "The indent level within the list (`w:ilvl`, 0-based).",
    ),
    (
        "ordered",
        "True for an ordered (counter) list, false for a bullet list.",
    ),
    (
        "marker_text",
        r#"The synthesized marker ("1.", "(a)", a bullet); empty when not materialized."#,
    ),
];

const TABLE_CELL_FIELDS: &[(&str, &str)] = &[
    ("row", "0-based row index in the table grid."),
    (
        "col",
        "0-based logical grid column (after `gridBefore`; a merged cell occupies its first column).",
    ),
    (
        "text",
        "Concatenated visible text of every paragraph in the cell; the address `table_op.set_cell_text` takes.",
    ),
    (
        "col_span",
        "Horizontal span (`gridSpan`); 1 means no merge.",
    ),
    (
        "row_span",
        "Vertical span (resolved `vMerge`); continuation cells fold into the anchor and are not emitted.",
    ),
    (
        "borders",
        "The cell's four EFFECTIVE borders (cell override, else table outer or inside), each a [border](#border) or null.",
    ),
    (
        "shading",
        "Background fill from cell shading (`w:shd`) as a hex color; null when none.",
    ),
    (
        "v_align",
        r#""top", "center", or "bottom"; null means the default (top)."#,
    ),
    (
        "paragraphs",
        "The cell's content as render-ready paragraphs, each carrying the SAME segment shape the full render view uses (below), so one segment renderer covers body text and cells.",
    ),
];

const CELL_PARAGRAPH_FIELDS: &[(&str, &str)] = &[
    (
        "segments",
        "The paragraph's runs in the full render view's segment shape (marks, `style_props`, hyperlinks, per-run tracked status).",
    ),
    (
        "block_id",
        "The cell paragraph's own id; a `replace` or `set_format` can target it in place, like a body paragraph.",
    ),
    (
        "guard",
        "The cell paragraph's own staleness guard, same mechanism as body blocks.",
    ),
];

const TABLE_META_FIELDS: &[(&str, &str)] = &[
    ("cols", "Grid column widths in twips, from `w:tblGrid`."),
    (
        "align",
        r#"Table alignment ("left", "center", "right"); null means left."#,
    ),
    (
        "indent",
        "Table indent from the leading margin, in twips; null when unset.",
    ),
];

const FULL_DOC_BLOCK_FIELDS: &[(&str, &str)] = &[
    (
        "block_id",
        "Stable projection block identity. For blocks present in the target reading this is the canonical block id; for deleted-only blocks it is a stable tombstone id.",
    ),
    (
        "doc1_block_id",
        "Base-side block id; null for inserted blocks. In the single-document view, base and target are the same document.",
    ),
    (
        "doc2_block_id",
        "Target-side block id; null for deleted blocks.",
    ),
    (
        "block_type",
        r#""Paragraph", "Heading", "Table", or "Opaque"."#,
    ),
    (
        "heading_level",
        "Heading outline level, when the block is a heading.",
    ),
    ("style_id", "Paragraph or table style id, when carried."),
    (
        "change_type",
        r#""Unchanged", "Modified", "Inserted", or "Deleted"."#,
    ),
    (
        "align",
        "Paragraph alignment, an [alignment value](#enumerated-vocabularies) or null.",
    ),
    (
        "indent",
        "Render-resolved [indentation](#indentation): `effective_first_line_twips` already folds in a literal-prefix marker's leading-tab landing, so it is the single first-line origin to apply.",
    ),
    (
        "spacing",
        "Paragraph [spacing](#paragraph-spacing), or null.",
    ),
    (
        "borders",
        "Paragraph [borders](#paragraph-borders), or null.",
    ),
    (
        "tab_stops",
        "Effective tab stops (`position` twips, `alignment`, `leader`); empty means no custom stops.",
    ),
    (
        "numbering_text",
        r#"The synthesized auto-number label ("1.", "(a)"), when auto-numbered."#,
    ),
    ("numbering_ilvl", "The numbering level, when auto-numbered."),
    (
        "numbering_num_id",
        "The numbering instance id (Word auto-numbering only; null for a literal-prefix list). Lets a consumer join a paragraph to an existing list.",
    ),
    (
        "segments",
        "The block's inline runs as [segments](#segments-in-the-full-render-view).",
    ),
    (
        "table_diff",
        "Structural table diff, present only when table structure changed.",
    ),
    (
        "content_types",
        r#"Content present in this block, e.g. ["text"], ["image"], ["text","image"]."#,
    ),
    (
        "equation_xmls",
        "Raw OMML XML strings for equations in this block.",
    ),
    (
        "equation_doc1_count",
        "How many leading entries of `equation_xmls` are base-side.",
    ),
    (
        "image_data_uris",
        r#"Base64 data URIs for images in this block ("data:image/png;base64,...")."#,
    ),
    (
        "image_doc1_count",
        "How many leading entries of `image_data_uris` are base-side.",
    ),
    (
        "image_metadata_changes",
        r#"Image metadata that changed while pixels stayed identical: "Size", "Cropping", "AltText"."#,
    ),
    (
        "move_id",
        r#"Shared move identifier linking a "moved from" block to its "moved to" counterpart."#,
    ),
    (
        "move_direction",
        r#""From" (content left here) or "To" (content arrived here); else null."#,
    ),
    (
        "structural_change",
        r#"Join or split annotation: {"Join":{"into_block_id":id}} or {"Split":{"from_block_id":id}}."#,
    ),
    (
        "border_group_id",
        "Group id for consecutive paragraphs sharing one visual border box (OOXML paragraph border merging).",
    ),
    (
        "paragraph_mark_status",
        "Tracked status of the paragraph mark itself, when tracked; the last entry in `segments` is then the synthesized newline segment for that change.",
    ),
];

const INLINE_TEXT_FIELDS: &[(&str, &str)] = &[
    ("text", "The run's visible text."),
    (
        "marks",
        r#"Boolean marks as strings: "Bold", "Italic", "Underline", "Subscript", "Superscript". Strike and the other tri-state toggles live in `style_props`."#,
    ),
    (
        "style_props",
        "The run's value-carrying [style properties](#style-properties): fonts, size, color, highlight, underline style, and every tri-state toggle.",
    ),
    (
        "formatting_change",
        "The tracked formatting change pending on this run (the before state from `w:rPrChange`), or null; see [formatting change](#formatting-change).",
    ),
    (
        "rev_id",
        "Engine revision identity of the tracked change this span belongs to; 0 when there is no selectable revision (a pairwise diff projection or a legacy change). Present on `Inserted` and `Deleted`, absent on `Unchanged`. This is the bridge from a rendered span to its `revisions()` row for selective accept and reject.",
    ),
];

const INLINE_OPAQUE_FIELDS: &[(&str, &str)] = &[
    (
        "segment_type",
        r#""Equal", "Insert", or "Delete": the diff role of this anchor."#,
    ),
    (
        "kind",
        r#"What the anchor is: "Drawing", "Omml", "Hyperlink", "Field", "Sdt", "Ruby", "SmartArt", "CommentReference", "FootnoteReference", "EndnoteReference", "SmartTag", "Sym", "Ptab", "CustomXml", or {"Unknown":name}."#,
    ),
    ("opaque_id", "The anchor's stable id."),
    (
        "inline_index",
        "Position of the anchor in the block's inline stream.",
    ),
    ("text", "Visible label when known, else null."),
    (
        "reference_id",
        "The `w:id` for footnote, endnote, and comment references; else null.",
    ),
    (
        "field_kind",
        r#"For fields: "Begin", "Instruction", "Separate", "End", "Simple", or {"Unknown":name}."#,
    ),
    ("field_instruction", "Field instruction text, for fields."),
    (
        "asset_ref",
        "Asset payload: an image data URI, or raw equation OMML XML; null for other kinds.",
    ),
    (
        "asset_width_emu",
        "Drawing display width in EMU (from `wp:extent` cx); null for non-drawings.",
    ),
    (
        "asset_height_emu",
        "Drawing display height in EMU (from `wp:extent` cy); null for non-drawings.",
    ),
    ("alt_text", "Alt text from `wp:docPr` descr, when present."),
    (
        "url",
        "Hyperlink target: the external URL, or `#anchor` for an internal bookmark link.",
    ),
    (
        "content_hash",
        "The drawing's own stable content hash, the guard `set_image_attrs` validates; distinct from the containing block's guard.",
    ),
];

const STYLE_PROPS_FIELDS: &[(&str, &str)] = &[
    (
        "font_family",
        "Resolved single Latin font from `w:rFonts` (ascii or hAnsi slot).",
    ),
    (
        "font_family_theme",
        r#"Theme font reference for the ascii or hAnsi slot (e.g. "minorHAnsi"); theme attributes take precedence over direct font names."#,
    ),
    ("font_size", "Font size in half-points (24 means 12pt)."),
    (
        "color",
        r#"Text color from `w:color` ("FF0000" or "auto")."#,
    ),
    (
        "color_theme",
        "Theme color reference (themeColor, themeShade, themeTint); when present it wins and `color` is the pre-resolved fallback.",
    ),
    (
        "highlight",
        "Highlight color (the OOXML highlight vocabulary), or null.",
    ),
    (
        "underline_style",
        r#"Underline style from `w:u` ("Single", "Double", "Dotted", ...), or null."#,
    ),
    (
        "font_east_asia",
        "East Asian font family from `w:rFonts` eastAsia.",
    ),
    (
        "font_east_asia_theme",
        "Theme font reference for the eastAsia slot.",
    ),
    ("font_cs", "Complex script font family from `w:rFonts` cs."),
    ("font_cs_theme", "Theme font reference for the cs slot."),
    ("lang", r#"Language tag from `w:lang` (e.g. "en-US")."#),
    ("lang_east_asia", "East Asian language tag."),
    (
        "char_spacing",
        "Character spacing in twips from `w:spacing`.",
    ),
    ("char_style_id", "Character style id from `w:rStyle`."),
    ("run_border", "Run-level border from `w:bdr`, or null."),
    (
        "position",
        "Vertical offset in half-points from `w:position`; positive raises, negative lowers.",
    ),
    ("kern", "Kerning threshold in half-points from `w:kern`."),
    (
        "char_width_scaling",
        "Character width scaling percent from `w:w`; 100 is normal.",
    ),
    (
        "bold_cs",
        "Complex script bold, a [tri-state](#tri-state-toggles).",
    ),
    ("italic_cs", "Complex script italic, tri-state."),
    ("strike", "Strikethrough, tri-state."),
    ("double_strike", "Double strikethrough, tri-state."),
    ("caps", "All caps, tri-state."),
    ("small_caps", "Small caps, tri-state."),
    ("vanish", "Hidden text, tri-state."),
    (
        "web_hidden",
        "Hidden in web view (distinct from `vanish`), tri-state.",
    ),
    ("emboss", "Embossed text, tri-state."),
    ("imprint", "Imprinted text, tri-state."),
    ("outline", "Outline text, tri-state."),
    ("shadow", "Shadow text, tri-state."),
    ("font_size_cs", "Complex script font size in half-points."),
    ("rtl", "Right-to-left run flag, tri-state."),
    ("cs", "Complex script flag, tri-state."),
    (
        "font_hint",
        "Font hint from `w:rFonts` hint, for ambiguous Unicode ranges.",
    ),
    ("no_proof", "Suppress proofing marks, tri-state."),
    ("spec_vanish", "Style separator vanish, tri-state."),
    ("o_math", "Math formatting context, tri-state."),
    ("snap_to_grid", "Snap to document grid, tri-state."),
    ("run_shading", "Run-level shading from `w:shd`, or null."),
    (
        "emphasis_mark",
        "East Asian emphasis mark from `w:em`, or null.",
    ),
    (
        "text_effect",
        "Animated text effect from `w:effect`, or null.",
    ),
    ("fit_text", "Fit text constraint from `w:fitText`, or null."),
    (
        "preserved",
        "Unmodeled run properties carried verbatim: an array of `{name, raw_xml}` pairs (qualified element name plus the exact serialized subtree), captured at import and re-emitted on serialization. The engine never synthesizes these; render consumers may ignore them, but their presence makes two runs format-distinct.",
    ),
];

const FORMATTING_CHANGE_FIELDS: &[(&str, &str)] = &[
    ("previous_marks", "The boolean marks before the change."),
    (
        "previous_style_props",
        "The [style properties](#style-properties) before the change.",
    ),
    (
        "previous_rpr_authored",
        "Per-slot authored-versus-inherited provenance of the previous state; the serializer consults it on reject so inherited values are not baked into the run. Internal detail for a renderer.",
    ),
    (
        "revision_id",
        "The wire `w:id` of the formatting change (Word does not keep it unique); pairing detail, never an address.",
    ),
    ("author", "Revision author."),
    ("date", "Revision date, when carried."),
    (
        "identity",
        "Engine-minted document-unique identity, the value the resolution surface addresses; 0 is the pre-identity sentinel.",
    ),
];

const INDENTATION_FIELDS: &[(&str, &str)] = &[
    (
        "left",
        "Left indent in twips; continuation lines start here.",
    ),
    ("right", "Right indent in twips."),
    (
        "effective_first_line_twips",
        "First-line indent in twips relative to `left`; positive indents right, negative hangs. In this render projection it is the resolved first-line origin (a literal-prefix leading tab is already folded in), so apply it as a single text-indent.",
    ),
    (
        "start_chars",
        "Left indent in hundredths of a character; non-zero takes precedence over twip `left`. An explicit 0 is a real override and is preserved.",
    ),
    (
        "end_chars",
        "Right indent in hundredths of a character, same precedence rule.",
    ),
    (
        "first_line_chars",
        "First-line indent in hundredths of a character, same precedence rule.",
    ),
    (
        "hanging_chars",
        "Hanging indent in hundredths of a character, same precedence rule.",
    ),
];

const PARAGRAPH_SPACING_FIELDS: &[(&str, &str)] = &[
    ("before", "Space before the paragraph in twips."),
    ("after", "Space after the paragraph in twips."),
    (
        "before_lines",
        "Space before in hundredths of a line; takes precedence over `before`.",
    ),
    (
        "after_lines",
        "Space after in hundredths of a line; takes precedence over `after`.",
    ),
    (
        "before_autospacing",
        "When true, consumer-determined spacing overrides `before` and `before_lines`.",
    ),
    (
        "after_autospacing",
        "When true, consumer-determined spacing overrides `after` and `after_lines`.",
    ),
    (
        "line",
        "Line spacing value; interpretation depends on `line_rule`.",
    ),
    (
        "line_rule",
        r#""Auto" (line is 240ths of a line: 240 single, 480 double), "Exact" (twips, may clip), or "AtLeast" (twips, expands)."#,
    ),
];

const PARAGRAPH_BORDERS_FIELDS: &[(&str, &str)] = &[
    ("top", "Top edge, a [border](#border) or null."),
    ("bottom", "Bottom edge."),
    ("left", "Left edge."),
    ("right", "Right edge."),
    (
        "between",
        "Border between adjacent paragraphs sharing the same border set.",
    ),
    (
        "bar",
        "Vertical bar border drawn to the side of the paragraph.",
    ),
];

const BORDER_FIELDS: &[(&str, &str)] = &[
    (
        "style",
        r#"Border style from the OOXML border vocabulary ("Single", "Double", "Dashed", ...)."#,
    ),
    ("color", r#"Border color as hex, or "auto"."#),
    ("size", "Border width in eighths of a point."),
    ("space", "Border offset from text in points."),
    (
        "extra_attrs",
        "Verbatim round-trip of border attributes the typed fields do not model (theme colors, frame, shadow), as name and value pairs.",
    ),
];

const TAB_STOP_FIELDS: &[(&str, &str)] = &[
    ("position", "Tab stop position in twips."),
    (
        "alignment",
        "Tab stop alignment (the OOXML tab alignment vocabulary).",
    ),
    ("leader", "Leader character vocabulary, or null."),
];

const SECTION_PROPERTIES_FIELDS: &[(&str, &str)] = &[
    ("page_width", "Page width in twips (`w:pgSz`)."),
    ("page_height", "Page height in twips."),
    ("orientation", "Page orientation, when authored."),
    ("columns", "Number of text columns."),
    ("column_space", "Space between columns in twips."),
    (
        "column_defs",
        "Per-column width and space definitions, when columns are unequal.",
    ),
    ("margin_top", "Top margin in twips (`w:pgMar`)."),
    ("margin_bottom", "Bottom margin in twips."),
    ("margin_left", "Left margin in twips."),
    ("margin_right", "Right margin in twips."),
    ("header_distance", "Header distance in twips."),
    ("footer_distance", "Footer distance in twips."),
    ("gutter", "Gutter in twips."),
    ("rtl_gutter", "Right-to-left gutter flag."),
    ("section_type", "Section break type, when authored."),
    ("page_borders", "Page borders, when authored."),
    ("line_numbering", "Line numbering settings, when authored."),
    ("v_align", "Vertical alignment of text on the page."),
    ("text_direction", "Text flow direction."),
    ("page_number_type", "Page number format and start."),
    ("doc_grid_type", "Document grid type."),
    ("doc_grid_line_pitch", "Document grid line pitch in twips."),
    (
        "doc_grid_char_space",
        "Document grid character space in twips.",
    ),
    (
        "title_page",
        "Distinct first-page header and footer flag (`w:titlePg`).",
    ),
    ("bidi", "Right-to-left section layout flag."),
    ("form_prot", "Section-level form protection flag."),
    ("no_endnote", "Suppress endnotes in this section."),
    (
        "paper_size_code",
        "Standard paper size code, when authored.",
    ),
    (
        "column_separator",
        "Draw a vertical separator between columns.",
    ),
    (
        "equal_width",
        "Whether columns are equal width; false means `column_defs` carries per-column widths.",
    ),
    (
        "footnote_pr",
        "Section-level footnote properties, when authored.",
    ),
    (
        "endnote_pr",
        "Section-level endnote properties, when authored.",
    ),
    (
        "header_refs",
        "Effective header references for this section (own plus inherited), each naming its band kind.",
    ),
    (
        "footer_refs",
        "Effective footer references, same semantics.",
    ),
    ("paper_source", "Printer tray codes, when authored."),
    (
        "printer_settings_rid",
        "Relationship id of the printer settings part, carried verbatim.",
    ),
];

const REVISION_RECORD_FIELDS: &[(&str, &str)] = &[
    (
        "revision_id",
        "Engine-minted identity; 0 marks a reported-but-never-selectable record.",
    ),
    (
        "wire_id",
        "The raw OOXML `w:id`, diagnostics only, never an address.",
    ),
    ("author", "Author, when the source carried one."),
    ("date", "ISO-8601 date, when carried."),
    ("kind", "One of the [revision kinds](#revision-kinds)."),
    (
        "block_id",
        "The block the revision lives in (`body_section` for a body-level section change).",
    ),
    (
        "location",
        "Story scope: body, header or footer, footnote, endnote, or comment.",
    ),
    (
        "excerpt",
        r#"Visible text of the change, or a descriptor such as "formatting"."#,
    ),
];

/// The complete revision `kind` vocabulary, as `RevisionKind::as_str` emits it.
/// The wildcard-free match in `enum_vocabularies_are_pinned` breaks compilation
/// here when a variant is added, forcing this list and the page to follow.
const REVISION_KIND_TAGS: &[&str] = &[
    "insert",
    "delete",
    "format_run",
    "format_paragraph",
    "format_table",
    "format_row",
    "format_cell",
    "format_section",
    "opaque_interior",
    "move",
];

// ─── Compile-time pins ──────────────────────────────────────────────────────

/// Wildcard-free matches: adding an enum variant fails compilation HERE, so
/// the documented vocabularies cannot silently fall behind the engine.
#[allow(dead_code, clippy::match_same_arms)]
fn enum_vocabularies_are_pinned() {
    let _ = |k: RevisionKind| match k {
        RevisionKind::Insert
        | RevisionKind::Delete
        | RevisionKind::FormatRun
        | RevisionKind::FormatParagraph
        | RevisionKind::FormatTable
        | RevisionKind::FormatRow
        | RevisionKind::FormatCell
        | RevisionKind::FormatSection
        | RevisionKind::OpaqueInterior
        | RevisionKind::Move => (),
    };
    let _ = |m: Mark| match m {
        Mark::Bold | Mark::Italic | Mark::Underline | Mark::Subscript | Mark::Superscript => (),
    };
    let _ = |v: MarkValue| match v {
        MarkValue::Inherit | MarkValue::On | MarkValue::Off => (),
    };
    let _ = |a: Alignment| match a {
        Alignment::Left
        | Alignment::Center
        | Alignment::Right
        | Alignment::Justify
        | Alignment::Distribute
        | Alignment::HighKashida
        | Alignment::LowKashida
        | Alignment::MediumKashida
        | Alignment::NumTab
        | Alignment::ThaiDistribute => (),
    };
    let _ = |r: LineSpacingRule| match r {
        LineSpacingRule::Auto | LineSpacingRule::Exact | LineSpacingRule::AtLeast => (),
    };
    let _ = |b: BlockType| match b {
        BlockType::Paragraph | BlockType::Heading | BlockType::Table | BlockType::Opaque => (),
    };
    let _ = |c: ChangeType| match c {
        ChangeType::Unchanged
        | ChangeType::Modified
        | ChangeType::Inserted
        | ChangeType::Deleted => {}
    };
    let _ = |s: InlineChangeSegmentType| match s {
        InlineChangeSegmentType::Equal
        | InlineChangeSegmentType::Insert
        | InlineChangeSegmentType::Delete => (),
    };
    let _ = |k: OpaqueSegmentKind| match k {
        OpaqueSegmentKind::Drawing
        | OpaqueSegmentKind::Omml
        | OpaqueSegmentKind::Hyperlink
        | OpaqueSegmentKind::Field
        | OpaqueSegmentKind::Sdt
        | OpaqueSegmentKind::Ruby
        | OpaqueSegmentKind::SmartArt
        | OpaqueSegmentKind::CommentReference
        | OpaqueSegmentKind::FootnoteReference
        | OpaqueSegmentKind::EndnoteReference
        | OpaqueSegmentKind::SmartTag
        | OpaqueSegmentKind::Sym
        | OpaqueSegmentKind::Ptab
        | OpaqueSegmentKind::CustomXml => (),
        OpaqueSegmentKind::Unknown(_) => (),
    };
    let _ = |m: MoveDirection| match m {
        MoveDirection::From | MoveDirection::To => (),
    };
    let _ = |s: StructuralChange| match s {
        StructuralChange::Join { .. } | StructuralChange::Split { .. } => (),
    };
    let _ = |i: ImageMetadataChange| match i {
        ImageMetadataChange::Size
        | ImageMetadataChange::Cropping
        | ImageMetadataChange::AltText => {}
    };
    let _ = |t: TrackingStatus| match t {
        TrackingStatus::Normal
        | TrackingStatus::Inserted(_)
        | TrackingStatus::Deleted(_)
        | TrackingStatus::InsertedThenDeleted(_) => (),
    };
    let _ = |t: TrackStatus| match t {
        TrackStatus::Normal
        | TrackStatus::Inserted(_)
        | TrackStatus::Deleted(_)
        | TrackStatus::InsertedThenDeleted { .. } => (),
    };
}

/// Function-pointer casts: the documented facade read surface, pinned by
/// signature. A rename or signature change fails compilation HERE first.
#[test]
fn facade_read_surface_signatures_hold() {
    use stemma::tracked_model;
    let _: fn(&[u8]) -> Result<Document, stemma::RuntimeError> = Document::parse;
    let _: fn(&Document) -> DocumentView = Document::read;
    let _: fn(&Document) -> String = Document::to_text;
    let _: fn(&Document) -> String = Document::to_markdown;
    let _: fn(&Document) -> String = Document::to_html;
    let _: fn(&Document) -> DocumentOutline = Document::outline;
    let _: fn(&Document, &str, &str, WindowFormat) -> Result<String, WindowError> =
        Document::window;
    let _: fn(&Document) -> Result<Document, stemma::RuntimeError> = Document::read_accepted;
    let _: fn(&Document) -> Result<Document, stemma::RuntimeError> = Document::read_rejected;
    let _: fn(&Document) -> Vec<tracked_model::RevisionRecord> = Document::revisions;
    let _: fn(&Document, &stemma::ExportOptions) -> Result<Vec<u8>, stemma::RuntimeError> =
        Document::serialize;
    let _: fn(&Document) -> &stemma::EditSnapshot = Document::snapshot;
    let _: fn(&stemma::EditSnapshot) -> stemma::domain::FullDocViewResult =
        build_tracked_document_view_from_snapshot;
}

// ─── The page ───────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
fn render() -> String {
    let doc = exemplar();

    // Field-table pin: a RevisionRecord destructuring pattern breaks
    // compilation when the struct changes shape.
    let revisions = doc.revisions();
    let RevisionRecord {
        revision_id: _,
        wire_id: _,
        author: _,
        date: _,
        kind: first_kind,
        block_id: _,
        location: _,
        excerpt: _,
    } = revisions
        .first()
        .expect("exemplar carries pending revisions")
        .clone();
    assert!(
        REVISION_KIND_TAGS.contains(&first_kind.as_str()),
        "revision kind {:?} missing from the documented tag list",
        first_kind.as_str()
    );

    // ── Lean view examples ──
    let lean = doc.read();
    let tracked_block = find_block(&lean, "capped at");
    let lean_example = serde_json::to_value(tracked_block).expect("BlockView serializes");
    assert_fields("BlockView", &lean_example, BLOCK_VIEW_FIELDS, &[]);

    let mut text_keys: Option<Value> = None;
    let mut opaque_keys: Option<Value> = None;
    let mut revision_view: Option<Value> = None;
    for block in &lean.blocks {
        for seg in &block.segments {
            let v = serde_json::to_value(seg).expect("SegmentView serializes");
            match seg {
                SegmentView::Text { status, .. } => {
                    if text_keys.is_none() {
                        text_keys = Some(v["Text"].clone());
                    }
                    if revision_view.is_none()
                        && let TrackStatus::Inserted(rv) = status
                    {
                        revision_view =
                            Some(serde_json::to_value(rv).expect("RevisionView serializes"));
                    }
                }
                SegmentView::Opaque { .. } => {
                    if opaque_keys.is_none() {
                        opaque_keys = Some(v["Opaque"].clone());
                    }
                }
            }
        }
    }
    let text_seg = text_keys.expect("exemplar has text segments");
    assert_fields("SegmentView::Text", &text_seg, SEGMENT_TEXT_FIELDS, &[]);
    assert_fields(
        "SegmentView::Opaque",
        &opaque_keys.expect("exemplar has an opaque anchor (the hyperlink)"),
        SEGMENT_OPAQUE_FIELDS,
        &["metadata"],
    );
    assert_fields(
        "RevisionView",
        &revision_view.expect("exemplar has an inserted span"),
        REVISION_VIEW_FIELDS,
        &[],
    );

    // ListMembership is absent from the exemplar (no numbering part in the
    // minimal DOCX); pin its shape by deserializing into the real type and
    // re-serializing. A field the type dropped fails here; a field it gained
    // appears in the round-tripped value and fails `assert_fields`.
    let list_membership: stemma::view::ListMembership = serde_json::from_value(serde_json::json!({
        "num_id": 3, "ilvl": 0, "ordered": true, "marker_text": "1."
    }))
    .expect("ListMembership round-trips");
    assert_fields(
        "ListMembership",
        &serde_json::to_value(&list_membership).expect("ListMembership serializes"),
        LIST_MEMBERSHIP_FIELDS,
        &[],
    );

    let table_block = lean
        .blocks
        .iter()
        .find(|b| !b.cells.is_empty())
        .expect("exemplar has a table");
    let cell_example = serde_json::to_value(&table_block.cells[0]).expect("cell serializes");
    assert_fields("TableCellView", &cell_example, TABLE_CELL_FIELDS, &[]);
    assert_fields(
        "CellParagraphView",
        &cell_example["paragraphs"][0],
        CELL_PARAGRAPH_FIELDS,
        &[],
    );
    let table_meta = serde_json::to_value(
        table_block
            .table
            .as_ref()
            .expect("table block carries metadata"),
    )
    .expect("TableMetaView serializes");
    assert_fields("TableMetaView", &table_meta, TABLE_META_FIELDS, &[]);

    // ── Full render view examples ──
    let full = build_tracked_document_view_from_snapshot(doc.snapshot());
    let rich_block = full
        .blocks
        .iter()
        .find(|b| b.block_id == tracked_block.id)
        .expect("full view carries the tracked block");
    let rich_example = serde_json::to_value(rich_block).expect("FullDocBlock serializes");
    assert_fields("FullDocBlock", &rich_example, FULL_DOC_BLOCK_FIELDS, &[]);

    let mut inline_text: Option<Value> = None;
    let mut inline_opaque: Option<Value> = None;
    let mut formatting_change: Option<Value> = None;
    for block in &full.blocks {
        for seg in &block.segments {
            let v = serde_json::to_value(seg).expect("InlineChange serializes");
            match seg {
                InlineChange::Inserted { .. } if inline_text.is_none() => {
                    inline_text = Some(v["Inserted"].clone());
                }
                InlineChange::Unchanged {
                    formatting_change: Some(_),
                    ..
                } if formatting_change.is_none() => {
                    formatting_change = Some(v["Unchanged"]["formatting_change"].clone());
                }
                InlineChange::Opaque { .. } if inline_opaque.is_none() => {
                    inline_opaque = Some(v["Opaque"].clone());
                }
                _ => {}
            }
        }
    }
    let inline_text = inline_text.expect("full view has an inserted segment");
    assert_fields(
        "InlineChange::Inserted",
        &inline_text,
        INLINE_TEXT_FIELDS,
        &[],
    );
    assert_fields(
        "InlineChange::Opaque",
        &inline_opaque.expect("full view has an opaque segment (the hyperlink)"),
        INLINE_OPAQUE_FIELDS,
        &[],
    );
    assert_fields(
        "StyleProps",
        &inline_text["style_props"],
        STYLE_PROPS_FIELDS,
        &[],
    );
    assert_fields(
        "FormattingChange",
        &formatting_change.expect("full view has a pending formatting change (set_format)"),
        FORMATTING_CHANGE_FIELDS,
        &[],
    );

    let section = serde_json::to_value(
        full.body_section_properties
            .as_ref()
            .expect("exemplar sectPr projects section properties"),
    )
    .expect("SectionProperties serializes");
    assert_fields(
        "SectionProperties",
        &section,
        SECTION_PROPERTIES_FIELDS,
        &[],
    );

    // Geometry shapes absent from the exemplar: construct them against the
    // real types (a struct literal fails compilation when fields change).
    assert_fields(
        "Indentation",
        &serde_json::to_value(stemma::domain::Indentation::default()).expect("serializes"),
        INDENTATION_FIELDS,
        &[],
    );
    assert_fields(
        "ParagraphSpacing",
        &serde_json::to_value(stemma::domain::ParagraphSpacing {
            before: None,
            after: None,
            before_lines: None,
            after_lines: None,
            before_autospacing: None,
            after_autospacing: None,
            line: None,
            line_rule: None,
        })
        .expect("serializes"),
        PARAGRAPH_SPACING_FIELDS,
        &[],
    );
    assert_fields(
        "ParagraphBorders",
        &serde_json::to_value(stemma::domain::ParagraphBorders {
            top: None,
            bottom: None,
            left: None,
            right: None,
            between: None,
            bar: None,
        })
        .expect("serializes"),
        PARAGRAPH_BORDERS_FIELDS,
        &[],
    );
    assert_fields(
        "Border",
        &serde_json::to_value(stemma::domain::Border {
            style: stemma::domain::BorderStyle::Single,
            color: None,
            size: None,
            space: None,
            extra_attrs: Vec::new(),
        })
        .expect("serializes"),
        BORDER_FIELDS,
        &[],
    );
    // TabStopDef's module is private, so pin its shape from a live value: the
    // exemplar's fourth paragraph authors a center tab stop.
    let tab_stop_pin = full
        .blocks
        .iter()
        .find_map(|b| {
            serde_json::to_value(b).expect("block serializes")["tab_stops"]
                .as_array()
                .and_then(|a| a.first().cloned())
        })
        .expect("exemplar authors a tab stop");
    assert_fields("TabStopDef", &tab_stop_pin, TAB_STOP_FIELDS, &[]);

    // ── Assemble the page ──
    let lean_pretty = pretty(&lean_example);
    let cell_pretty = pretty(&cell_example);
    let rich_pretty = pretty(&rich_example);
    let section_pretty = pretty(&section);

    let mut page = String::new();
    let _ = write!(
        page,
        "\
# Read model reference

<!-- GENERATED FILE. Do not edit by hand: this page is rendered from live
     engine values by stemma-engine/tests/read_model_reference.rs, and that
     test fails the gate when the page drifts (field tables are asserted
     against the engine's own serialization; every example is real engine
     output for a small exemplar document). Regenerate with:

         just regen-read-model-reference
-->

The typed read model: what you render a document FROM, on every surface. The
[v4 operation reference](operations.md) documents the write half (the durable,
strictly-parsed transaction wire); this page documents the read half, rendered
from live engine values so it cannot silently disagree with the engine.

**Stability, stated honestly:** the read model is public but ENGINE-VERSION
BOUND ([Tier 2 and 3](../guide/stability.md#rust-api)), the deliberate
opposite of the additive v4 write wire. Render from it live and re-read after
every engine upgrade. Never persist it: durability is DOCX bytes plus v4
transactions, which replay to any state. Fields are added between releases;
a consumer must tolerate unknown fields when deserializing these shapes.

## The layers

```text
DOCX bytes + v4 transactions     durable, portable: persist exactly these
        |  import / replay
        v
the typed IR (CanonDoc)          Tier 2: public, version-bound, in-memory only
        |  projected on demand
        v
lean view      full render view  this page: the read model
        |
        v
your renderer or product         built on the api::Document facade (Tier 1)
```

Two projections serve different jobs, and both are views over the same IR:

* The **lean view** is the editing-and-navigation read: block ids, guards,
  visible text, tracked statuses, span handles, table cell addressing. It is
  what `api::Document::read()` returns.
* The **full render view** is the render-faithful read: every run carries its
  resolved value formatting (`style_props`), opaque anchors carry their asset
  payloads (image data URIs, equation OMML, EMU extents), blocks carry
  paragraph geometry, and the result includes header, footer, comment, and
  note stories plus page geometry.

## Reading with the facade

Build a product on the Tier 1 `api::Document` facade, not on a transport's
JSON. Its read surface (each method's signature is compile-pinned by the test
that renders this page):

| Method | Returns |
|---|---|
| `read()` | The lean view below (`DocumentView`). |
| `to_text()`, `to_markdown()`, `to_html()` | One-way TEXT projections of the current reading. They flatten; they are not the render read. |
| `outline()`, `window(from, to, format)` | Structural navigation slices. |
| `read_accepted()`, `read_rejected()` | Accept-all and reject-all projections, each a new `Document`. |
| `revisions()` | Pending tracked changes as [revision records](#revision-records). |
| `review()` | The session audit report. |
| `serialize(&ExportOptions)` | Validated DOCX bytes back out. |
| `snapshot()` | The documented Tier 3 escape hatch; the full render view is built from it. |

The write surface (`apply`, `apply_authored`, `diff`, `diff_as`, `project`)
is documented in the [v4 operation reference](operations.md) and the
[stability policy](../guide/stability.md#rust-api).

## Where each surface serves the read model

| Surface | Lean view | Full render view |
|---|---|---|
| Rust | `Document::read()` | `stemma::runtime::build_tracked_document_view_from_snapshot(doc.snapshot())`, or `SimpleRuntime::single_document_view` / `full_document_view` |
| HTTP | `GET /api/documents/{{id}}` serves a reduced hand-projection of it | `GET /api/documents/{{id}}/rich` serializes it whole, stamping each block with the lean `guard` and attaching the lean table `cells` and `table` metadata by block id |
| MCP | `inspect_docx` with `detail:\"formatting\"` serves a projection of it (block detail plus spans) | not exposed; cell interiors reach MCP through the lean view's `paragraphs`, which reuse the full view's segment shape |

Two honest caveats a builder should know:

* The HTTP and MCP lean projections rename and trim fields; the engine types
  documented here are the model, and each transport page shows its own wire.
  `GET /api/documents/{{id}}/rich` is the one surface that serializes the
  engine types verbatim (plus the stamped `guard`, `cells`, and `table` keys).
* The full view result also carries `footnotes` and `endnotes` stories, but
  the HTTP `/rich` envelope does not currently include them; over HTTP, note
  TEXT is reachable only by resolving the inline reference anchors.

## Units

| Unit | Where |
|---|---|
| Half-points | `font_size`, `font_size_cs`, `position`, `kern` (24 half-points is 12pt) |
| Twips, 1/1440 inch | indentation, spacing `before`/`after`, `char_spacing`, tab stop `position`, table `cols` and `indent`, every section dimension |
| Hundredths of a line | `before_lines`, `after_lines`; `line` is 240ths of a line when `line_rule` is `\"Auto\"` |
| Hundredths of a character | `start_chars`, `end_chars`, `first_line_chars`, `hanging_chars` |
| Percent | `char_width_scaling` (100 is normal) |
| EMU, 914400 per inch | `asset_width_emu`, `asset_height_emu`, drawing extents |
| Eighths of a point | border `size` |
| Data URI | `image_data_uris`, image `asset_ref` |
| OMML XML | `equation_xmls`, equation `asset_ref` |

## The lean view

`DocumentView` is a flat list of blocks in document order: `{{\"blocks\":[...]}}`.
Every example below is real engine output for a small exemplar document that
was edited through the same v4 wire path every transport uses.

### Block

{block_view_table}
### Track status

A span or block's tracked state makes invalid states unrepresentable: a
`\"Normal\"` span carries no revision, and a tracked span always carries the
revision that produced it:

* `\"Normal\"`
* `{{\"Inserted\": revision}}`
* `{{\"Deleted\": revision}}`
* `{{\"InsertedThenDeleted\": {{\"inserted\": revision, \"deleted\": revision}}}}`, text
  inserted by one pending revision and deleted by another; resolve it, do not
  edit it.

Each `revision` object:

{revision_view_table}
### Segments

A lean segment is externally tagged: `{{\"Text\": {{...}}}}` or `{{\"Opaque\": {{...}}}}`.

`Text` fields:

{segment_text_table}
`Opaque` fields:

{segment_opaque_table}
### A real lean block

The exemplar's tracked replacement, exactly as the engine serializes it (the
segments walk unchanged, deleted, and inserted spans, and the inserted phrase
carries a bold mark):

```json
{lean_pretty}
```

### Tables in the lean view

A table block's `cells` array addresses the grid; each cell's `paragraphs`
carry render-ready segments in the full view's `InlineChange` shape, so one
segment renderer covers body text and cell interiors. List membership, when a
paragraph participates in Word auto-numbering, has fields `num_id`, `ilvl`,
`ordered`, and `marker_text`.

Cell fields:

{table_cell_table}
Cell paragraph fields:

{cell_paragraph_table}
Table metadata (`table` on the block): {table_meta_inline}

One real cell from the exemplar's table:

```json
{cell_pretty}
```

## The full render view

The full view result carries `blocks` (below), the `footnotes`, `endnotes`,
and `comments` stories, the `headers` and `footers` bands, and
`body_section_properties`. Body blocks serialize as follows.

### Full block

{full_doc_block_table}
### Segments in the full render view

A segment is externally tagged with its diff role: `{{\"Unchanged\": {{...}}}}`,
`{{\"Inserted\": {{...}}}}`, `{{\"Deleted\": {{...}}}}`, or `{{\"Opaque\": {{...}}}}`.

The three text variants share these fields:

{inline_text_table}
`Opaque` fields:

{inline_opaque_table}
### Style properties

Every text segment carries `style_props`, the run's RESOLVED value formatting:
the style cascade (direct formatting, character style, paragraph style,
document defaults) is collapsed at import, so a renderer applies these values
directly instead of re-implementing the cascade. What was authored directly
versus inherited is provenance the serializer tracks separately; it is not
part of the render read.

{style_props_table}
#### Tri-state toggles

Toggle properties are `\"Inherit\"` (absent in the source, resolve from
context), `\"On\"`, or `\"Off\"` (explicitly disabled). A renderer treats
`\"Inherit\"` as off unless its own style context says otherwise.

### Formatting change

A pending tracked FORMATTING change carries the before state, so a renderer
can show what the formatting was and a reject can restore it:

{formatting_change_table}
### Paragraph geometry

#### Indentation

{indentation_table}
#### Paragraph spacing

{paragraph_spacing_table}
#### Paragraph borders

{paragraph_borders_table}
#### Border

{border_table}
#### Tab stops

{tab_stop_table}
### Enumerated vocabularies

Every value list below is compile-pinned against the engine's enums by the
test that renders this page.

* Block `align` and story paragraph `align`: `\"Left\"`, `\"Center\"`,
  `\"Right\"`, `\"Justify\"`, `\"Distribute\"`, `\"HighKashida\"`,
  `\"LowKashida\"`, `\"MediumKashida\"`, `\"NumTab\"`, `\"ThaiDistribute\"`.
* `block_type`: `\"Paragraph\"`, `\"Heading\"`, `\"Table\"`, `\"Opaque\"`.
* `change_type`: `\"Unchanged\"`, `\"Modified\"`, `\"Inserted\"`, `\"Deleted\"`.
* Opaque `segment_type`: `\"Equal\"`, `\"Insert\"`, `\"Delete\"`.
* `move_direction`: `\"From\"`, `\"To\"`.
* `image_metadata_changes` entries: `\"Size\"`, `\"Cropping\"`, `\"AltText\"`.
* `line_rule`: `\"Auto\"`, `\"Exact\"`, `\"AtLeast\"`.

### Stories and bands

The full view projects the parallel stories with the SAME segment shape body
blocks use:

| Story | Shape |
|---|---|
| `footnotes`, `endnotes` | `{{\"id\", \"segments\"}}` per story. |
| `comments` | `{{\"id\", \"author\", \"date\", \"segments\", \"resolved\", \"parent_para_id\"}}`; `resolved` comes from the comments-extended part, and `parent_para_id` links a reply to its thread parent. A commented span in the body carries a `CommentReference` opaque anchor whose `reference_id` equals the comment `id`. |
| `headers`, `footers` | One band per reference the body section binds: `{{\"kind\": \"default\"/\"first\"/\"even\", \"paragraphs\": [...]}}`, each paragraph carrying `align`, `tab_stops`, and `segments` so a centered footer renders centered. |

### Section properties

Page geometry for the body section; every dimension is twips.

{section_properties_table}
### A real full-view block

The same tracked replacement as in the lean example, in the full render view.
Note the external segment tags, the per-segment `style_props`, and `rev_id`
linking each tracked span to its revision record:

```json
{rich_pretty}
```

### The exemplar's section properties

```json
{section_pretty}
```

### Images and equations, twice

Media surfaces in TWO representations, deliberately: inline `Opaque` segments
(`asset_ref` plus EMU extents, positioned in the text flow) for faithful
rendering, and flattened block-level arrays (`image_data_uris`,
`equation_xmls`, with `*_doc1_count` splitting base from target) for consumers
that only need the assets. Render from the segments; the arrays are the
cheap census.

### Preserved, unmodeled properties

Run properties the engine does not model are never dropped: they ride in
`style_props.preserved` as `{{\"name\", \"raw_xml\"}}` pairs, verbatim from the
source, and are re-emitted on serialization. A renderer may ignore them; an
auditor can read them; the engine never synthesizes them.

## Revision identity and the review loop

Every tracked span links to its revision: the lean view through
`status.Inserted.revision_id`, the full render view through `rev_id` (and,
for a pending FORMAT change, through `formatting_change.identity` on the
run). These are ENGINE
MINTED identities, unique within the document, minted at import and by every
producer; they are not the wire `w:id`, which Word does not keep unique. A
review UI renders spans, groups them by revision id, and passes those ids to
selective accept or reject. That loop is the same on every transport
(`Resolution::Selective` in Rust, revision selections over MCP,
`POST /resolve` over HTTP).

### Revision records

`Document::revisions()` enumerates the pending changes:

{revision_record_table}
### Revision kinds

{revision_kinds_list}

Transports serve projections of these records (HTTP `GET /revisions` rows
carry `revision_id`, `author`, `kind`, `block_id`, `excerpt`, `date`, and
omit records with `revision_id` 0, which are reported but never selectable).

## Related

* [Stability and compatibility](../guide/stability.md): the tier vocabulary this page is bound by.
* [v4 operation reference](operations.md): the write half, generated the same way.
* [Embed the engine](embedding.md): the facade and session runtime these views are read through.
* [HTTP API reference](http.md): the demo transport's wire for both views.
* [MCP advanced reference](mcp-advanced.md): the tool surface over the same model.
",
        block_view_table = field_table(BLOCK_VIEW_FIELDS),
        revision_view_table = field_table(REVISION_VIEW_FIELDS),
        segment_text_table = field_table(SEGMENT_TEXT_FIELDS),
        segment_opaque_table = field_table(SEGMENT_OPAQUE_FIELDS),
        table_cell_table = field_table(TABLE_CELL_FIELDS),
        cell_paragraph_table = field_table(CELL_PARAGRAPH_FIELDS),
        table_meta_inline = TABLE_META_FIELDS
            .iter()
            .map(|(k, v)| format!("`{k}` ({})", v.trim_end_matches('.')))
            .collect::<Vec<_>>()
            .join(", "),
        full_doc_block_table = field_table(FULL_DOC_BLOCK_FIELDS),
        inline_text_table = field_table(INLINE_TEXT_FIELDS),
        inline_opaque_table = field_table(INLINE_OPAQUE_FIELDS),
        style_props_table = field_table(STYLE_PROPS_FIELDS),
        formatting_change_table = field_table(FORMATTING_CHANGE_FIELDS),
        indentation_table = field_table(INDENTATION_FIELDS),
        paragraph_spacing_table = field_table(PARAGRAPH_SPACING_FIELDS),
        paragraph_borders_table = field_table(PARAGRAPH_BORDERS_FIELDS),
        border_table = field_table(BORDER_FIELDS),
        tab_stop_table = field_table(TAB_STOP_FIELDS),
        section_properties_table = field_table(SECTION_PROPERTIES_FIELDS),
        revision_record_table = field_table(REVISION_RECORD_FIELDS),
        revision_kinds_list = REVISION_KIND_TAGS
            .iter()
            .map(|t| format!("`{t}`"))
            .collect::<Vec<_>>()
            .join(", "),
        lean_pretty = lean_pretty,
        cell_pretty = cell_pretty,
        rich_pretty = rich_pretty,
        section_pretty = section_pretty,
    );

    for (line_number, line) in page.lines().enumerate() {
        for banned in ["\u{2014}", "\u{2013}", " - "] {
            assert!(
                !line.contains(banned),
                "rendered page line {} contains sequence {banned:?} banned by scripts/check-docs.py: {line}",
                line_number + 1
            );
        }
        assert!(
            !line.ends_with(" -"),
            "rendered page line {} ends with a dash, banned by scripts/check-docs.py: {line}",
            line_number + 1
        );
    }
    page
}

#[test]
fn read_model_reference_is_current() {
    let want = render();
    let have = std::fs::read_to_string(doc_path()).unwrap_or_else(|e| {
        panic!(
            "docs/reference/read-model.md is missing ({e}); run `just regen-read-model-reference`"
        )
    });
    assert!(
        have == want,
        "docs/reference/read-model.md is stale relative to the engine read model; \
         run `just regen-read-model-reference` and commit the result"
    );
}

#[test]
#[ignore = "writes docs/reference/read-model.md; run via `just regen-read-model-reference`"]
fn regenerate_read_model_reference() {
    let path = doc_path();
    std::fs::write(&path, render()).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

// Suppress the "never used" lint for the compile-pin helper: its value IS
// that it compiles.
#[test]
fn enum_pins_compile() {
    enum_vocabularies_are_pinned();
}
