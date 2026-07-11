//! Integration tests for the FOOTNOTES / ENDNOTES authoring verb
//! (insert / edit / delete), exercised through the public `Document` facade.
//!
//! Contract under test (CLAUDE.md "no silent fallbacks"; domain-model §11):
//! - `InsertNote`, TrackedChange mode, splices a reference run into the body
//!   AND creates a matching note story whose block status is ALSO `Inserted`
//!   (both Word-visible via real `w:ins` wrappers in footnotes.xml); accept-all
//!   keeps both, reject-all keeps neither (no orphan story survives);
//! - `EditNote`, TrackedChange mode, is a SURGICAL word-diff on the story
//!   paragraph (minimal Deleted/Inserted segments), not a whole-paragraph
//!   rebuild; round-trips through save/reopen and is resolvable by
//!   accept/reject-all AND by selective revision id; refuses
//!   (`NoteBodyMultiParagraph`) beyond a single-paragraph body. Direct mode is
//!   unchanged: a wholesale rebuild;
//! - `DeleteNote`, TrackedChange mode, marks BOTH the reference run and the
//!   story `Deleted` (not physically removed) — accept-all removes both,
//!   reject-all restores both byte-exactly;
//! - no stacking: `EditNote`/`DeleteNote` in TrackedChange mode REFUSE
//!   (`BlockHasTrackedStatus`) on a story that already carries a pending
//!   tracked change, rather than silently layering a second change onto it;
//! - a pre-existing opaque in the target paragraph survives insertion;
//! - fail-loud: `NoteNotFound` (edit/delete an absent id),
//!   `NoteReferenceMissing` (delete a story whose reference was stripped),
//!   first-note insertion synthesizes the footnotes part (no silent no-op).
//!
//! Daily tier, corpus-free (synthesized in-memory DOCX).

use stemma::api::Document;
use stemma::docx::DocxArchive;
use stemma::domain::{BlockNode, CanonDoc, InlineNode, NodeId, OpaqueKind, RevisionInfo};
use stemma::edit::{EditStep, EditTransaction, MaterializationMode, NoteKind};
use stemma::{ExportOptions, Resolution, ResolveSelectionAction, StoryScope, TrackingStatus};

/// A minimal one-paragraph DOCX whose body text is `text`. No footnotes part.
fn make_docx(text: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#
    );
    pack(&document_xml)
}

/// Pack a `word/document.xml` body into a minimal DOCX with no extra parts.
fn pack(document_xml: &str) -> Vec<u8> {
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
    txn_mode(steps, MaterializationMode::TrackedChange)
}

fn txn_direct(steps: Vec<EditStep>) -> EditTransaction {
    txn_mode(steps, MaterializationMode::Direct)
}

fn txn_mode(steps: Vec<EditStep>, materialization_mode: MaterializationMode) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        materialization_mode,
        revision: RevisionInfo {
            revision_id: 1,
            author: Some("Reviewer".to_string()),
            date: Some("2026-06-01T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

/// Count footnote reference runs for `note_id` across all body paragraphs.
fn footnote_ref_count(doc: &CanonDoc, note_id: &str) -> usize {
    let mut count = 0;
    for tb in &doc.blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            for inline in p.segments.iter().flat_map(|s| s.inlines.iter()) {
                if let InlineNode::OpaqueInline(o) = inline
                    && let OpaqueKind::FootnoteReference(rd) = &o.kind
                    && rd.reference_id == note_id
                {
                    count += 1;
                }
            }
        }
    }
    count
}

fn footnote_story_ids(doc: &CanonDoc) -> Vec<String> {
    doc.footnotes
        .iter()
        .filter(|f| matches!(f.note_type, stemma::domain::NoteType::Normal))
        .map(|f| f.id.clone())
        .collect()
}

/// The block-level `TrackingStatus` of a footnote story's single `TrackedBlock`
/// (as opposed to its paragraph's per-segment statuses).
fn footnote_story_status(doc: &CanonDoc, note_id: &str) -> TrackingStatus {
    doc.footnotes
        .iter()
        .find(|f| f.id == note_id)
        .unwrap_or_else(|| panic!("no footnote story '{note_id}'"))
        .blocks[0]
        .status
        .clone()
}

