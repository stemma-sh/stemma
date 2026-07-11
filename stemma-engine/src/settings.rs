//! Parse document settings from `word/settings.xml`.

use crate::docx::DocxArchive;
use crate::domain::{CompatSettings, DocProtectEdit, DocumentProtection};

/// Extract the local name from a possibly namespace-prefixed element name.
fn local_name(name: &str) -> &str {
    if let Some(pos) = name.find(':') {
        &name[pos + 1..]
    } else {
        name
    }
}

fn parse_settings_root(archive: &DocxArchive) -> Result<Option<xmltree::Element>, String> {
    let Some(xml_bytes) = archive.get("word/settings.xml") else {
        return Ok(None);
    };

    crate::word_xml::parse_document_xml(xml_bytes)
        .map(Some)
        .map_err(|err| format!("failed to parse word/settings.xml: {err:?}"))
}

fn parse_on_off_value(value: &str, context: &str) -> Result<bool, String> {
    match value {
        "1" | "true" | "on" => Ok(true),
        "0" | "false" | "off" => Ok(false),
        other => Err(format!("{context} has invalid CT_OnOff value '{other}'")),
    }
}

/// Parse `w:evenAndOddHeaders` from `word/settings.xml` (ISO 29500-1 §17.15.1.35).
///
/// When this element is present (it's a CT_OnOff toggle element — presence means "on"),
/// the document uses different headers/footers for even and odd pages.
/// When absent, even-page headers/footers should be ignored.
///
/// Returns `true` if the element is present, `false` otherwise.
///
/// Test-support only: the production reader uses the richer
/// [`parse_even_and_odd_headers_state`] path that keeps the absent-vs-off
/// distinction; this boolean form exists to exercise the parse directly.
#[cfg(test)]
pub fn parse_even_and_odd_headers(archive: &DocxArchive) -> Result<bool, String> {
    let Some(root) = parse_settings_root(archive)? else {
        return Ok(false);
    };
    Ok(root.children.iter().any(|child| {
        matches!(child, xmltree::XMLNode::Element(el) if local_name(&el.name) == "evenAndOddHeaders")
    }))
}

/// Parse `w:evenAndOddHeaders` as a three-state value, preserving the
/// absent-vs-explicitly-off distinction the boolean [`parse_even_and_odd_headers`]
/// collapses (ISO 29500-1 §17.15.1.35).
///
/// `CT_OnOff` semantics: the element present with no `w:val` (or `w:val="1"` /
/// `"true"` / `"on"`) means **on**; present with `w:val="0"` / `"false"` /
/// `"off"` means **explicitly off**; the element absent means **None** (the
/// setting was never stated — NOT the same as off, per CLAUDE.md "no silent
/// fallbacks"). An invalid `w:val` is a hard error, never coerced.
pub fn parse_even_and_odd_headers_state(archive: &DocxArchive) -> Result<Option<bool>, String> {
    let Some(root) = parse_settings_root(archive)? else {
        return Ok(None);
    };
    for child in &root.children {
        let xmltree::XMLNode::Element(el) = child else {
            continue;
        };
        if local_name(&el.name) != "evenAndOddHeaders" {
            continue;
        }
        return match crate::xml_attrs::attr_get(el, "val") {
            // Present, no explicit val → on (CT_OnOff toggle default).
            None => Ok(Some(true)),
            Some(val) => Ok(Some(parse_on_off_value(
                val,
                "word/settings.xml evenAndOddHeaders",
            )?)),
        };
    }
    Ok(None)
}

