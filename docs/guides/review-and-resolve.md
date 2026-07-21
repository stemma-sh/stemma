# Review and resolve changes

Use this guide when a document already contains tracked changes and you need to
inspect, accept, or reject them from the CLI.

## Inspect pending revisions

```bash
stemma extract redline.docx --format json
```

The JSON contains a `revisions` array. Each row includes its current
`revision_id`, kind, author, block id, and excerpt.

Revision ids are session-derived addresses. Always extract them from the
current file immediately before selecting by id.

## Resolve changes

Accept every pending change:

```bash
stemma resolve redline.docx -o accepted.docx --accept-all
```

Reject every pending change:

```bash
stemma resolve redline.docx -o rejected.docx --reject-all
```

Resolve one author's changes:

```bash
stemma resolve redline.docx \
  -o reviewer-accepted.docx \
  --accept-author "L. Marsh"
```

Resolve selected revision ids:

```bash
stemma resolve redline.docx \
  -o selected.docx \
  --reject-ids 3,4
```

Exactly one disposition is required. A selector that matches nothing is an
error and creates no output.

## Verify by content

Accepting and rejecting both remove revision markers, so marker absence does
not prove which action occurred. Inspect the resulting content:

```bash
stemma extract accepted.docx --format text
stemma validate accepted.docx
```

Accept keeps the proposed state. Reject restores the prior state. Outputs are
create-new, and Stemma will not replace an existing path.

For all selectors and failure behavior, see
[CLI reference: resolve](../reference/cli.md#resolve).
