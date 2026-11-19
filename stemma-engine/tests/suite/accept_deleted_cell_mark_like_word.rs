//! Accepting the tracked deletion of a table cell's final paragraph mark.
//!
//! Each expectation below was observed by having Word accept all and reject
//! all in small documents: a table of plain cells followed by a body
//! paragraph, the named cells' only paragraph carrying a deleted mark
//! (`w:pPr/w:rPr/w:del`).
//!
//! - a cell that is not the last in its row: Accept All joins its paragraph
//!   with the first paragraph of the next cell and the boundary between the
//!   two cells goes, as accepting a deleted mark joins two body paragraphs.
//!   This holds whatever else the next cell holds, whatever the cell spans,
//!   and whether the cell's text is untracked, partly deleted (what survives
//!   joins) or all deleted (the cell simply goes);
//! - several neighbouring cells: the joins chain;
//! - the last cell of a row: nothing structural happens;
//! - every cell of a row: the row goes and its text continues into the first
//!   cell of the next row or, for the last row, into the paragraph after the
//!   table; a table left without rows goes (with one cell and all of its
//!   text deleted, nothing of it is left).
//!
//! Reject All restores the original every time. Accepting only the marks is
//! their share of Accept All, so it changes the structure the same way (the
//! selective-resolution contract); any deleted text stays pending, which the
//! readings below do not show. A row must still hold at least one cell and
//! cover its grid, and a cell must still end with a paragraph, so every
//! accepted document must also validate.
//!
//! A cell of two paragraphs whose final mark is deleted is a shape Word's
//! own Accept All could not resolve; it keeps the engine's earlier reading
//! (pinned below as a characterization, not as Word's behaviour).

use std::collections::HashSet;
use std::io::{Cursor, Write};

use stemma::api::Document;
use stemma::docx::DocxArchive;
use stemma::{ExportOptions, Resolution, ResolveSelectionAction};
use zip::ZipWriter;
use zip::write::FileOptions;

const WHO: &str = r#"w:author="Ingrid" w:date="2024-03-02T09:10:00Z""#;
const WIDTH: u32 = 2000;
const CLOSING: &str = "Closing line.";

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

/// One cell paragraph: untracked text, then tracked-deleted text, and
/// whether its mark is deleted.
#[derive(Clone, Copy)]
struct Para {
    live: &'static str,
    gone: &'static str,
    mark: bool,
}

/// A paragraph of untracked text.
fn p(live: &'static str) -> Para {
    Para {
        live,
        gone: "",
        mark: false,
    }
}

/// A paragraph of untracked text whose mark is deleted.
fn m(live: &'static str) -> Para {
    Para {
        live,
        gone: "",
        mark: true,
    }
}

/// A paragraph whose mark is deleted, holding untracked then deleted text.
fn md(live: &'static str, gone: &'static str) -> Para {
    Para {
        live,
        gone,
        mark: true,
    }
}

/// A cell: its paragraphs and the grid columns it spans.
struct Cell(Vec<Para>, u32);

fn c(paragraphs: &[Para]) -> Cell {
    Cell(paragraphs.to_vec(), 1)
}

/// Builds a body and records the wire ids of the deleted marks.
#[derive(Default)]
struct Body {
    xml: String,
    next_id: u32,
    marks: Vec<u32>,
}

impl Body {
    fn id(&mut self) -> u32 {
        self.next_id += 1;
        self.next_id
    }

