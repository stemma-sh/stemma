//! Integration tests for `InsertImage` / `ReplaceImage` (Verb C) — authoring
//! binary image media through the package via the PendingParts channel.
//!
//! Contract under test (CLAUDE.md "no silent fallbacks"; domain-model §11):
//!  - InsertImage accept-all has the drawing + a registered media part + a real
//!    rId (no placeholder logical rId left behind, no orphan rId);
//!  - InsertImage reject-all equals the baseline AND opens validator-clean (an
//!    unreferenced media part is fine; a referenced-but-missing rId is not);
//!  - ReplaceImage swaps the binary, content_hash changes, opaque inventory does
//!    not shrink;
//!  - fail-loud: UnsupportedImageFormat (magic mismatch), ImageBytesEmpty.
//!
//! Daily tier, corpus-free (synthesized in-memory DOCX + a tiny synthetic PNG).

use stemma::api::{Document, validate};
use stemma::docx::DocxArchive;
use stemma::domain::{BlockNode, CanonDoc, InlineNode, NodeId, OpaqueKind, RevisionInfo};
use stemma::edit::{
    EditError, EditStep, EditTransaction, ImageFormat, ImageSource, MaterializationMode,
    apply_transaction,
};
use stemma::runtime::ExportOptions;
use stemma::{Resolution, accept_all, reject_all_with_styles};

/// A tiny but magic-valid PNG byte blob: the 8-byte PNG signature plus a short
/// payload. Content is irrelevant — only the leading magic is checked.
fn tiny_png(tag: u8) -> Vec<u8> {
    let mut v = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    v.extend_from_slice(&[tag; 24]);
    v
}

fn make_text_docx(paragraphs: &[&str]) -> Vec<u8> {
    let mut document_xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>"#,
    );
    for para in paragraphs {
        document_xml.push_str(&format!(r#"<w:p><w:r><w:t>{para}</w:t></w:r></w:p>"#));
    }
    document_xml.push_str("<w:sectPr/></w:body></w:document>");

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

fn first_block_id(canon: &CanonDoc) -> NodeId {
    match &canon.blocks[0].block {
        BlockNode::Paragraph(p) => p.id.clone(),
        _ => panic!("first block is not a paragraph"),
    }
}

/// Count drawing opaque inlines across all paragraphs/segments.
fn drawing_count(canon: &CanonDoc) -> usize {
    let mut n = 0;
    for tb in &canon.blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::OpaqueInline(o) = inline
                        && matches!(o.kind, OpaqueKind::Drawing)
                    {
                        n += 1;
                    }
                }
            }
        }
    }
    n
}

/// Extract the `r:embed` blip rId from a drawing fragment (test-local; the
/// engine's `find_blip_rid` is crate-private).
fn blip_rid(xml: &str) -> Option<String> {
    let start = xml.find("r:embed=\"")? + "r:embed=\"".len();
    let end = xml[start..].find('"')?;
    Some(xml[start..start + end].to_string())
}

/// The first drawing's blip rId, if any.
fn first_drawing_rid(canon: &CanonDoc) -> Option<String> {
    for tb in &canon.blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::OpaqueInline(o) = inline
                        && matches!(o.kind, OpaqueKind::Drawing)
                        && let Some(raw) = &o.raw_xml
                        && let Ok(s) = std::str::from_utf8(raw)
                        && let Some(rid) = blip_rid(s)
                    {
                        return Some(rid);
                    }
                }
            }
        }
    }
    None
}

/// The first drawing's `raw_xml` as a UTF-8 string (for asserting on the
/// serialized fragment, e.g. that `wp:extent` was applied).
fn first_drawing_raw(canon: &CanonDoc) -> String {
    for tb in &canon.blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::OpaqueInline(o) = inline
                        && matches!(o.kind, OpaqueKind::Drawing)
                        && let Some(raw) = &o.raw_xml
                    {
                        return String::from_utf8(raw.clone()).expect("utf8 raw_xml");
                    }
                }
            }
        }
    }
    panic!("no drawing raw_xml found");
}

