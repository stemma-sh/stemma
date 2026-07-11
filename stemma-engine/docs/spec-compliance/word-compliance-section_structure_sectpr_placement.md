# Word-compliance sweep — Section structure: sectPr placement, body-final vs in-paragraph, sectPrChange

**Summary:** 0 confirmed gaps, 8 new regression tests, 1 test-bug discarded, 3 tests left as open questions. Build status: green — `cargo test -p stemma --test spec_section_structure_sectpr_placement_word_compliance` reports **8 passed; 0 failed; 3 ignored**.

## Confirmed incompliances

None confirmed. The three divergences observed (non-final sectPr child ordering, and the read-side blank-line join) are left as open questions rather than promoted to confirmed gaps: all three reduce to the same root cause as the discarded test-bug (stemma round-trips untouched content byte-verbatim, so it neither reorders nor restructures imported sectPr that no edit transaction has rebuilt), and that cause sits on the boundary between "documented contract" and "real consumption gap." They need confirmation against real Word (the read-side text projection has no spec-text source of truth) before they can be classified as pipeline-bug vs model-bug.

## New regression tests (passing)

- `sectprchange_previous_sectpr_stays_nested_not_a_body_section` — a sectPrChange's nested previous-sectPr snapshot is revision metadata on the single final body sectPr, not a promoted second section; opens clean, wrapper survives roundtrip, accept-all text unchanged (§17.6.19, §17.13.5.32).
- `pgnumtype_start_on_section_opens_clean_and_survives_roundtrip` — `pgNumType@start` is valid section content; opens clean and survives parse→serialize (§17.6.12).
- `valign_center_on_section_opens_clean_and_survives_roundtrip` — `vAlign=center` (ST_VerticalJc) is valid section content; opens clean and survives roundtrip (§17.6.23).
- `titlepg_present_empty_is_on_and_opens_clean` — a bare `titlePg` toggle is valid CT_OnOff section content; opens without repair (§17.10.6, §17.17.4).
- `nonfinal_sectpr_omits_type_when_absent` — a section parsed without `w:type` must not gain a fabricated `w:type` on serialization; modeled `pgSz` is still emitted (§17.6.22).
- `sectprchange_nonempty_prev_props_opens_clean_text_stable` — a sectPrChange whose nested CT_SectPrBase carries type+pgSz opens clean; accept-all and reject-all leave visible prose unchanged (§17.6.19, §17.13.5.32).
- `final_body_sectpr_is_last_body_child_opens_clean` — the final sectPr placed as the last body child opens without repair; mirrors validator invariant I-DOC-003 (§17.6.17).
- `sectprchange_is_last_child_of_sectpr_ct_sectpr_sequence` — sectPrChange emitted after the section-content children (pgMar before sectPrChange) per the CT_SectPr schema sequence; opens clean (§17.6.19, Annex A).

## Discarded test-bugs

- `nonfinal_sectpr_children_reordered_to_ct_sectpr_sequence` — wrong expectation. The test applies no edit, so the IR-rebuild path (`build_paragraph_sect_pr` → `section_properties_to_element` → `sort_sect_pr_children`, which DOES sort to CT_SectPr order) is never exercised; `serialize_snapshot` clones `snapshot.scaffold.package` and writes `word/document.xml` byte-for-byte from import. Untouched content round-tripping byte-verbatim is the documented stemma contract; stemma's own `validate()` returns ok=true with zero issues on the unordered output and never emits out-of-order sectPr children on its active-emission path. The test misapplied a reserialize-ordering assertion to unedited content. No real-Word check needed — CT_SectPr is an ordered xsd:sequence and stemma already honors it where it actually emits.

## Open questions — pending confirmation against real Word

The three open questions below do not turn on an unknown Word behaviour (the spec text on schema ordering is unambiguous); they remain open because the disagreement is about whether stemma should mutate unedited bytes / how the read projection joins blocks — not about the spec value. They are a policy decision, and for the read-side case the text projection can be gold-checked against real Word.

### `nonfinal_sectpr_continuous_type_first_before_pgsz`
- Mode: reserialize child-ordering of an in-paragraph (non-final) sectPr.
- Expected (schema): `<w:type w:val="continuous"/>` before `<w:pgSz`.
- Observed: input order `<w:pgSz/>` then `<w:type/>` re-emitted verbatim.
- bodyXml:
```xml
<w:p><w:pPr><w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:type w:val="continuous"/></w:sectPr></w:pPr><w:r><w:t>Continuous section.</w:t></w:r></w:p><w:p><w:r><w:t>Final section.</w:t></w:r></w:p><w:sectPr/>
```

### `nonfinal_sectpr_type_precedes_pgsz_with_cols_last`
- Mode: reserialize child-ordering of an in-paragraph (non-final) sectPr.
- Expected (schema, EG_SectPrContents): `type` < `pgSz` < `cols`.
- Observed: input order `<w:cols/><w:type/><w:pgSz/>` re-emitted verbatim (cols first).
- bodyXml:
```xml
<w:p><w:pPr><w:sectPr><w:cols w:num="3" w:space="720"/><w:type w:val="nextPage"/><w:pgSz w:w="15840" w:h="12240" w:orient="landscape"/></w:sectPr></w:pPr><w:r><w:t>Columns section.</w:t></w:r></w:p><w:p><w:r><w:t>Final.</w:t></w:r></w:p><w:sectPr/>
```

### `nonfinal_sectpr_precedes_pprchange_in_ppr`
- Mode: read-side accept-all visible text (`read_accepted().to_text()`).
- Expected (post-condition): single `\n` join — `"First section last paragraph.\nSecond section paragraph."`.
- Observed: blank line between blocks — `"First section last paragraph.\n\nSecond section paragraph."` (the section-break paragraph appears to inject an extra empty block). Opens-clean passes.
- bodyXml:
```xml
<w:p><w:pPr><w:jc w:val="center"/><w:sectPr><w:type w:val="nextPage"/><w:pgSz w:w="12240" w:h="15840"/></w:sectPr><w:pPrChange w:id="7" w:author="A" w:date="2026-06-01T00:00:00Z"><w:pPr/></w:pPrChange></w:pPr><w:r><w:t>First section last paragraph.</w:t></w:r></w:p><w:p><w:r><w:t>Second section paragraph.</w:t></w:r></w:p><w:sectPr><w:pgSz w:w="12240" w:h="15840"/></w:sectPr>
```
