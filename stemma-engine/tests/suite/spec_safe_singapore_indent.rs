//! Spec-compliance test: SAFE US vs Singapore indent inheritance.
//!
//! Property under test:
//!
//! **Per-attribute cascade** — ECMA-376 §17.3.1.12: "Indentation settings are
//! overridden on an individual basis."  When direct `w:ind` specifies
//! `left="-720"` but omits `firstLine`, the absent `firstLine` inherits from
//! the style chain (Normal's `firstLine="720"`).
//!
//! All indent attributes (`left`, `right`, `firstLine`, `hanging`) cascade
//! independently through direct > numbering > style.

use std::fs;
use std::io::{Cursor, Read};

use stemma::{CanonDoc, DocxRuntime, ExportMode, Mark, SimpleRuntime, TransactionMeta};
use xmltree::{Element, XMLNode};
use zip::ZipArchive;

use crate::common;

const WORD_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn import_doc(name: &str) -> (SimpleRuntime, CanonDoc) {
    let path = format!("testdata/safe-us-vs-singapore/{name}.docx");
    let bytes = fs::read(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    let runtime = SimpleRuntime::new();
    let import = runtime
        .import_docx(&bytes)
        .unwrap_or_else(|e| panic!("import {path}: {e:?}"));
    let view = runtime.view(&import.doc_handle).expect("view");
    (runtime, std::sync::Arc::unwrap_or_clone(view.canonical))
}
/// Find the "Events" heading paragraph in a SAFE document.
/// It has literal_prefix "1." and body text "Events".
fn find_events_paragraph(doc: &CanonDoc) -> &stemma::ParagraphNode {
    common::all_paragraphs(doc)
        .into_iter()
        .find(|p| {
            p.literal_prefix.as_deref() == Some("1.")
                && common::paragraph_text(p).contains("Events")
        })
        .expect("should find '1. Events' paragraph")
}

/// Find the "Post-Money Valuation Cap" paragraph (the one immediately
/// before "1. Events").
fn find_postmoney_paragraph(doc: &CanonDoc) -> &stemma::ParagraphNode {
    common::all_paragraphs(doc)
        .into_iter()
        .find(|p| common::paragraph_text(p).contains("Post-Money Valuation Cap"))
        .expect("should find 'Post-Money Valuation Cap' paragraph")
}

fn find_primary_footer_page_number_paragraph(doc: &CanonDoc) -> &stemma::ParagraphNode {
    let footer = doc
        .footers
        .iter()
        .find(|footer| footer.part_name == "footer1.xml")
        .expect("should find primary footer story");
    footer
        .blocks
        .iter()
        .find_map(|tracked| match &tracked.block {
            stemma::BlockNode::Paragraph(p)
                if common::paragraph_text(p).contains("-6-")
                    || common::paragraph_text(p).contains("-2-") =>
            {
                Some(p)
            }
            _ => None,
        })
        .expect("should find page-number footer paragraph")
}

fn find_paragraph_containing<'a>(doc: &'a CanonDoc, needle: &str) -> &'a stemma::ParagraphNode {
    common::all_paragraphs(doc)
        .into_iter()
        .find(|p| common::paragraph_text(p).contains(needle))
        .unwrap_or_else(|| panic!("should find paragraph containing {needle:?}"))
}

fn is_w_tag(element: &Element, local: &str) -> bool {
    let name_local = match element.name.rsplit_once(':') {
        Some((_, l)) => l,
        None => &element.name,
    };
    if name_local != local {
        return false;
    }
    if element.prefix.as_deref() == Some("w") {
        return true;
    }
    if element.namespace.as_deref() == Some(WORD_NS) {
        return true;
    }
    element.name == format!("w:{local}")
}

fn find_w_child<'a>(parent: &'a Element, tag: &str) -> Option<&'a Element> {
    parent.children.iter().find_map(|child| match child {
        XMLNode::Element(el) if is_w_tag(el, tag) => Some(el),
        _ => None,
    })
}

fn attr_value<'a>(element: &'a Element, qname: &str) -> Option<&'a str> {
    let local = qname.rsplit_once(':').map(|(_, l)| l).unwrap_or(qname);
    element
        .attributes
        .iter()
        .find_map(|(name, value)| (name.local_name == local).then_some(value.as_str()))
}

fn generate_redline_docx(before: &[u8], after: &[u8]) -> Vec<u8> {
    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(before).expect("import before");
    let import_after = runtime.import_docx(after).expect("import after");
    runtime
        .diff_and_redline(
            &import_before.doc_handle,
            &import_after.doc_handle,
            TransactionMeta {
                author: "Stemma".to_string(),
                reason: Some("SAFE Singapore indent regression".to_string()),
                timestamp_utc: Some("2026-03-26T00:00:00Z".to_string()),
            },
        )
        .expect("diff_and_redline");
    runtime
        .export_docx(&import_before.doc_handle, ExportMode::Redline)
        .expect("export redline")
}

