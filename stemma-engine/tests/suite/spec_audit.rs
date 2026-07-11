//! RFC 0001 — the audit core, engine level: `Document::review` (session
//! form) and `stemma::audit` (stateless form) must answer, mechanically:
//! WHAT changed (tracked census + untracked direct delta), what happened to
//! every pre-existing revision (disposition by content, never by marker
//! absence alone), and is everything else provably untouched (full
//! structural equality, never the guard hash).
//!
//! Every expectation below is stated from the domain rule (what a reviewer
//! receiving `after` can verify against `before`), not from the current
//! implementation's output.

use stemma::api::Document;
use stemma::audit::{DirectChangeKind, RevisionDisposition, UntouchedViolationKind};
use stemma::edit::{
    ContentFragment, EditStep, EditTransaction, MaterializationMode, ParagraphContent,
};
use stemma::{RevisionInfo, RevisionKind, StoryScope};

// ─── Fixtures ────────────────────────────────────────────────────────────────

fn zip_docx(parts: &[(&str, &str)]) -> Vec<u8> {
    use std::io::Write;
    use zip::write::FileOptions;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        for (name, content) in parts {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

fn make_docx_with_body(body_inner: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
    );
    zip_docx(&[
        (
            "[Content_Types].xml",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#,
        ),
        (
            "_rels/.rels",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#,
        ),
        (
            "word/_rels/document.xml.rels",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#,
        ),
        ("word/document.xml", &document_xml),
    ])
}

const THREE_PARAS: &str = r#"<w:p><w:r><w:t>Service levels apply.</w:t></w:r></w:p><w:p><w:r><w:t>Credits are the sole remedy.</w:t></w:r></w:p><w:p><w:r><w:t>Left alone.</w:t></w:r></w:p>"#;

/// A body whose second paragraph carries TWO pending revisions by two
/// authors: del #41 ("sole ") by Alice, ins #42 ("exclusive ") by Bob.
const PARAS_WITH_PENDING: &str = r#"<w:p><w:r><w:t>Service levels apply.</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve">Credits are the </w:t></w:r><w:del w:id="41" w:author="Alice" w:date="2026-07-01T00:00:00Z"><w:r><w:delText xml:space="preserve">sole </w:delText></w:r></w:del><w:ins w:id="42" w:author="Bob" w:date="2026-07-01T00:00:00Z"><w:r><w:t xml:space="preserve">exclusive </w:t></w:r></w:ins><w:r><w:t>remedy.</w:t></w:r></w:p><w:p><w:r><w:t>Left alone.</w:t></w:r></w:p>"#;

fn revision(id: u32, author: &str) -> RevisionInfo {
    RevisionInfo {
        revision_id: id,
        author: Some(author.to_string()),
        date: Some("2026-07-04T00:00:00Z".to_string()),
        apply_op_id: None,
    }
}

/// Replace the whole text of the paragraph holding `needle` with
/// `replacement`.
fn replace_step(doc: &Document, needle: &str, replacement: &str) -> EditStep {
    let view = doc.read();
    let block = view
        .blocks
        .iter()
        .find(|b| b.text.contains(needle))
        .expect("target block exists");
    EditStep::ReplaceParagraphText {
        block_id: block.id.clone(),
        rationale: None,
        replacement_role: None,
        expect: block.text.clone(),
        semantic_hash: Some(block.guard.clone()),
        content: ParagraphContent {
            fragments: vec![ContentFragment::Text(replacement.to_string())],
        },
    }
}

fn apply(doc: &Document, step: EditStep, mode: MaterializationMode, rev: RevisionInfo) -> Document {
    doc.apply(&EditTransaction {
        steps: vec![step],
        summary: None,
        materialization_mode: mode,
        revision: rev,
    })
    .expect("edit applies")
}

// ─── The identity audit ──────────────────────────────────────────────────────

/// Domain rule: a document audited against itself changed nothing — every
/// section empty, every block verified untouched, package valid.
#[test]
fn audit_of_identical_documents_is_empty_and_fully_verified() {
    let bytes = make_docx_with_body(THREE_PARAS);
    let report = stemma::audit(&bytes, &bytes).expect("audit runs");
    assert!(report.new_revisions.is_empty(), "{report:?}");
    assert!(report.preexisting_revisions.is_empty(), "{report:?}");
    assert!(report.direct_changes.is_empty(), "{report:?}");
    assert!(report.untouched.violations.is_empty(), "{report:?}");
    assert_eq!(
        report.untouched.verified_blocks, 3,
        "all three paragraphs verified: {report:?}"
    );
    assert!(report.validator.ok, "{report:?}");
}

/// Same rule through the session door: a freshly parsed document reviews
/// clean.
#[test]
fn review_of_unedited_document_is_empty() {
    let doc = Document::parse(&make_docx_with_body(THREE_PARAS)).expect("parse");
    let report = doc.review().expect("review runs");
    assert!(report.new_revisions.is_empty());
    assert!(report.direct_changes.is_empty());
    assert!(report.untouched.violations.is_empty());
    assert_eq!(report.untouched.verified_blocks, 3);
}

// ─── Section 1a: the tracked census ──────────────────────────────────────────

/// A tracked session: the edit appears in the census (with the author and
/// id the transaction carried), the direct delta is EMPTY (a tracked change
/// leaves committed content alone), and everything else is verified.
#[test]
fn tracked_edit_lands_in_census_not_direct_delta() {
    let doc = Document::parse(&make_docx_with_body(THREE_PARAS)).expect("parse");
    let step = replace_step(&doc, "Credits", "Credits are the exclusive remedy.");
    let edited = apply(
        &doc,
        step,
        MaterializationMode::TrackedChange,
        revision(1, "AI"),
    );

    let report = edited.review().expect("review runs");
    assert!(
        !report.new_revisions.is_empty(),
        "the tracked edit must appear in the census: {report:?}"
    );
    assert!(
        report
            .new_revisions
            .iter()
            .all(|r| r.author.as_deref() == Some("AI")),
        "census rows carry the transaction's author: {report:?}"
    );
    assert!(
        report.direct_changes.is_empty(),
        "a tracked change is not a direct change: {report:?}"
    );
    assert!(report.untouched.violations.is_empty(), "{report:?}");
    assert!(
        report.untouched.verified_blocks >= 2,
        "the unedited paragraphs are verified: {report:?}"
    );
    assert!(report.validator.ok, "{report:?}");
}

/// Receipts↔audit agreement, engine edition: the census's new ids are
/// exactly the surviving ids above the baseline watermark — the same set
/// the write receipt reports.
#[test]
fn census_ids_agree_with_the_watermark_rule() {
    let doc = Document::parse(&make_docx_with_body(THREE_PARAS)).expect("parse");
    let watermark = stemma::max_revision_id(&doc.snapshot().canonical);
    let step = replace_step(&doc, "Credits", "Credits are the exclusive remedy.");
    let edited = apply(
        &doc,
        step,
        MaterializationMode::TrackedChange,
        revision(1, "AI"),
    );

    let report = edited.review().expect("review runs");
    let census_ids: std::collections::HashSet<u32> =
        report.new_revisions.iter().map(|r| r.revision_id).collect();
    let watermark_ids: std::collections::HashSet<u32> =
        stemma::tracked_model::enumerate_revisions(&edited.snapshot().canonical)
            .into_iter()
            .map(|r| r.revision_id)
            .filter(|id| *id > watermark)
            .collect();
    assert_eq!(
        census_ids, watermark_ids,
        "in a session the record-identity census and the enumerated-watermark rule are the \
         same set"
    );
}

// ─── Section 2: the direct (untracked) delta ─────────────────────────────────

/// A direct-mode session: the edit appears in the direct delta (committed
/// content moved with no covering redline), the census is empty, and the
/// proof still covers the rest.
#[test]
fn direct_edit_lands_in_direct_delta_not_census() {
    let doc = Document::parse(&make_docx_with_body(THREE_PARAS)).expect("parse");
    let step = replace_step(&doc, "Credits", "Credits are the exclusive remedy.");
    let edited = apply(&doc, step, MaterializationMode::Direct, revision(1, "AI"));

    let report = edited.review().expect("review runs");
    assert!(
        report.new_revisions.is_empty(),
        "a direct edit authors no revision: {report:?}"
    );
    assert_eq!(report.direct_changes.len(), 1, "{report:?}");
    let row = &report.direct_changes[0];
    assert_eq!(row.kind, DirectChangeKind::BlockModified);
    assert_eq!(row.story, StoryScope::Body);
    assert!(
        row.new_excerpt
            .as_deref()
            .unwrap_or("")
            .contains("exclusive"),
        "{row:?}"
    );
    assert!(report.untouched.violations.is_empty(), "{report:?}");
}

/// A mixed session reports BOTH: the tracked edit in the census, the
/// direct edit in the direct delta.
#[test]
fn mixed_session_reports_both_sections() {
    let doc = Document::parse(&make_docx_with_body(THREE_PARAS)).expect("parse");
    let tracked = replace_step(&doc, "Credits", "Credits are the exclusive remedy.");
    let doc = apply(
        &doc,
        tracked,
        MaterializationMode::TrackedChange,
        revision(1, "AI"),
    );
    let direct = replace_step(&doc, "Service", "Service levels apply strictly.");
    let doc = apply(&doc, direct, MaterializationMode::Direct, revision(2, "AI"));

    let report = doc.review().expect("review runs");
    assert!(!report.new_revisions.is_empty(), "{report:?}");
    assert_eq!(report.direct_changes.len(), 1, "{report:?}");
    assert!(report.untouched.violations.is_empty(), "{report:?}");
}

// ─── Section 1b: pre-existing revision dispositions ──────────────────────────

/// Untouched pending revisions are reported as pre-existing with
/// disposition Untouched — never absorbed into the session's census.
#[test]
fn preexisting_revisions_left_alone_are_untouched() {
    let doc = Document::parse(&make_docx_with_body(PARAS_WITH_PENDING)).expect("parse");
    let report = doc.review().expect("review runs");
    assert!(report.new_revisions.is_empty(), "{report:?}");
    assert_eq!(report.preexisting_revisions.len(), 2, "{report:?}");
    assert!(
        report
            .preexisting_revisions
            .iter()
            .all(|p| p.disposition == RevisionDisposition::Untouched),
        "{report:?}"
    );
    assert!(report.untouched.violations.is_empty(), "{report:?}");
}

/// Resolving a pre-existing revision flips its disposition to Resolved, and
/// the committed effect of that resolution is annotated on the direct-delta
/// row it produces — attributed, not silently dropped and not misread as a
/// hand edit.
#[test]
fn resolved_preexisting_revision_reports_resolved_and_annotates_direct_delta() {
    let doc = Document::parse(&make_docx_with_body(PARAS_WITH_PENDING)).expect("parse");
    // Accept Alice's deletion (#41); leave Bob's insertion (#42) pending.
    let resolved = doc
        .project(stemma::Resolution::Selective {
            ids: std::collections::HashSet::from([41]),
            action: stemma::ResolveSelectionAction::Accept,
        })
        .expect("selective accept applies");

    let report = resolved.review().expect("review runs");
    let by_id = |id: u32| {
        report
            .preexisting_revisions
            .iter()
            .find(|p| p.record.revision_id == id)
            .unwrap_or_else(|| panic!("record {id} present: {report:?}"))
    };
    assert_eq!(
        by_id(41).disposition,
        RevisionDisposition::Resolved,
        "{report:?}"
    );
    assert_eq!(
        by_id(42).disposition,
        RevisionDisposition::Untouched,
        "{report:?}"
    );
    assert!(report.new_revisions.is_empty(), "{report:?}");

    // Accepting the deletion moved committed content ("sole " is gone) —
    // that committed change must be annotated with revision 41.
    assert_eq!(report.direct_changes.len(), 1, "{report:?}");
    let row = &report.direct_changes[0];
    assert!(
        row.coincides_with_resolution.contains(&41),
        "the committed effect of resolving #41 is attributed: {row:?}"
    );
    assert!(report.untouched.violations.is_empty(), "{report:?}");
}

// ─── Section 3: the untouched proof ──────────────────────────────────────────

/// The stateless form sees an out-of-band edit (made by "someone else",
/// i.e. a different byte package): it lands in the direct delta and the
/// remaining blocks still verify.
#[test]
fn stateless_audit_reports_out_of_band_edit() {
    let before = make_docx_with_body(THREE_PARAS);
    let after = make_docx_with_body(&THREE_PARAS.replace("sole remedy", "only remedy"));
    let report = stemma::audit(&before, &after).expect("audit runs");
    assert!(report.new_revisions.is_empty(), "{report:?}");
    assert_eq!(report.direct_changes.len(), 1, "{report:?}");
    assert_eq!(
        report.direct_changes[0].kind,
        DirectChangeKind::BlockModified
    );
    assert_eq!(report.untouched.verified_blocks, 2, "{report:?}");
    assert!(report.untouched.violations.is_empty(), "{report:?}");
}

/// A deliberately corrupted pairing: a difference no diff row and no census
/// row explains must surface as an untouched-proof VIOLATION — the proof
/// verifies equality itself, it does not trust the diff.
#[test]
fn unexplained_difference_is_an_untouched_violation() {
    use stemma::audit::audit_documents;

    let doc = Document::parse(&make_docx_with_body(THREE_PARAS)).expect("parse");
    let before = doc.snapshot().canonical.as_ref().clone();
    let mut after = before.clone();
    // A change invisible to the text/marks diff AND to the census, but a
    // real fidelity difference (it changes what serializes):
    // `numbering_suppressed` is document content under the roundtrip
    // comparator's classification, and no diff row or revision covers it.
    // The proof must catch it itself — it does not trust the diff.
    {
        let tb = &mut after.blocks[2];
        if let stemma::BlockNode::Paragraph(p) = &mut tb.block {
            p.numbering_suppressed = true;
        } else {
            panic!("fixture block 2 is a paragraph");
        }
    }

    let report = audit_documents(
        &before,
        &after,
        // Synthetic style-less fixture: no style table to re-resolve against.
        None,
        None,
        stemma::ValidationReport {
            ok: true,
            issues: vec![],
        },
    )
    .expect("audit runs");
    assert!(
        report
            .untouched
            .violations
            .iter()
            .any(|v| matches!(v.kind, UntouchedViolationKind::BlockDiffers { .. })),
        "the unexplained difference is a violation: {report:?}"
    );
}

// ─── Cross-story coverage ────────────────────────────────────────────────────

/// The stateless census sees a header revision — the enumeration hole this
/// RFC's step 0 closed. A `before` without the header revision vs an
/// `after` with it: the census must carry a Header-scoped insert.
#[test]
fn header_revision_appears_in_stateless_census() {
    let header_before = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:r><w:t xml:space="preserve">Confidential </w:t></w:r></w:p></w:hdr>"#;
    let header_after = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:r><w:t xml:space="preserve">Confidential </w:t></w:r><w:ins w:id="201" w:author="H. Reviewer" w:date="2026-07-04T10:00:00Z"><w:r><w:t>v2</w:t></w:r></w:ins></w:p></w:hdr>"#;
    let make = |header_xml: &str| {
        zip_docx(&[
            (
                "[Content_Types].xml",
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/></Types>"#,
            ),
            (
                "_rels/.rels",
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#,
            ),
            (
                "word/_rels/document.xml.rels",
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdH1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/></Relationships>"#,
            ),
            (
                "word/document.xml",
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body><w:p><w:r><w:t>Body paragraph.</w:t></w:r></w:p><w:sectPr><w:headerReference w:type="default" r:id="rIdH1"/></w:sectPr></w:body></w:document>"#,
            ),
            ("word/header1.xml", header_xml),
        ])
    };

    let report = stemma::audit(&make(header_before), &make(header_after)).expect("audit runs");
    let header_rows: Vec<_> = report
        .new_revisions
        .iter()
        .filter(|r| matches!(&r.location, StoryScope::Header { .. }))
        .collect();
    assert_eq!(
        header_rows.len(),
        1,
        "the header's new w:ins is in the census: {report:?}"
    );
    assert_eq!(header_rows[0].revision_id, 201);
    assert_eq!(header_rows[0].kind, RevisionKind::Insert);
    assert!(report.untouched.violations.is_empty(), "{report:?}");
}
