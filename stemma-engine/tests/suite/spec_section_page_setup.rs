//! Spec-compliance tests for the sections / page-setup + headers/footers verbs.
//!
//! These encode behavioral constraints from ECMA-376 / ISO 29500:
//! - §17.6.22 / §17.6.11: a `Continuous` section that omits page properties is
//!   stored and written as authored. Real Word reads the omitted values as its
//!   defaults (letter portrait, 1440 margins, 720 header and footer), not as the
//!   preceding section's, so copying a neighbour's values in would change the
//!   layout on save;
//! - the page-setup grammar refuses an empty patch (no silent no-op);
//! - the v4 wire edge rejects an unknown `section_type` / `orientation` token —
//!   NEVER mapping it to a default;
//! - §17.6.18 `w:titlePg` gates a distinct first-page header;
//! - §17.15.1.35 `w:evenAndOddHeaders` round-trips present / absent honestly.

use stemma::api::Document;
use stemma::domain::{RevisionInfo, SectionType};
use stemma::edit::{
    EditStep, EditTransaction, MaterializationMode, PageSetupPatch, SectionTarget,
    apply_transaction,
};
use stemma::edit_v4::{AdapterError, parse_transaction};
use stemma::runtime::ExportOptions;

fn zip_docx(parts: &[(&str, &str)]) -> Vec<u8> {
    use std::io::Write;
    use zip::write::FileOptions;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        for (name, data) in parts {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
const RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
const DOC_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;

/// `word/document.xml` of a serialized package.
fn document_xml(docx: &[u8]) -> String {
    let archive = stemma::docx::DocxArchive::read(docx).expect("read archive");
    String::from_utf8(
        archive
            .get("word/document.xml")
            .expect("document.xml")
            .to_vec(),
    )
    .expect("utf-8")
}

/// A two-section doc: the first paragraph carries a NextPage section break with
/// an explicit landscape `pgSz`; the body section is `Continuous` and OMITS
/// `pgSz`.
fn make_two_section_docx() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:pPr><w:sectPr><w:pgSz w:w="16838" w:h="11906" w:orient="landscape"/><w:type w:val="nextPage"/></w:sectPr></w:pPr><w:r><w:t>First section.</w:t></w:r></w:p>
<w:p><w:r><w:t>Second section.</w:t></w:r></w:p>
<w:sectPr><w:type w:val="continuous"/></w:sectPr>
</w:body></w:document>"#;
    zip_docx(&[
        ("[Content_Types].xml", CONTENT_TYPES),
        ("_rels/.rels", RELS),
        ("word/_rels/document.xml.rels", DOC_RELS),
        ("word/document.xml", document_xml),
    ])
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

/// A continuous section that omits its page size keeps it omitted. Real Word
/// opens this shape and saves the continuous section as letter portrait
/// (12240 x 15840), its default, not the preceding landscape page, so the
/// stored model must not take the neighbour's values and a save must not write
/// them.
#[test]
fn continuous_section_keeps_its_omitted_page_size() {
    let doc = Document::parse(&make_two_section_docx()).expect("parse");
    let canon = doc.snapshot().canonical.clone();

    let body_sp = canon
        .body_section_properties
        .as_ref()
        .expect("body sectPr present");
    assert_eq!(body_sp.section_type, Some(SectionType::Continuous));
    assert_eq!(body_sp.page_width, None, "no page width was authored");
    assert_eq!(body_sp.page_height, None, "no page height was authored");
    assert_eq!(body_sp.orientation, None, "no orientation was authored");

    let xml = document_xml(&doc.serialize(&ExportOptions::default()).expect("serialize"));
    let body_sect = &xml[xml.rfind("<w:sectPr").expect("body sectPr")..];
    assert!(
        !body_sect.contains("pgSz"),
        "the continuous section is written without a page size: {body_sect}"
    );
}

/// The same for margins: a continuous section whose `w:pgMar` names only `top`
/// keeps only `top`. Word reads the missing header and footer distances as 720
/// and the missing margins as 1440, not the preceding section's 0 and 766.
#[test]
fn continuous_section_keeps_its_omitted_margins() {
    let authored = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:pPr><w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="2880" w:right="2880" w:bottom="2880" w:left="2880" w:header="0" w:footer="766" w:gutter="0"/></w:sectPr></w:pPr><w:r><w:t>First section.</w:t></w:r></w:p>
<w:p><w:r><w:t>Second section.</w:t></w:r></w:p>
<w:sectPr><w:type w:val="continuous"/><w:pgMar w:top="500"/></w:sectPr>
</w:body></w:document>"#;
    let doc = Document::parse(&zip_docx(&[
        ("[Content_Types].xml", CONTENT_TYPES),
        ("_rels/.rels", RELS),
        ("word/_rels/document.xml.rels", DOC_RELS),
        ("word/document.xml", authored),
    ]))
    .expect("parse");
    let canon = doc.snapshot().canonical.clone();
    let body_sp = canon
        .body_section_properties
        .as_ref()
        .expect("body sectPr present");
    assert_eq!(body_sp.margin_top, Some(500));
    assert_eq!(
        (
            body_sp.margin_left,
            body_sp.margin_right,
            body_sp.margin_bottom,
            body_sp.header_distance,
            body_sp.footer_distance,
        ),
        (None, None, None, None, None),
        "only the authored top margin is stored"
    );

    let xml = document_xml(&doc.serialize(&ExportOptions::default()).expect("serialize"));
    let body_sect = &xml[xml.rfind("<w:sectPr").expect("body sectPr")..];
    let pg_mar = &body_sect[body_sect.find("<w:pgMar").expect("pgMar")..];
    let pg_mar = &pg_mar[..pg_mar.find("/>").expect("pgMar end")];
    assert_eq!(
        pg_mar.trim_end(),
        r#"<w:pgMar w:top="500""#,
        "only top is written: {pg_mar}"
    );
}

/// The page-setup grammar refuses an empty patch (no silent no-op).
#[test]
fn empty_patch_refused() {
    let doc = Document::parse(&make_two_section_docx()).expect("parse");
    let base = doc.snapshot().canonical.clone();
    let err = apply_transaction(
        &base,
        &txn(
            vec![EditStep::SetPageSetup {
                target: SectionTarget::Body,
                patch: PageSetupPatch::default(),
                semantic_hash: None,
                rationale: None,
            }],
            MaterializationMode::TrackedChange,
        ),
    )
    .expect_err("empty patch refused");
    assert!(matches!(
        err,
        stemma::edit::EditError::NoPageSetupRequested { .. }
    ));
}

/// The v4 wire edge rejects an unknown `section_type` token — never defaulting.
#[test]
fn unknown_section_type_rejected_at_wire() {
    let json = r#"
    {
      "ops": [{
        "op": "set_section_type",
        "target": { "section": "body" },
        "section_type": "diagonal_page"
      }],
      "revision": { "author": "Tester" }
    }"#;
    let parsed = parse_transaction(json).expect("schema accepts (token checked at adapter)");
    let err = parsed
        .into_edit_transaction()
        .expect_err("unknown section_type must be rejected, never defaulted");
    assert!(
        matches!(err, AdapterError::UnknownSectionType { ref value, .. } if value == "diagonal_page"),
        "got {err:?}"
    );
}

/// The v4 wire edge rejects an unknown `orientation` token — never defaulting.
#[test]
fn unknown_orientation_rejected_at_wire() {
    let json = r#"
    {
      "ops": [{
        "op": "set_page_setup",
        "target": { "section": "body" },
        "orientation": "sideways"
      }],
      "revision": { "author": "Tester" }
    }"#;
    let parsed = parse_transaction(json).expect("schema accepts (token checked at adapter)");
    let err = parsed
        .into_edit_transaction()
        .expect_err("unknown orientation must be rejected, never coerced");
    assert!(
        matches!(err, AdapterError::UnknownOrientation { ref value, .. } if value == "sideways"),
        "got {err:?}"
    );
}

/// §17.6.18: titlePg on the section is what gates a distinct first-page header.
/// Setting it via `SetHeaderFooterMode` records the flag on the section.
#[test]
fn title_pg_gates_first_page_header() {
    let doc = Document::parse(&make_two_section_docx()).expect("parse");
    let base = doc.snapshot().canonical.clone();
    // No titlePg by default.
    assert_eq!(
        base.body_section_properties.as_ref().unwrap().title_page,
        None,
        "no titlePg by default"
    );
    let result = apply_transaction(
        &base,
        &txn(
            vec![EditStep::SetHeaderFooterMode {
                title_page: Some(true),
                even_and_odd: None,
                link: None,
                rationale: None,
            }],
            MaterializationMode::Direct,
        ),
    )
    .expect("set titlePg")
    .0;
    assert_eq!(
        result.body_section_properties.as_ref().unwrap().title_page,
        Some(true),
        "titlePg gates the first-page header (§17.6.18)"
    );
}

/// §17.15.1.35: evenAndOddHeaders round-trips present / absent honestly.
/// Setting it to `Some(true)` and serializing must produce a settings.xml whose
/// re-parse reads the flag as on; a document that never set it stays absent.
#[test]
fn even_and_odd_headers_round_trips_present_and_absent() {
    // Absent by default: a doc that never toggled the setting reads None.
    let doc = Document::parse(&make_two_section_docx()).expect("parse");
    assert_eq!(
        doc.snapshot().canonical.even_and_odd_headers,
        None,
        "no evenAndOddHeaders setting → None (absent, NOT off)"
    );

    // Toggle it on, serialize, re-parse: the flag must survive as present-on.
    let toggled = doc
        .apply(&txn(
            vec![EditStep::SetHeaderFooterMode {
                title_page: None,
                even_and_odd: Some(true),
                link: None,
                rationale: None,
            }],
            MaterializationMode::Direct,
        ))
        .expect("toggle even_and_odd");
    assert_eq!(
        toggled.snapshot().canonical.even_and_odd_headers,
        Some(true)
    );

    let bytes = toggled
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let reparsed = Document::parse(&bytes).expect("re-parse");
    assert_eq!(
        reparsed.snapshot().canonical.even_and_odd_headers,
        Some(true),
        "evenAndOddHeaders present-on survives serialize → parse"
    );
}
