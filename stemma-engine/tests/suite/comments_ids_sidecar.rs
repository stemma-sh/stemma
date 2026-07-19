//! The `word/commentsIds.xml` durable-id sidecar (w16cid, MS-DOCX
//! §2.5.3.1) must gain an entry for every newly-authored comment on a package
//! that CARRIES the part, and lose entries for deleted comments — otherwise
//! Word distrusts the new comment's extended metadata (reply shows unthreaded,
//! thread resolve reads done=false). Packages WITHOUT the part are left
//! untouched (Word does not require it; we never create it).
//!
//! Daily tier, corpus-free (synthetic in-memory DOCX carrying a real
//! commentsIds part).

use stemma::api::Document;
use stemma::docx::DocxArchive;
use stemma::domain::BlockNode;
use stemma::edit::{EditStep, EditTransaction, MaterializationMode};
use stemma::{ExportOptions, domain::RevisionInfo};

const COMMENT_PARA_ID: &str = "11111111";
const ORIGINAL_DURABLE_ID: &str = "AAAA0001";

/// A one-paragraph DOCX carrying a single anchored comment (id 0) plus the
/// comments / commentsExtended sidecars. When `with_ids` is true it also carries
/// `word/commentsIds.xml` with one durable-id entry for the comment.
fn make_commented_docx(with_ids: bool) -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t xml:space="preserve">Discuss the </w:t></w:r><w:commentRangeStart w:id="0"/><w:r><w:t>Term</w:t></w:r><w:commentRangeEnd w:id="0"/><w:r><w:commentReference w:id="0"/></w:r><w:r><w:t xml:space="preserve"> clause.</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;

    let comments_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml"><w:comment w:id="0" w:author="Reviewer" w:date="2026-06-01T00:00:00Z" w:initials="R"><w:p w14:paraId="{COMMENT_PARA_ID}"><w:r><w:t>Original note.</w:t></w:r></w:p></w:comment></w:comments>"#
    );

    let comments_extended_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w15:commentsEx xmlns:w15="http://schemas.microsoft.com/office/word/2012/wordml"><w15:commentEx w15:paraId="{COMMENT_PARA_ID}" w15:done="0"/></w15:commentsEx>"#
    );

    let comments_ids_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w16cid:commentsIds xmlns:w16cid="http://schemas.microsoft.com/office/word/2016/wordml/cid"><w16cid:commentId w16cid:paraId="{COMMENT_PARA_ID}" w16cid:durableId="{ORIGINAL_DURABLE_ID}"/></w16cid:commentsIds>"#
    );

    // Content types: always the comment + commentsExtended overrides; add the
    // commentsIds override only when the part is present.
    let ids_override = if with_ids {
        r#"<Override PartName="/word/commentsIds.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.commentsIds+xml"/>"#
    } else {
        ""
    };
    let content_types = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/comments.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"/><Override PartName="/word/commentsExtended.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.commentsExtended+xml"/>{ids_override}</Types>"#
    );

    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;

    let ids_rel = if with_ids {
        r#"<Relationship Id="rId4" Type="http://schemas.microsoft.com/office/2016/relationships/commentsIds" Target="commentsIds.xml"/>"#
    } else {
        ""
    };
    let doc_rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="comments.xml"/><Relationship Id="rId3" Type="http://schemas.microsoft.com/office/2011/relationships/commentsExtended" Target="commentsExtended.xml"/>{ids_rel}</Relationships>"#
    );

    use std::io::Write;
    use zip::write::FileOptions;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        let mut write = |name: &str, data: &str| {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        };
        write("[Content_Types].xml", &content_types);
        write("_rels/.rels", rels);
        write("word/_rels/document.xml.rels", &doc_rels);
        write("word/document.xml", document_xml);
        write("word/comments.xml", &comments_xml);
        write("word/commentsExtended.xml", &comments_extended_xml);
        if with_ids {
            write("word/commentsIds.xml", &comments_ids_xml);
        }
        zip.finish().unwrap();
    }
    buf
}

