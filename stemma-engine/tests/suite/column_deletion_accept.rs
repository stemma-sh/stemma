//! Accepting a tracked column deletion (`w:cellDel` down a column) through
//! the model: the deleted column gives its width to a neighbour only where
//! the neighbour grew. Word merges a deleted cell that keeps a paragraph
//! mark into the preceding cell, which takes its width; a cell deleted
//! whole, its paragraph marks tracked as deleted too, leaves with its width
//! (Word 16 observations, 2026-11-09, on these shapes).

use stemma::api::{Document, Resolution};
use stemma::domain::BlockNode;

const REV: &str = r#"w:author="Reviewer" w:date="2026-01-01T00:00:00Z""#;

/// How the deleted column's cells are written.
#[derive(Clone, Copy)]
enum Deleted {
    /// Text and paragraph mark tracked as deleted (the cell deleted whole).
    Whole,
    /// Text tracked as deleted, paragraph mark kept.
    TextOnly,
    /// Content untracked.
    Untracked,
}

fn cell(text: &str, width: u32, deleted: Option<Deleted>, id: &mut u32) -> String {
    let mut next = || {
        *id += 1;
        *id
    };
    match deleted {
        None => format!(
            r#"<w:tc><w:tcPr><w:tcW w:w="{width}" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:tc>"#
        ),
        Some(kind) => {
            let marker = format!(r#"<w:cellDel w:id="{}" {REV}/>"#, next());
            let paragraph = match kind {
                Deleted::Whole => format!(
                    r#"<w:p><w:pPr><w:rPr><w:del w:id="{}" {REV}/></w:rPr></w:pPr><w:del w:id="{}" {REV}><w:r><w:delText>{text}</w:delText></w:r></w:del></w:p>"#,
                    next(),
                    next()
                ),
                Deleted::TextOnly => format!(
                    r#"<w:p><w:del w:id="{}" {REV}><w:r><w:delText>{text}</w:delText></w:r></w:del></w:p>"#,
                    next()
                ),
                Deleted::Untracked => format!(r#"<w:p><w:r><w:t>{text}</w:t></w:r></w:p>"#),
            };
            format!(
                r#"<w:tc><w:tcPr><w:tcW w:w="{width}" w:type="dxa"/>{marker}</w:tcPr>{paragraph}</w:tc>"#
            )
        }
    }
}

/// A 4-column table (2000/2500/1500/3000) whose column 1 is tracked as
/// deleted, written as `deleted` says, under `table_properties`.
fn table_xml(table_properties: &str, deleted: Deleted) -> String {
    let widths = [2000, 2500, 1500, 3000];
    let rows = [
        ["Item", "Supplier", "Qty", "Notes"],
        [
            "Garden tools",
            "Alder Lane",
            "12",
            "Payable within thirty days",
        ],
    ];
    let mut id = 100;
    let mut xml = format!(
        "<w:tbl><w:tblPr>{table_properties}</w:tblPr><w:tblGrid>{}</w:tblGrid>",
        widths
            .iter()
            .map(|w| format!(r#"<w:gridCol w:w="{w}"/>"#))
            .collect::<String>()
    );
    for row in rows {
        xml.push_str("<w:tr>");
        for (index, (text, width)) in row.iter().zip(widths).enumerate() {
            xml.push_str(&cell(text, width, (index == 1).then_some(deleted), &mut id));
        }
        xml.push_str("</w:tr>");
    }
    xml.push_str("</w:tbl>");
    xml
}

fn docx(body: &str) -> Vec<u8> {
    use std::io::Write;
    let w = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#;
    let parts = [
        ("[Content_Types].xml", r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#.to_string()),
        ("_rels/.rels", r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#.to_string()),
        ("word/document.xml", format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document {w}><w:body>{body}<w:p><w:r><w:t>Closing note.</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#)),
    ];
    let mut bytes = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
        let options: zip::write::FileOptions = zip::write::FileOptions::default();
        for (path, content) in &parts {
            zip.start_file(*path, options).unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    bytes
}

/// The accepted table's grid and its first row's cell widths.
fn accepted_geometry(table_properties: &str, deleted: Deleted) -> (Vec<u32>, Vec<i64>) {
    let document = Document::parse(&docx(&table_xml(table_properties, deleted))).expect("parse");
    let accepted = document.project(Resolution::AcceptAll).expect("accept");
    let BlockNode::Table(table) = &accepted.snapshot().canonical.blocks[0].block else {
        panic!("a table first")
    };
    let widths = table.rows[0]
        .cells
        .iter()
        .map(|cell| {
            cell.formatting
                .width
                .as_ref()
                .map_or(-1, |w| i64::from(w.w))
        })
        .collect();
    (table.formatting.grid_cols.clone(), widths)
}

const FIXED_FITTING: &str = r#"<w:tblW w:w="6500" w:type="dxa"/><w:tblLayout w:type="fixed"/>"#;
const AUTO: &str = r#"<w:tblW w:w="0" w:type="auto"/>"#;

/// A fixed-layout table whose preferred width is the survivors' sum, its
/// column deleted whole (as another tool writes it): the survivors keep
/// their widths. Word's Accept gives 2000/1500/3000.
#[test]
fn a_column_deleted_whole_leaves_with_its_width() {
    assert_eq!(
        accepted_geometry(FIXED_FITTING, Deleted::Whole),
        (vec![2000, 1500, 3000], vec![2000, 1500, 3000])
    );
}

/// Auto width, the column deleted whole: no transfer either. Word's
/// Accept gives 2000/1500/3000.
#[test]
fn an_auto_width_column_deleted_whole_leaves_with_its_width() {
    assert_eq!(
        accepted_geometry(AUTO, Deleted::Whole),
        (vec![2000, 1500, 3000], vec![2000, 1500, 3000])
    );
}

/// A deleted cell that keeps its content untracked, or its paragraph mark,
/// merges into the preceding cell, which takes its width. Word's Accept
/// gives 4500/1500/3000 for both.
#[test]
fn a_deleted_cell_keeping_its_paragraph_gives_its_width_to_the_preceding_cell() {
    for deleted in [Deleted::Untracked, Deleted::TextOnly] {
        assert_eq!(
            accepted_geometry(AUTO, deleted),
            (vec![4500, 1500, 3000], vec![4500, 1500, 3000])
        );
    }
}
