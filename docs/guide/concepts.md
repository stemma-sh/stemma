# Concepts

Everything stemma does is one pipeline:

```text
DOCX bytes -> import -> CanonDoc -> edit / diff -> apply -> serialize -> DOCX bytes
```

Four ideas carry the whole system.

## The document is typed, not text

Importing a `.docx` produces a **CanonDoc**: an ordered list of blocks
(paragraphs, tables, opaque content like images and fields), each with a
**stable id** (`p_7`, `tbl_2`). Every read shows these ids and every edit
targets them. Content is never addressed by line number or byte offset.
Content stemma has no typed model for (an exotic extension, a foreign
namespace) is not dropped: it is preserved verbatim and re-emitted in place.

A document is more than its body. Footnotes, endnotes, headers, footers, and
comments are separate **stories**. They are parallel block sequences that
reads, edits, and tracked-change resolution all reach.

## One file is three documents

A document carrying tracked changes is really three documents at once: the
text **as it stands** (changes pending), the text **if everything is
accepted**, and the text **if everything is rejected**. Stemma exposes the
resolved readings as **projections** (`read_accepted`, `read_rejected`; the
redline itself is `to_text`). Projecting never mutates the source (every
verb returns a new value), but the result is not a text view: a projection
is a **full `Document`**, so you can read it, project it again, or serialize
it as the resolved `.docx` a reviewer will actually receive.

```rust,no_run
use stemma::api::{Document, ExportOptions, RuntimeError};

fn three_readings(edited: &Document) -> Result<(), RuntimeError> {
    // One file, three readings. `to_text()` is the redline (deletion AND
    // insertion visible); `read_accepted()` resolves everything as accepted;
    // `read_rejected()` as rejected.
    let _redline = edited.to_text();
    let accepted = edited.read_accepted()?;
    let rejected = edited.read_rejected()?;
    assert_ne!(
        accepted.to_text(),
        rejected.to_text(),
        "the two resolutions genuinely differ"
    );
    // A projection is a full Document: serializing the accept-all projection
    // writes the `.docx` a counterparty's "accept all" button would produce.
    let _resolved_docx: Vec<u8> = accepted.serialize(&ExportOptions::default())?;
    Ok(())
}
```

Runnable: `cargo run -p stemma --example walk_the_document`.

## Edits are transactions with receipts

A typed edit **transaction** is a list of operations plus an author. Either
every operation in that transaction lands or none do. Itemized replacement
worklists are deliberately different: each item reports its own applied or
refused outcome so a caller can reissue only what failed.

Every write returns a **receipt** naming exactly what happened: which blocks
changed, which revision ids were created, and where a moved block landed.
Every refusal names what went wrong and what to do instead. Partial outcomes
are always explicit and never reported as complete success.

Ops are anchored optimistically: an edit carries the text it `expect`s to
find (or a content hash), so an edit planned against stale state fails
loudly instead of changing the wrong thing.

## Nothing leaves unvalidated

Serialization runs a post-serialization OOXML linker before bytes are written.
This structural validation pass checks the emitted package, and it guards
**every** exit: `Document::serialize` runs it by default
(`ExportOptions::default()` gates at the blocking level; opting out is the
explicit, engine-internal `ExportOptions::unchecked`), and
`save_docx` refuses to persist a structurally corrupt file. Transport output
commits are create-new: inputs and every existing destination are refused.
Bytes are staged beside the destination, committed without clobbering, and
verified by length and SHA-256 before success. This is collision-safe
visibility, not a power-loss durability claim.

Next, read [Revisions](revisions.md) for the tracked-change type system these
concepts exist to serve.
