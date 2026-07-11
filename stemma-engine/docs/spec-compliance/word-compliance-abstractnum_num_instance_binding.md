# Word-compliance — Numbering definition instances (abstractNum / num / numId binding, multiLevelType)

**Summary:** 0 confirmed gaps, 2 regression tests, 0 test-bugs discarded, 10 open questions (each surfaces a candidate real gap pending confirmation against real Word). Build status: GREEN — `cargo test -p stemma --test spec_abstractnum_num_instance_binding_word_compliance -- --test-threads=1` reports **2 passed; 0 failed; 10 ignored**.

All ten ignored assertions concern the same family of constraints: Word range-restricts `ST_DecimalNumber` values used as numbering identifiers (plus one length cap on `numId`), even though the underlying schema type (`xsd:integer` / unbounded string) permits the out-of-range values. stemma's `validate()` opens every one of these files clean. The assertions are almost certainly correct (negative numbering ids and over-length ids are documented Word load failures), but they are classified as open questions rather than CONFIRMED-GAP because confirmation requires checking against real Word: stemma faithfully round-trips arbitrary `ST_DecimalNumber`, and "Word refuses to load" must be observed against real Word before the validator is ratcheted to reject. See the open questions below.

## Confirmed incompliances

None promoted to CONFIRMED-GAP. The candidate gaps (validator does not range-check numbering ids) are catalogued under the open questions below; they should be promoted once checking against real Word confirms Word's load behaviour. Classification of every candidate is **pipeline-bug** (validator omission) rather than model-bug — the parser/IR represent the values faithfully; only the validity pass fails to reject them. Confidence is high for the negative-id cases and medium for the 32-char length cap.

## New regression tests

These pass and run in the suite — they pin behaviour stemma already gets right.

- `num_id_value_under_32_chars_opens_clean_and_numbers` — a 10-digit `numId` (`1234567890`) is within Word's 32-char cap, so the bound document opens clean and accept-all yields exactly the paragraph run text (the synthesized list marker is not body content). ISO 29500-1 §17.9.15, §17.9.18.
- `abstractnum_not_directly_referenceable_by_content` — an `abstractNum` cannot be referenced directly by content: `numId=7` matching only an `abstractNumId=7` (no `num` instance) is a dangling reference, so no marker is synthesized and both accept-all and reject-all yield exactly the body run text. ISO 29500-1 §17.9.1, §17.9.18.

## Discarded test-bugs

None. No test in this file encoded a wrong expectation, so none were deleted.

## Open questions — pending confirmation against real Word

Each candidate gap is a validator question: does real Word refuse to load the file (so `validate()` must report `!ok`)? Check against real Word for all entries: Word should FAIL to open; stemma currently opens clean. Expected validator result: **`report.ok == false`**.

1. `negative_abstractnumid_word_will_not_load` — §2.1.275 (ISO §17.9.1, §17.18.10). Negative `abstractNum/@abstractNumId`. Expected: Word will not load; `report.ok == false`. numbering.xml:
```
<w:abstractNum w:abstractNumId="-1"><w:multiLevelType w:val="singleLevel"/><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/><w:lvlJc w:val="start"/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="-1"/></w:num>
```
bodyXml:
```
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Item</w:t></w:r></w:p><w:sectPr/>
```

2. `negative_abstract_num_id_does_not_open_clean` — §2.1.275 (ISO §17.9.1). Same negative-`abstractNumId` numbering.xml as #1. bodyXml:
```
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Numbered item.</w:t></w:r></w:p><w:sectPr/>
```

3. `abstractnum_negative_id_word_will_not_load` — §2.1.275 (ISO §17.9.1, §17.18.10). Negative `abstractNumId`, `lvl` WITHOUT `lvlJc`. numbering.xml:
```
<w:abstractNum w:abstractNumId="-1"><w:multiLevelType w:val="singleLevel"/><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="-1"/></w:num>
```
bodyXml:
```
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>item</w:t></w:r></w:p><w:sectPr/>
```

4. `negative_abstractnumid_does_not_open_clean` — §2.1.275 (ISO §17.9.1, §17.18.10). Negative `abstractNumId`, `lvl` without `lvlJc`. numbering.xml:
```
<w:abstractNum w:abstractNumId="-1"><w:multiLevelType w:val="singleLevel"/><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="-1"/></w:num>
```
bodyXml:
```
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Item</w:t></w:r></w:p><w:sectPr/>
```

