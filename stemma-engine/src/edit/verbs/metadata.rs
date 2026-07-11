//! `set_core_property` / `set_custom_property` — author **package-level**
//! document metadata (`docProps/core.xml` §15.2.12.1, `docProps/custom.xml`
//! §15.2.12.2).
//!
//! ## Why this is NOT an `EditStep`
//!
//! Every authoring verb in `edit/verbs/*` is a body-content edit: it takes
//! `&mut CanonDoc` inside `apply_transaction(&CanonDoc) -> CanonDoc` and
//! produces tracked changes that reject-all/accept-all resolve. Document
//! metadata has none of that shape:
//!
//! - it lives in its own OPC part, not in `word/document.xml` / `CanonDoc`;
//! - `apply_transaction` has no `DocxPackage` handle, so it *cannot* reach the
//!   part even if we wanted to;
//! - it is **untracked** — core/custom properties carry no `w:ins`/`w:del`
//!   markup; Word treats a property edit as a direct package mutation, not a
//!   reviewable change.
//!
//! So these are plain functions over a `&mut DocxPackage`, surfaced as
//! `Document`/runtime methods (see `runtime.rs`), never as `EditStep`s. The
//! flow is the verb recipe applied at the package edge: read the raw part,
//! parse it via `crate::docprops` (fail-fast, no empty-object fallback), set
//! the one named typed field, reserialize, write it back with
//! `DocxPackage::set_part`.
//!
//! ## app.xml is out of scope here
//!
//! `docProps/app.xml` carries Word-recalculated statistics (Pages, Words,
//! TotalTime, Lines…) alongside a few user-authored fields (Company, Manager).
//! Rewriting the recalculated fields would fight Word's own bookkeeping, so
//! this verb does **not** touch app.xml at all. A future `set_app_property`
//! limited to Company/Manager is the honest extension point.

use crate::docprops::{
    CORE_PROPS_PATH, CUSTOM_PROPS_PATH, CoreField, CoreProperties, CustomProperties, DocPropsError,
};
use crate::docx_package::DocxPackage;

/// A minimal, valid empty `docProps/core.xml`, used when the package carries no
/// core-properties part yet. We synthesize the standard namespace shell so the
/// freshly-set field round-trips; we do NOT invent any field values.
const EMPTY_CORE_XML: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:dcmitype="http://purl.org/dc/dcmitype/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"></cp:coreProperties>"#;

/// A minimal, valid empty `docProps/custom.xml`.
const EMPTY_CUSTOM_XML: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/custom-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes"></Properties>"#;

/// Set a single core property (`title`, `creator`/`author`, `subject`,
/// `description`, `keywords`, `lastModifiedBy`, `category`, `created`,
/// `modified`).
///
/// `field` is the public string name; an unknown name is rejected
/// ([`DocPropsError::UnknownCoreField`]) — never coerced. If the package has no
/// `docProps/core.xml` yet, a minimal valid part is synthesized so the new
/// field can be written; existing fields are preserved across the rewrite.
pub fn set_core_property(
    package: &mut DocxPackage,
    field: &str,
    value: &str,
) -> Result<(), DocPropsError> {
    let core_field = CoreField::parse(field)?;
    let raw = package.get_part(CORE_PROPS_PATH).unwrap_or(EMPTY_CORE_XML);
    let mut props = CoreProperties::parse(raw)?;
    props.set(core_field, value.to_string());
    let bytes = props.serialize()?;
    package.set_part(CORE_PROPS_PATH, bytes);
    Ok(())
}

/// Read a single core property, returning `None` if absent. Convenience for
/// callers that want to inspect without re-parsing the part themselves.
pub fn get_core_property(
    package: &DocxPackage,
    field: &str,
) -> Result<Option<String>, DocPropsError> {
    let core_field = CoreField::parse(field)?;
    let Some(raw) = package.get_part(CORE_PROPS_PATH) else {
        return Ok(None);
    };
    let props = CoreProperties::parse(raw)?;
    Ok(props.get(core_field).map(|s| s.to_string()))
}

/// Set a single user-defined custom property to a string (`vt:lpwstr`) value.
/// Inserting a new name appends; an existing name is replaced in place. If the
/// package has no `docProps/custom.xml` yet, a minimal valid part is
/// synthesized.
pub fn set_custom_property(
    package: &mut DocxPackage,
    name: &str,
    value: &str,
) -> Result<(), DocPropsError> {
    let raw = package
        .get_part(CUSTOM_PROPS_PATH)
        .unwrap_or(EMPTY_CUSTOM_XML);
    let mut props = CustomProperties::parse(raw)?;
    props.set_string(name, value.to_string());
    let bytes = props.serialize()?;
    package.set_part(CUSTOM_PROPS_PATH, bytes);
    Ok(())
}

/// Read a single custom property's string value, `None` if absent.
pub fn get_custom_property(
    package: &DocxPackage,
    name: &str,
) -> Result<Option<String>, DocPropsError> {
    let Some(raw) = package.get_part(CUSTOM_PROPS_PATH) else {
        return Ok(None);
    };
    let props = CustomProperties::parse(raw)?;
    Ok(props.get(name).map(|s| s.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docx::{DocxArchive, DocxFile};

    /// Build a minimal `DocxPackage` carrying the three required metadata parts
    /// plus an optional `docProps/core.xml`. Enough for the package round-trip
    /// the metadata verb needs.
    fn package_with_core(core_xml: Option<&[u8]>) -> DocxPackage {
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
        if let Some(core) = core_xml {
            files.push(DocxFile {
                name: "docProps/core.xml".to_string(),
                data: core.to_vec(),
            });
        }
        let archive = DocxArchive::from_parts(files);
        DocxPackage::from_archive(&archive).expect("package from archive")
    }

    #[test]
    fn set_core_title_when_part_absent_synthesizes() {
        let mut pkg = package_with_core(None);
        assert!(pkg.get_part(CORE_PROPS_PATH).is_none());
        set_core_property(&mut pkg, "title", "Synthesized").expect("set title");
        assert_eq!(
            get_core_property(&pkg, "title").unwrap().as_deref(),
            Some("Synthesized")
        );
    }

    #[test]
    fn set_core_preserves_existing_fields() {
        let core = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><dc:creator>Alice</dc:creator></cp:coreProperties>"#;
        let mut pkg = package_with_core(Some(core));
        set_core_property(&mut pkg, "title", "New").expect("set title");
        assert_eq!(
            get_core_property(&pkg, "title").unwrap().as_deref(),
            Some("New")
        );
        assert_eq!(
            get_core_property(&pkg, "creator").unwrap().as_deref(),
            Some("Alice"),
            "existing creator must survive the title write"
        );
    }

    #[test]
    fn unknown_core_field_rejected_no_part_written() {
        let mut pkg = package_with_core(None);
        let err = set_core_property(&mut pkg, "totalTime", "5");
        assert!(matches!(err, Err(DocPropsError::UnknownCoreField { .. })));
        assert!(
            pkg.get_part(CORE_PROPS_PATH).is_none(),
            "a rejected unknown field must not write a part"
        );
    }

    #[test]
    fn set_custom_property_round_trips() {
        let mut pkg = package_with_core(None);
        set_custom_property(&mut pkg, "MatterNumber", "M-42").expect("set custom");
        assert_eq!(
            get_custom_property(&pkg, "MatterNumber")
                .unwrap()
                .as_deref(),
            Some("M-42")
        );
    }
}
