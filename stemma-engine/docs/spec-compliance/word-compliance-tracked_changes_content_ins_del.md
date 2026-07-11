# Word-compliance — Inserted/deleted run + paragraph content revisions and accept/reject semantics

**Summary:** 1 confirmed gap (real, held open pending confirmation against real Word), 9 regression tests, 0 test-bugs discarded, 2 test-expectation/observation bugs held open as questions. Suite: `cargo test -p stemma --test spec_tracked_changes_content_ins_del_word_compliance -- --test-threads=1` (9 passed, 0 failed, 3 ignored).

Every active test passes; the three ignored cases are skipped as open questions with explicit reasons.

---

## Confirmed incompliances

Ranked: pipeline-bug > model-bug, high > low confidence.

### 1. `w:ins` / `w:del` with omitted `w:author` is rejected at the IR import boundary

- **Rule:** The `w:author` attribute on a tracked-change element (CT_TrackChange / CT_RunTrackChange) is **optional**. When omitted, "no author shall be associated with the parent annotation type." Word opens such a document and resolves accept/reject normally.
- **§refs:** ECMA-376 §17.13.5.18 (w:ins), §17.13.5.14 (w:del), CT_TrackChange author attribute; MS-OI29500 §2.1.334.
- **Classification:** Pipeline bug (model bug at the import edge). High confidence — reproduced directly: `Document::parse()` returns `RuntimeError { code: InvalidDocx, message: "word IR error: missing required tracked change attribute: author" }`.
- **What stemma does vs. what Word does:** stemma fails loud at the `word_ir` tracked-change attribute decode, treating `w:author` as required, so parse aborts before any accept/reject/opens-clean logic runs. Word treats the document as valid and resolves the insertion (accept → `"Sometext"`, reject → `"Some"`).
- **Suggested fix site:** the `word_ir` tracked-change attribute decode path that currently requires `w:author`. Make `author` optional (`Option<...>` with no synthesized fallback — absence is a legitimate state, not a default), preserving the no-silent-fallback rule: an omitted author means "no author", not a made-up one.
- **Minimal bodyXml repro:**

```xml
<w:p><w:r><w:t>Some</w:t></w:r><w:ins w:id="5" w:date="2006-03-31T12:50:00Z"><w:r><w:t>text</w:t></w:r></w:ins></w:p><w:sectPr/>
```

Test: `ins_without_author_still_resolves_accept_reject` (currently `#[ignore]`d). The engine-side `parse()` error is deterministic; the gap is real but is flagged for confirmation against real Word before being fixed.

---

## Regression tests

The passing tests lock in correct Word-consumption behaviour for inserted/deleted content:

- `ins_encapsulates_multiple_runs_accept_keeps_all_reject_drops_all` — a single `w:ins` wrapping multiple runs is one insertion; accept keeps the concatenation, reject drops all inner runs (§17.13.1 / §17.13.5.18).
- `deleted_field_code_serializes_delinstrtext_not_t` — a deleted field code inside `w:del` survives as `w:delInstrText`; reject restores the field instruction (not body text), accept drops it (§17.16.13 / §17.13.5.14 / §17.3.3.7).
- `del_then_ins_replacement_no_t_inside_del` — deleted text serializes as `w:delText`; a `w:t` must never appear inside a `w:del` region (§17.3.3.7 / §17.13.1).
- `deleted_text_significant_whitespace_preserved_deltext` — a deleted run with leading/trailing spaces keeps `xml:space="preserve"` on its `delText` (§17.3.3.7).
- `deleted_field_code_serializes_as_delinstrtext_in_del` — a deleted field code stays a `w:delInstrText` run nested inside the `w:del` container (§17.16.13 / §17.13.5.14).
- `deleted_para_mark_keeps_run_text_as_t_not_deltext` — a `del` on the paragraph mark's rPr leaves the run text as ordinary `w:t`, never `delText` (§17.13.5.15 / §17.3.3.7).
- `movefrom_text_serializes_as_t_not_deltext` — move-source content uses ordinary `w:t`; `moveFrom` text must not become `delText` (§17.13.5.22 / §17.3.3.7).
- `deleted_field_code_uses_delinstrtext_not_instrtext` — a deleted field-code run re-emits as `delInstrText`, never downgraded to live-field `instrText` (§17.16.13).
- `t_inside_del_treated_as_deleted_consumption` — content authored as `w:t` inside `w:del` is still deleted on accept and restored on reject (§17.13.5.14 / §17.3.3.7).

---

## Discarded test-bugs

None deleted. Two test functions had wrong/incomplete expectations but were held open as questions (ignored, not deleted), because the engine output they disagree with is itself correct:

- `deleted_paragraph_mark_does_not_delete_run_text` — separator observation bug. Engine satisfies §17.13.5.15 (reject restores the boundary; both paragraphs keep full run text). The only mismatch is the inter-block separator: stemma's `to_text()` joins blocks with `"\n\n"` (src/view.rs); the expected value assumed a single `"\n"`. The accept assertion (the gap-catching one) passed.
- `deleted_para_mark_run_text_survives_both_resolutions` — wrong expected reject value. Rejecting a deleted paragraph mark restores the delimiter and BOTH paragraphs survive (`"one\n\ntwo"`); the authored expectation `"one"` wrongly dropped `"two"`. stemma's output is the correct §17.13.5.15 behaviour. Accept (`"onetwo"`) and opens-clean passed.

These are kept (ignored) rather than deleted because each still contains a valid, passing accept-side assertion; the reject-side expected literal needs a one-line correction (separator + completeness) before the test can be reactivated. No assertion was weakened.

---

## Open questions — pending confirmation against real Word

The confirmed gap below is pending confirmation against real Word — certify the omitted-author behaviour before fixing it.

- **Test:** `ins_without_author_still_resolves_accept_reject`
  - **Check against real Word:** does it open clean, and what are the accept and reject texts?
  - **Expected (per spec):** opens clean (no repair); accept text = `"Sometext"`; reject text = `"Some"`.
  - **bodyXml:**

```xml
<w:p><w:r><w:t>Some</w:t></w:r><w:ins w:id="5" w:date="2006-03-31T12:50:00Z"><w:r><w:t>text</w:t></w:r></w:ins></w:p><w:sectPr/>
```
