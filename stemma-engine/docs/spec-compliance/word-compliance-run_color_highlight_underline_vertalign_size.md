# Word-compliance sweep — Run color, highlight, underline, vertical alignment, and font size

**Summary:** 0 confirmed gaps, 12 new regression tests (green), 0 test-bugs.

This area covers how Word consumes run-level character formatting: text color (`w:color`), text highlight (`w:highlight`), underline (`w:u`), vertical run alignment (`w:vertAlign`), and font size (`w:sz`). It focused on the render-only nature of these properties (they decorate or recolor existing run content but never add, remove, or move text), the resolution precedence among theme-derived color attributes (`themeColor` / `themeTint` / `themeShade`), the supersession of run `w:shd` by `w:highlight`, and schema-valid edge values (`auto` underline color, odd half-point sizes, `none` underline/highlight, direct overrides of inherited style underline).

## Confirmed incompliances

None. Every probed constraint — color/highlight/underline/vertAlign/size resolution, theme attribute precedence, highlight-over-shd supersession, and odd/edge enum values — matched Word's consumption behavior (open-clean validity and unchanged accept/reject text on these render-only properties).

## New regression tests

- `highlight_supersedes_run_shd_render_only` — when a run carries both `w:highlight` and run `w:shd`, the highlight wins for the run background and the property is render-only, so accept/reject text is unchanged (§17.3.2.15).
- `underline_themetint_supersedes_themeshade_render_only` — on `w:u`, `themeTint` takes precedence over `themeShade` when resolving the underline color, and underline is decoration only, so text is unchanged (MS-OI29500 §2.1.100).
- `underline_color_auto_opens_clean` — `w:u` with `w:color="auto"` is schema-valid (ST_HexColorAuto) and Word opens it without repair (§17.3.2.40).
- `vertalign_subscript_render_only_no_font_change` — `w:vertAlign="subscript"` shifts rendering only; it changes no run text and never alters the font choice, so accept/reject text is unchanged (§17.3.2.42).
- `sz_odd_half_point_opens_clean` — an odd `w:sz` half-point value (a valid ST_HpsMeasure measured in half-points) opens clean in Word without repair (§17.3.2.38).
- `color_themetint_wins_over_themeshade` — when a `w:color` carries both `themeTint` and `themeShade`, Word resolves the color via `themeTint` (MS-OI29500 §2.1.72).
- `underline_themetint_wins_over_themeshade` — when a `w:u` carries both `themeTint` and `themeShade`, Word resolves the underline color via `themeTint` (MS-OI29500 §2.1.100).
- `underline_words_enum_member_preserved` — `w:u w:val="words"` is a member of ST_Underline and the value round-trips preserved and opens clean (§17.18.99, §17.3.2.40).
- `underline_none_explicit_off_render_only` — `w:u w:val="none"` is an explicit off value that is render-only and leaves accept/reject text unchanged (§17.3.2.40).
- `highlight_lightgray_palette_member_render_only` — `w:highlight w:val="lightGray"` is an enumerated ST_HighlightColor member, render-only, leaving accept/reject text unchanged (§17.18.40, §17.3.2.15).
- `highlight_supersedes_run_shd_both_present` — with both `w:highlight` and run `w:shd` present the document opens clean and highlight supersedes the run shading for the background (§17.3.2.15).
- `direct_u_none_overrides_style_underline_render_neutral` — a direct `w:u w:val="none"` on a run overrides an inherited style underline; the override is render-only and text is unchanged (§17.3.2.40, §17.7.2).

## Discarded test-bugs

None.
