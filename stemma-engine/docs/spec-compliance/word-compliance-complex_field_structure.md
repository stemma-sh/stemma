# Word-compliance — Complex field structure (fldChar/instrText nesting, begin/separate/end, unclosed fields)

**Summary:** 0 confirmed gaps, 11 new regression tests, 0 test-bugs discarded, 1 open question (ignored). Build status: green — `cargo test -p stemma --test spec_complex_field_structure_word_compliance` reports **11 passed; 0 failed; 1 ignored**.

This area mines ISO/IEC 29500-1 §17.16 (complex fields) and MS-OI29500 §2.1.454 (nested field type names) for complex-field structural behaviour. All structural/ordering/validity constraints stemma already satisfies (11 regression tests pin them). One read-side behaviour could not be confidently classified as a fixable engine gap without checking against real Word and is left as an open question.

## Confirmed incompliances

None confirmed. The single divergence found (result-less field code leaking into read text) is left as an open question below rather than asserted as a confirmed gap, because the correct Word reading of a `begin`/codes/`end` field with **no** `separate` has not yet been confirmed against real Word, and the §17.16.18 reading of "result region" vs. "field-code region" is interpretation-sensitive.

## New regression tests (passing — pinned behaviour)

| Test | One-line rule |
| --- | --- |
| `complex_field_codes_resume_after_inner_field_end` | §17.16.2 — outer instrText fragment stays after the inner `end` and before the outer `separate`; canonical nested begin/begin/separate/end/separate/end ordering preserved; opens clean. |
| `tracked_field_codes_mixed_ins_del_invisible_in_both_projections` | §17.16.18/§17.16.13 — `instrText` in `w:ins` and `delInstrText` in `w:del` inside a balanced field are non-displayed field codes; accept and reject both read only the result `Yes`; opens clean. |
| `del_instrtext_outside_complex_field_is_deleted_regular_text` | §17.16.13 — a `delInstrText` not inside a field is ordinary deleted text: accept drops it, reject restores `GONE`; opens clean. |
| `complex_field_result_region_text_is_displayed` | §17.16.2/§17.16.18 — the `separate`..`end` region is the displayed result (`Rex Jaeschke`); the `AUTHOR` code is hidden; untracked so accept==reject; opens clean. |
| `orphan_separate_fldchar_without_begin_not_a_field_boundary` | §17.16.18/§17.18.29 — an orphan `separate` with no begin/end is inert; surrounding runs read normally (`alpha beta`); opens clean. |
| `nested_field_type_name_composition_roundtrips_structure` | MS-OI29500 §2.1.454 / §17.16.2 — a non-resolvable outer type name is a Word semantic limit, not a load error; both begin/end pairs and the inner `QUOTE "TE"` instruction survive verbatim; opens clean. |
| `orphan_separate_fldchar_no_begin_reads_literal` | §17.16.18/§17.18.29 — a lone `separate` contributes no text; literals `A` and `B` read under accept and reject; opens clean. |
| `orphan_end_fldchar_no_begin_reads_literal` | §17.16.18/§17.18.29 — a stray `end` opens/closes no field; literals `X` and `Y` read under accept and reject; opens clean. |
| `begin_separate_end_field_with_empty_field_code` | §17.16.2 — a begin/separate/end field with no code serializes in order, fabricates no `instrText`, and reads its cached result `cached`; opens clean. |
| `nested_field_type_name_preserved_opens_clean` | §17.16.2 / MS-OI29500 §2.1.454 — nested begin/end pairs preserved in document order; non-resolvable nested type name opens without repair. |
| `complex_field_begin_carries_flddata_child_preserved_and_opens_clean` | §17.16.18 / §A.1 (CT_FldChar) — a `begin` fldChar carrying a `fldData` child re-emits the `fldData` nested inside the same fldChar with base64 intact; opens clean. |

## Discarded test-bugs

None. No test in this file encoded a wrong expectation.

## Open questions — pending confirmation against real Word

| Test | Check against real Word | Expected value | bodyXml |
| --- | --- | --- | --- |
| `complex_field_no_separate_result_runs_are_field_codes` | read accept text (and reject text — untracked, so they must match) | `before  after` (the `HELLO` run between `begin` and `end`, with no `separate`, is field code and must not be displayed) | see below |

```xml
<w:p><w:r><w:t xml:space="preserve">before </w:t></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText>PAGE</w:instrText></w:r><w:r><w:t>HELLO</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:t xml:space="preserve"> after</w:t></w:r></w:p><w:sectPr/>
```

**What stemma does vs. what the test asserts:** stemma reads `before HELLO after`, surfacing the `<w:t>HELLO</w:t>` run that sits between `begin` and `end`. The test asserts `before  after` on the §17.16.18 reading that, with no `separate`, the entire begin..end span is field code and the field has no result region to display. If real Word confirms `before  after`, this is a real pipeline-level read-projection gap (field-code-region detection ignores a missing-`separate` field, so it treats text runs between begin and end as displayable result). Suggested fix site: the read-projection field-walk that classifies runs as code vs. result — when a field has a `begin` and `end` but no `separate`, no run inside the span is result. If real Word instead shows Word displaying `HELLO`, the test expectation is wrong and the test becomes a test-bug to discard.
