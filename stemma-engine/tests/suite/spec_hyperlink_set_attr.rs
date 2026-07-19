//! Spec-roundtrip tests for `set_attr(hyperlink, { href, anchor })`.
//!
//! Verifies the engine's option (A) direct-mutation path end-to-end:
//! build a DOCX -> import -> apply_edit(set_attr) -> export -> re-import,
//! and assert the new URL appears in both `HyperlinkData.url` and the
//! `<Relationship>` entry under `word/_rels/document.xml.rels`.
//!
//! The design decision (no `w:hyperlinkChange`, no tracked envelope — OOXML
//! defines no element to represent a hyperlink retarget as a tracked change)
//! is also pinned here: the mutation is invisible to the tracked-change audit
//! trail by intent.

use std::io::{Cursor, Write};

use stemma::{
    BlockNode, DocxRuntime, ExportMode, InlineNode, NodeId, OpaqueKind, SimpleRuntime,
    domain::RevisionInfo,
    edit::{EditStep, EditTransaction, MaterializationMode},
};
use zip::ZipWriter;
use zip::write::FileOptions;

// ── DOCX builder helpers ──────────────────────────────────────────────────

const CONTENT_TYPES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

const PACKAGE_RELS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

fn document_rels_xml(url: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId100" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="{url}" TargetMode="External"/>
</Relationships>"#
    )
}

fn document_xml() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <w:body>
    <w:p>
      <w:r><w:t xml:space="preserve">See </w:t></w:r>
      <w:hyperlink r:id="rId100">
        <w:r><w:t xml:space="preserve">the policy</w:t></w:r>
      </w:hyperlink>
      <w:r><w:t xml:space="preserve"> for details.</w:t></w:r>
    </w:p>
    <w:sectPr/>
  </w:body>
</w:document>"#
}

