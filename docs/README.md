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
| Embed the Rust engine | [`stemma-engine` README](../stemma-engine/README.md) |

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

## Look up an exact contract

- [CLI reference](reference/cli.md): commands, exit codes, worklists, receipts,
  and examples.
- [MCP core reference](reference/mcp.md): the default five-tool agent surface.
- [MCP advanced reference](reference/mcp-advanced.md): optional expert tools,
  v4 transactions, and advanced recipes.
- [HTTP API](reference/http.md): local demonstration transport, not a stable
  hosted product surface.

## Evidence and internals

- [Agent benchmarks](benchmarks.md): current results, methodology, caveats, and
  corrections.
- [Benchmark archive](benchmarks-history.md): retired pins and earlier waves.
- [Architecture](internals/architecture.md): workspace and engine map.
- [Testing](internals/testing.md): validation tiers and contributor test commands.
- [Design notes](internals/design-notes.md): decisions behind shipped designs.
- [RFC 0001](rfcs/0001-audit-and-session-review.md) and
  [RFC 0002](rfcs/0002-opaque-descent.md): detailed design records.

For source setup and contribution expectations, see
[CONTRIBUTING.md](../CONTRIBUTING.md).
