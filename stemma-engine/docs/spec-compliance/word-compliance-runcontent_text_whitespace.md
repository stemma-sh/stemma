# Word-compliance sweep — Run text content and whitespace preservation (t, delText, instrText)

0 confirmed gaps, 12 new regression tests (green), 0 test-bugs.

## Confirmed incompliances

None. Coverage spans `w:t` literal-text semantics (XML predefined-entity decode on read and re-escape on serialize, empty-`w:t` no-contribution, `xml:space="preserve"` whitespace significance), CT_R / EG_RunInnerContent ordering and document-order preservation across heterogeneous inner content, `delText` whitespace handling across accept/reject, and the opaque-object read surface of `w:sym`, `w:pgNum`, and `w:ptab` (one U+FFFC each, distinct from the `w:tab` = U+0009 case); stemma matched Word/spec behavior on every probe.

## New regression tests

- `t_xml_entity_escapes_unescaped_in_read_text` — `&amp;`/`&lt;`/`&gt;` in `w:t` decode to `& < >` on the read surface (§17.3.3.31, XML 1.0 §2.10).
- `sym_is_opaque_object_replacement_not_glyph_in_read_text` — `w:sym` reads as one U+FFFC opaque object, not literal text, with neighbours intact (§17.3.3.30, Unicode U+FFFC).
- `empty_t_contributes_no_text_neighbors_concatenate` — an empty `w:t` adds no characters; adjacent `w:t` payloads concatenate verbatim with no separator (§17.3.3.31).
- `t_text_xml_entities_decode_in_read_and_reescape_on_serialize` — entities decode on read and the serializer re-escapes them on save by rebuilding `w:t` from the decoded text model (§17.3.3.31, XML 1.0 §2.4).
- `empty_t_contributes_no_text_and_concatenates_siblings` — an empty `w:t` between two non-empty siblings reads as the two payloads concatenated (`AB`), no invented whitespace/glyph (§17.3.3.31).
- `whitespace_only_t_with_preserve_keeps_all_spaces_in_read_surface` — a whitespace-only `w:t` with `xml:space="preserve"` is significant whitespace: three interior spaces survive into the read surface (§17.3.3.31, XML 1.0 §2.10).
- `xml_predefined_entities_in_t_roundtrip` — predefined entities decode on read and re-emit as escapes on serialize, never raw and never double-escaped (§17.3.3.31, XML 1.0 §2.10, §A.1 CT_Text).
- `rpr_precedes_run_inner_content` — CT_R is EG_RPr (once) then EG_RunInnerContent: `w:rPr` serializes before `w:t` within the run (§17.3.3, §A.1 CT_R, §17.3.2.28).
- `mixed_run_content_document_order_preserved` — EG_RunInnerContent keeps authored order (t/tab/t) and the two text fragments do not coalesce across the tab (§17.3.3, §17.3.3.31, §17.3.3.32, §A.1 EG_RunInnerContent).
- `deltext_preserve_whitespace_concatenates_with_live_run_on_reject` — live trailing preserve-space + deleted `delText` leading preserve-space: reject re-joins both preserved fragments, accept drops exactly the `delText` leaving the live trailing space (§17.3.3.7, §17.3.3.31, §17.13.5.14, XML 1.0 §2.10).
- `pgnum_block_is_opaque_barrier_not_literal_text` — `w:pgNum` is a legacy field-like marker reading as one U+FFFC with no stored digit text, surrounding prose preserved, reject == accept (§17.3.3.22, Unicode U+FFFC).
- `ptab_is_opaque_barrier_distinct_from_tab` — `w:ptab` (absolute-position tab) surfaces as one U+FFFC positioning object, distinct from `w:tab` = U+0009 (§17.3.3.23, §17.3.3.32, Unicode U+FFFC).

## Discarded test-bugs

None.
