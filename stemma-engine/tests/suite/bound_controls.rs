//! An edit to a content control bound to a data store reaches the store.
//!
//! Domain rule (§17.5.2.6, observed with Word): a control with a
//! `w:dataBinding` shows the value of its store node, and Word refills the
//! control from the store when it opens the document. A tracked edit of the
//! control alone was undone by Word on open; with the bound node holding the
//! control's new text Word kept the tracked edit, Accept gave the new text
//! and Reject the old. When Word edits a bound control it writes the new
//! text into the store, so an edit here must too. Stores nobody edited keep
//! every byte.

use stemma::api::Document;
use stemma::domain::{BlockNode, InlineNode, OpaqueKind};
use stemma::{ExportOptions, Resolution};

const CORE: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Civil Appeal No. 82/12</dc:title><dc:creator>Registrar</dc:creator></cp:coreProperties>"#;

const TITLE_BINDING: &str = r#"<w:dataBinding w:prefixMappings="xmlns:ns0='http://schemas.openxmlformats.org/package/2006/metadata/core-properties' xmlns:ns1='http://purl.org/dc/elements/1.1/'" w:xpath="/ns0:coreProperties[1]/ns1:title[1]" w:storeItemID="{6C3C8BC8-F283-45AE-878A-BAB7291924A1}"/>"#;

fn docx() -> Vec<u8> {
    let header_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:tbl><w:tblGrid><w:gridCol w:w="4000"/><w:gridCol w:w="4000"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>Judgment No. 66/14</w:t></w:r></w:p></w:tc><w:tc><w:sdt><w:sdtPr><w:alias w:val="Title"/><w:id w:val="78735415"/>{TITLE_BINDING}<w:text/></w:sdtPr><w:sdtContent><w:p><w:pPr><w:jc w:val="right"/></w:pPr><w:r><w:t>Civil Appeal No. 82/12</w:t></w:r></w:p></w:sdtContent></w:sdt></w:tc></w:tr></w:tbl><w:p/></w:hdr>"#
    );
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body><w:p><w:r><w:t xml:space="preserve">In the matter of </w:t></w:r><w:sdt><w:sdtPr><w:id w:val="5"/>{TITLE_BINDING}<w:text/></w:sdtPr><w:sdtContent><w:r><w:t>Civil Appeal No. 82/12</w:t></w:r></w:sdtContent></w:sdt><w:r><w:t>.</w:t></w:r></w:p><w:p><w:r><w:t>The appellant was convicted.</w:t></w:r></w:p><w:sectPr><w:headerReference w:type="default" r:id="rIdH1"/><w:pgSz w:w="12240" w:h="15840"/></w:sectPr></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/><Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdH1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/></Relationships>"#;
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
            ("word/header1.xml", header_xml.as_str()),
            ("docProps/core.xml", CORE),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

fn apply(doc: &Document, op: serde_json::Value) -> Result<Document, String> {
    let json = serde_json::json!({"ops":[op],"revision":{"author":"Registrar"}}).to_string();
    let txn = stemma::edit_v4::parse_transaction(&json)
        .map_err(|e| format!("{e:?}"))?
        .into_edit_transaction()
        .map_err(|e| format!("{e:?}"))?;
    doc.apply(&txn).map_err(|e| e.to_string())
}

