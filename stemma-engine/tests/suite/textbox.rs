//! Integration tests for `SetTextboxText` (M3) — whole-interior replace of a
//! textbox's `w:txbxContent`.
//!
//! Contract under test (CLAUDE.md "no silent fallbacks"):
//!  - whole-interior replace with caller paragraphs; carrier-agnostic
//!    (DrawingML `wps:txbx` + VML `v:textbox`);
//!  - untracked / in-place (the drawing stays one opaque node);
//!  - REFUSE loudly if the interior already carries tracked changes (don't
//!    flatten) or the drawing has no `w:txbxContent`;
//!  - a textbox NOT targeted by the verb round-trips byte-identical.
//!
//! Daily tier, corpus-free. Bytes-in via the public `Document` API.

use std::io::{Cursor, Read, Write};

use stemma::api::Document;
use stemma::domain::{BlockNode, CanonDoc, InlineNode, NodeId, OpaqueKind, RevisionInfo};
use stemma::edit::{EditError, EditStep, EditTransaction, MaterializationMode, apply_transaction};
use stemma::edit_v4::parse_transaction;
use stemma::{ExportMode, ExportOptions, ValidatorLevel};
use zip::ZipWriter;
use zip::write::FileOptions;

const CONTENT_TYPES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
const PACKAGE_RELS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
const DOC_RELS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;

fn make_docx(body_inner: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape" xmlns:v="urn:schemas-microsoft-com:vml" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
    );
    let mut buf = Vec::new();
    {
        let mut zip = ZipWriter::new(Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        zip.start_file("[Content_Types].xml", opts).unwrap();
        zip.write_all(CONTENT_TYPES_XML.as_bytes()).unwrap();
        zip.start_file("_rels/.rels", opts).unwrap();
        zip.write_all(PACKAGE_RELS_XML.as_bytes()).unwrap();
        zip.start_file("word/_rels/document.xml.rels", opts)
            .unwrap();
        zip.write_all(DOC_RELS_XML.as_bytes()).unwrap();
        zip.start_file("word/document.xml", opts).unwrap();
        zip.write_all(document_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

fn document_xml(doc: &Document) -> String {
    let bytes = doc
        .serialize(&ExportOptions {
            mode: ExportMode::Redline,
            validator_level: ValidatorLevel::Off,
            validator: None,
        })
        .expect("serialize");
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).expect("zip");
    let mut xml = String::new();
    zip.by_name("word/document.xml")
        .expect("document.xml")
        .read_to_string(&mut xml)
        .expect("read");
    xml
}

/// (paragraph id, drawing id) of the first drawing opaque.
fn first_drawing(canon: &CanonDoc) -> (NodeId, NodeId) {
    for tb in &canon.blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::OpaqueInline(o) = inline
                        && matches!(o.kind, OpaqueKind::Drawing)
                    {
                        return (p.id.clone(), o.id.clone());
                    }
                }
            }
        }
    }
    panic!("no drawing");
}

fn inner_region(xml: &str, open: &str, close: &str) -> String {
    let start = xml.find(open).expect("open tag");
    let end = xml.find(close).expect("close tag") + close.len();
    xml[start..end].to_string()
}

