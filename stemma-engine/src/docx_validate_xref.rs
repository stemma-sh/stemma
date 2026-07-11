//! Cross-part reference validation checks for DOCX packages.
//!
//! Verifies that cross-part references resolve to defined targets:
//! - Style IDs referenced in story parts must exist in `word/styles.xml`
//! - NumIds referenced in story parts must exist in `word/numbering.xml`

use std::collections::HashSet;

use xmltree::{Element, XMLNode};

use crate::docx_validate::{PackageState, ValidationFinding, ValidationSeverity};

// =============================================================================
// I-XREF-001: Referenced style IDs must exist in styles.xml
// =============================================================================

/// Check that every style ID referenced in story parts (pStyle, rStyle, tblStyle)
/// exists as a defined style in `word/styles.xml`.
///
/// Missing style references cause Word to silently fall back to default formatting.
pub(crate) fn check_xref_001_style_ids(state: &PackageState) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();

    // Defined style IDs from the pre-parsed styles.xml. An unparseable
    // styles.xml is absent here and already an I-XML-001 error; with no
    // definitions visible, references are reported as dangling against it.
    let defined_style_ids: HashSet<String> = match &state.styles_root {
        Some(root) => collect_defined_style_ids(root),
        None => {
            // No styles.xml — if no style references exist, that's fine.
            // If references exist, they're all dangling.
            HashSet::new()
        }
    };

    // Walk each story part and collect referenced style IDs.
    for (part_name, root) in &state.story_parts {
        let mut referenced = Vec::new();
        collect_referenced_style_ids(root, &mut referenced);

        for style_id in &referenced {
            if !defined_style_ids.contains(style_id.as_str()) {
                findings.push(ValidationFinding {
                    rule_id: "I-XREF-001",
                    severity: ValidationSeverity::Warning,
                    message: format!(
                        "style ID {style_id:?} is referenced but not defined in word/styles.xml"
                    ),
                    location: part_name.clone(),
                });
            }
        }
    }

    findings
}

/// Collect all `w:styleId` attribute values from `w:style` elements in styles.xml.
fn collect_defined_style_ids(root: &Element) -> HashSet<String> {
    let mut ids = HashSet::new();
    for child in &root.children {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };
        if local_name(&el.name) == "style"
            && let Some(id) = get_val_attr(el, "styleId")
        {
            ids.insert(id.to_string());
        }
    }
    ids
}

/// Recursively collect all style ID references from pStyle, rStyle, tblStyle elements.
fn collect_referenced_style_ids(element: &Element, out: &mut Vec<String>) {
    let local = local_name(&element.name);
    match local {
        "pStyle" | "rStyle" | "tblStyle" => {
            if let Some(val) = get_val_attr(element, "val") {
                out.push(val.to_string());
            }
        }
        _ => {}
    }

    for child in &element.children {
        if let XMLNode::Element(el) = child {
            collect_referenced_style_ids(el, out);
        }
    }
}

// =============================================================================
// I-XREF-002: Referenced numIds must exist in numbering.xml
// =============================================================================

/// Check that every numId referenced in story parts exists as a defined
/// `w:num` in `word/numbering.xml`.
///
/// Special case: `numId="0"` means "no numbering" and is always valid.
pub(crate) fn check_xref_002_num_ids(state: &PackageState) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();

    // First collect all referenced numIds across story parts.
    let mut all_references: Vec<(String, String)> = Vec::new(); // (part_name, numId)
    for (part_name, root) in &state.story_parts {
        let mut referenced = Vec::new();
        collect_referenced_num_ids(root, &mut referenced);
        for num_id in referenced {
            all_references.push((part_name.clone(), num_id));
        }
    }

    // If no numId references exist, no numbering.xml is needed.
    if all_references.is_empty() {
        return findings;
    }

    // Defined numIds from the pre-parsed numbering.xml (unparseable -> absent,
    // already an I-XML-001 error).
    let defined_num_ids: HashSet<String> = match &state.numbering_root {
        Some(root) => collect_defined_num_ids(root),
        None => {
            // No numbering.xml but numId references exist — all non-zero are dangling.
            HashSet::new()
        }
    };

    for (part_name, num_id) in &all_references {
        // numId="0" means "no numbering" — always valid.
        if num_id == "0" {
            continue;
        }

        if !defined_num_ids.contains(num_id.as_str()) {
            findings.push(ValidationFinding {
                rule_id: "I-XREF-002",
                severity: ValidationSeverity::Warning,
                message: format!(
                    "numId {num_id:?} is referenced but not defined in word/numbering.xml"
                ),
                location: part_name.clone(),
            });
        }
    }

    findings
}

