//! Integration tests for `docx_validate_annotations` checks.
//!
//! These tests build minimal XML trees using `xmltree::Element` directly
//! (no full DOCX needed) and verify each check function.

use std::io::Cursor;
use stemma::docx_validate_annotations::*;
use xmltree::Element;

/// Parse a WML XML snippet.
fn parse_wml(xml: &str) -> Element {
    Element::parse(Cursor::new(xml.as_bytes())).expect("test XML should parse")
}

// ---------------------------------------------------------------------------
// I-ANN-001: Annotation ID uniqueness
// ---------------------------------------------------------------------------

#[test]
fn annotation_ids_unique_across_multiple_parts() {
    let doc = parse_wml(
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:ins w:id="10" w:author="a" w:date="2024-01-01T00:00:00Z">
                        <w:r><w:t>x</w:t></w:r>
                    </w:ins>
                </w:p>
            </w:body>
        </w:document>"#,
    );
    let hdr = parse_wml(
        r#"<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:p>
                <w:del w:id="20" w:author="b" w:date="2024-01-01T00:00:00Z">
                    <w:r><w:delText>y</w:delText></w:r>
                </w:del>
            </w:p>
        </w:hdr>"#,
    );
    let stories = vec![
        ("word/document.xml".to_string(), &doc),
        ("word/header1.xml".to_string(), &hdr),
    ];
    let findings = check_annotation_id_uniqueness(&stories);
    let ann001: Vec<_> = findings
        .iter()
        .filter(|f| f.rule_id == "I-ANN-001")
        .collect();
    assert!(
        ann001.is_empty(),
        "distinct IDs across parts should be fine"
    );
}

#[test]
fn annotation_id_collision_reported() {
    let doc = parse_wml(
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:ins w:id="42" w:author="a" w:date="2024-01-01T00:00:00Z">
                        <w:r><w:t>x</w:t></w:r>
                    </w:ins>
                    <w:del w:id="42" w:author="b" w:date="2024-01-01T00:00:00Z">
                        <w:r><w:delText>y</w:delText></w:r>
                    </w:del>
                </w:p>
            </w:body>
        </w:document>"#,
    );
    let stories = vec![("word/document.xml".to_string(), &doc)];
    let findings = check_annotation_id_uniqueness(&stories);
    let ann001: Vec<_> = findings
        .iter()
        .filter(|f| f.rule_id == "I-ANN-001")
        .collect();
    assert_eq!(ann001.len(), 1);
    assert!(ann001[0].message.contains("42"));
}

// ---------------------------------------------------------------------------
// I-ANN-002: ID validity
// ---------------------------------------------------------------------------

#[test]
fn annotation_id_not_a_number() {
    let doc = parse_wml(
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:commentRangeStart w:id="notanumber"/>
                    <w:commentRangeEnd w:id="notanumber"/>
                </w:p>
            </w:body>
        </w:document>"#,
    );
    let stories = vec![("word/document.xml".to_string(), &doc)];
    let findings = check_annotation_id_uniqueness(&stories);
    let ann002: Vec<_> = findings
        .iter()
        .filter(|f| f.rule_id == "I-ANN-002")
        .collect();
    assert_eq!(
        ann002.len(),
        2,
        "both commentRangeStart and End have bad IDs"
    );
}

// ---------------------------------------------------------------------------
// I-ANN-003: Bookmark pairing
// ---------------------------------------------------------------------------

#[test]
fn bookmark_properly_paired() {
    let doc = parse_wml(
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:bookmarkStart w:id="0" w:name="bm1"/>
                    <w:r><w:t>text</w:t></w:r>
                    <w:bookmarkEnd w:id="0"/>
                </w:p>
                <w:p>
                    <w:bookmarkStart w:id="1" w:name="bm2"/>
                    <w:bookmarkEnd w:id="1"/>
                </w:p>
            </w:body>
        </w:document>"#,
    );
    let stories = vec![("word/document.xml".to_string(), &doc)];
    let findings = check_bookmark_pairing(&stories);
    assert!(findings.is_empty());
}

