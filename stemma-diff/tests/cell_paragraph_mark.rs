//! A paragraph mark's run properties (`w:pPr/w:rPr`) are part of the
//! paragraph, so changing them is a paragraph-property revision. Inside a
//! table cell, comparison must record it like it does in the body: the
//! redline carries the change, Accept All shows the target's mark and Reject
//! All restores the source's.

use std::io::Write;

use stemma::api::Document;
use stemma::domain::{BlockNode, Mark, ParagraphFormattingChangeCarrier, ParagraphNode};
use stemma::{ExportOptions, Resolution};

fn docx(cell_paragraph: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="4000"/><w:gridCol w:w="4000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="4000" w:type="dxa"/></w:tcPr>{cell_paragraph}</w:tc><w:tc><w:tcPr><w:tcW w:w="4000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>Unchanged neighbour cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/><w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"></Relationships>"#;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = zip::write::FileOptions::default();
        for (name, body) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/document.xml", document_xml.as_str()),
        ] {
            zip.start_file(name, opts).expect("start part");
            zip.write_all(body.as_bytes()).expect("write part");
        }
        zip.finish().expect("finish package");
    }
    buf
}

fn first_cell_paragraph(document: &Document) -> ParagraphNode {
    let BlockNode::Table(table) = &document.snapshot().canonical.blocks[0].block else {
        panic!("the fixture starts with a table");
    };
    let BlockNode::Paragraph(paragraph) = &table.rows[0].cells[0].blocks[0] else {
        panic!("the first cell starts with a paragraph");
    };
    (**paragraph).clone()
}

fn reopened(document: &Document) -> Document {
    let bytes = document
        .serialize(&ExportOptions::default())
        .expect("serialize");
    Document::parse(&bytes).expect("reopen")
}

#[test]
fn cell_paragraph_mark_change_is_a_revision_that_both_resolutions_honour() {
    let base = Document::parse(&docx(
        r#"<w:p><w:r><w:t>Alpha beta gamma</w:t></w:r></w:p>"#,
    ))
    .expect("parse base");
    let target = Document::parse(&docx(
        r#"<w:p><w:pPr><w:rPr><w:b/></w:rPr></w:pPr><w:r><w:t>Alpha beta delta</w:t></w:r></w:p>"#,
    ))
    .expect("parse target");
    assert!(
        first_cell_paragraph(&target)
            .paragraph_mark_marks
            .contains(&Mark::Bold),
        "fixture: the target's cell paragraph mark is bold"
    );

    let redline = reopened(&stemma_diff::diff_as(&base, &target, "compare").expect("compare"));
    let change = first_cell_paragraph(&redline)
        .formatting_change
        .expect("the paragraph mark change is recorded as a revision");
    assert!(
        matches!(
            change.carrier,
            ParagraphFormattingChangeCarrier::ParagraphMarkProperties
                | ParagraphFormattingChangeCarrier::ParagraphAndMarkProperties
        ),
        "the revision is carried by the paragraph mark: {:?}",
        change.carrier
    );

    let accepted = reopened(&redline.project(Resolution::AcceptAll).expect("accept"));
    assert!(
        first_cell_paragraph(&accepted)
            .paragraph_mark_marks
            .contains(&Mark::Bold),
        "Accept All shows the target's bold paragraph mark"
    );
    let rejected = reopened(&redline.project(Resolution::RejectAll).expect("reject"));
    assert!(
        !first_cell_paragraph(&rejected)
            .paragraph_mark_marks
            .contains(&Mark::Bold),
        "Reject All restores the source's plain paragraph mark"
    );
}
