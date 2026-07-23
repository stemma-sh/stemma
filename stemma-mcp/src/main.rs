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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
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
use sha2::{Digest as _, Sha256};
use stemma_artifacts::{ArtifactError, ArtifactIdentity, PathAuthority, ReadArtifact};

mod task_delivery;
use task_delivery::{
    PendingTaskDeclaration, TaskDeclarationArg, TaskRegistry, TaskSaveOutcome,
    TaskWriteFailureOutcome,
};

use stemma::edit_v4::catalog as op_catalog;
use stemma::edit_v4::parse_transaction;
use stemma::extended_markdown::to_extended_markdown_blocks;
use stemma::view::{
    BlockRole, BlockView, DocumentView, FormFieldIdentity, OpaqueAnchorKind, OpaqueMetadata,
    SegmentView, TextMark, TrackStatus, build_document_view, build_document_view_from_canon,
    build_outline,
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
    /// Path to a .docx under the MCP workspace root. Relative paths resolve
    /// from that root.
    path: String,
    /// Complete task declaration. Valid only on the first open for a task and
    /// mutually exclusive with task_id.
    #[serde(default)]
    task: Option<TaskDeclarationArg>,
    /// Existing task to bind this declared target to. Mutually exclusive with
    /// task.
    #[serde(default)]
    task_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    /// The doc_id returned by open_docx.
    doc_id: String,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum InspectQuery {
    /// Compact current structural index. The safe default for navigation.
    #[default]
    Index,
    /// Paged id-bearing extended Markdown for document comprehension.
    /// Tables are explicit bounded summaries with exact follow-up ids.
    Document,
    /// Exact guarded planning detail for one block, with formatting on request.
    Block,
    /// Structured inventory of every pending tracked revision.
    Revisions,
    /// Bounded rollup of the pending revisions: exact counts by author and
    /// kind, no rows. The dense-document entry point — combine with `filter`
    /// and drill down with `revisions` only where the counts say to look.
    RevisionsSummary,
    /// Authored style table and document defaults.
    Styles,
    /// Case-insensitive text or opaque-metadata search.
    Find,
    /// Inclusive bounded block range.
    Window,
    /// One heading and its section, through the next peer/higher heading.
    Section,
    /// Plain current document text.
    Text,
    /// Current document HTML.
    Html,
    /// Full extended-markdown redline with inline insertions/deletions.
    Redline,
    /// Document projected as if every pending revision were accepted.
    Accepted,
    /// Document projected as if every pending revision were rejected.
    Rejected,
    /// Footnote/endnote inventory with note ids, kinds, and editable body text.
    Notes,
    /// Complete transaction-operation vocabulary; optionally filter by name.
    Operations,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
/// Compact guarded planning detail, or the complete run-formatting projection.
enum InspectBlockDetail {
    #[default]
    Compact,
    Formatting,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct InspectDocxArgs {
    /// The doc_id returned by open_docx.
    doc_id: String,
    /// Projection to return. Omit for the first compact index page.
    #[serde(default)]
    query: InspectQuery,
    /// Required for query="block" or query="section"; use an id from
    /// open_docx or the document projection.
    #[serde(default)]
    block_id: Option<String>,
    /// Valid only for query="block": "compact" (default) or "formatting".
    /// Compact retains exact editable identity and opaque anchors; formatting
    /// adds complete run-level marks, style properties, and metadata.
    #[serde(default)]
    detail: Option<InspectBlockDetail>,
    /// Required for query="find". For query="operations", optionally pass one
    /// exact operation name to retrieve only that entry.
    #[serde(default)]
    pattern: Option<String>,
    /// Additive batch form for query="find": one to eight non-empty patterns.
    /// Mutually exclusive with pattern. Outcomes retain input order and
    /// duplicates; offset/limit and cell_offset/cell_limit apply to each.
    #[serde(default)]
    patterns: Option<Vec<String>>,
    /// Valid only for query="revisions": AND-combined author, kind, and block
    /// range filters matching the revision inventory contract.
    #[serde(default)]
    filter: Option<RevisionFilter>,
    /// Required only for query="window".
    #[serde(default)]
    from_block_id: Option<String>,
    /// Required only for query="window".
    #[serde(default)]
    to_block_id: Option<String>,
    /// Required only for query="window": "text", "markdown", or "html".
    #[serde(default)]
    format: Option<String>,
    /// First result to return; valid for query="index", "document", or "find".
    #[serde(default)]
    offset: Option<usize>,
    /// Page size; valid for query="index", "document", or "find".
    #[serde(default)]
    limit: Option<usize>,
    /// Offset within a table's cells for query="block", or within each matched
    /// table's matching cells for query="find". Defaults to 0.
    #[serde(default)]
    cell_offset: Option<usize>,
    /// Cells returned for a table query="block" (default 8), or matching cells
    /// per table for query="find" (default 4). May not exceed 64.
    #[serde(default)]
    cell_limit: Option<usize>,
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

impl TransactionArg {
    fn operation_count(&self) -> Option<usize> {
        self.0.get("ops").and_then(Value::as_array).map(Vec::len)
    }
}

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

/// Fields accepted by the MCP edge before the transaction reaches the engine
/// parser. Keep these separate from `operation_vocabulary()` so the catalog is
/// honest about both contracts: `path` is a real compact-surface input, while
/// the engine itself sees only the resolved `bytes_base64` field.
fn operation_edge_fields(op: &str) -> &'static [&'static str] {
    match op {
        "insert_image" | "replace_image" => &["path"],
        _ => &[],
    }
}

fn operation_edge_examples(op: &str) -> &'static [&'static str] {
    match op {
        "insert_image" => &[
            r#"{"op":"insert_image","target":"<block_id>","path":"/workspace/logo.png","format":"png","alt_text":"<description>"}"#,
        ],
        "replace_image" => &[
            r#"{"op":"replace_image","target":"<block_id>","drawing_id":"<drawing_id>","path":"/workspace/replacement.png","format":"png"}"#,
        ],
        _ => &[],
    }
}

/// Route map from the 26-tool surface used by the historical benchmark to the
/// five-tool core. This is both documentation and a drift guard: callers can
/// ask for the operation catalog and see the exact replacement route instead
/// of inferring that a removed tool name means a removed capability.
const LEGACY_TOOL_EQUIVALENTS: &[(&str, &str)] = &[
    ("open_docx", "open_docx"),
    ("read_outline", "inspect_docx query='index'"),
    ("read_markdown", "inspect_docx query='document'"),
    ("read_block", "inspect_docx query='block'"),
    ("find", "inspect_docx query='find'"),
    ("get_section", "inspect_docx query='section'"),
    ("read_text", "inspect_docx query='text'"),
    ("read_html", "inspect_docx query='html'"),
    ("read_redline", "inspect_docx query='redline'"),
    ("read_accepted", "inspect_docx query='accepted'"),
    ("read_rejected", "inspect_docx query='rejected'"),
    ("read_index", "inspect_docx query='index' or query='notes'"),
    ("read_styles", "inspect_docx query='styles'"),
    ("read_window", "inspect_docx query='window'"),
    ("list_revisions", "inspect_docx query='revisions'"),
    ("apply_edit", "execute_plan transaction, preview=false"),
    (
        "apply_batch",
        "execute_plan transaction, preview=true|false",
    ),
    ("check_edit", "execute_plan transaction, preview=true"),
    ("accept_changes", "execute_plan resolution action='accept'"),
    ("reject_changes", "execute_plan resolution action='reject'"),
    (
        "replace_text",
        "execute_plan replacement_worklist with one item",
    ),
    ("replace_text_batch", "execute_plan replacement_worklist"),
    (
        "replace_all",
        "execute_plan replacement_worklist item replace_all=true",
    ),
    ("compare_docx", "execute_plan comparison"),
    ("validate_docx", "verify_docx doc_id mode"),
    ("save_docx", "save_docx"),
];

/// Complete transaction vocabulary for the compact surface. Operation names
/// and parser fields come from edit_v4's actual fail-loud parser table; fields
/// consumed by the MCP edge before parsing are identified separately. Curated
/// examples are additive teaching material.
/// Intent hints for the op names cold agents actually guess (observed in
/// transcripts): capabilities that exist but are not ops of that name. The
/// unknown-op refusal quotes the matching hint so the error names the
/// agent's next move instead of only dumping the catalog.
const OP_INTENT_HINTS: &[(&str, &str)] = &[
    (
        "toc",
        "a table of contents is not an op — use `insert` with a {\"type\":\"toc\"} content block",
    ),
    (
        "image",
        "images are `insert_image`, `replace_image`, `set_image_attrs`, `set_image_layout`, or `delete_image`",
    ),
    (
        "field",
        "fields are not a free-form op — cross-references via `insert_cross_ref`, form fields via `set_form_field_value`",
    ),
];

fn operation_catalog(operation: Option<&str>) -> Result<Value, String> {
    let vocabulary = stemma::edit_v4::operation_vocabulary();
    if let Some(name) = operation
        && !vocabulary.iter().any(|(candidate, _)| *candidate == name)
    {
        // Actionability: name the closest real op(s) (substring match both
        // ways) and any intent hint before the full catalog dump.
        let lowered = name.to_ascii_lowercase();
        let mut hints: Vec<String> = Vec::new();
        if !lowered.is_empty() {
            let close: Vec<&str> = vocabulary
                .iter()
                .map(|(candidate, _)| *candidate)
                .filter(|candidate| {
                    candidate.contains(lowered.as_str()) || lowered.contains(*candidate)
                })
                .collect();
            if !close.is_empty() {
                hints.push(format!("closest matches: {}", close.join(", ")));
            }
            for (intent, hint) in OP_INTENT_HINTS {
                if lowered.contains(intent) {
                    hints.push((*hint).to_string());
                }
            }
        }
        let hint_text = if hints.is_empty() {
            String::new()
        } else {
            format!(" {};", hints.join("; "))
        };
        return Err(format!(
            "unknown transaction operation '{name}';{hint_text} known operations: {}",
            vocabulary
                .iter()
                .map(|(candidate, _)| *candidate)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let operations: Vec<Value> = op_catalog::operation_catalog()
        .into_iter()
        .filter(|spec| operation.is_none_or(|wanted| wanted == spec.name))
        .map(|spec| {
            let mut accepted_fields = spec.fields.to_vec();
            accepted_fields.extend_from_slice(operation_edge_fields(spec.name));
            json!({
                "name": spec.name,
                "group": spec.group,
                "accepted_fields": accepted_fields,
                "parser_fields": spec.fields,
                "mcp_edge_fields": operation_edge_fields(spec.name),
                "cue": spec.cue,
                "examples": spec.examples,
                "mcp_edge_examples": operation_edge_examples(spec.name),
            })
        })
        .collect();
    Ok(json!({
        "transaction_envelope": {
            "ops": "non-empty ordered operation array",
            "revision": {"author": "required tracked-change author", "date": "optional ISO-8601"},
            "summary": "optional",
        },
        "operation_count": vocabulary.len(),
        "filtered": operation.is_some(),
        "operations": operations,
        "legacy_surface_routes": LEGACY_TOOL_EQUIVALENTS.iter().map(|(old, route)| {
            json!({"historical_tool": old, "core_route": route})
        }).collect::<Vec<_>>(),
        "workflow": "inspect targets and guards; execute_plan preview=true; apply the identical plan with preview=false when apply_ready=true",
        "server_version": SERVER_VERSION,
    }))
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
                && let Some(spec) = op_catalog::operation_spec(name)
            {
                for shape in spec.examples {
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
    /// (a path under the MCP workspace root, read server-side — preferred; no
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
    /// Zero-based result offset. Defaults to 0.
    #[serde(default)]
    offset: Option<usize>,
    /// Maximum top-level matches returned. Defaults to 16 and may not exceed 64.
    #[serde(default)]
    limit: Option<usize>,
    /// Zero-based offset within each matched table's matching cells. Defaults
    /// to 0. Use the per-table `matching_cells_next_offset` to continue.
    #[serde(default)]
    cell_offset: Option<usize>,
    /// Maximum matching cells returned per matched table. Defaults to 4 and
    /// may not exceed 64. Every table reports its true count and continuation.
    #[serde(default)]
    cell_limit: Option<usize>,
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
    /// New path under the MCP workspace root. Relative paths resolve from the
    /// root; an existing destination is refused.
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
    /// Where to search. Omit for top-level and table-cell body paragraphs. To
    /// restrict: an inclusive top-level block range
    /// `{ "from_block_id": "p_3", "to_block_id": "p_9" }` or a single block
    /// `{ "block_id": "p_7" }` (including a table-cell paragraph id).
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
    /// Run the complete ordered worklist against a throwaway snapshot. Per-item
    /// outcomes are exact, successful items feed later planning, and no state
    /// is persisted.
    #[serde(default)]
    preview: bool,
    /// Author-impersonation override (see replace_text). Default false.
    #[serde(default)]
    allow_existing_author: bool,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReplaceTextScopeArg {
    /// Restrict to one paragraph id, including a paragraph nested in a table
    /// cell. Matching-cell ids come from inspect_docx find/block results.
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
    /// Path to the base ("original") .docx under the MCP workspace root.
    base_path: String,
    /// Path to the target ("modified") .docx under the MCP workspace root.
    target_path: String,
    /// New redline path under the MCP workspace root.
    out_path: String,
    /// Author name stamped on the redline's tracked changes. Defaults to "stemma".
    #[serde(default)]
    author: Option<String>,
}

// ─── Result helpers ──────────────────────────────────────────────────────────

/// A potentially large evidence collection. Only evidence may use this type:
/// submitted worklist items and transaction operations are decision-plane data
/// and use [`CompleteDecisionOutcomes`] below instead.
#[derive(Debug)]
struct CappedEvidenceSet {
    rows: Vec<Value>,
    total: usize,
    set_sha256: String,
}

impl CappedEvidenceSet {
    /// Commit to the complete, deterministically ordered set before retaining
    /// only the inline prefix. The commitment is SHA-256 over a compact JSON
    /// array whose object keys are sorted recursively and whose array order is
    /// preserved.
    fn new(complete: Vec<Value>, cap: usize) -> Self {
        let total = complete.len();
        let set_sha256 = canonical_set_sha256(&complete);
        let rows = complete.into_iter().take(cap).collect();
        Self {
            rows,
            total,
            set_sha256,
        }
    }

    fn returned(&self) -> usize {
        self.rows.len()
    }

    fn omitted(&self) -> usize {
        self.total - self.returned()
    }

    fn metadata(&self) -> Value {
        json!({
            "total": self.total,
            "returned": self.returned(),
            "omitted": self.omitted(),
            "set_sha256": self.set_sha256,
        })
    }
}

/// Decision-plane outcomes are complete by construction. There is deliberately
/// no cap or paging method on this type: shortening the rows would violate the
/// submitted-count invariant and panic at this internal programmer boundary.
#[derive(Debug)]
struct CompleteDecisionOutcomes {
    rows: Vec<Value>,
}

impl CompleteDecisionOutcomes {
    fn new(species: &str, submitted: usize, rows: Vec<Value>) -> Self {
        assert_eq!(
            rows.len(),
            submitted,
            "{species} outcome count must equal submitted count"
        );
        Self { rows }
    }

    fn uniform(species: &str, submitted: usize, status: &str) -> Self {
        let rows = (0..submitted)
            .map(|index| json!({"index": index, "status": status}))
            .collect();
        Self::new(species, submitted, rows)
    }

    fn into_rows(self) -> Vec<Value> {
        self.rows
    }
}

fn attach_transaction_outcomes(
    result: CallToolResult,
    operation_count: usize,
    preview: bool,
) -> CallToolResult {
    let is_error = result.is_error == Some(true);
    let operation_status = if is_error {
        "not_applied"
    } else if preview {
        "would_apply"
    } else {
        "applied"
    };
    let atomicity_status = if is_error {
        "refused"
    } else if preview {
        "would_apply"
    } else {
        "committed"
    };
    let outcomes =
        CompleteDecisionOutcomes::uniform("transaction", operation_count, operation_status);
    let Some(Value::Object(mut payload)) = result.structured_content.clone() else {
        return result;
    };
    payload.insert("operation_count".into(), json!(operation_count));
    payload.insert(
        "operation_outcomes".into(),
        Value::Array(outcomes.into_rows()),
    );
    payload.insert(
        "atomicity".into(),
        json!({"mode": "all", "status": atomicity_status}),
    );
    let value = Value::Object(payload);
    let rebuilt = if is_error {
        CallToolResult::structured_error(value)
    } else {
        CallToolResult::structured(value)
    };
    rebuilt.with_meta(result.meta)
}

fn attach_known_transaction_outcomes(
    result: CallToolResult,
    operation_count: Option<usize>,
    preview: bool,
) -> CallToolResult {
    match operation_count {
        Some(operation_count) => attach_transaction_outcomes(result, operation_count, preview),
        None => result,
    }
}

fn canonical_set_sha256(rows: &[Value]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"[");
    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            digest.update(b",");
        }
        let mut encoded = Vec::new();
        write_canonical_json(row, &mut encoded);
        digest.update(encoded);
    }
    digest.update(b"]");
    format!("{:x}", digest.finalize())
}

fn write_canonical_json(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(value) => out.extend_from_slice(if *value { b"true" } else { b"false" }),
        Value::Number(value) => out.extend_from_slice(value.to_string().as_bytes()),
        Value::String(value) => {
            out.extend_from_slice(
                &serde_json::to_vec(value).expect("serializing a JSON string cannot fail"),
            );
        }
        Value::Array(values) => {
            out.push(b'[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                write_canonical_json(value, out);
            }
            out.push(b']');
        }
        Value::Object(values) => {
            out.push(b'{');
            let mut keys: Vec<&str> = values.keys().map(String::as_str).collect();
            keys.sort_unstable();
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                out.extend_from_slice(
                    &serde_json::to_vec(key).expect("serializing a JSON key cannot fail"),
                );
                out.push(b':');
                write_canonical_json(&values[key], out);
            }
            out.push(b'}');
        }
    }
}

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

/// The full editing playbook, served as MCP `instructions` on initialize.
///
/// This is the CANONICAL guidance for driving stemma — golden path, sharp
/// edges, tracked-table/style/note recipes, and the layer-vs-resolve policy.
/// It ships in the server so every MCP client that surfaces `instructions`
/// gets it out of the box (npx, mcpb, any non-Claude host) — agents must not
/// need a client-specific skill file to drive the engine well. Packaging must
/// not fork this guidance into a separate skill.
const INSTRUCTIONS: &str = include_str!("instructions.md");
const CORE_INSTRUCTIONS: &str = "Stemma core profile: open_docx -> inspect_docx -> \
execute_plan -> save_docx. Inspect before editing. execute_plan previews or \
applies one explicit v4 transaction or revision-resolution selection through the typed engine; for several literal \
substitutions use its explicit replacement_worklist path instead of one read/edit round trip per phrase. Never invent block \
ids, span handles, guards, revision authors, or unsupported operations. Inspect query=operations before an unfamiliar \
transaction and query=notes before editing a footnote/endnote. The operations catalog is parser-derived and also maps every \
historical tool to its five-tool route. For several known phrases, use inspect_docx query=find with patterns=[...] once; each ordered pattern has its own exact total and bounded continuation. Use execute_plan comparison (without doc_id) to produce a redline from two files. Treat any plan error, \
unexplained direct_delta row, unexpected changed pre-existing revision, untouched violation, or NEW validator issue as \
incomplete. Comment-story rows are requested annotations, and property_change rows are direct \
OOXML property edits such as image resizing or hyperlink retargeting; reconcile them with the \
user's requested operations and disclose them rather than rejecting a valid output. Rows carrying \
coincides_with_resolution disclose a possible resolution effect; session_resolution_evidenced=true \
means the row's exact ordered content transition reconciles with successful typed resolution commands. \
Validator findings unchanged from baseline are \
pre-existing evidence, not a regression; \
disclose them, but do not withhold an otherwise valid requested output. Ordinary edits must remain pending tracked changes: resolution is not a finalize \
step, and must be used only when the user explicitly asks to accept, reject, or clean up \
revisions. save_docx runs a fresh session audit, refuses a non-deliverable result before path creation, then runs the serialized package gate and commits to a new path. \
Use verify_docx to inspect detailed session evidence without saving or to audit a producer-neutral before/after pair; it is not a required call before save_docx. Set \
STEMMA_MCP_PROFILE=advanced only for expert escape-hatch tools, not broader semantics.";

// ─── Runtime configuration (parsed at the edge) ──────────────────────────────

/// Env var: idle seconds before an open document is evicted from memory.
const ENV_DOC_TTL_SECS: &str = "STEMMA_MCP_DOC_TTL_SECS";
/// Env var: the largest `.docx` `open_docx` will read, in bytes.
const ENV_MAX_DOC_BYTES: &str = "STEMMA_MCP_MAX_DOC_BYTES";
/// Env var: maximum bytes read from one image path in an edit transaction.
const ENV_MAX_IMAGE_BYTES: &str = "STEMMA_MCP_MAX_IMAGE_BYTES";
/// Env var: maximum aggregate image bytes read by one edit transaction.
const ENV_MAX_IMAGE_TOTAL_BYTES: &str = "STEMMA_MCP_MAX_IMAGE_TOTAL_BYTES";
/// Env var: the only filesystem tree agent-controlled MCP paths may access.
const ENV_WORKSPACE_ROOT: &str = "STEMMA_MCP_WORKSPACE_ROOT";
/// Env var selecting the compact default surface or the full expert surface.
const ENV_PROFILE: &str = "STEMMA_MCP_PROFILE";

/// The product-facing MCP surface. File lifecycle remains explicit because
/// opening and committing artifacts are safety boundaries, while the semantic
/// work is the compact read -> execute -> verify sequence in the middle.
const CORE_TOOLS: &[&str] = &[
    "open_docx",
    "inspect_docx",
    "execute_plan",
    "verify_docx",
    "save_docx",
];
const DEFAULT_CORE_INDEX_LIMIT: usize = 16;
const MAX_CORE_INDEX_LIMIT: usize = 256;
const DEFAULT_CORE_DOCUMENT_LIMIT: usize = 16;
const CORE_DOCUMENT_TABLE_CELL_PREVIEWS: usize = 4;
const CORE_DOCUMENT_CELL_EXCERPT_CHARS: usize = 120;
const DEFAULT_FIND_LIMIT: usize = 16;
const MAX_FIND_LIMIT: usize = 64;
const DEFAULT_FIND_CELL_LIMIT: usize = 4;
const MAX_FIND_CELL_LIMIT: usize = 64;
const MAX_BATCH_FIND_PATTERNS: usize = 8;
const MAX_BATCH_FIND_LIMIT: usize = 16;
const MAX_BATCH_FIND_CELL_LIMIT: usize = 4;
const MAX_BATCH_FIND_RESPONSE_BYTES: usize = 256 * 1024;
const FIND_CELL_EXCERPT_CHARS: usize = 120;
const FIND_TEXT_EXCERPT_CHARS: usize = 240;
const DEFAULT_BLOCK_CELL_LIMIT: usize = 8;
const MAX_BLOCK_CELL_LIMIT: usize = 64;
const BLOCK_CELL_EXCERPT_CHARS: usize = 120;

/// Normalize an advertised tool schema for strict function-calling clients.
/// Gemini rejects `$defs`/`$ref`, and common Gemini bridges mishandle the JSON
/// Schema `anyOf: [T, null]` encoding used for optional fields. Optionality is
/// already represented by absence from `required`, so advertising `T` alone
/// loses no model-call shape. The Rust argument structs remain the authoritative
/// validation boundary; this only makes their wire schema self-contained.
fn inline_local_schema_refs(schema: Value) -> Result<Value, String> {
    fn expand(
        value: Value,
        defs: &serde_json::Map<String, Value>,
        active: &mut HashSet<String>,
    ) -> Result<Value, String> {
        match value {
            Value::Array(items) => items
                .into_iter()
                .map(|item| expand(item, defs, active))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array),
            Value::Object(mut object) => {
                if let Some(reference) = object.remove("$ref") {
                    let reference = reference
                        .as_str()
                        .ok_or_else(|| "JSON Schema $ref must be a string".to_string())?;
                    let name = reference.strip_prefix("#/$defs/").ok_or_else(|| {
                        format!("unsupported non-local JSON Schema reference: {reference}")
                    })?;
                    if !active.insert(name.to_string()) {
                        return Err(format!("recursive JSON Schema definition: {name}"));
                    }
                    let target = defs.get(name).cloned().ok_or_else(|| {
                        format!("JSON Schema reference has no matching definition: {reference}")
                    })?;
                    let mut expanded = expand(target, defs, active)?;
                    active.remove(name);

                    // JSON Schema permits annotations beside `$ref`. Preserve
                    // them explicitly instead of silently discarding metadata.
                    if !object.is_empty() {
                        let expanded_object = expanded.as_object_mut().ok_or_else(|| {
                            format!("referenced JSON Schema definition is not an object: {name}")
                        })?;
                        for (key, sibling) in object {
                            expanded_object.insert(key, expand(sibling, defs, active)?);
                        }
                    }
                    Ok(expanded)
                } else {
                    object.remove("$defs");
                    let mut object = object
                        .into_iter()
                        .map(|(key, child)| Ok((key, expand(child, defs, active)?)))
                        .collect::<Result<serde_json::Map<_, _>, String>>()?;

                    if let Some(Value::Array(branches)) = object.get("anyOf") {
                        let non_null: Vec<&Value> = branches
                            .iter()
                            .filter(|branch| {
                                branch.get("type") != Some(&Value::String("null".into()))
                            })
                            .collect();
                        let null_count = branches.len() - non_null.len();
                        if branches.len() == 2 && null_count == 1 && non_null.len() == 1 {
                            let mut replacement = non_null[0]
                                .as_object()
                                .ok_or_else(|| {
                                    "optional JSON Schema branch is not an object".to_string()
                                })?
                                .clone();
                            object.remove("anyOf");
                            for (key, value) in object {
                                replacement.insert(key, value);
                            }
                            return Ok(Value::Object(replacement));
                        }
                    }
                    // Schemars represents Option<primitive> as
                    // `type:["integer","null"]` rather than anyOf. Gemini's
                    // dialect rejects type arrays just as it rejects nullable
                    // anyOf; field absence already carries optionality.
                    if let Some(Value::Array(types)) = object.get("type") {
                        let non_null: Vec<&Value> = types
                            .iter()
                            .filter(|kind| kind.as_str() != Some("null"))
                            .collect();
                        let null_count = types.len() - non_null.len();
                        if types.len() == 2 && null_count == 1 && non_null.len() == 1 {
                            object.insert("type".to_string(), non_null[0].clone());
                        }
                    }
                    Ok(Value::Object(object))
                }
            }
            scalar => Ok(scalar),
        }
    }

    let defs = schema
        .get("$defs")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    expand(schema, &defs, &mut HashSet::new())
}

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
/// Default per-image cap: 20 MiB before base64 expansion.
const DEFAULT_MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
/// Default aggregate image cap per transaction: 50 MiB before base64 expansion.
const DEFAULT_MAX_IMAGE_TOTAL_BYTES: u64 = 50 * 1024 * 1024;

/// Server configuration parsed once at startup from the environment. Parsing is
/// fail-loud: a malformed value is a startup error, never a silent fallback. An
/// absent value takes the documented default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolProfile {
    Core,
    Advanced,
}

#[derive(Clone, Copy, Debug)]
struct Config {
    /// Compact product surface by default; the full engine surface is opt-in.
    profile: ToolProfile,
    /// Idle seconds before an open document is evicted; `0` disables eviction.
    doc_ttl_secs: u64,
    /// Largest `.docx` (in bytes) `open_docx` will read; `0` disables the cap.
    max_doc_bytes: u64,
    /// Largest image source read by an edit; `0` disables the per-file cap.
    max_image_bytes: u64,
    /// Aggregate image source bytes in one edit; `0` disables the total cap.
    max_image_total_bytes: u64,
}

impl Config {
    /// Read the config from the process environment. `Err` is an actionable,
    /// human-readable message describing which variable is malformed.
    fn from_env() -> Result<Self, String> {
        Ok(Self {
            profile: env_profile(ENV_PROFILE)?,
            doc_ttl_secs: env_u64(ENV_DOC_TTL_SECS, DEFAULT_DOC_TTL_SECS)?,
            max_doc_bytes: env_u64(ENV_MAX_DOC_BYTES, DEFAULT_MAX_DOC_BYTES)?,
            max_image_bytes: env_u64(ENV_MAX_IMAGE_BYTES, DEFAULT_MAX_IMAGE_BYTES)?,
            max_image_total_bytes: env_u64(
                ENV_MAX_IMAGE_TOTAL_BYTES,
                DEFAULT_MAX_IMAGE_TOTAL_BYTES,
            )?,
        })
    }

    /// The documented defaults, used when the server is constructed without a
    /// parsed environment (the test constructor and any embedding that does not
    /// go through `main`).
    #[cfg(test)]
    fn defaults() -> Self {
        Self {
            profile: ToolProfile::Core,
            doc_ttl_secs: DEFAULT_DOC_TTL_SECS,
            max_doc_bytes: DEFAULT_MAX_DOC_BYTES,
            max_image_bytes: DEFAULT_MAX_IMAGE_BYTES,
            max_image_total_bytes: DEFAULT_MAX_IMAGE_TOTAL_BYTES,
        }
    }
}

fn parse_profile_setting(name: &str, raw: Option<&str>) -> Result<ToolProfile, String> {
    match raw {
        None | Some("core") => Ok(ToolProfile::Core),
        Some("advanced") => Ok(ToolProfile::Advanced),
        Some(value) => Err(format!(
            "{name}={value:?} is invalid; expected 'core' or 'advanced'"
        )),
    }
}

fn env_profile(name: &str) -> Result<ToolProfile, String> {
    match std::env::var(name) {
        Ok(raw) => parse_profile_setting(name, Some(&raw)),
        Err(std::env::VarError::NotPresent) => parse_profile_setting(name, None),
        Err(std::env::VarError::NotUnicode(_)) => Err(format!("{name} is not valid UTF-8")),
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

/// Resolve one workspace-root setting against an explicit startup directory.
/// Keeping environment access outside this helper makes every path case
/// deterministic and parallel-testable.
fn artifact_authority_from_setting(
    configured_root: Option<&std::ffi::OsStr>,
    startup_dir: &Path,
) -> Result<PathAuthority, String> {
    let supplied_root = match configured_root {
        Some(value) if value.is_empty() => {
            return Err(format!(
                "{ENV_WORKSPACE_ROOT} is empty; set it to an existing directory or unset it to use the startup directory"
            ));
        }
        Some(value) => PathBuf::from(value),
        None => startup_dir.to_path_buf(),
    };
    let root = if supplied_root.is_absolute() {
        supplied_root
    } else {
        startup_dir.join(supplied_root)
    };
    PathAuthority::rooted(&root)
        .map_err(|e| format!("cannot use {} as {ENV_WORKSPACE_ROOT}: {e}", root.display()))
}

fn artifact_authority_from_env() -> Result<PathAuthority, String> {
    let configured_root = std::env::var_os(ENV_WORKSPACE_ROOT);
    match configured_root.as_deref() {
        // Absolute and explicitly empty settings do not depend on cwd. Preserve
        // that property even if the process's startup directory was removed.
        Some(value) if value.is_empty() || Path::new(value).is_absolute() => {
            artifact_authority_from_setting(Some(value), Path::new("."))
        }
        configured_root => {
            let startup_dir = std::env::current_dir()
                .map_err(|e| format!("cannot determine the default MCP workspace root: {e}"))?;
            artifact_authority_from_setting(configured_root, &startup_dir)
        }
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
         {ENV_PROFILE}        Tool surface: core (default, 5 tools) or advanced\n                              \
         (full expert surface).\n    \
         {ENV_DOC_TTL_SECS}   Idle seconds before an open document is evicted from\n                              \
         memory (default {DEFAULT_DOC_TTL_SECS} = 24h; set 0 to disable).\n    \
         {ENV_MAX_DOC_BYTES}  Largest .docx open_docx will read, in bytes\n                              \
         (default {DEFAULT_MAX_DOC_BYTES} = 50 MiB; set 0 to disable).\n    \
         {ENV_MAX_IMAGE_BYTES} Largest single image path an edit will read\n                              \
         (default {DEFAULT_MAX_IMAGE_BYTES} = 20 MiB; set 0 to disable).\n    \
         {ENV_MAX_IMAGE_TOTAL_BYTES} Aggregate image path bytes per edit\n                              \
         (default {DEFAULT_MAX_IMAGE_TOTAL_BYTES} = 50 MiB; set 0 to disable).\n    \
         {ENV_WORKSPACE_ROOT} Only filesystem tree MCP tools may read or write\n                              \
         (default: the canonical server startup directory).\n    \
         RUST_LOG                  Log filter (default stemma_mcp=info); logs go to stderr.\n\
         \n\
         See stemma-mcp/README.md for the full tool surface and lifecycle notes.\n"
    )
}

/// Every successful object payload names the build that produced it. Keeping
/// this at the response boundary prevents a newly added tool from emitting an
/// artifact or decision receipt without provenance.
fn ok(mut value: Value) -> CallToolResult {
    if let Value::Object(payload) = &mut value {
        payload
            .entry("server_version")
            .or_insert_with(|| json!(SERVER_VERSION));
    }
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

fn artifact_error_code(error: &ArtifactError) -> &'static str {
    match error {
        ArtifactError::PathOutsideAuthority { .. } => "artifact_outside_workspace",
        ArtifactError::OutputExists { .. } => "artifact_output_exists",
        ArtifactError::ProtectedSource { .. } => "artifact_protected_source",
        ArtifactError::SourceTooLarge { .. } => "artifact_source_too_large",
        ArtifactError::PathResolution { kind: "source", .. }
        | ArtifactError::IdentityPathNotUtf8 {
            operation: "source",
            ..
        }
        | ArtifactError::WindowsAlternateDataStream {
            operation: "source",
            ..
        }
        | ArtifactError::SourceNotFile { .. }
        | ArtifactError::ReadOpen { .. }
        | ArtifactError::ReadMetadata { .. }
        | ArtifactError::Read { .. } => "artifact_read_failed",
        _ => "artifact_commit_failed",
    }
}

fn artifact_fail(error: ArtifactError) -> CallToolResult {
    fail(artifact_error_code(&error), error.to_string())
}

fn attach_input_artifacts(
    result: CallToolResult,
    sources: Vec<ArtifactIdentity>,
) -> CallToolResult {
    let Some(Value::Object(mut payload)) = result.structured_content.clone() else {
        return result;
    };
    payload.insert("input_artifacts".to_string(), json!(sources));
    let value = Value::Object(payload);
    let rebuilt = if result.is_error == Some(true) {
        CallToolResult::structured_error(value)
    } else {
        CallToolResult::structured(value)
    };
    rebuilt.with_meta(result.meta)
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

/// Mutation receipts never serialize an entire table merely because one cell
/// paragraph changed. The table id and semantic hash prove which top-level
/// block changed; callers can explicitly inspect it when full content is
/// genuinely needed. Non-table rows retain the normal compact outline shape.
fn receipt_block_row(view: &BlockView, tracked: &TrackedBlock) -> Value {
    if !matches!(view.role, BlockRole::Table) {
        return block_row(view, tracked);
    }
    let status = match &tracked.status {
        TrackingStatus::Normal => "normal",
        TrackingStatus::Inserted(_) => "inserted",
        TrackingStatus::Deleted(_) => "deleted",
        TrackingStatus::InsertedThenDeleted(_) => "inserted_then_deleted",
    };
    json!({
        "id": view.id.to_string(),
        "kind": "table",
        "status": status,
        "semantic_hash": block_semantic_hash_for_block(&tracked.block),
        "cell_count": view.cells.len(),
        "text_chars": view.text.chars().count(),
        "content_omitted": true,
        "inspect": "inspect_docx query=block with this id for full table content",
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

/// A bounded excerpt centered on the case-insensitive match when possible.
/// Find is a locator, so its excerpt must show the wording that caused the hit
/// even when that wording occurs near the end of a long clause. Exact text is
/// retrieved by inspecting the returned block id.
fn match_excerpt(text: &str, needle_lower: &str, limit: usize) -> String {
    let total = text.chars().count();
    if total <= limit {
        return text.to_string();
    }
    let lowered = text.to_lowercase();
    let match_start = lowered
        .find(needle_lower)
        .map(|byte| lowered[..byte].chars().count())
        .unwrap_or(0);
    let needle_chars = needle_lower.chars().count().min(limit);
    let flank = (limit - needle_chars) / 2;
    let start = match_start.saturating_sub(flank).min(total - limit);
    text.chars().skip(start).take(limit).collect()
}

/// Build one bounded find page from an already-built view.
///
/// Singular and batch find both call this function, so batching changes only
/// transport shape: matching, totals, ordering, excerpts, and continuation
/// remain the singular contract.
fn find_page(
    view: &DocumentView,
    pattern: &str,
    offset: usize,
    limit: usize,
    cell_offset: usize,
    cell_limit: usize,
) -> Result<Value, String> {
    let needle = pattern.to_lowercase();
    let mut matches = Vec::new();
    for block in &view.blocks {
        let is_table = matches!(block.role, BlockRole::Table);
        let all_matching_cells: Vec<_> = if is_table {
            block
                .cells
                .iter()
                .filter(|cell| cell.text.to_lowercase().contains(&needle))
                .collect()
        } else {
            Vec::new()
        };
        let matching_cell_count = all_matching_cells.len();
        let matching_cells: Vec<Value> = all_matching_cells
            .into_iter()
            .skip(cell_offset)
            .take(cell_limit)
            .map(|cell| {
                let text_chars = cell.text.chars().count();
                let text_excerpt =
                    match_excerpt(&cell.text, &needle, FIND_CELL_EXCERPT_CHARS);
                json!({
                    "row": cell.row,
                    "col": cell.col,
                    "block_id": cell.paragraphs.first().map(|paragraph| paragraph.block_id.as_str()),
                    "text_excerpt": text_excerpt,
                    "text_chars": text_chars,
                    "text_truncated": text_chars > FIND_CELL_EXCERPT_CHARS,
                })
            })
            .collect();
        let matching_cells_returned = matching_cells.len();
        let matching_cells_next = cell_offset + matching_cells_returned;
        let text_chars = block.text.chars().count();
        let base = |matched_in: &str| {
            let mut row = json!({
                "id": block.id.to_string(),
                "role": role_label(&block.role),
                "role_token": block.role_token,
                "list": list_json(block.list.as_ref()),
                "matched_in": matched_in,
                "text": null,
            });
            if is_table {
                row["table_text_omitted"] = json!(true);
                row["matching_cells"] = json!(matching_cells);
                row["matching_cell_count"] = json!(matching_cell_count);
                row["matching_cells_offset"] = json!(cell_offset);
                row["matching_cells_limit"] = json!(cell_limit);
                row["matching_cells_returned"] = json!(matching_cells_returned);
                row["matching_cells_has_more"] = json!(matching_cells_next < matching_cell_count);
                row["matching_cells_next_offset"] = json!(
                    (matching_cells_next < matching_cell_count).then_some(matching_cells_next)
                );
            } else {
                row["text_excerpt"] =
                    json!(match_excerpt(&block.text, &needle, FIND_TEXT_EXCERPT_CHARS));
                row["text_chars"] = json!(text_chars);
                row["text_truncated"] = json!(text_chars > FIND_TEXT_EXCERPT_CHARS);
                row["text_omitted"] = json!(true);
            }
            row
        };
        if block.text.to_lowercase().contains(&needle) {
            matches.push(base("text"));
        }
        for (matched_in, anchor) in opaque_metadata_matches(block, &needle) {
            let mut row = base(matched_in);
            row["anchor"] = anchor;
            matches.push(row);
        }
    }

    let total = matches.len();
    if offset > total {
        return Err(format!(
            "find offset {offset} exceeds total match count {total}"
        ));
    }
    let page: Vec<Value> = matches.into_iter().skip(offset).take(limit).collect();
    let returned = page.len();
    let next = offset + returned;
    Ok(json!({
        "pattern": pattern,
        "count": total,
        "matches": page,
        "offset": offset,
        "limit": limit,
        "returned": returned,
        "has_more": next < total,
        "next_offset": if next < total { Some(next) } else { None },
        "cell_offset": cell_offset,
        "cell_limit": cell_limit,
    }))
}

fn batch_find_response_bytes(payload: &Value) -> Result<usize, usize> {
    let encoded_bytes = serde_json::to_vec(payload)
        .expect("batch find payload is always JSON-serializable")
        .len();
    if encoded_bytes > MAX_BATCH_FIND_RESPONSE_BYTES {
        Err(encoded_bytes)
    } else {
        Ok(encoded_bytes)
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
        "set_sha256": canonical_set_sha256(
            &rows.iter().map(revision_row_json).collect::<Vec<_>>()
        ),
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
        "detail": "formatting",
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

/// The default block-planning projection. It preserves every value needed to
/// identify and guard an edit, including durable opaque ids that a structural
/// paragraph replacement must carry forward. Repeated text-span formatting is
/// intentionally absent and explicitly recoverable with `detail=formatting`.
fn compact_block_detail_json(block: &BlockView) -> Value {
    let anchors: Vec<Value> = block
        .segments
        .iter()
        .filter_map(|segment| {
            let SegmentView::Opaque {
                id,
                kind,
                status,
                text,
                handle,
                ..
            } = segment
            else {
                return None;
            };
            Some(json!({
                "id": id.to_string(),
                "handle": handle.as_ref().map(|h| h.0.clone()),
                "anchor_kind": anchor_kind_str(kind),
                "status": track_status_json(status),
                "text": text,
            }))
        })
        .collect();
    let level = match &block.role {
        BlockRole::Heading { level } => Some(*level),
        _ => None,
    };
    json!({
        "detail": "compact",
        "formatting_available": true,
        "id": block.id.to_string(),
        "role": role_label(&block.role),
        "level": level,
        "opaque_label": block.opaque_label,
        "style": block.style_id,
        "role_token": block.role_token,
        "list": list_json(block.list.as_ref()),
        "cells": cells_json(block),
        "text": block.text,
        "literal_prefix": block.literal_prefix,
        "guard": block.guard,
        "block_status": track_status_json(&block.block_status),
        "anchors": anchors,
    })
}

/// The bounded core projection of one top-level block. Paragraphs retain the
/// normal compact/formatting shape. Tables replace their aggregate text and
/// unbounded full-cell bodies with a page of addressable cell locators. Every
/// paragraph id in a cell is returned, so exact text, anchors, and formatting
/// remain one explicit block inspection away; the advanced `read_block`
/// surface deliberately continues to return the complete table in one call.
fn core_block_detail_json(
    block: &BlockView,
    detail: InspectBlockDetail,
    cell_offset: Option<usize>,
    cell_limit: Option<usize>,
) -> Result<Value, String> {
    let is_table = matches!(block.role, BlockRole::Table);
    if !is_table {
        if cell_offset.is_some() || cell_limit.is_some() {
            return Err(format!(
                "cell_offset/cell_limit require a table block; '{}' is a {}",
                block.id,
                role_label(&block.role)
            ));
        }
        return Ok(match detail {
            InspectBlockDetail::Compact => compact_block_detail_json(block),
            InspectBlockDetail::Formatting => block_detail_json(block),
        });
    }

    let offset = cell_offset.unwrap_or(0);
    let limit = cell_limit.unwrap_or(DEFAULT_BLOCK_CELL_LIMIT);
    if limit == 0 || limit > MAX_BLOCK_CELL_LIMIT {
        return Err(format!(
            "block cell_limit must be between 1 and {MAX_BLOCK_CELL_LIMIT}, got {limit}"
        ));
    }
    let total = block.cells.len();
    if offset > total {
        return Err(format!(
            "block cell_offset {offset} is beyond cell_count {total} for table '{}'",
            block.id
        ));
    }

    let cells: Vec<Value> = block
        .cells
        .iter()
        .skip(offset)
        .take(limit)
        .map(|cell| {
            let text_chars = cell.text.chars().count();
            let text_excerpt: String = cell.text.chars().take(BLOCK_CELL_EXCERPT_CHARS).collect();
            let block_ids: Vec<&str> = cell
                .paragraphs
                .iter()
                .map(|paragraph| paragraph.block_id.as_str())
                .collect();
            json!({
                "row": cell.row,
                "col": cell.col,
                "block_id": block_ids.first().copied(),
                "block_ids": block_ids,
                "text_excerpt": text_excerpt,
                "text_chars": text_chars,
                "text_truncated": text_chars > BLOCK_CELL_EXCERPT_CHARS,
            })
        })
        .collect();
    let returned = cells.len();
    let next = offset + returned;
    let mut result = match detail {
        InspectBlockDetail::Compact => compact_block_detail_json(block),
        InspectBlockDetail::Formatting => block_detail_json(block),
    };
    result["text"] = Value::Null;
    result["table_text_chars"] = json!(block.text.chars().count());
    result["table_text_omitted"] = json!(true);
    result["cells"] = json!(cells);
    result["cell_count"] = json!(total);
    result["cells_offset"] = json!(offset);
    result["cells_limit"] = json!(limit);
    result["cells_returned"] = json!(returned);
    result["cells_has_more"] = json!(next < total);
    result["cells_next_offset"] = json!((next < total).then_some(next));
    Ok(result)
}

/// Detail projection for a paragraph nested inside a table cell. The engine's
/// top-level `DocumentView.blocks` deliberately keeps a table as one block, but
/// edit targeting recurses into its cell paragraphs. Surface those same ids at
/// the read edge so every advertised edit target is inspectable before use.
fn cell_paragraph_detail_json(
    view: &stemma::view::DocumentView,
    target: &str,
    detail: InspectBlockDetail,
) -> Option<Value> {
    for table in &view.blocks {
        if !matches!(table.role, BlockRole::Table) {
            continue;
        }
        for cell in &table.cells {
            for paragraph in &cell.paragraphs {
                if paragraph.block_id != target {
                    continue;
                }
                let mut text = String::new();
                for segment in &paragraph.segments {
                    match segment {
                        stemma::InlineChange::Unchanged { text: part, .. }
                        | stemma::InlineChange::Inserted { text: part, .. } => text.push_str(part),
                        stemma::InlineChange::Deleted { .. } => {}
                        stemma::InlineChange::Opaque { text: part, .. } => {
                            if let Some(part) = part {
                                text.push_str(part);
                            }
                        }
                    }
                }
                let nested_in = json!({
                    "table_id": table.id.to_string(),
                    "row": cell.row,
                    "col": cell.col,
                });
                return Some(match detail {
                    InspectBlockDetail::Formatting => json!({
                        "detail": "formatting",
                        "id": paragraph.block_id,
                        "role": "paragraph",
                        "nested_in": nested_in,
                        "guard": paragraph.guard,
                        "text": text,
                        "segments": paragraph.segments,
                        "server_version": SERVER_VERSION,
                    }),
                    InspectBlockDetail::Compact => {
                        let anchors: Vec<Value> = paragraph
                            .segments
                            .iter()
                            .filter_map(|segment| {
                                let stemma::InlineChange::Opaque {
                                    segment_type,
                                    kind,
                                    opaque_id,
                                    text,
                                    reference_id,
                                    content_hash,
                                    ..
                                } = segment
                                else {
                                    return None;
                                };
                                Some(json!({
                                    "id": opaque_id,
                                    "segment_type": segment_type,
                                    "anchor_kind": kind,
                                    "text": text,
                                    "reference_id": reference_id,
                                    "content_hash": content_hash,
                                }))
                            })
                            .collect();
                        json!({
                            "detail": "compact",
                            "formatting_available": true,
                            "id": paragraph.block_id,
                            "role": "paragraph",
                            "nested_in": nested_in,
                            "guard": paragraph.guard,
                            "text": text,
                            "anchors": anchors,
                            "server_version": SERVER_VERSION,
                        })
                    }
                });
            }
        }
    }
    None
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

/// Extended Markdown for the bounded core document projection. Prose blocks
/// remain exact. A table is one top-level block but can contain an entire
/// contract, so rendering its flattened aggregate would defeat block paging.
/// Instead surface an explicit summary plus a few addressable cell previews;
/// `query="block"` pages every cell and each returned paragraph id provides
/// exact content. The advanced Markdown reads retain the complete projection.
fn core_document_markdown(blocks: &[BlockView]) -> String {
    let mut out = String::new();
    for (index, block) in blocks.iter().enumerate() {
        if index > 0 {
            out.push_str("\n\n");
        }
        if !matches!(block.role, BlockRole::Table) {
            out.push_str(&to_extended_markdown_blocks(std::slice::from_ref(block)));
            continue;
        }

        // Emit the same table header contract without first materializing and
        // discarding the engine renderer's unbounded flattened table body.
        out.push_str(&core_table_markdown_header(block));
        out.push('\n');
        out.push_str(&format!(
            "<obj id={} kind=table cells={} chars={} content=bounded/>",
            block.id,
            block.cells.len(),
            block.text.chars().count()
        ));
        for cell in block.cells.iter().take(CORE_DOCUMENT_TABLE_CELL_PREVIEWS) {
            let block_ids = cell
                .paragraphs
                .iter()
                .map(|paragraph| paragraph.block_id.as_str())
                .collect::<Vec<_>>()
                .join(",");
            let text_chars = cell.text.chars().count();
            let excerpt: String = cell
                .text
                .chars()
                .take(CORE_DOCUMENT_CELL_EXCERPT_CHARS)
                .map(|character| match character {
                    '\n' | '\r' => ' ',
                    other => other,
                })
                .collect();
            out.push_str(&format!(
                "\ncell[{},{}] blocks={} chars={}: {}",
                cell.row, cell.col, block_ids, text_chars, excerpt
            ));
        }
        let omitted = block
            .cells
            .len()
            .saturating_sub(CORE_DOCUMENT_TABLE_CELL_PREVIEWS);
        if omitted > 0 {
            out.push_str(&format!(
                "\n<more cells={} next_offset={} inspect_block={}/>",
                omitted, CORE_DOCUMENT_TABLE_CELL_PREVIEWS, block.id
            ));
        }
    }
    out
}

/// Header parity for a table summarized by [`core_document_markdown`]. Kept
/// concrete instead of introducing a second generic Markdown renderer: this is
/// the one role whose body must be bounded at the core edge.
fn core_table_markdown_header(block: &BlockView) -> String {
    debug_assert!(matches!(block.role, BlockRole::Table));
    let mut header = format!("#{} role=table", block.id);
    if let Some(style) = &block.style_id
        && !style.is_empty()
    {
        header.push_str(" style=");
        header.push_str(style);
    }
    match &block.block_status {
        TrackStatus::Inserted(_) => header.push_str(" status=inserted"),
        TrackStatus::Deleted(_) => header.push_str(" status=deleted"),
        TrackStatus::InsertedThenDeleted { .. } => header.push_str(" status=inserted_then_deleted"),
        TrackStatus::Normal => {}
    }
    header
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
                    .map(|(bv, tb)| receipt_block_row(bv, tb))
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

/// Session evidence for typed revision resolutions that successfully committed.
///
/// The audit itself remains end-state-derived. This evidence supplies the one
/// fact a producer-neutral before/after comparison cannot know: which changed
/// pre-existing revisions were explicitly selected through this session's
/// accept/reject command. Structural locations are retained because legitimate
/// resolution effects can surface at a neighboring paragraph or containing
/// table rather than at the revision record's own block id.
#[derive(Clone, Debug, Default)]
struct SessionResolutionEvidence {
    revision_ids: HashSet<u32>,
    /// Ordered committed-content transitions derived independently by auditing
    /// each successful resolution's exact before/after snapshots. Final review
    /// must be able to replay a location's whole chain from the session
    /// baseline row to the current row; a direct edit before, between, or after
    /// these transitions breaks the chain and remains unexplained.
    direct_transition_batches: Vec<Vec<stemma::audit::DirectChange>>,
}

#[derive(Clone, Debug)]
struct SessionSources {
    artifacts: Vec<ArtifactIdentity>,
    resolutions: SessionResolutionEvidence,
}

impl SessionSources {
    fn new(artifacts: Vec<ArtifactIdentity>) -> Self {
        Self {
            artifacts,
            resolutions: SessionResolutionEvidence::default(),
        }
    }
}

#[derive(Clone)]
struct StemmaServer {
    runtime: Arc<SimpleRuntime>,
    /// Startup configuration (TTL, size cap). Immutable for the process.
    config: Config,
    /// Agent-controlled filesystem authority and create-new commit boundary.
    artifacts: PathAuthority,
    /// Exact source identities incorporated into each open document. The open
    /// DOCX is always first; successfully applied image sources are appended.
    source_artifacts: Arc<Mutex<HashMap<String, SessionSources>>>,
    /// Couples image-backed runtime mutation with source registration and with
    /// session export, preventing a save from observing only half that state.
    artifact_session_gate: Arc<Mutex<()>>,
    /// Every `doc_id` this server has handed out from `open_docx`. The engine
    /// reports a missing handle the same way whether it was never opened or was
    /// evicted after its TTL; membership here disambiguates the two so an
    /// evicted handle yields an actionable "re-open" error instead of a generic
    /// unknown-id one. Grows by one small string per open; never pruned (an
    /// issued id that the runtime no longer holds is, by definition, evicted).
    issued_doc_ids: Arc<Mutex<HashSet<String>>>,
    /// Hash-bound multi-document delivery tasks and doc_id associations.
    tasks: Arc<Mutex<TaskRegistry>>,
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

    #[cfg(test)]
    fn with_config(config: Config) -> Self {
        let artifacts = PathAuthority::explicit()
            .expect("test/embedded process must have a readable current directory");
        Self::with_config_and_authority(config, artifacts)
    }

    fn with_config_and_authority(config: Config, artifacts: PathAuthority) -> Self {
        let tool_router = Self::router_for_profile(config.profile);
        Self {
            runtime: Arc::new(SimpleRuntime::new()),
            config,
            artifacts,
            source_artifacts: Arc::new(Mutex::new(HashMap::new())),
            artifact_session_gate: Arc::new(Mutex::new(())),
            issued_doc_ids: Arc::new(Mutex::new(HashSet::new())),
            tasks: Arc::new(Mutex::new(TaskRegistry::default())),
            // Routers are composed with `+`. Each parallel stream contributes
            // its own named router; the base `tool_router()` carries the core
            // open/read/edit/save tools, `read_projections_router()` the
            // read-surface projections.
            tool_router,
        }
    }

    fn router_for_profile(profile: ToolProfile) -> ToolRouter<Self> {
        let mut router = Self::tool_router()
            + Self::read_projections_router()
            + Self::read_index_router()
            + Self::agentic_router();
        if profile == ToolProfile::Core {
            let names: Vec<String> = router
                .list_all()
                .into_iter()
                .map(|tool| tool.name.into_owned())
                .collect();
            for name in names {
                if !CORE_TOOLS.contains(&name.as_str()) {
                    router.disable_route(name);
                }
            }
        }
        for name in CORE_TOOLS {
            let route = router
                .map
                .get_mut(*name)
                .unwrap_or_else(|| panic!("core tool route is missing: {name}"));
            let schema = Value::Object(route.attr.input_schema.as_ref().clone());
            let schema = inline_local_schema_refs(schema)
                .unwrap_or_else(|error| panic!("invalid schema for core tool {name}: {error}"));
            let schema = schema
                .as_object()
                .unwrap_or_else(|| panic!("core tool schema is not an object: {name}"))
                .clone();
            route.attr.input_schema = Arc::new(schema);
        }
        router
    }

    fn max_doc_bytes(&self) -> Option<u64> {
        (self.config.max_doc_bytes > 0).then_some(self.config.max_doc_bytes)
    }

    fn max_image_bytes(&self) -> Option<u64> {
        (self.config.max_image_bytes > 0).then_some(self.config.max_image_bytes)
    }

    fn max_image_total_bytes(&self) -> Option<u64> {
        (self.config.max_image_total_bytes > 0).then_some(self.config.max_image_total_bytes)
    }

    fn read_source(
        &self,
        path: &str,
        role: &str,
        max_bytes: Option<u64>,
    ) -> Result<ReadArtifact, CallToolResult> {
        self.artifacts
            .read_source(path, role, max_bytes)
            .map_err(artifact_fail)
    }

    fn missing_source_state(doc_id: &str) -> CallToolResult {
        fail_json(json!({
            "code": "artifact_session_state_missing",
            "error": format!(
                "doc_id '{doc_id}' is open but its protected source identity state is missing. \
                 Refusing persistence; re-open the document with open_docx."
            ),
            "doc_id": doc_id,
        }))
    }

    fn protected_sources(&self, doc_id: &str) -> Result<Vec<ArtifactIdentity>, CallToolResult> {
        let artifacts = self
            .source_artifacts
            .lock()
            .expect("source_artifacts mutex poisoned")
            .get(doc_id)
            .map(|session| session.artifacts.clone());
        match artifacts {
            Some(artifacts) if !artifacts.is_empty() => Ok(artifacts),
            _ => Err(Self::missing_source_state(doc_id)),
        }
    }

    fn session_resolution_evidence(
        &self,
        doc_id: &str,
    ) -> Result<SessionResolutionEvidence, CallToolResult> {
        self.source_artifacts
            .lock()
            .expect("source_artifacts mutex poisoned")
            .get(doc_id)
            .filter(|session| !session.artifacts.is_empty())
            .map(|session| session.resolutions.clone())
            .ok_or_else(|| Self::missing_source_state(doc_id))
    }

    fn record_resolution_evidence(
        &self,
        doc_id: &str,
        revision_ids: HashSet<u32>,
        direct_transitions: Vec<stemma::audit::DirectChange>,
    ) {
        let mut sessions = self
            .source_artifacts
            .lock()
            .expect("source_artifacts mutex poisoned");
        let session = sessions
            .get_mut(doc_id)
            .filter(|session| !session.artifacts.is_empty())
            .expect("resolution source state disappeared while artifact_session_gate was held");
        session.resolutions.revision_ids.extend(revision_ids);
        session
            .resolutions
            .direct_transition_batches
            .push(direct_transitions);
    }

    /// Derive the exact committed-content effect of one successful typed
    /// resolution. This is an independent audit of the operation's before and
    /// after snapshots, not a receipt-derived block whitelist.
    fn resolution_direct_transitions(
        before: &stemma::runtime::EditSnapshot,
        after: &stemma::runtime::EditSnapshot,
    ) -> Result<Vec<stemma::audit::DirectChange>, CallToolResult> {
        let before_bytes = stemma::serialize_snapshot(before, &stemma::ExportOptions::unchecked())
            .map_err(|error| fail(&format!("{:?}", error.code), error.message))?;
        let after_bytes = stemma::serialize_snapshot(after, &stemma::ExportOptions::unchecked())
            .map_err(|error| fail(&format!("{:?}", error.code), error.message))?;
        let before_styles = stemma::style_table_from_docx(&before_bytes)
            .map_err(|error| fail(&format!("{:?}", error.code), error.message))?;
        let after_styles = stemma::style_table_from_docx(&after_bytes)
            .map_err(|error| fail(&format!("{:?}", error.code), error.message))?;
        let report = stemma::audit::audit_documents(
            &before.canonical,
            &after.canonical,
            before_styles.as_ref(),
            after_styles.as_ref(),
            stemma::runtime::ValidationReport {
                ok: true,
                issues: Vec::new(),
            },
        )
        .map_err(|error| fail(&format!("{:?}", error.code), error.message))?;
        Ok(report.direct_changes)
    }

    fn record_sources(
        &self,
        doc_id: &str,
        sources: Vec<ArtifactIdentity>,
    ) -> Result<(), CallToolResult> {
        if sources.is_empty() {
            return Ok(());
        }
        let mut sessions = self
            .source_artifacts
            .lock()
            .expect("source_artifacts mutex poisoned");
        let Some(session) = sessions.get_mut(doc_id) else {
            return Err(Self::missing_source_state(doc_id));
        };
        if session.artifacts.is_empty() {
            return Err(Self::missing_source_state(doc_id));
        }
        for source in sources {
            let duplicate = session.artifacts.iter().any(|existing| {
                existing.resolved_path == source.resolved_path
                    && existing.bytes == source.bytes
                    && existing.digest == source.digest
            });
            if !duplicate {
                session.artifacts.push(source);
            }
        }
        Ok(())
    }

    fn evict_expired_sessions(&self, ttl_secs: u64) {
        let _session_guard = self
            .artifact_session_gate
            .lock()
            .expect("artifact_session_gate mutex poisoned");
        self.runtime.evict_expired(ttl_secs);
        self.source_artifacts
            .lock()
            .expect("source_artifacts mutex poisoned")
            .retain(|doc_id, _| self.runtime.contains_handle(&DocHandle(doc_id.clone())));
    }

    fn apply_edit_with_sources(
        &self,
        handle: &DocHandle,
        txn: &stemma::edit::EditTransaction,
        allow_existing_author: bool,
        sources: Vec<ArtifactIdentity>,
    ) -> CallToolResult {
        if sources.is_empty() {
            return self.apply_edit_receipt(handle, txn, allow_existing_author);
        }

        let _session_guard = self
            .artifact_session_gate
            .lock()
            .expect("artifact_session_gate mutex poisoned");
        if !self.runtime.contains_handle(handle) {
            return self.apply_edit_receipt(handle, txn, allow_existing_author);
        }
        if let Err(failure) = self.protected_sources(&handle.0) {
            return failure;
        }
        let (result, applied) = self.apply_edit_receipt_outcome(handle, txn, allow_existing_author);
        if applied {
            if let Err(failure) = self.record_sources(&handle.0, sources.clone()) {
                return failure;
            }
            return attach_input_artifacts(result, sources);
        }
        result
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
        let _session_guard = self
            .artifact_session_gate
            .lock()
            .expect("artifact_session_gate mutex poisoned");
        self.source_artifacts
            .lock()
            .expect("source_artifacts mutex poisoned")
            .remove(doc_id);
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

    /// A bounded page of the structural index used by the compact core. The
    /// page contract is explicit (`returned`, `has_more`, `next_offset`), so a
    /// large document never turns an omitted query into a silent truncation or
    /// a multi-hundred-kilobyte history entry.
    fn core_index_page(
        &self,
        doc_id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Value, CallToolResult> {
        if limit == 0 || limit > MAX_CORE_INDEX_LIMIT {
            return Err(fail(
                "invalid_argument",
                format!("index limit must be between 1 and {MAX_CORE_INDEX_LIMIT}, got {limit}"),
            ));
        }
        let handle = DocHandle(doc_id.to_string());
        let outline = self
            .runtime
            .with(&handle, |snap| {
                let view = build_document_view(snap);
                stemma::view::build_outline(&view)
            })
            .map_err(|e| {
                fail(
                    &format!("{:?}", e.code),
                    format!("doc not open: {}", e.message),
                )
            })?;
        if offset > outline.total_blocks {
            return Err(fail(
                "InvalidRange",
                format!(
                    "index offset {offset} is beyond total_blocks {} in doc '{doc_id}'",
                    outline.total_blocks
                ),
            ));
        }
        let end = offset.saturating_add(limit).min(outline.total_blocks);
        let entries: Vec<Value> = outline.entries[offset..end]
            .iter()
            .map(outline_entry_json)
            .collect();
        let has_more = end < outline.total_blocks;
        Ok(json!({
            "doc_id": doc_id,
            "total_blocks": outline.total_blocks,
            "total_chars": outline.total_chars,
            "offset": offset,
            "limit": limit,
            "returned": entries.len(),
            "has_more": has_more,
            "next_offset": has_more.then_some(end),
            "entries": entries,
            "server_version": SERVER_VERSION,
        }))
    }

    /// A bounded page of id-bearing extended Markdown. Paging is by top-level
    /// block, matching the index's stable document order. The complete
    /// projection remains retrievable without injecting the entire document
    /// into every later model turn.
    fn core_document_page(
        &self,
        doc_id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Value, CallToolResult> {
        if limit == 0 || limit > MAX_CORE_INDEX_LIMIT {
            return Err(fail(
                "invalid_argument",
                format!("document limit must be between 1 and {MAX_CORE_INDEX_LIMIT}, got {limit}"),
            ));
        }
        let handle = DocHandle(doc_id.to_string());
        self.runtime
            .with(&handle, |snap| {
                let view = build_document_view(snap);
                let total = view.blocks.len();
                if offset > total {
                    return Err(fail(
                        "InvalidRange",
                        format!(
                            "document offset {offset} is beyond total_blocks {total} in doc '{doc_id}'"
                        ),
                    ));
                }
                let end = offset.saturating_add(limit).min(total);
                let has_more = end < total;
                Ok(json!({
                    "doc_id": doc_id,
                    "content": core_document_markdown(&view.blocks[offset..end]),
                    "tables_bounded": true,
                    "table_cell_preview_limit": CORE_DOCUMENT_TABLE_CELL_PREVIEWS,
                    "total_blocks": total,
                    "offset": offset,
                    "limit": limit,
                    "returned": end - offset,
                    "has_more": has_more,
                    "next_offset": has_more.then_some(end),
                    "server_version": SERVER_VERSION,
                }))
            })
            .map_err(|e| {
                fail(
                    &format!("{:?}", e.code),
                    format!("doc not open: {}", e.message),
                )
            })?
    }
}

#[tool_router]
impl StemmaServer {
    #[tool(
        description = "Open a .docx file into the engine. Returns a doc_id, block_count, \
                       total_chars, server_version, and the first 16 rows of a PAGED compact \
                       structural index (id, index, role, heading depth, a 120-char \
                       text preview, char/byte length, tracked status, role_token, list \
                       membership), plus returned/has_more/next_offset. Prefer inspect_docx \
                       query='find' for known wording or query='index' with offset/limit for \
                       another page; do not request every page as a substitute for find/window. \
                       open_docx deliberately does not echo an unbounded document projection. \
                       For a multi-document delivery, the first task-bearing call supplies the \
                       complete task declaration; later declared targets use task_id."
    )]
    async fn open_docx(&self, Parameters(args): Parameters<OpenArgs>) -> CallToolResult {
        if args.task.is_some() && args.task_id.is_some() {
            return fail(
                "invalid_argument",
                "open_docx accepts task or task_id, never both",
            );
        }
        if args
            .task_id
            .as_deref()
            .is_some_and(|id| id.trim().is_empty())
        {
            return fail("invalid_argument", "task_id must be a non-empty string");
        }
        let task_id = args
            .task
            .as_ref()
            .map(|task| task.task_id.clone())
            .or_else(|| args.task_id.clone());
        let _task_guard = task_id.as_ref().map(|_| {
            self.artifact_session_gate
                .lock()
                .expect("artifact_session_gate mutex poisoned")
        });
        let mut pending_task: Option<PendingTaskDeclaration> = None;
        // The artifact boundary checks the metadata cap before buffering and
        // checks it again while reading, so growth during the read also refuses.
        let source = match (args.task, args.task_id.as_deref()) {
            (Some(declaration), None) => {
                match self.prepare_task_declaration(declaration, &args.path) {
                    Ok(pending) => {
                        let source = self
                            .read_source(&args.path, "task_open_target", self.max_doc_bytes())
                            .and_then(|source| {
                                if source.identity().digest != pending.source.identity().digest
                                    || source.identity().bytes != pending.source.identity().bytes
                                {
                                    return Err(fail_json(json!({
                                        "code": "task_input_drift",
                                        "error": format!(
                                            "target {:?} changed while its task declaration was being bound",
                                            args.path
                                        ),
                                        "task_id": pending.task.task_id,
                                    })));
                                }
                                Ok(source)
                            });
                        pending_task = Some(pending);
                        match source {
                            Ok(source) => source,
                            Err(failure) => return failure,
                        }
                    }
                    Err(failure) => return failure,
                }
            }
            (None, Some(task_id)) => match self.prepare_existing_task_open(task_id, &args.path) {
                Ok(source) => source,
                Err(failure) => return failure,
            },
            (None, None) => {
                match self
                    .artifacts
                    .read_source(&args.path, "input_docx", self.max_doc_bytes())
                {
                    Ok(source) => source,
                    Err(ArtifactError::SourceTooLarge { size, limit, .. }) => {
                        return fail_json(json!({
                            "code": "doc_too_large",
                            "error": format!(
                                "'{}' is {} bytes, over the {}-byte open limit. Raise \
                                 {ENV_MAX_DOC_BYTES} to open larger files (or set it to 0 to \
                                 disable the cap).",
                                args.path, size, limit,
                            ),
                            "path": args.path,
                            "size_bytes": size,
                            "limit_bytes": limit,
                            "env_var": ENV_MAX_DOC_BYTES,
                        }));
                    }
                    Err(error) => return artifact_fail(error),
                }
            }
            (Some(_), Some(_)) => unreachable!("mutual exclusion checked above"),
        };
        let input_artifact = source.identity().clone();
        let import = match self.runtime.import_docx(source.bytes()) {
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
        self.source_artifacts
            .lock()
            .expect("source_artifacts mutex poisoned")
            .insert(
                doc_id.clone(),
                SessionSources::new(vec![input_artifact.clone()]),
            );
        if task_id.is_some()
            && let Err(failure) = self.register_task_doc(
                pending_task,
                task_id.as_deref(),
                &input_artifact.resolved_path,
                &doc_id,
            )
        {
            return failure;
        }
        let page = match self.core_index_page(&doc_id, 0, DEFAULT_CORE_INDEX_LIMIT) {
            Ok(page) => page,
            Err(failure) => return failure,
        };
        // The document's origin authors (the existing redline's authors, off
        // limits to the impersonation guard) are captured by the engine
        // itself at import time — see `EditSnapshot::guard_author` /
        // `SnapshotMeta::origin_authors`. No transport-side bookkeeping
        // needed here.
        ok(json!({
            "doc_id": doc_id,
            "block_count": page["total_blocks"],
            "total_chars": page["total_chars"],
            "server_version": SERVER_VERSION,
            "task_id": task_id,
            "input_artifact": input_artifact,
            "index": page["entries"],
            "index_offset": page["offset"],
            "index_limit": page["limit"],
            "index_returned": page["returned"],
            "index_has_more": page["has_more"],
            "index_next_offset": page["next_offset"],
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
                .or_else(|| {
                    cell_paragraph_detail_json(&view, &target, InspectBlockDetail::Formatting)
                })
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
                       Returns matching block ids with role, a match-centered text excerpt of at \
                       most 240 characters, exact text length, role_token, list membership, \
                       and (for table matches) a PAGED matching_cells locator list as \
                       [{row,col,block_id,text_excerpt,text_chars,text_truncated}]. Each table \
                       returns at most 4 cells by default plus its true matching_cell_count, \
                       returned/has_more/next_offset; pass cell_offset/cell_limit (maximum 64) \
                       to retrieve the rest. Inspect a returned cell block_id for exact text, \
                       anchors, or formatting. Use \
                       this when you know the wording but not the block id; inspect a returned \
                       block id for exact full text."
    )]
    async fn find(&self, Parameters(args): Parameters<FindArgs>) -> CallToolResult {
        let offset = args.offset.unwrap_or(0);
        let limit = args.limit.unwrap_or(DEFAULT_FIND_LIMIT);
        let cell_offset = args.cell_offset.unwrap_or(0);
        let cell_limit = args.cell_limit.unwrap_or(DEFAULT_FIND_CELL_LIMIT);
        if limit == 0 || limit > MAX_FIND_LIMIT {
            return fail(
                "invalid_argument",
                format!("find limit must be between 1 and {MAX_FIND_LIMIT}, got {limit}"),
            );
        }
        if cell_limit == 0 || cell_limit > MAX_FIND_CELL_LIMIT {
            return fail(
                "invalid_argument",
                format!(
                    "find cell_limit must be between 1 and {MAX_FIND_CELL_LIMIT}, got {cell_limit}"
                ),
            );
        }
        let handle = DocHandle(args.doc_id.clone());
        let result = self.runtime.with(&handle, move |snap| {
            let view = build_document_view(snap);
            find_page(&view, &args.pattern, offset, limit, cell_offset, cell_limit)
        });
        match result {
            Ok(Ok(payload)) => ok(payload),
            Ok(Err(message)) => fail("invalid_argument", message),
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
        let submitted_operations = args.transaction.operation_count();
        let txn_json = args.transaction.to_json_string();
        // Resolve any image `path` alternative to `bytes_base64` before parsing.
        let (txn_json, image_sources) = match resolve_image_paths(
            &self.artifacts,
            &txn_json,
            self.max_image_bytes(),
            self.max_image_total_bytes(),
        ) {
            Ok(resolved) => resolved,
            Err(f) => {
                return attach_known_transaction_outcomes(f, submitted_operations, false);
            }
        };

        // Parse + schema-validate at the edge (parse_transaction does both).
        let v4 = match parse_transaction(&txn_json) {
            Ok(v) => v,
            Err(e) => {
                return attach_known_transaction_outcomes(
                    fail(
                        "schema_error",
                        augment_schema_error(&txn_json, &e.to_string()),
                    ),
                    submitted_operations,
                    false,
                );
            }
        };
        let operation_count = v4.ops.len();
        let mut txn = match v4.into_edit_transaction() {
            Ok(t) => t,
            Err(e) => {
                return attach_transaction_outcomes(
                    fail("adapter_error", e.to_string()),
                    operation_count,
                    false,
                );
            }
        };

        // Per-call mode override, parsed at the edge with no silent fallback.
        // Absent => keep whatever the transaction body declared (default tracked).
        // "direct" applies immediately with no w:ins/w:del markup.
        match parse_materialization_mode(&args.mode) {
            Ok(Some(m)) => txn.materialization_mode = m,
            Ok(None) => {}
            Err(msg) => {
                return attach_transaction_outcomes(
                    fail("invalid_argument", msg),
                    operation_count,
                    false,
                );
            }
        }

        let handle = DocHandle(args.doc_id.clone());
        let result =
            self.apply_edit_with_sources(&handle, &txn, args.allow_existing_author, image_sources);
        attach_transaction_outcomes(result, operation_count, false)
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
        self.apply_edit_receipt_outcome(handle, txn, allow_existing_author)
            .0
    }

    /// As `apply_edit_receipt`, plus whether the runtime mutation committed.
    /// The boolean remains true if only post-apply receipt construction fails.
    fn apply_edit_receipt_outcome(
        &self,
        handle: &DocHandle,
        txn: &stemma::edit::EditTransaction,
        allow_existing_author: bool,
    ) -> (CallToolResult, bool) {
        if let Some(failure) = self.refuse_direct_task_mutation(&handle.0, "direct mutation tool") {
            return (failure, false);
        }
        // Capture the pre-edit canonical so the receipt can name exactly which
        // blocks changed (honest before/after structural diff) and which
        // revision ids are newly created.
        let before = match self
            .runtime
            .with(handle, |snap| Arc::clone(&snap.canonical))
        {
            Ok(c) => c,
            Err(e) => {
                return (
                    fail(
                        &format!("{:?}", e.code),
                        format!("doc not open: {}", e.message),
                    ),
                    false,
                );
            }
        };
        // Semantic identities are deterministic record keys, not counters.
        // Capture the exact pre-edit set so the receipt can report the
        // after-minus-before set without assuming numeric ordering.
        let before_revision_ids: HashSet<u32> = revision_rows(&before)
            .iter()
            .map(|r| r.revision_id)
            .collect();

        match self
            .runtime
            .apply_edit_authored(handle, txn, allow_existing_author)
        {
            Ok(result) => {
                let changed = changed_block_ids(&before, &result.canonical);
                // Enumerate the AFTER-doc's actually-present revisions with the
                // SAME walk list_revisions uses and subtract the captured set:
                // receipt == read surface by construction, with no counter or
                // watermark assumption.
                let revision_ids = match self
                    .runtime
                    .with(&handle.clone(), |snap| revision_rows(&snap.canonical))
                {
                    Ok(rows) => {
                        let mut ids: Vec<u32> = rows
                            .iter()
                            .map(|r| r.revision_id)
                            .filter(|id| !before_revision_ids.contains(id))
                            .collect();
                        ids.sort_unstable();
                        ids.dedup();
                        ids
                    }
                    Err(e) => {
                        return (
                            fail(
                                &format!("{:?}", e.code),
                                format!("doc not open after edit: {}", e.message),
                            ),
                            result.applied,
                        );
                    }
                };
                let (changed_blocks, block_count) =
                    match self.changed_block_rows(&handle.0, &changed) {
                        Ok(v) => v,
                        Err(r) => return (r, result.applied),
                    };
                let moves = move_receipts(&before, &result.canonical);
                let table_rows_changed = table_receipts(&before, &result.canonical);
                (
                    ok(Self::bounded_transaction_receipt(json!({
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
                    }))),
                    result.applied,
                )
            }
            // The engine's RuntimeError carries an actionable message and a code
            // (e.g. StaleEdit, OpaqueDestroyed, AnchorNotFound, NoOpEdit).
            Err(e) => (
                fail_json(json!({
                    "code": format!("{:?}", e.code),
                    "error": e.message,
                    "details": format!("{:?}", e.details),
                })),
                false,
            ),
        }
    }

    #[tool(
        description = "Export an open document to a .docx file at the given path, \
                       including any tracked changes applied. Runs a fresh session audit and \
                       refuses before path creation unless that audit is deliverable; then runs \
                       the blocking serialization gate and commits create-new. Returns exact \
                       input/output identities, the passing audit decision commitment, and \
                       validation result. In a declared task, earlier target saves remain \
                       task-pending; the last target save writes a create-once complete or \
                       partial manifest and returns success only when every effect is satisfied."
    )]
    async fn save_docx(&self, Parameters(args): Parameters<SaveArgs>) -> CallToolResult {
        let handle = DocHandle(args.doc_id.clone());
        let _session_guard = self
            .artifact_session_gate
            .lock()
            .expect("artifact_session_gate mutex poisoned");
        let task_binding = match self.prepare_task_save(&args.doc_id, &args.path) {
            Ok(binding) => binding,
            Err(failure) => return failure,
        };
        // Bind the save to a fresh audit of this exact in-memory generation.
        // Audit detail stays out of the save receipt; its exact decision counts,
        // verdict, and commitment remain inline below.
        let audit_report = match self.runtime.review_session(&handle) {
            Ok(report) => report,
            Err(error) => return fail(&format!("{:?}", error.code), error.message),
        };
        let baseline_bytes = match self.runtime.session_source_bytes(&handle) {
            Ok(bytes) => bytes,
            Err(error) => return fail(&format!("{:?}", error.code), error.message),
        };
        let resolution_evidence = match self.session_resolution_evidence(&args.doc_id) {
            Ok(evidence) => evidence,
            Err(failure) => return failure,
        };
        let mut audit =
            audit_report_json(&audit_report, Some(&resolution_evidence), None, None, None)
                .expect("default audit page coordinates are valid");
        attach_baseline_validation(
            &mut audit,
            &stemma::api::validate(&baseline_bytes),
            &audit_report.validator,
        );
        if audit["verdict"]["deliverable"] != true {
            return fail_json(json!({
                "code": "verification_failed",
                "error": "save refused: the fresh session audit is not deliverable; no output path was created",
                "audit": {
                    "counts": audit["counts"],
                    "verdict": audit["verdict"],
                },
                "remediation": {
                    "kind": "inspect_verification",
                    "tool": "verify_docx",
                    "arguments": {"doc_id": args.doc_id},
                },
            }));
        }
        let audit_decision = json!({
            "counts": audit["counts"],
            "verdict": audit["verdict"],
        });
        let audit_set_sha256 = canonical_set_sha256(std::slice::from_ref(&audit_decision));
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
        let input_artifacts = match self.protected_sources(&args.doc_id) {
            Ok(artifacts) => artifacts,
            Err(failure) => return failure,
        };
        let input_artifacts =
            match self.task_protected_sources(task_binding.as_ref(), input_artifacts) {
                Ok(artifacts) => artifacts,
                Err(failure) => return failure,
            };
        let output_artifact = match self.artifacts.commit_new(
            &args.path,
            "output_docx",
            &bytes,
            &input_artifacts,
        ) {
            Ok(output) => output,
            Err(error) => {
                let failure_message = error.to_string();
                let task_failure =
                    match self.record_task_write_failure(task_binding.as_ref(), &failure_message) {
                        Ok(outcome) => outcome,
                        Err(failure) => return failure,
                    };
                return match task_failure {
                    TaskWriteFailureOutcome::Retryable => artifact_fail(error),
                    TaskWriteFailureOutcome::Partial {
                        task_id,
                        manifest,
                        unsatisfied_effects,
                    } => fail_json(json!({
                        "code": "task_partial",
                        "error": format!(
                            "task {task_id:?} terminated partial after an output commit failed: {failure_message}"
                        ),
                        "task": {
                            "task_id": task_id,
                            "status": "partial",
                            "manifest": manifest,
                            "unsatisfied_effects": unsatisfied_effects,
                        },
                        "failed_output": {
                            "path": args.path,
                            "committed": false,
                            "error": failure_message,
                        },
                        "verdict": {"status": "partial", "deliverable": false},
                    })),
                };
            }
        };
        let audit_scope = if task_binding.is_some() {
            "declared_task_to_saved_output"
        } else {
            "open_session_to_saved_output"
        };
        let audit_binding = json!({
            "doc_id": args.doc_id,
            "scope": audit_scope,
            "output_sha256": output_artifact.identity.digest.hex.clone(),
            "set_sha256": audit_set_sha256,
            "counts": audit_decision["counts"],
            "verdict": audit_decision["verdict"],
        });
        let task_outcome = if task_binding.is_some() {
            let typed_audit_binding = serde_json::from_value(audit_binding.clone())
                .expect("the task save audit receipt always matches the task audit model");
            let committed_revision_ids: HashSet<u32> = audit_report
                .new_revisions
                .iter()
                .map(|revision| revision.revision_id)
                .collect();
            match self.record_task_save(
                task_binding.as_ref(),
                &output_artifact,
                typed_audit_binding,
                &committed_revision_ids,
            ) {
                Ok(outcome) => outcome,
                Err(failure) => return failure,
            }
        } else {
            TaskSaveOutcome::NotFinal
        };
        let document_receipt = json!({
            "path": args.path,
            "bytes_written": bytes.len(),
            "input_artifacts": input_artifacts,
            "output_artifact": output_artifact,
            "audit_binding": audit_binding,
            "validation": {"level": "blocking", "ok": true},
        });
        match task_outcome {
            TaskSaveOutcome::NotFinal if task_binding.is_none() => {
                let mut payload = document_receipt;
                payload["verdict"] = json!({"status": "pass", "deliverable": true});
                payload["server_version"] = json!(SERVER_VERSION);
                ok(payload)
            }
            TaskSaveOutcome::NotFinal => ok(json!({
                "document": document_receipt,
                "task": {
                    "task_id": task_binding.expect("task save has a binding").task_id,
                    "status": "executing",
                    "deliverable": false,
                },
                "verdict": {"status": "task_pending", "deliverable": false},
                "server_version": SERVER_VERSION,
            })),
            TaskSaveOutcome::Complete { task_id, manifest } => ok(json!({
                "document": document_receipt,
                "task": {
                    "task_id": task_id,
                    "status": "complete",
                    "manifest": manifest,
                },
                "verdict": {"status": "pass", "deliverable": true},
                "server_version": SERVER_VERSION,
            })),
            TaskSaveOutcome::Partial {
                task_id,
                manifest,
                unsatisfied_effects,
            } => fail_json(json!({
                "code": "task_partial",
                "error": format!(
                    "task {task_id:?} terminated partial; unsatisfied effects: {}",
                    unsatisfied_effects.join(", ")
                ),
                "document": document_receipt,
                "task": {
                    "task_id": task_id,
                    "status": "partial",
                    "manifest": manifest,
                    "unsatisfied_effects": unsatisfied_effects,
                },
                "verdict": {"status": "partial", "deliverable": false},
            })),
        }
    }

    #[tool(
        description = "Compare two .docx files and write a redline .docx (the target with \
                       tracked changes relative to the base) to out_path. Returns the \
                       number of detected changes."
    )]
    async fn compare_docx(&self, Parameters(args): Parameters<CompareArgs>) -> CallToolResult {
        let base = match self.read_source(&args.base_path, "base_docx", self.max_doc_bytes()) {
            Ok(source) => source,
            Err(failure) => return failure,
        };
        let target = match self.read_source(&args.target_path, "target_docx", self.max_doc_bytes())
        {
            Ok(source) => source,
            Err(failure) => return failure,
        };
        let (base_import, target_import) =
            match self.runtime.import_docx_pair(base.bytes(), target.bytes()) {
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
        let input_artifacts = vec![base.identity().clone(), target.identity().clone()];
        let output_artifact = match self.artifacts.commit_new(
            &args.out_path,
            "output_redline",
            &result.redline_bytes,
            &input_artifacts,
        ) {
            Ok(output) => output,
            Err(error) => return artifact_fail(error),
        };
        ok(json!({
            "out_path": args.out_path,
            "change_count": result.diff.changes.len(),
            "bytes_written": result.redline_bytes.len(),
            "input_artifacts": input_artifacts,
            "output_artifact": output_artifact,
            "server_version": SERVER_VERSION,
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
                identity: 0,
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
                    "expected {} match(es) but found {actual}; if one site is intended, \
                     narrow the target (longer old text, or scope:{{block_id}} from the \
                     listed matches) — raise expected_matches or use \"all\" only after \
                     verifying every listed match is intended",
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
                identity: 0,
                author: Some(options.author.clone()),
                date: None,
                apply_op_id: None,
            },
        };

        let unreached =
            unreached_cells_json(&canonical, &options.old, options.match_mode, &options.scope);
        let mut receipt =
            self.apply_edit_receipt(&handle, &transaction, args.allow_existing_author);
        attach_field(&mut receipt, "match_count", json!(match_count));
        attach_field(&mut receipt, "matches", json!(matches));
        attach_field(&mut receipt, "normalization_applied", json!(normalization));
        attach_field(&mut receipt, "skipped_straddles", skipped);
        attach_field(&mut receipt, "unreached_matches", unreached);
        receipt
    }

    fn replace_text_batch_impl(&self, args: ReplaceTextBatchArgs) -> CallToolResult {
        if args.author.trim().is_empty() {
            return fail("invalid_argument", "author must be a non-empty string");
        }
        if args.replacements.is_empty() {
            return fail("invalid_argument", "replacements must be a non-empty list");
        }

        let submitted = args.replacements.len();
        let handle = DocHandle(args.doc_id.clone());
        let mut items: Vec<Value> = Vec::with_capacity(args.replacements.len());
        let mut applied = 0usize;
        let mut failed = 0usize;
        let mut preview_snapshot = if args.preview {
            match self.runtime.with(&handle, Clone::clone) {
                Ok(snapshot) => Some(snapshot),
                Err(e) => {
                    return fail(
                        &format!("{:?}", e.code),
                        format!("doc not open: {}", e.message),
                    );
                }
            }
        } else {
            None
        };

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
            let canonical = if let Some(snapshot) = &preview_snapshot {
                Arc::clone(&snapshot.canonical)
            } else {
                match self
                    .runtime
                    .with(&handle, |snap| Arc::clone(&snap.canonical))
                {
                    Ok(canonical) => canonical,
                    Err(e) => {
                        return fail(
                            &format!("{:?}", e.code),
                            format!("doc not open: {}", e.message),
                        );
                    }
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
                    let before = Arc::clone(&canonical);
                    let before_revision_ids: HashSet<u32> = revision_rows(&before)
                        .into_iter()
                        .map(|revision| revision.revision_id)
                        .collect();
                    let transaction = stemma::edit::EditTransaction {
                        steps: plan.steps,
                        summary: Some("replace_text_batch".to_string()),
                        materialization_mode: stemma::edit::MaterializationMode::TrackedChange,
                        revision: stemma::RevisionInfo {
                            revision_id: 0,
                            identity: 0,
                            author: Some(args.author.clone()),
                            date: None,
                            apply_op_id: None,
                        },
                    };
                    let outcome = if let Some(snapshot) = &preview_snapshot {
                        snapshot
                            .apply_authored(&transaction, args.allow_existing_author)
                            .map(|next| (Arc::clone(&next.canonical), Some(next)))
                    } else {
                        self.runtime
                            .apply_edit_authored(&handle, &transaction, args.allow_existing_author)
                            .map(|result| (result.canonical, None))
                    };
                    match outcome {
                        Ok((after, next_preview)) => {
                            let changed = changed_block_ids(&before, &after);
                            let mut revision_ids: Vec<u32> = revision_rows(&after)
                                .into_iter()
                                .map(|revision| revision.revision_id)
                                .filter(|identity| !before_revision_ids.contains(identity))
                                .collect();
                            revision_ids.sort_unstable();
                            revision_ids.dedup();
                            let unreached = unreached_cells_json(
                                &before,
                                &item.old,
                                match_mode,
                                &options.scope,
                            );
                            let status = if args.preview {
                                "would_apply"
                            } else {
                                "applied"
                            };
                            items.push(json!({"index": index, "old": item.old, "status": status,
                                "match_count": match_count, "matches": matched,
                                "revision_ids": revision_ids,
                                "changed_blocks": changed,
                                "normalization_applied": normalization,
                                "skipped_straddles": skipped,
                                "unreached_matches": unreached}));
                            if let Some(next_preview) = next_preview {
                                preview_snapshot = Some(next_preview);
                            }
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

        let outcomes = CompleteDecisionOutcomes::new("replacement worklist", submitted, items);
        ok(json!({
            "doc_id": args.doc_id,
            "author": args.author,
            "submitted": submitted,
            "preview": args.preview,
            "applied": if args.preview { 0 } else { applied },
            "would_apply": if args.preview { Some(applied) } else { None },
            "failed": failed,
            "items": outcomes.into_rows(),
        }))
    }

    #[tool(
        description = "Apply a whole tracked find/replace worklist in one call. \
                       Items run in order and report complete per-item outcomes; one \
                       refusal does not hide other outcomes. preview=true uses a \
                       throwaway snapshot."
    )]
    async fn replace_text_batch(
        &self,
        Parameters(args): Parameters<ReplaceTextBatchArgs>,
    ) -> CallToolResult {
        if let Some(failure) = self.refuse_direct_task_mutation(&args.doc_id, "replace_text_batch")
        {
            return failure;
        }
        self.replace_text_batch_impl(args)
    }
}

/// The wire string for an `ExpectedMatches` (for the mismatch error).
fn expected_matches_label(e: &stemma::edit::ExpectedMatches) -> String {
    match e {
        stemma::edit::ExpectedMatches::All => "all".to_string(),
        stemma::edit::ExpectedMatches::Count(n) => n.to_string(),
    }
}

/// Table-cell occurrences of `needle` that WholeDoc's legacy top-level scan did
/// not reach, as JSON for the receipt's honesty disclosure. Explicit MCP scopes
/// describe the complete intended search boundary, so matches outside one are
/// deliberately out of scope rather than "unreached".
fn unreached_cells_json(
    doc: &stemma::CanonDoc,
    needle: &str,
    mode: stemma::edit::MatchMode,
    scope: &stemma::edit::ReplaceTextScope,
) -> Value {
    if !matches!(scope, stemma::edit::ReplaceTextScope::WholeDoc) {
        return json!([]);
    }
    let cells = stemma::edit::unreached_cell_matches(doc, needle, mode);
    json!(
        cells
            .iter()
            .map(|m| json!({
                "region": "table_cell",
                "block_id": m.paragraph_id.to_string(),
                "table_id": m.table_id.to_string(),
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
/// body and table-cell paragraphs. A single `block_id`, OR both
/// `from_block_id`+`to_block_id`. Mixing
/// the two forms, or supplying only one range endpoint, is rejected.
fn parse_replace_text_scope(
    arg: &Option<ReplaceTextScopeArg>,
) -> Result<stemma::edit::ReplaceTextScope, String> {
    let Some(s) = arg else {
        return Ok(stemma::edit::ReplaceTextScope::BodyAndTables);
    };
    match (&s.block_id, &s.from_block_id, &s.to_block_id) {
        (None, None, None) => Ok(stemma::edit::ReplaceTextScope::BodyAndTables),
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
    let Some(Value::Object(mut payload)) = result.structured_content.clone() else {
        return;
    };
    payload.insert(key.to_string(), value);
    *result = CallToolResult::structured(Value::Object(payload)).with_meta(result.meta.clone());
}

/// Mark a successful, complete preview with the exact state transition it has
/// unlocked. This is deliberately response-local rather than more repeated
/// tool-description prose: one small cue at the decision point can avoid an
/// entire read/reformulate/preview loop. Partial worklists (one or more failed
/// items) never receive the cue.
fn attach_preview_apply_cue(result: &mut CallToolResult) {
    if result.is_error == Some(true) {
        return;
    }
    let Some(Value::Object(payload)) = result.structured_content.as_ref() else {
        return;
    };
    let would_apply = match payload.get("would_apply") {
        Some(Value::Bool(value)) => *value,
        Some(Value::Number(value)) => value.as_u64().is_some_and(|count| count > 0),
        _ => false,
    };
    let has_failures = payload
        .get("failed")
        .and_then(Value::as_u64)
        .is_some_and(|count| count > 0);
    if !would_apply || has_failures {
        return;
    }
    attach_field(result, "apply_ready", json!(true));
    attach_field(
        result,
        "next_action",
        json!(
            "When this preview covers all intended changes, call execute_plan with the identical plan and preview=false. No re-inspection is needed unless document state changes."
        ),
    );
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

/// Parsed `by_kind` filter: an exact revision kind, or "format" as the group
/// alias for all *PrChange kinds (the common "show me formatting changes"
/// query, without naming each carrier). ONE vocabulary, ONE parser — shared
/// by the revision-inventory filter and the resolution `by_filter` selector
/// so the two surfaces can never drift apart.
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

    fn parse(raw: &str) -> Result<Self, String> {
        if raw == "format" {
            return Ok(Self::AnyFormat);
        }
        match RevisionKind::parse(raw) {
            Some(kind) => Ok(Self::Exact(kind)),
            None => Err(format!(
                "by_kind must be \"insert\", \"delete\", \"format\" (any formatting \
                 change), \"format_run\", \"format_paragraph\", \"format_table\", \
                 \"format_row\", \"format_cell\", \"format_section\", or \
                 \"opaque_interior\" (tracked changes inside embedded content — \
                 visible but not individually resolvable); got {raw:?}"
            )),
        }
    }
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
        // Filter parsing + application is shared with the revisions_summary
        // projection (one code path — see filtered_revision_rows).
        let rows = match self.filtered_revision_rows(&args.doc_id, args.filter.as_ref()) {
            Ok(rows) => rows,
            Err(result) => return result,
        };

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
fn resolve_image_paths(
    authority: &PathAuthority,
    txn_json: &str,
    max_image_bytes: Option<u64>,
    max_total_bytes: Option<u64>,
) -> Result<(String, Vec<ArtifactIdentity>), CallToolResult> {
    use base64::Engine as _;
    let Ok(mut value) = serde_json::from_str::<Value>(txn_json) else {
        return Ok((txn_json.to_string(), Vec::new()));
    };
    let Some(ops) = value.get_mut("ops").and_then(Value::as_array_mut) else {
        return Ok((txn_json.to_string(), Vec::new()));
    };
    let mut sources = Vec::new();
    let mut total_bytes = 0_u64;
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
                        "ops[{i}]: an image op needs its bytes — supply `path` (inside the \
                         MCP workspace root, read server-side) or `bytes_base64` (the \
                         base64-encoded bytes)"
                    ),
                ));
            }
            (false, Some(path)) => {
                let source = match authority.read_source(
                    &path,
                    format!("input_image_{i}"),
                    max_image_bytes,
                ) {
                    Ok(source) => source,
                    Err(ArtifactError::SourceTooLarge { size, limit, .. }) => {
                        return Err(fail_json(json!({
                            "code": "artifact_source_too_large",
                            "error": format!(
                                "image path {path:?} is {size} bytes, over the {limit}-byte \
                                 per-image limit; reduce the image or raise {ENV_MAX_IMAGE_BYTES}"
                            ),
                            "path": path,
                            "size_bytes": size,
                            "limit_bytes": limit,
                            "env_var": ENV_MAX_IMAGE_BYTES,
                        })));
                    }
                    Err(error) => return Err(artifact_fail(error)),
                };
                total_bytes = total_bytes.saturating_add(source.identity().bytes);
                if let Some(limit) = max_total_bytes
                    && total_bytes > limit
                {
                    return Err(fail_json(json!({
                        "code": "artifact_source_too_large",
                        "error": format!(
                            "image paths in this edit total {total_bytes} bytes, over the \
                             {limit}-byte aggregate limit; reduce the transaction or raise \
                             {ENV_MAX_IMAGE_TOTAL_BYTES}"
                        ),
                        "size_bytes": total_bytes,
                        "limit_bytes": limit,
                        "env_var": ENV_MAX_IMAGE_TOTAL_BYTES,
                    })));
                }
                let encoded = base64::engine::general_purpose::STANDARD.encode(source.bytes());
                obj.remove("path");
                obj.insert("bytes_base64".to_string(), Value::String(encoded));
                sources.push(source.identity().clone());
            }
            (true, None) => {
                // Already carries bytes; nothing to resolve.
            }
        }
    }
    Ok((value.to_string(), sources))
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
    /// AND-combined filter over the same axes and vocabulary as the revision
    /// inventory filter: author ∧ kind ∧ block-range. At least one axis is
    /// required — an all-empty filter would be `all` in disguise and fails
    /// loudly instead of resolving everything by accident. This is the
    /// selector for prompts shaped like "reject author X's changes in
    /// Section Y": one call, no id enumeration.
    ByFilter {
        /// Same contract as the inventory filter's `by_author` (exact match;
        /// an anonymized revision never matches).
        #[serde(default)]
        by_author: Option<String>,
        /// Same vocabulary as the inventory filter's `by_kind` ("insert",
        /// "delete", "format", or an exact formatting-change kind).
        #[serde(default)]
        by_kind: Option<String>,
        /// Same contract as the inventory filter's `by_block_range`
        /// (inclusive, document order; unknown endpoint => AnchorNotFound).
        #[serde(default)]
        by_block_range: Option<BlockRange>,
    },
    /// Every tracked change in the document (collected from the read view, one
    /// code path with the other selectors — not `Resolution::AcceptAll`).
    All,
}

fn change_selector_json(selector: &ChangeSelector) -> Value {
    match selector {
        ChangeSelector::ByIds { revision_ids } => {
            json!({"by": "by_ids", "revision_ids": revision_ids})
        }
        ChangeSelector::ByAuthor { author } => json!({"by": "by_author", "author": author}),
        ChangeSelector::ByRange {
            from_block_id,
            to_block_id,
        } => json!({
            "by": "by_range",
            "from_block_id": from_block_id,
            "to_block_id": to_block_id,
        }),
        ChangeSelector::ByFilter {
            by_author,
            by_kind,
            by_block_range,
        } => json!({
            "by": "by_filter",
            "by_author": by_author,
            "by_kind": by_kind,
            "by_block_range": by_block_range.as_ref().map(|range| json!({
                "from_block_id": range.from_block_id,
                "to_block_id": range.to_block_id,
            })),
        }),
        ChangeSelector::All => json!({"by": "all"}),
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ResolutionActionArg {
    Accept,
    Reject,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ResolutionPlanArg {
    /// Whether the selected pending revisions are accepted or rejected.
    action: ResolutionActionArg,
    /// Explicit revision selection. Prefer the bulk selectors (by_author,
    /// by_range, by_filter for author ∧ kind ∧ block-range conjunctions, all)
    /// over enumerating ids — one selector call replaces the whole
    /// inventory-then-id-list round trip. Obtain authors, kinds, and block
    /// ids from inspect_docx.
    selector: ChangeSelector,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReplacementWorklistArg {
    /// Author stamped on every tracked replacement in the worklist.
    author: String,
    /// Literal replacements compiled and applied server-side in order. Each
    /// item has the same exact-count and barrier contract as replace_text.
    replacements: Vec<CoreReplacementItem>,
}

#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum CoreReplacementMatchMode {
    Exact,
    NormalizeWs,
}

impl CoreReplacementMatchMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::NormalizeWs => "normalize_ws",
        }
    }
}

fn default_core_replacement_match_mode() -> CoreReplacementMatchMode {
    CoreReplacementMatchMode::Exact
}

#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum CoreBarrierPolicy {
    Skip,
    Fail,
}

impl CoreBarrierPolicy {
    fn as_str(self) -> &'static str {
        match self {
            Self::Skip => "skip",
            Self::Fail => "fail",
        }
    }
}

fn default_core_barrier_policy() -> CoreBarrierPolicy {
    CoreBarrierPolicy::Skip
}

/// Gemini's function schema cannot faithfully represent the advanced
/// `number | "all"` union: its schema cleaner collapses that union to string
/// and causes numeric counts to arrive as `"2"`. The compact edge models the
/// two domain states explicitly instead of accepting or guessing stringified
/// numbers.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CoreReplacementItem {
    /// Task effect association. Required in a task-bound session and forbidden
    /// otherwise.
    #[serde(default)]
    effect_id: Option<String>,
    old: String,
    new: String,
    #[serde(default)]
    scope: Option<ReplaceTextScopeArg>,
    /// Exact number of occurrences required. Defaults to 1 when omitted and
    /// replace_all is false.
    #[serde(default)]
    expected_matches: Option<usize>,
    /// Require and replace every occurrence. Mutually exclusive with
    /// expected_matches.
    #[serde(default)]
    replace_all: bool,
    /// Literal matching (default) or whitespace/typographic-quote folding.
    #[serde(default = "default_core_replacement_match_mode")]
    match_mode: CoreReplacementMatchMode,
    /// Skip a match that crosses an opaque/revision wall (default), or fail
    /// this worklist item explicitly.
    #[serde(default = "default_core_barrier_policy")]
    on_barrier_match: CoreBarrierPolicy,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ExecutePlanArgs {
    /// The doc_id returned by open_docx. Required for transaction, resolution,
    /// and replacement_worklist; omit only for a comparison producer plan.
    #[serde(default)]
    doc_id: Option<String>,
    /// One atomic v4 edit transaction. Mutually exclusive with resolution.
    #[serde(default)]
    transaction: Option<TransactionArg>,
    /// One explicit accept/reject selection. Mutually exclusive with transaction.
    #[serde(default)]
    resolution: Option<ResolutionPlanArg>,
    /// Server-side literal replacement worklist. Mutually exclusive with
    /// transaction and resolution. This is the compact bulk-authoring path:
    /// use replace_all=true only when every occurrence is intended.
    #[serde(default)]
    replacement_worklist: Option<ReplacementWorklistArg>,
    /// Producer path equivalent to the advanced compare_docx capability:
    /// compare base_path to target_path and write a tracked redline to out_path.
    /// Mutually exclusive with every doc_id-backed plan.
    #[serde(default)]
    comparison: Option<ComparisonPlanArg>,
    /// Validate and report the exact outcome without mutation when true.
    preview: bool,
    /// "tracked" (default) or "direct" for transaction plans only.
    #[serde(default)]
    mode: Option<String>,
    /// Author-impersonation override for transaction plans only.
    #[serde(default)]
    allow_existing_author: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ComparisonPlanArg {
    /// Path to the base/original DOCX under the workspace root.
    base_path: String,
    /// Path to the target/modified DOCX under the workspace root.
    target_path: String,
    /// New redline path under the workspace root.
    out_path: String,
    /// Author stamped on the generated tracked changes. Defaults to "stemma".
    #[serde(default)]
    author: Option<String>,
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
    /// New output path for the rendered redline under the MCP workspace root.
    path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum AuditDetail {
    Census,
    DirectDelta,
    Preexisting,
    Violations,
    ValidatorIssues,
}

impl AuditDetail {
    fn as_str(self) -> &'static str {
        match self {
            Self::Census => "census",
            Self::DirectDelta => "direct_delta",
            Self::Preexisting => "preexisting",
            Self::Violations => "violations",
            Self::ValidatorIssues => "validator_issues",
        }
    }
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
    /// Return a particular audit section page. Omit for the bounded first page
    /// of every section.
    #[serde(default)]
    detail: Option<AuditDetail>,
    /// Zero-based row offset for detail. Valid only when detail is set.
    #[serde(default)]
    offset: Option<usize>,
    /// Detail page size, 1..=64. Valid only when detail is set.
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct AuditDocxArgs {
    /// Path of the baseline .docx under the MCP workspace root.
    before_path: String,
    /// Path of the .docx to certify under the MCP workspace root.
    after_path: String,
    /// When set, additionally materialize the before → after delta as a
    /// tracked-changes .docx at `render.path`.
    #[serde(default)]
    render: Option<RenderSpec>,
    /// Return a particular audit section page. Omit for the bounded first page
    /// of every section.
    #[serde(default)]
    detail: Option<AuditDetail>,
    /// Zero-based row offset for detail. Valid only when detail is set.
    #[serde(default)]
    offset: Option<usize>,
    /// Detail page size, 1..=64. Valid only when detail is set.
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct VerifyDocxArgs {
    /// Verify changes made since open_docx. Mutually exclusive with the path pair.
    #[serde(default)]
    doc_id: Option<String>,
    /// Baseline for producer-neutral verification. Requires after_path and no doc_id.
    #[serde(default)]
    before_path: Option<String>,
    /// Changed file for producer-neutral verification. Requires before_path and no doc_id.
    #[serde(default)]
    after_path: Option<String>,
    /// Optionally materialize the verified delta as a create-new redline.
    #[serde(default)]
    render: Option<RenderSpec>,
    /// Return a particular audit section page: census, direct_delta,
    /// preexisting, violations, or validator_issues. Omit for a bounded first
    /// page of every section.
    #[serde(default)]
    detail: Option<AuditDetail>,
    /// Zero-based row offset for detail. Valid only when detail is set.
    #[serde(default)]
    offset: Option<usize>,
    /// Detail page size, 1..=64. Valid only when detail is set.
    #[serde(default)]
    limit: Option<usize>,
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
    /// Cap only transaction EVIDENCE. `operation_outcomes` is attached later
    /// through `CompleteDecisionOutcomes` and can never enter this function.
    fn bounded_transaction_receipt(mut receipt: Value) -> Value {
        let object = receipt
            .as_object_mut()
            .expect("transaction receipt is always a JSON object");
        let mut truncated = serde_json::Map::new();
        for (field, count_field, cap) in [
            ("revision_ids", "revision_count", Self::RECEIPT_ID_CAP),
            (
                "changed_block_ids",
                "changed_block_count",
                Self::RECEIPT_ID_CAP,
            ),
            (
                "changed_blocks",
                "changed_block_rows_count",
                Self::RECEIPT_BLOCK_ROW_CAP,
            ),
            ("moves", "move_count", Self::RECEIPT_BLOCK_ROW_CAP),
            (
                "table_receipts",
                "table_receipt_count",
                Self::RECEIPT_BLOCK_ROW_CAP,
            ),
        ] {
            let complete = object
                .remove(field)
                .unwrap_or_else(|| Value::Array(Vec::new()));
            let Value::Array(complete) = complete else {
                panic!("transaction receipt field {field} must be an array");
            };
            let evidence = CappedEvidenceSet::new(complete, cap);
            object.insert(count_field.into(), json!(evidence.total));
            object.insert(field.into(), Value::Array(evidence.rows.clone()));
            object.insert(format!("{field}_evidence"), evidence.metadata());
            if evidence.omitted() > 0 {
                truncated.insert(format!("{field}_omitted"), json!(evidence.omitted()));
            }
        }
        if !truncated.is_empty() {
            truncated.insert(
                "advice".into(),
                json!(
                    "decision outcomes and exact counts are complete; inspect_docx and verify_docx expose omitted evidence"
                ),
            );
            object.insert("truncated".into(), Value::Object(truncated));
        }
        receipt
    }

    /// Lower an inclusive block-id range to the set of block ids it covers,
    /// in canonical document order. Shared by the `ByRange` and `ByFilter`
    /// selectors so the range contract (unknown endpoint => AnchorNotFound,
    /// out-of-order endpoints => InvalidRange) cannot drift between them.
    fn block_ids_in_range(
        &self,
        doc_id: &str,
        from_block_id: &str,
        to_block_id: &str,
    ) -> Result<std::collections::HashSet<String>, CallToolResult> {
        let handle = DocHandle(doc_id.to_string());
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
        let Some(from) = pos(from_block_id) else {
            return Err(fail(
                "AnchorNotFound",
                format!("range start block '{from_block_id}' not found in doc '{doc_id}'"),
            ));
        };
        let Some(to) = pos(to_block_id) else {
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
        Ok(order[from..=to].iter().cloned().collect())
    }

    /// Enumerate pending revisions with the optional AND-combined filter
    /// applied — the SHARED front half of `list_revisions` and the
    /// `revisions_summary` projection, so the filter semantics are one code
    /// path and cannot drift.
    fn filtered_revision_rows(
        &self,
        doc_id: &str,
        filter: Option<&RevisionFilter>,
    ) -> Result<Vec<RevisionRow>, CallToolResult> {
        // Parse the optional kind filter at the edge — no silent fallback.
        let kind_filter = match filter.and_then(|f| f.by_kind.as_deref()) {
            None => None,
            Some(raw) => match KindFilter::parse(raw) {
                Ok(parsed) => Some(parsed),
                Err(message) => return Err(fail("invalid_argument", message)),
            },
        };
        // The block-range filter is the only one that can fail loudly: an
        // unknown or out-of-order endpoint is a caller error, not an empty
        // result. Same contract as the accept/reject range selectors.
        let in_range: Option<std::collections::HashSet<String>> =
            match filter.and_then(|f| f.by_block_range.as_ref()) {
                None => None,
                Some(BlockRange {
                    from_block_id,
                    to_block_id,
                }) => Some(self.block_ids_in_range(doc_id, from_block_id, to_block_id)?),
            };
        let author_filter = filter.and_then(|f| f.by_author.as_deref());

        // One walk, then the AND-combined filters. revision_rows is the shared
        // enumeration; the filters here mirror the accept/reject selectors.
        let handle = DocHandle(doc_id.to_string());
        let rows: Vec<RevisionRow> = self
            .runtime
            .with(&handle, |snap| revision_rows(&snap.canonical))
            .map_err(|e| {
                fail(
                    &format!("{:?}", e.code),
                    format!("doc not open: {}", e.message),
                )
            })?;
        Ok(rows
            .into_iter()
            .filter(|r| {
                in_range
                    .as_ref()
                    .is_none_or(|set| set.contains(&r.block_id))
            })
            .filter(|r| author_filter.is_none_or(|a| r.author.as_deref() == Some(a)))
            .filter(|r| kind_filter.as_ref().is_none_or(|k| k.matches(r.kind)))
            .collect())
    }

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
    ///   endpoints => `InvalidRange`;
    /// - `ByFilter` AND-combines its axes with the same per-axis contracts;
    ///   an all-empty filter => `invalid_argument` (never an implicit `All`).
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
                let allowed = self.block_ids_in_range(doc_id, &from_block_id, &to_block_id)?;
                records
                    .iter()
                    .filter(|r| allowed.contains(r.block_id.to_string().as_str()))
                    .map(|r| r.revision_id)
                    .collect()
            }
            ChangeSelector::ByFilter {
                by_author,
                by_kind,
                by_block_range,
            } => {
                if by_author.is_none() && by_kind.is_none() && by_block_range.is_none() {
                    return Err(fail(
                        "invalid_argument",
                        "by_filter requires at least one of by_author, by_kind, or \
                         by_block_range — to resolve every tracked change, use the \
                         explicit {\"by\": \"all\"} selector instead",
                    ));
                }
                let kind_filter = match by_kind.as_deref() {
                    None => None,
                    Some(raw) => match KindFilter::parse(raw) {
                        Ok(filter) => Some(filter),
                        Err(message) => return Err(fail("invalid_argument", message)),
                    },
                };
                let allowed = match by_block_range {
                    None => None,
                    Some(BlockRange {
                        from_block_id,
                        to_block_id,
                    }) => Some(self.block_ids_in_range(doc_id, &from_block_id, &to_block_id)?),
                };
                records
                    .iter()
                    // Same author contract as ByAuthor: an anonymized revision
                    // (author == None) never matches a filter author.
                    .filter(|r| {
                        by_author
                            .as_deref()
                            .is_none_or(|a| r.author.as_deref() == Some(a))
                    })
                    .filter(|r| kind_filter.as_ref().is_none_or(|k| k.matches(r.kind)))
                    .filter(|r| {
                        allowed
                            .as_ref()
                            .is_none_or(|s| s.contains(r.block_id.to_string().as_str()))
                    })
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
        description = "Inspect an open DOCX through one bounded semantic surface. Omit query (or \
                       use 'index') for the first 16 rows of the compact current structural \
                       index; use offset/limit (maximum 256) only to page it. Use the explicit \
                       query='document' for a PAGED id-bearing extended-Markdown projection (default \
                       16 blocks; use offset/limit, maximum 256). Its prose is exact; tables are \
                       bounded summaries with four cell previews and route to query='block' for all \
                       cells. Prefer query='find' plus pattern to \
                       locate one known phrase or opaque metadata. For several known phrases, pass \
                       patterns=[...] (maximum 8) instead; every ordered pattern, including \
                       duplicates and zero matches, gets the same singular result shape with exact \
                       totals and continuation. Batch pages are capped at 16 matches per pattern, \
                       4 matching cells per table, and 256 KiB encoded. Then use \
                       query='block' plus block_id for compact exact text, guards, durable opaque \
                       anchors, nested table-cell paragraphs, and list identity. Table blocks return \
                       8 bounded cell locators by default; page them with cell_offset/cell_limit \
                       (maximum 64), then inspect a locator's block_ids for exact cell paragraphs. Pass \
                       detail='formatting' only when complete run marks/style properties are needed. \
                       Use query='window' plus \
                       from_block_id, to_block_id, and format ('text'|'markdown'|'html') for an \
                       inclusive bounded range. query='section' plus block_id returns one heading \
                       and its section. query='revisions_summary' returns exact pending-revision \
                       counts by author and kind with NO rows — START HERE on a dense document, \
                       then drill down. query='revisions' returns the pending-revision inventory; \
                       both take the optional filter={by_author?,by_kind?,by_block_range?: \
                       {from_block_id,to_block_id}}, AND-combined. To resolve a whole \
                       selection, pass the same axes to execute_plan's resolution \
                       selector (by='by_filter') instead of enumerating ids. \
                       query='styles' returns authored styles and document defaults; query='notes' \
                       returns footnote/endnote ids, kinds, and editable body text. The legacy \
                       comprehension projections remain explicit as query='text', query='html', \
                       query='redline', query='accepted', or query='rejected'. query='operations' \
                       returns every transaction op from \
                       the authoritative parser with its accepted fields, cues, and exact examples; \
                       pass pattern='<op_name>' to retrieve one operation. \
                       This tool is read-only; re-inspect a block after editing it because span \
                       handles and guards become stale."
    )]
    async fn inspect_docx(&self, Parameters(args): Parameters<InspectDocxArgs>) -> CallToolResult {
        let has_query_args = args.block_id.is_some()
            || args.detail.is_some()
            || args.pattern.is_some()
            || args.patterns.is_some()
            || args.filter.is_some()
            || args.from_block_id.is_some()
            || args.to_block_id.is_some()
            || args.format.is_some()
            || args.offset.is_some()
            || args.limit.is_some()
            || args.cell_offset.is_some()
            || args.cell_limit.is_some();
        let has_non_index_args = args.block_id.is_some()
            || args.detail.is_some()
            || args.pattern.is_some()
            || args.patterns.is_some()
            || args.filter.is_some()
            || args.from_block_id.is_some()
            || args.to_block_id.is_some()
            || args.format.is_some()
            || args.cell_offset.is_some()
            || args.cell_limit.is_some();
        match args.query {
            InspectQuery::Index => {
                if has_non_index_args {
                    return fail(
                        "invalid_argument",
                        "query 'index' accepts only doc_id, offset, and limit",
                    );
                }
                match self.core_index_page(
                    &args.doc_id,
                    args.offset.unwrap_or(0),
                    args.limit.unwrap_or(DEFAULT_CORE_INDEX_LIMIT),
                ) {
                    Ok(page) => ok(page),
                    Err(failure) => failure,
                }
            }
            InspectQuery::Document => {
                if has_non_index_args {
                    return fail(
                        "invalid_argument",
                        "query 'document' accepts only doc_id, offset, and limit",
                    );
                }
                match self.core_document_page(
                    &args.doc_id,
                    args.offset.unwrap_or(0),
                    args.limit.unwrap_or(DEFAULT_CORE_DOCUMENT_LIMIT),
                ) {
                    Ok(page) => ok(page),
                    Err(failure) => failure,
                }
            }
            InspectQuery::Block => {
                if args.pattern.is_some()
                    || args.patterns.is_some()
                    || args.filter.is_some()
                    || args.from_block_id.is_some()
                    || args.to_block_id.is_some()
                    || args.format.is_some()
                    || args.offset.is_some()
                    || args.limit.is_some()
                {
                    return fail(
                        "invalid_argument",
                        "query 'block' accepts only doc_id, block_id, detail, cell_offset, and cell_limit",
                    );
                }
                let Some(block_id) = args.block_id else {
                    return fail(
                        "invalid_argument",
                        "query 'block' requires a non-empty block_id",
                    );
                };
                if block_id.trim().is_empty() {
                    return fail(
                        "invalid_argument",
                        "query 'block' requires a non-empty block_id",
                    );
                }
                let detail = args.detail.unwrap_or_default();
                let cell_offset = args.cell_offset;
                let cell_limit = args.cell_limit;
                let handle = DocHandle(args.doc_id.clone());
                let target = block_id.clone();
                let result = self.runtime.with(&handle, move |snapshot| {
                    let view = build_document_view(snapshot);
                    if let Some(block) = view
                        .blocks
                        .iter()
                        .find(|block| block.id.to_string() == target)
                    {
                        return core_block_detail_json(block, detail, cell_offset, cell_limit)
                            .map(Some);
                    }
                    if cell_offset.is_some() || cell_limit.is_some() {
                        return Err(format!(
                            "cell_offset/cell_limit require a top-level table block; '{target}' is not one"
                        ));
                    }
                    Ok(cell_paragraph_detail_json(&view, &target, detail))
                });
                match result {
                    Ok(Ok(Some(payload))) => ok(payload),
                    Ok(Ok(None)) => fail("AnchorNotFound", format!("block '{block_id}' not found")),
                    Ok(Err(message)) => fail("invalid_argument", message),
                    Err(error) => fail(
                        &format!("{:?}", error.code),
                        format!("doc not open: {}", error.message),
                    ),
                }
            }
            InspectQuery::Revisions => {
                if args.block_id.is_some()
                    || args.detail.is_some()
                    || args.pattern.is_some()
                    || args.patterns.is_some()
                    || args.from_block_id.is_some()
                    || args.to_block_id.is_some()
                    || args.format.is_some()
                    || args.offset.is_some()
                    || args.limit.is_some()
                    || args.cell_offset.is_some()
                    || args.cell_limit.is_some()
                {
                    return fail(
                        "invalid_argument",
                        "query 'revisions' accepts only doc_id and optional filter",
                    );
                }
                self.list_revisions(Parameters(ListRevisionsArgs {
                    doc_id: args.doc_id,
                    filter: args.filter,
                }))
                .await
            }
            InspectQuery::RevisionsSummary => {
                if args.block_id.is_some()
                    || args.detail.is_some()
                    || args.pattern.is_some()
                    || args.patterns.is_some()
                    || args.from_block_id.is_some()
                    || args.to_block_id.is_some()
                    || args.format.is_some()
                    || args.offset.is_some()
                    || args.limit.is_some()
                    || args.cell_offset.is_some()
                    || args.cell_limit.is_some()
                {
                    return fail(
                        "invalid_argument",
                        "query 'revisions_summary' accepts only doc_id and optional filter",
                    );
                }
                let rows = match self.filtered_revision_rows(&args.doc_id, args.filter.as_ref()) {
                    Ok(rows) => rows,
                    Err(result) => return result,
                };
                // Exact counts by author × kind. An anonymized revision is
                // grouped under `author: null` — reported, never invented.
                let mut by_author: Vec<(
                    Option<String>,
                    std::collections::BTreeMap<String, usize>,
                )> = Vec::new();
                for row in &rows {
                    let entry = match by_author.iter_mut().find(|(a, _)| *a == row.author) {
                        Some((_, kinds)) => kinds,
                        None => {
                            by_author.push((row.author.clone(), Default::default()));
                            &mut by_author.last_mut().expect("just pushed").1
                        }
                    };
                    *entry.entry(row.kind.as_str().to_string()).or_default() += 1;
                }
                let authors: Vec<Value> = by_author
                    .into_iter()
                    .map(|(author, kinds)| {
                        let author_total: usize = kinds.values().sum();
                        json!({
                            "author": author,
                            "kinds": kinds,
                            "total": author_total,
                        })
                    })
                    .collect();
                ok(json!({
                    "doc_id": args.doc_id,
                    "total": rows.len(),
                    "by_author": authors,
                    "server_version": SERVER_VERSION,
                }))
            }
            InspectQuery::Styles => {
                if has_query_args {
                    return fail("invalid_argument", "query 'styles' accepts only doc_id");
                }
                self.read_styles(Parameters(ReadArgs {
                    doc_id: args.doc_id,
                }))
                .await
            }
            InspectQuery::Find => {
                if args.block_id.is_some()
                    || args.detail.is_some()
                    || args.filter.is_some()
                    || args.from_block_id.is_some()
                    || args.to_block_id.is_some()
                    || args.format.is_some()
                {
                    return fail(
                        "invalid_argument",
                        "query 'find' accepts only doc_id, exactly one of pattern or patterns, offset, limit, cell_offset, and cell_limit",
                    );
                }
                match (args.pattern, args.patterns) {
                    (Some(_), Some(_)) => fail(
                        "invalid_argument",
                        "query 'find' requires exactly one of pattern or patterns, not both",
                    ),
                    (None, None) => fail(
                        "invalid_argument",
                        "query 'find' requires exactly one of pattern or patterns",
                    ),
                    (Some(pattern), None) => {
                        if pattern.trim().is_empty() {
                            return fail(
                                "invalid_argument",
                                "query 'find' requires non-empty pattern",
                            );
                        }
                        self.find(Parameters(FindArgs {
                            doc_id: args.doc_id,
                            pattern,
                            offset: args.offset,
                            limit: args.limit,
                            cell_offset: args.cell_offset,
                            cell_limit: args.cell_limit,
                        }))
                        .await
                    }
                    (None, Some(patterns)) => {
                        if patterns.is_empty() || patterns.len() > MAX_BATCH_FIND_PATTERNS {
                            return fail(
                                "invalid_argument",
                                format!(
                                    "query 'find' patterns must contain 1 to {MAX_BATCH_FIND_PATTERNS} entries, got {}",
                                    patterns.len()
                                ),
                            );
                        }
                        if let Some((index, _)) = patterns
                            .iter()
                            .enumerate()
                            .find(|(_, pattern)| pattern.trim().is_empty())
                        {
                            return fail(
                                "invalid_argument",
                                format!(
                                    "query 'find' patterns[{index}] must be a non-empty string"
                                ),
                            );
                        }
                        let offset = args.offset.unwrap_or(0);
                        let limit = args.limit.unwrap_or(DEFAULT_FIND_LIMIT);
                        let cell_offset = args.cell_offset.unwrap_or(0);
                        let cell_limit = args.cell_limit.unwrap_or(DEFAULT_FIND_CELL_LIMIT);
                        if limit == 0 || limit > MAX_BATCH_FIND_LIMIT {
                            return fail(
                                "invalid_argument",
                                format!(
                                    "batch find limit must be between 1 and {MAX_BATCH_FIND_LIMIT}, got {limit}; use continuation or singular pattern for larger pages"
                                ),
                            );
                        }
                        if cell_limit == 0 || cell_limit > MAX_BATCH_FIND_CELL_LIMIT {
                            return fail(
                                "invalid_argument",
                                format!(
                                    "batch find cell_limit must be between 1 and {MAX_BATCH_FIND_CELL_LIMIT}, got {cell_limit}; use continuation or singular pattern for larger pages"
                                ),
                            );
                        }
                        let handle = DocHandle(args.doc_id);
                        let result = self.runtime.with(&handle, move |snapshot| {
                            let view = build_document_view(snapshot);
                            patterns
                                .iter()
                                .enumerate()
                                .map(|(pattern_index, pattern)| {
                                    find_page(
                                        &view,
                                        pattern,
                                        offset,
                                        limit,
                                        cell_offset,
                                        cell_limit,
                                    )
                                    .map(|result| {
                                        json!({
                                            "pattern_index": pattern_index,
                                            "result": result,
                                        })
                                    })
                                })
                                .collect::<Result<Vec<_>, _>>()
                        });
                        let outcomes = match result {
                            Ok(Ok(outcomes)) => outcomes,
                            Ok(Err(message)) => return fail("invalid_argument", message),
                            Err(error) => {
                                return fail(
                                    &format!("{:?}", error.code),
                                    format!("doc not open: {}", error.message),
                                );
                            }
                        };
                        let payload = json!({
                            "pattern_count": outcomes.len(),
                            "outcomes": outcomes,
                            "limits": {
                                "max_patterns": MAX_BATCH_FIND_PATTERNS,
                                "max_matches_per_pattern": MAX_BATCH_FIND_LIMIT,
                                "max_matching_cells_per_table": MAX_BATCH_FIND_CELL_LIMIT,
                                "max_response_bytes": MAX_BATCH_FIND_RESPONSE_BYTES,
                            },
                            "server_version": SERVER_VERSION,
                        });
                        if let Err(encoded_bytes) = batch_find_response_bytes(&payload) {
                            return fail_json(json!({
                                "code": "response_too_large",
                                "error": format!(
                                    "batch find response is {encoded_bytes} bytes, exceeding the {MAX_BATCH_FIND_RESPONSE_BYTES}-byte cap; lower limit, request fewer patterns, or use singular find"
                                ),
                                "actual_bytes": encoded_bytes,
                                "max_bytes": MAX_BATCH_FIND_RESPONSE_BYTES,
                                "remediation": {
                                    "kind": "narrow_batch_find",
                                    "max_patterns": MAX_BATCH_FIND_PATTERNS,
                                    "max_limit": MAX_BATCH_FIND_LIMIT,
                                    "max_cell_limit": MAX_BATCH_FIND_CELL_LIMIT,
                                },
                            }));
                        }
                        ok(payload)
                    }
                }
            }
            InspectQuery::Window => {
                if args.block_id.is_some()
                    || args.detail.is_some()
                    || args.pattern.is_some()
                    || args.patterns.is_some()
                    || args.filter.is_some()
                    || args.offset.is_some()
                    || args.limit.is_some()
                    || args.cell_offset.is_some()
                    || args.cell_limit.is_some()
                {
                    return fail(
                        "invalid_argument",
                        "query 'window' accepts only doc_id, from_block_id, to_block_id, and format",
                    );
                }
                let (Some(from_block_id), Some(to_block_id), Some(format)) =
                    (args.from_block_id, args.to_block_id, args.format)
                else {
                    return fail(
                        "invalid_argument",
                        "query 'window' requires from_block_id, to_block_id, and format",
                    );
                };
                self.read_window(Parameters(WindowArgs {
                    doc_id: args.doc_id,
                    from_block_id,
                    to_block_id,
                    format,
                }))
                .await
            }
            InspectQuery::Section => {
                if args.detail.is_some()
                    || args.pattern.is_some()
                    || args.patterns.is_some()
                    || args.filter.is_some()
                    || args.from_block_id.is_some()
                    || args.to_block_id.is_some()
                    || args.format.is_some()
                    || args.offset.is_some()
                    || args.limit.is_some()
                    || args.cell_offset.is_some()
                    || args.cell_limit.is_some()
                {
                    return fail(
                        "invalid_argument",
                        "query 'section' accepts only doc_id and block_id",
                    );
                }
                let Some(heading_id) = args.block_id.filter(|id| !id.trim().is_empty()) else {
                    return fail(
                        "invalid_argument",
                        "query 'section' requires a non-empty heading block_id",
                    );
                };
                self.get_section(Parameters(SectionArgs {
                    doc_id: args.doc_id,
                    heading_id,
                }))
                .await
            }
            InspectQuery::Text => {
                if has_query_args {
                    return fail("invalid_argument", "query 'text' accepts only doc_id");
                }
                self.read_text(Parameters(ReadArgs {
                    doc_id: args.doc_id,
                }))
                .await
            }
            InspectQuery::Html => {
                if has_query_args {
                    return fail("invalid_argument", "query 'html' accepts only doc_id");
                }
                self.read_html(Parameters(ReadArgs {
                    doc_id: args.doc_id,
                }))
                .await
            }
            InspectQuery::Redline => {
                if has_query_args {
                    return fail("invalid_argument", "query 'redline' accepts only doc_id");
                }
                self.read_redline(Parameters(ReadArgs {
                    doc_id: args.doc_id,
                }))
                .await
            }
            InspectQuery::Accepted => {
                if has_query_args {
                    return fail("invalid_argument", "query 'accepted' accepts only doc_id");
                }
                self.read_accepted(Parameters(ReadArgs {
                    doc_id: args.doc_id,
                }))
                .await
            }
            InspectQuery::Rejected => {
                if has_query_args {
                    return fail("invalid_argument", "query 'rejected' accepts only doc_id");
                }
                self.read_rejected(Parameters(ReadArgs {
                    doc_id: args.doc_id,
                }))
                .await
            }
            InspectQuery::Notes => {
                if has_query_args {
                    return fail("invalid_argument", "query 'notes' accepts only doc_id");
                }
                let handle = DocHandle(args.doc_id.clone());
                match self
                    .runtime
                    .with(&handle, |snapshot| notes_json(snapshot.canonical.as_ref()))
                {
                    Ok(notes) => ok(json!({
                        "doc_id": args.doc_id,
                        "note_count": notes.len(),
                        "notes": notes,
                        "server_version": SERVER_VERSION,
                    })),
                    Err(error) => fail(
                        &format!("{:?}", error.code),
                        format!("doc not open: {}", error.message),
                    ),
                }
            }
            InspectQuery::Operations => {
                if args.block_id.is_some()
                    || args.detail.is_some()
                    || args.patterns.is_some()
                    || args.filter.is_some()
                    || args.from_block_id.is_some()
                    || args.to_block_id.is_some()
                    || args.format.is_some()
                    || args.offset.is_some()
                    || args.limit.is_some()
                    || args.cell_offset.is_some()
                    || args.cell_limit.is_some()
                {
                    return fail(
                        "invalid_argument",
                        "query 'operations' accepts only doc_id and optional pattern",
                    );
                }
                let operation = args.pattern.as_deref();
                if operation.is_some_and(|name| name.trim().is_empty()) {
                    return fail(
                        "invalid_argument",
                        "query 'operations' pattern must be a non-empty exact operation name",
                    );
                }
                let handle = DocHandle(args.doc_id.clone());
                if let Err(error) = self.runtime.with(&handle, |_| ()) {
                    return fail(
                        &format!("{:?}", error.code),
                        format!("doc not open: {}", error.message),
                    );
                }
                match operation_catalog(operation) {
                    Ok(catalog) => ok(catalog),
                    Err(message) => fail("invalid_argument", message),
                }
            }
        }
    }

    #[tool(
        description = "Preview or execute one explicit TransformPlan through Stemma's typed \
                       engine. Supply exactly one of transaction, resolution, \
                       replacement_worklist, or comparison. A transaction is \
                       the existing atomic v4 contract: {ops:[...], revision:{author}, summary?}; \
                       preview=true validates it on a throwaway snapshot and preview=false applies \
                       all ops or none; both return touched-block-only receipts. A resolution is \
                       {action:'accept'|'reject', selector:{by:...}} \
                       and uses revision ids/authors obtained from inspect_docx; preview reports the \
                       exact ids without resolving them. Before an unfamiliar structural plan, \
                       call inspect_docx query='operations' (optionally pattern='<op>') for the \
                       complete parser-derived operation vocabulary, parser fields, MCP-edge \
                       fields, exact \
                       examples, and cues. High-value transaction ops include \
                       replace/insert/delete/move, \
                       comment_create/comment_reply/comment_resolve, table_op and table/cell/row \
                       formatting, run/paragraph formatting, notes, images, styles, numbering, \
                       bookmarks, content controls, equations, cross-references, and headers or \
                       footers. Common exact op shapes: paragraph edit \
                       {op:'replace',target:'p_3',expect:'old text',content:{type:'paragraph', \
                       content:[{type:'text',text:'new text'}]}}; table cell edit \
                       {op:'table_op',target:'tbl_1',table_op:{kind:'set_cell_text',row_index:0, \
                       col_index:1,text:'new text'}}; anchored comment \
                       {op:'comment_create',target:'p_3',expect:'anchor text',body:'comment', \
                       author:'Reviewer'}; reply to that comment with \
                       {op:'comment_reply',parent_comment_id:'1',body:'reply',author:'Reviewer'}. \
                       Notes are first-class: inspect_docx query='notes' returns note_id/kind/body; \
                       then use insert_note/edit_note/delete_note. parent_comment_id is the string comment id returned by inspection, never \
                       a block id or opaque-anchor id. When replacing a paragraph that contains an opaque \
                       anchor, preserve it in content.content as \
                       {type:'opaque_ref',attrs:{id:'<opaque id from inspect block anchors>'}}. \
                       For bulk literal changes use replacement_worklist:{author,replacements:[ \
                       {effect_id?,old,new,expected_matches?:2,replace_all?:false,scope?,match_mode?, \
                       on_barrier_match?}]}; use replace_all:true instead of expected_matches \
                       only when every occurrence is intended. Omitted scope searches top-level \
                       and table-cell paragraphs; use scope only to restrict or disambiguate. It \
                       supports an exact throwaway preview or apply, reports every item independently, and avoids \
                       one search/read/edit round trip per phrase. A task-bound session requires \
                       every item to name and exactly match one declared effect_id; other plan \
                       shapes are refused there. It is explicitly non-atomic: \
                       failed items are returned alongside applied items for exact re-issue. \
                       Resolution is NOT a finalize step: for ordinary fill/edit tasks leave both \
                       existing and newly authored revisions pending; accept/reject only when the \
                       user explicitly asks to resolve or clean revisions. \
                       Obtain targets, cell coordinates, spans, and guards from \
                       inspect_docx; unknown or stale plan state fails loudly. comparison is the \
                       producer path {base_path,target_path,out_path,author?}; omit doc_id and use \
                       preview=false to generate a tracked redline from two files."
    )]
    async fn execute_plan(&self, Parameters(args): Parameters<ExecutePlanArgs>) -> CallToolResult {
        let ExecutePlanArgs {
            doc_id,
            transaction,
            resolution,
            replacement_worklist,
            comparison,
            preview,
            mode,
            allow_existing_author,
        } = args;
        let plan_count = usize::from(transaction.is_some())
            + usize::from(resolution.is_some())
            + usize::from(replacement_worklist.is_some())
            + usize::from(comparison.is_some());
        if plan_count != 1 {
            return fail(
                "invalid_argument",
                "execute_plan requires exactly one of transaction, resolution, replacement_worklist, or comparison",
            );
        }
        if let Some(comparison) = comparison {
            if doc_id.is_some() || mode.is_some() || allow_existing_author || preview {
                return fail(
                    "invalid_argument",
                    "comparison requires doc_id omitted, preview=false, mode omitted, and allow_existing_author=false",
                );
            }
            return self
                .compare_docx(Parameters(CompareArgs {
                    base_path: comparison.base_path,
                    target_path: comparison.target_path,
                    out_path: comparison.out_path,
                    author: comparison.author,
                }))
                .await;
        }
        let Some(doc_id) = doc_id.filter(|value| !value.trim().is_empty()) else {
            return fail(
                "invalid_argument",
                "transaction, resolution, and replacement_worklist require a non-empty doc_id",
            );
        };
        if let Some(binding) = self.task_doc_binding(&doc_id)
            && replacement_worklist.is_none()
        {
            let shape = if transaction.is_some() {
                "transaction"
            } else {
                "resolution"
            };
            return fail_json(json!({
                "code": "task_plan_shape_undeclarable",
                "error": format!(
                    "{shape} cannot execute in task {:?}: task manifest v1 admits only declaration-matched replacement_worklist items",
                    binding.task_id
                ),
                "task_id": binding.task_id,
                "doc_id": doc_id,
                "shape": shape,
            }));
        }
        match (transaction, resolution, replacement_worklist) {
            (Some(transaction), None, None) => {
                let mut result = self
                    .apply_batch(Parameters(BatchArgs {
                        doc_id,
                        transaction,
                        preview,
                        mode,
                        allow_existing_author,
                    }))
                    .await;
                if preview {
                    attach_preview_apply_cue(&mut result);
                }
                result
            }
            (None, Some(resolution), None) => {
                if mode.is_some() || allow_existing_author {
                    return fail(
                        "invalid_argument",
                        "mode and allow_existing_author are valid only for transaction plans",
                    );
                }
                if preview {
                    let selector = change_selector_json(&resolution.selector);
                    let ids = match self.resolve_revision_ids(&doc_id, resolution.selector) {
                        Ok(ids) => ids,
                        Err(result) => return result,
                    };
                    let mut revision_ids: Vec<u32> = ids.into_iter().collect();
                    revision_ids.sort_unstable();
                    let action = match resolution.action {
                        ResolutionActionArg::Accept => "accept",
                        ResolutionActionArg::Reject => "reject",
                    };
                    let evidence = CappedEvidenceSet::new(
                        revision_ids.into_iter().map(Value::from).collect(),
                        Self::RECEIPT_ID_CAP,
                    );
                    let selected = evidence.total;
                    let evidence_metadata = evidence.metadata();
                    let revision_ids = evidence.rows;
                    let mut result = ok(json!({
                        "doc_id": doc_id,
                        "applied": false,
                        "would_apply": true,
                        "resolution": {
                            "action": action,
                            "selector": selector,
                            "selected": selected,
                            "revision_ids": revision_ids,
                            "revision_ids_evidence": evidence_metadata,
                            "deliverable": true,
                        },
                    }));
                    attach_preview_apply_cue(&mut result);
                    return result;
                }
                match resolution.action {
                    ResolutionActionArg::Accept => {
                        self.accept_changes(Parameters(AcceptArgs {
                            doc_id,
                            selector: resolution.selector,
                        }))
                        .await
                    }
                    ResolutionActionArg::Reject => {
                        self.reject_changes(Parameters(RejectArgs {
                            doc_id,
                            selector: resolution.selector,
                        }))
                        .await
                    }
                }
            }
            (None, None, Some(worklist)) => {
                if mode.is_some() {
                    return fail(
                        "invalid_argument",
                        "mode is valid only for transaction plans; replacement_worklist always authors tracked changes",
                    );
                }
                let _session_guard = self
                    .artifact_session_gate
                    .lock()
                    .expect("artifact_session_gate mutex poisoned");
                let task_binding =
                    match self.validate_task_worklist(&doc_id, &worklist.replacements) {
                        Ok(binding) => binding,
                        Err(failure) => return failure,
                    };
                let mut replacements = Vec::with_capacity(worklist.replacements.len());
                for (index, item) in worklist.replacements.into_iter().enumerate() {
                    if item.replace_all && item.expected_matches.is_some() {
                        return fail(
                            "invalid_argument",
                            format!(
                                "replacement_worklist item {index} must use exactly one of expected_matches or replace_all=true"
                            ),
                        );
                    }
                    replacements.push(ReplaceItem {
                        old: item.old,
                        new: item.new,
                        scope: item.scope,
                        expected_matches: if item.replace_all {
                            Some(ExpectedMatchesArg::Keyword("all".to_string()))
                        } else {
                            item.expected_matches.map(ExpectedMatchesArg::Count)
                        },
                        match_mode: item.match_mode.as_str().to_string(),
                        on_barrier_match: item.on_barrier_match.as_str().to_string(),
                    });
                }
                let mut result = self.replace_text_batch_impl(ReplaceTextBatchArgs {
                    doc_id,
                    author: worklist.author,
                    replacements,
                    preview,
                    allow_existing_author,
                });
                if !preview
                    && let Some(binding) = &task_binding
                    && let Err(failure) = self.record_task_worklist_outcomes(binding, &mut result)
                {
                    return failure;
                }
                if preview {
                    attach_preview_apply_cue(&mut result);
                }
                result
            }
            _ => unreachable!("plan_count and comparison branch enforce one doc-backed plan"),
        }
    }

    #[tool(
        description = "Verify intended versus actual DOCX changes using the engine-derived audit. \
                       For an open session pass only doc_id; this verifies changes since open_docx. \
                       For producer-neutral verification pass before_path and after_path with no \
                       doc_id. Exactly one mode is required. Do not call both modes for one edit: \
                       doc_id is the authoritative session audit before save, while a saved-path \
                       pair redundantly recomputes the same engine evidence. \
                       Returns new tracked changes, direct \
                       untracked delta, dispositions of pre-existing revisions, untouched-scope \
                       proof, validation, and exact input identities. Session mode partitions \
                       changed prior revisions into expected (selected by a successful typed \
                       accept/reject command) and unexpected; producer-neutral mode has no such \
                       command evidence and remains conservative. A direct_delta row is \
                       unexplained unless it is an allowed comment/property effect or carries \
                       session_resolution_evidenced=true after exact ordered-transition \
                       reconciliation. Every list is explicitly \
                       paged (16 rows by default, 64 maximum); use detail with offset/limit to \
                       retrieve every finding without flooding conversation history. Optional render.path commits \
                       a create-new audit redline. baseline_validator describes the input before \
                       editing; validator.new_issue_count reports regressions introduced by the \
                       current document. Findings unchanged from baseline must be disclosed but \
                       do not block an otherwise valid output. Any unexpectedly changed/resolved \
                       prior revision, untouched violation, NEW validator issue, or unexplained \
                       direct_delta row is incomplete."
    )]
    async fn verify_docx(&self, Parameters(args): Parameters<VerifyDocxArgs>) -> CallToolResult {
        match (args.doc_id, args.before_path, args.after_path) {
            (Some(doc_id), None, None) if !doc_id.trim().is_empty() => {
                self.review_session(Parameters(ReviewSessionArgs {
                    doc_id,
                    render: args.render,
                    detail: args.detail,
                    offset: args.offset,
                    limit: args.limit,
                }))
                .await
            }
            (None, Some(before_path), Some(after_path))
                if !before_path.trim().is_empty() && !after_path.trim().is_empty() =>
            {
                self.audit_docx(Parameters(AuditDocxArgs {
                    before_path,
                    after_path,
                    render: args.render,
                    detail: args.detail,
                    offset: args.offset,
                    limit: args.limit,
                }))
                .await
            }
            _ => fail(
                "invalid_argument",
                "verify_docx requires exactly one mode: non-empty doc_id, or non-empty \
                 before_path plus after_path",
            ),
        }
    }

    /// Caps for resolution receipts. A bulk resolution can touch hundreds of
    /// revisions and blocks; a receipt that inlines every id and block row
    /// stops being a receipt and becomes a projection — it outgrows the
    /// client's context and forces file-offload post-processing. Counts are
    /// always exact; lists are bounded with an explicit `truncated` report —
    /// the cap is never silent (same contract as the revision-inventory row
    /// cap).
    const RECEIPT_ID_CAP: usize = 64;
    const RECEIPT_BLOCK_ROW_CAP: usize = 16;

    /// Assemble the accept/reject receipt: exact counts always, bounded
    /// evidence with immutable complete-set commitments, and an explicit
    /// `truncated` report when any list was capped. ONE builder for both
    /// actions so the receipt shapes cannot drift.
    fn resolution_receipt(
        doc_id: &str,
        action: &str, // "accepted" | "rejected" — also keys "<action>_revision_ids"
        resolved_ids: &[u32],
        cascaded_ids: &[u32],
        changed_block_ids: &[String],
        changed_blocks: Vec<Value>,
        block_count: usize,
    ) -> Value {
        let action_count_key = action;
        let action_ids_key = format!("{action}_revision_ids");
        let action_ids_omitted_key = format!("{action_ids_key}_omitted");
        let action_ids = CappedEvidenceSet::new(
            resolved_ids.iter().copied().map(Value::from).collect(),
            Self::RECEIPT_ID_CAP,
        );
        let cascaded = CappedEvidenceSet::new(
            cascaded_ids.iter().copied().map(Value::from).collect(),
            Self::RECEIPT_ID_CAP,
        );
        let block_ids = CappedEvidenceSet::new(
            changed_block_ids.iter().cloned().map(Value::from).collect(),
            Self::RECEIPT_ID_CAP,
        );
        let block_rows = CappedEvidenceSet::new(changed_blocks, Self::RECEIPT_BLOCK_ROW_CAP);
        let ids_omitted = action_ids.omitted();
        let cascaded_omitted = cascaded.omitted();
        let block_ids_omitted = block_ids.omitted();
        let rows_omitted = block_rows.omitted();
        let mut receipt = serde_json::Map::new();
        receipt.insert("doc_id".into(), json!(doc_id));
        receipt.insert(action_count_key.into(), json!(resolved_ids.len()));
        receipt.insert(
            action_ids_key.clone(),
            Value::Array(action_ids.rows.clone()),
        );
        receipt.insert(format!("{action_ids_key}_evidence"), action_ids.metadata());
        receipt.insert("cascaded".into(), json!(cascaded_ids.len()));
        receipt.insert(
            "cascaded_revision_ids".into(),
            Value::Array(cascaded.rows.clone()),
        );
        receipt.insert("cascaded_revision_ids_evidence".into(), cascaded.metadata());
        receipt.insert("changed_block_count".into(), json!(changed_block_ids.len()));
        receipt.insert(
            "changed_block_ids".into(),
            Value::Array(block_ids.rows.clone()),
        );
        receipt.insert("changed_block_ids_evidence".into(), block_ids.metadata());
        receipt.insert(
            "changed_blocks".into(),
            Value::Array(block_rows.rows.clone()),
        );
        receipt.insert("changed_blocks_evidence".into(), block_rows.metadata());
        receipt.insert("block_count".into(), json!(block_count));
        receipt.insert("server_version".into(), json!(SERVER_VERSION));
        if ids_omitted + cascaded_omitted + block_ids_omitted + rows_omitted > 0 {
            let mut truncated = serde_json::Map::new();
            truncated.insert(action_ids_omitted_key, json!(ids_omitted));
            truncated.insert(
                "cascaded_revision_ids_omitted".into(),
                json!(cascaded_omitted),
            );
            truncated.insert("changed_block_ids_omitted".into(), json!(block_ids_omitted));
            truncated.insert("changed_blocks_rows_omitted".into(), json!(rows_omitted));
            truncated.insert(
                "advice".into(),
                json!(
                    "counts are exact; the omitted entries are not lost — inspect_docx \
                     {query: \"revisions\"} lists what remains pending, and verify_docx \
                     checks the saved result independently of this receipt"
                ),
            );
            receipt.insert("truncated".into(), Value::Object(truncated));
        }
        Value::Object(receipt)
    }

    #[tool(
        description = "Accept tracked changes selected by id, author, block range, an \
                       AND-combined filter (by_filter: author ∧ kind ∧ block range — the \
                       one-call form of \"author X's changes in section Y\"), or all. \
                       The selector is lowered to a concrete revision-id set against the \
                       current read view; an empty/unmatched selection fails loudly \
                       (InvalidRange) rather than silently doing nothing. Accepting only \
                       some changes leaves the rest tracked. Returns a lean receipt: \
                       exact counts (accepted, cascaded, changed_block_count) plus \
                       BOUNDED lists (accepted_revision_ids, cascaded_revision_ids, \
                       changed_block_ids, changed_blocks rows) — a bulk resolution \
                       reports an explicit `truncated` breakdown instead of an \
                       unbounded id dump; counts are always exact and every capped \
                       list carries omitted + set_sha256 metadata for its complete set. \
                       Policy: by default, LAYER your tracked changes beside other authors' \
                       pending changes; only resolve (accept/reject) another author's pending \
                       change when the user's instruction calls for it (a cleanup/tighten-class \
                       task), and report what you resolved distinctly in your final summary."
    )]
    async fn accept_changes(&self, Parameters(args): Parameters<AcceptArgs>) -> CallToolResult {
        if let Some(failure) = self.refuse_direct_task_mutation(&args.doc_id, "accept_changes") {
            return failure;
        }
        let _session_guard = self
            .artifact_session_gate
            .lock()
            .expect("artifact_session_gate mutex poisoned");
        if let Err(failure) = self.session_resolution_evidence(&args.doc_id) {
            return failure;
        }
        let ids = match self.resolve_revision_ids(&args.doc_id, args.selector) {
            Ok(ids) => ids,
            Err(r) => return r,
        };
        let handle = DocHandle(args.doc_id.clone());
        let before = match self.runtime.with(&handle, Clone::clone) {
            Ok(snapshot) => snapshot,
            Err(e) => {
                return fail(
                    &format!("{:?}", e.code),
                    format!("doc not open: {}", e.message),
                );
            }
        };
        // Derive the evidence from the same pure projection the runtime will
        // commit, before mutating session state. If serialization or audit
        // cannot explain the projected transition, the command fails without
        // having partially committed a resolution.
        let projected = match before.project(Resolution::Selective {
            ids: ids.clone(),
            action: ResolveSelectionAction::Accept,
        }) {
            Ok(snapshot) => snapshot,
            Err(error) => return fail(&format!("{:?}", error.code), error.message),
        };
        let direct_transitions = match Self::resolution_direct_transitions(&before, &projected) {
            Ok(transitions) => transitions,
            Err(failure) => return failure,
        };
        match self
            .runtime
            .resolve_tracked_revisions(&handle, &ids, ResolveSelectionAction::Accept)
        {
            Ok(result) => {
                assert_eq!(
                    result.canonical, projected.canonical,
                    "runtime selective-accept result diverged from its preflight projection"
                );
                let mut accepted: Vec<u32> = ids.iter().copied().collect();
                accepted.sort_unstable();
                let changed = changed_block_ids(&before.canonical, &result.canonical);
                let evidence_ids: HashSet<u32> = accepted
                    .iter()
                    .copied()
                    .chain(result.cascaded_revision_ids.iter().copied())
                    .collect();
                self.record_resolution_evidence(&args.doc_id, evidence_ids, direct_transitions);
                let (changed_blocks, block_count) =
                    match self.changed_block_rows(&args.doc_id, &changed) {
                        Ok(v) => v,
                        Err(r) => return r,
                    };
                // Cascaded revisions (e.g. accepting a deletion stacked over
                // an insertion settles the insertion's claim on that range)
                // are never silent — counted exactly, listed bounded.
                ok(Self::resolution_receipt(
                    &args.doc_id,
                    "accepted",
                    &accepted,
                    &result.cascaded_revision_ids,
                    &changed,
                    changed_blocks,
                    block_count,
                ))
            }
            Err(e) => fail_json(json!({
                "code": format!("{:?}", e.code),
                "error": e.message,
            })),
        }
    }

    #[tool(
        description = "Reject tracked changes selected by id, author, block range, an \
                       AND-combined filter (by_filter: author ∧ kind ∧ block range), or all. \
                       Same selector lowering and fail-loud contract as accept_changes; \
                       rejecting only some changes leaves the rest tracked. Returns a lean \
                       receipt: exact counts (rejected, cascaded, changed_block_count) plus \
                       BOUNDED lists with an explicit `truncated` breakdown and complete-set \
                       commitment metadata on bulk resolutions — counts are always exact, \
                       never an unbounded id dump. \
                       Policy: by default, LAYER your tracked changes beside other authors' \
                       pending changes; only resolve (accept/reject) another author's pending \
                       change when the user's instruction calls for it (a cleanup/tighten-class \
                       task), and report what you resolved distinctly in your final summary."
    )]
    async fn reject_changes(&self, Parameters(args): Parameters<RejectArgs>) -> CallToolResult {
        if let Some(failure) = self.refuse_direct_task_mutation(&args.doc_id, "reject_changes") {
            return failure;
        }
        let _session_guard = self
            .artifact_session_gate
            .lock()
            .expect("artifact_session_gate mutex poisoned");
        if let Err(failure) = self.session_resolution_evidence(&args.doc_id) {
            return failure;
        }
        let ids = match self.resolve_revision_ids(&args.doc_id, args.selector) {
            Ok(ids) => ids,
            Err(r) => return r,
        };
        let handle = DocHandle(args.doc_id.clone());
        let before = match self.runtime.with(&handle, Clone::clone) {
            Ok(snapshot) => snapshot,
            Err(e) => {
                return fail(
                    &format!("{:?}", e.code),
                    format!("doc not open: {}", e.message),
                );
            }
        };
        let projected = match before.project(Resolution::Selective {
            ids: ids.clone(),
            action: ResolveSelectionAction::Reject,
        }) {
            Ok(snapshot) => snapshot,
            Err(error) => return fail(&format!("{:?}", error.code), error.message),
        };
        let direct_transitions = match Self::resolution_direct_transitions(&before, &projected) {
            Ok(transitions) => transitions,
            Err(failure) => return failure,
        };
        match self
            .runtime
            .resolve_tracked_revisions(&handle, &ids, ResolveSelectionAction::Reject)
        {
            Ok(result) => {
                assert_eq!(
                    result.canonical, projected.canonical,
                    "runtime selective-reject result diverged from its preflight projection"
                );
                let mut rejected: Vec<u32> = ids.iter().copied().collect();
                rejected.sort_unstable();
                let changed = changed_block_ids(&before.canonical, &result.canonical);
                let evidence_ids: HashSet<u32> = rejected
                    .iter()
                    .copied()
                    .chain(result.cascaded_revision_ids.iter().copied())
                    .collect();
                self.record_resolution_evidence(&args.doc_id, evidence_ids, direct_transitions);
                let (changed_blocks, block_count) =
                    match self.changed_block_rows(&args.doc_id, &changed) {
                        Ok(v) => v,
                        Err(r) => return r,
                    };
                ok(Self::resolution_receipt(
                    &args.doc_id,
                    "rejected",
                    &rejected,
                    &result.cascaded_revision_ids,
                    &changed,
                    changed_blocks,
                    block_count,
                ))
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
        let submitted_operations = args.transaction.operation_count();
        let txn_json = args.transaction.to_json_string();
        let (txn_json, _image_sources) = match resolve_image_paths(
            &self.artifacts,
            &txn_json,
            self.max_image_bytes(),
            self.max_image_total_bytes(),
        ) {
            Ok(resolved) => resolved,
            Err(f) => return attach_known_transaction_outcomes(f, submitted_operations, true),
        };
        let v4 = match parse_transaction(&txn_json) {
            Ok(v) => v,
            Err(e) => {
                return attach_known_transaction_outcomes(
                    fail(
                        "schema_error",
                        augment_schema_error(&txn_json, &e.to_string()),
                    ),
                    submitted_operations,
                    true,
                );
            }
        };
        let operation_count = v4.ops.len();
        let txn = match v4.into_edit_transaction() {
            Ok(t) => t,
            Err(e) => {
                return attach_transaction_outcomes(
                    fail("adapter_error", e.to_string()),
                    operation_count,
                    true,
                );
            }
        };

        let handle = DocHandle(args.doc_id.clone());
        // Run the same package-aware, author-protected apply used by commit and
        // discard the derived snapshot. Pure apply_transaction cannot check
        // package-level style/media constraints or origin-author impersonation.
        let outcome = self
            .runtime
            .with(&handle, |snap| snap.apply_authored(&txn, false).map(|_| ()));
        let result = match outcome {
            Ok(Ok(())) => ok(json!({ "doc_id": args.doc_id, "would_apply": true })),
            Ok(Err(error)) => fail_json(json!({
                "code": format!("{:?}", error.code),
                "error": error.message,
                "details": format!("{:?}", error.details),
                "would_apply": false,
            })),
            Err(e) => fail(
                &format!("{:?}", e.code),
                format!("doc not open: {}", e.message),
            ),
        };
        attach_transaction_outcomes(result, operation_count, true)
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
                       counts that partition changed prior revisions into expected and unexpected \
                       using successful typed resolution evidence, \
                       untouched ({verified_blocks, parts, violations} — every block outside \
                       the reported changes verified structurally identical to the baseline), \
                       and validator (the package verdict on the would-be save bytes). The \
                       response separates baseline_validator findings already present at open \
                       from validator.new_issue_count regressions introduced since open. Run it \
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
        let _session_guard = self
            .artifact_session_gate
            .lock()
            .expect("artifact_session_gate mutex poisoned");
        let report = match self.runtime.review_session(&handle) {
            Ok(r) => r,
            Err(e) => return fail(&format!("{:?}", e.code), e.message),
        };
        let resolution_evidence = match self.session_resolution_evidence(&args.doc_id) {
            Ok(evidence) => evidence,
            Err(failure) => return failure,
        };
        let mut payload = match audit_report_json(
            &report,
            Some(&resolution_evidence),
            args.detail,
            args.offset,
            args.limit,
        ) {
            Ok(payload) => payload,
            Err(message) => return fail("invalid_argument", message),
        };
        let source = match self.runtime.session_source_bytes(&handle) {
            Ok(bytes) => bytes,
            Err(error) => return fail(&format!("{:?}", error.code), error.message),
        };
        let baseline_validation = stemma::api::validate(&source);
        attach_baseline_validation(&mut payload, &baseline_validation, &report.validator);
        payload["doc_id"] = json!(args.doc_id);
        payload["server_version"] = json!(SERVER_VERSION);
        let input_artifacts = match self.protected_sources(&args.doc_id) {
            Ok(artifacts) => artifacts,
            Err(failure) => return failure,
        };
        payload["input_artifacts"] = json!(input_artifacts);
        if let Some(render) = &args.render {
            // The current document as bytes, via the same gated export the
            // save path uses — an unsaveable document cannot render either,
            // and the failure names the gate finding.
            let current = match self.runtime.export_docx(&handle, ExportMode::Redline) {
                Ok(b) => b,
                Err(e) => return fail(&format!("{:?}", e.code), e.message),
            };
            match self.render_redline_between(
                &source,
                &current,
                &render.path,
                "session_redline",
                &input_artifacts,
            ) {
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
                       after's bytes), and baseline_validator (the before file's verdict); \
                       validator.new_issue_count isolates regressions. compare_docx answers \
                       'produce a redline'; audit_docx \
                       answers 'certify what happened' — optional render.path also writes \
                       the redline, subsuming compare_docx when set. Audit lists are explicitly \
                       paged: omit detail for the first 16 rows of every section, or select one \
                       detail with offset/limit (maximum 64) to retrieve all findings."
    )]
    async fn audit_docx(&self, Parameters(args): Parameters<AuditDocxArgs>) -> CallToolResult {
        let before = match self.read_source(&args.before_path, "before_docx", self.max_doc_bytes())
        {
            Ok(source) => source,
            Err(failure) => return failure,
        };
        let after = match self.read_source(&args.after_path, "after_docx", self.max_doc_bytes()) {
            Ok(source) => source,
            Err(failure) => return failure,
        };
        let report = match stemma::audit(before.bytes(), after.bytes()) {
            Ok(r) => r,
            Err(e) => return fail(&format!("{:?}", e.code), e.message),
        };
        let input_artifacts = vec![before.identity().clone(), after.identity().clone()];
        let mut payload =
            match audit_report_json(&report, None, args.detail, args.offset, args.limit) {
                Ok(payload) => payload,
                Err(message) => return fail("invalid_argument", message),
            };
        let baseline_validation = stemma::api::validate(before.bytes());
        attach_baseline_validation(&mut payload, &baseline_validation, &report.validator);
        payload["before_path"] = json!(args.before_path);
        payload["after_path"] = json!(args.after_path);
        payload["server_version"] = json!(SERVER_VERSION);
        payload["input_artifacts"] = json!(input_artifacts);
        if let Some(render) = &args.render {
            match self.render_redline_between(
                before.bytes(),
                after.bytes(),
                &render.path,
                "audit_redline",
                &input_artifacts,
            ) {
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
                       returns the same lean, touched-block-only receipt as apply while \
                       discarding the derived snapshot (nothing persists); preview=false \
                       applies it as tracked changes. Every submitted operation has an uncapped \
                       inline outcome plus the atomicity result; potentially large revision and \
                       changed-block evidence is capped with exact counts, omission counts, and \
                       complete-set SHA-256 commitments."
    )]
    async fn apply_batch(&self, Parameters(args): Parameters<BatchArgs>) -> CallToolResult {
        let submitted_operations = args.transaction.operation_count();
        let txn_json = args.transaction.to_json_string();
        let (txn_json, image_sources) = match resolve_image_paths(
            &self.artifacts,
            &txn_json,
            self.max_image_bytes(),
            self.max_image_total_bytes(),
        ) {
            Ok(resolved) => resolved,
            Err(f) => {
                return attach_known_transaction_outcomes(f, submitted_operations, args.preview);
            }
        };
        let v4 = match parse_transaction(&txn_json) {
            Ok(v) => v,
            Err(e) => {
                return attach_known_transaction_outcomes(
                    fail(
                        "schema_error",
                        augment_schema_error(&txn_json, &e.to_string()),
                    ),
                    submitted_operations,
                    args.preview,
                );
            }
        };
        let operation_count = v4.ops.len();
        let mut txn = match v4.into_edit_transaction() {
            Ok(t) => t,
            Err(e) => {
                return attach_transaction_outcomes(
                    fail("adapter_error", e.to_string()),
                    operation_count,
                    args.preview,
                );
            }
        };
        match parse_materialization_mode(&args.mode) {
            Ok(Some(m)) => txn.materialization_mode = m,
            Ok(None) => {}
            Err(msg) => {
                return attach_transaction_outcomes(
                    fail("invalid_argument", msg),
                    operation_count,
                    args.preview,
                );
            }
        }
        let handle = DocHandle(args.doc_id.clone());

        if args.preview {
            // Dry-run the exact package-aware, authored path and build the same
            // touched-block-only receipt as commit from the discarded derived
            // snapshot. Returning the whole outline here used to dominate
            // agent history (100KB+ per preview on benchmark documents).
            let outcome = self.runtime.with(&handle, |snap| {
                let before = Arc::clone(&snap.canonical);
                let before_revision_ids: HashSet<u32> = revision_rows(&before)
                    .iter()
                    .map(|row| row.revision_id)
                    .collect();
                snap.apply_authored(&txn, args.allow_existing_author)
                    .map(|preview| {
                        let after = preview.canonical;
                        let changed = changed_block_ids(&before, &after);
                        let want: HashSet<&str> = changed.iter().map(String::as_str).collect();
                        let view = build_document_view_from_canon(&after);
                        let changed_blocks = view
                            .blocks
                            .iter()
                            .zip(after.blocks.iter())
                            .filter(|(block, _)| want.contains(block.id.to_string().as_str()))
                            .map(|(bv, tb)| receipt_block_row(bv, tb))
                            .collect::<Vec<_>>();
                        let mut revision_ids: Vec<u32> = revision_rows(&after)
                            .iter()
                            .map(|row| row.revision_id)
                            .filter(|id| !before_revision_ids.contains(id))
                            .collect();
                        revision_ids.sort_unstable();
                        revision_ids.dedup();
                        Self::bounded_transaction_receipt(json!({
                            "doc_id": args.doc_id,
                            "applied": false,
                            "would_apply": true,
                            "revision_ids": revision_ids,
                            "changed_block_ids": changed,
                            "changed_blocks": changed_blocks,
                            "block_count": after.blocks.len(),
                            "moves": move_receipts(&before, &after),
                            "table_receipts": table_receipts(&before, &after),
                            "server_version": SERVER_VERSION,
                        }))
                    })
            });
            let result = match outcome {
                Ok(Ok(receipt)) => ok(receipt),
                Ok(Err(error)) => fail_json(json!({
                    "code": format!("{:?}", error.code),
                    "error": error.message,
                    "details": format!("{:?}", error.details),
                    "would_apply": false,
                })),
                Err(e) => fail(
                    &format!("{:?}", e.code),
                    format!("doc not open: {}", e.message),
                ),
            };
            return attach_transaction_outcomes(result, operation_count, true);
        }

        let result =
            self.apply_edit_with_sources(&handle, &txn, args.allow_existing_author, image_sources);
        attach_transaction_outcomes(result, operation_count, false)
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

const DEFAULT_AUDIT_PAGE_ROWS: usize = 16;
const MAX_AUDIT_PAGE_ROWS: usize = 64;

#[derive(Debug, Clone, Copy)]
struct AuditPageRequest {
    detail: AuditDetail,
    offset: usize,
    limit: usize,
}

fn parse_audit_page_request(
    detail: Option<AuditDetail>,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<Option<AuditPageRequest>, String> {
    let Some(detail) = detail else {
        if offset.is_some() || limit.is_some() {
            return Err("offset and limit require detail".to_string());
        }
        return Ok(None);
    };
    let limit = limit.unwrap_or(DEFAULT_AUDIT_PAGE_ROWS);
    if !(1..=MAX_AUDIT_PAGE_ROWS).contains(&limit) {
        return Err(format!(
            "audit detail limit must be between 1 and {MAX_AUDIT_PAGE_ROWS}, got {limit}"
        ));
    }
    Ok(Some(AuditPageRequest {
        detail,
        offset: offset.unwrap_or(0),
        limit,
    }))
}

fn audit_rows_page(rows: &[Value], offset: usize, limit: usize) -> Result<Value, String> {
    let total = rows.len();
    if offset > total {
        return Err(format!(
            "audit detail offset {offset} exceeds section total {total}"
        ));
    }
    let end = offset.saturating_add(limit).min(total);
    let returned = end - offset;
    let has_more = end < total;
    let omitted = total - returned;
    let mut page = json!({
        "rows": &rows[offset..end],
        "total": total,
        "offset": offset,
        "returned": returned,
        "omitted": omitted,
        "set_sha256": canonical_set_sha256(rows),
        "has_more": has_more,
    });
    if has_more {
        page["next_offset"] = json!(end);
    }
    Ok(page)
}

fn audit_array_and_page(page: Value) -> (Value, Value) {
    let rows = page["rows"].clone();
    let mut metadata = page;
    metadata
        .as_object_mut()
        .expect("audit page is always an object")
        .remove("rows");
    (rows, metadata)
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

fn validation_issue_json(issue: &stemma::runtime::ValidationIssue) -> Value {
    json!({
        "code": validation_issue_code_str(&issue.code),
        "message": issue.message,
        "context": issue.context,
    })
}

fn audit_direct_change_explanation(c: &stemma::audit::DirectChange) -> Option<&'static str> {
    match &c.story {
        StoryScope::Comment { .. } => Some("comment_annotation"),
        _ if !c.coincides_with_resolution.is_empty() => Some("revision_resolution"),
        _ if c.kind == stemma::audit::DirectChangeKind::BlockModified
            && c.old_excerpt == c.new_excerpt =>
        {
            Some("property_change")
        }
        _ => None,
    }
}

fn session_evidences_resolution_effect(
    change: &stemma::audit::DirectChange,
    evidence: Option<&SessionResolutionEvidence>,
) -> bool {
    let Some(evidence) = evidence else {
        return false;
    };
    let mut current = change.old_excerpt.clone();
    let mut matched = false;
    for batch in &evidence.direct_transition_batches {
        let mut candidates = batch.iter().filter(|transition| {
            transition.story == change.story
                && transition.block_id == change.block_id
                && transition.old_excerpt == current
        });
        let Some(transition) = candidates.next() else {
            continue;
        };
        // Two effects in one atomic resolution with the same location and
        // starting value are ambiguous to replay. Refuse to attribute the
        // final row rather than guessing.
        if candidates.next().is_some() {
            return false;
        }
        current = transition.new_excerpt.clone();
        matched = true;
    }
    // A later direct mutation changes the final value and fails here.
    matched && current == change.new_excerpt
}

fn audit_direct_row_json(
    c: &stemma::audit::DirectChange,
    resolution_evidence: Option<&SessionResolutionEvidence>,
) -> Value {
    json!({
        "story": c.story,
        "kind": c.kind.as_str(),
        "block_id": c.block_id.as_ref().map(|id| id.to_string()),
        "old_excerpt": c.old_excerpt.as_deref().map(cap_excerpt),
        "new_excerpt": c.new_excerpt.as_deref().map(cap_excerpt),
        "coincides_with_resolution": c.coincides_with_resolution,
        "explanation": audit_direct_change_explanation(c),
        "session_resolution_evidenced":
            session_evidences_resolution_effect(c, resolution_evidence),
    })
}

/// Bounded `AuditReport` wire shape shared by `review_session` and
/// `audit_docx` (RFC 0001): sections 1–4, every claim engine-derived and every
/// list explicitly paged. A selected detail page changes only that section;
/// the other sections remain on their bounded first page. The caller adds its
/// identity fields (`doc_id` / paths) and optional `render`.
fn audit_report_json(
    report: &stemma::audit::AuditReport,
    resolution_evidence: Option<&SessionResolutionEvidence>,
    detail: Option<AuditDetail>,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<Value, String> {
    use stemma::audit::{RevisionDisposition, UntouchedViolationKind};

    let unexplained_direct_changes = report
        .direct_changes
        .iter()
        .filter(|change| {
            let intrinsically_explained = matches!(&change.story, StoryScope::Comment { .. })
                || (change.kind == stemma::audit::DirectChangeKind::BlockModified
                    && change.old_excerpt == change.new_excerpt);
            !intrinsically_explained
                && !session_evidences_resolution_effect(change, resolution_evidence)
        })
        .count();
    let changed_prior_revisions = report
        .preexisting_revisions
        .iter()
        .filter(|prior| !matches!(&prior.disposition, RevisionDisposition::Untouched))
        .count();
    let expected_changed_prior_revisions = report
        .preexisting_revisions
        .iter()
        .filter(|prior| {
            matches!(&prior.disposition, RevisionDisposition::Resolved)
                && resolution_evidence.is_some_and(|evidence| {
                    evidence.revision_ids.contains(&prior.record.revision_id)
                })
        })
        .count();
    let unexpected_changed_prior_revisions =
        changed_prior_revisions - expected_changed_prior_revisions;

    let census: Vec<Value> = report
        .new_revisions
        .iter()
        .map(audit_census_row_json)
        .collect();
    let direct: Vec<Value> = report
        .direct_changes
        .iter()
        .map(|change| audit_direct_row_json(change, resolution_evidence))
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
        .map(validation_issue_json)
        .collect();

    let request = parse_audit_page_request(detail, offset, limit)?;
    let coordinates = |section: AuditDetail| match request {
        Some(request) if request.detail == section => (request.offset, request.limit),
        _ => (0, DEFAULT_AUDIT_PAGE_ROWS),
    };
    let (census_offset, census_limit) = coordinates(AuditDetail::Census);
    let census = audit_rows_page(&census, census_offset, census_limit)?;
    let (direct_offset, direct_limit) = coordinates(AuditDetail::DirectDelta);
    let direct = audit_rows_page(&direct, direct_offset, direct_limit)?;
    let (preexisting_offset, preexisting_limit) = coordinates(AuditDetail::Preexisting);
    let preexisting = audit_rows_page(&preexisting, preexisting_offset, preexisting_limit)?;
    let (violations_offset, violations_limit) = coordinates(AuditDetail::Violations);
    let (violations, violations_page) = audit_array_and_page(audit_rows_page(
        &violations,
        violations_offset,
        violations_limit,
    )?);
    let (issues_offset, issues_limit) = coordinates(AuditDetail::ValidatorIssues);
    let (validator_issues, validator_issues_page) = audit_array_and_page(audit_rows_page(
        &validator_issues,
        issues_offset,
        issues_limit,
    )?);

    let mut payload = json!({
        "counts": {
            "new_revisions": report.new_revisions.len(),
            "direct_changes": report.direct_changes.len(),
            "unexplained_direct_changes": unexplained_direct_changes,
            "preexisting_revisions": report.preexisting_revisions.len(),
            "changed_prior_revisions": changed_prior_revisions,
            "expected_changed_prior_revisions": expected_changed_prior_revisions,
            "unexpected_changed_prior_revisions": unexpected_changed_prior_revisions,
            "untouched_violations": report.untouched.violations.len(),
            "validator_issues": report.validator.issues.len(),
            "new_validator_issues": Value::Null,
        },
        "session": {
            "census": census,
            "direct_delta": direct,
        },
        "preexisting": preexisting,
        "untouched": {
            "verified_blocks": report.untouched.verified_blocks,
            "parts": report.untouched.parts,
            "violations": violations,
            "violations_page": violations_page,
        },
        "validator": {
            "ok": report.validator.ok,
            "issues": validator_issues,
            "issues_page": validator_issues_page,
        },
    });
    if let Some(request) = request {
        payload["requested_detail"] = json!({
            "section": request.detail.as_str(),
            "offset": request.offset,
            "limit": request.limit,
        });
    }
    Ok(payload)
}

fn attach_baseline_validation(
    payload: &mut Value,
    baseline: &stemma::runtime::ValidationReport,
    current: &stemma::runtime::ValidationReport,
) {
    let baseline_issues: Vec<Value> = baseline.issues.iter().map(validation_issue_json).collect();
    let baseline_page = audit_rows_page(&baseline_issues, 0, DEFAULT_AUDIT_PAGE_ROWS)
        .expect("zero-offset default audit page is valid");
    let (baseline_rows, baseline_page) = audit_array_and_page(baseline_page);
    payload["baseline_validator"] = json!({
        "ok": baseline.ok,
        "issues": baseline_rows,
        "issues_page": baseline_page,
    });

    let new_issue_count = current
        .issues
        .iter()
        .filter(|issue| !baseline.issues.contains(issue))
        .count();
    let resolved_baseline_issue_count = baseline
        .issues
        .iter()
        .filter(|issue| !current.issues.contains(issue))
        .count();
    payload["validator"]["baseline_issue_count"] = json!(baseline.issues.len());
    payload["validator"]["new_issue_count"] = json!(new_issue_count);
    payload["validator"]["resolved_baseline_issue_count"] = json!(resolved_baseline_issue_count);
    payload["validator"]["unchanged_from_baseline"] =
        json!(new_issue_count == 0 && resolved_baseline_issue_count == 0);
    payload["counts"]["new_validator_issues"] = json!(new_issue_count);
    let blocking_finding_count = payload["counts"]["unexplained_direct_changes"]
        .as_u64()
        .expect("audit count is an integer")
        + payload["counts"]["unexpected_changed_prior_revisions"]
            .as_u64()
            .expect("audit count is an integer")
        + payload["counts"]["untouched_violations"]
            .as_u64()
            .expect("audit count is an integer")
        + u64::try_from(new_issue_count).expect("usize fits u64 on supported targets");
    payload["verdict"] = json!({
        "status": if blocking_finding_count == 0 { "pass" } else { "fail" },
        "deliverable": blocking_finding_count == 0,
        "blocking_finding_count": blocking_finding_count,
    });
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
        output_role: &str,
        protected_sources: &[ArtifactIdentity],
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
        let output_artifact = self
            .artifacts
            .commit_new(
                out_path,
                output_role,
                &result.redline_bytes,
                protected_sources,
            )
            .map_err(artifact_fail)?;
        Ok(json!({
            "path": out_path,
            "change_count": result.diff.changes.len(),
            "bytes_written": result.redline_bytes.len(),
            "output_artifact": output_artifact,
            "server_version": SERVER_VERSION,
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
    /// Every tool call funnels through here. We (1) sweep expired documents and
    /// their coupled artifact state so a long-lived host cannot grow without
    /// bound, (2) dispatch to the router, and (3) upgrade an ambiguous missing-
    /// handle error into an actionable "re-open" / "unknown id" one. The
    /// `#[tool_handler]` macro fills in `list_tools`/`get_tool`; only `call_tool`
    /// is overridden (the macro skips a method we define ourselves).
    async fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let referenced_doc_id = request
            .arguments
            .as_ref()
            .and_then(|a| a.get("doc_id"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        if self.config.doc_ttl_secs > 0 {
            self.evict_expired_sessions(self.config.doc_ttl_secs);
        }
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
        //
        // The full editing playbook ships IN the server: `instructions.md` is
        // the canonical guidance (golden path, sharp edges, layering policy),
        // and every MCP client that surfaces server `instructions` gets it at
        // connect time — no client-specific skill file required. Packaging
        // must not fork the in-band guidance.
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("stemma-mcp", SERVER_VERSION))
            .with_instructions(match self.config.profile {
                ToolProfile::Core => CORE_INSTRUCTIONS,
                ToolProfile::Advanced => INSTRUCTIONS,
            })
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
    let artifacts = match artifact_authority_from_env() {
        Ok(authority) => authority,
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
        profile = ?config.profile,
        max_doc_bytes = config.max_doc_bytes,
        max_image_bytes = config.max_image_bytes,
        max_image_total_bytes = config.max_image_total_bytes,
        workspace_root = %artifacts.root().expect("production MCP authority is rooted").display(),
        "stemma-mcp starting on stdio"
    );
    let service = StemmaServer::with_config_and_authority(config, artifacts)
        .serve(stdio())
        .await?;
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

    /// The server-shipped instructions ARE the editing playbook — a cold agent
    /// on any MCP client must receive the golden path, the sharp edges, and
    /// the layering policy at connect time. Pins the load-bearing sections so
    /// a refactor cannot silently ship a stub (the pre-2026-07 state was a
    /// 395-char summary — 2% of the guidance — and every non-Claude client
    /// ran the engine blind).
    #[test]
    fn instructions_carry_the_full_playbook() {
        for marker in [
            "## Golden path",
            "## Sharp edges",
            "AuthorImpersonation",
            "replace_text",
            "## Multi-document tasks",
            "## Policy: layer beside, don't resolve, unless asked",
        ] {
            assert!(
                INSTRUCTIONS.contains(marker),
                "server instructions lost required section/marker: {marker}"
            );
        }
        assert!(
            INSTRUCTIONS.len() > 8_000,
            "server instructions suspiciously short ({} bytes) — stub shipped?",
            INSTRUCTIONS.len()
        );
    }

    /// A minimal DOCX (no styles part → Normal-styled paragraphs) whose body is
    /// `body_inner`, optionally with a numbering.xml carrying numId=1 (decimal)
    /// and numId=2 (bullet).
    pub(super) fn make_docx(body_inner: &str, with_numbering: bool) -> Vec<u8> {
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
    fn core_table_detail_is_bounded_paged_and_exactly_retrievable() {
        let cells: String = (0..12)
            .map(|col| {
                let text = format!("CELL-{col:02}-{}", "x".repeat(200));
                format!(r#"<w:tc><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:tc>"#)
            })
            .collect();
        let body = format!(
            r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tr>{cells}</w:tr></w:tbl>"#
        );
        let doc = Document::parse(&make_docx(&body, false)).expect("parse table");
        let view = doc.read();
        let table = view
            .blocks
            .iter()
            .find(|block| matches!(block.role, BlockRole::Table))
            .expect("table block");

        let full = block_detail_json(table);
        assert_eq!(full["cells"].as_array().expect("full cells").len(), 12);
        assert!(
            full["cells"][0]["text"]
                .as_str()
                .expect("full cell text")
                .chars()
                .count()
                > BLOCK_CELL_EXCERPT_CHARS,
            "advanced full-read surface keeps exact unbounded cell text"
        );

        let first = core_block_detail_json(table, InspectBlockDetail::Compact, None, None)
            .expect("default core table page");
        assert_eq!(first["text"], Value::Null);
        assert_eq!(first["table_text_omitted"], true);
        assert_eq!(first["cell_count"], 12);
        assert_eq!(first["cells_returned"], DEFAULT_BLOCK_CELL_LIMIT);
        assert_eq!(first["cells_has_more"], true);
        assert_eq!(first["cells_next_offset"], DEFAULT_BLOCK_CELL_LIMIT);
        let first_cell = &first["cells"][0];
        assert_eq!(first_cell["text_truncated"], true);
        assert_eq!(
            first_cell["text_excerpt"]
                .as_str()
                .expect("excerpt")
                .chars()
                .count(),
            BLOCK_CELL_EXCERPT_CHARS
        );
        let paragraph_id = first_cell["block_ids"][0]
            .as_str()
            .expect("cell paragraph id");
        let exact = cell_paragraph_detail_json(&view, paragraph_id, InspectBlockDetail::Compact)
            .expect("returned cell paragraph is inspectable");
        assert_eq!(exact["text"], full["cells"][0]["text"]);

        let second = core_block_detail_json(
            table,
            InspectBlockDetail::Formatting,
            Some(DEFAULT_BLOCK_CELL_LIMIT),
            Some(8),
        )
        .expect("continuation page");
        assert_eq!(second["cells_returned"], 4);
        assert_eq!(second["cells_has_more"], false);
        assert_eq!(second["cells_next_offset"], Value::Null);
        assert_eq!(second["cells"][0]["col"], DEFAULT_BLOCK_CELL_LIMIT);
    }

    #[test]
    fn core_paragraph_detail_rejects_table_cell_paging_arguments() {
        let doc = Document::parse(&make_docx(
            r#"<w:p><w:r><w:t>Body paragraph.</w:t></w:r></w:p>"#,
            false,
        ))
        .expect("parse");
        let error = core_block_detail_json(
            &doc.read().blocks[0],
            InspectBlockDetail::Compact,
            Some(0),
            None,
        )
        .expect_err("cell paging on a paragraph must fail loud");
        assert!(error.contains("require a table block"), "{error}");
    }

    #[test]
    fn core_document_markdown_bounds_tables_without_hiding_the_read_path() {
        let cells: String = (0..12)
            .map(|col| {
                let text = format!("CELL-{col:02}-{}-TAIL-CELL-{col:02}", "x".repeat(300));
                format!(r#"<w:tc><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:tc>"#)
            })
            .collect();
        let body = format!(
            r#"<w:p><w:r><w:t>Exact prose remains complete.</w:t></w:r></w:p><w:tbl><w:tblPr/><w:tr>{cells}</w:tr></w:tbl>"#
        );
        let doc = Document::parse(&make_docx(&body, false)).expect("parse table document");
        let view = doc.read();
        let core = core_document_markdown(&view.blocks);
        let advanced = to_extended_markdown_blocks(&view.blocks);
        let table = view
            .blocks
            .iter()
            .find(|block| matches!(block.role, BlockRole::Table))
            .expect("table block");
        assert_eq!(
            core_table_markdown_header(table),
            to_extended_markdown_blocks(std::slice::from_ref(table))
                .lines()
                .next()
                .expect("advanced table header"),
            "the bounded core summary retains the authoritative table header"
        );

        assert!(core.contains("Exact prose remains complete."));
        assert!(core.contains("kind=table cells=12"));
        assert_eq!(core.matches("\ncell[").count(), 4);
        assert!(core.contains("<more cells=8 next_offset=4 inspect_block=tbl_1/>"));
        assert!(
            core.contains("blocks=p_2"),
            "first cell is addressable: {core}"
        );
        assert!(
            !core.contains("TAIL-CELL-11"),
            "an unrequested late cell body must not leak into the bounded page"
        );
        assert!(
            advanced.contains("TAIL-CELL-11"),
            "advanced exact Markdown remains complete"
        );
        assert!(
            core.len() * 3 < advanced.len(),
            "bounded table projection should be materially smaller: core={} advanced={}",
            core.len(),
            advanced.len()
        );
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
    fn compact_block_detail_preserves_every_opaque_anchor_id() {
        let body = r#"<w:p>
            <w:r><w:fldChar w:fldCharType="begin"/></w:r>
            <w:r><w:instrText>PAGE</w:instrText></w:r>
            <w:r><w:fldChar w:fldCharType="separate"/></w:r>
            <w:r><w:t>1</w:t></w:r>
            <w:r><w:fldChar w:fldCharType="end"/></w:r>
        </w:p>"#;
        let doc = Document::parse(&make_docx(body, false)).expect("parse field paragraph");
        let view = doc.read();
        let full = block_detail_json(&view.blocks[0]);
        let compact = compact_block_detail_json(&view.blocks[0]);
        let full_ids: Vec<&str> = full["spans"]
            .as_array()
            .expect("formatting spans")
            .iter()
            .filter(|span| span["kind"] == "anchor")
            .filter_map(|span| span["id"].as_str())
            .collect();
        let compact_ids: Vec<&str> = compact["anchors"]
            .as_array()
            .expect("compact anchors")
            .iter()
            .filter_map(|anchor| anchor["id"].as_str())
            .collect();
        assert!(!full_ids.is_empty(), "fixture must contain opaque anchors");
        assert_eq!(compact_ids, full_ids, "compact detail loses no anchor ids");
        assert_eq!(compact["formatting_available"], true);
        assert!(compact.get("spans").is_none());
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

    /// Gemini's function-calling dialect rejects JSON Schema references. The
    /// compact default profile must therefore advertise self-contained schemas;
    /// downstream clients may still normalize nullable/union syntax for their
    /// own dialect, but they must not need an out-of-band definition resolver.
    #[test]
    fn core_tool_schemas_are_self_contained() {
        let router = StemmaServer::router_for_profile(ToolProfile::Core);
        for tool in router.list_all() {
            let name = tool.name;
            let value = Value::Object(tool.input_schema.as_ref().clone());
            let encoded = value.to_string();
            assert!(
                !encoded.contains("\"$defs\"") && !encoded.contains("\"$ref\""),
                "{name} must not advertise JSON Schema references: {value}"
            );
            assert!(
                !encoded.contains("\"type\":\"null\""),
                "{name} must express optionality through `required`, not a null union: {value}"
            );
            assert!(
                !encoded.contains("\"type\":["),
                "{name} must advertise one Gemini-compatible type per field: {value}"
            );
        }

        let execute = router.get("execute_plan").expect("execute schema");
        let execute = Value::Object(execute.input_schema.as_ref().clone());
        let replacement = &execute["properties"]["replacement_worklist"]["properties"]["replacements"]
            ["items"]["properties"];
        assert_eq!(
            replacement["match_mode"]["enum"],
            json!(["exact", "normalize_ws"])
        );
        assert_eq!(
            replacement["on_barrier_match"]["enum"],
            json!(["skip", "fail"])
        );

        let inspect = router.get("inspect_docx").expect("inspect schema");
        let inspect = Value::Object(inspect.input_schema.as_ref().clone());
        assert_eq!(
            inspect["properties"]["detail"]["enum"],
            json!(["compact", "formatting"])
        );

        let verify = router.get("verify_docx").expect("verify schema");
        let verify = Value::Object(verify.input_schema.as_ref().clone());
        assert_eq!(
            verify["properties"]["detail"]["enum"],
            json!([
                "census",
                "direct_delta",
                "preexisting",
                "violations",
                "validator_issues"
            ])
        );
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

    #[test]
    fn successful_payloads_carry_the_server_version() {
        let result = ok(json!({ "ok": true }));
        let payload = result
            .structured_content
            .expect("ok() produces a structured payload");
        assert_eq!(payload["server_version"], SERVER_VERSION);
        assert_eq!(payload["ok"], true);
    }

    #[test]
    fn augmented_receipts_keep_text_and_structured_content_equal() {
        let mut result = ok(json!({ "applied": true }));
        attach_field(&mut result, "match_count", json!(2));
        let structured = result.structured_content.as_ref().unwrap();
        let text: Value =
            serde_json::from_str(&result.content[0].as_text().expect("text fallback").text)
                .unwrap();
        assert_eq!(&text, structured);
    }

    #[test]
    fn preview_apply_cue_marks_only_complete_successful_previews() {
        let mut transaction = ok(json!({"would_apply": true}));
        let before_bytes = serde_json::to_vec(transaction.structured_content.as_ref().unwrap())
            .expect("serialize base preview")
            .len();
        attach_preview_apply_cue(&mut transaction);
        let after_bytes = serde_json::to_vec(transaction.structured_content.as_ref().unwrap())
            .expect("serialize guided preview")
            .len();
        assert!(
            after_bytes - before_bytes < 256,
            "decision-point cue must stay tiny: before={before_bytes} after={after_bytes}"
        );
        let transaction = transaction.structured_content.expect("transaction payload");
        assert_eq!(transaction["apply_ready"], true);
        assert!(
            transaction["next_action"]
                .as_str()
                .is_some_and(|text| text.contains("identical plan"))
        );

        let mut complete_worklist = ok(json!({"would_apply": 3, "failed": 0}));
        attach_preview_apply_cue(&mut complete_worklist);
        assert_eq!(
            complete_worklist.structured_content.as_ref().unwrap()["apply_ready"],
            true
        );

        let mut partial_worklist = ok(json!({"would_apply": 2, "failed": 1}));
        attach_preview_apply_cue(&mut partial_worklist);
        assert!(
            partial_worklist
                .structured_content
                .as_ref()
                .unwrap()
                .get("apply_ready")
                .is_none(),
            "a partial worklist must be repaired, never invited to apply"
        );
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
    // default is the broad BodyAndTables scope — the caller's blast-radius
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

    // ─── ChangeSelector::ByFilter: AND-combined bulk resolution ───────────────
    //
    // Domain rule under test: "resolve author X's changes in <range>" is ONE
    // selector call — the conjunction the single-axis selectors cannot express.
    // Fixture: AuthorA inserts in p_1, AuthorB deletes in p_1, AuthorB inserts
    // in p_3; p_2 is clean (a range boundary that must exclude p_1).

    /// p_1: "Keep " + <ins AuthorA "alpha "> + <del AuthorB "beta "> + "tail."
    /// p_2: clean boundary paragraph.
    /// p_3: "End " + <ins AuthorB "gamma "> + "stop."
    fn filter_selector_docx() -> Vec<u8> {
        let body = concat!(
            r#"<w:p><w:r><w:t xml:space="preserve">Keep </w:t></w:r>"#,
            r#"<w:ins w:id="10" w:author="AuthorA" w:date="2026-01-01T00:00:00Z"><w:r><w:t xml:space="preserve">alpha </w:t></w:r></w:ins>"#,
            r#"<w:del w:id="11" w:author="AuthorB" w:date="2026-02-01T00:00:00Z"><w:r><w:delText xml:space="preserve">beta </w:delText></w:r></w:del>"#,
            r#"<w:r><w:t>tail.</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t>Clean boundary.</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t xml:space="preserve">End </w:t></w:r>"#,
            r#"<w:ins w:id="12" w:author="AuthorB" w:date="2026-02-01T00:00:00Z"><w:r><w:t xml:space="preserve">gamma </w:t></w:r></w:ins>"#,
            r#"<w:r><w:t>stop.</w:t></w:r></w:p>"#,
        );
        make_docx(body, false)
    }

    /// Author ∧ range: rejecting AuthorB inside p_3..p_3 must remove exactly
    /// the gamma insertion — AuthorB's p_1 deletion and AuthorA's insertion
    /// stay pending. This is the "reject author X's changes in section Y
    /// only" review instruction as a one-call selector.
    #[tokio::test]
    async fn by_filter_resolves_author_within_range_only() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &filter_selector_docx()).await;
        let rejected = server
            .reject_changes(Parameters(RejectArgs {
                doc_id: doc_id.clone(),
                selector: ChangeSelector::ByFilter {
                    by_author: Some("AuthorB".to_string()),
                    by_kind: None,
                    by_block_range: Some(BlockRange {
                        from_block_id: "p_3".to_string(),
                        to_block_id: "p_3".to_string(),
                    }),
                },
            }))
            .await;
        let payload = structured(&rejected);
        assert_eq!(rejected.is_error, Some(false), "{payload}");
        assert_eq!(
            payload["rejected_revision_ids"].as_array().map(Vec::len),
            Some(1),
            "exactly the in-range AuthorB revision is selected: {payload}"
        );

        let canonical = server
            .runtime
            .with(&DocHandle(doc_id), |snapshot| {
                Arc::clone(&snapshot.canonical)
            })
            .unwrap();
        let remaining: Vec<(String, Option<String>)> = revision_rows(&canonical)
            .iter()
            .map(|row| (row.block_id.to_string(), row.author.clone()))
            .collect();
        assert_eq!(
            remaining.len(),
            2,
            "AuthorA's insertion and AuthorB's out-of-range deletion stay pending: {remaining:?}"
        );
        assert!(
            remaining.iter().all(|(block, _)| block == "p_1"),
            "every surviving revision lives outside the filtered range: {remaining:?}"
        );
    }

    /// Author ∧ kind: accepting AuthorB's deletions must leave AuthorB's
    /// insertion pending — the kind axis uses the same vocabulary as the
    /// revision-inventory filter.
    #[tokio::test]
    async fn by_filter_kind_axis_selects_within_author() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &filter_selector_docx()).await;
        let accepted = server
            .accept_changes(Parameters(AcceptArgs {
                doc_id: doc_id.clone(),
                selector: ChangeSelector::ByFilter {
                    by_author: Some("AuthorB".to_string()),
                    by_kind: Some("delete".to_string()),
                    by_block_range: None,
                },
            }))
            .await;
        let payload = structured(&accepted);
        assert_eq!(accepted.is_error, Some(false), "{payload}");
        assert_eq!(
            payload["accepted_revision_ids"].as_array().map(Vec::len),
            Some(1),
            "exactly AuthorB's deletion is selected: {payload}"
        );
        let canonical = server
            .runtime
            .with(&DocHandle(doc_id), |snapshot| {
                Arc::clone(&snapshot.canonical)
            })
            .unwrap();
        let authors_pending: Vec<Option<String>> = revision_rows(&canonical)
            .iter()
            .map(|row| row.author.clone())
            .collect();
        assert_eq!(
            authors_pending.len(),
            2,
            "AuthorA's insertion and AuthorB's p_3 insertion stay pending: {authors_pending:?}"
        );
    }

    /// An all-empty filter is `all` in disguise — it must fail loudly, never
    /// resolve everything by accident.
    #[tokio::test]
    async fn by_filter_requires_at_least_one_axis() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &filter_selector_docx()).await;
        let result = server
            .reject_changes(Parameters(RejectArgs {
                doc_id,
                selector: ChangeSelector::ByFilter {
                    by_author: None,
                    by_kind: None,
                    by_block_range: None,
                },
            }))
            .await;
        assert_eq!(result.is_error, Some(true));
        let message = format!("{result:?}");
        assert!(
            message.contains("at least one"),
            "the refusal names the missing axes: {message}"
        );
    }

    /// Wire shape: the selector parses from its documented JSON form, and an
    /// unrecognized extra field is refused (same unknown-field contract as the
    /// other variants).
    #[test]
    fn by_filter_wire_shape_parses_and_denies_unknown_fields() {
        let wire = serde_json::json!({
            "by": "by_filter",
            "by_author": "Alice",
            "by_block_range": {"from_block_id": "p_2", "to_block_id": "p_5"},
        });
        let selector: ChangeSelector =
            serde_json::from_value(wire).expect("documented by_filter wire shape parses");
        assert!(matches!(
            selector,
            ChangeSelector::ByFilter {
                by_author: Some(_),
                by_kind: None,
                by_block_range: Some(_),
            }
        ));
        let extra =
            serde_json::json!({ "by": "by_filter", "by_author": "Alice", "author": "Alice" });
        serde_json::from_value::<ChangeSelector>(extra)
            .expect_err("an unrecognized extra field inside by_filter must be refused");
    }

    // ─── inspect_docx revisions_summary: counts, not rows ─────────────────────

    /// Domain rule: a review round is triaged by exact author × kind counts
    /// before any row is read. The rollup carries NO rows and composes with
    /// the same AND-combined filter as the inventory.
    #[tokio::test]
    async fn revisions_summary_counts_by_author_and_kind() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &filter_selector_docx()).await;
        let summary = structured(
            &server
                .inspect_docx(Parameters(
                    serde_json::from_value(json!({
                        "doc_id": doc_id.clone(),
                        "query": "revisions_summary",
                    }))
                    .expect("documented wire shape parses"),
                ))
                .await,
        );
        assert_eq!(summary["total"], 3, "{summary}");
        assert!(
            summary.get("revisions").is_none(),
            "the rollup carries counts, not rows: {summary}"
        );
        let authors = summary["by_author"].as_array().expect("by_author array");
        let of = |name: &str| {
            authors
                .iter()
                .find(|a| a["author"] == name)
                .unwrap_or_else(|| panic!("author {name} missing: {summary}"))
        };
        assert_eq!(of("AuthorA")["kinds"]["insert"], 1);
        assert_eq!(of("AuthorA")["total"], 1);
        assert_eq!(of("AuthorB")["kinds"]["insert"], 1);
        assert_eq!(of("AuthorB")["kinds"]["delete"], 1);
        assert_eq!(of("AuthorB")["total"], 2);

        let filtered = structured(
            &server
                .inspect_docx(Parameters(
                    serde_json::from_value(json!({
                        "doc_id": doc_id,
                        "query": "revisions_summary",
                        "filter": {"by_block_range": {"from_block_id": "p_3", "to_block_id": "p_3"}},
                    }))
                    .expect("filtered wire shape parses"),
                ))
                .await,
        );
        assert_eq!(
            filtered["total"], 1,
            "the rollup composes with the shared filter: {filtered}"
        );
    }

    // ─── Resolution receipts: exact counts, bounded lists ─────────────────────
    //
    // Domain rule: a receipt reports WHAT HAPPENED (exact counts, bounded
    // evidence), it is not a projection. A bulk resolution must not inline
    // hundreds of ids and block rows; the cap is disclosed, never silent.

    /// N paragraphs, each carrying exactly one AuthorA insertion — a bulk
    /// review round bigger than every receipt cap.
    fn bulk_insertions_docx(paragraphs: usize) -> Vec<u8> {
        let mut body = String::new();
        for i in 0..paragraphs {
            body.push_str(&format!(
                concat!(
                    r#"<w:p><w:r><w:t xml:space="preserve">Clause {i} </w:t></w:r>"#,
                    r#"<w:ins w:id="{id}" w:author="AuthorA" w:date="2026-01-01T00:00:00Z">"#,
                    r#"<w:r><w:t xml:space="preserve">added {i} </w:t></w:r></w:ins>"#,
                    r#"<w:r><w:t>end.</w:t></w:r></w:p>"#
                ),
                i = i,
                id = 100 + i,
            ));
        }
        make_docx(&body, false)
    }

    /// Bulk acceptance: counts exact, every list capped, truncation explicit.
    #[tokio::test]
    async fn bulk_resolution_receipt_is_bounded_with_explicit_truncation() {
        const N: usize = 70; // above RECEIPT_ID_CAP (64) and ROW_CAP (16)
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &bulk_insertions_docx(N)).await;
        let accepted = server
            .accept_changes(Parameters(AcceptArgs {
                doc_id,
                selector: ChangeSelector::ByAuthor {
                    author: "AuthorA".to_string(),
                },
            }))
            .await;
        let payload = structured(&accepted);
        assert_eq!(accepted.is_error, Some(false), "{payload}");
        assert_eq!(payload["accepted"], N, "exact count survives the cap");
        assert_eq!(
            payload["accepted_revision_ids"].as_array().map(Vec::len),
            Some(StemmaServer::RECEIPT_ID_CAP),
            "the id list is bounded: {payload}"
        );
        assert_eq!(payload["changed_block_count"], N);
        assert_eq!(
            payload["changed_blocks"].as_array().map(Vec::len),
            Some(StemmaServer::RECEIPT_BLOCK_ROW_CAP),
            "block rows are bounded: {payload}"
        );
        let truncated = &payload["truncated"];
        assert_eq!(
            truncated["accepted_revision_ids_omitted"],
            N - StemmaServer::RECEIPT_ID_CAP,
            "the cap is disclosed, never silent: {payload}"
        );
        assert_eq!(
            truncated["changed_blocks_rows_omitted"],
            N - StemmaServer::RECEIPT_BLOCK_ROW_CAP
        );
        assert!(
            truncated["advice"]
                .as_str()
                .is_some_and(|a| a.contains("inspect_docx")),
            "the truncation report names the follow-up surface: {payload}"
        );
        for (field, total, returned) in [
            (
                "accepted_revision_ids_evidence",
                N,
                StemmaServer::RECEIPT_ID_CAP,
            ),
            (
                "changed_block_ids_evidence",
                N,
                StemmaServer::RECEIPT_ID_CAP,
            ),
            (
                "changed_blocks_evidence",
                N,
                StemmaServer::RECEIPT_BLOCK_ROW_CAP,
            ),
        ] {
            let evidence = &payload[field];
            assert_eq!(evidence["total"], total, "{field}: {payload}");
            assert_eq!(evidence["returned"], returned, "{field}: {payload}");
            assert_eq!(evidence["omitted"], total - returned, "{field}: {payload}");
            assert_eq!(
                evidence["set_sha256"].as_str().map(str::len),
                Some(64),
                "{field} must commit to its complete pre-cap set: {payload}"
            );
        }
    }

    #[tokio::test]
    async fn bulk_resolution_preview_echoes_selector_counts_and_set_commitment() {
        const N: usize = 70;
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &bulk_insertions_docx(N)).await;
        let preview = server
            .execute_plan(Parameters(ExecutePlanArgs {
                doc_id: Some(doc_id),
                transaction: None,
                resolution: Some(ResolutionPlanArg {
                    action: ResolutionActionArg::Accept,
                    selector: ChangeSelector::All,
                }),
                replacement_worklist: None,
                comparison: None,
                preview: true,
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        let payload = structured(&preview);
        assert_eq!(preview.is_error, Some(false), "{payload}");
        assert_eq!(payload["resolution"]["selector"], json!({"by": "all"}));
        assert_eq!(payload["resolution"]["selected"], N);
        assert_eq!(
            payload["resolution"]["revision_ids"]
                .as_array()
                .map(Vec::len),
            Some(StemmaServer::RECEIPT_ID_CAP)
        );
        assert_eq!(
            payload["resolution"]["revision_ids_evidence"]["omitted"],
            N - StemmaServer::RECEIPT_ID_CAP
        );
        assert_eq!(
            payload["resolution"]["revision_ids_evidence"]["set_sha256"]
                .as_str()
                .map(str::len),
            Some(64)
        );
    }

    /// A small resolution keeps the complete lists and carries NO truncated
    /// key — the bounded receipt only changes shape when something was capped.
    #[tokio::test]
    async fn small_resolution_receipt_is_complete_and_untruncated() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &filter_selector_docx()).await;
        let accepted = server
            .accept_changes(Parameters(AcceptArgs {
                doc_id,
                selector: ChangeSelector::ByAuthor {
                    author: "AuthorA".to_string(),
                },
            }))
            .await;
        let payload = structured(&accepted);
        assert_eq!(accepted.is_error, Some(false), "{payload}");
        assert_eq!(payload["accepted"], 1);
        assert_eq!(
            payload["accepted_revision_ids"].as_array().map(Vec::len),
            Some(1),
            "small receipts stay complete: {payload}"
        );
        assert!(
            payload.get("truncated").is_none(),
            "nothing was capped, so nothing is reported truncated: {payload}"
        );
    }

    /// Transaction outcomes are the decision plane: one row per submitted op,
    /// even when the transaction is larger than every evidence-list cap.
    #[tokio::test]
    async fn atomic_transaction_outcomes_are_complete_above_receipt_caps() {
        const N: usize = 70;
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &make_multi_para_docx(N)).await;
        let ops: Vec<Value> = (0..N)
            .map(|i| {
                json!({
                    "op": "replace",
                    "target": format!("p_{}", i + 1),
                    "expect": format!("Paragraph {i}"),
                    "content": {
                        "type": "paragraph",
                        "content": [{"type": "text", "text": format!("Rewritten {i}.")}],
                    },
                })
            })
            .collect();
        let result = server
            .apply_batch(Parameters(BatchArgs {
                doc_id,
                transaction: TransactionArg(json!({
                    "ops": ops,
                    "revision": {"author": "Decision Plane Test"},
                })),
                preview: true,
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        let payload = structured(&result);
        assert_eq!(result.is_error, Some(false), "{payload}");
        assert_eq!(payload["operation_count"], N);
        assert_eq!(payload["atomicity"]["mode"], "all");
        assert_eq!(payload["atomicity"]["status"], "would_apply");
        assert_eq!(payload["changed_block_count"], N);
        assert_eq!(
            payload["changed_block_ids"].as_array().map(Vec::len),
            Some(StemmaServer::RECEIPT_ID_CAP)
        );
        assert_eq!(
            payload["changed_blocks"].as_array().map(Vec::len),
            Some(StemmaServer::RECEIPT_BLOCK_ROW_CAP)
        );
        for field in [
            "revision_ids_evidence",
            "changed_block_ids_evidence",
            "changed_blocks_evidence",
        ] {
            assert_eq!(
                payload[field]["set_sha256"].as_str().map(str::len),
                Some(64),
                "capped transaction evidence commits to the full set: {payload}"
            );
        }
        let outcomes = payload["operation_outcomes"]
            .as_array()
            .expect("transaction outcomes");
        assert_eq!(outcomes.len(), N, "no decision outcome may be capped");
        for (index, outcome) in outcomes.iter().enumerate() {
            assert_eq!(outcome["index"], index);
            assert_eq!(outcome["status"], "would_apply");
        }
        assert!(
            payload.get("operation_outcomes_evidence").is_none(),
            "decision rows are structurally inline, never an evidence set: {payload}"
        );
    }

    #[tokio::test]
    async fn refused_atomic_transaction_still_accounts_for_every_operation() {
        const N: usize = 70;
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &make_multi_para_docx(N)).await;
        let ops: Vec<Value> = (0..N)
            .map(|i| {
                json!({
                    "op": "replace",
                    "target": format!("p_{}", i + 1),
                    "expect": format!("Paragraph {i}"),
                    "content": {
                        "type": "paragraph",
                        "content": [{"type": "text", "text": format!("Rewritten {i}.")}],
                    },
                })
            })
            .collect();
        let result = server
            .apply_batch(Parameters(BatchArgs {
                doc_id,
                transaction: TransactionArg(json!({
                    "ops": ops,
                    "revision": {"author": "Decision Plane Test"},
                })),
                preview: false,
                mode: Some("not_a_mode".to_string()),
                allow_existing_author: false,
            }))
            .await;
        let payload = structured(&result);
        assert_eq!(result.is_error, Some(true), "{payload}");
        assert_eq!(payload["operation_count"], N);
        assert_eq!(payload["atomicity"]["status"], "refused");
        let outcomes = payload["operation_outcomes"]
            .as_array()
            .expect("refused transaction outcomes");
        assert_eq!(outcomes.len(), N);
        assert!(
            outcomes
                .iter()
                .all(|outcome| outcome["status"] == "not_applied"),
            "atomic refusal leaves every operation unapplied: {payload}"
        );
    }

    /// Non-atomic worklists have the same completeness rule: every submitted
    /// item gets an inline outcome, including failures, with no cap metadata.
    #[tokio::test]
    async fn replacement_worklist_outcomes_are_complete_above_receipt_caps() {
        const N: usize = 70;
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &make_multi_para_docx(1)).await;
        let replacements = (0..N)
            .map(|i| ReplaceItem {
                old: format!("absent phrase {i}"),
                new: format!("replacement {i}"),
                scope: None,
                expected_matches: None,
                match_mode: "exact".to_string(),
                on_barrier_match: "skip".to_string(),
            })
            .collect();
        let result = server
            .replace_text_batch(Parameters(ReplaceTextBatchArgs {
                doc_id,
                author: "Decision Plane Test".to_string(),
                replacements,
                preview: true,
                allow_existing_author: false,
            }))
            .await;
        let payload = structured(&result);
        assert_eq!(result.is_error, Some(false), "{payload}");
        assert_eq!(payload["submitted"], N);
        assert_eq!(payload["failed"], N);
        assert_eq!(
            payload["items"].as_array().map(Vec::len),
            Some(N),
            "every submitted worklist item must have an inline outcome: {payload}"
        );
        assert!(
            payload.get("items_evidence").is_none(),
            "decision rows are never cappable evidence: {payload}"
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

        // Four pending revisions: the plain ins ("added ") and del ("removed "),
        // and the stacked span's ins + del over "contested " (a stacked span is
        // TWO rows — one per resolvable revision). H7: rows carry engine-minted
        // IDENTITIES (not the wire ids 10/11/1/2), so identify each row by its
        // stable content (kind + excerpt), and assert the identities are simply
        // four distinct, resolvable (non-zero) handles.
        assert_eq!(rows.len(), 4, "four rows");
        let by = |kind: RevisionKind, excerpt: &str| {
            rows.iter()
                .find(|r| r.kind == kind && r.excerpt == excerpt)
                .unwrap_or_else(|| panic!("a {kind:?} row for {excerpt:?}"))
        };
        let ids: Vec<u32> = rows.iter().map(|r| r.revision_id).collect();
        let unique: std::collections::HashSet<u32> = ids.iter().copied().collect();
        assert_eq!(unique.len(), 4, "four distinct identities: {ids:?}");
        assert!(
            ids.iter().all(|&id| id != 0),
            "every id resolvable: {ids:?}"
        );

        let ins = by(RevisionKind::Insert, "added ");
        assert_eq!(ins.author.as_deref(), Some("AuthorA"));
        assert_eq!(ins.block_id, "p_1");
        assert_eq!(ins.date.as_deref(), Some("2026-01-01T00:00:00Z"));

        let del = by(RevisionKind::Delete, "removed ");
        assert_eq!(del.author.as_deref(), Some("AuthorB"));

        // The stacked span: the insertion (AuthorA) and the deletion (AuthorB)
        // are independent rows, each resolvable on its own identity.
        assert_eq!(
            by(RevisionKind::Insert, "contested ").author.as_deref(),
            Some("AuthorA")
        );
        assert_eq!(
            by(RevisionKind::Delete, "contested ").author.as_deref(),
            Some("AuthorB")
        );
    }

    #[test]
    fn revision_row_location_is_body_for_an_ordinary_body_revision_and_crosses_the_wire() {
        let doc = Document::parse(&redline_docx()).expect("parse redline");
        let rows = revision_rows(&doc.snapshot().canonical);
        let ins = rows
            .iter()
            .find(|r| r.kind == RevisionKind::Insert && r.excerpt == "added ")
            .unwrap();
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
            .find(|r| r.kind == RevisionKind::Insert && r.author.as_deref() == Some("Reviewer"))
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

    #[tokio::test]
    async fn compact_notes_query_surfaces_editable_note_identity_and_body() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &footnote_docx()).await;
        let result = server
            .inspect_docx(Parameters(InspectDocxArgs {
                doc_id,
                query: InspectQuery::Notes,
                block_id: None,
                detail: None,
                pattern: None,
                patterns: None,
                filter: None,
                from_block_id: None,
                to_block_id: None,
                format: None,
                offset: None,
                limit: None,
                cell_offset: None,
                cell_limit: None,
            }))
            .await;
        let payload = structured(&result);
        assert_eq!(result.is_error, Some(false), "{payload}");
        assert_eq!(payload["note_count"], 1);
        assert_eq!(
            payload["notes"],
            json!([{"note_id": "1", "kind": "footnote", "text": "See Appendix C"}]),
            "the five-tool surface must expose everything edit_note requires: {payload}"
        );
    }

    #[test]
    fn compact_operation_catalog_is_parser_complete_and_routes_the_old_surface() {
        let payload = operation_catalog(None).expect("complete catalog");
        let operations = payload["operations"].as_array().expect("operation rows");
        let authoritative = stemma::edit_v4::operation_vocabulary();
        assert_eq!(operations.len(), authoritative.len());
        for (name, fields) in authoritative {
            let row = operations
                .iter()
                .find(|row| row["name"] == *name)
                .unwrap_or_else(|| panic!("catalog omitted parser operation {name}"));
            assert_eq!(row["parser_fields"], json!(fields));
            let accepted = row["accepted_fields"].as_array().expect("accepted fields");
            for field in *fields {
                assert!(
                    accepted.iter().any(|candidate| candidate == field),
                    "catalog omitted parser field {name}.{field}"
                );
            }
            assert!(row["group"].as_str().is_some_and(|group| !group.is_empty()));
            assert!(row["cue"].as_str().is_some_and(|cue| !cue.is_empty()));
        }

        for image_op in ["insert_image", "replace_image"] {
            let row = operations
                .iter()
                .find(|row| row["name"] == image_op)
                .expect("image operation row");
            assert_eq!(row["mcp_edge_fields"], json!(["path"]));
            assert!(
                row["accepted_fields"]
                    .as_array()
                    .is_some_and(|fields| fields.iter().any(|field| field == "path")),
                "compact catalog must advertise the server-resolved image path: {row}"
            );
            assert!(
                row["mcp_edge_examples"]
                    .as_array()
                    .is_some_and(|examples| !examples.is_empty()),
                "compact catalog must show the edge-valid image path shape: {row}"
            );
        }

        let routes = payload["legacy_surface_routes"]
            .as_array()
            .expect("historical route map");
        assert_eq!(routes.len(), 26, "every historical tool has a core route");
        for required in [
            "compare_docx",
            "read_accepted",
            "read_rejected",
            "read_redline",
            "read_index",
            "get_section",
            "apply_edit",
            "replace_text_batch",
        ] {
            assert!(
                routes.iter().any(|row| row["historical_tool"] == required),
                "historical capability {required} has no five-tool route"
            );
        }

        for operation in [
            "move",
            "table_op",
            "insert_note",
            "edit_note",
            "create_style",
            "set_page_setup",
            "insert_image",
        ] {
            let one = operation_catalog(Some(operation)).expect("known operation");
            let row = &one["operations"][0];
            assert_eq!(row["name"], operation);
            assert!(
                row["examples"]
                    .as_array()
                    .is_some_and(|examples| !examples.is_empty()),
                "historically taught operation {operation} needs an exact example"
            );
        }
    }

    /// An unknown-op refusal must name the agent's next move, not only dump
    /// the catalog: near-name guesses get closest matches, and the observed
    /// guessed intents (toc/image/field — cold agents guessed all three) get
    /// their capability's real spelling.
    #[test]
    fn unknown_operation_refusal_names_the_next_move() {
        let toc = operation_catalog(Some("toc")).expect_err("toc is not an op");
        assert!(
            toc.contains("{\"type\":\"toc\"}"),
            "the toc guess routes to the insert content block: {toc}"
        );
        let image = operation_catalog(Some("image")).expect_err("image is not an op");
        assert!(
            image.contains("closest matches") && image.contains("insert_image"),
            "the image guess names the real image ops: {image}"
        );
        let typo = operation_catalog(Some("zzz_no_such_op")).expect_err("unknown");
        assert!(
            typo.contains("known operations"),
            "a no-match guess still gets the full catalog list: {typo}"
        );
    }

    #[tokio::test]
    async fn compact_projection_queries_preserve_historical_read_vocabulary() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &redline_docx()).await;
        for (query, field) in [
            (InspectQuery::Text, "text"),
            (InspectQuery::Html, "html"),
            (InspectQuery::Redline, "markdown"),
            (InspectQuery::Accepted, "markdown"),
            (InspectQuery::Rejected, "markdown"),
        ] {
            let result = server
                .inspect_docx(Parameters(InspectDocxArgs {
                    doc_id: doc_id.clone(),
                    query,
                    block_id: None,
                    detail: None,
                    pattern: None,
                    patterns: None,
                    filter: None,
                    from_block_id: None,
                    to_block_id: None,
                    format: None,
                    offset: None,
                    limit: None,
                    cell_offset: None,
                    cell_limit: None,
                }))
                .await;
            let payload = structured(&result);
            assert_eq!(result.is_error, Some(false), "{payload}");
            assert!(
                payload[field]
                    .as_str()
                    .is_some_and(|content| !content.is_empty()),
                "compact projection must carry the historical {field} payload: {payload}"
            );
        }
    }

    #[test]
    fn revision_filters_are_and_combined_on_author_and_kind() {
        let doc = Document::parse(&redline_docx()).expect("parse redline");
        let rows = revision_rows(&doc.snapshot().canonical);

        // H7: rows carry minted identities (not wire ids), so assert the filter
        // COMPOSITION by the structural facts of this fixture, not literal ids.
        // by_author: AuthorA owns exactly the two insertions.
        let author_a: Vec<&_> = rows
            .iter()
            .filter(|r| r.author.as_deref() == Some("AuthorA"))
            .collect();
        assert_eq!(author_a.len(), 2, "AuthorA owns two revisions");
        assert!(
            author_a.iter().all(|r| r.kind == RevisionKind::Insert),
            "both of AuthorA's revisions are insertions"
        );

        // by_kind: exactly two deletions (the plain del + the stacked del).
        let deletes: Vec<u32> = rows
            .iter()
            .filter(|r| r.kind == RevisionKind::Delete)
            .map(|r| r.revision_id)
            .collect();
        assert_eq!(deletes.len(), 2, "two deletions");

        // AND-combined: AuthorB has no insertions, so author+kind intersects to
        // exactly the deletions.
        let author_b_deletes: Vec<u32> = rows
            .iter()
            .filter(|r| r.author.as_deref() == Some("AuthorB") && r.kind == RevisionKind::Delete)
            .map(|r| r.revision_id)
            .collect();
        assert_eq!(
            author_b_deletes, deletes,
            "AuthorB's deletions are exactly all deletions (AuthorB never inserted)"
        );
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

    #[test]
    fn find_excerpt_is_bounded_and_keeps_a_late_match_visible() {
        let text = format!("{}UniquE Needle{}", "α".repeat(400), "ω".repeat(400));
        let excerpt = match_excerpt(&text, "unique needle", FIND_TEXT_EXCERPT_CHARS);
        assert_eq!(excerpt.chars().count(), FIND_TEXT_EXCERPT_CHARS);
        assert!(
            excerpt.to_lowercase().contains("unique needle"),
            "match must remain visible in bounded excerpt: {excerpt}"
        );
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
        assert_eq!(report["set_sha256"].as_str().map(str::len), Some(64));
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

    pub(super) fn structured(result: &CallToolResult) -> Value {
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
                task: None,
                task_id: None,
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

    /// A move toward the start of the document places its destination copy
    /// before its source shadow. The collapsed move census row names that
    /// first destination carrier, but verified delivery must still account
    /// for the paired source carrier and permit the save.
    #[tokio::test]
    async fn save_docx_delivers_tracked_move_with_destination_before_source() {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("input.docx"), make_multi_para_docx(6))
            .expect("write input");
        let authority = PathAuthority::rooted(workspace.path()).expect("rooted authority");
        let server = StemmaServer::with_config_and_authority(Config::defaults(), authority);
        let opened = server
            .open_docx(Parameters(OpenArgs {
                path: "input.docx".to_string(),
                task: None,
                task_id: None,
            }))
            .await;
        let doc_id = structured(&opened)["doc_id"]
            .as_str()
            .expect("open doc id")
            .to_string();

        let moved = server
            .apply_edit(Parameters(ApplyEditArgs {
                doc_id: doc_id.clone(),
                transaction: TransactionArg(json!({
                    "ops": [{
                        "op": "move",
                        "target": "p_4",
                        "destination": { "anchor": "p_1", "position": "after" },
                    }],
                    "revision": { "author": "Mover" },
                })),
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        assert_eq!(structured(&moved)["applied"], true, "{moved:?}");

        let saved = server
            .save_docx(Parameters(SaveArgs {
                doc_id,
                path: "moved.docx".to_string(),
            }))
            .await;
        let payload = structured(&saved);
        assert_eq!(saved.is_error, Some(false), "{payload}");
        assert_eq!(
            payload["audit_binding"]["verdict"]["deliverable"], true,
            "{payload}"
        );
        assert_eq!(
            payload["audit_binding"]["counts"]["untouched_violations"],
            0
        );
        assert!(
            workspace.path().join("moved.docx").is_file(),
            "verified save creates the requested destination"
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
                task: None,
                task_id: None,
            }))
            .await;
        let doc_id = structured(&open)["doc_id"]
            .as_str()
            .expect("doc_id")
            .to_string();

        let transaction = replace_txn_arg("p_50", "Paragraph 49", "Paragraph FIFTY rewritten.");
        let preview = server
            .apply_batch(Parameters(BatchArgs {
                doc_id: doc_id.clone(),
                transaction: transaction.clone(),
                preview: true,
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        let preview_payload = structured(&preview);
        let preview_bytes =
            serde_json::to_vec(&preview_payload).expect("serialize preview receipt");
        assert!(
            preview_payload.get("preview_outline").is_none(),
            "preview must not echo the whole document: {preview_payload}"
        );
        assert_eq!(preview_payload["changed_block_ids"], json!(["p_50"]));
        assert_eq!(
            preview_payload["changed_blocks"]
                .as_array()
                .expect("changed blocks")
                .len(),
            1
        );
        assert!(
            preview_bytes.len() < 16 * 1024,
            "preview receipt must be < 16KB on a 102-block doc, was {} bytes",
            preview_bytes.len()
        );

        let result = server
            .apply_edit(Parameters(ApplyEditArgs {
                doc_id: doc_id.clone(),
                transaction,
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

        // open_docx returns a bounded first page of the compact index. It must
        // be strictly smaller than the OLD heavy outline that tripped the
        // host's truncation limit (~49KB), AND it must DROP the two
        // heaviest per-block fields — full `text` and `semantic_hash` — which
        // are what made the heavy outline blow up. The compact row carries only
        // a bounded text_preview.
        let open_payload = structured(&open);
        let index = open_payload["index"].as_array().expect("index rows");
        assert_eq!(index.len(), DEFAULT_CORE_INDEX_LIMIT);
        assert_eq!(open_payload["index_has_more"], true);
        assert_eq!(open_payload["index_next_offset"], DEFAULT_CORE_INDEX_LIMIT);
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
        assert!(
            compact_bytes.len() < 32 * 1024,
            "bounded open receipt must be < 32KB on a 102-block doc, was {} bytes",
            compact_bytes.len()
        );
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

        let document_page = server
            .inspect_docx(Parameters(InspectDocxArgs {
                doc_id: doc_id.clone(),
                query: InspectQuery::Document,
                block_id: None,
                detail: None,
                pattern: None,
                patterns: None,
                filter: None,
                from_block_id: None,
                to_block_id: None,
                format: None,
                offset: None,
                limit: None,
                cell_offset: None,
                cell_limit: None,
            }))
            .await;
        let document_payload = structured(&document_page);
        assert_eq!(document_payload["returned"], DEFAULT_CORE_DOCUMENT_LIMIT);
        assert_eq!(document_payload["total_blocks"], 102);
        assert_eq!(document_payload["has_more"], true);
        assert_eq!(document_payload["next_offset"], DEFAULT_CORE_DOCUMENT_LIMIT);
        assert!(
            document_payload["content"]
                .as_str()
                .expect("paged markdown")
                .contains("#p_1")
        );
        let document_bytes = serde_json::to_vec(&document_payload).expect("serialize page");
        assert!(
            document_bytes.len() < 16 * 1024,
            "default document page must be bounded on a 102-block doc, was {} bytes",
            document_bytes.len()
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
                task: None,
                task_id: None,
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
                task: None,
                task_id: None,
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
                task: None,
                task_id: None,
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

    #[tokio::test]
    async fn check_and_batch_preview_enforce_the_commit_author_guard() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &redline_docx()).await;
        let transaction = || {
            TransactionArg(json!({
                "ops": [{
                    "op": "replace",
                    "target": "p_2",
                    "expect": "Second clean paragraph.",
                    "content": {"type": "paragraph", "content": [
                        {"type": "text", "text": "Second paragraph, tightened."}
                    ]}
                }],
                "revision": {"author": "AuthorA"}
            }))
        };

        let checked = server
            .check_edit(Parameters(CheckArgs {
                doc_id: doc_id.clone(),
                transaction: transaction(),
            }))
            .await;
        assert_eq!(structured(&checked)["code"], "AuthorImpersonation");
        assert_eq!(structured(&checked)["would_apply"], false);

        let previewed = server
            .apply_batch(Parameters(BatchArgs {
                doc_id,
                transaction: transaction(),
                preview: true,
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        assert_eq!(structured(&previewed)["code"], "AuthorImpersonation");
        assert_eq!(structured(&previewed)["would_apply"], false);
    }

    #[tokio::test]
    async fn check_preview_and_commit_share_package_aware_style_validation() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &make_multi_para_docx(2)).await;
        let transaction = || {
            TransactionArg(json!({
                "ops": [{
                    "op": "apply_style",
                    "target": "p_1",
                    "style_id": "DefinitelyMissingStyle"
                }],
                "revision": {"author": "Reviewer"}
            }))
        };

        let checked = server
            .check_edit(Parameters(CheckArgs {
                doc_id: doc_id.clone(),
                transaction: transaction(),
            }))
            .await;
        let previewed = server
            .apply_batch(Parameters(BatchArgs {
                doc_id: doc_id.clone(),
                transaction: transaction(),
                preview: true,
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        let committed = server
            .apply_edit(Parameters(ApplyEditArgs {
                doc_id,
                transaction: transaction(),
                mode: None,
                allow_existing_author: false,
            }))
            .await;

        for result in [&checked, &previewed, &committed] {
            assert_eq!(
                structured(result)["code"],
                "AnchorNotFound",
                "preview and commit must share the package-aware refusal: {}",
                structured(result)
            );
        }
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

    /// The MCP default body scope includes table-cell paragraphs. A global
    /// replacement must therefore edit both occurrences and honestly report no
    /// unreached cells.
    #[tokio::test]
    async fn replace_text_default_scope_reaches_table_cell_matches() {
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
        assert_eq!(p["match_count"], 2, "body and table-cell matches apply");
        let unreached = p["unreached_matches"]
            .as_array()
            .expect("unreached_matches present");
        assert!(
            unreached.is_empty(),
            "default scope reached every cell: {p}"
        );
    }

    /// An explicit scope is the caller's complete search boundary. A matching
    /// table cell outside a body-paragraph scope is deliberately excluded, not
    /// reported as an unreached edit that might prompt the agent to widen scope.
    #[tokio::test]
    async fn replace_text_explicit_scope_does_not_report_outside_matches() {
        let server = StemmaServer::new();
        let body = r#"<w:p><w:r><w:t>Acme Corp is the party.</w:t></w:r></w:p><w:tbl><w:tblPr/><w:tr><w:tc><w:tcPr/><w:p><w:r><w:t>Acme Corp</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#;
        let doc_id = open_and_id(&server, &make_docx(body, false)).await;
        let found = structured(
            &server
                .find(Parameters(FindArgs {
                    doc_id: doc_id.clone(),
                    pattern: "is the party".to_string(),
                    offset: None,
                    limit: None,
                    cell_offset: None,
                    cell_limit: None,
                }))
                .await,
        );
        let body_id = found["matches"][0]["id"]
            .as_str()
            .expect("body paragraph id")
            .to_string();

        let result = server
            .replace_text(Parameters(ReplaceTextArgs {
                doc_id,
                old: "Acme Corp".to_string(),
                new: "Acme Corporation".to_string(),
                author: "Counsel".to_string(),
                scope: Some(ReplaceTextScopeArg {
                    block_id: Some(body_id),
                    from_block_id: None,
                    to_block_id: None,
                }),
                expected_matches: Some(ExpectedMatchesArg::Count(1)),
                match_mode: "exact".to_string(),
                on_barrier_match: "skip".to_string(),
                allow_existing_author: false,
            }))
            .await;
        let payload = structured(&result);
        assert_ne!(result.is_error, Some(true), "scoped replacement succeeds");
        assert_eq!(payload["match_count"], 1);
        assert_eq!(payload["unreached_matches"], json!([]));
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
                preview: false,
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
    // The receipt reports the exact after-minus-before semantic identity set.
    // These tests pin the load-bearing cases so a future edge reconstruction
    // cannot silently break per-op attribution by treating identities as
    // counters or wire ids.

    fn rev_ids(p: &Value) -> Vec<u64> {
        p["revision_ids"]
            .as_array()
            .expect("revision_ids array")
            .iter()
            .map(|v| v.as_u64().expect("revision id is a number"))
            .collect()
    }

    /// A TRACKED edit on a clean document: the receipt's revision_ids are exactly
    /// the revisions the document now carries (the doc had none before).
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
        assert!(
            ids.iter().all(|id| *id != 0),
            "identities are nonzero: {ids:?}"
        );
        let distinct: HashSet<_> = ids.iter().collect();
        assert_eq!(distinct.len(), ids.len(), "identities are unique: {ids:?}");

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

    #[tokio::test]
    async fn compact_revision_query_preserves_author_kind_and_range_filters() {
        let body = concat!(
            r#"<w:p><w:r><w:t>One </w:t></w:r><w:ins w:id="1" w:author="Prior"><w:r><w:t>alpha</w:t></w:r></w:ins></w:p>"#,
            r#"<w:p><w:r><w:t>Two </w:t></w:r><w:ins w:id="2" w:author="Other"><w:r><w:t>beta</w:t></w:r></w:ins></w:p>"#,
        );
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &make_docx(body, false)).await;
        let result = server
            .inspect_docx(Parameters(InspectDocxArgs {
                doc_id,
                query: InspectQuery::Revisions,
                block_id: None,
                detail: None,
                pattern: None,
                patterns: None,
                filter: Some(RevisionFilter {
                    by_author: Some("Prior".to_string()),
                    by_kind: Some("insert".to_string()),
                    by_block_range: Some(BlockRange {
                        from_block_id: "p_1".to_string(),
                        to_block_id: "p_1".to_string(),
                    }),
                }),
                from_block_id: None,
                to_block_id: None,
                format: None,
                offset: None,
                limit: None,
                cell_offset: None,
                cell_limit: None,
            }))
            .await;
        let payload = structured(&result);
        assert_eq!(result.is_error, Some(false), "{payload}");
        assert_eq!(payload["total"], 1, "{payload}");
        assert_eq!(payload["revisions"][0]["author"], "Prior");
        assert_eq!(payload["revisions"][0]["block_id"], "p_1");
    }

    /// A tracked edit on a doc that ALREADY carries pending revisions: the
    /// receipt names ONLY the newly-minted revisions, never the pre-existing
    /// ones. RFC-0004 §H7 regression: "new" is identity-set difference, never a
    /// wire-space or numeric-identity watermark.
    #[tokio::test]
    async fn revision_ids_exclude_preexisting_revisions() {
        // p_1 arrives with a pre-existing tracked insertion (wire id 5); p_2 is
        // clean and is what we edit.
        let body = concat!(
            r#"<w:p><w:r><w:t xml:space="preserve">Base </w:t></w:r><w:ins w:id="5" w:author="Prior" w:date="2020-01-01T00:00:00Z"><w:r><w:t>added</w:t></w:r></w:ins></w:p>"#,
            r#"<w:p><w:r><w:t>Second paragraph.</w:t></w:r></w:p>"#,
        );
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &make_docx(body, false)).await;

        // The doc already has one pending revision before we touch it.
        let before = structured(
            &server
                .list_revisions(Parameters(ListRevisionsArgs {
                    doc_id: doc_id.clone(),
                    filter: None,
                }))
                .await,
        );
        let preexisting: Vec<u64> = before["revisions"]
            .as_array()
            .expect("revisions array")
            .iter()
            .map(|r| r["revision_id"].as_u64().expect("revision_id"))
            .collect();
        assert_eq!(preexisting.len(), 1, "one pre-existing revision: {before}");

        let result = server
            .apply_edit(Parameters(ApplyEditArgs {
                doc_id,
                transaction: replace_txn_arg("p_2", "Second paragraph.", "Second REWRITTEN."),
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        let p = structured(&result);
        let ids = rev_ids(&p);
        assert!(!ids.is_empty(), "the tracked edit created revisions: {p}");
        assert!(
            !ids.iter().any(|id| preexisting.contains(id)),
            "the receipt must NOT report the pre-existing revision {preexisting:?} as new: {ids:?}"
        );
    }

    /// A DIRECT-mode edit leaves NO tracked revisions (it stamps then resolves),
    /// so the receipt's revision_ids is empty — the honest answer (nothing is
    /// pending review). The after-minus-before identity set is empty.
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

    #[tokio::test]
    async fn compact_transaction_composes_row_insert_and_delete_on_one_table() {
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
        let table_detail = structured(
            &server
                .inspect_docx(Parameters(InspectDocxArgs {
                    doc_id: doc_id.clone(),
                    query: InspectQuery::Block,
                    block_id: Some(table_id.clone()),
                    detail: None,
                    pattern: None,
                    patterns: None,
                    filter: None,
                    from_block_id: None,
                    to_block_id: None,
                    format: None,
                    offset: None,
                    limit: None,
                    cell_offset: None,
                    cell_limit: None,
                }))
                .await,
        );
        let guard = table_detail["guard"]
            .as_str()
            .expect("table detail carries a guard")
            .to_string();
        let plan = || {
            TransactionArg(json!({
                "ops": [
                    {
                        "op": "table_op",
                        "target": table_id.clone(),
                        "semantic_hash": guard.clone(),
                        "table_op": {
                            "kind": "insert_row",
                            "ref_row": 1,
                            "position": "after",
                            "cells": ["NEW0", "NEW1"]
                        }
                    },
                    {
                        "op": "table_op",
                        "target": table_id.clone(),
                        "semantic_hash": guard.clone(),
                        "table_op": {"kind": "delete_row", "row_index": 0}
                    }
                ],
                "revision": {"author": "Table Transaction Test"}
            }))
        };

        let preview = server
            .execute_plan(Parameters(ExecutePlanArgs {
                doc_id: Some(doc_id.clone()),
                transaction: Some(plan()),
                resolution: None,
                replacement_worklist: None,
                comparison: None,
                preview: true,
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        let preview_payload = structured(&preview);
        assert_eq!(preview.is_error, Some(false), "{preview_payload}");
        assert_eq!(preview_payload["apply_ready"], true, "{preview_payload}");

        let applied = server
            .execute_plan(Parameters(ExecutePlanArgs {
                doc_id: Some(doc_id),
                transaction: Some(plan()),
                resolution: None,
                replacement_worklist: None,
                comparison: None,
                preview: false,
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        let applied_payload = structured(&applied);
        assert_eq!(applied.is_error, Some(false), "{applied_payload}");
        assert_eq!(applied_payload["applied"], true, "{applied_payload}");
        assert!(
            applied_payload["revision_ids"]
                .as_array()
                .is_some_and(|ids| ids.len() >= 2),
            "both structural changes must remain pending: {applied_payload}"
        );
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

    /// Every paragraph id surfaced inside a table is a real edit target and
    /// must therefore be inspectable through the compact core before editing.
    #[tokio::test]
    async fn compact_block_inspection_reaches_table_cell_paragraphs() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &make_table_docx()).await;
        let found = structured(
            &server
                .find(Parameters(FindArgs {
                    doc_id: doc_id.clone(),
                    pattern: "R0C1".to_string(),
                    offset: None,
                    limit: None,
                    cell_offset: None,
                    cell_limit: None,
                }))
                .await,
        );
        let cell_id = found["matches"][0]["matching_cells"]
            .as_array()
            .expect("matching table cells")
            .iter()
            .find(|cell| cell["text_excerpt"] == "R0C1")
            .and_then(|cell| cell["block_id"].as_str())
            .expect("matching cell paragraph id")
            .to_string();

        let detail = server
            .inspect_docx(Parameters(InspectDocxArgs {
                doc_id,
                query: InspectQuery::Block,
                block_id: Some(cell_id.clone()),
                detail: None,
                pattern: None,
                patterns: None,
                filter: None,
                from_block_id: None,
                to_block_id: None,
                format: None,
                offset: None,
                limit: None,
                cell_offset: None,
                cell_limit: None,
            }))
            .await;
        let payload = structured(&detail);
        assert_eq!(detail.is_error, Some(false), "nested detail: {payload}");
        assert_eq!(payload["id"], cell_id);
        assert_eq!(payload["text"], "R0C1");
        assert_eq!(payload["nested_in"]["row"], 0);
        assert_eq!(payload["nested_in"]["col"], 1);
        assert_eq!(payload["detail"], "compact");
        assert_eq!(payload["formatting_available"], true);
        assert!(payload.get("segments").is_none());
        assert!(
            payload["guard"]
                .as_str()
                .is_some_and(|guard| !guard.is_empty())
        );
    }

    /// Run-level style objects are a deliberate on-demand expansion, not part
    /// of the permanent planning context. The exact text and guard agree across
    /// both projections, while the compact response stays materially smaller.
    #[tokio::test]
    async fn compact_cell_block_detail_omits_repeated_formatting_but_full_is_retrievable() {
        let runs: String = (0..40)
            .map(|index| {
                let color = format!("{:06X}", index + 1);
                format!(
                    r#"<w:r><w:rPr><w:b/><w:i/><w:color w:val="{color}"/><w:sz w:val="22"/><w:highlight w:val="yellow"/></w:rPr><w:t>Styled segment {index}. </w:t></w:r>"#
                )
            })
            .collect();
        let body = format!(
            r#"<w:tbl><w:tblPr/><w:tr><w:tc><w:tcPr/><w:p>{runs}</w:p></w:tc></w:tr></w:tbl>"#
        );
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &make_docx(&body, false)).await;
        let found = structured(
            &server
                .find(Parameters(FindArgs {
                    doc_id: doc_id.clone(),
                    pattern: "Styled segment 0".to_string(),
                    offset: None,
                    limit: None,
                    cell_offset: None,
                    cell_limit: None,
                }))
                .await,
        );
        let cell_id = found["matches"][0]["matching_cells"][0]["block_id"]
            .as_str()
            .expect("cell paragraph id")
            .to_string();
        let inspect = |detail| InspectDocxArgs {
            doc_id: doc_id.clone(),
            query: InspectQuery::Block,
            block_id: Some(cell_id.clone()),
            detail,
            pattern: None,
            patterns: None,
            filter: None,
            from_block_id: None,
            to_block_id: None,
            format: None,
            offset: None,
            limit: None,
            cell_offset: None,
            cell_limit: None,
        };

        let compact = structured(&server.inspect_docx(Parameters(inspect(None))).await);
        let formatting = structured(
            &server
                .inspect_docx(Parameters(inspect(Some(InspectBlockDetail::Formatting))))
                .await,
        );
        assert_eq!(compact["text"], formatting["text"]);
        assert_eq!(compact["guard"], formatting["guard"]);
        assert!(compact.get("segments").is_none());
        assert_eq!(formatting["segments"].as_array().map(Vec::len), Some(40));
        let compact_bytes = serde_json::to_vec(&compact)
            .expect("compact serialization")
            .len();
        let formatting_bytes = serde_json::to_vec(&formatting)
            .expect("formatting serialization")
            .len();
        assert!(
            compact_bytes * 4 < formatting_bytes,
            "compact detail should be at least 4x smaller: compact={compact_bytes}, formatting={formatting_bytes}"
        );
    }

    /// A table is one top-level find match, so top-level pagination alone does
    /// not bound its cell locators. Nested pagination must keep the default
    /// response small while making every matching cell exactly retrievable.
    #[tokio::test]
    async fn find_pages_matching_table_cells_without_hiding_any() {
        let rows: String = (0..20)
            .map(|index| {
                let padding = "x".repeat(300);
                format!(
                    r#"<w:tr><w:tc><w:tcPr/><w:p><w:r><w:t>{padding} [placeholder {index}]</w:t></w:r></w:p></w:tc></w:tr>"#
                )
            })
            .collect();
        let body = format!(r#"<w:tbl><w:tblPr/>{rows}</w:tbl>"#);
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &make_docx(&body, false)).await;
        let find_page = |cell_offset, cell_limit| FindArgs {
            doc_id: doc_id.clone(),
            pattern: "[".to_string(),
            offset: None,
            limit: None,
            cell_offset,
            cell_limit,
        };

        let first = structured(&server.find(Parameters(find_page(None, None))).await);
        assert_eq!(first["count"], 1, "the table is one block match");
        let first_table = &first["matches"][0];
        assert_eq!(first_table["matching_cell_count"], 20);
        assert_eq!(first_table["matching_cells_returned"], 4);
        assert_eq!(first_table["matching_cells_has_more"], true);
        assert_eq!(first_table["matching_cells_next_offset"], 4);
        assert!(
            first_table["matching_cells"]
                .as_array()
                .expect("matching cell locators")
                .iter()
                .all(|cell| cell["text_excerpt"]
                    .as_str()
                    .is_some_and(|excerpt| excerpt.contains("[placeholder"))),
            "even a late cell match remains visible in each bounded excerpt: {first_table}"
        );
        assert!(
            serde_json::to_vec(&first)
                .expect("serialize first page")
                .len()
                < 4 * 1024,
            "default broad find page must remain bounded: {first}"
        );

        let second = structured(&server.find(Parameters(find_page(Some(4), Some(8)))).await);
        let third = structured(&server.find(Parameters(find_page(Some(12), Some(8)))).await);
        assert_eq!(second["matches"][0]["matching_cells_returned"], 8);
        assert_eq!(second["matches"][0]["matching_cells_next_offset"], 12);
        assert_eq!(third["matches"][0]["matching_cells_returned"], 8);
        assert_eq!(third["matches"][0]["matching_cells_has_more"], false);
        assert_eq!(
            third["matches"][0]["matching_cells_next_offset"],
            Value::Null
        );

        let ids: HashSet<&str> = [&first, &second, &third]
            .into_iter()
            .flat_map(|page| {
                page["matches"][0]["matching_cells"]
                    .as_array()
                    .expect("matching cells")
            })
            .map(|cell| cell["block_id"].as_str().expect("cell paragraph id"))
            .collect();
        assert_eq!(ids.len(), 20, "all cell ids are retrievable exactly once");
    }

    #[tokio::test]
    async fn find_bounds_long_paragraphs_while_exact_block_text_remains_retrievable() {
        let exact = format!(
            "{}UniquE Needle{}",
            "long prefix ".repeat(80),
            " long suffix".repeat(80)
        );
        let body = format!(r#"<w:p><w:r><w:t>{exact}</w:t></w:r></w:p>"#);
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &make_docx(&body, false)).await;
        let found = structured(
            &server
                .find(Parameters(FindArgs {
                    doc_id: doc_id.clone(),
                    pattern: "unique needle".to_string(),
                    offset: None,
                    limit: None,
                    cell_offset: None,
                    cell_limit: None,
                }))
                .await,
        );
        let hit = &found["matches"][0];
        assert_eq!(hit["text"], Value::Null);
        assert_eq!(hit["text_chars"], exact.chars().count());
        assert_eq!(hit["text_truncated"], true);
        let excerpt = hit["text_excerpt"].as_str().expect("bounded excerpt");
        assert!(excerpt.to_lowercase().contains("unique needle"));
        assert_eq!(excerpt.chars().count(), FIND_TEXT_EXCERPT_CHARS);

        let detail = structured(
            &server
                .inspect_docx(Parameters(InspectDocxArgs {
                    doc_id,
                    query: InspectQuery::Block,
                    block_id: Some(hit["id"].as_str().expect("block id").to_string()),
                    detail: None,
                    pattern: None,
                    patterns: None,
                    filter: None,
                    from_block_id: None,
                    to_block_id: None,
                    format: None,
                    offset: None,
                    limit: None,
                    cell_offset: None,
                    cell_limit: None,
                }))
                .await,
        );
        assert_eq!(detail["text"], exact);
    }

    #[tokio::test]
    async fn compact_whole_document_worklist_previews_and_edits_a_table_cell_paragraph() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &make_table_docx()).await;
        let found = structured(
            &server
                .find(Parameters(FindArgs {
                    doc_id: doc_id.clone(),
                    pattern: "R0C1".to_string(),
                    offset: None,
                    limit: None,
                    cell_offset: None,
                    cell_limit: None,
                }))
                .await,
        );
        let cell_id = found["matches"][0]["matching_cells"][0]["block_id"]
            .as_str()
            .expect("matching cell paragraph id")
            .to_string();
        let worklist = || ReplacementWorklistArg {
            author: "Cell Worklist Test".to_string(),
            replacements: vec![CoreReplacementItem {
                effect_id: None,
                old: "R0C1".to_string(),
                new: "CELL REPLACED".to_string(),
                scope: None,
                expected_matches: Some(1),
                replace_all: false,
                match_mode: default_core_replacement_match_mode(),
                on_barrier_match: default_core_barrier_policy(),
            }],
        };

        let preview = server
            .execute_plan(Parameters(ExecutePlanArgs {
                doc_id: Some(doc_id.clone()),
                transaction: None,
                resolution: None,
                replacement_worklist: Some(worklist()),
                comparison: None,
                preview: true,
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        let preview_payload = structured(&preview);
        assert_eq!(preview.is_error, Some(false), "{preview_payload}");
        assert_eq!(preview_payload["would_apply"], 1);
        assert_eq!(preview_payload["items"][0]["unreached_matches"], json!([]));
        assert_eq!(preview_payload["apply_ready"], true);
        assert!(
            preview_payload["next_action"]
                .as_str()
                .is_some_and(|text| text.contains("preview=false"))
        );

        let before = structured(
            &server
                .inspect_docx(Parameters(InspectDocxArgs {
                    doc_id: doc_id.clone(),
                    query: InspectQuery::Block,
                    block_id: Some(cell_id.clone()),
                    detail: None,
                    pattern: None,
                    patterns: None,
                    filter: None,
                    from_block_id: None,
                    to_block_id: None,
                    format: None,
                    offset: None,
                    limit: None,
                    cell_offset: None,
                    cell_limit: None,
                }))
                .await,
        );
        assert_eq!(before["text"], "R0C1", "preview must not persist: {before}");

        let applied = server
            .execute_plan(Parameters(ExecutePlanArgs {
                doc_id: Some(doc_id.clone()),
                transaction: None,
                resolution: None,
                replacement_worklist: Some(worklist()),
                comparison: None,
                preview: false,
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        let applied_payload = structured(&applied);
        assert_eq!(applied.is_error, Some(false), "{applied_payload}");
        assert_eq!(applied_payload["applied"], 1);
        assert!(applied_payload.get("apply_ready").is_none());

        let after = structured(
            &server
                .inspect_docx(Parameters(InspectDocxArgs {
                    doc_id,
                    query: InspectQuery::Block,
                    block_id: Some(cell_id),
                    detail: None,
                    pattern: None,
                    patterns: None,
                    filter: None,
                    from_block_id: None,
                    to_block_id: None,
                    format: None,
                    offset: None,
                    limit: None,
                    cell_offset: None,
                    cell_limit: None,
                }))
                .await,
        );
        assert_eq!(after["text"], "CELL REPLACED");
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
        let authority = PathAuthority::explicit().expect("explicit test authority");
        let (resolved, sources) =
            resolve_image_paths(&authority, &txn, None, None).expect("resolves");
        let value: Value = serde_json::from_str(&resolved).expect("json");
        let op = &value["ops"][0];
        assert!(op.get("path").is_none(), "path removed after resolution");
        let encoded = op["bytes_base64"].as_str().expect("bytes_base64 present");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("decodes");
        assert_eq!(decoded, png, "bytes match the file on disk");
        assert_eq!(sources.len(), 1, "the consumed image is identified");
        assert_eq!(sources[0].bytes, png.len() as u64);
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
        let authority = PathAuthority::explicit().expect("explicit test authority");
        let err =
            resolve_image_paths(&authority, &txn, None, None).expect_err("both sources must fail");
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
        let authority = PathAuthority::explicit().expect("explicit test authority");
        let err = resolve_image_paths(&authority, &txn, None, None)
            .expect_err("neither source must fail");
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
        let authority = PathAuthority::explicit().expect("explicit test authority");
        let (resolved, sources) =
            resolve_image_paths(&authority, &txn, None, None).expect("resolves");
        let value: Value = serde_json::from_str(&resolved).expect("json");
        assert_eq!(value["ops"][0]["op"], "replace");
        assert_eq!(value["ops"][1]["bytes_base64"], "AAAA");
        assert!(sources.is_empty());
    }

    #[test]
    fn resolve_image_paths_enforces_per_file_and_aggregate_caps() {
        let png = png_100x50();
        let path = write_temp_png(&png);
        let one = json!({
            "ops": [
                { "op": "insert_image", "target": "p_1", "path": path.clone(), "format": "png" }
            ],
            "revision": { "author": "Imager" }
        })
        .to_string();
        let authority = PathAuthority::explicit().expect("explicit test authority");
        let error = resolve_image_paths(&authority, &one, Some(1), None)
            .expect_err("per-file cap refuses before encoding");
        assert_eq!(structured(&error)["code"], "artifact_source_too_large");
        assert_eq!(structured(&error)["env_var"], ENV_MAX_IMAGE_BYTES);

        let two = json!({
            "ops": [
                { "op": "insert_image", "target": "p_1", "path": path.clone(), "format": "png" },
                { "op": "insert_image", "target": "p_2", "path": path.clone(), "format": "png" }
            ],
            "revision": { "author": "Imager" }
        })
        .to_string();
        let error = resolve_image_paths(&authority, &two, None, Some(png.len() as u64 + 1))
            .expect_err("aggregate cap refuses the transaction");
        let payload = structured(&error);
        assert_eq!(payload["code"], "artifact_source_too_large");
        assert_eq!(payload["env_var"], ENV_MAX_IMAGE_TOTAL_BYTES);
        std::fs::remove_file(path).ok();
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
                task: None,
                task_id: None,
            }))
            .await;
        let doc_id = structured(&open)["doc_id"].as_str().unwrap().to_string();

        let png_path = write_temp_png(&png_100x50());
        let original_png = std::fs::read(&png_path).expect("read test png");
        let result = server
            .apply_edit(Parameters(ApplyEditArgs {
                doc_id: doc_id.clone(),
                transaction: TransactionArg(json!({
                    "ops": [
                        { "op": "insert_image", "target": "p_1",
                          "path": png_path.clone(), "format": "png" }
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
        let text_payload: Value =
            serde_json::from_str(&result.content[0].as_text().expect("text fallback").text)
                .expect("text fallback is JSON");
        assert_eq!(
            text_payload, payload,
            "text and structured MCP receipts must agree"
        );
        assert_eq!(
            Path::new(
                payload["input_artifacts"][0]["resolved_path"]
                    .as_str()
                    .expect("resolved path")
            ),
            Path::new(&png_path)
                .canonicalize()
                .expect("canonical png path"),
            "the apply receipt identifies the consumed image: {payload}"
        );

        let second = server
            .apply_edit(Parameters(ApplyEditArgs {
                doc_id: doc_id.clone(),
                transaction: TransactionArg(json!({
                    "ops": [
                        { "op": "insert_image", "target": "p_2",
                          "path": png_path.clone(), "format": "png" }
                    ],
                    "revision": { "author": "Imager Two" }
                })),
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        assert_eq!(second.is_error, Some(false), "second image insert applies");
        let review = server
            .review_session(Parameters(ReviewSessionArgs {
                doc_id: doc_id.clone(),
                render: None,
                detail: None,
                offset: None,
                limit: None,
            }))
            .await;
        assert_eq!(
            structured(&review)["input_artifacts"]
                .as_array()
                .unwrap()
                .len(),
            2,
            "the open DOCX plus one exact image identity are retained without duplicates"
        );

        let save = server
            .save_docx(Parameters(SaveArgs {
                doc_id,
                path: png_path.clone(),
            }))
            .await;
        assert_eq!(
            structured(&save)["code"],
            "artifact_protected_source",
            "an incorporated image remains protected for the session"
        );
        assert_eq!(
            std::fs::read(&png_path).expect("image survives refusal"),
            original_png
        );
        std::fs::remove_file(png_path).ok();
    }

    // ─── review_session / audit_docx (RFC 0001) ─────────────────────────────

    #[test]
    fn audit_rows_are_explicitly_paged_never_silently_truncated() {
        let small: Vec<Value> = (0..3).map(|i| json!({ "i": i })).collect();
        let payload = audit_rows_page(&small, 0, DEFAULT_AUDIT_PAGE_ROWS).unwrap();
        assert_eq!(payload["total"], 3);
        assert_eq!(payload["returned"], 3);
        assert_eq!(payload["has_more"], false);
        assert!(payload.get("next_offset").is_none(), "{payload}");

        let big: Vec<Value> = (0..DEFAULT_AUDIT_PAGE_ROWS + 7)
            .map(|i| json!({ "i": i }))
            .collect();
        let payload = audit_rows_page(&big, 0, DEFAULT_AUDIT_PAGE_ROWS).unwrap();
        assert_eq!(payload["total"], DEFAULT_AUDIT_PAGE_ROWS + 7);
        assert_eq!(
            payload["rows"].as_array().unwrap().len(),
            DEFAULT_AUDIT_PAGE_ROWS
        );
        assert_eq!(payload["has_more"], true);
        assert_eq!(payload["next_offset"], DEFAULT_AUDIT_PAGE_ROWS);
        assert_eq!(payload["omitted"], 7);
        let complete_set_hash = payload["set_sha256"]
            .as_str()
            .expect("audit page commits to the complete set")
            .to_string();
        assert_eq!(complete_set_hash.len(), 64);

        let tail = audit_rows_page(&big, DEFAULT_AUDIT_PAGE_ROWS, 7).unwrap();
        assert_eq!(tail["returned"], 7);
        assert_eq!(tail["has_more"], false);
        assert_eq!(tail["omitted"], DEFAULT_AUDIT_PAGE_ROWS);
        assert_eq!(
            tail["set_sha256"], complete_set_hash,
            "every page is bound to the same complete immutable set"
        );
        assert!(audit_rows_page(&big, big.len() + 1, 1).is_err());
        assert!(parse_audit_page_request(None, Some(0), None).is_err());
        assert!(
            parse_audit_page_request(
                Some(AuditDetail::Violations),
                None,
                Some(MAX_AUDIT_PAGE_ROWS + 1)
            )
            .is_err()
        );
    }

    #[test]
    fn baseline_validation_is_separated_from_new_regressions() {
        use stemma::runtime::{ValidationIssue, ValidationIssueCode, ValidationReport};

        let existing = ValidationIssue {
            code: ValidationIssueCode::WordprocessingInvariant,
            message: "pre-existing table finding".to_string(),
            context: Some("table t_1".to_string()),
        };
        let introduced = ValidationIssue {
            code: ValidationIssueCode::SchemaInvariant,
            message: "new finding".to_string(),
            context: Some("word/document.xml".to_string()),
        };
        let baseline = ValidationReport {
            ok: false,
            issues: vec![existing.clone()],
        };
        let current = ValidationReport {
            ok: false,
            issues: vec![existing, introduced],
        };
        let mut payload = json!({
            "validator": {},
            "counts": {
                "unexplained_direct_changes": 0,
                "changed_prior_revisions": 0,
                "unexpected_changed_prior_revisions": 0,
                "untouched_violations": 0,
            },
        });

        attach_baseline_validation(&mut payload, &baseline, &current);

        assert_eq!(payload["baseline_validator"]["issues_page"]["total"], 1);
        assert_eq!(payload["validator"]["baseline_issue_count"], 1);
        assert_eq!(payload["validator"]["new_issue_count"], 1);
        assert_eq!(payload["validator"]["resolved_baseline_issue_count"], 0);
        assert_eq!(payload["validator"]["unchanged_from_baseline"], false);
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
                detail: None,
                offset: None,
                limit: None,
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
        assert_eq!(payload["baseline_validator"]["ok"], true);
        assert_eq!(payload["validator"]["new_issue_count"], 0);
        assert_eq!(payload["validator"]["unchanged_from_baseline"], true);
        assert_eq!(payload["counts"]["unexplained_direct_changes"], 0);
        assert_eq!(payload["counts"]["changed_prior_revisions"], 0);
        assert_eq!(payload["counts"]["untouched_violations"], 0);
        assert_eq!(payload["counts"]["new_validator_issues"], 0);
        assert_eq!(payload["verdict"]["status"], "pass");
        assert_eq!(payload["verdict"]["deliverable"], true);
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
                detail: None,
                offset: None,
                limit: None,
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

    #[tokio::test]
    async fn review_session_accounts_for_requested_comment_annotation() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &make_multi_para_docx(2)).await;
        let applied = server
            .apply_edit(Parameters(ApplyEditArgs {
                doc_id: doc_id.clone(),
                transaction: TransactionArg(json!({
                    "ops": [{
                        "op": "comment_create",
                        "target": "p_1",
                        "expect": "Paragraph 0",
                        "body": "Confirm approval before proceeding.",
                        "author": "Reviewer"
                    }],
                    "revision": {"author": "Reviewer"}
                })),
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        assert_eq!(structured(&applied)["applied"], true, "{applied:?}");

        let review = server
            .review_session(Parameters(ReviewSessionArgs {
                doc_id,
                render: None,
                detail: None,
                offset: None,
                limit: None,
            }))
            .await;
        let payload = structured(&review);
        assert_eq!(review.is_error, Some(false), "{payload}");
        assert_eq!(payload["session"]["direct_delta"]["total"], 1);
        assert_eq!(
            payload["session"]["direct_delta"]["rows"][0]["explanation"], "comment_annotation",
            "the committed comment story is explicitly classified, not hidden: {payload}"
        );
        assert_eq!(
            payload["untouched"]["violations"].as_array().unwrap().len(),
            0,
            "comment range markers are accounted for by the annotation: {payload}"
        );
    }

    #[tokio::test]
    async fn review_session_pages_every_census_row_without_hiding_the_total() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &make_multi_para_docx(12)).await;
        for paragraph_index in 1..=9 {
            let result = server
                .apply_edit(Parameters(ApplyEditArgs {
                    doc_id: doc_id.clone(),
                    transaction: replace_txn_arg(
                        &format!("p_{}", paragraph_index + 1),
                        &format!("Paragraph {paragraph_index}"),
                        &format!("Paragraph {paragraph_index} rewritten."),
                    ),
                    mode: None,
                    allow_existing_author: false,
                }))
                .await;
            assert_eq!(structured(&result)["applied"], true, "{paragraph_index}");
        }

        let first = server
            .review_session(Parameters(ReviewSessionArgs {
                doc_id: doc_id.clone(),
                render: None,
                detail: None,
                offset: None,
                limit: None,
            }))
            .await;
        let first = structured(&first);
        let census = &first["session"]["census"];
        let total = census["total"].as_u64().unwrap() as usize;
        assert!(total > DEFAULT_AUDIT_PAGE_ROWS, "{first}");
        assert_eq!(
            census["rows"].as_array().unwrap().len(),
            DEFAULT_AUDIT_PAGE_ROWS
        );
        assert_eq!(census["has_more"], true);
        assert_eq!(census["next_offset"], DEFAULT_AUDIT_PAGE_ROWS);

        let tail = server
            .review_session(Parameters(ReviewSessionArgs {
                doc_id,
                render: None,
                detail: Some(AuditDetail::Census),
                offset: Some(DEFAULT_AUDIT_PAGE_ROWS),
                limit: Some(MAX_AUDIT_PAGE_ROWS),
            }))
            .await;
        let tail = structured(&tail);
        let census = &tail["session"]["census"];
        assert_eq!(census["total"], total);
        assert_eq!(
            census["returned"],
            total.saturating_sub(DEFAULT_AUDIT_PAGE_ROWS)
        );
        assert_eq!(census["has_more"], false);
        assert_eq!(tail["requested_detail"]["section"], "census");
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
                task: None,
                task_id: None,
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
                detail: None,
                offset: None,
                limit: None,
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

    #[cfg(unix)]
    fn create_file_symlink(source: &Path, destination: &Path) -> bool {
        std::os::unix::fs::symlink(source, destination).expect("create file symlink");
        true
    }

    /// APFS refuses to create names that are not valid UTF-8 (EILSEQ, os
    /// error 92 on macOS), so scenarios needing an on-disk non-UTF-8 path
    /// cannot be provisioned there — nor can such a canonical path reach the
    /// wire on that filesystem. Bounded skip, reported, never absorbed; any
    /// other error still panics.
    #[cfg(unix)]
    fn provision_non_utf8(result: std::io::Result<()>, what: &Path) -> bool {
        match result {
            Ok(()) => true,
            Err(error) if cfg!(target_os = "macos") && error.raw_os_error() == Some(92) => {
                eprintln!(
                    "skipping on-disk non-UTF-8 assertions: this filesystem cannot represent {}: {error}",
                    what.display()
                );
                false
            }
            Err(error) => panic!("provision non-UTF-8 name {}: {error}", what.display()),
        }
    }

    #[cfg(windows)]
    fn create_file_symlink(source: &Path, destination: &Path) -> bool {
        match std::os::windows::fs::symlink_file(source, destination) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("skipping symlink assertion without Windows symlink privilege: {error}");
                false
            }
            Err(error) => panic!("create file symlink: {error}"),
        }
    }

    /// Delivery invariant: save is the commit boundary, so a fresh audit that
    /// finds an unexplained direct change must refuse before the destination
    /// exists. Returning "review_required" after writing would turn a failed
    /// trust decision into an apparently usable artifact.
    #[tokio::test]
    async fn save_docx_refuses_non_deliverable_audit_before_path_creation() {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("input.docx"), make_multi_para_docx(3))
            .expect("write input");
        let authority = PathAuthority::rooted(workspace.path()).expect("rooted authority");
        let server = StemmaServer::with_config_and_authority(Config::defaults(), authority);
        let opened = server
            .open_docx(Parameters(OpenArgs {
                path: "input.docx".to_string(),
                task: None,
                task_id: None,
            }))
            .await;
        let doc_id = structured(&opened)["doc_id"]
            .as_str()
            .expect("open doc id")
            .to_string();

        let applied = server
            .apply_edit(Parameters(ApplyEditArgs {
                doc_id: doc_id.clone(),
                transaction: replace_txn_arg("p_2", "Paragraph 1", "Paragraph 1 changed directly."),
                mode: Some("direct".to_string()),
                allow_existing_author: false,
            }))
            .await;
        assert_eq!(structured(&applied)["applied"], true, "{applied:?}");

        let saved = server
            .save_docx(Parameters(SaveArgs {
                doc_id,
                path: "must-not-exist.docx".to_string(),
            }))
            .await;
        let payload = structured(&saved);
        assert_eq!(saved.is_error, Some(true), "{payload}");
        assert_eq!(payload["code"], "verification_failed", "{payload}");
        assert_eq!(payload["audit"]["verdict"]["deliverable"], false);
        assert!(
            payload["audit"]["counts"]["unexplained_direct_changes"]
                .as_u64()
                .is_some_and(|count| count > 0),
            "the refusal exposes the blocking finding count: {payload}"
        );
        assert!(
            !workspace.path().join("must-not-exist.docx").exists(),
            "a failed audit must not create a deliverable"
        );
    }

    /// Delivery distinguishes a requested typed resolution from an unexpected
    /// changed prior revision. Both audits observe a resolved pre-existing
    /// revision; only the MCP command is session evidence that the transition
    /// was intended. A blanket "resolved is deliverable" rule would make the
    /// bypass half of this regression fail.
    #[tokio::test]
    async fn save_docx_allows_only_session_evidenced_prior_revision_resolution() {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("input.docx"), filter_selector_docx())
            .expect("write input");
        let authority = PathAuthority::rooted(workspace.path()).expect("rooted authority");
        let server = StemmaServer::with_config_and_authority(Config::defaults(), authority);
        let opened = server
            .open_docx(Parameters(OpenArgs {
                path: "input.docx".to_string(),
                task: None,
                task_id: None,
            }))
            .await;
        let doc_id = structured(&opened)["doc_id"]
            .as_str()
            .expect("open doc id")
            .to_string();
        let selector = ChangeSelector::ByFilter {
            by_author: Some("AuthorB".to_string()),
            by_kind: Some("insert".to_string()),
            by_block_range: Some(BlockRange {
                from_block_id: "p_3".to_string(),
                to_block_id: "p_3".to_string(),
            }),
        };
        let rejected = server
            .reject_changes(Parameters(RejectArgs {
                doc_id: doc_id.clone(),
                selector,
            }))
            .await;
        assert_eq!(rejected.is_error, Some(false), "{}", structured(&rejected));

        let review = server
            .review_session(Parameters(ReviewSessionArgs {
                doc_id: doc_id.clone(),
                render: None,
                detail: None,
                offset: None,
                limit: None,
            }))
            .await;
        let review = structured(&review);
        assert_eq!(review["counts"]["changed_prior_revisions"], 1, "{review}");
        assert_eq!(
            review["counts"]["expected_changed_prior_revisions"], 1,
            "{review}"
        );
        assert_eq!(
            review["counts"]["unexpected_changed_prior_revisions"], 0,
            "{review}"
        );
        assert_eq!(review["verdict"]["deliverable"], true, "{review}");

        let saved = server
            .save_docx(Parameters(SaveArgs {
                doc_id: doc_id.clone(),
                path: "resolved.docx".to_string(),
            }))
            .await;
        assert_eq!(saved.is_error, Some(false), "{}", structured(&saved));
        assert!(workspace.path().join("resolved.docx").is_file());

        let stateless = server
            .audit_docx(Parameters(AuditDocxArgs {
                before_path: "input.docx".to_string(),
                after_path: "resolved.docx".to_string(),
                render: None,
                detail: None,
                offset: None,
                limit: None,
            }))
            .await;
        let stateless = structured(&stateless);
        assert_eq!(
            stateless["counts"]["unexpected_changed_prior_revisions"], 1,
            "producer-neutral audit has no session command evidence: {stateless}"
        );
        assert_eq!(stateless["verdict"]["deliverable"], false, "{stateless}");

        let direct = server
            .apply_edit(Parameters(ApplyEditArgs {
                doc_id: doc_id.clone(),
                transaction: replace_txn_arg("p_3", "End stop.", "End changed directly."),
                mode: Some("direct".to_string()),
                allow_existing_author: false,
            }))
            .await;
        assert_eq!(direct.is_error, Some(false), "{}", structured(&direct));
        let refused_same_block = server
            .save_docx(Parameters(SaveArgs {
                doc_id,
                path: "same-block-direct.docx".to_string(),
            }))
            .await;
        assert_eq!(
            structured(&refused_same_block)["code"],
            "verification_failed",
            "a later direct edit cannot inherit an earlier resolution transition"
        );
        assert!(!workspace.path().join("same-block-direct.docx").exists());

        let bypass_authority =
            PathAuthority::rooted(workspace.path()).expect("second rooted authority");
        let bypass = StemmaServer::with_config_and_authority(Config::defaults(), bypass_authority);
        let opened = bypass
            .open_docx(Parameters(OpenArgs {
                path: "input.docx".to_string(),
                task: None,
                task_id: None,
            }))
            .await;
        let bypass_doc_id = structured(&opened)["doc_id"]
            .as_str()
            .expect("bypass doc id")
            .to_string();
        let ids = bypass
            .resolve_revision_ids(
                &bypass_doc_id,
                ChangeSelector::ByFilter {
                    by_author: Some("AuthorB".to_string()),
                    by_kind: Some("insert".to_string()),
                    by_block_range: Some(BlockRange {
                        from_block_id: "p_3".to_string(),
                        to_block_id: "p_3".to_string(),
                    }),
                },
            )
            .expect("resolve ids");
        bypass
            .runtime
            .resolve_tracked_revisions(
                &DocHandle(bypass_doc_id.clone()),
                &ids,
                ResolveSelectionAction::Reject,
            )
            .expect("engine resolution");

        let refused = bypass
            .save_docx(Parameters(SaveArgs {
                doc_id: bypass_doc_id,
                path: "unexpected-resolution.docx".to_string(),
            }))
            .await;
        let refused = structured(&refused);
        assert_eq!(refused["code"], "verification_failed", "{refused}");
        assert_eq!(
            refused["audit"]["counts"]["unexpected_changed_prior_revisions"], 1,
            "{refused}"
        );
        assert!(!workspace.path().join("unexpected-resolution.docx").exists());
    }

    /// Resolution evidence follows the typed operation's structural footprint,
    /// not only the revision record's own block id. Accepting a deleted
    /// paragraph mark removes its following paragraph; resolving text inside a
    /// cell is reported by the audit at the containing table. Both are expected
    /// effects of the selected revision set and must remain saveable.
    #[tokio::test]
    async fn save_docx_reconciles_paragraph_join_and_table_resolution_effects() {
        let workspace = tempfile::tempdir().expect("workspace");
        let paragraph_join = make_docx(
            concat!(
                r#"<w:p><w:pPr><w:rPr><w:del w:id="1" w:author="Resolver" w:date="2026-01-01T00:00:00Z"/></w:rPr></w:pPr><w:r><w:t>First</w:t></w:r></w:p>"#,
                r#"<w:p><w:r><w:t>Second</w:t></w:r></w:p>"#,
            ),
            false,
        );
        std::fs::write(workspace.path().join("paragraph.docx"), paragraph_join)
            .expect("write paragraph fixture");
        let authority = PathAuthority::rooted(workspace.path()).expect("rooted authority");
        let server = StemmaServer::with_config_and_authority(Config::defaults(), authority);
        let opened = server
            .open_docx(Parameters(OpenArgs {
                path: "paragraph.docx".to_string(),
                task: None,
                task_id: None,
            }))
            .await;
        let doc_id = structured(&opened)["doc_id"]
            .as_str()
            .expect("paragraph doc id")
            .to_string();
        let accepted = server
            .accept_changes(Parameters(AcceptArgs {
                doc_id: doc_id.clone(),
                selector: ChangeSelector::ByAuthor {
                    author: "Resolver".to_string(),
                },
            }))
            .await;
        assert_eq!(accepted.is_error, Some(false), "{}", structured(&accepted));
        let saved = server
            .save_docx(Parameters(SaveArgs {
                doc_id,
                path: "paragraph-resolved.docx".to_string(),
            }))
            .await;
        assert_eq!(saved.is_error, Some(false), "{}", structured(&saved));

        let before_mutation_authority =
            PathAuthority::rooted(workspace.path()).expect("before-mutation rooted authority");
        let before_mutation =
            StemmaServer::with_config_and_authority(Config::defaults(), before_mutation_authority);
        let opened = before_mutation
            .open_docx(Parameters(OpenArgs {
                path: "paragraph.docx".to_string(),
                task: None,
                task_id: None,
            }))
            .await;
        let before_mutation_doc_id = structured(&opened)["doc_id"]
            .as_str()
            .expect("before-mutation doc id")
            .to_string();
        let direct_before = before_mutation
            .apply_edit(Parameters(ApplyEditArgs {
                doc_id: before_mutation_doc_id.clone(),
                transaction: replace_txn_arg("p_2", "Second", "Altered"),
                mode: Some("direct".to_string()),
                allow_existing_author: false,
            }))
            .await;
        assert_eq!(
            direct_before.is_error,
            Some(false),
            "{}",
            structured(&direct_before)
        );
        let accepted = before_mutation
            .accept_changes(Parameters(AcceptArgs {
                doc_id: before_mutation_doc_id.clone(),
                selector: ChangeSelector::ByAuthor {
                    author: "Resolver".to_string(),
                },
            }))
            .await;
        assert_eq!(accepted.is_error, Some(false), "{}", structured(&accepted));
        let refused_before = before_mutation
            .save_docx(Parameters(SaveArgs {
                doc_id: before_mutation_doc_id,
                path: "direct-before-resolution.docx".to_string(),
            }))
            .await;
        assert_eq!(
            structured(&refused_before)["code"],
            "verification_failed",
            "an earlier direct edit cannot be absorbed into a later paragraph-join transition"
        );
        assert!(
            !workspace
                .path()
                .join("direct-before-resolution.docx")
                .exists()
        );

        let table = make_docx(
            concat!(
                r#"<w:tbl><w:tblPr/><w:tr><w:tc><w:tcPr/><w:p>"#,
                r#"<w:r><w:t xml:space="preserve">Price </w:t></w:r>"#,
                r#"<w:del w:id="2" w:author="ResolverA" w:date="2026-01-01T00:00:00Z"><w:r><w:delText>113</w:delText></w:r></w:del>"#,
                r#"<w:ins w:id="3" w:author="ResolverA" w:date="2026-01-01T00:00:00Z"><w:r><w:t>999</w:t></w:r></w:ins>"#,
                r#"<w:r><w:t xml:space="preserve"> term </w:t></w:r>"#,
                r#"<w:del w:id="4" w:author="ResolverB" w:date="2026-01-02T00:00:00Z"><w:r><w:delText>30</w:delText></w:r></w:del>"#,
                r#"<w:ins w:id="5" w:author="ResolverB" w:date="2026-01-02T00:00:00Z"><w:r><w:t>45</w:t></w:r></w:ins>"#,
                r#"</w:p></w:tc></w:tr></w:tbl>"#,
            ),
            false,
        );
        std::fs::write(workspace.path().join("table.docx"), table).expect("write table fixture");
        let authority = PathAuthority::rooted(workspace.path()).expect("second rooted authority");
        let server = StemmaServer::with_config_and_authority(Config::defaults(), authority);
        let opened = server
            .open_docx(Parameters(OpenArgs {
                path: "table.docx".to_string(),
                task: None,
                task_id: None,
            }))
            .await;
        let doc_id = structured(&opened)["doc_id"]
            .as_str()
            .expect("table doc id")
            .to_string();
        for author in ["ResolverA", "ResolverB"] {
            let accepted = server
                .accept_changes(Parameters(AcceptArgs {
                    doc_id: doc_id.clone(),
                    selector: ChangeSelector::ByAuthor {
                        author: author.to_string(),
                    },
                }))
                .await;
            assert_eq!(
                accepted.is_error,
                Some(false),
                "{author}: {}",
                structured(&accepted)
            );
        }
        let review = server
            .review_session(Parameters(ReviewSessionArgs {
                doc_id: doc_id.clone(),
                render: None,
                detail: None,
                offset: None,
                limit: None,
            }))
            .await;
        let review = structured(&review);
        assert_eq!(review["counts"]["changed_prior_revisions"], 4, "{review}");
        assert_eq!(
            review["counts"]["unexpected_changed_prior_revisions"], 0,
            "{review}"
        );
        assert_eq!(
            review["counts"]["unexplained_direct_changes"], 0,
            "{review}"
        );
        assert_eq!(review["verdict"]["deliverable"], true, "{review}");
        let saved = server
            .save_docx(Parameters(SaveArgs {
                doc_id,
                path: "table-resolved.docx".to_string(),
            }))
            .await;
        assert_eq!(saved.is_error, Some(false), "{}", structured(&saved));
    }

    #[tokio::test]
    async fn rooted_mcp_open_refuses_traversal_and_symlink_escape() {
        let world = tempfile::tempdir().expect("world");
        let workspace = world.path().join("workspace");
        std::fs::create_dir(&workspace).expect("workspace");
        let input = workspace.join("input.docx");
        let outside = world.path().join("outside.docx");
        std::fs::write(&input, make_multi_para_docx(3)).expect("write input");
        std::fs::write(&outside, make_multi_para_docx(4)).expect("write outside");

        let authority = PathAuthority::rooted(&workspace).expect("rooted authority");
        let server = StemmaServer::with_config_and_authority(Config::defaults(), authority);

        let traversal = server
            .open_docx(Parameters(OpenArgs {
                path: "../outside.docx".to_string(),
                task: None,
                task_id: None,
            }))
            .await;
        assert_eq!(structured(&traversal)["code"], "artifact_outside_workspace");

        let escaped_link = workspace.join("escaped.docx");
        if create_file_symlink(&outside, &escaped_link) {
            let escaped = server
                .open_docx(Parameters(OpenArgs {
                    path: "escaped.docx".to_string(),
                    task: None,
                    task_id: None,
                }))
                .await;
            assert_eq!(structured(&escaped)["code"], "artifact_outside_workspace");
        }

        let absolute_inside = server
            .open_docx(Parameters(OpenArgs {
                path: input.display().to_string(),
                task: None,
                task_id: None,
            }))
            .await;
        assert_eq!(
            absolute_inside.is_error,
            Some(false),
            "an absolute path inside the configured root is authorized"
        );
        assert_eq!(
            Path::new(
                structured(&absolute_inside)["input_artifact"]["resolved_path"]
                    .as_str()
                    .expect("resolved path")
            ),
            input.canonicalize().unwrap()
        );

        let stream_read = server
            .open_docx(Parameters(OpenArgs {
                path: "input.docx:hidden".to_string(),
                task: None,
                task_id: None,
            }))
            .await;
        assert_eq!(structured(&stream_read)["code"], "artifact_read_failed");

        let original = std::fs::read(&input).unwrap();
        let stream_write = server
            .save_docx(Parameters(SaveArgs {
                doc_id: structured(&absolute_inside)["doc_id"]
                    .as_str()
                    .unwrap()
                    .to_string(),
                path: "input.docx:stemma-output".to_string(),
            }))
            .await;
        assert_eq!(structured(&stream_write)["code"], "artifact_commit_failed");
        assert_eq!(std::fs::read(&input).unwrap(), original);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn mcp_fails_loudly_before_non_utf8_paths_reach_json_receipts() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;

        let world = tempfile::tempdir().expect("world");
        let workspace = world.path().join("workspace");
        std::fs::create_dir(&workspace).expect("workspace");
        let non_utf8_docx = workspace.join(OsString::from_vec(b"input-\xff.docx".to_vec()));
        if !provision_non_utf8(
            std::fs::write(&non_utf8_docx, make_multi_para_docx(3)),
            &non_utf8_docx,
        ) {
            return;
        }
        let alias = workspace.join("input-alias.docx");
        assert!(create_file_symlink(&non_utf8_docx, &alias));

        let authority = PathAuthority::rooted(&workspace).expect("rooted authority");
        let server = StemmaServer::with_config_and_authority(Config::defaults(), authority);
        let alias_result = server
            .open_docx(Parameters(OpenArgs {
                path: "input-alias.docx".to_string(),
                task: None,
                task_id: None,
            }))
            .await;
        assert_eq!(alias_result.is_error, Some(true));
        assert_eq!(
            structured(&alias_result)["code"],
            "artifact_read_failed",
            "a non-UTF8 canonical source path is a typed read refusal, not a JSON panic"
        );
        assert!(
            structured(&alias_result)["error"]
                .as_str()
                .unwrap()
                .contains("not valid UTF-8")
        );

        let non_utf8_workspace = world
            .path()
            .join(OsString::from_vec(b"workspace-\xff".to_vec()));
        std::fs::create_dir(&non_utf8_workspace).expect("non-UTF8 workspace");
        std::fs::write(
            non_utf8_workspace.join("input.docx"),
            make_multi_para_docx(3),
        )
        .expect("write rooted input");
        let startup_error = artifact_authority_from_setting(None, &non_utf8_workspace)
            .expect_err("a rooted MCP cannot start with a non-UTF8 canonical workspace");
        assert!(
            startup_error.contains(ENV_WORKSPACE_ROOT)
                && startup_error.contains("not valid UTF-8")
                && startup_error.contains("serialized receipts"),
            "startup refusal is explicit and actionable: {startup_error}"
        );
    }

    #[tokio::test]
    async fn rooted_mcp_refuses_outside_compare_audit_and_render_paths() {
        let world = tempfile::tempdir().expect("world");
        let workspace = world.path().join("workspace");
        let outside = world.path().join("outside");
        std::fs::create_dir(&workspace).expect("workspace");
        std::fs::create_dir(&outside).expect("outside");
        let input = workspace.join("input.docx");
        let target = workspace.join("target.docx");
        let outside_docx = outside.join("outside.docx");
        std::fs::write(&input, make_multi_para_docx(3)).expect("write input");
        std::fs::write(&target, make_multi_para_docx(4)).expect("write target");
        std::fs::write(&outside_docx, make_multi_para_docx(5)).expect("write outside");

        let authority = PathAuthority::rooted(&workspace).expect("rooted authority");
        let server = StemmaServer::with_config_and_authority(Config::defaults(), authority);
        let opened = server
            .open_docx(Parameters(OpenArgs {
                path: "input.docx".to_string(),
                task: None,
                task_id: None,
            }))
            .await;
        let doc_id = structured(&opened)["doc_id"].as_str().unwrap().to_string();
        let outside_path = outside_docx.display().to_string();

        for (base_path, target_path, out_path) in [
            (
                outside_path.clone(),
                "target.docx".to_string(),
                "compare-outside-base.docx".to_string(),
            ),
            (
                "input.docx".to_string(),
                outside_path.clone(),
                "compare-outside-target.docx".to_string(),
            ),
        ] {
            let denied = server
                .compare_docx(Parameters(CompareArgs {
                    base_path,
                    target_path,
                    out_path: out_path.clone(),
                    author: None,
                }))
                .await;
            assert_eq!(structured(&denied)["code"], "artifact_outside_workspace");
            assert!(!workspace.join(out_path).exists());
        }

        for (before_path, after_path) in [
            (outside_path.clone(), "target.docx".to_string()),
            ("input.docx".to_string(), outside_path.clone()),
        ] {
            let denied = server
                .audit_docx(Parameters(AuditDocxArgs {
                    before_path,
                    after_path,
                    render: None,
                    detail: None,
                    offset: None,
                    limit: None,
                }))
                .await;
            assert_eq!(structured(&denied)["code"], "artifact_outside_workspace");
        }

        let audit_output = outside.join("audit.docx");
        let denied = server
            .audit_docx(Parameters(AuditDocxArgs {
                before_path: "input.docx".to_string(),
                after_path: "target.docx".to_string(),
                render: Some(RenderSpec {
                    path: audit_output.display().to_string(),
                }),
                detail: None,
                offset: None,
                limit: None,
            }))
            .await;
        assert_eq!(structured(&denied)["code"], "artifact_outside_workspace");
        assert!(!audit_output.exists());

        let review_output = outside.join("review.docx");
        let denied = server
            .review_session(Parameters(ReviewSessionArgs {
                doc_id,
                render: Some(RenderSpec {
                    path: review_output.display().to_string(),
                }),
                detail: None,
                offset: None,
                limit: None,
            }))
            .await;
        assert_eq!(structured(&denied)["code"], "artifact_outside_workspace");
        assert!(!review_output.exists());
    }

    #[tokio::test]
    async fn rooted_mcp_refuses_outside_images_before_check_or_commit() {
        let world = tempfile::tempdir().expect("world");
        let workspace = world.path().join("workspace");
        std::fs::create_dir(&workspace).expect("workspace");
        std::fs::write(workspace.join("input.docx"), make_multi_para_docx(3)).expect("write input");
        let outside_image = world.path().join("outside.png");
        std::fs::write(&outside_image, png_100x50()).expect("write outside image");

        let authority = PathAuthority::rooted(&workspace).expect("rooted authority");
        let server = StemmaServer::with_config_and_authority(Config::defaults(), authority);
        let opened = server
            .open_docx(Parameters(OpenArgs {
                path: "input.docx".to_string(),
                task: None,
                task_id: None,
            }))
            .await;
        let doc_id = structured(&opened)["doc_id"].as_str().unwrap().to_string();
        let image_transaction = || {
            TransactionArg(json!({
                "ops": [{
                    "op": "insert_image",
                    "target": "p_1",
                    "path": outside_image.display().to_string(),
                    "format": "png"
                }],
                "revision": { "author": "Imager" }
            }))
        };

        let checked = server
            .check_edit(Parameters(CheckArgs {
                doc_id: doc_id.clone(),
                transaction: image_transaction(),
            }))
            .await;
        assert_eq!(structured(&checked)["code"], "artifact_outside_workspace");

        let applied = server
            .apply_batch(Parameters(BatchArgs {
                doc_id: doc_id.clone(),
                transaction: image_transaction(),
                preview: false,
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        assert_eq!(structured(&applied)["code"], "artifact_outside_workspace");

        let review = server
            .review_session(Parameters(ReviewSessionArgs {
                doc_id,
                render: None,
                detail: None,
                offset: None,
                limit: None,
            }))
            .await;
        assert_eq!(structured(&review)["session"]["census"]["total"], 0);
        assert_eq!(
            structured(&review)["session"]["direct_delta"]["total"],
            0,
            "neither rejected media path may mutate the open document"
        );
    }

    /// The transport, not caller convention, owns filesystem authority and
    /// create-new persistence. Exercise the shared policy through every MCP
    /// writer and verify a refused call leaves the existing bytes untouched.
    #[tokio::test]
    async fn artifact_boundary_is_enforced_across_mcp_surfaces() {
        let workspace = tempfile::tempdir().expect("workspace");
        let outside = tempfile::tempdir().expect("outside workspace");
        let input = workspace.path().join("input.docx");
        let target = workspace.path().join("target.docx");
        std::fs::write(&input, make_multi_para_docx(3)).expect("write input");
        std::fs::write(&target, make_multi_para_docx(4)).expect("write target");

        let authority = PathAuthority::rooted(workspace.path()).expect("rooted authority");
        let server = StemmaServer::with_config_and_authority(Config::defaults(), authority);

        let denied = server
            .open_docx(Parameters(OpenArgs {
                path: outside.path().join("outside.docx").display().to_string(),
                task: None,
                task_id: None,
            }))
            .await;
        assert_eq!(
            structured(&denied)["code"],
            "artifact_outside_workspace",
            "a missing path outside the root is a safe refusal"
        );

        let outside_input = outside.path().join("outside.docx");
        std::fs::write(&outside_input, make_multi_para_docx(3)).expect("write outside input");
        let denied = server
            .open_docx(Parameters(OpenArgs {
                path: outside_input.display().to_string(),
                task: None,
                task_id: None,
            }))
            .await;
        assert_eq!(structured(&denied)["code"], "artifact_outside_workspace");

        let opened = server
            .open_docx(Parameters(OpenArgs {
                path: "input.docx".to_string(),
                task: None,
                task_id: None,
            }))
            .await;
        assert_eq!(
            opened.is_error,
            Some(false),
            "relative rooted read succeeds"
        );
        let open_payload = structured(&opened);
        let doc_id = open_payload["doc_id"].as_str().unwrap().to_string();
        assert_eq!(open_payload["input_artifact"]["role"], "input_docx");
        assert_eq!(
            open_payload["input_artifact"]["digest"]["algorithm"],
            "sha256"
        );

        let hard_link = workspace.path().join("input-hard-link.docx");
        std::fs::hard_link(&input, &hard_link).expect("create source hard link");
        let alias_save = server
            .save_docx(Parameters(SaveArgs {
                doc_id: doc_id.clone(),
                path: "input-hard-link.docx".to_string(),
            }))
            .await;
        assert_eq!(
            structured(&alias_save)["code"],
            "artifact_protected_source",
            "hard-link aliases of a source are protected at the MCP surface"
        );

        let saved = server
            .save_docx(Parameters(SaveArgs {
                doc_id: doc_id.clone(),
                path: "result.docx".to_string(),
            }))
            .await;
        assert_eq!(saved.is_error, Some(false), "fresh output commits");
        let save_payload = structured(&saved);
        assert_eq!(save_payload["verdict"]["status"], "pass");
        assert_eq!(save_payload["verdict"]["deliverable"], true);
        assert_eq!(save_payload["validation"]["level"], "blocking");
        assert_eq!(save_payload["validation"]["ok"], true);
        assert_eq!(save_payload["audit_binding"]["doc_id"], doc_id);
        assert_eq!(
            save_payload["audit_binding"]["set_sha256"]
                .as_str()
                .map(str::len),
            Some(64)
        );
        assert_eq!(save_payload["audit_binding"]["verdict"]["status"], "pass");
        assert_eq!(
            save_payload["output_artifact"]["collision_policy"],
            "create_new"
        );
        assert_eq!(save_payload["output_artifact"]["disposition"], "created");
        let result_path = workspace.path().join("result.docx");
        let committed = std::fs::read(&result_path).expect("committed output");
        assert_eq!(
            save_payload["output_artifact"]["identity"]["bytes"],
            committed.len() as u64
        );
        let reread = server
            .artifacts
            .read_source("result.docx", "verification", None)
            .expect("reread committed bytes");
        assert_eq!(
            save_payload["output_artifact"]["identity"]["digest"]["hex"],
            reread.identity().digest.hex
        );
        assert_eq!(
            save_payload["audit_binding"]["output_sha256"],
            reread.identity().digest.hex
        );

        let compared = server
            .compare_docx(Parameters(CompareArgs {
                base_path: "input.docx".to_string(),
                target_path: "target.docx".to_string(),
                out_path: "compare.docx".to_string(),
                author: None,
            }))
            .await;
        assert_eq!(compared.is_error, Some(false));
        let compare_payload = structured(&compared);
        let compare_reread = server
            .artifacts
            .read_source("compare.docx", "verification", None)
            .unwrap();
        assert_eq!(
            compare_payload["output_artifact"]["identity"]["digest"]["hex"],
            compare_reread.identity().digest.hex
        );
        assert_eq!(
            compare_payload["output_artifact"]["identity"]["role"],
            "output_redline"
        );

        let compact_compared = server
            .execute_plan(Parameters(ExecutePlanArgs {
                doc_id: None,
                transaction: None,
                resolution: None,
                replacement_worklist: None,
                comparison: Some(ComparisonPlanArg {
                    base_path: "input.docx".to_string(),
                    target_path: "target.docx".to_string(),
                    out_path: "compare-core.docx".to_string(),
                    author: Some("Core Comparator".to_string()),
                }),
                preview: false,
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        assert_eq!(compact_compared.is_error, Some(false));
        assert_eq!(
            structured(&compact_compared)["output_artifact"]["identity"]["role"],
            "output_redline",
            "the five-tool producer route preserves compare_docx semantics"
        );

        let audited = server
            .audit_docx(Parameters(AuditDocxArgs {
                before_path: "input.docx".to_string(),
                after_path: "result.docx".to_string(),
                render: Some(RenderSpec {
                    path: "audit.docx".to_string(),
                }),
                detail: None,
                offset: None,
                limit: None,
            }))
            .await;
        assert_eq!(audited.is_error, Some(false));
        let audit_payload = structured(&audited);
        let audit_reread = server
            .artifacts
            .read_source("audit.docx", "verification", None)
            .unwrap();
        assert_eq!(
            audit_payload["render"]["output_artifact"]["identity"]["digest"]["hex"],
            audit_reread.identity().digest.hex
        );

        let reviewed = server
            .review_session(Parameters(ReviewSessionArgs {
                doc_id: doc_id.clone(),
                render: Some(RenderSpec {
                    path: "review.docx".to_string(),
                }),
                detail: None,
                offset: None,
                limit: None,
            }))
            .await;
        assert_eq!(reviewed.is_error, Some(false));
        let review_payload = structured(&reviewed);
        let review_reread = server
            .artifacts
            .read_source("review.docx", "verification", None)
            .unwrap();
        assert_eq!(
            review_payload["render"]["output_artifact"]["identity"]["digest"]["hex"],
            review_reread.identity().digest.hex
        );

        let save_again = server
            .save_docx(Parameters(SaveArgs {
                doc_id: doc_id.clone(),
                path: "result.docx".to_string(),
            }))
            .await;
        assert_eq!(structured(&save_again)["code"], "artifact_output_exists");
        assert_eq!(std::fs::read(&result_path).unwrap(), committed);

        let over_input = server
            .save_docx(Parameters(SaveArgs {
                doc_id: doc_id.clone(),
                path: "input.docx".to_string(),
            }))
            .await;
        assert_eq!(structured(&over_input)["code"], "artifact_protected_source");

        let compare = server
            .compare_docx(Parameters(CompareArgs {
                base_path: "input.docx".to_string(),
                target_path: "target.docx".to_string(),
                out_path: "result.docx".to_string(),
                author: None,
            }))
            .await;
        assert_eq!(structured(&compare)["code"], "artifact_output_exists");

        let audit = server
            .audit_docx(Parameters(AuditDocxArgs {
                before_path: "input.docx".to_string(),
                after_path: "result.docx".to_string(),
                render: Some(RenderSpec {
                    path: "result.docx".to_string(),
                }),
                detail: None,
                offset: None,
                limit: None,
            }))
            .await;
        assert_eq!(structured(&audit)["code"], "artifact_protected_source");

        let review = server
            .review_session(Parameters(ReviewSessionArgs {
                doc_id,
                render: Some(RenderSpec {
                    path: "result.docx".to_string(),
                }),
                detail: None,
                offset: None,
                limit: None,
            }))
            .await;
        assert_eq!(structured(&review)["code"], "artifact_output_exists");
        assert_eq!(std::fs::read(&result_path).unwrap(), committed);

        let outside_output = outside.path().join("new.docx");
        let denied = server
            .compare_docx(Parameters(CompareArgs {
                base_path: "input.docx".to_string(),
                target_path: "target.docx".to_string(),
                out_path: outside_output.display().to_string(),
                author: None,
            }))
            .await;
        assert_eq!(structured(&denied)["code"], "artifact_outside_workspace");
        assert!(!outside_output.exists());
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
    fn tool_profile_defaults_to_core_and_refuses_unknown_values() {
        assert_eq!(
            parse_profile_setting(ENV_PROFILE, None),
            Ok(ToolProfile::Core)
        );
        assert_eq!(
            parse_profile_setting(ENV_PROFILE, Some("core")),
            Ok(ToolProfile::Core)
        );
        assert_eq!(
            parse_profile_setting(ENV_PROFILE, Some("advanced")),
            Ok(ToolProfile::Advanced)
        );
        let error = parse_profile_setting(ENV_PROFILE, Some("full"))
            .expect_err("unknown profiles must fail loud");
        assert!(
            error.contains(ENV_PROFILE) && error.contains("core") && error.contains("advanced")
        );
    }

    #[test]
    fn core_guidance_is_materially_smaller_than_the_advanced_playbook() {
        assert!(CORE_INSTRUCTIONS.contains("open_docx -> inspect_docx"));
        assert!(CORE_INSTRUCTIONS.contains("execute_plan -> save_docx"));
        assert!(CORE_INSTRUCTIONS.contains("patterns=[...]"));
        assert!(CORE_INSTRUCTIONS.contains("not a required call before save_docx"));
        assert!(CORE_INSTRUCTIONS.contains("resolution is not a finalize"));
        assert!(
            CORE_INSTRUCTIONS.len() * 10 < INSTRUCTIONS.len(),
            "core startup guidance should remain at least 10x smaller: core={} advanced={}",
            CORE_INSTRUCTIONS.len(),
            INSTRUCTIONS.len()
        );

        let router = StemmaServer::router_for_profile(ToolProfile::Core);
        let inspect = router.get("inspect_docx").expect("inspect core tool");
        let inspect_description = inspect.description.as_deref().unwrap_or_default();
        for marker in [
            "query='find'",
            "query='window'",
            "query='document'",
            "query='notes'",
            "query='operations'",
            "query='accepted'",
            "query='rejected'",
        ] {
            assert!(
                inspect_description.contains(marker),
                "compact inspection description lost bounded-navigation marker: {marker}"
            );
        }
        let execute = router.get("execute_plan").expect("execute core tool");
        let execute_description = execute.description.as_deref().unwrap_or_default();
        for marker in [
            "opaque_ref",
            "attrs",
            "touched-block-only",
            "Resolution is NOT a finalize step",
            "comparison is the producer path",
            "insert_note/edit_note/delete_note",
        ] {
            assert!(
                execute_description.contains(marker),
                "compact execution description lost load-bearing guidance: {marker}"
            );
        }
    }

    #[test]
    fn workspace_root_setting_is_fail_loud_and_canonical() {
        let world = tempfile::tempdir().expect("world");
        let startup = world.path().join("startup");
        let configured = startup.join("configured");
        std::fs::create_dir(&startup).expect("startup");
        std::fs::create_dir(&configured).expect("configured root");

        let default = artifact_authority_from_setting(None, &startup.join("."))
            .expect("missing setting uses the startup directory");
        assert_eq!(
            default.root(),
            Some(startup.canonicalize().unwrap().as_path()),
            "the default root is canonical"
        );

        let relative_setting = PathBuf::from("configured").join("..").join("configured");
        let relative =
            artifact_authority_from_setting(Some(relative_setting.as_os_str()), &startup)
                .expect("a relative setting resolves from the startup directory");
        assert_eq!(
            relative.root(),
            Some(configured.canonicalize().unwrap().as_path()),
            "relative roots are resolved and canonicalized without changing process cwd"
        );

        let empty = artifact_authority_from_setting(Some(std::ffi::OsStr::new("")), &startup)
            .expect_err("an explicitly empty root must fail");
        assert!(
            empty.contains(ENV_WORKSPACE_ROOT) && empty.contains("empty"),
            "empty-root error is actionable: {empty}"
        );

        let missing =
            artifact_authority_from_setting(Some(std::ffi::OsStr::new("missing")), &startup)
                .expect_err("a missing root must fail");
        assert!(
            missing.contains(ENV_WORKSPACE_ROOT) && missing.contains("missing"),
            "missing-root error names the setting and path: {missing}"
        );

        let file = startup.join("not-a-directory");
        std::fs::write(&file, b"file").expect("write non-directory root");
        let not_directory = artifact_authority_from_setting(Some(file.as_os_str()), &startup)
            .expect_err("a file cannot be the workspace root");
        assert!(
            not_directory.contains(ENV_WORKSPACE_ROOT) && not_directory.contains("not a directory"),
            "non-directory error is actionable: {not_directory}"
        );
    }

    #[test]
    fn humanize_secs_renders_whole_units() {
        assert_eq!(humanize_secs(DEFAULT_DOC_TTL_SECS), "24h");
        assert_eq!(humanize_secs(3600), "1h");
        assert_eq!(humanize_secs(1800), "30m");
        assert_eq!(humanize_secs(45), "45s");
    }

    #[tokio::test]
    async fn source_artifact_registry_is_removed_only_after_runtime_eviction() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &make_multi_para_docx(3)).await;
        let handle = DocHandle(doc_id.clone());

        server.evict_expired_sessions(u64::MAX);
        assert!(server.runtime.contains_handle(&handle));
        assert!(
            server
                .source_artifacts
                .lock()
                .unwrap()
                .contains_key(&doc_id)
        );

        server.evict_expired_sessions(0);
        assert!(!server.runtime.contains_handle(&handle));
        assert!(
            !server
                .source_artifacts
                .lock()
                .unwrap()
                .contains_key(&doc_id)
        );
    }

    #[tokio::test]
    async fn missing_source_registry_fails_closed_for_save_and_review() {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("input.docx"), make_multi_para_docx(3))
            .expect("write input");
        let authority = PathAuthority::rooted(workspace.path()).expect("rooted authority");
        let server = StemmaServer::with_config_and_authority(Config::defaults(), authority);
        let opened = server
            .open_docx(Parameters(OpenArgs {
                path: "input.docx".to_string(),
                task: None,
                task_id: None,
            }))
            .await;
        let doc_id = structured(&opened)["doc_id"].as_str().unwrap().to_string();
        assert!(server.runtime.contains_handle(&DocHandle(doc_id.clone())));
        server.source_artifacts.lock().unwrap().remove(&doc_id);

        let save = server
            .save_docx(Parameters(SaveArgs {
                doc_id: doc_id.clone(),
                path: "saved.docx".to_string(),
            }))
            .await;
        assert_eq!(structured(&save)["code"], "artifact_session_state_missing");
        assert!(!workspace.path().join("saved.docx").exists());

        let review = server
            .review_session(Parameters(ReviewSessionArgs {
                doc_id,
                render: Some(RenderSpec {
                    path: "review.docx".to_string(),
                }),
                detail: None,
                offset: None,
                limit: None,
            }))
            .await;
        assert_eq!(
            structured(&review)["code"],
            "artifact_session_state_missing"
        );
        assert!(!workspace.path().join("review.docx").exists());
    }

    #[tokio::test]
    async fn missing_source_registry_refuses_resolution_before_mutation() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &filter_selector_docx()).await;
        let before = server
            .resolve_revision_ids(
                &doc_id,
                ChangeSelector::ByAuthor {
                    author: "AuthorB".to_string(),
                },
            )
            .expect("AuthorB revisions before refused command");
        assert!(!before.is_empty());
        server.source_artifacts.lock().unwrap().remove(&doc_id);

        let rejected = server
            .reject_changes(Parameters(RejectArgs {
                doc_id: doc_id.clone(),
                selector: ChangeSelector::ByAuthor {
                    author: "AuthorB".to_string(),
                },
            }))
            .await;
        assert_eq!(
            structured(&rejected)["code"],
            "artifact_session_state_missing"
        );
        let after = server
            .resolve_revision_ids(
                &doc_id,
                ChangeSelector::ByAuthor {
                    author: "AuthorB".to_string(),
                },
            )
            .expect("AuthorB revisions after refused command");
        assert_eq!(
            after, before,
            "failure to establish resolution evidence must leave the runtime unchanged"
        );
    }

    #[tokio::test]
    async fn missing_source_registry_refuses_path_backed_image_before_mutation() {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("input.docx"), make_multi_para_docx(3))
            .expect("write input");
        std::fs::write(workspace.path().join("image.png"), png_100x50()).expect("write image");
        let authority = PathAuthority::rooted(workspace.path()).expect("rooted authority");
        let server = StemmaServer::with_config_and_authority(Config::defaults(), authority);
        let opened = server
            .open_docx(Parameters(OpenArgs {
                path: "input.docx".to_string(),
                task: None,
                task_id: None,
            }))
            .await;
        let doc_id = structured(&opened)["doc_id"].as_str().unwrap().to_string();
        let handle = DocHandle(doc_id.clone());
        let before = server.runtime.view(&handle).unwrap().fingerprint;
        server.source_artifacts.lock().unwrap().remove(&doc_id);

        let result = server
            .apply_edit(Parameters(ApplyEditArgs {
                doc_id,
                transaction: TransactionArg(json!({
                    "ops": [{
                        "op": "insert_image",
                        "target": "p_1",
                        "path": "image.png",
                        "format": "png"
                    }],
                    "revision": { "author": "Imager" }
                })),
                mode: None,
                allow_existing_author: false,
            }))
            .await;

        assert_eq!(
            structured(&result)["code"],
            "artifact_session_state_missing"
        );
        assert_eq!(server.runtime.view(&handle).unwrap().fingerprint, before);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn save_never_opens_a_protected_source_replaced_by_a_fifo() {
        let workspace = tempfile::tempdir().expect("workspace");
        let input = workspace.path().join("input.docx");
        let existing = workspace.path().join("existing.docx");
        std::fs::write(&input, make_multi_para_docx(3)).expect("write input");
        std::fs::write(&existing, b"keep me").expect("write existing output");
        let authority = PathAuthority::rooted(workspace.path()).expect("rooted authority");
        let server = StemmaServer::with_config_and_authority(Config::defaults(), authority);
        let opened = server
            .open_docx(Parameters(OpenArgs {
                path: "input.docx".to_string(),
                task: None,
                task_id: None,
            }))
            .await;
        let doc_id = structured(&opened)["doc_id"].as_str().unwrap().to_string();
        std::fs::remove_file(&input).unwrap();
        let status = std::process::Command::new("mkfifo")
            .arg(&input)
            .status()
            .expect("run mkfifo");
        assert!(status.success(), "mkfifo failed with {status}");

        let result = server
            .save_docx(Parameters(SaveArgs {
                doc_id,
                path: "existing.docx".to_string(),
            }))
            .await;

        assert_eq!(structured(&result)["code"], "artifact_commit_failed");
        assert_eq!(std::fs::read(existing).unwrap(), b"keep me");
    }

    /// open_docx refuses a file larger than the configured cap, and the error
    /// names the size, the limit, and the env var to raise — checked on the
    /// file's metadata, before the bytes are read into memory.
    #[tokio::test]
    async fn open_docx_rejects_a_file_over_the_size_cap() {
        let server = StemmaServer::with_config(Config {
            profile: ToolProfile::Core,
            doc_ttl_secs: DEFAULT_DOC_TTL_SECS,
            max_doc_bytes: 100,
            max_image_bytes: DEFAULT_MAX_IMAGE_BYTES,
            max_image_total_bytes: DEFAULT_MAX_IMAGE_TOTAL_BYTES,
        });
        let docx = make_multi_para_docx(3);
        assert!(docx.len() as u64 > 100, "fixture must exceed the tiny cap");
        let path = write_temp_docx(&docx);
        let result = server
            .open_docx(Parameters(OpenArgs {
                path: path.clone(),
                task: None,
                task_id: None,
            }))
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
            profile: ToolProfile::Core,
            doc_ttl_secs: DEFAULT_DOC_TTL_SECS,
            max_doc_bytes: 0,
            max_image_bytes: DEFAULT_MAX_IMAGE_BYTES,
            max_image_total_bytes: DEFAULT_MAX_IMAGE_TOTAL_BYTES,
        });
        let path = write_temp_docx(&make_multi_para_docx(3));
        let result = server
            .open_docx(Parameters(OpenArgs {
                path: path.clone(),
                task: None,
                task_id: None,
            }))
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

    #[tokio::test]
    async fn compact_surface_inspects_executes_and_verifies_through_shared_kernels() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &make_multi_para_docx(3)).await;

        let document = server
            .inspect_docx(Parameters(InspectDocxArgs {
                doc_id: doc_id.clone(),
                query: InspectQuery::Document,
                block_id: None,
                detail: None,
                pattern: None,
                patterns: None,
                filter: None,
                from_block_id: None,
                to_block_id: None,
                format: None,
                offset: None,
                limit: None,
                cell_offset: None,
                cell_limit: None,
            }))
            .await;
        assert_eq!(document.is_error, Some(false));
        assert!(
            structured(&document)["content"]
                .as_str()
                .unwrap()
                .contains("Paragraph 0")
        );

        let block = server
            .inspect_docx(Parameters(InspectDocxArgs {
                doc_id: doc_id.clone(),
                query: InspectQuery::Block,
                block_id: Some("p_1".to_string()),
                detail: None,
                pattern: None,
                patterns: None,
                filter: None,
                from_block_id: None,
                to_block_id: None,
                format: None,
                offset: None,
                limit: None,
                cell_offset: None,
                cell_limit: None,
            }))
            .await;
        assert_eq!(block.is_error, Some(false));
        assert_eq!(structured(&block)["id"], "p_1");

        let transaction = replace_txn_arg("p_1", "Paragraph 0", "Revised paragraph");
        let preview = server
            .execute_plan(Parameters(ExecutePlanArgs {
                doc_id: Some(doc_id.clone()),
                transaction: Some(transaction.clone()),
                resolution: None,
                replacement_worklist: None,
                comparison: None,
                preview: true,
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        assert_eq!(preview.is_error, Some(false));
        assert_eq!(structured(&preview)["applied"], false);
        assert_eq!(structured(&preview)["would_apply"], true);
        assert_eq!(structured(&preview)["changed_block_ids"], json!(["p_1"]));
        assert_eq!(
            structured(&preview)["changed_blocks"]
                .as_array()
                .expect("preview changed blocks")
                .len(),
            1
        );
        assert!(structured(&preview).get("preview_outline").is_none());

        let applied = server
            .execute_plan(Parameters(ExecutePlanArgs {
                doc_id: Some(doc_id.clone()),
                transaction: Some(transaction),
                resolution: None,
                replacement_worklist: None,
                comparison: None,
                preview: false,
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        assert_eq!(applied.is_error, Some(false));
        assert_eq!(structured(&applied)["applied"], true);
        let applied_revision_count = structured(&applied)["revision_ids"]
            .as_array()
            .expect("execute receipt revision ids")
            .len();

        let resolution_preview = server
            .execute_plan(Parameters(ExecutePlanArgs {
                doc_id: Some(doc_id.clone()),
                transaction: None,
                resolution: Some(ResolutionPlanArg {
                    action: ResolutionActionArg::Accept,
                    selector: ChangeSelector::All,
                }),
                replacement_worklist: None,
                comparison: None,
                preview: true,
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        assert_eq!(resolution_preview.is_error, Some(false));
        assert_eq!(structured(&resolution_preview)["would_apply"], true);
        assert_eq!(
            structured(&resolution_preview)["resolution"]["revision_ids"]
                .as_array()
                .map(Vec::len),
            Some(applied_revision_count)
        );

        let verified = server
            .verify_docx(Parameters(VerifyDocxArgs {
                doc_id: Some(doc_id),
                before_path: None,
                after_path: None,
                render: None,
                detail: None,
                offset: None,
                limit: None,
            }))
            .await;
        assert_eq!(verified.is_error, Some(false));
        assert_eq!(structured(&verified)["validator"]["ok"], true);
        assert!(
            structured(&verified)["session"]["census"]["total"]
                .as_u64()
                .is_some_and(|total| total > 0)
        );
    }

    #[tokio::test]
    async fn compact_execute_plan_runs_a_server_side_replacement_worklist() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &make_multi_para_docx(3)).await;
        let item = |old: &str, new: &str| CoreReplacementItem {
            effect_id: None,
            old: old.to_string(),
            new: new.to_string(),
            scope: None,
            expected_matches: Some(1),
            replace_all: false,
            match_mode: default_core_replacement_match_mode(),
            on_barrier_match: default_core_barrier_policy(),
        };

        let preview = server
            .execute_plan(Parameters(ExecutePlanArgs {
                doc_id: Some(doc_id.clone()),
                transaction: None,
                resolution: None,
                replacement_worklist: Some(ReplacementWorklistArg {
                    author: "Worklist Test".to_string(),
                    replacements: vec![
                        item("Paragraph 0", "First replacement"),
                        item("Paragraph 2", "Second replacement"),
                    ],
                }),
                comparison: None,
                preview: true,
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        let preview_payload = structured(&preview);
        assert_eq!(preview.is_error, Some(false), "preview: {preview_payload}");
        assert_eq!(preview_payload["applied"], 0);
        assert_eq!(preview_payload["would_apply"], 2);
        assert_eq!(preview_payload["items"][0]["status"], "would_apply");
        assert_eq!(preview_payload["items"][1]["status"], "would_apply");
        let absent = structured(
            &server
                .find(Parameters(FindArgs {
                    doc_id: doc_id.clone(),
                    pattern: "First replacement".to_string(),
                    offset: None,
                    limit: None,
                    cell_offset: None,
                    cell_limit: None,
                }))
                .await,
        );
        assert_eq!(absent["count"], 0, "preview must not persist: {absent}");

        let result = server
            .execute_plan(Parameters(ExecutePlanArgs {
                doc_id: Some(doc_id.clone()),
                transaction: None,
                resolution: None,
                replacement_worklist: Some(ReplacementWorklistArg {
                    author: "Worklist Test".to_string(),
                    replacements: vec![
                        item("Paragraph 0", "First replacement"),
                        item("Paragraph 2", "Second replacement"),
                    ],
                }),
                comparison: None,
                preview: false,
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        let payload = structured(&result);
        assert_eq!(result.is_error, Some(false), "worklist result: {payload}");
        assert_eq!(payload["applied"], 2);
        assert_eq!(payload["failed"], 0);

        let first = structured(
            &server
                .find(Parameters(FindArgs {
                    doc_id,
                    pattern: "First replacement".to_string(),
                    offset: None,
                    limit: None,
                    cell_offset: None,
                    cell_limit: None,
                }))
                .await,
        );
        assert_eq!(first["count"], 1);
    }

    #[tokio::test]
    async fn compact_worklist_layers_over_a_prior_authors_pending_insertion() {
        let server = StemmaServer::new();
        let bytes = make_docx(
            r#"<w:p><w:r><w:t xml:space="preserve">Interest: </w:t></w:r><w:ins w:id="5" w:author="Prior Counsel" w:date="2020-01-01T00:00:00Z"><w:r><w:t>rate of 8% above base rate</w:t></w:r></w:ins></w:p>"#,
            false,
        );
        let doc_id = open_and_id(&server, &bytes).await;
        let worklist = || ReplacementWorklistArg {
            author: "Reviewing Counsel".to_string(),
            replacements: vec![CoreReplacementItem {
                effect_id: None,
                old: "rate of 8% above base rate".to_string(),
                new: "rate of 2% above base rate".to_string(),
                scope: None,
                expected_matches: Some(1),
                replace_all: false,
                match_mode: default_core_replacement_match_mode(),
                on_barrier_match: CoreBarrierPolicy::Fail,
            }],
        };

        let preview = server
            .execute_plan(Parameters(ExecutePlanArgs {
                doc_id: Some(doc_id.clone()),
                transaction: None,
                resolution: None,
                replacement_worklist: Some(worklist()),
                comparison: None,
                preview: true,
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        let preview_payload = structured(&preview);
        assert_eq!(preview.is_error, Some(false), "{preview_payload}");
        assert_eq!(preview_payload["would_apply"], 1);
        assert_eq!(preview_payload["items"][0]["match_count"], 1);

        let applied = server
            .execute_plan(Parameters(ExecutePlanArgs {
                doc_id: Some(doc_id.clone()),
                transaction: None,
                resolution: None,
                replacement_worklist: Some(worklist()),
                comparison: None,
                preview: false,
                mode: None,
                allow_existing_author: false,
            }))
            .await;
        let applied_payload = structured(&applied);
        assert_eq!(applied.is_error, Some(false), "{applied_payload}");
        assert_eq!(applied_payload["applied"], 1);

        let accepted = structured(
            &server
                .read_accepted(Parameters(ReadArgs {
                    doc_id: doc_id.clone(),
                }))
                .await,
        );
        assert!(
            accepted["markdown"]
                .as_str()
                .is_some_and(|text| text.contains("rate of 2% above base rate")),
            "accept-all projection contains the layered replacement: {accepted}"
        );

        let rejected = server
            .reject_changes(Parameters(RejectArgs {
                doc_id: doc_id.clone(),
                selector: ChangeSelector::ByAuthor {
                    author: "Reviewing Counsel".to_string(),
                },
            }))
            .await;
        assert_eq!(rejected.is_error, Some(false), "{}", structured(&rejected));
        let restored = structured(
            &server
                .read_accepted(Parameters(ReadArgs {
                    doc_id: doc_id.clone(),
                }))
                .await,
        );
        assert!(
            restored["markdown"]
                .as_str()
                .is_some_and(|text| text.contains("rate of 8% above base rate")),
            "reject restores prior insertion: {restored}"
        );
        let canonical = server
            .runtime
            .with(&DocHandle(doc_id), |snapshot| {
                Arc::clone(&snapshot.canonical)
            })
            .unwrap();
        assert!(
            revision_rows(&canonical)
                .iter()
                .any(|revision| revision.author.as_deref() == Some("Prior Counsel")),
            "rejecting the layered edit preserves the prior author's insertion"
        );
    }

    #[tokio::test]
    async fn compact_inspection_defaults_to_index_and_supports_find_and_window() {
        let default_args: InspectDocxArgs =
            serde_json::from_value(json!({"doc_id": "doc_1"})).expect("default inspect args");
        assert!(matches!(default_args.query, InspectQuery::Index));

        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &make_multi_para_docx(4)).await;
        let index = server
            .inspect_docx(Parameters(InspectDocxArgs {
                doc_id: doc_id.clone(),
                query: InspectQuery::Index,
                block_id: None,
                detail: None,
                pattern: None,
                patterns: None,
                filter: None,
                from_block_id: None,
                to_block_id: None,
                format: None,
                offset: None,
                limit: None,
                cell_offset: None,
                cell_limit: None,
            }))
            .await;
        assert_eq!(structured(&index)["total_blocks"], 4);
        assert!(structured(&index).get("markdown").is_none());

        let found = server
            .inspect_docx(Parameters(InspectDocxArgs {
                doc_id: doc_id.clone(),
                query: InspectQuery::Find,
                block_id: None,
                detail: None,
                pattern: Some("Paragraph 2".to_string()),
                patterns: None,
                filter: None,
                from_block_id: None,
                to_block_id: None,
                format: None,
                offset: None,
                limit: None,
                cell_offset: None,
                cell_limit: None,
            }))
            .await;
        assert_eq!(structured(&found)["count"], 1);
        assert_eq!(structured(&found)["matches"][0]["id"], "p_3");

        let window = server
            .inspect_docx(Parameters(InspectDocxArgs {
                doc_id,
                query: InspectQuery::Window,
                block_id: None,
                detail: None,
                pattern: None,
                patterns: None,
                filter: None,
                from_block_id: Some("p_2".to_string()),
                to_block_id: Some("p_3".to_string()),
                format: Some("markdown".to_string()),
                offset: None,
                limit: None,
                cell_offset: None,
                cell_limit: None,
            }))
            .await;
        let window_payload = structured(&window);
        let content = window_payload["content"].as_str().expect("window content");
        assert!(content.contains("Paragraph 1"));
        assert!(content.contains("Paragraph 2"));
        assert!(!content.contains("Paragraph 0"));
        assert!(!content.contains("Paragraph 3"));
    }

    #[tokio::test]
    async fn compact_batch_find_preserves_pattern_order_duplicates_and_zero_outcomes() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &make_multi_para_docx(20)).await;
        let args: InspectDocxArgs = serde_json::from_value(json!({
            "doc_id": doc_id.clone(),
            "query": "find",
            "patterns": ["Paragraph 2", "absent phrase", "Paragraph 2"],
            "limit": 16
        }))
        .expect("patterns[] is an additive typed find input");

        let result = server.inspect_docx(Parameters(args)).await;
        let payload = structured(&result);
        assert_eq!(result.is_error, Some(false), "{payload}");
        assert_eq!(payload["pattern_count"], 3);
        assert_eq!(payload["outcomes"].as_array().unwrap().len(), 3);
        assert_eq!(payload["outcomes"][0]["pattern_index"], 0);
        assert_eq!(payload["outcomes"][0]["result"]["pattern"], "Paragraph 2");
        assert_eq!(payload["outcomes"][0]["result"]["count"], 1);
        assert_eq!(payload["outcomes"][0]["result"]["matches"][0]["id"], "p_3");
        assert_eq!(payload["outcomes"][1]["pattern_index"], 1);
        assert_eq!(payload["outcomes"][1]["result"]["pattern"], "absent phrase");
        assert_eq!(payload["outcomes"][1]["result"]["count"], 0);
        assert_eq!(payload["outcomes"][2]["pattern_index"], 2);
        assert_eq!(
            payload["outcomes"][2]["result"], payload["outcomes"][0]["result"],
            "duplicate patterns retain independent ordered outcomes with singular-find semantics"
        );
        let singular_args: InspectDocxArgs = serde_json::from_value(json!({
            "doc_id": doc_id,
            "query": "find",
            "pattern": "Paragraph 2",
            "limit": 16
        }))
        .unwrap();
        let singular = server.inspect_docx(Parameters(singular_args)).await;
        let mut singular_payload = structured(&singular);
        singular_payload
            .as_object_mut()
            .unwrap()
            .remove("server_version");
        assert_eq!(
            payload["outcomes"][0]["result"], singular_payload,
            "batch result pages are exactly the singular find contract"
        );
        assert_eq!(payload["limits"]["max_patterns"], 8);
        assert_eq!(payload["limits"]["max_matches_per_pattern"], 16);
        assert_eq!(payload["limits"]["max_matching_cells_per_table"], 4);
        assert!(
            serde_json::to_vec(&payload).unwrap().len() <= 256 * 1024,
            "a successful batch response stays under the advertised byte cap"
        );
    }

    #[tokio::test]
    async fn compact_batch_find_pages_each_pattern_with_exact_totals() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &make_multi_para_docx(20)).await;
        let first_args: InspectDocxArgs = serde_json::from_value(json!({
            "doc_id": doc_id.clone(),
            "query": "find",
            "patterns": ["Paragraph"],
            "limit": 16
        }))
        .unwrap();
        let first = structured(&server.inspect_docx(Parameters(first_args)).await);
        let first_result = &first["outcomes"][0]["result"];
        assert_eq!(first_result["count"], 20);
        assert_eq!(first_result["returned"], 16);
        assert_eq!(first_result["has_more"], true);
        assert_eq!(first_result["next_offset"], 16);
        assert_eq!(first_result["matches"][0]["id"], "p_1");
        assert_eq!(first_result["matches"][15]["id"], "p_16");

        let second_args: InspectDocxArgs = serde_json::from_value(json!({
            "doc_id": doc_id,
            "query": "find",
            "patterns": ["Paragraph"],
            "offset": 16,
            "limit": 16
        }))
        .unwrap();
        let second = structured(&server.inspect_docx(Parameters(second_args)).await);
        let second_result = &second["outcomes"][0]["result"];
        assert_eq!(second_result["count"], 20);
        assert_eq!(second_result["returned"], 4);
        assert_eq!(second_result["has_more"], false);
        assert_eq!(second_result["next_offset"], Value::Null);
        assert_eq!(second_result["matches"][0]["id"], "p_17");
        assert_eq!(second_result["matches"][3]["id"], "p_20");
    }

    #[tokio::test]
    async fn compact_batch_find_rejects_ambiguous_empty_and_unbounded_inputs() {
        let server = StemmaServer::new();
        let doc_id = open_and_id(&server, &make_multi_para_docx(2)).await;
        let cases = [
            (
                json!({
                    "doc_id": doc_id,
                    "query": "find",
                    "pattern": "Paragraph",
                    "patterns": ["Paragraph"]
                }),
                "exactly one of pattern or patterns",
            ),
            (
                json!({"doc_id": doc_id, "query": "find", "patterns": []}),
                "1 to 8 entries",
            ),
            (
                json!({"doc_id": doc_id, "query": "find", "patterns": ["Paragraph", " "]}),
                "patterns[1]",
            ),
            (
                json!({
                    "doc_id": doc_id,
                    "query": "find",
                    "patterns": ["1", "2", "3", "4", "5", "6", "7", "8", "9"]
                }),
                "1 to 8 entries",
            ),
            (
                json!({
                    "doc_id": doc_id,
                    "query": "find",
                    "patterns": ["Paragraph"],
                    "limit": 17
                }),
                "batch find limit must be between 1 and 16",
            ),
            (
                json!({
                    "doc_id": doc_id,
                    "query": "find",
                    "patterns": ["Paragraph"],
                    "cell_limit": 5
                }),
                "batch find cell_limit must be between 1 and 4",
            ),
        ];

        for (value, expected) in cases {
            let args: InspectDocxArgs = serde_json::from_value(value).unwrap();
            let result = server.inspect_docx(Parameters(args)).await;
            let payload = structured(&result);
            assert_eq!(result.is_error, Some(true), "{payload}");
            assert!(
                payload["error"]
                    .as_str()
                    .is_some_and(|error| error.contains(expected)),
                "expected '{expected}' in {payload}"
            );
        }
    }

    #[test]
    fn batch_find_response_cap_fails_loud() {
        let oversized = json!({
            "outcomes": ["x".repeat(MAX_BATCH_FIND_RESPONSE_BYTES)]
        });
        let actual = batch_find_response_bytes(&oversized)
            .expect_err("oversized batch response must be refused");
        assert!(actual > MAX_BATCH_FIND_RESPONSE_BYTES);
        assert_eq!(
            batch_find_response_bytes(&json!({"outcomes": []})),
            Ok(r#"{"outcomes":[]}"#.len())
        );
    }

    #[tokio::test]
    async fn compact_surface_refuses_ambiguous_query_and_verify_modes() {
        let server = StemmaServer::new();
        let bad_query = server
            .inspect_docx(Parameters(InspectDocxArgs {
                doc_id: "doc_unused".to_string(),
                query: InspectQuery::Document,
                block_id: Some("p_1".to_string()),
                detail: None,
                pattern: None,
                patterns: None,
                filter: None,
                from_block_id: None,
                to_block_id: None,
                format: None,
                offset: None,
                limit: None,
                cell_offset: None,
                cell_limit: None,
            }))
            .await;
        assert_eq!(structured(&bad_query)["code"], "invalid_argument");

        let bad_verify = server
            .verify_docx(Parameters(VerifyDocxArgs {
                doc_id: Some("doc_1".to_string()),
                before_path: Some("before.docx".to_string()),
                after_path: Some("after.docx".to_string()),
                render: None,
                detail: None,
                offset: None,
                limit: None,
            }))
            .await;
        assert_eq!(structured(&bad_verify)["code"], "invalid_argument");
    }
}

/// Public-contract guards: the tool reference and crate README must stay in
/// sync with the actual registered surface, and task-delivery tests drive the
/// same private wire argument structs used by the router.
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
mod public_contract_tests {
    use super::tests::{make_docx, structured};
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
        "inspect_docx",
        "execute_plan",
        "verify_docx",
    ];

    const MCP_MD: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../docs/reference/mcp.md"
    ));
    const MCP_ADVANCED_MD: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../docs/reference/mcp-advanced.md"
    ));
    const MCP_README: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md"));

    /// The five core-profile tools, documented in the core reference; the
    /// remaining registered tools are documented in the advanced reference.
    const CORE_TOOLS: &[&str] = &[
        "open_docx",
        "inspect_docx",
        "execute_plan",
        "verify_docx",
        "save_docx",
    ];

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
            "inspect_docx" => d!(InspectDocxArgs),
            "execute_plan" => d!(ExecutePlanArgs),
            "verify_docx" => d!(VerifyDocxArgs),
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

    /// The registered tool surface is exactly `EXPECTED_TOOLS` — no more, no
    /// fewer. This is what forces the canonical list (and thus the docs checks
    /// below) to track reality when a tool is added or renamed.
    #[test]
    fn registered_tool_set_matches_canonical_list() {
        let core = StemmaServer::new();
        let core_actual: BTreeSet<String> = core
            .tool_router
            .list_all()
            .iter()
            .map(|t| t.name.to_string())
            .collect();
        let core_expected: BTreeSet<String> =
            CORE_TOOLS.iter().map(|name| (*name).to_string()).collect();
        assert_eq!(
            core_actual, core_expected,
            "default core profile must stay compact"
        );

        let advanced = StemmaServer::with_config(Config {
            profile: ToolProfile::Advanced,
            ..Config::defaults()
        });
        let actual: BTreeSet<String> = advanced
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
        assert_eq!(actual.len(), 31, "expected exactly 31 registered tools");
    }

    /// Every registered tool is documented — the five core tools in the core
    /// reference, the complete surface in the advanced reference — and the
    /// README's stated count matches. Loud, specific failures name the tool.
    #[test]
    fn every_tool_documented_and_counts_agree() {
        for name in CORE_TOOLS {
            assert!(
                MCP_MD.contains(&format!("`{name}")),
                "core tool `{name}` is registered but absent from docs/reference/mcp.md"
            );
        }
        for name in EXPECTED_TOOLS {
            let needle = format!("`{name}");
            assert!(
                MCP_MD.contains(&needle) || MCP_ADVANCED_MD.contains(&needle),
                "tool `{name}` is registered but absent from both MCP references"
            );
            assert!(
                MCP_README.contains(&needle),
                "tool `{name}` is registered but absent from stemma-mcp/README.md"
            );
        }
        assert!(
            MCP_README.contains("5 tools") && MCP_README.contains("31 tools"),
            "stemma-mcp/README.md must state both core and advanced tool counts"
        );
    }

    /// Every fully-literal tool-call example in the MCP references still deserializes against
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
        let mut examples = tool_call_examples(MCP_MD, &tools);
        examples.extend(tool_call_examples(MCP_ADVANCED_MD, &tools));

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
                    "MCP reference `{name}` example is not valid JSON after placeholder \
                     substitution: {e}\n---\n{raw}\n---"
                )
            });
            if let Err(e) = check_args(&name, value) {
                panic!(
                    "MCP reference `{name}` example no longer deserializes against its \
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
            "expected to verify several literal MCP reference examples, only checked {checked}"
        );
        for must in [
            "compare_docx",
            "accept_changes",
            "save_docx",
            "replace_text",
        ] {
            assert!(
                names_seen.contains(must),
                "expected a literal `{must}` example in the MCP references to be verified"
            );
        }
    }

    fn declared_replacement(
        effect_id: &str,
        find: &str,
        replace: &str,
    ) -> task_delivery::TaskEffectArg {
        task_delivery::TaskEffectArg {
            effect_id: effect_id.to_string(),
            op: task_delivery::TaskEffectOperationArg::ReplaceText,
            find: find.to_string(),
            replace: replace.to_string(),
            match_mode: task_delivery::TaskMatchModeArg::Exact,
            scope: task_delivery::TaskEffectScopeArg::default(),
            expected_matches: 1,
            on_barrier_match: task_delivery::TaskBarrierPolicyArg::Skip,
        }
    }

    fn task_worklist_item(effect_id: &str, old: &str, new: &str) -> CoreReplacementItem {
        CoreReplacementItem {
            effect_id: Some(effect_id.to_string()),
            old: old.to_string(),
            new: new.to_string(),
            scope: None,
            expected_matches: Some(1),
            replace_all: false,
            match_mode: CoreReplacementMatchMode::Exact,
            on_barrier_match: CoreBarrierPolicy::Skip,
        }
    }

    async fn execute_task_item(
        server: &StemmaServer,
        doc_id: &str,
        effect_id: &str,
        old: &str,
        new: &str,
    ) -> CallToolResult {
        server
            .execute_plan(Parameters(ExecutePlanArgs {
                doc_id: Some(doc_id.to_string()),
                transaction: None,
                resolution: None,
                replacement_worklist: Some(ReplacementWorklistArg {
                    author: "Task Agent".to_string(),
                    replacements: vec![task_worklist_item(effect_id, old, new)],
                }),
                comparison: None,
                preview: false,
                mode: None,
                allow_existing_author: false,
            }))
            .await
    }

    #[tokio::test]
    async fn task_last_save_writes_partial_manifest_and_names_unexecuted_effect() {
        let temp = tempfile::tempdir().expect("task tempdir");
        let first_path = temp.path().join("first.docx");
        let second_path = temp.path().join("second.docx");
        let first_output = temp.path().join("out-first.docx");
        let second_output = temp.path().join("out-second.docx");
        let manifest_path = temp.path().join("task.json");
        std::fs::write(
            &first_path,
            make_docx("<w:p><w:r><w:t>Alpha old</w:t></w:r></w:p>", false),
        )
        .expect("first target");
        std::fs::write(
            &second_path,
            make_docx("<w:p><w:r><w:t>Beta old</w:t></w:r></w:p>", false),
        )
        .expect("second target");

        let server = StemmaServer::new();
        let first_open = server
            .open_docx(Parameters(OpenArgs {
                path: first_path.to_string_lossy().into_owned(),
                task: Some(TaskDeclarationArg {
                    task_id: "task-partial".to_string(),
                    manifest_path: manifest_path.to_string_lossy().into_owned(),
                    inputs: vec![],
                    targets: vec![
                        task_delivery::TaskTargetArg {
                            path: first_path.to_string_lossy().into_owned(),
                            effects: vec![declared_replacement(
                                "e-alpha",
                                "Alpha old",
                                "Alpha new",
                            )],
                        },
                        task_delivery::TaskTargetArg {
                            path: second_path.to_string_lossy().into_owned(),
                            effects: vec![
                                declared_replacement("e-beta-missing", "Beta absent", "Beta new"),
                                declared_replacement("e-beta-never", "Beta old", "Beta final"),
                            ],
                        },
                    ],
                }),
                task_id: None,
            }))
            .await;
        assert_eq!(
            first_open.is_error,
            Some(false),
            "{}",
            structured(&first_open)
        );
        let first_doc_id = structured(&first_open)["doc_id"]
            .as_str()
            .expect("first task doc_id")
            .to_string();
        let applied =
            execute_task_item(&server, &first_doc_id, "e-alpha", "Alpha old", "Alpha new").await;
        assert_eq!(applied.is_error, Some(false), "{}", structured(&applied));
        let first_save = server
            .save_docx(Parameters(SaveArgs {
                doc_id: first_doc_id,
                path: first_output.to_string_lossy().into_owned(),
            }))
            .await;
        assert_eq!(
            first_save.is_error,
            Some(false),
            "{}",
            structured(&first_save)
        );
        assert_eq!(structured(&first_save)["task"]["status"], "executing");
        assert_eq!(structured(&first_save)["verdict"]["deliverable"], false);
        assert!(
            !manifest_path.exists(),
            "non-final save must not write manifest"
        );

        let second_open = server
            .open_docx(Parameters(OpenArgs {
                path: second_path.to_string_lossy().into_owned(),
                task: None,
                task_id: Some("task-partial".to_string()),
            }))
            .await;
        assert_eq!(
            second_open.is_error,
            Some(false),
            "{}",
            structured(&second_open)
        );
        let second_doc_id = structured(&second_open)["doc_id"]
            .as_str()
            .expect("second task doc_id")
            .to_string();
        let unsatisfiable = execute_task_item(
            &server,
            &second_doc_id,
            "e-beta-missing",
            "Beta absent",
            "Beta new",
        )
        .await;
        let unsatisfiable_payload = structured(&unsatisfiable);
        assert_eq!(
            unsatisfiable.is_error,
            Some(false),
            "{unsatisfiable_payload}"
        );
        assert_eq!(unsatisfiable_payload["failed"], 1);
        assert_eq!(unsatisfiable_payload["items"][0]["status"], "mismatch");
        let final_save = server
            .save_docx(Parameters(SaveArgs {
                doc_id: second_doc_id,
                path: second_output.to_string_lossy().into_owned(),
            }))
            .await;
        let final_payload = structured(&final_save);
        assert_eq!(final_save.is_error, Some(true), "{final_payload}");
        assert_eq!(final_payload["code"], "task_partial");
        assert_eq!(
            final_payload["task"]["unsatisfied_effects"],
            json!(["e-beta-missing", "e-beta-never"])
        );
        let manifest = stemma_artifacts::decode_task_manifest(
            &std::fs::read(&manifest_path).expect("partial manifest exists"),
        )
        .expect("partial manifest validates");
        assert_eq!(
            manifest.status,
            stemma_artifacts::TaskManifestStatus::Partial
        );
        assert_eq!(
            manifest.targets[0].effects[0].status,
            stemma_artifacts::TaskEffectStatus::Satisfied
        );
        assert_eq!(
            manifest.targets[1].effects[0].status,
            stemma_artifacts::TaskEffectStatus::Unsatisfied
        );
        assert!(
            manifest.targets[1].effects[0]
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("mismatch"))
        );
        assert_eq!(
            manifest.targets[1].effects[1].status,
            stemma_artifacts::TaskEffectStatus::Unsatisfied
        );
        assert_eq!(
            manifest.targets[1].effects[1].reason.as_deref(),
            Some("no execute_plan named this effect")
        );
    }

    #[tokio::test]
    async fn task_midstream_output_write_failure_commits_an_honest_partial_manifest() {
        let temp = tempfile::tempdir().expect("task tempdir");
        let first_path = temp.path().join("first.docx");
        let second_path = temp.path().join("second.docx");
        let first_output = temp.path().join("out-first.docx");
        let second_output = temp.path().join("out-second.docx");
        let manifest_path = temp.path().join("task.json");
        std::fs::write(
            &first_path,
            make_docx("<w:p><w:r><w:t>First old</w:t></w:r></w:p>", false),
        )
        .unwrap();
        std::fs::write(
            &second_path,
            make_docx("<w:p><w:r><w:t>Second old</w:t></w:r></w:p>", false),
        )
        .unwrap();

        let mut server = StemmaServer::new();
        let first_open = server
            .open_docx(Parameters(OpenArgs {
                path: first_path.to_string_lossy().into_owned(),
                task: Some(TaskDeclarationArg {
                    task_id: "task-write-failure".to_string(),
                    manifest_path: manifest_path.to_string_lossy().into_owned(),
                    inputs: vec![],
                    targets: vec![
                        task_delivery::TaskTargetArg {
                            path: first_path.to_string_lossy().into_owned(),
                            effects: vec![declared_replacement(
                                "e-first",
                                "First old",
                                "First new",
                            )],
                        },
                        task_delivery::TaskTargetArg {
                            path: second_path.to_string_lossy().into_owned(),
                            effects: vec![declared_replacement(
                                "e-second",
                                "Second old",
                                "Second new",
                            )],
                        },
                    ],
                }),
                task_id: None,
            }))
            .await;
        let first_doc_id = structured(&first_open)["doc_id"]
            .as_str()
            .unwrap()
            .to_string();
        let first_apply =
            execute_task_item(&server, &first_doc_id, "e-first", "First old", "First new").await;
        assert_eq!(first_apply.is_error, Some(false));
        let first_save = server
            .save_docx(Parameters(SaveArgs {
                doc_id: first_doc_id,
                path: first_output.to_string_lossy().into_owned(),
            }))
            .await;
        assert_eq!(first_save.is_error, Some(false));
        assert!(first_output.exists());

        let second_open = server
            .open_docx(Parameters(OpenArgs {
                path: second_path.to_string_lossy().into_owned(),
                task: None,
                task_id: Some("task-write-failure".to_string()),
            }))
            .await;
        let second_doc_id = structured(&second_open)["doc_id"]
            .as_str()
            .unwrap()
            .to_string();
        let second_apply = execute_task_item(
            &server,
            &second_doc_id,
            "e-second",
            "Second old",
            "Second new",
        )
        .await;
        assert_eq!(second_apply.is_error, Some(false));

        server.artifacts = stemma_artifacts::PathAuthority::explicit_at(temp.path())
            .unwrap()
            .with_commit_failpoint(stemma_artifacts::CommitFailpoint::AfterStageSyncOnce);
        let failed_save = server
            .save_docx(Parameters(SaveArgs {
                doc_id: second_doc_id.clone(),
                path: second_output.to_string_lossy().into_owned(),
            }))
            .await;
        let payload = structured(&failed_save);
        assert_eq!(failed_save.is_error, Some(true), "{payload}");
        assert_eq!(payload["code"], "task_partial");
        assert_eq!(payload["failed_output"]["committed"], false);
        assert!(!second_output.exists());
        assert!(manifest_path.exists());

        let manifest =
            stemma_artifacts::decode_task_manifest(&std::fs::read(&manifest_path).unwrap())
                .unwrap();
        assert_eq!(
            manifest.status,
            stemma_artifacts::TaskManifestStatus::Partial
        );
        assert!(manifest.targets[0].output.is_some());
        assert_eq!(
            manifest.targets[0].effects[0].status,
            stemma_artifacts::TaskEffectStatus::Satisfied
        );
        assert!(manifest.targets[1].output.is_none());
        assert_eq!(
            manifest.targets[1].effects[0].status,
            stemma_artifacts::TaskEffectStatus::Unsatisfied
        );
        assert!(
            manifest.targets[1].effects[0]
                .minted_revision_ids
                .is_empty()
        );
        assert!(
            manifest.targets[1].effects[0]
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("not committed"))
        );

        let after_termination = execute_task_item(
            &server,
            &second_doc_id,
            "e-second",
            "Second old",
            "Second new",
        )
        .await;
        assert_eq!(structured(&after_termination)["code"], "task_terminated");
    }

    #[tokio::test]
    async fn task_effect_id_cannot_substitute_an_unrelated_replacement() {
        let temp = tempfile::tempdir().expect("task tempdir");
        let target_path = temp.path().join("target.docx");
        let manifest_path = temp.path().join("task.json");
        std::fs::write(
            &target_path,
            make_docx(
                "<w:p><w:r><w:t>Declared old; unrelated old</w:t></w:r></w:p>",
                false,
            ),
        )
        .expect("target");
        let server = StemmaServer::new();
        let opened = server
            .open_docx(Parameters(OpenArgs {
                path: target_path.to_string_lossy().into_owned(),
                task: Some(TaskDeclarationArg {
                    task_id: "task-substitution".to_string(),
                    manifest_path: manifest_path.to_string_lossy().into_owned(),
                    inputs: vec![],
                    targets: vec![task_delivery::TaskTargetArg {
                        path: target_path.to_string_lossy().into_owned(),
                        effects: vec![declared_replacement(
                            "e-declared",
                            "Declared old",
                            "Declared new",
                        )],
                    }],
                }),
                task_id: None,
            }))
            .await;
        let doc_id = structured(&opened)["doc_id"]
            .as_str()
            .expect("task doc_id")
            .to_string();
        let refused = execute_task_item(
            &server,
            &doc_id,
            "e-declared",
            "unrelated old",
            "unrelated new",
        )
        .await;
        let payload = structured(&refused);
        assert_eq!(refused.is_error, Some(true), "{payload}");
        assert_eq!(payload["code"], "effect_declaration_mismatch");

        let document = server
            .inspect_docx(Parameters(InspectDocxArgs {
                doc_id,
                query: InspectQuery::Document,
                block_id: None,
                detail: None,
                pattern: None,
                patterns: None,
                filter: None,
                from_block_id: None,
                to_block_id: None,
                format: None,
                offset: None,
                limit: None,
                cell_offset: None,
                cell_limit: None,
            }))
            .await;
        let content = structured(&document)["content"]
            .as_str()
            .expect("document content")
            .to_string();
        assert!(content.contains("unrelated old"));
        assert!(!content.contains("unrelated new"));
    }

    #[tokio::test]
    async fn task_complete_is_limited_to_the_effects_the_caller_declared() {
        // Schema v1 deliberately trusts the declaration. It can verify the
        // declared effect, but it cannot detect omitted intent.
        let temp = tempfile::tempdir().expect("task tempdir");
        let target_path = temp.path().join("target.docx");
        let output_path = temp.path().join("output.docx");
        let manifest_path = temp.path().join("task.json");
        std::fs::write(
            &target_path,
            make_docx("<w:p><w:r><w:t>Only declared old</w:t></w:r></w:p>", false),
        )
        .expect("target");
        let server = StemmaServer::new();
        let opened = server
            .open_docx(Parameters(OpenArgs {
                path: target_path.to_string_lossy().into_owned(),
                task: Some(TaskDeclarationArg {
                    task_id: "task-complete".to_string(),
                    manifest_path: manifest_path.to_string_lossy().into_owned(),
                    inputs: vec![],
                    targets: vec![task_delivery::TaskTargetArg {
                        path: target_path.to_string_lossy().into_owned(),
                        effects: vec![declared_replacement(
                            "e-only",
                            "Only declared old",
                            "Only declared new",
                        )],
                    }],
                }),
                task_id: None,
            }))
            .await;
        let doc_id = structured(&opened)["doc_id"]
            .as_str()
            .expect("task doc_id")
            .to_string();
        let applied = execute_task_item(
            &server,
            &doc_id,
            "e-only",
            "Only declared old",
            "Only declared new",
        )
        .await;
        assert_eq!(applied.is_error, Some(false), "{}", structured(&applied));
        let saved = server
            .save_docx(Parameters(SaveArgs {
                doc_id,
                path: output_path.to_string_lossy().into_owned(),
            }))
            .await;
        let payload = structured(&saved);
        assert_eq!(saved.is_error, Some(false), "{payload}");
        assert_eq!(payload["task"]["status"], "complete");
        assert_eq!(payload["verdict"]["deliverable"], true);
        let manifest = stemma_artifacts::decode_task_manifest(
            &std::fs::read(&manifest_path).expect("complete manifest exists"),
        )
        .expect("complete manifest validates");
        assert_eq!(
            manifest.status,
            stemma_artifacts::TaskManifestStatus::Complete
        );
    }
}
