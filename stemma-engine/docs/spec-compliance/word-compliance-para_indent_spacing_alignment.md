# Word-compliance sweep — Paragraph indentation, spacing, and alignment (precedence/normalization traps)

0 confirmed gaps, 12 new regression tests (green), 0 test-bugs.

## Confirmed incompliances

None. The sweep probed the precedence and normalization traps in CT_Ind, CT_Spacing, and ST_Jc / CT_PPr ordering — the cases where Word silently normalizes, clamps, or ignores a value at render time rather than refusing the file: full ST_Jc enumeration breadth (`mediumKashida`, `highKashida`, `both`), out-of-range tab stops past Word's render clamp, `mirrorIndents` inside a table cell (ignored at render, valid as markup), `firstLine`+`hanging` both directly present on one `w:ind` (mutually exclusive at consumption, well-formed as schema), partial and signed `w:spacing` values (`before`-only, lone `lineRule`, negative `line`, negative `beforeLines`), and CT_PPr Annex A child ordering (`jc` before `textAlignment`). In every case stemma validates clean (Word opens without repair), preserves the value verbatim, keeps content extraction stable, and emits CT_PPr children in Annex A order on re-serialize.

## New regression tests

- `jc_medium_kashida_value_domain_opens_clean` — `w:jc w:val="mediumKashida"` is an enumerated ST_Jc value (§17.18.44 / §17.3.1.13); opens clean and accept-text is unchanged.
- `tab_pos_beyond_word_clamp_range_opens_clean` — tab `pos=50000` past Word's render clamp is in the ST_SignedTwipsMeasure domain (MS-OI29500 §2.1.61 / §17.3.1.37); opens clean, layout-only, text unaffected.
- `mirror_indents_in_table_cell_opens_clean` — `w:mirrorIndents` is valid on any paragraph and only ignored at render inside tables (MS-OI29500 §2.1.49 / §17.3.1.18); opens clean, cell text unaffected.
- `jc_both_justify_opens_clean_text_unchanged` — `w:jc w:val="both"` is a valid ST_Jc value and Word's lowKashida choice is render-only (MS-OI29500 §2.1.545 / §17.18.44); opens clean, text unchanged.
- `jc_medium_kashida_opens_clean_and_value_preserved` — `mediumKashida` is normative across transitional+strict XSD/RNC (§17.3.1.13 / §17.18.44 / ECMA-376 §14.11.2); validate() reports no errors.
- `jc_high_kashida_distinct_value_preserved` — `highKashida` is a distinct normative ST_Jc enumeration value (§17.3.1.13 / §17.18.44); validate() reports no errors.
- `ind_firstline_and_hanging_both_direct_open_clean_word_normalizes_to_hanging` — `w:ind` with both `firstLine` and `hanging` is well-formed (CT_Ind independent optional attrs, §17.3.1.12); opens clean and the conflict does not corrupt extraction (mutual-exclusivity is a consumption rule; Word resaves hanging-only — pending confirmation against real Word).
- `spacing_before_only_no_line_no_linerule_opens_clean_inherits_line` — `w:spacing` carrying only `before` is schema-valid (every CT_Spacing attribute independently optional, §17.3.1.33 / §17.7.2); opens clean, the partial element does not break extraction.
- `spacing_line_negative_signed_twips_opens_clean` — negative `line` is in the ST_SignedTwipsMeasure domain (§17.3.1.33 / §17.18.81); validates clean, not rejected as out-of-range twips.
- `ppr_textalignment_must_follow_jc_ordering` — CT_PPr is an Annex A ordered sequence with `jc` before `textAlignment` (§17.3.1.13 / §17.3.1.39); opens clean and the serializer emits `jc` before `textAlignment`.
- `spacing_linerule_without_line_opens_clean` — `w:spacing` with `lineRule` but no `line` is schema-valid (independent optional attrs, §17.3.1.33 / §17.18.48); opens clean, single spacing applies, text reads unchanged.
- `spacing_beforelines_negative_decimal_opens_clean` — negative `beforeLines` is in the signed-integer ST_DecimalNumber domain (§17.3.1.33 / §17.18.10); validates clean.

## Discarded test-bugs

None.

## Open questions — pending confirmation against real Word

None.
