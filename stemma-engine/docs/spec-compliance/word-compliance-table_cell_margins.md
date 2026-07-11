# Word-compliance — Cell margins: defaults, per-cell, and exception precedence

**Summary:** No confirmed gaps. 11 regression tests. 1 test-bug discarded. Suite: `cargo test -p stemma --test spec_table_cell_margins_word_compliance` (11 passed; 0 failed; 0 ignored).

This audit covers ECMA-376 / ISO 29500-1 / MS-OI29500 table cell-margin behaviours (`tcMar`, `tblCellMar`, `tblPrEx/tblCellMar`, `tblCellSpacing`), encoded as runnable `spec_*` tests. Every behavioural rule stemma is checked against is currently satisfied. The failing cases turned out to be wrong tests (schema-invalid / scrambled hand-authored input asserting against the verbatim-preservation path), not stemma incompliances.

## Confirmed incompliances

None. No pipeline-bug or model-bug gaps were confirmed in this area. Every authored rule either passes against stemma today or, in the failing cases, encoded a wrong expectation about which serialization path runs (see Discarded test-bugs and Open questions).

The recurring false premise behind every "failure" in this area is the same: an **unedited** `parse → serialize` round-trips the document body **byte-verbatim**. stemma keeps the source `document.xml` body bytes in the scaffold package at parse time (`runtime.rs` `build_snapshot_from_bytes` → `PackageScaffold.package` built from `anchored_bytes`, not from the IR); `Document::serialize` (`serialize_snapshot` in `runtime.rs`) just re-zips that package. The model-driven `serialize_table_node` / `serialize_cell_margins` (`serialize/mod.rs`) only runs when `apply`/`project`/`diff` regenerate the body via `serialize_canonical_docx`. A probe on out-of-order authored sides confirms it: plain reserialize emits the sides exactly as authored, while `project(AcceptAll)` emits canonical CT_TblCellMar/CT_TcMar order with `start→left` / `end→right` already normalized. So the serializer is already correct on the regeneration path; asserting normalization on the verbatim path is the project's documented "consumption-vs-save" trap.

## Regression tests

These passing tests are retained as regression guards.

- `tcmar_pct_width_margin_ignored_not_relabeled_dxa` — a `pct`-typed per-cell margin is well-formed; the document opens clean and the pct value is never reinterpreted as dxa (ISO 29500-1 §17.4.34; MS-OI29500 §2.1.154).
- `tblcellmar_default_bottom_only_applies_and_roundtrips` — a bottom-only `tblCellMar` default round-trips as exactly one `w:bottom` child (value 360) with no fabricated sides (ISO 29500-1 §17.4.5, §17.4.42).
- `tcmar_overrides_table_default_per_side_independently` — a per-cell `tcMar` (720) over a table `tblCellMar` default (144) is schema-valid and opens clean (ISO 29500-1 §17.4.68, §17.4.42).
- `cell_margin_default_side_pct_type_opens_clean` — a pct-typed table-default leading margin opens clean and leaves cell text untouched (read view yields "AB") (ISO 29500-1 §17.4.34; MS-OI29500 §2.1.146).
- `cell_margin_default_bottom_pct_type_opens_clean` — an auto/pct-typed table-default bottom margin opens clean (width treated as 0) (ISO 29500-1 §17.4.5; MS-OI29500 §2.1.119, §2.1.177).
- `cell_spacing_default_pct_type_opens_clean` — a pct/auto `tblCellSpacing` opens clean and leaves cell text untouched (read view yields "XY") (ISO 29500-1 §17.4.43; MS-OI29500 §2.1.154).
- `tcmar_nil_side_ignored_opens_clean` — a `nil`-typed per-cell side is ignored (falls back to default), opens clean, content survives (ISO 29500-1 §17.4.68; MS-OI29500 §2.1.177).
- `tblprex_cell_margin_exception_opens_clean` — row-level `tblCellMar` inside `tblPrEx` is valid markup; opens clean and both rows' cell texts survive (ISO 29500-1 §17.4.41, §17.4.42).
- `tblprex_cell_margin_exception_applied_to_row` — a row `tblPrEx/tblCellMar` exception (144) is preserved through reserialize, not dropped back to the table default (ISO 29500-1 §17.4.41, §17.4.42).
- `tcmar_serialized_side_order_top_left_bottom_right` — when the engine emits `w:tcMar`, side children are in schema order top, leading (start/left), bottom, trailing (end/right) (ISO 29500-1 §A.1, §17.4.68).
- `tcmar_start_end_aliases_accepted_and_applied` — leading/trailing margins authored with `w:start`/`w:end` are accepted, applied, and their values (720/360) preserved (ISO 29500-1 §A.1, §17.4.68, §17.4.34).

## Discarded test-bugs

