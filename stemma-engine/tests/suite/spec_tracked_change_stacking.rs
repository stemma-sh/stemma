//! B1: re-editing a paragraph that ALREADY carries a tracked change.
//!
//! Before the fix, a second tracked `ReplaceParagraphText` on a paragraph with a
//! pending tracked insertion was rejected — the staleness guard was checked
//! against the block AFTER prep flattened its tracked changes, so the client's
//! guard (the hash of the *projected*, redlined block) never matched, surfacing a
//! spurious `BlockSemanticHashMismatch`. Validation now runs BEFORE prep, against
//! the block the client actually saw, and the segment-level tracked precondition
//! is gone (prep flattens to a Normal base right after — flatten-then-diff).
//!
//! PRESERVE-prior-pending-changes semantics (the principle: never accept a user's
//! pending change for them): a re-edit diffs the new content against the accept-all
//! view but CARRIES the paragraph's prior tracked changes through verbatim — so
//! both the prior and the new changes stay pending, with their original revision
//! identity + author. accept-all yields the final text; reject-all restores the
//! PRISTINE original. (This inverts the former flatten-then-diff, which silently
//! accepted the prior change.)
//!
//! Daily tier, corpus-free.

use stemma::Resolution;
use stemma::api::{Document, validate};
use stemma::domain::{BlockNode, CanonDoc, InlineNode, NodeId, RevisionInfo};
use stemma::edit::{
    ContentFragment, EditStep, EditTransaction, MaterializationMode, ParagraphContent,
};
use stemma::runtime::ExportOptions;
use stemma::semantic_hash::block_guard;

fn make_para_docx(text: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#
    );
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

fn first_block_id(canon: &CanonDoc) -> NodeId {
    match &canon.blocks[0].block {
        BlockNode::Paragraph(p) => p.id.clone(),
        _ => panic!("first block is not a paragraph"),
    }
}

/// Concatenated text of the first paragraph's segments (post-projection view).
fn para_text(canon: &CanonDoc) -> String {
    let mut s = String::new();
    if let BlockNode::Paragraph(p) = &canon.blocks[0].block {
        for seg in &p.segments {
            for inline in &seg.inlines {
                if let InlineNode::Text(t) = inline {
                    s.push_str(&t.text);
                }
            }
        }
    }
    s
}

fn txn(steps: Vec<EditStep>, mode: MaterializationMode) -> EditTransaction {
    txn_by(steps, mode, "Reviewer")
}

