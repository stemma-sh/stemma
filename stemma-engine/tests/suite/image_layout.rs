//! `SetImageLayout` end-to-end: from the wire JSON op, through the engine, to a
//! serialized DOCX that the post-serialization validator accepts — plus the
//! direct-untracked semantics (accept-all == reject-all == the edited doc, NOT
//! reject==original, because OOXML has no tracked envelope for drawing display
//! attributes — exactly like `SetImageAttributes`).
//!
//! Crop targets `a:srcRect`; position/wrap target the `wp:anchor` and are gated
//! (fail loud) on an inline drawing. Daily tier, corpus-free.

use stemma::api::Document;
use stemma::docx_validate::validate_docx;
use stemma::edit_v4::parse_transaction;
use stemma::{ExportOptions, Resolution};

/// Fail with the validator's error messages if the serialized DOCX is not clean.
fn assert_validator_clean(docx: &[u8]) {
    let report = validate_docx(docx);
    if report.has_errors() {
        let msgs: Vec<String> = report
            .errors()
            .map(|e| format!("{} @ {}: {}", e.rule_id, e.location, e.message))
            .collect();
        panic!(
            "post-serialization validator rejected the output:\n{}",
            msgs.join("\n")
        );
    }
}

/// Wrap a `<w:drawing>` fragment into a single-paragraph DOCX with a real image
/// relationship + media part, so the serialized output is a complete package the
/// validator can check.
fn docx_with_drawing(drawing: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r>{drawing}</w:r></w:p><w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdDoc" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
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
        zip.write_all(b"\x89PNG\r\n\x1a\n-fake-image-").unwrap();
        zip.finish().unwrap();
    }
    buf
}

const INLINE_PICTURE: &str = r#"<w:drawing><wp:inline xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><wp:extent cx="100" cy="200"/><wp:docPr id="1" name="Picture 1"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic><pic:nvPicPr><pic:cNvPr id="1" name="Picture 1"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed="rId1"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="100" cy="200"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing>"#;

const ANCHORED_PICTURE: &str = r#"<w:drawing><wp:anchor xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" distT="0" distB="0" distL="0" distR="0" simplePos="0" relativeHeight="0" behindDoc="0" locked="0" layoutInCell="1" allowOverlap="1"><wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="column"><wp:posOffset>0</wp:posOffset></wp:positionH><wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV><wp:extent cx="100" cy="200"/><wp:effectExtent l="0" t="0" r="0" b="0"/><wp:wrapNone/><wp:docPr id="1" name="Picture 1"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic><pic:nvPicPr><pic:cNvPr id="1" name="Picture 1"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed="rId1"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="100" cy="200"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:anchor></w:drawing>"#;

/// Resolve the first drawing's hosting paragraph id + the drawing's own id, as
/// the wire op needs them.
fn drawing_addr(docx: &[u8]) -> (String, String) {
    use stemma::domain::{BlockNode, InlineNode, OpaqueKind};
    let canon = Document::parse(docx)
        .expect("parse")
        .snapshot()
        .canonical
        .clone();
    for tb in &canon.blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::OpaqueInline(o) = inline
                        && matches!(o.kind, OpaqueKind::Drawing)
                    {
                        return (p.id.to_string(), o.id.to_string());
                    }
                }
            }
        }
    }
    panic!("no drawing found");
}

/// Apply a `set_image_layout` op (given the inner `"…"` op fields) to `docx` via
/// the full wire path, in TrackedChange mode, and return the serialized output.
fn apply_layout(docx: &[u8], op_fields: &str) -> Vec<u8> {
    let (block, drawing) = drawing_addr(docx);
    let json = format!(
        r#"{{
          "ops": [{{ "op": "set_image_layout", "target": "{block}", "drawing_id": "{drawing}"{op_fields} }}],
          "revision": {{ "author": "test" }}
        }}"#
    );
    let txn = parse_transaction(&json)
        .expect("schema accepts")
        .into_edit_transaction()
        .expect("translate");
    let edited = Document::parse(docx)
        .expect("parse")
        .apply(&txn)
        .expect("apply");
    edited
        .serialize(&ExportOptions::default())
        .expect("serialize")
}

