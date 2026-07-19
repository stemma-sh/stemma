//! Selective resolution of a tracked table ROW INSERT/DELETE must never leave the
//! table in a shape OOXML forbids.
//!
//! Domain rule (ECMA-376 §17.13.5): a WHOLE-ROW insertion or deletion is carried
//! by the row-level marker `w:trPr/w:ins` or `w:trPr/w:del` (§17.13.5.13/.17) —
//! deletions additionally track each cell's content (`w:del`). The cell-level
//! markers `w:cellIns` / `w:cellDel` (§17.13.5.1-2) are for cells changed WITHIN
//! a surviving row (a column op or a cell merge); real Word never emits them on
//! the cells of a row that is itself inserted/deleted (see the `row_del_*`
//! word-compliance fixtures: a deleted row's `<w:tcPr>` carries no `cellDel`).
//!
//! Before the fix the engine minted a redundant per-cell `cellDel`/`cellIns` on
//! top of the row marker. That both diverged from Word AND made an invalid state
//! *representable*: `CT_Row` (§17.4.72) requires `tc+`, but selectively resolving
//! one cell's `cellDel` in isolation physically dropped that cell while the row
//! (its marker unresolved) survived — a `<w:tr>` with zero `<w:tc>`, which the
//! engine's own importer refuses. The fix removes the spurious per-cell markers,
//! so only the row marker removes cells and it removes the whole row atomically;
//! the cell-less-row shape is now unrepresentable. A serializer backstop
//! (`serialize_refuses_cell_less_row`, in `serialize::mod`) fails loud should any
//! other path ever produce one.

use std::collections::HashSet;

use stemma::ExportOptions;
use stemma::api::Document;
use stemma::domain::{NodeId, RevisionInfo};
use stemma::edit::{EditStep, EditTransaction, MaterializationMode, TableOp};
use stemma::tracked_model::{ResolveSelectionAction, RevisionKind, enumerate_revisions};

const MUTATION_AUTHOR: &str = "Corpus Mutation";

// ─── Minimal-docx plumbing (mirrors the shared spec-suite prelude) ───────────

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

/// A normal 2-row, SINGLE-column table (no tracked changes) so a `delete_row`
/// authored on top is the ONLY revision in the document. Single-column is the
/// sharpest case for the row-delete-subset invariant: the row has exactly one
/// cell, so resolving that cell's deletion in isolation would leave a `<w:tr>`
/// with zero `<w:tc>`.
fn two_row_table_docx() -> Vec<u8> {
    let body = r#"<w:tbl>
        <w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr>
        <w:tblGrid><w:gridCol w:w="9576"/></w:tblGrid>
        <w:tr>
            <w:tc><w:tcPr><w:tcW w:w="9576" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>R0C0</w:t></w:r></w:p></w:tc>
        </w:tr>
        <w:tr>
            <w:tc><w:tcPr><w:tcW w:w="9576" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>R1C0</w:t></w:r></w:p></w:tc>
        </w:tr>
    </w:tbl><w:p/><w:sectPr/>"#;
    make_docx(body)
}

/// A normal 2-row, 2-column table — used to prove the subset invariant also
/// covers a MULTI-cell row: accepting BOTH cells' deletions but not the
/// row-structure marker would empty the row of cells just the same.
fn two_row_two_col_table_docx() -> Vec<u8> {
    let body = r#"<w:tbl>
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
    </w:tbl><w:p/><w:sectPr/>"#;
    make_docx(body)
}

