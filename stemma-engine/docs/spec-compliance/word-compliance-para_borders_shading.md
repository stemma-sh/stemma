# Word-compliance sweep — Paragraph borders and shading (pBdr, shd, between/bar collapse)

0 confirmed gaps, 11 new regression tests (green), 0 test-bugs.

## Confirmed incompliances

None. The sweep probed paragraph border edges (`w:pBdr`: top/bottom/between/bar, including `none`, val-only defaulting, out-of-range `sz`, and `color="auto"`) and paragraph shading (`w:shd`: `clear`/pattern values, `fill`, `themeFill` supersession and fallback) for open-clean validity, verbatim roundtrip of theme references, and the consumption rule that borders and shading contribute no glyphs to accepted/rejected text — plus the between-collapse suppression when adjacent border sets differ. Stemma matches Word on every case.

## New regression tests

- `explicit_none_border_edge_opens_clean_and_text_unchanged` — `none` is a valid ST_Border member; border edges contribute no glyphs, so accepted/rejected text equals the run content and the doc opens clean (§17.3.4 / §17.18.2).
- `shd_color_auto_default_pattern_clear_text_unchanged` — `shd` with `val=clear` and an omitted (auto) color paints a background only; accepted/rejected text is unchanged and the doc opens clean (§17.3.5 / §17.18.78).
- `between_collapse_suppressed_when_border_sets_differ` — adjacent paragraphs with differing bottom `space` (1 vs 0) suppress the between collapse; both schema-valid CT_PBdr sets open clean (§17.3.1.5 / §17.3.1.7 / §17.3.4).
- `bar_border_consumption_no_glyphs_text_unchanged` — the `bar` (mirror-margin) border is ignorable decoration contributing no glyphs; accepted/rejected text is unchanged and the doc opens clean (§17.3.1.4 / §17.3.4).
- `shd_themefill_supersedes_literal_fill_roundtrip` — `themeFill` and the literal `fill` both survive the roundtrip so neither the theme-resolution path nor the cached-fallback path is broken (§17.3.5 / §17.18.97).
- `para_shd_themefill_omitted_falls_back_to_fill` — with `themeFill` omitted, a `clear`/`auto`/literal-`fill` shd is well-formed and Word resolves the background from the literal fill, opening clean (MS-OI29500 §2.1.144 / §17.3.5).
- `para_border_omitted_sz_color_space_word_defaults_zero_auto` — a val-only border edge is valid input; Word defaults `color=auto`/`sz=0`/`space=0`, the doc opens clean, and the no-glyph rule holds on accepted text (MS-OI29500 §2.1.38 / §17.3.4).
- `para_border_sz_out_of_range_opens_clean` — `sz=200` is well-formed against the unbounded ST_EighthPointMeasure type (Word clamps on render); the doc opens clean (§17.3.4 / §17.18.23).
- `para_shd_pattern_only_no_colors_resolves_and_opens_clean` — a pattern-only `shd` (`pct20`, no color/fill) is valid since only `val` is required; omitted colors resolve to auto, accepted text is unchanged, and the doc opens clean (§17.3.5 / §17.18.78).
- `para_border_color_auto_preserved_and_opens_clean` — `color="auto"` is the schema default and a legal ST_HexColor token for a border edge; the document opens clean (§17.3.4 / §17.18.38).
- `para_pbdr_between_bar_only_resolve_and_opens_clean` — CT_PBdr declares all six edges `minOccurs=0`, so a pBdr carrying only `between` and `bar` is well-formed and opens clean (§17.3.1.24 / §17.3.4).

## Discarded test-bugs

None.

## Open questions — pending confirmation against real Word

None.
