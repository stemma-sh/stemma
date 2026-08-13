# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html). Being
pre-1.0, minor (`0.x`) releases may include breaking changes; see
[docs/guide/stability.md](docs/guide/stability.md).

## [Unreleased]

### Changed

- **Ordinary worklists no longer require precomputed document identity.**
  `stemma.worklist.v0` accepts an omitted `input` binding for the direct
  apply path while retaining exact hash-and-size verification whenever the
  binding is supplied. Every apply receipt still records the actual input and
  now reports `input_binding: unbound` or `input_verified` explicitly.

## [0.5.1] — 2026-08-13

### Added

- **The public demo is an executable release artifact.** A synthetic agreement,
  exact MCP instruction, approved worklist, expected native redline, and
  accept/reject projections now live together under `demo/`. The merge gate
  regenerates the redline, compares its document and revision model with the
  checked-in result, and verifies both generated and checked-in accept/reject
  projections. The checked-in Word visual is captured from that synthetic
  workflow before the release commit is frozen; release qualification confirms
  the same behavior against the candidate.
- **CI has one stable required status.** `required CI gate` depends on every
  mandatory job, including the new demo check, so default-branch protection
  can require one status without silently losing coverage when jobs are
  renamed or split.
- **GitHub releases now carry their own usable summary.** Release notes are
  rendered from the exact version's changelog and include installation,
  platforms, pre-1.0 status, product scope, and version-pinned documentation
  links alongside the live documentation site. Creation and recovery both
  refuse an existing draft whose body differs from the source-controlled
  rendering; recovery reads changelog content from the requested release
  commit, not the newer workflow checkout.

### Changed

- **The MCP privacy statement now describes the actual data boundary.** Stemma
  does not upload documents to a Stemma-operated service, and path-based parsing
  and writes remain local; the docs now state that an MCP client or configured
  model provider may receive selected document content through tool calls.
- **The promoted Claude Code setup is project-local and workspace-confined.**
  The exact clean-install-tested command uses `--scope local` and requires an
  explicit `STEMMA_MCP_WORKSPACE_ROOT` instead of registering a user-scoped
  server with an implicit startup-directory boundary.
- **Benchmark summaries name their provenance and version basis.** The 95%
  result is identified as a maintainer-run agent benchmark on pinned
  pre-release v0.1-line builds, the v0.2.0 compact-contract rerun remains
  separate, and the report explicitly says that no v0.5 agent rerun is claimed.
- **The AI-assistance disclosure now sits beside validation evidence.** The
  README names the three-platform tests, specification-focused conformance
  suite, protocol smoke tests, exact-artifact release qualification, and
  published benchmark corrections, then links directly to content-safe
  first-use reporting.

## [0.5.0] — 2026-08-04

### Added

- **Formatting refusals now answer with data instead of prose.** Asking for a
  tracked formatting change on a span that already carries someone else's
  pending formatting revision refuses with `format_revision_conflict`: the
  target and its tracking state, the invariant that would break, the
  conflicting revision and its author, and a list of recovery actions with
  every field the engine can determine already filled in. `expect` matching
  more than one eligible span in the block refuses with
  `ambiguous_format_target` and the occurrence count. Both report
  `mutation: none`, so a refused transaction is guaranteed to have written
  nothing.
- **Boolean formatting has an explicit off state.** `set_format` takes `marks`
  as a tri-state patch object: an omitted property is unchanged, `true` turns
  it on, `false` turns it off. Turning bold off produces one tracked formatting
  revision whose accept keeps it off and whose reject restores the exact prior
  formatting, including whether it was direct or inherited.
- **The first formatting revision inside a pending insertion is supported.**
  Formatting still-pending inserted text authors a separately attributed
  property revision, and the insertion and the formatting stay independently
  resolvable. A second formatting revision on the same span, Direct-mode
  formatting over a pending insertion, and formatting inside a pending deletion
  remain refused, each with a target-specific explanation.

### Fixed

- **A tracked note is now resolved as one operation.** Inserting a footnote or
  endnote writes two carriers, the body reference and the note story. They were
  offered as independently selectable proposals, so a partial resolution could
  accept the reference while leaving its definition pending, leaving a dangling
  reference no further resolution could clear. Both carriers now share one
  review identity, and a resolution that removes a reference reconciles the
  note definitions behind it.
- **A created header or footer is resolved as one operation**, for the same
  reason: the new story and the section reference that names it shared no
  identity, so a partial resolution could leave an orphan story or a dangling
  reference. Selective rejection of a section change now prunes orphans exactly
  as reject-all did.
- **Deleting the last paragraph that references a header or footer now prunes
  the backing story in Direct mode too**, so accepting a tracked deletion and
  performing the same deletion directly reach the same package. Unlinking a
  reference explicitly still keeps the story addressable, so unlink and relink
  round-trips.
