//! Moved-from text is written as `w:t`, not `w:delText`.
//!
//! ECMA-376 §17.13.5.22: `w:moveFrom` content is runs of plain text; deleted
//! text (`w:delText`, §17.3.3.7) belongs only inside `w:del`. Word repairs a
//! file whose moveFrom runs carry `w:delText` (checked in Word: it wraps each
//! such run in a new `w:del` by "Unknown", and the document's revisions are
//! lost). The validator reports it, so a file Word would repair never passes
//! as valid, and the engine never writes it back: import reads the run as
//! moved text with a warning, an untouched save is refused, and Accept/Reject
//! write `w:t`.
//!
//! The same holds for a field instruction: when Word 16 moves a paragraph
//! holding a complex field it writes the moved-from instruction as
//! `w:instrText` (observed), and it reports a file whose moveFrom run spells
//! the instruction as `w:delInstrText` as corrupted (observed).

use std::io::Write;

fn docx(text_element: &str) -> Vec<u8> {
    let t = text_element;
    docx_moving(
        &format!("<w:r><{t}>Travelling line.</{t}></w:r>"),
        "<w:r><w:t>Travelling line.</w:t></w:r>",
    )
}

/// The moved paragraph holds a complex field; `instr_element` spells the
/// moved-from copy's instruction.
fn docx_with_field(instr_element: &str) -> Vec<u8> {
    let field = |instr: &str| {
        format!(
            concat!(
                r#"<w:r><w:t xml:space="preserve">Travelling </w:t></w:r>"#,
                r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r>"#,
                r#"<w:r><{i} xml:space="preserve"> QUOTE "line" </{i}></w:r>"#,
                r#"<w:r><w:fldChar w:fldCharType="separate"/></w:r>"#,
                r#"<w:r><w:t>line</w:t></w:r>"#,
                r#"<w:r><w:fldChar w:fldCharType="end"/></w:r>"#,
            ),
            i = instr
        )
    };
    docx_moving(&field(instr_element), &field("w:instrText"))
}

fn docx_moving(move_from_runs: &str, move_to_runs: &str) -> Vec<u8> {
    let m = r#"w:author="Mover" w:date="2026-01-01T00:00:00Z""#;
    let body = format!(
        concat!(
            r#"<w:p><w:r><w:t>Opening line.</w:t></w:r></w:p>"#,
            r#"<w:p><w:pPr><w:rPr><w:moveTo w:id="1" {m}/></w:rPr></w:pPr><w:moveToRangeStart w:id="2" {m} w:name="move1"/>"#,
            r#"<w:moveTo w:id="3" {m}>{to}</w:moveTo><w:moveToRangeEnd w:id="2"/></w:p>"#,
            r#"<w:p><w:r><w:t>Middle line.</w:t></w:r></w:p>"#,
            r#"<w:p><w:pPr><w:rPr><w:moveFrom w:id="4" {m}/></w:rPr></w:pPr><w:moveFromRangeStart w:id="5" {m} w:name="move1"/>"#,
            r#"<w:moveFrom w:id="6" {m}>{from}</w:moveFrom><w:moveFromRangeEnd w:id="5"/></w:p>"#,
            r#"<w:p><w:r><w:t>Closing line.</w:t></w:r></w:p>"#,
        ),
        m = m,
        from = move_from_runs,
        to = move_to_runs
    );
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let mut bytes = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
        let options: zip::write::FileOptions = zip::write::FileOptions::default();
        for (name, data) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/document.xml", document.as_str()),
        ] {
            zip.start_file(name, options).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    bytes
}

#[test]
fn deleted_text_inside_move_from_fails_validation() {
    let report = stemma::api::validate(&docx("w:delText"));
    assert!(!report.ok, "Word repairs this file: {report:?}");
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.message.contains("moveFrom")
                && issue.message.contains("delText")
                && issue.message.contains("word/document.xml")
                && issue.message.contains("Travelling line.")),
        "{report:?}"
    );
}

