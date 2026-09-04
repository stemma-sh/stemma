# Stemma — User Guide

A one-page tour of the public API. Stemma is a headless engine for Word documents
that carry **tracked changes**: it parses a `.docx` into a typed model, authors
changes as valid tracked-change OOXML, and proves the result is valid before it
leaves the engine. The downstream `stemma-diff` crate discovers changes between
independently authored documents.

If you want the *why* behind the model, read [`domain-model.md`](../domain-model.md).
This page is the *how*.

---

## The whole API in one screen

The engine revolves around one type, `Document`, and a handful of verbs. Every
verb is a pure value transformation: it returns a **new** `Document` and never
mutates the one you hold. Comparison is an optional downstream operation over
two `Document` values.

```rust,no_run
use stemma::api::{Document, EditTransaction, RuntimeError};
use stemma::{ExportOptions, Resolution};

fn document_lifecycle(
    docx_bytes: &[u8],
    transaction: &EditTransaction,
    base: &Document,
    target: &Document,
) -> Result<Vec<u8>, RuntimeError> {

// 1. Parse bytes into the typed model.
let doc = Document::parse(&docx_bytes)?;
for diagnostic in doc.diagnostics() {
    eprintln!("import: {}", diagnostic.message);
}

// 2. Author a change (see "Authoring edits" below for the transaction).
let edited = doc.apply(&transaction)?;

// 3. ...or ask the downstream comparison crate to discover changes.
let comparison = stemma_diff::diff_detailed(&base, &target)?;
for diagnostic in &comparison.base_diagnostics {
    eprintln!("base import: {}", diagnostic.message);
}
for diagnostic in &comparison.target_diagnostics {
    eprintln!("target import: {}", diagnostic.message);
}
let redlined = comparison.document;

// 4. Resolve tracked changes: accept-all, reject-all, or a selected set.
let clean = edited.project(Resolution::AcceptAll)?;

// 5. Emit DOCX bytes (runs the validator gate first).
let out: Vec<u8> = edited.serialize(&ExportOptions::default())?;
# Ok(out)
# }
```

That is the entire surface. The sections below expand each verb.

---

## The verbs

| Verb | Signature | What it does |
|---|---|---|
| `parse` | `&[u8] -> Document` | Decode a `.docx`. Fails fast on unsafe or unrecognized state; records any bounded import normalization. |
| `diagnostics` | `() -> &[Diagnostic]` | Immutable import provenance, including every deterministic normalization. |
| `apply` | `&EditTransaction -> Document` | **Author** new tracked changes. Precondition-checked and atomic. |
| `project` | `Resolution -> Document` | Resolve tracked changes: `AcceptAll`, `RejectAll`, or `Selective`. |
| `serialize` | `&ExportOptions -> Vec<u8>` | Emit DOCX bytes. Runs the blocking linker and any caller-supplied validator before returning. |
| `check` | `&EditTransaction -> Result<(), RuntimeError>` | `apply`'s dry run: run the same package-aware preconditions, mutate nothing. Answers "would this still apply, or is it stale?" |
| `read` | `() -> DocumentView` | A projection for inspecting/targeting blocks. Does not expose the internal IR. |

Plus free functions at the product edges:

```text
stemma::api::validate(&bytes) -> ValidationReport   // a property of bytes; no Document needed
stemma_diff::diff(&base, &target) -> Result<Document, RuntimeError>
```

Every fallible `Document` verb returns `Result<_, stemma::RuntimeError>`.

---

## Authoring edits

`apply` takes an `EditTransaction`: a small, serializable, replayable list of typed
steps. The canonical step is `ReplaceParagraphText` — replace one paragraph's text,
tracked, guarded by what you expect the paragraph to currently say.

