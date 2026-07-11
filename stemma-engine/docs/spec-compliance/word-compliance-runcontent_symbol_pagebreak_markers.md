# Word-compliance sweep — Symbol characters and rendered-layout markers (sym, lastRenderedPageBreak)

**Summary:** 0 confirmed gaps, 11 new regression tests, 0 test-bugs discarded, 1 open question (ignored). Build status: green — `cargo test -p stemma --test spec_runcontent_symbol_pagebreak_markers_word_compliance -- --test-threads=1` reports 11 passed; 0 failed; 1 ignored.

## Confirmed incompliances

None confirmed. The one diverging observation (`sym_contributes_no_text_to_accept_and_reject`) cannot be confidently classified as a pipeline-bug vs. model-bug and is left as an open question below rather than asserted as a gap.

## Open question

### `sym_contributes_no_text_to_accept_and_reject`

- **Rule (confidence: medium):** A `w:sym` symbol glyph contributes no character to the consumed/plain-text stream. It is a font-specific glyph resolved from `sym@font` + `sym@char`, not a `w:t` literal-text payload, so Word's plain-text reading of `AB <sym/> CD` is `"ABCD"`.
- **§refs:** ECMA-376 §17.3.3.30 (sym), §17.3.3.31 (run content model).
- **Classification:** model-bug candidate (read-projection convention) — NOT confirmed.
- **What stemma does vs. what Word does:** stemma maps `w:sym` to an `OpaqueInline` that surfaces as a single U+FFFC (OBJECT REPLACEMENT CHARACTER) anchor placeholder in the read/plain-text projection. `read_accepted().to_text()` and `read_rejected().to_text()` both yield `"AB\u{FFFC}CD"`. The spec rule says no character is contributed, so the expected reading is `"ABCD"`.

  ```
  assertion `left == right` failed
    left: "AB\u{FFFC}CD"
   right: "ABCD"
  ```

  Both accept and reject readings show the identical U+FFFC, which confirms this is the opaque-anchor placeholder convention of the read projection, not a tracked-change artifact (reject is identical to accept).
- **Why an open question (not a confirmed gap):** The U+FFFC is a deliberate anchor-placeholder convention in stemma's read surface, used consistently for opaque inlines. Whether the plain-text projection *should* drop it for `w:sym` (so to_text reads as Word's consumed text) versus keeping a stable anchor for downstream positioning is a model decision, not an obvious defect. Confidence is medium and it warrants review before being encoded as a gap. The assertion was kept intact (not weakened) — the fail is the finding.
- **Suggested fix site (if accepted as a gap):** the read/plain-text projection's opaque-inline rendering for `w:sym` (the `OpaqueInline` -> text path), making `sym` contribute no character to `to_text()` while still preserving the element verbatim on serialize.
- **Minimal bodyXml repro:**

  ```xml
  <w:p><w:r><w:t>AB</w:t><w:sym w:font="Wingdings" w:char="F0E8"/><w:t>CD</w:t></w:r></w:p><w:sectPr/>
  ```

## New regression tests

All passing — they encode stemma's verbatim-preservation contract for opaque run markers and the non-semantic read behaviour of `lastRenderedPageBreak`.

- `sym_char_and_font_preserved_verbatim_on_roundtrip` — `sym@font` and `sym@char` survive serialize unchanged; opens clean (§17.3.3.30).
- `sym_char_is_four_digit_short_hex` — a 4-hex-digit `char` (ST_ShortHexNumber) is schema-valid and opens clean (§17.18.79 / §17.3.3.30).
- `last_rendered_page_break_preserved_not_regenerated` — exactly one authored `lastRenderedPageBreak` survives an untouched round-trip, never duplicated or dropped (§17.3.3.13).
- `sym_glyph_kept_as_sym_element_not_converted_to_t` — symbol stays a `w:sym` element, not transcoded to a `w:t` literal (§17.3.3.30/§17.3.3.31).
- `sym_pua_char_attr_preserved_not_normalized_on_save` — F000-shifted PUA `char="F03A"` preserved verbatim, not de-shifted (§17.3.3.30).
- `lastrenderedpagebreak_is_non_semantic_for_accept_reject` — accept and reject readings both equal the surrounding text with no marker character (§17.3.3.13/§17.3.3.31).
- `sym_direct_unicode_char_form_preserved` — direct `char="0045"` preserved verbatim, never rewritten to PUA `F045` (§17.3.3.30).
- `sym_char_f000_pua_offset_preserved_verbatim_on_serialize` — PUA `char="F0B7"` preserved verbatim, never collapsed to `00B7` (§17.3.3.30).
- `sym_font_attribute_preserved_independent_of_run_rfonts` — `sym@font` preserved verbatim and authored together with `char` on the `sym` element, independent of run `rFonts` (§17.3.3.30).
- `sym_char_short_hex_four_digits_opens_clean` — a `sym` with `char="F045"` validates with no errors (§17.18.79 / §17.3.3.30).
- `lastrenderedpagebreak_is_non_semantic_no_text_no_separator` — page-break marker contributes no glyph/separator to accepted or rejected text (§17.3.3.13/§17.3.3.31).

## Discarded test-bugs

None.