/// Apply the desired `w:evenAndOddHeaders` state to a parsed `word/settings.xml`
/// root element (ISO 29500-1 §17.15.1.35), returning whether the element set
/// changed. This is the WRITER counterpart to [`parse_even_and_odd_headers_state`]:
///
/// - `None`  → remove any existing `w:evenAndOddHeaders` element (absent).
/// - `Some(true)`  → ensure a bare `<w:evenAndOddHeaders/>` is present (on).
/// - `Some(false)` → ensure `<w:evenAndOddHeaders w:val="0"/>` (explicitly off).
///
/// The element is inserted at the front of the settings root when newly added.
/// ECMA-376 sequences `w:settings` children, but Word tolerates ordering for
/// this toggle in practice and we keep the change minimal rather than re-sorting
/// the whole part. Any existing element is rewritten in place (so a present-on
/// element flipped to explicit-off keeps its position).
pub fn set_even_and_odd_headers(root: &mut xmltree::Element, desired: Option<bool>) {
    use xmltree::{Element, XMLNode};

    // Remove every existing evenAndOddHeaders element first; we re-add exactly
    // one when `desired` is `Some(_)`.
    root.children.retain(|child| {
        !matches!(child, XMLNode::Element(el) if local_name(&el.name) == "evenAndOddHeaders")
    });

    let Some(on) = desired else {
        return; // None → absent: leave it removed.
    };

    let mut el = Element::new("evenAndOddHeaders");
    el.prefix = Some("w".to_string());
    el.namespace = Some("http://schemas.openxmlformats.org/wordprocessingml/2006/main".to_string());
    if !on {
        // Explicitly off: emit w:val="0" (present-but-off is distinct from absent).
        crate::xml_attrs::attr_set(&mut el, "w:val", "0");
    }
    // Insert at the front so the toggle is grouped with other top-level settings.
    root.children.insert(0, XMLNode::Element(el));
}

/// Parse the `w:updateFields` "update fields on open" setting from a parsed
/// `word/settings.xml` root (ISO 29500-1 §17.15.1.81).
///
/// `w:updateFields` is a `CT_OnOff` toggle: present with no `w:val` (or
/// `w:val="1"`/`"true"`/`"on"`) instructs the consuming application to refresh
/// every field result (REF/PAGEREF/TOC/SEQ/…) when the document is opened;
/// present with `w:val="0"`/`"false"`/`"off"` means explicitly do not; absent
/// means the document never asserted the setting (`None` — NOT the same as off,
/// per "no silent fallbacks"). An invalid `w:val` is a hard error.
pub fn parse_update_fields(root: &xmltree::Element) -> Result<Option<bool>, String> {
    for child in &root.children {
        let xmltree::XMLNode::Element(el) = child else {
            continue;
        };
        if local_name(&el.name) != "updateFields" {
            continue;
        }
        return match crate::xml_attrs::attr_get(el, "val") {
            None => Ok(Some(true)),
            Some(val) => Ok(Some(parse_on_off_value(
                val,
                "word/settings.xml updateFields",
            )?)),
        };
    }
    Ok(None)
}

/// Apply the desired `w:updateFields` state to a parsed `word/settings.xml`
/// root element (ISO 29500-1 §17.15.1.81) — the WRITER counterpart to
/// [`parse_update_fields`]:
///
/// - `Some(true)`  → ensure `<w:updateFields w:val="true"/>` is present (Word
///   recomputes all field results on the next open).
/// - `Some(false)` → ensure `<w:updateFields w:val="false"/>` (explicitly off).
/// - `None`        → remove any existing `w:updateFields` element (absent).
///
/// Any existing element is removed and exactly one is re-inserted at the front
/// of the settings root for `Some(_)`. We emit an explicit `w:val` (rather than
/// the bare-element shorthand) so the on/off state is unambiguous in the part —
/// this is exactly what Word writes when it sets the flag. ECMA-376 sequences
/// `w:settings` children, but Word tolerates the leading position for this
/// toggle in practice and the validator does not gate settings child order, so
/// we keep the change minimal rather than re-sorting the whole part.
pub fn set_update_fields(root: &mut xmltree::Element, desired: Option<bool>) {
    use xmltree::{Element, XMLNode};

    root.children.retain(
        |child| !matches!(child, XMLNode::Element(el) if local_name(&el.name) == "updateFields"),
    );

    let Some(on) = desired else {
        return; // None → absent: leave it removed.
    };

    let mut el = Element::new("updateFields");
    el.prefix = Some("w".to_string());
    el.namespace = Some("http://schemas.openxmlformats.org/wordprocessingml/2006/main".to_string());
    crate::xml_attrs::attr_set(&mut el, "w:val", if on { "true" } else { "false" });
    root.children.insert(0, XMLNode::Element(el));
}

