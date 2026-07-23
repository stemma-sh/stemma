//! stemma-api: an HTTP transport adapter for the `stemma` DOCX engine.
//!
//! This is the symmetric sibling of `stemma-mcp`. Where the MCP server maps
//! *stdio* MCP calls onto the engine, this maps *HTTP/JSON* calls onto it — the
//! same "transport adapter" role the workspace README describes. It exists so a
//! browser front-end (the ProseMirror editor in `stemma-examples/`) can drive a
//! real `.docx` through the durable loop:
//!
//! ```text
//! upload .docx -> parse -> read (block view) -> edit (typed transaction) -> serialize -> .docx
//! ```
//!
//! ## What it is and isn't
//!
//! - It is a thin edge. The engine owns no durable state; this server keeps
//!   opened documents in an in-memory map keyed by a `doc_id`. Persist the saved
//!   `.docx` bytes (plus the transactions) if you want durability — the
//!   in-memory [`Document`] is a hot cache, exactly as the domain model says.
//! - It is a **new** consumer, so it depends on the stable Tier-1 facade
//!   ([`stemma::api::Document`]) for every verb, and reaches the unstable engine
//!   API only at the wire edge to *decode* a transaction
//!   ([`stemma::edit_v4::parse_transaction`], the same path the hosted pipeline
//!   and `examples/quickstart.rs` use). Parse at the edge; operate on the domain
//!   type. (`stemma-mcp` reaches deeper only because it predates the facade.)
//! - It is **fail-loud**: a stale edit, an unknown doc_id, or malformed
//!   transaction JSON returns a structured error, never a best-effort mutation.
//!
//! ## Endpoints
//!
//! | Method & path | Body | Returns |
//! |---|---|---|
//! | `POST /api/documents` | raw `.docx` bytes | `{ doc_id, document }` |
//! | `POST /api/compare` | `{ base_doc_id, target_doc_id, author? }` | `{ doc_id, document }` — a NEW redline document (reject-all == base, accept-all == target); `author` attributes the revisions, empty = 400 |
//! | `GET  /api/documents/{id}` | — | `{ document }` (the read view) |
//! | `POST /api/documents/{id}/apply` | a v4 transaction (JSON) | `{ document }` (re-read after apply) |
//! | `GET  /api/documents/{id}/rich` | — | `{ blocks }` (the rich, render-faithful projection) |
//! | `GET  /api/documents/{id}/revisions` | — | `{ revisions }` (pending tracked changes) |
//! | `POST /api/documents/{id}/resolve` | `{ revision_ids, action }` | `{ document }` (accept/reject) |
//! | `GET  /api/documents/{id}/export?mode=redline\|accepted\|rejected` | — | `.docx` bytes |
//! | `GET  /api/operations` | — | `{ operations }` (the engine's v4 op catalog: fields, cues, canonical shapes) |
//!
//! Everything else is served as static files from `stemma-examples`, so
//! `cargo run -p stemma-api` and then opening the printed URL is the whole demo:
//! a browser review editor (Suggesting/Editing modes, accept/reject) on this
//! one transport.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::HeaderValue;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::{Value, json};
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;

use stemma::api::{BlockRole, BlockView, Document, DocumentView, SegmentView, TrackStatus};
use stemma::edit_v4::catalog::operation_catalog;
use stemma::edit_v4::parse_transaction;
use stemma::runtime::build_tracked_document_view_from_snapshot;
use stemma::semantic_hash::block_semantic_hash_for_full_doc_block;
use stemma::view::{RevisionView, TextMark};
use stemma::{
    ExportMode, ExportOptions, Resolution, ResolveSelectionAction, ValidatorLevel,
    enumerate_revisions,
};

// ─── Session store ────────────────────────────────────────────────────────────

/// The server's only state: opened documents keyed by `doc_id`, plus a counter
/// for minting the next id. A [`Document`] is a pure value (every verb returns a
/// new one), so a write is "compute the next `Document`, store it back" under
/// the lock — no in-place mutation, no shared IR.
#[derive(Clone)]
struct AppState {
    docs: Arc<Mutex<HashMap<String, Document>>>,
    next_id: Arc<AtomicU64>,
}

impl AppState {
    fn new() -> Self {
        Self {
            docs: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    fn mint_id(&self) -> String {
        format!("doc-{}", self.next_id.fetch_add(1, Ordering::Relaxed))
    }
}

// ─── Error edge ─────────────────────────────────────────────────────────────────

/// A structured API error. Mirrors the MCP server's `{ code, error }` receipt so
/// a client can branch on `code` (e.g. re-read the document after `StaleEdit`)
/// rather than scraping a message.
#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    code: String,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status,
            code: code.into(),
            message: message.into(),
        }
    }

    fn not_found(doc_id: &str) -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "UnknownDocument",
            format!("no open document with doc_id {doc_id:?}; upload one first"),
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({ "code": self.code, "error": self.message })),
        )
            .into_response()
    }
}

/// Map a facade [`stemma::RuntimeError`] to an HTTP error, preserving its
/// structured `code`. A precondition failure (stale guard, missing target, …) is
/// the caller's fault → 422; we keep the engine's code verbatim.
fn runtime_err(e: stemma::RuntimeError) -> ApiError {
    ApiError::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        format!("{:?}", e.code),
        e.message,
    )
}

// ─── DocumentView -> JSON ──────────────────────────────────────────────────────
//
// The wire shape the front-end consumes. One object per block, carrying exactly
// what a block-addressed editor needs: the stable `id`, the staleness `guard` a
// write op must echo, whether the block is editable, and the inline segments
// (text + marks + tracked status). Tables and opaque blocks are surfaced
// honestly as non-editable placeholders — never silently dropped.

fn document_json(view: &DocumentView) -> Value {
    json!({
        "blocks": view.blocks.iter().map(block_json).collect::<Vec<_>>(),
    })
}

