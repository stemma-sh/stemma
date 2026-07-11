//! Post-serialization DOCX validator.
//!
//! After serializing a DOCX file, this module checks the final ZIP bytes for
//! OOXML spec violations before returning them to the caller. Each check is a
//! standalone function that inspects parsed package state and appends findings.

use std::collections::{HashMap, HashSet};
use std::io::Cursor;

use xmltree::{Element, ParserConfig, XMLNode};
use zip::ZipArchive;

// =============================================================================
// Data model
// =============================================================================

/// Result of validating a serialized DOCX package.
pub struct DocxValidation {
    pub findings: Vec<ValidationFinding>,
}

pub struct ValidationFinding {
    pub rule_id: &'static str,
    pub severity: ValidationSeverity,
    pub message: String,
    /// Where the problem was found (e.g., "word/document.xml", "word/_rels/document.xml.rels")
    pub location: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationSeverity {
    /// Word will reject the file or lose data.
    Error,
    /// File opens but behavior may be wrong.
    Warning,
}

impl DocxValidation {
    pub fn has_errors(&self) -> bool {
        self.findings
            .iter()
            .any(|f| f.severity == ValidationSeverity::Error)
    }

    pub fn errors(&self) -> impl Iterator<Item = &ValidationFinding> {
        self.findings
            .iter()
            .filter(|f| f.severity == ValidationSeverity::Error)
    }

    /// Findings the given `blocking` rule set does not refuse on, collapsed to
    /// one summary line per rule id (in first-seen order).
    ///
    /// These are *advisory on the calling path*: its gate refuses only on the
    /// `blocking` rules, so every other finding describes a condition that
    /// path deliberately lets through — Word opens the file and loses no data
    /// (e.g. `I-ANN-001`, a duplicate annotation `w:id`: non-conformant per
    /// ECMA-376 but tolerated by Word, and on the merge path inherited
    /// byte-faithfully from the input rather than introduced here). A
    /// finding's own [`Display`](std::fmt::Display) renders its intrinsic
    /// severity as `ERROR`, which on this path is a false alarm; and one line
    /// per occurrence turns a successful operation into a wall of them. So the
    /// caller labels these `advisory` and this method de-duplicates: a
    /// document with N findings of one rule collapses to a single counted line
    /// carrying one representative location and message. Blocking findings are
    /// the caller's to surface loudly and are excluded here.
    pub fn advisory_summary(&self, blocking: &[&str]) -> Vec<String> {
        use std::collections::hash_map::Entry;

        let mut order: Vec<&str> = Vec::new();
        let mut by_rule: HashMap<&str, (usize, &ValidationFinding)> = HashMap::new();
        for finding in &self.findings {
            if blocking.contains(&finding.rule_id) {
                continue;
            }
            match by_rule.entry(finding.rule_id) {
                Entry::Occupied(mut e) => e.get_mut().0 += 1,
                Entry::Vacant(e) => {
                    order.push(finding.rule_id);
                    e.insert((1, finding));
                }
            }
        }
        order
            .into_iter()
            .map(|rule_id| {
                let (count, example) = by_rule[rule_id];
                let times = if count == 1 {
                    String::new()
                } else {
                    format!(" ×{count}")
                };
                format!(
                    "[{}]{times} @ {}: {}",
                    example.rule_id, example.location, example.message
                )
            })
            .collect()
    }
}

impl std::fmt::Display for ValidationFinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sev = match self.severity {
            ValidationSeverity::Error => "ERROR",
            ValidationSeverity::Warning => "WARN",
        };
        write!(
            f,
            "[{}] {} @ {}: {}",
            self.rule_id, sev, self.location, self.message
        )
    }
}

impl std::fmt::Debug for ValidationFinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

// =============================================================================
// Relationship type constants
// =============================================================================

const HEADER_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header";
const FOOTER_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer";
const FOOTNOTES_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes";
const ENDNOTES_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/endnotes";
const COMMENTS_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments";
const PEOPLE_REL_TYPE: &str = "http://schemas.microsoft.com/office/2011/relationships/people";

/// Relationship namespace used in officeDocument relationships.
const REL_ATTR_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

// =============================================================================
// Parsed package state (shared across checks)
// =============================================================================

/// A parsed relationship entry from a `.rels` file.
pub(crate) struct ParsedRelationship {
    pub(crate) id: String,
    pub(crate) rel_type: String,
    pub(crate) target: String,
    pub(crate) target_mode: Option<String>,
}

/// Package state extracted once, shared across all checks.
///
/// Every XML part is parsed exactly once, here. A part that fails to parse is
/// reported as an I-XML-001 Error during construction and is simply absent
/// from the parsed state — so a check that finds nothing to inspect can trust
/// that the absence was already reported, never silently tolerated.
pub(crate) struct PackageState {
    /// All part names in the ZIP archive.
    pub(crate) part_names: HashSet<String>,
    /// Parsed [Content_Types].xml, if available.
    pub(crate) content_types_xml: Option<Element>,
    /// Parsed .rels files: key is the .rels part name, value is the parsed relationships.
    pub(crate) rels_files: HashMap<String, Vec<ParsedRelationship>>,
    /// Story parts (document.xml, headers, footers, footnotes, endnotes,
    /// comments) parsed once, sorted by part name so findings are deterministic.
    pub(crate) story_parts: Vec<(String, Element)>,
    /// Parsed word/styles.xml, if present and well-formed.
    pub(crate) styles_root: Option<Element>,
    /// Parsed word/numbering.xml, if present and well-formed.
    pub(crate) numbering_root: Option<Element>,
    /// The main document part name, located via the officeDocument relationship
    /// in `_rels/.rels` (OPC §9.3) — NOT assumed to be `word/document.xml`.
    /// `None` when the package has no discoverable main part (reported by
    /// I-PKG-002).
    pub(crate) main_part: Option<String>,
}

impl PackageState {
    /// Whether a part named `name` exists in the package, comparing part names
    /// ASCII case-insensitively per OPC part-name equivalence (ECMA-376 Part 2
    /// §9.1). A relationship target that resolves to `customXml/item1.xml` is
    /// satisfied by a stored `customXML/item1.xml`, and vice versa.
    pub(crate) fn contains_part_ci(&self, name: &str) -> bool {
        self.part_names.contains(name)
            || self.part_names.iter().any(|n| n.eq_ignore_ascii_case(name))
    }