#[test]
fn bookmark_orphan_start() {
    let doc = parse_wml(
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:bookmarkStart w:id="5" w:name="orphan"/>
                    <w:r><w:t>no end</w:t></w:r>
                </w:p>
            </w:body>
        </w:document>"#,
    );
    let stories = vec![("word/document.xml".to_string(), &doc)];
    let findings = check_bookmark_pairing(&stories);
    assert_eq!(findings.len(), 1);
    assert!(findings[0].message.contains("no matching bookmarkEnd"));
}

#[test]
fn bookmark_orphan_end() {
    let doc = parse_wml(
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:bookmarkEnd w:id="8"/>
                </w:p>
            </w:body>
        </w:document>"#,
    );
    let stories = vec![("word/document.xml".to_string(), &doc)];
    let findings = check_bookmark_pairing(&stories);
    assert_eq!(findings.len(), 1);
    assert!(findings[0].message.contains("no matching bookmarkStart"));
}

// ---------------------------------------------------------------------------
// I-DOC-001 / I-DOC-002 / I-DOC-003: Document structure
// ---------------------------------------------------------------------------

#[test]
fn valid_document_structure() {
    let doc = parse_wml(
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p><w:r><w:t>hello</w:t></w:r></w:p>
                <w:sectPr/>
            </w:body>
        </w:document>"#,
    );
    let findings = check_document_root(&doc);
    assert!(findings.is_empty());
}

#[test]
fn wrong_root_element_detected() {
    let doc = parse_wml(
        r#"<w:glossaryDocument xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body><w:sectPr/></w:body>
        </w:glossaryDocument>"#,
    );
    let findings = check_document_root(&doc);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].rule_id, "I-DOC-001");
}

#[test]
fn missing_body_detected() {
    let doc = parse_wml(
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"/>"#,
    );
    let findings = check_document_root(&doc);
    let doc002: Vec<_> = findings
        .iter()
        .filter(|f| f.rule_id == "I-DOC-002")
        .collect();
    assert_eq!(doc002.len(), 1);
}

#[test]
fn body_without_sect_pr_detected() {
    let doc = parse_wml(
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p/>
            </w:body>
        </w:document>"#,
    );
    let findings = check_document_root(&doc);
    let doc003: Vec<_> = findings
        .iter()
        .filter(|f| f.rule_id == "I-DOC-003")
        .collect();
    assert_eq!(doc003.len(), 1);
}

// ---------------------------------------------------------------------------
// I-TC-001 / I-TC-002: Tracked changes
// ---------------------------------------------------------------------------

#[test]
fn valid_tracked_change_passes() {
    let doc = parse_wml(
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:del w:id="1" w:author="a" w:date="2024-01-01T00:00:00Z">
                        <w:r><w:delText>old</w:delText></w:r>
                    </w:del>
                    <w:ins w:id="2" w:author="a" w:date="2024-01-01T00:00:00Z">
                        <w:r><w:t>new</w:t></w:r>
                    </w:ins>
                </w:p>
            </w:body>
        </w:document>"#,
    );
    let stories = vec![("word/document.xml".to_string(), &doc)];
    let findings = check_tracked_change_content_model(&stories);
    assert!(findings.is_empty());
}

#[test]
fn tracked_change_with_nested_hyperlink_fails() {
    let doc = parse_wml(
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:ins w:id="1" w:author="a" w:date="2024-01-01T00:00:00Z">
                        <w:hyperlink>
                            <w:r><w:t>link text</w:t></w:r>
                        </w:hyperlink>
                    </w:ins>
                </w:p>
            </w:body>
        </w:document>"#,
    );
    let stories = vec![("word/document.xml".to_string(), &doc)];
    let findings = check_tracked_change_content_model(&stories);
    let tc001: Vec<_> = findings
        .iter()
        .filter(|f| f.rule_id == "I-TC-001")
        .collect();
    assert_eq!(tc001.len(), 1);
    assert!(tc001[0].message.contains("hyperlink"));
}

