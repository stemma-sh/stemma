//! Integration tests for the SECTIONS / PAGE-SETUP authoring verbs
//! (`EditStep::SetPageSetup` / `SetSectionType` / `InsertSectionBreak`, §17.6).
//!
//! Covered here:
//! - roundtrip: a full `w:sectPr` (page size, orientation, margins, columns,
//!   gutter) survives parse → serialize → parse;
//! - T1: tracked `SetPageSetup` — reject-all restores the prior sectPr,
//!   accept-all keeps the new layout (== Direct apply);
//! - no-op refusal: an empty patch is refused (`NoPageSetupRequested`).

use stemma::api::Document;
use stemma::domain::{NodeId, PageOrientation, RevisionInfo, SectionType};
use stemma::edit::{
    ColumnLayout, EditStep, EditTransaction, MaterializationMode, PageMargins, PageSetupPatch,
    SectionTarget, apply_transaction,
};
use stemma::runtime::ExportOptions;
use stemma::{accept_all, reject_all_with_styles};

/// Build a DOCX whose body `w:sectPr` carries a full set of page-setup
/// properties: landscape A4-ish page size, explicit margins + gutter, and a
/// two-column layout. Mirrors the fidelity-test fixture shape.
fn make_full_sectpr_docx() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:r><w:t>Body paragraph one.</w:t></w:r></w:p>
<w:p><w:r><w:t>Body paragraph two.</w:t></w:r></w:p>
<w:sectPr>
<w:pgSz w:w="16838" w:h="11906" w:orient="landscape"/>
<w:pgMar w:top="720" w:bottom="720" w:left="1440" w:right="1440" w:header="360" w:footer="360" w:gutter="180"/>
<w:cols w:num="2" w:space="708"/>
</w:sectPr>
</w:body></w:document>"#;

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

/// A full `w:sectPr` survives parse → serialize → parse with every modeled
/// page-setup property intact.
#[test]
fn full_sectpr_roundtrips() {
    let doc = Document::parse(&make_full_sectpr_docx()).expect("parse");
    let base = doc.snapshot().canonical.clone();
    let sp = base
        .body_section_properties
        .as_ref()
        .expect("body sectPr parsed");
    assert_eq!(sp.page_width, Some(16838));
    assert_eq!(sp.page_height, Some(11906));
    assert_eq!(sp.orientation, Some(PageOrientation::Landscape));
    assert_eq!(sp.margin_top, Some(720));
    assert_eq!(sp.margin_left, Some(1440));
    assert_eq!(sp.gutter, Some(180));
    assert_eq!(sp.columns, Some(2));
    assert_eq!(sp.column_space, Some(708));

    // Serialize → re-parse: the same properties must survive the byte trip.
    let bytes = doc.serialize(&ExportOptions::default()).expect("serialize");
    let reparsed = Document::parse(&bytes).expect("re-parse");
    let sp2 = reparsed
        .snapshot()
        .canonical
        .body_section_properties
        .as_ref()
        .expect("body sectPr survives roundtrip");
    assert_eq!(sp2.page_width, Some(16838), "page width survives");
    assert_eq!(sp2.page_height, Some(11906), "page height survives");
    assert_eq!(
        sp2.orientation,
        Some(PageOrientation::Landscape),
        "orientation survives"
    );
    assert_eq!(sp2.margin_top, Some(720), "top margin survives");
    assert_eq!(sp2.margin_right, Some(1440), "right margin survives");
    assert_eq!(sp2.gutter, Some(180), "gutter survives");
    assert_eq!(sp2.columns, Some(2), "column count survives");
    assert_eq!(sp2.column_space, Some(708), "column space survives");
}

