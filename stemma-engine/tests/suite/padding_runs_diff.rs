//! Spaces a replacement keeps stay in their own runs.
//!
//! Domain rule: replacing a paragraph's text changes only what differs, and
//! everything else keeps its formatting. A line laid out with padding runs
//! of different formatting ("Juli 2024", four small spaces, three spaces,
//! "SSN 1979") that loses two spaces and gains an "I" keeps the four small
//! spaces as they were: a run of spaces is as many spaces, not one word that
//! changed.

use stemma::ExportOptions;
use stemma::api::Document;

fn docx() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Juli 2024</w:t></w:r><w:r><w:rPr><w:sz w:val="16"/></w:rPr><w:t xml:space="preserve">    </w:t></w:r><w:r><w:rPr><w:lang w:val="en-US"/></w:rPr><w:t xml:space="preserve">   </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>SSN 1979</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;
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
            ("word/document.xml", document_xml),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

#[test]
fn a_replacement_keeps_the_padding_runs_it_does_not_change() {
    let doc = Document::parse(&docx()).expect("parse");
    let paragraph = doc.read().blocks[0].id.to_string();
    for mode in ["tracked_change", "direct"] {
        let json = serde_json::json!({"ops":[{"op":"replace","target":paragraph,
            "expect":"Juli 2024","content":{"type":"paragraph",
                "content":[{"type":"text","text":"Juli 2024     ISSN 1979"}]}}],
            "materialization_mode":mode,"revision":{"author":"Editor"}})
        .to_string();
        let edited = doc
            .apply(
                &stemma::edit_v4::parse_transaction(&json)
                    .unwrap()
                    .into_edit_transaction()
                    .unwrap(),
            )
            .unwrap_or_else(|e| panic!("{mode}: {e}"));
        let accepted = edited.read_accepted().expect("accept");
        assert_eq!(
            accepted.read().blocks[0].text,
            "Juli 2024     ISSN 1979",
            "{mode}"
        );
        let bytes = accepted.serialize(&ExportOptions::default()).expect("save");
        let xml = String::from_utf8(
            stemma::docx::DocxArchive::read(&bytes)
                .unwrap()
                .get("word/document.xml")
                .unwrap()
                .to_vec(),
        )
        .unwrap()
        .replace(" />", "/>");
        assert!(
            xml.contains(r#"<w:sz w:val="16"/></w:rPr><w:t xml:space="preserve">    </w:t>"#),
            "{mode}: the four small spaces keep their run: {xml}"
        );
    }
}
