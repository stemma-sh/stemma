//! Tests for math equation redline (tracked changes) output.
//!
//! Verifies that:
//! 1. Math-containing redline pipelines do not crash (regression for inline opaque handling).
//! 2. Math equation changes use inline math tracking for math-only paragraph edits.
//! 3. The math wrapper (oMath vs oMathPara) is preserved from the source document.
//!
//! Uses testdata fixtures: math-equations, math-wc021, image-math-wc030, image-math-combined.

use std::fs;
use std::io::{Cursor, Read};

use stemma::{DocxRuntime, ExportMode, SimpleRuntime, TransactionMeta};
use xmltree::{Element, XMLNode};
use zip::ZipArchive;

use crate::common;

const WORD_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn redline_meta() -> TransactionMeta {
    TransactionMeta {
        author: "math_redline".to_string(),
        reason: Some("math redline test".to_string()),
        timestamp_utc: Some("2025-06-01T00:00:00Z".to_string()),
    }
}

/// Run the full redline pipeline on a before/after pair and return the
/// exported DOCX bytes.
fn run_redline_pipeline(before_path: &str, after_path: &str) -> Vec<u8> {
    let before_bytes =
        fs::read(before_path).unwrap_or_else(|err| panic!("read {before_path}: {err}"));
    let after_bytes = fs::read(after_path).unwrap_or_else(|err| panic!("read {after_path}: {err}"));

    let runtime = SimpleRuntime::new();

    let import_before = runtime
        .import_docx(&before_bytes)
        .unwrap_or_else(|err| panic!("import {before_path}: {err:?}"));
    let import_after = runtime
        .import_docx(&after_bytes)
        .unwrap_or_else(|err| panic!("import {after_path}: {err:?}"));

    let apply = runtime
        .diff_and_redline(
            &import_before.doc_handle,
            &import_after.doc_handle,
            redline_meta(),
        )
        .unwrap_or_else(|err| panic!("diff_and_redline failed: {err:?}"));
    assert!(apply.applied, "redline must be marked as applied");

    let exported = runtime
        .export_docx(&import_before.doc_handle, ExportMode::Redline)
        .unwrap_or_else(|err| panic!("export_docx failed: {err:?}"));
    assert!(!exported.is_empty(), "exported DOCX must not be empty");

    // Export contract: re-import must succeed.
    let verify = SimpleRuntime::new();
    verify
        .import_docx(&exported)
        .unwrap_or_else(|err| panic!("re-import of exported redline DOCX failed: {err:?}"));

    exported
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

fn extract_document_xml_from_path(path: &str) -> String {
    let bytes = fs::read(path).unwrap_or_else(|err| panic!("read {path}: {err}"));
    extract_document_xml(&bytes)
}

fn local_name(name: &str) -> &str {
    match name.rsplit_once(':') {
        Some((_, local)) => local,
        None => name,
    }
}

/// Count w:del/w:ins elements that contain m:oMath or m:oMathPara descendants
/// (whole-element-level math tracking).
fn count_whole_paragraph_tracked(root: &Element) -> (usize, usize) {
    let mut whole_del = 0;
    let mut whole_ins = 0;
    count_whole_para_recursive(root, &mut whole_del, &mut whole_ins);
    (whole_del, whole_ins)
}

fn count_whole_para_recursive(el: &Element, del_count: &mut usize, ins_count: &mut usize) {
    let local = local_name(&el.name);

    if (local == "del" || local == "ins")
        && (el.namespace.as_deref() == Some(WORD_NS) || el.prefix.as_deref() == Some("w"))
        && has_math_descendant(el)
    {
        if local == "del" {
            *del_count += 1;
        } else {
            *ins_count += 1;
        }
    }

    for child in &el.children {
        if let XMLNode::Element(child_el) = child {
            count_whole_para_recursive(child_el, del_count, ins_count);
        }
    }
}

fn has_math_descendant(el: &Element) -> bool {
    let local = local_name(&el.name);
    if local == "oMath" || local == "oMathPara" {
        return true;
    }
    el.children
        .iter()
        .any(|child| matches!(child, XMLNode::Element(child_el) if has_math_descendant(child_el)))
}

/// Check if document.xml has any m:oMath containing w:del or w:ins children
/// (inline math tracking — the preferred approach).
fn has_inline_math_tracking(root: &Element) -> bool {
    check_inline_math_tracking(root)
}

fn check_inline_math_tracking(el: &Element) -> bool {
    let local = local_name(&el.name);
    if local == "oMath" || local == "oMathPara" {
        return has_tracked_change_descendant(el);
    }
    el.children.iter().any(
        |child| matches!(child, XMLNode::Element(child_el) if check_inline_math_tracking(child_el)),
    )
}

fn has_tracked_change_descendant(el: &Element) -> bool {
    let local = local_name(&el.name);
    if (local == "del" || local == "ins")
        && (el.namespace.as_deref() == Some(WORD_NS) || el.prefix.as_deref() == Some("w"))
    {
        return true;
    }
    el.children
        .iter()
        .any(|child| matches!(child, XMLNode::Element(child_el) if has_tracked_change_descendant(child_el)))
}

// =============================================================================
// Redline pipeline crash regression tests
// =============================================================================

/// The math-equations redline pipeline must not crash.
#[test]
fn math_equations_redline_succeeds() {
    run_redline_pipeline(
        "testdata/math-equations/before.docx",
        "testdata/math-equations/after.docx",
    );
}

/// The math-wc021 redline pipeline must not crash.
#[test]
fn math_wc021_redline_succeeds() {
    run_redline_pipeline(
        "testdata/math-wc021/before.docx",
        "testdata/math-wc021/after.docx",
    );
}

/// The image-math-wc030 redline pipeline must not crash.
#[test]
fn image_math_wc030_redline_succeeds() {
    run_redline_pipeline(
        "testdata/image-math-wc030/before.docx",
        "testdata/image-math-wc030/after.docx",
    );
}

/// The image-math-combined redline pipeline must not crash.
#[test]
fn image_math_combined_redline_succeeds() {
    run_redline_pipeline(
        "testdata/image-math-combined/before.docx",
        "testdata/image-math-combined/after.docx",
    );
}

// =============================================================================
// Math tracking tests
// =============================================================================

/// Math-only paragraph edits should keep the paragraph and track the change
/// inside `m:oMath`. This fixture's `word_redline.docx` uses inline math
/// tracking too, so the expectation is oracle-backed rather than "current
/// Stemma behavior".
#[test]
#[ignore = "requires private corpus (math-equations word_redline.docx, a Word-generated reference); run via just nightly"]
fn math_equations_uses_inline_math_tracking() {
    let docx = run_redline_pipeline(
        "testdata/math-equations/before.docx",
        "testdata/math-equations/after.docx",
    );
    let xml = extract_document_xml(&docx);
    let root = Element::parse(Cursor::new(xml.as_bytes())).expect("parse document.xml");
    let word_redline_path = common::samples_dir().join("math-equations/word_redline.docx");
    let word_xml = extract_document_xml_from_path(&word_redline_path.to_string_lossy());
    let word_root =
        Element::parse(Cursor::new(word_xml.as_bytes())).expect("parse Word document.xml");

    let inline = has_inline_math_tracking(&root);
    let whole = count_whole_paragraph_tracked(&root);
    let word_inline = has_inline_math_tracking(&word_root);
    let word_whole = count_whole_paragraph_tracked(&word_root);

    assert!(
        inline,
        "math-equations should have inline math tracking inside m:oMath"
    );
    assert_eq!(
        whole,
        (0, 0),
        "math-equations should not track the whole math paragraph as a delete/insert"
    );
    assert!(
        word_inline,
        "Word's math-equations redline fixture should use inline math tracking too"
    );
    assert_eq!(
        word_whole,
        (0, 0),
        "Word's math-equations redline fixture should not track the whole paragraph either"
    );
    assert!(
        xml.contains("oMath"),
        "output should still contain math content"
    );
}

// =============================================================================
// Math wrapper preservation tests
// =============================================================================

/// The output must preserve the math wrapper from the source document.
/// If the source uses m:oMathPara (block display), the output should too.
/// If the source uses bare m:oMath (inline), the output should not wrap in m:oMathPara.
#[test]
fn image_math_wc030_preserves_math_wrapper() {
    fn extract_doc_xml(path: &str) -> String {
        let bytes = fs::read(path).unwrap();
        let cursor = Cursor::new(&bytes);
        let mut zip = ZipArchive::new(cursor).unwrap();
        let mut f = zip.by_name("word/document.xml").unwrap();
        let mut s = String::new();
        f.read_to_string(&mut s).unwrap();
        s
    }

    let after_xml = extract_doc_xml("testdata/image-math-wc030/after.docx");

    let after_para = after_xml.matches("oMathPara").count() / 2;

    fn count_bare_omath(xml: &str) -> usize {
        let mut count = 0;
        for (idx, _) in xml.match_indices("oMath") {
            if idx + 9 <= xml.len() && &xml[idx..idx + 9] == "oMathPara" {
                continue;
            }
            if idx >= 4 && &xml[idx - 4..idx] == "Para" {
                continue;
            }
            let prefix = &xml[..idx];
            if prefix.ends_with('<') || prefix.ends_with(':') {
                count += 1;
            }
        }
        count / 2
    }

    let after_bare = count_bare_omath(&after_xml);

    let docx = run_redline_pipeline(
        "testdata/image-math-wc030/before.docx",
        "testdata/image-math-wc030/after.docx",
    );
    let output_xml = extract_document_xml(&docx);
    let output_para = output_xml.matches("oMathPara").count() / 2;
    let output_bare = count_bare_omath(&output_xml);

    // The output wrapper counts should match the after document
    assert_eq!(
        output_para, after_para,
        "oMathPara count in output ({output_para}) should match after doc ({after_para})"
    );

    if after_bare > 0 && after_para == 0 {
        assert!(
            output_bare > 0,
            "Source uses inline m:oMath but output wraps in m:oMathPara. \
             The math wrapper should be preserved from the source document."
        );
    }
}

// =============================================================================
// Math paragraph tracking marker tests
// =============================================================================

/// Every w:p containing an m:oMathPara in the image-math-combined redline must
/// carry tracked changes either on the paragraph mark or inside the OMML tree.
///
/// Word uses both strategies:
/// - whole inserted display-math paragraphs use paragraph-mark tracking
/// - display-math replaced by an empty paragraph keeps the paragraph and places
///   `w:del` inside `m:r` / `m:ctrlPr`
#[test]
fn image_math_combined_omath_para_paragraphs_have_tracking_markers() {
    let docx = run_redline_pipeline(
        "testdata/image-math-combined/before.docx",
        "testdata/image-math-combined/after.docx",
    );
    let xml = extract_document_xml(&docx);
    let root = Element::parse(Cursor::new(xml.as_bytes())).expect("parse document.xml");

    let violations = collect_omath_para_paragraphs_without_tracking(&root);

    assert!(
        violations.is_empty(),
        "Every w:p containing m:oMathPara must have tracked changes either in w:pPr/w:rPr \
         or inside the OMML tree. \
         Found {} paragraph(s) without a tracking marker.",
        violations.len()
    );
}

/// Collect paragraph indices for every w:p that contains an m:oMathPara child
/// but lacks both paragraph-mark tracking and internal OMML tracked changes.
fn collect_omath_para_paragraphs_without_tracking(root: &Element) -> Vec<usize> {
    let mut violations = Vec::new();
    collect_violations_recursive(root, &mut violations, &mut 0);
    violations
}

fn collect_violations_recursive(el: &Element, violations: &mut Vec<usize>, para_idx: &mut usize) {
    let local = local_name(&el.name);
    if local == "p"
        && (el.namespace.as_deref() == Some(WORD_NS) || el.prefix.as_deref() == Some("w"))
    {
        let idx = *para_idx;
        *para_idx += 1;
        if paragraph_has_omath_para_child(el)
            && !paragraph_has_ppr_rpr_tracking(el)
            && !paragraph_has_internal_math_tracking(el)
        {
            violations.push(idx);
        }
        // Don't recurse into children — w:p is a leaf in document structure
        return;
    }
    for child in &el.children {
        if let XMLNode::Element(child_el) = child {
            collect_violations_recursive(child_el, violations, para_idx);
        }
    }
}

/// Returns true if the w:p element has a direct m:oMathPara child.
fn paragraph_has_omath_para_child(p: &Element) -> bool {
    p.children
        .iter()
        .any(|child| matches!(child, XMLNode::Element(el) if local_name(&el.name) == "oMathPara"))
}

/// Returns true if w:p has w:pPr/w:rPr containing a w:del or w:ins element.
fn paragraph_has_ppr_rpr_tracking(p: &Element) -> bool {
    let Some(ppr) = p.children.iter().find_map(|child| {
        if let XMLNode::Element(el) = child
            && local_name(&el.name) == "pPr"
        {
            return Some(el);
        }
        None
    }) else {
        return false;
    };
    let Some(rpr) = ppr.children.iter().find_map(|child| {
        if let XMLNode::Element(el) = child
            && local_name(&el.name) == "rPr"
        {
            return Some(el);
        }
        None
    }) else {
        return false;
    };
    rpr.children.iter().any(|child| {
        if let XMLNode::Element(el) = child {
            let ln = local_name(&el.name);
            return (ln == "del" || ln == "ins")
                && (el.namespace.as_deref() == Some(WORD_NS) || el.prefix.as_deref() == Some("w"));
        }
        false
    })
}

fn paragraph_has_internal_math_tracking(p: &Element) -> bool {
    fn has_tracking(el: &Element) -> bool {
        let ln = local_name(&el.name);
        if (ln == "del" || ln == "ins")
            && (el.namespace.as_deref() == Some(WORD_NS) || el.prefix.as_deref() == Some("w"))
        {
            return true;
        }
        el.children.iter().any(|child| match child {
            XMLNode::Element(child_el) => has_tracking(child_el),
            _ => false,
        })
    }

    p.children.iter().any(|child| match child {
        XMLNode::Element(el) if local_name(&el.name) == "oMathPara" => has_tracking(el),
        _ => false,
    })
}
