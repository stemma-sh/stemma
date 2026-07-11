# Word-compliance sweep — Run breaks, tabs, and hyphens (br/cr/tab/ptab/noBreakHyphen/softHyphen)

0 confirmed gaps, 12 new regression tests (green), 0 test-bugs.

## Confirmed incompliances

None. Coverage spans `w:br` (type=page/column/textWrapping and the `clear` attribute), `w:cr`, `w:softHyphen`, and `w:tab` pos handling against ISO/IEC 29500-1 §17.3.3.1 (CT_Br), §17.18.3 (ST_BrClear), §17.18.4 (ST_BrType), §17.3.3.29 (softHyphen), §17.3.1.37 / §17.18.81 (tab pos) and MS-OI29500 §2.1.61, asserting both open-clean validity and accept/reject read text; stemma matches Word on every case.

## New regression tests

- `clear_ignored_on_page_break_reads_as_plain_page_break` — `clear` on a `type=page` break is schema-valid and ignored; the break adds no character so accepted text is "AB".
- `column_break_intra_paragraph_deletes_no_content` — a mid-paragraph `type=column` break relocates following text but deletes no content; accept and reject both read "AB".
- `ordered_breaks_in_run_all_layout_only_concatenate` — interleaved text/`br`/`cr` are layout-only; the three fragments concatenate in source order to "ABC".
- `clear_attr_ignored_on_page_break` — `clear=all` on a `type=page` break is ignored (not an error) and changes no text; accept and reject read "AB".
- `clear_omitted_defaults_to_none` — a bare `w:br` (type and clear omitted) is fully conformant via §17.3.3.1 defaults and contributes no character; accepted text is "XY".
- `bare_clear_all_break_defaults_type_textwrapping` — with type omitted it defaults to textWrapping, for which `clear=all` is a valid restart; the break adds no character so accepted text is "PQ".
- `clear_on_page_break_is_valid_and_ignored` — CT_Br declares type and clear as independent optional attributes, so `type=page` + `clear=all` is schema-valid; accept and reject read "AB".
- `explicit_textwrapping_br_reads_like_bare_br` — explicit `type=textWrapping` (the canonical default) opens without repair and is layout-only; accepted text is "XY".
- `softhyphen_zero_width_in_accept_and_reject` — an unrealized `w:softHyphen` is zero-width and contributes no character; both accept and reject read "breaking".
- `column_break_contributes_no_text_character` — `column` is an enumerated ST_BrType value that opens without repair and contributes no character; accepted text is "PQ".
- `br_page_with_clear_attr_clear_ignored_opens_clean` — `type=page` with a `clear` attribute opens clean because clear is simply ignored on a non-textWrapping break; accepted text is "AB".
- `tab_pos_at_word_max_boundary_preserved_opens_clean` — `w:pos="31680"` (Word's documented max tab pos) is in-range and must round-trip verbatim, never clamped or dropped.

## Discarded test-bugs

None.