#[test]
fn tracked_change_missing_id_fails() {
    let doc = parse_wml(
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:del w:author="a" w:date="2024-01-01T00:00:00Z">
                        <w:r><w:delText>x</w:delText></w:r>
                    </w:del>
                </w:p>
            </w:body>
        </w:document>"#,
    );
    let stories = vec![("word/document.xml".to_string(), &doc)];
    let findings = check_tracked_change_content_model(&stories);
    let tc002: Vec<_> = findings
        .iter()
        .filter(|f| f.rule_id == "I-TC-002")
        .collect();
    assert_eq!(tc002.len(), 1);
}

#[test]
fn tracked_change_with_deeply_nested_forbidden_element() {
    // fldSimple is a paragraph-level-only element (EG_PContent \ EG_ContentRunContent)
    // that must not appear anywhere inside a tracked change, even nested inside w:r.
    // The recursive descendant check should catch this.
    let doc = parse_wml(
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:del w:id="3" w:author="a" w:date="2024-01-01T00:00:00Z">
                        <w:r>
                            <w:fldSimple w:instr="PAGE">
                                <w:r><w:t>1</w:t></w:r>
                            </w:fldSimple>
                        </w:r>
                    </w:del>
                </w:p>
            </w:body>
        </w:document>"#,
    );
    let stories = vec![("word/document.xml".to_string(), &doc)];
    let findings = check_tracked_change_content_model(&stories);
    let tc001: Vec<_> = findings
        .iter()
        .filter(|f| f.rule_id == "I-TC-001")
        .collect();
    assert_eq!(
        tc001.len(),
        1,
        "fldSimple nested inside w:del should be caught by descendant check"
    );
}

#[test]
fn deleted_sdt_with_plain_text_fails() {
    // I-TC-001 deleted-text content model: an inline content control (w:sdt)
    // wrapped in a w:del keeps its inner run as w:t — but inside a deletion run
    // text must be w:delText. Word repairs such a file and accept-all crashes.
    let doc = parse_wml(
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:del w:id="1" w:author="a" w:date="2024-01-01T00:00:00Z">
                        <w:sdt>
                            <w:sdtPr><w:id w:val="9"/></w:sdtPr>
                            <w:sdtContent><w:r><w:t>secret</w:t></w:r></w:sdtContent>
                        </w:sdt>
                    </w:del>
                </w:p>
            </w:body>
        </w:document>"#,
    );
    let stories = vec![("word/document.xml".to_string(), &doc)];
    let findings = check_tracked_change_content_model(&stories);
    let tc001: Vec<_> = findings
        .iter()
        .filter(|f| f.rule_id == "I-TC-001" && f.message.contains("delText"))
        .collect();
    assert_eq!(
        tc001.len(),
        1,
        "w:t inside a deleted sdt must be flagged as needing w:delText; got {findings:?}"
    );
}

#[test]
fn deleted_sdt_with_deltext_passes() {
    // The repaired form (w:delText inside the deleted sdt) is clean.
    let doc = parse_wml(
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:del w:id="1" w:author="a" w:date="2024-01-01T00:00:00Z">
                        <w:sdt>
                            <w:sdtPr><w:id w:val="9"/></w:sdtPr>
                            <w:sdtContent><w:r><w:delText>secret</w:delText></w:r></w:sdtContent>
                        </w:sdt>
                    </w:del>
                </w:p>
            </w:body>
        </w:document>"#,
    );
    let stories = vec![("word/document.xml".to_string(), &doc)];
    let findings = check_tracked_change_content_model(&stories);
    assert!(
        findings.is_empty(),
        "delText inside a deleted sdt is the correct content model; got {findings:?}"
    );
}

