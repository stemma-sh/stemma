# MCP advanced reference

Use this page when the default five-tool profile cannot express a required
expert operation. Start with the [core reference](mcp.md) for the normal
inspect, execute, verify, and save workflow.

Enable the complete 31-tool surface at server startup:

```bash
STEMMA_MCP_PROFILE=advanced npx -y @stemma-sh/mcp
```

Unknown profile values fail startup. Advanced tools use the same typed engine,
workspace boundary, artifact identities, and validation gates as the core
profile.

## Read and navigate

| Tool | Purpose |
|---|---|
| `open_docx(path)` | Open a workspace-confined source and return a handle, compact index, and input identity. |
| `read_outline` / `read_index` | Read the current structural outline or lightweight block index. |
| `read_window(from_block_id,to_block_id,format)` | Render an inclusive block range as text, Markdown, or HTML. |
| `read_markdown` / `read_text` / `read_html` | Read whole-document projections. |
| `read_block(block_id)` | Read one block's text spans, handles, guards, marks, and opaque anchors. |
| `read_redline` / `read_accepted` / `read_rejected` | Read current, accept-all, or reject-all projections without mutation. |
| `find(pattern)` | Locate matching visible text and return bounded excerpts plus block ids. |
| `get_section(heading_id)` | Read one heading subtree. |
| `read_styles` | Read authored document defaults and styles. |
| `list_revisions` | List current revisions and their session-derived ids. |

Re-read a block after editing it. Span handles and guards become stale after a
write to the same block. Revision ids must come from the current session, never
from raw XML or an earlier open.

## Edit

| Tool | Purpose |
|---|---|
| `replace_text` | Tracked replacement of one exact or normalized phrase, including text inside existing redlines. |
| `replace_text_batch` | Ordered replacement worklist with per-item outcomes and optional throwaway preview. Deliberately non-atomic. |
| `replace_all` | Tracked find and replace across body paragraphs. |
| `apply_edit` | Apply one atomic v4 transaction, tracked by default. |
| `apply_batch` | Preview or apply one v4 transaction. |
| `check_edit` | Run the same validation as `apply_edit` against a throwaway clone. |

`replace_text` defaults to one expected match. A mismatch reports the matches
needed to disambiguate. `match_mode:"normalize_ws"` normalizes supported
whitespace and quote classes for matching while preserving the replacement
text verbatim.

```text
replace_text     {"doc_id": ..., "old": "30 days", "new": "45 days", "author": "Approved Reviewer"}
```

## Review, resolve, and export

| Tool | Purpose |
|---|---|
| `accept_changes` / `reject_changes` | Resolve changes by current id, author, block range, or all. |
| `review_session` | Audit everything changed since open, including untouched proof and validation. |
| `audit_docx` | Produce the same audit for any two workspace-confined files. |
| `compare_docx` | Commit a create-new redline from base and target files. |
| `validate_docx` | Run package and OOXML validation. |
| `save_docx` | Validate and commit an open document to a new path. |

An empty resolution selection is an error. Accept keeps the proposed state;
reject restores the prior state. Verify by content because both actions remove
revision markers.

## The edit transaction (v4)

```json
{
  "ops": [
    {
      "op": "replace",
      "target": "p_3",
      "expect": "strict liability",
      "content": {
        "type": "paragraph",
        "content": [
          {
            "type": "text",
            "text": "The Supplier's liability is limited to negligence."
          }
        ]
      }
    }
  ],
  "revision": {
    "author": "J. Osei"
  },
  "summary": "Soften liability clause"
}
```

A transaction is atomic. Targets are block ids from the current document.
`expect`, `guard`, or `semantic_hash` provides optimistic concurrency.

Operation families include:

- block and span replacement, insertion, deletion, and movement;
- run, paragraph, cell, row, table, and section formatting;
- comments and footnotes or endnotes;
- images, equations, bookmarks, cross-references, and numbering;
- styles and content controls;
- page setup, headers, and footers.

