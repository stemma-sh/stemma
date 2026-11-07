//! New text in a marked paragraph is body text.
//!
//! Domain rule: content that marks any of its text states its own
//! formatting. Its unchanged words keep the template's formatting, but its
//! new text is the template's body text (the formatting most of the template
//! has), not the formatting of the template words it happens to replace: a
//! definition whose term the caller marks bold italic has a plain " means …".

use stemma::api::Document;
use stemma::domain::{BlockNode, InlineNode, Mark};

fn docx() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:rPr><w:b/><w:i/></w:rPr><w:t>re</w:t></w:r><w:r><w:rPr><w:b/><w:i/></w:rPr><w:noBreakHyphen/></w:r><w:r><w:rPr><w:b/><w:i/></w:rPr><w:t>sealed</w:t></w:r><w:r><w:t>:</w:t></w:r><w:r><w:t xml:space="preserve"> a record is </w:t></w:r><w:r><w:rPr><w:b/><w:i/></w:rPr><w:t>re</w:t></w:r><w:r><w:rPr><w:b/><w:i/></w:rPr><w:noBreakHyphen/></w:r><w:r><w:rPr><w:b/><w:i/></w:rPr><w:t>sealed</w:t></w:r><w:r><w:t xml:space="preserve"> if the record is no longer open to a member of the public or to a person who is reasonably interested.</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;
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
fn unmarked_new_text_beside_a_marked_term_is_body_text() {
    let doc = Document::parse(&docx()).expect("parse");
    let template = doc.read().blocks[0].id.to_string();
    let json = serde_json::json!({"ops":[{"op":"insert","target":{"anchor":template,"position":"after"},
        "content":[{"type":"paragraph","attrs":{"format_from":template},"content":[
            {"type":"text","text":"Records Office","marks":[{"type":"bold"},{"type":"italic"}]},
            {"type":"text","text":" means the Executive Office of that name established under the "},
            {"type":"text","text":"Public Records Act 1999","marks":[{"type":"italic"}]},
            {"type":"text","text":"."}]}]}],
        "materialization_mode":"direct","revision":{"author":"Editor"}})
    .to_string();
    let edited = doc
        .apply(
            &stemma::edit_v4::parse_transaction(&json)
                .unwrap()
                .into_edit_transaction()
                .unwrap(),
        )
        .expect("insert applies");
    let BlockNode::Paragraph(inserted) = &edited.snapshot().canonical.blocks[1].block else {
        panic!("the new paragraph")
    };
    let runs: Vec<(String, bool, bool)> = inserted
        .all_inlines()
        .filter_map(|inline| match inline {
            InlineNode::Text(t) => Some((
                t.text.clone(),
                t.marks.contains(&Mark::Bold),
                t.marks.contains(&Mark::Italic),
            )),
            _ => None,
        })
        .collect();
    let formatted = |needle: &str| {
        runs.iter()
            .filter(|(text, ..)| text.contains(needle))
            .map(|(_, b, i)| (*b, *i))
            .collect::<Vec<_>>()
    };
    assert_eq!(formatted("Records Office"), [(true, true)], "{runs:?}");
    assert_eq!(
        formatted("means"),
        [(false, false)],
        "the unmarked text is body text: {runs:?}"
    );
    assert_eq!(formatted("Public Records Act"), [(false, true)], "{runs:?}");
}
