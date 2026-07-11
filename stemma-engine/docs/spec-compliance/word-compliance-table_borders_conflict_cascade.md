# Word-compliance — Table & cell borders: conflict resolution and inside-edge cascade

**Summary:** 1 confirmed gap, 7 regression tests, 2 test-bugs discarded, 2 open questions pending confirmation against real Word. Suite: `cargo test -p stemma --test spec_table_borders_conflict_cascade_word_compliance` (7 passed, 0 failed, 3 ignored).

## Confirmed incompliances

Ranked: pipeline-bug > model-bug, high > low confidence.

### 1. Zero-spacing shared-edge border-conflict uses the wrong MS border-number weight table (pipeline-bug, high confidence)

- **Test:** `adjacent_shared_edge_dotdash_uses_ms_border_number_eight`
- **Rule:** At a zero-spacing shared edge, Word weights `dotDash` with border-number 8 (the MS-OI table), so `dotDash sz=8` (weight 64) beats `single sz=60` (weight 60) and the resolved edge on **both** cells becomes the `dotDash` red border.
- **§refs:** MS-OI29500 §2.1.169; ISO 29500-1 §17.4.66 rule 3.
- **Classification:** pipeline-bug.
- **What stemma does vs. what Word does:** The test asserts "both sides of the resolved shared edge must show the single winner; the right cell's left edge becomes dotDash red." Reserialized XML kept the right cell's left edge as `single/sz=60/0000FF` unchanged: no winner-resolution. The `dotDash` border (MS border-number 8, weight 64 > single weight 60) winner was never materialized onto the right cell's left edge. Root cause: `border_weight` in `stemma-engine/src/import.rs` (the `border_number` match table) hard-codes `dotDash` to 4 with the whole tail running sequentially `4..23`. Word applies the MS-OI29500 §2.1.169 numbering instead: `dotDash=8, dotDotDash=9, triple=10, thinThickSmallGap=11, thickThinSmallGap=12, thinThickThinSmallGap=13, thinThickMediumGap=14, thickThinMediumGap=15, thinThickThinMediumGap=16, thinThickLargeGap=17, thickThinLargeGap=18, thinThickThinLargeGap=19, wave=20, doubleWave=21, dashSmallGap=22, dashDotStroked=23, threeDEmboss=24, threeDEngrave=25, outset=26, inset=27` (single=1, thick=2, double=3 stay; Dotted/Dashed remain special-cased to weight 1; None/Nil to 0).
- **Suggested fix site:** `stemma-engine/src/import.rs` — `fn border_weight` (the `border_number` match table). The resolution/materialization machinery in `resolve_adjacent_cell_border_conflicts` already writes the winner onto both edges correctly; only the weight table is wrong.
- **Minimal bodyXml repro:**

```xml
<w:tbl><w:tblPr><w:tblW w:w="5000" w:type="dxa"/></w:tblPr><w:tblGrid><w:gridCol w:w="2500"/><w:gridCol w:w="2500"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="2500" w:type="dxa"/><w:tcBorders><w:right w:val="dotDash" w:sz="8" w:space="0" w:color="FF0000"/></w:tcBorders></w:tcPr><w:p><w:r><w:t>L</w:t></w:r></w:p></w:tc><w:tc><w:tcPr><w:tcW w:w="2500" w:type="dxa"/><w:tcBorders><w:left w:val="single" w:sz="60" w:space="0" w:color="0000FF"/></w:tcBorders></w:tcPr><w:p><w:r><w:t>R</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:sectPr/>
```

## Regression tests

The passing tests (kept active, they encode the verbatim-preservation contract and valid round-trips):

