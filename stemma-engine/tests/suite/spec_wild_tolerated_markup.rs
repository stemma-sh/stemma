//! Wild-input tolerance edges, each Word-verified and NARROW (see
//! `crate::compat`'s charter: schema-invalid shapes real producers emit and
//! Word opens without repair; tolerated with a named, observable rule —
//! never a silent fallback).
//!
//! 1. A run-level `w:rPrChange` that DUPLICATES the same run's in-`rPr`
//!    change (LibreOffice 24.2 emits this stray copy when round-tripping a
//!    formatting revision): the duplicate is dropped with a diagnostic; the
//!    real revision — already carried by the in-rPr element — survives with
//!    full semantics. A run-level `rPrChange` that does NOT duplicate the
//!    host run's change still fails loud: dropping it would destroy a
//!    revision, and no producer is known to emit that shape.
//! 2. A bare `w:rPr` as a direct `w:p` child (Microsoft Outlook emits this;
//!    CT_P has no such member; Word opens the package valid and unrepaired):
//!    preserved VERBATIM as a zero-width foreign-position element — never
//!    dropped, never guessed into paragraph-mark properties.

use std::io::{Cursor, Read, Write};

use stemma::api::Document;
use stemma::tracked_model::{RevisionKind, enumerate_revisions};
use stemma::{ExportOptions, Resolution};
use zip::ZipWriter;
use zip::write::FileOptions;

fn build_docx(document_xml: &str) -> Vec<u8> {
    let cursor = Cursor::new(Vec::new());
    let mut zip = ZipWriter::new(cursor);
    let options = FileOptions::default();
    zip.start_file("[Content_Types].xml", options).unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#).unwrap();
    zip.start_file("_rels/.rels", options).unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#).unwrap();
    zip.start_file("word/_rels/document.xml.rels", options)
        .unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#).unwrap();
    zip.start_file("word/document.xml", options).unwrap();
    zip.write_all(document_xml.as_bytes()).unwrap();
    zip.finish().unwrap().into_inner()
}

fn wrap_body(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    )
}

fn document_xml_of(docx: &[u8]) -> String {
    let mut zip = zip::ZipArchive::new(Cursor::new(docx.to_vec())).expect("zip");
    let mut part = zip.by_name("word/document.xml").expect("main part");
    let mut out = String::new();
    part.read_to_string(&mut out).expect("utf8");
    out
}

/// The LO 24.2 stray-duplicate emission, verbatim shape from the wild
/// witness: the change lives in rPr AND a childless copy trails the rPr as
/// a direct run child. Word opens it valid and unrepaired (oracle-checked).
const STRAY_DUPLICATE: &str = concat!(
    r#"<w:p><w:r><w:rPr><w:b/>"#,
    r#"<w:rPrChange w:id="7" w:author="Daniela" w:date="2023-12-15T16:49:00Z"><w:rPr/></w:rPrChange>"#,
    r#"</w:rPr>"#,
    r#"<w:rPrChange w:id="7" w:author="Daniela" w:date="2023-12-15T16:49:00Z"><w:rPr/></w:rPrChange>"#,
    r#"<w:t>Titolo</w:t></w:r></w:p>"#,
);

#[test]
fn stray_duplicate_run_level_rprchange_is_tolerated_and_revision_survives() {
    let doc = Document::parse(&build_docx(&wrap_body(STRAY_DUPLICATE))).expect(
        "a run-level rPrChange duplicating the run's own change is Word-tolerated wild input",
    );
    let records = enumerate_revisions(&doc.snapshot().canonical);
    assert_eq!(
        records.len(),
        1,
        "ONE formatting revision, not zero, not two: {records:?}"
    );
    assert_eq!(records[0].kind, RevisionKind::FormatRun);

    // The real change resolves with full semantics; the stray never returns.
    let rejected = doc.project(Resolution::RejectAll).expect("reject");
    let xml = document_xml_of(
        &rejected
            .serialize(&ExportOptions::default())
            .expect("serialize"),
    );
    assert!(!xml.contains("rPrChange"), "{xml}");
    assert!(
        !xml.contains("<w:b/>") && !xml.contains("<w:b "),
        "reject strips the bold: {xml}"
    );
}

#[test]
fn non_duplicate_run_level_rprchange_still_fails_loud() {
    let body = concat!(
        r#"<w:p><w:r><w:rPr><w:b/></w:rPr>"#,
        r#"<w:rPrChange w:id="9" w:author="Someone Else" w:date="2024-01-01T00:00:00Z"><w:rPr/></w:rPrChange>"#,
        r#"<w:t>Titolo</w:t></w:r></w:p>"#,
    );
    let err = match Document::parse(&build_docx(&wrap_body(body))) {
        Ok(_) => panic!("a NON-duplicate run-level rPrChange carries a revision we must not drop"),
        Err(e) => e,
    };
    assert!(format!("{err}").contains("rPrChange"), "{err}");
}

