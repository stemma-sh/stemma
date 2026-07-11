# Word-compliance sweep — OPC relationships & part addressing: rId resolution, TargetMode External, relative-reference resolution, .rels naming

**Summary:** 1 confirmed gap, 3 new regression tests, 0 test-bugs discarded. Build green: `cargo test -p stemma --test spec_opc_relationships_addressing_word_compliance -- --test-threads=1` => 3 passed, 0 failed, 9 ignored (1 confirmed gap + 8 open questions).

All nine dangling-reference assertions exercise the same underlying divergence: `stemma::api::validate` (the public package validator) does not run the I-REL relationship-resolution family, so a `.docx` whose body names an `r:id` / `r:embed` / `r:link` that has no backing `Relationship` in `word/_rels/document.xml.rels` is reported `ValidationReport { ok: true, issues: [] }`. Word, by contrast, cannot resolve the reference and reports the file as needing repair (it drops the picture / object / header / hyperlink target). One of the nine (the `r:link` linked-image case) is classified as a CONFIRMED GAP with a precise fix site; the other eight are recorded as open questions because they share the same root cause but warrant a single coordinated engine fix plus confirmation against real Word before being treated as confirmed gaps.

---

## Confirmed incompliances

Ranked: pipeline-bug, high confidence.

### 1. Dangling `a:blip r:link` linked image is not flagged (`dangling_blip_link_flagged_no_silent_drop`)

- **Rule:** A DrawingML picture whose `a:blip r:link` names a relationship Id absent from `word/_rels/document.xml.rels` is a dangling explicit reference; `validate` must report I-REL-001 and `opensClean` must NOT hold (a linked image with no backing relationship cannot be located).
- **§refs:** ECMA-376 Part 2 OPC §6.5.3; ISO 29500-1 §9.2.
- **Classification:** pipeline-bug.
- **What stemma does vs what Word does:** `validate()` returns `ok: true` for a full `pic:pic` with `r:link="rIdLinkMissing"` and no backing relationship. The `r:link` resolution branch is not checked by the public `validate` path, so `opens_clean(&b)` is `true`. Word cannot resolve the linked image and reports the file as needing repair (not open-clean). The assertion `!opens_clean(&b)` therefore fails against the current engine.
- **Suggested fix site:** `stemma-engine/src/runtime.rs: validate_docx_report()`. The curated structural-check block omits relationship-resolution. Wire in the relationship-reference resolution check on the production path: collect `r:id` / `r:embed` / `r:link` from `word/document.xml` (reuse `docx_validate::collect_relationship_references` / `is_relationship_reference_attr`) and resolve them against `word/_rels/document.xml.rels`, emitting I-REL-001 when an Id is missing. The fix must cover all surfaces the sibling tests exercise (blip `r:embed`, blip `r:link`, `w:hyperlink r:id`, `headerReference r:id`, OLE `r:id`), not just `r:link`, and must preserve the empty-reference negative case (`empty_rembed_rlink_not_dangling`, already handled by the rich validator via the `!value.is_empty()` guard). The deeper structural fix is to stop maintaining two divergent validators: route `api::validate` through the same checks `docx_validate::validate_docx` runs, or keep the curated subset and the rich validator in sync.
- **Minimal bodyXml repro:**

```xml
<w:p><w:r><w:drawing><wp:inline xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"><wp:extent cx="914400" cy="914400"/><wp:docPr id="2" name="Picture 2"/><a:graphic xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:nvPicPr><pic:cNvPr id="2" name="Picture 2"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:link="rIdLinkMissing" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="914400"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p><w:sectPr/>
```

### Open questions (same root cause, pending coordinated fix + confirmation against real Word)

The following eight assertions fail for the identical reason (the public `validate` path does not run the I-REL family) but are recorded as open questions so they ride the same engine fix and are confirmed against real Word together rather than each being independently promoted:

