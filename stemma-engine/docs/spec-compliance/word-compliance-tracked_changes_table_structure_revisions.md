# Word-compliance — Table-structure revisions: cellDel/cellIns/cellMerge, deleted/inserted rows, tbl*Change snapshots

No confirmed gaps. 12 regression tests. No test-bugs.

## Confirmed incompliances

None. This audit covers how stemma serializes and consumes table-structure tracked changes: `cellMerge` merge-state revisions (vMerge=cont/rest, empty/id-only, with/without vMergeOrig), `cellDel`, deleted rows (`trPr/w:del`), and the prior-snapshot revision records `tcPrChange`, `trPrChange`, and `tblGridChange`. It checked Annex A child ordering on the re-emitted parts (CT_TcPr, CT_TcPrInner, CT_TrPr, CT_TblGrid), opens-clean validation, and accept-all / reject-all text survival. stemma matches Word on every probed constraint.

## Regression tests

- `cellmerge_cont_is_merge_state_not_content_delete` — cellMerge vMerge=cont changes merge STATE only; accept and reject both keep all cells' text (§17.13.5.3).
- `cellmerge_empty_revision_is_ignorable_noop` — a cellMerge with neither vMerge nor vMergeOrig is ignorable; accept and reject are no-ops (§17.13.5.3).
- `celldel_serializes_before_tcprchange_in_tcpr` — CT_TcPr emits cellDel (cell-markup group) before the trailing tcPrChange (Annex A CT_TcPr/CT_TcPrInner).
- `cellmerge_serializes_after_vmerge_before_tcprchange` — shd (CT_TcPrBase child) serializes before the cellMerge marker per CT_TcPrInner ordering (§17.13.5.3).
- `cellmerge_text_survives_both_resolutions_and_independent_of_sibling_celldel` — sibling cellDel drops its cell's text on accept while the cellMerge cell survives; reject restores all four cells (§17.13.5.1, §17.13.5.3).
- `trprchange_snapshot_serializes_after_base_row_props_in_ct_trpr_order` — CT_TrPr emits the live base row props (trHeight) before the trPrChange snapshot child (Annex A CT_TrPr, §17.13.5.37).
- `tblgridchange_prior_grid_serializes_after_live_gridcols` — CT_TblGrid emits live gridCol* in order, then the tblGridChange prior-grid record (Annex A CT_TblGrid, §17.13.5.33).
- `cellmerge_with_inline_run_edit_resolves_independently_in_same_cell` — an inline w:ins resolves independently of the cellMerge in the same cell; accept keeps 'Add', reject drops it, cell never dropped (§17.13.5.3, §17.13.5.18).
- `cellmerge_cont_without_vmergeorig_opens_clean_text_survives` — cellMerge vMerge=cont with vMergeOrig omitted (defaults 'rest') opens clean; both resolutions keep the text (§17.13.5.3, §17.18.1).
- `cellmerge_no_attrs_revision_ignored_baseline_both_resolutions` — id-only cellMerge is schema-valid and ignorable; accept and reject leave the baseline cell text (§17.13.5.3, §17.18.1).
- `cellmerge_vmerge_rest_split_revision_opens_clean_text_survives` — cellMerge vMerge=rest (split) is a merge-state revision, not a deletion; opens clean, text survives both resolutions (§17.13.5.3, §17.18.1).
- `row_del_independent_of_inner_inserted_run_content` — accepting a row del (trPr/w:del) removes the whole row; rejecting un-deletes the row but independently rejects the inner w:ins (§17.13.5.12, §17.13.5.18).

## Discarded test-bugs

None.

## Open questions — pending confirmation against real Word

None.
