//! A footnote with a custom mark keeps its mark in the reference's run.
//!
//! Domain rule (§17.11.14, `customMarkFollows`): a footnote reference whose
//! number is replaced by a custom mark carries that mark as the content that
//! follows it in the same run, here a symbol (`w:sym`, an asterisk). Split
//! into two runs, the reference has no mark and Word attaches it elsewhere on
//! the next save; an edit anywhere in the document rewrites the paragraph, so
//! it must keep the pair in one run.

use stemma::ExportOptions;
use stemma::api::Document;

const BODY: &str = concat!(
    r#"<w:p><w:r><w:t>Officers in the LME</w:t></w:r>"#,
    r#"<w:r><w:footnoteReference w:customMarkFollows="1" w:id="1"/><w:sym w:font="Symbol" w:char="F02A"/></w:r>"#,
    r#"<w:r><w:t>, and others.</w:t></w:r></w:p>"#,
    r#"<w:p><w:r><w:t>An unrelated paragraph.</w:t></w:r></w:p>"#,
);

fn docx() -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>{BODY}<w:sectPr/></w:body></w:document>"#
    );
    let footnotes = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote><w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote><w:footnote w:id="1"><w:p><w:r><w:sym w:font="Symbol" w:char="F02A"/></w:r><w:r><w:t>Counted once.</w:t></w:r></w:p></w:footnote></w:footnotes>"#;
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/footnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes" Target="footnotes.xml"/></Relationships>"#;
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
            ("word/footnotes.xml", footnotes),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

#[test]
fn an_edit_elsewhere_keeps_a_custom_note_mark_in_the_references_run() {
    let doc = Document::parse(&docx()).expect("parse");
    let id = doc.read().blocks[1].id.to_string();
    let json = serde_json::json!({"ops":[{"op":"replace","target":id,"expect":"unrelated",
        "content":{"type":"paragraph","content":[{"type":"text","text":"A different paragraph."}]}}],
        "revision":{"author":"Editor"}})
    .to_string();
    let edited = doc
        .apply(
            &stemma::edit_v4::parse_transaction(&json)
                .unwrap()
                .into_edit_transaction()
                .unwrap(),
        )
        .expect("edit applies");
    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let xml = String::from_utf8(
        stemma::docx::DocxArchive::read(&bytes)
            .unwrap()
            .get("word/document.xml")
            .unwrap()
            .to_vec(),
    )
    .unwrap()
    .replace(
        r#" xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#,
        "",
    );
    let at = xml.find("<w:footnoteReference").expect("the reference");
    let run_end = at + xml[at..].find("</w:r>").expect("its run ends");
    assert!(
        xml[at..run_end].contains("<w:sym "),
        "the custom mark is in the reference's run: {}",
        &xml[at.saturating_sub(80)..run_end + 6]
    );
    let reopened = Document::parse(&bytes).expect("reopen");
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