fn part(doc: &Document, name: &str) -> String {
    let bytes = doc.serialize(&ExportOptions::default()).expect("serialize");
    String::from_utf8(
        stemma::docx::DocxArchive::read(&bytes)
            .unwrap()
            .get(name)
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

/// The paragraph inside the header's bound control (right cell).
fn header_title_paragraph(doc: &Document) -> String {
    let BlockNode::Table(table) = &doc.snapshot().canonical.headers[0].blocks[0].block else {
        panic!("the header starts with its table");
    };
    match &table.rows[0].cells[1].blocks[0] {
        BlockNode::Paragraph(p) => p.id.to_string(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_tracked_edit_of_a_bound_header_control_writes_the_store() {
    let doc = Document::parse(&docx()).expect("parse");
    let edited = apply(
        &doc,
        serde_json::json!({"op":"edit_header","header_part":"header1.xml",
            "target":header_title_paragraph(&doc),"expect":"Civil Appeal No. 82/12",
            "content":[{"type":"text","text":"Criminal Appeal No. 82/12"}]}),
    )
    .expect("the header edit applies");
    let core = part(&edited, "docProps/core.xml");
    assert_eq!(
        core,
        CORE.replace("Civil Appeal No. 82/12", "Criminal Appeal No. 82/12"),
        "the bound node holds the control's new text, every other byte kept"
    );
    let header = part(&edited, "word/header1.xml");
    assert!(
        header.contains("<w:delText>Civil</w:delText>") || header.contains("Civil</w:delText>"),
        "the control's edit stays a tracked change: {header}"
    );
    let reopened = Document::parse(&edited.serialize(&ExportOptions::default()).unwrap()).unwrap();
    let differences = stemma::roundtrip_compare::compare_canon_docs(
        &edited.snapshot().canonical,
        &reopened.snapshot().canonical,
    );
    assert!(
        differences.is_empty(),
        "{:?}",
        &differences[..differences.len().min(2)]
    );
}

#[test]
fn filling_a_bound_body_control_writes_the_store() {
    let doc = Document::parse(&docx()).expect("parse");
    let canonical = &doc.snapshot().canonical;
    let BlockNode::Paragraph(paragraph) = &canonical.blocks[0].block else {
        panic!("the body starts with the paragraph holding the control");
    };
    let sdt = paragraph
        .segments
        .iter()
        .flat_map(|s| &s.inlines)
        .find_map(|inline| match inline {
            InlineNode::OpaqueInline(o) if matches!(o.kind, OpaqueKind::Sdt) => {
                Some(o.id.to_string())
            }
            _ => None,
        })
        .expect("an inline control");
    let edited = apply(
        &doc,
        serde_json::json!({"op":"set_content_control_value","target":paragraph.id.to_string(),
            "sdt_id":sdt,"text":"Criminal Appeal No. 82/12"}),
    )
    .expect("the fill applies");
    assert!(
        part(&edited, "docProps/core.xml")
            .contains("<dc:title>Criminal Appeal No. 82/12</dc:title>")
    );
}

#[test]
fn an_edit_elsewhere_leaves_the_store_untouched() {
    let doc = Document::parse(&docx()).expect("parse");
    let body = doc.read().blocks[1].id.to_string();
    let edited = apply(
        &doc,
        serde_json::json!({"op":"replace","target":body,"expect":"The appellant was convicted.",
            "content":{"type":"paragraph","content":[{"type":"text","text":"The appellant was acquitted."}]}}),
    )
    .expect("the body edit applies");
    assert_eq!(part(&edited, "docProps/core.xml"), CORE);
    let accepted = edited.project(Resolution::AcceptAll).expect("accept");
    assert_eq!(part(&accepted, "docProps/core.xml"), CORE);
}

/// Word refills a bound control from its store on open, so a resolved
/// document reads as resolved in Word only when its store holds what the
/// control now shows: the rejected edit's old text, the accepted edit's new
/// text.
#[test]
fn resolving_a_tracked_edit_of_a_bound_control_carries_the_store() {
    let doc = Document::parse(&docx()).expect("parse");
    let edited = apply(
        &doc,
        serde_json::json!({"op":"edit_header","header_part":"header1.xml",
            "target":header_title_paragraph(&doc),"expect":"Civil Appeal No. 82/12",
            "content":[{"type":"text","text":"Criminal Appeal No. 82/12"}]}),
    )
    .expect("the header edit applies");
    let rejected = edited.project(Resolution::RejectAll).expect("reject");
    assert_eq!(part(&rejected, "docProps/core.xml"), CORE);
    let accepted = edited.project(Resolution::AcceptAll).expect("accept");
    assert_eq!(
        part(&accepted, "docProps/core.xml"),
        CORE.replace("Civil Appeal No. 82/12", "Criminal Appeal No. 82/12")
    );
}
