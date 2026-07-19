//! Integration tests for the HEADERS/FOOTERS authoring verbs
//! (`EditStep::EditHeader` / `EditFooter` / `SetHeaderFooterMode`, §17.10).
//!
//! Covered here:
//! - T1: tracked `EditHeader` — reject-all restores the original header text,
//!   accept-all keeps the edited text; the edit is story-scoped (body untouched);
//! - opaque preservation inside a header: a `PAGE` field run survives an
//!   `EditHeader`, or the edit fails `OpaqueDestroyed`;
//! - `SetHeaderFooterMode` title_page / even_and_odd toggles;
//! - link / unlink an existing header reference.

use stemma::api::Document;
use stemma::domain::{BlockNode, HeaderFooterKind, InlineNode, NodeId, OpaqueKind, RevisionInfo};
use stemma::edit::{
    ContentFragment, EditStep, EditTransaction, HeaderFooterLink, MaterializationMode,
    ParagraphContent, StoryRef, apply_transaction,
};
use stemma::{accept_all, reject_all_with_styles};

/// Build a DOCX with a body, a `header1.xml` part (Default kind) whose single
/// paragraph optionally hosts a `PAGE` field, and a `headerReference` in the
/// body sectPr. When `with_page_field` is set the header paragraph is
/// "Page <PAGE>." so an opaque field run is present to test preservation.
fn make_header_docx(header_text: &str, with_page_field: bool) -> Vec<u8> {
    let header_para = if with_page_field {
        // A simple field (PAGE) is an opaque inline anchor in the IR.
        r#"<w:p><w:r><w:t xml:space="preserve">Page </w:t></w:r><w:fldSimple w:instr=" PAGE "><w:r><w:t>1</w:t></w:r></w:fldSimple><w:r><w:t>.</w:t></w:r></w:p>"#
            .to_string()
    } else {
        format!(r#"<w:p><w:r><w:t xml:space="preserve">{header_text}</w:t></w:r></w:p>"#)
    };
    let header_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">{header_para}</w:hdr>"#
    );

    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>
<w:p><w:r><w:t>Body paragraph.</w:t></w:r></w:p>
<w:sectPr>
<w:headerReference w:type="default" r:id="rIdH1"/>
<w:pgSz w:w="12240" w:h="15840"/>
</w:sectPr>
</w:body></w:document>"#;

    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdH1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/></Relationships>"#;

    use std::io::Write;
    use zip::write::FileOptions;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        zip.start_file("[Content_Types].xml", opts).unwrap();
        zip.write_all(content_types.as_bytes()).unwrap();
        zip.start_file("_rels/.rels", opts).unwrap();
        zip.write_all(rels.as_bytes()).unwrap();
        zip.start_file("word/_rels/document.xml.rels", opts)
            .unwrap();
        zip.write_all(doc_rels.as_bytes()).unwrap();
        zip.start_file("word/document.xml", opts).unwrap();
        zip.write_all(document_xml.as_bytes()).unwrap();
        zip.start_file("word/header1.xml", opts).unwrap();
        zip.write_all(header_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

fn txn(steps: Vec<EditStep>, mode: MaterializationMode) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        materialization_mode: mode,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("Tester".to_string()),
            date: Some("2026-06-01T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

fn text_content(text: &str) -> ParagraphContent {
    ParagraphContent {
        fragments: vec![ContentFragment::Text(text.to_string())],
    }
}

/// Visible text of a header story's first paragraph (Text inlines only).
fn header_text(canon: &stemma::domain::CanonDoc, part: &str) -> String {
    let story = canon
        .headers
        .iter()
        .find(|h| h.part_name == part)
        .expect("header story present");
    story
        .blocks
        .iter()
        .flat_map(|b| match &b.block {
            BlockNode::Paragraph(p) => p.segments.clone(),
            _ => vec![],
        })
        .flat_map(|s| s.inlines)
        .filter_map(|i| match i {
            InlineNode::Text(t) => Some(t.text),
            _ => None,
        })
        .collect()
}

fn body_text(canon: &stemma::domain::CanonDoc) -> String {
    canon
        .blocks
        .iter()
        .flat_map(|b| match &b.block {
            BlockNode::Paragraph(p) => p.segments.clone(),
            _ => vec![],
        })
        .flat_map(|s| s.inlines)
        .filter_map(|i| match i {
            InlineNode::Text(t) => Some(t.text),
            _ => None,
        })
        .collect()
}

/// The header story's part name + first paragraph block id.
fn header_addr(canon: &stemma::domain::CanonDoc) -> (String, NodeId) {
    let story = canon.headers.first().expect("header story present");
    let block_id = story
        .blocks
        .iter()
        .find_map(|b| match &b.block {
            BlockNode::Paragraph(p) => Some(p.id.clone()),
            _ => None,
        })
        .expect("header has a paragraph block");
    (story.part_name.clone(), block_id)
}

/// T1: a tracked EditHeader reverses on reject and applies on accept, and the
/// edit is story-scoped — the body text never changes.
#[test]
fn tracked_edit_header_reject_restores_accept_keeps() {
    let doc = Document::parse(&make_header_docx("Confidential Draft", false)).expect("parse");
    let base = doc.snapshot().canonical.clone();
    let (part, block_id) = header_addr(&base);
    assert_eq!(header_text(&base, &part), "Confidential Draft");
    let base_body = body_text(&base);

    let steps = vec![EditStep::EditHeader {
        story: StoryRef::Header(part.clone()),
        block_id,
        expect: "Confidential Draft".to_string(),
        semantic_hash: None,
        content: text_content("Final Version"),
        rationale: None,
    }];

    let tracked = apply_transaction(
        &base,
        &txn(steps.clone(), MaterializationMode::TrackedChange),
    )
    .expect("tracked edit_header")
    .0;

    // reject-all restores the original header text.
    let mut rejected = tracked.clone();
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        header_text(&rejected, &part),
        "Confidential Draft",
        "reject restores the original header text"
    );

    // accept-all keeps the edited header text.
    let mut accepted = tracked.clone();
    accept_all(&mut accepted);
    assert_eq!(
        header_text(&accepted, &part),
        "Final Version",
        "accept keeps the edited header text"
    );

    // The body is untouched on both projections (story-scoped edit).
    assert_eq!(body_text(&tracked), base_body, "body untouched (tracked)");
    assert_eq!(body_text(&accepted), base_body, "body untouched (accepted)");
}

/// Opaque preservation: an `EditHeader` that drops the PAGE field opaque is
/// refused with `OpaqueDestroyed`; an edit that preserves it succeeds.
#[test]
fn edit_header_preserves_page_field_or_fails_loud() {
    let doc = Document::parse(&make_header_docx("", true)).expect("parse");
    let base = doc.snapshot().canonical.clone();
    let (part, block_id) = header_addr(&base);

    // Confirm the header carries an opaque field anchor.
    let opaque_id = base
        .headers
        .iter()
        .find(|h| h.part_name == part)
        .unwrap()
        .blocks
        .iter()
        .flat_map(|b| match &b.block {
            BlockNode::Paragraph(p) => p.segments.clone(),
            _ => vec![],
        })
        .flat_map(|s| s.inlines)
        .find_map(|i| match i {
            InlineNode::OpaqueInline(o) if matches!(o.kind, OpaqueKind::Field(_)) => Some(o.id),
            _ => None,
        })
        .expect("header has a PAGE field opaque");

    // (a) An edit that does NOT reference the opaque drops it → OpaqueDestroyed.
    let drop_err = apply_transaction(
        &base,
        &txn(
            vec![EditStep::EditHeader {
                story: StoryRef::Header(part.clone()),
                block_id: block_id.clone(),
                expect: "Page ".to_string(),
                semantic_hash: None,
                content: text_content("No field here"),
                rationale: None,
            }],
            MaterializationMode::TrackedChange,
        ),
    )
    .expect_err("dropping the PAGE field must fail loud");
    assert!(
        matches!(drop_err, stemma::edit::EditError::OpaqueDestroyed { .. }),
        "got {drop_err:?}"
    );

    // (b) An edit that preserves the opaque (references it) succeeds and keeps it.
    let ok = apply_transaction(
        &base,
        &txn(
            vec![EditStep::EditHeader {
                story: StoryRef::Header(part.clone()),
                block_id,
                expect: "Page ".to_string(),
                semantic_hash: None,
                content: ParagraphContent {
                    fragments: vec![
                        ContentFragment::Text("Page no. ".to_string()),
                        ContentFragment::PreservedInlineRef(opaque_id.clone()),
                        ContentFragment::Text(" total.".to_string()),
                    ],
                },
                rationale: None,
            }],
            MaterializationMode::Direct,
        ),
    )
    .expect("preserving the PAGE field succeeds")
    .0;

    // The opaque survives.
    let survived = ok
        .headers
        .iter()
        .find(|h| h.part_name == part)
        .unwrap()
        .blocks
        .iter()
        .flat_map(|b| match &b.block {
            BlockNode::Paragraph(p) => p.segments.clone(),
            _ => vec![],
        })
        .flat_map(|s| s.inlines)
        .any(|i| matches!(i, InlineNode::OpaqueInline(o) if o.id == opaque_id));
    assert!(
        survived,
        "the PAGE field opaque survives the preserving edit"
    );
}

/// `SetHeaderFooterMode` toggles titlePg and evenAndOddHeaders on the section /
/// document; the toggle round-trips through the IR.
#[test]
fn set_header_footer_mode_title_page_and_even_odd_toggle() {
    let doc = Document::parse(&make_header_docx("Head", false)).expect("parse");
    let base = doc.snapshot().canonical.clone();

    let result = apply_transaction(
        &base,
        &txn(
            vec![EditStep::SetHeaderFooterMode {
                title_page: Some(true),
                even_and_odd: Some(true),
                link: None,
                rationale: None,
            }],
            MaterializationMode::Direct,
        ),
    )
    .expect("set mode ok")
    .0;

    assert_eq!(
        result.body_section_properties.as_ref().unwrap().title_page,
        Some(true),
        "titlePg set on the section"
    );
    assert_eq!(
        result.even_and_odd_headers,
        Some(true),
        "evenAndOddHeaders set on the document"
    );

    // Explicit-off is distinct from absent.
    let off = apply_transaction(
        &result,
        &txn(
            vec![EditStep::SetHeaderFooterMode {
                title_page: Some(false),
                even_and_odd: Some(false),
                link: None,
                rationale: None,
            }],
            MaterializationMode::Direct,
        ),
    )
    .expect("set mode off ok")
    .0;
    assert_eq!(
        off.even_and_odd_headers,
        Some(false),
        "explicit off, not absent"
    );
}

/// Link an existing header reference, then unlink it. Linking a kind with no
/// existing story fails loud.
#[test]
fn link_and_unlink_existing_header_reference() {
    let doc = Document::parse(&make_header_docx("Head", false)).expect("parse");
    let base = doc.snapshot().canonical.clone();

    // Linking a FIRST-page header that has no story fails loud (v1 links
    // existing stories only).
    let err = apply_transaction(
        &base,
        &txn(
            vec![EditStep::SetHeaderFooterMode {
                title_page: None,
                even_and_odd: None,
                link: Some(HeaderFooterLink {
                    is_header: true,
                    kind: HeaderFooterKind::First,
                    link: true,
                }),
                rationale: None,
            }],
            MaterializationMode::Direct,
        ),
    )
    .expect_err("linking a non-existent first-page header must fail");
    assert!(
        matches!(
            err,
            stemma::edit::EditError::HeaderFooterRefNotResolvable { .. }
        ),
        "got {err:?}"
    );

    // Unlink the existing Default header reference, then relink it.
    let unlinked = apply_transaction(
        &base,
        &txn(
            vec![EditStep::SetHeaderFooterMode {
                title_page: None,
                even_and_odd: None,
                link: Some(HeaderFooterLink {
                    is_header: true,
                    kind: HeaderFooterKind::Default,
                    link: false,
                }),
                rationale: None,
            }],
            MaterializationMode::Direct,
        ),
    )
    .expect("unlink ok")
    .0;
    assert!(
        !unlinked
            .body_section_properties
            .as_ref()
            .unwrap()
            .header_refs
            .iter()
            .any(|r| r.kind == HeaderFooterKind::Default),
        "Default header reference removed"
    );

    let relinked = apply_transaction(
        &unlinked,
        &txn(
            vec![EditStep::SetHeaderFooterMode {
                title_page: None,
                even_and_odd: None,
                link: Some(HeaderFooterLink {
                    is_header: true,
                    kind: HeaderFooterKind::Default,
                    link: true,
                }),
                rationale: None,
            }],
            MaterializationMode::Direct,
        ),
    )
    .expect("relink ok")
    .0;
    assert!(
        relinked
            .body_section_properties
            .as_ref()
            .unwrap()
            .header_refs
            .iter()
            .any(|r| r.kind == HeaderFooterKind::Default),
        "Default header reference relinked to header1.xml"
    );
}