/// Crop on an inline picture: serializes, validator-clean, `a:srcRect` present,
/// media bytes untouched.
#[test]
fn crop_inline_validates_clean_and_carries_src_rect() {
    let base = docx_with_drawing(INLINE_PICTURE);
    let out = apply_layout(
        &base,
        r#", "crop": { "left": 12000, "top": 0, "right": 34000, "bottom": 0 }"#,
    );

    assert_validator_clean(&out);

    let archive = stemma::docx::DocxArchive::read(&out).expect("read out");
    let doc_xml =
        String::from_utf8(archive.get("word/document.xml").expect("doc").to_vec()).unwrap();
    assert!(
        doc_xml.contains("srcRect"),
        "a:srcRect must be present: {doc_xml}"
    );
    assert!(doc_xml.contains(r#"l="12000""#), "{doc_xml}");
    assert!(doc_xml.contains(r#"r="34000""#), "{doc_xml}");
    // The binary media part is never touched by a display-attribute edit.
    assert_eq!(
        archive.get("word/media/image1.png"),
        Some(&b"\x89PNG\r\n\x1a\n-fake-image-"[..]),
        "media bytes must be byte-identical"
    );
}

/// Position + wrap on an anchored picture: serializes, validator-clean, the new
/// attributes present and the anchor child order intact.
#[test]
fn position_wrap_anchor_validates_clean() {
    let base = docx_with_drawing(ANCHORED_PICTURE);
    let out = apply_layout(
        &base,
        r#", "position_h": { "relative_from": "page", "offset": 914400 },
            "position_v": { "relative_from": "margin", "align": "center" },
            "wrap": "square""#,
    );

    assert_validator_clean(&out);

    let archive = stemma::docx::DocxArchive::read(&out).expect("read out");
    let doc_xml =
        String::from_utf8(archive.get("word/document.xml").expect("doc").to_vec()).unwrap();
    assert!(doc_xml.contains(r#"relativeFrom="page""#), "{doc_xml}");
    assert!(doc_xml.contains("914400"));
    assert!(doc_xml.contains("center"));
    assert!(doc_xml.contains("wrapSquare"));
    assert!(
        !doc_xml.contains("wrapNone"),
        "old wrap replaced: {doc_xml}"
    );
    // Anchor child order: positionH before extent, wrap before docPr.
    let h = doc_xml.find("positionH").unwrap();
    let ext = doc_xml
        .find("<wp:extent")
        .or_else(|| doc_xml.find(":extent"))
        .unwrap();
    let wrap = doc_xml.find("wrapSquare").unwrap();
    let docpr = doc_xml.find("docPr").unwrap();
    assert!(h < ext, "positionH must precede extent: {doc_xml}");
    assert!(wrap < docpr, "wrap must precede docPr: {doc_xml}");
}

/// Direct-untracked semantics: accept-all and reject-all both reproduce the
/// EDITED document (NOT the original) — there is no tracked envelope to revert.
/// Asserted at the read-projection text/structure layer via the snapshot's
/// clean projections being identical to the edited doc.
#[test]
fn layout_edit_is_untracked_accept_equals_reject_equals_edited() {
    let base = docx_with_drawing(INLINE_PICTURE);
    let (block, drawing) = drawing_addr(&base);
    let json = format!(
        r#"{{
          "ops": [{{ "op": "set_image_layout", "target": "{block}", "drawing_id": "{drawing}",
                     "crop": {{ "left": 25000 }} }}],
          "revision": {{ "author": "test" }}
        }}"#
    );
    let txn = parse_transaction(&json)
        .expect("schema")
        .into_edit_transaction()
        .expect("translate");
    let edited = Document::parse(&base)
        .expect("parse")
        .apply(&txn)
        .expect("apply");

    // The crop landed in the edited snapshot...
    let edited_xml = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let edited_doc = String::from_utf8(
        stemma::docx::DocxArchive::read(&edited_xml)
            .unwrap()
            .get("word/document.xml")
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(
        edited_doc.contains(r#"l="25000""#),
        "crop present in edited: {edited_doc}"
    );

    // ...and survives BOTH accept-all and reject-all (untracked: no envelope).
    for (label, resolution) in [
        ("accept-all", Resolution::AcceptAll),
        ("reject-all", Resolution::RejectAll),
    ] {
        let projected = edited.project(resolution).expect("project");
        let resolved = projected
            .serialize(&ExportOptions::default())
            .expect("serialize resolved");
        let doc = String::from_utf8(
            stemma::docx::DocxArchive::read(&resolved)
                .unwrap()
                .get("word/document.xml")
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(
            doc.contains(r#"l="25000""#),
            "untracked crop must survive {label} (accept==reject==edited): {doc}"
        );
    }
}
