//! Daily-tier spec tests for the COMMENTS verb against ECMA-376 §17.13.4
//! (comments / commentRangeStart / commentRangeEnd / commentReference) and
//! MS-DOCX §2.5.1 (commentsExtended: w15:commentEx / w15:paraId /
//! w15:paraIdParent / w15:done).
//!
//! Behavioral constraints (these encode the spec, not the current code):
//! - SPEC §17.13.4.4/.5: an authored comment range emits exactly one balanced
//!   `commentRangeStart` / `commentRangeEnd` pair carrying the same `w:id`.
//! - SPEC §17.13.4.6: the `commentReference` w:id must resolve to a comment in
//!   word/comments.xml (I-XREF-003 — zero dangling references).
//! - SPEC MS-DOCX §2.5.1: a reply records a `w15:commentEx` whose
//!   `w15:paraIdParent` is the parent comment's first-body-paragraph
//!   `w14:paraId`; `CommentResolve` flips `w15:done`.
//! - Fail-loud (CLAUDE.md "no silent fallbacks"): a missing anchor, an empty
//!   body, and a missing comment target each surface their own EditError.
//!
//! Daily tier, corpus-free (synthesized in-memory DOCX).

use stemma::ExportOptions;
use stemma::api::Document;
use stemma::docx::DocxArchive;
use stemma::domain::{BlockNode, CanonDoc, NodeId, RevisionInfo};
use stemma::edit::{EditError, EditStep, EditTransaction, MaterializationMode, apply_transaction};

