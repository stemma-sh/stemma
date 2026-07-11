# stemma-examples

A **Word frontend** on the `stemma` engine — a single browser editor
([`editor/`](editor/)) that opens a `.docx`, renders it with its real formatting
(fonts, sizes, colors, highlights, alignment, inline images, real tables,
equations, page geometry) from stemma's `/rich` projection, and edits it as
tracked or direct changes with a B/I/U/S toolbar. It runs on the
[`stemma-api`](../stemma-api) backend and a thin client core, and goes beyond the
in-crate `cargo run --example …` snippets in [`../stemma-engine/examples`](../stemma-engine/examples).

```bash
cargo run -p stemma-api      # serves it at http://127.0.0.1:3000  (→ /editor/)
```

`stemma-api` serves this directory's static files itself, so there is no second
server and no CORS. The first load fetches ProseMirror (and MathJax for
equations) from a CDN and caches it; no build step, no `node_modules`.

## The one principle the frontend obeys

**The model and the tracked-change semantics live in the engine, never in the
client.** stemma owns the document model, the *formatting* (fonts/colors/sizes/
alignment, surfaced via `/rich`), and the tracked-change semantics (`w:ins`/
`w:del`, accept/reject, author attribution, materialization — validated against
a real Word oracle). The browser is a *projection* plus an *input surface* that
emits typed transactions. ProseMirror holds no authoritative state and invents
no tracked-change logic of its own. Get this wrong and you've rebuilt an
editor-heritage stack that merely exports `.docx`; get it right and the client
is thin and the engine does the hard part.

This is why the editor is **block-grained**: stemma is block-addressed and
edits are typed transactions, so an edit commits as a `replace` op pinned by the
block's `guard` (a stale edit fails loud), not as a stream of keystrokes. A block
is **committable** only when a whole-block text `replace` round-trips it
faithfully — a plain paragraph/heading, no opaque inline, no structural list
prefix, no existing tracked change. Everything else renders (so the document
reads correctly) but is excluded from commit, so nothing is silently dropped:

- **Tables, images, fields, opaque content** → read-only placeholders; stemma
  preserves them byte-for-byte.
- **Numbered / lettered clauses** ("1.", "(a)") → the prefix renders as an inert
  chip; the block is excluded from a whole-block replace that would drop it.
- **Already-tracked blocks** → lock after their change. Layering a new edit over
  pending ins/del is the engine's *span-level* replace, which these demos don't
  drive.

## editor — the Word frontend

Open a `.docx`. It renders with its **real formatting** — fonts, sizes, colors,
highlights, alignment, inline images, **real tables**, and **equations (OMML →
MathML → MathJax)**, on a **page sized from the section properties** — all
sourced from `GET /api/documents/{id}/rich` (a thin serialize of stemma's
`FullDocViewResult`, plus the lean view's table cells and the page geometry).
Edit any paragraph/heading, then **Commit**. The mode toggle maps onto stemma's
`MaterializationMode`:

- **Suggesting** → `tracked_change`: edits become `<ins>`/`<del>` a reviewer can
  resolve. The edited block then locks (it now has pending changes).
- **Editing** → `direct`: edits are baked in with no redline; the block stays a
  clean, re-editable block.

**Export** gives you the document exactly as shown.

### Boundaries (honest)

- **Rendering is formatting-faithful, not pixel-exact.** stemma is not a layout
  engine — line breaking and pagination are Word's. The character/paragraph
  formatting is all here; the page geometry is not.
- **Editing is text-grained, with B/I/U/S authoring.** Typing rewrites a
  paragraph and commits as one guard-pinned `replace`; the **B/I/U/S toolbar**
  marks the selection and those marks ride along in the commit content, so
  bold/italic/underline/strike round-trip through the engine. A rewritten block's
  run-level *display* styling (font/color/size) still simplifies on commit — and
  a font/size/**color** picker is the next step, mapping onto the engine's
  `set_format` op (which sets color/highlight as a tracked `rPrChange`).
- Tables, images, numbered clauses, and already-tracked blocks render but aren't
  text-editable here.

**Commit is optimistic — it masks the server round-trip.** The instant you
commit, the editor renders the *predicted* result locally (in Suggesting mode,
the `<del>`old`</del><ins>`new`</ins>` redline; in Editing mode, the text is
already on screen), marks the block as syncing, and fires the transaction in the
background. When the engine acknowledges, the editor **reconciles**: it adopts
the authoritative read view for those blocks (the engine's precise word-level
redline + the new `guard`s) and clears the syncing state — patching *only* the
changed blocks in one mapped transaction, so the caret and scroll survive. If the
engine **rejects** (e.g. `StaleEdit`), the optimistic edit **rolls back** to its
baseline and the error surfaces loudly — never a silent divergence. On a server
800 ms away the redline still appears in ~10 ms.

The per-block `guard` is the rebase token: a committed block is **locked** until
its commit is acknowledged and its new guard returns, and commits drain one at a
time, so a block can never be committed against a stale guard. Because edits are
block-addressed, a commit to one block never disturbs the caret — or an in-flight
commit — in another. (Still single-writer: same-block concurrent edits from two
*actors* surface as `StaleEdit`, not an automatic merge; and local undo reverts
the editor view but not the committed engine state — both are deliberate
boundaries.)

## Structure

```
stemma-examples/
  index.html            redirect → /editor/
  shared/
    stemma-doc.js        rich schema, /rich → ProseMirror mapping, API client, decorations
    math.js              OMML → MathML → MathJax (equations)
    styles.css           styling
  editor/                the Word editor (imports shared)
  samples/              bundled .docx samples
```

## The API contract

The editor speaks the JSON endpoints served by
[`stemma-api`](../stemma-api/src/main.rs):

| Method & path | Body | Returns |
|---|---|---|
| `POST /api/documents` | raw `.docx` bytes | `{ doc_id, document }` |
| `GET  /api/documents/{id}` | — | `{ document }` |
| `POST /api/documents/{id}/apply` | a [v4 transaction](../stemma-engine/src/edit_v4.rs) | `{ document }` |
| `GET  /api/documents/{id}/rich` | — | `{ blocks, section }` (rich projection + page geometry) |
| `GET  /api/documents/{id}/revisions` | — | `{ revisions }` (pending tracked changes) |
| `POST /api/documents/{id}/resolve` | `{ revision_ids, action }` | `{ document }` (accept/reject) |
| `GET  /api/documents/{id}/export?mode=redline\|accepted\|rejected` | — | `.docx` |

`document` is `{ blocks: [ { id, role, level, guard, editable, text,
literal_prefix, segments } ] }`. Each `segment` is a text run (`text`, `marks`,
tracked `status`) or an opaque `anchor`. That is the whole surface the editor
needs: block ids and guards to address edits, segments to render the redline,
`literal_prefix` to render clause numbering honestly.

## Configuration

| Env var | Default | Meaning |
|---|---|---|
| `STEMMA_API_PORT` | `3000` | TCP port (binds `127.0.0.1`). |
| `STEMMA_API_STATIC_DIR` | this directory | Static asset root. |
