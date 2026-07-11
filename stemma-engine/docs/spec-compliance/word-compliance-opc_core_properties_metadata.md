# Word-compliance sweep — OPC core properties part + WordprocessingML metadata (docProps) round-trip

**Summary:** 2 confirmed gaps, 8 new regression tests, 0 test-bugs discarded. Build green — `cargo test -p stemma --test spec_opc_core_properties_metadata_word_compliance` reports **8 passed; 0 failed; 3 ignored** (2 confirmed-gap tests + 1 open-question test, all skipped at the daily tier).

The area covers the OPC Core Properties part (`docProps/core.xml`), the custom-properties part (`docProps/custom.xml`), and the typed metadata edit path (`set_core_property` / `set_custom_property`) round-trip behaviour against how real Microsoft Word reads/writes the package.

---

## Confirmed incompliances

Ranked: pipeline-bug > model-bug, high > low confidence. Both confirmed gaps here are the **same root-cause pipeline bug** — stemma's lossy nine-field core-properties model — surfaced from two angles.

### 1. `unmodeled_core_fields_preserved_across_property_edit` (pipeline-bug, high confidence)

- **Rule:** Setting one core property (e.g. `dc:title`) must preserve the package's other already-present core-property elements that this verb does not author — `cp:contentStatus`, `cp:revision`, `cp:version`, `cp:lastPrinted`. A property edit must not silently drop spec-valid metadata.
- **§refs:** ECMA-376 §8.3.4 (Core property elements: contentStatus, revision, version, lastPrinted); ISO/IEC 29500-2 §8.3.4.
- **Classification:** pipeline-bug.
- **What stemma does vs. what Word does:** Word's File>Info edits a single field and leaves the rest intact. stemma's `set_core_property` rewrites `docProps/core.xml` from a lossy nine-field model and **drops every unmodeled element**. After `set_core_property("title","Edited")` the re-emitted core.xml is just `<cp:coreProperties …><dc:title>Edited</dc:title></cp:coreProperties>` — `contentStatus=Draft`, `revision=3`, `version=1.0`, `lastPrinted` are all gone. The assertion `core.contains("Draft")` (and the revision/version/lastPrinted siblings) fails.
- **Suggested fix site:** `stemma-engine/src/docprops.rs` — `CoreProperties::parse` (the `_ =>` discard arm) drops spec-valid core-property elements. `CoreProperties` needs to carry unmodeled elements through (e.g. a `Vec` of preserved `(qname, value)` "other" elements), and `CoreProperties::serialize` must re-emit them. The verb path `set_core_property` in `src/edit/verbs/metadata.rs` then becomes lossless because it rewrites from a model that no longer discards data.
- **Minimal bodyXml repro:**

```xml
<w:p><w:r><w:t>Body whose package core props carry revision and version.</w:t></w:r></w:p><w:sectPr/>
```

(paired with a `docProps/core.xml` carrying `cp:contentStatus`, `cp:revision`, `cp:version`, `cp:lastPrinted`)

### 2. `set_core_property_preserves_unmodeled_standard_fields` (pipeline-bug, high confidence)

- **Rule:** Setting one core property (e.g. `dc:title`) must preserve every other core property already present in `docProps/core.xml` — including standard fields stemma does not model (`cp:revision`, `dc:language`, `cp:contentStatus`, `dc:identifier`, `cp:version`, `cp:lastPrinted`, `dc:subject`) — because Word treats a property edit as a targeted field write, not a rewrite of the part from a nine-field subset.
- **§refs:** ISO/IEC 29500-2 §8.3.4; ECMA-376 §15.2.12.1.
- **Classification:** pipeline-bug.
- **What stemma does vs. what Word does:** Word's File>Info would still show Revision / Language / Identifier after a title edit. stemma's nine-field rewrite drops them — re-emitted core.xml after `set_core_property("title","Edited")` is just `<cp:coreProperties …><dc:title>Edited</dc:title></cp:coreProperties>`. The assertions `core.contains("<cp:revision>7</cp:revision>")`, `core.contains("en-US")`, `core.contains("DOC-42")` fail.
- **Suggested fix site:** `stemma-engine/src/docprops.rs` — same `CoreProperties::parse` `_ =>` drop arm + `serialize`. Add an opaque passthrough collection on the `CoreProperties` struct (e.g. `unmodeled: Vec<XMLNode>` capturing any child element whose local name is not one of the nine modeled fields) and re-emit those nodes in `serialize`, making `set_core_property` a targeted field write. The verb in `src/edit/verbs/metadata.rs` needs no change once the model is non-lossy.
- **Minimal bodyXml repro:**