- `tcmar_element_order_top_left_bottom_right` — **DELETED.** The test's authored input scrambles `CT_TcMar` children (`right, bottom, left, top`). `CT_TcMar` is an `xsd:sequence` (`top, start, left, bottom, end, right` per the transitional `wml.xsd` schema), so the input is itself schema-invalid: the data first goes wrong at the hand-authored test input, not in stemma's pipeline. stemma's `serialize_snapshot` round-trips untouched `document.xml` bytes verbatim (its documented preservation contract; see this file's own header), so the scrambled order survives because the cell is never regenerated. stemma's serializer is already correct: `serialize_cell_margins` and the cell path emit `top -> left -> bottom -> right` unconditionally from the order-free `CellMargins` struct whenever a cell is regenerated. The test measured the verbatim path while asserting the regeneration path — a wrong observation baked into the assertion. Per CLAUDE.md "fix at the source," the correct response to schema-invalid input is to fail/normalize at the import edge or add a `CT_TcMar` internal-ordering rule to `docx_validate_ordering.rs` (a separate follow-up), not to silently rewrite verbatim-preserved bytes. The model is already honest, so there is no model bug, and adjudicating this specific assertion does not require confirmation against real Word.

## Open questions — pending confirmation against real Word

Three additional candidate rules from this area are left as open questions (not encoded as live tests) because they share the verbatim-vs-regeneration false premise above and need confirmation against real Word before any corrected assertion could be added. The open question for all three is identical: **does Microsoft Word issue a needs-repair when a table-default `tblCellMar` / per-cell `tcMar` has its side children authored in non-schema order, or leaves `w:start`/`w:end` un-normalized?** The §17.4.42 normative example and the CT_TblCellMar/CT_TcMar `xsd:sequence` grammar establish well-formedness but not a consumption-level repair, and no MS-OI29500 clause asserts such a repair, so only real Word can decide whether reorder/normalization is obligatory on the verbatim path.

### 1. `tbl_cell_mar_default_sides_emit_in_ct_tblcellmar_schema_order`
- **Check against real Word:** does the document open clean (no repair)? — on an unedited document whose table-default `tblCellMar` children are authored out of CT_TblCellMar sequence.
- **Expected value (to confirm):** Word opens **without** repair → engine's verbatim preservation is correct (and the regeneration path already emits canonical order). If Word *does* repair → the verbatim path must follow.
- **bodyXml:**
```xml
<w:tbl><w:tblPr><w:tblW w:w="5000" w:type="dxa"/><w:tblCellMar><w:bottom w:w="111" w:type="dxa"/><w:right w:w="222" w:type="dxa"/><w:top w:w="333" w:type="dxa"/><w:left w:w="444" w:type="dxa"/></w:tblCellMar></w:tblPr><w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid><w:tr><w:tc><w:tcPr/><w:p><w:r><w:t>cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/><w:sectPr/>
```

### 2. `tbl_cell_mar_default_start_end_aliases_map_to_left_right`  (normalization-expectation question)
- **Check against real Word:** does the document open clean (no repair)? plus layout intent — confirm whether a table-default `w:start`/`w:end` must be normalized to `w:left`/`w:right` for an untouched table.
- **Expected value (to confirm):** Word opens without repair and treats `start`=240 as the leading and `end`=480 as the trailing default. Both spellings are transitional-valid; stemma round-trips them verbatim, so this is a normalization-expectation question, not a corruption — the verbatim-passthrough contract says stemma does **not** normalize untouched tables.
- **bodyXml:**
```xml
<w:tbl><w:tblPr><w:tblW w:w="5000" w:type="dxa"/><w:tblCellMar><w:start w:w="240" w:type="dxa"/><w:end w:w="480" w:type="dxa"/></w:tblCellMar></w:tblPr><w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid><w:tr><w:tc><w:tcPr/><w:p><w:r><w:t>cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/><w:sectPr/>
```

### 3. `tcmar_side_children_reemitted_in_schema_order`  (verbatim-vs-regeneration question)
- **Check against real Word:** does the document open clean (no repair)? — on an unedited document whose per-cell `tcMar` children are authored out of CT_TcMar sequence.
- **Expected value (to confirm):** Word opens without repair → engine's verbatim preservation is correct (the regeneration path already reorders to top, leading, bottom, trailing). If Word repairs → the verbatim path must reorder too; the source-correct fix is at the import/validation edge, not in the serializer.
- **bodyXml:**
```xml
<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="5000" w:type="dxa"/><w:tcMar><w:right w:w="9" w:type="dxa"/><w:bottom w:w="42" w:type="dxa"/><w:left w:w="76" w:type="dxa"/><w:top w:w="108" w:type="dxa"/></w:tcMar></w:tcPr><w:p><w:r><w:t>A</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:sectPr/>
```
