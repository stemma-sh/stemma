//! stemma-mcp: a Model Context Protocol server exposing the `stemma` DOCX
//! engine to agents over stdio.
//!
//! This is a proof of concept: it lets an MCP client (e.g. Claude Code) open a
//! DOCX, read its block structure, apply typed tracked-change edits using the
//! v4 edit schema, save the result, and produce a redline between two files.
//!
//! Design notes:
//! - The engine owns no durable state. A `SimpleRuntime` holds opened documents
//!   in memory keyed by a `doc_id` (the handle string). Persist the saved DOCX
//!   bytes if you want durability; the in-memory snapshot is a hot cache.
//! - Edits go through the v4 schema (`stemma::edit_v4`) exactly as the hosted
//!   pipeline does. Validation is fail-loud: a stale `expect`, a destroyed
//!   opaque inline, or an unsupported structure returns an actionable tool
//!   error rather than a best-effort mutation.
//! - Tool results are structured JSON so the model gets machine-readable
//!   feedback (block ids, semantic hashes, error codes) to drive its next step.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};

use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Implementation, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::Deserialize;
use serde_json::{Value, json};

use stemma::edit_v4::parse_transaction;
use stemma::extended_markdown::to_extended_markdown_blocks;
use stemma::view::{
    BlockRole, BlockView, FormFieldIdentity, OpaqueAnchorKind, OpaqueMetadata, SegmentView,
    TextMark, TrackStatus, build_document_view, build_document_view_from_canon, build_outline,
};
use stemma::{
    BlockNode, CanonDoc, DocHandle, DocxRuntime, ExportMode, NoteType, Resolution,
    ResolveSelectionAction, RevisionKind, SimpleRuntime, StoryScope, TableNode, TableRowNode,
    TrackedBlock, TrackingStatus, TransactionMeta, block_semantic_hash_for_block,
};

// ─── Tool argument schemas ───────────────────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct OpenArgs {
    /// Absolute path to a .docx file on the local filesystem.
    path: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    /// The doc_id returned by open_docx.
    doc_id: String,
}

/// The v4 edit-transaction argument as it crosses the MCP wire.
///
/// This is a newtype over `serde_json::Value` for one reason: the parameter
/// MUST advertise a real `{"type": "object"}` JSON Schema. `serde_json::Value`'s
/// own `JsonSchema` impl is the unconstrained `true` schema — it carries no
/// `"type"` at all. A strict MCP client bridge treats an untyped parameter as
/// opaque and JSON-stringifies it before sending; the server's serde layer then
/// sees a STRING where it wants the `EditTransactionV4` struct and rejects every
/// mutation call (`apply_edit` / `check_edit` / `apply_batch`). Advertising an
/// object schema here is what keeps the transaction crossing the wire as an
/// object. `stemma::edit_v4::parse_transaction` remains the authoritative typed
/// parser + schema validator, so the wire schema stays deliberately shallow (it
/// pins the top-level shape; the op catalog lives in the tool description).
#[derive(Debug, Clone, Deserialize)]
#[serde(transparent)]
struct TransactionArg(Value);

impl schemars::JsonSchema for TransactionArg {
    /// Inline the object schema directly into the parent (like a primitive)
    /// rather than emitting a `$ref` — a strict bridge that mishandles an
    /// untyped param may also mishandle a `$ref`, so the `transaction` property
    /// is literally `{"type": "object", ...}` on the wire.
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        "EditTransactionV4".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        // A faithful-but-shallow schema for the v4 transaction. It pins the
        // top-level shape (so a client can validate before sending and the
        // bridge transmits an object) without duplicating the ~40-op catalog,
        // whose source of truth is `stemma::edit_v4` and whose prose lives in
        // the apply_edit description. Each op object is typed only by its
        // required snake_case `op` discriminator; per-op fields are accepted as
        // additional properties and validated by `parse_transaction` at apply.
        schemars::Schema::try_from(json!({
            "type": "object",
            "description": "A v4 edit transaction: an atomic, ordered list of \
                            tracked-change ops plus the revision identity stamped \
                            on every change. All ops apply or none do.",
            "properties": {
                "ops": {
                    "type": "array",
                    "description": "Ordered ops. Each is an object tagged by a \
                                    snake_case `op` discriminator (e.g. replace, \
                                    insert, delete, set_para_format, table_op). \
                                    See the apply_edit tool description for the \
                                    fields each op kind takes.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "op": {
                                "type": "string",
                                "description": "The op kind (snake_case)."
                            }
                        },
                        "required": ["op"]
                    }
                },
                "revision": {
                    "type": "object",
                    "description": "Identity stamped on every tracked change this \
                                    transaction produces.",
                    "properties": {
                        "author": {
                            "type": "string",
                            "description": "Author name on the tracked changes."
                        },
                        "date": {
                            "type": "string",
                            "description": "Optional ISO-8601 timestamp."
                        },
                        "apply_op_id": {
                            "type": "string",
                            "description": "Optional group id stamped on every change."
                        }
                    },
                    "required": ["author"]
                },
                "summary": {
                    "type": "string",
                    "description": "Optional human-readable description of the change."
                },
                "materialization_mode": {
                    "type": "string",
                    "enum": ["tracked_change", "direct"],
                    "description": "How to write the edit (default tracked_change). \
                                    The apply_edit `mode` argument, when set, \
                                    overrides this."
                }
            },
            "required": ["ops", "revision"]
        }))
        .expect("static transaction schema is a valid JSON Schema")
    }
}

impl TransactionArg {
    /// The raw transaction JSON as a string, for handing to
    /// `stemma::edit_v4::parse_transaction` (the authoritative parser).
    ///
    /// A transaction that arrives as a JSON *string* (the object, encoded one
    /// or more times) is unwrapped. This is an intentional edge contract, not
    /// a fallback: some MCP hosts stringify object parameters (stale cached
    /// schema, bridge quirks, or model habit), and layers can stack across a
    /// sandbox→host bridge. A top-level string is never a valid transaction,
    /// so unwrapping is unambiguous at every depth — and the result still has
    /// to pass `parse_transaction` or the call fails loudly like any other
    /// malformed input.
    fn to_json_string(&self) -> String {
        let mut value = self.0.clone();
        // Bounded: each level must itself be valid JSON to continue, so this
        // cannot spin; 4 is far beyond any observed stacking.
        for _ in 0..4 {
            match value {
                Value::String(inner) => match serde_json::from_str::<Value>(&inner) {
                    Ok(next) => value = next,
                    // Not JSON at all: hand the text to parse_transaction for
                    // its loud, detailed error.
                    Err(_) => return inner,
                },
                other => return other.to_string(),
            }
        }
        value.to_string()
    }
}

