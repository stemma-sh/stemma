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

`allow_existing_author` is NOT a transaction field: continuing a Word reviewer
group whose label was already present in the document is a per-call assertion
made on the transport (the `allow_existing_author` tool argument over MCP, the
`?allow_existing_author=true` query parameter on HTTP `/apply`), never part
of the durable edit format. See the
[AuthorLabelCollision refusal](mcp-advanced.md#refusal-vocabulary).

## Content nodes

The `content` field of `replace`, `insert`, `edit_header`, and `edit_footer`
takes the nodes below, each shown here inside a complete op.

A text node's `marks` is an ARRAY of marks (`[{"type":"bold"}]`; a plain
mark may also be written as its bare name, `["bold"]`). `set_format`'s `marks`
is a different shape: a tri-state patch (`{"bold":true,"italic":false}`)
where an omitted property is unchanged, with the value-carrying formatting
(color, font, size) in sibling fields.

### `paragraph` node

A body paragraph: `content` is an ordered inline list; `role` is optional.

```json
{"op":"replace","target":"<block_id>","expect":"<text currently in the block>","content":{"type":"paragraph","content":[{"type":"text","text":"<new text>"}]}}
```

### `text` node

An inline text run. `marks` is an ARRAY of marks: bold, italic, underline, strike, subscript, superscript (each as `{"type":"bold"}` or the bare name `"bold"`), and inline_role (`{"type":"inline_role","id":"<role>"}`). Mixing marked and unmarked runs is how a phrase inside a paragraph gets formatting.

```json
{"op":"replace","target":"<block_id>","expect":"<text currently in the block>","content":{"type":"paragraph","content":[{"type":"text","text":"The Supplier shall "},{"type":"text","text":"indemnify","marks":[{"type":"bold"}]},{"type":"text","text":" the Customer."}]}}
```

### `hyperlink` node

An inline hyperlink wrapping its own inline list. `attrs` takes `href` (external URL) and/or `anchor` (internal bookmark), plus optional `title`.

```json
{"op":"replace","target":"<block_id>","expect":"<text currently in the block>","content":{"type":"paragraph","content":[{"type":"text","text":"See "},{"type":"hyperlink","attrs":{"href":"https://example.com/terms"},"content":[{"type":"text","text":"the standard terms"}]}]}}
```

### `opaque_ref` node

A pointer to an EXISTING opaque inline node (image, field, footnote reference) by its stable id. Opaque content is never authored inline; a replace payload must reference every opaque node of the target block, in order. Comment references and comment range markers (`comment_<id>_start`, `comment_<id>_end`) may be left out: they are put back at their text position.

