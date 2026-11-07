//! A table indent that is not in twips is kept as written.
//!
//! Domain rule (§17.4.51 tblInd, CT_TblWidth): the indent's value is read in
//! the unit its w:type names; only a twips (dxa) indent moves the table. A
//! `w:w="40.0" w:type="pct"` indent read as 40 twips and rewritten as dxa
//! moved an edited table 2 pt to the right. The element is kept verbatim.

use stemma::ExportOptions;
use stemma::api::Document;

fn docx() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:tbl><w:tblPr><w:tblW w:w="5000" w:type="pct"/><w:tblInd w:w="40.0" w:type="pct"/></w:tblPr><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>Rate $25</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/><w:sectPr/></w:body></w:document>"#;
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
fn a_percent_table_indent_is_kept_when_the_table_is_edited() {
    let doc = Document::parse(&docx()).expect("parse");
    let table = doc.read().blocks[0].id.to_string();
    let json = serde_json::json!({"ops":[{"op":"table_op","target":table,
        "table_op":{"kind":"set_cell_text","row_index":0,"col_index":0,"text":"Rate $26"}}],
        "revision":{"author":"Clerk"}})
    .to_string();
    let edited = doc
        .apply(
            &stemma::edit_v4::parse_transaction(&json)
                .unwrap()
                .into_edit_transaction()
                .unwrap(),
        )
        .expect("the cell is edited");
    let bytes = edited.serialize(&ExportOptions::default()).expect("save");
    let xml = String::from_utf8(
        stemma::docx::DocxArchive::read(&bytes)
            .unwrap()
            .get("word/document.xml")
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(
        xml.contains(r#"w:w="40.0" w:type="pct" />"#) && !xml.contains(r#"w:type="dxa""#),
        "the indent is kept as written: {xml}"
    );
    assert_eq!(xml.matches("<w:tblInd").count(), 1, "{xml}");
    let reopened = Document::parse(&bytes).expect("reopen");
    let differences = stemma::roundtrip_compare::compare_canon_docs(
        &edited.snapshot().canonical,
        &reopened.snapshot().canonical,
    );
    assert!(
        differences.is_empty(),
        "{:?}",
        &differences[..differences.len().min(2)]
    );
}

/// A row added at the end can take another row's formatting: insert_row
/// places it by ref_row and copies format_from_row (a requirements table
/// whose last row has bullets and whose earlier rows do not).
#[test]
fn an_inserted_row_copies_the_row_it_is_formatted_from() {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:tbl><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid><w:tr><w:trPr><w:trHeight w:val="500"/></w:trPr><w:tc><w:p><w:r><w:t>3. Plain requirement</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:trPr><w:trHeight w:val="900"/></w:trPr><w:tc><w:p><w:r><w:t>4. Tall requirement</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/><w:sectPr/></w:body></w:document>"#;
    let bytes = {
        let mut archive = stemma::docx::DocxArchive::read(&docx()).unwrap();
        archive.upsert("word/document.xml", document_xml.as_bytes().to_vec());
        archive.write().unwrap()
    };
    let doc = Document::parse(&bytes).expect("parse");
    let table = doc.read().blocks[0].id.to_string();
    let json = serde_json::json!({"ops":[{"op":"table_op","target":table,
        "table_op":{"kind":"insert_row","ref_row":1,"position":"after","format_from_row":0,
            "cells":["5. New requirement"]}}],
        "materialization_mode":"direct","revision":{"author":"Clerk"}})
    .to_string();
    let edited = doc
        .apply(
            &stemma::edit_v4::parse_transaction(&json)
                .unwrap()
                .into_edit_transaction()
                .unwrap(),
        )
        .expect("the row is inserted");
    let stemma::domain::BlockNode::Table(table) = &edited.snapshot().canonical.blocks[0].block
    else {
        panic!("table")
    };
    assert_eq!(table.rows.len(), 3);
    let height = |row: &stemma::domain::TableRowNode| row.height;
    assert_eq!(
        height(&table.rows[2]),
        height(&table.rows[0]),
        "copied from row 0"
    );
    assert_ne!(
        height(&table.rows[2]),
        height(&table.rows[1]),
        "not from the row it follows"
    );
}
