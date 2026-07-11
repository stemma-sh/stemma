# Word-compliance — Move revisions: moveFrom/moveTo paired ranges, source/dest paragraphs, accept/reject pairing

**Summary:** 1 confirmed gap (paragraph-mark `moveFrom`/`moveTo` not tracked), regression tests covering the move behaviour stemma gets right, 0 test-bugs discarded. Two further divergences are held open as questions (a read-surface question and a Word-forbidden-nesting case whose assertion was left intact, not weakened). Suite: `cargo test -p stemma --test spec_tracked_changes_move_word_compliance` (active tests pass; the open-question cases are `#[ignore]`d).

---

## Confirmed incompliances

The move-revision area has one confirmed gap — paragraph-mark `moveFrom`/`moveTo` is not tracked (below) — plus two divergences held open as questions (a read-surface question, and a Word-forbidden-nesting case whose desired refusal contract needs confirmation against real Word before being promoted to a hard gap).

### Paragraph-mark `moveFrom`/`moveTo` is not tracked; the implied paragraph-break merge never happens (confirmed gap, pending confirmation against real Word)

- **Rule:** a `<w:moveFrom>` in a paragraph's `pPr/rPr` marks the paragraph END mark as moved away — semantically a paragraph-mark deletion — so accepting the move MERGES the paragraph with the following one (and `<w:moveTo>` is the insert analogue). stemma recognizes only `ins`/`del` as paragraph-mark markers and drops the move status, so the merge never occurs.
- **§refs:** ECMA-376 §17.13.5.21 (moveFrom move-source paragraph) / §17.13.5.26 (moveTo paragraph mark); the merge semantics mirror paragraph-mark del (§17.13.5.15: "the contents of this paragraph ... are combined with the following paragraph"). Verbatim §17.13.5.21: "The moveFrom element as a child of the run properties of the paragraph mark specify that this paragraph mark was part of the content which was moved in the document."
- **What stemma does vs Word:** `word_ir.rs::extract_para_mark_status` matches only `w:del`/`w:ins`; `parse_rpr_element` treats `moveFrom`/`moveTo` as unknown rPr children (logs "unknown rPr child element: moveFrom"). The paragraph-mark move status is lost, so accept does NOT merge.
- **Probe (del vs moveFrom paragraph mark, same two-paragraph body):**
  - `<w:del>` paragraph mark, accept → `"First.Second."` (merged — correct).
  - `<w:moveFrom>` paragraph mark, accept → `"First.\n\nSecond."` (NOT merged — bug).
- **Suggested fix site:** `stemma-engine/src/word_ir.rs`, `extract_para_mark_status` — add `moveFrom → Deleted` and `moveTo → Inserted` cases alongside the existing `del`/`ins` handling (and stop treating them as unknown in `parse_rpr_element`).
- **Minimal bodyXml:**
  ```xml
  <w:p><w:pPr><w:rPr><w:moveFrom w:id="1" w:author="A" w:date="2021-01-01T00:00:00Z"/></w:rPr></w:pPr><w:r><w:t>First.</w:t></w:r></w:p><w:p><w:r><w:t>Second.</w:t></w:r></w:p>
  ```
  Expected accept: `First.Second.` (merged). Observed: `First.\n\nSecond.`.

---

## Regression tests

These stay active; each encodes correct, currently-satisfied Word behaviour.

- `orphan_moveto_range_end_no_start_opens_lenient_runs_unaffected` — an orphan `w:moveToRangeEnd` (no matching start) is non-conformant but Word opens it without repair; validate stays clean and accepting all revisions leaves surrounding run text unchanged (§17.13.5.27, §17.13.2).
- `movefrom_run_content_paragraph_pair_accept_drops_source_reject_keeps` — a name-linked source/dest move pair across separate paragraphs: accept drops the `moveFrom` source and keeps the `moveTo` dest (anchor then relocated); reject does the inverse (§17.13.5.22/.25).
- `move_range_markers_paired_by_name_id_distinct_roundtrip` — source/dest linked by identical `w:name`, range ends paired by `w:id` only; serializer re-emits `w:name` on both range starts and id-only on the ends (§17.13.5.27/.23, §17.13.2, §A.1).
- `move_run_content_requires_author_opens_clean` — `moveTo`/`moveFrom` run wrappers carry the schema-required `w:author` (CT_TrackChange base) and it survives re-serialization (§17.13.5.22/.25, §A.1).
- `move_range_start_carries_name_author_date_movebookmark` — `moveFromRangeStart`/`moveToRangeStart` are CT_MoveBookmark and round-trip name+author+date on the same element (§17.13.5.24/.28, §A.1).
- `move_range_end_is_markuprange_no_name_author_date` — `moveFromRangeEnd`/`moveToRangeEnd` are CT_MarkupRange: re-serialize with the linking id only, no name/author/date (§17.13.5.23/.27, §A.1).
- `move_range_end_carries_id_only_not_name` — both range ends pair by `w:id` only and must not carry `w:name` on re-serialize (§17.13.5.23/.27, §A.1).
- `orphan_complete_moveto_container_no_source_reads_as_insertion` — an orphan complete destination container (no source) opens clean; accept keeps the moved content (insertion-like), reject discards the destination copy (§17.13.5.25/.28).
- `duplicate_movefrom_range_start_id_opens_lenient` — duplicate `moveFromRangeStart` `w:id` is non-conformant but Word opens it without repair; stemma must not refuse the file (§17.13.5.24, §17.13.2).
- `moveto_range_end_is_markuprange_id_only_never_name` — destination container with a CT_MoveBookmark start and a CT_MarkupRange end carrying `w:displacedByCustomXml`: `w:name` lives on the start, the end re-serializes with id (+ its own `displacedByCustomXml`) only, never name/author (§17.13.5.27/.28, §A.1).
- `paragraph_mark_moveto_in_rpr_keeps_run_content` — a paragraph-mark moveTo marks only the end glyph; unwrapped run content survives both resolutions (§17.13.5.26).
- `moveto_nested_in_moveto_is_rejected_like_word` — stemma rejects moveTo-in-moveTo like Word (MS §2.1.341); Word does not allow the moveTo element to be a descendant of another moveTo element (I-TC-003 same-type nesting).
- `orphan_movefromrangeend_without_start_still_opens` — an unpaired moveFromRangeEnd is non-conformant per spec but Word opens it leniently; stemma must not error (§17.13.5.23).
- `move_range_displacedbycustomxml_preserved_opens_clean` — `displacedByCustomXml` on move range markers is preserved verbatim and opens clean (§17.13.5.24/.27).
- `intra_paragraph_move_name_links_halves_and_resolves` — intra-paragraph move resolves: accept keeps dest, reject keeps source (§17.13.5.22/.25).

