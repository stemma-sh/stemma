# Persist and replay

How a product stores documents and agent edits durably. The rule itself is
one line of the [stability policy](stability.md): persist DOCX bytes plus
edit transactions, never the in-memory model. This page is the rule as a
flow, and the reason it is the flow you want.

## Why this storage model fits agent edits

Storing bytes plus a transaction log is not a compromise forced by the
engine; it is the storage model an agent-editing product wants, because
three product features fall out of it for free:

- **Version history, undo, and audit are the log.** Every edit is a small,
  strict, self-describing JSON transaction with an author, a date, and a
  summary. Replaying a prefix of the log IS time travel; the log IS the
  audit trail. No separate history subsystem to build or migrate.
- **Human review of agent edits is native.** Transactions apply as tracked
  changes by default, so "the agent proposes, the human disposes" is not a
  feature you build on top; it is what the file format already says. The
  review UI reads [revision identities](../reference/read-model.md#revision-identity-and-the-review-loop)
  and resolves them selectively, and each accept or reject decision is one
  more entry in the same log (below).
- **Concurrent agents are already handled.** A transaction carries the
  `guard` of what it read; an agent editing a document a human just touched
  is refused atomically with `StaleEdit` and re-plans. Optimistic
  concurrency is in the write path, not in your storage layer.

## What to persist

| Persist | Why |
|---|---|
| The source DOCX bytes, immutable | The baseline every replay starts from. |
| The change log, append-only, in order | Every v4 transaction verbatim, AND every accept/reject decision as a [resolution entry](#resolutions-are-part-of-the-log), one ordered stream. |
| The engine version alongside the log | An OLDER engine rejects a transaction using a NEWER field (loudly, by design); the stamp tells you which engine the log is known-good under. |
| Exported checkpoints (optional, recommended) | Bound replay time and survive engine upgrades; see below. |

Never persist the IR, an `EditSnapshot`, a snapshot blob, or any read view.
The in-memory model is engine-version-bound; a stored copy becomes a
migration problem on the next release. Snapshot blobs
(`export_snapshot_blob`) are hot handoff between processes of the SAME
engine build, schema-checked and fingerprint-checked, cache entries by
contract.

## Storing a transaction

Store the transaction JSON exactly as the transport received it. It is
already strict (unknown fields and ops are hard errors) and additive across
engine releases: a newer engine keeps accepting an older log.

Two fields decide whether replay is deterministic, and both are the
caller's to pin:

- **Always set `revision.date`.** If omitted, the engine stamps the
  apply-time clock into the tracked change, so every replay would mint
  different dates. A stored transaction must carry the date it was
  originally applied with.
- **`apply_op_id` is caller-owned.** The engine never invents one. If your
  product groups changes by apply call, mint the id at admission time and
  store it inside the transaction like everything else.

One thing is deliberately NOT in the transaction: `allow_existing_author`,
the per-call assertion that continuing an existing author is intended. It is
transport policy, enforced at admission. Which leads to the one subtlety of
replay:

## Resolutions are part of the log

Edits are only half of this product's loop; the other half is the human
accepting or rejecting them, and those decisions must survive a cold resume
too. A resolution is NOT a v4 transaction (the transports list it as its own
plan shape), so the durable log is one ordered stream of two entry kinds:
v4 transactions, verbatim, and resolution entries, each recording the
action (`accept` or `reject`) and the selected revision identities, in the
position the decision was made.

Replaying a stored resolution works because revision identities are
DERIVED, not counted: the engine computes each identity as a content hash
of the revision's canonical record, so replaying the same edits over the
same baseline re-mints the same identities the original session handed the
review UI. The stored ids keep resolving for exactly the reason stored
guards keep matching.

Order is load-bearing, in both directions. A transaction recorded after a
resolution carries guards minted against the post-resolution state, so
skipping or reordering resolution entries fails the next transaction
loudly. And a resolution entry that no longer matches anything (a corrupted
or misplaced selection) is itself refused, an empty selection is an error,
so a bad entry is caught at its own log position rather than silently
absorbed.

The simpler alternative is CHECKPOINT-AFTER-RESOLVE: `serialize()` bakes
resolved changes into the checkpoint bytes, so if your product resolves
rarely (one review round at the end), checkpointing at each resolution and
replaying only later edits is an honest design with no resolution entries
at all. Products where review is continuous want the log entries; products
where review is terminal can take the checkpoint. Either way the
engine-upgrade rule below still applies: identity derivation is itself
versioned, so a log with resolution entries re-baselines at upgrade time
exactly like everything else.

## Replaying a log

Reconstruction is: import the stored bytes, then apply each entry in order
through the same parse path every transport uses. This snippet is
compile-checked against the real facade by the engine's doctests:

```rust,no_run
use std::collections::HashSet;
use stemma::api::Document;
use stemma::edit_v4::parse_transaction;
use stemma::tracked_model::ResolveSelectionAction;
use stemma::Resolution;

/// One durable log entry: an admitted v4 transaction, or a review decision.
enum LogEntry {
    Edit(String),
    Resolve(ResolveSelectionAction, HashSet<u32>),
}

fn replay(baseline: &[u8], log: &[LogEntry]) -> Result<Document, Box<dyn std::error::Error>> {
    let mut doc = Document::parse(baseline)?;
    for entry in log {
        doc = match entry {
            // Bare `apply`, deliberately: policy (the author-collision
            // guard) was already enforced when this call was first
            // admitted. Replay re-executes recorded facts; it does not
            // re-litigate them. Using `apply_authored` here would refuse
            // your own history at the first author who edited twice.
            LogEntry::Edit(json) => {
                doc.apply(&parse_transaction(json)?.into_edit_transaction()?)?
            }
            LogEntry::Resolve(action, ids) => doc.project(Resolution::Selective {
                ids: ids.clone(),
                action: *action,
            })?,
        };
    }
    Ok(doc)
}
```

On the same engine version, replaying an untouched log over untouched bytes
is deterministic: block ids, guards, and revision identities all re-derive
to the same values, which is exactly why the guards stored inside later
transactions keep matching and the ids stored inside resolution entries
keep resolving.

## When replay fails

An entry that applied once can refuse on replay in only three ways: the log
was reordered, edited, or partially lost; the baseline bytes are not the
bytes the log was recorded against; or the engine version changed underneath
a log that was never re-baselined.

What the engine does: refuses loudly and atomically. A guard mismatch is
`StaleEdit` carrying the expected and actual hash; nothing partial applies;
the document state remains the last good transaction's result. Guards are
scheme-versioned (a `v2:` prefix today; legacy bare-hex guards still
validate under the formula they were minted with), so a stored transaction
keeps replaying across guard-formula upgrades.

What your store should do: treat a replay refusal as corruption or version
drift, and fall back to the newest checkpoint. Do NOT re-anchor: a stored
transaction is an immutable fact, and rewriting its `guard` or `expect` to
force it through fabricates a history that never happened, silently, which
is the one thing this storage model exists to prevent.

What is promised across engine versions is that the FORMAT stays
interpretable (additive schema, old logs keep parsing), not that a newer
engine reproduces byte-identical output for an old log. So upgrade the way
the model wants you to:

## Checkpointing

Every N transactions (and always before an engine upgrade), `serialize()`
the current document and store the bytes as a checkpoint. A checkpoint is a
complete, self-contained DOCX: pending tracked changes live in the bytes,
and revision identities re-derive deterministically when it is re-imported.
From then on, replay is checkpoint plus log suffix:

- replay stays bounded no matter how long the document lives;
- an engine upgrade re-baselines cleanly: checkpoint on the old version,
  verify on the new one, and let the old log segments retire with their
  stamp;
- recovery from a corrupted log tail is the same operation as an upgrade.

## Cold resume in a host

A server built on `SimpleRuntime` composes with this directly: sessions are
in-memory and evictable precisely BECAUSE the durable state lives in your
store. On demand, `import_docx` the checkpoint (or baseline), replay the log
suffix, serve the session; on idle, let `evict_expired` drop it without
ceremony. See [Embed the engine](../reference/embedding.md).

## Related

- [Embed the engine](../reference/embedding.md): the runtime this flow
  assumes.
- [Stability and compatibility](stability.md): the one-line rule this page
  expands, and the v4 additivity contract.
- [v4 operation reference](../reference/operations.md): the transaction
  format being stored.
- [Editing](editing.md): the review-before-save discipline at authoring
  time.
