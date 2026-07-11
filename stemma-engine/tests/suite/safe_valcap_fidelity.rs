use std::fs;
use std::io::{Cursor, Read};

use stemma::{
    CanonDoc, DocxRuntime, ExportMode, Mark, RevisionInfo, SimpleRuntime, TransactionMeta,
    diff_documents, merge_diff,
};
use xmltree::{Element, XMLNode};
use zip::ZipArchive;

use crate::common;

const WORD_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn import_doc(fixture: &str, name: &str) -> (SimpleRuntime, CanonDoc) {
    let path = format!("testdata/{fixture}/{name}.docx");
    let bytes = fs::read(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    let runtime = SimpleRuntime::new();
    let import = runtime
        .import_docx(&bytes)
        .unwrap_or_else(|e| panic!("import {path}: {e:?}"));
    let view = runtime.view(&import.doc_handle).expect("view");
    (runtime, std::sync::Arc::unwrap_or_clone(view.canonical))
}

fn generate_redline_docx(fixture: &str) -> Vec<u8> {
    let before_path = format!("testdata/{fixture}/before.docx");
    let after_path = format!("testdata/{fixture}/after.docx");
    let before = fs::read(&before_path).unwrap_or_else(|e| panic!("read {before_path}: {e}"));
    let after = fs::read(&after_path).unwrap_or_else(|e| panic!("read {after_path}: {e}"));
    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before).expect("import before");
    let import_after = runtime.import_docx(&after).expect("import after");
    runtime
        .diff_and_redline(
            &import_before.doc_handle,
            &import_after.doc_handle,
            TransactionMeta {
                author: "Stemma".to_string(),
                reason: Some("SAFE valcap fidelity regression".to_string()),
                timestamp_utc: Some("2026-03-26T00:00:00Z".to_string()),
            },
        )
        .expect("diff_and_redline");
    runtime
        .export_docx(&import_before.doc_handle, ExportMode::Redline)
        .expect("export redline")
}

fn merge_redline_canonical(fixture: &str) -> CanonDoc {
    let (_runtime_before, before) = import_doc(fixture, "before");
    let (_runtime_after, after) = import_doc(fixture, "after");
    let diff = diff_documents(&before, &after).expect("diff_documents");
    merge_diff(
        &before,
        &after,
        &diff,
        &RevisionInfo {
            revision_id: 1,
            author: Some("Stemma".to_string()),
            date: Some("2026-03-26T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    )
    .expect("merge_diff")
    .doc
}

fn diff_fixture(fixture: &str) -> stemma::DocumentDiff {
    let (_runtime_before, before) = import_doc(fixture, "before");
    let (_runtime_after, after) = import_doc(fixture, "after");
    diff_documents(&before, &after).expect("diff_documents")
}

fn element_to_string(el: &Element) -> String {
    let mut buf = Vec::new();
    el.write(&mut buf).expect("serialize element");
    String::from_utf8(buf).expect("utf-8")
}

fn generate_redline_document_xml(fixture: &str) -> Element {
    let redline = generate_redline_docx(fixture);
    let mut zip = ZipArchive::new(Cursor::new(redline)).expect("open redline zip");
    let mut file = zip.by_name("word/document.xml").expect("word/document.xml");
    let mut xml = String::new();
    file.read_to_string(&mut xml).expect("read document.xml");
    Element::parse(Cursor::new(xml.as_bytes())).expect("parse document.xml")
}

fn generate_redline_story_xml(fixture: &str, part_name: &str) -> Element {
    let redline = generate_redline_docx(fixture);
    let mut zip = ZipArchive::new(Cursor::new(redline)).expect("open redline zip");
    let mut file = zip
        .by_name(part_name)
        .unwrap_or_else(|e| panic!("{part_name}: {e}"));
    let mut xml = String::new();
    file.read_to_string(&mut xml)
        .unwrap_or_else(|e| panic!("read {part_name}: {e}"));
    Element::parse(Cursor::new(xml.as_bytes())).unwrap_or_else(|e| panic!("parse {part_name}: {e}"))
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

fn find_paragraph_containing<'a>(doc: &'a CanonDoc, needle: &str) -> &'a stemma::ParagraphNode {
    common::all_paragraphs(doc)
        .into_iter()
        .find(|p| common::paragraph_text(p).contains(needle))
        .unwrap_or_else(|| panic!("should find paragraph containing {needle:?}"))
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
                if common::paragraph_text(p).contains("-5-")
                    || common::paragraph_text(p).contains("-2-") =>
            {
                Some(p)
            }
            _ => None,
        })
        .expect("should find page-number footer paragraph")
}

