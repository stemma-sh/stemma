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
            identity: 0,
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
        matches!(redline.blocks[0].block, BlockNode::Table(_)),
        "first block is a table"
    );
    // The table is ONE insertion proposal carried natively on its rows
    // (§17.13.5.17), the form insert(table) authors and the table reopens
    // as: no separate block carrier. Word reads it as a single insertion
    // covering the whole table, content included.
    assert!(
        matches!(redline.blocks[0].status, TrackingStatus::Normal),
        "the insertion is carried by the rows, not an extra block carrier"
    );
    if let BlockNode::Table(t) = &redline.blocks[0].block {
        let identities: std::collections::BTreeSet<u32> = t
            .rows
            .iter()
            .map(|row| match &row.tracking_status {
                Some(TrackingStatus::Inserted(r)) => r.identity,
                other => panic!("every inserted-table row carries the insertion: {other:?}"),
            })
            .collect();
        assert_eq!(identities.len(), 1, "one proposal for the whole table");
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

// ─── Cells carry their source content, footnote references included ─────────

/// A capital-structure list whose amounts carry footnote references, with a
/// bold label run: "Estimated Total Value: $100¹", "Secured Debt: $40²",
/// followed by a paragraph that stays (so the range does not end the body).
fn footnoted_list_docx() -> Vec<u8> {
    let body = format!(
        concat!(
            r#"<w:p><w:r><w:rPr><w:b/></w:rPr><w:t>Estimated Total Value</w:t></w:r>"#,
            r#"<w:r><w:t xml:space="preserve">: $100</w:t></w:r>{}</w:p>"#,
            r#"<w:p><w:r><w:rPr><w:b/></w:rPr><w:t>Secured Debt</w:t></w:r>"#,
            r#"<w:r><w:t xml:space="preserve">: $40</w:t></w:r>{}</w:p>"#,
            r#"<w:p><w:r><w:t>After the list.</w:t></w:r></w:p>"#,
        ),
        note_ref(1),
        note_ref(2)
    );
    footnoted_docx(&body)
}

fn note_ref(id: u32) -> String {
    format!(
        r#"<w:r><w:rPr><w:rStyle w:val="FootnoteReference"/></w:rPr><w:footnoteReference w:id="{id}"/></w:r>"#
    )
}

/// A package with `body` and footnotes 1 and 2.
fn footnoted_docx(body: &str) -> Vec<u8> {
    let note = |id: u32, text: &str| {
        format!(
            r#"<w:footnote w:id="{id}"><w:p><w:r><w:rPr><w:rStyle w:val="FootnoteReference"/></w:rPr><w:footnoteRef/></w:r><w:r><w:t xml:space="preserve"> {text}</w:t></w:r></w:p></w:footnote>"#
        )
    };
    let footnotes = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:footnotes xmlns:w="{W_NS}"><w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote><w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote>{}{}</w:footnotes>"#,
        note(1, "Valuation as of the petition date."),
        note(2, "Secured by the plant.")
    );
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W_NS}" xmlns:r="{R_NS}"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/footnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdFn" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes" Target="footnotes.xml"/></Relationships>"#;
    use std::io::Write;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions = zip::write::FileOptions::default();
        for (name, data) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/document.xml", document_xml.as_str()),
            ("word/footnotes.xml", footnotes.as_str()),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

fn colon_table_step(canon: &CanonDoc) -> EditStep {
    EditStep::BlocksToTable {
        from_block_id: block_id_at(canon, 0),
        to_block_id: block_id_at(canon, 1),
        delimiter: ": ".to_string(),
        header: Some(vec!["Item".to_string(), "Amount".to_string()]),
        rationale: None,
    }
}