fn block_json(block: &BlockView) -> Value {
    let (role, level, editable) = match &block.role {
        BlockRole::Paragraph => ("paragraph", None, true),
        BlockRole::Heading { level } => ("heading", Some(*level), true),
        BlockRole::Table => ("table", None, false),
        BlockRole::Opaque => ("opaque", None, false),
    };
    json!({
        "id": block.id.to_string(),
        "role": role,
        "level": level,
        // A write op echoes this so a stale edit fails loud instead of
        // corrupting the wrong block.
        "guard": block.guard,
        // The front-end renders tables/opaque blocks as read-only placeholders;
        // only paragraphs and headings commit edits.
        "editable": editable,
        "text": block.text,
        // A typed-in enumeration label ("1.", "(a)") this paragraph leads with.
        // It is part of `text` but is NOT one of the addressable `segments`
        // (it is structural), so the editor renders it but excludes such a block
        // from a whole-block text replace — which would otherwise drop it.
        "literal_prefix": block.literal_prefix,
        "segments": block.segments.iter().map(segment_json).collect::<Vec<_>>(),
    })
}

fn segment_json(seg: &SegmentView) -> Value {
    match seg {
        SegmentView::Text {
            text,
            status,
            marks,
            ..
        } => json!({
            "kind": "text",
            "text": text,
            "marks": marks.iter().map(mark_str).collect::<Vec<_>>(),
            "track": track_json(status),
        }),
        // An opaque inline anchor (image, field, hyperlink, …). The editor shows
        // it as an inert chip carrying its label; it is not text-editable.
        SegmentView::Opaque { text, status, .. } => json!({
            "kind": "anchor",
            "text": text,
            "track": track_json(status),
        }),
    }
}

fn mark_str(mark: &TextMark) -> &'static str {
    match mark {
        TextMark::Bold => "bold",
        TextMark::Italic => "italic",
        TextMark::Underline => "underline",
        TextMark::Strike => "strike",
        TextMark::Subscript => "subscript",
        TextMark::Superscript => "superscript",
    }
}

/// Flatten a tracked status into `{ status, author?, revision_id? }`. The
/// stacked `inserted_then_deleted` case carries both pending revisions; the
/// editor renders it as struck-through inserted text.
fn track_json(status: &TrackStatus) -> Value {
    fn rev(r: &RevisionView) -> Value {
        json!({ "revision_id": r.revision_id, "author": r.author, "date": r.date })
    }
    match status {
        TrackStatus::Normal => json!({ "status": "normal" }),
        TrackStatus::Inserted(r) => json!({ "status": "inserted", "revision": rev(r) }),
        TrackStatus::Deleted(r) => json!({ "status": "deleted", "revision": rev(r) }),
        TrackStatus::InsertedThenDeleted { inserted, deleted } => json!({
            "status": "inserted_then_deleted",
            "inserted": rev(inserted),
            "deleted": rev(deleted),
        }),
    }
}

// ─── Handlers ───────────────────────────────────────────────────────────────────

/// `POST /api/documents` — body is raw `.docx` bytes. Parse, store, return the
/// new `doc_id` plus the initial read view (so the client renders without a
/// second round trip).
async fn upload(State(state): State<AppState>, body: Bytes) -> Result<Json<Value>, ApiError> {
    if body.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "EmptyBody",
            "POST the raw .docx bytes as the request body",
        ));
    }
    let doc = Document::parse(&body).map_err(|e| {
        // Bad bytes are the client's fault (not a valid .docx) → 400.
        ApiError::new(StatusCode::BAD_REQUEST, format!("{:?}", e.code), e.message)
    })?;
    let view = doc.read();
    let document = document_json(&view);
    let doc_id = state.mint_id();
    state
        .docs
        .lock()
        .expect("docs map poisoned")
        .insert(doc_id.clone(), doc);
    Ok(Json(json!({ "doc_id": doc_id, "document": document })))
}

/// `GET /api/documents/{id}` — the current read view of an open document.
async fn read(
    State(state): State<AppState>,
    Path(doc_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let docs = state.docs.lock().expect("docs map poisoned");
    let doc = docs
        .get(&doc_id)
        .ok_or_else(|| ApiError::not_found(&doc_id))?;
    Ok(Json(json!({ "document": document_json(&doc.read()) })))
}

#[derive(Debug, Deserialize)]
struct ApplyQuery {
    /// Author-impersonation override, mirroring the MCP transport's
    /// `allow_existing_author` flag. The transaction JSON *is* the whole
    /// request body (handed verbatim to `parse_transaction`), so this option
    /// rides as a query parameter rather than a body field — the same
    /// "transaction body + separate transport-level flag" split the MCP
    /// tool args use. Default false: authoring under an author that already
    /// authors a pending revision in the uploaded document's redline is
    /// refused (`AuthorImpersonation`, mapped to 422 below). Pass `true` to
    /// deliberately continue that author's own work.
    #[serde(default)]
    allow_existing_author: bool,
}

/// `POST /api/documents/{id}/apply` — body is a v4 edit transaction (JSON).
///
/// The transaction crosses the wire as the exact JSON the v4 parser expects, so
/// we hand the body string straight to [`parse_transaction`] (the authoritative
/// schema validator) rather than re-deriving the shape here. On success the
/// stored document is *replaced* by the new value and the fresh read view is
/// returned, now carrying the tracked change.
async fn apply(
    State(state): State<AppState>,
    Path(doc_id): Path<String>,
    Query(q): Query<ApplyQuery>,
    body: String,
) -> Result<Json<Value>, ApiError> {
    // Decode + schema-validate at the edge.
    let parsed = parse_transaction(&body)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "SchemaError", e.to_string()))?;
    let txn = parsed
        .into_edit_transaction()
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "AdapterError", e.to_string()))?;

    let mut docs = state.docs.lock().expect("docs map poisoned");
    let doc = docs
        .get(&doc_id)
        .ok_or_else(|| ApiError::not_found(&doc_id))?;
    // `apply_authored` is pure: it returns a NEW document. Replace the stored
    // value only after it succeeds, so a rejected edit leaves the session
    // untouched. Unlike bare `apply`, this enforces the author-impersonation
    // guard (engine-owned policy + data — see `stemma::api::Document::
    // apply_authored`), so an HTTP write is held to the same standard as an
    // MCP one.
    let edited = doc
        .apply_authored(&txn, q.allow_existing_author)
        .map_err(runtime_err)?;
    let document = document_json(&edited.read());
    docs.insert(doc_id, edited);
    Ok(Json(json!({ "document": document })))
}