/// T1 for tracked `SetPageSetup`: reject-all restores the prior sectPr,
/// accept-all keeps the new layout and equals a Direct apply.
#[test]
fn tracked_set_page_setup_reject_restores_accept_keeps() {
    let doc = Document::parse(&make_full_sectpr_docx()).expect("parse");
    let base = doc.snapshot().canonical.clone();
    let base_orientation = base
        .body_section_properties
        .as_ref()
        .unwrap()
        .orientation
        .clone();
    assert_eq!(base_orientation, Some(PageOrientation::Landscape));

    // Flip to portrait + change margins, tracked.
    let steps = vec![EditStep::SetPageSetup {
        target: SectionTarget::Body,
        patch: PageSetupPatch {
            orientation: Some(PageOrientation::Portrait),
            margins: Some(PageMargins {
                top: 1000,
                bottom: 1000,
                left: 1000,
                right: 1000,
                header: 500,
                footer: 500,
            }),
            ..Default::default()
        },
        semantic_hash: None,
        rationale: None,
    }];

    let tracked = apply_transaction(
        &base,
        &txn(steps.clone(), MaterializationMode::TrackedChange),
    )
    .expect("tracked apply")
    .0;
    assert!(
        tracked.body_section_property_change.is_some(),
        "tracked SetPageSetup records a w:sectPrChange"
    );
    assert_eq!(
        tracked
            .body_section_properties
            .as_ref()
            .unwrap()
            .orientation,
        Some(PageOrientation::Portrait),
        "new layout is portrait"
    );

    // reject-all == baseline.
    let mut rejected = tracked.clone();
    reject_all_with_styles(&mut rejected, None);
    let rejected_sp = rejected.body_section_properties.as_ref().unwrap();
    assert_eq!(
        rejected_sp.orientation, base_orientation,
        "reject restores the prior orientation"
    );
    assert_eq!(
        rejected_sp.margin_top,
        base.body_section_properties.as_ref().unwrap().margin_top,
        "reject restores the prior margins"
    );
    assert!(
        rejected.body_section_property_change.is_none(),
        "reject clears the sectPrChange"
    );

    // accept-all == direct apply.
    let mut accepted = tracked.clone();
    accept_all(&mut accepted);
    let direct = apply_transaction(&base, &txn(steps, MaterializationMode::Direct))
        .expect("direct apply")
        .0;
    assert_eq!(
        accepted
            .body_section_properties
            .as_ref()
            .unwrap()
            .orientation,
        direct.body_section_properties.as_ref().unwrap().orientation,
        "accept orientation equals direct"
    );
    assert_eq!(
        accepted
            .body_section_properties
            .as_ref()
            .unwrap()
            .margin_top,
        direct.body_section_properties.as_ref().unwrap().margin_top,
        "accept margins equal direct"
    );
    assert!(
        accepted.body_section_property_change.is_none(),
        "accept clears the sectPrChange"
    );
}

/// An empty patch is refused with `NoPageSetupRequested` — no empty
/// sectPrChange, no silent no-op.
#[test]
fn empty_patch_is_refused() {
    let doc = Document::parse(&make_full_sectpr_docx()).expect("parse");
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
    .expect_err("empty patch must be refused");
    assert!(
        matches!(err, stemma::edit::EditError::NoPageSetupRequested { .. }),
        "empty patch refused as NoPageSetupRequested, got {err:?}"
    );
}

/// A no-op patch (sets exactly the current values) is silently skipped: no
/// sectPrChange is authored.
#[test]
fn noop_patch_authors_no_change() {
    let doc = Document::parse(&make_full_sectpr_docx()).expect("parse");
    let base = doc.snapshot().canonical.clone();
    // The base is already landscape; re-asserting landscape is a no-op.
    let tracked = apply_transaction(
        &base,
        &txn(
            vec![EditStep::SetPageSetup {
                target: SectionTarget::Body,
                patch: PageSetupPatch {
                    orientation: Some(PageOrientation::Landscape),
                    ..Default::default()
                },
                semantic_hash: None,
                rationale: None,
            }],
            MaterializationMode::TrackedChange,
        ),
    )
    .expect("no-op apply ok")
    .0;
    assert!(
        tracked.body_section_property_change.is_none(),
        "a no-op SetPageSetup authors no sectPrChange"
    );
}

/// `SetSectionType` flips the section type without inventing page geometry.
#[test]
fn set_section_type_changes_only_the_type() {
    let doc = Document::parse(&make_full_sectpr_docx()).expect("parse");
    let base = doc.snapshot().canonical.clone();
    let result = apply_transaction(
        &base,
        &txn(
            vec![EditStep::SetSectionType {
                target: SectionTarget::Body,
                section_type: SectionType::Continuous,
                semantic_hash: None,
                rationale: None,
            }],
            MaterializationMode::TrackedChange,
        ),
    )
    .expect("set section type ok")
    .0;
    let sp = result.body_section_properties.as_ref().unwrap();
    assert_eq!(sp.section_type, Some(SectionType::Continuous));
    // Page geometry is untouched.
    assert_eq!(sp.page_width, Some(16838));
    assert_eq!(sp.columns, Some(2));
}

