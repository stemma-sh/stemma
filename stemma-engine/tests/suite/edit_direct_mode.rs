//! Direct-materialization-mode tests for the STRUCTURAL verbs, driven END-TO-END
//! from real DOCX bytes (import -> public API -> CanonDoc projections).
//!
//! The structural verbs (`ReplaceBlockRange` over a multi-block range,
//! `BlocksToTable`, `SetBlockRangeAttr`) are exercised elsewhere only in
//! `TrackedChange` mode; their `Direct`-mode branch is otherwise dark.
//!
//! DOMAIN RULE (the clean invariant these tests pin):
//!   Direct mode == "the already-accepted state". Applying an edit in `Direct`
//!   mode must produce the SAME document text + structure as applying the same
//!   edit in `TrackedChange` mode and then accepting all changes — and the
//!   Direct result must read as Normal (no `Inserted` / `Deleted` tracked
//!   segments or block statuses, no lingering formatting-change record), so a
//!   subsequent reject-all is a no-op.
//!
//! Inputs are authored as body XML, zipped into a minimal valid .docx, and
//! imported with `Document::parse` — the same bytes-in idiom as
//! `blocks_to_table.rs` / `spec_span_addressing.rs`. No hand-built IR.
//! Corpus-free, daily tier.

use stemma::api::Document;
use stemma::domain::{
    BlockNode, CanonDoc, NodeId, ParagraphNode, RevisionInfo, TableNode, TrackingStatus,
};
use stemma::edit::{
    BlockSpec, EditStep, EditTransaction, MaterializationMode, ParagraphBlockSpec,
    apply_transaction, parse_paragraph_markup,
};
use stemma::vocabulary;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

// ─── Synthetic-docx helper (adapted from spec_span_addressing.rs ~line 30) ───

/// Build a minimal valid .docx from a body-XML snippet.
fn make_docx(body_inner: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W_NS}" xmlns:r="{R_NS}"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
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

fn plain_paras(texts: &[&str]) -> Vec<u8> {
    let body = texts
        .iter()
        .map(|t| format!(r#"<w:p><w:r><w:t xml:space="preserve">{t}</w:t></w:r></w:p>"#))
        .collect::<String>();
    make_docx(&body)
}

fn test_revision() -> RevisionInfo {
    RevisionInfo {
        revision_id: 100,
        author: Some("Test Author".to_string()),
        date: Some("2026-06-07T00:00:00Z".to_string()),
        apply_op_id: None,
    }
}

fn tx(steps: Vec<EditStep>, mode: MaterializationMode) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        materialization_mode: mode,
        revision: test_revision(),
    }
}

/// Apply a transaction on the imported CanonDoc and return the resulting doc.
fn apply(canon: &CanonDoc, steps: Vec<EditStep>, mode: MaterializationMode) -> CanonDoc {
    apply_transaction(canon, &tx(steps, mode))
        .expect("apply must succeed")
        .0
}

/// The id of the i-th top-level block, as assigned by import.
fn block_id_at(canon: &CanonDoc, idx: usize) -> NodeId {
    match &canon.blocks[idx].block {
        BlockNode::Paragraph(p) => p.id.clone(),
        BlockNode::Table(t) => t.id.clone(),
        BlockNode::OpaqueBlock(o) => o.id.clone(),
    }
}

// ─── projection + comparison helpers (all over the public CanonDoc) ──────────

/// Block-order fingerprint: (kind, visible-text). Tables fingerprint their
/// cell-text matrix. Ignores tracking status and block ids, so it compares "what
/// the document reads as" — exactly the accepted-state equality being claimed.
fn doc_fingerprint(doc: &CanonDoc) -> Vec<(String, String)> {
    doc.blocks
        .iter()
        .map(|tb| match &tb.block {
            BlockNode::Paragraph(p) => ("para".to_string(), para_visible_text(p)),
            BlockNode::Table(t) => ("table".to_string(), table_cell_matrix(t)),
            BlockNode::OpaqueBlock(o) => ("opaque".to_string(), o.id.to_string()),
        })
        .collect()
}

fn para_visible_text(p: &ParagraphNode) -> String {
    let mut text = String::new();
    for seg in &p.segments {
        for inline in &seg.inlines {
            if let stemma::domain::InlineNode::Text(t) = inline {
                text.push_str(&t.text);
            }
        }
    }
    text
}

fn table_cell_matrix(t: &TableNode) -> String {
    let mut out = String::new();
    for row in &t.rows {
        for cell in &row.cells {
            out.push('[');
            for block in &cell.blocks {
                if let BlockNode::Paragraph(p) = block {
                    out.push_str(&para_visible_text(p));
                }
            }
            out.push(']');
        }
        out.push(';');
    }
    out
}