#[derive(Debug, Deserialize)]
struct ExportQuery {
    /// `redline` (default): the tracked document as-is. `accepted`: accept-all
    /// projection (clean final). `rejected`: reject-all projection (the
    /// baseline). Unknown values are refused — no silent fallback.
    #[serde(default)]
    mode: Option<String>,
}

/// `GET /api/documents/{id}/export` — serialize back to a `.docx` and stream it
/// as a download. The validator gate runs at `Blocking` on this to-bytes edge
/// (structurally-corrupt output is refused, per the engine contract).
async fn export(
    State(state): State<AppState>,
    Path(doc_id): Path<String>,
    Query(q): Query<ExportQuery>,
) -> Result<Response, ApiError> {
    let docs = state.docs.lock().expect("docs map poisoned");
    let doc = docs
        .get(&doc_id)
        .ok_or_else(|| ApiError::not_found(&doc_id))?;

    let opts = ExportOptions {
        mode: ExportMode::Redline,
        validator_level: ValidatorLevel::Blocking,
        validator: None,
    };
    // `redline` serializes the document as it stands (tracked changes pending);
    // `accepted`/`rejected` project first, then serialize the resolved reading.
    // `serialize` is a pure render of whatever document it is handed.
    let bytes = match q.mode.as_deref() {
        None | Some("redline") => doc.serialize(&opts).map_err(runtime_err)?,
        Some("accepted") => doc
            .read_accepted()
            .map_err(runtime_err)?
            .serialize(&opts)
            .map_err(runtime_err)?,
        Some("rejected") => doc
            .read_rejected()
            .map_err(runtime_err)?
            .serialize(&opts)
            .map_err(runtime_err)?,
        Some(other) => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "BadExportMode",
                format!("mode must be 'redline', 'accepted', or 'rejected', got {other:?}"),
            ));
        }
    };

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
            .parse()
            .expect("static content-type is valid"),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        format!("attachment; filename=\"{doc_id}.docx\"")
            .parse()
            .expect("content-disposition is valid"),
    );
    Ok((headers, bytes).into_response())
}

/// `GET /api/operations` — the engine's v4 operation catalog: every op the
/// transaction parser accepts, with its accepted fields, group, one-line cue,
/// and canonical shape(s) (placeholders `<...>` are the caller's to fill).
///
/// This is the same engine-owned catalog (`stemma::edit_v4::catalog`) the MCP
/// server projects via `inspect_docx query="operations"`, minus that
/// transport's edge-only image `path` fields: `/apply` hands the transaction
/// straight to the engine parser, so `insert_image`/`replace_image` take
/// `bytes_base64` here. Shapes are returned as JSON objects; every one is
/// parse-valid by construction (test-pinned in the engine), so the `expect`
/// below can only trip on a programmer bug, never on user input.
async fn operations() -> Json<Value> {
    let operations: Vec<Value> = operation_catalog()
        .into_iter()
        .map(|spec| {
            let examples: Vec<Value> = spec
                .examples
                .iter()
                .map(|shape| {
                    serde_json::from_str(shape)
                        .expect("catalog shapes are parse-valid JSON, pinned by engine tests")
                })
                .collect();
            json!({
                "name": spec.name,
                "group": spec.group,
                "fields": spec.fields,
                "cue": spec.cue,
                "examples": examples,
            })
        })
        .collect();
    Json(json!({
        "transaction_envelope": {
            "ops": "non-empty ordered operation array",
            "revision": {"author": "required tracked-change author", "date": "optional ISO-8601"},
            "summary": "optional",
        },
        "operation_count": operations.len(),
        "operations": operations,
    }))
}

