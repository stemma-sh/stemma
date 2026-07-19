//! DeleteImage — a dedicated op that removes an inline Drawing opaque as a tracked
//! deletion (tracked: the drawing's segment becomes Deleted, surrounding text stays
//! Normal, accept=gone/reject=restored; direct: removed). It does NOT route through
//! the text-replace path, so it neither touches nor weakens the OpaqueDestroyed
//! guard (case `does_not_weaken_opaque_destroyed`).
//!
//! Daily tier, corpus-free.

use stemma::api::{Document, validate};
use stemma::domain::{
    BlockNode, CanonDoc, InlineNode, NodeId, OpaqueKind, RevisionInfo, TrackingStatus,
};
use stemma::edit::{
    ContentFragment, EditStep, EditTransaction, ImageFormat, ImageSource, MaterializationMode,
    ParagraphContent,
};
use stemma::runtime::ExportOptions;
use stemma::{ErrorCode, Resolution, RuntimeError};

fn tiny_png(tag: u8) -> Vec<u8> {
    let mut v = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    v.extend_from_slice(&[tag; 24]);
    v
}

fn make_text_docx(text: &str) -> Vec<u8> {
    docx_from_body(&format!(
        r#"<w:p><w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p>"#
    ))
}
/// A 1×1 table whose only cell holds "cell " + an inline drawing (a textbox shape,
/// a Drawing opaque with no media part).
fn make_table_docx() -> Vec<u8> {
    let drawing = r#"<w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="900000" cy="900000"/><wp:docPr id="1" name="Shape 1"/><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:txbx><w:txbxContent><w:p><w:r><w:t>S</w:t></w:r></w:p></w:txbxContent></wps:txbx><wps:bodyPr/></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>"#;
    docx_from_body(&format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="5000" w:type="pct"/></w:tblPr><w:tblGrid><w:gridCol w:w="4800"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="4800" w:type="dxa"/></w:tcPr><w:p><w:r><w:t xml:space="preserve">cell </w:t></w:r>{drawing}</w:p></w:tc></w:tr></w:tbl><w:p/>"#
    ))
}
/// One paragraph "Hi " + two inline drawings with DISTINCT docPr ids.
fn make_two_drawings_docx() -> Vec<u8> {
    let d = |id: u32, name: &str| {
        format!(
            r#"<w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="900000" cy="900000"/><wp:docPr id="{id}" name="{name}"/><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:txbx><w:txbxContent><w:p><w:r><w:t>{name}</w:t></w:r></w:p></w:txbxContent></wps:txbx><wps:bodyPr/></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>"#
        )
    };
    docx_from_body(&format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Hi </w:t></w:r>{}{}</w:p>"#,
        d(1, "A"),
        d(2, "B")
    ))
}
fn docx_from_body(body: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><w:body>{body}<w:sectPr/></w:body></w:document>"#
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

fn txn(steps: Vec<EditStep>, mode: MaterializationMode) -> EditTransaction {
    txn_by(steps, mode, "Imager")
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
            date: Some("2026-07-01T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

/// `Document` isn't `Debug`, so `Result::expect_err` won't compile — extract the
/// error by hand.
fn apply_err(r: Result<Document, RuntimeError>) -> RuntimeError {
    match r {
        Ok(_) => panic!("expected the edit to be rejected, but it applied"),
        Err(e) => e,
    }
}

fn insert_step(block_id: NodeId, bytes: Vec<u8>) -> EditStep {
    let image = ImageSource::new(
        bytes,
        ImageFormat::Png,
        914_400,
        457_200,
        Some("logo".into()),
        0,
    )
    .expect("valid png source");
    EditStep::InsertImage {
        block_id,
        expect: None,
        semantic_hash: None,
        image,
        rationale: None,
    }
}
fn delete_step(block_id: NodeId, drawing_id: NodeId, hash: Option<String>) -> EditStep {
    EditStep::DeleteImage {
        block_id,
        drawing_id,
        semantic_hash: hash,
        rationale: None,
    }
}

fn first_block_id(canon: &CanonDoc) -> NodeId {
    match &canon.blocks[0].block {
        BlockNode::Paragraph(p) => p.id.clone(),
        _ => panic!("not a paragraph"),
    }
}
fn drawing_count(canon: &CanonDoc) -> usize {
    canon
        .blocks
        .iter()
        .filter_map(|tb| match &tb.block {
            BlockNode::Paragraph(p) => Some(p),
            _ => None,
        })
        .flat_map(|p| &p.segments)
        .flat_map(|s| &s.inlines)
        .filter(
            |i| matches!(i, InlineNode::OpaqueInline(o) if matches!(o.kind, OpaqueKind::Drawing)),
        )
        .count()
}
fn first_drawing(canon: &CanonDoc) -> (NodeId, NodeId, Option<String>) {
    if let BlockNode::Paragraph(p) = &canon.blocks[0].block {
        for seg in &p.segments {
            for i in &seg.inlines {
                if let InlineNode::OpaqueInline(o) = i
                    && matches!(o.kind, OpaqueKind::Drawing)
                {
                    return (p.id.clone(), o.id.clone(), o.content_hash.clone());
                }
            }
        }
    }
    panic!("no drawing found");
}
fn para_text(canon: &CanonDoc) -> String {
    let mut s = String::new();
    if let BlockNode::Paragraph(p) = &canon.blocks[0].block {
        for seg in &p.segments {
            for i in &seg.inlines {
                if let InlineNode::Text(t) = i {
                    s.push_str(&t.text);
                }
            }
        }
    }
    s
}

/// A doc whose first paragraph is "Hello world" plus a Normal inline drawing.
fn doc_with_inline_drawing() -> Document {
    let doc = Document::parse(&make_text_docx("Hello world")).unwrap();
    let bid = first_block_id(&doc.snapshot().canonical);
    doc.apply(&txn(
        vec![insert_step(bid, tiny_png(1))],
        MaterializationMode::Direct,
    ))
    .expect("insert a Normal drawing")
}

/// Tracked delete tombstones the drawing (still present, Deleted) and keeps the
/// text; accept-all drops it; reject-all restores it byte-identical.
#[test]
fn tracked_delete_tombstones_drawing_keeps_text() {
    let doc = doc_with_inline_drawing();
    let (bid, did, hash) = first_drawing(&doc.snapshot().canonical);
    assert_eq!(drawing_count(&doc.snapshot().canonical), 1);

    let edited = doc
        .apply(&txn(
            vec![delete_step(bid, did, hash.clone())],
            MaterializationMode::TrackedChange,
        ))
        .expect("tracked DeleteImage applies");

    // Tombstoned: the drawing is still in the snapshot (as a Deleted segment), and
    // the text is untouched.
    assert_eq!(
        drawing_count(&edited.snapshot().canonical),
        1,
        "drawing tombstoned, not removed"
    );
    assert_eq!(
        para_text(&edited.snapshot().canonical),
        "Hello world",
        "text preserved"
    );
    // The drawing sits in a Deleted segment; text sits in Normal segment(s).
    if let BlockNode::Paragraph(p) = &edited.snapshot().canonical.blocks[0].block {
        let drawing_seg_deleted = p.segments.iter().any(|s| {
            matches!(s.status, stemma::domain::TrackingStatus::Deleted(_))
                && s.inlines.iter().any(|i| matches!(i, InlineNode::OpaqueInline(o) if matches!(o.kind, OpaqueKind::Drawing)))
        });
        assert!(
            drawing_seg_deleted,
            "the drawing is isolated in a Deleted segment"
        );
    }

    // accept-all → drawing gone, text kept.
    let acc = edited.project(Resolution::AcceptAll).expect("accept-all");
    assert_eq!(
        drawing_count(&acc.snapshot().canonical),
        0,
        "accept removes the drawing"
    );
    assert_eq!(para_text(&acc.snapshot().canonical), "Hello world");

    // reject-all → drawing restored byte-identical (same content_hash), text kept.
    let rej = edited.project(Resolution::RejectAll).expect("reject-all");
    assert_eq!(
        drawing_count(&rej.snapshot().canonical),
        1,
        "reject restores the drawing"
    );
    let (_, _, rej_hash) = first_drawing(&rej.snapshot().canonical);
    assert_eq!(
        rej_hash, hash,
        "reject restores the drawing byte-identical (same content_hash)"
    );
    assert_eq!(para_text(&rej.snapshot().canonical), "Hello world");
}

/// Direct (untracked) delete physically removes the drawing immediately, keeps text.
#[test]
fn direct_delete_removes_drawing_keeps_text() {
    let doc = doc_with_inline_drawing();
    let (bid, did, hash) = first_drawing(&doc.snapshot().canonical);
    let edited = doc
        .apply(&txn(
            vec![delete_step(bid, did, hash)],
            MaterializationMode::Direct,
        ))
        .expect("direct DeleteImage applies");
    assert_eq!(
        drawing_count(&edited.snapshot().canonical),
        0,
        "direct mode removes the drawing"
    );
    assert_eq!(
        para_text(&edited.snapshot().canonical),
        "Hello world",
        "text preserved"
    );
}

/// Deleting one of two drawings leaves the other intact.
#[test]
fn delete_one_of_two_drawings_keeps_the_other() {
    let doc = Document::parse(&make_text_docx("Hello world")).unwrap();
    let bid = first_block_id(&doc.snapshot().canonical);
    let doc = doc
        .apply(&txn(
            vec![
                insert_step(bid.clone(), tiny_png(1)),
                insert_step(bid, tiny_png(2)),
            ],
            MaterializationMode::Direct,
        ))
        .expect("insert two drawings");
    assert_eq!(drawing_count(&doc.snapshot().canonical), 2);
    let (bid, did, hash) = first_drawing(&doc.snapshot().canonical);
    let edited = doc
        .apply(&txn(
            vec![delete_step(bid, did, hash)],
            MaterializationMode::Direct,
        ))
        .expect("delete one drawing");
    assert_eq!(
        drawing_count(&edited.snapshot().canonical),
        1,
        "the other drawing survives"
    );
}

/// A wrong content_hash guard is rejected (stale-snapshot detection on the drawing).
#[test]
fn guard_mismatch_is_rejected() {
    let doc = doc_with_inline_drawing();
    let (bid, did, _) = first_drawing(&doc.snapshot().canonical);
    let err = apply_err(doc.apply(&txn(
        vec![delete_step(
            bid,
            did,
            Some("v2:deadbeef_not_the_real_hash".to_string()),
        )],
        MaterializationMode::TrackedChange,
    )));
    // A drawing content_hash mismatch is the stale-snapshot class (StaleEdit).
    assert_eq!(
        err.code,
        ErrorCode::StaleEdit,
        "expected a stale-guard rejection, got {err:?}"
    );
}

/// Deleting one's OWN pending inserted drawing UN-PROPOSES it — no tombstone; both
/// accept-all and reject-all yield a doc with no drawing.
#[test]
fn delete_just_inserted_drawing_same_author_unproposes() {
    let doc = Document::parse(&make_text_docx("Hello world")).unwrap();
    let bid = first_block_id(&doc.snapshot().canonical);
    // insert TRACKED (a pending Inserted drawing), then delete it TRACKED, same author.
    let inserted = doc
        .apply(&txn(
            vec![insert_step(bid, tiny_png(1))],
            MaterializationMode::TrackedChange,
        ))
        .expect("tracked insert");
    let (bid, did, hash) = first_drawing(&inserted.snapshot().canonical);
    let edited = inserted
        .apply(&txn(
            vec![delete_step(bid, did, hash)],
            MaterializationMode::TrackedChange,
        ))
        .expect("tracked delete of own pending insert");
    // Un-proposed: neither resolution keeps the drawing.
    assert_eq!(
        drawing_count(
            &edited
                .project(Resolution::AcceptAll)
                .unwrap()
                .snapshot()
                .canonical
        ),
        0
    );
    assert_eq!(
        drawing_count(
            &edited
                .project(Resolution::RejectAll)
                .unwrap()
                .snapshot()
                .canonical
        ),
        0
    );
    assert_eq!(para_text(&edited.snapshot().canonical), "Hello world");
}

/// reject-all of a tracked delete serializes validator-clean (the drawing rides its
/// segment untouched — the load-bearing model assumption).
#[test]
fn reject_all_serializes_validator_clean() {
    let doc = doc_with_inline_drawing();
    let (bid, did, hash) = first_drawing(&doc.snapshot().canonical);
    let edited = doc
        .apply(&txn(
            vec![delete_step(bid, did, hash)],
            MaterializationMode::TrackedChange,
        ))
        .unwrap();
    let rej = edited.project(Resolution::RejectAll).unwrap();
    let bytes = rej
        .serialize(&ExportOptions::default())
        .expect("serialize reject-all");
    let report = validate(&bytes);
    assert!(
        report.ok,
        "reject-all serializes validator-clean: {:?}",
        report.issues
    );
    // And the accept-all path (drawing gone) serializes clean too.
    let acc = edited.project(Resolution::AcceptAll).unwrap();
    let bytes = acc
        .serialize(&ExportOptions::default())
        .expect("serialize accept-all");
    assert!(validate(&bytes).ok, "accept-all serializes clean");
}

/// DeleteImage does NOT weaken OpaqueDestroyed: after deleting one drawing, a text
/// ReplaceParagraphText that omits the SURVIVING drawing's anchor still fails loud.
#[test]
fn does_not_weaken_opaque_destroyed() {
    let doc = Document::parse(&make_text_docx("Hello world")).unwrap();
    let bid = first_block_id(&doc.snapshot().canonical);
    let doc = doc
        .apply(&txn(
            vec![
                insert_step(bid.clone(), tiny_png(1)),
                insert_step(bid, tiny_png(2)),
            ],
            MaterializationMode::Direct,
        ))
        .unwrap();
    let (bid, did, hash) = first_drawing(&doc.snapshot().canonical);
    // Delete one drawing (the survivor stays).
    let doc = doc
        .apply(&txn(
            vec![delete_step(bid.clone(), did, hash)],
            MaterializationMode::Direct,
        ))
        .expect("delete one drawing");
    assert_eq!(drawing_count(&doc.snapshot().canonical), 1);
    // A text replace that carries no PreservedInlineRef for the survivor must still
    // trip OpaqueDestroyed — the guard is untouched.
    let err = apply_err(doc.apply(&txn(
        vec![EditStep::ReplaceParagraphText {
            block_id: bid,
            rationale: None,
            replacement_role: None,
            expect: "Hello world".to_string(),
            semantic_hash: None,
            content: ParagraphContent {
                fragments: vec![ContentFragment::Text("Hello world".to_string())],
            },
        }],
        MaterializationMode::Direct,
    )));
    assert_eq!(
        err.code,
        ErrorCode::OpaqueDestroyed,
        "OpaqueDestroyed still fires — the text-replace guard is not weakened; got {err:?}"
    );
}

// ─── #2/#3 follow-up cases: cross-author stack, no-merge, table cell ───────────

fn all_drawings(canon: &CanonDoc) -> Vec<(NodeId, NodeId, Option<String>)> {
    let mut out = Vec::new();
    if let BlockNode::Paragraph(p) = &canon.blocks[0].block {
        for seg in &p.segments {
            for i in &seg.inlines {
                if let InlineNode::OpaqueInline(o) = i
                    && matches!(o.kind, OpaqueKind::Drawing)
                {
                    out.push((p.id.clone(), o.id.clone(), o.content_hash.clone()));
                }
            }
        }
    }
    out
}
fn first_cell_drawing(canon: &CanonDoc) -> (NodeId, NodeId, Option<String>) {
    for tb in &canon.blocks {
        if let BlockNode::Table(t) = &tb.block {
            for row in &t.rows {
                for cell in &row.cells {
                    for b in &cell.blocks {
                        if let BlockNode::Paragraph(p) = b {
                            for seg in &p.segments {
                                for i in &seg.inlines {
                                    if let InlineNode::OpaqueInline(o) = i
                                        && matches!(o.kind, OpaqueKind::Drawing)
                                    {
                                        return (
                                            p.id.clone(),
                                            o.id.clone(),
                                            o.content_hash.clone(),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    panic!("no cell drawing found");
}
fn cell_drawing_count(canon: &CanonDoc) -> usize {
    let mut n = 0;
    for tb in &canon.blocks {
        if let BlockNode::Table(t) = &tb.block {
            for row in &t.rows {
                for cell in &row.cells {
                    for b in &cell.blocks {
                        if let BlockNode::Paragraph(p) = b {
                            for seg in &p.segments {
                                n += seg.inlines.iter().filter(|i| matches!(i, InlineNode::OpaqueInline(o) if matches!(o.kind, OpaqueKind::Drawing))).count();
                            }
                        }
                    }
                }
            }
        }
    }
    n
}

/// A CROSS-author deletion of a pending inserted drawing STACKS
/// (InsertedThenDeleted), not un-proposes; both resolutions drop it.
#[test]
fn delete_cross_author_inserted_drawing_stacks() {
    let doc = Document::parse(&make_text_docx("Hello world")).unwrap();
    let bid = first_block_id(&doc.snapshot().canonical);
    let inserted = doc
        .apply(&txn_by(
            vec![insert_step(bid, tiny_png(1))],
            MaterializationMode::TrackedChange,
            "Author A",
        ))
        .unwrap();
    let (bid, did, hash) = first_drawing(&inserted.snapshot().canonical);
    let edited = inserted
        .apply(&txn_by(
            vec![delete_step(bid, did, hash)],
            MaterializationMode::TrackedChange,
            "Author B",
        ))
        .expect("cross-author delete applies");
    if let BlockNode::Paragraph(p) = &edited.snapshot().canonical.blocks[0].block {
        let stacked = p.segments.iter().any(|s| {
            matches!(s.status, TrackingStatus::InsertedThenDeleted(_))
                && s.inlines.iter().any(|i| matches!(i, InlineNode::OpaqueInline(o) if matches!(o.kind, OpaqueKind::Drawing)))
        });
        assert!(
            stacked,
            "cross-author delete stacks as InsertedThenDeleted, got {:?}",
            p.segments.iter().map(|s| &s.status).collect::<Vec<_>>()
        );
    }
    assert_eq!(
        drawing_count(
            &edited
                .project(Resolution::AcceptAll)
                .unwrap()
                .snapshot()
                .canonical
        ),
        0
    );
    assert_eq!(
        drawing_count(
            &edited
                .project(Resolution::RejectAll)
                .unwrap()
                .snapshot()
                .canonical
        ),
        0
    );
}

/// Deleting two adjacent drawings must NOT merge their Deleted segments — each
/// deletion is its own revision, and `normalize_segments` merges only IDENTICAL
/// status (`last.status == segment.status`, which compares the full RevisionInfo).
/// This is the invariant that also keeps a cross-author deletion from being
/// re-attributed to a neighbour.
#[test]
fn deleting_two_adjacent_drawings_keeps_segments_separate() {
    // Two PRE-PLACED drawings with distinct ids (insert_image collides two inserts on
    // one id — a separate quirk, not DeleteImage's).
    let doc = Document::parse(&make_two_drawings_docx()).unwrap();
    let ds = all_drawings(&doc.snapshot().canonical);
    assert_eq!(ds.len(), 2, "two distinct drawings");
    assert_ne!(ds[0].1, ds[1].1, "distinct drawing ids");
    // Delete BOTH in one transaction. Guards omitted: deleting one drawing shifts a
    // sibling's content_hash (docPr renumber) even within a txn — the editor re-reads
    // /rich for a fresh guard between edits; this case is purely about the resulting
    // segment structure. Each delete stamps its own revision.
    let d = doc
        .apply(&txn(
            vec![
                delete_step(ds[0].0.clone(), ds[0].1.clone(), None),
                delete_step(ds[1].0.clone(), ds[1].1.clone(), None),
            ],
            MaterializationMode::TrackedChange,
        ))
        .expect("delete both drawings in one txn");
    if let BlockNode::Paragraph(p) = &d.snapshot().canonical.blocks[0].block {
        let segs: Vec<_> = p
            .segments
            .iter()
            .map(|s| {
                (
                    format!("{:?}", s.status)
                        .chars()
                        .take(14)
                        .collect::<String>(),
                    s.inlines.len(),
                )
            })
            .collect();
        let deleted_drawing_segs = p
            .segments
            .iter()
            .filter(|s| {
                matches!(s.status, TrackingStatus::Deleted(_))
                    && s.inlines.iter().any(|i| matches!(i, InlineNode::OpaqueInline(o) if matches!(o.kind, OpaqueKind::Drawing)))
            })
            .count();
        assert_eq!(
            deleted_drawing_segs, 2,
            "each author's deletion stays a separate segment; segs={segs:?}"
        );
    }
}

/// Delete an image inside a TABLE CELL — the verb resolves the cell paragraph via
/// find_paragraph_path (find_block_index would be top-level-only → BlockNotFound).
#[test]
fn delete_image_in_a_table_cell() {
    let doc = Document::parse(&make_table_docx()).unwrap();
    assert_eq!(
        cell_drawing_count(&doc.snapshot().canonical),
        1,
        "the cell has a drawing"
    );
    let (bid, did, hash) = first_cell_drawing(&doc.snapshot().canonical);
    let edited = doc
        .apply(&txn(
            vec![delete_step(bid, did, hash)],
            MaterializationMode::Direct,
        ))
        .expect("delete a cell drawing (cell-aware resolve)");
    assert_eq!(
        cell_drawing_count(&edited.snapshot().canonical),
        0,
        "cell drawing removed"
    );
}