```rust,no_run
use stemma::api::Document;
use stemma::edit_v4::parse_transaction;

fn replace_first_paragraph(doc: &Document) -> Result<Document, Box<dyn std::error::Error>> {

// Find the block you want to target via the read projection.
let view = doc.read();
let block = view.blocks.first().expect("document has a paragraph");
let transaction_json = format!(
    r#"{{"ops":[{{"op":"replace","target":"{id}","guard":"{guard}",
        "expect":"Hello world","content":{{"type":"paragraph","content":[
        {{"type":"text","text":"Goodbye world"}}]}}}}],
        "revision":{{"author":"Jane","date":"2026-05-31T00:00:00Z"}}}}"#,
    id = block.id,
    guard = block.guard,
);
let txn = parse_transaction(&transaction_json)?.into_edit_transaction()?;

Ok(doc.apply(&txn)?)
# }
```

The generated [v4 operation reference](../../../docs/reference/operations.md)
lists the complete supported vocabulary (insert/delete blocks, move ranges,
replace tables and hyperlinks, and more). `EditTransaction` is the *authoring*
vocabulary — keep it small and durable. Persist your DOCX bytes plus your
transactions and you can reconstruct any past state by replaying them.

### The `expect` precondition

`expect` is what makes edits safe against a moving document. If the paragraph no longer
says `"Hello world"` (someone else edited it, the document was re-imported, …), `apply`
fails with a stale-edit error instead of clobbering the wrong text. Use `check` to test
this without producing a document:

```rust,no_run
# use stemma::api::{Document, EditTransaction, RuntimeError};
# fn check_edit(doc: &Document, txn: &EditTransaction) -> Result<(), RuntimeError> {
match doc.check(&txn) {
    Ok(()) => { /* safe to apply */ }
    Err(e) => { /* stale or otherwise invalid; re-read and rebuild the edit */ }
}
# Ok(())
# }
```

---

## Discovering changes (diff)

When you have two documents and want the redline *between* them, use the
downstream `stemma-diff` crate. Each input is compared by its accepted reading;
the result is a `Document` whose tracked changes turn the accepted base into the
accepted target.

```rust,no_run
use stemma::api::{Document, RuntimeError};
use stemma::Resolution;

fn compare_documents(
    base_bytes: &[u8],
    target_bytes: &[u8],
) -> Result<(Document, Document), RuntimeError> {
let base   = Document::parse(&base_bytes)?;
let target = Document::parse(&target_bytes)?;
let comparison = stemma_diff::diff_detailed(&base, &target)?;

for diagnostic in &comparison.base_diagnostics {
    eprintln!("base import: {}", diagnostic.message);
}
for diagnostic in &comparison.target_diagnostics {
    eprintln!("target import: {}", diagnostic.message);
}

let redlined = comparison.document;

// Invariant you can rely on:
//   reject-all(redlined) == accepted base
//   accept-all(redlined) == accepted target
let back_to_base = redlined.project(Resolution::RejectAll)?;
let to_target    = redlined.project(Resolution::AcceptAll)?;
# Ok((back_to_base, to_target))
# }
```

`apply` and `stemma_diff::diff` produce the *same kind of thing* (a document
with attributed changes); they differ in ownership. The engine **authors** an
explicit edit. The downstream comparer **discovers** changes latent between two
documents and lowers them through engine-owned Word operations.

---

## Resolving changes (project)

`project` answers "what does the document look like if these changes are resolved."

```rust,no_run
use std::collections::HashSet;
use stemma::api::{Document, RuntimeError};
use stemma::{Resolution, ResolveSelectionAction};

fn project_document(doc: &Document) -> Result<(), RuntimeError> {

doc.project(Resolution::AcceptAll)?;   // keep every change
doc.project(Resolution::RejectAll)?;   // discard every change

// Accept or reject only specific revisions (by revision id):
let mut ids = HashSet::new();
ids.insert(1u32);
doc.project(Resolution::Selective { ids, action: ResolveSelectionAction::Accept })?;
# Ok(())
# }
```

`Selective` requires a non-empty id set; an empty set is rejected with a clear error
rather than silently doing nothing.

---

## Emitting DOCX (serialize)

