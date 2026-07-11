//! `update_fields` — author the package-level **update-fields-on-open** setting
//! (`w:updateFields` in `word/settings.xml`, ISO 29500-1 §17.15.1.81).
//!
//! ## What this does (and what it deliberately does NOT)
//!
//! A DOCX field (REF / PAGEREF / TOC / SEQ / …) carries a cached *result* — the
//! last-computed text Word displayed. The authoring intent here is "make Word
//! recompute those results". The honest, Word-correct way to express that at the
//! package level is the `w:updateFields` toggle: with it set on, Word refreshes
//! every field's result the next time the document is opened.
//!
//! We do **not** recompute field results in-engine. stemma's IR models field
//! *runs* (`fldSimple` / complex `w:fldChar`+`w:instrText`) and preserves the
//! cached result text opaquely, but it has no field-evaluation engine: computing
//! a REF/PAGEREF/TOC result requires a layout pass (page numbers, bookmark
//! resolution, document order) that the IR cannot perform. Writing a *guessed*
//! result would be a silent fallback that fabricates content — exactly what
//! CLAUDE.md forbids. So we leave the cached results untouched and set the flag
//! that tells the real consumption oracle (Word) to do the recompute correctly.
//!
//! ## Why this is NOT an `EditStep`
//!
//! Like `metadata.rs` (`set_core_property`), this is a **package-level,
//! untracked** mutation, not a body-content edit:
//!
//! - it lives in `word/settings.xml`, not in `word/document.xml` / `CanonDoc`;
//! - `apply_transaction(&CanonDoc) -> CanonDoc` has no `DocxPackage` handle, so
//!   it cannot reach the settings part;
//! - `w:updateFields` carries no `w:ins`/`w:del` markup — Word treats it as a
//!   plain document setting, not a reviewable tracked change. accept-all and
//!   reject-all both leave it in place (it is not a redline to resolve).
//!
//! So this is a plain function over `&mut DocxPackage`, surfaced as a
//! `Document` / `EditSnapshot` method (see `runtime.rs` / `api.rs`), never as an
//! `EditStep`. The flow mirrors the metadata verb and the
//! `apply_even_and_odd_headers_to_settings` settings-synthesis precedent: parse
//! the existing settings part (fail-fast, no empty-object fallback), or
//! synthesize a minimal valid `<w:settings>` shell when the part is absent, run
//! the `crate::settings::set_update_fields` writer, reserialize, and write the
//! part back — registering the content-type override + document relationship if
//! the part was created from scratch.

use crate::docx_package::DocxPackage;
use std::io::Cursor;
use xmltree::{Element, EmitterConfig};

const SETTINGS_PATH: &str = "word/settings.xml";
const WML_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const SETTINGS_CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml";
const SETTINGS_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings";

/// Error authoring the update-fields-on-open setting. Package-level and
/// fail-loud: a malformed existing settings part or a serialization failure
/// stops the flow with context — never a best-effort default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateFieldsError {
    /// `word/settings.xml` exists but could not be parsed.
    MalformedSettings { message: String },
    /// The rewritten `word/settings.xml` could not be serialized.
    WriteFailed { message: String },
}

impl std::fmt::Display for UpdateFieldsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UpdateFieldsError::MalformedSettings { message } => {
                write!(f, "failed to parse word/settings.xml: {message}")
            }
            UpdateFieldsError::WriteFailed { message } => {
                write!(f, "failed to serialize word/settings.xml: {message}")
            }
        }
    }
}

impl std::error::Error for UpdateFieldsError {}

/// Set the document's `w:updateFields` "refresh fields on open" setting
/// (§17.15.1.81).
///
/// - `Some(true)`  → `<w:updateFields w:val="true"/>`: Word recomputes every
///   field result (REF/PAGEREF/TOC/SEQ/…) on the next open.
/// - `Some(false)` → `<w:updateFields w:val="false"/>`: explicitly off.
/// - `None`        → remove the element entirely (the document makes no
///   assertion).
///
/// If the package has no `word/settings.xml` yet, a minimal valid part is
/// synthesized (and its content-type override + document relationship
/// registered) so the setting has a home; existing settings are preserved
/// across the rewrite. A malformed existing part is a hard error, never coerced.
pub fn set_update_fields_on_open(
    package: &mut DocxPackage,
    desired: Option<bool>,
) -> Result<(), UpdateFieldsError> {
    let (mut root, part_existed) = match package.get_part(SETTINGS_PATH) {
        Some(bytes) => {
            let root = Element::parse(Cursor::new(bytes)).map_err(|e| {
                UpdateFieldsError::MalformedSettings {
                    message: e.to_string(),
                }
            })?;
            (root, true)
        }
        None => {
            let mut root = Element::new("w:settings");
            let mut ns = xmltree::Namespace::empty();
            ns.put("w", WML_NS);
            root.namespaces = Some(ns);
            (root, false)
        }
    };

    crate::settings::set_update_fields(&mut root, desired);

    let mut buf = Vec::new();
    root.write_with_config(
        &mut buf,
        EmitterConfig::new().write_document_declaration(true),
    )
    .map_err(|e| UpdateFieldsError::WriteFailed {
        message: e.to_string(),
    })?;
    package.set_part(SETTINGS_PATH, buf);

    // If we created the part from scratch, register its content-type override
    // and a document relationship so Word recognizes it (mirrors
    // `apply_even_and_odd_headers_to_settings`).
    if !part_existed {
        package
            .content_types
            .add_override("/word/settings.xml", SETTINGS_CONTENT_TYPE);
        package.document_rels.add(SETTINGS_REL_TYPE, "settings.xml");
    }

    Ok(())
}

