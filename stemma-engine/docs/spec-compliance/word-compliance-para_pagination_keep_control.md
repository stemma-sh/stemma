# Word-compliance sweep — Paragraph pagination and line-keeping control

0 confirmed gaps, 12 new regression tests (green), 0 test-bugs.

## Confirmed incompliances

None. Coverage spans the paragraph pagination and line-keeping control toggles on `CT_PPrBase` (`autoSpaceDE`, `autoSpaceDN`, `snapToGrid`, `adjustRightInd`, `pageBreakBefore` vs `keepNext`, `kinsoku`/`overflowPunct`, `widowControl`, `suppressAutoHyphens`, `suppressOverlap`, `topLinePunct`, `mirrorIndents`, `wordWrap`) for clean-open structural validity and for layout-only consumption semantics (accept/reject never alter run text); stemma matches Word's consumption and preservation contract in every case.

## New regression tests

- `autospacede_val_off_is_off_and_opens_clean` — `autoSpaceDE w:val="0"` is an in-domain `ST_OnOff` literal: opens clean and the off toggle adds/removes no characters, so accepted text is verbatim (§17.3.1.2).
- `autospacedn_val_off_is_off_and_opens_clean` — `autoSpaceDN w:val="0"` is an in-domain `ST_OnOff` literal: opens clean and changes no characters, so accepted text is verbatim (§17.3.1.3).
- `snaptogrid_val_off_is_off_and_opens_clean` — `snapToGrid w:val="off"` is an in-domain `ST_OnOff` literal: opens clean and changes no characters, so accepted text is verbatim (§17.3.1.32).
- `adjustrightind_val_off_is_off_and_opens_clean` — `adjustRightInd w:val="0"` is an in-domain `ST_OnOff` literal: opens clean and changes no characters, so accepted text is verbatim (§17.3.1.1).
- `page_break_before_supersedes_keep_next_layout_only` — `keepNext` + `pageBreakBefore` are layout-only page-placement toggles: opens clean and both accept and reject readings are the verbatim run text (§17.3.1.23/§17.3.1.15).
- `overflow_punct_overrides_kinsoku_on_conflict_text_verbatim` — `overflowPunct`-over-`kinsoku` is a line-breaking tie-break only: opens clean and adds/removes no characters, so accepted text is verbatim (MS-OI29500 §2.1.52).
- `widow_control_bare_present_means_on_layout_only_text_verbatim` — a bare `widowControl` element means on (val omitted = true): opens clean and both accept and reject readings are the verbatim run text (§17.3.1.44).
- `suppress_auto_hyphens_bare_present_layout_only_opens_clean` — a bare `suppressAutoHyphens` element is display-time only (soft hyphens are not stored text): opens clean and both accept and reject readings are verbatim (§17.3.1.34).
- `suppress_overlap_present_implied_true_layout_only` — a bare `suppressOverlap` child of `pPr` is schema-valid frame repositioning only: opens clean and both accept and reject readings are verbatim (§17.3.1.36).
- `top_line_punct_bare_present_implied_true` — a bare `topLinePunct` element only compresses first-of-line punctuation at display time: opens clean and both reject and accept readings are verbatim (§17.3.1.43).
- `mirror_indents_present_implied_true_layout_only` — a bare `mirrorIndents` element only remaps indents to page edges: opens clean and both accept and reject readings are verbatim (§17.3.1.18).
- `word_wrap_bare_present_implied_true` — a bare `wordWrap` element selects break points only: opens clean and both accept and reject readings are verbatim (§17.3.1.45).

## Discarded test-bugs

None.