```json
{"op":"replace","target":"<block_id>","expect":"<text currently in the block>","content":{"type":"paragraph","content":[{"type":"text","text":"<new text before the image> "},{"type":"opaque_ref","attrs":{"id":"<opaque_id>"}}]}}
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

52 operations in 14 groups:

* [Content](#content): `replace`, `insert`, `delete`, `join_paragraphs`, `move`, `blocks_to_table`
* [Formatting and styles](#formatting-and-styles): `set_attr`, `set_format`, `set_para_format`, `apply_style`, `create_style`, `modify_style`, `set_doc_defaults`
* [Tables](#tables): `set_cell_format`, `set_row_format`, `table_op`, `set_table_format`
* [References](#references): `insert_cross_ref`, `insert_bookmark`, `rename_bookmark`, `remove_bookmark`, `unlink_hyperlink`
* [Numbering](#numbering): `set_numbering`
* [Images](#images): `set_image_attrs`, `delete_image`, `insert_image`, `replace_image`, `set_image_layout`
* [Comments](#comments): `comment_create`, `comment_reply`, `comment_resolve`, `comment_delete`
* [Notes](#notes): `insert_note`, `edit_note`, `delete_note`
* [Sections](#sections): `set_page_setup`, `set_section_type`, `insert_section_break`
* [Headers and footers](#headers-and-footers): `edit_header`, `edit_footer`, `create_header`, `create_footer`, `set_header_footer_mode`
* [Equations](#equations): `insert_equation`
* [Content controls](#content-controls): `wrap_content_control`, `set_content_control_value`, `sdt_text_fill`, `wrap_blocks_content_control`
* [Form fields](#form-fields): `set_form_field_value`, `set_block_form_text`
* [Textboxes and opaque content](#textboxes-and-opaque-content): `set_textbox_text`, `opaque_text_edit`

## Content

### `replace`

Tracked whole-paragraph, guarded span, or hyperlink display-text replacement. For a hyperlink, target its opaque id and use content.type=hyperlink; attrs.href/anchor assert the current destination, not a new one. Change the destination separately with set_attr. A link in a header or footer adds header_part or footer_part (the part find names under story_matches). A typed label ("1." written as text, not list numbering) is rewritten by a whole-paragraph replace whose content starts with the new label; an automatic list label changes with set_numbering.

Fields: `target`, `content`, `span`, `expect`, `guard`, `semantic_hash`, `header_part`, `footer_part`, `rationale`.

```json
{"op":"replace","target":"<block_id>","expect":"<text currently in the block>","content":{"type":"paragraph","content":[{"type":"text","text":"<new text>"}]}}
```

```json
{"op":"replace","target":"<hyperlink_id>","expect":"<current display text>","content":{"type":"hyperlink","attrs":{"href":"https://example.com/current"},"content":[{"type":"text","text":"<new display text>"}]}}
```

### `insert`

Insert paragraphs, tables, or a native table of contents. Paragraph attrs.format_from selects a template paragraph (its text supplies the formatting; anchors, fields and links in it are not copied): local text replacement inherits its mixed character formatting and paragraph/list properties. attrs.tab_stops overrides direct stops with [{position: signed twips, alignment: left|right|center|decimal|bar|start|end|num|clear, leader?: dot|heavy|hyphen|middleDot|none|underscore}]; positions must be unique and within -31680..31680. Omit to inherit the template; [] removes direct stops, while clear entries suppress inherited positions. Explicit marks add to inherited formatting; content that marks any of its text keeps the template's formatting on unchanged words, and its new text is the template's body text plus the marks given. Source objects, identity, and revision history are not copied. Role selects a representative, not an exact neighbor.

Fields: `target`, `content`, `rationale`.

```json
{"op":"insert","target":{"anchor":"<block_id>","position":"after"},"content":[{"type":"paragraph","attrs":{"format_from":"<paragraph_id>"},"content":[{"type":"text","text":"<new text>"}]}]}
```

```json
{"op":"insert","target":{"anchor":"<block_id>","position":"after"},"content":[{"type":"paragraph","attrs":{"format_from":"<paragraph_id>"},"content":[{"type":"text","text":"<new text>"},{"type":"hyperlink","attrs":{"href":"<new_url>"},"content":[{"type":"text","text":"<new display text>"}]}]}]}
```

```json
{"op":"insert","target":{"anchor":"<block_id>","position":"after"},"content":[{"type":"toc"}]}
```

```json
{"op":"insert","target":{"anchor":"<block_id>","position":"after"},"content":[{"type":"table","attrs":{"width":{"w":5000,"width_type":"pct"},"borders":{"top":{"style":"single","size":4,"color":"000000"},"bottom":{"style":"single","size":4,"color":"000000"},"left":{"style":"single","size":4,"color":"000000"},"right":{"style":"single","size":4,"color":"000000"},"inside_h":{"style":"single","size":4,"color":"000000"},"inside_v":{"style":"single","size":4,"color":"000000"}}},"content":[{"attrs":{"cant_split":true},"content":[{"content":[{"type":"paragraph","content":[{"type":"text","text":"Item","marks":[{"type":"bold"}]}]}]},{"content":[{"type":"paragraph","content":[{"type":"text","text":"Amount","marks":[{"type":"bold"}]}]}]}]},{"attrs":{"cant_split":true},"content":[{"content":[{"type":"paragraph","content":[{"type":"text","text":"Total"}]}]},{"content":[{"type":"paragraph","content":[{"type":"text","text":"100"}]}]}]}]}]}
```

### `delete`

Track deletion of one block. For a paragraph in a header or footer, add header_part or footer_part; the story keeps its last paragraph.

Fields: `target`, `expect`, `guard`, `semantic_hash`, `header_part`, `footer_part`, `rationale`.

```json
{"op":"delete","target":"<block_id>","expect":"<text currently in the block>"}
```

### `join_paragraphs`

Join a paragraph with the paragraph right after it (with: that paragraph's id), as Delete at the end of the first does in Word: tracks the deletion of the paragraph mark between them. The joined paragraph takes the first paragraph's paragraph formatting (the tracked change records the second's, so Reject restores both) and each part keeps its own character formatting; mind the space at the seam (the first text's end is kept as it is).

Fields: `target`, `with`, `expect`, `guard`, `semantic_hash`, `rationale`.

```json
{"op":"join_paragraphs","target":"<block_id>","with":"<block_id>","expect":"<text currently in the block>"}
```

### `move`

Track relocation of one top-level block or one contiguous range of them in a single op; a paragraph in a table cell is refused (move its row with move_row).

Fields: `target`, `destination`, `expect`, `guard`, `semantic_hash`, `rationale`.

```json
{"op":"move","target":"<block_id>","destination":{"anchor":"<block_id>","position":"after"}}
```

```json
{"op":"move","target":{"from":"<block_id>","to":"<block_id>"},"destination":{"anchor":"<block_id>","position":"after"}}
```

### `blocks_to_table`

Convert a contiguous paragraph range into a tracked table. Each cell keeps its source text, run formatting, line breaks and footnote/endnote references.

Fields: `from`, `to`, `delimiter`, `header`, `rationale`.

```json
{"op":"blocks_to_table","from":"<block_id>","to":"<block_id>","delimiter":" | ","header":["Feature","Notes"]}
```

## Formatting and styles

### `set_attr`

Change a hyperlink's target or other kind-specific attributes; expect_href guards an href retarget. A link's visible text is changed with replace on the hyperlink id (content.type=hyperlink). A link in a header or footer adds header_part or footer_part.

Fields: `target`, `attrs`, `expect_href`, `expect_anchor`, `header_part`, `footer_part`, `rationale`.

```json
{"op":"set_attr","target":"<hyperlink_id>","attrs":{"href":"<new_url>"},"expect_href":"<current_url>"}
```

### `set_format`

Tracked character formatting on a guarded text span; marks is a tri-state object where an omitted property is unchanged, true turns it on and false turns it off, and the value-carrying fields (color, font, size) ride alongside it. The expect text may run across bookmark, comment-range and proofing marks, not across a link, field or other inline object (the refusal names it).

Fields: `target`, `expect`, `semantic_hash`, `marks`, `color`, `highlight`, `font_family`, `font_size_half_points`, `caps`, `small_caps`, `char_spacing`, `rationale`.

```json
{"op":"set_format","target":"<block_id>","expect":"<exact unique text>","marks":{"bold":true}}
```

### `set_para_format`

Tracked paragraph formatting: alignment, indentation, spacing, borders, shading, and pagination (keep_with_next, keep_lines_together, page_break_before).

Fields: `target`, `semantic_hash`, `align`, `indent`, `spacing`, `borders`, `shading`, `keep_with_next`, `keep_lines_together`, `page_break_before`, `rationale`.

```json
{"op":"set_para_format","target":"<block_id>","align":"center","spacing":{"after":120}}
```

```json
{"op":"set_para_format","target":"<block_id>","keep_with_next":true}
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