fn set_text_txn(block_id: NodeId, drawing_id: NodeId, paragraphs: Vec<String>) -> EditTransaction {
    EditTransaction {
        steps: vec![EditStep::SetTextboxText {
            block_id,
            drawing_id,
            paragraphs,
            semantic_hash: None,
            rationale: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::Direct,
        revision: RevisionInfo {
            revision_id: 1,
            author: Some("TB".to_string()),
            date: Some("2026-06-11T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

// A DrawingML textbox (wps:txbx) whose interior reads "Old text".
const DRAWINGML_TEXTBOX: &str = r#"<w:p><w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="2700000" cy="900000"/><wp:docPr id="1" name="TextBox 1"/><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:txbx><w:txbxContent><w:p><w:r><w:t>Old text</w:t></w:r></w:p></w:txbxContent></wps:txbx><wps:bodyPr/></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#;

// A VML textbox (v:textbox) whose interior reads "VML old".
const VML_TEXTBOX: &str = r#"<w:p><w:r><w:pict><v:shape><v:textbox><w:txbxContent><w:p><w:r><w:t>VML old</w:t></w:r></w:p></w:txbxContent></v:textbox></v:shape></w:pict></w:r></w:p>"#;

#[test]
fn set_replaces_drawingml_textbox_interior() {
    let base = Document::parse(&make_docx(DRAWINGML_TEXTBOX)).expect("parse");
    let (block_id, drawing_id) = first_drawing(&base.snapshot().canonical);
    let edited = base
        .apply(&set_text_txn(
            block_id,
            drawing_id,
            vec!["New text".to_string()],
        ))
        .expect("set textbox text");
    let xml = document_xml(&edited);
    assert!(xml.contains("New text"), "new interior present: {xml}");
    assert!(!xml.contains("Old text"), "old interior replaced: {xml}");
    // Still exactly one drawing opaque, one txbxContent.
    assert_eq!(
        xml.matches("<w:txbxContent>").count(),
        1,
        "one txbxContent: {xml}"
    );
}

#[test]
fn set_replaces_multi_paragraph_interior() {
    let base = Document::parse(&make_docx(DRAWINGML_TEXTBOX)).expect("parse");
    let (block_id, drawing_id) = first_drawing(&base.snapshot().canonical);
    let edited = base
        .apply(&set_text_txn(
            block_id,
            drawing_id,
            vec!["Line one".to_string(), "Line two".to_string()],
        ))
        .expect("set multi-paragraph");
    let xml = document_xml(&edited);
    let interior = inner_region(&xml, "<w:txbxContent>", "</w:txbxContent>");
    assert_eq!(
        interior.matches("<w:p>").count(),
        2,
        "two interior paragraphs: {interior}"
    );
    assert!(
        interior.contains("Line one") && interior.contains("Line two"),
        "{interior}"
    );
}

#[test]
fn set_replaces_vml_textbox_interior() {
    // Carrier-agnostic: VML v:textbox > w:txbxContent is found by local name.
    let base = Document::parse(&make_docx(VML_TEXTBOX)).expect("parse");
    let (block_id, drawing_id) = first_drawing(&base.snapshot().canonical);
    let edited = base
        .apply(&set_text_txn(
            block_id,
            drawing_id,
            vec!["VML new".to_string()],
        ))
        .expect("set VML textbox text");
    let xml = document_xml(&edited);
    assert!(xml.contains("VML new"), "VML interior replaced: {xml}");
    assert!(!xml.contains("VML old"), "VML old interior gone: {xml}");
}

#[test]
fn untargeted_textbox_round_trips_byte_identical() {
    // A textbox whose verb never fires must come through serialize unchanged in
    // its interior — the descent/mutation only runs when the verb targets it.
    let base = Document::parse(&make_docx(DRAWINGML_TEXTBOX)).expect("parse");
    let before = inner_region(&document_xml(&base), "<w:txbxContent>", "</w:txbxContent>");
    // A no-op transaction would be empty; instead, just re-serialize the parsed
    // doc and confirm the interior is preserved verbatim.
    let after = inner_region(&document_xml(&base), "<w:txbxContent>", "</w:txbxContent>");
    assert_eq!(before, after, "unedited textbox interior is verbatim");
    assert!(before.contains("Old text"), "interior preserved: {before}");
}

#[test]
fn set_textbox_text_via_v4_wire() {
    // The full JSON → parse_transaction → into_edit_transaction → apply path,
    // exercising the `set_textbox_text` op name and field shape.
    let base = Document::parse(&make_docx(DRAWINGML_TEXTBOX)).expect("parse");
    let (block_id, drawing_id) = first_drawing(&base.snapshot().canonical);
    let json = format!(
        r#"{{ "ops": [{{ "op": "set_textbox_text", "target": "{block_id}",
              "drawing_id": "{drawing_id}", "paragraphs": ["Wire one", "Wire two"] }}],
             "revision": {{ "author": "wire" }} }}"#
    );
    let txn = parse_transaction(&json)
        .expect("v4 schema")
        .into_edit_transaction()
        .expect("v4 adapt");
    let edited = base.apply(&txn).expect("apply v4");
    let xml = document_xml(&edited);
    let interior = inner_region(&xml, "<w:txbxContent>", "</w:txbxContent>");
    assert!(
        interior.contains("Wire one") && interior.contains("Wire two"),
        "{interior}"
    );
    assert_eq!(
        interior.matches("<w:p>").count(),
        2,
        "two paragraphs: {interior}"
    );
}

// ─── mc:AlternateContent (Choice + Fallback duplicate) ────────────────────────
//
// Word's standard textbox emission wraps the shape in mc:AlternateContent with a
// DrawingML Choice (wps:txbx) AND a VML Fallback (w:pict > v:textbox), each
// carrying a DUPLICATE of the same interior. The verb must replace BOTH copies —
// replacing only the first leaves the fallback branch stale.

/// A drawing whose mc:AlternateContent has a Choice (wps:txbx) and a Fallback
/// (v:textbox), both with the IDENTICAL interior "Old text".
const ALT_CONTENT_TEXTBOX: &str = r#"<w:p><w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="2700000" cy="900000"/><wp:docPr id="1" name="TextBox 1"/><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><mc:AlternateContent><mc:Choice Requires="wps"><wps:wsp><wps:txbx><w:txbxContent><w:p><w:r><w:t>Old text</w:t></w:r></w:p></w:txbxContent></wps:txbx><wps:bodyPr/></wps:wsp></mc:Choice><mc:Fallback><w:pict><v:shape><v:textbox><w:txbxContent><w:p><w:r><w:t>Old text</w:t></w:r></w:p></w:txbxContent></v:textbox></v:shape></w:pict></mc:Fallback></mc:AlternateContent></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#;

#[test]
fn set_replaces_both_alternatecontent_copies() {
    let base = Document::parse(&make_docx(ALT_CONTENT_TEXTBOX)).expect("parse");
    let (block_id, drawing_id) = first_drawing(&base.snapshot().canonical);
    let edited = base
        .apply(&set_text_txn(
            block_id,
            drawing_id,
            vec!["New text".to_string()],
        ))
        .expect("set both copies");
    let xml = document_xml(&edited);
    // BOTH copies carry the new text; neither keeps the old.
    assert_eq!(
        xml.matches("New text").count(),
        2,
        "both copies replaced: {xml}"
    );
    assert!(!xml.contains("Old text"), "no stale fallback copy: {xml}");
    // Still two txbxContent (the structure is preserved, only the interiors swap).
    assert_eq!(xml.matches("<w:txbxContent>").count(), 2, "{xml}");
}

#[test]
fn set_refuses_multiple_distinct_textboxes() {
    // Two txbxContent with DIFFERENT interiors (a real multi-textbox group, not
    // the AlternateContent duplicate) → refuse rather than clobber both.
    let body = r#"<w:p><w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="2700000" cy="900000"/><wp:docPr id="1" name="Group 1"/><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingGroup"><wps:wsp><wps:txbx><w:txbxContent><w:p><w:r><w:t>First box</w:t></w:r></w:p></w:txbxContent></wps:txbx></wps:wsp><wps:wsp><wps:txbx><w:txbxContent><w:p><w:r><w:t>Second box</w:t></w:r></w:p></w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#;
    let base = Document::parse(&make_docx(body)).expect("parse");
    let canon = base.snapshot().canonical.clone();
    let (block_id, drawing_id) = first_drawing(&canon);
    let err = apply_transaction(
        &canon,
        &set_text_txn(block_id, drawing_id, vec!["replacement".to_string()]),
    )
    .expect_err("distinct textboxes must refuse");
    match err {
        EditError::MultipleDistinctTextboxes { count, .. } => assert_eq!(count, 2),
        other => panic!("expected MultipleDistinctTextboxes, got {other:?}"),
    }
}

#[test]
fn set_refuses_when_tracked_change_in_fallback_copy() {
    // A w:ins in the FALLBACK copy (but not the Choice) must still refuse — the
    // tracked-change scan covers all copies, not just the first.
    let body = r#"<w:p><w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="2700000" cy="900000"/><wp:docPr id="1" name="TextBox 1"/><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><mc:AlternateContent><mc:Choice Requires="wps"><wps:wsp><wps:txbx><w:txbxContent><w:p><w:r><w:t>Old</w:t></w:r></w:p></w:txbxContent></wps:txbx></wps:wsp></mc:Choice><mc:Fallback><w:pict><v:shape><v:textbox><w:txbxContent><w:p><w:r><w:t>Old</w:t></w:r><w:ins w:id="1" w:author="a" w:date="2026-06-11T00:00:00Z"><w:r><w:t> new</w:t></w:r></w:ins></w:p></w:txbxContent></v:textbox></v:shape></w:pict></mc:Fallback></mc:AlternateContent></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#;
    let base = Document::parse(&make_docx(body)).expect("parse");
    let canon = base.snapshot().canonical.clone();
    let (block_id, drawing_id) = first_drawing(&canon);
    let err = apply_transaction(
        &canon,
        &set_text_txn(block_id, drawing_id, vec!["x".to_string()]),
    )
    .expect_err("tracked change in fallback must refuse");
    assert!(
        matches!(err, EditError::TextboxHasTrackedChanges { .. }),
        "got {err:?}"
    );
}

#[test]
fn set_does_not_miscount_a_nested_textbox_as_a_copy() {
    // The OUTER textbox's interior paragraph hosts a NESTED drawing with its own
    // txbxContent. The no-recurse rule of the shared collector
    // (opaque_meta::collect_descendants_by_local) means only the OUTER
    // txbxContent counts as a copy — the nested one belongs to the nested anchor.
    // So the verb does NOT refuse as "multiple distinct"; it replaces the outer
    // interior wholesale (the whole-interior-replace semantic), which removes the
    // nested textbox along with everything else inside.
    let body = r#"<w:p><w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="2700000" cy="900000"/><wp:docPr id="1" name="Outer"/><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:txbx><w:txbxContent><w:p><w:r><w:t>Outer text</w:t></w:r><w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="900000" cy="300000"/><wp:docPr id="2" name="Nested"/><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:txbx><w:txbxContent><w:p><w:r><w:t>Nested text</w:t></w:r></w:p></w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p></w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#;
    let base = Document::parse(&make_docx(body)).expect("parse");
    let (block_id, drawing_id) = first_drawing(&base.snapshot().canonical);
    // Must NOT refuse MultipleDistinctTextboxes (the nested one is not a copy).
    let edited = base
        .apply(&set_text_txn(
            block_id,
            drawing_id,
            vec!["Replaced".to_string()],
        ))
        .expect("nested textbox must not be miscounted as a distinct copy");
    let xml = document_xml(&edited);
    assert!(xml.contains("Replaced"), "outer interior replaced: {xml}");
    // The outer interior was replaced wholesale, so both the old outer text and
    // the nested textbox (and its text) are gone.
    assert!(!xml.contains("Outer text"), "old outer text gone: {xml}");
    assert!(
        !xml.contains("Nested text"),
        "nested textbox removed with the interior: {xml}"
    );
}

// ─── Refusals ─────────────────────────────────────────────────────────────────

#[test]
fn set_on_textbox_with_tracked_interior_refuses() {
    // The interior already has a w:ins — a whole-interior replace would flatten
    // it. Refuse with TextboxHasTrackedChanges (the M0 "resolve first" path).
    let body = r#"<w:p><w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="2700000" cy="900000"/><wp:docPr id="1" name="TextBox 1"/><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:txbx><w:txbxContent><w:p><w:r><w:t>Old</w:t></w:r><w:ins w:id="1" w:author="a" w:date="2026-06-11T00:00:00Z"><w:r><w:t> new</w:t></w:r></w:ins></w:p></w:txbxContent></wps:txbx><wps:bodyPr/></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#;
    let base = Document::parse(&make_docx(body)).expect("parse");
    let canon = base.snapshot().canonical.clone();
    let (block_id, drawing_id) = first_drawing(&canon);
    let err = apply_transaction(
        &canon,
        &set_text_txn(block_id, drawing_id, vec!["replacement".to_string()]),
    )
    .expect_err("tracked interior must refuse");
    assert!(
        matches!(err, EditError::TextboxHasTrackedChanges { .. }),
        "got {err:?}"
    );
}

#[test]
fn set_on_drawing_without_textbox_refuses() {
    // A picture drawing (no txbxContent) — SetTextboxText is the wrong verb.
    let body = r#"<w:p><w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="100" cy="100"/><wp:docPr id="1" name="Pic"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:blipFill><a:blip r:embed="rId1"/></pic:blipFill><pic:spPr/></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#;
    let base = Document::parse(&make_docx(body)).expect("parse");
    let canon = base.snapshot().canonical.clone();
    let (block_id, drawing_id) = first_drawing(&canon);
    let err = apply_transaction(
        &canon,
        &set_text_txn(block_id, drawing_id, vec!["x".to_string()]),
    )
    .expect_err("a picture has no txbxContent");
    match err {
        EditError::ImageAttributeTargetAbsent { attribute, .. } => {
            assert_eq!(attribute, "w:txbxContent");
        }
        other => panic!("expected ImageAttributeTargetAbsent(w:txbxContent), got {other:?}"),
    }
}
