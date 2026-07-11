//! Blindspot regression: accepting a tracked ROW deletion must REMOVE the row.
//!
//! Domain rule (ECMA-376 §17.13.5 "Annotations / Revisions", §17.13.5.12 `del`
//! within row properties `CT_TrPr`): a table row whose `<w:trPr>` carries a
//! `<w:del>` element represents a tracked deletion of the ENTIRE row. The
//! tracked-changes acceptance model says that accepting a deletion produces the
//! document as if the deleted content were never present — the content is gone,
//! not merely "untracked". For a row-level deletion that means the whole `<w:tr>`
//! must be removed on accept. This is exactly what Microsoft Word's "Accept All
//! Changes" does: a row marked deleted via `trPr/w:del` disappears.
//!
//! The normalize/accept path in `stemma::normalize::normalize_docx` (the
//! "accept all" path) currently STRIPS the `trPr/w:del` marker but KEEPS the
//! row (see normalize.rs:973-982, whose comment claims "Word keeps row structure
//! intact"). Two in-module unit tests (normalize.rs:1849, :1957) assert
//! `tr_count == 2`, locking in that behavior. If this behavior is wrong, those
//! two tests are `test_bug_in_existing_suite`.
//!
//! This test encodes the DOMAIN-CORRECT expectation (accept removes the row) and
//! does NOT touch any production code or any existing test.

use stemma::docx::{DocxArchive, DocxFile};
use stemma::normalize::normalize_docx;

/// Minimal `[Content_Types].xml` + `_rels` + `word/document.xml` archive.
/// `normalize_docx` only needs to locate and parse `word/document.xml`, but we
/// include the package scaffolding so the archive resembles a real DOCX.
fn archive_with_document_xml(document_xml: &str) -> DocxArchive {
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;
    let root_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

    DocxArchive::from_parts(vec![
        DocxFile {
            name: "[Content_Types].xml".to_string(),
            data: content_types.as_bytes().to_vec(),
        },
        DocxFile {
            name: "_rels/.rels".to_string(),
            data: root_rels.as_bytes().to_vec(),
        },
        DocxFile {
            name: "word/document.xml".to_string(),
            data: document_xml.as_bytes().to_vec(),
        },
    ])
}

/// Count `<w:tr>` and `<w:tr ...>` opening tags in serialized XML.
fn count_rows(xml: &str) -> usize {
    xml.matches("<w:tr>").count() + xml.matches("<w:tr ").count()
}

#[test]
fn accept_of_tracked_row_deletion_removes_the_row() {
    // 2-row table. Row 1 is plain (survives). Row 2 carries a row-level tracked
    // deletion via <w:trPr><w:del/></w:trPr>: the WHOLE row was deleted.
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:tbl>
      <w:tblPr><w:tblW w:w="5000" w:type="dxa" /></w:tblPr>
      <w:tblGrid><w:gridCol w:w="5000" /></w:tblGrid>
      <w:tr>
        <w:tc><w:p><w:r><w:t>surviving row</w:t></w:r></w:p></w:tc>
      </w:tr>
      <w:tr>
        <w:trPr>
          <w:del w:id="1" w:author="A" w:date="2026-01-01T00:00:00Z" />
        </w:trPr>
        <w:tc><w:p><w:r><w:t>deleted row text</w:t></w:r></w:p></w:tc>
      </w:tr>
    </w:tbl>
  </w:body>
</w:document>"#;

    let archive = archive_with_document_xml(xml);
    let (result_archive, _stats) = normalize_docx(&archive).expect("normalize should succeed");

    let result_xml = std::str::from_utf8(result_archive.get("word/document.xml").unwrap()).unwrap();

    eprintln!("--- accepted (normalize) document.xml ---\n{result_xml}\n---");

    // The non-deleted row must survive.
    assert!(
        result_xml.contains("surviving row"),
        "row 1 (not deleted) must remain after accept; got:\n{result_xml}"
    );

    // Tracking markers must be gone regardless of outcome.
    assert!(
        !result_xml.contains("<w:del"),
        "row deletion marker must be stripped on accept; got:\n{result_xml}"
    );

    // DOMAIN-CORRECT EXPECTATION (ECMA-376 §17.13.5.12 + tracked-change accept
    // model): accepting a row deletion removes the row. The deleted row's text
    // must be gone, and exactly one row must remain.
    assert!(
        !result_xml.contains("deleted row text"),
        "accepting a row deletion must remove the deleted row's content; got:\n{result_xml}"
    );
    assert_eq!(
        count_rows(result_xml),
        1,
        "accepting a tracked row deletion (trPr/w:del) must yield a 1-row table \
         (the deleted row is removed), per ECMA-376 §17.13.5.12; got:\n{result_xml}"
    );
}
