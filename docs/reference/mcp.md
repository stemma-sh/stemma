# MCP core reference

Use this page to look up the default five-tool MCP contract. For installation
and a first edit, start with [Use Stemma with an agent](../guides/use-with-an-agent.md).

The default `core` profile exposes:

```text
open_docx -> inspect_docx -> execute_plan -> verify_docx -> save_docx
```

Set `STEMMA_MCP_PROFILE=advanced` only when a task needs an expert escape hatch.
The complete 31-tool surface is in the
[advanced reference](mcp-advanced.md). Unknown profile values fail startup.

## Tools

| Tool | Purpose |
|---|---|
| `open_docx(path, task?, task_id?)` | Open one workspace-confined source. Returns `doc_id`, exact `input_artifact`, and the first compact index page. The first task target may declare a complete multi-file task; later targets name its `task_id`. |
| `inspect_docx(doc_id, ...)` | Locate and read bounded document content, revisions, styles, notes, projections, or operation schemas. |
| `execute_plan(...)` | Preview or apply one transaction, replacement worklist, revision selection, or two-file comparison. |
| `verify_docx(...)` | Audit an open session or a before/after pair before delivery. |
| `save_docx(doc_id, path)` | Validate and commit an open document to a new path. |

Every successful object response and structured error includes
`server_version`. Tagged binaries report
`version+g<12-character-commit>`.

## Golden path

1. Call `open_docx(path)`. Keep the returned `doc_id` and exact input identity.
2. Use `inspect_docx` to find relevant content, then inspect the exact block.
3. Submit an explicit plan to `execute_plan` with `preview:true`.
4. Resolve every refusal, then submit the same plan with `preview:false`.
5. Call `verify_docx({doc_id})` and inspect every reported section.
6. Call `save_docx(doc_id, path)` with a new output path. Save reruns the
   session audit and refuses a
   non-deliverable result before creating the output path. A passing result
   is serialized, checked by the package gate, and committed create-new.

A complete preview reports `apply_ready:true`. Do not rediscover or reformulate
the plan between preview and apply unless the document state or intended scope
changed.

## Multi-document task delivery

When success depends on several outputs, declare the complete task on the first
task-bearing `open_docx`. The declaration fixes every read-only input, target,
and exact-count replacement before any task mutation. Task-bound worklist items
must name a matching `effect_id`; other mutation shapes are refused. Earlier
target saves remain non-deliverable task state. The last target save emits a
create-once complete or partial manifest.

The manifest carries checkable evidence, trusting Stemma emitted it. It is
unsigned and cannot prove the caller declared everything the human intended.
See [Verify a multi-document task delivery](../guides/verify-task-delivery.md)
for the wire examples, partial semantics, offline command, and trust limits.

## Inspecting a document

`open_docx` returns the first 16 rows of a compact index. Each row contains a
stable block id, role, bounded text preview, and tracking status. Use
`index_has_more` and `index_next_offset` when structural navigation requires
another page.

Prefer targeted `inspect_docx` queries:

| Query | Use it for |
|---|---|
| `find` | Locate wording without reading every block. Pass `patterns` with up to eight known phrases to batch them in one request; each pattern, including a zero-match pattern, returns its own exact totals and paging. |
| `block` | Read one exact paragraph, table locator set, guard, or formatting projection. |
| `window` | Read an inclusive range of block ids. |
| `document` | Page through the extended-Markdown document projection. |
| `revisions` | List current pending revisions, optionally filtered by author, kind, or block range. |
| `styles` | Inspect authored style state. |
| `notes` | Read editable footnote and endnote rows. |
| `redline`, `accepted`, `rejected` | Read the current, accept-all, or reject-all projection. |
| `operations` | Retrieve the parser-derived transaction vocabulary and examples. |

Find results are locators, not substitutes for exact reads. Inspect a returned
block id before editing it. Request `detail:"formatting"` only when complete run
spans, marks, and style properties are needed.

Reads are explicitly bounded. Find returns at most 16 matches by default.
Document pages default to 16 top-level blocks. Table reads return paged cell
locators rather than an unbounded aggregate body.

## Executing a plan

`execute_plan` accepts four plan shapes:

| Shape | Behavior |
|---|---|
| v4 transaction | Atomic. Every operation applies or none do. |
| `replacement_worklist` | Ordered literal replacements with per-item outcomes. Deliberately non-atomic. |
| revision selection | Accepts or rejects the explicit current revision selection. |
| `comparison` | Produces a tracked redline from base and target paths. |

For a replacement worklist:

- use an integer `expected_matches` or `replace_all:true`, never both;
- use `match_mode:"exact"` or `match_mode:"normalize_ws"`;
- preview first;
- reissue only refused items after correcting them;
- scope by block id when the intended text is not unique.

