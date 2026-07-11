# Word-conformance — master summary

Exhaustive Word-compliance audit of the stemma engine against the WordprocessingML
reference (ECMA-376 / ISO 29500 §17 + MS-OI29500 + shared markup + OPC). Every area
carries runnable `spec_*_word_compliance.rs` tests.

## Headline

- **Coverage:** 69 registry areas (plus `tables`, `sections`) = full WordprocessingML surface.
- **Tests:** 71 files, **871 test functions**, suite green (`cargo test -p stemma` → 1582 passed / 0 failed / 36 ignored).
- **Confirmed incompliances:** **6** (all pipeline-bugs, concrete fix sites) + **1 model-bug** (`webHidden`) surfaced by inspection. **All seven are closed:** #1–#5 fixed and verified (see table), `webHidden` modeled, plus orphan-instrText handling, bdo/dir/pgNum/contentPart parser breadth, and the OPC duplicate-entry sibling gap. The parked-for-review backlog is fully triaged: none of the original population remain (58 rewritten to correct suite contracts, 2 deleted with covering tests, the rest reclassified). A further 13 candidate gaps and 11 open questions remain pending confirmation against real Word, plus 4 additional open questions. Checking against real Word overturned two prose-based claims: negative `numId`/`abstractNumId` open clean in Word despite MS-OI §2.1.275/§2.1.287, and a para-mark `moveFrom` merges on accept — the engine was fixed accordingly.
- **Parked for review:** ~36 — the ordering subset was **adjudicated benign against real Word** (see below).
- **Discarded test-bugs:** ~25 (mostly the consumption-vs-save / wrong-path trap).

## Confirmed incompliances (ranked)

| # | Severity | Area | Rule | Fix site |
|---|---|---|---|---|
| 1 | **medium** | simple-field-vs-complex | **Fixed.** A field's displayed result must read as text. The human-readable surface (`view::to_plain_text`) now surfaces a `Field` anchor's cached result (complex-field structural markers contribute nothing, like Word); the block-identity surface (`import::extract_block_text` + story content hash) deliberately KEEPS one U+FFFC per opaque so block identity stays stable against volatile field results — two distinct, documented contracts. 10 field tests un-ignored. Related import fix landed with it: an orphan `w:instrText` outside any complex field is now imported as regular text per §17.16.23 | `src/view.rs` (`opaque_read_text`), `src/word_ir.rs` (`reclassify_orphan_instr_text`) |
| 2 | low | opc-core-properties | **Fixed** (verified). Setting one core property silently dropped other present, unmodeled core props (`contentStatus`, `revision`, `version`, `lastPrinted`). Now carried through via `UnmodeledCoreProp` in `CoreProperties::parse`/`serialize`; area tests green | `src/docprops.rs` |
| 3 | low | opc-zip-physical-package | **Fixed** (with the sibling duplicate-ZIP-entry gap). Override PartName matching must be ASCII case-insensitive (OPC §6.2/§7.2); stemma resolved `word/document.xml` case-sensitively and reported it missing when stored as `word/Document.xml` (Word opens clean). Fixed at the package primitives (`DocxArchive`/`DocxPackage`/`has_override`/validator I-PKG-002+I-CT-001); duplicate (case-equivalent) ZIP part names now rejected at the read edge (`DocxError::DuplicatePartName`, validator I-PKG-003). Both tests un-ignored + roundtrip guard added; confirmed against real Word (case-mismatched package and stemma's roundtrip of it open clean; duplicate-entry package is corrupt to Word); corpus parse zero new refusals | `src/docx.rs`, `src/docx_package.rs`, `src/docx_validate.rs` |
| 4 | low | runcontent-embedded-objects | **Fixed** (verified). Nested `m:oMath` / `m:oMath` outside a paragraph now flagged by `check_omath_placement`, wired into `validate_docx_report`; area tests green | `src/docx_validate_annotations.rs` |
| 5 | low | table-borders-conflict | **Fixed** (verified). `border_weight` now uses the explicit MS-OI29500 §2.1.169 border-number table (non-sequential, skips 4-7 and 23); area tests green | `src/import.rs` `border_weight` |

(#2 produced two test entries — `contentStatus/revision/...` and `dc:title`-preserves — same root cause.)

### Model-bug (no silent fallbacks)
- **Fixed.** `w:webHidden` (§17.3.2.44) was unmodeled — a run carrying it that was rebuilt by an edit silently dropped it. Now a `MarkValue` field mirrored on `vanish` through the whole chain (word_ir parse, style resolve/overlay/linked-char, domain `StyleProps`, import bridge, `build_rpr` emit at Annex A position 18), guarded by an authoring-path rebuild test verified to fail without the emit.

## Adjudication against real Word

| Case | Word verdict | Conclusion |
|---|---|---|
| (control) well-formed doc | clean | harness sane |
| nested `m:oMath` | **repaired** | gap #4 confirmed |
| `m:oMath` outside paragraph | **cannot open** | gap #4 confirmed |
| `word/Document.xml` + `/word/document.xml` override | **clean** | gap #3 confirmed (stemma wrongly rejects) |
| text-first `rPr` (`rPr` after `t`) | clean | **benign** — not a gap |
| mis-ordered `rPr` (`sz` before `u`) | clean | **benign** |
| reversed `pPr` strict-sequence children | clean | **benign** |

**Key finding:** Word tolerates element-order variance and text-first `rPr` on open. So stemma's
verbatim preservation of untouched markup is correct, and the entire **ordering parked-for-review
population is benign** (no normalization-on-save gap). The `#[ignore]`d ordering
tests can be downgraded/closed accordingly.

## Methodology findings (baked into the workflow)

1. **Consumption vs. save.** An `xml*` assertion on `reserialize()` of an *unedited* doc tests
   stemma's verbatim-preservation contract, NOT Word normalization. "Word ignores X on render"
   ≠ "Word strips X on save." → grounding gate: each rule carries a verbatim spec quote + a
   surface tag (`serializer-emits` / `consumption` / `validity` / `save-normalization`), grep-verified.
2. **Serializer-ordering needs the edit path.** Even a `serializer-emits` assertion is invalid on
   the no-edit roundtrip, because `build_paragraph_properties`/`build_rpr` only run on authored/
   edited content (untouched body is re-zipped byte-for-byte). To test ordering, apply an edit
   that rebuilds the block, THEN assert.

## Per-area reports
One `word-compliance-<area>.md` per area in this directory.