    /// The parsed comments story (`word/comments.xml`), if present and well-formed.
    pub(crate) fn comments_root(&self) -> Option<&Element> {
        self.story_parts
            .iter()
            .find(|(name, _)| name == "word/comments.xml")
            .map(|(_, root)| root)
    }
}

// =============================================================================
// Main entry point
// =============================================================================

/// Validate a serialized DOCX package for OOXML spec compliance.
/// Checks package-level invariants that the serializer might violate.
pub fn validate_docx(bytes: &[u8]) -> DocxValidation {
    let mut findings = Vec::new();

    let cursor = Cursor::new(bytes);
    let mut zip = match ZipArchive::new(cursor) {
        Ok(z) => z,
        Err(e) => {
            findings.push(ValidationFinding {
                rule_id: "I-PKG-000",
                severity: ValidationSeverity::Error,
                message: format!("cannot open ZIP archive: {e}"),
                location: "(package)".to_string(),
            });
            return DocxValidation { findings };
        }
    };

    // Build the package state once.
    let state = build_package_state(&mut zip, &mut findings);

    // Run all checks.
    check_pkg_001_rels_exists(&state, &mut findings);
    check_pkg_002_document_exists(&state, &mut findings);
    check_ct_001_content_types(&state, &mut findings);
    check_ct_002_canonical_wml_content_types(&state, &mut findings);
    check_rel_001_rid_references(&state, &mut findings);
    check_rel_004_hdrftr_ref_requires_rid(&state, &mut findings);
    check_rel_002_id_uniqueness(&state, &mut findings);
    check_rel_003_internal_targets(&state, &mut findings);
    check_story_001_story_relationships(&state, &mut findings);
    check_people_001_people_relationship(&state, &mut findings);

    // Namespace / MC checks and annotation/structure checks on each story part.
    check_ns_all_stories(&state, &mut findings);
    check_annotations_and_structure(&state, &mut findings);

    // Cross-part reference checks.
    check_cross_part_references(&state, &mut findings);

    DocxValidation { findings }
}

// =============================================================================
// Namespace / MC checks across all stories
// =============================================================================

fn check_ns_all_stories(state: &PackageState, findings: &mut Vec<ValidationFinding>) {
    use crate::docx_validate_namespaces::{
        check_mc_ignorable_coverage, check_namespace_declarations, collect_namespace_usage,
    };

    for (part_name, root) in &state.story_parts {
        // One traversal per part feeds both namespace checks.
        let usage = collect_namespace_usage(root);
        findings.extend(check_mc_ignorable_coverage(part_name, root, &usage));
        findings.extend(check_namespace_declarations(part_name, &usage));
    }
}

fn check_annotations_and_structure(state: &PackageState, findings: &mut Vec<ValidationFinding>) {
    use crate::docx_validate_annotations::{
        check_annotation_id_uniqueness, check_bookmark_name_length, check_bookmark_pairing,
        check_colfirst_collast_pairing, check_comment_marker_pairing, check_comment_range_count,
        check_custom_xml_range_pairing, check_document_root, check_footnote_endnote_id_range,
        check_no_nested_tracked_changes, check_omath_placement, check_para_id_range,
        check_perm_id_validity, check_tracked_change_content_model,
    };
    use crate::docx_validate_ordering::check_element_ordering;

    let story_refs: Vec<(String, &xmltree::Element)> = state
        .story_parts
        .iter()
        .map(|(name, root)| (name.clone(), root))
        .collect();

    // Annotation checks across all stories.
    findings.extend(check_annotation_id_uniqueness(&story_refs));
    findings.extend(check_bookmark_name_length(&story_refs));
    findings.extend(check_bookmark_pairing(&story_refs));
    findings.extend(check_comment_marker_pairing(&story_refs));
    findings.extend(check_custom_xml_range_pairing(&story_refs));
    findings.extend(check_para_id_range(&story_refs));
    findings.extend(check_tracked_change_content_model(&story_refs));
    findings.extend(check_footnote_endnote_id_range(&story_refs));
    findings.extend(check_no_nested_tracked_changes(&story_refs));
    findings.extend(check_comment_range_count(&story_refs));
    findings.extend(check_omath_placement(&story_refs));
    findings.extend(check_perm_id_validity(&story_refs));
    findings.extend(check_colfirst_collast_pairing(&story_refs));

    // Element ordering checks across all stories.
    findings.extend(check_element_ordering(&story_refs));

    // Document structure check on the resolved main document part.
    if let Some(main_part) = &state.main_part
        && let Some(doc_root) = state
            .story_parts
            .iter()
            .find(|(name, _)| name == main_part)
            .map(|(_, root)| root)
    {
        findings.extend(check_document_root(doc_root));
    }
}

// =============================================================================
// Cross-part reference checks
// =============================================================================

fn check_cross_part_references(state: &PackageState, findings: &mut Vec<ValidationFinding>) {
    use crate::docx_validate_xref::{
        check_xref_001_style_ids, check_xref_002_num_ids, check_xref_003_comment_ids,
        check_xref_004_style_count, check_xref_005_style_id_length,
    };

    findings.extend(check_xref_001_style_ids(state));
    findings.extend(check_xref_002_num_ids(state));
    findings.extend(check_xref_003_comment_ids(state));
    findings.extend(check_xref_004_style_count(state));
    findings.extend(check_xref_005_style_id_length(state));
}

// =============================================================================
// Package state construction
// =============================================================================

