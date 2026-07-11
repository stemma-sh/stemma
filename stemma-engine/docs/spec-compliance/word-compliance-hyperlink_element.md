# Word-compliance — Hyperlink run wrapper (w:hyperlink: r:id, anchor, tgtFrame, tooltip, history, docLocation)

0 confirmed gaps, 9 new regression tests (green), 0 test-bugs.

## Confirmed incompliances

None. The tests probe the `w:hyperlink` run wrapper (ISO 29500-1 §17.16.22 / MS-OI29500 §2.1.521 / ECMA-376 Part 1 §A.1 CT_Hyperlink), focusing on the serializer path that actively rebuilds the element (`build_hyperlink_element`): preservation of all six attributes (`r:id`, `anchor`, `tgtFrame`, `tooltip`, `history`, `docLocation`), the `history`-omitted-defaults-to-false rule, simultaneous `r:id`+`anchor` round-trip, nested run formatting/`rStyle`, and nested bookmark range markers — all behave per spec and open clean.

## New regression tests

- `hyperlink_nested_bookmark_preserved_on_serialize` — a nested `w:bookmarkStart`/`w:bookmarkEnd` range (EG_PContent) inside a rebuilt hyperlink must survive serialization, with the display run intact.
- `hyperlink_tooltip_attr_preserved_on_serialize` — `w:tooltip` is re-emitted verbatim by `build_hyperlink_element` on round-trip.
- `hyperlink_tgtframe_value_preserved_on_serialize` — `w:tgtFrame` and its enumerated value (`_blank`) survive re-serialization unchanged.
- `hyperlink_doclocation_attr_preserved_on_serialize` — `w:docLocation` is captured and re-emitted on round-trip.
- `hyperlink_inner_run_formatting_restored_after_text_edit` — a bold inner run keeps its `w:b` rPr and text when the hyperlink is rebuilt.
- `hyperlink_both_rid_and_anchor_roundtrips_both_attrs` — exactly one `w:hyperlink` element carries both `r:id` and `anchor` after round-trip, and the package validates clean.
- `hyperlink_history_default_false_when_omitted` — an omitted `w:history` is not synthesized on export (defaults to false) and the doc validates clean.
- `hyperlink_extra_attrs_history_tgtframe_tooltip_roundtrip` — explicit `tooltip`/`tgtFrame`/`history="1"` all round-trip verbatim and the doc validates clean.
- `hyperlink_nested_run_formatting_preserved_on_serialize` — a nested run's `w:b` and `rStyle w:val="Hyperlink"` are preserved on serialize.

## Discarded test-bugs

None.

## Open questions — pending confirmation against real Word

None.
