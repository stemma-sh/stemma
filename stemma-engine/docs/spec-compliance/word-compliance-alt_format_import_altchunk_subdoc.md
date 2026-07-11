# Word-compliance — Alternative format import (altChunk) and subdocuments: anchors + referenced parts round-trip

**Summary:** 0 confirmed gaps, 12 new regression tests (green), 0 test-bugs.

The stemma engine handles unexpanded `altChunk` anchors correctly across every constraint mined for this area: the anchor element, its `r:id` binding, and its nested `altChunkPr`/`matchSrc` subtree all round-trip verbatim; the package validates clean; and an unexpanded / id-less / unprocessable anchor contributes no text to accept/reject reads. No incompliance was confirmed.

## Confirmed incompliances

None. Probed `w:altChunk` / `w:altChunkPr` anchors (ECMA-376 §17.17, [MS-OI29500] §2.1.527): document-order preservation of multiple body- and cell-level anchors, verbatim round-trip of `matchSrc` import directives, no synthesis of an absent `altChunkPr`, per-anchor `r:id` survival, opens-clean validation, and read-side consumption semantics (an unexpanded / id-less / unprocessable chunk contributes no text). The engine matched Word behavior on every constraint.

## New regression tests

All 12 tests are active (green) and pin correct Word behavior:

- `two_body_altchunk_anchors_preserve_document_order` — two body-level anchors and the interleaved paragraph re-serialize in authored document order (§17.17.1.1, CT_AltChunk maxOccurs=unbounded).
- `altchunk_matchsrc_false_roundtrips_verbatim` — `altChunkPr/matchSrc@val=false` stays nested inside the unexpanded anchor and is preserved verbatim, not normalized or hoisted (§17.17.2.2/§17.17.2.3).
- `multiple_altchunk_anchors_preserve_document_order` — multiple anchors are schema-valid and the package opens in Word without repair (EG_BlockLevelElts altChunk maxOccurs=unbounded).
- `altchunk_without_properties_gains_no_synthesized_altchunkpr` — a bare anchor is re-emitted with no fabricated `altChunkPr` (§A.1 minOccurs=0 + §17.17.2.2, no silent synthesis).
- `altchunk_in_table_cell_roundtrips_and_opens_clean` — a cell-nested anchor opens clean and the engine re-emits it (with its `r:id`) rather than flattening it (CT_Tc → EG_BlockLevelElts, §17.17.2.1).
- `altchunk_matchsrc_val_false_roundtrips_verbatim` — explicit `matchSrc@val=false` (or `0`) survives round-trip without being coerced or dropped (§17.17.2.3).
- `altchunkpr_without_matchsrc_child_roundtrips` — an `altChunkPr` with no `matchSrc` child is schema-valid and opens clean (matchSrc minOccurs=0).
- `altchunk_without_rid_preserved_opaquely` — an anchor with no `r:id` is preserved opaquely, opens clean, and contributes no accepted text (§17.17.2.1 "if id omitted, parent shall be ignored").
- `two_altchunk_anchors_each_preserve_own_rid` — two consecutive anchors each keep their distinct `r:id`; anchors are not merged or collapsed (§17.17.2.1).
- `altchunk_in_table_cell_validates_and_roundtrips` — a table-cell anchor passes the validator and round-trips (CT_Tc admits altChunk).
- `altchunk_unrecognized_content_type_contributes_no_text` — a non-importing/unprocessable anchor is ignored and contributes no text on either accept-all or reject-all reads (§17.17.2.1 + MS-OI29500 §2.1.527).
- `altchunk_without_rid_is_ignored_and_opens_clean` — an id-less anchor opens clean and accept-all reads only the real paragraph text (§17.17.2.1).

## Discarded test-bugs

None.

## Open questions — pending confirmation against real Word

None.