fn build_package_state(
    zip: &mut ZipArchive<Cursor<&[u8]>>,
    findings: &mut Vec<ValidationFinding>,
) -> PackageState {
    let mut part_names = HashSet::new();
    let mut file_contents: HashMap<String, Vec<u8>> = HashMap::new();

    for i in 0..zip.len() {
        let mut file = match zip.by_index(i) {
            Ok(f) => f,
            Err(e) => {
                findings.push(ValidationFinding {
                    rule_id: "I-PKG-000",
                    severity: ValidationSeverity::Error,
                    message: format!("cannot read ZIP entry {i}: {e}"),
                    location: "(package)".to_string(),
                });
                continue;
            }
        };
        let name = file.name().to_string();
        if name.ends_with('/') {
            // Directory entries -- skip.
            continue;
        }
        // OPC §7.3: ZIP item names shall be unique; names differing only in
        // ASCII case are equivalent (OPC §6.2). Word reports corruption.
        if let Some(existing) = part_names
            .iter()
            .find(|n: &&String| n.eq_ignore_ascii_case(&name))
        {
            findings.push(ValidationFinding {
                rule_id: "I-PKG-003",
                severity: ValidationSeverity::Error,
                message: format!(
                    "duplicate ZIP part name {name:?} (case-equivalent to {existing:?}); \
                     part names must be unique (OPC §6.2, §7.3)"
                ),
                location: name.clone(),
            });
        }
        part_names.insert(name.clone());

        // Read content for parts we need to inspect.
        let needs_content = name == "[Content_Types].xml"
            || name.ends_with(".rels")
            || name == "word/document.xml"
            || (name.starts_with("word/") && name.ends_with(".xml"));

        if needs_content {
            let mut data = Vec::new();
            use std::io::Read;
            if let Err(e) = file.read_to_end(&mut data) {
                findings.push(ValidationFinding {
                    rule_id: "I-PKG-000",
                    severity: ValidationSeverity::Error,
                    message: format!("cannot read ZIP entry {name:?}: {e}"),
                    location: name.clone(),
                });
            } else {
                file_contents.insert(name.clone(), data);
            }
        }
    }

    // Parse [Content_Types].xml
    let content_types_xml = file_contents
        .get("[Content_Types].xml")
        .and_then(|data| parse_xml(data, "[Content_Types].xml", findings));

    // Parse all .rels files
    let mut rels_files = HashMap::new();
    for name in &part_names {
        if name.ends_with(".rels")
            && let Some(data) = file_contents.get(name)
            && let Some(root) = parse_xml(data, name, findings)
        {
            let rels = parse_relationships(&root);
            rels_files.insert(name.clone(), rels);
        }
    }

    // Parse styles.xml and numbering.xml once (parse failure -> I-XML-001).
    let styles_root = file_contents
        .remove("word/styles.xml")
        .and_then(|data| parse_xml(&data, "word/styles.xml", findings));
    let numbering_root = file_contents
        .remove("word/numbering.xml")
        .and_then(|data| parse_xml(&data, "word/numbering.xml", findings));

    // Locate the main document part via the officeDocument relationship in
    // _rels/.rels (OPC §9.3) — its name is not fixed at word/document.xml.
    let main_part = resolve_main_part(&part_names, &rels_files);

    // Parse story parts once: the main document part plus any word/*.xml that
    // could be a story part. Sorted so findings come out in a deterministic
    // order. Part-name matching is ASCII case-insensitive (OPC §6.2).
    let mut story_names: Vec<String> = part_names
        .iter()
        .filter(|name| {
            main_part
                .as_deref()
                .is_some_and(|m| name.eq_ignore_ascii_case(m))
                || is_story_part_name(name)
        })
        .cloned()
        .collect();
    story_names.sort();
    let mut story_parts = Vec::with_capacity(story_names.len());
    for name in story_names {
        if let Some(data) = file_contents.remove(&name) {
            // An empty (0-byte / whitespace-only) header or footer part is a
            // Word-tolerated empty running head, not a malformed part: Word
            // emits it and opens it without error. It has no root to inspect
            // and no reference resolves to content, so skip it rather than
            // raising an I-XML-001 Error — which is defined as "Word rejects or
            // loses data", and Word does neither here. A header/footer part
            // that has content but no root is still malformed and flagged.
            if is_empty_running_head_part(&name, &data) {
                continue;
            }
            if let Some(root) = parse_xml(&data, &name, findings) {
                story_parts.push((name, root));
            }
        }
    }

    PackageState {
        part_names,
        content_types_xml,
        rels_files,
        story_parts,
        styles_root,
        numbering_root,
        main_part,
    }
}

/// Locate the main document part from parsed package state: follow the
/// officeDocument relationship in `_rels/.rels` (OPC §9.3). Returns `None` when
/// the root relationships part is absent, the relationship is missing, its
/// target is External, or the resolved part is not present — each of which
/// leaves the package without a discoverable main part (reported by I-PKG-002).
fn resolve_main_part(
    part_names: &HashSet<String>,
    rels_files: &HashMap<String, Vec<ParsedRelationship>>,
) -> Option<String> {
    let (_, root_rels) = rels_files
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("_rels/.rels"))?;
    let rel = root_rels
        .iter()
        .find(|r| r.rel_type == crate::docx_package::OFFICE_DOCUMENT_REL_TYPE)?;
    if rel.target_mode.as_deref() == Some("External") {
        return None;
    }
    let part_name = crate::docx_package::normalize_package_path(&rel.target);
    part_names
        .iter()
        .any(|n| n.eq_ignore_ascii_case(&part_name))
        .then_some(part_name)
}

/// True for an empty (0-byte / whitespace-only) `word/headerN.xml` or
/// `word/footerN.xml` part. Word emits this shape for an empty running head and
/// opens it without error, so it is tolerated — not flagged as malformed. The
/// exemption is narrow: only header/footer parts, and only when they carry no
/// content at all (a header/footer part with content but no root is malformed).
fn is_empty_running_head_part(name: &str, data: &[u8]) -> bool {
    let Some(filename) = name.strip_prefix("word/") else {
        return false;
    };
    (filename.starts_with("header") || filename.starts_with("footer"))
        && crate::word_xml::is_empty_or_whitespace_xml(data)
}

/// Check if a part name looks like a story part (header, footer, footnotes, endnotes, comments).
fn is_story_part_name(name: &str) -> bool {
    let Some(filename) = name.strip_prefix("word/") else {
        return false;
    };
    filename.starts_with("header")
        || filename.starts_with("footer")
        || filename == "footnotes.xml"
        || filename == "endnotes.xml"
        || filename == "comments.xml"
}

// =============================================================================
// XML helpers
// =============================================================================

/// Parse one part's XML, recording an I-XML-001 Error on failure.
///
/// This is the soundness rule every part-level check relies on: a part that is
/// not well-formed cannot be inspected, so the parse failure itself must be a
/// hard finding — otherwise a corrupt part would validate "clean" precisely
/// because it is corrupt.
fn parse_xml(
    bytes: &[u8],
    part_name: &str,
    findings: &mut Vec<ValidationFinding>,
) -> Option<Element> {
    let config = ParserConfig::new()
        .ignore_comments(true)
        .whitespace_to_characters(true);
    match Element::parse_with_config(Cursor::new(bytes), config) {
        Ok(el) => Some(el),
        Err(e) => {
            findings.push(ValidationFinding {
                rule_id: "I-XML-001",
                severity: ValidationSeverity::Error,
                message: format!("part is not well-formed XML: {e}"),
                location: part_name.to_string(),
            });
            None
        }
    }
}

