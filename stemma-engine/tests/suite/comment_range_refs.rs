//! A comment's range markers can be named in a replacement.
//!
//! Domain rule (§17.13.4.3-5): a comment's range start, range end and
//! reference are structural markers; the engine keeps them on the text they
//! bracket when the paragraph is rewritten. The reads show each marker as an
//! anchor with its own id (the comment id is in its metadata), so a
//! whole-paragraph `replace` may name them where they belong, as it may name
//! the reference, or leave them out; either way the comment stays on its text
//! and appears once.

use stemma::ExportOptions;
use stemma::api::{Document, validate};

const COMMENTED_PARAGRAPH: &str = r#"<w:p><w:r><w:t xml:space="preserve">The notice period is </w:t></w:r><w:commentRangeStart w:id="0"/><w:r><w:t xml:space="preserve">30 days</w:t></w:r><w:commentRangeEnd w:id="0"/><w:r><w:commentReference w:id="0"/></w:r><w:r><w:t xml:space="preserve"> from the date of this letter.</w:t></w:r></w:p>"#;

fn docx() -> Vec<u8> {
    package(COMMENTED_PARAGRAPH)
}

/// The commented paragraph as the one paragraph of a one-cell table.
fn docx_in_cell() -> Vec<u8> {
    package(&format!(
        r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid><w:tr><w:tc>{COMMENTED_PARAGRAPH}</w:tc></w:tr></w:tbl><w:p/>"#
    ))
}

