# stemma-mcp

A [Model Context Protocol](https://modelcontextprotocol.io) adapter for applying
approved changes to an existing Word document. It exposes the `stemma` DOCX
engine over stdio so an agent can create native tracked changes without treating
the `.docx` as opaque bytes or flattened text.

> **Pre-1.0.** A `0.x` minor release may break API and wire contracts —
> deliberately, with changelog notice. The
> [stability policy](https://github.com/stemma-sh/stemma/blob/main/docs/guide/stability.md)
> states exactly what you can depend on today.

## Compact agent path

For the current product job, ignore the broad advanced surface and use:

```text
open_docx -> inspect_docx -> execute_plan -> verify_docx -> save_docx
```

`inspect_docx` defaults to the first page of a compact current index and selects bounded find or
window reads, a paged document projection with exact prose and bounded table summaries, exact paragraph block details,
pending revisions, styles, editable footnote/endnote rows, historical projection
views, or the complete parser-derived transaction-operation catalog. Table block reads return paged cell locators whose
paragraph ids provide exact retrieval; table finds return only matching cell
excerpts, not the whole table. `execute_plan` handles an atomic v4 transaction, an
explicitly non-atomic literal-replacement worklist with per-item outcomes, an
explicit accept/reject selection, or a two-file comparison producer plan through
the same typed kernels as advanced authoring and selective resolution. Receipts
omit whole-table content.
Worklist items use an integer `expected_matches` or `replace_all:true`; the two
states are separate at the wire edge so strict function callers never guess a
`number | "all"` union. Worklists support an exact throwaway preview, expose
typed `exact|normalize_ws` matching, and include table-cell paragraphs in the
default whole-body scope without flattening them. A cell paragraph `block_id`
can still restrict an ambiguous replacement.
`verify_docx` audits either the open session or a producer-neutral before/after
pair. Every audit list is paged at 16 rows by default (64 maximum), with totals
and continuation metadata rather than an unbounded structural-diff echo. Only
after its intent, untouched-scope, prior-revision, and validator checks pass
should `save_docx` commit to a new path.

For a multi-document instruction, the first task-bearing `open_docx` can bind
the complete replacement set and every input by hash. Earlier target saves stay
non-deliverable at task level; the final save writes a complete or partial
create-once manifest that `stemma verify-task` can check later without the MCP
session. The manifest is unsigned evidence: it cannot authenticate its producer
or detect intent omitted from the declaration. See the
[task-delivery guide](../docs/guides/verify-task-delivery.md).

The [CLI `stemma.worklist.v0` contract](../docs/reference/cli.md#apply) is the
canonical local interface. MCP remains a thin adapter; it does not define a
second plan or receipt standard.

## Why

A `.docx` is a ZIP of XML. Naive agent tooling either unzips and string-edits the
XML (fragile, corrupts the file) or extracts plain text (loses all structure and
can't write changes back). `stemma` parses the document into a typed IR, applies
edits as proper tracked changes (`w:ins`/`w:del`), and serializes back to a valid
DOCX. This server puts that behind a set of MCP tools (read/navigate, edit,
review).

The design goal is **fail-loud, model-first editing**: a stale or ambiguous edit
returns an actionable error instead of silently changing the wrong thing.

## Tools

The default `core` profile exposes 5 tools over the wire: `open_docx`,
`inspect_docx`, `execute_plan`, `verify_docx`, and `save_docx`. This
is the compact inspect/execute/verify flow with explicit open/save safety
boundaries. Set `STEMMA_MCP_PROFILE=advanced` to expose all 31 tools when an
individual expert escape hatch is needed. Unknown profile values fail
startup rather than silently selecting a surface.

### Open / save / compare

| Tool | What it does |
|---|---|
| `open_docx` | Open a workspace-confined `.docx`; returns a `doc_id`, exact `input_artifact` identity, and a compact `index`. It also hosts the optional complete task declaration (`task`) or a later target binding (`task_id`). |
| `save_docx` | Export an open doc (including tracked changes) to a new `.docx` path. Gates the bytes through the engine's post-serialization OOXML linker and refuses an existing destination or input alias. |
| `compare_docx` | Diff two `.docx` files and commit a redline to a new path (target with tracked changes vs base). |

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
| `find` | Find blocks whose visible text contains a pattern; returns bounded match-centered excerpts and resolves wording → block id for exact follow-up reading. |
| `get_section` | One heading and the blocks under it, as extended markdown (windowed reading). |
| `read_styles` | The un-resolved style table from `word/styles.xml`: document-default run props plus one row per style exactly as authored. Read this before a global re-skin (e.g. a font change) to learn whether body text inherits from `doc_default` or a named style. |
| `list_revisions` | Structured index of pending tracked changes (`{id, kind, author, text, date}`, filterable by author / kind / block range) — the id source for selective `accept_changes`/`reject_changes`. |

### Edit (tracked changes)

| Tool | What it does |
|---|---|
| `apply_edit` | Apply a v4 edit transaction as atomic tracked changes. Fails loudly on a stale `expect`/`semantic_hash`, a destroyed opaque inline, or an unsupported structure. |
| `replace_text` | Server-side tracked replace of one phrase: exact or whitespace/quote-normalized matching over body text, splicing through existing redlines; a match straddling an opaque anchor or tracked-change boundary is never half-applied. A zero-match error carries a `diagnosis` explaining why. |
| `replace_text_batch` | A list of `replace_text` items applied in order against live state, with per-item outcomes; `preview:true` runs the same sequence on a throwaway snapshot. A failed item never blocks the rest. A partial result is reviewable but incomplete. Omitted scope covers top-level and table-cell paragraphs; use exact counts and `on_barrier_match: "fail"`, and restrict by block only to disambiguate. |
| `replace_all` | Tracked find-and-replace across body paragraphs (one tracked rewrite per matching paragraph; opaque anchors preserved; barrier-straddle policy). |
| `apply_batch` | One v4 transaction with a `preview` switch (dry-run outline without persisting, or apply). |

### Review / verify

| Tool | What it does |
|---|---|
| `check_edit` | Dry-run a v4 transaction against a clone and discard it — `{would_apply}` or the same actionable error `apply_edit` would report. Mutates nothing. |
| `accept_changes` / `reject_changes` | Accept/reject tracked changes selected by id, author, block range, or all. Empty/unmatched selection fails loudly (never a silent no-op). |
| `validate_docx` | Export + run the package/wordprocessing/schema validators; returns `{ok, issues}`. Use after a series of edits. |
| `review_session` | Everything this session changed since `open_docx`, against the retained open-time baseline: census of new tracked changes (all stories), any direct (untracked) delta, disposition of pre-existing revisions, a proof every other block is untouched, input identities, and the validator verdict on the would-be save. Lists return 16 rows by default; use `detail`, `offset`, and `limit` (maximum 64) for the rest. Run before `save_docx`. Optional `render: {path}` also commits a create-new baseline-to-now redline. |
| `audit_docx` | The same explicitly paged report for any two workspace-confined `.docx` files, computed statelessly, with exact input identities. Optional `render: {path}` also commits a create-new before-to-after redline, subsuming `compare_docx`. |

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

The server takes no arguments and speaks JSON-RPC over stdio. The `.docx` paths
an agent opens and saves are passed as tool arguments
(`open_docx { "path": ... }`). Filesystem access is confined to
`STEMMA_MCP_WORKSPACE_ROOT`; there is no path argument that widens it. Logs go
to stderr (`RUST_LOG=stemma_mcp=debug` for more).

### Workspace and artifact boundary

`STEMMA_MCP_WORKSPACE_ROOT` names the directory the MCP server may read from
and write to. The server canonicalizes it once at startup. When the variable is
unset, the canonical startup current directory is the root. A relative root is
resolved from that startup directory. Startup fails loudly if the root cannot
be resolved to a directory or its canonical path cannot be represented exactly
in UTF-8 artifact receipts.

Tool paths may be relative or absolute. Relative paths resolve under the root;
absolute paths must still resolve inside it. Existing source symlinks are
canonicalized, so a symlink that escapes the root is refused. Set the root to
the narrowest directory containing the documents and media the agent needs:

```bash
STEMMA_MCP_WORKSPACE_ROOT=/absolute/path/to/documents npx -y @stemma-sh/mcp
```

Every output path is create-new. `save_docx`, `compare_docx`, and optional
`review_session`/`audit_docx` renders refuse any existing destination and any
alias of a consumed input; this release has no overwrite override. Output bytes
are validated first, staged in the destination directory, committed without
clobbering, then read back and checked for exact byte length and SHA-256. A
successful response includes input/output artifact identity, collision policy
`create_new`, and disposition `created`, while retaining the tool's existing
response keys. Every successful object response and structured error also
includes `server_version`; tagged binaries stamp it as
`version+g<12-character-commit>`.

For an image supplied by `path`, the edit receipt returns its exact source
identity only when the runtime mutation applied. Stemma couples mutation,
source registration, and save/review export so an exported session cannot omit
an applied image's source. Repeated sources with the same resolved path, byte
length, and SHA-256 are deduplicated in the session registry. The registry has
no independent TTL: eviction removes its row only after the runtime confirms
the document handle is gone. Missing source state for a live handle fails closed
on save, review, and path-backed image persistence with
`artifact_session_state_missing` until the document is reopened.

This is a fail-loud boundary for ordinary caller mistakes and failed writes,
not an operating-system sandbox. It does not defend against a hostile local
process running as the same user and racing filesystem paths, storage corruption,
or power loss. Staging and no-clobber commit provide create-new visibility; they
are not a claim of power-loss durability.

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
      "env": {
        "STEMMA_MCP_WORKSPACE_ROOT": "/absolute/path/to/documents"
      }
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
      "args": [],
      "env": {
        "STEMMA_MCP_WORKSPACE_ROOT": "/absolute/path/to/documents"
      }
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
env = { STEMMA_MCP_WORKSPACE_ROOT = "/absolute/path/to/documents" }
```

### VS Code

Command palette → "MCP: Add Server…" → "Command (stdio)" → enter the absolute
binary path → name it `stemma`. Set `STEMMA_MCP_WORKSPACE_ROOT` in the server's
environment configuration when the generated entry is not project-local.

### Any other MCP client

Every client that speaks stdio MCP needs the same four facts: transport
`stdio`, command = the absolute path to the built `stemma-mcp` binary, no
arguments, and an explicit `STEMMA_MCP_WORKSPACE_ROOT` for the documents it may
access. Consult your client's docs for where its server list and environment
live and add those values.

### Packaged installers

Two build scripts wrap the server as a drop-in bundle instead of hand-editing a
config:

- `mcpb/` — `build-mcpb.sh` produces a `.mcpb` bundle you drag-and-drop into
  Claude Desktop's extensions.
- `plugin/` — `build-plugin.sh` produces a Claude plugin `.zip` that bundles the
  server binary and its stdio wiring; install it as a plugin in your client's
  plugin settings. Agent guidance comes from the connected MCP server's
  initialize instructions and tool descriptions, just like the npm and MCPB
  distributions.

Both build the binary for the host you run on; see each directory for options.
The MCPB installer requires a **Document workspace** directory and injects it
as `STEMMA_MCP_WORKSPACE_ROOT`; the bundle has no permissive default. MCPB
packaging does not create an OS sandbox — the server still enforces this
application-level boundary itself. Plugin hosts must configure the same
environment variable explicitly; otherwise the plugin server uses its startup
directory.

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
and restarting the server drops all open handles. Five filesystem, lifecycle,
and resource settings are parsed once at startup (a malformed value is a
fail-loud startup error, not a silent fallback):

- **Workspace root.** `STEMMA_MCP_WORKSPACE_ROOT` confines reads and writes as
  described above. Unset means the canonical startup current directory.
- **Idle eviction.** Before every tool call the server evicts documents that have
  not been touched within `STEMMA_MCP_DOC_TTL_SECS` (default `86400`, i.e. 24h),
  so a long-lived host does not grow without bound. The default is deliberately
  generous — longer than any realistic single editing session — so a live session
  is never evicted mid-flight; set it lower for a busy multi-document host, or to
  `0` to disable eviction entirely. If you use a `doc_id` after its document was
  evicted, the tool returns a structured `doc_evicted` error telling you to
  re-open the file with `open_docx` (distinct from the `unknown_doc_id` error for
  a `doc_id` that was never opened). Coupled source-identity state has no second
  idle clock and is removed only after the runtime confirms the handle was
  evicted.
- **Open-size cap.** `open_docx` refuses a file larger than
  `STEMMA_MCP_MAX_DOC_BYTES` (default `52428800`, i.e. 50 MiB), checked against the
  file's metadata before it is read into memory. The `doc_too_large` error names
  the file size, the limit, and the variable to raise; set it to `0` to disable
  the cap.
- **Per-image path cap.** `STEMMA_MCP_MAX_IMAGE_BYTES` limits each image file
  read through an edit's `path` field (default `20971520`, i.e. 20 MiB).
- **Per-transaction image path cap.** `STEMMA_MCP_MAX_IMAGE_TOTAL_BYTES` limits
  the aggregate bytes read from image paths in one transaction (default
  `52428800`, i.e. 50 MiB).

Both image caps measure file bytes before base64 expansion, return
`artifact_source_too_large` when exceeded, and can independently be disabled
with `0`. Keep them enabled on untrusted workloads.

## Status / limits (PoC)

- The outline reports **block-level** tracking status. Inline tracked changes
  (text inserted/deleted inside a paragraph) live in segments and are present in
  the exported DOCX but not separately broken out in the read view yet.
- Edit breadth is whatever the v4 schema + engine support today. Unsupported cases
  (e.g. tables with merged cells) fail loudly with a named error rather than a
  best-effort mutation. Growing breadth is the point: see the engine's `EditStep`
  set in `stemma-engine/src/edit.rs`.
- Workspace confinement and no-clobber commits reduce accidental authority and
  mutation; they do not isolate the server from a hostile same-user process.
  See [SECURITY.md](../SECURITY.md).