#[test]
fn deleted_content_control_textbox_keeps_plain_text() {
    // A textbox (w:txbxContent) is a SEPARATE story: its runs stay w:t even when
    // it sits inside a deleted content control (Word accepts this). The
    // deleted-text-form check must flag the control's OWN run text but not the
    // textbox's.
    let doc = parse_wml(
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:del w:id="1" w:author="a" w:date="2024-01-01T00:00:00Z">
                        <w:sdt>
                            <w:sdtPr><w:id w:val="9"/></w:sdtPr>
                            <w:sdtContent>
                                <w:r><w:delText>control body</w:delText></w:r>
                                <w:r>
                                    <w:drawing>
                                        <w:txbxContent>
                                            <w:p><w:r><w:t>caption</w:t></w:r></w:p>
                                        </w:txbxContent>
                                    </w:drawing>
                                </w:r>
                            </w:sdtContent>
                        </w:sdt>
                    </w:del>
                </w:p>
            </w:body>
        </w:document>"#,
    );
    let stories = vec![("word/document.xml".to_string(), &doc)];
    let findings = check_tracked_change_content_model(&stories);
    let deltext: Vec<_> = findings
        .iter()
        .filter(|f| f.rule_id == "I-TC-001" && f.message.contains("delText"))
        .collect();
    assert!(
        deltext.is_empty(),
        "w:t inside a deleted textbox (separate story) must stay legal; got {findings:?}"
    );
}

#[test]
fn deleted_direct_run_with_plain_text_is_tolerated() {
    // A direct deleted run with w:t (<w:del><w:r><w:t>…) is a tolerated Word
    // quirk, NOT the content-control class — it must not be flagged (matches
    // the serializer, which only rewrites text inside opaque raw XML).
    let doc = parse_wml(
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:del w:id="1" w:author="a" w:date="2024-01-01T00:00:00Z">
                        <w:r><w:t>deleted with w:t</w:t></w:r>
                    </w:del>
                </w:p>
            </w:body>
        </w:document>"#,
    );
    let stories = vec![("word/document.xml".to_string(), &doc)];
    let findings = check_tracked_change_content_model(&stories);
    let deltext: Vec<_> = findings
        .iter()
        .filter(|f| f.rule_id == "I-TC-001" && f.message.contains("delText"))
        .collect();
    assert!(
        deltext.is_empty(),
        "a direct deleted run with w:t is a tolerated quirk and must not be flagged; got {findings:?}"
    );
}

#[test]
fn moved_sdt_keeps_plain_text() {
    // w:moveFrom keeps w:t (not w:delText) — the deleted-text-form check must
    // only fire inside w:del, even for a content control.
    let doc = parse_wml(
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:moveFrom w:id="1" w:author="a" w:date="2024-01-01T00:00:00Z">
                        <w:sdt>
                            <w:sdtPr><w:id w:val="9"/></w:sdtPr>
                            <w:sdtContent><w:r><w:t>moved</w:t></w:r></w:sdtContent>
                        </w:sdt>
                    </w:moveFrom>
                </w:p>
            </w:body>
        </w:document>"#,
    );
    let stories = vec![("word/document.xml".to_string(), &doc)];
    let findings = check_tracked_change_content_model(&stories);
    let deltext: Vec<_> = findings
        .iter()
        .filter(|f| f.rule_id == "I-TC-001" && f.message.contains("delText"))
        .collect();
    assert!(
        deltext.is_empty(),
        "w:t inside w:moveFrom is correct (moves keep w:t); got {findings:?}"
    );
}

// ---------------------------------------------------------------------------
// I-ANN-009: customXml*Range start/end pairing (task #6 commit b)
// ---------------------------------------------------------------------------
//
// ECMA-376 §17.13.5.4-.11: customXmlInsRange / customXmlDelRange /
// customXmlMoveFromRange / customXmlMoveToRange are start/end marker pairs
// (linked by w:id) that delimit the revision-tracked custom-XML markup. A
// start with no matching end (or an end with no start) is a torn pair —
// non-conformant. The transparent-wrapper model (task #6) carries these as
// paired Decoration markers, so a torn pair is now constructible; this check is
// the validator-side safety net (mirrors I-ANN-003 bookmark / I-ANN-005 comment
// pairing).

