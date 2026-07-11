# Word-compliance — Header/footer references and type matching (default/even/first)

**Summary:** 1 confirmed gap, 6 new regression tests (all green), 0 test-bugs discarded. Build status: GREEN — `cargo test -p stemma --test spec_headers_footers_references_types_word_compliance -- --test-threads=1` → 6 passed, 0 failed, 2 ignored (validator-gap tests, pending confirmation against real Word).

The file carries 6 active regression tests plus 2 validator-gap tests `#[ignore]`d pending confirmation against real Word. The confirmed-gap and open-question sections below record the real validator gaps.

---

## Confirmed incompliances

### 1. Absent / empty `r:id` on a CT_Rel-derived header reference is not flagged (pipeline-bug, high confidence)

- **Rule:** A `w:headerReference` that omits its `r:id` attribute is non-conformant. The schema makes `r:id` required on `CT_Rel` (the base of `CT_HdrFtrRef`), and §17.10.5 says a header whose relationship is not present makes the document non-conformant. A Word-faithful `validate()` must report an error (`opensClean = false`). An empty `r:id=""` is the same shape: it resolves no relationship.
- **§refs:** ISO 29500-1 §17.10.5; ECMA-376 Annex A `CT_HdrFtrRef` / `CT_Rel`; ECMA-376 Part 2 OPC §6.5.
- **Classification:** pipeline-bug.
- **What stemma does vs what Word does (the assertion failure):** stemma reports the document clean (`report.ok == true`); the expected post-condition is `opensClean == false`. Word reports the document needs repair / cannot resolve the header reference (non-conformant). I-REL-001 only fires for an `r:id` *value* that names no relationship; an **absent** `r:id` is never recorded, and the `collect_relationship_references` guard `if is_rel_ref && !value.is_empty()` in `docx_validate.rs` deliberately drops an empty `r:id=""` before the dangling-ref check. So both the omitted-attribute and empty-string cases pass validation silently.
- **Suggested fix site:** `stemma-engine/src/docx_validate.rs` — the I-REL family. `check_rel_001_rid_references` only checks `r:id` values that are present; `collect_relationship_references` never records an absent attribute, and it guards `!value.is_empty()`. Add a sibling check (e.g. `I-REL-004` "CT_Rel-derived reference requires r:id") that walks `state.story_parts` for `w:headerReference` / `w:footerReference` (CT_HdrFtrRef extends CT_Rel) and emits `ValidationSeverity::Error` when `r:id` is missing or empty. The `HEADER_REL_TYPE` / `FOOTER_REL_TYPE` constants and the part-walking infra already exist at the top of the module.
- **Minimal bodyXml repro:**

```xml
<w:p><w:r><w:t>Body</w:t></w:r></w:p><w:sectPr><w:headerReference w:type="default"/><w:pgSz w:w="12240" w:h="15840"/></w:sectPr>
```

---

## New regression tests (active, green)

These 6 tests pin stemma's verbatim-preservation and CT_SectPr serialization contract for header/footer references:

- `hdrftr_refs_serialized_at_sectpr_head_in_authored_order` — EG_HdrFtrReferences is the leading group of CT_SectPr; authored header(default) precedes footer(default), and all refs precede `pgSz` (§17.10.5, §17.10.2; Annex A CT_SectPr).
- `titlepg_section_without_refs_synthesizes_nothing_and_opens_clean` — a `titlePg` section with no refs round-trips verbatim, fabricates no references, and opens clean (resolution is a consumption rule, not save-synthesis).
- `plain_section_without_refs_synthesizes_no_header_ref_and_opens_clean` — a plain section with no refs fabricates no default `headerReference` and no `first` ref, and opens clean (MS-OI §2.1.298 / §17.10.5).
- `empty_sectpr_synthesizes_no_refs_and_opens_clean` — an empty `<w:sectPr/>` round-trips verbatim with no synthesized refs and opens clean.
- `hdrftr_type_attr_roundtrips_exact_enum_spelling` — `first` / `even` / `default` re-serialize as the exact literal ST_HdrFtr tokens (§17.18.36; ECMA-376 Part 4 §2.18.41 ST_HdrFtr).
- `six_hdrftr_refs_all_types_preserved` — all three header and all three footer types survive the round-trip; CT_SectPr permits up to 6 refs (§17.10.5, §17.10.2; Annex A CT_SectPr).

