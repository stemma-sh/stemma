//! Sentinel: the DOCUMENT-FINAL paragraph mark never carries a tracked mark
//! insertion or deletion.
//!
//! Word cannot resolve a revision on the document-final paragraph mark — accept
//! of a paragraph-mark insertion merges with the FOLLOWING paragraph, and the
//! final mark has no follower, so accept-all leaves it pending forever; the
//! mark-deletion twin has the same defect. The engine re-attributes a trailing
//! append/delete to the PRECEDING mark, matching what Word itself produces when
//! you press Enter at (or delete) the end of the last paragraph.
//!
//! These tests pin the emitted attribution (final `w:p` mark untracked, the
//! preceding mark tracked) and that our own accept/reject projections are
//! unchanged (reject restores the original, accept keeps every insertion).

use stemma::domain::*;
use stemma::edit::*;
use stemma::tracked_model::ResolveSelectionAction;
use stemma::{
    DocHandle, DocxRuntime, ExportMode, SimpleRuntime, accept_all, reject_all_with_styles,
};
use xmltree::Element;

fn test_revision() -> RevisionInfo {
    RevisionInfo {
        revision_id: 100,
        identity: 0,
        author: Some("Sentinel".to_string()),
        date: Some("2026-03-28T00:00:00Z".to_string()),
        apply_op_id: None,
    }
}

