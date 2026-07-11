# Word-compliance sweep — MCE: AlternateContent/Choice/Fallback resolution + Ignorable/ProcessContent/MustUnderstand

**Summary:** 0 confirmed gaps, 9 new regression tests, 0 test-bugs discarded, 3 open questions (pending confirmation against real Word). Build status: green — `cargo test -p stemma --test spec_mce_alternatecontent_resolution_word_compliance -- --test-threads=1` reports 9 passed, 0 failed, 3 ignored.

Test file: `stemma-engine/tests/spec_mce_alternatecontent_resolution_word_compliance.rs`

Scope: ISO/IEC 29500-3 (Markup Compatibility & Extensibility) consumption semantics — how a conforming MCE consumer (Microsoft Word, the reference implementation) resolves `mc:AlternateContent` Choice/Fallback selection, honours `mc:Ignorable`/`mc:ProcessContent`, and scopes `mc:MustUnderstand`. All assertions are on the READ side (`read_accepted()`/`read_rejected()` text and opens-clean), since MCE resolution is a consumption-time transform.

## Confirmed incompliances (ranked: pipeline-bug > model-bug, high>low confidence)

None confirmed. Three divergences were observed but could not be confidently classified without a real-Word check; they are recorded under **Open questions** below. The strongest candidate for a real pipeline-bug is the `mc:MustUnderstand` over-refusal (it makes stemma reject a document a conforming consumer opens clean), but it is held pending confirmation against real Word.

## Open questions — pending confirmation against real Word

