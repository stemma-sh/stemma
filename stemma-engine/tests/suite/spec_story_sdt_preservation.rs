//! Story-part block content controls survive the edit-path rebuild (§17.5.2).
//!
//! DOMAIN RULE: header/footer parts are rebuilt wholesale from the IR whenever
//! any edit is applied to the document. A `w:sdt` wrapping story blocks —
//! Word's page-number gallery (`w:docPartObj`/`w:docPartGallery`) is the
//! canonical real-world case — is part of the document's content model. The
//! rebuild must re-emit the envelope (`w:sdtPr` verbatim, `w:sdtContent`
//! enclosing the same blocks), not silently unwrap it (CLAUDE.md "no silent
//! fallbacks"). Content alone surviving is not enough: the envelope is what
//! makes Word treat the range as a content control.
//!
//! The wrapper rides the same model as `WrapBlocksInContentControl`
//! (`TrackedBlock::block_sdt_wrap`), so accept/reject semantics are inherited:
//! SDT structure is untracked and survives resolution unchanged.

use std::io::Write;

use stemma::ExportOptions;
use stemma::RevisionInfo;
use stemma::api::Document;
use stemma::edit::{EditStep, EditTransaction, MaterializationMode};
use zip::write::FileOptions;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

/// A footer part whose content is wrapped in Word's page-number gallery SDT.
fn page_number_footer(inner_blocks: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:ftr xmlns:w="{W_NS}"><w:sdt><w:sdtPr><w:id w:val="123456"/><w:docPartObj><w:docPartGallery w:val="Page Numbers (Bottom of Page)"/><w:docPartUnique/></w:docPartObj></w:sdtPr><w:sdtContent>{inner_blocks}</w:sdtContent></w:sdt></w:ftr>"#
    )
}

const PAGE_FIELD_PARA: &str =
    r#"<w:p><w:fldSimple w:instr=" PAGE "><w:r><w:t>1</w:t></w:r></w:fldSimple></w:p>"#;

/// Body doc + footer part + footerReference (mirrors
/// blindspot_editfooter_story.rs::make_footer_docx).
fn make_docx_with_footer(footer_xml: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W_NS}" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>
<w:p><w:r><w:t>Body paragraph.</w:t></w:r></w:p>
<w:sectPr>
<w:footerReference w:type="default" r:id="rIdF1"/>
<w:pgSz w:w="12240" w:h="15840"/>
</w:sectPr>
</w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdF1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/></Relationships>"#;

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
        zip.start_file("word/footer1.xml", opts).unwrap();
        zip.write_all(footer_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

/// Any edit forces the story-part rebuild (an un-edited export returns the
/// original bytes and proves nothing — see roundtrip_fidelity.rs module docs).
fn reserialize_trigger() -> EditTransaction {
    EditTransaction {
        steps: vec![EditStep::SetDocDefaults {
            font_family: Some("Calibri".to_string()),
            font_size_half_points: None,
            rationale: Some("story sdt reserialize trigger".to_string()),
        }],
        materialization_mode: MaterializationMode::Direct,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("fid".into()),
            date: Some("2026-07-02T00:00:00Z".into()),
            apply_op_id: None,
        },
        summary: Some("reserialize trigger".to_string()),
    }
}

fn part_of(docx: &[u8], part: &str) -> String {
    let archive = stemma::docx::DocxArchive::read(docx).expect("read docx archive");
    let bytes = archive
        .get(part)
        .unwrap_or_else(|| panic!("{part} present"));
    String::from_utf8(bytes.to_vec()).expect("part utf-8")
}

fn edit_and_serialize(docx: &[u8]) -> Vec<u8> {
    let doc = Document::parse(docx).expect("parse");
    let edited = doc.apply(&reserialize_trigger()).expect("apply trigger");
    edited
        .serialize(&ExportOptions::default())
        .expect("serialize")
}

