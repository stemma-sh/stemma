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
| HTTP | `GET /api/documents/{id}` serves a reduced hand-projection of it | `GET /api/documents/{id}/rich` serializes it whole, stamping each block with the lean `guard` and attaching the lean table `cells` and `table` metadata by block id |
| MCP | `inspect_docx` with `detail:"formatting"` serves a projection of it (block detail plus spans) | not exposed; cell interiors reach MCP through the lean view's `paragraphs`, which reuse the full view's segment shape |

Two honest caveats a builder should know:

* The HTTP and MCP lean projections rename and trim fields; the engine types
  documented here are the model, and each transport page shows its own wire.
  `GET /api/documents/{id}/rich` is the one surface that serializes the
  engine types verbatim (plus the stamped `guard`, `cells`, and `table` keys).
* The full view result also carries `footnotes` and `endnotes` stories, but
  the HTTP `/rich` envelope does not currently include them; over HTTP, note
  TEXT is reachable only by resolving the inline reference anchors.

## Units

| Unit | Where |
|---|---|
| Half-points | `font_size`, `font_size_cs`, `position`, `kern` (24 half-points is 12pt) |
| Twips, 1/1440 inch | indentation, spacing `before`/`after`, `char_spacing`, tab stop `position`, table `cols` and `indent`, every section dimension |
| Hundredths of a line | `before_lines`, `after_lines`; `line` is 240ths of a line when `line_rule` is `"Auto"` |
| Hundredths of a character | `start_chars`, `end_chars`, `first_line_chars`, `hanging_chars` |
| Percent | `char_width_scaling` (100 is normal) |
| EMU, 914400 per inch | `asset_width_emu`, `asset_height_emu`, drawing extents |
| Eighths of a point | border `size` |
| Data URI | `image_data_uris`, image `asset_ref` |
| OMML XML | `equation_xmls`, equation `asset_ref` |

## The lean view

`DocumentView` is a flat list of blocks in document order: `{"blocks":[...]}`.
Every example below is real engine output for a small exemplar document that
was edited through the same v4 wire path every transport uses.

### Block

