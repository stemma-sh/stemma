# Word-compliance — Cell merging: vMerge (vertical) and gridSpan (horizontal) with merge-revision accept/reject

**Summary:** This area is covered by regression tests for the merge behaviour stemma gets right, plus a set of discarded test-bugs. Two divergences remain open pending confirmation against real Word: a candidate vMerge-continuation read-suppression gap and a confirmed parse-strictness gap on negative `gridBefore`/`gridAfter`. Suite: `cargo test -p stemma --test spec_table_cell_vmerge_gridspan_word_compliance -- --test-threads=1` (active tests pass; the open-question cases are `#[ignore]`d).

The most substantive candidate finding (`vmerge_continue_content_suppressed_on_accepted_read`) is a real merge-consumption gap, held open pending confirmation against real Word rather than asserted, because its expected read string is entangled with the same cell-join-separator question that produced the discarded test-bugs.

## Confirmed incompliances

No incompliance is confidently classified as a daily-assertable pipeline-bug or model-bug. The candidate gaps are ranked below and carried in the open questions section.

### 1. vMerge continuation content not suppressed on accepted read (candidate model-bug, medium confidence)

- **Rule:** A `<w:vMerge w:val="continue"/>` cell joins the anchor's merged region; the merged column reads as the anchor's text only. The continuation cell's stored paragraph text is not a separate visible cell.
- **§refs:** ISO 29500-1 §17.4.84 (vMerge); §17.18.57 (ST_Merge).
- **Classification:** model-bug candidate — stemma does not model vertical-merge content suppression on read.
- **What stemma does vs Word:** stemma's `read_accepted().to_text()` returns `"ANCHOR HIDDEN"` — it surfaces the continuation cell's stored `HIDDEN` text as a separate visible cell. Word shows only `"ANCHOR"` for the merged column. (Assertion `accept_text == "ANCHOR"` fails with actual `"ANCHOR HIDDEN"`.)
- **Suggested fix site:** the read/view projection that flattens table cells to text (`view.rs` cell-text / merged-region handling). A vMerge=continue cell should contribute no text to the accepted/visible read, folding into the restart anchor's region.
- **Caveat:** the failing string also rides on stemma's single-space inter-cell join contract, so the exact expected literal must be confirmed against real Word before this becomes an assertion. Carried in the open questions section.
- **Minimal bodyXml repro:**

```xml
<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="2000" w:type="dxa"/><w:vMerge w:val="restart"/></w:tcPr><w:p><w:r><w:t>ANCHOR</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:tcPr><w:tcW w:w="2000" w:type="dxa"/><w:vMerge w:val="continue"/></w:tcPr><w:p><w:r><w:t>HIDDEN</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:sectPr/>
```

### 2. stemma rejects negative `gridBefore` / `gridAfter`; Word ignores them (pipeline-bug, pending confirmation against real Word)

- **Rule:** a negative (or 0) `gridAfter`/`gridBefore` is ignored by Word, not an error — the file opens clean and the value is treated as no-op (clamped to 0).
- **§refs:** MS-OI29500 §2.1.128 (17.4.14 gridAfter): *"Word only ignores 0 or negative values for this element."* MS-OI29500 §2.1.129 (17.4.15 gridBefore): *"Word only understands non-negative values for the val attribute."* The `val` is `ST_DecimalNumber` (signed, ISO 29500-1 §17.18.10).
- **What stemma does vs Word:** `import.rs` parses both with `val.parse::<u32>()` and returns `InvalidDocx { "gridAfter: invalid value \"-1\"" }` (same for `gridBefore`). A signed/negative value makes stemma refuse a document real Word opens — fail-loud where Word is lenient.
- **Suggested fix site:** `stemma-engine/src/import.rs`, the `gridBefore` and `gridAfter` parse blocks — parse as `i64` and clamp `<0` to `0` rather than erroring (and treat the clamped 0 as a no-op, which the existing layout code already does).
- **Minimal bodyXml:**
  ```xml
  <w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="2500"/><w:gridCol w:w="2500"/></w:tblGrid><w:tr><w:trPr><w:gridAfter w:val="-1"/></w:trPr><w:tc><w:p><w:r><w:t>L</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>R</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
  ```
  Expected: opens clean; accepted text `L R`. Observed: `Document::parse` errors with `InvalidDocx: gridAfter: invalid value "-1"`.

## Regression tests

These pass and encode behaviour stemma gets right:

