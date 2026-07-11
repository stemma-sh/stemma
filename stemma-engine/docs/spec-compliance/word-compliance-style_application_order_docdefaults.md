# Word-compliance — Style application order (docDefaults -> table -> numbering -> para -> char -> direct)

No confirmed gaps. 12 regression tests. No test-bugs.

## Confirmed incompliances

None. This audit covers the OOXML style-resolution cascade (§17.7.2): docDefaults (`pPrDefault`/`rPrDefault`) as the base layer, then table-style, numbering-level, paragraph-style chain, character-style, and direct formatting layers — covering layer ordering, disallowed children at each layer, `numId=0` numbering removal, and toggle-property XOR semantics (§17.7.3) across layers. Every probed behavior matches Word.

## Regression tests

- `docdefaults_pprdefault_only_no_rprdefault_opens_clean` — a `docDefaults` with only `pPrDefault` and no `rPrDefault` resolves and opens clean.
- `docdefaults_rprdefault_empty_rpr_opens_clean` — an `rPrDefault` wrapping an empty `rPr` is a valid no-op base layer.
- `numbering_lvl_ppr_disallowed_child_ignored_opens_clean` — a disallowed child under a numbering level's `pPr` is ignored, not fatal.
- `numbering_lvl_rpr_disallowed_child_ignored_opens_clean` — a disallowed child under a numbering level's `rPr` is ignored, not fatal.
- `tablenormal_style_children_ignored_opens_clean` — unexpected children of the `TableNormal` style are ignored and the doc opens clean.
- `pprdefault_disallowed_child_sectpr_ignored_opens_clean` — a `sectPr` (disallowed) under `pPrDefault` is ignored, not applied.
- `rprdefault_disallowed_child_rstyle_ignored_opens_clean` — an `rStyle` (disallowed) under `rPrDefault` is ignored, not applied.
- `direct_numid_zero_removes_style_numbering_opens_clean` — direct `numId=0` removes numbering inherited from a paragraph style (§17.9.18).
- `docdefaults_rprdefault_then_pprdefault_order_opens_clean` — `docDefaults` tolerates `rPrDefault` before `pPrDefault` and resolves both layers.
- `toggle_xor_table_level_with_para_chain_cancels_bold` — a toggle (`b`) set at the table-style layer XORs against the paragraph-style chain, cancelling bold (§17.7.3).
- `toggle_docdefaults_true_shortcircuits_xor_to_true` — a toggle set true in `docDefaults` short-circuits the XOR cascade to true (§17.7.3).
- `toggle_first_value_in_basedon_chain_wins_not_nearest` — within a `basedOn` style chain the first (most-derived) explicit value wins, not the nearest ancestor.

## Discarded test-bugs

None.

## Open questions — pending confirmation against real Word

None.
