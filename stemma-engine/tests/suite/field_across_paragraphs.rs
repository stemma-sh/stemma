//! A field whose instruction runs on into the next paragraph stays a field.
//!
//! Domain rule (§17.16.18): a complex field is begin, instruction, optional
//! separate and result, end, and those runs may lie in different paragraphs.
//! Word writes table-of-contents entry fields this way: `TC \l2 "` ends one
//! paragraph and the entry's title, still instruction text, fills the next,
//! followed by the field's end. The title is field code, hidden from the page
//! and read by the contents; an edit anywhere else must leave it field code.

use stemma::ExportOptions;
use stemma::api::Document;

const BODY: &str = concat!(
    r#"<w:p><w:r><w:t xml:space="preserve">Before the entry.</w:t></w:r>"#,
    r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> TC \l2 "</w:instrText></w:r></w:p>"#,
    r#"<w:p><w:pPr><w:rPr><w:b/></w:rPr></w:pPr><w:r><w:rPr><w:b/></w:rPr><w:instrText>Management Rights</w:instrText></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#,
    r#"<w:p><w:r><w:t>The Office strives to serve.</w:t></w:r></w:p>"#,
);

fn docx(body: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
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
fn an_edit_elsewhere_keeps_a_field_instruction_that_continues_into_the_next_paragraph() {
    let doc = Document::parse(&docx(BODY)).expect("parse");
    let last = doc.read().blocks[2].id.to_string();
    let json = serde_json::json!({"ops":[{"op":"replace","target":last,"expect":"strives",
        "content":{"type":"paragraph","content":[{"type":"text","text":"The Office aims to serve."}]}}],
        "revision":{"author":"Editor"}})
    .to_string();
    let edited = doc
        .apply(
            &stemma::edit_v4::parse_transaction(&json)
                .unwrap()
                .into_edit_transaction()
                .unwrap(),
        )
        .expect("edit applies");
    for (label, document) in [("unedited", &doc), ("edited", &edited)] {
        let xml = document_xml(document);
        assert!(
            xml.contains(">Management Rights</w:instrText>"),
            "{label}: the title stays field instruction: {xml}"
        );
        assert!(
            !xml.contains(">Management Rights</w:t>"),
            "{label}: it does not become document text: {xml}"
        );
    }
}