```rust,no_run
use stemma::api::{Document, RuntimeError};
use stemma::{ExportOptions, ExportMode};

fn serialize_document(doc: &Document) -> Result<Vec<u8>, RuntimeError> {

// Default: redline output, no extra validation gate.
let bytes = doc.serialize(&ExportOptions::default())?;

// Gate output on an external validator (e.g. a Word-Oracle check). If the
// validator returns Err, serialize fails — nothing invalid leaves the engine.
let opts = ExportOptions {
    mode: ExportMode::Redline,
    validator_level: stemma::ValidatorLevel::Blocking,
    validator: Some(std::sync::Arc::new(|bytes: &[u8]| {
        // Run the caller's external check here. Return Err(msg) to block output.
        let _ = bytes;
        Ok(())
    })),
};
doc.serialize(&opts)
# }
```

The supported default runs the built-in post-serialization validator before
bytes leave the engine. The `validator` hook is an *additional* gate you
supply. `ExportOptions::unchecked()` is reserved for internal, non-delivered
bytes and makes opting out explicit.

---

## Inspecting a document (read)

`read()` returns a `DocumentView` — a designed single-document projection of the
document's blocks suitable for deciding *what to target* in an edit. Each `BlockView`
carries a stable `id` (the handle an `EditTransaction` targets), a `role`
(`Paragraph` / `Heading { level }` / `Table` / `Opaque`), the visible `text`, the
block and paragraph-mark tracked status, and `segments` for fine-grained inspection.

```rust,no_run
use stemma::api::{Document, SegmentView, TrackStatus};

fn inspect_document(doc: &Document) {

for block in doc.read().blocks {
    println!("{} [{:?}]: {}", block.id, block.role, block.text);

    // Inline structure: tracked-change spans and opaque anchors.
    for seg in &block.segments {
        match seg {
            SegmentView::Text { text, status, .. } => {
                if *status != TrackStatus::Normal {
                    println!("  {:?}: {:?}", status, text);   // an insertion/deletion
                }
            }
            // Opaque anchors (image, equation, field, …) carry their own id —
            // pass it to `ContentFragment::PreservedInlineRef` to keep them
            // through an edit.
            SegmentView::Opaque { id, kind, .. } => {
                println!("  opaque {:?} ({})", kind, id);
            }
        }
    }
}
# }
```

`DocumentView` is designed independently of the internal IR and exposes none of
the internal `CanonDoc` or change-vocabulary types. Its read shapes are still
engine-version-bound: do not persist them, and re-read the document after an
engine upgrade.

---

## Validation

```rust,no_run
use stemma::api::validate;

fn validate_bytes(bytes: &[u8]) {
let report = stemma::api::validate(&bytes);
if !report.ok {
    for issue in &report.issues {
        eprintln!("{:?}: {}", issue.code, issue.message);
    }
}
# }
```

`validate` is a property of bytes — use it on any `.docx`, no `Document` required. It is
the same check `serialize` runs on its output.

This is intentionally distinct from import diagnostics. `validate(&bytes)`
reports on the exact bytes. `Document::parse(&bytes)` may apply a narrowly
supported deterministic normalization and reports it through
`doc.diagnostics()`. In v0.6 the package-level exception is a dead root
thumbnail relationship, matching Word's save behavior; active missing targets
still fail.

---

## What to persist

- **Persist:** the DOCX bytes and your `EditTransaction`s. Together they reconstruct any
  past state (replay the transactions from a stored baseline).
- **Do not persist:** the in-memory `Document` / its snapshot. It is engine-version-bound
  by design — treat it as a hot value, not a storage format.

---

## Sessions (advanced)

`Document` owns no session state — it is just a value. If you are building a server that
holds many documents across requests, `stemma::SimpleRuntime` is an opinionated session
layer (a handle store with TTL eviction) built on the same engine. Most callers should
start with `Document`; reach for `SimpleRuntime` only when you need a managed multi-document
store. See its docs for the handle/eviction model.