/// Parse `w:defaultTabStop` from `word/settings.xml`.
///
/// Returns the interval in twips, or `None` if the file is missing or the element is absent.
pub fn parse_default_tab_stop(archive: &DocxArchive) -> Result<Option<i32>, String> {
    let Some(root) = parse_settings_root(archive)? else {
        return Ok(None);
    };
    for child in &root.children {
        let el = match child {
            xmltree::XMLNode::Element(el) => el,
            _ => continue,
        };
        if local_name(&el.name) == "defaultTabStop" {
            let val = crate::xml_attrs::attr_get(el, "val")
                .ok_or_else(|| "word/settings.xml defaultTabStop missing w:val".to_string())?;
            let parsed = val.parse::<i32>().map_err(|err| {
                format!("word/settings.xml defaultTabStop has invalid w:val '{val}': {err}")
            })?;
            return Ok(Some(parsed));
        }
    }
    Ok(None)
}

/// Parse compatibility settings from `w:compat/w:compatSetting` in `word/settings.xml`.
///
/// The XML structure is:
/// ```xml
/// <w:settings>
///   <w:compat>
///     <w:compatSetting w:name="compatibilityMode" w:uri="..." w:val="15"/>
///     <w:compatSetting w:name="overrideTableStyleFontSizeAndJustification" w:uri="..." w:val="1"/>
///     <w:compatSetting w:name="doNotFlipMirrorIndents" w:uri="..." w:val="1"/>
///   </w:compat>
/// </w:settings>
/// ```
///
/// Returns `CompatSettings::default()` if the file is missing or the elements
/// are absent. Each field is `None` when the corresponding setting is not present.
pub fn parse_compat_settings(archive: &DocxArchive) -> Result<CompatSettings, String> {
    let Some(root) = parse_settings_root(archive)? else {
        return Ok(CompatSettings::default());
    };

    // Find the w:compat child of w:settings.
    let compat_el = match root.children.iter().find_map(|child| {
        if let xmltree::XMLNode::Element(el) = child
            && local_name(&el.name) == "compat"
        {
            return Some(el);
        }
        None
    }) {
        Some(el) => el,
        None => return Ok(CompatSettings::default()),
    };

    let mut settings = CompatSettings::default();

    // Iterate over w:compatSetting children.
    for child in &compat_el.children {
        let el = match child {
            xmltree::XMLNode::Element(el) if local_name(&el.name) == "compatSetting" => el,
            _ => continue,
        };

        let name = crate::xml_attrs::attr_get(el, "name")
            .map(|n| n.as_str())
            .ok_or_else(|| "word/settings.xml compatSetting missing w:name".to_string())?;
        let val = crate::xml_attrs::attr_get(el, "val")
            .map(|v| v.as_str())
            .ok_or_else(|| format!("word/settings.xml compatSetting '{name}' missing w:val"))?;

        match name {
            "compatibilityMode" => {
                settings.compatibility_mode = Some(val.parse::<u32>().map_err(|err| {
                    format!(
                        "word/settings.xml compatSetting 'compatibilityMode' has invalid w:val '{val}': {err}"
                    )
                })?);
            }
            "overrideTableStyleFontSizeAndJustification" => {
                settings.override_table_style_font_size_and_justification = Some(
                    parse_on_off_value(
                        val,
                        "word/settings.xml compatSetting 'overrideTableStyleFontSizeAndJustification'",
                    )?,
                );
            }
            "doNotFlipMirrorIndents" => {
                settings.do_not_flip_mirror_indents = Some(parse_on_off_value(
                    val,
                    "word/settings.xml compatSetting 'doNotFlipMirrorIndents'",
                )?);
            }
            "enableOpenTypeFeatures" => {
                settings.enable_open_type_features = Some(parse_on_off_value(
                    val,
                    "word/settings.xml compatSetting 'enableOpenTypeFeatures'",
                )?);
            }
            "differentiateMultirowTableHeaders" => {
                settings.differentiate_multirow_table_headers = Some(parse_on_off_value(
                    val,
                    "word/settings.xml compatSetting 'differentiateMultirowTableHeaders'",
                )?);
            }
            "allowTextAfterFloatingTableBreak" => {
                settings.allow_text_after_floating_table_break = Some(parse_on_off_value(
                    val,
                    "word/settings.xml compatSetting 'allowTextAfterFloatingTableBreak'",
                )?);
            }
            _ => {} // Ignore unknown compat settings.
        }
    }

    Ok(settings)
}

