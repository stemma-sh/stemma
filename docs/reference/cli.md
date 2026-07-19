# CLI reference

`stemma` is the canonical local process contract for Stemma's focused workflow:
apply an explicit approved worklist to an existing DOCX and create a native
tracked-changes redline. It also exposes the engine's existing compare, read,
resolve, and validate verbs. Install/build instructions live in
[stemma-cli/README.md](../../stemma-cli/README.md).

The compact product path is `inspect -> execute -> verify`. `execute` is the
agent-facing alias of the same worklist implementation documented as `apply`;
there is one execution contract, not two engines.

Contract: **stdout carries data, stderr carries diagnostics.** `apply` persists
its authoritative machine-readable receipt and mirrors it to stdout. Every
operational failure exits nonzero with a one-line `error: …` message on stderr
that names what failed and which file or id; user input never panics.
`--version` prints the crate version; `--help` (and `<command> --help`) print
usage.

## Filesystem and output contract

CLI paths are explicit filesystem authority supplied by the human caller. They
are not confined by the MCP-only `STEMMA_MCP_WORKSPACE_ROOT` setting.
Supplied and canonical paths must be valid UTF-8 because the shared artifact
identity is a portable serialized contract; non-UTF-8 paths are refused before
source bytes are read or output staging begins.
Windows alternate-data-stream path syntax is likewise refused on every
platform. Sources must be regular files; obvious FIFOs, devices, and
directories are rejected before open and the opened handle is checked again.

Every CLI output is create-new. `apply`, `compare`, and `resolve` refuse an
existing destination, including a symlink or hard-link alias of an input.
`apply` protects the DOCX, worklist, and durable receipt. Its receipt defaults to
`<out>.receipt.json`; `--receipt FILE` overrides it. There is no overwrite option
in this release; remove or rename an unwanted prior output yourself, or choose
new paths.

Output is validated, staged in the destination directory, committed without
clobbering, and read back to verify exact byte length and SHA-256. The existing
human-readable stderr receipt remains and appends:

```text
bytes=<n> sha256=<hex> collision_policy=create_new disposition=created
```

This prevents ordinary accidental replacement and detects failed or mismatched
commits. It is not an operating-system sandbox, protection from a hostile local
process running as the same user, a storage-integrity guarantee, or a power-loss
durability promise.

## Commands

| Command | Purpose |
|---|---|
| `stemma inspect <input> [--format markdown\|json]` | Emit the compact revision-aware projection, bound to the exact input identity. |
| `stemma execute <input> --plan <json> -o <out>` | Execute a concrete plan; exact alias of `apply`. |
| `stemma verify <before> <after> [--policy tracked-delivery-v0]` | Certify any producer's result; exit `3` when policy fails. |
| `stemma apply <input> --worklist <json> -o <out> [--receipt <json>] [--emit-partial]` | Apply a `stemma.worklist.v0` and emit a native redline plus durable JSON receipt. |
| `stemma compare <base> <target> -o <out> [--author NAME]` | Diff two files into a redline (`reject-all == base`, `accept-all == target`); `--author` attributes the revisions. |
| `stemma extract <file> [--format text\|json]` | Read the body as plain text (default) or structured JSON. |
| `stemma resolve <file> -o <out> <disposition>` | Accept/reject tracked changes; write the result. |
| `stemma validate <file>` | Parse + validate; print block/revision counts. |

Exit codes: `0` complete success or verification pass, `1` an operational failure (bad file,
invalid worklist, refused destination), `2` a usage error (clap), and `3` an
executed `apply` whose receipt is partial or a completed `verify` whose policy
result is `fail`. By default an apply exit `3` creates no DOCX. `--emit-partial` explicitly
requests a non-deliverable partial redline, but the status and exit remain
partial/`3`.

## inspect, execute, verify

`inspect` emits extended Markdown as the compact agent language. Its first line
binds the projection to the input SHA-256, byte count, block count, and pending-
revision count; the following addressable blocks retain revision and opaque-
object annotations. `--format json` wraps the same projection in
`stemma.inspect.v0`.

`execute` is a visible alias for `apply`, and `--plan` is the corresponding
alias for `--worklist`. Both routes execute `stemma.worklist.v0` through the
same typed planner, tracked-change materializer, audit, and safe artifact
boundary.

`verify` audits any before/after pair under `tracked-delivery-v0`. It passes
only when the result validates, contains no untracked committed delta, leaves
every pre-existing revision untouched, and has no unexplained untouched-scope
violation. Its `stemma.verify.v0` JSON includes exact input identities and
accepted/rejected projection hashes. A policy failure is a structured result
on stdout with exit `3`, not an operational error.

## apply

`apply` is the focused product workflow. It reads the DOCX and worklist as
protected source artifacts, validates the complete worklist before mutation,
applies each item in order against live document state, audits the result, and
only then commits a new redline.

```bash
demo_dir="$(mktemp -d)"
cargo run -p stemma-cli -- apply \
  stemma-engine/testdata/simple-text/before.docx \
  --worklist stemma-cli/examples/approved-worklist.json \
  -o "$demo_dir/redline.docx" \
  --receipt "$demo_dir/receipt.json"
```

`--plan` is a visible alias for `--worklist`; `--worklist` is the canonical
name because v0 is an explicit change list, not a general intent language.

### Worklist v0

