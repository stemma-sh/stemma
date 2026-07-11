# Word-compliance — Default style selection + general style flags (next/default/autoRedefine/hidden/semiHidden/latentStyles)

0 confirmed gaps, 12 new regression tests (green), 0 test-bugs.

## Confirmed incompliances

None. The tests probe how stemma consumes the style-graph "general flags" — default style selection (`w:default` per type, including the `default="0"` off-value), cross-type and dangling `basedOn`/`link`/`next` references, and `autoRedefine` placement — asserting that each well-formed-but-ignored configuration opens clean (`validate()` reports no errors) and leaves run text untouched on the accepted read view, exactly as Word consumes it.

## New regression tests

- `based_on_cross_type_ignored_opens_clean` — a paragraph style whose `basedOn` names a character style has that element ignored (§17.7.4.3); package opens clean and run text is unchanged.
- `based_on_nonexistent_styleid_ignored_opens_clean` — a `basedOn` referencing an absent styleId is ignored (§17.7.4.3); `validate()` reports no errors.
- `link_cross_type_ignored_opens_clean` — a paragraph style's `link` to a non-character style is ignored (§17.7.4.6); `validate()` reports no errors.
- `default_character_style_does_not_default_paragraphs_opens_clean` — a character-only `default` with no paragraph default is valid (§17.7.4.17); opens clean and run text is unchanged.
- `autoredefine_on_character_style_is_noop_opens_clean` — `autoRedefine` on a character style is a benign no-op since Word applies it only to paragraph styles (MS-OI29500 §2.1.232 / §17.7.4.2); opens clean and run text is unchanged.
- `next_existing_character_style_ignored` — a `next` pointing at an existing character style is well-formed-but-ignored (§17.7.4.10 / MS-OI29500 §2.1.238); opens without repair and the styled paragraph reads as the literal run.
- `based_on_dangling_reference_is_root_opens_clean` — a `basedOn` pointing at a non-existent styleId is ignored and the style becomes its own root (§17.7.4.3); opens clean and content is unchanged.
- `link_cross_type_or_dangling_ignored_opens_clean` — a `link` to a non-existent or non-character styleId is ignored (§17.7.4.6); opens clean and the styled paragraph's text is preserved.
- `basedon_cross_type_reference_ignored_no_property_leak` — an ignored cross-type `basedOn` contributes no properties and never alters text (§17.7.4.3); opens clean and run text is unchanged.
- `basedon_nonexistent_styleid_is_root_opens_clean` — a `basedOn` pointing at a non-existent styleId is ignored, changing only the inheritance root (§17.7.4.3); opens clean and run text is unchanged.
- `default_offvalue_paragraph_style_not_applied` — a `default="0"` paragraph style is well-formed (ST_OnOff off-value equals not-specified, so the style is NOT the default) (§17.7.4.17); opens clean and visible text is the literal run.
- `based_on_type_mismatch_ignored_paragraph_on_character` — a cross-type `basedOn` (paragraph style based on a character style) is well-formed markup Word ignores rather than rejecting (§17.7.4.3); opens without repair and content is unaffected.

## Discarded test-bugs

None.

## Open questions — pending confirmation against real Word

None.