/// True if the doc carries ANY Inserted/Deleted tracking — block status, tracked
/// segment, tracked paragraph mark, table row/cell tracking, or a recorded
/// formatting change. This is what "reads as Normal" forbids.
fn has_any_tracked(doc: &CanonDoc) -> bool {
    fn para_has_tracked(p: &ParagraphNode) -> bool {
        p.segments
            .iter()
            .any(|s| !matches!(s.status, TrackingStatus::Normal))
            || matches!(&p.para_mark_status, Some(s) if !matches!(s, TrackingStatus::Normal))
            || p.formatting_change.is_some()
    }
    fn block_has_tracked(b: &BlockNode) -> bool {
        match b {
            BlockNode::Paragraph(p) => para_has_tracked(p),
            BlockNode::Table(t) => {
                t.formatting_change.is_some()
                    || t.rows.iter().any(|row| {
                        matches!(&row.tracking_status, Some(s) if !matches!(s, TrackingStatus::Normal))
                            || row.cells.iter().any(|cell| {
                                matches!(&cell.tracking_status, Some(s) if !matches!(s, TrackingStatus::Normal))
                                    || cell.blocks.iter().any(block_has_tracked)
                            })
                    })
            }
            BlockNode::OpaqueBlock(_) => false,
        }
    }
    doc.blocks
        .iter()
        .any(|tb| !matches!(tb.status, TrackingStatus::Normal) || block_has_tracked(&tb.block))
}

fn accepted(doc: &CanonDoc) -> CanonDoc {
    let mut d = doc.clone();
    stemma::accept_all(&mut d);
    d
}

fn rejected(doc: &CanonDoc) -> CanonDoc {
    let mut d = doc.clone();
    stemma::reject_all_with_styles(&mut d, None);
    d
}

/// Resolve the `body_text` role id from the imported doc's vocabulary (don't
/// hardcode — the clusterer names roles heuristically).
fn body_role(canon: &CanonDoc) -> String {
    let vocab = vocabulary::extract_vocabulary(canon);
    vocab
        .paragraph_roles
        .iter()
        .find(|r| r.id == "body_text")
        .or_else(|| vocab.paragraph_roles.first())
        .expect("at least one paragraph role")
        .id
        .clone()
}

// ─── 1. ReplaceBlockRange over a multi-block range ───────────────────────────

#[test]
fn replace_block_range_direct_equals_accept_all_of_tracked() {
    // Replace the contiguous range [p0..=p1] with two fresh paragraphs. (from !=
    // to forces the structural delete+insert path, not inline diff.)
    // DOMAIN RULE: Direct == accept_all(TrackedChange).
    let bytes = plain_paras(&["alpha line", "beta line", "gamma tail"]);
    let canon = Document::parse(&bytes)
        .expect("parse")
        .snapshot()
        .canonical
        .clone();
    let role = body_role(&canon);
    let from = block_id_at(&canon, 0);
    let to = block_id_at(&canon, 1);

    let steps = || {
        vec![EditStep::ReplaceBlockRange {
            from_block_id: from.clone(),
            to_block_id: to.clone(),
            rationale: None,
            expect: "alpha".to_string(),
            semantic_hash: None,
            blocks: vec![
                BlockSpec::Paragraph(ParagraphBlockSpec {
                    role: Some(role.clone()),
                    content: parse_paragraph_markup("first replacement").unwrap(),
                    restart_numbering: false,
                    list: None,
                }),
                BlockSpec::Paragraph(ParagraphBlockSpec {
                    role: Some(role.clone()),
                    content: parse_paragraph_markup("second replacement").unwrap(),
                    restart_numbering: false,
                    list: None,
                }),
            ],
        }]
    };

    let tracked = apply(&canon, steps(), MaterializationMode::TrackedChange);
    let direct = apply(&canon, steps(), MaterializationMode::Direct);

    assert_eq!(
        doc_fingerprint(&direct),
        doc_fingerprint(&accepted(&tracked)),
        "Direct multi-block replace must equal accept-all of the tracked replace"
    );
    assert_eq!(
        doc_fingerprint(&direct),
        vec![
            ("para".to_string(), "first replacement".to_string()),
            ("para".to_string(), "second replacement".to_string()),
            ("para".to_string(), "gamma tail".to_string()),
        ],
        "the two source paragraphs are replaced; the third is untouched"
    );

    assert!(
        !has_any_tracked(&direct),
        "Direct result must carry no Inserted/Deleted tracking"
    );
    assert_eq!(
        doc_fingerprint(&direct),
        doc_fingerprint(&rejected(&direct)),
        "reject-all on a Direct result must be a no-op"
    );
}

// ─── 2. BlocksToTable ────────────────────────────────────────────────────────

