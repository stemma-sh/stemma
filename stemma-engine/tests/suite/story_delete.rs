//! A paragraph in a header or footer can be deleted.
//!
//! Domain rule: a header or footer is a story like the body (§17.10); a
//! line of it (a stale file path under the page number) is removed as a body
//! paragraph is, addressed with its part because ids are story-local. The
//! story keeps at least one paragraph, as every story must.

use stemma::ExportOptions;
use stemma::api::Document;

fn docx() -> Vec<u8> {
    let footer_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:ftr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:r><w:t>Page 1</w:t></w:r></w:p><w:p><w:r><w:t>Cc/docs/elections/resolution</w:t></w:r></w:p></w:ftr>"#;
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body><w:p><w:r><w:t>Resolution text.</w:t></w:r></w:p><w:sectPr><w:footerReference w:type="default" r:id="rIdF1"/><w:pgSz w:w="12240" w:h="15840"/></w:sectPr></w:body></w:document>"#;
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
            ("word/footer1.xml", footer_xml),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

fn delete(doc: &Document, target: &str, expect: &str, mode: &str) -> Result<Document, String> {
    let json = serde_json::json!({"ops":[{"op":"delete","footer_part":"footer1.xml","target":target,"expect":expect}],
        "materialization_mode":mode,"revision":{"author":"Clerk"}})
    .to_string();
    let txn = stemma::edit_v4::parse_transaction(&json)
        .map_err(|e| format!("{e:?}"))?
        .into_edit_transaction()
        .map_err(|e| format!("{e:?}"))?;
    doc.apply(&txn).map_err(|e| e.to_string())
}

fn footer_texts(doc: &Document) -> Vec<String> {
    doc.snapshot().canonical.footers[0]
        .blocks
        .iter()
        .map(|b| match &b.block {
            stemma::domain::BlockNode::Paragraph(p) => p
                .segments
                .iter()
                .flat_map(|s| &s.inlines)
                .filter_map(|i| match i {
                    stemma::domain::InlineNode::Text(t) => Some(t.text.as_str()),
                    _ => None,
                })
                .collect(),
            other => panic!("{other:?}"),
        })
        .collect()
}

#[test]
fn a_footer_paragraph_is_deleted_tracked_and_direct() {
    let doc = Document::parse(&docx()).expect("parse");
    let stemma::domain::BlockNode::Paragraph(path_line) =
        &doc.snapshot().canonical.footers[0].blocks[1].block
    else {
        panic!("the footer's second block is the path line");
    };
    let path_line = path_line.id.to_string();
    for mode in ["tracked_change", "direct"] {
        let edited = delete(&doc, &path_line, "Cc/docs/elections/resolution", mode)
            .unwrap_or_else(|e| panic!("{mode}: {e}"));
        let accepted = edited.read_accepted().expect("accept");
        assert_eq!(
            footer_texts(&accepted),
            ["Page 1"],
            "{mode}: the path line is gone"
        );
        if mode == "tracked_change" {
            let rejected = edited.read_rejected().expect("reject");
            assert_eq!(
                footer_texts(&rejected),
                ["Page 1", "Cc/docs/elections/resolution"],
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
