# Word-compliance — Font table elements: font/name/charset/family/pitch/panose1/sig/altName/notTrueType

0 confirmed gaps, 11 new regression tests (green), 0 test-bugs.

## Confirmed incompliances

None. The tests probe how stemma consumes and re-emits font references (`w:rFonts` link keys across the ascii/hAnsi/eastAsia/cs slots, comma-bearing and `@`-prefixed vertical-form names, paragraph-mark vs run-level font properties) and how absent fonts with no backing `fontTable.xml` entry are handled across parse, validate, reserialize, and accept/reject — every behavior already matched the spec.

## New regression tests

- `rfonts_ascii_name_with_comma_is_single_link_key` — an `rFonts/@ascii` name is one opaque `ST_String` link key; the comma is re-emitted intact and never split into `Foo`/`Bar` (§17.8.3.10, MS-OI29500 §2.1.262).
- `absent_font_substitution_inside_w_ins_preserves_tracking` — an uninstalled font on a run inside `w:ins` is render-time substitution: accept keeps the inserted text, reject drops exactly it, package opens clean (§17.8.2, §17.13.5.16).
- `three_distinct_absent_slots_no_silent_guess` — distinct absent ascii/eastAsia/cs faces are each preserved verbatim, none collapsed or silently guessed, and the document opens clean with no font table (§17.8.2, §17.3.2.26).
- `paragraph_mark_rfonts_absent_font_opens_clean_text_verbatim` — an absent font on the paragraph-mark `rPr` is render-only: body text is verbatim under both accept and reject and the package opens clean (§17.8.2, §17.3.2.26).
- `rfonts_names_absent_font_table_opens_clean_no_rewrite` — a font with no `fontTable.xml` entry opens clean and keeps the authored name verbatim; no save-time rewrite to a resolved substitute in `document.xml` (§17.8.2, §17.8.3.10, MS-OI29500 §2.1.262).
- `rfonts_eastasia_cs_distinct_slots_text_preserved_both_resolutions` — distinct ascii/eastAsia/cs slots stay distinct through reserialize; eastAsia and cs are not dropped or collapsed onto ascii (§17.8.3.10, §17.3.2.26, MS-OI29500 §2.1.88).
- `rfonts_in_ins_named_font_is_property_not_revision` — `rFonts` on a run inside `w:ins` is a font property, not part of the revision: accept keeps the inserted text, reject drops exactly it, opens clean (§17.3.2.26, §17.13.5.16, MS-OI29500 §2.1.88).
- `vertical_at_prefixed_font_in_w_del_tracked_semantics` — an `@`-prefixed vertical-text font on a deleted run does not alter tracked semantics: reject restores the deleted text, accept removes exactly the delText, opens clean (MS-OI29500 §2.1.270, §17.13.5.14).
- `rfonts_family_unknown_to_table_does_not_error_validate` — missing substitution-hint metadata (no font table) is advisory, not an error: a run naming a font with no backing entry validates clean (§17.8.2, §17.8.3.9, §17.8.3.10).
- `rfonts_at_prefixed_vertical_name_distinct_from_unprefixed` — `@SimSun` and `SimSun` are two distinct link keys: the leading `@` vertical-form prefix survives reserialize verbatim and is never stripped (§17.8.3.10, §17.3.2.26, MS-OI29500 §2.1.88).
- `rfonts_on_paragraph_mark_rpr_distinct_from_run_opens_clean` — paragraph-mark `rPr` `rFonts` selects the mark-glyph font only; it does not alter body run text under accept/reject and the package opens clean (§17.3.2.26, ISO 29500-1 §17.3.2, MS-OI29500 §2.1.88).

## Discarded test-bugs

None.

## Open questions — pending confirmation against real Word

None.
