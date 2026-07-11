use std::collections::HashMap;

use xmltree::{Element, Namespace, XMLNode};

use crate::docx::DocxArchive;

// =============================================================================
// Error types
// =============================================================================

#[derive(Debug)]
pub enum PackageError {
    /// A required part is missing from the archive.
    MissingPart(String),
    /// XML parsing failed for a package metadata file.
    XmlParse { part: String, reason: String },
    /// XML serialization failed for a package metadata file.
    XmlWrite {
        part: String,
        source: xmltree::Error,
    },
    /// A Relationship element is missing a required attribute.
    MalformedRelationship { part: String, message: String },
    /// The package root-relationships part (`_rels/.rels`) is present but
    /// declares no `officeDocument` relationship. ECMA-376 Part 2 (OPC §9.3)
    /// locates the main document part *only* through this relationship, so a
    /// package without it has no discoverable main part.
    MissingOfficeDocumentRelationship,
    /// The `officeDocument` relationship targets an External part. The main
    /// document part must live inside the package.
    ExternalMainDocument { target: String },
    /// The `officeDocument` relationship resolves to a part name that is not
    /// present in the package.
    MainDocumentPartAbsent { rel_id: String, part: String },
}

impl std::fmt::Display for PackageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackageError::MissingPart(p) => write!(f, "missing package part: {p}"),
            PackageError::XmlParse { part, reason } => {
                write!(f, "failed to parse XML in {part}: {reason}")
            }
            PackageError::XmlWrite { part, source } => {
                write!(f, "failed to write XML for {part}: {source}")
            }
            PackageError::MalformedRelationship { part, message } => {
                write!(f, "malformed Relationship in {part}: {message}")
            }
            PackageError::MissingOfficeDocumentRelationship => write!(
                f,
                "package root relationships part {ROOT_RELS_PATH} has no officeDocument \
                 relationship (type {OFFICE_DOCUMENT_REL_TYPE}); ECMA-376 Part 2 §9.3 locates \
                 the main document part through this relationship"
            ),
            PackageError::ExternalMainDocument { target } => write!(
                f,
                "the officeDocument relationship targets an External part {target:?}; the main \
                 document part must be internal to the package"
            ),
            PackageError::MainDocumentPartAbsent { rel_id, part } => write!(
                f,
                "the officeDocument relationship {rel_id} resolves to {part:?}, which is not \
                 present in the package"
            ),
        }
    }
}

// =============================================================================
// ContentTypes — [Content_Types].xml
// =============================================================================

/// A `<Default Extension="..." ContentType="..."/>` entry.
#[derive(Clone, Debug, PartialEq)]
pub struct DefaultContentType {
    pub extension: String,
    pub content_type: String,
}

/// An `<Override PartName="..." ContentType="..."/>` entry.
#[derive(Clone, Debug, PartialEq)]
pub struct OverrideContentType {
    pub part_name: String,
    pub content_type: String,
}

const CONTENT_TYPES_NS: &str = "http://schemas.openxmlformats.org/package/2006/content-types";

/// Parsed `[Content_Types].xml`.
#[derive(Clone, Debug)]
pub struct ContentTypes {
    pub defaults: Vec<DefaultContentType>,
    pub overrides: Vec<OverrideContentType>,
}

impl ContentTypes {
    /// Parse from the raw XML bytes of `[Content_Types].xml`.
    pub fn parse(bytes: &[u8]) -> Result<Self, PackageError> {
        let root = parse_xml(bytes, "[Content_Types].xml")?;

        let mut defaults = Vec::new();
        let mut overrides = Vec::new();

        for child in &root.children {
            let el = match child {
                XMLNode::Element(el) => el,
                _ => continue,
            };
            match local_name(el) {
                "Default" => {
                    let extension = attr(el, "Extension").ok_or_else(|| {
                        PackageError::MalformedRelationship {
                            part: "[Content_Types].xml".to_string(),
                            message: "Default element missing Extension attribute".to_string(),
                        }
                    })?;
                    let content_type = attr(el, "ContentType").ok_or_else(|| {
                        PackageError::MalformedRelationship {
                            part: "[Content_Types].xml".to_string(),
                            message: "Default element missing ContentType attribute".to_string(),
                        }
                    })?;
                    defaults.push(DefaultContentType {
                        extension: extension.to_string(),
                        content_type: content_type.to_string(),
                    });
                }
                "Override" => {
                    let part_name = attr(el, "PartName").ok_or_else(|| {
                        PackageError::MalformedRelationship {
                            part: "[Content_Types].xml".to_string(),
                            message: "Override element missing PartName attribute".to_string(),
                        }
                    })?;
                    let content_type = attr(el, "ContentType").ok_or_else(|| {
                        PackageError::MalformedRelationship {
                            part: "[Content_Types].xml".to_string(),
                            message: "Override element missing ContentType attribute".to_string(),
                        }
                    })?;
                    overrides.push(OverrideContentType {
                        part_name: part_name.to_string(),
                        content_type: content_type.to_string(),
                    });
                }
                _ => {}
            }
        }

        Ok(ContentTypes {
            defaults,
            overrides,
        })
    }

    /// Serialize back to XML bytes.
    pub fn serialize(&self) -> Result<Vec<u8>, PackageError> {
        let mut root = Element::new("Types");
        let mut ns = Namespace::empty();
        ns.put("", CONTENT_TYPES_NS);
        root.namespaces = Some(ns);
        root.namespace = Some(CONTENT_TYPES_NS.to_string());

        for d in &self.defaults {
            let mut el = Element::new("Default");
            el.attributes
                .insert(attr_name("Extension"), d.extension.clone());
            el.attributes
                .insert(attr_name("ContentType"), d.content_type.clone());
            root.children.push(XMLNode::Element(el));
        }

        for o in &self.overrides {
            let mut el = Element::new("Override");
            el.attributes
                .insert(attr_name("PartName"), o.part_name.clone());
            el.attributes
                .insert(attr_name("ContentType"), o.content_type.clone());
            root.children.push(XMLNode::Element(el));
        }

        write_xml(&root, "[Content_Types].xml")
    }

    /// Add an override entry if one for the given part name doesn't already exist.
    pub fn add_override(&mut self, part_name: &str, content_type: &str) {
        if !self.has_override(part_name) {
            self.overrides.push(OverrideContentType {
                part_name: part_name.to_string(),
                content_type: content_type.to_string(),
            });
        }
    }

    /// Check whether an override for the given part name exists.
    /// Override PartName comparison is ASCII case-insensitive (OPC §7.2).
    pub fn has_override(&self, part_name: &str) -> bool {
        self.overrides
            .iter()
            .any(|o| o.part_name.eq_ignore_ascii_case(part_name))
    }

    /// Check whether a default for the given extension exists.
    pub fn has_default(&self, extension: &str) -> bool {
        self.defaults
            .iter()
            .any(|d| d.extension.eq_ignore_ascii_case(extension))
    }

