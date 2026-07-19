# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html). Being
pre-1.0, minor (`0.x`) releases may include breaking changes; see
[docs/guide/stability.md](docs/guide/stability.md).

## [Unreleased]

## [0.2.0] — 2026-07-18

### Added

- **Engine-minted revision identity.** One user intention now produces one
  engine revision identity across the affected OOXML carriers. Audits,
  untouched-scope proofs, selective resolution, and transport receipts key on
  stable revision identities rather than parser-local counters or reminted
  wire ids.
- **Focused approved-worklist CLI.** `stemma apply INPUT --worklist FILE -o
  OUTPUT` applies the experimental `stemma.worklist.v0` as native tracked
  changes only after the worklist's SHA-256 and byte count match the exact
  input. It audits preservation and untouched scope, then commits an
  authoritative create-new `stemma.apply_receipt.v0` sidecar with exact
  artifact identities and every item outcome before any DOCX. Partial
  worklists exit `3` and create no DOCX by default; `--emit-partial` may create
  an explicitly non-deliverable diagnostic redline without changing that
  status or exit code. Receipts identify the exact running executable and make
  output persistence conditional on actual exit, presence, byte size, and hash
  agreement rather than treating a pre-commit receipt as delivery proof.
- **Publishable `stemma-artifacts` boundary.** The shared MCP/CLI host boundary
  identifies exact input/output bytes with SHA-256, stages output in the
  destination directory, commits create-new without clobbering, and verifies
  the committed bytes before reporting success.
- **MCP workspace confinement.** `STEMMA_MCP_WORKSPACE_ROOT` confines MCP reads
  and writes, defaulting to the canonical startup current directory. Relative
  paths resolve under it and source symlinks may not escape it.
- **Portable path receipts.** Non-UTF-8 supplied or canonical paths fail loudly
  before reads or staging, preventing lossy identities and JSON serialization
  panics.
- **Portable regular-file paths.** Windows alternate-data-stream syntax is
  refused on every platform before reads or staging. Obvious FIFOs, devices,
  and directories are rejected before open and the opened handle is checked
  again, preventing a no-writer FIFO from blocking the transport edge.
- **MCP image resource bounds.** Path-backed image edits default to 20 MiB per
  image and 50 MiB aggregate per transaction, measured before base64 expansion.
  Either limit can be configured or disabled independently; over-limit reads
  fail as `artifact_source_too_large`.
- **Bounded MCP revision workflows.** `inspect_docx` adds
  `revisions_summary`, with exact totals grouped by author and kind, and
  revision resolution adds an AND-combined `by_filter` selector. Resolution
  receipts report exact selected/matched/resolved counts, cap listed
  identities explicitly, and report every truncated list with omitted counts
  and a follow-up route.

### Changed

- `MatchCountMismatch` refusals (CLI worklist and MCP `replacement_worklist`)
  now lead with the safe remediation: narrow the target to the intended site
  using the listed matches; raising `expected_matches` (or `"all"`) is advised
  only after verifying every listed occurrence is intended. The server
  instructions state the same rule. Previously the error suggested passing
  `expected_matches` first, which invited confirming ambiguity instead of
  resolving it.
- Full accept/reject now follows Word-native mixed-move semantics, including
  nested move-range markers, and descends into revision-bearing glossary
  document parts and their related stories. Selective resolution remains
  identity-bound to modeled revisions.
- Compact revision inspection now accepts the same author, kind, and bounded
  block-range filters as the advanced inventory. Verification reports input
  validation separately from newly introduced issues, and the untouched
  comparator correctly treats reminted stacked-revision identifiers as
  ephemeral while still checking their authored metadata. Granular table
  operations may compose structural row/cell changes within one atomic
  transaction; repeated guards on one table are evaluated against the atomic
  transaction's inspected base snapshot, while pre-existing mid-redline tables
  remain refused.
- Claude plugin packaging no longer bundles a separate agent skill. MCP
  initialize instructions and tool descriptions are the single canonical
  guidance source across plugin, npm, MCPB, and direct stdio installs.