fn generate_redline_document_xml(before: &[u8], after: &[u8]) -> Element {
    let redline = generate_redline_docx(before, after);
    let mut zip = ZipArchive::new(Cursor::new(redline)).expect("open redline zip");
    let mut file = zip.by_name("word/document.xml").expect("word/document.xml");
    let mut xml = String::new();
    file.read_to_string(&mut xml).expect("read document.xml");
    Element::parse(Cursor::new(xml.as_bytes())).expect("parse document.xml")
}

fn generate_redline_story_xml(before: &[u8], after: &[u8], part_name: &str) -> Element {
    let redline = generate_redline_docx(before, after);
    let mut zip = ZipArchive::new(Cursor::new(redline)).expect("open redline zip");
    let mut file = zip
        .by_name(part_name)
        .unwrap_or_else(|e| panic!("{part_name}: {e}"));
    let mut xml = String::new();
    file.read_to_string(&mut xml)
        .unwrap_or_else(|e| panic!("read {part_name}: {e}"));
    Element::parse(Cursor::new(xml.as_bytes())).unwrap_or_else(|e| panic!("parse {part_name}: {e}"))
}

fn find_dissolution_event_paragraph(root: &Element) -> &Element {
    fn collect_text(el: &Element, out: &mut String) {
        if is_w_tag(el, "t")
            && let Some(text) = el.get_text()
        {
            out.push_str(&text);
        }
        for child in &el.children {
            if let XMLNode::Element(node) = child {
                collect_text(node, out);
            }
        }
    }

    fn visit(el: &Element) -> Option<&Element> {
        if is_w_tag(el, "p") {
            let mut text = String::new();
            collect_text(el, &mut text);
            if text.contains("Dissolution Event before the termination") {
                return Some(el);
            }
        }
        for child in &el.children {
            if let XMLNode::Element(node) = child
                && let Some(found) = visit(node)
            {
                return Some(found);
            }
        }
        None
    }

    visit(root).expect("should find Dissolution Event paragraph")
}

fn find_events_heading_paragraph(root: &Element) -> &Element {
    fn collect_text(el: &Element, out: &mut String) {
        if is_w_tag(el, "t")
            && let Some(text) = el.get_text()
        {
            out.push_str(&text);
        }
        for child in &el.children {
            if let XMLNode::Element(node) = child {
                collect_text(node, out);
            }
        }
    }

    fn visit(el: &Element) -> Option<&Element> {
        if is_w_tag(el, "p") {
            let mut text = String::new();
            collect_text(el, &mut text);
            if text.contains("Events") && text.contains("1.") {
                return Some(el);
            }
        }
        for child in &el.children {
            if let XMLNode::Element(node) = child
                && let Some(found) = visit(node)
            {
                return Some(found);
            }
        }
        None
    }

    visit(root).expect("should find Events heading paragraph")
}

fn find_paragraph_containing_xml<'a>(root: &'a Element, needle: &str) -> &'a Element {
    fn collect_text(el: &Element, out: &mut String) {
        if (is_w_tag(el, "t") || is_w_tag(el, "delText"))
            && let Some(text) = el.get_text()
        {
            out.push_str(&text);
        }
        for child in &el.children {
            if let XMLNode::Element(node) = child {
                collect_text(node, out);
            }
        }
    }

    fn visit<'a>(el: &'a Element, needle: &str) -> Option<&'a Element> {
        if is_w_tag(el, "p") {
            let mut text = String::new();
            collect_text(el, &mut text);
            if text.contains(needle) {
                return Some(el);
            }
        }
        for child in &el.children {
            if let XMLNode::Element(node) = child
                && let Some(found) = visit(node, needle)
            {
                return Some(found);
            }
        }
        None
    }

    visit(root, needle).unwrap_or_else(|| panic!("should find paragraph containing {needle:?}"))
}

fn run_has_italic(run: &Element) -> bool {
    find_w_child(run, "rPr")
        .and_then(|rpr| find_w_child(rpr, "i"))
        .is_some()
}

fn run_text(run: &Element) -> String {
    fn collect_text(el: &Element, out: &mut String) {
        if is_w_tag(el, "t")
            && let Some(text) = el.get_text()
        {
            out.push_str(&text);
        }
        for child in &el.children {
            if let XMLNode::Element(node) = child {
                collect_text(node, out);
            }
        }
    }

    let mut text = String::new();
    collect_text(run, &mut text);
    text
}

