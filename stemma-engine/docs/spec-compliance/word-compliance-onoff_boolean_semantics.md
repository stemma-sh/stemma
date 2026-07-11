# Word-compliance sweep — CT_OnOff boolean property value-domain and present-means-true semantics

0 confirmed gaps, 12 new regression tests (green), 0 test-bugs.

## Confirmed incompliances

None. The sweep probed every layer of CT_OnOff handling (§17.3.2 run / §17.3.1 paragraph on-off and toggle properties, and the shared `CT_OnOff` / `ST_OnOff` type from §A.1): a bare element with no `w:val` resolving to true ("present means true"), the strict boolean lexical domain for `w:val` (`true`/`false`/`1`/`0` plus the legacy `on`/`off`), and `w:val="off"`/`"0"`/`"false"` resolving to "not applied" rather than being silently flipped on. Import, resolution, and serialize round-trips all matched Word's documented value-domain and present-means-true semantics, including a document built from strict-only boolean lexicals that opens clean in Word.

## New regression tests

- `onoff_attribute_dirty_omitted_defaults_false_not_present_means_true` — an omitted `w:val`-carried optional attribute (e.g. `dirty`) defaults to its attribute default (false), which is distinct from a bare on-off *element* whose mere presence means true.
- `onoff_bare_emboss_present_means_true_display_only_text_survives` — a bare `<w:emboss/>` (no `w:val`) resolves to true (§17.3.2.13) and its run text survives the round-trip as a display-only property.
- `onoff_bare_bcs_present_means_true_complex_script_bold_display_only` — a bare `<w:bCs/>` resolves to true (complex-script bold, §17.3.2.2) and is preserved as a display-only property.
- `widow_control_val_off_is_off_not_flipped_on` — `<w:widowControl w:val="off"/>` resolves to off and is never inverted to on.
- `keep_next_val_off_round_trips_as_explicit_off` — `<w:keepNext w:val="off"/>` round-trips as an explicit off, neither dropped nor flipped.
- `page_break_before_val_off_resolves_to_not_applied` — `<w:pageBreakBefore w:val="off"/>` resolves to "not applied" (§17.3.1.23).
- `run_toggle_bold_val_off_serializes_explicit_off` — a run toggle `<w:b w:val="off"/>` serializes back as an explicit off rather than an omission.
- `contextual_spacing_val_off_resolves_not_applied` — `<w:contextualSpacing w:val="off"/>` resolves to "not applied" (§17.3.1.9).
- `widow_control_off_literal_means_off_not_on` — the literal `off` lexical for widowControl reads as off, confirming `off`/`0`/`false` are equivalent false lexicals.
- `section_title_pg_on_literal_means_true` — `<w:titlePg w:val="on"/>` reads as true, confirming `on`/`1`/`true` are equivalent true lexicals (§17.10.6).
- `contextual_spacing_on_literal_means_true` — the literal `on` lexical for contextualSpacing resolves to true.
- `onoff_strict_only_boolean_lexicals_open_clean` — a document using only strict boolean lexicals for its on-off properties opens clean in Word with no repair.

## Discarded test-bugs

None.

## Open questions — pending confirmation against real Word

None.
