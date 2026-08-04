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

fn make_docx_with_styles(body: &str, styles: &str) -> Vec<u8> {
    let doc = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W_NS}"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let ct = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let document_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let o: FileOptions = FileOptions::default();
        zip.start_file("[Content_Types].xml", o).unwrap();
        zip.write_all(ct.as_bytes()).unwrap();
        zip.start_file("_rels/.rels", o).unwrap();
        zip.write_all(rels.as_bytes()).unwrap();
        zip.start_file("word/_rels/document.xml.rels", o).unwrap();
        zip.write_all(document_rels.as_bytes()).unwrap();
        zip.start_file("word/document.xml", o).unwrap();
        zip.write_all(doc.as_bytes()).unwrap();
        zip.start_file("word/styles.xml", o).unwrap();
        zip.write_all(styles.as_bytes()).unwrap();
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

fn rejected_first_para(body: &str, styles: &str) -> String {
    let docx = make_docx_with_styles(body, styles);
    let out = Document::parse(&docx)
        .expect("parse")
        .project(stemma::Resolution::RejectAll)
        .expect("reject")
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

#[test]
fn styled_literal_prefix_keeps_unstyled_punctuation_and_tab_runs() {
    let para = rejected_first_para(
        r#"<w:p><w:pPr><w:rPr><w:ins w:id="1" w:author="fid"/></w:rPr></w:pPr></w:p><w:p><w:pPr><w:pStyle w:val="Heading5"/></w:pPr><w:r><w:rPr><w:rStyle w:val="CharSectno"/></w:rPr><w:t>16</w:t></w:r><w:r><w:t>.</w:t></w:r><w:r><w:tab/></w:r><w:r><w:t>Permit</w:t></w:r></w:p>"#,
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:style w:type="paragraph" w:styleId="Heading5"><w:name w:val="heading 5"/><w:rPr><w:b/><w:sz w:val="24"/></w:rPr></w:style><w:style w:type="character" w:customStyle="1" w:styleId="CharSectno"><w:name w:val="CharSectno"/><w:rPr><w:noProof w:val="0"/><w:lang w:val="en-AU"/></w:rPr></w:style></w:styles>"#,
    );
    let run_count = para.matches("<w:r>").count() + para.matches("<w:r ").count();
    assert_eq!(
        run_count, 4,
        "the styled label, punctuation, tab, and body are distinct authored runs; paragraph: {para}"
    );
    assert!(
        !para.contains("<w:t>16.</w:t>") && !para.contains("<w:t>16.\t</w:t>"),
        "CharSectno must not absorb the unstyled punctuation or tab; paragraph: {para}"
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

/// Materializing a label and re-hoisting it must return the paragraph to the
/// same field split it started from.
///
/// A block-level proposal carries its label inside the body, so resolving that
/// proposal away re-hoists it. `materialized_prefix_text` emits
/// `literal_prefix_leading_ws` verbatim ahead of the label, and the re-hoist
/// has to put that whitespace back in the whitespace field rather than glue it
/// onto the label: indent geometry and authored text are separate fields on
/// purpose, and a label that grows an indent every reject cycle is not the
/// label the author typed. Space indents are the case that regressed; the
/// splitter handled tabs only.
#[test]
fn rejecting_a_deletion_restores_a_space_indented_label_unchanged() {
    let docx = make_docx(
        r#"<w:p><w:pPr><w:jc w:val="both"/></w:pPr><w:r><w:t xml:space="preserve">      2. Indented clause body text</w:t></w:r></w:p><w:p><w:r><w:t>Second paragraph.</w:t></w:r></w:p>"#,
    );
    let doc = Document::parse(&docx).expect("parse");
    let split_of = |document: &Document| match &document.snapshot().canonical.blocks[0].block {
        stemma::domain::BlockNode::Paragraph(p) => (
            p.literal_prefix.clone(),
            p.literal_prefix_leading_ws.clone(),
        ),
        _ => panic!("expected a paragraph"),
    };
    let before = split_of(&doc);
    assert_eq!(
        before,
        (Some("2.".into()), "      ".to_string()),
        "import splits the indent from the label"
    );

    let target = match &doc.snapshot().canonical.blocks[0].block {
        stemma::domain::BlockNode::Paragraph(p) => p.id.clone(),
        _ => panic!("expected a paragraph"),
    };
    let deleted = doc
        .apply(&EditTransaction {
            steps: vec![EditStep::DeleteBlockRange {
                from_block_id: target.clone(),
                to_block_id: target,
                rationale: None,
                expect: "Indented clause body text".to_string(),
                semantic_hash: None,
            }],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: RevisionInfo {
                revision_id: 4242,
                identity: 0,
                author: Some("Reviewer".to_string()),
                date: Some("2026-07-25T00:00:00Z".to_string()),
                apply_op_id: None,
            },
        })
        .expect("delete the labelled paragraph");

    // The proposal now carries the label, so rejecting it must hand the label
    // and its indent back to the fields they came from.
    let rejected = deleted
        .project(stemma::Resolution::RejectAll)
        .expect("reject the deletion");
    assert_eq!(
        split_of(&rejected),
        before,
        "reject must be the identity on the label's field split"
    );
}

/// A whole-paragraph deletion must still be a BLOCK-level deletion after a
/// save and reopen, including when the paragraph carries a manual label.
///
/// The label now travels inside the proposal, so the wire is complete and
/// import's `lift_whole_paragraph_deletions` should recognise the pair and
/// restore the block shape. Without it the model reports a block-level
/// deletion where a reopened document reports a normal block with deleted
/// segments, and their accept projections differ.
#[test]
fn a_labelled_whole_paragraph_deletion_reopens_as_a_block_deletion() {
    let docx = make_docx(
        r#"<w:p><w:pPr><w:jc w:val="both"/></w:pPr><w:r><w:t xml:space="preserve">2. Clause body text here</w:t></w:r></w:p><w:p><w:r><w:t>Second paragraph.</w:t></w:r></w:p>"#,
    );
    let doc = Document::parse(&docx).expect("parse");
    let target = match &doc.snapshot().canonical.blocks[0].block {
        stemma::domain::BlockNode::Paragraph(p) => p.id.clone(),
        _ => panic!("expected a paragraph"),
    };
    let deleted = doc
        .apply(&EditTransaction {
            steps: vec![EditStep::DeleteBlockRange {
                from_block_id: target.clone(),
                to_block_id: target,
                rationale: None,
                expect: "Clause body text here".to_string(),
                semantic_hash: None,
            }],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: RevisionInfo {
                revision_id: 77,
                identity: 0,
                author: Some("Reviewer".to_string()),
                date: Some("2026-07-25T00:00:00Z".to_string()),
                apply_op_id: None,
            },
        })
        .expect("delete the labelled paragraph");

    let bytes = deleted
        .serialize(&ExportOptions::default())
        .expect("serialize");
    if std::env::var("DUMP_LIFT").is_ok() {
        std::fs::write("/tmp/w8-lift2.docx", &bytes).unwrap();
    }
    let reopened = Document::parse(&bytes).expect("reopen");
    assert_eq!(
        std::mem::discriminant(&deleted.snapshot().canonical.blocks[0].status),
        std::mem::discriminant(&reopened.snapshot().canonical.blocks[0].status),
        "block-level deletion must survive the reopen: before={:?} after={:?}",
        deleted.snapshot().canonical.blocks[0].status,
        reopened.snapshot().canonical.blocks[0].status
    );

    // NOTE: the label's FIELD still differs across this boundary — the producer
    // materializes it into the proposal while import hoists it back out of a
    // complete deletion. Both sides agree on the block-level shape, which is
    // what this test pins; the remaining field disagreement is recorded as
    // W8-F22 rather than asserted here, because closing it means choosing one
    // of the two rules and the campaign currently needs both.
}