fn run_color_value(run: &Element) -> Option<&str> {
    find_w_child(run, "rPr")
        .and_then(|rpr| find_w_child(rpr, "color"))
        .and_then(|color| attr_value(color, "w:val"))
}

fn collect_runs<'a>(el: &'a Element, out: &mut Vec<&'a Element>) {
    if is_w_tag(el, "r") {
        out.push(el);
    }
    for child in &el.children {
        if let XMLNode::Element(node) = child {
            collect_runs(node, out);
        }
    }
}

fn run_contains_field_code(run: &Element) -> bool {
    run.children.iter().any(|child| {
        matches!(
            child,
            XMLNode::Element(el) if is_w_tag(el, "fldChar") || is_w_tag(el, "instrText")
        )
    })
}

// ==========================================================================
// Test: "Events" heading — firstLine inherits from Normal style
// ==========================================================================

/// The "Events" paragraph has direct `w:ind w:left="-720"` without firstLine.
/// Per §17.3.1.12 per-attribute cascade: absent firstLine inherits from
/// Normal style's firstLine=720.  First line starts at -720+720 = 0.
#[test]
fn safe_singapore_events_heading_inherits_first_line_from_style() {
    let (_rt, doc) = import_doc("before");
    let events = find_events_paragraph(&doc);

    // The Events paragraph has a literal prefix "1." stripped.
    assert_eq!(
        events.literal_prefix.as_deref(),
        Some("1."),
        "Events paragraph should have literal_prefix '1.'"
    );

    let indent = events.indent.as_ref().expect("Events should have indent");
    assert_eq!(
        indent.left,
        Some(-720),
        "Events left indent should be -720 twips"
    );
    // Per-attribute cascade: absent firstLine inherits 720 from Normal.
    assert_eq!(
        indent.effective_first_line_twips,
        Some(720),
        "Events firstLine should inherit 720 from Normal style (per-attribute cascade)"
    );
}

/// The "Post-Money Valuation Cap" paragraph has direct `w:ind w:left="-720"`
/// without firstLine.  Per §17.3.1.12 per-attribute cascade: absent firstLine
/// inherits from Normal style's firstLine=720.
#[test]
fn safe_singapore_postmoney_inherits_first_line_from_style() {
    let (_rt, doc) = import_doc("before");
    let postmoney = find_postmoney_paragraph(&doc);

    let indent = postmoney
        .indent
        .as_ref()
        .expect("Post-Money paragraph should have indent");
    assert_eq!(
        indent.left,
        Some(-720),
        "Post-Money left indent should be -720 twips"
    );
    // Per-attribute cascade: absent firstLine inherits 720 from Normal.
    assert_eq!(
        indent.effective_first_line_twips,
        Some(720),
        "Post-Money firstLine should inherit 720 from Normal style (per-attribute cascade)"
    );
}

/// Both "Events" and "Post-Money" have the same indent model:
/// left=-720, firstLine=720 (inherited from Normal).  First-line text
/// position = -720 + 720 = 0 (at the text body edge).
#[test]
fn safe_singapore_events_aligns_with_postmoney() {
    let (_rt, doc) = import_doc("before");

    let events = find_events_paragraph(&doc);
    let postmoney = find_postmoney_paragraph(&doc);

    let events_indent = events.indent.as_ref().unwrap();
    let postmoney_indent = postmoney.indent.as_ref().unwrap();

    // Both have left=-720 and firstLine=720 (from Normal), so first-line text position = 0.
    let events_text_pos =
        events_indent.left.unwrap_or(0) + events_indent.effective_first_line_twips.unwrap_or(0);
    let postmoney_text_pos = postmoney_indent.left.unwrap_or(0)
        + postmoney_indent.effective_first_line_twips.unwrap_or(0);

    assert_eq!(
        events_text_pos, postmoney_text_pos,
        "Events and Post-Money should have the same first-line text position. \
         Events at {events_text_pos}, Post-Money at {postmoney_text_pos}."
    );
    assert_eq!(
        events_text_pos, 0,
        "Both first lines start at 0 twips (text body edge)"
    );
}

#[test]
fn safe_singapore_events_heading_preserves_prefix_run_formatting() {
    let (_rt, doc) = import_doc("after");
    let events = find_events_paragraph(&doc);
    let body = events
        .first_content_text_node()
        .expect("Events heading should have body text");

    assert_eq!(events.literal_prefix.as_deref(), Some("1."));
    assert!(
        events.literal_prefix_marks.contains(&Mark::Bold),
        "heading prefix should keep bold formatting"
    );
    assert!(
        !events.literal_prefix_marks.contains(&Mark::Italic),
        "heading prefix should not inherit body italics"
    );
    assert!(
        body.marks.contains(&Mark::Italic),
        "heading body text should remain italic"
    );
}

