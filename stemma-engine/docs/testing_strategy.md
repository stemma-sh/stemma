# Stemma Engine Testing Strategy

This covers the invariants that live in the `stemma` crate's own test home
(`stemma-engine/tests/`) and run under `cargo test -p stemma` **without building
the transport crates** (stemma-mcp / stemma-api).

The engine is a pipeline:

```
DOCX bytes -> import -> CanonDoc -> edit / diff -> Transaction -> apply -> CanonDoc -> serialize -> DOCX bytes
```

Each stage has invariants. The engine tests prove the ones that depend only on
stemma symbols (`api`, `domain`, `edit`, `edit_v4`, `view`, `docx`, and the
`runtime` re-exports `accept_all` / `reject_all` / `SimpleRuntime` /
`DocxRuntime` / `ExportMode` / `ExportOptions` / `Resolution`).

Run suites via `just -f stemma-engine/Justfile <recipe>`.

---

## Philosophy: Word is the oracle for consumption, not production

Word is authoritative about how it **reads** our markup. If Word accepts all
changes in our redline and gets different text or formatting than intended, we
have a bug. Word's interpretation of OOXML is the ground truth because that's
what the lawyer will open.

But Word is **not** authoritative about what the "correct" diff is. Word's
`CompareDocuments` is just another diff algorithm with its own quirks. A case
where stemma produces a finer-grained, more readable redline than Word is not a
bug — it's a feature.

The daily engine gate uses stemma's own `accept_all` / `reject_all`, so it
cannot catch a bug where stemma's redline markup and stemma's accept/reject are
*both* wrong in the same way. The real-Word conformance tier breaks that
circularity by asking real Microsoft Word to validate and accept/reject the
redline. That tier is **not part of this crate**: it is held out — it drives a real
Word instance, ships with nothing here, and does not run on a public clone.

---

## Three tiers of fidelity

Every correctness property falls into one of three tiers. The per-verb fidelity
gate (`edit_fidelity_invariants.rs`) and the edit-pipeline parity gate
(`edit_invariants.rs`) prove the engine half of each tier in canonical space.

### Tier 1: Text identity (hard gate)

`accept_all(redline(A, B)) == text(B)` and `reject_all(redline(A, B)) == text(A)`.

If accepting all changes doesn't produce the text of B, the redline is broken.
No judgment calls. Engine mirror: invariants #6 / #14 applied to the edit path.

### Tier 2: Structural formatting identity (hard gate)

Style name, numbering / list markers, bold / italic on headings, font size, and
opaque (hyperlink) survival must be preserved through accept. A heading that
becomes Normal, or a dropped bullet marker, is a bug a lawyer would notice.
Engine mirror: the structural subset of invariant #18, checked in canonical
space.

### Tier 3: Presentation quality (benchmark, not gated)

How the tracked-change markup *looks* before accept/reject — granularity of
ins/del spans, formatting-only changes surfaced or suppressed. A difference here
might mean we're *better*, not wrong. Tracked, not gated.

---

## Engine invariants (the ones that live here)

These are the invariants whose tests moved into `stemma-engine/tests/`. The full
catalog — all 22 numbered invariants, including the diff/redline/normalize and
Word-oracle tiers — is in [`invariants.md`](invariants.md), which is the
numbering source of truth (inherited from the catalog of the engine's original
host application so cross-references stay stable). Below is the hard-gate subset most
relevant to day-to-day engine work.

### #6 / #14 — Redline accept/reject [Tier 1, hard gate]

`reject_all(apply_edit(A, e)) == text(A)` and
`accept_all(apply_edit(A, e)) == text(A ⊕ e)`. Accepting all changes yields the
target; rejecting all yields the base. Proven on the edit construction path in
`edit_invariants.rs` (`edit_tracked_accept_matches_new_text`,
`edit_tracked_reject_matches_original_text`).

### #7 / #12 — Fixpoint + roundtrip fidelity [hard gate]

Edit fixpoint: `apply → serialize → reparse → accept` equals
`apply → accept` in canonical space — the serializer round-trip introduces no
drift (`edit_serialize_reparse_accept_equals_canonical_accept`).

### #13 (hermetic stand-in) — validator clean sweep [hard gate, daily]