#[test]
fn outlook_bare_paragraph_rpr_is_preserved_verbatim() {
    let body = concat!(
        r#"<w:p><w:rPr><w:rFonts w:ascii="Arial"/></w:rPr>"#,
        r#"<w:r><w:t>Bare paragraph-level rPr ahead of me.</w:t></w:r></w:p>"#,
    );
    let doc = Document::parse(&build_docx(&wrap_body(body)))
        .expect("Word opens the Outlook bare-rPr shape valid and unrepaired (oracle-checked)");
    assert_eq!(doc.read().blocks.len(), 1);
    assert!(doc.to_text().contains("Bare paragraph-level rPr"));

    // Round-trip preserves the stray element byte-for-byte — never dropped,
    // never merged into paragraph-mark properties.
    let xml = document_xml_of(&doc.serialize(&ExportOptions::default()).expect("serialize"));
    assert!(
        xml.contains(r#"<w:rPr><w:rFonts w:ascii="Arial"/></w:rPr><w:r>"#)
            || xml
                .replace(" />", "/>")
                .contains(r#"<w:rPr><w:rFonts w:ascii="Arial"/></w:rPr><w:r>"#),
        "the bare rPr survives in place: {xml}"
    );
}

/// A tracked-DELETED final cell paragraph mark (`w:pPr/w:rPr/w:del` on a
/// cell's only paragraph) is a legal PENDING state in the wild: automated
/// Word pipelines author it when tracked-deleting whole cell contents, and
/// desktop Word (oracle-checked) opens such documents valid and unrepaired,
/// counts one delete, CLEARS the cell content on accept (retaining the
/// structural final paragraph, CT_Tc §17.4.66) and restores it on reject.
/// The engine's own producers must still never author the state (W5-F7,
/// pinned by mark_cell_content_deleted's spec tests) — but the validator
/// must not condemn wild input for it, and resolution must match Word.
const DELETED_CELL_FINAL_MARK: &str = concat!(
    r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr>"#,
    r#"<w:tblGrid><w:gridCol w:w="2269"/></w:tblGrid>"#,
    r#"<w:tr><w:tc><w:tcPr><w:tcW w:w="2269" w:type="dxa"/></w:tcPr>"#,
    r#"<w:p><w:pPr><w:rPr><w:del w:id="607" w:author="svcProcess" w:date="2015-12-04T17:10:00Z"/></w:rPr></w:pPr>"#,
    r#"<w:del w:id="608" w:author="svcProcess" w:date="2015-12-04T17:10:00Z">"#,
    r#"<w:r><w:delText>Short title</w:delText></w:r></w:del>"#,
    r#"</w:p></w:tc></w:tr></w:tbl>"#,
    r#"<w:p><w:r><w:t>Tail paragraph.</w:t></w:r></w:p>"#,
);

#[test]
fn deleted_cell_final_mark_is_a_valid_pending_state() {
    let bytes = build_docx(&wrap_body(DELETED_CELL_FINAL_MARK));
    let report = stemma::api::validate(&bytes);
    assert!(
        report.ok,
        "Word opens this wild shape valid and unrepaired; the validator must not \
         condemn the pending state: {report:?}"
    );
    let doc = Document::parse(&bytes).expect("parse");
    let records = enumerate_revisions(&doc.snapshot().canonical);
    assert!(
        !records.is_empty(),
        "the pending deletion is enumerated, not hidden: {records:?}"
    );
}

#[test]
fn deleted_cell_final_mark_resolves_like_word() {
    let bytes = build_docx(&wrap_body(DELETED_CELL_FINAL_MARK));
    let doc = Document::parse(&bytes).expect("parse");

    // Accept: content cleared, structural final paragraph retained.
    let accepted = doc.project(Resolution::AcceptAll).expect("accept");
    let accepted_text = accepted.to_text();
    assert!(
        !accepted_text.contains("Short title") && accepted_text.contains("Tail paragraph."),
        "Word's accept clears the cell content: {accepted_text:?}"
    );
    let xml = document_xml_of(
        &accepted
            .serialize(&ExportOptions::default())
            .expect("serialize"),
    );
    let cell = &xml[xml.find("<w:tc>").expect("cell survives")..xml.find("</w:tc>").unwrap()];
    assert!(
        cell.contains("<w:p"),
        "the cell retains a structural final paragraph (CT_Tc §17.4.66): {cell}"
    );
    assert!(!xml.contains("w:del"), "accept fully resolves: {xml}");

    // Reject: content restored.
    let rejected = doc.project(Resolution::RejectAll).expect("reject");
    let rejected_text = rejected.to_text();
    assert!(
        rejected_text.contains("Short title"),
        "Word's reject restores the cell content: {rejected_text:?}"
    );
}
