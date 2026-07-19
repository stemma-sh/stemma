//! MCP read-surface tests for opaque metadata (milestone M1).
//!
//! The tool methods (`read_block`, `find`) live in the `stemma-mcp` *binary*
//! crate and are not importable here, so — following the convention of
//! `tool_surface_invariants.rs` — these tests drive the same engine surface the
//! tool bodies call (`build_document_view`) and re-implement the thin JSON
//! projection the tools own (`block_detail_json`'s anchor branch, `find`'s
//! metadata match). The load-bearing assertion is that the ENGINE surfaces the
//! correct typed metadata; the JSON shaping is a thin, mechanical wrap.
//!
//! Daily-tier: every document is synthesized in-memory; no corpus, no env.

use std::io::Write;

use serde_json::{Value, json};
use stemma::api::Document;
use stemma::domain::{NodeId, RevisionInfo};
use stemma::edit::{EditStep, EditTransaction, MaterializationMode, SdtValue};
use stemma::view::{
    BlockView, DocumentView, FormFieldIdentity, OpaqueAnchorKind, OpaqueMetadata, SegmentView,
    build_document_view,
};
use stemma::{DocxRuntime, SimpleRuntime};

// ─── In-memory DOCX builder (body-inner injection, mirrors view.rs helper) ────

fn make_docx_with_body(body_inner: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;

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

fn sdt_body(tag: &str, alias: &str, value: &str) -> String {
    format!(
        r#"<w:p><w:r><w:t>Tenant: </w:t></w:r><w:sdt><w:sdtPr><w:alias w:val="{alias}"/><w:tag w:val="{tag}"/><w:text/></w:sdtPr><w:sdtContent><w:r><w:t xml:space="preserve">{value}</w:t></w:r></w:sdtContent></w:sdt></w:p>"#
    )
}

// ─── Thin re-implementations of the tool's JSON projection (the binary owns the
//     originals; these mirror them so the shapes are asserted against the real
//     engine view). Kept minimal and in lockstep with main.rs. ────────────────

/// `block_detail_json`'s anchor branch (the part this milestone changed): the
/// anchor object plus `metadata` when present.
fn anchor_json(seg: &SegmentView) -> Value {
    let SegmentView::Opaque {
        id, kind, metadata, ..
    } = seg
    else {
        panic!("expected opaque segment");
    };
    let mut anchor = json!({
        "kind": "anchor",
        "id": id.to_string(),
        "anchor_kind": anchor_kind_str(*kind),
    });
    if let Some(meta) = metadata {
        anchor["metadata"] = serde_json::to_value(meta).expect("OpaqueMetadata serializes");
    }
    anchor
}

fn anchor_kind_str(kind: OpaqueAnchorKind) -> &'static str {
    match kind {
        OpaqueAnchorKind::Drawing => "image",
        OpaqueAnchorKind::ContentControl => "content_control",
        OpaqueAnchorKind::Field => "field",
        _ => "other",
    }
}

/// `find`'s metadata match (mirrors `opaque_metadata_matches` in main.rs):
/// `(matched_in, anchor_id)` per opaque whose surfaced metadata hits `needle`.
fn metadata_matches(block: &BlockView, needle: &str) -> Vec<(&'static str, String)> {
    let needle = needle.to_lowercase();
    let mut out = Vec::new();
    for seg in &block.segments {
        let SegmentView::Opaque { id, metadata, .. } = seg else {
            continue;
        };
        let Some(meta) = metadata else { continue };
        let hit = |s: &Option<String>| {
            s.as_deref()
                .is_some_and(|v| v.to_lowercase().contains(&needle))
        };
        match meta {
            OpaqueMetadata::ContentControl {
                tag,
                alias,
                display_text,
                ..
            } if hit(tag) || hit(alias) || hit(display_text) => {
                out.push(("content_control", id.to_string()))
            }
            OpaqueMetadata::Drawing { alt_text, .. } if hit(alt_text) => {
                out.push(("image_alt", id.to_string()))
            }
            OpaqueMetadata::Field {
                form:
                    Some(FormFieldIdentity::TextInput {
                        name: Some(name), ..
                    }),
                ..
            } if name.to_lowercase().contains(&needle) => out.push(("form_field", id.to_string())),
            _ => {}
        }
    }
    out
}

fn first_opaque(view: &DocumentView) -> &SegmentView {
    view.blocks[0]
        .segments
        .iter()
        .find(|s| matches!(s, SegmentView::Opaque { .. }))
        .expect("an opaque anchor")
}

// ─── Tests (§5 items 18-21) ───────────────────────────────────────────────────

#[test]
fn read_block_surfaces_content_control_metadata() {
    // §5.18: a named SDT's anchor JSON carries metadata.meta_kind ==
    // "content_control" and the tag.
    let docx = make_docx_with_body(&sdt_body("TenantName", "Tenant Name", "Acme Corporation"));
    let doc = Document::parse(&docx).expect("parse");
    let view = doc.read();
    let anchor = anchor_json(first_opaque(&view));

    assert_eq!(anchor["anchor_kind"], json!("content_control"));
    assert_eq!(anchor["metadata"]["meta_kind"], json!("content_control"));
    assert_eq!(anchor["metadata"]["tag"], json!("TenantName"));
    assert_eq!(anchor["metadata"]["alias"], json!("Tenant Name"));
    assert_eq!(
        anchor["metadata"]["display_text"],
        json!("Acme Corporation")
    );
}

