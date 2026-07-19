//! Selective / accept-all / reject-all resolution of a tracked deletion must
//! never orphan a range-marker half whose partner lives in a BODY-LEVEL marker.
//!
//! A bookmark / comment / permission range is a start/end pair joined by a
//! part-local id (ECMA-376 §17.13.6). The tracked-resolution paths already snap-
//! shot the story's whole pairs and collapse any the projection tears
//! (`tracked_model::collapse_resolution_torn_range_markers`) — but that repair
//! only saw markers modeled as paragraph inlines. A range marker that is a
//! DIRECT child of `w:body` (between paragraphs) is imported as a
//! verbatim-spliced `OpaqueBlock`; before the fix that block carried no id in the
//! model and was invisible to the repair. So a projection that removed the
//! paragraph holding the marker's INLINE partner left the body-level half
//! orphaned, and serialization refused the document ("serialization introduced
//! unpaired bookmarks … refusing to emit").
//!
//! The fix (position-preserving) keeps the body-level marker a spliced opaque
//! block — byte fidelity is unchanged — but records its family/id/role on the
//! block (`OpaqueBlockNode::range_marker`) so the repair pairs it with its inline
//! partner. When a resolution removes the partner, the collapse re-inserts it
//! adjacent to the surviving opaque block, collapsing the range to a point (the
//! same rule the inline path uses). The opaque half has `Normal` status and is
//! never removed, so it is always the survivor.
//!
//! These sentinels drive the exact resolution paths (Selective accept/reject of
//! each minted id, plus AcceptAll / RejectAll) and assert the output both
//! serializes AND re-imports with a balanced marker set — for the comment family
//! that catches the silent lone-half ship (its integrity check is a non-blocking
//! WARN). Corpus-free, daily tier: synthetic in-memory DOCX.

use std::collections::HashSet;
use std::io::Read;

use stemma::ExportOptions;
use stemma::api::Document;
use stemma::domain::{BlockNode, NodeId, RevisionInfo};
use stemma::edit::{EditStep, EditTransaction, MaterializationMode};
use stemma::tracked_model::{ResolveSelectionAction, enumerate_revisions};
use zip::ZipArchive;

fn pack(body_inner_xml: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_inner_xml}<w:sectPr/></w:body></w:document>"#
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

fn document_xml_of(docx: &[u8]) -> String {
    let mut zip = ZipArchive::new(std::io::Cursor::new(docx)).expect("open DOCX zip");
    let mut file = zip
        .by_name("word/document.xml")
        .expect("word/document.xml present");
    let mut out = String::new();
    file.read_to_string(&mut out).expect("read document.xml");
    out
}

fn para_ids(doc: &Document) -> Vec<NodeId> {
    doc.snapshot()
        .canonical
        .blocks
        .iter()
        .filter_map(|tb| match &tb.block {
            BlockNode::Paragraph(p) => Some(p.id.clone()),
            _ => None,
        })
        .collect()
}

