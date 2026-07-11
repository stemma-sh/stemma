# Word-compliance sweep — settings.xml settings (updateFields, decimalSymbol, compat) and Mail Merge / ODSO

Summary: 0 confirmed gaps, 9 new regression tests (green), 0 test-bugs.

## Confirmed incompliances

None. Coverage spans settings.xml document-level flags (`w:updateFields`, `w:decimalSymbol`, `w:compat`/compatibility settings) together with Mail Merge field plumbing — `MERGEFIELD` complex fields, `w:fldSimple` mergefields, the `\* MERGEFORMAT` / general-format switches, the `w:fldLock` attribute, quoted field-name arguments, cached field results, and the `w:fldData` ODSO blob — checking that stemma preserves the instruction text, switches, lock state, and embedded data verbatim while reading only the materialized result text. Every probe matched Word's behavior.

## New regression tests

All in `stemma-engine/tests/spec_settings_document_and_mail_merge_word_compliance.rs`, all passing:

- `mergefield_empty_cached_result_reads_empty` — a MERGEFIELD whose cached result is empty reads as empty text; the instruction never leaks into the read view.
- `mergefield_multirun_cached_result_concatenated_in_read` — a cached result split across multiple runs is concatenated into one contiguous string in the read view.
- `mergefield_general_format_switch_preserved_and_hidden` — the general-format switch on a MERGEFIELD is preserved on serialize and stays out of the read text.
- `fldsimple_mergefield_with_switch_instr_preserved` — a `w:fldSimple` MERGEFIELD carrying a switch keeps its full `w:instr` verbatim across roundtrip.
- `mergeformat_switch_complex_field_preserved_and_not_read` — a complex field with `\* MERGEFORMAT` preserves the switch and excludes the instruction from the read text.
- `mergefield_quoted_name_with_m_switch_verbatim` — a quoted merge-field name plus the `\m` switch survives roundtrip byte-for-byte in the instruction.
- `fldsimple_mergefield_fldlock_attr_preserved` — the `w:fldLock` attribute on a `w:fldSimple` mergefield is retained on serialize.
- `fldsimple_mergefield_instr_switches_preserved_verbatim` — all instruction switches on a `w:fldSimple` mergefield are preserved exactly as written.
- `complex_mergefield_with_flddata_blob_preserved` — the `w:fldData` ODSO data blob on a complex mergefield is preserved intact through the roundtrip.

## Discarded test-bugs

None.
