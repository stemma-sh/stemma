//! Renaming a contents entry keeps its page-number field.
//!
//! Domain rule: a table-of-contents entry is one link holding the label, a
//! tab, the title, a tab and a PAGEREF field whose cached result is the page
//! number. Changing the title changes only the title: the field stays a field
//! (Word updates it), the label and tabs stay as they are. The read shows the
//! link's whole display text, so a caller passes that text, edited; only the
//! words that differ may change.

use stemma::api::Document;
use stemma::domain::{InlineNode, OpaqueKind};
use stemma::{BlockNode, ExportOptions, Resolution};

const ENTRY: &str = concat!(
    r#"<w:p><w:pPr><w:pStyle w:val="TOC2"/></w:pPr><w:hyperlink w:anchor="_Toc1" w:history="1">"#,
    r#"<w:r><w:t>A.</w:t></w:r><w:r><w:tab/></w:r><w:r><w:t>Pecuniary Reparations</w:t></w:r>"#,
    r#"<w:r><w:rPr><w:webHidden/></w:rPr><w:tab/></w:r>"#,
    r#"<w:r><w:rPr><w:webHidden/></w:rPr><w:fldChar w:fldCharType="begin"/></w:r>"#,
    r#"<w:r><w:rPr><w:webHidden/></w:rPr><w:instrText xml:space="preserve"> PAGEREF _Toc1 \h </w:instrText></w:r>"#,
    r#"<w:r><w:rPr><w:webHidden/></w:rPr><w:fldChar w:fldCharType="separate"/></w:r>"#,
    r#"<w:r><w:rPr><w:webHidden/></w:rPr><w:t>22</w:t></w:r>"#,
    r#"<w:r><w:rPr><w:webHidden/></w:rPr><w:fldChar w:fldCharType="end"/></w:r>"#,
    r#"</w:hyperlink></w:p>"#,
);

fn docx(body: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    use std::io::Write;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions = zip::write::FileOptions::default();
        for (name, data) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/document.xml", document_xml.as_str()),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

fn link_id(doc: &Document) -> String {
    let BlockNode::Paragraph(p) = &doc.snapshot().canonical.blocks[0].block else {
        panic!("entry paragraph");
    };
    p.all_inlines()
        .find_map(|inline| match inline {
            InlineNode::OpaqueInline(o) if matches!(o.kind, OpaqueKind::Hyperlink(_)) => {
                Some(o.id.to_string())
            }
            _ => None,
        })
        .expect("entry link")
}

fn rename(doc: &Document, expect: &str, text: &str) -> Result<Document, String> {
    let json = serde_json::json!({"ops":[{"op":"replace","target":link_id(doc),"expect":expect,
        "content":{"type":"hyperlink","attrs":{"anchor":"_Toc1"},"content":[{"type":"text","text":text}]}}],
        "revision":{"author":"Editor"}})
    .to_string();
    let txn = stemma::edit_v4::parse_transaction(&json)
        .map_err(|e| format!("{e:?}"))?
        .into_edit_transaction()
        .map_err(|e| format!("{e:?}"))?;
    doc.apply(&txn).map_err(|e| e.to_string())
}

fn document_xml(doc: &Document) -> String {
    let bytes = doc.serialize(&ExportOptions::default()).expect("serialize");
    String::from_utf8(
        stemma::docx::DocxArchive::read(&bytes)
            .unwrap()
            .get("word/document.xml")
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

#[test]
fn renaming_a_contents_entry_by_its_whole_text_keeps_the_page_field() {
    let doc = Document::parse(&docx(ENTRY)).expect("parse");
    let edited = rename(
        &doc,
        "A.\tPecuniary Reparations\t22",
        "A.\tPecuniary Reparations for Moral Prejudice\t22",
    )
    .expect("rename applies");
    let xml = document_xml(&edited);
    assert!(xml.contains("PAGEREF _Toc1"), "the field stays: {xml}");
    assert_eq!(xml.matches("w:fldCharType=").count(), 3, "{xml}");
    assert!(!xml.contains("<w:del "), "nothing is deleted: {xml}");
    assert!(
        xml.contains(" for Moral Prejudice"),
        "only the new words are inserted: {xml}"
    );
    for resolved in [
        edited.project(Resolution::AcceptAll).unwrap(),
        edited.project(Resolution::RejectAll).unwrap(),
    ] {
        assert!(document_xml(&resolved).contains("PAGEREF _Toc1"));
    }
    let reopened = Document::parse(&edited.serialize(&ExportOptions::default()).unwrap()).unwrap();
    let differences = stemma::roundtrip_compare::compare_canon_docs(
        &edited.snapshot().canonical,
        &reopened.snapshot().canonical,
    );
    assert!(
        differences.is_empty(),
        "{:?}",
        &differences[..differences.len().min(3)]
    );
}

#[test]
fn a_rename_that_reaches_into_the_page_field_is_refused() {
    let doc = Document::parse(&docx(ENTRY)).expect("parse");
    let err = rename(&doc, "Reparations\t22", "Reparations\t23")
        .err()
        .expect("refused");
    assert!(err.contains("field"), "{err}");
}
