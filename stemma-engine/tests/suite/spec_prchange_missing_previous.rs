//! Spec tests: `*PrChange` whose previous-properties child is ABSENT.
//!
//! Word always writes the previous-state child, even when empty
//! (`<w:rPrChange…><w:rPr/></w:rPrChange>`). LibreOffice omits the child
//! entirely when the prior state carried no direct formatting — the same
//! meaning in a different spelling. Domain rule: an absent previous-state
//! child is an EMPTY previous state; it is never a reason to drop the
//! tracked change. Dropping it silently would make the revision invisible
//! to enumeration and resolution and destroy it on rebuild (the
//! table-side parsers already keep the revision in this case; these tests
//! pin the run/paragraph/section parsers to the same behavior).

use std::io::{Cursor, Read, Write};

use stemma::api::Document;
use stemma::tracked_model::{RevisionKind, enumerate_revisions};
use stemma::{ExportOptions, Resolution};
use zip::ZipWriter;
use zip::write::FileOptions;

const CONTENT_TYPES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

const RELS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

const WORD_RELS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
</Relationships>"#;

fn build_docx(document_xml: &str) -> Vec<u8> {
    let cursor = Cursor::new(Vec::new());
    let mut zip = ZipWriter::new(cursor);
    let options = FileOptions::default();

    zip.start_file("[Content_Types].xml", options).unwrap();
    zip.write_all(CONTENT_TYPES_XML.as_bytes()).unwrap();
    zip.start_file("_rels/.rels", options).unwrap();
    zip.write_all(RELS_XML.as_bytes()).unwrap();
    zip.start_file("word/_rels/document.xml.rels", options)
        .unwrap();
    zip.write_all(WORD_RELS_XML.as_bytes()).unwrap();
    zip.start_file("word/document.xml", options).unwrap();
    zip.write_all(document_xml.as_bytes()).unwrap();

    zip.finish().unwrap().into_inner()
}

fn wrap_body(body_content: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"
            xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml"
            mc:Ignorable="w14">
  <w:body>
{body_content}
  </w:body>
</w:document>"#
    )
}

fn document_xml_of(docx: &[u8]) -> String {
    let mut zip = zip::ZipArchive::new(Cursor::new(docx.to_vec())).expect("zip");
    let mut part = zip.by_name("word/document.xml").expect("main part");
    let mut out = String::new();
    part.read_to_string(&mut out).expect("utf8");
    out
}

fn resolved_xml(doc: &Document, resolution: Resolution) -> String {
    let resolved = doc.project(resolution).expect("resolve");
    let bytes = resolved
        .serialize(&ExportOptions::default())
        .expect("serialize resolved");
    document_xml_of(&bytes)
}

/// The LibreOffice emission: bold applied, rPrChange with NO inner w:rPr.
fn rpr_change_missing_previous_docx() -> Vec<u8> {
    build_docx(&wrap_body(
        r#"    <w:p>
      <w:r>
        <w:rPr><w:b/><w:rPrChange w:id="7" w:author="LibreOffice User" w:date="2026-07-16T08:00:00Z"></w:rPrChange></w:rPr>
        <w:t>Bolded phrase</w:t>
      </w:r>
      <w:r><w:t xml:space="preserve"> tail.</w:t></w:r>
    </w:p>
    <w:sectPr/>"#,
    ))
}

#[test]
fn rprchange_without_previous_child_is_kept_and_enumerated() {
    let doc = Document::parse(&rpr_change_missing_previous_docx()).expect("parse");
    let records = enumerate_revisions(&doc.snapshot().canonical);
    assert_eq!(
        records.len(),
        1,
        "a child-less w:rPrChange is one pending revision, not zero: {records:?}"
    );
    assert_eq!(records[0].kind, RevisionKind::FormatRun);
    assert_ne!(
        records[0].revision_id, 0,
        "the kept revision must carry a minted, selectable identity"
    );

    // Rebuild preserves the tracked change (a dropped one would vanish here).
    let bytes = doc
        .serialize(&ExportOptions::default())
        .expect("serialize roundtrip");
    let xml = document_xml_of(&bytes);
    assert_eq!(
        xml.matches("<w:rPrChange").count(),
        1,
        "roundtrip must re-emit the formatting change: {xml}"
    );
}

