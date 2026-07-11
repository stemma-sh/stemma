# stemma

A typed-IR DOCX compiler with first-class tracked-change semantics — and an
MCP server that puts it in front of agents.

```
DOCX bytes -> import -> CanonDoc -> edit / diff -> apply -> serialize -> DOCX bytes
```

Programmatic DOCX editing usually forces a bad trade. Converters (pandoc,
markdown pipelines) flatten the file and cannot write tracked changes back.
XML-splicing libraries (python-docx and kin) edit in place but have no
tracked-change model — insertions, deletions, moves, and their accept/reject
semantics are yours to hand-roll against raw XML. stemma treats the problem
as compilation instead: parse the whole document into a typed intermediate
representation (the IR — one canonical tree, `CanonDoc`), apply edits as
typed transformations with tracked-change semantics built in, and serialize
back to a file Word opens clean.

## Highlights

- **Tracked changes as a type system, not markup.** Insertions, deletions,
  moves, formatting changes, comments, and their accept/reject semantics are
  parsed into a canonical typed IR and serialized back to files that open
  clean in Word — including documents already carrying someone else's
  redline (visible tracked-changes markup).
- **Agent-ready.** The [MCP server](stemma-mcp/) exposes read/edit/review
  verbs over stdio: every write returns a receipt naming exactly what
  changed, every refusal names an escape hatch, and originals are never
  touched.
- **Verified before bytes leave.** A post-serialization OOXML linker — it
  reference-checks the emitted package the way a code linker checks symbols —
  gates every output; ~1,060 ECMA-376/ISO-29500 conformance tests; a
  real-Word oracle tier behind that.
