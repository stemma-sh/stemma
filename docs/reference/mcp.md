# MCP tool reference

The stemma-mcp tool surface: every verb, its receipt, and the refusals you
may hit with their escape hatches. **This page is self-contained by design —
it is written to be pasted whole into an agent's context.** Install/wiring
instructions live in [stemma-mcp/README.md](../../stemma-mcp/README.md).

## Golden path

1. `open_docx(path)` → `doc_id` + a compact `index` (one row per block: stable
   `id`, `role`, a 120-char `text_preview`, tracking `block_status`). Address
   all later ops by these ids. Full block `text` and the per-block
   `semantic_hash` guard are NOT in this index — `read_outline` (every block)
   or `read_block(id)` carry them when you need the guard or the full text.
2. Understand the document: `read_markdown` (whole doc, id-bearing, with
   `<ins>`/`<del>`), `read_index` + `read_window` for large documents,
   `find(pattern)` to resolve wording → block id, `list_revisions` for the
   structured index of pending tracked changes.
3. Resolve existing changes if the task asks:
   `accept_changes`/`reject_changes` with a selector — `{"by":"by_ids",
   "revision_ids":[...]}`, `{"by":"by_author","author":"..."}`,
   `{"by":"by_range","from_block_id":"p_4","to_block_id":"p_6"}`, or
   `{"by":"all"}`. Batch ids into one call. An empty selection fails loudly.
4. Make your own edits tracked. For "find this phrase, replace it" — the
   dominant case — use `replace_text(doc_id, old, new, author)` FIRST: it
   matches server-side and splices a tracked change through existing
   redlines in one call. Use `apply_edit` for surgical/structural cases.
5. `validate_docx(doc_id)` → expect `{ok: true}`.
6. Review before saving: `read_accepted` (what the recipient gets on
   accept-all) or `review_session` (everything this session changed +
   untouched proof + validator verdict). Diff it against the task.
7. `save_docx(doc_id, path)` — write to a NEW path (convention — the server
   does not enforce it; it will overwrite whatever `path` names).

## Tools

| Read / navigate | |
|---|---|
| `open_docx(path)` | Open; returns `doc_id` + a compact per-block `index` (no full text / `semantic_hash` — use `read_outline` / `read_block` for those). |
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
| `replace_text(doc_id, old, new, author, scope?, expected_matches?, match_mode?)` | Server-side tracked replace of one phrase, splicing through existing redlines. `expected_matches` defaults to 1; a count mismatch fails listing every match `{block_id, excerpt}` so you disambiguate in one step; pass `"all"` to replace everywhere. `match_mode:"normalize_ws"` folds NBSP/typographic spaces and curly/straight quotes. |
| `replace_text_batch(doc_id, replacements[], author)` | A worklist of replace_text items applied in order against live state; per-item outcomes; a failed item never blocks the rest (deliberately non-atomic). |
| `replace_all(doc_id, needle, replacement, scope?, case_sensitive?, whole_word?, on_barrier_match?)` | Tracked find/replace across all body paragraphs; takes no `author` (changes are stamped with the fixed `stemma` identity — use `replace_text` when you need a named author). Refuses paragraphs that already carry tracked changes; `replace_text` splices through them. |
| `apply_edit(doc_id, transaction, mode?, allow_existing_author?)` | Atomic v4 transaction (see below). `mode:"direct"` applies untracked. |
| `apply_batch(doc_id, transaction, preview, mode?, allow_existing_author?)` | Same, with a required dry-run `preview` switch (true = preview, false = apply). |
| `check_edit(doc_id, transaction)` | Full dry-run: `{would_apply}` or the exact error `apply_edit` would raise. Mutates nothing. |

| Review / verify / export | |
|---|---|
| `accept_changes` / `reject_changes` | Resolve by selector (above). Accept keeps the new state; reject restores the prior state exactly. |
| `review_session(doc_id, render?)` | Everything THIS session changed (census + any untracked delta), proof all other blocks are untouched, validator verdict; `render: {path}` optionally writes the session delta as a redline docx. |
| `audit_docx(before_path, after_path, render?)` | The same report for ANY two files — certify edits stemma didn't make. `render: {path}` also writes the before→after redline. |
| `compare_docx(base_path, target_path, out_path, author?)` | Diff two files into a redline docx. |
| `validate_docx` | `{ok, issues}` from the package/OOXML validators. |
| `save_docx(doc_id, path)` | Validate + write. Refuses structurally corrupt output. Write to a new path by convention — the server does not enforce it and will overwrite `path`. |

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
EXACTLY one of `path` — server-side file read, preferred — or
`bytes_base64`; omit `cx`/`cy` to use the image's intrinsic size, give one
to scale the other by aspect ratio), `insert_cross_ref`, `set_numbering`
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
  input file (never modified) and what you save.

## Recipes

The canonical tasks, end to end. Same contract as the rest of this page:
self-contained, paste-ready.


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
