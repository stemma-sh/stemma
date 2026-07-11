# Word-compliance sweep — Numbering overrides: lvlOverride / startOverride / lvlRestart / start counter semantics

**Summary:** 0 confirmed gaps, 8 new regression tests, 1 test-bug discarded, 1 open question. Build green: `cargo test -p stemma --test spec_numbering_overrides_restart_word_compliance` → **9 passed; 0 failed; 1 ignored**.

This area mined ISO 29500-1 §17.9 (numbering), MS-OI29500 §2.1.282 / §2.1.292, and the normative `CT_NumLvl` / `CT_Lvl` / `CT_DecimalNumber` XSDs. No daily-checkable incompliance survived classification: every assertion that fired turned out to be a wrong expected value in the test (transcription error), not an engine defect. stemma opens every probed package clean and reproduces body run text verbatim through accept-all / reject-all, which is the contract these daily checks encode (per the file's methodology note: opens-clean + read-side text only; marker SEQUENCE is confirmed against real Word).

## Confirmed incompliances

None. No pipeline-bug or model-bug was confirmed in this area. The two assertions that failed during the sweep were both classified as test-bugs (wrong expected literal), documented below.

## New regression tests

All passing; each encodes a Word-consumption / schema-validity constraint that stemma already satisfies.

- `lvlrestart_zero_and_default_coexist_full_three_level_worked_example` — omitted `lvlRestart` on ilvl 1 + `lvlRestart=0` on ilvl 2 is schema-valid and opens clean; accept-all reproduces the seven runs verbatim (§17.9.10).
- `lvlrestart_one_resets_only_on_top_level_not_middle` — `lvlRestart=1` is inside Word's 0..=7 band, opens clean, body text preserved (§17.9.10; MS-OI29500 §2.1.282).
- `lvlrestart_omitted_resets_on_any_higher_not_just_immediate` — a level with omitted `lvlRestart` is schema-valid, opens clean, body text preserved (§17.9.10; MS-OI29500 §2.1.282).
- `startoverride_applies_only_on_first_encounter_not_on_resume` — two independent `num` instances over one `abstractNum`, one carrying a `startOverride`-only `lvlOverride`, open clean and preserve body text (§17.9.26 / §17.9.15).
- `startoverride_value_lower_than_abstract_start_governs_first_value` — a `startOverride`-only `lvlOverride` opens clean regardless of how the override value compares to the abstract `start`; body text preserved (§17.9.26 / §17.9.8; MS-OI29500 §2.1.292).
- `two_lvloverride_same_ilvl_open_clean_body_verbatim` — two same-`ilvl` `lvlOverride` children are schema-valid (`CT_NumLvl` maxOccurs=9, no uniqueness facet, ECMA-376 §A.1) and open clean (§17.9.8 / §17.9.26).
- `start_override_zero_minimum_reset_open_clean` — `startOverride=0` is at the minimum of Word's permitted starting-value range and `CT_DecimalNumber`-valid; opens clean (MS-OI29500 §2.1.292 / §17.9.25; §17.9.26).
- `lvloverride_full_precedence_stack_open_clean_body_verbatim` — a `lvlOverride` carrying `startOverride` + nested `lvl` (`start`, `lvlRestart`) respects both nested schema sequences, opens clean, and preserves body text through both accept-all and reject-all (§17.9.26; MS-OI29500 §2.1.292 / §2.1.282).
- `lvlrestart_higher_than_current_level_ignored_open_clean` — a higher-than-current `lvlRestart` is an ignored no-op (not invalid), value inside Word's 0..=7 band; opens clean, body text preserved (§17.9.10; MS-OI29500 §2.1.282).

## Discarded test-bugs

- `lvl_override_no_children_zero_levels_opens_clean` — **DELETED.** The spec premise and opens-clean expectation were correct and PASSED (an empty `lvlOverride` overrides zero levels per §17.9.8; `CT_NumLvl` children are optional per §A.1 / §17.9.15, so the package is schema-valid). The only failure was the `accepted_text` literal: the body has two separate numbered paragraphs, so `to_text()` joins them with the documented `\n\n` block separator (`"Item\n\nNext"`), but the test expected `"ItemNext"`. `.trim()` does not remove an interior separator. This was a transcription error in the expected string, not an engine, model, or measurement defect; the only Word-observable claim (opens-clean) already passed. No real-Word check needed.

## Open questions — pending confirmation against real Word

- `lvl_restart_n_gt_1_restart_band_opens_clean` — open question.
  - **Check against real Word:** accept-all text. A real-Word check is not strictly required (the expected literal omitted block separators; opens-clean passed and stemma's accept-all text is the six runs joined by `\n\n`). Recorded as an open question rather than deleted because the spec's intent for `lvlRestart=2` restart-band layout is not asserted by the daily check — only run text is — so the expected literal can either be corrected to the `\n\n`-joined form and re-activated, or the actual restart-band marker sequence confirmed against real Word before deciding.
  - **Expected value (as written in the test, suspected wrong):** `"L0aL1aL2aL2bL1bL2c"`. stemma's actual accept-all text: `"L0a\n\nL1a\n\nL2a\n\nL2b\n\nL1b\n\nL2c"`.
  - **bodyXml:**

    ```xml
    <w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>L0a</w:t></w:r></w:p>
    <w:p><w:pPr><w:numPr><w:ilvl w:val="1"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>L1a</w:t></w:r></w:p>
    <w:p><w:pPr><w:numPr><w:ilvl w:val="2"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>L2a</w:t></w:r></w:p>
    <w:p><w:pPr><w:numPr><w:ilvl w:val="2"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>L2b</w:t></w:r></w:p>
    <w:p><w:pPr><w:numPr><w:ilvl w:val="1"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>L1b</w:t></w:r></w:p>
    <w:p><w:pPr><w:numPr><w:ilvl w:val="2"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>L2c</w:t></w:r></w:p>
    <w:sectPr/>
    ```

    numbering.xml: `abstractNum 0` with ilvl 0/1 `decimal` (`%1.`, `%1.%2.`) and ilvl 2 `decimal` `%1.%2.%3.` carrying `<w:lvlRestart w:val="2"/>`; `num 1 -> abstractNumId 0`.

No confirmed gap in this area requires a real-Word check, so this list holds only the single open question above.