| Field | Meaning |
|---|---|
| `id` | Stable block id, the handle an edit operation targets. |
| `role` | "Paragraph", {"Heading":{"level":n}}, "Table", or "Opaque". |
| `style_id` | Paragraph or table style id, when the document carries one; else null. |
| `role_token` | The role token `insert` accepts to author a NEW paragraph formatted like this block; null for tables and opaque blocks. |
| `text` | Concatenated visible text: the CURRENT redline reading, pending insertions and not-yet-accepted deletions both present (project the document for the accept-all or reject-all reading). What a plain `expect` matches against. |
| `block_status` | Whole-block tracked status (a block-level insert or delete), a [track status](#track-status). |
| `paragraph_mark_status` | Tracked status of the trailing paragraph mark, a [track status](#track-status). |
| `guard` | The staleness guard: the block's semantic hash at read time. A write op carries it back, and a block that changed since the read fails loud with `StaleEdit`. |
| `list` | Word auto-numbering membership (`num_id`, `ilvl`, `ordered`, `marker_text`); null for non-list paragraphs and for literal-prefix lists. |
| `cells` | Per-cell grid addressing for a table block, row-major (see [tables](#tables-in-the-lean-view)); empty for non-table blocks. |
| `table` | Table-level metadata: `cols` (grid column widths, twips), `align`, `indent` (twips); null for non-table blocks. |
| `literal_prefix` | The typed-in enumeration label ("1.", "(a)") already prepended to `text`; surfaced separately because it is a structural marker, not a span an edit can address. |
| `segments` | Inline structure for fine-grained targeting; see below. |
| `opaque_label` | For an opaque block, the honest description of what kind of placeholder it is; null otherwise. |

### Track status

A span or block's tracked state makes invalid states unrepresentable: a
`"Normal"` span carries no revision, and a tracked span always carries the
revision that produced it:

* `"Normal"`
* `{"Inserted": revision}`
* `{"Deleted": revision}`
* `{"InsertedThenDeleted": {"inserted": revision, "deleted": revision}}`, text
  inserted by one pending revision and deleted by another; resolve it, do not
  edit it.

Each `revision` object:

| Field | Meaning |
|---|---|
| `revision_id` | Stable engine-minted identity, unique within the document; the value selective accept and reject address. NOT the wire `w:id`, which Word does not keep unique. |
| `author` | Author of the change, when the source carried one; the view never invents one. |
| `date` | ISO-8601 timestamp, when the source carried one. |
| `apply_op_id` | Group id of the `apply` call that created this revision; null for changes loaded from an imported DOCX. |

### Segments

A lean segment is externally tagged: `{"Text": {...}}` or `{"Opaque": {...}}`.

`Text` fields:

| Field | Meaning |
|---|---|
| `text` | The run's visible text. |
| `status` | The span's [track status](#track-status). A run breaks where status or marks change. |
| `marks` | Meaningful inline marks: "Bold", "Italic", "Underline", "Strike", "Subscript", "Superscript". Value-carrying formatting (fonts, sizes, colors) lives in the full render view, not here. |
| `handle` | Ephemeral block-local span handle (`s_0`, `s_1`, ...), valid only while the block `guard` is unchanged. |

`Opaque` fields:

| Field | Meaning |
|---|---|
| `id` | The anchor's durable id, the preferred selector for anchor-relative operations. |
| `kind` | Anchor kind from a small public vocabulary: "Drawing", "Equation", "Hyperlink", "Field", "FootnoteRef", "EndnoteRef", "Comment", "ContentControl", "HardBreak", "CommentRangeStart", "CommentRangeEnd", and future kinds (the vocabulary is non-exhaustive by design). |
| `status` | The anchor's [track status](#track-status). |
| `text` | Visible label when one is known (field result, hyperlink display text); else null. |
| `handle` | Span handle ordinal, shared with text spans so the sequence is dense. |
| `metadata` | Kind-specific structure (a content control's tag and value, a drawing's EMU extent and alt text, ...), omitted when the kind carries nothing discoverable. |

### A real lean block

The exemplar's tracked replacement, exactly as the engine serializes it (the
segments walk unchanged, deleted, and inserted spans, and the inserted phrase
carries a bold mark):

```json
{
  "block_status": "Normal",
  "cells": [],
  "guard": "v2:f6be3e6376eea6af30a9cefb60e155a63a556c047c3c9bb749c6431ad0fa30e6",
  "id": "p_2",
  "list": null,
  "literal_prefix": null,
  "opaque_label": null,
  "paragraph_mark_status": "Normal",
  "role": "Paragraph",
  "role_token": "body_text",
  "segments": [
    {
      "Text": {
        "handle": "s_0",
        "marks": [],
        "status": "Normal",
        "text": "Liability is "
      }
    },
    {
      "Text": {
        "handle": "s_1",
        "marks": [],
        "status": {
          "Deleted": {
            "apply_op_id": null,
            "author": "J. Osei",
            "date": "2026-07-06T10:30:00Z",
            "revision_id": 570499357
          }
        },
        "text": "limited to direct damages"
      }
    },
    {
      "Text": {
        "handle": "s_2",
        "marks": [],
        "status": {
          "Inserted": {
            "apply_op_id": null,
            "author": "J. Osei",
            "date": "2026-07-06T10:30:00Z",
            "revision_id": 1616159355
          }
        },
        "text": "capped at "
      }
    },
    {
      "Text": {
        "handle": "s_3",
        "marks": [
          "Bold"
        ],
        "status": {
          "Inserted": {
            "apply_op_id": null,
            "author": "J. Osei",
            "date": "2026-07-06T10:30:00Z",
            "revision_id": 1616159355
          }
        },
        "text": "twice the fees paid"
      }
    },
    {
      "Text": {
        "handle": "s_4",
        "marks": [],
        "status": "Normal",
        "text": "."
      }
    }
  ],
  "style_id": null,
  "table": null,
  "text": "Liability is limited to direct damagescapped at twice the fees paid."
}
```

### Tables in the lean view

A table block's `cells` array addresses the grid; each cell's `paragraphs`
carry render-ready segments in the full view's `InlineChange` shape, so one
segment renderer covers body text and cell interiors. List membership, when a
paragraph participates in Word auto-numbering, has fields `num_id`, `ilvl`,
`ordered`, and `marker_text`.

Cell fields:

| Field | Meaning |
|---|---|
| `row` | 0-based row index in the table grid. |
| `col` | 0-based logical grid column (after `gridBefore`; a merged cell occupies its first column). |
| `text` | Concatenated visible text of every paragraph in the cell; the address `table_op.set_cell_text` takes. |
| `col_span` | Horizontal span (`gridSpan`); 1 means no merge. |
| `row_span` | Vertical span (resolved `vMerge`); continuation cells fold into the anchor and are not emitted. |
| `borders` | The cell's four EFFECTIVE borders (cell override, else table outer or inside), each a [border](#border) or null. |
| `shading` | Background fill from cell shading (`w:shd`) as a hex color; null when none. |
| `v_align` | "top", "center", or "bottom"; null means the default (top). |
| `paragraphs` | The cell's content as render-ready paragraphs, each carrying the SAME segment shape the full render view uses (below), so one segment renderer covers body text and cells. |

Cell paragraph fields:

| Field | Meaning |
|---|---|
| `segments` | The paragraph's runs in the full render view's segment shape (marks, `style_props`, hyperlinks, per-run tracked status). |
| `block_id` | The cell paragraph's own id; a `replace` or `set_format` can target it in place, like a body paragraph. |
| `guard` | The cell paragraph's own staleness guard, same mechanism as body blocks. |

Table metadata (`table` on the block): `cols` (Grid column widths in twips, from `w:tblGrid`), `align` (Table alignment ("left", "center", "right"); null means left), `indent` (Table indent from the leading margin, in twips; null when unset)

One real cell from the exemplar's table:

```json
{
  "borders": {
    "bottom": null,
    "left": null,
    "right": null,
    "top": null
  },
  "col": 0,
  "col_span": 1,
  "paragraphs": [
    {
      "block_id": "__edit_table_r0_c0_b0",
      "guard": "dbf49bdbb62cfb7261d8472e6ddc1cf4dc9fa39e180ce48ac0cfa0eba21a3e8e",
      "segments": [
        {
          "Unchanged": {
            "formatting_change": null,
            "marks": [],
            "style_props": {
              "bold_cs": "Inherit",
              "caps": "Inherit",
              "char_spacing": null,
              "char_style_id": null,
              "char_width_scaling": null,
              "color": null,
              "color_theme": null,
              "cs": "Inherit",
              "double_strike": "Inherit",
              "emboss": "Inherit",
              "emphasis_mark": null,
              "fit_text": null,
              "font_cs": null,
              "font_cs_theme": null,
              "font_east_asia": null,
              "font_east_asia_theme": null,
              "font_family": null,
              "font_family_theme": null,
              "font_hint": null,
              "font_size": null,
              "font_size_cs": null,
              "highlight": null,
              "imprint": "Inherit",
              "italic_cs": "Inherit",
              "kern": null,
              "lang": null,
              "lang_east_asia": null,
              "no_proof": "Inherit",
              "o_math": "Inherit",
              "outline": "Inherit",
              "position": null,
              "preserved": [],
              "rtl": "Inherit",
              "run_border": null,
              "run_shading": null,
              "shadow": "Inherit",
              "small_caps": "Inherit",
              "snap_to_grid": "Inherit",
              "spec_vanish": "Inherit",
              "strike": "Inherit",
              "text_effect": null,
              "underline_style": null,
              "vanish": "Inherit",
              "web_hidden": "Inherit"
            },
            "text": "Fee"
          }
        }
      ]
    }
  ],
  "row": 0,
  "row_span": 1,
  "shading": null,
  "text": "Fee",
  "v_align": null
}
```

## The full render view

The full view result carries `blocks` (below), the `footnotes`, `endnotes`,
and `comments` stories, the `headers` and `footers` bands, and
`body_section_properties`. Body blocks serialize as follows.

### Full block

| Field | Meaning |
|---|---|
| `block_id` | Stable projection block identity. For blocks present in the target reading this is the canonical block id; for deleted-only blocks it is a stable tombstone id. |
| `doc1_block_id` | Base-side block id; null for inserted blocks. In the single-document view, base and target are the same document. |
| `doc2_block_id` | Target-side block id; null for deleted blocks. |
| `block_type` | "Paragraph", "Heading", "Table", or "Opaque". |
| `heading_level` | Heading outline level, when the block is a heading. |
| `style_id` | Paragraph or table style id, when carried. |
| `change_type` | "Unchanged", "Modified", "Inserted", or "Deleted". |
| `align` | Paragraph alignment, an [alignment value](#enumerated-vocabularies) or null. |
| `indent` | Render-resolved [indentation](#indentation): `effective_first_line_twips` already folds in a literal-prefix marker's leading-tab landing, so it is the single first-line origin to apply. |
| `spacing` | Paragraph [spacing](#paragraph-spacing), or null. |
| `borders` | Paragraph [borders](#paragraph-borders), or null. |
| `tab_stops` | Effective tab stops (`position` twips, `alignment`, `leader`); empty means no custom stops. |
| `numbering_text` | The synthesized auto-number label ("1.", "(a)"), when auto-numbered. |
| `numbering_ilvl` | The numbering level, when auto-numbered. |
| `numbering_num_id` | The numbering instance id (Word auto-numbering only; null for a literal-prefix list). Lets a consumer join a paragraph to an existing list. |
| `segments` | The block's inline runs as [segments](#segments-in-the-full-render-view). |
| `table_diff` | Structural table diff, present only when table structure changed. |
| `content_types` | Content present in this block, e.g. ["text"], ["image"], ["text","image"]. |
| `equation_xmls` | Raw OMML XML strings for equations in this block. |
| `equation_doc1_count` | How many leading entries of `equation_xmls` are base-side. |
| `image_data_uris` | Base64 data URIs for images in this block ("data:image/png;base64,..."). |
| `image_doc1_count` | How many leading entries of `image_data_uris` are base-side. |
| `image_metadata_changes` | Image metadata that changed while pixels stayed identical: "Size", "Cropping", "AltText". |
| `move_id` | Shared move identifier linking a "moved from" block to its "moved to" counterpart. |
| `move_direction` | "From" (content left here) or "To" (content arrived here); else null. |
| `structural_change` | Join or split annotation: {"Join":{"into_block_id":id}} or {"Split":{"from_block_id":id}}. |
| `border_group_id` | Group id for consecutive paragraphs sharing one visual border box (OOXML paragraph border merging). |
| `paragraph_mark_status` | Tracked status of the paragraph mark itself, when tracked; the last entry in `segments` is then the synthesized newline segment for that change. |

### Segments in the full render view

A segment is externally tagged with its diff role: `{"Unchanged": {...}}`,
`{"Inserted": {...}}`, `{"Deleted": {...}}`, or `{"Opaque": {...}}`.

The three text variants share these fields:

| Field | Meaning |
|---|---|
| `text` | The run's visible text. |
| `marks` | Boolean marks as strings: "Bold", "Italic", "Underline", "Subscript", "Superscript". Strike and the other tri-state toggles live in `style_props`. |
| `style_props` | The run's value-carrying [style properties](#style-properties): fonts, size, color, highlight, underline style, and every tri-state toggle. |
| `formatting_change` | The tracked formatting change pending on this run (the before state from `w:rPrChange`), or null; see [formatting change](#formatting-change). |
| `rev_id` | Engine revision identity of the tracked change this span belongs to; 0 when there is no selectable revision (a pairwise diff projection or a legacy change). Present on `Inserted` and `Deleted`, absent on `Unchanged`. This is the bridge from a rendered span to its `revisions()` row for selective accept and reject. |

`Opaque` fields:

| Field | Meaning |
|---|---|
| `segment_type` | "Equal", "Insert", or "Delete": the diff role of this anchor. |
| `kind` | What the anchor is: "Drawing", "Omml", "Hyperlink", "Field", "Sdt", "Ruby", "SmartArt", "CommentReference", "FootnoteReference", "EndnoteReference", "SmartTag", "Sym", "Ptab", "CustomXml", or {"Unknown":name}. |
| `opaque_id` | The anchor's stable id. |
| `inline_index` | Position of the anchor in the block's inline stream. |
| `text` | Visible label when known, else null. |
| `reference_id` | The `w:id` for footnote, endnote, and comment references; else null. |
| `field_kind` | For fields: "Begin", "Instruction", "Separate", "End", "Simple", or {"Unknown":name}. |
| `field_instruction` | Field instruction text, for fields. |
| `asset_ref` | Asset payload: an image data URI, or raw equation OMML XML; null for other kinds. |
| `asset_width_emu` | Drawing display width in EMU (from `wp:extent` cx); null for non-drawings. |
| `asset_height_emu` | Drawing display height in EMU (from `wp:extent` cy); null for non-drawings. |
| `alt_text` | Alt text from `wp:docPr` descr, when present. |
| `url` | Hyperlink target: the external URL, or `#anchor` for an internal bookmark link. |
| `content_hash` | The drawing's own stable content hash, the guard `set_image_attrs` validates; distinct from the containing block's guard. |

### Style properties

Every text segment carries `style_props`, the run's RESOLVED value formatting:
the style cascade (direct formatting, character style, paragraph style,
document defaults) is collapsed at import, so a renderer applies these values
directly instead of re-implementing the cascade. What was authored directly
versus inherited is provenance the serializer tracks separately; it is not
part of the render read.

| Field | Meaning |
|---|---|
| `font_family` | Resolved single Latin font from `w:rFonts` (ascii or hAnsi slot). |
| `font_family_theme` | Theme font reference for the ascii or hAnsi slot (e.g. "minorHAnsi"); theme attributes take precedence over direct font names. |
| `font_size` | Font size in half-points (24 means 12pt). |
| `color` | Text color from `w:color` ("FF0000" or "auto"). |
| `color_theme` | Theme color reference (themeColor, themeShade, themeTint); when present it wins and `color` is the pre-resolved fallback. |
| `highlight` | Highlight color (the OOXML highlight vocabulary), or null. |
| `underline_style` | Underline style from `w:u` ("Single", "Double", "Dotted", ...), or null. |
| `font_east_asia` | East Asian font family from `w:rFonts` eastAsia. |
| `font_east_asia_theme` | Theme font reference for the eastAsia slot. |
| `font_cs` | Complex script font family from `w:rFonts` cs. |
| `font_cs_theme` | Theme font reference for the cs slot. |
| `lang` | Language tag from `w:lang` (e.g. "en-US"). |
| `lang_east_asia` | East Asian language tag. |
| `char_spacing` | Character spacing in twips from `w:spacing`. |
| `char_style_id` | Character style id from `w:rStyle`. |
| `run_border` | Run-level border from `w:bdr`, or null. |
| `position` | Vertical offset in half-points from `w:position`; positive raises, negative lowers. |
| `kern` | Kerning threshold in half-points from `w:kern`. |
| `char_width_scaling` | Character width scaling percent from `w:w`; 100 is normal. |
| `bold_cs` | Complex script bold, a [tri-state](#tri-state-toggles). |
| `italic_cs` | Complex script italic, tri-state. |
| `strike` | Strikethrough, tri-state. |
| `double_strike` | Double strikethrough, tri-state. |
| `caps` | All caps, tri-state. |
| `small_caps` | Small caps, tri-state. |
| `vanish` | Hidden text, tri-state. |
| `web_hidden` | Hidden in web view (distinct from `vanish`), tri-state. |
| `emboss` | Embossed text, tri-state. |
| `imprint` | Imprinted text, tri-state. |
| `outline` | Outline text, tri-state. |
| `shadow` | Shadow text, tri-state. |
| `font_size_cs` | Complex script font size in half-points. |
| `rtl` | Right-to-left run flag, tri-state. |
| `cs` | Complex script flag, tri-state. |
| `font_hint` | Font hint from `w:rFonts` hint, for ambiguous Unicode ranges. |
| `no_proof` | Suppress proofing marks, tri-state. |
| `spec_vanish` | Style separator vanish, tri-state. |
| `o_math` | Math formatting context, tri-state. |
| `snap_to_grid` | Snap to document grid, tri-state. |
| `run_shading` | Run-level shading from `w:shd`, or null. |
| `emphasis_mark` | East Asian emphasis mark from `w:em`, or null. |
| `text_effect` | Animated text effect from `w:effect`, or null. |
| `fit_text` | Fit text constraint from `w:fitText`, or null. |
| `preserved` | Unmodeled run properties carried verbatim: an array of `{name, raw_xml}` pairs (qualified element name plus the exact serialized subtree), captured at import and re-emitted on serialization. The engine never synthesizes these; render consumers may ignore them, but their presence makes two runs format-distinct. |

#### Tri-state toggles

Toggle properties are `"Inherit"` (absent in the source, resolve from
context), `"On"`, or `"Off"` (explicitly disabled). A renderer treats
`"Inherit"` as off unless its own style context says otherwise.

### Formatting change

A pending tracked FORMATTING change carries the before state, so a renderer
can show what the formatting was and a reject can restore it:

| Field | Meaning |
|---|---|
| `previous_marks` | The boolean marks before the change. |
| `previous_style_props` | The [style properties](#style-properties) before the change. |
| `previous_rpr_authored` | Per-slot authored-versus-inherited provenance of the previous state; the serializer consults it on reject so inherited values are not baked into the run. Internal detail for a renderer. |
| `revision_id` | The wire `w:id` of the formatting change (Word does not keep it unique); pairing detail, never an address. |
| `author` | Revision author. |
| `date` | Revision date, when carried. |
| `identity` | Engine-minted document-unique identity, the value the resolution surface addresses; 0 is the pre-identity sentinel. |

### Paragraph geometry

#### Indentation

| Field | Meaning |
|---|---|
| `left` | Left indent in twips; continuation lines start here. |
| `right` | Right indent in twips. |
| `effective_first_line_twips` | First-line indent in twips relative to `left`; positive indents right, negative hangs. In this render projection it is the resolved first-line origin (a literal-prefix leading tab is already folded in), so apply it as a single text-indent. |
| `start_chars` | Left indent in hundredths of a character; non-zero takes precedence over twip `left`. An explicit 0 is a real override and is preserved. |
| `end_chars` | Right indent in hundredths of a character, same precedence rule. |
| `first_line_chars` | First-line indent in hundredths of a character, same precedence rule. |
| `hanging_chars` | Hanging indent in hundredths of a character, same precedence rule. |

#### Paragraph spacing

| Field | Meaning |
|---|---|
| `before` | Space before the paragraph in twips. |
| `after` | Space after the paragraph in twips. |
| `before_lines` | Space before in hundredths of a line; takes precedence over `before`. |
| `after_lines` | Space after in hundredths of a line; takes precedence over `after`. |
| `before_autospacing` | When true, consumer-determined spacing overrides `before` and `before_lines`. |
| `after_autospacing` | When true, consumer-determined spacing overrides `after` and `after_lines`. |
| `line` | Line spacing value; interpretation depends on `line_rule`. |
| `line_rule` | "Auto" (line is 240ths of a line: 240 single, 480 double), "Exact" (twips, may clip), or "AtLeast" (twips, expands). |

#### Paragraph borders

| Field | Meaning |
|---|---|
| `top` | Top edge, a [border](#border) or null. |
| `bottom` | Bottom edge. |
| `left` | Left edge. |
| `right` | Right edge. |
| `between` | Border between adjacent paragraphs sharing the same border set. |
| `bar` | Vertical bar border drawn to the side of the paragraph. |

#### Border

| Field | Meaning |
|---|---|
| `style` | Border style from the OOXML border vocabulary ("Single", "Double", "Dashed", ...). |
| `color` | Border color as hex, or "auto". |
| `size` | Border width in eighths of a point. |
| `space` | Border offset from text in points. |
| `extra_attrs` | Verbatim round-trip of border attributes the typed fields do not model (theme colors, frame, shadow), as name and value pairs. |

#### Tab stops

| Field | Meaning |
|---|---|
| `position` | Tab stop position in twips. |
| `alignment` | Tab stop alignment (the OOXML tab alignment vocabulary). |
| `leader` | Leader character vocabulary, or null. |

### Enumerated vocabularies

Every value list below is compile-pinned against the engine's enums by the
test that renders this page.

* Block `align` and story paragraph `align`: `"Left"`, `"Center"`,
  `"Right"`, `"Justify"`, `"Distribute"`, `"HighKashida"`,
  `"LowKashida"`, `"MediumKashida"`, `"NumTab"`, `"ThaiDistribute"`.
* `block_type`: `"Paragraph"`, `"Heading"`, `"Table"`, `"Opaque"`.
* `change_type`: `"Unchanged"`, `"Modified"`, `"Inserted"`, `"Deleted"`.
* Opaque `segment_type`: `"Equal"`, `"Insert"`, `"Delete"`.
* `move_direction`: `"From"`, `"To"`.
* `image_metadata_changes` entries: `"Size"`, `"Cropping"`, `"AltText"`.
* `line_rule`: `"Auto"`, `"Exact"`, `"AtLeast"`.

### Stories and bands

The full view projects the parallel stories with the SAME segment shape body
blocks use:

| Story | Shape |
|---|---|
| `footnotes`, `endnotes` | `{"id", "segments"}` per story. |
| `comments` | `{"id", "author", "date", "segments", "resolved", "parent_para_id"}`; `resolved` comes from the comments-extended part, and `parent_para_id` links a reply to its thread parent. A commented span in the body carries a `CommentReference` opaque anchor whose `reference_id` equals the comment `id`. |
| `headers`, `footers` | One band per reference the body section binds: `{"kind": "default"/"first"/"even", "paragraphs": [...]}`, each paragraph carrying `align`, `tab_stops`, and `segments` so a centered footer renders centered. |

### Section properties

Page geometry for the body section; every dimension is twips.

| Field | Meaning |
|---|---|
| `page_width` | Page width in twips (`w:pgSz`). |
| `page_height` | Page height in twips. |
| `orientation` | Page orientation, when authored. |
| `columns` | Number of text columns. |
| `column_space` | Space between columns in twips. |
| `column_defs` | Per-column width and space definitions, when columns are unequal. |
| `margin_top` | Top margin in twips (`w:pgMar`). |
| `margin_bottom` | Bottom margin in twips. |
| `margin_left` | Left margin in twips. |
| `margin_right` | Right margin in twips. |
| `header_distance` | Header distance in twips. |
| `footer_distance` | Footer distance in twips. |
| `gutter` | Gutter in twips. |
| `rtl_gutter` | Right-to-left gutter flag. |
| `section_type` | Section break type, when authored. |
| `page_borders` | Page borders, when authored. |
| `line_numbering` | Line numbering settings, when authored. |
| `v_align` | Vertical alignment of text on the page. |
| `text_direction` | Text flow direction. |
| `page_number_type` | Page number format and start. |
| `doc_grid_type` | Document grid type. |
| `doc_grid_line_pitch` | Document grid line pitch in twips. |
| `doc_grid_char_space` | Document grid character space in twips. |
| `title_page` | Distinct first-page header and footer flag (`w:titlePg`). |
| `bidi` | Right-to-left section layout flag. |
| `form_prot` | Section-level form protection flag. |
| `no_endnote` | Suppress endnotes in this section. |
| `paper_size_code` | Standard paper size code, when authored. |
| `column_separator` | Draw a vertical separator between columns. |
| `equal_width` | Whether columns are equal width; false means `column_defs` carries per-column widths. |
| `footnote_pr` | Section-level footnote properties, when authored. |
| `endnote_pr` | Section-level endnote properties, when authored. |
| `header_refs` | Effective header references for this section (own plus inherited), each naming its band kind. |
| `footer_refs` | Effective footer references, same semantics. |
| `paper_source` | Printer tray codes, when authored. |
| `printer_settings_rid` | Relationship id of the printer settings part, carried verbatim. |

### A real full-view block

The same tracked replacement as in the lean example, in the full render view.
Note the external segment tags, the per-segment `style_props`, and `rev_id`
linking each tracked span to its revision record:

```json
{
  "align": "Left",
  "block_id": "p_2",
  "block_type": "Paragraph",
  "border_group_id": null,
  "borders": null,
  "change_type": "Modified",
  "content_types": [
    "text"
  ],
  "doc1_block_id": "p_2",
  "doc2_block_id": "p_2",
  "equation_doc1_count": 0,
  "equation_xmls": [],
  "heading_level": null,
  "image_data_uris": [],
  "image_doc1_count": 0,
  "image_metadata_changes": [],
  "indent": null,
  "move_direction": null,
  "move_id": null,
  "numbering_ilvl": null,
  "numbering_num_id": null,
  "numbering_text": null,
  "paragraph_mark_status": null,
  "segments": [
    {
      "Unchanged": {
        "formatting_change": null,
        "marks": [],
        "style_props": {
          "bold_cs": "Inherit",
          "caps": "Inherit",
          "char_spacing": null,
          "char_style_id": null,
          "char_width_scaling": null,
          "color": null,
          "color_theme": null,
          "cs": "Inherit",
          "double_strike": "Inherit",
          "emboss": "Inherit",
          "emphasis_mark": null,
          "fit_text": null,
          "font_cs": null,
          "font_cs_theme": null,
          "font_east_asia": null,
          "font_east_asia_theme": null,
          "font_family": null,
          "font_family_theme": null,
          "font_hint": null,
          "font_size": null,
          "font_size_cs": null,
          "highlight": null,
          "imprint": "Inherit",
          "italic_cs": "Inherit",
          "kern": null,
          "lang": null,
          "lang_east_asia": null,
          "no_proof": "Inherit",
          "o_math": "Inherit",
          "outline": "Inherit",
          "position": null,
          "preserved": [],
          "rtl": "Inherit",
          "run_border": null,
          "run_shading": null,
          "shadow": "Inherit",
          "small_caps": "Inherit",
          "snap_to_grid": "Inherit",
          "spec_vanish": "Inherit",
          "strike": "Inherit",
          "text_effect": null,
          "underline_style": null,
          "vanish": "Inherit",
          "web_hidden": "Inherit"
        },
        "text": "Liability is "
      }
    },
    {
      "Deleted": {
        "formatting_change": null,
        "marks": [],
        "rev_id": 570499357,
        "style_props": {
          "bold_cs": "Inherit",
          "caps": "Inherit",
          "char_spacing": null,
          "char_style_id": null,
          "char_width_scaling": null,
          "color": null,
          "color_theme": null,
          "cs": "Inherit",
          "double_strike": "Inherit",
          "emboss": "Inherit",
          "emphasis_mark": null,
          "fit_text": null,
          "font_cs": null,
          "font_cs_theme": null,
          "font_east_asia": null,
          "font_east_asia_theme": null,
          "font_family": null,
          "font_family_theme": null,
          "font_hint": null,
          "font_size": null,
          "font_size_cs": null,
          "highlight": null,
          "imprint": "Inherit",
          "italic_cs": "Inherit",
          "kern": null,
          "lang": null,
          "lang_east_asia": null,
          "no_proof": "Inherit",
          "o_math": "Inherit",
          "outline": "Inherit",
          "position": null,
          "preserved": [],
          "rtl": "Inherit",
          "run_border": null,
          "run_shading": null,
          "shadow": "Inherit",
          "small_caps": "Inherit",
          "snap_to_grid": "Inherit",
          "spec_vanish": "Inherit",
          "strike": "Inherit",
          "text_effect": null,
          "underline_style": null,
          "vanish": "Inherit",
          "web_hidden": "Inherit"
        },
        "text": "limited to direct damages"
      }
    },
    {
      "Inserted": {
        "formatting_change": null,
        "marks": [],
        "rev_id": 1616159355,
        "style_props": {
          "bold_cs": "Inherit",
          "caps": "Inherit",
          "char_spacing": null,
          "char_style_id": null,
          "char_width_scaling": null,
          "color": null,
          "color_theme": null,
          "cs": "Inherit",
          "double_strike": "Inherit",
          "emboss": "Inherit",
          "emphasis_mark": null,
          "fit_text": null,
          "font_cs": null,
          "font_cs_theme": null,
          "font_east_asia": null,
          "font_east_asia_theme": null,
          "font_family": null,
          "font_family_theme": null,
          "font_hint": null,
          "font_size": null,
          "font_size_cs": null,
          "highlight": null,
          "imprint": "Inherit",
          "italic_cs": "Inherit",
          "kern": null,
          "lang": null,
          "lang_east_asia": null,
          "no_proof": "Inherit",
          "o_math": "Inherit",
          "outline": "Inherit",
          "position": null,
          "preserved": [],
          "rtl": "Inherit",
          "run_border": null,
          "run_shading": null,
          "shadow": "Inherit",
          "small_caps": "Inherit",
          "snap_to_grid": "Inherit",
          "spec_vanish": "Inherit",
          "strike": "Inherit",
          "text_effect": null,
          "underline_style": null,
          "vanish": "Inherit",
          "web_hidden": "Inherit"
        },
        "text": "capped at "
      }
    },
    {
      "Inserted": {
        "formatting_change": null,
        "marks": [
          "Bold"
        ],
        "rev_id": 1616159355,
        "style_props": {
          "bold_cs": "Inherit",
          "caps": "Inherit",
          "char_spacing": null,
          "char_style_id": null,
          "char_width_scaling": null,
          "color": null,
          "color_theme": null,
          "cs": "Inherit",
          "double_strike": "Inherit",
          "emboss": "Inherit",
          "emphasis_mark": null,
          "fit_text": null,
          "font_cs": null,
          "font_cs_theme": null,
          "font_east_asia": null,
          "font_east_asia_theme": null,
          "font_family": null,
          "font_family_theme": null,
          "font_hint": null,
          "font_size": null,
          "font_size_cs": null,
          "highlight": null,
          "imprint": "Inherit",
          "italic_cs": "Inherit",
          "kern": null,
          "lang": null,
          "lang_east_asia": null,
          "no_proof": "Inherit",
          "o_math": "Inherit",
          "outline": "Inherit",
          "position": null,
          "preserved": [],
          "rtl": "Inherit",
          "run_border": null,
          "run_shading": null,
          "shadow": "Inherit",
          "small_caps": "Inherit",
          "snap_to_grid": "Inherit",
          "spec_vanish": "Inherit",
          "strike": "Inherit",
          "text_effect": null,
          "underline_style": null,
          "vanish": "Inherit",
          "web_hidden": "Inherit"
        },
        "text": "twice the fees paid"
      }
    },
    {
      "Unchanged": {
        "formatting_change": null,
        "marks": [],
        "style_props": {
          "bold_cs": "Inherit",
          "caps": "Inherit",
          "char_spacing": null,
          "char_style_id": null,
          "char_width_scaling": null,
          "color": null,
          "color_theme": null,
          "cs": "Inherit",
          "double_strike": "Inherit",
          "emboss": "Inherit",
          "emphasis_mark": null,
          "fit_text": null,
          "font_cs": null,
          "font_cs_theme": null,
          "font_east_asia": null,
          "font_east_asia_theme": null,
          "font_family": null,
          "font_family_theme": null,
          "font_hint": null,
          "font_size": null,
          "font_size_cs": null,
          "highlight": null,
          "imprint": "Inherit",
          "italic_cs": "Inherit",
          "kern": null,
          "lang": null,
          "lang_east_asia": null,
          "no_proof": "Inherit",
          "o_math": "Inherit",
          "outline": "Inherit",
          "position": null,
          "preserved": [],
          "rtl": "Inherit",
          "run_border": null,
          "run_shading": null,
          "shadow": "Inherit",
          "small_caps": "Inherit",
          "snap_to_grid": "Inherit",
          "spec_vanish": "Inherit",
          "strike": "Inherit",
          "text_effect": null,
          "underline_style": null,
          "vanish": "Inherit",
          "web_hidden": "Inherit"
        },
        "text": "."
      }
    }
  ],
  "spacing": null,
  "structural_change": null,
  "style_id": null,
  "tab_stops": [],
  "table_diff": null
}
```

### The exemplar's section properties

```json
{
  "bidi": null,
  "column_defs": [],
  "column_separator": null,
  "column_space": null,
  "columns": null,
  "doc_grid_char_space": null,
  "doc_grid_line_pitch": null,
  "doc_grid_type": null,
  "endnote_pr": null,
  "equal_width": null,
  "footer_distance": 708,
  "footer_refs": [
    {
      "kind": "Default",
      "part_path": "synthesized-blank-footer-default.xml",
      "synthesized": true
    }
  ],
  "footnote_pr": null,
  "form_prot": null,
  "gutter": 0,
  "header_distance": 708,
  "header_refs": [
    {
      "kind": "Default",
      "part_path": "synthesized-blank-header-default.xml",
      "synthesized": true
    }
  ],
  "line_numbering": null,
  "margin_bottom": 1440,
  "margin_left": 1440,
  "margin_right": 1440,
  "margin_top": 1440,
  "no_endnote": null,
  "orientation": null,
  "page_borders": null,
  "page_height": 16838,
  "page_number_type": null,
  "page_width": 11906,
  "paper_size_code": null,
  "paper_source": null,
  "printer_settings_rid": null,
  "rtl_gutter": null,
  "section_type": null,
  "text_direction": null,
  "title_page": null,
  "v_align": null
}
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
`style_props.preserved` as `{"name", "raw_xml"}` pairs, verbatim from the
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

| Field | Meaning |
|---|---|
| `revision_id` | Engine-minted identity; 0 marks a reported-but-never-selectable record. |
| `wire_id` | The raw OOXML `w:id`, diagnostics only, never an address. |
| `author` | Author, when the source carried one. |
| `date` | ISO-8601 date, when carried. |
| `kind` | One of the [revision kinds](#revision-kinds). |
| `block_id` | The block the revision lives in (`body_section` for a body-level section change). |
| `location` | Story scope: body, header or footer, footnote, endnote, or comment. |
| `excerpt` | Visible text of the change, or a descriptor such as "formatting". |

### Revision kinds

`insert`, `delete`, `format_run`, `format_paragraph`, `format_table`, `format_row`, `format_cell`, `format_section`, `opaque_interior`, `move`

Transports serve projections of these records (HTTP `GET /revisions` rows
carry `revision_id`, `author`, `kind`, `block_id`, `excerpt`, `date`, and
omit records with `revision_id` 0, which are reported but never selectable).

## Related

* [Stability and compatibility](../guide/stability.md): the tier vocabulary this page is bound by.
* [v4 operation reference](operations.md): the write half, generated the same way.
* [Embed the engine](embedding.md): the facade and session runtime these views are read through.
* [HTTP API reference](http.md): the demo transport's wire for both views.
* [MCP advanced reference](mcp-advanced.md): the tool surface over the same model.
