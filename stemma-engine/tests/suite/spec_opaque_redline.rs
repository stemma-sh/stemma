//! Spec-compliance tests: Paragraph-level opaques must NOT be inside w:del/w:ins
//!
//! Per OOXML (ECMA-376 Annex A, CT_RunTrackChange), w:del and w:ins can only
//! contain EG_ContentRunContent (runs, smartTag, sdt, etc.) — NOT w:hyperlink
//! or w:fldSimple. These must be direct w:p children.
//!
//! Fixtures are in `testdata/synthesized/opaque-redline-hyperlink/` and
//! `testdata/synthesized/opaque-redline-field/`.

use std::fs;
use std::io::{Cursor, Read};
#[allow(unused_imports)]
use stemma_diff::test_support::{DocumentComparisonExt as _, RuntimeComparisonExt as _};

use stemma::{DocxRuntime, ExportMode, SimpleRuntime, TransactionMeta};
use xmltree::{Element, XMLNode};
use zip::ZipArchive;

// ── helpers ──────────────────────────────────────────────────────────────

fn redline_export(before_path: &str, after_path: &str) -> Vec<u8> {
    let before = fs::read(before_path).unwrap_or_else(|e| panic!("read {before_path}: {e}"));
    let after = fs::read(after_path).unwrap_or_else(|e| panic!("read {after_path}: {e}"));

    let runtime = SimpleRuntime::new();
    let ib = runtime.import_docx(&before).expect("import before");
    let ia = runtime.import_docx(&after).expect("import after");

    let meta = TransactionMeta {
        author: "spec_opaque_redline".to_string(),
        reason: Some("opaque redline spec test".to_string()),
        timestamp_utc: Some("2025-01-15T10:30:00Z".to_string()),
    };
    let apply = runtime
        .diff_and_redline(&ib.doc_handle, &ia.doc_handle, meta)
        .expect("diff_and_redline");
    assert!(apply.applied, "redline should be applied");

    runtime
        .export_docx(&ib.doc_handle, ExportMode::Redline)
        .expect("export redline")
}

fn extract_document_xml(docx_bytes: &[u8]) -> String {
    let cursor = Cursor::new(docx_bytes);
    let mut zip = ZipArchive::new(cursor).expect("open DOCX zip");
    let mut file = zip
        .by_name("word/document.xml")
        .expect("DOCX must contain word/document.xml");
    let mut out = String::new();
    file.read_to_string(&mut out)
        .expect("read word/document.xml");
    out
}

fn parse_xml(xml: &str) -> Element {
    Element::parse(Cursor::new(xml.as_bytes())).expect("parse XML")
}

fn local_name(name: &str) -> &str {
    match name.rsplit_once(':') {
        Some((_, local)) => local,
        None => name,
    }
}

/// Check whether any element with `child_tag` appears as a descendant of
/// any element with `parent_tag`. Returns the first offending path found.
fn find_illegal_nesting(root: &Element, parent_tag: &str, child_tag: &str) -> Option<String> {
    fn search_for_parent(
        el: &Element,
        parent_tag: &str,
        child_tag: &str,
        path: &str,
    ) -> Option<String> {
        let tag = local_name(&el.name);
        let current_path = format!("{}/{}", path, el.name);

        if tag == parent_tag {
            // We're inside the parent — now look for the child anywhere below.
            if let Some(child_path) = search_for_child(el, child_tag, &current_path) {
                return Some(child_path);
            }
        }

        // Keep searching in children for parent elements.
        for child in &el.children {
            if let XMLNode::Element(child_el) = child
                && let Some(found) =
                    search_for_parent(child_el, parent_tag, child_tag, &current_path)
            {
                return Some(found);
            }
        }
        None
    }

    fn search_for_child(el: &Element, child_tag: &str, path: &str) -> Option<String> {
        for child in &el.children {
            if let XMLNode::Element(child_el) = child {
                let child_path = format!("{}/{}", path, child_el.name);
                if local_name(&child_el.name) == child_tag {
                    return Some(child_path);
                }
                if let Some(found) = search_for_child(child_el, child_tag, &child_path) {
                    return Some(found);
                }
            }
        }
        None
    }

    search_for_parent(root, parent_tag, child_tag, "")
}

fn find_all_elements<'a>(root: &'a Element, tag: &str, out: &mut Vec<&'a Element>) {
    if local_name(&root.name) == tag {
        out.push(root);
    }
    for child in &root.children {
        if let XMLNode::Element(el) = child {
            find_all_elements(el, tag, out);
        }
    }
}

// ── hyperlink fixtures ───────────────────────────────────────────────────

const HYPERLINK_BEFORE: &str = "testdata/synthesized/opaque-redline-hyperlink/before.docx";
const HYPERLINK_AFTER: &str = "testdata/synthesized/opaque-redline-hyperlink/after.docx";

fn hyperlink_xml() -> (String, Element) {
    let exported = redline_export(HYPERLINK_BEFORE, HYPERLINK_AFTER);
    let xml = extract_document_xml(&exported);
    let root = parse_xml(&xml);
    (xml, root)
}

