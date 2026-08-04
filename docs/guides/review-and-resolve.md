# Review and resolve changes

Use this guide when a document already contains tracked changes and you need to
inspect, accept, or reject them from the CLI.

## Inspect pending revisions

```bash
stemma extract redline.docx --format json
```

The JSON contains a `revisions` array. Each row includes its current
`revision_id`, kind, author, date (where the source carries one), block id,
and excerpt.

Revision ids are the engine's content-derived identities: a revision whose
own content is untouched keeps its id across save/reopen and across a
`resolve` of *other* revisions, so ids read here remain valid against the
resolved output. Never take ids from raw XML (`w:id` is not the same value),
and re-extract if an operation may have altered a surviving revision's
content; see [id durability](../reference/cli.md#extract).

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

A revision whose author is the empty string (Word anonymization writes
`w:author=""`) is selectable as its own group: `--accept-author ""` /
`--reject-author ""`.

## Mixed outcomes in one call

To accept one party's changes and reject everyone else's, give `resolve` a
resolution plan:

```bash
cat > plan.json <<'PLAN'
{ "schema": "stemma.resolution_plan.v0",
  "accept": { "authors": ["L. Marsh"] },
  "rest": "reject" }
PLAN
stemma resolve redline.docx -o final.docx --plan plan.json
stemma validate final.docx     # expect: OK, 0 pending revisions
```

Add `--dry-run --format json` first to preview the receipt's
`accepted`/`rejected`/`remaining` partition without writing anything.
Chaining single-disposition passes through an intermediate file still works
(see [CLI reference: resolve](../reference/cli.md#resolve)).

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
