//! The v4 operation catalog: the engine-owned description of every
//! transaction operation, decorating the parser table with the teaching
//! material (group, cue, canonical shapes) that every transport projects.
//!
//! The parser's [`super::OP_FIELDS`] allowlist is the single source of truth
//! for which ops exist and which fields each accepts; this module decorates
//! that table and never restates it, so a catalog row cannot disagree with
//! what `parse_transaction` enforces. Consumers project this catalog rather
//! than owning op knowledge of their own: the MCP server (`inspect_docx`
//! `query="operations"` plus its schema-error shape hints), the HTTP demo
//! surface (`GET /api/operations`), and the generated reference page
//! (`docs/reference/operations.md`, rendered by
//! `stemma-engine/tests/operations_reference.rs`).

/// One operation's catalog row: the parser contract (`name`, `fields`) plus
/// engine-owned teaching material.
#[derive(Debug, Clone, Copy)]
pub struct OperationSpec {
    /// The wire `op` tag.
    pub name: &'static str,
    /// Every top-level key the parser accepts for this op, from the same
    /// table `parse_transaction` validates against.
    pub fields: &'static [&'static str],
    /// Coarse family for browsing (e.g. `"content"`, `"tables"`).
    pub group: &'static str,
    /// One-sentence purpose cue.
    pub cue: &'static str,
    /// Canonical JSON shape(s) of the op, placeholders `<...>` left for the
    /// caller to fill. EVERY shape is itself a parse-valid v4 op, pinned by
    /// `op_shapes_are_themselves_schema_valid` below: a surface that teaches
    /// an invalid shape is a new trap, not a fix. A new required field on any
    /// op makes its shape stale and fails that test by construction.
    pub examples: &'static [&'static str],
}

/// The full catalog, one row per parser op, in parser-table order.
pub fn operation_catalog() -> Vec<OperationSpec> {
    super::OP_FIELDS.iter().map(spec).collect()
}

/// One op's catalog row, or `None` for a name the parser does not accept.
pub fn operation_spec(name: &str) -> Option<OperationSpec> {
    super::OP_FIELDS
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(spec)
}

