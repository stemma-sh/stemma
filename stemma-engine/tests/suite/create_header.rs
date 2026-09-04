//! Integration tests for the `CreateHeader` / `CreateFooter` authoring verbs
//! (`EditStep::CreateHeader` / `CreateFooter`, §17.10.2 / §17.13.5.32).
//!
//! These author a NET-NEW, blank header/footer story PLUS a body-section
//! reference, tracked as a `w:sectPrChange`. The contract under test:
//!   - accept-all == the doc WITH the new story + section reference (== Direct);
//!   - reject-all retains the unrevisionable physical story binding, but the
//!     story is blank and the original synthesized-blank Default slot remains;
//!   - both projections are validator-clean;
//!   - the new story serializes as a valid part and survives a reparse;
//!   - fail-loud refusals: a kind already referenced on the section (incl. the
//!     synthesized Default), and a stacked tracked sectPrChange.
//!
//! The verb authors the genuinely net-new `Even` kind: the importer always
//! materializes a blank `Default` header/footer reference per §17.10.2, so a
//! `Default` create is refused in favor of `EditHeader`.

use std::collections::HashSet;

use stemma::api::Document;
use stemma::domain::{BlockNode, HeaderFooterKind, InlineNode, RevisionInfo, TrackedBlock};
use stemma::edit::{
    EditStep, EditTransaction, MaterializationMode, PageSetupPatch, SectionTarget,
    apply_transaction,
};
use stemma::runtime::ExportOptions;
use stemma::{Resolution, accept_all, reject_all_with_styles};

