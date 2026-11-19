//! A table row moved with tracking on.
//!
//! Word records a moved row without row marks: every cell paragraph of the
//! source row has its text in `w:moveFrom` and its mark moved from
//! (`w:pPr/w:rPr/w:moveFrom`), every cell paragraph of the destination row
//! the same with `w:moveTo`, the halves paired by the move range's name.
//! A deletion struck before the move sits inside the moved text of both
//! copies (`w:del` nested in `w:moveFrom` and in `w:moveTo`).
//!
//! Observed in Word 16, in a table that also holds inserted rows:
//!
//! - Accept All removes the source row and keeps the destination row
//!   without the deleted text;
//! - Reject All keeps the source row with the deleted text restored and
//!   removes the destination row with everything in it.

use std::io::{Cursor, Write};

use stemma::api::Document;
use stemma::docx::DocxArchive;
use stemma::{ExportOptions, Resolution};
use zip::ZipWriter;
use zip::write::FileOptions;

const WHO: &str = r#"w:author="Mover" w:date="2024-05-06T07:08:00Z""#;

fn docx(body: &str) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let options = FileOptions::default();
    zip.start_file("[Content_Types].xml", options).unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#).unwrap();
    zip.start_file("_rels/.rels", options).unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#).unwrap();
    zip.start_file("word/_rels/document.xml.rels", options)
        .unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#).unwrap();
    zip.start_file("word/document.xml", options).unwrap();
    zip.write_all(
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
        )
        .as_bytes(),
    )
    .unwrap();
    zip.finish().unwrap().into_inner()
}

fn cell(content: &str, mark: &str) -> String {
    format!(
        r#"<w:tc><w:tcPr><w:tcW w:w="2000" w:type="dxa"/></w:tcPr><w:p>{mark}{content}</w:p></w:tc>"#
    )
}

fn plain_row(texts: [&str; 3]) -> String {
    let cells: String = texts
        .iter()
        .map(|text| cell(&format!("<w:r><w:t>{text}</w:t></w:r>"), ""))
        .collect();
    format!("<w:tr>{cells}</w:tr>")
}

