//! Accepting the deletion of a section break's paragraph mark.
//!
//! Observed in Word 16: the first section (a first-page header, a default
//! footer, a distinct first page) ends in a paragraph whose mark is a
//! tracked deletion; the next section has no header or footer references of
//! its own and so showed the first section's. Accept All merges the two
//! sections. The merged section keeps the next section's own properties (no
//! distinct first page) and gets the header and footer references it had
//! inherited, written as its own. Reject All keeps both sections as they
//! were.
//!
//! A later section is not the same (Word 16, the deleted break of a section
//! with a default header and footer, between a section with headers only and
//! a section with headers of its own but no footer): the merged section keeps
//! its own references only. Which difference decides (the removed section
//! being the first, or the next section having no references of its own) is
//! not observed; the engine writes the inherited references only when both
//! hold.
//!
//! Rejecting an inserted section-break mark is not the same (Word 16, a
//! first section with a default header and footer, the next section with
//! references of other types only): Reject All leaves one section holding
//! the next section's own references and nothing it had inherited.

use std::io::{Cursor, Write};

use stemma::api::Document;
use stemma::docx::DocxArchive;
use stemma::{ExportOptions, Resolution};
use zip::ZipWriter;
use zip::write::FileOptions;

const WHO: &str = r#"w:author="Ingrid" w:date="2024-03-02T09:10:00Z""#;
const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

/// The document, its section-break mark tracked as `mark` (`del` or `ins`).
fn docx(mark: &str) -> Vec<u8> {
    let body = format!(
        concat!(
            r#"<w:p><w:r><w:t>Cover page.</w:t></w:r></w:p>"#,
            r#"<w:p><w:pPr><w:rPr><w:{mark} w:id="1" {who}/></w:rPr><w:sectPr>"#,
            r#"<w:headerReference w:type="first" r:id="rIdHead"/>"#,
            r#"<w:footerReference w:type="default" r:id="rIdFoot"/>"#,
            r#"<w:titlePg/></w:sectPr></w:pPr><w:r><w:t>End of cover.</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t>Main text.</w:t></w:r></w:p>"#,
            r#"<w:sectPr><w:type w:val="continuous"/></w:sectPr>"#,
        ),
        who = WHO,
        mark = mark
    );
    package(&body)
}

/// A package holding `body`, one header part (`rIdHead`) and one footer
/// part (`rIdFoot`).
fn package(body: &str) -> Vec<u8> {
    let story = |root: &str, text: &str| {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:{root} xmlns:w="{W}"><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:{root}>"#
        )
    };
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let options = FileOptions::default();
    let mut file = |name: &str, content: &str| {
        zip.start_file(name, options).unwrap();
        zip.write_all(content.as_bytes()).unwrap();
    };
    file(
        "[Content_Types].xml",
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/><Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/></Types>"#,
    );
    file(
        "_rels/.rels",
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#,
    );
    file(
        "word/_rels/document.xml.rels",
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdHead" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/><Relationship Id="rIdFoot" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/></Relationships>"#,
    );
    file("word/header1.xml", &story("hdr", "Crest"));
    file("word/footer1.xml", &story("ftr", "Harbour office"));
    file(
        "word/document.xml",
        &format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="{W}" xmlns:r="{R}"><w:body>{body}</w:body></w:document>"#
        ),
    );
    zip.finish().unwrap().into_inner()
}

/// A header or footer reference: (element, type, target part).
type Reference = (String, String, String);

