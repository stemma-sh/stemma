# stemma-mcp

A [Model Context Protocol](https://modelcontextprotocol.io) server that exposes the
`stemma` DOCX engine to agents over stdio. It is a proof of concept for driving
real, structure-aware Word edits from an agent (e.g. Claude Code) instead of
treating a `.docx` as opaque bytes or flattened text.

> **Pre-1.0.** A `0.x` minor release may break API and wire contracts —
> deliberately, with changelog notice. The
> [stability policy](https://github.com/stemma-sh/stemma/blob/main/docs/guide/stability.md)
> states exactly what you can depend on today.

## Why

A `.docx` is a ZIP of XML. Naive agent tooling either unzips and string-edits the
XML (fragile, corrupts the file) or extracts plain text (loses all structure and
can't write changes back). `stemma` parses the document into a typed IR, applies
edits as proper tracked changes (`w:ins`/`w:del`), and serializes back to a valid
DOCX. This server puts that behind a set of MCP tools (read/navigate, edit,
review).

The design goal is **fail-loud, model-first editing**: every edit is anchored to a
stable block id and pinned with an `expect` precondition. A stale or ambiguous
edit returns an actionable error instead of silently changing the wrong thing.

## Tools

The server exposes 28 tools over the wire. They split into a read/navigation tier
(comprehension), an edit tier (tracked changes), and a review tier (selective
accept/reject, validate, dry-run, session/stateless audit).

### Open / save / compare

| Tool | What it does |
|---|---|
| `open_docx` | Open a `.docx`; returns a `doc_id` and a compact `index` (one row per block: stable `id`, `role`, heading depth, a 120-char `text_preview`, char/byte length, and tracking `block_status`). |
| `save_docx` | Export an open doc (including tracked changes) to a `.docx` path. Gates the bytes through the engine's post-serialization OOXML linker before writing — refuses to persist a structurally-corrupt file. |
| `compare_docx` | Diff two `.docx` files and write a redline `.docx` (target with tracked changes vs base). |

### Read / navigate (comprehension)

| Tool | What it does |
|---|---|
| `read_outline` | Re-read the current outline (reflects edits already applied). |
| `read_index` | Lightweight structural index: one row per block (id, role, heading depth, 120-char preview, char/byte length, status) + totals. The navigation tier for a large document. |
| `read_window` | Render an inclusive block-id window `[from..to]` as `text` / `markdown` / `html`. Use after `read_index` to read a sub-range. |
| `read_markdown` | The whole document as id-bearing extended markdown (anchors, `<ins>`/`<del>`, marks). The surface to *understand* a document. |
| `read_block` | One block's spans in detail: text spans carry a `handle` (`s_0`, `s_1`, …) + marks + status; opaque spans carry a durable anchor id. Read this before a span-level edit. |
| `read_text` / `read_html` | Plain-text / HTML projections of the current document. |
| `read_redline` / `read_accepted` / `read_rejected` | Read the document as it stands, or the accept-all / reject-all projection (throwaway — never mutates the stored snapshot). |
| `find` | Find blocks whose visible text contains a pattern; resolves wording → block id. |
| `get_section` | One heading and the blocks under it, as extended markdown (windowed reading). |
| `read_styles` | The un-resolved style table from `word/styles.xml`: document-default run props plus one row per style exactly as authored. Read this before a global re-skin (e.g. a font change) to learn whether body text inherits from `doc_default` or a named style. |
| `list_revisions` | Structured index of pending tracked changes (`{id, kind, author, text, date}`, filterable by author / kind / block range) — the id source for selective `accept_changes`/`reject_changes`. |

### Edit (tracked changes)

| Tool | What it does |
|---|---|
| `apply_edit` | Apply a v4 edit transaction as atomic tracked changes. Fails loudly on a stale `expect`/`semantic_hash`, a destroyed opaque inline, or an unsupported structure. |
| `replace_text` | Server-side tracked replace of one phrase: exact or whitespace/quote-normalized matching over body text, splicing through existing redlines; a match straddling an opaque anchor or tracked-change boundary is never half-applied. A zero-match error carries a `diagnosis` explaining why. |
| `replace_text_batch` | A list of `replace_text` items applied in order against live state, with per-item outcomes; a failed item never blocks the rest (deliberately non-atomic). Use instead of N `replace_text` round trips. |
| `replace_all` | Tracked find-and-replace across body paragraphs (one tracked rewrite per matching paragraph; opaque anchors preserved; barrier-straddle policy). |
| `apply_batch` | One v4 transaction with a `preview` switch (dry-run outline without persisting, or apply). |

### Review / verify

| Tool | What it does |
|---|---|
| `check_edit` | Dry-run a v4 transaction against a clone and discard it — `{would_apply}` or the same actionable error `apply_edit` would report. Mutates nothing. |
| `accept_changes` / `reject_changes` | Accept/reject tracked changes selected by id, author, block range, or all. Empty/unmatched selection fails loudly (never a silent no-op). |
| `validate_docx` | Export + run the package/wordprocessing/schema validators; returns `{ok, issues}`. Use after a series of edits. |
| `review_session` | Everything this session changed since `open_docx`, against the retained open-time baseline: census of new tracked changes (all stories), any direct (untracked) delta, disposition of pre-existing revisions, a proof every other block is untouched, and the validator verdict on the would-be save. Run before `save_docx`. Optional `render: {path}` also writes the baseline→now delta as a redline `.docx`. |
| `audit_docx` | The same report for ANY two `.docx` files, computed statelessly — certify edits stemma didn't make (another tool, a human, a raw-XML agent). Optional `render: {path}` also writes the before→after redline, subsuming `compare_docx`. |

### The edit transaction (v4 schema)

`apply_edit` takes a `transaction` object. Minimal replace:

```json
{
  "ops": [
    {
      "op": "replace",
      "target": "p_3",
      "expect": "strict liability",
      "content": {
        "type": "paragraph",
        "content": [{ "type": "text", "text": "The Supplier's liability is limited to negligence." }]
      }
    }
  ],
  "revision": { "author": "Agent" },
  "summary": "Soften liability clause"
}
```

Op kinds span the engine's edit breadth: `replace` (whole-block or, with a `span`
handle, sub-paragraph), `insert`, `delete`, `move`, `set_attr`, `set_format`,
`set_para_format` (alignment / indentation / spacing / paragraph borders / shading,
as a tracked `w:pPrChange`), `set_cell_format` (one table cell's borders / shading /
width / vertical alignment / margins in place, as a tracked `w:tcPrChange`),
`set_row_format` (one table row's height / height rule in place, as a tracked
`w:trPrChange`),
`set_table_format` (a table's borders / width / default cell margins in place, as
a tracked `w:tblPrChange`),
`comment_create`/`comment_reply`/`comment_resolve`/`comment_delete`,
`insert_note`/`edit_note`/`delete_note`, `insert_image`/`replace_image`,
`insert_cross_ref`, `set_numbering`, `insert_bookmark`, `apply_style`,
`set_image_attrs` (resize / alt-text) and `set_image_layout` (crop / floating
position / text-wrap, both direct-untracked drawing display edits),
`create_style`/`modify_style`, `insert_equation`, `wrap_content_control`
(inline run-span `w:sdt`; optional `data_binding: { xpath, store_item_id,
prefix_mappings? }` emits a `w:dataBinding` and authors the backing
`customXml` datastore part) / `wrap_blocks_content_control` (block-level
`w:sdt` around a range of paragraphs/tables), page-setup
and header/footer ops, and more. Targets are block ids from the outline; `expect`
(or `guard`/`semantic_hash`) is the optimistic-concurrency guard. See
`stemma-engine/src/edit_v4.rs` for the full grammar and contract.

## Install

**From npm (released versions)** — prebuilt binaries for Linux, macOS, and
Windows behind a one-command launcher; no Rust toolchain needed:

```bash
npx -y @stemma-sh/mcp            # or: npm install -g @stemma-sh/mcp
```

**From source (this checkout)** — prerequisite: a Rust >= 1.91 toolchain:

```bash
cargo build -p stemma-mcp --release
# binary at target/release/stemma-mcp
```

The client stanzas below show the built-binary path. If you installed from
npm, substitute command `npx` with args `["-y", "@stemma-sh/mcp"]` wherever a
binary path appears.

## Wire it into Claude Code / Claude Desktop

The server takes no arguments and speaks JSON-RPC over stdio. The `.docx` paths an
agent opens/saves are passed as tool arguments (`open_docx { "path": ... }`), so
there is no doc-dir flag to configure — the agent reads and writes wherever it has
filesystem access. Logs go to stderr (`RUST_LOG=stemma_mcp=debug` for more).

### Claude Code (CLI)

Register the built binary by absolute path:

```bash
# project scope (writes .mcp.json in the repo, shared with collaborators)
claude mcp add stemma --scope project -- /absolute/path/to/target/release/stemma-mcp

# or user scope (just for you, all projects)
claude mcp add stemma --scope user -- /absolute/path/to/target/release/stemma-mcp

# installed from npm instead:
claude mcp add stemma --scope user -- npx -y @stemma-sh/mcp
```

Verify it is connected:

```bash
claude mcp list          # shows "stemma  ✓ connected"
```

### `.mcp.json` (project-scoped, drop-in)

`claude mcp add --scope project` writes this; you can also create it by hand at the
repo root. Replace the path with your built binary:

```json
{
  "mcpServers": {
    "stemma": {
      "type": "stdio",
      "command": "/absolute/path/to/target/release/stemma-mcp",
      "args": [],
      "env": {}
    }
  }
}
```

The same stdio stanza works as-is in Cursor — drop it into `~/.cursor/mcp.json`.

### Claude Desktop (`claude_desktop_config.json`)

Same stanza under `mcpServers` (macOS:
`~/Library/Application Support/Claude/claude_desktop_config.json`, Windows:
`%APPDATA%\Claude\claude_desktop_config.json`). Restart Claude Desktop after editing.

```json
{
  "mcpServers": {
    "stemma": {
      "command": "/absolute/path/to/target/release/stemma-mcp",
      "args": []
    }
  }
}
```

Then ask Claude to open a `.docx`, inspect it (`read_markdown` / `read_index`),
find a clause (`find`), make a tracked change (`apply_edit`), check it
(`validate_docx`), and save it (`save_docx`).

### Codex CLI (`~/.codex/config.toml`)

```toml
[mcp_servers.stemma]
command = "/absolute/path/to/target/release/stemma-mcp"
args = []
```

### VS Code

Command palette → "MCP: Add Server…" → "Command (stdio)" → enter the absolute
binary path → name it `stemma`.

### Any other MCP client

Every client that speaks stdio MCP needs the same three facts: transport
`stdio`, command = the absolute path to the built `stemma-mcp` binary, no
arguments. Consult your client's docs for where its server list lives and drop
those in.

### Packaged installers

Two build scripts wrap the server as a drop-in bundle instead of hand-editing a
config:

- `mcpb/` — `build-mcpb.sh` produces a `.mcpb` bundle you drag-and-drop into
  Claude Desktop's extensions.
- `plugin/` — `build-plugin.sh` produces a Claude plugin `.zip` that bundles the
  server binary, its stdio wiring, and the cold-start skill; install it as a
  plugin in your client's plugin settings.

Both build the binary for the host you run on; see each directory for options.

## Smoke test

Drives the full protocol end to end over real stdio (handshake, **full tool-surface
registration check**, open, edit, stale-edit rejection, save, reopen-and-verify):

```bash
cargo build -p stemma-mcp
python3 stemma-mcp/smoke_test.py target/debug/stemma-mcp stemma-examples/samples/safe-agreement.docx
```

The input can be any prose `.docx` with an editable body paragraph; the script
picks the first one, edits it, and reopens the saved copy to verify.

## Lifecycle

Documents live in an in-memory `SimpleRuntime` keyed by `doc_id`; there is no
persistence, so the durable artifacts are the input `.docx` and the saved output,
and restarting the server drops all open handles. Two knobs bound resource use,
both parsed once at startup (a malformed value is a fail-loud startup error, not a
silent fallback):

- **Idle eviction.** Before every tool call the server evicts documents that have
  not been touched within `STEMMA_MCP_DOC_TTL_SECS` (default `86400`, i.e. 24h),
  so a long-lived host does not grow without bound. The default is deliberately
  generous — longer than any realistic single editing session — so a live session
  is never evicted mid-flight; set it lower for a busy multi-document host, or to
  `0` to disable eviction entirely. If you use a `doc_id` after its document was
  evicted, the tool returns a structured `doc_evicted` error telling you to
  re-open the file with `open_docx` (distinct from the `unknown_doc_id` error for
  a `doc_id` that was never opened).
- **Open-size cap.** `open_docx` refuses a file larger than
  `STEMMA_MCP_MAX_DOC_BYTES` (default `52428800`, i.e. 50 MiB), checked against the
  file's metadata before it is read into memory. The `doc_too_large` error names
  the file size, the limit, and the variable to raise; set it to `0` to disable
  the cap.

## Status / limits (PoC)

- The outline reports **block-level** tracking status. Inline tracked changes
  (text inserted/deleted inside a paragraph) live in segments and are present in
  the exported DOCX but not separately broken out in the read view yet.
- Edit breadth is whatever the v4 schema + engine support today. Unsupported cases
  (e.g. tables with merged cells) fail loudly with a named error rather than a
  best-effort mutation. Growing breadth is the point: see the engine's `EditStep`
  set in `stemma-engine/src/edit.rs`.
