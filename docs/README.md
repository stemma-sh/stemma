# Stemma documentation

Stemma creates reviewable Word redlines from existing `.docx` files. It can
compare two versions, apply an approved list of replacements, or give an agent
a bounded document-editing workflow.

New here? Start with [Create your first redline](getting-started.md). It takes
two Word documents and produces one native tracked-changes comparison.

## Start by goal

| I want to | Start here |
|---|---|
| Compare two versions of a document | [Create your first redline](getting-started.md) |
| Apply exact, approved replacements | [Apply approved changes](guides/apply-approved-changes.md) |
| Connect Stemma to an agent | [Use Stemma with an agent](guides/use-with-an-agent.md) |
| Verify a multi-document delivery | [Verify a task delivery](guides/verify-task-delivery.md) |
| Review, accept, or reject revisions | [Review and resolve changes](guides/review-and-resolve.md) |
| Fix an error or refusal | [Troubleshooting](help/troubleshooting.md) |
| Read working code for a common flow | [Examples](examples.md) |
| Embed the Rust engine | [`stemma-engine` README](https://github.com/stemma-sh/stemma/blob/main/stemma-engine/README.md) |
| Build a viewer, renderer, or service | [Embed the engine](reference/embedding.md); render from the [read model reference](reference/read-model.md) |
| Store documents and edits durably | [Persist and replay](guide/persistence.md) |

## Understand the model

The guide explains the ideas that make Stemma safe:

1. [Concepts](guide/concepts.md): typed documents, projections, and explicit
   outcomes.
2. [Revisions](guide/revisions.md): Word revision types, authorship, and
   accept/reject behavior.
3. [Editing](guide/editing.md): transactions, receipts, and review before save.
4. [Fidelity](guide/fidelity.md): what Stemma preserves and what it does not
   promise.
5. [Stability](guide/stability.md): compatibility guarantees for each public
   surface.
6. [Persist and replay](guide/persistence.md): the storage model, and why it
   fits an agent-editing product.

## Look up an exact contract

- [CLI reference](reference/cli.md): commands, exit codes, worklists, receipts,
  and examples.
- [MCP core reference](reference/mcp.md): the default five-tool agent surface.
- [MCP advanced reference](reference/mcp-advanced.md): optional expert tools,
  v4 transactions, and advanced recipes.
- [v4 operation reference](reference/operations.md): every transaction
  operation, its accepted fields, and canonical shapes, generated from the
  engine's parser table.
- [Read model reference](reference/read-model.md): the typed views a renderer
  consumes (blocks, segments, run formatting, revision identity), generated
  from live engine values and labeled version-bound.
- [Embed the engine](reference/embedding.md): the facade lifecycle, hosting
  sessions, the concurrency model, and the map to rustdoc.
- [HTTP API](reference/http.md): local demonstration transport, not a stable
  hosted product surface.

## Evidence and internals

- [Agent benchmarks](benchmarks.md): current results, methodology, caveats, and
  corrections.
- [Benchmark archive](benchmarks-history.md): retired pins and earlier waves.
- [Architecture](internals/architecture.md): workspace and engine map.
- [Testing](internals/testing.md): validation tiers and contributor test commands.
- [Design notes](internals/design-notes.md): decisions behind shipped designs.
- [Change log](https://github.com/stemma-sh/stemma/blob/main/CHANGELOG.md):
  the release history, and where every announced breaking change lands.

For source setup and contribution expectations, see
[CONTRIBUTING.md](https://github.com/stemma-sh/stemma/blob/main/CONTRIBUTING.md).
