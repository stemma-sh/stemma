# Word-compliance — Theme fonts: rFonts theme attrs and theme->font-table->actual-name resolution

No confirmed gaps. 12 regression tests. No test-bugs.

## Confirmed incompliances

None. This audit covers the `rFonts` theme-font slots (`asciiTheme`, `hAnsiTheme`, `eastAsiaTheme`, `cstheme`), the `ST_Theme` token enumeration (`majorAscii`/`minorAscii`/`majorHAnsi`/`minorHAnsi`/`majorEastAsia`/`minorEastAsia`/`minorBidi`), the lowercase `cstheme` spelling in `CT_Fonts`, the `hint`/`cs`/`rtl` font-slot routing rules, and coexistence of explicit and theme slots — standalone and inside `w:ins`/`w:del`/paragraph-mark `rPr` — finding stemma schema-valid (opens clean), byte-verbatim on roundtrip (tokens never flattened), and consumption-correct (theme/hint/cs/rtl select font slots only and never alter resolved text on accept/reject) in every case.

## Regression tests

- `ascii_axis_theme_tokens_major_minor_ascii_legal_and_preserved` — `asciiTheme` `majorAscii`/`minorAscii` are enumerated `ST_Theme` values that open clean and survive roundtrip verbatim as tokens, not flattened to a concrete face (§17.18.96; §17.3.2.26).
- `cstheme_off_axis_major_eastasia_lowercase_name_preserved_opens_clean` — `cstheme="majorEastAsia"` opens clean and is re-emitted with the lowercase `cstheme` spelling; the serializer never produces camelCase `csTheme` (§17.3.2.26; wml.xsd CT_Fonts).
- `cs_forces_cstheme_slot_for_all_chars_text_identity_no_revision` — `cstheme`+`w:cs` forces the complex-script slot for all chars but is consumption-only, so accept and reject both yield the original mixed-script text unchanged (MS-OI29500 §2.1.88; §17.3.2.26).
- `coexisting_ascii_and_asciitheme_legal_text_identity` — explicit `ascii` and `asciiTheme` on one `rFonts` is legal font-resolution arbitration; with no tracked change, accept and reject reproduce the run text verbatim (§17.3.2.26; MS-OI29500 §2.1.88).
- `theme_ascii_slot_tokens_major_minor_ascii_open_clean` — `asciiTheme`/`hAnsiTheme` carrying `majorAscii`/`minorAscii` tokens are schema-valid `CT_Fonts` and Word opens the run without repair (§17.3.2.26; §17.18.96; wml.xsd CT_Fonts).
- `rtl_alone_triggers_cstheme_font_slot_text_preserved` — `w:rtl` with a `cstheme=minorBidi` token triggers the complex-script slot for layout only; accept and reject both equal the source run text (MS-OI29500 §2.1.88; §17.3.2.26).
- `rfonts_hint_default_is_legal_st_hint_no_eastasia_promotion` — `hint="default"` is a valid `ST_Hint` value that opens clean and does not promote the run to the east-asia slot (§17.18.41; wml.xsd CT_Fonts ST_Hint).
- `minor_ascii_theme_token_legal_and_roundtrips` — `asciiTheme=minorAscii`/`hAnsiTheme=majorAscii` are enumerated `ST_Theme` members; the run is schema-valid and Word opens without repair (§17.3.2.26; §17.18.96; MS-OI29500 §2.1.88).
- `theme_rfonts_run_inside_ins_accept_keeps_reject_drops` — theme-slot attrs on a run inside `w:ins` are font properties only: accept keeps the inserted run's text, reject drops exactly that run, base text untouched (MS-OI29500 §2.1.88; §17.13.5.16).
- `within_element_multislot_coexistence_in_del_reject_restores_text` — coexisting `ascii`+`asciiTheme`+`eastAsia`+`eastAsiaTheme` on one `rFonts` inside `w:del` are font selectors only: reject restores the deleted text verbatim, accept removes exactly the `w:delText` (MS-OI29500 §2.1.88; §17.13.5.14).
- `theme_eastasia_hint_run_in_del_reject_restores_text` — `eastAsiaTheme`+`hint="eastAsia"` on a run inside `w:del` are EA-slot selectors: reject restores the deleted text, accept removes exactly the `w:delText` (MS-OI29500 §2.1.88; §17.18.41; §17.13.5.14).
- `paragraph_mark_hint_eastasia_with_eastasiatheme_opens_clean_text_preserved` — `hint="eastAsia"` alongside `eastAsiaTheme` on the paragraph-mark `rPr` is the documented schema-valid structure that opens clean and carries no revision, so accept reproduces the body text unchanged (§17.3.2.26 example; §17.18.41; MS-OI29500 §2.1.88).

## Discarded test-bugs

None.

## Open questions — pending confirmation against real Word

None.
