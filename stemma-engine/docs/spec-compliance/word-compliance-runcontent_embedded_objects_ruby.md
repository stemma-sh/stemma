# Word-compliance sweep — Embedded run content and phonetic guides (drawing, object, ruby, contentPart, control)

**Summary:** 1 confirmed gap, 9 new regression tests, 0 test-bugs discarded, 2 open questions (pending confirmation against real Word). Build status: green — `cargo test -p stemma --test spec_runcontent_embedded_objects_ruby_word_compliance -- --test-threads=1` reports **9 passed, 0 failed, 3 ignored**.

## Confirmed incompliances

Ranked: pipeline-bug > model-bug, high > low confidence.

### 1. `omath_nested_inside_omath_opens_unclean` — pipeline-bug, high confidence

- **Rule:** A document containing an `m:oMath` element nested directly inside another `m:oMath` element is rejected by Word (it fails to open); a conforming Word-compatible engine must not report it as opens-clean.
- **§refs:** ISO 29500-1 §22.1.2.77; MS-OI29500 §2.1.1687.
- **Classification:** pipeline-bug.
- **What stemma does vs. what Word does:** `stemma::api::validate` returns `ok=true, issues=[]` for an `m:oMath` nested directly inside another `m:oMath`. MS-OI29500 §2.1.1687 states "Word fails to open a file with an oMath element inside a math object argument or inside another oMath element." stemma's validator does not enforce this, so it reports clean for markup Word refuses to open. The assertion `!opens_clean(&b)` fails because `validate` is clean.
- **Suggested fix site:** `stemma-engine/src/docx_validate_annotations.rs` — add a new structural check (e.g. `check_omath_placement`), registered from `check_annotations_and_structure` in `stemma-engine/src/docx_validate.rs` alongside the other story-tree walks. The validator already parses every story part into an xmltree and walks it with full ancestor paths (see `is_math_context` in `docx_validate_annotations.rs`, which already detects `oMath`/`oMathPara` ancestry), so the data needed is present — there is simply no check that emits a finding when an `m:oMath` element has an `m:oMath` ancestor (or, for the sibling test, when `m:oMath` is a direct child of `w:body` outside any `w:p`).
- **Minimal repro (bodyXml):**

```xml
<w:p><m:oMath xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math"><m:r><m:t>a</m:t></m:r><m:oMath><m:r><m:t>b</m:t></m:r></m:oMath></m:oMath></w:p><w:sectPr/>
```

## New regression tests

These passed against the current engine and now run daily as regression guards.

- `content_part_run_child_imports_clean` — a run carrying `w:contentPart` is conformant run content; stemma imports + validates without error (ECMA §17.3.3.2; MS-OI29500 §2.1.102).
- `embedded_object_run_roundtrips_byte_stable_and_opens_clean` — `w:object` + `w:objectEmbed` is opaque run content; the wrapper and OLE `r:id` survive roundtrip and the package validates (§17.3.3.19 / §17.3.3.20).
- `embedded_object_reads_as_single_object_replacement_char` — an embedded object surfaces as exactly one U+FFFC between surrounding runs (§17.3.3.19; Unicode 5.4.6).
- `ruby_phonetic_guide_roundtrips_structure_and_child_order` — `w:ruby` survives roundtrip preserving `rubyPr` → `rt` → `rubyBase` order and opens clean (§17.3.3.25 / §17.3.3.24 / §17.3.3.27).
- `ruby_base_text_absent_from_read_surface_one_object_char` — ruby reads as exactly one U+FFFC; `rubyBase` glyph never leaks into run text; reject == accept with no tracked change (§17.3.3.25 / §17.3.3.27).
- `ruby_child_order_rubypr_rt_rubybase_preserved_opens_clean` — CT_Ruby strict sequence `rubyPr` → `rt` → `rubyBase` preserved on re-serialize and opens clean (§17.3.3.25; §A.1).
- `embedded_object_vml_drawing_objectembed_roundtrip_opens_clean` — OLE object with VML pict static rep + `w:objectEmbed`; VML, `objectEmbed`, and `dxaOrig` survive and the package stays valid (§17.3.3.19 / §17.3.3.20; §A.1).
- `ruby_read_surface_is_single_anchor_not_duplicated_base_text` — ruby contributes exactly one anchor; guide text `tō` never appears and the base is not double-counted (§17.3.3.25).
- `drawing_multiple_children_last_wins_read_surface` — a multi-child `w:drawing` is consumed as ONE object (last child wins): exactly one U+FFFC, not one per child; reject == accept (§17.3.3.9; MS-OI29500 §2.1.104).

## Discarded test-bugs

None. No test in this area encoded a wrong expectation.

## Open questions — held from the confirmed list

These are `#[ignore]`d, held back from the confirmed list pending classification or confirmation against real Word. Both look like genuine Word-compliance gaps.

- `content_part_reads_as_single_object_replacement_char` — `Document::parse` panics: `RuntimeError { code: InvalidDocx, message: "word IR error: unknown run-level element: contentPart" }`. Per ECMA §17.3.3.2 the consumer "should continue to process the file" and MS-OI29500 §2.1.102 documents Word loads `contentPart` (silently ignoring an unrecognized root namespace). stemma's run-atom classifier (`word_ir.rs`) omits `contentPart` and fails-fast as `UnknownRunElement`, so it cannot even read a document Word opens fine. Expected accept-text: `See \u{FFFC} now`.
- `omath_outside_paragraph_opens_unclean` — `stemma::api::validate` returns `ok=true, issues=[]` for an `m:oMath` that is a direct child of `w:body` (outside any `w:p`). MS-OI29500 §2.1.1687: "Word will not open a file where oMath occurs outside of any p element." stemma's validator does not flag body-level `oMath`, so it reports clean for a file Word refuses to open. (Same fix site as confirmed gap #1.)

## Open questions — pending confirmation against real Word

Confirmed gaps and open items with a real-Word expectation, for gold-checking against real Word. The check in each case: does the document open clean in real Word?

| Test | Check against real Word | Expected | bodyXml |
| --- | --- | --- | --- |
| `omath_nested_inside_omath_opens_unclean` | does the document open clean? | Word fails to open (file repair) — not clean | `<w:p><m:oMath xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math"><m:r><m:t>a</m:t></m:r><m:oMath><m:r><m:t>b</m:t></m:r></m:oMath></m:oMath></w:p><w:sectPr/>` |
| `omath_outside_paragraph_opens_unclean` | does the document open clean? | Word will not open a file where oMath occurs outside any p element — not clean | `<m:oMath xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math"><m:r><m:t>z</m:t></m:r></m:oMath><w:p><w:r><w:t>after</w:t></w:r></w:p><w:sectPr/>` |