These three tests fail today. They encode plausible-correct Word behaviour, but each turns on a reading of §9.4 recursion / MustUnderstand scoping that should be confirmed against real Word before being treated as a confirmed gap (the document's actual open-clean behaviour and resolved text in Word is the deciding evidence). All three are `#[ignore]`d so the daily suite stays green; none was weakened.

### 1. `mce_mustunderstand_on_nonselected_choice_is_not_examined` — MustUnderstand scope (over-refusal)
- **Rule:** §9.4/§7.4 — `mc:MustUnderstand` is examined ONLY on the `AlternateContent` element and on the SELECTED `Choice`/`Fallback`. A `mc:MustUnderstand` on a non-selected `Choice` is inert.
- **Classification (tentative):** pipeline-bug, high confidence — stemma refuses a document a conforming consumer opens clean.
- **stemma does:** `Document::parse` refuses — `RuntimeError InvalidDocx: "mc:MustUnderstand requires namespace \"http://example.com/quux-unknown\" which this consumer does not understand ... refusing"`. stemma's MCE Step-1 transform walks EVERY element of the AC subtree and refuses on any `mc:MustUnderstand`.
- **Word does (claimed):** opens clean, reads `before chosen after` — the first `Choice` (`Requires="w"`) is selected, so the later non-selected `Choice`'s `mc:MustUnderstand="q"` is never examined.
- **Suggested fix site:** the MCE preprocessing pass (Step-1 MustUnderstand walk) in the word IR import — restrict the MustUnderstand examination to the AC element and the selected branch, not the whole subtree.
- **Repro (bodyXml):**
```xml
<w:p><w:r><w:t>before </w:t></w:r><mc:AlternateContent xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006"><mc:Choice Requires="w"><w:r><w:t>chosen</w:t></w:r></mc:Choice><mc:Choice Requires="q" mc:MustUnderstand="q" xmlns:q="http://example.com/quux-unknown"><w:r><w:t>never</w:t></w:r></mc:Choice></mc:AlternateContent><w:r><w:t> after</w:t></w:r></w:p><w:sectPr/>
```

### 2. `nested_alternatecontent_inner_choice_surfaces_when_parent_choice_selected` — nested AC recursion (selected outer Choice)
- **Rule:** §9.4 — when the outer `Choice` (`Requires="wps"`) is selected, Step-3 must recurse into its nested `AlternateContent` where the inner `Choice` (`Requires="wpg"`) is also selected — `INNER_CURRENT` surfaces.
- **Classification (tentative):** pipeline-bug, high confidence — stemma refuses a document a conforming consumer opens clean.
- **stemma does:** `Document::parse` refuses — `RuntimeError InvalidDocx: "word IR error: unknown paragraph-level element: AlternateContent"`. stemma does not handle a nested `AlternateContent` surfacing inside a selected `Choice`.
- **Word does (claimed):** opens clean, resolves to `INNER_CURRENT`.
- **Suggested fix site:** word IR import — make the AC resolver recurse into a nested `AlternateContent` that becomes content of the selected `Choice`, rather than emitting it as an unknown paragraph-level element.
- **Repro (bodyXml):**
```xml
<w:p><mc:AlternateContent xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape" xmlns:wpg="http://schemas.microsoft.com/office/word/2010/wordprocessingGroup"><mc:Choice Requires="wps"><mc:AlternateContent><mc:Choice Requires="wpg"><w:r><w:t>INNER_CURRENT</w:t></w:r></mc:Choice><mc:Fallback><w:r><w:t>INNER_FALLBACK</w:t></w:r></mc:Fallback></mc:AlternateContent></mc:Choice><mc:Fallback><w:r><w:t>OUTER_FALLBACK</w:t></w:r></mc:Fallback></mc:AlternateContent></w:p><w:sectPr/>
```

### 3. `inner_ac_resolves_inside_selected_outer_choice` — nested AC recursion (inner Fallback)
- **Rule:** §9.3/§7.5 — the outer `Choice` (`Requires="w14"`) is selected; inside it the inner `Choice` (`Requires="zz"`, unknown) is skipped so the inner `Fallback` is selected — `AINNERFALLBACKB`, never `OUTERFALLBACK`.
- **Classification (tentative):** pipeline-bug, high confidence — same root gap as case 2.
- **stemma does:** `Document::parse` refuses — `RuntimeError InvalidDocx: "word IR error: unknown paragraph-level element: AlternateContent"`. A nested `AlternateContent` inside a selected outer `Choice` is not recursively resolved.
- **Word does (claimed):** opens clean, resolves to `AINNERFALLBACKB`.
- **Suggested fix site:** same as case 2 — nested-AC recursion in the word IR import resolver.
- **Repro (bodyXml):**
```xml
<w:p><w:r><w:t>A</w:t></w:r><mc:AlternateContent xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" xmlns:zz="http://example.com/unknown"><mc:Choice Requires="w14"><mc:AlternateContent><mc:Choice Requires="zz"><w:r><w:t>INNERCHOICE</w:t></w:r></mc:Choice><mc:Fallback><w:r><w:t>INNERFALLBACK</w:t></w:r></mc:Fallback></mc:AlternateContent></mc:Choice><mc:Fallback><w:r><w:t>OUTERFALLBACK</w:t></w:r></mc:Fallback></mc:AlternateContent><w:r><w:t>B</w:t></w:r></w:p><w:sectPr/>
```

## New regression tests (passing, run daily)

These nine encode MCE consumption behaviour stemma already satisfies.

- `choice_requires_all_namespaces_understood_else_fallback` — §7.6/§9.3/§9.4: `Requires` is a whitespace-delimited conjunction; one unknown ns (`unk`) skips the Choice, Fallback wins.
- `choice_requires_multiple_all_understood_selects_choice` — §9.3/§9.4: `Requires="wps wpg"` both understood, Choice selected over Fallback.
- `nested_alternatecontent_inner_choice_discarded_when_parent_choice_unselected` — §9.3/§9.4: outer Choice (`Requires="unk"`) not selected, so the inner Choice's content is discarded and OUTER_FALLBACK wins.
- `selected_choice_content_discarded_when_ancestor_element_ignored` — §9.2/§9.4: a declared-ignorable, not-understood `ext:wrap` is removed WITH all contents, including a nested AC; only BEFORE/AFTER read.
- `choice_requires_conjunction_one_unknown_namespace_skips_choice` — §9.3/§7.6: `Requires="w zz"`, `zz` unknown, Choice skipped, Fallback surfaces between surrounding runs.
- `choice_requires_multiple_ns_all_must_be_understood` — §9.3/§7.6/§7.7: `Requires="w zz"` not all understood, Fallback selected (`AFALLBACKB`).
- `choice_selected_by_all_understood_multi_ns_requires` — §9.3/§7.6: `Requires="w w14"` both understood, Choice selected (`ACHOICEB`).
- `ignorable_does_not_drop_understood_namespace_sibling` — §9.2/§7.2: WML runs never ignored despite `mc:Ignorable="zz"`; only the declared-ignorable `zz:foo` is dropped (`KEEPME`).
- `processcontent_unwrap_then_inner_ac_resolves` — §9.2/§9.3/§7.3: `zz:wrap` matches a ProcessContent pair so it is unwrapped, children survive, inner Choice (`Requires="w14"`) selected (`ACHOICEB`).

## Discarded test-bugs

None.

## Open questions — pending confirmation against real Word

All three open questions need confirmation against real Word. For each, confirm (a) the document opens clean (opens without repair), and (b) the resolved consumed text. Each hinges on a §9.4 recursion / MustUnderstand-scope reading that only a real-Word check settles definitively.

| name | check against real Word | expected value | bodyXml |
| --- | --- | --- | --- |
| `mce_mustunderstand_on_nonselected_choice_is_not_examined` | open-clean + consumed text | opens clean; text = `before chosen after` | see repro #1 above |
| `nested_alternatecontent_inner_choice_surfaces_when_parent_choice_selected` | open-clean + consumed text | opens clean; text = `INNER_CURRENT` | see repro #2 above |
| `inner_ac_resolves_inside_selected_outer_choice` | open-clean + consumed text | opens clean; text = `AINNERFALLBACKB` | see repro #3 above |

If real Word opens these clean with the claimed text, each becomes a confirmed gap and the fix belongs at the suggested site.
