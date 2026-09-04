use std::fs;
#[allow(unused_imports)]
use stemma_diff::test_support::{DocumentComparisonExt as _, RuntimeComparisonExt as _};

use stemma::{CanonDoc, DocxRuntime, Mark, RevisionInfo, SimpleRuntime, TransactionMeta};
use stemma_diff::test_support::{DiffChange, DocumentDiff, diff_documents, merge_diff};

use crate::common;

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

#[test]
fn safe_valcap_comparisons_refuse_unswitchable_document_defaults() {
    for fixture in ["safe-valcap-vs-discount", "safe-valcap-vs-mfn"] {
        let before_path = format!("testdata/{fixture}/before.docx");
        let after_path = format!("testdata/{fixture}/after.docx");
        let before = fs::read(&before_path).unwrap_or_else(|e| panic!("read {before_path}: {e}"));
        let after = fs::read(&after_path).unwrap_or_else(|e| panic!("read {after_path}: {e}"));
        let runtime = SimpleRuntime::new();
        let base = runtime.import_docx(&before).expect("import before");
        let target = runtime.import_docx(&after).expect("import after");

        let error = runtime
            .diff_and_redline(
                &base.doc_handle,
                &target.doc_handle,
                TransactionMeta {
                    author: "Stemma".to_string(),
                    reason: Some("global style refusal regression".to_string()),
                    timestamp_utc: Some("2026-03-26T00:00:00Z".to_string()),
                },
            )
            .expect_err("unswitchable global style state must refuse");

        assert_eq!(error.code, stemma::ErrorCode::UnsupportedEdit);
        assert!(
            error.message.contains("document defaults") || error.message.contains("active style"),
            "{fixture}: refusal should identify global style state: {error:?}"
        );
    }
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
            identity: 0,
            author: Some("Stemma".to_string()),
            date: Some("2026-03-26T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    )
    .expect("merge_diff")
    .doc
}

fn diff_fixture(fixture: &str) -> DocumentDiff {
    let (_runtime_before, before) = import_doc(fixture, "before");
    let (_runtime_after, after) = import_doc(fixture, "after");
    diff_documents(&before, &after).expect("diff_documents")
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
fn safe_valcap_discount_diff_inserts_empty_separator_before_heading() {
    let diff = diff_fixture("safe-valcap-vs-discount");
    let heading_idx = diff
        .changes
        .iter()
        .position(|change| match change {
            DiffChange::BlockInserted {
                block: stemma::BlockNode::Paragraph(p),
                ..
            } => common::paragraph_text(p).contains("Company Representations"),
            DiffChange::BlockModified { new_text, .. } => {
                new_text.contains("Company Representations")
            }
            _ => false,
        })
        .expect("inserted heading paragraph");

    let separator = &diff.changes[heading_idx - 1];
    match separator {
        DiffChange::BlockInserted {
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
            DiffChange::BlockModified {
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
