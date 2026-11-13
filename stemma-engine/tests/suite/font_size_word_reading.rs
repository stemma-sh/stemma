//! A run's font size is read as Word reads it (Word 16, verified on the
//! oracle): `w:sz` / `w:szCs` (ST_HpsMeasure, §17.3.2.38) values "21.0",
//! "21.4", "21.5" and "21.6" all mean 21 half-points (truncated, not
//! rounded), and a non-numeric value ("abc") means 0, which Word saves as 0.
//! The model holds the value Word reads; an untouched run keeps the bytes the
//! document wrote.

use stemma::ExportOptions;
use stemma::api::Document;
use stemma::domain::{BlockNode, InlineNode};

use crate::common::{docx_with_body_and_comments, read_zip_entry};

fn sizes(doc: &Document) -> Vec<String> {
    let bytes = doc.serialize(&ExportOptions::default()).expect("save");
    let xml = read_zip_entry(&bytes, "word/document.xml").expect("document.xml");
    xml.split("<w:sz w:val=\"")
        .skip(1)
        .map(|rest| rest.split('"').next().unwrap().to_string())
        .collect()
}

#[test]
fn decimal_sizes_truncate_and_a_non_numeric_size_is_zero() {
    let run = |size: &str, text: &str| {
        format!(
            r#"<w:r><w:rPr><w:sz w:val="{size}"/></w:rPr><w:t xml:space="preserve">{text} </w:t></w:r>"#
        )
    };
    let body = format!(
        "<w:p>{}{}{}{}{}</w:p>",
        run("21.0", "Alpha"),
        run("21.4", "Beta"),
        run("21.5", "Gamma"),
        run("21.6", "Delta"),
        run("abc", "Epsilon"),
    );
    let doc = Document::parse(&docx_with_body_and_comments(&body, &[])).expect("Word opens it");
    let BlockNode::Paragraph(paragraph) = &doc.snapshot().canonical.blocks[0].block else {
        panic!("a paragraph");
    };
    let read: Vec<Option<u32>> = paragraph
        .segments
        .iter()
        .flat_map(|segment| &segment.inlines)
        .filter_map(|inline| match inline {
            InlineNode::Text(text) => Some(text.style_props.font_size),
            _ => None,
        })
        .collect();
    assert_eq!(read, [Some(21), Some(21), Some(21), Some(21), Some(0)]);
    assert_eq!(sizes(&doc), ["21.0", "21.4", "21.5", "21.6", "abc"]);
}