#[test]
fn plain_text_inside_move_from_is_valid() {
    let report = stemma::api::validate(&docx("w:t"));
    assert!(report.ok, "{report:?}");
}

/// The engine still opens such a file (its moves read as moves), but it
/// will not write the defect back: an untouched save is refused with the
/// rule, while resolving the moves writes a valid file.
#[test]
fn the_engine_opens_such_a_file_but_does_not_write_the_defect_back() {
    let document = stemma::api::Document::parse(&docx("w:delText")).expect("lenient import");
    assert!(
        document
            .diagnostics()
            .iter()
            .any(|d| d.message.contains("w:moveFrom") && d.message.contains("Travelling line.")),
        "import names the run: {:?}",
        document.diagnostics()
    );
    assert_eq!(
        stemma::enumerate_revisions(&document.snapshot().canonical)
            .iter()
            .map(|r| r.kind.as_str())
            .collect::<Vec<_>>(),
        vec!["move"]
    );
    let refused = document
        .serialize(&stemma::ExportOptions::default())
        .expect_err("an untouched save would hand Word a file it repairs");
    assert!(refused.message.contains("I-TC-004"), "{}", refused.message);
    for resolution in [stemma::Resolution::AcceptAll, stemma::Resolution::RejectAll] {
        let resolved = document.project(resolution).expect("resolve");
        let saved = resolved
            .serialize(&stemma::ExportOptions::default())
            .expect("the resolved file saves");
        assert!(stemma::api::validate(&saved).ok);
    }
}

#[test]
fn deleted_instruction_text_inside_move_from_fails_validation() {
    let report = stemma::api::validate(&docx_with_field("w:delInstrText"));
    assert!(
        !report.ok,
        "Word reports this file as corrupted: {report:?}"
    );
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.message.contains("I-TC-004")
                && issue.message.contains("w:delInstrText")
                && issue.message.contains("QUOTE")),
        "{report:?}"
    );
}

/// Word 16 writes a moved field's instruction as `w:instrText` inside
/// `w:moveFrom` (observed), so that file is valid.
#[test]
fn plain_instruction_text_inside_move_from_is_valid() {
    let report = stemma::api::validate(&docx_with_field("w:instrText"));
    assert!(report.ok, "{report:?}");
}

#[test]
fn a_deleted_instruction_in_move_from_is_named_as_such_and_resolves_to_instr_text() {
    let document =
        stemma::api::Document::parse(&docx_with_field("w:delInstrText")).expect("lenient import");
    assert!(
        document
            .diagnostics()
            .iter()
            .any(|d| d.message.contains("w:moveFrom")
                && d.message.contains("w:delInstrText")
                && d.message.contains("w:instrText")
                && d.message.contains("QUOTE")),
        "import names the run and its element: {:?}",
        document.diagnostics()
    );
    let refused = document
        .serialize(&stemma::ExportOptions::default())
        .expect_err("an untouched save would hand Word a file it reports as corrupted");
    assert!(refused.message.contains("I-TC-004"), "{}", refused.message);
    for (label, resolution) in [
        ("accept all", stemma::Resolution::AcceptAll),
        ("reject all", stemma::Resolution::RejectAll),
    ] {
        let resolved = document.project(resolution).expect("resolve");
        let saved = resolved
            .serialize(&stemma::ExportOptions::default())
            .expect("the resolved file saves");
        assert!(stemma::api::validate(&saved).ok);
        let xml = document_xml(&saved);
        assert!(
            xml.contains("QUOTE") && !xml.contains("delInstrText"),
            "{label}: the kept field's instruction is w:instrText: {xml}"
        );
    }
}

fn document_xml(docx: &[u8]) -> String {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(docx)).expect("zip");
    let mut xml = String::new();
    std::io::Read::read_to_string(
        &mut archive.by_name("word/document.xml").expect("document part"),
        &mut xml,
    )
    .expect("utf-8");
    xml
}