/// Collect all `w:numId` attribute values from `w:num` elements in numbering.xml.
fn collect_defined_num_ids(root: &Element) -> HashSet<String> {
    let mut ids = HashSet::new();
    for child in &root.children {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };
        if local_name(&el.name) == "num"
            && let Some(id) = get_val_attr(el, "numId")
        {
            ids.insert(id.to_string());
        }
    }
    ids
}

/// Recursively collect all `w:numId w:val="..."` references inside `w:numPr`.
fn collect_referenced_num_ids(element: &Element, out: &mut Vec<String>) {
    let local = local_name(&element.name);

    // Only look for numId inside numPr containers.
    if local == "numPr" {
        for child in &element.children {
            if let XMLNode::Element(el) = child
                && local_name(&el.name) == "numId"
                && let Some(val) = get_val_attr(el, "val")
            {
                out.push(val.to_string());
            }
        }
    }

    // Recurse into children to find numPr elements at any depth.
    for child in &element.children {
        if let XMLNode::Element(el) = child {
            collect_referenced_num_ids(el, out);
        }
    }
}

// =============================================================================
// I-XREF-003: Comment references must resolve to comments.xml
// =============================================================================

/// Check that every `commentReference` w:id in story parts points to a
/// `w:comment` defined in `word/comments.xml`.
///
/// A dangling reference means the comment balloon will be lost when Word
/// opens the file.
pub(crate) fn check_xref_003_comment_ids(state: &PackageState) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();

    // Collect all commentReference w:id values from story parts.
    let mut all_references: Vec<(String, String)> = Vec::new(); // (part_name, id)
    for (part_name, root) in &state.story_parts {
        let mut referenced = Vec::new();
        collect_comment_references(root, &mut referenced);
        for id in referenced {
            all_references.push((part_name.clone(), id));
        }
    }

    // If no commentReference elements exist, nothing to check.
    if all_references.is_empty() {
        return findings;
    }

    // Defined w:comment w:id values from the pre-parsed comments.xml
    // (unparseable -> absent, already an I-XML-001 error).
    let defined_comment_ids: HashSet<String> = match state.comments_root() {
        Some(root) => collect_defined_comment_ids(root),
        None => {
            // No comments.xml but commentReference elements exist — all are dangling.
            HashSet::new()
        }
    };

    for (part_name, id) in &all_references {
        if !defined_comment_ids.contains(id.as_str()) {
            findings.push(ValidationFinding {
                rule_id: "I-XREF-003",
                severity: ValidationSeverity::Warning,
                message: format!(
                    "commentReference w:id='{id}' is referenced but not defined in word/comments.xml"
                ),
                location: part_name.clone(),
            });
        }
    }

    findings
}

/// Collect all `w:id` attribute values from `w:comment` elements in comments.xml.
fn collect_defined_comment_ids(root: &Element) -> HashSet<String> {
    let mut ids = HashSet::new();
    for child in &root.children {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };
        if local_name(&el.name) == "comment"
            && let Some(id) = get_val_attr(el, "id")
        {
            ids.insert(id.to_string());
        }
    }
    ids
}

/// Recursively collect all `w:id` values from `w:commentReference` elements.
fn collect_comment_references(element: &Element, out: &mut Vec<String>) {
    if local_name(&element.name) == "commentReference"
        && let Some(id) = get_val_attr(element, "id")
    {
        out.push(id.to_string());
    }
    for child in &element.children {
        if let XMLNode::Element(el) = child {
            collect_comment_references(el, out);
        }
    }
}

// =============================================================================
// I-XREF-004: Style count must not exceed 4,079 (MS-OE376 §2.1.243)
// =============================================================================

/// Check that the total number of `w:style` elements in `word/styles.xml`
/// does not exceed 4,079.
///
/// Word refuses to open documents with more than 4,079 styles.
pub(crate) fn check_xref_004_style_count(state: &PackageState) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();

    let Some(root) = &state.styles_root else {
        return findings;
    };

    let count = root
        .children
        .iter()
        .filter(|child| matches!(child, xmltree::XMLNode::Element(el) if local_name(&el.name) == "style"))
        .count();

    if count > 4079 {
        findings.push(ValidationFinding {
            rule_id: "I-XREF-004",
            severity: ValidationSeverity::Error,
            message: format!(
                "styles.xml defines {count} styles; Word refuses to open documents with more than 4,079 styles (MS-OE376 §2.1.243)"
            ),
            location: "word/styles.xml".to_string(),
        });
    }

    findings
}

