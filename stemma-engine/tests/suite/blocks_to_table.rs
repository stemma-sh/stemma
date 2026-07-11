//! Integration tests for `BlocksToTable` — converting a contiguous run of
//! paragraphs (a bullet list) into a TABLE as a single composed tracked change.
//!
//! Contract under test (CLAUDE.md "no silent fallbacks"; the tracked-change
//! model):
//!  - the conversion is a real tracked change composed of a tracked INSERT (the
//!    new table) + a tracked DELETE (the source paragraphs), so:
//!      * accept-all => the table only (header + one row per source paragraph,
//!        cells carrying the delimiter-split text; the source paragraphs gone);
//!      * reject-all => the original paragraphs verbatim (no table);
//!  - both projections serialize validator-clean;
//!  - the pre-projection redline carries exactly the envelope shape we claim:
//!    an Inserted table block sitting before a run of Deleted source paragraphs;
//!  - opaque preservation: a source paragraph carrying an opaque inline is
//!    refused (`BlocksToTableOpaqueInline`), never silently dropped;
//!  - fail-loud at the edge: a ragged split => `BlocksToTableSplitMismatch`;
//!    a non-paragraph in the range => `BlocksToTableNonParagraph`.
//!
//! Daily tier, corpus-free (synthesized in-memory DOCX).

use stemma::Resolution;
use stemma::api::{Document, validate};
use stemma::domain::{BlockNode, CanonDoc, InlineNode, NodeId, RevisionInfo, TrackingStatus};
use stemma::edit::{EditError, EditStep, EditTransaction, MaterializationMode, apply_transaction};
use stemma::runtime::ExportOptions;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

/// Build a DOCX from raw `<w:p>...` body XML, with an optional extra
/// document-rel (id, type, target) so a `<w:hyperlink r:id=...>` resolves.
fn make_docx(body_paragraphs: &str, extra_doc_rel: Option<(&str, &str, &str)>) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W_NS}" xmlns:r="{R_NS}"><w:body>{body_paragraphs}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let extra = extra_doc_rel
        .map(|(id, ty, target)| {
            format!(
                r#"<Relationship Id="{id}" Type="{ty}" Target="{target}" TargetMode="External"/>"#
            )
        })
        .unwrap_or_default();
    let doc_rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{extra}</Relationships>"#
    );

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

/// A simple bullet-list-shaped DOCX: three plain paragraphs, each "Term — def".
fn three_bullet_docx() -> Vec<u8> {
    let body = [
        "Uptime — 99.9% measured monthly",
        "Support — 24/7 email and chat",
        "Trial — 30 days, no card required",
    ]
    .iter()
    .map(|t| format!(r#"<w:p><w:r><w:t xml:space="preserve">{t}</w:t></w:r></w:p>"#))
    .collect::<String>();
    make_docx(&body, None)
}

fn block_id_at(canon: &CanonDoc, idx: usize) -> NodeId {
    match &canon.blocks[idx].block {
        BlockNode::Paragraph(p) => p.id.clone(),
        BlockNode::Table(t) => t.id.clone(),
        BlockNode::OpaqueBlock(o) => o.id.clone(),
    }
}

/// All live paragraph texts (segments dropped if Deleted), in block order.
/// Used to read the accept/reject projection.
fn paragraph_texts(canon: &CanonDoc) -> Vec<String> {
    let mut out = Vec::new();
    for tb in &canon.blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            let mut text = String::new();
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::Text(t) = inline {
                        text.push_str(&t.text);
                    }
                }
            }
            out.push(text);
        }
    }
    out
}

fn table_count(canon: &CanonDoc) -> usize {
    canon
        .blocks
        .iter()
        .filter(|tb| matches!(tb.block, BlockNode::Table(_)))
        .count()
}

/// rows × cells text matrix for the first table in the doc.
fn first_table_cell_texts(canon: &CanonDoc) -> Vec<Vec<String>> {
    let table = canon
        .blocks
        .iter()
        .find_map(|tb| match &tb.block {
            BlockNode::Table(t) => Some(t),
            _ => None,
        })
        .expect("no table in doc");
    table
        .rows
        .iter()
        .map(|row| {
            row.cells
                .iter()
                .map(|cell| {
                    let mut text = String::new();
                    for block in &cell.blocks {
                        if let BlockNode::Paragraph(p) = block {
                            for seg in &p.segments {
                                for i in &seg.inlines {
                                    if let InlineNode::Text(t) = i {
                                        text.push_str(&t.text);
                                    }
                                }
                            }
                        }
                    }
                    text
                })
                .collect()
        })
        .collect()
}

