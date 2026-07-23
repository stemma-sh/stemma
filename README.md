# Stemma

<p align="center">
  <a href="https://github.com/stemma-sh/stemma/actions/workflows/ci.yml?query=branch%3Amain"><img src="https://img.shields.io/github/actions/workflow/status/stemma-sh/stemma/ci.yml?branch=main&style=flat-square" alt="CI status"></a>
  <a href="https://crates.io/crates/stemma-cli"><img src="https://img.shields.io/crates/v/stemma-cli?style=flat-square" alt="crates.io version"></a>
  <a href="https://www.npmjs.com/package/@stemma-sh/mcp"><img src="https://img.shields.io/npm/v/@stemma-sh/mcp?style=flat-square" alt="npm version"></a>
  <a href="LICENSE-MIT"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue?style=flat-square" alt="MIT or Apache-2.0 license"></a>
</p>

**Safe tracked changes for Word automation.**

Stemma edits existing Word documents using native tracked changes. Give it a
`.docx` and the changes you want; it creates a new redline that reviewers
accept or reject in Microsoft Word. The original is preserved, and ambiguous
changes are refused instead of guessed.

<!-- Screenshot: a Stemma-produced redline open in Word (tracked changes
     visible, author attributed). Place the image here before promotion. -->

Use it to:

- turn a returned draft into one clean, attributed redline (contracts,
  policies, any negotiated document);
- let an AI assistant propose edits that your reviewers accept or reject in
  Word, not in a chat window;
- fill an existing `.docx` template into a finished document: text, tables,
  and content controls, edited in place, with tracked changes or silently
  (direct mode);
- apply the same approved wording change across many documents, with a
  per-file receipt.

Everything runs on your machine, as a command-line tool or a local MCP
server: documents are read and written locally and never sent to a service.

