//! Numbering that names a list but no level, live and as a tracked
//! paragraph-property change's previous numbering, in a document without a
//! styles part.
//!
//! Domain rule: CT_NumPr's numId and ilvl are each schema-optional
//! (§17.3.1.19). Word 16 puts such a paragraph at its style's level, and with
//! no style level at level 0; Reject All writes the resolved level into the
//! restored live `w:numPr`. The style cases are in
//! `spec_numbering_list_without_level`.

use stemma::api::Document;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn docx(paragraph_properties: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W_NS}"><w:body><w:p><w:pPr>{paragraph_properties}</w:pPr><w:r><w:t>Listed item</w:t></w:r></w:p></w:body></w:document>"#
    );
    let numbering_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="{W_NS}"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let document_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/></Relationships>"#;
    use std::io::Write;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions = zip::write::FileOptions::default();
        for (name, data) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/_rels/document.xml.rels", document_rels),
            ("word/document.xml", document_xml.as_str()),
            ("word/numbering.xml", numbering_xml.as_str()),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

fn saved_document_xml(doc: &Document) -> String {
    let saved = doc
        .serialize(&stemma::ExportOptions::default())
        .expect("save");
    String::from_utf8(
        stemma::docx::DocxArchive::read(&saved)
            .unwrap()
            .get("word/document.xml")
            .unwrap()
            .to_vec(),
    )
    .unwrap()
    .replace(" />", "/>")
}

#[test]
fn numbering_with_a_list_but_no_level_opens_at_level_zero_without_styles() {
    let direct = Document::parse(&docx(r#"<w:numPr><w:numId w:val="1"/></w:numPr>"#))
        .expect("a list without a level opens");
    let list = direct.read().blocks[0].list.clone().expect("numbered");
    assert_eq!((list.num_id, list.ilvl), (1, 0));
    assert!(
        saved_document_xml(&direct).contains(r#"<w:numPr><w:numId w:val="1"/></w:numPr>"#),
        "an untouched paragraph keeps the numPr as written"
    );

    let tracked = Document::parse(&docx(
        r#"<w:ind w:left="720"/><w:pPrChange w:id="1" w:author="Reviewer" w:date="2026-01-01T00:00:00Z"><w:pPr><w:numPr><w:numId w:val="1"/></w:numPr></w:pPr></w:pPrChange>"#,
    ))
    .expect("a previous list without a level opens");
    let rejected = tracked
        .project(stemma::Resolution::RejectAll)
        .expect("reject all");
    let list = rejected.read().blocks[0].list.clone().expect("numbered");
    assert_eq!((list.num_id, list.ilvl), (1, 0));
    assert!(
        saved_document_xml(&rejected)
            .contains(r#"<w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr>"#),
        "Word's Reject All writes the resolved level"
    );
}

#[test]
fn previous_numbering_with_a_complete_pair_still_opens_and_rejects_to_it() {
    let doc = Document::parse(&docx(
        r#"<w:ind w:left="720"/><w:pPrChange w:id="1" w:author="Reviewer" w:date="2026-01-01T00:00:00Z"><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr></w:pPrChange>"#,
    ))
    .expect("a complete previous numbering pair opens");
    let rejected = doc
        .project(stemma::Resolution::RejectAll)
        .expect("reject all");
    let saved = rejected
        .serialize(&stemma::ExportOptions::default())
        .expect("save");
    let xml = String::from_utf8(
        stemma::docx::DocxArchive::read(&saved)
            .unwrap()
            .get("word/document.xml")
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(
        xml.contains(r#"<w:ilvl w:val="0""#) && xml.contains(r#"<w:numId w:val="1""#),
        "rejecting restores the previous numbering: {xml}"
    );
    Document::parse(&saved).expect("the rejected document reopens");
}
