# stemma

**Safe tracked changes for Word automation.**

Give Stemma an existing `.docx` and a short approved worklist. It creates a new,
Word-native redline while preserving the original document and any revisions it
already contains. Every requested item is reported as applied or explicitly
refused; ambiguity is never silently guessed.

```text
existing DOCX + approved old-to-new changes
    -> tracked-native execution
    -> scope and preservation audit
    -> new redline DOCX + machine-readable receipt
```

The v0.2.0 product focus is this workflow. The typed Rust engine remains the
correctness kernel, the CLI is the canonical process contract, and MCP is the
agent adapter. The HTTP/editor demo and broad low-level verb surface remain
available, but are not where current product development is focused.

## Compact agent workflow

The default product shape is three semantic stages:

```text
inspect -> execute -> verify
```

`inspect` emits compact, revision-aware extended Markdown rather than exposing
the typed IR. `execute` compiles an exact-input-bound plan through the typed
engine. `verify` independently certifies any before/after pair and returns
accepted/rejected projection identities. Markdown is the agent language, never
the source of truth.

## Try the workflow

From a clone:

```bash
demo_dir="$(mktemp -d)"
cargo run -p stemma-cli -- inspect \
  stemma-engine/testdata/simple-text/before.docx
cargo run -p stemma-cli -- execute \
  stemma-engine/testdata/simple-text/before.docx \
  --plan stemma-cli/examples/approved-worklist.json \
  -o "$demo_dir/redline.docx" \
  --receipt "$demo_dir/receipt.json"
cargo run -p stemma-cli -- verify \
  stemma-engine/testdata/simple-text/before.docx \
  "$demo_dir/redline.docx"
```

The example worklist is deliberately concrete:

```json
{
  "schema": "stemma.worklist.v0",
  "input": {
    "sha256": "2cdfb8ecd1a27ef7132ebbaa1f718d6705ea6532bf3b155c09bfd7e87d410667",
    "bytes": 11431
  },
  "author": "Approved Reviewer",
  "changes": [
    {
      "id": "change-1",
      "old": "foo bar",
      "new": "review-ready language",
      "expected_matches": 1
    }
  ]
}
```

`redline.docx` opens in Word with a native tracked replacement. Rejecting the
new revisions restores the input text; accepting them shows the approved new
text. `receipt.json` contains exact input, worklist, and output SHA-256
identities, complete per-item outcomes, new revision counts, preservation of
pre-existing revisions, untouched-scope findings, and package validation.

The worklist's required input hash and byte count bind approval to one exact
document. `stemma validate INPUT` prints both values. An all-applied worklist
creates the redline and exits `0`. If any item is refused, Stemma persists the
receipt, creates no DOCX, and exits `3`. `--emit-partial` is an explicit escape
hatch that creates a non-deliverable partial redline and still exits `3`.

The durable receipt defaults to `<out>.receipt.json`; `--receipt FILE` overrides
it. Receipt and DOCX paths are create-new. The receipt is committed first, so a
failed DOCX commit can leave a diagnostic receipt without a DOCX, but never a
DOCX without its receipt. Delivery therefore requires the documented process
exit plus output presence and receipt hash/size agreement; the receipt makes
those checks explicit. JSON is also mirrored to stdout for pipelines; a closed
stdout does not invalidate the durable result.

Receipts contain paths, hashes, match excerpts, and diagnoses. Treat them as
document-sensitive metadata rather than publishing them by default.

The experimental `stemma.worklist.v0` scope is intentionally narrow: guarded
old-to-new changes in top-level body paragraphs. Exact match count defaults to
one; optional block/range scope and `normalize_ws` matching handle deliberate
disambiguation. Matches crossing tracked/opaque barriers are refused. Matches
in top-level table cells are detected and refused for default-scope items.
Nested table cells, headers, footers, notes, comments, and textboxes are
named as unsearched receipt limitations.

Full command and receipt contract: [CLI reference](docs/reference/cli.md).
For the historical pre-release Candidate A identity and its synthetic
walkthrough, see [Evaluate the approved-worklist workflow](docs/evaluation.md).

## Agent adapter

The released MCP server runs from npm without a Rust toolchain. Its default
`core` profile exposes 5 tools over the complete typed edit transaction; set
`STEMMA_MCP_PROFILE=advanced` to opt into the full 31-tool expert surface:

```bash
npx -y @stemma-sh/mcp
```

The compact agent path is:

```text
open_docx -> inspect_docx -> execute_plan -> verify_docx -> save_docx
```

