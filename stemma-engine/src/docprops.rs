//! Package-level document metadata: `docProps/core.xml` (§15.2.12.1, the
//! Dublin Core / Open Packaging core properties) and `docProps/custom.xml`
//! (§15.2.12.2, user-defined custom properties).
//!
//! This is a **package-level** concern, not a body-content edit: it lives in
//! its own OPC part, has no tracked-change semantics, and is reached through
//! the `DocxPackage` part store — never through `CanonDoc`/`apply_transaction`,
//! which only sees the body IR. The `edit/verbs/metadata.rs` verb owns the
//! "read the part, mutate one typed field, write the part back" flow; this
//! module owns the typed model + the parse/serialize at the part edge.
//!
//! Design (CLAUDE.md "parse at the edges, fail fast"):
//! - `parse(&[u8])` turns raw XML into a typed [`CoreProperties`] /
//!   [`CustomProperties`]. Malformed XML is a hard [`DocPropsError`], never an
//!   empty-object fallback.
//! - `serialize(&self)` emits a well-formed part. For core properties only the
//!   modeled fields are emitted; an absent (`None`) field is simply not written
//!   (it is genuinely absent in the OPC model, not defaulted).
//! - We model only the standard Dublin Core / `cp:` fields a user authors. We
//!   do **not** model `app.xml`'s Word-recalculated statistics (Pages,
//!   TotalTime, Words…); that part is left untouched by the metadata verb.

use xmltree::{AttributeName, Element, Namespace, XMLNode};

/// Archive path of the OPC core-properties part.
pub const CORE_PROPS_PATH: &str = "docProps/core.xml";
/// Archive path of the custom-properties part.
pub const CUSTOM_PROPS_PATH: &str = "docProps/custom.xml";

// Namespace URIs used by the two parts (§15.2.12).
const NS_CP: &str = "http://schemas.openxmlformats.org/package/2006/metadata/core-properties";
const NS_DC: &str = "http://purl.org/dc/elements/1.1/";
const NS_DCTERMS: &str = "http://purl.org/dc/terms/";
const NS_DCMITYPE: &str = "http://purl.org/dc/dcmitype/";
const NS_XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";
const NS_VT: &str = "http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes";
const NS_CUSTOM: &str = "http://schemas.openxmlformats.org/officeDocument/2006/custom-properties";

/// The format-class (`xsi:type`) Word stamps on the two W3CDTF date fields.
const DCTERMS_W3CDTF: &str = "dcterms:W3CDTF";

/// A failure to parse or serialize a document-properties part. Carries the
/// part path and a message so the caller can see *which* part and *why*
/// (CLAUDE.md "errors should be actionable"). Never a silent fallback.
#[derive(Debug)]
pub enum DocPropsError {
    /// The part's XML could not be parsed.
    MalformedXml { part: String, message: String },
    /// The part's XML could not be re-serialized.
    WriteFailed { part: String, message: String },
    /// A custom property element was structurally invalid (e.g. missing the
    /// required `name` attribute). We refuse rather than skip it.
    MalformedCustomProperty { message: String },
    /// The caller named a core-property field that this model does not know.
    /// No silent map-to-Other; the unknown key is surfaced verbatim.
    UnknownCoreField { field: String },
}

impl std::fmt::Display for DocPropsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DocPropsError::MalformedXml { part, message } => {
                write!(f, "malformed XML in {part}: {message}")
            }
            DocPropsError::WriteFailed { part, message } => {
                write!(f, "failed to serialize {part}: {message}")
            }
            DocPropsError::MalformedCustomProperty { message } => {
                write!(f, "malformed custom property: {message}")
            }
            DocPropsError::UnknownCoreField { field } => {
                write!(f, "unknown core-property field '{field}'")
            }
        }
    }
}

impl std::error::Error for DocPropsError {}

/// The named, user-authored fields of a `set_core_property` request. A typed
/// enum (not a free string key) so an unknown field is impossible to express —
/// the wire/caller edge maps a string to this and fails loud on a miss
/// (CLAUDE.md "make invalid states hard").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreField {
    /// `dc:title`
    Title,
    /// `dc:creator` (the author)
    Creator,
    /// `dc:subject`
    Subject,
    /// `dc:description`
    Description,
    /// `cp:keywords`
    Keywords,
    /// `cp:lastModifiedBy`
    LastModifiedBy,
    /// `cp:category`
    Category,
    /// `dcterms:created` (W3CDTF timestamp)
    Created,
    /// `dcterms:modified` (W3CDTF timestamp)
    Modified,
}