/// The canonical JSON shape(s) of one v4 op, for teaching the model after a
/// schema error (the discovery tax: structural runs spent 3–8 calls guessing
/// `move` destinations and `set_image_attrs` fields). Placeholders `<...>` are
/// the caller's to fill. Most ops have exactly one shape; `move` has two
/// (single-block and range) because a malformed move is ambiguous about
/// which the caller meant — teach both. EVERY shape here is itself a
/// parse-valid v4 op — pinned by `op_shapes_are_themselves_schema_valid`,
/// because an error message that suggests an INVALID shape is a new trap,
/// not a fix. A new required field on any op makes its shape stale and fails
/// that test by construction.
fn v4_op_shapes(op: &str) -> &'static [&'static str] {
    match op {
        "replace" => &[
            r#"{"op":"replace","target":"<block_id>","content":{"type":"paragraph","content":[{"type":"text","text":"<new text>"}]}}"#,
        ],
        "insert" => &[
            r#"{"op":"insert","target":{"anchor":"<block_id>","position":"after"},"content":[{"type":"paragraph","role":"body_text","content":[{"type":"text","text":"<new text>"}]}]}"#,
            // A native table-of-contents field — `levels` is optional (default
            // 1-3); see the `insert` doc comment above for the full contract.
            r#"{"op":"insert","target":{"anchor":"<block_id>","position":"after"},"content":[{"type":"toc"}]}"#,
        ],
        "delete" => &[r#"{"op":"delete","target":"<block_id>"}"#],
        // Single block, then the contiguous range form (moves several blocks
        // — a section — in one op; `from`/`to` in either doc order).
        "move" => &[
            r#"{"op":"move","target":"<block_id>","destination":{"anchor":"<block_id>","position":"after"}}"#,
            r#"{"op":"move","target":{"from":"<block_id>","to":"<block_id>"},"destination":{"anchor":"<block_id>","position":"after"}}"#,
        ],
        "set_image_attrs" => &[
            r#"{"op":"set_image_attrs","target":"<block_id>","drawing_id":"<drawing_id>","resize":{"cx":4320000,"cy":2880000}}"#,
        ],
        // Retarget a hyperlink: `expect_href` is REQUIRED whenever `attrs.href`
        // is set (optimistic concurrency — the adapter refuses a stale retarget
        // without it), so the shape shows it. The agent reads the current href
        // from any read tool before retargeting.
        "set_attr" => &[
            r#"{"op":"set_attr","target":"<hyperlink_id>","attrs":{"href":"<new_url>"},"expect_href":"<current_url>"}"#,
        ],
        // In-place table edit. The inner op is tagged on `kind` (not `op`); the
        // common case is one cell's text. `insert_row`'s `cells` carries the
        // new row's content in the SAME op — no separate fill step needed;
        // omit `cells` (or give fewer than the column count) for a
        // blank/partly-blank row.
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
        // Replace an existing note's body by its `note_id` (from list_revisions
        // or read_index's notes section).
        "edit_note" => &[
            r#"{"op":"edit_note","note_id":"<note_id>","note_kind":"footnote","body":"<new note body text>"}"#,
        ],
        // Delete a note and its body-side reference run, by `note_id`.
        "delete_note" => &[r#"{"op":"delete_note","note_id":"<note_id>","note_kind":"footnote"}"#],
        _ => &[],
    }
}

/// Append the canonical op shape(s) to a v4 schema error so the model fixes the
/// op in ONE follow-up instead of guessing across several. Best-effort: scans
/// the (structurally-valid-but-schema-invalid) transaction for the op names it
/// used and appends each known shape once. Unknown ops and envelope-level
/// errors fall through to the bare parser message — never a fabricated hint.
fn augment_schema_error(txn_json: &str, base: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(txn_json) else {
        return base.to_string();
    };
    let mut shapes = Vec::new();
    let mut seen = std::collections::HashSet::new();
    if let Some(ops) = value.get("ops").and_then(Value::as_array) {
        for op in ops {
            if let Some(name) = op.get("op").and_then(Value::as_str)
                && seen.insert(name)
            {
                for shape in v4_op_shapes(name) {
                    shapes.push(format!("  {name}: {shape}"));
                }
            }
        }
    }
    if shapes.is_empty() {
        base.to_string()
    } else {
        format!(
            "{base}\nExpected op shape(s) — fill the <…> placeholders:\n{}",
            shapes.join("\n")
        )
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ApplyEditArgs {
    /// The doc_id returned by open_docx.
    doc_id: String,
    /// A v4 edit transaction object. Shape:
    /// {
    ///   "ops": [ { "op": "replace", "target": "<block_id>",
    ///              "expect": "<substring currently in the block>",
    ///              "content": { "type": "paragraph",
    ///                           "content": [ { "type": "text", "text": "new text" } ] } } ],
    ///   "revision": { "author": "Agent" },
    ///   "summary": "what this change does"
    /// }
    /// Op kinds (the `"op"` tag, snake_case):
    ///   replace, insert, delete, move, set_attr, set_format, set_para_format,
    ///   set_cell_format, set_row_format, set_table_format,
    ///   insert_cross_ref, set_numbering, insert_bookmark, rename_bookmark,
    ///   remove_bookmark, apply_style, set_image_attrs, set_image_layout,
    ///   comment_create,
    ///   comment_reply, comment_resolve, comment_delete, insert_note, edit_note,
    ///   delete_note, set_page_setup, set_section_type, insert_section_break,
    ///   create_header, create_footer, edit_header, edit_footer,
    ///   set_header_footer_mode, insert_equation,
    ///   wrap_content_control, wrap_blocks_content_control,
    ///   set_content_control_value, set_form_field_value, set_textbox_text,
    ///   opaque_text_edit, sdt_text_fill, insert_image,
    ///   replace_image, create_style, modify_style, set_doc_defaults, table_op,
    ///   blocks_to_table.
    /// opaque_text_edit does a surgical, tracked find→replace of text INSIDE a
    /// textbox paragraph or inline content control (address via opaque_text_targets),
    /// unlike set_textbox_text which replaces a whole textbox interior untracked.
    /// Targets are block_id strings from read_outline / read_index. `expect`
    /// pins the edit to the current text so a stale edit fails loudly instead of
    /// corrupting the wrong block.
    ///
    /// REPLACE can target a SUB-SPAN of a paragraph (tracked-replace just a few
    /// words, leaving the rest of the paragraph untouched) via an optional
    /// `span`: `{ "op": "replace", "target": "<para_id>", "span": "<s_n>",
    /// "guard": "<guard>", "content": { "type": "paragraph", "content":
    /// [ { "type": "text", "text": "new words" } ] } }`. Get `<s_n>` and the
    /// `guard` from read_block; `span` may also be `"whole"` (the default,
    /// = whole-paragraph replace), `{ "after": "<anchor_id>" }`,
    /// `{ "before": "<anchor_id>" }`, or `{ "between": [<endpoint>, <endpoint>] }`.
    /// A span op REQUIRES the block `guard`, and may carry `expect` = the
    /// span's exact current text (recommended: a mismatch fails stale_edit
    /// instead of editing the wrong words). The edit is applied as a splice:
    /// tracked changes elsewhere in the paragraph are carried through
    /// untouched, so a new tracked change can be layered beside existing
    /// ones. Pending INSERTIONS are editable too: editing your own pending
    /// insertion revises it in place; editing ANOTHER author's pending
    /// insertion STACKS — the removed text stays visible as
    /// inserted-then-deleted (both revisions pending, independently
    /// resolvable via accept_changes/reject_changes; resolutions cascade and
    /// the response's cascaded_revision_ids names what was carried along).
    /// Refused: spans over pending DELETIONS (the text is already struck),
    /// spans over an inserted-then-deleted range (resolve it instead), and
    /// spans that would separate a bookmarkStart/commentRangeStart from its
    /// partner.
    ///
    /// INSERT authors a new paragraph and REQUIRES a `role` on each PARAGRAPH
    /// content block: `{ "op": "insert", "target": {...}, "content": [ { "type":
    /// "paragraph", "role": "<role_token>", "content": [...] } ] }`. Get
    /// `<role_token>` from any read tool's `role_token` field (present even when
    /// `style` is null, e.g. "body_text"); or pass the alias "default" / "body"
    /// for the document's default body paragraph role.
    ///
    /// A paragraph content block may also carry an optional `list` field to
    /// author it AS a list item at a chosen level in one tracked insert:
    /// `{ "type": "paragraph", "role": "<role_token>", "content": [...],
    /// "list": { "num_id": <n>, "ilvl": <0..=8> } }`. The inserted paragraph
    /// gets its `w:numPr` (numId + ilvl) from the start — no follow-up
    /// `set_numbering` (which is refused on a freshly-inserted paragraph). To
    /// add a sub-point UNDER an existing list item, read that item's
    /// `list.num_id` (from read_block / find), then insert after it with
    /// `{ "num_id": <that num_id>, "ilvl": <parent.ilvl + 1> }`. `num_id` MUST
    /// be a num_id the document already uses (the engine never fabricates a
    /// numbering definition — an unknown num_id is refused).
    ///
    /// A `toc` content block inserts a native table-of-contents field — no
    /// `role` (unlike `paragraph`/`table`, it never asks for one): `{ "op":
    /// "insert", "target": {...}, "content": [ { "type": "toc" } ] }`, or with
    /// explicit heading levels: `{ "type": "toc", "levels": { "from": 1,
    /// "to": 3 } }` (`1 <= from <= to <= 9`; an inverted or out-of-range pair
    /// is refused, not clamped). `levels` defaults to `1-3` — Word's own
    /// "Automatic Table of Contents" range and field switches (hyperlinked
    /// entries, page numbers hidden in web layout, outline levels included).
    /// The field has no cached entries yet: Word computes and displays them
    /// the next time the document is opened (the same apply also turns on
    /// the document's "update fields on open" setting, so this happens
    /// automatically — no separate op needed). A `toc` block is insert-only
    /// (`replace` refuses it) and top-level only (refused inside a table
    /// cell).
    ///
    /// MOVE relocates one block or a contiguous RANGE of blocks (a whole
    /// section) in one op: `{ "op": "move", "target": "<block_id>" | \
    /// { "from": "<block_id>", "to": "<block_id>" }, "destination": \
    /// { "anchor": "<block_id>", "position": "before" | "after" } }`. `from`/
    /// `to` may be given in either document order. Do NOT move a range by
    /// chaining several single-block moves that each anchor on the PREVIOUS
    /// move's block — once moved, that id becomes a moveFrom shadow at its
    /// OLD position, and anchoring on it is refused
    /// (`AmbiguousAnchorAfterMove`, naming the moveTo copy to anchor on
    /// instead). The apply_edit receipt's `moves` entry confirms where the
    /// run landed (source_id->copy_id pairs, prev/next neighbor at the
    /// destination) without a follow-up read.
    ///
    /// A `table_op` carries `{ "op": "table_op", "target": "<table_block_id>",
    /// "table_op": { "kind": "insert_row" | "delete_row" | "insert_column" |
    /// "delete_column" | "merge_cells" | "set_cell_text", ... } }`. For
    /// set_cell_text supply the `{ "row", "col" }` from the table block's `cells`
    /// (read_block / read_outline / find expose per-cell `{row, col, text, block_id}`).
    ///
    /// `insert_row` carries the new row's CONTENT in the SAME op via an
    /// optional `cells` field — one plain-text string per column, left to
    /// right: `{ "kind": "insert_row", "ref_row": <r>, "position": "before" |
    /// "after", "cells": ["<col0 text>", "<col1 text>", ...] }`. Fewer entries
    /// than columns leaves the rest empty; omit `cells` entirely for an
    /// all-blank row. MORE entries than the table has columns is refused
    /// (naming the column count) rather than clamped. This is ONE tracked row
    /// insertion — do not follow it with `set_cell_text` calls to fill a row
    /// you just inserted blank; give the content up front instead. If you DO
    /// need to fill a row inserted earlier IN THE SAME apply_edit call,
    /// `set_cell_text` on that row's cells is allowed (the content becomes
    /// part of the same pending insertion); `set_cell_text` on a cell
    /// carrying a PRE-EXISTING tracked change (from an earlier call, or
    /// imported from Word) is still refused — accept/reject that revision
    /// first, or target the cell paragraph's own `block_id` (from `cells`)
    /// with a tracked `replace` instead of the grid address.
    ///
    /// `delete_row` marks the whole row (and its cells) as a tracked
    /// deletion: `{ "kind": "delete_row", "row_index": <r> }`. Deleting the
    /// table's last remaining row is refused — delete the whole table block
    /// instead.
    ///
    /// `set_para_format` sets paragraph-level formatting in place as a tracked
    /// `w:pPrChange` (no role swap): `{ "op": "set_para_format", "target":
    /// "<para_id>", "align"?, "indent"?, "spacing"?, "borders"?, "shading"? }`.
    /// `borders` is `{ "top"?, "bottom"?, "left"?, "right"?, "between"?, "bar"? }`
    /// where each edge is `{ "style": "single"|"double"|..., "color"?: "RRGGBB"
    /// |"auto", "size"?: <eighths-pt>, "space"?: <pt> }`. `shading` is `{ "fill"?:
    /// "RRGGBB"|"auto", "pattern"?: "clear"|"solid"|..., "color"?: "RRGGBB" }`.
    /// At least one of align/indent/spacing/borders/shading is required; an
    /// unknown style/pattern token is refused at the wire edge (no silent
    /// fallback).
    ///
    /// `set_cell_format` sets ONE table cell's formatting in place as a tracked
    /// `w:tcPrChange`: `{ "op": "set_cell_format", "target": "<table_block_id>",
    /// "row_index": <r>, "col_index": <c>, "borders"?, "shading"?, "width"?,
    /// "v_align"?, "margins"? }`. Address the cell by the LOGICAL `{row, col}`
    /// the table block's `cells` expose (same address as set_cell_text). `borders`
    /// is a cell border set `{ "top"?, ..., "inside_h"?, "inside_v"? }` (edges as
    /// above); `shading` as above; `width` is `{ "w": <n>, "width_type": "dxa"|
    /// "pct"|"auto"|"nil" }`; `v_align` is "top"|"center"|"bottom"; `margins` is
    /// `{ "top"?, "bottom"?, "left"?, "right"? }` in twips. At least one property
    /// is required. It byte-preserves the table's tblPr, every other cell, and the
    /// target cell's untouched properties; accept keeps the new tcPr, reject
    /// reverts it.
    ///
    /// `set_row_format` sets ONE table row's height in place as a tracked
    /// `w:trPrChange`: `{ "op": "set_row_format", "target": "<table_block_id>",
    /// "row_index": <r>, "height"?, "height_rule"? }`. `height` is the row height
    /// in twips; `height_rule` is "exact"|"atLeast"|"auto". At least one property
    /// is required. It byte-preserves the table's tblPr, every other row, and
    /// every cell of the target row; accept keeps the new trPr, reject reverts it.
    ///
    /// `set_table_format` sets TABLE-level formatting in place as a tracked
    /// `w:tblPrChange`: `{ "op": "set_table_format", "target": "<table_block_id>",
    /// "borders"?, "width"?, "default_cell_margins"? }`. `borders` is a table
    /// border set `{ "top"?, ..., "inside_h"?, "inside_v"? }` (edges as above);
    /// `width` is `{ "w": <n>, "width_type": "dxa"|"pct"|"auto"|"nil" }`;
    /// `default_cell_margins` is `{ "top"?, "bottom"?, "left"?, "right"? }` in
    /// twips. At least one property is required. There is NO table-level shading
    /// (cell shading lives on set_cell_format). It byte-preserves every row and
    /// cell and the table's untouched tblPr; accept keeps the new tblPr, reject
    /// reverts it.
    ///
    /// A `set_numbering` carries `{ "op": "set_numbering", "target":
    /// "<list_para_id>", "change": { "kind": <list-change> } }` where
    /// <list-change> is one of: set_list (num_id+ilvl), set_level (ilvl),
    /// set_type (bullet<->numbered, num_id), indent, outdent, restart, continue,
    /// remove, split. Read a paragraph's `list` field ({num_id, ilvl, ordered,
    /// marker_text}, on read_block / read_outline / read_index / find) to target
    /// these.
    ///
    /// `split` divides ONE numbered/bulleted list at a list item so the tail
    /// renumbers from 1 as an INDEPENDENT list: `{ "op": "set_numbering",
    /// "target": "<split_item_id>", "change": { "kind": "split" } }`. The split
    /// item and the contiguous following items at the same num_id/ilvl are
    /// re-pointed (as tracked pPrChanges) at a BRAND-NEW num_id whose definition
    /// the engine authors by cloning the source list's level formats — so the
    /// two lists look identical but count separately. No fields to supply: the
    /// engine allocates the new num_id and clones the definition itself. To
    /// drive it, read the split item's `list.num_id` from read_block (to confirm
    /// it is a list item), then call split targeting that block. Items before
    /// the split keep the original num_id. Refused on a non-list paragraph.
    ///
    /// `create_header` / `create_footer` author a NET-NEW, blank running head and
    /// reference it from the body section, tracked as a `w:sectPrChange`:
    /// `{ "op": "create_header", "kind": "default" | "first" | "even" }`. The new
    /// story starts blank (one empty paragraph) — follow up with `edit_header` /
    /// `edit_footer` to fill it. Accept keeps the new header/footer; reject
    /// restores the original section (no header) and drops the blank story. A
    /// header/footer of the same `kind` already on the section is refused (edit it
    /// instead); a section that already carries a tracked sectPr change is refused
    /// (accept/reject it first).
    ///
    /// A `comment_create` carries `{ "op": "comment_create", "target":
    /// "<block_id>", "expect": "<anchor substring>", "body": "<comment text>",
    /// "author": "..." }`. Comments are annotations, not tracked changes.
    ///
    /// A `blocks_to_table` converts a CONTIGUOUS RUN of paragraphs (e.g. a bullet
    /// list) INTO a table, as one tracked change: `{ "op": "blocks_to_table",
    /// "from": "<first_para_id>", "to": "<last_para_id>", "delimiter": " — ",
    /// "header": ["Feature", "Notes"] }`. Each source paragraph becomes one body
    /// row; its visible text is split by `delimiter` into cells (e.g.
    /// "Term — definition" -> ["Term", "definition"]). The optional `header` adds
    /// a leading header row and FIXES the column count — every body row must split
    /// into exactly that many cells or the op is refused (no ragged table). The
    /// new table is a tracked INSERT and the source paragraphs a tracked DELETE,
    /// so accept-all => the table, reject-all => the original paragraphs. To drive
    /// it: read the list block ids (read_outline / read_index / find give the
    /// contiguous paragraph ids), then call blocks_to_table over the range with
    /// the delimiter (and header, if you want labeled columns). A source paragraph
    /// carrying an opaque inline (image/field/hyperlink) is refused — split it out
    /// of the range first (its content would be lost in a text-only cell).
    ///
    /// `insert_image` / `replace_image` supply the image bytes by EITHER `path`
    /// (an absolute path to the file on disk, read server-side — preferred; no
    /// hand-encoding) OR `bytes_base64` (the base64 bytes). Exactly one is
    /// required; both or neither is refused. `format` is `"png"|"jpeg"|"gif"`.
    /// `cx`/`cy` (the display box, in EMUs) are OPTIONAL: omit BOTH to use the
    /// image's intrinsic pixel size at 96 DPI, or give ONE to scale the other by
    /// the intrinsic aspect ratio. Shape: `{ "op": "insert_image", "target":
    /// "<para_id>", "path": "/abs/logo.png", "format": "png" }`.
    transaction: TransactionArg,
    /// Materialization mode for this transaction: "tracked" (default) writes the
    /// edit as tracked changes (w:ins/w:del) that a reviewer can accept/reject;
    /// "direct" applies the edit immediately with no tracked markup (the change
    /// is baked into the text). When set, this overrides any
    /// `materialization_mode` inside the transaction body. Unknown values are
    /// rejected (no silent fallback).
    #[serde(default)]
    mode: Option<String>,
    /// Author-impersonation override. When the transaction's
    /// `revision.author` already authors revisions in this document (the
    /// existing redline), the write is refused so the agent's edits stay
    /// distinguishable from the prior reviewer's. Pass `true` to deliberately
    /// continue an existing author's work. Default false.
    #[serde(default)]
    allow_existing_author: bool,
}

/// Parse the per-call materialization-mode override at the MCP edge. Returns the
/// engine mode, or an actionable error for an unknown value — never a silent
/// fallback (CLAUDE.md "no silent fallbacks").
fn parse_materialization_mode(
    mode: &Option<String>,
) -> Result<Option<stemma::edit::MaterializationMode>, String> {
    match mode.as_deref() {
        None => Ok(None),
        Some("tracked") => Ok(Some(stemma::edit::MaterializationMode::TrackedChange)),
        Some("direct") => Ok(Some(stemma::edit::MaterializationMode::Direct)),
        Some(other) => Err(format!("mode must be 'tracked' or 'direct', got '{other}'")),
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadBlockArgs {
    /// The doc_id returned by open_docx.
    doc_id: String,
    /// The block id (from read_markdown / read_outline) to inspect in detail.
    block_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct FindArgs {
    /// The doc_id returned by open_docx.
    doc_id: String,
    /// Text to search for (case-insensitive substring) across block text.
    pattern: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SectionArgs {
    /// The doc_id returned by open_docx.
    doc_id: String,
    /// A heading block id (from read_markdown). Returns that heading and the
    /// blocks under it, up to the next heading of the same or higher level.
    heading_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SaveArgs {
    /// The doc_id returned by open_docx.
    doc_id: String,
    /// Absolute path to write the resulting .docx to.
    path: String,
}

// ─── find-replace (stream: find-and-replace-all as tracked changes) ───────────
// Self-contained block (args + tool) so parallel main.rs additions merge as a
// trivial both-added. See `stemma::edit::plan_find_replace_all`.

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct FindReplaceArgs {
    /// The doc_id returned by open_docx.
    doc_id: String,
    /// Literal substring to find across paragraph text.
    needle: String,
    /// Literal replacement text. Written verbatim (including its casing) even
    /// under case-insensitive matching.
    replacement: String,
    /// Which stories to search: "body" (default) or "body_and_stories".
    #[serde(default = "default_scope")]
    scope: String,
    /// Case-sensitive matching (default true).
    #[serde(default = "default_true")]
    case_sensitive: bool,
    /// Whole-word matching: a match counts only at Unicode non-alphanumeric
    /// boundaries (default false).
    #[serde(default)]
    whole_word: bool,
    /// What to do when a needle straddles a barrier anchor (opaque/field/
    /// hyperlink/break): "skip" (default, leave that paragraph untouched) or
    /// "fail" (reject the whole operation).
    #[serde(default = "default_barrier")]
    on_barrier_match: String,
}

fn default_scope() -> String {
    "body".to_string()
}
fn default_true() -> bool {
    true
}
fn default_barrier() -> String {
    "skip".to_string()
}

// ─── replace_text: tracked-native find/replace (splices through tracked) ──────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReplaceTextArgs {
    /// The doc_id returned by open_docx.
    doc_id: String,
    /// The literal phrase to find (compared under `match_mode`).
    old: String,
    /// The literal replacement, written verbatim.
    new: String,
    /// Author stamped on the resulting tracked change.
    author: String,
    /// Where to search. Omit for the whole body. To restrict: an inclusive block
    /// range `{ "from_block_id": "p_3", "to_block_id": "p_9" }` or a single
    /// block `{ "block_id": "p_7" }`.
    #[serde(default)]
    scope: Option<ReplaceTextScopeArg>,
    /// How many occurrences must match. A number (default 1) requires EXACTLY
    /// that many — if the actual count differs the call fails with the per-match
    /// contexts so you can disambiguate in one follow-up. Pass "all" to replace
    /// every occurrence.
    #[serde(default)]
    expected_matches: Option<ExpectedMatchesArg>,
    /// "exact" (default) byte-for-byte, or "normalize_ws" to also match across
    /// visually-equivalent characters (NBSP family + typographic spaces + tab as
    /// space; curly vs straight apostrophes/quotes). Whatever folding actually
    /// fires is reported in the receipt's `normalization_applied`.
    #[serde(default = "default_match_mode")]
    match_mode: String,
    /// What to do when a match would straddle a wall (an opaque anchor or a
    /// tracked-change boundary): "skip" (default, leave that paragraph untouched,
    /// reported in `skipped_straddles`) or "fail" (reject the whole operation).
    #[serde(default = "default_barrier")]
    on_barrier_match: String,
    /// Author-impersonation override. When `author` already authors revisions
    /// in this document (the existing redline), the write is refused so the
    /// agent's edits stay distinguishable from the prior reviewer's. Pass
    /// `true` to deliberately continue an existing author's work. Default false.
    #[serde(default)]
    allow_existing_author: bool,
}

/// Either a single number or the string "all".
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
enum ExpectedMatchesArg {
    Count(usize),
    Keyword(String),
}

/// One find/replace instruction inside a `replace_text_batch` worklist. Same
/// semantics as a single `replace_text` call (server-side match, tracked splice,
/// `expected_matches` disambiguation contract), minus the shared `doc_id`/`author`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReplaceItem {
    /// The literal phrase to find (compared under `match_mode`).
    old: String,
    /// The literal replacement, written verbatim.
    new: String,
    #[serde(default)]
    scope: Option<ReplaceTextScopeArg>,
    /// Occurrences that must match: a number (default 1) or "all". A mismatch
    /// fails THIS item only (reported with per-match contexts), never the batch.
    #[serde(default)]
    expected_matches: Option<ExpectedMatchesArg>,
    #[serde(default = "default_match_mode")]
    match_mode: String,
    #[serde(default = "default_barrier")]
    on_barrier_match: String,
}

/// A whole find/replace worklist applied in ONE call. The agent hands over its
/// list of instructions (the "counsel sent a list of changes" shape) instead
/// of one round trip per phrase.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReplaceTextBatchArgs {
    /// The doc_id returned by open_docx.
    doc_id: String,
    /// Author stamped on every resulting tracked change (one identity for the
    /// whole worklist).
    author: String,
    /// The find/replace instructions, applied in order against live state (so a
    /// later item sees earlier items' edits).
    replacements: Vec<ReplaceItem>,
    /// Author-impersonation override (see replace_text). Default false.
    #[serde(default)]
    allow_existing_author: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReplaceTextScopeArg {
    /// Restrict to a single block.
    #[serde(default)]
    block_id: Option<String>,
    /// Restrict to an inclusive block-id range (document order).
    #[serde(default)]
    from_block_id: Option<String>,
    #[serde(default)]
    to_block_id: Option<String>,
}

fn default_match_mode() -> String {
    "exact".to_string()
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CompareArgs {
    /// Absolute path to the base ("original") .docx.
    base_path: String,
    /// Absolute path to the target ("modified") .docx.
    target_path: String,
    /// Absolute path to write the redline .docx to.
    out_path: String,
    /// Author name stamped on the redline's tracked changes. Defaults to "stemma".
    #[serde(default)]
    author: Option<String>,
}

// ─── Result helpers ──────────────────────────────────────────────────────────

/// The build identity reported in-band: in every error payload (see
/// `fail_json`) and in the `open_docx` response. The pack scripts inject the
/// full stamp (e.g. "0.1.0+gd364e389") via `STEMMA_MCP_BUILD_STAMP` at compile
/// time; a plain `cargo build` falls back to the crate version. In-band
/// identity exists because two registrations of this server look identical
/// from inside a session (same display name), so a stale duplicate is
/// otherwise only diagnosable by failure forensics.
const SERVER_VERSION: &str = match option_env!("STEMMA_MCP_BUILD_STAMP") {
    Some(stamp) => stamp,
    None => env!("CARGO_PKG_VERSION"),
};

// ─── Runtime configuration (parsed at the edge) ──────────────────────────────

/// Env var: idle seconds before an open document is evicted from memory.
const ENV_DOC_TTL_SECS: &str = "STEMMA_MCP_DOC_TTL_SECS";
/// Env var: the largest `.docx` `open_docx` will read, in bytes.
const ENV_MAX_DOC_BYTES: &str = "STEMMA_MCP_MAX_DOC_BYTES";

/// Default idle TTL: 24 hours. Deliberately generous — an interactive editing
/// session can sit idle (agent thinking, user away) far longer than minutes, so
/// a short TTL would evict a document mid-session, which is surprising. A day is
/// long enough to never strand a live session yet still bound a long-lived,
/// multi-document host. `0` disables eviction entirely.
const DEFAULT_DOC_TTL_SECS: u64 = 86_400;
/// Default open-size cap: 50 MiB. Real DOCX files (including image-heavy ones)
/// sit well under this; the cap exists to refuse an accidental or pathological
/// huge read before it is pulled into memory. `0` disables the cap.
const DEFAULT_MAX_DOC_BYTES: u64 = 50 * 1024 * 1024;

/// Server configuration parsed once at startup from the environment. Parsing is
/// fail-loud: a malformed value is a startup error, never a silent fallback. An
/// absent value takes the documented default.
#[derive(Clone, Copy, Debug)]
struct Config {
    /// Idle seconds before an open document is evicted; `0` disables eviction.
    doc_ttl_secs: u64,
    /// Largest `.docx` (in bytes) `open_docx` will read; `0` disables the cap.
    max_doc_bytes: u64,
}

impl Config {
    /// Read the config from the process environment. `Err` is an actionable,
    /// human-readable message describing which variable is malformed.
    fn from_env() -> Result<Self, String> {
        Ok(Self {
            doc_ttl_secs: env_u64(ENV_DOC_TTL_SECS, DEFAULT_DOC_TTL_SECS)?,
            max_doc_bytes: env_u64(ENV_MAX_DOC_BYTES, DEFAULT_MAX_DOC_BYTES)?,
        })
    }

    /// The documented defaults, used when the server is constructed without a
    /// parsed environment (the test constructor and any embedding that does not
    /// go through `main`).
    #[cfg(test)]
    fn defaults() -> Self {
        Self {
            doc_ttl_secs: DEFAULT_DOC_TTL_SECS,
            max_doc_bytes: DEFAULT_MAX_DOC_BYTES,
        }
    }
}

/// Parse a single u64 setting from a raw value (`None` = variable absent).
/// Pure so the parse rules are unit-testable without touching the environment.
fn parse_u64_setting(name: &str, raw: Option<&str>, default: u64) -> Result<u64, String> {
    match raw {
        None => Ok(default),
        Some(s) => s.trim().parse::<u64>().map_err(|_| {
            format!(
                "{name}={s:?} is not a non-negative integer; unset it to use the default \
                 ({default}) or set it to 0 to disable"
            )
        }),
    }
}

fn env_u64(name: &str, default: u64) -> Result<u64, String> {
    match std::env::var(name) {
        Ok(raw) => parse_u64_setting(name, Some(&raw), default),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(std::env::VarError::NotUnicode(_)) => Err(format!("{name} is not valid UTF-8")),
    }
}

/// Render a duration in whole hours/minutes/seconds for an error message
/// (`86400` → `24h`, `1800` → `30m`, `45` → `45s`).
fn humanize_secs(secs: u64) -> String {
    if secs != 0 && secs.is_multiple_of(3600) {
        format!("{}h", secs / 3600)
    } else if secs != 0 && secs.is_multiple_of(60) {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

// ─── CLI argument handling ───────────────────────────────────────────────────

/// The action a command line resolves to. This binary is an MCP stdio server,
/// not an interactive CLI, so the only accepted invocations are the bare server
/// launch plus `--help`/`--version`; anything else is a usage error rather than
/// a silent server start (which would surface downstream as a confusing
/// "connection closed").
#[derive(Debug, PartialEq, Eq)]
enum Cli {
    Serve,
    Help,
    Version,
    /// Unrecognized arguments (carries the offending argument list for the
    /// usage message).
    Bad(String),
}

/// Parse the command line (arguments EXCLUDING argv[0]).
fn parse_cli(args: &[String]) -> Cli {
    match args.first().map(String::as_str) {
        None => Cli::Serve,
        Some("--help" | "-h") if args.len() == 1 => Cli::Help,
        Some("--version" | "-V") if args.len() == 1 => Cli::Version,
        Some(_) => Cli::Bad(args.join(" ")),
    }
}

fn usage() -> String {
    format!(
        "stemma-mcp {SERVER_VERSION}\n\
         Model Context Protocol (MCP) server exposing stemma's tracked-change DOCX editing verbs.\n\
         \n\
         It speaks MCP over stdio (JSON-RPC on stdin/stdout); it is NOT an interactive CLI.\n\
         Launch it from an MCP client (e.g. Claude Code), not directly from a shell.\n\
         \n\
         USAGE:\n    \
         stemma-mcp [--help | --version]\n\
         \n\
         ENVIRONMENT:\n    \
         {ENV_DOC_TTL_SECS}   Idle seconds before an open document is evicted from\n                              \
         memory (default {DEFAULT_DOC_TTL_SECS} = 24h; set 0 to disable).\n    \
         {ENV_MAX_DOC_BYTES}  Largest .docx open_docx will read, in bytes\n                              \
         (default {DEFAULT_MAX_DOC_BYTES} = 50 MiB; set 0 to disable).\n    \
         RUST_LOG                  Log filter (default stemma_mcp=info); logs go to stderr.\n\
         \n\
         See stemma-mcp/README.md for the full tool surface and lifecycle notes.\n"
    )
}

fn ok(value: Value) -> CallToolResult {
    CallToolResult::structured(value)
}

/// Every error payload passes through here so each one names the build that
/// produced it (`server_version`).
fn fail_json(mut payload: Value) -> CallToolResult {
    payload["server_version"] = json!(SERVER_VERSION);
    CallToolResult::structured_error(payload)
}

/// A domain failure surfaced as a tool error (is_error = true) with an
/// actionable, structured payload. The model reads `error`/`code` to decide
/// its next step (e.g. re-read the outline after a stale-edit failure).
fn fail(code: &str, message: impl Into<String>) -> CallToolResult {
    fail_json(json!({ "code": code, "error": message.into() }))
}

/// Render one block of the current document as a read-view row. Built from the
/// [`BlockView`] so the row carries the SAME discoverability fields the detail
/// read does: the insert-acceptable `role_token`, list membership, and (for
/// tables) per-cell grid addressing. `tracked` is the source `TrackedBlock` —
/// used only for its `block_status` label and the raw-node `semantic_hash`,
/// which is the value a write op carries as its guard.
fn block_row(view: &BlockView, tracked: &TrackedBlock) -> Value {
    let kind = match &view.role {
        BlockRole::Paragraph | BlockRole::Heading { .. } => "paragraph",
        BlockRole::Table => "table",
        BlockRole::Opaque => "opaque",
    };
    let status_label = match &tracked.status {
        TrackingStatus::Normal => "normal",
        TrackingStatus::Inserted(_) => "inserted",
        TrackingStatus::Deleted(_) => "deleted",
        TrackingStatus::InsertedThenDeleted(_) => "inserted_then_deleted",
    };
    json!({
        "id": view.id.to_string(),
        "kind": kind,
        // For opaque blocks: WHY this is a placeholder (e.g. "sdt",
        // "quarantined_nested_tracked_changes" — the latter means the block
        // carries nested tracked changes the engine cannot represent yet; it
        // is read-only and its text is deliberately not shown).
        "opaque_label": view.opaque_label,
        "style": view.style_id,
        // Insert-acceptable role token (see block_detail_json) — present even
        // when `style` is null (Normal-styled doc).
        "role_token": view.role_token,
        "list": list_json(view.list.as_ref()),
        "cells": cells_json(view),
        "status": status_label,
        "text": view.text,
        "semantic_hash": block_semantic_hash_for_block(&tracked.block),
    })
}

// ─── DocumentView → JSON (detail-on-demand) ────────────────────────────────────

fn track_status_json(status: &TrackStatus) -> Value {
    match status {
        TrackStatus::Normal => json!("normal"),
        TrackStatus::Inserted(r) => json!({
            "status": "inserted", "revision_id": r.revision_id,
            "author": r.author, "date": r.date,
        }),
        // The stacked state: ONE span, compound status carrying BOTH pending
        // revisions. Resolvable (accept/reject by either id), not
        // text-editable.
        TrackStatus::InsertedThenDeleted { inserted, deleted } => json!({
            "status": "inserted_then_deleted",
            "inserted": {
                "revision_id": inserted.revision_id,
                "author": inserted.author, "date": inserted.date,
            },
            "deleted": {
                "revision_id": deleted.revision_id,
                "author": deleted.author, "date": deleted.date,
            },
        }),
        TrackStatus::Deleted(r) => json!({
            "status": "deleted", "revision_id": r.revision_id,
            "author": r.author, "date": r.date,
        }),
    }
}

/// One row of the compact revision table emitted by `list_revisions`.
///
/// This is the *summary* projection of a tracked change: who, what kind, where,
/// and a short excerpt of the affected text — enough for an agent to build an
/// accept/reject id list (or read a specific block for context) WITHOUT pulling
/// the whole `read_redline` markdown. One tracked status (one `w:ins`/`w:del`)
/// produces exactly one row; a stacked inserted-then-deleted span produces TWO
/// (one per pending revision), matching what accept_changes/reject_changes
/// resolve. `author`/`date` are `None` when the source DOCX carried none — never
/// invented (CLAUDE.md: no fabricated authors).
struct RevisionRow {
    revision_id: u32,
    author: Option<String>,
    /// Which OOXML change element carries the revision; serialized as the
    /// wire name (`"insert"`, `"delete"`, `"format_run"`, `"format_paragraph"`,
    /// `"format_table"`, `"format_row"`, `"format_cell"`, `"format_section"`,
    /// or `"opaque_interior"` — a tracked change inside embedded content
    /// (textbox/object), reported with `revision_id` 0 and NOT individually
    /// resolvable).
    kind: RevisionKind,
    block_id: String,
    /// ≤ 80 chars of the affected text, truncated on a char boundary.
    excerpt: String,
    date: Option<String>,
    /// Which story this revision lives in — the main body, or a
    /// footnote/endnote/header/footer/comment by id. Serializes as
    /// `"Body"`, `{"Footnote":{"id":"3"}}`, etc. (serde's default
    /// externally-tagged representation of `StoryScope`) — without this a
    /// caller cannot tell "this one lives in footnote 2" apart from a body
    /// revision that happens to share the same revision_id space.
    location: StoryScope,
}

/// One [`RevisionRow`] per pending revision, in document order, sourced from
/// the engine's canonical enumeration (`stemma::tracked_model::
/// enumerate_revisions`) — the SAME walk the accept/reject selectors lower
/// against, and the walk the no-invisible-ink contract is pinned on
/// (spec_revision_enumeration.rs): inline segments, paragraph marks, table
/// row/cell structure, cell-interior paragraphs, and run/paragraph/table/row/
/// cell formatting changes.
fn revision_rows(canonical: &stemma::CanonDoc) -> Vec<RevisionRow> {
    stemma::tracked_model::enumerate_revisions(canonical)
        .into_iter()
        .map(|r| RevisionRow {
            revision_id: r.revision_id,
            author: r.author,
            kind: r.kind,
            block_id: r.block_id.to_string(),
            excerpt: cap_excerpt(&r.excerpt),
            date: r.date,
            location: r.location,
        })
        .collect()
}

/// Excerpts stay readable in receipts: cap at 80 chars on a char boundary.
fn cap_excerpt(s: &str) -> String {
    if s.chars().count() <= 80 {
        s.to_string()
    } else {
        s.chars().take(80).collect()
    }
}

/// Apply the [`MAX_REVISION_ROWS`] cap, returning the rows to emit and — ONLY
/// when the cap bites — an explicit truncation report. The report is the whole
/// point of the cap being non-silent (CLAUDE.md): it names the limit, the true
/// total, how many rows were omitted, and how to narrow the query to see them.
/// `None` when nothing was dropped.
fn cap_revision_rows(rows: &[RevisionRow]) -> (&[RevisionRow], Option<Value>) {
    if rows.len() <= MAX_REVISION_ROWS {
        return (rows, None);
    }
    let report = json!({
        "limit": MAX_REVISION_ROWS,
        "total": rows.len(),
        "omitted": rows.len() - MAX_REVISION_ROWS,
        "advice": "narrow the result with filter.by_block_range or filter.by_author to see the omitted rows",
    });
    (&rows[..MAX_REVISION_ROWS], Some(report))
}

fn revision_row_json(r: &RevisionRow) -> Value {
    json!({
        "revision_id": r.revision_id,
        "author": r.author,
        "kind": r.kind.as_str(),
        "block_id": r.block_id,
        "excerpt": r.excerpt,
        "date": r.date,
        "location": r.location,
    })
}

fn mark_strs(marks: &[TextMark]) -> Vec<&'static str> {
    marks
        .iter()
        .map(|m| match m {
            TextMark::Bold => "bold",
            TextMark::Italic => "italic",
            TextMark::Underline => "underline",
            TextMark::Strike => "strike",
            TextMark::Subscript => "subscript",
            TextMark::Superscript => "superscript",
        })
        .collect()
}

/// For one block, find every opaque anchor whose surfaced metadata contains the
/// (already-lowercased) needle, returning `(matched_in, anchor_json)` per hit.
///
/// What each kind contributes (§2.9 / addendum §A.5):
/// - ContentControl: `tag`, `alias`, `display_text` → `matched_in:
///   "content_control"`.
/// - Field: the legacy form-field `name` and dropdown `entries` (from a Begin
///   anchor's ffData) → `matched_in: "form_field"`. The field RESULT is already
///   findable as ordinary block text (`paragraph_text` includes it), so it is
///   NOT re-matched here; the instruction text is noise and deliberately skipped.
/// - Drawing: `alt_text` → `matched_in: "image_alt"`.
///
/// Other kinds carry nothing an agent searches for and contribute no match.
fn opaque_metadata_matches(block: &BlockView, needle: &str) -> Vec<(&'static str, Value)> {
    let mut out = Vec::new();
    for seg in &block.segments {
        let SegmentView::Opaque {
            id, kind, metadata, ..
        } = seg
        else {
            continue;
        };
        let Some(metadata) = metadata else { continue };
        let hit = |s: &Option<String>| {
            s.as_deref()
                .is_some_and(|v| v.to_lowercase().contains(needle))
        };
        match metadata {
            OpaqueMetadata::ContentControl {
                tag,
                alias,
                display_text,
                ..
            } if hit(tag) || hit(alias) || hit(display_text) => {
                out.push((
                    "content_control",
                    json!({
                        "id": id.to_string(),
                        "anchor_kind": anchor_kind_str(kind),
                        "tag": tag,
                    }),
                ));
            }
            OpaqueMetadata::Field {
                form: Some(form), ..
            } if form_field_matches(form, needle) => {
                out.push((
                    "form_field",
                    json!({
                        "id": id.to_string(),
                        "anchor_kind": anchor_kind_str(kind),
                        "name": form_field_name(form),
                    }),
                ));
            }
            OpaqueMetadata::Drawing { alt_text, .. } if hit(alt_text) => {
                out.push((
                    "image_alt",
                    json!({
                        "id": id.to_string(),
                        "anchor_kind": anchor_kind_str(kind),
                        "alt_text": alt_text,
                    }),
                ));
            }
            _ => {}
        }
    }
    out
}

/// True when a legacy form field's `name` or (for a dropdown) one of its list
/// entries contains the needle.
fn form_field_matches(form: &FormFieldIdentity, needle: &str) -> bool {
    let name_hit = form_field_name(form).is_some_and(|n| n.to_lowercase().contains(needle));
    let entry_hit = match form {
        FormFieldIdentity::DropDown { entries, .. } => {
            entries.iter().any(|e| e.to_lowercase().contains(needle))
        }
        _ => false,
    };
    name_hit || entry_hit
}

fn form_field_name(form: &FormFieldIdentity) -> Option<&str> {
    match form {
        FormFieldIdentity::TextInput { name, .. }
        | FormFieldIdentity::Checkbox { name, .. }
        | FormFieldIdentity::DropDown { name, .. } => name.as_deref(),
    }
}

fn anchor_kind_str(kind: &OpaqueAnchorKind) -> String {
    match kind {
        OpaqueAnchorKind::Drawing => "image".to_string(),
        OpaqueAnchorKind::Equation => "equation".to_string(),
        OpaqueAnchorKind::Hyperlink => "hyperlink".to_string(),
        OpaqueAnchorKind::Field => "field".to_string(),
        OpaqueAnchorKind::FootnoteRef => "footnote_ref".to_string(),
        OpaqueAnchorKind::EndnoteRef => "endnote_ref".to_string(),
        OpaqueAnchorKind::Comment => "comment".to_string(),
        OpaqueAnchorKind::ContentControl => "content_control".to_string(),
        OpaqueAnchorKind::Other => "other".to_string(),
        // `OpaqueAnchorKind` is `#[non_exhaustive]`, so this downstream crate
        // must carry a fallback arm. A future kind added in stemma surfaces with
        // its debug name (`unknown:<Variant>`), distinguishable from both
        // `Other → "other"` and from each other — never silently collapsed onto
        // one label (the silent-Other pattern we kill everywhere else).
        other => format!("unknown:{other:?}"),
    }
}

fn role_label(role: &BlockRole) -> &'static str {
    match role {
        BlockRole::Paragraph => "paragraph",
        BlockRole::Heading { .. } => "heading",
        BlockRole::Table => "table",
        BlockRole::Opaque => "opaque",
    }
}

/// The detail-on-demand projection of one block: spans in order, text spans
/// carrying an ephemeral `handle` (valid against this read, guarded by the
/// block hash at write time) and their marks/status; opaque spans carrying
/// their durable anchor id.
fn block_detail_json(block: &BlockView) -> Value {
    let mut spans = Vec::new();
    for seg in &block.segments {
        match seg {
            SegmentView::Text {
                text,
                status,
                marks,
                handle,
            } => {
                // The `s_<n>` handle is assigned by the engine's authoritative
                // span enumeration (`view::enumerate_text_spans`) — the SAME
                // enumeration the write path resolves against. Surface it
                // verbatim; do NOT re-derive a counter here (a parallel counter
                // would drift from the resolver and silently mis-target).
                spans.push(json!({
                    "handle": handle.as_ref().map(|h| h.0.clone()),
                    "kind": "text",
                    "text": text,
                    "status": track_status_json(status),
                    "marks": mark_strs(marks),
                }));
            }
            SegmentView::Opaque {
                id,
                kind,
                status,
                text,
                handle,
                metadata,
            } => {
                let mut anchor = json!({
                    "kind": "anchor",
                    "id": id.to_string(),
                    "handle": handle.as_ref().map(|h| h.0.clone()),
                    "anchor_kind": anchor_kind_str(kind),
                    "status": track_status_json(status),
                    "text": text,
                });
                // `OpaqueMetadata` is a flat, self-describing object
                // (`#[serde(tag = "meta_kind")]`). Omitted entirely when the
                // kind is bare (`None`), so bare anchors add no JSON noise.
                if let Some(meta) = metadata {
                    anchor["metadata"] =
                        serde_json::to_value(meta).expect("OpaqueMetadata must serialize");
                }
                spans.push(anchor);
            }
        }
    }
    let level = match &block.role {
        BlockRole::Heading { level } => Some(*level),
        _ => None,
    };
    json!({
        "id": block.id.to_string(),
        "role": role_label(&block.role),
        "level": level,
        "opaque_label": block.opaque_label,
        "style": block.style_id,
        // The role token an `insert`/`replace` op accepts to author a NEW
        // paragraph formatted like this one (the document's private role
        // vocabulary). For a `Normal`-styled doc `style` is null but this is
        // still a usable token (e.g. "body_text"). Pass it as a v4 insert
        // block's `role`, or pass "default"/"body" for the document body role.
        "role_token": block.role_token,
        "list": list_json(block.list.as_ref()),
        "cells": cells_json(block),
        "text": block.text,
        // The typed-in enumeration label ("1.", "(a)") this paragraph carries,
        // when it has one. It is ALREADY included at the front of `text` (as
        // "{label}\t…") because Word reads it as real text — but it is NOT one of
        // the `spans` below: the label is structural and not span-addressable, so
        // a span replace targets only the body. Surfaced here so a reader can see
        // why `text` leads with a label that no span accounts for. `null` for
        // paragraphs without a literal prefix (and for auto-numbered paragraphs,
        // whose marker lives in `list`).
        "literal_prefix": block.literal_prefix,
        // The block staleness guard — a write op carries this as `guard`; if
        // the block changed since the read, the op fails loud (StaleEdit).
        "guard": block.guard,
        "block_status": track_status_json(&block.block_status),
        "spans": spans,
    })
}

/// The list/numbering membership as JSON, or `null` when absent. Surfaces
/// `num_id` + `ilvl` + ordered/bullet + the synthesized marker so the granular
/// `set_numbering` list ops are targetable. Shared by the block-row, detail, and
/// index projections so a list reads identically wherever it surfaces.
fn list_json(list: Option<&stemma::view::ListMembership>) -> Value {
    match list {
        None => Value::Null,
        Some(list) => json!({
            "num_id": list.num_id,
            "ilvl": list.ilvl,
            "ordered": list.ordered,
            "marker_text": list.marker_text,
        }),
    }
}

/// A table block's per-cell grid addressing as JSON (empty for non-table
/// blocks). Each entry is the `{row, col}` a `table_op.set_cell_text` targets
/// plus the cell's visible text, so an agent can locate "the cell containing X".
/// `block_id` is the cell's FIRST paragraph's block id (`null` for a cell with
/// no paragraph) — the id a tracked `replace` targets when `set_cell_text`'s
/// "resolve the pre-existing revision first" refusal points the agent at an
/// alternative: address the cell's own paragraph block directly instead of
/// going through the grid address.
fn cells_json(block: &BlockView) -> Vec<Value> {
    block
        .cells
        .iter()
        .map(|c| {
            json!({
                "row": c.row,
                "col": c.col,
                "text": c.text,
                "block_id": c.paragraphs.first().map(|p| p.block_id.as_str()),
            })
        })
        .collect()
}

// ─── Write receipts (the lean response contract) ───────────────────────────────
//
// Every mutation tool (apply_edit / apply_batch / accept_changes /
// reject_changes / replace_all) returns the SAME shape: a status, the revision
// ids the call created or resolved, the ids of the blocks that actually changed,
// and the read-view ROWS for those changed blocks ONLY. It never echoes the full
// document outline (the dominant measured cost: ~50KB of unrequested outline on
// every write, re-read on every subsequent turn). To see the rest of the
// document, the caller issues an explicit read (read_index / read_outline /
// read_block) — one good way, no `detail` knob.
//
// `changed_block_ids` is computed by comparing the before/after canonical IR
// block-by-block for full structural equality (NOT the guard hash, which
// deliberately ignores pending-deleted text and formatting and would
// under-report). A block counts as changed if it was added, removed, or its
// `TrackedBlock` value differs. The apply path clones the document and mutates
// only its targets, so untouched blocks stay byte-identical and never appear.

/// The set of block ids whose `TrackedBlock` value differs between two canonical
/// snapshots, plus ids added or removed, in `after`-document order (removed ids
/// are appended after, in `before` order). Honest change detection for receipts.
fn changed_block_ids(before: &CanonDoc, after: &CanonDoc) -> Vec<String> {
    use std::collections::HashMap;
    let before_by_id: HashMap<&str, &TrackedBlock> = before
        .blocks
        .iter()
        .map(|tb| (block_id_str(tb), tb))
        .collect();
    let after_ids: std::collections::HashSet<&str> =
        after.blocks.iter().map(block_id_str).collect();

    let mut changed: Vec<String> = Vec::new();
    for tb in &after.blocks {
        let id = block_id_str(tb);
        match before_by_id.get(id) {
            // Present before: changed iff the full tracked block differs.
            Some(prev) if *prev == tb => {}
            _ => changed.push(id.to_string()),
        }
    }
    // Blocks that existed before but are gone now (e.g. a block-range delete in
    // direct mode) are genuine changes — name them too.
    for tb in &before.blocks {
        let id = block_id_str(tb);
        if !after_ids.contains(id) {
            changed.push(id.to_string());
        }
    }
    changed
}

/// The stable id of a tracked block as `&str` (paragraph / table / opaque).
fn block_id_str(tb: &TrackedBlock) -> &str {
    match &tb.block {
        BlockNode::Paragraph(p) => p.id.0.as_ref(),
        BlockNode::Table(t) => t.id.0.as_ref(),
        BlockNode::OpaqueBlock(o) => o.id.0.as_ref(),
    }
}

/// The receipt's `moves` entry for each moveFrom/moveTo pair group CREATED by
/// this transaction: the (source_id -> copy_id) pairs and the blocks
/// immediately surrounding where the run landed (`prev`/`next`, each an id +
/// short text preview). This is the in-band replacement for a whole-document
/// re-read after a move — the caller confirms placement from the receipt
/// alone, without spending a `read_outline` call.
///
/// Diff-based (before/after), like `changed_block_ids`: a `move_id` already
/// present before this transaction (an untouched pre-existing move elsewhere
/// in the document — imported, or from an earlier call) is not reported,
/// only fresh moveTo copies this call created. `changed_block_ids` /
/// `changed_blocks` already surface the moved blocks themselves (the
/// moveFrom shadow and the moveTo copy both count as changed); this adds
/// the UNCHANGED neighbor context that tells the caller WHERE the run landed,
/// which no other part of the receipt carries.
fn move_receipts(before: &CanonDoc, after: &CanonDoc) -> Vec<Value> {
    if !after.blocks.iter().any(|tb| tb.move_id.is_some()) {
        return Vec::new();
    }
    let before_ids: std::collections::HashSet<&str> =
        before.blocks.iter().map(block_id_str).collect();
    // block id -> the move_id it ALREADY carried before this transaction.
    // Fresh mints are unique per step, but an IMPORTED document's move ids
    // come from arbitrary `w:name` strings, so a collision with a
    // pre-existing group is possible — group membership below is therefore
    // decided per BLOCK (did this block acquire the marker in this call?),
    // never by the id string alone, or a collision would silently zip pairs
    // across two different moves.
    let before_move_ids: std::collections::HashMap<&str, Option<&str>> = before
        .blocks
        .iter()
        .map(|tb| (block_id_str(tb), tb.move_id.as_deref()))
        .collect();
    let freshly_marked = |tb: &TrackedBlock| -> bool {
        let id = block_id_str(tb);
        match before_move_ids.get(id) {
            Some(prior) => *prior != tb.move_id.as_deref(), // pre-existing block, marker is new
            None => true,                                   // block itself is new (a fresh copy)
        }
    };
    let outline = build_outline(&build_document_view_from_canon(after));
    let preview_by_id: std::collections::HashMap<&str, &str> = outline
        .entries
        .iter()
        .map(|e| (e.id.0.as_ref(), e.text_preview.as_str()))
        .collect();

    // Every move_id with a FRESH Inserted half (a copy this transaction
    // created), in first-appearance order.
    let mut move_ids: Vec<&str> = Vec::new();
    for tb in &after.blocks {
        let Some(move_id) = tb.move_id.as_deref() else {
            continue;
        };
        if !matches!(tb.status, TrackingStatus::Inserted(_)) {
            continue;
        }
        if before_ids.contains(block_id_str(tb)) {
            continue; // not fresh: an untouched pre-existing move
        }
        if !move_ids.contains(&move_id) {
            move_ids.push(move_id);
        }
    }

    let neighbor = |index: Option<usize>| -> Value {
        match index.and_then(|i| after.blocks.get(i)) {
            Some(tb) => {
                let id = block_id_str(tb);
                json!({
                    "id": id,
                    "text_preview": preview_by_id.get(id).copied().unwrap_or(""),
                })
            }
            None => Value::Null,
        }
    };

    move_ids
        .into_iter()
        .map(|move_id| {
            let sources: Vec<&str> = after
                .blocks
                .iter()
                .filter(|tb| {
                    tb.move_id.as_deref() == Some(move_id)
                        && matches!(tb.status, TrackingStatus::Deleted(_))
                        && freshly_marked(tb)
                })
                .map(block_id_str)
                .collect();
            let copy_positions: Vec<usize> = after
                .blocks
                .iter()
                .enumerate()
                .filter(|(_, tb)| {
                    tb.move_id.as_deref() == Some(move_id)
                        && matches!(tb.status, TrackingStatus::Inserted(_))
                        && freshly_marked(tb)
                })
                .map(|(i, _)| i)
                .collect();
            // Non-empty by construction: `move_id` was collected above from
            // an Inserted block carrying it.
            let first = *copy_positions
                .first()
                .expect("move_id has at least one Inserted copy");
            let last = *copy_positions
                .last()
                .expect("move_id has at least one Inserted copy");
            let pairs: Vec<Value> = sources
                .iter()
                .zip(
                    copy_positions
                        .iter()
                        .map(|&i| block_id_str(&after.blocks[i])),
                )
                .map(|(source_id, copy_id)| json!({ "source_id": source_id, "copy_id": copy_id }))
                .collect();
            json!({
                "move_id": move_id,
                "pairs": pairs,
                "prev": neighbor(first.checked_sub(1)),
                "next": neighbor(Some(last + 1)),
            })
        })
        .collect()
}

/// Concatenate a table row's cells' visible LIVE text (skipping `Deleted`
/// segments — the accept-all-ish reading), one entry per cell, for a receipt
/// preview. Not the render/redline projection: just enough to recognize the
/// row from the transaction that produced it.
fn row_cell_texts(row: &TableRowNode) -> Vec<String> {
    row.cells
        .iter()
        .map(|cell| {
            let mut text = String::new();
            for block in &cell.blocks {
                if let BlockNode::Paragraph(p) = block {
                    for seg in &p.segments {
                        if matches!(seg.status, TrackingStatus::Deleted(_)) {
                            continue;
                        }
                        for inline in &seg.inlines {
                            if let stemma::InlineNode::Text(t) = inline {
                                text.push_str(&t.text);
                            }
                        }
                    }
                }
            }
            text
        })
        .collect()
}

/// The receipt's `table_receipts` entry for each table a transaction
/// STRUCTURALLY changed via `table_op` (insert_row / delete_row / …): the
/// rows FRESHLY marked inserted/deleted THIS transaction, each with a
/// cell-text preview and the immediate neighbor rows' text — the same
/// in-band "confirm placement without a follow-up read" idiom
/// `move_receipts` uses for moves.
///
/// Diff-based (before/after), like `move_receipts` / `changed_block_ids`:
/// matched by TABLE block id (a `table_op` edits a table IN PLACE — the
/// table's own block id never changes) and, within a table, by ROW id
/// (`TableRowNode.id`) — NEVER by positional index (insert/delete shift
/// later rows) and never by tracking-status alone (a row that was ALREADY
/// Inserted/Deleted before this transaction — an imported pending change, or
/// one from an earlier `apply_edit` call — must not be reported as fresh).
///
/// Deviates from a literal "row_index_before for deletes": this engine never
/// removes a deleted row from the model (it stays in place with
/// `tracking_status: Deleted` until a later accept), so the AFTER array
/// index is meaningful and stable for BOTH inserted and deleted rows — there
/// is no separate "before index" to report.
fn table_receipts(before: &CanonDoc, after: &CanonDoc) -> Vec<Value> {
    use std::collections::HashMap;

    let before_tables: HashMap<&str, &TableNode> = before
        .blocks
        .iter()
        .filter_map(|tb| match &tb.block {
            BlockNode::Table(t) => Some((t.id.0.as_ref(), t.as_ref())),
            _ => None,
        })
        .collect();

    let mut receipts = Vec::new();
    for tb in &after.blocks {
        let BlockNode::Table(after_table) = &tb.block else {
            continue;
        };
        let Some(before_table) = before_tables.get(after_table.id.0.as_ref()) else {
            continue; // The table itself is new — a block insert, not a row op.
        };
        // Row id -> the row's tracking status BEFORE this transaction (only
        // rows that already carried one; absent = Normal/didn't exist yet).
        let before_status: HashMap<&str, &TrackingStatus> = before_table
            .rows
            .iter()
            .filter_map(|r| r.tracking_status.as_ref().map(|s| (r.id.0.as_ref(), s)))
            .collect();

        let mut rows_receipt: Vec<Value> = Vec::new();
        for (idx, row) in after_table.rows.iter().enumerate() {
            let status_label = match &row.tracking_status {
                Some(TrackingStatus::Inserted(_)) => "inserted",
                Some(TrackingStatus::Deleted(_)) => "deleted",
                _ => continue, // Normal (or unmarked): nothing fresh to report.
            };
            let already_pending = match before_status.get(row.id.0.as_ref()) {
                Some(TrackingStatus::Inserted(_)) => status_label == "inserted",
                Some(TrackingStatus::Deleted(_)) => status_label == "deleted",
                _ => false,
            };
            if already_pending {
                continue;
            }
            let prev = idx
                .checked_sub(1)
                .and_then(|i| after_table.rows.get(i))
                .map(row_cell_texts);
            let next = after_table.rows.get(idx + 1).map(row_cell_texts);
            rows_receipt.push(json!({
                "row_index": idx,
                "status": status_label,
                "cell_texts": row_cell_texts(row),
                "prev_row_texts": prev,
                "next_row_texts": next,
            }));
        }
        if !rows_receipt.is_empty() {
            receipts.push(json!({
                "table_id": after_table.id.0.as_ref(),
                "rows": rows_receipt,
            }));
        }
    }
    receipts
}

impl StemmaServer {
    /// Build the read-view ROWS for a specific set of block ids from the current
    /// snapshot, in document order. The lean receipt's `changed_blocks` payload:
    /// the same row shape `read_outline` emits (id, kind, role_token, list,
    /// cells, status, text, semantic_hash), but only for the blocks named. A
    /// changed id that no longer exists in the document (it was deleted) is
    /// silently absent from the rows — its id still appears in
    /// `changed_block_ids`, so the deletion is reported, just without a row to
    /// render. Returns the rows plus the snapshot's total block count.
    fn changed_block_rows(
        &self,
        doc_id: &str,
        changed_ids: &[String],
    ) -> Result<(Vec<Value>, usize), CallToolResult> {
        let want: std::collections::HashSet<&str> =
            changed_ids.iter().map(String::as_str).collect();
        let handle = DocHandle(doc_id.to_string());
        self.runtime
            .with(&handle, |snap| {
                let view = build_document_view(snap);
                let rows: Vec<Value> = view
                    .blocks
                    .iter()
                    .zip(snap.canonical.blocks.iter())
                    .filter(|(bv, _)| want.contains(bv.id.to_string().as_str()))
                    .map(|(bv, tb)| block_row(bv, tb))
                    .collect();
                (rows, snap.canonical.blocks.len())
            })
            .map_err(|e| {
                fail(
                    &format!("{:?}", e.code),
                    format!("doc not open: {}", e.message),
                )
            })
    }
}

// ─── Server ──────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct StemmaServer {
    runtime: Arc<SimpleRuntime>,
    /// Startup configuration (TTL, size cap). Immutable for the process.
    config: Config,
    /// Every `doc_id` this server has handed out from `open_docx`. The engine
    /// reports a missing handle the same way whether it was never opened or was
    /// evicted after its TTL; membership here disambiguates the two so an
    /// evicted handle yields an actionable "re-open" error instead of a generic
    /// unknown-id one. Grows by one small string per open; never pruned (an
    /// issued id that the runtime no longer holds is, by definition, evicted).
    issued_doc_ids: Arc<Mutex<HashSet<String>>>,
    // The COMPOSED router (base + read-projections + read-index + agentic),
    // built in `new()`. `#[tool_handler(router = self.tool_router)]` routes
    // every wire call through THIS field, so all merged routers are reachable.
    tool_router: ToolRouter<Self>,
}

impl StemmaServer {
    /// Construct with the documented default configuration (used by tests and
    /// embeddings that do not parse the environment).
    #[cfg(test)]
    fn new() -> Self {
        Self::with_config(Config::defaults())
    }

    fn with_config(config: Config) -> Self {
        Self {
            runtime: Arc::new(SimpleRuntime::new()),
            config,
            issued_doc_ids: Arc::new(Mutex::new(HashSet::new())),
            // Routers are composed with `+`. Each parallel stream contributes
            // its own named router; the base `tool_router()` carries the core
            // open/read/edit/save tools, `read_projections_router()` the
            // read-surface projections.
            tool_router: Self::tool_router()
                + Self::read_projections_router()
                + Self::read_index_router()
                + Self::agentic_router(),
        }
    }

    /// Upgrade an ambiguous "doc handle not found" tool error into an
    /// actionable one. The engine surfaces both "never opened" and "evicted
    /// after TTL" as `InvalidDocx: doc handle not found`; this server issued the
    /// handle, so it can tell them apart. `referenced_doc_id` is the `doc_id`
    /// argument of the call that failed (if any). Non-missing errors and calls
    /// that never named a `doc_id` pass through unchanged.
    fn attribute_missing_doc(
        &self,
        result: CallToolResult,
        referenced_doc_id: Option<&str>,
    ) -> CallToolResult {
        if result.is_error != Some(true) {
            return result;
        }
        let payload = match &result.structured_content {
            Some(v) => v,
            None => return result,
        };
        let is_missing_handle = payload.get("code").and_then(Value::as_str) == Some("InvalidDocx")
            && payload
                .get("error")
                .and_then(Value::as_str)
                .is_some_and(|m| m.contains("doc handle not found"));
        if !is_missing_handle {
            return result;
        }
        let Some(doc_id) = referenced_doc_id else {
            return result;
        };
        let issued = self
            .issued_doc_ids
            .lock()
            .expect("issued_doc_ids mutex poisoned")
            .contains(doc_id);
        if issued {
            fail_json(json!({
                "code": "doc_evicted",
                "error": format!(
                    "doc_id '{doc_id}' is no longer open: it was evicted after {} of \
                     inactivity ({ENV_DOC_TTL_SECS}={}). Re-open the document with open_docx \
                     to continue.",
                    humanize_secs(self.config.doc_ttl_secs),
                    self.config.doc_ttl_secs,
                ),
                "doc_id": doc_id,
                "ttl_secs": self.config.doc_ttl_secs,
            }))
        } else {
            fail_json(json!({
                "code": "unknown_doc_id",
                "error": format!(
                    "no open document has doc_id '{doc_id}'. Open the file first with \
                     open_docx, which returns the doc_id to use."
                ),
                "doc_id": doc_id,
            }))
        }
    }

    /// Build the read-view outline for an open document from its current
    /// in-memory snapshot (reflects any edits already applied).
    fn outline(&self, doc_id: &str) -> Result<Vec<Value>, CallToolResult> {
        let handle = DocHandle(doc_id.to_string());
        self.runtime
            .with(&handle, |snap| {
                // Build the read view once so each outline row carries the
                // discoverability fields (role_token / list / cells). The view's
                // blocks are 1:1 with `snap.canonical.blocks` in document order
                // (view::build_document_view_from_canon maps each block), so the
                // zip is faithful.
                let view = build_document_view(snap);
                view.blocks
                    .iter()
                    .zip(snap.canonical.blocks.iter())
                    .map(|(bv, tb)| block_row(bv, tb))
                    .collect::<Vec<_>>()
            })
            .map_err(|e| {
                fail(
                    &format!("{:?}", e.code),
                    format!("doc not open: {}", e.message),
                )
            })
    }
}

#[tool_router]
impl StemmaServer {
    #[tool(
        description = "Open a .docx file into the engine. Returns a doc_id, block_count, \
                       total_chars, server_version, and a COMPACT structural index: one \
                       lightweight row per block (id, index, role, heading depth, a 120-char \
                       text preview, char/byte length, tracked status, role_token, list \
                       membership). This is the navigation tier — scan it for the block ids to \
                       edit, then read_block / read_outline for full text + semantic_hash, or \
                       read_markdown to understand the document. open_docx deliberately does \
                       NOT echo full block text (that would overflow tool-result limits on a \
                       large document)."
    )]
    async fn open_docx(&self, Parameters(args): Parameters<OpenArgs>) -> CallToolResult {
        // Enforce the size cap on the file's metadata BEFORE reading it into
        // memory, so an oversized file is refused without ever being buffered.
        if self.config.max_doc_bytes > 0 {
            match std::fs::metadata(&args.path) {
                Ok(meta) if meta.len() > self.config.max_doc_bytes => {
                    return fail_json(json!({
                        "code": "doc_too_large",
                        "error": format!(
                            "'{}' is {} bytes, over the {}-byte open limit. Raise \
                             {ENV_MAX_DOC_BYTES} to open larger files (or set it to 0 to \
                             disable the cap).",
                            args.path, meta.len(), self.config.max_doc_bytes,
                        ),
                        "path": args.path,
                        "size_bytes": meta.len(),
                        "limit_bytes": self.config.max_doc_bytes,
                        "env_var": ENV_MAX_DOC_BYTES,
                    }));
                }
                Ok(_) => {}
                Err(e) => return fail("io_error", format!("cannot stat {}: {e}", args.path)),
            }
        }
        let bytes = match std::fs::read(&args.path) {
            Ok(b) => b,
            Err(e) => return fail("io_error", format!("cannot read {}: {e}", args.path)),
        };
        let import = match self.runtime.import_docx(&bytes) {
            Ok(r) => r,
            Err(e) => return fail(&format!("{:?}", e.code), e.message),
        };
        let doc_id = import.doc_handle.0.clone();
        // Record the handle we are about to hand out so a later "doc handle not
        // found" for this id can be attributed to eviction rather than a typo.
        self.issued_doc_ids
            .lock()
            .expect("issued_doc_ids mutex poisoned")
            .insert(doc_id.clone());
        // Return the COMPACT structural index (the navigation tier), not the
        // heavy per-block outline (id + role + 120-char preview + length per
        // row, vs the full row with text + semantic_hash + cells). On the
        // 102-block document the heavy outline was ~50KB and tripped the host's
        // truncation limit, spilling to a temp file the agent then had to grep
        // for its own doc_id. The
        // compact index keeps open_docx inside tool-result limits; read_outline /
        // read_block pull the heavy rows on demand.
        let handle = DocHandle(doc_id.clone());
        let outline = match self.runtime.with(&handle, |snap| {
            let view = build_document_view(snap);
            stemma::view::build_outline(&view)
        }) {
            Ok(o) => o,
            Err(e) => return fail(&format!("{:?}", e.code), e.message),
        };
        let index: Vec<Value> = outline.entries.iter().map(outline_entry_json).collect();
        // The document's origin authors (the existing redline's authors, off
        // limits to the impersonation guard) are captured by the engine
        // itself at import time — see `EditSnapshot::guard_author` /
        // `SnapshotMeta::origin_authors`. No transport-side bookkeeping
        // needed here.
        ok(json!({
            "doc_id": doc_id,
            "block_count": outline.total_blocks,
            "total_chars": outline.total_chars,
            "server_version": SERVER_VERSION,
            "index": index,
        }))
    }

    #[tool(
        description = "Re-read the current outline of an open document. One row per block: \
                       id, kind, style, role_token (the role an insert/replace accepts to \
                       author a paragraph like this — present even when style is null), list \
                       ({num_id, ilvl, ordered, marker_text} for list paragraphs, else null), \
                       cells ([{row, col, text}] for table blocks), status, text, semantic_hash. \
                       Reflects edits already applied. Call this after an edit fails as stale."
    )]
    async fn read_outline(&self, Parameters(args): Parameters<ReadArgs>) -> CallToolResult {
        match self.outline(&args.doc_id) {
            Ok(outline) => ok(json!({
                "doc_id": args.doc_id,
                "block_count": outline.len(),
                "blocks": outline,
            })),
            Err(err_result) => err_result,
        }
    }

    #[tool(
        description = "Read the document as extended markdown: honest, id-bearing tagged prose \
                       for comprehension. Reads like a contract, but each block carries its \
                       stable id (#p_7 role=para), opaque objects appear as addressable anchor \
                       tokens (<fn id=..>, <field id=..>, <link id=..>), tracked changes show as \
                       <ins>/<del>, and meaningful marks as <b>/<i>/<u>/<s>. This is the surface \
                       to UNDERSTAND a document; target edits by the block and anchor ids it shows."
    )]
    async fn read_markdown(&self, Parameters(args): Parameters<ReadArgs>) -> CallToolResult {
        let handle = DocHandle(args.doc_id.clone());
        match self.runtime.with(&handle, |snap| {
            let view = stemma::view::build_document_view(snap);
            stemma::extended_markdown::to_extended_markdown(&view)
        }) {
            Ok(markdown) => ok(json!({ "doc_id": args.doc_id, "markdown": markdown })),
            Err(e) => fail(
                &format!("{:?}", e.code),
                format!("doc not open: {}", e.message),
            ),
        }
    }

    #[tool(
        description = "Inspect one block in detail before editing it: returns its spans in \
                       order as structured JSON. Text spans carry a `handle` (s_0, s_1, ...) \
                       and their marks and tracked status; opaque spans carry their durable \
                       anchor id. Also returns `role_token` (the role an insert/replace accepts \
                       for a paragraph like this), `list` ({num_id, ilvl, ordered, marker_text} \
                       for a list paragraph, else null), and `cells` ([{row, col, text}] for a \
                       table block — the address set_cell_text targets). This is where \
                       run/formatting detail lives — read it before changing formatting, \
                       authoring an insert, targeting a span, a list op, or a table cell."
    )]
    async fn read_block(&self, Parameters(args): Parameters<ReadBlockArgs>) -> CallToolResult {
        let handle = DocHandle(args.doc_id.clone());
        let target = args.block_id.clone();
        let result = self.runtime.with(&handle, move |snap| {
            let view = build_document_view(snap);
            view.blocks
                .iter()
                .find(|b| b.id.to_string() == target)
                .map(block_detail_json)
        });
        match result {
            Ok(Some(detail)) => ok(detail),
            Ok(None) => fail(
                "AnchorNotFound",
                format!("block '{}' not found", args.block_id),
            ),
            Err(e) => fail(
                &format!("{:?}", e.code),
                format!("doc not open: {}", e.message),
            ),
        }
    }

    #[tool(
        description = "Find blocks whose visible text contains `pattern` (case-insensitive). \
                       Returns matching block ids with role, text, role_token, list membership, \
                       and (for table matches) the table's cells ([{row, col, text}]) so a \
                       phrase inside a table resolves to a cell address for set_cell_text. Use \
                       this when you know the wording but not the block id."
    )]
    async fn find(&self, Parameters(args): Parameters<FindArgs>) -> CallToolResult {
        let handle = DocHandle(args.doc_id.clone());
        let needle = args.pattern.to_lowercase();
        let result = self.runtime.with(&handle, move |snap| {
            let view = build_document_view(snap);
            let mut matches = Vec::new();
            for b in &view.blocks {
                // Common block fields, shared by a text match and any anchor
                // match in this block.
                let base = |matched_in: &str| {
                    json!({
                        "id": b.id.to_string(),
                        "role": role_label(&b.role),
                        "text": b.text,
                        // So a phrase found inside a table resolves to a cell
                        // address, and a phrase in a list paragraph carries its
                        // list membership.
                        "role_token": b.role_token,
                        "list": list_json(b.list.as_ref()),
                        "cells": cells_json(b),
                        "matched_in": matched_in,
                    })
                };
                // Existing behavior, unchanged except for the additive
                // `matched_in: "text"` tag.
                if b.text.to_lowercase().contains(&needle) {
                    matches.push(base("text"));
                }
                // New: match the needle against each opaque anchor's surfaced
                // metadata (tag/alias/value/field result/ffData name/dropdown
                // entries/image alt). A hit reports WHERE it matched and the
                // anchor id, so the agent can feed it to a write verb.
                for (matched_in, anchor) in opaque_metadata_matches(b, &needle) {
                    let mut row = base(matched_in);
                    row["anchor"] = anchor;
                    matches.push(row);
                }
            }
            matches
        });
        match result {
            Ok(matches) => ok(json!({
                "pattern": args.pattern, "count": matches.len(), "matches": matches,
            })),
            Err(e) => fail(
                &format!("{:?}", e.code),
                format!("doc not open: {}", e.message),
            ),
        }
    }

    #[tool(
        description = "Read one section as extended markdown: the heading with the given id and \
                       the blocks under it, up to the next heading of the same or higher level. \
                       Use for windowed reading of a large document instead of read_markdown."
    )]
    async fn get_section(&self, Parameters(args): Parameters<SectionArgs>) -> CallToolResult {
        let handle = DocHandle(args.doc_id.clone());
        let heading_id = args.heading_id.clone();
        // Closure returns Result<Value, String>: Err is an actionable tool error.
        let result = self.runtime.with(&handle, move |snap| {
            let view = build_document_view(snap);
            let blocks = &view.blocks;
            let Some(start) = blocks.iter().position(|b| b.id.to_string() == heading_id) else {
                return Err(format!("heading '{heading_id}' not found"));
            };
            let level = match &blocks[start].role {
                BlockRole::Heading { level } => *level,
                other => {
                    return Err(format!(
                        "block '{heading_id}' is a {}, not a heading",
                        role_label(other)
                    ));
                }
            };
            let mut end = start + 1;
            while end < blocks.len() {
                if let BlockRole::Heading { level: l } = &blocks[end].role
                    && *l <= level
                {
                    break;
                }
                end += 1;
            }
            Ok(json!({
                "heading_id": heading_id,
                "block_count": end - start,
                "markdown": to_extended_markdown_blocks(&blocks[start..end]),
            }))
        });
        match result {
            Ok(Ok(payload)) => ok(payload),
            Ok(Err(msg)) => fail("not_found", msg),
            Err(e) => fail(
                &format!("{:?}", e.code),
                format!("doc not open: {}", e.message),
            ),
        }
    }

    #[tool(
        description = "Apply a v4 edit transaction to an open document. By default the edit is \
                       written as tracked changes (w:ins/w:del) a reviewer can accept/reject; \
                       pass mode='direct' to apply it immediately with NO tracked markup (the \
                       change is baked straight into the text). Atomic: either every op applies \
                       or none do. Preconditions (expect / semantic_hash) fail loudly on a stale \
                       edit rather than corrupting the wrong block; an op that would change \
                       NOTHING fails loudly too (code NoOpEdit) rather than silently reporting \
                       success. Returns a LEAN receipt: {applied, revision_ids (the new \
                       revisions this transaction created), changed_block_ids, changed_blocks \
                       (read-view rows for ONLY the blocks that changed), block_count, moves \
                       (for any move op(s): the source_id->copy_id pairs and the prev/next \
                       block at the destination, so you can confirm placement without a \
                       re-read), server_version}. It does NOT echo the whole document — call read_index / \
                       read_outline / read_block to see the rest. The error payload is \
                       actionable: re-read the outline and retry."
    )]
    async fn apply_edit(&self, Parameters(args): Parameters<ApplyEditArgs>) -> CallToolResult {
        let txn_json = args.transaction.to_json_string();
        // Resolve any image `path` alternative to `bytes_base64` before parsing.
        let txn_json = match resolve_image_paths(&txn_json) {
            Ok(s) => s,
            Err(f) => return f,
        };

        // Parse + schema-validate at the edge (parse_transaction does both).
        let v4 = match parse_transaction(&txn_json) {
            Ok(v) => v,
            Err(e) => {
                return fail(
                    "schema_error",
                    augment_schema_error(&txn_json, &e.to_string()),
                );
            }
        };
        let mut txn = match v4.into_edit_transaction() {
            Ok(t) => t,
            Err(e) => return fail("adapter_error", e.to_string()),
        };

        // Per-call mode override, parsed at the edge with no silent fallback.
        // Absent => keep whatever the transaction body declared (default tracked).
        // "direct" applies immediately with no w:ins/w:del markup.
        match parse_materialization_mode(&args.mode) {
            Ok(Some(m)) => txn.materialization_mode = m,
            Ok(None) => {}
            Err(msg) => return fail("invalid_argument", msg),
        }

        let handle = DocHandle(args.doc_id.clone());
        self.apply_edit_receipt(&handle, &txn, args.allow_existing_author)
    }

    /// Apply a transaction and build the lean write receipt: status, the
    /// revision ids the transaction created, the ids of the blocks that
    /// actually changed, and the read-view rows for THOSE blocks only (never the
    /// full outline). Shared by `apply_edit`, `apply_batch`, and `replace_all`
    /// so every creating-write tool returns one shape. On engine failure the
    /// structured `{code, error, details}` error is surfaced unchanged —
    /// including `AuthorImpersonation` from the engine's author-impersonation
    /// guard (`SimpleRuntime::apply_edit_authored`), which runs before the
    /// write is attempted.
    fn apply_edit_receipt(
        &self,
        handle: &DocHandle,
        txn: &stemma::edit::EditTransaction,
        allow_existing_author: bool,
    ) -> CallToolResult {
        // Capture the pre-edit canonical so the receipt can name exactly which
        // blocks changed (honest before/after structural diff) and which
        // revision ids are newly created.
        let before = match self
            .runtime
            .with(handle, |snap| Arc::clone(&snap.canonical))
        {
            Ok(c) => c,
            Err(e) => {
                return fail(
                    &format!("{:?}", e.code),
                    format!("doc not open: {}", e.message),
                );
            }
        };
        let max_before = stemma::max_revision_id(&before);

        match self
            .runtime
            .apply_edit_authored(handle, txn, allow_existing_author)
        {
            Ok(result) => {
                let changed = changed_block_ids(&before, &result.canonical);
                // The transaction stamps new revisions sequentially from
                // max_before+1 — but not every stamped id SURVIVES: the
                // whole-paragraph diff/normalize can drop a stamped segment,
                // leaving a gap in the range (a multi-op transaction exposed a
                // phantom id this way; the raw-range form over-reported it).
                // So enumerate the AFTER-doc's actually-present revisions with
                // the SAME walk list_revisions uses and keep the ids above
                // max_before: receipt == read surface by construction.
                let revision_ids = match self
                    .runtime
                    .with(&handle.clone(), |snap| revision_rows(&snap.canonical))
                {
                    Ok(rows) => {
                        let mut ids: Vec<u32> = rows
                            .iter()
                            .map(|r| r.revision_id)
                            .filter(|id| *id > max_before)
                            .collect();
                        ids.sort_unstable();
                        ids.dedup();
                        ids
                    }
                    Err(e) => {
                        return fail(
                            &format!("{:?}", e.code),
                            format!("doc not open after edit: {}", e.message),
                        );
                    }
                };
                let (changed_blocks, block_count) =
                    match self.changed_block_rows(&handle.0, &changed) {
                        Ok(v) => v,
                        Err(r) => return r,
                    };
                let moves = move_receipts(&before, &result.canonical);
                let table_rows_changed = table_receipts(&before, &result.canonical);
                ok(json!({
                    "applied": result.applied,
                    "doc_id": handle.0,
                    "revision_ids": revision_ids,
                    "changed_block_ids": changed,
                    "changed_blocks": changed_blocks,
                    "block_count": block_count,
                    // Neighborhood receipt for any move(s) this transaction
                    // performed — empty when none did. See `move_receipts`.
                    "moves": moves,
                    // Fresh row inserts/deletes any table_op made this
                    // transaction — empty when none did. See `table_receipts`.
                    "table_receipts": table_rows_changed,
                    "server_version": SERVER_VERSION,
                }))
            }
            // The engine's RuntimeError carries an actionable message and a code
            // (e.g. StaleEdit, OpaqueDestroyed, AnchorNotFound, NoOpEdit).
            Err(e) => fail_json(json!({
                "code": format!("{:?}", e.code),
                "error": e.message,
                "details": format!("{:?}", e.details),
            })),
        }
    }

    #[tool(
        description = "Export an open document to a .docx file at the given path, \
                       including any tracked changes applied. Returns bytes written."
    )]
    async fn save_docx(&self, Parameters(args): Parameters<SaveArgs>) -> CallToolResult {
        let handle = DocHandle(args.doc_id.clone());
        let bytes = match self.runtime.export_docx(&handle, ExportMode::Redline) {
            Ok(b) => b,
            Err(e) => return fail(&format!("{:?}", e.code), e.message),
        };
        // To-disk save gates on the built-in linker's BLOCKING_RULES: refuse to
        // persist structurally-corrupt bytes (Word would reject the file or lose
        // data) rather than discovering the corruption downstream.
        if let Err(e) = stemma::gate_serialized_bytes(&bytes, stemma::ValidatorLevel::Blocking) {
            return fail(&format!("{:?}", e.code), e.message);
        }
        if let Err(e) = std::fs::write(&args.path, &bytes) {
            return fail("io_error", format!("cannot write {}: {e}", args.path));
        }
        ok(json!({ "path": args.path, "bytes_written": bytes.len() }))
    }

    #[tool(
        description = "Compare two .docx files and write a redline .docx (the target with \
                       tracked changes relative to the base) to out_path. Returns the \
                       number of detected changes."
    )]
    async fn compare_docx(&self, Parameters(args): Parameters<CompareArgs>) -> CallToolResult {
        let base = match read_or_fail(&args.base_path) {
            Ok(b) => b,
            Err(r) => return r,
        };
        let target = match read_or_fail(&args.target_path) {
            Ok(b) => b,
            Err(r) => return r,
        };
        let (base_import, target_import) = match self.runtime.import_docx_pair(&base, &target) {
            Ok(pair) => pair,
            Err(e) => return fail(&format!("{:?}", e.code), e.message),
        };
        let meta = TransactionMeta {
            author: args.author.unwrap_or_else(|| "stemma".to_string()),
            reason: None,
            timestamp_utc: None,
        };
        let result = match self.runtime.compare_and_redline(
            &base_import.doc_handle,
            &target_import.doc_handle,
            meta,
        ) {
            Ok(r) => r,
            Err(e) => return fail(&format!("{:?}", e.code), e.message),
        };
        // Gate the redline before persisting it, same as save_docx.
        if let Err(e) =
            stemma::gate_serialized_bytes(&result.redline_bytes, stemma::ValidatorLevel::Blocking)
        {
            return fail(&format!("{:?}", e.code), e.message);
        }
        if let Err(e) = std::fs::write(&args.out_path, &result.redline_bytes) {
            return fail("io_error", format!("cannot write {}: {e}", args.out_path));
        }
        ok(json!({
            "out_path": args.out_path,
            "change_count": result.diff.changes.len(),
            "bytes_written": result.redline_bytes.len(),
        }))
    }

    #[tool(
        description = "Find every occurrence of `needle` across the document's paragraphs and \
                       replace it with `replacement` as tracked changes (w:ins/w:del). \
                       Composes one tracked paragraph rewrite per matching paragraph; opaque \
                       anchors (fields, hyperlinks, images, breaks) are preserved. Matching \
                       honors `case_sensitive` and `whole_word`; the literal `replacement` \
                       casing is always written. A needle that straddles an anchor is never \
                       half-edited: `on_barrier_match`='skip' leaves that paragraph untouched, \
                       'fail' rejects the whole operation. Paragraphs that already carry tracked \
                       changes are refused (it would fold unrelated history) — use replace_text \
                       for those, which splices the change beside the existing markup. A needle \
                       that matches nothing fails loudly (code NoOpEdit) — nothing is changed. \
                       Atomic: either every match is rewritten or none are. Returns the lean \
                       write receipt (applied, revision_ids, changed_block_ids, changed_blocks, \
                       block_count, server_version) plus match_count — NOT the whole document."
    )]
    async fn replace_all(&self, Parameters(args): Parameters<FindReplaceArgs>) -> CallToolResult {
        // Parse string enums at the edge — no silent fallback to a default for an
        // unrecognized value (CLAUDE.md "no silent fallbacks").
        let scope = match args.scope.as_str() {
            "body" => stemma::edit::FindReplaceScope::BodyOnly,
            "body_and_stories" => stemma::edit::FindReplaceScope::BodyAndStories,
            other => {
                return fail(
                    "invalid_argument",
                    format!("scope must be 'body' or 'body_and_stories', got '{other}'"),
                );
            }
        };
        let on_barrier_match = match args.on_barrier_match.as_str() {
            "skip" => stemma::edit::BarrierPolicy::Skip,
            "fail" => stemma::edit::BarrierPolicy::Fail,
            other => {
                return fail(
                    "invalid_argument",
                    format!("on_barrier_match must be 'skip' or 'fail', got '{other}'"),
                );
            }
        };

        let options = stemma::edit::FindReplaceOptions {
            needle: args.needle,
            replacement: args.replacement,
            scope,
            case_sensitive: args.case_sensitive,
            whole_word: args.whole_word,
            on_barrier_match,
        };

        // Plan against the document's CURRENT in-memory canonical snapshot.
        let handle = DocHandle(args.doc_id.clone());
        let canonical = match self.runtime.with(&handle, |snap| snap.canonical.clone()) {
            Ok(c) => c,
            Err(e) => {
                return fail(
                    &format!("{:?}", e.code),
                    format!("doc not open: {}", e.message),
                );
            }
        };

        let steps = match stemma::edit::plan_find_replace_all(&canonical, &options) {
            Ok(s) => s,
            // replace_all rewrites whole paragraphs and refuses ones that already
            // carry tracked changes (it would fold unrelated history). Point the
            // caller at replace_text, which SPLICES through tracked paragraphs.
            Err(
                e @ (stemma::edit::EditError::ParagraphContainsTrackedSegments { .. }
                | stemma::edit::EditError::BlockHasTrackedStatus { .. }),
            ) => {
                return fail(
                    "UnsupportedEdit",
                    format!(
                        "{e}; replace_all rewrites whole paragraphs and cannot touch one that \
                         already carries tracked changes — use replace_text, which splices the \
                         replacement beside the existing markup"
                    ),
                );
            }
            Err(e) => return fail("UnsupportedEdit", e.to_string()),
        };

        if steps.is_empty() {
            // True no-op: needle absent, empty, or == replacement. Fail loudly
            // rather than pretending an edit happened (CLAUDE.md "no silent
            // fallbacks"): a replace_all that matched nothing is a mistake the
            // caller should see, in the same family as accept/reject's empty
            // selector and apply_edit's NoOpEdit.
            return fail(
                "NoOpEdit",
                format!(
                    "replace_all matched nothing for needle {:?} (absent, empty, or equal to \
                     the replacement) — nothing was changed",
                    options.needle
                ),
            );
        }

        let match_count = steps.len();
        let transaction = stemma::edit::EditTransaction {
            steps,
            summary: Some("find-and-replace".to_string()),
            materialization_mode: stemma::edit::MaterializationMode::TrackedChange,
            revision: stemma::RevisionInfo {
                revision_id: 0,
                author: Some("stemma".to_string()),
                date: None,
                apply_op_id: None,
            },
        };

        // Route through the SAME lean-receipt path the v4 surface uses, then add
        // match_count (the only field specific to find-and-replace). This tool
        // has no `allow_existing_author` of its own (its author is the fixed
        // "stemma" identity), so it never opts out of the impersonation guard.
        let mut receipt = self.apply_edit_receipt(&handle, &transaction, false);
        attach_field(&mut receipt, "match_count", json!(match_count));
        receipt
    }

    #[tool(
        description = "Tracked-native find/replace: find the literal phrase `old` and replace \
                       it with `new` as a tracked change, SPLICING through paragraphs that \
                       already carry tracked changes (unlike replace_all, which refuses them). \
                       Match + write happen server-side in one call — no read_block, span \
                       handles, or guards needed. Contract: `expected_matches` defaults to 1 \
                       and the call FAILS (code MatchCountMismatch) listing each match's \
                       {block_id, excerpt} if the actual count differs, so you disambiguate in \
                       one follow-up; pass \"all\" to replace everywhere. `match_mode` \
                       \"normalize_ws\" also matches across NBSP/typographic spaces and \
                       curly/straight quotes, and the receipt reports which folding fired. A \
                       match straddling an opaque anchor or a tracked-change boundary is never \
                       half-applied: on_barrier_match \"skip\" leaves it (reported in \
                       skipped_straddles) or \"fail\" rejects the whole call. Matches BODY text \
                       only: a paragraph's numbering label (\"1.\", \"(a)\") is structural, not \
                       matchable or editable here. On a ZERO-match the error carries a \
                       `diagnosis` array explaining why and what to change — e.g. the needle \
                       only matches under normalize_ws, includes a structural numbering label \
                       to drop, spans a wall, was already applied, matches only outside your \
                       scope, or is one character off a near miss — so you fix it in one \
                       follow-up. Returns the lean write receipt plus matches, match_count, \
                       normalization_applied, and skipped_straddles."
    )]
    async fn replace_text(&self, Parameters(args): Parameters<ReplaceTextArgs>) -> CallToolResult {
        // Parse the structured/string args at the edge — no silent fallback.
        let scope = match parse_replace_text_scope(&args.scope) {
            Ok(s) => s,
            Err(msg) => return fail("invalid_argument", msg),
        };
        let expected = match parse_expected_matches(&args.expected_matches) {
            Ok(e) => e,
            Err(msg) => return fail("invalid_argument", msg),
        };
        let match_mode = match args.match_mode.as_str() {
            "exact" => stemma::edit::MatchMode::Exact,
            "normalize_ws" => stemma::edit::MatchMode::NormalizeWs,
            other => {
                return fail(
                    "invalid_argument",
                    format!("match_mode must be 'exact' or 'normalize_ws', got '{other}'"),
                );
            }
        };
        let on_barrier_match = match args.on_barrier_match.as_str() {
            "skip" => stemma::edit::BarrierPolicy::Skip,
            "fail" => stemma::edit::BarrierPolicy::Fail,
            other => {
                return fail(
                    "invalid_argument",
                    format!("on_barrier_match must be 'skip' or 'fail', got '{other}'"),
                );
            }
        };
        if args.author.trim().is_empty() {
            return fail("invalid_argument", "author must be a non-empty string");
        }

        let options = stemma::edit::ReplaceTextOptions {
            old: args.old,
            new: args.new,
            author: args.author,
            scope,
            expected,
            match_mode,
            on_barrier_match,
        };

        let handle = DocHandle(args.doc_id.clone());
        let canonical = match self.runtime.with(&handle, |snap| snap.canonical.clone()) {
            Ok(c) => c,
            Err(e) => {
                return fail(
                    &format!("{:?}", e.code),
                    format!("doc not open: {}", e.message),
                );
            }
        };

        let plan = match stemma::edit::plan_replace_text(&canonical, &options) {
            Ok(p) => p,
            Err(stemma::edit::ReplaceTextError::MatchCountMismatch {
                expected,
                actual,
                sites,
                diagnosis,
            }) => {
                // The disambiguation contract: name every match so the agent
                // can re-issue with the right scope/expected_matches in one step.
                let matches: Vec<Value> = sites
                    .iter()
                    .map(|s| json!({ "block_id": s.block_id.to_string(), "excerpt": s.excerpt }))
                    .collect();
                // On a ZERO-match, the diagnosis probes (when any fired) are
                // folded into the error message AND surfaced as the `diagnosis`
                // array so the agent fixes the call in one follow-up instead of
                // falling back to read_block/apply_edit ceremony. A genuinely
                // absent needle yields an empty diagnosis (no speculative advice).
                let base = format!(
                    "expected {} match(es) but found {actual}; pass expected_matches to \
                     confirm, narrow scope, or use \"all\"",
                    expected_matches_label(&expected)
                );
                let error = if diagnosis.is_empty() {
                    base
                } else {
                    format!("{base}. {}", diagnosis.join("; "))
                };
                return fail_json(json!({
                    "code": "MatchCountMismatch",
                    "error": error,
                    "expected": expected_matches_label(&expected),
                    "actual": actual,
                    "matches": matches,
                    "diagnosis": diagnosis,
                }));
            }
            Err(stemma::edit::ReplaceTextError::Engine(e)) => {
                return fail(&format!("{:?}", edit_error_code(&e)), e.to_string());
            }
        };

        if plan.steps.is_empty() {
            // No replaceable match (all straddled walls under skip, or zero
            // matches with expected "all"). Report honestly rather than a fake
            // success.
            return fail_json(json!({
                "code": "NoOpEdit",
                "error": "replace_text changed nothing (no replaceable match in scope)",
                "skipped_straddles": straddles_json(&plan.skipped_straddles),
            }));
        }

        let match_count = plan.matches.len();
        let normalization: Vec<&str> = plan
            .normalization_applied
            .iter()
            .map(|c| c.as_str())
            .collect();
        let matches: Vec<Value> = plan
            .matches
            .iter()
            .map(|s| json!({ "block_id": s.block_id.to_string(), "excerpt": s.excerpt }))
            .collect();
        let skipped = straddles_json(&plan.skipped_straddles);

        let transaction = stemma::edit::EditTransaction {
            steps: plan.steps,
            summary: Some("replace_text".to_string()),
            materialization_mode: stemma::edit::MaterializationMode::TrackedChange,
            revision: stemma::RevisionInfo {
                revision_id: 0,
                author: Some(options.author.clone()),
                date: None,
                apply_op_id: None,
            },
        };

        let unreached = unreached_cells_json(&canonical, &options.old, options.match_mode);
        let mut receipt =
            self.apply_edit_receipt(&handle, &transaction, args.allow_existing_author);
        attach_field(&mut receipt, "match_count", json!(match_count));
        attach_field(&mut receipt, "matches", json!(matches));
        attach_field(&mut receipt, "normalization_applied", json!(normalization));
        attach_field(&mut receipt, "skipped_straddles", skipped);
        attach_field(&mut receipt, "unreached_matches", unreached);
        receipt
    }

    #[tool(description = "Apply a WHOLE find/replace worklist in one call — the \
                       'counsel sent a list of changes' shape. Takes `replacements`: \
                       a list of {old, new, expected_matches?, scope?, match_mode?, \
                       on_barrier_match?}, each with the SAME semantics as replace_text \
                       (server-side match, tracked splice through existing redlines, no \
                       read_block/handles). Applied in order against live state, so a \
                       later item sees earlier edits. NON-ATOMIC by design: every item's \
                       outcome is reported per-item and a failure never blocks the rest — \
                       a wrong needle (MatchCountMismatch, with the per-match \
                       {block_id, excerpt} contexts + diagnosis) or a no-op is recorded \
                       under `failed` for one-shot re-issue, while the matching items \
                       still apply. Returns {applied, failed, items:[{index, old, status, \
                       match_count, changed_blocks | error, ...}]}. Use this instead of N \
                       replace_text round trips whenever you have more than one phrase to \
                       change.")]
    async fn replace_text_batch(
        &self,
        Parameters(args): Parameters<ReplaceTextBatchArgs>,
    ) -> CallToolResult {
        if args.author.trim().is_empty() {
            return fail("invalid_argument", "author must be a non-empty string");
        }
        if args.replacements.is_empty() {
            return fail("invalid_argument", "replacements must be a non-empty list");
        }

        let handle = DocHandle(args.doc_id.clone());
        let mut items: Vec<Value> = Vec::with_capacity(args.replacements.len());
        let mut applied = 0usize;
        let mut failed = 0usize;

        for (index, item) in args.replacements.into_iter().enumerate() {
            // Parse this item's options at the edge — a bad item fails only itself.
            let scope = match parse_replace_text_scope(&item.scope) {
                Ok(s) => s,
                Err(msg) => {
                    items.push(json!({"index": index, "old": item.old,
                        "status": "error", "error": msg}));
                    failed += 1;
                    continue;
                }
            };
            let expected = match parse_expected_matches(&item.expected_matches) {
                Ok(e) => e,
                Err(msg) => {
                    items.push(json!({"index": index, "old": item.old,
                        "status": "error", "error": msg}));
                    failed += 1;
                    continue;
                }
            };
            let match_mode = match item.match_mode.as_str() {
                "exact" => stemma::edit::MatchMode::Exact,
                "normalize_ws" => stemma::edit::MatchMode::NormalizeWs,
                other => {
                    items.push(json!({"index": index, "old": item.old, "status": "error",
                        "error": format!("match_mode must be 'exact' or 'normalize_ws', got '{other}'")}));
                    failed += 1;
                    continue;
                }
            };
            let on_barrier_match = match item.on_barrier_match.as_str() {
                "skip" => stemma::edit::BarrierPolicy::Skip,
                "fail" => stemma::edit::BarrierPolicy::Fail,
                other => {
                    items.push(json!({"index": index, "old": item.old, "status": "error",
                        "error": format!("on_barrier_match must be 'skip' or 'fail', got '{other}'")}));
                    failed += 1;
                    continue;
                }
            };

            let options = stemma::edit::ReplaceTextOptions {
                old: item.old.clone(),
                new: item.new,
                author: args.author.clone(),
                scope,
                expected,
                match_mode,
                on_barrier_match,
            };

            // Re-fetch the canonical each iteration so item N plans against the
            // state left by items 1..N (sequential, live).
            let canonical = match self.runtime.with(&handle, |snap| snap.canonical.clone()) {
                Ok(c) => c,
                Err(e) => {
                    return fail(
                        &format!("{:?}", e.code),
                        format!("doc not open: {}", e.message),
                    );
                }
            };

            match stemma::edit::plan_replace_text(&canonical, &options) {
                Ok(plan) if plan.steps.is_empty() => {
                    items.push(
                        json!({"index": index, "old": item.old, "status": "no_match",
                        "error": "no replaceable match in scope",
                        "skipped_straddles": straddles_json(&plan.skipped_straddles)}),
                    );
                    failed += 1;
                }
                Ok(plan) => {
                    let match_count = plan.matches.len();
                    let matched: Vec<Value> = plan
                        .matches
                        .iter()
                        .map(|s| json!({"block_id": s.block_id.to_string(), "excerpt": s.excerpt}))
                        .collect();
                    let normalization: Vec<&str> = plan
                        .normalization_applied
                        .iter()
                        .map(|c| c.as_str())
                        .collect();
                    let skipped = straddles_json(&plan.skipped_straddles);
                    let before = match self.runtime.with(&handle, |s| Arc::clone(&s.canonical)) {
                        Ok(c) => c,
                        Err(e) => {
                            return fail(
                                &format!("{:?}", e.code),
                                format!("doc not open: {}", e.message),
                            );
                        }
                    };
                    let transaction = stemma::edit::EditTransaction {
                        steps: plan.steps,
                        summary: Some("replace_text_batch".to_string()),
                        materialization_mode: stemma::edit::MaterializationMode::TrackedChange,
                        revision: stemma::RevisionInfo {
                            revision_id: 0,
                            author: Some(args.author.clone()),
                            date: None,
                            apply_op_id: None,
                        },
                    };
                    match self.runtime.apply_edit_authored(
                        &handle,
                        &transaction,
                        args.allow_existing_author,
                    ) {
                        Ok(result) => {
                            let changed = changed_block_ids(&before, &result.canonical);
                            let unreached = unreached_cells_json(&before, &item.old, match_mode);
                            items.push(
                                json!({"index": index, "old": item.old, "status": "applied",
                                "match_count": match_count, "matches": matched,
                                "changed_blocks": changed,
                                "normalization_applied": normalization,
                                "skipped_straddles": skipped,
                                "unreached_matches": unreached}),
                            );
                            applied += 1;
                        }
                        Err(e) => {
                            items.push(json!({"index": index, "old": item.old, "status": "error",
                                "error": format!("{:?}: {}", e.code, e.message)}));
                            failed += 1;
                        }
                    }
                }
                Err(stemma::edit::ReplaceTextError::MatchCountMismatch {
                    expected,
                    actual,
                    sites,
                    diagnosis,
                }) => {
                    let matched: Vec<Value> = sites
                        .iter()
                        .map(|s| json!({"block_id": s.block_id.to_string(), "excerpt": s.excerpt}))
                        .collect();
                    items.push(
                        json!({"index": index, "old": item.old, "status": "mismatch",
                        "expected": expected_matches_label(&expected), "actual": actual,
                        "matches": matched, "diagnosis": diagnosis}),
                    );
                    failed += 1;
                }
                Err(stemma::edit::ReplaceTextError::Engine(e)) => {
                    items.push(json!({"index": index, "old": item.old, "status": "error",
                        "error": format!("{:?}: {}", edit_error_code(&e), e)}));
                    failed += 1;
                }
            }
        }

        ok(json!({
            "doc_id": args.doc_id,
            "author": args.author,
            "applied": applied,
            "failed": failed,
            "items": items,
        }))
    }
}