fn package(body: &str) -> Vec<u8> {
    let w = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{w}"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let comments_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:comments xmlns:w="{w}"><w:comment w:id="0" w:author="Partner" w:date="2026-10-01T09:00:00Z" w:initials="P"><w:p><w:r><w:t>Check this period.</w:t></w:r></w:p></w:comment></w:comments>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/comments.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdCm" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="comments.xml"/></Relationships>"#;
    use std::io::Write;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions = zip::write::FileOptions::default();
        for (name, data) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/document.xml", document_xml.as_str()),
            ("word/comments.xml", comments_xml.as_str()),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

/// The anchor ids the read shows for paragraph 1, by kind.
fn anchors(doc: &Document) -> Vec<(String, String)> {
    let view = serde_json::to_value(&doc.read().blocks[0]).unwrap();
    let mut out = Vec::new();
    fn walk(value: &serde_json::Value, out: &mut Vec<(String, String)>) {
        match value {
            serde_json::Value::Object(map) => {
                if let (Some(kind), Some(id)) = (map.get("kind"), map.get("id"))
                    && let (Some(kind), Some(id)) = (kind.as_str(), id.as_str())
                    && kind.starts_with("Comment")
                {
                    out.push((kind.to_string(), id.to_string()));
                }
                map.values().for_each(|v| walk(v, out));
            }
            serde_json::Value::Array(items) => items.iter().for_each(|v| walk(v, out)),
            _ => {}
        }
    }
    walk(&view, &mut out);
    out
}

fn replace(doc: &Document, content: serde_json::Value) -> Result<Document, String> {
    replace_in(doc, "p_1", content)
}

fn replace_in(
    doc: &Document,
    target: &str,
    content: serde_json::Value,
) -> Result<Document, String> {
    let json = serde_json::json!({"ops":[{"op":"replace","target":target,"expect":"30 days",
        "content":{"type":"paragraph","content":content}}],"revision":{"author":"Reviewer"}})
    .to_string();
    let txn = stemma::edit_v4::parse_transaction(&json)
        .map_err(|e| format!("{e:?}"))?
        .into_edit_transaction()
        .map_err(|e| format!("{e:?}"))?;
    doc.apply(&txn).map_err(|e| e.to_string())
}

/// The commented span and the marker counts of the saved accepted reading.
fn commented(doc: &Document) -> (String, usize, usize, usize) {
    let accepted = doc.read_accepted().expect("accept");
    let bytes = accepted.serialize(&ExportOptions::default()).expect("save");
    assert!(validate(&bytes).ok, "the saved document validates");
    let xml = String::from_utf8(
        stemma::docx::DocxArchive::read(&bytes)
            .unwrap()
            .get("word/document.xml")
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    let start = xml.find("<w:commentRangeStart").expect("range start");
    let end = xml.find("<w:commentRangeEnd").expect("range end");
    let mut span = String::new();
    let mut rest = &xml[start..end];
    while let Some(at) = rest.find("<w:t") {
        rest = &rest[at..];
        let open = rest.find('>').unwrap() + 1;
        let close = rest.find("</w:t>").unwrap();
        span.push_str(&rest[open..close]);
        rest = &rest[close + 6..];
    }
    (
        span,
        xml.matches("<w:commentRangeStart").count(),
        xml.matches("<w:commentRangeEnd").count(),
        xml.matches("<w:commentReference").count(),
    )
}

#[test]
fn the_range_markers_have_their_own_ids() {
    let doc = Document::parse(&docx()).expect("parse");
    let anchors = anchors(&doc);
    let start = anchors
        .iter()
        .find(|(kind, _)| kind == "CommentRangeStart")
        .expect("start");
    let end = anchors
        .iter()
        .find(|(kind, _)| kind == "CommentRangeEnd")
        .expect("end");
    assert_ne!(
        start.1, end.1,
        "the start and end are told apart: {anchors:?}"
    );
}

#[test]
fn a_replacement_naming_the_markers_keeps_the_comment_on_its_text() {
    let doc = Document::parse(&docx()).expect("parse");
    let anchors = anchors(&doc);
    let id_of = |kind: &str| {
        anchors
            .iter()
            .find(|(k, _)| k == kind)
            .map(|(_, id)| id.clone())
            .unwrap()
    };
    let reference = doc.read().blocks[0].clone();
    let reference = serde_json::to_value(&reference).unwrap().to_string();
    let reference_id = reference
        .split("\"id\":\"")
        .filter_map(|s| s.split('"').next())
        .find(|id| id.contains("widget"))
        .expect("the comment reference anchor")
        .to_string();
    let edited = replace(
        &doc,
        serde_json::json!([
            {"type":"text","text":"The notice period is "},
            {"type":"opaque_ref","attrs":{"id":id_of("CommentRangeStart")}},
            {"type":"text","text":"45 days"},
            {"type":"opaque_ref","attrs":{"id":id_of("CommentRangeEnd")}},
            {"type":"opaque_ref","attrs":{"id":reference_id}},
            {"type":"text","text":" from the date of this letter."}
        ]),
    )
    .expect("the markers may be named");
    assert_eq!(commented(&edited), ("45 days".to_string(), 1, 1, 1));
}

#[test]
fn a_replacement_leaving_the_markers_out_keeps_the_comment_on_its_text() {
    let doc = Document::parse(&docx()).expect("parse");
    let edited = replace(
        &doc,
        serde_json::json!([{"type":"text","text":"The notice period is 45 days from the date of this letter."}]),
    )
    .expect("the markers may be left out");
    assert_eq!(commented(&edited), ("45 days".to_string(), 1, 1, 1));
}

/// A cell paragraph's segments carry the same marker ids the body read shows:
/// the range start as `comment_<id>_start`, never the bare comment id, which
/// names the comment (it is the marker's `reference_id`).
#[test]
fn a_cell_paragraphs_range_start_has_the_id_the_body_read_gives_it() {
    let doc = Document::parse(&docx_in_cell()).expect("parse");
    let cell = serde_json::to_value(&doc.read().blocks[0].cells[0].paragraphs[0]).unwrap();
    let block_id = cell["block_id"].as_str().unwrap().to_string();
    let opaques: Vec<(String, String)> = cell["segments"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|segment| segment.get("Opaque"))
        .map(|o| {
            (
                o["opaque_id"].as_str().unwrap().to_string(),
                o["reference_id"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let start = opaques
        .first()
        .expect("the range start comes first")
        .clone();
    assert_eq!(start, ("comment_0_start".to_string(), "0".to_string()));
    let reference_id = opaques
        .iter()
        .find(|(id, _)| id.contains("widget"))
        .expect("the comment reference anchor")
        .0
        .clone();

    let edited = replace_in(
        &doc,
        &block_id,
        serde_json::json!([
            {"type":"text","text":"The notice period is "},
            {"type":"opaque_ref","attrs":{"id":start.0}},
            {"type":"text","text":"45 days"},
            {"type":"opaque_ref","attrs":{"id":reference_id}},
            {"type":"text","text":" from the date of this letter."}
        ]),
    )
    .expect("the marker named by its cell-segment id is found");
    assert_eq!(commented(&edited), ("45 days".to_string(), 1, 1, 1));
}