impl CoreField {
    /// Parse the public string name of a core field. Fails loud on an unknown
    /// key — no silent fallback to "Other".
    pub fn parse(name: &str) -> Result<CoreField, DocPropsError> {
        match name {
            "title" => Ok(CoreField::Title),
            "creator" | "author" => Ok(CoreField::Creator),
            "subject" => Ok(CoreField::Subject),
            "description" => Ok(CoreField::Description),
            "keywords" => Ok(CoreField::Keywords),
            "lastModifiedBy" | "last_modified_by" => Ok(CoreField::LastModifiedBy),
            "category" => Ok(CoreField::Category),
            "created" => Ok(CoreField::Created),
            "modified" => Ok(CoreField::Modified),
            other => Err(DocPropsError::UnknownCoreField {
                field: other.to_string(),
            }),
        }
    }
}

/// Typed `docProps/core.xml` — the OPC core properties (§15.2.12.1). Every
/// field is optional: `None` means the element is genuinely absent (the part
/// did not carry it), not a default value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CoreProperties {
    pub title: Option<String>,
    pub creator: Option<String>,
    pub subject: Option<String>,
    pub description: Option<String>,
    pub keywords: Option<String>,
    pub last_modified_by: Option<String>,
    pub category: Option<String>,
    /// `dcterms:created`, a W3CDTF timestamp.
    pub created: Option<String>,
    /// `dcterms:modified`, a W3CDTF timestamp.
    pub modified: Option<String>,
    /// Standard core-property elements present in the part that this type does
    /// not model explicitly (e.g. `cp:contentStatus`, `cp:revision`,
    /// `cp:version`, `cp:lastPrinted`, `dc:language`, `dc:identifier`). They are
    /// carried through verbatim so a targeted `set_core_property` edit does not
    /// silently drop spec-valid metadata (ECMA-376 §8.3.4; "no silent fallbacks").
    pub unmodeled: Vec<UnmodeledCoreProp>,
}

/// A core-property element preserved opaquely across a property edit. Captures
/// the qualified name (prefix + namespace + local) and text content. These
/// standard fields are text-only; element attributes are not part of the
/// captured form (the modeled `created`/`modified` are the only attributed core
/// elements, and they are not unmodeled).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UnmodeledCoreProp {
    pub prefix: Option<String>,
    pub namespace: Option<String>,
    pub local: String,
    pub text: Option<String>,
}

impl CoreProperties {
    /// Parse `docProps/core.xml`. Malformed XML is a hard error (no
    /// empty-object fallback).
    pub fn parse(bytes: &[u8]) -> Result<CoreProperties, DocPropsError> {
        let root = parse_xml(bytes, CORE_PROPS_PATH)?;
        let mut props = CoreProperties::default();
        for child in &root.children {
            let XMLNode::Element(el) = child else {
                continue;
            };
            let text = element_text(el);
            // Match on (prefix-stripped) local name; the surrounding namespace
            // is fixed by the schema, so the local name disambiguates.
            match local_name(&el.name) {
                "title" => props.title = text,
                "creator" => props.creator = text,
                "subject" => props.subject = text,
                "description" => props.description = text,
                "keywords" => props.keywords = text,
                "lastModifiedBy" => props.last_modified_by = text,
                "category" => props.category = text,
                "created" => props.created = text,
                "modified" => props.modified = text,
                _ => {
                    // Unmodeled standard fields (contentStatus, revision,
                    // identifier, language, version, lastPrinted, …) are not
                    // authored by this verb, but a targeted `set_core_property`
                    // rewrites the part from the model — so we MUST carry them
                    // through, else the edit silently drops spec-valid metadata
                    // (ECMA-376 §8.3.4; "no silent fallbacks").
                    props.unmodeled.push(UnmodeledCoreProp {
                        prefix: el.prefix.clone(),
                        namespace: el.namespace.clone(),
                        local: local_name(&el.name).to_string(),
                        text,
                    });
                }
            }
        }
        Ok(props)
    }

