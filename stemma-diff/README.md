# stemma-diff

Opinionated DOCX comparison built downstream of the Stemma Word engine.

`stemma-diff` owns the question “how did these independently authored
documents change?” It aligns accepted readings, chooses one Stemma review
presentation, and compiles the result into native tracked-change and package
operations supplied by `stemma`.

The dependency is intentionally one-way:

```text
stemma-diff  --->  stemma
stemma       -X->  stemma-diff
```

The public surface is deliberately small:

```rust,ignore
use stemma::api::Document;

let base = Document::parse(&base_bytes)?;
let target = Document::parse(&target_bytes)?;
let redline = stemma_diff::diff(&base, &target)?;
let attributed = stemma_diff::diff_as(&base, &target, "Reviewer")?;
```

Adapters that emit receipts should use `diff_detailed` or `diff_as_detailed`.
The detailed result includes the document, semantic change count, flattened
input revision censuses, and source-labelled import diagnostics. The
convenience functions return only the document.

There is one comparison contract and one presentation. Unknown or unsafe states
are refused with context; there is no fallback comparer and no selectable Word
imitation mode.

Each input is compared by its accepted reading. A successful result obeys the
same native carrier laws as an explicitly authored engine edit: Reject restores
the accepted base and Accept restores the accepted target. If no verified Word
carrier can represent that relationship, comparison returns a contextual typed
refusal rather than a partial redline.

The engine remains responsible for DOCX parsing, typed edits, native revision
carriers, package closure, serialization, validation, and Accept/Reject
projection. Comparison-specific correspondence, alignment, presentation, and
result models do not live in the engine crate.