#[test]
fn find_locates_content_control_by_alias_then_set_succeeds() {
    // §5.19: the end-to-end discovery → write flow that motivates this work.
    // `find "Tenant Name"` returns the SDT anchor id with matched_in
    // content_control; that id then drives SetContentControlValue successfully.
    let docx = make_docx_with_body(&sdt_body("TenantName", "Tenant Name", "Acme Corporation"));
    let runtime = SimpleRuntime::new();
    let handle = runtime.import_docx(&docx).expect("import").doc_handle;

    // find by the human alias.
    let (matched_in, anchor_id) = runtime
        .with(&handle, |snap| {
            let view = build_document_view(snap);
            metadata_matches(&view.blocks[0], "tenant name")
                .into_iter()
                .next()
                .expect("a content-control match for the alias")
        })
        .expect("with");
    assert_eq!(matched_in, "content_control");

    // also find by the current value.
    let by_value = runtime
        .with(&handle, |snap| {
            let view = build_document_view(snap);
            metadata_matches(&view.blocks[0], "acme")
        })
        .expect("with");
    assert_eq!(by_value, vec![("content_control", anchor_id.clone())]);

    // Feed the discovered anchor id to the write verb — it must resolve.
    let block_id = runtime
        .with(&handle, |snap| {
            build_document_view(snap).blocks[0].id.to_string()
        })
        .expect("with");
    let txn = EditTransaction {
        steps: vec![EditStep::SetContentControlValue {
            block_id: NodeId::from(block_id.as_str()),
            sdt_id: NodeId::from(anchor_id.as_str()),
            value: SdtValue::Text("Globex Limited".to_string()),
            // Untracked path (the value-set must succeed); tracked:true is the
            // refusing stub until the B1 projector descent lands.
            tracked: false,
            rationale: None,
        }],
        summary: None,
        // The verb is untracked/structural (no w:sdtChange); Direct is its mode.
        materialization_mode: MaterializationMode::Direct,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("tester".to_string()),
            date: Some("2026-06-11T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    };
    runtime
        .apply_edit(&handle, &txn)
        .expect("set_content_control_value succeeds against the discovered id");

    // The new value is now surfaced.
    let new_value = runtime
        .with(&handle, |snap| {
            let view = build_document_view(snap);
            match first_opaque(&view) {
                SegmentView::Opaque {
                    metadata: Some(OpaqueMetadata::ContentControl { display_text, .. }),
                    ..
                } => display_text.clone(),
                _ => None,
            }
        })
        .expect("with");
    assert_eq!(new_value.as_deref(), Some("Globex Limited"));
}

#[test]
fn find_locates_image_by_alt_text() {
    // §5.20: an image's alt text is findable, reported as matched_in image_alt.
    let body = r#"<w:p><w:r><w:drawing xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><wp:inline><wp:extent cx="1143000" cy="685800"/><wp:docPr id="1" name="Picture 1" descr="Acme logo"/><a:graphic><a:graphicData><a:blip r:embed="rId5"/></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#;
    let docx = make_docx_with_body(body);
    let doc = Document::parse(&docx).expect("parse");
    let view = doc.read();
    let matches = metadata_matches(&view.blocks[0], "acme logo");
    assert_eq!(matches.len(), 1, "the alt text is findable");
    assert_eq!(matches[0].0, "image_alt");
}

#[test]
fn read_block_surfaces_drawing_textbox_text() {
    // M3-read: a drawing carrying a textbox surfaces its interior text in the
    // anchor metadata JSON (read_block's detail surface), so an agent can read
    // what the textbox says before a future set_textbox_text replaces it.
    let body = r#"<w:p><w:r><w:drawing xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:txbx><w:txbxContent><w:p><w:r><w:t>Quarterly Report</w:t></w:r></w:p></w:txbxContent></wps:txbx></w:drawing></w:r></w:p>"#;
    let docx = make_docx_with_body(body);
    let doc = Document::parse(&docx).expect("parse");
    let view = doc.read();
    let anchor = anchor_json(first_opaque(&view));
    assert_eq!(anchor["metadata"]["meta_kind"], json!("drawing"));
    assert_eq!(
        anchor["metadata"]["textbox_text"],
        json!("Quarterly Report")
    );
}

#[test]
fn find_text_match_shape_unchanged_plus_matched_in() {
    // §5.21: a plain text match still returns the block (with matched_in: "text")
    // and produces NO spurious anchor match.
    let docx = make_docx_with_body(r#"<w:p><w:r><w:t>The quick brown fox</w:t></w:r></w:p>"#);
    let doc = Document::parse(&docx).expect("parse");
    let view = doc.read();
    let block = &view.blocks[0];

    // The text match fires (the existing behavior).
    assert!(block.text.to_lowercase().contains("brown"));
    // No opaque metadata match on a plain-text block.
    assert!(
        metadata_matches(block, "brown").is_empty(),
        "a plain text block has no metadata anchor match"
    );
}