fn txn_by(steps: Vec<EditStep>, mode: MaterializationMode, author: &str) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        materialization_mode: mode,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some(author.to_string()),
            date: Some("2026-06-29T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

/// (status-tag, text, author) for each text run of the first paragraph's tracked
/// segments — for asserting a prior change keeps its identity across a re-edit.
fn segs(canon: &CanonDoc) -> Vec<(&'static str, String, Option<String>)> {
    use stemma::domain::TrackingStatus;
    let mut out = Vec::new();
    if let BlockNode::Paragraph(p) = &canon.blocks[0].block {
        for seg in &p.segments {
            let (tag, author) = match &seg.status {
                TrackingStatus::Normal => ("normal", None),
                TrackingStatus::Inserted(r) => ("ins", r.author.clone()),
                TrackingStatus::Deleted(r) => ("del", r.author.clone()),
                TrackingStatus::InsertedThenDeleted(s) => ("insdel", s.deleted.author.clone()),
            };
            for inline in &seg.inlines {
                if let InlineNode::Text(t) = inline {
                    out.push((tag, t.text.clone(), author.clone()));
                }
            }
        }
    }
    out
}

fn replace(block_id: NodeId, expect: &str, text: &str, hash: Option<String>) -> EditStep {
    EditStep::ReplaceParagraphText {
        block_id,
        rationale: None,
        replacement_role: None,
        expect: expect.to_string(),
        semantic_hash: hash,
        content: ParagraphContent {
            fragments: vec![ContentFragment::Text(text.to_string())],
        },
    }
}

#[test]
fn reedit_tracked_paragraph_preserves_prior_pending_changes() {
    let doc0 = Document::parse(&make_para_docx("Hello world")).unwrap();
    let id = first_block_id(&doc0.snapshot().canonical);

    // Edit 1 (tracked): append " v1" → a pending tracked insertion.
    let doc1 = doc0
        .apply(&txn(
            vec![replace(id, "Hello world", "Hello world v1", None)],
            MaterializationMode::TrackedChange,
        ))
        .expect("first tracked edit applies");

    // The guard the client holds is the hash of the PROJECTED (redlined) block.
    let guard = block_guard(&doc1.snapshot().canonical.blocks[0].block);

    // Edit 2 (tracked) on the SAME paragraph, carrying that guard.
    let id2 = first_block_id(&doc1.snapshot().canonical);
    let doc2 = doc1
        .apply(&txn(
            vec![replace(
                id2,
                "Hello world v1",
                "Hello world v1 v2",
                Some(guard),
            )],
            MaterializationMode::TrackedChange,
        ))
        .expect("second tracked edit on an already-tracked paragraph must apply");

    // accept-all → the final intended text.
    let acc = doc2.project(Resolution::AcceptAll).expect("accept-all");
    assert_eq!(
        para_text(&acc.snapshot().canonical),
        "Hello world v1 v2",
        "accept-all = final text"
    );

    // reject-all → the PRISTINE ORIGINAL. A re-edit must NEVER accept the prior
    // pending change for the user: edit 1 (" v1") stays pending, so rejecting all
    // restores "Hello world", not the intermediate "Hello world v1". (This inverts
    // the former flatten-then-diff semantics, which silently accepted edit 1.)
    let rej = doc2.project(Resolution::RejectAll).expect("reject-all");
    assert_eq!(
        para_text(&rej.snapshot().canonical),
        "Hello world",
        "reject-all restores the pristine original — prior pending change preserved"
    );

    // The accept-all projection serializes validator-clean.
    let bytes = acc.serialize(&ExportOptions::default()).expect("serialize");
    assert!(
        validate(&bytes).ok,
        "accept-all serializes clean: {:?}",
        validate(&bytes).issues
    );
}

#[test]
fn reedit_preserves_an_unrelated_prior_pending_change() {
    // The exact production repro: two tracked edits in disjoint regions of the
    // SAME paragraph. Both must stay pending — the second must NOT accept the first.
    let doc0 = Document::parse(&make_para_docx("This is a test now foo bar baz")).unwrap();
    let id = first_block_id(&doc0.snapshot().canonical);
    let doc1 = doc0
        .apply(&txn(
            vec![replace(
                id,
                "This is a test now foo bar baz",
                "This is a EXAM now foo bar baz",
                None,
            )],
            MaterializationMode::TrackedChange,
        ))
        .expect("edit 1");
    let g = block_guard(&doc1.snapshot().canonical.blocks[0].block);
    let id1 = first_block_id(&doc1.snapshot().canonical);
    let doc2 = doc1
        .apply(&txn(
            vec![replace(
                id1,
                "This is a EXAM now foo bar baz",
                "This is a EXAM now foo bar QUUX",
                Some(g),
            )],
            MaterializationMode::TrackedChange,
        ))
        .expect("edit 2");

    let r = segs(&doc2.snapshot().canonical);
    assert!(
        r.iter().any(|(t, x, _)| *t == "del" && x == "test"),
        "edit 1's deletion of 'test' is still pending, got {r:?}"
    );
    assert!(
        r.iter().any(|(t, x, _)| *t == "del" && x == "baz"),
        "edit 2's deletion of 'baz' is pending, got {r:?}"
    );
    assert_eq!(
        para_text(
            &doc2
                .project(Resolution::AcceptAll)
                .unwrap()
                .snapshot()
                .canonical
        ),
        "This is a EXAM now foo bar QUUX",
        "accept-all = both edits applied"
    );
    assert_eq!(
        para_text(
            &doc2
                .project(Resolution::RejectAll)
                .unwrap()
                .snapshot()
                .canonical
        ),
        "This is a test now foo bar baz",
        "reject-all = pristine original — edit 1 was NOT accepted by edit 2"
    );
}

#[test]
fn reedit_by_another_author_preserves_their_pending_change() {
    // Author B makes a tracked change; author A then edits elsewhere in the same
    // paragraph. B's change must stay pending AND attributed to B — never accepted
    // for A, never re-attributed.
    let doc0 = Document::parse(&make_para_docx("This is a test now foo bar baz")).unwrap();
    let id = first_block_id(&doc0.snapshot().canonical);
    let doc1 = doc0
        .apply(&txn_by(
            vec![replace(
                id,
                "This is a test now foo bar baz",
                "This is a EXAM now foo bar baz",
                None,
            )],
            MaterializationMode::TrackedChange,
            "Author B",
        ))
        .expect("B's edit");
    let g = block_guard(&doc1.snapshot().canonical.blocks[0].block);
    let id1 = first_block_id(&doc1.snapshot().canonical);
    let doc2 = doc1
        .apply(&txn_by(
            vec![replace(
                id1,
                "This is a EXAM now foo bar baz",
                "This is a EXAM now foo bar QUUX",
                Some(g),
            )],
            MaterializationMode::TrackedChange,
            "Author A",
        ))
        .expect("A's edit");

    let r = segs(&doc2.snapshot().canonical);
    assert!(
        r.iter()
            .any(|(t, x, a)| *t == "del" && x == "test" && a.as_deref() == Some("Author B")),
        "B's deletion of 'test' stays pending and attributed to B, got {r:?}"
    );
    assert!(
        r.iter()
            .any(|(t, x, a)| *t == "ins" && x == "EXAM" && a.as_deref() == Some("Author B")),
        "B's insertion of 'EXAM' stays attributed to B, got {r:?}"
    );
    assert!(
        r.iter()
            .any(|(t, x, a)| *t == "del" && x == "baz" && a.as_deref() == Some("Author A")),
        "A's deletion of 'baz' is attributed to A, got {r:?}"
    );
    assert_eq!(
        para_text(
            &doc2
                .project(Resolution::RejectAll)
                .unwrap()
                .snapshot()
                .canonical
        ),
        "This is a test now foo bar baz",
        "reject-all = pristine original"
    );
}
