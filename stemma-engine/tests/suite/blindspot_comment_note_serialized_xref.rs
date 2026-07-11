//! Blindspot regression: CommentDelete / EditNote serialized-output referential
//! integrity (validator rule I-XREF-003 + §17.13.4 / §17.11).
//!
//! The existing per-verb gates assert CommentDelete and EditNote against the
//! re-parsed (self-healing) IR only — never against the SERIALIZED package that
//! real Word consumes. This file closes that gap by running the post-
//! serialization validator (`stemma::docx_validate::validate_docx`) and a
//! byte-level marker scan on the emitted parts.
//!
//! Domain-correct expectations (encode the spec, not current behavior):
//!
//! - CommentDelete (§17.13.4.4/.5/.6): after deleting comment id N, the
//!   serialized package must contain NO `commentRangeStart`, `commentRangeEnd`,
//!   or `commentReference` carrying `w:id="N"`, and the validator must report
//!   zero I-XREF-003 dangling-reference findings and `!has_errors()`. A dangling
//!   commentRange marker makes Word raise a repair dialog.
//!
//! - EditNote (§17.11.3/.7 + §17.11.10/.2): editing a footnote's body is a
//!   body-text-only change; the `footnoteReference` w:id linkage from
//!   document.xml into footnotes.xml must stay intact — exactly one reference,
//!   still resolving to its story — and the validator must report
//!   `!has_errors()`.

use stemma::ExportOptions;
use stemma::api::Document;
use stemma::docx::DocxArchive;
use stemma::domain::{BlockNode, CanonDoc, NodeId, NoteType, RevisionInfo};
use stemma::edit::{EditStep, EditTransaction, MaterializationMode, NoteKind};

/// One-paragraph DOCX with NO pre-existing comments / notes.
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

fn create_comment_step(block_id: NodeId, expect: &str, body: &str) -> EditStep {
    EditStep::CommentCreate {
        block_id,
        expect: expect.to_string(),
        semantic_hash: None,
        body: body.to_string(),
        author: Some("Reviewer".to_string()),
        rationale: None,
    }
}

