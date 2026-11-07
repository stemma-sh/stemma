//! A paragraph is not moved to a list level its list does not define.
//!
//! Domain rule (§17.9.6: a numbering level is defined by its abstract
//! numbering's w:lvl): indenting an item onto a level the list lacks leaves
//! it unnumbered in Word, and the document reopens with its numbering
//! unresolved. The edit is refused, naming the list and the level.

use stemma::api::Document;

#[test]
fn indenting_onto_an_undefined_level_is_refused() {
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/spec-compliance/numbering/start-at-override/input.docx"
    ))
    .expect("fixture");
    let doc = Document::parse(&bytes).expect("parse");
    let item = doc
        .read()
        .blocks
        .iter()
        .find(|b| b.list.is_some())
        .map(|b| b.id.to_string())
        .expect("a list item");
    let json =
        serde_json::json!({"ops":[{"op":"set_numbering","target":item,"change":{"kind":"indent"}}],
        "revision":{"author":"Editor"}})
        .to_string();
    let Err(err) = doc.apply(
        &stemma::edit_v4::parse_transaction(&json)
            .unwrap()
            .into_edit_transaction()
            .unwrap(),
    ) else {
        panic!("the list defines no level 1");
    };
    assert!(
        err.message.contains("defines no such level"),
        "{}",
        err.message
    );
}

fn two_level_list_docx() -> Vec<u8> {
    let numbering = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="bullet"/><w:lvlText w:val="•"/><w:lvlJc w:val="left"/><w:pPr><w:ind w:left="720" w:hanging="360"/></w:pPr></w:lvl><w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="bullet"/><w:lvlText w:val="o"/><w:lvlJc w:val="left"/><w:pPr><w:ind w:left="1440" w:hanging="360"/></w:pPr></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#;
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr><w:ind w:left="720" w:hanging="360"/></w:pPr><w:r><w:t>Blood pressure</w:t></w:r></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr><w:ind w:left="720" w:hanging="360"/></w:pPr><w:r><w:t>Systolic pressure</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdN" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/></Relationships>"#;
    use std::io::Write;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions = zip::write::FileOptions::default();
        for (name, data) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/document.xml", document_xml),
            ("word/numbering.xml", numbering),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

/// Indenting a list item drops its direct indentation, as Word's Increase
/// Indent does (observed: ListIndent removes a direct w:ind into the
/// pPrChange), so the item takes its new level's indent; kept, a direct
/// indent equal to the old level's left it where it was.
#[test]
fn indenting_a_list_item_takes_the_new_levels_indent() {
    let doc = Document::parse(&two_level_list_docx()).expect("parse");
    let item = doc.read().blocks[1].id.to_string();
    let json =
        serde_json::json!({"ops":[{"op":"set_numbering","target":item,"change":{"kind":"indent"}}],
        "revision":{"author":"Editor"}})
        .to_string();
    let edited = doc
        .apply(
            &stemma::edit_v4::parse_transaction(&json)
                .unwrap()
                .into_edit_transaction()
                .unwrap(),
        )
        .expect("the list defines level 1");
    let stemma::domain::BlockNode::Paragraph(p) = &edited.snapshot().canonical.blocks[1].block
    else {
        panic!("paragraph")
    };
    assert_eq!(p.numbering.as_ref().map(|n| n.ilvl), Some(1));
    assert!(!p.has_direct_indent, "the direct indent is dropped");
    assert_eq!(
        p.indent.as_ref().and_then(|i| i.left),
        Some(1440),
        "level 1's indent applies"
    );
    let bytes = edited
        .serialize(&stemma::ExportOptions::default())
        .expect("save");
    let reopened = Document::parse(&bytes).expect("reopen");
    let differences = stemma::roundtrip_compare::compare_canon_docs(
        &edited.snapshot().canonical,
        &reopened.snapshot().canonical,
    );
    assert!(
        differences.is_empty(),
        "{:?}",
        &differences[..differences.len().min(2)]
    );
    let rejected = edited.read_rejected().expect("reject");
    let stemma::domain::BlockNode::Paragraph(r) = &rejected.snapshot().canonical.blocks[1].block
    else {
        panic!("paragraph")
    };
    assert_eq!(
        r.numbering.as_ref().map(|n| n.ilvl),
        Some(0),
        "reject restores the level"
    );
    assert!(r.has_direct_indent, "and the direct indent");
}