/// `GET /api/documents/{id}/rich` — the **rich read projection**: stemma's full
/// document view, serialized as-is, plus the per-block `guard`.
///
/// This is the high-fidelity surface (the engine's `FullDocViewResult`,
/// serialized whole): every text segment carries its `style_props`
/// (`font_family`, `font_size` in half-points, `color`, `highlight`, underline,
/// …), its `marks`, and any tracked `formatting_change`; every opaque segment
/// carries its kind, `asset_ref` (image data-URI / equation OMML), EMU
/// dimensions, and field metadata; blocks carry `align`/`indent`/`spacing`/
/// numbering and base64 `image_data_uris`. We serialize stemma's own typed view
/// directly (it derives `Serialize`) and stamp each block's `guard` — so this
/// stays a thin projection of the engine, not a re-modeling of it.
///
/// `guard` is `block_semantic_hash_for_full_doc_block`, the SAME hash a write op
/// carries, so a block addressed from this view edits without going stale.
async fn rich_read(
    State(state): State<AppState>,
    Path(doc_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let docs = state.docs.lock().expect("docs map poisoned");
    let doc = docs
        .get(&doc_id)
        .ok_or_else(|| ApiError::not_found(&doc_id))?;

    // The write guard must be the one `apply` checks. The lean read view's guard
    // (`block_semantic_hash_for_block`) is that hash and stays valid across
    // sequential edits; `block_semantic_hash_for_full_doc_block` matches it on a
    // clean doc but DIVERGES once a tracked change exists. So stamp the lean
    // guard, correlated by block id, and fall back to the full-doc-block hash
    // only for blocks the lean view doesn't carry.
    let lean = doc.read();
    let guard_by_id: HashMap<String, String> = lean
        .blocks
        .iter()
        .map(|b| (b.id.to_string(), b.guard.clone()))
        .collect();
    // The lean view carries the table cell grid (positions, spans, resolved
    // borders/shading) and table-level metadata (column widths, alignment); the
    // rich full-doc view doesn't surface them for an unchanged table. Attach both
    // by id so the editor can render a real, Word-faithful table.
    let cells_by_id: HashMap<String, Value> = lean
        .blocks
        .iter()
        .filter(|b| !b.cells.is_empty())
        .map(|b| {
            (
                b.id.to_string(),
                serde_json::to_value(&b.cells).unwrap_or(Value::Null),
            )
        })
        .collect();
    let table_by_id: HashMap<String, Value> = lean
        .blocks
        .iter()
        .filter_map(|b| {
            b.table.as_ref().map(|t| {
                (
                    b.id.to_string(),
                    serde_json::to_value(t).unwrap_or(Value::Null),
                )
            })
        })
        .collect();

    let view = build_tracked_document_view_from_snapshot(doc.snapshot());
    let blocks: Vec<Value> = view
        .blocks
        .iter()
        .map(|b| {
            let mut v = serde_json::to_value(b).expect("FullDocBlock serializes");
            let id = b.block_id.to_string();
            v["guard"] = json!(
                guard_by_id
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| block_semantic_hash_for_full_doc_block(b))
            );
            if let Some(cells) = cells_by_id.get(&id) {
                v["cells"] = cells.clone();
            }
            if let Some(table) = table_by_id.get(&id) {
                v["table"] = table.clone();
            }
            v
        })
        .collect();

    // Body section properties (page size + margins, in twips) for page geometry.
    let section = view
        .body_section_properties
        .as_ref()
        .map(|s| serde_json::to_value(s).unwrap_or(Value::Null))
        .unwrap_or(Value::Null);

    // Header/footer bands referenced by the body section. Each carries its
    // `kind` (default/first/even) and the story's inline `segments` — the SAME
    // segment shape body blocks use — so the frontend renders them read-only
    // with faithful formatting (tabs, marks, fields). `inline_index` is stamped
    // for parity with body segments (header/footer text is not addressable for
    // editing, so a stable 0 is fine).
    let project_band = |p: &stemma::HeaderFooterPayload| -> Value {
        // One entry per paragraph, carrying its alignment (w:jc) and tab stops
        // (w:tabs) so the frontend can center/right-align and position tabbed
        // content the way Word does — not flatten it to one left-aligned line.
        let paragraphs: Vec<Value> = p
            .paragraphs
            .iter()
            .map(|para| {
                let segments: Vec<Value> = para
                    .segments
                    .iter()
                    .map(|s| serde_json::to_value(s).expect("InlineChange serializes"))
                    .collect();
                json!({
                    "align": para.align.as_ref().map(|a| serde_json::to_value(a).unwrap_or(Value::Null)),
                    "tab_stops": serde_json::to_value(&para.tab_stops).unwrap_or(Value::Null),
                    "segments": segments,
                })
            })
            .collect();
        json!({ "kind": p.kind, "paragraphs": paragraphs })
    };
    let headers: Vec<Value> = view.headers.iter().map(project_band).collect();
    let footers: Vec<Value> = view.footers.iter().map(project_band).collect();

    // Comment threads (§17.13.4). Each `CommentPayload` carries the comment id
    // (the SAME `reference_id` the commented span's `CommentReference` opaque
    // segment carries — so the frontend links a sidebar card to its highlighted
    // span), the author/date, the body flattened to inline `segments` (same
    // shape body blocks use), the `resolved` (`w15:done`) flag, and
    // `parent_para_id` for reply-thread children. We serialize the engine's
    // typed view directly — a thin projection, not a re-modeling.
    let comments: Vec<Value> = view
        .comments
        .iter()
        .map(|c| {
            let segments: Vec<Value> = c
                .segments
                .iter()
                .map(|s| serde_json::to_value(s).expect("InlineChange serializes"))
                .collect();
            json!({
                "id": c.id,
                "author": c.author,
                "date": c.date,
                "segments": segments,
                "resolved": c.resolved,
                "parent_para_id": c.parent_para_id,
            })
        })
        .collect();

    Ok(Json(json!({
        "blocks": blocks,
        "section": section,
        "headers": headers,
        "footers": footers,
        "comments": comments,
    })))
}

/// `GET /api/documents/{id}/revisions` — the pending tracked changes, in
/// document order. This is the review surface: a redline tool (and the proposals
/// example) reads it to build an accept/reject worklist. One row per SELECTABLE
/// revision; non-selectable records (`revision_id == 0`) are omitted — both
/// legacy pre-identity formatting changes AND `opaque_interior` records
/// (tracked changes inside verbatim opaque content like a textbox, which the
/// census reports but which cannot be resolved by id — see
/// `RevisionKind::OpaqueInterior`). This endpoint is a resolve worklist, so it
/// deliberately lists only what `POST /resolve` can act on; the full honest
/// census (including opaque interiors) is what `enumerate_revisions` returns.
/// Grouping into "proposals" is the client's job (it knows which apply produced
/// which ids) — the engine enumerates a flat, honest list.
async fn revisions(
    State(state): State<AppState>,
    Path(doc_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let docs = state.docs.lock().expect("docs map poisoned");
    let doc = docs
        .get(&doc_id)
        .ok_or_else(|| ApiError::not_found(&doc_id))?;
    // `enumerate_revisions` is the SAME walk the accept/reject selector lowers
    // against, so a row here is resolvable by its `revision_id` below.
    let rows: Vec<Value> = enumerate_revisions(&doc.snapshot().canonical)
        .into_iter()
        .filter(|r| r.revision_id != 0)
        .map(|r| {
            json!({
                "revision_id": r.revision_id,
                "author": r.author,
                "kind": r.kind.as_str(),
                "block_id": r.block_id.to_string(),
                "excerpt": r.excerpt,
                "date": r.date,
            })
        })
        .collect();
    Ok(Json(json!({ "revisions": rows })))
}

#[derive(Debug, Deserialize)]
struct ResolveBody {
    /// The revision ids to resolve. Must be currently pending (from `revisions`).
    revision_ids: Vec<u32>,
    /// `accept` bakes the change in; `reject` reverts it. The unnamed rest stay
    /// pending. Unknown values are refused — no silent fallback.
    action: String,
}

/// `POST /api/documents/{id}/resolve` — accept or reject a specific set of
/// tracked changes (the accept/decline-a-proposal verb). Selective resolution
/// leaves every revision NOT named still pending, so a reviewer resolves one
/// proposal at a time. Returns the re-read document.
async fn resolve(
    State(state): State<AppState>,
    Path(doc_id): Path<String>,
    Json(body): Json<ResolveBody>,
) -> Result<Json<Value>, ApiError> {
    let action = match body.action.as_str() {
        "accept" => ResolveSelectionAction::Accept,
        "reject" => ResolveSelectionAction::Reject,
        other => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "BadAction",
                format!("action must be 'accept' or 'reject', got {other:?}"),
            ));
        }
    };
    if body.revision_ids.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "EmptySelection",
            "revision_ids must name at least one pending revision",
        ));
    }
    let ids: HashSet<u32> = body.revision_ids.into_iter().collect();

    let mut docs = state.docs.lock().expect("docs map poisoned");
    let doc = docs
        .get(&doc_id)
        .ok_or_else(|| ApiError::not_found(&doc_id))?;
    // `project` is pure (returns a new Document); store it only on success, so a
    // rejected selection leaves the session untouched.
    let resolved = doc
        .project(Resolution::Selective { ids, action })
        .map_err(runtime_err)?;
    let document = document_json(&resolved.read());
    docs.insert(doc_id, resolved);
    Ok(Json(json!({ "document": document })))
}

