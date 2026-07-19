//! Integration tests for the granular list ops (`verbs::numbering`'s Indent,
//! Outdent, Restart, Continue, SetType) authored as tracked `w:pPrChange`.
//!
//! Domain rule (OOXML §17.13 accept/reject for a tracked pPrChange carrying the
//! previous w:numPr): `accept_all` keeps the NEW numbering; `reject_all` restores
//! the BASE numbering exactly. Plus fail-loud: indent/outdent past the 0..=8
//! bounds, and any list op on an unnumbered paragraph, are refused with a typed
//! error rather than clamped or guessed.
//!
//! The fixture carries a real `word/numbering.xml` with two list instances:
//! `numId=1` (decimal) and `numId=2` (bullet), so `SetType` can swap kinds by
//! re-pointing at an existing definition (never fabricating one).

use stemma::Resolution;
use stemma::api::Document;
use stemma::domain::{BlockNode, CanonDoc, NodeId, RevisionInfo};
use stemma::edit::{EditStep, EditTransaction, MaterializationMode, NumberingChange};

// ─── Fixture: a doc with a decimal list (numId=1) + bullet list (numId=2) ─────

fn make_two_list_docx(paras: &[(&str, Option<(u32, u32)>)]) -> Vec<u8> {
    let mut document_xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>"#,
    );
    for (text, numbering) in paras {
        document_xml.push_str("<w:p>");
        if let Some((num_id, ilvl)) = numbering {
            document_xml.push_str(&format!(
                r#"<w:pPr><w:numPr><w:ilvl w:val="{ilvl}"/><w:numId w:val="{num_id}"/></w:numPr></w:pPr>"#
            ));
        }
        document_xml.push_str(&format!(r#"<w:r><w:t>{text}</w:t></w:r></w:p>"#));
    }
    document_xml.push_str("<w:sectPr/></w:body></w:document>");

    // abstractNum 0 = decimal (numId=1), abstractNum 1 = bullet (numId=2).
    let numbering_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl><w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="lowerLetter"/><w:lvlText w:val="(%2)"/></w:lvl></w:abstractNum><w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/><w:lvlText w:val="&#8226;"/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num><w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num></w:numbering>"#;

    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId10" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/></Relationships>"#;

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
        zip.start_file("word/numbering.xml", opts).unwrap();
        zip.write_all(numbering_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

fn parse(paras: &[(&str, Option<(u32, u32)>)]) -> (Document, Vec<String>) {
    let doc = Document::parse(&make_two_list_docx(paras)).expect("parse two-list docx");
    let ids = doc.read().blocks.iter().map(|b| b.id.to_string()).collect();
    (doc, ids)
}

fn step(block_id: &str, change: NumberingChange) -> EditTransaction {
    EditTransaction {
        steps: vec![EditStep::SetParagraphNumbering {
            block_id: NodeId::from(block_id),
            semantic_hash: None,
            change,
            rationale: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("Counsel".to_string()),
            date: Some("2026-06-05T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

/// Apply expecting failure; return the RuntimeError (Document is not Debug, so
/// `unwrap_err` cannot be used directly).
fn apply_expect_err(doc: &Document, txn: &EditTransaction) -> stemma::RuntimeError {
    match doc.apply(txn) {
        Ok(_) => panic!("expected the op to be refused, but it applied"),
        Err(e) => e,
    }
}

/// `(num_id, ilvl)` of paragraph block_idx, or None if unnumbered.
fn para_numbering(doc: &CanonDoc, block_idx: usize) -> Option<(u32, u32)> {
    match &doc.blocks[block_idx].block {
        BlockNode::Paragraph(p) => p.numbering.as_ref().map(|n| (n.num_id, n.ilvl)),
        other => panic!("block {block_idx} is not a paragraph: {other:?}"),
    }
}

fn numbering_after(doc: &Document, resolution: Resolution, block_idx: usize) -> Option<(u32, u32)> {
    let resolved = doc.project(resolution).expect("project");
    para_numbering(&resolved.snapshot().canonical, block_idx)
}

// ─── Indent / Outdent ────────────────────────────────────────────────────────

#[test]
fn indent_accept_is_target_reject_is_base() {
    let (doc, ids) = parse(&[("Item one", Some((1, 0)))]);
    let edited = doc
        .apply(&step(&ids[0], NumberingChange::Indent))
        .expect("indent");
    assert_eq!(
        numbering_after(&edited, Resolution::AcceptAll, 0),
        Some((1, 1)),
        "accept => indented to ilvl 1"
    );
    assert_eq!(
        numbering_after(&edited, Resolution::RejectAll, 0),
        Some((1, 0)),
        "reject => base ilvl 0"
    );
}

#[test]
fn outdent_accept_is_target_reject_is_base() {
    let (doc, ids) = parse(&[("Item one", Some((1, 1)))]);
    let edited = doc
        .apply(&step(&ids[0], NumberingChange::Outdent))
        .expect("outdent");
    assert_eq!(
        numbering_after(&edited, Resolution::AcceptAll, 0),
        Some((1, 0))
    );
    assert_eq!(
        numbering_after(&edited, Resolution::RejectAll, 0),
        Some((1, 1))
    );
}

#[test]
fn outdent_at_level_zero_refused() {
    let (doc, ids) = parse(&[("Item one", Some((1, 0)))]);
    let err = apply_expect_err(&doc, &step(&ids[0], NumberingChange::Outdent));
    assert!(
        format!("{err:?}").contains("range") || err.message.contains("0..=8"),
        "outdent below level 0 must be refused: {err:?}"
    );
}

#[test]
fn indent_on_unnumbered_refused() {
    let (doc, ids) = parse(&[("Plain text", None)]);
    let err = apply_expect_err(&doc, &step(&ids[0], NumberingChange::Indent));
    assert!(
        err.message.contains("no list") || format!("{err:?}").contains("Unnumbered"),
        "indent on an unnumbered paragraph must be refused: {err:?}"
    );
}

// ─── Restart / Continue ──────────────────────────────────────────────────────

#[test]
fn restart_applies_and_keeps_the_list() {
    // Restart on a continuing list is a real change (it flips restart_numbering),
    // so it must apply — not be refused as a no-op. The restart flag is consumed
    // by the engine's numbering-restart materialization (it allocates a fresh
    // numId override at apply/serialize time, per `materialize_restart_numbering`),
    // so the SNAPSHOT no longer carries the flag; what we assert here is that the
    // paragraph still carries a list and the op was accepted. The flag-setting
    // itself is covered by `restart_sets_restart_flag` in the verb unit tests.
    let (doc, ids) = parse(&[("First", Some((1, 0))), ("Second", Some((1, 0)))]);
    // Capture the true base numbering BEFORE the edit.
    let base_num = para_numbering(&doc.snapshot().canonical, 1);
    let edited = doc
        .apply(&step(&ids[1], NumberingChange::Restart))
        .expect("restart applies");
    let canon = &edited.snapshot().canonical;
    if let BlockNode::Paragraph(p) = &canon.blocks[1].block {
        assert!(
            p.numbering.is_some(),
            "restart keeps the paragraph on its list"
        );
        assert_eq!(p.numbering.as_ref().unwrap().ilvl, 0, "level unchanged");
    } else {
        panic!("block 1 not a paragraph");
    }
    // Reject restores the exact base numbering (level + original numId), undoing
    // any fresh restart-override numId the materialization introduced.
    assert_eq!(
        numbering_after(&edited, Resolution::RejectAll, 1),
        base_num,
        "reject restores the base list/level"
    );
}

#[test]
fn continue_on_already_continuing_is_noop_refused() {
    // Imported list paragraphs have restart_numbering=false, so Continue is a
    // no-op and must be refused (no spurious tracked change).
    let (doc, ids) = parse(&[("First", Some((1, 0)))]);
    let err = apply_expect_err(&doc, &step(&ids[0], NumberingChange::Continue));
    assert!(
        err.message.contains("no-op")
            || format!("{err:?}").contains("NoNumberingChangeRequested")
            || err.message.contains("already has"),
        "continue on an already-continuing list is a no-op: {err:?}"
    );
}

// ─── SetType (bullet <-> numbered, via an EXISTING numId) ─────────────────────

#[test]
fn set_type_swaps_to_existing_bullet_list() {
    // Decimal item (numId=1) -> bullet (numId=2, an EXISTING definition). The
    // caller resolved num_id=2 from numbering.xml; the engine never fabricates.
    let (doc, ids) = parse(&[("Item one", Some((1, 0)))]);
    let edited = doc
        .apply(&step(
            &ids[0],
            NumberingChange::SetType {
                num_id: 2,
                synthesized_text: String::new(),
                is_bullet: true,
            },
        ))
        .expect("set_type to bullet");
    assert_eq!(
        numbering_after(&edited, Resolution::AcceptAll, 0),
        Some((2, 0)),
        "accept => re-pointed at the bullet list (numId 2), level preserved"
    );
    assert_eq!(
        numbering_after(&edited, Resolution::RejectAll, 0),
        Some((1, 0)),
        "reject => base decimal list (numId 1)"
    );
}

#[test]
fn set_type_on_unnumbered_refused() {
    let (doc, ids) = parse(&[("Plain", None)]);
    let err = apply_expect_err(
        &doc,
        &step(
            &ids[0],
            NumberingChange::SetType {
                num_id: 2,
                synthesized_text: String::new(),
                is_bullet: true,
            },
        ),
    );
    assert!(
        err.message.contains("no list") || format!("{err:?}").contains("Unnumbered"),
        "set_type on unnumbered paragraph must be refused: {err:?}"
    );
}