#[test]
fn safe_singapore_after_import_preserves_footer_page_number_text_formatting() {
    let (_rt, doc) = import_doc("after");
    let paragraph = find_primary_footer_page_number_paragraph(&doc);
    let page_number_run = paragraph
        .all_inlines()
        .find_map(|inline| match inline {
            stemma::InlineNode::Text(text) if text.text == "6" => Some(text),
            _ => None,
        })
        .expect("footer page number paragraph should contain text run '6'");

    assert_eq!(
        page_number_run.style_props.char_style_id.as_deref(),
        Some("PageNumber"),
        "import should retain the PageNumber character style on the field result run"
    );
    assert_eq!(
        page_number_run.style_props.font_size,
        Some(22),
        "import should retain the 11pt explicit size on the field result run"
    );

    let trailing_hyphen_run = paragraph
        .all_inlines()
        .filter_map(|inline| match inline {
            stemma::InlineNode::Text(text) if text.text == "-" => Some(text),
            _ => None,
        })
        .last()
        .expect("footer page number paragraph should contain trailing '-' run");

    assert_eq!(
        trailing_hyphen_run.style_props.char_style_id.as_deref(),
        Some("PageNumber"),
        "import should retain the PageNumber character style on the trailing hyphen run"
    );
    assert_eq!(
        trailing_hyphen_run.style_props.font_size,
        Some(22),
        "import should retain the 11pt explicit size on the trailing hyphen run"
    );
}

#[test]
fn safe_singapore_after_import_preserves_footer_page_field_instruction_metadata() {
    let (_rt, doc) = import_doc("after");
    let paragraph = find_primary_footer_page_number_paragraph(&doc);
    let instruction = paragraph
        .all_inlines()
        .find_map(|inline| match inline {
            stemma::InlineNode::OpaqueInline(opaque) => match &opaque.kind {
                stemma::OpaqueKind::Field(data)
                    if data.field_kind == stemma::FieldKind::Instruction =>
                {
                    data.instruction_text.as_deref()
                }
                _ => None,
            },
            _ => None,
        })
        .expect("footer page number paragraph should contain PAGE field instruction");

    assert_eq!(instruction.trim(), "PAGE");
}

#[test]
fn safe_singapore_after_import_preserves_direct_auto_color_runs() {
    let (_rt, doc) = import_doc("after");

    let liquidity_paragraph = find_paragraph_containing(&doc, "the Liquidity Capitalization");
    let liquidity_run = liquidity_paragraph
        .all_inlines()
        .find_map(|inline| match inline {
            stemma::InlineNode::Text(text) if text.text == "the Liquidity Capitalization" => {
                Some(text)
            }
            _ => None,
        })
        .expect("Liquidity Price paragraph should contain the highlighted term run");
    assert_eq!(
        liquidity_run.style_props.color.as_deref(),
        Some("auto"),
        "direct auto color should survive import on body runs",
    );
    assert!(
        liquidity_run.rpr_authored.color,
        "direct auto color should mark the run as having direct color",
    );

    let provision_paragraph =
        find_paragraph_containing(&doc, "In the event any one or more of the provisions");
    assert_eq!(provision_paragraph.literal_prefix.as_deref(), Some("(e)"));
    assert_eq!(
        provision_paragraph
            .literal_prefix_style_props
            .color
            .as_deref(),
        Some("auto"),
        "direct auto color should survive import on stripped literal prefixes",
    );
}

#[test]
fn safe_singapore_redline_keeps_heading_prefix_non_italic() {
    let before = fs::read("testdata/safe-us-vs-singapore/before.docx").expect("read before");
    let after = fs::read("testdata/safe-us-vs-singapore/after.docx").expect("read after");
    let root = generate_redline_document_xml(&before, &after);
    let paragraph = find_events_heading_paragraph(&root);

    let runs: Vec<&Element> = paragraph
        .children
        .iter()
        .filter_map(|child| match child {
            XMLNode::Element(el) if is_w_tag(el, "r") => Some(el),
            _ => None,
        })
        .collect();

    let prefix_run = runs
        .iter()
        .copied()
        .find(|run| run_text(run) == "1.")
        .expect("Events heading should contain a literal prefix run");
    let body_run = runs
        .iter()
        .copied()
        .find(|run| run_text(run).contains("Events"))
        .expect("Events heading should contain a body text run");

    assert!(
        !run_has_italic(prefix_run),
        "serializer should not apply body italics to the literal prefix run"
    );
    assert!(
        run_has_italic(body_run),
        "serializer should preserve italics on the heading body run"
    );
}