/// The wire string for an `ExpectedMatches` (for the mismatch error).
fn expected_matches_label(e: &stemma::edit::ExpectedMatches) -> String {
    match e {
        stemma::edit::ExpectedMatches::All => "all".to_string(),
        stemma::edit::ExpectedMatches::Count(n) => n.to_string(),
    }
}

/// Table-cell occurrences of `needle` that the body-only replace_text scan did
/// NOT reach (engine finding #5), as JSON for the receipt's honesty disclosure.
/// A non-empty list means "applied N" is incomplete: the needle also lives in a
/// table cell that this verb cannot splice — fix it with set_cell_text. Returning
/// this (instead of a silent under-replace) is the receipt-honesty contract: a
/// region that was not searched is disclosed, never folded into a green result.
fn unreached_cells_json(
    doc: &stemma::CanonDoc,
    needle: &str,
    mode: stemma::edit::MatchMode,
) -> Value {
    let cells = stemma::edit::unreached_cell_matches(doc, needle, mode);
    json!(
        cells
            .iter()
            .map(|m| json!({
                "region": "table_cell",
                "block_id": m.table_id.to_string(),
                "row": m.row,
                "col": m.col,
                "excerpt": m.excerpt,
            }))
            .collect::<Vec<_>>()
    )
}

/// Skipped straddles as JSON for the receipt.
fn straddles_json(skipped: &[stemma::edit::SkippedStraddle]) -> Value {
    json!(
        skipped
            .iter()
            .map(|s| json!({ "block_id": s.block_id.to_string(), "wall": s.wall }))
            .collect::<Vec<_>>()
    )
}