/// Apply a tracked `DeleteBlockRange` over a single body paragraph.
fn tracked_delete_block(doc: &Document, block: &NodeId, expect: &str) -> Document {
    let txn = EditTransaction {
        steps: vec![EditStep::DeleteBlockRange {
            from_block_id: block.clone(),
            to_block_id: block.clone(),
            rationale: None,
            expect: expect.to_string(),
            semantic_hash: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("Reviewer".to_string()),
            date: Some("2026-07-10T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    };
    doc.apply(&txn).expect("apply tracked delete")
}

fn all_rev_ids(doc: &Document) -> HashSet<u32> {
    enumerate_revisions(&doc.snapshot().canonical)
        .into_iter()
        .map(|r| r.revision_id)
        .collect()
}

/// `(open_count, close_count)` of a start/end marker family in the XML.
fn marker_counts(xml: &str, open: &str, close: &str) -> (usize, usize) {
    (xml.matches(open).count(), xml.matches(close).count())
}

/// Assert the serialized document has a balanced marker set for every range
/// family and re-imports cleanly. `serialize` runs the blocking validator, so a
/// torn pair would already have refused here — this additionally pins that no
/// lone half slipped through a NON-blocking family check (e.g. comment ranges).
fn assert_clean(doc: &Document, ctx: &str) {
    let bytes = doc
        .serialize(&ExportOptions::default())
        .unwrap_or_else(|e| panic!("{ctx}: serialize refused: {e:?}"));
    let xml = document_xml_of(&bytes);
    for (open, close) in [
        ("<w:bookmarkStart ", "<w:bookmarkEnd "),
        ("<w:commentRangeStart ", "<w:commentRangeEnd "),
        ("<w:permStart ", "<w:permEnd "),
    ] {
        let (o, c) = marker_counts(&xml, open, close);
        assert_eq!(o, c, "{ctx}: unbalanced {open}/{close}: {o} vs {c}\n{xml}");
    }
    Document::parse(&bytes).unwrap_or_else(|e| panic!("{ctx}: re-import refused: {e:?}"));
}

/// Drive every resolution path over a straddling tracked delete and assert each
/// output is clean. `body` places one half of a range pair as a BODY-LEVEL
/// marker; deleting `delete_para_index` removes the paragraph holding its inline
/// partner.
fn assert_all_resolutions_clean(name: &str, body: &str, delete_para_index: usize, expect: &str) {
    let base = Document::parse(&pack(body)).expect("parse base");
    let ids = para_ids(&base);
    let edited = tracked_delete_block(&base, &ids[delete_para_index], expect);

    for &id in &all_rev_ids(&edited) {
        for action in [
            ResolveSelectionAction::Accept,
            ResolveSelectionAction::Reject,
        ] {
            let projected = edited
                .project(stemma::Resolution::Selective {
                    ids: HashSet::from([id]),
                    action,
                })
                .unwrap_or_else(|e| panic!("{name}: project selective id={id} {action:?}: {e:?}"));
            assert_clean(&projected, &format!("{name}/selective id={id} {action:?}"));
        }
    }
    for (label, res) in [
        ("AcceptAll", stemma::Resolution::AcceptAll),
        ("RejectAll", stemma::Resolution::RejectAll),
    ] {
        let projected = edited
            .project(res)
            .unwrap_or_else(|e| panic!("{name}: project {label}: {e:?}"));
        assert_clean(&projected, &format!("{name}/{label}"));
    }
}

// ── bookmark: the exact reported shape ───────────────────────────────────────

/// A body-level `bookmarkStart` (direct child of `w:body`, between P0 and P1)
/// with its `bookmarkEnd` INSIDE P1. A tracked delete of P1, resolved either
/// way, must not orphan the surviving body-level start. This is the exact
/// orphaned-body-level-start repro; before the fix the selective-accept and
/// accept-all legs refused with
/// "1 orphaned bookmarkStart".
#[test]
fn bodylevel_bookmark_start_partner_deleted_all_resolutions_clean() {
    assert_all_resolutions_clean(
        "bodylevel-bookmarkStart",
        r#"<w:p><w:r><w:t>alpha</w:t></w:r></w:p>
           <w:bookmarkStart w:id="0" w:name="span"/>
           <w:p><w:r><w:t>beta</w:t></w:r><w:bookmarkEnd w:id="0"/></w:p>
           <w:p><w:r><w:t>gamma</w:t></w:r></w:p>"#,
        1,
        "beta",
    );
}

/// Mirror: body-level `bookmarkEnd`, its `bookmarkStart` inside the deleted P1.
#[test]
fn bodylevel_bookmark_end_partner_deleted_all_resolutions_clean() {
    assert_all_resolutions_clean(
        "bodylevel-bookmarkEnd",
        r#"<w:p><w:r><w:t>alpha</w:t></w:r></w:p>
           <w:p><w:bookmarkStart w:id="0" w:name="span"/><w:r><w:t>beta</w:t></w:r></w:p>
           <w:bookmarkEnd w:id="0"/>
           <w:p><w:r><w:t>gamma</w:t></w:r></w:p>"#,
        1,
        "beta",
    );
}

// ── permission range twin ────────────────────────────────────────────────────

#[test]
fn bodylevel_permission_range_partner_deleted_all_resolutions_clean() {
    assert_all_resolutions_clean(
        "bodylevel-permStart",
        r#"<w:p><w:r><w:t>alpha</w:t></w:r></w:p>
           <w:permStart w:id="1" w:edGrp="everyone"/>
           <w:p><w:r><w:t>beta</w:t></w:r><w:permEnd w:id="1"/></w:p>
           <w:p><w:r><w:t>gamma</w:t></w:r></w:p>"#,
        1,
        "beta",
    );
}

// ── comment range twin ───────────────────────────────────────────────────────

/// A body-level `commentRangeStart` (direct child of `w:body`, before P1) whose
/// `commentRangeEnd` is INSIDE P1. Deleting P1 removes the end; before the fix
/// the surviving body-level start orphaned as a non-blocking `I-ANN-005`
/// (comment-range integrity is a WARN, so it slipped past the serializer instead
/// of refusing). `assert_clean`'s marker-balance check catches that.
#[test]
fn bodylevel_comment_range_partner_deleted_all_resolutions_clean() {
    assert_all_resolutions_clean(
        "bodylevel-commentRangeStart",
        r#"<w:p><w:r><w:t>alpha</w:t></w:r></w:p>
           <w:commentRangeStart w:id="0"/>
           <w:p><w:r><w:t>beta</w:t></w:r><w:commentRangeEnd w:id="0"/></w:p>
           <w:p><w:r><w:t>gamma</w:t></w:r></w:p>"#,
        1,
        "beta",
    );
}