fn txn(steps: Vec<EditStep>, mode: MaterializationMode) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        materialization_mode: mode,
        revision: RevisionInfo {
            revision_id: 1,
            author: Some("Imager".to_string()),
            date: Some("2026-06-05T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

fn insert_step(block_id: NodeId, bytes: Vec<u8>) -> EditStep {
    let image = ImageSource::new(
        bytes,
        ImageFormat::Png,
        914_400,
        457_200,
        Some("logo".into()),
        0,
    )
    .expect("valid png source");
    EditStep::InsertImage {
        block_id,
        expect: None,
        semantic_hash: None,
        image,
        rationale: None,
    }
}

// ─── Verb-core (CanonDoc) shape ──────────────────────────────────────────────

#[test]
fn insert_image_appends_inserted_drawing_segment() {
    let base = Document::parse(&make_text_docx(&["Hello."]))
        .unwrap()
        .snapshot()
        .canonical
        .clone();
    assert_eq!(drawing_count(&base), 0, "baseline has no drawing");
    let block_id = first_block_id(&base);

    let (edited, _pending) = apply_transaction(
        &base,
        &txn(
            vec![insert_step(block_id, tiny_png(1))],
            MaterializationMode::TrackedChange,
        ),
    )
    .expect("insert applies");

    assert_eq!(drawing_count(&edited), 1, "one drawing inserted");

    // Reject-all (verb core) drops the inserted drawing → baseline shape.
    let mut rejected = edited.clone();
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(drawing_count(&rejected), 0, "reject-all drops the drawing");

    // Accept-all keeps the drawing; equals direct.
    let mut accepted = edited;
    accept_all(&mut accepted);
    assert_eq!(drawing_count(&accepted), 1, "accept-all keeps the drawing");
    let block_id2 = first_block_id(&base);
    let (direct, _) = apply_transaction(
        &base,
        &txn(
            vec![insert_step(block_id2, tiny_png(1))],
            MaterializationMode::Direct,
        ),
    )
    .expect("direct applies");
    assert_eq!(
        drawing_count(&accepted),
        drawing_count(&direct),
        "accept-all drawing inventory equals direct"
    );
}

// ─── Full save path: media materialization + validity ────────────────────────

#[test]
fn insert_image_accept_all_has_registered_media_and_real_rid() {
    let doc = Document::parse(&make_text_docx(&["Hello."])).unwrap();
    let block_id = first_block_id(&doc.snapshot().canonical);

    let edited = doc
        .apply(&txn(
            vec![insert_step(block_id, tiny_png(2))],
            MaterializationMode::TrackedChange,
        ))
        .expect("apply InsertImage");

    // The materialized snapshot's IR rId must be a REAL rId (not the logical
    // placeholder), and a media part must exist.
    let rid = first_drawing_rid(&edited.snapshot().canonical).expect("drawing has a blip rId");
    assert!(
        !rid.starts_with("rIdimg"),
        "logical rId must be rewritten to a real package rId, got {rid}"
    );

    // Accept-all then serialize: validator-clean, with a media part present.
    let accepted = edited.project(Resolution::AcceptAll).expect("accept-all");
    let bytes = accepted
        .serialize(&ExportOptions::default())
        .expect("serialize accept-all");

    let report = validate(&bytes);
    assert!(
        report.ok,
        "accept-all must open validator-clean: {:?}",
        report.issues
    );

    let archive = DocxArchive::read(&bytes).expect("read");
    let media: Vec<String> = archive
        .list()
        .filter(|n| n.starts_with("word/media/"))
        .map(|n| n.to_string())
        .collect();
    assert_eq!(media.len(), 1, "exactly one media part written: {media:?}");
    assert!(media[0].ends_with(".png"));
    assert_eq!(
        archive.get(&media[0]).unwrap(),
        tiny_png(2).as_slice(),
        "media bytes preserved"
    );
}

#[test]
fn insert_image_reject_all_is_baseline_and_validator_clean() {
    let baseline_bytes = make_text_docx(&["Hello."]);
    let doc = Document::parse(&baseline_bytes).unwrap();
    let block_id = first_block_id(&doc.snapshot().canonical);

    let edited = doc
        .apply(&txn(
            vec![insert_step(block_id, tiny_png(3))],
            MaterializationMode::TrackedChange,
        ))
        .expect("apply InsertImage");

    let rejected = edited.project(Resolution::RejectAll).expect("reject-all");

    // reject-all == baseline body: no drawing remains.
    assert_eq!(
        drawing_count(&rejected.snapshot().canonical),
        0,
        "reject-all drops the inserted drawing"
    );

    // CRITICAL: the reject-all package must open clean. The media part may be
    // left unreferenced (harmless), but there must be NO dangling rId.
    let bytes = rejected
        .serialize(&ExportOptions::default())
        .expect("serialize reject-all");
    let report = validate(&bytes);
    assert!(
        report.ok,
        "reject-all of an inserted image must open validator-clean (no orphan rId): {:?}",
        report.issues
    );

    // The reject-all visible text equals the baseline visible text.
    let baseline_text = Document::parse(&baseline_bytes).unwrap().to_text();
    assert_eq!(
        rejected.to_text(),
        baseline_text,
        "reject-all text == baseline"
    );
}

// ─── ReplaceImage ────────────────────────────────────────────────────────────

/// A DOCX whose paragraph hosts a `w:drawing` referencing `word/media/image1.png`
/// via blip `rId1`.
fn make_image_docx() -> Vec<u8> {
    let drawing = r#"<w:drawing><wp:inline xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><wp:extent cx="100" cy="200"/><wp:docPr id="1" name="Picture 1"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:blipFill><a:blip r:embed="rId1"/></pic:blipFill><pic:spPr/></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing>"#;
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
        zip.write_all(&tiny_png(9)).unwrap();
        zip.finish().unwrap();
    }
    buf
}

fn first_drawing(canon: &CanonDoc) -> (NodeId, NodeId, Option<String>) {
    for tb in &canon.blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::OpaqueInline(o) = inline
                        && matches!(o.kind, OpaqueKind::Drawing)
                    {
                        return (p.id.clone(), o.id.clone(), o.content_hash.clone());
                    }
                }
            }
        }
    }
    panic!("no drawing found");
}

