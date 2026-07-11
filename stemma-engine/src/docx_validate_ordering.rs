//! Element-ordering validation checks for DOCX property containers.
//!
//! OOXML (ECMA-376 Part 1, Annex A) defines XSD sequences for property
//! containers like `w:pPr`, `w:rPr`, `w:tblPr`, `w:trPr`, and `w:tcPr`.
//! Children must appear in the order specified by the schema. This module
//! validates that serialized XML respects those ordering constraints.

use xmltree::{Element, XMLNode};

use crate::docx_validate::{ValidationFinding, ValidationSeverity};

// =============================================================================
// Ordering tables — derived from ECMA-376 Part 1, Annex A XSD sequences
// =============================================================================

/// CT_PPrBase + CT_PPr sequence (§17.3.1.26 / Annex A).
///
/// `pub(crate)`: also consulted by `serialize::build_paragraph_properties` to
/// place preserved (unmodeled) pPr children at their schema-correct position
/// on emission.
pub(crate) const PPR_ORDER: &[&str] = &[
    "pStyle",
    "keepNext",
    "keepLines",
    "pageBreakBefore",
    "framePr",
    "widowControl",
    "numPr",
    "suppressLineNumbers",
    "pBdr",
    "shd",
    "tabs",
    "suppressAutoHyphens",
    "kinsoku",
    "wordWrap",
    "overflowPunct",
    "topLinePunct",
    "autoSpaceDE",
    "autoSpaceDN",
    "bidi",
    "adjustRightInd",
    "snapToGrid",
    "spacing",
    "ind",
    "contextualSpacing",
    "mirrorIndents",
    "suppressOverlap",
    "jc",
    "textDirection",
    "textAlignment",
    "textboxTightWrap",
    "outlineLvl",
    "divId",
    "cnfStyle",
    "rPr",
    "sectPr",
    "pPrChange",
];

/// CT_RPrBase + CT_RPr sequence (§17.3.2.28 / Annex A).
///
/// `pub(crate)`: also consulted by `serialize::build_rpr` to place preserved
/// (unmodeled) rPr children at their schema-correct position on emission.
pub(crate) const RPR_ORDER: &[&str] = &[
    "rStyle",
    "rFonts",
    "b",
    "bCs",
    "i",
    "iCs",
    "caps",
    "smallCaps",
    "strike",
    "dstrike",
    "outline",
    "shadow",
    "emboss",
    "imprint",
    "noProof",
    "snapToGrid",
    "vanish",
    "webHidden",
    "color",
    "spacing",
    "w",
    "kern",
    "position",
    "sz",
    "szCs",
    "highlight",
    "u",
    "effect",
    "bdr",
    "shd",
    "fitText",
    "vertAlign",
    "rtl",
    "cs",
    "em",
    "lang",
    "eastAsianLayout",
    "specVanish",
    "oMath",
    "rPrChange",
];

/// CT_TblPrBase + CT_TblPr sequence (§17.4.60 / Annex A).
pub(crate) const TBLPR_ORDER: &[&str] = &[
    "tblStyle",
    "tblpPr",
    "tblOverlap",
    "bidiVisual",
    "tblStyleRowBandSize",
    "tblStyleColBandSize",
    "tblW",
    "jc",
    "tblCellSpacing",
    "tblInd",
    "tblBorders",
    "shd",
    "tblLayout",
    "tblCellMar",
    "tblLook",
    "tblCaption",
    "tblDescription",
    "tblPrChange",
];

/// CT_TrPrBase + CT_TrPr sequence (§17.4.82 / Annex A).
pub(crate) const TRPR_ORDER: &[&str] = &[
    "cnfStyle",
    "divId",
    "gridBefore",
    "gridAfter",
    "wBefore",
    "wAfter",
    "cantSplit",
    "trHeight",
    "tblHeader",
    "tblCellSpacing",
    "jc",
    "hidden",
    "ins",
    "del",
    "trPrChange",
];

/// CT_TcPrBase + CT_TcPr sequence (§17.4.70 / Annex A).
pub(crate) const TCPR_ORDER: &[&str] = &[
    "cnfStyle",
    "tcW",
    "gridSpan",
    "hMerge",
    "vMerge",
    "tcBorders",
    "shd",
    "noWrap",
    "tcMar",
    "textDirection",
    "tcFitText",
    "vAlign",
    "hideMark",
    "headers",
    "cellIns",
    "cellDel",
    "cellMerge",
    "tcPrChange",
];