/// A plain two-paragraph DOCX with an empty body `w:sectPr` (no header/footer).
fn make_plain_docx() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Body paragraph one.</w:t></w:r></w:p><w:p><w:r><w:t>Body paragraph two.</w:t></w:r></w:p><w:sectPr><w:pgSz w:w="12240" w:h="15840"/></w:sectPr></w:body></w:document>"#;
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
            identity: 0,
            author: Some("Tester".to_string()),
            date: Some("2026-06-01T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

/// Assert a serialized package carries no validator ERROR-severity finding.
fn assert_validator_clean(label: &str, bytes: &[u8]) {
    let report = stemma::docx_validate::validate_docx(bytes);
    let errors: Vec<_> = report
        .findings
        .iter()
        .filter(|f| f.severity == stemma::docx_validate::ValidationSeverity::Error)
        .collect();
    assert!(
        errors.is_empty(),
        "[{label}] validator must be clean, got {} error(s): {:#?}",
        errors.len(),
        errors
    );
}

/// True when the body section references a header of `kind`.
fn has_header_ref(doc: &Document, kind: &HeaderFooterKind) -> bool {
    doc.snapshot()
        .canonical
        .body_section_properties
        .as_ref()
        .map(|sp| sp.header_refs.iter().any(|r| &r.kind == kind))
        .unwrap_or(false)
}

fn has_footer_ref(doc: &Document, kind: &HeaderFooterKind) -> bool {
    doc.snapshot()
        .canonical
        .body_section_properties
        .as_ref()
        .is_some_and(|section| {
            section
                .footer_refs
                .iter()
                .any(|reference| &reference.kind == kind)
        })
}

fn story_blocks_are_blank(blocks: &[TrackedBlock]) -> bool {
    blocks.iter().all(|block| match &block.block {
        BlockNode::Paragraph(paragraph) => paragraph.segments.iter().all(|segment| {
            segment.inlines.iter().all(|inline| match inline {
                InlineNode::Text(text) => text.text.is_empty(),
                _ => false,
            })
        }),
        _ => false,
    })
}

/// The tracked redline of a `CreateHeader { Even }` validates clean, accept-all
/// keeps the new Even header story + reference, and reject-all keeps only its
/// inert physical binding while preserving the synthesized Default header.
#[test]
fn create_header_tracked_redline_validates_and_projects() {
    let doc = Document::parse(&make_plain_docx()).expect("parse");
    let base_header_count = doc.snapshot().canonical.headers.len();
    assert!(
        !has_header_ref(&doc, &HeaderFooterKind::Even),
        "base must have no Even header reference"
    );

    let edited = doc
        .apply(&txn(
            vec![EditStep::CreateHeader {
                kind: HeaderFooterKind::Even,
                rationale: None,
            }],
            MaterializationMode::TrackedChange,
        ))
        .expect("CreateHeader applies");

    // The tracked redline itself validates clean (no dangling relationship; the
    // synthesized header part + content-type are present and consistent), and its
    // rebuilt sectPr carries the new even `w:headerReference` plus the recording
    // `w:sectPrChange`. This is the artifact Word consumes; accept/reject happen
    // in the consumer (the Word-oracle conformance case), so we assert the redline
    // is correct here and the IR projections below.
    let redline = edited
        .serialize(&ExportOptions::default())
        .expect("serialize redline");
    assert_validator_clean("create_header tracked redline", &redline);
    let redline_archive = stemma::docx::DocxArchive::read(&redline).expect("read redline");
    let redline_doc = String::from_utf8_lossy(
        redline_archive
            .get("word/document.xml")
            .expect("document.xml present"),
    )
    .into_owned();
    assert!(
        redline_doc.contains(r#"w:type="even""#) && redline_doc.contains("w:headerReference"),
        "the tracked redline sectPr carries the new even headerReference"
    );
    assert!(
        redline_doc.contains("w:sectPrChange"),
        "the new reference is recorded as a tracked w:sectPrChange"
    );
    // The new story serializes as a real (non synthesized-blank) header part.
    assert!(
        redline_archive
            .list()
            .into_iter()
            .any(|n| n.starts_with("word/header") && n.ends_with(".xml")),
        "the new header story serializes as a part"
    );

    // accept-all (IR projection): the new Even reference + story are kept.
    let accepted = edited.project(Resolution::AcceptAll).expect("accept all");
    assert!(
        has_header_ref(&accepted, &HeaderFooterKind::Even),
        "accept keeps the new Even header reference"
    );
    assert_eq!(
        accepted.snapshot().canonical.headers.len(),
        base_header_count + 1,
        "accept keeps exactly one net-new header story"
    );

    // Header/footer bindings are physical, not revision-switchable. Reject
    // retains the blank Even part/reference while clearing the tracked history.
    let rejected = edited.project(Resolution::RejectAll).expect("reject all");
    assert!(
        has_header_ref(&rejected, &HeaderFooterKind::Even),
        "reject retains the physical Even header reference"
    );
    assert_eq!(
        rejected.snapshot().canonical.headers.len(),
        base_header_count + 1,
        "reject retains exactly one inert physical Even header story"
    );
    let retained = rejected
        .snapshot()
        .canonical
        .headers
        .iter()
        .find(|story| story.kind == HeaderFooterKind::Even)
        .expect("retained Even header");
    assert!(
        story_blocks_are_blank(&retained.blocks),
        "the retained Even header has no active content"
    );
    assert!(
        rejected
            .snapshot()
            .canonical
            .body_section_property_change
            .is_none(),
        "reject clears the tracked sectPrChange"
    );
}

/// The new Even header story serializes as a valid part and survives a reparse.
///
/// The reference reaches the output through the TRACKED sectPr rebuild path (the
/// modeled section is emitted when `body_section_property_change` is `Some` — the
/// same path the sibling page-setup verb uses). So we round-trip the tracked
/// redline: serialize it, read it back, and confirm the new Even header story +
/// `w:headerReference` are present (the `w:sectPrChange` carries the prior state
/// so a later reject still works).
#[test]
fn create_header_part_survives_reparse() {
    let doc = Document::parse(&make_plain_docx()).expect("parse");
    let edited = doc
        .apply(&txn(
            vec![EditStep::CreateHeader {
                kind: HeaderFooterKind::Even,
                rationale: None,
            }],
            MaterializationMode::TrackedChange,
        ))
        .expect("CreateHeader applies");
    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    assert_validator_clean("create_header tracked", &bytes);

    let reparsed = Document::parse(&bytes).expect("reparse");
    assert!(
        reparsed
            .snapshot()
            .canonical
            .headers
            .iter()
            .any(|h| h.kind == HeaderFooterKind::Even),
        "the new Even header story round-trips through serialize → parse"
    );
    assert!(
        has_header_ref(&reparsed, &HeaderFooterKind::Even),
        "the body Even header reference round-trips"
    );
}

/// `CreateHeader` for a kind already referenced on the section is refused — no
/// silent duplicate. The importer always references a Default header, so a
/// `Default` create is refused outright.
#[test]
fn create_header_duplicate_default_is_refused() {
    let doc = Document::parse(&make_plain_docx()).expect("parse");
    let base = doc.snapshot().canonical.clone();
    assert!(
        has_header_ref(&doc, &HeaderFooterKind::Default),
        "the importer materializes a Default header reference"
    );

    let err = apply_transaction(
        &base,
        &txn(
            vec![EditStep::CreateHeader {
                kind: HeaderFooterKind::Default,
                rationale: None,
            }],
            MaterializationMode::TrackedChange,
        ),
    )
    .expect_err("duplicate Default header must be refused");
    assert!(
        matches!(
            err,
            stemma::edit::EditError::HeaderFooterAlreadyExists {
                is_header: true,
                ..
            }
        ),
        "duplicate refused as HeaderFooterAlreadyExists, got {err:?}"
    );
}

/// Creating the same net-new kind twice is refused the second time: the first
/// (Direct) create adds the Even reference; the second create sees it and
/// refuses.
#[test]
fn create_header_duplicate_even_is_refused() {
    let doc = Document::parse(&make_plain_docx()).expect("parse");
    let base = doc.snapshot().canonical.clone();
    let after_first = apply_transaction(
        &base,
        &txn(
            vec![EditStep::CreateHeader {
                kind: HeaderFooterKind::Even,
                rationale: None,
            }],
            MaterializationMode::Direct,
        ),
    )
    .expect("first CreateHeader applies")
    .0;

    let err = apply_transaction(
        &after_first,
        &txn(
            vec![EditStep::CreateHeader {
                kind: HeaderFooterKind::Even,
                rationale: None,
            }],
            MaterializationMode::TrackedChange,
        ),
    )
    .expect_err("duplicate Even header must be refused");
    assert!(
        matches!(
            err,
            stemma::edit::EditError::HeaderFooterAlreadyExists {
                is_header: true,
                ..
            }
        ),
        "duplicate refused as HeaderFooterAlreadyExists, got {err:?}"
    );
}

/// `CreateHeader` is refused when the body section already carries a tracked
/// `w:sectPrChange` — the caller must accept/reject the pending change first.
#[test]
fn create_header_refuses_to_stack_sectprchange() {
    let doc = Document::parse(&make_plain_docx()).expect("parse");
    let base = doc.snapshot().canonical.clone();

    // Author a tracked page-setup change first (leaves a body sectPrChange).
    let with_change = apply_transaction(
        &base,
        &txn(
            vec![EditStep::SetPageSetup {
                target: SectionTarget::Body,
                patch: PageSetupPatch {
                    columns: Some(stemma::edit::ColumnLayout {
                        count: 2,
                        space: 720,
                    }),
                    ..Default::default()
                },
                semantic_hash: None,
                rationale: None,
            }],
            MaterializationMode::TrackedChange,
        ),
    )
    .expect("page-setup change applies")
    .0;
    assert!(with_change.body_section_property_change.is_some());

    let err = apply_transaction(
        &with_change,
        &txn(
            vec![EditStep::CreateHeader {
                kind: HeaderFooterKind::Even,
                rationale: None,
            }],
            MaterializationMode::TrackedChange,
        ),
    )
    .expect_err("CreateHeader on a section with a pending sectPrChange must be refused");
    assert!(
        matches!(
            err,
            stemma::edit::EditError::SectionAlreadyHasTrackedChange { .. }
        ),
        "refused as SectionAlreadyHasTrackedChange, got {err:?}"
    );
}

/// The footer twin: a tracked `CreateFooter { Even }` validates clean and
/// projects both ways (accept keeps the footer, reject leaves it blank).
#[test]
fn create_footer_tracked_redline_validates_and_projects() {
    let doc = Document::parse(&make_plain_docx()).expect("parse");
    let base_footer_count = doc.snapshot().canonical.footers.len();
    let edited = doc
        .apply(&txn(
            vec![EditStep::CreateFooter {
                kind: HeaderFooterKind::Even,
                rationale: None,
            }],
            MaterializationMode::TrackedChange,
        ))
        .expect("CreateFooter applies");
    let redline = edited
        .serialize(&ExportOptions::default())
        .expect("serialize redline");
    assert_validator_clean("create_footer tracked redline", &redline);

    let reopened = Document::parse(&redline).expect("reopen tracked footer creation");
    let identities: HashSet<u32> = stemma::enumerate_revisions(&reopened.snapshot().canonical)
        .into_iter()
        .map(|record| record.revision_id)
        .collect();
    assert_eq!(
        identities.len(),
        1,
        "the new footer story and its section reference are one selectable creation"
    );

    let accepted = reopened
        .project(Resolution::Selective {
            ids: identities.clone(),
            action: stemma::ResolveSelectionAction::Accept,
        })
        .expect("selectively accept footer creation");
    assert_eq!(
        accepted.snapshot().canonical.footers.len(),
        base_footer_count + 1,
        "accept keeps the net-new footer story"
    );

    let rejected = reopened
        .project(Resolution::Selective {
            ids: identities,
            action: stemma::ResolveSelectionAction::Reject,
        })
        .expect("selectively reject footer creation");
    assert_eq!(
        rejected.snapshot().canonical.footers.len(),
        base_footer_count + 1,
        "reject retains exactly one inert physical footer story"
    );
    assert!(
        has_footer_ref(&rejected, &HeaderFooterKind::Even),
        "reject retains the physical Even footer reference"
    );
    let retained = rejected
        .snapshot()
        .canonical
        .footers
        .iter()
        .find(|story| story.kind == HeaderFooterKind::Even)
        .expect("retained Even footer");
    assert!(
        story_blocks_are_blank(&retained.blocks),
        "the retained Even footer has no active content"
    );
}

/// Header/footer part bindings are not revision-switchable in Word. Reject
/// therefore retains the newly created physical binding, but its story must be
/// empty and the original synthesized blank slot and all unrelated section
/// properties must remain unchanged.
#[test]
fn create_header_reject_all_retains_only_inert_physical_structure() {
    let doc = Document::parse(&make_plain_docx()).expect("parse");
    let base = doc.snapshot().canonical.clone();

    let mut tracked = apply_transaction(
        &base,
        &txn(
            vec![EditStep::CreateHeader {
                kind: HeaderFooterKind::Even,
                rationale: None,
            }],
            MaterializationMode::TrackedChange,
        ),
    )
    .expect("apply")
    .0;
    accept_all(&mut tracked.clone()); // smoke: accept does not panic

    reject_all_with_styles(&mut tracked, None);
    assert_eq!(
        tracked
            .headers
            .iter()
            .filter(|story| story.kind == HeaderFooterKind::Default)
            .collect::<Vec<_>>(),
        base.headers
            .iter()
            .filter(|story| story.kind == HeaderFooterKind::Default)
            .collect::<Vec<_>>(),
        "reject preserves the original logical blank slot"
    );
    let retained = tracked
        .headers
        .iter()
        .filter(|story| story.kind == HeaderFooterKind::Even && !story.synthesized)
        .collect::<Vec<_>>();
    assert_eq!(retained.len(), 1, "one physical Even header remains");
    assert!(
        retained[0].blocks.is_empty(),
        "the retained physical header has no active content"
    );

    let mut actual_section = tracked
        .body_section_properties
        .clone()
        .expect("created header has a final section binding");
    assert!(actual_section.header_refs.iter().any(|reference| {
        reference.kind == HeaderFooterKind::Even
            && reference.part_path == retained[0].part_name
            && !reference.synthesized
    }));
    actual_section
        .header_refs
        .retain(|reference| reference.kind != HeaderFooterKind::Even);
    assert_eq!(
        Some(actual_section),
        base.body_section_properties,
        "the inert binding is the only retained section-property delta"
    );
}
