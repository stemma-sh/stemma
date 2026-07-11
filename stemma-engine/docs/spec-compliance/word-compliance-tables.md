# Word-compliance — Tables (§17.4)

**Summary:** No confirmed incompliances · 7 regression tests · 3 test-bugs discarded.
Tests live in `stemma-engine/tests/spec_tables_word_compliance.rs`
(`cargo test -p stemma --test spec_tables_word_compliance` → 7 passed; 0 failed).

## Confirmed incompliances

None. No assertion here traced to a stemma pipeline- or model-bug. The serializer
emits correct `tcPr`/`trPr` child ordering, preserves `cnfStyle`/`hideMark`/`cantSplit`, and
resolves `vMerge` defaults (bare/absent `val` == continue) the way Word does.

## Regression tests (passing)

- `vmerge_absent_val_defaults_to_continue` — absent `w:vMerge` val == continue; a bare continuation
  below a restart opens clean and the merged region reads exactly the anchor text (§17.18.57, §17.4.84).
- `vmerge_bare_no_val_is_continue` — the merge anchor keeps `val="restart"` and the continuation cell
  round-trips as a bare `<w:vMerge/>` == continue (§17.4.84, §17.18.57).
- `tcpr_child_order_tcw_gridspan_vmerge_opens_clean` — re-serialized `tcPr` preserves schema sequence
  `tcW → gridSpan → vMerge` (§17.4.70 / Annex A CT_TcPrInner).
- `tc_cnfstyle_direct_membership_roundtrips` — `cnfStyle` is preserved on roundtrip with its `firstRow`
  membership bit intact (§17.4.7, §17.3.1.8).
- `tbl_header_explicit_off_is_not_header` — `tblHeader w:val="0"` (off) is never re-emitted as an enabled
  bare/open `tblHeader` (§17.4.49, §17.17.4).
- `trpr_cantsplit_tblheader_order_opens_clean` — `cantSplit + trHeight + tblHeader` in CT_TrPrBase order
  opens clean and `cantSplit` survives roundtrip (§17.4.6, §17.4.49).
- `tcpr_hidemark_preserved_and_accepted` — `hideMark` after `vAlign` in CT_TcPr order opens clean and is
  preserved across roundtrip (§17.4.21, §17.4.70).

## Discarded test-bugs (methodology lesson)

Three mined specs asserted that stemma should **strip/normalize markup on save**. They were deleted
because they conflate Word's **consumption/render** behavior with **save-rewriting**:

- `valign_both_is_dropped_not_top_center_bottom` — asserted `w:val="both"` is dropped on save.
- `tbl_header_explicit_false_not_header` — asserted `<w:tblHeader w:val="false"/>` is removed on save.
- `tblcellspacing_type_pct_treated_as_zero_width` — asserted `tblCellSpacing type="pct"` is rewritten to `w="0"`.

The cited text (ECMA §17.18.101, §17.4.49; MS-OI29500 §2.1.154, §2.1.181) says Word **ignores these on
layout** — it does not say Word physically rewrites the markup on save. stemma deliberately round-trips
**untouched** content byte-verbatim (passthrough archive write, `serialize_snapshot` in
`src/runtime.rs`). A re-serialized *unedited* document therefore reflects the verbatim-preservation
contract, not Word-normalization. stemma's **model/edit path** already treats all three as off/zero
(the consumption semantics), so there is no incompliance — preserving the original bytes is correct and
arguably superior (lossless, opens clean in Word).

**Rule for future audits:** an `xmlContains`/`xmlOmits` assertion on `reserialize()` of an *unedited*
doc is only valid for markup stemma's serializer *actively generates* (child ordering, structural
synthesis) — never for "Word ignores/normalizes X." To test the latter, assert **consumption semantics**
via read views (`read_accepted`/`read_rejected`/model), or the **typed edit path** on an actually-edited
cell, or confirm it against real Word.

## Open questions — pending confirmation against real Word

Empty. No case here requires confirmation against real Word: the consumption semantics are confirmed by stemma's
model, and save-verbatim preservation is a deliberate, valid contract — not a divergence to confirm.
