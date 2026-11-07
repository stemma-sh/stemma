//! `w:spacing/@w:line` is ST_SignedTwipsMeasure (ISO 29500-1 §17.3.1.33): Word
//! documents carry negative values, and a document with one must open and keep it.

use std::io::{Read, Write};

use stemma::ExportOptions;
use stemma::api::Document;

fn docx(body: &str) -> Vec<u8> {
    let parts = [
        ("[Content_Types].xml", r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#.to_string()),
        ("_rels/.rels", r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#.to_string()),
        ("word/document.xml", format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#)),
    ];
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        for (name, data) in parts {
            zip.start_file(name, zip::write::FileOptions::default())
                .unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

#[test]
fn a_negative_line_spacing_opens_and_round_trips() {
    let doc = Document::parse(&docx(
        r#"<w:p><w:pPr><w:spacing w:line="-20" w:lineRule="auto"/></w:pPr><w:r><w:t>Note text.</w:t></w:r></w:p>"#,
    ))
    .expect("a schema-valid negative line spacing opens");
    let bytes = doc.serialize(&ExportOptions::default()).expect("serialize");
    let mut xml = String::new();
    zip::ZipArchive::new(std::io::Cursor::new(&bytes[..]))
        .unwrap()
        .by_name("word/document.xml")
        .unwrap()
        .read_to_string(&mut xml)
        .unwrap();
    assert!(
        xml.contains(r#"w:line="-20""#),
        "the value is written back: {xml}"
    );
}
