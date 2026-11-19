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
fn non_duplicate_run_level_rprchange_is_no_revision() {
    // Word lists no revision for a w:rPrChange placed directly in a run (not
    // inside its rPr): it opens the file without repair, and Accept All,
    // Reject All and a plain save all drop the element and keep the run's
    // properties (oracle-checked). So it is read as no revision.
    let body = concat!(
        r#"<w:p><w:r><w:rPr><w:b/></w:rPr>"#,
        r#"<w:rPrChange w:id="9" w:author="Someone Else" w:date="2024-01-01T00:00:00Z"><w:rPr/></w:rPrChange>"#,
        r#"<w:t>Titolo</w:t></w:r></w:p>"#,
    );
    let doc = Document::parse(&build_docx(&wrap_body(body)))
        .expect("Word opens a run-level rPrChange without repair");
    assert!(doc.revisions().is_empty(), "{:?}", doc.revisions());
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
/// counts one delete and restores the content on reject. Here the cell is
/// the only one of the table's only row, its text deleted too: accepting
/// removes the cell's text and its mark, so the row and the table go (Word,
/// observed) and the paragraph after the table stays as it was.
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

    // Accept: the cell's text and mark go, and with them the table.
    let accepted = doc.project(Resolution::AcceptAll).expect("accept");
    let accepted_text = accepted.to_text();
    assert!(
        !accepted_text.contains("Short title") && accepted_text.contains("Tail paragraph."),
        "Word's accept removes the cell content: {accepted_text:?}"
    );
    let xml = document_xml_of(
        &accepted
            .serialize(&ExportOptions::default())
            .expect("serialize"),
    );
    assert!(
        !xml.contains("<w:tbl") && !xml.contains("<w:tc"),
        "Word's accept removes the table: {xml}"
    );
    assert_eq!(
        accepted_text, "Tail paragraph.",
        "only the paragraph after it is left"
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

/// Word writes a run with no content that still carries a tracked formatting
/// change, for example between a field's instruction and its separator after
/// the whole field was reformatted, and opens such a document without repair
/// (CT_R admits zero content children, §17.3.2.25; `w:rPrChange`,
/// §17.13.5.31). The document loads, and its text is the same pending,
/// accepted and rejected, as in Word.
///
/// Word itself discards an empty run when it opens the file, so it shows no
/// change for it. The model keeps the run, as it keeps every empty run, and
/// so holds the change the markup states: it is listed, an untouched save
/// keeps it, accepting keeps the run's properties and rejecting restores the
/// earlier ones, leaving no tracked markup behind either way.
const EMPTY_RUN_FORMATTING_CHANGE: &str = concat!(
    r#"<w:p><w:r><w:t xml:space="preserve">Opening words </w:t></w:r>"#,
    r#"<w:r><w:rPr><w:b/><w:sz w:val="28"/>"#,
    r#"<w:rPrChange w:id="9" w:author="Ingrid" w:date="2024-03-02T09:10:00Z"><w:rPr/></w:rPrChange>"#,
    r#"</w:rPr></w:r>"#,
    r#"<w:r><w:t>closing words</w:t></w:r></w:p>"#,
);

#[test]
fn empty_run_formatting_change_is_listed_kept_and_resolved() {
    let doc = Document::parse(&build_docx(&wrap_body(EMPTY_RUN_FORMATTING_CHANGE)))
        .expect("an empty run with a tracked formatting change loads");
    let records = enumerate_revisions(&doc.snapshot().canonical);
    assert_eq!(records.len(), 1, "one formatting change: {records:?}");
    assert_eq!(records[0].kind, RevisionKind::FormatRun);
    assert_eq!(doc.to_text(), "Opening words closing words");

    let untouched = document_xml_of(&doc.serialize(&ExportOptions::default()).expect("save"))
        .replace(" />", "/>");
    assert!(
        untouched.contains("<w:rPrChange") && untouched.contains(r#"w:author="Ingrid""#),
        "an untouched save keeps the change pending: {untouched}"
    );
    assert_eq!(untouched.matches("<w:b/>").count(), 1, "{untouched}");

    let accepted = document_xml_of(
        &doc.project(Resolution::AcceptAll)
            .expect("accept")
            .serialize(&ExportOptions::default())
            .expect("save accepted"),
    )
    .replace(" />", "/>");
    assert!(!accepted.contains("rPrChange"), "{accepted}");
    assert!(
        accepted.contains("<w:b/>") && accepted.contains(r#"<w:sz w:val="28"/>"#),
        "accept keeps the run's properties: {accepted}"
    );

    let rejected = document_xml_of(
        &doc.project(Resolution::RejectAll)
            .expect("reject")
            .serialize(&ExportOptions::default())
            .expect("save rejected"),
    )
    .replace(" />", "/>");
    assert!(!rejected.contains("rPrChange"), "{rejected}");
    assert!(
        !rejected.contains("<w:b/>") && !rejected.contains("w:sz"),
        "reject restores the earlier, empty properties: {rejected}"
    );
}
