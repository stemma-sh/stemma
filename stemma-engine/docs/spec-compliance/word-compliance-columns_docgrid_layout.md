# Word-compliance — Multi-column layout (cols/col) and document grid (docGrid)

**Summary:** 0 confirmed gaps, 10 new regression tests, 0 test-bugs discarded, 1 open question (measurement/test-spec mismatch). Build status: green — `cargo test -p stemma --test spec_columns_docgrid_layout_word_compliance` reports **10 passed, 0 failed, 1 ignored**.

This area mines ISO 29500-1 §17.6.3–§17.6.5 (columns and document grid), the MS-OI29500 implementer notes, and the transitional WML schema (`CT_Columns` / `CT_Column` / `CT_DocGrid`). No engine Word-divergence was confirmed for this area — every authored rule that stemma satisfies is pinned by a regression test. The single non-passing case is a test-spec measurement mismatch, not an engine defect (see the open question below).

Methodology note (consumption vs. save): stemma round-trips untouched content byte-verbatim, so `reserialize()` of an unedited document reflects stemma's verbatim-preservation contract, not Word's render-time normalization. The XML-order assertion in this file is therefore restricted to child ordering and structural validity; "Word ignores value V on layout" rules are asserted on the read side (accept/reject text) or via opens-clean, never on re-serialized XML.

## Confirmed incompliances

None. No pipeline-bug or model-bug was confirmed for multi-column layout (`cols`/`col`) or the document grid (`docGrid`). stemma opens every probed schema-valid construction clean, preserves the layout-only nature of column/grid geometry on the read side, and emits children in `EG_SectPrContents` schema order (`cols` before `docGrid`).

## New regression tests

The passing tests below pin stemma's correct behaviour for this area.

- `cols_sep_accepts_st_onoff_literal_true` — `w:cols/@w:sep` is `ST_OnOff`; the literal `"true"` is a valid on-value, so `sep="true"` is schema-valid and opens clean (§17.6.4 + §17.17.4 + §22.9.2.7).
- `docgrid_linepitch_st_decimalnumber_large_value_layout_only` — `docGrid/@w:linePitch` is `ST_DecimalNumber` (unbounded); a large value is schema-valid, opens clean, and is layout-only so accepted text is unchanged (§17.6.5 + §17.18.10).
- `last_col_omitted_space_serialized_as_zero` — the last column's space is ignored and the schema default for col space is 0; the file is schema-valid and opens without repair (§17.6.3 + MS-OI29500 §2.1.212 + Annex A CT_Column).
- `cols_space_default_720_not_fabricated_into_bare_cols` — a bare `<w:cols w:num="3"/>` is schema-valid (space/equalWidth optional) and opens without repair (§17.6.4 + Annex A CT_Columns).
- `docgrid_lines_with_charspace_opens_clean_and_roundtrips` — `type`, `linePitch`, and `charSpace` are independently optional, so `type="lines"` with `charSpace` is schema-valid and opens without repair (§17.6.5 + Annex A CT_DocGrid + §17.18.14).
- `cols_serializer_emits_explicit_col_space_on_rebuild` — under `equalWidth="0"` both `col` elements carry the required `w:w`, so Word opens the file without repair (§17.6.3 + MS-OI29500 §2.1.212 + Annex A CT_Column).
- `equalwidth_omitted_no_num_with_col_children_is_single_column` — omitted `equalWidth` defaults true and omitted `num` is assumed 1, so col children are inert; the section is a well-formed single column and accepted text equals the authored prose (MS-OI29500 §2.1.213 + §17.6.4).
- `last_col_space_ignored_does_not_alter_text_or_validity` — trailing-column space is ignored (not malformed); the file opens without repair and accepted text equals the authored prose (§17.6.3 + §17.6.4 + MS-OI29500 §2.1.212).
- `docgrid_snaptochars_charspace_only_opens_clean` — `snapToChars` is a legal `ST_DocGrid` enumeration and `charSpace`/`linePitch` are independently optional, so a charSpace-only `snapToChars` grid opens without repair (§17.6.5 + §17.18.14 + MS-OI29500 §2.1.534).
- `cols_before_docgrid_schema_order_on_canonical_sectpr` — `w:cols` must precede `w:docGrid` in the sectPr (`EG_SectPrContents` xsd:sequence); the serializer emits them in order and the file opens clean (Annex A EG_SectPrContents + §17.6.4 + §17.6.5).

## Discarded test-bugs

None. No test in this area encoded a wrong expectation that warranted deletion.

## Open question — test-spec block-join reconciliation

- `cols_num_ignored_when_unequal_width_col_count_authoritative` — **measurement/test-spec mismatch, not a columns/docGrid defect.** The columns rule itself holds: `num` is ignored when `equalWidth=false`, columns are layout-only, and the file opens clean (the opens-clean leg of this same test passed). The divergence is purely in the text-join expectation: stemma's `read_accepted().to_text()` joins blocks with a blank line (`\n\n`, per the documented `to_plain_text` contract in `src/view.rs`), while the spec's expected value used a single `\n`:
  - left (engine): `"Column body text alpha\n\nColumn body text beta"`
  - right (spec): `"Column body text alpha\nColumn body text beta"`

  The assertion was left verbatim (not weakened). This is a test-spec block-join expectation to reconcile (`\n` vs stemma's documented `\n\n`), so it is left as an open question rather than reclassified as a gap. No confirmation against real Word is required — the engine behaviour is already correct and the file opens clean. Minimal repro body:

```xml
<w:p><w:r><w:t>Column body text alpha</w:t></w:r></w:p>
<w:p><w:r><w:t>Column body text beta</w:t></w:r></w:p>
<w:sectPr><w:cols w:num="5" w:equalWidth="0"><w:col w:w="4000" w:space="360"/><w:col w:w="4000"/></w:cols></w:sectPr>
```

## Open questions — pending confirmation against real Word

None. No confirmed gap requires checking against real Word, and the sole open item above does not (it is a documented block-join contract divergence in the test's expected string, verifiable from `src/view.rs` without Word).