/// One copy of the moved row: `side` is `moveFrom` or `moveTo`, `range`
/// the range id, `first` the first wire id the copy may use.
fn moved_row(side: &str, range: u32, first: u32) -> String {
    let mut id = first;
    let mut next = || {
        id += 1;
        id
    };
    let mut cells = String::new();
    for (index, text) in ["Gamma", "Delta", "Epsilon"].iter().enumerate() {
        let mark = format!(
            r#"<w:pPr><w:rPr><w:{side} w:id="{}" {WHO}/></w:rPr></w:pPr>"#,
            next()
        );
        let start = if index == 0 {
            format!(r#"<w:{side}RangeStart w:id="{range}" {WHO} w:name="move1"/>"#)
        } else {
            String::new()
        };
        let struck = if index == 2 {
            format!(
                r#"<w:del w:id="{}" {WHO}><w:r><w:delText>!</w:delText></w:r></w:del>"#,
                next()
            )
        } else {
            String::new()
        };
        let content = format!(
            r#"{start}<w:{side} w:id="{}" {WHO}><w:r><w:t>{text}</w:t></w:r>{struck}</w:{side}>"#,
            next()
        );
        cells.push_str(&cell(&content, &mark));
    }
    format!(r#"<w:tr>{cells}</w:tr><w:{side}RangeEnd w:id="{range}"/>"#)
}

fn inserted_row(first: u32) -> String {
    let mut cells = String::new();
    for (index, text) in ["Zeta", "Eta", "Theta"].iter().enumerate() {
        let id = first + 2 * index as u32;
        let mark = format!(r#"<w:pPr><w:rPr><w:ins w:id="{id}" {WHO}/></w:rPr></w:pPr>"#);
        let content = format!(
            r#"<w:ins w:id="{}" {WHO}><w:r><w:t>{text}</w:t></w:r></w:ins>"#,
            id + 1
        );
        cells.push_str(&cell(&content, &mark));
    }
    format!(
        r#"<w:tr><w:trPr><w:ins w:id="{}" {WHO}/></w:trPr>{cells}</w:tr>"#,
        first + 10
    )
}

fn body() -> String {
    let grid = r#"<w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="2000"/><w:gridCol w:w="2000"/></w:tblGrid>"#;
    format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="6000" w:type="dxa"/></w:tblPr>{grid}{}{}{}{}{}</w:tbl><w:p><w:r><w:t>Closing line.</w:t></w:r></w:p>"#,
        plain_row(["Alpha", "Beta", "Head"]),
        moved_row("moveFrom", 100, 100),
        inserted_row(200),
        moved_row("moveTo", 300, 300),
        plain_row(["Iota", "Kappa", "Lambda"]),
    )
}

fn local(element: &xmltree::Element) -> &str {
    element.name.rsplit(':').next().unwrap()
}

fn collect_text(element: &xmltree::Element, out: &mut String) {
    for child in element.children.iter().filter_map(|c| c.as_element()) {
        if local(child) == "t" {
            out.push_str(&child.get_text().unwrap_or_default());
        } else {
            collect_text(child, out);
        }
    }
}

/// Each row's cell texts joined by `|`, then the paragraphs after the table.
fn rows(bytes: &[u8]) -> Vec<String> {
    fn walk(element: &xmltree::Element, out: &mut Vec<String>) {
        match local(element) {
            "tr" => {
                let cells: Vec<String> = element
                    .children
                    .iter()
                    .filter_map(|c| c.as_element())
                    .filter(|el| local(el) == "tc")
                    .map(|tc| {
                        let mut text = String::new();
                        collect_text(tc, &mut text);
                        text
                    })
                    .collect();
                out.push(cells.join("|"));
            }
            "p" => {
                let mut text = String::new();
                collect_text(element, &mut text);
                out.push(text);
            }
            _ => {
                for child in element.children.iter().filter_map(|c| c.as_element()) {
                    walk(child, out);
                }
            }
        }
    }
    let archive = DocxArchive::read(bytes).expect("the package reads");
    let root = xmltree::Element::parse(archive.get("word/document.xml").expect("document part"))
        .expect("document.xml parses");
    let mut out = Vec::new();
    walk(&root, &mut out);
    out
}

fn model(resolution: Resolution) -> Vec<String> {
    let bytes = Document::parse(&docx(&body()))
        .expect("the document loads")
        .project(resolution)
        .expect("the document resolves")
        .serialize(&ExportOptions::default())
        .expect("the resolved document saves");
    let report = stemma::validate_docx_report(&bytes).expect("validation runs");
    assert!(report.ok, "the resolved document validates: {report:?}");
    rows(&bytes)
}

fn wire(accept: bool) -> Vec<String> {
    let archive = DocxArchive::read(&docx(&body())).expect("the package reads");
    let (out, _) = if accept {
        stemma::normalize::normalize_docx(&archive).expect("accept")
    } else {
        stemma::normalize::reject_all_docx(&archive).expect("reject")
    };
    rows(&out.write().expect("the resolved package writes"))
}

const ACCEPTED: [&str; 5] = [
    "Alpha|Beta|Head",
    "Zeta|Eta|Theta",
    "Gamma|Delta|Epsilon",
    "Iota|Kappa|Lambda",
    "Closing line.",
];

const REJECTED: [&str; 4] = [
    "Alpha|Beta|Head",
    "Gamma|Delta|Epsilon!",
    "Iota|Kappa|Lambda",
    "Closing line.",
];

#[test]
fn accept_all_removes_the_source_row_of_a_moved_row() {
    assert_eq!(model(Resolution::AcceptAll), ACCEPTED);
}

#[test]
fn reject_all_removes_the_destination_row_of_a_moved_row() {
    assert_eq!(model(Resolution::RejectAll), REJECTED);
}

#[test]
fn the_package_resolution_reads_a_moved_row_the_same() {
    assert_eq!(wire(true), ACCEPTED);
    assert_eq!(wire(false), REJECTED);
}