```json
{
  "schema": "stemma.worklist.v0",
  "input": {
    "sha256": "2cdfb8ecd1a27ef7132ebbaa1f718d6705ea6532bf3b155c09bfd7e87d410667",
    "bytes": 11431
  },
  "author": "Approved Reviewer",
  "changes": [
    {
      "id": "change-1",
      "old": "twelve (12) months",
      "new": "twenty-four (24) months",
      "expected_matches": 1,
      "match_mode": "normalize_ws",
      "scope": { "block_id": "p_41" }
    }
  ]
}
```

Top-level and item objects reject unknown fields. `schema`, `input`, `author`,
and a non-empty `changes` array are required; v0 accepts at most 100 changes and
a 1 MiB worklist. `input.sha256` is exactly 64 lowercase hex characters and
`input.bytes` is the exact source length. Stemma checks both before planning, so
a worklist approved for one document cannot run against another document that
happens to contain the same phrase. Run `stemma validate INPUT` to print the
binding values.

Item IDs must be non-empty and unique. `old` must be non-empty, `old` and `new`
must differ, and `expected_matches` must be a positive integer (default `1`).
Empty `new` text is a tracked deletion.

`match_mode` is `exact` by default. `normalize_ws` folds visually equivalent
spaces and straight/curly quote classes only for matching; the replacement is
written verbatim and the receipt names every normalization class actually used.

`scope` is optional and defaults to all top-level body paragraphs. It may be a
single block:

```json
{ "block_id": "p_41" }
```

or an inclusive body-block range:

```json
{ "from_block_id": "p_35", "to_block_id": "p_48" }
```

Use `stemma extract <input> --format json` to inspect current block IDs. A
match-count mismatch refuses that item and returns actual matches with excerpts
instead of guessing. A match crossing an opaque anchor or existing tracked
segment also refuses the item. Default-scope matches detected in table cells
are reported as `unreachable_match` and not partially applied.

Items are deliberately independent and sequential for complete outcome
reporting. A refused item leaves the in-memory document unchanged and does not
block later evaluation; a later item sees every earlier successful edit. If any
item refuses, the receipt status is `partial`, `deliverable` is false, the
process exits `3`, and no DOCX is created by default. `--emit-partial` persists
the successful edits only for explicit diagnosis/review; it never changes the
status, deliverability, or exit code.

### Receipt v0

`stemma.apply_receipt.v0` includes:

- exact SHA-256 identities and byte sizes for input, worklist, and the expected
  output bytes;
- producer version/build stamp, exact running-executable identity, ruleset,
  verification profile, `complete` or `partial` status, deliverability, and
  applied/refused counts;
- every item ID, status, expected/actual match count, match excerpts, changed
  block IDs, actual scope, match mode, new revision count, or explicit stable
  refusal code and diagnosis;
- declared supported, conditionally detected, and unsearched regions;
- validator result, direct-change count, untouched-scope violations,
  pre-existing revision preservation, and the audited new revision count;
- expected output byte identity, the enforced `create_new` collision policy,
  and the process-exit/presence/identity checks required to confirm that those
  bytes were persisted.

Revision numbers are intentionally absent. Word revision IDs are session-local
handles and can change when the exact output is reopened; publishing them as
artifact identities would be misleading.

Before output, Stemma blocks any untracked direct change, unexplained
untouched-scope violation, changed/resolved pre-existing revision, invalid
package, or disagreement between the execution-time item revision census and
the package audit census.

Current coverage is intentionally narrow: top-level body paragraphs. Headers,
footers, footnotes, endnotes, comments, textboxes, and nested table cells are
named as unsearched. For default-scope items, occurrences in top-level table
cells are conditionally detected and refused; this is not a recursive table
search. A `complete` receipt means complete under this declared v0 coverage, not
universal DOCX coverage.

The durable receipt is committed before the DOCX. Its `deliverable` field is a
policy result for the exact candidate bytes, not proof of the subsequent
filesystem commit. Persistence is confirmed only when the actual process exit
matches `output.persistence_confirmation`, the output exists, and its byte
length and SHA-256 match the receipt. If DOCX commit fails, the diagnostic
receipt may remain but exit `1` makes the failed persistence explicit. Stemma
never commits a DOCX first and then hopes stdout succeeds. The same JSON is
mirrored to stdout as a convenience. A closed stdout produces a warning and
leaves the durable receipt and command result authoritative.

Receipts are document-sensitive metadata. They include caller-supplied and
resolved filesystem paths, match excerpts, worklist diagnoses, and artifact
hashes. Do not publish or transmit a sidecar without applying the same handling
and redaction policy as the underlying matter.

## compare

Discovers the deltas between two documents and materializes them as tracked
changes on the output — the two versions collapse into one reviewable file you
step through in Word like any reviewer redline.

```
$ stemma compare memo.docx memo-v2.docx -o redline.docx
wrote redline to redline.docx (2 tracked revisions); bytes=<n> sha256=<hex> collision_policy=create_new disposition=created
```

The receipt goes to stderr; nothing goes to stdout. The output path must not
exist. A destination equal to or aliasing either input is also refused.

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
wrote resolved document to final.docx; bytes=<n> sha256=<hex> collision_policy=create_new disposition=created
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
the block and pending-revision counts plus the exact input-binding values used
by `stemma.worklist.v0`:

```
$ stemma validate redline.docx
OK: redline.docx — 1 block, 2 pending revisions; bytes=<n> sha256=<hex>
```

On failure, exit nonzero with the structured reason on stderr:

```
$ stemma validate broken.docx
error: broken.docx: not a valid DOCX (InvalidDocx: docx read failed: invalid Zip archive: Invalid zip header)
```

## Recipes

Every `-o` path below is assumed not to exist. On a rerun, choose a new name;
the CLI will not replace the prior output.

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