/// Count `w:id="N"`-carrying occurrences of a marker local-name in an XML blob.
/// We look for the marker tag followed (within the same element open tag) by the
/// target `w:id`. This is a deliberately literal scan of the SERIALIZED bytes —
/// the surface a dangling marker would survive into.
fn count_marker_for_id(xml: &str, marker: &str, id: &str) -> usize {
    let needle_id = format!(r#"w:id="{id}""#);
    let mut count = 0;
    let mut search_from = 0;
    while let Some(rel) = xml[search_from..].find(marker) {
        let tag_start = search_from + rel;
        // Find the end of this element's open tag.
        let tag_end = match xml[tag_start..].find('>') {
            Some(e) => tag_start + e,
            None => break,
        };
        if xml[tag_start..=tag_end].contains(&needle_id) {
            count += 1;
        }
        search_from = tag_end + 1;
    }
    count
}

// =============================================================================
// CommentDelete — serialized package must carry NO dangling markers for the
// deleted id (§17.13.4 + I-XREF-003).
// =============================================================================
#[test]
fn comment_delete_leaves_no_dangling_markers_in_serialized_package() {
    let base = Document::parse(&make_docx("The Effective Date governs the term.")).expect("parse");
    let block_id = first_block_id(&base.snapshot().canonical);

    // Author a comment, then delete it.
    let created = base
        .apply(&txn(vec![create_comment_step(
            block_id,
            "Effective Date",
            "Define this term.",
        )]))
        .expect("create");
    let cid = created.snapshot().canonical.comments[0].id.clone();

    let deleted = created
        .apply(&txn(vec![EditStep::CommentDelete {
            comment_id: cid.clone(),
            rationale: None,
        }]))
        .expect("delete");

    // The IR must no longer carry the comment story (sanity, not the point).
    assert!(
        deleted
            .snapshot()
            .canonical
            .comments
            .iter()
            .all(|c| c.id != cid),
        "deleted comment story still present in IR"
    );

    let bytes = deleted
        .serialize(&ExportOptions::default())
        .expect("serialize");

    // (1) The package validator: zero I-XREF-003 dangling-reference findings and
    // no errors of any rule.
    let validation = stemma::docx_validate::validate_docx(&bytes);
    let xref003: Vec<String> = validation
        .findings
        .iter()
        .filter(|f| f.rule_id == "I-XREF-003")
        .map(|f| f.to_string())
        .collect();
    assert!(
        xref003.is_empty(),
        "I-XREF-003 dangling commentReference after delete: {xref003:?}"
    );
    assert!(
        !validation.has_errors(),
        "validator reported errors after CommentDelete: {:?}",
        validation
            .findings
            .iter()
            .map(|f| f.to_string())
            .collect::<Vec<_>>()
    );

    // (2) Byte-level: NO commentRangeStart / commentRangeEnd / commentReference
    // for the deleted id survives in ANY story part. A dangling commentRange
    // marker (which I-XREF-003 does NOT check) makes Word raise a repair dialog.
    let archive = DocxArchive::read(&bytes).expect("read");
    for part in [
        "word/document.xml",
        "word/footnotes.xml",
        "word/endnotes.xml",
        "word/header1.xml",
        "word/footer1.xml",
    ] {
        let Some(part_bytes) = archive.get(part) else {
            continue;
        };
        let xml = String::from_utf8_lossy(part_bytes);
        for marker in ["commentRangeStart", "commentRangeEnd", "commentReference"] {
            let n = count_marker_for_id(&xml, marker, &cid);
            assert_eq!(
                n, 0,
                "dangling {marker} w:id=\"{cid}\" survived in {part} after CommentDelete \
                 (count={n}); Word would raise a repair dialog. Part XML:\n{xml}"
            );
        }
    }

    // (3) If comments.xml is still emitted, it must not define the deleted id.
    if let Some(cx) = archive.get("word/comments.xml") {
        let cx = String::from_utf8_lossy(cx);
        assert!(
            !cx.contains(&format!(r#"w:id="{cid}""#)),
            "deleted comment id={cid} still defined in comments.xml:\n{cx}"
        );
    }
}

// =============================================================================
// EditNote — editing a footnote body must preserve the reference linkage
// (§17.11.3/.7 + §17.11.10/.2).
// =============================================================================
#[test]
fn edit_note_preserves_reference_linkage_in_serialized_package() {
    let base =
        Document::parse(&make_docx("The clause has a footnote marker here.")).expect("parse");
    let block_id = first_block_id(&base.snapshot().canonical);

    // Insert a footnote, then edit its body.
    let inserted = base
        .apply(&txn(vec![EditStep::InsertNote {
            block_id,
            expect: "footnote".to_string(),
            semantic_hash: None,
            note_kind: NoteKind::Footnote,
            body: "Original footnote body.".to_string(),
            rationale: None,
        }]))
        .expect("insert");

    let note_id = inserted
        .snapshot()
        .canonical
        .footnotes
        .iter()
        .find(|f| matches!(f.note_type, NoteType::Normal))
        .expect("authored footnote")
        .id
        .clone();

    // Commit the insert (accept-all) before editing: EditNote in TrackedChange
    // mode refuses to stack a second tracked change onto a story that still
    // carries its own pending InsertNote status (CLAUDE.md "no silent
    // fallbacks" — no silent layering). This mirrors the realistic workflow of
    // editing an EXISTING, already-committed footnote.
    let committed = inserted
        .project(stemma::Resolution::AcceptAll)
        .expect("accept the insert");

    let edited = committed
        .apply(&txn(vec![EditStep::EditNote {
            note_id: note_id.clone(),
            note_kind: NoteKind::Footnote,
            body: "Revised footnote body with new wording.".to_string(),
            rationale: None,
        }]))
        .expect("edit note");

    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");

    // (1) Validator: no structural / xref errors introduced by the edit.
    let validation = stemma::docx_validate::validate_docx(&bytes);
    assert!(
        !validation.has_errors(),
        "validator reported errors after EditNote: {:?}",
        validation
            .findings
            .iter()
            .map(|f| f.to_string())
            .collect::<Vec<_>>()
    );

    let archive = DocxArchive::read(&bytes).expect("read");
    let doc_xml = String::from_utf8_lossy(archive.get("word/document.xml").expect("document.xml"));
    let footnotes_xml =
        String::from_utf8_lossy(archive.get("word/footnotes.xml").expect("footnotes.xml"));

    // (2) The reference linkage is intact: exactly one footnoteReference in the
    // body (the edit touches body text only, never the reference run), and its
    // w:id still resolves to the footnote story.
    assert_eq!(
        doc_xml.matches("footnoteReference").count(),
        1,
        "EditNote must not duplicate or drop the footnoteReference run; \
         document.xml:\n{doc_xml}"
    );
    assert_eq!(
        count_marker_for_id(&doc_xml, "footnoteReference", &note_id),
        1,
        "the footnoteReference w:id=\"{note_id}\" must survive the edit; \
         document.xml:\n{doc_xml}"
    );
    // The story for that id is still present and carries its footnoteRef marker.
    assert!(
        count_marker_for_id(&footnotes_xml, "<w:footnote ", &note_id) >= 1
            || count_marker_for_id(&footnotes_xml, "<w:footnote", &note_id) >= 1,
        "footnote story w:id=\"{note_id}\" must still be defined; \
         footnotes.xml:\n{footnotes_xml}"
    );

    // (3) TrackedChange EditNote is a SURGICAL word-diff (not a whole-paragraph
    // rebuild): the raw XML carries the changed word as a real `w:del`/
    // `w:delText` (the OLD word) alongside a `w:ins` (the NEW words), NOT a
    // single flat run with the whole new sentence. Assert on the tracked
    // carriers directly, then on the RESOLVED (accept/reject) text.
    assert!(
        footnotes_xml.contains("<w:del")
            && footnotes_xml.contains("<w:delText>Original</w:delText>"),
        "the removed word must be a tracked w:del/w:delText, not silently dropped:\n{footnotes_xml}"
    );
    assert!(
        footnotes_xml.contains("<w:ins") && footnotes_xml.contains("Revised"),
        "the new word must be a tracked w:ins, not baked in untracked:\n{footnotes_xml}"
    );

    let accepted_bytes = edited
        .project(stemma::Resolution::AcceptAll)
        .expect("accept all")
        .serialize(&ExportOptions::default())
        .expect("serialize accepted");
    let accepted_footnotes = String::from_utf8_lossy(
        DocxArchive::read(&accepted_bytes)
            .expect("read accepted")
            .get("word/footnotes.xml")
            .expect("footnotes.xml"),
    )
    .into_owned();
    // The accepted text may be split across several `<w:r><w:t>` runs (the
    // surgical diff only merges adjacent SAME-status text, it does not force
    // everything back into one run) — so assert on ordered substrings, not one
    // contiguous string. It must also still carry the footnoteRef decoration.
    assert!(
        accepted_footnotes.contains("<w:footnoteRef"),
        "accept-all must keep the footnoteRef decoration:\n{accepted_footnotes}"
    );
    for expected in ["Revised", "footnote body", "with new wording"] {
        assert!(
            accepted_footnotes.contains(expected),
            "accept-all must contain {expected:?}:\n{accepted_footnotes}"
        );
    }
    assert!(
        !accepted_footnotes.contains("Original"),
        "accept-all must not leave the old word behind:\n{accepted_footnotes}"
    );

    let rejected_bytes = edited
        .project(stemma::Resolution::RejectAll)
        .expect("reject all")
        .serialize(&ExportOptions::default())
        .expect("serialize rejected");
    let rejected_footnotes = String::from_utf8_lossy(
        DocxArchive::read(&rejected_bytes)
            .expect("read rejected")
            .get("word/footnotes.xml")
            .expect("footnotes.xml"),
    )
    .into_owned();
    assert!(
        rejected_footnotes.contains("<w:footnoteRef"),
        "reject-all must keep the footnoteRef decoration:\n{rejected_footnotes}"
    );
    for expected in ["Original", "footnote body"] {
        assert!(
            rejected_footnotes.contains(expected),
            "reject-all must restore {expected:?}:\n{rejected_footnotes}"
        );
    }
    assert!(
        !rejected_footnotes.contains("Revised") && !rejected_footnotes.contains("with new wording"),
        "reject-all must not leave any of the new text behind:\n{rejected_footnotes}"
    );

    // (4) Re-import must still see exactly one authored footnote story.
    let reimported = Document::parse(&bytes).expect("reparse");
    let normal = reimported
        .snapshot()
        .canonical
        .footnotes
        .iter()
        .filter(|f| matches!(f.note_type, NoteType::Normal))
        .count();
    assert_eq!(
        normal, 1,
        "exactly one authored footnote after EditNote roundtrip"
    );
}