#[test]
fn rprchange_without_previous_child_resolves_as_empty_previous() {
    let doc = Document::parse(&rpr_change_missing_previous_docx()).expect("parse");

    let accepted = resolved_xml(&doc, Resolution::AcceptAll);
    assert!(
        !accepted.contains("<w:rPrChange"),
        "accept drops the change history: {accepted}"
    );
    assert!(
        accepted.contains("<w:b/") || accepted.contains("<w:b "),
        "accept keeps the new formatting (`<w:b`, not `<w:body`): {accepted}"
    );

    let rejected = resolved_xml(&doc, Resolution::RejectAll);
    assert!(
        !rejected.contains("<w:rPrChange"),
        "reject drops the marker: {rejected}"
    );
    assert!(
        !rejected.contains("<w:b/") && !rejected.contains("<w:b "),
        "reject restores the (empty) previous run properties: {rejected}"
    );
}

#[test]
fn pprchange_without_previous_child_is_kept_and_resolves() {
    let docx = build_docx(&wrap_body(
        r#"    <w:p>
      <w:pPr>
        <w:jc w:val="center"/>
        <w:pPrChange w:id="8" w:author="LibreOffice User" w:date="2026-07-16T08:00:00Z"></w:pPrChange>
      </w:pPr>
      <w:r><w:t>Centered paragraph.</w:t></w:r>
    </w:p>
    <w:sectPr/>"#,
    ));
    let doc = Document::parse(&docx).expect("parse");
    let records = enumerate_revisions(&doc.snapshot().canonical);
    assert_eq!(
        records.len(),
        1,
        "a child-less w:pPrChange is one pending revision: {records:?}"
    );
    assert_eq!(records[0].kind, RevisionKind::FormatParagraph);

    let accepted = resolved_xml(&doc, Resolution::AcceptAll);
    assert!(!accepted.contains("<w:pPrChange"), "{accepted}");
    assert!(
        accepted.contains(r#"<w:jc w:val="center""#),
        "accept keeps the new alignment: {accepted}"
    );

    let rejected = resolved_xml(&doc, Resolution::RejectAll);
    assert!(!rejected.contains("<w:pPrChange"), "{rejected}");
    assert!(
        !rejected.contains(r#"<w:jc w:val="center""#),
        "reject restores the (empty) previous paragraph properties: {rejected}"
    );
}

#[test]
fn sectprchange_without_previous_child_or_id_is_kept() {
    // Both wrinkles at once: no inner w:sectPr AND no w:id — the parser must
    // keep the revision and hand id-minting to the wire-zero pass rather
    // than silently dropping the record.
    let docx = build_docx(&wrap_body(
        r#"    <w:p><w:r><w:t>Body text.</w:t></w:r></w:p>
    <w:sectPr>
      <w:pgSz w:w="12240" w:h="15840"/>
      <w:sectPrChange w:author="LibreOffice User" w:date="2026-07-16T08:00:00Z"></w:sectPrChange>
    </w:sectPr>"#,
    ));
    let doc = Document::parse(&docx).expect("parse");
    let records = enumerate_revisions(&doc.snapshot().canonical);
    assert_eq!(
        records.len(),
        1,
        "a child-less, id-less w:sectPrChange is one pending revision: {records:?}"
    );
    assert_eq!(records[0].kind, RevisionKind::FormatSection);
    assert_ne!(
        records[0].revision_id, 0,
        "the missing wire id must be minted, not conflated with the census-only sentinel"
    );
}
