//! Text and links inside a table in a header are editable.
//!
//! Domain rule: a header is a story like the body (§17.10), and a letterhead
//! is commonly a table in the header whose cells hold the address lines and an
//! e-mail link. Every paragraph the read lists there is a story-local edit
//! target: `edit_header` reaches it inside the table, as body edits reach cell
//! paragraphs, and a link in it is addressed with its header part. Block ids
//! repeat across stories, so a header target never resolves to the body.

use stemma::api::Document;
use stemma::domain::{BlockNode, InlineNode, OpaqueKind};
use stemma::{ExportOptions, Resolution};

const HEADER: &str = concat!(
    r#"<w:tbl><w:tblGrid><w:gridCol w:w="4000"/><w:gridCol w:w="4000"/></w:tblGrid><w:tr>"#,
    r#"<w:tc><w:p><w:r><w:t>Market Town Council</w:t></w:r></w:p></w:tc>"#,
    r#"<w:tc><w:p><w:r><w:t>Town Clerk</w:t></w:r></w:p>"#,
    r#"<w:p><w:r><w:t>Telephone 01234 567890</w:t></w:r></w:p>"#,
    r#"<w:p><w:r><w:t xml:space="preserve">E-mail: </w:t></w:r><w:hyperlink r:id="rIdMail"><w:r><w:t>townclerk@example.org</w:t></w:r></w:hyperlink></w:p></w:tc>"#,
    r#"</w:tr></w:tbl><w:p/>"#,
);

fn docx() -> Vec<u8> {
    let header_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">{HEADER}</w:hdr>"#
    );
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body><w:p><w:r><w:t>Telephone the office.</w:t></w:r></w:p><w:p><w:r><w:t>Second body paragraph.</w:t></w:r></w:p><w:p><w:r><w:t>Third body paragraph.</w:t></w:r></w:p><w:sectPr><w:headerReference w:type="default" r:id="rIdH1"/><w:pgSz w:w="12240" w:h="15840"/></w:sectPr></w:body></w:document>"#;
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdH1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/></Relationships>"#;
    let header_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdMail" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="mailto:townclerk@example.org" TargetMode="External"/></Relationships>"#;
    use std::io::Write;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions = zip::write::FileOptions::default();
        for (name, data) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/_rels/header1.xml.rels", header_rels),
            ("word/document.xml", document_xml),
            ("word/header1.xml", header_xml.as_str()),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

/// The story-local ids of the right cell's paragraphs: clerk, telephone, e-mail.
fn contact_ids(doc: &Document) -> Vec<String> {
    let canonical = &doc.snapshot().canonical;
    let BlockNode::Table(table) = &canonical.headers[0].blocks[0].block else {
        panic!("the header starts with its table");
    };
    table.rows[0].cells[1]
        .blocks
        .iter()
        .map(|block| match block {
            BlockNode::Paragraph(p) => p.id.to_string(),
            other => panic!("{other:?}"),
        })
        .collect()
}

fn apply(doc: &Document, op: serde_json::Value) -> Result<Document, String> {
    let json = serde_json::json!({"ops":[op],"revision":{"author":"Clerk"}}).to_string();
    let txn = stemma::edit_v4::parse_transaction(&json)
        .map_err(|e| format!("{e:?}"))?
        .into_edit_transaction()
        .map_err(|e| format!("{e:?}"))?;
    doc.apply(&txn).map_err(|e| e.to_string())
}

