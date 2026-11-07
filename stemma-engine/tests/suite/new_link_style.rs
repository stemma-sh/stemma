//! A link an edit creates is styled as a link.
//!
//! Domain rule: Word applies the document's Hyperlink character style (the
//! style named "Hyperlink", whatever its localized id) to the text of a link
//! it inserts, so a new link reads blue and underlined like the others. Links
//! already in the document keep their formatting as authored.

use stemma::ExportOptions;
use stemma::api::Document;

const STYLES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style><w:style w:type="character" w:styleId="Hipervinculo"><w:name w:val="Hyperlink"/><w:rPr><w:color w:val="0563C1"/><w:u w:val="single"/></w:rPr></w:style></w:styles>"#;

fn docx() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body><w:p><w:r><w:t xml:space="preserve">See the </w:t></w:r><w:hyperlink r:id="rIdOld"><w:r><w:t>old bill</w:t></w:r></w:hyperlink><w:r><w:t>.</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdS" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rIdOld" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.org/old" TargetMode="External"/></Relationships>"#;
    use std::io::Write;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions = zip::write::FileOptions::default();
        for (name, data) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/document.xml", document_xml),
            ("word/styles.xml", STYLES),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

#[test]
fn a_new_link_takes_the_documents_hyperlink_style_by_name() {
    let doc = Document::parse(&docx()).expect("parse");
    let anchor = doc.read().blocks[0].id.to_string();
    let json = serde_json::json!({"ops":[{"op":"insert","target":{"anchor":anchor,"position":"after"},
        "content":[{"type":"paragraph","attrs":{"format_from":anchor},"content":[
            {"type":"text","text":"And the "},
            {"type":"hyperlink","attrs":{"href":"https://example.org/new"},"content":[{"type":"text","text":"new bill"}]},
            {"type":"text","text":"."}]}]}],
        "revision":{"author":"Editor"}})
    .to_string();
    let edited = doc
        .apply(
            &stemma::edit_v4::parse_transaction(&json)
                .unwrap()
                .into_edit_transaction()
                .unwrap(),
        )
        .expect("insert applies");
    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let xml = String::from_utf8(
        stemma::docx::DocxArchive::read(&bytes)
            .unwrap()
            .get("word/document.xml")
            .unwrap()
            .to_vec(),
    )
    .unwrap()
    .replace(
        r#" xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#,
        "",
    );
    let new = xml.find(">new bill<").expect("the new link");
    let run_start = xml[..new].rfind("<w:r>").expect("its run");
    assert!(
        xml[run_start..new].contains(r#"<w:rStyle w:val="Hipervinculo""#),
        "the new link takes the style named Hyperlink: {}",
        &xml[run_start..new + 12]
    );
    let old = xml.find(">old bill<").expect("the old link");
    let old_start = xml[..old].rfind("<w:r>").expect("its run");
    assert!(
        !xml[old_start..old].contains("rStyle"),
        "the existing link keeps its formatting"
    );
    let reopened = Document::parse(&bytes).expect("reopen");
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
