# Editing

How changes are made, and the discipline that keeps them safe.

## Transactions

An edit is a **transaction**: a list of ops, an author, an optional summary.
Transactions are atomic — every op applies or none do. Ops target block ids
from the outline and carry an `expect` precondition (the text the op assumes
is there) or a content hash; if the document has changed under you, the op
fails with a stale-edit error instead of mutating the wrong content.

Two materialization modes:

- **Tracked** (the default): the edit lands as proper revision markup a
  reviewer can accept or reject. This is the mode for anything a human will
  review.
- **Direct**: the edit is applied as if a tracked edit had been made and
  immediately accepted — same semantics, no markup. Use it only when the
  task explicitly wants untracked changes.

## Receipts and refusals

Every successful write returns a receipt: the ids of blocks that actually
changed, the revision ids created, move destinations. Trust the receipt over
your own mental model of what the edit did.

Every refusal names its escape hatch. A stale anchor tells you to re-read
the block; an ambiguous match lists every candidate so you can disambiguate
in one step; an author collision names the explicit override. If you hit an
error that leaves you stuck with no next move, that is a bug in stemma —
report it.

Two rules that exist because real agent transcripts showed their absence
failing:

- **Scope edits minimally.** "Replace the notice address" means the tokens
  the instruction denotes — not the trailing qualifiers next to them. Prefer
  the surgical verbs (`replace_text` with an exact needle) over widened
  spans.
- **Sessions are not interactive.** Decide, apply, and always save; an edit
  that is never saved does not exist.

## Review, then save

Saving is the commit gate. Before writing bytes, reconstruct what the
recipient will actually receive — the accept-all projection, or a full
session review (`review_session`: everything this session changed, proof
that everything else is untouched, and a validator verdict) — and check it
against what you were asked to do. The engine validates structure on every
save; *whether the change is the right change* is only checkable against
the projection, and "looked right in my live view" is the failure mode that
survives everything else.

The MCP `review_session` verb and the Rust `Document::review()` are the same
read-back — the census of what you changed, the direct (untracked) delta, and a
proof that everything else is untouched:

```rust
let report = doc.review().expect("review the session");
assert!(
    report.direct_changes.is_empty(),
    "every edit was tracked — an untracked delta here would itself be a finding"
);
assert!(
    report.untouched.violations.is_empty(),
    "everything outside the two edited paragraphs is provably untouched"
);
// Only now, having read back what the recipient receives, commit to bytes.
let out = doc.serialize(&ExportOptions::default()).expect("serialize validated DOCX");
```

Runnable: `cargo run -p stemma --example review_before_save`.

Saving always targets a new, unused path. Inputs and every existing destination
are refused; there is no overwrite option. MCP paths must also stay inside its
configured workspace root. Successful transport commits report the exact byte
length and SHA-256 of the create-new artifact.

For MCP image edits backed by a server-side `path`, source identity joins the
receipt and session only after the mutation applies; edits rejected before
mutation and previews register nothing. The session deduplicates repeated exact
sources and couples registration with save/review export. Source identity state
has no independent TTL and is removed only after the runtime confirms the
document was evicted; missing state for a live document fails closed. Image path
reads are bounded before base64 expansion; see the [MCP
reference](../reference/mcp.md#filesystem-and-artifact-boundary) for the defaults
and configuration variables.

Next: [Fidelity](fidelity.md) — what the output does and doesn't promise.
