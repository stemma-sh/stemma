//! A role's new paragraphs take the role's typical run formatting.
//!
//! Domain rule: a role names a kind of paragraph ("body text"), grouped by
//! paragraph-level formatting. New text inserted with a role takes the run
//! formatting of the role's exemplar, so the exemplar must carry the
//! formatting most of the role's paragraphs have: a document whose first body
//! paragraph happens to open with an italic note must not make every inserted
//! body paragraph (or table cell) italic.

use stemma::api::Document;
use stemma::{ExportOptions, Resolution};

fn docx(body: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
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

const BODY: &str = concat!(
    r#"<w:p><w:r><w:rPr><w:i/></w:rPr><w:t>Note: figures are indicative.</w:t></w:r></w:p>"#,
    r#"<w:p><w:r><w:t>Peak bodies apply for four years of funding.</w:t></w:r></w:p>"#,
    r#"<w:p><w:r><w:t>Grassroots initiatives apply for two years.</w:t></w:r></w:p>"#,
    r#"<w:p><w:r><w:t>Applications close in March.</w:t></w:r></w:p>"#,
);

#[test]
fn a_new_body_paragraph_is_not_italic_because_the_first_one_opens_italic() {
    let doc = Document::parse(&docx(BODY)).expect("parse");
    let view = doc.read();
    let role = view.blocks[1].role_token.clone().expect("a body role");
    assert_eq!(
        view.blocks[0].role_token.as_deref(),
        Some(role.as_str()),
        "fixture: the italic note shares the body role"
    );
    let anchor = view.blocks[3].id.to_string();
    let json = serde_json::json!({"ops":[{"op":"insert","target":{"anchor":anchor,"position":"after"},"content":[{"type":"paragraph","role":role,"content":[{"type":"text","text":"Results are published in June."}]}]}],"revision":{"author":"Editor"}}).to_string();
    let inserted = doc
        .apply(
            &stemma::edit_v4::parse_transaction(&json)
                .expect("parse")
                .into_edit_transaction()
                .expect("adapt"),
        )
        .expect("insert");
    let accepted = inserted.project(Resolution::AcceptAll).expect("accept");
    let bytes = accepted.serialize(&ExportOptions::default()).expect("save");
    let xml = String::from_utf8(
        stemma::docx::DocxArchive::read(&bytes)
            .expect("read")
            .get("word/document.xml")
            .expect("part")
            .to_vec(),
    )
    .expect("utf-8");
    let at = xml.find("Results are published").expect("inserted");
    let run = &xml[xml[..at]
        .rfind("<w:r>")
        .or_else(|| xml[..at].rfind("<w:r "))
        .expect("run")..at];
    assert!(
        !run.contains("<w:i"),
        "the new paragraph is plain like most body text: {run}"
    );
}
