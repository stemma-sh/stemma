# CLI reference

`stemma` is a thin command-line interface to the engine's core verbs — a
zero-integration path for adopters driving the engine from Node, Python, or a
shell instead of Rust. It wraps the stable `Document` facade: compare two files
into a tracked-changes redline, read a document as text or JSON, resolve tracked
changes, and validate a package. Install/build instructions live in
[stemma-cli/README.md](../../stemma-cli/README.md).

No sample files are required to try it: any two saved versions of a real
Word document work as `compare` inputs, and
`cargo run -p stemma --example redline_from_two_files` demonstrates the same
flow on fixtures bundled with the engine crate.

Contract: **stdout carries data, stderr carries diagnostics.** Every failure
exits nonzero with a one-line `error: …` message on stderr that names what
failed and which file or id; user input never panics. `--version` prints the
crate version; `--help` (and `<command> --help`) print usage.

## Commands

| Command | Purpose |
|---|---|
| `stemma compare <base> <target> -o <out> [--author NAME]` | Diff two files into a redline (`reject-all == base`, `accept-all == target`); `--author` attributes the revisions. |
| `stemma extract <file> [--format text\|json]` | Read the body as plain text (default) or structured JSON. |
| `stemma resolve <file> -o <out> <disposition>` | Accept/reject tracked changes; write the result. |
| `stemma validate <file>` | Parse + validate; print block/revision counts. |

Exit codes: `0` success, `1` a runtime failure (bad file, unknown id/author,
refused overwrite), `2` a usage error (clap — unknown flag, missing argument,
missing/duplicate disposition).

## compare

Discovers the deltas between two documents and materializes them as tracked
changes on the output — the two versions collapse into one reviewable file you
step through in Word like any reviewer redline.

```
$ stemma compare memo.docx memo-v2.docx -o redline.docx
wrote redline to redline.docx (2 tracked revisions)
```

The receipt (revision count) goes to stderr; nothing goes to stdout. Overwriting
an existing output file is allowed; overwriting either input is refused:

```
$ stemma compare memo.docx memo-v2.docx -o memo.docx
error: refusing to overwrite the input file memo.docx: choose a different --out path
```

> **Attribution.** By default the redline's tracked changes carry the engine's
> own (blank) authorship — discovery has no authoring identity. Pass
> `--author NAME` to attribute every discovered revision to `NAME` (it appears
> as each change's `author`, and `resolve --accept-author`/`--reject-author`
> can then select by it):
>
> ```
> $ stemma compare memo.docx memo-v2.docx -o redline.docx --author "L. Marsh"
> ```
>
> An empty `--author ""` is refused — omit the flag for an anonymous redline;
> there is no silent fallback to anonymous.

## extract

Read the document body. `--format text` (the default) prints the plain-text
reading to stdout; `--format json` prints a structured projection.

```
$ stemma extract redline.docx --format text
This is a test now foo bar bazwhat are the chances
```

The text reading shows the document **as it stands**: on a redline, both the
tracked deletion and the tracked insertion surface (here `now foo bar baz` was
deleted and `what are the chances` inserted). To read one side, resolve first.

`--format json` gives blocks plus a `revisions` array — the pending tracked
changes, each with its `revision_id` (the id `resolve --accept-ids`/`--reject-ids`
takes), `kind`, `author`, `block_id`, and a short `excerpt`:

```
$ stemma extract redline.docx --format json
{
  "blocks": [
    {
      "id": "p_1",
      "role": "paragraph",
      "text": "This is a test now foo bar bazwhat are the chances"
    }
  ],
  "revisions": [
    {
      "revision_id": 3,
      "kind": "delete",
      "author": "",
      "block_id": "p_1",
      "excerpt": "now foo bar baz"
    },
    {
      "revision_id": 4,
      "kind": "insert",
      "author": "",
      "block_id": "p_1",
      "excerpt": "what are the chances"
    }
  ]
}
```

Fields are a projection of the engine's read view: `role` is one of `paragraph`,
`heading` (with a `heading_level`), `table`, `opaque`; `style_id` and
`heading_level` are omitted when absent. `author` is empty when the source change
carried no `w:author` (Word anonymization, third-party tools, or a `compare`
redline produced without `--author`). Revision ids are session handles read live from the document — always
re-`extract` to get current ids, never reuse ids from a previous run or from raw
XML.

## resolve

Accept or reject tracked changes and write the resolved document. Exactly one
disposition is required (clap enforces it):

| Disposition | Effect |
|---|---|
| `--accept-all` / `--reject-all` | Every pending change. |
| `--accept-author <NAME>` / `--reject-author <NAME>` | Every change by that author. |
| `--accept-ids <a,b,…>` / `--reject-ids <a,b,…>` | The named revision ids. |

```
$ stemma resolve redline.docx -o final.docx --accept-all
wrote resolved document to final.docx
$ stemma extract final.docx --format text
This is a test what are the chances
```

Accept keeps the new state; reject restores the prior state exactly (marker
absence alone proves nothing — verify by content). A selection that matches
nothing fails loudly rather than writing an unchanged file: an unknown id, an
author with no changes, or an accept/reject-all on a document with no pending
changes are all errors, and no output is written.

```
$ stemma resolve redline.docx -o out.docx --accept-ids 42
error: revision id(s) 42 not found in redline.docx (pending ids: 3, 4)
```

## validate

Parse and validate a package. On success, exit `0` with an `OK` line reporting
the block and pending-revision counts:

```
$ stemma validate redline.docx
OK: redline.docx — 1 block, 2 pending revisions
```

On failure, exit nonzero with the structured reason on stderr:

```
$ stemma validate broken.docx
error: broken.docx: not a valid DOCX (InvalidDocx: docx read failed: invalid Zip archive: Invalid zip header)
```

## Recipes

**Redline two files and enumerate what changed** (shell + `jq`):

```
stemma compare as-sent.docx as-returned.docx -o changed.docx
stemma extract changed.docx --format json | jq '.revisions[] | {id: .revision_id, kind, excerpt}'
```

**Flatten a redline to a clean final** — accept everything, leaving no revision
machinery:

```
stemma resolve draft-redline.docx -o final.docx --accept-all
stemma validate final.docx          # expect: OK
```

**Accept one reviewer, leave the rest pending:**

```
stemma resolve markup.docx -o round2.docx --accept-author "L. Marsh"
```

The output still carries every other author's changes, untouched; run
`stemma extract round2.docx --format json` to confirm what remains pending.
