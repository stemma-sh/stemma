//! A table an edit creates has a column grid.
//!
//! Domain rule: every table carries `w:tblGrid` (CT_Tbl, §17.4.49), the
//! column widths in twips that Word writes from the table's and its cells'
//! widths; without one a renderer sizes the columns by its own rules. A new
//! table spanning the text width with columns of one third and two thirds, on
//! a page with 9,360 twips of text width, has the grid 3,120 / 6,240.

use stemma::api::Document;

fn docx(sect_pr: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Appearances:</w:t></w:r></w:p>{sect_pr}</w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    use std::io::Write;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions = zip::write::FileOptions::default();
        for (name, data) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/document.xml", document_xml.as_str()),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

const LETTER: &str = r#"<w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="720" w:footer="720" w:gutter="0"/></w:sectPr>"#;

fn grid_after_insert(sect_pr: &str, cell_widths: Option<(u32, u32)>) -> Vec<u32> {
    let doc = Document::parse(&docx(sect_pr)).expect("parse");
    let anchor = doc.read().blocks[0].id.to_string();
    let cell = |text: &str, width: Option<u32>| {
        let mut cell = serde_json::json!({"content":[{"type":"paragraph","content":[{"type":"text","text":text}]}]});
        if let Some(w) = width {
            cell["attrs"] = serde_json::json!({"width":{"w":w,"width_type":"pct"}});
        }
        cell
    };
    let (a, b) = match cell_widths {
        Some((a, b)) => (Some(a), Some(b)),
        None => (None, None),
    };
    let json = serde_json::json!({"ops":[{"op":"insert","target":{"anchor":anchor,"position":"after"},"content":[{"type":"table","attrs":{"width":{"w":5000,"width_type":"pct"}},"content":[{"content":[cell("Party",a),cell("Representation",b)]},{"content":[cell("Applicants",a),cell("R Mabwe",b)]}]}]}],"revision":{"author":"Editor"}}).to_string();
    let edited = doc
        .apply(
            &stemma::edit_v4::parse_transaction(&json)
                .expect("parse")
                .into_edit_transaction()
                .expect("adapt"),
        )
        .expect("insert");
    let bytes = edited
        .serialize(&stemma::ExportOptions::default())
        .expect("save");
    let reopened = Document::parse(&bytes).expect("reopen");
    let grid_of = |doc: &Document| {
        doc.snapshot()
            .canonical
            .blocks
            .iter()
            .find_map(|tracked| match &tracked.block {
                stemma::domain::BlockNode::Table(table) => Some(table.formatting.grid_cols.clone()),
                _ => None,
            })
            .expect("the table")
    };
    let live = grid_of(&edited);
    assert_eq!(live, grid_of(&reopened), "the grid reads back as written");
    live
}

#[test]
fn a_new_table_gets_its_grid_from_the_cell_widths() {
    assert_eq!(
        grid_after_insert(LETTER, Some((1667, 3333))),
        vec![3120, 6240]
    );
}

#[test]
fn a_new_table_without_cell_widths_is_split_equally() {
    assert_eq!(grid_after_insert(LETTER, None), vec![4680, 4680]);
}

/// Word reads a section with no page size or margins as letter portrait with
/// 1440 margins, so the text width is 9360 as for an explicit letter page.
#[test]
fn a_section_without_a_page_size_is_read_as_words_default_page() {
    assert_eq!(grid_after_insert("<w:sectPr/>", None), vec![4680, 4680]);
}

fn apply(doc: &Document, ops: serde_json::Value, mode: &str) -> Document {
    let json =
        serde_json::json!({"ops":ops,"materialization_mode":mode,"revision":{"author":"Editor"}})
            .to_string();
    doc.apply(
        &stemma::edit_v4::parse_transaction(&json)
            .expect("parse")
            .into_edit_transaction()
            .expect("adapt"),
    )
    .expect("apply")
}

fn table_of(doc: &Document) -> (String, Vec<u32>) {
    doc.snapshot()
        .canonical
        .blocks
        .iter()
        .find_map(|tracked| match &tracked.block {
            stemma::domain::BlockNode::Table(table) => {
                Some((table.id.to_string(), table.formatting.grid_cols.clone()))
            }
            _ => None,
        })
        .expect("the table")
}

fn set_widths(table: &str, first: u32, second: u32) -> serde_json::Value {
    serde_json::json!([
        {"op":"set_cell_format","target":table,"row_index":0,"col_index":0,"width":{"w":first,"width_type":"pct"}},
        {"op":"set_cell_format","target":table,"row_index":0,"col_index":1,"width":{"w":second,"width_type":"pct"}}
    ])
}

/// Widths set after the table was inserted re-size its grid, as Word rewrites
/// the grid when column widths change: an agent that inserts the table first
/// and sizes its columns in a later edit gets the columns it asked for.
#[test]
fn widths_set_after_insertion_resize_the_grid() {
    let doc = Document::parse(&docx(LETTER)).expect("parse");
    let anchor = doc.read().blocks[0].id.to_string();
    let cell = |text: &str| serde_json::json!({"content":[{"type":"paragraph","content":[{"type":"text","text":text}]}]});
    for mode in ["direct", "tracked_change"] {
        let inserted = apply(
            &doc,
            serde_json::json!([{"op":"insert","target":{"anchor":anchor,"position":"after"},"content":[{"type":"table","attrs":{"width":{"w":5000,"width_type":"pct"}},"content":[{"content":[cell("Party"),cell("Representation")]}]}]}]),
            mode,
        );
        let (table, grid) = table_of(&inserted);
        assert_eq!(grid, vec![4680, 4680], "{mode}: equal columns at first");
        let sized = apply(&inserted, set_widths(&table, 1667, 3333), mode);
        assert_eq!(
            table_of(&sized).1,
            vec![3120, 6240],
            "{mode}: the grid follows the widths"
        );
    }
}

/// A tracked re-size of a table that came with the document keeps its grid:
/// rewriting the grid untracked would outlive a Reject of the new widths.
#[test]
fn a_tracked_resize_of_an_existing_table_keeps_its_grid() {
    let body = r#"<w:tbl><w:tblPr><w:tblW w:w="5000" w:type="pct"/></w:tblPr><w:tblGrid><w:gridCol w:w="4680"/><w:gridCol w:w="4680"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>Party</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Representation</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/>"#;
    let doc = Document::parse(&docx(&format!("{body}{LETTER}"))).expect("parse");
    let (table, grid) = table_of(&doc);
    assert_eq!(grid, vec![4680, 4680]);
    let sized = apply(&doc, set_widths(&table, 1667, 3333), "tracked_change");
    assert_eq!(table_of(&sized).1, vec![4680, 4680]);
    let direct = apply(&doc, set_widths(&table, 1667, 3333), "direct");
    assert_eq!(
        table_of(&direct).1,
        vec![3120, 6240],
        "a direct re-size rewrites it"
    );
}
