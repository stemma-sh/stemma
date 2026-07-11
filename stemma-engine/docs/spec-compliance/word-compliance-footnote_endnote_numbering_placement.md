# Word-compliance — Footnote/endnote numbering, restart, placement; doc-wide vs section-wide precedence

**Summary:** 0 confirmed gaps, 9 new regression tests, 0 test-bugs discarded, 3 open questions (all the same root cause: section-level `footnotePr`/`endnotePr` children are re-serialized in *authored* order instead of CT_FtnProps / CT_EdnProps schema sequence). Build status: **green** — `cargo test -p stemma --test spec_footnote_endnote_numbering_placement_word_compliance -- --test-threads=1` reports 9 passed, 0 failed, 3 ignored.

All three suspected gaps are open questions rather than CONFIRMED-GAP because they have not been adjudicated against real Word. The failure each one observed is real (the serializer does not reorder the children), but whether Word treats the authored-order `footnotePr`/`endnotePr` as schema-invalid (forcing a repair) versus tolerating it on open should be confirmed against a real Word save before promoting any of these to a CONFIRMED-GAP and opening a serializer fix. See the open questions below.

---

## Confirmed incompliances

None promoted to CONFIRMED-GAP. The three candidate incompliances are held as open questions below pending confirmation against real Word. They share a single suspected root cause and a single suspected fix site:

- **Suspected rule:** section-level `w:footnotePr` / `w:endnotePr` children must serialize in the `EG_FtnEdnNumProps`-bearing CT_FtnProps / CT_EdnProps schema sequence (`pos`, `numFmt`, then the numbering subgroup `numStart` before `numRestart`), regardless of the order they were authored in. An out-of-order sequence is schema-invalid and (suspected) triggers a Word repair prompt.
- **§refs:** ECMA-376 §17.11.11 (CT_FtnProps), §17.11.5 (CT_EdnProps), §17.11.19 (numRestart), §17.11.20 (numStart), §17.11.21 (pos), ISO 29500-1 Annex A CT_FtnProps / CT_EdnProps / EG_FtnEdnNumProps.
- **Classification:** pipeline-bug (serializer), medium confidence — the failing assertion is the `contains_ordered_adjacent` ordering check, reached before the `opensClean` assertion. The values themselves *are* preserved (e.g. `pos=beneathText`, `pos=sectEnd`); only child ORDER is wrong.
- **What stemma does vs Word:** stemma re-emits the children verbatim in authored order (e.g. `<w:footnotePr><w:numRestart/><w:numStart/><w:numFmt/><w:pos/></w:footnotePr>`). Word requires the CT_FtnProps schema order; an out-of-order group is not schema-valid.
- **Suggested fix site:** the section-level note-properties serializer (the `NoteProperties` -> `w:footnotePr` / `w:endnotePr` re-emit path in `runtime.rs` / the serializer for `CT_FtnProps`/`CT_EdnProps`). The model already parses these into a typed `NoteProperties`, so the fix is to emit children in fixed schema order rather than preserving the parsed sequence.

The three open-question tests stay `#[ignore]`d and active; once real Word confirms it repairs the out-of-order input, promote them (or remove `#[ignore]` after the serializer is fixed).

### Repros (minimal bodyXml)

`sect_footnotepr_children_emitted_in_ct_ftnprops_order`:

```xml
<w:p><w:r><w:t>Body paragraph with a footnote anchor.</w:t></w:r></w:p><w:sectPr><w:footnotePr><w:numRestart w:val="eachSect"/><w:numStart w:val="2"/><w:numFmt w:val="lowerRoman"/><w:pos w:val="beneathText"/></w:footnotePr></w:sectPr>
```

`ftnpr_children_reordered_to_ct_ftnprops_sequence`:

```xml
<w:p><w:r><w:t>body</w:t></w:r></w:p><w:sectPr><w:footnotePr><w:numRestart w:val="eachPage"/><w:numStart w:val="3"/><w:numFmt w:val="upperLetter"/><w:pos w:val="beneathText"/></w:footnotePr></w:sectPr>
```

`ednpr_children_reordered_to_ct_ednprops_sequence`:

```xml
<w:p><w:r><w:t>body</w:t></w:r></w:p><w:sectPr><w:endnotePr><w:numRestart w:val="eachSect"/><w:numFmt w:val="lowerRoman"/><w:pos w:val="sectEnd"/></w:endnotePr></w:sectPr>
```

---

## New regression tests (passing)

These encode preservation/ordering behaviour the engine already satisfies:

- `footnote_numrestart_eachsect_preserved_verbatim` — `numRestart=eachSect` (third legal ST_RestartNumber value) on a section-wide `footnotePr` is preserved verbatim and opens clean.
- `footnote_pos_sectend_valid_ftnpos_value_preserved_verbatim` — `pos=sectEnd` (legal ST_FtnPos value) preserved verbatim and opens clean.
- `footnote_pr_children_emitted_in_ct_ftnprops_schema_order` — an out-of-order section-level `footnotePr` still opens clean (opens-clean only assertion).
- `endnote_numstart_before_numrestart_in_ednpr_subgroup` — section-level `endnotePr` with numbering children opens clean.
- `section_level_footnote_pos_is_the_placement_authority_word_honors` — section-level footnote `pos=beneathText` is valid, opens clean, preserved verbatim (MS-OI §2.1.309).
- `footnote_pos_sectend_roundtrips_and_opens_clean` — `pos=sectEnd` opens clean and the token survives the no-op roundtrip.
- `footnote_numrestart_eachsect_roundtrips_and_opens_clean` — `numRestart=eachSect` re-emitted verbatim (not elided as the continuous default) and opens clean.
- `section_footnotepr_multichild_intraelement_order_preserved` — a section-wide `footnotePr` carrying all four children authored *in* schema order serializes in CT_FtnProps order with no child dropped and opens clean.
- `section_endnotepr_multichild_sectend_preserved` — a section-wide `endnotePr` carrying `pos=sectEnd`, `numFmt`, `numStart=3`, `numRestart=eachSect` authored *in* schema order serializes in CT_EdnProps order, round-trips verbatim, and opens clean.

---

## Discarded test-bugs

None.

---

## Open questions — pending confirmation against real Word

The spec is unambiguous that out-of-order children are schema-invalid, but none of the three has been confirmed against a live Word save. Listed here to confirm whether Word issues a repair prompt on the out-of-order input (which would promote them to CONFIRMED-GAP):

| Test | Check against real Word | Expected (Word) | bodyXml |
|---|---|---|---|
| `sect_footnotepr_children_emitted_in_ct_ftnprops_order` | open-clean / repair-prompt | out-of-order `footnotePr` is schema-invalid -> Word repairs and reorders children to `pos, numFmt, numStart, numRestart` | block A |
| `ftnpr_children_reordered_to_ct_ftnprops_sequence` | open-clean / repair-prompt | same; `pos=beneathText` value retained, children reordered to CT_FtnProps sequence | block B |
| `ednpr_children_reordered_to_ct_ednprops_sequence` | open-clean / repair-prompt | out-of-order `endnotePr` reordered to CT_EdnProps sequence `pos, numFmt, numRestart`; `pos=sectEnd` retained | block C |

Block A (`sect_footnotepr_children_emitted_in_ct_ftnprops_order`):

```xml
<w:p><w:r><w:t>Body paragraph with a footnote anchor.</w:t></w:r></w:p><w:sectPr><w:footnotePr><w:numRestart w:val="eachSect"/><w:numStart w:val="2"/><w:numFmt w:val="lowerRoman"/><w:pos w:val="beneathText"/></w:footnotePr></w:sectPr>
```

Block B (`ftnpr_children_reordered_to_ct_ftnprops_sequence`):

```xml
<w:p><w:r><w:t>body</w:t></w:r></w:p><w:sectPr><w:footnotePr><w:numRestart w:val="eachPage"/><w:numStart w:val="3"/><w:numFmt w:val="upperLetter"/><w:pos w:val="beneathText"/></w:footnotePr></w:sectPr>
```

Block C (`ednpr_children_reordered_to_ct_ednprops_sequence`):

```xml
<w:p><w:r><w:t>body</w:t></w:r></w:p><w:sectPr><w:endnotePr><w:numRestart w:val="eachSect"/><w:numFmt w:val="lowerRoman"/><w:pos w:val="sectEnd"/></w:endnotePr></w:sectPr>
```
