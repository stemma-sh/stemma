# Word-compliance — Footnote/endnote reference plumbing and content stories

**Summary:** 0 confirmed gaps, 12 new regression tests, 0 test-bugs discarded, 1 open question parked for confirmation against real Word / validator work. Build status: GREEN — `cargo test -p stemma --test spec_footnote_endnote_references_word_compliance -- --test-threads=1` reports 12 passed, 0 failed, 0 ignored.

Area: footnote/endnote *reference* runs in the body story (`w:footnoteReference`, `w:endnoteReference`, CT_FtnEdnRef) and how Word consumes/round-trips them (§17.11.7, §17.11.10, §17.11.14, §17.18.10, Annex A CT_FtnEdnRef / ST_DecimalNumber / ST_OnOff). Methodology: verbatim-preservation markup (ids, rStyle, customMarkFollows) is asserted via `reserialize()`; consumption semantics (one anchor object-char in the body, reject-all == baseline) via `read_accepted()`/`read_rejected()` + `to_text()`; Word's note-id ceiling (<=32767) via `validate()`.

Note: the two note-id-ceiling tests (`footnote_reference_id_above_word_max_not_clean`, `endnote_reference_id_above_word_max_not_clean`) were open questions in an earlier pass; they are now fixed in the engine (`check_footnote_endnote_id_range` now range-checks in-body reference ids, not just note definitions) and pass as regression tests.

## Confirmed incompliances

None were promoted to CONFIRMED-GAP. Every assertion the suite encodes with confidence currently passes against the engine — the reference-run plumbing (id/rStyle/customMarkFollows verbatim preservation, single-anchor read text, range-ceiling validation, reject-all baseline equivalence) is Word-compliant in stemma today.

One additional behaviour (a dangling reference id with no matching note definition) is a real, plausibly conformance-blocking gap but needs checking against real Word to confirm the exact "needs repair" verdict before it becomes a gate. It is recorded as an open question below rather than asserted as a confirmed gap, so the suite encodes only behaviour we are confident about. The corresponding test is not present in the suite (it would fail today); it is carried here and in the open questions pending confirmation against real Word.

## Open questions (pending classification / confirmation against real Word)

Ranked: pipeline-bug > model-bug, high > low confidence.

### 1. `footnote_reference_dangling_id_not_in_part_is_nonconformant` — no `footnoteReference` -> footnotes-part xref check (model/validator-bug, high confidence)

- **Rule:** a `w:footnoteReference w:id="N"` whose `N` is not present as a footnote definition in the footnotes part is non-conformant; Word would not open it clean, so the validator must report an error.
- **§refs:** ISO 29500-1 §17.11.14 (the reference id is the match key into the footnotes part); cross-reference §17.11.10 (footnote definitions), §17.18.10 (ST_DecimalNumber id space).
- **Classification:** model-bug / validator gap (the validity contract does not model the body-reference -> note-definition cross-reference), not a read-projection or serializer issue.
- **What stemma does vs Word:** for a body containing `<w:footnoteReference w:id="999"/>` with no footnotes part (or no id=999 definition), `stemma::api::validate()` returns `ok=true, issues=[]`. The validator only range-checks reference ids against Word's `[-2147483648, 32767]` ceiling (`src/docx_validate_annotations.rs`, the `footnoteReference`/`endnoteReference` arm); it performs no check that the referenced id resolves to an actual note definition. So a dangling note reference is silently accepted. Word is expected to report "needs repair" when id=999 resolves to no footnote.
- **Suggested fix site:** `src/docx_validate_annotations.rs` — extend the existing footnote/endnote reference pass that already range-checks ids so it also resolves each `w:id` against the set of definition ids parsed from `word/footnotes.xml` / `word/endnotes.xml` (and the reserved ids -1 separator / 0 continuationSeparator). Emit an actionable error (`dangling footnoteReference w:id=N: no matching footnote definition in word/footnotes.xml`) when the id is unresolved. This is the same shape as the range check already in place; it adds the cross-part resolution the model is currently missing.
- **Why an open question, not CONFIRMED-GAP:** the spec rule is unambiguous that the id is a match key, but the exact Word verdict (hard "needs repair" vs. tolerant open) has not been confirmed against real Word. To avoid encoding an unconfirmed Word verdict as a gate (and because no fixture currently in the corpus pins it), it is parked here for a single confirmation against real Word before landing.
- **Minimal repro (bodyXml):**

