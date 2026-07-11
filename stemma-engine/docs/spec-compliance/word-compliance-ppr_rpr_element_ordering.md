# Word-compliance sweep — pPr / rPr child element ordering and schema validity (Annex A)

0 confirmed gaps, 12 new regression tests (green), 0 test-bugs.

This area covers the child-element ordering and schema validity of the paragraph-mark
run properties (`CT_ParaRPr`), run properties (`CT_RPr` / `EG_RPrBase`), and paragraph
properties (`CT_PPrBase`) sequences defined in ECMA-376 / ISO 29500 Annex A, with a
focus on the interaction between Annex-A ordering and tracked-change markers
(`w:ins` / `w:del` / `w:rPrChange` / `w:pPrChange`) in the paragraph-mark rPr, plus
the serializer's edit/rebuild path (where Annex-A ordering is actively produced) versus
the no-edit roundtrip (where untouched body markup is re-zipped byte-for-byte and the
authored order is preserved verbatim, per the documented contract that Word
tolerates rPr/pPr order variance on open and does not normalize it on save).

## Confirmed incompliances

None. Every probed ordering and schema-validity constraint — Annex-A child sequencing
in `CT_ParaRPr` / `CT_RPr` / `CT_PPrBase`, tracked-change marker placement within the
paragraph-mark rPr, `numPr`-before-`spacing`/`ind`/`jc` ordering on rebuild,
`pPrChange`/`rPrChange` last-child placement, and repeatable-choice tolerance — round-trips
schema-valid and opens clean in real Word.

## New regression tests

- `para_mark_rpr_trackchange_ins_before_rstyle_opens_clean` — a `w:ins` tracked-change marker in the paragraph-mark rPr precedes `rStyle` in the Annex-A `CT_ParaRPr` sequence and the package opens clean.
- `run_rpr_lang_before_b_interleave_schema_valid_opens_clean` — an authored `lang`-before-`b` interleaving in `CT_RPr` round-trips schema-valid and opens clean (Word tolerates the order variance).
- `run_rpr_change_emitted_after_base_props_on_edit_path` — on the edit/rebuild path the serializer emits `w:rPrChange` after the base run properties (Annex-A last-child position).
- `para_mark_rpr_before_jc_in_ppr_verbatim_opens_clean` — the paragraph-mark `rPr` precedes `jc` within `pPr` on the no-edit roundtrip, preserved verbatim and opening clean.
- `para_mark_rpr_inner_children_out_of_order_verbatim_opens_clean` — out-of-order inner children of the paragraph-mark rPr survive the no-edit roundtrip verbatim and open clean (Word tolerates the variance).
- `para_mark_rpr_del_marker_precedes_run_props` — a `w:del` paragraph-mark deletion marker precedes the base run properties in `CT_ParaRPr`.
- `rpr_base_is_repeatable_choice_duplicate_bool_opens_clean` — `EG_RPrBase` is a repeatable choice, so a duplicated boolean property round-trips schema-valid and opens clean.
- `ppr_numpr_emitted_before_spacing_ind_jc_on_rebuild` — on rebuild the serializer emits `numPr` before `spacing`, `ind`, and `jc` per the Annex-A `CT_PPrBase` sequence.
- `ppr_change_is_last_child_of_ppr` — `w:pPrChange` is emitted as the last child of `pPr` (Annex-A `CT_PPrBase` last-child position).
- `para_mark_del_first_in_ct_pararpr_merges_on_accept` — a `w:del` deletion marker placed first in `CT_ParaRPr` is consumed as a paragraph-mark deletion and merges the paragraph on accept.
- `para_mark_del_after_base_props_still_consumed_as_deletion` — a `w:del` deletion marker placed after the base run properties is still consumed as a paragraph-mark deletion.
- `stacked_para_mark_ins_before_del_collapses_break_both_resolutions` — a stacked paragraph-mark `w:ins`-before-`w:del` collapses the paragraph break consistently under both accept and reject resolutions.

## Discarded test-bugs

None.
