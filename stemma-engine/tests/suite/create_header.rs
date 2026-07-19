//! Integration tests for the `CreateHeader` / `CreateFooter` authoring verbs
//! (`EditStep::CreateHeader` / `CreateFooter`, §17.10.2 / §17.13.5.32).
//!
//! These author a NET-NEW, blank header/footer story PLUS a body-section
//! reference, tracked as a `w:sectPrChange`. The contract under test:
//!   - accept-all == the doc WITH the new story + section reference (== Direct);
//!   - reject-all == the original (the new story is pruned, the original sectPr —
//!     including the importer's synthesized-blank Default reference — restored);
//!   - both projections are validator-clean;
//!   - the new story serializes as a valid part and survives a reparse;
//!   - fail-loud refusals: a kind already referenced on the section (incl. the
//!     synthesized Default), and a stacked tracked sectPrChange.
//!
//! The verb authors the genuinely net-new `Even` kind: the importer always
//! materializes a blank `Default` header/footer reference per §17.10.2, so a
//! `Default` create is refused in favor of `EditHeader`.

use stemma::api::Document;
use stemma::domain::{HeaderFooterKind, RevisionInfo};
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

/// The tracked redline of a `CreateHeader { Even }` validates clean, accept-all
/// keeps the new Even header story + reference, and reject-all restores the
/// original (no Even reference, original sectPr) while keeping the synthesized
/// Default header.
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

    // reject-all (IR projection): no Even reference, the original section
    // restored, and the orphan blank Even story pruned (back to the base header
    // inventory).
    let rejected = edited.project(Resolution::RejectAll).expect("reject all");
    assert!(
        !has_header_ref(&rejected, &HeaderFooterKind::Even),
        "reject drops the Even header reference"
    );
    assert_eq!(
        rejected.snapshot().canonical.headers.len(),
        base_header_count,
        "reject prunes the net-new Even header story"
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
/// projects both ways (accept keeps the footer, reject prunes it).
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

    let accepted = edited.project(Resolution::AcceptAll).expect("accept all");
    assert_eq!(
        accepted.snapshot().canonical.footers.len(),
        base_footer_count + 1,
        "accept keeps the net-new footer story"
    );

    let rejected = edited.project(Resolution::RejectAll).expect("reject all");
    assert_eq!(
        rejected.snapshot().canonical.footers.len(),
        base_footer_count,
        "reject prunes the net-new footer story"
    );
}

/// Reject-all at the IR layer (engine accept/reject) reconstructs the base
/// exactly — including the importer's synthesized Default header/footer.
#[test]
fn create_header_reject_all_reconstructs_base_ir() {
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
        tracked.headers, base.headers,
        "reject-all reconstructs the original header inventory exactly"
    );
    assert_eq!(
        tracked.body_section_properties, base.body_section_properties,
        "reject-all reconstructs the original section properties exactly"
    );
}
