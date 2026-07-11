# Word-compliance — Custom XML markup, smart tags, and their tracked-change ranges

0 confirmed gaps, 12 new regression tests (green), 0 test-bugs.

## Confirmed incompliances

None. The tests probe inline and block-level `w:customXml` / `w:smartTag` transparency (read-through on accept/reject), the empty-content placeholder affordance, `displacedByCustomXml` leniency on annotation range markers, unmatched `w:customXmlInsRangeEnd` inertness, verbatim round-trip of the office-namespace `smartTagPr`/`attr` property bag, smart-tags nested inside `w:ins`/`w:del`, and Annex A child ordering (`customXmlPr`/`smartTagPr` before content, `placeholder` before `attr`) on re-nest serialization; stemma matches Word on every case.

## New regression tests

- `empty_inline_customxml_placeholder_never_enters_text` — an empty inline `w:customXml` placeholder is display-only chrome and never enters the character stream (§17.5.1.8/.3).
- `displaced_by_customxml_ignored_when_no_block_customxml_comment_marker` — `displacedByCustomXml` on a `commentRangeStart` with no block-level customXml present is silently ignored, not an error (§17.13.5/§17.18.13).
- `displaced_by_customxml_no_effect_on_inline_customxml` — `displacedByCustomXml` is inert for inline customXml; the wrapped run reads through transparently (§17.13.5/§17.18.13).
- `block_level_customxml_around_table_is_transparent` — a block-level customXml wrapping a whole table is a transparent container; cell text reads through (§17.5.1.6, MS-OI29500 §2.1.191).
- `custom_xml_insrange_unmatched_end_no_insertion_present` — an unmatched `w:customXmlInsRangeEnd` is ignored, no insertion is present, and accept/reject read identically (§17.13.5.6).
- `smarttag_pr_attr_uri_office_namespace_roundtrips_verbatim` — the office-namespace uri (the only value Word accepts) on a `smartTagPr`/`attr` round-trips verbatim (§17.5.1.2/.10, MS-OI29500 §2.1.187).
- `smarttag_inside_ins_resolves_as_inserted_text` — a `smartTag` inside `w:ins` is transparent; its runs resolve as ordinary inserted text (accept keeps, reject removes) (§17.5.1.9/§17.13.5.1, MS-OI29500 §2.1.192).
- `smarttag_inside_del_resolves_as_deleted_text` — a `smartTag` inside `w:del` is transparent; its runs resolve as ordinary deleted text (accept removes, reject restores) (§17.5.1.9/§17.13.5.14, MS-OI29500 §2.1.192).
- `block_customxml_wrapping_table_is_transparent` — a uri-less block-level customXml wrapping a table is a transparent block container; the assumed-null uri requires no attachedSchema (§17.5.1.6, MS-OI29500 §2.1.191).
- `smarttag_pr_renest_precedes_run_content` — on re-nest the serializer emits `smartTagPr` before the wrapped run content per CT_SmartTagRun = (smartTagPr?, EG_PContent*) (§17.5.1.9/.10, Annex A).
- `customxmlpr_renest_placeholder_then_attr_before_runs` — on re-nest the serializer emits `customXmlPr` before runs and `placeholder` before `attr` per CT_CustomXmlRun/CT_CustomXmlPr (§17.5.1.3/.7/.8, Annex A).
- `custom_xml_pr_precedes_block_content` — in a block-level customXml the `customXmlPr` child serializes first, immediately before the wrapped paragraph, per CT_CustomXmlBlock (§17.5.1.6/.7, §A.1).

## Discarded test-bugs

None.

## Open questions — pending confirmation against real Word

None.