```xml
<w:p><w:r><w:t>Body unaffected by a metadata edit.</w:t></w:r></w:p><w:sectPr/>
```

(paired with a `docProps/core.xml` carrying `cp:revision=7`, `dc:language=en-US`, `dc:identifier=DOC-42`)

---

## New regression tests

The passing tests below are now active and run daily. They encode Word-correct behaviour the engine already satisfies, so they guard against regressions.

- `no_core_props_part_not_invented_on_roundtrip` — a package with no Core Properties part round-trips without one being invented and still opens clean (no-fallback directive).
- `dcterms_date_requires_w3cdtf_xsi_type` — `dcterms:created`/`modified` carry `xsi:type="dcterms:W3CDTF"` with the dcterms prefix bound to `http://purl.org/dc/terms/`.
- `core_props_root_has_no_attributes` — the serialized `coreProperties` root declares only namespaces and carries no non-namespace attributes.
- `dc_core_elements_carry_no_xsi_type_or_lang` — `dc:` core-property elements are bare text with no `xsi:type` and no `xml:lang`.
- `custom_property_pid_starts_at_two` — the first custom property carries `pid="2"`; reserved pid 0/1 are never used.
- `dcterms_dates_carry_w3cdtf_xsi_type` — `dcterms:created` carries `xsi:type="dcterms:W3CDTF"` (sibling check pinned via the date edit path).
- `set_core_created_emits_w3cdtf_xsi_type` — `set_core_property("created")` emits `xsi:type="dcterms:W3CDTF"` with the dcterms prefix bound correctly.
- `custom_property_emits_required_fmtid_and_pid` — every `<property>` in custom.xml carries the required `fmtid` and `pid` attributes (CT_Property, use="required").

---

## Discarded test-bugs

None. No test in this file mis-stated the spec.

---

## Open questions — pending confirmation against real Word

Every confirmed gap is listed below for confirmation against real Word. (One open-question test is also pending confirmation against real Word.)

### `unmodeled_core_fields_preserved_across_property_edit`
- **Check against real Word:** does the document open clean?
- **Expected:** Given a core.xml carrying `<cp:contentStatus>Draft</cp:contentStatus>`, `<cp:revision>3</cp:revision>`, `<cp:version>1.0</cp:version>`, `<cp:lastPrinted>2026-01-01T00:00:00Z</cp:lastPrinted>`, after `set_core_property("title","X")` the re-serialized core.xml STILL contains `contentStatus=Draft`, `revision=3`, `version=1.0`, `lastPrinted` — Word preserves them on a single-field edit. stemma currently drops all four (lossy nine-field model), which is the divergence.
- **bodyXml:**

```xml
<w:p><w:r><w:t>Body whose package core props carry revision and version.</w:t></w:r></w:p><w:sectPr/>
```

### `set_core_property_preserves_unmodeled_standard_fields`
- **Check against real Word:** does the document open clean?
- **Expected:** After `set_core_property("title","Edited")` the package opens clean in Word AND File>Info still shows the original Revision number, Language, and Identifier — i.e. those elements survive the title write. If stemma's rewrite dropped them, Word's read view loses metadata it should have retained.
- **bodyXml:**

```xml
<w:p><w:r><w:t>Body unaffected by a metadata edit.</w:t></w:r></w:p><w:sectPr/>
```

### `custom_property_name_case_insensitive_unique` (open question)
- **Check against real Word:** does the document open clean, or does Word repair it?
- **Expected:** Word treats `Reviewer` and `reviewer` as the same property (case-insensitive uniqueness, MS-OI29500 §2.1.1724 / ISO 29500-1 §22.3.2.2). After `set_custom_property("Reviewer","a")` then `set_custom_property("reviewer","b")` exactly ONE `<property>` should remain (value `b`). stemma matches case-sensitively and emits two case-colliding `<property>` elements (`name="Reviewer" pid=2` + `name="reviewer" pid=3`) — a duplicate Word would reject/repair. Pending confirmation against real Word.
- **bodyXml:**

```xml
<w:p><w:r><w:t>Case-collision carrier.</w:t></w:r></w:p><w:sectPr/>
```