    /// Set one named field to a value, in place. The field is typed, so an
    /// unknown key cannot reach here (the wire edge maps string → `CoreField`).
    pub fn set(&mut self, field: CoreField, value: String) {
        match field {
            CoreField::Title => self.title = Some(value),
            CoreField::Creator => self.creator = Some(value),
            CoreField::Subject => self.subject = Some(value),
            CoreField::Description => self.description = Some(value),
            CoreField::Keywords => self.keywords = Some(value),
            CoreField::LastModifiedBy => self.last_modified_by = Some(value),
            CoreField::Category => self.category = Some(value),
            CoreField::Created => self.created = Some(value),
            CoreField::Modified => self.modified = Some(value),
        }
    }

    /// Read one named field.
    pub fn get(&self, field: CoreField) -> Option<&str> {
        match field {
            CoreField::Title => self.title.as_deref(),
            CoreField::Creator => self.creator.as_deref(),
            CoreField::Subject => self.subject.as_deref(),
            CoreField::Description => self.description.as_deref(),
            CoreField::Keywords => self.keywords.as_deref(),
            CoreField::LastModifiedBy => self.last_modified_by.as_deref(),
            CoreField::Category => self.category.as_deref(),
            CoreField::Created => self.created.as_deref(),
            CoreField::Modified => self.modified.as_deref(),
        }
    }

    /// Serialize back to a `docProps/core.xml` part. Only the present
    /// (`Some`) fields are emitted; an absent field is left out.
    pub fn serialize(&self) -> Result<Vec<u8>, DocPropsError> {
        // Root <cp:coreProperties> with the full standard namespace set, so
        // the qualified children below resolve and Word reads the part.
        let mut root = Element::new("coreProperties");
        root.prefix = Some("cp".to_string());
        root.namespace = Some(NS_CP.to_string());
        let mut ns = Namespace::empty();
        ns.put("cp", NS_CP);
        ns.put("dc", NS_DC);
        ns.put("dcterms", NS_DCTERMS);
        ns.put("dcmitype", NS_DCMITYPE);
        ns.put("xsi", NS_XSI);
        root.namespaces = Some(ns);

        push_text_child(&mut root, "dc", NS_DC, "title", &self.title);
        push_text_child(&mut root, "dc", NS_DC, "creator", &self.creator);
        push_text_child(&mut root, "dc", NS_DC, "subject", &self.subject);
        push_text_child(&mut root, "dc", NS_DC, "description", &self.description);
        push_text_child(&mut root, "cp", NS_CP, "keywords", &self.keywords);
        push_text_child(
            &mut root,
            "cp",
            NS_CP,
            "lastModifiedBy",
            &self.last_modified_by,
        );
        push_text_child(&mut root, "cp", NS_CP, "category", &self.category);
        push_dcterms_date(&mut root, "created", &self.created);
        push_dcterms_date(&mut root, "modified", &self.modified);

        // Re-emit unmodeled standard fields verbatim so a targeted edit is
        // lossless (ECMA-376 §8.3.4).
        for prop in &self.unmodeled {
            let mut el = Element::new(&prop.local);
            el.prefix = prop.prefix.clone();
            el.namespace = prop.namespace.clone();
            if let Some(text) = &prop.text {
                el.children.push(XMLNode::Text(text.clone()));
            }
            root.children.push(XMLNode::Element(el));
        }

        write_xml(&root, CORE_PROPS_PATH)
    }
}

/// One user-defined custom property (§15.2.12.2). v1 models the common
/// scalar value classes Word emits as `<vt:*>` children. The `value` keeps the
/// raw string; `kind` records which `vt:` element wrapped it so a round-trip
/// re-emits the same class (no silent coercion to string).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomProperty {
    /// The property name (the `name` attribute).
    pub name: String,
    /// The string form of the value.
    pub value: String,
    /// The `vt:` value class (e.g. `lpwstr`, `i4`, `bool`).
    pub kind: CustomValueKind,
}

