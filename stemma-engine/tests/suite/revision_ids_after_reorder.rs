//! A revision keeps its id when another editor reorders the document.
//!
//! Domain rule (docs/guide/revisions.md, "Revision ids are content-derived
//! identities"): a revision id names what the change is, and a revision whose
//! own content is untouched keeps its id across save and reopen. Moving two
//! paragraphs past each other with tracking off (a cut and paste in Word, a
//! table sort) leaves every revision's content as it was, so each id must still
//! name the same change, and accepting an id read before the reorder must
//! accept that change and no other.

use std::collections::BTreeMap;
use std::io::{Read, Write};

use stemma::ExportOptions;
use stemma::api::{Document, Resolution};
use stemma::tracked_model::ResolveSelectionAction;

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn docx() -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}"><w:body><w:p><w:r><w:t>Alpha clause applies.</w:t></w:r></w:p><w:p><w:r><w:t>Beta clause applies.</w:t></w:r></w:p><w:p><w:r><w:t>Final clause.</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    write_zip(&[
        ("[Content_Types].xml", content_types.as_bytes().to_vec()),
        ("_rels/.rels", rels.as_bytes().to_vec()),
        ("word/document.xml", document_xml.into_bytes()),
    ])
}

fn write_zip(parts: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions = zip::write::FileOptions::default();
        for (name, data) in parts {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

/// `bytes` with the first two body paragraphs swapped and every other part,
/// the saved revision-identity part included, byte for byte as it was.
fn swap_first_two_paragraphs(bytes: &[u8]) -> Vec<u8> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut parts: Vec<(String, Vec<u8>)> = Vec::new();
    for index in 0..archive.len() {
        let mut file = archive.by_index(index).unwrap();
        let mut data = Vec::new();
        file.read_to_end(&mut data).unwrap();
        parts.push((file.name().to_string(), data));
    }
    let (_, document) = parts
        .iter_mut()
        .find(|(name, _)| name == "word/document.xml")
        .unwrap();
    let xml = String::from_utf8(document.clone()).unwrap();
    let body = xml.find("<w:body>").unwrap() + "<w:body>".len();
    let first_end = xml[body..].find("</w:p>").unwrap() + body + "</w:p>".len();
    let second_end = xml[first_end..].find("</w:p>").unwrap() + first_end + "</w:p>".len();
    let swapped = format!(
        "{}{}{}{}",
        &xml[..body],
        &xml[first_end..second_end],
        &xml[body..first_end],
        &xml[second_end..]
    );
    *document = swapped.into_bytes();
    let parts: Vec<(&str, Vec<u8>)> = parts
        .iter()
        .map(|(name, data)| (name.as_str(), data.clone()))
        .collect();
    write_zip(&parts)
}

/// Each revision's id, keyed by what the change is.
fn ids_by_change(doc: &Document) -> BTreeMap<(String, String), u32> {
    doc.revisions()
        .into_iter()
        .map(|r| ((r.kind.as_str().to_string(), r.excerpt), r.revision_id))
        .collect()
}

#[test]
fn an_untracked_reorder_leaves_each_revision_its_id() {
    let base = Document::parse(&docx()).expect("parse");
    let json = serde_json::json!({"ops":[
        {"op":"replace","target":"p_1","expect":"Alpha",
         "content":{"type":"paragraph","content":[{"type":"text","text":"Gamma clause applies."}]}},
        {"op":"replace","target":"p_2","expect":"Beta",
         "content":{"type":"paragraph","content":[{"type":"text","text":"Delta clause applies."}]}}],
        "revision":{"author":"Bob","date":"2026-11-06T09:00:00Z"}})
    .to_string();
    let redline = base
        .apply(
            &stemma::edit_v4::parse_transaction(&json)
                .unwrap()
                .into_edit_transaction()
                .unwrap(),
        )
        .expect("the redline applies");
    let saved = redline.serialize(&ExportOptions::default()).unwrap();
    let before = ids_by_change(&Document::parse(&saved).expect("reopen"));
    assert_eq!(before.len(), 4, "four revisions: {before:?}");

    let reordered = Document::parse(&swap_first_two_paragraphs(&saved)).expect("reopen reordered");
    assert_eq!(ids_by_change(&reordered), before);

    // The id read for "delete Alpha" before the reorder accepts exactly that.
    let delete_alpha = before[&("delete".to_string(), "Alpha".to_string())];
    let accepted = reordered
        .project(Resolution::Selective {
            ids: [delete_alpha].into_iter().collect(),
            action: ResolveSelectionAction::Accept,
        })
        .expect("accept the Alpha deletion alone");
    assert_eq!(
        accepted.read_rejected().unwrap().to_text(),
        "Beta clause applies.\n\n clause applies.\n\nFinal clause."
    );
}
