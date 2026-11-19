//! Paragraph-mark resolution when the join target is blocked or vanishing —
//! wild-witnessed archive/model divergence on the reject path.
//!
//! §17.13.5.20 (mark insertion) / §17.13.5.15 (mark deletion): resolving a
//! paragraph mark AWAY (rejecting an inserted mark, accepting a deleted one)
//! removes the paragraph break, joining content into the FOLLOWING paragraph.
//! Two shapes exercise the byte path where no following paragraph is
//! reachable:
//!
//! 1. A SURVIVING table follows. There is nothing to join into — but a donor
//!    the same resolution EMPTIES (fully-inserted paragraph rejected /
//!    fully-deleted paragraph accepted) has no content to carry and its mark
//!    is resolved away, so Word removes the paragraph entirely. Leaving an
//!    empty husk splits the flow Word shows. Wild shape: an inserted heading
//!    paragraph directly before a retained table, rejected.
//! 2. The table BETWEEN donor and the next paragraph is emptied of every row
//!    by the same resolution (all rows `w:trPr/w:ins` on reject). The table
//!    vanishes on this pass, so the join proceeds ACROSS it — one logical
//!    paragraph rejoins.
//!
//! Every test asserts wire/model parity (`normalize_docx`/`reject_all_docx`
//! vs `Document::project` + export): the model path already implements these
//! rules (`paragraph_emptied_by_accept_reject`, `table_emptied_by_accept_reject`
//! in tracked_model.rs); this file pins the byte path to them.
//!
//! Daily tier: synthesized in-memory DOCX, no corpus, no real-Word oracle.

use std::io::Write;

use stemma::api::{Document, validate};
use stemma::docx::DocxArchive;
use stemma::{ExportOptions, Resolution};