/// The `vt:` value class of a custom property. An unknown class is preserved
/// verbatim rather than mapped to a default — we never guess a type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CustomValueKind {
    /// `vt:lpwstr` — a Unicode string.
    Lpwstr,
    /// `vt:i4` — a 32-bit signed integer.
    I4,
    /// `vt:bool` — a boolean.
    Bool,
    /// `vt:filetime` — a timestamp.
    Filetime,
    /// Any other `vt:` element local name, kept verbatim.
    Other(String),
}

impl CustomValueKind {
    fn from_vt_local(local: &str) -> CustomValueKind {
        match local {
            "lpwstr" => CustomValueKind::Lpwstr,
            "i4" => CustomValueKind::I4,
            "bool" => CustomValueKind::Bool,
            "filetime" => CustomValueKind::Filetime,
            other => CustomValueKind::Other(other.to_string()),
        }
    }
    fn vt_local(&self) -> &str {
        match self {
            CustomValueKind::Lpwstr => "lpwstr",
            CustomValueKind::I4 => "i4",
            CustomValueKind::Bool => "bool",
            CustomValueKind::Filetime => "filetime",
            CustomValueKind::Other(s) => s.as_str(),
        }
    }
}

/// Typed `docProps/custom.xml` — an ordered list of user-defined properties.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CustomProperties {
    pub properties: Vec<CustomProperty>,
}

impl CustomProperties {
    /// Parse `docProps/custom.xml`. Malformed XML or a structurally invalid
    /// property element is a hard error.
    pub fn parse(bytes: &[u8]) -> Result<CustomProperties, DocPropsError> {
        let root = parse_xml(bytes, CUSTOM_PROPS_PATH)?;
        let mut properties = Vec::new();
        for child in &root.children {
            let XMLNode::Element(el) = child else {
                continue;
            };
            if local_name(&el.name) != "property" {
                continue;
            }
            let name = attr(el, "name").ok_or_else(|| DocPropsError::MalformedCustomProperty {
                message: "<property> missing required `name` attribute".to_string(),
            })?;
            // The first child element is the typed <vt:*> value.
            let value_el = el.children.iter().find_map(|c| match c {
                XMLNode::Element(e) => Some(e),
                _ => None,
            });
            let value_el = value_el.ok_or_else(|| DocPropsError::MalformedCustomProperty {
                message: format!("custom property '{name}' has no <vt:*> value element"),
            })?;
            let kind = CustomValueKind::from_vt_local(local_name(&value_el.name));
            let value = element_text(value_el).unwrap_or_default();
            properties.push(CustomProperty {
                name: name.to_string(),
                value,
                kind,
            });
        }
        Ok(CustomProperties { properties })
    }

    /// Set (insert or replace) a property by name with an `lpwstr` value — the
    /// class Word uses for user-entered string properties. Replacing keeps the
    /// existing position; inserting appends.
    pub fn set_string(&mut self, name: &str, value: String) {
        if let Some(existing) = self.properties.iter_mut().find(|p| p.name == name) {
            existing.value = value;
            existing.kind = CustomValueKind::Lpwstr;
        } else {
            self.properties.push(CustomProperty {
                name: name.to_string(),
                value,
                kind: CustomValueKind::Lpwstr,
            });
        }
    }

    /// Read a property's string value by name.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.properties
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.value.as_str())
    }

    /// Serialize back to a `docProps/custom.xml` part. Custom properties carry
    /// a 1-based `pid` starting at 2 (pid 0/1 are reserved per §15.2.12.2) and
    /// a fixed `fmtid`.
    pub fn serialize(&self) -> Result<Vec<u8>, DocPropsError> {
        const FMTID: &str = "{D5CDD505-2E9C-101B-9397-08002B2CF9AE}";
        let mut root = Element::new("Properties");
        root.namespace = Some(NS_CUSTOM.to_string());
        let mut ns = Namespace::empty();
        ns.put("", NS_CUSTOM);
        ns.put("vt", NS_VT);
        root.namespaces = Some(ns);

        for (i, prop) in self.properties.iter().enumerate() {
            let pid = i as u32 + 2;
            let mut prop_el = Element::new("property");
            prop_el.namespace = Some(NS_CUSTOM.to_string());
            prop_el
                .attributes
                .insert(AttributeName::local("fmtid"), FMTID.to_string());
            prop_el
                .attributes
                .insert(AttributeName::local("pid"), pid.to_string());
            prop_el
                .attributes
                .insert(AttributeName::local("name"), prop.name.clone());

            let mut value_el = Element::new(prop.kind.vt_local());
            value_el.prefix = Some("vt".to_string());
            value_el.namespace = Some(NS_VT.to_string());
            value_el.children.push(XMLNode::Text(prop.value.clone()));
            prop_el.children.push(XMLNode::Element(value_el));
            root.children.push(XMLNode::Element(prop_el));
        }

        write_xml(&root, CUSTOM_PROPS_PATH)
    }
}

