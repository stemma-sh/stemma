# Word-compliance — Font embedding: embedRegular/Bold/Italic, embedTrueTypeFonts, obfuscated font part

0 confirmed gaps, 12 new regression tests (green), 0 test-bugs.

## Confirmed incompliances

None. The tests probe the run-side font-embedding linkage that drives Word's embedded-form selection: the four `w:rFonts` name slots (`ascii`, `hAnsi`, `eastAsia`, `cs`), the theme-slot variants (`asciiTheme`/`hAnsiTheme`), the `w:hint` attribute that routes ambiguous glyphs to a slot, and the complex-script bold toggle `w:bCs` — the markup that links a run to its embedded regular/bold/italic form (`embedRegular`/`embedBold`/`embedItalic` under `embedTrueTypeFonts`) and to the obfuscated font part. Checked: structured re-emission on `reserialize()`, package validity (`opensClean`), and consumption-side text invariance (`acceptText`) for the bold/italic toggles; stemma re-emits every slot verbatim, opens clean, and treats embedded-form selection as display-only, matching Word.

## New regression tests

- `rfonts_cs_slot_preserved_for_complex_script_embed_form` — the Complex Script (`w:cs`) slot survives reserialize so an rtl run stays linked to its CS embedded font (§17.3.2.26 / §17.8.3.6 / §17.8.3.10).
- `rfonts_hint_attribute_roundtrips_verbatim` — `w:hint` is authored slot-selection content and is re-emitted verbatim, never silently dropped (§17.3.2.26 / §17.18.41).
- `empty_rfonts_inherits_and_opens_clean` — a slotless `<w:rFonts/>` is pure inheritance: opens clean and never alters run text (§17.3.2.26 / §17.8.3.10).
- `rfonts_ascii_theme_slot_preserved_verbatim` — `w:asciiTheme` is an authored theme-font slot reference re-emitted verbatim to keep theme/embedded-font linkage (§17.3.2.26 / §17.18.96).
- `rfonts_overlength_name_not_truncated_on_save` — an over-length font name is conformant markup Word opens; the <32 cap is a render limit, so save preserves the name and text (MS-OI29500 §2.1.88 / §2.1.270).
- `embed_italic_form_italic_only_run_text_invariant` — `embedItalic` form selection is display-only; accepted read text is unchanged regardless of which embedded form renders (§17.8.3.5 / MS-OI29500 §2.1.261).
- `rfonts_cs_slot_preserved_for_complex_script_embed_linkage` — both `cs` and the independent `ascii` slot survive reserialize so per-script font/embed resolution stays intact (§17.3.2.26 / §17.8.3.6 / CT_Fonts).
- `rfonts_hint_eastasia_value_domain_and_preserved` — `hint="eastAsia"` is within ST_Hint and is preserved so Word keeps reading the eastAsia slot and its embedded form (§17.3.2.26 / ST_Hint).
- `complex_script_bold_toggle_is_display_only_accept_text_unchanged` — `w:bCs` only selects the bold embedded form for rendering; accepted text content is unchanged by the toggle (MS-OI29500 §2.1.264 / §17.8.3.4 / §17.3.2.26).
- `all_five_rfonts_slots_plus_hint_roundtrip_together` — all four literal slots survive together and in document order alongside `hint` so each script class keeps its embedded-font linkage (§17.3.2.26 / §17.8.3.6 / CT_Fonts).
- `cs_slot_with_bcs_toggle_roundtrips_clean` — `w:cs` plus the `w:bCs` complex-script bold toggle both survive reserialize unchanged and open clean (§17.3.2.26 / §17.3.2.2 / CT_Fonts).
- `rfonts_hint_eastasia_preserved_and_clean` — `hint="eastAsia"` and the `eastAsia` name slot it routes to both survive the roundtrip linked to the embedded form (§17.3.2.26 / CT_Fonts).

## Discarded test-bugs

None.

## Open questions — pending confirmation against real Word

None.