- `tl2br_diagonal_cell_border_survives_roundtrip` — the `tl2br` diagonal cell border (one of CT_TcBorders' eight edges) must survive round-trip; dropping it is content loss Word never performs.
- `br2tl_diagonal_cell_border_survives_roundtrip` — `br2tl` (one of the eight explicit cell-border edges) must be re-emitted, not dropped.
- `tblprex_row_exception_borders_survive_roundtrip` — `tblPrEx` row-exception borders (a distinct cascade tier) must survive round-trip rather than dropping the row override.
- `nonzero_cell_spacing_preserves_both_opposing_cell_borders` — with non-zero `tblCellSpacing`, both opposing per-cell borders are preserved (no winner-selection erases the weaker border).
- `cell_diagonal_tl2br_border_preserved_on_roundtrip` — a `tl2br` diagonal keeps its style (`double`) and color (`FF0000`) intact on round-trip.
- `cell_diagonal_tr2bl_border_preserved_on_roundtrip` — the `tr2bl` anti-diagonal cell border survives re-serialization.
- `logical_start_end_cell_borders_emitted_as_left_right` — the `start`/`left` logical edge border survives re-serialization (as `left` after normalization, or preserved as `start`), never dropped.

## Discarded test-bugs

- `adjacent_shared_edge_triple_uses_ms_border_number_ten` — Asserted that a plain parse→serialize round-trip of an UNEDITED table materializes a single resolved shared-edge winner onto BOTH cells. MS-OI29500 §2.1.169 / ISO 29500-1 §17.4.66 describe border-conflict resolution exclusively in terms of DISPLAY ("shall be displayed" / "displayed over the alternative border"); the standard never says Word rewrites saved markup to collapse the two opposing edges into the winner. Word keeps both cells' distinct `tcBorders` in the file and resolves only at paint time, so a Word round-trip preserves left-cell `right=triple/FF0000` and right-cell `left=double/0000FF` — exactly what stemma emitted. `serialize_snapshot` re-archives the parsed-then-rewritten original `document.xml`; it does not re-render the body from the typed IR, and `serialize_table_node` / `resolve_adjacent_cell_border_conflicts` only feed the diff/redline/edit emission path, never the plain round-trip. The failing assertion encodes behavior Word does not perform. (The genuinely divergent `border_weight` table is real but is exercised by the dotDash gap above, not by this round-trip surface.)
- `no_tcborders_cell_inherits_from_tblprex_exceptions` — Conflated a consumption/rendering rule with a serialization rule. ISO 29500-1 §17.4.39 ("the table-level exception border shall be DISPLAYED") and MS-OI29500 §2.1.169 ("Word DETERMINES … border for each cell … for RENDERING") are display/compute rules, not save rules; neither says Word rewrites `tcBorders` or deletes the table-level `tblBorders` on save. For this unedited doc stemma round-trips the body byte-verbatim — `tblPrEx` (sz=24 red) survives, base `tblBorders` (sz=4 black) survives, the empty cell keeps an empty `tcPr`. The `xmlOmits w:sz="4"` assertion demanded stemma DELETE the table-level base borders, which Word never does (they still govern other rows/non-overridden edges; deleting them is genuine content loss). The sibling `tblprex_row_exception_borders_survive_roundtrip` passes and correctly encodes the contract. Not a pipeline/model gap.

## Open questions — pending confirmation against real Word

The confirmed gap and two open items below are pending confirmation against real Word:

| Name | Check against real Word | Expected | bodyXml |
| --- | --- | --- | --- |
| `adjacent_shared_edge_dotdash_uses_ms_border_number_eight` | does the document open clean? | (open-clean; resolved shared edge paints dotDash red on both cells) | `<w:tbl><w:tblPr><w:tblW w:w="5000" w:type="dxa"/></w:tblPr><w:tblGrid><w:gridCol w:w="2500"/><w:gridCol w:w="2500"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="2500" w:type="dxa"/><w:tcBorders><w:right w:val="dotDash" w:sz="8" w:space="0" w:color="FF0000"/></w:tcBorders></w:tcPr><w:p><w:r><w:t>L</w:t></w:r></w:p></w:tc><w:tc><w:tcPr><w:tcW w:w="2500" w:type="dxa"/><w:tcBorders><w:left w:val="single" w:sz="60" w:space="0" w:color="0000FF"/></w:tcBorders></w:tcPr><w:p><w:r><w:t>R</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:sectPr/>` |
| `tcborders_child_edges_in_annex_a_sequence` (open question) | does the document open clean? | (does out-of-order CT_TcBorders children force a strict-open repair? confirm gap vs. verbatim contract) | `<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tr><w:tc><w:tcPr><w:tcW w:w="5000" w:type="dxa"/><w:tcBorders><w:bottom w:val="single" w:sz="6" w:space="0" w:color="00FF00"/><w:top w:val="single" w:sz="4" w:space="0" w:color="FF0000"/></w:tcBorders></w:tcPr><w:p><w:r><w:t>x</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:sectPr/>` |
| `tcborders_child_ordering_follows_ct_tcborders_sequence` (open question) | does the document open clean? | (same root cause; confirm whether insideV/bottom/top order forces repair) | `<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="2000" w:type="dxa"/><w:tcBorders><w:insideV w:val="single" w:sz="4" w:space="0" w:color="00FF00"/><w:bottom w:val="single" w:sz="8" w:space="0" w:color="000000"/><w:top w:val="single" w:sz="8" w:space="0" w:color="000000"/></w:tcBorders></w:tcPr><w:p><w:r><w:t>R1C1</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:sectPr/>` |
