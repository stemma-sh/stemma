# Stemma — User Guide

A one-page tour of the public API. Stemma is a headless engine for Word documents
that carry **tracked changes**: it parses a `.docx` into a typed model, lets you
author or discover changes, materializes them as valid tracked-change OOXML, and
proves the result is valid before it leaves the engine.

If you want the *why* behind the model, read [`domain-model.md`](../domain-model.md).
This page is the *how*.

---

## The whole API in one screen

You work with one type, `Document`, and a handful of verbs. Every verb is a pure
value transformation: it returns a **new** `Document` and never mutates the one you
hold.

```rust
use stemma::api::{Document, validate};

// 1. Parse bytes into the typed model.
let doc = Document::parse(&docx_bytes)?;

// 2. Author a change (see "Authoring edits" below for the transaction).
let edited = doc.apply(&transaction)?;

// 3. ...or discover the changes between two documents.
let redlined = base.diff(&target)?;

// 4. Resolve tracked changes: accept-all, reject-all, or a selected set.
let clean = edited.project(stemma::Resolution::AcceptAll)?;

// 5. Emit DOCX bytes (runs the validator gate first).
let out: Vec<u8> = edited.serialize(&stemma::ExportOptions::default())?;
```

That is the entire surface. The sections below expand each verb.

---

## The verbs

| Verb | Signature | What it does |
|---|---|---|
| `parse` | `&[u8] -> Document` | Decode a `.docx`. Fails fast on anything unrecognized (encrypted package, missing `word/document.xml`). |
| `apply` | `&EditTransaction -> Document` | **Author** new tracked changes. Precondition-checked and atomic. |
| `diff` | `&Document -> Document` | **Discover** the changes between this document and another, materialized as tracked changes. |
| `project` | `Resolution -> Document` | Resolve tracked changes: `AcceptAll`, `RejectAll`, or `Selective`. |
| `serialize` | `&ExportOptions -> Vec<u8>` | Emit DOCX bytes. Runs the validator (and optional Word-Oracle gate) before returning. |
| `check` | `&EditTransaction -> Result<(), EditError>` | `apply`'s dry run: run the preconditions, mutate nothing. Answers "would this still apply, or is it stale?" |
| `read` | `() -> DocumentView` | A projection for inspecting/targeting blocks. Does not expose the internal IR. |

Plus a free function:

```rust
stemma::api::validate(&bytes) -> ValidationReport   // a property of bytes; no Document needed
```

Every fallible verb returns `Result<_, stemma::RuntimeError>` (except `check`, which
returns `EditError` because it is *about* whether an edit is valid).

---

## Authoring edits

`apply` takes an `EditTransaction`: a small, serializable, replayable list of typed
steps. The canonical step is `ReplaceParagraphText` — replace one paragraph's text,
tracked, guarded by what you expect the paragraph to currently say.

```rust
use stemma::edit::{
    ContentFragment, EditStep, EditTransaction, MaterializationMode, ParagraphContent,
};
use stemma::{NodeId, RevisionInfo};

// Find the block you want to target via the read projection.
let block_id = doc.read().blocks.first().unwrap().block_id.clone();

let txn = EditTransaction {
    steps: vec![EditStep::ReplaceParagraphText {
        block_id,
        expect: "Hello world".to_string(),     // precondition: fails if the text drifted
        content: ParagraphContent {
            fragments: vec![ContentFragment::Text("Goodbye world".to_string())],
        },
        rationale: None,
        replacement_role: None,
        semantic_hash: None,
    }],
    summary: None,
    materialization_mode: MaterializationMode::TrackedChange,  // vs. Direct (untracked)
    revision: RevisionInfo {
        revision_id: 1,
        author: Some("Jane".to_string()),
        date: Some("2026-05-31T00:00:00Z".to_string()),
        apply_op_id: None,
    },
};

let edited = doc.apply(&txn)?;
```

`ReplaceParagraphText` is one variant; see `stemma::edit::EditStep` for the full set
(insert/delete blocks, move ranges, replace tables and hyperlinks, …). `EditTransaction`
is the *authoring* vocabulary — keep it small and durable. Persist your DOCX bytes plus
your transactions and you can reconstruct any past state by replaying them.