// =============================================================================
// I-XREF-005: Style ID must not exceed 253 characters (MS-OE376 §2.1.243)
// =============================================================================

/// Check that no `w:styleId` attribute in `word/styles.xml` exceeds 253
/// characters.
///
/// Word ignores styles whose styleId is longer than 253 characters.
pub(crate) fn check_xref_005_style_id_length(state: &PackageState) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();

    let Some(root) = &state.styles_root else {
        return findings;
    };

    for child in &root.children {
        let xmltree::XMLNode::Element(el) = child else {
            continue;
        };
        if local_name(&el.name) != "style" {
            continue;
        }
        if let Some(id) = get_val_attr(el, "styleId")
            && id.len() > 253
        {
            findings.push(ValidationFinding {
                    rule_id: "I-XREF-005",
                    severity: ValidationSeverity::Warning,
                    message: format!(
                        "styleId {:?} is {} characters long; Word ignores styles with styleId > 253 characters (MS-OE376 §2.1.243)",
                        id,
                        id.len()
                    ),
                    location: "word/styles.xml".to_string(),
                });
        }
    }

    findings
}

// =============================================================================
// Helpers
// =============================================================================

fn local_name(name: &str) -> &str {
    match name.rsplit_once(':') {
        Some((_, local)) => local,
        None => name,
    }
}

