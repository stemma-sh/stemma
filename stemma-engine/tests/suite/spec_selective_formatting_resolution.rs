//! Selective (by-id) resolution of tracked FORMATTING changes.
//!
//! THE CONTRACT: a tracked formatting change is a revision like any other —
//! it carries a stable `revision_id` (parsed from `w:id`, stored on the
//! model, re-emitted by the serializer), it is enumerable
//! (`tracked_model::enumerate_revisions`), and resolving it by id behaves
//! exactly like Word: accept keeps the new formatting and discards the change
//! record; reject restores the COMPLETE previous state the record snapshotted.
//! Unselected formatting changes stay pending and round-trip with their id
//! intact.
//!
//! Closes the formatting half of the enumeration gap; the structural half
//! (table rows/cells) is pinned by spec_revision_enumeration.rs.
//!
//! CLASS AUDIT: every formatting-change carrier the selective
//! (by-id) resolution surface claims to reach, in one file, each checked
//! through BOTH the in-memory canonical model AND a real save+reparse round
//! trip (not just `.project()` — a bug that only manifests after
//! serialization, like this one, would hide behind an in-memory-only check).
//! Found via this audit: `SetRunFormatting`'s dispatch arm (edit/mod.rs) was
//! the ONLY formatting verb dispatch that never called `stamp_revision`
//! before invoking its verb — it passed the transaction's raw, un-stamped
//! `revision` straight through, so every run-level `w:rPrChange`
//! (`FormattingChange.revision_id`, domain/mod.rs) was born as the `0`
//! legacy sentinel. `project_block_for_selected_resolution`
//! (tracked_model.rs) already correctly resolves run-level formatting by
//! id — gated behind `fc.revision_id != 0`, the same sentinel guard every
//! other carrier here relies on — so a sentinel id silently satisfies
//! neither accept nor reject: `run_rprchange_reject_by_id_...` below is the
//! refutation (fails pre-fix, confirming the bug is in the CREATION path,
//! not resolution). Paragraph (`pPrChange`), table (`tblPrChange`), row
//! (`trPrChange`), and cell (`tcPrChange`) formatting all stamp correctly
//! (their dispatch arms call `stamp_revision` before or inside their verb)
//! and are PINNED here, not just asserted to exist — this file is now the
//! one place that proves the whole class, carrier by carrier, rather than
//! trusting that "similar-looking code does the similar-looking right
//! thing."

use std::collections::HashSet;

use stemma::api::Document;
use stemma::domain::*;
use stemma::edit::{
    CellFormattingPatch, EditStep, EditTransaction, InlineMarkSet, MaterializationMode,
    ParagraphFormattingPatch, RowFormattingPatch, RunStyleEdit, TableFormattingPatch,
};
use stemma::tracked_model::{ResolveSelectionAction, RevisionKind, enumerate_revisions};
use stemma::{Alignment, BlockNode, Resolution, RevisionInfo};

/// Resolve `id` under `action`, then serialize and RE-PARSE the result — the
/// persisted-markup half of every check in this file. A bug that only shows
/// up after a save (like the one this file's class audit found) is invisible
/// to a `.project()`-only check; parsing the engine's own serialized bytes
/// back through its own parser is the strongest form of "does this survive
/// persistence" without hand-rolling XML inspection (dialect-safe by
/// construction — nothing here compares raw fragments).
fn resolve_and_persist(doc: &Document, id: u32, action: ResolveSelectionAction) -> Document {
    let resolved = doc
        .project(Resolution::Selective {
            ids: HashSet::from([id]),
            action,
        })
        .unwrap_or_else(|e| panic!("selective {action:?} of id {id} applies: {e}"));
    let bytes = resolved
        .serialize(&stemma::ExportOptions {
            mode: stemma::ExportMode::Redline,
            validator_level: stemma::ValidatorLevel::Blocking,
            validator: None,
        })
        .unwrap_or_else(|e| panic!("resolved doc serializes clean: {e}"));
    Document::parse(&bytes).expect("resolved doc re-parses")
}

