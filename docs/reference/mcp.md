# MCP tool reference

The default `core` profile exposes only `open_docx`, `inspect_docx`,
`execute_plan`, `verify_docx`, and `save_docx`. Set
`STEMMA_MCP_PROFILE=advanced` to opt into the complete 31-tool surface described
below. Unknown profile values fail startup. Every verb, its receipt, and the
refusals you may hit are documented here. **This page is self-contained by design —
it is written to be pasted whole into an agent's context.** Install/wiring
instructions live in [stemma-mcp/README.md](../../stemma-mcp/README.md).

## Golden path

1. `open_docx(path)` → `doc_id`, exact `input_artifact`, and the first 16 rows
   of a paged compact `index` (stable `id`, `role`, a 120-char `text_preview`,
   tracking `block_status`). `index_has_more` and `index_next_offset` make the
   bound explicit. Prefer `find` over walking every page. Exact block text,
   guards, durable opaque anchors, and table-cell coordinates are available
   through `inspect_docx` with `query:"block"`; pass `detail:"formatting"`
   when complete run spans, marks, and style properties are actually needed.
2. `inspect_docx(doc_id)` returns the first compact index page by default. Prefer
   `query:"find", pattern:"..."` to locate wording, then `query:"block"` for
   compact exact paragraph text, guards, nested table-cell paragraphs, lists,
   and opaque anchors. Its default omits repeated run-formatting objects explicitly and
   reports `formatting_available:true`; request `detail:"formatting"` for the
   complete formatting projection.
   Find is a locator: non-table matches carry a match-centered excerpt of at
   most 240 characters and an exact character count; inspect the returned id
   for full text. A table block inspection likewise omits the aggregate table body and returns
   eight cell locators by default. Every locator carries every paragraph id in
   that cell; page with `cell_offset`/`cell_limit` (maximum 64), then inspect a
   paragraph id for exact text, anchors, or formatting. This keeps the core read
   bounded without hiding content. A table find returns only matching cell
   excerpts and their paragraph ids;
   it returns at most four 120-character cell excerpts per matched table by
   default, with the true
   `matching_cell_count` and explicit continuation metadata. Page them with
   `cell_offset`/`cell_limit` when a broad pattern matches more; inspect a
   returned cell id when its bounded excerpt is insufficient.
   Find returns at most 16 matches by default and reports `count`, `returned`,
   `has_more`, and `next_offset`; pass `offset`/`limit` to page deliberately.
   Page `query:"index"` with `offset`/`limit` only when structural navigation
   requires it. Use `query:"window"` with inclusive block ids for context;
   `query:"document"` is a paged extended-Markdown projection (16 top-level
   blocks by default, 256 maximum) with exact prose and explicit bounded table
   summaries. Each table summary includes four addressable cell previews and
   routes to `query:"block"` for all cells, so one large table cannot defeat
   block paging. Query `revisions` for pending changes, `styles` for style
   state, or `notes` for editable footnote/endnote `{note_id,kind,text}` rows.
   Revision inspection accepts an AND-combined
   `filter:{by_author?,by_kind?,by_block_range?:{from_block_id,to_block_id}}`,
   which is the exact inventory to use before a selective resolution.
   `section` reads one heading subtree; `text`, `html`, `redline`, `accepted`,
   and `rejected` preserve the corresponding historical comprehension
   projections. `operations` returns the complete parser-derived transaction
   vocabulary, accepted top-level fields, operational cues, exact examples for
   the historically important shapes, and the route from every historical
   tool to the five-tool core. Pass `pattern:"edit_note"` (for example) to
   retrieve one operation instead of the whole catalog.
