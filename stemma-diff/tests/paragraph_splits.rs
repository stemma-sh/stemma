//! A paragraph split in two is one tracked paragraph mark: the mark that
//! ends the first part is inserted, and the second part keeps the
//! paragraph's own mark. Reject All gives the one paragraph back, between
//! its unchanged neighbours, and Accept All the two, in memory and after
//! save and reopen, also resolving revision by revision.

use std::collections::HashSet;
use std::io::Write;

use stemma::api::{Document, ResolveSelectionAction};
use stemma::{ExportOptions, Resolution};

fn docx(paragraphs: &[&str]) -> Vec<u8> {
    let body: String = paragraphs
        .iter()
        .map(|text| format!(r#"<w:p><w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p>"#))
        .collect();
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

fn reopened(document: &Document) -> Document {
    Document::parse(
        &document
            .serialize(&ExportOptions::default())
            .expect("serialize"),
    )
    .expect("reopen")
}

fn reading(document: &Document, resolution: Resolution) -> String {
    let resolved = reopened(&document.project(resolution).expect("resolve"));
    assert!(resolved.revisions().is_empty(), "a resolved reading");
    resolved.to_text()
}

fn assert_laws(base: &[&str], target: &[&str]) {
    let base = Document::parse(&docx(base)).expect("parse base");
    let target = Document::parse(&docx(target)).expect("parse target");
    let redline = stemma_diff::diff_as(&base, &target, "compare").expect("compare");
    let saved = reopened(&redline);
    for (form, doc) in [("in memory", &redline), ("reopened", &saved)] {
        assert_eq!(
            reading(doc, Resolution::RejectAll),
            base.to_text(),
            "{form}: Reject All"
        );
        assert_eq!(
            reading(doc, Resolution::AcceptAll),
            target.to_text(),
            "{form}: Accept All"
        );
        let ids: HashSet<u32> = doc.revisions().iter().map(|r| r.revision_id).collect();
        for (action, want) in [
            (ResolveSelectionAction::Reject, &base),
            (ResolveSelectionAction::Accept, &target),
        ] {
            let each = Resolution::Selective {
                ids: ids.clone(),
                action,
            };
            assert_eq!(
                reading(doc, each),
                want.to_text(),
                "{form}: {action:?} each revision"
            );
        }
    }
}

const OPENING: &str = "Opening remarks about the allotment rules and the yearly fee.";
const CLOSING: &str = "Closing remarks about the water butts and the shared shed.";
const FIRST: &str = "The committee inspects every plot on the first Saturday of the month.";
const SECOND: &str = "Each plot holder keeps the paths beside the plot clear of weeds.";

#[test]
fn a_paragraph_split_in_two_comes_back_whole_on_reject() {
    let whole = format!("{FIRST} {SECOND}");
    assert_laws(
        &[OPENING, &whole, CLOSING],
        &[OPENING, FIRST, SECOND, CLOSING],
    );
}

#[test]
fn a_paragraph_split_at_the_end_of_the_story_comes_back_whole_on_reject() {
    let whole = format!("{FIRST} {SECOND}");
    assert_laws(&[OPENING, &whole], &[OPENING, FIRST, SECOND]);
}

#[test]
fn a_paragraph_split_after_a_short_lead_comes_back_whole_on_reject() {
    let lead = "Plots are let for one year.";
    let rest = "The committee inspects every plot on the first Saturday of the month and writes to any plot holder whose paths are not kept clear.";
    let whole = format!("{lead} {rest}");
    assert_laws(&[OPENING, &whole, CLOSING], &[OPENING, lead, rest, CLOSING]);
}

/// The second part opens with a typed label: the label is part of the
/// inserted text, so Reject All leaves no stray label behind.
#[test]
fn a_paragraph_split_before_a_typed_label_comes_back_whole_on_reject() {
    let first = "(a) the plot let to the holder for the season;";
    let second = "(b) the shared shed and the tools kept in it;";
    let whole = format!("{first} {second}");
    assert_laws(
        &[OPENING, &whole, CLOSING],
        &[OPENING, first, second, CLOSING],
    );
}
