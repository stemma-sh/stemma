# CLI reference

Use this page to look up the exact command, exit-code, worklist, and receipt
contracts. For a first successful workflow, start with
[Create your first redline](../getting-started.md).

`stemma` is the canonical local process contract for Stemma's focused workflow:
apply an explicit worklist to an existing DOCX and create a native
tracked-changes redline. Exact input binding is optional for ordinary use and
available when a worklist crosses an approval boundary. The CLI also exposes
the engine's existing compare,
extract, read, resolve, and validate verbs. Install/build instructions live in
[stemma-cli/README.md](https://github.com/stemma-sh/stemma/blob/main/stemma-cli/README.md).

The compact product path is `inspect -> execute`: successful execution includes
serialization, verification of the exact candidate bytes, and create-new
commit. `execute` is the agent-facing alias of the same worklist implementation
documented as `apply`; there is one execution contract, not two engines.
Standalone `verify` is an optional producer-neutral recheck, not a required
second pass after successful execution.

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
| `stemma verify-task <manifest.json> [--root <dir>]` | Verify an MCP task delivery from its manifest and artifact files. |
| `stemma apply <input> --worklist <json> -o <out> [--receipt <json>] [--emit-partial]` | Apply a `stemma.worklist.v0` and emit a native redline plus durable JSON receipt. |
| `stemma compare <base> <target> -o <out> [--author NAME] [--format text\|json]` | Diff two files into a redline (`reject-all == base`, `accept-all == target`); `--author` attributes the revisions; `--format json` emits a `stemma.compare_receipt.v0`. |
| `stemma extract <file> [--format text\|json]` | Read the body as plain text (default) or structured JSON. |
| `stemma read <file>` | Emit the full structured read model (`stemma.read.v0`): typed blocks with per-segment tracked status, plus the complete revision census. |
| `stemma resolve <file> -o <out> <disposition> [--dry-run] [--format text\|json]` | Accept/reject tracked changes; write the result; `--format json` emits a `stemma.resolve_receipt.v0`. |
| `stemma validate <file> [--format text\|json]` | Parse + validate; print block/revision counts, or a structured `stemma.validate.v0` result. |

Exit codes: `0` complete success or verification pass, `1` an operational failure (bad file,
invalid worklist, refused destination), `2` a usage error (clap), and `3` an
executed `apply` whose receipt is partial or a completed `verify` whose policy
result is `fail`. By default an apply exit `3` creates no DOCX. `--emit-partial` explicitly
requests a non-deliverable partial redline, but the status and exit remain
partial/`3`.

`verify-task` has its own four-way projection: `0` verified complete, `1`
verified partial, `2` artifact/evidence mismatch, and `3` usage, I/O, malformed
manifest, or unknown schema. A verified partial is a consistent statement about
an incomplete task; it is not the same state as a verification mismatch.

## inspect, execute, verify

`inspect` emits extended Markdown as the compact agent language. Its first line
binds the projection to the input SHA-256, byte count, block count, and pending-
revision count; the following addressable blocks retain revision and opaque-
object annotations. `--format json` wraps the same projection in
`stemma.inspect.v1`: `{schema, input, block_count, pending_revision_count,
projection}`, where the two `*_count` fields are integers and `projection` is
the extended-Markdown text as one string. (v0 named the counts
`blocks`/`pending_revisions`, colliding with the read model's `blocks`
**array**; v1 renamed them so the two shapes cannot be conflated. The
extended-Markdown projection and its `@stemma inspect.v0` header line are
unchanged.) This wrapper is deliberately not structured render data; for the
[read model](read-model.md)'s block array in one call, use
[`read`](#read).

### Extended-Markdown projection grammar

The projection's inline annotations, for consumers that parse it:

| Markup | Meaning |
|---|---|
| `<ins id=N by="Author">…</ins>` | Pending tracked insertion (`id` = the `revision_id` that `resolve --accept-ids` takes; `by` = author, `""` for the empty author group). |
| `<del id=N by="Author">…</del>` | Pending tracked deletion. |
| `<del id=N …>` + `<ins id=N …>` sharing one id | The two halves of ONE tracked move: the source's leaving text and the destination's arriving text. |
| `<b>…</b>`, `<i>…</i>`, … | Formatting marks of the CURRENT state, not revision markers. |
| `<obj id=… kind=table/>`, `<obj id=… kind=opaque …/>` | Non-text block anchors. |

Two pending revision kinds deliberately carry **no inline marker** in this
projection: a formatting change (`format_run` renders as its current marks,
e.g. plain `<b>bold</b>`; `format_paragraph` as ordinary text) and a pending
paragraph-mark insertion/deletion. The projection shows the current *reading*;
it is not a complete revision inventory. To enumerate every pending revision
(including formatting and paragraph-mark changes, with author, kind, block
id, and a locating excerpt), pair the projection with
[`extract --format json`](#extract), whose `revisions` array is complete by
construction.

`execute` is a visible alias for `apply`, and `--plan` is the corresponding
alias for `--worklist`. Both routes execute `stemma.worklist.v0` through the
same typed planner, tracked-change materializer, audit, and safe artifact
boundary. The execution audit is rerun over the exact serialized candidate
before its receipt or DOCX is committed.

`verify` audits any before/after pair under `tracked-delivery-v0`. It passes
only when the result validates, contains no untracked committed delta, leaves
every pre-existing revision untouched, and has no unexplained untouched-scope
violation. Its `stemma.verify.v0` JSON includes exact input identities and
accepted/rejected projection hashes. A policy failure is a structured result
on stdout with exit `3`, not an operational error. Use it when the output came
from another producer or when an independent recheck is useful; a successful
`execute` has already passed the same delivery policy.

## verify-task

`verify-task` reads `stemma.task_manifest.v1`, every input and output it names,
and no MCP session state. Paths resolve relative to the manifest directory;
`--root` selects a different artifact directory for a relocated delivery. The
command recomputes byte lengths, SHA-256 digests, save-time audit counts and
commitments, then confirms that every revision identity claimed by a satisfied
effect is present in the corresponding output.

```bash
stemma verify-task delivery/task.json
```

An unknown schema fails with exit `3`; it is never decoded as a nearby version.
The manifest is unsigned. Verification proves consistency with the files, not
producer authenticity, declaration timing, or completeness of the caller's
intent. Creation is MCP-only; see
[Verify a multi-document task delivery](../guides/verify-task-delivery.md).

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
  "author": "Approved Reviewer",
  "changes": [
    {
      "id": "change-1",
      "old": "twelve (12) months",
      "new": "twenty-four (24) months"
    }
  ]
}
```

Top-level and item objects reject unknown fields. `schema`, `author`, and a
non-empty `changes` array are required; v0 accepts at most 100 changes and a
1 MiB worklist. When `input` is omitted, Stemma applies the worklist to the
explicit positional input and reports `input_binding: "unbound"`; the receipt
still records that input's exact identity.

For a worklist approved separately from execution, add an exact input binding:

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
      "new": "twenty-four (24) months"
    }
  ]
}
```

When present, `input.sha256` must be exactly 64 lowercase hexadecimal
characters and `input.bytes` must be the exact source length. Stemma checks
both before planning and reports `input_binding: "input_verified"`, so a
worklist approved for one document cannot run against another document that
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
- `input_binding`, which is `unbound` when the worklist omitted its optional
  identity or `input_verified` when the supplied identity matched;
- producer version/build stamp, exact running-executable identity, ruleset,
  verification profile, `complete` or `partial` status, deliverability, and
  applied/refused counts;
- every item ID, status, expected/actual match count, match excerpts, changed
  block IDs, actual scope, match mode, new revision count, or explicit stable
  refusal code and diagnosis;
- declared supported, conditionally detected, and unsearched regions;
- validator result, direct-change count, untouched-scope violations,
  pre-existing revision preservation, and the audited new revision count;
- `verification.artifact_stage`, which is `serialized_output` for a complete
  delivery, and `verification.output_sha256`, which must equal the output
  artifact digest;
- expected output byte identity, the enforced `create_new` collision policy,
  and the process-exit/presence/identity checks required to confirm that those
  bytes were persisted.

Revision numbers are intentionally absent. Word revision IDs are session-local
handles and can change when the exact output is reopened; publishing them as
artifact identities would be misleading.

Before output, Stemma serializes the candidate and audits those exact bytes.
It blocks any untracked direct change, unexplained untouched-scope violation,
changed/resolved pre-existing revision, invalid package, disagreement between
the execution-time item revision census and the serialized package audit
census, or disagreement between the verification output hash and candidate
bytes.

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
changes on the output. The two versions collapse into one reviewable file you
step through in Word like any reviewer redline.

```
$ stemma compare memo.docx memo-v2.docx -o redline.docx
wrote redline to redline.docx (2 tracked revisions); bytes=<n> sha256=<hex> collision_policy=create_new disposition=created
```

The human summary goes to stderr; stdout stays empty by default. The output
path must not exist. A destination equal to or aliasing either input is also
refused.

`--format json` additionally emits a `stemma.compare_receipt.v0` on stdout:
the exact `base`/`target` input identities, the requested `author` (`null`
for an anonymous redline), the committed `output` identity (bytes, SHA-256,
collision policy, disposition), and `revisions`: the full census of
discovered revisions, same rows and ids as `extract --format json` on the
output, so a consumer can drive `resolve` without a second read.

> **Attribution.** By default the redline's tracked changes carry the engine's
> own blank authorship because discovery has no authoring identity. Pass
> `--author NAME` to attribute every discovered revision to `NAME` (it appears
> as each change's `author`, and `resolve --accept-author`/`--reject-author`
> can then select by it):
>
> ```
> $ stemma compare memo.docx memo-v2.docx -o redline.docx --author "L. Marsh"
> ```
>
> An empty `--author ""` is refused. Omit the flag for an anonymous redline;
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

`--format json` gives blocks plus a `revisions` array of pending tracked
changes. Each entry has its `revision_id` (the id `resolve --accept-ids`/`--reject-ids`
takes), `kind`, `author`, `date`, `block_id`, and a short `excerpt`:

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
      "date": "",
      "block_id": "p_1",
      "excerpt": "now foo bar baz"
    },
    {
      "revision_id": 4,
      "kind": "insert",
      "author": "",
      "date": "",
      "block_id": "p_1",
      "excerpt": "what are the chances"
    }
  ]
}
```

`kind` is the engine's full revision vocabulary (the same closed set the
[read model reference](read-model.md) documents), not just insert/delete:
`insert`, `delete`, `move` (a source/destination pair enumerates as ONE
atomic record), `format_run`, `format_paragraph`, `format_table`,
`format_row`, `format_cell`, `format_section`, and `opaque_interior` (a
change inside opaque content such as a textbox; individually resolvable only
when it carries a nonzero `revision_id`; a census-only interior reports
`revision_id: 0` and is never selectable). A consumer switching on `kind`
must handle, or explicitly refuse, all ten: a real multi-party redline
routinely carries moves and formatting changes alongside inserts and
deletes.

Fields are a projection of the engine's read view: `role` is one of `paragraph`,
`heading` (with a `heading_level`), `table`, `opaque`; `style_id` and
`heading_level` are omitted when absent. Every `text` value is the current
redline reading: pending insertions and not-yet-accepted deletions are both
present, exactly what Word shows with tracked changes displayed. For a
`table` block, `text` is that reading flattened: every cell paragraph's text
joined by single spaces, recursing into nested tables (structure and cell
addressing live in the richer reads, see the
[read model reference](read-model.md)). For an
`opaque` block, `text` is the empty string: an opaque block has no readable
text, and this command emits no placeholder label for it.
`author` is the empty string when the source change carries a blank
`w:author=""` (Word anonymization, third-party tools, or a `compare` redline
produced without `--author`); the empty string is a real, selectable author
group: `resolve --accept-author ""` / `--reject-author ""` target exactly
those changes. A tracked change whose `w:author` attribute is entirely
absent is refused at import (`InvalidDocx: missing required tracked change
attribute: author`), so an extracted revision always has an `author` value.
`date` is the change's `w:date` timestamp (ISO-8601), the same value the
read model's `RevisionRecord.date` carries: empty when the source stamps a
blank date (a `compare` redline does), omitted when the source carries no
`w:date` at all.

> **Id durability.** A `revision_id` is the engine's content-derived identity:
> a hash of the change's kind, story, author, date, and content, plus a
> disambiguating ordinal among identical duplicates. It is **not** the wire
> `w:id`, and it is not a parse-order counter. The contract: the id of a
> revision whose own content is untouched survives serialize/reopen and
> survives a `resolve` of *other* revisions, so ids read before a selective
> resolve remain valid against its output. The one edge: if resolving a change
> alters a *surviving* revision's content (or removes an identical-signature
> duplicate ahead of it), that survivor is re-keyed; a mixed `resolve --plan`
> detects this at its internal phase boundary and refuses rather than
> resolving the wrong revision. Ids never come from raw XML; always read
> them from `extract`, `read`, or a receipt.

## read

Emit the engine's full structured [read model](read-model.md) in one call, as
`stemma.read.v0` JSON on stdout:

```
$ stemma read redline.docx
{
  "schema": "stemma.read.v0",
  "input": { "role": "input_docx", "supplied_path": "…", "digest": { … }, "bytes": … },
  "blocks": [ … ],
  "revisions": [ … ]
}
```

`blocks` is the typed block array the read-model reference documents,
serialized whole: every text segment carries its tracked `status` (`Normal`,
or `Inserted`/`Deleted` with the revision's id, author, and date), its
`marks`, and its span `handle`; blocks carry their `guard`, list membership,
literal prefix, and (for tables) the cell grid. This is enough to render a
redline view from one invocation. `revisions` is the same complete census as
`extract --format json` (the segment view alone omits formatting-change
records), so the ids that `resolve` selects on arrive in the same call.
`extract` remains the compact body reading; `read` is the full-fidelity
machine surface.

Stability, same statement as the read-model reference: the envelope
(`schema`, `input`, the presence of `blocks`/`revisions`) is the v0 contract,
but the block shapes inside are the engine-version-bound read model. Fields
are added between releases; render from it live, tolerate unknown fields, and
never persist it (durability is DOCX bytes).

## resolve

Accept or reject tracked changes and write the resolved document. Exactly one
disposition is required (clap enforces it):

| Disposition | Effect |
|---|---|
| `--accept-all` / `--reject-all` | Every pending change. |
| `--accept-author <NAME>` / `--reject-author <NAME>` | Every change by that author. |
| `--accept-ids <a,b,…>` / `--reject-ids <a,b,…>` | The named revision ids. |
| `--plan <file.json>` | A `stemma.resolution_plan.v0` mixed disposition (below). |

```
$ stemma resolve redline.docx -o final.docx --accept-all
wrote resolved document to final.docx; bytes=<n> sha256=<hex> collision_policy=create_new disposition=created
$ stemma extract final.docx --format text
This is a test what are the chances
```

Accept keeps the new state; reject restores the prior state exactly. This is
total over the whole revision vocabulary, moves included: a `move` is one
atomic revision (accepting lands the text at its destination, rejecting
restores the source), and after `--accept-all` or `--reject-all` the output
reports **zero** pending revisions; `stemma validate <out>` is the cheap
post-condition check. Marker
absence alone proves nothing, so verify by content. A selection that matches
nothing fails loudly rather than writing an unchanged file: an unknown id, an
author with no changes (the empty author group `--accept-author ""` included),
or an accept/reject-all on a document with no pending
changes are all errors, and no output is written.

```
$ stemma resolve redline.docx -o out.docx --accept-ids 42
error: revision id(s) 42 not found in redline.docx (pending ids: 3, 4)
```

### Resolution plan v0

`--plan <file.json>` expresses a mixed disposition. The flag selectors can
only accept *or* reject per invocation; a plan does both in one call, one
output, one receipt:

```json
{
  "schema": "stemma.resolution_plan.v0",
  "accept": { "authors": ["L. Marsh"], "ids": [17] },
  "reject": { "authors": [""] },
  "rest": "reject"
}
```

- `accept` / `reject` each take `authors` (exact author groups; `""` is the
  empty-author group) and/or `ids`. Both objects are optional, as is either
  field within them.
- `rest` disposes of every *selectable* pending revision no selector matched:
  `"accept"`, `"reject"`, or `"leave"` (the default: a plan only resolves
  what it names; this default is part of the contract).
- Fail-loud, like the flags: an author with no pending changes, an unknown
  id, an id selected by both sides, an unknown field (a misspelled selector
  never silently selects nothing), a wrong `schema`, and a plan that resolves
  nothing are all refused before anything is written.
- Plans address the selectable census (`revision_id != 0`). Census-only
  records (id `0`, e.g. some opaque-interior changes) are outside plan scope
  and stay pending; only the total `--accept-all`/`--reject-all` resolve
  those.
- A mixed plan executes accepts before rejects internally. Revision ids are
  content-derived and survive the internal phase (see **Id durability**
  above); in the rare case an accepted change re-keys a survivor selected
  for rejection, the command refuses with the affected ids rather than
  resolving the wrong revision.

The plan file is a protected source: the output path may not alias it.

### Receipt and dry-run

The human summary goes to stderr; stdout stays empty by default. `--format
json` emits a `stemma.resolve_receipt.v0` on stdout:

```json
{
  "schema": "stemma.resolve_receipt.v0",
  "input": { "role": "input_docx", "supplied_path": "…", "digest": { … }, "bytes": … },
  "dry_run": false,
  "accepted": [ { "revision_id": 3, "kind": "delete", "author": "…", "block_id": "p_1", "excerpt": "…" } ],
  "rejected": [ … ],
  "remaining": [ … ],
  "output": { "identity": { … }, "collision_policy": "create_new", "disposition": "created" }
}
```

`accepted`/`rejected` partition the **input** document's census rows this call
resolved; `remaining` enumerates what is still pending in the output (same row
shape as `extract --format json`). `--dry-run` plans and reports the full
outcome, receipt included, without writing anything: `output` is `null`,
`dry_run` is `true`, and the destination is untouched (its create-new check
runs only on a real write).

## validate

Parse and validate a package. On success, exit `0` with an `OK` line reporting
the block and pending-revision counts plus the exact input-binding values used
by `stemma.worklist.v0`:

For example, `stemma validate redline.docx` reports `OK:` followed by the
document path, one block, two pending revisions, its byte count, and its
SHA-256.

On failure, exit nonzero with the structured reason on stderr:

```
$ stemma validate broken.docx
error: broken.docx: not a valid DOCX (InvalidDocx: docx read failed: invalid Zip archive: Invalid zip header)
```

`--format json` emits a `stemma.validate.v0` result on stdout, for valid
**and** invalid packages alike (mirroring `verify`'s pass/fail contract):
`{schema, status, input, block_count, pending_revision_count, issues}`, where
`status` is `"ok"` or `"invalid"` and `issues` enumerates each validator
finding (`code`, `message`, `context`), empty exactly when `ok`. An invalid
package still exits `1`. Only an operational failure (unreadable file, not a
parseable DOCX at all) stays on the `error:` stderr path in both formats.

> **What import requires of a package.** Every command that reads a `.docx`
> shares one fail-loud import: the package must carry `[Content_Types].xml`,
> `_rels/.rels`, `word/document.xml`, **and** `word/_rels/document.xml.rels`.
> The document-part relationships file is required even when it declares no
> relationships; a structurally minimal package without it is refused
> (`InvalidDocx: missing word/_rels/document.xml.rels`), not repaired. Tools
> that generate fixture or test inputs must include all four parts.

## Recipes

Every `-o` path below is assumed not to exist. On a rerun, choose a new name;
the CLI will not replace the prior output.

**Redline two files and enumerate what changed** (shell + `jq`):

```
stemma compare as-sent.docx as-returned.docx -o changed.docx
stemma extract changed.docx --format json | jq '.revisions[] | {id: .revision_id, kind, excerpt}'
```

**Flatten a redline to a clean final.** Accept everything, leaving no revision
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

**Mixed outcome (accept one party, reject the rest).** One call, one plan,
one receipt:

```
cat > plan.json <<'PLAN'
{ "schema": "stemma.resolution_plan.v0",
  "accept": { "authors": ["L. Marsh"] },
  "rest": "reject" }
PLAN
stemma resolve markup.docx -o final.docx --plan plan.json --format json
stemma validate final.docx        # expect: OK, 0 pending revisions
```

Preview the same plan without writing: add `--dry-run` and read the receipt's
`accepted`/`rejected`/`remaining` partition. The multi-pass composition
(`--accept-author` then `--reject-all` through an intermediate file) still
works, and because revision ids are durable (see **Id durability**), ids
read before the first pass remain valid for the second when their revisions
were untouched.