The omitted replacement scope covers top-level and table-cell paragraphs.
Preview runs the ordered worklist against a throwaway snapshot, including the
effect of earlier successful items, without changing the live document.

For a two-file comparison, omit `doc_id` and pass
`comparison:{base_path,target_path,out_path,author?}` with `preview:false`.

## Verification

`verify_docx({doc_id})` audits the open session before delivery. `save_docx`
recomputes the same audit and refuses a non-deliverable result before the
destination exists, so a saved output is independently verification-gated.
The producer-neutral form audits any before/after pair. A deliverable result
should report:

- no unexpected direct delta;
- no untouched-scope violations;
- every pre-existing revision in its intended disposition;
- `validator.new_issue_count: 0`.

`baseline_validator` reports findings that already existed when the source was
opened. Unchanged baseline findings must be disclosed, but they are not new
regressions.

Audit lists return 16 rows by default and 64 at most. Use `detail`, `offset`,
and `limit` to retrieve all continuation pages.

## Filesystem and artifact boundary

The MCP server confines every filesystem path to
`STEMMA_MCP_WORKSPACE_ROOT`. The root is canonicalized once at startup. If the
variable is unset, the canonical startup current directory becomes the root.
Startup fails if the root is invalid, is not a directory, or cannot be
represented exactly in UTF-8 artifact receipts.

Relative paths resolve under the root. Absolute paths must still resolve inside
it. Existing source symlinks are canonicalized, so a symlink that escapes the
root is refused.

Every output commit is create-new:

- an existing destination is refused;
- a destination that aliases a consumed input is refused;
- there is no overwrite flag or per-call override;
- bytes are validated and staged in the destination directory;
- commit does not clobber a path created concurrently;
- committed bytes are read back and checked by byte length and SHA-256.

An input artifact identity has this shape:

```json
{
  "role": "input_docx",
  "supplied_path": "draft.docx",
  "resolved_path": "/workspace/draft.docx",
  "digest": {
    "algorithm": "sha256",
    "hex": "<64 lowercase hex characters>"
  },
  "bytes": 12345
}
```

An output artifact adds the collision policy and disposition:

```json
{
  "identity": {
    "role": "output_redline",
    "supplied_path": "result.docx",
    "resolved_path": "/workspace/result.docx",
    "digest": {
      "algorithm": "sha256",
      "hex": "<64 lowercase hex characters>"
    },
    "bytes": 23456
  },
  "collision_policy": "create_new",
  "disposition": "created"
}
```

Artifact identities contain absolute paths and exact-byte hashes. Treat them as
document-sensitive metadata.

This boundary protects against ordinary caller mistakes and failed writes. It
does not protect against a hostile same-user process racing paths, storage
corruption, or power loss.

Stable artifact failure codes:

| Code | Meaning |
|---|---|
| `artifact_outside_workspace` | A path resolves outside the configured root. |
| `artifact_output_exists` | The create-new destination already exists. |
| `artifact_protected_source` | The destination aliases a protected input. |
| `artifact_source_too_large` | A path-backed image exceeds a configured limit. |
| `artifact_read_failed` | A source cannot be resolved, represented, opened, identified, or read safely. |
| `artifact_commit_failed` | Staging, commit, or post-commit verification failed. |
| `artifact_session_state_missing` | A live document lost protected source state and must be reopened. |

## Refusal vocabulary

| Refusal | Safe next action |
|---|---|
| Stale guard or expected text | Re-read the block and rebuild the plan. |
| Match-count mismatch | Narrow or scope the replacement, or correct the expected count. |
| Existing author collision | Use a distinct author or make the documented explicit override. |
| Opaque content would be destroyed | Edit around the opaque object or choose a structure-aware operation. |
| Empty revision selection | Re-read the current revision list and correct the selector. |
| Existing output | Choose a new output path. |
| Task declaration mismatch | Reissue the exact declared replacement, or edit the document outside a task. The declaration cannot be widened. |
| Unknown or already-applied `effect_id` | Use a pending effect declared for that target; do not restate a completed effect. |
| Undeclarable task plan shape | Use a declaration-matched replacement worklist, or leave task mode for broader editing. |

No refusal should be converted into apparent success. See
[Troubleshooting](../help/troubleshooting.md) for symptom-first recovery and the
[advanced refusal table](mcp-advanced.md#refusal-vocabulary) for stable engine
names.

## Session rules

- Re-read a block after any edit that can invalidate its guards.
- Scope edits to the content the instruction actually denotes.
- Preview before applying.
- Verify before saving.
- Save to a new path. Unsaved in-memory edits are not durable.
- Treat artifact identities and receipts as document-sensitive metadata.

## Related

- [Use Stemma with an agent](../guides/use-with-an-agent.md)
- [MCP advanced reference](mcp-advanced.md)
- [CLI approved-worklist contract](cli.md#apply)
- [Stability policy](../guide/stability.md)
