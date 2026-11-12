# Python

> **Experimental.** The Python package is new in 0.7 and its API may change
> before 1.0, in any minor release, with a changelog entry. The engine it
> wraps is the same one the CLI and MCP server run; only the Python surface is
> experimental.

The `stemma-py` package gives a Python program the engine's core cycle: open a
`.docx`, read it, apply a v4 transaction, review, and save, plus comparison
and audit. It is a thin binding over the Rust
[`api::Document` facade](embedding.md#the-document-lifecycle-tier-1):
transactions go in as v4 JSON, results come back as plain Python values (dicts,
lists, `str`, `bytes`). There are no per-operation helper classes; the
[v4 operation reference](operations.md) is the write vocabulary, exactly as
for every other transport. The shapes of those values (`Revision`,
`ValidationReport`, `SaveReceipt`, `Transaction`, …) are `TypedDict`s and
aliases importable from `stemma` for annotations. Byte inputs
(`Document.from_bytes`, `audit`, `validate`) take `bytes`, `bytearray`, or
`memoryview`.

## Install

```bash
pip install stemma-py
```

The PyPI distribution is **`stemma-py`**; the import name is **`stemma`**.

Wheels are published for Linux (x86_64 and aarch64, glibc; x86_64 musl), macOS
(Apple silicon and Intel), and Windows (x86_64). One wheel per platform serves
every CPython from 3.9 on (the stable ABI). A wheel needs no compiler and has
no Python dependencies. On other platforms pip builds the source distribution,
which needs a Rust toolchain and a C compiler.

## The one-edit cycle

Open a document, read a block's `id` and `guard`, apply one tracked
replacement, check it, and save:

```python
import stemma

doc = stemma.Document.open("contract.docx")
for diagnostic in doc.diagnostics():
    print("import normalization:", diagnostic["message"])

block = doc.read()["blocks"][1]

edited = doc.apply_authored({
    "ops": [{
        "op": "replace",
        "target": block["id"],
        "guard": block["guard"],
        "content": {"type": "paragraph", "content": [
            {"type": "text", "text": "Liability is capped at twice the fees paid."}]},
    }],
    "revision": {"author": "J. Osei", "date": "2026-07-06T10:30:00Z"},
})

# One file, two readings.
assert "twice the fees" in edited.read_accepted().to_text()
assert "twice the fees" not in edited.read_rejected().to_text()

report = edited.review()
assert report["direct_changes"] == [] and report["validator"]["ok"]

receipt = edited.save("contract-redline.docx")
print(receipt["sha256"])
```

Every `Document` is immutable. `apply`, `project`, and the other verbs return a
new `Document` and leave the receiver as it was, so earlier states stay valid
to read, branch, or save.

A transaction is the v4 JSON text or an equivalent `dict`; both go through the
engine's own parser, which rejects unknown fields. Edits are tracked by
default. Set `"materialization_mode": "direct"` in the transaction for an
untracked edit. Use `apply_authored` whenever the author label comes from a
user or a caller: it refuses a label that already names revisions pending in
the opened document, raising `AuthorLabelCollision`, unless you pass
`allow_existing_author=True` deliberately. `apply` enforces no label policy.
`check(transaction)` is the dry run: it raises exactly what `apply` would.

`save(path)` never overwrites: an existing path raises `FileExistsError` and is
left untouched, the same create-new rule as the CLI. Pass `overwrite=True` to
replace a file atomically; the replaced file keeps its permissions, and a new
one gets the usual mode for a new file (0666 less the umask on Unix). `serialize()` returns the bytes instead. Both run
the engine's save validation, so bytes Word would reject are refused, never
written.

## Resolve revisions

`revisions()` is the engine's census: one row per carrier, formatting changes
included, each with the `revision_id` that `project` selects (`0` marks a
census-only row that cannot be selected).

```python
pending = edited.revisions()
mine = {r["revision_id"] for r in pending
        if r["author"] == "J. Osei" and r["revision_id"] != 0}

accepted = edited.project("accept", ids=mine)
# Other authors' revisions stay pending; only the selected ones are resolved.
assert not any(r["revision_id"] in mine for r in accepted.revisions())

rejected_everything = edited.project("reject_all")
```

`project` takes the engine's `Resolution` shapes: `"accept_all"` and
`"reject_all"` take no ids; `"accept"` and `"reject"` require `ids` and leave
every other revision pending. An empty selection raises `InvalidRange`.
`read_accepted()` and `read_rejected()` are the all-or-nothing shorthands.
While a table carries a pending tracked cell merge (a `cell_merge` row, id
`0`), every resolution refuses, the shorthands included: resolving merge
changes is not supported, so `StemmaError` is raised with code
`UnsupportedEdit`.

## Compare two documents

```python
base = stemma.Document.open("as-sent.docx")
target = stemma.Document.open("as-returned.docx")

result = stemma.compare(base, target, author="Approved Reviewer")
print(result.semantic_change_count, "semantic changes")
redline = result.document
assert redline.read_rejected().to_text() == base.read_accepted().to_text()
assert redline.read_accepted().to_text() == target.read_accepted().to_text()
redline.save("changed.docx")
```

Both inputs are compared by their accepted readings. Pending revisions in an
input are flattened first and reported in `result.flattened_input_revisions`.
`author=None` produces an anonymous redline; an empty string is refused.

## Verify a delivery

`Document.review()` audits everything a document value changed since it was
opened. `stemma.audit(before, after)` asks the same question of any two files,
whoever produced them. Both return the same report:

```python
with open("contract.docx", "rb") as f:
    before = f.read()
with open("contract-redline.docx", "rb") as f:
    after = f.read()

report = stemma.audit(before, after)
tracked_only = (
    report["validator"]["ok"]
    and report["direct_changes"] == []
    and report["untouched"]["violations"] == []
    and all(
        r["disposition"] in ("untouched", "edited_inside")
        for r in report["preexisting_revisions"]
    )
)
assert tracked_only

assert stemma.validate(after) == {"ok": True, "issues": []}
```

| Key | What it holds |
|---|---|
| `new_revisions` | Revisions present after and not before, shaped like `revisions()` rows. |
| `preexisting_revisions` | Revisions pending before, each with `disposition` and `after_excerpt`. `untouched`: same content. `edited_inside`: changed only by the new tracked revisions (rejecting them restores it), such as an edit stacked inside another author's pending insertion; it is still pending and still theirs. `modified`: changed in a way the new revisions do not explain. `resolved`: no longer pending. |
| `direct_changes` | Committed-content changes no tracked change covers. Empty for a tracked delivery. |
| `untouched` | `verified_blocks`, the story `parts` walked, and `violations` of the proof that nothing else changed. |
| `validator` | The package verdict on the after bytes: `ok` and `issues`. |

`validate(data)` checks bytes exactly as they are, without the import-time
normalizations `Document.from_bytes` may apply and report in `diagnostics()`.
Invalid bytes are a result (`ok` is `False`), not an exception.

## Errors

Every engine refusal raises `stemma.StemmaError` or a subclass, and nothing was
changed: each call is atomic. The binding never retries, defaults, or swallows
an error. Each exception carries:

- `code`: the engine's stable refusal name, such as `"StaleEdit"` or
  `"ValidationFailed"`;
- `message`: the actionable explanation;
- `details`: a dict of structured context. Keys appear when the refusal has
  them: `block_id`, `step_index`, `context`, `stale_edit`,
  `opaque_preservation`, `ambiguous_anchor`, `author_label_collision`,
  `formatting`.

Subclasses exist for the codes below, so callers can catch the ones they
recover from:

| Exception | `code` | Do instead |
|---|---|---|
| `TransactionError` | `schema_error`, `adapter_error` | Fix the transaction against the [operation reference](operations.md). |
| `StaleEdit` | `StaleEdit` | Re-read the block and rebuild the operation. `details["stale_edit"]` carries the expected and actual guard or text. |
| `NoOpEdit` | `NoOpEdit` | Correct the intended text. |
| `AmbiguousAnchorAfterMove` | `AmbiguousAnchorAfterMove` | Anchor on `details["ambiguous_anchor"]["moved_to_block_id"]` or a stable neighbor. |
| `OpaqueDestroyed` | `OpaqueDestroyed` | Edit around the ids in `details["opaque_preservation"]`. |
| `AuthorLabelCollision` | `AuthorLabelCollision` | Continue the existing group with `allow_existing_author=True` only if intended; otherwise ask for another label. |
| `InvalidRange` | `InvalidRange` | Re-read `revisions()` and correct the selection. |
| `FormatRevisionConflict` | `FormatRevisionConflict` | Resolve or leave pending the revision in `details["formatting"]`. |
| `AmbiguousFormatTarget` | `AmbiguousFormatTarget` | Lengthen `expect` until it matches one span. |

Other engine codes (`InvalidDocx`, `ValidationFailed`, `AnchorNotFound`,
`UnsupportedEdit`, `PrefixDuplicatesLabel`, …) raise `StemmaError` itself;
switch on `code`. Several rows of the
[refusal vocabulary](mcp-advanced.md#refusal-vocabulary) arrive this way under
a broader code (`StyleNotFound` as `AnchorNotFound`,
`ParagraphContainsTrackedSegments` as `UnsupportedEdit`), and its MCP-only
rows (`MatchCountMismatch`, `doc_id_required`) are never raised here.

```python
stale_plan = doc.read()["blocks"][1]
changed_meanwhile = doc.apply({
    "ops": [{"op": "replace", "target": stale_plan["id"], "guard": stale_plan["guard"],
             "content": {"type": "paragraph", "content": [
                 {"type": "text", "text": "Liability is unlimited."}]}}],
    "revision": {"author": "Someone Else"},
})

try:
    changed_meanwhile.apply_authored({
        "ops": [{"op": "replace", "target": stale_plan["id"], "guard": stale_plan["guard"],
                 "content": {"type": "paragraph", "content": [
                     {"type": "text", "text": "Liability is capped."}]}}],
        "revision": {"author": "J. Osei"},
    })
except stemma.StaleEdit as error:
    assert error.details["stale_edit"]["expected"] == stale_plan["guard"]
    current = changed_meanwhile.read()["blocks"][1]  # re-read, then re-plan
```

File problems are ordinary Python errors: `Document.open` raises
`FileNotFoundError`, `PermissionError`, or another `OSError`, and `save` raises
`FileExistsError` for an existing path. Misused arguments raise `TypeError` or
`ValueError`.

## Mapping to the Rust facade

Each Python call is one Rust call. Rustdoc on these items is the detailed
contract.

| Python | Rust |
|---|---|
| `Document.from_bytes(data)` | `api::Document::parse` |
| `Document.open(path)` | read the file, then `api::Document::parse` |
| `doc.diagnostics()` | `Document::diagnostics` |
| `doc.read()` | `Document::read` (the [lean view](read-model.md#the-lean-view)) |
| `doc.to_markdown()`, `doc.to_text()` | `Document::to_markdown`, `Document::to_text` |
| `doc.apply(txn)` | `edit_v4::parse_transaction`, `into_edit_transaction`, `Document::apply` |
| `doc.apply_authored(txn, allow_existing_author=False)` | the same, then `Document::apply_authored` |
| `doc.check(txn)` | the same, then `Document::check` |
| `doc.project("accept_all")`, `("reject_all")` | `Document::project(Resolution::AcceptAll)`, `RejectAll` |
| `doc.project("accept", ids=...)`, `("reject", ids=...)` | `Document::project(Resolution::Selective { ids, action })` |
| `doc.read_accepted()`, `doc.read_rejected()` | `Document::read_accepted`, `Document::read_rejected` |
| `doc.revisions()` | `Document::revisions` |
| `doc.review()` | `Document::review` |
| `doc.serialize()` | `Document::serialize(&ExportOptions::default())` |
| `doc.save(path, overwrite=False)` | `serialize`, then the CLI's create-new commit (or an atomic replace) |
| `stemma.compare(base, target, author=...)` | `stemma_diff::diff_as_detailed` (`diff_detailed` when `author` is `None`) |
| `stemma.audit(before, after)` | `api::audit` |
| `stemma.validate(data)` | `api::validate` |

The read view and the report dicts are the engine's read shapes. Like the Rust
read model, they are engine-version-bound: read them, do not persist them.
Persist the DOCX bytes and the transactions, as described in
[Persist and replay](../guide/persistence.md).

The package ships type stubs (`py.typed`), and each method's docstring names
its Rust counterpart, so `help(stemma.Document.apply)` and an editor's hover
show the same contract.

## Related

- [v4 operation reference](operations.md): every operation a transaction may
  carry.
- [Read model reference](read-model.md): the blocks and segments `read()`
  returns.
- [Embed the engine](embedding.md): the same lifecycle from Rust.
- [Stability and compatibility](../guide/stability.md): what each surface
  promises.
