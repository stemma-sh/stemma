# HTTP API reference

`stemma-api` maps the same verbs as the MCP server onto HTTP/JSON. The
transaction grammar, receipts, and refusal vocabulary are identical to the
[MCP reference](mcp.md); only the transport differs.

> **Scope.** This is example/demo infrastructure: a single-process, in-memory,
> single-origin server. It has no auth, no TLS, and no session eviction; it
> binds loopback only; and documents live only in RAM (keyed by `doc_id`) until
> the process exits. For a hosted runtime or any new consumer, build on the
> engine itself, not this adapter: see [Embed the engine](embedding.md) for
> the facade and session runtime, and the
> [read model reference](read-model.md) for the shapes you read. See
> [stemma-api/README.md](https://github.com/stemma-sh/stemma/blob/main/stemma-api/README.md#scope).

## Run it (and see it)

```bash
cargo run -p stemma-api          # then open http://127.0.0.1:3000
```

One command starts the API **and** serves the browser review editor that
runs on it. This Word-style front end uses plain static files and requires no
build step. The first load fetches ProseMirror and MathJax from public CDNs. It
renders real formatting, edits in Suggesting/Editing mode, and
accepts/rejects tracked changes. It's the fastest way to watch the engine
work, and its source (`stemma-examples/editor`) doubles as a reference
client for the endpoints below.

## Endpoints

| Method & path | Body | Returns |
|---|---|---|
| `POST /api/documents` | raw `.docx` bytes | `{ doc_id, document }` |
| `POST /api/compare` | `{ base_doc_id, target_doc_id, author? }` | `{ doc_id, document }`, containing a **new** redline document |
| `GET  /api/documents/{id}` | none | `{ document }` |
| `POST /api/documents/{id}/apply` | a [v4 transaction](operations.md) (JSON) | `{ document }` after apply |
| `GET  /api/documents/{id}/rich` | none | `{ blocks, section, headers, footers, comments }`, the engine's [full render view](read-model.md#the-full-render-view) serialized whole, with fonts, colors, images, equations, and a per-block guard |
| `GET  /api/documents/{id}/revisions` | none | `{ revisions }`, containing pending tracked changes |
| `POST /api/documents/{id}/resolve` | `{ revision_ids, action }` | `{ document }` (accept/reject) |
| `GET  /api/documents/{id}/export?mode=redline\|accepted\|rejected` | none | `.docx` download |
| `GET  /api/operations` | none | `{ transaction_envelope, operation_count, operations }`, the engine's v4 operation catalog |

## Discover the operation catalog

`GET /api/operations` returns the engine's own operation catalog: one row per
transaction op with its `name`, `group`, accepted `fields`, one-line `cue`,
and canonical `examples` (as JSON objects, placeholders like `<block_id>`
left for you to fill). It is the same catalog published as the
[v4 operation reference](operations.md) and served over MCP; because `/apply`
hands the transaction straight to the engine parser, the rows list exactly
the fields `/apply` accepts (image ops take `bytes_base64` here; the MCP-only
`path` convenience does not exist on this transport). Unknown fields and
unknown ops are rejected loudly, so build requests from these rows rather
than guessing.

## Render a document

`GET /api/documents/{id}/rich` is the render read: it serializes the engine's
[full render view](read-model.md#the-full-render-view) verbatim (typed blocks,
segments with resolved `style_props`, image data URIs, equation OMML, header
and footer bands, comment threads, section page geometry), then stamps three
keys onto each block by id from the lean view: `guard` (the SAME hash a write
op carries, so a block addressed from this view edits without going stale)
plus, for table blocks, `cells` and `table` metadata. Field-by-field
documentation lives in the read model reference, generated from the engine so
it cannot drift; this page only owns the envelope and the stamping rule.

`GET /api/documents/{id}` is the lean navigation read: a reduced projection
of the [lean view](read-model.md#the-lean-view) with per-block `id`, `role`,
`level`, `guard`, `editable`, `text`, `literal_prefix`, and `segments` (each
segment carrying `marks` and a `track` status with its `revision_id`).

## Review and resolve

Each `GET /revisions` row carries `revision_id` (the engine-minted identity,
see [revision identity](read-model.md#revision-identity-and-the-review-loop)),
`author`, `kind`, `block_id`, `excerpt`, and `date`. The complete `kind`
vocabulary is `insert`, `delete`, `format_run`, `format_paragraph`,
`format_table`, `format_row`, `format_cell`, `format_section`,
`opaque_interior`, and `move`. Records that exist but are not selectable
(`revision_id` 0) are omitted.

`POST /resolve` takes `{ "revision_ids": [...], "action": "accept" }` where
`action` is exactly `accept` or `reject` (anything else is `400` `BadAction`).
An empty `revision_ids` list is refused with `400` `EmptySelection`: an empty
selection is a caller mistake, never a silent no-op.

## Compare two documents into a redline

`POST /api/compare` discovers the deltas between two **already-uploaded**
documents and materializes them as tracked changes in a **new** stored
document. A base/target pair therefore collapses into one reviewable redline
that the rest of the API drives:

```jsonc
// POST /api/compare
{ "base_doc_id": "doc-1", "target_doc_id": "doc-2", "author": "L. Marsh" }

// 200 OK
{ "doc_id": "doc-3", "document": { "blocks": [ /* the read view */ ] } }
```

The returned `doc_id` is a first-class session document: `/revisions`,
`/resolve`, and `/export` compose with it exactly as with an uploaded file. The
engine's round-trip contract holds. **Reject-all reconstructs `base`, and
accept-all reconstructs `target`.** Therefore, `GET /api/documents/doc-3/export?mode=rejected`
returns the base and `mode=accepted` returns the target.

- **Attribution.** The optional `author` field attributes the discovered
  revisions. Omit it and the redline is anonymous because the Tier-1 `diff`
  carries no authoring identity. Include it and every revision is attributed to
  that name (the Tier-1 `diff_as`), surfacing as each row's `author` under
  `/revisions`. A present-but-empty `author` is a client mistake, not a request
  for an anonymous redline: it returns `400` `BadAuthor` (omit the field
  instead). There is no silent fallback to anonymous.
- **Unknown ids.** An unknown `base_doc_id` or `target_doc_id` returns `404`
  `UnknownDocument`, the same path every id-addressed endpoint uses.
- **Same document.** Comparing a document against itself is allowed and honest:
  `diff` finds no deltas, so the new document is an empty redline (its
  `/revisions` list is empty; accept-all and reject-all read identically).

To accept or reject **all of one author's** changes, compose the existing
verbs: `GET /revisions` returns each row's `author`, so filter to that author
client-side and pass the resulting `revision_ids` to `POST /resolve`. There is
no by-author selector on `/resolve` itself. Resolution is by explicit id, and
`/revisions` already carries the author to filter on.

## Continue an existing author's work

`/apply` enforces the same author-impersonation guard as every other
transport: a transaction whose `revision.author` already owns pending
revisions in the document is refused with `422` `AuthorImpersonation`. Every
multi-round session hits this on round two. Continuing that author's own work
is a per-call assertion, made as a query parameter, never a transaction
field:

```jsonc
// POST /api/documents/doc-1/apply?allow_existing_author=true
{
  "ops": [ /* ... */ ],
  "revision": { "author": "J. Osei" }   // same author as round one
}
```

Without the parameter, use a distinct author per round instead. There is no
silent continuation.

Every failure has the shape `{ code, error }` and an HTTP status. The `code` is
the engine's own refusal name (e.g. `StaleEdit`), so clients can branch on
it exactly as the [refusal vocabulary](mcp.md#refusal-vocabulary)
describes. Documents live in an in-memory session keyed by `doc_id`; the
durable artifacts are the uploaded file and what you export.

More detail: [stemma-api/README.md](https://github.com/stemma-sh/stemma/blob/main/stemma-api/README.md).
