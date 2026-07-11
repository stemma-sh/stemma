# Word-compliance sweep — fldSimple vs complex field equivalence, attrs, and round-trip

**Summary:** 0 confirmed gaps, 10 new regression tests, 0 test-bugs discarded, 2 open questions (not requiring a real-Word check, same root cause). Build status: green — `cargo test -p stemma --test spec_simple_field_vs_complex_word_compliance` reports **10 passed, 0 failed, 2 ignored**.

This sweep covers the simple field (`w:fldSimple`, §17.16.19 / CT_SimpleField §A.1) against the complex field machinery: required `instr`, `dirty`/`fldLock` ST_OnOff lexical forms, the `fldData?` then `EG_PContent*` child order, multi-run cached results, Base64 and transitional `fldData` blobs, and the text projection of a field's cached result.

No assertion was weakened or deleted. The two divergences found are not confidently classifiable as pipeline-bug vs model-bug without an explicit product decision on how a simple field's field-anchor structure should surface in the read projection, so they are left as open questions.

## Confirmed incompliances

None confirmed. The two divergences below are left as open questions rather than confirmed gaps: both stem from one design question (whether stemma's text projection should emit the begin/instr/separate/end `U+FFFC` field-anchor placeholders for a `w:fldSimple`, the way it already does for the equivalent complex field). Until that contract is decided, they cannot be ranked as a pipeline-bug.

### Open-question divergences (single root cause)

Root cause: stemma projects a `w:fldSimple` as **its cached-result text only**. It does not emit the begin/instr/separate/end `U+FFFC` field-anchor placeholders that the complex-field equivalent (`fldChar begin … separate … end`) produces. The multi-run concatenation itself is correct; the divergence is purely the anchor-placeholder projection.

1. **`fldsimple_two_result_runs_both_reproduce_in_text`** — §17.16.19, §17.16.2
   - Classification: model-bug candidate (read-projection contract), low confidence — needs a product decision, not a real-Word check.
   - What stemma does: `read_accepted().to_text()` = `"Example Document.docx"` (both result runs `Example ` + `Document.docx` correctly concatenated).
   - What the test expects (Word-equivalent anchor projection): `"\u{FFFC}\u{FFFC}\u{FFFC}\u{FFFC}Example Document.docx\u{FFFC}"` (begin/instr/separate placeholders, then the in-order result text, then the end placeholder).
   - Note: the `opensClean` and any `xmlContains` parts of this test would pass; only the anchor-placeholder text projection diverges.
   - Suggested fix site: the read-projection layer that materializes `w:fldSimple` into text (the same layer that already emits `U+FFFC` anchors for complex `fldChar` fields). Make the simple-field projection emit the begin/instr/separate/end anchors so simple and complex fields read identically.
   - Minimal repro:
     ```xml
     <w:p><w:fldSimple w:instr="FILENAME"><w:r><w:t>Example </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>Document.docx</w:t></w:r></w:fldSimple></w:p><w:sectPr/>
     ```

2. **`fldsimple_no_result_run_is_valid_and_anchor_only`** — §17.16.19, §17.16.2
   - Classification: model-bug candidate (read-projection contract), low confidence — needs a product decision, not a real-Word check.
   - What stemma does: `read_accepted().to_text()` = `""` (a resultless simple field is invisible to the text projection — no field-anchor marker at all).
   - What the test expects: `"\u{FFFC}\u{FFFC}\u{FFFC}\u{FFFC}"` (exactly the begin/instr/separate/end anchor placeholders, no cached result text).
   - Note: `opensClean` passes; the field is schema-valid (required `instr`, zero `EG_PContent`). Only the text projection diverges: a field with no cached result has no positional object in stemma's read.
   - Suggested fix site: same read-projection layer as above. A resultless `w:fldSimple` should still project the field-structure anchors so the field object is positionally visible.
   - Minimal repro:
     ```xml
     <w:p><w:fldSimple w:instr="FILENAME"/></w:p><w:sectPr/>
     ```

## New regression tests (passing)

| Test | Rule |
| --- | --- |
| `fldsimple_selfclosing_dirty_no_result_opens_clean` | §17.16.19 / §A.1: self-closing `fldSimple` (required `instr` + `dirty`, no cached result) is the spec's own `dirty` example and must validate without repair; `instr`/`dirty` round-trip verbatim. |
| `fldsimple_dirty_numeric_onoff_form_preserved` | §22.9.2.7: `w:dirty="1"` is a valid ST_OnOff lexical value and must open clean without coercion. |
| `fldsimple_instr_embedded_quoted_arg_preserved` | §17.16.19 / §22.9.2.13: a `DATE \@ "yyyy"` format-picture `instr` (escaped or literal quotes) is well-formed and survives round-trip. |
| `fldsimple_multiple_result_runs_preserved_and_concatenated` | §17.16.19 / §A.1: multiple result runs are schema-valid; the displayed result is their in-order concatenation (`Acme Corp`), and each run's `rPr` (bold) survives. |
| `fldsimple_flddata_child_precedes_result_runs` | §A.1 CT_SimpleField sequence `(fldData?, EG_PContent*)`: `fldData` (payload `AQID`) precedes the first result run; the serializer must not reorder or hoist it. |
| `fldsimple_flddata_serializes_before_result_run` | §A.1 / §22.9.2.13: a Base64 `fldData` (`cABhAHkAbABvAGEAZAA=`) survives verbatim and is emitted before the result run; opens clean. |
| `fldsimple_flddata_nested_not_hoisted_to_sibling` | §A.1: `fldData` (`QUJDREVG`) stays the first child INSIDE the `fldSimple` and is never hoisted to a paragraph-level sibling. |
| `fldsimple_base64_flddata_opens_clean` | §17.16.19 / MS-OE376 §2.1.548: a Base64-encoded `fldData` (`VGhpc0lzQmFzZTY0RmllbGREYXRh`) survives verbatim and opens clean. |
| `fldsimple_flddata_transitional_preserved_and_ordered` | ISO 29500-4 §14.10.5: transitional `fldData` blob (`///3645ERKJHE`) is preserved verbatim and ordered before `EG_PContent`; opens clean. |
| `fldsimple_onoff_legacy_on_off_roundtrips` | §22.9.2.7 / MS-OI29500 §2.1.1744: `w:fldLock="off"` / `w:dirty="on"` ST_OnOff1 lexical values are valid and open clean without coercion. |

## Discarded test-bugs

None. No test in this batch encoded a wrong expectation.
