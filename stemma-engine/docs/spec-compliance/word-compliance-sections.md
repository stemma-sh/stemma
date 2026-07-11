# Word-compliance sweep — Sections & page setup (§17.6)

**Summary:** 0 confirmed gaps, 10 new regression tests, 0 test-bugs discarded. Build status: green — `cargo test -p stemma --test spec_sections_word_compliance -- --test-threads=1` reports **10 passed; 0 failed; 0 ignored**.

This area targets section properties under ECMA-376 / ISO 29500 / MS-OI29500 §17.6: `sectPr` placement (non-final-in-`pPr` / final-in-`body`), page size and orientation, continuous section breaks with inherited page props, and column definitions. Every assertion holds against the current engine, so there are no incompliances to record and nothing to ignore.

## Confirmed incompliances

None. Every test in this file passes against the current engine, so there are no pipeline-bug or model-bug incompliances to rank.

## New regression tests

All ten tests are active (passing) and run daily. Each encodes a Word-consumption / OOXML-storage rule for section properties.

| Test | Rule |
| --- | --- |
| `sectpr_placement_nonfinal_in_ppr_final_in_body` | §17.6.17/§17.6.18: a non-final section's `sectPr` round-trips nested in the last paragraph's `pPr`; the final section's `sectPr` is the last child of `w:body`. |
| `pgsz_omitted_orient_not_synthesized_portrait` | §17.6.13: a `pgSz` with only `w`/`h` (portrait implied) is valid and opens without repair; orientation is not synthesized. |
| `continuous_section_omits_page_level_props_carries_linenumbering_valid` | §17.6.22/§17.6.8: a continuous `sectPr` that omits page-level props but carries `lnNumType` is valid (page props inherited). |
| `pgsz_orient_landscape_authoritative_over_dimensions` | §17.6.13: a landscape `pgSz` with explicit `w`/`h` and `orient` is valid markup Word opens without repair. |
| `two_section_inparagraph_sectpr_opens_clean` | §17.6.17/§17.6.18: non-final `sectPr` in `pPr` + final `sectPr` as a body child is well-formed; Word opens it without repair. |
| `cols_unequal_num_matches_col_count_opens_clean` | §17.6.4/§17.6.3: an unequal-width `cols` block where `num` equals the child `col` count and every `col` carries a width is Word-valid. |
| `cols_equalwidth_default_true_ignores_col_children` | §17.6.4: `equalWidth` defaulting true with `num=3` and three `col` children is well-formed; column geometry is layout-only and the read surface reports body text intact. |
| `multi_section_sectpr_placement_in_body_and_ppr_opens_clean` | §17.6.17/§17.6.18: non-final `sectPr` inside the last paragraph's `pPr` and final `sectPr` as the last body child matches Word's storage contract. |
| `continuous_section_omitting_page_props_opens_clean` | §17.6.22: a continuous section may omit page-level properties (inherited); supplying `type val="continuous"` and omitting `pgSz`/`pgMar` opens without repair. |
| `sectpr_in_table_cell_paragraph_opens_clean` | §17.6.18: a `sectPr` on a table-cell paragraph must not reach Word as a cell-paragraph section break; the document opens without repair. |

## Discarded test-bugs

None. No test in this file encoded a wrong expectation, so nothing was deleted.