fn build_docx_with_hyperlink(url: &str) -> Vec<u8> {
    let cursor = Cursor::new(Vec::new());
    let mut zip = ZipWriter::new(cursor);
    let options = FileOptions::default();

    zip.start_file("[Content_Types].xml", options).unwrap();
    zip.write_all(CONTENT_TYPES_XML.as_bytes()).unwrap();

    zip.start_file("_rels/.rels", options).unwrap();
    zip.write_all(PACKAGE_RELS_XML.as_bytes()).unwrap();

    zip.start_file("word/_rels/document.xml.rels", options)
        .unwrap();
    zip.write_all(document_rels_xml(url).as_bytes()).unwrap();

    zip.start_file("word/document.xml", options).unwrap();
    zip.write_all(document_xml().as_bytes()).unwrap();

    let cursor = zip.finish().unwrap();
    cursor.into_inner()
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn find_hyperlink_id_and_url(
    runtime: &SimpleRuntime,
    handle: &stemma::DocHandle,
) -> (NodeId, Option<String>) {
    let view = runtime.view(handle).expect("view");
    for block in &view.canonical.blocks {
        if let BlockNode::Paragraph(p) = &block.block {
            for inline in p.all_inlines_owned() {
                if let InlineNode::OpaqueInline(o) = inline
                    && let OpaqueKind::Hyperlink(data) = &o.kind
                {
                    return (o.id.clone(), data.url.clone());
                }
            }
        }
    }
    panic!("no hyperlink in imported document");
}

fn extract_hyperlink_targets_from_rels(docx_bytes: &[u8]) -> Vec<String> {
    let cursor = Cursor::new(docx_bytes);
    let mut zip = zip::ZipArchive::new(cursor).expect("zip");
    let mut rels_xml = String::new();
    {
        use std::io::Read;
        let mut file = zip
            .by_name("word/_rels/document.xml.rels")
            .expect("word/_rels/document.xml.rels missing");
        file.read_to_string(&mut rels_xml).expect("read rels");
    }
    // Crude but robust enough: find Relationship elements whose Type is the
    // hyperlink relationship and capture their Target attribute.
    let mut out = Vec::new();
    let hyperlink_type =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink";
    for chunk in rels_xml.split("<Relationship ") {
        if chunk.contains(hyperlink_type)
            && let Some(start) = chunk.find("Target=\"")
        {
            let after = &chunk[start + "Target=\"".len()..];
            if let Some(end) = after.find('"') {
                out.push(after[..end].to_string());
            }
        }
    }
    out
}

fn revision_info() -> RevisionInfo {
    RevisionInfo {
        revision_id: 0,
        identity: 0,
        author: Some("set_attr_spec".to_string()),
        date: Some("2026-05-21T00:00:00Z".to_string()),
        apply_op_id: None,
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[test]
fn hyperlink_href_change_roundtrips_through_serializer() {
    let runtime = SimpleRuntime::new();
    let docx_bytes = build_docx_with_hyperlink("https://example.com/old-target");
    let import = runtime.import_docx(&docx_bytes).expect("import");

    let (hyperlink_id, original_url) = find_hyperlink_id_and_url(&runtime, &import.doc_handle);
    assert_eq!(
        original_url.as_deref(),
        Some("https://example.com/old-target")
    );

    let txn = EditTransaction {
        steps: vec![EditStep::SetHyperlinkAttr {
            hyperlink_id: hyperlink_id.clone(),
            new_href: Some("https://example.com/new-target".to_string()),
            new_anchor: None,
            expect_href: Some("https://example.com/old-target".to_string()),
            expect_anchor: None,
            rationale: Some("retarget policy link".to_string()),
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: revision_info(),
    };
    runtime
        .apply_edit(&import.doc_handle, &txn)
        .expect("apply_edit");

    // 1. The in-memory IR carries the new URL.
    let (_, after_url) = find_hyperlink_id_and_url(&runtime, &import.doc_handle);
    assert_eq!(
        after_url.as_deref(),
        Some("https://example.com/new-target"),
        "HyperlinkData.url must reflect the new target after apply",
    );

    // 2. Export and re-parse.
    let exported = runtime
        .export_docx(&import.doc_handle, ExportMode::Redline)
        .expect("export");

    let rels_targets = extract_hyperlink_targets_from_rels(&exported);
    assert!(
        rels_targets
            .iter()
            .any(|t| t == "https://example.com/new-target"),
        "exported rels must contain the new URL; got {rels_targets:?}",
    );
    // The old URL's relationship entry may remain as an orphan: the active
    // <w:hyperlink r:id> no longer points at it, but no sweep is performed
    // at serialize time. This is intentional — the hyperlink-retarget
    // contract does not include an orphan-relationship sweep. The cross-file
    // invariant pinned by
    // `hyperlink_rid_remains_consistent_after_mutation` is that the
    // hyperlink element's rId resolves to the new URL, not that the rels
    // file has been purged.

    // 3. Re-import the exported bytes and confirm the new URL is preserved.
    let reimport = runtime.import_docx(&exported).expect("re-import");
    let (_, reimported_url) = find_hyperlink_id_and_url(&runtime, &reimport.doc_handle);
    assert_eq!(
        reimported_url.as_deref(),
        Some("https://example.com/new-target"),
        "URL must survive roundtrip through serializer",
    );
}

#[test]
fn hyperlink_rid_remains_consistent_after_mutation() {
    // After mutation, the inline `<w:hyperlink r:id="X">` must reference an
    // rId whose `<Relationship>` Target matches the new URL. The serializer
    // re-allocates the rId from `data.url` via the rel resolver; the test
    // pins the cross-file consistency.
    let runtime = SimpleRuntime::new();
    let docx_bytes = build_docx_with_hyperlink("https://example.com/old-target");
    let import = runtime.import_docx(&docx_bytes).expect("import");

    let (hyperlink_id, _) = find_hyperlink_id_and_url(&runtime, &import.doc_handle);

    let txn = EditTransaction {
        steps: vec![EditStep::SetHyperlinkAttr {
            hyperlink_id,
            new_href: Some("https://example.com/new-target".to_string()),
            new_anchor: None,
            expect_href: Some("https://example.com/old-target".to_string()),
            expect_anchor: None,
            rationale: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: revision_info(),
    };
    runtime
        .apply_edit(&import.doc_handle, &txn)
        .expect("apply_edit");
    let exported = runtime
        .export_docx(&import.doc_handle, ExportMode::Redline)
        .expect("export");

    // Pull the rId off the document.xml hyperlink element, and the matching
    // Target off the rels file, and assert they agree on the new URL.
    use std::io::Read;
    let cursor = Cursor::new(&exported);
    let mut zip = zip::ZipArchive::new(cursor).expect("zip");
    let mut doc_xml = String::new();
    zip.by_name("word/document.xml")
        .expect("document.xml")
        .read_to_string(&mut doc_xml)
        .expect("read doc xml");
    let mut rels_xml = String::new();
    zip.by_name("word/_rels/document.xml.rels")
        .expect("rels")
        .read_to_string(&mut rels_xml)
        .expect("read rels");

    // Extract the rId from the first `<w:hyperlink r:id="...">` in document.xml.
    let hyperlink_pos = doc_xml.find("<w:hyperlink").expect("w:hyperlink in export");
    let after = &doc_xml[hyperlink_pos..];
    let rid_start = after.find("r:id=\"").expect("r:id attribute on hyperlink");
    let rid_after = &after[rid_start + "r:id=\"".len()..];
    let rid_end = rid_after.find('"').expect("r:id close quote");
    let rid = &rid_after[..rid_end];

    // Find the matching <Relationship Id="<rid>"> Target in rels_xml.
    let rel_marker = format!("Id=\"{rid}\"");
    let rel_pos = rels_xml
        .find(&rel_marker)
        .unwrap_or_else(|| panic!("rels missing Relationship with Id={rid}; rels={rels_xml}"));
    // The Relationship element starts somewhere before rel_pos; the Target
    // attribute follows in the same element. Searching forward from rel_pos
    // is sufficient because attributes are on the same line.
    let rel_after = &rels_xml[rel_pos..];
    let target_start = rel_after.find("Target=\"").expect("Target attribute");
    let target_after = &rel_after[target_start + "Target=\"".len()..];
    let target_end = target_after.find('"').expect("Target close quote");
    let target = &target_after[..target_end];

    assert_eq!(
        target, "https://example.com/new-target",
        "the inline hyperlink's rId must resolve to the new URL after mutation",
    );
}

#[test]
fn hyperlink_set_attr_produces_no_tracked_changes() {
    // Pin the design decision: option (A) is intentionally not tracked.
    // After `set_attr(hyperlink, { href })`, the document must contain no
    // new tracked-change segments around the hyperlink. The mutation is
    // invisible to the tracked-change audit trail by design.
    //
    // OOXML defines no `w:hyperlinkChange` element, so a retarget cannot be
    // represented as a tracked change.
    use stemma::TrackingStatus;
    let runtime = SimpleRuntime::new();
    let docx_bytes = build_docx_with_hyperlink("https://example.com/old-target");
    let import = runtime.import_docx(&docx_bytes).expect("import");

    let (hyperlink_id, _) = find_hyperlink_id_and_url(&runtime, &import.doc_handle);

    let txn = EditTransaction {
        steps: vec![EditStep::SetHyperlinkAttr {
            hyperlink_id,
            new_href: Some("https://example.com/new-target".to_string()),
            new_anchor: None,
            expect_href: Some("https://example.com/old-target".to_string()),
            expect_anchor: None,
            rationale: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: revision_info(),
    };
    runtime
        .apply_edit(&import.doc_handle, &txn)
        .expect("apply_edit");

    let view = runtime.view(&import.doc_handle).expect("view");
    for block in &view.canonical.blocks {
        assert!(
            matches!(block.status, TrackingStatus::Normal),
            "block status must remain Normal; got {:?}",
            block.status
        );
        if let BlockNode::Paragraph(p) = &block.block {
            for seg in &p.segments {
                assert!(
                    matches!(seg.status, TrackingStatus::Normal),
                    "segment status must remain Normal; got {:?}",
                    seg.status
                );
                // Inside the hyperlink: no run is Inserted or Deleted.
                for inline in &seg.inlines {
                    if let InlineNode::OpaqueInline(o) = inline
                        && let OpaqueKind::Hyperlink(data) = &o.kind
                    {
                        for run in &data.runs {
                            assert!(
                                matches!(run.status, TrackingStatus::Normal),
                                "hyperlink run must stay Normal; got {:?}",
                                run.status,
                            );
                        }
                    }
                }
            }
        }
    }
}
