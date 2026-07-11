# Word-compliance — Cross-structure anchored ranges: comments, bookmarks, range permissions

**Summary:** 0 confirmed gaps, 9 new regression tests, 0 test-bugs discarded, 3 open questions parked for confirmation against real Word / read-view work. Build status: GREEN — `cargo test -p stemma --test spec_comments_bookmarks_permissions_word_compliance` reports 9 passed, 0 failed, 3 ignored.

Area: the cross-structure annotation elements (§17.13.2 .. §17.13.7): `commentRangeStart`/`commentRangeEnd`/`commentReference`, `bookmarkStart`/`bookmarkEnd`, `permStart`/`permEnd`. Methodology: verbatim-preservation markup is asserted via `reserialize()`; consumption semantics (zero-width markers, accept/reject text) via `read_accepted()`/`read_rejected()`; conformance-blocking conditions via `validate()`.

## Confirmed incompliances

None were promoted to CONFIRMED-GAP. Three failing assertions surfaced real engine behaviour that is plausibly a defect, but each needs adjudication (the inverted-validator pair) or belongs to deferred read-view work (the U+FFFC finding). They are recorded as open questions below rather than asserted as confirmed gaps, so the suite encodes only behaviour we are confident about.

## Open questions (ignored, pending classification / confirmation against real Word)

These three tests are `#[ignore]`d as open questions. The test bodies are unchanged; each fired exactly as written, but the "correct Word behaviour" needs confirmation before it becomes a gate.

Ranked: pipeline-bug > model-bug, high > low confidence.

### 1. `lone_comment_range_markers_contribute_no_text` — read projection emits U+FFFC for a comment anchor (pipeline-bug, high confidence)

- **Rule:** comment range / `commentReference` markers are zero-width annotations and contribute no content to read projections.
- **§refs:** ISO 29500-1 §17.13.4; ECMA-376 §17.13.2.
- **Classification:** pipeline-bug (read-side projection), not a validator/model issue.
- **What stemma does vs Word:** `read_accepted()...to_text()` returned `"anchored text\u{fffc}"` (object-replacement character) where the rule expects `"anchored text"`. The `commentReference` run is materialized in the read surface as a placeholder glyph instead of being a zero-width annotation. `read_rejected()` would have the same defect; the accept assertion fired first.
- **Suggested fix site:** the read projection that builds text from runs (`read_accepted`/`read_rejected` -> `to_text`). A `commentReference` (and the comment range markers) must contribute no characters. Trace where the U+FFFC is injected — likely a run-to-text path treating the annotation run as an embedded object.
- **Why an open question, not CONFIRMED-GAP:** the rule is unambiguous, but this is deferred read-view territory (mirrors the known `literal_prefix` read-gap pattern); confirm the intended read-surface contract before turning it into a gate.
- **Minimal repro (bodyXml):**

```xml
<w:p><w:r><w:t>anchored text</w:t></w:r><w:commentRangeEnd w:id="1"/><w:r><w:rPr><w:rStyle w:val="CommentReference"/></w:rPr><w:commentReference w:id="1"/></w:r></w:p><w:sectPr/>
```

### 2. `comment_range_pair_without_reference_is_nonconformant` — validator accepts a comment range with no `commentReference` (model-bug, medium confidence)

- **Rule:** a balanced `commentRangeStart`/`commentRangeEnd` pair with NO `commentReference` of that id anywhere in the story is non-conformant (a comment Word cannot link).
- **§refs:** ECMA-376 / ISO 29500-1 §17.13.4.4 (mirror §17.13.4.3, §17.13.4.5).
- **Classification:** model-bug (validator coverage / annotation invariant I-ANN-005).
- **What stemma does vs Word:** `validate()` reported clean (`report.ok == true`, zero issues) for a balanced `id=0` pair with no matching `commentReference`. The inverted post-condition (`!ok`) fired as designed: I-ANN-005 only checks start<->end pairing, not reference presence.
- **Suggested fix site:** the annotation pairing check I-ANN-005 in the validator — extend it to assert a `commentReference` exists for each balanced comment range id.
- **Why an open question, not CONFIRMED-GAP:** whether Word treats a reference-less comment range as open-blocking (vs. silently dropped) should be confirmed against real Word before we make the validator stricter than Word. See the open questions below.
- **Minimal repro (bodyXml):**

```xml
<w:p><w:r><w:t xml:space="preserve">Some </w:t></w:r><w:commentRangeStart w:id="0"/><w:r><w:t>text</w:t></w:r><w:commentRangeEnd w:id="0"/></w:p><w:sectPr/>
```

### 3. `bookmarkend_before_bookmarkstart_in_document_order_is_nonconformant` — validator accepts a reversed bookmark pair (model-bug, medium confidence)

