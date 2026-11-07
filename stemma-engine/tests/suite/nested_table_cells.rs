//! A table nested in a cell is read like the rest of the cell.
//!
//! Domain rule (§17.4.66: a cell holds paragraphs and tables): a form often
//! lays its labelled fields out in a table inside a cell of a larger table.
//! Those paragraphs are part of the cell's content and edit targets, so a
//! read of the outer cell lists them, in document order, and carries their
//! text; an edit addressed to one of them applies.

use stemma::api::Document;

fn docx() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:tbl><w:tblGrid><w:gridCol w:w="8000"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>Applicant details</w:t></w:r></w:p><w:tbl><w:tblGrid><w:gridCol w:w="4000"/><w:gridCol w:w="4000"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t xml:space="preserve">Agency Name: </w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t xml:space="preserve">Telephone: </w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/></w:tc></w:tr></w:tbl><w:p/><w:sectPr/></w:body></w:document>"#;
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
fn a_nested_tables_paragraphs_are_read_and_edited_through_the_outer_cell() {
    let doc = Document::parse(&docx()).expect("parse");
    let view = doc.read();
    let cell = &view.blocks[0].cells[0];
    let texts: Vec<String> = cell
        .paragraphs
        .iter()
        .map(|p| {
            p.segments
                .iter()
                .map(|s| match s {
                    stemma::InlineChange::Unchanged { text, .. } => text.clone(),
                    _ => String::new(),
                })
                .collect()
        })
        .collect();
    assert_eq!(
        texts,
        ["Applicant details", "Agency Name: ", "Telephone: ", ""],
        "the outer cell lists the nested table's paragraphs in document order"
    );
    assert!(
        cell.text.contains("Agency Name:") && cell.text.contains("Telephone:"),
        "{:?}",
        cell.text
    );
    let agency = cell.paragraphs[1].block_id.clone();
    let json = serde_json::json!({"ops":[{"op":"replace","target":agency,"expect":"Agency Name: ",
        "content":{"type":"paragraph","content":[{"type":"text","text":"Agency Name: Coastal Plain Housing"}]}}],
        "revision":{"author":"Clerk"}})
    .to_string();
    let edited = doc
        .apply(
            &stemma::edit_v4::parse_transaction(&json)
                .unwrap()
                .into_edit_transaction()
                .unwrap(),
        )
        .expect("a nested paragraph is an edit target");
    let accepted = edited.read_accepted().expect("accept");
    assert!(
        accepted.read().blocks[0].cells[0]
            .text
            .contains("Agency Name: Coastal Plain Housing"),
        "{:?}",
        accepted.read().blocks[0].cells[0].text
    );
}

/// A typed label in a cell ("b. " before an answer option) is text the
/// reader sees: the cell's text and its paragraph's view carry it, as a body
/// read does (the literal-prefix read rule).
#[test]
fn a_typed_label_in_a_cell_is_read() {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:tbl><w:tblGrid><w:gridCol w:w="4000"/><w:gridCol w:w="4000"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t xml:space="preserve">a. bar chart</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t xml:space="preserve">b. line graph</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/><w:sectPr/></w:body></w:document>"#;
    let bytes = {
        let mut archive = stemma::docx::DocxArchive::read(&docx()).unwrap();
        archive.upsert("word/document.xml", document_xml.as_bytes().to_vec());
        archive.write().unwrap()
    };
    let doc = Document::parse(&bytes).expect("parse");
    let view = doc.read();
    let cell = &view.blocks[0].cells[1];
    assert_eq!(cell.text, "b. line graph", "the cell reads as written");
    let paragraph = &cell.paragraphs[0];
    assert_eq!(
        paragraph.literal_prefix.as_deref(),
        Some("b. "),
        "the label is read with its separator"
    );
}