fn txn(steps: Vec<EditStep>) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("Author".to_string()),
            date: Some("2026-06-02T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

/// The `word/commentsIds.xml` text of a serialized package, or None if absent.
fn comments_ids_of(bytes: &[u8]) -> Option<String> {
    let archive = DocxArchive::read(bytes).expect("read serialized package");
    archive
        .get("word/commentsIds.xml")
        .map(|b| String::from_utf8(b.to_vec()).expect("utf8 commentsIds"))
}

/// Count `w16cid:commentId` entries in the commentsIds XML.
fn entry_count(xml: &str) -> usize {
    xml.matches("<w16cid:commentId").count()
}

fn first_body_block_id(doc: &Document) -> stemma::domain::NodeId {
    match &doc.snapshot().canonical.blocks[0].block {
        BlockNode::Paragraph(p) => p.id.clone(),
        other => panic!("expected paragraph, got {other:?}"),
    }
}

/// The last-paragraph paraId (commentsIds key) of the comment at `idx`.
fn comment_key(doc: &Document, idx: usize) -> String {
    doc.snapshot().canonical.comments[idx]
        .last_para_id()
        .expect("comment has a paraId")
        .to_string()
}

/// Sentinel: replying on a commentsIds-bearing package adds the reply's
/// durable-id entry (and preserves the original's). FAILS without the fix (the
/// part passes through verbatim, missing the reply).
#[test]
fn reply_on_commentsids_doc_adds_entry() {
    let base = Document::parse(&make_commented_docx(true)).expect("parse");
    let replied = base
        .apply(&txn(vec![EditStep::CommentReply {
            parent_comment_id: "0".to_string(),
            body: "A threaded reply.".to_string(),
            author: Some("Author".to_string()),
            rationale: None,
        }]))
        .expect("apply reply");

    let reply_key = comment_key(&replied, 1);
    let bytes = replied
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let ids = comments_ids_of(&bytes).expect("commentsIds present");

    assert_eq!(entry_count(&ids), 2, "one entry per comment: {ids}");
    assert!(
        ids.contains(&format!(r#"w16cid:paraId="{COMMENT_PARA_ID}""#)),
        "original comment entry preserved: {ids}"
    );
    assert!(
        ids.contains(&format!(r#"w16cid:durableId="{ORIGINAL_DURABLE_ID}""#)),
        "original durableId preserved (stable identity): {ids}"
    );
    assert!(
        ids.contains(&format!(r#"w16cid:paraId="{reply_key}""#)),
        "the reply's paraId now has a durable-id entry: {ids}"
    );
}

/// Create + reply on a commentsIds-bearing package adds TWO entries (both new
/// comments), on top of the original.
#[test]
fn create_and_reply_chain_adds_two_entries() {
    let base = Document::parse(&make_commented_docx(true)).expect("parse");
    let block_id = first_body_block_id(&base);

    let created = base
        .apply(&txn(vec![EditStep::CommentCreate {
            block_id,
            expect: "clause".to_string(),
            semantic_hash: None,
            body: "A brand-new comment.".to_string(),
            author: Some("Author".to_string()),
            rationale: None,
        }]))
        .expect("apply create");
    // Reply to the freshly-created comment (id 1).
    let new_comment_id = created.snapshot().canonical.comments[1].id.clone();
    let chained = created
        .apply(&txn(vec![EditStep::CommentReply {
            parent_comment_id: new_comment_id,
            body: "A reply to the new comment.".to_string(),
            author: Some("Author".to_string()),
            rationale: None,
        }]))
        .expect("apply reply");

    assert_eq!(
        chained.snapshot().canonical.comments.len(),
        3,
        "original + created + reply"
    );
    let bytes = chained
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let ids = comments_ids_of(&bytes).expect("commentsIds present");
    assert_eq!(
        entry_count(&ids),
        3,
        "original entry + the two newly-authored comments: {ids}"
    );
    // Every current comment's key is represented.
    for idx in 0..3 {
        let key = comment_key(&chained, idx);
        assert!(
            ids.contains(&format!(r#"w16cid:paraId="{key}""#)),
            "comment {idx} keyed {key} has an entry: {ids}"
        );
    }
}

/// Deleting a thread removes its entries from commentsIds (no dangling durable
/// ids), consistent with the whole-thread delete contract.
#[test]
fn delete_removes_subtree_entries() {
    let base = Document::parse(&make_commented_docx(true)).expect("parse");
    let replied = base
        .apply(&txn(vec![EditStep::CommentReply {
            parent_comment_id: "0".to_string(),
            body: "Reply that will be deleted with its thread.".to_string(),
            author: Some("Author".to_string()),
            rationale: None,
        }]))
        .expect("apply reply");
    // Delete the thread root (id 0) — the whole thread goes.
    let deleted = replied
        .apply(&txn(vec![EditStep::CommentDelete {
            comment_id: "0".to_string(),
            rationale: None,
        }]))
        .expect("apply delete");
    assert!(
        deleted.snapshot().canonical.comments.is_empty(),
        "thread deleted"
    );

    let bytes = deleted
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let ids = comments_ids_of(&bytes).expect("commentsIds still present (part not removed)");
    assert_eq!(entry_count(&ids), 0, "all entries removed: {ids}");
    assert!(
        !ids.contains(&format!(r#"w16cid:durableId="{ORIGINAL_DURABLE_ID}""#)),
        "no dangling durableId for the deleted comment: {ids}"
    );
}

/// A package WITHOUT a commentsIds part is left untouched: authoring a comment
/// does not create the part (Word doesn't require it — creating it is scope
/// creep).
#[test]
fn doc_without_commentsids_part_is_untouched() {
    let base = Document::parse(&make_commented_docx(false)).expect("parse");
    // Sanity: the input truly lacks the part.
    assert!(
        comments_ids_of(&make_commented_docx(false)).is_none(),
        "fixture has no commentsIds part"
    );

    let replied = base
        .apply(&txn(vec![EditStep::CommentReply {
            parent_comment_id: "0".to_string(),
            body: "A reply on a doc with no durable-id sidecar.".to_string(),
            author: Some("Author".to_string()),
            rationale: None,
        }]))
        .expect("apply reply");
    let bytes = replied
        .serialize(&ExportOptions::default())
        .expect("serialize");
    assert!(
        comments_ids_of(&bytes).is_none(),
        "no commentsIds part is created where it was absent"
    );
}