/// Parse the optional structured scope arg into the engine type. Empty/None =>
/// whole doc. A single `block_id`, OR both `from_block_id`+`to_block_id`. Mixing
/// the two forms, or supplying only one range endpoint, is rejected.
fn parse_replace_text_scope(
    arg: &Option<ReplaceTextScopeArg>,
) -> Result<stemma::edit::ReplaceTextScope, String> {
    let Some(s) = arg else {
        return Ok(stemma::edit::ReplaceTextScope::WholeDoc);
    };
    match (&s.block_id, &s.from_block_id, &s.to_block_id) {
        (None, None, None) => Ok(stemma::edit::ReplaceTextScope::WholeDoc),
        (Some(id), None, None) => Ok(stemma::edit::ReplaceTextScope::SingleBlock(
            stemma::NodeId::from(id.as_str()),
        )),
        (None, Some(from), Some(to)) => Ok(stemma::edit::ReplaceTextScope::Range {
            from: stemma::NodeId::from(from.as_str()),
            to: stemma::NodeId::from(to.as_str()),
        }),
        _ => Err(
            "scope must be either { block_id } or { from_block_id, to_block_id }, not a mix \
             or a partial range"
                .to_string(),
        ),
    }
}

/// Parse `expected_matches`: absent => exactly 1; a number => exactly that many;
/// "all" => everywhere. Any other string is rejected.
fn parse_expected_matches(
    arg: &Option<ExpectedMatchesArg>,
) -> Result<stemma::edit::ExpectedMatches, String> {
    match arg {
        None => Ok(stemma::edit::ExpectedMatches::Count(1)),
        Some(ExpectedMatchesArg::Count(n)) => Ok(stemma::edit::ExpectedMatches::Count(*n)),
        Some(ExpectedMatchesArg::Keyword(k)) if k == "all" => {
            Ok(stemma::edit::ExpectedMatches::All)
        }
        Some(ExpectedMatchesArg::Keyword(other)) => Err(format!(
            "expected_matches must be a number or \"all\", got {other:?}"
        )),
    }
}

/// Merge one extra `key: value` into the structured payload of a successful
/// `CallToolResult` receipt (used to add tool-specific fields like
/// `match_count` to the shared write receipt). A no-op on error results — the
/// error payload is already complete and must not be augmented.
fn attach_field(result: &mut CallToolResult, key: &str, value: Value) {
    if result.is_error == Some(true) {
        return;
    }
    if let Some(structured) = result.structured_content.as_mut()
        && let Some(obj) = structured.as_object_mut()
    {
        obj.insert(key.to_string(), value);
    }
}

// ─── Read-surface projections (comprehension / roadmap A) ──────────────────────
//
// Pure, read-only projections of the already-materialized snapshot. None of
// these mutate the stored snapshot: `read_accepted` / `read_rejected` project a
// THROWAWAY snapshot from `snap.canonical` inside `runtime.with` and discard it
// after rendering, so a read never resolves the persisted document.

/// Max rows `list_revisions` returns. A 300-revision doc is a few KB, well under
/// this; the cap only bites a pathological doc, and when it does the response
/// reports `truncated` EXPLICITLY (CLAUDE.md: no silent cap). Sized so the full
/// table stays inside a host tool-result limit even at the cap.
const MAX_REVISION_ROWS: usize = 2000;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListRevisionsArgs {
    /// The doc_id returned by open_docx.
    doc_id: String,
    /// Optional filters, AND-combined: a row is returned only if it matches
    /// every filter you set. Omit the whole object (or leave every field null)
    /// to list all revisions.
    #[serde(default)]
    filter: Option<RevisionFilter>,
}

/// Compact-table filters for `list_revisions`. All fields are optional and
/// AND-combined (so `{by_author, by_kind}` is "this author's changes of this
/// kind"). A filter that matches nothing yields an empty `revisions` list — that
/// is a valid answer ("no such revisions"), not an error: unlike the
/// accept/reject SELECTOR (which fails loud on an empty match because resolving
/// nothing is a mistake), a READ of "no matches" is legitimately empty.
#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RevisionFilter {
    /// Keep only revisions authored by this exact author string. An anonymized
    /// revision (no `w:author` in the source) never matches an author filter —
    /// the table does not invent an author to match it.
    #[serde(default)]
    by_author: Option<String>,
    /// Keep only rows of this kind: "insert", "delete", one of the
    /// formatting-change kinds ("format_run", "format_paragraph",
    /// "format_table", "format_row", "format_cell", "format_section"), or
    /// "format" (any formatting change). Any other value fails loudly at the
    /// edge (no silent fallback).
    #[serde(default)]
    by_kind: Option<String>,
    /// Keep only revisions touching a block in the inclusive id range
    /// [from_block_id..to_block_id] in document order. Unknown endpoint =>
    /// AnchorNotFound; out-of-order endpoints => InvalidRange.
    #[serde(default)]
    by_block_range: Option<BlockRange>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct BlockRange {
    from_block_id: String,
    to_block_id: String,
}

#[tool_router(router = read_projections_router)]
impl StemmaServer {
    #[tool(
        description = "Read the document as plain text: visible run text, one U+FFFC (object \
                       replacement) per opaque anchor (image/field/footnote-ref), blocks joined \
                       by a blank line. Reads the document as it currently stands (tracked \
                       insertions and deletions both surface); use read_accepted/read_rejected \
                       to read a single resolution."
    )]
    async fn read_text(&self, Parameters(args): Parameters<ReadArgs>) -> CallToolResult {
        let handle = DocHandle(args.doc_id.clone());
        match self.runtime.with(&handle, |snap| {
            let view = build_document_view(snap);
            stemma::view::to_plain_text(&view)
        }) {
            Ok(text) => ok(json!({ "doc_id": args.doc_id, "text": text })),
            Err(e) => fail(
                &format!("{:?}", e.code),
                format!("doc not open: {}", e.message),
            ),
        }
    }

    #[tool(
        description = "Read the accept-all reading of the document: every tracked change resolved \
                       as accepted, rendered as extended markdown. This is a READ — it projects a \
                       throwaway document and discards it; the stored snapshot keeps its tracked \
                       changes intact. Use to preview 'what the document says if everything is \
                       accepted'."
    )]
    async fn read_accepted(&self, Parameters(args): Parameters<ReadArgs>) -> CallToolResult {
        self.read_resolved(&args.doc_id, Resolution::AcceptAll, "accept-all")
    }

    #[tool(
        description = "Read the reject-all reading of the document: every tracked change resolved \
                       as rejected, rendered as extended markdown. This is a READ — it projects a \
                       throwaway document and discards it; the stored snapshot is untouched. The \
                       reject-all body equals the document's baseline."
    )]
    async fn read_rejected(&self, Parameters(args): Parameters<ReadArgs>) -> CallToolResult {
        self.read_resolved(&args.doc_id, Resolution::RejectAll, "reject-all")
    }

    #[tool(
        description = "Read the current redline of the document as extended markdown with tracked \
                       changes intact: insertions appear as <ins>, deletions as <del>. This is \
                       the comprehension surface for reviewing pending changes before resolving \
                       them."
    )]
    async fn read_redline(&self, Parameters(args): Parameters<ReadArgs>) -> CallToolResult {
        let handle = DocHandle(args.doc_id.clone());
        match self.runtime.with(&handle, |snap| {
            let view = build_document_view(snap);
            stemma::extended_markdown::to_extended_markdown(&view)
        }) {
            Ok(markdown) => ok(json!({ "doc_id": args.doc_id, "markdown": markdown })),
            Err(e) => fail(
                &format!("{:?}", e.code),
                format!("doc not open: {}", e.message),
            ),
        }
    }

    #[tool(
        description = "List the document's tracked changes as a table — one row per \
                       revision: {revision_id, author, kind, block_id, excerpt (<=80 chars of the \
                       affected text), date}. The structured index for building an \
                       accept_changes/reject_changes id list; read_redline / read_markdown give \
                       the full prose for reading what the changes actually say. `kind` is \
                       \"insert\", \"delete\", or a formatting-change kind (\"format_run\", \
                       \"format_paragraph\", \"format_table\", \"format_row\", \"format_cell\", \
                       \"format_section\"); moves are not a distinct kind (they surface as \
                       insert/delete pairs). Covers every story: body, headers, footers, \
                       footnotes, endnotes, and comments (`location` says which). A stacked \
                       inserted-then-deleted span yields \
                       TWO rows (one per pending revision), matching what accept/reject resolve. \
                       Optional `filter` (AND-combined): by_author (exact; an anonymized revision \
                       never matches), by_kind (a kind name, or \"format\" for any \
                       formatting change), by_block_range \
                       {from_block_id, to_block_id} (inclusive, document order). Returns \
                       {revisions:[...], count (rows returned), total (rows matching the filter)} \
                       and, ONLY when total exceeds the row cap, an explicit `truncated` report \
                       (limit, total, omitted, advice) — the cap is never silent."
    )]
    async fn list_revisions(
        &self,
        Parameters(args): Parameters<ListRevisionsArgs>,
    ) -> CallToolResult {
        // Parse the optional kind filter at the edge — no silent fallback.
        // Accepts every RevisionKind wire name, plus "format" as the group
        // alias for all *PrChange kinds (the common "show me formatting
        // changes" query, without naming each carrier).
        enum KindFilter {
            Exact(RevisionKind),
            AnyFormat,
        }
        impl KindFilter {
            fn matches(&self, kind: RevisionKind) -> bool {
                match self {
                    KindFilter::Exact(k) => *k == kind,
                    KindFilter::AnyFormat => kind.is_format(),
                }
            }
        }
        let kind_filter = match args.filter.as_ref().and_then(|f| f.by_kind.as_deref()) {
            None => None,
            Some("format") => Some(KindFilter::AnyFormat),
            Some(other) => match RevisionKind::parse(other) {
                Some(kind) => Some(KindFilter::Exact(kind)),
                None => {
                    return fail(
                        "invalid_argument",
                        format!(
                            "by_kind must be \"insert\", \"delete\", \"format\" (any formatting \
                             change), \"format_run\", \"format_paragraph\", \"format_table\", \
                             \"format_row\", \"format_cell\", \"format_section\", or \
                             \"opaque_interior\" (tracked changes inside embedded content — \
                             visible but not individually resolvable); got {other:?}"
                        ),
                    );
                }
            },
        };

        let handle = DocHandle(args.doc_id.clone());
        let view = match self.runtime.with(&handle, build_document_view) {
            Ok(v) => v,
            Err(e) => {
                return fail(
                    &format!("{:?}", e.code),
                    format!("doc not open: {}", e.message),
                );
            }
        };

        // The block-range filter is the only one that can fail loudly: an unknown
        // or out-of-order endpoint is a caller error, not an empty result. Lower
        // it to the inclusive set of block ids in range (same contract as the
        // accept/reject ByRange selector).
        let block_range = args.filter.as_ref().and_then(|f| f.by_block_range.as_ref());
        let in_range: Option<std::collections::HashSet<String>> = match block_range {
            None => None,
            Some(BlockRange {
                from_block_id,
                to_block_id,
            }) => {
                let pos = |bid: &str| view.blocks.iter().position(|b| b.id.to_string() == bid);
                let Some(from) = pos(from_block_id) else {
                    return fail(
                        "AnchorNotFound",
                        format!(
                            "range start block '{from_block_id}' not found in doc '{}'",
                            args.doc_id
                        ),
                    );
                };
                let Some(to) = pos(to_block_id) else {
                    return fail(
                        "AnchorNotFound",
                        format!(
                            "range end block '{to_block_id}' not found in doc '{}'",
                            args.doc_id
                        ),
                    );
                };
                if from > to {
                    return fail(
                        "InvalidRange",
                        format!(
                            "range endpoints out of document order: '{from_block_id}' (#{from}) comes after '{to_block_id}' (#{to})"
                        ),
                    );
                }
                Some(
                    view.blocks[from..=to]
                        .iter()
                        .map(|b| b.id.to_string())
                        .collect(),
                )
            }
        };

        let author_filter = args.filter.as_ref().and_then(|f| f.by_author.as_deref());

        // One walk, then the AND-combined filters. revision_rows is the shared
        // enumeration; the filters here mirror the accept/reject selectors.
        let rows: Vec<RevisionRow> = match self
            .runtime
            .with(&handle, |snap| revision_rows(&snap.canonical))
        {
            Ok(rows) => rows,
            Err(e) => {
                return fail(
                    &format!("{:?}", e.code),
                    format!("doc not open: {}", e.message),
                );
            }
        };
        let rows: Vec<RevisionRow> = rows
            .into_iter()
            .filter(|r| {
                in_range
                    .as_ref()
                    .is_none_or(|set| set.contains(&r.block_id))
            })
            .filter(|r| author_filter.is_none_or(|a| r.author.as_deref() == Some(a)))
            .filter(|r| kind_filter.as_ref().is_none_or(|k| k.matches(r.kind)))
            .collect();

        let total = rows.len();
        let (emitted, truncation) = cap_revision_rows(&rows);
        let revisions: Vec<Value> = emitted.iter().map(revision_row_json).collect();

        let mut payload = json!({
            "doc_id": args.doc_id,
            "count": revisions.len(),
            "total": total,
            "revisions": revisions,
        });
        // Explicit, never silent: when the cap bites, the report rides alongside.
        if let Some(report) = truncation {
            payload["truncated"] = report;
        }
        ok(payload)
    }
}

impl StemmaServer {
    /// Project a throwaway snapshot under `resolution` and render it as extended
    /// markdown. The projection is local to this call — the stored snapshot is
    /// never resolved, so reads do not mutate state. A projection failure is
    /// surfaced as an actionable tool error (never a best-effort empty read).
    fn read_resolved(&self, doc_id: &str, resolution: Resolution, label: &str) -> CallToolResult {
        let handle = DocHandle(doc_id.to_string());
        // The closure returns Result<String, RuntimeError>: the inner Err is a
        // projection failure (fail loud), the outer Err is "doc not open".
        let rendered = self.runtime.with(&handle, move |snap| {
            let projected = snap.project(resolution)?;
            let view = build_document_view(&projected);
            Ok::<String, stemma::RuntimeError>(stemma::extended_markdown::to_extended_markdown(
                &view,
            ))
        });
        match rendered {
            Ok(Ok(markdown)) => ok(json!({
                "doc_id": doc_id, "resolution": label, "markdown": markdown,
            })),
            Ok(Err(e)) => fail(
                &format!("{:?}", e.code),
                format!("{label} projection failed: {}", e.message),
            ),
            Err(e) => fail(
                &format!("{:?}", e.code),
                format!("doc not open: {}", e.message),
            ),
        }
    }
}

fn read_or_fail(path: &str) -> Result<Vec<u8>, CallToolResult> {
    std::fs::read(Path::new(path)).map_err(|e| fail("io_error", format!("cannot read {path}: {e}")))
}

/// Resolve the `path` alternative to `bytes_base64` on `insert_image` /
/// `replace_image` ops at the MCP edge, so an agent can point at a file on disk
/// instead of hand-encoding base64 (which stalls MCP-only agents that have no
/// sanctioned encoder). The engine op still receives `bytes_base64` — the path
/// is read server-side and encoded here, mirroring how `open_docx` takes a path.
///
/// Contract (fail loud, no silent fallback): each image op must carry EXACTLY
/// one of `{bytes_base64, path}`. Both, or neither, is `invalid_argument`. On the
/// `path` branch the file is read and base64-encoded; `path` is then removed so
/// the engine schema (which knows only `bytes_base64`) accepts the op.
///
/// Best-effort on shape: input that is not a transaction object with an `ops`
/// array is returned unchanged for `parse_transaction` to reject with its own
/// detailed error.
fn resolve_image_paths(txn_json: &str) -> Result<String, CallToolResult> {
    use base64::Engine as _;
    let Ok(mut value) = serde_json::from_str::<Value>(txn_json) else {
        return Ok(txn_json.to_string());
    };
    let Some(ops) = value.get_mut("ops").and_then(Value::as_array_mut) else {
        return Ok(txn_json.to_string());
    };
    for (i, op) in ops.iter_mut().enumerate() {
        let Some(obj) = op.as_object_mut() else {
            continue;
        };
        let is_image = matches!(
            obj.get("op").and_then(Value::as_str),
            Some("insert_image" | "replace_image")
        );
        if !is_image {
            continue;
        }
        let has_bytes = obj.get("bytes_base64").is_some_and(|v| !v.is_null());
        let path = obj
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|p| !p.is_empty());
        match (has_bytes, path) {
            (true, Some(_)) => {
                return Err(fail(
                    "invalid_argument",
                    format!("ops[{i}]: supply exactly one of bytes_base64 or path, not both"),
                ));
            }
            (false, None) => {
                return Err(fail(
                    "invalid_argument",
                    format!(
                        "ops[{i}]: an image op needs its bytes — supply `path` (an absolute \
                         path to the image file, read server-side) or `bytes_base64` (the \
                         base64-encoded bytes)"
                    ),
                ));
            }
            (false, Some(path)) => {
                let bytes = read_or_fail(&path)?;
                let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
                obj.remove("path");
                obj.insert("bytes_base64".to_string(), Value::String(encoded));
            }
            (true, None) => {
                // Already carries bytes; nothing to resolve.
            }
        }
    }
    Ok(value.to_string())
}

// ─── Read-surface scale (roadmap A): structural index + id-range windowing + HTML ───
// Added as their own `read_index_router` (composed in `new()`), purely additive
// read-only projections — see also `replace_all`, the read-surface projections,
// and the agentic surface. None mutate the stored snapshot.

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct WindowArgs {
    /// The doc_id returned by open_docx.
    doc_id: String,
    /// First block id of the inclusive window (from read_index / read_markdown).
    from_block_id: String,
    /// Last block id of the inclusive window. Must not come before from_block_id
    /// in document order.
    to_block_id: String,
    /// Render format: "text", "markdown", or "html". Parsed with no fallback —
    /// an unrecognized value fails loudly.
    format: String,
}

/// Parse a window `format` wire string into the typed [`WindowFormat`] at the
/// edge — no silent fallback to a default for an unrecognized value (CLAUDE.md
/// "no silent fallbacks"; mirrors `replace_all`'s scope parsing).
fn parse_window_format(raw: &str) -> Result<stemma::api::WindowFormat, CallToolResult> {
    match raw {
        "text" => Ok(stemma::api::WindowFormat::Text),
        "markdown" => Ok(stemma::api::WindowFormat::Markdown),
        "html" => Ok(stemma::api::WindowFormat::Html),
        other => Err(fail(
            "invalid_argument",
            format!("format must be 'text', 'markdown', or 'html', got '{other}'"),
        )),
    }
}

/// Map a [`stemma::view::WindowError`] to the same wire codes the agentic
/// `ByRange` selector uses (AnchorNotFound / InvalidRange), so windowing and
/// selection fail-loud the same way.
fn window_error_fail(doc_id: &str, e: &stemma::view::WindowError) -> CallToolResult {
    match e {
        stemma::view::WindowError::AnchorNotFound(id) => fail(
            "AnchorNotFound",
            format!("window anchor '{id}' not found in doc '{doc_id}'"),
        ),
        stemma::view::WindowError::OutOfOrder { from, to } => fail(
            "InvalidRange",
            format!(
                "window endpoints out of document order in doc '{doc_id}': from (#{from}) comes after to (#{to})"
            ),
        ),
    }
}

/// One structural-index row as JSON. Faithful to the engine's `OutlineEntry`.
fn outline_entry_json(entry: &stemma::api::OutlineEntry) -> Value {
    json!({
        "id": entry.id.to_string(),
        "index": entry.index,
        "role": role_label(&entry.role),
        "depth": entry.depth,
        "text_preview": entry.text_preview,
        "char_len": entry.char_len,
        "byte_len": entry.byte_len,
        "block_status": track_status_json(&entry.block_status),
        // Discoverability fields, mirrored from the block view: the
        // insert-acceptable role token and list membership, so list paragraphs
        // and authoring roles are visible from the navigation tier.
        "role_token": entry.role_token,
        "list": list_json(entry.list.as_ref()),
    })
}

/// One row per authored (non-separator) footnote/endnote: `{note_id, kind,
/// text}`. Discoverability surface for `insert_note`/`edit_note`/
/// `delete_note`: before this, no read tool exposed a note's id or body text
/// at all — an agent had to unzip the docx to see footnote text, and a note
/// id was only visible via list_revisions once it already carried a
/// revision. `text` is the note body's full visible text (both stories are
/// single-paragraph in v1, so this is the whole body — no truncation, unlike
/// an outline entry's 120-char preview, since note bodies are typically
/// short).
fn notes_json(canonical: &CanonDoc) -> Vec<Value> {
    let footnotes = canonical
        .footnotes
        .iter()
        .filter(|f| matches!(f.note_type, NoteType::Normal))
        .map(|f| note_row_json("footnote", &f.id, &f.blocks));
    let endnotes = canonical
        .endnotes
        .iter()
        .filter(|e| matches!(e.note_type, NoteType::Normal))
        .map(|e| note_row_json("endnote", &e.id, &e.blocks));
    footnotes.chain(endnotes).collect()
}

fn note_row_json(kind: &str, note_id: &str, blocks: &[TrackedBlock]) -> Value {
    let text: String = blocks
        .iter()
        .map(|tb| stemma::import::extract_block_text(&tb.block))
        .collect();
    json!({
        "note_id": note_id,
        "kind": kind,
        "text": text,
    })
}

/// One `w:style` row of the faithful style-table projection as JSON.
fn style_row_json(row: &stemma::StyleRow) -> Value {
    json!({
        "style_id": row.style_id,
        "name": row.name,
        "type": row.style_type,
        "based_on": row.based_on,
        "font_family": row.font_family,
        "font_family_is_theme": row.font_family_is_theme,
        "font_size_half_points": row.font_size_half_points,
        "color": row.color,
        "bold": row.bold,
        "is_default": row.is_default,
    })
}