- **A move or insertion landing at the end of a document now round-trips.** The
  document-final paragraph mark cannot carry a revision, so it is handed to the
  incoming paragraph and the preceding mark is marked inserted instead. The
  displaced paragraph's own properties were not recorded anywhere, so rejecting
  the change merged the content under the wrong paragraph style and silently
  lost formatting such as all-caps. The paragraph inheriting the final mark now
  records the displaced properties as a tracked properties change, matching
  what Word writes for the same edit: accept keeps the incoming style and
  reject restores the original.
- **Resolving a paragraph mark no longer fails on a numbered paragraph whose
  body became tracked.** An OOXML run cannot cross a revision wrapper, so a
  literal numbering label sharing its source run with newly tracked body text
  had no valid representation; the surviving label now becomes its own run
  before the tracked body.
- **Selective resolution no longer rewrites paragraphs it did not select.**
  Projection normalization ran over every paragraph, so resolving one selection
  could reshape an unrelated pending insertion elsewhere and make accepting it
  differ from making the same edit directly.
- **Restoring a numbering label recomputes its inherited geometry.** A rejected
  paragraph-formatting change that brings a label back now re-resolves its
  indent and tab origin against the document's styles, numbering, and default
  tab stop, instead of only its style table, so the in-memory result matches a
  save and reopen.
- **Rejecting a Word-authored paragraph-formatting change restores numbering
  inherited from the previous style.** The change snapshot records direct
  paragraph properties, so an absent `numPr` must fall through to the restored
  style rather than erase its list label. Full and selective rejection now
  reapply that cascade and agree with a save and reopen.
- **A role-less paragraph inserted before a literal prefix inherits the prefix
  run's formatting.** Import keeps that prefix outside the ordinary body-run
  stream, but it remains the physical run at Word's insertion point; inserted
  text now uses its formatting rather than the following body's formatting.
- **A role-less paragraph inserted before a leading hard break inherits the
  break run's formatting.** A break-only run is still the physical insertion
  point; the engine no longer skips it and accidentally copies formatting from
  the following text run.
- **Pagination-cache markers no longer create false document differences.**
  `w:lastRenderedPageBreak` remains preserved for untouched round-trips, but
  canonical comparison now removes the producer-owned cache marker and rejoins
  equal-format text fragments it split.

- **Full resolution is now move-complete.** `accept-all`/`reject-all` (engine
  `Resolution::AcceptAll`/`RejectAll`, CLI `resolve --accept-all`/
  `--reject-all`) previously settled a tracked move's content but left its
  markup — the `w:moveFrom`/`w:moveTo` containers and the `move*Range*`
  bookmarks — in the package, so a document containing a move came out of a
  bulk resolution still reporting one pending revision that no further
  command could clear. Both projections now remove the move markup as a unit
  with the content, and the resolved output re-imports with zero pending
  revisions.
- Serializing a document that still carries a pending inline move (for
  example after selectively resolving an unrelated revision) no longer
  double-wraps the move content (`w:del` nested inside `w:moveFrom`, `w:ins`
  inside `w:moveTo`). The nested form re-imported as quarantined nested
  tracking, turning a clean pending move into an unresolvable opaque block.
- `resolve`'s author-not-found hint now lists a blank-author group by its
  actual selector token (`"" (empty author)`) instead of the untypeable
  placeholder `<anonymous>`.

### Added

- **Every writing CLI verb now has a machine-readable result.** `resolve`,
  `compare`, and `validate` take `--format json`: `resolve` emits a
  `stemma.resolve_receipt.v0` (the accepted/rejected/remaining partition of
  the input's revision census plus the committed output identity), `compare`
  a `stemma.compare_receipt.v0` (input identities, discovered-revision
  census, output identity), and `validate` a `stemma.validate.v0` result
  that is structured for **invalid** packages too (issues enumerated, exit
  still `1`). Human summaries stay on stderr; stdout carries data.
- `stemma read <file>`: the full structured read model in one call —
  `stemma.read.v0` with the engine's typed block array (per-segment tracked
  status, marks, span handles, guards, table cells) plus the complete
  pending-revision census. One invocation now serves what previously
  required fusing `extract --format json` with `inspect`.
- `resolve --plan <file>`: a `stemma.resolution_plan.v0` JSON file expresses
  a mixed disposition in one call — accept/reject selectors by author and by
  id plus a `rest` disposition (default `leave`), so "accept one party,
  reject the rest" is a single invocation with a single receipt. Unknown
  plan fields, contested ids, unmatched selectors, and plans that resolve
  nothing are all refused loudly.
- `resolve --dry-run`: plan and report the full outcome (receipt included
  under `--format json`) without writing any output.
- `extract --format json` revision rows now carry the change's `w:date`
  (ISO-8601) as `date`, mirroring the read model's `RevisionRecord.date`;
  the field is omitted when the source markup carries no date.
- CLI reference: the full ten-value `kind` vocabulary on `extract`, the
  empty-author selector contract, the import package-part requirements, the
  extended-Markdown projection grammar (including which revision kinds carry
  no inline marker), and a mixed-resolution (multi-pass) recipe.