// =============================================================================
// Public entry point
// =============================================================================

/// Check element ordering in all property containers across all story parts.
///
/// Walks the XML tree of each story part, finds pPr/rPr/tblPr/trPr/tcPr
/// elements, and validates that their children appear in the XSD-defined order.
pub fn check_element_ordering(stories: &[(String, &Element)]) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();
    for (part_name, root) in stories {
        check_element_ordering_recursive(root, part_name, &mut findings);
    }
    findings
}

// =============================================================================
// Recursive tree walker
// =============================================================================

fn check_element_ordering_recursive(
    element: &Element,
    part_name: &str,
    findings: &mut Vec<ValidationFinding>,
) {
    let local = local_name(&element.name);

    // Check this element if it's a property container.
    match local {
        "pPr" => check_children_order(
            element,
            PPR_ORDER,
            "I-ORD-001",
            "w:pPr",
            part_name,
            findings,
        ),
        "rPr" => check_children_order(
            element,
            RPR_ORDER,
            "I-ORD-002",
            "w:rPr",
            part_name,
            findings,
        ),
        "tblPr" => check_children_order(
            element,
            TBLPR_ORDER,
            "I-ORD-003",
            "w:tblPr",
            part_name,
            findings,
        ),
        "trPr" => check_children_order(
            element,
            TRPR_ORDER,
            "I-ORD-004",
            "w:trPr",
            part_name,
            findings,
        ),
        "tcPr" => check_children_order(
            element,
            TCPR_ORDER,
            "I-ORD-005",
            "w:tcPr",
            part_name,
            findings,
        ),
        _ => {}
    }

    // Recurse into children.
    for child in &element.children {
        if let XMLNode::Element(el) = child {
            check_element_ordering_recursive(el, part_name, findings);
        }
    }
}

// =============================================================================
// Core ordering check
// =============================================================================

