# Stemma — The Domain Model

This is the canonical description of *what stemma is*. It is the document to read before touching the public API, the crate split, or the edit grammar. Everything else (the product brief, the README, the API reference) should be consistent with this.

It is written model-first, in the spirit of `CLAUDE.md`: name the data, name the allowed shapes, name the transitions, name the invariants. If those are right, the code's shape is forced.

> Looking for *how to use the engine* rather than *why it is shaped this way*? See the [user guide](user/guide.md) — a one-page tour of the `Document` API.

---

## 1. What stemma is, in one sentence

**Stemma is a library for working with documents that carry attributed change — it parses a Word document into a typed model, lets you author or discover changes, materializes those changes as valid tracked-change OOXML, and proves the result is valid before it leaves the engine.**

The name is from textual criticism: a *stemma* is the tree of how a text changed across copies. That is the domain — not "a DOCX reader," but "the structure of change in a document."

---

## 2. The core object: content plus attributed deltas

A normal document model answers "what does the document say." Stemma's model answers a harder question: **"what does the document say, what did it say before, and who changed it."**

A tracked-changes DOCX already encodes exactly this. Inside one file, two projections are superimposed:

- **reject-all** — the baseline. What the document was before this round of changes.
- **accept-all** — the target. What the document becomes if every change is accepted.

Between them sits a set of **attributed deltas**: each insertion, deletion, move, and formatting change, tagged with author and date. That triple — *baseline + target + attributed deltas, held in one structure* — is the core object. Everything stemma does is a function over it.

Internally this object is `CanonDoc` (`src/domain.rs:111`): a tree of `TrackedBlock`s, each a `BlockNode` (paragraph, table, or opaque) wrapped in a `TrackingStatus` (`Normal | Inserted(rev) | Deleted(rev)`), with deltas living at three layers — whole block, inline segment, and the paragraph mark itself. A pristine document is just this object with zero deltas (every status `Normal`). The public handle that wraps it is `Document` (see §9).

> **Naming note.** "Delta" / "change" here means the stemma-level concept. The OOXML word *revision* (a single `w:id`-bearing `w:ins`/`w:del`) is a narrower thing — one delta's serialized form. We avoid calling the whole object a "revision" to keep that distinction clean.

---

## 3. The spine

Every capability rides one path:

```
            typed intent  ──►  valid tracked OOXML  ──►  proven valid
          (what to change)     (materialization)        (validation)
```

- **Typed intent** is never loose XML. It is either an authored `EditTransaction` or a discovered alignment (§5). Both are typed, both validated at the edge.
- **Materialization** lowers intent into a `CanonDoc` whose tracked changes are structurally correct — balanced field ranges, paired moves, joined paragraph marks, preserved opaques. This is the one routine that must never be duplicated (§6).
- **Proven valid** means the serialized bytes are re-parsed and checked against ~20 codified rules from ECMA-376 / MS-OI29500 before they leave the engine, with an optional external Word-validation gate on top (via the `ExportValidator` hook).

This spine is the product. "We don't splice XML and hope Word opens it; we materialize into a typed model and refuse to emit a document we can't prove valid" is the whole pitch, and §6 is where it is won or lost.

---

## 4. The verbs

Six functions over the core object. This is the entire public vocabulary.

| Verb | Type | Meaning |
|---|---|---|
| `parse` | `bytes → Document` | Decode a DOCX into the typed model. Fail fast on anything unrecognized. |
| `read` | `Document → DocumentView` | A designed, stable projection for targeting and inspection (node IDs, role labels, text, tracked status, opaque anchors). Not an IR dump. |
| `apply` | `Document × EditTransaction → Document` | **Author** new deltas. Precondition-checked, atomic. |
| `diff` | `Document × Document → Document` | **Discover** the deltas between two baselines and materialize them as tracked changes. |
| `project` | `Document × Resolution → Document` | Resolve deltas: accept-all, reject-all, or selective. |
| `serialize` | `Document × ExportOptions → bytes` | Emit DOCX. Runs the validator (and optional Oracle) before returning. |