#[test]
fn safe_singapore_redline_preserves_empty_first_page_footer_style() {
    let before = fs::read("testdata/safe-us-vs-singapore/before.docx").expect("read before");
    let after = fs::read("testdata/safe-us-vs-singapore/after.docx").expect("read after");
    let redline = generate_redline_docx(&before, &after);
    let mut zip = ZipArchive::new(Cursor::new(redline)).expect("open redline zip");
    let mut file = zip.by_name("word/footer3.xml").expect("word/footer3.xml");
    let mut xml = String::new();
    file.read_to_string(&mut xml).expect("read footer3.xml");
    let root = Element::parse(Cursor::new(xml.as_bytes())).expect("parse footer3.xml");
    let paragraph = root
        .children
        .iter()
        .find_map(|child| match child {
            XMLNode::Element(el) if is_w_tag(el, "p") => Some(el),
            _ => None,
        })
        .expect("footer3 should contain a paragraph");
    let ppr = find_w_child(paragraph, "pPr").expect("footer paragraph should have pPr");
    let pstyle =
        find_w_child(ppr, "pStyle").expect("empty footer paragraph should keep Footer style");

    assert_eq!(
        attr_value(pstyle, "w:val"),
        Some("Footer"),
        "formatting-only footer story changes must preserve the target footer paragraph style"
    );

    let paragraph_mark_rpr = find_w_child(ppr, "rPr")
        .expect("empty footer paragraph should serialize paragraph-mark rPr when tracked formatting changed");
    assert!(
        find_w_child(paragraph_mark_rpr, "sz").is_none(),
        "current paragraph-mark rPr should not keep the removed 9pt direct size"
    );
    assert!(
        find_w_child(paragraph_mark_rpr, "color").is_none(),
        "current paragraph-mark rPr should not keep the removed direct color"
    );

    let rpr_change = find_w_child(paragraph_mark_rpr, "rPrChange")
        .expect("removed paragraph-mark formatting should be represented as rPrChange");
    let previous_rpr = find_w_child(rpr_change, "rPr")
        .expect("rPrChange should contain the previous paragraph-mark rPr snapshot");
    let previous_sz = find_w_child(previous_rpr, "sz")
        .expect("previous paragraph-mark rPr should keep the removed 9pt size");
    let previous_color = find_w_child(previous_rpr, "color")
        .expect("previous paragraph-mark rPr should keep the removed direct color");

    assert_eq!(attr_value(previous_sz, "w:val"), Some("18"));
    assert_eq!(attr_value(previous_color, "w:val"), Some("222222"));

    let ppr_change = find_w_child(ppr, "pPrChange")
        .expect("footer paragraph should still track the previous paragraph properties");
    let previous_ppr = find_w_child(ppr_change, "pPr")
        .expect("pPrChange should contain the previous paragraph properties");
    assert!(
        find_w_child(previous_ppr, "rPr").is_none(),
        "previous paragraph-mark rPr should be tracked via rPrChange, not duplicated in pPrChange"
    );
}

#[test]
fn safe_singapore_redline_preserves_footer_page_number_field_run_formatting() {
    let before = fs::read("testdata/safe-us-vs-singapore/before.docx").expect("read before");
    let after = fs::read("testdata/safe-us-vs-singapore/after.docx").expect("read after");
    let root = generate_redline_story_xml(&before, &after, "word/footer1.xml");

    let mut runs = Vec::new();
    collect_runs(&root, &mut runs);
    let field_runs: Vec<&Element> = runs
        .into_iter()
        .filter(|run| run_contains_field_code(run))
        .collect();

    assert!(
        !field_runs.is_empty(),
        "footer1 should contain PAGE field runs"
    );

    for run in field_runs {
        let rpr = find_w_child(run, "rPr")
            .expect("field-code runs must keep run properties when wrapped during serialization");
        let rstyle = find_w_child(rpr, "rStyle")
            .expect("field-code runs must keep the PageNumber character style");
        let sz = find_w_child(rpr, "sz").expect("field-code runs must keep the 11pt explicit size");

        assert_eq!(
            attr_value(rstyle, "w:val"),
            Some("PageNumber"),
            "field-code wrapper runs should preserve PageNumber style"
        );
        assert_eq!(
            attr_value(sz, "w:val"),
            Some("22"),
            "field-code wrapper runs should preserve the 11pt explicit size"
        );
    }
}

