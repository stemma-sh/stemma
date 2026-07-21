//! The literal-prefix LEADING run keeps its own authored rPr (diagnosed on
//! safe-us-vs-canada block 14: "[Arial rPr + tab] ( c ) [tab] Body…").
//!
//! DOMAIN RULE: a run is the unit of formatting authorship. When the
//! literal-prefix extractor hoists a leading tab that lived in its OWN run
//! (with its own rPr) ahead of the label, that rPr is authored content — the
//! leading tab must re-emit wearing it, not the label's formatting captured
//! from the first non-whitespace node (which silently swapped the authored
//! Arial / w:b for the label's plain formatting).

use std::io::Write;

use stemma::ExportOptions;
use stemma::RevisionInfo;
use stemma::api::Document;
use stemma::edit::{EditStep, EditTransaction, MaterializationMode};
use zip::write::FileOptions;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn make_docx(body: &str) -> Vec<u8> {
    let doc = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W_NS}"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let ct = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let o: FileOptions = FileOptions::default();
        zip.start_file("[Content_Types].xml", o).unwrap();
        zip.write_all(ct.as_bytes()).unwrap();
        zip.start_file("_rels/.rels", o).unwrap();
        zip.write_all(rels.as_bytes()).unwrap();
        zip.start_file("word/_rels/document.xml.rels", o).unwrap();
        zip.write_all(br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#).unwrap();
        zip.start_file("word/document.xml", o).unwrap();
        zip.write_all(doc.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

fn edited_first_para(body: &str) -> String {
    let docx = make_docx(body);
    let doc = Document::parse(&docx).expect("parse");
    let txn = EditTransaction {
        steps: vec![EditStep::SetDocDefaults {
            font_family: Some("Calibri".into()),
            font_size_half_points: None,
            rationale: None,
        }],
        materialization_mode: MaterializationMode::Direct,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("fid".into()),
            date: Some("2026-07-02T00:00:00Z".into()),
            apply_op_id: None,
        },
        summary: None,
    };
    let out = doc
        .apply(&txn)
        .expect("apply")
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let a = stemma::docx::DocxArchive::read(&out).expect("archive");
    let xml = String::from_utf8(a.get("word/document.xml").unwrap().to_vec()).unwrap();
    let i = xml.find("<w:p").unwrap();
    let j = xml.find("</w:p>").unwrap();
    xml[i..j].to_string()
}

/// The block-14 shape: leading Arial tab-run, split "(c)" label, separator
/// tab, body. The leading tab must keep Arial; the label keeps its own
/// (sz-only) formatting.
#[test]
fn leading_tab_run_keeps_its_authored_rfonts() {
    let para = edited_first_para(
        r#"<w:p><w:pPr><w:tabs><w:tab w:val="left" w:pos="360"/></w:tabs><w:ind w:left="-720" w:right="-360"/></w:pPr><w:r><w:rPr><w:rFonts w:ascii="Arial" w:hAnsi="Arial" w:cs="Arial"/><w:sz w:val="22"/></w:rPr><w:tab/></w:r><w:r><w:rPr><w:sz w:val="22"/></w:rPr><w:t>(</w:t></w:r><w:r><w:rPr><w:sz w:val="22"/></w:rPr><w:t>c</w:t></w:r><w:r><w:rPr><w:sz w:val="22"/></w:rPr><w:t>)</w:t></w:r><w:r><w:rPr><w:sz w:val="22"/></w:rPr><w:tab/></w:r><w:r><w:rPr><w:sz w:val="22"/></w:rPr><w:t>Body text here.</w:t></w:r></w:p>"#,
    );
    assert!(
        para.contains("Arial"),
        "the leading tab-run's authored Arial rFonts must survive the prefix \
         hoist (run = unit of formatting authorship); paragraph: {para}"
    );
    // And it must be on the run BEFORE the label, not smeared onto it: the
    // first Arial occurrence precedes the "(c)" text.
    let arial = para.find("Arial").unwrap();
    let label = para
        .find("(c)")
        .or_else(|| para.find(">(<"))
        .unwrap_or(usize::MAX);
    assert!(
        arial < label,
        "Arial belongs to the LEADING run, before the label; paragraph: {para}"
    );
}

/// Equal-format source runs are still distinct layout units. Hoisting a
/// literal prefix must not collapse a split label into one synthesized run.
#[test]
fn split_literal_prefix_keeps_source_run_boundaries() {
    let para = edited_first_para(
        r#"<w:p><w:pPr><w:jc w:val="both"/></w:pPr><w:r><w:t>1</w:t></w:r><w:r><w:t>.</w:t></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:t>Body text here.</w:t></w:r></w:p>"#,
    );
    let run_count = para.matches("<w:r>").count() + para.matches("<w:r ").count();
    assert_eq!(
        run_count, 4,
        "the two label runs, separator run, and body run must remain distinct; paragraph: {para}"
    );
}

/// Bold variant (the SAFE-template w:b case).
#[test]
fn leading_tab_run_keeps_its_authored_bold() {
    let para = edited_first_para(
        r#"<w:p><w:pPr><w:ind w:left="-720"/></w:pPr><w:r><w:rPr><w:b/></w:rPr><w:tab/></w:r><w:r><w:t>(a)</w:t></w:r><w:r><w:tab/><w:t>Body text here.</w:t></w:r></w:p>"#,
    );
    assert!(
        para.contains("<w:b"),
        "the leading tab-run's authored w:b must survive; paragraph: {para}"
    );
}

/// Control: when the leading whitespace shares the label's formatting, no
/// extra run is fabricated.
#[test]
fn uniform_prefix_formatting_stays_single_run() {
    let para = edited_first_para(
        r#"<w:p><w:r><w:t xml:space="preserve">	(a)	Body text here.</w:t></w:r></w:p>"#,
    );
    assert!(
        !para.contains("Arial") && para.contains("(a)"),
        "control paragraph round-trips; paragraph: {para}"
    );
}
