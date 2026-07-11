# Contributing to stemma

Thanks for helping. Stemma is a typed-IR DOCX engine with first-class
tracked-change semantics, plus an MCP server and a demo HTTP API in front of it.
The bar is correctness on real-world documents: output that opens clean in Word
and whose accept/reject matches the engine's.

## Dev setup

Tooling versions are pinned in [`.mise.toml`](.mise.toml):

```bash
mise install     # rust (pinned exact version — see .mise.toml), just, python 3.11
```

If you don't use `mise`, install the rust version pinned in
[`.mise.toml`](.mise.toml) (the toolchain lint runs on; `just lint` checks
this and names the fix if yours differs — the MSRV floor for merely
*building* is 1.91) and [`just`](https://github.com/casey/just) yourself. Python is only needed for the optional packaging/smoke scripts.

## The one command that must be green

```bash
just gate        # clippy (-D warnings) + the full daily test suite
```

This is the merge gate. It is hermetic — no network, no corpus, no real-Word
oracle — so it is green on a fresh clone with every env var unset. Budget a few
minutes (roughly ~8 on a cold build): it compiles the whole workspace and runs
the daily test tier. CI runs this same gate, plus a platform matrix, an MSRV
build, an npm-packaging smoke, and a contamination check (see
[`.github/workflows/ci.yml`](.github/workflows/ci.yml)).

The engine crate has a richer, engine-scoped gate with optional host-only tiers:
`just -f stemma-engine/Justfile --list`. Those extra tiers (corpus sweeps,
stress) skip gracefully when their env vars are unset and are not required for a
PR.

## Where to start reading

- [`docs/internals/architecture.md`](docs/internals/architecture.md) — the map:
  crates, the import → IR → edit → serialize loop, where each concern lives.
- [`stemma-engine/docs/domain-model.md`](stemma-engine/docs/domain-model.md) —
  the canonical data model: the core types, allowed shapes, transitions, and
  invariants. Read this before touching the public API.
- [`docs/internals/testing.md`](docs/internals/testing.md) — the test tiers and
  how to run each.

## Code philosophy (the short version)

The full statement lives in
[`stemma-engine/docs/domain-model.md`](stemma-engine/docs/domain-model.md); the
load-bearing parts:

- **No silent fallbacks.** If input is invalid, config is missing, decoding
  fails, or an invariant breaks, return a clear, typed error with context and
  stop. "Continuing" in an unknown state is the bug. An edit that would destroy
  something the engine cannot model fails loud, naming what it could not
  preserve — it never best-efforts past it.
- **Parse at the edges, keep the core clean.** Decode and validate raw input
  (DOCX bytes, wire JSON, CLI args) into domain types at the boundary. Core
  logic operates on already-validated types and does typed transformations.
- **Fail fast.** Validate at boundaries; assert internal invariants that
  represent "this must never happen".
- **Tests encode domain rules, not current behavior.** A test must be
  justifiable from the domain model. If you can't say *why* the expected value
  is correct without pointing at the implementation, the test is pinning the
  implementation, not the spec — fix the test first. When fixing a bug, correct
  the test to the intended behavior before correcting the code.
- **Simple beats clever.** Prefer the one good way used consistently. No
  patterns, registries, or abstraction layers for hypothetical extension.

## Pull requests

- `just gate` is green (clippy `-D warnings` clean, daily tests pass).
- No new `#[ignore]`d tests without a stated reason in the code. The suite runs
  ~1,060 spec-compliance tests; the handful that are disabled are disabled for a
  documented, non-gap reason. Keep it that way.
- **DOCX conformance claims need a spec citation.** If a change asserts what
  Word or the format requires, cite the section — ECMA-376 / ISO/IEC 29500, or
  the OPC part (e.g. "OPC §9.3", "ECMA-376 Annex A ordering"), matching the
  citation style already used throughout `stemma-engine/src/docx_validate.rs`.
  "Word does X" without a reference is not reviewable.
- Keep changes small and legible. Explain the *what* and *why* in the PR body;
  the [PR template](.github/pull_request_template.md) lists the checks.

## License

By contributing you agree your work is dual-licensed under
[MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE), the Rust-ecosystem
convention, with no additional terms.
