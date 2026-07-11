# stemma docs

A typed-IR DOCX compiler with first-class tracked-change semantics — and an
MCP server that puts it in front of agents.

**Pick your surface first:**

| You are… | Use | Start here |
|---|---|---|
| building an agent | the MCP server | [MCP reference](reference/mcp.md) — verbs, refusals, recipes |
| building an app/service | the HTTP API | [HTTP API](reference/http.md) — one command also serves a browser Word-style review editor |
| embedding in Rust | `stemma-engine` | the crate docs + [architecture](internals/architecture.md); runnable examples: `cargo run -p stemma --example my_first_edit` |
| scripting from a shell or CI | the `stemma` CLI | [CLI reference](reference/cli.md) — compare, extract, resolve, validate |

The same engine and the same semantics behind every surface — a tracked change
made over MCP is the tracked change the editor renders and the Rust API
resolves. Each section below answers one question:

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
