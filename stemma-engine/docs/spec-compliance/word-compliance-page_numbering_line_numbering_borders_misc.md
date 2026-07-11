# Word-compliance sweep — Section page-numbering, line-numbering, page borders, vAlign, textDirection, bidi

0 confirmed gaps, 12 new regression tests (green), 0 test-bugs.

## Confirmed incompliances

None. The sweep probed section-level page numbering (`w:pgNumType`: `chapStyle`, `chapSep` colon/period, schema-default fmt/chapSep), line numbering (`w:lnNumType`: `countBy` presence as the on-switch, `restart=continuous`, omitted `start` defaulting to 1), page borders (`w:pgBorders`: `offsetFrom=page`, `display=allPages`, `zOrder=front`, canonical top/left/bottom/right child order), vertical justification (`w:vAlign` top/bottom), and text direction (`w:textDirection` lr plus the Transitional legacy token `btLr`). All probed inputs are schema-valid per ECMA-376 / ISO 29500-1 and MS-OI29500; stemma opens each clean (no Word repair) and preserves the authored attributes and child order on reserialize.

## New regression tests

- `lnnumtype_continuous_with_countby_opens_clean` — §17.6.8 / §17.18.47: `countBy=1` with `restart=continuous` is schema-valid (opens clean) and both `countBy` and `restart` are preserved verbatim on reserialize.
- `textdirection_lr_section_opens_clean` — §17.6.20 / §17.18.93: `textDirection val=lr` is a valid ST_TextDirection at section level; opens clean.
- `valign_bottom_section_opens_clean` — §17.6.23 / §17.18.101: `vAlign val=bottom` is a valid ST_VerticalJc value; opens clean.
- `pgnumtype_chapsep_colon_opens_clean` — §17.6.12 / §17.18.6: `chapStyle=1` with `chapSep=colon` is schema-valid; opens clean.
- `pgborders_offsetfrom_page_canonical_children_opens_clean` — §17.6.10 / §17.18.63: `offsetFrom=page` with four CT_Border edges opens clean, `offsetFrom=page` is preserved, and the top/left/bottom/right child order survives reserialize.
- `pgnumtype_chapstyle_only_relies_on_word_defaults` — §17.6.12: a chapStyle-only `pgNumType` (fmt/chapSep optional with schema defaults) is schema-valid; opens clean.
- `pgnumtype_chapsep_period_value` — §17.18.6 / §17.6.12: `chapSep=period` with chapStyle is a defined ST_ChapterSep value; opens clean.
- `valign_top_section_value` — §17.6.23 / §17.18.101: `vAlign val=top` is a defined ST_VerticalJc value; opens clean.
- `textdirection_lr_section_value` — §17.6.20 / §17.18.93: `textDirection val=lr` is the §17.6.20 canonical section example; opens clean.
- `pgborders_display_allpages_zorder_front_explicit` — §17.6.10: `display=allPages` + `offsetFrom=page` + `zOrder=front` are all defined CT_PageBorders values (front is Word's own default); opens clean.
- `section_textdirection_btlr_legacy_token_opens_clean` — §17.6.20 / §17.18.93: `btLr` is a valid ST_TextDirection in the Transitional schema Word uses; opens clean (rejecting it would wrongly apply the Strict grammar).
- `lnnumtype_countby_present_start_omitted_defaults_one_opens_clean` — §17.6.8: a `countBy`-only `lnNumType` (start defaults 1, restart defaults newPage) is schema-valid; opens clean.

## Discarded test-bugs

None.

## Open questions — pending confirmation against real Word

None.
