# Word-compliance — Level text & number format: lvlText %x substitution, numFmt, lvljc, suff, isLgl

0 confirmed gaps, 12 new regression tests (green), 0 test-bugs.

This area probes how stemma *consumes* numbering-level definitions: the
`lvlText` escape grammar (`%x` substitution vs. literal text), `numFmt`
precedence (including `custom`/`format`), the `lvlJc` supported-value set,
the `suff` default, the `isLgl` boolean toggle, and `lvlOverride` level
replacement. All assertions are consumption-side (opens-clean validity and
accept-all body text), because stemma round-trips untouched numbering.xml
byte-verbatim, so a "Word ignores value V at render" claim is not "Word
rewrites V on save" and an xmlOmits check on a verbatim-preserved attribute
would be a test-bug rather than a gap.

Test file: `stemma-engine/tests/spec_lvltext_numfmt_rendering_word_compliance.rs`.

## Confirmed incompliances

None. stemma opens every probed level definition clean and preserves body
text verbatim through accept-all; no markup that Word accepts was rejected,
and no body text was corrupted while resolving a level.

## New regression tests

- `lvltext_bare_percent_is_literal` — a bare trailing `%` in `lvlText` is literal text, not an escape; the file opens clean (ISO 29500-1 §17.9.11 / MS-OI29500 §2.1.283).
- `lvltext_percent_zero_is_literal_not_escape` — `lvlText="%0."` is schema-valid ST_String and `%0` is outside Word's legal escape set `%[1-9]`; opens clean and accept-all reproduces both run strings verbatim (ECMA-376 §17.9.11 / MS-OI29500 §2.1.283).
- `lvltext_percent_zero_is_literal_not_placeholder` — placeholders are one-based (`>=1`), so `%0` in `%0.%1` is literal text Word tolerates; opens clean (ISO 29500-1 §17.9.11 / MS-OI29500 §2.1.283).
- `lvltext_over_nine_escapes_opens_clean` — `val` is unbounded ST_String; Word's 9-escape cap is a render-time clamp, not a validity gate, so a 10-escape `lvlText` opens without repair (MS-OI29500 §2.1.283 / ECMA-376 §17.9.11).
- `lvltext_null_on_with_val_present_opens_clean` — `val`-with-content makes the `null` attribute ignored, so `null="on"` together with `val="%1."` is a resolvable level; opens clean (ECMA-376 §17.9.11 / MS-OI29500 §2.1.283).
- `lvljc_center_supported_value_opens_clean` — `center` is one of Word's three supported `lvlJc` values (start, center, end); a supported value validates clean (ISO 29500-1 §17.9.7 / MS-OI29500 §2.1.281).
- `lvljc_distribute_unsupported_opens_clean` — `distribute` is a legal ST_Jc value; Word's support gap is a render-time fallback, not a validity gate, so it opens clean (ECMA-376 §17.18.44 / §17.9.7 / MS-OI29500 §2.1.281).
- `lvljc_distribute_unsupported_value_opens_clean` — ST_Jc enumerates `distribute`, so `lvlJc="distribute"` is schema-valid and Word falls back at render rather than rejecting; package validates clean (ECMA-376 §17.9.7 / MS-OI29500 §2.1.281 / wml.xsd ST_Jc).
- `omitted_suff_element_defaults_to_tab_opens_clean` — CT_Lvl declares `suff` minOccurs=0 and an omitted `suff` defaults to tab; a level with no `suff` opens clean and accept-all reproduces run text verbatim (ECMA-376 §17.9.28).
- `islgl_explicit_false_off_opens_clean_body_intact` — `isLgl` is a CT_OnOff boolean; `val="false"` turns the legal toggle off and is schema-valid, so the package opens clean with body text intact (ECMA-376 §17.9.4 / §17.17.4).
- `numfmt_val_custom_with_format_attr_opens_clean` — `numFmt val="custom"` with a `format` attribute is the case where Word uses the format string; a recognised, well-formed level that opens without repair (ECMA-376 §17.9.17 / MS-OI29500 §2.1.286).
- `lvloverride_full_lvl_replacement_opens_clean` — a full `lvl` inside `lvlOverride` is a well-formed replacement of the abstract level; the `num/lvlOverride/lvl` chain is schema-valid and opens without repair (ECMA-376 §17.9.5 / §17.9.8).

## Discarded test-bugs

None.

## Open questions — pending confirmation against real Word

Read-side marker-rendering (`marker_text` synthesis) matches Word on every
probed rendering rule (decimal, upperRoman, lowerLetter, bullet-literal, none,
forward-reference-ignored, multilevel substitution). Two questions remain,
pending confirmation against real Word:

- Does real Word add `null='on'` (and/or refuse to open) for a `<w:lvlText/>`
  with neither `val` nor `null`? MS-OI29500 §2.1.283 calls it a save
  requirement; confirm load behavior. bodyXml: a numbering.xml level with
  `<w:numFmt w:val="decimal"/><w:lvlText/>`.
- The 31-char post-substitution cap and the "Word ignores excess" render
  tolerance (MS-OI29500 §2.1.283) are render-time facts; confirm Word's
  truncation if/when marker truncation is modeled (stemma currently renders the
  full expansion; an opens-clean test already passes).
