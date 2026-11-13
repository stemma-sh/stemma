# Troubleshooting

Use this page when a CLI command or MCP tool refuses to continue. Stemma errors
are intended to identify the failed invariant and the next safe action.

## The output path already exists

CLI and MCP outputs are create-new. They will not overwrite an existing file or
write through an alias of an input. In Python, `save(path)` follows the same
rule unless you pass `overwrite=True`, which replaces the file atomically.

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

## A match is inside a link or content control

A text replacement searches body and table-cell paragraphs, a hyperlink's
display text and an inline content control's value included, and replaces an
occurrence there inside the link or control. Some controls cannot be edited
this way: one whose content is locked (`w:lock` `contentLocked` or
`sdtContentLocked`), a drop-down, date, checkbox or other control whose value
is not free text, and one showing its placeholder. A link or control already
holding pending changes, or one the replacement would leave empty, is not
edited either. When the text occurs in one of those, the item refuses rather
than replacing only the other occurrences: the CLI receipt reports
`matches_inside_inline_objects`, and MCP `replace_text` reports
`ReplaceTextInsideInlineObjects` with `unreached_matches` naming each object
and why.

Unlock or fill the control (or resolve its pending changes) first, or narrow
the scope to exclude it, then run the replacement again. A match that runs
across a link's or control's edge refuses as a straddle: which part of the new
text belongs inside the object is not known. Headers, footers, notes and
comments are not searched at all; the CLI receipt lists them under
`coverage.unsearched`.

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

## An MCP tool asks for a `doc_id`

While exactly one document is open, tools may omit `doc_id`. With none or
several open, the call refuses with `doc_id_required` and lists
`open_doc_ids`; an id that names no open document refuses with
`unknown_doc_id` and the same list. Use the `doc_id` that `open_docx`
returned. A document idle past the server's timeout is closed
(`doc_evicted`); open it again.

## MCP `save_docx` reports `needs_review`

The file was written. The session audit found a change it cannot explain: a
direct (untracked) change, or a pre-existing revision that changed. Inspect
the output with `verify_docx` and fix or disclose each finding before
delivering it. A declared task refuses such a save instead and creates no
output.

## An MCP edit is stale

Another edit changed the addressed block after it was inspected. Re-read the
block, rebuild the operation from the current guard or expected text, preview
again, and then apply.

## An edit's `expect` runs across an inline object

`expect` is refused (`ExpectCrossesInlineObject`, wire code
`UnsupportedEdit`) when the text it names runs across an object the operation
cannot span: a field, an image, a line break, or for some operations a
bookmark or comment-range marker. The message names the object. Give text on
one side of it.

## A column operation names the wrong column

A column operation's `expect_header` must equal the text of the column's top
cell. On a mismatch the refusal lists every column with its 0-based index and
header; pick the index from that list.

## `edit_note` refuses the body's line count

A note of several paragraphs takes one line per paragraph, separated by `\n`
and in order (`NoteBodyLineCountMismatch`). Repeat a paragraph's current text
to keep it.

## A saved snapshot blob is refused

Snapshot blobs are a same-version cache: a blob written by another engine
release is refused on load. Re-import the source DOCX. See
[persistence](../guide/persistence.md).

## A document that opened before is refused

Some documents an earlier release opened are now refused at import, with the
element and attribute named: an on/off or numeric property no schema form
admits (which used to be dropped silently), or a tracked change whose previous
numbering names a list but no level. Stemma refuses rather than lose that
property. Report the case with the error message if Word opens the file.

## A document fails validation

The input is not a readable DOCX package or the generated result violates a
structural rule. Preserve the exact error and file identity when reporting the
problem. Do not convert a validation failure into apparent success.

Raw validation describes the exact bytes supplied. The Rust engine's
`Document::parse` can accept a narrowly supported Word-compatible import
normalization, but it reports that action through `Document::diagnostics()`;
it does not turn the original bytes into a validation pass. The only such
package normalization is removal of a dead package-root thumbnail
relationship whose target is absent. A missing relationship used by document
content is still a hard failure.

## Still stuck?

- CLI details: [CLI reference](../reference/cli.md)
- Agent details: [MCP core reference](../reference/mcp.md)
- Advanced refusals: [MCP advanced reference](../reference/mcp-advanced.md)
- Security reports: [SECURITY.md](https://github.com/stemma-sh/stemma/blob/main/SECURITY.md)
- Other bugs: use the repository issue templates