- The default MCP profile is now the complete five-stage compact front end:
  `open_docx -> inspect_docx -> execute_plan -> verify_docx -> save_docx`.
  Inspection defaults to the first 16 rows of a paged compact index and multiplexes bounded
  find/window, paged document, block, revision, and style projections;
  block inspection defaults to exact guarded planning detail and exposes the
  complete run-formatting projection through explicit `detail: "formatting"`;
  table finds return only matching cell excerpts and all finds are explicitly
  paged (16 blocks and four matching cells per table by default, each with
  independent continuation metadata); execution handles the
  existing atomic v4 transaction, an explicit non-atomic replacement worklist
  with per-item outcomes, revision resolution, or a two-file comparison
  producer plan with receipts that omit whole-table content; inspection also
  exposes editable note bodies, historical accepted/rejected/redline and
  section projections, and a parser-derived operation catalog that maps all 26
  historical tools onto the five-tool core;
  replacement worklists now support exact throwaway preview, typed match and
  barrier modes, and formatting-preserving table-cell paragraph splices in
  their default whole-body scope; the document projection is paged at 16
  top-level blocks by default instead of returning an unbounded payload;
  verification reuses the session and producer-neutral
  audit kernels while paging every audit section at 16 rows by default (64
  maximum) with totals and continuation metadata. Comment annotations,
  direct OOXML property changes, and committed revision-resolution effects
  are classified explicitly; comment anchors and hyperlink retargeting are
  accounted changes rather than false untouched-scope violations.
  Set `STEMMA_MCP_PROFILE=advanced` to restore the legacy 31-tool surface; this
  is the migration path for callers that still need individual expert verbs.
- Operation failures now return actionable, operation-specific errors with
  target context and review-round guidance instead of generic execution
  failures.
- Worklist receipts retain an outcome for every requested item even when
  evidence details are bounded. Resolution receipts keep exact totals
  authoritative and disclose omitted evidence rows rather than silently
  dropping them.
- MCP `check_edit` and `apply_batch(preview=true)` now execute and discard the
  same package-aware, author-protected snapshot apply used by commit. Preview
  can no longer approve an origin-author impersonation or dangling style that
  the persisted path would refuse.
- MCP `open_docx`, save/compare/audit/review render, and persisted image-backed
  edit responses add artifact identity while retaining their existing response
  keys. Image sources register only after mutation applies; registration and
  save/review export are coupled, repeated exact sources deduplicate within the
  session, and source identity expires with the document TTL. Artifact failures
  use the documented `artifact_*` error codes. Every successful object response
  and structured error also reports the exact `server_version` build identity.
- Release-candidate binaries embed `version+g<commit>` and must pass the real
  confined-workspace MCP smoke plus the mandatory safe-artifact wire harness
  on every native release target before upload. Each archive carries the
  machine-readable report, including the exact binary SHA-256. A protected
  environment holds tag creation and publication until qualification approval;
  an aggregate manifest first re-verifies all five binaries, architectures,
  reports, timestamps, build stamps, and stable case sets. Publish jobs verify
  downloaded bytes against that manifest again, refuse cross-SHA reuse through
  SHA-stamped native packages, publish lifecycle-free prepacked npm tarballs
  only at identical measured and registry integrity, require active tag
  update/deletion protection, claim the approved tag before npm publication,
  recheck it immediately before release publication, and
  expose the GitHub release only after its exact draft asset set is complete.
- CLI `compare` and `resolve` now refuse every existing output, not only an input
  alias. Their existing stderr success line appends byte length, SHA-256,
  `collision_policy=create_new`, and `disposition=created`.
- Wave 7 conformance fixes preserve formatting-change revisions with absent or
  populated previous-property payloads; derive move identity from nested range
  markers; re-nest same-name smart tags by wrapper polarity; compare hyperlink
  and decoration identity rather than ephemeral counters; evaluate atomic table
  guards from the base snapshot; re-emit explicit autospacing and underline-off
  tri-state overrides; inject note-reference styles only for authored
  references; coalesce equal-status untouched-proof segments; tolerate two
  Word-verified wildcard emissions; and no longer enforce a deleted-cell final
  mark as a document-state invariant.

The safe artifact boundary reduces ordinary caller mistakes and failed-write
damage. It is not a sandbox against hostile same-user processes, a storage
integrity guarantee, or a power-loss durability promise.

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

[Unreleased]: https://github.com/stemma-sh/stemma/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/stemma-sh/stemma/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/stemma-sh/stemma/releases/tag/v0.1.0
