# HTTP API reference

`stemma-api` maps the same verbs as the MCP server onto HTTP/JSON — the
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
runs on it — a Word-style front-end (no build step — plain static files;
the first load fetches ProseMirror and MathJax from public CDNs) that
renders real formatting, edits in Suggesting/Editing mode, and
accepts/rejects tracked changes. It's the fastest way to watch the engine
work, and its source (`stemma-examples/editor`) doubles as a reference
client for the endpoints below.

## Endpoints

| Method & path | Body | Returns |
|---|---|---|
| `POST /api/documents` | raw `.docx` bytes | `{ doc_id, document }` |
| `POST /api/compare` | `{ base_doc_id, target_doc_id, author? }` | `{ doc_id, document }` — a **new** redline document |
| `GET  /api/documents/{id}` | — | `{ document }` |
| `POST /api/documents/{id}/apply` | a v4 transaction (JSON) | `{ document }` after apply |
| `GET  /api/documents/{id}/rich` | — | `{ blocks }` — render-faithful projection (fonts, colors, images, equations) + per-block guard |
| `GET  /api/documents/{id}/revisions` | — | `{ revisions }` — pending tracked changes |
| `POST /api/documents/{id}/resolve` | `{ revision_ids, action }` | `{ document }` (accept/reject) |
| `GET  /api/documents/{id}/export?mode=redline\|accepted\|rejected` | — | `.docx` download |

## Compare two documents into a redline

`POST /api/compare` discovers the deltas between two **already-uploaded**
documents and materializes them as tracked changes in a **new** stored
document — so a base/target pair collapses into one reviewable redline that the
rest of the API drives:

```jsonc
// POST /api/compare
{ "base_doc_id": "doc-1", "target_doc_id": "doc-2", "author": "L. Marsh" }

// 200 OK
{ "doc_id": "doc-3", "document": { "blocks": [ /* the read view */ ] } }
```

The returned `doc_id` is a first-class session document: `/revisions`,
`/resolve`, and `/export` compose with it exactly as with an uploaded file. The
engine's round-trip contract holds — **reject-all reconstructs `base`,
accept-all reconstructs `target`** — so `GET /api/documents/doc-3/export?mode=rejected`
returns the base and `mode=accepted` returns the target.

- **Attribution.** The optional `author` field attributes the discovered
  revisions. Omit it and the redline is anonymous (the Tier-1 `diff` — discovery
  carries no authoring identity); include it and every revision is attributed to
  that name (the Tier-1 `diff_as`), surfacing as each row's `author` under
  `/revisions`. A present-but-empty `author` is a client mistake, not a request
  for an anonymous redline: it returns `400` `BadAuthor` (omit the field
  instead) — there is no silent fallback to anonymous.
- **Unknown ids.** An unknown `base_doc_id` or `target_doc_id` returns `404`
  `UnknownDocument`, the same path every id-addressed endpoint uses.
- **Same document.** Comparing a document against itself is allowed and honest:
  `diff` finds no deltas, so the new document is an empty redline (its
  `/revisions` list is empty; accept-all and reject-all read identically).

To accept or reject **all of one author's** changes, compose the existing
verbs: `GET /revisions` returns each row's `author`, so filter to that author
client-side and pass the resulting `revision_ids` to `POST /resolve`. There is
no by-author selector on `/resolve` itself — resolution is by explicit id, and
`/revisions` already carries the author to filter on.

Failure shape everywhere: `{ code, error }` with an HTTP status — `code` is
the engine's own refusal name (e.g. `StaleEdit`), so clients can branch on
it exactly as the [refusal vocabulary](mcp.md#refusal-vocabulary--every-refusal-names-its-escape-hatch)
describes. Documents live in an in-memory session keyed by `doc_id`; the
durable artifacts are the uploaded file and what you export.

More detail: [stemma-api/README.md](../../stemma-api/README.md).
