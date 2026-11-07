//! Underline colour survives editing.
//!
//! Domain rule (§17.3.2.40): `w:u` carries the underline's style in `w:val` and
//! its colour in `w:color` (or a theme colour with tint/shade). A red underline
//! under black text is part of how the run looks; writing the run back without
//! the colour turns the underline black. Any edit rewrites paragraphs from the
//! model, so the model must carry the colour — an edit elsewhere in the
//! document used to drop it from every underlined run.

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

const BODY: &str = r#"<w:p><w:r><w:rPr><w:u w:val="thick" w:color="FF0000"/></w:rPr><w:t>Red underlined heading</w:t></w:r></w:p><w:p><w:r><w:rPr><w:u w:val="single" w:themeColor="accent1" w:themeShade="BF"/></w:rPr><w:t>Theme underlined words</w:t></w:r></w:p><w:p><w:r><w:t>Plain closing paragraph.</w:t></w:r></w:p>"#;

fn underlines(doc: &Document) -> Vec<String> {
    let bytes = doc.serialize(&ExportOptions::default()).expect("serialize");
    let xml = String::from_utf8(
        stemma::docx::DocxArchive::read(&bytes)
            .expect("read")
            .get("word/document.xml")
            .expect("document part")
            .to_vec(),
    )
    .expect("utf-8");
    xml.split("<w:u ")
        .skip(1)
        .map(|rest| {
            rest[..rest.find("/>").expect("empty element")]
                .trim()
                .to_string()
        })
        .collect()
}

fn replace(doc: &Document, index: usize, expect: &str, text: &str) -> Document {
    let id = doc.read().blocks[index].id.to_string();
    let json = format!(
        r#"{{"ops":[{{"op":"replace","target":"{id}","expect":"{expect}","content":{{"type":"paragraph","content":[{{"type":"text","text":"{text}"}}]}}}}],"revision":{{"author":"Editor"}}}}"#
    );
    doc.apply(
        &stemma::edit_v4::parse_transaction(&json)
            .expect("parse")
            .into_edit_transaction()
            .expect("adapt"),
    )
    .expect("apply")
}

#[test]
fn an_edit_elsewhere_keeps_every_underline_colour() {
    let doc = Document::parse(&docx(BODY)).expect("parse");
    let edited = replace(&doc, 2, "Plain closing", "Plain final paragraph.");
    let accepted = edited.project(Resolution::AcceptAll).expect("accept");
    for document in [&edited, &accepted] {
        let found = underlines(document);
        assert!(
            found
                .iter()
                .any(|u| u.contains(r#"w:val="thick""#) && u.contains(r#"w:color="FF0000""#)),
            "{found:?}"
        );
        assert!(
            found
                .iter()
                .any(|u| u.contains(r#"w:themeColor="accent1""#)
                    && u.contains(r#"w:themeShade="BF""#)),
            "{found:?}"
        );
    }
}

#[test]
fn typing_into_an_underlined_run_keeps_its_colour() {
    let doc = Document::parse(&docx(BODY)).expect("parse");
    let edited = replace(&doc, 0, "Red underlined", "Red underlined title");
    let accepted = edited.project(Resolution::AcceptAll).expect("accept");
    let found = underlines(&accepted);
    assert!(
        !found.is_empty()
            && found
                .iter()
                .filter(|u| u.contains(r#"w:val="thick""#))
                .all(|u| u.contains(r#"w:color="FF0000""#)),
        "every thick underline is still red: {found:?}"
    );
}

/// `w:val="none"` with a colour is how Word writes an underline turned off
/// over an inherited one (the colour stays on the element). The model carries
/// that colour, so an edit elsewhere writes it back: on the run and on the
/// paragraph mark, the document reads the same after a save and reopen.
#[test]
fn an_edit_elsewhere_keeps_a_turned_off_underlines_colour() {
    let body = concat!(
        r#"<w:p><w:pPr><w:rPr><w:u w:val="none" w:color="auto"/></w:rPr></w:pPr>"#,
        r#"<w:r><w:rPr><w:u w:val="none" w:color="auto"/></w:rPr><w:t>No underline here</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>Plain closing paragraph.</w:t></w:r></w:p>"#,
    );
    let doc = Document::parse(&docx(body)).expect("parse");
    let edited = replace(&doc, 1, "Plain closing", "Plain final paragraph.");
    let found = underlines(&edited);
    assert_eq!(
        found
            .iter()
            .filter(|u| u.contains(r#"w:val="none""#) && u.contains(r#"w:color="auto""#))
            .count(),
        2,
        "the run's and the paragraph mark's: {found:?}"
    );
    let reopened = Document::parse(
        &edited
            .serialize(&ExportOptions::default())
            .expect("serialize"),
    )
    .expect("reopen");
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
