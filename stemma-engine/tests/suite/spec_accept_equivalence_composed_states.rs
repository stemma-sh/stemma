//! Wire/model accept (and reject) EQUIVALENCE over COMPOSED session states.
//!
//! The engine resolves tracked changes two ways, and their agreement is a
//! documented invariant (`normalize.rs`, the row-level accept comment: the wire
//! path "keeps this path in agreement with the IR accept path"):
//!
//!   - MODEL path: `Document::project(Resolution::AcceptAll)` — accept-all on
//!     the parsed IR (`tracked_model::accept_all`).
//!   - WIRE path: `normalize::normalize_docx` over the SERIALIZED bytes of the
//!     same document.
//!
//! THE INVARIANT: for any document `d`,
//!
//!     text(normalize_docx(serialize(d)))  ==  text(project(AcceptAll)(d).serialize())
//!
//! and the reject twin (`reject_all_docx` vs `project(RejectAll)`). The two
//! paths already agree on freshly-imported documents (see
//! `cross_path_projection_equivalence`). This file pins the harder case: a
//! COMPOSED session state — a document that has been edited with a structural
//! verb and then had a *subset* of the resulting composed multi-part tracked
//! change SELECTIVELY resolved mid-session. A subset resolution can leave the
//! IR in a state whose two accept paths disagree if the composed deletion was
//! allowed to fragment into an internally inconsistent shape (a whole-row
//! deletion whose row marker, cell contents, and cell paragraph marks were
//! separately resolvable — resolving some but not others stranded a marker with
//! no faithful single wire encoding, so `serialize`→`normalize_docx` and
//! `project(AcceptAll)` drifted).
//!
//! WHY THE LOOP: which way a fragmented state drifts on the wire depends on
//! serialization iteration order (the serializer walks `HashMap`s seeded by
//! `RandomState`), so a single serialize can pass by luck. Each iteration below
//! re-serializes from scratch (fresh maps, fresh order); several cycles per
//! state sample enough orders to surface an order-dependent divergence reliably
//! rather than flakily.
//!
//! THE COMPARISON: an order-invariant multiset of whitespace-collapsed body
//! paragraph/cell texts (reimport → `to_text` → per-line normalize → sort). The
//! F7 class is "one path retains content the other removes" — a SET difference
//! that a multiset comparison captures regardless of paragraph ordering, which
//! is itself order-stable but need not be relied on.

use std::collections::HashSet;

use stemma::ExportOptions;
use stemma::api::Document;
use stemma::docx::DocxArchive;
use stemma::domain::{BlockNode, NodeId, RevisionInfo};
use stemma::edit::{
    BlockSpec, ContentFragment, EditStep, EditTransaction, InsertPosition, MaterializationMode,
    ParagraphBlockSpec, ParagraphContent, TableOp,
};
use stemma::normalize::{normalize_docx, reject_all_docx};
use stemma::tracked_model::{ResolveSelectionAction, enumerate_revisions};

const AUTHOR: &str = "Equivalence Reviewer";

/// How many fresh serialize→compare cycles per state (see WHY THE LOOP).
const SERIALIZE_CYCLES: usize = 6;

// ── minimal in-memory DOCX (shared spec-suite shape) ─────────────────────────

fn make_docx(body_xml: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_xml}</w:body></w:document>"#
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

fn para(text: &str) -> String {
    format!(r#"<w:p><w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p>"#)
}

// ── the comparison channel ───────────────────────────────────────────────────

/// Order-invariant multiset of body texts: reimport the resolved bytes, take
/// the plain-text reading (tracked changes are all resolved to zero at this
/// point, so this is the accepted/rejected body), collapse whitespace per line,
/// drop empties, and sort so paragraph ordering cannot mask a content set
/// difference.
fn text_multiset(bytes: &[u8], label: &str) -> Vec<String> {
    let doc = Document::parse(bytes).unwrap_or_else(|e| panic!("{label}: reparse resolved: {e:?}"));
    let mut v: Vec<String> = doc
        .to_text()
        .split('\n')
        .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|s| !s.is_empty())
        .collect();
    v.sort();
    v
}

/// Serialize a composed state to bytes. Uses the UNCHECKED export so this test
/// isolates the accept/reject-equivalence invariant from the save gate (a
/// separate concern with its own tests) — but note the serializer's own
/// structural backstops (e.g. the cell-less-`<w:tr>` refusal) still fire here,
/// so a fragmentation that produced invalid structure fails loud rather than
/// slipping through.
fn serialize(d: &Document, label: &str) -> Vec<u8> {
    d.serialize(&ExportOptions::unchecked())
        .unwrap_or_else(|e| panic!("{label}: serialize composed state: {e:?}"))
}

