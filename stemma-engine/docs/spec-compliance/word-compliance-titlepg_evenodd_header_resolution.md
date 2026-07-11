# Word-compliance — titlePg / evenAndOddHeaders and effective header/footer resolution

**Summary:** No confirmed pipeline/model gaps, 11 regression tests, 0 test-bugs discarded. 1 test held as an open question (a real divergence whose correct behaviour depends on how real Word behaves on save). Suite: `cargo test -p stemma --test spec_titlepg_evenodd_header_resolution_word_compliance -- --test-threads=1` (11 passed; 0 failed; 1 ignored).

This area covers ISO 29500-1 §17.10 (header/footer references, `titlePg`, `evenAndOddHeaders`), §17.6.12 (`pgNumType`), §17.17.4 (CT_OnOff), Annex A `EG_SectPrContents`, and the MS-OI29500 implementer notes. Per the methodology guard at the top of the test file: stemma round-trips UNTOUCHED content byte-verbatim, so `reserialize()` of an unedited document reflects stemma's verbatim-preservation contract, not Word's render-time normalization. The `xml*` assertions are confined to (a) serializer-emitted child ORDER and (b) opens-clean validity; consumption semantics are asserted on the read side.

## Confirmed incompliances

None confirmed as a gap. The one genuine divergence found (sectPr child reordering on reserialize) is held as an open question rather than a confirmed gap because the "correct" behaviour is itself unresolved: stemma's verbatim-passthrough is a defensible contract for an UNTOUCHED sectPr, and whether Word actively normalizes `EG_SectPrContents` child order on SAVE (as opposed to merely tolerating out-of-order children on OPEN) is a question for real Word. See the open-question note below and the open questions section.

### Open question — `sectpr_titlepg_pgnumtype_input_order_normalized_on_reserialize`

- **Rule:** ECMA-376 Annex A `EG_SectPrContents` / §17.6.12 / §17.10.6 — `EG_SectPrContents` is an `xsd:sequence` with `pgNumType` BEFORE `titlePg`. When authored out of schema order, a serializer that rebuilds `sectPr` from the typed model would re-emit canonical Annex A order (`pgNumType` before `titlePg`).
- **§refs:** §17.6.12 (pgNumType), §17.10.6 (titlePg), Annex A `EG_SectPrContents` / `CT_SectPr`.
- **Classification:** pipeline-vs-model boundary, unresolved. The surface exercised is verbatim-passthrough: the `sectPr` was not touched by an edit, so the typed serializer's ordering pass was never invoked. This is not clearly a pipeline bug (the verbatim contract is intentional) nor clearly a model bug.
- **What stemma does vs what Word does:** stemma round-trips the `sectPr` verbatim and preserves the authored input order. The reserialized `document.xml` emitted `<w:sectPr><w:titlePg/><w:pgNumType w:start="3"/></w:sectPr>` — `pgNumType` at offset 284 follows `titlePg` at offset 272, so the assertion `pg < tp` (pgNumType must precede titlePg) FAILS. What real Word emits on SAVE for this input is not yet measured.
- **Suggested fix site (if confirmation against real Word shows Word normalizes on save):** the typed `sectPr` serializer in `runtime.rs` (sectPr child emission), so that `EG_SectPrContents` children are emitted in Annex A sequence order regardless of authored order — and the verbatim-passthrough path would need to route sectPr through that typed ordering pass. Do NOT patch downstream of the serializer.
- **Companion (passing) control:** `sectpr_pgnumtype_precedes_titlepg_on_reserialize` (already-in-order input) passes, confirming stemma preserves correct order but does not actively impose it.
- **Minimal bodyXml repro:**

```xml
<w:p><w:r><w:t>Body</w:t></w:r></w:p><w:sectPr><w:titlePg/><w:pgNumType w:start="3"/></w:sectPr>
```

## Regression tests

These passing tests encode behaviour stemma gets right; they run as regression guards.

- `titlepg_with_pgnumtype_start_opens_clean_and_preserved` — titlePg combined with `pgNumType/@start` is a conformant configuration; opens clean (§17.10.6 / §17.6.12 / MS-OI29500 §2.1.299).
- `titlepg_false_with_pgnumtype_opens_clean_body_text_unaffected` — `titlePg=false` is the defined CT_OnOff off-state; first-page resolution is a render rule, so accept/reject body text equals authored prose (§17.10.6 / §17.17.4).
- `per_section_titlepg_two_sections_each_independent_opens_clean` — `titlePg` is a per-section property; a two-section doc with titlePg on each opens clean (§17.10.6 / §17.10.5 / MS-OI29500 §2.1.299).
- `titlepg_false_explicit_zero_distinct_from_omitted_opens_clean` — `titlePg w:val="0"` is a valid CT_OnOff OFF value (equivalent to omitted); opens clean (§17.10.6 / §17.17.4).
- `duplicate_first_header_ref_in_section_nonconformant` — two `headerReference w:type="first"` in one section is non-conformant; validate() must report an error (§17.10.5).
- `duplicate_first_footer_ref_in_section_nonconformant` — two `footerReference w:type="first"` is non-conformant (spec worked example); a third `even` footer is conformant (§17.10.2).
- `duplicate_even_header_ref_in_section_nonconformant` — a section is capped at one header per ST_HdrFtr type; two `even` headers is non-conformant (§17.10.5).
- `duplicate_default_footer_ref_in_section_nonconformant` — two `default` footers exceeds one-per-type; non-conformant (§17.10.2).
- `sectpr_pgnumtype_precedes_titlepg_on_reserialize` — in-order input (`pgNumType` before `titlePg`) is preserved on reserialize; opens clean (Annex A EG_SectPrContents / §17.6.12 / §17.10.6).
- `titlepg_and_evenandodd_orthogonal_open_clean` — `titlePg` (first-page gating) and `evenAndOddHeaders` (even-page gating) are orthogonal, independently valid toggles; opens clean (§17.10.6 / §17.10.1 / §17.17.4).
- `titlepg_val_numeric_one_is_explicit_on` — `titlePg w:val="1"` is an enumerated CT_OnOff ON token; conformant, opens clean (§17.17.4 / §17.10.6).

## Discarded test-bugs

None. No test encoded an incorrect expectation this round.

## Open questions — pending confirmation against real Word

One item is pending confirmation against real Word.

- **Test:** `sectpr_titlepg_pgnumtype_input_order_normalized_on_reserialize`
- **Check against real Word:** open-then-save round-trip — load the repro in Word, save, and inspect the emitted `word/document.xml` for the `EG_SectPrContents` child order inside `<w:sectPr>`.
- **Expected value (hypothesis under test):** if Word normalizes on save, the saved `sectPr` should emit `<w:pgNumType .../>` BEFORE `<w:titlePg/>` (canonical Annex A sequence). If Word preserves the authored out-of-order children verbatim, stemma's current behaviour is correct and the test should be deleted as a test-bug.
- **bodyXml:**

```xml
<w:p><w:r><w:t>Body</w:t></w:r></w:p><w:sectPr><w:titlePg/><w:pgNumType w:start="3"/></w:sectPr>
```
