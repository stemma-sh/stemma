//! Permission ranges that end between table rows survive.
//!
//! Domain rule (§17.13.7: w:permStart / w:permEnd may appear wherever a
//! bookmark may, including as children of w:tbl, w:tr and w:tc): a protected
//! form marks its editable cells with a permission range that starts in a
//! cell and ends between rows. Both halves are kept on save, edited or not;
//! dropping the end tears the range and changes what the protection allows.

use stemma::ExportOptions;
use stemma::api::Document;

fn docx() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Order form</w:t></w:r></w:p><w:tbl><w:tblGrid><w:gridCol w:w="4000"/><w:gridCol w:w="4000"/></w:tblGrid><w:tr><w:tc><w:p><w:permStart w:id="1" w:edGrp="everyone"/><w:r><w:t>NDC</w:t></w:r></w:p></w:tc><w:tc><w:permStart w:id="2" w:edGrp="everyone"/><w:p><w:r><w:t>Name</w:t></w:r></w:p><w:permEnd w:id="2"/></w:tc><w:permEnd w:id="3"/></w:tr><w:permEnd w:id="1"/><w:tr><w:tc><w:p><w:permStart w:id="3" w:edGrp="everyone"/><w:r><w:t>x</w:t></w:r></w:p></w:tc><w:tc><w:p/></w:tc></w:tr></w:tbl><w:p/><w:sectPr/></w:body></w:document>"#;
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

fn ids(xml: &str, element: &str) -> Vec<String> {
    let mut out: Vec<String> = xml
        .match_indices(&format!("<w:{element} "))
        .map(|(at, _)| {
            let rest = &xml[at..];
            let id = rest.find("w:id=\"").expect("an id") + 6;
            rest[id..id + rest[id..].find('"').unwrap()].to_string()
        })
        .collect();
    out.sort();
    out
}

fn saved_xml(doc: &Document) -> String {
    let bytes = doc.serialize(&ExportOptions::default()).expect("save");
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
fn permission_ranges_ending_between_rows_and_cells_are_kept() {
    let doc = Document::parse(&docx()).expect("parse");
    let body = doc.read().blocks[0].id.to_string();
    let json = serde_json::json!({"ops":[{"op":"replace","target":body,"expect":"Order form",
        "content":{"type":"paragraph","content":[{"type":"text","text":"Order form (2026)"}]}}],
        "revision":{"author":"Clerk"}})
    .to_string();
    let edited = doc
        .apply(
            &stemma::edit_v4::parse_transaction(&json)
                .unwrap()
                .into_edit_transaction()
                .unwrap(),
        )
        .expect("an edit outside the table");
    for (label, document) in [("untouched", &doc), ("edited elsewhere", &edited)] {
        let xml = saved_xml(document);
        assert_eq!(ids(&xml, "permStart"), ["1", "2", "3"], "{label}: {xml}");
        assert_eq!(
            ids(&xml, "permEnd"),
            ["1", "2", "3"],
            "{label}: every range keeps its end"
        );
    }
}

/// Typing into a protected form's editable cell: its paragraph holds only a
/// w:permStart, a zero-width marker that carries no formatting, so the text
/// takes the paragraph mark's formatting as in any empty paragraph.
#[test]
fn text_typed_after_a_permission_start_takes_the_paragraph_marks_formatting() {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:tbl><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid><w:tr><w:tc><w:p><w:pPr><w:rPr><w:b/></w:rPr></w:pPr><w:permStart w:id="1" w:edGrp="everyone"/></w:p></w:tc></w:tr><w:permEnd w:id="1"/></w:tbl><w:p/><w:sectPr/></w:body></w:document>"#;
    let bytes = {
        let base = docx();
        let mut archive = stemma::docx::DocxArchive::read(&base).unwrap();
        archive.upsert("word/document.xml", document_xml.as_bytes().to_vec());
        archive.write().unwrap()
    };
    let doc = Document::parse(&bytes).expect("parse");
    let table = doc.read().blocks[0].id.to_string();
    let json = serde_json::json!({"ops":[{"op":"table_op","target":table,
        "table_op":{"kind":"set_cell_text","row_index":0,"col_index":0,"text":"50102-0101-01"}}],
        "materialization_mode":"direct","revision":{"author":"Clerk"}})
    .to_string();
    let edited = doc
        .apply(
            &stemma::edit_v4::parse_transaction(&json)
                .unwrap()
                .into_edit_transaction()
                .unwrap(),
        )
        .expect("the cell is filled");
    let xml = saved_xml(&edited);
    let text = xml.find(">50102-0101-01<").expect("the text");
    let run = xml[..text].rfind("<w:r>").expect("its run");
    assert!(
        xml[run..text].contains("<w:b"),
        "bold from the paragraph mark: {}",
        &xml[run..text]
    );
    assert_eq!(ids(&xml, "permEnd"), ["1"]);
}
