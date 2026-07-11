# Word-compliance — Property-change revisions: rPrChange, pPrChange, sectPrChange (reject-restore snapshot semantics)

**Summary:** 2 confirmed gaps, 10 regression tests, 0 test-bugs discarded. Suite: `cargo test -p stemma --test spec_tracked_changes_property_revisions_word_compliance -- --test-threads=1` (10 passed; 0 failed; 2 ignored). The 2 ignored tests encode the confirmed gaps below. Both reduce to the same confirmed validator gap, so they are `#[ignore]`d (not deleted) — they encode correct Word behaviour the engine does not yet satisfy.

---

## Confirmed incompliances

Ranked: pipeline-bug > model-bug, high > low confidence. Both findings are the **same** validator pipeline bug surfacing on two different revision elements.

### 1. `w:pPrChange` missing the required `w:id` opens clean (validator pipeline bug, high confidence)

- **Rule:** `w:pPrChange` carries `CT_TrackChange` attributes; the `w:id` row states "If this attribute is omitted, then the document is non-conformant." A `pPrChange` with no `w:id` must be reported non-conformant, not opens-clean.
- **§refs:** ECMA-376 / ISO 29500-1 §17.13.5.29 (`pPrChange`, id row); §A.1 `CT_Markup` (`w:id` use=required) via `CT_TrackChange`.
- **Classification:** pipeline-bug (validator). The `validate()` surface lacks a required-id check for property-change revisions.
- **What stemma does vs. what Word does:** stemma's `validate()` returns `ok=true` (opens-clean) for a `pPrChange` lacking `w:id`. Assertion failure: `validate() report: ok=true` against `assert_not_opens_clean`. Word treats the omitted required `w:id` as non-conformant. The required-id enforcement (I-TC-002) currently covers only `w:del` / `w:ins`, so property-change revisions slip through.
- **Suggested fix site:** the tracked-change conformance check behind `stemma::api::validate` (I-TC-002 required-id enforcement). Extend the required-`w:id` rule from `w:del`/`w:ins` to the property-change revision elements (`w:pPrChange`, `w:rPrChange`, `w:sectPrChange`, and the table-property change variants `w:tblPrChange` / `w:trPrChange` / `w:tcPrChange`) so a missing `w:id` is reported non-conformant with element + location context.
- **Minimal `bodyXml` repro:**

```xml
<w:p><w:pPr><w:jc w:val="center"/><w:pPrChange w:author="John Doe" w:date="2006-01-01T12:00:00Z"><w:pPr/></w:pPrChange></w:pPr><w:r><w:t>Centered</w:t></w:r></w:p><w:sectPr/>
```

### 2. `w:sectPrChange` missing the required `w:id` opens clean (validator pipeline bug, high confidence)

- **Rule:** `w:sectPrChange` carries `CT_TrackChange`; the `w:id` row states "If this attribute is omitted, then the document is non-conformant." A `sectPrChange` with no `w:id` must be reported non-conformant, not opens-clean.
- **§refs:** ECMA-376 / ISO 29500-1 §17.13.5.32 (`sectPrChange`, id row); §A.1 `CT_Markup` / `CT_TrackChange`.
- **Classification:** pipeline-bug (validator). Same root cause as finding 1 — I-TC-002 does not cover `sectPrChange`.
- **What stemma does vs. what Word does:** stemma's `validate()` returns `ok=true` for a `sectPrChange` lacking `w:id`. Assertion failure: `validate() report: ok=true` against `assert_not_opens_clean`. Word treats the omitted required `w:id` as non-conformant.
- **Suggested fix site:** identical to finding 1 — extending the I-TC-002 required-id rule to property-change revisions closes both at once.
- **Minimal `bodyXml` repro:**

```xml
<w:p><w:r><w:t>Body</w:t></w:r></w:p><w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:sectPrChange w:author="John Doe" w:date="2006-01-01T12:00:00Z"><w:sectPr/></w:sectPrChange></w:sectPr>
```

---