#[test]
fn safe_singapore_redline_normalizes_footer_page_number_field_result() {
    let before = fs::read("testdata/safe-us-vs-singapore/before.docx").expect("read before");
    let after = fs::read("testdata/safe-us-vs-singapore/after.docx").expect("read after");
    let root = generate_redline_story_xml(&before, &after, "word/footer1.xml");

    fn find_run_with_text<'a>(el: &'a Element, needle: &str) -> Option<&'a Element> {
        if is_w_tag(el, "r") && run_text(el) == needle {
            return Some(el);
        }
        for child in &el.children {
            if let XMLNode::Element(node) = child
                && let Some(found) = find_run_with_text(node, needle)
            {
                return Some(found);
            }
        }
        None
    }

    fn has_inserted_run_with_text(el: &Element, needle: &str) -> bool {
        if is_w_tag(el, "ins") {
            for child in &el.children {
                if let XMLNode::Element(run) = child
                    && is_w_tag(run, "r")
                    && run_text(run) == needle
                {
                    return true;
                }
            }
        }
        el.children.iter().any(|child| match child {
            XMLNode::Element(node) => has_inserted_run_with_text(node, needle),
            _ => false,
        })
    }

    assert!(
        !has_inserted_run_with_text(&root, "6"),
        "PAGE field cached result should not be emitted as tracked inserted text"
    );

    let result_run = find_run_with_text(&root, "6")
        .expect("redline footer should contain the current page-number result run");
    let rpr =
        find_w_child(result_run, "rPr").expect("page-number result run should keep run properties");
    let rstyle =
        find_w_child(rpr, "rStyle").expect("page-number result run should keep PageNumber style");
    let sz =
        find_w_child(rpr, "sz").expect("page-number result run should keep the 11pt explicit size");

    assert_eq!(attr_value(rstyle, "w:val"), Some("PageNumber"));
    assert_eq!(attr_value(sz, "w:val"), Some("22"));

    let trailing_hyphen_run = root
        .children
        .iter()
        .find_map(|child| match child {
            XMLNode::Element(paragraph) if is_w_tag(paragraph, "p") => {
                paragraph.children.iter().rev().find_map(|node| match node {
                    XMLNode::Element(run) if is_w_tag(run, "r") && run_text(run) == "-" => {
                        Some(run)
                    }
                    _ => None,
                })
            }
            _ => None,
        })
        .expect("redline footer should contain trailing '-' run");
    let trailing_rpr = find_w_child(trailing_hyphen_run, "rPr")
        .expect("trailing hyphen run should keep run properties");
    let trailing_rstyle = find_w_child(trailing_rpr, "rStyle")
        .expect("trailing hyphen run should keep PageNumber style");
    let trailing_sz = find_w_child(trailing_rpr, "sz")
        .expect("trailing hyphen run should keep the 11pt explicit size");

    assert_eq!(attr_value(trailing_rstyle, "w:val"), Some("PageNumber"));
    assert_eq!(attr_value(trailing_sz, "w:val"), Some("22"));
}

// ==========================================================================
// Test: [name] and [title] paragraphs — alignment via firstLine cascade
// ==========================================================================

/// The "[name]" paragraph has direct `w:ind w:left="5040" w:right="-360"` with
/// NO firstLine.  Per §17.3.1.12 per-attribute cascade: absent firstLine
/// inherits 720 from Normal.  First-line text position = 5040 + 720 = 5760.
///
/// The "[title]" paragraph has `w:ind w:left="5220" w:right="-360" w:firstLine="540"`.
/// First-line text position = 5220 + 540 = 5760.
///
/// Both should be vertically aligned at 5760 twips.
#[test]
fn safe_singapore_name_title_aligned_via_first_line_cascade() {
    let (_rt, doc) = import_doc("after");

    let name_para = common::all_paragraphs(&doc)
        .into_iter()
        .find(|p| {
            let t = common::paragraph_text(p);
            t.contains("[name]") && !t.contains("[title]")
        })
        .expect("should find '[name]' paragraph");

    let title_para = common::all_paragraphs(&doc)
        .into_iter()
        .find(|p| common::paragraph_text(p).contains("[title]"))
        .expect("should find '[title]' paragraph");

    let name_indent = name_para
        .indent
        .as_ref()
        .expect("[name] should have indent");
    let title_indent = title_para
        .indent
        .as_ref()
        .expect("[title] should have indent");

    // [name]: left=5040, firstLine should inherit 720 from Normal
    assert_eq!(name_indent.left, Some(5040), "[name] left");
    assert_eq!(
        name_indent.effective_first_line_twips,
        Some(720),
        "[name] firstLine should inherit 720 from Normal (per-attribute cascade)"
    );

    // [title]: left=5220, firstLine=540 (explicit)
    assert_eq!(title_indent.left, Some(5220), "[title] left");
    assert_eq!(
        title_indent.effective_first_line_twips,
        Some(540),
        "[title] firstLine should be 540 (explicit)"
    );

    // Both first-line text positions should be 5760 twips
    let name_pos =
        name_indent.left.unwrap_or(0) + name_indent.effective_first_line_twips.unwrap_or(0);
    let title_pos =
        title_indent.left.unwrap_or(0) + title_indent.effective_first_line_twips.unwrap_or(0);
    assert_eq!(
        name_pos, title_pos,
        "[name] and [title] should align. [name]={name_pos}, [title]={title_pos}"
    );
    assert_eq!(name_pos, 5760, "Both should start at 5760 twips");
}

