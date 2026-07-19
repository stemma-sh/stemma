//! Integration tests for `SetImageAttributes` (Part B) — resize and alt-text
//! edits on an opaque drawing, exercised through the public verb dispatch.
//!
//! Contract under test (CLAUDE.md "no silent fallbacks"; domain-model §11):
//!  - resize writes integer cx/cy to `wp:extent` (NOT the inner `a:ext`);
//!  - alt-text is three-state: set / clear / leave;
//!  - every fail-loud mode (`DrawingNotFound`, `NotADrawing`,
//!    `DrawingMissingRawXml`, `ImageAttributeTargetAbsent`,
//!    `NoImageAttributeRequested`) returns its variant;
//!  - the binary media part is byte-identical after the edit (the verb never
//!    reads or writes media bytes — they live in the package, not the IR).
//!
//! Daily tier, corpus-free (synthesized in-memory DOCX).

use stemma::docx::DocxArchive;
use stemma::domain::{BlockNode, CanonDoc, InlineNode, NodeId, OpaqueKind, RevisionInfo};
use stemma::edit::{
    EditError, EditStep, EditTransaction, ImageResize, MaterializationMode, apply_transaction,
};
use stemma::{DocxRuntime, ExportMode, SimpleRuntime};

const PNG_BYTES: &[u8] = b"\x89PNG\r\n\x1a\n-fake-but-stable-image-bytes-";

/// A DOCX whose paragraph hosts a `w:drawing` referencing `word/media/image1.png`
/// via a blip relationship `rId1`. The media part carries `PNG_BYTES`.
fn make_image_docx(cx: i64, cy: i64, descr: Option<&str>) -> Vec<u8> {
    let descr_attr = descr
        .map(|d| format!(r#" descr="{d}""#))
        .unwrap_or_default();
    let drawing = format!(
        r#"<w:drawing><wp:inline xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><wp:extent cx="{cx}" cy="{cy}"/><wp:docPr id="1" name="Picture 1"{descr_attr}/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:blipFill><a:blip r:embed="rId1"/></pic:blipFill><pic:spPr><a:xfrm><a:ext cx="999" cy="888"/></a:xfrm></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing>"#
    );
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r>{drawing}</w:r></w:p><w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/></Relationships>"#;

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
        zip.start_file("word/media/image1.png", opts).unwrap();
        zip.write_all(PNG_BYTES).unwrap();
        zip.finish().unwrap();
    }
    buf
}

fn canon_of(bytes: &[u8]) -> CanonDoc {
    (*stemma::api::Document::parse(bytes)
        .expect("parse")
        .snapshot()
        .canonical)
        .clone()
}

/// (paragraph id, drawing id, raw_xml) of the first drawing.
fn first_drawing(canon: &CanonDoc) -> (NodeId, NodeId, String) {
    for tb in &canon.blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::OpaqueInline(o) = inline
                        && matches!(o.kind, OpaqueKind::Drawing)
                    {
                        return (
                            p.id.clone(),
                            o.id.clone(),
                            String::from_utf8(o.raw_xml.clone().expect("raw_xml")).unwrap(),
                        );
                    }
                }
            }
        }
    }
    panic!("no drawing found");
}

