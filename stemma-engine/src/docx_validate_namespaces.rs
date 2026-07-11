//! Namespace and Markup Compatibility (MC) validation checks for DOCX parts.
//!
//! These checks verify that:
//! - Extension namespace prefixes used in elements/attributes are listed in `mc:Ignorable`
//! - Every namespace prefix used in element/attribute names has a corresponding `xmlns:` declaration

use std::collections::{HashMap, HashSet};

use xmltree::{Element, XMLNode};

use crate::docx_validate::{ValidationFinding, ValidationSeverity};

// =============================================================================
// Core OOXML namespaces — always understood, never need mc:Ignorable
// =============================================================================

const CORE_OOXML_NAMESPACES: &[&str] = &[
    "http://schemas.openxmlformats.org/wordprocessingml/2006/main", // w:
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships", // r:
    "http://schemas.openxmlformats.org/officeDocument/2006/math",   // m:
    "http://schemas.openxmlformats.org/markup-compatibility/2006",  // mc:
    "http://schemas.openxmlformats.org/drawingml/2006/main",        // a:
    "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing", // wp:
    "http://schemas.openxmlformats.org/drawingml/2006/picture",     // pic:
    "urn:schemas-microsoft-com:vml",                                // v:
    "urn:schemas-microsoft-com:office:office",                      // o:
    "urn:schemas-microsoft-com:office:word",                        // w10:
    "http://www.w3.org/XML/1998/namespace",                         // xml:
    "http://www.w3.org/2000/xmlns/",                                // xmlns:
];

// =============================================================================
// I-NS-001: Extension namespaces used must be in mc:Ignorable
// =============================================================================

/// Namespace facts about one part, collected in a single tree traversal and
/// shared by both checks in this module (the traversal dominates their cost;
/// collecting twice per part used to be the validator's hottest spot).
pub(crate) struct NamespaceUsage {
    /// Every namespace prefix used in element or attribute names.
    pub(crate) used_prefixes: HashSet<String>,
    /// Declared prefixes -> namespace URI (first binding seen wins; these
    /// checks only need "is this prefix bound, and to which family of URI").
    pub(crate) declared: HashMap<String, String>,
}

/// Walk the tree once, collecting used prefixes and declared prefix bindings.
///
/// Declarations are gathered from three sources: each element's namespace map,
/// the element's own resolved `prefix`/`namespace` pair, and attribute
/// `prefix`/`namespace` pairs (if the parser resolved a pair, the declaration
/// existed in scope in the source XML).
pub(crate) fn collect_namespace_usage(root: &Element) -> NamespaceUsage {
    let mut usage = NamespaceUsage {
        used_prefixes: HashSet::new(),
        declared: HashMap::new(),
    };
    collect_namespace_usage_into(root, &mut usage);
    usage
}

fn collect_namespace_usage_into(element: &Element, usage: &mut NamespaceUsage) {
    // Used prefix on the element name.
    if let Some(prefix) = &element.prefix {
        insert_used(&mut usage.used_prefixes, prefix);
    } else if let Some(prefix) = extract_prefix(&element.name) {
        insert_used(&mut usage.used_prefixes, prefix);
    }

    // Declarations on this element's namespace map.
    if let Some(ns) = &element.namespaces {
        for (prefix, uri) in ns {
            insert_declared(&mut usage.declared, prefix, uri);
        }
    }
    // The element's own resolved binding.
    if let (Some(prefix), Some(uri)) = (&element.prefix, &element.namespace) {
        insert_declared(&mut usage.declared, prefix, uri);
    }

    // Attributes: used prefixes and resolved bindings.
    for (attr_name, _) in &element.attributes {
        if let Some(prefix) = &attr_name.prefix {
            insert_used(&mut usage.used_prefixes, prefix);
        } else if let Some(prefix) = extract_prefix(&attr_name.local_name) {
            insert_used(&mut usage.used_prefixes, prefix);
        }
        if let (Some(prefix), Some(uri)) = (&attr_name.prefix, &attr_name.namespace) {
            insert_declared(&mut usage.declared, prefix, uri);
        }
    }

    for child in &element.children {
        if let XMLNode::Element(child_el) = child {
            collect_namespace_usage_into(child_el, usage);
        }
    }
}

/// Insert without allocating when the prefix is already known (the common case
/// by an enormous margin — a part uses a handful of prefixes across hundreds
/// of thousands of nodes).
fn insert_used(set: &mut HashSet<String>, prefix: &str) {
    if !set.contains(prefix) {
        set.insert(prefix.to_string());
    }
}

