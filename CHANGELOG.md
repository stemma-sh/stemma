# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html). Being
pre-1.0, minor (`0.x`) releases may include breaking changes; see
[docs/guide/stability.md](docs/guide/stability.md).

## [Unreleased]

## [0.1.0] — Initial public release

First public release of stemma: a typed-IR DOCX compiler with first-class
tracked-change semantics, and the transports that put it in front of agents and
applications.

### Added

- **`stemma-engine`** — the core crate. Imports `.docx` into a canonical, typed
  IR (`CanonDoc`), diffs and merges with tracked-change semantics, applies typed
  edit transactions, and serializes back to a `.docx` that opens clean in Word.
  A post-serialization OOXML linker checks the output against codified
  ECMA-376 / ISO 29500 structural invariants before bytes leave the engine.
  Opaque content the engine does not model (equations, drawings, embedded
  objects, content controls) round-trips byte-faithfully or fails loud —
  never silently dropped.
- **Tiered public API.** A stable `api::Document` facade (Tier 1) over a
  typed-IR/domain-model tier (Tier 2) and an explicitly-unstable engine API
  (Tier 3); everything else is sealed. See
  [`stemma-engine/README.md`](stemma-engine/README.md) and
  [docs/guide/stability.md](docs/guide/stability.md).
- **`stemma-mcp`** — an MCP server exposing the engine as 28 tools over stdio:
  read/navigation, tracked-change editing, and review (selective accept/reject,
  validate, dry-run). Distributed on npm as `@stemma-sh/mcp` (`npx -y
  @stemma-sh/mcp`): a launcher package over per-platform prebuilt binaries,
  published by the tag-triggered release workflow alongside GitHub-release
  archives (see [RELEASING.md](RELEASING.md)).
- **`stemma-api`** — a demo HTTP/JSON adapter that serves a browser, Word-style
  review editor (`stemma-examples`) from a single `cargo run -p stemma-api`,
  including `POST /api/compare` for producing a redline from two uploaded
  documents. Local-only demo infrastructure (see [SECURITY.md](SECURITY.md)).
- **`stemma-cli`** — the `stemma` command-line tool: `compare` (redline two
  files), `extract` (text or JSON with pending tracked changes), `resolve`
  (accept/reject by id, author, or all), `validate`. See
  [docs/reference/cli.md](docs/reference/cli.md).
- **Conformance suite** — a hermetic daily gate (`just gate`: clippy with
  warnings denied plus the full daily test tier) with ~1,060 spec-compliance
  tests tied to ECMA-376 / ISO 29500 / MS-OI29500 constraints, plus optional
  host-only corpus and stress tiers.
- **Benchmark report** — model sweeps with deterministic gates and disclosed
  losses; see [docs/benchmarks.md](docs/benchmarks.md), every number backed by
  per-cell data.
- Dual-licensed under MIT OR Apache-2.0.

[Unreleased]: https://github.com/stemma-sh/stemma/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/stemma-sh/stemma/releases/tag/v0.1.0
