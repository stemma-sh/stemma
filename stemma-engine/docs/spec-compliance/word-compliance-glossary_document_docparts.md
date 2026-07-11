# Word-compliance — Glossary document and docPart entries (building blocks)

0 confirmed gaps, 12 new regression tests (green), 0 test-bugs.

## Confirmed incompliances

None. The tests probe the glossary document part and its `w:docParts` / `w:docPart` building-block entries — specifically the `w:docPartPr` metadata (`w:name`/`w:unique`/`w:category` with `w:name`+`w:gallery`) — checking that gallery enum tokens, optional/absent gallery and category, the `w:unique` `CT_OnOff` toggle, and child-element ordering all round-trip and open clean in Word.

## New regression tests

- `docpartlist_txtbox_gallery_enum_clean` — a `docPart` whose category gallery is the `txtBox` enum token round-trips and opens clean.
- `docpartobj_unique_without_gallery_opens_clean` — a unique `docPart` entry with no `w:gallery` on its category opens clean.
- `docpartlist_category_without_gallery_opens_clean` — a `docPart` carrying a category `w:name` but no `w:gallery` opens clean.
- `docpartobj_autotxt_gallery_enum_clean` — the `autoTxt` gallery enum token on a `docPart` round-trips and opens clean.
- `docpartobj_default_gallery_enum_opens_clean` — the `default` gallery enum token on a `docPart` opens clean.
- `docpartlist_category_only_no_gallery_opens_clean` — a `docPart` category supplying only `w:name` (gallery omitted) opens clean.
- `docpartlist_empty_opens_clean` — an empty `w:docParts` list serializes and opens clean.
- `docpartobj_autotxt_gallery_order_gallery_before_unique` — within the entry, `w:gallery` is serialized before `w:unique`, matching Annex A child ordering.
- `docpartlist_any_gallery_enum_opens_clean` — the `any` gallery enum token on a `docPart` opens clean.
- `docpartobj_gallery_schema_enum_token_tblofcontents_clean` — the `tblOfContents` gallery enum token (schema-valid) round-trips and opens clean.
- `docpartunique_off_value_preserved_ct_onoff` — a `w:unique` set to the `off` value is preserved as a `CT_OnOff` toggle rather than dropped or forced on.
- `docpartlist_gallery_default_enum_clean` — the `default` gallery enum on a `docPart` list entry round-trips and opens clean.

## Discarded test-bugs

None.

## Open questions — pending confirmation against real Word

None.