/// Check that element children appear in the order defined by `expected_order`.
///
/// Walks the children, looks up each child's local name in the ordering table,
/// and flags any child that appears before a previously-seen element that should
/// come after it in the sequence.
fn check_children_order(
    parent: &Element,
    expected_order: &[&str],
    rule_id: &'static str,
    parent_display: &str,
    part_name: &str,
    findings: &mut Vec<ValidationFinding>,
) {
    let mut max_seen_pos: Option<usize> = None;
    let mut max_seen_name: &str = "";

    for child in &parent.children {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };

        let child_local = local_name(&el.name);

        // Look up the child in the ordering table.
        let Some(pos) = expected_order.iter().position(|&name| name == child_local) else {
            // Unknown child — skip (may be an extension element, not our concern).
            continue;
        };

        if let Some(prev_pos) = max_seen_pos
            && pos < prev_pos
        {
            findings.push(ValidationFinding {
                rule_id,
                // Error: Annex A defines these as xsd:sequence, so a
                // mis-ordered container is invalid OOXML. Word happens to
                // repair it quietly, which is why I-ORD-* is not in
                // BLOCKING_RULES — but the Full gate must refuse it.
                severity: ValidationSeverity::Error,
                message: format!(
                    "{parent_display} child <w:{child_local}> (sequence position {pos}) \
                         appears after <w:{max_seen_name}> (sequence position {prev_pos})"
                ),
                location: part_name.to_string(),
            });
            // Don't update max — we want to report all out-of-order elements
            // relative to the highest-position element seen so far.
            continue;
        }

        max_seen_pos = Some(pos);
        max_seen_name = expected_order[pos];
    }
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

    fn check_ppr(xml: &str) -> Vec<ValidationFinding> {
        let root = parse_xml(xml);
        let stories = vec![("test.xml".to_string(), &root)];
        check_element_ordering(&stories)
    }

    // -------------------------------------------------------------------------
    // I-ORD-001: pPr ordering
    // -------------------------------------------------------------------------

    #[test]
    fn ppr_correct_order_no_findings() {
        let findings = check_ppr(
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                <w:body>
                    <w:p>
                        <w:pPr>
                            <w:pStyle w:val="Normal"/>
                            <w:keepNext/>
                            <w:spacing w:before="120"/>
                            <w:ind w:left="720"/>
                            <w:jc w:val="center"/>
                        </w:pPr>
                    </w:p>
                </w:body>
            </w:document>"#,
        );
        assert!(
            findings.is_empty(),
            "expected no findings, got: {findings:?}"
        );
    }

    #[test]
    fn ppr_jc_before_spacing_is_violation() {
        let findings = check_ppr(
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                <w:body>
                    <w:p>
                        <w:pPr>
                            <w:pStyle w:val="Normal"/>
                            <w:jc w:val="center"/>
                            <w:spacing w:before="120"/>
                        </w:pPr>
                    </w:p>
                </w:body>
            </w:document>"#,
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule_id, "I-ORD-001");
        assert!(findings[0].message.contains("spacing"));
        assert!(findings[0].message.contains("jc"));
    }

    #[test]
    fn ppr_multiple_violations() {
        // jc before keepNext, spacing before keepNext
        let findings = check_ppr(
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                <w:body>
                    <w:p>
                        <w:pPr>
                            <w:jc w:val="center"/>
                            <w:keepNext/>
                            <w:spacing w:before="120"/>
                        </w:pPr>
                    </w:p>
                </w:body>
            </w:document>"#,
        );
        // keepNext (pos 1) after jc (pos 26) -> violation
        // spacing (pos 21) after jc (pos 26) -> violation
        assert_eq!(findings.len(), 2, "findings: {findings:?}");
        assert!(findings.iter().all(|f| f.rule_id == "I-ORD-001"));
    }

    #[test]
    fn ppr_sectpr_after_rpr_ok() {
        let findings = check_ppr(
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                <w:body>
                    <w:p>
                        <w:pPr>
                            <w:pStyle w:val="Normal"/>
                            <w:rPr/>
                            <w:sectPr/>
                        </w:pPr>
                    </w:p>
                </w:body>
            </w:document>"#,
        );
        assert!(
            findings.is_empty(),
            "expected no findings, got: {findings:?}"
        );
    }

    // -------------------------------------------------------------------------
    // I-ORD-002: rPr ordering
    // -------------------------------------------------------------------------

    #[test]
    fn rpr_correct_order_no_findings() {
        let findings = check_ppr(
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                <w:body>
                    <w:p>
                        <w:r>
                            <w:rPr>
                                <w:rStyle w:val="Bold"/>
                                <w:b/>
                                <w:i/>
                                <w:color w:val="FF0000"/>
                                <w:sz w:val="24"/>
                                <w:u w:val="single"/>
                            </w:rPr>
                        </w:r>
                    </w:p>
                </w:body>
            </w:document>"#,
        );
        assert!(
            findings.is_empty(),
            "expected no findings, got: {findings:?}"
        );
    }

    #[test]
    fn rpr_sz_before_bold_is_violation() {
        let findings = check_ppr(
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                <w:body>
                    <w:p>
                        <w:r>
                            <w:rPr>
                                <w:sz w:val="24"/>
                                <w:b/>
                            </w:rPr>
                        </w:r>
                    </w:p>
                </w:body>
            </w:document>"#,
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule_id, "I-ORD-002");
        assert!(findings[0].message.contains("b"));
        assert!(findings[0].message.contains("sz"));
    }

    // -------------------------------------------------------------------------
    // I-ORD-003: tblPr ordering
    // -------------------------------------------------------------------------

    #[test]
    fn tblpr_correct_order_no_findings() {
        let findings = check_ppr(
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                <w:body>
                    <w:tbl>
                        <w:tblPr>
                            <w:tblStyle w:val="TableGrid"/>
                            <w:tblW w:w="0" w:type="auto"/>
                            <w:tblBorders/>
                            <w:tblLook w:val="04A0"/>
                        </w:tblPr>
                    </w:tbl>
                </w:body>
            </w:document>"#,
        );
        assert!(
            findings.is_empty(),
            "expected no findings, got: {findings:?}"
        );
    }

    #[test]
    fn tblpr_borders_before_style_is_violation() {
        let findings = check_ppr(
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                <w:body>
                    <w:tbl>
                        <w:tblPr>
                            <w:tblBorders/>
                            <w:tblStyle w:val="TableGrid"/>
                        </w:tblPr>
                    </w:tbl>
                </w:body>
            </w:document>"#,
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule_id, "I-ORD-003");
    }

    // -------------------------------------------------------------------------
    // I-ORD-004: trPr ordering
    // -------------------------------------------------------------------------

    #[test]
    fn trpr_correct_order_no_findings() {
        let findings = check_ppr(
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                <w:body>
                    <w:tbl>
                        <w:tr>
                            <w:trPr>
                                <w:gridBefore w:val="1"/>
                                <w:trHeight w:val="500"/>
                            </w:trPr>
                        </w:tr>
                    </w:tbl>
                </w:body>
            </w:document>"#,
        );
        assert!(
            findings.is_empty(),
            "expected no findings, got: {findings:?}"
        );
    }

    #[test]
    fn trpr_del_before_gridbefore_is_violation() {
        let findings = check_ppr(
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                <w:body>
                    <w:tbl>
                        <w:tr>
                            <w:trPr>
                                <w:del w:id="1" w:author="test"/>
                                <w:gridBefore w:val="1"/>
                            </w:trPr>
                        </w:tr>
                    </w:tbl>
                </w:body>
            </w:document>"#,
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule_id, "I-ORD-004");
    }

    // -------------------------------------------------------------------------
    // I-ORD-005: tcPr ordering
    // -------------------------------------------------------------------------

    #[test]
    fn tcpr_correct_order_no_findings() {
        let findings = check_ppr(
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                <w:body>
                    <w:tbl>
                        <w:tr>
                            <w:tc>
                                <w:tcPr>
                                    <w:tcW w:w="1000" w:type="dxa"/>
                                    <w:gridSpan w:val="2"/>
                                    <w:vMerge w:val="restart"/>
                                    <w:tcBorders/>
                                    <w:shd w:fill="FF0000"/>
                                    <w:vAlign w:val="center"/>
                                </w:tcPr>
                            </w:tc>
                        </w:tr>
                    </w:tbl>
                </w:body>
            </w:document>"#,
        );
        assert!(
            findings.is_empty(),
            "expected no findings, got: {findings:?}"
        );
    }

    #[test]
    fn tcpr_valign_before_tcw_is_violation() {
        let findings = check_ppr(
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                <w:body>
                    <w:tbl>
                        <w:tr>
                            <w:tc>
                                <w:tcPr>
                                    <w:vAlign w:val="center"/>
                                    <w:tcW w:w="1000" w:type="dxa"/>
                                </w:tcPr>
                            </w:tc>
                        </w:tr>
                    </w:tbl>
                </w:body>
            </w:document>"#,
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule_id, "I-ORD-005");
    }

    // -------------------------------------------------------------------------
    // Unknown children are ignored
    // -------------------------------------------------------------------------

    #[test]
    fn unknown_children_ignored() {
        let findings = check_ppr(
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                <w:body>
                    <w:p>
                        <w:pPr>
                            <w:pStyle w:val="Normal"/>
                            <w14:someExtension xmlns:w14="http://example.com"/>
                            <w:jc w:val="center"/>
                        </w:pPr>
                    </w:p>
                </w:body>
            </w:document>"#,
        );
        assert!(
            findings.is_empty(),
            "expected no findings, got: {findings:?}"
        );
    }

    // -------------------------------------------------------------------------
    // Findings are Error severity
    // -------------------------------------------------------------------------

    #[test]
    fn findings_are_errors() {
        // Annex A sequence order is normative (the XSD uses xsd:sequence), so a
        // mis-ordered property container is invalid OOXML, not a style nit:
        // Error severity, refused by the Full gate.
        let findings = check_ppr(
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                <w:body>
                    <w:p>
                        <w:pPr>
                            <w:jc w:val="center"/>
                            <w:pStyle w:val="Normal"/>
                        </w:pPr>
                    </w:p>
                </w:body>
            </w:document>"#,
        );
        assert!(!findings.is_empty());
        for f in &findings {
            assert_eq!(f.severity, ValidationSeverity::Error);
        }
    }
}
