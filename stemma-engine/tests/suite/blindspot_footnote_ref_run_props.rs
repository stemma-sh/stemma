//! Regression gate: the footnote/endnote auto-number reference run
//! (`w:footnoteRef` §17.11.6 / `w:endnoteRef` §17.11.1) must retain its host
//! run's `w:rPr` when the engine rebuilds the note story.
//!
//! Word renders the auto-number inside a footnote body via a run whose `w:rPr`
//! carries the note-reference character style plus fonts/size, e.g.
//! `<w:r><w:rPr><w:rStyle w:val="Refdenotaalpie"/><w:rFonts w:ascii="Arial"
//! w:hAnsi="Arial" w:cs="Arial"/><w:sz w:val="16"/><w:szCs w:val="16"/></w:rPr>
//! <w:footnoteRef/></w:r>`. The importer models `w:footnoteRef` as a zero-width
//! decoration whose `raw_xml` is only the bare marker; the run's `rPr` lived on
//! the parent run and must be captured on the decoration and re-synthesized
//! around the marker on serialization. Before the fix the serializer emitted a
//! bare `<w:r><w:footnoteRef/></w:r>` and the rPr was silently dropped on EVERY
//! rebuild of the story — a text-invisible fidelity loss (no `w:t` content, so
//! text-stream comparisons never see it).
//!
//! Corpus-free: the document (with a footnotes.xml part) is synthesized here.

use stemma::ExportOptions;
use stemma::api::Document;
use stemma::docx::DocxArchive;
use stemma::domain::{Alignment, BlockNode, CanonDoc, NodeId, RevisionInfo};
use stemma::edit::{EditStep, EditTransaction, MaterializationMode, ParagraphFormattingPatch};

/// The footnote reference run's rPr, verbatim, as Word authored it.
const REF_RPR: &str = concat!(
    r#"<w:rPr>"#,
    r#"<w:rStyle w:val="Refdenotaalpie"/>"#,
    r#"<w:rFonts w:ascii="Arial" w:hAnsi="Arial" w:cs="Arial"/>"#,
    r#"<w:sz w:val="16"/>"#,
    r#"<w:szCs w:val="16"/>"#,
    r#"</w:rPr>"#,
);

/// Build a minimal `.docx` with one body paragraph that references footnote #1,
/// and a `word/footnotes.xml` whose content footnote carries the styled
/// `w:footnoteRef` reference run above.
fn make_docx_with_footnote() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t xml:space="preserve">Body paragraph with a note.</w:t></w:r><w:r><w:rPr><w:rStyle w:val="Refdenotaalpie"/></w:rPr><w:footnoteReference w:id="1"/></w:r></w:p><w:sectPr/></w:body></w:document>"#;

    let footnotes_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote><w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote><w:footnote w:id="1"><w:p><w:r>{REF_RPR}<w:footnoteRef/></w:r><w:r><w:t xml:space="preserve"> The footnote body text.</w:t></w:r></w:p></w:footnote></w:footnotes>"#
    );

    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/footnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdFn" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes" Target="footnotes.xml"/></Relationships>"#;

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
        zip.start_file("word/footnotes.xml", opts).unwrap();
        zip.write_all(footnotes_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

fn first_block_id(doc: &CanonDoc) -> NodeId {
    match &doc.blocks[0].block {
        BlockNode::Paragraph(p) => p.id.clone(),
        other => panic!("expected paragraph, got {other:?}"),
    }
}

/// A Direct-mode body edit unrelated to the footnote must not perturb the
/// footnote's reference-run rPr: the rebuilt `word/footnotes.xml` must still
/// carry the rStyle / rFonts / sz on the `w:footnoteRef` run, byte-faithfully.
#[test]
fn footnote_ref_run_retains_rpr_after_body_edit() {
    let base = Document::parse(&make_docx_with_footnote()).expect("parse");
    let block_id = first_block_id(&base.snapshot().canonical);

    let txn = EditTransaction {
        steps: vec![EditStep::SetParagraphFormatting {
            block_id,
            semantic_hash: None,
            patch: ParagraphFormattingPatch {
                align: Some(Alignment::Center),
                ..ParagraphFormattingPatch::default()
            },
            rationale: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::Direct,
        revision: RevisionInfo {
            revision_id: 1,
            author: Some("Editor".to_string()),
            date: Some("2026-07-09T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    };

    let edited = base.apply(&txn).expect("apply body edit");
    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");

    let archive = DocxArchive::read(&bytes).expect("read exported docx");
    let raw = String::from_utf8(
        archive
            .get("word/footnotes.xml")
            .expect("footnotes.xml")
            .to_vec(),
    )
    .expect("footnotes.xml utf8");
    // Normalize the serializer's self-closing convention (`<w:x />`) to the
    // compact form so the byte comparison is about property fidelity, not
    // empty-element whitespace.
    let footnotes_xml = raw.replace(" />", "/>");

    // The auto-number reference run must still carry its full rPr. The whole rPr
    // block, byte-verbatim, must sit immediately before the footnoteRef marker.
    let expected_run = format!("{REF_RPR}<w:footnoteRef/>");
    assert!(
        footnotes_xml.contains(&expected_run),
        "footnoteRef run lost its rPr on story rebuild.\n\
         expected to contain:\n  {expected_run}\n\
         got footnotes.xml:\n{footnotes_xml}"
    );

    // Redundant explicit witnesses for the individual properties the bug drops,
    // so a partial regression names which property was lost.
    for needle in [
        r#"<w:rStyle w:val="Refdenotaalpie"/>"#,
        r#"<w:rFonts w:ascii="Arial" w:hAnsi="Arial" w:cs="Arial"/>"#,
        r#"<w:sz w:val="16"/>"#,
        r#"<w:szCs w:val="16"/>"#,
    ] {
        assert!(
            footnotes_xml.contains(needle),
            "footnoteRef run lost {needle}\nfootnotes.xml:\n{footnotes_xml}"
        );
    }
}