// ==========================================================================
// Test: [name] firstLine cascade survives the full compare (diff) path
// ==========================================================================

/// The full_document_view (compare) path builds full-doc blocks from
/// before + after. The [name] paragraph's firstLine inheritance must
/// survive through this path — the same cascade applies.
#[test]
fn safe_singapore_name_first_line_cascade_in_compare_path() {
    let before_path = "testdata/safe-us-vs-singapore/before.docx";
    let after_path = "testdata/safe-us-vs-singapore/after.docx";
    let before_bytes = fs::read(before_path).unwrap();
    let after_bytes = fs::read(after_path).unwrap();

    let runtime = SimpleRuntime::new();
    let before_import = runtime.import_docx(&before_bytes).unwrap();
    let after_import = runtime.import_docx(&after_bytes).unwrap();

    let full_view = runtime
        .full_document_view(&before_import.doc_handle, &after_import.doc_handle)
        .expect("full_document_view");

    // Find the full-doc block containing "[name]" (not "[title]")
    let name_block = full_view
        .blocks
        .iter()
        .find(|b| {
            let text: String = b
                .segments
                .iter()
                .map(|s| match s {
                    stemma::InlineChange::Unchanged { text, .. } => text.as_str(),
                    stemma::InlineChange::Inserted { text, .. } => text.as_str(),
                    stemma::InlineChange::Deleted { text, .. } => text.as_str(),
                    stemma::InlineChange::Opaque { .. } => "",
                })
                .collect();
            text.contains("[name]") && !text.contains("[title]")
        })
        .expect("should find '[name]' full-doc block");

    let indent = name_block
        .indent
        .as_ref()
        .expect("[name] block should have indent");

    // The after.docx [name] has left=5040, no firstLine → inherits 720 from Normal.
    assert_eq!(indent.left, Some(5040), "[name] compare-path left");
    assert_eq!(
        indent.effective_first_line_twips,
        Some(720),
        "[name] compare-path: firstLine should inherit 720 from Normal (per-attribute cascade)"
    );

    // NOTE: the JSON-projection assertion (full_doc_block_to_payload preserves
    // effective_first_line_twips) lives with the consuming application — it tests the app-layer
    // json_types projection, which is not part of the stemma engine.
    // See its full_doc_payload_projection tests.
}

/// Same test using the after (Singapore) document.
/// Per-attribute cascade applies identically.
#[test]
fn safe_singapore_after_inherits_first_line_from_style() {
    let (_rt, doc) = import_doc("after");
    let events = find_events_paragraph(&doc);

    let indent = events.indent.as_ref().expect("Events should have indent");
    assert_eq!(indent.left, Some(-720));
    // Per-attribute cascade: absent firstLine inherits 720 from Normal.
    assert_eq!(
        indent.effective_first_line_twips,
        Some(720),
        "Events firstLine should inherit 720 from Normal in after.docx (per-attribute cascade)"
    );

    let postmoney = find_postmoney_paragraph(&doc);
    let pm_indent = postmoney.indent.as_ref().unwrap();
    assert_eq!(
        pm_indent.effective_first_line_twips,
        Some(720),
        "Post-Money firstLine should inherit 720 from Normal in after.docx (per-attribute cascade)"
    );
}

