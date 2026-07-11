# Word-compliance — tblPrEx exceptions, bidiVisual, shading, alignment, and cell-content properties

No confirmed gaps. 12 regression tests. No test-bugs.

## Confirmed incompliances

None. This audit covers row-level table-property exceptions (`tblPrEx` child ordering and the trailing `tblPrExChange`), table/row/cell shading (`shd`, including `themeFill` retention and `val="nil"` no-shading consumption), cell alignment and content properties (`vAlign`, `textDirection`), and the table-level `tblPr` shading slot — checking both schema-correct serialization order (CT_TblPrExBase / CT_TblPrBase) and Word's display-only consumption semantics on the accepted/rejected read views. The stemma engine matches Word on every probed constraint.

## Regression tests

- `tblprex_jc_before_shd_ct_tblprexbase_order` — inside `tblPrEx`, `jc` (CT_TblPrExBase pos 2) must serialize before `shd` (pos 6) and the doc opens clean (ISO 29500-1 §17.4.30/§17.4.60).
- `tblprexchange_serializes_last_in_tblprex` — `tblPrExChange` is appended after the base property children, so live `tblBorders` serializes before the trailing `tblPrExChange` (ISO 29500-1 §17.4.61).
- `table_cell_shd_themefill_retained_roundtrip` — a cell `shd` with `themeFill` (which supersedes the literal `fill`) must be retained on round-trip or the cell color changes (ISO 29500-1 §17.4.32/§17.3.5).
- `table_level_shd_nil_ignored_text_survives` — table-level `shd val="nil"` is display-only; accepted and rejected read text both equal the authored text (ISO 29500-1 §17.4.31/§17.18.78).
- `cell_valign_explicit_top_opens_clean` — `vAlign val="top"` is an allowed cell value, opens clean, and is display-only so it does not alter cell text (MS-OI29500 §2.1.181; ISO 29500-1 §17.4.83/§17.18.101).
- `cell_shd_nil_ignored_as_no_shading` — a cell `shd val="nil"` is consumed by Word as no-shading and must never alter the cell's text content (MS-OI29500 §2.1.553; ISO 29500-1 §17.18.78/§17.4.32).
- `tblprex_shd_after_tblborders_order` — CT_TblPrExBase mandates `tblBorders` before `shd`; serialized `tblPrEx` keeps that order and opens without repair (ISO 29500-1 §A.1 CT_TblPrExBase/§17.4.60/§17.4.30).
- `cell_textdirection_tbrl_valid_and_preserved` — `textDirection val="tbRl"` is Word-respected and layout-only; the stored cell text is preserved (MS-OI29500 §2.1.558; ISO 29500-1 §17.4.72/§17.18.93).
- `row_shd_nil_ignored_as_no_shading` — a row-level `shd val="nil"` in `tblPrEx` is consumed as no-shading and leaves the row's text content unchanged (MS-OI29500 §2.1.553; ISO 29500-1 §17.18.78/§17.4.30).
- `tblprex_internal_child_order_jc_shd_cellmar_look` — CT_TblPrExBase is an ordered sequence `jc → shd → tblCellMar → tblLook`; the serializer preserves that order and Word opens it without repair (ISO 29500-1 §17.4.60).
- `table_level_shd_superseded_by_cell_shd_is_consumption` — table `shd` is superseded background only and carries no content; accept and reject both yield exactly the authored cell text (ISO 29500-1 §17.4.31/§17.4.32).
- `tblpr_shd_serializes_after_tblborders_before_tbllayout` — CT_TblPrBase orders `tblBorders → shd → tblLayout`; the serializer preserves that order and the `tblPr` opens without repair (ECMA-376 Annex A CT_TblPrBase; ISO 29500-1 §17.4.59/§17.4.31).

## Discarded test-bugs

None.

## Open questions — pending confirmation against real Word

None.
