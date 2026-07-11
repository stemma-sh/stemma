//! Selective (by-id) and blanket (accept-all/reject-all) RESOLUTION of
//! footnote/endnote-story tracked changes and body/mid-document
//! `w:sectPrChange`.
//!
//! THE CONTRACT (mirrors `spec_selective_formatting_resolution.rs` for
//! pPrChange): once a revision is enumerable (`spec_revision_enumeration.rs`
//! pins that half), resolving it by id must actually MATERIALIZE the
//! decision — footnote/endnote ins/del resolve exactly like body ins/del
//! (accept ins = keep text, drop the marker; accept del = remove the text;
//! reject mirrors), writing correct `word/footnotes.xml`/`word/endnotes.xml`;
//! `w:sectPrChange` accept = keep the live `w:sectPr`, drop the change
//! element; reject = restore the embedded prior `w:sectPr`.
//!
//! THE CRITICAL PROPERTY: a mixed document (body text by one author, a
//! footnote revision and a sectPrChange by another) resolved via
//! `{"by":"all"}` must leave ZERO tracked-change machinery ANYWHERE — not
//! just in the body. Before this fix, `accept_changes{by:"all"}` on such a
//! document returned a GREEN receipt while the footnote/endnote/sectPr
//! revisions survived untouched (found fabricating
//! held-out benchmark validation of footnote-story and sectPr revisions) —
//! a confident receipt over a silently half-resolved document.
//!
//! Source of truth throughout: the model (`ParagraphNode`/`FootnoteStory`/
//! `body_section_properties` fields) and the RE-SERIALIZED markup — never
//! `list_revisions` for the resolution verdict (it can be blind to
//! resolution state; see spec_revision_enumeration.rs's own warning), and
//! never a raw XML fragment string-equality comparison (compare parsed
//! attributes/structure, or canonicalize).

use std::collections::HashSet;

use stemma::RevisionKind;
use stemma::api::Document;
use stemma::edit::{
    EditStep, EditTransaction, MaterializationMode, PageMargins, PageSetupPatch, SectionTarget,
};
use stemma::tracked_model::{ResolveSelectionAction, enumerate_revisions};
use stemma::{Resolution, RevisionInfo, StoryScope};

// ─── Zip-building helpers (mirrors spec_revision_enumeration.rs — integration
// tests do not share modules, so these are deliberately duplicated) ─────────

const FOOTNOTE_REL: &str = r#"<Relationship Id="rIdFootnotes" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes" Target="footnotes.xml"/>"#;
const FOOTNOTE_CT: &str = r#"<Override PartName="/word/footnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"/>"#;

fn make_docx_with_footnotes(body_inner: &str, footnotes_xml: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>{FOOTNOTE_CT}</Types>"#
    );
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{FOOTNOTE_REL}</Relationships>"#
    );
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
        zip.start_file("word/footnotes.xml", opts).unwrap();
        zip.write_all(footnotes_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

fn make_docx_with_body(body_inner: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
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
        zip.finish().unwrap();
    }
    buf
}

fn footnote_part_xml(author: &str, date: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote>
<w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote>
<w:footnote w:id="1"><w:p>
<w:r><w:footnoteRef/></w:r>
<w:r><w:t xml:space="preserve"> Based on the </w:t></w:r>
<w:del w:id="101" w:author="{author}" w:date="{date}"><w:r><w:delText xml:space="preserve">2024</w:delText></w:r></w:del>
<w:ins w:id="102" w:author="{author}" w:date="{date}"><w:r><w:t xml:space="preserve">2025</w:t></w:r></w:ins>
<w:r><w:t xml:space="preserve"> survey.</w:t></w:r>
</w:p></w:footnote>
</w:footnotes>"#
    )
}

const BODY_WITH_FOOTNOTE_REF: &str = r#"<w:p><w:r><w:t>Claim needing a citation</w:t></w:r><w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:footnoteReference w:id="1"/></w:r></w:p>"#;
const BODY_TEXT_AND_FOOTNOTE_REF: &str = r#"<w:p><w:r><w:t>Program costs are itemized below.</w:t></w:r><w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:footnoteReference w:id="1"/></w:r></w:p><w:p><w:r><w:t>The collective plans to expand its outreach programming.</w:t></w:r></w:p>"#;