/// `InsertSectionBreak` attaches a fresh section break to a body paragraph that
/// did not previously own one.
#[test]
fn insert_section_break_attaches_to_paragraph() {
    let doc = Document::parse(&make_full_sectpr_docx()).expect("parse");
    let base = doc.snapshot().canonical.clone();
    let first_id = NodeId::from(doc.read().blocks[0].id.to_string().as_str());

    let result = apply_transaction(
        &base,
        &txn(
            vec![EditStep::InsertSectionBreak {
                anchor_block_id: first_id.clone(),
                section_type: SectionType::NextPage,
                properties: PageSetupPatch {
                    columns: Some(ColumnLayout {
                        count: 3,
                        space: 360,
                    }),
                    ..Default::default()
                },
                rationale: None,
            }],
            MaterializationMode::Direct,
        ),
    )
    .expect("insert section break ok")
    .0;

    let para = match &result.blocks[0].block {
        stemma::domain::BlockNode::Paragraph(p) => p,
        _ => panic!("first block is a paragraph"),
    };
    let sp = para
        .section_properties
        .as_ref()
        .expect("paragraph now owns a section break");
    assert_eq!(sp.section_type, Some(SectionType::NextPage));
    assert_eq!(sp.columns, Some(3));

    // Inserting a break on a paragraph that already owns one is refused.
    let err = apply_transaction(
        &result,
        &txn(
            vec![EditStep::InsertSectionBreak {
                anchor_block_id: first_id,
                section_type: SectionType::NextPage,
                properties: PageSetupPatch::default(),
                rationale: None,
            }],
            MaterializationMode::Direct,
        ),
    )
    .expect_err("clobbering an existing break is refused");
    assert!(
        matches!(
            err,
            stemma::edit::EditError::SectionAlreadyHasTrackedChange { .. }
        ),
        "got {err:?}"
    );
}

/// Build a minimal DOCX whose body `w:sectPr` is empty (`<w:sectPr/>`) and which
/// ships NO header/footer parts or relationships. This is the exact shape that
/// triggered the Word "needs repair" dialog: on import, blank default
/// header/footer stories are synthesized for the first section (§17.10.5), and a
/// tracked `SetPageSetup` records the prior sectPr in a `w:sectPrChange`.
fn make_empty_sectpr_docx() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Hello.</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;
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

/// Domain rule (ECMA-376 §17.10 + Part 2 OPC): every `r:id` emitted into the
/// package — including those inside a `w:sectPrChange`'s previous `w:sectPr` —
/// must resolve to a relationship registered in `word/_rels/document.xml.rels`.
/// A dangling reference is invalid OOXML and makes Word demand repair.
///
/// Regression: a tracked `SetPageSetup` rebuilds the body sectPr and records the
/// prior state in a `w:sectPrChange`. The synthesized blank header/footer refs
/// (added on import for the first section) were emitted into that snapshot with
/// the bare story `part_path` as `r:id` (a placeholder, never a registered rId),
/// leaving a dangling reference. This asserts the serialized output carries ZERO
/// dangling relationship references.
#[test]
fn tracked_set_page_setup_emits_no_dangling_relationship() {
    let doc = Document::parse(&make_empty_sectpr_docx()).expect("parse");

    let steps = vec![EditStep::SetPageSetup {
        target: SectionTarget::Body,
        patch: PageSetupPatch {
            margins: Some(PageMargins {
                top: 1000,
                bottom: 1000,
                left: 1000,
                right: 1000,
                header: 720,
                footer: 720,
            }),
            ..Default::default()
        },
        semantic_hash: None,
        rationale: None,
    }];

    let edited = doc
        .apply(&txn(steps, MaterializationMode::TrackedChange))
        .expect("tracked SetPageSetup applies");
    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize tracked redline");

    let validation = stemma::docx_validate::validate_docx(&bytes);
    let dangling: Vec<_> = validation
        .findings
        .iter()
        .filter(|f| f.rule_id == "I-REL-001")
        .collect();
    assert!(
        dangling.is_empty(),
        "tracked SetPageSetup serialized {} dangling relationship reference(s): {:#?}",
        dangling.len(),
        dangling
    );
}