    fn paragraph(&mut self, para: Para) -> String {
        let ppr = if para.mark {
            let id = self.id();
            self.marks.push(id);
            format!(r#"<w:pPr><w:rPr><w:del w:id="{id}" {WHO}/></w:rPr></w:pPr>"#)
        } else {
            String::new()
        };
        let live = if para.live.is_empty() {
            String::new()
        } else {
            format!(
                r#"<w:r><w:t xml:space="preserve">{}</w:t></w:r>"#,
                para.live
            )
        };
        let gone = if para.gone.is_empty() {
            String::new()
        } else {
            format!(
                r#"<w:del w:id="{}" {WHO}><w:r><w:delText>{}</w:delText></w:r></w:del>"#,
                self.id(),
                para.gone
            )
        };
        format!("<w:p>{ppr}{live}{gone}</w:p>")
    }

    /// A table whose grid has `columns` columns, one row per entry.
    fn table(mut self, rows: &[Vec<Cell>]) -> Self {
        let columns: u32 = rows[0].iter().map(|cell| cell.1).sum();
        let grid: String = (0..columns)
            .map(|_| format!(r#"<w:gridCol w:w="{WIDTH}"/>"#))
            .collect();
        let mut xml = format!(
            r#"<w:tbl><w:tblPr><w:tblW w:w="{}" w:type="dxa"/></w:tblPr><w:tblGrid>{grid}</w:tblGrid>"#,
            WIDTH * columns
        );
        for row in rows {
            xml.push_str("<w:tr>");
            for Cell(paragraphs, span) in row {
                let span_pr = if *span > 1 {
                    format!(r#"<w:gridSpan w:val="{span}"/>"#)
                } else {
                    String::new()
                };
                xml.push_str(&format!(
                    r#"<w:tc><w:tcPr><w:tcW w:w="{}" w:type="dxa"/>{span_pr}</w:tcPr>"#,
                    WIDTH * span
                ));
                for para in paragraphs {
                    let paragraph = self.paragraph(*para);
                    xml.push_str(&paragraph);
                }
                xml.push_str("</w:tc>");
            }
            xml.push_str("</w:tr>");
        }
        xml.push_str("</w:tbl>");
        self.xml.push_str(&xml);
        self
    }

    fn text(mut self, text: &str) -> Self {
        self.xml
            .push_str(&format!("<w:p><w:r><w:t>{text}</w:t></w:r></w:p>"));
        self
    }

    fn empty_paragraph(mut self) -> Self {
        self.xml.push_str("<w:p/>");
        self
    }
}

/// A table followed by "Closing line.".
fn table(rows: &[Vec<Cell>]) -> Body {
    Body::default().table(rows).text(CLOSING)
}

/// What a resolved document holds: every paragraph's text in document order
/// (cells included, deleted text not), the number of rows and cells, and
/// whether each row covers the table grid (its cells' spans plus
/// `w:gridAfter`).
#[derive(Debug, PartialEq)]
struct Reading {
    paragraphs: Vec<String>,
    rows: usize,
    cells: usize,
    rows_cover_grid: bool,
}

fn local(element: &xmltree::Element) -> &str {
    element.name.rsplit(':').next().unwrap()
}

fn children<'a>(
    element: &'a xmltree::Element,
    name: &'a str,
) -> impl Iterator<Item = &'a xmltree::Element> {
    element
        .children
        .iter()
        .filter_map(|c| c.as_element())
        .filter(move |el| local(el) == name)
}

fn val(element: &xmltree::Element) -> u32 {
    element
        .attributes
        .iter()
        .find(|(name, _)| name.local_name == "val")
        .map(|(_, value)| value.parse().unwrap())
        .unwrap()
}

fn walk(element: &xmltree::Element, reading: &mut Reading) {
    match local(element) {
        "p" => {
            let mut text = String::new();
            collect_text(element, &mut text);
            reading.paragraphs.push(text);
            return;
        }
        "tr" => reading.rows += 1,
        "tc" => reading.cells += 1,
        "tbl" => {
            let grid = children(element, "tblGrid")
                .flat_map(|g| children(g, "gridCol"))
                .count() as u32;
            for row in children(element, "tr") {
                let after = children(row, "trPr")
                    .flat_map(|pr| children(pr, "gridAfter"))
                    .map(val)
                    .sum::<u32>();
                let spans: u32 = children(row, "tc")
                    .map(|tc| {
                        children(tc, "tcPr")
                            .flat_map(|pr| children(pr, "gridSpan"))
                            .map(val)
                            .next()
                            .unwrap_or(1)
                    })
                    .sum();
                if spans + after != grid {
                    reading.rows_cover_grid = false;
                }
            }
        }
        _ => {}
    }
    for child in element.children.iter().filter_map(|c| c.as_element()) {
        walk(child, reading);
    }
}

fn collect_text(element: &xmltree::Element, out: &mut String) {
    for child in &element.children {
        if let Some(el) = child.as_element() {
            if local(el) == "t" {
                out.push_str(&el.get_text().unwrap_or_default());
            } else {
                collect_text(el, out);
            }
        }
    }
}

fn reading(bytes: &[u8]) -> Reading {
    let archive = DocxArchive::read(bytes).expect("the package reads");
    let xml = archive.get("word/document.xml").expect("document part");
    let root = xmltree::Element::parse(xml).expect("document.xml parses");
    let mut reading = Reading {
        paragraphs: Vec::new(),
        rows: 0,
        cells: 0,
        rows_cover_grid: true,
    };
    walk(&root, &mut reading);
    reading
}

fn expected(paragraphs: &[&str], rows: usize, cells: usize) -> Reading {
    Reading {
        paragraphs: paragraphs.iter().map(|p| p.to_string()).collect(),
        rows,
        cells,
        rows_cover_grid: true,
    }
}

fn assert_valid(bytes: &[u8], label: &str) {
    let report = stemma::validate_docx_report(bytes).expect("validation runs");
    assert!(
        report.ok,
        "{label}: the resolved document validates: {report:?}"
    );
}

/// Resolve through the model (`Document::project`).
fn model(body: &str, resolution: Resolution) -> Vec<u8> {
    Document::parse(&docx(body))
        .expect("the document loads")
        .project(resolution)
        .expect("the document resolves")
        .serialize(&ExportOptions::default())
        .expect("the resolved document saves")
}

/// Resolve the package bytes directly.
fn wire(body: &str, accept: bool) -> Vec<u8> {
    let archive = DocxArchive::read(&docx(body)).expect("the package reads");
    let (out, _) = if accept {
        stemma::normalize::normalize_docx(&archive).expect("accept")
    } else {
        stemma::normalize::reject_all_docx(&archive).expect("reject")
    };
    out.write().expect("the resolved package writes")
}

/// Accept only the revisions with the given wire ids.
fn accept_marks(body: &str, wire_ids: &[u32]) -> Vec<u8> {
    let doc = Document::parse(&docx(body)).expect("the document loads");
    let ids: HashSet<u32> = doc
        .revisions()
        .into_iter()
        .filter(|record| wire_ids.contains(&record.wire_id))
        .map(|record| record.revision_id)
        .collect();
    assert_eq!(ids.len(), wire_ids.len(), "each mark is one revision");
    doc.project(Resolution::Selective {
        ids,
        action: ResolveSelectionAction::Accept,
    })
    .expect("the selection resolves")
    .serialize(&ExportOptions::default())
    .expect("the resolved document saves")
}

/// Accept All (model and wire) and accepting just the deleted marks read
/// `accepted`; Reject All (model and wire) reads `original`.
fn assert_resolves(body: &Body, accepted: Reading, original: Reading) {
    for (label, bytes) in [
        ("model accept", model(&body.xml, Resolution::AcceptAll)),
        ("wire accept", wire(&body.xml, true)),
        ("selective accept", accept_marks(&body.xml, &body.marks)),
    ] {
        assert_eq!(reading(&bytes), accepted, "{label}");
        assert_valid(&bytes, label);
    }
    for (label, bytes) in [
        ("model reject", model(&body.xml, Resolution::RejectAll)),
        ("wire reject", wire(&body.xml, false)),
    ] {
        assert_eq!(reading(&bytes), original, "{label}");
        assert_valid(&bytes, label);
    }
}

/// "Maple", "Birch", "Cedar"; `marks[i]` sets cell i's mark deleted.
fn three(marks: [bool; 3]) -> Vec<Cell> {
    ["Maple", "Birch", "Cedar"]
        .iter()
        .zip(marks)
        .map(|(text, mark)| c(&[if mark { m(text) } else { p(text) }]))
        .collect()
}

/// A second row "Fern", "Moss", "Reed".
fn plain_second_row() -> Vec<Cell> {
    vec![c(&[p("Fern")]), c(&[p("Moss")]), c(&[p("Reed")])]
}

fn original_three() -> Reading {
    expected(&["Maple", "Birch", "Cedar", CLOSING], 1, 3)
}

#[test]
fn a_middle_cell_joins_the_next_cell() {
    assert_resolves(
        &table(&[three([false, true, false])]),
        expected(&["Maple", "BirchCedar", CLOSING], 1, 2),
        original_three(),
    );
}

#[test]
fn the_first_cell_joins_the_second() {
    assert_resolves(
        &table(&[three([true, false, false])]),
        expected(&["MapleBirch", "Cedar", CLOSING], 1, 2),
        original_three(),
    );
}

#[test]
fn the_last_cell_of_a_row_stays() {
    assert_resolves(
        &table(&[three([false, false, true])]),
        original_three(),
        original_three(),
    );
}

#[test]
fn neighbouring_cells_chain_into_the_last_of_them() {
    let body = table(&[vec![
        c(&[p("Maple")]),
        c(&[m("Birch")]),
        c(&[m("Cedar")]),
        c(&[m("Spruce")]),
    ]]);
    assert_resolves(
        &body,
        expected(&["Maple", "BirchCedarSpruce", CLOSING], 1, 2),
        expected(&["Maple", "Birch", "Cedar", "Spruce", CLOSING], 1, 4),
    );
}

#[test]
fn a_single_row_table_whose_every_cell_joins_goes_into_the_next_paragraph() {
    assert_resolves(
        &table(&[three([true, true, true])]),
        expected(&["MapleBirchCedarClosing line."], 0, 0),
        original_three(),
    );
}

#[test]
fn a_one_cell_table_goes_into_the_next_paragraph() {
    assert_resolves(
        &table(&[vec![c(&[m("Maple")])]]),
        expected(&["MapleClosing line."], 0, 0),
        expected(&["Maple", CLOSING], 1, 1),
    );
}

#[test]
fn a_one_cell_table_with_partly_deleted_text_takes_what_survives_along() {
    assert_resolves(
        &table(&[vec![c(&[md("stay ", "lost")])]]),
        expected(&["stay Closing line."], 0, 0),
        expected(&["stay lost", CLOSING], 1, 1),
    );
}

#[test]
fn a_one_cell_table_with_all_text_deleted_goes_without_a_trace() {
    let accepted = expected(&[CLOSING], 0, 0);
    let body = table(&[vec![c(&[md("", "lost")])]]);
    for (label, bytes) in [
        ("model accept", model(&body.xml, Resolution::AcceptAll)),
        ("wire accept", wire(&body.xml, true)),
    ] {
        assert_eq!(reading(&bytes), accepted, "{label}");
        assert_valid(&bytes, label);
    }
    // Accepting just the mark removes the table; the deleted text, still
    // pending, joins the paragraph after it.
    let bytes = accept_marks(&body.xml, &body.marks);
    assert_eq!(reading(&bytes), accepted, "selective accept");
    assert_valid(&bytes, "selective accept");
    for (label, bytes) in [
        ("model reject", model(&body.xml, Resolution::RejectAll)),
        ("wire reject", wire(&body.xml, false)),
    ] {
        assert_eq!(
            reading(&bytes),
            expected(&["lost", CLOSING], 1, 1),
            "{label}"
        );
    }
}

#[test]
fn a_middle_cell_in_a_two_row_table_joins_within_its_row() {
    assert_resolves(
        &table(&[three([false, true, false]), plain_second_row()]),
        expected(
            &["Maple", "BirchCedar", "Fern", "Moss", "Reed", CLOSING],
            2,
            5,
        ),
        expected(
            &["Maple", "Birch", "Cedar", "Fern", "Moss", "Reed", CLOSING],
            2,
            6,
        ),
    );
}

#[test]
fn a_first_row_whose_every_cell_joins_goes_into_the_next_row() {
    assert_resolves(
        &table(&[three([true, true, true]), plain_second_row()]),
        expected(&["MapleBirchCedarFern", "Moss", "Reed", CLOSING], 1, 3),
        expected(
            &["Maple", "Birch", "Cedar", "Fern", "Moss", "Reed", CLOSING],
            2,
            6,
        ),
    );
}

#[test]
fn a_last_row_whose_every_cell_joins_goes_into_the_next_paragraph() {
    assert_resolves(
        &table(&[plain_second_row(), three([true, true, true])]),
        expected(
            &["Fern", "Moss", "Reed", "MapleBirchCedarClosing line."],
            1,
            3,
        ),
        expected(
            &["Fern", "Moss", "Reed", "Maple", "Birch", "Cedar", CLOSING],
            2,
            6,
        ),
    );
}

#[test]
fn a_middle_cell_with_all_text_deleted_goes() {
    let body = table(&[vec![
        c(&[p("Maple")]),
        c(&[md("", "Birch")]),
        c(&[p("Cedar")]),
    ]]);
    assert_resolves(
        &body,
        expected(&["Maple", "Cedar", CLOSING], 1, 2),
        original_three(),
    );
}

#[test]
fn a_middle_cell_with_partly_deleted_text_joins_with_what_survives() {
    let body = table(&[vec![
        c(&[p("Maple")]),
        c(&[md("Bir", "ch")]),
        c(&[p("Cedar")]),
    ]]);
    let accepted = expected(&["Maple", "BirCedar", CLOSING], 1, 2);
    for (label, bytes) in [
        ("model accept", model(&body.xml, Resolution::AcceptAll)),
        ("wire accept", wire(&body.xml, true)),
        ("selective accept", accept_marks(&body.xml, &body.marks)),
    ] {
        assert_eq!(reading(&bytes), accepted, "{label}");
        assert_valid(&bytes, label);
    }
    for (label, bytes) in [
        ("model reject", model(&body.xml, Resolution::RejectAll)),
        ("wire reject", wire(&body.xml, false)),
    ] {
        assert_eq!(
            reading(&bytes),
            expected(&["Maple", "Birch", "Cedar", CLOSING], 1, 3),
            "{label}"
        );
    }
}

#[test]
fn a_last_cell_with_partly_deleted_text_keeps_its_cell() {
    let body = table(&[vec![
        c(&[p("Maple")]),
        c(&[p("Birch")]),
        c(&[md("Ced", "ar")]),
    ]]);
    let accepted = expected(&["Maple", "Birch", "Ced", CLOSING], 1, 3);
    for (label, bytes) in [
        ("model accept", model(&body.xml, Resolution::AcceptAll)),
        ("wire accept", wire(&body.xml, true)),
        ("selective accept", accept_marks(&body.xml, &body.marks)),
    ] {
        assert_eq!(reading(&bytes), accepted, "{label}");
        assert_valid(&bytes, label);
    }
    for (label, bytes) in [
        ("model reject", model(&body.xml, Resolution::RejectAll)),
        ("wire reject", wire(&body.xml, false)),
    ] {
        assert_eq!(reading(&bytes), original_three(), "{label}");
    }
}

#[test]
fn a_cell_spanning_two_columns_joins_the_next_cell() {
    let body = table(&[vec![Cell(vec![m("Maple")], 2), c(&[p("Cedar")])]]);
    assert_resolves(
        &body,
        expected(&["MapleCedar", CLOSING], 1, 1),
        expected(&["Maple", "Cedar", CLOSING], 1, 2),
    );
}

#[test]
fn a_cell_joins_the_first_paragraph_of_a_next_cell_of_two() {
    let body = table(&[vec![
        c(&[m("Maple")]),
        c(&[p("Birch"), p("Larch")]),
        c(&[p("Cedar")]),
    ]]);
    assert_resolves(
        &body,
        expected(&["MapleBirch", "Larch", "Cedar", CLOSING], 1, 2),
        expected(&["Maple", "Birch", "Larch", "Cedar", CLOSING], 1, 3),
    );
}

#[test]
fn a_table_whose_cells_all_join_goes_into_an_empty_paragraph_after_it() {
    let body = Body::default()
        .text("Opening line.")
        .table(&[vec![c(&[m("Maple")]), c(&[m("Birch")])]])
        .empty_paragraph();
    assert_resolves(
        &body,
        expected(&["Opening line.", "MapleBirch"], 0, 0),
        expected(&["Opening line.", "Maple", "Birch", ""], 1, 2),
    );
}

#[test]
fn accepting_one_mark_of_several_joins_only_that_cell() {
    let body = table(&[three([true, true, false])]);
    let bytes = accept_marks(&body.xml, &body.marks[1..]);
    assert_eq!(
        reading(&bytes),
        expected(&["Maple", "BirchCedar", CLOSING], 1, 2)
    );
    assert_valid(&bytes, "partial selective accept");
    // The first cell's mark is still pending, and still resolves as Word
    // does once accepted.
    let reopened = Document::parse(&bytes).expect("the document reloads");
    assert_eq!(reopened.revisions().len(), 1, "one mark stays pending");
    let accepted = reopened
        .project(Resolution::AcceptAll)
        .expect("the rest resolves")
        .serialize(&ExportOptions::default())
        .expect("saves");
    assert_eq!(
        reading(&accepted),
        expected(&["MapleBirchCedar", CLOSING], 1, 1)
    );
}

/// CHARACTERIZATION (not Word's behaviour: Word's own Accept All failed on
/// this shape): a cell of two paragraphs whose final mark is deleted keeps
/// its cells; the mark has no paragraph to join inside the cell.
#[test]
fn a_two_paragraph_cell_with_a_deleted_final_mark_keeps_its_cells() {
    let body = table(&[vec![
        c(&[p("Maple")]),
        c(&[p("Birch"), m("Larch")]),
        c(&[p("Cedar")]),
    ]]);
    let original = expected(&["Maple", "Birch", "Larch", "Cedar", CLOSING], 1, 3);
    for (label, bytes) in [
        ("model accept", model(&body.xml, Resolution::AcceptAll)),
        ("wire accept", wire(&body.xml, true)),
    ] {
        assert_eq!(reading(&bytes), original, "{label}");
        assert_valid(&bytes, label);
    }
}