`validate(bytes) → ValidationReport` stands alongside as a property of bytes, usable without a `Document`.

`check(Document × EditTransaction) → Result<(), Vec<EditError>>` is `apply`'s dry-run twin — run the preconditions, mutate nothing. It is the highest-value verb for an agent or MCP loop: "would this still apply, or is it stale?" Do not fold it into `apply`.

### One model, one write path, many read projections

There are several representations in flight — DOCX bytes, the internal IR, the structured `DocumentView`, the LLM-facing extended-markdown — and it is tempting to read that as proliferation. It is not, once you sort them by **direction**:

- **In *and* out: DOCX bytes.** The durable source and target format. One, fixed.
- **Internal: `CanonDoc` (the IR).** The private working representation. One, hidden, free to evolve (that is the whole reason it is sealed — see §9).
- **In only: `EditTransaction`.** The *single* write path. Every mutation — authored or LLM-proposed — arrives as a transaction. There is no second way in, and no symmetric pressure to add `write(format = "markdown")`.
- **Out only: the read projections.** `DocumentView` (structured traversal, the ProseMirror-style node tree consumers iterate), extended-markdown (the LLM read surface), DOCX-out (`serialize`). *This is the only category that grows*, and it grows safely: each projection is a one-way renderer of the same sealed model, like ProseMirror's `toJSON` / `DOMSerializer` / `toText`.

So the surface is **not** `read(format = …)`. A format enum would falsely imply the projections are interchangeable encodings; they have different shapes, return types, and options. Instead each projection is its own typed method, named by audience:

```
doc.read()            -> DocumentView   // structured traversal (apps, stemma)
doc.to_markdown(opts) -> String         // extended-markdown (LLM read surface)
doc.serialize(opts)   -> Vec<u8>        // DOCX out
```

The invariant to hold: **one model, one write path (`EditTransaction`), many read projections.** Adding a reader (HTML, plain text, a different LLM dialect) is additive and safe; adding a *writer* is a deliberate change to the one path. New representations are almost always new readers — so they cost nothing architecturally.

Note that `read` is load-bearing and currently under-built: intent can only target what the caller first *saw*. The view is effectively the engine's query language and deserves its own stability contract, designed independently of the IR so the IR stays free to move underneath it.

**The view exposes structure, not positions.** A `DocumentView` is an ordered, *complete* list of spans per block — every text run, every opaque anchor, every hard break — each carrying its tracked status and content. It deliberately carries **no character offsets**. The reason is rigorous, not lazy: an offset is only well-defined relative to a single canonical "block text," and there is no such thing. Different consumers count differently — the edit path's `expect` treats a hard break as a *section boundary*, a redline renderer counts it as one position, an LLM view may render it as a tag. These are not bugs to reconcile; they are different *readings*, each correct for its reader. So position is a property of *a way of reading*, not of the document. This mirrors ProseMirror exactly: a `Node` does not store its position; `doc.descendants((node, pos) => …)` *computes* `pos` during the walk, in the caller's own counting. The engine therefore exposes the spans in order and completely; a consumer that needs offsets sums span widths **in its own width function** (e.g. "text = UTF-16 code units, hard break = 1, opaque = 0" — which is a renderer/frontend coupling, and belongs to that consumer, never baked into the engine surface). Exposing positions from the view would force the engine to canonicalize one reader's coordinate space and freeze it; exposing only structure keeps every reader honest and the IR free.

---

## 5. Authored vs discovered deltas — the key distinction

`apply` and `diff` look different but produce the *same kind of thing*: a `Document` with attributed deltas. The difference is not the output — it is the **act**.

- **`apply` authors.** An LLM or a human says "change this specific clause to say X." Intent names a target and a desired state. The engine infers the minimal deltas (word-diff) and materializes them.
- **`diff` discovers.** Given two baselines, the engine *finds* the deltas that turn one into the other. Nobody authored them; they were latent in the difference between the documents.

