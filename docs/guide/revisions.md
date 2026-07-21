# Revisions

Tracked changes are stemma's reason to exist. This chapter is the mental
model; the [editing chapter](editing.md) covers how to author them.

## The type system

Word's revision machinery is richer than "inserted text, deleted text."
Stemma models each kind as a first-class type with its own accept/reject
semantics:

- **Text insertions and deletions** (`w:ins`/`w:del`), including runs
  nested inside other authors' revisions.
- **Moves** (`w:moveFrom`/`w:moveTo`) use a paired source and destination;
  rejecting restores the original position.
- **Formatting changes** cover paragraph (`w:pPrChange`), run (`w:rPrChange`),
  table, row, cell, and section-properties changes. Each carries a complete
  snapshot of the *previous* formatting; rejecting restores that snapshot
  exactly, down to fields like keep-next and borders.
- **Paragraph-mark changes** have structural effects. Deleting a paragraph mark *joins two
  paragraphs* on accept (§17.13.5.15). This is the semantics hand-editing
  gets wrong most often.
- **Structural changes** include inserted or deleted table rows and whole
  tracked blocks.

All of these are enumerable (`list_revisions`) and resolvable across every
story, including the body, footnotes, tables, and section properties.

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

## Revision ids are session handles

Ids are assigned when a document is imported and are stable for that
session; they are not durable properties of the file. Resolve changes
against what `list_revisions` returns *now*. An id remembered from an earlier
session, or read out of the raw XML, may address nothing or may address
something else.

## Accept and reject are not symmetric erasures

Both accepting and rejecting a change remove its marker. Therefore, "the
marker is gone" tells you nothing about *which* happened. The difference is content:
**accept keeps the new state; reject restores the prior state exactly.** If
you need to verify a resolution (yours or anyone's), compare content, not
markers: does the clause read twelve months or six?

```rust
// Accept everything one author proposed; reject another's specific change.
let resolved = after_accept
    .project(Resolution::Selective { ids: bob_now, action: ResolveSelectionAction::Reject })
    .expect("reject Bob's change");
// Verify by content. Every marker is gone, so check the text.
assert!(
    p3.starts_with(P3_ORIGINAL) && !p3.contains("Bob's rewrite"),
    "rejecting Bob restored the prior text instead of merely removing a marker"
);
```

Runnable: `cargo run -p stemma --example resolve_a_redline`.

This has a useful corollary. A document's **committed text**, meaning what it
says if every pending change is rejected, is derivable at any time. It is
what your counterparty's "reject all" button produces. When stemma resolves
selectively (accept this author's changes, reject that one change), the
engine guarantees the untouched revisions are preserved marker-for-marker
and the resolved ones land on the correct side of that line.

Next, read [Editing](editing.md) for transactions, receipts, and the
review-then-save discipline.