fn local_element_name(element: &Element) -> &str {
    match element.name.rsplit_once(':') {
        Some((_, local)) => local,
        None => &element.name,
    }
}

fn get_attr<'a>(element: &'a Element, local: &str) -> Option<&'a str> {
    for (name, value) in &element.attributes {
        if name.local_name == local {
            return Some(value.as_str());
        }
    }
    None
}

fn parse_relationships(root: &Element) -> Vec<ParsedRelationship> {
    let mut rels = Vec::new();
    for child in &root.children {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };
        if local_element_name(el) != "Relationship" {
            continue;
        }
        let Some(id) = get_attr(el, "Id") else {
            continue;
        };
        let Some(target) = get_attr(el, "Target") else {
            continue;
        };
        let rel_type = get_attr(el, "Type").unwrap_or("").to_string();
        let target_mode = get_attr(el, "TargetMode").map(|s| s.to_string());
        rels.push(ParsedRelationship {
            id: id.to_string(),
            rel_type,
            target: target.to_string(),
            target_mode,
        });
    }
    rels
}

// =============================================================================
// I-PKG-001: Package relationships part exists
// =============================================================================

fn check_pkg_001_rels_exists(state: &PackageState, findings: &mut Vec<ValidationFinding>) {
    if !state.part_names.contains("_rels/.rels") {
        findings.push(ValidationFinding {
            rule_id: "I-PKG-001",
            severity: ValidationSeverity::Error,
            message: "package relationships part _rels/.rels is missing".to_string(),
            location: "_rels/.rels".to_string(),
        });
    }
}

// =============================================================================
// I-PKG-002: document.xml exists
// =============================================================================

fn check_pkg_002_document_exists(state: &PackageState, findings: &mut Vec<ValidationFinding>) {
    // The main document part is located by the officeDocument relationship in
    // _rels/.rels (OPC §9.3), NOT by a fixed name. A package with no
    // discoverable main part (missing relationship, External target, or a
    // target that resolves to an absent part) cannot be opened.
    if state.main_part.is_none() {
        findings.push(ValidationFinding {
            rule_id: "I-PKG-002",
            severity: ValidationSeverity::Error,
            message: "no main document part could be located via the officeDocument relationship \
                      in _rels/.rels (OPC §9.3)"
                .to_string(),
            location: "_rels/.rels".to_string(),
        });
    }
}

// =============================================================================
// I-CT-001: Every part has a content type
// =============================================================================

fn check_ct_001_content_types(state: &PackageState, findings: &mut Vec<ValidationFinding>) {
    let Some(ct_root) = &state.content_types_xml else {
        // If [Content_Types].xml is missing, that's already a problem.
        findings.push(ValidationFinding {
            rule_id: "I-CT-001",
            severity: ValidationSeverity::Error,
            message: "[Content_Types].xml is missing from the package".to_string(),
            location: "[Content_Types].xml".to_string(),
        });
        return;
    };

    // Collect Default extensions and Override part names.
    let mut default_extensions: HashSet<String> = HashSet::new();
    let mut override_parts: HashSet<String> = HashSet::new();

    for child in &ct_root.children {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };
        let local = local_element_name(el);
        match local {
            "Default" => {
                if let Some(ext) = get_attr(el, "Extension") {
                    default_extensions.insert(ext.to_lowercase());
                }
            }
            "Override" => {
                if let Some(part) = get_attr(el, "PartName") {
                    // PartName in [Content_Types].xml uses leading slash: "/word/document.xml".
                    // Stored lowercased: Override PartName matching is ASCII
                    // case-insensitive (OPC §7.2).
                    let normalized = part.strip_prefix('/').unwrap_or(part);
                    override_parts.insert(normalized.to_ascii_lowercase());
                }
            }
            _ => {}
        }
    }

    // Check every part in the ZIP (excluding _rels parts and [Content_Types].xml itself).
    for part_name in &state.part_names {
        if part_name == "[Content_Types].xml" || part_name.contains("_rels/") {
            continue;
        }

        // Check if covered by an Override (ASCII case-insensitive, OPC §7.2).
        if override_parts.contains(&part_name.to_ascii_lowercase()) {
            continue;
        }

        // Check if covered by a Default extension.
        let ext = part_name
            .rsplit_once('.')
            .map(|(_, e)| e.to_lowercase())
            .unwrap_or_default();
        if !ext.is_empty() && default_extensions.contains(&ext) {
            continue;
        }

        findings.push(ValidationFinding {
            rule_id: "I-CT-001",
            severity: ValidationSeverity::Error,
            message: format!(
                "part {part_name:?} has no content type (no matching Default extension or Override)"
            ),
            location: "[Content_Types].xml".to_string(),
        });
    }
}

// =============================================================================
// I-CT-002: WordprocessingML parts carry their canonical content type
// =============================================================================

