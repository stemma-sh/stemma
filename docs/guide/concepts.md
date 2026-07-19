# Concepts

Everything stemma does is one pipeline:

```
DOCX bytes -> import -> CanonDoc -> edit / diff -> apply -> serialize -> DOCX bytes
```

Four ideas carry the whole system.

## The document is typed, not text

Importing a `.docx` produces a **CanonDoc**: an ordered list of blocks
(paragraphs, tables, opaque content like images and fields), each with a
**stable id** (`p_7`, `tbl_2`). Every read shows these ids and every edit
targets them — you never address content by line number or byte offset.
Content stemma has no typed model for (an exotic extension, a foreign
namespace) is not dropped: it is preserved verbatim and re-emitted in place.

A document is more than its body. Footnotes, endnotes, headers, footers, and
comments are separate **stories** — parallel block sequences that reads,
edits, and tracked-change resolution all reach.

## One file is three documents

A document carrying tracked changes is really three documents at once: the
text **as it stands** (changes pending), the text **if everything is
accepted**, and the text **if everything is rejected**. Stemma exposes each
as a read-only **projection** (`to_text` for the redline, `read_accepted`,
`read_rejected`). Projections never mutate the stored document — they are how
you check what a reviewer will actually receive before you commit to it.

```rust
// One file, three readings. `to_text()` is the redline (deletion AND insertion
// visible); `read_accepted()` resolves as accepted; `read_rejected()` as rejected.
let redline = edited.to_text();
let accepted = edited.read_accepted().expect("accept-all").to_text();
let rejected = edited.read_rejected().expect("reject-all").to_text();
assert_ne!(accepted, rejected, "the two resolutions genuinely differ");
```

Runnable: `cargo run -p stemma --example walk_the_document`.

## Edits are transactions with receipts

Changes are applied as atomic **transactions**: a list of ops plus an author.
Either every op lands or none do. Every write returns a **receipt** naming
exactly what happened — which blocks changed, which revision ids were
created, where a moved block landed. Every refusal is an error that names
what went wrong *and what to do instead*. There is no silent partial success
anywhere in the surface.

Ops are anchored optimistically: an edit carries the text it `expect`s to
find (or a content hash), so an edit planned against stale state fails
loudly instead of changing the wrong thing.

## Nothing leaves unvalidated

Serialization runs a post-serialization OOXML linker — a structural
validation pass over the emitted package — before bytes are written;
`save_docx` refuses to persist a structurally corrupt file. Transport output
commits are create-new: inputs and every existing destination are refused.
Bytes are staged beside the destination, committed without clobbering, and
verified by length and SHA-256 before success. This is collision-safe
visibility, not a power-loss durability claim.

Next: [Revisions](revisions.md) — the tracked-change type system these
concepts exist to serve.
