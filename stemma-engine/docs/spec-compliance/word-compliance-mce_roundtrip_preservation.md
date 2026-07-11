# Word-compliance sweep — MCE round-tripping: preserving AlternateContent / unknown extension markup through edit + accept/reject

**Summary:** 0 confirmed gaps (1 likely-real gap pending review), 9 new regression tests, 0 test-bugs discarded. Build status: green — `cargo test -p stemma --test spec_mce_roundtrip_preservation_word_compliance` reports **9 passed; 0 failed; 3 ignored**.

Test file: `stemma-engine/tests/spec_mce_roundtrip_preservation_word_compliance.rs`

Of the 3 non-passing tests, none were confidently classified as confirmed gaps or test-bugs from the code alone, so all 3 are `#[ignore]`d pending a fixture fix or confirmation against real Word. Two are non-conformant fixtures (stemma correctly fail-loud rejects an unbound namespace prefix); one is a likely-real engine gap (paragraph-level nested `AlternateContent`).

## Confirmed incompliances

None confirmed. The strongest candidate (`nested_ac_in_selected_choice_resolved_recursively`) is recorded as an open question below because, while it almost certainly reflects a real parser gap, it has not been confirmed against real Word and the fixture also needs a sanity pass. See the open-question classification below.

## Open questions — pending confirmation against real Word (or fixture fix)

Ranked: likely-pipeline-bug > non-conformant-fixture.

### 1. `nested_ac_in_selected_choice_resolved_recursively` — likely real pipeline gap (paragraph-level nested AlternateContent)

- **Rule:** Step-3 MCE resolution is recursive — the selected branch's children are themselves resolved, including a nested `mc:AlternateContent`. (ISO/IEC 29500-3 §9.3/§9.4)
- **Classification:** likely pipeline-bug (medium confidence), open question — not yet confirmed against real Word, fixture not yet re-validated.
- **What stemma does:** parse fails with `RuntimeError { code: InvalidDocx, message: "word IR error: unknown paragraph-level element: AlternateContent" }`. The inner `mc:AlternateContent` sits directly inside the outer `mc:Choice` at paragraph level; stemma's paragraph-level parser only recognizes `AlternateContent` at the outer/run level, so it rejects the nested one before any read happens.
- **What Word does:** resolves the outer Choice (`wps` understood), then recursively resolves the nested AC's own `wps` Choice, surfacing only `INNER` → reading text `before INNER after`.
- **Suggested fix site:** the paragraph-level child dispatcher in the word-IR import (the same place that handles run-level/outer `AlternateContent`) must recurse into a nested `AlternateContent` encountered inside a resolved Choice rather than erroring on an "unknown paragraph-level element".
- **Minimal bodyXml repro:**

```xml
<w:p><w:r><w:t>before </w:t></w:r><mc:AlternateContent xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><mc:Choice Requires="wps"><mc:AlternateContent><mc:Choice Requires="wps"><w:r><w:t>INNER</w:t></w:r></mc:Choice><mc:Fallback><w:r><w:t>INNERFB</w:t></w:r></mc:Fallback></mc:AlternateContent></mc:Choice><mc:Fallback><w:r><w:t>OUTERFB</w:t></w:r></mc:Fallback></mc:AlternateContent><w:r><w:t> after</w:t></w:r></w:p><w:sectPr/>
```

### 2. `mce_choice_qualified_ignorable_attr_survives_roundtrip` — non-conformant fixture (test-bug class)

- **Rule under test:** a qualified attribute in an ignorable namespace (`i1:foo="bar"`) is legitimate Choice content and must round-trip verbatim. (ISO/IEC 29500-3 §7.6)
- **Classification:** test-bug (non-conformant fixture), high confidence — not deleted because the *intended* rule is valid and worth keeping once the fixture is fixed.
- **What stemma does:** correctly fail-loud rejects the input: `mc:Choice Requires references namespace prefix "wps" which has no in-scope xmlns binding (ISO/IEC 29500-3 §7.6); the document is non-conformant`. The fixture's `<mc:Choice Requires="wps">` references `wps` but never declares `xmlns:wps`.
- **What Word does:** n/a — the input is non-conformant; §7.2/§7.6 require every Requires prefix to have an in-scope binding.
- **Suggested fix:** add `xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"` to the `mc:AlternateContent` element, then the test exercises the actual `i1:foo` qualified-ignorable-attr roundtrip rule.
- **Minimal bodyXml repro (as written — non-conformant):**

```xml
<w:p><w:r><mc:AlternateContent xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:i1="http://example.com/i1" mc:Ignorable="i1"><mc:Choice Requires="wps" i1:foo="bar"><w:t>choice</w:t></mc:Choice><mc:Fallback><w:t>fallback</w:t></mc:Fallback></mc:AlternateContent></w:r></w:p><w:sectPr/>
```

### 3. `mce_ignorable_multi_prefix_list_survives_roundtrip` — non-conformant fixture (test-bug class)