/// Every recognized WML part present in the package must be content-typed with
/// its canonical type via an `Override`, NOT merely covered by the generic
/// `Default Extension="xml"` (`application/xml`).
///
/// ECMA-376 Part 1 §15.2 fixes the content type of each WML part, and Word
/// locates parts such as `word/comments.xml`, `word/footnotes.xml`, and the
/// style/numbering tables *by content type*. A part that resolves only through
/// the `xml` Default is not recognized as its WML role: Word reports
/// "unreadable content" and drops the part on repair. That is a hard
/// data-loss / repair class, so a missing-or-wrong Override here is an
/// **Error** (and a blocking rule). I-CT-001 only checks that *some* content
/// type covers each part; this rule checks it is the *correct* one.
fn check_ct_002_canonical_wml_content_types(
    state: &PackageState,
    findings: &mut Vec<ValidationFinding>,
) {
    let Some(ct_root) = &state.content_types_xml else {
        // Absent [Content_Types].xml is already reported by I-CT-001.
        return;
    };

    // Collect declared Overrides as part_name (no leading slash) -> content type.
    let mut overrides: HashMap<String, String> = HashMap::new();
    for child in &ct_root.children {
        let XMLNode::Element(el) = child else {
            continue;
        };
        if local_element_name(el) != "Override" {
            continue;
        }
        let (Some(part), Some(ct)) = (get_attr(el, "PartName"), get_attr(el, "ContentType")) else {
            continue;
        };
        let normalized = part.strip_prefix('/').unwrap_or(part).to_string();
        overrides.insert(normalized, ct.to_string());
    }

    for part_name in &state.part_names {
        let Some(expected) = crate::docx_package::canonical_wml_content_type(part_name) else {
            continue;
        };
        // The main document part legitimately carries any of the four WML
        // main-part content types (document/template x plain/macroEnabled) —
        // Word opens all four; only a type outside the family is a defect.
        // The scaffold/merge paths preserve an existing variant rather than
        // rewriting it (that would silently change the document's kind).
        let accepted: &[&str] = if state.main_part.as_deref() == Some(part_name.as_str()) {
            MAIN_DOCUMENT_CONTENT_TYPE_FAMILY
        } else {
            std::slice::from_ref(&expected)
        };
        match overrides.get(part_name.as_str()) {
            None => {
                findings.push(ValidationFinding {
                    rule_id: "I-CT-002",
                    severity: ValidationSeverity::Error,
                    message: format!(
                        "WML part {part_name:?} has no content-type Override; it must be declared \
                         as {expected:?} (a generic xml Default is not enough — Word drops the part \
                         on repair)"
                    ),
                    location: "[Content_Types].xml".to_string(),
                });
            }
            Some(actual) if !accepted.contains(&actual.as_str()) => {
                findings.push(ValidationFinding {
                    rule_id: "I-CT-002",
                    severity: ValidationSeverity::Error,
                    message: format!(
                        "WML part {part_name:?} is content-typed as {actual:?}, expected {expected:?}"
                    ),
                    location: "[Content_Types].xml".to_string(),
                });
            }
            Some(_) => {} // a canonical content type for this part
        }
    }

    // The main document part's content type is fixed (ECMA-376 Part 1 §15.2)
    // regardless of its non-fixed name. A non-conventional main part name (e.g.
    // word/document2.xml) is skipped by the filename-keyed sweep above, so
    // verify its main-family Override here — a serializer that omits or
    // mislabels it must still be caught (Word drops the part on repair).
    if let Some(main_part) = &state.main_part
        && crate::docx_package::canonical_wml_content_type(main_part).is_none()
    {
        let main_ct = MAIN_DOCUMENT_CONTENT_TYPE_FAMILY[0];
        // Override PartName comparison is ASCII case-insensitive (OPC §7.2.3):
        // a part stored as `word/Document.xml` is content-typed by an Override
        // `/word/document.xml`, so match case-insensitively.
        let main_override = overrides
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(main_part))
            .map(|(_, ct)| ct);
        match main_override {
            None => findings.push(ValidationFinding {
                rule_id: "I-CT-002",
                severity: ValidationSeverity::Error,
                message: format!(
                    "main document part {main_part:?} has no content-type Override; it must be \
                     declared as {main_ct:?} (a generic xml Default is not enough — Word drops the \
                     part on repair)"
                ),
                location: "[Content_Types].xml".to_string(),
            }),
            Some(actual) if !MAIN_DOCUMENT_CONTENT_TYPE_FAMILY.contains(&actual.as_str()) => {
                findings.push(ValidationFinding {
                    rule_id: "I-CT-002",
                    severity: ValidationSeverity::Error,
                    message: format!(
                        "main document part {main_part:?} is content-typed as {actual:?}, expected \
                         a WordprocessingML main-document type"
                    ),
                    location: "[Content_Types].xml".to_string(),
                });
            }
            Some(_) => {}
        }
    }
}

/// The four content types Word accepts for the main document part
/// (document/template x plain/macro-enabled). Word opens all four; only a type
/// outside this family is a defect.
const MAIN_DOCUMENT_CONTENT_TYPE_FAMILY: &[&str] = &[
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.template.main+xml",
    "application/vnd.ms-word.document.macroEnabled.main+xml",
    "application/vnd.ms-word.template.macroEnabledTemplate.main+xml",
];

// =============================================================================
// I-REL-001: Every r:id/r:embed/r:link reference resolves
// =============================================================================

fn check_rel_001_rid_references(state: &PackageState, findings: &mut Vec<ValidationFinding>) {
    // For each story part, collect all relationship references and verify they
    // resolve against the part's .rels file. The per-part comparison lives in
    // `rel_dangling_findings`, which the production validate path
    // (runtime::validate_docx_report, via `check_story_rel_references`) shares,
    // so the two validators cannot drift on what counts as a dangling reference.
    for (part_name, root) in &state.story_parts {
        let mut referenced_ids: Vec<String> = Vec::new();
        collect_relationship_references(root, &mut referenced_ids);
        if referenced_ids.is_empty() {
            continue;
        }
        let rels_path = rels_path_for_part(part_name);
        let available = state.rels_files.get(&rels_path).map(|v| v.as_slice());
        findings.extend(rel_dangling_findings(
            part_name,
            &rels_path,
            &referenced_ids,
            available,
        ));
    }
}

/// Per-part I-REL-001 comparison: report every referenced relationship Id that
/// fails to resolve. `available` is `None` when the part has no `.rels` file at
/// all (every non-empty reference is then dangling), or `Some(rels)` listing the
/// relationships the part's `.rels` declares.
///
/// Single source of truth shared by the rich PackageState validator and the
/// production validate path.
fn rel_dangling_findings(
    part_name: &str,
    rels_path: &str,
    referenced_ids: &[String],
    available: Option<&[ParsedRelationship]>,
) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();
    match available {
        None => {
            // No .rels file but references exist -- all are broken.
            for rid in referenced_ids {
                findings.push(ValidationFinding {
                    rule_id: "I-REL-001",
                    severity: ValidationSeverity::Error,
                    message: format!(
                        "relationship reference {rid:?} in {part_name:?} has no .rels file ({rels_path:?})"
                    ),
                    location: part_name.to_string(),
                });
            }
        }
        Some(rels) => {
            let available_ids: HashSet<&str> = rels.iter().map(|r| r.id.as_str()).collect();
            for rid in referenced_ids {
                if !available_ids.contains(rid.as_str()) {
                    findings.push(ValidationFinding {
                        rule_id: "I-REL-001",
                        severity: ValidationSeverity::Error,
                        message: format!(
                            "relationship reference {rid:?} not found in {rels_path:?}"
                        ),
                        location: part_name.to_string(),
                    });
                }
            }
        }
    }
    findings
}