- `explicit_vmerge_continue_value_roundtrips_verbatim` — §17.4.84: an explicit restart/continue vMerge chain in one column is conformant and opens without repair.
- `gridspan_exceeding_grid_augments_grid_serializer_preserves_span` — §17.4.17: an over-large gridSpan augments the grid (opens clean) and the spanning cell's content survives.
- `tcpr_order_gridspan_hmerge_vmerge` — Annex A CT_TcPrBase: re-serialized tcPr orders gridSpan before hMerge before vMerge, and opens clean.
- `gridafter_exceeding_remaining_grid_is_ignored_not_truncating` — MS-OI §2.1.128 / §17.4.14: a gridAfter exceeding the remaining grid is ignored; no cell content is dropped (both LEFT and RIGHT survive).
- `hmerge_val_restart_opens_clean` — ST_Merge §17.18.57: a restart-then-continue horizontal merge is conformant and opens without repair.
- `gridbefore_positive_skips_columns_content_preserved` — in-bounds gridBefore skips leading columns, no cell/text dropped (§17.4.15, MS §2.1.129).
- `gridbefore_exceeding_grid_is_ignored_content_preserved` — out-of-range gridBefore is ignored; row content survives (§17.4.15).
- `gridspan_with_vmerge_restart_anchor_opens_clean` — gridSpan + vMerge=restart on one anchor is conformant; opens clean, anchor text survives (§17.4.17/§17.4.84).
- `cellmerge_rest_split_revision_opens_clean` — `ST_AnnotationVMerge=rest` split revision opens clean; both cells visible (§17.13.5.3/§17.18.1).
- `lone_vmerge_restart_reads_as_ordinary_cell` — a restart with no continuation reads as an ordinary cell (§17.4.84).
- `vmerge_group_mismatched_gridspan_still_opens` — a grid-mismatched vMerge group is non-conformant per spec but Word opens it leniently; stemma must not error (§17.4.84).
- `gridspan_zero_consumed_as_one_grid_column` — a `gridSpan` of 0 is consumed as one grid column; both cells survive (§17.4.17, MS §2.1.130).
- `gridspan_zero_treated_as_one_opens_clean` — `gridSpan=0` opens clean, treated as one column (§17.4.17).
- `orphan_vmerge_continue_without_restart_is_ignored_content_visible` — an orphan continue without a restart is ignored; the cell's content stays visible (§17.4.84).
- `gridafter_zero_is_noop_not_truncation` — a 0 `gridAfter` is a no-op, not a truncation (§17.4.14).
- `cellmerge_cont_revision_rejects_to_unmerged_visible_cell` — rejecting a cellMerge continuation restores the unmerged, visible cell (§17.13.5.3).

## Discarded test-bugs

- `explicit_vmerge_continue_serializes_as_bare_vmerge` — asserted on the wrong path. An unedited `reserialize()` re-zips the stored `word/document.xml` verbatim (it never invokes `serialize_table_node`), so `w:val="continue"` is correctly preserved byte-for-byte. The IR serializer's bare-`<w:vMerge/>` emission only runs on edit/apply/diff paths. §17.4.84 makes explicit `continue` and bare vMerge denote the same continuation, so the opens-clean assertion (which passes) is the only valid one. Consumption-vs-save test-bug; no engine defect.
- `gridspan_zero_occupies_one_grid_column_in_mixed_row` — the cited spec (MS-OI §2.1.130, §17.4.17) governs only grid-column accounting, which stemma already honors (`grid_span.max(1)`; both cells survive). The failing assertion `acceptText == "ABC"` smuggled in an ungrounded bare-concatenation rule; stemma's documented contract space-joins cells (`"A BC"`), matching Word's table read better than bare concat. Also carries a second construction bug (inside-tbl `<w:sectPr/>`).
- `split_cellmerge_reject_restores_merged_state_text_survives` — the load-bearing invariant (cellMerge vMerge=rest is a split-state revision, not a content deletion; text survives both resolutions) is fully satisfied; stemma reads both `Upper` and `Lower`. The only divergence is the expected `"UpperLower"` (bare concat) vs stemma's documented single-space join `"Upper Lower"`. The adjacent sibling test on the same shape correctly asserts the space-joined form. A split yields two separate visible cells, so bare concatenation cannot be justified from the domain rule. Asserted-literal test-bug; no code change.

## Open questions — pending confirmation against real Word

Items pending confirmation against real Word before becoming assertions:

| Test | Check against real Word | Expected value | bodyXml |
|---|---|---|---|
| `vmerge_continue_content_suppressed_on_accepted_read` | accept-all read text | `ANCHOR` (continuation cell's stored text suppressed into the anchor's merged region) | see repro below |

```xml
<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="2000" w:type="dxa"/><w:vMerge w:val="restart"/></w:tcPr><w:p><w:r><w:t>ANCHOR</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:tcPr><w:tcW w:w="2000" w:type="dxa"/><w:vMerge w:val="continue"/></w:tcPr><w:p><w:r><w:t>HIDDEN</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:sectPr/>
```

The other two held tests (`cellmerge_no_vmerge_no_vmergeorig_is_ignored`, `orphan_hmerge_continue_is_standalone_visible_cell`) do not need confirmation against real Word in their own right — their substantive §17.13.5.3 / §17.18.57 invariant (no content lost) already holds. They are parked only because their asserted literals encode the same bare-concat expectation that the discarded test-bugs proved wrong, plus the inside-tbl `<w:sectPr/>` I-DOC-003 fixture flaw. They should be re-authored with single-space joins and a trailing `<w:sectPr/>`, then re-promoted.

Two further tests assert that an accepted vMerge/cellMerge *continuation* cell's text is suppressed (merged away), leaving only the anchor. stemma deliberately surfaces continuation-cell text in its flat read view (documented in `view.rs table_cell_views`: "Vertical merge continuations are surfaced too … the anchor cell holds the content"), so this is a model decision, not a testable defect until real Word settles it:
- `vmerge_omitted_val_is_continue_not_dropped`
- `cellmerge_cont_revision_accepts_to_merged_anchor_only_text`

Run both bodies through real Word's accept-all to determine whether the merged continuation cell's text should vanish from the reading. If Word drops it, stemma's flat reader is a model gap; if not, delete the two tests. bodyXml is inline in each `#[ignore]`d test.

For the negative-`gridBefore`/`gridAfter` gap (finding 2 above): confirm real Word opens the repro without repair and renders both cells, then land the i64-clamp import fix and remove the `#[ignore]`.
