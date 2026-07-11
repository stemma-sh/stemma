# Word-compliance sweep — Numbering changes under tracked changes (numberingChange, ins numPr)

0 confirmed gaps, 12 new regression tests (green), 0 test-bugs.

## Confirmed incompliances

None. The sweep probed how stemma handles numbering revisions carried inside `w:numPr` — the `w:numberingChange` prior-numbering cache (`@original`) and the `w:ins`/`w:pPrChange` tracked-change children — across schema ordering (Annex A `CT_NumPr` sequence), open-clean validity in Word, and accept/reject read semantics, including suppressed numbering (`numId=0`), multi-level and edge-case `@original` forms (`%%`, structured length 31, double-percent), and `numberingChange` nested in a `LISTNUM` `fldChar` field. Every probed behavior matched ECMA-376 / ISO 29500 / MS-OI29500.

## New regression tests

- `numpr_ins_follows_numberingchange_annexa_order` — serializer emits `w:ins` after `w:numberingChange` inside `numPr` per the Annex A `CT_NumPr` sequence (ilvl, numId, numberingChange, ins); opens clean.
- `pprchange_must_be_last_child_with_prior_numpr_snapshot` — prior `numId=1` survives intact inside the `pPrChange` property snapshot, with `pPrChange` last in `CT_PPr` (§17.13.5.29).
- `pprchange_numbering_snapshot_carries_no_run_text` — `pPrChange` is a property revision with no run content, so accept and reject both leave body text `item` unchanged (§17.13.5.29, §17.9.18).
- `numpr_ins_serialized_after_numbering_change` — `numberingChange` is serialized before `ins` per the `CT_NumPr` xsd:sequence, even when `@original` carries a structured value; opens clean (§14.7.1.2).
- `numbering_change_multilevel_original_opens_clean` — the multi-level `@original` (the standard's own example) opens clean and, being a numbering cache, leaves run text `one` on both accept and reject (Part 4 §14.7.1.2).
- `ins_numbering_symbol_rpr_does_not_bleed_into_body` — a numbering level's `rPr` formats only the synthesized counter, not body runs; accepting inserted numbering yields bare run text `one` (§17.9.24, §17.13.5.19).
- `numbering_change_in_numpr_preserved_for_transitional_despite_strict_drop` — a transitional document with `numberingChange` inside `numPr` (the form Word saves) opens clean without repair (§17.13.5, Part 4 §14.7.1.2).
- `numbering_suppressed_numid0_keeps_tracked_ins_on_reject` — `numId=0` suppresses the counter and the `ins` carries no run text, so both accept and reject preserve typed run text `one`; opens clean (§17.9.18, §17.13.5.19).
- `numberingchange_original_structured_length_31_opens_clean` — an `@original` whose reduced length is within Word's 31-char limit opens clean without repair (MS-OI §2.1.1772, Part 4 §14.7.1.2).
- `numbering_change_on_listnum_fldchar_opens_clean` — `numberingChange` nested in a `fldChar` begin (the standard's LISTNUM revision shape) opens clean and leaves field result + trailing run `2.body` on accept and reject (Part 4 §14.7.1.1, Annex A CT_FldChar).
- `numbering_change_double_percent_original_opens_clean` — Word treats `%%` as a single literal `%` in the prefix; this well-formed `@original` opens clean, and rejecting leaves body text `one` (MS-OI §2.1.1772, Part 4 §14.7.1.2).
- `numbering_change_on_listnum_field_records_history_not_pending` — `@original` on a LISTNUM field is a non-authoritative history cache, not pending content; the live separate-result run `2.` stays the visible text on both accept and reject (Part 4 §14.7.1.1, §17.16.5.33, MS-OI §2.1.1771).

## Discarded test-bugs

None.

## Open questions — pending confirmation against real Word

None.