## Regression tests

Each encodes a behavioural constraint stemma already satisfies and must not regress.

| Test | Rule |
| --- | --- |
| `para_mark_rprchange_reject_restores_prior_mark_props` | A paragraph-mark `rPrChange` (in `pPr/rPr`) with a complete prior snapshot opens clean and carries no character content; accept/reject leave run text unchanged (§17.13.5.30, §17.13.3). |
| `rprchange_reject_restores_removed_prior_bold` | A run-level `rPrChange` whose current `rPr` holds only the trailing record plus a non-null prior bold snapshot opens clean; accept/reject change no text (§17.13.5.31). |
| `rprchange_records_removed_formatting_reject_restores_italic` | A run `rPrChange` recording removed italic (empty current `rPr`, prior `rPr` has `w:i`) opens clean; text stays "Plain" through accept/reject (§17.13.5.31, §A.1). |
| `rprchange_literal_explicit_toggle_off_in_prior_snapshot_order` | Current `w:b` precedes `w:rPrChange`, and the prior snapshot orders `w:b` before `w:color`; schema-correct `CT_RPr` ordering opens clean (§17.13.5.31, §A.1 CT_RPr). |
| `pprchange_null_prior_reject_removes_all_current_ppr_props` | `CT_PPrBase` order (spacing before jc) with trailing `pPrChange` and an empty prior `pPr` opens clean; accept/reject leave text unchanged (§17.13.5.29, §A.1). |
| `para_mark_rprchange_multi_toggle_snapshot_order_and_location` | A paragraph-mark `rPrChange` with prior snapshot ordered b→i→color opens clean; the property revision carries no text (§17.13.5.30, §A.1 CT_RPr). |
| `prchange_track_attrs_id_author_preserved_roundtrip` | A `pPrChange` carrying required `w:id` and `w:author` opens clean and round-trips both attributes verbatim (§A.1 CT_Markup/CT_TrackChange, §17.13.5.29). |
| `rprchange_serializes_as_trailing_rpr_child_after_edit_reemit` | `rPrChange` serializes as the trailing child of `rPr`, after current props (`<w:rPr><w:b/>…<w:rPrChange>…</w:rPrChange></w:rPr>`), and opens clean (§17.13.5.31, §17.13.3, §A.1 CT_RPr). |
| `sectprchange_prior_sectpr_word_delta_semantics` | A `sectPr` with `pgSz`, `cols`, and a trailing `sectPrChange` carrying a partial prior `sectPr` opens clean; accept keeps current section and leaves body text unchanged (§17.13.5.32, §A.1). |
| `sectprchange_prior_sectpr_optional_opens_clean` | The inner `sectPr` of `sectPrChange` is `minOccurs=0`, so a `sectPrChange` with no prior-`sectPr` child opens clean (§17.13.5.32, §A.1 CT_SectPrChange). |

---

## Discarded test-bugs

None. No test in this file encoded a wrong expectation; nothing was deleted.

---

## Open questions — pending confirmation against real Word

No confirmed gap in this area requires confirmation against real Word — both findings are validator (`validate()` / I-TC-002) gaps fully decidable against the ECMA-376 schema's required-`w:id` rule.

One existing passing test carries a deferred question that is intentionally not asserted on the read side and would benefit from confirmation against real Word:

| Name | Check against real Word | Expected value | bodyXml |
| --- | --- | --- | --- |
| `sectprchange_prior_sectpr_word_delta_semantics` | reject-resolution (Word section-property delta semantics, MS-OI29500 §2.1.348) | Reject restores the prior `sectPr` (`pgSz` 15840×12240) per Word's section delta semantics; body text stays "Body". | `<w:p><w:r><w:t>Body</w:t></w:r></w:p><w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:cols w:num="2"/><w:sectPrChange w:id="10" w:author="A" w:date="2024-01-01T00:00:00Z"><w:sectPr><w:pgSz w:w="15840" w:h="12240"/></w:sectPr></w:sectPrChange></w:sectPr>` |
