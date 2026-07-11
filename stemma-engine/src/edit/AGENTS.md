# Adding an authoring verb (parallel-agent guide)

This module is the **authoring grammar**: the `EditStep`s a human or LLM can
*request*. Growing it is how we add breadth. This file is the recipe so many
agents can add verbs **in parallel without colliding**.

Read `stemma-engine/docs/domain-model.md` first — especially §4 (the verbs), §6+§11
(Invariant M, the one materializer), and §8 (the grammar is a roadmap, not a
gate). The §8 priority order is the work queue: run-formatting → attr-granular
pPr → merged-cell tables → split/join → story edits.

## The one rule that keeps parallel work safe

**You add grammar and an adapter. You never touch the materializer.**

There is exactly one routine that lowers deltas into `TrackedSegment`s
(Invariant M). Its passes — `coalesce_split_field_sequences` →
`normalize_paragraph_opaque_reading_order` → `normalize_segments` — and the
shared word-diff (`crate::diff`) are owned centrally. A verb produces the
*input* to that machinery (changed content / change-vocabulary) and calls it.
If your verb needs the materializer to behave differently, stop and raise it —
that is a cross-cutting change, not a verb.

Corollaries (from domain-model.md §11): preserve opaque anchors or fail with
`OpaqueDestroyed` (never silently drop); no silent fallbacks — add an explicit
`EditError` variant instead of best-effort; reject-all = baseline,
accept-all = target must still hold.

## Where your code goes

Put the **bulk** of your verb in its own file so it is yours alone:

- `edit/verbs/<verb>.rs` — your `validate_*` and `apply_*` functions. **New
  file, no merge conflicts.** (Existing verbs' logic still lives inline in
  `edit/mod.rs`; extract the one you are adjacent to as you go — incrementally,
  carried by your feature, per CLAUDE.md "refactor when reality demands it".
  Do not pre-split files for verbs nobody is writing.)

The **shared seams** you touch are small, one-line, and marked. Append at the
sentinel so merges stay trivial:

| File | What you add | Seam |
|---|---|---|
| `edit/mod.rs` | one `EditStep` variant | `// ─── add new authoring verbs above ───` |
| `edit/mod.rs` | one `apply_transaction` arm that calls `verbs::<verb>::apply(...)` | end of the `match step` |
| `edit/mod.rs` | `EditError` variants for your failure modes | the `EditError` enum + its `Display` arm |
| `edit_v4.rs` | one `Op` variant + `translate_op` arm + `validate_schema` case | `// ─── add new wire ops above ───` |
| `runtime.rs` | map your `EditError`s to an `ErrorCode` | the `apply_edit` error match |
| `domain.rs` | new IR shape, only if the change cannot be expressed today | n/a |
| `serialize.rs` | emit the new construct, only if it adds OOXML | n/a |

`domain.rs` and `serialize.rs` are the high-overlap files. If your verb needs
them (tables, story edits), split the relevant cluster into a submodule
(`domain/<area>.rs`, `serialize/<area>.rs`) **as part of your feature**, the same
way `edit/markup.rs` was carved out — that keeps the next agent off your lines.
Verbs that only touch the grammar (run-formatting, attr-granular pPr) avoid
both files entirely; do those first.

## Tests (your verb lands green on its own)

- Unit tests inline in `edit/verbs/<verb>.rs`.
- Integration: `stemma-engine/tests/<verb>_*.rs` (per-feature files — no conflict);
  gate with `just -f stemma-engine/Justfile gate`.
  Daily-tier `spec_*` for the OOXML constraint you implement.
- Conformance: a verb is done when accept-all → intended, reject-all →
  original, output validates, and opaques are preserved. Encode these as
  hermetic structural checks over an in-memory witness. (A held-out
  harness additionally judges output against Microsoft Word; it is not part of
  this repo, so the daily gate must stand on its own.)
- **Standing rule — ratchet the daily gate:** every conformance catch gets a
  sentinel fixture in `stemma-engine/tests/spec_sentinel_invariants.rs` **plus** a
  hermetic structural check (accept/reject/fixpoint/identity/validator-clean
  over the in-memory witness), so the class can never silently regress on the
  corpus-free daily tier again. Adding the fixture is part of fixing the bug,
  not a follow-up.

## Parallel workflow

One worktree per verb, branched off the agreed foundation commit. Land green on
your own tests before merge. Because the body is in your own `verbs/<verb>.rs`
and the central edits are one-liners at marked seams, integration is a
near-trivial merge. Do not edit the materializer or another verb's file.

