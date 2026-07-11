# Word-compliance sweep — Structured document tags (SDT): properties, content levels, typed sub-tags

0 confirmed gaps, 12 new regression tests (green), 0 test-bugs.

## Confirmed incompliances

None. Coverage spans CT_SdtPr property ordering and identity preservation (rPr-before-alias, alias/tag/id), the control-kind elements (comboBox + listItem, text, date), typed sub-tag child sequences (full CT_SdtDate: dateFormat/lid/storeMappedDataAs/calendar; sdtEndPr/rPr), opaque boolean and enum children (temporary, lock=sdtContentLocked), placeholder/showingPlcHdr/dataBinding co-occurrence, tabIndex navigation order, all three SDT content levels (block, inline, cell-level sdt-in-tr), and nested inline-inside-block SDTs — stemma opens every arrangement clean and round-trips the markup verbatim and in Annex A order.

## New regression tests

- `cell_level_sdt_wrapping_tc_opens_clean` — a cell-level SDT (w:sdt child of w:tr wrapping one w:tc, CT_SdtCell) opens clean and re-emits sdtContent ordered before the wrapped tc (§17.5.2.32/.33).
- `combobox_sdt_listitems_roundtrip_clean` — a comboBox SDT opens clean and round-trips its kind element plus every listItem displayText/value unchanged (§17.5.2.5/.21).
- `nested_inline_sdt_inside_block_sdt_preserved_clean` — an inline SDT nested inside a block SDT's paragraph opens clean and both outer and inner tag identities survive without flattening (§17.5.2.29/.31).
- `sdt_end_pr_rpr_round_trips` — sdtEndPr opens clean and its end-character rPr (b + i) round-trips intact and in order inside sdtEndPr (§17.5.2.37/.28).
- `sdt_tab_index_round_trips` — tabIndex is a conformant sdtPr child that opens clean and round-trips verbatim with its val preserved (§17.5.2.41).
- `sdt_lock_sdtcontentlocked_enum_value` — the ST_Lock value sdtContentLocked opens clean and is preserved without normalizing to another spelling (§17.18.49/§17.5.2.23).
- `sdt_temporary_boolean_preserved` — the opaque w:temporary boolean child opens clean and is re-emitted verbatim, preserving Word's remove-on-edit semantic (§17.5.2.43).
- `sdt_date_full_child_sequence_preserved` — a full CT_SdtDate opens clean and preserves the opaque date sub-tree including storeMappedDataAs and the trailing calendar child (§17.5.2.7/.40/.3).
- `cell_level_sdt_rewraps_tc_inside_tr` — a single-cell sdt-in-tr opens clean and the SDT stays at cell level (tr > sdt > sdtContent > tc, not promoted to row level) with its alias preserved (§17.5.2.32/.1).
- `sdtpr_rpr_precedes_alias_full_property_order` — CT_SdtPr keeps rPr ahead of the identity elements (rPr before alias) and the replacement-text rStyle survives round-trip (§17.5.2.38/.27, §A.1).
- `placeholder_showingplchdr_databinding_coexist_in_order` — placeholder/showingPlcHdr/dataBinding co-occurring in CT_SdtPr order opens clean without repair (§17.5.2.25).
- `sdt_date_child_order_full_ct_sdtdate` — a fully-populated CT_SdtDate opens clean, keeps the fixed child order dateFormat < lid < storeMappedDataAs < calendar, and round-trips fullDate verbatim (§17.5.2.7, §A.1).

## Discarded test-bugs

None.