/// A one-paragraph DOCX with NO pre-existing comments. The verb authors the
/// first comment, forcing the synthesize-comments.xml path.
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
            identity: 0,
            author: Some("Reviewer".to_string()),
            date: Some("2026-06-01T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

fn create_step(block_id: NodeId, expect: &str, body: &str) -> EditStep {
    EditStep::CommentCreate {
        block_id,
        expect: expect.to_string(),
        semantic_hash: None,
        body: body.to_string(),
        author: Some("Reviewer".to_string()),
        rationale: None,
    }
}

/// §17.13.4.4/.5 + .6: an authored comment emits a balanced range pair and the
/// reference resolves to comments.xml (zero dangling — I-XREF-003), and
/// commentsExtended is well-formed.
#[test]
fn spec_authored_comment_balanced_range_and_resolving_reference() {
    let base = Document::parse(&make_docx("The Effective Date governs the term.")).expect("parse");
    let block_id = first_block_id(&base.snapshot().canonical);
    let edited = base
        .apply(&txn(vec![create_step(
            block_id,
            "Effective Date",
            "Define this term.",
        )]))
        .expect("create");

    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");

    // I-XREF: run the package validator; assert no dangling commentReference
    // (I-XREF-003) findings, error or warning.
    let validation = stemma::docx_validate::validate_docx(&bytes);
    let xref003: Vec<String> = validation
        .findings
        .iter()
        .filter(|f| f.rule_id == "I-XREF-003")
        .map(|f| f.to_string())
        .collect();
    assert!(
        xref003.is_empty(),
        "expected zero dangling commentReference findings, got: {xref003:?}"
    );

    // §17.13.4.4/.5: balanced range markers in document.xml.
    let archive = DocxArchive::read(&bytes).expect("read");
    let doc_xml = String::from_utf8_lossy(archive.get("word/document.xml").expect("document.xml"));
    let starts = doc_xml.matches("commentRangeStart").count();
    let ends = doc_xml.matches("commentRangeEnd").count();
    assert_eq!(starts, 1, "exactly one commentRangeStart");
    assert_eq!(ends, 1, "exactly one commentRangeEnd");
    assert!(
        doc_xml.contains("commentReference"),
        "commentReference present in body"
    );

    // comments.xml exists and carries the body text.
    let comments_xml =
        String::from_utf8_lossy(archive.get("word/comments.xml").expect("comments.xml"));
    assert!(comments_xml.contains("Define this term."));
    // The comment's first body paragraph carries a w14:paraId (commentsExtended
    // key). It is only emitted when there is extended metadata; a bare authored
    // comment has none yet, so paraId may be absent here — that is fine. The
    // reply/resolve tests below assert the paraId/commentsExtended linkage.
}

/// MS-DOCX §2.5.1: a reply records a w15:commentEx whose w15:paraIdParent is the
/// parent's first-body-paragraph w14:paraId, threading the reply under it.
#[test]
fn spec_reply_threads_under_parent_via_para_id_parent() {
    let base =
        Document::parse(&make_docx("Reply target around Governing Law clause.")).expect("parse");
    let block_id = first_block_id(&base.snapshot().canonical);
    let created = base
        .apply(&txn(vec![create_step(
            block_id,
            "Governing Law",
            "Is New York correct?",
        )]))
        .expect("create");
    let parent_id = created.snapshot().canonical.comments[0].id.clone();

    let replied = created
        .apply(&txn(vec![EditStep::CommentReply {
            parent_comment_id: parent_id.clone(),
            body: "Yes, confirmed by counsel.".to_string(),
            author: Some("Counsel".to_string()),
            rationale: None,
        }]))
        .expect("reply");

    let canon = &replied.snapshot().canonical;
    assert_eq!(canon.comments.len(), 2, "parent + reply");

    // The parent now has a first-body-paragraph paraId.
    let parent = canon
        .comments
        .iter()
        .find(|c| c.id == parent_id)
        .expect("parent present");
    let parent_para_id = parent
        .first_para_id()
        .expect("parent has a w14:paraId after reply")
        .to_string();

    // The reply's commentsExtended record threads under the parent's paraId.
    let reply = canon
        .comments
        .iter()
        .find(|c| c.id != parent_id)
        .expect("reply present");
    let reply_para_id = reply.first_para_id().expect("reply has paraId").to_string();
    let reply_rec = canon
        .comments_extended
        .iter()
        .find(|r| r.para_id == reply_para_id)
        .expect("reply has a commentsExtended record");
    assert_eq!(
        reply_rec.para_id_parent.as_deref(),
        Some(parent_para_id.as_str()),
        "w15:paraIdParent threads the reply under the parent"
    );

    // Serialized commentsExtended.xml carries the threading.
    let bytes = replied
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let archive = DocxArchive::read(&bytes).expect("read");
    let ext = String::from_utf8_lossy(
        archive
            .get("word/commentsExtended.xml")
            .expect("commentsExtended.xml emitted"),
    );
    assert!(ext.contains("w15:paraIdParent"), "paraIdParent serialized");
    assert!(ext.contains(&parent_para_id), "parent paraId referenced");
}

/// MS-DOCX §2.5.1: CommentResolve flips w15:done to true and back to false.
#[test]
fn spec_resolve_flips_done_flag() {
    let base = Document::parse(&make_docx("Resolve this Termination note.")).expect("parse");
    let block_id = first_block_id(&base.snapshot().canonical);
    let created = base
        .apply(&txn(vec![create_step(block_id, "Termination", "Check.")]))
        .expect("create");
    let cid = created.snapshot().canonical.comments[0].id.clone();

    // Resolve.
    let resolved = created
        .apply(&txn(vec![EditStep::CommentResolve {
            comment_id: cid.clone(),
            done: true,
            rationale: None,
        }]))
        .expect("resolve");
    let canon = &resolved.snapshot().canonical;
    let para_id = canon.comments[0]
        .first_para_id()
        .expect("paraId after resolve")
        .to_string();
    let rec = canon
        .comments_extended
        .iter()
        .find(|r| r.para_id == para_id)
        .expect("commentsExtended record after resolve");
    assert!(rec.done, "w15:done set true after resolve(true)");

    // Serialized w15:done="1".
    let bytes = resolved
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let archive = DocxArchive::read(&bytes).expect("read");
    let ext = String::from_utf8_lossy(
        archive
            .get("word/commentsExtended.xml")
            .expect("commentsExtended.xml"),
    );
    assert!(ext.contains(r#"w15:done="1""#), "done=1 serialized: {ext}");

    // Un-resolve.
    let unresolved = resolved
        .apply(&txn(vec![EditStep::CommentResolve {
            comment_id: cid,
            done: false,
            rationale: None,
        }]))
        .expect("unresolve");
    let canon2 = &unresolved.snapshot().canonical;
    let rec2 = canon2
        .comments_extended
        .iter()
        .find(|r| r.para_id == para_id)
        .expect("record still present");
    assert!(!rec2.done, "w15:done cleared after resolve(false)");
}

/// MS-DOCX §2.5.1 round-trip invariant (the linkage that survives a real Word
/// save): after resolve + reply, **serialize and re-parse through the public
/// `Document::parse` path**, then assert the comment↔commentsExtended linkage
/// is intact in the re-imported IR:
///   - the resolved comment's first body paragraph carries a `w14:paraId`;
///   - `commentsExtended` has a `w15:commentEx` keyed on that SAME paraId with
///     `done == true`;
///   - the reply's `w15:commentEx` threads under the parent via
///     `w15:paraIdParent == parent paraId`.
///
/// This is the daily, VM-free encoding of the Word-oracle conformance claim
/// (`comments_present_resolved_and_threaded_in_word`). The bug it guards: the
/// serializer emitted commentsExtended correctly, but the `Document::parse`
/// import path (`import_and_anchor`) never parsed commentsExtended.xml, so the
/// resolved flag + threading were silently dropped on every re-import — and no
/// daily test exercised serialize→parse for this part. Word kept the data; our
/// own re-read lost it. Asserting the linkage at parse level catches it without
/// a real-Word oracle.
#[test]
fn spec_resolve_and_reply_linkage_survives_serialize_reparse() {
    let base = Document::parse(&make_docx("The Confidential Information clause is broad."))
        .expect("parse");
    let block_id = first_block_id(&base.snapshot().canonical);
    let created = base
        .apply(&txn(vec![create_step(
            block_id,
            "Confidential Information",
            "Should we narrow this definition?",
        )]))
        .expect("create");
    let cid = created.snapshot().canonical.comments[0].id.clone();

    let resolved = created
        .apply(&txn(vec![EditStep::CommentResolve {
            comment_id: cid.clone(),
            done: true,
            rationale: None,
        }]))
        .expect("resolve");
    let threaded = resolved
        .apply(&txn(vec![EditStep::CommentReply {
            parent_comment_id: cid,
            body: "Agreed; see redline.".to_string(),
            author: Some("Counsel".to_string()),
            rationale: None,
        }]))
        .expect("reply");

    let bytes = threaded
        .serialize(&ExportOptions::default())
        .expect("serialize");

    // The round-trip through the public parse path must preserve the linkage.
    let reparsed = Document::parse(&bytes).expect("reparse");
    let canon = &reparsed.snapshot().canonical;
    assert_eq!(
        canon.comments.len(),
        2,
        "parent + reply present after reparse"
    );

    // The resolved comment's first body paragraph carries a w14:paraId, and a
    // commentEx with that SAME paraId is done. (Linkage asserted at the
    // re-imported IR level — no fabricated paraId, no fallback.)
    let parent_para_id = canon.comments[0]
        .first_para_id()
        .expect("parent comment's first paragraph carries a w14:paraId after reparse")
        .to_string();
    assert!(
        canon
            .comments_extended
            .iter()
            .any(|r| r.para_id == parent_para_id && r.done),
        "resolved comment's w15:commentEx (keyed on its first-paragraph w14:paraId) \
         carries w15:done after serialize→reparse"
    );

    // The reply threads under the parent via w15:paraIdParent.
    assert!(
        canon
            .comments_extended
            .iter()
            .any(|r| r.para_id_parent.as_deref() == Some(parent_para_id.as_str())),
        "reply's w15:commentEx threads under the parent paraId after serialize→reparse"
    );

    // Byte-level: comments.xml's resolved comment paragraph carries the exact
    // w14:paraId that commentsExtended.xml references with w15:done="1".
    let archive = DocxArchive::read(&bytes).expect("read");
    let comments_xml = String::from_utf8_lossy(
        archive
            .get("word/comments.xml")
            .expect("comments.xml emitted"),
    )
    .into_owned();
    let ext_xml = String::from_utf8_lossy(
        archive
            .get("word/commentsExtended.xml")
            .expect("commentsExtended.xml emitted"),
    )
    .into_owned();
    assert!(
        comments_xml.contains(&format!(r#"w14:paraId="{parent_para_id}""#)),
        "comments.xml carries the parent paraId on a comment paragraph: {comments_xml}"
    );
    assert!(
        ext_xml.contains(&format!(r#"w15:paraId="{parent_para_id}""#))
            && ext_xml.contains(r#"w15:done="1""#),
        "commentsExtended.xml references the parent paraId with done=1: {ext_xml}"
    );
}

/// Fail-loud: CommentCreate with an anchor that is not present → CommentAnchorNotFound.
#[test]
fn spec_create_missing_anchor_is_anchor_not_found() {
    let canon = Document::parse(&make_docx("No such phrase here."))
        .expect("parse")
        .snapshot()
        .canonical
        .clone();
    let block_id = first_block_id(&canon);
    let err = apply_transaction(
        &canon,
        &txn(vec![create_step(block_id.clone(), "ABSENT PHRASE", "x")]),
    )
    .expect_err("missing anchor must fail");
    match err {
        EditError::CommentAnchorNotFound {
            block_id: b,
            expected,
            ..
        } => {
            assert_eq!(b, block_id);
            assert_eq!(expected, "ABSENT PHRASE");
        }
        other => panic!("expected CommentAnchorNotFound, got {other:?}"),
    }
}

/// Fail-loud: an empty (whitespace-only) body → CommentEmptyBody.
#[test]
fn spec_create_empty_body_is_refused() {
    let canon = Document::parse(&make_docx("Anchor on Scope please."))
        .expect("parse")
        .snapshot()
        .canonical
        .clone();
    let block_id = first_block_id(&canon);
    let err = apply_transaction(&canon, &txn(vec![create_step(block_id, "Scope", "   ")]))
        .expect_err("empty body must fail");
    assert!(
        matches!(err, EditError::CommentEmptyBody { .. }),
        "expected CommentEmptyBody, got {err:?}"
    );
}

/// Fail-loud: replying to / resolving a nonexistent comment → CommentTargetNotFound.
#[test]
fn spec_reply_and_resolve_unknown_comment_is_target_not_found() {
    let canon = Document::parse(&make_docx("Body text."))
        .expect("parse")
        .snapshot()
        .canonical
        .clone();

    let reply_err = apply_transaction(
        &canon,
        &txn(vec![EditStep::CommentReply {
            parent_comment_id: "999".to_string(),
            body: "orphaned reply".to_string(),
            author: None,
            rationale: None,
        }]),
    )
    .expect_err("reply to unknown parent must fail");
    assert!(
        matches!(reply_err, EditError::CommentTargetNotFound { ref comment_id, .. } if comment_id == "999"),
        "expected CommentTargetNotFound, got {reply_err:?}"
    );

    let resolve_err = apply_transaction(
        &canon,
        &txn(vec![EditStep::CommentResolve {
            comment_id: "999".to_string(),
            done: true,
            rationale: None,
        }]),
    )
    .expect_err("resolve unknown must fail");
    assert!(
        matches!(resolve_err, EditError::CommentTargetNotFound { ref comment_id, .. } if comment_id == "999"),
        "expected CommentTargetNotFound, got {resolve_err:?}"
    );
}