---

## Discarded test-bugs

None. No test encoded a wrong expectation that required deletion.

---

## Open questions — pending confirmation against real Word

Every item below needs checking against real Word. Each lists the check, the expected `opensClean` value, and the minimal repro body.

### 1. Absent `r:id` on `headerReference` (confirmed gap)
- **Check against real Word:** does the document open clean?
- **Expected:** Word reports the document needs repair / cannot resolve the header reference (non-conformant) → `opensClean = false`.
- **bodyXml:**

```xml
<w:p><w:r><w:t>Body</w:t></w:r></w:p><w:sectPr><w:headerReference w:type="default"/><w:pgSz w:w="12240" w:h="15840"/></w:sectPr>
```

### 2. Empty `r:id=""` on `headerReference` (open question, pending confirmation against real Word)
- **Check against real Word:** does the document open clean?
- **Expected (hypothesis, unconfirmed):** `ST_RelationshipId` is `xsd:string` with no `minLength`, so `r:id=""` is schema-valid. Established consumption precedent (dangling `basedOn`, orphan parts, overturned negative-`numId` rows) is that Word silently ignores unresolvable references → likely `opensClean = true`. Only checking against real Word can settle it. If real Word shows needs-repair, this flips to the same pipeline gap (fix site `docx_validate.rs`).
- **bodyXml:**

```xml
<w:p><w:r><w:t>Body</w:t></w:r></w:p><w:sectPr><w:headerReference w:type="default" r:id=""/><w:pgSz w:w="12240" w:h="15840"/></w:sectPr>
```

### 3. Two `w:type="default"` header references in one section (open question, pending confirmation against real Word)
- **Check against real Word:** does the document open clean?
- **Expected (unconfirmed):** §17.10.5 / §17.18.36 make "more than a single header or footer of each type" ISO non-conformant, but ISO non-conformance ≠ Word needs-repair. MS-OI29500 §17.10.5 / §17.18.36 record no per-type-budget enforcement, and Word is documented as lenient with duplicate sibling elements (silently uses one). No per-`(kind,type)` budget check exists in any `docx_validate*.rs`. The repair claim is pure ISO prose with zero Word corroboration (MS-OI prose has been overturned twice against real Word — never invert on prose alone). Only checking against real Word can settle whether duplicate-default-header triggers a repair. If confirmed, add a per-`(kind,type)` budget check in `src/docx_validate.rs`.
- **bodyXml:**

```xml
<w:p><w:r><w:t>Body</w:t></w:r></w:p><w:sectPr><w:headerReference w:type="default" r:id="rId1"/><w:headerReference w:type="default" r:id="rId1"/><w:pgSz w:w="12240" w:h="15840"/></w:sectPr>
```

### 4. `headerReference w:type="odd"` — value outside closed ST_HdrFtr enum (existing `#[ignore]`d validator-gap test: `hdrftr_ref_invalid_type_value_nonconformant`)
- **Check against real Word:** does the document open clean?
- **Expected:** does real Word report needs-repair for a type outside `{default, even, first}` (§17.18.36)? stemma's import silently coerces `odd → Default`; `validate()` is package-level and does not walk sectPr enum domains. If Word repairs → real gap (add an ST_HdrFtr enum check). If Word tolerates → coercion is acceptable and the test should assert opens-clean + coerced kind.
- **bodyXml:**

```xml
<w:p><w:r><w:t>Body</w:t></w:r></w:p><w:sectPr><w:headerReference w:type="odd" r:id="rId99"/></w:sectPr>
```

### 5. `headerReference` missing required `w:type` attribute (existing `#[ignore]`d validator-gap test: `hdrftr_ref_missing_type_attribute_nonconformant`)
- **Check against real Word:** does the document open clean?
- **Expected:** `CT_HdrFtrRef` declares `type` with `use="required"`. Does real Word repair a reference with no `type`? stemma's package-level `validate()` does not walk the body, reports ok, and import silently assigns a default kind. If Word repairs → enforce the required attribute (real gap). If Word tolerates → the silent default is acceptable and the test should assert opens-clean.
- **bodyXml:**

```xml
<w:p><w:r><w:t>Body</w:t></w:r></w:p><w:sectPr><w:headerReference r:id="rId99"/></w:sectPr>
```