    /// For every recognized WordprocessingML part in `part_paths` that has **no**
    /// Override, add its canonical content-type Override (OPC §10.1.2 / ECMA-376
    /// Part 1 §15.2). Never rewrites an existing Override — an explicit author
    /// choice is left untouched. Idempotent.
    ///
    /// This is the shared core of the save-time content-type guarantee: a WML
    /// part whose content type is supplied only by `Default Extension="xml"` is
    /// not located by Word (which resolves parts by content type), so it is
    /// dropped on repair. `part_paths` are archive paths without a leading slash
    /// (e.g. `word/document.xml`).
    pub fn ensure_canonical_wml_for_parts<'a>(
        &mut self,
        part_paths: impl IntoIterator<Item = &'a str>,
    ) {
        for path in part_paths {
            let Some(content_type) = canonical_wml_content_type(path) else {
                continue;
            };
            let override_name = format!("/{path}");
            if !self.has_override(&override_name) {
                self.add_override(&override_name, content_type);
            }
        }
    }
}

// =============================================================================
// RelationshipSet — a single .rels file
// =============================================================================

const RELS_NS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";

/// A single `<Relationship>` entry from a `.rels` file.
#[derive(Clone, Debug, PartialEq)]
pub struct Relationship {
    pub id: String,
    pub rel_type: String,
    pub target: String,
    pub target_mode: Option<String>,
}

/// A parsed `.rels` file: a set of `Relationship` entries with auto-ID tracking.
#[derive(Clone, Debug)]
pub struct RelationshipSet {
    pub entries: Vec<Relationship>,
    next_id: u32,
    /// Directory the described part lives in (e.g. `word/` for
    /// `word/_rels/document.xml.rels`, `""` for the package root `_rels/.rels`).
    /// Relationship `Target`s are resolved against this base so that dedup
    /// recognizes the same part whether it is written as a package-absolute
    /// target (`/word/x.xml`) or a base-relative one (`x.xml`, `../y/z.xml`).
    base_dir: String,
}

/// The directory that a `.rels` part's relationships resolve against: the path
/// up to (and excluding) the `_rels/` segment. `word/_rels/document.xml.rels`
/// -> `word/`; the package root `_rels/.rels` -> `""`.
fn rels_base_dir(rels_path: &str) -> String {
    match rels_path.find("_rels/") {
        Some(idx) => rels_path[..idx].to_string(),
        None => String::new(),
    }
}

impl RelationshipSet {
    /// Parse from the raw XML bytes of a `.rels` file.
    pub fn parse(bytes: &[u8], part_name: &str) -> Result<Self, PackageError> {
        let root = parse_xml(bytes, part_name)?;

        let mut entries = Vec::new();
        let mut max_id: u32 = 0;

        for child in &root.children {
            let el = match child {
                XMLNode::Element(el) => el,
                _ => continue,
            };
            if local_name(el) != "Relationship" {
                continue;
            }

            let id = attr(el, "Id")
                .ok_or_else(|| PackageError::MalformedRelationship {
                    part: part_name.to_string(),
                    message: "Relationship element missing Id attribute".to_string(),
                })?
                .to_string();

            let rel_type = attr(el, "Type")
                .ok_or_else(|| PackageError::MalformedRelationship {
                    part: part_name.to_string(),
                    message: format!("Relationship {id} missing Type attribute"),
                })?
                .to_string();

            let target = attr(el, "Target")
                .ok_or_else(|| PackageError::MalformedRelationship {
                    part: part_name.to_string(),
                    message: format!("Relationship {id} missing Target attribute"),
                })?
                .to_string();

            let target_mode = attr(el, "TargetMode").map(|s| s.to_string());

            if let Some(n) = id.strip_prefix("rId").and_then(|s| s.parse::<u32>().ok()) {
                max_id = max_id.max(n);
            }

            entries.push(Relationship {
                id,
                rel_type,
                target,
                target_mode,
            });
        }

        Ok(RelationshipSet {
            entries,
            next_id: max_id + 1,
            base_dir: rels_base_dir(part_name),
        })
    }

    /// Create an empty relationship set (no entries, ids starting at `rId1`).
    /// Used by the save path to author a brand-new `.rels` part (e.g. a
    /// `customXml/_rels/item*.xml.rels` for a content-control data binding).
    /// The base directory is empty: a freshly authored set has no pre-existing
    /// absolute targets to reconcile relative ones against.
    pub fn empty() -> Self {
        RelationshipSet {
            entries: Vec::new(),
            next_id: 1,
            base_dir: String::new(),
        }
    }

    /// Serialize back to `.rels` XML bytes.
    pub fn serialize(&self, part_name: &str) -> Result<Vec<u8>, PackageError> {
        let mut root = Element::new("Relationships");
        let mut ns = Namespace::empty();
        ns.put("", RELS_NS);
        root.namespaces = Some(ns);
        root.namespace = Some(RELS_NS.to_string());

        for rel in &self.entries {
            let mut el = Element::new("Relationship");
            el.attributes.insert(attr_name("Id"), rel.id.clone());
            el.attributes
                .insert(attr_name("Type"), rel.rel_type.clone());
            el.attributes
                .insert(attr_name("Target"), rel.target.clone());
            if let Some(ref mode) = rel.target_mode {
                el.attributes.insert(attr_name("TargetMode"), mode.clone());
            }
            root.children.push(XMLNode::Element(el));
        }

        write_xml(&root, part_name)
    }

    /// Add a relationship entry, returning the assigned rId.
    ///
    /// If the relationship set already has an entry with the same type and target,
    /// returns the existing rId without adding a duplicate.
    pub fn add(&mut self, rel_type: &str, target: &str) -> String {
        if let Some(existing) = self.find_by_type_and_target(rel_type, target) {
            return existing.id.clone();
        }
        let id = format!("rId{}", self.next_id);
        self.next_id += 1;
        self.entries.push(Relationship {
            id: id.clone(),
            rel_type: rel_type.to_string(),
            target: target.to_string(),
            target_mode: None,
        });
        id
    }

    /// Add a relationship with a specific target mode (e.g. "External" for hyperlinks).
    pub fn add_external(&mut self, rel_type: &str, target: &str) -> String {
        if let Some(existing) = self.entries.iter().find(|r| {
            r.rel_type == rel_type
                && r.target == target
                && r.target_mode.as_deref() == Some("External")
        }) {
            return existing.id.clone();
        }
        let id = format!("rId{}", self.next_id);
        self.next_id += 1;
        self.entries.push(Relationship {
            id: id.clone(),
            rel_type: rel_type.to_string(),
            target: target.to_string(),
            target_mode: Some("External".to_string()),
        });
        id
    }

    /// Add a relationship with a preferred rId. Uses the preferred ID if available,
    /// otherwise assigns a new one. Returns the actually assigned rId.
    pub fn add_with_preferred_id(
        &mut self,
        rel_type: &str,
        target: &str,
        preferred_id: &str,
    ) -> String {
        // Check if this exact relationship already exists.
        if let Some(existing) = self.find_by_type_and_target(rel_type, target) {
            return existing.id.clone();
        }

        let id = if self.entries.iter().any(|r| r.id == preferred_id) {
            // preferred_id is taken, assign a new one
            let id = format!("rId{}", self.next_id);
            self.next_id += 1;
            id
        } else {
            // Use the preferred ID
            let id = preferred_id.to_string();
            // Update next_id if this preferred ID uses rId numbering
            if let Some(n) = id.strip_prefix("rId").and_then(|s| s.parse::<u32>().ok())
                && n >= self.next_id
            {
                self.next_id = n + 1;
            }
            id
        };

        self.entries.push(Relationship {
            id: id.clone(),
            rel_type: rel_type.to_string(),
            target: target.to_string(),
            target_mode: None,
        });
        id
    }