#[test]
fn replace_image_swaps_binary_changes_hash_preserves_opaque() {
    let base = Document::parse(&make_image_docx())
        .unwrap()
        .snapshot()
        .canonical
        .clone();
    let (block_id, drawing_id, before_hash) = first_drawing(&base);
    let opaque_before = drawing_count(&base);

    // The base drawing's display box is cx="100" cy="200" (a 1:2 box). Replace
    // with a real 100x200 PNG (1:2) at a NEW extent cx=200 cy=400 — also 1:2, so
    // the aspect guard passes and the extent is applied. (Needs a decodable
    // header now that an undecodable one is refused.)
    let new_bytes = png_wh(100, 200);
    let image = ImageSource::new(new_bytes.clone(), ImageFormat::Png, 200, 400, None, 0).unwrap();
    let (edited, pending) = apply_transaction(
        &base,
        &txn(
            vec![EditStep::ReplaceImage {
                block_id,
                drawing_id,
                semantic_hash: None,
                image,
                allow_stretch: false,
                rationale: None,
            }],
            MaterializationMode::Direct,
        ),
    )
    .expect("replace applies");

    // content_hash changed (the drawing's blip rId was rewritten).
    let (_, _, after_hash) = first_drawing(&edited);
    assert_ne!(
        after_hash, before_hash,
        "content_hash must change after a media swap"
    );

    // The requested display extent is APPLIED to wp:extent (regression: it used
    // to be silently discarded — only the rId was swapped).
    let raw = first_drawing_raw(&edited);
    assert!(
        raw.contains(r#"cx="200""#),
        "wp:extent @cx must be applied: {raw}"
    );
    assert!(
        raw.contains(r#"cy="400""#),
        "wp:extent @cy must be applied: {raw}"
    );

    // opaque inventory did not shrink.
    assert_eq!(
        drawing_count(&edited),
        opaque_before,
        "opaque inventory non-shrinking"
    );

    // The new media was staged.
    assert_eq!(pending.media.len(), 1, "one media staged");
    assert_eq!(pending.media[0].bytes, new_bytes, "staged the new bytes");
}

#[test]
fn replace_image_full_path_registers_new_media_validator_clean() {
    let doc = Document::parse(&make_image_docx()).unwrap();
    let (block_id, drawing_id, _) = first_drawing(&doc.snapshot().canonical);

    // 3:4 PNG into a 3:4 extent — decodable header, not a stretch.
    let new_bytes = png_wh(300, 400);
    let image = ImageSource::new(new_bytes.clone(), ImageFormat::Png, 300, 400, None, 0).unwrap();
    let edited = doc
        .apply(&txn(
            vec![EditStep::ReplaceImage {
                block_id,
                drawing_id,
                semantic_hash: None,
                image,
                allow_stretch: false,
                rationale: None,
            }],
            MaterializationMode::Direct,
        ))
        .expect("apply ReplaceImage");

    // The drawing's rId is a real rId (the new media's), not the logical one.
    let rid = first_drawing_rid(&edited.snapshot().canonical).expect("blip rId");
    assert!(
        !rid.starts_with("rIdimg"),
        "logical rId must be rewritten, got {rid}"
    );

    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let report = validate(&bytes);
    assert!(
        report.ok,
        "ReplaceImage output must open validator-clean: {:?}",
        report.issues
    );

    // The new binary is present in the package.
    let archive = DocxArchive::read(&bytes).expect("read");
    let has_new = archive
        .list()
        .filter(|n| n.starts_with("word/media/"))
        .any(|n| {
            archive
                .get(n)
                .map(|b| b == new_bytes.as_slice())
                .unwrap_or(false)
        });
    assert!(
        has_new,
        "the new media binary must be present in the package"
    );
}

// ─── Aspect-ratio guard (replace) ────────────────────────────────────────────

/// A magic-valid PNG whose IHDR declares the given pixel dimensions, so the
/// engine's `intrinsic_dimensions` reader returns `(w, h)` and the aspect guard
/// can fire.
fn png_wh(w: u32, h: u32) -> Vec<u8> {
    let mut v = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    v.extend_from_slice(&[0, 0, 0, 13]); // IHDR length
    v.extend_from_slice(b"IHDR");
    v.extend_from_slice(&w.to_be_bytes());
    v.extend_from_slice(&h.to_be_bytes());
    v.extend_from_slice(&[8, 2, 0, 0, 0]);
    v
}

fn replace_step(
    block_id: NodeId,
    drawing_id: NodeId,
    image: ImageSource,
    allow_stretch: bool,
) -> EditStep {
    EditStep::ReplaceImage {
        block_id,
        drawing_id,
        semantic_hash: None,
        image,
        allow_stretch,
        rationale: None,
    }
}

#[test]
fn replace_matching_aspect_applies_extent() {
    // 800x600 image (4:3) into a 4:3 extent (e.g. 4000000x3000000 EMU) →
    // succeeds, extent applied.
    let base = Document::parse(&make_image_docx())
        .unwrap()
        .snapshot()
        .canonical
        .clone();
    let (block_id, drawing_id, _) = first_drawing(&base);
    let image = ImageSource::new(
        png_wh(800, 600),
        ImageFormat::Png,
        4_000_000,
        3_000_000,
        None,
        0,
    )
    .unwrap();
    let (edited, _) = apply_transaction(
        &base,
        &txn(
            vec![replace_step(block_id, drawing_id, image, false)],
            MaterializationMode::Direct,
        ),
    )
    .expect("matching aspect must apply");
    let raw = first_drawing_raw(&edited);
    assert!(
        raw.contains(r#"cx="4000000""#) && raw.contains(r#"cy="3000000""#),
        "{raw}"
    );
}

#[test]
fn replace_mismatched_aspect_refuses() {
    // 800x600 image (4:3) into a 16:9 extent → ImageAspectMismatch (refuse the
    // silent stretch).
    let base = Document::parse(&make_image_docx())
        .unwrap()
        .snapshot()
        .canonical
        .clone();
    let (block_id, drawing_id, _) = first_drawing(&base);
    let image = ImageSource::new(
        png_wh(800, 600),
        ImageFormat::Png,
        16_000_000,
        9_000_000,
        None,
        0,
    )
    .unwrap();
    let err = apply_transaction(
        &base,
        &txn(
            vec![replace_step(block_id, drawing_id, image, false)],
            MaterializationMode::Direct,
        ),
    )
    .expect_err("mismatched aspect must refuse");
    match err {
        EditError::ImageAspectMismatch {
            intrinsic_w,
            intrinsic_h,
            requested_cx,
            requested_cy,
            ..
        } => {
            assert_eq!((intrinsic_w, intrinsic_h), (800, 600));
            assert_eq!((requested_cx, requested_cy), (16_000_000, 9_000_000));
        }
        other => panic!("expected ImageAspectMismatch, got {other:?}"),
    }
}

#[test]
fn replace_allow_stretch_overrides_aspect_guard() {
    // Same mismatch, but allow_stretch=true → succeeds, extent applied (a
    // deliberate stretch).
    let base = Document::parse(&make_image_docx())
        .unwrap()
        .snapshot()
        .canonical
        .clone();
    let (block_id, drawing_id, _) = first_drawing(&base);
    let image = ImageSource::new(
        png_wh(800, 600),
        ImageFormat::Png,
        16_000_000,
        9_000_000,
        None,
        0,
    )
    .unwrap();
    let (edited, _) = apply_transaction(
        &base,
        &txn(
            vec![replace_step(block_id, drawing_id, image, true)],
            MaterializationMode::Direct,
        ),
    )
    .expect("allow_stretch must permit the stretch");
    let raw = first_drawing_raw(&edited);
    assert!(
        raw.contains(r#"cx="16000000""#) && raw.contains(r#"cy="9000000""#),
        "{raw}"
    );
}

#[test]
fn replace_undecodable_header_refuses() {
    // tiny_png is magic-valid (passes ImageSource::new) but has no IHDR, so its
    // pixel dimensions can't be decoded. A header we can't read is a corrupt
    // image we're about to embed — refuse with ImageHeaderUndecodable.
    let base = Document::parse(&make_image_docx())
        .unwrap()
        .snapshot()
        .canonical
        .clone();
    let (block_id, drawing_id, _) = first_drawing(&base);
    let image = ImageSource::new(tiny_png(2), ImageFormat::Png, 100, 100, None, 0).unwrap();
    let err = apply_transaction(
        &base,
        &txn(
            vec![replace_step(block_id, drawing_id, image, false)],
            MaterializationMode::Direct,
        ),
    )
    .expect_err("undecodable header must refuse");
    match err {
        EditError::ImageHeaderUndecodable { format, .. } => assert_eq!(format, "image/png"),
        other => panic!("expected ImageHeaderUndecodable, got {other:?}"),
    }
}

#[test]
fn replace_allow_stretch_does_not_bypass_undecodable_header() {
    // allow_stretch opts into STRETCHING, not into corrupt bytes — the
    // undecodable-header refusal must still fire.
    let base = Document::parse(&make_image_docx())
        .unwrap()
        .snapshot()
        .canonical
        .clone();
    let (block_id, drawing_id, _) = first_drawing(&base);
    let image = ImageSource::new(tiny_png(4), ImageFormat::Png, 100, 100, None, 0).unwrap();
    let err = apply_transaction(
        &base,
        &txn(
            vec![replace_step(block_id, drawing_id, image, true)],
            MaterializationMode::Direct,
        ),
    )
    .expect_err("undecodable header must refuse even with allow_stretch");
    assert!(
        matches!(err, EditError::ImageHeaderUndecodable { .. }),
        "got {err:?}"
    );
}

// ─── Fail-loud ───────────────────────────────────────────────────────────────

#[test]
fn unsupported_format_fails_loud() {
    // GIF bytes declared as PNG.
    let gif = b"GIF89a-payload".to_vec();
    let err = ImageSource::new(gif, ImageFormat::Png, 1, 1, None, 0).unwrap_err();
    assert!(
        matches!(err, EditError::UnsupportedImageFormat { .. }),
        "got {err:?}"
    );
}

#[test]
fn empty_bytes_fails_loud() {
    let err = ImageSource::new(Vec::new(), ImageFormat::Png, 1, 1, None, 0).unwrap_err();
    assert!(
        matches!(err, EditError::ImageBytesEmpty { .. }),
        "got {err:?}"
    );
}

/// A DOCX whose drawing has a blip but NO `wp:extent` — replace must fail loud
/// rather than silently skip applying the (now required-and-used) extent.
fn make_image_docx_no_extent() -> Vec<u8> {
    let drawing = r#"<w:drawing><wp:inline xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><wp:docPr id="1" name="Picture 1"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:blipFill><a:blip r:embed="rId1"/></pic:blipFill><pic:spPr/></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing>"#;
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
    buf
}

#[test]
fn replace_without_extent_fails_loud() {
    let base = Document::parse(&make_image_docx_no_extent())
        .unwrap()
        .snapshot()
        .canonical
        .clone();
    let (block_id, drawing_id, _) = first_drawing(&base);
    // A real 100x100 PNG into a 1:1 extent — decodable header, aspect matches,
    // so the verb proceeds and then refuses because there is no wp:extent to
    // write.
    let image = ImageSource::new(png_wh(100, 100), ImageFormat::Png, 100, 100, None, 0).unwrap();
    let err = apply_transaction(
        &base,
        &txn(
            vec![replace_step(block_id, drawing_id, image, false)],
            MaterializationMode::Direct,
        ),
    )
    .expect_err("missing wp:extent must fail loud");
    match err {
        EditError::ImageAttributeTargetAbsent { attribute, .. } => {
            assert_eq!(attribute, "wp:extent");
        }
        other => panic!("expected ImageAttributeTargetAbsent(wp:extent), got {other:?}"),
    }
}

// ─── B7: second image into a doc that already has one ────────────────────────

#[test]
fn insert_second_image_into_doc_with_existing_image_validates() {
    // B7: a doc that ALREADY contains an image (target_image_rels non-empty) is
    // the only case where `copy_target_media_for_inserted_drawings` runs. It used
    // to collect the verb-staged logical "rIdimg…" rId and look it up in the
    // target rels, failing with an orphaned-rId InvalidDocx — even though that
    // media is registered by `apply_pending_media`, not copied from the target.
    let doc = Document::parse(&make_image_docx()).unwrap();
    let base = doc.snapshot().canonical.clone();
    assert_eq!(drawing_count(&base), 1, "fixture starts with one image");
    let block_id = first_block_id(&base);

    let edited = doc
        .apply(&txn(
            vec![insert_step(block_id, tiny_png(7))],
            MaterializationMode::TrackedChange,
        ))
        .expect("inserting a second image applies");
    assert_eq!(
        drawing_count(&edited.snapshot().canonical),
        2,
        "now two drawings"
    );

    // Accept-all must serialize validator-clean (no orphaned rId) — the B7 bug.
    let accepted = edited.project(Resolution::AcceptAll).expect("accept-all");
    let bytes = accepted
        .serialize(&ExportOptions::default())
        .expect("serialize accept-all");
    assert!(
        validate(&bytes).ok,
        "two-image accept-all must validate: {:?}",
        validate(&bytes).issues
    );

    // Reject-all drops the inserted image, keeps the original, still valid.
    let rejected = edited.project(Resolution::RejectAll).expect("reject-all");
    assert_eq!(
        drawing_count(&rejected.snapshot().canonical),
        1,
        "reject-all keeps only the original image"
    );
    let rbytes = rejected
        .serialize(&ExportOptions::default())
        .expect("serialize reject-all");
    assert!(validate(&rbytes).ok, "reject-all must validate");
}