This is why "merge is edit" is *almost* right but worth stating precisely: **merge and edit are the same in their output and their materialization, and different in their producer.** Authoring and discovery are genuinely distinct operations with different inputs (one document + intent, versus two documents). Collapsing them into one surface type would be over-unification — two real things forced into one for tidiness.

What must be unified is not the act but the **lowering**: both must converge on one materializer, so a delta means the same valid OOXML no matter who produced it. That is the next section, and it is the only unification that buys correctness.

---

## 6. The one materializer (the load-bearing invariant)

> **Invariant M.** There is exactly one routine that lowers deltas into tracked-change `TrackedSegment`s, and every producer of deltas calls it. Field-character balance, move pairing, paragraph-mark joins, empty-segment pruning, and opaque reading-order preservation are enforced there and nowhere else.

This invariant **was violated** until 2026-05-31; the normalization passes have now converged (see "Status" below). It was the single most important correctness issue in the engine. Here is the verified picture, kept because it documents the original divergence and the shape of the fix.

**The word-diff engine is already shared.** Both paths tokenize with the same `tokenize` (`src/diff.rs:46`, legal-enumerator and apostrophe fusing included) and run the same `similar` Patience diff. The edit path's `diff_text_sections` (`src/edit.rs:2613`) is a thin adapter that *already calls* `crate::diff::diff_block_content` (`src/edit.rs:2616`). There is no second word-diff. Good.

