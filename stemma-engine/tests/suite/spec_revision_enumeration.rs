//! Revision-enumeration completeness: "no invisible ink", resolution edition.
//!
//! THE CONTRACT: every tracked revision present in the serialized markup is
//! enumerable from the read surface (`DocumentView`: block statuses + segment
//! statuses + paragraph-mark statuses — the SAME traversal the MCP's
//! `list_revisions` and accept/reject selector lowering use). A revision the
//! read surface cannot enumerate is one no selector can resolve: "accept all
//! of author X" reports success, the projection shows zero pending for X, and
//! the saved file still carries X's markup — the projection lies.
//!
//! `tracked_model::enumerate_revisions` is the canonical
//! walk (segments, paragraph marks, table row/cell structure, cell-interior
//! paragraphs, and run/paragraph/table/row/cell formatting changes — which now
//! carry `revision_id` end to end: parsed from `w:id`, stored on the model,
//! re-emitted by the serializer, and resolvable selectively). The MCP's
//! `list_revisions` and selector lowering both source from it.
//!
//! The walk also covers footnote/endnote STORY
//! revisions (`doc.footnotes` / `doc.endnotes` — these were once completely
//! unreachable: invisible to
//! `list_revisions` AND unresolvable by `by_ids`/`by_author`/`all`, though
//! the underlying resolution machinery for stories already worked once an id
//! reached it — see `spec_story_and_section_resolution.rs`), and the
//! body-level section-properties change (`doc.body_section_property_change`,
//! `<w:sectPrChange>` on the body `w:sectPr`) and its mid-document sibling
//! (`ParagraphNode.section_property_change`, a section break's own
//! `<w:sectPrChange>`), found in passing while fixing the story gap — same
//! struct (`SectionPropertyChange`), same fix. Every `RevisionRecord` now
//! carries a `location: StoryScope` so a caller can tell WHICH story (or the
//! body) a revision lives in — see `location_*` tests below.
//!
//! The walk also covers HEADER,
//! FOOTER, and COMMENT stories — the last carriers `resolvable_revision_ids`
//! accepted but the listing hid. A comment's interior blocks enumerate like
//! any story's; its whole-story tracking status (the marker `comment_delete`
//! writes) enumerates under the `comment_story` sentinel block id. With this,
//! `enumerate_revisions` and `resolvable_revision_ids` agree on the COMPLETE
//! carrier set (pinned by
//! `enumerate_revisions_ids_agree_with_resolvable_revision_ids`), which is
//! what the audit census (`stemma::audit`) requires: a revision the census
//! cannot see is a change the audit would silently under-report. In the same
//! change `RevisionRecord::kind` was promoted from a collapsed string to
//! `RevisionKind`, with each `*PrChange` carrier a first-class kind.

use stemma::RevisionKind;
use stemma::api::Document;
use stemma::edit::{
    EditStep, EditTransaction, MaterializationMode, ParagraphFormattingPatch, TableOp,
};
use stemma::{Alignment, RevisionInfo};

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

const PARA_AND_TABLE: &str = r#"<w:p><w:r><w:t>Service levels apply.</w:t></w:r></w:p><w:p><w:r><w:t>Credits are the sole remedy.</w:t></w:r></w:p><w:tbl><w:tblPr/><w:tblGrid><w:gridCol/><w:gridCol/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>Metric</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>99.5%</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:p><w:r><w:t>Latency</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>4h</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#;
const TWO_PARAS: &str = r#"<w:p><w:r><w:t>Service levels apply.</w:t></w:r></w:p><w:p><w:r><w:t>Left alone.</w:t></w:r></w:p>"#;

fn revision(id: u32) -> RevisionInfo {
    RevisionInfo {
        revision_id: id,
        author: Some("enum-test".to_string()),
        date: Some("2026-06-12T00:00:00Z".to_string()),
        apply_op_id: None,
    }
}

fn apply(doc: &Document, step: EditStep, id: u32) -> Document {
    doc.apply(&EditTransaction {
        steps: vec![step],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: revision(id),
    })
    .expect("authoring step applies")
}