/// Parse a `w:documentProtection/@w:edit` value into [`DocProtectEdit`]
/// (`ST_DocProtect`, ISO/IEC 29500-1 §17.18.31). The enum is closed: an
/// out-of-enum value is a hard error naming the offending value — never coerced
/// to a catch-all.
fn parse_doc_protect_edit(value: &str) -> Result<DocProtectEdit, String> {
    match value {
        "none" => Ok(DocProtectEdit::None),
        "readOnly" => Ok(DocProtectEdit::ReadOnly),
        "comments" => Ok(DocProtectEdit::Comments),
        "trackedChanges" => Ok(DocProtectEdit::TrackedChanges),
        "forms" => Ok(DocProtectEdit::Forms),
        other => Err(format!(
            "word/settings.xml documentProtection has unknown w:edit value '{other}' \
             (ST_DocProtect ISO/IEC 29500-1 §17.18.31 allows: none, readOnly, \
             comments, trackedChanges, forms)"
        )),
    }
}

/// Parse `w:documentProtection` from `word/settings.xml` (`CT_DocProtect`,
/// ISO/IEC 29500-1 §17.15.1.29).
///
/// Returns `None` when the element is absent (the document declares no
/// protection). When present, captures the three facts a host needs to decide
/// policy: the [`DocProtectEdit`] mode (`w:edit`, absent-vs-present kept
/// honestly), the three-state `w:enforcement` toggle, and whether a password
/// credential is present (`w:hash`/`w:salt` legacy or `w:hashValue`/
/// `w:saltValue` agile) — presence only; credential material is never read.
///
/// An out-of-enum `w:edit` or an invalid `w:enforcement` is a hard error (no
/// silent fallback). The engine does NOT enforce protection; this is a reported
/// fact (see [`DocumentProtection`]).
pub fn parse_document_protection(
    archive: &DocxArchive,
) -> Result<Option<DocumentProtection>, String> {
    let Some(root) = parse_settings_root(archive)? else {
        return Ok(None);
    };
    for child in &root.children {
        let xmltree::XMLNode::Element(el) = child else {
            continue;
        };
        if local_name(&el.name) != "documentProtection" {
            continue;
        }

        let edit = match crate::xml_attrs::attr_get(el, "edit") {
            None => None,
            Some(val) => Some(parse_doc_protect_edit(val)?),
        };

        let enforcement = match crate::xml_attrs::attr_get(el, "enforcement") {
            None => None,
            Some(val) => Some(parse_on_off_value(
                val,
                "word/settings.xml documentProtection enforcement",
            )?),
        };

        let has_credential = ["hash", "salt", "hashValue", "saltValue"]
            .iter()
            .any(|name| crate::xml_attrs::attr_get(el, name).is_some());

        return Ok(Some(DocumentProtection {
            edit,
            enforcement,
            has_credential,
        }));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to build a minimal DocxArchive with the given settings.xml content.
    fn archive_with_settings(settings_xml: &str) -> DocxArchive {
        DocxArchive::from_parts(vec![crate::docx::DocxFile {
            name: "word/settings.xml".to_string(),
            data: settings_xml.as_bytes().to_vec(),
        }])
    }

    #[test]
    fn test_parse_default_tab_stop_present() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:defaultTabStop w:val="720"/>
</w:settings>"#;
        let archive = archive_with_settings(xml);
        assert_eq!(parse_default_tab_stop(&archive).unwrap(), Some(720));
    }

    #[test]
    fn test_parse_default_tab_stop_different_value() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:defaultTabStop w:val="360"/>
</w:settings>"#;
        let archive = archive_with_settings(xml);
        assert_eq!(parse_default_tab_stop(&archive).unwrap(), Some(360));
    }

    #[test]
    fn test_parse_default_tab_stop_absent() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:zoom w:percent="100"/>
</w:settings>"#;
        let archive = archive_with_settings(xml);
        assert_eq!(parse_default_tab_stop(&archive).unwrap(), None);
    }

    #[test]
    fn test_parse_default_tab_stop_missing_file() {
        let archive = DocxArchive::from_parts(vec![]);
        assert_eq!(parse_default_tab_stop(&archive).unwrap(), None);
    }

    #[test]
    fn test_parse_default_tab_stop_invalid_value_errors() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:defaultTabStop w:val="abc"/>
