# HTTP API reference

`stemma-api` maps the same verbs as the MCP server onto HTTP/JSON. The
transaction grammar, receipts, and refusal vocabulary are identical to the
[MCP reference](mcp.md); only the transport differs.

> **Scope.** This is example/demo infrastructure: a single-process, in-memory,
> single-origin server. It has no auth, no TLS, and no session eviction; it
> binds loopback only; and documents live only in RAM (keyed by `doc_id`) until
> the process exits. For a hosted runtime, build on the engine's own session
> primitive (`SimpleRuntime`, with TTL eviction) rather than this adapter. See
> [stemma-api/README.md](../../stemma-api/README.md#scope).

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
| `POST /api/documents/{id}/apply` | a v4 transaction (JSON) | `{ document }` after apply |
| `GET  /api/documents/{id}/rich` | none | `{ blocks }`, a render-faithful projection with fonts, colors, images, equations, and a per-block guard |
| `GET  /api/documents/{id}/revisions` | none | `{ revisions }`, containing pending tracked changes |
| `POST /api/documents/{id}/resolve` | `{ revision_ids, action }` | `{ document }` (accept/reject) |
| `GET  /api/documents/{id}/export?mode=redline\|accepted\|rejected` | none | `.docx` download |

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

Every failure has the shape `{ code, error }` and an HTTP status. The `code` is
the engine's own refusal name (e.g. `StaleEdit`), so clients can branch on
it exactly as the [refusal vocabulary](mcp.md#refusal-vocabulary)
describes. Documents live in an in-memory session keyed by `doc_id`; the
durable artifacts are the uploaded file and what you export.

More detail: [stemma-api/README.md](../../stemma-api/README.md).