`spec_validator_clean_sweep.rs` sweeps **every** checked-in `testdata/**/*.docx`
through `serialize(parse(A))` and asserts the post-serialization linker
(`docx_validate::validate_docx`, gated at `ValidatorLevel::Blocking` →
`BLOCKING_RULES`) reports zero blocking findings. This is the corpus-free
stand-in for the nightly Word-oracle #13 ("opens clean in Word"): it mirrors how
`roundtrip_fidelity.rs` (#12) sweeps the same set, but checks a *different*
invariant — the bytes we emit are structurally clean by the rules Word would
otherwise repair, not just IR-stable. It also makes the whole validator
(ordering, annotation, xref, namespace checks) execute daily over real
structures, so a structural regression cannot land green unseen.

Blocking classes guarded hermetically here: `I-TC-001/002` (tracked-change
content model + id), `I-DOC-001/002` (document/body shape), `I-PKG-001/002`
(package parts), and **`I-CT-002`** (every WordprocessingML part carries its
canonical content type per ECMA-376 Part 1 §15.2 — Word locates comments /
footnotes / styles / numbering *by content type*, so a part covered only by the
generic `xml` Default is dropped on repair). Element **ordering** (`I-ORD-*`)
runs daily through this sweep but stays **non-blocking (Warning)**: opaque
preservation faithfully round-trips source orderings that Word itself tolerates,
so a blocking ordering rule would falsely reject valid round-trips. Accept/reject
identity over real Word semantics (#14/#14b) remains oracle-only.

`spec_content_type_canonical.rs` is the unit sentinel behind `I-CT-002`: a
comments part with no Override is flagged Error, a canonical Override is clean,
and the engine *repairs* the input class (emits the canonical Override on
serialize via `DocxPackage::ensure_canonical_wml_content_types`).

### #10 — Identity [hard gate]

An identity edit emits zero tracked spans (no phantom `w:ins` / `w:del`):
`edit_identity_replacement_produces_no_tracked_spans`. Mirror of `diff(A, A)`
being empty.

### #20 — Edit engine I1–I9 [hard gate]

The edit engine produces the same `TrackedSegment` model as `merge_diff`. Its
own invariants (documented in `stemma-engine/docs/` and `edit/AGENTS.md`):

- **I1**: every `OpaqueInlineNode` / `HardBreakNode` survives editing exactly.
- **I2**: `accept_all(edited)` produces the replacement text.
- **I3**: `reject_all(edited)` produces the original text.
- **I4**: unchanged text retains original marks and `style_props`.
- **I5**: the edited paragraph keeps its original `block_id`.
- **I6**: `serialize(edited)` produces valid DOCX (accept/reject correct).
- **I7**: no empty `TrackedSegment`s in output.
- **I8**: adjacent segments with identical `TrackingStatus` are merged.
- **I9**: identical replacement content is a no-op — no tracked changes emitted.

These plus the per-verb fidelity gate live in `edit_fidelity_invariants.rs`.

### Per-verb fidelity gate [hard gate]

For every authoring verb, the "done" criterion is:

1. **Reversibility**: `reject_all(apply_edit(A, e)) == text(A)`.
2. **accept == direct**: `accept_all(tracked materialization) == direct materialization`.
3. **Non-shrinking opaque inventory**: the set of `OpaqueInlineNode` IDs after
   the edit is a superset of (or equal to) the set before — a verb may never
   silently destroy an opaque anchor (it must fail with `OpaqueDestroyed`).

A verb is **done** when all three hold for its representative edits in
`edit_fidelity_invariants.rs`, and (nightly) the Word oracle agrees in the
held-out conformance tier. Every new verb adds a case to both.

### #20e — Edit engine: table replace [hard gate, daily]

`EditStep::ReplaceTable` (the v4 `replace(table)` op) routed through the same
diff machinery as `merge_diff`. Engine layers pinned in `stemma-engine/tests/`:

- Engine apply (canonical): row insert / delete / matched-row cell change
  produce the right `TrackingStatus` markers — `spec_edit_table_tracked_changes.rs`.
- Accept/reject identity (T1): `accept_all == target`, `reject_all == base` for
  row insert, row delete, cell text change, nested table change —
  `redline_table_replace.rs`.
- Identity edit no-op (I9): identity replace emits zero tracked changes —
  `spec_edit_table_tracked_changes.rs`.

(The wire **adapter** layer — schema rejection + `translate_op` routing + engine
fail-fast — is tested with the consuming application's wire layer, since it
exercises the wire surface, not the engine.)

### #13 / #14 / #14b + verb conformance — real-Word conformance [nightly, hard gate]

> **Held-out tier — not in this repo.** These cases drive a real Microsoft
> Word instance through an external automation service. No such service ships
> with the engine, so this tier does not run on a public clone — it is how the
> engine is verified against real Word, not a capability of the published
> library.

- **#13** `serialize(parse(A))` opens clean in Word.
- **#14** Word's accept/reject of an edited DOCX matches our canonical
  accept/reject.
- **#14b** `word_clean(A) ∧ word_clean(B) → word_clean(redline(A, B))` — the
  non-negotiable "opens clean" gate, applied to the edit path.

The tier's coverage spans the edit path (open-clean and accept/reject over
edited documents; per-verb conformance across all authoring verbs; the table
specialization) and the document path (the full diff/redline + production
paths):

- **#13** — `serialize(parse(A))` opens clean, all fixtures.
- **#14 / #14b / #15** — redline accept/reject, opens-clean, and
  Word-comparison over fixture pairs. (Volatile fields — `PAGE`/`DATE`/`TIME`
  family — are projected as the opaque field barrier `\u{FFFC}` on both sides, since
  Word recomputes them on open and their rendered text is not a stable target.)
- **#18** — Tier-2 formatting parity after accept.
- **#21** — normalize/reject output + sample redlines open clean.
- **#17** — mutation → redline → Word validate/accept/reject.

### Sentinel corpus — relational invariants over real structures [daily, hard gate]

`spec_sentinel_invariants.rs` is the **hermetic stand-in** for the corpus/Word
tiers. The corpus sweeps (`roundtrip_fidelity.rs`, `redline_fixpoint_daily.rs`,
`identity_invariant.rs`) skip gracefully when the DOCX corpus is absent, so in
the corpus-free daily gate they check **zero real structures** — a whole
structural class could regress while the gate stayed green. The sentinel corpus
closes that blind spot: every witness is a small **in-memory** `(before, after)`
DOCX pair (the `pack(document_xml)` idiom — no corpus dependency), so the
relational invariants run on every `cargo test -p stemma`.

Witnesses, one+ per structural class that previously only the corpus/Word tiers
touched:

- **move** across a paragraph boundary and across a table-cell boundary;
- **pre-existing ins/del** spanning a paragraph mark (§17.13.5.15 merge rule)
  and a table-cell boundary;
- **hyperlink / field / SDT** opaque adjacent to a changed run (opaque survives
  accept);
- **footnote-body** edit, **header** redline (non-body stories);
- **nested-table** cell change;
- the **Tier-2 formatting bundle** (heading style + numbering + bold).

Each witness asserts, in canonical space: #6 (`accept_all(merge(A,B))==text(B)`,
`reject_all==text(A)`), #7 (fixpoint), #10 (`diff(A,A)`/`diff(B,B)` empty,
`redline(A,A)` no markup), the structural subset of #18 (style_id /
heading_level / num_id / ilvl / Bold / opaque survive accept on the **diff**
path — the catalog otherwise covers Tier-2 only via Word), and #13's hermetic
proxy (`serialize(redline)` is `validate_docx`-clean).

