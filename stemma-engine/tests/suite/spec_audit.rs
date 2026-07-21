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
        identity: 0,
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

/// Build a synthetic revision-heavy artifact entirely through public engine
/// operations: diff authors a real move, then two unrelated tracked edits add
/// distinct prior authors. Tests using this helper therefore audit a
/// Stemma-produced artifact, the exact persistence boundary they specify.
fn stemma_produced_revision_heavy_docx() -> Vec<u8> {
    let before = make_docx_with_body(concat!(
        r#"<w:p><w:r><w:t>Opening language remains unchanged.</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>Paragraph Bravo is long enough for move detection.</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>Paragraph Charlie is long enough for move detection.</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>Prior edit target one.</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>Prior edit target two.</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>Unrelated editable tail.</w:t></w:r></w:p>"#,
    ));
    let target = make_docx_with_body(concat!(
        r#"<w:p><w:r><w:t>Opening language remains unchanged.</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>Paragraph Charlie is long enough for move detection.</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>Paragraph Bravo is long enough for move detection.</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>Prior edit target one.</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>Prior edit target two.</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>Unrelated editable tail.</w:t></w:r></w:p>"#,
    ));
    let base = Document::parse(&before).expect("parse synthetic move base");
    let target = Document::parse(&target).expect("parse synthetic move target");
    let moved = base
        .diff_as(&target, "Mover")
        .expect("author synthetic move");
    let alice_step = replace_step(&moved, "target one", "reviewed target one");
    let with_alice = apply(
        &moved,
        alice_step,
        MaterializationMode::TrackedChange,
        revision(1, "Alice"),
    );
    apply(
        &with_alice,
        replace_step(&with_alice, "target two", "reviewed target two"),
        MaterializationMode::TrackedChange,
        revision(2, "Bob"),
    )
    .serialize(&stemma::ExportOptions::default())
    .expect("produce canonical revision-heavy input")
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

/// A tracked move is one logical revision with two structural locations: the
/// moveFrom source shadow and the moveTo destination copy. The census collapses
/// those carriers into one row, but that row must still account for both
/// locations in the untouched proof.
#[test]
fn tracked_move_accounts_for_source_shadow_and_destination_copy() {
    let before = make_docx_with_body(concat!(
        r#"<w:p><w:r><w:t>Opening language remains unchanged.</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>Paragraph Bravo is long enough for move detection.</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>Paragraph Charlie is long enough for move detection.</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>Closing language remains unchanged.</w:t></w:r></w:p>"#,
    ));
    let target = make_docx_with_body(concat!(
        r#"<w:p><w:r><w:t>Opening language remains unchanged.</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>Paragraph Charlie is long enough for move detection.</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>Paragraph Bravo is long enough for move detection.</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>Closing language remains unchanged.</w:t></w:r></w:p>"#,
    ));
    let base = Document::parse(&before).expect("parse move base");
    let target = Document::parse(&target).expect("parse move target");
    let moved = base.diff_as(&target, "Mover").expect("author tracked move");

    let report = moved.review().expect("review tracked move");
    assert_eq!(
        report
            .new_revisions
            .iter()
            .filter(|row| row.kind == RevisionKind::Move)
            .count(),
        1,
        "one move intention produces one census row: {report:?}"
    );
    assert!(
        report.direct_changes.is_empty(),
        "a tracked move is not a direct change: {report:?}"
    );
    assert!(
        report.untouched.violations.is_empty(),
        "both carriers are accounted for by the move census row: {report:?}"
    );
}

/// Receipts↔audit agreement, engine edition: the census's new identities are
/// exactly the after-side identity set minus the baseline identity set — the
/// same rule the write receipt uses.
#[test]
fn census_ids_agree_with_identity_set_difference() {
    let doc = Document::parse(&make_docx_with_body(THREE_PARAS)).expect("parse");
    let before_ids: std::collections::HashSet<u32> =
        stemma::tracked_model::enumerate_revisions(&doc.snapshot().canonical)
            .into_iter()
            .map(|r| r.revision_id)
            .collect();
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
    let new_identity_ids: std::collections::HashSet<u32> =
        stemma::tracked_model::enumerate_revisions(&edited.snapshot().canonical)
            .into_iter()
            .map(|r| r.revision_id)
            .filter(|id| !before_ids.contains(id))
            .collect();
    assert_eq!(
        census_ids, new_identity_ids,
        "in a session the audit census and receipt identity-set difference are the same set"
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

/// A saved artifact may use different OOXML `w:id` values from its input.
/// Those values are diagnostics, not correspondence keys: a delivery audit
/// must agree with the session receipt that every unrelated prior revision,
/// including a grouped move, remained untouched.
#[test]
fn saved_unrelated_edit_preserves_multi_author_and_move_revisions() {
    let before = stemma_produced_revision_heavy_docx();
    let doc = Document::parse(&before).expect("parse revision-heavy input");
    let step = replace_step(&doc, "Unrelated editable tail", "Unrelated revised tail.");
    let edited = apply(
        &doc,
        step,
        MaterializationMode::TrackedChange,
        revision(1, "Current Editor"),
    );

    let session = edited.review().expect("session receipt audit runs");
    assert!(
        session
            .preexisting_revisions
            .iter()
            .all(|row| row.disposition == RevisionDisposition::Untouched),
        "the session receipt says every prior revision is preserved: {session:?}"
    );
    assert!(
        session
            .preexisting_revisions
            .iter()
            .any(|row| row.record.kind == RevisionKind::Move),
        "the synthetic witness includes one grouped move: {session:?}"
    );

    let saved = edited
        .serialize(&stemma::ExportOptions::default())
        .expect("save revision-heavy output");
    let delivery = stemma::audit(&before, &saved).expect("delivery audit runs");
    assert_eq!(
        delivery.preexisting_revisions, session.preexisting_revisions,
        "save/reopen delivery evidence must agree with the session receipt"
    );
    assert!(delivery.direct_changes.is_empty(), "{delivery:?}");
}

/// Engine identities are stable across the save/reopen boundary for every
/// semantically unchanged prior revision. The serializer is free to replace
/// wire ids, so this compares the public enumeration identity by semantic row.
#[test]
fn prior_revision_identity_is_stable_across_save_and_reopen() {
    let before = stemma_produced_revision_heavy_docx();
    let doc = Document::parse(&before).expect("parse revision-heavy input");
    let before_rows = stemma::tracked_model::enumerate_revisions(&doc.snapshot().canonical);

    let step = replace_step(&doc, "Unrelated editable tail", "Unrelated revised tail.");
    let edited = apply(
        &doc,
        step,
        MaterializationMode::TrackedChange,
        revision(1, "Current Editor"),
    );
    let edited_rows = stemma::tracked_model::enumerate_revisions(&edited.snapshot().canonical);
    let saved = edited
        .serialize(&stemma::ExportOptions::default())
        .expect("save revision-heavy output");
    let reopened = Document::parse(&saved).expect("reopen stemma-produced artifact");
    let reopened_rows = stemma::tracked_model::enumerate_revisions(&reopened.snapshot().canonical);

    for row in &reopened_rows {
        let edited_id = edited_rows
            .iter()
            .find(|edited| {
                edited.kind == row.kind
                    && edited.author == row.author
                    && edited.excerpt == row.excerpt
            })
            .map(|edited| edited.revision_id);
        assert_eq!(
            edited_id.as_ref(),
            Some(&row.revision_id),
            "produced revision identity changed on its first save/reopen: {row:?}"
        );
    }

    for row in reopened_rows
        .iter()
        .filter(|row| row.author.as_deref() != Some("Current Editor"))
    {
        let before_id = before_rows
            .iter()
            .find(|before| {
                before.kind == row.kind
                    && before.author == row.author
                    && before.excerpt == row.excerpt
            })
            .map(|before| before.revision_id);
        assert_eq!(
            before_id.as_ref(),
            Some(&row.revision_id),
            "unchanged revision identity changed across save/reopen: {row:?}"
        );
    }
}

/// Receipt/audit agreement is an invariant over the representative synthetic
/// fixture shapes, not a one-off example. The session and delivery paths must
/// assign the same disposition to every prior revision after an unrelated edit.
#[test]
fn session_and_delivery_prior_dispositions_agree_across_fixture_matrix() {
    for (name, body, needle, replacement) in [
        (
            "multi-author inserts/deletes",
            PARAS_WITH_PENDING,
            "Left alone",
            "Tail revised.",
        ),
        (
            "multi-author with move",
            THREE_PARAS,
            "Unrelated editable tail",
            "Unrelated revised tail.",
        ),
    ] {
        let before = if name == "multi-author with move" {
            stemma_produced_revision_heavy_docx()
        } else {
            make_docx_with_body(body)
        };
        let doc = Document::parse(&before).unwrap_or_else(|error| panic!("{name}: {error}"));
        let edited = apply(
            &doc,
            replace_step(&doc, needle, replacement),
            MaterializationMode::TrackedChange,
            revision(1, "Matrix Editor"),
        );
        let session = edited
            .review()
            .unwrap_or_else(|error| panic!("{name}: session audit: {error}"));
        let saved = edited
            .serialize(&stemma::ExportOptions::default())
            .unwrap_or_else(|error| panic!("{name}: save: {error}"));
        let delivery = stemma::audit(&before, &saved)
            .unwrap_or_else(|error| panic!("{name}: delivery audit: {error}"));

        let session_dispositions: Vec<_> = session
            .preexisting_revisions
            .iter()
            .map(|row| (&row.record.revision_id, &row.disposition))
            .collect();
        let delivery_dispositions: Vec<_> = delivery
            .preexisting_revisions
            .iter()
            .map(|row| (&row.record.revision_id, &row.disposition))
            .collect();
        assert_eq!(
            delivery_dispositions, session_dispositions,
            "{name}: session receipt and delivery audit disagree"
        );
    }
}

/// Resolving a pre-existing revision flips its disposition to Resolved, and
/// the committed effect of that resolution is annotated on the direct-delta
/// row it produces — attributed, not silently dropped and not misread as a
/// hand edit.
#[test]
fn resolved_preexisting_revision_reports_resolved_and_annotates_direct_delta() {
    let doc = Document::parse(&make_docx_with_body(PARAS_WITH_PENDING)).expect("parse");
    // Address the revisions by their minted identities (H7), not the raw wire
    // ids 41/42: Alice's deletion ("sole ") and Bob's insertion ("exclusive ").
    let id_of = |author: &str, kind: RevisionKind| {
        stemma::tracked_model::enumerate_revisions(&doc.snapshot().canonical)
            .into_iter()
            .find(|r| r.author.as_deref() == Some(author) && r.kind == kind)
            .unwrap_or_else(|| panic!("a {kind:?} revision by {author} exists"))
            .revision_id
    };
    let alice_del = id_of("Alice", RevisionKind::Delete);
    let bob_ins = id_of("Bob", RevisionKind::Insert);
    // Accept Alice's deletion; leave Bob's insertion pending.
    let resolved = doc
        .project(stemma::Resolution::Selective {
            ids: std::collections::HashSet::from([alice_del]),
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
        by_id(alice_del).disposition,
        RevisionDisposition::Resolved,
        "{report:?}"
    );
    assert_eq!(
        by_id(bob_ins).disposition,
        RevisionDisposition::Untouched,
        "{report:?}"
    );
    assert!(report.new_revisions.is_empty(), "{report:?}");

    // Accepting the deletion moved committed content ("sole " is gone) —
    // that committed change must be annotated with the resolved revision.
    assert_eq!(report.direct_changes.len(), 1, "{report:?}");
    let row = &report.direct_changes[0];
    assert!(
        row.coincides_with_resolution.contains(&alice_del),
        "the committed effect of resolving Alice's deletion is attributed: {row:?}"
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
    // H7: the census carries the minted identity, not the raw wire id 201.
    assert_ne!(
        header_rows[0].revision_id, 0,
        "the census row carries a real minted identity: {report:?}"
    );
    assert_eq!(header_rows[0].kind, RevisionKind::Insert);
    assert!(report.untouched.violations.is_empty(), "{report:?}");
}

// ─── Untouched proof vs positional decoration refs ──────────────────────────

/// Domain rule: the untouched proof compares DOCUMENT CONTENT. A tracked
/// insertion in one paragraph must not indict a later, untouched paragraph
/// merely because the engine's internal inline counter (embedded in
/// decoration `opaque_ref`s) renumbered — that counter is a store reference,
/// not content. Witnessed on wild documents: a one-word tracked edit made
/// the verification fail on pure `decoration.opaque_ref` differences in
/// otherwise untouched paragraphs.
#[test]
fn tracked_edit_does_not_indict_untouched_decoration_bearing_paragraph() {
    let before = make_docx_with_body(concat!(
        r#"<w:p><w:r><w:t>Opening paragraph.</w:t></w:r></w:p>"#,
        r#"<w:p><w:bookmarkStart w:id="3" w:name="Anchor"/>"#,
        r#"<w:r><w:t>Bookmarked paragraph.</w:t></w:r>"#,
        r#"<w:bookmarkEnd w:id="3"/></w:p>"#,
    ));
    let after = make_docx_with_body(concat!(
        r#"<w:p><w:r><w:t>Opening paragraph.</w:t></w:r>"#,
        r#"<w:ins w:id="90" w:author="Editor" w:date="2026-07-16T08:00:00Z">"#,
        r#"<w:r><w:t xml:space="preserve"> Added.</w:t></w:r></w:ins></w:p>"#,
        r#"<w:p><w:bookmarkStart w:id="3" w:name="Anchor"/>"#,
        r#"<w:r><w:t>Bookmarked paragraph.</w:t></w:r>"#,
        r#"<w:bookmarkEnd w:id="3"/></w:p>"#,
    ));
    let report = stemma::audit(&before, &after).expect("audit runs");
    assert_eq!(report.new_revisions.len(), 1, "{report:?}");
    assert!(report.direct_changes.is_empty(), "{report:?}");
    assert!(
        report.untouched.violations.is_empty(),
        "the bookmarked paragraph is untouched content: {report:?}"
    );
}

/// The counterpart that keeps the proof honest: a bookmark RENAMED in place
/// (no tracked markup) IS a direct change to document content (§17.13.6.2 —
/// the name is the bookmark's identity; the numeric id is a disposable
/// pairing key) and must surface as an untouched-proof violation.
#[test]
fn silent_bookmark_rename_is_an_untouched_violation() {
    let before = make_docx_with_body(concat!(
        r#"<w:p><w:bookmarkStart w:id="3" w:name="Anchor"/>"#,
        r#"<w:r><w:t>Bookmarked paragraph.</w:t></w:r>"#,
        r#"<w:bookmarkEnd w:id="3"/></w:p>"#,
    ));
    let after = make_docx_with_body(concat!(
        r#"<w:p><w:bookmarkStart w:id="3" w:name="Renamed"/>"#,
        r#"<w:r><w:t>Bookmarked paragraph.</w:t></w:r>"#,
        r#"<w:bookmarkEnd w:id="3"/></w:p>"#,
    ));
    let report = stemma::audit(&before, &after).expect("audit runs");
    assert!(
        !report.untouched.violations.is_empty(),
        "a renamed bookmark is a content change the proof must surface: {report:?}"
    );
}

/// Same rule for hyperlink-bearing paragraphs: hyperlink identity is the
/// TARGET (url + anchor), not the engine's ephemeral inline NodeId — that id
/// is minted from a document-global counter and renumbers whenever any
/// earlier inline content shifts. A tracked insertion in one paragraph must
/// not produce a direct-change row for an untouched hyperlink paragraph
/// later in the document.
#[test]
fn tracked_edit_does_not_indict_untouched_hyperlink_paragraph() {
    let hyperlink_para = concat!(
        r#"<w:p><w:r><w:t xml:space="preserve">Email is </w:t></w:r>"#,
        r#"<w:hyperlink w:anchor="section1" w:history="1">"#,
        r#"<w:r><w:t>info@example.org</w:t></w:r></w:hyperlink></w:p>"#,
    );
    let before = make_docx_with_body(&format!(
        r#"<w:p><w:r><w:t>Opening paragraph.</w:t></w:r></w:p>{hyperlink_para}"#
    ));
    let after = make_docx_with_body(&format!(
        concat!(
            r#"<w:p><w:r><w:t>Opening paragraph.</w:t></w:r>"#,
            r#"<w:ins w:id="90" w:author="Editor" w:date="2026-07-16T08:00:00Z">"#,
            r#"<w:r><w:t xml:space="preserve"> Added.</w:t></w:r></w:ins></w:p>"#,
            "{}"
        ),
        hyperlink_para
    ));
    let report = stemma::audit(&before, &after).expect("audit runs");
    assert_eq!(report.new_revisions.len(), 1, "{report:?}");
    assert!(
        report.direct_changes.is_empty(),
        "an untouched hyperlink paragraph is not a direct change: {report:?}"
    );
    assert!(report.untouched.violations.is_empty(), "{report:?}");
}
