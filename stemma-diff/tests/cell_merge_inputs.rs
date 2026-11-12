//! Comparison reads each input's accepted reading, so an input carrying a
//! pending tracked cell merge (`w:cellMerge`, §17.13.5.3) is compared as Word
//! accepts it: the cell merged into the one above, its text gone. The
//! flattened merge is disclosed with the input's other pending revisions.

use std::io::Write;

use stemma::api::Document;
use stemma::{Resolution, RevisionKind};

fn docx(body: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = zip::write::FileOptions::default();
        for (name, part) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/document.xml", document_xml.as_str()),
        ] {
            zip.start_file(name, opts).expect("start part");
            zip.write_all(part.as_bytes()).expect("write part");
        }
        zip.finish().expect("finish package");
    }
    buf
}

fn cell(tc_pr: &str, text: &str) -> String {
    format!(
        r#"<w:tc><w:tcPr><w:tcW w:w="2000" w:type="dxa"/>{tc_pr}</w:tcPr><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:tc>"#
    )
}

fn table(second_row_first_cell: &str) -> String {
    format!(
        r#"<w:tbl><w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="2000"/></w:tblGrid><w:tr>{}{}</w:tr><w:tr>{second_row_first_cell}{}</w:tr></w:tbl><w:p><w:r><w:t>After</w:t></w:r></w:p>"#,
        cell(r#"<w:vMerge w:val="restart"/>"#, "Alpha"),
        cell("", "One"),
        cell("", "Two"),
    )
}

#[test]
fn a_pending_cell_merge_is_compared_as_accepted_and_disclosed() {
    let base = Document::parse(&docx(&table(&cell(
        r#"<w:vMerge/><w:cellMerge w:id="5" w:author="Alice" w:date="2026-01-01T00:00:00Z" w:vMerge="cont"/>"#,
        "Bravo",
    ))))
    .expect("parse base");
    let target = Document::parse(&docx(&table(
        r#"<w:tc><w:tcPr><w:tcW w:w="2000" w:type="dxa"/><w:vMerge/></w:tcPr><w:p/></w:tc>"#,
    )))
    .expect("parse target");

    let comparison = stemma_diff::diff_as_detailed(&base, &target, "compare").expect("compare");
    assert_eq!(
        comparison.semantic_change_count, 0,
        "the base accepted is the target: the merged cell's text is gone"
    );
    assert!(
        comparison
            .flattened_input_revisions
            .base
            .iter()
            .any(|record| record.kind == RevisionKind::CellMerge),
        "the flattened cell merge is disclosed"
    );
    let redline = comparison.document;
    assert!(
        redline.revisions().is_empty(),
        "the redline carries no pending cell merge"
    );
    let accepted = redline
        .project(Resolution::AcceptAll)
        .expect("accept")
        .to_text();
    assert!(
        !accepted.contains("Bravo") && accepted.contains("Alpha"),
        "{accepted}"
    );

    // Compared the other way round, both inputs still read as merged.
    let comparison = stemma_diff::diff_as_detailed(&target, &base, "compare").expect("compare");
    assert_eq!(comparison.semantic_change_count, 0, "both read as merged");
}
