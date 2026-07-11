# Fidelity contract

What stemma guarantees about its output — and one thing it deliberately does
not. The short version: **rendering and content are the contract; byte
identity of the serialized XML is not.**

## What is guaranteed

- **Render fidelity.** A document edited by stemma looks the same in Word,
  outside the edit, as it did before. The model separates *authored* values
  from *effective* (inherited/computed) ones and serializes only what the
  document actually said — an authored literal font is never silently
  replaced by a theme reference, an `auto` color never becomes a theme
  color, authored-off toggles round-trip as authored-off.
- **Content completeness.** Nothing is silently dropped. Constructs the
  typed model understands are carried typed; constructs it does not are
  preserved verbatim and re-emitted at their schema position (the
  `PreservedProp` mechanism); opaque objects (images, equations, fields,
  unknown elements) are carried byte-for-byte. Where a gap in this coverage
  is known, it is inventoried and gated (see below), not ignored.
- **Fail-loud.** If stemma cannot import a construct faithfully, it refuses
  with an error — it does not guess. Every output is checked by an OOXML
  validator before bytes leave the engine.
- **Tracked-change semantics.** Accepting or rejecting stemma-authored
  changes — in stemma or in Word — restores the document's *content and
  rendering* according to Word's own semantics. This is verified against a
  real Microsoft Word instance as a behavioral oracle.

These properties are enforced by the test suite, including a per-block
untouched-content fidelity gate that runs a tracked edit over several
hundred real-world documents and censuses every block the edit did not
touch. Its list of known residual classes is documented in the gate and
only ever shrinks.

## Explicitly out of contract: byte identity

Opening a document, making an edit, and saving **rewrites the serialized
XML** — attribute order, namespace prefixes, self-closing style, rsids,
revision-id numbering, and whitespace between tags may all change in parts
of the file the edit never touched. This is an intentional design decision,
not an accident:

- **Word does the same.** Open a `.docx` in Word, type one character, save:
  the whole file is rewritten. No tool in the ecosystem provides byte
  stability for an edited document.
- **Every consumer that matters keys on content, not bytes.** Word,
  Word Compare, and stemma's own projections compare text and effective
  formatting. The churn is invisible to all of them; the properties that
  *are* visible are exactly the guaranteed ones above.
- **Byte preservation would grade the wrong thing.** The user-visible
  fidelity bugs we have found and fixed were all *model expressiveness*
  bugs, and fixing them in the model fixed them everywhere — including in
  the edited content itself, where byte-splicing untouched blocks could
  never reach.

Practical consequences, stated plainly:

- Checksums, content-addressed stores, and raw-XML diffs (e.g. a
  git-tracked `.docx`) **will** show changes on untouched content after an
  edit. Use Word Compare or a content-level diff instead.
- "Reject all changes" restores the document's content and rendering, not
  its original bytes. Keep the original file if you need it — exactly as
  you would before letting Word save over it.
- The one byte-stable path: opening and saving **without any edit**
  round-trips the original package byte-identically.

If your workflow contractually requires byte-identical untouched content,
that requirement is not met today — tell us about the use case rather than
discovering the churn in production.
