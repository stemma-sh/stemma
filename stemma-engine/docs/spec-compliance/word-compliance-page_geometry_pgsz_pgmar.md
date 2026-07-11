# Word-compliance sweep — Page geometry: pgSz and pgMar (size, orientation, margins, gutter)

0 confirmed gaps, 10 new regression tests (green), 0 test-bugs.

## Confirmed incompliances

None. The sweep probed the section-properties page-geometry children (`w:pgSz`, `w:pgMar`, `w:rtlGutter`, `w:docGrid`) for EG_SectPrContents child ordering, signed/over-range value preservation on save (negative margins, gutter beyond Word's load-clamp, paper `code` beyond 0..118), the orientation flag interacting with literal w/h, absence-vs-synthesis of optional children, and the consumption rule that `code` is a label that never sets geometry — stemma matches Word on every probe.

## New regression tests

- `sectpr_rtlgutter_emitted_after_cols_before_docgrid` — EG_SectPrContents is an xsd:sequence, so re-serialized children must appear in order pgMar < cols < rtlGutter < docGrid (Annex A; §17.6.16).
- `pgsz_orient_landscape_authoritative_dims_not_swapped` — `orient="landscape"` with portrait-shaped dims (w<h) is valid; orient is a layout flag over literal w/h, so the file opens clean without dimension swapping (§17.6.13; ST_PageOrientation §17.18.65).
- `pgsz_code_is_label_only_does_not_set_dimensions` — `code` is a paper-size label only; with explicit w/h present the section is valid and accepting the document yields unchanged prose (§17.6.13; MS-OI §2.1.220).
- `negative_top_margin_preserved_and_opens_clean` — `w:top` is ST_SignedTwipsMeasure; a negative top is a documented Word layout instruction and must be re-emitted verbatim (`w:top="-720"`) and open clean (§17.6.11; §17.18.81; MS-OI §2.1.555).
- `gutter_over_word_max_preserved_not_clamped` — `w:gutter` is unsignedLong; 40000 (> Word's 31680 load-clamp) is schema-valid, Word clamps only on load, so stemma must preserve the authored value verbatim and open clean (MS-OI §2.1.218).
- `pgsz_code_over_118_preserved_and_opens_clean` — `code=240` (> Word's 0..118 load-clamp) is descriptive metadata only; stemma must preserve it verbatim and the doc opens clean (MS-OI §2.1.220; §17.6.13).
- `absent_pgsz_pgmar_not_synthesized` — pgSz/pgMar are optional CT_SectPr children; a bare sectPr must not gain synthesized size/margins on save and must open clean (§17.6.13; §17.6.11).
- `pgmar_negative_top_bottom_opens_clean` — negative top/bottom (ST_SignedTwipsMeasure, floor -31680) is layout-only and must not mutate prose, so accept == reject == the single paragraph and the doc opens clean (§17.6.11; MS-OI §2.1.218; §17.18.81).
- `pgsz_code_above_word_max_118_opens_clean` — `code=999` is a label-only identifier that never sets geometry; w/h govern size, the doc opens clean, and prose is unchanged (§17.6.13; MS-OI §2.1.220).
- `pgsz_code_only_without_w_h_has_no_meaning` — a code-only pgSz (no w/h) "has no meaning" for size; Word falls back to default geometry, the file opens clean, and prose is unchanged (§17.6.13).

## Discarded test-bugs

None.

## Open questions — pending confirmation against real Word

None.