/// The faithful style-table projection (`EditSnapshot::style_table`) as JSON.
fn style_table_json(doc_id: &str, projection: &stemma::StyleTableProjection) -> Value {
    let styles: Vec<Value> = projection.styles.iter().map(style_row_json).collect();
    json!({
        "doc_id": doc_id,
        "doc_default": {
            "font_family": projection.doc_default.font_family,
            "font_family_is_theme": projection.doc_default.font_family_is_theme,
            "font_size_half_points": projection.doc_default.font_size_half_points,
        },
        "default_para_style_id": projection.default_para_style_id,
        "default_char_style_id": projection.default_char_style_id,
        "styles": styles,
    })
}

#[tool_router(router = read_index_router)]
impl StemmaServer {
    #[tool(
        description = "Read the document's structural index: one lightweight row per block in \
                       document order (id, index, role, heading depth, a 120-char text preview, \
                       char/byte length, tracked status, role_token, and list membership) plus \
                       total_blocks and total_chars, PLUS a `notes` array — one row per authored \
                       footnote/endnote ({note_id, kind, text}) — since a note's body is otherwise \
                       invisible to every other read tool (read_index/read_outline/read_block are \
                       body-only). Use `notes[].note_id` to target edit_note/delete_note. The \
                       navigation tier for a large document — scan this to find block ids (and \
                       list paragraphs / authoring roles) worth windowing into with read_window, \
                       without rendering the whole body."
    )]
    async fn read_index(&self, Parameters(args): Parameters<ReadArgs>) -> CallToolResult {
        let handle = DocHandle(args.doc_id.clone());
        match self.runtime.with(&handle, |snap| {
            let view = build_document_view(snap);
            let outline = stemma::view::build_outline(&view);
            let notes = notes_json(&snap.canonical);
            (outline, notes)
        }) {
            Ok((outline, notes)) => {
                let entries: Vec<Value> = outline.entries.iter().map(outline_entry_json).collect();
                ok(json!({
                    "doc_id": args.doc_id,
                    "total_blocks": outline.total_blocks,
                    "total_chars": outline.total_chars,
                    "entries": entries,
                    "notes": notes,
                }))
            }
            Err(e) => fail(
                &format!("{:?}", e.code),
                format!("doc not open: {}", e.message),
            ),
        }
    }

    #[tool(
        description = "Read the document's STYLE TABLE: a faithful, UN-resolved projection of \
                       word/styles.xml. Returns the document default run props (doc_default: the \
                       font/size unstyled body text inherits) plus one row per w:style exactly as \
                       authored — style_id, name, type, based_on, and any font/size/color/bold it \
                       LITERALLY sets (no basedOn-chain resolution). font_family_is_theme=true \
                       means the font is a theme reference (e.g. minorHAnsi), not a literal \
                       typeface. ALWAYS call this BEFORE a global re-skin (e.g. changing the body \
                       font): it tells you whether body text inherits from doc_default (use \
                       set_doc_defaults — one edit) or from a specific style (use modify_style). \
                       An absent styles.xml is the empty table; malformed styles.xml fails loudly."
    )]
    async fn read_styles(&self, Parameters(args): Parameters<ReadArgs>) -> CallToolResult {
        let handle = DocHandle(args.doc_id.clone());
        match self.runtime.with(&handle, |snap| snap.style_table()) {
            Ok(Ok(projection)) => ok(style_table_json(&args.doc_id, &projection)),
            // Inner Err: styles.xml present-but-malformed (fail loud).
            Ok(Err(e)) => fail(&format!("{:?}", e.code), e.message),
            // Outer Err: doc not open.
            Err(e) => fail(
                &format!("{:?}", e.code),
                format!("doc not open: {}", e.message),
            ),
        }
    }

    #[tool(
        description = "Read an inclusive block-id window [from_block_id..to_block_id] in the \
                       chosen format ('text', 'markdown', or 'html'). A windowed read is exactly \
                       the slice of the full read in that format. Use after read_index to read a \
                       sub-range of a large document. Unknown endpoint => AnchorNotFound; \
                       out-of-order endpoints => InvalidRange; unrecognized format fails loudly."
    )]
    async fn read_window(&self, Parameters(args): Parameters<WindowArgs>) -> CallToolResult {
        // Parse the format at the edge — no silent fallback.
        let format = match parse_window_format(&args.format) {
            Ok(f) => f,
            Err(r) => return r,
        };
        let handle = DocHandle(args.doc_id.clone());
        let from = args.from_block_id.clone();
        let to = args.to_block_id.clone();
        // The closure returns Result<String, WindowError>: the inner Err is a
        // window-addressing failure (fail loud), the outer Err is "doc not open".
        let rendered = self.runtime.with(&handle, move |snap| {
            let view = build_document_view(snap);
            let slice = stemma::view::block_range(&view, &from, &to)?;
            let out = match format {
                stemma::api::WindowFormat::Text => stemma::view::to_plain_text_blocks(slice),
                stemma::api::WindowFormat::Markdown => {
                    stemma::extended_markdown::to_extended_markdown_blocks(slice)
                }
                stemma::api::WindowFormat::Html => stemma::html::to_html_blocks(slice),
            };
            Ok::<String, stemma::view::WindowError>(out)
        });
        match rendered {
            Ok(Ok(content)) => ok(json!({
                "doc_id": args.doc_id,
                "from_block_id": args.from_block_id,
                "to_block_id": args.to_block_id,
                "format": args.format,
                "content": content,
            })),
            Ok(Err(window_err)) => window_error_fail(&args.doc_id, &window_err),
            Err(e) => fail(
                &format!("{:?}", e.code),
                format!("doc not open: {}", e.message),
            ),
        }
    }

    #[tool(
        description = "Read the whole document as HTML: every block id surfaces as an id/data-id, \
                       all text is HTML-escaped, headings map to <h1>..<h6>, tracked changes to \
                       <ins>/<del>, and each opaque anchor to exactly one addressable \
                       <span class=\"anchor\">. Honest but NOT pixel fidelity: tables and opaque \
                       blocks render as addressable placeholder <div>s carrying their text."
    )]
    async fn read_html(&self, Parameters(args): Parameters<ReadArgs>) -> CallToolResult {
        let handle = DocHandle(args.doc_id.clone());
        match self.runtime.with(&handle, |snap| {
            let view = build_document_view(snap);
            stemma::html::to_html(&view)
        }) {
            Ok(html) => ok(json!({ "doc_id": args.doc_id, "html": html })),
            Err(e) => fail(
                &format!("{:?}", e.code),
                format!("doc not open: {}", e.message),
            ),
        }
    }
}

// ─── Agentic surface (roadmap E): selective accept/reject, dry-run, validate, batch ───
// Added as their own `agentic_router` (composed in `new()`), purely additive —
// see also `replace_all` and the read-surface projections.

/// fail-loud error, never a silent no-op.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(tag = "by", rename_all = "snake_case", deny_unknown_fields)]
enum ChangeSelector {
    /// Exactly these revision ids.
    ByIds { revision_ids: Vec<u32> },
    /// Every revision authored by this exact author string. A revision whose
    /// author is anonymized (no `w:author`) is surfaced as unmatched-because-
    /// anonymous — the selector never invents an author to match it.
    ByAuthor { author: String },
    /// Every revision touching any block from `from_block_id`..=`to_block_id`
    /// in read-view order. Unknown endpoint => AnchorNotFound; out-of-order
    /// endpoints => InvalidRange.
    ByRange {
        from_block_id: String,
        to_block_id: String,
    },
    /// Every tracked change in the document (collected from the read view, one
    /// code path with the other selectors — not `Resolution::AcceptAll`).
    All,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct AcceptArgs {
    /// The doc_id returned by open_docx.
    doc_id: String,
    /// Which tracked changes to accept.
    selector: ChangeSelector,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RejectArgs {
    /// The doc_id returned by open_docx.
    doc_id: String,
    /// Which tracked changes to reject.
    selector: ChangeSelector,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CheckArgs {
    /// The doc_id returned by open_docx.
    doc_id: String,
    /// A v4 edit transaction object (same shape as apply_edit's `transaction`).
    transaction: TransactionArg,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ValidateArgs {
    /// The doc_id returned by open_docx.
    doc_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RenderSpec {
    /// Absolute output path for the rendered redline .docx.
    path: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReviewSessionArgs {
    /// The doc_id returned by open_docx.
    doc_id: String,
    /// When set, additionally materialize the session delta (baseline → now)
    /// as a tracked-changes .docx at `render.path`.
    #[serde(default)]
    render: Option<RenderSpec>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct AuditDocxArgs {
    /// Absolute path of the baseline .docx.
    before_path: String,
    /// Absolute path of the .docx to certify.
    after_path: String,
    /// When set, additionally materialize the before → after delta as a
    /// tracked-changes .docx at `render.path`.
    #[serde(default)]
    render: Option<RenderSpec>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct BatchArgs {
    /// The doc_id returned by open_docx.
    doc_id: String,
    /// A v4 edit transaction object (already atomic; a "batch" is one
    /// transaction plus a preview switch, no new grouping type).
    transaction: TransactionArg,
    /// When true, run the dry-run (check) path and return a preview outline
    /// built from the discarded canonical; nothing is persisted. When false,
    /// apply the transaction as tracked changes.
    preview: bool,
    /// Materialization mode override: "tracked" (default) or "direct". Same
    /// semantics as apply_edit's `mode`; unknown values are rejected. Applies
    /// to both the preview and the persisted apply.
    #[serde(default)]
    mode: Option<String>,
    /// Author-impersonation override (same contract as apply_edit's). Default
    /// false: a transaction whose `revision.author` already authors revisions
    /// in this document is refused.
    #[serde(default)]
    allow_existing_author: bool,
}

/// Map an `EditError` to a wire `ErrorCode` by delegating to the engine's single
/// classifier (`stemma::map_edit_error`) — one source of truth, so adding verb
/// error variants never silently drifts this mapping out of sync.
fn edit_error_code(e: &stemma::edit::EditError) -> stemma::ErrorCode {
    stemma::map_edit_error(e.clone()).code
}

impl StemmaServer {
    /// Lower a [`ChangeSelector`] to the concrete set of revision ids it names,
    /// by walking the read view of the open document. This is the ONLY place
    /// author/range selectors become id-sets; the engine's `Resolution` is left
    /// as `HashSet<u32>`.
    ///
    /// Fail-loud contract (CLAUDE.md — no silent fallbacks):
    /// - an empty/unmatched resolved set => `InvalidRange` "no tracked changes
    ///   matched selector", never a silent no-op;
    /// - `ByAuthor` against an anonymized (author = None) revision counts as
    ///   unmatched-because-anonymous — the author is never invented;
    /// - `ByRange` with an unknown endpoint => `AnchorNotFound`; out-of-order
    ///   endpoints => `InvalidRange`.
    fn resolve_revision_ids(
        &self,
        doc_id: &str,
        selector: ChangeSelector,
    ) -> Result<std::collections::HashSet<u32>, CallToolResult> {
        let handle = DocHandle(doc_id.to_string());
        // The canonical enumeration — the SAME walk list_revisions emits, so
        // selector and census never desync (no invisible ink): inline
        // segments (both legs of a stacked pair), paragraph marks, table
        // row/cell structure, cell-interior paragraphs, and formatting
        // changes. Records with revision_id 0 are not selectable and are
        // excluded from the universe: legacy pre-identity formatting snapshots,
        // AND every OpaqueInterior record (tracked changes inside verbatim
        // opaque content, reported by the census but not individually
        // resolvable). This is what keeps `All`/`ByAuthor` from handing the
        // resolver a sentinel id it would refuse.
        let records: Vec<stemma::tracked_model::RevisionRecord> = self
            .runtime
            .with(&handle, |snap| {
                stemma::tracked_model::enumerate_revisions(&snap.canonical)
            })
            .map_err(|e| {
                fail(
                    &format!("{:?}", e.code),
                    format!("doc not open: {}", e.message),
                )
            })?
            .into_iter()
            .filter(|r| r.revision_id != 0)
            .collect();

        let ids: std::collections::HashSet<u32> = match selector {
            ChangeSelector::ByIds { revision_ids } => {
                // Honor exactly the named ids that actually exist as tracked
                // changes; an id that names nothing is a no-op request and
                // must fail loud rather than silently resolve to {}.
                let present: std::collections::HashSet<u32> =
                    records.iter().map(|r| r.revision_id).collect();
                let missing: Vec<u32> = revision_ids
                    .iter()
                    .copied()
                    .filter(|id| !present.contains(id))
                    .collect();
                if !missing.is_empty() {
                    return Err(fail(
                        "InvalidRange",
                        format!(
                            "no tracked changes matched selector: revision ids {missing:?} are not present in doc '{doc_id}'"
                        ),
                    ));
                }
                revision_ids.into_iter().collect()
            }
            ChangeSelector::ByAuthor { author } => records
                .iter()
                // Anonymized revisions (author == None) are never matched by an
                // author string; we do not invent an author for them.
                .filter(|r| r.author.as_deref() == Some(author.as_str()))
                .map(|r| r.revision_id)
                .collect(),
            ChangeSelector::ByRange {
                from_block_id,
                to_block_id,
            } => {
                // Block order comes from the canonical document order.
                let order: Vec<String> = self
                    .runtime
                    .with(&handle, |snap| {
                        snap.canonical
                            .blocks
                            .iter()
                            .map(|tb| match &tb.block {
                                stemma::BlockNode::Paragraph(p) => p.id.to_string(),
                                stemma::BlockNode::Table(t) => t.id.to_string(),
                                stemma::BlockNode::OpaqueBlock(o) => o.id.to_string(),
                            })
                            .collect()
                    })
                    .map_err(|e| {
                        fail(
                            &format!("{:?}", e.code),
                            format!("doc not open: {}", e.message),
                        )
                    })?;
                let pos = |bid: &str| order.iter().position(|b| b == bid);
                let Some(from) = pos(&from_block_id) else {
                    return Err(fail(
                        "AnchorNotFound",
                        format!("range start block '{from_block_id}' not found in doc '{doc_id}'"),
                    ));
                };
                let Some(to) = pos(&to_block_id) else {
                    return Err(fail(
                        "AnchorNotFound",
                        format!("range end block '{to_block_id}' not found in doc '{doc_id}'"),
                    ));
                };
                if from > to {
                    return Err(fail(
                        "InvalidRange",
                        format!(
                            "range endpoints out of document order: '{from_block_id}' (#{from}) comes after '{to_block_id}' (#{to})"
                        ),
                    ));
                }
                let allowed: std::collections::HashSet<&str> =
                    order[from..=to].iter().map(String::as_str).collect();
                records
                    .iter()
                    .filter(|r| allowed.contains(r.block_id.to_string().as_str()))
                    .map(|r| r.revision_id)
                    .collect()
            }
            ChangeSelector::All => records.iter().map(|r| r.revision_id).collect(),
        };

        if ids.is_empty() {
            return Err(fail(
                "InvalidRange",
                format!("no tracked changes matched selector in doc '{doc_id}'"),
            ));
        }
        Ok(ids)
    }
}

#[tool_router(router = agentic_router)]
impl StemmaServer {
    #[tool(
        description = "Accept tracked changes selected by id, author, block range, or all. \
                       The selector is lowered to a concrete revision-id set against the \
                       current read view; an empty/unmatched selection fails loudly \
                       (InvalidRange) rather than silently doing nothing. Accepting only \
                       some changes leaves the rest tracked. Returns a lean receipt: \
                       accepted_revision_ids, cascaded_revision_ids, changed_block_ids, \
                       changed_blocks (rows for the changed blocks only), block_count, \
                       server_version — NOT the whole outline. \
                       Policy: by default, LAYER your tracked changes beside other authors' \
                       pending changes; only resolve (accept/reject) another author's pending \
                       change when the user's instruction calls for it (a cleanup/tighten-class \
                       task), and report what you resolved distinctly in your final summary."
    )]
    async fn accept_changes(&self, Parameters(args): Parameters<AcceptArgs>) -> CallToolResult {
        let ids = match self.resolve_revision_ids(&args.doc_id, args.selector) {
            Ok(ids) => ids,
            Err(r) => return r,
        };
        let handle = DocHandle(args.doc_id.clone());
        let before = match self
            .runtime
            .with(&handle, |snap| Arc::clone(&snap.canonical))
        {
            Ok(c) => c,
            Err(e) => {
                return fail(
                    &format!("{:?}", e.code),
                    format!("doc not open: {}", e.message),
                );
            }
        };
        match self
            .runtime
            .resolve_tracked_revisions(&handle, &ids, ResolveSelectionAction::Accept)
        {
            Ok(result) => {
                let mut accepted: Vec<u32> = ids.into_iter().collect();
                accepted.sort_unstable();
                let changed = changed_block_ids(&before, &result.canonical);
                let (changed_blocks, block_count) =
                    match self.changed_block_rows(&args.doc_id, &changed) {
                        Ok(v) => v,
                        Err(r) => return r,
                    };
                ok(json!({
                    "doc_id": args.doc_id,
                    "accepted_revision_ids": accepted,
                    // Revisions resolved as a CASCADE of this acceptance
                    // (e.g. accepting a deletion stacked over an insertion
                    // settles the insertion's claim on that range). Never
                    // silent — track these too.
                    "cascaded_revision_ids": result.cascaded_revision_ids,
                    "changed_block_ids": changed,
                    "changed_blocks": changed_blocks,
                    "block_count": block_count,
                    "server_version": SERVER_VERSION,
                }))
            }
            Err(e) => fail_json(json!({
                "code": format!("{:?}", e.code),
                "error": e.message,
            })),
        }
    }

    #[tool(
        description = "Reject tracked changes selected by id, author, block range, or all. \
                       Same selector lowering and fail-loud contract as accept_changes; \
                       rejecting only some changes leaves the rest tracked. Returns a lean \
                       receipt: rejected_revision_ids, cascaded_revision_ids, \
                       changed_block_ids, changed_blocks (rows for the changed blocks only), \
                       block_count, server_version — NOT the whole outline. \
                       Policy: by default, LAYER your tracked changes beside other authors' \
                       pending changes; only resolve (accept/reject) another author's pending \
                       change when the user's instruction calls for it (a cleanup/tighten-class \
                       task), and report what you resolved distinctly in your final summary."
    )]
    async fn reject_changes(&self, Parameters(args): Parameters<RejectArgs>) -> CallToolResult {
        let ids = match self.resolve_revision_ids(&args.doc_id, args.selector) {
            Ok(ids) => ids,
            Err(r) => return r,
        };
        let handle = DocHandle(args.doc_id.clone());
        let before = match self
            .runtime
            .with(&handle, |snap| Arc::clone(&snap.canonical))
        {
            Ok(c) => c,
            Err(e) => {
                return fail(
                    &format!("{:?}", e.code),
                    format!("doc not open: {}", e.message),
                );
            }
        };
        match self
            .runtime
            .resolve_tracked_revisions(&handle, &ids, ResolveSelectionAction::Reject)
        {
            Ok(result) => {
                let mut rejected: Vec<u32> = ids.into_iter().collect();
                rejected.sort_unstable();
                let changed = changed_block_ids(&before, &result.canonical);
                let (changed_blocks, block_count) =
                    match self.changed_block_rows(&args.doc_id, &changed) {
                        Ok(v) => v,
                        Err(r) => return r,
                    };
                ok(json!({
                    "doc_id": args.doc_id,
                    "rejected_revision_ids": rejected,
                    "cascaded_revision_ids": result.cascaded_revision_ids,
                    "changed_block_ids": changed,
                    "changed_blocks": changed_blocks,
                    "block_count": block_count,
                    "server_version": SERVER_VERSION,
                }))
            }
            Err(e) => fail_json(json!({
                "code": format!("{:?}", e.code),
                "error": e.message,
            })),
        }
    }

    #[tool(
        description = "Dry-run a v4 edit transaction WITHOUT applying it: parse + adapt it \
                       at the same edge apply_edit uses, run the verb core on a clone of the \
                       document, and discard the result. Mutates nothing. Returns \
                       {would_apply:true} if the edit would still apply cleanly, or an \
                       actionable structured error (same code apply_edit would report) if it \
                       is stale or unsupported. Use this to validate an edit before committing."
    )]
    async fn check_edit(&self, Parameters(args): Parameters<CheckArgs>) -> CallToolResult {
        let txn_json = args.transaction.to_json_string();
        let txn_json = match resolve_image_paths(&txn_json) {
            Ok(s) => s,
            Err(f) => return f,
        };
        let v4 = match parse_transaction(&txn_json) {
            Ok(v) => v,
            Err(e) => {
                return fail(
                    "schema_error",
                    augment_schema_error(&txn_json, &e.to_string()),
                );
            }
        };
        let txn = match v4.into_edit_transaction() {
            Ok(t) => t,
            Err(e) => return fail("adapter_error", e.to_string()),
        };

        let handle = DocHandle(args.doc_id.clone());
        // Run the verb core on a clone of the canonical IR and DISCARD it. The
        // `with` closure returns the dry-run Result; the cloned canonical is
        // dropped at the end of the closure, so nothing is persisted.
        let outcome = self.runtime.with(&handle, |snap| {
            stemma::edit::apply_transaction(&snap.canonical.clone(), &txn).map(|_| ())
        });
        match outcome {
            Ok(Ok(())) => ok(json!({ "doc_id": args.doc_id, "would_apply": true })),
            Ok(Err(edit_err)) => fail_json(json!({
                "code": format!("{:?}", edit_error_code(&edit_err)),
                "error": edit_err.to_string(),
                "would_apply": false,
            })),
            Err(e) => fail(
                &format!("{:?}", e.code),
                format!("doc not open: {}", e.message),
            ),
        }
    }

    #[tool(
        description = "Validate the open document: export it (redline mode) and run the \
                       package/wordprocessing/schema validators over the bytes. Returns \
                       {ok:bool, issues:[{code,message,context}]} where code is one of the \
                       known validation codes (package_invariant, wordprocessing_invariant, \
                       schema_invariant). Use after a series of edits to confirm the result \
                       is still a well-formed DOCX with no orphan tracked-change markup."
    )]
    async fn validate_docx(&self, Parameters(args): Parameters<ValidateArgs>) -> CallToolResult {
        let handle = DocHandle(args.doc_id.clone());
        let bytes = match self.runtime.export_docx(&handle, ExportMode::Redline) {
            Ok(b) => b,
            Err(e) => return fail(&format!("{:?}", e.code), e.message),
        };
        let report = stemma::api::validate(&bytes);
        let issues: Vec<Value> = report
            .issues
            .iter()
            .map(|i| {
                json!({
                    "code": validation_issue_code_str(&i.code),
                    "message": i.message,
                    "context": i.context,
                })
            })
            .collect();
        ok(json!({ "doc_id": args.doc_id, "ok": report.ok, "issues": issues }))
    }

    #[tool(
        description = "Review everything this session changed since open_docx, against the \
                       retained open-time baseline (RFC 0001). Returns the engine-derived \
                       AuditReport: session.census (new tracked changes, one row per revision, \
                       all stories), session.direct_delta (committed-content changes with NO \
                       covering tracked change — in a tracked session this being non-empty is \
                       itself a finding), preexisting (every revision already pending at open, \
                       with disposition untouched|modified|resolved; resolved rows' committed \
                       effects are annotated on direct rows via coincides_with_resolution), \
                       untouched ({verified_blocks, parts, violations} — every block outside \
                       the reported changes verified structurally identical to the baseline), \
                       and validator (the package verdict on the would-be save bytes). Run it \
                       BEFORE save_docx: review, then save iff it matches intent. Saving does \
                       not reset the baseline; re-opening does. Optional render.path \
                       additionally materializes the baseline→now delta as a tracked-changes \
                       .docx (for direct-mode or mixed sessions this is the only way to SEE \
                       the delta as a redline)."
    )]
    async fn review_session(
        &self,
        Parameters(args): Parameters<ReviewSessionArgs>,
    ) -> CallToolResult {
        let handle = DocHandle(args.doc_id.clone());
        let report = match self.runtime.review_session(&handle) {
            Ok(r) => r,
            Err(e) => return fail(&format!("{:?}", e.code), e.message),
        };
        let mut payload = audit_report_json(&report);
        payload["doc_id"] = json!(args.doc_id);
        payload["server_version"] = json!(SERVER_VERSION);
        if let Some(render) = &args.render {
            let source = match self.runtime.session_source_bytes(&handle) {
                Ok(b) => b,
                Err(e) => return fail(&format!("{:?}", e.code), e.message),
            };
            // The current document as bytes, via the same gated export the
            // save path uses — an unsaveable document cannot render either,
            // and the failure names the gate finding.
            let current = match self.runtime.export_docx(&handle, ExportMode::Redline) {
                Ok(b) => b,
                Err(e) => return fail(&format!("{:?}", e.code), e.message),
            };
            match self.render_redline_between(&source, &current, &render.path) {
                Ok(render_json) => payload["render"] = render_json,
                Err(failure) => return failure,
            }
        }
        ok(payload)
    }

    #[tool(
        description = "Certify what happened between ANY two .docx files — the edits need not \
                       have been made by stemma (another tool, a human, a raw-XML agent). \
                       Returns the same AuditReport as review_session, computed statelessly: \
                       session.census (tracked changes present in after but not before, \
                       matched by record identity — never raw id ranges), session.direct_delta \
                       (committed-content changes with no covering tracked change), \
                       preexisting (before's pending revisions with disposition \
                       untouched|modified|resolved), untouched (structural proof over \
                       everything else, all stories), validator (package verdict on \
                       after's bytes). compare_docx answers 'produce a redline'; audit_docx \
                       answers 'certify what happened' — optional render.path also writes \
                       the redline, subsuming compare_docx when set."
    )]
    async fn audit_docx(&self, Parameters(args): Parameters<AuditDocxArgs>) -> CallToolResult {
        let before = match read_or_fail(&args.before_path) {
            Ok(b) => b,
            Err(r) => return r,
        };
        let after = match read_or_fail(&args.after_path) {
            Ok(b) => b,
            Err(r) => return r,
        };
        let report = match stemma::audit(&before, &after) {
            Ok(r) => r,
            Err(e) => return fail(&format!("{:?}", e.code), e.message),
        };
        let mut payload = audit_report_json(&report);
        payload["before_path"] = json!(args.before_path);
        payload["after_path"] = json!(args.after_path);
        payload["server_version"] = json!(SERVER_VERSION);
        if let Some(render) = &args.render {
            match self.render_redline_between(&before, &after, &render.path) {
                Ok(render_json) => payload["render"] = render_json,
                Err(failure) => return failure,
            }
        }
        ok(payload)
    }

    #[tool(
        description = "Apply a v4 edit transaction with a preview switch. A v4 transaction is \
                       already atomic (all ops apply or none), so a 'batch' is just one \
                       transaction plus preview. preview=true runs the dry-run path and \
                       returns {applied:false, would_apply:true, preview_outline} built from \
                       the discarded canonical (nothing persisted); preview=false applies it \
                       as tracked changes and returns the lean write receipt (applied, \
                       revision_ids, changed_block_ids, changed_blocks, block_count, \
                       server_version) — the changed blocks only, not the whole document."
    )]
    async fn apply_batch(&self, Parameters(args): Parameters<BatchArgs>) -> CallToolResult {
        let txn_json = args.transaction.to_json_string();
        let txn_json = match resolve_image_paths(&txn_json) {
            Ok(s) => s,
            Err(f) => return f,
        };
        let v4 = match parse_transaction(&txn_json) {
            Ok(v) => v,
            Err(e) => {
                return fail(
                    "schema_error",
                    augment_schema_error(&txn_json, &e.to_string()),
                );
            }
        };
        let mut txn = match v4.into_edit_transaction() {
            Ok(t) => t,
            Err(e) => return fail("adapter_error", e.to_string()),
        };
        match parse_materialization_mode(&args.mode) {
            Ok(Some(m)) => txn.materialization_mode = m,
            Ok(None) => {}
            Err(msg) => return fail("invalid_argument", msg),
        }
        let handle = DocHandle(args.doc_id.clone());

        // The author-impersonation guard runs inside `apply_edit_receipt`
        // below, on the PERSISTING path only — a preview (dry run) returns
        // before reaching it and persists nothing, so it stays free to model
        // any author.
        if args.preview {
            // Dry-run: apply the verb core to a clone, build the outline from
            // that discarded canonical, and persist nothing.
            let outcome = self.runtime.with(&handle, |snap| {
                stemma::edit::apply_transaction(&snap.canonical.clone(), &txn).map(
                    |(canon, _pending)| {
                        let view = stemma::view::build_document_view_from_canon(&canon);
                        view.blocks
                            .iter()
                            .zip(canon.blocks.iter())
                            .map(|(bv, tb)| block_row(bv, tb))
                            .collect::<Vec<_>>()
                    },
                )
            });
            return match outcome {
                Ok(Ok(preview_outline)) => ok(json!({
                    "doc_id": args.doc_id,
                    "applied": false,
                    "would_apply": true,
                    "preview_outline": preview_outline,
                })),
                Ok(Err(edit_err)) => fail_json(json!({
                    "code": format!("{:?}", edit_error_code(&edit_err)),
                    "error": edit_err.to_string(),
                    "would_apply": false,
                })),
                Err(e) => fail(
                    &format!("{:?}", e.code),
                    format!("doc not open: {}", e.message),
                ),
            };
        }

        self.apply_edit_receipt(&handle, &txn, args.allow_existing_author)
    }
}

/// Stable wire string for each validation issue code. Exhaustive — there is no
/// catch-all "other"; a new `ValidationIssueCode` variant forces a decision here.
fn validation_issue_code_str(code: &stemma::ValidationIssueCode) -> &'static str {
    use stemma::ValidationIssueCode;
    match code {
        ValidationIssueCode::PackageInvariant => "package_invariant",
        ValidationIssueCode::WordprocessingInvariant => "wordprocessing_invariant",
        ValidationIssueCode::SchemaInvariant => "schema_invariant",
    }
}

// ─── Audit report wire shape (review_session / audit_docx, RFC 0001) ─────────

/// Serialize a row list under the report's shared cap: `{rows, total}` plus
/// an explicit `truncated` report ONLY when the cap bites — same non-silent
/// policy as `cap_revision_rows`.
fn capped_rows_json(rows: Vec<Value>, narrow_advice: &str) -> Value {
    let total = rows.len();
    if total <= MAX_REVISION_ROWS {
        return json!({ "rows": rows, "total": total });
    }
    json!({
        "rows": rows[..MAX_REVISION_ROWS],
        "total": total,
        "truncated": {
            "limit": MAX_REVISION_ROWS,
            "omitted": total - MAX_REVISION_ROWS,
            "advice": narrow_advice,
        },
    })
}

fn audit_census_row_json(r: &stemma::tracked_model::RevisionRecord) -> Value {
    json!({
        "revision_id": r.revision_id,
        "author": r.author,
        "kind": r.kind.as_str(),
        "block_id": r.block_id.to_string(),
        "excerpt": cap_excerpt(&r.excerpt),
        "date": r.date,
        "location": r.location,
    })
}

fn audit_direct_row_json(c: &stemma::audit::DirectChange) -> Value {
    json!({
        "story": c.story,
        "kind": c.kind.as_str(),
        "block_id": c.block_id.as_ref().map(|id| id.to_string()),
        "old_excerpt": c.old_excerpt.as_deref().map(cap_excerpt),
        "new_excerpt": c.new_excerpt.as_deref().map(cap_excerpt),
        "coincides_with_resolution": c.coincides_with_resolution,
    })
}

/// The full `AuditReport` wire shape shared by `review_session` and
/// `audit_docx` (RFC 0001): sections 1–4, every claim engine-derived. The
/// caller adds its identity fields (`doc_id` / paths) and optional `render`.
fn audit_report_json(report: &stemma::audit::AuditReport) -> Value {
    use stemma::audit::{RevisionDisposition, UntouchedViolationKind};

    let census: Vec<Value> = report
        .new_revisions
        .iter()
        .map(audit_census_row_json)
        .collect();
    let direct: Vec<Value> = report
        .direct_changes
        .iter()
        .map(audit_direct_row_json)
        .collect();
    let preexisting: Vec<Value> = report
        .preexisting_revisions
        .iter()
        .map(|p| {
            let mut row = audit_census_row_json(&p.record);
            let (disposition, after_excerpt) = match &p.disposition {
                RevisionDisposition::Untouched => ("untouched", None),
                RevisionDisposition::Modified { after_excerpt } => {
                    ("modified", Some(cap_excerpt(after_excerpt)))
                }
                RevisionDisposition::Resolved => ("resolved", None),
            };
            row["disposition"] = json!(disposition);
            if let Some(excerpt) = after_excerpt {
                row["after_excerpt"] = json!(excerpt);
            }
            row
        })
        .collect();
    let violations: Vec<Value> = report
        .untouched
        .violations
        .iter()
        .map(|v| {
            let mut row = json!({ "story": v.story, "detail": v.detail });
            match &v.kind {
                UntouchedViolationKind::BlockDiffers {
                    before_block_id,
                    after_block_id,
                } => {
                    row["kind"] = json!("block_differs");
                    row["before_block_id"] = json!(before_block_id.to_string());
                    row["after_block_id"] = json!(after_block_id.to_string());
                }
                UntouchedViolationKind::SequenceLengthMismatch {
                    before_remaining,
                    after_remaining,
                } => {
                    row["kind"] = json!("sequence_length_mismatch");
                    row["before_remaining"] = json!(before_remaining);
                    row["after_remaining"] = json!(after_remaining);
                }
                UntouchedViolationKind::StoryMissing => row["kind"] = json!("story_missing"),
                UntouchedViolationKind::StoryUnexpected => row["kind"] = json!("story_unexpected"),
            }
            row
        })
        .collect();
    let validator_issues: Vec<Value> = report
        .validator
        .issues
        .iter()
        .map(|i| {
            json!({
                "code": validation_issue_code_str(&i.code),
                "message": i.message,
                "context": i.context,
            })
        })
        .collect();

    json!({
        "session": {
            "census": capped_rows_json(census, "resolve or save in stages, or read list_revisions with filters for the full census"),
            "direct_delta": capped_rows_json(direct, "compare_docx renders the full delta as a redline"),
        },
        "preexisting": capped_rows_json(preexisting, "read list_revisions with filters for the full pending set"),
        "untouched": {
            "verified_blocks": report.untouched.verified_blocks,
            "parts": report.untouched.parts,
            // Violations are findings, never capped: hiding one would be the
            // exact silent under-report the audit exists to kill.
            "violations": violations,
        },
        "validator": { "ok": report.validator.ok, "issues": validator_issues },
    })
}