In [published agent benchmarks](https://stemma.sh/docs/benchmarks), Stemma reaches 95% task
success, compared with 82% for the same model editing the raw document XML
directly.

[Documentation](https://stemma.sh/docs) ·
[CLI reference](https://stemma.sh/docs/reference/cli) ·
[MCP setup](stemma-mcp/README.md) ·
[Benchmarks](https://stemma.sh/docs/benchmarks) ·
[Changelog](CHANGELOG.md)

## Quick start: with an AI assistant

The Stemma MCP server ships prebuilt binaries for Linux, macOS, and Windows;
`npx` fetches the right one, so there is nothing to build. Add it to Claude
Code with:

```bash
claude mcp add stemma --scope user -- npx -y @stemma-sh/mcp
```

Then ask for the edit in plain language, for example: *"Open nda.docx and
extend the confidentiality term from 2 to 3 years as a tracked change, then
save it as nda-redline.docx."* The agent opens, inspects, edits, verifies,
and saves; the result opens in Word as an ordinary redline, attributed to the
author you chose, ready to accept or reject.

The document stays on your machine with the server; the agent requests only
the parts it needs. See
[MCP setup and configuration for other clients](stemma-mcp/README.md).

## Quick start: command line

The CLI installs from source via the Rust toolchain:

```bash
cargo install stemma-cli
```

### Compare two versions

If you have an original and a revised document, turn their differences into
native tracked changes:

```bash
stemma compare as-sent.docx as-returned.docx \
  -o changed.docx \
  --author "Approved Reviewer"
```

Stemma creates `changed.docx` and reports the result:

```text
wrote redline to changed.docx (<n> tracked revisions); bytes=<n> sha256=<hex> collision_policy=create_new disposition=created
```

**Rejecting every change reconstructs `as-sent.docx`. Accepting every change
reconstructs `as-returned.docx`.**

### Apply approved changes

For a controlled worklist, first inspect the exact identity of the document:

```bash
stemma validate agreement.docx
```

The command prints the file's byte count and SHA-256. Put those values into a
short approved worklist:

```json
{
  "schema": "stemma.worklist.v0",
  "input": {
    "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    "bytes": 48271
  },
  "author": "Approved Reviewer",
  "changes": [
    {
      "id": "payment-term",
      "old": "Payment is due within 30 days.",
      "new": "Payment is due within 45 days.",
      "expected_matches": 1
    }
  ]
}
```

Save that as `changes.json`, replacing the example hash and byte count with the
values reported for your document. Then create the redline:

```bash
stemma apply agreement.docx \
  --worklist changes.json \
  -o agreement-redline.docx
```

On success, Stemma writes `agreement-redline.docx`, saves a durable receipt at
`agreement-redline.docx.receipt.json`, and returns the same receipt on stdout.
Its decisive fields include:

```json
{
  "status": "complete",
  "deliverable": true,
  "summary": {
    "total": 1,
    "applied": 1,
    "refused": 0
  }
}
```

The complete receipt also records exact artifact hashes and every item outcome,
including diagnoses for refused changes.

`agreement-redline.docx` is a new Word document containing a native tracked
replacement. The original is never overwritten. If the old text is missing,
duplicated, or unsafe to replace, Stemma refuses the worklist instead of
silently choosing a target.

See the [worklist format and complete CLI contract](https://stemma.sh/docs/reference/cli#apply).

## Why Stemma

Word documents are not plain text. They contain revisions, formatting,
comments, tables, fields, notes, and content that must survive an edit.

Common automation approaches either flatten the document or manipulate its XML
directly. Stemma models Word revisions explicitly, including what accepting or
rejecting each change must produce.

- **Reviewable output:** changes appear as native Word revisions.
- **Bounded execution:** stale, missing, or ambiguous instructions are refused.
- **Preservation:** existing revisions and content outside the requested change
  remain part of the document.
- **Verified delivery:** output is validated and written to a new path without
  replacing the source or another existing file.
- **Multi-file evidence:** an MCP task can bind declared replacements and inputs
  before mutation, then emit a manifest that is independently checkable from
  the delivered files. The manifest does not prove undeclared intent.

## Current scope

The focused CLI worklist currently supports explicit old-to-new changes in
top-level body paragraphs. It can guard expected match counts, restrict a
replacement to a block or range, and normalize deliberate whitespace or quote
differences. Unsupported or ambiguous cases are reported rather than guessed.

The engine and MCP server expose broader editing and revision workflows. Stemma
is still pre-1.0, so experimental contracts may change between `0.x` minor
releases with changelog notice.

Stemma is not intended for:

- authoring documents from scratch or from a template language (filling an
  existing `.docx` template by editing it is in scope);
- one-way conversion from DOCX to Markdown or HTML;
- byte-identical XML round trips;
- replacing Word as a general-purpose interactive editor.

See the [fidelity contract](https://stemma.sh/docs/guide/fidelity) and
[stability policy](https://stemma.sh/docs/guide/stability) before building a durable
integration.

## Documentation by goal

- Applying approved changes: [CLI reference](https://stemma.sh/docs/reference/cli)
- Verifying a multi-document delivery: [task-delivery guide](https://stemma.sh/docs/guides/verify-task-delivery)
- Connecting an agent: [MCP setup](stemma-mcp/README.md)
- Embedding the Rust engine: [engine README](stemma-engine/README.md)
- Understanding revisions and fidelity: [guide](https://stemma.sh/docs/guide/concepts)
- Reviewing evidence: [benchmarks](https://stemma.sh/docs/benchmarks)
- Contributing: [contributor guide](CONTRIBUTING.md)

## Development

From a source checkout:

```bash
mise install
just gate
```

The workspace contains the Rust engine, CLI, MCP server, shared artifact
boundary, and a local HTTP/editor demonstration. See the
[architecture map](https://stemma.sh/docs/internals/architecture) for the component layout.

Most of the code was written with AI assistance. Human maintainers provide the
domain model, product direction, review, and release decisions.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for setup and pull request expectations.
Report security issues through [SECURITY.md](SECURITY.md).

## License

Licensed under either [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at
your option.