fn insert_declared(map: &mut HashMap<String, String>, prefix: &str, uri: &str) {
    if !map.contains_key(prefix) {
        map.insert(prefix.to_string(), uri.to_string());
    }
}

/// Check that every extension namespace prefix used in element or attribute
/// names is listed in the root element's `mc:Ignorable` attribute.
///
/// If an extension prefix like `w16du` is used (e.g., in `w16du:dateUtc`)
/// but not listed in `mc:Ignorable`, Word will reject the file.
pub fn check_mc_ignorable_coverage(
    part_path: &str,
    root: &Element,
    usage: &NamespaceUsage,
) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();

    // Parse mc:Ignorable from root — get the space-separated list of prefixes.
    let ignorable_prefixes: HashSet<&str> = get_mc_ignorable_prefixes(root);

    // For each used prefix: resolve it to a URI, check if it's core or ignorable.
    let core_ns_set: HashSet<&str> = CORE_OOXML_NAMESPACES.iter().copied().collect();

    for prefix in &usage.used_prefixes {
        // Skip the empty/default prefix.
        if prefix.is_empty() {
            continue;
        }

        // Try to resolve this prefix to a namespace URI.
        let Some(uri) = usage.declared.get(prefix).map(String::as_str) else {
            // Prefix has no declaration — this is I-NS-002 territory, skip here.
            continue;
        };

        // If it's a core namespace, it doesn't need mc:Ignorable.
        if core_ns_set.contains(uri) {
            continue;
        }

        // If the prefix is not in mc:Ignorable, flag it. This is a Warning
        // rather than an Error because Word natively understands many Microsoft
        // extension namespaces (a14, wps, wpc, etc.) without mc:Ignorable.
        // Only specific namespaces (like w16du) are known to cause rejection.
        if !ignorable_prefixes.contains(prefix.as_str()) {
            findings.push(ValidationFinding {
                rule_id: "I-NS-001",
                severity: ValidationSeverity::Warning,
                message: format!(
                    "namespace prefix '{prefix}' (URI: {uri}) is used but not listed \
                     in mc:Ignorable — may cause issues with strict consumers"
                ),
                location: part_path.to_string(),
            });
        }
    }

    findings
}

// =============================================================================
// I-NS-002: Every namespace prefix used has a declaration
// =============================================================================

/// Check that every namespace prefix used in element or attribute names
/// has a corresponding `xmlns:{prefix}` declaration somewhere in the tree.
///
/// While normally guaranteed by XML parsers, our tree-building code may
/// create prefixed elements/attributes without declaring the namespace.
///
/// Note: namespace declarations can appear on any element, not just the root.
/// A prefix is considered declared if any element in the tree has an
/// `xmlns:{prefix}` declaration in its namespace map.
pub fn check_namespace_declarations(
    part_path: &str,
    usage: &NamespaceUsage,
) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();

    for prefix in &usage.used_prefixes {
        if prefix.is_empty() {
            continue;
        }

        // "xml" and "xmlns" are always implicitly declared.
        if prefix == "xml" || prefix == "xmlns" {
            continue;
        }

        if !usage.declared.contains_key(prefix.as_str()) {
            findings.push(ValidationFinding {
                rule_id: "I-NS-002",
                severity: ValidationSeverity::Error,
                message: format!(
                    "namespace prefix '{prefix}' is used in element or attribute names \
                     but has no xmlns:{prefix} declaration"
                ),
                location: part_path.to_string(),
            });
        }
    }

    findings
}

// =============================================================================
// Helpers
// =============================================================================

/// Extract the space-separated list of prefixes from the root's `mc:Ignorable` attribute.
fn get_mc_ignorable_prefixes(root: &Element) -> HashSet<&str> {
    // Look for mc:Ignorable as a prefixed attribute.
    for (attr_name, value) in &root.attributes {
        let is_mc_ignorable = (attr_name.local_name == "Ignorable"
            && (attr_name.prefix.as_deref() == Some("mc")
                || attr_name.namespace.as_deref().is_some_and(|ns| {
                    ns == "http://schemas.openxmlformats.org/markup-compatibility/2006"
                })))
            || attr_name.local_name == "mc:Ignorable";
        if is_mc_ignorable {
            return value.split_whitespace().collect();
        }
    }
    HashSet::new()
}

