//! An author can format content they are still proposing.
//!
//! Domain rule: a pending insertion is the author's draft. Bolding a header
//! cell of a table they just inserted changes what they propose — the
//! insertion carries the bold — rather than stacking a formatting revision on
//! an insertion (accept keeps the bold table, reject removes the table). It
//! used to be refused ("has tracked status 'inserted'"), so an agent had to
//! reject its own table and rebuild it untracked. Content another author
//! inserted is still theirs: formatting it is refused.

use stemma::api::Document;
use stemma::{ExportOptions, Resolution};

fn docx() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Peak bodies apply for four years of funding.</w:t></w:r></w:p><w:p><w:r><w:t>Applications close in March.</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;
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

fn apply(
    doc: &Document,
    ops: serde_json::Value,
    author: &str,
) -> Result<Document, stemma::RuntimeError> {
    let json = serde_json::json!({"ops":ops,"revision":{"author":author}}).to_string();
    doc.apply(
        &stemma::edit_v4::parse_transaction(&json)
            .expect("parse")
            .into_edit_transaction()
            .expect("adapt"),
    )
}

fn document_xml(doc: &Document) -> String {
    let bytes = doc.serialize(&ExportOptions::default()).expect("save");
    String::from_utf8(
        stemma::docx::DocxArchive::read(&bytes)
            .expect("read")
            .get("word/document.xml")
            .expect("part")
            .to_vec(),
    )
    .expect("utf-8")
}

/// A two-column table whose cell paragraphs name no role: body text.
fn with_table(doc: &Document) -> Document {
    let anchor = doc.read().blocks[0].id.to_string();
    let cell = |text: &str| serde_json::json!({"content":[{"type":"paragraph","content":[{"type":"text","text":text}]}]});
    apply(
        doc,
        serde_json::json!([{"op":"insert","target":{"anchor":anchor,"position":"after"},"content":[{"type":"table","content":[{"content":[cell("Grant stream"),cell("Funding")]},{"content":[cell("Peak Body"),cell("$400,000")]}]}]}]),
        "Editor",
    )
    .expect("a table of role-less cell paragraphs inserts as body text")
}

fn header_cell(doc: &Document) -> String {
    doc.snapshot()
        .canonical
        .blocks
        .iter()
        .find_map(|tracked| match &tracked.block {
            stemma::domain::BlockNode::Table(table) => match &table.rows[0].cells[0].blocks[0] {
                stemma::domain::BlockNode::Paragraph(p) => Some(p.id.to_string()),
                _ => None,
            },
            _ => None,
        })
        .expect("the table's first cell paragraph")
}

#[test]
fn the_author_bolds_a_cell_of_the_table_they_are_proposing() {
    let doc = Document::parse(&docx()).expect("parse");
    let proposed = with_table(&doc);
    let cell = header_cell(&proposed);
    let bolded = apply(
        &proposed,
        serde_json::json!([{"op":"set_format","target":cell,"expect":"Grant stream","marks":{"bold":true}}]),
        "Editor",
    )
    .expect("the author formats their own proposal");
    let xml = document_xml(&bolded);
    assert!(
        !xml.contains("w:rPrChange"),
        "no formatting revision on an insertion: {xml}"
    );

    let accepted = bolded.project(Resolution::AcceptAll).expect("accept");
    let xml = document_xml(&accepted);
    let at = xml.find(">Grant stream</w:t>").expect("the header");
    let run = &xml[xml[..at].rfind("<w:r>").expect("its run")..at];
    assert!(run.contains("<w:b"), "accepted, the header is bold: {run}");

    let rejected = bolded.project(Resolution::RejectAll).expect("reject");
    assert!(
        !document_xml(&rejected).contains("<w:tbl>"),
        "rejected, the table is gone"
    );
}

#[test]
fn another_authors_pending_table_is_not_formatted() {
    let doc = Document::parse(&docx()).expect("parse");
    let proposed = with_table(&doc);
    let cell = header_cell(&proposed);
    let refused = apply(
        &proposed,
        serde_json::json!([{"op":"set_format","target":cell,"expect":"Grant stream","marks":{"bold":true}}]),
        "Reviewer",
    );
    assert!(refused.is_err(), "another author's proposal is refused");
}