    /// Find the first relationship matching a given type. Test-support only.
    #[cfg(test)]
    pub fn find_by_type(&self, rel_type: &str) -> Option<&Relationship> {
        self.entries.iter().find(|r| r.rel_type == rel_type)
    }

    /// Resolve an internal relationship `Target` to its canonical package part
    /// path, using this set's base directory. A leading-slash target is
    /// package-absolute; anything else is relative to the described part's
    /// directory. `.`/`..` segments are collapsed. External (URL) targets are
    /// not part paths and must not be run through this.
    fn resolve_internal_target(&self, target: &str) -> String {
        if target.starts_with('/') {
            normalize_package_path(target)
        } else {
            normalize_package_path(&format!("{}{}", self.base_dir, target))
        }
    }

    /// Find an internal relationship by type and target part. Targets are
    /// compared by their resolved package path, so a pre-existing absolute
    /// target (`/word/x.xml`) and a base-relative one (`x.xml`) for the same
    /// part are recognized as the same relationship — the invariant that keeps
    /// part addition idempotent (no duplicate `(type, target)` relationship,
    /// which Word repairs on open). External-target entries never match here.
    pub fn find_by_type_and_target(&self, rel_type: &str, target: &str) -> Option<&Relationship> {
        let wanted = self.resolve_internal_target(target);
        self.entries.iter().find(|r| {
            r.rel_type == rel_type
                && r.target_mode.as_deref() != Some("External")
                && self.resolve_internal_target(&r.target) == wanted
        })
    }

    /// Find a relationship by its rId.
    pub fn find_by_id(&self, id: &str) -> Option<&Relationship> {
        self.entries.iter().find(|r| r.id == id)
    }

    /// Return the current maximum rId number (for callers that need to allocate
    /// IDs externally). Returns 0 if the set is empty.
    pub fn max_rid_number(&self) -> u32 {
        self.next_id.saturating_sub(1)
    }
}

// =============================================================================
// DocxPackage — the full typed package model
// =============================================================================

/// A DOCX package with parsed metadata and raw part bytes.
///
/// The three metadata files (`[Content_Types].xml`, `_rels/.rels`, and
/// `word/_rels/document.xml.rels`) are parsed into typed structures.
/// All other parts are stored as raw bytes in `parts`.
#[derive(Clone, Debug)]
pub struct DocxPackage {
    pub content_types: ContentTypes,
    pub root_rels: RelationshipSet,
    pub document_rels: RelationshipSet,
    /// Relationship sets for story parts (e.g. `word/_rels/header1.xml.rels`).
    /// Keys are the `.rels` file paths.
    pub story_rels: HashMap<String, RelationshipSet>,
    /// Raw part bytes, keyed by archive path. Does NOT include the three
    /// metadata parts (content types, root rels, document rels) or story rels
    /// that are in `story_rels`. The main document part IS in here (under its
    /// resolved, possibly non-conventional name), so it round-trips verbatim.
    parts: HashMap<String, Vec<u8>>,
    /// The resolved main document part name (via the officeDocument
    /// relationship — NOT assumed to be `word/document.xml`).
    main_part_name: String,
    /// The main part's own relationships part path, derived from
    /// `main_part_name` (e.g. `word/_rels/document2.xml.rels`).
    document_rels_path: String,
}

pub(crate) const CONTENT_TYPES_PATH: &str = "[Content_Types].xml";
const ROOT_RELS_PATH: &str = "_rels/.rels";

/// The OPC relationship type whose target is the package's main document part
/// (ECMA-376 Part 2 §9.3 / ISO/IEC 29500-2). The main part name is NOT fixed:
/// Word writes `word/document.xml` by convention, but any conformant name is
/// legal and must be discovered through this relationship.
pub const OFFICE_DOCUMENT_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument";

/// The canonical content type of the main document part (ECMA-376 Part 1 §15.2).
const MAIN_DOCUMENT_CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml";

/// The directory prefix of a part name, including the trailing `/`. Empty for a
/// part at the package root. `"word/document2.xml"` → `"word/"`;
/// `"document.xml"` → `""`.
pub fn part_dir(part_name: &str) -> &str {
    match part_name.rfind('/') {
        Some(i) => &part_name[..=i],
        None => "",
    }
}

/// The relationships part path for a part (OPC §9.3.4): the part's siblings
/// `_rels/` folder plus `<name>.rels`. `"word/document2.xml"` →
/// `"word/_rels/document2.xml.rels"`; `"document.xml"` →
/// `"_rels/document.xml.rels"`.
pub fn rels_part_path(part_name: &str) -> String {
    match part_name.rsplit_once('/') {
        Some((dir, file)) => format!("{dir}/_rels/{file}.rels"),
        None => format!("_rels/{part_name}.rels"),
    }
}

/// Normalize an OPC part name resolved from the package root: drop a single
/// leading `/` (a root-anchored target) and collapse `.`/`..` path segments
/// (OPC pack-URI resolution). Backslashes are not OPC path separators and are
/// left untouched — the ZIP read edge already rejects `..`-bearing raw item
/// names, so this only reshapes relationship targets into stored part names.
pub fn normalize_package_path(target: &str) -> String {
    let trimmed = target.strip_prefix('/').unwrap_or(target);
    let mut segments: Vec<&str> = Vec::new();
    for seg in trimmed.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    segments.join("/")
}

/// Locate the main document part per ECMA-376 Part 2 (OPC §9.3): parse the
/// package root-relationships part (`_rels/.rels`) and follow the
/// `officeDocument` relationship to the main part name.
///
/// Errors are distinct and name what was searched: a missing `_rels/.rels`
/// ([`PackageError::MissingPart`]), a `.rels` without the relationship
/// ([`PackageError::MissingOfficeDocumentRelationship`]), an External target
/// ([`PackageError::ExternalMainDocument`]), and a resolved-but-absent part
/// ([`PackageError::MainDocumentPartAbsent`]). There is no fallback to a
/// conventional name: an OPC package without a discoverable main part is
/// malformed, not defaulted.
pub fn resolve_main_document_part(archive: &DocxArchive) -> Result<String, PackageError> {
    let root_rels_bytes = archive
        .get(ROOT_RELS_PATH)
        .ok_or_else(|| PackageError::MissingPart(ROOT_RELS_PATH.to_string()))?;
    let root_rels = RelationshipSet::parse(root_rels_bytes, ROOT_RELS_PATH)?;
    main_document_part_from_root_rels(&root_rels, archive)
}

/// Core of [`resolve_main_document_part`], reused by [`DocxPackage::from_archive`]
/// which has already parsed the root relationships.
fn main_document_part_from_root_rels(
    root_rels: &RelationshipSet,
    archive: &DocxArchive,
) -> Result<String, PackageError> {
    let rel = root_rels
        .entries
        .iter()
        .find(|r| r.rel_type == OFFICE_DOCUMENT_REL_TYPE)
        .ok_or(PackageError::MissingOfficeDocumentRelationship)?;
    if rel.target_mode.as_deref() == Some("External") {
        return Err(PackageError::ExternalMainDocument {
            target: rel.target.clone(),
        });
    }
    let part_name = normalize_package_path(&rel.target);
    if archive.get(&part_name).is_none() {
        return Err(PackageError::MainDocumentPartAbsent {
            rel_id: rel.id.clone(),
            part: part_name,
        });
    }
    Ok(part_name)
}