fn spec(entry: &(&'static str, &'static [&'static str])) -> OperationSpec {
    let (name, fields) = *entry;
    OperationSpec {
        name,
        fields,
        group: group_of(name),
        cue: cue_of(name),
        examples: shapes_of(name),
    }
}

fn group_of(op: &str) -> &'static str {
    match op {
        "replace" | "insert" | "delete" | "move" | "blocks_to_table" => "content",
        "set_attr" | "set_format" | "set_para_format" | "apply_style" | "create_style"
        | "modify_style" | "set_doc_defaults" => "formatting_and_styles",
        "set_cell_format" | "set_row_format" | "set_table_format" | "table_op" => "tables",
        "insert_cross_ref" | "insert_bookmark" | "rename_bookmark" | "remove_bookmark" => {
            "references"
        }
        "set_numbering" => "numbering",
        "set_image_attrs" | "delete_image" | "insert_image" | "replace_image"
        | "set_image_layout" => "images",
        "comment_create" | "comment_reply" | "comment_resolve" | "comment_delete" => "comments",
        "insert_note" | "edit_note" | "delete_note" => "notes",
        "set_page_setup" | "set_section_type" | "insert_section_break" => "sections",
        "edit_header"
        | "edit_footer"
        | "create_header"
        | "create_footer"
        | "set_header_footer_mode" => "headers_and_footers",
        "insert_equation" => "equations",
        "wrap_content_control"
        | "wrap_blocks_content_control"
        | "set_content_control_value"
        | "sdt_text_fill" => "content_controls",
        "set_form_field_value" => "form_fields",
        "set_textbox_text" | "opaque_text_edit" => "textboxes_and_opaque_content",
        // Reaching this arm means a new op was added to OP_FIELDS without a
        // group decision; `every_op_has_a_deliberate_group` fails on it.
        _ => "other",
    }
}

/// The generic cue a new op falls back to until it gets a deliberate one;
/// `every_op_has_a_deliberate_cue` fails on any op still carrying it.
pub const GENERIC_CUE: &str =
    "Typed transaction operation; use the advertised fields and preview before apply.";

fn cue_of(op: &str) -> &'static str {
    match op {
        "replace" => "Tracked whole-paragraph or guarded span replacement.",
        "insert" => "Insert paragraphs, tables, or a native table of contents at an anchor.",
        "delete" => "Track deletion of one block.",
        "move" => "Track relocation of one block or one contiguous range in a single op.",
        "set_attr" => {
            "Change a hyperlink's target or other kind-specific attributes; \
             expect_href guards an href retarget."
        }
        "set_format" => {
            "Tracked character formatting on a guarded text span; marks is the \
             complete replacement mark set as an array of tagged objects, and \
             the value-carrying fields (color, font, size) ride alongside it."
        }
        "set_para_format" => {
            "Tracked paragraph formatting: alignment, indentation, spacing, borders, shading."
        }
        "set_cell_format" => {
            "Tracked one-cell formatting by row/col index: borders, shading, \
             width, vertical alignment, margins."
        }
        "set_row_format" => "Tracked row height by row index (twips, with a height rule).",
        "set_table_format" => {
            "Tracked table-level borders, width, or default cell margins, in \
             place; cell shading belongs to set_cell_format."
        }
        "insert_cross_ref" => {
            "Insert a field-backed cross-reference to an existing bookmark after anchor text."
        }
        "set_numbering" => "Change a paragraph's auto-numbering membership (remove, split, join).",
        "insert_bookmark" => "Wrap anchor text in a named bookmark.",
        "rename_bookmark" => "Rename an existing bookmark, addressed by its current name.",
        "remove_bookmark" => "Remove a bookmark's start and end markers; the text stays.",
        "apply_style" => "Apply a named paragraph style as a tracked property change.",
        "set_image_attrs" => {
            "Resize an existing drawing or change alt text; this is a direct property edit."
        }
        "delete_image" => {
            "Track deletion of one drawing by drawing_id; the media part is never touched."
        }
        "insert_image" => {
            "Insert image bytes/path at a paragraph; dimensions may use intrinsic size."
        }
        "replace_image" => "Replace an existing drawing's media by drawing_id.",
        "set_image_layout" => {
            "Crop, float position, or text wrap on an existing drawing; direct \
             property edit, position and wrap require an anchored drawing."
        }
        "comment_create" => "Create an anchored comment; comments are annotations, not revisions.",
        "comment_reply" => "Reply to an existing comment thread by its parent comment id.",
        "comment_resolve" => "Mark a comment thread resolved or unresolved (w15:done).",
        "comment_delete" => "Delete a comment and its anchor range markers.",
        "insert_note" => "Insert a footnote/endnote reference and its story body.",
        "edit_note" => "Edit one note body by its note_id from the current notes index.",
        "delete_note" => "Delete one note and its body-side reference by note_id.",
        "set_page_setup" => "Change page geometry for the body or a paragraph's section.",
        "set_section_type" => {
            "Set a section's break type token; an unknown token is refused, never defaulted."
        }
        "insert_section_break" => {
            "Insert a mid-document section break with optional geometry overrides."
        }
        "edit_header" => {
            "Tracked text edit of one header story paragraph, addressed by part \
             name and story-local id."
        }
        "edit_footer" => {
            "Tracked text edit of one footer story paragraph, addressed by part \
             name and story-local id."
        }
        "create_header" => {
            "Author a new blank header story of the given kind and bind it to the body section."
        }
        "create_footer" => {
            "Author a new blank footer story of the given kind and bind it to the body section."
        }
        "set_header_footer_mode" => {
            "Toggle title-page or even/odd header modes, or link/unlink one \
             header or footer reference."
        }
        "insert_equation" => {
            "Insert a caller-supplied OMML fragment after anchor text, inline or block placement."
        }
        "blocks_to_table" => "Convert a contiguous paragraph range into a tracked table.",
        "wrap_content_control" => {
            "Wrap a text span in a typed inline content control (w:sdt), \
             optionally with a data binding."
        }
        "wrap_blocks_content_control" => {
            "Wrap a contiguous top-level block range in a block-level content control."
        }
        "set_content_control_value" => {
            "Set an existing control's displayed value: exactly one of text, checked, selected."
        }
        "set_form_field_value" => {
            "Fill a legacy form field (FORMTEXT, FORMCHECKBOX, FORMDROPDOWN): \
             exactly one of text, checked, selected."
        }
        "set_textbox_text" => {
            "Replace a textbox's interior paragraphs wholesale; direct edit, \
             refuses an interior that already carries tracked changes."
        }
        "opaque_text_edit" => "Tracked find/replace inside a textbox or inline content control.",
        "sdt_text_fill" => {
            "Tracked whole-value fill of a content control, inline (block_id + \
             sdt_id) or block-level (body_index)."
        }
        "create_style" | "modify_style" => "Create or modify one named Word style.",
        "set_doc_defaults" => "Change inherited document-default font settings once.",
        "table_op" => "Structural table edits; insert_row carries cell text in the same op.",
        _ => GENERIC_CUE,
    }
}

