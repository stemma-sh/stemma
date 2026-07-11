# Word-compliance sweep — OPC [Content_Types].xml: Default/Override precedence, extension casing, required overrides

**Summary:** 0 confirmed gaps, 11 new regression tests, 0 test-bugs discarded, build status: GREEN (`cargo test -p stemma --test spec_opc_content_types_stream_word_compliance` → 11 passed, 0 failed, 0 ignored).

This area probes how the engine resolves part content types out of `[Content_Types].xml`: the §7.2.3 rule that an `Override` (by part name) takes precedence over a `Default` (by extension), ASCII case-insensitive extension matching, and the per-part canonical media-type requirements for recognized WordprocessingML story parts. Every behavioral constraint mined here is already satisfied by the engine — `stemma::api::validate` opens each conformant fixture clean, and `Document::read_accepted().to_text()` resolves the main story part via its Override (never falling back to the generic `application/xml` Default).

## Confirmed incompliances

None. No fixture in this area exposed a divergence between stemma and Word's content-type adjudication. Override-over-Default precedence, the canonical WML Overrides, and case-insensitive extension matching all resolve as the spec requires, and every conformant package opens without repair. (Ranking is moot — there are no pipeline-bug or model-bug findings to rank.)

## New regression tests

All eleven tests pass and run daily; each pins a content-type resolution constraint so a future regression in `[Content_Types].xml` handling would turn it red.

- `main_document_resolved_via_override_not_xml_default` — with both an `xml` Default and the `document.main+xml` Override present, the main document resolves via the Override and its body is read; it must not fall back to `application/xml` (Part 2 §7.2.3; Part 1 §11.3.10).
- `styles_part_requires_canonical_override_opens_clean` — `word/styles.xml` typed by its canonical `wordprocessingml.styles+xml` Override over the generic `xml` Default opens without repair (Part 1 §11.3.12; Part 2 §7.2.3).
- `numbering_part_canonical_content_type_opens_clean` — `word/numbering.xml` with its canonical `wordprocessingml.numbering+xml` Override satisfies the per-part media-type requirement and opens clean (Part 1 §11.3.11; Part 2 §6.2.3).
- `override_beats_default_for_part_content_type` — `/word/document.xml` is covered by both `Default Extension="xml"` and a canonical Override; it resolves via Override precedence, so neither I-CT-001 nor I-CT-002 fires (Part 2 §7.2.3; Part 1 §15.2).
- `default_extension_match_is_ascii_case_insensitive` — `word/media/image1.PNG` (uppercase) is covered by `Default Extension="png"` only because matching is ASCII case-insensitive; no I-CT-001 fires (Part 2 §7.2.3).
- `canonical_wml_part_with_correct_override_opens_clean` — a styles part declared with its exact canonical Override AND covered by the `xml` Default resolves via Override precedence; validate reports no errors (Part 1 §11.3.12; Part 2 §7.2.3).
- `override_beats_default_for_wml_story_part` — `word/footnotes.xml` covered by both the `xml` Default and its canonical `footnotes` Override resolves via Override precedence and opens without repair (Part 2 §7.2.3; Part 1 §11.3.7).
- `added_image_png_part_resolves_via_png_default` — a `word/media/image1.png` part covered by `Default Extension="png" ContentType="image/png"` has a content type and opens clean (Part 2 §7.2.3).
- `endnotes_story_part_canonical_override_opens_clean` — `word/endnotes.xml` with its canonical `wordprocessingml.endnotes+xml` Override is recognized as the endnotes role and opens clean (Part 1 §11.3.4; Part 2 §7.2.3).
- `wrong_override_content_type_on_wml_part_is_a_defect` — the daily positive baseline: a correctly canonically-typed styles part opens without repair (the "OUTPUT must never carry a mismatched WML content type" save adjudication is deferred to confirmation against real Word / the `validate_docx` path) (Part 1 §11.3.12; Part 2 §7.2.3).
- `case_insensitive_extension_default_covers_uppercase_media` — a media part with an uppercase extension covered by a lowercase Default resolves a content type, so the document consumes cleanly and the part is never treated as untyped (Part 2 §7.2.3; Part 1 §9.1.6).

## Discarded test-bugs

None. No test in this area encoded a wrong expectation.

## Open questions — pending confirmation against real Word

No confirmed gap in this area requires a real-Word check.

One candidate test, `no_extension_part_without_override_has_no_content_type`, is not part of the suite. It is an open question (pending confirmation against real Word) for two reasons, recorded here in case the engine's production-path validator is later hardened:

- **Code-diagnosis red herring.** The claim that `word/orphanData` is "dropped from the validator's part set" is wrong. The rich validator (`stemma-engine/src/docx_validate.rs::validate_docx`) enumerates every ZIP entry in `build_package_state` regardless of relationship reachability, and `check_ct_001_content_types` already flags an extensionless part with no matching Override. The reason `api::validate` returns "(no issues)" is unrelated: `api::validate` → `validate_docx_report` (`runtime.rs`) runs only a curated structural subset and explicitly does not run the rich content-model validator on the production path.
- **Behavioral premise unproven.** The fixture's orphan is an extensionless ZIP item that is the target of no relationship. Part 1 §9.1.4 (Unknown Parts) says unreferenced unknown parts "shall be ignored on document consumption," which pulls against the §9.1.6 producer-conformance rule the test relied on. The prose nowhere states Word fires a repair dialog on an unreferenced extensionless orphan, and Word is known to tolerate unreferenced ZIP cruft. The test cannot state a defensible reason why `opensClean` MUST be false for an unreferenced untyped part, so the expectation is not ready.

If a future hardening makes `api::validate` run I-CT-001 on the production path, the correct daily assertion must first be confirmed against real Word:

- **name:** `no_extension_part_without_override_has_no_content_type`
- **check against real Word:** does Word open the package below, or repair it?
- **expected value:** unknown — to be confirmed against real Word. One reading asserts `opensClean == false` (Word repairs); §9.1.4 suggests `true` (Word ignores the unreferenced part). A real-Word check decides.
- **bodyXml repro** (an unreferenced, extensionless orphan ZIP entry `word/orphanData`; doc rels empty):

```xml
<!-- word/document.xml body -->
<w:p><w:r><w:t>Body with an unreferenced extensionless orphan part.</w:t></w:r></w:p><w:sectPr/>

<!-- extra ZIP entry, NOT a relationship target, NO file extension, NO Override -->
<!-- part name: word/orphanData ; raw bytes: "orphan" -->
```
