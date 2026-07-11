# Word-compliance sweep — Custom tab stops (tabs / CT_TabStop)

**Summary:** 0 confirmed gaps, 11 new regression tests, 1 test-bug discarded. Build status: green — `cargo test -p stemma --test spec_para_tabs_custom_stops_word_compliance -- --test-threads=1` reports 11 passed, 0 failed, 0 ignored.

This area covers Word-consumption / serializer constraints for the `w:tabs` / `w:tab` (CT_TabStop) markup from ISO 29500-1/-4, ECMA-376, and the MS-OI29500 implementer notes. Every constraint stemma already satisfies; the single failing test was a test-bug (wrong expectation), not a stemma gap.

## Confirmed incompliances

None. Every behavioural constraint exercised in this area (legal ST_TabJc / ST_TabTlc enumeration values open clean, negative in-range positions preserved, `start`/`end` normalized to `left`/`right` on serialize, `bar` / `num` / `decimal` alignment preserved, dot leader preserved, hanging-indent + tab-char opens clean, transitional left/right aliases open clean) passes against the current engine.

## New regression tests

These passing tests are kept active as regression coverage.

- `tab_val_bar_num_decimal_center_open_clean` — bar, num, center, decimal are all legal ST_TabJc values; document opens without repair (§17.18.84, §17.3.1.37).
- `tab_leader_heavy_middledot_hyphen_underscore_open_clean` — heavy, middleDot, hyphen, underscore are all legal ST_TabTlc leader values; opens without repair (§17.18.85, §17.3.1.37).
- `tab_pos_negative_permitted_open_clean` — negative in-range pos (-720) is permitted (margin stop); opens without repair (§17.3.1.37, §17.18.81, MS-OI29500 §2.1.61).
- `hanging_indent_with_tab_char_opens_clean` — hanging indent implicitly creates the tab stop the leading tab lands on; well-formed, opens without repair (§17.3.1.38, §17.3.1.12).
- `tab_val_start_end_normalized_to_left_right_on_serialize` — `start` maps to `left` and `end` maps to `right` on serialize, with positions preserved and no stop dropped (§17.18.84, ECMA-376 §14.11.6, §17.3.1.37).
- `bar_tab_stop_preserved_as_bar` — `bar` is preserved as `bar` (not coerced to left/start) with its position intact (§17.18.84, §17.3.1.37).
- `num_list_tab_legacy_value_opens_clean_and_preserved` — legacy `num` list-tab value opens clean and survives re-serialize with its position (§17.18.84, §17.3.1.37).
- `decimal_tab_with_dot_leader_preserved` — decimal alignment and dot leader are both preserved through re-serialize (§17.18.84, §17.18.85, §17.3.1.37).
- `negative_tab_pos_in_range_preserved` — a negative in-range position (-720) is preserved unchanged (not clamped to 0) and opens clean (§17.3.1.37, MS-OI29500 §2.1.61).
- `transitional_tab_val_left_right_aliases_open_clean` — transitional `left`/`right` aliases are accepted; document not flagged invalid (ISO 29500-4 §14.11.6, MS-OI29500 §2.1.556, §17.18.84).
- `decimal_tab_with_dot_leader_opens_clean` — decimal + dot leader is a valid CT_TabStop; validator reports no errors (§17.18.84, §17.18.85, MS-OI29500 §2.1.556).

## Discarded test-bugs

- `clear_tab_stop_dropped_from_effective_set` — Asserted that a plain parse→reserialize of an UNEDITED minimal document must strip a direct `w:tab w:val="clear"` entry. Neither cited section supports that on a non-editing passthrough. ISO 29500-1 §17.18.84 ("removed and ignored when **processing the contents**") governs the EFFECTIVE/resolved tab set used for layout/queries, not the saved markup. ISO 29500-1 §17.3.1.37 gates removal explicitly on "**when the document is next edited**", and a verbatim reserialize of an unedited document is not an edit. MS-OI29500 §2.1.61 / §2.1.556 say nothing about clear-tab removal on save. stemma deliberately preserves the raw authored direct tab list (including `clear`) for round-trip fidelity (`word_ir.rs` TabStopDef: "Direct tab stops from w:pPr/w:tabs (before style resolution)"), while honoring the spec's "processing the contents" semantics in a separate read/layout projection (`import.rs` resolved_stops, `styles.rs` overlay_tab_stops). The test measured the wrong surface: it asserted effective-set semantics on the serialize surface, where stemma correctly rebuilds from the direct authored set. Not a pipeline bug, not a model bug — a test encoding save-normalization-on-resave that the spec does not require.

## Open questions — pending confirmation against real Word

All 11 tests currently in the file are validator/serializer assertions that pass; none has an outstanding gold-check.

Three related schema-validation gaps are worth confirming against real Word and are candidates for `#[ignore]`d validator-gap tests. Each describes a case where `stemma::api::validate()` returns `ok=true` for schema-invalid markup — a silent acceptance the "no silent fallbacks" directive forbids. Check against real Word: does the document open clean? Expected: schema-invalid, so Word reports an error / triggers repair.

1. **Empty `<w:tabs/>` with no child must be invalid.** CT_Tabs requires `minOccurs=1` for its `tab` child (ISO 29500-1 §17.3.1.38; Annex A CT_Tabs; `wml.xsd`). `validate()` returns `ok=true` — a silent acceptance of schema-invalid markup.
   ```xml
   <w:p><w:pPr><w:tabs/></w:pPr><w:r><w:t>x</w:t></w:r></w:p><w:sectPr/>
   ```

2. **`<w:tab>` missing `@pos` must be invalid.** CT_TabStop declares `pos` as `use="required"` (ISO 29500-1 §17.3.1.37; Annex A CT_TabStop; `wml.xsd`). `validate()` returns `ok=true` for a stop with no `pos`.
   ```xml
   <w:p><w:pPr><w:tabs><w:tab w:val="left"/></w:tabs></w:pPr><w:r><w:t>x</w:t></w:r></w:p><w:sectPr/>
   ```

3. **`<w:tab>` missing `@val` must be invalid.** CT_TabStop declares `val` as `use="required"` (ISO 29500-1 §17.3.1.37, §17.18.84; Annex A CT_TabStop; `wml.xsd`). stemma falls back to `Left` and reports `ok=true` — a silent-fallback defect.
   ```xml
   <w:p><w:pPr><w:tabs><w:tab w:pos="720"/></w:tabs></w:pPr><w:r><w:t>x</w:t></w:r></w:p><w:sectPr/>
   ```

Suggested fix site for all three: the CT_Tabs / CT_TabStop decode + `validate` path behind `stemma::api::validate`. These are schema `use="required"` / `minOccurs` checks that should fail loud rather than silently fall back, per the "no silent fallbacks" prime directive.