fn find_header_paragraph_containing<'a>(
    doc: &'a CanonDoc,
    part_name: &str,
    needle: &str,
) -> &'a stemma::ParagraphNode {
    let header = doc
        .headers
        .iter()
        .find(|header| header.part_name == part_name)
        .unwrap_or_else(|| panic!("should find header story {part_name}"));
    header
        .blocks
        .iter()
        .find_map(|tracked| match &tracked.block {
            stemma::BlockNode::Paragraph(p) if common::paragraph_text(p).contains(needle) => {
                Some(p)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("should find header paragraph containing {needle:?}"))
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

fn body_paragraphs(root: &Element) -> Vec<&Element> {
    fn visit<'a>(el: &'a Element, out: &mut Vec<&'a Element>) {
        if is_w_tag(el, "p") {
            out.push(el);
        }
        for child in &el.children {
            if let XMLNode::Element(node) = child {
                visit(node, out);
            }
        }
    }

    let body = root
        .children
        .iter()
        .find_map(|child| match child {
            XMLNode::Element(el) if is_w_tag(el, "body") => Some(el),
            _ => None,
        })
        .expect("document body");
    let mut out = Vec::new();
    visit(body, &mut out);
    out
}

fn collect_paragraph_text(paragraph: &Element) -> String {
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

    let mut out = String::new();
    collect_text(paragraph, &mut out);
    out
}

fn paragraph_tab_positions(paragraph: &Element) -> Vec<String> {
    find_w_child(paragraph, "pPr")
        .and_then(|ppr| find_w_child(ppr, "tabs"))
        .map(|tabs| {
            tabs.children
                .iter()
                .filter_map(|child| match child {
                    XMLNode::Element(el) if is_w_tag(el, "tab") => {
                        attr_value(el, "w:pos").map(ToOwned::to_owned)
                    }
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn paragraph_run_sequences(paragraph: &Element) -> Vec<(String, bool)> {
    fn run_sequence(run: &Element) -> Option<(String, bool)> {
        if !is_w_tag(run, "r") {
            return None;
        }
        let italic = find_w_child(run, "rPr")
            .and_then(|rpr| find_w_child(rpr, "i"))
            .is_some();
        let mut out = String::new();
        for child in &run.children {
            match child {
                XMLNode::Element(el) if is_w_tag(el, "tab") => out.push_str("<TAB>"),
                XMLNode::Element(el) if is_w_tag(el, "t") || is_w_tag(el, "delText") => {
                    if let Some(text) = el.get_text() {
                        out.push_str(&text);
                    }
                }
                _ => {}
            }
        }
        (!out.is_empty()).then_some((out, italic))
    }

    fn visit_container(element: &Element, out: &mut Vec<(String, bool)>) {
        for child in &element.children {
            match child {
                XMLNode::Element(el) if is_w_tag(el, "r") => {
                    if let Some(seq) = run_sequence(el) {
                        out.push(seq);
                    }
                }
                XMLNode::Element(el)
                    if is_w_tag(el, "ins")
                        || is_w_tag(el, "del")
                        || el.name.ends_with(":moveFrom")
                        || el.name.ends_with(":moveTo") =>
                {
                    visit_container(el, out);
                }
                _ => {}
            }
        }
    }

    let mut out = Vec::new();
    visit_container(paragraph, &mut out);
    out
}

fn paragraph_has_deleted_para_mark(paragraph: &Element) -> bool {
    find_w_child(paragraph, "pPr")
        .and_then(|ppr| find_w_child(ppr, "rPr"))
        .and_then(|rpr| find_w_child(rpr, "del"))
        .is_some()
}

fn find_first_field_run<'a>(root: &'a Element, tag: &str) -> &'a Element {
    fn visit<'a>(el: &'a Element, tag: &str) -> Option<&'a Element> {
        if is_w_tag(el, "r")
            && el.children.iter().any(|child| match child {
                XMLNode::Element(node) => is_w_tag(node, tag),
                _ => false,
            })
        {
            return Some(el);
        }
        for child in &el.children {
            if let XMLNode::Element(node) = child
                && let Some(found) = visit(node, tag)
            {
                return Some(found);
            }
        }
        None
    }

    visit(root, tag).unwrap_or_else(|| panic!("should find run containing {tag}"))
}

fn find_first_text_run_containing<'a>(root: &'a Element, needle: &str) -> &'a Element {
    fn run_text(run: &Element) -> String {
        let mut out = String::new();
        for child in &run.children {
            if let XMLNode::Element(el) = child
                && (is_w_tag(el, "t") || is_w_tag(el, "delText"))
                && let Some(text) = el.get_text()
            {
                out.push_str(&text);
            }
        }
        out
    }

    fn visit<'a>(el: &'a Element, needle: &str) -> Option<&'a Element> {
        if is_w_tag(el, "r") && run_text(el).contains(needle) {
            return Some(el);
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

    visit(root, needle).unwrap_or_else(|| panic!("should find run containing text {needle:?}"))
}