## Recipe: adding a formatting verb (the long-tail template)

The formatting IR, serializer, and accept/reject are ALREADY built and wired for
paragraph / cell / row / table properties (borders, shading, widths, vAlign,
margins, row height, …). Authoring is the only gap. `verbs::cell_formatting`
(`SetCellFormatting`, a tracked `w:tcPrChange`) is the **exemplar**; mirror it.

A formatting verb is **in-place**: it sets only the requested properties on ONE
node and records the node's prior properties in the EXISTING tracked envelope
(`CellFormattingChange` / `ParagraphFormattingChange` / `RowFormattingChange` /
`TableFormattingChange`). It is a property delta, **not** a segment ins/del, so
it touches **none** of: the materializer (Invariant M), the serializer, or
accept/reject — those already restore `previous_*` on reject and clear the
change on accept. Do **not** re-invent an envelope or emit anything new.

Per-verb footprint (everything else is reused):

1. **One patch struct** next to `CellFormattingPatch` (`edit/mod.rs`): the fields
   the caller may set, each `Option`. Scope it to exactly the properties the
   accept/reject projection restores (e.g. cell reject restores width / borders /
   shading / vAlign / margins) — authoring a field reject won't revert is a lie.
   Add `is_empty()`.
2. **One snapshot helper** next to `snapshot_cell_formatting` (`edit/mod.rs`):
   `snapshot_<node>_formatting(node, &rev) -> <Node>FormattingChange`, mirroring
   the field mapping the classifier uses in `tracked_model.rs` so an authored
   change is byte-identical to one Word produced. Call it BEFORE mutating.
3. **One verb file** `edit/verbs/<verb>.rs` mirroring `cell_formatting.rs`: no-op
   short-circuit → stacking guard (`*.formatting_change.is_some()` → refuse) →
   snapshot → apply only requested fields → `TrackedChange` sets the change /
   `Direct` clears it. Register `pub(crate) mod <verb>;` in `verbs/mod.rs`.
4. **~4 seam lines**: one `EditStep` variant + one dispatch arm (`edit/mod.rs`),
   one `EditError::No<Node>FormattingRequested` (+ `Display` + `runtime.rs`
   `UnsupportedEdit` group), one wire `Op` + `translate_op` arm + `validate_schema`
   case (`edit_v4.rs`, parsing borders/shading/width/vAlign/margins at the edge,
   fail-loud on a bad token), one MCP op-description line.

**In-place cell/row ops edit ONE node; they never rebuild the whole table.** An
in-place property edit byte-preserves `tblPr`, every `trPr`, and all other cells,
so there is nothing to lose — cite the `apply_set_cell_text_in_place` precedent
in a comment. (RFC-0003 DELETED the old blanket `validate_base_table_v4_compatible`
refusal — `replace(table)` now carries base formatting via
`carry_base_formatting_onto_target` rather than dropping it — so there is no
blanket table guard to bypass. Structural ops still call the narrow
`validate_table_not_mid_redline`; in-place property edits don't need even that.)
Address cells by LOGICAL `{row_index, col_index}` (after `gridBefore`, advancing
by each cell's `gridSpan`), the same address the read view mints.

Recommended remaining fan-out (each carries its OWN patch struct + snapshot
helper — don't pre-build them):
- **`SetRowHeight`** via a `RowFormattingPatch { height, height_rule }` +
  `snapshot_row_formatting` → `w:trPrChange`.
- **`SetTableBorders` / `SetTableShading` / `SetTableWidth`** via a
  `TableFormattingPatch { borders, width, default_cell_margins, … }` +
  `snapshot_table_formatting` → `w:tblPrChange` (a whole-table property edit, but
  still in-place: it rewrites only `tblPr`, never the rows/cells, so it likewise
  bypasses the v4 replace guard).

**Descoped:** merged-grid *column* formatting ops (set a property down a logical
column) need a formatting-aware table diff to map logical columns onto physical
cells across spans — out of scope until that exists.

Invariants to test daily (see `stemma-engine/tests/cell_formatting.rs`): accept-all ==
node with the requested formatting; reject-all == original (the `*PrChange`
reverts); validator-clean on both; the change is a tracked `*PrChange`, NOT a
segment ins/del; tblPr + every other node + the target's untouched properties
byte-identical; stacking guard refuses a second change; no-op refused; opaque
preservation. Queue the case for a real-Word gold pass in the held-out conformance tier (the
real-Word tier lives outside this crate — see `docs/testing_strategy.md`).
