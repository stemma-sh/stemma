//! Rejecting a move leaves no empty paragraph where it was moved to.
//!
//! Domain rule (§17.13.5.21/.26, moveTo on a paragraph mark): a paragraph
//! whose content and mark were both moved in is, on Reject, removed whole, as
//! a fully inserted paragraph is; that its neighbour is a table changes
//! nothing. The case is a lead's: a clause moved down to just before a table
//! that also gained a row, where Reject All restored the clause and left an
//! empty paragraph at the destination.

use stemma::ExportOptions;
use stemma::api::{Document, Resolution};

const MOVED_BEFORE_TABLE: &str = r#"<w:p><w:pPr><w:rPr><w:moveFrom w:id="27" w:author="Alice" w:date="2026-10-01T09:00:00Z"/></w:rPr></w:pPr><w:moveFromRangeStart w:id="20" w:author="Alice" w:date="2026-10-01T09:00:00Z" w:name="mv1"/><w:moveFrom w:id="21" w:author="Alice" w:date="2026-10-01T09:00:00Z"><w:r><w:t xml:space="preserve">Notices: in writing.</w:t></w:r></w:moveFrom><w:moveFromRangeEnd w:id="20"/></w:p><w:p><w:r><w:t xml:space="preserve">Section 1: Purpose.</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve">Section 2: Term.</w:t></w:r></w:p><w:p><w:pPr><w:rPr><w:moveTo w:id="28" w:author="Alice" w:date="2026-10-01T09:00:00Z"/></w:rPr></w:pPr><w:moveToRangeStart w:id="22" w:author="Alice" w:date="2026-10-01T09:00:00Z" w:name="mv1"/><w:moveTo w:id="23" w:author="Alice" w:date="2026-10-01T09:00:00Z"><w:r><w:t xml:space="preserve">Notices: in writing.</w:t></w:r></w:moveTo><w:moveToRangeEnd w:id="22"/></w:p><w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="4000"/><w:gridCol w:w="4000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="4000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t xml:space="preserve">Part</w:t></w:r></w:p></w:tc><w:tc><w:tcPr><w:tcW w:w="4000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t xml:space="preserve">Cost</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:trPr><w:ins w:id="24" w:author="Alice" w:date="2026-10-01T09:00:00Z"/></w:trPr><w:tc><w:tcPr><w:tcW w:w="4000" w:type="dxa"/></w:tcPr><w:p><w:ins w:id="25" w:author="Alice" w:date="2026-10-01T09:00:00Z"><w:r><w:t xml:space="preserve">Setup</w:t></w:r></w:ins></w:p></w:tc><w:tc><w:tcPr><w:tcW w:w="4000" w:type="dxa"/></w:tcPr><w:p><w:ins w:id="26" w:author="Alice" w:date="2026-10-01T09:00:00Z"><w:r><w:t xml:space="preserve">USD 300</w:t></w:r></w:ins></w:p></w:tc></w:tr></w:tbl><w:p><w:r><w:t xml:space="preserve">End.</w:t></w:r></w:p>"#;

const INSERTED_BEFORE_TABLE: &str = r#"<w:p><w:r><w:t>Section 1: Purpose.</w:t></w:r></w:p><w:p><w:pPr><w:rPr><w:ins w:id="1" w:author="Alice" w:date="2026-10-01T09:00:00Z"/></w:rPr></w:pPr><w:ins w:id="2" w:author="Alice" w:date="2026-10-01T09:00:00Z"><w:r><w:t>A new clause.</w:t></w:r></w:ins></w:p><w:tbl><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>Item</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:r><w:t>End.</w:t></w:r></w:p>"#;

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

/// The body's top-level paragraphs and tables as text, in order; an empty
/// paragraph reads as "".
fn body_blocks(doc: &Document) -> Vec<String> {
    let bytes = doc.serialize(&ExportOptions::default()).expect("save");
    let reopened = Document::parse(&bytes).expect("reopen");
    reopened
        .read()
        .blocks
        .iter()
        .map(|b| b.text.clone())
        .collect()
}

#[test]
fn reject_all_restores_a_moved_clause_without_an_empty_paragraph() {
    let doc = Document::parse(&docx(MOVED_BEFORE_TABLE)).expect("parse");
    let rejected = doc.project(Resolution::RejectAll).expect("reject all");
    assert_eq!(
        body_blocks(&rejected),
        vec![
            "Notices: in writing.",
            "Section 1: Purpose.",
            "Section 2: Term.",
            "Part Cost",
            "End."
        ],
        "the clause is back at the top and nothing is left where it was moved to"
    );
    let accepted = doc.project(Resolution::AcceptAll).expect("accept all");
    assert_eq!(
        body_blocks(&accepted),
        vec![
            "Section 1: Purpose.",
            "Section 2: Term.",
            "Notices: in writing.",
            "Part Cost Setup USD 300",
            "End."
        ]
    );
}

#[test]
fn reject_all_removes_an_inserted_paragraph_before_a_table() {
    let doc = Document::parse(&docx(INSERTED_BEFORE_TABLE)).expect("parse");
    let rejected = doc.project(Resolution::RejectAll).expect("reject all");
    assert_eq!(
        body_blocks(&rejected),
        vec!["Section 1: Purpose.", "Item", "End."]
    );
}