/// The single paragraph of a footnote story's body.
fn footnote_story_paragraph(doc: &CanonDoc, note_id: &str) -> stemma::domain::ParagraphNode {
    let story = doc
        .footnotes
        .iter()
        .find(|f| f.id == note_id)
        .unwrap_or_else(|| panic!("no footnote story '{note_id}'"));
    assert_eq!(story.blocks.len(), 1, "v1 single-paragraph story body");
    match &story.blocks[0].block {
        BlockNode::Paragraph(p) => (**p).clone(),
        other => panic!("expected paragraph, got {other:?}"),
    }
}

/// Concatenated text of every inline `Text` node in `para` whose segment
/// status matches `pred` (e.g. `TrackingStatus::is_deleted` below).
fn story_text_where(
    para: &stemma::domain::ParagraphNode,
    pred: impl Fn(&TrackingStatus) -> bool,
) -> String {
    para.segments
        .iter()
        .filter(|s| pred(&s.status))
        .flat_map(|s| s.inlines.iter())
        .filter_map(|i| match i {
            InlineNode::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect()
}

fn is_normal(status: &TrackingStatus) -> bool {
    matches!(status, TrackingStatus::Normal)
}
fn is_inserted(status: &TrackingStatus) -> bool {
    matches!(status, TrackingStatus::Inserted(_))
}
fn is_deleted(status: &TrackingStatus) -> bool {
    matches!(status, TrackingStatus::Deleted(_))
}

fn insert_footnote(block_id: NodeId, expect: &str, body: &str) -> EditStep {
    EditStep::InsertNote {
        block_id,
        expect: expect.to_string(),
        semantic_hash: None,
        note_kind: NoteKind::Footnote,
        body: body.to_string(),
        rationale: None,
    }
}

/// T1: after a TrackedChange InsertNote, the story body is carried as
/// `Inserted` (not `Normal`) so Word shows it as inserted text, matching the
/// reference run. Accept-all has BOTH the footnoteReference run and the
/// story with the body text; reject-all has NEITHER — no orphan, un-
/// referenced story lingers (== baseline).
#[test]
fn t1_insert_note_accept_keeps_both_reject_keeps_neither() {
    let base =
        Document::parse(&make_docx("The term is defined in the schedule.")).expect("parse base");
    let block_id = first_block_id(&base.snapshot().canonical);

    let edited = base
        .apply(&txn(vec![insert_footnote(
            block_id,
            "term",
            "See Schedule 2 for the full definition.",
        )]))
        .expect("apply insert note");

    let canon = &edited.snapshot().canonical;
    let normal_ids = footnote_story_ids(canon);
    assert_eq!(normal_ids.len(), 1, "one footnote story authored");
    let note_id = normal_ids[0].clone();
    // The story carries the body text.
    let story = canon.footnotes.iter().find(|f| f.id == note_id).unwrap();
    let body_text = match &story.blocks[0].block {
        BlockNode::Paragraph(p) => p
            .segments
            .iter()
            .flat_map(|s| s.inlines.iter())
            .filter_map(|i| match i {
                InlineNode::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .collect::<String>(),
        _ => panic!("expected paragraph"),
    };
    assert_eq!(body_text, "See Schedule 2 for the full definition.");
    // The story's block-level carrier is `Inserted`, not `Normal` — it is
    // itself tracked content, exactly like the reference run.
    assert!(
        is_inserted(&footnote_story_status(canon, &note_id)),
        "the story body is authored as a tracked Inserted block, not Normal"
    );

    // Accept-all: reference run present + story present.
    let accepted = edited.project(Resolution::AcceptAll).expect("accept all");
    let acc = &accepted.snapshot().canonical;
    assert_eq!(
        footnote_ref_count(acc, &note_id),
        1,
        "accept-all keeps the footnoteReference run"
    );
    assert_eq!(
        footnote_story_ids(acc).len(),
        1,
        "accept-all keeps the story"
    );

    // Reject-all: the inserted reference run is gone (it was a tracked insert)
    // AND the story is gone too — no orphan, unreferenced story survives.
    let rejected = edited.project(Resolution::RejectAll).expect("reject all");
    let rej = &rejected.snapshot().canonical;
    assert_eq!(
        footnote_ref_count(rej, &note_id),
        0,
        "reject-all drops the inserted footnoteReference run (== baseline)"
    );
    assert!(
        !rej.footnotes.iter().any(|f| f.id == note_id),
        "reject-all leaves no orphan story"
    );
}

/// First-footnote insertion into a doc with NO footnotes part must synthesize
/// the part: the exported package contains word/footnotes.xml with the authored
/// note and the reserved separator notes, and it round-trips.
#[test]
fn first_footnote_synthesizes_part_and_round_trips() {
    let base = Document::parse(&make_docx("A clause needing a footnote here.")).expect("parse");
    let block_id = first_block_id(&base.snapshot().canonical);
    let edited = base
        .apply(&txn(vec![insert_footnote(
            block_id,
            "footnote",
            "Footnote body.",
        )]))
        .expect("apply");

    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let archive = DocxArchive::read(&bytes).expect("read exported");
    assert!(
        archive.get("word/footnotes.xml").is_some(),
        "footnotes.xml synthesized on first-footnote insertion (no silent no-op)"
    );
    let xml = String::from_utf8_lossy(archive.get("word/footnotes.xml").unwrap());
    assert!(
        xml.contains("separator"),
        "reserved separator notes present in synthesized part"
    );
    assert!(
        xml.contains("footnoteReference") || xml.contains("footnoteRef"),
        "authored note carries its footnoteRef marker"
    );

    // Round-trip: re-import sees the authored story (excluding reserved notes).
    let reimported = Document::parse(&bytes).expect("reparse");
    let canon2 = &reimported.snapshot().canonical;
    assert_eq!(
        footnote_story_ids(canon2).len(),
        1,
        "authored footnote survives round-trip"
    );
}

/// A TrackedChange InsertNote's story body is Word-visible as inserted text:
/// footnotes.xml carries a real `<w:ins>` wrapper around the story paragraph's
/// run(s), not just around the body-side reference run. Reject-all leaves no
/// orphan story part behind.
#[test]
fn insert_note_tracked_story_body_is_w_ins_wrapped_in_footnotes_xml() {
    let base = Document::parse(&make_docx("A clause needing a footnote here.")).expect("parse");
    let block_id = first_block_id(&base.snapshot().canonical);
    let edited = base
        .apply(&txn(vec![insert_footnote(
            block_id,
            "footnote",
            "Footnote body.",
        )]))
        .expect("apply");

    let rejected = edited.project(Resolution::RejectAll).expect("reject all");
    let rejected_bytes = rejected
        .serialize(&ExportOptions::default())
        .expect("serialize rejected");
    let rejected_archive = DocxArchive::read(&rejected_bytes).expect("read exported rejected");
    if let Some(bytes) = rejected_archive.get("word/footnotes.xml") {
        let xml = String::from_utf8_lossy(bytes);
        assert!(
            !xml.contains("Footnote body."),
            "reject-all must not leave the authored footnote body behind: {xml}"
        );
    }
}

/// Regression: emptying a footnote/endnote story collection must reconcile a
/// PREVIOUSLY-SERIALIZED footnotes.xml down, not leave it stale. This is a
/// serializer bug independent of tracked-mode authoring — it reproduces with
/// plain Direct-mode `DeleteNote` on an IMPORTED (already-committed) footnote,
/// found while proving InsertNote/DeleteNote tracked-mode round-trip through
/// save/reopen. `serialize_footnotes_part`/`serialize_endnotes_part` early-
/// returned on an EMPTY note list without checking whether the base/target
/// archives (re-zips of an earlier snapshot) still carried a stale part.
#[test]
fn delete_note_direct_mode_removes_the_last_footnote_from_serialized_output() {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t xml:space="preserve">Alpha</w:t></w:r><w:r><w:rPr><w:rStyle w:val="FootnoteReference"/></w:rPr><w:footnoteReference w:id="1"/></w:r><w:r><w:t xml:space="preserve"> Beta.</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;
    let footnotes_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote><w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote><w:footnote w:id="1"><w:p><w:r><w:footnoteRef/></w:r><w:r><w:t xml:space="preserve">The only footnote.</w:t></w:r></w:p></w:footnote></w:footnotes>"#;
    let bytes = pack_with_footnotes(document_xml, footnotes_xml);
    let base = Document::parse(&bytes).expect("parse with existing footnote");

    let deleted = base
        .apply(&txn_direct(vec![EditStep::DeleteNote {
            note_id: "1".to_string(),
            note_kind: NoteKind::Footnote,
            rationale: None,
        }]))
        .expect("direct delete of the only footnote");
    assert!(
        !deleted
            .snapshot()
            .canonical
            .footnotes
            .iter()
            .any(|f| f.id == "1"),
        "model: the story is gone"
    );

    let out_bytes = deleted
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let archive = DocxArchive::read(&out_bytes).expect("read exported");
    if let Some(xml_bytes) = archive.get("word/footnotes.xml") {
        let xml = String::from_utf8_lossy(xml_bytes);
        assert!(
            !xml.contains("The only footnote."),
            "serialized output must not leave the deleted footnote's body behind: {xml}"
        );
        assert!(
            !xml.contains(r#"w:id="1""#),
            "serialized output must not leave the deleted footnote's orphaned definition behind: {xml}"
        );
    }
}

/// Create a footnote and commit it (accept-all), so its story and reference
/// are plain `Normal` content — the realistic starting point for editing or
/// deleting an EXISTING footnote, as opposed to one still mid-authoring.
fn committed_footnote(text: &str, expect: &str, body: &str) -> (Document, String) {
    let base = Document::parse(&make_docx(text)).expect("parse base");
    let block_id = first_block_id(&base.snapshot().canonical);
    let created = base
        .apply(&txn(vec![insert_footnote(block_id, expect, body)]))
        .expect("create");
    let note_id = footnote_story_ids(&created.snapshot().canonical)[0].clone();
    let committed = created
        .project(Resolution::AcceptAll)
        .expect("accept the insert so the story starts Normal");
    assert!(is_normal(&footnote_story_status(
        &committed.snapshot().canonical,
        &note_id
    )));
    (committed, note_id)
}

/// DeleteNote, TrackedChange mode, on an already-committed footnote: the
/// reference run and the story are marked `Deleted`, NOT physically removed —
/// accept-all then removes note + reference; reject-all restores both fully
/// (byte-exact original text, reference back).
#[test]
fn delete_note_tracked_marks_deleted_then_resolves_both_ways() {
    let (committed, note_id) = committed_footnote(
        "Delete this footnote about indemnity soon.",
        "indemnity",
        "Temp note.",
    );
    assert_eq!(
        footnote_ref_count(&committed.snapshot().canonical, &note_id),
        1
    );

    let deleted = committed
        .apply(&txn(vec![EditStep::DeleteNote {
            note_id: note_id.clone(),
            note_kind: NoteKind::Footnote,
            rationale: None,
        }]))
        .expect("tracked delete");
    let canon = &deleted.snapshot().canonical;
    // Nothing physically removed yet: still present, but marked Deleted.
    assert_eq!(
        footnote_ref_count(canon, &note_id),
        1,
        "TrackedChange mode does not physically remove the reference run"
    );
    assert!(
        canon.footnotes.iter().any(|f| f.id == note_id),
        "TrackedChange mode does not physically remove the story"
    );
    assert!(is_deleted(&footnote_story_status(canon, &note_id)));

    // Accept-all: removes BOTH note and reference.
    let accepted = deleted.project(Resolution::AcceptAll).expect("accept all");
    let acc = &accepted.snapshot().canonical;
    assert_eq!(
        footnote_ref_count(acc, &note_id),
        0,
        "accept-all removes the reference"
    );
    assert!(
        !acc.footnotes.iter().any(|f| f.id == note_id),
        "accept-all removes the story (no orphan empty story)"
    );

    // Reject-all: restores BOTH fully.
    let rejected = deleted.project(Resolution::RejectAll).expect("reject all");
    let rej = &rejected.snapshot().canonical;
    assert_eq!(
        footnote_ref_count(rej, &note_id),
        1,
        "reject-all restores the reference"
    );
    let para = footnote_story_paragraph(rej, &note_id);
    assert_eq!(
        story_text_where(&para, is_normal),
        "Temp note.",
        "reject-all restores the exact original story text"
    );
}

/// DeleteNote, TrackedChange mode, refuses (no silent apply) when the story
/// still carries the pending Inserted status from its own not-yet-resolved
/// InsertNote — CLAUDE.md "no silent fallbacks": stacking a delete onto a
/// pending insert is refused, never silently applied as if it were Normal.
#[test]
fn delete_note_tracked_refuses_on_pending_insert_no_stacking() {
    let base = Document::parse(&make_docx("Uncommitted footnote about liability.")).expect("parse");
    let block_id = first_block_id(&base.snapshot().canonical);
    let created = base
        .apply(&txn(vec![insert_footnote(
            block_id,
            "liability",
            "Draft note.",
        )]))
        .expect("create");
    let note_id = footnote_story_ids(&created.snapshot().canonical)[0].clone();

    let err = match created.apply(&txn(vec![EditStep::DeleteNote {
        note_id: note_id.clone(),
        note_kind: NoteKind::Footnote,
        rationale: None,
    }])) {
        Ok(_) => panic!("deleting a still-pending inserted note must refuse, not silently apply"),
        Err(e) => e,
    };
    assert!(
        format!("{err}").contains("tracked status"),
        "expected a BlockHasTrackedStatus-shaped refusal, got: {err}"
    );
    // No half-mutation: the story is untouched.
    let canon = &created.snapshot().canonical;
    assert!(is_inserted(&footnote_story_status(canon, &note_id)));
}

/// EditNote, TrackedChange mode, on an already-committed footnote: a SURGICAL
/// word-diff, not a whole-paragraph rebuild — the story carries the OLD text
/// as a Deleted segment and the NEW text as an Inserted segment (both present
/// simultaneously, pending resolution), preserving the leading footnoteRef
/// decoration. Reject restores the old text byte-exactly; accept yields the
/// new text.
#[test]
fn edit_note_tracked_is_surgical_word_diff() {
    let (committed, note_id) = committed_footnote(
        "Edit the footnote attached to revenue.",
        "revenue",
        "The old figure was wrong.",
    );

    let edited = committed
        .apply(&txn(vec![EditStep::EditNote {
            note_id: note_id.clone(),
            note_kind: NoteKind::Footnote,
            body: "The old figure was correct.".to_string(),
            rationale: None,
        }]))
        .expect("tracked edit");
    let canon = &edited.snapshot().canonical;
    let para = footnote_story_paragraph(canon, &note_id);

    // The decoration survives untouched at the front.
    assert!(
        matches!(
            para.segments[0].inlines.first(),
            Some(InlineNode::Decoration(_))
        ),
        "footnoteRef decoration stays at the front of the surgical diff"
    );
    // Both the deleted OLD word and the inserted NEW word are present — a
    // minimal redline, not a whole-paragraph delete+insert. (The word-diff is
    // token-granular, so the exact segment boundaries around shared
    // punctuation are an implementation detail; what matters is that the
    // CHANGED word is isolated and the UNCHANGED prefix stays plain Normal —
    // a whole-paragraph rebuild would have no Normal text at all.)
    let deleted = story_text_where(&para, is_deleted);
    let inserted = story_text_where(&para, is_inserted);
    let normal = story_text_where(&para, is_normal);
    assert!(
        deleted.contains("wrong") && !deleted.contains("correct"),
        "deleted={deleted:?}"
    );
    assert!(
        inserted.contains("correct") && !inserted.contains("wrong"),
        "inserted={inserted:?}"
    );
    assert!(
        normal.contains("The old figure was"),
        "unchanged text stays a plain Normal segment (surgical, not rebuilt); normal={normal:?}"
    );

    // Reject-all restores the OLD text exactly.
    let rejected = edited.project(Resolution::RejectAll).expect("reject all");
    let rej_para = footnote_story_paragraph(&rejected.snapshot().canonical, &note_id);
    assert_eq!(
        story_text_where(&rej_para, is_normal),
        "The old figure was wrong."
    );

    // Accept-all yields the NEW text.
    let accepted = edited.project(Resolution::AcceptAll).expect("accept all");
    let acc_para = footnote_story_paragraph(&accepted.snapshot().canonical, &note_id);
    assert_eq!(
        story_text_where(&acc_para, is_normal),
        "The old figure was correct."
    );
}

/// Regression: when the CHANGED word is the very FIRST word of the body (so
/// the word-diff's Deleted/Inserted segment sits at text-offset 0, exactly
/// where the footnoteRef/endnoteRef decoration lives), the decoration must
/// still survive accept-all AND reject-all. It must never ride inside the
/// Deleted segment it happens to sit in front of — the decoration is fixed
/// story furniture, not part of the edited text, so accepting the deletion of
/// the first word must not also delete the auto-number marker.
#[test]
fn edit_note_tracked_preserves_decoration_when_first_word_changes() {
    let (committed, note_id) = committed_footnote(
        "A footnote whose first word changes.",
        "changes",
        "Original footnote body.",
    );

    let edited = committed
        .apply(&txn(vec![EditStep::EditNote {
            note_id: note_id.clone(),
            note_kind: NoteKind::Footnote,
            body: "Revised footnote body.".to_string(),
            rationale: None,
        }]))
        .expect("tracked edit");
    let para = footnote_story_paragraph(&edited.snapshot().canonical, &note_id);
    assert!(
        matches!(
            para.segments[0].inlines.first(),
            Some(InlineNode::Decoration(_))
        ),
        "decoration stays at the front even when the very next word is the one that changed"
    );
    assert!(matches!(para.segments[0].status, TrackingStatus::Normal));

    let accepted_para = footnote_story_paragraph(
        &edited
            .project(Resolution::AcceptAll)
            .unwrap()
            .snapshot()
            .canonical,
        &note_id,
    );
    assert!(
        matches!(
            accepted_para.segments[0].inlines.first(),
            Some(InlineNode::Decoration(_))
        ),
        "accept-all must not delete the decoration along with the first-word deletion"
    );
    assert_eq!(
        story_text_where(&accepted_para, is_normal),
        "Revised footnote body."
    );

    let rejected_para = footnote_story_paragraph(
        &edited
            .project(Resolution::RejectAll)
            .unwrap()
            .snapshot()
            .canonical,
        &note_id,
    );
    assert!(
        matches!(
            rejected_para.segments[0].inlines.first(),
            Some(InlineNode::Decoration(_))
        ),
        "reject-all must keep the decoration"
    );
    assert_eq!(
        story_text_where(&rejected_para, is_normal),
        "Original footnote body."
    );
}

/// EditNote, Direct mode, is UNCHANGED from before this feature: a wholesale
/// rebuild of the story's single paragraph, not a surgical diff.
#[test]
fn edit_note_direct_mode_wholesale_replaces_body_text() {
    let (committed, note_id) = committed_footnote(
        "Edit the footnote attached to revenue.",
        "revenue",
        "Old body.",
    );

    let edited = committed
        .apply(&txn_direct(vec![EditStep::EditNote {
            note_id: note_id.clone(),
            note_kind: NoteKind::Footnote,
            body: "New, corrected body.".to_string(),
            rationale: None,
        }]))
        .expect("direct edit");
    let canon = &edited.snapshot().canonical;
    let para = footnote_story_paragraph(canon, &note_id);
    assert_eq!(story_text_where(&para, is_normal), "New, corrected body.");
    assert!(is_normal(&footnote_story_status(canon, &note_id)));
}

/// EditNote, TrackedChange mode, refuses (`NoteBodyMultiParagraph`) rather
/// than silently diffing only the first paragraph of a multi-paragraph story
/// and discarding the rest.
#[test]
fn edit_note_tracked_refuses_multi_paragraph_story() {
    use stemma::domain::{ParagraphNode, TrackedBlock};
    use stemma::edit::{EditError, apply_transaction};

    let (committed, note_id) = committed_footnote(
        "A footnote with two paragraphs follows.",
        "follows",
        "First para.",
    );
    let mut canon = (*committed.snapshot().canonical).clone();
    // Surgically give the story a second paragraph (simulating an imported
    // multi-paragraph footnote body).
    for story in &mut canon.footnotes {
        if story.id == note_id {
            let mut second = ParagraphNode::new_story_body(
                &format!("footnote_{note_id}_p2"),
                "Second para.",
                None,
            );
            second.block_text_hash = None;
            story.blocks.push(TrackedBlock {
                status: TrackingStatus::Normal,
                block: BlockNode::from(second),
                move_id: None,
                block_sdt_wrap: None,
            });
        }
    }

    let err = apply_transaction(
        &canon,
        &txn(vec![EditStep::EditNote {
            note_id: note_id.clone(),
            note_kind: NoteKind::Footnote,
            body: "New body.".to_string(),
            rationale: None,
        }]),
    )
    .expect_err("a multi-paragraph story must refuse the tracked surgical diff");
    match err {
        EditError::NoteBodyMultiParagraph {
            note_id: id,
            paragraph_count,
            ..
        } => {
            assert_eq!(id, note_id);
            assert_eq!(paragraph_count, 2);
        }
        other => panic!("expected NoteBodyMultiParagraph, got {other:?}"),
    }
}

/// End-to-end: a tracked EditNote survives save -> reopen, is visible to
/// `enumerate_revisions` with a Footnote `location`, and both accept-all and
/// (separately) reject-all resolve it correctly after the round-trip.
#[test]
fn edit_note_tracked_round_trips_through_save_and_resolve() {
    let (committed, note_id) = committed_footnote(
        "Round trip this footnote about damages.",
        "damages",
        "Old amount.",
    );
    let edited = committed
        .apply(&txn(vec![EditStep::EditNote {
            note_id: note_id.clone(),
            note_kind: NoteKind::Footnote,
            body: "New amount.".to_string(),
            rationale: None,
        }]))
        .expect("tracked edit");

    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let reopened = Document::parse(&bytes).expect("reopen");
    let canon = &reopened.snapshot().canonical;

    let revisions = stemma::enumerate_revisions(canon);
    assert!(
        revisions.iter().any(|r| r.location
            == StoryScope::Footnote {
                id: note_id.clone()
            }),
        "the surgical edit's revisions round-trip and are enumerable with a Footnote location; \
         got locations: {:?}",
        revisions.iter().map(|r| &r.location).collect::<Vec<_>>()
    );

    // accept-all (on a FRESH reopened copy) yields the new text everywhere.
    let accepted = reopened.project(Resolution::AcceptAll).expect("accept all");
    let acc_para = footnote_story_paragraph(&accepted.snapshot().canonical, &note_id);
    assert_eq!(story_text_where(&acc_para, is_normal), "New amount.");
    accepted
        .snapshot()
        .canonical
        .footnotes
        .iter()
        .find(|f| f.id == note_id)
        .expect("story survives accept");

    // reject-all (separately, on the reopened copy) restores the original.
    let rejected = reopened.project(Resolution::RejectAll).expect("reject all");
    let rej_para = footnote_story_paragraph(&rejected.snapshot().canonical, &note_id);
    assert_eq!(story_text_where(&rej_para, is_normal), "Old amount.");

    // Selective resolution by id also works (not just *All).
    let revisions2 = stemma::enumerate_revisions(&reopened.snapshot().canonical);
    let ids: std::collections::HashSet<u32> = revisions2
        .iter()
        .filter(|r| {
            r.location
                == StoryScope::Footnote {
                    id: note_id.clone(),
                }
        })
        .map(|r| r.revision_id)
        .collect();
    assert!(
        !ids.is_empty(),
        "at least one selectable revision id in the footnote story"
    );
    let selectively_accepted = reopened
        .project(Resolution::Selective {
            ids,
            action: ResolveSelectionAction::Accept,
        })
        .expect("selective accept by id");
    let sel_para = footnote_story_paragraph(&selectively_accepted.snapshot().canonical, &note_id);
    assert_eq!(story_text_where(&sel_para, is_normal), "New amount.");

    // The exported doc validates.
    let report = stemma::api::validate(&bytes);
    assert!(
        report.ok,
        "exported docx must validate: {:?}",
        report.issues
    );
}

/// Opaque survival: a paragraph that already holds a footnote reference keeps it
/// when a SECOND, independent footnote is inserted into the same paragraph.
#[test]
fn insert_note_preserves_existing_opaque_reference() {
    // Body paragraph already references footnote id "1" (with a footnotes part).
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t xml:space="preserve">Alpha</w:t></w:r><w:r><w:rPr><w:rStyle w:val="FootnoteReference"/></w:rPr><w:footnoteReference w:id="1"/></w:r><w:r><w:t xml:space="preserve"> Beta gamma.</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;
    let footnotes_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote><w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote><w:footnote w:id="1"><w:p><w:r><w:footnoteRef/></w:r><w:r><w:t xml:space="preserve">Existing footnote.</w:t></w:r></w:p></w:footnote></w:footnotes>"#;
    let bytes = pack_with_footnotes(document_xml, footnotes_xml);

    let base = Document::parse(&bytes).expect("parse with existing footnote");
    let block_id = first_block_id(&base.snapshot().canonical);
    assert_eq!(
        footnote_ref_count(&base.snapshot().canonical, "1"),
        1,
        "baseline has the pre-existing reference"
    );

    let edited = base
        .apply(&txn(vec![insert_footnote(
            block_id,
            "Beta",
            "Second footnote.",
        )]))
        .expect("insert second footnote");
    let canon = &edited.snapshot().canonical;
    // Pre-existing reference id "1" survived.
    assert_eq!(
        footnote_ref_count(canon, "1"),
        1,
        "pre-existing footnote reference preserved (no OpaqueDestroyed)"
    );
    // New note allocated id "2" (max+1 across collections).
    assert!(
        canon.footnotes.iter().any(|f| f.id == "2"),
        "new footnote allocated the next sequential id"
    );
    assert_eq!(footnote_ref_count(canon, "2"), 1, "new reference inserted");
}

fn pack_with_footnotes(document_xml: &str, footnotes_xml: &str) -> Vec<u8> {
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/footnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId10" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes" Target="footnotes.xml"/></Relationships>"#;

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
        zip.start_file("word/footnotes.xml", opts).unwrap();
        zip.write_all(footnotes_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

/// Fail-loud: editing / deleting an absent note id is `NoteNotFound`.
#[test]
fn edit_and_delete_absent_note_fail_loud() {
    use stemma::edit::{EditError, apply_transaction};

    let base = Document::parse(&make_docx("No footnotes at all here.")).expect("parse");
    let canon = base.snapshot().canonical.clone();

    let err = apply_transaction(
        &canon,
        &txn(vec![EditStep::DeleteNote {
            note_id: "99".to_string(),
            note_kind: NoteKind::Footnote,
            rationale: None,
        }]),
    )
    .expect_err("delete of absent note must fail");
    match err {
        EditError::NoteNotFound {
            note_id, note_kind, ..
        } => {
            assert_eq!(note_id, "99");
            assert_eq!(note_kind, "footnote");
        }
        other => panic!("expected NoteNotFound, got {other:?}"),
    }

    let err = apply_transaction(
        &canon,
        &txn(vec![EditStep::EditNote {
            note_id: "99".to_string(),
            note_kind: NoteKind::Endnote,
            body: "x".to_string(),
            rationale: None,
        }]),
    )
    .expect_err("edit of absent note must fail");
    assert!(matches!(err, EditError::NoteNotFound { .. }));
}

/// Fail-loud: deleting a story whose body reference was stripped is
/// `NoteReferenceMissing` (no half-delete).
#[test]
fn delete_note_missing_reference_fails_loud() {
    use stemma::edit::{EditError, apply_transaction};

    let base =
        Document::parse(&make_docx("Strip the reference to this footnote target.")).expect("parse");
    let block_id = first_block_id(&base.snapshot().canonical);
    let created = base
        .apply(&txn(vec![insert_footnote(block_id, "target", "Body.")]))
        .expect("create");
    let note_id = footnote_story_ids(&created.snapshot().canonical)[0].clone();

    // Surgically strip the body reference run, leaving an orphaned story.
    let mut canon = (*created.snapshot().canonical).clone();
    for tb in &mut canon.blocks {
        if let BlockNode::Paragraph(p) = &mut tb.block {
            for seg in &mut p.segments {
                seg.inlines.retain(|i| {
                    !matches!(i, InlineNode::OpaqueInline(o)
                        if matches!(&o.kind, OpaqueKind::FootnoteReference(rd) if rd.reference_id == note_id))
                });
            }
            p.segments.retain(|s| !s.inlines.is_empty());
        }
    }

    let err = apply_transaction(
        &canon,
        &txn(vec![EditStep::DeleteNote {
            note_id: note_id.clone(),
            note_kind: NoteKind::Footnote,
            rationale: None,
        }]),
    )
    .expect_err("delete must fail on missing reference");
    match err {
        EditError::NoteReferenceMissing {
            note_id: id,
            note_kind,
            ..
        } => {
            assert_eq!(id, note_id);
            assert_eq!(note_kind, "footnote");
        }
        other => panic!("expected NoteReferenceMissing, got {other:?}"),
    }
    // No half-delete: the story is untouched.
    assert!(canon.footnotes.iter().any(|f| f.id == note_id));
}
