# Editing

How changes are made, and the discipline that keeps them safe.

## Transactions

An edit is a **transaction**: a list of ops, an author, an optional summary.
Transactions are atomic. Every op applies or none do. Ops target block ids
from the outline and carry an `expect` precondition (the text the op assumes
is there) or a content hash; if the document has changed under you, the op
fails with a stale-edit error instead of mutating the wrong content.

Two materialization modes:

- **Tracked** (the default): the edit lands as proper revision markup a
  reviewer can accept or reject. This is the mode for anything a human will
  review.
- **Direct**: the edit is applied as if a tracked edit had been made and
  immediately accepted. It has the same semantics without revision markup.
  Use it only when the
  task explicitly wants untracked changes.

Some operations are inherently direct because Word records no tracked change
for them:

- image resize and alt-text edits (`set_image_attrs`);
- `move_column` and `swap_cells`, which are refused in a tracked transaction
  (Word records no column or cell moves); send them with direct mode;
- `set_block_form_text` and `set_textbox_text`, which update the field result
  or the textbox's paragraphs in place;
- `merge_cells`, which is applied untracked even inside a tracked
  transaction, as Word's Merge Cells is with Track Changes on: neither the
  merge nor the text it moves into the top-left cell is a revision.

Accepting or rejecting revisions does not undo these changes. The audit
reports them as direct changes.

## Paragraph insertion

Paragraphs can be inserted before or after a body paragraph or a paragraph in
a table cell, including nested cells. Native list numbering and local paragraph
formatting are retained. Without a role, insertion inherits the anchor's
formatting for `before`, or the following paragraph's formatting for `after`
(the anchor's when there is no following paragraph in the same container).
An explicit role selects its vocabulary exemplar, which need not have the
same formatting as the nearest paragraph.

A paragraph inserted after the last paragraph a bookmark or a permission
range covers, or before the first, joins that range, in the body and in table
cells, as it does in Word when you press Enter at either edge of the covered
paragraph. A comment range does not grow, also as in Word: the new paragraph
lands outside it and the comment keeps covering the text it covered.

Cell insertion currently accepts paragraph blocks only. It refuses pending
tracked table/row/cell ancestors, cell content-control ranges, and literal
text numbering that would require renumbering. Resolve the enclosing change
first or use native list numbering where applicable; no weaker export path is
selected automatically.

## Body content controls

A body-level content control whose content is paragraphs and tables is
**open**: its blocks are ordinary blocks with their own ids, every edit
reaches them, and `sdt_text_fill` addresses the control by its first block's
id. A control holding anything else directly (a body-level bookmark, custom
XML) is **sealed**: it is preserved as one opaque block, `sdt_text_fill`
addresses it by `body_index`, and `set_block_form_text` reaches the form
fields inside it.

## Receipts and refusals

Every successful write returns a receipt: the ids of blocks that actually
changed, the revision ids created, move destinations. Trust the receipt over
your own mental model of what the edit did.

Every refusal names its escape hatch. A stale anchor tells you to re-read
the block; an ambiguous match lists every candidate so you can disambiguate
in one step; an author collision names the explicit override. If you hit an
error that leaves you stuck with no next move, that is a bug in stemma.
Please report it.

Two rules that exist because real agent transcripts showed their absence
failing:

- **Scope edits minimally.** "Replace the notice address" means the tokens
  the instruction denotes, not the trailing qualifiers next to them. Prefer
  the surgical verbs (`replace_text` with an exact needle) over widened
  spans.
- **Sessions are not interactive.** Decide, apply, and always save; an edit
  that is never saved does not exist.

## Review, then save

Saving is the commit gate. Before writing bytes, reconstruct what the
recipient will actually receive. Use the accept-all projection or a full
session review (`review_session`: everything this session changed, proof
that everything else is untouched, and a validator verdict), then check it
against what you were asked to do. The engine validates structure on every
save; *whether the change is the right change* is only checkable against
the projection, and "looked right in my live view" is the failure mode that
survives everything else.

The MCP `review_session` verb and the Rust `Document::review()` are the same
read-back. Both return the census of what you changed, the direct untracked
delta, and proof that everything else is untouched.

The proof is scoped to the canonical block model, not natural-language task
correctness or identical rendering in every Word consumer. Session review uses
retained block identities. File-pair audit uses explicit insertion markers and
conservative baseline positions; independently imported numeric IDs do not
establish lineage. Package validation is a separate structural check, not a
visual comparison. Always verify the requested content and formatting as well.

```rust
let report = doc.review().expect("review the session");
assert!(
    report.direct_changes.is_empty(),
    "every edit was tracked; an untracked delta would itself be a finding"
);
assert!(
    report.untouched.violations.is_empty(),
    "everything outside the two edited paragraphs is provably untouched"
);
// Only now, having read back what the recipient receives, commit to bytes.
let out = doc.serialize(&ExportOptions::default()).expect("serialize validated DOCX");
```

Runnable: `cargo run -p stemma --example review_before_save`.

The CLI and MCP always save to a new, unused path: inputs and every existing
destination are refused, and there is no overwrite option. MCP paths must also
stay inside its configured workspace root. The Python binding follows the same
rule by default; its `save(path, overwrite=True)` replaces an existing file
atomically. Successful transport commits report the exact byte
length and SHA-256 of the create-new artifact.

MCP also binds path-backed media to the session and output receipt. See the
[MCP filesystem contract](../reference/mcp.md#filesystem-and-artifact-boundary)
and [advanced image limits](../reference/mcp-advanced.md#path-backed-images)
for the transport-specific rules.

Next, read [Fidelity](fidelity.md) to understand what the output does and does
not promise.