#[test]
fn blocks_to_table_direct_equals_accept_all_of_tracked() {
    // Convert three "Term — def" paragraphs into a 2-col table with a header.
    // DOMAIN RULE: Direct == accept_all(TrackedChange).
    let bytes = plain_paras(&["Uptime — 99.9%", "Support — 24/7", "Trial — 30 days"]);
    let canon = Document::parse(&bytes)
        .expect("parse")
        .snapshot()
        .canonical
        .clone();
    let from = block_id_at(&canon, 0);
    let to = block_id_at(&canon, 2);

    let steps = || {
        vec![EditStep::BlocksToTable {
            from_block_id: from.clone(),
            to_block_id: to.clone(),
            delimiter: " — ".to_string(),
            header: Some(vec!["Feature".to_string(), "Notes".to_string()]),
            rationale: None,
        }]
    };

    let tracked = apply(&canon, steps(), MaterializationMode::TrackedChange);
    let direct = apply(&canon, steps(), MaterializationMode::Direct);

    assert_eq!(
        doc_fingerprint(&direct),
        doc_fingerprint(&accepted(&tracked)),
        "Direct blocks_to_table must equal accept-all of the tracked conversion"
    );
    assert_eq!(
        doc_fingerprint(&direct),
        vec![
            (
                "table".to_string(),
                "[Feature][Notes];[Uptime][99.9%];[Support][24/7];[Trial][30 days];".to_string()
            ),
            ("para".to_string(), String::new()),
        ],
        "Direct conversion leaves the table plus the empty surviving final mark \
         (the source range ended the document; a body must end with a paragraph)"
    );

    assert!(
        !has_any_tracked(&direct),
        "Direct table must carry no Inserted tracking on rows/cells/runs"
    );
    assert_eq!(
        doc_fingerprint(&direct),
        doc_fingerprint(&rejected(&direct)),
        "reject-all on a Direct conversion must be a no-op"
    );
}

// ─── 3. SetBlockRangeAttr ────────────────────────────────────────────────────

#[test]
fn set_block_range_attr_direct_equals_accept_all_of_tracked() {
    // A Heading1-styled paragraph supplies a distinct role; promote the plain
    // body paragraph into it. The style change guarantees a real pPr delta (not
    // a no-op). DOMAIN RULE: Direct == accept_all(TrackedChange).
    let body = concat!(
        r#"<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Big Heading</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t xml:space="preserve">plain body paragraph</w:t></w:r></w:p>"#,
    );
    let canon = Document::parse(&make_docx(body))
        .expect("parse")
        .snapshot()
        .canonical
        .clone();
    let target = block_id_at(&canon, 1); // the body paragraph

    // Resolve the heading role id from the imported vocabulary.
    let vocab = vocabulary::extract_vocabulary(&canon);
    let heading_role = vocab
        .paragraph_roles
        .iter()
        .find(|r| r.id != "body_text")
        .expect("the Heading1 paragraph must produce a second, non-body role")
        .id
        .clone();

    let steps = || {
        vec![EditStep::SetBlockRangeAttr {
            from_block_id: target.clone(),
            to_block_id: target.clone(),
            role: heading_role.clone(),
            rationale: None,
        }]
    };

    let tracked = apply(&canon, steps(), MaterializationMode::TrackedChange);
    let direct = apply(&canon, steps(), MaterializationMode::Direct);

    let find_target = |doc: &CanonDoc| -> ParagraphNode {
        doc.blocks
            .iter()
            .find_map(|tb| match &tb.block {
                BlockNode::Paragraph(p) if p.id == target => Some((**p).clone()),
                _ => None,
            })
            .expect("target paragraph present")
    };

    let direct_body = find_target(&direct);
    let accepted_body = find_target(&accepted(&tracked));

    // Direct's new paragraph properties == the accepted-state pPr.
    assert_eq!(
        direct_body.style_id, accepted_body.style_id,
        "Direct must carry the exemplar's style (same as accepted state)"
    );
    assert_eq!(direct_body.numbering, accepted_body.numbering);
    assert_eq!(direct_body.heading_level, accepted_body.heading_level);
    assert_eq!(
        direct_body.style_id.as_deref(),
        Some("Heading1"),
        "the verb actually changed the style (not a no-op)"
    );

    // No formatting-change record on the Direct result → reject is a no-op.
    assert!(
        direct_body.formatting_change.is_none(),
        "Direct set_attr must NOT leave a ParagraphFormattingChange (pPrChange) behind"
    );
    assert!(
        !has_any_tracked(&direct),
        "Direct set_attr result must read as Normal"
    );
    let rej_body = rejected(&direct)
        .blocks
        .iter()
        .find_map(|tb| match &tb.block {
            BlockNode::Paragraph(p) if p.id == target => Some((**p).clone()),
            _ => None,
        })
        .expect("target present");
    assert_eq!(
        rej_body.style_id, direct_body.style_id,
        "reject-all on a Direct set_attr must be a no-op (new pPr kept)"
    );
}
