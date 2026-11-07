//! A section's decimal twips are read as Word reads them, never dropped.
//!
//! Domain rule (§17.6.11 pgMar, ST_TwipsMeasure): Google Docs writes page
//! margins and header/footer distances as decimal twips
//! (`w:footer="566.9291338582677"`); Word opens them and saves the truncated
//! integer (566, observed with the Word oracle). The section is rebuilt from
//! the model on save, so a value the import cannot read must not silently
//! become Word's default distance (720): that moved every page's footer.

use stemma::ExportOptions;
use stemma::api::Document;

fn docx(sect_pr: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Body text</w:t></w:r></w:p>{sect_pr}</w:body></w:document>"#
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

/// The pgMar of the saved document after an edit to the body, which rebuilds
/// the section from the model (an untouched document keeps it verbatim).
fn saved_page_margins(sect_pr: &str) -> String {
    let doc = Document::parse(&docx(sect_pr)).expect("parse");
    let paragraph = doc.read().blocks[0].id.to_string();
    let json = serde_json::json!({"ops":[{"op":"replace","target":paragraph,
        "expect":"Body text","content":{"type":"paragraph",
            "content":[{"type":"text","text":"Body copy"}]}}],
        "revision":{"author":"Clerk"}})
    .to_string();
    let edited = doc
        .apply(
            &stemma::edit_v4::parse_transaction(&json)
                .unwrap()
                .into_edit_transaction()
                .unwrap(),
        )
        .expect("the body is edited");
    let bytes = edited.serialize(&ExportOptions::default()).expect("save");
    let xml = String::from_utf8(
        stemma::docx::DocxArchive::read(&bytes)
            .unwrap()
            .get("word/document.xml")
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    let start = xml.find("<w:pgMar").expect("the section keeps its pgMar");
    xml[start..start + xml[start..].find("/>").unwrap()].to_string()
}

#[test]
fn decimal_header_and_footer_distances_are_kept_as_word_reads_them() {
    let pg_mar = saved_page_margins(
        r#"<w:sectPr><w:pgSz w:w="11909" w:h="16834"/><w:pgMar w:top="1440" w:bottom="1440" w:left="1440" w:right="1440" w:header="566.9291338582677" w:footer="566.9291338582677"/></w:sectPr>"#,
    );
    assert!(
        pg_mar.contains(r#"w:header="566""#) && pg_mar.contains(r#"w:footer="566""#),
        "the distances are kept, truncated as Word reads them: {pg_mar}"
    );
}

#[test]
fn a_universal_measure_margin_is_read_in_twips() {
    let pg_mar = saved_page_margins(
        r#"<w:sectPr><w:pgMar w:top="1in" w:bottom="1440" w:left="2.54cm" w:right="1440" w:header="720" w:footer="720"/></w:sectPr>"#,
    );
    assert!(
        pg_mar.contains(r#"w:top="1440""#) && pg_mar.contains(r#"w:left="1440""#),
        "1in and 2.54cm are 1440 twips: {pg_mar}"
    );
}

#[test]
fn a_margin_no_schema_form_admits_is_refused_with_its_attribute() {
    let Err(err) = Document::parse(&docx(
        r#"<w:sectPr><w:pgMar w:top="1440" w:bottom="1440" w:left="1440" w:right="1440" w:header="wide" w:footer="720"/></w:sectPr>"#,
    )) else {
        panic!("an unreadable header distance must not become the default");
    };
    let message = err.to_string();
    assert!(
        message.contains("pgMar") && message.contains("header") && message.contains("wide"),
        "the refusal names the attribute and value: {message}"
    );
}