impl StemmaServer {
    /// Materialize base → target as a tracked-changes .docx at `out_path` —
    /// the compare_docx pipeline (import pair, diff+merge, blocking gate,
    /// write) reused by the audit tools' `render` option.
    fn render_redline_between(
        &self,
        base: &[u8],
        target: &[u8],
        out_path: &str,
    ) -> Result<Value, CallToolResult> {
        let (base_import, target_import) = self
            .runtime
            .import_docx_pair(base, target)
            .map_err(|e| fail(&format!("{:?}", e.code), e.message))?;
        let meta = TransactionMeta {
            author: "stemma".to_string(),
            reason: None,
            timestamp_utc: None,
        };
        let result = self
            .runtime
            .compare_and_redline(&base_import.doc_handle, &target_import.doc_handle, meta)
            .map_err(|e| fail(&format!("{:?}", e.code), e.message))?;
        // Gate the redline before persisting it, same as save_docx/compare_docx.
        stemma::gate_serialized_bytes(&result.redline_bytes, stemma::ValidatorLevel::Blocking)
            .map_err(|e| fail(&format!("{:?}", e.code), e.message))?;
        std::fs::write(out_path, &result.redline_bytes)
            .map_err(|e| fail("io_error", format!("cannot write {out_path}: {e}")))?;
        Ok(json!({
            "path": out_path,
            "change_count": result.diff.changes.len(),
            "bytes_written": result.redline_bytes.len(),
        }))
    }
}

// Route over the COMPOSED router stored on the instance, not the default
// `Self::tool_router()` (which is only the base inherent router). Without this
// the read-projection / read-index / agentic routers merged in `new()` are
// defined but unreachable over the wire — `list_tools`/`call_tool` would only
// ever see the base open/read/edit/save tools. See `StemmaServer::new`.
#[tool_handler(router = self.tool_router)]
impl ServerHandler for StemmaServer {
    /// Every tool call funnels through here. We (1) sweep expired documents so a
    /// long-lived host cannot grow without bound, (2) note the `doc_id` the call
    /// referenced, dispatch to the router, and (3) upgrade an ambiguous missing-
    /// handle error into an actionable "re-open" / "unknown id" one. The
    /// `#[tool_handler]` macro fills in `list_tools`/`get_tool`; only `call_tool`
    /// is overridden (the macro skips a method we define ourselves).
    async fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        if self.config.doc_ttl_secs > 0 {
            self.runtime.evict_expired(self.config.doc_ttl_secs);
        }
        let referenced_doc_id = request
            .arguments
            .as_ref()
            .and_then(|a| a.get("doc_id"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let tcc = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
        let result = self.tool_router.call(tcc).await?;
        Ok(self.attribute_missing_doc(result, referenced_doc_id.as_deref()))
    }

    fn get_info(&self) -> ServerInfo {
        // Identify as this server, not rmcp's build-env default: the handshake's
        // `serverInfo` is how a host distinguishes stemma from any other MCP
        // server it has wired up. Version tracks SERVER_VERSION (the same
        // build-stamp identity carried on every tool payload) so the handshake
        // and the responses report one version.
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("stemma-mcp", SERVER_VERSION))
            .with_instructions(
                "stemma DOCX engine. Workflow: open_docx(path) -> read block ids/text/hashes; \
             apply_edit(doc_id, v4 transaction) to make tracked changes (anchor each op to a \
             block id and pin it with `expect`); save_docx(doc_id, path) to write the result. \
             compare_docx(base, target, out) produces a redline. Edits are atomic and fail \
             loudly on stale preconditions; on a stale error, re-read the outline and retry.",
            )
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Handle CLI args before anything else: this binary is an MCP stdio server,
    // so --help/--version must print and exit cleanly, and an unrecognized
    // argument must fail loudly rather than silently starting the server (which
    // an interactive user would only see as a confusing "connection closed").
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse_cli(&args) {
        Cli::Help => {
            print!("{}", usage());
            return Ok(());
        }
        Cli::Version => {
            println!("{SERVER_VERSION}");
            return Ok(());
        }
        Cli::Bad(offending) => {
            eprintln!(
                "stemma-mcp: unrecognized argument: {offending}\n\n{}",
                usage()
            );
            std::process::exit(2);
        }
        Cli::Serve => {}
    }

    // Parse configuration at the edge: a malformed env var is a startup error,
    // never a silent fallback. Absent vars take the documented defaults.
    let config = match Config::from_env() {
        Ok(c) => c,
        Err(message) => {
            eprintln!("stemma-mcp: invalid configuration: {message}");
            std::process::exit(2);
        }
    };

    // Log to stderr so stdout stays a clean JSON-RPC channel.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "stemma_mcp=info".into()),
        )
        .init();

    tracing::info!(
        doc_ttl_secs = config.doc_ttl_secs,
        max_doc_bytes = config.max_doc_bytes,
        "stemma-mcp starting on stdio"
    );
    let service = StemmaServer::with_config(config).serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Read-tool JSON discoverability invariants: the JSON the read tools emit
    //! carries the fields a COLD agent needs (role_token, list, cells). These
    //! call the SAME projection functions the tool bodies use (`block_row`,
    //! `block_detail_json`, `outline_entry_json`) on a real parsed document, so
    //! the wire shape is what's asserted — not a re-derivation.

    use super::*;
    use stemma::api::Document;