/// Read the current `w:updateFields` state, `None` if the part is absent or the
/// element is not present. A malformed existing part is a hard error.
pub fn get_update_fields_on_open(package: &DocxPackage) -> Result<Option<bool>, UpdateFieldsError> {
    let Some(bytes) = package.get_part(SETTINGS_PATH) else {
        return Ok(None);
    };
    let root =
        Element::parse(Cursor::new(bytes)).map_err(|e| UpdateFieldsError::MalformedSettings {
            message: e.to_string(),
        })?;
    crate::settings::parse_update_fields(&root)
        .map_err(|message| UpdateFieldsError::MalformedSettings { message })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docx::{DocxArchive, DocxFile};

    /// Minimal package with the four required parts plus an optional
    /// `word/settings.xml`.
    fn package_with_settings(settings_xml: Option<&[u8]>) -> DocxPackage {
        let mut files = vec![
            DocxFile {
                name: "[Content_Types].xml".to_string(),
                data: br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#.to_vec(),
            },
            DocxFile {
                name: "_rels/.rels".to_string(),
                data: br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#.to_vec(),
            },
            DocxFile {
                name: "word/_rels/document.xml.rels".to_string(),
                data: br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#.to_vec(),
            },
            DocxFile {
                name: "word/document.xml".to_string(),
                data: br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:sectPr/></w:body></w:document>"#.to_vec(),
            },
        ];
        if let Some(s) = settings_xml {
            files.push(DocxFile {
                name: "word/settings.xml".to_string(),
                data: s.to_vec(),
            });
        }
        let archive = DocxArchive::from_parts(files);
        DocxPackage::from_archive(&archive).expect("package from archive")
    }

    #[test]
    fn set_on_when_part_absent_synthesizes_and_registers() {
        let mut pkg = package_with_settings(None);
        assert!(pkg.get_part(SETTINGS_PATH).is_none());

        set_update_fields_on_open(&mut pkg, Some(true)).expect("set updateFields on");

        // The setting reads back as on.
        assert_eq!(get_update_fields_on_open(&pkg).unwrap(), Some(true));

        // The synthesized part is registered so Word recognizes it.
        let part = std::str::from_utf8(pkg.get_part(SETTINGS_PATH).unwrap()).unwrap();
        assert!(
            part.contains("updateFields") && part.contains(r#"w:val="true""#),
            "synthesized settings must carry updateFields w:val=true, got: {part}"
        );
    }

    #[test]
    fn set_on_preserves_existing_settings() {
        let settings = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:defaultTabStop w:val="708"/></w:settings>"#;
        let mut pkg = package_with_settings(Some(settings));

        set_update_fields_on_open(&mut pkg, Some(true)).expect("set updateFields on");

        assert_eq!(get_update_fields_on_open(&pkg).unwrap(), Some(true));
        let part = std::str::from_utf8(pkg.get_part(SETTINGS_PATH).unwrap()).unwrap();
        assert!(
            part.contains("defaultTabStop"),
            "existing defaultTabStop must survive the updateFields write: {part}"
        );
    }

    #[test]
    fn set_none_removes_existing_element() {
        let settings = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:updateFields w:val="true"/></w:settings>"#;
        let mut pkg = package_with_settings(Some(settings));
        assert_eq!(get_update_fields_on_open(&pkg).unwrap(), Some(true));

        set_update_fields_on_open(&mut pkg, None).expect("clear updateFields");

        assert_eq!(
            get_update_fields_on_open(&pkg).unwrap(),
            None,
            "None must remove the updateFields element entirely"
        );
    }

    #[test]
    fn set_false_is_distinct_from_absent() {
        let mut pkg = package_with_settings(None);
        set_update_fields_on_open(&mut pkg, Some(false)).expect("set updateFields off");
        assert_eq!(
            get_update_fields_on_open(&pkg).unwrap(),
            Some(false),
            "explicit off is distinct from absent (no silent fallback)"
        );
    }

    #[test]
    fn idempotent_no_duplicate_element() {
        let mut pkg = package_with_settings(None);
        set_update_fields_on_open(&mut pkg, Some(true)).expect("first set");
        set_update_fields_on_open(&mut pkg, Some(true)).expect("second set");
        let part = std::str::from_utf8(pkg.get_part(SETTINGS_PATH).unwrap()).unwrap();
        assert_eq!(
            part.matches("updateFields").count(),
            1,
            "re-setting must not duplicate the element: {part}"
        );
    }

    #[test]
    fn malformed_settings_is_hard_error() {
        let mut pkg = package_with_settings(Some(b"<w:settings><unclosed>"));
        let err = set_update_fields_on_open(&mut pkg, Some(true));
        assert!(matches!(
            err,
            Err(UpdateFieldsError::MalformedSettings { .. })
        ));
    }
}