fn make_docx_with_body(body_inner: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
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

const TWO_PARAS: &str = r#"<w:p><w:r><w:t>Centered later.</w:t></w:r></w:p><w:p><w:r><w:t>Left alone.</w:t></w:r></w:p>"#;

/// A doc whose first paragraph carries a pPrChange (center, was default).
/// The engine assigns each tracked change its own id from the document's
/// revision counter (the transaction's id is a seed, not the per-change id) —
/// callers discover the assigned id via `ppr_change_id`.
fn doc_with_ppr_change(id: u32) -> Document {
    let doc = Document::parse(&make_docx_with_body(TWO_PARAS)).expect("parse");
    let view = doc.read();
    let target = view
        .blocks
        .iter()
        .find(|b| b.text.contains("Centered later"))
        .expect("target paragraph");
    doc.apply(&EditTransaction {
        steps: vec![EditStep::SetParagraphFormatting {
            block_id: target.id.clone(),
            semantic_hash: Some(target.guard.clone()),
            patch: ParagraphFormattingPatch {
                align: Some(Alignment::Center),
                ..Default::default()
            },
            rationale: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: id,
            identity: 0,
            author: Some("fmt-test".to_string()),
            date: Some("2026-06-12T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    })
    .expect("tracked pPrChange applies")
}

fn first_para(doc: &Document) -> &stemma::ParagraphNode {
    doc.snapshot()
        .canonical
        .blocks
        .iter()
        .find_map(|tb| match &tb.block {
            BlockNode::Paragraph(p)
                if p.segments.iter().any(|s| {
                    s.inlines.iter().any(
                        |i| matches!(i, stemma::InlineNode::Text(t) if t.text.contains("Centered")),
                    )
                }) =>
            {
                Some(p)
            }
            _ => None,
        })
        .expect("target paragraph in canonical")
}

/// The engine-assigned id of the document's single formatting revision.
fn ppr_change_id(doc: &Document) -> u32 {
    let records = enumerate_revisions(&doc.snapshot().canonical);
    let fmt: Vec<_> = records.iter().filter(|r| r.kind.is_format()).collect();
    assert_eq!(fmt.len(), 1, "exactly one formatting revision: {records:?}");
    assert!(
        fmt[0].revision_id != 0,
        "an authored change is never the legacy sentinel"
    );
    fmt[0].revision_id
}

#[test]
fn an_authored_ppr_change_is_enumerable_with_its_author() {
    let doc = doc_with_ppr_change(41);
    let records = enumerate_revisions(&doc.snapshot().canonical);
    let fmt: Vec<_> = records.iter().filter(|r| r.kind.is_format()).collect();
    assert_eq!(fmt.len(), 1, "exactly one formatting revision: {records:?}");
    assert_eq!(fmt[0].author.as_deref(), Some("fmt-test"));
}

/// The wire `w:id` the serializer wrote on the (single) `<w:pPrChange>` — the
/// on-disk OOXML id, distinct from the engine identity under H7.
fn serialized_pprchange_wire_id(xml: &str) -> Option<&str> {
    let after = xml.split(r#"<w:pPrChange w:id=""#).nth(1)?;
    after.split('"').next()
}

#[test]
fn the_serialized_ppr_change_id_round_trips() {
    let doc = doc_with_ppr_change(41);
    // H7 splits two ids that used to coincide: the caller-facing minted
    // IDENTITY (what enumerate/Selective address) and the on-disk wire `w:id`
    // (what the serializer emits; Word does not keep it unique). This test
    // pins each against the right thing — never forcing them equal.
    let id = ppr_change_id(&doc);
    let bytes = doc
        .serialize(&stemma::ExportOptions {
            mode: stemma::ExportMode::Redline,
            validator_level: stemma::ValidatorLevel::Blocking,
            validator: None,
        })
        .expect("serialize");
    let xml = String::from_utf8(
        stemma::docx::DocxArchive::read(&bytes)
            .expect("zip")
            .get("word/document.xml")
            .expect("document.xml")
            .to_vec(),
    )
    .expect("utf8");
    // (a) The serializer emits the change with a wire `w:id`.
    let wire_id = serialized_pprchange_wire_id(&xml)
        .unwrap_or_else(|| panic!("a pPrChange w:id is serialized: {xml}"))
        .to_string();

    let reparsed = Document::parse(&bytes).expect("re-parse");
    // (b) The IDENTITY handle survives the round-trip: the reparsed change
    // still enumerates under the same minted id a caller would resolve by.
    let records = enumerate_revisions(&reparsed.snapshot().canonical);
    assert!(
        records
            .iter()
            .any(|r| r.kind.is_format() && r.revision_id == id),
        "the identity handle survives the round-trip: {records:?}"
    );
    // (c) The wire `w:id` emission is stable across saves (deterministic).
    let bytes2 = reparsed
        .serialize(&stemma::ExportOptions {
            mode: stemma::ExportMode::Redline,
            validator_level: stemma::ValidatorLevel::Blocking,
            validator: None,
        })
        .expect("re-serialize");
    let xml2 = String::from_utf8(
        stemma::docx::DocxArchive::read(&bytes2)
            .expect("zip")
            .get("word/document.xml")
            .expect("document.xml")
            .to_vec(),
    )
    .expect("utf8");
    assert_eq!(
        serialized_pprchange_wire_id(&xml2),
        Some(wire_id.as_str()),
        "the serialized wire id is stable across saves: {xml2}"
    );
}

#[test]
fn accepting_a_ppr_change_by_id_keeps_formatting_and_discards_the_record() {
    let doc = doc_with_ppr_change(41);
    let id = ppr_change_id(&doc);
    let resolved = doc
        .project(Resolution::Selective {
            ids: HashSet::from([id]),
            action: ResolveSelectionAction::Accept,
        })
        .expect("selective accept");
    let p = first_para(&resolved);
    assert_eq!(p.align, Some(Alignment::Center), "the new formatting stays");
    assert!(p.formatting_change.is_none(), "the change record is gone");
}

#[test]
fn rejecting_a_ppr_change_by_id_restores_the_previous_state() {
    let doc = doc_with_ppr_change(41);
    let id = ppr_change_id(&doc);
    let prev_alignment = first_para(&doc)
        .formatting_change
        .as_ref()
        .expect("pending pPrChange")
        .previous_alignment
        .clone();
    let resolved = doc
        .project(Resolution::Selective {
            ids: HashSet::from([id]),
            action: ResolveSelectionAction::Reject,
        })
        .expect("selective reject");
    let p = first_para(&resolved);
    // The contract is restore-what-the-record-snapshotted: the verb records
    // the EFFECTIVE previous alignment (explicit Left for a default-aligned
    // paragraph), so reject restores exactly that.
    assert_eq!(
        p.align, prev_alignment,
        "alignment restored to the recorded previous state"
    );
    assert!(p.formatting_change.is_none(), "the change record is gone");
}

#[test]
fn an_unselected_ppr_change_stays_pending() {
    // Resolve a DIFFERENT id (an inline edit) — the formatting change must
    // survive untouched, id intact.
    let doc = doc_with_ppr_change(41);
    let view = doc.read();
    let other = view
        .blocks
        .iter()
        .find(|b| b.text.contains("Left alone"))
        .expect("other paragraph");
    let doc = doc
        .apply(&EditTransaction {
            steps: vec![EditStep::ReplaceParagraphText {
                block_id: other.id.clone(),
                rationale: None,
                replacement_role: None,
                expect: "Left alone.".to_string(),
                semantic_hash: Some(other.guard.clone()),
                content: stemma::edit::ParagraphContent {
                    fragments: vec![stemma::edit::ContentFragment::Text(
                        "Left mostly alone.".to_string(),
                    )],
                },
            }],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: RevisionInfo {
                revision_id: 90,
                identity: 0,
                author: Some("fmt-test".to_string()),
                date: Some("2026-06-12T00:00:00Z".to_string()),
                apply_op_id: None,
            },
        })
        .expect("second edit applies");
    let fmt_id = ppr_change_id(&doc);
    let other_id = enumerate_revisions(&doc.snapshot().canonical)
        .into_iter()
        .find(|r| !r.kind.is_format())
        .expect("the text edit is enumerable")
        .revision_id;
    assert_ne!(fmt_id, other_id);
    let resolved = doc
        .project(Resolution::Selective {
            ids: HashSet::from([other_id]),
            action: ResolveSelectionAction::Accept,
        })
        .expect("resolve only the text edit");
    let p = first_para(&resolved);
    let fc = p
        .formatting_change
        .as_ref()
        .expect("pPrChange still pending");
    // H7: the caller-facing handle is the minted identity (what `fmt_id` /
    // enumerate report), not the wire `revision_id`.
    assert_eq!(fc.identity, fmt_id, "untouched, id intact");
}

// ═══════════════════════════════════════════════════════════════════════
// PERSISTED-MARKUP CHECK for pPrChange — the existing tests above only
// check `.project()`'s in-memory result; this closes the "after save" half
// of the contract for the carrier that was already correct in memory.
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn accepting_a_ppr_change_by_id_persists_after_save() {
    let doc = doc_with_ppr_change(41);
    let id = ppr_change_id(&doc);
    let reparsed = resolve_and_persist(&doc, id, ResolveSelectionAction::Accept);
    let p = first_para(&reparsed);
    assert_eq!(
        p.align,
        Some(Alignment::Center),
        "the new alignment survives a save + re-parse"
    );
    assert!(
        p.formatting_change.is_none(),
        "no dangling pPrChange after a save + re-parse"
    );
}

#[test]
fn rejecting_a_ppr_change_by_id_persists_after_save() {
    let doc = doc_with_ppr_change(41);
    let id = ppr_change_id(&doc);
    let prev_alignment = first_para(&doc)
        .formatting_change
        .as_ref()
        .expect("pending pPrChange")
        .previous_alignment
        .clone();
    let reparsed = resolve_and_persist(&doc, id, ResolveSelectionAction::Reject);
    let p = first_para(&reparsed);
    assert_eq!(
        p.align, prev_alignment,
        "the reverted alignment survives a save + re-parse"
    );
    assert!(
        p.formatting_change.is_none(),
        "no dangling pPrChange after a save + re-parse"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// RUN (w:rPrChange, §17.13.5.31) — THE BUG this file's class audit found.
// `SetRunFormatting`'s dispatch arm never stamps a fresh revision id, so
// every run-level formatting change is born as the `0` sentinel: invisible
// to selective by-id resolution (accept and reject both silently no-op).
// These tests FAIL against unfixed code — that failure IS the refutation
// evidence; they must PASS once edit/mod.rs's SetRunFormatting arm stamps
// like its SetCellFormatting/SetRowFormatting/SetTableFormatting siblings.
// ═══════════════════════════════════════════════════════════════════════

/// A doc whose first paragraph carries a tracked bold change on the word
/// "Centered" (was unformatted).
fn doc_with_run_bold_change(id: u32) -> Document {
    let doc = Document::parse(&make_docx_with_body(TWO_PARAS)).expect("parse");
    let view = doc.read();
    let target = view
        .blocks
        .iter()
        .find(|b| b.text.contains("Centered later"))
        .expect("target paragraph");
    doc.apply(&EditTransaction {
        steps: vec![EditStep::SetRunFormatting {
            block_id: target.id.clone(),
            expect: "Centered".to_string(),
            semantic_hash: Some(target.guard.clone()),
            marks: InlineMarkSet {
                bold: true,
                ..Default::default()
            },
            style: RunStyleEdit::default(),
            rationale: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: id,
            identity: 0,
            author: Some("fmt-test".to_string()),
            date: Some("2026-06-12T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    })
    .expect("tracked SetRunFormatting applies")
}

/// The TextNode carrying the given exact text, anywhere in the document.
fn run_with_text<'a>(canon: &'a CanonDoc, target: &str) -> &'a TextNode {
    for tb in &canon.blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::Text(t) = inline
                        && t.text == target
                    {
                        return t;
                    }
                }
            }
        }
    }
    panic!("no run with text {target:?}");
}

/// The engine-assigned id of the document's single run-formatting revision.
fn run_bold_change_id(doc: &Document) -> u32 {
    let records = enumerate_revisions(&doc.snapshot().canonical);
    let fmt: Vec<_> = records.iter().filter(|r| r.kind.is_format()).collect();
    assert_eq!(fmt.len(), 1, "exactly one formatting revision: {records:?}");
    assert!(
        fmt[0].revision_id != 0,
        "THE BUG: an authored run-formatting change was assigned the `0` \
         legacy sentinel instead of a real id — SetRunFormatting's dispatch \
         arm never called stamp_revision. A sentinel id can never be \
         selected by accept_changes/reject_changes (both are gated on \
         revision_id != 0 in project_block_for_selected_resolution), so \
         resolving \"this run formatting change\" by id is impossible: \
         got records={records:?}"
    );
    fmt[0].revision_id
}

#[test]
fn an_authored_run_formatting_change_is_enumerable_with_a_real_id() {
    let doc = doc_with_run_bold_change(41);
    // The refutation: this assertion is what fails pre-fix.
    let _id = run_bold_change_id(&doc);
}

/// THE PRECISE refutation for bug A (the missing `stamp_revision` call),
/// isolated from bug B: every other test in this file supplies a non-zero
/// `revision_id` (41) at the TRANSACTION level, which — pre-fix — flowed
/// straight through unstamped and happened to land as a usable id anyway
/// (masking bug A while still exposing bug B). A real caller (the MCP
/// surface) never manually assigns a revision_id; it sends the untagged
/// default, which is the `0` sentinel. This test sends exactly that,
/// mirroring real usage, and asserts the engine allocates its OWN fresh,
/// non-zero id regardless — proving `stamp_revision` actually fires, not
/// just that a caller-supplied real id happens to survive.
#[test]
fn a_run_formatting_change_gets_a_stamped_id_even_when_the_caller_sends_the_zero_sentinel() {
    let doc = Document::parse(&make_docx_with_body(TWO_PARAS)).expect("parse");
    let view = doc.read();
    let target = view
        .blocks
        .iter()
        .find(|b| b.text.contains("Centered later"))
        .expect("target paragraph");
    let doc = doc
        .apply(&EditTransaction {
            steps: vec![EditStep::SetRunFormatting {
                block_id: target.id.clone(),
                expect: "Centered".to_string(),
                semantic_hash: Some(target.guard.clone()),
                marks: InlineMarkSet {
                    bold: true,
                    ..Default::default()
                },
                style: RunStyleEdit::default(),
                rationale: None,
            }],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: RevisionInfo {
                revision_id: 0, // the real-world sentinel, not a hand-picked id
                identity: 0,
                author: Some("fmt-test".to_string()),
                date: Some("2026-06-12T00:00:00Z".to_string()),
                apply_op_id: None,
            },
        })
        .expect("tracked SetRunFormatting applies");
    let id = run_bold_change_id(&doc); // asserts != 0 internally
    assert_ne!(
        id, 0,
        "THE BUG: a transaction sending revision_id=0 (what every real MCP \
         caller sends — ids are server-assigned, not client-picked) must \
         still get a real, engine-stamped id, exactly like every sibling \
         formatting verb (cell/row/table/paragraph)"
    );
}

#[test]
fn accepting_a_run_formatting_change_by_id_keeps_bold_and_drops_the_record() {
    let doc = doc_with_run_bold_change(41);
    let id = run_bold_change_id(&doc);
    let resolved = doc
        .project(Resolution::Selective {
            ids: HashSet::from([id]),
            action: ResolveSelectionAction::Accept,
        })
        .expect("selective accept");
    let t = run_with_text(&resolved.snapshot().canonical, "Centered");
    assert!(t.marks.contains(&Mark::Bold), "the new bold mark stays");
    assert!(t.formatting_change.is_none(), "the change record is gone");
}

#[test]
fn accepting_a_run_formatting_change_by_id_persists_after_save() {
    let doc = doc_with_run_bold_change(41);
    let id = run_bold_change_id(&doc);
    let reparsed = resolve_and_persist(&doc, id, ResolveSelectionAction::Accept);
    let t = run_with_text(&reparsed.snapshot().canonical, "Centered");
    assert!(
        t.marks.contains(&Mark::Bold),
        "bold survives a save + re-parse"
    );
    assert!(
        t.formatting_change.is_none(),
        "no dangling rPrChange after a save + re-parse"
    );
}

#[test]
fn rejecting_a_run_formatting_change_by_id_restores_unformatted_and_drops_the_record() {
    let doc = doc_with_run_bold_change(41);
    let id = run_bold_change_id(&doc);
    let resolved = doc
        .project(Resolution::Selective {
            ids: HashSet::from([id]),
            action: ResolveSelectionAction::Reject,
        })
        .expect("selective reject");
    let t = run_with_text(&resolved.snapshot().canonical, "Centered");
    assert!(
        !t.marks.contains(&Mark::Bold),
        "reject must revert the mark — THE BUG left it bold"
    );
    assert!(t.formatting_change.is_none(), "the change record is gone");
}

#[test]
fn rejecting_a_run_formatting_change_by_id_persists_after_save() {
    let doc = doc_with_run_bold_change(41);
    let id = run_bold_change_id(&doc);
    let reparsed = resolve_and_persist(&doc, id, ResolveSelectionAction::Reject);
    let t = run_with_text(&reparsed.snapshot().canonical, "Centered");
    assert!(
        !t.marks.contains(&Mark::Bold),
        "reject-then-save must persist the reverted (unformatted) run — \
         THE BUG kept the new bold formatting in the saved file"
    );
    assert!(
        t.formatting_change.is_none(),
        "no dangling rPrChange after a save + re-parse"
    );
}

#[test]
fn an_unselected_run_formatting_change_stays_pending() {
    let doc = doc_with_run_bold_change(41);
    let view = doc.read();
    let other = view
        .blocks
        .iter()
        .find(|b| b.text.contains("Left alone"))
        .expect("other paragraph");
    let doc = doc
        .apply(&EditTransaction {
            steps: vec![EditStep::ReplaceParagraphText {
                block_id: other.id.clone(),
                rationale: None,
                replacement_role: None,
                expect: "Left alone.".to_string(),
                semantic_hash: Some(other.guard.clone()),
                content: stemma::edit::ParagraphContent {
                    fragments: vec![stemma::edit::ContentFragment::Text(
                        "Left mostly alone.".to_string(),
                    )],
                },
            }],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: RevisionInfo {
                revision_id: 90,
                identity: 0,
                author: Some("fmt-test".to_string()),
                date: Some("2026-06-12T00:00:00Z".to_string()),
                apply_op_id: None,
            },
        })
        .expect("second edit applies");
    let fmt_id = run_bold_change_id(&doc);
    let other_id = enumerate_revisions(&doc.snapshot().canonical)
        .into_iter()
        .find(|r| !r.kind.is_format())
        .expect("the text edit is enumerable")
        .revision_id;
    assert_ne!(fmt_id, other_id);
    let resolved = doc
        .project(Resolution::Selective {
            ids: HashSet::from([other_id]),
            action: ResolveSelectionAction::Accept,
        })
        .expect("resolve only the text edit");
    let t = run_with_text(&resolved.snapshot().canonical, "Centered");
    let fc = t
        .formatting_change
        .as_ref()
        .expect("rPrChange still pending");
    // H7: the caller-facing handle is the minted identity (what `fmt_id` /
    // enumerate report), not the wire `revision_id`.
    assert_eq!(fc.identity, fmt_id, "untouched, id intact");
}

// ═══════════════════════════════════════════════════════════════════════
// TABLE / ROW / CELL (w:tblPrChange §17.13.5.34, w:trPrChange §17.13.5.36,
// w:tcPrChange §17.13.5.37) — dispatch arms already stamp correctly; PINNED
// here with the same by-id + persisted-save rigor as the run/paragraph
// carriers, not just assumed from sibling code looking correct.
// ═══════════════════════════════════════════════════════════════════════

fn make_table_docx() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:tbl><w:tblPr><w:tblW w:w="5000" w:type="pct"/></w:tblPr><w:tblGrid><w:gridCol w:w="2400"/><w:gridCol w:w="2400"/></w:tblGrid><w:tr><w:tc><w:tcPr/><w:p><w:r><w:t xml:space="preserve">A</w:t></w:r></w:p></w:tc><w:tc><w:tcPr/><w:p><w:r><w:t xml:space="preserve">B</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/><w:sectPr/></w:body></w:document>"#;
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

fn first_table(canon: &CanonDoc) -> &TableNode {
    for tb in &canon.blocks {
        if let BlockNode::Table(t) = &tb.block {
            return t;
        }
    }
    panic!("no table block");
}

fn table_id(canon: &CanonDoc) -> NodeId {
    first_table(canon).id.clone()
}

fn fmt_change_id(doc: &Document) -> u32 {
    let records = enumerate_revisions(&doc.snapshot().canonical);
    let fmt: Vec<_> = records.iter().filter(|r| r.kind.is_format()).collect();
    assert_eq!(fmt.len(), 1, "exactly one formatting revision: {records:?}");
    assert!(
        fmt[0].revision_id != 0,
        "an authored change is never the legacy sentinel: {records:?}"
    );
    fmt[0].revision_id
}

/// The engine-assigned id of the `w:tblPrChange` specifically. Unlike the run/
/// row/cell carriers, a `SetTableFormatting` edit enumerates as TWO revisions:
/// the `tblPrChange` PLUS a companion no-op `trPrChange` the verb stamps on the
/// first row, because Word never registers a lone `tblPrChange` (see
/// `table_formatting.rs`'s WORD RULE). Select the table carrier by kind — the
/// companion is a no-op (previous == current row formatting) so accepting or
/// rejecting the table id alone still leaves the row untouched.
fn table_fmt_change_id(doc: &Document) -> u32 {
    let records = enumerate_revisions(&doc.snapshot().canonical);
    let tbl: Vec<_> = records
        .iter()
        .filter(|r| r.kind == RevisionKind::FormatTable)
        .collect();
    assert_eq!(tbl.len(), 1, "exactly one tblPrChange: {records:?}");
    assert!(
        records.iter().any(|r| r.kind == RevisionKind::FormatRow),
        "SetTableFormatting stamps a companion trPrChange so Word registers \
         the table change: {records:?}"
    );
    assert!(
        tbl[0].revision_id != 0,
        "an authored change is never the legacy sentinel: {records:?}"
    );
    tbl[0].revision_id
}

fn doc_with_table_border_change(id: u32) -> Document {
    let doc = Document::parse(&make_table_docx()).expect("parse");
    let tid = table_id(&doc.snapshot().canonical);
    doc.apply(&EditTransaction {
        steps: vec![EditStep::SetTableFormatting {
            block_id: tid,
            semantic_hash: None,
            patch: TableFormattingPatch {
                borders: Some(BorderSet {
                    top: Some(Border {
                        style: stemma::domain::BorderStyle::Single,
                        size: Some(8),
                        space: Some(0),
                        color: Some("FF0000".to_string()),
                        extra_attrs: Vec::new(),
                    }),
                    ..Default::default()
                }),
                width: None,
                default_cell_margins: None,
            },
            rationale: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: id,
            identity: 0,
            author: Some("fmt-test".to_string()),
            date: Some("2026-06-12T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    })
    .expect("tracked SetTableFormatting applies")
}

#[test]
fn accepting_a_table_formatting_change_by_id_persists_after_save() {
    let doc = doc_with_table_border_change(41);
    let id = table_fmt_change_id(&doc);
    let reparsed = resolve_and_persist(&doc, id, ResolveSelectionAction::Accept);
    let t = first_table(&reparsed.snapshot().canonical);
    let top = t.formatting.borders.as_ref().and_then(|b| b.top.as_ref());
    assert_eq!(
        top.and_then(|b| b.color.as_deref()),
        Some("FF0000"),
        "the new border color survives a save + re-parse"
    );
    assert!(
        t.formatting_change.is_none(),
        "no dangling tblPrChange after a save + re-parse"
    );
}

#[test]
fn rejecting_a_table_formatting_change_by_id_persists_after_save() {
    let doc = doc_with_table_border_change(41);
    let id = table_fmt_change_id(&doc);
    let prev_borders = first_table(&doc.snapshot().canonical)
        .formatting_change
        .as_ref()
        .expect("pending tblPrChange")
        .previous_borders
        .clone();
    let reparsed = resolve_and_persist(&doc, id, ResolveSelectionAction::Reject);
    let t = first_table(&reparsed.snapshot().canonical);
    assert_eq!(
        t.formatting.borders, prev_borders,
        "the reverted borders survive a save + re-parse"
    );
    assert!(
        t.formatting_change.is_none(),
        "no dangling tblPrChange after a save + re-parse"
    );
}

fn doc_with_row_height_change(id: u32) -> Document {
    let doc = Document::parse(&make_table_docx()).expect("parse");
    let tid = table_id(&doc.snapshot().canonical);
    doc.apply(&EditTransaction {
        steps: vec![EditStep::SetRowFormatting {
            block_id: tid,
            row_index: 0,
            semantic_hash: None,
            patch: RowFormattingPatch {
                height: Some(720),
                height_rule: Some(HeightRule::AtLeast),
            },
            rationale: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: id,
            identity: 0,
            author: Some("fmt-test".to_string()),
            date: Some("2026-06-12T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    })
    .expect("tracked SetRowFormatting applies")
}

#[test]
fn accepting_a_row_formatting_change_by_id_persists_after_save() {
    let doc = doc_with_row_height_change(41);
    let id = fmt_change_id(&doc);
    let reparsed = resolve_and_persist(&doc, id, ResolveSelectionAction::Accept);
    let row = &first_table(&reparsed.snapshot().canonical).rows[0];
    assert_eq!(
        row.height,
        Some(720),
        "the new row height survives a save + re-parse"
    );
    assert!(
        row.formatting_change.is_none(),
        "no dangling trPrChange after a save + re-parse"
    );
}

#[test]
fn rejecting_a_row_formatting_change_by_id_persists_after_save() {
    let doc = doc_with_row_height_change(41);
    let id = fmt_change_id(&doc);
    let prev_height = first_table(&doc.snapshot().canonical).rows[0]
        .formatting_change
        .as_ref()
        .expect("pending trPrChange")
        .previous_height;
    let reparsed = resolve_and_persist(&doc, id, ResolveSelectionAction::Reject);
    let row = &first_table(&reparsed.snapshot().canonical).rows[0];
    assert_eq!(
        row.height, prev_height,
        "the reverted row height survives a save + re-parse"
    );
    assert!(
        row.formatting_change.is_none(),
        "no dangling trPrChange after a save + re-parse"
    );
}

fn doc_with_cell_shading_change(id: u32) -> Document {
    let doc = Document::parse(&make_table_docx()).expect("parse");
    let tid = table_id(&doc.snapshot().canonical);
    doc.apply(&EditTransaction {
        steps: vec![EditStep::SetCellFormatting {
            block_id: tid,
            row_index: 0,
            col_index: 0,
            semantic_hash: None,
            patch: CellFormattingPatch {
                shading: Some(Shading {
                    fill: Some("1F4E78".to_string()),
                    val: None,
                    color: None,
                    extra_attrs: Vec::new(),
                }),
                borders: None,
                width: None,
                v_align: None,
                margins: None,
            },
            rationale: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: id,
            identity: 0,
            author: Some("fmt-test".to_string()),
            date: Some("2026-06-12T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    })
    .expect("tracked SetCellFormatting applies")
}

#[test]
fn accepting_a_cell_formatting_change_by_id_persists_after_save() {
    let doc = doc_with_cell_shading_change(41);
    let id = fmt_change_id(&doc);
    let reparsed = resolve_and_persist(&doc, id, ResolveSelectionAction::Accept);
    let cell = &first_table(&reparsed.snapshot().canonical).rows[0].cells[0];
    assert_eq!(
        cell.formatting
            .shading
            .as_ref()
            .and_then(|s| s.fill.as_deref()),
        Some("1F4E78"),
        "the new shading survives a save + re-parse"
    );
    assert!(
        cell.formatting_change.is_none(),
        "no dangling tcPrChange after a save + re-parse"
    );
}

#[test]
fn rejecting_a_cell_formatting_change_by_id_persists_after_save() {
    let doc = doc_with_cell_shading_change(41);
    let id = fmt_change_id(&doc);
    let prev_shading = first_table(&doc.snapshot().canonical).rows[0].cells[0]
        .formatting_change
        .as_ref()
        .expect("pending tcPrChange")
        .previous_shading
        .clone();
    let reparsed = resolve_and_persist(&doc, id, ResolveSelectionAction::Reject);
    let cell = &first_table(&reparsed.snapshot().canonical).rows[0].cells[0];
    assert_eq!(
        cell.formatting.shading, prev_shading,
        "the reverted shading survives a save + re-parse"
    );
    assert!(
        cell.formatting_change.is_none(),
        "no dangling tcPrChange after a save + re-parse"
    );
}
