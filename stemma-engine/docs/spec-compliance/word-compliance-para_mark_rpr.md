# Word-compliance sweep — Paragraph-mark run properties (pPr/rPr, pilcrow formatting, mark del/ins)

Area slug: `para-mark-rpr`
Test file: `stemma-engine/tests/spec_para_mark_rpr_word_compliance.rs`

ECMA / MS refs probed: §17.3.1.29 (rPr, paragraph-mark run properties), §17.3.1.30
(CT_ParaRPrOriginal, previous paragraph-mark snapshot for revisions), §17.13.5.15
(deleted paragraph mark), §17.7.3 (toggle XOR), §17.3.2.15 (highlight), §17.3.2.36
(specVanish); MS-OI29500 §2.1.57 / §2.1.58 / §2.1.97; MS-OE376 §2.1.62.

Summary: 0 confirmed gaps, 12 new regression tests (green), 0 test-bugs.

## Confirmed incompliances

None. The sweep probed how the paragraph-mark rPr behaves under Word consumption:
mark-glyph formatting (bold, highlight, specVanish, style-inherited and explicit
toggle XOR) never leaks into or alters the run text; the mark del merges with the
following paragraph on accept and restores the split on reject (final-mark delete
is a no-op merge); the rPrChange previous-mark snapshot's ins/del/moveFrom children
are ignored by Word (no paragraph merge); and the serializer never synthesizes an
absent mark rPr from inherited style formatting. stemma matched Word on every probe.

## New regression tests

- `list_item_mark_formatting_leaves_run_text` — §17.3.1.29 / MS-OI29500 §2.1.57: bold on a numbered paragraph's mark rPr leaves the accepted run text intact ("Item text").
- `deleted_mark_with_mark_bold_merges_keeping_both_run_texts` — §17.13.5.15: accepting a deleted mark (carrying mark bold) merges with the following paragraph keeping both run texts; reject keeps two separate paragraphs.
- `rprchange_snapshot_del_ignored_keeps_two_paragraphs` — §17.3.1.30 / MS-OI29500 §2.1.58: a del inside the rPrChange previous-mark snapshot is ignored; accept and reject both keep the two paragraphs separate.
- `rprchange_snapshot_ins_ignored_and_preserved_verbatim` — §17.3.1.30 / MS-OI29500 §2.1.58: an ins inside the previous-mark snapshot is ignored (not a live insertion); accept equals reject, paragraphs stay distinct.
- `specvanish_without_vanish_on_mark_keeps_run_text_visible` — §17.3.2.36 / MS-OI29500 §2.1.97: specVanish without vanish is a mark-glyph hide hint only; the run text stays visible on accept and reject.
- `highlight_on_paragraph_mark_does_not_leak_into_run_text` — §17.3.2.15 / §17.3.1.29: a highlight on the mark rPr colors only the mark glyph and never propagates into the run text.
- `mark_rprchange_empty_prior_snapshot_reject_clears_mark_formatting` — §17.3.1.30 / §17.13.5.30: an empty inner rPr snapshot is valid; accept keeps current mark formatting and reject restores the un-bolded mark, neither changing run text.
- `del_in_prior_mark_snapshot_is_ignored_no_paragraph_merge` — §17.3.1.30 / MS-OE376 §2.1.62: a del living only in the prior-mark snapshot is ignored; neither accept nor reject merges the two paragraphs.
- `para_mark_bold_xor_with_bold_paragraph_style_preserves_mark_b_and_text` — §17.7.3 / §17.3.1.29: toggle XOR between mark b and a bold paragraph style is appearance-only; the run text is preserved.
- `para_mark_inherits_style_bold_without_synthesizing_explicit_mark_rpr` — §17.3.1.29 / §17.7.1: an absent mark rPr means "inherit"; the serializer must not synthesize an explicit mark rPr for style-inherited bold, and the pStyle reference is preserved verbatim.
- `para_mark_explicit_bold_off_overrides_bold_style_and_is_preserved` — §17.7.3 / §17.3.1.29: an explicit mark `b w:val="0"` under a bold style is appearance-only toggle resolution; the run text content is unchanged.
- `delete_final_paragraph_mark_is_noop_merge` — §17.13.5.15: a deleted mark merges with the following paragraph, so the final mark (no following paragraph) is a no-op merge on accept and a restore on reject; both texts survive as separate paragraphs.

## Discarded test-bugs

None.

## Open questions — pending confirmation against real Word

None.