/// The ids of the footnote references in a table cell, in order.
fn cell_note_references(canon: &CanonDoc, row: usize, column: usize) -> Vec<String> {
    let table = canon
        .blocks
        .iter()
        .find_map(|tb| match &tb.block {
            BlockNode::Table(t) => Some(t),
            _ => None,
        })
        .expect("a table");
    table.rows[row].cells[column]
        .blocks
        .iter()
        .filter_map(|block| match block {
            BlockNode::Paragraph(p) => Some(p),
            _ => None,
        })
        .flat_map(|p| p.all_inlines())
        .filter_map(|inline| match inline {
            InlineNode::OpaqueInline(o) => match &o.kind {
                stemma::domain::OpaqueKind::FootnoteReference(r) => Some(r.reference_id.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// The normal (non-separator) notes' ids and texts.
fn normal_notes(canon: &CanonDoc) -> Vec<(String, String)> {
    canon
        .footnotes
        .iter()
        .filter(|note| note.note_type == stemma::domain::NoteType::Normal)
        .map(|note| {
            let text = note
                .blocks
                .iter()
                .filter_map(|tb| match &tb.block {
                    BlockNode::Paragraph(p) => Some(p),
                    _ => None,
                })
                .flat_map(|p| p.all_inlines())
                .filter_map(|inline| match inline {
                    InlineNode::Text(t) => Some(t.text.as_str()),
                    _ => None,
                })
                .collect::<String>();
            (note.id.clone(), text)
        })
        .collect()
}

fn saved_document_xml(doc: &Document) -> String {
    let bytes = doc.serialize(&ExportOptions::default()).expect("serialize");
    String::from_utf8(
        stemma::docx::DocxArchive::read(&bytes)
            .expect("read")
            .get("word/document.xml")
            .expect("document part")
            .to_vec(),
    )
    .expect("utf-8")
}

/// Domain rule: converting a list to a table keeps what the list says — the
/// footnote references after the amounts and the notes they point to (the
/// conversion changes layout, not content). In a tracked conversion the list
/// stays as a deletion beside the inserted table, and Word repairs a file in
/// which one note is referenced twice (Word-verified, even with one reference
/// deleted and one inserted). So each reference in the table points to its own
/// copy of the note: accept keeps the table with the copies, reject the list
/// with the originals, and either way every note is referenced once.
#[test]
fn a_tracked_conversion_carries_footnote_references_with_their_own_notes() {
    let doc = Document::parse(&footnoted_list_docx()).expect("parse");
    let canon = doc.snapshot().canonical.clone();
    let edited = doc
        .apply(&txn(
            vec![colon_table_step(&canon)],
            MaterializationMode::TrackedChange,
        ))
        .expect("a list whose amounts carry footnote references converts");

    let redline = &edited.snapshot().canonical;
    let copies = [
        cell_note_references(redline, 1, 1),
        cell_note_references(redline, 2, 1),
    ];
    assert_eq!(copies[0].len(), 1, "the reference follows its amount");
    assert_eq!(copies[1].len(), 1, "the reference follows its amount");
    assert!(
        !["1", "2"].contains(&copies[0][0].as_str()) && copies[0] != copies[1],
        "each table reference points to a note of its own: {copies:?}"
    );
    let notes = normal_notes(redline);
    for (copy, original) in [(&copies[0][0], "1"), (&copies[1][0], "2")] {
        let text_of = |id: &str| {
            notes
                .iter()
                .find(|(note, _)| note == id)
                .map(|(_, text)| text.clone())
                .unwrap_or_else(|| panic!("note {id} exists: {notes:?}"))
        };
        assert_eq!(
            text_of(copy),
            text_of(original),
            "the copy has the same content"
        );
    }

    // Saved: every note referenced exactly once, and the package is clean.
    let xml = saved_document_xml(&edited);
    for (id, _) in &notes {
        let references = xml
            .split("<w:footnoteReference ")
            .skip(1)
            .filter(|element| {
                element[..element.find("/>").expect("an empty element")]
                    .contains(&format!(r#"w:id="{id}""#))
            })
            .count();
        assert_eq!(references, 1, "note {id} is referenced once: {xml}");
    }
    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let report = validate(&bytes);
    assert!(report.ok, "{report:?}");

    // Accept: the table, its amounts with their references, the copies only.
    let accepted = edited.project(Resolution::AcceptAll).expect("accept");
    let accepted = &accepted.snapshot().canonical;
    assert_eq!(
        first_table_cell_texts(accepted),
        vec![
            vec!["Item".to_string(), "Amount".to_string()],
            vec!["Estimated Total Value".to_string(), "$100".to_string()],
            vec!["Secured Debt".to_string(), "$40".to_string()],
        ]
    );
    assert_eq!(cell_note_references(accepted, 1, 1), copies[0]);
    assert_eq!(
        normal_notes(accepted)
            .into_iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>(),
        vec![copies[0][0].clone(), copies[1][0].clone()],
        "the originals went with the list"
    );

    // Reject: the list with the original notes only.
    let rejected = edited.project(Resolution::RejectAll).expect("reject");
    let rejected = &rejected.snapshot().canonical;
    assert_eq!(table_count(rejected), 0);
    assert_eq!(
        normal_notes(rejected),
        vec![
            (
                "1".to_string(),
                " Valuation as of the petition date.".to_string()
            ),
            ("2".to_string(), " Secured by the plant.".to_string()),
        ]
    );
}

/// Without tracking the list simply becomes the table: the references move
/// with their amounts and the notes stay as they are.
#[test]
fn a_direct_conversion_moves_footnote_references() {
    let doc = Document::parse(&footnoted_list_docx()).expect("parse");
    let canon = doc.snapshot().canonical.clone();
    let converted = doc
        .apply(&txn(
            vec![colon_table_step(&canon)],
            MaterializationMode::Direct,
        ))
        .expect("direct conversion");
    let converted = &converted.snapshot().canonical;
    assert_eq!(cell_note_references(converted, 1, 1), vec!["1".to_string()]);
    assert_eq!(cell_note_references(converted, 2, 1), vec!["2".to_string()]);
    assert_eq!(normal_notes(converted).len(), 2, "no note is copied");
}

/// A cell keeps its source runs' formatting: the bold label stays bold.
#[test]
fn a_cell_keeps_its_source_formatting() {
    let doc = Document::parse(&footnoted_list_docx()).expect("parse");
    let canon = doc.snapshot().canonical.clone();
    let converted = doc
        .apply(&txn(
            vec![colon_table_step(&canon)],
            MaterializationMode::Direct,
        ))
        .expect("direct conversion");
    let xml = saved_document_xml(&converted);
    let label = xml
        .find(">Estimated Total Value</w:t>")
        .expect("the label is written");
    let run = &xml[xml[..label].rfind("<w:r>").expect("its run")..label];
    let bold = |run: &str| run.contains("<w:b/>") || run.contains("<w:b />");
    assert!(bold(run), "the label keeps its bold: {run}");
    let amount = xml.find(">$100</w:t>").expect("the amount is written");
    let run = &xml[xml[..amount].rfind("<w:r>").expect("its run")..amount];
    assert!(!bold(run), "the amount was never bold: {run}");
}

/// A note reference inside the delimiter belongs to no cell; the conversion
/// refuses rather than drop it.
#[test]
fn a_reference_inside_the_delimiter_is_refused() {
    // "Value:¹ $100": the reference sits between the ":" and the " " of ": ".
    let body = format!(
        concat!(
            r#"<w:p><w:r><w:t>Value:</w:t></w:r>{}<w:r><w:t xml:space="preserve"> $100</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t>Debt:</w:t></w:r>{}<w:r><w:t xml:space="preserve"> $40</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t>After the list.</w:t></w:r></w:p>"#,
        ),
        note_ref(1),
        note_ref(2)
    );
    let doc = Document::parse(&footnoted_docx(&body)).expect("parse");
    let canon = doc.snapshot().canonical.clone();
    let err = apply_transaction(
        &canon,
        &txn(
            vec![colon_table_step(&canon)],
            MaterializationMode::TrackedChange,
        ),
    )
    .expect_err("a reference no cell can hold is refused");
    assert!(
        err.to_string().contains("inside the delimiter"),
        "the refusal names the cause: {err}"
    );
}
