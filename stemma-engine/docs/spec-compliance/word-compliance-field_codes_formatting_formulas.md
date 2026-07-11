# Word-compliance — Field codes catalog, switches/formatting, and =formula evaluation

0 confirmed gaps, 12 new regression tests (green), 0 test-bugs.

## Confirmed incompliances

None. The tests probe complex and simple fields (`fldChar`/`instrText`/`fldSimple`) end to end: instruction-text verbatim survival across serialize (escaped operators, multi-section numeric `\#` pictures, date `\@` pictures, general `\*` switches including `FirstCap`/`Upper`/`CHARFORMAT`/`MERGEFORMAT`/`MERGEFORMATINET`, and instruction text split across `instrText` runs), structural fidelity of the begin→separate→end boundary and of multi-run cached results, schema-valid opens-clean, and read semantics — confirming the engine surfaces the cached result verbatim and never recomputes an `=formula` or applies update-time switch casing. Every constraint held.

## New regression tests

- `formula_not_equal_operator_survives_escaped_and_cached_one_reads` — §17.16.3.3: the `<>` not-equal operator reserializes XML-escaped (`&lt;&gt;`) verbatim inside `instrText`, and accept/reject both read the cached `1` (no recomputation).
- `numeric_picture_three_subpictures_survive_verbatim` — §17.16.4.2: a positive;negative;zero `\#` picture is one argument; all sections and literal `;` separators reserialize verbatim and the field opens clean.
- `firstcap_general_switch_survives_and_cached_result_not_recased` — §17.16.4.3.2/.3: an `AUTHOR \* FirstCap` field reads its cached lowercase result on accept and reject; the engine never applies update-time casing.
- `mergeformat_multi_run_result_structure_preserved` — §17.16.4.3.3: a `\* MERGEFORMAT` field keeps its multi-run result structure (underline `rPr` on the seconds run) and re-emits the full quoted instruction verbatim.
- `numeric_picture_section_separator_survives_verbatim` — §17.16.4.2: the `;` section separator in a `\#` picture is preserved (never split on `;`), and accept reads the cached numeric result.
- `date_picture_no_whitespace_stays_unquoted` — MS-OI29500 §2.1.458 / §17.16.4.1: a whitespace-free `\@ yyyy-MM-dd` picture stays unquoted (the serializer adds no quotes), and accept reads the cached date.
- `charformat_complex_field_result_reads_instruction_hidden` — §17.16.4.3.3/§17.16.18: a `\* CHARFORMAT` field displays the leading run plus cached result on accept and reject; the instruction stays hidden.
- `mergeformatinet_on_includepicture_survives_verbatim` — MS-OI29500 §2.1.459 / §17.16.5.27: the Word-only `\* MERGEFORMATINET` switch and its `INCLUDEPICTURE` field name survive byte-verbatim and the field opens clean.
- `charformat_first_instrtext_run_rpr_survives` — §17.16.4.3.3/§17.16.23: the `rPr` (b/color/u) on the first `instrText` run survives serialize, the instruction split across runs reserializes verbatim, and the fldChars stay in begin→separate→end order.
- `general_upper_switch_not_applied_to_cached_result_text` — §17.16.4.3.2/.3/§17.16.19: a `fldSimple` MERGEFIELD with `\* Upper` reads its cached lowercase result on accept and reject; the switch is not applied on read.
- `numeric_picture_with_positive_negative_zero_subpictures_survives` — §17.16.4.2/§17.16.19: a `fldSimple` positive;negative;zero `\#` picture with an embedded quoted literal reserializes byte-verbatim and opens clean.
- `coexisting_date_and_numeric_switches_both_survive` — §17.16.4.1/.2/§17.16.19: a `fldSimple` bearing both a `\@` and a `\#` switch is valid OOXML and opens without repair.

## Discarded test-bugs

None.

## Open questions — pending confirmation against real Word

None.