fn find_prefix_segment_text_nodes(
    paragraph: &stemma::ParagraphNode,
) -> Vec<(&stemma::TextNode, &stemma::TrackingStatus)> {
    paragraph
        .segments
        .iter()
        .flat_map(|segment| {
            segment
                .inlines
                .iter()
                .filter_map(move |inline| match inline {
                    stemma::InlineNode::Text(text)
                        if text.id.0.contains("_pfx_") || text.id.0.contains("_npfx_") =>
                    {
                        Some((text.as_ref(), &segment.status))
                    }
                    _ => None,
                })
        })
        .collect()
}

#[test]
fn safe_valcap_discount_import_preserves_tabbed_literal_prefix_geometry() {
    let (_runtime, doc) = import_doc("safe-valcap-vs-discount", "after");
    let paragraph = find_paragraph_containing(&doc, "Senior to payments for Common Stock.");

    assert_eq!(paragraph.literal_prefix.as_deref(), Some("(iii)"));
    assert_eq!(paragraph.literal_prefix_leading_tab_twips, Some(1080));
    assert_eq!(paragraph.literal_prefix_leading_tab_count, 2);
    assert!(paragraph.literal_prefix_has_trailing_tab);
    assert_eq!(paragraph.literal_prefix_trailing_tab_stop_twips, Some(1080));
    assert_eq!(
        paragraph
            .indent
            .as_ref()
            .and_then(|indent| indent.effective_first_line_twips),
        Some(720),
        "leading-tab literal prefixes should preserve the resolved first-line indent alongside tab geometry",
    );
}

#[test]
fn safe_valcap_clause_prefix_uses_visible_text_formatting_not_leading_tab_font() {
    for fixture in ["safe-valcap-vs-discount", "safe-valcap-vs-mfn"] {
        let (_runtime, doc) = import_doc(fixture, "after");
        let paragraph = find_paragraph_containing(&doc, "Dissolution Event before");

        assert_eq!(paragraph.literal_prefix.as_deref(), Some("(c)"));
        assert_eq!(
            paragraph.literal_prefix_style_props.font_family.as_deref(),
            Some("Times New Roman"),
            "visible clause prefix should follow the visible prefix text, not the empty leading-tab run",
        );
        assert_eq!(paragraph.literal_prefix_style_props.font_size, Some(22));
    }
}

