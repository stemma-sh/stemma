//! Integration tests for the COMMENTS authoring verb (create / reply / resolve
//! / delete), exercised through the public `Document` facade.
//!
//! Contract under test (CLAUDE.md "no silent fallbacks"; domain-model §11):
//! - `CommentCreate` anchors a comment range around `expect` and pushes a
//!   comment story; the markers survive an import -> apply -> export -> import
//!   round-trip;
//! - comments are ANNOTATIONS, not tracked changes: AcceptAll and RejectAll
//!   both retain the story + markers (the markers are never `w:ins`/`w:del`);
//! - `CommentDelete` removes the story AND all three anchor markers;
//! - a deliberate orphan (range markers present but no matching story) plus a
//!   missing marker fails `CommentRangeOrphaned` rather than half-deleting.
//!
//! Daily tier, corpus-free (synthesized in-memory DOCX).

use stemma::api::Document;
use stemma::docx::DocxArchive;
use stemma::domain::{
    BlockNode, CanonDoc, CommentExtended, InlineNode, NodeId, OpaqueKind, RevisionInfo,
};
use stemma::edit::{EditStep, EditTransaction, MaterializationMode};
use stemma::{ExportOptions, Resolution};

/// A minimal one-paragraph DOCX whose body text is `text`. No pre-existing
/// comments — the verb authors them from scratch.
fn make_docx(text: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"></Relationships>"#;

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

fn first_block_id(doc: &CanonDoc) -> NodeId {
    match &doc.blocks[0].block {
        BlockNode::Paragraph(p) => p.id.clone(),
        other => panic!("expected paragraph, got {other:?}"),
    }
}

fn txn(steps: Vec<EditStep>) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 1,
            author: Some("Reviewer".to_string()),
            date: Some("2026-06-01T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

/// Count the three comment markers (start/end/reference) for `id` across all
/// body paragraphs in a canonical doc.
fn marker_counts(doc: &CanonDoc, id: &str) -> (usize, usize, usize) {
    let mut start = 0;
    let mut end = 0;
    let mut reference = 0;
    for tb in &doc.blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    match inline {
                        InlineNode::CommentRangeStart { id: i } if i == id => start += 1,
                        InlineNode::CommentRangeEnd { id: i } if i == id => end += 1,
                        // The reference is a zero-width marker when freshly
                        // authored (InlineNode::CommentReference) but normalizes
                        // to an opaque run-level element on re-import. Both carry
                        // the same w:id and serialize identically — count either.
                        InlineNode::CommentReference { id: i } if i == id => reference += 1,
                        InlineNode::OpaqueInline(o) => {
                            if let OpaqueKind::CommentReference(rd) = &o.kind
                                && rd.reference_id == id
                            {
                                reference += 1;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    (start, end, reference)
}

fn comment_id_of(doc: &CanonDoc) -> String {
    doc.comments
        .first()
        .expect("at least one comment story")
        .id
        .clone()
}

/// T1: a comment body + anchored span round-trips through import -> apply ->
/// export -> import.
#[test]
fn t1_comment_round_trips_body_and_anchored_span() {
    let base =
        Document::parse(&make_docx("The term Net Revenue is defined below.")).expect("parse base");
    let block_id = first_block_id(&base.snapshot().canonical);

    let edited = base
        .apply(&txn(vec![EditStep::CommentCreate {
            block_id,
            expect: "Net Revenue".to_string(),
            semantic_hash: None,
            body: "Should this be capitalized consistently?".to_string(),
            author: Some("Reviewer".to_string()),
            rationale: None,
        }]))
        .expect("apply comment create");

    // The authored comment + 3 markers are present in the IR.
    let canon = &edited.snapshot().canonical;
    assert_eq!(canon.comments.len(), 1, "one comment story authored");
    let cid = comment_id_of(canon);
    assert_eq!(
        marker_counts(canon, &cid),
        (1, 1, 1),
        "exactly one of each anchor marker"
    );
    assert_eq!(
        canon.comments[0]
            .blocks
            .first()
            .and_then(|b| match &b.block {
                BlockNode::Paragraph(p) => p.first_content_text_node().map(|t| t.text.clone()),
                _ => None,
            })
            .as_deref(),
        Some("Should this be capitalized consistently?"),
        "comment body text preserved"
    );

    // Export -> re-import: the comment + markers survive the round-trip.
    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let reimported = Document::parse(&bytes).expect("reparse");
    let canon2 = &reimported.snapshot().canonical;
    assert_eq!(canon2.comments.len(), 1, "comment survives round-trip");
    let cid2 = comment_id_of(canon2);
    assert_eq!(
        marker_counts(canon2, &cid2),
        (1, 1, 1),
        "anchor markers survive round-trip"
    );

    // comments.xml is present in the exported package.
    let archive = DocxArchive::read(&bytes).expect("read exported");
    assert!(
        archive.get("word/comments.xml").is_some(),
        "comments.xml emitted"
    );
}

/// Annotation invariant: comments are NOT tracked changes, so both AcceptAll
/// and RejectAll retain the comment story + its anchor markers.
#[test]
fn comments_survive_accept_all_and_reject_all() {
    let base =
        Document::parse(&make_docx("Confidential Information is broadly defined.")).expect("parse");
    let block_id = first_block_id(&base.snapshot().canonical);
    let edited = base
        .apply(&txn(vec![EditStep::CommentCreate {
            block_id,
            expect: "Confidential Information".to_string(),
            semantic_hash: None,
            body: "Cross-check the definitions section.".to_string(),
            author: Some("Reviewer".to_string()),
            rationale: None,
        }]))
        .expect("apply");
    let cid = comment_id_of(&edited.snapshot().canonical);

    let accepted = edited.project(Resolution::AcceptAll).expect("accept all");
    let acc = &accepted.snapshot().canonical;
    assert_eq!(acc.comments.len(), 1, "accept-all keeps the comment story");
    assert_eq!(
        marker_counts(acc, &cid),
        (1, 1, 1),
        "accept-all keeps all anchor markers (comments are not w:ins/w:del)"
    );

    let rejected = edited.project(Resolution::RejectAll).expect("reject all");
    let rej = &rejected.snapshot().canonical;
    assert_eq!(rej.comments.len(), 1, "reject-all keeps the comment story");
    assert_eq!(
        marker_counts(rej, &cid),
        (1, 1, 1),
        "reject-all keeps all anchor markers (comments are not w:ins/w:del)"
    );
}

/// `CommentDelete` removes the story AND all three anchor markers.
#[test]
fn comment_delete_removes_story_and_all_markers() {
    let base =
        Document::parse(&make_docx("Delete this comment about Indemnity later.")).expect("parse");
    let block_id = first_block_id(&base.snapshot().canonical);
    let created = base
        .apply(&txn(vec![EditStep::CommentCreate {
            block_id,
            expect: "Indemnity".to_string(),
            semantic_hash: None,
            body: "Temporary note.".to_string(),
            author: Some("Reviewer".to_string()),
            rationale: None,
        }]))
        .expect("create");
    let cid = comment_id_of(&created.snapshot().canonical);

    let deleted = created
        .apply(&txn(vec![EditStep::CommentDelete {
            comment_id: cid.clone(),
            rationale: None,
        }]))
        .expect("delete");
    let canon = &deleted.snapshot().canonical;
    assert!(canon.comments.is_empty(), "comment story removed");
    assert_eq!(
        marker_counts(canon, &cid),
        (0, 0, 0),
        "all three anchor markers removed"
    );
}

/// A deliberate orphan: the comment story exists but one anchor marker is
/// missing. Deleting it must fail `CommentRangeOrphaned`, never half-delete.
#[test]
fn comment_delete_orphan_fails_loud() {
    use stemma::edit::{EditError, apply_transaction};

    let base = Document::parse(&make_docx("Orphan range around Liability here.")).expect("parse");
    let block_id = first_block_id(&base.snapshot().canonical);
    let created = base
        .apply(&txn(vec![EditStep::CommentCreate {
            block_id,
            expect: "Liability".to_string(),
            semantic_hash: None,
            body: "note".to_string(),
            author: Some("Reviewer".to_string()),
            rationale: None,
        }]))
        .expect("create");
    let cid = comment_id_of(&created.snapshot().canonical);

    // Surgically remove just the commentReference marker from the IR to
    // manufacture an orphaned range (a half-present anchor).
    let mut canon = (*created.snapshot().canonical).clone();
    for tb in &mut canon.blocks {
        if let BlockNode::Paragraph(p) = &mut tb.block {
            for seg in &mut p.segments {
                seg.inlines
                    .retain(|i| !matches!(i, InlineNode::CommentReference { id } if *id == cid));
            }
        }
    }

    let err = apply_transaction(
        &canon,
        &txn(vec![EditStep::CommentDelete {
            comment_id: cid.clone(),
            rationale: None,
        }]),
    )
    .expect_err("delete must fail on orphaned range");
    match err {
        EditError::CommentRangeOrphaned {
            comment_id,
            missing_markers,
            ..
        } => {
            assert_eq!(comment_id, cid);
            assert_eq!(missing_markers, vec!["commentReference"]);
        }
        other => panic!("expected CommentRangeOrphaned, got {other:?}"),
    }

    // No half-delete: the story and the surviving markers are untouched.
    assert_eq!(marker_counts(&canon, &cid), (1, 1, 0));
    assert_eq!(canon.comments.len(), 1);
}

/// B4: editing a paragraph that carries a comment must preserve the comment
/// range (anchored to its text) instead of collapsing/dropping it — and must not
/// DUPLICATE the markers (the no-double-inject gate).
#[test]
fn comment_range_survives_a_tracked_paragraph_edit() {
    use stemma::api::validate;
    use stemma::edit::{ContentFragment, ParagraphContent};

    let base = Document::parse(&make_docx("The original text here.")).expect("parse");
    let block_id = first_block_id(&base.snapshot().canonical);

    // Comment the word "text".
    let commented = base
        .apply(&txn(vec![EditStep::CommentCreate {
            block_id,
            expect: "text".to_string(),
            semantic_hash: None,
            body: "clarify".to_string(),
            author: Some("Reviewer".to_string()),
            rationale: None,
        }]))
        .expect("apply comment create");
    let id = comment_id_of(&commented.snapshot().canonical);
    assert_eq!(
        {
            let (s, e, _) = marker_counts(&commented.snapshot().canonical, &id);
            (s, e)
        },
        (1, 1),
        "comment range present before the edit"
    );

    // Tracked edit of the SAME paragraph (append). Before the fix the range
    // markers collapsed to the paragraph end; the range must now survive, with
    // EXACTLY one start + one end (no double injection).
    let bid = first_block_id(&commented.snapshot().canonical);
    let edited = commented
        .apply(&txn(vec![EditStep::ReplaceParagraphText {
            block_id: bid,
            rationale: None,
            replacement_role: None,
            expect: "original".to_string(),
            semantic_hash: None,
            content: ParagraphContent {
                fragments: vec![ContentFragment::Text(
                    "The original text here. EDITED".to_string(),
                )],
            },
        }]))
        .expect("edit a commented paragraph");

    let (s1, e1, r1) = marker_counts(&edited.snapshot().canonical, &id);
    assert_eq!(
        (s1, e1),
        (1, 1),
        "comment range preserved exactly once after the edit (not dropped, not duplicated)"
    );
    assert!(r1 >= 1, "comment reference preserved");
    assert_eq!(
        edited.snapshot().canonical.comments.len(),
        1,
        "the comment story is still present"
    );

    // accept-all and reject-all both serialize validator-clean.
    for (label, res) in [
        ("accept-all", Resolution::AcceptAll),
        ("reject-all", Resolution::RejectAll),
    ] {
        let bytes = edited
            .project(res)
            .expect("project")
            .serialize(&ExportOptions::default())
            .expect("serialize");
        assert!(
            validate(&bytes).ok,
            "{label} must validate: {:?}",
            validate(&bytes).issues
        );
    }
}

// ─── Reply anchoring ──────────────────────────────────────────────────────────

/// The ordered `(kind, id)` sequence of comment markers across all body
/// paragraphs, in document order. `kind` is "start" / "end" / "ref". Lets a
/// test assert Word's threaded-reply shape (reply markers nested against the
/// parent's), not just per-id counts.
fn body_marker_sequence(doc: &CanonDoc) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    for tb in &doc.blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    match inline {
                        InlineNode::CommentRangeStart { id } => out.push(("start", id.clone())),
                        InlineNode::CommentRangeEnd { id } => out.push(("end", id.clone())),
                        InlineNode::CommentReference { id } => out.push(("ref", id.clone())),
                        InlineNode::OpaqueInline(o) => {
                            if let OpaqueKind::CommentReference(rd) = &o.kind {
                                out.push(("ref", rd.reference_id.clone()));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    out
}

/// Author a comment on `expect` in the first body paragraph, returning the
/// edited document and the new comment id.
fn create_comment(base: &Document, expect: &str, body: &str) -> (Document, String) {
    let block_id = first_block_id(&base.snapshot().canonical);
    let edited = base
        .apply(&txn(vec![EditStep::CommentCreate {
            block_id,
            expect: expect.to_string(),
            semantic_hash: None,
            body: body.to_string(),
            author: Some("Reviewer".to_string()),
            rationale: None,
        }]))
        .expect("apply comment create");
    let cid = edited
        .snapshot()
        .canonical
        .comments
        .last()
        .expect("a comment story")
        .id
        .clone();
    (edited, cid)
}

fn reply_to(parent: &Document, parent_id: &str, body: &str) -> (Document, String) {
    let edited = parent
        .apply(&txn(vec![EditStep::CommentReply {
            parent_comment_id: parent_id.to_string(),
            body: body.to_string(),
            author: Some("Author".to_string()),
            rationale: None,
        }]))
        .expect("apply comment reply");
    let rid = edited
        .snapshot()
        .canonical
        .comments
        .last()
        .expect("reply story")
        .id
        .clone();
    (edited, rid)
}

/// Sentinel: a reply MUST author its own anchor range at the parent's
/// span (commentRangeStart/End + commentReference for the reply id). Without the
/// fix the reply carried none — invisible to Word. The reply markers must also
/// survive a serialize -> re-import round-trip, and thread under the parent in
/// commentsExtended.
#[test]
fn reply_authors_its_own_anchor_markers() {
    let base =
        Document::parse(&make_docx("Clarify the Termination clause please.")).expect("parse");
    let (commented, parent_id) = create_comment(&base, "Termination", "Is 30 days enough?");
    let (replied, reply_id) = reply_to(&commented, &parent_id, "Sixty is safer.");
    assert_ne!(parent_id, reply_id, "reply gets a fresh comment id");

    let canon = &replied.snapshot().canonical;
    assert_eq!(canon.comments.len(), 2, "parent + reply stories");

    // The core assertion: the reply has its OWN full anchor range in the body.
    assert_eq!(
        marker_counts(canon, &reply_id),
        (1, 1, 1),
        "reply authors its own commentRangeStart/End + commentReference"
    );
    // The parent keeps its single anchor range untouched.
    assert_eq!(
        marker_counts(canon, &parent_id),
        (1, 1, 1),
        "parent range is unchanged by the reply"
    );

    // Word's threaded shape: reply start nested just after the parent's start;
    // reply end + ref just after the parent's reference run.
    let seq = body_marker_sequence(canon);
    let idx = |k: &str, id: &str| {
        seq.iter()
            .position(|(kk, ii)| *kk == k && ii == id)
            .unwrap_or_else(|| panic!("missing marker {k}/{id} in {seq:?}"))
    };
    assert_eq!(
        idx("start", &reply_id),
        idx("start", &parent_id) + 1,
        "reply commentRangeStart is adjacent to the parent's start"
    );
    assert!(
        idx("end", &reply_id) > idx("ref", &parent_id)
            && idx("ref", &reply_id) > idx("end", &reply_id),
        "reply end + ref follow the parent's reference run: {seq:?}"
    );

    // commentsExtended threads the reply under the parent.
    let parent_para = canon.comments[0].first_para_id().expect("parent paraId");
    let reply_rec = canon
        .comments_extended
        .iter()
        .find(|r| r.para_id == canon.comments[1].first_para_id().unwrap())
        .expect("reply has a commentsExtended record");
    assert_eq!(
        reply_rec.para_id_parent.as_deref(),
        Some(parent_para),
        "reply threads under the parent's paraId"
    );

    // Round-trip: the reply's anchor range survives export + re-import.
    let bytes = replied
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let reimported = Document::parse(&bytes).expect("reparse");
    let canon2 = &reimported.snapshot().canonical;
    assert_eq!(canon2.comments.len(), 2, "both stories survive round-trip");
    // Ids are stable across the round-trip (comment w:id is preserved).
    assert_eq!(
        marker_counts(canon2, &reply_id),
        (1, 1, 1),
        "reply anchor range survives the round-trip"
    );
}

/// A reply-of-reply nests against its immediate parent (the first reply) and
/// threads under it, and authors its own anchor range too.
#[test]
fn reply_of_reply_threads_and_anchors() {
    let base = Document::parse(&make_docx("Review the Indemnity section.")).expect("parse");
    let (commented, root_id) = create_comment(&base, "Indemnity", "Too broad?");
    let (r1_doc, r1_id) = reply_to(&commented, &root_id, "Agree, narrow it.");
    let (r2_doc, r2_id) = reply_to(&r1_doc, &r1_id, "Proposed wording attached.");

    let canon = &r2_doc.snapshot().canonical;
    assert_eq!(canon.comments.len(), 3, "root + 2 replies");
    for id in [&root_id, &r1_id, &r2_id] {
        assert_eq!(
            marker_counts(canon, id),
            (1, 1, 1),
            "comment {id} has its own full anchor range"
        );
    }

    // r2 threads under r1 (its immediate parent), not the root.
    let para_of = |id: &str| {
        canon
            .comments
            .iter()
            .find(|c| c.id == id)
            .and_then(|c| c.first_para_id())
            .unwrap()
            .to_string()
    };
    let r2_rec = canon
        .comments_extended
        .iter()
        .find(|r| r.para_id == para_of(&r2_id))
        .expect("r2 record");
    assert_eq!(
        r2_rec.para_id_parent.as_deref(),
        Some(para_of(&r1_id).as_str()),
        "reply-of-reply threads under the first reply"
    );

    // r2's start nests just after r1's start (which nests after the root's).
    let seq = body_marker_sequence(canon);
    let idx = |k: &str, id: &str| {
        seq.iter()
            .position(|(kk, ii)| *kk == k && ii == id)
            .unwrap()
    };
    assert!(
        idx("start", &root_id) < idx("start", &r1_id)
            && idx("start", &r1_id) < idx("start", &r2_id),
        "starts nest root -> r1 -> r2: {seq:?}"
    );
}

/// Deleting a REPLY removes only the reply's markers and story; the parent's
/// range, story, and commentsExtended record are untouched.
#[test]
fn delete_reply_leaves_parent_intact() {
    let base = Document::parse(&make_docx("Discuss the Governing Law choice.")).expect("parse");
    let (commented, parent_id) = create_comment(&base, "Governing Law", "Which state?");
    let (replied, reply_id) = reply_to(&commented, &parent_id, "Delaware.");

    let deleted = replied
        .apply(&txn(vec![EditStep::CommentDelete {
            comment_id: reply_id.clone(),
            rationale: None,
        }]))
        .expect("delete reply");
    let canon = &deleted.snapshot().canonical;

    assert_eq!(
        marker_counts(canon, &reply_id),
        (0, 0, 0),
        "reply markers removed"
    );
    assert_eq!(
        marker_counts(canon, &parent_id),
        (1, 1, 1),
        "parent range untouched by the reply delete"
    );
    assert_eq!(canon.comments.len(), 1, "only the parent story remains");
    assert_eq!(canon.comments[0].id, parent_id);
    let parent_para = canon.comments[0].first_para_id().unwrap();
    assert!(
        canon
            .comments_extended
            .iter()
            .any(|r| r.para_id == parent_para),
        "parent's commentsExtended record survives"
    );
}

/// Contract: deleting a thread's ROOT deletes the whole thread — the root and
/// every reply under it (stories, markers, and commentsExtended records),
/// matching Word. No reply is left dangling or re-parented.
#[test]
fn delete_parent_deletes_whole_thread() {
    let base =
        Document::parse(&make_docx("Reconsider the Limitation of Liability cap.")).expect("parse");
    let (commented, root_id) = create_comment(&base, "Limitation of Liability", "Cap too low?");
    let (r1_doc, r1_id) = reply_to(&commented, &root_id, "Raise to 2x fees.");
    let (r2_doc, r2_id) = reply_to(&r1_doc, &r1_id, "Agreed.");

    let deleted = r2_doc
        .apply(&txn(vec![EditStep::CommentDelete {
            comment_id: root_id.clone(),
            rationale: None,
        }]))
        .expect("delete root");
    let canon = &deleted.snapshot().canonical;

    assert!(
        canon.comments.is_empty(),
        "the whole thread's stories are gone"
    );
    for id in [&root_id, &r1_id, &r2_id] {
        assert_eq!(
            marker_counts(canon, id),
            (0, 0, 0),
            "markers for {id} removed with the thread"
        );
    }
    assert!(
        canon.comments_extended.is_empty(),
        "the thread's commentsExtended records are gone (no dangling parent)"
    );
}

/// A reply whose parent carries no anchor markers is refused
/// (`CommentParentUnanchored`) — never authored as an unreachable comment.
#[test]
fn reply_to_unanchored_parent_refuses() {
    use stemma::edit::{EditError, apply_transaction};

    let base = Document::parse(&make_docx("Note on the Confidentiality term.")).expect("parse");
    let (commented, parent_id) = create_comment(&base, "Confidentiality", "Scope?");

    // Strip the parent's anchor markers to simulate an unanchored parent
    // (the exact defective shape this fix prevents us from producing).
    let mut canon = (*commented.snapshot().canonical).clone();
    for tb in &mut canon.blocks {
        if let BlockNode::Paragraph(p) = &mut tb.block {
            for seg in &mut p.segments {
                seg.inlines.retain(|i| {
                    !matches!(
                        i,
                        InlineNode::CommentRangeStart { id }
                            | InlineNode::CommentRangeEnd { id }
                            | InlineNode::CommentReference { id } if *id == parent_id
                    )
                });
            }
        }
    }

    let err = apply_transaction(
        &canon,
        &txn(vec![EditStep::CommentReply {
            parent_comment_id: parent_id.clone(),
            body: "This would be invisible.".to_string(),
            author: Some("Author".to_string()),
            rationale: None,
        }]),
    )
    .expect_err("reply to an unanchored parent must fail");
    match err {
        EditError::CommentParentUnanchored {
            parent_comment_id,
            missing_markers,
            ..
        } => {
            assert_eq!(parent_comment_id, parent_id);
            assert_eq!(
                missing_markers,
                vec!["commentRangeStart", "commentRangeEnd", "commentReference"]
            );
        }
        other => panic!("expected CommentParentUnanchored, got {other:?}"),
    }
    // No reply story was authored.
    assert_eq!(canon.comments.len(), 1, "no reply appended on refusal");
}

/// Thread-resolve: `w15:done` is a THREAD property — Word derives Done from the
/// thread ROOT's record. Resolving a REPLY must resolve the whole thread: the
/// root's record (and every record in the thread) flips to done, and the
/// reply's `paraIdParent` threading link is left intact.
#[test]
fn resolve_on_reply_resolves_whole_thread() {
    let base = Document::parse(&make_docx("Track the Payment Terms discussion.")).expect("parse");
    let (commented, parent_id) = create_comment(&base, "Payment Terms", "Net 30 or 60?");
    let (replied, reply_id) = reply_to(&commented, &parent_id, "Net 45.");

    let resolved = replied
        .apply(&txn(vec![EditStep::CommentResolve {
            comment_id: reply_id.clone(),
            done: true,
            rationale: None,
        }]))
        .expect("resolve reply");
    let canon = &resolved.snapshot().canonical;

    // Records key on the comment's LAST-paragraph paraId; single-paragraph
    // comments have last == first, so first_para_id is fine for lookup here.
    let key_of = |id: &str| {
        canon
            .comments
            .iter()
            .find(|c| c.id == id)
            .and_then(|c| c.last_para_id())
            .unwrap()
            .to_string()
    };
    let reply_rec = canon
        .comments_extended
        .iter()
        .find(|r| r.para_id == key_of(&reply_id))
        .expect("reply record");
    assert!(reply_rec.done, "reply's record is marked done");
    assert_eq!(
        reply_rec.para_id_parent.as_deref(),
        Some(key_of(&parent_id).as_str()),
        "resolve leaves the reply's threading link intact"
    );
    // The ROOT (parent) record — the one Word reads — is resolved too.
    let parent_rec = canon
        .comments_extended
        .iter()
        .find(|r| r.para_id == key_of(&parent_id))
        .expect("parent (thread root) record");
    assert!(
        parent_rec.para_id_parent.is_none(),
        "parent is the thread root"
    );
    assert!(
        parent_rec.done,
        "resolving a reply resolves the thread root Word derives Done from"
    );
}

/// Multi-paragraph resolve: a comment's `commentEx` record keys on its LAST-paragraph
/// paraId. Resolving a MULTI-PARAGRAPH comment must flip that existing record —
/// not synthesize a duplicate orphan keyed on the first paragraph, which would
/// leave the real record (and Word) reading Done=false.
#[test]
fn resolve_multi_paragraph_comment_flips_last_para_record_no_orphan() {
    use stemma::edit::apply_transaction;

    let base =
        Document::parse(&make_docx("Multi-paragraph comment on Warranties.")).expect("parse");
    let (created, cid) = create_comment(&base, "Warranties", "First paragraph of the note.");

    // Build a two-paragraph comment with a record keyed on the LAST paragraph
    // (exactly how Word writes a multi-paragraph comment).
    let mut canon = (*created.snapshot().canonical).clone();
    let first_key = canon.comments[0].first_para_id().unwrap().to_string();
    let last_key = "0CADAD02".to_string();
    assert_ne!(first_key, last_key);
    let mut second = canon.comments[0].blocks[0].clone();
    if let BlockNode::Paragraph(p) = &mut second.block {
        p.para_id = Some(last_key.clone());
        p.id = NodeId::from(format!("cm_{cid}_p2"));
    }
    canon.comments[0].blocks.push(second);
    canon.comments_extended.push(CommentExtended {
        para_id: last_key.clone(),
        para_id_parent: None,
        done: false,
    });
    let records_before = canon.comments_extended.len();

    let (out, _) = apply_transaction(
        &canon,
        &txn(vec![EditStep::CommentResolve {
            comment_id: cid.clone(),
            done: true,
            rationale: None,
        }]),
    )
    .expect("resolve multi-paragraph comment");

    assert_eq!(
        out.comments_extended.len(),
        records_before,
        "no orphan record synthesized — count unchanged"
    );
    let rec = out
        .comments_extended
        .iter()
        .find(|r| r.para_id == last_key)
        .expect("the last-paragraph-keyed record still exists");
    assert!(
        rec.done,
        "the real (last-paragraph) record is flipped to done"
    );
    assert!(
        !out.comments_extended.iter().any(|r| r.para_id == first_key),
        "no record keyed on the FIRST paragraph was created"
    );
}

/// Latent twin: a reply under a MULTI-PARAGRAPH parent must thread via the
/// parent's LAST-paragraph paraId (the record key), not its first — otherwise
/// `paraIdParent` points at a paraId no record uses and the thread breaks.
#[test]
fn reply_under_multi_paragraph_parent_threads_to_last_para() {
    use stemma::edit::apply_transaction;

    let base = Document::parse(&make_docx("Reply threading on Assignment clause.")).expect("parse");
    let (created, parent_id) = create_comment(&base, "Assignment", "Parent, paragraph one.");

    // Give the parent a second paragraph so first != last paraId.
    let mut canon = (*created.snapshot().canonical).clone();
    let first_key = canon.comments[0].first_para_id().unwrap().to_string();
    let last_key = "0CBEEF02".to_string();
    assert_ne!(first_key, last_key);
    let mut second = canon.comments[0].blocks[0].clone();
    if let BlockNode::Paragraph(p) = &mut second.block {
        p.para_id = Some(last_key.clone());
        p.id = NodeId::from(format!("cm_{parent_id}_p2"));
    }
    canon.comments[0].blocks.push(second);

    let (out, _) = apply_transaction(
        &canon,
        &txn(vec![EditStep::CommentReply {
            parent_comment_id: parent_id.clone(),
            body: "Reply to the multi-paragraph parent.".to_string(),
            author: Some("Author".to_string()),
            rationale: None,
        }]),
    )
    .expect("reply under multi-paragraph parent");

    let reply = out.comments.last().expect("reply story");
    let reply_key = reply.last_para_id().unwrap().to_string();
    let reply_rec = out
        .comments_extended
        .iter()
        .find(|r| r.para_id == reply_key)
        .expect("reply record");
    assert_eq!(
        reply_rec.para_id_parent.as_deref(),
        Some(last_key.as_str()),
        "reply threads under the parent's LAST-paragraph paraId"
    );
    assert_ne!(
        reply_rec.para_id_parent.as_deref(),
        Some(first_key.as_str()),
        "reply must NOT thread under the parent's first paragraph"
    );
    // The parent is represented as the thread root, keyed on its last paragraph.
    let parent_rec = out
        .comments_extended
        .iter()
        .find(|r| r.para_id == last_key)
        .expect("parent (root) record keyed on last paragraph");
    assert!(
        parent_rec.para_id_parent.is_none(),
        "parent is the thread root"
    );
}
