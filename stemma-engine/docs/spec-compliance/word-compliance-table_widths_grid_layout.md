# Word-compliance — Table widths, grid, indent, and layout algorithm

**Summary:** 2 confirmed gaps (both open pending fix-site adjudication and confirmation against real Word), 10 regression tests, 0 test-bugs discarded. Suite: `cargo test -p stemma --test spec_table_widths_grid_layout_word_compliance -- --test-threads=1` (10 passed; 0 failed; 2 ignored).

The two gaps are the same defect (percent-form `CT_TblWidth` width rejected at parse) surfacing through two distinct elements (`tcW`, `tblW`). They are held open rather than fully closed because the evidence is a stemma-side parse failure that is conclusively stricter than Word per the schema, but the correct fix site (width-value parser vs. import edge tolerance) still needs adjudication, and confirming the percent-form examples against real Word would harden the spec before the engine fix lands.

## Confirmed incompliances

Ranked: pipeline-bug > model-bug, high > low confidence.

### 1. Percent-form `tcW` width fails the whole parse (pipeline-bug, high confidence)

- **Rule:** A cell preferred width `w:tcW` is a `CT_TblWidth` whose `@w` is `ST_MeasurementOrPercent`, which admits a percent literal (e.g. `33.3%`). The literal §17.4.71 normative example uses `w:w="33.3%"`. Word opens such a cell at one-third width with no repair prompt.
- **§refs:** §17.4.71 (CT_TblWidth / tcW), §17.18.107 (ST_MeasurementOrPercent), §17.18.11 (ST_DecimalNumberOrPercent context).
- **Classification:** pipeline-bug. The width-value parser rejects the percent token and propagates `RuntimeError { code: InvalidDocx }` all the way out of `Document::parse`, so stemma cannot even open a schema-valid document.
- **What stemma does vs. what Word does:** Word reads the percent and lays the cell out. stemma aborts the entire parse: `parse: RuntimeError { code: InvalidDocx, message: "invalid width value '33.3%' in tcW element" }`. The failure surfaces via `reserialize()`'s `Document::parse(...).expect("parse")` before any assertion body runs.
- **Suggested fix site:** the `CT_TblWidth` `@w` value parser (the width-token parser shared by `tcW`/`tblW`/`tblInd`/`wBefore`/`wAfter`). It must accept the percent form of `ST_MeasurementOrPercent` on the parse path, not only the validate path (the validate path already tolerates it — see the passing `tcw_decimal_percent_opens_clean`). Refusing a schema-valid width by failing `Document::parse` is stricter than Word and violates the no-silent-corruption / "stricter than Word is a bug" contract.
- **Minimal bodyXml repro:**

```xml
<w:tbl><w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="33.3%" w:type="pct"/></w:tcPr><w:p><w:r><w:t>A</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:sectPr/>
```

### 2. Percent-form `tblW` width fails the whole parse (pipeline-bug, high confidence)

- **Rule:** A table preferred width `w:tblW` is a `CT_TblWidth` whose `@w` is `ST_MeasurementOrPercent`; the §17.4.87 normative example uses `w:w="100%"` with `type="pct"`. Word opens a full-width table with no repair.
- **§refs:** §17.4.63 (tblW), §17.4.87 (CT_TblWidth), §17.18.107 (ST_MeasurementOrPercent).
- **Classification:** pipeline-bug. Same parser defect as #1, surfaced via `tblW`.
- **What stemma does vs. what Word does:** Word reads `100%` and renders full width. stemma aborts the parse: `parse: RuntimeError { code: InvalidDocx, message: "invalid width value '100%' in tblW element" }`. The failure surfaces at `reserialize()`'s `Document::parse(...).expect("parse")`.
- **Suggested fix site:** identical to #1 — the shared `CT_TblWidth` `@w` width-token parser. Fixing one fixes both.
- **Minimal bodyXml repro:**

```xml
<w:tbl><w:tblPr><w:tblW w:w="100%" w:type="pct"/></w:tblPr><w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>A</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:sectPr/>
```

## Regression tests

These currently pass (they guard behaviour stemma already gets right).

- `gridcol_universal_measure_opens_clean` — `gridCol/@w="1in"` is a valid ST_TwipsMeasure (union admits a universal measure); validate must be clean (§17.4.16, §22.9.2.14).
- `tcw_decimal_percent_opens_clean` — `tcW/@w="33.3%"` with `type="pct"` validates clean on the validate path (§17.4.71, §17.18.107).
- `ct_tbl_emits_tblpr_before_tblgrid_before_tr` — serializer emits `CT_Tbl` children in the Annex A order `tblPr → tblGrid → tr` (§17.4.48, §17.4.60).
- `tblind_universal_measure_opens_clean` — `tblInd/@w="1in"` via CT_TblWidth (universal measure) opens clean (§17.4.50, §17.4.87).
- `wbefore_survives_reserialization` — `wBefore` (and its paired `gridBefore`) round-trip rather than being dropped (§17.4.86, §17.4.14).
- `gridcol_omitted_width_opens_clean` — an omitted `gridCol/@w` is a defined zero-width state and validates clean (§17.4.16, §17.4.48).
- `tcw_contradictory_type_value_opens_clean` — `tcW type="dxa" w="50%"` (contradictory) opens clean by ignoring the type, not by treating the file as corrupt (§17.4.87, §17.4.71).
- `wafter_pct_width_opens_clean` — `wAfter` is a CT_TblWidth, so a `pct` width `"25%"` validates clean (§17.4.82, §17.4.87).
- `gridcol_sum_neq_tblw_opens_clean` — gridCol-sum vs `tblW` mismatch is a valid preference conflict resolved at layout, never repair (§17.4.63, §17.4.16).
- `tcw_percent_literal_with_contradictory_dxa_type_opens_clean` — percent literal `w="50%"` with contradictory `type="dxa"` validates clean (type ignored, percent literal admitted) (§17.4.87, §17.18.107).

## Discarded test-bugs

None. No test encoded a wrong expectation; both failures are real Word-compliance gaps, kept (ignored) as open questions rather than deleted.

## Open questions — pending confirmation against real Word

Neither gap strictly requires confirmation against real Word (the schema itself, §17.18.107, settles that the percent form is admissible, and the §17.4.71 / §17.4.87 normative examples use the exact literals). They are listed here so the percent-form open behaviour can optionally be confirmed against real Word before the parser fix lands.

| name | check against real Word | expected | bodyXml |
| --- | --- | --- | --- |
| `tcw_percent_form_width_round_trips` | open-clean | opens without repair; cell laid out at one-third width | `<w:tbl><w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="33.3%" w:type="pct"/></w:tcPr><w:p><w:r><w:t>A</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:sectPr/>` |
| `tblw_percent_form_width_round_trips` | open-clean | opens without repair; table laid out at full (100%) width | `<w:tbl><w:tblPr><w:tblW w:w="100%" w:type="pct"/></w:tblPr><w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>A</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:sectPr/>` |
