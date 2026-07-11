use std::fs;
use std::io::{Cursor, Read};

use stemma::{
    BlockNode, DiffChange, DocxRuntime, ExportMode, InlineNode, OpaqueKind, SimpleRuntime,
    TransactionMeta, diff_documents,
};
use xmltree::{Element, XMLNode};
use zip::ZipArchive;

use crate::common;

const WORD_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn import_doc(name: &str) -> (SimpleRuntime, stemma::CanonDoc) {
    let path = format!("testdata/image-math-combined/{name}.docx");
    let bytes = fs::read(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    let runtime = SimpleRuntime::new();
    let import = runtime
        .import_docx(&bytes)
        .unwrap_or_else(|e| panic!("import {path}: {e:?}"));
    let view = runtime.view(&import.doc_handle).expect("view");
    (runtime, std::sync::Arc::unwrap_or_clone(view.canonical))
}

fn generate_redline_document_xml() -> Element {
    let before_path = "testdata/image-math-combined/before.docx";
    let after_path = "testdata/image-math-combined/after.docx";
    let before = fs::read(before_path).unwrap_or_else(|e| panic!("read {before_path}: {e}"));
    let after = fs::read(after_path).unwrap_or_else(|e| panic!("read {after_path}: {e}"));
    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before).expect("import before");
    let import_after = runtime.import_docx(&after).expect("import after");
    runtime
        .diff_and_redline(
            &import_before.doc_handle,
            &import_after.doc_handle,
            TransactionMeta {
                author: "Stemma".to_string(),
                reason: Some("image math fidelity regression".to_string()),
                timestamp_utc: Some("2026-03-26T00:00:00Z".to_string()),
            },
        )
        .expect("diff_and_redline");
    let redline = runtime
        .export_docx(&import_before.doc_handle, ExportMode::Redline)
        .expect("export redline");
    let mut zip = ZipArchive::new(Cursor::new(redline)).expect("open redline zip");
    let mut file = zip.by_name("word/document.xml").expect("word/document.xml");
    let mut xml = String::new();
    file.read_to_string(&mut xml).expect("read document.xml");
    Element::parse(Cursor::new(xml.as_bytes())).expect("parse document.xml")
}

fn has_omml_block(paragraph: &stemma::ParagraphNode) -> bool {
    paragraph
        .all_inlines_owned()
        .iter()
        .any(|inline| matches!(inline, InlineNode::OpaqueInline(opaque) if matches!(opaque.kind, OpaqueKind::OmmlBlock)))
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

fn has_direct_omath_para(paragraph: &Element) -> bool {
    paragraph.children.iter().any(|child| match child {
        XMLNode::Element(el) => {
            el.name
                .rsplit_once(':')
                .map(|(_, local)| local)
                .unwrap_or(&el.name)
                == "oMathPara"
        }
        _ => false,
    })
}

fn has_w_track_descendant(element: &Element, local: &str) -> bool {
    if is_w_tag(element, local) {
        return true;
    }
    element.children.iter().any(|child| match child {
        XMLNode::Element(el) => has_w_track_descendant(el, local),
        _ => false,
    })
}

#[test]
fn image_math_after_import_preserves_empty_replacement_paragraph_mark_theme_font() {
    let (_runtime, after) = import_doc("after");
    let paragraphs = common::all_paragraphs(&after);
    let paragraph = paragraphs
        .get(8)
        .copied()
        .expect("fixture should have paragraph 8");

    assert_eq!(common::paragraph_text(paragraph), "");
    assert_eq!(
        paragraph
            .paragraph_mark_style_props
            .font_east_asia_theme
            .as_deref(),
        Some("minorEastAsia")
    );
}

#[test]
fn image_math_diff_keeps_themed_math_replacement_on_modified_paragraph_path() {
    let (_runtime_before, before) = import_doc("before");
    let (_runtime_after, after) = import_doc("after");
    let diff = diff_documents(&before, &after).expect("diff_documents");

    let modified = diff
        .changes
        .iter()
        .find_map(|change| match change {
            DiffChange::BlockModified {
                old_block: BlockNode::Paragraph(old_p),
                new_block: BlockNode::Paragraph(new_p),
                ..
            } if has_omml_block(old_p)
                && old_p
                    .paragraph_mark_style_props
                    .font_east_asia_theme
                    .as_deref()
                    == Some("minorEastAsia")
                && common::paragraph_text(new_p).is_empty() =>
            {
                Some((old_p, new_p))
            }
            _ => None,
        })
        .expect(
            "diff should keep the themed math-to-empty replacement on the modified paragraph path",
        );

    assert!(
        has_omml_block(modified.0),
        "modified paragraph should still carry the base math block"
    );
    assert_eq!(
        modified
            .1
            .paragraph_mark_style_props
            .font_east_asia_theme
            .as_deref(),
        Some("minorEastAsia"),
        "modified paragraph must adopt the target paragraph-mark theme font"
    );
}

#[test]
fn image_math_redline_tracks_themed_math_deletion_inside_omml() {
    let root = generate_redline_document_xml();
    let paragraphs = body_paragraphs(&root);
    let themed_math = paragraphs
        .iter()
        .find(|paragraph| {
            if !has_direct_omath_para(paragraph) {
                return false;
            }
            let Some(ppr) = find_w_child(paragraph, "pPr") else {
                return false;
            };
            let Some(rpr) = find_w_child(ppr, "rPr") else {
                return false;
            };
            let Some(rfonts) = find_w_child(rpr, "rFonts") else {
                return false;
            };
            attr_value(rfonts, "w:eastAsiaTheme") == Some("minorEastAsia")
        })
        .copied()
        .expect("redline should contain themed math paragraph");
    let ppr = find_w_child(themed_math, "pPr").expect("themed math paragraph should have pPr");
    let rpr =
        find_w_child(ppr, "rPr").expect("themed math paragraph should have paragraph-mark rPr");
    let rfonts = find_w_child(rpr, "rFonts")
        .expect("themed math paragraph should preserve paragraph-mark rFonts");

    assert_eq!(
        attr_value(rfonts, "w:eastAsiaTheme"),
        Some("minorEastAsia"),
        "themed math paragraph should preserve the target paragraph-mark theme font"
    );
    assert!(
        !has_w_track_descendant(rpr, "del"),
        "themed math paragraph should not delete the paragraph mark itself"
    );
    assert!(
        has_w_track_descendant(themed_math, "del"),
        "themed math paragraph should track the math deletion inside the OMML tree"
    );
}