</w:settings>"#;
        let archive = archive_with_settings(xml);
        let err = parse_default_tab_stop(&archive).expect_err("invalid defaultTabStop must error");
        assert!(err.contains("defaultTabStop"));
    }

    // ── evenAndOddHeaders tests ─────────────────────────────────────────

    #[test]
    fn test_parse_even_and_odd_headers_present() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:evenAndOddHeaders/>
</w:settings>"#;
        let archive = archive_with_settings(xml);
        assert!(parse_even_and_odd_headers(&archive).unwrap());
    }

    #[test]
    fn test_parse_even_and_odd_headers_absent() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:defaultTabStop w:val="720"/>
</w:settings>"#;
        let archive = archive_with_settings(xml);
        assert!(!parse_even_and_odd_headers(&archive).unwrap());
    }

    #[test]
    fn test_parse_even_and_odd_headers_missing_file() {
        let archive = DocxArchive::from_parts(vec![]);
        assert!(!parse_even_and_odd_headers(&archive).unwrap());
    }

    #[test]
    fn test_parse_even_and_odd_headers_malformed_xml_errors() {
        let archive = archive_with_settings("not xml");
        let err =
            parse_even_and_odd_headers(&archive).expect_err("malformed settings.xml must error");
        assert!(err.contains("word/settings.xml"));
    }

    // ── evenAndOddHeaders three-state parser + writer ───────────────────

    #[test]
    fn state_present_bare_is_on() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:evenAndOddHeaders/>
</w:settings>"#;
        let archive = archive_with_settings(xml);
        assert_eq!(
            parse_even_and_odd_headers_state(&archive).unwrap(),
            Some(true)
        );
    }

    #[test]
    fn state_present_val_zero_is_explicit_off() {
        // Present with w:val="0" is explicitly off — distinct from absent.
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:evenAndOddHeaders w:val="0"/>
</w:settings>"#;
        let archive = archive_with_settings(xml);
        assert_eq!(
            parse_even_and_odd_headers_state(&archive).unwrap(),
            Some(false)
        );
    }

    #[test]
    fn state_absent_is_none_not_off() {
        // Absent is None — NOT the same as explicitly off (no silent fallback).
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:defaultTabStop w:val="720"/>
</w:settings>"#;
        let archive = archive_with_settings(xml);
        assert_eq!(parse_even_and_odd_headers_state(&archive).unwrap(), None);
    }

    #[test]
    fn state_invalid_val_errors() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:evenAndOddHeaders w:val="maybe"/>
</w:settings>"#;
        let archive = archive_with_settings(xml);
        let err = parse_even_and_odd_headers_state(&archive).expect_err("invalid w:val must error");
        assert!(err.contains("evenAndOddHeaders"));
    }

    /// The writer round-trips all three states through parse → set → reparse.
    #[test]
    fn writer_round_trips_three_states() {
        fn round_trip(start: &str, desired: Option<bool>) -> Option<bool> {
            let mut root = xmltree::Element::parse(start.as_bytes()).unwrap();
            set_even_and_odd_headers(&mut root, desired);
            let mut buf = Vec::new();
            root.write_with_config(
                &mut buf,
                xmltree::EmitterConfig::new().write_document_declaration(true),
            )
            .unwrap();
            let archive = DocxArchive::from_parts(vec![crate::docx::DocxFile {
                name: "word/settings.xml".to_string(),
                data: buf,
            }]);
            parse_even_and_odd_headers_state(&archive).unwrap()
        }

        let empty = r#"<?xml version="1.0"?><w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"/>"#;
        // None → absent.
        assert_eq!(round_trip(empty, None), None);
        // Some(true) → present-on.
        assert_eq!(round_trip(empty, Some(true)), Some(true));
        // Some(false) → present-off (distinct from absent).
        assert_eq!(round_trip(empty, Some(false)), Some(false));

        // Writing None onto a doc that HAD the element removes it (→ absent).
        let with = r#"<?xml version="1.0"?><w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:evenAndOddHeaders/></w:settings>"#;
        assert_eq!(round_trip(with, None), None);
        // Flipping present-on to explicit-off.
        assert_eq!(round_trip(with, Some(false)), Some(false));
    }

    // ── compat settings tests ───────────────────────────────────────────

    #[test]
    fn test_parse_compat_settings_compatibility_mode() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:compat>
    <w:compatSetting w:name="compatibilityMode"
                     w:uri="http://schemas.microsoft.com/office/word"
                     w:val="15"/>
  </w:compat>