/// I-REL-001 for a single story part on the production validate path.
///
/// `runtime::validate_docx_report` runs a curated structural subset rather than
/// the full PackageState validator. This is the single entry point it uses, so
/// dangling-reference detection stays identical to the rich validator: collect
/// the r:id/r:embed/r:link references on `root`, parse the part's `.rels` bytes,
/// and report any non-empty reference that does not resolve. Word repairs such a
/// file and drops the referenced content (ECMA-376 Part 2 OPC §6.5.3;
/// ISO 29500-1 §9.2).
pub(crate) fn check_story_rel_references(
    part_name: &str,
    root: &Element,
    rels_bytes: Option<&[u8]>,
) -> Vec<ValidationFinding> {
    let mut referenced_ids: Vec<String> = Vec::new();
    collect_relationship_references(root, &mut referenced_ids);
    if referenced_ids.is_empty() {
        return Vec::new();
    }
    let rels_path = rels_path_for_part(part_name);
    // A present-but-malformed .rels resolves to an empty relationship set, so
    // every reference is dangling -- never silently "ok" because the .rels could
    // not be parsed. An absent .rels part stays `None` (the "no .rels file" arm).
    let parsed = rels_bytes.map(|bytes| {
        Element::parse(bytes)
            .map(|r| parse_relationships(&r))
            .unwrap_or_default()
    });
    rel_dangling_findings(part_name, &rels_path, &referenced_ids, parsed.as_deref())
}

// =============================================================================
// I-REL-004: A CT_Rel-derived reference requires its r:id attribute
// =============================================================================

/// `w:headerReference` / `w:footerReference` (CT_HdrFtrRef extends CT_Rel) make
/// `r:id` a REQUIRED attribute. A reference whose `r:id` is ABSENT or EMPTY names
/// no header/footer part, so the relationship cannot be resolved and Word reports
/// the document non-conformant. ISO 29500-1 §17.10.5; ECMA-376 Annex A
/// CT_HdrFtrRef / CT_Rel; ECMA-376 Part 2 OPC §6.5.
///
/// Both cases are confirmed against real Word:
/// an ABSENT `r:id` → Word "cannot open the file" (hard failure); an EMPTY
/// `r:id=""` → Word opens with REPAIR. Both are non-conformant. Note this is the
/// OPPOSITE of empty `r:embed`/`r:link` on `a:blip`, which Word writes as a
/// conformant empty placeholder (preserved in [`rel_dangling_findings`]): the
/// difference is that `r:id` is *required* on CT_HdrFtrRef — the reference is the
/// whole point of the element — whereas the blip attributes are optional. The
/// earlier "empty is schema-valid, leave it clean" hypothesis was overturned by
/// the oracle; resolution of a *present, non-empty* id remains I-REL-001's job.
fn check_rel_004_hdrftr_ref_requires_rid(
    state: &PackageState,
    findings: &mut Vec<ValidationFinding>,
) {
    for (part_name, root) in &state.story_parts {
        collect_hdrftr_refs_missing_rid(part_name, root, findings);
    }
}

/// Per-part walk for I-REL-004, shared by the rich validator and the production
/// path (via [`check_story_hdrftr_ref_rid`]) so the two cannot drift.
fn collect_hdrftr_refs_missing_rid(
    part_name: &str,
    element: &Element,
    findings: &mut Vec<ValidationFinding>,
) {
    let local = local_element_name(element);
    if (local == "headerReference" || local == "footerReference")
        && !has_nonempty_relationship_id(element)
    {
        findings.push(ValidationFinding {
            rule_id: "I-REL-004",
            severity: ValidationSeverity::Error,
            message: format!(
                "{local} in {part_name:?} has no usable r:id (CT_HdrFtrRef extends CT_Rel, r:id use=\"required\"): it is absent or empty, so the header/footer relationship cannot be resolved"
            ),
            location: part_name.to_string(),
        });
    }
    for child in &element.children {
        if let XMLNode::Element(el) = child {
            collect_hdrftr_refs_missing_rid(part_name, el, findings);
        }
    }
}

/// True iff the element carries a PRESENT, NON-EMPTY `r:id` attribute
/// (relationships namespace or `r:` prefix, or the legacy `r:id` local form).
/// `use="required"` demands presence, and real Word confirms an empty value is
/// equally non-conformant for CT_HdrFtrRef (Word repairs it).
fn has_nonempty_relationship_id(element: &Element) -> bool {
    element.attributes.iter().any(|(name, value)| {
        let local = name.local_name.as_str();
        let is_rid = (local == "id"
            && (name.namespace.as_deref() == Some(REL_ATTR_NS)
                || name.prefix.as_deref() == Some("r")))
            || local == "r:id";
        is_rid && !value.is_empty()
    })
}

/// I-REL-004 for a single story part on the production validate path.
///
/// `runtime::validate_docx_report` runs a curated structural subset rather than
/// the full `PackageState` validator; this is the per-root entry point it uses,
/// so the production path and the rich validator share the same logic.
pub(crate) fn check_story_hdrftr_ref_rid(
    part_name: &str,
    root: &Element,
) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();
    collect_hdrftr_refs_missing_rid(part_name, root, &mut findings);
    findings
}

/// Collect all relationship reference attribute values from an XML element tree.
///
/// Looks for `r:id`, `r:embed`, `r:link` attributes (both prefixed and namespace-qualified).
fn collect_relationship_references(element: &Element, out: &mut Vec<String>) {
    for (attr_name, value) in &element.attributes {
        let is_rel_ref = is_relationship_reference_attr(attr_name);
        if is_rel_ref && !value.is_empty() {
            out.push(value.clone());
        }
    }
    for child in &element.children {
        if let XMLNode::Element(el) = child {
            collect_relationship_references(el, out);
        }
    }
}

/// Check if an attribute name is a relationship reference (r:id, r:embed, r:link).
fn is_relationship_reference_attr(name: &xmltree::AttributeName) -> bool {
    let local = name.local_name.as_str();
    // Match by local name if the namespace is the relationships namespace.
    if let Some(ns) = &name.namespace
        && ns == REL_ATTR_NS
        && (local == "id" || local == "embed" || local == "link")
    {
        return true;
    }
    // Match by prefix: r:id, r:embed, r:link
    if let Some(prefix) = &name.prefix
        && prefix == "r"
        && (local == "id" || local == "embed" || local == "link")
    {
        return true;
    }
    // Match fully-qualified legacy forms like "r:id" stored as local_name
    if local == "r:id" || local == "r:embed" || local == "r:link" {
        return true;
    }
    false
}

/// Compute the .rels path for a given part.
/// e.g., "word/document.xml" -> "word/_rels/document.xml.rels"
fn rels_path_for_part(part: &str) -> String {
    match part.rsplit_once('/') {
        Some((dir, filename)) => format!("{dir}/_rels/{filename}.rels"),
        None => format!("_rels/{part}.rels"),
    }
}

