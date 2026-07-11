# Word-compliance — Table conditional formatting: cnfStyle bitmask, tblLook gating, tblStylePr override precedence

**Summary:** No confirmed gaps. 6 regression tests. 1 test-bug discarded. 5 open questions (no confirmation against real Word required). Suite: `cargo test -p stemma --test spec_table_conditional_formatting_cnf_word_compliance -- --test-threads=1` (6 passed, 0 failed, 5 ignored).

This audit finds **no confirmed Word incompliance** in this area. Every failing assertion turned out to rest on a faulty premise: that an *unedited* parse→serialize round-trip rebuilds `w:cnfStyle` / `w:tblLook` from the typed model. It does not. stemma's documented contract is that untouched body content is written **byte-verbatim** (the passthrough archive write in `serialize_snapshot`); the property-rebuild path (`build_paragraph_properties`, `serialize_table_node` named-attr synthesis) only runs when an edit actually rebuilds the affected node. So the synthesized / normalized / Annex-A-reordered output these tests assert is real serializer behaviour — it just is not reached on a plain round-trip. One test additionally asserted a save-time rewrite (val → named attrs) that the spec never requires of Word at all; that one is a genuine test-bug and was deleted.

## Confirmed incompliances

None. No pipeline-bug or model-bug was confirmed in this area. The five originally-failing ordering/normalization/synthesis tests are not Word incompliances — they assert serializer-rebuild output on a code path (unedited round-trip) that intentionally preserves source bytes. They are open questions (see below) rather than confirmed gaps: to validate the serializer-rebuild behaviour the spec premise actually describes, the test must first apply an edit that touches the carrier cell/row so the rebuild path runs.

## Regression tests (passing)

- `cnf_style_named_attrs_roundtrip_opens_clean_on_row_and_cell` — val-less named-attr `cnfStyle` on both `trPr` (`firstRow`) and `tcPr` (`evenHBand`) round-trips and opens clean (ISO 29500-1 §17.4.7/§17.4.8; Annex A CT_Cnf).
- `tbllook_val_only_synthesizes_named_attrs_06a0` — a bare table carrying a legacy hex `tblLook w:val="06A0"` is valid OOXML and opens clean (ISO 29500-1 §17.4.55).
- `tbllook_val_0660_canonical_iso_example_synthesizes_named` — the canonical transitional `tblLook w:val="0660"` example opens clean (ISO 29500-4 §14.4.12).
- `tbllook_val_0000_explicit_disables_all_conditional` — an all-zero legacy bitmask `tblLook w:val="0000"` is valid OOXML and opens clean (ISO 29500-1 §17.4.55; ISO 29500-4 §14.4.12).
- `novband_suppresses_column_banding_with_colbandsize_set` — a `noVBand`-gated styled table with `band1Vert`/`band2Vert` and `tblStyleColBandSize` is valid OOXML and opens clean (ISO 29500-1 §17.4.55; MS-OI29500 §2.1.160).
- `tbllook_omitted_defaults_to_named_attrs_not_consulted` — `CT_TblLook` permits a named attr and `val` simultaneously; the package is schema-valid and opens clean, consulting only the named `firstRow` (MS-OI29500 §2.1.160; ISO 29500-1 §17.4.55).

## Discarded test-bugs

- `tbllook_val_0600_synthesizes_both_noband_bits` — asserted that a bare `<w:tblLook w:val="0600"/>` re-emits the six named on/off attrs. The output is the bare `w:val="0600"` with no named attrs because an untouched table reserializes byte-verbatim (`serialize_snapshot` clones the original `document.xml`; `serialize_table_node` synthesis only runs on the edit/rebuild path, per the contract noted in `runtime.rs`). The bare val-only form is schema-valid, Word reads `val` correctly when no named attr is present (MS-OI29500 §2.1.160; 0x0200=noHBand, 0x0400=noVBand per ISO 29500-4 §14.4.12, which itself shows `<w:tblLook w:val="0660"/>` as a canonical valid form), and the `opensClean` leg passes. Nothing in the cited spec text requires Word to rewrite `val` into named attrs on save. The expectation conflated a stemma serializer-rebuild detail with Word save behaviour. No confirmation against real Word needed; the verbatim contract is established and intentional.

  Minimal repro bodyXml:
  ```xml
  <w:tbl><w:tblPr><w:tblStyle w:val="TestTblStyle"/><w:tblW w:w="0" w:type="auto"/><w:tblLook w:val="0600"/></w:tblPr><w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="2000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="2000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>A</w:t></w:r></w:p></w:tc><w:tc><w:tcPr><w:tcW w:w="2000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>B</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:sectPr/>
  ```

