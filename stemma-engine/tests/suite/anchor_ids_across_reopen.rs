//! An untouched paragraph keeps its anchor ids, and its guard, when the
//! document is edited elsewhere, saved and reopened.
//!
//! Domain rule (read-model.md): an anchor's `id` is its durable id, the
//! preferred selector for anchor-relative ops, and a block's `guard` changes
//! only when the block does. Edit → save → read again is the ordinary CLI
//! workflow, so an id read before an edit elsewhere must still name the same
//! object after the reopen, and an op guarded by an untouched block's earlier
//! guard must still apply. Making a word bold in the first paragraph splits
//! its run; that must not renumber the content controls of the second.

use stemma::ExportOptions;
use stemma::api::Document;
use stemma::view::SegmentView;

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn docx() -> Vec<u8> {
    let control = |tag: &str, value: &str| {
        format!(
            r#"<w:sdt><w:sdtPr><w:tag w:val="{tag}"/><w:id w:val="{}"/><w:text/></w:sdtPr><w:sdtContent><w:r><w:t>{value}</w:t></w:r></w:sdtContent></w:sdt>"#,
            tag.len() * 1000 + 7
        )
    };
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}"><w:body><w:p><w:r><w:t>The parties agree as follows.</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve">Buyer: </w:t></w:r>{}<w:r><w:t xml:space="preserve"> Seller: </w:t></w:r>{}</w:p><w:sectPr/></w:body></w:document>"#,
        control("buyer", "Acme Ltd"),
        control("seller", "Beta Corp"),
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

fn apply(doc: &Document, op: serde_json::Value) -> Result<Document, String> {
    let json = serde_json::json!({"ops":[op],"materialization_mode":"direct",
        "revision":{"author":"Editor"}})
    .to_string();
    let txn = stemma::edit_v4::parse_transaction(&json)
        .map_err(|e| format!("{e:?}"))?
        .into_edit_transaction()
        .map_err(|e| format!("{e:?}"))?;
    doc.apply(&txn).map_err(|e| e.to_string())
}

/// The anchor ids and guard of block `p_2`.
fn p2(doc: &Document) -> (Vec<String>, String) {
    let view = doc.read();
    let block = view
        .blocks
        .iter()
        .find(|b| b.id.to_string() == "p_2")
        .expect("p_2");
    let ids = block
        .segments
        .iter()
        .filter_map(|s| match s {
            SegmentView::Opaque { id, .. } => Some(id.to_string()),
            _ => None,
        })
        .collect();
    (ids, block.guard.clone())
}

#[test]
fn an_edit_elsewhere_and_a_reopen_leave_a_paragraphs_anchor_ids_and_guard_unchanged() {
    let original = Document::parse(&docx()).expect("parse");
    let (ids, guard) = p2(&original);
    assert_eq!(ids.len(), 2, "two content controls in p_2: {ids:?}");

    let edited = apply(
        &original,
        serde_json::json!({"op":"replace","target":"p_1","expect":"parties",
        "content":{"type":"paragraph","content":[
            {"type":"text","text":"The "},
            {"type":"text","text":"parties","marks":[{"type":"bold"}]},
            {"type":"text","text":" agree as follows."}]}}),
    )
    .expect("the bold edit applies");
    let reopened =
        Document::parse(&edited.serialize(&ExportOptions::default()).unwrap()).expect("reopen");
    assert_eq!(p2(&reopened), (ids.clone(), guard.clone()));

    // An op guarded by the guard read before the edit still applies.
    apply(
        &reopened,
        serde_json::json!({"op":"set_para_format","target":"p_2","semantic_hash":guard,
        "align":"center"}),
    )
    .expect("the guard read before the edit still holds");

    // The seller id read before the edit still fills the seller.
    let filled = apply(
        &reopened,
        serde_json::json!({"op":"set_content_control_value","target":"p_2",
        "sdt_id":ids[1],"text":"Gamma GmbH"}),
    )
    .expect("the fill applies");
    let xml = String::from_utf8(
        stemma::docx::DocxArchive::read(&filled.serialize(&ExportOptions::default()).unwrap())
            .unwrap()
            .get("word/document.xml")
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(xml.contains("Acme Ltd"), "the buyer keeps its value: {xml}");
    assert!(xml.contains("Gamma GmbH"));
    assert!(!xml.contains("Beta Corp"));
}