/// Assert the wire and model paths agree for BOTH accept and reject on a single
/// composed state, looped over fresh serializations.
fn assert_paths_agree(d: &Document, label: &str) {
    for cycle in 0..SERIALIZE_CYCLES {
        // ACCEPT: normalize_docx(serialize(d)) == project(AcceptAll)(d).serialize()
        let model_accept = d
            .project(stemma::Resolution::AcceptAll)
            .unwrap_or_else(|e| panic!("{label}: project(AcceptAll): {e:?}"));
        let model_bytes = serialize(&model_accept, &format!("{label}/accept/model"));

        let d_bytes = serialize(d, &format!("{label}/accept/wire-input/cycle{cycle}"));
        let archive = DocxArchive::read(&d_bytes)
            .unwrap_or_else(|e| panic!("{label}: DocxArchive::read: {e:?}"));
        let (accepted, _) =
            normalize_docx(&archive).unwrap_or_else(|e| panic!("{label}: normalize_docx: {e:?}"));
        let wire_bytes = accepted
            .write()
            .unwrap_or_else(|e| panic!("{label}: write accepted: {e:?}"));

        let model_txt = text_multiset(&model_bytes, &format!("{label}/accept/model"));
        let wire_txt = text_multiset(&wire_bytes, &format!("{label}/accept/wire"));
        assert_eq!(
            model_txt, wire_txt,
            "[{label}] ACCEPT cross-path divergence (cycle {cycle}):\n  \
             model project(AcceptAll): {model_txt:?}\n  \
             wire  normalize_docx:     {wire_txt:?}"
        );

        // REJECT twin: reject_all_docx(serialize(d)) == project(RejectAll)(d).serialize()
        let model_reject = d
            .project(stemma::Resolution::RejectAll)
            .unwrap_or_else(|e| panic!("{label}: project(RejectAll): {e:?}"));
        let model_rej_bytes = serialize(&model_reject, &format!("{label}/reject/model"));

        let d_bytes_r = serialize(d, &format!("{label}/reject/wire-input/cycle{cycle}"));
        let archive_r = DocxArchive::read(&d_bytes_r)
            .unwrap_or_else(|e| panic!("{label}: DocxArchive::read (reject): {e:?}"));
        let (rejected, _) = reject_all_docx(&archive_r)
            .unwrap_or_else(|e| panic!("{label}: reject_all_docx: {e:?}"));
        let wire_rej_bytes = rejected
            .write()
            .unwrap_or_else(|e| panic!("{label}: write rejected: {e:?}"));

        let model_rej_txt = text_multiset(&model_rej_bytes, &format!("{label}/reject/model"));
        let wire_rej_txt = text_multiset(&wire_rej_bytes, &format!("{label}/reject/wire"));
        assert_eq!(
            model_rej_txt, wire_rej_txt,
            "[{label}] REJECT cross-path divergence (cycle {cycle}):\n  \
             model project(RejectAll): {model_rej_txt:?}\n  \
             wire  reject_all_docx:    {wire_rej_txt:?}"
        );
    }
}

// ── edit plumbing ────────────────────────────────────────────────────────────

