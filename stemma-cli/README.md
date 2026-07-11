# stemma-cli

A thin command-line interface to the `stemma` DOCX engine. It gives adopters
who aren't writing Rust — Node, Python, shell — a zero-integration path to the
engine's core verbs: compare two files into a tracked-changes redline, read a
document as text or JSON, resolve tracked changes, and validate a package.

The binary is named `stemma`. It drives only the engine's stable
`stemma::api::Document` facade, so it stays a genuinely thin shell over the same
verbs the library and MCP server expose.

> **Pre-1.0.** A `0.x` minor release may break API and wire contracts —
> deliberately, with changelog notice. The
> [stability policy](https://github.com/stemma-sh/stemma/blob/main/docs/guide/stability.md)
> states exactly what you can depend on today.

## Build / run

```sh
cargo run -p stemma-cli -- --help
# or build the binary:
cargo build -p stemma-cli --release   # target/release/stemma
```

## Commands

```
stemma compare  <base.docx> <target.docx> -o <redline.docx> [--author NAME]
stemma extract  <file.docx> [--format text|json]
stemma resolve  <file.docx> -o <out.docx> (--accept-all | --reject-all
                | --accept-author NAME | --reject-author NAME
                | --accept-ids a,b | --reject-ids a,b)
stemma validate <file.docx>
```

- `compare` produces a redline whose reject-all reading is the base and whose
  accept-all reading is the target. `--author NAME` attributes every discovered
  revision to `NAME`; omit it for an anonymous redline (an empty `--author ""`
  is refused, not silently anonymized).
- `extract --format json` gives blocks plus a `revisions` array (pending tracked
  changes with `revision_id` / `kind` / `author` / `block_id` / `excerpt`).
- `resolve` requires exactly one disposition; a selection that matches nothing
  (unknown id, author with no changes) fails loudly instead of writing an
  unchanged file.
- `validate` exits `0` with an `OK` line, nonzero with the structured reason.

stdout carries data, stderr carries diagnostics; every failure exits nonzero
with a one-line actionable `error:` message. Full reference with real
invocations and output: **[docs/reference/cli.md](../docs/reference/cli.md)**.