3. Submit one explicit v4 transaction or revision-resolution selection to
   `execute_plan` with `preview:true`. Resolve every refusal; then submit the same
   plan with `preview:false`. Preview and apply both return touched-block-only
   receipts, never a whole-document echo. A complete successful preview reports `apply_ready:true`
   and directs the caller to apply the identical plan; do not rediscover or
   reformulate it unless document state or intended scope changes. A
   transaction is atomic: all operations apply or none do. A resolution reports the exact selected
   revision ids, but is not a finalization step: ordinary fill/edit tasks leave
   both existing and newly authored changes pending. Accept or reject only when
   the user explicitly asks to resolve or clean revisions. For several literal
   substitutions, instead pass
   `replacement_worklist:{author,replacements:[...]}` first with
   `preview:true`, then with `preview:false`.
   Each item uses an integer `expected_matches` (default 1) or
   `replace_all:true`, never a stringified count and never both.
   `match_mode` is the typed enum `exact|normalize_ws`. The omitted scope covers
   top-level and table-cell paragraphs; a table find's matching cell paragraph
   id remains a valid `scope:{block_id}` for disambiguation. Both use the same
   formatting-preserving tracked splice. This reuses the tracked
   `replace_text_batch` kernel: it is deliberately non-atomic and reports every
   applied or failed item for exact re-issue. Preview runs the ordered worklist
   against a throwaway snapshot, including the effect of earlier successful
   items, without persisting it.
   For a two-file producer workflow, omit `doc_id` and pass
   `comparison:{base_path,target_path,out_path,author?}` with `preview:false`;
   this is the five-tool route for producing a tracked redline from two DOCX
   files.
4. `verify_docx({doc_id})` must report no unexpected direct delta, no
   untouched-scope violations, every pre-existing revision in its intended
   disposition, and `validator.new_issue_count: 0`. `baseline_validator`
   describes findings already present when the input was opened; unchanged
   baseline findings must be disclosed, but are not regressions and do not
   block an otherwise valid output. Audit sections expose the first 16
   rows plus `total`, `returned`, `has_more`, and `next_offset`; select
   `detail:"census"|"direct_delta"|"preexisting"|"violations"|"validator_issues"`
   with `offset`/`limit` (maximum 64) to retrieve every remaining row.
5. `save_docx(doc_id, path)` commits to a new path only after the complete plan
   and verification checks pass. An existing destination or input alias is
   refused; there is no overwrite option.