/// Find the single top-level table's block id.
fn table_block_id(doc: &Document) -> NodeId {
    use stemma::domain::BlockNode;
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

/// Parse the docx, apply a tracked `delete_row` of `row_index`, return the
/// edited document.
fn deleted_row_doc(row_index: usize) -> Document {
    deleted_row_doc_from(&two_row_table_docx(), row_index)
}

fn deleted_row_doc_from(docx: &[u8], row_index: usize) -> Document {
    let doc = Document::parse(docx).expect("parse base");
    let block_id = table_block_id(&doc);
    let txn = EditTransaction {
        steps: vec![EditStep::TableStructureOp {
            block_id,
            semantic_hash: None,
            op: TableOp::DeleteRow { row_index },
            rationale: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some(MUTATION_AUTHOR.to_string()),
            date: Some("2026-06-01T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    };
    doc.apply(&txn).expect("apply delete_row")
}

/// Ids authored by the mutation author, in enumeration order, tagged with kind
/// and excerpt so the test documents WHAT each constituent is.
fn mutation_revisions(doc: &Document) -> Vec<(u32, RevisionKind, String)> {
    enumerate_revisions(&doc.snapshot().canonical)
        .into_iter()
        .filter(|r| r.author.as_deref() == Some(MUTATION_AUTHOR))
        .map(|r| (r.revision_id, r.kind, r.excerpt))
        .collect()
}

/// Apply a tracked `insert_row` after `ref_row`, single-column fixture.
fn inserted_row_doc() -> Document {
    let doc = Document::parse(&two_row_table_docx()).expect("parse base");
    let block_id = table_block_id(&doc);
    let txn = EditTransaction {
        steps: vec![EditStep::TableStructureOp {
            block_id,
            semantic_hash: None,
            op: TableOp::InsertRow {
                ref_row: 0,
                position: stemma::edit::TableInsertPosition::After,
                cells: None,
            },
            rationale: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some(MUTATION_AUTHOR.to_string()),
            date: Some("2026-06-01T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    };
    doc.apply(&txn).expect("apply insert_row")
}

/// Every single-id subset × {Accept, Reject} must round-trip through the
/// serializer AND the engine's own importer.
fn assert_every_single_id_subset_importable(doc: &Document, label: &str) {
    let ids: Vec<u32> = mutation_revisions(doc)
        .into_iter()
        .map(|(id, _, _)| id)
        .collect();
    assert!(!ids.is_empty(), "{label}: minted no revisions");
    for &id in &ids {
        for action in [
            ResolveSelectionAction::Accept,
            ResolveSelectionAction::Reject,
        ] {
            let projected = doc
                .project(stemma::Resolution::Selective {
                    ids: HashSet::from([id]),
                    action,
                })
                .unwrap_or_else(|e| panic!("{label}: project id={id} {action:?}: {e:?}"));
            let bytes = projected
                .serialize(&ExportOptions::default())
                .unwrap_or_else(|e| panic!("{label}: serialize id={id} {action:?}: {e:?}"));
            Document::parse(&bytes).unwrap_or_else(|e| {
                panic!("{label}: reparse id={id} {action:?} produced invalid OOXML: {e:?}")
            });
        }
    }
}

// ─── Word parity: a whole-row op mints no per-cell cellIns/cellDel ────────────

#[test]
fn row_delete_mints_row_marker_and_content_but_no_cell_marker() {
    let doc = deleted_row_doc(0);
    let revs = mutation_revisions(&doc);

    // Row-level structural delete marker: present (trPr w:del).
    assert!(
        revs.iter()
            .any(|(_, k, e)| *k == RevisionKind::Delete && e.starts_with("row[")),
        "expected a row-level delete marker, got {revs:?}"
    );
    // Cell CONTENT deletion: present (the cell text is tracked-deleted).
    assert!(
        revs.iter()
            .any(|(_, k, e)| *k == RevisionKind::Delete && e == "R0C0"),
        "expected the cell content deletion, got {revs:?}"
    );
    // Per-cell cellDel: ABSENT — Word does not emit it on a deleted row's cells,
    // and its presence is what made a cell-less row representable.
    assert!(
        !revs.iter().any(|(_, _, e)| e.starts_with("cell[")),
        "a whole-row deletion must NOT mint a per-cell cellDel, got {revs:?}"
    );
}

#[test]
fn row_insert_mints_row_marker_but_no_cell_marker() {
    let doc = inserted_row_doc();
    let revs = mutation_revisions(&doc);

    assert!(
        revs.iter()
            .any(|(_, k, e)| *k == RevisionKind::Insert && e.starts_with("row[")),
        "expected a row-level insert marker, got {revs:?}"
    );
    assert!(
        !revs.iter().any(|(_, _, e)| e.starts_with("cell[")),
        "a whole-row insertion must NOT mint a per-cell cellIns, got {revs:?}"
    );
}

// ─── The invariant: no subset resolution yields a cell-less row ───────────────

#[test]
fn delete_row_every_single_id_subset_serializes_to_importable_bytes() {
    for row_index in [0usize, 1usize] {
        assert_every_single_id_subset_importable(&deleted_row_doc(row_index), "delete_row");
    }
}

#[test]
fn insert_row_every_single_id_subset_serializes_to_importable_bytes() {
    assert_every_single_id_subset_importable(&inserted_row_doc(), "insert_row");
}

#[test]
fn delete_row_content_deletion_subset_stays_importable_multi_cell() {
    // Multi-cell row: accept every cell's CONTENT deletion but NOT the row-
    // structure marker. Each cell empties to a bare paragraph, the row and its
    // cells survive — a valid table (no cell-less row).
    let doc = deleted_row_doc_from(&two_row_two_col_table_docx(), 0);
    let content_ids: HashSet<u32> = mutation_revisions(&doc)
        .into_iter()
        .filter(|(_, k, e)| *k == RevisionKind::Delete && (e == "R0C0" || e == "R0C1"))
        .map(|(id, _, _)| id)
        .collect();
    assert_eq!(content_ids.len(), 2, "expected two cell content deletions");

    for action in [
        ResolveSelectionAction::Accept,
        ResolveSelectionAction::Reject,
    ] {
        let projected = doc
            .project(stemma::Resolution::Selective {
                ids: content_ids.clone(),
                action,
            })
            .unwrap_or_else(|e| panic!("project content-deletes {action:?}: {e:?}"));
        let bytes = projected
            .serialize(&ExportOptions::default())
            .unwrap_or_else(|e| panic!("serialize content-deletes {action:?}: {e:?}"));
        Document::parse(&bytes)
            .unwrap_or_else(|e| panic!("reparse content-deletes {action:?}: invalid OOXML: {e:?}"));
    }
}

// ─── Full-set resolution still deletes / restores the row correctly ───────────

#[test]
fn full_resolution_of_delete_is_target_on_accept_base_on_reject() {
    let doc = deleted_row_doc(0);
    let all_ids: HashSet<u32> = mutation_revisions(&doc)
        .into_iter()
        .map(|(id, _, _)| id)
        .collect();

    // Accept all → row 0 gone, only "R1C0" remains.
    let accepted = doc
        .project(stemma::Resolution::Selective {
            ids: all_ids.clone(),
            action: ResolveSelectionAction::Accept,
        })
        .expect("project accept-all");
    assert_eq!(strip_ws(&accepted.to_text()), "R1C0");

    // Reject all → both rows restored.
    let rejected = doc
        .project(stemma::Resolution::Selective {
            ids: all_ids,
            action: ResolveSelectionAction::Reject,
        })
        .expect("project reject-all");
    assert_eq!(strip_ws(&rejected.to_text()), "R0C0R1C0");
}

fn strip_ws(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}
