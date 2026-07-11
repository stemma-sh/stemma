# Word-compliance sweep — Run bidirectional, language, and direction properties

0 confirmed gaps, 12 new regression tests (green), 0 test-bugs.

## Confirmed incompliances

None. Coverage spans the run-level bidirectional and direction properties (`w:rtl`, `w:cs`/`w:szCs`, the `w:dir`/`w:bdo` direction wrappers, the `w:em` emphasis mark) together with the run language triple (`w:lang` val/eastAsia/bidi) against ECMA-376 / ISO/IEC 29500-1 §17.3.2 and MS-OI29500, asserting on the read side (`read_accepted()` / `read_rejected()` text) and on `validate()` (opens-clean); every probed behaviour matched what a conforming consumer (Word) does — these properties are display-only or proofing-only hints that carry no tracked-change semantics, leave logical character order and run text unchanged, and the documents open clean.

## New regression tests

- `rtl_bare_toggle_display_only_not_revision` — a bare `w:rtl` toggle is a display-only right-to-left direction flag, not a tracked change; accept and reject reproduce the same logical text.
- `rtl_explicit_off_val_zero_is_valid_present_off_toggle` — `w:rtl w:val="0"` is a valid explicit off-state of the CT_OnOff toggle and opens clean with text unchanged.
- `lang_bidi_only_complex_script_proofing_tag_valid_and_text_preserved` — a run carrying only `w:lang w:bidi=…` (a complex-script proofing tag) is schema-valid and its text is preserved verbatim.
- `dir_ltr_embedding_is_display_only_text_unchanged` — `w:dir w:val="ltr"` raises the bidi embedding level for layout only; logical character order and run text are unchanged across accept/reject.
- `cs_bare_toggle_complex_script_formatting_display_only_text_unchanged` — a bare `w:cs` complex-script-formatting toggle is display-only; it carries no revision and never alters run text.
- `lang_bidi_only_is_proofing_hint_opens_clean` — `w:lang w:bidi=…` is a proofing-language hint only; the run opens clean without repair.
- `hebrew_run_without_cs_or_rtl_stays_ascii_slot_text_verbatim` — Hebrew run content lacking `w:cs`/`w:rtl` is preserved verbatim in its stored slot; stemma does not reorder or reclassify the text.
- `lang_bidi_only_complex_script_proofing_tag_preserved` — the complex-script `w:lang w:bidi` proofing tag round-trips preserved with no content effect.
- `em_emphasis_mark_is_display_only_text_unchanged` — `w:em` (emphasis mark) is a display-only decoration, not a revision, and leaves run text unchanged on accept/reject.
- `rtl_cs_szcs_complex_script_run_opens_clean_text_unchanged` — a run combining `w:rtl` + `w:cs` + `w:szCs` (full complex-script set) is schema-valid, opens clean, and its text is unchanged.
- `lang_eastasia_proofing_tag_is_display_neutral` — `w:lang w:eastAsia=…` is an East Asian proofing-language tag with no display or content effect; text is preserved.
- `em_on_rtl_complex_script_run_is_display_only` — `w:em` applied to an RTL complex-script run stays display-only; the combined direction and emphasis marks carry no tracked-change semantics.

## Discarded test-bugs

None.
