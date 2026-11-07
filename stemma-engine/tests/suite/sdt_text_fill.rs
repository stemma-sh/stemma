//! `sdt_text_fill` — content-control value fills (RFC-0002 §Phase-2).
//!
//! INLINE controls carry their bytes on the IR node, so they resolve through the
//! IR accept/reject path (like `opaque_text_edit`). An OPEN block control (its
//! content is ordinary blocks) is model content: its fill is a text replace of
//! the value paragraph. A SEALED block control (here: a customXml element
//! directly inside its content) keeps its bytes in the serialize scaffold: the
//! fill is STAGED and applied at save time, and its reversal is the
//! whole-document BYTE path (`reject_all_docx`) — so these tests drive that
//! path end-to-end.
//!
//! Corpus-free: fixtures built in-process.

use stemma::docx::DocxArchive;
use stemma::domain::{BlockNode, OpaqueKind, RevisionInfo};
use stemma::edit::{EditStep, EditTransaction, MaterializationMode};
use stemma::normalize::{normalize_docx, reject_all_docx};
use stemma::opaque_targets::{BlockSdtAddress, OpaqueTextTargetKind, opaque_text_targets};
use stemma::{
    CanonDoc, DocxRuntime, SimpleRuntime, accept_all, api::Document, reject_all_with_styles,
};

fn docx_bytes(body_inner: &str) -> Vec<u8> {
    package(body_inner, None)
}