fn docx_with_body(body_inner: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
    let mut buf = Vec::new();
    {
        use zip::write::FileOptions;
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        zip.start_file("[Content_Types].xml", opts).unwrap();
        zip.write_all(content_types.as_bytes()).unwrap();
        zip.start_file("_rels/.rels", opts).unwrap();
        zip.write_all(rels.as_bytes()).unwrap();
        zip.start_file("word/_rels/document.xml.rels", opts)
            .unwrap();
        zip.write_all(doc_rels.as_bytes()).unwrap();
        zip.start_file("word/document.xml", opts).unwrap();
        zip.write_all(document_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

fn wire_resolve(body_inner: &str, accept: bool) -> Vec<u8> {
    let bytes = docx_with_body(body_inner);
    let archive = DocxArchive::read(&bytes).expect("read archive");
    let (out, _) = if accept {
        stemma::normalize::normalize_docx(&archive).expect("normalize")
    } else {
        stemma::normalize::reject_all_docx(&archive).expect("reject")
    };
    out.write().expect("write")
}

fn model_resolve(body_inner: &str, accept: bool) -> Vec<u8> {
    let doc = Document::parse(&docx_with_body(body_inner)).expect("parse");
    let resolution = if accept {
        Resolution::AcceptAll
    } else {
        Resolution::RejectAll
    };
    doc.project(resolution)
        .expect("project")
        .serialize(&ExportOptions::default())
        .expect("serialize")
}

fn body_text(bytes: &[u8]) -> String {
    Document::parse(bytes).expect("reparse").to_text()
}

fn document_xml_of(bytes: &[u8]) -> String {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut f = zip.by_name("word/document.xml").unwrap();
    let mut s = String::new();
    use std::io::Read;
    f.read_to_string(&mut s).unwrap();
    s
}

fn paragraph_count(xml: &str) -> usize {
    xml.matches("<w:p>").count() + xml.matches("<w:p ").count()
}

const NORMAL_TABLE: &str = r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol/></w:tblGrid><w:tr><w:tc><w:tcPr/><w:p><w:r><w:t>Cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#;

const TAIL_P: &str = r#"<w:p><w:r><w:t>tail.</w:t></w:r></w:p>"#;

// ── 1. Emptied donor, surviving table: the wild shape ───────────────────────

#[test]
fn reject_drops_fully_inserted_paragraph_before_surviving_table() {
    // Insert-marked pilcrow + all-inserted content, then a NORMAL table:
    // reject un-proposes the whole paragraph; no husk may remain.
    let body = format!(
        r#"<w:p><w:pPr><w:rPr><w:ins w:id="1" w:author="A" w:date="2024-01-01T00:00:00Z"/></w:rPr></w:pPr><w:ins w:id="2" w:author="A" w:date="2024-01-01T00:00:00Z"><w:r><w:t>Added heading</w:t></w:r></w:ins></w:p>{NORMAL_TABLE}{TAIL_P}"#
    );
    let wire = wire_resolve(&body, false);
    let xml = document_xml_of(&wire);
    assert!(
        !xml.contains("Added heading"),
        "rejected insertion must not survive: {xml}"
    );
    assert_eq!(
        paragraph_count(&xml),
        2, // the cell paragraph + the tail paragraph — no empty husk
        "reject must remove the emptied donor paragraph, not leave a husk: {xml}"
    );
    let model = model_resolve(&body, false);
    assert_eq!(
        body_text(&wire),
        body_text(&model),
        "wire and model reject must agree on this shape"
    );
    assert!(validate(&wire).ok, "wire output must validate");
}

#[test]
fn accept_drops_fully_deleted_paragraph_before_surviving_table() {
    // The accept twin: delete-marked pilcrow + all-deleted content.
    let body = format!(
        r#"<w:p><w:pPr><w:rPr><w:del w:id="1" w:author="A" w:date="2024-01-01T00:00:00Z"/></w:rPr></w:pPr><w:del w:id="2" w:author="A" w:date="2024-01-01T00:00:00Z"><w:r><w:delText>Old heading</w:delText></w:r></w:del></w:p>{NORMAL_TABLE}{TAIL_P}"#
    );
    let wire = wire_resolve(&body, true);
    let xml = document_xml_of(&wire);
    assert!(
        !xml.contains("Old heading"),
        "accepted deletion must not survive: {xml}"
    );
    assert_eq!(
        paragraph_count(&xml),
        2,
        "accept must remove the emptied donor paragraph, not leave a husk: {xml}"
    );
    let model = model_resolve(&body, true);
    assert_eq!(
        body_text(&wire),
        body_text(&model),
        "wire and model accept must agree on this shape"
    );
    assert!(validate(&wire).ok, "wire output must validate");
}

// ── 2. Join across a table the same resolution empties ──────────────────────

#[test]
fn reject_joins_across_table_emptied_by_same_reject() {
    // Donor has SURVIVING content and an insert-marked pilcrow; the table
    // between donor and the next paragraph is all inserted rows. Reject
    // removes the break AND the table: one logical paragraph rejoins.
    let body = r#"<w:p><w:pPr><w:rPr><w:ins w:id="1" w:author="A" w:date="2024-01-01T00:00:00Z"/></w:rPr></w:pPr><w:r><w:t xml:space="preserve">Alpha </w:t></w:r></w:p><w:tbl><w:tblPr/><w:tblGrid><w:gridCol/></w:tblGrid><w:tr><w:trPr><w:ins w:id="2" w:author="A" w:date="2024-01-01T00:00:00Z"/></w:trPr><w:tc><w:tcPr/><w:p><w:r><w:t>inserted cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:r><w:t>beta.</w:t></w:r></w:p>"#.to_string();
    let wire = wire_resolve(&body, false);
    let xml = document_xml_of(&wire);
    assert!(
        !xml.contains("<w:tbl"),
        "all-inserted table must vanish on reject: {xml}"
    );
    let text = body_text(&wire);
    assert!(
        text.contains("Alpha beta."),
        "the paragraph must rejoin across the vanished table: {text:?}"
    );
    let model = model_resolve(&body, false);
    assert_eq!(
        text,
        body_text(&model),
        "wire and model reject must agree on the join-across shape"
    );
    assert!(validate(&wire).ok, "wire output must validate");
}

// ── 3. Negative controls ─────────────────────────────────────────────────────

#[test]
fn donor_with_surviving_content_joins_the_first_cell_of_a_surviving_table() {
    // Mark resolves away and the content survives; the next surviving block
    // is a table followed by a paragraph. Word 16 (observed): the content
    // joins the first paragraph of the table's first cell, which keeps its
    // properties, and the table stays. Nothing may disappear.
    let body = format!(
        r#"<w:p><w:pPr><w:rPr><w:ins w:id="1" w:author="A" w:date="2024-01-01T00:00:00Z"/></w:rPr></w:pPr><w:r><w:t>Kept</w:t></w:r></w:p>{NORMAL_TABLE}{TAIL_P}"#
    );
    let wire = wire_resolve(&body, false);
    let xml = document_xml_of(&wire);
    assert!(xml.contains("Kept"), "surviving content must stay: {xml}");
    assert_eq!(
        paragraph_count(&xml),
        2, // cell paragraph holding the donor's text + tail
        "donor with surviving content joins the first cell: {xml}"
    );
    let cell = &xml[xml.find("<w:tc>").expect("the table stays")..];
    let first_cell_paragraph = &cell[..cell.find("</w:p>").expect("a cell paragraph")];
    assert!(
        first_cell_paragraph.contains("Kept") && first_cell_paragraph.contains("Cell"),
        "the donor's text leads the first cell paragraph: {xml}"
    );
    let model = model_resolve(&body, false);
    assert_eq!(body_text(&wire), body_text(&model));
    assert!(validate(&wire).ok, "wire output must validate");
}

#[test]
fn empty_paragraph_with_inserted_mark_goes_on_reject() {
    // An empty paragraph is nothing but its mark. Word, on Reject All,
    // removes an empty paragraph whose mark is inserted: before a
    // paragraph, before a table, and as the first block of the body
    // (observed in Word; the block that follows keeps its own properties).
    let body = format!(
        r#"<w:p><w:pPr><w:rPr><w:ins w:id="1" w:author="A" w:date="2024-01-01T00:00:00Z"/></w:rPr></w:pPr></w:p>{NORMAL_TABLE}{TAIL_P}"#
    );
    let wire = wire_resolve(&body, false);
    let xml = document_xml_of(&wire);
    assert_eq!(
        paragraph_count(&xml),
        2, // cell + tail
        "the empty inserted paragraph is removed: {xml}"
    );
    let model = model_resolve(&body, false);
    assert_eq!(paragraph_count(&document_xml_of(&model)), 2);
    assert_eq!(body_text(&wire), body_text(&model));
}

// ── 3. A paragraph that is nothing but its mark ─────────────────────────────
//
// Word's results for these shapes were observed directly (Accept All and
// Reject All on each).

const HEAD_P: &str = r#"<w:p><w:r><w:t>head.</w:t></w:r></w:p>"#;
const INSERTED_MARK: &str =
    r#"<w:pPr><w:rPr><w:ins w:id="1" w:author="A" w:date="2024-01-01T00:00:00Z"/></w:rPr></w:pPr>"#;
const DELETED_MARK: &str =
    r#"<w:pPr><w:rPr><w:del w:id="1" w:author="A" w:date="2024-01-01T00:00:00Z"/></w:rPr></w:pPr>"#;

fn all_paragraphs(xml: &str) -> usize {
    // `paragraph_count` sees `<w:p>` and `<w:p ...>`, the latter including
    // the self-closing `<w:p />`.
    paragraph_count(xml) + xml.matches("<w:p/>").count()
}

/// Both paths leave `expected` paragraphs and the same text.
fn assert_paragraphs_after(body: &str, accept: bool, expected: usize) {
    let wire = wire_resolve(body, accept);
    let model = model_resolve(body, accept);
    assert_eq!(
        all_paragraphs(&document_xml_of(&wire)),
        expected,
        "byte path: {}",
        document_xml_of(&wire)
    );
    assert_eq!(
        all_paragraphs(&document_xml_of(&model)),
        expected,
        "model path: {}",
        document_xml_of(&model)
    );
    assert_eq!(body_text(&wire), body_text(&model));
}

#[test]
fn empty_paragraph_with_deleted_mark_goes_on_accept() {
    let before_table = format!("{HEAD_P}<w:p>{DELETED_MARK}</w:p>{NORMAL_TABLE}{TAIL_P}");
    assert_paragraphs_after(&before_table, true, 3); // head + cell + tail
    assert_paragraphs_after(&before_table, false, 4);
    let before_paragraph = format!("{HEAD_P}<w:p>{DELETED_MARK}</w:p>{TAIL_P}");
    assert_paragraphs_after(&before_paragraph, true, 2);
    assert_paragraphs_after(&before_paragraph, false, 3);
}

#[test]
fn empty_paragraph_with_inserted_mark_stays_on_accept_and_goes_on_reject() {
    let before_paragraph = format!("{HEAD_P}<w:p>{INSERTED_MARK}</w:p>{TAIL_P}");
    assert_paragraphs_after(&before_paragraph, true, 3);
    assert_paragraphs_after(&before_paragraph, false, 2);
    let at_the_end = format!("{HEAD_P}<w:p>{INSERTED_MARK}</w:p>");
    assert_paragraphs_after(&at_the_end, true, 2);
    assert_paragraphs_after(&at_the_end, false, 1);
}

/// A paragraph appended after an empty last paragraph has its break written
/// on that empty paragraph. Rejecting the append leaves the story ending
/// with one empty paragraph, as it did before.
#[test]
fn empty_paragraph_before_a_rejected_append_is_what_the_body_ends_with() {
    let body = format!(
        r#"{HEAD_P}<w:p>{INSERTED_MARK}</w:p><w:p><w:ins w:id="2" w:author="A" w:date="2024-01-01T00:00:00Z"><w:r><w:t>appended.</w:t></w:r></w:ins></w:p>"#
    );
    assert_paragraphs_after(&body, false, 2);
    assert_paragraphs_after(&body, true, 3);
}

/// What the revision is does not depend on whether the paragraph carries a
/// `w14:paraId`: some programs add one to every paragraph when they save.
#[test]
fn an_empty_inserted_paragraph_reads_the_same_with_and_without_a_paragraph_id() {
    let with_attributes =
        |attributes: &str| format!("{HEAD_P}<w:p{attributes}>{INSERTED_MARK}</w:p>{TAIL_P}");
    let plain = Document::parse(&docx_with_body(&with_attributes(""))).expect("parse");
    let identified = Document::parse(&docx_with_body(&with_attributes(
        r#" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" w14:paraId="0010A0B1""#,
    )))
    .expect("parse");
    let statement = |doc: &Document| {
        doc.revisions()
            .into_iter()
            .map(|record| (record.kind, record.excerpt, record.block_id))
            .collect::<Vec<_>>()
    };
    assert_eq!(statement(&plain), statement(&identified));
    assert_eq!(statement(&plain).len(), 1);
}