#[derive(Debug, Deserialize)]
struct CompareBody {
    /// The baseline document (already uploaded). Reject-all of the produced
    /// redline reconstructs this document.
    base_doc_id: String,
    /// The target document (already uploaded). Accept-all of the produced
    /// redline reconstructs this document.
    target_doc_id: String,
    /// Optional attribution for the discovered revisions. Absent = the redline
    /// is anonymous (the Tier-1 `diff`); present = every revision is attributed
    /// to this name (`diff_as`). An empty string is refused with a 400 — there
    /// is no silent fallback to anonymous.
    #[serde(default)]
    author: Option<String>,
}

/// `POST /api/compare` — discover the deltas between two already-uploaded
/// documents (`base` → `target`) and materialize them as tracked changes in a
/// **new** stored document, returning its fresh `doc_id` plus its read view.
///
/// The result is a first-class session document: `/revisions`, `/resolve`, and
/// `/export` compose with the returned `doc_id` exactly as they do with an
/// uploaded one. The engine's round-trip contract holds on it — reject-all
/// reconstructs `base`, accept-all reconstructs `target` (see
/// [`stemma::api::Document::diff`]).
///
/// Attribution: the optional `author` field attributes the discovered
/// revisions. Absent, the redline is anonymous (the Tier-1 `diff`, discovery
/// without an authoring transaction). Present, every revision is attributed to
/// that name (the Tier-1 `diff_as`). An empty-string `author` is refused with a
/// 400 (`BadAuthor`) — omit the field for an anonymous redline; there is no
/// silent fallback to anonymous.
///
/// An unknown `base_doc_id` or `target_doc_id` takes the same 404 path as every
/// other endpoint (`UnknownDocument`). Comparing a document against itself is
/// allowed and honest: `diff` finds no deltas, so the new document carries an
/// empty redline (accept-all and reject-all read identically).
async fn compare(
    State(state): State<AppState>,
    Json(body): Json<CompareBody>,
) -> Result<Json<Value>, ApiError> {
    // Reject an empty author at the edge (before locking the store): a present
    // but empty `author` is a client mistake, not a request for an anonymous
    // redline (that is the absent field). 400, not the engine's 422.
    if let Some(author) = &body.author
        && author.is_empty()
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "BadAuthor",
            "author must be a non-empty string; omit the field for an anonymous redline",
        ));
    }
    // Mint the id up front (a pure counter bump — it touches no document), so
    // the store is locked only for the read-diff-store critical section.
    let doc_id = state.mint_id();
    let mut docs = state.docs.lock().expect("docs map poisoned");
    let base = docs
        .get(&body.base_doc_id)
        .ok_or_else(|| ApiError::not_found(&body.base_doc_id))?;
    let target = docs
        .get(&body.target_doc_id)
        .ok_or_else(|| ApiError::not_found(&body.target_doc_id))?;
    // `diff`/`diff_as` are pure: they return a NEW document (the redline),
    // touching neither input. Store it under the freshly minted id. `author`
    // present = attributed (`diff_as`); absent = anonymous (`diff`).
    let redline = match &body.author {
        Some(author) => base.diff_as(target, author).map_err(runtime_err)?,
        None => base.diff(target).map_err(runtime_err)?,
    };
    let document = document_json(&redline.read());
    docs.insert(doc_id.clone(), redline);
    Ok(Json(json!({ "doc_id": doc_id, "document": document })))
}

// ─── Wiring ─────────────────────────────────────────────────────────────────────

/// Resolve the directory holding the browser example's static assets. In a
/// normal workspace checkout this is `../stemma-examples` relative to this crate
/// (its `index.html` plus the `editor/` app); `STEMMA_API_STATIC_DIR` overrides it.
fn static_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("STEMMA_API_STATIC_DIR") {
        return PathBuf::from(dir);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("stemma-examples")
}