/// Get the value of a `w:val` or plain `val` attribute (handles both prefixed and unprefixed).
fn get_val_attr<'a>(element: &'a Element, local: &str) -> Option<&'a str> {
    for (name, value) in &element.attributes {
        if name.local_name == local {
            return Some(value.as_str());
        }
    }
    None
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
    // Style ID collection helpers
    // -----------------------------------------------------------------------

    #[test]
    fn collect_defined_styles_from_styles_xml() {
        let xml = r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:style w:type="paragraph" w:styleId="Normal">
                <w:name w:val="Normal"/>
            </w:style>
            <w:style w:type="paragraph" w:styleId="Heading1">
                <w:name w:val="heading 1"/>
            </w:style>
            <w:style w:type="character" w:styleId="DefaultParagraphFont">
                <w:name w:val="Default Paragraph Font"/>
            </w:style>
        </w:styles>"#;
        let root = parse_xml(xml);
        let ids = collect_defined_style_ids(&root);
        assert_eq!(ids.len(), 3);
        assert!(ids.contains("Normal"));
        assert!(ids.contains("Heading1"));
        assert!(ids.contains("DefaultParagraphFont"));
    }

    #[test]
    fn collect_referenced_styles_from_document() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:pPr><w:pStyle w:val="Heading1"/></w:pPr>
                    <w:r>
                        <w:rPr><w:rStyle w:val="Strong"/></w:rPr>
                        <w:t>hello</w:t>
                    </w:r>
                </w:p>
                <w:tbl>
                    <w:tblPr><w:tblStyle w:val="TableGrid"/></w:tblPr>
                </w:tbl>
            </w:body>
        </w:document>"#;
        let root = parse_xml(xml);
        let mut refs = Vec::new();
        collect_referenced_style_ids(&root, &mut refs);
        assert_eq!(refs.len(), 3);
        assert!(refs.contains(&"Heading1".to_string()));
        assert!(refs.contains(&"Strong".to_string()));
        assert!(refs.contains(&"TableGrid".to_string()));
    }

    // -----------------------------------------------------------------------
    // NumId collection helpers
    // -----------------------------------------------------------------------

    #[test]
    fn collect_defined_nums_from_numbering_xml() {
        let xml = r#"<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:abstractNum w:abstractNumId="0"/>
            <w:num w:numId="1">
                <w:abstractNumId w:val="0"/>
            </w:num>
            <w:num w:numId="2">
                <w:abstractNumId w:val="0"/>
            </w:num>
        </w:numbering>"#;
        let root = parse_xml(xml);
        let ids = collect_defined_num_ids(&root);
        assert_eq!(ids.len(), 2);
        assert!(ids.contains("1"));
        assert!(ids.contains("2"));
    }

    #[test]
    fn collect_referenced_numids_from_document() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:pPr>
                        <w:numPr>
                            <w:ilvl w:val="0"/>
                            <w:numId w:val="5"/>
                        </w:numPr>
                    </w:pPr>
                </w:p>
                <w:p>
                    <w:pPr>
                        <w:numPr>
                            <w:ilvl w:val="0"/>
                            <w:numId w:val="0"/>
                        </w:numPr>
                    </w:pPr>
                </w:p>
            </w:body>
        </w:document>"#;
        let root = parse_xml(xml);
        let mut refs = Vec::new();
        collect_referenced_num_ids(&root, &mut refs);
        assert_eq!(refs.len(), 2);
        assert!(refs.contains(&"5".to_string()));
        assert!(refs.contains(&"0".to_string()));
    }

    // -----------------------------------------------------------------------
    // Integration-style tests using PackageState
    // -----------------------------------------------------------------------

    use std::collections::HashMap;

    fn make_state(parts: &[(&str, &str)]) -> PackageState {
        let mut story_parts = Vec::new();
        let mut styles_root = None;
        let mut numbering_root = None;

        for (name, content) in parts {
            let root = parse_xml(content);
            match *name {
                "word/styles.xml" => styles_root = Some(root),
                "word/numbering.xml" => numbering_root = Some(root),
                n if n == "word/document.xml"
                    || n == "word/comments.xml"
                    || n.starts_with("word/header")
                    || n.starts_with("word/footer") =>
                {
                    story_parts.push((n.to_string(), root));
                }
                _ => {}
            }
        }
        story_parts.sort_by(|(a, _), (b, _)| a.cmp(b));

        PackageState {
            part_names: parts.iter().map(|(n, _)| n.to_string()).collect(),
            content_types_xml: None,
            rels_files: HashMap::new(),
            story_parts,
            styles_root,
            numbering_root,
            main_part: Some("word/document.xml".to_string()),
        }
    }

    // -----------------------------------------------------------------------
    // I-XREF-001: Style ID tests
    // -----------------------------------------------------------------------

    #[test]
    fn xref_001_valid_style_reference_no_findings() {
        let state = make_state(&[
            (
                "word/styles.xml",
                r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                    <w:style w:type="paragraph" w:styleId="Normal">
                        <w:name w:val="Normal"/>
                    </w:style>
                </w:styles>"#,
            ),
            (
                "word/document.xml",
                r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                    <w:body>
                        <w:p><w:pPr><w:pStyle w:val="Normal"/></w:pPr></w:p>
                    </w:body>
                </w:document>"#,
            ),
        ]);
        let findings = check_xref_001_style_ids(&state);
        assert!(
            findings.is_empty(),
            "expected 0 findings, got: {findings:?}"
        );
    }

    #[test]
    fn xref_001_missing_style_produces_warning() {
        let state = make_state(&[
            (
                "word/styles.xml",
                r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                    <w:style w:type="paragraph" w:styleId="Normal">
                        <w:name w:val="Normal"/>
                    </w:style>
                </w:styles>"#,
            ),
            (
                "word/document.xml",
                r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                    <w:body>
                        <w:p><w:pPr><w:pStyle w:val="NonExistent"/></w:pPr></w:p>
                    </w:body>
                </w:document>"#,
            ),
        ]);
        let findings = check_xref_001_style_ids(&state);
        assert_eq!(findings.len(), 1, "expected 1 finding, got: {findings:?}");
        assert_eq!(findings[0].rule_id, "I-XREF-001");
        assert_eq!(findings[0].severity, ValidationSeverity::Warning);
        assert!(findings[0].message.contains("NonExistent"));
    }

    #[test]
    fn xref_001_multiple_style_types() {
        let state = make_state(&[
            (
                "word/styles.xml",
                r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                    <w:style w:type="paragraph" w:styleId="Normal"/>
                    <w:style w:type="table" w:styleId="TableGrid"/>
                </w:styles>"#,
            ),
            (
                "word/document.xml",
                r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                    <w:body>
                        <w:p>
                            <w:pPr><w:pStyle w:val="Normal"/></w:pPr>
                            <w:r><w:rPr><w:rStyle w:val="MissingChar"/></w:rPr></w:r>
                        </w:p>
                        <w:tbl>
                            <w:tblPr><w:tblStyle w:val="TableGrid"/></w:tblPr>
                        </w:tbl>
                    </w:body>
                </w:document>"#,
            ),
        ]);
        let findings = check_xref_001_style_ids(&state);
        assert_eq!(
            findings.len(),
            1,
            "only MissingChar should be flagged: {findings:?}"
        );
        assert!(findings[0].message.contains("MissingChar"));
    }

    // -----------------------------------------------------------------------
    // I-XREF-002: NumId tests
    // -----------------------------------------------------------------------

    #[test]
    fn xref_002_valid_numid_no_findings() {
        let state = make_state(&[
            (
                "word/numbering.xml",
                r#"<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                    <w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
                </w:numbering>"#,
            ),
            (
                "word/document.xml",
                r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                    <w:body>
                        <w:p><w:pPr><w:numPr><w:numId w:val="1"/></w:numPr></w:pPr></w:p>
                    </w:body>
                </w:document>"#,
            ),
        ]);
        let findings = check_xref_002_num_ids(&state);
        assert!(
            findings.is_empty(),
            "expected 0 findings, got: {findings:?}"
        );
    }

    #[test]
    fn xref_002_missing_numid_produces_warning() {
        let state = make_state(&[
            (
                "word/numbering.xml",
                r#"<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                    <w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
                </w:numbering>"#,
            ),
            (
                "word/document.xml",
                r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                    <w:body>
                        <w:p><w:pPr><w:numPr><w:numId w:val="99"/></w:numPr></w:pPr></w:p>
                    </w:body>
                </w:document>"#,
            ),
        ]);
        let findings = check_xref_002_num_ids(&state);
        assert_eq!(findings.len(), 1, "expected 1 finding, got: {findings:?}");
        assert_eq!(findings[0].rule_id, "I-XREF-002");
        assert_eq!(findings[0].severity, ValidationSeverity::Warning);
        assert!(findings[0].message.contains("99"));
    }

    #[test]
    fn xref_002_numid_zero_is_valid() {
        let state = make_state(&[(
            "word/document.xml",
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                    <w:body>
                        <w:p><w:pPr><w:numPr><w:numId w:val="0"/></w:numPr></w:pPr></w:p>
                    </w:body>
                </w:document>"#,
        )]);
        let findings = check_xref_002_num_ids(&state);
        assert!(
            findings.is_empty(),
            "numId=0 means 'no numbering', not a dangling ref: {findings:?}"
        );
    }

    #[test]
    fn xref_002_no_numbering_xml_no_refs_is_ok() {
        let state = make_state(&[(
            "word/document.xml",
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                    <w:body>
                        <w:p><w:pPr><w:pStyle w:val="Normal"/></w:pPr></w:p>
                    </w:body>
                </w:document>"#,
        )]);
        let findings = check_xref_002_num_ids(&state);
        assert!(
            findings.is_empty(),
            "no numId refs means no numbering.xml needed: {findings:?}"
        );
    }

    #[test]
    fn xref_002_no_numbering_xml_with_refs_produces_warnings() {
        let state = make_state(&[(
            "word/document.xml",
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                    <w:body>
                        <w:p><w:pPr><w:numPr><w:numId w:val="5"/></w:numPr></w:pPr></w:p>
                    </w:body>
                </w:document>"#,
        )]);
        let findings = check_xref_002_num_ids(&state);
        assert_eq!(
            findings.len(),
            1,
            "numId=5 referenced but no numbering.xml: {findings:?}"
        );
        assert_eq!(findings[0].rule_id, "I-XREF-002");
    }

    // -----------------------------------------------------------------------
    // I-XREF-003: Comment reference cross-part check
    // -----------------------------------------------------------------------

    #[test]
    fn xref_003_valid_comment_reference_no_findings() {
        let state = make_state(&[
            (
                "word/comments.xml",
                r#"<w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                    <w:comment w:id="0" w:author="Alice" w:date="2024-01-01T00:00:00Z">
                        <w:p><w:r><w:t>Comment text</w:t></w:r></w:p>
                    </w:comment>
                </w:comments>"#,
            ),
            (
                "word/document.xml",
                r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                    <w:body>
                        <w:p>
                            <w:r>
                                <w:commentReference w:id="0"/>
                            </w:r>
                        </w:p>
                    </w:body>
                </w:document>"#,
            ),
        ]);
        let findings = check_xref_003_comment_ids(&state);
        assert!(
            findings.is_empty(),
            "valid reference should produce no findings: {findings:?}"
        );
    }

    #[test]
    fn xref_003_dangling_comment_reference_produces_warning() {
        let state = make_state(&[
            (
                "word/comments.xml",
                r#"<w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                    <w:comment w:id="1" w:author="Alice" w:date="2024-01-01T00:00:00Z">
                        <w:p><w:r><w:t>A comment</w:t></w:r></w:p>
                    </w:comment>
                </w:comments>"#,
            ),
            (
                "word/document.xml",
                r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                    <w:body>
                        <w:p>
                            <w:r>
                                <w:commentReference w:id="99"/>
                            </w:r>
                        </w:p>
                    </w:body>
                </w:document>"#,
            ),
        ]);
        let findings = check_xref_003_comment_ids(&state);
        assert_eq!(findings.len(), 1, "expected 1 finding, got: {findings:?}");
        assert_eq!(findings[0].rule_id, "I-XREF-003");
        assert_eq!(findings[0].severity, ValidationSeverity::Warning);
        assert!(findings[0].message.contains("99"));
    }

    // -----------------------------------------------------------------------
    // I-XREF-004: Style count limit
    // -----------------------------------------------------------------------

    #[test]
    fn xref_004_style_count_within_limit_no_findings() {
        // Build styles.xml with exactly 10 styles — well under 4,079.
        let mut styles_body = String::from(
            r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
        );
        for i in 0..10 {
            styles_body.push_str(&format!(
                r#"<w:style w:type="paragraph" w:styleId="Style{i}"/>"#
            ));
        }
        styles_body.push_str("</w:styles>");

        let state = make_state(&[("word/styles.xml", &styles_body)]);
        let findings = check_xref_004_style_count(&state);
        assert!(
            findings.is_empty(),
            "10 styles should produce no findings: {findings:?}"
        );
    }

    #[test]
    fn xref_004_style_count_over_limit_produces_error() {
        // Build styles.xml with 4,080 styles — one over the limit.
        let mut styles_body = String::from(
            r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
        );
        for i in 0..4080 {
            styles_body.push_str(&format!(
                r#"<w:style w:type="paragraph" w:styleId="Style{i}"/>"#
            ));
        }
        styles_body.push_str("</w:styles>");

        let state = make_state(&[("word/styles.xml", &styles_body)]);
        let findings = check_xref_004_style_count(&state);
        assert_eq!(
            findings.len(),
            1,
            "4080 styles should produce 1 finding: {findings:?}"
        );
        assert_eq!(findings[0].rule_id, "I-XREF-004");
        assert_eq!(findings[0].severity, ValidationSeverity::Error);
        assert!(findings[0].message.contains("4080"));
    }

    // -----------------------------------------------------------------------
    // I-XREF-005: Style ID length limit
    // -----------------------------------------------------------------------

    #[test]
    fn xref_005_style_id_within_limit_no_findings() {
        let id_253 = "A".repeat(253);
        let xml = format!(
            r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                <w:style w:type="paragraph" w:styleId="{id_253}"/>
            </w:styles>"#
        );
        let state = make_state(&[("word/styles.xml", &xml)]);
        let findings = check_xref_005_style_id_length(&state);
        assert!(
            findings.is_empty(),
            "253-char styleId should produce no findings: {findings:?}"
        );
    }

    #[test]
    fn xref_005_style_id_over_limit_produces_warning() {
        let id_254 = "B".repeat(254);
        let xml = format!(
            r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                <w:style w:type="paragraph" w:styleId="{id_254}"/>
            </w:styles>"#
        );
        let state = make_state(&[("word/styles.xml", &xml)]);
        let findings = check_xref_005_style_id_length(&state);
        assert_eq!(
            findings.len(),
            1,
            "254-char styleId should produce 1 finding: {findings:?}"
        );
        assert_eq!(findings[0].rule_id, "I-XREF-005");
        assert_eq!(findings[0].severity, ValidationSeverity::Warning);
        assert!(findings[0].message.contains("254"));
    }

    #[test]
    fn xref_003_no_comments_xml_with_refs_produces_warning() {
        let state = make_state(&[(
            "word/document.xml",
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                    <w:body>
                        <w:p>
                            <w:r>
                                <w:commentReference w:id="5"/>
                            </w:r>
                        </w:p>
                    </w:body>
                </w:document>"#,
        )]);
        let findings = check_xref_003_comment_ids(&state);
        assert_eq!(
            findings.len(),
            1,
            "no comments.xml but ref exists: {findings:?}"
        );
        assert_eq!(findings[0].rule_id, "I-XREF-003");
        assert!(findings[0].message.contains("5"));
    }
}