impl DocxPackage {
    /// Parse all metadata from a `DocxArchive`, consuming the metadata parts
    /// into typed structures and keeping everything else as raw bytes.
    pub fn from_archive(archive: &DocxArchive) -> Result<Self, PackageError> {
        // Parse [Content_Types].xml (required).
        let ct_bytes = archive
            .get(CONTENT_TYPES_PATH)
            .ok_or_else(|| PackageError::MissingPart(CONTENT_TYPES_PATH.to_string()))?;
        let content_types = ContentTypes::parse(ct_bytes)?;

        // Parse _rels/.rels (required).
        let root_rels_bytes = archive
            .get(ROOT_RELS_PATH)
            .ok_or_else(|| PackageError::MissingPart(ROOT_RELS_PATH.to_string()))?;
        let root_rels = RelationshipSet::parse(root_rels_bytes, ROOT_RELS_PATH)?;

        // Locate the main document part via the officeDocument relationship
        // (OPC §9.3): its name is not fixed. The document rels part path is
        // derived from the resolved name, not hardcoded to word/document.xml.
        let main_part_name = main_document_part_from_root_rels(&root_rels, archive)?;
        let document_rels_path = rels_part_path(&main_part_name);

        // Parse the main part's relationships (required).
        let doc_rels_bytes = archive
            .get(&document_rels_path)
            .ok_or_else(|| PackageError::MissingPart(document_rels_path.clone()))?;
        let document_rels = RelationshipSet::parse(doc_rels_bytes, &document_rels_path)?;

        // Parse story rels (word/_rels/header*.xml.rels, footer*.xml.rels, etc.)
        // All name comparisons are ASCII case-insensitive: part-name
        // equivalence is case-insensitive per OPC §6.2, and stored spellings
        // may differ from the canonical ones.
        let document_rels_path_lower = document_rels_path.to_ascii_lowercase();
        let mut story_rels = HashMap::new();
        for name in archive.list() {
            let lower = name.to_ascii_lowercase();
            if lower.starts_with("word/_rels/")
                && lower.ends_with(".rels")
                && lower != document_rels_path_lower
                && let Some(bytes) = archive.get(name)
            {
                let rels = RelationshipSet::parse(bytes, name)?;
                story_rels.insert(name.to_string(), rels);
            }
        }

        // Collect all remaining parts as raw bytes. The main document part is
        // NOT a metadata part, so it is stored here under its resolved name and
        // round-trips verbatim.
        let metadata_paths: [&str; 3] = [
            CONTENT_TYPES_PATH,
            ROOT_RELS_PATH,
            document_rels_path.as_str(),
        ];
        let mut parts = HashMap::new();
        for name in archive.list() {
            if metadata_paths.iter().any(|m| m.eq_ignore_ascii_case(name))
                || story_rels.contains_key(name)
            {
                continue;
            }
            if let Some(bytes) = archive.get(name) {
                parts.insert(name.to_string(), bytes.to_vec());
            }
        }

        Ok(DocxPackage {
            content_types,
            root_rels,
            document_rels,
            story_rels,
            parts,
            main_part_name,
            document_rels_path,
        })
    }

    /// Serialize all metadata back to XML and combine with raw parts into a
    /// `DocxArchive` ready for ZIP writing.
    pub fn into_archive(self) -> Result<DocxArchive, PackageError> {
        let mut files = Vec::new();

        // Serialize [Content_Types].xml first (convention: it's the first entry).
        files.push(crate::docx::DocxFile {
            name: CONTENT_TYPES_PATH.to_string(),
            data: self.content_types.serialize()?,
        });

        // Serialize _rels/.rels.
        files.push(crate::docx::DocxFile {
            name: ROOT_RELS_PATH.to_string(),
            data: self.root_rels.serialize(ROOT_RELS_PATH)?,
        });

        // Serialize the main part's relationships at its derived path (which
        // follows the resolved main part name, e.g. document2.xml.rels).
        files.push(crate::docx::DocxFile {
            name: self.document_rels_path.clone(),
            data: self.document_rels.serialize(&self.document_rels_path)?,
        });

        // Serialize story rels.
        let mut story_rels_sorted: Vec<_> = self.story_rels.into_iter().collect();
        story_rels_sorted.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, rels) in story_rels_sorted {
            files.push(crate::docx::DocxFile {
                name: name.clone(),
                data: rels.serialize(&name)?,
            });
        }

        // Add all other parts.
        let mut parts_sorted: Vec<_> = self.parts.into_iter().collect();
        parts_sorted.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, data) in parts_sorted {
            files.push(crate::docx::DocxFile { name, data });
        }

        Ok(DocxArchive::from_parts(files))
    }

    /// Ensure every recognized WordprocessingML part present in the package
    /// carries its canonical content-type Override.
    ///
    /// OPC (ECMA-376 Part 2 §10.1.2) requires every part to have exactly one
    /// content type, and ECMA-376 Part 1 §15.2 fixes the content type of each
    /// WML part (e.g. `word/comments.xml` is
    /// `…wordprocessingml.comments+xml`). Word locates parts like comments,
    /// footnotes, and the style/numbering tables *by content type*, not by
    /// filename: a `word/comments.xml` that resolves only via the generic
    /// `Default Extension="xml"` (→ `application/xml`) is not recognized as the
    /// comments part, so Word reports "unreadable content" and drops the
    /// comments on repair.
    ///
    /// Some inbound packages (including hand-authored ones) ship a WML part with
    /// no Override; we preserve such defects verbatim today. This method makes
    /// the *output* honest: for every recognized WML part that has **no**
    /// Override at all, it adds the canonical one. It never rewrites an Override
    /// that already exists — an explicit author choice is left untouched rather
    /// than silently second-guessed.
    pub fn ensure_canonical_wml_content_types(&mut self) {
        // Collect part paths first to avoid borrowing `self.parts` while
        // mutating `self.content_types`.
        let part_paths: Vec<String> = self.parts.keys().cloned().collect();
        self.content_types
            .ensure_canonical_wml_for_parts(part_paths.iter().map(String::as_str));

        // The main document part's content type is fixed by ECMA-376 Part 1
        // §15.2 regardless of its (non-fixed) part name. `canonical_wml_content_type`
        // is keyed by the conventional `word/document.xml` filename, so a
        // non-standard main part name (e.g. `word/document2.xml`) is skipped by
        // the loop above — add its canonical Override here for the RESOLVED
        // name if none exists. Never rewrites an existing author choice.
        let override_name = format!("/{}", self.main_part_name);
        if !self.content_types.has_override(&override_name) {
            self.content_types
                .add_override(&override_name, MAIN_DOCUMENT_CONTENT_TYPE);
        }
    }

    /// The resolved main document part name (OPC §9.3). Not assumed to be
    /// `word/document.xml`.
    pub fn main_document_part_name(&self) -> &str {
        &self.main_part_name
    }

    /// Resolve `path` to the stored key, ASCII case-insensitively (OPC §6.2).
    /// Exact match first (the common case, O(1)); otherwise scan. The archive
    /// read edge rejects case-equivalent duplicates, so a scan hit is unique.
    fn stored_part_key(&self, path: &str) -> Option<&str> {
        if let Some((key, _)) = self.parts.get_key_value(path) {
            return Some(key.as_str());
        }
        self.parts
            .keys()
            .find(|k| k.eq_ignore_ascii_case(path))
            .map(|k| k.as_str())
    }

    /// Get the raw bytes of a part by its archive path (case-insensitive).
    pub fn get_part(&self, path: &str) -> Option<&[u8]> {
        let key = self.stored_part_key(path)?;
        self.parts.get(key).map(|v| v.as_slice())
    }

    /// Set (insert or replace) a part's raw bytes. Resolves an existing part
    /// case-insensitively and keeps its stored spelling, so relationship
    /// targets pointing at the original spelling stay valid and no
    /// case-equivalent duplicate part is ever created (OPC §6.2, §7.3).
    pub fn set_part(&mut self, path: &str, data: Vec<u8>) {
        let key = self
            .stored_part_key(path)
            .map(str::to_string)
            .unwrap_or_else(|| path.to_string());
        self.parts.insert(key, data);
    }

    /// Remove a part. Returns the removed data if the part existed.
    /// Test-support only.
    #[cfg(test)]
    pub fn remove_part(&mut self, path: &str) -> Option<Vec<u8>> {
        let key = self.stored_part_key(path)?.to_string();
        self.parts.remove(&key)
    }

    /// Check whether a part exists (case-insensitive, OPC §6.2).
    pub fn has_part(&self, path: &str) -> bool {
        self.stored_part_key(path).is_some()
    }

    /// Iterate over all part paths (excluding metadata).
    pub fn part_names(&self) -> impl Iterator<Item = &str> {
        self.parts.keys().map(|s| s.as_str())
    }

    /// Get or create the RelationshipSet for a story part's `.rels` file.
    ///
    /// `rels_path` should be the full `.rels` path, e.g. `word/_rels/header1.xml.rels`.
    /// Test-support only.
    #[cfg(test)]
    pub fn story_rels_mut(&mut self, rels_path: &str) -> &mut RelationshipSet {
        self.story_rels
            .entry(rels_path.to_string())
            .or_insert_with(RelationshipSet::empty)
    }
}