**The materializer was duplicated.** Turning diff output into `Vec<TrackedSegment>` existed twice, with different invariant coverage. The "before" picture (✅ = pass ran on that path, ❌ = it didn't):

| Pass | Edit path (`edit.rs`) — before | Merge path (`tracked_model.rs`) — before |
|---|---|---|
| build segments | `reconstruct_section_segments` | `inline_changes_to_segments_with_opaques` (`:746`) |
| merge adjacent same-status segments | ✅ `normalize_segments` (`:3350`) | ❌ |
| merge adjacent same-format text nodes | ✅ | ❌ |
| coalesce split field sequences | ❌ | ✅ `coalesce_split_field_sequences` (`:373`) |
| normalize opaque reading order | ❌ | ✅ `normalize_paragraph_opaque_reading_order` (`:1053`) |
| formatting-change (rPrChange) | ❌ (input is plain text) | ✅ `detect_formatting_change` (`diff.rs:3300`) |

Each path was internally correct *for the input shape it was built for*, but neither enforced the union. The fixpoint invariant (`diff → merge → accept → re-diff = empty`) exercised only the merge path; the edit path was covered by separate metamorphic tests. So the two could drift: a bug compaction would catch was invisible to merge tests, a bug field-coalescing would catch was invisible to edit tests.

**Status (converged 2026-05-31).** Both paths now run the same three normalization passes in one fixed order — **`coalesce_split_field_sequences` → `normalize_paragraph_opaque_reading_order` → `normalize_segments`** (compaction last, so the structural passes still see un-merged boundaries). The two opaque/field passes are `pub(crate)` in `tracked_model.rs` and called from the edit path's Phase-4 normalize (`edit.rs`); `normalize_segments` is `pub(crate)` in `edit.rs` and called as the merge path's final pass. The passes are heavily guarded no-ops outside the exact pathological shapes they repair, so the convergence changed **no** well-formed output. Verified by differential: the full corpus fixpoint sweep produces the **identical set of 13 failing fixtures with and without the M change** (set-equality check, not just a count) — every one a pre-existing formatting-mark residual (`marks:[Underline]→[]` etc., the known formatting-tracked-changes gap), zero structural, zero text-content divergence introduced by M. (Those 13 were previously *masked*: an unrelated `vMerge` panic in `diff_documents` aborted the rayon sweep early; that table-structure bug was fixed alongside this work — see the `vMerge` restart-anchor fix in `apply_table_structure_changed` — which unmasked the formatting residuals without adding any.) The one row still NOT unified is **formatting-change (rPrChange)** — it is a *producer* difference (the edit path's input is plain text and carries no `formatting_change`), not a normalization difference, so it belongs to the §8 authoring-grammar roadmap, not here. The segment **builders** (`reconstruct_section_segments` vs `inline_changes_to_segments_with_opaques`) remain two functions; unifying the builders themselves is the deeper, higher-risk step deferred until a real need appears — the normalization convergence already closes the drift the safety net was built to catch.

**The fix.** Extract one materializer whose input is the existing change vocabulary (`InlineChange[]` per paragraph, plus block-level `DiffChange[]`) and whose output is `Vec<TrackedSegment>` with *all* passes applied in a fixed order. Both producers become adapters:

- `apply_replace_paragraph_text` already produces section diffs — it converts them to `InlineChange[]` and calls the materializer.
- `merge_diff` already has `InlineChange[]` — it calls the same materializer.

The change vocabulary (`InlineChange` / `DiffChange`) is the materializer's **internal input contract**. It is not a public type (see §9), but it is the seam where both producers meet. This is a refactor, not a rewrite: the diff engine is shared, the passes already exist, and the work is routing the second producer through one set of passes and reconciling their order.

The expected cost is snapshot churn: applying *all* passes to *both* paths changes output in edge cases where one path previously skipped a pass. That churn is the win made visible — both paths now normalize identically. Gate the change on the fixpoint and metamorphic invariants still holding.

**Safety net before the refactor (built 2026-05-31).** Three tiers now guard the unification, so a divergence introduced by routing both producers through one set of passes is loud rather than silent:

| Guard | Tier | What it pins | File |
|---|---|---|---|
| Curated fixpoint (`diff → merge → accept → re-diff = ∅`, **unfiltered**) | daily | merge-path lowering on one fixture per category: plain text, multi-paragraph, numbering/ordering, footnotes, equations, images, combined opaque, tables | `stemma-engine/tests/redline_fixpoint_daily.rs` |
| Cross-path equivalence, plain text | daily | EDIT vs MERGE produce the **same** segment structure (status discriminant + text + inline kinds, modulo revision identity) for a word edit | `stemma-engine/tests/cross_path_materializer.rs` |
| Cross-path equivalence, text around a preserved opaque | daily | same, for an edit beside a real `OmmlBlock`/`Drawing` opaque | same file |
| Full fixpoint sweep | nightly | every corpus fixture, including field-heavy and story inputs | `stemma-engine/tests/redline_invariants.rs` |

The cross-path tests **currently pass** — the two materializers already agree on the shapes covered, which bounds (does not eliminate) the risk M carries. Two honest gaps remain, by deliberate choice: **split field sequences** (`Begin`/`Separate`/`End`) and **rPrChange/formatting-only** changes have no cross-path case, because no in-tree fixture has the layout and a synthetic-input equivalence test mostly tests its own constructor. Those categories are covered end-to-end by the nightly fixpoint sweep instead. So the gate for landing M is: **both cross-path tests stay green, the curated daily fixpoint stays green, and the full nightly sweep stays green.**

---

## 7. Why we unify at materialization, not at the grammar

There is a tempting stronger move: make `diff` lower into a public `EditTransaction`, so there is one surface type and "an audit is just reviewing a transaction." We reject it. The reasoning is the crux of the whole design.

For `diff → EditTransaction → apply` to be **lossless**, the `EditStep` grammar must express *everything diff can discover*. It cannot today. Verified gap table (merge-produced change → edit step that expresses it):

| Merge-produced tracked change | Edit grammar | Status |
|---|---|---|
| Block insert / delete / text-modify | `InsertParagraphs` / `DeleteBlockRange` / `ReplaceParagraphText` | covered |
| Word-level inline insert/delete | `ReplaceParagraphText` → shared word-diff | covered |
| Move (paired moveFrom/moveTo) | `MoveBlockRange` | covered |
| Simple-table row/cell changes | `ReplaceTable` | covered |
| Hyperlink text / href | `ReplaceHyperlinkText` / `SetHyperlinkAttr` | covered |
| **Run formatting-only (rPrChange)** | — | **gap** |
| **Attr-granular pPrChange** (flip alignment only) | `SetBlockRangeAttr` is role-only (`edit.rs:4618`) | **partial** |
| **Surgical para split / join** (tracked para-mark ins/del) | only whole-block `ReplaceBlockRange` | **gap** |
| **Merged-cell / header-row / formatted tables** | `ReplaceTable` *rejects* (`edit.rs:5432–5448`) | **gap** |
| **tcPrChange / tblPrChange / trPrChange** | — | **gap** |
| **Opaque inline changed** (image/equation/field) | — | **gap** |
| **sectPrChange** | — | **gap** |
| **Story edits** (headers/footers/notes/comments) | every step targets `doc.blocks` only | **gap** |

Under the "lower to transaction" model, **all seven gaps become a blocking prerequisite** — diff would silently lose fidelity until the grammar is closed. Worse, it breaks a property we advertise: an `EditTransaction` is *small, durable JSON*; a whole-document diff lowered to per-paragraph steps can be **larger than the document**. Forcing discovery through the authoring vocabulary fights the grain on both counts.

So the decision:

> **Unify at the materialization layer (Invariant M). Do not force `diff` through the `EditStep` grammar.** `apply` and `diff` converge on the same materializer; `diff` produces a `Document` directly via that materializer, using the full change vocabulary. `EditTransaction` stays the small, durable, *authoring* vocabulary.

This gets the correctness win (one materializer) without the grammar-closure tax, keeps `EditTransaction` honest, and — most importantly — **decouples** the correctness refactor (ship now) from capability growth (grow on demand). Under the rejected model the two are chained; under this one they are independent.

The honest cost to state out loud: a diff-derived `Document` is not an `EditTransaction`, so you cannot replay or inspect a diff *as a transaction*. You inspect it by reading the `Document` it produced — via `read` or by extracting its tracked spans. For a downstream audit layer that wants word-level change data, the path is `diff → Document → read/extract`, which is one hop more than reading it off a transaction. That is the correct trade: discovery output is a document with changes, not a list of authored edits.

---

## 8. The authoring grammar is a roadmap, not a gate

Because diff keeps full fidelity through the materializer (§7), the gap table is reframed: it is the list of changes a *human or LLM author* cannot yet *request*, even though diff can discover and materialize them. Grow it when a real authoring use case appears — never speculatively (`CLAUDE.md`: "refactor when reality demands it").

Likely order, by authoring demand:

1. **Run formatting (`SetRunAttr` → rPrChange)** — "bold this defined term, tracked." The materializer already emits rPrChange on the discovery side; this lifts it to the authoring side (and adapts `reconstruct_section_segments` to carry `formatting_change`, a known consumer change).
2. **Attr-granular paragraph properties** — set one property (alignment, indent) without swapping a whole role.
3. **Merged-cell table edits** — today `ReplaceTable` fails fast (`TableHasMergedCellsNotInSpec`) rather than silently mangling; the engine's own error text points at "a future merge-aware op" (`edit.rs:758`). Good failure, real gap.
4. **Surgical split/join, opaque inline edits, sectPr, story edits** — rarer as *authored* intent; defer until asked.

Each is a self-contained addition to one grammar feeding one materializer. None is load-bearing for v1.

---

## 9. Public surface, grounded in the model

The durability tiers decide visibility. Durable things are the vocabulary; ephemeral things are opaque.

**Durable, public, semver-stable** (mark `#[non_exhaustive]`):
- `Docx` — the bytes. The only authoritative artifact.
- `EditTransaction` — the authoring intent. Small, serializable, replayable.
- `ValidationReport`, `ApplyReport` — stable rule IDs, what changed.

**The handle** (opaque):
- `Document` — wraps the internal `EditSnapshot` (`CanonDoc` + package scaffold). Exposes the verbs. Internals are not API.

**The read projection** (its own stability contract):
- `DocumentView` — designed query surface; not a `CanonDoc` dump.

**Internal — stays `pub(crate)`:**
- `CanonDoc` and all of `domain` — the IR. It is engine-version-bound *by design*; that is the whole reason it is not the API. Exposing a field freezes it. Composability lives in `DocumentView` + `EditTransaction` + the verbs, not in IR access. If a real "I need the raw IR" use case ever lands, add it behind an explicit `unstable_ir` feature with a no-semver warning — not before.
- The change vocabulary (`InlineChange`, `DiffChange`) — the materializer's input contract (§6). Internal seam, not public type.
- `edit_v4` — the wire adapter. One public schema; v3 stays an internal deprecated input adapter.

This is the contradiction the earlier brief carried (durability table said `CanonDoc` is ephemeral; an old draft proposed exposing `domain`). Resolved here: **the durability contract is the API contract.**

---

## 10. Crate boundaries

The model dictates the split. The seam is the dependency line, not just the concept.

- **`stemma` (core).** The `Document`, the verbs, the *one materializer*, the shared word-diff (`tokenize` + Patience), `merge` materialization, `redline_extract`, validation, and the change-vocabulary types. Tiny dep surface (zip, xml, serde). This is what someone audits or compiles to WASM.
- **`stemma-diff` (optional).** The *alignment* — block pairing, the move-detection heuristics, table-row alignment, the tokenizer-policy choices. This is a pure **intent producer**: it discovers deltas and feeds them to core's materializer. It depends on `stemma`; it does **not** re-implement materialization.
- **`stemma-runtime` (optional).** `SimpleRuntime`, the handle store, TTL eviction, the zstd+bincode snapshot blob. Session and transport concerns — kept out of the crate someone audits.

The cut that matters: **the word-diff and the materializer stay in core, because `apply` needs them.** Move the alignment *intelligence* to `stemma-diff`, but if you try to move the materialization primitive out, you will duplicate it back in — the exact bug §6 exists to kill. `stemma-diff` calls into core; never the reverse for materialization.

`redline_extract` stays in core: it is not diffing, it is a special-purpose parser ("what tracked changes does this single document already contain"), a basic capability a consumer expects on `Document`.

---

## 11. Named invariants (enforce once, preserve through every step)

- **M — one normalization pass set.** §6. Both delta producers run the same three normalization passes in the same fixed order (field-coalesce → opaque-reorder → compact), so a delta normalizes to the same valid OOXML no matter who produced it. Converged 2026-05-31; the segment builders remain two functions (deeper unification deferred).
- **Field-character balance.** The four structural pieces of a field (`Begin`/`Instruction`/`Separate`/`End`) share one tracking status. `coalesce_split_field_sequences` (`tracked_model.rs:373`).
- **Move pairing.** A moved block's deletion and insertion share one `move_id`; serialized as paired `w:moveFrom`/`w:moveTo`.
- **Opaque preservation.** Any edit that would drop a preserved opaque/hard-break anchor fails with `OpaqueDestroyed` listing every missing anchor — never a silent drop.
- **Non-empty tables.** Accept/reject that empties a table removes it (ECMA-376 §17.4.37).
- **Direct vs inherited formatting.** Properties set on the element are tracked distinctly from style-inherited ones, so the serializer never emits inherited values as direct.
- **Tri-state marks.** `Inherit ≠ Off`. "Absent" and "explicitly disabled" are different states and stay different.
- **Reject-all = baseline, accept-all = target.** The two projections of the core object must reconstruct exactly (`redline_extract`'s reconstruction invariants).
- **Proven-valid output.** Serialized bytes pass the post-serialization validator (and optional Oracle) or they do not leave the engine.

Each is named here once; preserve it through every transformation, fail fast with context if it breaks.

---

## 12. The model in one breath

A stemma `Document` is **content plus attributed deltas** — the same thing a tracked-changes DOCX is. You **author** deltas with `apply(EditTransaction)` or **discover** them with `diff(a, b)`; either way they flow through **one materializer** into valid tracked OOXML, and nothing leaves the engine **unproven**. The IR stays private so it can keep improving; the vocabulary you hold — bytes, transactions, the document handle, the read view — stays stable. Diff is an intent producer sitting on the core, not part of it. Everything else is detail.
