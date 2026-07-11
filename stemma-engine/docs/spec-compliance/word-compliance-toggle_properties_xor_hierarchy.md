# Word-compliance — Toggle properties XOR combination across the style hierarchy

No confirmed gaps. 12 regression tests. No test-bugs.

## Confirmed incompliances

None. This audit covers the ECMA-376 §17.7.3 toggle-property XOR resolution rule across the full style hierarchy (docDefaults, table style, paragraph style, character style, and direct/run-level formatting), including how a toggle's effective value is computed by XOR-folding every `true` occurrence up the chain, how non-toggle properties instead resolve by last-value-wins, how direct formatting overrides the hierarchy without participating in the XOR fold, and how all of this interacts with tracked-change accept/reject so that resolution never mutates run text.

## Regression tests

- `table_style_bold_alone_resolves_on_single_level` — a single toggle occurrence (bold from a table style) with no other level resolves on.
- `toggle_xor_resolution_never_deletes_run_text` — XOR toggle resolution affects only formatting, never the textual content of a run.
- `xor_table_para_char_three_levels_opens_clean_text_survives` — three stacked toggle levels (table + paragraph + character style) open clean in Word and the run text survives.
- `missing_toggle_level_falls_to_docdefault_not_ecma_default` — an absent toggle at a level inherits the docDefault value, not the bare ECMA element default.
- `direct_toggle_wins_over_multilevel_xor_no_combination` — a direct-formatting toggle overrides the hierarchy outright and does not XOR-combine with style levels.
- `nontoggle_color_last_value_wins_not_xor` — a non-toggle property (color) resolves by last-value-wins, never by XOR.
- `bold_toggle_on_tracked_inserted_run_accept_keeps_reject_drops` — a bold toggle on a tracked-inserted run is kept on accept and dropped on reject.
- `toggle_xor_table_plus_para_levels_two_true_resolves_off` — two `true` toggle occurrences (table + paragraph style) XOR-fold to off.
- `toggle_xor_docdefaults_true_short_circuits_to_on_over_two_styles` — a `true` toggle at docDefaults combined with two style levels resolves on per the XOR fold.
- `toggle_direct_formatting_off_wins_over_xor_table_and_para` — an explicit direct-formatting off wins over a table+paragraph XOR result.
- `table_style_xor_paragraph_style_bold_resolves_off` — table-style bold XOR paragraph-style bold (two `true`s) resolves off.
- `direct_off_overrides_table_plus_paragraph_style_bold` — direct off overrides the combined table + paragraph style bold.

## Discarded test-bugs

None.

## Open questions — pending confirmation against real Word

None.
