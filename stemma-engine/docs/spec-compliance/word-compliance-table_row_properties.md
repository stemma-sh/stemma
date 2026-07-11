# Word-compliance — Row properties: height, pagination, header repeat, hidden rows

**Summary:** No confirmed gaps. 12 regression tests. No test-bugs. Suite: `cargo test -p stemma --test spec_table_row_properties_word_compliance` (12 passed; 0 failed).

This audit covers `w:trPr` / CT_TrPrBase row-level properties (`w:gridBefore`/`w:gridAfter`, `w:wBefore`/`w:wAfter`, `w:tblCellSpacing`, `w:tblHeader`, `w:trHeight`, `w:cantSplit`, `w:hidden`, and the trailing `w:trPrChange` sequence) per ISO 29500-1 / ECMA-376 Annex A (CT_TrPrBase, CT_TrPr) / MS-OI29500.

## Confirmed incompliances

None. This audit probes whether stemma diverges from Word on row-property open-clean validity, on the table-rebuild serialization path (tables are always rebuilt from the typed model, never passed through verbatim, so each `w:trPr` child must survive the rebuild), on CT_TrPrBase's unbounded `xsd:choice` ordering (members in any order are schema-valid), on the `trPrChange` revision sequence, and on `w:hidden`'s display-only read semantics. Every case matches Word's behavior.

## Regression tests

- `row_gridbefore_gridafter_opens_clean_and_preserved` — `gridBefore`/`gridAfter` are valid CT_TrPrBase grid-offset children; a row carrying positive offsets opens clean (§17.4.15 / §17.4.14).
- `row_wbefore_wafter_opens_clean_and_preserved` — `wBefore` (CT_TblWidth) paired with `gridBefore` is schema-valid and opens clean (§17.4.86 / §17.4.85).
- `row_tblcellspacing_in_trpr_opens_clean_and_preserved` — `tblCellSpacing` (CT_TblWidth) in `trPr` with a dxa value is schema-valid and opens clean (§17.4.43).
- `trpr_base_children_any_order_opens_clean_tblheader_before_trheight` — CT_TrPrBase is an unbounded `xsd:choice`, so `tblHeader` before `trHeight` opens clean without repair (§17.4.81 / §17.4.49 / §17.4.80).
- `trpr_base_children_any_order_opens_clean_hidden_before_cantsplit` — same unbounded-choice rule: `hidden` before `cantSplit` is schema-valid and opens clean (§17.4.6 / §17.4.20).
- `trpr_base_choice_then_trprchange_sequence_opens_clean` — reordered base members followed by a correctly-trailing `trPrChange` form a valid CT_TrPr and open clean (§17.4.81 / §17.13.5.37).
- `trpr_wbefore_wafter_roundtrip_verbatim_and_opens_clean` — an unedited row preserves both `wBefore` and `wAfter` across the rebuild serialize and opens clean (§17.4.86 / §17.4.87).
- `cantsplit_survives_table_roundtrip` — `cantSplit` is re-emitted by the model-rebuild serializer so the keep-together semantic is not silently lost (§17.4.6 / §17.4.81).
- `hidden_row_marker_survives_table_roundtrip` — `w:hidden` is re-emitted on rebuild so the row is not silently un-hidden (§17.4.20 / §17.17.4).
- `trheight_omitted_val_defaults_zero_preserved` — a `trHeight` with `hRule` but no `val` is valid (val defaults to 0, `exact` is a valid ST_HeightRule token) and opens clean (§17.4.80 / §17.18.37).
- `cantsplit_explicit_off_preserved_not_coerced_on` — `cantSplit val="0"` is a valid CT_OnOff child and is not coerced to ON (§17.4.6 / §17.17.4).
- `hidden_and_cantsplit_coexist_ct_trprbase_order_clean` — `cantSplit` then `hidden` coexist in schema order and open clean; `hidden` is display-only, so the row's text is still present in the rejected read view (§17.4.6 / §17.4.20).

## Discarded test-bugs

None.

## Open questions — pending confirmation against real Word

None.
