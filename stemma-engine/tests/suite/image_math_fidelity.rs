use std::fs;
#[allow(unused_imports)]
use stemma_diff::test_support::{DocumentComparisonExt as _, RuntimeComparisonExt as _};

use stemma::{BlockNode, DocxRuntime, InlineNode, OpaqueKind, SimpleRuntime, TransactionMeta};
use stemma_diff::test_support::{DiffChange, diff_documents};

use crate::common;

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

fn has_omml_block(paragraph: &stemma::ParagraphNode) -> bool {
    paragraph
        .all_inlines_owned()
        .iter()
        .any(|inline| matches!(inline, InlineNode::OpaqueInline(opaque) if matches!(opaque.kind, OpaqueKind::OmmlBlock)))
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
fn image_math_redline_refuses_unqualified_whole_object_deletion() {
    let before_path = "testdata/image-math-combined/before.docx";
    let after_path = "testdata/image-math-combined/after.docx";
    let before = fs::read(before_path).expect("read before");
    let after = fs::read(after_path).expect("read after");
    let runtime = SimpleRuntime::new();
    let imported_before = runtime.import_docx(&before).expect("import before");
    let imported_after = runtime.import_docx(&after).expect("import after");
    let err = runtime
        .diff_and_redline(
            &imported_before.doc_handle,
            &imported_after.doc_handle,
            TransactionMeta {
                author: "Stemma".to_string(),
                reason: Some("image math fidelity regression".to_string()),
                timestamp_utc: Some("2026-03-26T00:00:00Z".to_string()),
            },
        )
        .expect_err("unqualified block-math carrier must refuse");
    assert_eq!(
        err.message,
        "side-only block equations have no qualified native Word carrier"
    );
}
