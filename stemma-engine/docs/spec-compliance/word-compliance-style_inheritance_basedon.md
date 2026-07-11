# Word-compliance — Style inheritance via basedOn (build-up, conflict override, cycles)

No confirmed gaps. 12 regression tests. No test-bugs.

## Confirmed incompliances

None. This audit covers the `w:basedOn` style-inheritance rules of ECMA-376 / ISO 29500 §17.7 and MS-OI29500 §2.1.233 — cyclic chains (multi-node and self-reference), missing/dangling targets, cross-type basedOn (paragraph-on-character, paragraph-on-table, numbering), over-cap `@val` lengths, duplicate styleIds, and toggle turn-off through an inherited chain — and stemma matches Word's consumption in every case (opens clean, body text survives).

## Regression tests

- `basedon_cycle_opens_clean_and_body_survives` — a two-node basedOn cycle must validate clean and the plain body paragraph's text must round-trip unchanged (MS-OI29500 §2.1.233; ISO 29500-1 §17.7.4.3, §17.7.1).
- `basedon_missing_target_ignored_opens_clean` — an unmatched basedOn target is ignored (style becomes a root), document opens clean, body text survives (ISO 29500-1 §17.7.4.3, §17.7.1).
- `para_basedon_char_crosstype_opens_clean` — a paragraph style basedOn a character style has its basedOn ignored, opens clean, body text survives (ISO 29500-1 §17.7.4.3).
- `turn_off_inherited_toggle_in_chain_reject_text_survives` — turning off an inherited toggle (`w:b w:val="0"`) while inheriting the rest is a legal build-up; opens clean and plain text round-trips unchanged (ISO 29500-1 §17.7.1, §17.7.4.3).
- `basedon_cycle_opens_clean_and_preserved` — the format allows loops; a basedOn cycle validates with no errors (ECMA-376 §17.7.4.3; MS-OI29500 §2.1.233).
- `basedon_type_mismatch_para_on_char_ignored_opens_clean` — a cross-type basedOn (paragraph on character) is ignored, so the styles part is valid and the validator reports no errors (ECMA-376 §17.7.4.3).
- `numbering_style_basedon_ignored_opens_clean` — basedOn on a numbering style is ignored; document is valid even with the element present (ECMA-376 §17.7.4.3).
- `basedon_self_reference_single_node_cycle_opens_clean` — a one-node self-referential basedOn cycle is broken and validates clean, with no hang/crash/error (ISO 29500-1 §17.7.4.3; MS-OI29500 §2.1.233).
- `paragraph_style_basedon_table_style_ignored_opens_clean` — a paragraph style basedOn a table style is ignored; styles part stays valid and opens without repair (ISO 29500-1 §17.7.4.3).
- `numbering_style_basedon_always_ignored_opens_clean` — a numbering style's basedOn is always ignored, keeping the styles part valid; opens clean (ISO 29500-1 §17.7.4.3).
- `basedon_val_over_253_chars_opens_clean` — a basedOn `@val` over Word's 253-char cap references no existing style, so basedOn is ignored; opens clean and body text survives (MS-OI29500 §2.1.233; ISO 29500-1 §17.7.4.3).
- `duplicate_styleid_opens_clean` — duplicate styleIds are recoverable (first kept, rest reassigned); document opens without repair and body text round-trips unchanged (ISO 29500-1 §17.7.4.17).

## Discarded test-bugs

None.

## Open questions — pending confirmation against real Word

None.