// ─── XML helpers (local to this module) ───────────────────────────────────────

fn parse_xml(bytes: &[u8], part: &str) -> Result<Element, DocPropsError> {
    crate::word_xml::parse_document_xml(bytes).map_err(|e| DocPropsError::MalformedXml {
        part: part.to_string(),
        message: format!("{e:?}"),
    })
}

fn write_xml(element: &Element, part: &str) -> Result<Vec<u8>, DocPropsError> {
    let mut out = Vec::new();
    element
        .write(&mut out)
        .map_err(|e| DocPropsError::WriteFailed {
            part: part.to_string(),
            message: e.to_string(),
        })?;
    Ok(out)
}

fn local_name(name: &str) -> &str {
    match name.find(':') {
        Some(pos) => &name[pos + 1..],
        None => name,
    }
}

fn attr<'a>(el: &'a Element, local: &str) -> Option<&'a str> {
    el.attributes
        .iter()
        .find(|(k, _)| k.local_name == local)
        .map(|(_, v)| v.as_str())
}

/// Concatenated text content of an element, or `None` if it has no text.
fn element_text(el: &Element) -> Option<String> {
    let mut s = String::new();
    for child in &el.children {
        if let XMLNode::Text(t) = child {
            s.push_str(t);
        }
    }
    if s.is_empty() { None } else { Some(s) }
}

/// Append `<prefix:local>value</prefix:local>` to `root`, but only when
/// `value` is `Some` — an absent field is not emitted at all.
fn push_text_child(
    root: &mut Element,
    prefix: &str,
    ns: &str,
    local: &str,
    value: &Option<String>,
) {
    let Some(text) = value else {
        return;
    };
    let mut el = Element::new(local);
    el.prefix = Some(prefix.to_string());
    el.namespace = Some(ns.to_string());
    el.children.push(XMLNode::Text(text.clone()));
    root.children.push(XMLNode::Element(el));
}