#[test]
fn customxml_ins_range_balanced_pair_is_clean() {
    let doc = parse_wml(
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:customXmlInsRangeStart w:id="1" w:author="a" w:date="2024-01-01T00:00:00Z"/>
                    <w:customXml w:uri="urn:x" w:element="e"><w:r><w:t>x</w:t></w:r></w:customXml>
                    <w:customXmlInsRangeEnd w:id="1"/>
                </w:p>
            </w:body>
        </w:document>"#,
    );
    let stories = vec![("word/document.xml".to_string(), &doc)];
    let findings = check_custom_xml_range_pairing(&stories);
    let ann009: Vec<_> = findings
        .iter()
        .filter(|f| f.rule_id == "I-ANN-009")
        .collect();
    assert!(
        ann009.is_empty(),
        "a balanced customXmlInsRange pair (same w:id) is conformant; got {ann009:?}"
    );
}

#[test]
fn customxml_ins_range_start_without_end_is_torn() {
    let doc = parse_wml(
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:customXmlInsRangeStart w:id="1" w:author="a" w:date="2024-01-01T00:00:00Z"/>
                    <w:customXml w:uri="urn:x" w:element="e"><w:r><w:t>x</w:t></w:r></w:customXml>
                </w:p>
            </w:body>
        </w:document>"#,
    );
    let stories = vec![("word/document.xml".to_string(), &doc)];
    let findings = check_custom_xml_range_pairing(&stories);
    let ann009: Vec<_> = findings
        .iter()
        .filter(|f| f.rule_id == "I-ANN-009")
        .collect();
    assert_eq!(
        ann009.len(),
        1,
        "ECMA §17.13.5.6: a customXmlInsRangeStart with no matching End is a torn pair; got {ann009:?}"
    );
    assert!(ann009[0].message.contains("customXmlInsRangeStart"));
}

#[test]
fn customxml_del_range_end_without_start_is_torn() {
    let doc = parse_wml(
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:r><w:t>x</w:t></w:r>
                    <w:customXmlDelRangeEnd w:id="7"/>
                </w:p>
            </w:body>
        </w:document>"#,
    );
    let stories = vec![("word/document.xml".to_string(), &doc)];
    let findings = check_custom_xml_range_pairing(&stories);
    let ann009: Vec<_> = findings
        .iter()
        .filter(|f| f.rule_id == "I-ANN-009")
        .collect();
    assert_eq!(
        ann009.len(),
        1,
        "ECMA §17.13.5.5: a customXmlDelRangeEnd with no matching Start is a torn pair; got {ann009:?}"
    );
    assert!(ann009[0].message.contains("customXmlDelRangeEnd"));
}

#[test]
fn customxml_move_ranges_balanced_pairs_are_clean() {
    let doc = parse_wml(
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p>
                    <w:customXmlMoveFromRangeStart w:id="2"/>
                    <w:customXml w:uri="urn:x" w:element="e"><w:r><w:t>B</w:t></w:r></w:customXml>
                    <w:customXmlMoveFromRangeEnd w:id="2"/>
                    <w:customXmlMoveToRangeStart w:id="5"/>
                    <w:customXml w:uri="urn:x" w:element="e"><w:r><w:t>B</w:t></w:r></w:customXml>
                    <w:customXmlMoveToRangeEnd w:id="5"/>
                </w:p>
            </w:body>
        </w:document>"#,
    );
    let stories = vec![("word/document.xml".to_string(), &doc)];
    let findings = check_custom_xml_range_pairing(&stories);
    let ann009: Vec<_> = findings
        .iter()
        .filter(|f| f.rule_id == "I-ANN-009")
        .collect();
    assert!(
        ann009.is_empty(),
        "balanced customXmlMoveFrom/ToRange pairs are conformant; got {ann009:?}"
    );
}
