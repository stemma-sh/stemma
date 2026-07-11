# Word-compliance sweep — OPC physical package (ZIP): structural validity, part-name constraints, feature restrictions

0 confirmed gaps, 8 new regression tests (green), 0 test-bugs.

Test file: `stemma-engine/tests/spec_opc_zip_physical_package_word_compliance.rs`
Spec basis: ISO/IEC 29500-2 (OPC) §7.2–§7.3 + Annex B, ECMA-376 Part 1 §11.2, wml.xsd CT_Body, MS-OE376 §2.1.8.

## Confirmed incompliances

None. The sweep probed ZIP item-name mapping (leading-slash stripping, ASCII, uniqueness), the minimal conformant WordprocessingML package, production-side ZIP feature restrictions (stored/DEFLATE only, no encryption), consumption tolerance for streaming-mode (bit-3 / data-descriptor) entries and EOCD archive comments, zero-block bodies with only a trailing sectPr, and resave conformance of the content-type item and main-part item name — stemma matched Word-aligned behavior on all of them.

## New regression tests

All eight pass and run daily (no `#[ignore]`):

- `opc_main_document_zip_item_name_no_leading_slash_ascii` — the Main Document part `/word/document.xml` is stored as the single ZIP item `word/document.xml` (leading slash removed, ASCII, non-interleaved; ISO 29500-2 §7.3.2/§7.3.3/§7.3.4).
- `opc_minimal_wordprocessingml_package_opens_clean` — the minimal conformant package (content-type item + package-relationship item + one Main Document with an empty-paragraph body) validates with no errors (§11.2, ISO 29500-2 §7.2.2).
- `opc_package_production_deflate_or_stored_only_no_encryption` — produced packages use only stored/DEFLATE with no encryption, so any conforming reader can fully inflate the main part (ISO 29500-2 §7.3.1/§7.3.6, Annex B Tables B.3/B.4).
- `zip_data_descriptor_entries_open_clean` — a streaming-mode ZIP (general-purpose bit 3, zeroed local headers, signed data descriptors) parses, validates clean, and reads back the correct text (Annex B Tables B.1/B.5: consumption=Yes).
- `zip_eocd_archive_comment_tolerated` — a non-empty EOCD archive comment is read-tolerated and ignored; parse, validate, and text read-back all succeed (Annex B Table B.2: consumption=Yes, production=No).
- `opc_body_zero_blocks_only_sectpr_opens_clean` — a body with zero block-level elements and only a trailing `w:sectPr` is schema-valid (CT_Body EG_BlockLevelElts minOccurs=0), validates clean, and the sectPr survives re-save.
- `opc_resave_content_type_item_and_unprefixed_main_part` — a saved package still contains `[Content_Types].xml` and the main part under the slash-free item name `word/document.xml`, and revalidates clean (§11.2, ISO 29500-2 §7.3.3/§7.3.4/§7.3.7, MS-OE376 §2.1.8).
- `zip_item_name_mapping_strips_leading_slash_on_save` — on save, the logical part name `/word/document.xml` maps to the ZIP item `word/document.xml` (slash stripped, `/` separators, ASCII), readable strictly by that name (ISO 29500-2 §7.3.3/§7.3.4).

## Discarded test-bugs

None.

## Open questions — pending confirmation against real Word

None.