fn txn(step: EditStep) -> EditTransaction {
    EditTransaction {
        steps: vec![step],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some(AUTHOR.to_string()),
            date: Some("2026-06-01T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
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

fn table_block_id(doc: &Document) -> NodeId {
    doc.snapshot()
        .canonical
        .blocks
        .iter()
        .find_map(|tb| match &tb.block {
            BlockNode::Table(t) => Some(t.id.clone()),
            _ => None,
        })
        .expect("a table block")
}

/// Every revision id this test's author minted, in enumeration order.
fn mutation_ids(doc: &Document) -> Vec<u32> {
    enumerate_revisions(&doc.snapshot().canonical)
        .into_iter()
        .filter(|r| r.author.as_deref() == Some(AUTHOR))
        .map(|r| r.revision_id)
        .collect()
}

/// The battery of subsets to selectively resolve on the way to a composed
/// state: every single id, every all-but-one complement, and the full set — the
/// shapes most likely to strand a constituent of a composed deletion.
fn subsets(ids: &[u32]) -> Vec<HashSet<u32>> {
    let mut out: Vec<HashSet<u32>> = Vec::new();
    for &id in ids {
        out.push(HashSet::from([id]));
    }
    for &skip in ids {
        let comp: HashSet<u32> = ids.iter().copied().filter(|&i| i != skip).collect();
        if comp.len() >= 2 {
            out.push(comp);
        }
    }
    out.push(ids.iter().copied().collect());
    out
}

/// For a base document carrying one composed tracked change, build every
/// composed session state (selectively resolve each subset with each action)
/// and assert both accept paths and both reject paths agree on each.
fn assert_every_composed_state_agrees(base: &Document, tag: &str) {
    let ids = mutation_ids(base);
    assert!(!ids.is_empty(), "{tag}: base minted no revisions");
    for subset in subsets(&ids) {
        for (an, action) in [
            ("accept", ResolveSelectionAction::Accept),
            ("reject", ResolveSelectionAction::Reject),
        ] {
            let composed = base
                .project(stemma::Resolution::Selective {
                    ids: subset.clone(),
                    action,
                })
                .unwrap_or_else(|e| panic!("{tag}: selective {an} {subset:?}: {e:?}"));
            let mut ids_sorted: Vec<u32> = subset.iter().copied().collect();
            ids_sorted.sort_unstable();
            assert_paths_agree(&composed, &format!("{tag}/sel-{an}-{ids_sorted:?}"));
        }
    }
    // The un-resolved base itself (no mid-session resolution) must also agree.
    assert_paths_agree(base, &format!("{tag}/base"));
}

// ── fixtures ─────────────────────────────────────────────────────────────────

fn one_col_two_row_table() -> String {
    r#"<w:tbl>
        <w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr>
        <w:tblGrid><w:gridCol w:w="9576"/></w:tblGrid>
        <w:tr><w:tc><w:tcPr><w:tcW w:w="9576" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>R0C0</w:t></w:r></w:p></w:tc></w:tr>
        <w:tr><w:tc><w:tcPr><w:tcW w:w="9576" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>R1C0</w:t></w:r></w:p></w:tc></w:tr>
    </w:tbl><w:p/><w:sectPr/>"#.to_string()
}

fn two_col_two_row_table() -> String {
    r#"<w:tbl>
        <w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr>
        <w:tblGrid><w:gridCol w:w="4788"/><w:gridCol w:w="4788"/></w:tblGrid>
        <w:tr>
            <w:tc><w:tcPr><w:tcW w:w="4788" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>R0C0</w:t></w:r></w:p></w:tc>
            <w:tc><w:tcPr><w:tcW w:w="4788" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>R0C1</w:t></w:r></w:p></w:tc>
        </w:tr>
        <w:tr>
            <w:tc><w:tcPr><w:tcW w:w="4788" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>R1C0</w:t></w:r></w:p></w:tc>
            <w:tc><w:tcPr><w:tcW w:w="4788" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>R1C1</w:t></w:r></w:p></w:tc>
        </w:tr>
    </w:tbl><w:p/><w:sectPr/>"#.to_string()
}

fn multi_para_cell_table() -> String {
    r#"<w:tbl>
        <w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr>
        <w:tblGrid><w:gridCol w:w="9576"/></w:tblGrid>
        <w:tr><w:tc><w:tcPr><w:tcW w:w="9576" w:type="dxa"/></w:tcPr>
            <w:p><w:r><w:t>R0P0</w:t></w:r></w:p><w:p><w:r><w:t>R0P1</w:t></w:r></w:p></w:tc></w:tr>
        <w:tr><w:tc><w:tcPr><w:tcW w:w="9576" w:type="dxa"/></w:tcPr>
            <w:p><w:r><w:t>R1P0</w:t></w:r></w:p></w:tc></w:tr>
    </w:tbl><w:p/><w:sectPr/>"#
        .to_string()
}

fn nested_table_doc() -> String {
    r#"<w:tbl>
        <w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr>
        <w:tblGrid><w:gridCol w:w="9576"/></w:tblGrid>
        <w:tr><w:tc><w:tcPr><w:tcW w:w="9576" w:type="dxa"/></w:tcPr>
            <w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid>
                <w:tr><w:tc><w:tcPr><w:tcW w:w="4000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>NestedA</w:t></w:r></w:p></w:tc></w:tr>
            </w:tbl><w:p><w:r><w:t>Outer0</w:t></w:r></w:p></w:tc></w:tr>
        <w:tr><w:tc><w:tcPr><w:tcW w:w="9576" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>Outer1</w:t></w:r></w:p></w:tc></w:tr>
    </w:tbl><w:p/><w:sectPr/>"#.to_string()
}

fn deleted_row(table_body: &str, row_index: usize) -> Document {
    let base = Document::parse(&make_docx(table_body)).expect("parse");
    let block_id = table_block_id(&base);
    base.apply(&txn(EditStep::TableStructureOp {
        block_id,
        semantic_hash: None,
        op: TableOp::DeleteRow { row_index },
        rationale: None,
    }))
    .expect("apply delete_row")
}

// ── the tests: one per composed-change family for attributable failure ───────

#[test]
fn delete_block_range_composed_states_agree() {
    let body = format!(
        "{}{}{}{}",
        para("Alpha one"),
        para("Beta two"),
        para("Gamma three"),
        para("Delta four")
    );
    let base = Document::parse(&make_docx(&format!("{body}<w:sectPr/>"))).expect("parse");
    let pids = para_ids(&base);
    let d = base
        .apply(&txn(EditStep::DeleteBlockRange {
            from_block_id: pids[1].clone(),
            to_block_id: pids[2].clone(),
            rationale: None,
            expect: String::new(),
            semantic_hash: None,
        }))
        .expect("apply delete range");
    assert_every_composed_state_agrees(&d, "delete_block_range");
}

#[test]
fn replace_block_range_composed_states_agree() {
    let body = format!(
        "{}{}{}",
        para("Alpha one"),
        para("Beta two"),
        para("Gamma three")
    );
    let base = Document::parse(&make_docx(&format!("{body}<w:sectPr/>"))).expect("parse");
    let pids = para_ids(&base);
    let d = base
        .apply(&txn(EditStep::ReplaceBlockRange {
            from_block_id: pids[0].clone(),
            to_block_id: pids[1].clone(),
            rationale: None,
            expect: String::new(),
            semantic_hash: None,
            blocks: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
                role: Some("default".to_string()),
                content: ParagraphContent {
                    fragments: vec![ContentFragment::Text("Replacement line".to_string())],
                },
                restart_numbering: false,
                list: None,
            })],
        }))
        .expect("apply replace range");
    assert_every_composed_state_agrees(&d, "replace_block_range");
}