// =============================================================================
// Internal XML helpers
// =============================================================================

fn parse_xml(bytes: &[u8], part_name: &str) -> Result<Element, PackageError> {
    crate::word_xml::parse_document_xml(bytes).map_err(|e| PackageError::XmlParse {
        part: part_name.to_string(),
        reason: format!("{e:?}"),
    })
}

fn write_xml(element: &Element, part_name: &str) -> Result<Vec<u8>, PackageError> {
    let mut out = Vec::new();
    element
        .write(&mut out)
        .map_err(|e| PackageError::XmlWrite {
            part: part_name.to_string(),
            source: e,
        })?;
    Ok(out)
}

/// Get the local name of an element (strip namespace prefix).
fn local_name(element: &Element) -> &str {
    if let Some(pos) = element.name.find(':') {
        &element.name[pos + 1..]
    } else {
        &element.name
    }
}

/// Get an attribute value, trying both unqualified and with common variations.
fn attr<'a>(element: &'a Element, local: &str) -> Option<&'a str> {
    // Try direct local name lookup (handles both namespaced and unqualified).
    for (name, value) in &element.attributes {
        if name.local_name == local {
            return Some(value.as_str());
        }
    }
    None
}

/// Create an unqualified attribute name.
fn attr_name(local: &str) -> xmltree::AttributeName {
    xmltree::AttributeName::local(local)
}

/// The canonical OPC content type for a WordprocessingML part, keyed by its
/// archive path. Returns `None` for parts whose content type is not fixed by
/// ECMA-376 Part 1 §15.2 (e.g. media, customXml, glossary, or unknown names) —
/// those are content-typed elsewhere (media via a `Default` extension,
/// customXml/glossary via their own dedicated override logic) and must not be
/// rewritten here.
///
/// Matched by exact filename under `word/` so it cannot collide with a
/// differently-named author part. Header/footer parts are matched by the
/// `header<N>.xml` / `footer<N>.xml` numbering convention Word uses.
pub(crate) fn canonical_wml_content_type(path: &str) -> Option<&'static str> {
    let file = path.strip_prefix("word/")?;
    // No nested directories: a story part lives directly under word/.
    if file.contains('/') {
        return None;
    }
    let ct = match file {
        "document.xml" => {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"
        }
        "styles.xml" => "application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml",
        "numbering.xml" => {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"
        }
        "settings.xml" => {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml"
        }
        "webSettings.xml" => {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.webSettings+xml"
        }
        "fontTable.xml" => {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.fontTable+xml"
        }
        "footnotes.xml" => {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"
        }
        "endnotes.xml" => {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.endnotes+xml"
        }
        "comments.xml" => {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"
        }
        _ if is_numbered_story(file, "header") => {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"
        }
        _ if is_numbered_story(file, "footer") => {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"
        }
        _ => return None,
    };
    Some(ct)
}

