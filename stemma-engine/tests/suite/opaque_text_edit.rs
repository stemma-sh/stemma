//! `opaque_text_edit` — interior-aware fidelity (RFC-0002 §Phase-1).
//!
//! The standard per-verb fidelity gate fingerprints the DocumentView, which
//! surfaces an opaque as a single anchor and NOT its interior text — too coarse
//! to prove a surgical splice inside a textbox reversed correctly. These tests
//! drive the real accept/reject IR path and assert at the INTERIOR-TEXT level:
//!
//! 1. Reversibility — reject-all restores the original interior text.
//! 2. Accept == direct — accept-all equals a direct-mode apply.
//! 3. The edit is a real tracked change (accept and reject diverge).
//!
//! Corpus-free: the textbox/SDT fixtures are built in-process.

use stemma::domain::{NodeId, RevisionInfo};
use stemma::edit::{EditStep, EditTransaction, MaterializationMode, apply_transaction};
use stemma::opaque_targets::{OpaqueTextTarget, OpaqueTextTargetKind, opaque_text_targets};
use stemma::{
    CanonDoc, DocxRuntime, Resolution, SimpleRuntime, accept_all,
    api::Document,
    enumerate_revisions, reject_all_with_styles,
    tracked_model::{ResolveSelectionAction, RevisionKind},
};

