//! A footer edit naming part of a paragraph replaces just that part.
//!
//! Domain rule: `edit_footer`'s `expect` names text in the footer paragraph
//! and `content` is what replaces it, as its shape reads. A footer holding
//! page-number fields beside a version line ("Page 2 of 9  November 2023 v2")
//! has its version changed with the fields and the rest kept, tracked as a
//! replacement of those words. A part that occurs more than once is refused.
//! Only what differs between `expect` and `content` changes: the characters
//! they share keep their own runs, as an edit of the whole paragraph diffs
//! them (a journal header's padding runs keep their formatting when two of
//! their spaces make room for a restored letter).

use stemma::ExportOptions;
use stemma::api::Document;

const VERSION_LINE: &str = r#"<w:p><w:r><w:t xml:space="preserve">Page </w:t></w:r><w:fldSimple w:instr=" PAGE "><w:r><w:t>2</w:t></w:r></w:fldSimple><w:r><w:t xml:space="preserve"> of </w:t></w:r><w:fldSimple w:instr=" NUMPAGES "><w:r><w:t>9</w:t></w:r></w:fldSimple><w:r><w:t xml:space="preserve">  November 2023 v2 (v2 draft)</w:t></w:r></w:p>"#;

fn docx() -> Vec<u8> {
    docx_with(VERSION_LINE)
}

fn docx_with(paragraph: &str) -> Vec<u8> {
    let footer_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:ftr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">{paragraph}</w:ftr>"#
    );
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body><w:p><w:r><w:t>Guidance text.</w:t></w:r></w:p><w:sectPr><w:footerReference w:type="default" r:id="rIdF1"/><w:pgSz w:w="12240" w:h="15840"/></w:sectPr></w:body></w:document>"#;
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdF1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/></Relationships>"#;
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
            ("word/footer1.xml", footer_xml.as_str()),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

fn edit(doc: &Document, expect: &str, text: &str, mode: &str) -> Result<Document, String> {
    let stemma::domain::BlockNode::Paragraph(p) =
        &doc.snapshot().canonical.footers[0].blocks[0].block
    else {
        panic!("footer paragraph")
    };
    let json = serde_json::json!({"ops":[{"op":"edit_footer","footer_part":"footer1.xml",
        "target":p.id.to_string(),"expect":expect,"content":[{"type":"text","text":text}]}],
        "materialization_mode":mode,"revision":{"author":"Editor"}})
    .to_string();
    let txn = stemma::edit_v4::parse_transaction(&json)
        .map_err(|e| format!("{e:?}"))?
        .into_edit_transaction()
        .map_err(|e| format!("{e:?}"))?;
    doc.apply(&txn).map_err(|e| e.to_string())
}

fn footer_xml(doc: &Document) -> String {
    let bytes = doc.serialize(&ExportOptions::default()).expect("save");
    String::from_utf8(
        stemma::docx::DocxArchive::read(&bytes)
            .unwrap()
            .get("word/footer1.xml")
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

/// The footer's text as a reader sees it (run boundaries do not matter).
fn footer_text(doc: &Document) -> String {
    let xml = footer_xml(doc);
    let mut text = String::new();
    let mut rest = xml.as_str();
    while let Some(at) = rest.find("<w:t") {
        rest = &rest[at..];
        let open_end = rest.find('>').unwrap() + 1;
        if rest[..open_end].ends_with("/>") {
            rest = &rest[open_end..];
            continue;
        }
        let close = rest.find("</w:t>").unwrap();
        text.push_str(&rest[open_end..close]);
        rest = &rest[close..];
    }
    text
}

#[test]
fn a_footer_version_beside_page_fields_is_replaced_alone() {
    let doc = Document::parse(&docx()).expect("parse");
    for mode in ["tracked_change", "direct"] {
        let edited = edit(&doc, "November 2023 v2", "March 2024 v3", mode)
            .unwrap_or_else(|e| panic!("{mode}: {e}"));
        let accepted = edited.read_accepted().expect("accept");
        let xml = footer_xml(&accepted);
        assert!(
            xml.contains(" PAGE ") && xml.contains(" NUMPAGES "),
            "{mode}: fields kept: {xml}"
        );
        assert!(
            xml.contains("March 2024 v3") && !xml.contains("November 2023"),
            "{mode}: {xml}"
        );
        assert!(
            xml.contains("(v2 draft)"),
            "{mode}: the rest of the line is kept: {xml}"
        );
        if mode == "tracked_change" {
            let rejected = footer_text(&edited.read_rejected().expect("reject"));
            assert_eq!(
                rejected, "Page 2 of 9  November 2023 v2 (v2 draft)",
                "reject restores it"
            );
        }
        let reopened = Document::parse(&edited.serialize(&ExportOptions::default()).expect("save"))
            .expect("reopen");
        let differences = stemma::roundtrip_compare::compare_canon_docs(
            &edited.snapshot().canonical,
            &reopened.snapshot().canonical,
        );
        assert!(
            differences.is_empty(),
            "{mode}: {:?}",
            &differences[..differences.len().min(2)]
        );
    }
}

#[test]
fn a_part_that_occurs_twice_is_refused() {
    let doc = Document::parse(&docx()).expect("parse");
    let Err(err) = edit(&doc, "v2", "v3", "tracked_change") else {
        panic!("\"v2\" occurs twice");
    };
    assert!(err.contains("occurs 2 times"), "{err}");
}

#[test]
fn only_what_differs_changes_and_the_padding_runs_keep_their_formatting() {
    let doc = Document::parse(&docx_with(
        r#"<w:p><w:r><w:t>Juli 2024</w:t></w:r><w:r><w:rPr><w:sz w:val="16"/></w:rPr><w:t xml:space="preserve">    </w:t></w:r><w:r><w:rPr><w:lang w:val="en-US"/></w:rPr><w:t xml:space="preserve">   </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>SSN 1979-4940</w:t></w:r></w:p>"#,
    ))
    .expect("parse");
    for mode in ["tracked_change", "direct"] {
        let edited = edit(&doc, "2024       SSN", "2024     ISSN", mode)
            .unwrap_or_else(|e| panic!("{mode}: {e}"));
        let accepted = edited.read_accepted().expect("accept");
        assert_eq!(
            footer_text(&accepted),
            "Juli 2024     ISSN 1979-4940",
            "{mode}"
        );
        let xml = footer_xml(&accepted).replace(" />", "/>");
        assert!(
            xml.contains(r#"<w:sz w:val="16"/></w:rPr><w:t xml:space="preserve">    </w:t>"#),
            "{mode}: the small padding run keeps its four spaces: {xml}"
        );
        assert!(
            xml.contains("SSN 1979-4940") && xml.contains("<w:b/>"),
            "{mode}: the bold run keeps its text and formatting: {xml}"
        );
    }
}
