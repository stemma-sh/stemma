//! The `w:sectPrChange` previous snapshot is never empty.
//!
//! WORD RULE (bisected against live Word): Word
//! registers NO revision for a `sectPrChange` whose previous `<w:sectPr/>`
//! snapshot has no children — the tracked layout change is invisible in the
//! review pane and unrejectable (reject silently keeps the new layout). Any
//! non-empty snapshot registers. Word's own writer materializes the previous
//! EFFECTIVE page geometry into the snapshot; we mirror it when the previous
//! section had no authored properties.

use std::io::Write;

use stemma::ExportOptions;
use stemma::RevisionInfo;
use stemma::api::Document;
use stemma::domain::{HeaderFooterKind, PageOrientation};
use stemma::edit::*;
use zip::write::FileOptions;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn make_docx(sect_pr: &str) -> Vec<u8> {
    let doc = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W_NS}"><w:body><w:p><w:r><w:t>Body paragraph.</w:t></w:r></w:p>{sect_pr}</w:body></w:document>"#
    );
    let ct = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let o: FileOptions = FileOptions::default();
        zip.start_file("[Content_Types].xml", o).unwrap();
        zip.write_all(ct.as_bytes()).unwrap();
        zip.start_file("_rels/.rels", o).unwrap();
        zip.write_all(rels.as_bytes()).unwrap();
        zip.start_file("word/_rels/document.xml.rels", o).unwrap();
        zip.write_all(br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#).unwrap();
        zip.start_file("word/document.xml", o).unwrap();
        zip.write_all(doc.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

fn tracked_txn(steps: Vec<EditStep>) -> EditTransaction {
    EditTransaction {
        steps,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 1,
            author: Some("fid".into()),
            date: Some("2026-07-02T00:00:00Z".into()),
            apply_op_id: None,
        },
        summary: None,
    }
}

fn serialized_doc_xml(base: &[u8], steps: Vec<EditStep>) -> String {
    let out = Document::parse(base)
        .expect("parse")
        .apply(&tracked_txn(steps))
        .expect("apply")
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let a = stemma::docx::DocxArchive::read(&out).expect("archive");
    String::from_utf8(a.get("word/document.xml").unwrap().to_vec()).unwrap()
}

fn sect_pr_change_previous(xml: &str) -> String {
    let i = xml.find("<w:sectPrChange").expect("sectPrChange present");
    let j = xml[i..].find("</w:sectPrChange>").expect("closed") + i;
    xml[i..j].to_string()
}

fn page_setup_step() -> EditStep {
    EditStep::SetPageSetup {
        target: SectionTarget::Body,
        patch: PageSetupPatch {
            orientation: Some(PageOrientation::Landscape),
            margins: Some(PageMargins {
                top: 720,
                bottom: 720,
                left: 1440,
                right: 1440,
                header: 360,
                footer: 360,
            }),
            ..Default::default()
        },
        semantic_hash: None,
        rationale: None,
    }
}

/// Previous section had no authored properties: the snapshot materializes
/// Word's default page geometry instead of an empty `<w:sectPr/>`.
#[test]
fn empty_previous_sect_pr_materializes_default_geometry() {
    let xml = serialized_doc_xml(&make_docx("<w:sectPr/>"), vec![page_setup_step()]);
    let prev = sect_pr_change_previous(&xml);
    assert!(
        prev.contains("<w:pgSz") && prev.contains("<w:pgMar"),
        "an empty previous snapshot is invisible to Word; it must carry the \
         default page geometry. sectPrChange: {prev}"
    );
    assert!(
        prev.contains(r#"w:w="12240""#) && prev.contains(r#"w:h="15840""#),
        "the materialized snapshot is Word's Letter-portrait default. \
         sectPrChange: {prev}"
    );
}

/// Previous section HAD authored properties: the snapshot stays the faithful
/// authored state — no default materialization on top of it.
#[test]
fn authored_previous_sect_pr_stays_verbatim() {
    let base = make_docx(
        r#"<w:sectPr><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1134" w:right="1134" w:bottom="1134" w:left="1134" w:header="708" w:footer="708" w:gutter="0"/></w:sectPr>"#,
    );
    let xml = serialized_doc_xml(&base, vec![page_setup_step()]);
    let prev = sect_pr_change_previous(&xml);
    assert!(
        prev.contains(r#"w:w="11906""#) && prev.contains(r#"w:top="1134""#),
        "authored previous state must snapshot verbatim (A4 dims, not Letter \
         defaults). sectPrChange: {prev}"
    );
    assert!(
        !prev.contains("12240"),
        "no default materialization on an authored snapshot. sectPrChange: {prev}"
    );
}

/// CreateHeader rides the same snapshot mechanism (its sectPrChange records
/// the pre-create section): same non-empty guarantee.
#[test]
fn create_header_previous_snapshot_nonempty() {
    let xml = serialized_doc_xml(
        &make_docx("<w:sectPr/>"),
        vec![EditStep::CreateHeader {
            kind: HeaderFooterKind::Even,
            rationale: None,
        }],
    );
    let prev = sect_pr_change_previous(&xml);
    assert!(
        prev.contains("<w:pgSz") && prev.contains("<w:pgMar"),
        "create_header's sectPrChange snapshot must be Word-visible too. \
         sectPrChange: {prev}"
    );
}

/// WORD RULE (verified against live Word): Word never registers a reference-only
/// `sectPrChange` — its own writer adds header refs UNTRACKED and tracks the
/// story CONTENT. So a tracked create_header's blank paragraph carries an
/// inserted paragraph mark: the Word-visible face of the creation.
#[test]
fn tracked_create_header_story_paragraph_is_inserted() {
    let out = Document::parse(&make_docx("<w:sectPr/>"))
        .expect("parse")
        .apply(&tracked_txn(vec![EditStep::CreateHeader {
            kind: HeaderFooterKind::Even,
            rationale: None,
        }]))
        .expect("apply")
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let a = stemma::docx::DocxArchive::read(&out).expect("archive");
    let hdr =
        String::from_utf8(a.get("word/header1.xml").expect("header part").to_vec()).expect("utf-8");
    assert!(
        hdr.contains("<w:pPr><w:rPr><w:ins ") || hdr.contains("<w:rPr><w:ins "),
        "the blank story paragraph must carry an inserted paragraph mark \
         (the only header-creation revision Word can see); header1.xml: {hdr}"
    );
}

/// `w:orient` is descriptive — the page renders from w/h. "Make it landscape"
/// must swap portrait-shaped explicit dims (Word's own writer does), and on a
/// section with no authored pgSz it must materialize dimensions or the flag
/// changes nothing on screen.
#[test]
fn orientation_change_reconciles_page_dimensions() {
    // Explicit portrait dims: landscape swaps them.
    let base = make_docx(r#"<w:sectPr><w:pgSz w:w="11906" w:h="16838"/></w:sectPr>"#);
    let xml = serialized_doc_xml(
        &base,
        vec![EditStep::SetPageSetup {
            target: SectionTarget::Body,
            patch: PageSetupPatch {
                orientation: Some(PageOrientation::Landscape),
                ..Default::default()
            },
            semantic_hash: None,
            rationale: None,
        }],
    );
    // The LIVE pgSz is the first one inside the live sectPr (the snapshot's
    // copy sits later, inside sectPrChange).
    let i = xml.find("<w:sectPr>").unwrap();
    let live = &xml[i..];
    let live = &live[live.find("<w:pgSz").unwrap()..];
    let live = &live[..live.find("/>").unwrap()];
    assert!(
        live.contains(r#"w:w="16838""#) && live.contains(r#"w:h="11906""#),
        "landscape must swap the portrait A4 dims; live pgSz: {live}"
    );

    // No authored pgSz: landscape materializes default dims, swapped.
    let xml = serialized_doc_xml(
        &make_docx("<w:sectPr/>"),
        vec![EditStep::SetPageSetup {
            target: SectionTarget::Body,
            patch: PageSetupPatch {
                orientation: Some(PageOrientation::Landscape),
                ..Default::default()
            },
            semantic_hash: None,
            rationale: None,
        }],
    );
    let i = xml.find("<w:sectPr>").unwrap();
    let live = &xml[i..];
    let live = &live[live.find("<w:pgSz").unwrap()..];
    let live = &live[..live.find("/>").unwrap()];
    assert!(
        live.contains(r#"w:w="15840""#) && live.contains(r#"w:h="12240""#),
        "landscape on a default section materializes Letter dims, swapped; \
         live pgSz: {live}"
    );

    // Idempotence: dims already landscape-shaped are left alone.
    let base =
        make_docx(r#"<w:sectPr><w:pgSz w:w="16838" w:h="11906" w:orient="landscape"/></w:sectPr>"#);
    let xml = serialized_doc_xml(
        &base,
        vec![EditStep::SetPageSetup {
            target: SectionTarget::Body,
            patch: PageSetupPatch {
                orientation: Some(PageOrientation::Landscape),
                margins: Some(PageMargins {
                    top: 720,
                    bottom: 720,
                    left: 1440,
                    right: 1440,
                    header: 360,
                    footer: 360,
                }),
                ..Default::default()
            },
            semantic_hash: None,
            rationale: None,
        }],
    );
    let i = xml.find("<w:sectPr>").unwrap();
    let live = &xml[i..];
    let live = &live[live.find("<w:pgSz").unwrap()..];
    let live = &live[..live.find("/>").unwrap()];
    assert!(
        live.contains(r#"w:w="16838""#) && live.contains(r#"w:h="11906""#),
        "already-landscape dims stay verbatim; live pgSz: {live}"
    );
}
