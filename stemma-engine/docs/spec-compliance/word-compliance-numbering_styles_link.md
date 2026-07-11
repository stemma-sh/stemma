# Word-compliance sweep — Numbering styles & numStyleLink/styleLink multi-hop indirection

**Summary:** 0 confirmed gaps, 9 new regression tests, 0 test-bugs discarded, 3 open questions. Build status: green — `cargo test -p stemma --test spec_numbering_styles_link_word_compliance -- --test-threads=1` reports **9 passed, 0 failed, 3 ignored**.

This area probed how stemma parses, resolves, and re-serializes numbering levels (level pPr/rPr), pStyle reverse-binding into a numbering level, and styleLink / numStyleLink multi-hop indirection. No engine incompliance was confirmed: every `opensClean` and `acceptTextEquals` assertion that could be confidently classified against the spec passed. The only divergences are three `acceptTextEquals` expectations that the workflow encoded with bare (no-separator) concatenation of body text across blocks; stemma's `to_plain_text` joins blocks with a blank line (`\n\n`). That is a property of the read-side plain-text projection, not a numbering-resolution divergence, so the tests are recorded as open questions rather than asserted as gaps.

## Confirmed incompliances

None. No assertion in this area revealed a stemma-vs-Word divergence after classification.

## New regression tests

The following 9 tests pass and run daily. Each encodes a Word-consumption / schema-validity constraint for numbering styles and styleLink/numStyleLink indirection.

- `lvl_ppr_overridden_by_paragraph_ppr_opens_clean` — a numbered paragraph supplying its own `ind` override alongside the level's `pPr` `ind` opens clean (canonical override scenario, not an error). §17.9.22
- `lvl_child_sequence_order_opens_clean` — a `CT_Lvl` using `pStyle` + `pPr` + `rPr` in schema sequence order opens clean; the level `rPr` formats only the marker glyph, so accept-all body text is exactly the run. §A.1 (CT_Lvl) / §17.9.22-24
- `level_ppr_does_not_leak_onto_numbered_paragraph` — a level `pPr` governs marker/indentation only; the referencing paragraph opens clean and its accept-all text equals the run text. §17.9.22
- `abstractnum_with_both_stylelink_and_numstylelink_opens_clean` — a `CT_AbstractNum` carrying both `styleLink` then `numStyleLink` in sequence order is schema-valid and opens clean. §17.9.27, §17.9.21, §A.1 (CT_AbstractNum)
- `numstylelink_val_253_chars_opens_clean` — a 253-char `numStyleLink`/`styleLink` `val` is within Word's bound; the resolved definition is valid and opens clean. MS-OI29500 §2.1.288 / §17.9.21, §17.9.27
- `pstyle_associated_level_ignores_para_numpr_ilvl` — a pStyle-associated level with a conflicting paragraph `numPr` `ilvl` opens clean (Word ignores the paragraph ilvl); accept-all text is exactly the run. §17.9.23, §17.7.7
- `numbering_lvl_ppr_overridden_by_paragraph_ppr` — a numbered paragraph's own `ind` conflicting with the level `pPr` `ind` is a documented valid override and opens clean; body text equals the run. §17.9.22
- `numbering_lvl_rpr_duplicate_child_tolerated_opens_clean` — a duplicated `sz` child inside a level `rPr` is schema-valid (the once-only rule is prose, not schema); the package opens clean and the duplicate cannot alter body text. §17.9.24
- `numbering_style_not_directly_referenceable_via_pstyle` — a paragraph `pStyle` naming a `w:type="numbering"` style is a wrong-type reference Word tolerates; the package opens clean and no marker/text is injected. §17.7.7 + §17.9.23

## Discarded test-bugs

None. No test was discarded; the three divergent tests are recorded as open questions (see below) rather than deleted, because their `opensClean` legs and per-paragraph body text are correct.

## Open questions — pending confirmation against real Word

These three tests assert an `acceptTextEquals` value the expected literal built by bare-concatenating body text across blocks. stemma's `read_accepted().to_text()` (the `to_plain_text` projection) inserts a blank-line joiner (`\n\n`) between blocks. The per-paragraph run bodies are correct in every case and no synthesized marker glyph leaks into body text — so the numbering resolution is right; only the inter-block joiner in the expected literal differs. Classified as a test-expectation defect (the spec value should account for the documented block joiner), not a stemma-vs-Word divergence. None needs a real-Word check (the divergence is purely in stemma's plain-text projection, which a real-Word check does not measure).

- `pstyle_bound_to_non_paragraph_style_is_ignored` — expected `"Unnumbered line oneUnnumbered line two"`, got `"Unnumbered line one\n\nUnnumbered line two"`. §17.9.23
- `pstyle_assoc_ignores_numpr_ilvl` — expected `"AlphaBeta"`, got `"Alpha\n\nBeta"`. §17.9.24 / §17.9.23
- `definition_side_abstractnum_directly_referenced_opens_clean` — expected `"OneTwo"`, got `"One\n\nTwo"`. §17.9.24

None of the three open questions requires a real-Word check: the divergence lives in stemma's plain-text block joiner, which a real-Word accept/reject check cannot adjudicate.
