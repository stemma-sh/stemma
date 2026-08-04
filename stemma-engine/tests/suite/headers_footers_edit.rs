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

/// A header run's effective style is one value with one resolution rule.
///
/// ISO 29500-1 §17.7.4.17: an unstyled paragraph implicitly references the
/// default paragraph style — in EVERY story, and in every code path that
/// resolves run properties. The story import path used to skip the fallback
/// the body path applied, so a header run imported WITHOUT the default
/// style's rPr contribution — and the first body-only apply (whose rebuild
/// re-resolves marks document-wide, WITH the fallback) visibly changed the
/// header's effective fonts in memory while the wire never changed.
#[test]
fn a_body_edit_does_not_move_a_header_runs_effective_style() {
    let header_xml = concat!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
        r#"<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
        r#"<w:p><w:r><w:t>HDR</w:t></w:r></w:p></w:hdr>"#,
    );
    let styles_xml = concat!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
        r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
        r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/>"#,
        r#"<w:rPr><w:rFonts w:ascii="Arial"/><w:sz w:val="22"/></w:rPr></w:style>"#,
        r#"</w:styles>"#,
    );
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>
<w:p><w:r><w:t>Body paragraph here.</w:t></w:r></w:p>
<w:sectPr><w:headerReference w:type="default" r:id="rIdH1"/></w:sectPr>
</w:body></w:document>"#;
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdH1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/><Relationship Id="rIdS1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#;

    use std::io::Write;
    use zip::write::FileOptions;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        let mut write = |name: &str, content: &str| {
            zip.start_file(name, opts).unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        };
        write("[Content_Types].xml", content_types);
        write("_rels/.rels", rels);
        write("word/_rels/document.xml.rels", doc_rels);
        write("word/document.xml", document_xml);
        write("word/header1.xml", header_xml);
        write("word/styles.xml", styles_xml);
        zip.finish().unwrap();
    }

    let header_run_style = |doc: &Document| {
        let canon = doc.snapshot().canonical.clone();
        let para = &canon.headers[0].blocks[0];
        let BlockNode::Paragraph(p) = &para.block else {
            panic!("header paragraph");
        };
        let InlineNode::Text(t) = &p.segments[0].inlines[0] else {
            panic!("header run");
        };
        (t.style_props.font_family.clone(), t.style_props.font_size)
    };

    let doc = Document::parse(&buf).expect("parse");
    let imported = header_run_style(&doc);
    assert_eq!(
        imported,
        (Some("Arial".into()), Some(22)),
        "§17.7.4.17: the unstyled header paragraph resolves its runs against \
         the default paragraph style"
    );

    let body_id = {
        let canon = doc.snapshot().canonical.clone();
        let BlockNode::Paragraph(p) = &canon.blocks[0].block else {
            panic!("body paragraph");
        };
        p.id.clone()
    };
    let edited = doc
        .apply(&txn(
            vec![EditStep::ReplaceParagraphText {
                block_id: body_id,
                rationale: None,
                replacement_role: None,
                expect: "Body".to_string(),
                semantic_hash: None,
                content: text_content("Edited paragraph here."),
            }],
            MaterializationMode::TrackedChange,
        ))
        .expect("apply body edit");
    assert_eq!(
        header_run_style(&edited),
        imported,
        "a body-only edit must not move a header run's effective style"
    );

    let saved = edited
        .serialize(&stemma::ExportOptions::default())
        .expect("serialize");
    let reopened = Document::parse(&saved).expect("reopen");
    assert_eq!(
        header_run_style(&reopened),
        imported,
        "and the reopened document agrees with both"
    );
}

