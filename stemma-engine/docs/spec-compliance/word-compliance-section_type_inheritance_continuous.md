# Word-compliance sweep — Section type and continuous-section inheritance

**Summary:** 0 confirmed gaps, 10 new regression tests, 0 test-bugs discarded, 2 open questions (deferred for adjudication). Build status: green — `cargo test -p stemma --test spec_section_type_inheritance_continuous_word_compliance -- --test-threads=1` reports **10 passed; 0 failed; 2 ignored**.

This area covers `w:sectPr` section typing (`w:type@val=continuous|nextPage`), continuous-section geometry inheritance (§17.6.22), columns (§17.6.3/§17.6.4), page margins (§17.6.11), line numbering (§17.6.8), and the layout-only nature of the continuous section mark (§17.18.77) including same-page footnote anchors (§17.11.14).

## Drafting-error correction

ISO 29500-1 §17.6.22 literally says a continuous section's omitted page-level properties "shall be inherited from the **following** section". This is a **confirmed drafting error**; Word inherits from the **PRECEDING** section. stemma implements the corrected behavior in `src/import.rs::propagate_continuous_section_properties` (forward pass, fills None page-geometry fields of a `Continuous` section from `prev_props`). All tests here pin the corrected preceding-section semantics; none encode the literal "following" text.

## Confirmed incompliances

None. No assertion in this area was classified as a pipeline-bug or model-bug. All opens-clean and inheritance constraints hold. The two failing read-view assertions are open questions (methodology/modeling questions on stemma's `to_text()` read surface) rather than continuous-section engine bugs (see the open questions below).

## New regression tests (passing — active daily)

These encode correct Word behaviour that stemma already satisfies; they stay active as regression guards.

- `cols_equalwidth_true_ignores_col_children_layout_only` — §17.6.4/§17.6.22: `cols` with `equalWidth=1`, `num=2`, and extra (ignored) `col` children is well-formed; opens clean and accept-all reproduces the body prose unchanged.
- `cols_num_ignored_when_equalwidth_absent` — §17.6.4/§17.6.22: with `equalWidth=0` the `num` value is ignored and the explicit `col` children are well-formed; opens clean, prose unchanged.
- `section_negative_top_margin_signed_twips_opens_clean` — §17.6.11 + MS-OI29500 §2.1.218 (ST_SignedTwipsMeasure §17.18.81): a signed `w:top` of `-720` is valid and opens without repair; margins are layout-only.
- `continuous_break_inherits_geometry_when_type_val_continuous` — §17.6.22 (ST_SectionMark §17.18.77) + MS-OI29500 §2.1.227: a continuous section break with explicit `w:type@val=continuous` in a non-final `sectPr` is valid CT_SectType content; opens clean.
- `pgmar_out_of_domain_margins_open_clean` — MS-OI29500 §2.1.218 (§17.6.11): Word clamps over-domain `pgMar` values on load rather than rejecting; opens clean.
- `cols_num_above_word_max_opens_clean` — MS-OI29500 §2.1.213 (§17.6.4): Word clamps an out-of-range `cols@num` on load; opens clean.
- `unequal_cols_requires_width_on_each_col` — MS-OI29500 §2.1.212 (§17.6.3/§17.6.4): with `equalWidth=false` each `col` carries `w` and `num` matches the children; opens clean.
- `lnnumtype_countby_above_word_max_opens_clean` — MS-OI29500 §2.1.215 (§17.6.8): Word clamps an out-of-range `lnNumType@countBy` on load; combined with §17.6.22 allowing `lnNumType` on continuous breaks, opens clean.
- `lone_continuous_section_no_predecessor_keeps_own_geometry` — §17.6.22: a lone continuous section is well-formed; continuous inheritance is a no-op with no predecessor; opens clean.
- `continuous_section_does_not_fabricate_geometry_when_predecessor_also_omits` — §17.6.18/§17.6.22: two sections that both omit `pgSz` open clean; inheritance must not fabricate a `pgSz` with no source value to copy.

## Discarded test-bugs

None.

## Open questions (ignored, pending adjudication)

- `continuous_break_adds_no_text_unlike_nextpage` — §17.6.22. Assertion expected accept-all/reject-all text `"Paragraph one.Paragraph two."` (contiguous concatenation), but stemma's read-view `to_text()` returns `"Paragraph one.\n\nParagraph two."`. This is a paragraph-join methodology mismatch on the read surface (stemma inserts a blank-line paragraph separator; the spec author assumed contiguous concatenation), not a continuous-section defect. Reported as-is rather than weakened.
  ```xml
  <w:p><w:r><w:t>Paragraph one.</w:t></w:r><w:pPr><w:sectPr><w:type w:val="continuous"/></w:sectPr></w:pPr></w:p><w:p><w:r><w:t>Paragraph two.</w:t></w:r></w:p><w:sectPr/>
  ```
- `continuous_break_with_footnote_ref_is_layout_only` — §17.18.77/§17.6.22/§17.11.14. Two findings: (1) same `"\n\n"` paragraph-join separator as above; (2) `to_text()` surfaces the `w:footnoteReference` anchor as U+FFFC (OBJECT REPLACEMENT CHARACTER), whereas the assertion expects the footnote anchor to contribute no characters. Whether U+FFFC is correct is a read-view modeling question (it is a deliberate anchor placeholder, not necessarily a Word-text divergence). A non-fatal validator WARN also surfaced: `[I-XREF-001]` style ID `"FootnoteReference"` referenced but not defined (this test does not assert opens-clean, so it did not affect the result).
  ```xml
  <w:p><w:r><w:t>First section paragraph.</w:t></w:r><w:r><w:footnoteReference w:id="2"/></w:r><w:pPr><w:sectPr><w:type w:val="continuous"/></w:sectPr></w:pPr></w:p><w:p><w:r><w:t>Second section paragraph.</w:t></w:r></w:p><w:sectPr/>
  ```

Neither open question turns on an unknown Word behaviour: both are read-view modeling/methodology questions about stemma's own `to_text()` projection (paragraph-join separator and U+FFFC footnote anchor placeholder), not divergences in how real Word consumes the markup. They are resolved by deciding stemma's read-view contract.