#[test]
fn safe_valcap_discount_redline_preserves_tabbed_literal_prefix_layout() {
    let document_xml = generate_redline_document_xml("safe-valcap-vs-discount");
    let paragraph =
        find_paragraph_containing_xml(&document_xml, "Senior to payments for Common Stock.");
    let tab_positions = paragraph_tab_positions(paragraph);
    let run_sequences = paragraph_run_sequences(paragraph);
    let tab_count: usize = run_sequences
        .iter()
        .map(|(sequence, _)| sequence.matches("<TAB>").count())
        .sum();

    assert_eq!(tab_positions, vec!["360".to_string()]);
    assert_eq!(
        tab_count, 3,
        "redline should keep the three-tab prefix/body layout"
    );
}

#[test]
fn safe_valcap_clause_redline_prefix_run_does_not_serialize_leading_tab_font_family() {
    for fixture in ["safe-valcap-vs-discount", "safe-valcap-vs-mfn"] {
        let document_xml = generate_redline_document_xml(fixture);
        let paragraph = find_paragraph_containing_xml(&document_xml, "Dissolution Event before");
        let prefix_run = find_first_text_run_containing(paragraph, "(c)");
        let rpr = find_w_child(prefix_run, "rPr").expect("prefix run rPr");
        let font_family =
            find_w_child(rpr, "rFonts").and_then(|fonts| attr_value(fonts, "w:ascii"));

        assert!(
            font_family != Some("Arial"),
            "visible clause prefix run should not carry Arial from the empty leading-tab run",
        );
        // The leading tab now re-emits as its OWN run wearing its authored
        // formatting (literal_prefix_leading_rpr) — source-faithful, instead
        // of being folded into the label run. Verify the tab precedes the
        // label within the paragraph and carries the authored Arial.
        let para_xml = element_to_string(paragraph);
        let label_pos = para_xml.find("(c)").expect("label present");
        let lead = &para_xml[..label_pos];
        assert!(
            lead.contains("<w:tab"),
            "the consumed leading tab must still emit, before the label; paragraph: {para_xml}"
        );
        assert!(
            lead.contains("Arial"),
            "the leading tab run keeps its authored Arial rFonts \
             (literal_prefix_leading_rpr); paragraph: {para_xml}"
        );
    }
}

#[test]
fn safe_valcap_discount_diff_inserts_empty_separator_before_heading() {
    let diff = diff_fixture("safe-valcap-vs-discount");
    let heading_idx = diff
        .changes
        .iter()
        .position(|change| match change {
            stemma::DiffChange::BlockInserted {
                block: stemma::BlockNode::Paragraph(p),
                ..
            } => common::paragraph_text(p).contains("Company Representations"),
            stemma::DiffChange::BlockModified { new_text, .. } => {
                new_text.contains("Company Representations")
            }
            _ => false,
        })
        .expect("inserted heading paragraph");

    let separator = &diff.changes[heading_idx - 1];
    match separator {
        stemma::DiffChange::BlockInserted {
            block: stemma::BlockNode::Paragraph(p),
            ..
        } => {
            assert_eq!(common::paragraph_text(p), "");
            let spacing = p.spacing.as_ref().expect("separator spacing");
            let indent = p.indent.as_ref().expect("separator indent");
            assert_eq!(spacing.before, Some(0));
            assert_eq!(indent.left, Some(-720));
            assert_eq!(indent.right, Some(-360));
        }
        other => panic!("expected inserted empty separator before heading, got {other:?}"),
    }
}

#[test]
fn safe_valcap_discount_redline_preserves_empty_separator_paragraph_mark_formatting() {
    let document_xml = generate_redline_document_xml("safe-valcap-vs-discount");
    let body = body_paragraphs(&document_xml);
    let heading_idx = body
        .iter()
        .position(|p| collect_paragraph_text(p).contains("Company Representations"))
        .expect("heading paragraph");
    let separator = body[heading_idx - 1];
    let ppr = find_w_child(separator, "pPr").expect("separator pPr");
    let rpr = find_w_child(ppr, "rPr").expect("separator pPr/rPr");
    let sz = find_w_child(rpr, "sz").expect("separator paragraph-mark font size");
    let ind = find_w_child(ppr, "ind").expect("separator indent");

    assert_eq!(collect_paragraph_text(separator), "");
    assert!(
        find_w_child(ppr, "tabs").is_none(),
        "separator should not inherit clause tab stops"
    );
    assert!(
        find_w_child(ppr, "pPrChange").is_none(),
        "separator should stay inserted, not collapsed into a paragraph formatting change"
    );
    assert_eq!(attr_value(sz, "w:val"), Some("22"));
    assert_eq!(attr_value(ind, "w:left"), Some("-720"));
    assert_eq!(attr_value(ind, "w:right"), Some("-360"));
}

