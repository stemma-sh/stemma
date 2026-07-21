# Troubleshooting

Use this page when a CLI command or MCP tool refuses to continue. Stemma errors
are intended to identify the failed invariant and the next safe action.

## The output path already exists

Stemma outputs are create-new. It will not overwrite an existing file or write
through an alias of an input.

Choose a new output path, or deliberately remove or rename the old output
before retrying.

## A replacement found zero or multiple matches

The requested text does not identify exactly the expected content.

- Copy the text from a current Stemma inspection rather than retyping it.
- Add surrounding words to make the match unique.
- Restrict the replacement to a block or range.
- Use `normalize_ws` only for deliberate whitespace or quote normalization.
- Change `expected_matches` only when multiple replacements are truly intended.

Do not broaden the operation merely to make the error disappear.

## The input hash or byte count does not match

The worklist was approved for different bytes. Run:

```bash
stemma validate agreement.docx
```

If the document legitimately changed, review the current content and approve a
new worklist with the new identity. Do not copy the new hash into an old
approval without reviewing the changed document.

## `stemma apply` exits 3

Exit `3` means the worklist was evaluated but at least one item refused. The
receipt is partial and not deliverable. By default no DOCX is created.

Read `items` in the receipt, correct each refused instruction, and run again
with a fresh output path.

## A match is reported as unreachable

The text was detected outside the focused CLI worklist's supported top-level
body-paragraph scope, commonly in a table cell or another document story.

Use the MCP or Rust engine surface that supports the relevant structure. Do not
assume the CLI searched or edited that region.

## An author name is refused

The document already contains revisions under that author. Stemma refuses to
make new work indistinguishable from existing work by default.

Use a distinct author. Only use an explicit existing-author override when you
intend to continue that author's revision identity.

## An MCP path is outside the workspace

Every MCP file path must resolve under `STEMMA_MCP_WORKSPACE_ROOT`. Symlinks
that escape the root are also refused.

Move the artifact under the configured root or restart the server with a
different explicit root. No tool argument can widen the boundary at runtime.

## An MCP edit is stale

Another edit changed the addressed block after it was inspected. Re-read the
block, rebuild the operation from the current guard or expected text, preview
again, and then apply.

## A document fails validation

The input is not a readable DOCX package or the generated result violates a
structural rule. Preserve the exact error and file identity when reporting the
problem. Do not convert a validation failure into apparent success.

## Still stuck?

- CLI details: [CLI reference](../reference/cli.md)
- Agent details: [MCP core reference](../reference/mcp.md)
- Advanced refusals: [MCP advanced reference](../reference/mcp-advanced.md)
- Security reports: [SECURITY.md](../../SECURITY.md)
- Other bugs: use the repository issue templates