- **`stemma::api` re-exports every type a `Document` signature names.**
  `Resolution`, `ResolveSelectionAction`, `ExportOptions`, `RuntimeError`,
  `RevisionRecord`, `RevisionKind`, `EditTransaction`, `AuditReport`, and
  the validation report types are now importable from `stemma::api`, next
  to `Document`. Previously `api::Resolution` existed only as a *private*
  import, so the natural `use stemma::api::Resolution;` failed with a
  "private" error that read as selective resolution not being public, while
  the working paths (`stemma::runtime::Resolution`,
  `stemma::tracked_model::ResolveSelectionAction`) were named on no doc
  page.
- Embedding docs: a compile-checked resolve-a-redline walkthrough
  (enumerate via `Document::revisions()`, chain a mixed accept/reject
  triage through `Resolution::Selective { ids: HashSet<u32>, .. }`,
  serialize the resolved `.docx`). The concepts page no longer describes
  projections as read-only text views: `project`/`read_accepted`/
  `read_rejected` return full serializable `Document`s, and
  `Document::serialize` is documented as running the same blocking
  validation gate as `save_docx` (default `ExportOptions`). The guide
  pages' Rust snippets (concepts, revisions) are now doctest-compiled
  against the real facade, closing the drift that let prose name a
  `list_revisions` method the crate spells `revisions()`.

### Changed

- `inspect --format json` is now `stemma.inspect.v1`: the summary integers
  are named `block_count`/`pending_revision_count`. v0 reused the read
  model's `blocks` key for a **count**, inviting consumers to conflate the
  two shapes. The extended-Markdown projection and its `@stemma inspect.v0`
  header line are unchanged.
- The CLI reference's revision-id guidance now states the actual durability
  contract: ids are content-derived engine identities that survive
  serialize/reopen and selective resolution of other changes (the previous
  wording claimed ids never survive a `resolve`, which was wrong).

## [0.4.0] — 2026-07-21

### Added

- MCP task declarations bind every declared target and read-only input by
  SHA-256 before the first task mutation. Schema v1 admits exact-count tracked
  replacement effects, validates each `effect_id` and replacement field before
  mutation, and refuses task-bound operations whose outcome cannot be decided
  later by revision identity.
- The last target save now emits a create-once
  `stemma.task_manifest.v1`. Complete manifests bind every input, target
  baseline, committed output, passing save audit, full effect declaration, and
  minted revision identities. Missing effects or a later write failure produce
  an explicit partial manifest and a failing final call; abandoned tasks write
  no manifest.
- `stemma verify-task <manifest.json> [--root <dir>]` verifies a delivery from
  its files alone. Exit `0` is verified complete, `1` verified partial, `2` an
  artifact/evidence mismatch, and `3` usage, I/O, malformed JSON, or unknown
  schema.

### Changed

- A task-bound `save_docx` no longer treats one valid document as task success.
  Earlier saves report `task_pending` and `deliverable:false`; only the final
  all-effects join can return a deliverable task result.
- Task-manifest claims are intentionally limited: the format is unsigned and
  verifies consistency with declared files and effects, not producer
  authenticity, declaration timing, or effects omitted from the caller's
  declaration.

- Successful CLI `apply`/`execute` now reruns the delivery audit over the exact
  serialized candidate before committing its receipt or DOCX. Complete
  receipts bind `verification.artifact_stage: "serialized_output"` and
  `verification.output_sha256` to the output artifact digest; standalone
  `verify` remains available as an optional producer-neutral recheck.
- MCP `save_docx` now runs a fresh session audit and refuses a non-deliverable
  result before creating the destination path, then applies the serialized
  package gate and create-new commit. The common agent path is therefore
  `open_docx -> inspect_docx -> execute_plan -> save_docx`; `verify_docx`
  remains available for detailed evidence inspection or producer-neutral
  before/after audit. Successful typed accept/reject commands are retained as
  session evidence, so their selected pre-existing revisions and exact ordered
  committed-content effects are reported as expected rather than blocked.
  Missing evidence, an engine-bypass resolution, or a direct mutation before,
  between, or after those effects still fails closed. Producer-neutral audits
  remain conservative because they have no session command evidence.
- The untouched proof now expands a grouped tracked-move census identity to
  every paired source and destination carrier. Verified delivery therefore
  accepts correctly tracked moves while continuing to block any mutation not
  covered by the exact move identity.
- Core `inspect_docx` find now accepts an additive `patterns` array for up to
  eight known phrases. Outcomes preserve input order and duplicates, report
  zero matches explicitly, and reuse the singular result contract with exact
  totals and continuation. Batch pages and their encoded response are capped
  explicitly; singular find remains unchanged.
- Safe-artifact release qualification now selects the explicit `advanced` MCP
  profile required by its persistence-producer cases, verifies the required
  tool set before running any case, and freezes that profile in the candidate
  manifest. This keeps the harness independent of the product-default compact
  surface while failing before mutation if the expected escape-hatch tools are
  unavailable.

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

[Unreleased]: https://github.com/stemma-sh/stemma/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/stemma-sh/stemma/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/stemma-sh/stemma/compare/v0.2.0...v0.4.0
[0.2.0]: https://github.com/stemma-sh/stemma/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/stemma-sh/stemma/releases/tag/v0.1.0