/// The canonical JSON shape(s) of one v4 op, for teaching a caller the exact
/// working form (the discovery tax: structural agent runs spent 3 to 8 calls
/// guessing `move` destinations and `set_image_attrs` fields). Most ops have
/// exactly one shape; `move` has two (single-block and range) because a
/// malformed move is ambiguous about which the caller meant, so teach both.
fn shapes_of(op: &str) -> &'static [&'static str] {
    match op {
        "replace" => &[
            r#"{"op":"replace","target":"<block_id>","content":{"type":"paragraph","content":[{"type":"text","text":"<new text>"}]}}"#,
        ],
        "insert" => &[
            r#"{"op":"insert","target":{"anchor":"<block_id>","position":"after"},"content":[{"type":"paragraph","role":"body_text","content":[{"type":"text","text":"<new text>"}]}]}"#,
            // A native table-of-contents field; `levels` is optional (default
            // 1-3). See the `insert` op documentation for the full contract.
            r#"{"op":"insert","target":{"anchor":"<block_id>","position":"after"},"content":[{"type":"toc"}]}"#,
        ],
        "delete" => &[r#"{"op":"delete","target":"<block_id>"}"#],
        // Single block, then the contiguous range form (moves several blocks,
        // a section, in one op; `from`/`to` in either doc order).
        "move" => &[
            r#"{"op":"move","target":"<block_id>","destination":{"anchor":"<block_id>","position":"after"}}"#,
            r#"{"op":"move","target":{"from":"<block_id>","to":"<block_id>"},"destination":{"anchor":"<block_id>","position":"after"}}"#,
        ],
        "set_image_attrs" => &[
            r#"{"op":"set_image_attrs","target":"<block_id>","drawing_id":"<drawing_id>","resize":{"cx":4320000,"cy":2880000}}"#,
        ],
        // Retarget a hyperlink: `expect_href` is REQUIRED whenever `attrs.href`
        // is set (optimistic concurrency; a stale retarget without it is
        // refused), so the shape shows it. Read the current href from any read
        // surface before retargeting.
        "set_attr" => &[
            r#"{"op":"set_attr","target":"<hyperlink_id>","attrs":{"href":"<new_url>"},"expect_href":"<current_url>"}"#,
        ],
        // In-place table edit. The inner op is tagged on `kind` (not `op`); the
        // common case is one cell's text. `insert_row`'s `cells` carries the
        // new row's content in the SAME op, no separate fill step needed; omit
        // `cells` (or give fewer than the column count) for a blank or
        // partly-blank row.
        "table_op" => &[
            r#"{"op":"table_op","target":"<table_id>","table_op":{"kind":"set_cell_text","row_index":0,"col_index":0,"text":"<new text>"}}"#,
            r#"{"op":"table_op","target":"<table_id>","table_op":{"kind":"insert_row","ref_row":0,"position":"after","cells":["<row content, one per column, left-to-right>"]}}"#,
            r#"{"op":"table_op","target":"<table_id>","table_op":{"kind":"delete_row","row_index":0}}"#,
        ],
        // Insert a footnote/endnote reference after `expect` in `target`, plus
        // its story body. `note_kind` is `"footnote"` | `"endnote"`.
        "insert_note" => &[
            r#"{"op":"insert_note","target":"<block_id>","expect":"<substring currently in the block>","note_kind":"footnote","body":"<note body text>"}"#,
        ],
        // Replace an existing note's body by its `note_id` from the current
        // notes index.
        "edit_note" => &[
            r#"{"op":"edit_note","note_id":"<note_id>","note_kind":"footnote","body":"<new note body text>"}"#,
        ],
        // Delete a note and its body-side reference run, by `note_id`.
        "delete_note" => &[r#"{"op":"delete_note","note_id":"<note_id>","note_kind":"footnote"}"#],
        "set_format" => &[
            r#"{"op":"set_format","target":"<block_id>","expect":"<exact unique text>","marks":[{"type":"bold"}]}"#,
        ],
        "set_para_format" => &[
            r#"{"op":"set_para_format","target":"<block_id>","align":"center","spacing":{"after":120}}"#,
        ],
        "set_cell_format" => &[
            r#"{"op":"set_cell_format","target":"<table_id>","row_index":0,"col_index":0,"shading":{"fill":"D9EAF7"}}"#,
        ],
        "set_row_format" => &[
            r#"{"op":"set_row_format","target":"<table_id>","row_index":0,"height":360,"height_rule":"exact"}"#,
        ],
        "set_table_format" => &[
            r#"{"op":"set_table_format","target":"<table_id>","width":{"w":5000,"width_type":"pct"}}"#,
        ],
        "apply_style" => &[r#"{"op":"apply_style","target":"<block_id>","style_id":"Heading1"}"#],
        "create_style" => &[
            r#"{"op":"create_style","style_id":"Heading1","style_type":"para","name":"Heading 1","run_props":{"font_family":"Georgia","font_size_half_points":32,"bold":true},"para_props":{"spacing_before":240,"spacing_after":120}}"#,
        ],
        "modify_style" => &[
            r#"{"op":"modify_style","style_id":"Normal","style_type":"para","name":"Normal","run_props":{"font_family":"Georgia","font_size_half_points":24},"para_props":{}}"#,
        ],
        "set_doc_defaults" => {
            &[r#"{"op":"set_doc_defaults","font_family":"Georgia","font_size_half_points":24}"#]
        }
        "set_page_setup" => &[
            r#"{"op":"set_page_setup","target":{"section":"body"},"margins":{"top":1440,"bottom":1440,"left":1440,"right":1440,"header":720,"footer":720}}"#,
        ],
        "set_numbering" => &[
            r#"{"op":"set_numbering","target":"<block_id>","change":{"kind":"remove"}}"#,
            r#"{"op":"set_numbering","target":"<block_id>","change":{"kind":"split"}}"#,
        ],
        "insert_bookmark" => &[
            r#"{"op":"insert_bookmark","target":"<block_id>","expect":"<anchor text>","name":"bookmark_name"}"#,
        ],
        "insert_cross_ref" => &[
            r#"{"op":"insert_cross_ref","target":"<block_id>","expect":"<anchor text>","bookmark":"bookmark_name","ref_kind":"ref","as_hyperlink":true}"#,
        ],
        "comment_create" => &[
            r#"{"op":"comment_create","target":"<block_id>","expect":"<anchor text>","body":"<comment text>","author":"Reviewer"}"#,
        ],
        "comment_reply" => &[
            r#"{"op":"comment_reply","parent_comment_id":"<comment_id>","body":"<reply text>","author":"Reviewer"}"#,
        ],
        "comment_resolve" => {
            &[r#"{"op":"comment_resolve","comment_id":"<comment_id>","done":true}"#]
        }
        "comment_delete" => &[r#"{"op":"comment_delete","comment_id":"<comment_id>"}"#],
        "insert_image" => &[
            r#"{"op":"insert_image","target":"<block_id>","bytes_base64":"AAAA","format":"png","alt_text":"<description>"}"#,
        ],
        "replace_image" => &[
            r#"{"op":"replace_image","target":"<block_id>","drawing_id":"<drawing_id>","bytes_base64":"AAAA","format":"png"}"#,
        ],
        "blocks_to_table" => &[
            r#"{"op":"blocks_to_table","from":"<block_id>","to":"<block_id>","delimiter":" | ","header":["Feature","Notes"]}"#,
        ],
        "rename_bookmark" => &[
            r#"{"op":"rename_bookmark","target":"<block_id>","old_name":"current_bookmark_name","new_name":"new_bookmark_name"}"#,
        ],
        "remove_bookmark" => {
            &[r#"{"op":"remove_bookmark","target":"<block_id>","name":"bookmark_name"}"#]
        }
        "delete_image" => {
            &[r#"{"op":"delete_image","target":"<block_id>","drawing_id":"<drawing_id>"}"#]
        }
        // Position/wrap forms require an anchored (floating) drawing; crop
        // works on inline drawings too, so teach both routes.
        "set_image_layout" => &[
            r#"{"op":"set_image_layout","target":"<block_id>","drawing_id":"<drawing_id>","wrap":"square","position_h":{"relative_from":"margin","align":"center"}}"#,
            r#"{"op":"set_image_layout","target":"<block_id>","drawing_id":"<drawing_id>","crop":{"left":10000,"right":10000}}"#,
        ],
        // Section target is `{"section":"body"}` or `{"paragraph":"<block_id>"}`;
        // the type token set is next_page | continuous | even_page | odd_page |
        // next_column.
        "set_section_type" => &[
            r#"{"op":"set_section_type","target":{"section":"body"},"section_type":"continuous"}"#,
        ],
        "insert_section_break" => &[
            r#"{"op":"insert_section_break","anchor":"<block_id>","section_type":"next_page","orientation":"landscape"}"#,
        ],
        // Header/footer stories are addressed by part name (from the read
        // surface) plus the story-local paragraph id; `content` is the same
        // inline list a paragraph replace takes.
        "edit_header" => &[
            r#"{"op":"edit_header","header_part":"header1.xml","target":"<block_id>","expect":"<substring currently in the header paragraph>","content":[{"type":"text","text":"<new header text>"}]}"#,
        ],
        "edit_footer" => &[
            r#"{"op":"edit_footer","footer_part":"footer1.xml","target":"<block_id>","expect":"<substring currently in the footer paragraph>","content":[{"type":"text","text":"<new footer text>"}]}"#,
        ],
        // `kind` is default | first | even.
        "create_header" => &[r#"{"op":"create_header","kind":"default"}"#],
        "create_footer" => &[r#"{"op":"create_footer","kind":"default"}"#],
        "set_header_footer_mode" => &[
            r#"{"op":"set_header_footer_mode","title_page":true}"#,
            r#"{"op":"set_header_footer_mode","link":{"is_header":true,"kind":"default","link":false}}"#,
        ],
        // `omml` is the raw math fragment (m:oMath for inline placement,
        // m:oMathPara for block); the engine validates non-emptiness, not the
        // fragment's XML, at the wire edge.
        "insert_equation" => &[
            r#"{"op":"insert_equation","target":"<block_id>","expect":"<anchor text>","omml":"<m:oMath fragment>","placement":"inline"}"#,
        ],
        // `control.kind` is plain_text | rich_text | dropdown | combo_box |
        // checkbox | date | repeating_section.
        "wrap_content_control" => &[
            r#"{"op":"wrap_content_control","target":"<block_id>","expect":"<span text>","tag":"party_name","control":{"kind":"plain_text"}}"#,
        ],
        "wrap_blocks_content_control" => &[
            r#"{"op":"wrap_blocks_content_control","start_block":"<block_id>","end_block":"<block_id>","alias":"Schedule 1","control":{"kind":"rich_text"}}"#,
        ],
        "set_content_control_value" => &[
            r#"{"op":"set_content_control_value","target":"<block_id>","sdt_id":"<sdt_id>","text":"<new value>"}"#,
            r#"{"op":"set_content_control_value","target":"<block_id>","sdt_id":"<sdt_id>","checked":true}"#,
        ],
        "set_form_field_value" => &[
            r#"{"op":"set_form_field_value","target":"<block_id>","field_id":"<field_id>","text":"<new value>"}"#,
        ],
        "set_textbox_text" => &[
            r#"{"op":"set_textbox_text","target":"<block_id>","drawing_id":"<drawing_id>","paragraphs":["<paragraph text, one entry per paragraph>"]}"#,
        ],
        "opaque_text_edit" => &[
            r#"{"op":"opaque_text_edit","target":"<block_id>","opaque_id":"<opaque_id>","find":"<text currently inside>","replacement":"<new text>"}"#,
        ],
        // Inline control (block_id + sdt_id) or block-level control (body_index).
        "sdt_text_fill" => &[
            r#"{"op":"sdt_text_fill","block_id":"<block_id>","sdt_id":"<sdt_id>","value":"<new value>"}"#,
            r#"{"op":"sdt_text_fill","body_index":0,"value":"<new value>"}"#,
        ],
        _ => &[],
    }
}

// ─── Content-node grammar ────────────────────────────────────────────────────

/// One content-grammar node's teaching row. The `content` field of `replace`,
/// `insert`, `edit_header`, and `edit_footer` takes these nodes; publishing
/// them here keeps the grammar next to the ops that consume it, on every
/// surface the catalog reaches.
#[derive(Debug, Clone, Copy)]
pub struct ContentNodeSpec {
    /// The wire `type` tag (block nodes) or the shape's name (inline nodes).
    pub name: &'static str,
    /// One-sentence purpose cue.
    pub cue: &'static str,
    /// Full parse-valid op(s) demonstrating the node in context, placeholders
    /// `<...>` left for the caller. Pinned parse-valid by the same test as the
    /// op shapes.
    pub examples: &'static [&'static str],
}

/// The content-node grammar: every node kind accepted inside an op's
/// `content`, each shown inside a complete op. One `marks` vocabulary is used
/// everywhere: an ARRAY of tagged objects (`[{"type":"bold"}]`), both on a
/// text node authoring inline content and on `set_format` replacing a span's
/// mark set (whose value-carrying formatting rides in sibling fields).
pub fn content_node_catalog() -> &'static [ContentNodeSpec] {
    &[
        ContentNodeSpec {
            name: "paragraph",
            cue: "A body paragraph: `content` is an ordered inline list; `role` is optional.",
            examples: &[
                r#"{"op":"replace","target":"<block_id>","content":{"type":"paragraph","content":[{"type":"text","text":"<new text>"}]}}"#,
            ],
        },
        ContentNodeSpec {
            name: "text",
            cue: "An inline text run. `marks` is an ARRAY of tagged objects; mark types are \
                  bold, italic, underline, strike, subscript, superscript, and inline_role \
                  (which carries an `id`). Mixing marked and unmarked runs is how a phrase \
                  inside a paragraph gets formatting.",
            examples: &[
                r#"{"op":"replace","target":"<block_id>","content":{"type":"paragraph","content":[{"type":"text","text":"The Supplier shall "},{"type":"text","text":"indemnify","marks":[{"type":"bold"}]},{"type":"text","text":" the Customer."}]}}"#,
            ],
        },
        ContentNodeSpec {
            name: "hyperlink",
            cue: "An inline hyperlink wrapping its own inline list. `attrs` takes `href` \
                  (external URL) and/or `anchor` (internal bookmark), plus optional `title`.",
            examples: &[
                r#"{"op":"replace","target":"<block_id>","content":{"type":"paragraph","content":[{"type":"text","text":"See "},{"type":"hyperlink","attrs":{"href":"https://example.com/terms"},"content":[{"type":"text","text":"the standard terms"}]}]}}"#,
            ],
        },
        ContentNodeSpec {
            name: "opaque_ref",
            cue: "A pointer to an EXISTING opaque inline node (image, field, footnote \
                  reference) by its stable id. Opaque content is never authored inline; a \
                  replace payload must carry the same opaque id set as the target block.",
            examples: &[
                r#"{"op":"replace","target":"<block_id>","content":{"type":"paragraph","content":[{"type":"text","text":"<new text before the image> "},{"type":"opaque_ref","attrs":{"id":"<opaque_id>"}}]}}"#,
            ],
        },
        ContentNodeSpec {
            name: "table",
            cue: "A table block: rows contain cells, and each cell's `content` is a block \
                  list, so cells nest paragraphs and further tables.",
            examples: &[
                r#"{"op":"insert","target":{"anchor":"<block_id>","position":"after"},"content":[{"type":"table","content":[{"content":[{"content":[{"type":"paragraph","content":[{"type":"text","text":"<cell 1>"}]}]},{"content":[{"type":"paragraph","content":[{"type":"text","text":"<cell 2>"}]}]}]}]}]}"#,
            ],
        },
        ContentNodeSpec {
            name: "toc",
            cue: "A native table-of-contents field. Insert-only; `levels` defaults to \
                  headings 1 to 3 when omitted.",
            examples: &[
                r#"{"op":"insert","target":{"anchor":"<block_id>","position":"after"},"content":[{"type":"toc","levels":{"from":1,"to":2}}]}"#,
            ],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::super::parse_transaction;
    use super::*;

    /// The catalog is exactly the parser table: same ops, same order, same
    /// field lists. (True by construction today; this pins it against a
    /// future rewrite that re-states the table.)
    #[test]
    fn catalog_mirrors_the_parser_table() {
        let catalog = operation_catalog();
        let vocabulary = super::super::operation_vocabulary();
        assert_eq!(catalog.len(), vocabulary.len());
        for (row, (name, fields)) in catalog.iter().zip(vocabulary) {
            assert_eq!(row.name, *name);
            assert_eq!(row.fields, *fields);
        }
    }

    /// A new op must get a deliberate group decision, not fall into the
    /// `"other"` catch-all silently.
    #[test]
    fn every_op_has_a_deliberate_group() {
        for row in operation_catalog() {
            assert_ne!(
                row.group, "other",
                "op {} has no group; add it to group_of",
                row.name
            );
        }
    }

    #[test]
    fn operation_spec_rejects_unknown_names() {
        assert!(operation_spec("replace").is_some());
        assert!(operation_spec("toc").is_none());
    }

    /// THE HARDENING (teaching-surface seam): every shape this catalog
    /// advertises must ITSELF parse as a valid v4 op. A catalog or error
    /// message that teaches an invalid shape is a new trap, not a fix. A new
    /// required field on any op makes its shape stale and trips this test by
    /// construction. Every op must ship at least one shape: a name plus a
    /// bare field list is exactly the "guess the value shape" tax the catalog
    /// exists to remove.
    #[test]
    fn op_shapes_are_themselves_schema_valid() {
        for row in operation_catalog() {
            assert!(
                !row.examples.is_empty(),
                "op {} ships no canonical shape; add one to shapes_of",
                row.name
            );
            for shape in row.examples {
                let txn = format!(r#"{{"ops":[{shape}],"revision":{{"author":"shape-test"}}}}"#);
                parse_transaction(&txn).unwrap_or_else(|e| {
                    panic!(
                        "the suggested {} shape must be parse-valid; got {e}: {shape}",
                        row.name
                    )
                });
            }
        }
    }

    /// A new op must get a deliberate one-line cue, not the generic
    /// placeholder.
    #[test]
    fn every_op_has_a_deliberate_cue() {
        for row in operation_catalog() {
            assert_ne!(
                row.cue, GENERIC_CUE,
                "op {} still carries the generic cue; add one to cue_of",
                row.name
            );
        }
    }

    /// Content-node examples are complete ops, so they parse through the same
    /// gate as the op shapes, and each node must ship at least one.
    #[test]
    fn content_node_shapes_are_themselves_schema_valid() {
        for node in content_node_catalog() {
            assert!(
                !node.examples.is_empty(),
                "content node {} ships no example",
                node.name
            );
            for shape in node.examples {
                let txn = format!(r#"{{"ops":[{shape}],"revision":{{"author":"shape-test"}}}}"#);
                parse_transaction(&txn).unwrap_or_else(|e| {
                    panic!(
                        "the {} content-node shape must be parse-valid; got {e}: {shape}",
                        node.name
                    )
                });
            }
        }
    }

    /// The `table_op` teaching shapes cover BOTH `insert_row` (showing the
    /// `cells` field that carries the new row's content) and `delete_row`,
    /// not just `set_cell_text`, so a schema-error follow-up can fix a
    /// misshapen row-structural op in one try.
    #[test]
    fn table_op_shapes_teach_insert_row_with_cells_and_delete_row() {
        let shapes = operation_spec("table_op")
            .expect("table_op is a parser op")
            .examples;
        assert!(
            shapes
                .iter()
                .any(|s| s.contains("insert_row") && s.contains("cells")),
            "table_op shapes must show insert_row carrying `cells`: {shapes:?}"
        );
        assert!(
            shapes.iter().any(|s| s.contains("delete_row")),
            "table_op shapes must show delete_row: {shapes:?}"
        );
    }

    /// Shapes are teaching material for humans and models alike, and the
    /// generated docs page embeds them verbatim, so they must satisfy the
    /// public-docs style gate (scripts/check-docs.py): no em/en dashes and no
    /// spaced dash punctuation anywhere in a rendered line.
    #[test]
    fn op_shapes_satisfy_the_docs_style_gate() {
        let texts = operation_catalog()
            .into_iter()
            .map(|row| (row.name, row.cue, row.examples))
            .chain(
                content_node_catalog()
                    .iter()
                    .map(|node| (node.name, node.cue, node.examples)),
            );
        for (name, cue, examples) in texts {
            for text in examples.iter().copied().chain([cue]) {
                for banned in ["\u{2014}", "\u{2013}", " - "] {
                    assert!(
                        !text.contains(banned),
                        "{name} teaching text contains banned sequence {banned:?}: {text}"
                    );
                }
            }
        }
    }
}
