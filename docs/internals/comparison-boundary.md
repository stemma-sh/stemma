# Comparison boundary

Stemma's document comparison is a downstream subsystem of the document
engine. The engine can parse, edit, track, package, resolve, serialize, and
verify a DOCX without knowing how two independently authored documents were
aligned. Comparison infers that alignment and compiles it into the engine's
tracked-change and package operations.

```text
accepted base ─┐
               ├─ compare ─> verified semantic and physical plans
accepted next ─┘                         │
                                         v
                         engine materialization and verification
                                         │
                                         v
                               tracked-change DOCX
```

## Ownership test

Ask whether the behavior would still be useful if a caller explicitly
requested the edit instead of asking Stemma to infer how one document became
another.

- If yes, it belongs to the engine.
- If it exists to infer or present the relationship between two documents, it
  belongs to `compare`.

For example, importing an image relationship safely is an engine operation;
inferring that one image replaces another is comparison. Emitting a native
paragraph-property revision is an engine operation; choosing the clearest
honest revision envelope is comparison.

## Current crate map

The compile-time boundary is a workspace crate:

```text
stemma-diff  --->  stemma
stemma       -X->  stemma-diff
```

Product callers use `stemma_diff::diff` or `stemma_diff::diff_as`. There is no
comparison method on `stemma::api::Document`, no top-level document-comparison
orchestration or result model in `stemma::domain`, and no compatibility alias
for the former proof-planner path. Reusable localized before/after change and
table-plan types remain in the engine's version-bound domain layer because an
explicit edit can use them without inferring correspondence between documents.

Inside `stemma-diff`:

- `compiler` owns the product's correspondence, alignment, story pairing, and
  opinionated review presentation;
- `model` owns comparison-only results and read projections;
- `materialize` compiles inferred changes into engine-native tracked state;
- `table_diff` owns comparison-specific table inference.

`tracked_document_view` also lives in `stemma-diff`: it is the rich,
comparison-aware client projection used by review renderers, not an engine
mutation primitive. The engine retains the compact document read model and all
state needed to build either projection.

Inside `stemma`, accepted-reading normalization, native revision carriers,
package closure, transport remapping, serialization, Accept/Reject projection,
and validation remain engine operations. Localized before/after compilation
used by an explicitly targeted edit also remains in the engine; it does not
infer correspondence between independent documents.

The product exposes no mechanism selector, presentation policy, capability
probe, package-plan command, or corpus budget. There is one Stemma comparison
and one review presentation.

There is one product presentation: Stemma's. Word and other comparison engines
are empirical references for valid carriers and useful behavior, not selectable
review personalities.

## Plan seam

Comparison does not pass an unverified inferred change directly into OOXML.
Its product sequence is:

```text
accepted readings
  -> comparison-owned change model
  -> engine-native tracked state
  -> serializer and package linker
  -> independent Accept/Reject verification
```

Experimental proof plans, package censuses, benchmark traces, and capability
research are not compiled into either product crate.

## Invariants

The boundary preserves:

1. comparison over accepted input readings;
2. proof-driven semantic correspondence;
3. no similarity-created semantic lineage;
4. native Accept and Reject terminal verification;
5. source-based closed package construction;
6. complete transport-reference rewriting;
7. no hidden fallback; and
8. typed refusal when no verified plan exists.

New comparison behavior should normally be one inference or presentation rule
that lowers through an existing verified physical operation and terminal
verifier. If it instead requires comparison-specific branches across parsing,
package writing, serialization, and receipts, the ownership boundary is wrong
or the engine is missing a genuinely reusable operation.
