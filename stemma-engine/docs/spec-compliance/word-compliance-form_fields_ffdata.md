# Word-compliance — Form fields (ffData: textInput/checkBox/ddList) and form-field state

0 confirmed gaps, 12 new regression tests (green), 0 test-bugs.

## Confirmed incompliances

None. The tests probe CT_FFData and its three branches (CT_FFTextInput, CT_FFCheckBox, CT_FFDDList) per ISO/IEC 29500-1 §17.16 and §A.1: load-time tolerances (out-of-range ddList result index, FORMTEXT cached value exceeding maxLength, maxLength=0, non-numeric cached value under type=number), Word consumption semantics (checkbox state is consumed state not run text, ddList result index overrides default for the displayed selection), and CT_FFTextInput/CT_FFCheckBox/CT_FFDDList child-ordering round-trip fidelity (Annex A xsd:sequence), plus balanced fldChar emission for two form fields in one paragraph. stemma matches Word on every probed case.

## New regression tests

- `ddlist_overlong_result_index_opens_clean` — an out-of-range FORMDROPDOWN result index is ignored (falls back to default), not a load error (§17.16.28/§17.16.9).
- `checkbox_checked_omitted_state_from_default` — a checkBox with omitted checked + a default is well-formed, and the derived state is consumed state, never leaking into reading text (§17.16.8/§17.16.12).
- `textinput_maxlength_exceeded_on_load_opens_clean` — a FORMTEXT cached result exceeding maxLength at load shall not result in an error (§17.16.26).
- `formtext_maxlength_zero_opens_clean` — maxLength=0 is an edit-time clamp, not a load failure (§17.16.26 + MS-OI29500 §2.1.524).
- `two_form_fields_one_paragraph_balanced_fldchars` — two sibling form fields each emit their own balanced begin/end fldChars, unmerged, and open without repair (§17.16.17/§17.16.18/§17.18.26).
- `formtext_number_type_does_not_coerce_cached_run` — type=number is an editing hint; the cached run surfaces verbatim with no coercion or blanking, and opens clean (§17.16.34).
- `ddlist_child_order_result_default_listentry_roundtrip` — CT_FFDDList round-trips result, default, listEntry* order with listEntry entries in One/Two/Three appearance order (§A.1 / §17.16.25).
- `checkbox_size_default_checked_order_roundtrip` — CT_FFCheckBox round-trips (size|sizeAuto), default, checked in xsd:sequence order and opens clean (§A.1).
- `checkbox_size_half_point_value_preserved` — size/@val is a half-point count and is preserved verbatim (20 not converted to 10pt) (§17.16.29 / §A.1).
- `textinput_type_number_format_pattern_roundtrip` — CT_FFTextInput round-trips type before maxLength before format and keeps the format mask verbatim (§A.1 / §17.18.28).
- `ddlist_result_before_default_before_listentry_sequence` — a conformant serializer keeps result before default before the first listEntry (§17.16.28 / §A.1).
- `ddlist_result_index_overrides_default_for_displayed_selection` — result=2 selects 'Three' (overriding default=1); accept-all and reject-all read projections surface the cached selection verbatim (§17.16.28).

## Discarded test-bugs

None.

## Open questions — pending confirmation against real Word

None.