/// Every revision id enumerable from the resolution surface — the canonical
/// enumeration the MCP's `list_revisions` and selector lowering share.
fn view_enumerable_ids(doc: &Document) -> Vec<u32> {
    let mut ids: Vec<u32> = stemma::tracked_model::enumerate_revisions(&doc.snapshot().canonical)
        .into_iter()
        .map(|r| r.revision_id)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// Every revision id present in the serialized markup (any element carrying
/// `w:id` AND `w:author` — ins/del/cellDel/cellIns/pPrChange/rPrChange/...).
fn serialized_revision_ids(docx: &[u8]) -> Vec<u32> {
    let xml = String::from_utf8(
        stemma::docx::DocxArchive::read(docx)
            .expect("zip")
            .get("word/document.xml")
            .expect("document.xml")
            .to_vec(),
    )
    .expect("utf8");
    let mut ids = Vec::new();
    for elem in xml.split('<').filter(|e| e.contains("w:author=")) {
        if let Some(idpos) = elem.find("w:id=\"") {
            let rest = &elem[idpos + 6..];
            if let Some(end) = rest.find('"')
                && let Ok(id) = rest[..end].parse::<u32>()
            {
                ids.push(id);
            }
        }
    }
    ids.sort_unstable();
    ids.dedup();
    ids
}

#[test]
fn every_serialized_revision_is_enumerable_from_the_read_view() {
    let doc = Document::parse(&make_docx_with_body(PARA_AND_TABLE)).expect("parse");

    // One revision of each enumeration class:
    // inline text edit (enumerable today) …
    let view = doc.read();
    let para = view
        .blocks
        .iter()
        .find(|b| b.text.contains("Service levels"))
        .expect("paragraph");
    let doc = apply(
        &doc,
        EditStep::ReplaceParagraphText {
            block_id: para.id.clone(),
            rationale: None,
            replacement_role: None,
            expect: "Service levels apply.".to_string(),
            semantic_hash: Some(para.guard.clone()),
            content: stemma::edit::ParagraphContent {
                fragments: vec![stemma::edit::ContentFragment::Text(
                    "Service levels always apply.".to_string(),
                )],
            },
        },
        100,
    );

    // … a tracked table cell edit, a tracked row deletion, and a pPrChange
    // (all invisible to the view today).
    let view = doc.read();
    let table = view
        .blocks
        .iter()
        .find(|b| b.text.contains("Metric"))
        .expect("table");
    let doc = apply(
        &doc,
        EditStep::TableStructureOp {
            block_id: table.id.clone(),
            semantic_hash: Some(table.guard.clone()),
            op: TableOp::SetCellText {
                row_index: 0,
                col_index: 1,
                text: "99.9%".to_string(),
            },
            rationale: None,
        },
        110,
    );
    let view = doc.read();
    let table = view
        .blocks
        .iter()
        .find(|b| b.text.contains("Metric"))
        .expect("table");
    let doc = apply(
        &doc,
        EditStep::TableStructureOp {
            block_id: table.id.clone(),
            semantic_hash: Some(table.guard.clone()),
            op: TableOp::DeleteRow { row_index: 1 },
            rationale: None,
        },
        120,
    );
    let view = doc.read();
    let para = view
        .blocks
        .iter()
        .find(|b| b.text.contains("sole remedy"))
        .expect("paragraph");
    let doc = apply(
        &doc,
        EditStep::SetParagraphFormatting {
            block_id: para.id.clone(),
            semantic_hash: Some(para.guard.clone()),
            patch: ParagraphFormattingPatch {
                align: Some(Alignment::Center),
                ..Default::default()
            },
            rationale: None,
        },
        130,
    );

    let bytes = doc
        .serialize(&stemma::ExportOptions {
            mode: stemma::ExportMode::Redline,
            validator_level: stemma::ValidatorLevel::Blocking,
            validator: None,
        })
        .expect("serialize");

    let reparsed = Document::parse(&bytes).expect("re-parse");
    let enumerable = view_enumerable_ids(&reparsed);
    let serialized = serialized_revision_ids(&bytes);

    let invisible: Vec<u32> = serialized
        .iter()
        .copied()
        .filter(|id| !enumerable.contains(id))
        .collect();
    assert!(
        invisible.is_empty(),
        "every serialized revision must be enumerable from the read view \
         (else no selector can resolve it); invisible ids: {invisible:?} \
         (serialized {serialized:?}, enumerable {enumerable:?})"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Story + section-property enumeration
// ═══════════════════════════════════════════════════════════════════════════
//
// Footnotes/endnotes carry the SAME `w:ins`/`w:del` tracked-change grammar as
// body paragraphs, but the ONLY way to author one is to seed raw XML: the v4
// edit surface's `edit_note` is a wholesale UNTRACKED replace (it rebuilds
// the note body with `TrackingStatus::Normal` unconditionally — see
// `edit/verbs/footnotes.rs::apply_edit`), so there is no tracked in-place
// footnote-body edit verb to drive through `Document::apply`. These helpers
// hand-construct `word/footnotes.xml`/`word/endnotes.xml` plus the
// content-type override and relationship they need to be valid package parts
// — mirroring exactly how a real Word-authored tracked footnote edit is
// shaped (confirmed against the held-out benchmark fixtures this gap was
// found during held-out benchmark validation of footnote-story revisions).

use stemma::StoryScope;
use stemma::edit::{PageMargins, PageSetupPatch, SectionTarget};
use stemma::tracked_model::enumerate_revisions;

const FOOTNOTE_REL: &str = r#"<Relationship Id="rIdFootnotes" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes" Target="footnotes.xml"/>"#;
const FOOTNOTE_CT: &str = r#"<Override PartName="/word/footnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"/>"#;
const ENDNOTE_REL: &str = r#"<Relationship Id="rIdEndnotes" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/endnotes" Target="endnotes.xml"/>"#;
const ENDNOTE_CT: &str = r#"<Override PartName="/word/endnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.endnotes+xml"/>"#;

/// A minimal docx with a body plus an OPTIONAL footnotes.xml / endnotes.xml
/// part. Mirrors `make_docx_with_body`'s zip-building shape, extended with
/// the note-part plumbing (content-type override + relationship) a bare
/// `word/document.xml` change can't add on its own.
fn make_docx_with_notes(
    body_inner: &str,
    footnotes_xml: Option<&str>,
    endnotes_xml: Option<&str>,
) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
    );
    let mut ct_overrides = String::new();
    let mut rels_extra = String::new();
    if footnotes_xml.is_some() {
        ct_overrides.push_str(FOOTNOTE_CT);
        rels_extra.push_str(FOOTNOTE_REL);
    }
    if endnotes_xml.is_some() {
        ct_overrides.push_str(ENDNOTE_CT);
        rels_extra.push_str(ENDNOTE_REL);
    }
    let content_types = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>{ct_overrides}</Types>"#
    );
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{rels_extra}</Relationships>"#
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
        if let Some(fx) = footnotes_xml {
            zip.start_file("word/footnotes.xml", opts).unwrap();
            zip.write_all(fx.as_bytes()).unwrap();
        }
        if let Some(ex) = endnotes_xml {
            zip.start_file("word/endnotes.xml", opts).unwrap();
            zip.write_all(ex.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

/// A `word/footnotes.xml` (or endnotes.xml — same grammar) with the two
/// mandatory separator notes plus ONE real note (id 1) whose body carries a
/// tracked `w:del` ("2024") + `w:ins` ("2025") pair by `author`.
fn note_part_xml(elem: &str, author: &str, date: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:{elem}s xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:{elem} w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:{elem}>
<w:{elem} w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:{elem}>
<w:{elem} w:id="1"><w:p>
<w:r><w:{elem}Ref/></w:r>
<w:r><w:t xml:space="preserve"> Based on the </w:t></w:r>
<w:del w:id="101" w:author="{author}" w:date="{date}"><w:r><w:delText xml:space="preserve">2024</w:delText></w:r></w:del>
<w:ins w:id="102" w:author="{author}" w:date="{date}"><w:r><w:t xml:space="preserve">2025</w:t></w:r></w:ins>
<w:r><w:t xml:space="preserve"> survey.</w:t></w:r>
</w:p></w:{elem}>
</w:{elem}s>"#
    )
}

const BODY_WITH_FOOTNOTE_REF: &str = r#"<w:p><w:r><w:t>Claim needing a citation</w:t></w:r><w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:footnoteReference w:id="1"/></w:r></w:p>"#;
const BODY_WITH_ENDNOTE_REF: &str = r#"<w:p><w:r><w:t>Claim needing a citation</w:t></w:r><w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:endnoteReference w:id="1"/></w:r></w:p>"#;

#[test]
fn footnote_story_revisions_are_enumerable_with_footnote_location() {
    let bytes = make_docx_with_notes(
        BODY_WITH_FOOTNOTE_REF,
        Some(&note_part_xml(
            "footnote",
            "L. Marsh",
            "2026-06-05T10:00:00Z",
        )),
        None,
    );
    let doc = Document::parse(&bytes).expect("parse a doc with a tracked footnote revision");
    let records = enumerate_revisions(&doc.snapshot().canonical);
    let footnote_records: Vec<_> = records
        .iter()
        .filter(|r| matches!(&r.location, StoryScope::Footnote { id } if id == "1"))
        .collect();
    assert_eq!(
        footnote_records.len(),
        2,
        "the del+ins pair inside footnote 1 must both be enumerated: {records:?}"
    );
    assert!(
        footnote_records
            .iter()
            .any(|r| r.kind == RevisionKind::Delete && r.revision_id == 101),
        "the footnote's w:del (id 101) must be enumerated: {footnote_records:?}"
    );
    assert!(
        footnote_records
            .iter()
            .any(|r| r.kind == RevisionKind::Insert && r.revision_id == 102),
        "the footnote's w:ins (id 102) must be enumerated: {footnote_records:?}"
    );
    assert!(
        footnote_records
            .iter()
            .all(|r| r.author.as_deref() == Some("L. Marsh")),
        "author must be preserved: {footnote_records:?}"
    );
}

#[test]
fn endnote_story_revisions_are_enumerable_with_endnote_location() {
    let bytes = make_docx_with_notes(
        BODY_WITH_ENDNOTE_REF,
        None,
        Some(&note_part_xml(
            "endnote",
            "L. Marsh",
            "2026-06-05T10:00:00Z",
        )),
    );
    let doc = Document::parse(&bytes).expect("parse a doc with a tracked endnote revision");
    let records = enumerate_revisions(&doc.snapshot().canonical);
    let endnote_records: Vec<_> = records
        .iter()
        .filter(|r| matches!(&r.location, StoryScope::Endnote { id } if id == "1"))
        .collect();
    assert_eq!(
        endnote_records.len(),
        2,
        "the del+ins pair inside endnote 1 must both be enumerated: {records:?}"
    );
}

#[test]
fn body_paragraphs_are_tagged_with_the_body_location() {
    let doc = Document::parse(&make_docx_with_body(TWO_PARAS)).expect("parse");
    let view = doc.read();
    let para = view.blocks.first().expect("a paragraph");
    let doc = apply(
        &doc,
        EditStep::ReplaceParagraphText {
            block_id: para.id.clone(),
            rationale: None,
            replacement_role: None,
            expect: "Service levels apply.".to_string(),
            semantic_hash: Some(para.guard.clone()),
            content: stemma::edit::ParagraphContent {
                fragments: vec![stemma::edit::ContentFragment::Text(
                    "Service levels always apply.".to_string(),
                )],
            },
        },
        200,
    );
    let records = enumerate_revisions(&doc.snapshot().canonical);
    assert!(
        !records.is_empty() && records.iter().all(|r| r.location == StoryScope::Body),
        "every body revision must be tagged StoryScope::Body: {records:?}"
    );
}

#[test]
fn body_section_property_change_is_enumerable_with_body_location() {
    let doc = Document::parse(&make_docx_with_body(TWO_PARAS)).expect("parse");
    let doc = doc
        .apply(&EditTransaction {
            steps: vec![EditStep::SetPageSetup {
                target: SectionTarget::Body,
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
            }],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: revision(300),
        })
        .expect("tracked sectPrChange applies");
    let records = enumerate_revisions(&doc.snapshot().canonical);
    let sect_records: Vec<_> = records
        .iter()
        .filter(|r| r.kind == RevisionKind::FormatSection && r.location == StoryScope::Body)
        .collect();
    assert_eq!(
        sect_records.len(),
        1,
        "exactly one body-level sectPrChange must be enumerated: {records:?}"
    );
    assert_eq!(sect_records[0].author.as_deref(), Some("enum-test"));
}

#[test]
fn every_serialized_footnote_and_section_revision_is_enumerable() {
    // The general invariant test (`every_serialized_revision_is_enumerable_
    // from_the_read_view`), extended to a doc that ALSO carries a footnote
    // revision and a body sectPrChange — the exact mix the held-out fixture
    // used to find this gap.
    let bytes = make_docx_with_notes(
        BODY_WITH_FOOTNOTE_REF,
        Some(&note_part_xml(
            "footnote",
            "L. Marsh",
            "2026-06-05T10:00:00Z",
        )),
        None,
    );
    let doc = Document::parse(&bytes).expect("parse");
    let doc = doc
        .apply(&EditTransaction {
            steps: vec![EditStep::SetPageSetup {
                target: SectionTarget::Body,
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
            }],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: revision(300),
        })
        .expect("tracked sectPrChange applies");

    let out_bytes = doc
        .serialize(&stemma::ExportOptions {
            mode: stemma::ExportMode::Redline,
            validator_level: stemma::ValidatorLevel::Blocking,
            validator: None,
        })
        .expect("serialize");
    let reparsed = Document::parse(&out_bytes).expect("re-parse");
    let enumerable = view_enumerable_ids(&reparsed);

    // Serialized ids from BOTH document.xml (the sectPrChange) and
    // footnotes.xml (the del/ins pair) — `serialized_revision_ids` only reads
    // document.xml, so check footnotes.xml too.
    let mut serialized = serialized_revision_ids(&out_bytes);
    let archive = stemma::docx::DocxArchive::read(&out_bytes).expect("zip");
    let fn_xml = String::from_utf8(
        archive
            .get("word/footnotes.xml")
            .expect("footnotes.xml survives the round-trip")
            .to_vec(),
    )
    .expect("utf8");
    for elem in fn_xml.split('<').filter(|e| e.contains("w:author=")) {
        if let Some(idpos) = elem.find("w:id=\"") {
            let rest = &elem[idpos + 6..];
            if let Some(end) = rest.find('"')
                && let Ok(id) = rest[..end].parse::<u32>()
            {
                serialized.push(id);
            }
        }
    }
    serialized.sort_unstable();
    serialized.dedup();

    let invisible: Vec<u32> = serialized
        .iter()
        .copied()
        .filter(|id| !enumerable.contains(id))
        .collect();
    assert!(
        invisible.is_empty(),
        "every serialized revision (body, footnote, AND sectPrChange) must be \
         enumerable; invisible ids: {invisible:?} (serialized {serialized:?}, \
         enumerable {enumerable:?})"
    );
}

// ─── Header / footer / comment stories ──────────────────────────────────────

/// A minimal docx with a body plus a header part, a footer part, and a
/// comments part — each carrying ONE tracked revision in its serialized
/// markup. Same zip-building shape as `make_docx_with_notes`, extended with
/// the header/footer/comment plumbing (content-type overrides, document
/// relationships, and the sectPr header/footer references).
fn make_docx_with_header_footer_comment() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>
<w:p><w:commentRangeStart w:id="1"/><w:r><w:t>Body paragraph.</w:t></w:r><w:commentRangeEnd w:id="1"/><w:r><w:commentReference w:id="1"/></w:r></w:p>
<w:sectPr><w:headerReference w:type="default" r:id="rIdH1"/><w:footerReference w:type="default" r:id="rIdF1"/></w:sectPr>
</w:body></w:document>"#;
    // Header: a pending w:ins (id 201).
    let header_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:r><w:t xml:space="preserve">Confidential </w:t></w:r><w:ins w:id="201" w:author="H. Reviewer" w:date="2026-07-04T10:00:00Z"><w:r><w:t>v2</w:t></w:r></w:ins></w:p></w:hdr>"#;
    // Footer: a pending w:del (id 211).
    let footer_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:ftr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:r><w:t xml:space="preserve">Page footer </w:t></w:r><w:del w:id="211" w:author="F. Reviewer" w:date="2026-07-04T10:00:00Z"><w:r><w:delText>draft</w:delText></w:r></w:del></w:p></w:ftr>"#;
    // Comment 1: interior text carrying a pending w:ins (id 221).
    let comments_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:comment w:id="1" w:author="C. Reviewer" w:date="2026-07-04T10:00:00Z"><w:p><w:r><w:t xml:space="preserve">Needs </w:t></w:r><w:ins w:id="221" w:author="C. Reviewer" w:date="2026-07-04T10:00:00Z"><w:r><w:t>urgent </w:t></w:r></w:ins><w:r><w:t>review.</w:t></w:r></w:p></w:comment></w:comments>"#;

    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/><Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/><Override PartName="/word/comments.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdH1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/><Relationship Id="rIdF1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/><Relationship Id="rIdC1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="comments.xml"/></Relationships>"#;

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
        zip.start_file("word/footer1.xml", opts).unwrap();
        zip.write_all(footer_xml.as_bytes()).unwrap();
        zip.start_file("word/comments.xml", opts).unwrap();
        zip.write_all(comments_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

#[test]
fn header_story_revisions_are_enumerable_with_header_location() {
    let doc = Document::parse(&make_docx_with_header_footer_comment())
        .expect("parse a doc with a tracked header revision");
    let records = enumerate_revisions(&doc.snapshot().canonical);
    let header_records: Vec<_> = records
        .iter()
        .filter(|r| matches!(&r.location, StoryScope::Header { part_path, .. } if part_path == "header1.xml"))
        .collect();
    assert_eq!(
        header_records.len(),
        1,
        "the header's w:ins must be enumerated: {records:?}"
    );
    let r = header_records[0];
    assert_eq!(r.revision_id, 201);
    assert_eq!(r.kind, RevisionKind::Insert);
    assert_eq!(r.author.as_deref(), Some("H. Reviewer"));
    assert!(
        r.excerpt.contains("v2"),
        "excerpt should carry the inserted text, got {:?}",
        r.excerpt
    );
}

#[test]
fn footer_story_revisions_are_enumerable_with_footer_location() {
    let doc = Document::parse(&make_docx_with_header_footer_comment())
        .expect("parse a doc with a tracked footer revision");
    let records = enumerate_revisions(&doc.snapshot().canonical);
    let footer_records: Vec<_> = records
        .iter()
        .filter(|r| matches!(&r.location, StoryScope::Footer { part_path, .. } if part_path == "footer1.xml"))
        .collect();
    assert_eq!(
        footer_records.len(),
        1,
        "the footer's w:del must be enumerated: {records:?}"
    );
    let r = footer_records[0];
    assert_eq!(r.revision_id, 211);
    assert_eq!(r.kind, RevisionKind::Delete);
    assert_eq!(r.author.as_deref(), Some("F. Reviewer"));
}

#[test]
fn comment_interior_revisions_are_enumerable_with_comment_location() {
    let doc = Document::parse(&make_docx_with_header_footer_comment())
        .expect("parse a doc with a tracked revision inside a comment");
    let records = enumerate_revisions(&doc.snapshot().canonical);
    let comment_records: Vec<_> = records
        .iter()
        .filter(|r| matches!(&r.location, StoryScope::Comment { id } if id == "1"))
        .collect();
    assert_eq!(
        comment_records.len(),
        1,
        "the w:ins inside comment 1's text must be enumerated: {records:?}"
    );
    let r = comment_records[0];
    assert_eq!(r.revision_id, 221);
    assert_eq!(r.kind, RevisionKind::Insert);
    assert_eq!(r.author.as_deref(), Some("C. Reviewer"));
}