#[test]
fn move_block_range_composed_states_agree() {
    let body = format!(
        "{}{}{}{}",
        para("Alpha one"),
        para("Beta two"),
        para("Gamma three"),
        para("Delta four")
    );
    let base = Document::parse(&make_docx(&format!("{body}<w:sectPr/>"))).expect("parse");
    let pids = para_ids(&base);
    let d = base
        .apply(&txn(EditStep::MoveBlockRange {
            from_block_id: pids[1].clone(),
            to_block_id: pids[2].clone(),
            dest_anchor_id: pids[3].clone(),
            dest_position: InsertPosition::After,
            rationale: None,
            expect: None,
            semantic_hash: None,
        }))
        .expect("apply move");
    assert_every_composed_state_agrees(&d, "move_block_range");
}

#[test]
fn row_delete_single_column_composed_states_agree() {
    assert_every_composed_state_agrees(&deleted_row(&one_col_two_row_table(), 0), "row_del_1col");
}

#[test]
fn row_delete_two_column_composed_states_agree() {
    // The headline shape: a two-column row delete whose row marker + two
    // cell content deletions were separately resolvable. Every subset must
    // resolve coherently on both paths.
    assert_every_composed_state_agrees(&deleted_row(&two_col_two_row_table(), 0), "row_del_2col");
}

#[test]
fn row_delete_multi_paragraph_cell_composed_states_agree() {
    // A cell with two paragraphs: the interior paragraph mark is a real
    // (wire-representable) within-cell join, but the cell's FINAL mark must not
    // be tracked-deleted (that stranded marker was the F7 poison).
    assert_every_composed_state_agrees(
        &deleted_row(&multi_para_cell_table(), 0),
        "row_del_multipara",
    );
}

#[test]
fn row_delete_nested_table_composed_states_agree() {
    // A deleted outer row whose cell holds a nested table: the nested rows must
    // delete markerless-cell (row-marker only) just like the top level, or a
    // subset resolution would drop a nested cell out of a surviving nested row
    // (cell-less `<w:tr>`).
    assert_every_composed_state_agrees(&deleted_row(&nested_table_doc(), 0), "row_del_nested");
}

#[test]
fn insert_then_delete_chain_composed_states_agree() {
    // A CHAIN: insert a tracked paragraph, then delete a range that spans an
    // original paragraph — two independent composed changes coexisting. Partial
    // resolution across the two must still leave a wire-faithful state.
    let body = format!(
        "{}{}{}",
        para("Alpha one"),
        para("Beta two"),
        para("Gamma three")
    );
    let base = Document::parse(&make_docx(&format!("{body}<w:sectPr/>"))).expect("parse");
    let pids = para_ids(&base);
    let inserted = base
        .apply(&txn(EditStep::InsertParagraphs {
            anchor_block_id: pids[0].clone(),
            position: InsertPosition::After,
            rationale: None,
            blocks: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
                role: Some("default".to_string()),
                content: ParagraphContent {
                    fragments: vec![ContentFragment::Text("Inserted middle".to_string())],
                },
                restart_numbering: false,
                list: None,
            })],
        }))
        .expect("apply insert");
    let ins_pids = para_ids(&inserted);
    // Delete the last two original paragraphs (now shifted by the insert).
    let d = inserted
        .apply(&txn(EditStep::DeleteBlockRange {
            from_block_id: ins_pids[ins_pids.len() - 2].clone(),
            to_block_id: ins_pids[ins_pids.len() - 1].clone(),
            rationale: None,
            expect: String::new(),
            semantic_hash: None,
        }))
        .expect("apply delete range");
    assert_every_composed_state_agrees(&d, "insert_then_delete_chain");
}