fn docx_bytes(body_inner: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
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

fn textbox_docx(paras: &[&str]) -> CanonDoc {
    let inner: String = paras
        .iter()
        .map(|t| format!(r#"<w:p><w:r><w:t>{t}</w:t></w:r></w:p>"#))
        .collect();
    import(&format!(
        r#"<w:p><w:r><w:drawing><wp:inline><wp:extent cx="100" cy="100"/><wp:docPr id="1" name="TextBox 1"/><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:txbx><w:txbxContent>{inner}</w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#
    ))
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

fn only_target(canon: &CanonDoc, kind: OpaqueTextTargetKind) -> OpaqueTextTarget {
    let mut t: Vec<_> = opaque_text_targets(canon)
        .into_iter()
        .filter(|t| t.kind == kind)
        .collect();
    assert_eq!(t.len(), 1, "expected exactly one target of {kind:?}");
    t.pop().unwrap()
}

/// The visible interior text of the sole target of `kind`, after re-discovery.
fn interior_text(canon: &CanonDoc, kind: OpaqueTextTargetKind) -> String {
    opaque_text_targets(canon)
        .into_iter()
        .filter(|t| t.kind == kind)
        .map(|t| t.text)
        .collect::<Vec<_>>()
        .join("\n")
}

fn edit_step(t: &OpaqueTextTarget, find: &str, replacement: &str) -> EditStep {
    EditStep::OpaqueTextEdit {
        block_id: t.host_block_id.clone(),
        opaque_id: t.opaque_id.clone(),
        container_index: t.address.container_index,
        paragraph_index: t.address.paragraph_index,
        find: find.to_string(),
        replacement: replacement.to_string(),
        semantic_hash: None,
        rationale: None,
    }
}

/// Run the three interior-aware invariants for one splice.
fn assert_interior_fidelity(
    base: &CanonDoc,
    kind: OpaqueTextTargetKind,
    find: &str,
    replacement: &str,
    expect_after: &str,
) {
    let target = only_target(base, kind);
    let original = interior_text(base, kind);
    let step = edit_step(&target, find, replacement);

    let tracked = apply_transaction(base, &txn(step.clone(), MaterializationMode::TrackedChange))
        .expect("tracked apply")
        .0;

    // 1. Reversibility.
    let mut rejected = tracked.clone();
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        interior_text(&rejected, kind),
        original,
        "reject-all must restore the original interior text"
    );

    // 2. Accept == direct.
    let mut accepted = tracked.clone();
    accept_all(&mut accepted);
    let direct = apply_transaction(base, &txn(step, MaterializationMode::Direct))
        .expect("direct apply")
        .0;
    assert_eq!(
        interior_text(&accepted, kind),
        interior_text(&direct, kind),
        "accept-all must equal direct apply"
    );
    assert_eq!(
        interior_text(&accepted, kind),
        expect_after,
        "accepted interior text must be the replacement result"
    );

    // 3. It was a real tracked change: accept and reject diverge.
    assert_ne!(
        interior_text(&accepted, kind),
        interior_text(&rejected, kind),
        "a tracked edit must make accept and reject differ"
    );
}

#[test]
fn textbox_partial_edit_is_reversible() {
    let base = textbox_docx(&["The quick brown fox"]);
    assert_interior_fidelity(
        &base,
        OpaqueTextTargetKind::TextboxParagraph,
        "quick",
        "slow",
        "The slow brown fox",
    );
}

#[test]
fn textbox_second_paragraph_addressed_precisely() {
    let base = textbox_docx(&["First line", "Second line"]);
    // Address the SECOND paragraph; the first must be untouched.
    let target = opaque_text_targets(&base)
        .into_iter()
        .find(|t| t.address.paragraph_index == 1)
        .expect("second textbox paragraph");
    let step = edit_step(&target, "Second", "Third");
    let mut accepted = apply_transaction(&base, &txn(step, MaterializationMode::TrackedChange))
        .expect("apply")
        .0;
    accept_all(&mut accepted);
    assert_eq!(
        interior_text(&accepted, OpaqueTextTargetKind::TextboxParagraph),
        "First line\nThird line"
    );
}

#[test]
fn textbox_hyperlink_interior_edit_is_reversible() {
    // A textbox whose interior paragraph has a hyperlinked word. Editing inside the
    // hyperlink (Word edits links freely) must reverse cleanly through the real
    // accept/reject path — the tracked change sits inside the w:hyperlink.
    let inner = r#"<w:p><w:r><w:t xml:space="preserve">See </w:t></w:r><w:hyperlink r:id="rId9"><w:r><w:t>the report</w:t></w:r></w:hyperlink></w:p>"#;
    let base = import(&format!(
        r#"<w:p><w:r><w:drawing><wp:inline><wp:extent cx="100" cy="100"/><wp:docPr id="1" name="TextBox 1"/><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:txbx><w:txbxContent>{inner}</w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#
    ));
    let target = opaque_text_targets(&base)
        .into_iter()
        .find(|t| t.kind == OpaqueTextTargetKind::TextboxParagraph)
        .expect("textbox target");
    // Discovery reports the full visible text incl. the hyperlink word.
    assert_eq!(target.text, "See the report");
    // Edit the hyperlinked word specifically.
    let step = edit_step(&target, "the report", "the summary");
    let tracked = apply_transaction(&base, &txn(step, MaterializationMode::TrackedChange))
        .expect("apply")
        .0;

    let read = |c: &CanonDoc| interior_text(c, OpaqueTextTargetKind::TextboxParagraph);
    let mut accepted = tracked.clone();
    accept_all(&mut accepted);
    assert_eq!(read(&accepted), "See the summary");
    let mut rejected = tracked.clone();
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(read(&rejected), "See the report");
}

/// THE DOMAIN RULE: "replace the first occurrence" means first in the
/// DOCUMENT-ORDER visible text — the text discovery reports — not
/// direct-runs-first. When the find-string occurs both inside a doc-earlier
/// hyperlink and in a doc-later direct run, the hyperlinked occurrence is the
/// one edited; before this rule the direct run silently won, editing a
/// different occurrence than the agent addressed.
#[test]
fn first_occurrence_is_document_order_across_wrappers() {
    let inner = r#"<w:p><w:hyperlink r:id="rId9"><w:r><w:t>alpha</w:t></w:r></w:hyperlink><w:r><w:t xml:space="preserve"> and alpha</w:t></w:r></w:p>"#;
    let base = import(&format!(
        r#"<w:p><w:r><w:drawing><wp:inline><wp:extent cx="100" cy="100"/><wp:docPr id="1" name="TextBox 1"/><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:txbx><w:txbxContent>{inner}</w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#
    ));
    let target = only_target(&base, OpaqueTextTargetKind::TextboxParagraph);
    assert_eq!(target.text, "alpha and alpha", "doc-order discovery text");

    let step = edit_step(&target, "alpha", "beta");
    let direct = apply_transaction(&base, &txn(step, MaterializationMode::Direct))
        .expect("apply")
        .0;
    assert_eq!(
        interior_text(&direct, OpaqueTextTargetKind::TextboxParagraph),
        "beta and alpha",
        "the doc-order-FIRST (hyperlinked) occurrence is the one replaced"
    );
    // And the replacement landed INSIDE the hyperlink wrapper, not beside it.
    let target_after = only_target(&direct, OpaqueTextTargetKind::TextboxParagraph);
    let raw = raw_xml_of_target(&direct, &target_after);
    let link = raw
        .split("<w:hyperlink")
        .nth(1)
        .and_then(|s| s.split("</w:hyperlink>").next())
        .expect("hyperlink survives");
    assert!(
        link.contains("beta"),
        "replacement stays inside the wrapper: {raw}"
    );
    assert!(!link.contains("alpha"), "{raw}");
}

fn raw_xml_of_target(canon: &CanonDoc, target: &OpaqueTextTarget) -> String {
    for tb in &canon.blocks {
        let stemma::domain::BlockNode::Paragraph(p) = &tb.block else {
            continue;
        };
        for seg in &p.segments {
            for inline in &seg.inlines {
                if let stemma::domain::InlineNode::OpaqueInline(o) = inline
                    && o.id == target.opaque_id
                    && let Some(raw) = &o.raw_xml
                {
                    return String::from_utf8_lossy(raw).into_owned();
                }
            }
        }
    }
    panic!("target raw_xml not found");
}

#[test]
fn inline_sdt_fill_is_reversible() {
    let base = import(
        r#"<w:p><w:sdt><w:sdtPr><w:alias w:val="Tenant"/></w:sdtPr><w:sdtContent><w:r><w:t>Acme Corp</w:t></w:r></w:sdtContent></w:sdt></w:p>"#,
    );
    assert_interior_fidelity(
        &base,
        OpaqueTextTargetKind::InlineSdtText,
        "Acme Corp",
        "Globex Inc",
        "Globex Inc",
    );
}

/// Discovery must tell the caller UP FRONT that a region's text is readable
/// but not editable (pending tracked changes) — the same predicate the edit
/// verb refuses on — instead of leaving them to find out via the refusal.
#[test]
fn discovery_flags_regions_with_pending_tracked_changes() {
    let inner = r#"<w:p><w:r><w:t>clean</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve">has </w:t></w:r><w:ins w:id="7" w:author="A"><w:r><w:t>pending</w:t></w:r></w:ins></w:p>"#;
    let base = import(&format!(
        r#"<w:p><w:r><w:drawing><wp:inline><wp:extent cx="100" cy="100"/><wp:docPr id="1" name="TextBox 1"/><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:txbx><w:txbxContent>{inner}</w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#
    ));
    let targets: Vec<_> = opaque_text_targets(&base)
        .into_iter()
        .filter(|t| t.kind == OpaqueTextTargetKind::TextboxParagraph)
        .collect();
    assert_eq!(targets.len(), 2, "{targets:?}");
    assert!(
        !targets[0].has_tracked_changes,
        "clean paragraph: {targets:?}"
    );
    assert!(
        targets[1].has_tracked_changes,
        "paragraph with a pending w:ins is flagged: {targets:?}"
    );
    // And the flag agrees with the verb: editing the flagged region refuses.
    let step = edit_step(&targets[1], "pending", "resolved");
    let err = apply_transaction(&base, &txn(step, MaterializationMode::TrackedChange)).unwrap_err();
    assert!(
        format!("{err}").contains("tracked"),
        "flagged region refuses with the tracked-changes reason: {err}"
    );
}

#[test]
fn missing_find_refuses_loudly() {
    let base = textbox_docx(&["The quick brown fox"]);
    let target = only_target(&base, OpaqueTextTargetKind::TextboxParagraph);
    let step = edit_step(&target, "wolf", "dog");
    let err = apply_transaction(&base, &txn(step, MaterializationMode::TrackedChange)).unwrap_err();
    assert!(
        format!("{err}").contains("not found"),
        "expected a loud not-found refusal, got: {err}"
    );
}

#[test]
fn non_shrinking_opaque_inventory() {
    // The drawing anchor must survive the interior edit.
    let base = textbox_docx(&["Alpha bravo"]);
    let target = only_target(&base, OpaqueTextTargetKind::TextboxParagraph);
    let opaque_id: NodeId = target.opaque_id.clone();
    let step = edit_step(&target, "bravo", "charlie");
    let edited = apply_transaction(&base, &txn(step, MaterializationMode::TrackedChange))
        .expect("apply")
        .0;
    let present = opaque_text_targets(&edited)
        .iter()
        .any(|t| t.opaque_id == opaque_id);
    assert!(present, "the opaque anchor must not be dropped by the edit");
}

/// RFC-0002 §Phase-3b, end-to-end through the PUBLIC Document API: a tracked
/// interior edit mints a revision INSIDE a textbox whose id is individually
/// selectable — `Document::project(Resolution::Selective { .. })` accepts or
/// rejects just that interior revision, descending into the opaque fragment.
#[test]
fn descent_minted_interior_revision_is_selectively_resolvable() {
    // A textbox "Fix the report".
    let doc = Document::parse(&docx_bytes(
        r#"<w:p><w:r><w:drawing><wp:inline><wp:extent cx="100" cy="100"/><wp:docPr id="1" name="TextBox 1"/><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:txbx><w:txbxContent><w:p><w:r><w:t>Fix the report</w:t></w:r></w:p></w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#,
    ))
    .expect("parse");

    // Apply a tracked interior edit → mints w:del/w:ins inside the textbox.
    let target = opaque_text_targets(&doc.snapshot().canonical)
        .into_iter()
        .find(|t| t.kind == OpaqueTextTargetKind::TextboxParagraph)
        .expect("textbox target");
    let step = edit_step(&target, "report", "summary");
    let edited = doc
        .apply(&txn(step, MaterializationMode::TrackedChange))
        .expect("apply");

    // The minted interior revisions carry real, selectable ids.
    let ids: std::collections::HashSet<u32> = enumerate_revisions(&edited.snapshot().canonical)
        .iter()
        .filter(|r| r.kind == RevisionKind::OpaqueInterior && r.revision_id != 0)
        .map(|r| r.revision_id)
        .collect();
    assert!(
        !ids.is_empty(),
        "the tracked interior edit must mint selectable ids"
    );

    let read = |d: &Document| {
        opaque_text_targets(&d.snapshot().canonical)
            .into_iter()
            .find(|t| t.kind == OpaqueTextTargetKind::TextboxParagraph)
            .map(|t| t.text)
            .unwrap_or_default()
    };

    // Selectively REJECT just those interior ids → the textbox reverts.
    let rejected = edited
        .project(Resolution::Selective {
            ids: ids.clone(),
            action: ResolveSelectionAction::Reject,
        })
        .expect("selective reject");
    assert_eq!(read(&rejected), "Fix the report");

    // Selectively ACCEPT them → the edit is applied.
    let accepted = edited
        .project(Resolution::Selective {
            ids,
            action: ResolveSelectionAction::Accept,
        })
        .expect("selective accept");
    assert_eq!(read(&accepted), "Fix the summary");
}
