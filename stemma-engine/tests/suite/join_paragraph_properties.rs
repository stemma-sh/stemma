//! A joined paragraph keeps the first paragraph's properties.
//!
//! Domain rule (observed with the Word oracle, Delete at the end of a
//! paragraph): the joined paragraph takes the FIRST paragraph's paragraph
//! properties, and the surviving paragraph mark keeps its own run properties.
//! Tracked, Word deletes the first paragraph's mark and records the second
//! paragraph's old properties in a pPrChange, so Accept gives the first's
//! properties and Reject gives back both paragraphs as they were. Accepting a
//! bare deleted mark would keep the second paragraph's properties instead.

use stemma::ExportOptions;
use stemma::api::Document;

fn docx() -> Vec<u8> {
    let w = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{w}"><w:body><w:p><w:pPr><w:jc w:val="center"/><w:rPr><w:b/></w:rPr></w:pPr><w:r><w:t xml:space="preserve">The injury happened where he </w:t></w:r></w:p><w:p><w:pPr><w:spacing w:after="240"/><w:ind w:firstLine="0"/><w:rPr><w:i/></w:rPr></w:pPr><w:r><w:t>had been taken to the hospital.</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#
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

fn join(doc: &Document, mode: &str) -> Document {
    let first = doc.read().blocks[0].id.to_string();
    let second = doc.read().blocks[1].id.to_string();
    let json = serde_json::json!({"ops":[{"op":"join_paragraphs","target":first,"with":second,
        "expect":"where he"}],"materialization_mode":mode,"revision":{"author":"Editor"}})
    .to_string();
    doc.apply(
        &stemma::edit_v4::parse_transaction(&json)
            .unwrap()
            .into_edit_transaction()
            .unwrap(),
    )
    .unwrap_or_else(|e| panic!("{mode}: {e}"))
}

fn paragraphs(doc: &Document) -> Vec<String> {
    let bytes = doc.serialize(&ExportOptions::default()).expect("save");
    let xml = String::from_utf8(
        stemma::docx::DocxArchive::read(&bytes)
            .unwrap()
            .get("word/document.xml")
            .unwrap()
            .to_vec(),
    )
    .unwrap()
    .replace(" />", "/>");
    xml.split("<w:p>")
        .chain(xml.split("<w:p ").skip(1))
        .filter(|p| p.contains("</w:p>"))
        .map(|p| p[..p.find("</w:p>").unwrap()].to_string())
        .collect()
}

/// The pPr of a serialized paragraph, without its pPrChange.
fn live_ppr(paragraph: &str) -> &str {
    let start = paragraph.find("<w:pPr>").map_or(0, |i| i + "<w:pPr>".len());
    let end = paragraph
        .find("<w:pPrChange")
        .or_else(|| paragraph.find("</w:pPr>"))
        .unwrap_or(start);
    &paragraph[start..end]
}

#[test]
fn a_direct_join_keeps_the_first_paragraphs_properties_and_its_own_mark() {
    let joined = join(&Document::parse(&docx()).expect("parse"), "direct");
    let paragraphs = paragraphs(&joined);
    let merged = paragraphs
        .iter()
        .find(|p| p.contains("where he"))
        .expect("the joined paragraph");
    let ppr = live_ppr(merged);
    assert!(
        ppr.contains(r#"<w:jc w:val="center"/>"#),
        "the first paragraph's alignment: {merged}"
    );
    assert!(
        !ppr.contains("w:spacing") && !ppr.contains("w:ind"),
        "none of the second paragraph's properties: {merged}"
    );
    assert!(
        ppr.contains("<w:i/>") && !ppr.contains("<w:b/>"),
        "the surviving mark keeps its own run properties: {merged}"
    );
}

#[test]
fn a_tracked_join_records_the_second_paragraphs_properties() {
    let doc = Document::parse(&docx()).expect("parse");
    let tracked = join(&doc, "tracked_change");
    let tracked_paragraphs = paragraphs(&tracked);
    let second = tracked_paragraphs
        .iter()
        .find(|p| p.contains("had been taken"))
        .expect("the second paragraph");
    let ppr = live_ppr(second);
    assert!(
        ppr.contains(r#"<w:jc w:val="center"/>"#) && !ppr.contains("w:spacing"),
        "the second paragraph shows the first's properties: {second}"
    );
    assert!(
        second.contains("<w:pPrChange")
            && second.contains(r#"w:after="240""#)
            && second.contains(r#"w:firstLine="0""#),
        "its old properties are recorded: {second}"
    );

    let accepted = tracked.read_accepted().expect("accept");
    let merged = paragraphs_of_accepted(&accepted);
    assert!(
        merged.contains(r#"<w:jc w:val="center"/>"#) && !merged.contains("w:spacing"),
        "accept gives the first paragraph's properties: {merged}"
    );

    let rejected = tracked.read_rejected().expect("reject");
    let both = paragraphs(&rejected);
    let restored = both
        .iter()
        .find(|p| p.contains("had been taken"))
        .expect("the second paragraph");
    assert!(
        restored.contains(r#"w:after="240""#) && !live_ppr(restored).contains("w:jc"),
        "reject gives back the second paragraph's own properties: {restored}"
    );
}

fn paragraphs_of_accepted(doc: &Document) -> String {
    paragraphs(doc)
        .into_iter()
        .find(|p| p.contains("where he"))
        .expect("the joined paragraph")
}