Retrieve the current parser-derived operation catalog through
`inspect_docx` with `query:"operations"`. This is authoritative for accepted
fields and examples.

## Path-backed images

Image paths are workspace-confined. Source identity is registered only after a
mutation applies. Preview and rejected edits register nothing.

Two startup limits count source bytes before base64 expansion. Setting a value
to `0` disables that limit:

| Variable | Scope | Default |
|---|---|---|
| `STEMMA_MCP_MAX_IMAGE_BYTES` | Each image path | `20971520` bytes |
| `STEMMA_MCP_MAX_IMAGE_TOTAL_BYTES` | Image-path bytes in one transaction | `52428800` bytes |

Exceeding a limit returns `artifact_source_too_large`. Reduce or split the
input. Only the host operator should raise process-wide limits.

## Refusal vocabulary

| Refusal | Meaning | Do instead |
|---|---|---|
| `StaleEdit` | The addressed content changed after inspection. | Re-read and rebuild the operation. |
| `NoOpEdit` | The replacement equals the current text. | Re-read and correct the intended text. |
| `MatchCountMismatch` | The replacement found a different number of matches. | Narrow the text, scope it, or correct the expected count. |
| `AmbiguousAnchorAfterMove` | The plan anchors on a block moved earlier in the transaction. | Use the moved id named by the error or a stable neighbor. |
| `OpaqueDestroyed` | The operation would silently drop an opaque object. | Edit around it or use a structure-aware operation. |
| `AuthorImpersonation` | The author already owns revisions in the document. | Use a distinct author or the explicit override. |
| `CommentAnchorOverlapsDeleted` | A comment anchor falls on deleted text. | Anchor on retained text or resolve the deletion first. |
| `ParagraphContainsTrackedSegments` | The requested span operation lacks a safe coordinate space. | Resolve the paragraph or use `replace_text`. |
| `StyleNotFound` | A requested style does not exist. | Read current styles or create the style in the same transaction. |
| `InvalidRange` | A revision selector matched nothing. | Re-read revisions and correct the selector. |

Marks are objects such as `[{"type":"bold"}]`, not strings. Span replacement
takes plain text; use a whole-paragraph replacement for mark changes.

## Recipes

Every output path below must be new.

### Flatten a redline

```text
open_docx        {"path": "draft-redline.docx"}
accept_changes   {"doc_id": ..., "selector": {"by": "all"}}
validate_docx    {"doc_id": ...}
save_docx        {"doc_id": ..., "path": "final.docx"}
```

Use `reject_changes` instead when the proposed state should be discarded.

### Resolve one author

```text
open_docx        {"path": "grant-proposal.docx"}
list_revisions   {"doc_id": ...}
accept_changes   {"doc_id": ..., "selector": {"by": "by_author", "author": "L. Marsh"}}
validate_docx    {"doc_id": ...}
save_docx        {"doc_id": ..., "path": "grant-proposal-round2.docx"}
```

List revisions again before saving and confirm that only the intended
reviewer's changes were resolved.

### Compare two files

```text
compare_docx  {"base_path": "as-sent.docx",
               "target_path": "as-returned.docx",
               "out_path": "what-they-changed.docx",
               "author": "Approved Reviewer"}
```

Reject-all must reconstruct the base reading. Accept-all must reconstruct the
target reading.

### Negotiation round

For a mixed accept, reject, and counterproposal task:

1. Open the document and list current revisions.
2. Accept and reject only explicit current ids.
3. Add the counterproposal as a distinct author.
4. Read the accepted projection.
5. Run `review_session` or `validate_docx`.
6. Save to a new path.

Do not reuse revision ids from a previous session, hide new work under an
existing author's identity, or end with unsaved in-memory state.

## Related

- [MCP core reference](mcp.md)
- [Use Stemma with an agent](../guides/use-with-an-agent.md)
- [Troubleshooting](../help/troubleshooting.md)
- [Filesystem and artifact boundary](mcp.md#filesystem-and-artifact-boundary)