/// A header whose ENTIRE content is a pending insertion rejects to the
/// spec-blank story (§17.10.5) — and STAYS blank across save/reopen.
///
/// NOTE: this shape does NOT reach the synthesized-story serialization path
/// the wave-8 witness exercised (a multi-projection lifecycle where the
/// story normalizes to `synthesized` while the package retains the original
/// part; the serializer must drop the stale reference or the content
/// resurrects). A hermetic reproduction of the synthesized precondition is
/// an open test-construction task; this test pins the simpler
/// single-projection contract.
#[test]
fn a_fully_rejected_header_does_not_resurrect_across_save_and_reopen() {
    let header_xml = concat!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
        r#"<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
        r#"<w:p><w:pPr><w:rPr><w:ins w:id="7" w:author="R" w:date="2026-06-01T00:00:00Z"/></w:rPr></w:pPr>"#,
        r#"<w:ins w:id="8" w:author="R" w:date="2026-06-01T00:00:00Z">"#,
        r#"<w:r><w:t>Pending header text</w:t></w:r></w:ins></w:p></w:hdr>"#,
    );
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>
<w:p><w:r><w:t>Body paragraph.</w:t></w:r></w:p>
<w:sectPr><w:headerReference w:type="default" r:id="rIdH1"/></w:sectPr>
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
        let mut w = |n: &str, c: &str| {
            zip.start_file(n, opts).unwrap();
            zip.write_all(c.as_bytes()).unwrap();
        };
        w("[Content_Types].xml", content_types);
        w("_rels/.rels", rels);
        w("word/_rels/document.xml.rels", doc_rels);
        w("word/document.xml", document_xml);
        w("word/header1.xml", header_xml);
        zip.finish().unwrap();
    }

    let doc = Document::parse(&buf).expect("parse");
    let header_text = |d: &Document| -> String {
        d.snapshot()
            .canonical
            .headers
            .iter()
            .flat_map(|h| h.blocks.iter())
            .filter_map(|b| match &b.block {
                stemma::domain::BlockNode::Paragraph(p) => Some(
                    p.segments
                        .iter()
                        .flat_map(|s| s.inlines.iter())
                        .filter_map(|i| match i {
                            InlineNode::Text(t) => Some(t.text.as_str()),
                            _ => None,
                        })
                        .collect::<String>(),
                ),
                _ => None,
            })
            .collect()
    };
    assert!(header_text(&doc).contains("Pending header text"));

    let rejected = doc
        .project(stemma::Resolution::RejectAll)
        .expect("reject all");
    assert_eq!(
        header_text(&rejected),
        "",
        "reject removes the pending header content"
    );

    let saved = rejected
        .serialize(&stemma::ExportOptions::default())
        .expect("serialize projection");
    let reopened = Document::parse(&saved).expect("reopen");
    assert_eq!(
        header_text(&reopened),
        "",
        "the rejected header content must not resurrect from the package"
    );
}

/// Reference survival is PER-TARGET, never per-kind: a multi-section document
/// holds several same-kind stories, and a synthesized blank Default footer
/// (a section with no reference of its own) must not drop another section's
/// reference to a REAL Default footer on save (wave-8 real-word witness:
/// keying the drop on kind lost footer3.xml whenever any blank Default
/// existed, and the reopened document claimed a synthesized footer where the
/// in-memory one had content).
#[test]
fn a_blank_sections_synthesized_footer_does_not_drop_another_sections_real_one() {
    let footer_xml = concat!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
        r#"<w:ftr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
        r#"<w:p><w:r><w:t>Real footer content</w:t></w:r></w:p></w:ftr>"#,
    );
    // Section 1 (mid-document sectPr): NO footer reference — imports as a
    // synthesized blank Default footer story. Section 2 (body sectPr):
    // references the real footer.
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>
<w:p><w:r><w:t>Section one text.</w:t></w:r></w:p>
<w:p><w:pPr><w:sectPr><w:pgSz w:w="12240" w:h="15840"/></w:sectPr></w:pPr><w:r><w:t>Section boundary.</w:t></w:r></w:p>
<w:p><w:r><w:t>Section two text.</w:t></w:r></w:p>
<w:sectPr><w:footerReference w:type="default" r:id="rIdF1"/><w:pgSz w:w="12240" w:h="15840"/></w:sectPr>
</w:body></w:document>"#;
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdF1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/></Relationships>"#;
    use std::io::Write;
    use zip::write::FileOptions;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        let mut w = |n: &str, c: &str| {
            zip.start_file(n, opts).unwrap();
            zip.write_all(c.as_bytes()).unwrap();
        };
        w("[Content_Types].xml", content_types);
        w("_rels/.rels", rels);
        w("word/_rels/document.xml.rels", doc_rels);
        w("word/document.xml", document_xml);
        w("word/footer1.xml", footer_xml);
        zip.finish().unwrap();
    }

    let footer_texts = |d: &Document| -> Vec<String> {
        d.snapshot()
            .canonical
            .footers
            .iter()
            .filter(|f| !f.synthesized)
            .map(|f| {
                f.blocks
                    .iter()
                    .filter_map(|b| match &b.block {
                        BlockNode::Paragraph(p) => Some(
                            p.segments
                                .iter()
                                .flat_map(|s| s.inlines.iter())
                                .filter_map(|i| match i {
                                    InlineNode::Text(t) => Some(t.text.as_str()),
                                    _ => None,
                                })
                                .collect::<String>(),
                        ),
                        _ => None,
                    })
                    .collect()
            })
            .collect()
    };

    let doc = Document::parse(&buf).expect("parse");
    assert_eq!(footer_texts(&doc), vec!["Real footer content".to_string()]);

    let saved = doc
        .serialize(&stemma::ExportOptions::default())
        .expect("serialize");
    let reopened = Document::parse(&saved).expect("reopen");
    assert_eq!(
        footer_texts(&reopened),
        vec!["Real footer content".to_string()],
        "the real footer survives a save while a synthesized blank of the          same kind exists in another section"
    );
}
