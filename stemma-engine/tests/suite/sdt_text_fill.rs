//! `sdt_text_fill` — content-control value fills (RFC-0002 §Phase-2).
//!
//! INLINE controls carry their bytes on the IR node, so they resolve through the
//! IR accept/reject path (like `opaque_text_edit`). BLOCK-level (body) controls
//! keep their bytes in the serialize scaffold: the fill is STAGED and applied at
//! save time, and its reversal is the whole-document BYTE path (`reject_all_docx`)
//! — so these tests drive that path end-to-end.
//!
//! Corpus-free: fixtures built in-process.

use stemma::docx::DocxArchive;
use stemma::domain::{BlockNode, OpaqueKind, RevisionInfo};
use stemma::edit::{EditStep, EditTransaction, MaterializationMode};
use stemma::normalize::{normalize_docx, reject_all_docx};
use stemma::opaque_targets::{OpaqueTextTargetKind, opaque_text_targets};
use stemma::{
    CanonDoc, DocxRuntime, SimpleRuntime, accept_all, api::Document, reject_all_with_styles,
};

fn docx_bytes(body_inner: &str) -> Vec<u8> {
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
        for (name, data) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/document.xml", &document_xml),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

fn import(body_inner: &str) -> CanonDoc {
    (*SimpleRuntime::new()
        .import_docx(&docx_bytes(body_inner))
        .unwrap()
        .canonical)
        .clone()
}

fn txn(step: EditStep, mode: MaterializationMode) -> EditTransaction {
    EditTransaction {
        steps: vec![step],
        summary: None,
        materialization_mode: mode,
        revision: RevisionInfo {
            revision_id: 1,
            author: Some("Gate".to_string()),
            date: Some("2026-06-01T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

fn document_xml_of(bytes: &[u8]) -> String {
    let archive = DocxArchive::read(bytes).unwrap();
    String::from_utf8(archive.get("word/document.xml").unwrap().to_vec()).unwrap()
}

// ─── Inline SDT (IR path) ────────────────────────────────────────────────────

#[test]
fn inline_sdt_fill_tracked_is_reversible() {
    let base = import(
        r#"<w:p><w:sdt><w:sdtPr><w:alias w:val="Status"/></w:sdtPr><w:sdtContent><w:r><w:t>Draft</w:t></w:r></w:sdtContent></w:sdt></w:p>"#,
    );
    let target = opaque_text_targets(&base)
        .into_iter()
        .find(|t| t.kind == OpaqueTextTargetKind::InlineSdtText)
        .expect("inline sdt target");
    let step = EditStep::SdtTextFill {
        block_id: Some(target.host_block_id.clone()),
        sdt_id: Some(target.opaque_id.clone()),
        body_index: None,
        value: "Final".to_string(),
        semantic_hash: None,
        rationale: None,
    };
    let tracked =
        stemma::edit::apply_transaction(&base, &txn(step, MaterializationMode::TrackedChange))
            .expect("apply")
            .0;

    let read = |c: &CanonDoc| -> String {
        opaque_text_targets(c)
            .into_iter()
            .find(|t| t.kind == OpaqueTextTargetKind::InlineSdtText)
            .map(|t| t.text)
            .unwrap_or_default()
    };

    let mut accepted = tracked.clone();
    accept_all(&mut accepted);
    assert_eq!(read(&accepted), "Final");

    let mut rejected = tracked.clone();
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(read(&rejected), "Draft");
}

// ─── Block SDT (byte path) ───────────────────────────────────────────────────

fn block_sdt_body_index(canon: &CanonDoc) -> usize {
    for tb in &canon.blocks {
        if let BlockNode::OpaqueBlock(o) = &tb.block
            && matches!(o.kind, OpaqueKind::Sdt)
            && let Some(n) = o.proof_ref.docx_anchor.strip_prefix("body_index:")
        {
            return n.parse().unwrap();
        }
    }
    panic!("no block-level content control found");
}

fn block_sdt_docx() -> Vec<u8> {
    docx_bytes(
        r#"<w:sdt><w:sdtPr><w:alias w:val="Status"/></w:sdtPr><w:sdtContent><w:p><w:r><w:t>Draft</w:t></w:r></w:p></w:sdtContent></w:sdt>"#,
    )
}

fn fill_block(mode: MaterializationMode) -> Vec<u8> {
    let bytes = block_sdt_docx();
    let doc = Document::parse(&bytes).expect("parse");
    let body_index = block_sdt_body_index(&doc.snapshot().canonical);
    let step = EditStep::SdtTextFill {
        block_id: None,
        sdt_id: None,
        body_index: Some(body_index),
        value: "Final".to_string(),
        semantic_hash: None,
        rationale: None,
    };
    doc.apply(&txn(step, mode))
        .expect("apply")
        .serialize(&stemma::ExportOptions::default())
        .expect("serialize")
}

#[test]
fn block_sdt_fill_tracked_writes_redline() {
    let xml = document_xml_of(&fill_block(MaterializationMode::TrackedChange));
    // Old value struck through, new value inserted — a real redline inside the sdt.
    assert!(xml.contains("<w:del") && xml.contains("Draft"), "{xml}");
    assert!(xml.contains("<w:ins") && xml.contains("Final"), "{xml}");
}

#[test]
fn block_sdt_fill_direct_replaces_value() {
    let xml = document_xml_of(&fill_block(MaterializationMode::Direct));
    assert!(xml.contains("Final"), "{xml}");
    assert!(
        !xml.contains("Draft"),
        "direct fill leaves no old value: {xml}"
    );
    assert!(!xml.contains("<w:ins") && !xml.contains("<w:del"), "{xml}");
}

#[test]
fn block_sdt_fill_tracked_is_byte_reversible() {
    let filled = fill_block(MaterializationMode::TrackedChange);
    let archive = DocxArchive::read(&filled).unwrap();

    // reject-all (byte path) must restore the original value.
    let (rejected, _) = reject_all_docx(&archive).expect("reject");
    let rej_xml = String::from_utf8(rejected.get("word/document.xml").unwrap().to_vec()).unwrap();
    assert!(
        rej_xml.contains("Draft"),
        "reject restores old value: {rej_xml}"
    );
    assert!(
        !rej_xml.contains("Final"),
        "reject drops the inserted value: {rej_xml}"
    );
    assert!(
        !rej_xml.contains("<w:ins") && !rej_xml.contains("<w:del"),
        "{rej_xml}"
    );

    // accept-all (byte path) must keep the new value.
    let (accepted, _) = normalize_docx(&archive).expect("accept");
    let acc_xml = String::from_utf8(accepted.get("word/document.xml").unwrap().to_vec()).unwrap();
    assert!(
        acc_xml.contains("Final"),
        "accept keeps new value: {acc_xml}"
    );
    assert!(
        !acc_xml.contains("Draft"),
        "accept drops the deleted value: {acc_xml}"
    );
}

/// THE DOMAIN RULE: an applied fill is part of the document, full stop — it
/// must survive discovery reads and any number of subsequent edits. Block-SDT
/// bytes live in the serialize scaffold (`BodyTemplate.opaque_children`), so
/// the staged fill must be written back into the NEXT snapshot's template,
/// not just the serialized package: a stale template makes the next apply
/// re-stream the pre-fill bytes (silent data loss) and makes
/// `block_content_control_targets` read pre-fill text.
#[test]
fn block_sdt_fill_survives_discovery_read_and_subsequent_applies() {
    let bytes = docx_bytes(
        r#"<w:sdt><w:sdtPr><w:alias w:val="Status"/></w:sdtPr><w:sdtContent><w:p><w:r><w:t>Draft</w:t></w:r></w:p></w:sdtContent></w:sdt><w:sdt><w:sdtPr><w:alias w:val="Owner"/></w:sdtPr><w:sdtContent><w:p><w:r><w:t>Nobody</w:t></w:r></w:p></w:sdtContent></w:sdt>"#,
    );
    let doc = Document::parse(&bytes).expect("parse");
    let targets = doc.snapshot().block_content_control_targets();
    assert_eq!(targets.len(), 2, "two fillable block controls: {targets:?}");
    let (status_idx, owner_idx) = (targets[0].body_index, targets[1].body_index);

    let fill = |body_index: usize, value: &str| EditStep::SdtTextFill {
        block_id: None,
        sdt_id: None,
        body_index: Some(body_index),
        value: value.to_string(),
        semantic_hash: None,
        rationale: None,
    };

    // First apply: fill "Status".
    let after_first = doc
        .apply(&txn(fill(status_idx, "Final"), MaterializationMode::Direct))
        .expect("first fill");

    // Read-after-write: discovery must see the FILLED value immediately.
    let texts: Vec<String> = after_first
        .snapshot()
        .block_content_control_targets()
        .into_iter()
        .map(|t| t.text)
        .collect();
    assert_eq!(
        texts,
        vec!["Final".to_string(), "Nobody".to_string()],
        "post-fill discovery reads the filled value, not the import-time bytes"
    );

    // Second, unrelated apply: fill the OTHER control. The first fill must
    // survive — before the template write-back it was silently reverted here.
    let after_second = after_first
        .apply(&txn(
            fill(owner_idx, "Andreas"),
            MaterializationMode::Direct,
        ))
        .expect("second fill");
    let xml = document_xml_of(
        &after_second
            .serialize(&stemma::ExportOptions::default())
            .expect("serialize"),
    );
    assert!(
        xml.contains("Final"),
        "first fill survives the second apply: {xml}"
    );
    assert!(xml.contains("Andreas"), "second fill applied: {xml}");
    assert!(
        !xml.contains("Draft"),
        "no silent revert to pre-fill bytes: {xml}"
    );
    assert!(!xml.contains("Nobody"), "{xml}");
}

/// Same durability rule in TRACKED mode: the fill's redline (w:del of the old
/// value + w:ins of the new) must survive a subsequent apply, and reject-all
/// must still restore the ORIGINAL value afterwards.
#[test]
fn block_sdt_tracked_fill_survives_subsequent_apply_and_stays_reversible() {
    let bytes = docx_bytes(
        r#"<w:sdt><w:sdtPr><w:alias w:val="Status"/></w:sdtPr><w:sdtContent><w:p><w:r><w:t>Draft</w:t></w:r></w:p></w:sdtContent></w:sdt><w:sdt><w:sdtPr><w:alias w:val="Owner"/></w:sdtPr><w:sdtContent><w:p><w:r><w:t>Nobody</w:t></w:r></w:p></w:sdtContent></w:sdt>"#,
    );
    let doc = Document::parse(&bytes).expect("parse");
    let targets = doc.snapshot().block_content_control_targets();
    let (status_idx, owner_idx) = (targets[0].body_index, targets[1].body_index);

    let fill = |body_index: usize, value: &str| EditStep::SdtTextFill {
        block_id: None,
        sdt_id: None,
        body_index: Some(body_index),
        value: value.to_string(),
        semantic_hash: None,
        rationale: None,
    };

    let after = doc
        .apply(&txn(
            fill(status_idx, "Final"),
            MaterializationMode::TrackedChange,
        ))
        .expect("first fill")
        .apply(&txn(
            fill(owner_idx, "Andreas"),
            MaterializationMode::TrackedChange,
        ))
        .expect("second fill");
    let exported = after
        .serialize(&stemma::ExportOptions::default())
        .expect("serialize");
    let xml = document_xml_of(&exported);
    assert!(
        xml.contains("<w:ins")
            && xml.contains("Final")
            && xml.contains("<w:del")
            && xml.contains("Draft"),
        "first fill's redline survives the second apply: {xml}"
    );

    // Reject-all still reconstructs both original values.
    let archive = DocxArchive::read(&exported).unwrap();
    let (rejected, _) = reject_all_docx(&archive).expect("reject");
    let rej_xml = String::from_utf8(rejected.get("word/document.xml").unwrap().to_vec()).unwrap();
    assert!(
        rej_xml.contains("Draft") && rej_xml.contains("Nobody"),
        "reject-all restores both original values: {rej_xml}"
    );
    assert!(
        !rej_xml.contains("Final") && !rej_xml.contains("Andreas"),
        "reject-all drops both inserted values: {rej_xml}"
    );
}

/// A `semantic_hash` precondition on a BLOCK fill cannot be honored (block
/// bytes live in the scaffold; block discovery surfaces no hash) — it must
/// refuse loudly, never be silently dropped: the caller believes they hold a
/// stale-edit guard.
#[test]
fn block_sdt_fill_with_semantic_hash_refuses() {
    let bytes = block_sdt_docx();
    let doc = Document::parse(&bytes).expect("parse");
    let body_index = block_sdt_body_index(&doc.snapshot().canonical);
    let step = EditStep::SdtTextFill {
        block_id: None,
        sdt_id: None,
        body_index: Some(body_index),
        value: "Final".to_string(),
        semantic_hash: Some("deadbeef".to_string()),
        rationale: None,
    };
    let err = match doc.apply(&txn(step, MaterializationMode::Direct)) {
        Ok(_) => panic!("semantic_hash on a block fill must refuse"),
        Err(e) => e,
    };
    assert!(
        format!("{err}").contains("semantic_hash"),
        "refusal names the unsupported precondition: {err}"
    );
}

/// Two fills of the same block control in one transaction: refused at the
/// verb edge (step index known), not left to clobber or die at save time.
#[test]
fn duplicate_block_fill_in_one_transaction_refuses() {
    let bytes = block_sdt_docx();
    let doc = Document::parse(&bytes).expect("parse");
    let body_index = block_sdt_body_index(&doc.snapshot().canonical);
    let fill = |value: &str| EditStep::SdtTextFill {
        block_id: None,
        sdt_id: None,
        body_index: Some(body_index),
        value: value.to_string(),
        semantic_hash: None,
        rationale: None,
    };
    let two = EditTransaction {
        steps: vec![fill("First"), fill("Second")],
        summary: None,
        materialization_mode: MaterializationMode::Direct,
        revision: RevisionInfo {
            revision_id: 1,
            author: Some("Gate".to_string()),
            date: None,
            apply_op_id: None,
        },
    };
    let err = match doc.apply(&two) {
        Ok(_) => panic!("second fill of the same body_index must refuse"),
        Err(e) => e,
    };
    let msg = format!("{err}");
    assert!(
        msg.contains("already staged") && msg.contains("step 1"),
        "refusal names the duplicate and the offending step: {err}"
    );
}

/// Minted revision ids must clear the ids already living INSIDE block-opaque
/// bytes (invisible to the pure core's CanonDoc scan): a tracked fill next to
/// a block control carrying a pre-existing interior `w:ins w:id="800"` must
/// mint above 800, never a colliding id.
#[test]
fn block_fill_minted_ids_clear_preexisting_block_interior_ids() {
    let bytes = docx_bytes(
        r#"<w:sdt><w:sdtPr><w:alias w:val="History"/></w:sdtPr><w:sdtContent><w:p><w:ins w:id="800" w:author="Past"><w:r><w:t>old edit</w:t></w:r></w:ins></w:p></w:sdtContent></w:sdt><w:sdt><w:sdtPr><w:alias w:val="Status"/></w:sdtPr><w:sdtContent><w:p><w:r><w:t>Draft</w:t></w:r></w:p></w:sdtContent></w:sdt>"#,
    );
    let doc = Document::parse(&bytes).expect("parse");
    let targets = doc.snapshot().block_content_control_targets();
    let status = targets
        .iter()
        .find(|t| t.text == "Draft")
        .expect("clean control discoverable");
    let step = EditStep::SdtTextFill {
        block_id: None,
        sdt_id: None,
        body_index: Some(status.body_index),
        value: "Final".to_string(),
        semantic_hash: None,
        rationale: None,
    };
    let exported = doc
        .apply(&txn(step, MaterializationMode::TrackedChange))
        .expect("apply")
        .serialize(&stemma::ExportOptions::default())
        .expect("serialize");
    let xml = document_xml_of(&exported);

    // Collect every revision-carrier id; they must be unique document-wide.
    let mut ids: Vec<u32> = Vec::new();
    for carrier in ["<w:ins ", "<w:del "] {
        for chunk in xml.split(carrier).skip(1) {
            if let Some(id) = chunk
                .split("w:id=\"")
                .nth(1)
                .and_then(|s| s.split('"').next())
                .and_then(|s| s.parse::<u32>().ok())
            {
                ids.push(id);
            }
        }
    }
    assert!(
        ids.contains(&800),
        "pre-existing interior id survives: {ids:?}"
    );
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        ids.len(),
        "no two revision carriers may share an id: {ids:?}\n{xml}"
    );
    assert!(
        ids.iter().filter(|&&i| i != 800).all(|&i| i > 800),
        "minted ids clear the block-interior max: {ids:?}"
    );
}

#[test]
fn block_sdt_missing_index_refuses() {
    let bytes = block_sdt_docx();
    let doc = Document::parse(&bytes).expect("parse");
    let step = EditStep::SdtTextFill {
        block_id: None,
        sdt_id: None,
        body_index: Some(999),
        value: "X".to_string(),
        semantic_hash: None,
        rationale: None,
    };
    let err = match doc.apply(&txn(step, MaterializationMode::TrackedChange)) {
        Ok(_) => panic!("expected a block-not-found refusal"),
        Err(e) => e,
    };
    assert!(
        format!("{err}").to_lowercase().contains("content control")
            || format!("{err}").contains("999"),
        "expected a loud block-not-found refusal, got: {err}"
    );
}

#[test]
fn complex_content_control_is_not_fillable() {
    // A control whose value text hides inside a hyperlink is NOT cleanly
    // fillable — a whole-value set would relocate the hyperlink text (the wild
    // block-SDT reversibility bug the corpus surfaced). Discovery must not
    // advertise it, and a direct fill must refuse LOUD, not corrupt it.
    let base = import(
        r#"<w:p><w:sdt><w:sdtContent><w:r><w:t>See </w:t></w:r><w:hyperlink r:id="rId1"><w:r><w:t>WHO</w:t></w:r></w:hyperlink><w:r><w:t> report</w:t></w:r></w:sdtContent></w:sdt></w:p>"#,
    );
    // It IS surfaced as editable text (opaque_text_edit can reach it via hyperlink
    // descent) — but sdt_text_fill (whole-value SET) must still refuse it loud.
    assert!(
        opaque_text_targets(&base)
            .iter()
            .any(|t| t.kind == OpaqueTextTargetKind::InlineSdtText),
        "a hyperlink-bearing control's text is still discoverable for editing"
    );
    // The engine still finds the SDT node id via the view, but a fill refuses.
    // (Address it by scanning the block's inlines for the opaque id.)
    let sdt_id = {
        use stemma::domain::{BlockNode, InlineNode, OpaqueKind};
        let mut id = None;
        if let BlockNode::Paragraph(p) = &base.blocks[0].block {
            for seg in &p.segments {
                for inl in &seg.inlines {
                    if let InlineNode::OpaqueInline(o) = inl
                        && matches!(o.kind, OpaqueKind::Sdt)
                    {
                        id = Some(o.id.clone());
                    }
                }
            }
        }
        id.expect("sdt present")
    };
    let host = base.blocks[0].block.clone();
    let stemma::domain::BlockNode::Paragraph(p) = &host else {
        unreachable!()
    };
    let step = EditStep::SdtTextFill {
        block_id: Some(p.id.clone()),
        sdt_id: Some(sdt_id),
        body_index: None,
        value: "X".to_string(),
        semantic_hash: None,
        rationale: None,
    };
    let err =
        stemma::edit::apply_transaction(&base, &txn(step, MaterializationMode::TrackedChange))
            .unwrap_err();
    assert!(
        format!("{err}").contains("not a cleanly fillable value"),
        "expected a loud complex-content refusal, got: {err}"
    );
}

#[test]
fn ambiguous_target_refuses() {
    let base = import(r#"<w:p><w:r><w:t>hi</w:t></w:r></w:p>"#);
    let step = EditStep::SdtTextFill {
        block_id: None,
        sdt_id: None,
        body_index: None,
        value: "X".to_string(),
        semantic_hash: None,
        rationale: None,
    };
    let err =
        stemma::edit::apply_transaction(&base, &txn(step, MaterializationMode::TrackedChange))
            .unwrap_err();
    assert!(
        format!("{err}").contains("exactly one target"),
        "got: {err}"
    );
}