- **Benchmarked, with receipts.** Across three model sweeps: **95%** task
  success vs 76–85% for the same agent on Claude's stock DOCX skill on
  frontier model tiers, at 2–3× lower latency and roughly half the token
  traffic — tier-by-tier results, losses, and corrections disclosed inline.
  The two arms fail differently in kind: stemma's observed failures are loud
  refusals with the document untouched; the hand-editing failures were
  silent content loss (a dropped table row's text, emptied footnotes).
  [The report](docs/benchmarks.md) ·
  [per-cell data](docs/benchmark-data-model-sweeps-2026-07.json).
- **A cost structure, not just a pass rate.** The document stays server-side,
  so an agent's token traffic is flat in document size: the same 43-change
  task moved no more tokens in a ~150-page agreement than in a ~50-page one,
  while the raw-XML arm grew 33% and finished one turn from its ceiling. And
  the guardrails keep a budget model functional — 76% vs 20% on Haiku 4.5,
  at ~$0.10–0.25 per document on the resolution family — so high-volume
  pipelines can execute decomposed edits on a cheap tier instead of a
  frontier model hand-editing XML.
- **An honest fidelity contract.** Render fidelity and content completeness
  are guaranteed and gated; byte identity is explicitly out of contract —
  [read this](docs/guide/fidelity.md) before pointing a checksum at stemma output.

**Where it stands:** the engine is the hardened part — spec-tested,
corpus-hardened, and the component every guarantee above is about. The MCP
server and HTTP API are deliberately thin transports, proof-of-concept grade:
complete enough to drive real reviews end to end, and explicit about their
limits ([MCP status](stemma-mcp/README.md) ·
[HTTP scope](docs/reference/http.md)). And all of it is **pre-1.0**: under
`0.x` semver a minor release may break API contracts — deliberately, with
changelog notice, and along documented tier boundaries
([the stability policy](docs/guide/stability.md) says exactly what you can
depend on today).

## Quickstart

```bash
mise install                          # rust toolchain + just (or: rustc >= 1.91)
cargo run -p stemma --example my_first_edit   # fastest first run: parse → edit → validated bytes
just gate                             # lint + full test suite (optional; ~8 min)
```

The test suite is hermetic — no network, no external services: clone,
`just gate`, green. It is thorough, so it takes **~8 minutes** and has some
long stretches with no output — that is the suite running, not a hang. You do
**not** need to run it before trying the examples below; the quickest way to
see the engine work end to end is
`cargo run -p stemma --example my_first_edit`. Then pick your surface —
building an agent? MCP. An app or service? HTTP. Shell scripts or CI? The
CLI. Rust? Embed the engine crate directly:

**Agents (MCP)** — point your MCP client at the server. Released versions run
straight from npm, no Rust toolchain; from this checkout, build it
([wiring for both](stemma-mcp/README.md)):

```bash
npx -y @stemma-sh/mcp                 # released versions (prebuilt binaries)
cargo build -p stemma-mcp --release   # this checkout; binary at target/release/stemma-mcp
```

```jsonc
// agent calls:
replace_text { "doc_id": "doc_1", "old": "twelve (12) months",
               "new": "twenty-four (24) months", "author": "J. Osei" }
// stemma answers with a receipt, not a shrug:
{ "applied": true, "changed_block_ids": ["p_41"], "revision_ids": [17],
  "matches": [{ "block_id": "p_41", "excerpt": "…liability shall not exceed
  «twelve (12) months» of fees…" }] }
```

Full tool surface, refusal vocabulary, and recipes:
[the MCP reference](docs/reference/mcp.md) — self-contained, written to be
pasted into an agent's context.

**Apps & services (HTTP)** — the core verbs over JSON, and the fastest way
to *see* stemma work:

```bash
cargo run -p stemma-api               # open http://127.0.0.1:3000
```

`stemma-api` is a demo-grade, single-process server: in-memory documents, no
auth, no TLS, no persistence, loopback-only. It exists to exercise the engine
over HTTP and to serve the editor, not as a production deployment. See its
[scope and endpoints](docs/reference/http.md).

That one command serves a browser Word-style review editor on the API —
upload a `.docx`, edit in Suggesting/Editing mode, accept/reject tracked
changes, export redline/accepted/rejected copies. Note the editor's first
load fetches ProseMirror (from esm.sh) and MathJax (from jsdelivr) over the
public internet, so this surface — unlike the hermetic test suite — needs a
network connection the first time you open it.

**Command line** — no integration at all; redlines and extraction from any
shell, script, or CI job:

```bash
cargo install --path stemma-cli       # installs `stemma` into ~/.cargo/bin
stemma compare base.docx target.docx --author "J. Osei" -o redline.docx
stemma extract redline.docx --format json   # blocks + pending tracked changes
stemma resolve redline.docx --accept-author "J. Osei" -o resolved.docx
stemma validate resolved.docx
```

Any two versions of a real Word file work as `base`/`target`. For a
no-files-needed demo of the same flow,
`cargo run -p stemma --example redline_from_two_files` runs on bundled
fixtures. [The CLI reference](docs/reference/cli.md).

**Rust (embed the engine)** — `stemma-engine` directly; every verb on one
facade:

```rust
let doc = Document::parse(&docx).expect("parse DOCX bytes");
// Author one tracked edit as a typed, guard-pinned transaction (see the example).
let txn = parse_transaction(&txn_json)
    .expect("transaction JSON is schema-valid")
    .into_edit_transaction()
    .expect("v4 transaction translates to an EditTransaction");
let edited = doc.apply(&txn).expect("apply the tracked edit");
let out = edited
    .serialize(&ExportOptions::default())
    .expect("serialize to validated DOCX");
```

Runnable end-to-end (parse → tracked edit → receipt → validated bytes):
`cargo run -p stemma --example my_first_edit`. More:
[`walk_the_document`](stemma-engine/examples/walk_the_document.rs),
[`resolve_a_redline`](stemma-engine/examples/resolve_a_redline.rs),
[`redline_from_two_files`](stemma-engine/examples/redline_from_two_files.rs),
[`review_before_save`](stemma-engine/examples/review_before_save.rs).

## When stemma is the wrong tool

- **Generating documents from templates, no tracked changes involved** —
  python-docx or plain OOXML templating is simpler.
- **One-shot format conversion** (DOCX → Markdown/HTML and done) — use
  pandoc; stemma's projections exist to serve editing loops, not conversion
  pipelines.
- **Byte-identical round-trips** — out of contract by design; render
  fidelity and content completeness are the guarantees
  ([the fidelity contract](docs/guide/fidelity.md)).

stemma earns its keep where documents stop being flat text: redlines on top
of redlines, comments, footnotes, tables, headers — anything that must
survive a round-trip through Word's review machinery.

## Workspace

| Component | |
|---|---|
| [`stemma-engine/`](stemma-engine/) | Crate — the compiler: import, typed IR, diff/merge, edit transactions, serialization, validation. |
| [`stemma-mcp/`](stemma-mcp/) | Crate — the engine's verbs over MCP/stdio, for agents. |
| [`stemma-api/`](stemma-api/) | Crate — the same verbs over HTTP/JSON. |
| [`stemma-cli/`](stemma-cli/) | Crate — the `stemma` command-line tool: compare, extract, resolve, validate. |
| [`stemma-examples/`](stemma-examples/) | Static browser assets (no Cargo crate) — a review editor (Suggesting/Editing) served by `stemma-api`. |

## Documentation

The [docs](docs/README.md) carry the full story:
[guide](docs/guide/concepts.md) (concepts → revisions → editing → fidelity) ·
[MCP reference](docs/reference/mcp.md) (verbs, refusals, recipes —
self-contained; paste it into your agent) ·
[benchmarks](docs/benchmarks.md).

## How this was built

Most of this code was written by AI (Claude). The human was used for domain,
direction, taste, and ideas.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for setup and expectations, and
[SECURITY.md](SECURITY.md) for how to report vulnerabilities. AI-assisted
contributions are welcome and held to the same bar as everything else:
`just gate` green, tests justified from the domain, honest PR descriptions.

## License

Dual-licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT)
at your option.
