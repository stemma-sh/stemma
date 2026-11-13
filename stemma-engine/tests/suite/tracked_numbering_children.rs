//! A paragraph whose `w:numPr` carries a tracked child opens, and resolves as
//! Word resolves it (Word 16, verified on the oracle):
//! - `w:numPr/w:ins` (inserted numbering): Word lists one revision by its
//!   author, and Accept All and Reject All BOTH keep the numbering, dropping
//!   only the marker.
//! - `w:numPr/w:numberingChange` (legacy previous numbering text): Word lists
//!   no revision, and Accept and Reject both keep the numbering, dropping it.
//!
//! Each reading is disclosed as an import diagnostic.

use std::io::Write;

use stemma::api::Document;
use stemma::{ExportOptions, Resolution};

use crate::common::read_zip_entry;

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn docx(num_pr_child: &str, extra_ppr: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="{W}"><w:body><w:p><w:r><w:t>Plain opening line.</w:t></w:r></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/>{num_pr_child}</w:numPr>{extra_ppr}</w:pPr><w:r><w:t>Numbered line.</w:t></w:r></w:p><w:p><w:r><w:t>Plain closing line.</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#
    );
    let numbering_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:numbering xmlns:w="{W}"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/><w:lvlJc w:val="left"/><w:pPr><w:ind w:left="720" w:hanging="360"/></w:pPr></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/></Relationships>"#;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions = zip::write::FileOptions::default();
        for (name, data) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/document.xml", document_xml.as_str()),
            ("word/numbering.xml", numbering_xml.as_str()),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

/// The saved second paragraph's `w:pPr`.
fn numbered_ppr(doc: &Document) -> String {
    let bytes = doc.serialize(&ExportOptions::default()).expect("save");
    let xml = read_zip_entry(&bytes, "word/document.xml").expect("document.xml");
    let text = xml.find("Numbered line.").expect("the numbered paragraph");
    let start = xml[..text].rfind("<w:pPr>").expect("its pPr");
    let end = start + xml[start..].find("</w:pPr>").expect("pPr end");
    xml[start..end + "</w:pPr>".len()].to_string()
}

fn is_numbered_without_history(ppr: &str) -> bool {
    ppr.contains(r#"<w:ilvl w:val="0""#)
        && ppr.contains(r#"<w:numId w:val="1""#)
        && !ppr.contains("w:ins")
        && !ppr.contains("numberingChange")
        && !ppr.contains("pPrChange")
}

#[test]
fn inserted_numbering_is_one_revision_that_keeps_the_numbering_either_way() {
    let doc = Document::parse(&docx(
        r#"<w:ins w:id="5" w:author="Ines" w:date="2026-01-01T00:00:00Z"/>"#,
        "",
    ))
    .expect("Word opens it without repair");
    let revisions = doc.revisions();
    assert_eq!(revisions.len(), 1, "{revisions:?}");
    assert_eq!(revisions[0].author.as_deref(), Some("Ines"));
    assert!(
        doc.diagnostics()
            .iter()
            .any(|d| d.message.contains("w:numPr/w:ins")),
        "the reading is disclosed"
    );
    for resolution in [Resolution::AcceptAll, Resolution::RejectAll] {
        let resolved = doc.project(resolution).expect("resolve");
        assert!(resolved.revisions().is_empty());
        let ppr = numbered_ppr(&resolved);
        assert!(is_numbered_without_history(&ppr), "{ppr}");
    }
    let reopened =
        Document::parse(&doc.serialize(&ExportOptions::default()).unwrap()).expect("reopen");
    assert_eq!(reopened.revisions().len(), 1);
}

#[test]
fn a_numbering_change_is_no_revision_and_keeps_the_numbering() {
    let doc = Document::parse(&docx(
        r#"<w:numberingChange w:id="6" w:author="Ines" w:date="2026-01-01T00:00:00Z" w:original="%1."/>"#,
        "",
    ))
    .expect("Word opens it without repair");
    assert!(doc.revisions().is_empty());
    assert!(
        doc.diagnostics()
            .iter()
            .any(|d| d.message.contains("numberingChange")),
        "{}",
        read_zip_entry(
            &doc.serialize(&ExportOptions::default()).unwrap(),
            "word/document.xml"
        )
        .unwrap()
    );
    for resolution in [Resolution::AcceptAll, Resolution::RejectAll] {
        let ppr = numbered_ppr(&doc.project(resolution).expect("resolve"));
        assert!(is_numbered_without_history(&ppr), "{ppr}");
    }
}

/// Two revisions (the inserted numbering and an existing paragraph-property
/// change) have no single-revision spelling: still refused, naming the child.
#[test]
fn inserted_numbering_beside_a_paragraph_property_change_is_refused() {
    let error = match Document::parse(&docx(
        r#"<w:ins w:id="5" w:author="Ines" w:date="2026-01-01T00:00:00Z"/>"#,
        r#"<w:pPrChange w:id="7" w:author="Otto" w:date="2026-01-02T00:00:00Z"><w:pPr/></w:pPrChange>"#,
    )) {
        Ok(_) => panic!("two revisions cannot become one"),
        Err(error) => error,
    };
    assert!(error.message.contains("w:ins"), "{}", error.message);
}
