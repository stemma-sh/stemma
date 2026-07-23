# v4 operation reference

<!-- GENERATED FILE. Do not edit by hand: this page is rendered from the
     engine's operation catalog (stemma-engine/src/edit_v4/catalog.rs) by
     stemma-engine/tests/operations_reference.rs, and that test fails the
     gate when the page drifts. Regenerate with:

         just regen-operations-reference
-->

Every operation a v4 edit transaction accepts, rendered from the engine's own
parser table so this page cannot disagree with what the parser enforces.
Deserialization is strict per the
[stability contract](../guide/stability.md#v4-transaction-json-additive-unknown-fields-rejected):
an unknown field or an unknown `op` tag is a hard error, never a silent no-op,
so author against exactly the fields listed here.

The same catalog is served at runtime by every transport: over MCP via
`inspect_docx` with `query:"operations"` (which also lists that transport's
edge-only image `path` fields), and over HTTP at `GET /api/operations` (see
the [HTTP API reference](http.md#endpoints)). Placeholders like `<block_id>`
in the shapes below are yours to fill; each shape is otherwise schema-valid
verbatim, pinned by an engine test.

## Transaction envelope

A transaction is atomic: every op applies or none do. Targets are block ids
from the current document; `expect`, `guard`, or `semantic_hash` provides
optimistic concurrency where an op supports it.

```json
{"ops":[{"op":"..."}],"revision":{"author":"J. Osei"},"summary":"optional"}
```

| Field | Meaning |
|---|---|
| `ops` | Non-empty ordered operation array; each op is tagged by its snake_case `op` field. |
| `revision.author` | Required author stamped on every tracked change the transaction produces. |
| `revision.date` | Optional ISO-8601 timestamp. |
| `revision.apply_op_id` | Optional group id stamped on every change. |
| `summary` | Optional human-readable description. |
| `materialization_mode` | `tracked_change` (the default) or `direct`. |

`allow_existing_author` is NOT a transaction field: continuing an author who
already owns revisions in the document is a per-call assertion made on the
transport (the `allow_existing_author` tool argument over MCP, the
`?allow_existing_author=true` query parameter on HTTP `/apply`), never part
of the durable edit format. See the
[AuthorImpersonation refusal](mcp-advanced.md#refusal-vocabulary).

## Content nodes

The `content` field of `replace`, `insert`, `edit_header`, and `edit_footer`
takes the nodes below, each shown here inside a complete op.

One `marks` vocabulary is used everywhere: an ARRAY of tagged objects
(`[{"type":"bold"}]`), never bare strings. A text node's `marks` authors
inline content; `set_format`'s `marks` replaces a span's mark set, with the
value-carrying formatting (color, font, size) in sibling fields.

### `paragraph` node

A body paragraph: `content` is an ordered inline list; `role` is optional.

```json
{"op":"replace","target":"<block_id>","content":{"type":"paragraph","content":[{"type":"text","text":"<new text>"}]}}
```

### `text` node

An inline text run. `marks` is an ARRAY of tagged objects; mark types are bold, italic, underline, strike, subscript, superscript, and inline_role (which carries an `id`). Mixing marked and unmarked runs is how a phrase inside a paragraph gets formatting.

```json
{"op":"replace","target":"<block_id>","content":{"type":"paragraph","content":[{"type":"text","text":"The Supplier shall "},{"type":"text","text":"indemnify","marks":[{"type":"bold"}]},{"type":"text","text":" the Customer."}]}}
```

### `hyperlink` node

An inline hyperlink wrapping its own inline list. `attrs` takes `href` (external URL) and/or `anchor` (internal bookmark), plus optional `title`.

```json
{"op":"replace","target":"<block_id>","content":{"type":"paragraph","content":[{"type":"text","text":"See "},{"type":"hyperlink","attrs":{"href":"https://example.com/terms"},"content":[{"type":"text","text":"the standard terms"}]}]}}
```

### `opaque_ref` node

A pointer to an EXISTING opaque inline node (image, field, footnote reference) by its stable id. Opaque content is never authored inline; a replace payload must carry the same opaque id set as the target block.

```json
{"op":"replace","target":"<block_id>","content":{"type":"paragraph","content":[{"type":"text","text":"<new text before the image> "},{"type":"opaque_ref","attrs":{"id":"<opaque_id>"}}]}}
```

### `table` node

A table block: rows contain cells, and each cell's `content` is a block list, so cells nest paragraphs and further tables.

```json
{"op":"insert","target":{"anchor":"<block_id>","position":"after"},"content":[{"type":"table","content":[{"content":[{"content":[{"type":"paragraph","content":[{"type":"text","text":"<cell 1>"}]}]},{"content":[{"type":"paragraph","content":[{"type":"text","text":"<cell 2>"}]}]}]}]}]}
```

### `toc` node

A native table-of-contents field. Insert-only; `levels` defaults to headings 1 to 3 when omitted.

```json
{"op":"insert","target":{"anchor":"<block_id>","position":"after"},"content":[{"type":"toc","levels":{"from":1,"to":2}}]}
```

## Catalog

49 operations in 14 groups:

* [Content](#content): `replace`, `insert`, `delete`, `move`, `blocks_to_table`
* [Formatting and styles](#formatting-and-styles): `set_attr`, `set_format`, `set_para_format`, `apply_style`, `create_style`, `modify_style`, `set_doc_defaults`
* [Tables](#tables): `set_cell_format`, `set_row_format`, `table_op`, `set_table_format`
* [References](#references): `insert_cross_ref`, `insert_bookmark`, `rename_bookmark`, `remove_bookmark`
* [Numbering](#numbering): `set_numbering`
* [Images](#images): `set_image_attrs`, `delete_image`, `insert_image`, `replace_image`, `set_image_layout`
* [Comments](#comments): `comment_create`, `comment_reply`, `comment_resolve`, `comment_delete`
* [Notes](#notes): `insert_note`, `edit_note`, `delete_note`
* [Sections](#sections): `set_page_setup`, `set_section_type`, `insert_section_break`
* [Headers and footers](#headers-and-footers): `edit_header`, `edit_footer`, `create_header`, `create_footer`, `set_header_footer_mode`
* [Equations](#equations): `insert_equation`
* [Content controls](#content-controls): `wrap_content_control`, `set_content_control_value`, `sdt_text_fill`, `wrap_blocks_content_control`
* [Form fields](#form-fields): `set_form_field_value`
* [Textboxes and opaque content](#textboxes-and-opaque-content): `set_textbox_text`, `opaque_text_edit`

## Content

### `replace`

Tracked whole-paragraph or guarded span replacement.

Fields: `target`, `content`, `span`, `expect`, `guard`, `semantic_hash`, `rationale`.

```json
{"op":"replace","target":"<block_id>","content":{"type":"paragraph","content":[{"type":"text","text":"<new text>"}]}}
```

### `insert`

Insert paragraphs, tables, or a native table of contents at an anchor.

Fields: `target`, `content`, `rationale`.

```json
{"op":"insert","target":{"anchor":"<block_id>","position":"after"},"content":[{"type":"paragraph","role":"body_text","content":[{"type":"text","text":"<new text>"}]}]}
```

```json
{"op":"insert","target":{"anchor":"<block_id>","position":"after"},"content":[{"type":"toc"}]}
```

### `delete`

Track deletion of one block.

Fields: `target`, `expect`, `guard`, `semantic_hash`, `rationale`.

```json
{"op":"delete","target":"<block_id>"}
```

### `move`

Track relocation of one block or one contiguous range in a single op.

Fields: `target`, `destination`, `expect`, `guard`, `semantic_hash`, `rationale`.

```json
{"op":"move","target":"<block_id>","destination":{"anchor":"<block_id>","position":"after"}}
```

```json
{"op":"move","target":{"from":"<block_id>","to":"<block_id>"},"destination":{"anchor":"<block_id>","position":"after"}}
```

### `blocks_to_table`

Convert a contiguous paragraph range into a tracked table.

Fields: `from`, `to`, `delimiter`, `header`, `rationale`.

```json
{"op":"blocks_to_table","from":"<block_id>","to":"<block_id>","delimiter":" | ","header":["Feature","Notes"]}
```

## Formatting and styles

### `set_attr`

Change a hyperlink's target or other kind-specific attributes; expect_href guards an href retarget.

Fields: `target`, `attrs`, `expect_href`, `expect_anchor`, `rationale`.

```json
{"op":"set_attr","target":"<hyperlink_id>","attrs":{"href":"<new_url>"},"expect_href":"<current_url>"}
```

### `set_format`

Tracked character formatting on a guarded text span; marks is the complete replacement mark set as an array of tagged objects, and the value-carrying fields (color, font, size) ride alongside it.

Fields: `target`, `expect`, `semantic_hash`, `marks`, `color`, `highlight`, `font_family`, `font_size_half_points`, `caps`, `small_caps`, `char_spacing`, `rationale`.

```json
{"op":"set_format","target":"<block_id>","expect":"<exact unique text>","marks":[{"type":"bold"}]}
```

### `set_para_format`

Tracked paragraph formatting: alignment, indentation, spacing, borders, shading.

Fields: `target`, `semantic_hash`, `align`, `indent`, `spacing`, `borders`, `shading`, `rationale`.

```json
{"op":"set_para_format","target":"<block_id>","align":"center","spacing":{"after":120}}
```

### `apply_style`

Apply a named paragraph style as a tracked property change.

Fields: `target`, `style_id`, `semantic_hash`, `rationale`.

```json
{"op":"apply_style","target":"<block_id>","style_id":"Heading1"}
```

### `create_style`

Create or modify one named Word style.

Fields: `style_id`, `style_type`, `based_on`, `name`, `run_props`, `para_props`, `rationale`.

```json
{"op":"create_style","style_id":"Heading1","style_type":"para","name":"Heading 1","run_props":{"font_family":"Georgia","font_size_half_points":32,"bold":true},"para_props":{"spacing_before":240,"spacing_after":120}}
```

### `modify_style`

Create or modify one named Word style.

Fields: `style_id`, `style_type`, `based_on`, `name`, `run_props`, `para_props`, `rationale`.

```json
{"op":"modify_style","style_id":"Normal","style_type":"para","name":"Normal","run_props":{"font_family":"Georgia","font_size_half_points":24},"para_props":{}}
```

### `set_doc_defaults`

Change inherited document-default font settings once.

Fields: `font_family`, `font_size_half_points`, `rationale`.

```json
{"op":"set_doc_defaults","font_family":"Georgia","font_size_half_points":24}
```

## Tables

### `set_cell_format`

Tracked one-cell formatting by row/col index: borders, shading, width, vertical alignment, margins.

Fields: `target`, `semantic_hash`, `row_index`, `col_index`, `borders`, `shading`, `width`, `v_align`, `margins`, `rationale`.

```json
{"op":"set_cell_format","target":"<table_id>","row_index":0,"col_index":0,"shading":{"fill":"D9EAF7"}}
```

### `set_row_format`

Tracked row height by row index (twips, with a height rule).

Fields: `target`, `semantic_hash`, `row_index`, `height`, `height_rule`, `rationale`.

```json
{"op":"set_row_format","target":"<table_id>","row_index":0,"height":360,"height_rule":"exact"}
```

### `table_op`

Structural table edits; insert_row carries cell text in the same op.

Fields: `target`, `semantic_hash`, `table_op`, `rationale`.

```json
{"op":"table_op","target":"<table_id>","table_op":{"kind":"set_cell_text","row_index":0,"col_index":0,"text":"<new text>"}}
```

```json
{"op":"table_op","target":"<table_id>","table_op":{"kind":"insert_row","ref_row":0,"position":"after","cells":["<row content, one per column, left-to-right>"]}}
```

```json
{"op":"table_op","target":"<table_id>","table_op":{"kind":"delete_row","row_index":0}}
```

### `set_table_format`

Tracked table-level borders, width, or default cell margins, in place; cell shading belongs to set_cell_format.

Fields: `target`, `semantic_hash`, `borders`, `width`, `default_cell_margins`, `rationale`.

```json
{"op":"set_table_format","target":"<table_id>","width":{"w":5000,"width_type":"pct"}}
```

## References

### `insert_cross_ref`

Insert a field-backed cross-reference to an existing bookmark after anchor text.

Fields: `target`, `expect`, `semantic_hash`, `bookmark`, `ref_kind`, `as_hyperlink`, `no_paragraph_number`, `paragraph_number_relative`, `paragraph_number_full`, `above_below`, `rationale`.

```json
{"op":"insert_cross_ref","target":"<block_id>","expect":"<anchor text>","bookmark":"bookmark_name","ref_kind":"ref","as_hyperlink":true}
```

### `insert_bookmark`

Wrap anchor text in a named bookmark.

Fields: `target`, `expect`, `name`, `semantic_hash`, `rationale`.

```json
{"op":"insert_bookmark","target":"<block_id>","expect":"<anchor text>","name":"bookmark_name"}
```

### `rename_bookmark`

Rename an existing bookmark, addressed by its current name.

Fields: `target`, `old_name`, `new_name`, `semantic_hash`, `rationale`.

```json
{"op":"rename_bookmark","target":"<block_id>","old_name":"current_bookmark_name","new_name":"new_bookmark_name"}
```

### `remove_bookmark`

Remove a bookmark's start and end markers; the text stays.

Fields: `target`, `name`, `semantic_hash`, `rationale`.

```json
{"op":"remove_bookmark","target":"<block_id>","name":"bookmark_name"}
```

## Numbering

### `set_numbering`

Change a paragraph's auto-numbering membership (remove, split, join).

Fields: `target`, `change`, `semantic_hash`, `rationale`.

```json
{"op":"set_numbering","target":"<block_id>","change":{"kind":"remove"}}
```

```json
{"op":"set_numbering","target":"<block_id>","change":{"kind":"split"}}
```

## Images

### `set_image_attrs`

Resize an existing drawing or change alt text; this is a direct property edit.

Fields: `target`, `drawing_id`, `semantic_hash`, `resize`, `alt_text`, `rationale`.

```json
{"op":"set_image_attrs","target":"<block_id>","drawing_id":"<drawing_id>","resize":{"cx":4320000,"cy":2880000}}
```

### `delete_image`

Track deletion of one drawing by drawing_id; the media part is never touched.

Fields: `target`, `drawing_id`, `semantic_hash`, `rationale`.

```json
{"op":"delete_image","target":"<block_id>","drawing_id":"<drawing_id>"}
```

### `insert_image`

Insert image bytes/path at a paragraph; dimensions may use intrinsic size.

Fields: `target`, `bytes_base64`, `format`, `cx`, `cy`, `alt_text`, `expect`, `semantic_hash`, `rationale`.

```json
{"op":"insert_image","target":"<block_id>","bytes_base64":"AAAA","format":"png","alt_text":"<description>"}
```

### `replace_image`

Replace an existing drawing's media by drawing_id.

Fields: `target`, `drawing_id`, `bytes_base64`, `format`, `cx`, `cy`, `alt_text`, `allow_stretch`, `semantic_hash`, `rationale`.

```json
{"op":"replace_image","target":"<block_id>","drawing_id":"<drawing_id>","bytes_base64":"AAAA","format":"png"}
```

### `set_image_layout`

Crop, float position, or text wrap on an existing drawing; direct property edit, position and wrap require an anchored drawing.

Fields: `target`, `drawing_id`, `semantic_hash`, `position_h`, `position_v`, `wrap`, `crop`, `rationale`.

```json
{"op":"set_image_layout","target":"<block_id>","drawing_id":"<drawing_id>","wrap":"square","position_h":{"relative_from":"margin","align":"center"}}
```

```json
{"op":"set_image_layout","target":"<block_id>","drawing_id":"<drawing_id>","crop":{"left":10000,"right":10000}}
```

## Comments

### `comment_create`

Create an anchored comment; comments are annotations, not revisions.

Fields: `target`, `expect`, `body`, `author`, `semantic_hash`, `rationale`.

```json
{"op":"comment_create","target":"<block_id>","expect":"<anchor text>","body":"<comment text>","author":"Reviewer"}
```

### `comment_reply`

Reply to an existing comment thread by its parent comment id.

Fields: `parent_comment_id`, `body`, `author`, `rationale`.

```json
{"op":"comment_reply","parent_comment_id":"<comment_id>","body":"<reply text>","author":"Reviewer"}
```

### `comment_resolve`

Mark a comment thread resolved or unresolved (w15:done).

Fields: `comment_id`, `done`, `rationale`.

```json
{"op":"comment_resolve","comment_id":"<comment_id>","done":true}
```

### `comment_delete`

Delete a comment and its anchor range markers.

Fields: `comment_id`, `rationale`.

```json
{"op":"comment_delete","comment_id":"<comment_id>"}
```

## Notes

### `insert_note`

Insert a footnote/endnote reference and its story body.

Fields: `target`, `expect`, `note_kind`, `body`, `semantic_hash`, `rationale`.

```json
{"op":"insert_note","target":"<block_id>","expect":"<substring currently in the block>","note_kind":"footnote","body":"<note body text>"}
```

### `edit_note`

Edit one note body by its note_id from the current notes index.

Fields: `note_id`, `note_kind`, `body`, `rationale`.

```json
{"op":"edit_note","note_id":"<note_id>","note_kind":"footnote","body":"<new note body text>"}
```

### `delete_note`

Delete one note and its body-side reference by note_id.

Fields: `note_id`, `note_kind`, `rationale`.

```json
{"op":"delete_note","note_id":"<note_id>","note_kind":"footnote"}
```

## Sections

### `set_page_setup`

Change page geometry for the body or a paragraph's section.

Fields: `target`, `page_size`, `orientation`, `margins`, `columns`, `gutter`, `semantic_hash`, `rationale`.

```json
{"op":"set_page_setup","target":{"section":"body"},"margins":{"top":1440,"bottom":1440,"left":1440,"right":1440,"header":720,"footer":720}}
```

### `set_section_type`

Set a section's break type token; an unknown token is refused, never defaulted.

Fields: `target`, `section_type`, `semantic_hash`, `rationale`.

```json
{"op":"set_section_type","target":{"section":"body"},"section_type":"continuous"}
```

### `insert_section_break`

Insert a mid-document section break with optional geometry overrides.

Fields: `anchor`, `section_type`, `page_size`, `orientation`, `margins`, `columns`, `gutter`, `rationale`.

```json
{"op":"insert_section_break","anchor":"<block_id>","section_type":"next_page","orientation":"landscape"}
```

## Headers and footers

### `edit_header`

Tracked text edit of one header story paragraph, addressed by part name and story-local id.

Fields: `header_part`, `target`, `expect`, `content`, `semantic_hash`, `rationale`.

```json
{"op":"edit_header","header_part":"header1.xml","target":"<block_id>","expect":"<substring currently in the header paragraph>","content":[{"type":"text","text":"<new header text>"}]}
```

### `edit_footer`

Tracked text edit of one footer story paragraph, addressed by part name and story-local id.

Fields: `footer_part`, `target`, `expect`, `content`, `semantic_hash`, `rationale`.

```json
{"op":"edit_footer","footer_part":"footer1.xml","target":"<block_id>","expect":"<substring currently in the footer paragraph>","content":[{"type":"text","text":"<new footer text>"}]}
```

### `create_header`

Author a new blank header story of the given kind and bind it to the body section.

Fields: `kind`, `rationale`.

```json
{"op":"create_header","kind":"default"}
```

### `create_footer`

Author a new blank footer story of the given kind and bind it to the body section.

Fields: `kind`, `rationale`.

```json
{"op":"create_footer","kind":"default"}
```

### `set_header_footer_mode`

Toggle title-page or even/odd header modes, or link/unlink one header or footer reference.

Fields: `title_page`, `even_and_odd`, `link`, `rationale`.

```json
{"op":"set_header_footer_mode","title_page":true}
```

```json
{"op":"set_header_footer_mode","link":{"is_header":true,"kind":"default","link":false}}
```

## Equations

### `insert_equation`

Insert a caller-supplied OMML fragment after anchor text, inline or block placement.

Fields: `target`, `expect`, `semantic_hash`, `omml`, `placement`, `rationale`.

```json
{"op":"insert_equation","target":"<block_id>","expect":"<anchor text>","omml":"<m:oMath fragment>","placement":"inline"}
```

## Content controls

### `wrap_content_control`

Wrap a text span in a typed inline content control (w:sdt), optionally with a data binding.

Fields: `target`, `expect`, `semantic_hash`, `tag`, `alias`, `control`, `data_binding`, `rationale`.

```json
{"op":"wrap_content_control","target":"<block_id>","expect":"<span text>","tag":"party_name","control":{"kind":"plain_text"}}
```

### `set_content_control_value`

Set an existing control's displayed value: exactly one of text, checked, selected.

Fields: `target`, `sdt_id`, `text`, `checked`, `selected`, `tracked`, `rationale`.

```json
{"op":"set_content_control_value","target":"<block_id>","sdt_id":"<sdt_id>","text":"<new value>"}
```

```json
{"op":"set_content_control_value","target":"<block_id>","sdt_id":"<sdt_id>","checked":true}
```

### `sdt_text_fill`

Tracked whole-value fill of a content control, inline (block_id + sdt_id) or block-level (body_index).

Fields: `block_id`, `sdt_id`, `body_index`, `value`, `semantic_hash`, `rationale`.

```json
{"op":"sdt_text_fill","block_id":"<block_id>","sdt_id":"<sdt_id>","value":"<new value>"}
```

```json
{"op":"sdt_text_fill","body_index":0,"value":"<new value>"}
```

### `wrap_blocks_content_control`

Wrap a contiguous top-level block range in a block-level content control.

Fields: `start_block`, `end_block`, `tag`, `alias`, `control`, `rationale`.

```json
{"op":"wrap_blocks_content_control","start_block":"<block_id>","end_block":"<block_id>","alias":"Schedule 1","control":{"kind":"rich_text"}}
```

## Form fields

### `set_form_field_value`

Fill a legacy form field (FORMTEXT, FORMCHECKBOX, FORMDROPDOWN): exactly one of text, checked, selected.

Fields: `target`, `field_id`, `text`, `checked`, `selected`, `semantic_hash`, `rationale`.

```json
{"op":"set_form_field_value","target":"<block_id>","field_id":"<field_id>","text":"<new value>"}
```

## Textboxes and opaque content

### `set_textbox_text`

Replace a textbox's interior paragraphs wholesale; direct edit, refuses an interior that already carries tracked changes.

Fields: `target`, `drawing_id`, `paragraphs`, `semantic_hash`, `rationale`.

```json
{"op":"set_textbox_text","target":"<block_id>","drawing_id":"<drawing_id>","paragraphs":["<paragraph text, one entry per paragraph>"]}
```

### `opaque_text_edit`

Tracked find/replace inside a textbox or inline content control.

Fields: `target`, `opaque_id`, `find`, `replacement`, `container_index`, `paragraph_index`, `semantic_hash`, `rationale`.

```json
{"op":"opaque_text_edit","target":"<block_id>","opaque_id":"<opaque_id>","find":"<text currently inside>","replacement":"<new text>"}
```

## Related

* [Stability and compatibility](../guide/stability.md): how this schema evolves.
* [Read model reference](read-model.md): the read half, what you render a document from.
* [MCP advanced reference](mcp-advanced.md): the transaction inside the tool surface.
* [HTTP API reference](http.md): `POST /api/documents/{id}/apply` takes one of these transactions.