```xml
<w:p><w:r><w:t xml:space="preserve">Referenced text.</w:t></w:r><w:r><w:rPr><w:rStyle w:val="FootnoteReference"/></w:rPr><w:footnoteReference w:id="999"/></w:r></w:p><w:sectPr/>
```

(Build the docx with no `word/footnotes.xml`, or with a footnotes part that does not define id=999.)

## New regression tests

All 12 are active (green) and encode Word-compliant behaviour stemma already satisfies:

- `footnote_reference_run_preserves_rstyle_and_id_through_roundtrip` — the `footnoteReference w:id` and its `rStyle="FootnoteReference"` survive a no-op roundtrip verbatim (§17.11.14).
- `footnote_reference_id_above_word_max_not_clean` — a body `footnoteReference w:id="40000"` is outside Word's `[…, 32767]` ceiling, so `validate()` must report not-ok (MS-OI29500 §2.1.300/§2.1.302, §17.11.14/§17.18.10).
- `endnote_reference_id_above_word_max_not_clean` — same ceiling applies to `endnoteReference w:id="40000"` (§17.11.7/§17.18.10).
- `footnote_reference_custommarkfollows_false_preserved_verbatim` — an explicit `customMarkFollows="0"` is opaque markup round-tripped unchanged and opens clean (§17.11.14, Annex A CT_FtnEdnRef / ST_OnOff).
- `footnote_reference_reserved_id_value_preserved` — the authored id (7) is the stable match key and is round-tripped exactly, opening clean (§17.11.10/§17.11.14, ST_DecimalNumber).
- `endnote_reference_custommarkfollows_preserved_verbatim` — `customMarkFollows="true"` on an `endnoteReference` is re-emitted verbatim and the run survives, opening clean (§17.11.7, CT_FtnEdnRef).
- `footnote_reference_in_body_contributes_one_object_char_not_note_text` — accepted body text is the run text plus exactly one U+FFFC anchor, never the note prose (§17.11/§17.11.14).
- `footnote_reference_custommarkfollows_value1_preserved_verbatim` — `customMarkFollows="1"` (numbering-skip flag) survives the roundtrip rather than being dropped as a default (§17.11.14, §22.9.2.7).
- `endnote_reference_in_body_contributes_one_object_char_not_endnote_text` — accepted body text is the run text plus one anchor char, never the endnote prose (§17.11.7/§17.11.8).
- `footnote_reference_anchor_survives_reject_all_when_not_tracked` — an untracked reference is baseline content: both reject-all and accept-all retain run text plus the single anchor (§17.11.14, reject-all == baseline).
- `footnote_and_endnote_references_coexist_as_distinct_anchors_in_one_paragraph` — a footnote and an endnote reference in one paragraph survive the roundtrip as two distinct anchors targeting their respective parts (§17.11.14 / §17.11.7).
- `custommarkfollows_on_footnote_reference_preserved_verbatim` — `customMarkFollows="1"` and the id are both preserved inside the `footnoteReference` start tag (§17.11.14/§22.9.2.7).

## Discarded test-bugs

None. No test encoded a wrong expectation.

## Open questions — pending confirmation against real Word

One finding needs checking against real Word before it can become a gate.

### `footnote_reference_dangling_id_not_in_part_is_nonconformant`

- **Check against real Word:** does the document open clean or need repair?
- **Expected value:** Word reports "needs repair" (document does NOT open clean) because `footnoteReference w:id="999"` resolves to no footnote definition. Equivalently, `validate()` should return `ok=false` with a dangling-reference error.
- **bodyXml:**

```xml
<w:p><w:r><w:t xml:space="preserve">Referenced text.</w:t></w:r><w:r><w:rPr><w:rStyle w:val="FootnoteReference"/></w:rPr><w:footnoteReference w:id="999"/></w:r></w:p><w:sectPr/>
```

Build with no `word/footnotes.xml` (or one that omits id=999). If Word opens it clean, this is NOT a gap (Word tolerates dangling note references) and the finding should be dropped; if Word reports repair, land the xref check in `src/docx_validate_annotations.rs` and promote the test into the suite.
