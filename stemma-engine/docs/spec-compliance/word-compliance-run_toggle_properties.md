# Word-compliance sweep — Run toggle properties (bold, italic, caps, strike, effects) and XOR resolution

**Summary:** 0 confirmed gaps, 8 new regression tests, 3 test-bugs discarded, 1 test held as an open question. Build status: green (`cargo test -p stemma --test spec_run_toggle_properties_word_compliance` → 8 passed, 0 failed, 1 ignored).

This area covers run-level toggle properties (`b`/`bCs`, `i`/`iCs`, `caps`, `strike`/`dstrike`, the `outline`/`shadow`/`emboss`/`imprint` effect group, `effect`, `em`, `cs`) and their accept/reject (XOR-resolution) behaviour. The display-only consumption semantics all hold. The only failures were a cluster of `rPr` child-ordering tests that mismeasured stemma's verbatim-passthrough contract; three were dispositively wrong (deleted) and one is left as an open question on the consumption-vs-save boundary.

## Confirmed incompliances

None. No pipeline-bug or model-bug was confirmed in this area. The canonical-order emitter `build_rpr()` (`stemma-engine/src/serialize/mod.rs`) already emits CT_RPrBase children in Annex A order on the rebuild path, and every display-only toggle correctly survives accept/reject without changing stored characters.

## New regression tests (passing)

These 8 tests encode correct Word consumption semantics and run daily:

- `ics_complex_script_italic_independent_of_i` — a lone `<w:iCs/>` is display-only; accept and reject both preserve run text verbatim and the doc opens clean (§17.3.2.16/§17.3.2.17, §17.17.4).
- `cs_element_makes_ics_govern_and_drops_plain_i` — `bCs` + `i` + `cs` together is a pure display decision; no characters are added or removed on accept/reject; opens clean (§17.3.2.7, ISO/IEC 29500-1 §17.7.3).
- `effect_animated_text_is_not_a_toggle_enum_value` — `<w:effect w:val="lights"/>` is display-only animated text (not a §17.7.3 toggle); accept/reject preserve text; opens clean (§17.3.2.11, §17.18.94).
- `em_emphasis_mark_is_display_only_not_toggle` — `<w:em w:val="dot"/>` is a rendered glyph, not stored content; accept/reject preserve text; opens clean (§17.3.2.12).
- `italic_cs_split_bare_ics_display_only_survives_accept_reject` — bare `<w:iCs/>` (present-means-true CT_OnOff) survives accept-all and reject-all unchanged; opens clean (ISO/IEC 29500-1 §17.3.2.17, §17.17.4, MS-OI29500 §2.1.81).
- `italic_non_cs_toggle_is_display_only_survives_accept_reject` — bare `<w:i/>` is a §17.7.3 display toggle; accept/reject reproduce literal characters; opens clean (ISO/IEC 29500-1 §17.3.2.16, MS-OI29500 §2.1.80).
- `italic_cs_explicit_off_literal_zero_opens_clean` — `<w:iCs w:val="0"/>` is a legal CT_OnOff false literal; opens clean and accept/reject preserve text (ISO/IEC 29500-1 §17.17.4, §22.9.2.7, §17.3.2.17).
- `italic_and_italic_cs_cooccurrence_is_appearance_only_opens_clean` — `<w:i/>` + `<w:iCs/>` are independent toggles over disjoint character classes; co-occurrence is schema-valid, opens clean, accept/reject preserve text (ISO/IEC 29500-1 §17.3.2.16/§17.3.2.17, MS-OI29500 §2.1.80/§2.1.81).

## Discarded test-bugs

All three asserted that `reserialize()` of an UNEDITED document must reorder `rPr` children into CT_RPrBase Annex A sequence. That is wrong on two independent grounds, so the tests encode a behaviour the spec does not require and stemma intentionally does not perform:

1. **Schema.** `EG_RPrBase` is defined as an `<xsd:choice>` and referenced with `maxOccurs="unbounded"` (`reference/schemas/transitional/xsd/wml.xsd`). A repeated `xsd:choice` imposes no sibling-order constraint, so any order of these toggles is schema-valid and Word opens it without repair. Per CLAUDE.md the schema wins over prose; the grounding evidence misread "declared adjacent in a choice" as a sequence constraint.
2. **Wrong execution path.** `reserialize()` is `Document::parse(bytes).serialize(default)` — a no-edit roundtrip that re-zips untouched body markup byte-verbatim (stemma's documented verbatim-fidelity contract). The canonical-order emitter `build_rpr()` (`stemma-engine/src/serialize/mod.rs`) — which already emits the Annex A order correctly on the rebuild path — does not run for an untouched run. The test file's own module docstring restricts `xml*` ordering assertions to markup the serializer actively rebuilds; these tests violated that rule.

Deleted:

- `rpr_order_i_ics_caps` — asserted `i < iCs < caps` reordering on an unedited `<w:caps/><w:iCs/><w:i/>` roundtrip. Output was byte-identical to input (correct, per the verbatim contract). `build_rpr()` emits i(5)→iCs(6)→caps(7) on the rebuild path, so an edited run orders correctly.
- `rpr_order_strike_before_dstrike` — asserted `strike < dstrike` reordering on an unedited `<w:dstrike/><w:strike/>` roundtrip. Already adjudicated in `word-compliance-ppr_rpr_element_ordering.md` as a test-bug; opens-clean is the correct expectation.
- `rpr_order_outline_shadow_emboss_imprint` — asserted `outline < shadow < emboss < imprint` reordering on an unedited reversed roundtrip. All four toggles import into the IR (`import.rs`) and `build_rpr()` emits positions 11–14 in fixed order on rebuild; only the test's measurement surface (verbatim passthrough) was wrong.

## Open questions

- `rpr_order_b_before_bcs_before_i` (`#[ignore]`d) — same surface as the deleted three (authored `i, bCs, b`; reserialize preserved the authored order verbatim: `<w:rPr><w:i/><w:bCs/><w:b/></w:rPr>`). It is held rather than deleted because it sits on a genuine open question: whether stemma diverges from Word at the **consumption-vs-save** boundary (Word always writes CT_RPrBase order on save) or whether the spec test should instead drive an actual edit to force `build_rpr()` re-emission. Classification cannot be made with full confidence without an explicit product decision on that boundary. No real-Word gold-check is required — a repeated `xsd:choice` is order-free by grammar, so Word opens any sibling order clean; the open item is a contract decision, not an unknown Word behaviour. Suggested resolution: rewrite the test to assert against `build_rpr()` output (or an edit that rebuilds the run), matching the methodology the other reordering tests should have used; no engine change is expected.
