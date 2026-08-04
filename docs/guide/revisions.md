# Revisions

Tracked changes are stemma's reason to exist. This chapter is the mental
model; the [editing chapter](editing.md) covers how to author them.

## The type system

Word's revision machinery is richer than "inserted text, deleted text."
Stemma models each kind as a first-class type with its own accept/reject
semantics:

- **Text insertions and deletions** (`w:ins`/`w:del`), including runs
  nested inside other authors' revisions.
- **Moves** (`w:moveFrom`/`w:moveTo`) use a paired source and destination,
  but enumerate and resolve as ONE atomic revision: accepting lands the text
  at its destination, rejecting restores the original position, and either
  way every piece of the move's markup (both containers and the range
  bookmarks pairing them) leaves the document. This holds under bulk
  (`accept-all`/`reject-all`) and selective (by id, by author) resolution
  alike; a resolved document never carries a half-move.
- **Formatting changes** cover paragraph (`w:pPrChange`), run (`w:rPrChange`),
  table, row, cell, and section-properties changes. Each carries a complete
  snapshot of the *previous* formatting; rejecting restores that snapshot
  exactly, down to fields like keep-next and borders.
- **Paragraph-mark changes** have structural effects. Deleting a paragraph mark *joins two
  paragraphs* on accept (§17.13.5.15). This is the semantics hand-editing
  gets wrong most often.
- **Structural changes** include inserted or deleted table rows and whole
  tracked blocks.

All of these are enumerable (`Document::revisions()` in Rust,
`list_revisions` over MCP, `extract` in the CLI) and resolvable across
every story, including the body, footnotes, tables, and section properties.

## Authorship

Every revision carries an author name, and in a review that name is
load-bearing: whoever steps through the redline decides what to accept
partly by *who proposed it*. The dangerous failure is an edit that hides
inside someone else's authorship. Write under "Opposing Counsel" and your changes
become indistinguishable from theirs to every reviewer after you.

Stemma therefore refuses, by default, to author a revision under any name
that already has revisions in the document (`AuthorImpersonation`). The
refusal is deliberately blunt: a name is not an authenticated identity, so
stemma cannot know that you *are* the prior author, even when it is your own
name from an earlier round. Continuing an existing author's work is
always an explicit assertion (`allow_existing_author`), never a default an
agent can drift into.

Two edge shapes of authorship are worth stating exactly:

- **A blank author is a real author group.** Word anonymization ("Remove
  personal information") and some third-party tools write `w:author=""`.
  Stemma models that as the empty-string author: it enumerates as
  `author: ""` and is selectable as a group like any other name (the CLI
  selector token is the empty string, `--accept-author ""`).
- **A missing author is refused.** A tracked change with no `w:author`
  attribute at all fails import (`missing required tracked change
  attribute: author`) rather than being silently adopted into some default
  identity, so an enumerated revision always has an author value.

## Revision ids are content-derived identities

An id is not read from the file's `w:id` attributes (Word does not keep those
unique) and not a parse-order counter. The engine mints it as a hash of the
change's own identity (kind, story, author, date, and content), plus a
disambiguating ordinal among identical duplicates. Two consequences:

- **Durability.** A revision whose own content is untouched keeps its id
  across serialize/reopen and across resolution of *other* revisions. Ids
  enumerated before a selective resolve remain valid against its output, and
  against the saved file reopened tomorrow.
- **The re-key edge.** If an operation alters a surviving revision's content
  (or removes an identical-signature duplicate ahead of it), that survivor's
  signature, and so its id, changes. This is the honest edge of a
  content-derived identity: the id names *what the change is*, so a change
  that becomes something else is no longer addressable by its old name.

Practical discipline: read ids from the enumerator (`Document::revisions()`,
MCP `list_revisions`, the CLI's `extract` or `read`, or a receipt), never
from raw XML. Re-enumerating after each step is
always safe and costs one read; author/all selectors need no id carrying at
all. A selection naming an id that no longer exists fails loud (the CLI's
refusal lists the ids actually pending) rather than resolving something else.

## Accept and reject are not symmetric erasures

Both accepting and rejecting a change remove its marker. Therefore, "the
marker is gone" tells you nothing about *which* happened. The difference is content:
**accept keeps the new state; reject restores the prior state exactly.** If
you need to verify a resolution (yours or anyone's), compare content, not
markers: does the clause read twelve months or six?

```rust,no_run
use std::collections::HashSet;
use stemma::api::{Document, Resolution, ResolveSelectionAction, RuntimeError};

/// Reject one author's pending changes; leave everyone else's untouched.
fn reject_authors_changes(doc: &Document, author: &str) -> Result<Document, RuntimeError> {
    // Ids come from the census: each record carries `revision_id: u32`, and
    // `Resolution::Selective` takes the selection as a `HashSet<u32>`.
    let ids: HashSet<u32> = doc
        .revisions()
        .into_iter()
        .filter(|r| r.author.as_deref() == Some(author))
        .map(|r| r.revision_id)
        .collect();
    doc.project(Resolution::Selective { ids, action: ResolveSelectionAction::Reject })
}
```

The result of `project` is a full `Document`: verify the rejection by content
(does the clause still read "twelve months"?), project it again for the next
pass of a mixed triage, or serialize it as the resolved `.docx`; the
[embedding page](../reference/embedding.md#resolving-a-redline-tier-1) shows
that whole flow. Runnable, with the content-level verification spelled out:
`cargo run -p stemma --example resolve_a_redline`.

This has a useful corollary. A document's **committed text**, meaning what it
says if every pending change is rejected, is derivable at any time. It is
what your counterparty's "reject all" button produces. When stemma resolves
selectively (accept this author's changes, reject that one change), the
engine guarantees the untouched revisions are preserved marker-for-marker
and the resolved ones land on the correct side of that line.

Next, read [Editing](editing.md) for transactions, receipts, and the
review-then-save discipline.