- `dangling_rembed_blip_flagged` — dangling `a:blip r:embed` (I-REL-001) reported `ok:true`. ECMA-376 Part 2 OPC §6.5; ISO 29500-1 §9.2.
- `dangling_rlink_blip_flagged` — dangling `a:blip r:link` (would-be External linked image) reported `ok:true`. ECMA-376 Part 2 OPC §6.5; ISO 29500-1 §9.2.
- `dangling_hyperlink_rid_must_not_open_clean` — dangling `w:hyperlink r:id` reported `ok:true`. ISO 29500-1 §9.2, §17.16.22.
- `dangling_blip_embed_must_not_open_clean` — dangling `a:blip r:embed` against empty rels reported `ok:true`. ISO 29500-1 §9.2, §17.3.3.9.
- `dangling_blip_embed_flagged_no_silent_drop` — full `pic:pic` with dangling `r:embed` reported `ok:true`. ECMA-376 Part 2 OPC §6.5.3; ISO 29500-1 §9.2, §J.5.
- `dangling_header_reference_rid_flagged` — dangling `w:headerReference r:id` reported `ok:true`. ISO 29500-1 §17.10.5, §9.2.
- `dangling_drawingml_hlinkclick_rid_flagged` — dangling `a:hlinkClick r:id` (+ blip `r:embed`) reported `ok:true`. ISO 29500-1 §9.2, §J.5.
- `dangling_oleobject_rid_flagged` — dangling `o:OLEObject r:id` reported `ok:true` (confidence medium per spec). ISO 29500-1 §17.3.3.

---

## New regression tests

These passing tests stay active as regression guards.

- `empty_rembed_rlink_not_dangling` — empty `r:embed`/`r:link` name no relationship and must NOT be treated as dangling; the package validates clean (no I-REL-001 false positive). ECMA-376 Part 2 OPC §6.5; ISO 29500-1 §9.2.
- `package_rels_internal_target_resolves_from_package_root` — an Internal Target in `/_rels/.rels` resolves against the package root, so `Target="word/document.xml"` resolves and the package opens clean (no I-REL-003, no I-PKG-001). ISO 29500-2 OPC §6.5.2, §6.4.2.
- `relationships_part_is_not_content_typed_and_has_no_rels_to_itself` — a Relationships part is exempt from per-part content-type requirements and is never a relationship source/target; a package whose only auxiliary parts are `_rels` parts opens clean (no I-CT-001/I-CT-002). ISO 29500-2 OPC §6.5.2, §6.5.3.

---

## Discarded test-bugs

None. No test in this file encoded a wrong expectation; every failing assertion reflects a real engine divergence.

---

## Open questions — pending confirmation against real Word

Confirm the following against real Word (opens-clean vs needs-repair). The first is the confirmed gap; the remaining eight share the same root cause and should be confirmed in the same check.

| Test | Check against real Word | Expected | bodyXml |
|------|-------------|----------|---------|
| `dangling_blip_link_flagged_no_silent_drop` | opensClean | Word cannot resolve the linked image and reports the file as needing repair (not open-clean) | see repro above (`a:blip r:link="rIdLinkMissing"`) |
| `dangling_rembed_blip_flagged` | opensClean | Word repairs the file and drops the picture (not open-clean) | `a:blip r:embed="rId999"` minimal pic |
| `dangling_rlink_blip_flagged` | opensClean | Word cannot resolve linked image; needs repair | `a:blip r:link="rId888"` minimal pic |
| `dangling_hyperlink_rid_must_not_open_clean` | opensClean | Word drops/repairs the dangling hyperlink target | `w:hyperlink r:id="rId88"` |
| `dangling_blip_embed_must_not_open_clean` | opensClean | Word repairs; image embed is data loss | `a:blip r:embed="rId99"` |
| `dangling_blip_embed_flagged_no_silent_drop` | opensClean | Word flags for repair (data-loss), not open-clean | full `pic:pic` with `r:embed="rIdMissing"` |
| `dangling_header_reference_rid_flagged` | opensClean | Word cannot resolve section header; needs repair | `w:headerReference r:id="rIdHdrMissing"` in `w:sectPr` |
| `dangling_drawingml_hlinkclick_rid_flagged` | opensClean | Word repairs the dangling hlinkClick / embed | `a:hlinkClick r:id="rIdLinkMissing"` + blip `r:embed="rId1"` |
| `dangling_oleobject_rid_flagged` | opensClean | Word drops the OLE object (data-loss repair) | `o:OLEObject r:id="rId777"` in `w:object` |

The full bodyXml fragments for the eight open-question cases are the verbatim `body` literals in `stemma-engine/tests/spec_opc_relationships_addressing_word_compliance.rs` (each test wraps its fragment in the standard `make_docx` package with an intentionally empty `word/_rels/document.xml.rels`).
