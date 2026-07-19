//! An `xml:space="preserve"` declared on the DOCUMENT ROOT (`<w:document>`) must
//! survive the rebuild.
//!
//! DOMAIN RULE: `xml:space` (XML 1.0 §2.10) is legal on any element and is
//! INHERITED by descendants. Some generators declare it once on the story root
//! and then write space-only runs as bare `<w:t> </w:t>` — no per-element
//! `xml:space` — relying on the inherited value. Word honours that inheritance.
//!
//! Our streaming rebuild re-emits the root and (before this fix) dropped the
//! root attribute, stamping per-run `xml:space="preserve"` only on the runs the
//! IR models. OPAQUE interiors (a textbox's `w:txbxContent`, other verbatim
//! `raw_xml`) are carried byte-verbatim; their bare `<w:t> </w:t>` runs relied
//! on the inherited root attribute. Drop the root attribute and Word strips
//! those spaces — silent text corruption inside every verbatim-preserved region
//! (witnessed: a textbox "Proposed Rate(s):" collapsing to "ProposedRate(s):").
//!
//! Assertion (a) — the output `<w:document>` still carries
//! `xml:space="preserve"` — is the load-bearing witness: it is exactly what a
//! downstream XML consumer (Word) needs to keep the inherited spaces. It fails
//! without the fix. Assertion (b) confirms the opaque bytes are intact and the
//! engine reads the interior text with its space.

use std::io::Write;

use stemma::ExportOptions;
use stemma::api::Document;
use stemma::domain::RevisionInfo;
use stemma::edit::{
    BlockSpec, ContentFragment, EditStep, EditTransaction, InsertPosition, MaterializationMode,
    ParagraphBlockSpec, ParagraphContent,
};
use stemma::opaque_targets::OpaqueTextTargetKind;
use zip::write::FileOptions;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const WP_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing";
const A_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const WPS_NS: &str = "http://schemas.microsoft.com/office/word/2010/wordprocessingShape";

/// Build a DOCX whose `<w:document>` root carries `xml:space="preserve"` and
/// whose textbox interior (opaque `raw_xml`) contains a bare `<w:t> </w:t>`
/// between two words — legal markup that relies on the inherited root attribute.
fn make_docx(body_inner: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W_NS}" xmlns:wp="{WP_NS}" xmlns:a="{A_NS}" xmlns:wps="{WPS_NS}" xml:space="preserve"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        for (name, data) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/document.xml", document_xml.as_str()),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

/// A body paragraph (the unrelated edit anchor) followed by a paragraph that
/// hosts a DrawingML textbox. The textbox interior splits "Proposed Rate(s):"
/// across three runs, the middle one a bare `<w:t> </w:t>` with NO per-run
/// `xml:space` — the space is significant only via the inherited root attribute.
fn body_with_textbox() -> String {
    let textbox_para = r#"<w:p><w:r><w:t>Proposed</w:t></w:r><w:r><w:t> </w:t></w:r><w:r><w:t>Rate(s):</w:t></w:r></w:p>"#;
    format!(
        r#"<w:p><w:r><w:t>Body anchor</w:t></w:r></w:p><w:p><w:r><w:drawing><wp:inline><wp:extent cx="100" cy="100"/><wp:docPr id="1" name="TextBox 1"/><a:graphic><a:graphicData uri="{WPS_NS}"><wps:wsp><wps:txbx><w:txbxContent>{textbox_para}</w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#
    )
}

#[test]
fn root_xml_space_preserve_survives_rebuild_and_keeps_textbox_space() {
    let docx = make_docx(&body_with_textbox());
    let doc = Document::parse(&docx).expect("parse");

    // An unrelated tracked body edit — insert a paragraph after the first body
    // block. It touches nothing in the textbox but forces the full serialize
    // path (`write_ooxml_root_start` for word/document.xml).
    let anchor = doc.read().blocks[0].id.clone();
    let txn = EditTransaction {
        steps: vec![EditStep::InsertParagraphs {
            anchor_block_id: anchor,
            position: InsertPosition::After,
            rationale: None,
            blocks: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
                role: Some("default".to_string()),
                content: ParagraphContent {
                    fragments: vec![ContentFragment::Text("Inserted".to_string())],
                },
                restart_numbering: false,
                list: None,
            })],
        }],
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("Gate".to_string()),
            date: Some("2026-07-09T00:00:00Z".to_string()),
            apply_op_id: None,
        },
        summary: Some("unrelated body edit".to_string()),
    };
    let edited = doc.apply(&txn).expect("apply");
    let out = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");

    let archive = stemma::docx::DocxArchive::read(&out).expect("read output");
    let xml = String::from_utf8(
        archive
            .get("word/document.xml")
            .expect("document.xml")
            .to_vec(),
    )
    .expect("utf-8");

    // (a) The root element still carries `xml:space="preserve"`. Inspect only the
    // opening `<w:document …>` tag so a per-run `xml:space` elsewhere can't
    // masquerade as the root attribute.
    let open_tag_end = xml
        .find("<w:document")
        .and_then(|start| xml[start..].find('>').map(|rel| start + rel));
    let open_tag = open_tag_end.map(|end| &xml[..end]).unwrap_or(&xml);
    assert!(
        open_tag.contains(r#"xml:space="preserve""#),
        "the <w:document> root must keep its xml:space=\"preserve\" — dropping it \
         makes Word treat inherited-preserve spaces (e.g. the bare <w:t> </w:t> \
         inside the textbox) as insignificant; opening tag: {open_tag}"
    );

    // (b) The opaque textbox bytes are intact: the bare space-only run survives.
    assert!(
        xml.contains(r#"<w:t> </w:t>"#),
        "the byte-verbatim textbox interior must still carry the bare \
         <w:t> </w:t> run; document.xml: {xml}"
    );

    // (b, cont.) Reparse the output and read the textbox text through the engine's
    // opaque-interior discovery: it must read "Proposed Rate(s):" WITH the space.
    let reparsed = Document::parse(&out).expect("reparse output");
    let textbox_texts: Vec<String> = reparsed
        .opaque_text_targets()
        .into_iter()
        .filter(|t| t.kind == OpaqueTextTargetKind::TextboxParagraph)
        .map(|t| t.text)
        .collect();
    assert_eq!(
        textbox_texts,
        vec!["Proposed Rate(s):".to_string()],
        "the textbox interior text must read WITH the inherited space"
    );
}