fn para(text: &str) -> String {
    format!(r#"<w:p><w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p>"#)
}

/// Build a minimal DOCX whose `w:body` inner XML is `body_inner`.
fn make_docx_with_body(body_inner: &str) -> Vec<u8> {
    use std::io::Write as _;
    use zip::write::FileOptions;
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
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

fn insert_after(anchor: &NodeId, texts: &[&str]) -> EditTransaction {
    EditTransaction {
        steps: vec![EditStep::InsertParagraphs {
            anchor_block_id: anchor.clone(),
            position: InsertPosition::After,
            rationale: None,
            blocks: texts
                .iter()
                .map(|t| {
                    BlockSpec::Paragraph(ParagraphBlockSpec {
                        role: Some("body".to_string()),
                        content: parse_paragraph_markup(t).unwrap(),
                        restart_numbering: false,
                        list: None,
                    })
                })
                .collect(),
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    }
}

struct Fixture {
    runtime: SimpleRuntime,
    handle: DocHandle,
    /// Body-paragraph NodeIds in document order.
    para_ids: Vec<NodeId>,
}

fn import_body(body: &str) -> Fixture {
    let runtime = SimpleRuntime::new();
    let import = runtime
        .import_docx(&make_docx_with_body(body))
        .expect("import synthetic docx");
    let para_ids = import
        .canonical
        .blocks
        .iter()
        .filter_map(|tb| match &tb.block {
            BlockNode::Paragraph(p) => Some(p.id.clone()),
            _ => None,
        })
        .collect();
    Fixture {
        runtime,
        handle: import.doc_handle,
        para_ids,
    }
}

/// Apply `tx` to the fixture and return `(edited canonical, exported redline
/// document.xml root element)`.
fn apply_and_export(fixture: &Fixture, tx: &EditTransaction) -> (CanonDoc, Element) {
    let result = fixture
        .runtime
        .apply_edit(&fixture.handle, tx)
        .expect("apply_edit");
    let canonical = std::sync::Arc::unwrap_or_clone(result.canonical);
    let bytes = fixture
        .runtime
        .export_docx(&fixture.handle, ExportMode::Redline)
        .expect("export_docx");
    let root = parse_document_xml(&bytes);
    (canonical, root)
}

fn parse_document_xml(docx_bytes: &[u8]) -> Element {
    use std::io::Read as _;
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(docx_bytes.to_vec())).expect("zip open");
    let mut xml = Vec::new();
    archive
        .by_name("word/document.xml")
        .expect("document.xml present")
        .read_to_end(&mut xml)
        .expect("read document.xml");
    Element::parse(std::io::Cursor::new(xml)).expect("parse document.xml")
}

fn body_element(root: &Element) -> &Element {
    root.get_child("body").expect("w:body present")
}

/// The last `w:p` element in the body.
fn last_paragraph(root: &Element) -> &Element {
    body_element(root)
        .children
        .iter()
        .filter_map(|c| match c {
            xmltree::XMLNode::Element(el) if el.name == "p" => Some(el),
            _ => None,
        })
        .next_back()
        .expect("at least one w:p in body")
}

/// Does this paragraph's pilcrow (`w:pPr/w:rPr`) carry a `w:ins`/`w:del` marker?
fn paragraph_mark_has(el: &Element, marker: &str) -> bool {
    el.get_child("pPr")
        .and_then(|ppr| ppr.get_child("rPr"))
        .is_some_and(|rpr| rpr.get_child(marker).is_some())
}

/// Visible text of every body paragraph, joined by `\n` (the engine's own
/// projection, so paragraph-mark merges are applied).
fn doc_text(doc: &CanonDoc) -> String {
    doc.blocks
        .iter()
        .filter_map(|tb| match &tb.block {
            BlockNode::Paragraph(p) => {
                let mut text = String::new();
                for seg in &p.segments {
                    for inline in &seg.inlines {
                        if let InlineNode::Text(t) = inline {
                            text.push_str(&t.text);
                        }
                    }
                }
                Some(text)
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `(accept-all text, reject-all text)` through the engine's projections.
fn accept_reject_text(canonical: &CanonDoc) -> (String, String) {
    let mut accepted = canonical.clone();
    accept_all(&mut accepted);
    let mut rejected = canonical.clone();
    reject_all_with_styles(&mut rejected, None);
    (doc_text(&accepted), doc_text(&rejected))
}

/// The effective final-mark status the serializer would emit for a paragraph's
/// pilcrow: its own `para_mark_status`, else the block-level status.
fn effective_final_mark(tb: &TrackedBlock) -> TrackingStatus {
    match &tb.block {
        BlockNode::Paragraph(p) => p
            .para_mark_status
            .clone()
            .unwrap_or_else(|| tb.status.clone()),
        _ => tb.status.clone(),
    }
}

fn para_mark_of<'a>(blocks: &'a [TrackedBlock], id: &NodeId) -> &'a Option<TrackingStatus> {
    blocks
        .iter()
        .find_map(|tb| match &tb.block {
            BlockNode::Paragraph(p) if &p.id == id => Some(&p.para_mark_status),
            _ => None,
        })
        .expect("paragraph present")
}

// ── (i) tracked InsertParagraphs after the LAST body paragraph ──────────────

#[test]
fn insert_after_last_body_paragraph_leaves_final_mark_untracked() {
    let fixture = import_body(&format!("{}{}", para("First."), para("Second.")));
    let anchor = fixture.para_ids.last().unwrap().clone();
    let tx = insert_after(&anchor, &["Inserted A.", "Inserted B."]);
    let (canonical, root) = apply_and_export(&fixture, &tx);

    // Serialize check: the FINAL w:p's pilcrow carries no w:ins.
    let last = last_paragraph(&root);
    assert!(
        !paragraph_mark_has(last, "ins"),
        "document-final w:p must not carry a paragraph-mark insertion"
    );

    // The anchor (previously-final paragraph) now carries the insertion marker,
    // and the final paragraph's effective mark is untracked.
    let blocks = &canonical.blocks;
    let final_mark = effective_final_mark(blocks.last().unwrap());
    assert!(
        matches!(final_mark, TrackingStatus::Normal),
        "final paragraph's effective mark must be Normal, got {final_mark:?}"
    );
    assert!(
        matches!(
            para_mark_of(blocks, &anchor),
            Some(TrackingStatus::Inserted(_))
        ),
        "anchor mark must carry the insertion"
    );

    // Projections: reject restores the original text exactly; accept keeps all.
    let (accept, reject) = accept_reject_text(&canonical);
    assert_eq!(
        reject, "First.\nSecond.",
        "reject-all restores the original"
    );
    assert_eq!(
        accept, "First.\nSecond.\nInserted A.\nInserted B.",
        "accept-all keeps every inserted paragraph"
    );
}

// ── (ii) tracked DeleteBlockRange of the LAST paragraph ─────────────────────

#[test]
fn delete_last_body_paragraph_leaves_final_mark_untracked() {
    let fixture = import_body(&format!("{}{}", para("Keep me."), para("Delete me.")));
    let victim = fixture.para_ids.last().unwrap().clone();
    let preceding = fixture.para_ids[fixture.para_ids.len() - 2].clone();
    let tx = EditTransaction {
        steps: vec![EditStep::DeleteBlockRange {
            from_block_id: victim.clone(),
            to_block_id: victim.clone(),
            rationale: None,
            expect: "Delete me.".to_string(),
            semantic_hash: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };
    let (canonical, root) = apply_and_export(&fixture, &tx);

    // Serialize check: the FINAL w:p's pilcrow carries no w:del.
    let last = last_paragraph(&root);
    assert!(
        !paragraph_mark_has(last, "del"),
        "document-final w:p must not carry a paragraph-mark deletion"
    );

    // The final paragraph survives as the untracked final mark; the preceding
    // paragraph carries the mark-deletion.
    let blocks = &canonical.blocks;
    let final_mark = effective_final_mark(blocks.last().unwrap());
    assert!(
        matches!(final_mark, TrackingStatus::Normal),
        "final paragraph's effective mark must be Normal, got {final_mark:?}"
    );
    assert!(
        matches!(
            para_mark_of(blocks, &preceding),
            Some(TrackingStatus::Deleted(_))
        ),
        "preceding mark must carry the deletion"
    );

    let (accept, reject) = accept_reject_text(&canonical);
    assert_eq!(
        reject, "Keep me.\nDelete me.",
        "reject-all restores the deleted paragraph"
    );
    assert_eq!(
        accept, "Keep me.",
        "accept-all removes the deleted paragraph"
    );
}

// ── (iii) mid-document insert: the tail is untouched (regression pin) ────────

#[test]
fn mid_document_insert_keeps_marks_on_new_paragraphs() {
    let fixture = import_body(&format!("{}{}", para("First."), para("Last.")));
    let anchor = fixture.para_ids[0].clone(); // insert after the FIRST, not the last
    let tx = insert_after(&anchor, &["Middle."]);
    let (canonical, root) = apply_and_export(&fixture, &tx);

    // The final paragraph is untouched — no marker.
    let last = last_paragraph(&root);
    assert!(!paragraph_mark_has(last, "ins"));
    assert!(!paragraph_mark_has(last, "del"));

    let blocks = &canonical.blocks;
    // The inserted middle paragraph is a block-level insertion whose mark stays
    // inserted (driven by block status), exactly as before this fix.
    let inserted = blocks
        .iter()
        .find(|tb| matches!(tb.status, TrackingStatus::Inserted(_)))
        .expect("inserted block present");
    assert!(
        matches!(effective_final_mark(inserted), TrackingStatus::Inserted(_)),
        "a mid-document inserted paragraph keeps its inserted mark"
    );
    // The anchor must NOT have gained an inserted mark (that shift only happens
    // at the document end).
    assert!(
        !matches!(
            para_mark_of(blocks, &anchor),
            Some(TrackingStatus::Inserted(_))
        ),
        "mid-document anchor must not carry a shifted insertion mark"
    );

    let (accept, reject) = accept_reject_text(&canonical);
    assert_eq!(reject, "First.\nLast.");
    assert_eq!(accept, "First.\nMiddle.\nLast.");
}

// ── (iv) tracked MoveBlockRange to AFTER the LAST body paragraph ─────────────
//
// A move whose destination ends the document leaves the moved-in final
// paragraph as the document-final mark. Its pilcrow must NOT carry the move
// insertion (Word cannot resolve a mark change on the final pilcrow); the
// attribution shifts to the anchor's mark, exactly like a plain insert tail —
// while the move pair (shared move_id, moveFromRange/moveToRange markers) stays
// intact so reject restores the original order.

fn move_after(from: &NodeId, to: &NodeId, dest_anchor: &NodeId) -> EditTransaction {
    EditTransaction {
        steps: vec![EditStep::MoveBlockRange {
            from_block_id: from.clone(),
            to_block_id: to.clone(),
            dest_anchor_id: dest_anchor.clone(),
            dest_position: InsertPosition::After,
            rationale: None,
            expect: None,
            semantic_hash: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    }
}

fn five_para_fixture() -> Fixture {
    import_body(&format!(
        "{}{}{}{}{}",
        para("Para1."),
        para("Para2."),
        para("Para3."),
        para("Para4."),
        para("Para5."),
    ))
}

#[test]
fn move_to_after_last_body_paragraph_leaves_final_mark_untracked() {
    let fixture = five_para_fixture();
    let from = fixture.para_ids[1].clone(); // Para2
    let anchor = fixture.para_ids[4].clone(); // after Para5 (the last)
    let tx = move_after(&from, &from, &anchor);
    let (canonical, root) = apply_and_export(&fixture, &tx);

    // Serialize check: the FINAL w:p's pilcrow carries no insertion/move marker.
    let last = last_paragraph(&root);
    for marker in ["ins", "del", "moveTo", "moveFrom"] {
        assert!(
            !paragraph_mark_has(last, marker),
            "document-final w:p must not carry a paragraph-mark {marker}"
        );
    }

    // The moved-in final paragraph's effective mark is untracked; the anchor
    // (previously-final Para5) carries the shifted insertion.
    let blocks = &canonical.blocks;
    let final_mark = effective_final_mark(blocks.last().unwrap());
    assert!(
        matches!(final_mark, TrackingStatus::Normal),
        "final paragraph's effective mark must be Normal, got {final_mark:?}"
    );
    assert!(
        matches!(
            para_mark_of(blocks, &anchor),
            Some(TrackingStatus::Inserted(_))
        ),
        "anchor mark must carry the shifted insertion"
    );

    // The move pair is intact: the final block is still a block-level move
    // insertion (its runs stay wrapped in w:moveTo), only its pilcrow is
    // untracked.
    let final_block = blocks.last().unwrap();
    assert!(
        matches!(final_block.status, TrackingStatus::Inserted(_)) && final_block.move_id.is_some(),
        "final block must remain a moveTo insertion"
    );

    // Projections unchanged: reject restores the original order exactly, accept
    // yields the moved order.
    let (accept, reject) = accept_reject_text(&canonical);
    assert_eq!(
        reject, "Para1.\nPara2.\nPara3.\nPara4.\nPara5.",
        "reject-all restores the original order"
    );
    assert_eq!(
        accept, "Para1.\nPara3.\nPara4.\nPara5.\nPara2.",
        "accept-all yields the moved order"
    );
}

// ── SELECTIVE resolution must re-establish the same invariant ─────────────────
//
// The mint-time pass above keeps the document-final pilcrow untracked in the
// bytes an edit producer emits. A SELECTIVE resolution
// (`Resolution::Selective{ids, action}` — the MCP accept/reject surface) does
// not re-run it, yet it reshapes the body: it strips the edit-time suppression
// off a still-tracked trailing paragraph (re-exposing a block-level insertion/
// deletion on the final pilcrow), or it resolves every follower away so an
// anchor that still carries a pending mark-insertion becomes the final
// paragraph. Either way the projected document violates the invariant Word
// cannot resolve. `renormalize_final_mark_after_selective` re-establishes it.
//
// These sentinels FAIL without that pass. They assert BOTH the wire invariant
// (the last `w:p`'s pilcrow carries no `w:ins`/`w:del`) and that the projected
// accept/reject TEXT of the REMAINING revisions is the domain-correct result
// (accept keeps every surviving insertion, reject restores the original) — the
// normalization moves only the mark attribution, never the text.

fn ids_by_author(canon: &CanonDoc, author: &str) -> std::collections::HashSet<u32> {
    use stemma::view::{SegmentView, TrackStatus, build_document_view_from_canon};
    let view = build_document_view_from_canon(canon);
    let mut ids = std::collections::HashSet::new();
    let mut push = |s: &TrackStatus| {
        if let TrackStatus::Inserted(r) | TrackStatus::Deleted(r) = s
            && r.author.as_deref() == Some(author)
        {
            ids.insert(r.revision_id);
        }
    };
    for b in &view.blocks {
        push(&b.block_status);
        push(&b.paragraph_mark_status);
        for seg in &b.segments {
            match seg {
                SegmentView::Text { status, .. } | SegmentView::Opaque { status, .. } => {
                    push(status)
                }
            }
        }
    }
    ids
}

fn subsets(ids: &[u32]) -> Vec<Vec<u32>> {
    let mut out = Vec::new();
    let n = ids.len();
    for mask in 1u32..(1 << n) {
        let mut s = Vec::new();
        for (i, id) in ids.iter().enumerate() {
            if mask & (1 << i) != 0 {
                s.push(*id);
            }
        }
        out.push(s);
    }
    out
}

/// Every non-empty subset of `all_ids`, run through selective Accept and Reject
/// on `doc`, reporting each combination that leaves the document-final mark
/// tracked (model, wire, or validation). Returns the list of
/// `"<label> <action> subset=…"` violations — empty means clean.
fn sweep_doc(doc: &stemma::api::Document, all_ids: &[u32], label: &str) -> Vec<String> {
    assert!(!all_ids.is_empty(), "{label}: edit produced revision ids");
    let mut bad = Vec::new();
    for action in [
        ResolveSelectionAction::Accept,
        ResolveSelectionAction::Reject,
    ] {
        for subset in subsets(all_ids) {
            let set: std::collections::HashSet<u32> = subset.iter().copied().collect();
            let res = doc
                .project(stemma::Resolution::Selective { ids: set, action })
                .expect("project");
            let canon = res.snapshot().canonical.clone();
            let fm = effective_final_mark(canon.blocks.last().unwrap());
            if !matches!(fm, TrackingStatus::Normal) {
                bad.push(format!("{label} MODEL {action:?} subset={subset:?} {fm:?}"));
            }
            let bytes = res
                .serialize(&stemma::ExportOptions::default())
                .expect("serialize");
            let root = parse_document_xml(&bytes);
            let wlast = last_paragraph(&root);
            if paragraph_mark_has(wlast, "ins") || paragraph_mark_has(wlast, "del") {
                bad.push(format!("{label} WIRE {action:?} subset={subset:?}"));
            }
            let report = stemma::api::validate(&bytes);
            if !report.ok {
                bad.push(format!("{label} VALID {action:?} subset={subset:?}"));
            }
        }
    }
    bad
}

/// `sweep_doc` for a single tracked edit applied to `base_body`.
fn sweep_selective_final_mark(base_body: &str, tx: &EditTransaction, label: &str) -> Vec<String> {
    let doc = stemma::api::Document::parse(&make_docx_with_body(base_body))
        .expect("parse")
        .apply(tx)
        .expect("apply");
    let mut all_ids: Vec<u32> = ids_by_author(&doc.snapshot().canonical, "Sentinel")
        .into_iter()
        .collect();
    all_ids.sort_unstable();
    sweep_doc(&doc, &all_ids, label)
}

/// The insert-tail fixture the focused sentinels share: three plain paragraphs
/// with TWO tracked paragraphs appended after the last one. Returns the parsed
/// `Document` (post-edit), the anchor's shifted mark-insertion id, and the two
/// block-insertion ids in document order.
struct InsertTailCase {
    doc: stemma::api::Document,
    anchor_mark_id: u32,
    block_insert_ids: Vec<u32>,
}

fn insert_tail_case() -> InsertTailCase {
    let body = format!("{}{}{}", para("Alpha."), para("Beta."), para("Gamma."));
    let fixture = import_body(&body);
    let anchor = fixture.para_ids.last().unwrap().clone();
    let tx = insert_after(&anchor, &["Inserted A.", "Inserted B."]);
    let edited = std::sync::Arc::unwrap_or_clone(
        fixture
            .runtime
            .apply_edit(&fixture.handle, &tx)
            .unwrap()
            .canonical,
    );

    // The anchor (a surviving, block-Normal paragraph) carries the shifted
    // mark-insertion; the two appended paragraphs are block-level insertions.
    let mut anchor_mark_id = None;
    let mut block_insert_ids = Vec::new();
    for tb in &edited.blocks {
        if let TrackingStatus::Inserted(r) = &tb.status {
            // H7: address revisions by the engine-minted identity, not wire id.
            block_insert_ids.push(r.identity);
        } else if let BlockNode::Paragraph(p) = &tb.block
            && let Some(TrackingStatus::Inserted(r)) = &p.para_mark_status
        {
            anchor_mark_id = Some(r.identity);
        }
    }
    let doc = stemma::api::Document::parse(&make_docx_with_body(&body))
        .unwrap()
        .apply(&tx)
        .unwrap();
    InsertTailCase {
        doc,
        anchor_mark_id: anchor_mark_id.expect("anchor carries a shifted mark-insertion"),
        block_insert_ids,
    }
}

/// Project `ids`/`action` selectively and return the last `w:p`'s pilcrow
/// marker presence plus the projected accept-all / reject-all text of whatever
/// revisions REMAIN — the three facts every sentinel below asserts on.
fn selective_final_mark_facts(
    doc: &stemma::api::Document,
    ids: &[u32],
    action: ResolveSelectionAction,
) -> (bool, String, String) {
    let set: std::collections::HashSet<u32> = ids.iter().copied().collect();
    let res = doc
        .project(stemma::Resolution::Selective { ids: set, action })
        .expect("selective projection");
    let bytes = res.serialize(&stemma::ExportOptions::default()).unwrap();
    let root = parse_document_xml(&bytes);
    let wlast = last_paragraph(&root);
    let tracked = paragraph_mark_has(wlast, "ins") || paragraph_mark_has(wlast, "del");
    let (accept, reject) = accept_reject_text(&res.snapshot().canonical);
    (tracked, accept, reject)
}

/// Accepting ONLY the shifted tail mark (leaving both block insertions tracked)
/// must not strand the block insertion on the document-final pilcrow.
#[test]
fn selective_accept_of_only_the_tail_mark_leaves_final_mark_untracked() {
    let case = insert_tail_case();
    let (tracked, accept, reject) = selective_final_mark_facts(
        &case.doc,
        &[case.anchor_mark_id],
        ResolveSelectionAction::Accept,
    );
    assert!(
        !tracked,
        "document-final pilcrow must carry no w:ins/w:del after accepting the tail mark"
    );
    // The two block insertions remain: accept keeps them, reject restores base.
    assert_eq!(accept, "Alpha.\nBeta.\nGamma.\nInserted A.\nInserted B.");
    assert_eq!(reject, "Alpha.\nBeta.\nGamma.");
}

/// Rejecting the LAST block insertion (leaving the tail mark and the first
/// block insertion tracked) makes the first insertion the final paragraph — its
/// block-level insertion must not surface on the document-final pilcrow.
#[test]
fn selective_reject_of_last_block_insert_leaves_final_mark_untracked() {
    let case = insert_tail_case();
    let last_block_insert = *case.block_insert_ids.last().unwrap();
    let (tracked, accept, reject) = selective_final_mark_facts(
        &case.doc,
        &[last_block_insert],
        ResolveSelectionAction::Reject,
    );
    assert!(
        !tracked,
        "document-final pilcrow must carry no w:ins/w:del after rejecting the last block insert"
    );
    // The first block insertion + the tail mark remain: accept keeps the first
    // inserted paragraph, reject restores the original.
    assert_eq!(accept, "Alpha.\nBeta.\nGamma.\nInserted A.");
    assert_eq!(reject, "Alpha.\nBeta.\nGamma.");
}

/// Rejecting BOTH block insertions strands the anchor (which still carries the
/// pending mark-insertion) as the final paragraph. The pass materializes the
/// empty inserted trailing paragraph that pending break introduces, so the
/// final pilcrow is untracked and the surviving revision stays resolvable:
/// rejecting it restores the original, accepting it keeps the empty tail.
#[test]
fn selective_reject_stranding_the_anchor_materializes_empty_tail() {
    let case = insert_tail_case();
    let (tracked, accept, reject) = selective_final_mark_facts(
        &case.doc,
        &case.block_insert_ids,
        ResolveSelectionAction::Reject,
    );
    assert!(
        !tracked,
        "document-final pilcrow must carry no w:ins/w:del after stranding the anchor"
    );
    // The tail mark alone remains. Rejecting it merges the empty tail back onto
    // the original final paragraph; accepting it keeps the empty trailing para.
    assert_eq!(reject, "Alpha.\nBeta.\nGamma.");
    assert_eq!(accept, "Alpha.\nBeta.\nGamma.\n");
}

/// Comprehensive invariant: for the three tail-shaped edits (insert, delete,
/// move-to-end), EVERY non-empty subset of the produced revisions × both
/// actions must project to a document whose final pilcrow is untracked, that
/// re-parses, and that validates clean.
#[test]
fn selective_resolution_never_strands_a_tracked_final_mark() {
    let insert_body = format!("{}{}{}", para("Alpha."), para("Beta."), para("Gamma."));
    let anchor = import_body(&insert_body).para_ids.last().unwrap().clone();
    let insert_tx = insert_after(&anchor, &["Inserted A.", "Inserted B."]);
    let mut bad = sweep_selective_final_mark(&insert_body, &insert_tx, "insert-tail");

    // Delete tail: delete the last two paragraphs.
    let del_fixture = import_body(&insert_body);
    let v = del_fixture.para_ids.last().unwrap().clone();
    let v2 = del_fixture.para_ids[del_fixture.para_ids.len() - 2].clone();
    let del_tx = EditTransaction {
        steps: vec![EditStep::DeleteBlockRange {
            from_block_id: v2.clone(),
            to_block_id: v.clone(),
            rationale: None,
            expect: "Beta.".to_string(),
            semantic_hash: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };
    bad.extend(sweep_selective_final_mark(
        &insert_body,
        &del_tx,
        "delete-tail",
    ));

    // Move a middle paragraph to AFTER the last (moveTo destination at doc end).
    let five = format!(
        "{}{}{}{}{}",
        para("Para1."),
        para("Para2."),
        para("Para3."),
        para("Para4."),
        para("Para5.")
    );
    let five_fx = import_body(&five);
    let mv_tx = move_after(
        &five_fx.para_ids[1],
        &five_fx.para_ids[1],
        &five_fx.para_ids[4],
    );
    bad.extend(sweep_selective_final_mark(&five, &mv_tx, "move-tail"));

    // tail-adjacency bases: the renormalizer must keep each clean
    // through Selective too (two consecutive moves; two consecutive tail
    // inserts; insert-then-move; move-then-insert).
    let orig5 = import_body(&five).para_ids;
    let two_moves = consecutive_move_to_end_doc(&five, &[1, 2]);

    let mut two_inserts = stemma::api::Document::parse(&make_docx_with_body(&five)).unwrap();
    for t in ["Ins A.", "Ins B."] {
        two_inserts = ins_after_last(two_inserts, t);
    }

    let mut insert_then_move = stemma::api::Document::parse(&make_docx_with_body(&five)).unwrap();
    insert_then_move = ins_after_last(insert_then_move, "Inserted.");
    insert_then_move = move_orig_to_last(insert_then_move, &orig5, 1);

    let mut move_then_insert = stemma::api::Document::parse(&make_docx_with_body(&five)).unwrap();
    move_then_insert = move_orig_to_last(move_then_insert, &orig5, 1);
    move_then_insert = ins_after_last(move_then_insert, "Inserted.");

    for (doc, label) in [
        (&two_moves, "two-move-tail"),
        (&two_inserts, "two-insert-tail"),
        (&insert_then_move, "insert-then-move"),
        (&move_then_insert, "move-then-insert"),
    ] {
        let mut ids: Vec<u32> = ids_by_author(&doc.snapshot().canonical, "Sentinel")
            .into_iter()
            .collect();
        ids.sort_unstable();
        bad.extend(sweep_doc(doc, &ids, label));
    }

    assert!(
        bad.is_empty(),
        "selective resolution left a tracked final mark:\n{}",
        bad.join("\n")
    );
}

/// A `Document` on `body` with a chain of consecutive MoveBlockRange ops, each
/// moving the ORIGINAL paragraph at `move_indices[k]` (0-based into the initial
/// paragraph order) to AFTER the current document-final paragraph. This is the
/// consecutive-move-to-end shape.
fn consecutive_move_to_end_doc(body: &str, move_indices: &[usize]) -> stemma::api::Document {
    let orig_ids = import_body(body).para_ids;
    let mut doc = stemma::api::Document::parse(&make_docx_with_body(body)).expect("parse");
    for &idx in move_indices {
        let last = NodeId::from(doc.read().blocks.last().unwrap().id.to_string());
        let from = orig_ids[idx].clone();
        doc = doc
            .apply(&move_after(&from, &from, &last))
            .expect("move to end");
    }
    doc
}

// ── Consecutive moves to the document end (mint-time sibling) ─────────────────
//
// The FIRST MoveBlockRange to the document end normalizes correctly (its anchor
// is a surviving paragraph). The SECOND move's destination lands after the
// FIRST move's destination copy, so `normalize_moved_final_mark` walks back to a
// block-`Inserted` + `move_id` anchor. It used to bail there ("anchor is a move
// half"), leaving the second move's mark-insertion on the DOCUMENT-FINAL
// pilcrow — the Word-unresolvable state. The previous destination copy IS the
// right anchor (the paragraph immediately before this move's run); shifting the
// plain break onto its pilcrow touches only the marker, never either move's
// run-level pairing.

/// N consecutive moves to the document end must leave the final pilcrow
/// untracked, validate clean, and project to the correct moved / original order.
fn assert_consecutive_move_tail_clean(
    body: &str,
    move_indices: &[usize],
    expected_accept: &str,
    expected_reject: &str,
) {
    let doc = consecutive_move_to_end_doc(body, move_indices);
    let bytes = doc
        .serialize(&stemma::ExportOptions::default())
        .expect("serialize");
    let root = parse_document_xml(&bytes);
    let last = last_paragraph(&root);
    for marker in ["ins", "del", "moveTo", "moveFrom"] {
        assert!(
            !paragraph_mark_has(last, marker),
            "document-final w:p must carry no paragraph-mark {marker} after {} moves",
            move_indices.len()
        );
    }
    assert!(
        stemma::api::validate(&bytes).ok,
        "consecutive-move-tail export must validate clean"
    );
    let (accept, reject) = accept_reject_text(&doc.snapshot().canonical);
    assert_eq!(accept, expected_accept, "accept-all order");
    assert_eq!(reject, expected_reject, "reject-all restores the original");
}

#[test]
fn two_consecutive_moves_to_end_leave_final_mark_untracked() {
    let body = format!(
        "{}{}{}{}{}",
        para("Para1."),
        para("Para2."),
        para("Para3."),
        para("Para4."),
        para("Para5.")
    );
    // Move Para2 to end, then Para3 to the new end: [P1 P4 P5 P2 P3].
    assert_consecutive_move_tail_clean(
        &body,
        &[1, 2],
        "Para1.\nPara4.\nPara5.\nPara2.\nPara3.",
        "Para1.\nPara2.\nPara3.\nPara4.\nPara5.",
    );
}

#[test]
fn three_consecutive_moves_to_end_leave_final_mark_untracked() {
    let body = format!(
        "{}{}{}{}{}",
        para("Para1."),
        para("Para2."),
        para("Para3."),
        para("Para4."),
        para("Para5.")
    );
    // Move Para2, then Para3, then Para4, each to the end: [P1 P5 P2 P3 P4].
    assert_consecutive_move_tail_clean(
        &body,
        &[1, 2, 3],
        "Para1.\nPara5.\nPara2.\nPara3.\nPara4.",
        "Para1.\nPara2.\nPara3.\nPara4.\nPara5.",
    );
}

// ── The tail-adjacency family (insert / move producers) ──────────────────────
//
// A tail-marking producer (InsertParagraphs-after-last, MoveBlockRange-to-end)
// attributes the newly-inserted break to the paragraph immediately before its
// destination run. That anchor is usually a surviving paragraph, but a SECOND
// tail op lands after the FIRST op's destination copy. The move-after-move case
// fixed the move normalizer (which bailed on a block-`Inserted` + `move_id`
// anchor). Its insert sibling is MOVE-then-INSERT: it left the insert's final
// paragraph after a moveTo destination copy, and `normalize_inserted_final_mark`
// bailed on that same anchor — leaving the insert's mark on the document-final
// pilcrow. Both normalizers now share ONE anchor rule: a moveTo DESTINATION
// anchor receives the shifted break (only the pilcrow marker moves, never the
// move's run-level pairing); only a moveFrom SHADOW anchor is left to its own
// pairing. (Plain insert-after-insert was already clean — the insert walk-back
// consumes the plain inserts and lands on the surviving anchor.)

fn final_flags(bytes: &[u8]) -> Vec<&'static str> {
    let root = parse_document_xml(bytes);
    let last = last_paragraph(&root);
    ["ins", "del", "moveTo", "moveFrom"]
        .into_iter()
        .filter(|m| paragraph_mark_has(last, m))
        .collect()
}

fn ins_after_last(doc: stemma::api::Document, text: &str) -> stemma::api::Document {
    let last = NodeId::from(doc.read().blocks.last().unwrap().id.to_string());
    doc.apply(&insert_after(&last, &[text])).unwrap()
}

fn move_orig_to_last(
    doc: stemma::api::Document,
    orig_ids: &[NodeId],
    idx: usize,
) -> stemma::api::Document {
    let last = NodeId::from(doc.read().blocks.last().unwrap().id.to_string());
    let from = orig_ids[idx].clone();
    doc.apply(&move_after(&from, &from, &last)).unwrap()
}

/// Assert a tail-op chain leaves the document-final pilcrow untracked in the
/// redline, that the redline validates, and that accept-all / reject-all both
/// resolve to valid, flag-free documents with the expected text.
fn assert_tail_clean(doc: &stemma::api::Document, expected_accept: &str, expected_reject: &str) {
    let bytes = doc.serialize(&stemma::ExportOptions::default()).unwrap();
    assert!(
        final_flags(&bytes).is_empty(),
        "redline document-final pilcrow must be untracked, got {:?}",
        final_flags(&bytes)
    );
    assert!(stemma::api::validate(&bytes).ok, "redline must validate");
    for (res, name) in [
        (stemma::Resolution::AcceptAll, "accept-all"),
        (stemma::Resolution::RejectAll, "reject-all"),
    ] {
        let out = doc
            .project(res)
            .unwrap()
            .serialize(&stemma::ExportOptions::default())
            .unwrap();
        assert!(
            final_flags(&out).is_empty(),
            "{name} must clear the final mark"
        );
        assert!(
            stemma::api::validate(&out).ok,
            "{name} output must validate"
        );
    }
    let (accept, reject) = accept_reject_text(&doc.snapshot().canonical);
    assert_eq!(accept, expected_accept, "accept-all text");
    assert_eq!(reject, expected_reject, "reject-all text");
}

fn body3() -> String {
    format!("{}{}{}", para("Alpha."), para("Beta."), para("Gamma."))
}

fn body5() -> String {
    format!(
        "{}{}{}{}{}",
        para("Para1."),
        para("Para2."),
        para("Para3."),
        para("Para4."),
        para("Para5.")
    )
}

#[test]
fn two_consecutive_tail_inserts_leave_final_mark_untracked() {
    let mut doc = stemma::api::Document::parse(&make_docx_with_body(&body3())).unwrap();
    for t in ["One.", "Two."] {
        doc = ins_after_last(doc, t);
    }
    assert_tail_clean(
        &doc,
        "Alpha.\nBeta.\nGamma.\nOne.\nTwo.",
        "Alpha.\nBeta.\nGamma.",
    );
}

#[test]
fn three_consecutive_tail_inserts_leave_final_mark_untracked() {
    let mut doc = stemma::api::Document::parse(&make_docx_with_body(&body3())).unwrap();
    for t in ["One.", "Two.", "Three."] {
        doc = ins_after_last(doc, t);
    }
    assert_tail_clean(
        &doc,
        "Alpha.\nBeta.\nGamma.\nOne.\nTwo.\nThree.",
        "Alpha.\nBeta.\nGamma.",
    );
}

#[test]
fn insert_then_move_to_end_leaves_final_mark_untracked() {
    let body = body5();
    let orig = import_body(&body).para_ids;
    let mut doc = stemma::api::Document::parse(&make_docx_with_body(&body)).unwrap();
    doc = ins_after_last(doc, "Inserted.");
    doc = move_orig_to_last(doc, &orig, 1); // Para2 to the end
    assert_tail_clean(
        &doc,
        "Para1.\nPara3.\nPara4.\nPara5.\nInserted.\nPara2.",
        "Para1.\nPara2.\nPara3.\nPara4.\nPara5.",
    );
}

/// Core case: MOVE-to-end, then INSERT after the moved-in destination.
/// The insert's final paragraph lands after a moveTo destination copy; without
/// the shared anchor rule its block-insertion mark strands on the final pilcrow.
#[test]
fn move_to_end_then_insert_leaves_final_mark_untracked() {
    let body = body5();
    let orig = import_body(&body).para_ids;
    let mut doc = stemma::api::Document::parse(&make_docx_with_body(&body)).unwrap();
    doc = move_orig_to_last(doc, &orig, 1); // Para2 to the end
    doc = ins_after_last(doc, "Inserted.");
    assert_tail_clean(
        &doc,
        "Para1.\nPara3.\nPara4.\nPara5.\nPara2.\nInserted.",
        "Para1.\nPara2.\nPara3.\nPara4.\nPara5.",
    );
}

/// Interior move whose SOURCE is the document-final paragraph (to_end:false):
/// the moveFrom SHADOW of the moved-away last paragraph sits at the tail with a
/// paragraph-mark deletion. This is NOT a stranded final-mark state — the mark is
/// PAIRED with the moveTo destination and resolves via that pairing: accept-all
/// and reject-all both clear it to a valid, untracked-final-mark document. The
/// redline pilcrow carries the move-paired `w:del` by design (the engine leaves
/// move halves to their own pairing); a final-mark WIRE CHECKER that flags any
/// pilcrow `w:ins`/`w:del` over-flags this move-paired shape.
#[test]
fn interior_move_of_final_paragraph_is_move_paired_not_stranded() {
    let body = body5();
    let orig = import_body(&body).para_ids;
    let doc = stemma::api::Document::parse(&make_docx_with_body(&body))
        .unwrap()
        .apply(&move_after(&orig[4], &orig[4], &orig[0])) // Para5 (last) to after Para1
        .unwrap();

    // The final block is a moveFrom shadow (block-Deleted + move_id) — its mark
    // is resolved by the move pairing, so the engine leaves it in place.
    let last_block = doc.snapshot().canonical.blocks.last().unwrap();
    assert!(
        matches!(last_block.status, TrackingStatus::Deleted(_)) && last_block.move_id.is_some(),
        "the moved-away final paragraph's shadow ends the document as a moveFrom half"
    );

    // Both resolutions clear it: accept removes the moveFrom source (the
    // preceding paragraph becomes the untracked final mark), reject restores it.
    for (res, name) in [
        (stemma::Resolution::AcceptAll, "accept-all"),
        (stemma::Resolution::RejectAll, "reject-all"),
    ] {
        let out = doc
            .project(res)
            .unwrap()
            .serialize(&stemma::ExportOptions::default())
            .unwrap();
        assert!(
            final_flags(&out).is_empty(),
            "{name} of a from-end move must leave the final pilcrow untracked, got {:?}",
            final_flags(&out)
        );
        assert!(
            stemma::api::validate(&out).ok,
            "{name} output must validate"
        );
    }
    let (accept, reject) = accept_reject_text(&doc.snapshot().canonical);
    assert_eq!(accept, "Para1.\nPara5.\nPara2.\nPara3.\nPara4.");
    assert_eq!(reject, "Para1.\nPara2.\nPara3.\nPara4.\nPara5.");
}

// ── (v) interior→interior move: the tail is untouched (regression pin) ───────

#[test]
fn interior_move_keeps_marks_and_leaves_tail_untouched() {
    let fixture = five_para_fixture();
    let from = fixture.para_ids[1].clone(); // Para2
    let anchor = fixture.para_ids[2].clone(); // after Para3 (interior)
    let tx = move_after(&from, &from, &anchor);
    let (canonical, root) = apply_and_export(&fixture, &tx);

    // The document-final paragraph (Para5) is untouched — no marker.
    let last = last_paragraph(&root);
    for marker in ["ins", "del", "moveTo", "moveFrom"] {
        assert!(
            !paragraph_mark_has(last, marker),
            "untouched final w:p must not carry a paragraph-mark {marker}"
        );
    }

    let blocks = &canonical.blocks;
    // The moved-in clone keeps its own inserted (moveTo) mark, driven by block
    // status; the anchor must NOT gain a shifted mark.
    let clone = blocks
        .iter()
        .find(|tb| matches!(tb.status, TrackingStatus::Inserted(_)) && tb.move_id.is_some())
        .expect("moveTo clone present");
    assert!(
        matches!(effective_final_mark(clone), TrackingStatus::Inserted(_)),
        "an interior moveTo paragraph keeps its inserted mark"
    );
    assert!(
        !matches!(
            para_mark_of(blocks, &anchor),
            Some(TrackingStatus::Inserted(_))
        ),
        "interior-move anchor must not carry a shifted insertion mark"
    );

    let (accept, reject) = accept_reject_text(&canonical);
    assert_eq!(reject, "Para1.\nPara2.\nPara3.\nPara4.\nPara5.");
    assert_eq!(accept, "Para1.\nPara3.\nPara2.\nPara4.\nPara5.");
}

// ── A moved paragraph's pilcrow carries the MOVE marker ──────────────────────
//
// A tracked move wraps the paragraph's runs in w:moveTo / w:moveFrom. Its
// terminating pilcrow mark is PART of the move, so it must serialize as
// w:moveTo / w:moveFrom inside w:pPr/w:rPr — NOT a plain w:ins / w:del. Real
// Word (COM-fabricated native move) emits exactly this; a standalone w:ins on a
// moveTo destination's pilcrow is an INDEPENDENT paragraph-mark insertion Word
// rejects on its own, merging the paragraph with its neighbour (the wire-vs-Word
// divergence). Import already reads pilcrow moveTo/moveFrom as an
// Inserted/Deleted mark (word_ir::extract_para_mark_status), so the shape
// round-trips and the mark coalesces into the move revision.

/// Which tracked marker (if any) a paragraph's pilcrow rPr carries, and whether
/// its runs are a moveTo / moveFrom container.
fn paragraph_move_shape(el: &Element) -> (Vec<&'static str>, bool, bool) {
    let pilcrow: Vec<&'static str> = ["ins", "del", "moveTo", "moveFrom"]
        .into_iter()
        .filter(|m| paragraph_mark_has(el, m))
        .collect();
    let run_has = |name: &str| {
        el.children
            .iter()
            .any(|c| matches!(c, xmltree::XMLNode::Element(e) if e.name == name))
    };
    (pilcrow, run_has("moveTo"), run_has("moveFrom"))
}

/// Serialize `doc` and return its body `w:p` elements.
fn body_paragraphs(doc: &stemma::api::Document) -> Vec<Element> {
    let bytes = doc.serialize(&stemma::ExportOptions::default()).unwrap();
    let root = parse_document_xml(&bytes);
    body_element(&root)
        .children
        .iter()
        .filter_map(|c| match c {
            xmltree::XMLNode::Element(e) if e.name == "p" => Some(e.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn moved_paragraph_pilcrow_carries_move_marker_not_plain_ins_del() {
    let body = body5();
    let orig = import_body(&body).para_ids;
    // Move Para2 to an interior position (after Para4): produces both a moveTo
    // destination copy and a moveFrom source shadow, each a full paragraph.
    let doc = stemma::api::Document::parse(&make_docx_with_body(&body))
        .unwrap()
        .apply(&move_after(&orig[1], &orig[1], &orig[3]))
        .unwrap();

    let mut saw_move_to = false;
    let mut saw_move_from = false;
    for p in body_paragraphs(&doc) {
        let (pilcrow, runs_move_to, runs_move_from) = paragraph_move_shape(&p);
        if runs_move_to {
            saw_move_to = true;
            assert!(
                pilcrow.contains(&"moveTo") && !pilcrow.contains(&"ins"),
                "a moveTo-destination paragraph's pilcrow must be w:moveTo, not w:ins; got {pilcrow:?}"
            );
        }
        if runs_move_from {
            saw_move_from = true;
            assert!(
                pilcrow.contains(&"moveFrom") && !pilcrow.contains(&"del"),
                "a moveFrom-source paragraph's pilcrow must be w:moveFrom, not w:del; got {pilcrow:?}"
            );
        }
    }
    assert!(
        saw_move_to && saw_move_from,
        "the move produced both halves"
    );

    // The document validates and resolves cleanly on both sides.
    assert!(stemma::api::validate(&doc.serialize(&stemma::ExportOptions::default()).unwrap()).ok);
    let (accept, reject) = accept_reject_text(&doc.snapshot().canonical);
    assert_eq!(accept, "Para1.\nPara3.\nPara4.\nPara2.\nPara5.");
    assert_eq!(reject, "Para1.\nPara2.\nPara3.\nPara4.\nPara5.");
}

#[test]
fn moved_paragraph_pilcrow_move_marker_round_trips() {
    let body = body5();
    let orig = import_body(&body).para_ids;
    let doc = stemma::api::Document::parse(&make_docx_with_body(&body))
        .unwrap()
        .apply(&move_after(&orig[1], &orig[1], &orig[3]))
        .unwrap();
    // Re-import the serialized wire: pilcrow moveTo/moveFrom must be read back as
    // a tracked mark that coalesces into the move (block move_id set, mark
    // tracked) — not dropped to an untracked pilcrow.
    let bytes = doc.serialize(&stemma::ExportOptions::default()).unwrap();
    let reimported = stemma::api::Document::parse(&bytes).unwrap();
    let mut move_dest_marked = false;
    let mut move_src_marked = false;
    for tb in &reimported.snapshot().canonical.blocks {
        if tb.move_id.is_none() {
            continue;
        }
        if let BlockNode::Paragraph(p) = &tb.block {
            match tb.status {
                TrackingStatus::Inserted(_) => {
                    assert!(
                        matches!(p.para_mark_status, Some(TrackingStatus::Inserted(_))),
                        "moveTo destination pilcrow must round-trip as an Inserted mark"
                    );
                    move_dest_marked = true;
                }
                TrackingStatus::Deleted(_) => {
                    assert!(
                        matches!(p.para_mark_status, Some(TrackingStatus::Deleted(_))),
                        "moveFrom source pilcrow must round-trip as a Deleted mark"
                    );
                    move_src_marked = true;
                }
                _ => {}
            }
        }
    }
    assert!(
        move_dest_marked && move_src_marked,
        "both move halves round-tripped"
    );
    // Reject on the re-imported doc still restores the original order.
    let (_, reject) = accept_reject_text(&reimported.snapshot().canonical);
    assert_eq!(reject, "Para1.\nPara2.\nPara3.\nPara4.\nPara5.");
}

#[test]
fn suppressed_final_mark_still_serializes_untracked_after_move_marker_fix() {
    // A move-to-end shape: the moved-in final paragraph's pilcrow is
    // SUPPRESSED (para_mark Some(Normal)); the is_move move-marker branch must
    // not resurrect a marker on it. The final pilcrow stays untracked.
    let body = body5();
    let orig = import_body(&body).para_ids;
    let last = NodeId::from(
        stemma::api::Document::parse(&make_docx_with_body(&body))
            .unwrap()
            .read()
            .blocks
            .last()
            .unwrap()
            .id
            .to_string(),
    );
    let doc = stemma::api::Document::parse(&make_docx_with_body(&body))
        .unwrap()
        .apply(&move_after(&orig[1], &orig[1], &last))
        .unwrap();
    let bytes = doc.serialize(&stemma::ExportOptions::default()).unwrap();
    let root = parse_document_xml(&bytes);
    let last_p = last_paragraph(&root);
    for marker in ["ins", "del", "moveTo", "moveFrom"] {
        assert!(
            !paragraph_mark_has(last_p, marker),
            "document-final pilcrow must stay untracked (suppressed), got {marker}"
        );
    }
}

// ── Follow-up: selectively-resolved move states agree wire vs model ──────────
//
// Selectively resolving ONE constituent id of a move produces a fragmented
// state (e.g. rejecting a moveFrom source's block-deletion restores its block
// to Normal while its mark-deletion — now a moveFrom PILCROW after the F11
// serialize fix — stays pending). The wire accept/reject (normalize_docx /
// reject_all_docx) must resolve that pilcrow move marker exactly as the model's
// projection does: a moveFrom pilcrow is deletion-class (merges on accept), a
// moveTo pilcrow insertion-class (merges on reject). Without the move-aware
// para_mark_markers, the wire fails to chain a merge across a genuine moveFrom
// source and drifts from the model.

fn resolved_text_multiset(bytes: &[u8]) -> Vec<String> {
    let doc = stemma::api::Document::parse(bytes).expect("reparse resolved");
    let mut v: Vec<String> = doc
        .to_text()
        .split('\n')
        .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|s| !s.is_empty())
        .collect();
    v.sort();
    v
}

#[test]
fn move_selective_states_agree_wire_vs_model_on_accept_and_reject() {
    use stemma::docx::DocxArchive;
    use stemma::normalize::{normalize_docx, reject_all_docx};
    use stemma::tracked_model::enumerate_revisions;

    let body = format!(
        "{}{}{}{}",
        para("Para1."),
        para("Para2."),
        para("Para3."),
        para("Para4.")
    );
    let orig = import_body(&body).para_ids;
    // Range move: [Para2, Para3] to after Para4 — moveFrom sources + moveTo dests.
    let doc = stemma::api::Document::parse(&make_docx_with_body(&body))
        .unwrap()
        .apply(&move_after(&orig[1], &orig[2], &orig[3]))
        .unwrap();

    let records: Vec<_> = enumerate_revisions(&doc.snapshot().canonical)
        .into_iter()
        .filter(|r| r.author.as_deref() == Some("Sentinel"))
        .collect();
    // RFC-0004 §H7: a MOVE is ONE user intention. A two-block range move — its
    // source content + source pilcrows + destination clones, all sharing one
    // move_id — enumerates as ONE `Move` record under a SINGLE minted identity
    // (it cannot strand a constituent). Any final-mark shift the move induces on
    // a surviving paragraph is a SEPARATE, non-move revision.
    let move_records = records
        .iter()
        .filter(|r| r.kind == stemma::RevisionKind::Move)
        .count();
    assert_eq!(
        move_records, 1,
        "the range move must enumerate as exactly one atomic Move record, got {records:?}"
    );
    let mut ids: Vec<u32> = records.iter().map(|r| r.revision_id).collect();
    ids.sort_unstable();
    ids.dedup();

    // Single-id and all-but-one subsets: the shapes that strand one constituent.
    let mut subsets: Vec<Vec<u32>> = ids.iter().map(|&i| vec![i]).collect();
    for &skip in &ids {
        let comp: Vec<u32> = ids.iter().copied().filter(|&i| i != skip).collect();
        if comp.len() >= 2 {
            subsets.push(comp);
        }
    }
    subsets.push(ids.clone());

    let export = stemma::ExportOptions::unchecked();
    for action in [
        ResolveSelectionAction::Accept,
        ResolveSelectionAction::Reject,
    ] {
        for sub in &subsets {
            let set: std::collections::HashSet<u32> = sub.iter().copied().collect();
            let composed = doc
                .project(stemma::Resolution::Selective { ids: set, action })
                .unwrap_or_else(|e| panic!("selective {action:?} {sub:?}: {e:?}"));
            let wire = composed.serialize(&export).unwrap();
            let arch = DocxArchive::read(&wire).unwrap();

            let (wire_acc, _) = normalize_docx(&arch).unwrap();
            let model_acc = composed
                .project(stemma::Resolution::AcceptAll)
                .unwrap()
                .serialize(&export)
                .unwrap();
            assert_eq!(
                resolved_text_multiset(&wire_acc.write().unwrap()),
                resolved_text_multiset(&model_acc),
                "ACCEPT wire vs model diverged for {action:?} subset {sub:?}"
            );

            let (wire_rej, _) = reject_all_docx(&arch).unwrap();
            let model_rej = composed
                .project(stemma::Resolution::RejectAll)
                .unwrap()
                .serialize(&export)
                .unwrap();
            assert_eq!(
                resolved_text_multiset(&wire_rej.write().unwrap()),
                resolved_text_multiset(&model_rej),
                "REJECT wire vs model diverged for {action:?} subset {sub:?}"
            );
        }
    }
}
