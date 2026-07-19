//! Daily spec test for `ApplyStyle`: `w:pStyle` element ordering (ECMA-376
//! §17.3.1.27, CT_PPr / CT_PPrBase content model, Annex A).
//!
//! `w:pStyle` is the FIRST child of `w:pPr` (CT_PPrBase position 0). When a
//! paragraph carries a tracked style change, two `w:pPr` contexts exist:
//!   - the LIVE `w:pPr` holds the NEW `w:pStyle` (position 0);
//!   - the `w:pPrChange`'s inner `w:pPr` holds the PREVIOUS pPr — and if the
//!     prior paragraph had a style, its `w:pStyle` is also at position 0 there.
//!
//! This test pins both. The prior-style case uses a paragraph that already has
//! a style so the inner `w:pPr` carries a `w:pStyle` to position-check.

use stemma::ExportOptions;
use stemma::api::Document;
use stemma::domain::*;
use stemma::edit::*;

fn make_docx(style: Option<&str>) -> Vec<u8> {
    let pstyle = match style {
        Some(s) => format!(r#"<w:pPr><w:pStyle w:val="{s}"/></w:pPr>"#),
        None => String::new(),
    };
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p>{pstyle}<w:r><w:t>Clause text.</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#
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

fn document_xml(docx: &[u8]) -> String {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(docx)).expect("zip");
    let mut file = zip.by_name("word/document.xml").expect("document.xml");
    use std::io::Read;
    let mut s = String::new();
    file.read_to_string(&mut s).expect("read");
    s
}

fn apply_style_tracked(base: Document, style_id: &str) -> Vec<u8> {
    let id = base.read().blocks[0].id.to_string();
    let txn = EditTransaction {
        steps: vec![EditStep::ApplyStyle {
            block_id: NodeId::from(id.as_str()),
            semantic_hash: None,
            style_id: style_id.to_string(),
            rationale: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("Spec".to_string()),
            date: Some("2026-06-01T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    };
    base.apply(&txn)
        .expect("apply")
        .serialize(&ExportOptions::default())
        .expect("serialize")
}

/// The live `w:pStyle` for the new style is at `w:pPr` position 0 (it precedes
/// any other live pPr child and the `w:pPrChange`).
#[test]
fn new_pstyle_is_first_in_live_ppr() {
    let base = Document::parse(&make_docx(None)).expect("parse");
    let xml = document_xml(&apply_style_tracked(base, "Heading2"));

    // The new pStyle must appear, with val=Heading2 (serializer self-closes
    // with a trailing space: `<w:pStyle w:val="Heading2" />`).
    assert!(
        xml.contains(r#"<w:pStyle w:val="Heading2""#),
        "live pStyle for the new style must be emitted; got:\n{xml}"
    );

    // pStyle precedes pPrChange in the live pPr (position 0 vs position 35).
    let pstyle_pos = xml.find(r#"<w:pStyle w:val="Heading2""#).unwrap();
    let ppr_change_pos = xml.find("<w:pPrChange").expect("pPrChange present");
    assert!(
        pstyle_pos < ppr_change_pos,
        "live w:pStyle (pos 0) must precede w:pPrChange (pos 35)"
    );

    // It is the FIRST child of the live <w:pPr>: pStyle immediately follows the
    // opening <w:pPr> (no intervening element).
    let ppr_open = xml.find("<w:pPr>").or_else(|| xml.find("<w:pPr ")).unwrap();
    let after_open = &xml[ppr_open..];
    let first_child = after_open
        .find("<w:")
        .and_then(|i| after_open[i + 1..].find("<w:").map(|j| i + 1 + j))
        .unwrap();
    assert!(
        after_open[first_child..].starts_with("<w:pStyle"),
        "the first child of the live w:pPr must be w:pStyle; got:\n{}",
        &after_open[first_child
            ..first_child.min(after_open.len()).max(first_child)
                + 40.min(after_open.len() - first_child)]
    );
}

/// The previous-style `w:pStyle` is at position 0 inside the `w:pPrChange`'s
/// inner `w:pPr` (the complete prior pPr snapshot, §17.13.5.29).
#[test]
fn previous_pstyle_is_first_in_pprchange_inner_ppr() {
    // Base paragraph already has a style → the prior pPr snapshot carries it.
    let base = Document::parse(&make_docx(Some("Heading1"))).expect("parse");
    let xml = document_xml(&apply_style_tracked(base, "Heading2"));

    // Locate the pPrChange and its inner pPr.
    let change_pos = xml.find("<w:pPrChange").expect("pPrChange present");
    let inner = &xml[change_pos..];
    // The inner pPr opens after the pPrChange start tag.
    let inner_ppr = inner
        .find("<w:pPr>")
        .or_else(|| inner.find("<w:pPr "))
        .expect("inner pPr present in pPrChange");
    let after_inner_open = &inner[inner_ppr..];
    // First child element inside the inner pPr.
    let first_child = after_inner_open
        .find("<w:")
        .and_then(|i| after_inner_open[i + 1..].find("<w:").map(|j| i + 1 + j))
        .unwrap();
    assert!(
        after_inner_open[first_child..].starts_with("<w:pStyle"),
        "the first child of the pPrChange inner w:pPr must be w:pStyle; got:\n{}",
        &after_inner_open[first_child..(first_child + 60).min(after_inner_open.len())]
    );
    // And it carries the PREVIOUS style value (serializer self-closes with a
    // trailing space).
    assert!(
        after_inner_open[first_child..].starts_with(r#"<w:pStyle w:val="Heading1""#),
        "pPrChange inner pStyle must record the previous style (Heading1); got:\n{}",
        &after_inner_open[first_child..(first_child + 60).min(after_inner_open.len())]
    );
}
