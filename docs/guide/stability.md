# Stability and compatibility

Stemma is pre-1.0. Under standard [`0.x` semver](https://semver.org/#spec-item-4),
a minor version (`0.MINOR.PATCH`) may contain breaking changes. This page states
what "breaking" means for each surface stemma exposes, so you can depend on it
deliberately. It describes the contract the project already follows, not an
aspiration.

There are two kinds of surface: the **Rust API**, which is linked into your
binary, and the **wire surfaces**, which carry JSON across a process boundary
for transactions, MCP tool calls, and HTTP. They evolve under different rules.

## Rust API

The Rust surface is tiered, and the tiers *are* the stability contract. This is
documented in full at the top of
[`stemma-engine/src/lib.rs`](../../stemma-engine/src/lib.rs) and in
[`stemma-engine/README.md`](../../stemma-engine/README.md); in short:

- **Tier 1: the `api::Document` facade.** The intended public surface and the
  one new consumers should depend on. Stable within a `0.x` minor release line.
- **Tier 2 covers the typed IR and domain model** (`domain`, `diff`, `table`,
  `tracked_model`, …). Public but **engine-version-bound**: do not persist the
  IR. Its shape can change with any engine release.
- **Tier 3 covers the unstable engine API** (`edit`, `edit_v4`, `view`, `import`,
  the part-level modules, …). A deliberate, explicitly-unstable surface that the
  in-workspace transports drive directly. May change between minor versions.
- Everything else is sealed (`pub(crate)`).

Persist **DOCX bytes plus edit transactions** for durability. Never persist the
IR or an `EditSnapshot`. Together the bytes and transactions reconstruct any
past state by replay; a stored snapshot becomes a migration problem on the next
engine release.

## Wire surfaces

### v4 transaction JSON: additive, unknown fields rejected

The v4 edit-transaction schema
([`stemma-engine/src/edit_v4.rs`](../../stemma-engine/src/edit_v4.rs)) is the
durable, cross-boundary edit format. Its evolution is **additive**: new optional
fields and new op types may appear in a minor release; the meaning of an existing
op does not change under it.

Deserialization is strict. Every transaction struct is
`#[serde(deny_unknown_fields)]`. An unrecognized field is a **hard error**, not a
warning, and this is deliberate: a misspelled or misplaced key is far more likely
to be an authoring mistake that would silently no-op than an intended input, and
silently ignoring it is exactly the "best-effort into an unknown state" this
project refuses. Author against the fields the schema defines; do not send
speculative keys expecting them to be ignored.

Because the format is additive and strict, a transaction that a newer engine
accepts is still accepted by that engine after a minor bump; a transaction using
a *newer* field will be rejected by an *older* engine (rather than
mis-interpreted). Any breaking change to an existing field's meaning gets a
[CHANGELOG](../../CHANGELOG.md) entry.

### MCP tool schema: additive, renames are announced

The [MCP tool surface](../reference/mcp.md) evolves additively within `0.x`:

- A tool's argument schema may gain **optional** parameters in a minor release.
- New tools may be added.
- A successful response may gain additive identity or diagnostic fields. Existing
  response keys keep their meaning; clients should ignore response fields they
  do not consume.
- Making a previously-optional parameter required, renaming a tool or a
  parameter, or removing one is a breaking change and gets a
  [CHANGELOG](../../CHANGELOG.md) entry.

Build agents against documented tool arguments; unknown arguments are rejected at
the wire edge for the same reason transactions reject unknown fields.

### Task manifest: versioned and strict

`stemma.task_manifest.v1` is a persisted evidence format. Its schema field is
mandatory, all objects reject unknown fields, and cross-field invariants are
validated before emission and after decoding. An incompatible shape receives a
new schema identifier; `stemma verify-task` refuses an unknown schema rather
than guessing. Additive producer or diagnostic information therefore still
requires an explicit schema decision instead of silently changing what an
offline verifier trusts.

### HTTP API: demo surface, no stability promise

[`stemma-api`](../reference/http.md) is demo infrastructure for the browser
editor, not a product surface. Its routes and JSON shapes **carry no stability
guarantee** and may change or disappear in any release without a changelog entry.
It is also loopback-only with no authentication. See
[SECURITY.md](../../SECURITY.md). If you need a stable programmatic surface,
embed the Rust `api::Document` facade or drive the MCP tools.

## Summary

| Surface | Within a `0.x` minor line | Across `0.x` minors |
|---|---|---|
| Rust Tier 1 (`api::Document`) | stable | may break, with changelog |
| Rust Tier 2 (typed IR) | engine-version-bound; do not persist | may change |
| Rust Tier 3 (engine API) | unstable | may change |
| v4 transaction JSON | additive; unknown fields rejected | breaking meaning-changes get a changelog entry |
| MCP tool schema | additive (optional params) | renames/removals get a changelog entry |
| Task manifest JSON | exact versioned schema; unknown fields rejected | incompatible shapes get a new schema identifier |
| HTTP API | no promise | no promise |