</w:settings>"#;
        let archive = archive_with_settings(xml);
        let settings = parse_compat_settings(&archive).unwrap();
        assert_eq!(settings.compatibility_mode, Some(15));
        assert_eq!(
            settings.override_table_style_font_size_and_justification,
            None
        );
        assert_eq!(settings.do_not_flip_mirror_indents, None);
    }

    #[test]
    fn test_parse_compat_settings_all_fields() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:compat>
    <w:compatSetting w:name="compatibilityMode"
                     w:uri="http://schemas.microsoft.com/office/word"
                     w:val="15"/>
    <w:compatSetting w:name="overrideTableStyleFontSizeAndJustification"
                     w:uri="http://schemas.microsoft.com/office/word"
                     w:val="1"/>
    <w:compatSetting w:name="doNotFlipMirrorIndents"
                     w:uri="http://schemas.microsoft.com/office/word"
                     w:val="1"/>
  </w:compat>
</w:settings>"#;
        let archive = archive_with_settings(xml);
        let settings = parse_compat_settings(&archive).unwrap();
        assert_eq!(settings.compatibility_mode, Some(15));
        assert_eq!(
            settings.override_table_style_font_size_and_justification,
            Some(true)
        );
        assert_eq!(settings.do_not_flip_mirror_indents, Some(true));
    }

    #[test]
    fn test_parse_compat_settings_false_values() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:compat>
    <w:compatSetting w:name="overrideTableStyleFontSizeAndJustification"
                     w:uri="http://schemas.microsoft.com/office/word"
                     w:val="0"/>
    <w:compatSetting w:name="doNotFlipMirrorIndents"
                     w:uri="http://schemas.microsoft.com/office/word"
                     w:val="0"/>
  </w:compat>
</w:settings>"#;
        let archive = archive_with_settings(xml);
        let settings = parse_compat_settings(&archive).unwrap();
        assert_eq!(settings.compatibility_mode, None);
        assert_eq!(
            settings.override_table_style_font_size_and_justification,
            Some(false)
        );
        assert_eq!(settings.do_not_flip_mirror_indents, Some(false));
    }

    #[test]
    fn test_parse_compat_settings_missing_compat_element() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:zoom w:percent="100"/>
</w:settings>"#;
        let archive = archive_with_settings(xml);
        let settings = parse_compat_settings(&archive).unwrap();
        assert_eq!(settings.compatibility_mode, None);
        assert_eq!(
            settings.override_table_style_font_size_and_justification,
            None
        );
        assert_eq!(settings.do_not_flip_mirror_indents, None);
    }

    #[test]
    fn test_parse_compat_settings_missing_file() {
        let archive = DocxArchive::from_parts(vec![]);
        let settings = parse_compat_settings(&archive).unwrap();
        assert_eq!(settings, CompatSettings::default());
    }

    #[test]
    fn test_parse_compat_settings_unknown_settings_ignored() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:compat>
    <w:compatSetting w:name="unknownSetting"
                     w:uri="http://schemas.microsoft.com/office/word"
                     w:val="42"/>
    <w:compatSetting w:name="compatibilityMode"
                     w:uri="http://schemas.microsoft.com/office/word"
                     w:val="14"/>
  </w:compat>
</w:settings>"#;
        let archive = archive_with_settings(xml);
        let settings = parse_compat_settings(&archive).unwrap();
        assert_eq!(settings.compatibility_mode, Some(14));
    }

    #[test]
    fn test_parse_compat_settings_invalid_bool_errors() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:compat>
    <w:compatSetting w:name="doNotFlipMirrorIndents"
                     w:uri="http://schemas.microsoft.com/office/word"
                     w:val="maybe"/>
  </w:compat>
