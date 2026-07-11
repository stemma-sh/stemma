# Invariant Catalog

The master list of correctness invariants for the stemma engine. The engine is a
pipeline:

```
DOCX bytes -> import -> CanonDoc -> diff / edit -> Transaction -> apply -> CanonDoc -> serialize -> DOCX bytes
```

Each stage has invariants; the tests are organized to prove them. This catalog is
the **numbering source of truth** — `testing_strategy.md` documents the tiers and
the day-to-day gate, and references invariant numbers defined here. (Numbering is
inherited from the catalog of the engine's original host application so historical
cross-references stay stable; gaps and `[retired]` entries are kept on purpose.)

**Scope.** This crate owns the canonical-space invariants (import, diff, edit,
merge, serialize). Two tiers of invariant are proven *outside* this crate and are
listed here only for completeness:

- **Word-oracle invariants** (#13, #13b, #14, #14b, #15, #17, #21, and the
  `*-word-oracle` halves of #20) drive a real Microsoft Word instance. They live
  in a held-out real-Word conformance harness, not here. The OSS engine ships
  no Word-automation code. See `testing_strategy.md`.
- **App/runtime invariants** (#22's runtime gates, the representative-corpus
  *manifests*) belong to the consuming application, not the engine.

Run engine suites via `just -f stemma-engine/Justfile <recipe>`.

---

## Philosophy: Word is the oracle for consumption, not production

Word is authoritative about how it **reads** our markup. If Word accepts all
changes in our redline and gets different text or formatting than intended, we
have a bug — Word's interpretation of OOXML is ground truth because that's what
the user will open.

But Word is **not** authoritative about what the "correct" diff is. Word's
`CompareDocuments` is just another diff algorithm with its own quirks. A case
where stemma produces a finer-grained, more readable redline than Word is a
feature, not a bug.

### Word comparison is asymmetric about accept vs reject

Word's `CompareDocuments` is faithful about the "after" state but not always the
"before" state, so `accept_all(word_compare(A, B)) == text(B)` holds reliably but
`reject_all(word_compare(A, B)) == text(A)` does **not** always hold. This means
**our own redline invariant `reject_all(redline(A, B)) == text(A)` is a harder
guarantee than what Word achieves** — we gate on it (Tier 1, #6/#7/#14). When
testing `reject_all` on *externally*-produced tracked-change documents (the
normalize/accept production path, #19), the text-fidelity check is best-effort:
failures may indicate Word-comparison imperfection rather than a bug in us.

## Three tiers of fidelity

### Tier 1: Text identity (hard gate)

`accept_all(redline(A, B)) == text(B)` and `reject_all(redline(A, B)) == text(A)`.
If accepting all changes doesn't produce the text of B, the redline is broken. No
judgment calls. **Invariants: #6, #14.**

### Tier 2: Formatting identity (hard gate)

`formatted(accept(redline(A, B))) == formatted(B)` for structural properties. If a
user accepts all changes and the result lost bold headings, dropped numbering, or
changed font sizes, that's a bug. The test: *would a user opening the accepted
document in Word notice something is off?* Structural (gated): style name,
numbering/list markers, bold/italic on headings, font size, strikethrough leaking.
Cosmetic (tracked, not gated): indent/spacing drift, alignment, hyperlink
styling, body font/color. **Invariant: #18.**

### Tier 3: Redline presentation quality (quality metric)

How the markup *looks* before accept/reject — ins/del granularity, move
detection, format-only change surfacing. A difference here may mean we're
*better*, not wrong. Track, don't gate. **Invariants: #8, #15.**

## Non-negotiable: redline output opens clean in Word

```
word_clean(A) ^ word_clean(B) -> word_clean(redline(A, B))
```

No exceptions, no budget, no "report only." If two clean documents produce a
redline that triggers Word's "unreadable content" repair dialog, we've lost the
user's trust permanently. **Invariant: #14b** (Word-oracle harness).

---

## Invariants

### 1. Parse totality: `import(A)` succeeds for all valid DOCX [hard gate]

Any well-formed DOCX must import without error. We don't silently drop content or
fall back to empty defaults — if we can't parse it, we fail with context.

| Property | Test |
|---|---|
| `import(A)` succeeds across the corpus | `corpus_parse_totality.rs`, `stress_manifest.rs` |
| Headers/footers/footnotes/endnotes import | `stories`-style fixtures under `testdata/` |

### 2. Roundtrip identity [retired]

Byte-identity / LibreOffice visual-render roundtrip was retired: we target Word
not LibreOffice, and byte-identity is not the bar (render fidelity is — a theme
font legitimately differs from its baked literal). The active roundtrip
guarantees are **#12** (structural canonical roundtrip) and **#13** (Word
open-clean of serialized output).

### 3. Diff reconstruction: `inline_changes` reconstruct `old_text` / `new_text`

`BlockModified { old_text, new_text, inline_changes }` must satisfy
`concat(Unchanged+Deleted) == old_text` and `concat(Unchanged+Inserted) == new_text`.
Cheap (no XML export); catches diff-algorithm bugs directly.

| Property | Test |
|---|---|
| Reconstruction holds over all fixture pairs | `redline_invariants.rs` |

### 4. Diff correctness: the right blocks are identified as changed

The diff must identify the right paragraphs as modified/inserted/deleted with the
right alignment.

| Property | Test |
|---|---|
| Expected changes / alignment edge cases | `diff_simple.rs` |
| Semantic diffs (jurisdiction/terminology) | `safe_us_vs_canada.rs`, `safe_us_vs_cayman.rs` |
| Table diffs (cell/structure/row alignment) | `table_diff.rs` |
| Equations in full-doc view | `equation_full_doc.rs` |

### 5. Step application preserves structure

Applying a `Transaction` must preserve formatting, respect barriers (hyperlinks,
fields, SDTs), keep surrogate pairs intact and bookmark order stable, and reject
invalid operations. Proven across the `edit_*` and `replace_text_*`-style unit
tests plus the edit-engine invariants (#20).

### 6. Redline accept/reject: `reject_all == text(A)`, `accept_all == text(B)` [Tier 1, hard gate]

The core redline invariant. We walk the exported XML and extract three text
streams per paragraph (normal runs; `<w:del>`/`<w:delText>`; `<w:ins>`), with
hyperlinks projected as opaque placeholders and list markers synthesized from
`w:numPr` + `numbering.xml`, then check `normal+deleted == text(A)` and
`normal+inserted == text(B)`.

| Property | Test |
|---|---|
| Accept/reject holds over fixture pairs | `redline_invariants.rs` |
| Synthesized redlines re-import; markup present | `synthesized_redline.rs` |
| Edit-path mirror (`accept/reject(apply_edit(A,e))`) | `edit_invariants.rs` |

### 7. Fixpoint: `diff(accept_all(merge_diff(A, B, diff(A, B))), B) == empty`

`diff -> merge -> accept_all` faithfully transforms A into B in **canonical
space** (no serialization round-trip), catching diff/apply layers that compensate
for each other.

| Property | Test |
|---|---|
| Fixpoint holds (daily) | `redline_fixpoint_daily.rs`, `redline_invariants.rs` |
| Edit-path fixpoint | `edit_invariants.rs`, `self_edit_invariants.rs` |

### 8. Inline change granularity [Tier 3 — quality metric]

Accept/reject (#6) is satisfied by both fine-grained and coarse changes; only
fine-grained produces a useful redline. We verify adjacent Del/Ins pairs don't eat
shared text beyond token boundaries and bail-out heuristics.

| Property | Test |
|---|---|
| Common prefix/suffix preserved, principled exemptions | `redline_invariants.rs` |

### 9. Redline quality comparison [retired]

LibreOffice-compare quality benchmark, retired (not the product target; the
Word-oracle gives a stronger, product-relevant external oracle). Replaced by #15
(Word redline comparison), #14 (Word accept/reject), #18 (Word formatting).

### 10. Diff identity: `diff(A, A) == empty` and `redline(A, A)` has no markup

Importing the same bytes twice and diffing must produce zero changes, and the
redline pipeline on identical documents must emit no `<w:ins>`/`<w:del>`. Catches
normalization asymmetries and phantom-markup bugs.

| Property | Test |
|---|---|
| Zero changes / zero markup on identical docs (+ stress) | `identity_invariant.rs` |
| Edit-path identity (no phantom spans) | `edit_invariants.rs` |

### 11. Self-edit metamorphic testing

For each parseable document, apply programmatic modifications (delete paragraph,
replace word, insert paragraph) and run the canonical-space invariants (diff
reconstruction, fixpoint, accept/reject) on each (original, modified) pair —
exercising the pipeline on real-world paragraph structures.

| Property | Test |
|---|---|
| All three invariants hold (+ stress `#[ignore]`) | `self_edit_invariants.rs` |

### 12. Roundtrip fidelity: `import(serialize(import(A)))` == `import(A)` structurally

Import, serialize, re-import, and compare the two canonical representations; any
difference is information loss in the serializer.

| Property | Test |
|---|---|
| Zero structural diffs (IR₁ vs IR₂) | `roundtrip_fidelity.rs` |
| Element preservation (original vs output) | `element_fidelity.rs` |

> **KNOWN GAP (2026-06-29).** Both tests reach the serializer only via the
> *un-edited* export path, which returns the original scaffold bytes
> (`get_doc_bytes` re-zips the source package when no edit is pending) — so they
> compare an import to a re-import of the *same bytes* and cannot observe
> serializer information loss. The IR serializer (`serialize_canonical_docx`) runs
> only on the **edit path**. These tests must apply an edit before exporting to be
> meaningful; until then #12 does not actually gate serialization fidelity. The
> "state-3" element-loss class (constructs parsed-then-dropped on the edit path:
> `gridSpan`, `outlineLvl`, `cantSplit`, `w:numId=0` suppression, …) is invisible
> to the current tests. Tracked in the canonicalization-fidelity work.

### 13. Word-open-clean: `serialize(import(A))` opens clean in Word [hard gate — Word-oracle harness]

Structurally correct OOXML can still make Word flag "unreadable content." Every
round-tripped fixture is sent to the held-out real-Word oracle and must open without
the repair dialog. Catches element-ordering, attribute-edge-case, and
relationship/content-type bugs invisible to #12. *Held-out tier.*

### 13b. Word-open-clean stress validation [confidence sweep — Word-oracle harness]

#13 over the full stress corpus (parallel parse+export, then sequential
`/validate`). A regression check: if our serializer breaks a doc Word originally
opened cleanly, the sweep fails. *Held-out tier.*

### 14. Word accept/reject: `word_accept(redline(A,B)) == text(B)` [Tier 1, hard gate — Word-oracle harness]

The strongest correctness gate: Word itself performs accept/reject. If Word
disagrees with us about the accepted/rejected text, it's a real bug. *Held-out tier.*

### 14b. Redline cleanness [non-negotiable gate — Word-oracle harness]

`word_clean(A) ^ word_clean(B) -> word_clean(redline(A, B))`. Ship-stopper. Only
pairs where both inputs are Word-clean are tested. *Held-out tier.*

### 15. Word redline comparison [Tier 3 — quality metric — Word-oracle harness]

Our redline vs Word's `CompareDocuments`, as a quality metric (not a gate;
different algorithms produce different boundaries). *Held-out tier.*

### 16. Corpus feature fingerprinting

Scan parseable fixtures' raw XML (no IR parse) and extract per-document feature
flags (tables, hyperlinks, fields, SDTs, tracked changes, math, section breaks,
…) for stratified test selection — pick docs exercising specific feature
combinations rather than 2,000 similar simple docs.

| Property | Test |
|---|---|
| Feature extraction (`#[ignore]`) | `fingerprint_corpus.rs` |
| ZIP-health + full element census triage | `corpus_triage.rs` |

### 16b. Coverage-guided representative corpora

The corpus is too large for "all files every night." Maintain small minimized
subsets that preserve breadth (coverage-guided `cmin`/set-cover), with a curated
must-keep set for known failures and important edge cases. Coverage minimization
is a tool for **breadth**, not a replacement for invariant thinking; relational
invariants (diff, redline, fixpoint, fidelity, normalize) need their own
pair/tracked-change corpora, not one global minimized set.

> The representative-corpus *measurement/verification harness* and its manifests
> live in the consuming app (`measure_representative` / `verify_representative`),
> not in this engine. The engine provides the building
> blocks (#16 fingerprinting, #1 parse totality, the stress tiers).

### 17. Mutation testing: targeted edits -> redline -> Word [Word-oracle harness]

Apply structurally dangerous edits (text inside hyperlinks, formatting
boundaries, table cells, bookmarks, SDTs, existing tracked changes), generate
redlines, validate with Word (open-clean + accept/reject). *Held-out tier.*

### 18. Formatting fidelity: `formatted(accept(redline(A,B))) == formatted(B)` [Tier 2 gate + Tier 3 benchmark]

Text invariants (#6/#14) verify content; formatting (font size, bold, indent,
numbering, hyperlink styling) can be silently lost. Compare **resolved** formatting
properties (after style inheritance, numbering, theme resolution) paragraph by
paragraph. Tier-2 structural subset is a hard gate; Tier-3 cosmetic subset is a
tracked benchmark. The Word-resolved comparison lives in the held-out tier; the
engine proves the structural subset in canonical space via the edit/redline
fidelity gates (#20d Tier 2: heading style, bold-on-unchanged, numbering,
hyperlink survival through accept).

### 19. Normalize/accept production path: pre-existing tracked changes resolve cleanly

When a document arrives with pre-existing tracked changes, the pipeline accepts
them via the normalize path. Sub-invariants: **19a** zero revisions after
accept/reject (hard gate); **19b** diff pipeline succeeds with a tracked-changes
base; **19c** accept matches `after`, reject matches `before` (best-effort — Word
`CompareDocuments` may misencode the "before" state; see philosophy). Distinct
from #6: #6 is `reject_all` on *our own* output (must always hold); #19c is on
*external* input (may be unfaithful).

| Property | Test |
|---|---|
| Zero revisions after accept/reject; pipeline succeeds | `normalize.rs`, `redline_invariants.rs` |
| Row-delete/accept blindspot regression | `blindspot_normalize_row_delete_accept.rs` |

### 20. Edit engine: `apply_transaction` produces correct tracked changes [hard gate]

The edit engine applies step-based edits to a `CanonDoc`, producing
`TrackedSegment`s with the same model as `merge_diff` — so #6/#12/#13/#14/#14b/#18
apply to edited documents unchanged. Edit-specific invariants I1–I9 (documented in
`edit/AGENTS.md` / the edit-engine docs): opaque/hard-break survival, accept ==
replacement, reject == original, unchanged text keeps marks/style, block_id
preserved, valid serialize, no empty segments, adjacent-identical-status merge,
identity replace is a no-op.

| Property | Test |
|---|---|
| Unit invariants (daily) | `edit_fidelity_invariants.rs`, edit unit tests |
| Edit-pipeline parity with diff/redline tiers (#20d) | `edit_invariants.rs` |
| Table replace, 3 layers (#20e) | `redline_table_replace.rs`, `spec_edit_table_tracked_changes.rs` |
| Word-oracle for edited docs (#20c) | held-out real-Word tier |

### 20d. Edit-pipeline invariant parity [hard gate — daily]

`edit_invariants.rs` mirrors each diff/redline property on the edit construction
path — Tier 1: `accept_all(apply_edit(A,e)) == text(A⊕e)`, `reject_all == text(A)`,
edit fixpoint, identity edit emits no spans, `direct` materialization ==
`accept_all(tracked)`. Tier 2: heading style, bold-on-unchanged, numbering,
hyperlink opaque survive accept.

### 20e. Edit engine: table replace [hard gate — daily]

`replace(table)` routed through the same table-diff machinery as `merge_diff`,
pinned per layer: schema rejection, adapter routing, fail-fast on unsupported
formatting, canonical apply markers, accept/reject identity, identity no-op,
table-diff routing.

### 21. Word-open-clean of normalize/reject output and sample redlines [hard gate — Word-oracle harness]

Every DOCX the system can produce must open clean in Word; this closes the
normalize/reject-output and real-sample-redline gaps left by #13/#14b. *Held-out tier.*

### 22. source_change_id consistency: atoms match full-document segments

Every atom's `source_change_id` must appear on at least one full-document segment,
so the consuming UI can link cards to diff segments. The engine proves the
canonical-space consistency (sweep in `redline_invariants.rs`; regression in
`safe_us_vs_canada.rs`); the **runtime gates** that enforce it on every
two-doc/single-doc analysis live in the consuming app's server, not the engine.

---

## Invariant summary by tier

| Tier | Gate? | Invariants | Proves |
|---|---|---|---|
| **Non-negotiable** | Hard gate (Word) | #13, #13b, #14b, #21 | Output opens clean in Word |
| **Tier 1: Text** | Hard gate | #6, #14 | Accept/reject produces correct text |
| **Tier 2: Formatting** | Hard gate | #18 (structural), #20d (Tier 2) | Accept produces correct structural formatting |
| **Tier 3: Presentation** | Quality metric | #8, #15, #18 (cosmetic) | Redline looks good |
| **Pipeline** | Hard gate | #1, #3–#5, #7, #10–#12, #22 | Internal pipeline invariants hold |
| **Production path** | Mixed | #19a/#19b (hard), #19c (best-effort) | Pre-existing tracked changes resolve |
| **Edit engine** | Hard gate | #20a/#20b/#20d/#20e (canonical), #20c (Word) | Edit application produces correct tracked changes |

Engine-resident (run with no real-Word oracle): #1, #3–#8, #10–#12, #16, #18 (structural),
#19a/b, #20 (canonical), #22 (canonical). Held-out Word-oracle tier: #13,
#13b, #14, #14b, #15, #17, #20c, #21.

---

## Environment variables

- **`STEMMA_CORPUS_ROOT`** — corpus root for worktrees (gitignored sample/stress
  DOCX live in the main checkout). Corpus-dependent tests **skip gracefully** when
  unset, so the daily gate stays green without it.
- **`STRESS_CORPUS_DIR`** — external bulk DOCX corpus for the stress tiers; unset
  => stress tests skip.
- The held-out real-Word tier has its own service configuration; nothing in
  this repo reads it (the engine ships no Word-automation code).

See `testing_strategy.md` for the tiers, the daily gate, and run recipes.