### The `expect` precondition

`expect` is what makes edits safe against a moving document. If the paragraph no longer
says `"Hello world"` (someone else edited it, the document was re-imported, …), `apply`
fails with a stale-edit error instead of clobbering the wrong text. Use `check` to test
this without producing a document:

```rust
match doc.check(&txn) {
    Ok(()) => { /* safe to apply */ }
    Err(e) => { /* stale or otherwise invalid; re-read and rebuild the edit */ }
}
```

---

## Discovering changes (diff)

When you have two documents and want the redline *between* them, use `diff`. The result
is a `Document` whose tracked changes turn the base into the target.

```rust
let base   = Document::parse(&base_bytes)?;
let target = Document::parse(&target_bytes)?;
let redlined = base.diff(&target)?;

// Invariant you can rely on:
//   reject-all(redlined) == base
//   accept-all(redlined) == target
let back_to_base = redlined.project(stemma::Resolution::RejectAll)?;
let to_target    = redlined.project(stemma::Resolution::AcceptAll)?;
```

`apply` and `diff` produce the *same kind of thing* (a document with attributed
changes); they differ only in the act — `apply` **authors** changes you describe,
`diff` **discovers** changes latent between two documents.

---

## Resolving changes (project)

`project` answers "what does the document look like if these changes are resolved."

```rust
use std::collections::HashSet;
use stemma::{Resolution, ResolveSelectionAction};

doc.project(Resolution::AcceptAll)?;   // keep every change
doc.project(Resolution::RejectAll)?;   // discard every change

// Accept or reject only specific revisions (by revision id):
let mut ids = HashSet::new();
ids.insert(1u32);
doc.project(Resolution::Selective { ids, action: ResolveSelectionAction::Accept })?;
```

`Selective` requires a non-empty id set; an empty set is rejected with a clear error
rather than silently doing nothing.

---

## Emitting DOCX (serialize)

```rust
use stemma::{ExportOptions, ExportMode};

// Default: redline output, no extra validation gate.
let bytes = doc.serialize(&ExportOptions::default())?;

// Gate output on an external validator (e.g. a Word-Oracle check). If the
// validator returns Err, serialize fails — nothing invalid leaves the engine.
let opts = ExportOptions {
    mode: ExportMode::Redline,
    validator: Some(std::sync::Arc::new(|bytes: &[u8]| {
        // return Ok(()) to accept, Err(msg) to reject
        my_word_oracle_check(bytes)
    })),
};
let bytes = doc.serialize(&opts)?;
```

Serialize always runs the built-in post-serialization validator (≈20 codified rules
from ECMA-376 / MS-OI29500). The `validator` hook is an *additional* gate you supply.

---

## Inspecting a document (read)

`read()` returns a `DocumentView` — a designed single-document projection of the
document's blocks suitable for deciding *what to target* in an edit. Each `BlockView`
carries a stable `id` (the handle an `EditTransaction` targets), a `role`
(`Paragraph` / `Heading { level }` / `Table` / `Opaque`), the visible `text`, the
block and paragraph-mark tracked status, and `segments` for fine-grained inspection.

```rust
use stemma::api::{BlockRole, SegmentView, TrackStatus};

for block in doc.read().blocks {
    println!("{} [{:?}]: {}", block.id, block.role, block.text);

    // Inline structure: tracked-change spans and opaque anchors.
    for seg in &block.segments {
        match seg {
            SegmentView::Text { text, status } => {
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
```

`DocumentView` is its own stable type, designed independently of the IR: it exposes
**none** of the internal `CanonDoc` / change-vocabulary types, so your code never
depends on engine-version-bound internals.

---

## Validation

```rust
let report = stemma::api::validate(&bytes);
if !report.ok {
    for issue in &report.issues {
        eprintln!("{:?}: {}", issue.code, issue.message);
    }
}
```

`validate` is a property of bytes — use it on any `.docx`, no `Document` required. It is
the same check `serialize` runs on its output.

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