/// Extract the namespace prefix from an element or attribute name.
/// Returns `None` if there is no prefix.
fn extract_prefix(name: &str) -> Option<&str> {
    match name.split_once(':') {
        Some((prefix, local)) if !prefix.is_empty() && !local.is_empty() => Some(prefix),
        _ => None,
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use xmltree::ParserConfig;

    fn parse_xml(xml: &str) -> Element {
        let config = ParserConfig::new()
            .ignore_comments(true)
            .whitespace_to_characters(true);
        Element::parse_with_config(Cursor::new(xml.as_bytes()), config)
            .expect("test XML should parse")
    }

    // -----------------------------------------------------------------------
    // I-NS-001 tests
    // -----------------------------------------------------------------------

    #[test]
    fn ns_001_no_extension_namespaces() {
        let xml = r#"<w:document
            xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
            <w:body>
                <w:p><w:r><w:t>hello</w:t></w:r></w:p>
            </w:body>
        </w:document>"#;
        let root = parse_xml(xml);
        let usage = collect_namespace_usage(&root);
        let findings = check_mc_ignorable_coverage("word/document.xml", &root, &usage);
        assert!(findings.is_empty(), "core namespaces need no mc:Ignorable");
    }

    #[test]
    fn ns_001_extension_in_ignorable() {
        let xml = r#"<w:document
            xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006"
            xmlns:w16du="http://schemas.microsoft.com/office/word/2023/wordml/word16du"
            mc:Ignorable="w16du">
            <w:body>
                <w:p>
                    <w:ins w:id="1" w:author="x" w:date="2025-01-01" w16du:dateUtc="2025-01-01T00:00:00Z">
                        <w:r><w:t>text</w:t></w:r>
                    </w:ins>
                </w:p>
            </w:body>
        </w:document>"#;
        let root = parse_xml(xml);
        let usage = collect_namespace_usage(&root);
        let findings = check_mc_ignorable_coverage("word/document.xml", &root, &usage);
        assert!(
            findings.is_empty(),
            "w16du is in mc:Ignorable, should be fine. Got: {findings:?}"
        );
    }

    #[test]
    fn ns_001_extension_missing_from_ignorable() {
        let xml = r#"<w:document
            xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006"
            xmlns:w16du="http://schemas.microsoft.com/office/word/2023/wordml/word16du">
            <w:body>
                <w:p>
                    <w:ins w:id="1" w:author="x" w:date="2025-01-01" w16du:dateUtc="2025-01-01T00:00:00Z">
                        <w:r><w:t>text</w:t></w:r>
                    </w:ins>
                </w:p>
            </w:body>
        </w:document>"#;
        let root = parse_xml(xml);
        let usage = collect_namespace_usage(&root);
        let findings = check_mc_ignorable_coverage("word/document.xml", &root, &usage);
        let ns001: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "I-NS-001")
            .collect();
        assert_eq!(ns001.len(), 1, "w16du used but not in mc:Ignorable");
        assert!(ns001[0].message.contains("w16du"));
    }

    #[test]
    fn ns_001_multiple_extensions_partial_coverage() {
        let xml = r#"<w:document
            xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006"
            xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml"
            xmlns:w16du="http://schemas.microsoft.com/office/word/2023/wordml/word16du"
            mc:Ignorable="w14">
            <w:body>
                <w:p w14:paraId="12345678">
                    <w:ins w:id="1" w:author="x" w:date="2025-01-01" w16du:dateUtc="2025-01-01T00:00:00Z">
                        <w:r><w:t>text</w:t></w:r>
                    </w:ins>
                </w:p>
            </w:body>
        </w:document>"#;
        let root = parse_xml(xml);
        let usage = collect_namespace_usage(&root);
        let findings = check_mc_ignorable_coverage("word/document.xml", &root, &usage);
        let ns001: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "I-NS-001")
            .collect();
        // w14 is covered, w16du is not
        assert_eq!(ns001.len(), 1);
        assert!(ns001[0].message.contains("w16du"));
    }

    // -----------------------------------------------------------------------
    // I-NS-002 tests
    // -----------------------------------------------------------------------

    #[test]
    fn ns_002_all_prefixes_declared() {
        let xml = r#"<w:document
            xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
            <w:body>
                <w:p><w:r><w:t>hello</w:t></w:r></w:p>
            </w:body>
        </w:document>"#;
        let root = parse_xml(xml);
        let usage = collect_namespace_usage(&root);
        let findings = check_namespace_declarations("word/document.xml", &usage);
        assert!(findings.is_empty(), "all prefixes are declared");
    }

    #[test]
    fn ns_002_xml_prefix_always_ok() {
        // xml: prefix is always implicitly available
        let xml = r#"<w:document
            xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p><w:r><w:t xml:space="preserve"> </w:t></w:r></w:p>
            </w:body>
        </w:document>"#;
        let root = parse_xml(xml);
        let usage = collect_namespace_usage(&root);
        let findings = check_namespace_declarations("word/document.xml", &usage);
        assert!(findings.is_empty(), "xml: prefix is implicit");
    }
}
