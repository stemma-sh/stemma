# Embed the engine

How to build a product, a viewer, an editor backend, a hosted service, on the
engine itself. The demo HTTP server is deliberately not that surface; this
page is.

## Where the contracts live

This documentation set owns the DOCUMENT contract; the Rust crate owns the
API-surface detail. The handoff is deliberate, so here it is explicitly:

| You need | It lives in |
|---|---|
| The write vocabulary: every operation, field, and canonical shape | [v4 operation reference](operations.md) |
| The read shapes you render from | [Read model reference](read-model.md) |
| Refusal vocabulary and recovery | [MCP core](mcp.md#refusal-vocabulary) and [advanced](mcp-advanced.md#refusal-vocabulary) tables (engine-owned names, shared by every transport) |
| Stability promises per surface | [Stability and compatibility](../guide/stability.md) |
| Durable storage and replay | [Persist and replay](../guide/persistence.md) |
| Full method lists, constructors, error taxonomy | [`stemma-engine` README](https://github.com/stemma-sh/stemma/blob/main/stemma-engine/README.md) and rustdoc (`cargo doc -p stemma --open`) |
| Worked, compile-gated code | `stemma-engine/examples/` (each is runnable via `cargo run -p stemma --example <name>`) |

Method-level detail deliberately stays in rustdoc, where the compiler keeps it
honest. Everything below is orientation: the shapes of a correct embedding,
each at its honest stability tier.

## The document lifecycle (Tier 1)

`api::Document` is the stable facade
([Tier 1](../guide/stability.md#rust-api)) and the intended dependency for a
new consumer. Every verb returns a NEW `Document`; nothing mutates in place,
so holding, caching, and branching document states is plain value handling.

This snippet is compile-checked against the real facade by the engine's
doctests:

```rust,no_run
use stemma::api::Document;
use stemma::edit_v4::parse_transaction;

fn one_edit_cycle(source: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    // Bytes in.
    let doc = Document::parse(source)?;

    // Read: the lean view carries block ids, text, and each block's `guard`.
    let view = doc.read();
    let target = &view.blocks[1];

    // One v4 transaction, exactly as any transport would receive it. The
    // guard from the read rides along; if the block changed since, apply
    // fails loud with StaleEdit instead of editing the wrong text.
    let txn = parse_transaction(&format!(
        r#"{{"ops":[{{"op":"replace","target":"{id}","guard":"{guard}",
            "content":{{"type":"paragraph","content":[
                {{"type":"text","text":"Liability is capped at twice the fees paid."}}]}}}}],
           "revision":{{"author":"J. Osei","date":"2026-07-06T10:30:00Z"}}}}"#,
        id = target.id,
        guard = target.guard,
    ))?
    .into_edit_transaction()?;

    // Tracked by default; the author-collision guard is enforced here.
    let edited = doc.apply_authored(&txn, false)?;

    // Project, audit, serialize.
    let _clean = edited.read_accepted()?;
    let _report = edited.review()?;
    Ok(edited.serialize(&stemma::ExportOptions::default())?)
}
```

The read half of that loop (what `read()` and the full render view contain)
is the [read model reference](read-model.md); the write half (every op the
transaction accepts) is the [operation reference](operations.md). Runnable
versions: `cargo run -p stemma --example my_first_edit` and
`--example review_before_save`.

One import rule covers the whole facade: **every type a `Document` signature
names is importable from `stemma::api`**, next to `Document` itself:
`Resolution`, `ResolveSelectionAction`, `ExportOptions`, `RuntimeError`,
`RevisionRecord`, and the rest. (Most are also re-exported at the crate
root; `stemma::ExportOptions` and `stemma::api::ExportOptions` are the same
type.) Nothing on this page requires knowing which internal module defines
what.

## Resolving a redline (Tier 1)

The other half of a document lifecycle: the redline arrives from outside,
and your product's job is to enumerate it, resolve it, and save the result.
Three facts carry the whole flow:

- **`Document::revisions()` is the census.** One enumeration across every
  story and every revision kind, formatting changes included. Each
  `RevisionRecord` carries `revision_id: u32` (the engine-minted identity
  selective resolution addresses; `0` marks the rare census-only record that
  is reported but never selectable), `author`, `date`, `kind`, the hosting
  `block_id`, and an excerpt.
- **A projection is a full `Document`.** `project`, `read_accepted`, and
  `read_rejected` return the resolved document, not a text view: read it,
  enumerate it, project it again, serialize it. Passes chain in memory with
  no re-parse between them, and ids enumerated from one pass stay valid on
  the next (content-derived identity).
- **`serialize` validates.** `ExportOptions::default()` runs the same
  blocking structural gate `save_docx` uses, so bytes that reach your
  storage are bytes Word will open without flagging repairs.

Compile-checked, like everything on this page:

```rust,no_run
use std::collections::HashSet;
use stemma::api::{Document, Resolution, ResolveSelectionAction};

fn resolve_and_save(source: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let doc = Document::parse(source)?;

    // Triage by author: accept everything opposing counsel proposed...
    let theirs: HashSet<u32> = doc
        .revisions()
        .into_iter()
        .filter(|r| r.author.as_deref() == Some("Opposing Counsel"))
        .map(|r| r.revision_id)
        .collect();
    let after_accept = doc.project(Resolution::Selective {
        ids: theirs,
        action: ResolveSelectionAction::Accept,
    })?;

    // ...then, on the projection, reject everything still pending. Re-reading
    // the census from the value you are about to resolve is the discipline;
    // rejecting the full remainder could equally be `read_rejected()`.
    let rest: HashSet<u32> = after_accept.revisions().iter().map(|r| r.revision_id).collect();
    let resolved = after_accept.project(Resolution::Selective {
        ids: rest,
        action: ResolveSelectionAction::Reject,
    })?;

    // Nothing pending in memory, and validated resolved bytes out.
    assert!(resolved.revisions().is_empty(), "mixed triage fully resolved");
    Ok(resolved.serialize(&stemma::ExportOptions::default())?)
}
```

Runnable, with content-level verification of each resolution:
`cargo run -p stemma --example resolve_a_redline`.

## Hosting sessions (Tier 3: `SimpleRuntime`)

A server needs what the facade deliberately does not carry: session identity,
admission, and eviction. `SimpleRuntime` is the engine's one opinionated
session implementation, the same one the MCP server runs on. It is
[Tier 3](../guide/stability.md#rust-api): real, supported, and
version-bound; a hosted deployment that outgrows it wraps its own policy
around the same facade instead.

What it actually is:

- **A handle IS a session.** `import_docx(bytes)` returns a `DocHandle`; the
  runtime keys working state by that handle in a concurrent map. There is no
  user or tenant concept in the engine; your transport owns that mapping.
- **Internally thread-safe.** All methods take `&self`; hold an
  `Arc<SimpleRuntime>` directly, no outer lock. State is copy-on-write: reads
  share the document tree cheaply, a mutation clones it once.
- **Per-handle memory** is the parsed document tree, the package scaffold,
  and one immutable copy of the source bytes (the session-review baseline).
  The tree is engine-version-bound and lives only in memory; durability is
  the [persist and replay](../guide/persistence.md) discipline.
- **Eviction is caller-driven.** There is no background sweeper.
  `evict_expired(ttl_secs)` drops every handle idle longer than the TTL and
  returns the count; the MCP server calls it lazily at the top of each
  request, which is the intended pattern. `contains_handle` lets transport
  state check liveness after a sweep instead of keeping a second clock.
- **The write entry point for a caller-attributed edit** is
  `apply_edit_authored(handle, &txn, allow_existing_author)`, the same
  author-collision policy every transport enforces. `review_session(handle)`
  audits everything the handle changed since open, against the retained
  open-time baseline; `clone_handle` branches a session that shares that
  lineage.
- **Hot handoff, not storage.** `export_snapshot_blob` /
  `import_snapshot_blob` move a working session between processes running
  the SAME engine build (schema-version checked, fingerprint checked). They
  are short-TTL cache entries by contract, never durable storage.

Signatures, exact semantics, and the rest of the method set: rustdoc on
`stemma::runtime::SimpleRuntime`.

## Concurrency, stated plainly

The intended model is TURN-BASED editing with optimistic concurrency, not
real-time co-editing. There is no operational transform and no CRDT, and no
such guarantee should be designed against.

The whole mechanism is the guard: a read hands out each block's `guard` (its
semantic hash at read time), a write carries it back, and a write against a
block that changed in between is refused loudly with `StaleEdit`, atomically,
before anything applies. For a document shared by a human and their agent,
that is the entire story: the agent plans against a read; if the human
touched the document first, the agent's apply fails as a unit.

Recovery from `StaleEdit` is always the same loop:

1. Re-read the block (fresh text, fresh `guard`).
2. Rebuild the operation against what is actually there now; the refusal
   carries the expected and actual values to diff against.
3. Re-apply. Never retry the stale transaction unchanged, and never strip
   the guard to force an edit through.

Multiple agents on one document compose the same way: each collision costs
one refused transaction and one re-plan, and no interleaving can corrupt the
document, because every transaction is atomic and precondition-checked.

## Related

- [Persist and replay](../guide/persistence.md): the storage flow this
  runtime assumes.
- [Read model reference](read-model.md): what you render from.
- [v4 operation reference](operations.md): what you write with.
- [Stability and compatibility](../guide/stability.md): the tier vocabulary
  used above.
- [HTTP API reference](http.md): the demo transport (explicitly not a
  product surface).