</w:settings>"#;
        let archive = archive_with_settings(xml);
        let err = parse_compat_settings(&archive).expect_err("invalid compat bool must error");
        assert!(err.contains("doNotFlipMirrorIndents"));
    }

    // ── documentProtection tests ────────────────────────────────────────

    fn protection_of(inner: &str) -> Option<DocumentProtection> {
        let xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  {inner}
</w:settings>"#
        );
        parse_document_protection(&archive_with_settings(&xml)).unwrap()
    }

    #[test]
    fn protection_absent_is_none() {
        // No documentProtection element → None (NOT a synthesized default).
        assert_eq!(protection_of(r#"<w:defaultTabStop w:val="720"/>"#), None);
    }

    #[test]
    fn protection_missing_file_is_none() {
        let archive = DocxArchive::from_parts(vec![]);
        assert_eq!(parse_document_protection(&archive).unwrap(), None);
    }

    #[test]
    fn protection_each_edit_mode_parses() {
        // Every ST_DocProtect member the domain models round-trips to its enum.
        for (raw, expected) in [
            ("none", DocProtectEdit::None),
            ("readOnly", DocProtectEdit::ReadOnly),
            ("comments", DocProtectEdit::Comments),
            ("trackedChanges", DocProtectEdit::TrackedChanges),
            ("forms", DocProtectEdit::Forms),
        ] {
            let p = protection_of(&format!(
                r#"<w:documentProtection w:edit="{raw}" w:enforcement="1"/>"#
            ))
            .expect("protection present");
            assert_eq!(p.edit, Some(expected), "edit={raw}");
            assert_eq!(p.enforcement, Some(true));
            assert!(!p.has_credential);
        }
    }

    #[test]
    fn protection_enforcement_three_state() {
        // enforcement="1" → on, "0" → explicitly off, absent → None.
        assert_eq!(
            protection_of(r#"<w:documentProtection w:edit="forms" w:enforcement="1"/>"#)
                .unwrap()
                .enforcement,
            Some(true)
        );
        assert_eq!(
            protection_of(r#"<w:documentProtection w:edit="forms" w:enforcement="0"/>"#)
                .unwrap()
                .enforcement,
            Some(false)
        );
        assert_eq!(
            protection_of(r#"<w:documentProtection w:edit="forms"/>"#)
                .unwrap()
                .enforcement,
            None
        );
    }

    #[test]
    fn protection_edit_attr_absent_is_none_not_error() {
        // A protection element with no w:edit is a valid declared state (the
        // edit mode is unspecified) — edit is None, not a parse error.
        let p = protection_of(r#"<w:documentProtection w:enforcement="1"/>"#)
            .expect("protection present");
        assert_eq!(p.edit, None);
        assert_eq!(p.enforcement, Some(true));
    }

    #[test]
    fn protection_credential_presence_detected_not_stored() {
        // Legacy hash/salt.
        let legacy = protection_of(
            r#"<w:documentProtection w:edit="readOnly" w:enforcement="1"
                 w:cryptProviderType="rsaAES" w:cryptAlgorithmClass="hash"
                 w:hash="abc/def==" w:salt="xyz+123=="/>"#,
        )
        .unwrap();
        assert!(legacy.has_credential);
        // Agile hashValue/saltValue.
        let agile = protection_of(
            r#"<w:documentProtection w:edit="readOnly" w:enforcement="1"
                 w:hashValue="AAAA" w:saltValue="BBBB"/>"#,
        )
        .unwrap();
        assert!(agile.has_credential);
        // No credential attributes at all.
        let bare = protection_of(r#"<w:documentProtection w:edit="readOnly" w:enforcement="1"/>"#)
            .unwrap();
        assert!(!bare.has_credential);
    }

    #[test]
    fn protection_unknown_edit_value_fails_loud() {
        let err = parse_document_protection(&archive_with_settings(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:documentProtection w:edit="lockdown" w:enforcement="1"/>
</w:settings>"#,
        ))
        .expect_err("unknown w:edit must error, not map to a catch-all");
        assert!(err.contains("lockdown"), "error must name the value: {err}");
        assert!(err.contains("w:edit"));
    }

    #[test]
    fn protection_invalid_enforcement_fails_loud() {
        let err = parse_document_protection(&archive_with_settings(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:documentProtection w:edit="forms" w:enforcement="maybe"/>
</w:settings>"#,
        ))
        .expect_err("invalid enforcement must error");
        assert!(
            err.contains("enforcement"),
            "error must name enforcement: {err}"
        );
    }
}
