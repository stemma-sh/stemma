//! Spec-compliance tests for the sections / page-setup + headers/footers verbs.
//!
//! These encode behavioral constraints from ECMA-376 / ISO 29500:
//! - §17.6.22 ST_SectionMark: a `Continuous` section inherits page properties
//!   from the PRECEDING section (a known drafting-error workaround the importer
//!   implements; this test guards against a regression to following-section
//!   inheritance);
//! - the page-setup grammar refuses an empty patch (no silent no-op);
//! - the v4 wire edge rejects an unknown `section_type` / `orientation` token —
//!   NEVER mapping it to a default;
//! - §17.6.18 `w:titlePg` gates a distinct first-page header;
//! - §17.15.1.35 `w:evenAndOddHeaders` round-trips present / absent honestly.

use stemma::api::Document;
use stemma::domain::{PageOrientation, RevisionInfo, SectionType};
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

/// A two-section doc: the first paragraph carries a NextPage section break with
/// an explicit landscape `pgSz`; the body section is `Continuous` and OMITS
/// `pgSz`. Per §17.6.22 the continuous body section must inherit the preceding
/// section's page size.
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
            author: Some("Tester".to_string()),
            date: Some("2026-06-01T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

/// §17.6.22: a Continuous section inherits page properties from the PRECEDING
/// section, not the following one.
#[test]
fn continuous_section_inherits_from_preceding() {
    let doc = Document::parse(&make_two_section_docx()).expect("parse");
    let canon = doc.snapshot().canonical.clone();

    let body_sp = canon
        .body_section_properties
        .as_ref()
        .expect("body sectPr present");
    assert_eq!(body_sp.section_type, Some(SectionType::Continuous));
    // The continuous body section omits pgSz; it must have inherited the
    // PRECEDING (first paragraph's) landscape 16838x11906 page size.
    assert_eq!(
        body_sp.page_width,
        Some(16838),
        "continuous section inherits preceding page width (§17.6.22)"
    );
    assert_eq!(
        body_sp.page_height,
        Some(11906),
        "continuous section inherits preceding page height"
    );
    assert_eq!(
        body_sp.orientation,
        Some(PageOrientation::Landscape),
        "continuous section inherits preceding orientation"
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
