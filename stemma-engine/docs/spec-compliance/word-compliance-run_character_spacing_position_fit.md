# Word-compliance sweep — Run character spacing, kerning, scaling, position, and fit

0 confirmed gaps, 12 new regression tests (green), 0 test-bugs.

## Confirmed incompliances

None. Coverage spans stemma's consumption surface (accept-all / reject-all read text plus opens-clean validation) for every run-level layout-only directive in this area — `w:spacing` (character spacing, §17.3.2.35), `w:kern` (kerning threshold, §17.3.2.19), `w:fitText` (manual run width, §17.3.2.14), and `w:eastAsianLayout` (combine / combineBrackets / vert / vertCompress, §17.3.2.10) — including their cross-property conflicts (spacing-vs-fitText, combine-vs-vert, vert-vs-vertCompress), boundary values (kern at the Word max 3277, kern above sz), omitted/shared fitText ids, and direct-vs-style spacing resolution. All of these are render-only and never participate in the character stream; stemma preserved the authored text exactly on both accept and reject and opened every schema-valid package clean.

## New regression tests

- `ea_layout_combinebrackets_ignored_without_combine_text_unchanged` — `combineBrackets` is a display-only bracket-style hint, ignored without `combine`; injects no glyphs into the text stream (§17.3.2.10 / §17.18.8).
- `ea_layout_vertcompress_ignored_without_vert_text_unchanged` — `vertCompress` is a render-only compression hint, ignored without `vert`; never alters the text stream (§17.3.2.10 / §22.9.2.7).
- `fittext_id_render_only_text_unchanged_noncontiguous` — non-contiguous same-id `fitText` runs are merely unlinked for layout; the concatenated authored text is preserved (§17.3.2.14 / §17.18.10).
- `spacing_with_fittext_text_unchanged_opens_clean` — `spacing` and `fitText` co-occurring is resolved at layout only (spacing ignored when fitText present); text unchanged, opens clean (§17.3.2.35 / §17.3.2.14 / MS-OI29500 §2.1.96).
- `kern_below_sz_threshold_text_unchanged_opens_clean` — `kern` exceeding `sz` suppresses kerning at layout; kerning is render-only, text unchanged, opens clean (§17.3.2.19 / §17.3.2.38).
- `fittext_with_spacing_and_eastasianlayout_layout_only_text_unchanged` — `fitText` + `spacing` + `eastAsianLayout` are all valid CT_RPr children that may co-occur; all render-only, text unchanged, opens clean (§17.3.2.14 / §17.3.2.35 / §17.3.2.10).
- `eastasianlayout_combine_ignores_vert_layout_only_text_unchanged` — `combine` together with `vert` is resolved at layout (vert ignored); render-only, text unchanged, opens clean (§17.3.2.10 / MS-OI29500 §2.1.75).
- `eastasianlayout_vertcompress_ignored_layout_only_text_unchanged` — `vert` + `vertCompress` is resolved at layout (vertCompress ignored); render-only, text unchanged, opens clean (§17.3.2.10 / §22.9.2.7).
- `fittext_omitted_id_links_contiguous_runs_layout_only_text_unchanged` — `fitText` with omitted id (Word defaults id to 0) links contiguous runs for layout only; authored concatenation preserved, opens clean (§17.3.2.14 / §17.18.10).
- `kern_at_word_max_3277_with_sz_opens_clean_text_unchanged` — `kern=3277` is the inclusive max of Word's 0..3277 domain; in-range markup opens clean and is render-only, text unchanged (§17.3.2.19 / MS-OI29500 §2.1.83).
- `spacing_ignored_when_fittext_present_is_render_only_text_unchanged` — fitText wins over spacing (MS-OI29500 §2.1.96); both render-only, text unchanged, opens clean.
- `direct_spacing_overrides_style_spacing_last_writer_wins_non_toggle` — `spacing` is non-toggle (the closed toggle list excludes it), so direct spacing overrides style spacing; render-only, text unchanged, opens clean (§17.7.3 / §17.3.2.35).

## Discarded test-bugs

None.
