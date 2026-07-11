# Word-compliance — Linked styles (paragraph+character link pairing)

0 confirmed gaps, 11 new regression tests (green), 0 test-bugs.

## Confirmed incompliances

None. Probed `w:link` pairing as a consumption rule (ISO 29500-1 / ECMA-376 §17.7.4.6, MS-OI29500 §2.1.235): cross-type links (para→para, char→char, char→numbering, char→table), dangling links to undefined styleIds, half-pairings (a style with no child `<w:link>`), and duplicate links to the same style. In every case Word "ignores" the link and opens clean; stemma's validator reports no errors, leaves the verbatim `w:link` markup untouched, and read-side text (reject-all / accept-all) is unaffected.

## New regression tests

- `link_para_to_paragraph_style_cross_type_ignored_opens_clean` — a paragraph-style `w:link` pointing at another paragraph style is ignored (not rejected); opens clean and reject-all text is the original run text verbatim.
- `link_to_nonexistent_styleid_ignored_opens_clean` — a `w:link` whose val names an undefined styleId degrades to a no-op; opens clean and reject-all returns the original text.
- `char_style_link_to_character_style_cross_type_ignored_opens_clean` — a character-style `w:link` pointing at another character style is the mirror cross-type mismatch Word ignores; opens clean, run text unchanged.
- `char_style_without_link_not_part_of_pairing_opens_clean` — a standalone character style applied via `rStyle` (even when another style links TO it) is a valid, non-pairing construct; opens clean.
- `para_link_to_nonexistent_styleid_ignored_opens_clean` — the missing-style xref check (I-XREF-001) scopes to content references (pStyle/rStyle) only, not `w:link` targets in styles.xml, so a dangling link is not flagged; opens clean.
- `para_link_to_paragraph_style_crosstype_ignored_opens_clean` — a paragraph style's `w:link` must name a character style or be ignored; a para→para link is silently dropped, not a repair-triggering error.
- `char_link_to_numbering_style_crosstype_ignored_opens_clean` — a character style's `w:link` to a numbering style (target exists, wrong type) is silently dropped; validator reports no errors.
- `duplicate_link_to_same_style_last_wins_opens_clean` — two character styles both linking to one paragraph style (last-link-wins per MS-OI29500 §2.1.235) is tolerated, not an error; opens clean.
- `char_style_without_link_not_a_pairing_opens_clean` — a plain character style with no child `<w:link>`, referenced via `rStyle`, is a valid configuration; opens clean.
- `para_link_to_paragraph_style_is_cross_type_ignored` — a para→para link is ignored (not honored, not an error); accept-all text is unaffected because the paragraph still resolves to its own style.
- `char_link_to_table_style_parent_ignored` — a character style's `w:link` to a table style is a type mismatch Word ignores; opens clean and accept-all text is the run's text unchanged.

## Discarded test-bugs

None.

## Open questions — pending confirmation against real Word

None.