    /// A minimal DOCX (no styles part → Normal-styled paragraphs) whose body is
    /// `body_inner`, optionally with a numbering.xml carrying numId=1 (decimal)
    /// and numId=2 (bullet).
    fn make_docx(body_inner: &str, with_numbering: bool) -> Vec<u8> {
        let document_xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
        );
        let numbering_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum><w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/><w:lvlText w:val="&#8226;"/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num><w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num></w:numbering>"#;
        let num_override = if with_numbering {
            r#"<Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/>"#
        } else {
            ""
        };
        let content_types = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>{num_override}</Types>"#
        );
        let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
        let num_rel = if with_numbering {
            r#"<Relationship Id="rId10" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/>"#
        } else {
            ""
        };
        let doc_rels = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{num_rel}</Relationships>"#
        );
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
            if with_numbering {
                zip.start_file("word/numbering.xml", opts).unwrap();
                zip.write_all(numbering_xml.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
        }
        buf
    }

    #[test]
    fn block_detail_json_carries_role_token_for_a_normal_styled_paragraph() {
        // The read_block JSON must surface a non-null role_token even when
        // `style` is null (Normal doc), so a cold agent can author an insert.
        let doc = Document::parse(&make_docx(
            r#"<w:p><w:r><w:t>Body paragraph.</w:t></w:r></w:p>"#,
            false,
        ))
        .expect("parse");
        let view = doc.read();
        let detail = block_detail_json(&view.blocks[0]);
        assert_eq!(detail["style"], Value::Null, "Normal doc → null style");
        let token = detail["role_token"]
            .as_str()
            .expect("role_token is a non-null string");
        assert!(!token.is_empty(), "role_token usable: {detail}");
        assert_eq!(
            detail["list"],
            Value::Null,
            "non-list paragraph → null list"
        );
        assert_eq!(
            detail["cells"].as_array().map(|a| a.len()),
            Some(0),
            "a paragraph has no cells"
        );
    }

    #[test]
    fn block_detail_json_carries_cell_coordinates_for_a_table() {
        // The read_block JSON of a table must carry each cell's {row, col, text}.
        let body = r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr>
            <w:tr><w:tc><w:p><w:r><w:t>A1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B1</w:t></w:r></w:p></w:tc></w:tr>
        </w:tbl>"#;
        let doc = Document::parse(&make_docx(body, false)).expect("parse table");
        let view = doc.read();
        let table = view
            .blocks
            .iter()
            .find(|b| matches!(b.role, BlockRole::Table))
            .expect("table block");
        let detail = block_detail_json(table);
        let cells = detail["cells"].as_array().expect("cells array");
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0]["row"], 0);
        assert_eq!(cells[0]["col"], 0);
        assert_eq!(cells[0]["text"], "A1");
        assert_eq!(cells[1]["col"], 1);
        assert_eq!(cells[1]["text"], "B1");
    }

    #[test]
    fn block_detail_json_carries_list_membership_for_a_numbered_paragraph() {
        // The read_block JSON of a list paragraph must carry num_id/ilvl/ordered.
        let body = r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Item one</w:t></w:r></w:p>"#;
        let doc = Document::parse(&make_docx(body, true)).expect("parse numbered");
        let view = doc.read();
        let detail = block_detail_json(&view.blocks[0]);
        let list = &detail["list"];
        assert!(!list.is_null(), "list paragraph surfaces list membership");
        assert_eq!(list["num_id"], 1);
        assert_eq!(list["ilvl"], 0);
        assert_eq!(list["ordered"], true, "decimal list is ordered");
    }

    #[test]
    fn outline_entry_json_carries_role_token_and_list() {
        // The read_index row mirrors the block view's discoverability fields.
        let body = r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="2"/></w:numPr></w:pPr><w:r><w:t>Bullet</w:t></w:r></w:p>"#;
        let doc = Document::parse(&make_docx(body, true)).expect("parse bullet");
        let view = doc.read();
        let outline = stemma::view::build_outline(&view);
        let row = outline_entry_json(&outline.entries[0]);
        assert!(row["role_token"].as_str().is_some_and(|t| !t.is_empty()));
        assert_eq!(row["list"]["num_id"], 2);
        assert_eq!(row["list"]["ordered"], false, "bullet list is not ordered");
    }

    /// Regression for the MCP bridge bug: a bare `serde_json::Value` parameter
    /// advertises the unconstrained `true` schema (no `"type"`), so a strict
    /// client bridge treats it as opaque and JSON-stringifies it — and the
    /// server then rejects every mutation call with a serde "expected struct,
    /// got string" error. The `transaction` parameter MUST advertise an object
    /// schema with the top-level v4 shape.
    #[test]
    fn apply_edit_transaction_param_advertises_an_object_schema() {
        let schema = schemars::schema_for!(ApplyEditArgs);
        let value = serde_json::to_value(&schema).expect("schema serializes");
        let txn = &value["properties"]["transaction"];

        // The exact failure mode we are guarding against: the bare `true`
        // schema. A typed object schema is a JSON object, never a boolean.
        assert!(
            !txn.is_boolean(),
            "transaction must not be the opaque `true` schema: {txn}"
        );
        assert_eq!(
            txn["type"], "object",
            "transaction must advertise an object schema, got: {txn}"
        );

        // It pins the top-level shape so a client can validate before sending.
        let required = txn["required"].as_array().expect("required array");
        assert!(
            required.iter().any(|v| v == "ops") && required.iter().any(|v| v == "revision"),
            "transaction requires ops + revision: {txn}"
        );
        assert_eq!(txn["properties"]["ops"]["type"], "array");
        assert_eq!(
            txn["properties"]["ops"]["items"]["type"], "object",
            "each op is a typed object tagged by `op`: {txn}"
        );
        assert_eq!(txn["properties"]["revision"]["type"], "object");
    }

    /// `check_edit` and `apply_batch` share the same transaction parameter and
    /// must carry the same object schema (they were equally blocked by the bug).
    #[test]
    fn check_and_batch_transaction_params_advertise_object_schemas() {
        for value in [
            serde_json::to_value(schemars::schema_for!(CheckArgs)).unwrap(),
            serde_json::to_value(schemars::schema_for!(BatchArgs)).unwrap(),
        ] {
            let txn = &value["properties"]["transaction"];
            assert_eq!(txn["type"], "object", "transaction object schema: {txn}");
        }
    }

    /// A transaction that arrives double-encoded (the object as a JSON string —
    /// what a stringifying MCP host sends) is unwrapped and parses identically
    /// to the object form. A top-level string is never a valid transaction, so
    /// the unwrap is unambiguous.
    #[test]
    fn stringified_transaction_is_unwrapped_and_parses() {
        let txn = serde_json::json!({
            "ops": [{"op": "delete", "target": "p_1"}],
            "revision": {"author": "Agent"}
        });
        let object_form = TransactionArg(txn.clone());
        let string_form = TransactionArg(Value::String(txn.to_string()));
        assert_eq!(object_form.to_json_string(), string_form.to_json_string());
        parse_transaction(&string_form.to_json_string())
            .expect("double-encoded transaction parses like the object form");
    }

    /// Encoding layers can stack (model writes a string AND a bridge
    /// stringifies it again); the unwrap handles any observed depth.
    #[test]
    fn double_stringified_transaction_unwraps_to_the_same_transaction() {
        let txn = serde_json::json!({
            "ops": [{"op": "delete", "target": "p_1"}],
            "revision": {"author": "Agent"}
        });
        let once = txn.to_string();
        let twice = serde_json::to_string(&once).expect("string re-encodes");
        let arg = TransactionArg(Value::String(twice));
        assert_eq!(arg.to_json_string(), TransactionArg(txn).to_json_string());
        parse_transaction(&arg.to_json_string()).expect("double-encoded transaction parses");
    }

    /// Every error payload names the build that produced it, so a transcript
    /// showing a failure also shows WHICH installed server it came from.
    #[test]
    fn error_payloads_carry_the_server_version() {
        let result = fail("test_code", "boom");
        let payload = result
            .structured_content
            .clone()
            .expect("fail() produces a structured payload");
        assert_eq!(payload["server_version"], SERVER_VERSION);
        assert_eq!(payload["code"], "test_code");
    }

    /// The unwrap is not a leniency loophole: a string that isn't transaction
    /// JSON still fails loudly in the authoritative parser.
    #[test]
    fn garbage_string_transaction_still_fails_loudly() {
        let arg = TransactionArg(Value::String("not a transaction".into()));
        parse_transaction(&arg.to_json_string())
            .expect_err("non-JSON string transaction is refused");
    }

    /// The `selector` parameter (a derived internally-tagged enum) was never
    /// affected by the bug — it already advertises a structured schema, not the
    /// opaque `true`. Pin that so it cannot regress into an untyped param.
    #[test]
    fn accept_changes_selector_is_typed_not_opaque() {
        let schema = schemars::schema_for!(AcceptArgs);
        let value = serde_json::to_value(&schema).unwrap();
        let selector = &value["properties"]["selector"];
        assert!(
            !selector.is_boolean(),
            "selector must be a structured schema, not the opaque `true`: {selector}"
        );
    }

    // ─── no silent field drops (CLAUDE.md: no silent fallbacks) ───────────────
    //
    // A misnamed OPTIONAL field on a tool-argument struct (camelCase habit, a
    // typo) is otherwise dropped by serde with no error, and the field's
    // documented default silently takes over. For `ReplaceTextScopeArg` that
    // default is `ReplaceTextScope::WholeDoc` — the caller's blast-radius
    // limiter vanishes and a scoped replace becomes a document-wide one. Every
    // tool-argument struct must instead refuse the call, naming the field that
    // didn't match, exactly like `parse_transaction`'s schema refusals do.

    /// The sharp edge from the bug report: a `scope` meant to restrict a
    /// replace to `p_3..p_9` but spelled with camelCase keys must fail to
    /// deserialize, not silently resolve to `(None, None, None)` ==
    /// whole-document scope.
    #[test]
    fn replace_text_scope_rejects_camel_case_typo_instead_of_defaulting_to_whole_doc() {
        let camel_case = serde_json::json!({ "fromBlockId": "p_3", "toBlockId": "p_9" });
        let err = serde_json::from_value::<ReplaceTextScopeArg>(camel_case)
            .expect_err("a misnamed scope field must be refused, not silently dropped");
        let msg = err.to_string();
        assert!(
            msg.contains("fromBlockId"),
            "the refusal must name the unrecognized field so the caller can fix it: {msg}"
        );
    }

    /// A correctly-spelled scope still deserializes and still carries the
    /// caller's range (the fix must not reject valid input).
    #[test]
    fn replace_text_scope_accepts_correctly_spelled_range() {
        let good = serde_json::json!({ "from_block_id": "p_3", "to_block_id": "p_9" });
        let scope: ReplaceTextScopeArg =
            serde_json::from_value(good).expect("correctly-spelled scope must parse");
        assert_eq!(scope.from_block_id.as_deref(), Some("p_3"));
        assert_eq!(scope.to_block_id.as_deref(), Some("p_9"));
    }

    /// A representative misnamed optional field on a DIFFERENT tool
    /// (`apply_edit`'s `mode`) must be refused the same way — the contract is
    /// struct-wide, not special-cased to the one struct in the bug report.
    #[test]
    fn apply_edit_args_rejects_misnamed_mode_field() {
        let wire = serde_json::json!({
            "doc_id": "doc_1",
            "transaction": { "ops": [], "revision": { "author": "Agent" } },
            "Mode": "direct",
        });
        let err = serde_json::from_value::<ApplyEditArgs>(wire)
            .expect_err("a misnamed 'Mode' field must be refused, not silently ignored");
        assert!(
            err.to_string().contains("Mode"),
            "the refusal must name the unrecognized field: {err}"
        );
    }

    /// The advertised JSON schema for the scope parameter carries
    /// `additionalProperties: false`, so a strict client validates a typo'd
    /// key BEFORE sending it — the guarantee the deserialize-side tests above
    /// pin from the server side.
    #[test]
    fn replace_text_scope_schema_advertises_additional_properties_false() {
        let schema = schemars::schema_for!(ReplaceTextScopeArg);
        let value = serde_json::to_value(&schema).unwrap();
        assert_eq!(
            value["additionalProperties"], false,
            "scope schema must set additionalProperties: false: {value}"
        );
    }

    /// Every tool-argument struct advertises `additionalProperties: false`, so
    /// the no-silent-field-drop guarantee holds across the whole MCP surface,
    /// not just the one struct the bug report named — and a future tool-arg
    /// struct added to this list is caught if it forgets the annotation.
    #[test]
    fn every_tool_argument_struct_schema_denies_additional_properties() {
        let schemas: Vec<(&str, Value)> = vec![
            (
                "OpenArgs",
                serde_json::to_value(schemars::schema_for!(OpenArgs)).unwrap(),
            ),
            (
                "ReadArgs",
                serde_json::to_value(schemars::schema_for!(ReadArgs)).unwrap(),
            ),
            (
                "ApplyEditArgs",
                serde_json::to_value(schemars::schema_for!(ApplyEditArgs)).unwrap(),
            ),
            (
                "ReadBlockArgs",
                serde_json::to_value(schemars::schema_for!(ReadBlockArgs)).unwrap(),
            ),
            (
                "FindArgs",
                serde_json::to_value(schemars::schema_for!(FindArgs)).unwrap(),
            ),
            (
                "SectionArgs",
                serde_json::to_value(schemars::schema_for!(SectionArgs)).unwrap(),
            ),
            (
                "SaveArgs",
                serde_json::to_value(schemars::schema_for!(SaveArgs)).unwrap(),
            ),
            (
                "FindReplaceArgs",
                serde_json::to_value(schemars::schema_for!(FindReplaceArgs)).unwrap(),
            ),
            (
                "ReplaceTextArgs",
                serde_json::to_value(schemars::schema_for!(ReplaceTextArgs)).unwrap(),
            ),
            (
                "ReplaceItem",
                serde_json::to_value(schemars::schema_for!(ReplaceItem)).unwrap(),
            ),
            (
                "ReplaceTextBatchArgs",
                serde_json::to_value(schemars::schema_for!(ReplaceTextBatchArgs)).unwrap(),
            ),
            (
                "ReplaceTextScopeArg",
                serde_json::to_value(schemars::schema_for!(ReplaceTextScopeArg)).unwrap(),
            ),
            (
                "CompareArgs",
                serde_json::to_value(schemars::schema_for!(CompareArgs)).unwrap(),
            ),
            (
                "ListRevisionsArgs",
                serde_json::to_value(schemars::schema_for!(ListRevisionsArgs)).unwrap(),
            ),
            (
                "RevisionFilter",
                serde_json::to_value(schemars::schema_for!(RevisionFilter)).unwrap(),
            ),
            (
                "BlockRange",
                serde_json::to_value(schemars::schema_for!(BlockRange)).unwrap(),
            ),
            (
                "WindowArgs",
                serde_json::to_value(schemars::schema_for!(WindowArgs)).unwrap(),
            ),
            (
                "AcceptArgs",
                serde_json::to_value(schemars::schema_for!(AcceptArgs)).unwrap(),
            ),
            (
                "RejectArgs",
                serde_json::to_value(schemars::schema_for!(RejectArgs)).unwrap(),
            ),
            (
                "CheckArgs",
                serde_json::to_value(schemars::schema_for!(CheckArgs)).unwrap(),
            ),
            (
                "ValidateArgs",
                serde_json::to_value(schemars::schema_for!(ValidateArgs)).unwrap(),
            ),
            (
                "BatchArgs",
                serde_json::to_value(schemars::schema_for!(BatchArgs)).unwrap(),
            ),
        ];
        for (name, schema) in schemas {
            assert_eq!(
                schema["additionalProperties"], false,
                "{name}'s advertised schema must deny additional properties: {schema}"
            );
        }
    }

    /// `ChangeSelector` (the internally-tagged enum behind `accept_changes` /
    /// `reject_changes`) gets the same treatment per struct-shaped variant: an
    /// EXTRA, unrecognized field alongside a correctly-spelled `author` must be
    /// refused rather than silently ignored. (A wrong/missing `author` key
    /// alone would already fail as a "missing required field" — that isn't the
    /// unknown-field contract this test pins, so `author` is present and
    /// correct here and only the extra key is wrong.)
    #[test]
    fn change_selector_rejects_extra_field_inside_a_variant() {
        let wire = serde_json::json!({ "by": "by_author", "author": "Alice", "Author": "Alice" });
        let err = serde_json::from_value::<ChangeSelector>(wire).expect_err(
            "an unrecognized extra field inside a ChangeSelector variant must be refused",
        );
        assert!(
            err.to_string().contains("Author"),
            "the refusal must name the unrecognized field: {err}"
        );
    }

    // ─── list_revisions: compact revision table ────────────────────────────────
    //
    // These assert the SHARED enumeration `revision_rows` (the one the tool body
    // calls) on a real parsed document with a mixed redline: a plain insert, a
    // plain delete, and a stacked inserted-then-deleted span, across two authors
    // and a second paragraph — so the row shape, the two-rows-per-stacked-span
    // rule, the filters, and the excerpt cap are pinned to the wire output.

    /// p_1: "Alpha " + <ins AuthorA #10 "added "> + <del AuthorB #11 "removed ">
    ///       + <ins AuthorA #1><del AuthorB #2 "contested "> (stacked) + "omega."
    /// p_2: plain (no tracked changes) — a block-range boundary to filter on.
    fn redline_docx() -> Vec<u8> {
        let body = concat!(
            r#"<w:p><w:r><w:t xml:space="preserve">Alpha </w:t></w:r>"#,
            r#"<w:ins w:id="10" w:author="AuthorA" w:date="2026-01-01T00:00:00Z"><w:r><w:t xml:space="preserve">added </w:t></w:r></w:ins>"#,
            r#"<w:del w:id="11" w:author="AuthorB" w:date="2026-02-01T00:00:00Z"><w:r><w:delText xml:space="preserve">removed </w:delText></w:r></w:del>"#,
            r#"<w:ins w:id="1" w:author="AuthorA" w:date="2026-01-01T00:00:00Z"><w:del w:id="2" w:author="AuthorB" w:date="2026-02-01T00:00:00Z"><w:r><w:delText xml:space="preserve">contested </w:delText></w:r></w:del></w:ins>"#,
            r#"<w:r><w:t>omega.</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t>Second clean paragraph.</w:t></w:r></w:p>"#,
        );
        make_docx(body, false)
    }

    #[test]
    fn revision_rows_carry_the_compact_shape_per_tracked_change() {
        let doc = Document::parse(&redline_docx()).expect("parse redline");
        let rows = revision_rows(&doc.snapshot().canonical);

        // Four pending revisions: ins #10, del #11, and the stacked span's ins #1
        // + del #2 (a stacked span is TWO rows — one per resolvable revision).
        let ids: Vec<u32> = rows.iter().map(|r| r.revision_id).collect();
        assert_eq!(
            ids,
            vec![10, 11, 1, 2],
            "all four revisions, document order: {ids:?}"
        );

        let by_id = |id: u32| rows.iter().find(|r| r.revision_id == id).unwrap();
        let ins = by_id(10);
        assert_eq!(ins.kind, RevisionKind::Insert);
        assert_eq!(ins.author.as_deref(), Some("AuthorA"));
        assert_eq!(ins.block_id, "p_1");
        assert_eq!(ins.excerpt, "added ");
        assert_eq!(ins.date.as_deref(), Some("2026-01-01T00:00:00Z"));

        let del = by_id(11);
        assert_eq!(del.kind, RevisionKind::Delete);
        assert_eq!(del.author.as_deref(), Some("AuthorB"));
        assert_eq!(del.excerpt, "removed ");

        // The stacked span: the insertion (AuthorA) and the deletion (AuthorB)
        // are independent rows, each resolvable on its own id.
        assert_eq!(by_id(1).kind, RevisionKind::Insert);
        assert_eq!(by_id(1).author.as_deref(), Some("AuthorA"));
        assert_eq!(by_id(2).kind, RevisionKind::Delete);
        assert_eq!(by_id(2).author.as_deref(), Some("AuthorB"));
    }

    #[test]
    fn revision_row_location_is_body_for_an_ordinary_body_revision_and_crosses_the_wire() {
        let doc = Document::parse(&redline_docx()).expect("parse redline");
        let rows = revision_rows(&doc.snapshot().canonical);
        let ins = rows.iter().find(|r| r.revision_id == 10).unwrap();
        assert_eq!(ins.location, StoryScope::Body);

        // The wire JSON must actually carry it — this is the field that lets
        // a caller tell a footnote-story revision apart from a body one; a
        // struct field alone, silently dropped in revision_row_json, would
        // not be observable over MCP.
        let json = revision_row_json(ins);
        assert_eq!(json.get("location"), Some(&json!("Body")));
    }

    fn footnote_docx() -> Vec<u8> {
        // A minimal doc with one footnote body carrying a tracked insertion —
        // just enough to exercise the StoryScope::Footnote branch end to end.
        let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body><w:p><w:r><w:t>Claim needing a citation</w:t></w:r><w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:footnoteReference w:id="1"/></w:r></w:p><w:sectPr/></w:body></w:document>"#;
        let footnotes_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote><w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote><w:footnote w:id="1"><w:p><w:r><w:footnoteRef/></w:r><w:r><w:t xml:space="preserve">See </w:t></w:r><w:ins w:id="201" w:author="Reviewer" w:date="2026-01-01T00:00:00Z"><w:r><w:t>Appendix C</w:t></w:r></w:ins></w:p></w:footnote></w:footnotes>"#;
        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/footnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"/></Types>"#;
        let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
        let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdFootnotes" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes" Target="footnotes.xml"/></Relationships>"#;
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
            zip.start_file("word/footnotes.xml", opts).unwrap();
            zip.write_all(footnotes_xml.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        buf
    }

    #[test]
    fn revision_row_location_names_the_footnote_and_crosses_the_wire() {
        let doc = Document::parse(&footnote_docx()).expect("parse footnote doc");
        let rows = revision_rows(&doc.snapshot().canonical);
        let ins = rows
            .iter()
            .find(|r| r.revision_id == 201)
            .expect("footnote insertion is enumerated");
        assert_eq!(
            ins.location,
            StoryScope::Footnote {
                id: "1".to_string()
            }
        );

        let json = revision_row_json(ins);
        assert_eq!(
            json.get("location"),
            Some(&json!({"Footnote": {"id": "1"}})),
            "a caller must be able to tell this revision lives in footnote 1, not the body: {json}"
        );
    }

    /// Note discoverability (read_index's `notes` section): before this, no
    /// read tool exposed a footnote/endnote's id or body text at all — an
    /// agent had to unzip the docx to see it. `notes_json` must surface the
    /// id, kind, and full body text (base + any pending tracked insertion —
    /// same "current view" contract `extract_block_text` already has).
    #[test]
    fn notes_json_lists_footnote_id_kind_and_text() {
        let doc = Document::parse(&footnote_docx()).expect("parse footnote doc");
        let notes = notes_json(&doc.snapshot().canonical);
        assert_eq!(
            notes,
            vec![json!({"note_id": "1", "kind": "footnote", "text": "See Appendix C"})],
            "exactly one authored footnote row, with its id/kind/text — reserved \
             separator/continuationSeparator notes must NOT appear"
        );
    }

    /// The `read_index` TOOL (not just the `notes_json` helper) carries the
    /// `notes` array across the wire, so a cold agent gets footnote id/kind/
    /// text from the SAME call it already uses to find block ids — no extra
    /// round-trip, no unzipping the docx.
    #[tokio::test]
    async fn read_index_tool_surfaces_notes_across_the_wire() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &footnote_docx()).await;
        let result = server.read_index(Parameters(ReadArgs { doc_id })).await;
        assert_eq!(result.is_error, Some(false));
        let json = structured(&result);
        assert_eq!(
            json["notes"],
            json!([{"note_id": "1", "kind": "footnote", "text": "See Appendix C"}]),
            "read_index must carry the notes array: {json}"
        );
    }

    #[test]
    fn revision_filters_are_and_combined_on_author_and_kind() {
        let doc = Document::parse(&redline_docx()).expect("parse redline");
        let rows = revision_rows(&doc.snapshot().canonical);

        // by_author: AuthorA owns the two insertions (#10, #1).
        let author_a: Vec<u32> = rows
            .iter()
            .filter(|r| r.author.as_deref() == Some("AuthorA"))
            .map(|r| r.revision_id)
            .collect();
        assert_eq!(author_a, vec![10, 1]);

        // by_kind: deletions are #11 and the stacked #2.
        let deletes: Vec<u32> = rows
            .iter()
            .filter(|r| r.kind == RevisionKind::Delete)
            .map(|r| r.revision_id)
            .collect();
        assert_eq!(deletes, vec![11, 2]);

        // AND-combined: AuthorB's deletions are #11 and #2 (AuthorB has no
        // insertions, so the author+kind intersection is exactly the deletes).
        let author_b_deletes: Vec<u32> = rows
            .iter()
            .filter(|r| r.author.as_deref() == Some("AuthorB") && r.kind == RevisionKind::Delete)
            .map(|r| r.revision_id)
            .collect();
        assert_eq!(author_b_deletes, vec![11, 2]);
    }

    #[test]
    fn revision_rows_all_live_in_the_first_paragraph() {
        // The by_block_range filter is exercised at the tool level (it needs
        // block positions); here we pin the underlying truth it filters on —
        // every tracked change is in p_1, the second paragraph is clean — so a
        // range of just {p_2..p_2} would correctly yield zero rows.
        let doc = Document::parse(&redline_docx()).expect("parse redline");
        let rows = revision_rows(&doc.snapshot().canonical);
        assert!(
            rows.iter().all(|r| r.block_id == "p_1"),
            "all revisions are in p_1; p_2 is clean"
        );
    }

    #[test]
    fn cap_excerpt_truncates_on_a_char_boundary() {
        // Short text passes through unchanged; at the cap, unchanged.
        assert_eq!(cap_excerpt("short"), "short");
        let at_cap: String = "x".repeat(80);
        assert_eq!(cap_excerpt(&at_cap), at_cap);
        // Over the cap: truncated to 80 chars.
        let over = "y".repeat(105);
        assert_eq!(cap_excerpt(&over).chars().count(), 80);
        // Never splits a multi-byte scalar: emoji truncate to whole chars and
        // the result stays valid UTF-8 (no panic, no replacement char).
        let emoji = "🌼".repeat(85);
        let cut = cap_excerpt(&emoji);
        assert_eq!(cut.chars().filter(|c| *c == '🌼').count(), 80);
    }

    fn synthetic_row(id: u32) -> RevisionRow {
        RevisionRow {
            revision_id: id,
            author: Some("A".into()),
            kind: RevisionKind::Insert,
            block_id: "p_1".into(),
            excerpt: "x".into(),
            date: None,
            location: StoryScope::Body,
        }
    }

    #[test]
    fn cap_under_limit_emits_everything_and_no_report() {
        let rows: Vec<RevisionRow> = (0..3).map(synthetic_row).collect();
        let (emitted, report) = cap_revision_rows(&rows);
        assert_eq!(emitted.len(), 3);
        assert!(report.is_none(), "no truncation report when under the cap");
    }

    #[test]
    fn block_detail_surfaces_literal_prefix_in_text_but_not_as_a_span() {
        // A typed-in enumeration label ("A.\t") that the importer stripped into
        // ParagraphNode::literal_prefix must read like Word reads it: present at
        // the front of `text` AND surfaced as `literal_prefix`, but NOT one of
        // the `spans` (it is structural, not span-addressable). This is the read
        // that was previously blind and drove the doubled-numbering bug.
        let body = r#"<w:p><w:r><w:t xml:space="preserve">A.&#9;First item body</w:t></w:r></w:p>"#;
        let doc = Document::parse(&make_docx(body, false)).expect("parse literal-prefix para");
        let view = doc.read();
        let detail = block_detail_json(&view.blocks[0]);

        assert_eq!(
            detail["literal_prefix"], "A.",
            "the enumeration label is surfaced as structural metadata"
        );
        assert_eq!(
            detail["text"], "A.\tFirst item body",
            "text reads what Word reads: label + tab + body"
        );
        // The spans are body-only: the label is not a targetable span.
        let spans = detail["spans"].as_array().expect("spans array");
        let span_text: String = spans.iter().filter_map(|s| s["text"].as_str()).collect();
        assert_eq!(
            span_text, "First item body",
            "spans carry only the body, not the label"
        );
        assert!(
            !span_text.contains("A."),
            "the label must not appear as a span (it would be wrongly editable): {span_text:?}"
        );
    }

    #[test]
    fn cap_over_limit_truncates_and_reports_explicitly() {
        // The "no silent cap" invariant: when the cap bites, the response MUST
        // carry the limit, the true total, and the omitted count.
        let over = MAX_REVISION_ROWS + 37;
        let rows: Vec<RevisionRow> = (0..over as u32).map(synthetic_row).collect();
        let (emitted, report) = cap_revision_rows(&rows);
        assert_eq!(emitted.len(), MAX_REVISION_ROWS, "emits exactly the cap");
        let report = report.expect("truncation must be reported, never silent");
        assert_eq!(report["limit"], MAX_REVISION_ROWS);
        assert_eq!(report["total"], over);
        assert_eq!(report["omitted"], 37);
        assert!(
            report["advice"]
                .as_str()
                .is_some_and(|s| s.contains("filter")),
            "advice tells the caller how to fetch the omitted rows"
        );
    }

    // ─── Lean write receipts ───────────────────────────────────────────────

    /// A DOCX whose body is `n` distinct body paragraphs. Each carries a
    /// realistic clause-length sentence (so the heavy-outline `text` field is
    /// representative of a real document, not a toy 20-char string), prefixed with
    /// "Paragraph {i}" so block ids map to `expect` substrings.
    fn make_multi_para_docx(n: usize) -> Vec<u8> {
        let body: String = (0..n)
            .map(|i| {
                format!(
                    r#"<w:p><w:r><w:t>Paragraph {i}: the parties hereby agree that the foregoing \
                       provisions shall be construed in accordance with the governing law and \
                       interpreted to give effect to their evident commercial intent.</w:t></w:r></w:p>"#
                )
            })
            .collect();
        make_docx(&body, false)
    }

    /// A whole-paragraph replace transaction targeting one block id.
    fn replace_txn_arg(target: &str, expect: &str, new_text: &str) -> TransactionArg {
        TransactionArg(json!({
            "ops": [{
                "op": "replace",
                "target": target,
                "expect": expect,
                "content": { "type": "paragraph",
                             "content": [{ "type": "text", "text": new_text }] },
            }],
            "revision": { "author": "Receipts Test" },
            "summary": "test edit",
        }))
    }

    fn two_op_replace_txn_arg() -> TransactionArg {
        TransactionArg(json!({
            "ops": [
                { "op": "replace", "target": "p_2", "expect": "Paragraph 1",
                  "content": { "type": "paragraph",
                               "content": [{ "type": "text", "text": "Second paragraph, rewritten." }] } },
                { "op": "replace", "target": "p_4", "expect": "Paragraph 3",
                  "content": { "type": "paragraph",
                               "content": [{ "type": "text", "text": "Fourth paragraph, rewritten." }] } },
            ],
            "revision": { "author": "Receipts Test" },
            "summary": "multi-op test edit",
        }))
    }

    fn structured(result: &CallToolResult) -> Value {
        result
            .structured_content
            .clone()
            .expect("tool result carries a structured payload")
    }

    /// The receipt for a single-block edit on a multi-block document names ONLY
    /// the touched block — never the whole outline. This is the core economics
    /// fix: a write must not echo unrequested document content.
    #[tokio::test]
    async fn apply_edit_receipt_contains_only_the_touched_block() {
        let server = StemmaServer::new();
        let open = server
            .open_docx(Parameters(OpenArgs {
                path: write_temp_docx(&make_multi_para_docx(10)),
            }))
            .await;
        let doc_id = structured(&open)["doc_id"]
            .as_str()
            .expect("doc_id")
            .to_string();

        // open_docx returns the COMPACT index, not full block rows.
        let open_payload = structured(&open);
        assert_eq!(open_payload["block_count"], 10);
        assert!(
            open_payload.get("blocks").is_none(),
            "open_docx must not echo full block rows: {open_payload}"
        );
        assert!(
            open_payload["index"]
                .as_array()
                .is_some_and(|a| a.len() == 10),
            "open_docx returns a compact index row per block"
        );

        // Edit exactly one paragraph (p_4).
        let result = server
            .apply_edit(Parameters(ApplyEditArgs {
                doc_id: doc_id.clone(),
                transaction: replace_txn_arg("p_4", "Paragraph 3", "Paragraph FOUR rewritten."),
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        let payload = structured(&result);
        assert_eq!(payload["applied"], true, "edit applied: {payload}");
        assert!(
            payload.get("blocks").is_none(),
            "receipt must NOT echo the full outline: {payload}"
        );
        let changed_ids = payload["changed_block_ids"]
            .as_array()
            .expect("changed_block_ids array");
        assert_eq!(
            changed_ids.len(),
            1,
            "exactly one block changed: {changed_ids:?}"
        );
        assert_eq!(changed_ids[0], "p_4");
        let changed_blocks = payload["changed_blocks"]
            .as_array()
            .expect("changed_blocks array");
        assert_eq!(changed_blocks.len(), 1, "rows only for the touched block");
        assert_eq!(changed_blocks[0]["id"], "p_4");
        assert_eq!(payload["block_count"], 10, "block_count is the full count");
        // The newly created tracked revision id is reported.
        assert!(
            payload["revision_ids"]
                .as_array()
                .is_some_and(|a| !a.is_empty()),
            "the new revision id(s) are reported: {payload}"
        );
    }

    /// End-to-end MCP wire test for `insert`'s `toc` content block: applies
    /// cleanly through the actual `apply_edit` tool (not just the engine's
    /// `parse_transaction`), and a misnamed field on the block (`"level"`
    /// instead of `"levels"`) is still refused, not silently dropped — the
    /// same `deny_unknown_fields` guarantee every other v4 block carries.
    #[tokio::test]
    async fn apply_edit_inserts_toc_block() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &make_multi_para_docx(4)).await;

        let result = server
            .apply_edit(Parameters(ApplyEditArgs {
                doc_id: doc_id.clone(),
                transaction: TransactionArg(json!({
                    "ops": [{
                        "op": "insert",
                        "target": { "anchor": "p_1", "position": "before" },
                        "content": [{ "type": "toc" }]
                    }],
                    "revision": { "author": "Agent" }
                })),
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        let payload = structured(&result);
        assert_eq!(payload["applied"], true, "toc insert applied: {payload}");
        assert!(
            payload["revision_ids"]
                .as_array()
                .is_some_and(|a| !a.is_empty()),
            "the toc insert is a tracked revision: {payload}"
        );

        // A misnamed field ("level" instead of "levels") is refused at the
        // wire edge, not silently ignored.
        let bad = server
            .apply_edit(Parameters(ApplyEditArgs {
                doc_id,
                transaction: TransactionArg(json!({
                    "ops": [{
                        "op": "insert",
                        "target": { "anchor": "p_1", "position": "before" },
                        "content": [{ "type": "toc", "level": { "from": 1, "to": 3 } }]
                    }],
                    "revision": { "author": "Agent" }
                })),
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        assert_eq!(bad.is_error, Some(true), "misnamed `level` must be refused");
        let bad_payload = structured(&bad);
        let message = bad_payload["error"].as_str().unwrap_or_default();
        assert!(
            message.contains("level"),
            "error must name the bad field; got: {bad_payload}"
        );
    }

    /// The `moves` receipt entry is the in-band replacement for a
    /// whole-document re-read after a move: it must name every
    /// (source_id -> copy_id) pair the range move created, and the
    /// prev/next blocks immediately surrounding where the run landed.
    #[tokio::test]
    async fn range_move_receipt_pins_neighbors_and_pairs() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &make_multi_para_docx(6)).await;

        // Move [p_2..p_4] ("Paragraph 1".."Paragraph 3") to after p_6
        // ("Paragraph 5", the last block) — lands at the very end of the doc.
        let result = server
            .apply_edit(Parameters(ApplyEditArgs {
                doc_id,
                transaction: TransactionArg(json!({
                    "ops": [{
                        "op": "move",
                        "target": { "from": "p_2", "to": "p_4" },
                        "destination": { "anchor": "p_6", "position": "after" },
                    }],
                    "revision": { "author": "Receipts Test" },
                })),
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        let payload = structured(&result);
        assert_eq!(payload["applied"], true, "edit applied: {payload}");

        let moves = payload["moves"].as_array().expect("moves array");
        assert_eq!(moves.len(), 1, "exactly one move group: {payload}");
        let entry = &moves[0];

        let pairs = entry["pairs"].as_array().expect("pairs array");
        let source_ids: std::collections::HashSet<&str> = pairs
            .iter()
            .map(|p| p["source_id"].as_str().unwrap())
            .collect();
        assert_eq!(
            source_ids,
            std::collections::HashSet::from(["p_2", "p_3", "p_4"]),
            "pairs must name exactly the moved sources: {pairs:?}"
        );
        for pair in pairs {
            let copy_id = pair["copy_id"].as_str().expect("copy_id string");
            assert_ne!(
                copy_id,
                pair["source_id"].as_str().unwrap(),
                "the copy must have a FRESH id, distinct from its source"
            );
        }

        // Landed at the end: prev is p_6 (with a text preview), next is null.
        assert_eq!(entry["prev"]["id"], "p_6");
        assert!(
            entry["prev"]["text_preview"]
                .as_str()
                .is_some_and(|s| s.contains("Paragraph 5")),
            "prev's text_preview names the actual neighbor content: {entry}"
        );
        assert!(
            entry["next"].is_null(),
            "nothing follows the run at the end of the document: {entry}"
        );
    }

    /// A transaction with no move op reports an empty `moves` list — the
    /// field is always present (predictable shape), never omitted.
    #[tokio::test]
    async fn non_move_edit_reports_an_empty_moves_list() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &make_multi_para_docx(4)).await;
        let result = server
            .apply_edit(Parameters(ApplyEditArgs {
                doc_id,
                transaction: replace_txn_arg("p_2", "Paragraph 1", "Paragraph ONE rewritten."),
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        let payload = structured(&result);
        assert_eq!(payload["applied"], true, "edit applied: {payload}");
        assert_eq!(
            payload["moves"].as_array(),
            Some(&Vec::new()),
            "no move op means an empty (not omitted) moves list: {payload}"
        );
    }

    /// Hard size discipline: a single-block edit on a 102-block document must
    /// fit comfortably under the host tool-result limit.
    /// The old contract echoed ~50KB of outline here; the lean receipt is a
    /// fraction of that.
    #[tokio::test]
    async fn apply_edit_receipt_is_small_on_a_102_block_doc() {
        let server = StemmaServer::new();
        let open = server
            .open_docx(Parameters(OpenArgs {
                path: write_temp_docx(&make_multi_para_docx(102)),
            }))
            .await;
        let doc_id = structured(&open)["doc_id"]
            .as_str()
            .expect("doc_id")
            .to_string();

        let result = server
            .apply_edit(Parameters(ApplyEditArgs {
                doc_id: doc_id.clone(),
                transaction: replace_txn_arg("p_50", "Paragraph 49", "Paragraph FIFTY rewritten."),
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        let payload = structured(&result);
        let bytes = serde_json::to_vec(&payload).expect("serialize receipt");
        // The hard cap: a WRITE receipt names only the touched block, so it is a
        // small constant regardless of document size.
        assert!(
            bytes.len() < 16 * 1024,
            "write receipt must be < 16KB on a 102-block doc, was {} bytes",
            bytes.len()
        );

        // open_docx returns the compact index (the requested navigation tier,
        // inherently O(blocks)). It is not capped at 16KB, but it must be
        // strictly smaller than the OLD heavy outline that tripped the host's
        // truncation limit (~49KB), AND it must DROP the two
        // heaviest per-block fields — full `text` and `semantic_hash` — which
        // are what made the heavy outline blow up. The compact row carries only
        // a bounded text_preview.
        let open_payload = structured(&open);
        let index = open_payload["index"].as_array().expect("index rows");
        for row in index {
            assert!(
                row.get("text").is_none(),
                "compact index row must not carry full block text: {row}"
            );
            assert!(
                row.get("semantic_hash").is_none(),
                "compact index row must not carry the semantic_hash: {row}"
            );
            assert!(
                row.get("text_preview").is_some(),
                "compact index row carries a bounded preview instead: {row}"
            );
        }
        let compact_bytes = serde_json::to_vec(&open_payload).expect("serialize open");
        let heavy_outline = server.outline(&doc_id).expect("heavy outline");
        let heavy_bytes =
            serde_json::to_vec(&json!({ "blocks": heavy_outline })).expect("serialize heavy");
        assert!(
            compact_bytes.len() < heavy_bytes.len(),
            "open_docx compact index ({} bytes) must be smaller than the old heavy \
             outline ({} bytes)",
            compact_bytes.len(),
            heavy_bytes.len()
        );
    }

    /// The receipt's revision_ids must equal the revisions that actually EXIST
    /// after the edit (what list_revisions reports) — not the raw stamped range.
    /// A multi-op transaction exposed a phantom: the materializer stamps ids
    /// sequentially, but normalize can drop a stamped segment, so the contiguous
    /// (max_before, max_after] range over-reports. An agent that accept/rejects
    /// a phantom id gets "no such revision". Receipt == read surface, exactly.
    #[tokio::test]
    async fn receipt_revision_ids_are_exactly_the_surviving_revisions() {
        let server = StemmaServer::new();
        let open = server
            .open_docx(Parameters(OpenArgs {
                path: write_temp_docx(&make_multi_para_docx(10)),
            }))
            .await;
        let doc_id = structured(&open)["doc_id"]
            .as_str()
            .expect("doc_id")
            .to_string();

        let result = server
            .apply_edit(Parameters(ApplyEditArgs {
                doc_id: doc_id.clone(),
                transaction: two_op_replace_txn_arg(),
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        let payload = structured(&result);
        assert_eq!(payload["applied"], true, "multi-op edit applied: {payload}");
        let mut receipt_ids: Vec<u64> = payload["revision_ids"]
            .as_array()
            .expect("revision_ids array")
            .iter()
            .map(|v| v.as_u64().expect("numeric id"))
            .collect();
        receipt_ids.sort_unstable();
        assert!(!receipt_ids.is_empty(), "a tracked edit creates revisions");

        let listed = server
            .list_revisions(Parameters(ListRevisionsArgs {
                doc_id,
                filter: None,
            }))
            .await;
        let mut listed_ids: Vec<u64> = structured(&listed)["revisions"]
            .as_array()
            .expect("revisions array")
            .iter()
            .map(|r| r["revision_id"].as_u64().expect("numeric id"))
            .collect();
        listed_ids.sort_unstable();
        listed_ids.dedup();

        assert_eq!(
            receipt_ids, listed_ids,
            "receipt revision_ids must be exactly what list_revisions reports — \
             no phantom (stamped-then-dropped) ids, none missing"
        );
    }

    /// A whole-paragraph replace to identical text surfaces the engine's
    /// NoOpEdit as a tool ERROR at the MCP edge — the silent-no-op bug
    /// (apply_edit returning {"applied": true} while changing nothing) is
    /// fixed end to end.
    #[tokio::test]
    async fn apply_edit_no_op_is_a_tool_error_not_a_fake_success() {
        let server = StemmaServer::new();
        let open = server
            .open_docx(Parameters(OpenArgs {
                path: write_temp_docx(&make_multi_para_docx(3)),
            }))
            .await;
        let doc_id = structured(&open)["doc_id"]
            .as_str()
            .expect("doc_id")
            .to_string();

        // The exact current text of p_1, fetched from the read view.
        let p1_text = server
            .outline(&doc_id)
            .expect("outline")
            .iter()
            .find(|row| row["id"] == "p_1")
            .and_then(|row| row["text"].as_str().map(str::to_string))
            .expect("p_1 text");

        let result = server
            .apply_edit(Parameters(ApplyEditArgs {
                doc_id,
                // Replace p_1 with its EXACT current text → no change.
                transaction: replace_txn_arg("p_1", "Paragraph 0", &p1_text),
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        assert_eq!(
            result.is_error,
            Some(true),
            "a no-op edit must be a tool error, not a fake success"
        );
        let payload = structured(&result);
        assert_eq!(
            payload["code"], "NoOpEdit",
            "the error names the no-op: {payload}"
        );
    }

    /// Write the bytes to a temp .docx and return its path. The OS temp dir is
    /// writable in the test sandbox; each call uses a unique name.
    fn write_temp_docx(bytes: &[u8]) -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "stemma_mcp_receipt_test_{}_{n}.docx",
            std::process::id()
        ));
        std::fs::write(&path, bytes).expect("write temp docx");
        path.to_string_lossy().into_owned()
    }

    // ─── replace_text MCP edge ──────────────────────────────────────────────

    async fn open_and_id(server: &StemmaServer, bytes: &[u8]) -> String {
        let open = server
            .open_docx(Parameters(OpenArgs {
                path: write_temp_docx(bytes),
            }))
            .await;
        structured(&open)["doc_id"]
            .as_str()
            .expect("doc_id")
            .to_string()
    }

    fn replace_second_para(doc_id: &str, author: &str, allow_existing: bool) -> ReplaceTextArgs {
        ReplaceTextArgs {
            doc_id: doc_id.to_string(),
            old: "Second clean paragraph.".to_string(),
            new: "Second paragraph, tightened.".to_string(),
            author: author.to_string(),
            scope: None,
            expected_matches: None,
            match_mode: "exact".to_string(),
            on_barrier_match: "skip".to_string(),
            allow_existing_author: allow_existing,
        }
    }

    /// THE HARDENING (write-surface seam): every op shape the schema-error path
    /// suggests must ITSELF parse as a valid v4 op. An error message that
    /// teaches an invalid shape is a new trap, not a fix — and "it's just an
    /// error echo" is exactly how the next untyped-param defect would describe
    /// itself. A new required field on any op makes its shape stale and trips
    /// this test by construction.
    #[test]
    fn op_shapes_are_themselves_schema_valid() {
        for op in [
            "replace",
            "insert",
            "delete",
            "move",
            "set_image_attrs",
            "set_attr",
            "table_op",
            "insert_note",
            "edit_note",
            "delete_note",
        ] {
            let shapes = v4_op_shapes(op);
            assert!(!shapes.is_empty(), "a shape for every listed op: {op}");
            for shape in shapes {
                let txn = format!(r#"{{"ops":[{shape}],"revision":{{"author":"shape-test"}}}}"#);
                parse_transaction(&txn).unwrap_or_else(|e| {
                    panic!("the suggested {op} shape must be parse-valid; got {e}: {shape}")
                });
            }
        }
    }

    /// A bad `move` (missing `destination`) gets the canonical move shape
    /// appended to its schema error, so the model fixes it in one follow-up.
    #[tokio::test]
    async fn a_malformed_op_error_teaches_its_shape() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &make_multi_para_docx(4)).await;
        let result = server
            .apply_edit(Parameters(ApplyEditArgs {
                doc_id,
                // `move` with no `destination` — the exact shape-discovery miss.
                transaction: TransactionArg(json!({
                    "ops": [{ "op": "move", "target": "p_2" }],
                    "revision": { "author": "shape-test" },
                })),
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        assert_eq!(result.is_error, Some(true), "a malformed move is an error");
        let msg = structured(&result)["error"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            msg.contains("Expected op shape") && msg.contains(r#""op":"move""#),
            "the error teaches the move shape: {msg}"
        );
    }

    /// THE CONTRACT (author-impersonation refusal): an authored write must not
    /// adopt the identity of an author already present in the opened redline —
    /// that would make the agent's edits indistinguishable from the prior
    /// reviewer's and silently defeat layered review. It is a refusal, not a
    /// default, so it cannot be drifted off. The session's OWN author (novel at
    /// open) is fine to reuse across edits; `allow_existing_author=true`
    /// deliberately continues an existing author's work.
    #[tokio::test]
    async fn an_authored_write_refuses_to_impersonate_an_existing_author() {
        let server = StemmaServer::new();
        // The redline was authored by AuthorA and AuthorB.
        let doc_id = open_and_id(&server, &redline_docx()).await;

        // Impersonating an existing author is refused, fail-loud.
        let refused = server
            .replace_text(Parameters(replace_second_para(&doc_id, "AuthorA", false)))
            .await;
        assert_eq!(
            refused.is_error,
            Some(true),
            "impersonating AuthorA must be refused"
        );
        assert_eq!(
            structured(&refused)["code"],
            "AuthorImpersonation",
            "the refusal names the impersonation contract: {}",
            structured(&refused)
        );

        // A DISTINCT author is accepted.
        let ok = server
            .replace_text(Parameters(replace_second_para(&doc_id, "Reviewer", false)))
            .await;
        assert_ne!(
            ok.is_error,
            Some(true),
            "a distinct author must be accepted: {}",
            structured(&ok)
        );
    }

    /// The session's own (novel) author is not impersonation — a second edit by
    /// the SAME new author must not be refused just because the first edit put
    /// that author into the document. The off-limits set is frozen at open.
    #[tokio::test]
    async fn reusing_the_sessions_own_author_is_not_impersonation() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &redline_docx()).await;
        let first = server
            .replace_text(Parameters(replace_second_para(&doc_id, "Reviewer", false)))
            .await;
        assert_ne!(
            first.is_error,
            Some(true),
            "first edit applies: {}",
            structured(&first)
        );
        // "Reviewer" now authors a revision, but it was NOT in the open-time
        // set, so a second edit under it is fine. (Edit a different phrase so
        // the all-Normal precondition holds.)
        let mut second_args = replace_second_para(&doc_id, "Reviewer", false);
        second_args.old = "omega.".to_string();
        second_args.new = "omega tightened.".to_string();
        let second = server.replace_text(Parameters(second_args)).await;
        // Whatever the edit's own outcome, it must NOT be an impersonation refusal.
        if second.is_error == Some(true) {
            assert_ne!(
                structured(&second)["code"],
                "AuthorImpersonation",
                "the session's own author is never impersonation: {}",
                structured(&second)
            );
        }
    }

    /// The override deliberately continues an existing author's work.
    #[tokio::test]
    async fn allow_existing_author_overrides_the_impersonation_refusal() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &redline_docx()).await;
        let result = server
            .replace_text(Parameters(replace_second_para(&doc_id, "AuthorA", true)))
            .await;
        if result.is_error == Some(true) {
            assert_ne!(
                structured(&result)["code"],
                "AuthorImpersonation",
                "allow_existing_author=true must bypass the impersonation refusal: {}",
                structured(&result)
            );
        }
    }

    /// A unique phrase in exactly one paragraph: replace_text replaces it and the
    /// lean receipt carries match_count + matches + the touched block only.
    #[tokio::test]
    async fn replace_text_unique_phrase_returns_lean_receipt() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &make_multi_para_docx(10)).await;

        let result = server
            .replace_text(Parameters(ReplaceTextArgs {
                doc_id,
                old: "Paragraph 4".to_string(),
                new: "Clause 4".to_string(),
                author: "Counsel".to_string(),
                scope: None,
                expected_matches: None, // default 1
                match_mode: "exact".to_string(),
                on_barrier_match: "skip".to_string(),
                allow_existing_author: false,
            }))
            .await;
        let p = structured(&result);
        assert_eq!(p["applied"], true, "replace_text applied: {p}");
        assert_eq!(p["match_count"], 1);
        let changed = p["changed_block_ids"]
            .as_array()
            .expect("changed_block_ids");
        assert_eq!(
            changed.len(),
            1,
            "only the matched block changed: {changed:?}"
        );
        assert_eq!(
            changed[0], "p_5",
            "Paragraph 4 lives in block p_5 (1-indexed)"
        );
        let matches = p["matches"].as_array().expect("matches");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["block_id"], "p_5");
        // No full-document echo.
        assert!(p.get("blocks").is_none(), "no outline echo: {p}");
    }

    /// expected_matches default 1 but the phrase appears in two blocks → the call
    /// fails with MatchCountMismatch listing both sites, and nothing is applied.
    #[tokio::test]
    async fn replace_text_ambiguous_match_fails_with_sites() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &make_multi_para_docx(10)).await;

        // "the parties" appears in every paragraph → many matches, expected 1.
        let result = server
            .replace_text(Parameters(ReplaceTextArgs {
                doc_id,
                old: "the parties".to_string(),
                new: "the signatories".to_string(),
                author: "Counsel".to_string(),
                scope: None,
                expected_matches: None,
                match_mode: "exact".to_string(),
                on_barrier_match: "skip".to_string(),
                allow_existing_author: false,
            }))
            .await;
        assert_eq!(result.is_error, Some(true), "ambiguous match is an error");
        let p = structured(&result);
        assert_eq!(p["code"], "MatchCountMismatch");
        assert_eq!(p["actual"], 10, "all ten paragraphs matched");
        let matches = p["matches"].as_array().expect("matches list");
        assert_eq!(matches.len(), 10, "every site is named for disambiguation");
        assert!(
            matches[0]["excerpt"]
                .as_str()
                .is_some_and(|e| e.contains('«')),
            "each site carries a delimited excerpt: {}",
            matches[0]
        );
    }

    /// Receipt-honesty contract: replace_text matches BODY
    /// paragraphs only, so a needle living in a TABLE CELL is not replaced — but
    /// the receipt must DISCLOSE it under `unreached_matches`, never report a
    /// green `applied` as if complete. A batch verb would ship half a redline
    /// behind a green receipt without this disclosure.
    #[tokio::test]
    async fn replace_text_discloses_unreached_table_cell_matches() {
        let server = StemmaServer::new();
        // One body paragraph with "Acme Corp" and a 1x1 table whose cell ALSO
        // says "Acme Corp" — the signature-table shape in miniature.
        let body = r#"<w:p><w:r><w:t>Acme Corp is the party.</w:t></w:r></w:p><w:tbl><w:tblPr/><w:tr><w:tc><w:tcPr/><w:p><w:r><w:t>Acme Corp</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#;
        let doc_id = open_and_id(&server, &make_docx(body, false)).await;

        let result = server
            .replace_text(Parameters(ReplaceTextArgs {
                doc_id,
                old: "Acme Corp".to_string(),
                new: "Acme Corporation".to_string(),
                author: "Counsel".to_string(),
                scope: None,
                expected_matches: Some(ExpectedMatchesArg::Keyword("all".to_string())),
                match_mode: "exact".to_string(),
                on_barrier_match: "skip".to_string(),
                allow_existing_author: false,
            }))
            .await;
        assert_ne!(result.is_error, Some(true), "the body replace succeeds");
        let p = structured(&result);
        assert_eq!(p["match_count"], 1, "only the BODY occurrence is replaced");
        let unreached = p["unreached_matches"]
            .as_array()
            .expect("unreached_matches present");
        assert_eq!(
            unreached.len(),
            1,
            "the table-cell occurrence is disclosed, not silent: {p}"
        );
        assert_eq!(unreached[0]["region"], "table_cell");
        assert!(
            unreached[0]["excerpt"]
                .as_str()
                .is_some_and(|e| e.contains("Acme Corp"))
        );
    }

    /// replace_text_batch applies a whole worklist in ONE call and is
    /// NON-ATOMIC: clean items apply, while an ambiguous needle and an absent
    /// needle each fail their OWN item (with the disambiguation contexts) without
    /// blocking the others. This is the contract the "counsel sent a list"
    /// shape depends on — the spec, not the current behavior.
    #[tokio::test]
    async fn replace_text_batch_applies_worklist_non_atomically() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &make_multi_para_docx(5)).await;

        let item = |old: &str, new: &str, em: Option<ExpectedMatchesArg>| ReplaceItem {
            old: old.to_string(),
            new: new.to_string(),
            scope: None,
            expected_matches: em,
            match_mode: "exact".to_string(),
            on_barrier_match: "skip".to_string(),
        };
        let result = server
            .replace_text_batch(Parameters(ReplaceTextBatchArgs {
                doc_id,
                author: "Counsel".to_string(),
                allow_existing_author: false,
                replacements: vec![
                    item("Paragraph 0", "Clause 0", None), // unique → applies
                    item("Paragraph 3", "Clause 3", None), // unique → applies
                    item(
                        "governing law",
                        "applicable law",
                        Some(ExpectedMatchesArg::Keyword("all".to_string())),
                    ), // 5× → applies all
                    item("the parties", "each party", None), // 5 matches, expected 1 → mismatch
                    item("NONEXISTENT_QZX", "x", None),    // 0 matches → mismatch
                ],
            }))
            .await;

        assert_ne!(
            result.is_error,
            Some(true),
            "the batch call itself succeeds"
        );
        let p = structured(&result);
        assert_eq!(p["applied"], 3, "three clean items applied: {p}");
        assert_eq!(p["failed"], 2, "two needles failed their own item: {p}");
        let items = p["items"].as_array().expect("items");
        assert_eq!(items[2]["status"], "applied");
        assert_eq!(
            items[2]["match_count"], 5,
            "'governing law' replaced everywhere"
        );
        assert_eq!(items[3]["status"], "mismatch");
        assert_eq!(items[3]["actual"], 5, "ambiguous needle reports all sites");
        assert_eq!(items[4]["status"], "mismatch");
        assert_eq!(items[4]["actual"], 0, "absent needle reports zero matches");
    }

    /// `expected_matches: "all"` replaces every occurrence.
    #[tokio::test]
    async fn replace_text_expected_all_replaces_everywhere() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &make_multi_para_docx(5)).await;

        let result = server
            .replace_text(Parameters(ReplaceTextArgs {
                doc_id,
                old: "the parties".to_string(),
                new: "the signatories".to_string(),
                author: "Counsel".to_string(),
                scope: None,
                expected_matches: Some(ExpectedMatchesArg::Keyword("all".to_string())),
                match_mode: "exact".to_string(),
                on_barrier_match: "skip".to_string(),
                allow_existing_author: false,
            }))
            .await;
        let p = structured(&result);
        assert_eq!(p["applied"], true);
        assert_eq!(p["match_count"], 5, "all five paragraphs replaced");
    }

    /// replace_text matches BODY text only — a needle that includes a structural
    /// numbering label finds zero matches, and the MCP error surfaces the
    /// `diagnosis` array (and folds it into the message) naming the label and the
    /// working needle, so the agent fixes it in one follow-up instead of falling
    /// back to ceremony.
    #[tokio::test]
    async fn replace_text_label_bearing_needle_error_teaches_the_fix() {
        let server = StemmaServer::new();
        // A numbered heading: import hoists "1." into literal_prefix; body = "Events".
        let docx = make_docx(
            r#"<w:p><w:r><w:t xml:space="preserve">1.</w:t></w:r><w:r><w:tab/></w:r><w:r><w:t>Events</w:t></w:r></w:p>"#,
            false,
        );
        let doc_id = open_and_id(&server, &docx).await;

        let result = server
            .replace_text(Parameters(ReplaceTextArgs {
                doc_id,
                old: "1.\tEvents".to_string(),
                new: "Meetings".to_string(),
                author: "Counsel".to_string(),
                scope: None,
                expected_matches: None,
                match_mode: "exact".to_string(),
                on_barrier_match: "skip".to_string(),
                allow_existing_author: false,
            }))
            .await;
        assert_eq!(
            result.is_error,
            Some(true),
            "label-bearing needle is a zero-match error"
        );
        let p = structured(&result);
        assert_eq!(p["code"], "MatchCountMismatch");
        assert_eq!(p["actual"], 0);
        let diagnosis = p["diagnosis"]
            .as_array()
            .expect("a zero-match carries a diagnosis array");
        assert!(
            diagnosis.iter().any(|d| {
                let s = d.as_str().unwrap_or("");
                s.contains("1.") && s.contains("Events")
            }),
            "the diagnosis names the label and the working needle: {diagnosis:?}"
        );
        // The diagnosis is also folded into the human error string.
        assert!(
            p["error"].as_str().is_some_and(|e| e.contains("Events")),
            "the error message carries the diagnosis: {p}"
        );
    }

    // ─── revision_ids invariant ─────────────────────────────────────────────
    //
    // The receipt reconstructs the new revisions as the contiguous range
    // (max_before+1 ..= max_after). This is guaranteed by construction:
    // apply_transaction stamps ids from max_revision_id(before)+1, strictly
    // monotonic. These tests pin the three load-bearing cases so a future change
    // to the stamping or the edge reconstruction can't silently break the
    // receipt's per-op attribution.

    fn rev_ids(p: &Value) -> Vec<u64> {
        p["revision_ids"]
            .as_array()
            .expect("revision_ids array")
            .iter()
            .map(|v| v.as_u64().expect("revision id is a number"))
            .collect()
    }

    /// A TRACKED edit on a clean document: the receipt's revision_ids are exactly
    /// the revisions the document now carries (the doc had none before), and they
    /// are a contiguous ascending run.
    #[tokio::test]
    async fn revision_ids_match_the_new_tracked_revisions() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &make_multi_para_docx(5)).await;

        let result = server
            .apply_edit(Parameters(ApplyEditArgs {
                doc_id: doc_id.clone(),
                transaction: replace_txn_arg("p_3", "Paragraph 2", "Paragraph TWO rewritten."),
                mode: None, // tracked (default)
                allow_existing_author: false,
            }))
            .await;
        let p = structured(&result);
        let ids = rev_ids(&p);
        assert!(!ids.is_empty(), "a tracked edit creates revision ids: {p}");
        // Contiguous ascending.
        for w in ids.windows(2) {
            assert_eq!(w[1], w[0] + 1, "revision_ids are a contiguous run: {ids:?}");
        }

        // Cross-check against the source of truth: list_revisions on this
        // (previously clean) doc reports exactly the ids the receipt claimed.
        let listed = structured(
            &server
                .list_revisions(Parameters(ListRevisionsArgs {
                    doc_id,
                    filter: None,
                }))
                .await,
        );
        let mut listed_ids: Vec<u64> = listed["revisions"]
            .as_array()
            .expect("revisions array")
            .iter()
            .map(|r| r["revision_id"].as_u64().expect("revision_id"))
            .collect();
        listed_ids.sort_unstable();
        listed_ids.dedup();
        let mut claimed = ids.clone();
        claimed.sort_unstable();
        assert_eq!(
            claimed, listed_ids,
            "the receipt's revision_ids must equal the revisions the document carries"
        );
    }

    /// A DIRECT-mode edit leaves NO tracked revisions (it stamps then resolves),
    /// so the receipt's revision_ids is empty — the honest answer (nothing is
    /// pending review). The empty `(max_before+1..=max_after)` range falls out
    /// because max_after <= max_before.
    #[tokio::test]
    async fn direct_mode_edit_reports_no_revision_ids() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &make_multi_para_docx(5)).await;

        let result = server
            .apply_edit(Parameters(ApplyEditArgs {
                doc_id,
                transaction: replace_txn_arg("p_3", "Paragraph 2", "Paragraph TWO baked in."),
                mode: Some("direct".to_string()),
                allow_existing_author: false,
            }))
            .await;
        let p = structured(&result);
        assert_eq!(p["applied"], true, "direct edit applied: {p}");
        assert!(
            rev_ids(&p).is_empty(),
            "a direct edit leaves no tracked revisions to report: {p}"
        );
        // It still changed the document.
        assert_eq!(
            p["changed_block_ids"].as_array().map(|a| a.len()),
            Some(1),
            "direct edit still reports the changed block"
        );
    }

    // ─── table_receipts / cells_json block_id ergonomics ───────────────────

    /// A body paragraph followed by a 2x2 table (block id "t1"), cells
    /// R0C0/R0C1/R1C0/R1C1.
    fn make_table_docx() -> Vec<u8> {
        // No `w:tblPr`/`w:tcPr`, and `w:gridCol w:w="0"`: any of `w:tblW`,
        // `w:tcW`, or a NONZERO `w:gridCol` width imports as non-default
        // `TableFormatting`/`CellFormatting`, which
        // `validate_base_table_v4_compatible` refuses for a STRUCTURAL
        // table_op (see `TableHasFormattingNotInSpec`) — that guard is
        // orthogonal to this test's concern (row-op receipts).
        let body = r#"<w:p><w:r><w:t>Intro paragraph.</w:t></w:r></w:p><w:tbl><w:tblGrid><w:gridCol w:w="0"/><w:gridCol w:w="0"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>R0C0</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>R0C1</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:p><w:r><w:t>R1C0</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>R1C1</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#;
        make_docx(body, false)
    }

    fn table_block_id(outline: &Value) -> String {
        outline["blocks"]
            .as_array()
            .expect("blocks array")
            .iter()
            .find(|b| b["kind"] == "table")
            .expect("a table block")["id"]
            .as_str()
            .expect("table id")
            .to_string()
    }

    fn insert_row_with_cells_txn_arg(table_id: &str) -> TransactionArg {
        TransactionArg(json!({
            "ops": [{
                "op": "table_op",
                "target": table_id,
                "table_op": {
                    "kind": "insert_row",
                    "ref_row": 1,
                    "position": "after",
                    "cells": ["NEW0", "NEW1"],
                },
            }],
            "revision": { "author": "Receipts Test" },
        }))
    }

    /// `read_block`'s `cells` entries carry the cell's own paragraph `block_id`
    /// (not just `{row, col, text}`) — the id-addressed path a foreign pending
    /// change points an agent at instead of the grid address.
    #[tokio::test]
    async fn table_cells_carry_paragraph_block_id() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &make_table_docx()).await;
        let outline = structured(
            &server
                .read_outline(Parameters(ReadArgs {
                    doc_id: doc_id.clone(),
                }))
                .await,
        );
        let table_id = table_block_id(&outline);
        let detail = structured(
            &server
                .read_block(Parameters(ReadBlockArgs {
                    doc_id,
                    block_id: table_id,
                }))
                .await,
        );
        let cells = detail["cells"].as_array().expect("cells array");
        assert_eq!(cells.len(), 4);
        for cell in cells {
            let block_id = cell["block_id"]
                .as_str()
                .expect("cell block_id is a non-null string");
            assert!(!block_id.is_empty(), "cell block_id must be usable: {cell}");
        }
    }

    /// `apply_edit` on a table_op that structurally inserts a row reports a
    /// `table_receipts` entry naming the fresh row's content and its neighbor
    /// — the receipt an agent uses to confirm placement without a follow-up
    /// read, mirroring `moves`.
    #[tokio::test]
    async fn apply_edit_reports_table_receipts_for_insert_row_with_cells() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &make_table_docx()).await;
        let outline = structured(
            &server
                .read_outline(Parameters(ReadArgs {
                    doc_id: doc_id.clone(),
                }))
                .await,
        );
        let table_id = table_block_id(&outline);

        let result = server
            .apply_edit(Parameters(ApplyEditArgs {
                doc_id,
                transaction: insert_row_with_cells_txn_arg(&table_id),
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        let p = structured(&result);
        let receipts = p["table_receipts"]
            .as_array()
            .unwrap_or_else(|| panic!("table_receipts array: {p}"));
        assert_eq!(receipts.len(), 1, "one table structurally changed: {p}");
        let rows = receipts[0]["rows"].as_array().expect("rows array");
        assert_eq!(rows.len(), 1, "exactly one fresh row: {p}");
        let row = &rows[0];
        assert_eq!(row["status"], "inserted");
        assert_eq!(row["row_index"], 2);
        assert_eq!(
            row["cell_texts"],
            json!(["NEW0", "NEW1"]),
            "receipt carries the inserted row's own content"
        );
        assert_eq!(
            row["prev_row_texts"],
            json!(["R1C0", "R1C1"]),
            "prev neighbor is the row the insert was anchored after"
        );
        assert_eq!(
            row["next_row_texts"],
            Value::Null,
            "no row follows the freshly inserted last row"
        );
    }

    /// An edit unrelated to any table (a plain paragraph replace) reports an
    /// empty `table_receipts` — the receipt does not manufacture noise for
    /// tables the transaction never touched.
    #[tokio::test]
    async fn apply_edit_reports_no_table_receipts_for_unrelated_edits() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &make_table_docx()).await;

        let result = server
            .apply_edit(Parameters(ApplyEditArgs {
                doc_id,
                transaction: replace_txn_arg("p_1", "Intro paragraph.", "Rewritten intro."),
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        let p = structured(&result);
        assert_eq!(
            p["table_receipts"].as_array().map(|a| a.len()),
            Some(0),
            "no table_op ran, so table_receipts must be empty: {p}"
        );
    }

    /// The `table_op` teaching shapes cover BOTH `insert_row` (showing the
    /// `cells` field that carries the new row's content) and `delete_row` —
    /// not just `set_cell_text` — so a schema-error follow-up can fix a
    /// misshapen row-structural op in one try.
    #[test]
    fn table_op_shapes_teach_insert_row_with_cells_and_delete_row() {
        let shapes = v4_op_shapes("table_op");
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

    // ─── insert_image `path` alternative (MCP edge) ──────────────────────────

    /// A 100×50 magic-valid PNG (IHDR dimensions at offsets 16/20).
    fn png_100x50() -> Vec<u8> {
        let mut v = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        v.extend_from_slice(&[0, 0, 0, 13]);
        v.extend_from_slice(b"IHDR");
        v.extend_from_slice(&100u32.to_be_bytes());
        v.extend_from_slice(&50u32.to_be_bytes());
        v.extend_from_slice(&[8, 2, 0, 0, 0]);
        v
    }

    fn write_temp_png(bytes: &[u8]) -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "stemma_mcp_img_test_{}_{n}.png",
            std::process::id()
        ));
        std::fs::write(&path, bytes).expect("write temp png");
        path.to_string_lossy().into_owned()
    }

    /// `path` is read server-side and rewritten to `bytes_base64`; `path` is
    /// dropped so the engine schema accepts the op.
    #[test]
    fn resolve_image_paths_reads_file_into_bytes_base64() {
        use base64::Engine as _;
        let png = png_100x50();
        let path = write_temp_png(&png);
        let txn = json!({
            "ops": [
                { "op": "insert_image", "target": "p_1", "path": path, "format": "png" }
            ],
            "revision": { "author": "Imager" }
        })
        .to_string();
        let resolved = resolve_image_paths(&txn).expect("resolves");
        let value: Value = serde_json::from_str(&resolved).expect("json");
        let op = &value["ops"][0];
        assert!(op.get("path").is_none(), "path removed after resolution");
        let encoded = op["bytes_base64"].as_str().expect("bytes_base64 present");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("decodes");
        assert_eq!(decoded, png, "bytes match the file on disk");
    }

    /// Both `bytes_base64` and `path` → invalid_argument (exactly-one contract).
    #[test]
    fn resolve_image_paths_rejects_both_sources() {
        let txn = json!({
            "ops": [
                { "op": "insert_image", "target": "p_1",
                  "path": "/x.png", "bytes_base64": "AAAA", "format": "png" }
            ],
            "revision": { "author": "Imager" }
        })
        .to_string();
        let err = resolve_image_paths(&txn).expect_err("both sources must fail");
        assert_eq!(err.is_error, Some(true));
        let payload = err.structured_content.expect("payload");
        assert_eq!(payload["code"], "invalid_argument");
        assert!(
            payload["error"].as_str().unwrap().contains("exactly one"),
            "message names the contract: {payload}"
        );
    }

    /// Neither `bytes_base64` nor `path` → invalid_argument that redirects to
    /// both options in one step.
    #[test]
    fn resolve_image_paths_rejects_neither_source() {
        let txn = json!({
            "ops": [
                { "op": "replace_image", "target": "p_1", "drawing_id": "d1", "format": "png" }
            ],
            "revision": { "author": "Imager" }
        })
        .to_string();
        let err = resolve_image_paths(&txn).expect_err("neither source must fail");
        let payload = err.structured_content.expect("payload");
        assert_eq!(payload["code"], "invalid_argument");
        let msg = payload["error"].as_str().unwrap();
        assert!(
            msg.contains("path") && msg.contains("bytes_base64"),
            "message names both options: {msg}"
        );
    }

    /// Non-image ops (and a bytes-only image op) pass through untouched.
    #[test]
    fn resolve_image_paths_leaves_other_ops_untouched() {
        let txn = json!({
            "ops": [
                { "op": "replace", "target": "p_1",
                  "content": { "type": "paragraph", "content": [{ "type": "text", "text": "x" }] } },
                { "op": "insert_image", "target": "p_2", "bytes_base64": "AAAA", "format": "png" }
            ],
            "revision": { "author": "Imager" }
        })
        .to_string();
        let resolved = resolve_image_paths(&txn).expect("resolves");
        let value: Value = serde_json::from_str(&resolved).expect("json");
        assert_eq!(value["ops"][0]["op"], "replace");
        assert_eq!(value["ops"][1]["bytes_base64"], "AAAA");
    }

    /// End-to-end: an agent inserts an image by `path` with cx/cy OMITTED. The
    /// MCP reads + encodes the file, the engine defaults the display box from the
    /// intrinsic dimensions, and the edit applies — no hand-encoded base64, no
    /// guessed EMUs.
    #[tokio::test]
    async fn apply_edit_insert_image_by_path_without_dims() {
        let server = StemmaServer::new();
        let open = server
            .open_docx(Parameters(OpenArgs {
                path: write_temp_docx(&make_multi_para_docx(3)),
            }))
            .await;
        let doc_id = structured(&open)["doc_id"].as_str().unwrap().to_string();

        let png_path = write_temp_png(&png_100x50());
        let result = server
            .apply_edit(Parameters(ApplyEditArgs {
                doc_id,
                transaction: TransactionArg(json!({
                    "ops": [
                        { "op": "insert_image", "target": "p_1",
                          "path": png_path, "format": "png" }
                    ],
                    "revision": { "author": "Imager" }
                })),
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        assert_eq!(result.is_error, Some(false), "insert by path applies");
        let payload = structured(&result);
        assert_eq!(payload["applied"], true, "applied: {payload}");
    }

    // ─── review_session / audit_docx (RFC 0001) ─────────────────────────────

    #[test]
    fn capped_rows_json_truncation_is_explicit_never_silent() {
        let small: Vec<Value> = (0..3).map(|i| json!({ "i": i })).collect();
        let payload = capped_rows_json(small, "narrow it");
        assert_eq!(payload["total"], 3);
        assert!(payload.get("truncated").is_none(), "{payload}");

        let big: Vec<Value> = (0..MAX_REVISION_ROWS + 7)
            .map(|i| json!({ "i": i }))
            .collect();
        let payload = capped_rows_json(big, "narrow it");
        assert_eq!(payload["total"], MAX_REVISION_ROWS + 7);
        assert_eq!(payload["rows"].as_array().unwrap().len(), MAX_REVISION_ROWS);
        assert_eq!(payload["truncated"]["omitted"], 7, "{payload}");
    }

    /// End-to-end over the wire surface: open → tracked edit → review_session
    /// reports the census, an empty direct delta, the untouched proof, and
    /// the validator verdict; with `render` it also writes a gated redline.
    #[tokio::test]
    async fn review_session_tool_reports_census_and_renders() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &make_multi_para_docx(4)).await;
        let result = server
            .apply_edit(Parameters(ApplyEditArgs {
                doc_id: doc_id.clone(),
                transaction: replace_txn_arg("p_2", "Paragraph 1", "Paragraph TWO rewritten."),
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        assert_eq!(structured(&result)["applied"], true);

        let review = server
            .review_session(Parameters(ReviewSessionArgs {
                doc_id: doc_id.clone(),
                render: None,
            }))
            .await;
        assert_eq!(review.is_error, Some(false), "review succeeds");
        let payload = structured(&review);
        assert!(
            payload["session"]["census"]["total"].as_u64().unwrap() > 0,
            "the tracked edit is in the census: {payload}"
        );
        assert_eq!(
            payload["session"]["direct_delta"]["total"], 0,
            "a tracked edit is not a direct change: {payload}"
        );
        assert_eq!(
            payload["untouched"]["violations"].as_array().unwrap().len(),
            0
        );
        assert!(payload["untouched"]["verified_blocks"].as_u64().unwrap() >= 3);
        assert_eq!(payload["validator"]["ok"], true);
        assert!(
            payload.get("render").is_none(),
            "no render unless requested"
        );

        // With render: the session delta materializes as a redline docx.
        let out_path = std::env::temp_dir().join(format!(
            "stemma_mcp_review_render_{}.docx",
            std::process::id()
        ));
        let out_path = out_path.to_string_lossy().into_owned();
        let review = server
            .review_session(Parameters(ReviewSessionArgs {
                doc_id: doc_id.clone(),
                render: Some(RenderSpec {
                    path: out_path.clone(),
                }),
            }))
            .await;
        assert_eq!(review.is_error, Some(false), "review+render succeeds");
        let payload = structured(&review);
        assert_eq!(payload["render"]["path"], out_path.as_str());
        assert!(
            payload["render"]["bytes_written"].as_u64().unwrap() > 0,
            "{payload}"
        );
        assert!(std::path::Path::new(&out_path).exists());
        std::fs::remove_file(&out_path).ok();
    }

    /// Stateless certification over two files: the saved session output
    /// audits cleanly against the original — census carries the edit,
    /// everything else verified. Receipts↔audit agreement at the tool level:
    /// the census row count matches what the apply receipt reported.
    #[tokio::test]
    async fn audit_docx_tool_certifies_saved_output_against_original() {
        let server = StemmaServer::new();
        let before_path = write_temp_docx(&make_multi_para_docx(4));
        let open = server
            .open_docx(Parameters(OpenArgs {
                path: before_path.clone(),
            }))
            .await;
        let doc_id = structured(&open)["doc_id"].as_str().unwrap().to_string();
        let result = server
            .apply_edit(Parameters(ApplyEditArgs {
                doc_id: doc_id.clone(),
                transaction: replace_txn_arg("p_2", "Paragraph 1", "Paragraph TWO rewritten."),
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        let receipt = structured(&result);
        assert_eq!(receipt["applied"], true);
        let receipt_revision_count = receipt["revision_ids"].as_array().unwrap().len();

        let after_path = std::env::temp_dir().join(format!(
            "stemma_mcp_audit_after_{}.docx",
            std::process::id()
        ));
        let after_path = after_path.to_string_lossy().into_owned();
        let save = server
            .save_docx(Parameters(SaveArgs {
                doc_id: doc_id.clone(),
                path: after_path.clone(),
            }))
            .await;
        assert_eq!(save.is_error, Some(false), "save succeeds");

        let audit = server
            .audit_docx(Parameters(AuditDocxArgs {
                before_path: before_path.clone(),
                after_path: after_path.clone(),
                render: None,
            }))
            .await;
        assert_eq!(audit.is_error, Some(false), "audit succeeds");
        let payload = structured(&audit);
        assert_eq!(
            payload["session"]["census"]["total"].as_u64().unwrap() as usize,
            receipt_revision_count,
            "audit census and apply receipt agree on the revision count: {payload}"
        );
        assert_eq!(payload["session"]["direct_delta"]["total"], 0, "{payload}");
        assert_eq!(payload["preexisting"]["total"], 0, "{payload}");
        assert_eq!(
            payload["untouched"]["violations"].as_array().unwrap().len(),
            0
        );
        assert_eq!(payload["validator"]["ok"], true);
        std::fs::remove_file(&after_path).ok();
    }

    // ─── Lifecycle hardening: config, CLI, size cap, eviction attribution ─────

    #[test]
    fn parse_cli_maps_each_invocation() {
        assert_eq!(parse_cli(&[]), Cli::Serve);
        assert_eq!(parse_cli(&["--help".into()]), Cli::Help);
        assert_eq!(parse_cli(&["-h".into()]), Cli::Help);
        assert_eq!(parse_cli(&["--version".into()]), Cli::Version);
        assert_eq!(parse_cli(&["-V".into()]), Cli::Version);
        // An unrecognized flag, a positional argument, and extra tokens after a
        // known flag all fail loudly rather than starting the server.
        assert_eq!(parse_cli(&["--bogus".into()]), Cli::Bad("--bogus".into()));
        assert_eq!(
            parse_cli(&["file.docx".into()]),
            Cli::Bad("file.docx".into())
        );
        assert_eq!(
            parse_cli(&["--help".into(), "extra".into()]),
            Cli::Bad("--help extra".into())
        );
    }

    #[test]
    fn parse_u64_setting_defaults_parses_and_rejects_garbage() {
        // Absent variable → documented default.
        assert_eq!(parse_u64_setting("X", None, 42), Ok(42));
        // Present, valid (including the disable sentinel and surrounding space).
        assert_eq!(parse_u64_setting("X", Some("0"), 42), Ok(0));
        assert_eq!(parse_u64_setting("X", Some("123"), 42), Ok(123));
        assert_eq!(parse_u64_setting("X", Some("  7 "), 42), Ok(7));
        // Garbage is a fail-loud error that names the variable, never a fallback.
        for bad in ["abc", "-1", "", "1.5", "10MiB"] {
            let err = parse_u64_setting("STEMMA_MCP_X", Some(bad), 42)
                .expect_err("garbage must be rejected");
            assert!(err.contains("STEMMA_MCP_X"), "error names the var: {err}");
        }
    }

    #[test]
    fn humanize_secs_renders_whole_units() {
        assert_eq!(humanize_secs(DEFAULT_DOC_TTL_SECS), "24h");
        assert_eq!(humanize_secs(3600), "1h");
        assert_eq!(humanize_secs(1800), "30m");
        assert_eq!(humanize_secs(45), "45s");
    }

    /// open_docx refuses a file larger than the configured cap, and the error
    /// names the size, the limit, and the env var to raise — checked on the
    /// file's metadata, before the bytes are read into memory.
    #[tokio::test]
    async fn open_docx_rejects_a_file_over_the_size_cap() {
        let server = StemmaServer::with_config(Config {
            doc_ttl_secs: DEFAULT_DOC_TTL_SECS,
            max_doc_bytes: 100,
        });
        let docx = make_multi_para_docx(3);
        assert!(docx.len() as u64 > 100, "fixture must exceed the tiny cap");
        let path = write_temp_docx(&docx);
        let result = server
            .open_docx(Parameters(OpenArgs { path: path.clone() }))
            .await;
        assert_eq!(result.is_error, Some(true), "over-cap open is an error");
        let payload = structured(&result);
        assert_eq!(payload["code"], "doc_too_large");
        assert_eq!(payload["limit_bytes"], 100);
        assert_eq!(payload["size_bytes"].as_u64(), Some(docx.len() as u64));
        assert_eq!(payload["env_var"], ENV_MAX_DOC_BYTES);
        assert!(
            payload["error"]
                .as_str()
                .unwrap()
                .contains(ENV_MAX_DOC_BYTES),
            "message points at the env var to raise: {payload}"
        );
        std::fs::remove_file(&path).ok();
    }

    /// A cap of 0 disables the check: a real document opens regardless of size.
    #[tokio::test]
    async fn open_docx_size_cap_of_zero_is_disabled() {
        let server = StemmaServer::with_config(Config {
            doc_ttl_secs: DEFAULT_DOC_TTL_SECS,
            max_doc_bytes: 0,
        });
        let path = write_temp_docx(&make_multi_para_docx(3));
        let result = server
            .open_docx(Parameters(OpenArgs { path: path.clone() }))
            .await;
        assert_eq!(result.is_error, Some(false), "cap 0 opens any size");
        std::fs::remove_file(&path).ok();
    }

    /// The missing-handle upgrade distinguishes an evicted (previously issued)
    /// handle from one that was never opened, and leaves other errors untouched.
    #[test]
    fn attribute_missing_doc_distinguishes_evicted_from_unknown() {
        let server = StemmaServer::new();
        server
            .issued_doc_ids
            .lock()
            .unwrap()
            .insert("doc_issued".to_string());
        let missing = || fail("InvalidDocx", "doc handle not found");

        // Issued but gone from the runtime → evicted, with re-open guidance.
        let evicted = server.attribute_missing_doc(missing(), Some("doc_issued"));
        let p = structured(&evicted);
        assert_eq!(p["code"], "doc_evicted");
        assert_eq!(p["doc_id"], "doc_issued");
        assert_eq!(p["ttl_secs"], DEFAULT_DOC_TTL_SECS);
        assert!(p["error"].as_str().unwrap().contains("open_docx"));

        // Never issued → unknown id (not misreported as an eviction).
        let unknown = server.attribute_missing_doc(missing(), Some("doc_typo"));
        assert_eq!(structured(&unknown)["code"], "unknown_doc_id");

        // No doc_id in scope → passthrough unchanged.
        let passthrough = server.attribute_missing_doc(missing(), None);
        assert_eq!(structured(&passthrough)["code"], "InvalidDocx");

        // A different error code is never rewritten, even for an issued id.
        let other = server.attribute_missing_doc(
            fail("StaleEdit", "target block changed"),
            Some("doc_issued"),
        );
        assert_eq!(structured(&other)["code"], "StaleEdit");
    }
}

/// Documentation-truth guards: the tool reference and the crate README must stay
/// in sync with the ACTUAL registered tool surface and the real
/// `#[serde(deny_unknown_fields)]` argument structs.
///
/// These live in the binary crate (not `tests/`) on purpose: the arg structs and
/// the composed `ToolRouter` are private to this crate, so a `tests/` integration
/// test cannot see them, and a copy would itself drift. Checking against the real
/// types is the whole point.
///
/// The failure mode we prevent: a JSON example in `docs/reference/mcp.md` (which
/// agents paste verbatim) that names a field the struct rejects — e.g. the
/// `compare_docx {base,target,out}` vs. the real `{base_path,target_path,out_path}`.
#[cfg(test)]
mod doc_drift {
    use super::*;
    use std::collections::BTreeSet;

    /// The canonical registered-tool set. Kept in lockstep with the `#[tool]`
    /// methods by `registered_tool_set_matches_canonical_list` below — adding or
    /// renaming a tool without updating this list (and the docs) fails the suite.
    const EXPECTED_TOOLS: &[&str] = &[
        "open_docx",
        "read_outline",
        "read_markdown",
        "read_block",
        "find",
        "get_section",
        "apply_edit",
        "save_docx",
        "compare_docx",
        "replace_all",
        "replace_text",
        "replace_text_batch",
        "read_text",
        "read_accepted",
        "read_rejected",
        "read_redline",
        "list_revisions",
        "read_index",
        "read_styles",
        "read_window",
        "read_html",
        "accept_changes",
        "reject_changes",
        "check_edit",
        "validate_docx",
        "review_session",
        "audit_docx",
        "apply_batch",
    ];

    const MCP_MD: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../docs/reference/mcp.md"
    ));
    const MCP_README: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md"));

    /// Deserialize `value` against the argument struct the named tool actually
    /// binds. Every struct is `#[serde(deny_unknown_fields)]`, so a stray field
    /// name fails here with the serde error — exactly the drift we guard.
    fn check_args(tool: &str, value: Value) -> Result<(), String> {
        macro_rules! d {
            ($t:ty) => {
                serde_json::from_value::<$t>(value)
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            };
        }
        match tool {
            "open_docx" => d!(OpenArgs),
            "read_outline" | "read_markdown" | "read_text" | "read_accepted" | "read_rejected"
            | "read_redline" | "read_index" | "read_styles" | "read_html" => d!(ReadArgs),
            "read_block" => d!(ReadBlockArgs),
            "find" => d!(FindArgs),
            "get_section" => d!(SectionArgs),
            "apply_edit" => d!(ApplyEditArgs),
            "save_docx" => d!(SaveArgs),
            "compare_docx" => d!(CompareArgs),
            "replace_all" => d!(FindReplaceArgs),
            "replace_text" => d!(ReplaceTextArgs),
            "replace_text_batch" => d!(ReplaceTextBatchArgs),
            "list_revisions" => d!(ListRevisionsArgs),
            "read_window" => d!(WindowArgs),
            "accept_changes" => d!(AcceptArgs),
            "reject_changes" => d!(RejectArgs),
            "check_edit" => d!(CheckArgs),
            "validate_docx" => d!(ValidateArgs),
            "review_session" => d!(ReviewSessionArgs),
            "audit_docx" => d!(AuditDocxArgs),
            "apply_batch" => d!(BatchArgs),
            other => Err(format!("no arg-struct mapping for tool '{other}'")),
        }
    }

    /// Index just past the `}` that closes the `{` at byte 0 of `s`, honoring
    /// JSON string quoting/escapes so braces inside strings don't count. `None`
    /// if `s` does not start with `{` or never balances.
    fn brace_match(s: &str) -> Option<usize> {
        let bytes = s.as_bytes();
        if bytes.first() != Some(&b'{') {
            return None;
        }
        let mut depth = 0i32;
        let mut in_str = false;
        let mut escaped = false;
        for (i, &c) in bytes.iter().enumerate() {
            if in_str {
                if escaped {
                    escaped = false;
                } else if c == b'\\' {
                    escaped = true;
                } else if c == b'"' {
                    in_str = false;
                }
                continue;
            }
            match c {
                b'"' => in_str = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i + 1);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Every `<tool_name> { …json… }` example inside a fenced code block, as
    /// `(tool_name, raw_json)`. The JSON may span several lines (brace-matched).
    /// This is the tagging convention: a documented tool call is a fenced line
    /// that begins with a registered tool name followed by its argument object.
    fn tool_call_examples(md: &str, tools: &BTreeSet<&str>) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut in_fence = false;
        let mut block = String::new();
        for line in md.lines() {
            if line.trim_start().starts_with("```") {
                if in_fence {
                    out.extend(examples_in_block(&block, tools));
                    block.clear();
                }
                in_fence = !in_fence;
                continue;
            }
            if in_fence {
                block.push_str(line);
                block.push('\n');
            }
        }
        out
    }

    fn examples_in_block(block: &str, tools: &BTreeSet<&str>) -> Vec<(String, String)> {
        let lines: Vec<&str> = block.lines().collect();
        let mut out = Vec::new();
        for (idx, line) in lines.iter().enumerate() {
            for tool in tools {
                let Some(after) = line.strip_prefix(*tool) else {
                    continue;
                };
                let after_trim = after.trim_start();
                if !after_trim.starts_with('{') {
                    continue;
                }
                // Reassemble the (possibly multi-line) object and brace-match it.
                let mut buf = String::from(after_trim);
                let mut j = idx + 1;
                while brace_match(&buf).is_none() && j < lines.len() {
                    buf.push('\n');
                    buf.push_str(lines[j]);
                    j += 1;
                }
                if let Some(end) = brace_match(&buf) {
                    out.push(((*tool).to_string(), buf[..end].to_string()));
                }
                break;
            }
        }
        out
    }

    /// Parse the `<N> tools` count the README declares.
    fn declared_tool_count(readme: &str) -> Option<usize> {
        let words: Vec<&str> = readme.split_whitespace().collect();
        // The FIRST "<number> tools" phrase (skip prose like "MCP tools").
        words.windows(2).find_map(|w| {
            w[1].starts_with("tools")
                .then(|| w[0].parse::<usize>().ok())
                .flatten()
        })
    }

    /// The registered tool surface is exactly `EXPECTED_TOOLS` — no more, no
    /// fewer. This is what forces the canonical list (and thus the docs checks
    /// below) to track reality when a tool is added or renamed.
    #[test]
    fn registered_tool_set_matches_canonical_list() {
        let server = StemmaServer::new();
        let actual: BTreeSet<String> = server
            .tool_router
            .list_all()
            .iter()
            .map(|t| t.name.to_string())
            .collect();
        let expected: BTreeSet<String> = EXPECTED_TOOLS.iter().map(|s| s.to_string()).collect();
        assert_eq!(
            actual, expected,
            "registered MCP tools drifted from EXPECTED_TOOLS; update the canonical list AND the docs"
        );
        assert_eq!(actual.len(), 28, "expected exactly 28 registered tools");
    }

    /// Every registered tool is documented in both surfaces, and the README's
    /// stated count matches. Loud, specific failures name the missing tool.
    #[test]
    fn every_tool_documented_and_counts_agree() {
        for name in EXPECTED_TOOLS {
            let needle = format!("`{name}");
            assert!(
                MCP_MD.contains(&needle),
                "tool `{name}` is registered but absent from docs/reference/mcp.md"
            );
            assert!(
                MCP_README.contains(&needle),
                "tool `{name}` is registered but absent from stemma-mcp/README.md"
            );
        }
        let declared = declared_tool_count(MCP_README)
            .expect("stemma-mcp/README.md must state an '<N> tools' count");
        assert_eq!(
            declared,
            EXPECTED_TOOLS.len(),
            "stemma-mcp/README.md tool count ({declared}) disagrees with the registered surface ({})",
            EXPECTED_TOOLS.len()
        );
    }

    /// Every fully-literal tool-call example in mcp.md still deserializes against
    /// the tool's real `deny_unknown_fields` arg struct.
    ///
    /// Convention for the paste-ready recipe blocks: `...` marks an elided
    /// runtime value (substituted with a dummy here) and `<x>` marks a described
    /// placeholder (those examples are illustrative and skipped). Everything else
    /// is literal and MUST parse — a renamed/removed field fails loudly with the
    /// tool name and the serde error.
    #[test]
    fn documented_examples_deserialize_against_arg_structs() {
        let tools: BTreeSet<&str> = EXPECTED_TOOLS.iter().copied().collect();
        let examples = tool_call_examples(MCP_MD, &tools);

        let mut checked = 0usize;
        let mut names_seen = BTreeSet::new();
        for (name, raw) in examples {
            // `<x>` placeholders => illustrative template, not a literal example.
            if raw.contains('<') || raw.contains('>') {
                continue;
            }
            // `...` stands in for an elided runtime scalar (e.g. a live doc_id).
            let sanitized = raw.replace("...", "\"__elided__\"");
            let value: Value = serde_json::from_str(&sanitized).unwrap_or_else(|e| {
                panic!(
                    "mcp.md `{name}` example is not valid JSON after placeholder \
                     substitution: {e}\n---\n{raw}\n---"
                )
            });
            if let Err(e) = check_args(&name, value) {
                panic!(
                    "mcp.md `{name}` example no longer deserializes against its \
                     #[serde(deny_unknown_fields)] arg struct: {e}\n---\n{raw}\n---"
                );
            }
            checked += 1;
            names_seen.insert(name);
        }

        // Fail-fast if the extractor rots into a no-op: a green test that checked
        // nothing is a silent fallback, not a pass.
        assert!(
            checked >= 8,
            "expected to verify several literal mcp.md examples, only checked {checked}"
        );
        for must in [
            "compare_docx",
            "accept_changes",
            "save_docx",
            "replace_text",
        ] {
            assert!(
                names_seen.contains(must),
                "expected a literal `{must}` example in mcp.md to be verified"
            );
        }
    }
}