fn footnote_text(doc: &Document) -> String {
    doc.snapshot()
        .canonical
        .footnotes
        .iter()
        .find(|f| f.id == "1")
        .expect("footnote 1")
        .blocks
        .iter()
        .flat_map(|tb| match &tb.block {
            stemma::BlockNode::Paragraph(p) => {
                p.segments.iter().flat_map(|s| s.inlines.iter()).collect()
            }
            _ => vec![],
        })
        .filter_map(|i| match i {
            stemma::InlineNode::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect()
}

fn footnote_pending_ids(doc: &Document) -> Vec<u32> {
    enumerate_revisions(&doc.snapshot().canonical)
        .into_iter()
        .filter(|r| matches!(r.location, StoryScope::Footnote { .. }))
        .map(|r| r.revision_id)
        .collect()
}

fn revision(id: u32, author: &str) -> RevisionInfo {
    RevisionInfo {
        revision_id: id,
        author: Some(author.to_string()),
        date: Some("2026-06-12T00:00:00Z".to_string()),
        apply_op_id: None,
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Footnote-story resolution
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn accepting_a_footnote_del_and_ins_by_id_materializes_the_correction_in_footnotes_xml() {
    let bytes = make_docx_with_footnotes(
        BODY_WITH_FOOTNOTE_REF,
        &footnote_part_xml("L. Marsh", "2026-06-05T10:00:00Z"),
    );
    let doc = Document::parse(&bytes).expect("parse");
    // Pending state: BOTH halves of the tracked pair are present in the
    // model simultaneously (the domain rule — a real Word track-changes view
    // shows the struck-through OLD text and the underlined NEW text at once,
    // until resolved; `footnote_text` flattens every segment regardless of
    // status, so it reflects that literally).
    assert_eq!(
        footnote_text(&doc),
        " Based on the 20242025 survey.",
        "pending: BOTH old (struck) and new (inserted) text are present"
    );

    let resolved = doc
        .project(Resolution::Selective {
            ids: HashSet::from([101, 102]),
            action: ResolveSelectionAction::Accept,
        })
        .expect("selective accept of the footnote pair");

    assert_eq!(
        footnote_text(&resolved),
        " Based on the 2025 survey.",
        "accept must keep the NEW text (2025), dropping the OLD (2024)"
    );
    assert!(
        footnote_pending_ids(&resolved).is_empty(),
        "no footnote revision markers left pending after accept"
    );

    // Re-serialize and confirm footnotes.xml is valid, marker-free, and the
    // engine can re-import it clean.
    let out = resolved
        .serialize(&stemma::ExportOptions {
            mode: stemma::ExportMode::Redline,
            validator_level: stemma::ValidatorLevel::Blocking,
            validator: None,
        })
        .expect("serialize the resolved footnote");
    let fn_xml = String::from_utf8(
        stemma::docx::DocxArchive::read(&out)
            .expect("zip")
            .get("word/footnotes.xml")
            .expect("footnotes.xml present")
            .to_vec(),
    )
    .expect("utf8");
    assert!(
        !fn_xml.contains("w:ins") && !fn_xml.contains("w:del"),
        "no tracked markers remain: {fn_xml}"
    );
    assert!(
        fn_xml.contains("2025") && !fn_xml.contains("2024"),
        "new text kept, old text gone: {fn_xml}"
    );
    let reparsed = Document::parse(&out).expect("re-import the resolved footnote clean");
    assert_eq!(footnote_text(&reparsed), " Based on the 2025 survey.");
}

#[test]
fn rejecting_a_footnote_del_and_ins_by_id_restores_the_original_text() {
    let bytes = make_docx_with_footnotes(
        BODY_WITH_FOOTNOTE_REF,
        &footnote_part_xml("L. Marsh", "2026-06-05T10:00:00Z"),
    );
    let doc = Document::parse(&bytes).expect("parse");
    let resolved = doc
        .project(Resolution::Selective {
            ids: HashSet::from([101, 102]),
            action: ResolveSelectionAction::Reject,
        })
        .expect("selective reject of the footnote pair");
    assert_eq!(
        footnote_text(&resolved),
        " Based on the 2024 survey.",
        "reject must restore the OLD text (2024), dropping the NEW (2025)"
    );
    assert!(footnote_pending_ids(&resolved).is_empty());
}

#[test]
fn an_unselected_footnote_revision_stays_pending() {
    // Resolve a DIFFERENT id (a body edit) — the footnote revisions must
    // survive untouched, ids intact. Mirrors
    // spec_selective_formatting_resolution.rs::an_unselected_ppr_change_stays_pending.
    let bytes = make_docx_with_footnotes(
        BODY_TEXT_AND_FOOTNOTE_REF,
        &footnote_part_xml("L. Marsh", "2026-06-05T10:00:00Z"),
    );
    let doc = Document::parse(&bytes).expect("parse");
    let view = doc.read();
    let target = view
        .blocks
        .iter()
        .find(|b| b.text.contains("expand its outreach"))
        .expect("body paragraph");
    let doc = doc
        .apply(&EditTransaction {
            steps: vec![EditStep::ReplaceParagraphText {
                block_id: target.id.clone(),
                rationale: None,
                replacement_role: None,
                expect: "The collective plans to expand its outreach programming.".to_string(),
                semantic_hash: Some(target.guard.clone()),
                content: stemma::edit::ParagraphContent {
                    fragments: vec![stemma::edit::ContentFragment::Text(
                        "The collective plans to expand its youth outreach programming."
                            .to_string(),
                    )],
                },
            }],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: revision(200, "T. Byrne"),
        })
        .expect("body edit applies");

    let body_id = enumerate_revisions(&doc.snapshot().canonical)
        .into_iter()
        .find(|r| r.location == StoryScope::Body)
        .expect("the body edit is enumerable")
        .revision_id;

    let resolved = doc
        .project(Resolution::Selective {
            ids: HashSet::from([body_id]),
            action: ResolveSelectionAction::Accept,
        })
        .expect("resolve only the body edit");

    assert_eq!(
        footnote_text(&resolved),
        " Based on the 20242025 survey.",
        "untouched — the footnote's pending text (both halves) is unchanged"
    );
    assert_eq!(
        footnote_pending_ids(&resolved),
        vec![101, 102],
        "footnote revision ids survive intact, still pending"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Body sectPrChange resolution
// ═══════════════════════════════════════════════════════════════════════════

fn set_page_setup_step(target: SectionTarget) -> EditStep {
    EditStep::SetPageSetup {
        target,
        patch: PageSetupPatch {
            margins: Some(PageMargins {
                top: 1080,
                bottom: 1080,
                left: 1080,
                right: 1080,
                header: 720,
                footer: 720,
            }),
            ..Default::default()
        },
        semantic_hash: None,
        rationale: None,
    }
}

fn body_margins(doc: &Document) -> Option<i32> {
    doc.snapshot()
        .canonical
        .body_section_properties
        .as_ref()
        .and_then(|sp| sp.margin_top)
}

#[test]
fn accepting_a_body_sectpr_change_by_id_keeps_the_new_margins() {
    const TWO_PARAS: &str =
        r#"<w:p><w:r><w:t>Alpha.</w:t></w:r></w:p><w:p><w:r><w:t>Beta.</w:t></w:r></w:p>"#;
    let doc = Document::parse(&make_docx_with_body(TWO_PARAS)).expect("parse");
    let original_top = body_margins(&doc);
    let doc = doc
        .apply(&EditTransaction {
            steps: vec![set_page_setup_step(SectionTarget::Body)],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: revision(300, "R. Okafor"),
        })
        .expect("tracked sectPrChange applies");
    assert_eq!(
        body_margins(&doc),
        Some(1080),
        "new margins live immediately"
    );
    assert_ne!(
        body_margins(&doc),
        original_top,
        "must actually differ from the original"
    );

    let sect_id = enumerate_revisions(&doc.snapshot().canonical)
        .into_iter()
        .find(|r| r.kind == RevisionKind::FormatSection && r.location == StoryScope::Body)
        .expect("the sectPrChange is enumerable")
        .revision_id;

    let resolved = doc
        .project(Resolution::Selective {
            ids: HashSet::from([sect_id]),
            action: ResolveSelectionAction::Accept,
        })
        .expect("selective accept of the sectPrChange");
    assert_eq!(
        body_margins(&resolved),
        Some(1080),
        "accept keeps the new margins"
    );
    assert!(
        resolved
            .snapshot()
            .canonical
            .body_section_property_change
            .is_none(),
        "the change record is gone after accept"
    );

    let report = resolved.serialize(&stemma::ExportOptions {
        mode: stemma::ExportMode::Redline,
        validator_level: stemma::ValidatorLevel::Blocking,
        validator: None,
    });
    assert!(
        report.is_ok(),
        "resolved sectPr must serialize clean: {report:?}"
    );
}

#[test]
fn rejecting_a_body_sectpr_change_by_id_restores_the_previous_margins() {
    const TWO_PARAS: &str =
        r#"<w:p><w:r><w:t>Alpha.</w:t></w:r></w:p><w:p><w:r><w:t>Beta.</w:t></w:r></w:p>"#;
    let doc = Document::parse(&make_docx_with_body(TWO_PARAS)).expect("parse");
    let original_top = body_margins(&doc);
    let doc = doc
        .apply(&EditTransaction {
            steps: vec![set_page_setup_step(SectionTarget::Body)],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: revision(300, "R. Okafor"),
        })
        .expect("tracked sectPrChange applies");
    let sect_id = enumerate_revisions(&doc.snapshot().canonical)
        .into_iter()
        .find(|r| r.kind == RevisionKind::FormatSection && r.location == StoryScope::Body)
        .expect("the sectPrChange is enumerable")
        .revision_id;

    let resolved = doc
        .project(Resolution::Selective {
            ids: HashSet::from([sect_id]),
            action: ResolveSelectionAction::Reject,
        })
        .expect("selective reject of the sectPrChange");
    assert_eq!(
        body_margins(&resolved),
        original_top,
        "reject restores the ORIGINAL margins exactly"
    );
    assert!(
        resolved
            .snapshot()
            .canonical
            .body_section_property_change
            .is_none()
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// THE CRITICAL PROPERTY: mixed doc, resolve everything, nothing left dangling
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn resolving_every_enumerated_id_in_a_mixed_doc_leaves_zero_tracked_machinery_anywhere() {
    // Body text by T. Byrne + a footnote correction by L. Marsh + a body
    // sectPrChange by L. Marsh — exactly the shape
    // the held-out footnote-story and sectPr benchmark validation fixtures
    // fabricated (fresh content here, no fixture reference). `{"by":"all"}`
    // at the MCP layer selects every id `enumerate_revisions` returns; this
    // test exercises the identical engine-level operation: resolve the FULL
    // set of enumerated ids.
    let bytes = make_docx_with_footnotes(
        BODY_TEXT_AND_FOOTNOTE_REF,
        &footnote_part_xml("L. Marsh", "2026-06-05T10:00:00Z"),
    );
    let doc = Document::parse(&bytes).expect("parse");

    let view = doc.read();
    let target = view
        .blocks
        .iter()
        .find(|b| b.text.contains("expand its outreach"))
        .expect("body paragraph");
    let doc = doc
        .apply(&EditTransaction {
            steps: vec![EditStep::ReplaceParagraphText {
                block_id: target.id.clone(),
                rationale: None,
                replacement_role: None,
                expect: "The collective plans to expand its outreach programming.".to_string(),
                semantic_hash: Some(target.guard.clone()),
                content: stemma::edit::ParagraphContent {
                    fragments: vec![stemma::edit::ContentFragment::Text(
                        "The collective plans to expand its youth outreach programming."
                            .to_string(),
                    )],
                },
            }],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: revision(200, "T. Byrne"),
        })
        .expect("body edit applies");

    let doc = doc
        .apply(&EditTransaction {
            steps: vec![set_page_setup_step(SectionTarget::Body)],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: revision(300, "L. Marsh"),
        })
        .expect("tracked sectPrChange applies");

    let all_ids: HashSet<u32> = enumerate_revisions(&doc.snapshot().canonical)
        .into_iter()
        .map(|r| r.revision_id)
        .collect();
    assert!(
        all_ids.len() >= 4,
        "expect at least: 2 footnote (del+ins) + 1 body text + 1 sectPrChange, got {all_ids:?}"
    );

    let resolved = doc
        .project(Resolution::Selective {
            ids: all_ids,
            action: ResolveSelectionAction::Accept,
        })
        .expect("resolve EVERY enumerated id");

    // ── Model-level: nothing pending anywhere ──────────────────────────────
    assert!(
        enumerate_revisions(&resolved.snapshot().canonical).is_empty(),
        "zero revisions enumerable after resolving every id"
    );
    assert!(
        resolved
            .snapshot()
            .canonical
            .body_section_property_change
            .is_none()
    );

    // ── Serialized-markup level: the actual ground truth (never trust the
    //    model alone — see spec_revision_enumeration.rs's own warning) ─────
    let out = resolved
        .serialize(&stemma::ExportOptions {
            mode: stemma::ExportMode::Redline,
            validator_level: stemma::ValidatorLevel::Blocking,
            validator: None,
        })
        .expect("the fully-resolved mixed doc must serialize AND pass the blocking validator");

    let archive = stemma::docx::DocxArchive::read(&out).expect("zip");
    let doc_xml = String::from_utf8(archive.get("word/document.xml").unwrap().to_vec()).unwrap();
    let fn_xml = String::from_utf8(archive.get("word/footnotes.xml").unwrap().to_vec()).unwrap();
    for (label, xml) in [("document.xml", &doc_xml), ("footnotes.xml", &fn_xml)] {
        for marker in ["w:ins", "w:del", "w:sectPrChange", "w:pPrChange"] {
            assert!(
                !xml.contains(marker),
                "{label} must carry ZERO {marker} after resolving every id — \
                 a marker surviving here is a silent half-resolution behind a \
                 confident receipt: {xml}"
            );
        }
    }
    assert!(
        fn_xml.contains("2025") && !fn_xml.contains("2024"),
        "footnote correction kept"
    );

    // ── Re-import must be clean too ────────────────────────────────────────
    let reparsed = Document::parse(&out).expect("re-import the fully-resolved doc clean");
    assert!(enumerate_revisions(&reparsed.snapshot().canonical).is_empty());
}

// ═══════════════════════════════════════════════════════════════════════════
// accept_all / reject_all (the BLANKET functions) must agree with
// fully-selective resolution — pinning they share the walk, not a blind copy.
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn accept_all_agrees_with_resolving_every_id_selectively_for_footnotes_and_sectpr() {
    let bytes = make_docx_with_footnotes(
        BODY_WITH_FOOTNOTE_REF,
        &footnote_part_xml("L. Marsh", "2026-06-05T10:00:00Z"),
    );
    let doc = Document::parse(&bytes).expect("parse");
    let doc = doc
        .apply(&EditTransaction {
            steps: vec![set_page_setup_step(SectionTarget::Body)],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: revision(300, "L. Marsh"),
        })
        .expect("tracked sectPrChange applies");

    let via_accept_all = doc
        .project(Resolution::AcceptAll)
        .expect("accept_all resolves the mixed doc");
    assert!(enumerate_revisions(&via_accept_all.snapshot().canonical).is_empty());
    assert_eq!(footnote_text(&via_accept_all), " Based on the 2025 survey.");
    assert_eq!(body_margins(&via_accept_all), Some(1080));

    let all_ids: HashSet<u32> = enumerate_revisions(&doc.snapshot().canonical)
        .into_iter()
        .map(|r| r.revision_id)
        .collect();
    let via_selective_all = doc
        .project(Resolution::Selective {
            ids: all_ids,
            action: ResolveSelectionAction::Accept,
        })
        .expect("selectively resolving every id");

    assert_eq!(
        footnote_text(&via_accept_all),
        footnote_text(&via_selective_all),
        "accept_all and fully-selective accept must agree on footnote text"
    );
    assert_eq!(
        body_margins(&via_accept_all),
        body_margins(&via_selective_all),
        "accept_all and fully-selective accept must agree on body margins"
    );
}

#[test]
fn reject_all_agrees_with_resolving_every_id_selectively_for_footnotes_and_sectpr() {
    let bytes = make_docx_with_footnotes(
        BODY_WITH_FOOTNOTE_REF,
        &footnote_part_xml("L. Marsh", "2026-06-05T10:00:00Z"),
    );
    let doc = Document::parse(&bytes).expect("parse");
    let original_top = body_margins(&doc);
    let doc = doc
        .apply(&EditTransaction {
            steps: vec![set_page_setup_step(SectionTarget::Body)],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: revision(300, "L. Marsh"),
        })
        .expect("tracked sectPrChange applies");

    let via_reject_all = doc
        .project(Resolution::RejectAll)
        .expect("reject_all resolves the mixed doc");
    assert!(enumerate_revisions(&via_reject_all.snapshot().canonical).is_empty());
    assert_eq!(footnote_text(&via_reject_all), " Based on the 2024 survey.");
    assert_eq!(body_margins(&via_reject_all), original_top);

    let all_ids: HashSet<u32> = enumerate_revisions(&doc.snapshot().canonical)
        .into_iter()
        .map(|r| r.revision_id)
        .collect();
    let via_selective_all = doc
        .project(Resolution::Selective {
            ids: all_ids,
            action: ResolveSelectionAction::Reject,
        })
        .expect("selectively rejecting every id");

    assert_eq!(
        footnote_text(&via_reject_all),
        footnote_text(&via_selective_all)
    );
    assert_eq!(
        body_margins(&via_reject_all),
        body_margins(&via_selective_all)
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Mid-document (paragraph-level) sectPrChange — found in passing, same fix
// ═══════════════════════════════════════════════════════════════════════════

const BODY_WITH_MIDDOC_SECTION_BREAK: &str = r#"<w:p><w:pPr><w:sectPr><w:pgMar w:top="1440" w:bottom="1440" w:left="1440" w:right="1440" w:header="720" w:footer="720"/></w:sectPr></w:pPr><w:r><w:t>End of section one.</w:t></w:r></w:p><w:p><w:r><w:t>Section two body.</w:t></w:r></w:p>"#;

#[test]
fn mid_document_section_break_change_resolves_selectively() {
    let doc = Document::parse(&make_docx_with_body(BODY_WITH_MIDDOC_SECTION_BREAK)).expect("parse");
    let view = doc.read();
    let section_para = view
        .blocks
        .iter()
        .find(|b| b.text.contains("End of section one"))
        .expect("the mid-document section-break paragraph");
    let doc = doc
        .apply(&EditTransaction {
            steps: vec![set_page_setup_step(SectionTarget::Paragraph(
                section_para.id.clone(),
            ))],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: revision(400, "R. Okafor"),
        })
        .expect("tracked mid-document sectPrChange applies");

    let sect_id = enumerate_revisions(&doc.snapshot().canonical)
        .into_iter()
        .find(|r| r.kind == RevisionKind::FormatSection && r.block_id == section_para.id)
        .unwrap_or_else(|| {
            panic!(
                "the mid-document sectPrChange must be enumerable under its paragraph's block_id"
            )
        })
        .revision_id;

    let accepted = doc
        .project(Resolution::Selective {
            ids: HashSet::from([sect_id]),
            action: ResolveSelectionAction::Accept,
        })
        .expect("selective accept of the mid-document sectPrChange");
    let p = accepted
        .snapshot()
        .canonical
        .blocks
        .iter()
        .find_map(|tb| match &tb.block {
            stemma::BlockNode::Paragraph(p) if p.id == section_para.id => Some(p),
            _ => None,
        })
        .expect("the section-break paragraph");
    assert!(
        p.section_property_change.is_none(),
        "accept drops the mid-document change record"
    );
    assert_eq!(
        p.section_properties.as_ref().and_then(|sp| sp.margin_top),
        Some(1080)
    );
}