fn header_xml(doc: &Document) -> String {
    let bytes = doc.serialize(&ExportOptions::default()).expect("serialize");
    String::from_utf8(
        stemma::docx::DocxArchive::read(&bytes)
            .unwrap()
            .get("word/header1.xml")
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

/// The header's visible text, runs joined (deleted text excluded).
fn header_text(doc: &Document) -> String {
    let xml = header_xml(doc);
    let mut text = String::new();
    let mut rest = xml.as_str();
    while let Some(at) = rest.find("<w:t") {
        rest = &rest[at..];
        let open_end = rest.find('>').unwrap();
        if rest[..open_end].ends_with('/')
            || !(rest.starts_with("<w:t>") || rest.starts_with("<w:t "))
        {
            rest = &rest[open_end + 1..];
            continue;
        }
        let close = rest.find("</w:t>").unwrap();
        text.push_str(&rest[open_end + 1..close]);
        rest = &rest[close..];
    }
    text
}

fn reopens_the_same(doc: &Document) {
    let reopened = Document::parse(&doc.serialize(&ExportOptions::default()).unwrap()).unwrap();
    let differences = stemma::roundtrip_compare::compare_canon_docs(
        &doc.snapshot().canonical,
        &reopened.snapshot().canonical,
    );
    assert!(
        differences.is_empty(),
        "{:?}",
        &differences[..differences.len().min(3)]
    );
}

#[test]
fn a_paragraph_in_a_header_table_is_edited_tracked() {
    let doc = Document::parse(&docx()).expect("parse");
    let telephone = contact_ids(&doc)[1].clone();
    let edited = apply(
        &doc,
        serde_json::json!({"op":"edit_header","header_part":"header1.xml","target":telephone,
            "expect":"567890","content":[{"type":"text","text":"Telephone 01234 999999"}]}),
    )
    .expect("the cell paragraph is reachable");
    let xml = header_xml(&edited);
    assert!(xml.contains("<w:del ") && xml.contains("999999"), "{xml}");
    assert!(
        !xml.contains("tblLook"),
        "a table with no table properties gains none when written back: {xml}"
    );
    let accepted = header_text(&edited.project(Resolution::AcceptAll).unwrap());
    assert!(
        accepted.contains("Telephone 01234 999999") && !accepted.contains("567890"),
        "{accepted}"
    );
    let rejected = header_text(&edited.project(Resolution::RejectAll).unwrap());
    assert!(
        rejected.contains("Telephone 01234 567890") && !rejected.contains("999999"),
        "{rejected}"
    );
    assert_eq!(
        edited.read().blocks[0].text,
        "Telephone the office.",
        "the body is untouched"
    );
    reopens_the_same(&edited);
}

/// The id of the e-mail link in the header's contact cell.
fn mail_link_id(doc: &Document) -> String {
    let canonical = &doc.snapshot().canonical;
    let BlockNode::Table(table) = &canonical.headers[0].blocks[0].block else {
        panic!("the header starts with its table");
    };
    let BlockNode::Paragraph(p) = &table.rows[0].cells[1].blocks[2] else {
        panic!("the e-mail line");
    };
    p.all_inlines()
        .find_map(|inline| match inline {
            InlineNode::OpaqueInline(o) if matches!(o.kind, OpaqueKind::Hyperlink(_)) => {
                Some(o.id.to_string())
            }
            _ => None,
        })
        .expect("the e-mail link")
}

fn header_rels(doc: &Document) -> String {
    let bytes = doc.serialize(&ExportOptions::default()).expect("serialize");
    String::from_utf8(
        stemma::docx::DocxArchive::read(&bytes)
            .unwrap()
            .get("word/_rels/header1.xml.rels")
            .expect("the header keeps its rels")
            .to_vec(),
    )
    .unwrap()
}

#[test]
fn a_link_in_a_header_table_is_renamed_and_retargeted() {
    let doc = Document::parse(&docx()).expect("parse");
    let link = mail_link_id(&doc);
    let renamed = apply(
        &doc,
        serde_json::json!({"op":"replace","target":link,"header_part":"header1.xml",
            "expect":"townclerk@example.org",
            "content":{"type":"hyperlink","attrs":{"href":"mailto:townclerk@example.org"},"content":[{"type":"text","text":"clerk@example.org"}]}}),
    )
    .expect("the header link's text changes");
    let link = mail_link_id(&renamed);
    let edited = apply(
        &renamed,
        serde_json::json!({"op":"set_attr","target":link,"header_part":"header1.xml",
            "attrs":{"href":"mailto:clerk@example.org"},"expect_href":"mailto:townclerk@example.org"}),
    )
    .expect("the header link's target changes");
    assert!(
        header_rels(&edited).contains(r#"Target="mailto:clerk@example.org""#),
        "the new target is a relationship of the header part: {}",
        header_rels(&edited)
    );
    let accepted = edited.project(Resolution::AcceptAll).unwrap();
    assert!(
        header_text(&accepted).contains("E-mail: clerk@example.org"),
        "{}",
        header_text(&accepted)
    );
    let rejected = edited.project(Resolution::RejectAll).unwrap();
    assert!(header_text(&rejected).contains("E-mail: townclerk@example.org"));
    assert_eq!(edited.read().blocks[0].text, "Telephone the office.");
    reopens_the_same(&edited);
}

/// A header part on a paragraph replace is refused with the verb that does it.
#[test]
fn a_header_part_on_a_paragraph_replace_names_edit_header() {
    let doc = Document::parse(&docx()).expect("parse");
    let telephone = contact_ids(&doc)[1].clone();
    let err = apply(
        &doc,
        serde_json::json!({"op":"replace","target":telephone,"header_part":"header1.xml","expect":"567890",
            "content":{"type":"paragraph","content":[{"type":"text","text":"Telephone 01234 999999"}]}}),
    )
    .err()
    .expect("refused");
    assert!(err.contains("edit_header"), "{err}");
}