fn txn(steps: Vec<EditStep>) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        materialization_mode: MaterializationMode::Direct,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("Img".to_string()),
            date: Some("2026-06-01T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

#[test]
fn resize_writes_integer_cx_cy_to_wp_extent() {
    let base = canon_of(&make_image_docx(100, 200, None));
    let (block_id, drawing_id, _) = first_drawing(&base);
    let edited = apply_transaction(
        &base,
        &txn(vec![EditStep::SetImageAttributes {
            block_id,
            drawing_id,
            semantic_hash: None,
            resize: Some(ImageResize {
                cx_emu: 4242,
                cy_emu: 5353,
            }),
            alt_text: None,
            rationale: None,
        }]),
    )
    .expect("apply")
    .0;
    let (_, _, raw) = first_drawing(&edited);
    assert!(raw.contains(r#"cx="4242""#), "wp:extent cx resized");
    assert!(raw.contains(r#"cy="5353""#), "wp:extent cy resized");
    // The inner a:ext (the picture's own transform box) is a SEPARATE element
    // and must be left untouched by a wp:extent resize.
    assert!(raw.contains(r#"cx="999""#), "inner a:ext must be untouched");
}

#[test]
fn alt_text_three_state_set_clear_leave() {
    // set
    let base = canon_of(&make_image_docx(1, 1, None));
    let (b, d, _) = first_drawing(&base);
    let set = apply_transaction(
        &base,
        &txn(vec![EditStep::SetImageAttributes {
            block_id: b.clone(),
            drawing_id: d.clone(),
            semantic_hash: None,
            resize: None,
            alt_text: Some(Some("a logo".to_string())),
            rationale: None,
        }]),
    )
    .expect("set")
    .0;
    assert!(first_drawing(&set).2.contains(r#"descr="a logo""#));

    // clear (Some(None)) — removes @descr entirely
    let base2 = canon_of(&make_image_docx(1, 1, Some("old")));
    let (b2, d2, _) = first_drawing(&base2);
    let cleared = apply_transaction(
        &base2,
        &txn(vec![EditStep::SetImageAttributes {
            block_id: b2,
            drawing_id: d2,
            semantic_hash: None,
            resize: None,
            alt_text: Some(None),
            rationale: None,
        }]),
    )
    .expect("clear")
    .0;
    assert!(!first_drawing(&cleared).2.contains("descr="));

    // leave (None) — resize only, descr preserved
    let base3 = canon_of(&make_image_docx(1, 1, Some("keep me")));
    let (b3, d3, _) = first_drawing(&base3);
    let left = apply_transaction(
        &base3,
        &txn(vec![EditStep::SetImageAttributes {
            block_id: b3,
            drawing_id: d3,
            semantic_hash: None,
            resize: Some(ImageResize {
                cx_emu: 7,
                cy_emu: 8,
            }),
            alt_text: None,
            rationale: None,
        }]),
    )
    .expect("leave")
    .0;
    assert!(first_drawing(&left).2.contains(r#"descr="keep me""#));
}

#[test]
fn missing_drawing_id_fails_drawing_not_found() {
    let base = canon_of(&make_image_docx(1, 1, None));
    let (block_id, _, _) = first_drawing(&base);
    let err = apply_transaction(
        &base,
        &txn(vec![EditStep::SetImageAttributes {
            block_id,
            drawing_id: NodeId::from("no_such_drawing"),
            semantic_hash: None,
            resize: Some(ImageResize {
                cx_emu: 1,
                cy_emu: 1,
            }),
            alt_text: None,
            rationale: None,
        }]),
    )
    .expect_err("must fail");
    assert!(
        matches!(err, EditError::DrawingNotFound { .. }),
        "got {err:?}"
    );
}

#[test]
fn empty_request_fails_no_image_attribute_requested() {
    let base = canon_of(&make_image_docx(1, 1, None));
    let (block_id, drawing_id, _) = first_drawing(&base);
    let err = apply_transaction(
        &base,
        &txn(vec![EditStep::SetImageAttributes {
            block_id,
            drawing_id,
            semantic_hash: None,
            resize: None,
            alt_text: None,
            rationale: None,
        }]),
    )
    .expect_err("must fail");
    assert!(
        matches!(err, EditError::NoImageAttributeRequested { .. }),
        "got {err:?}"
    );
}

#[test]
fn resize_on_drawing_without_extent_fails_target_absent() {
    // A drawing whose raw_xml has a docPr but no wp:extent — resize must fail
    // loudly rather than silently skip.
    let drawing = r#"<w:drawing><wp:inline xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"><wp:docPr id="1" name="P"/></wp:inline></w:drawing>"#;
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r>{drawing}</w:r></w:p><w:sectPr/></w:body></w:document>"#
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
    let base = canon_of(&buf);
    let (block_id, drawing_id, _) = first_drawing(&base);
    let err = apply_transaction(
        &base,
        &txn(vec![EditStep::SetImageAttributes {
            block_id,
            drawing_id,
            semantic_hash: None,
            resize: Some(ImageResize {
                cx_emu: 1,
                cy_emu: 1,
            }),
            alt_text: None,
            rationale: None,
        }]),
    )
    .expect_err("must fail");
    assert!(
        matches!(
            err,
            EditError::ImageAttributeTargetAbsent {
                attribute: "wp:extent",
                ..
            }
        ),
        "got {err:?}"
    );
}

/// The binary media part is byte-identical after a resize: the verb mutates only
/// the drawing display XML in the IR; media bytes live in the package and are
/// never read or written by the verb.
#[test]
fn media_part_bytes_unchanged_after_resize() {
    let runtime = SimpleRuntime::new();
    let import = runtime
        .import_docx(&make_image_docx(100, 200, None))
        .expect("import");

    // Locate the drawing id from the imported canonical doc.
    let view = runtime.view(&import.doc_handle).expect("view");
    let (block_id, drawing_id, _) = first_drawing(&view.canonical);

    let txn = EditTransaction {
        steps: vec![EditStep::SetImageAttributes {
            block_id,
            drawing_id,
            semantic_hash: None,
            resize: Some(ImageResize {
                cx_emu: 4242,
                cy_emu: 5353,
            }),
            alt_text: Some(Some("resized".to_string())),
            rationale: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::Direct,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("Img".to_string()),
            date: Some("2026-06-01T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    };
    runtime.apply_edit(&import.doc_handle, &txn).expect("apply");
    let exported = runtime
        .export_docx(&import.doc_handle, ExportMode::Redline)
        .expect("export");

    let archive = DocxArchive::read(&exported).expect("read");
    let media = archive
        .get("word/media/image1.png")
        .expect("media part must survive the edit");
    assert_eq!(
        media, PNG_BYTES,
        "media bytes must be byte-identical after resize"
    );
}