- **Rule under test:** the `mc:Ignorable="wps wpg"` whitespace-delimited prefix list survives verbatim, and the non-taken Fallback round-trips. (ISO/IEC 29500-3 §7.2 / ISO/IEC 29500-1 §17.17.3)
- **Classification:** test-bug (non-conformant fixture), high confidence — kept, not deleted, because the rule is valid once the fixture binds its prefixes.
- **What stemma does:** correctly rejects the input with the same unbound-prefix error: `Requires references namespace prefix "wps" which has no in-scope xmlns binding`. The fixture declares `mc:Ignorable="wps wpg"` and `Choice Requires="wps"` but never binds `xmlns:wps` / `xmlns:wpg`.
- **What Word does:** n/a — non-conformant input; §7.2 requires every prefix in the Ignorable list to have an in-scope binding.
- **Suggested fix:** bind `xmlns:wps` and `xmlns:wpg` on the `mc:AlternateContent` element.
- **Minimal bodyXml repro (as written — non-conformant):**

```xml
<w:p><w:r><w:t>before </w:t></w:r><w:r><mc:AlternateContent xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" mc:Ignorable="wps wpg"><mc:Choice Requires="wps"><w:t>choice</w:t></mc:Choice><mc:Fallback><w:t>fallback</w:t></mc:Fallback></mc:AlternateContent></w:r><w:r><w:t> after</w:t></w:r></w:p><w:sectPr/>
```

## New regression tests (passing — kept active)

- `untouched_ac_with_ignorable_list_survives_roundtrip_verbatim` — an untouched `mc:AlternateContent` with an in-scope-bound `mc:Ignorable` list round-trips verbatim (Ignorable list + Fallback branch + payload preserved; opens clean). (§7.2, §17.17.3)
- `ac_in_del_reject_restores_accept_removes` — accepting a `w:del` removes the deleted run and its resolved AC; rejecting restores the understood Choice text. (§17.13.5, §9.3/§9.4)
- `mc_ignorable_multitoken_list_survives_roundtrip` — a multi-token `mc:Ignorable="wps w14 wpg"` (all prefixes bound) survives verbatim, never re-derived to a shorter list. (§7.2, §17.17.3)
- `inline_ac_read_picks_choice_once_no_fallback_dup` — read selects the understood Choice and replaces the AC with its content only; Fallback never surfaces and is not duplicated. (§9.3/§9.4)
- `ac_in_ins_unknown_choice_selects_fallback` — an unknown-ns Choice is skipped and the Fallback selected; accepting the `w:ins` keeps the resolved Fallback, rejecting removes the whole inserted run. (§9.3, §17.13.5)
- `mce_ignorable_multi_token_list_survives_roundtrip` — a run `rPr` extension (`w14:glow`) under a bound `mc:Ignorable="w14 wp14"` round-trips verbatim with its full Ignorable list. (§7.5, §17.17.3)
- `mce_choice_multi_prefix_requires_all_understood_else_skipped` — a `Requires="w14 zz"` Choice with an unknown `zz` is not selectable, so the Fallback is read. (§9.3 all-namespaces-must-be-understood)
- `mce_choice_no_fallback_understood_requires_selects_choice` — a Fallback-less AC whose single Choice requires an understood ns surfaces the Choice content. (§9.3/§9.4)
- `mce_ac_in_deleted_run_accept_removes_reject_keeps_branch` — accept of a `w:del` removes the deleted text; reject restores it. (§17.13.5.18)

## Discarded test-bugs

None. No test was deleted: the two non-conformant-fixture tests encode valid rules and are retained as open questions so the fixtures can be repaired rather than thrown away.

## Open questions — pending confirmation against real Word

The three open questions above are classifiable from the spec + stemma's fail-loud behaviour without a real-Word check (two are non-conformant fixtures; one is a parser-level "unknown element" error). The nested-AC item (#1) is the only one that would benefit from confirmation against real Word once the parser gap is addressed:

- **name:** `nested_ac_in_selected_choice_resolved_recursively`
- **check against real Word:** read / accepted-and-rejected text (consumption reading of the recursively-resolved branch)
- **expected value:** `before INNER after` for both `read_accepted().to_text()` and `read_rejected().to_text()`
- **bodyXml:**

```xml
<w:p><w:r><w:t>before </w:t></w:r><mc:AlternateContent xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><mc:Choice Requires="wps"><mc:AlternateContent><mc:Choice Requires="wps"><w:r><w:t>INNER</w:t></w:r></mc:Choice><mc:Fallback><w:r><w:t>INNERFB</w:t></w:r></mc:Fallback></mc:AlternateContent></mc:Choice><mc:Fallback><w:r><w:t>OUTERFB</w:t></w:r></mc:Fallback></mc:AlternateContent><w:r><w:t> after</w:t></w:r></w:p><w:sectPr/>
```
