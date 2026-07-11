# stemma-api

An HTTP transport adapter for the [`stemma`](../stemma-engine) DOCX engine — the
symmetric sibling of [`stemma-mcp`](../stemma-mcp). Where the MCP server maps
*stdio* MCP calls onto the engine, this maps *HTTP/JSON* calls onto it, so a
browser front-end can drive a real `.docx` through the durable loop:

```
upload .docx → parse → read → edit (typed transaction) → serialize → .docx
```

> **Pre-1.0.** A `0.x` minor release may break API and wire contracts —
> deliberately, with changelog notice. The
> [stability policy](https://github.com/stemma-sh/stemma/blob/main/docs/guide/stability.md)
> states exactly what you can depend on today.

It is the backend for [`stemma-examples`](../stemma-examples) — a browser
authoring **editor** — and serves its static files itself, so the whole demo is
one command:

```bash
cargo run -p stemma-api          # then open http://127.0.0.1:3000
```

## Design

- **Thin edge.** The engine owns no durable state; this server keeps opened
  documents in an in-memory map keyed by a `doc_id`. A [`Document`] is a pure
  value — every verb returns a new one — so a write is "compute the next
  `Document`, store it back". Persist the saved `.docx` bytes (plus the
  transactions) for durability; the in-memory document is a hot cache.
- **Stable surface.** As a *new* consumer it depends on the Tier-1 facade
  ([`stemma::api::Document`]) for every verb, reaching the unstable engine API
  only to *decode* a transaction at the wire edge
  ([`stemma::edit_v4::parse_transaction`] — the same path `examples/quickstart.rs`
  uses). Parse at the edge; operate on the domain type. (`stemma-mcp` reaches
  deeper only because it predates the facade.)
- **Fail-loud.** A stale edit, unknown `doc_id`, bad export mode, or malformed
  transaction JSON returns a structured `{ code, error }` with an HTTP status —
  never a best-effort mutation. `code` is the engine's own (e.g. `StaleEdit`),
  so a client can branch on it.

## Endpoints

| Method & path | Body | Returns |
|---|---|---|
| `POST /api/documents` | raw `.docx` bytes | `{ doc_id, document }` |
| `POST /api/compare` | `{ base_doc_id, target_doc_id }` | `{ doc_id, document }` — a **new** redline document (reject-all == base, accept-all == target) |
| `GET  /api/documents/{id}` | — | `{ document }` |
| `POST /api/documents/{id}/apply` | a v4 transaction (JSON) | `{ document }` after apply |
| `GET  /api/documents/{id}/rich` | — | `{ blocks }` — the rich, render-faithful projection (fonts, sizes, colors, highlights, alignment, images, equations); a thin serialize of stemma's own `FullDocViewResult` + per-block guard |
| `GET  /api/documents/{id}/revisions` | — | `{ revisions }` (pending tracked changes) |
| `POST /api/documents/{id}/resolve` | `{ revision_ids, action }` | `{ document }` (accept/reject) |
| `GET  /api/documents/{id}/export?mode=redline\|accepted\|rejected` | — | `.docx` (download) |
| `GET  /*` | — | static files (the examples) |

`document` is the read view: `{ blocks: [ { id, role, level, guard, editable,
text, segments } ] }`. See [`../stemma-examples/README.md`](../stemma-examples/README.md)
for the full contract and the editor that consumes it.

## Configuration

CLI flags win over env vars; both win over the loopback defaults.

| Flag | Default | Meaning |
|---|---|---|
| `--host[=ADDR]` | `127.0.0.1` | Bind address. Bare `--host` binds `0.0.0.0` (all interfaces) so the server is reachable from another machine or across a container boundary; `--host=ADDR` for a specific one. |
| `--port=N` | `3000` | TCP port. |

```bash
cargo run -p stemma-api -- --host          # 0.0.0.0:3000, reachable off-box
cargo run -p stemma-api -- --host --port=8080
```

| Env var | Default | Meaning |
|---|---|---|
| `STEMMA_API_PORT` | `3000` | TCP port (overridden by `--port`). |
| `STEMMA_API_STATIC_DIR` | `../stemma-examples` | Directory served for non-`/api` paths. |
| `RUST_LOG` | `stemma_api=info,tower_http=info` | Tracing filter. |

## Scope

This is example/demo infrastructure (`publish = false`): a single-process,
in-memory, single-origin server. It has no auth, no TLS, no eviction, and binds
loopback only. For a hosted runtime, the engine's `SimpleRuntime` (with TTL
eviction) is the session primitive to build on.
