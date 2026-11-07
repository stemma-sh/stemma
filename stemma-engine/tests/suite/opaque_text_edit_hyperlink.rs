//! `opaque_text_edit` on a hyperlink names the route that edits it.
//!
//! Domain rule: a hyperlink's text is modeled content, changed by `replace`
//! on the hyperlink's id; `opaque_text_edit` descends into raw markup a
//! hyperlink does not carry. Refusing must name that route, so the edit is not
//! a dead end.

use stemma::api::Document;

fn docx() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:hyperlink w:anchor="Conclusion"><w:r><w:t xml:space="preserve">CONCLUSION	30</w:t></w:r></w:hyperlink></w:p><w:p><w:bookmarkStart w:id="1" w:name="Conclusion"/><w:r><w:t>CONCLUSION</w:t></w:r><w:bookmarkEnd w:id="1"/></w:p><w:sectPr/></w:body></w:document>"#;
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
fn a_hyperlink_is_refused_with_the_replace_route() {
    let doc = Document::parse(&docx()).expect("parse");
    let snapshot = doc.snapshot();
    let stemma::domain::BlockNode::Paragraph(p) = &snapshot.canonical.blocks[0].block else {
        panic!("the contents line")
    };
    let link = p
        .segments
        .iter()
        .flat_map(|s| &s.inlines)
        .find_map(|inline| match inline {
            stemma::domain::InlineNode::OpaqueInline(o) => Some(o.id.to_string()),
            _ => None,
        })
        .expect("the hyperlink");
    let json = serde_json::json!({"ops":[{"op":"opaque_text_edit","target":p.id.to_string(),
        "opaque_id":link,"find":"CONCLUSION","replacement":"SECTION 5. CONCLUSION"}],
        "revision":{"author":"Editor"}})
    .to_string();
    let Err(err) = doc.apply(
        &stemma::edit_v4::parse_transaction(&json)
            .unwrap()
            .into_edit_transaction()
            .unwrap(),
    ) else {
        panic!("opaque_text_edit does not edit a hyperlink");
    };
    let message = err.to_string();
    assert!(
        message.contains("is a hyperlink")
            && message.contains(r#""op":"replace""#)
            && message.contains(&link),
        "the refusal names the replace route on the link: {message}"
    );
}
