# stemma docs

Safe tracked changes for Word automation. In v0.2.0, apply an approved
old-to-new worklist to an existing DOCX and receive a native redline plus
explicit item outcomes. The typed engine is the correctness kernel; CLI and MCP
are adapters.

**Pick your surface first:**

| You are… | Use | Start here |
|---|---|---|
| applying an approved worklist | the `stemma` CLI | [CLI reference](reference/cli.md) — apply contract, receipt, and refusal semantics |
| building an agent | the MCP server | [MCP reference](reference/mcp.md) — focused worklist path and advanced verbs |
| embedding in Rust | `stemma-engine` | the crate docs + [architecture](internals/architecture.md); runnable examples: `cargo run -p stemma --example my_first_edit` |
| maintaining the local HTTP/editor demo | the HTTP API | [HTTP API](reference/http.md) — non-production demonstration surface |

The same engine owns tracked-change semantics behind every surface. The
experimental aggregate worklist and receipt contract currently belongs to the
CLI; MCP exposes the same kernel through a narrower recommended tool sequence,
not a second receipt standard. Each section below answers one question:

- **[Guide](guide/concepts.md)** — *how do I think about this?* Four short
  chapters for humans: [concepts](guide/concepts.md) →
  [revisions](guide/revisions.md) → [editing](guide/editing.md) →
  [fidelity](guide/fidelity.md). Read in order; each builds the vocabulary
  the next uses.
- **[Reference](reference/mcp.md)** — *what exactly can I call, and how do I
  do the thing I came here for?* The [MCP tool surface](reference/mcp.md) —
  every verb, the refusal vocabulary, and end-to-end recipes for the
  canonical tasks (self-contained: paste it into your agent's context) —
  plus the [HTTP API](reference/http.md), the [CLI](reference/cli.md), and
  docs.rs for the Rust API.
- **[Benchmarks](benchmarks.md)** — *why should I believe any of this?*
  Three model sweeps, deterministic gates, losses disclosed, every number
  backed by [per-cell data](benchmark-data-model-sweeps-2026-07.json).
- **[Internals](internals/architecture.md)** — *how does it work / how do I
  contribute?* [Architecture](internals/architecture.md),
  [testing](internals/testing.md),
  [design notes](internals/design-notes.md).

Source and quickstart: [the repository](https://github.com/stemma-sh/stemma).