---

## Discarded test-bugs

None. No test here encoded a wrong expectation; nothing was deleted.

---

## Open questions — pending confirmation against real Word

These cases are pending confirmation against real Word before being promoted to a hard confirmed-gap (or reclassified). Both carry a question only real Word settles.

### 1. `move_in_math_treated_as_ins_del_not_move`

- **Check against real Word:** accept-all text.
- **Expected value:** `DEST tail`
- **Observed (stemma accept-all text read):** `\u{fffc} tail` — the math `moveTo` destination is surfaced as a single opaque-anchor placeholder (U+FFFC) rather than the math run's text `DEST`. The destination IS preserved on accept (the anchor survives; ins/del resolution is likely correct); only the text projection diverges. This is a read-surface question (math run text vs. opaque-anchor placeholder), which is why it is parked rather than called a pipeline gap.
- **Rule:** a math `moveTo` reads as an insertion, so accepting keeps the destination math run; the math `moveFrom` (deletion) is dropped — MS-OI29500 §2.1.341 (§17.13.5.25).
- **bodyXml:**

```xml
<w:p><w:moveToRangeStart w:id="0" w:author="A" w:date="2024-01-01T00:00:00Z" w:name="m1"/><w:moveTo w:id="1" w:author="A" w:date="2024-01-01T00:00:00Z"><m:oMath><m:r><m:t>DEST</m:t></m:r></m:oMath></w:moveTo><w:moveToRangeEnd w:id="0"/><w:r><w:t xml:space="preserve"> tail</w:t></w:r></w:p><w:p><w:moveFromRangeStart w:id="2" w:author="A" w:date="2024-01-01T00:00:00Z" w:name="m1"/><w:moveFrom w:id="3" w:author="A" w:date="2024-01-01T00:00:00Z"><m:oMath><m:r><m:t>SRC</m:t></m:r></m:oMath></w:moveFrom><w:moveFromRangeEnd w:id="2"/></w:p><w:sectPr/>
```

### 2. `nested_movefrom_rejected_at_parse`

- **Check against real Word:** does it open clean? (expected: NOT clean / refused-or-flagged) — the test's post-condition is inverted (`!report.ok`).
- **Expected value:** Word does not support nesting `w:moveFrom`, so a Word-faithful engine must NOT open the construct clean. Confirmation against real Word should establish whether real Word repairs/refuses this nesting (and the desired refusal contract) before stemma's lenient `ok=true` (0 issues) is promoted to a hard confirmed-gap fix.
- **Observed (stemma):** `report.ok == true`, 0 issues — stemma best-efforts a Word-forbidden nesting (opens it leniently). Suggested fix site if confirmed: the move-revision validator / parse-time nesting check should fail loud (or emit an issue) on a `w:moveFrom` directly inside another `w:moveFrom`, per the "no silent fallbacks" prime directive.
- **Rule:** Word does not support nesting `moveFrom` — MS-OI29500 §2.1.338 (ECMA-376 §17.13.5.22).
- **bodyXml:**

```xml
<w:p><w:moveFromRangeStart w:id="2" w:author="A" w:date="2024-01-01T00:00:00Z" w:name="m1"/><w:moveFrom w:id="3" w:author="A" w:date="2024-01-01T00:00:00Z"><w:moveFrom w:id="4" w:author="A" w:date="2024-01-01T00:00:00Z"><w:r><w:t>nested</w:t></w:r></w:moveFrom></w:moveFrom><w:moveFromRangeEnd w:id="2"/></w:p><w:sectPr/>
```

---

### 3. Paragraph-mark move-merge gap (confirmed incompliance above)

- **Check against real Word:** does accepting a paragraph-mark `moveFrom` merge the paragraph (as a paragraph-mark del does)?
- Once confirmed, land the `extract_para_mark_status` fix and remove the `#[ignore]`.