/// True when `file` is `{prefix}<N>.xml` with a (possibly empty) numeric suffix
/// — matching Word's `header1.xml` / `footer2.xml` story-part naming.
fn is_numbered_story(file: &str, prefix: &str) -> bool {
    let Some(rest) = file.strip_prefix(prefix) else {
        return false;
    };
    let Some(num) = rest.strip_suffix(".xml") else {
        return false;
    };
    !num.is_empty() && num.bytes().all(|b| b.is_ascii_digit())
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_content_types_xml() -> Vec<u8> {
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
  <Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/>
</Types>"#.to_vec()
    }

    fn minimal_rels_xml() -> Vec<u8> {
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#.to_vec()
    }

    fn minimal_document_rels_xml() -> Vec<u8> {
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/>
  <Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.com" TargetMode="External"/>
</Relationships>"#.to_vec()
    }

    // =========================================================================
    // ContentTypes tests
    // =========================================================================

    #[test]
    fn content_types_parse() {
        let ct = ContentTypes::parse(&minimal_content_types_xml()).unwrap();
        assert_eq!(ct.defaults.len(), 2);
        assert_eq!(ct.defaults[0].extension, "rels");
        assert_eq!(ct.defaults[1].extension, "xml");
        assert_eq!(ct.overrides.len(), 2);
        assert_eq!(ct.overrides[0].part_name, "/word/document.xml");
        assert_eq!(ct.overrides[1].part_name, "/word/styles.xml");
    }

    #[test]
    fn content_types_roundtrip() {
        let original = ContentTypes::parse(&minimal_content_types_xml()).unwrap();
        let serialized = original.serialize().unwrap();
        let reparsed = ContentTypes::parse(&serialized).unwrap();

        assert_eq!(original.defaults, reparsed.defaults);
        assert_eq!(original.overrides, reparsed.overrides);
    }

    #[test]
    fn content_types_add_override() {
        let mut ct = ContentTypes::parse(&minimal_content_types_xml()).unwrap();
        assert!(!ct.has_override("/word/people.xml"));

        ct.add_override(
            "/word/people.xml",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.people+xml",
        );
        assert!(ct.has_override("/word/people.xml"));
        assert_eq!(ct.overrides.len(), 3);

        // Adding again is a no-op.
        ct.add_override(
            "/word/people.xml",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.people+xml",
        );
        assert_eq!(ct.overrides.len(), 3);
    }

    #[test]
    fn content_types_has_default() {
        let ct = ContentTypes::parse(&minimal_content_types_xml()).unwrap();
        assert!(ct.has_default("rels"));
        assert!(ct.has_default("xml"));
        assert!(ct.has_default("XML")); // case-insensitive
        assert!(!ct.has_default("png"));
    }

    // =========================================================================
    // Canonical WML content-type tests (§15.2)
    // =========================================================================

    #[test]
    fn canonical_wml_content_type_known_parts() {
        let cases = &[
            ("word/comments.xml", "comments+xml"),
            ("word/footnotes.xml", "footnotes+xml"),
            ("word/endnotes.xml", "endnotes+xml"),
            ("word/styles.xml", "styles+xml"),
            ("word/numbering.xml", "numbering+xml"),
            ("word/settings.xml", "settings+xml"),
            ("word/fontTable.xml", "fontTable+xml"),
            ("word/header1.xml", "header+xml"),
            ("word/footer12.xml", "footer+xml"),
        ];
        for (path, suffix) in cases {
            let ct = canonical_wml_content_type(path)
                .unwrap_or_else(|| panic!("expected canonical type for {path}"));
            assert!(
                ct.ends_with(suffix),
                "{path}: {ct} should end with {suffix}"
            );
        }
    }

    #[test]
    fn canonical_wml_content_type_rejects_non_wml_and_nested() {
        // Media, customXml, nested dirs, and non-numbered story names are not
        // fixed-type WML parts and must NOT be auto-typed here.
        assert_eq!(canonical_wml_content_type("word/media/image1.png"), None);
        assert_eq!(canonical_wml_content_type("customXml/item1.xml"), None);
        assert_eq!(
            canonical_wml_content_type("word/glossary/document.xml"),
            None
        );
        assert_eq!(canonical_wml_content_type("word/header.xml"), None); // no number
        assert_eq!(canonical_wml_content_type("docProps/core.xml"), None);
    }

    #[test]
    fn ensure_canonical_adds_missing_comments_override_only() {
        // A package with a comments part but no comments Override.
        let archive = crate::docx::DocxArchive::from_parts(vec![
            crate::docx::DocxFile {
                name: "[Content_Types].xml".to_string(),
                data: minimal_content_types_xml(),
            },
            crate::docx::DocxFile {
                name: "_rels/.rels".to_string(),
                data: minimal_rels_xml(),
            },
            crate::docx::DocxFile {
                name: "word/_rels/document.xml.rels".to_string(),
                data: minimal_document_rels_xml(),
            },
            crate::docx::DocxFile {
                name: "word/document.xml".to_string(),
                data: b"<w:document/>".to_vec(),
            },
            crate::docx::DocxFile {
                name: "word/comments.xml".to_string(),
                data: b"<w:comments/>".to_vec(),
            },
        ]);
        let mut pkg = DocxPackage::from_archive(&archive).unwrap();
        assert!(!pkg.content_types.has_override("/word/comments.xml"));
        let before = pkg.content_types.overrides.len();

        pkg.ensure_canonical_wml_content_types();

        assert!(
            pkg.content_types.has_override("/word/comments.xml"),
            "comments override must be added"
        );
        let added = pkg
            .content_types
            .overrides
            .iter()
            .find(|o| o.part_name == "/word/comments.xml")
            .unwrap();
        assert!(added.content_type.ends_with("comments+xml"));
        // Exactly one override added; document.xml/styles.xml already present
        // are untouched (idempotent for already-canonical parts).
        assert_eq!(pkg.content_types.overrides.len(), before + 1);
    }

    #[test]
    fn ensure_canonical_does_not_rewrite_existing_override() {
        // If a WML part already has an Override (even a non-canonical one), we
        // leave it untouched — no silent second-guessing of an author choice.
        let mut ct = ContentTypes::parse(&minimal_content_types_xml()).unwrap();
        ct.add_override("/word/comments.xml", "application/totally-bogus");
        let archive = crate::docx::DocxArchive::from_parts(vec![
            crate::docx::DocxFile {
                name: "[Content_Types].xml".to_string(),
                data: ct.serialize().unwrap(),
            },
            crate::docx::DocxFile {
                name: "_rels/.rels".to_string(),
                data: minimal_rels_xml(),
            },
            crate::docx::DocxFile {
                name: "word/_rels/document.xml.rels".to_string(),
                data: minimal_document_rels_xml(),
            },
            crate::docx::DocxFile {
                name: "word/document.xml".to_string(),
                data: b"<w:document/>".to_vec(),
            },
            crate::docx::DocxFile {
                name: "word/comments.xml".to_string(),
                data: b"<w:comments/>".to_vec(),
            },
        ]);
        let mut pkg = DocxPackage::from_archive(&archive).unwrap();
        pkg.ensure_canonical_wml_content_types();
        let entry = pkg
            .content_types
            .overrides
            .iter()
            .find(|o| o.part_name == "/word/comments.xml")
            .unwrap();
        assert_eq!(
            entry.content_type, "application/totally-bogus",
            "existing override must be preserved, not rewritten"
        );
    }

    #[test]
    fn ensure_canonical_adds_document_override_when_only_xml_default_covers_it() {
        // Real-world shape (OpenXmlPowerTools HtmlConverter01 Test-08): the main
        // document part carries NO Override; its content type is supplied solely
        // by `Default Extension="xml"` pointed at the WML main type. The
        // post-serialization validator (I-CT-002) requires an explicit Override
        // for `/word/document.xml`, because Word locates the part by content type
        // and a generic xml Default is not enough. The save guarantee must add
        // the Override while leaving the author's Default untouched.
        let ct_xml = br#"<?xml version="1.0" encoding="utf-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
</Types>"#
            .to_vec();
        let archive = crate::docx::DocxArchive::from_parts(vec![
            crate::docx::DocxFile {
                name: "[Content_Types].xml".to_string(),
                data: ct_xml,
            },
            crate::docx::DocxFile {
                name: "_rels/.rels".to_string(),
                data: minimal_rels_xml(),
            },
            crate::docx::DocxFile {
                name: "word/_rels/document.xml.rels".to_string(),
                data: minimal_document_rels_xml(),
            },
            crate::docx::DocxFile {
                name: "word/document.xml".to_string(),
                data: b"<w:document/>".to_vec(),
            },
        ]);
        let mut pkg = DocxPackage::from_archive(&archive).unwrap();
        assert!(
            !pkg.content_types.has_override("/word/document.xml"),
            "precondition: document.xml has no Override, only the xml Default"
        );

        pkg.ensure_canonical_wml_content_types();

        let added = pkg
            .content_types
            .overrides
            .iter()
            .find(|o| o.part_name == "/word/document.xml")
            .expect("document.xml Override must be added");
        assert_eq!(
            added.content_type,
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"
        );
        // The author's `Default Extension="xml"` is preserved, not rewritten.
        assert!(
            pkg.content_types
                .defaults
                .iter()
                .any(|d| d.extension == "xml"),
            "the xml Default must be left in place"
        );
    }

    // =========================================================================
    // RelationshipSet tests
    // =========================================================================

    #[test]
    fn relationship_set_parse() {
        let rels =
            RelationshipSet::parse(&minimal_document_rels_xml(), "word/_rels/document.xml.rels")
                .unwrap();
        assert_eq!(rels.entries.len(), 3);
        assert_eq!(rels.entries[0].id, "rId1");
        assert_eq!(rels.entries[1].id, "rId2");
        assert_eq!(rels.entries[1].target, "header1.xml");
        assert_eq!(rels.entries[2].id, "rId5");
        assert_eq!(rels.entries[2].target_mode.as_deref(), Some("External"));
        // next_id should be max(1,2,5) + 1 = 6
        assert_eq!(rels.next_id, 6);
    }

    #[test]
    fn relationship_set_roundtrip() {
        let original =
            RelationshipSet::parse(&minimal_document_rels_xml(), "word/_rels/document.xml.rels")
                .unwrap();
        let serialized = original.serialize("word/_rels/document.xml.rels").unwrap();
        let reparsed = RelationshipSet::parse(&serialized, "word/_rels/document.xml.rels").unwrap();

        assert_eq!(original.entries, reparsed.entries);
        assert_eq!(original.next_id, reparsed.next_id);
    }

    #[test]
    fn relationship_set_add() {
        let mut rels =
            RelationshipSet::parse(&minimal_document_rels_xml(), "word/_rels/document.xml.rels")
                .unwrap();

        let id = rels.add(
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer",
            "footer1.xml",
        );
        assert_eq!(id, "rId6");
        assert_eq!(rels.entries.len(), 4);
        assert_eq!(rels.entries[3].target, "footer1.xml");

        // Adding the same type+target returns the existing ID.
        let id2 = rels.add(
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer",
            "footer1.xml",
        );
        assert_eq!(id2, "rId6");
        assert_eq!(rels.entries.len(), 4);
    }

    // Domain invariant: a relationship set carries at most one relationship per
    // (type, effective target part). Adding a part that is already referenced
    // must reuse the existing relationship — even when the pre-existing target
    // was written package-absolute (`/word/x.xml`) and the add uses a
    // base-relative form (`x.xml`), or vice versa. Two relationships of the same
    // type resolving to the same part (differing only in Id/target-form) are a
    // duplicate that Word repairs on open. Exercised on a NON-conventional main
    // part (`word/document2.xml`) carrying pre-existing extended-comment infra
    // with absolute targets — the write path must reference that part by its
    // real name, and dedup must span target-forms.
    #[test]
    fn relationship_set_add_dedups_across_absolute_and_relative_targets() {
        const COMMENTS_EXTENDED: &str =
            "http://schemas.microsoft.com/office/2011/relationships/commentsExtended";
        let doc_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="/word/styles.xml"/>
  <Relationship Id="Rext" Type="http://schemas.microsoft.com/office/2011/relationships/commentsExtended" Target="/word/commentsExtended.xml"/>
</Relationships>"#;
        // Non-conventional main part: rels live in document2.xml.rels, base word/.
        let mut rels = RelationshipSet::parse(doc_rels, "word/_rels/document2.xml.rels").unwrap();
        assert_eq!(rels.entries.len(), 2);

        // The serializer re-emits commentsExtended with a base-RELATIVE target.
        // It must reuse the pre-existing absolute-target relationship, not append.
        let reused = rels.add(COMMENTS_EXTENDED, "commentsExtended.xml");
        assert_eq!(
            reused, "Rext",
            "must reuse the pre-existing relationship id"
        );
        assert_eq!(
            rels.entries.len(),
            2,
            "no duplicate (type, target) relationship may be appended"
        );

        // Symmetric direction: a pre-existing RELATIVE target is matched by an
        // absolute-form add.
        let rel_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="Rrel" Type="http://schemas.microsoft.com/office/2011/relationships/commentsExtended" Target="commentsExtended.xml"/>
</Relationships>"#;
        let mut rels2 = RelationshipSet::parse(rel_rels, "word/_rels/document.xml.rels").unwrap();
        let again = rels2.add(COMMENTS_EXTENDED, "/word/commentsExtended.xml");
        assert_eq!(again, "Rrel");
        assert_eq!(rels2.entries.len(), 1);
    }

    #[test]
    fn relationship_set_add_with_preferred_id() {
        let mut rels =
            RelationshipSet::parse(&minimal_document_rels_xml(), "word/_rels/document.xml.rels")
                .unwrap();

        // Preferred rId3 is available.
        let id = rels.add_with_preferred_id(
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer",
            "footer1.xml",
            "rId3",
        );
        assert_eq!(id, "rId3");

        // Preferred rId1 is taken — gets a new ID.
        let id2 = rels.add_with_preferred_id(
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer",
            "footer2.xml",
            "rId1",
        );
        assert_eq!(id2, "rId6");
    }

    #[test]
    fn relationship_set_find_methods() {
        let rels =
            RelationshipSet::parse(&minimal_document_rels_xml(), "word/_rels/document.xml.rels")
                .unwrap();

        let header = rels.find_by_type(
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header",
        );
        assert!(header.is_some());
        assert_eq!(header.unwrap().target, "header1.xml");

        let by_id = rels.find_by_id("rId5");
        assert!(by_id.is_some());
        assert_eq!(by_id.unwrap().target, "https://example.com");

        let missing = rels.find_by_type("http://nonexistent");
        assert!(missing.is_none());
    }

    #[test]
    fn relationship_set_add_external() {
        let mut rels = RelationshipSet::empty();
        let id = rels.add_external(
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink",
            "https://example.com",
        );
        assert_eq!(id, "rId1");
        assert_eq!(rels.entries[0].target_mode.as_deref(), Some("External"));

        // Duplicate is a no-op.
        let id2 = rels.add_external(
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink",
            "https://example.com",
        );
        assert_eq!(id2, "rId1");
        assert_eq!(rels.entries.len(), 1);
    }

    #[test]
    fn relationship_set_max_rid_number() {
        let rels =
            RelationshipSet::parse(&minimal_document_rels_xml(), "word/_rels/document.xml.rels")
                .unwrap();
        assert_eq!(rels.max_rid_number(), 5);

        let empty = RelationshipSet::empty();
        assert_eq!(empty.max_rid_number(), 0);
    }

    // =========================================================================
    // DocxPackage tests
    // =========================================================================

    fn build_test_archive() -> DocxArchive {
        use crate::docx::DocxFile;
        DocxArchive::from_parts(vec![
            DocxFile {
                name: "[Content_Types].xml".to_string(),
                data: minimal_content_types_xml(),
            },
            DocxFile {
                name: "_rels/.rels".to_string(),
                data: minimal_rels_xml(),
            },
            DocxFile {
                name: "word/_rels/document.xml.rels".to_string(),
                data: minimal_document_rels_xml(),
            },
            DocxFile {
                name: "word/document.xml".to_string(),
                data: b"<w:document/>".to_vec(),
            },
            DocxFile {
                name: "word/styles.xml".to_string(),
                data: b"<w:styles/>".to_vec(),
            },
        ])
    }

    #[test]
    fn package_from_archive() {
        let archive = build_test_archive();
        let pkg = DocxPackage::from_archive(&archive).unwrap();

        assert_eq!(pkg.content_types.defaults.len(), 2);
        assert_eq!(pkg.content_types.overrides.len(), 2);
        assert_eq!(pkg.root_rels.entries.len(), 1);
        assert_eq!(pkg.document_rels.entries.len(), 3);
        assert!(pkg.has_part("word/document.xml"));
        assert!(pkg.has_part("word/styles.xml"));
        // Metadata parts should NOT be in parts map.
        assert!(!pkg.has_part("[Content_Types].xml"));
        assert!(!pkg.has_part("_rels/.rels"));
        assert!(!pkg.has_part("word/_rels/document.xml.rels"));
    }

    #[test]
    fn package_roundtrip() {
        let archive = build_test_archive();
        let pkg = DocxPackage::from_archive(&archive).unwrap();

        // Modify something.
        let mut pkg = pkg;
        pkg.content_types.add_override(
            "/word/people.xml",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.people+xml",
        );
        pkg.document_rels.add(
            "http://schemas.microsoft.com/office/2011/relationships/people",
            "people.xml",
        );
        pkg.set_part("word/people.xml", b"<people/>".to_vec());

        // Roundtrip through archive.
        let archive2 = pkg.into_archive().unwrap();
        let pkg2 = DocxPackage::from_archive(&archive2).unwrap();

        assert_eq!(pkg2.content_types.overrides.len(), 3);
        assert!(pkg2.content_types.has_override("/word/people.xml"));
        assert_eq!(pkg2.document_rels.entries.len(), 4);
        assert!(pkg2.has_part("word/people.xml"));
    }

    #[test]
    fn package_story_rels() {
        use crate::docx::DocxFile;
        let header_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/>
</Relationships>"#;

        let archive = DocxArchive::from_parts(vec![
            DocxFile {
                name: "[Content_Types].xml".to_string(),
                data: minimal_content_types_xml(),
            },
            DocxFile {
                name: "_rels/.rels".to_string(),
                data: minimal_rels_xml(),
            },
            DocxFile {
                name: "word/_rels/document.xml.rels".to_string(),
                data: minimal_document_rels_xml(),
            },
            DocxFile {
                name: "word/_rels/header1.xml.rels".to_string(),
                data: header_rels.to_vec(),
            },
            DocxFile {
                name: "word/document.xml".to_string(),
                data: b"<w:document/>".to_vec(),
            },
            DocxFile {
                name: "word/header1.xml".to_string(),
                data: b"<w:hdr/>".to_vec(),
            },
        ]);

        let pkg = DocxPackage::from_archive(&archive).unwrap();
        assert_eq!(pkg.story_rels.len(), 1);
        let header_rels = pkg.story_rels.get("word/_rels/header1.xml.rels").unwrap();
        assert_eq!(header_rels.entries.len(), 1);
        assert_eq!(header_rels.entries[0].target, "media/image1.png");

        // header1.xml.rels should NOT be in raw parts.
        assert!(!pkg.has_part("word/_rels/header1.xml.rels"));
    }

    #[test]
    fn part_lookup_is_ascii_case_insensitive_and_keeps_stored_spelling() {
        // OPC §6.2: part-name equivalence is ASCII case-insensitive. A part
        // stored with a different spelling must resolve, and set_part must
        // replace it under the stored spelling (so relationship targets that
        // point at it stay valid) instead of inserting a case-equivalent
        // sibling (forbidden by OPC §7.3).
        let archive = build_test_archive();
        let mut pkg = DocxPackage::from_archive(&archive).unwrap();
        let original = pkg.remove_part("word/document.xml").unwrap();
        pkg.set_part("word/Document.xml", original);

        assert!(pkg.has_part("word/document.xml"));
        assert!(pkg.get_part("word/document.xml").is_some());

        pkg.set_part("word/document.xml", b"<w:document/>".to_vec());
        let names: Vec<&str> = pkg
            .part_names()
            .filter(|n| n.eq_ignore_ascii_case("word/document.xml"))
            .collect();
        assert_eq!(
            names,
            vec!["word/Document.xml"],
            "set_part must replace the case-equivalent part under its stored spelling"
        );
    }

    #[test]
    fn has_override_is_ascii_case_insensitive() {
        // OPC §7.2: Override PartName matching is ASCII case-insensitive.
        let ct = ContentTypes::parse(&minimal_content_types_xml()).unwrap();
        assert!(ct.has_override("/word/document.xml"));
        assert!(ct.has_override("/word/Document.xml"));
        assert!(!ct.has_override("/word/comments.xml"));
    }

    #[test]
    fn package_set_and_remove_part() {
        let archive = build_test_archive();
        let mut pkg = DocxPackage::from_archive(&archive).unwrap();

        pkg.set_part("word/footnotes.xml", b"<footnotes/>".to_vec());
        assert!(pkg.has_part("word/footnotes.xml"));
        assert_eq!(
            pkg.get_part("word/footnotes.xml"),
            Some(b"<footnotes/>".as_slice())
        );

        let removed = pkg.remove_part("word/footnotes.xml");
        assert!(removed.is_some());
        assert!(!pkg.has_part("word/footnotes.xml"));
    }

    #[test]
    fn package_story_rels_mut_creates_empty() {
        let archive = build_test_archive();
        let mut pkg = DocxPackage::from_archive(&archive).unwrap();

        assert!(!pkg.story_rels.contains_key("word/_rels/footer1.xml.rels"));

        let rels = pkg.story_rels_mut("word/_rels/footer1.xml.rels");
        assert!(rels.entries.is_empty());

        let id = rels.add(
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image",
            "media/image2.png",
        );
        assert_eq!(id, "rId1");
    }

    #[test]
    fn package_missing_content_types_errors() {
        use crate::docx::DocxFile;
        let archive = DocxArchive::from_parts(vec![DocxFile {
            name: "_rels/.rels".to_string(),
            data: minimal_rels_xml(),
        }]);

        let result = DocxPackage::from_archive(&archive);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, PackageError::MissingPart(ref p) if p == "[Content_Types].xml"),
            "expected MissingPart, got: {err}"
        );
    }

    #[test]
    fn relationship_set_serialize_has_xmlns() {
        let rels = RelationshipSet::parse(&minimal_rels_xml(), "_rels/.rels").unwrap();
        let bytes = rels.serialize("_rels/.rels").unwrap();
        let output = String::from_utf8(bytes).unwrap();
        assert!(
            output
                .contains("xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\""),
            "Serialized .rels must include xmlns declaration, got: {output}"
        );
    }

    #[test]
    fn relationship_set_fresh_serialize_has_xmlns() {
        let mut fresh = RelationshipSet::empty();
        fresh.add(
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument",
            "word/document.xml",
        );
        let bytes = fresh.serialize("_rels/.rels").unwrap();
        let output = String::from_utf8(bytes).unwrap();
        assert!(
            output
                .contains("xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\""),
            "Freshly constructed .rels must include xmlns declaration, got: {output}"
        );
    }

    #[test]
    fn content_types_serialize_has_xmlns() {
        let ct = ContentTypes::parse(&minimal_content_types_xml()).unwrap();
        let bytes = ct.serialize().unwrap();
        let output = String::from_utf8(bytes).unwrap();
        assert!(
            output
                .contains("xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\""),
            "Serialized [Content_Types].xml must include xmlns declaration, got: {output}"
        );
    }

    #[test]
    fn relationship_set_empty_serialize_roundtrip() {
        let empty = RelationshipSet::empty();
        let bytes = empty.serialize("_rels/.rels").unwrap();
        let reparsed = RelationshipSet::parse(&bytes, "_rels/.rels").unwrap();
        assert!(reparsed.entries.is_empty());
        assert_eq!(reparsed.next_id, 1);
    }
}