This is the compact product path. `execute_plan` is the same v4 transaction and
execution kernel as advanced `apply_batch`; `verify_docx` is the same audit
kernel as `review_session` and `audit_docx`. Enable `advanced` for individual
expert escape-hatch verbs, not for broader engine semantics. The narrow
[CLI worklist](cli.md#apply) remains the canonical approved-replacement process
contract.

## Filesystem and artifact boundary

The MCP server confines every filesystem path to
`STEMMA_MCP_WORKSPACE_ROOT`. The root is canonicalized once at startup. If the
variable is unset, the canonical startup current directory is used; a relative
root is resolved from that directory. An invalid or non-directory root is a
fail-loud startup error, as is a canonical root that cannot be represented
exactly in UTF-8 artifact receipts.

Relative tool paths resolve under the workspace root. Absolute paths are
accepted only when they resolve inside it. Reads canonicalize existing paths,
so a source symlink that resolves outside the workspace is refused. This covers
DOCX inputs to open/compare/audit, image paths supplied to edit operations, and
other server-side file reads. Output parents must also resolve inside the root.

MCP and CLI output commits are **create-new only** in this release:

- any existing destination is refused, even when it is not an input;
- any destination that aliases a consumed input is refused;
- there is no overwrite flag or per-call override;
- validated bytes are staged in the destination directory and verified before
  commit;
- commit does not clobber an existing path, including one created concurrently;
- committed bytes are read back and verified by byte length and SHA-256 before
  success is returned.

The artifact boundary reports exact identity additively. Existing response keys
remain. An input identity has this shape:

```json
{
  "role": "input_docx",
  "supplied_path": "draft.docx",
  "resolved_path": "/workspace/draft.docx",
  "digest": { "algorithm": "sha256", "hex": "<64 lowercase hex chars>" },
  "bytes": 12345
}
```

An `output_artifact` wraps the same identity:

```json
{
  "identity": {
    "role": "output_redline",
    "supplied_path": "result.docx",
    "resolved_path": "/workspace/result.docx",
    "digest": { "algorithm": "sha256", "hex": "<64 lowercase hex chars>" },
    "bytes": 23456
  },
  "collision_policy": "create_new",
  "disposition": "created"
}
```

Artifact identities contain absolute resolved paths and exact-byte hashes.
Treat them as document-sensitive metadata; do not publish receipts or logs
without an explicit retention and redaction decision.

Identity paths are serialized as JSON strings. If a supplied path or its
canonical target is not valid UTF-8, Stemma refuses the read or commit before
reading source bytes or staging an output; it never panics or emits a lossy path.
Windows alternate-data-stream path syntax is also refused on every platform
before read or staging, so an apparent output cannot attach a named stream to
an input file and the path contract does not change by host.
Sources must be regular files: obvious FIFOs, devices, and directories are
rejected before open and the opened handle is checked again.

Every successful object response and every structured error includes
`server_version`. Tagged binaries report `version+g<12-character-commit>` so a
receipt can be tied to the exact release build that produced it.

Response additions by tool:

| Tool | Additive artifact keys |
|---|---|
| `open_docx` | `input_artifact` |
| `save_docx` | `input_artifacts`, `output_artifact` |
| `compare_docx` | `input_artifacts`, `output_artifact` |
| `audit_docx` | `input_artifacts`; when rendered, `render.output_artifact` |
| `review_session` | `input_artifacts`; when rendered, `render.output_artifact` |
| successful image-backed `apply_edit` / commit-mode `apply_batch` | `input_artifacts` for persisted image sources |

Path-backed image identities are registered only after the runtime reports that
the mutation applied. The mutation, registration, and save/review export paths
share one session gate, so an export cannot observe an applied image without its
source identity. The session registry deduplicates the same resolved path,
byte length, and SHA-256 identity. It has no independent TTL: after each runtime
eviction sweep, registry state is removed only for handles the runtime confirms
are gone. A live handle with missing source state fails closed on save, review,
or path-backed image persistence. `check_edit`, edits rejected before mutation,
and preview-mode `apply_batch` may read an image to evaluate the transaction but
do not persist or register it and therefore do not return `input_artifacts` for
that preview.

Server-read image `path` inputs have two startup-configured limits. Both count
the source bytes before Stemma expands them to base64; setting either value to
`0` disables that limit:

| Variable | Scope | Default |
|---|---|---|
| `STEMMA_MCP_MAX_IMAGE_BYTES` | Each image path | `20971520` bytes (20 MiB) |
| `STEMMA_MCP_MAX_IMAGE_TOTAL_BYTES` | Aggregate image-path bytes in one transaction | `52428800` bytes (50 MiB) |

Exceeding either cap returns `artifact_source_too_large`. Reduce the image or
split an aggregate-heavy transaction; only a host operator should raise or
disable these process-wide guards. The caps also apply when `check_edit` or
preview-mode `apply_batch` reads path-backed images, even though those calls
register no source identities.

Artifact failures use stable transport codes:

| Code | Meaning |
|---|---|
| `artifact_outside_workspace` | A path resolves outside the configured MCP root |
| `artifact_output_exists` | The create-new destination already exists |
| `artifact_protected_source` | The destination aliases an input protected by the operation or session |
| `artifact_source_too_large` | An image path exceeds the per-image cap, or image paths exceed the aggregate transaction cap |
| `artifact_read_failed` | A source cannot be resolved, represented in a portable receipt, opened, identified, or read safely |
| `artifact_commit_failed` | A destination cannot be represented in a portable receipt, or staging/no-clobber commit/post-commit verification fails |
| `artifact_session_state_missing` | A live document has lost its protected source registry, so save, review, and path-backed image persistence are refused until it is reopened |

`open_docx` retains its existing `doc_too_large` code for the document-size cap.

This boundary protects against ordinary caller mistakes and failed writes. It
does not protect against a hostile local process running as the same user and
racing paths, storage corruption, or power loss. The same-directory staging and
no-clobber commit contract concerns visibility and collision safety, not
power-loss durability.

## Tools

| Read / navigate | |
|---|---|
| `open_docx(path)` | Open a workspace-confined source; returns `doc_id`, a compact per-block `index`, and `input_artifact` identity (no full text / `semantic_hash` — use `read_outline` / `read_block` for those). |
| `read_outline` / `read_index` | Current outline / lightweight per-block index (id, role, 120-char preview, lengths, status). |
| `read_window(from_block_id,to_block_id,format)` | Render a block-id range as text/markdown/html. |
| `read_markdown` / `read_text` / `read_html` | Whole-document projections; markdown carries block ids and `<ins>`/`<del>`. |
| `read_block(block_id)` | One block's spans in detail: text spans carry a `handle` (`s_0`…) + `guard` for span-level edits; opaque spans carry durable anchor ids. Re-read after any edit to the same block — handles and guards go stale. |
| `read_redline` / `read_accepted` / `read_rejected` | As-it-stands / accept-all / reject-all projections. Read-only, never mutate. |
| `find(pattern)` | Blocks whose visible text matches → ids. |
| `get_section(heading_id)` | One heading + its blocks. |
| `read_styles` | The style table as authored, incl. document defaults. Read before a global re-skin. |
| `list_revisions` | Pending tracked changes: `{revision_id, kind, author, block_id, excerpt}`, filterable. The ONLY id source for selective accept/reject — ids are assigned at import; never reuse ids from raw XML or a previous session. |

| Edit (tracked by default) | |
|---|---|
| `replace_text(doc_id, old, new, author, scope?, expected_matches?, match_mode?)` | Server-side tracked replace of one phrase, splicing through existing redlines. `expected_matches` defaults to 1; a count mismatch fails listing every match `{block_id, excerpt}` so you disambiguate in one step; pass `"all"` to replace everywhere. `match_mode:"normalize_ws"` folds NBSP/typographic spaces and curly/straight quotes. The default body scope includes top-level and table-cell paragraphs; a single-block scope can disambiguate either. |
| `replace_text_batch(doc_id, replacements[], author, preview?)` | A worklist of replace_text items applied in order against live state, or against a throwaway snapshot with `preview:true`; per-item outcomes; a failed item never blocks the rest (deliberately non-atomic). |
| `replace_all(doc_id, needle, replacement, scope?, case_sensitive?, whole_word?, on_barrier_match?)` | Tracked find/replace across all body paragraphs; takes no `author` (changes are stamped with the fixed `stemma` identity — use `replace_text` when you need a named author). Refuses paragraphs that already carry tracked changes; `replace_text` splices through them. |
| `apply_edit(doc_id, transaction, mode?, allow_existing_author?)` | Atomic v4 transaction (see below). `mode:"direct"` applies untracked. |
| `apply_batch(doc_id, transaction, preview, mode?, allow_existing_author?)` | Same, with a required dry-run `preview` switch (true = preview, false = apply). |
| `check_edit(doc_id, transaction)` | Full dry-run: `{would_apply}` or the exact error `apply_edit` would raise. Mutates nothing. |

| Review / verify / export | |
|---|---|
| `accept_changes` / `reject_changes` | Resolve by selector (above). Accept keeps the new state; reject restores the prior state exactly. |
| `review_session(doc_id, render?, detail?, offset?, limit?)` | Everything THIS session changed (census + any untracked delta), proof all other blocks are untouched, input identities, and validator verdict; lists are explicitly paged at 16 rows by default (64 maximum), and `render: {path}` optionally commits a create-new session redline and returns its identity. |
| `audit_docx(before_path, after_path, render?, detail?, offset?, limit?)` | The same paged report for ANY two workspace-confined files. Returns input identities; `render: {path}` also commits a create-new before-to-after redline and returns its identity. |
| `compare_docx(base_path, target_path, out_path, author?)` | Diff two workspace-confined files and commit a create-new redline with input/output identities. |
| `validate_docx` | `{ok, issues}` from the package/OOXML validators. |
| `save_docx(doc_id, path)` | Validate and commit create-new. Refuses corrupt bytes, an existing destination, or an input alias; retains `path`/`bytes_written` and adds input/output identity. |

## The edit transaction (v4)

```json
{
  "ops": [{
    "op": "replace",
    "target": "p_3",
    "expect": "strict liability",
    "content": { "type": "paragraph",
                 "content": [{ "type": "text", "text": "…new text…" }] }
  }],
  "revision": { "author": "J. Osei" },
  "summary": "Soften liability clause"
}
```

Op kinds: `replace` (whole block, or sub-paragraph with a `span` handle +
`guard` from `read_block`), `insert`, `delete`, `move`,
`set_format` / `set_para_format` / `set_cell_format` / `set_row_format` /
`set_table_format` (each a proper tracked `*PrChange`), `comment_create` /
`comment_reply` / `comment_resolve` / `comment_delete`, `insert_note` /
`edit_note` / `delete_note`, `insert_image` / `replace_image` (supply
EXACTLY one of `path` — workspace-confined and subject to the image-path caps
above — or `bytes_base64`; omit `cx`/`cy` to use the image's intrinsic size,
give one to scale the other by aspect ratio), `insert_cross_ref`, `set_numbering`
(numbered paragraphs need the `list: {num_id, ilvl}` field — copy `num_id`
from a sibling list item), `insert_bookmark`, `apply_style`,
`create_style` / `modify_style`, `insert_equation`, content-control wraps,
page-setup and header/footer ops, and `{"type":"toc"}` blocks in `insert`.

## Refusal vocabulary — every refusal names its escape hatch

| Refusal | Meaning | Do instead |
|---|---|---|
| `StaleEdit` / expect mismatch | The block changed since you read it (any write to a block invalidates its handles/guards). | Re-read the block, rebuild the op. |
| `NoOpEdit` | Your replacement equals the current text (often an NBSP/curly-quote mismatch — copy characters verbatim into `content`). | Re-read; fix the text. |
| `MatchCountMismatch` | `replace_text` found ≠ expected matches; the error lists each `{block_id, excerpt}`. | Narrow the needle with surrounding context, scope it, or pass `expected_matches:"all"`. |
| `AmbiguousAnchorAfterMove` | You anchored on a block moved earlier in the same transaction. | Anchor on the id the error names (the moved copy) or a stable neighbor. |
| `OpaqueDestroyed` | The edit would silently drop an image/field/anchor. | Edit around the opaque, or restructure to keep it; never retry verbatim. |
| `AuthorImpersonation` | Your author already has revisions in this document. | Use a distinct author, or pass `allow_existing_author: true` to deliberately continue that author's work. |
| `CommentAnchorOverlapsDeleted` | Comment anchor falls on text marked for deletion. | Anchor on text that stays, or resolve the deletion first. (Commenting a paragraph that merely *carries* tracked changes is fine.) |
| `ParagraphContainsTrackedSegments` | Text-splicing verbs (hyperlink, find/replace span ops) need an unambiguous coordinate space. | Resolve the paragraph's changes first, or use `replace_text`, which splices through redlines. |
| `StyleNotFound` | `apply_style` targets an undefined style. | `read_styles` for real ids, or `create_style` in the same transaction. |
| `InvalidRange` (empty selection) | Your accept/reject selector matched nothing. | Re-run `list_revisions`; check author spelling and current ids. |
| Marks schema error | Marks are objects: `[{"type":"bold"}]`, never `["bold"]`. Span replaces take plain text only — use a whole-paragraph replace for mark changes. | Fix the shape. |

## Session rules

- Sessions are non-interactive: never end on a question; make the
  documented-default choice and ALWAYS `save_docx` — unsaved edits are lost.
- Scope replacements to the tokens the instruction denotes; preserve
  adjacent qualifiers you weren't asked to touch.
- Before saving, read back the accept-all projection (or `review_session`)
  and diff it against the task.
- Documents live in memory keyed by `doc_id`; the durable artifacts are the
  input file (never modified) and each create-new output whose identity the
  server returns.

## Recipes

The canonical tasks, end to end. Same contract as the rest of this page:
self-contained, paste-ready.

Every output path in these recipes is assumed not to exist. On a rerun, choose a
new name; Stemma will not replace the prior output.


### Flatten a redline to a clean final

**Task**: "Turn this marked-up draft into a clean final — accept everything,
no revision machinery left in any part of the file."

```
open_docx        {"path": "draft-redline.docx"}          → doc_id, outline
accept_changes   {"doc_id": ..., "selector": {"by": "all"}}
validate_docx    {"doc_id": ...}                          → {"ok": true}
save_docx        {"doc_id": ..., "path": "final.docx"}
```

**What to check**: `accept_changes` fails loudly if there is nothing to
accept (never a silent no-op). After it, `read_redline` shows no `<ins>` or
`<del>` anywhere — including footnotes and tables. To *discard* the proposed
changes instead of keeping them, the sequence is identical with
`reject_changes`.

**Why it's harder than it looks**: accepting a deleted paragraph mark must
merge two paragraphs; accepting table-row changes must keep the grid legal.
The engine owns those semantics — this recipe never touches XML.


### Selective resolution

**Task**: "Accept everything from L. Marsh — including her footnote edits —
but leave T. Byrne's changes pending for the next round."

```
open_docx        {"path": "grant-proposal.docx"}
list_revisions   {"doc_id": ...}
   → [{revision_id: 110, author: "L. Marsh", kind: "delete", ...},
      {revision_id: 107, author: "T. Byrne", kind: "insert", ...}, ...]
accept_changes   {"doc_id": ..., "selector": {"by": "by_author", "author": "L. Marsh"}}
validate_docx    {"doc_id": ...}
save_docx        {"doc_id": ..., "path": "grant-proposal-round2.docx"}
```

**What to check**: `list_revisions` afterwards shows only T. Byrne's
revisions still pending, untouched. Resolution reaches every story — a
footnote-body revision resolves exactly like a body one.

**Rules that matter here**: use the `revision_id`s this session's
`list_revisions` returned (ids are assigned at import — never reuse ids from
raw XML or an earlier session). For finer control than by-author, collect
ids and use one `{"by": "by_ids", "revision_ids": [...]}` call per
disposition.


### Produce a redline from two files

**Task**: "Here's the agreement we sent and the version they returned with
silent edits — give me a proper tracked-changes comparison."

```
compare_docx  {"base_path": "as-sent.docx", "target_path": "as-returned.docx",
               "out_path": "what-they-changed.docx"}
```

One call. The output is the target document with every difference expressed
as tracked changes against the base — open it in Word and step through
accept/reject like any reviewer redline.

**What to check**: open the output with `open_docx` + `list_revisions` to
enumerate what changed, or gate it with `validate_docx`. For a structured
report instead of (or alongside) a redline, `audit_docx(before_path,
after_path)` returns the change census, an untouched-blocks proof, and a
validator verdict — the certification form of the same comparison.


### The negotiation loop

**Task**: "Accept opposing counsel's changes in the Fees and Service Levels
sections, reject their change that cuts the liability cap to six months,
leave our reviewer's edits pending, and add our counter — twenty-four
months — as a tracked change with a comment explaining why."

```
open_docx        {"path": "agreement-their-markup.docx"}
list_revisions   {"doc_id": ...}                → ids by author + kind
accept_changes   {"doc_id": ..., "selector": {"by": "by_ids", "revision_ids": [<their fee/SLA ids>]}}
reject_changes   {"doc_id": ..., "selector": {"by": "by_ids", "revision_ids": [<the cap change id>]}}
replace_text     {"doc_id": ..., "old": "twelve (12) months",
                  "new": "twenty-four (24) months", "author": "J. Osei"}
apply_edit       {"doc_id": ..., "transaction": {"ops": [{
                    "op": "comment_create", "target": "<liability clause id>",
                    "expect": "aggregate liability",
                    "body": "Six months does not cover our exposure; we propose twenty-four.",
                    "author": "J. Osei"}],
                    "revision": {"author": "J. Osei"}}}
read_accepted    {"doc_id": ...}    ← review: is this what we meant to send?
validate_docx    {"doc_id": ...}
save_docx        {"doc_id": ..., "path": "agreement-our-counter.docx"}
```

**What to check**: the rejected change's clause reads its *original* text
(reject restores prior state — marker absence alone proves nothing); the
counter rides as a pending tracked change by the new author; the comment is
anchored on the clause. Commenting a paragraph that carries tracked changes
is supported — the natural edit-then-comment order works.

**Identity**: the counter's author must be distinct from the document's
existing reviewers, or the engine refuses (`AuthorImpersonation`) to keep
your changes distinguishable from theirs.
