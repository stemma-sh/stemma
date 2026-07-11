# Word-compliance sweep — Run fonts and complex-script property splitting (rFonts slots, cs, *Cs pairing)

0 confirmed gaps, 11 new regression tests (green), 0 test-bugs.

## Confirmed incompliances

None. Coverage spans per-character routing of a run's text into the four `w:rFonts` script slots (ascii, hAnsi, eastAsia, cs), how RTL force and East Asian classification select the complex-script (`cs`) slot, the `w:hint` slot-selection override, the `*Cs` complex-script run-property pairing (`bCs` per-character bold split), default-font resolution when `w:rFonts` is absent, and the per-element (not cross-level) nature of the `w:ascii`/`w:asciiTheme` mutual ignore — across reserialize, accept, and reject paths, with text preservation and schema validity asserted.

## New regression tests

- `rtl_forces_all_chars_into_cs_slot_text_preserved` — when RTL is in force, every character routes through the complex-script (`cs`) slot and the run text survives the roundtrip intact.
- `no_rfonts_default_font_is_times_new_roman_text_preserved` — a run with no `w:rFonts` resolves to the Times New Roman default and preserves its text.
- `eastasia_classified_chars_escape_cs_rtl_slot` — characters classified as East Asian stay in the `eastAsia` slot rather than being pulled into the `cs`/RTL slot.
- `mixed_script_run_ascii_cs_slots_no_toggle` — a run mixing ASCII and complex-script characters keeps each in its own slot without spuriously toggling a property.
- `rtl_forces_all_chars_to_cs_slot_over_classification` — an explicit RTL force wins over per-character script classification, routing all characters to the `cs` slot.
- `ascii_asciitheme_ignore_is_per_element_only` — the `w:ascii`/`w:asciiTheme` mutual ignore applies only within a single element, not beyond it.
- `rfonts_hint_default_is_valid_enum_and_preserved` — the default `w:hint` value is a valid ST_Hint enum member and is preserved through serialization.
- `rfonts_all_nine_slots_schema_valid` — a `w:rFonts` carrying all nine slot attributes serializes to schema-valid markup.
- `basic_latin_run_with_hint_eastasia_is_not_a_revision` — a Basic-Latin run tagged `w:hint="eastAsia"` is not misread as a tracked revision.
- `hint_eastasia_eastasian_char_bypasses_bcs_per_char_bold_split` — an East Asian character under `w:hint="eastAsia"` bypasses the `bCs` per-character bold split.
- `ascii_asciitheme_mutual_ignore_is_per_element_not_cross_level` — the `w:ascii`/`w:asciiTheme` mutual ignore is scoped per element and does not propagate across levels.

## Discarded test-bugs

None.