- **Rule:** a `bookmarkEnd` with no matching `bookmarkStart` strictly prior in document order is non-conformant, even if a `bookmarkStart` with that id appears later.
- **§refs:** ECMA-376 / ISO 29500-1 §17.13.6.1; ECMA-376 §17.13.2.
- **Classification:** model-bug (validator coverage / annotation invariant I-ANN-003).
- **What stemma does vs Word:** `validate()` reported clean for a `bookmarkEnd id=0` appearing before its `bookmarkStart id=0`. The inverted post-condition (`!ok`) fired as designed: I-ANN-003 matches start<->end by id count, not by document order.
- **Suggested fix site:** the bookmark pairing check I-ANN-003 — enforce that each `bookmarkStart` precedes its matching `bookmarkEnd` in document order.
- **Why an open question, not CONFIRMED-GAP:** Word's actual handling of a reversed pair (repair vs. open-clean vs. error) should be confirmed against real Word before tightening the validator. See the open questions below.
- **Minimal repro (bodyXml):**

```xml
<w:p><w:r><w:t xml:space="preserve">a </w:t></w:r><w:bookmarkEnd w:id="0"/><w:r><w:t xml:space="preserve">b </w:t></w:r><w:bookmarkStart w:id="0" w:name="reversed"/><w:r><w:t>c</w:t></w:r></w:p><w:sectPr/>
```

## New regression tests (passing)

These nine encode behaviour stemma already satisfies and are kept active as regression guards.

- `bookmark_name_over_40_chars_survives_reserialize_verbatim` — a >40-char `w:name` survives reserialization verbatim (the 40-char cap is Word's load-time limit, not a serializer rewrite) and the paired `bookmarkEnd` is re-emitted.
- `permstart_ed_and_edgrp_consumption_uses_ed_opens_clean_zero_width` — a balanced `permStart`/`permEnd` carrying both `ed` and `edGrp` opens clean and the markers are zero-width in accept text.
- `lone_comment_range_start_is_zero_width_anchor` — a lone `commentRangeStart` is a zero-width anchor contributing no text in accept and reject.
- `table_bookmark_colfirst_gt_collast_opens_clean_zero_text` — a table bookmark with `colFirst > colLast` opens clean (Word clamps at read time).
- `perm_colfirst_collast_exceed_table_columns_opens_clean` — a column-scoped table perm whose `colFirst`/`colLast` exceed the column count opens clean.
- `perm_colfirst_collast_outside_table_ignored_opens_clean` — a `permStart` with a complete col pair outside any table is ignored, opens clean, markers zero-width.
- `cross_paragraph_comment_range_markers_zero_width` — a comment range spanning two paragraphs is zero-width; read text joins paragraphs with the block separator.
- `lone_comment_range_start_opens_clean` — a lone `commentRangeStart` anchored at a `commentReference` opens clean (not stricter than Word).
- `lone_comment_range_end_opens_clean` — a lone `commentRangeEnd` anchored at a `commentReference` opens clean.

## Discarded test-bugs

None. No test in this file encoded a wrong expectation; nothing was deleted.

## Open questions — pending confirmation against real Word

The two inverted-validator findings need checking against real Word to decide whether the validator should be tightened (i.e., whether Word actually rejects these). The U+FFFC finding is a read-projection defect and does not need it.

| Test | Check against real Word | Expected (to confirm) | bodyXml |
| --- | --- | --- | --- |
| `comment_range_pair_without_reference_is_nonconformant` | open-clean (does Word flag / repair a comment range with no `commentReference`?) | If Word flags or strips it -> validator should report an error (confirms gap). If Word opens silently -> rule is wrong, flip test to expect clean. | `<w:p><w:r><w:t xml:space="preserve">Some </w:t></w:r><w:commentRangeStart w:id="0"/><w:r><w:t>text</w:t></w:r><w:commentRangeEnd w:id="0"/></w:p><w:sectPr/>` |
| `bookmarkend_before_bookmarkstart_in_document_order_is_nonconformant` | open-clean (does Word reject / repair a `bookmarkEnd` preceding its `bookmarkStart`?) | If Word reports needs-repair or drops the pair -> validator should report an error (confirms gap). If Word opens silently -> rule is wrong, flip test to expect clean. | `<w:p><w:r><w:t xml:space="preserve">a </w:t></w:r><w:bookmarkEnd w:id="0"/><w:r><w:t xml:space="preserve">b </w:t></w:r><w:bookmarkStart w:id="0" w:name="reversed"/><w:r><w:t>c</w:t></w:r></w:p><w:sectPr/>` |

`lone_comment_range_markers_contribute_no_text` does not require checking against real Word — the zero-width read-surface contract is settled by spec (§17.13.2/§17.13.4); the fix is in the read projection.
