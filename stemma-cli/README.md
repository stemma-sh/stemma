# stemma-cli

A thin command-line interface to the `stemma` DOCX engine. Its primary job is
to provide a compact `inspect -> execute -> verify` front end over the typed
DOCX engine. `execute` applies an explicit approved worklist and creates a
native Word redline with complete item outcomes. It also exposes compare,
extract, resolve, and validate as maintenance verbs.

The binary is named `stemma`. General-purpose verbs drive the engine's stable
`stemma::api::Document` facade. The experimental worklist command uses the
engine's tracked-native replacement planner while field evidence determines
whether that orchestration belongs behind a shared application facade.

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

From the repository root, run the focused workflow against bundled inputs:

```sh
demo_dir="$(mktemp -d)"
cargo run -p stemma-cli -- apply \
  stemma-engine/testdata/simple-text/before.docx \
  --worklist stemma-cli/examples/approved-worklist.json \
  -o "$demo_dir/redline.docx" \
  --receipt "$demo_dir/receipt.json"
```

## Filesystem contract

CLI paths are explicit authority supplied by the human caller; they are not
confined by `STEMMA_MCP_WORKSPACE_ROOT`. Output commits are nevertheless
create-new only. `apply`, `compare`, and `resolve` refuse any existing
destination and any alias of an input, with no overwrite option. `apply`
protects its DOCX and worklist sources and its durable receipt destination.

The CLI validates output, stages it in the destination directory, commits
without clobbering, then verifies the committed byte length and SHA-256. Its
existing human-readable success line is retained and followed by
`bytes=<n> sha256=<hex> collision_policy=create_new disposition=created`.
This guards ordinary caller mistakes and failed writes; it is not a durability
promise across power loss or protection from a hostile same-user local process.

## Commands

```
stemma apply    <input.docx> --worklist <changes.json> -o <redline.docx>
                [--receipt <receipt.json>] [--emit-partial]
stemma inspect  <input.docx> [--format markdown|json]
stemma execute  <input.docx> --plan <changes.json> -o <redline.docx>
stemma verify   <before.docx> <after.docx> [--policy tracked-delivery-v0]
stemma compare  <base.docx> <target.docx> -o <redline.docx> [--author NAME]
stemma extract  <file.docx> [--format text|json]
stemma resolve  <file.docx> -o <out.docx> (--accept-all | --reject-all
                | --accept-author NAME | --reject-author NAME
                | --accept-ids a,b | --reject-ids a,b)
stemma validate <file.docx>
```

- `apply` consumes an exact-input-bound experimental `stemma.worklist.v0` and
  commits its authoritative `stemma.apply_receipt.v0` sidecar before any
  Word-native redline. The sidecar defaults to `<out>.receipt.json`; stdout is
  a best-effort mirror. Exit `0` means every item applied. If any item refuses,
  exit `3` writes a non-deliverable partial receipt and no DOCX by default.
  `--emit-partial` explicitly requests a diagnostic partial redline; it remains
  non-deliverable and still exits `3`. Receipt-only delivery is invalid: the
  actual exit, output presence, and output hash/size must match the receipt's
  `persistence_confirmation` requirements.
  The producer section identifies the exact running executable by SHA-256 and
  byte size; optional compile-time `STEMMA_BUILD_STAMP` is only a readable
  build label.
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
with a one-line actionable `error:` message. For `apply`, the durable sidecar
rather than stdout is authoritative. Full reference with real invocations and
output: **[docs/reference/cli.md](../docs/reference/cli.md)**.

Apply receipts contain paths, hashes, match excerpts, and diagnoses. Treat them
as document-sensitive metadata rather than publishing them by default.