5. `negative_num_abstract_num_id_reference_does_not_open_clean` — §2.1.276 (ISO §17.9.2, §17.9.15). `num/@abstractNumId` child reference is negative (`-5`); abstractNum itself valid. numbering.xml:
```
<w:abstractNum w:abstractNumId="0"><w:multiLevelType w:val="singleLevel"/><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/><w:lvlJc w:val="start"/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="-5"/></w:num>
```
bodyXml:
```
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Numbered item.</w:t></w:r></w:p><w:sectPr/>
```

6. `negative_numid_reference_word_restricts_nonnegative` — §2.1.287 (ISO §17.9.18, §17.18.10). Valid numbering (abstractNumId=0); paragraph `numPr/numId` reference is `-1`. numbering.xml:
```
<w:abstractNum w:abstractNumId="0"><w:multiLevelType w:val="singleLevel"/><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/><w:lvlJc w:val="start"/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
```
bodyXml:
```
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="-1"/></w:numPr></w:pPr><w:r><w:t>Item</w:t></w:r></w:p><w:sectPr/>
```

7. `negative_num_id_reference_on_paragraph_does_not_open_clean` — §2.1.287 (ISO §17.9.18). Valid numbering (abstractNumId=0, `lvl` with `lvlJc`); paragraph `numId` reference `-1`. numbering.xml:
```
<w:abstractNum w:abstractNumId="0"><w:multiLevelType w:val="singleLevel"/><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/><w:lvlJc w:val="start"/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
```
bodyXml:
```
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="-1"/></w:numPr></w:pPr><w:r><w:t>Numbered item.</w:t></w:r></w:p><w:sectPr/>
```

8. `negative_numid_reference_does_not_open_clean` — §2.1.287 (ISO §17.9.18, §17.18.10). Valid numbering (`lvl` without `lvlJc`); paragraph `numId` reference `-5`. numbering.xml:
```
<w:abstractNum w:abstractNumId="0"><w:multiLevelType w:val="singleLevel"/><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
```
bodyXml:
```
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="-5"/></w:numPr></w:pPr><w:r><w:t>Item</w:t></w:r></w:p><w:sectPr/>
```

9. `numpr_numid_reference_negative_word_restricts_at_least_zero` — §2.1.287 (ISO §17.9.18, §17.3.1.19). Valid numbering (`lvl` without `lvlJc`); `numPr/numId` reference `-3`. numbering.xml:
```
<w:abstractNum w:abstractNumId="0"><w:multiLevelType w:val="singleLevel"/><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
```
bodyXml:
```
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="-3"/></w:numPr></w:pPr><w:r><w:t>item</w:t></w:r></w:p><w:sectPr/>
```

10. `numid_instance_id_over_32_chars_word_will_not_load` — §2.1.285 (ISO §17.9.15, §17.18.10). `num/@numId` is 33 characters (`123456789012345678901234567890123`), exceeding Word's 32-char cap. Confidence medium — checking against real Word should confirm the exact length boundary (32 vs another value). numbering.xml:
```
<w:abstractNum w:abstractNumId="0"><w:multiLevelType w:val="singleLevel"/><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/><w:lvlJc w:val="start"/></w:lvl></w:abstractNum><w:num w:numId="123456789012345678901234567890123"><w:abstractNumId w:val="0"/></w:num>
```
bodyXml:
```
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="123456789012345678901234567890123"/></w:numPr></w:pPr><w:r><w:t>Item</w:t></w:r></w:p><w:sectPr/>
```

**Suggested fix site (once confirmed against real Word):** the numbering-part validation pass that consumes `word/numbering.xml` and the paragraph `numPr/numId` reference. Add range checks that report a validator error when `abstractNum/@abstractNumId`, `num/@numId`, `num/@abstractNumId`, or `numPr/numId/@val` is negative, plus a length check (>32 chars) on `numId`. The constraint is "Word will not load", so these are load-validity errors (`report.ok == false`), not silent normalizations — consistent with the no-silent-fallback directive.