**Standing rule — ratchet the daily gate toward Word:** every Word-oracle catch
gets a sentinel fixture here **and** a hermetic structural check, so the class
that Word caught can never silently regress on the daily tier again. The Word
oracle stays the consumption authority; the sentinel corpus is how its findings
become daily, corpus-free regression guards. Adding the fixture is part of
fixing the bug, not a follow-up.

---

## Out of scope here

Integration tiers for downstream applications (transport servers, frontends,
application-level corpus sweeps) live with those applications, not in the
engine test home.

---

## Test import rule

**`stemma-engine/tests/` may import ONLY stemma symbols.** If a test needs a
non-stemma symbol, it does **not** belong in the engine test home; lift the
symbol into stemma first. No shims, no re-export hacks.

---

## Environment variables and graceful skip

The daily gate is **corpus-free and oracle-free**. With
`STEMMA_CORPUS_ROOT` and `STRESS_CORPUS_DIR` **both
unset**, `just -f stemma-engine/Justfile gate` must still exit 0:

- `STEMMA_CORPUS_ROOT` — corpus root for worktrees. The bulk DOCX corpus is
  not distributed with the repo. Resolved by `tests/common/mod.rs::corpus_root()`; falls
  back to the repo root relative to `CARGO_MANIFEST_DIR`. Corpus-dependent tests
  **skip gracefully** (print `SKIP:`) when fixtures are absent — they never fail.
- `STRESS_CORPUS_DIR` — external bulk corpus (~83k files). Stress tiers skip
  when unset.

Invariant: **both unset ⇒ the daily gate is still green; the corpus and
oracle tiers skip, never fail.**

---

## Gate command map

| Recipe | Scope | Tier |
|---|---|---|
| `gate` | `clippy test` — the merge gate other streams target | daily, corpus-free, no real-Word oracle |
| `clippy` | `cargo clippy -p stemma --all-targets --all-features -- -D warnings` | static |
| `test` | `cargo test -p stemma` | daily |
| `unit` | `cargo test -p stemma --lib` | daily |
| `fidelity` | `--test edit_fidelity_invariants` (per-verb gate, I1–I9) | daily |
| `edit-invariants` | `--test edit_invariants` (Tier 1/2 parity) | daily |
| `edit-table-unit` | `--test redline_table_replace --test spec_edit_table_tracked_changes` | daily |
| `gate-confidence` | `gate` then `cargo test -p stemma -- --ignored` (host-only, skips when corpus unset) | confidence |
| `fuzz` | heavy 20k-transaction fidelity sweep (release) | nightly, host |
| `nightly` | `gate-confidence` then `fuzz` (host-side full nightly, no real-Word oracle) | nightly, host |

The merge gate other streams target is `just -f stemma-engine/Justfile gate`.

### Real-Word conformance (held out)

The Word-oracle tier lives outside `stemma`, so the engine ships no
Word-automation code or `reqwest` dependency; it does not run on a public
clone.