/// A package holding `body_inner` and, when given, a styles part.
fn package(body_inner: &str, styles: Option<&str>) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = match styles {
        None => r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#.to_string(),
        Some(_) => r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdStyles" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#.to_string(),
    };
    let content_types = match styles {
        None => content_types.to_string(),
        Some(_) => content_types.replace(
            "</Types>",
            r#"<Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/></Types>"#,
        ),
    };
    let styles_xml = styles.map(|styles| {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">{styles}</w:styles>"#
        )
    });
    use std::io::Write;
    use zip::write::FileOptions;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        let mut parts = vec![
            ("[Content_Types].xml", content_types.as_str()),
            ("_rels/.rels", rels),
            ("word/_rels/document.xml.rels", doc_rels.as_str()),
            ("word/document.xml", document_xml.as_str()),
        ];
        if let Some(styles_xml) = &styles_xml {
            parts.push(("word/styles.xml", styles_xml.as_str()));
        }
        for (name, data) in parts {
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
            identity: 0,
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

// ─── Sealed block SDT (byte path) ────────────────────────────────────────────

/// A block control holding one value paragraph, sealed by a customXml element
/// directly inside its content (no block model slot for it there).
fn sealed_sdt(alias: &str, paragraph: &str) -> String {
    format!(
        r#"<w:sdt><w:sdtPr><w:alias w:val="{alias}"/></w:sdtPr><w:sdtContent><w:customXml w:uri="urn:x" w:element="sealed">{paragraph}</w:customXml></w:sdtContent></w:sdt>"#
    )
}

fn sealed_index(target: &stemma::opaque_targets::BlockSdtTextTarget) -> usize {
    match target.address {
        BlockSdtAddress::BodyIndex(index) => index,
        BlockSdtAddress::FirstBlock(_) => panic!("expected a sealed control: {target:?}"),
    }
}

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
    docx_bytes(&sealed_sdt(
        "Status",
        r#"<w:p><w:r><w:t>Draft</w:t></w:r></w:p>"#,
    ))
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
    let bytes = docx_bytes(&format!(
        "{}{}",
        sealed_sdt("Status", r#"<w:p><w:r><w:t>Draft</w:t></w:r></w:p>"#),
        sealed_sdt("Owner", r#"<w:p><w:r><w:t>Nobody</w:t></w:r></w:p>"#)
    ));
    let doc = Document::parse(&bytes).expect("parse");
    let targets = doc.snapshot().block_content_control_targets();
    assert_eq!(targets.len(), 2, "two fillable block controls: {targets:?}");
    let (status_idx, owner_idx) = (sealed_index(&targets[0]), sealed_index(&targets[1]));

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
        .apply(&txn(fill(owner_idx, "Jordan"), MaterializationMode::Direct))
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
    assert!(xml.contains("Jordan"), "second fill applied: {xml}");
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
    let bytes = docx_bytes(&format!(
        "{}{}",
        sealed_sdt("Status", r#"<w:p><w:r><w:t>Draft</w:t></w:r></w:p>"#),
        sealed_sdt("Owner", r#"<w:p><w:r><w:t>Nobody</w:t></w:r></w:p>"#)
    ));
    let doc = Document::parse(&bytes).expect("parse");
    let targets = doc.snapshot().block_content_control_targets();
    let (status_idx, owner_idx) = (sealed_index(&targets[0]), sealed_index(&targets[1]));

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
            fill(owner_idx, "Jordan"),
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
        !rej_xml.contains("Final") && !rej_xml.contains("Jordan"),
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
            identity: 0,
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
    let bytes = docx_bytes(&format!(
        "{}{}",
        sealed_sdt(
            "History",
            r#"<w:p><w:ins w:id="800" w:author="Past"><w:r><w:t>old edit</w:t></w:r></w:ins></w:p>"#
        ),
        sealed_sdt("Status", r#"<w:p><w:r><w:t>Draft</w:t></w:r></w:p>"#)
    ));
    let doc = Document::parse(&bytes).expect("parse");
    let targets = doc.snapshot().block_content_control_targets();
    let status = targets
        .iter()
        .find(|t| t.text == "Draft")
        .expect("clean control discoverable");
    let step = EditStep::SdtTextFill {
        block_id: None,
        sdt_id: None,
        body_index: Some(sealed_index(status)),
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

// ─── Open block SDT (model path) ─────────────────────────────────────────────

/// Two open block controls: a formatted "Status" value and an "Owner" value.
fn open_controls_docx() -> Vec<u8> {
    docx_bytes(
        r#"<w:sdt><w:sdtPr><w:alias w:val="Status"/><w:tag w:val="status"/></w:sdtPr><w:sdtContent><w:p><w:r><w:rPr><w:b/></w:rPr><w:t>Draft</w:t></w:r></w:p></w:sdtContent></w:sdt><w:sdt><w:sdtPr><w:alias w:val="Owner"/></w:sdtPr><w:sdtContent><w:p><w:r><w:t>Nobody</w:t></w:r></w:p></w:sdtContent></w:sdt>"#,
    )
}

fn open_fill(block_id: &stemma::domain::NodeId, value: &str, hash: Option<&str>) -> EditStep {
    EditStep::SdtTextFill {
        block_id: Some(block_id.clone()),
        sdt_id: None,
        body_index: None,
        value: value.to_string(),
        semantic_hash: hash.map(str::to_string),
        rationale: None,
    }
}

fn first_paragraph_id(canonical: &CanonDoc) -> stemma::domain::NodeId {
    let BlockNode::Paragraph(paragraph) = &canonical.blocks[0].block else {
        panic!("the body opens with the control's paragraph")
    };
    paragraph.id.clone()
}

fn first_block(target: &stemma::opaque_targets::BlockSdtTextTarget) -> stemma::domain::NodeId {
    match &target.address {
        BlockSdtAddress::FirstBlock(id) => id.clone(),
        BlockSdtAddress::BodyIndex(_) => panic!("expected an open control: {target:?}"),
    }
}

#[test]
fn an_open_block_control_is_discovered_by_its_first_block() {
    let doc = Document::parse(&open_controls_docx()).expect("parse");
    let targets = doc.snapshot().block_content_control_targets();
    let found: Vec<_> = targets
        .iter()
        .map(|t| (t.alias.clone(), t.tag.clone(), t.text.clone()))
        .collect();
    assert_eq!(
        found,
        vec![
            (Some("Status".into()), Some("status".into()), "Draft".into()),
            (Some("Owner".into()), None, "Nobody".into()),
        ]
    );
    // The address is the control's first block, an ordinary paragraph.
    let canonical = &doc.snapshot().canonical;
    assert_eq!(first_block(&targets[0]), first_paragraph_id(canonical));
}

#[test]
fn an_open_block_fill_is_a_reversible_redline_inside_the_control() {
    let doc = Document::parse(&open_controls_docx()).expect("parse");
    let status = first_block(&doc.snapshot().block_content_control_targets()[0]);
    let filled = doc
        .apply(&txn(
            open_fill(&status, "Final", None),
            MaterializationMode::TrackedChange,
        ))
        .expect("fill");
    let bytes = filled
        .serialize(&stemma::ExportOptions::default())
        .expect("serialize");
    let xml = document_xml_of(&bytes);
    // The control's wrapper is kept; the redline is inside its content.
    let content = xml
        .split("<w:sdtContent>")
        .nth(1)
        .and_then(|rest| rest.split("</w:sdtContent>").next())
        .expect("the control is kept");
    assert!(
        content.contains("<w:del") && content.contains("Draft"),
        "{xml}"
    );
    assert!(
        content.contains("<w:ins") && content.contains("Final"),
        "{xml}"
    );
    assert!(xml.contains(r#"<w:alias w:val="Status""#), "{xml}");

    let archive = DocxArchive::read(&bytes).unwrap();
    let (rejected, _) = reject_all_docx(&archive).expect("reject");
    let rejected = String::from_utf8(rejected.get("word/document.xml").unwrap().to_vec()).unwrap();
    assert!(rejected.contains("Draft") && !rejected.contains("Final"));
    let (accepted, _) = normalize_docx(&archive).expect("accept");
    let accepted = String::from_utf8(accepted.get("word/document.xml").unwrap().to_vec()).unwrap();
    assert!(accepted.contains("Final") && !accepted.contains("Draft"));

    // The value keeps the run formatting it replaced.
    let reopened = Document::parse(&bytes).expect("reopen");
    let accepted = reopened
        .project(stemma::Resolution::AcceptAll)
        .expect("accept");
    let targets = accepted.snapshot().block_content_control_targets();
    assert_eq!(targets[0].text, "Final");
    let BlockNode::Paragraph(value) = &accepted.snapshot().canonical.blocks[0].block else {
        panic!("the value paragraph")
    };
    assert!(value.all_inlines().any(|inline| matches!(inline,
        stemma::domain::InlineNode::Text(text)
            if text.text == "Final" && text.marks.contains(&stemma::Mark::Bold))));
}

#[test]
fn a_direct_open_block_fill_replaces_the_value() {
    let doc = Document::parse(&open_controls_docx()).expect("parse");
    let owner = first_block(&doc.snapshot().block_content_control_targets()[1]);
    let filled = doc
        .apply(&txn(
            open_fill(&owner, "Jordan", None),
            MaterializationMode::Direct,
        ))
        .expect("fill");
    let texts: Vec<String> = filled
        .snapshot()
        .block_content_control_targets()
        .into_iter()
        .map(|t| t.text)
        .collect();
    assert_eq!(texts, vec!["Draft".to_string(), "Jordan".to_string()]);
    let xml = document_xml_of(
        &filled
            .serialize(&stemma::ExportOptions::default())
            .expect("serialize"),
    );
    assert!(!xml.contains("<w:ins") && !xml.contains("<w:del"), "{xml}");
}

#[test]
fn an_open_block_fill_with_a_stale_hash_refuses() {
    let doc = Document::parse(&open_controls_docx()).expect("parse");
    let status = first_block(&doc.snapshot().block_content_control_targets()[0]);
    assert!(
        doc.apply(&txn(
            open_fill(&status, "Final", Some("stale")),
            MaterializationMode::TrackedChange,
        ))
        .is_err(),
        "a semantic_hash is the value paragraph's guard"
    );
    let view = doc.read();
    let current = view.blocks[0].guard.clone();
    doc.apply(&txn(
        open_fill(&status, "Final", Some(&current)),
        MaterializationMode::TrackedChange,
    ))
    .expect("the current guard is accepted");
}

#[test]
fn an_open_block_control_whose_value_holds_a_link_is_not_fillable() {
    let doc = Document::parse(&docx_bytes(
        r#"<w:sdt><w:sdtPr><w:alias w:val="Source"/></w:sdtPr><w:sdtContent><w:p><w:r><w:t xml:space="preserve">See </w:t></w:r><w:hyperlink w:anchor="top"><w:r><w:t>top</w:t></w:r></w:hyperlink></w:p></w:sdtContent></w:sdt>"#,
    ))
    .expect("parse");
    assert!(doc.snapshot().block_content_control_targets().is_empty());
    let first = first_paragraph_id(&doc.snapshot().canonical);
    let err = match doc.apply(&txn(
        open_fill(&first, "X", None),
        MaterializationMode::TrackedChange,
    )) {
        Ok(_) => panic!("a value paragraph with a link must refuse"),
        Err(err) => err,
    };
    assert!(format!("{err}").contains("hyperlink"), "{err}");
}

#[test]
fn filling_an_open_placeholder_control_clears_its_placeholder_flag() {
    for mode in [
        MaterializationMode::TrackedChange,
        MaterializationMode::Direct,
    ] {
        let doc = Document::parse(&docx_bytes(
            r#"<w:sdt><w:sdtPr><w:alias w:val="Name"/><w:showingPlcHdr/></w:sdtPr><w:sdtContent><w:p><w:r><w:rPr><w:rStyle w:val="PlaceholderText"/></w:rPr><w:t>Click here to enter text.</w:t></w:r></w:p></w:sdtContent></w:sdt>"#,
        ))
        .expect("parse");
        let name = first_block(&doc.snapshot().block_content_control_targets()[0]);
        let filled = doc
            .apply(&txn(open_fill(&name, "Jane Doe", None), mode))
            .expect("fill");
        let xml = document_xml_of(
            &filled
                .serialize(&stemma::ExportOptions::default())
                .expect("serialize"),
        );
        assert!(!xml.contains("showingPlcHdr"), "{mode:?}: {xml}");
        assert!(xml.contains(r#"<w:alias w:val="Name""#), "{mode:?}: {xml}");
    }
}

#[test]
fn an_open_block_control_with_more_than_one_paragraph_of_text_is_not_fillable() {
    let doc = Document::parse(&docx_bytes(
        r#"<w:sdt><w:sdtPr><w:alias w:val="Clause"/></w:sdtPr><w:sdtContent><w:p><w:r><w:t>First</w:t></w:r></w:p><w:p><w:r><w:t>Second</w:t></w:r></w:p></w:sdtContent></w:sdt>"#,
    ))
    .expect("parse");
    assert!(doc.snapshot().block_content_control_targets().is_empty());
    let first = first_paragraph_id(&doc.snapshot().canonical);
    let err = match doc.apply(&txn(
        open_fill(&first, "X", None),
        MaterializationMode::TrackedChange,
    )) {
        Ok(_) => panic!("a control with two paragraphs of text must refuse"),
        Err(err) => err,
    };
    assert!(
        format!("{err}").contains("edit its paragraphs directly"),
        "{err}"
    );
}

// ─── Filling a placeholder (§17.5.2.27) ─────────────────────────────────────
//
// Text entered in place of a control's placeholder takes the run properties the
// control keeps for entered text (`sdtPr/w:rPr`); without them it is
// unformatted, and it never inherits the placeholder's formatting. Filled
// controls Word saved carry no placeholder style on their value.

const PLACEHOLDER_STYLES: &str = r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style><w:style w:type="character" w:default="1" w:styleId="DefaultParagraphFont"><w:name w:val="Default Paragraph Font"/></w:style><w:style w:type="character" w:styleId="Platzhaltertext"><w:name w:val="Placeholder Text"/><w:basedOn w:val="DefaultParagraphFont"/><w:rPr><w:color w:val="808080"/></w:rPr></w:style>"#;

/// `docx_bytes` with a styles part holding `styles`.
fn docx_with_styles(body_inner: &str, styles: &str) -> Vec<u8> {
    package(body_inner, Some(styles))
}

/// A control showing its placeholder in the (localized) placeholder style,
/// keeping `entered` as the run properties for entered text.
fn placeholder_sdt(entered: &str, content: &str) -> String {
    format!(
        r#"<w:sdt><w:sdtPr>{entered}<w:alias w:val="Name"/><w:showingPlcHdr/></w:sdtPr><w:sdtContent>{content}</w:sdtContent></w:sdt>"#
    )
}

const PLACEHOLDER_RUN: &str = r#"<w:r><w:rPr><w:rStyle w:val="Platzhaltertext"/></w:rPr><w:t>Click here to enter text.</w:t></w:r>"#;
const ENTERED_RPR: &str = r#"<w:rPr><w:b/><w:color w:val="1F3864"/></w:rPr>"#;

/// The serialized run holding `text`, with its rPr.
fn run_holding(xml: &str, text: &str) -> String {
    let at = xml
        .find(&format!(">{text}<"))
        .unwrap_or_else(|| panic!("no {text:?} in {xml}"));
    let start = xml[..at]
        .rfind("<w:r>")
        .or_else(|| xml[..at].rfind("<w:r "));
    let start = start.unwrap_or_else(|| panic!("no run around {text:?} in {xml}"));
    let end = at + xml[at..].find("</w:r>").expect("run end") + "</w:r>".len();
    xml[start..end].to_string()
}

fn reopens_as_saved(edited: &Document, label: &str) {
    let reopened = Document::parse(
        &edited
            .serialize(&stemma::ExportOptions::default())
            .expect("serialize"),
    )
    .expect("reopen");
    let differences = stemma::roundtrip_compare::compare_canon_docs(
        &edited.snapshot().canonical,
        &reopened.snapshot().canonical,
    );
    assert!(differences.is_empty(), "{label}: {differences:?}");
}

#[test]
fn a_filled_open_placeholder_takes_the_controls_entered_text_formatting() {
    for (entered, expected) in [
        (ENTERED_RPR, Some(["<w:b", r#"w:val="1F3864""#])),
        ("", None),
    ] {
        for mode in [
            MaterializationMode::TrackedChange,
            MaterializationMode::Direct,
        ] {
            let label = format!("{mode:?} entered={entered:?}");
            let doc = Document::parse(&docx_with_styles(
                &placeholder_sdt(entered, &format!("<w:p>{PLACEHOLDER_RUN}</w:p>")),
                PLACEHOLDER_STYLES,
            ))
            .expect("parse");
            let name = first_block(&doc.snapshot().block_content_control_targets()[0]);
            let filled = doc
                .apply(&txn(open_fill(&name, "Jane Doe", None), mode))
                .expect("fill");
            reopens_as_saved(&filled, &label);
            let xml = document_xml_of(
                &filled
                    .serialize(&stemma::ExportOptions::default())
                    .expect("serialize"),
            );
            let value = run_holding(&xml, "Jane Doe");
            assert!(!value.contains("Platzhaltertext"), "{label}: {value}");
            match expected {
                Some(properties) => {
                    for property in properties {
                        assert!(value.contains(property), "{label}: {value}");
                    }
                }
                None => assert!(!value.contains("<w:rPr"), "{label}: {value}"),
            }
            if mode == MaterializationMode::TrackedChange {
                // Reject restores the placeholder as it was.
                let rejected = document_xml_of(
                    &filled
                        .project(stemma::Resolution::RejectAll)
                        .expect("reject")
                        .serialize(&stemma::ExportOptions::default())
                        .expect("serialize"),
                );
                assert!(
                    run_holding(&rejected, "Click here to enter text.").contains("Platzhaltertext"),
                    "{label}: {rejected}"
                );
            }
        }
    }
}

#[test]
fn a_control_showing_its_value_keeps_the_values_formatting() {
    // `w:showingPlcHdr` is a toggle: turned off, the content is the value.
    let doc = Document::parse(&docx_with_styles(
        r#"<w:sdt><w:sdtPr><w:rPr><w:color w:val="1F3864"/></w:rPr><w:alias w:val="Name"/><w:showingPlcHdr w:val="0"/></w:sdtPr><w:sdtContent><w:p><w:r><w:rPr><w:i/></w:rPr><w:t>Ada</w:t></w:r></w:p></w:sdtContent></w:sdt>"#,
        PLACEHOLDER_STYLES,
    ))
    .expect("parse");
    let name = first_block(&doc.snapshot().block_content_control_targets()[0]);
    let filled = doc
        .apply(&txn(
            open_fill(&name, "Grace", None),
            MaterializationMode::Direct,
        ))
        .expect("fill");
    reopens_as_saved(&filled, "value");
    let xml = document_xml_of(
        &filled
            .serialize(&stemma::ExportOptions::default())
            .expect("serialize"),
    );
    let value = run_holding(&xml, "Grace");
    assert!(value.contains("<w:i"), "{value}");
    assert!(!value.contains("1F3864"), "{value}");
}

#[test]
fn a_filled_inline_placeholder_leaves_the_placeholder_state() {
    for mode in [
        MaterializationMode::TrackedChange,
        MaterializationMode::Direct,
    ] {
        let doc = Document::parse(&docx_with_styles(
            &format!(
                "<w:p><w:r><w:t xml:space=\"preserve\">Name: </w:t></w:r>{}</w:p>",
                placeholder_sdt(ENTERED_RPR, PLACEHOLDER_RUN)
            ),
            PLACEHOLDER_STYLES,
        ))
        .expect("parse");
        let target = opaque_text_targets(&doc.snapshot().canonical)
            .into_iter()
            .find(|t| t.kind == OpaqueTextTargetKind::InlineSdtText)
            .expect("inline sdt target");
        let filled = doc
            .apply(&txn(
                EditStep::SdtTextFill {
                    block_id: Some(target.host_block_id.clone()),
                    sdt_id: Some(target.opaque_id.clone()),
                    body_index: None,
                    value: "Jane Doe".to_string(),
                    semantic_hash: None,
                    rationale: None,
                },
                mode,
            ))
            .expect("fill");
        let xml = document_xml_of(
            &filled
                .serialize(&stemma::ExportOptions::default())
                .expect("serialize"),
        );
        assert!(!xml.contains("showingPlcHdr"), "{mode:?}: {xml}");
        let value = run_holding(&xml, "Jane Doe");
        assert!(!value.contains("Platzhaltertext"), "{mode:?}: {value}");
        assert!(
            value.contains("<w:b") && value.contains("1F3864"),
            "{mode:?}: {value}"
        );
    }
}

#[test]
fn a_filled_sealed_placeholder_leaves_the_placeholder_state() {
    for mode in [
        MaterializationMode::TrackedChange,
        MaterializationMode::Direct,
    ] {
        let sealed = placeholder_sdt(
            ENTERED_RPR,
            &format!(
                r#"<w:customXml w:uri="urn:x" w:element="sealed"><w:p>{PLACEHOLDER_RUN}</w:p></w:customXml>"#
            ),
        );
        let doc = Document::parse(&docx_with_styles(&sealed, PLACEHOLDER_STYLES)).expect("parse");
        let body_index = block_sdt_body_index(&doc.snapshot().canonical);
        let filled = doc
            .apply(&txn(
                EditStep::SdtTextFill {
                    block_id: None,
                    sdt_id: None,
                    body_index: Some(body_index),
                    value: "Jane Doe".to_string(),
                    semantic_hash: None,
                    rationale: None,
                },
                mode,
            ))
            .expect("fill");
        let xml = document_xml_of(
            &filled
                .serialize(&stemma::ExportOptions::default())
                .expect("serialize"),
        );
        assert!(!xml.contains("showingPlcHdr"), "{mode:?}: {xml}");
        let value = run_holding(&xml, "Jane Doe");
        assert!(!value.contains("Platzhaltertext"), "{mode:?}: {value}");
        assert!(
            value.contains("<w:b") && value.contains("1F3864"),
            "{mode:?}: {value}"
        );
    }
}

// ─── Revisions inside a sealed control are pending state ────────────────────
//
// A tracked change is pending until accepted or rejected, wherever it sits
// (ISO 29500 §17.13.5). A sealed control keeps its bytes outside the block
// model, but its revisions are still the document's: the census must report
// them, and accept-all/reject-all must resolve them exactly as Word does.

fn sealed_xml_after(doc: &Document, resolution: stemma::api::Resolution) -> String {
    document_xml_of(
        &doc.project(resolution)
            .expect("project")
            .serialize(&stemma::ExportOptions::default())
            .expect("serialize"),
    )
}

fn assert_resolved(xml: &str, kept: &str, gone: &str, label: &str) {
    assert!(xml.contains(kept), "{label}: keeps {kept:?}: {xml}");
    assert!(!xml.contains(gone), "{label}: drops {gone:?}: {xml}");
    assert!(
        !xml.contains("<w:ins") && !xml.contains("<w:del"),
        "{label}: nothing pending: {xml}"
    );
}

#[test]
fn a_tracked_sealed_fill_is_reported_and_resolved_by_the_projections() {
    let doc = Document::parse(&block_sdt_docx()).expect("parse");
    let body_index = block_sdt_body_index(&doc.snapshot().canonical);
    let filled = doc
        .apply(&txn(
            EditStep::SdtTextFill {
                block_id: None,
                sdt_id: None,
                body_index: Some(body_index),
                value: "Final".to_string(),
                semantic_hash: None,
                rationale: None,
            },
            MaterializationMode::TrackedChange,
        ))
        .expect("fill");
    let reopened = Document::parse(
        &filled
            .serialize(&stemma::ExportOptions::default())
            .expect("serialize"),
    )
    .expect("reopen");
    for (label, doc) in [("in session", &filled), ("reopened", &reopened)] {
        // The fill is one deletion and one insertion.
        assert_eq!(doc.revisions().len(), 2, "{label}: {:?}", doc.revisions());
        let accepted = sealed_xml_after(doc, stemma::api::Resolution::AcceptAll);
        assert_resolved(&accepted, "Final", "Draft", &format!("{label} accept"));
        let rejected = sealed_xml_after(doc, stemma::api::Resolution::RejectAll);
        assert_resolved(&rejected, "Draft", "Final", &format!("{label} reject"));
        for resolution in [
            stemma::api::Resolution::AcceptAll,
            stemma::api::Resolution::RejectAll,
        ] {
            let projected = doc.project(resolution).expect("project");
            assert!(
                projected.revisions().is_empty(),
                "{label}: {:?}",
                projected.revisions()
            );
        }
    }
}

#[test]
fn an_imported_revision_inside_a_sealed_control_is_reported_and_resolved() {
    let doc = Document::parse(&docx_bytes(&sealed_sdt(
        "Status",
        r#"<w:p><w:r><w:t>Kept </w:t></w:r><w:ins w:id="5" w:author="A"><w:r><w:t>Added</w:t></w:r></w:ins></w:p>"#,
    )))
    .expect("parse");
    let records = doc.revisions();
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].author.as_deref(), Some("A"), "{records:?}");
    let accepted = sealed_xml_after(&doc, stemma::api::Resolution::AcceptAll);
    assert_resolved(&accepted, "Added", "<w:ins", "accept");
    let rejected = sealed_xml_after(&doc, stemma::api::Resolution::RejectAll);
    assert_resolved(&rejected, "Kept", "Added", "reject");
}

#[test]
fn a_selective_resolution_leaves_a_sealed_controls_revision_pending() {
    let doc = Document::parse(&docx_bytes(&format!(
        r#"<w:p><w:ins w:id="7" w:author="B"><w:r><w:t>Body</w:t></w:r></w:ins></w:p>{}"#,
        sealed_sdt(
            "Status",
            r#"<w:p><w:ins w:id="5" w:author="A"><w:r><w:t>Added</w:t></w:r></w:ins></w:p>"#,
        )
    )))
    .expect("parse");
    let records = doc.revisions();
    let body: Vec<u32> = records
        .iter()
        .filter(|r| r.author.as_deref() == Some("B"))
        .map(|r| r.revision_id)
        .collect();
    assert_eq!(body.len(), 1, "{records:?}");
    let projected = doc
        .project(stemma::api::Resolution::Selective {
            ids: body.into_iter().collect(),
            action: stemma::ResolveSelectionAction::Accept,
        })
        .expect("project");
    // Selection cannot address the control's interior, so it stays pending
    // and stays reported.
    let left = projected.revisions();
    assert_eq!(left.len(), 1, "{left:?}");
    assert_eq!(left[0].author.as_deref(), Some("A"), "{left:?}");
    let xml = document_xml_of(
        &projected
            .serialize(&stemma::ExportOptions::default())
            .expect("serialize"),
    );
    assert!(xml.contains(r#"w:author="A""#), "{xml}");
}

// ─── Review sees inside a sealed control ────────────────────────────────────
//
// A review certifies what changed since the document was opened. A sealed
// control's content is part of the document, so a tracked fill must be in the
// census, and a direct fill must not pass as untouched.

fn sealed_fill(mode: MaterializationMode) -> (Document, Document) {
    let doc = Document::parse(&block_sdt_docx()).expect("parse");
    let body_index = block_sdt_body_index(&doc.snapshot().canonical);
    let filled = doc
        .apply(&txn(
            EditStep::SdtTextFill {
                block_id: None,
                sdt_id: None,
                body_index: Some(body_index),
                value: "Final".to_string(),
                semantic_hash: None,
                rationale: None,
            },
            mode,
        ))
        .expect("fill");
    (doc, filled)
}

#[test]
fn a_review_reports_a_tracked_sealed_fill_as_new_revisions() {
    let (source, filled) = sealed_fill(MaterializationMode::TrackedChange);
    let bytes = |doc: &Document| {
        doc.serialize(&stemma::ExportOptions::default())
            .expect("serialize")
    };
    for (label, report) in [
        ("session", filled.review().expect("review")),
        (
            "stateless",
            stemma::api::audit(&bytes(&source), &bytes(&filled)).expect("audit"),
        ),
    ] {
        // The fill is one deletion and one insertion.
        assert_eq!(report.new_revisions.len(), 2, "{label}: {report:?}");
        assert!(report.direct_changes.is_empty(), "{label}: {report:?}");
        assert!(
            report.untouched.violations.is_empty(),
            "{label}: {report:?}"
        );
    }
}

#[test]
fn a_review_does_not_certify_a_directly_filled_sealed_control_as_untouched() {
    let (_, filled) = sealed_fill(MaterializationMode::Direct);
    let report = filled.review().expect("review");
    assert!(report.new_revisions.is_empty(), "{report:?}");
    assert_eq!(report.untouched.violations.len(), 1, "{report:?}");
}

#[test]
fn a_review_of_an_unchanged_sealed_control_verifies_it() {
    let doc = Document::parse(&block_sdt_docx()).expect("parse");
    let report = doc.review().expect("review");
    assert!(report.untouched.violations.is_empty(), "{report:?}");
    assert_eq!(report.untouched.verified_blocks, 1, "{report:?}");
}