fn app(state: AppState) -> Router {
    let static_dir = static_dir();
    Router::new()
        .route("/api/documents", post(upload))
        .route("/api/compare", post(compare))
        .route("/api/documents/{id}", get(read))
        .route("/api/documents/{id}/apply", post(apply))
        .route("/api/documents/{id}/rich", get(rich_read))
        .route("/api/documents/{id}/revisions", get(revisions))
        .route("/api/documents/{id}/resolve", post(resolve))
        .route("/api/documents/{id}/export", get(export))
        .route("/api/operations", get(operations))
        // Anything not under /api is served from the examples' static assets, so
        // the front-end and API share an origin (no CORS) and one command runs
        // the whole demo. The landing page links to each example.
        .fallback_service(ServeDir::new(static_dir))
        // Dev server: tell the browser to always revalidate. `ServeDir` only
        // sends `Last-Modified`, which triggers heuristic caching — a browser
        // then serves a stale `app.js` after an edit without rechecking. With
        // `no-cache` it revalidates every load (cheap 304s for unchanged files),
        // so an edited example shows up on a plain reload.
        .layer(SetResponseHeaderLayer::overriding(
            axum::http::header::CACHE_CONTROL,
            HeaderValue::from_static("no-cache"),
        ))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// The host/port to bind, resolved from CLI flags then env then defaults. Flags
/// win so a quick `cargo run -p stemma-api -- --host` overrides everything.
struct Bind {
    host: IpAddr,
    port: u16,
}

/// Parse `--host[=ADDR]` and `--port=N` (CLI) over `STEMMA_API_PORT` (env) over
/// the loopback defaults. `--host` with no value binds `0.0.0.0` (all
/// interfaces) — needed to reach the server from another machine or across a
/// container boundary. Fails loud on an unknown flag or bad value (no silent
/// fallback); `--help` prints usage.
fn parse_bind() -> Bind {
    fn die(msg: &str) -> ! {
        eprintln!("stemma-api: {msg}");
        eprintln!("usage: stemma-api [--host[=ADDR]] [--port=N]");
        eprintln!(
            "  --host          bind 0.0.0.0 (all interfaces); --host=ADDR for a specific one"
        );
        eprintln!("  --port=N        TCP port (default 3000, or $STEMMA_API_PORT)");
        std::process::exit(2);
    }

    let mut host: Option<IpAddr> = None;
    let mut port: Option<u16> = None;
    let mut args = std::env::args().skip(1).peekable();
    while let Some(arg) = args.next() {
        let parse_host = |v: &str| {
            v.parse::<IpAddr>()
                .unwrap_or_else(|_| die(&format!("invalid --host address: {v}")))
        };
        let parse_port = |v: &str| {
            v.parse::<u16>()
                .unwrap_or_else(|_| die(&format!("invalid --port: {v}")))
        };
        match arg.as_str() {
            "--help" | "-h" => die("help"),
            "--host" => {
                // Optional value: consume the next token only if it isn't a flag.
                host = Some(match args.peek() {
                    Some(v) if !v.starts_with('-') => parse_host(&args.next().unwrap()),
                    _ => IpAddr::from([0, 0, 0, 0]),
                });
            }
            s if s.starts_with("--host=") => host = Some(parse_host(&s["--host=".len()..])),
            "--port" => {
                port = Some(parse_port(
                    &args
                        .next()
                        .unwrap_or_else(|| die("--port requires a value")),
                ))
            }
            s if s.starts_with("--port=") => port = Some(parse_port(&s["--port=".len()..])),
            other => die(&format!("unknown argument: {other}")),
        }
    }

    Bind {
        host: host.unwrap_or(IpAddr::from([127, 0, 0, 1])),
        port: port
            .or_else(|| {
                std::env::var("STEMMA_API_PORT")
                    .ok()
                    .and_then(|s| s.parse().ok())
            })
            .unwrap_or(3000),
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "stemma_api=info,tower_http=info".into()),
        )
        .init();

    let Bind { host, port } = parse_bind();
    let addr = SocketAddr::new(host, port);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .unwrap_or_else(|e| panic!("cannot bind {addr}: {e}"));

    tracing::info!("stemma-api listening on http://{addr}");
    if host.is_unspecified() {
        tracing::info!("reach it at http://localhost:{port} (or http://<this-host-ip>:{port})");
    } else {
        tracing::info!("open http://{addr} for the examples");
    }

    axum::serve(listener, app(AppState::new()))
        .await
        .expect("server error");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal single-paragraph DOCX, borrowed from the ProseMirror
    /// example's sample set so this transport test needs no zip-building
    /// helper of its own.
    const SIMPLE_TEXT_DOCX: &[u8] =
        include_bytes!("../../stemma-examples/samples/simple-text.docx");

    /// A base/target pair (same body, one word changed) from the engine's own
    /// diff fixture, so a compare test has two genuinely-differing documents
    /// without building a second docx by hand.
    const BEFORE_DOCX: &[u8] =
        include_bytes!("../../stemma-engine/testdata/simple-text/before.docx");
    const AFTER_DOCX: &[u8] = include_bytes!("../../stemma-engine/testdata/simple-text/after.docx");

    /// Upload raw bytes through the real handler and return the minted `doc_id`.
    async fn upload_bytes(state: &AppState, bytes: &[u8]) -> String {
        upload(State(state.clone()), Bytes::from(bytes.to_vec()))
            .await
            .expect("upload")
            .0["doc_id"]
            .as_str()
            .expect("doc_id")
            .to_string()
    }

    /// The visible text of a stored document, read back out of the session
    /// store (the compare handler returns the lean view, not the text).
    fn stored_text(state: &AppState, doc_id: &str) -> String {
        let docs = state.docs.lock().expect("docs map poisoned");
        docs.get(doc_id).expect("doc stored").to_text()
    }

    /// Build a v4 transaction JSON replacing `block`'s whole text, authored
    /// by `author`. `expect` defaults to the block's accept-all text, which is
    /// only a valid precondition on a block with no pending deletion (`expect`
    /// is matched against the VISIBLE pending text — Normal ∪ Inserted, never
    /// struck). For a redlined block, target its visible text explicitly via
    /// [`replace_txn_json_expect`].
    fn replace_txn_json(block: &BlockView, replacement: &str, author: &str) -> String {
        replace_txn_json_expect(&block.id.to_string(), &block.text, replacement, author)
    }

    /// As [`replace_txn_json`], but with an explicit `expect` precondition.
    fn replace_txn_json_expect(
        target: &str,
        expect: &str,
        replacement: &str,
        author: &str,
    ) -> String {
        json!({
            "ops": [{
                "op": "replace",
                "target": target,
                "expect": expect,
                "content": {
                    "type": "paragraph",
                    "content": [{ "type": "text", "text": replacement }],
                },
            }],
            "revision": { "author": author },
        })
        .to_string()
    }

    /// `SIMPLE_TEXT_DOCX`, re-serialized after seeding one tracked change
    /// authored by `origin_author` — built by authoring + serializing (the
    /// real path a document with an existing redline arrives through), so
    /// `origin_author`'s revision is genuinely present in the redline these
    /// bytes carry, not just poked into some internal field.
    fn docx_with_existing_author(origin_author: &str) -> Vec<u8> {
        let doc = Document::parse(SIMPLE_TEXT_DOCX).expect("parse fixture");
        let block = doc
            .read()
            .blocks
            .first()
            .cloned()
            .expect("fixture has a block");
        let txn_json = replace_txn_json(&block, "Seeded change", origin_author);
        let parsed = parse_transaction(&txn_json).expect("parse seed transaction");
        let txn = parsed
            .into_edit_transaction()
            .expect("adapt seed transaction");
        let seeded = doc.apply(&txn).expect("seed the origin author's revision");
        seeded
            .serialize(&ExportOptions::default())
            .expect("serialize the seeded redline")
    }

    /// `GET /api/operations` serves the engine catalog verbatim: one row per
    /// parser op, examples as JSON objects, and no MCP edge fields — this
    /// transport's `/apply` feeds the engine parser directly, so advertising
    /// `path` on the image ops would teach a field the parser rejects.
    #[tokio::test]
    async fn operations_serves_the_engine_catalog_without_mcp_edge_fields() {
        let payload = operations().await.0;
        assert_eq!(
            payload["operation_count"].as_u64(),
            Some(stemma::edit_v4::operation_vocabulary().len() as u64),
            "one catalog row per parser op: {payload}"
        );
        let ops = payload["operations"].as_array().expect("operations array");
        let insert_image = ops
            .iter()
            .find(|row| row["name"] == "insert_image")
            .expect("insert_image row");
        let fields = insert_image["fields"].as_array().expect("fields array");
        assert!(
            fields.iter().any(|f| f == "bytes_base64") && !fields.iter().any(|f| f == "path"),
            "insert_image must advertise the parser's bytes_base64, never the MCP edge path: {insert_image}"
        );
        let replace = ops
            .iter()
            .find(|row| row["name"] == "replace")
            .expect("replace row");
        assert!(
            replace["examples"][0].is_object(),
            "examples are decoded JSON objects, not strings: {replace}"
        );
    }

    /// THE CONTRACT: `POST /apply` refuses a write whose `revision.author`
    /// already authors a pending revision in the uploaded document's
    /// redline — the same author-impersonation guard MCP enforces (see
    /// `stemma::api::Document::apply_authored`), now held at the HTTP edge
    /// too. `allow_existing_author=true` deliberately continues that
    /// author's own work; a plain 400/404 stays reserved for malformed
    /// input, so the refusal maps to 422 with the engine's code, per this
    /// module's existing `runtime_err` convention.
    #[tokio::test]
    async fn apply_refuses_to_impersonate_the_uploaded_documents_existing_author() {
        let state = AppState::new();
        let docx = docx_with_existing_author("AuthorA");
        let uploaded = upload(State(state.clone()), Bytes::from(docx))
            .await
            .expect("upload");
        let doc_id = uploaded.0["doc_id"].as_str().expect("doc_id").to_string();
        let block = {
            let docs = state.docs.lock().expect("docs map poisoned");
            docs.get(&doc_id)
                .expect("doc stored")
                .read()
                .blocks
                .first()
                .cloned()
                .expect("uploaded doc has a block")
        };
        // The uploaded doc carries AuthorA's pending redline: the original text
        // is struck and "Seeded change" is the visible-pending insertion. `expect`
        // matches the VISIBLE pending text (Normal ∪ Inserted, never struck), so
        // target the still-live "Seeded change" rather than the accept-all reading
        // (`block.text`), which also spans the struck original and is not the state
        // being edited.
        let impersonating = replace_txn_json_expect(
            &block.id.to_string(),
            "Seeded change",
            "Attempted impersonation",
            "AuthorA",
        );

        let refused = apply(
            State(state.clone()),
            Path(doc_id.clone()),
            Query(ApplyQuery {
                allow_existing_author: false,
            }),
            impersonating.clone(),
        )
        .await;
        let err = refused.expect_err("impersonating AuthorA must be refused over HTTP");
        assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(err.code, "AuthorImpersonation");
        assert!(
            err.message.contains("AuthorA"),
            "the error names the impersonated author: {}",
            err.message
        );

        // The override deliberately continues that author's own work.
        let _ = apply(
            State(state.clone()),
            Path(doc_id),
            Query(ApplyQuery {
                allow_existing_author: true,
            }),
            impersonating,
        )
        .await
        .expect("allow_existing_author=true bypasses the refusal");
    }

    /// A distinct author is never impersonation, and the default
    /// `allow_existing_author=false` does not block ordinary writes to a
    /// document with no pre-existing redline.
    #[tokio::test]
    async fn apply_accepts_a_distinct_author_by_default() {
        let state = AppState::new();
        let uploaded = upload(State(state.clone()), Bytes::from(SIMPLE_TEXT_DOCX.to_vec()))
            .await
            .expect("upload");
        let doc_id = uploaded.0["doc_id"].as_str().expect("doc_id").to_string();
        let block = {
            let docs = state.docs.lock().expect("docs map poisoned");
            docs.get(&doc_id)
                .expect("doc stored")
                .read()
                .blocks
                .first()
                .cloned()
                .expect("uploaded doc has a block")
        };
        let txn = replace_txn_json(&block, "First edit", "Counsel");

        let _ = apply(
            State(state),
            Path(doc_id),
            Query(ApplyQuery {
                allow_existing_author: false,
            }),
            txn,
        )
        .await
        .expect("a distinct author on a clean document is accepted");
    }

    /// THE CONTRACT: `POST /api/compare` of two uploaded documents stores a NEW
    /// redline document whose engine round-trip holds — reject-all reconstructs
    /// the base, accept-all reconstructs the target — and whose fresh `doc_id`
    /// is distinct from both inputs (so `/revisions`, `/resolve`, `/export`
    /// compose with it without disturbing either input).
    #[tokio::test]
    async fn compare_stores_a_redline_whose_accept_all_is_target_and_reject_all_is_base() {
        let state = AppState::new();
        let base_id = upload_bytes(&state, BEFORE_DOCX).await;
        let target_id = upload_bytes(&state, AFTER_DOCX).await;
        let base_text = stored_text(&state, &base_id);
        let target_text = stored_text(&state, &target_id);
        assert_ne!(base_text, target_text, "the fixtures must differ");

        let out = compare(
            State(state.clone()),
            Json(CompareBody {
                base_doc_id: base_id.clone(),
                target_doc_id: target_id.clone(),
                author: None,
            }),
        )
        .await
        .expect("compare two known documents");
        let redline_id = out.0["doc_id"].as_str().expect("doc_id").to_string();
        assert_ne!(redline_id, base_id, "the redline is a new document");
        assert_ne!(redline_id, target_id, "the redline is a new document");

        let (accepted, rejected) = {
            let docs = state.docs.lock().expect("docs map poisoned");
            let stored = docs.get(&redline_id).expect("redline stored");
            (
                stored.read_accepted().expect("accept-all").to_text(),
                stored.read_rejected().expect("reject-all").to_text(),
            )
        };
        assert_eq!(accepted, target_text, "accept-all reconstructs the target");
        assert_eq!(rejected, base_text, "reject-all reconstructs the base");
    }

    /// An unknown `base_doc_id` (or `target_doc_id`) takes the same 404
    /// `UnknownDocument` path every other endpoint uses — no silent
    /// empty-diff, no best-effort.
    #[tokio::test]
    async fn compare_unknown_doc_id_is_404() {
        let state = AppState::new();
        let known = upload_bytes(&state, BEFORE_DOCX).await;

        let err = compare(
            State(state.clone()),
            Json(CompareBody {
                base_doc_id: "doc-does-not-exist".to_string(),
                target_doc_id: known.clone(),
                author: None,
            }),
        )
        .await
        .expect_err("an unknown base is refused");
        assert_eq!(err.status, StatusCode::NOT_FOUND);
        assert_eq!(err.code, "UnknownDocument");

        let err = compare(
            State(state.clone()),
            Json(CompareBody {
                base_doc_id: known,
                target_doc_id: "doc-does-not-exist".to_string(),
                author: None,
            }),
        )
        .await
        .expect_err("an unknown target is refused");
        assert_eq!(err.status, StatusCode::NOT_FOUND);
        assert_eq!(err.code, "UnknownDocument");
    }

    /// Comparing a document against itself is allowed and honest: `diff` finds
    /// no deltas, so the stored redline is empty (its `/revisions` worklist is
    /// empty, and accept-all reads the same as reject-all).
    #[tokio::test]
    async fn compare_same_doc_is_an_empty_redline() {
        let state = AppState::new();
        let id = upload_bytes(&state, SIMPLE_TEXT_DOCX).await;

        let out = compare(
            State(state.clone()),
            Json(CompareBody {
                base_doc_id: id.clone(),
                target_doc_id: id,
                author: None,
            }),
        )
        .await
        .expect("comparing a document against itself is allowed");
        let redline_id = out.0["doc_id"].as_str().expect("doc_id").to_string();

        let revisions = revisions(State(state.clone()), Path(redline_id.clone()))
            .await
            .expect("revisions of the empty redline");
        let rows = revisions.0["revisions"]
            .as_array()
            .expect("revisions array");
        assert!(
            rows.is_empty(),
            "an identical-doc compare yields no revisions"
        );

        let docs = state.docs.lock().expect("docs map poisoned");
        let stored = docs.get(&redline_id).expect("redline stored");
        assert_eq!(
            stored.read_accepted().expect("accept-all").to_text(),
            stored.read_rejected().expect("reject-all").to_text(),
            "an empty redline reads identically accepted and rejected",
        );
    }

    /// A present `author` attributes every discovered revision (`diff_as`),
    /// visible on the stored redline's `/revisions` rows, and the round-trip is
    /// unchanged (reject-all == base, accept-all == target).
    #[tokio::test]
    async fn compare_with_author_attributes_the_revisions() {
        let state = AppState::new();
        let base_id = upload_bytes(&state, BEFORE_DOCX).await;
        let target_id = upload_bytes(&state, AFTER_DOCX).await;
        let base_text = stored_text(&state, &base_id);
        let target_text = stored_text(&state, &target_id);

        let out = compare(
            State(state.clone()),
            Json(CompareBody {
                base_doc_id: base_id.clone(),
                target_doc_id: target_id.clone(),
                author: Some("Reviewer".to_string()),
            }),
        )
        .await
        .expect("compare with an author");
        let redline_id = out.0["doc_id"].as_str().expect("doc_id").to_string();

        // Every pending revision on the stored redline names the author.
        let rows = revisions(State(state.clone()), Path(redline_id.clone()))
            .await
            .expect("revisions of the attributed redline");
        let rows = rows.0["revisions"]
            .as_array()
            .expect("revisions array")
            .clone();
        assert!(!rows.is_empty(), "the redline carries revisions");
        assert!(
            rows.iter()
                .all(|r| r["author"].as_str() == Some("Reviewer")),
            "every revision row is attributed to the supplied author: {rows:?}"
        );

        // The round-trip is undisturbed by attribution.
        let docs = state.docs.lock().expect("docs map poisoned");
        let stored = docs.get(&redline_id).expect("redline stored");
        assert_eq!(
            stored.read_accepted().expect("accept-all").to_text(),
            target_text,
            "accept-all reconstructs the target"
        );
        assert_eq!(
            stored.read_rejected().expect("reject-all").to_text(),
            base_text,
            "reject-all reconstructs the base"
        );
    }

    /// A present-but-empty `author` is a client mistake, not a request for an
    /// anonymous redline (that is the ABSENT field): 400 `BadAuthor`, and no
    /// document is stored.
    #[tokio::test]
    async fn compare_empty_author_is_400() {
        let state = AppState::new();
        let base_id = upload_bytes(&state, BEFORE_DOCX).await;
        let target_id = upload_bytes(&state, AFTER_DOCX).await;
        let doc_count_before = state.docs.lock().expect("docs map poisoned").len();

        let err = compare(
            State(state.clone()),
            Json(CompareBody {
                base_doc_id: base_id,
                target_doc_id: target_id,
                author: Some(String::new()),
            }),
        )
        .await
        .expect_err("an empty author is refused");
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert_eq!(err.code, "BadAuthor");
        assert_eq!(
            state.docs.lock().expect("docs map poisoned").len(),
            doc_count_before,
            "a refused compare stores no new document"
        );
    }
}