fn assert_footer_keeps_gallery_envelope(footer_xml: &str) {
    assert_eq!(
        footer_xml.matches("<w:sdt>").count(),
        1,
        "footer must carry exactly one content-control envelope; footer1.xml: {footer_xml}"
    );
    assert!(
        footer_xml.contains("Page Numbers (Bottom of Page)"),
        "the page-number gallery sdtPr (docPartGallery) must survive the story rebuild \
         verbatim; footer1.xml: {footer_xml}"
    );
    let sdt_content = footer_xml
        .find("<w:sdtContent>")
        .expect("sdtContent present");
    let sdt_close = footer_xml
        .find("</w:sdtContent>")
        .expect("sdtContent close present");
    let page_para = footer_xml.find(" PAGE ").expect("PAGE field present");
    assert!(
        sdt_content < page_para && page_para < sdt_close,
        "the PAGE-field paragraph must be enclosed by the sdtContent envelope; \
         footer1.xml: {footer_xml}"
    );
}

/// The canonical case: Word's page-number footer gallery survives an unrelated
/// document edit.
#[test]
fn footer_page_number_sdt_survives_edit_rebuild() {
    let docx = make_docx_with_footer(&page_number_footer(PAGE_FIELD_PARA));
    let out = edit_and_serialize(&docx);

    let footer_xml = part_of(&out, "word/footer1.xml");
    assert_footer_keeps_gallery_envelope(&footer_xml);

    let report = stemma::api::validate(&out);
    assert!(
        report.ok,
        "output must validate clean; issues: {:?}",
        report.issues
    );
}

/// Idempotence: re-importing our own output and editing again must not
/// duplicate or nest the envelope.
#[test]
fn footer_sdt_envelope_is_stable_across_reimport() {
    let docx = make_docx_with_footer(&page_number_footer(PAGE_FIELD_PARA));
    let once = edit_and_serialize(&docx);
    let twice = edit_and_serialize(&once);

    let footer_xml = part_of(&twice, "word/footer1.xml");
    assert_footer_keeps_gallery_envelope(&footer_xml);
}

/// A multi-block envelope keeps its full span: both paragraphs inside ONE
/// sdtContent, and a trailing unwrapped paragraph stays outside it.
#[test]
fn footer_sdt_spanning_two_paragraphs_keeps_span_and_boundary() {
    let two_paras = format!(r#"{PAGE_FIELD_PARA}<w:p><w:r><w:t>Confidential</w:t></w:r></w:p>"#);
    let footer_xml_in = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:ftr xmlns:w="{W_NS}"><w:sdt><w:sdtPr><w:id w:val="123456"/><w:docPartObj><w:docPartGallery w:val="Page Numbers (Bottom of Page)"/><w:docPartUnique/></w:docPartObj></w:sdtPr><w:sdtContent>{two_paras}</w:sdtContent></w:sdt><w:p><w:r><w:t>Outside the control</w:t></w:r></w:p></w:ftr>"#
    );
    let docx = make_docx_with_footer(&footer_xml_in);
    let out = edit_and_serialize(&docx);

    let footer_xml = part_of(&out, "word/footer1.xml");
    assert_eq!(
        footer_xml.matches("<w:sdt>").count(),
        1,
        "exactly one envelope; footer1.xml: {footer_xml}"
    );
    let close = footer_xml
        .find("</w:sdtContent>")
        .expect("sdtContent close present");
    let confidential = footer_xml
        .find("Confidential")
        .expect("second wrapped paragraph present");
    let outside = footer_xml
        .find("Outside the control")
        .expect("unwrapped paragraph present");
    assert!(
        confidential < close,
        "second paragraph must stay INSIDE the envelope (span=2); footer1.xml: {footer_xml}"
    );
    assert!(
        close < outside,
        "trailing paragraph must stay OUTSIDE the envelope; footer1.xml: {footer_xml}"
    );
}