`inspect_docx` defaults to the first page of a compact current index and selects bounded find or
window reads, a paged document projection with exact prose and bounded table
summaries, block detail, revisions, styles, editable notes, historical
accepted/rejected/redline projections, or the complete parser-derived edit
operation catalog.
Table block detail is a bounded page of cell locators; every cell paragraph id
remains available for exact follow-up inspection.
`execute_plan` previews or applies the existing atomic v4 transaction, executes
an explicitly non-atomic server-side literal-replacement worklist with per-item
outcomes, handles an accept/reject selection, or produces a tracked redline
from a two-file comparison through the typed engine.
Worklist preview uses a throwaway snapshot, and the default whole-body scope
includes table-cell paragraphs with the same formatting-preserving tracked
splice as top-level body text.
Receipts omit whole-table content. `verify_docx` audits either the open session
or any producer's before/after pair; each audit section is explicitly paged
with totals and continuation metadata. Treat any
plan error, direct change, unexpected prior-revision change, untouched-scope
violation, or validator issue as incomplete. Save only to a new path. The MCP
workspace boundary confines agent-controlled reads and writes to
`STEMMA_MCP_WORKSPACE_ROOT`. See the
[MCP golden path](stemma-mcp/README.md) and
[filesystem contract](docs/reference/mcp.md#filesystem-and-artifact-boundary).

## Why Stemma

- **Layered Word revisions.** New tracked changes are authored beside existing
  reviewers' revisions rather than flattening the document through Markdown.
- **Bounded execution.** Match counts, optional scopes, author separation, and
  tracked/opaque barriers turn ambiguity into a named refusal.
- **Evidence before delivery.** Blocking OOXML validation, preservation audit,
  exact artifact identity, and create-new output semantics run before success.
- **A typed safety kernel.** DOCX bytes are parsed into one canonical typed IR;
  tracked insert/delete/format semantics and accept/reject projections are not
  hand-written XML splices at the adapter edge.

The engine and historical agent evaluations remain documented in the
[benchmark report](docs/benchmarks.md) and
[per-cell data](docs/benchmark-data-model-sweeps-2026-07.json). The CLI worklist
remains the narrow approved-replacement contract; the MCP core exposes the same
typed transaction and assurance kernels as the advanced profile through a
smaller semantic surface.

The project is **pre-1.0**. A `0.x` minor release may change the experimental
worklist or receipt contract with changelog notice. See the
[stability policy](docs/guide/stability.md).

## When stemma is the wrong tool

- **Generating documents from templates, no tracked changes involved**:
  python-docx or plain OOXML templating is simpler.
- **One-shot format conversion** (DOCX to Markdown/HTML and done): use pandoc;
  Stemma's projections exist to serve editing loops, not conversion pipelines.
- **Byte-identical round-trips**: out of contract by design; render fidelity and
  content completeness are the guarantees in the
  [fidelity contract](docs/guide/fidelity.md).
- **Broad interactive Word editing**: Stemma deliberately uses Word as the
  review surface rather than building another general-purpose editor.

## Workspace

| Component | Role |
|---|---|
| [`stemma-engine/`](stemma-engine/) | Correctness kernel: import, typed IR, tracked edit semantics, serialization, and validation. |
| [`stemma-artifacts/`](stemma-artifacts/) | Shared host boundary: path authority, exact identity, protected sources, and create-new commits. |
| [`stemma-cli/`](stemma-cli/) | Canonical process contract: approved worklist apply, compare, extract, resolve, and validate. |
| [`stemma-mcp/`](stemma-mcp/) | Agent adapter over stdio. |
| [`stemma-api/`](stemma-api/) | Maintenance-only HTTP demonstration surface; not a production service. |
| [`stemma-examples/`](stemma-examples/) | Maintenance-only browser demonstration assets served by `stemma-api`. |

## Documentation

The [docs](docs/README.md) include the
[CLI contract](docs/reference/cli.md),
[MCP reference](docs/reference/mcp.md),
[engine guide](docs/guide/concepts.md), and
[benchmark history](docs/benchmarks.md).

## How this was built

Most of this code was written by AI (Claude). The human was used for domain,
direction, taste, and ideas.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for setup and expectations, and
[SECURITY.md](SECURITY.md) for vulnerability reports. AI-assisted contributions
are welcome and held to the same bar: `just gate` green, tests justified from
the domain, and honest PR descriptions.

## License

Dual-licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT)
at your option.