/// Per section, in order: its header and footer references, and whether it
/// has a distinct first page.
fn sections(bytes: &[u8]) -> Vec<(Vec<Reference>, bool)> {
    let archive = DocxArchive::read(bytes).expect("the package reads");
    let rels = String::from_utf8_lossy(
        archive
            .get("word/_rels/document.xml.rels")
            .expect("document relationships"),
    )
    .to_string();
    let target = |id: &str| {
        let at = rels
            .find(&format!(r#"Id="{id}""#))
            .unwrap_or_else(|| panic!("relationship {id} exists"));
        let tail = &rels[at..];
        let start = tail.find(r#"Target=""#).unwrap() + 8;
        tail[start..start + tail[start..].find('"').unwrap()].to_string()
    };
    let root = xmltree::Element::parse(archive.get("word/document.xml").expect("document part"))
        .expect("document.xml parses");
    let mut out = Vec::new();
    fn find_sections<'a>(element: &'a xmltree::Element, out: &mut Vec<&'a xmltree::Element>) {
        for child in element.children.iter().filter_map(|c| c.as_element()) {
            if child.name.ends_with("sectPr") {
                out.push(child);
            } else {
                find_sections(child, out);
            }
        }
    }
    let mut found = Vec::new();
    find_sections(&root, &mut found);
    for section in found {
        let mut refs = Vec::new();
        let mut title_page = false;
        for child in section.children.iter().filter_map(|c| c.as_element()) {
            let attr = |local: &str| {
                child
                    .attributes
                    .iter()
                    .find(|(name, _)| name.local_name == local)
                    .map(|(_, value)| value.clone())
                    .unwrap()
            };
            match child.name.as_str() {
                "headerReference" | "footerReference" => {
                    refs.push((child.name.clone(), attr("type"), target(&attr("id"))))
                }
                "titlePg" => title_page = true,
                _ => {}
            }
        }
        refs.sort();
        out.push((refs, title_page));
    }
    out
}

fn resolved(mark: &str, resolution: Resolution) -> Vec<u8> {
    Document::parse(&docx(mark))
        .expect("the document loads")
        .project(resolution)
        .expect("the document resolves")
        .serialize(&ExportOptions::default())
        .expect("the resolved document saves")
}

fn inherited() -> Vec<Reference> {
    vec![
        (
            "footerReference".to_string(),
            "default".to_string(),
            "footer1.xml".to_string(),
        ),
        (
            "headerReference".to_string(),
            "first".to_string(),
            "header1.xml".to_string(),
        ),
    ]
}

#[test]
fn accept_all_writes_the_inherited_header_and_footer_into_the_merged_section() {
    assert_eq!(
        sections(&resolved("del", Resolution::AcceptAll)),
        [(inherited(), false)]
    );
}

#[test]
fn reject_all_keeps_both_sections_as_they_were() {
    assert_eq!(
        sections(&resolved("del", Resolution::RejectAll)),
        [(inherited(), true), (Vec::new(), false)]
    );
}

#[test]
fn reject_all_of_an_inserted_break_leaves_the_next_section_its_own_references() {
    assert_eq!(
        sections(&resolved("ins", Resolution::RejectAll)),
        [(Vec::new(), false)]
    );
    assert_eq!(
        sections(&resolved("ins", Resolution::AcceptAll)),
        [(inherited(), true), (Vec::new(), false)]
    );
}

#[test]
fn accept_all_of_a_later_break_leaves_the_next_section_its_own_references() {
    let body = format!(
        concat!(
            r#"<w:p><w:pPr><w:sectPr><w:headerReference w:type="default" r:id="rIdHead"/>"#,
            r#"</w:sectPr></w:pPr><w:r><w:t>Cover page.</w:t></w:r></w:p>"#,
            r#"<w:p><w:pPr><w:rPr><w:del w:id="1" {who}/></w:rPr><w:sectPr>"#,
            r#"<w:headerReference w:type="default" r:id="rIdHead"/>"#,
            r#"<w:footerReference w:type="default" r:id="rIdFoot"/>"#,
            r#"</w:sectPr></w:pPr><w:r><w:t>Schedule.</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t>Main text.</w:t></w:r></w:p>"#,
            r#"<w:sectPr><w:headerReference w:type="default" r:id="rIdHead"/></w:sectPr>"#,
        ),
        who = WHO
    );
    let header = || {
        vec![(
            "headerReference".to_string(),
            "default".to_string(),
            "header1.xml".to_string(),
        )]
    };
    let accepted = Document::parse(&package(&body))
        .expect("the document loads")
        .project(Resolution::AcceptAll)
        .expect("the document resolves")
        .serialize(&ExportOptions::default())
        .expect("the resolved document saves");
    assert_eq!(sections(&accepted), [(header(), false), (header(), false)]);
}