fn txn(steps: Vec<EditStep>, mode: MaterializationMode) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        materialization_mode: mode,
        revision: RevisionInfo {
            revision_id: 7,
            author: Some("L2T".to_string()),
            date: Some("2026-06-05T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

fn list_to_table_step(from: NodeId, to: NodeId, header: Option<Vec<String>>) -> EditStep {
    EditStep::BlocksToTable {
        from_block_id: from,
        to_block_id: to,
        delimiter: " — ".to_string(),
        header,
        rationale: None,
    }
}

/// The headline case: a 3-bullet list -> a 2-column table with a header.
/// accept-all == the table (header "Feature"/"Notes" present, cells carry the
/// split text, source paragraphs gone); reject-all == the original paragraphs.
#[test]
fn three_bullets_to_two_col_table_with_header() {
    let doc = Document::parse(&three_bullet_docx()).expect("parse");
    let canon = doc.snapshot().canonical.clone();
    assert_eq!(canon.blocks.len(), 3, "fixture is three paragraphs");
    let from = block_id_at(&canon, 0);
    let to = block_id_at(&canon, 2);

    let edited = doc
        .apply(&txn(
            vec![list_to_table_step(
                from,
                to,
                Some(vec!["Feature".to_string(), "Notes".to_string()]),
            )],
            MaterializationMode::TrackedChange,
        ))
        .expect("apply blocks_to_table");

    // --- The redline envelope: an Inserted table before three Deleted paras. ---
    let redline = &edited.snapshot().canonical;
    assert_eq!(
        redline.blocks.len(),
        4,
        "table + 3 deleted source paragraphs"
    );
    assert!(
        matches!(redline.blocks[0].status, TrackingStatus::Inserted(_)),
        "first block is the tracked-inserted table"
    );
    assert!(
        matches!(redline.blocks[0].block, BlockNode::Table(_)),
        "first block is a table"
    );
    // The table is tracked-inserted at EVERY level: rows, cells, and cell runs.
    if let BlockNode::Table(t) = &redline.blocks[0].block {
        for row in &t.rows {
            assert!(
                matches!(row.tracking_status, Some(TrackingStatus::Inserted(_))),
                "every inserted-table row must carry Inserted tracking (w:trPr/w:ins)"
            );
            for cell in &row.cells {
                assert!(
                    matches!(cell.tracking_status, Some(TrackingStatus::Inserted(_))),
                    "every inserted-table cell must carry Inserted tracking (w:cellIns)"
                );
                for block in &cell.blocks {
                    if let BlockNode::Paragraph(p) = block {
                        for seg in &p.segments {
                            assert!(
                                matches!(seg.status, TrackingStatus::Inserted(_)),
                                "inserted-table cell runs must be Inserted (w:ins) so a \
                                 run-level reader rejects them too"
                            );
                        }
                    }
                }
            }
        }
    }
    // The source range ends the DOCUMENT here, so the tail rule applies: every
    // source paragraph but the last is tracked-deleted (block + mark); the LAST
    // becomes the surviving final mark — block-Normal, runs tracked-deleted,
    // pilcrow untracked — because Word can never resolve a revision on the
    // document-final paragraph mark.
    let last = redline.blocks.len() - 1;
    for (i, tb) in redline.blocks[1..last].iter().enumerate() {
        assert!(
            matches!(tb.status, TrackingStatus::Deleted(_)),
            "source paragraph {i} must be tracked-deleted"
        );
        if let BlockNode::Paragraph(p) = &tb.block {
            assert!(
                matches!(p.para_mark_status, Some(TrackingStatus::Deleted(_))),
                "deleted source paragraph {i} must have a deleted paragraph mark"
            );
        } else {
            panic!("source block {i} should still be a paragraph in the redline");
        }
    }
    let survivor = &redline.blocks[last];
    assert!(
        matches!(survivor.status, TrackingStatus::Normal),
        "the final source paragraph survives as the document-final mark"
    );
    if let BlockNode::Paragraph(p) = &survivor.block {
        assert!(
            !matches!(p.para_mark_status, Some(TrackingStatus::Deleted(_))),
            "the surviving final mark is untracked"
        );
        for seg in &p.segments {
            assert!(
                matches!(seg.status, TrackingStatus::Deleted(_)),
                "the survivor's runs are tracked-deleted"
            );
        }
    } else {
        panic!("the survivor should be a paragraph");
    }

    // --- accept-all: the table only, header + 3 rows of split cells. ---
    let accepted = edited.project(Resolution::AcceptAll).expect("accept");
    let acc = accepted.snapshot().canonical.clone();
    assert_eq!(table_count(&acc), 1, "accept-all keeps exactly the table");
    assert_eq!(
        paragraph_texts(&acc),
        vec![String::new()],
        "accept-all drops the source text; the empty surviving final mark remains \
         (a body must end with a paragraph mark — Word leaves the same survivor)"
    );
    let cells = first_table_cell_texts(&acc);
    assert_eq!(
        cells,
        vec![
            vec!["Feature".to_string(), "Notes".to_string()],
            vec!["Uptime".to_string(), "99.9% measured monthly".to_string()],
            vec!["Support".to_string(), "24/7 email and chat".to_string()],
            vec!["Trial".to_string(), "30 days, no card required".to_string()],
        ],
        "header + one row per bullet, each split on the delimiter"
    );

    // accept-all serializes validator-clean.
    let acc_bytes = accepted
        .serialize(&ExportOptions::default())
        .expect("serialize accept");
    let report = validate(&acc_bytes);
    assert!(
        report.ok,
        "accept-all must open validator-clean: {:?}",
        report.issues
    );

    // --- reject-all: the original three paragraphs verbatim, no table. ---
    let rejected = edited.project(Resolution::RejectAll).expect("reject");
    let rej = rejected.snapshot().canonical.clone();
    assert_eq!(table_count(&rej), 0, "reject-all drops the table");
    assert_eq!(
        paragraph_texts(&rej),
        vec![
            "Uptime — 99.9% measured monthly".to_string(),
            "Support — 24/7 email and chat".to_string(),
            "Trial — 30 days, no card required".to_string(),
        ],
        "reject-all restores the original paragraphs verbatim"
    );

    let rej_bytes = rejected
        .serialize(&ExportOptions::default())
        .expect("serialize reject");
    let report = validate(&rej_bytes);
    assert!(
        report.ok,
        "reject-all must open validator-clean: {:?}",
        report.issues
    );
}

/// No explicit header: the column count is the first row's split. accept-all is
/// a header-less table whose rows carry the split text.
#[test]
fn no_header_uses_first_row_shape() {
    let doc = Document::parse(&three_bullet_docx()).expect("parse");
    let canon = doc.snapshot().canonical.clone();
    let from = block_id_at(&canon, 0);
    let to = block_id_at(&canon, 2);

    let edited = doc
        .apply(&txn(
            vec![list_to_table_step(from, to, None)],
            MaterializationMode::TrackedChange,
        ))
        .expect("apply");

    let accepted = edited.project(Resolution::AcceptAll).expect("accept");
    let cells = first_table_cell_texts(&accepted.snapshot().canonical);
    assert_eq!(cells.len(), 3, "three body rows, no header");
    assert_eq!(
        cells[0],
        vec!["Uptime".to_string(), "99.9% measured monthly".to_string()]
    );
}

/// Without a header, a source paragraph that does not split into the first
/// row's column count is refused — a ragged grid is never emitted.
#[test]
fn no_header_ragged_split_is_refused() {
    // First bullet splits into 2; the second has no delimiter (1 cell).
    let body = ["A — one", "B has no delimiter", "C — three"]
        .iter()
        .map(|t| format!(r#"<w:p><w:r><w:t xml:space="preserve">{t}</w:t></w:r></w:p>"#))
        .collect::<String>();
    let doc = Document::parse(&make_docx(&body, None)).expect("parse");
    let canon = doc.snapshot().canonical.clone();
    let from = block_id_at(&canon, 0);
    let to = block_id_at(&canon, 2);

    let err = apply_transaction(
        &canon,
        &txn(
            vec![list_to_table_step(from, to, None)],
            MaterializationMode::TrackedChange,
        ),
    )
    .expect_err("ragged split must fail");
    assert!(
        matches!(
            err,
            EditError::BlocksToTableSplitMismatch {
                actual_columns: 1,
                expected_columns: 2,
                ..
            }
        ),
        "got {err:?}"
    );
}

/// WITH a header the column count is fixed at header.len(); a row without the
/// delimiter is NOT ragged — its whole text goes in the first cell and the rest
/// are padded empty (lossless). A row with EXTRA delimiters folds the overflow
/// into the last cell (no text dropped).
#[test]
fn header_pads_short_rows_and_folds_extra() {
    let body = [
        "A — one",            // exactly 2 cells
        "B has no delimiter", // 1 cell -> padded to ["B has no delimiter", ""]
        "C — two — three",    // 3 fragments -> folds to ["C", "two — three"]
    ]
    .iter()
    .map(|t| format!(r#"<w:p><w:r><w:t xml:space="preserve">{t}</w:t></w:r></w:p>"#))
    .collect::<String>();
    let doc = Document::parse(&make_docx(&body, None)).expect("parse");
    let canon = doc.snapshot().canonical.clone();
    let from = block_id_at(&canon, 0);
    let to = block_id_at(&canon, 2);

    let edited = doc
        .apply(&txn(
            vec![list_to_table_step(
                from,
                to,
                Some(vec!["Feature".to_string(), "Notes".to_string()]),
            )],
            MaterializationMode::TrackedChange,
        ))
        .expect("apply");

    let accepted = edited.project(Resolution::AcceptAll).expect("accept");
    let cells = first_table_cell_texts(&accepted.snapshot().canonical);
    assert_eq!(
        cells,
        vec![
            vec!["Feature".to_string(), "Notes".to_string()],
            vec!["A".to_string(), "one".to_string()],
            vec!["B has no delimiter".to_string(), String::new()],
            vec!["C".to_string(), "two — three".to_string()],
        ],
        "header fixes 2 columns: short rows padded, extra delimiters folded"
    );

    // Still validator-clean and reject-restores-the-paragraphs.
    let acc_bytes = accepted.serialize(&ExportOptions::default()).expect("ser");
    assert!(
        validate(&acc_bytes).ok,
        "padded grid must be validator-clean"
    );
    let rejected = edited.project(Resolution::RejectAll).expect("reject");
    assert_eq!(
        paragraph_texts(&rejected.snapshot().canonical),
        vec![
            "A — one".to_string(),
            "B has no delimiter".to_string(),
            "C — two — three".to_string(),
        ],
        "reject-all restores the original paragraphs verbatim"
    );
}

/// A source paragraph carrying an opaque inline (a hyperlink) is refused — the
/// conversion would lose the opaque on accept-all, so we never proceed.
#[test]
fn opaque_inline_in_source_is_refused() {
    let hyperlink_rel = (
        "rId100",
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink",
        "https://example.com/",
    );
    let body = concat!(
        r#"<w:p><w:r><w:t xml:space="preserve">Plain — first</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t xml:space="preserve">Link — </w:t></w:r>"#,
        r#"<w:hyperlink r:id="rId100"><w:r><w:t>here</w:t></w:r></w:hyperlink></w:p>"#,
    );
    let doc = Document::parse(&make_docx(body, Some(hyperlink_rel))).expect("parse");
    let canon = doc.snapshot().canonical.clone();
    let from = block_id_at(&canon, 0);
    let to = block_id_at(&canon, 1);

    let err = apply_transaction(
        &canon,
        &txn(
            vec![list_to_table_step(
                from,
                to,
                Some(vec!["K".to_string(), "V".to_string()]),
            )],
            MaterializationMode::TrackedChange,
        ),
    )
    .expect_err("opaque-bearing source must be refused");
    assert!(
        matches!(err, EditError::BlocksToTableOpaqueInline { .. }),
        "got {err:?}"
    );
}

/// A non-paragraph in the source range (here: a table) is refused.
#[test]
fn non_paragraph_in_range_is_refused() {
    let body = concat!(
        r#"<w:p><w:r><w:t>A — one</w:t></w:r></w:p>"#,
        r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol/></w:tblGrid>"#,
        r#"<w:tr><w:tc><w:tcPr/><w:p><w:r><w:t>cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
        r#"<w:p><w:r><w:t>C — three</w:t></w:r></w:p>"#,
    );
    let doc = Document::parse(&make_docx(body, None)).expect("parse");
    let canon = doc.snapshot().canonical.clone();
    assert_eq!(canon.blocks.len(), 3, "para, table, para");
    let from = block_id_at(&canon, 0);
    let to = block_id_at(&canon, 2);

    let err = apply_transaction(
        &canon,
        &txn(
            vec![list_to_table_step(
                from,
                to,
                Some(vec!["K".to_string(), "V".to_string()]),
            )],
            MaterializationMode::TrackedChange,
        ),
    )
    .expect_err("table in range must be refused");
    assert!(
        matches!(
            err,
            EditError::BlocksToTableNonParagraph {
                actual_kind: "table",
                ..
            }
        ),
        "got {err:?}"
    );
}