// =============================================================================
// I-REL-002: Relationship ID uniqueness
// =============================================================================

fn check_rel_002_id_uniqueness(state: &PackageState, findings: &mut Vec<ValidationFinding>) {
    for (rels_path, rels) in &state.rels_files {
        let mut seen: HashMap<&str, usize> = HashMap::new();
        for rel in rels {
            *seen.entry(rel.id.as_str()).or_insert(0) += 1;
        }
        for (id, count) in &seen {
            if *count > 1 {
                findings.push(ValidationFinding {
                    rule_id: "I-REL-002",
                    severity: ValidationSeverity::Error,
                    message: format!(
                        "relationship Id {id:?} appears {count} times (must be unique)"
                    ),
                    location: rels_path.clone(),
                });
            }
        }
    }
}

// =============================================================================
// I-REL-003: Internal relationship targets resolve
// =============================================================================

fn check_rel_003_internal_targets(state: &PackageState, findings: &mut Vec<ValidationFinding>) {
    for (rels_path, rels) in &state.rels_files {
        // Determine the base directory for resolving relative targets.
        // e.g., "word/_rels/document.xml.rels" -> source part is in "word/"
        let base_dir = rels_base_dir(rels_path);

        for rel in rels {
            // Skip external relationships.
            if rel
                .target_mode
                .as_deref()
                .is_some_and(|m| m.eq_ignore_ascii_case("external"))
            {
                continue;
            }

            // A fragment-only target ("#BookmarkName") resolves to the source
            // part itself (RFC 3986 §4.4), not to a package part — old Word
            // files emit bookmark hyperlinks this way. Nothing to resolve.
            if rel.target.starts_with('#') {
                continue;
            }

            let resolved = resolve_target(&base_dir, &rel.target);

            // OPC part-name equivalence is ASCII case-insensitive (ECMA-376
            // Part 2 §9.1): a target resolving to `customXml/item1.xml` is
            // satisfied by a stored `customXML/item1.xml` and must not be
            // reported as missing.
            if !state.contains_part_ci(&resolved) {
                findings.push(ValidationFinding {
                    rule_id: "I-REL-003",
                    severity: ValidationSeverity::Error,
                    message: format!(
                        "relationship {id:?} target {target:?} resolves to {resolved:?} which does not exist in the package",
                        id = rel.id,
                        target = rel.target,
                    ),
                    location: rels_path.clone(),
                });
            }
        }
    }
}

/// Compute the base directory for resolving targets from a .rels file.
///
/// The .rels file at `{dir}/_rels/{filename}.rels` corresponds to the part
/// `{dir}/{filename}`, so relative targets resolve from `{dir}/`.
fn rels_base_dir(rels_path: &str) -> String {
    // e.g., "word/_rels/document.xml.rels" -> strip "_rels/document.xml.rels" -> "word/"
    // e.g., "_rels/.rels" -> strip "_rels/.rels" -> ""
    if let Some(idx) = rels_path.find("_rels/") {
        rels_path[..idx].to_string()
    } else {
        String::new()
    }
}

/// Resolve a relationship target relative to a base directory.
fn resolve_target(base_dir: &str, target: &str) -> String {
    if target.starts_with('/') {
        // Absolute target -- strip leading slash.
        return target.strip_prefix('/').unwrap_or(target).to_string();
    }

    // Relative target -- resolve from base directory.
    let combined = format!("{base_dir}{target}");

    // Normalize path segments (handle "../" etc.)
    normalize_path(&combined)
}

/// Simple path normalization: resolve `.` and `..` segments.
fn normalize_path(path: &str) -> String {
    let mut segments: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            _ => segments.push(seg),
        }
    }
    segments.join("/")
}

// =============================================================================
// I-STORY-001: Story parts have corresponding relationships
// =============================================================================

fn check_story_001_story_relationships(
    state: &PackageState,
    findings: &mut Vec<ValidationFinding>,
) {
    let doc_rels_path = "word/_rels/document.xml.rels";
    let doc_rels = state.rels_files.get(doc_rels_path);

    // Build a set of resolved target paths from document.xml.rels.
    let rel_targets: HashSet<String> = doc_rels
        .map(|rels| {
            rels.iter()
                .map(|r| resolve_target("word/", &r.target))
                .collect()
        })
        .unwrap_or_default();

    // Check each story part pattern.
    let story_checks: &[(&str, &[&str])] = &[
        ("header", &[HEADER_REL_TYPE]),
        ("footer", &[FOOTER_REL_TYPE]),
        ("footnotes.xml", &[FOOTNOTES_REL_TYPE]),
        ("endnotes.xml", &[ENDNOTES_REL_TYPE]),
        ("comments.xml", &[COMMENTS_REL_TYPE]),
    ];

    for part_name in &state.part_names {
        let Some(filename) = part_name.strip_prefix("word/") else {
            continue;
        };

        for (pattern, expected_types) in story_checks {
            let matches = if pattern.contains('.') {
                // Exact filename match (e.g., "footnotes.xml")
                filename == *pattern
            } else {
                // Prefix match (e.g., "header" matches "header1.xml", "header2.xml")
                filename.starts_with(pattern) && filename.ends_with(".xml")
            };

            if !matches {
                continue;
            }

            // Check if this part is referenced by any relationship.
            if !rel_targets.contains(part_name.as_str()) {
                // Also check by relationship type: maybe the target string differs
                // (e.g., relative vs absolute). Check if any rel of the right type
                // resolves to this part.
                let has_matching_rel = doc_rels
                    .map(|rels| {
                        rels.iter().any(|r| {
                            expected_types.contains(&r.rel_type.as_str())
                                && resolve_target("word/", &r.target) == *part_name
                        })
                    })
                    .unwrap_or(false);

                if !has_matching_rel {
                    findings.push(ValidationFinding {
                        rule_id: "I-STORY-001",
                        severity: ValidationSeverity::Warning,
                        message: format!(
                            "story part {part_name:?} exists but has no corresponding relationship in {doc_rels_path}"
                        ),
                        location: doc_rels_path.to_string(),
                    });
                }
            }
        }
    }
}

// =============================================================================
// I-PEOPLE-001: people.xml has relationship if present
// =============================================================================