/// ECMA-376 Annex A: w:hyperlink must NOT appear inside w:del.
#[test]
fn spec_hyperlink_not_inside_del() {
    let (_xml, root) = hyperlink_xml();
    if let Some(path) = find_illegal_nesting(&root, "del", "hyperlink") {
        panic!(
            "w:hyperlink found inside w:del — violates CT_RunTrackChange content model.\nPath: {path}"
        );
    }
}

/// ECMA-376 Annex A: w:hyperlink must NOT appear inside w:ins.
#[test]
fn spec_hyperlink_not_inside_ins() {
    let (_xml, root) = hyperlink_xml();
    if let Some(path) = find_illegal_nesting(&root, "ins", "hyperlink") {
        panic!(
            "w:hyperlink found inside w:ins — violates CT_RunTrackChange content model.\nPath: {path}"
        );
    }
}

/// Hyperlink must still exist in the output exactly once (not dropped, not duplicated).
#[test]
fn spec_hyperlink_preserved_in_output() {
    let (_xml, root) = hyperlink_xml();
    let mut hyperlinks = Vec::new();
    find_all_elements(&root, "hyperlink", &mut hyperlinks);
    assert_eq!(
        hyperlinks.len(),
        1,
        "w:hyperlink should appear exactly once in redline output (not duplicated across del/ins segments), found {}",
        hyperlinks.len()
    );
}

/// Text changes around hyperlink must still produce tracked changes.
#[test]
fn spec_hyperlink_redline_has_tracked_text() {
    let (_xml, root) = hyperlink_xml();
    let mut dels = Vec::new();
    find_all_elements(&root, "del", &mut dels);
    let mut inss = Vec::new();
    find_all_elements(&root, "ins", &mut inss);
    assert!(
        !dels.is_empty() || !inss.is_empty(),
        "redline should contain tracked changes (w:del or w:ins) for the text modifications"
    );
}

// ── fldSimple fixtures ───────────────────────────────────────────────────

const FIELD_BEFORE: &str = "testdata/synthesized/opaque-redline-field/before.docx";
const FIELD_AFTER: &str = "testdata/synthesized/opaque-redline-field/after.docx";

fn field_xml() -> (String, Element) {
    let exported = redline_export(FIELD_BEFORE, FIELD_AFTER);
    let xml = extract_document_xml(&exported);
    let root = parse_xml(&xml);
    (xml, root)
}

/// ECMA-376 Annex A: w:fldSimple must NOT appear inside w:del.
#[test]
fn spec_fldsimple_not_inside_del() {
    let (_xml, root) = field_xml();
    if let Some(path) = find_illegal_nesting(&root, "del", "fldSimple") {
        panic!(
            "w:fldSimple found inside w:del — violates CT_RunTrackChange content model.\nPath: {path}"
        );
    }
}

/// ECMA-376 Annex A: w:fldSimple must NOT appear inside w:ins.
#[test]
fn spec_fldsimple_not_inside_ins() {
    let (_xml, root) = field_xml();
    if let Some(path) = find_illegal_nesting(&root, "ins", "fldSimple") {
        panic!(
            "w:fldSimple found inside w:ins — violates CT_RunTrackChange content model.\nPath: {path}"
        );
    }
}

/// fldSimple must still exist in the output exactly once (not dropped, not duplicated).
#[test]
fn spec_fldsimple_preserved_in_output() {
    let (_xml, root) = field_xml();
    let mut fields = Vec::new();
    find_all_elements(&root, "fldSimple", &mut fields);
    assert_eq!(
        fields.len(),
        1,
        "w:fldSimple should appear exactly once in redline output, found {}",
        fields.len()
    );
}

// ── opaque-roundtrip sample (real-world fixture) ─────────────────────────

/// The smaller synthesized fixtures above continue to prove legal hyperlink
/// and field ordering. This broader sample also contains an unrelated table-row
/// replacement followed by later story changes. Native Word Review Reject All
/// hangs on that carrier order, so comparison must refuse before producing an
/// artifact rather than weakening the opaque-ordering guarantees.
#[test]
fn spec_opaque_roundtrip_refuses_the_unqualified_table_carrier_order() {
    let before_path = std::path::PathBuf::from("testdata").join("opaque-roundtrip/before.docx");
    let after_path = std::path::PathBuf::from("testdata").join("opaque-roundtrip/after.docx");
    let before = fs::read(&before_path).expect("read opaque-roundtrip before");
    let after = fs::read(&after_path).expect("read opaque-roundtrip after");
    let runtime = SimpleRuntime::new();
    let ib = runtime.import_docx(&before).expect("import before");
    let ia = runtime.import_docx(&after).expect("import after");
    let error = runtime
        .diff_and_redline(
            &ib.doc_handle,
            &ia.doc_handle,
            TransactionMeta {
                author: "spec_opaque_redline".to_string(),
                reason: Some("opaque redline carrier boundary".to_string()),
                timestamp_utc: Some("2025-01-15T10:30:00Z".to_string()),
            },
        )
        .expect_err("unsafe table carrier order must refuse");

    assert_eq!(error.code, stemma::ErrorCode::UnsupportedEdit);
    assert!(error.message.contains("table-row replacement"));
    assert!(error.message.contains("followed by another change"));
}
