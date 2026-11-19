//! A negative `w:sz` / `w:szCs`.
//!
//! ST_HpsMeasure is unsigned, but documents carry negative sizes. Checked
//! against Word: it opens such a document without repair, holds the size as
//! an unsigned 16-bit number ("-24" is 65512, "-1" is 65535), and saves that
//! number.

use std::io::{Cursor, Read, Write};

use stemma::ExportOptions;
use stemma::api::Document;
use zip::{ZipWriter, write::FileOptions};

fn docx(body: &str) -> Vec<u8> {
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
    let mut buf = Vec::new();
    {
        let mut zip = ZipWriter::new(Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        for (name, text) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/document.xml", document.as_str()),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(text.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

fn saved_document_xml(doc: &Document) -> String {
    let saved = doc.serialize(&ExportOptions::default()).expect("save");
    let mut zip = zip::ZipArchive::new(Cursor::new(saved)).unwrap();
    let mut xml = String::new();
    zip.by_name("word/document.xml")
        .unwrap()
        .read_to_string(&mut xml)
        .unwrap();
    xml.replace(" />", "/>")
}

fn sized(size: &str) -> Vec<u8> {
    docx(&format!(
        r#"<w:p><w:r><w:rPr><w:sz w:val="{size}"/><w:szCs w:val="{size}"/></w:rPr><w:t>The lamp room is on the top floor.</w:t></w:r></w:p>"#
    ))
}

#[test]
fn a_negative_size_loads_and_keeps_its_text() {
    let doc = Document::parse(&sized("-24")).expect("a negative size loads");
    assert_eq!(
        doc.read_accepted().unwrap().to_text().trim(),
        "The lamp room is on the top floor."
    );
    assert!(stemma::api::validate(&sized("-24")).ok);
}

/// An untouched run is written back as authored, which Word opens. The
/// number the model holds (65512 for "-24") is pinned beside the parser.
#[test]
fn an_untouched_run_keeps_the_size_it_wrote() {
    let xml = saved_document_xml(&Document::parse(&sized("-24")).unwrap());
    assert!(
        xml.contains(r#"<w:sz w:val="-24"/><w:szCs w:val="-24"/>"#),
        "{xml}"
    );
}

/// Below the 16-bit range there is no observation of what Word holds.
#[test]
fn a_size_below_the_16_bit_range_is_refused() {
    let error = Document::parse(&sized("-65536"))
        .err()
        .expect("no known reading");
    assert!(format!("{error:?}").contains("sz"), "{error:?}");
}
