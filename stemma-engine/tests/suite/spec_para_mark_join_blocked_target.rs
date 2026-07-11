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
fn donor_with_surviving_content_stays_before_surviving_table() {
    // Mark resolves away but the content survives; with no join target the
    // paragraph stays as its own paragraph (its mark becomes an ordinary
    // terminating mark). Nothing may disappear.
    let body = format!(
        r#"<w:p><w:pPr><w:rPr><w:ins w:id="1" w:author="A" w:date="2024-01-01T00:00:00Z"/></w:rPr></w:pPr><w:r><w:t>Kept</w:t></w:r></w:p>{NORMAL_TABLE}{TAIL_P}"#
    );
    let wire = wire_resolve(&body, false);
    let xml = document_xml_of(&wire);
    assert!(xml.contains("Kept"), "surviving content must stay: {xml}");
    assert_eq!(
        paragraph_count(&xml),
        3, // donor + cell paragraph + tail
        "donor with surviving content must remain its own paragraph: {xml}"
    );
    let model = model_resolve(&body, false);
    assert_eq!(body_text(&wire), body_text(&model));
}

#[test]
fn already_empty_base_paragraph_survives_reject() {
    // The paragraph was ALREADY empty in the base; its mark merely became
    // inserted (appending a new paragraph after it). Rejecting removes what
    // was appended — never the base paragraph itself.
    let body = format!(
        r#"<w:p><w:pPr><w:rPr><w:ins w:id="1" w:author="A" w:date="2024-01-01T00:00:00Z"/></w:rPr></w:pPr></w:p>{NORMAL_TABLE}{TAIL_P}"#
    );
    let wire = wire_resolve(&body, false);
    let xml = document_xml_of(&wire);
    assert_eq!(
        paragraph_count(&xml),
        3, // the empty-but-base paragraph + cell + tail
        "an already-empty base paragraph must survive reject: {xml}"
    );
    let model = model_resolve(&body, false);
    assert_eq!(body_text(&wire), body_text(&model));
}