Structural table edits. Row and column indices are 0-based, as in a table read's cells (the first column is 0, including a label column). Read the table before a column op and give expect_header, the text of the column's top cell: a column op whose header does not match is refused with every column's index and header. move_row relocates an existing complete row using pre-operation row_index/ref_row and explicit before/after position. reorder_rows expresses a complete target-order permutation of source row indices in one operation; each source row must occur exactly once. move_column relocates one column (grid index col_index, before/after ref_col) with each cell's content, formatting and width, in direct mode; on a merged grid every cell must lie in the moving column, in the columns it crosses, outside both, or span both. swap_cells exchanges the contents of two cells (row/col as in cells), each keeping its cell formatting, in direct mode: use it to swap entries in some rows only. Both preserve complete content/formatting associations and require a clean source table. A vertically merged group moves only as a whole (a continuing row must stay directly below the row it continues); tables with table-level range boundaries are refused. insert_row carries cell text in the same op and inherits the reference row's height and header-row flag, and the reference cells' cell, paragraph, and leading text formatting; format_from_row names another row to copy instead (the new row is still placed by ref_row and position), e.g. a last row formatted like an earlier one. New rows do not inherit source editing-session IDs or tracked formatting history. Tracked delete_column on an explicit simple grid preserves surviving grid widths using fixed layout and reversible table/grid/cell geometry as one intention; Reject restores source layout and preferred widths. Row offsets, mismatched physical grids, and adjacent survivors narrower than 720 twips are refused (two pending slots require Word's 360-twip minimum). No whole-table replacement is substituted. merge_cells merges a rectangle as Word's Merge Cells does: the absorbed cells' text moves into the top-left cell, and the merge is not a tracked change (Word records none with Track Changes on). Down one column it works on a table with merged cells and extends or joins existing vertical merges; it never cuts one. insert_row does not join the new row to a merged group (neither does Word's Insert Row at the group's end): merge_cells down each merged column from the group's top row through the new row. Table reads give a merged cell its row_span / col_span.

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

```json
{"op":"table_op","target":"<table_id>","table_op":{"kind":"move_row","row_index":2,"ref_row":0,"position":"before"}}
```

```json
{"op":"table_op","target":"<table_id>","table_op":{"kind":"reorder_rows","row_order":[2,0,1]}}
```

```json
{"op":"table_op","target":"<table_id>","table_op":{"kind":"move_column","col_index":1,"expect_header":"<text of the column's top cell>","ref_col":0,"position":"before"}}
```

```json
{"op":"table_op","target":"<table_id>","table_op":{"kind":"swap_cells","row":0,"col":0,"with_row":0,"with_col":1}}
```

```json
{"op":"table_op","target":"<table_id>","table_op":{"kind":"merge_cells","start_row":2,"start_col":0,"end_row":4,"end_col":0}}
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

### `unlink_hyperlink`

Remove a hyperlink, retaining its plain-text runs and formatting. Target the hyperlink opaque ID and supply its exact display text as expect. Tracks complete link deletion plus ordinary-text insertion. Pending target revisions and complex run payload refuse; unrelated revisions remain untouched.

Fields: `target`, `expect`.

```json
{"op":"unlink_hyperlink","target":"<hyperlink_id>","expect":"<exact display text>"}
```

## Numbering

### `set_numbering`

Change a paragraph's auto-numbering: its level (indent / outdent: one level deeper or shallower in the same list, the label re-derived), its membership (remove from the list; split the list here; set_list puts it in a list by num_id and level), or, with kind format, the label format of this item and the following items of its list.

Fields: `target`, `change`, `semantic_hash`, `rationale`.

```json
{"op":"set_numbering","target":"<block_id>","change":{"kind":"indent"}}
```

```json
{"op":"set_numbering","target":"<block_id>","change":{"kind":"remove"}}
```

```json
{"op":"set_numbering","target":"<block_id>","change":{"kind":"split"}}
```

```json
{"op":"set_numbering","target":"<block_id>","change":{"kind":"format","num_fmt":"lowerLetter","lvl_text":"(%1)"}}
```

## Images

### `set_image_attrs`

Resize an existing drawing (DrawingML extent or a legacy VML shape's width/height) or change alt text; this is a direct property edit. For a drawing in a header or footer, add header_part or footer_part.

Fields: `target`, `drawing_id`, `semantic_hash`, `resize`, `alt_text`, `header_part`, `footer_part`, `rationale`.

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
{"op":"insert_image","target":"<block_id>","bytes_base64":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC","format":"png","alt_text":"<description>"}
```

### `replace_image`

Replace an existing drawing's media by drawing_id.

Fields: `target`, `drawing_id`, `bytes_base64`, `format`, `cx`, `cy`, `alt_text`, `allow_stretch`, `semantic_hash`, `rationale`.

```json
{"op":"replace_image","target":"<block_id>","drawing_id":"<drawing_id>","bytes_base64":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC","format":"png"}
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

Edit one note body by its note_id from the current notes index; for a note of several paragraphs give one line per paragraph, separated by 
, and each paragraph is edited in place.

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

Tracked text edit of one header story paragraph, addressed by part name and story-local id. expect names text in it and content what replaces it: plain replacement text for a part (a date beside page-number fields) changes just that part; content repeating the paragraph's other text, or keeping its objects as opaque_ref, rebuilds the whole paragraph. It reaches a paragraph inside a table in the header (a letterhead cell). find reports header matches under story_matches. A link in the header: replace or set_attr with header_part.

Fields: `header_part`, `target`, `expect`, `content`, `semantic_hash`, `rationale`.

```json
{"op":"edit_header","header_part":"header1.xml","target":"<block_id>","expect":"<substring currently in the header paragraph>","content":[{"type":"text","text":"<new header text>"}]}
```

### `edit_footer`

Tracked text edit of one footer story paragraph, addressed by part name and story-local id. expect names text in it and content what replaces it: plain replacement text for a part (a date beside page-number fields) changes just that part; content repeating the paragraph's other text, or keeping its objects as opaque_ref, rebuilds the whole paragraph. It reaches a paragraph inside a table in the footer. A link in the footer: replace or set_attr with footer_part.

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

Tracked whole-value fill of a content control: inline (block_id + sdt_id), open block-level (its first block_id), or sealed block-level (body_index).

Fields: `block_id`, `sdt_id`, `body_index`, `value`, `semantic_hash`, `rationale`.

```json
{"op":"sdt_text_fill","block_id":"<block_id>","sdt_id":"<sdt_id>","value":"<new value>"}
```

```json
{"op":"sdt_text_fill","block_id":"<block_id>","value":"<new value>"}
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

Fill a legacy form field (FORMTEXT, FORMCHECKBOX, FORMDROPDOWN): at most one of text, checked, selected; default sets what the field shows when reset (a string for a text field, a boolean for a checkbox), with or without a value.

Fields: `target`, `field_id`, `text`, `checked`, `selected`, `default`, `semantic_hash`, `rationale`.

```json
{"op":"set_form_field_value","target":"<block_id>","field_id":"<field_id>","text":"<new value>"}
```

```json
{"op":"set_form_field_value","target":"<block_id>","field_id":"<field_id>","text":"<new value>","default":"<new value>"}
```

### `set_block_form_text`

Update a FORMTEXT cached result inside an opaque body block using its hash-bound field address. Direct field-value update; preserves the stored default and other fields.

Fields: `body_index`, `paragraph_index`, `field_index`, `semantic_hash`, `text`.

```json
{"op":"set_block_form_text","body_index":0,"paragraph_index":1,"field_index":0,"semantic_hash":"<containing-block hash>","text":"<new value>"}
```

## Textboxes and opaque content

### `set_textbox_text`

Replace a textbox's interior paragraphs wholesale as plain paragraphs (its run formatting is not kept); direct edit, refuses an interior that already carries tracked changes. To change words in place, tracked and keeping their formatting, use opaque_text_edit. For a textbox in a header or footer, add header_part or footer_part.

Fields: `target`, `drawing_id`, `paragraphs`, `semantic_hash`, `header_part`, `footer_part`, `rationale`.

```json
{"op":"set_textbox_text","target":"<block_id>","drawing_id":"<drawing_id>","paragraphs":["<paragraph text, one entry per paragraph>"]}
```

### `opaque_text_edit`

Tracked find/replace inside a textbox or inline content control. For one in a header or footer, add header_part or footer_part (its part name).

Fields: `target`, `opaque_id`, `find`, `replacement`, `container_index`, `paragraph_index`, `semantic_hash`, `header_part`, `footer_part`, `rationale`.

```json
{"op":"opaque_text_edit","target":"<block_id>","opaque_id":"<opaque_id>","find":"<text currently inside>","replacement":"<new text>"}
```

```json
{"op":"opaque_text_edit","header_part":"header2.xml","target":"<block_id>","opaque_id":"<opaque_id>","find":"<text currently inside>","replacement":"<new text>"}
```

## Related

* [Stability and compatibility](../guide/stability.md): how this schema evolves.
* [Read model reference](read-model.md): the read half, what you render a document from.
* [MCP advanced reference](mcp-advanced.md): the transaction inside the tool surface.
* [HTTP API reference](http.md): `POST /api/documents/{id}/apply` takes one of these transactions.