#[test]
fn safe_singapore_redline_preserves_tabbed_literal_prefix_layout() {
    let before = fs::read("testdata/safe-us-vs-singapore/before.docx").expect("read before");
    let after = fs::read("testdata/safe-us-vs-singapore/after.docx").expect("read after");
    let root = generate_redline_document_xml(&before, &after);
    let paragraph = find_dissolution_event_paragraph(&root);

    let ppr = find_w_child(paragraph, "pPr").expect("paragraph should have pPr");
    let tabs = find_w_child(ppr, "tabs").expect("current pPr should retain prefix tab stop");
    let current_tab = find_w_child(tabs, "tab").expect("current pPr should have a tab stop");
    assert_eq!(
        attr_value(current_tab, "w:pos"),
        Some("360"),
        "literal-prefix paragraph should serialize the consumed 360-twip tab stop"
    );

    let tab_runs = paragraph
        .children
        .iter()
        .filter_map(|child| match child {
            XMLNode::Element(el) if is_w_tag(el, "r") => Some(el),
            _ => None,
        })
        .flat_map(|run| run.children.iter())
        .filter(|child| matches!(child, XMLNode::Element(el) if is_w_tag(el, "tab")))
        .count();
    assert_eq!(
        tab_runs, 2,
        "literal-prefix paragraph should emit leading and trailing tab runs around '(c)'"
    );

    let ppr_change = find_w_child(ppr, "pPrChange").expect("paragraph should have pPrChange");
    let previous_ppr =
        find_w_child(ppr_change, "pPr").expect("pPrChange should contain previous pPr");
    let previous_tabs =
        find_w_child(previous_ppr, "tabs").expect("previous pPr should retain prefix tab stop");
    let previous_tab =
        find_w_child(previous_tabs, "tab").expect("previous pPr should have a tab stop");
    assert_eq!(
        attr_value(previous_tab, "w:pos"),
        Some("360"),
        "pPrChange should serialize the previous leading tab stop so reject view matches Word"
    );
}

#[test]
fn safe_singapore_import_preserves_explicit_first_line_zero_on_tabbed_clause() {
    let (_rt, doc) = import_doc("after");
    let paragraph = find_paragraph_containing(&doc, "Dissolution Event before the termination");

    assert_eq!(paragraph.literal_prefix.as_deref(), Some("(c)"));
    assert_eq!(paragraph.literal_prefix_leading_tab_count, 1);

    let indent = paragraph
        .indent
        .as_ref()
        .expect("Dissolution Event clause should keep indentation");
    assert_eq!(indent.left, Some(-720));
    assert_eq!(
        indent.effective_first_line_twips,
        Some(0),
        "import should preserve explicit firstLine=0 on the tabbed clause paragraph"
    );
}

#[test]
fn safe_singapore_redline_preserves_explicit_first_line_zero_on_tabbed_clause() {
    let before = fs::read("testdata/safe-us-vs-singapore/before.docx").expect("read before");
    let after = fs::read("testdata/safe-us-vs-singapore/after.docx").expect("read after");
    let root = generate_redline_document_xml(&before, &after);
    let paragraph = find_dissolution_event_paragraph(&root);

    let ppr = find_w_child(paragraph, "pPr").expect("paragraph should have pPr");
    let ind = find_w_child(ppr, "ind").expect("paragraph should keep direct indentation");
    assert_eq!(attr_value(ind, "w:left"), Some("-720"));
    assert_eq!(
        attr_value(ind, "w:firstLine"),
        Some("0"),
        "redline should serialize explicit firstLine=0 so Word accept preserves clause geometry"
    );
}

#[test]
fn safe_singapore_redline_preserves_direct_auto_color_runs() {
    let before = fs::read("testdata/safe-us-vs-singapore/before.docx").expect("read before");
    let after = fs::read("testdata/safe-us-vs-singapore/after.docx").expect("read after");
    let root = generate_redline_document_xml(&before, &after);

    let liquidity_paragraph = find_paragraph_containing_xml(&root, "the Liquidity Capitalization");
    let liquidity_run = liquidity_paragraph
        .children
        .iter()
        .filter_map(|child| match child {
            XMLNode::Element(el) if is_w_tag(el, "r") => Some(el),
            _ => None,
        })
        .find(|run| run_text(run) == "the Liquidity Capitalization")
        .expect("redline should keep the Liquidity Capitalization run");
    assert_eq!(
        run_color_value(liquidity_run),
        Some("auto"),
        "redline should preserve direct auto color on unchanged body runs",
    );

    let provision_paragraph =
        find_paragraph_containing_xml(&root, "In the event any one or more of the provisions");
    let prefix_run = provision_paragraph
        .children
        .iter()
        .filter_map(|child| match child {
            XMLNode::Element(el) if is_w_tag(el, "r") => Some(el),
            _ => None,
        })
        .find(|run| run_text(run) == "(e)")
        .expect("redline should keep the literal prefix run for '(e)'");
    assert_eq!(
        run_color_value(prefix_run),
        Some("auto"),
        "redline should preserve direct auto color on literal prefix runs",
    );
}