fn check_people_001_people_relationship(
    state: &PackageState,
    findings: &mut Vec<ValidationFinding>,
) {
    if !state.part_names.contains("word/people.xml") {
        return;
    }

    let doc_rels_path = "word/_rels/document.xml.rels";
    let has_people_rel = state
        .rels_files
        .get(doc_rels_path)
        .map(|rels| rels.iter().any(|r| r.rel_type == PEOPLE_REL_TYPE))
        .unwrap_or(false);

    if !has_people_rel {
        findings.push(ValidationFinding {
            rule_id: "I-PEOPLE-001",
            severity: ValidationSeverity::Warning,
            message: "word/people.xml exists but has no corresponding relationship in word/_rels/document.xml.rels".to_string(),
            location: doc_rels_path.to_string(),
        });
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(
        rule_id: &'static str,
        severity: ValidationSeverity,
        location: &str,
    ) -> ValidationFinding {
        ValidationFinding {
            rule_id,
            severity,
            message: format!("{rule_id}: detail"),
            location: location.to_string(),
        }
    }

    // -----------------------------------------------------------------------
    // advisory_summary: honest presentation of non-blocking findings
    // -----------------------------------------------------------------------

    /// A finding the gate does not refuse on is advisory on that path — Word
    /// opens the file with no data loss — so it must not be presented with the
    /// blocking-failure "ERROR" token, and a blocking finding must not appear
    /// in the advisory summary at all. Domain: only the `blocking` rule set
    /// causes refusal; a non-blocking Error-severity finding (e.g. I-ANN-001,
    /// a duplicate annotation id — non-conformant per ECMA-376 yet tolerated
    /// by Word) is not a failure of the operation that produced the bytes.
    #[test]
    fn advisory_summary_excludes_blocking_and_drops_error_label() {
        let validation = DocxValidation {
            findings: vec![
                finding("I-TC-001", ValidationSeverity::Error, "word/document.xml"),
                finding("I-ANN-001", ValidationSeverity::Error, "word/document.xml"),
            ],
        };
        let lines = validation.advisory_summary(&["I-TC-001"]);
        assert_eq!(lines.len(), 1, "blocking I-TC-001 must be excluded");
        assert!(lines[0].starts_with("[I-ANN-001]"));
        assert!(
            !lines.iter().any(|l| l.contains("ERROR")),
            "advisory findings must not carry the blocking ERROR label: {lines:?}"
        );
    }

    /// Repeats of one rule collapse to a single counted line. Domain: the count
    /// plus one representative locate the condition; on a path that does not
    /// block on the finding, enumerating every occurrence adds no
    /// decision-relevant information and only buries the signal.
    #[test]
    fn advisory_summary_collapses_repeats_with_count() {
        let validation = DocxValidation {
            findings: vec![
                finding("I-ANN-001", ValidationSeverity::Error, "word/document.xml"),
                finding("I-ANN-001", ValidationSeverity::Error, "word/header1.xml"),
                finding("I-ANN-001", ValidationSeverity::Error, "word/footer1.xml"),
            ],
        };
        let lines = validation.advisory_summary(&[]);
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].contains("×3"),
            "three findings of one rule collapse to one ×3 line: {}",
            lines[0]
        );
    }

    /// One line per distinct rule, in first-seen order; a lone finding carries
    /// no count suffix.
    #[test]
    fn advisory_summary_one_line_per_rule_first_seen_order() {
        let validation = DocxValidation {
            findings: vec![
                finding("I-ANN-001", ValidationSeverity::Error, "word/document.xml"),
                finding(
                    "I-ANN-002",
                    ValidationSeverity::Warning,
                    "word/document.xml",
                ),
            ],
        };
        let lines = validation.advisory_summary(&[]);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("[I-ANN-001]") && !lines[0].contains('×'));
        assert!(lines[1].starts_with("[I-ANN-002]"));
    }

    #[test]
    fn rels_path_for_document() {
        assert_eq!(
            rels_path_for_part("word/document.xml"),
            "word/_rels/document.xml.rels"
        );
    }

    #[test]
    fn rels_path_for_header() {
        assert_eq!(
            rels_path_for_part("word/header1.xml"),
            "word/_rels/header1.xml.rels"
        );
    }

    #[test]
    fn resolve_relative_target() {
        assert_eq!(resolve_target("word/", "document.xml"), "word/document.xml");
        assert_eq!(resolve_target("word/", "header1.xml"), "word/header1.xml");
    }

    #[test]
    fn resolve_absolute_target() {
        assert_eq!(
            resolve_target("word/", "/word/document.xml"),
            "word/document.xml"
        );
    }

    #[test]
    fn resolve_parent_reference() {
        assert_eq!(
            resolve_target("word/", "../customXml/item1.xml"),
            "customXml/item1.xml"
        );
    }

    #[test]
    fn normalize_path_handles_dots() {
        assert_eq!(
            normalize_path("word/../customXml/item1.xml"),
            "customXml/item1.xml"
        );
        assert_eq!(normalize_path("word/./document.xml"), "word/document.xml");
    }

    #[test]
    fn is_story_part_names() {
        assert!(is_story_part_name("word/header1.xml"));
        assert!(is_story_part_name("word/footer2.xml"));
        assert!(is_story_part_name("word/footnotes.xml"));
        assert!(is_story_part_name("word/endnotes.xml"));
        assert!(is_story_part_name("word/comments.xml"));
        assert!(!is_story_part_name("word/document.xml"));
        assert!(!is_story_part_name("word/styles.xml"));
        assert!(!is_story_part_name("_rels/.rels"));
    }

    #[test]
    fn rels_base_dir_computation() {
        assert_eq!(rels_base_dir("word/_rels/document.xml.rels"), "word/");
        assert_eq!(rels_base_dir("_rels/.rels"), "");
    }

    #[test]
    fn empty_running_head_part_is_only_a_contentless_header_or_footer() {
        // Tolerated: empty (0-byte / whitespace-only) header AND footer parts.
        assert!(is_empty_running_head_part("word/header1.xml", b""));
        assert!(is_empty_running_head_part("word/footer2.xml", b"  \r\n"));
        // Not tolerated: a header/footer part that has content but no root — a
        // truncated/malformed part, still flagged I-XML-001 downstream.
        assert!(!is_empty_running_head_part(
            "word/header1.xml",
            b"<?xml version=\"1.0\"?>"
        ));
        assert!(!is_empty_running_head_part("word/header1.xml", b"<w:hdr/>"));
        // Not tolerated: an empty NON-running-head part (only headers/footers
        // get this exemption).
        assert!(!is_empty_running_head_part("word/document.xml", b""));
        assert!(!is_empty_running_head_part("word/footnotes.xml", b""));
        assert!(!is_empty_running_head_part("word/styles.xml", b""));
    }
}