/// Append a `dcterms:*` date child with the required `xsi:type="dcterms:W3CDTF"`
/// attribute Word stamps on `created` / `modified`.
fn push_dcterms_date(root: &mut Element, local: &str, value: &Option<String>) {
    let Some(text) = value else {
        return;
    };
    let mut el = Element::new(local);
    el.prefix = Some("dcterms".to_string());
    el.namespace = Some(NS_DCTERMS.to_string());
    el.attributes.insert(
        AttributeName {
            local_name: "type".to_string(),
            namespace: Some(NS_XSI.to_string()),
            prefix: Some("xsi".to_string()),
        },
        DCTERMS_W3CDTF.to_string(),
    );
    el.children.push(XMLNode::Text(text.clone()));
    root.children.push(XMLNode::Element(el));
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_CORE: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:dcmitype="http://purl.org/dc/dcmitype/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><dc:title>Original Title</dc:title><dc:creator>Alice</dc:creator><cp:lastModifiedBy>Bob</cp:lastModifiedBy><dcterms:created xsi:type="dcterms:W3CDTF">2024-01-01T00:00:00Z</dcterms:created></cp:coreProperties>"#;

    const SAMPLE_CUSTOM: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/custom-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes"><property fmtid="{D5CDD505-2E9C-101B-9397-08002B2CF9AE}" pid="2" name="MatterNumber"><vt:lpwstr>M-1234</vt:lpwstr></property></Properties>"#;

    #[test]
    fn parses_core_fields() {
        let p = CoreProperties::parse(SAMPLE_CORE).expect("parse");
        assert_eq!(p.title.as_deref(), Some("Original Title"));
        assert_eq!(p.creator.as_deref(), Some("Alice"));
        assert_eq!(p.last_modified_by.as_deref(), Some("Bob"));
        assert_eq!(p.created.as_deref(), Some("2024-01-01T00:00:00Z"));
        // Absent fields stay None — no fallback default.
        assert_eq!(p.subject, None);
    }

    #[test]
    fn core_parse_serialize_parse_identity() {
        let p = CoreProperties::parse(SAMPLE_CORE).expect("parse");
        let bytes = p.serialize().expect("serialize");
        let p2 = CoreProperties::parse(&bytes).expect("reparse");
        assert_eq!(p, p2, "parse->serialize->parse must be identity");
    }

    #[test]
    fn set_then_get_core_field() {
        let mut p = CoreProperties::parse(SAMPLE_CORE).expect("parse");
        p.set(CoreField::Title, "New Title".to_string());
        assert_eq!(p.get(CoreField::Title), Some("New Title"));
        let bytes = p.serialize().expect("serialize");
        let p2 = CoreProperties::parse(&bytes).expect("reparse");
        assert_eq!(p2.title.as_deref(), Some("New Title"));
        // The other parsed fields survived the rewrite.
        assert_eq!(p2.creator.as_deref(), Some("Alice"));
    }

    #[test]
    fn absent_field_not_emitted() {
        let p = CoreProperties {
            title: Some("Only Title".to_string()),
            ..Default::default()
        };
        let bytes = p.serialize().expect("serialize");
        let xml = String::from_utf8(bytes).unwrap();
        assert!(xml.contains("Only Title"));
        assert!(
            !xml.contains("creator"),
            "absent creator must not be emitted"
        );
        assert!(
            !xml.contains("subject"),
            "absent subject must not be emitted"
        );
    }

    #[test]
    fn parses_custom_property() {
        let c = CustomProperties::parse(SAMPLE_CUSTOM).expect("parse");
        assert_eq!(c.properties.len(), 1);
        assert_eq!(c.get("MatterNumber"), Some("M-1234"));
        assert_eq!(c.properties[0].kind, CustomValueKind::Lpwstr);
    }

    #[test]
    fn custom_parse_serialize_parse_identity() {
        let c = CustomProperties::parse(SAMPLE_CUSTOM).expect("parse");
        let bytes = c.serialize().expect("serialize");
        let c2 = CustomProperties::parse(&bytes).expect("reparse");
        assert_eq!(c, c2, "parse->serialize->parse must be identity");
    }

    #[test]
    fn set_string_inserts_and_replaces() {
        let mut c = CustomProperties::parse(SAMPLE_CUSTOM).expect("parse");
        c.set_string("MatterNumber", "M-9999".to_string()); // replace
        c.set_string("Reviewer", "Carol".to_string()); // insert
        assert_eq!(c.get("MatterNumber"), Some("M-9999"));
        assert_eq!(c.get("Reviewer"), Some("Carol"));
        assert_eq!(c.properties.len(), 2);
        // pid is reassigned 1-based-from-2 on serialize.
        let bytes = c.serialize().expect("serialize");
        let xml = String::from_utf8(bytes).unwrap();
        assert!(xml.contains(r#"pid="2""#));
        assert!(xml.contains(r#"pid="3""#));
    }

    #[test]
    fn malformed_xml_is_error_not_fallback() {
        let bad = b"<cp:coreProperties><dc:title>unclosed";
        let err = CoreProperties::parse(bad);
        assert!(
            matches!(err, Err(DocPropsError::MalformedXml { .. })),
            "malformed XML must be a hard error, got {err:?}"
        );
    }

    #[test]
    fn unknown_core_field_name_rejected() {
        let err = CoreField::parse("totalTime");
        assert!(matches!(err, Err(DocPropsError::UnknownCoreField { .. })));
        // Known aliases resolve.
        assert_eq!(CoreField::parse("author").unwrap(), CoreField::Creator);
    }

    #[test]
    fn custom_property_missing_name_is_error() {
        let bad = br#"<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/custom-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes"><property fmtid="x" pid="2"><vt:lpwstr>v</vt:lpwstr></property></Properties>"#;
        let err = CustomProperties::parse(bad);
        assert!(matches!(
            err,
            Err(DocPropsError::MalformedCustomProperty { .. })
        ));
    }
}
