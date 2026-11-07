//! A tracked paragraph-property change whose previous numbering names a list
//! but no level is refused at import, as the same direct numbering is.
//!
//! Domain rule: CT_NumPr's numId and ilvl are each schema-optional (§17.3.1.19),
//! and the engine does not invent the missing level for a direct `w:numPr`
//! (import refuses "numId without ilvl" rather than defaulting ilvl to zero).
//! The previous `w:pPr` of a `w:pPrChange` (§17.13.5.29) is the paragraph's
//! direct properties before the change; rejecting the change makes it the
//! paragraph's direct `w:numPr` again. Accepting it in the snapshot while
//! refusing it live gave reject nowhere honest to go: the model invented
//! level 0 in memory and wrote no numbering, and the byte path wrote the
//! partial numPr back, which the engine then refused to open. One rule holds
//! on every path: the partial pair is refused wherever it is read.

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

fn refusal(paragraph_properties: &str) -> String {
    match Document::parse(&docx(paragraph_properties)) {
        Ok(_) => panic!("numbering without a level must be refused: {paragraph_properties}"),
        Err(err) => err.to_string(),
    }
}

#[test]
fn previous_numbering_with_a_list_but_no_level_is_refused_as_the_direct_form_is() {
    let direct = refusal(r#"<w:numPr><w:numId w:val="1"/></w:numPr>"#);
    let previous = refusal(
        r#"<w:ind w:left="720"/><w:pPrChange w:id="1" w:author="Reviewer" w:date="2026-01-01T00:00:00Z"><w:pPr><w:numPr><w:numId w:val="1"/></w:numPr></w:pPr></w:pPrChange>"#,
    );
    for message in [&direct, &previous] {
        assert!(
            message.contains("numId") && message.contains("ilvl"),
            "the refusal names the incomplete pair: {message}"
        );
    }
    assert!(
        previous.contains("pPrChange"),
        "the refusal says the pair is the change's previous state: {previous}"
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