#[test]
fn safe_valcap_mfn_import_preserves_heading_prefix_formatting() {
    let (_runtime, doc) = import_doc("safe-valcap-vs-mfn", "after");
    let paragraph = find_paragraph_containing(&doc, "Company Representations");

    assert_eq!(paragraph.literal_prefix.as_deref(), Some("4."));
    assert!(paragraph.literal_prefix_has_trailing_tab);
    assert!(
        paragraph.literal_prefix_marks.contains(&Mark::Bold),
        "heading prefix should preserve bold formatting",
    );
    assert!(
        !paragraph.literal_prefix_marks.contains(&Mark::Italic),
        "heading prefix should not inherit italic from the body run",
    );
}

#[test]
fn safe_valcap_mfn_redline_preserves_heading_prefix_tab_and_non_italic_formatting() {
    let document_xml = generate_redline_document_xml("safe-valcap-vs-mfn");
    let paragraph = find_paragraph_containing_xml(&document_xml, "Company Representations");
    let run_sequences = paragraph_run_sequences(paragraph);

    assert!(
        run_sequences
            .iter()
            .any(|(sequence, italic)| sequence.contains("3.<TAB>") && !italic),
        "deleted heading prefix should stay tab-separated and non-italic: {run_sequences:?}",
    );
    assert!(
        run_sequences
            .iter()
            .any(|(sequence, italic)| sequence.contains("4.<TAB>") && !italic),
        "inserted heading prefix should stay tab-separated and non-italic: {run_sequences:?}",
    );
}

#[test]
fn safe_valcap_mfn_diff_model_preserves_heading_prefix_non_italic_marks() {
    let doc = merge_redline_canonical("safe-valcap-vs-mfn");
    let paragraph = find_paragraph_containing(&doc, "Company Representations");
    let prefix_nodes = find_prefix_segment_text_nodes(paragraph);

    assert!(
        prefix_nodes.iter().any(|(text, status)| {
            matches!(status, stemma::TrackingStatus::Deleted(_))
                && text.text == "3.\t"
                && text.marks.contains(&Mark::Bold)
                && !text.marks.contains(&Mark::Italic)
        }),
        "deleted prefix should stay bold non-italic in merged model: {:?}",
        prefix_nodes
            .iter()
            .map(|(text, status)| (&text.id.0, &text.text, &text.marks, status))
            .collect::<Vec<_>>(),
    );
    assert!(
        prefix_nodes.iter().any(|(text, status)| {
            matches!(status, stemma::TrackingStatus::Inserted(_))
                && text.text == "4.\t"
                && text.marks.contains(&Mark::Bold)
                && !text.marks.contains(&Mark::Italic)
        }),
        "inserted prefix should stay bold non-italic in merged model: {:?}",
        prefix_nodes
            .iter()
            .map(|(text, status)| (&text.id.0, &text.text, &text.marks, status))
            .collect::<Vec<_>>(),
    );
}

#[test]
fn safe_valcap_mfn_diff_target_preserves_heading_literal_prefix_marks() {
    let (_runtime_before, before) = import_doc("safe-valcap-vs-mfn", "before");
    let (_runtime_after, after) = import_doc("safe-valcap-vs-mfn", "after");
    let diff = diff_documents(&before, &after).expect("diff_documents");

    let new_para = diff
        .changes
        .iter()
        .find_map(|change| match change {
            stemma::DiffChange::BlockModified {
                new_block: stemma::BlockNode::Paragraph(p),
                new_text,
                ..
            } if new_text.contains("Company Representations") => Some(p),
            _ => None,
        })
        .expect("modified heading paragraph");

    assert_eq!(new_para.literal_prefix.as_deref(), Some("4."));
    assert!(
        new_para.literal_prefix_marks.contains(&Mark::Bold),
        "diff target paragraph should preserve bold prefix marks",
    );
    assert!(
        !new_para.literal_prefix_marks.contains(&Mark::Italic),
        "diff target paragraph should preserve non-italic prefix marks",
    );
}

