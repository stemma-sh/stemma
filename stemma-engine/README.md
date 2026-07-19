# stemma

A typed-IR DOCX compiler with tracked-change semantics.

Stemma parses a `.docx` into a canonical, typed intermediate representation
(`CanonDoc`), diffs and merges documents with first-class tracked-change
semantics, applies typed edit transactions, and serializes back to a `.docx`
that opens cleanly in Word. A post-serialization OOXML linker checks the output
for structural violations before bytes leave the engine.

> **Pre-1.0.** A `0.x` minor release may break API and wire contracts —
> deliberately, with changelog notice. The
> [stability policy](https://github.com/stemma-sh/stemma/blob/main/docs/guide/stability.md)
> states exactly what you can depend on today.

```
DOCX bytes -> import -> CanonDoc -> edit / diff -> Transaction -> apply -> CanonDoc -> serialize -> DOCX bytes
```

## Entity model (the one-paragraph version)

Think of stemma as a compiler over a long-lived document:

| Compiler concept | Stemma type | Durability |
|---|---|---|
| source file | DOCX bytes (`&[u8]`) | **durable** — the only authoritative artifact |
| parser | `crate::import` | pure function |
| AST | `CanonDoc` | ephemeral, engine-version-bound |
| compilation unit | `EditSnapshot` (IR + package scaffold) | ephemeral |
| edit/refactor spec | `edit::EditTransaction` | **durable** — small JSON |
| code generator | `serialize` / `SimpleRuntime::export_docx` | pure function |
| diff output | `DocumentDiff`, `ApplyResult` | derived |

**Persist the DOCX bytes plus the edit transactions.** Together they
reconstruct any past state: replay the transactions from a stored baseline.
**Do not persist the IR or an `EditSnapshot`** — the IR is engine-version-bound,
so stored snapshots become a migration problem on any engine release. Keep
snapshots hot in the runtime's handle store and re-derive on cold access via
`SimpleRuntime::import_docx`. The full rationale lives in the crate-level doc
comment in `src/lib.rs`.

## Quickstart

The full, runnable version of this loop is
[`examples/quickstart.rs`](examples/quickstart.rs) — run it with
`cargo run --example quickstart`. The abbreviated shape:

```rust,ignore
use stemma::api::Document;
use stemma::edit_v4::parse_transaction;
use stemma::{ExportMode, ExportOptions, ValidatorLevel};

// 1. Parse DOCX bytes into the typed model. Fails fast on anything unrecognized
//    (encrypted package, missing word/document.xml, ...).
let doc = Document::parse(&docx_bytes).expect("parse");

// 2. Read the designed projection (block ids, roles, visible text, tracked
//    status, opaque anchors, and the per-block staleness `guard`) — no raw IR
//    exposure. Addressing and staleness pinning both come from here.
let view = doc.read();
let target = &view.blocks[0];

// 3. Author one edit as a typed, schema-validated, precondition-checked
//    transaction. The block `guard` pins the op to what you just read: a stale
//    edit fails loud (StaleEdit) rather than corrupting the wrong block.
let txn_json = format!(
    r#"{{ "ops": [ {{ "op": "replace", "target": "{}", "guard": "{}",
                      "content": {{ "type": "paragraph",
                                    "content": [ {{ "type": "text", "text": "..." }} ] }} }} ],
          "revision": {{ "author": "you" }}, "summary": "..." }}"#,
    target.id, target.guard,
);
let txn = parse_transaction(&txn_json).expect("schema").into_edit_transaction().expect("adapt");
let edited = doc.apply(&txn).expect("apply");

// 4. Serialize. Opt into the built-in OOXML linker on the to-disk path so
//    structurally-corrupt output is refused at the source.
let out = edited.serialize(&ExportOptions {
    mode: ExportMode::Redline,
    validator_level: ValidatorLevel::Blocking,
    validator: None,
}).expect("serialize");
```

> The facade's error types (`RuntimeError`, the v4 `SchemaError` /
> `AdapterError`) implement `Display` + `std::error::Error`, so they
> `?`-propagate straight into `Box<dyn Error>`. Each still carries a structured
> `code` (and `details`); switch on it when you need machine-readable handling,
> box it when you just want to bubble it up.

The `SimpleRuntime` (a `DashMap<DocHandle, EditSnapshot>` session with TTL
eviction) is one opinionated session implementation; the engine itself owns no
durable state.

### Validator levels and the perf tradeoff