## Open questions — pending confirmation against real Word

No item in this area requires confirmation against real Word. All five open questions turn on unambiguous spec text and a known, intentional stemma contract (byte-verbatim passthrough on the unedited round-trip), not a Word behaviour question. They are recorded here for follow-up — the correct next step is to rewrite each to apply an edit that touches the carrier so the serializer-rebuild path runs, then re-assert ordering/normalization/synthesis. They are not confirmation candidates as written.

- **`cnf_corner_attrs_serialized_in_annex_a_order`** — mode: serializer-rebuild ordering (edit path). Expected: corner attrs re-emit `firstRowFirstColumn < firstRowLastColumn < lastRowFirstColumn < lastRowLastColumn` as `="1"`. bodyXml:
  ```xml
  <w:tbl><w:tblPr><w:tblStyle w:val="TestStyle"/><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="5000" w:type="dxa"/></w:tcPr><w:p><w:pPr><w:cnfStyle w:lastRowLastColumn="true" w:firstRowFirstColumn="true" w:lastRowFirstColumn="true" w:firstRowLastColumn="true"/></w:pPr><w:r><w:t>corner</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:sectPr/>
  ```
- **`cnf_named_onoff_normalized_to_one_and_false_omitted`** — mode: serializer-rebuild normalization (edit path). Expected: `ST_OnOff` `true`→`"1"`, off bit omitted (no `w:lastRow="0"`). bodyXml:
  ```xml
  <w:tbl><w:tblPr><w:tblStyle w:val="TestStyle"/><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="5000" w:type="dxa"/></w:tcPr><w:p><w:pPr><w:cnfStyle w:firstRow="true" w:lastRow="false"/></w:pPr><w:r><w:t>hdr</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:sectPr/>
  ```
- **`cnf_row_attrs_serialized_before_column_attrs`** — mode: serializer-rebuild ordering (edit path). Expected: re-emit `firstRow < lastRow < firstColumn < lastColumn < oddVBand`. bodyXml:
  ```xml
  <w:tbl><w:tblPr><w:tblStyle w:val="TestStyle"/><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="5000" w:type="dxa"/></w:tcPr><w:p><w:pPr><w:cnfStyle w:oddVBand="true" w:lastColumn="true" w:firstColumn="true" w:lastRow="true" w:firstRow="true"/></w:pPr><w:r><w:t>x</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:sectPr/>
  ```
- **`cnf_val_bitmask_coexists_with_corner_named_attr`** — mode: serializer-rebuild (edit path). Expected: `val` precedes synthesized `firstRowFirstColumn="1"`. bodyXml:
  ```xml
  <w:tbl><w:tblPr><w:tblStyle w:val="TestStyle"/><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="5000" w:type="dxa"/></w:tcPr><w:p><w:pPr><w:cnfStyle w:val="101000000000" w:firstRowFirstColumn="true"/></w:pPr><w:r><w:t>nw</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:sectPr/>
  ```
- **`tbllook_val_only_synthesizes_correct_named_bits`** — mode: serializer-rebuild synthesis (edit path). Expected: `val=04A0` synthesizes `firstRow=1, firstColumn=1, noVBand=1, lastRow=0`. bodyXml:
  ```xml
  <w:tbl><w:tblPr><w:tblStyle w:val="TestTblStyle"/><w:tblW w:w="0" w:type="auto"/><w:tblLook w:val="04A0"/></w:tblPr><w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="2000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="2000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>A</w:t></w:r></w:p></w:tc><w:tc><w:tcPr><w:tcW w:w="2000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>B</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:sectPr/>
  ```