#[test]
fn safe_valcap_mfn_redline_preserves_footer_field_wrapper_as_style_only() {
    let footer_xml = generate_redline_story_xml("safe-valcap-vs-mfn", "word/footer1.xml");
    let begin_run = find_first_field_run(&footer_xml, "fldChar");
    let rpr = find_w_child(begin_run, "rPr").expect("field begin run should have rPr");

    let style = find_w_child(rpr, "rStyle").expect("field begin run should keep PageNumber style");
    assert_eq!(attr_value(style, "w:val"), Some("PageNumber"));
    assert!(
        find_w_child(rpr, "sz").is_none(),
        "field wrapper run should preserve direct style-only formatting, not resolved sz",
    );
    assert!(
        find_w_child(rpr, "rFonts").is_none(),
        "field wrapper run should preserve direct style-only formatting, not resolved fonts",
    );
}

#[test]
fn safe_valcap_mfn_import_preserves_footer_field_wrapper_as_style_only() {
    let (_runtime, doc) = import_doc("safe-valcap-vs-mfn", "after");
    let paragraph = find_primary_footer_page_number_paragraph(&doc);
    let begin_field = paragraph
        .all_inlines()
        .find_map(|inline| match inline {
            stemma::InlineNode::OpaqueInline(opaque)
                if matches!(
                    opaque.kind,
                    stemma::OpaqueKind::Field(stemma::FieldData {
                        field_kind: stemma::FieldKind::Begin,
                        ..
                    })
                ) =>
            {
                Some(opaque)
            }
            _ => None,
        })
        .expect("should find footer PAGE field begin");

    assert_eq!(
        begin_field.wrapper_style_props.char_style_id.as_deref(),
        Some("PageNumber"),
    );
    assert!(
        begin_field.wrapper_style_props.font_size.is_none(),
        "field wrapper should keep direct rStyle only, not resolved font size",
    );
    assert!(
        begin_field.wrapper_style_props.font_family.is_none(),
        "field wrapper should keep direct rStyle only, not resolved fonts",
    );
}

#[test]
fn safe_valcap_headers_do_not_mark_surviving_only_paragraph_deleted_for_empty_tail() {
    for fixture in ["safe-valcap-vs-discount", "safe-valcap-vs-mfn"] {
        let doc = merge_redline_canonical(fixture);
        let expected = if fixture.ends_with("discount") {
            "DISCOUNT ONLY"
        } else {
            "MFN ONLY"
        };
        let paragraph = find_header_paragraph_containing(&doc, "header1.xml", expected);

        assert_eq!(
            paragraph.para_mark_status, None,
            "surviving header paragraph should not get deleted para mark just because the story ends with a deleted empty paragraph",
        );
    }
}

#[test]
fn safe_valcap_discount_redline_header_only_paragraph_keeps_its_own_paragraph_mark() {
    let header_xml = generate_redline_story_xml("safe-valcap-vs-discount", "word/header1.xml");
    let paragraph = find_paragraph_containing_xml(&header_xml, "DISCOUNT ONLY");
    let ppr = find_w_child(paragraph, "pPr").expect("header paragraph pPr");
    let ind = find_w_child(ppr, "ind").expect("header indent");
    let jc = find_w_child(ppr, "jc").expect("header alignment");

    assert!(
        !paragraph_has_deleted_para_mark(paragraph),
        "surviving ONLY paragraph should not serialize a deleted paragraph mark",
    );
    assert_eq!(attr_value(ind, "w:left"), Some("-720"));
    assert_eq!(attr_value(ind, "w:right"), Some("-360"));
    assert_eq!(attr_value(ind, "w:firstLine"), Some("0"));
    assert_eq!(attr_value(jc, "w:val"), Some("center"));
}
