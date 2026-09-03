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

## `stemma compare` refuses a document pair

Comparison has one Stemma presentation and uses only engine-owned native Word
carriers whose Accept and Reject behavior is understood. A refusal means the
complete relationship between the two accepted readings could not be
represented safely; it is not retried under a weaker or more approximate diff.

Preserve both input identities and the complete error context when reporting
the case. A refusal that another DOCX tool does not make can be valuable
differential evidence, but the other artifact must still be checked for clean
opening, package closure, and correct native Accept/Reject results before it
demonstrates a missing Stemma capability.

## This author label is already in the document

That Word author label was present on pending revisions when the document was
opened.
New revisions with the same label will appear in Microsoft Word as part of the
same reviewer group.

Stemma has not changed the document.

To continue that reviewer group, retry with `allow_existing_author: true` over
MCP, `?allow_existing_author=true` over HTTP, or
`--allow-existing-author` in the CLI. To keep this editing round separate,
supply a different author label chosen by the user. Never invent a label merely
to clear this refusal.

This check concerns labels that were present when the document was opened.
Additional edits under the same newly chosen label in the current session are
allowed.

## An MCP path is outside the workspace

Every MCP file path must resolve under the selected workspace root. Symlinks
that escape the root are also refused. Run `stemma-mcp --help` to see the root
selection precedence.

Move the artifact under the configured root or restart the server with
`--workspace-root /path/to/documents`. No tool argument can widen the boundary
at runtime.

## An MCP edit is stale

Another edit changed the addressed block after it was inspected. Re-read the
block, rebuild the operation from the current guard or expected text, preview
again, and then apply.

## A document fails validation

The input is not a readable DOCX package or the generated result violates a
structural rule. Preserve the exact error and file identity when reporting the
problem. Do not convert a validation failure into apparent success.

Raw validation describes the exact bytes supplied. The Rust engine's
`Document::parse` can accept a narrowly supported Word-compatible import
normalization, but it reports that action through `Document::diagnostics()`;
it does not turn the original bytes into a validation pass. In v0.6 the only
such package normalization is removal of a dead package-root thumbnail
relationship whose target is absent. A missing relationship used by document
content is still a hard failure.

## Still stuck?

- CLI details: [CLI reference](../reference/cli.md)
- Agent details: [MCP core reference](../reference/mcp.md)
- Advanced refusals: [MCP advanced reference](../reference/mcp-advanced.md)
- Security reports: [SECURITY.md](https://github.com/stemma-sh/stemma/blob/main/SECURITY.md)
- Other bugs: use the repository issue templates
