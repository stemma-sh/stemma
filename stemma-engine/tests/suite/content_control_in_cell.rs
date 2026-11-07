//! A content control inside a table cell can be set.
//!
//! Domain rule (§17.5.2, w14:checkbox): a form's checkbox content controls
//! usually sit in table cells. `set_content_control_value` names the hosting
//! paragraph and the control by the ids the reads show; for a paragraph in a
//! cell that must work as it does for a body paragraph: the control becomes
//! checked (`w14:checked` 1) and shows the checked glyph.

use stemma::ExportOptions;
use stemma::api::Document;
use stemma::domain::{BlockNode, InlineNode, OpaqueKind};

fn docx() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml"><w:body><w:tbl><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t xml:space="preserve">Reference 1 </w:t></w:r><w:sdt><w:sdtPr><w:id w:val="-1201"/><w14:checkbox><w14:checked w14:val="0"/><w14:checkedState w14:val="2612" w14:font="MS Gothic"/><w14:uncheckedState w14:val="2610" w14:font="MS Gothic"/></w14:checkbox></w:sdtPr><w:sdtContent><w:r><w:rPr><w:rFonts w:ascii="MS Gothic" w:hAnsi="MS Gothic"/></w:rPr><w:t>☐</w:t></w:r></w:sdtContent></w:sdt></w:p></w:tc></w:tr></w:tbl><w:p/><w:sectPr/></w:body></w:document>"#;
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

/// The cell paragraph's id and its checkbox control's id.
fn cell_checkbox(doc: &Document) -> (String, String) {
    let snapshot = doc.snapshot();
    let canonical = &snapshot.canonical;
    let BlockNode::Table(table) = &canonical.blocks[0].block else {
        panic!("the first block is the table")
    };
    let BlockNode::Paragraph(p) = &table.rows[0].cells[0].blocks[0] else {
        panic!("the cell holds a paragraph")
    };
    let sdt = p
        .segments
        .iter()
        .flat_map(|s| &s.inlines)
        .find_map(|inline| match inline {
            InlineNode::OpaqueInline(o) if matches!(o.kind, OpaqueKind::Sdt) => {
                Some(o.id.to_string())
            }
            _ => None,
        })
        .expect("the paragraph holds the checkbox control");
    (p.id.to_string(), sdt)
}

#[test]
fn a_checkbox_in_a_table_cell_is_ticked() {
    let doc = Document::parse(&docx()).expect("parse");
    let (paragraph, control) = cell_checkbox(&doc);
    let json = serde_json::json!({"ops":[{"op":"set_content_control_value","target":paragraph,
        "sdt_id":control,"checked":true}],"materialization_mode":"direct","revision":{"author":"Clerk"}})
    .to_string();
    let edited = doc
        .apply(
            &stemma::edit_v4::parse_transaction(&json)
                .unwrap()
                .into_edit_transaction()
                .unwrap(),
        )
        .expect("the checkbox in the cell is set");
    let bytes = edited.serialize(&ExportOptions::default()).expect("save");
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
        xml.contains(r#"<w14:checked w14:val="1"/>"#),
        "the control is checked: {xml}"
    );
    assert!(
        xml.contains('☒') && !xml.contains('☐'),
        "it shows the checked glyph: {xml}"
    );
}