The built-in linker (`docx_validate::validate_docx`) re-parses every story part
multiple times and dominates serialize time (~29s out of ~33s for large
documents). So `ExportOptions::default()` uses `ValidatorLevel::Off` to keep the
hot export path fast. Paths that write a file to disk or hand bytes to an
external surface (the MCP `save_docx` / `compare_docx` tools) opt into
`ValidatorLevel::Blocking`, which refuses output that violates a structural
blocking rule (Word would reject the file or lose data). `ValidatorLevel::Full`
refuses on any error-severity finding. All three share one gate implementation
(`gate_serialized_bytes`) and the same `BLOCKING_RULES` set.

## Public surface

The **intended** public API is the [`api::Document`](src/api.rs) facade: build a
`Document` from DOCX bytes, call verbs (each returns a new `Document`), export
bytes back. New consumers should depend on `api` and nothing else.

The crate exposes more than the facade, in deliberate tiers (declared and
documented at the top of [`src/lib.rs`](src/lib.rs)):

1. **Facade** — `api`. The stable, documented v0.2.0 surface.
2. **Typed IR / domain model** — `domain`, `diff`, `table`, `table_diff`,
   `tracked_model`, `vocabulary`, `semantic_hash`, `redline_extract`,
   `roundtrip_compare`. The typed `CanonDoc` and its diff/redline views. These
   are public but **engine-version-bound** — do not persist the IR.
3. **Engine API (UNSTABLE)** — `edit`, `edit_v4`, `view`, `html`,
   `extended_markdown`, `import`, `runtime`, plus the OOXML part-level modules
   `docx`, `docx_validate`, `docx_validate_annotations`, `docprops`,
   `manual_markup`, `normalize`, `numbering`. These are a deliberate engine API
   that the in-workspace `stemma-mcp` server (which predates the facade) and
   downstream redline pipelines drive directly: `edit::apply_transaction`,
   `view::build_document_view`, `edit_v4::parse_transaction`, the validator,
   etc. They are **not** semver-stable and may change between minor versions.
   Treat them as "use only if you are inside the workspace and need
   transaction / view / part plumbing the facade does not yet route."

Everything else — the OOXML (de)serializer plumbing, the validator's
xref/namespace/ordering sub-checks, the styles/settings/word_ir part builders,
the OPC package writer — is sealed to `pub(crate)`. This keeps the v0.2.0
semver surface to the tiers above rather than freezing every internal helper.

### Why `stemma-mcp` reaches the engine API directly

`stemma-mcp` is a transport adapter (an MCP server). It maps wire requests onto
engine entry points — transactions, windowed reads, validation — several of
which the `Document` facade does not (yet) re-expose one-to-one. Rather than let
that access be ambient leakage, the engine entry points it needs are an
explicitly-labelled, explicitly-unstable Tier 3. As the facade grows to cover a
verb, the server should migrate onto `api::Document` and the corresponding
Tier-3 reach should shrink. The contract: **Tier 1 is stable; Tier 3 is the
acknowledged-unstable engine API; nothing else is public.**

## Documentation

- [`docs/domain-model.md`](docs/domain-model.md) — the canonical model: data,
  shapes, transitions, invariants. Read this before touching the public API.
- [`docs/testing_strategy.md`](docs/testing_strategy.md) — invariants, test
  tiers, and per-group recipes.
- [`docs/user/guide.md`](docs/user/guide.md) — a task-oriented user guide.

## Tests

Two tiers, both driven from `stemma-engine/Justfile`:

- **Daily** (`just -f stemma-engine/Justfile gate`): clippy `--all-features -D warnings`
  plus `cargo test -p stemma`. Corpus-free and no real-Word oracle — green with all env
  vars unset. This is the merge gate.
- **Confidence / nightly** (`just -f stemma-engine/Justfile gate-confidence`): the
  daily gate plus host-only `#[ignore]`d stress suites. These skip gracefully
  when `STEMMA_CORPUS_ROOT` / `STRESS_CORPUS_DIR` are unset.

A real-Word conformance tier (does stemma's output open clean in Word, and does
Word's accept/reject match stemma's?) exists, but is **not part of this crate**.
That tier is held out: it drives a real Word instance; nothing about it ships
with the engine, and it does not run on a public clone.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  <http://opensource.org/licenses/MIT>)

at your option (`license = "MIT OR Apache-2.0"`, the Rust-ecosystem convention).

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
