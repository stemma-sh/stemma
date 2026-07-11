# Word-compliance sweep — OMML (§22.1 Math): structure fidelity + Word variations for oMath/oMathPara round-trip

Summary: 0 confirmed gaps, 12 new regression tests (green), 0 test-bugs.

## Confirmed incompliances

None. Probed whether stemma corrupts OMML structure on import → re-serialize (verbatim opaque preservation): math-zone/display-math element survival, Annex A child ordering, namespace binding, `m:ctrlPr`/`w:rPr` nesting, `m:oMathParaPr`/`m:jc` justification, nested-fraction `m:f`/`m:num`/`m:den` shape, and `xml:space="preserve"` significant whitespace on `m:t` — all round-trip faithfully and the re-serialized documents validate clean (open in Word without repair).

## New regression tests

- `omathpara_jc_property_preserved_in_display_math` — §22.1.2.79/§22.1.3.7: `m:oMathParaPr` survives and its `m:jc m:val="right"` is preserved verbatim, not dropped or normalized to the centerGroup default.
- `omathpara_emitted_as_direct_child_of_paragraph` — §22.1.2.78: `m:oMathPara` round-trips as a direct child of `w:p`, never wrapped in a `w:r` run.
- `math_text_significant_whitespace_xmlspace_preserved` — §22.1.2.116: the `xml:space="preserve"` marker and the exact leading/trailing whitespace bytes on `m:t` round-trip unchanged.
- `math_namespace_prefix_preserved_on_roundtrip` — §22.1/§22.1.2.77: OMML elements stay bound to the math namespace URI and `m:oMath` survives in the `m:` prefix Word uses.
- `nested_fraction_structure_roundtrips_verbatim` — §22.1.2.36: nested `m:f`/`m:num`/`m:den` nesting and num-then-den child ordering round-trip without being flattened or reordered.
- `omml_ctrlpr_inner_wrpr_preserved_roundtrip` — §22.1.2.23: `m:ctrlPr` and its nested `w:rPr` (Cambria Math `w:rFonts` + `w:i`) survive, preserving control-character formatting.
- `omml_mt_xml_space_preserve_roundtrip` — §22.1.2.116: `m:t` with `xml:space="preserve"` keeps its doubled spaces (`a  +  b`) verbatim so equation spacing is unchanged.
- `omml_pmark_omath_runprop_only_on_paragraph_mark_preserved` — §17.3.2.22 + Annex A CT_RPr: `w:oMath` inside the paragraph-mark `w:rPr` is preserved and ordered after `w:specVanish` (pos 38 → 39).
- `omml_omathpara_paraprops_jc_preserved` — §22.1.2.78/79: `m:oMathParaPr` (carrying `m:jc m:val="right"`) round-trips in place as the first child of `m:oMathPara`.
- `omml_nested_fraction_structure_not_corrupted_when_text_edited` — §22.1.2.77: the container-independent `oMath` subtree keeps its nested `m:f`/`m:num`/`m:den` structure intact through the pipeline.
- `omath_text_xml_space_preserve_roundtrips` — §22.1.2.92 (m_CT_Text): leading/trailing significant spaces inside `m:t` survive verbatim, not collapsed or trimmed.
- `omath_ctrlpr_carries_wrpr_roundtrips` — §22.1.2.23 (m_CT_CtrlPr = w_EG_RPrMath?): `m:ctrlPr` legitimately nests `w:rPr`; the nested Cambria Math run properties survive verbatim.

## Discarded test-bugs

None.

## Open questions — pending confirmation against real Word

None.
