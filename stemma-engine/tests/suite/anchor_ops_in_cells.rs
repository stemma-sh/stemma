//! The anchor-splice verbs reach paragraphs inside table cells.
//!
//! Domain rule: a table cell holds ordinary paragraphs (§17.4.66 `w:tc`
//! contains `w:p`), and Word lets a reviewer comment on, footnote, bookmark,
//! cross-reference, wrap in a content control, add an image or equation to,
//! restyle and renumber cell text exactly as body text. The reads hand out
//! cell paragraph ids, so every op that targets a paragraph by id must land
//! its artifact in that cell paragraph — nested tables included. `move`
//! relocates whole top-level blocks (block-level moveFrom/moveTo), which a
//! cell paragraph cannot carry, so it refuses with an error that says the
//! block is in a table cell, never "not found".

use serde_json::{Value, json};
use stemma::ExportOptions;
use stemma::api::Document;
use stemma::domain::BlockNode;
use stemma::runtime::{ErrorCode, RuntimeError};

const DOCUMENT_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body><w:p><w:r><w:t>Body paragraph.</w:t></w:r></w:p><w:tbl><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid><w:tr><w:tc><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Cell region alpha wording.</w:t></w:r></w:p><w:tbl><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>Nested region gamma wording.</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/></w:tc></w:tr></w:tbl><w:p><w:r><w:t>Final paragraph.</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;

const STYLES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style><w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/></w:style></w:styles>"#;

const NUMBERING_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl><w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="lowerLetter"/><w:lvlText w:val="(%2)"/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#;

/// A 1x1 PNG.
const PNG_1X1: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";

fn docx() -> Vec<u8> {
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let document_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId10" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rId11" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/></Relationships>"#;
    use std::io::Write;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions = zip::write::FileOptions::default();
        for (name, data) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/_rels/document.xml.rels", document_rels),
            ("word/document.xml", DOCUMENT_XML),
            ("word/styles.xml", STYLES_XML),
            ("word/numbering.xml", NUMBERING_XML),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

/// The ids the reads show: (body paragraph, cell paragraph, nested-cell paragraph).
fn ids(doc: &Document) -> (String, String, String) {
    let snapshot = doc.snapshot();
    let blocks = &snapshot.canonical.blocks;
    let BlockNode::Paragraph(body) = &blocks[0].block else {
        panic!("the first block is the body paragraph")
    };
    let BlockNode::Table(table) = &blocks[1].block else {
        panic!("the second block is the table")
    };
    let cell = &table.rows[0].cells[0].blocks;
    let BlockNode::Paragraph(cell_para) = &cell[0] else {
        panic!("the cell opens with a paragraph")
    };
    let BlockNode::Table(nested) = &cell[1] else {
        panic!("the cell holds a nested table")
    };
    let BlockNode::Paragraph(nested_para) = &nested.rows[0].cells[0].blocks[0] else {
        panic!("the nested cell holds a paragraph")
    };
    (
        body.id.to_string(),
        cell_para.id.to_string(),
        nested_para.id.to_string(),
    )
}

fn apply(doc: &Document, op: Value) -> Result<Document, RuntimeError> {
    let json = json!({"ops":[op],"revision":{"author":"Reviewer"}}).to_string();
    let txn = stemma::edit_v4::parse_transaction(&json)
        .expect("wire transaction parses")
        .into_edit_transaction()
        .expect("wire transaction lowers");
    doc.apply(&txn)
}

/// The serialized main document part, self-closing tags normalized.
fn document_xml(doc: &Document) -> String {
    let bytes = doc.serialize(&ExportOptions::default()).expect("save");
    String::from_utf8(
        stemma::docx::DocxArchive::read(&bytes)
            .unwrap()
            .get("word/document.xml")
            .unwrap()
            .to_vec(),
    )
    .unwrap()
    .replace(" />", "/>")
}

/// The XML of the `w:p` whose text contains `text`.
fn paragraph_xml<'a>(xml: &'a str, text: &str) -> &'a str {
    let at = xml
        .find(text)
        .unwrap_or_else(|| panic!("{text:?} in {xml}"));
    let start = xml[..at]
        .rfind("<w:p>")
        .into_iter()
        .chain(xml[..at].rfind("<w:p "))
        .max()
        .expect("paragraph start");
    let end = at + xml[at..].find("</w:p>").expect("paragraph end");
    &xml[start..end]
}

/// Each op, aimed at the cell paragraph, lands its artifact there.
#[test]
fn anchor_ops_land_in_a_cell_paragraph() {
    let doc = Document::parse(&docx()).expect("parse");
    let (_, cell, _) = ids(&doc);
    let omml = r#"<m:oMath xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math"><m:r><m:t>x=1</m:t></m:r></m:oMath>"#;
    let cases: Vec<(&str, Value, &str)> = vec![
        (
            "comment_create",
            json!({"op":"comment_create","target":cell,"expect":"alpha","body":"Why alpha?"}),
            "<w:commentRangeStart",
        ),
        (
            "insert_note",
            json!({"op":"insert_note","target":cell,"expect":"alpha","note_kind":"footnote","body":"A cell note."}),
            "<w:footnoteReference",
        ),
        (
            "insert_bookmark",
            json!({"op":"insert_bookmark","target":cell,"expect":"alpha","name":"cell_mark"}),
            r#"w:name="cell_mark""#,
        ),
        (
            "insert_cross_ref",
            json!({"op":"insert_cross_ref","target":cell,"expect":"alpha","bookmark":"elsewhere","ref_kind":"ref"}),
            "REF elsewhere",
        ),
        (
            "wrap_content_control",
            json!({"op":"wrap_content_control","target":cell,"expect":"alpha","tag":"term","control":{"kind":"plain_text"}}),
            r#"<w:tag w:val="term"/>"#,
        ),
        (
            "insert_image",
            json!({"op":"insert_image","target":cell,"bytes_base64":PNG_1X1,"format":"png","cx":91440,"cy":91440,"alt_text":"dot"}),
            "<w:drawing",
        ),
        (
            "insert_equation",
            json!({"op":"insert_equation","target":cell,"expect":"alpha","omml":omml,"placement":"inline"}),
            "<m:oMath",
        ),
        (
            "apply_style",
            json!({"op":"apply_style","target":cell,"style_id":"Heading1"}),
            r#"<w:pStyle w:val="Heading1"/>"#,
        ),
        (
            "set_numbering indent",
            json!({"op":"set_numbering","target":cell,"change":{"kind":"indent"}}),
            r#"<w:ilvl w:val="1"/>"#,
        ),
        (
            "set_numbering remove",
            json!({"op":"set_numbering","target":cell,"change":{"kind":"remove"}}),
            r#"<w:numId w:val="0"/>"#,
        ),
    ];
    for (name, op, marker) in cases {
        let edited = apply(&doc, op).unwrap_or_else(|e| panic!("{name} on a cell paragraph: {e}"));
        let xml = document_xml(&edited);
        let para = paragraph_xml(&xml, "Cell region");
        assert!(
            para.contains(marker),
            "{name}: the cell paragraph carries {marker}: {para}"
        );
        let body = paragraph_xml(&xml, "Body paragraph.");
        assert!(
            !body.contains(marker),
            "{name}: the body paragraph is untouched: {body}"
        );
    }
}

/// Splitting a list at a cell item gives that item a new list of its own.
#[test]
fn a_list_splits_at_a_cell_item() {
    let doc = Document::parse(&docx()).expect("parse");
    let (_, cell, _) = ids(&doc);
    let edited = apply(
        &doc,
        json!({"op":"set_numbering","target":cell,"change":{"kind":"split"}}),
    )
    .expect("split at a cell item");
    let xml = document_xml(&edited);
    let para = paragraph_xml(&xml, "Cell region");
    let live = para.split("<w:pPrChange").next().unwrap();
    assert!(
        !live.contains(r#"<w:numId w:val="1"/>"#),
        "the cell item's live numbering leaves list 1: {para}"
    );
}

/// A nested cell paragraph is reached too.
#[test]
fn a_comment_lands_in_a_nested_cell_paragraph() {
    let doc = Document::parse(&docx()).expect("parse");
    let (_, _, nested) = ids(&doc);
    let edited = apply(
        &doc,
        json!({"op":"comment_create","target":nested,"expect":"gamma","body":"Why gamma?"}),
    )
    .expect("comment on a nested cell paragraph");
    let xml = document_xml(&edited);
    let para = paragraph_xml(&xml, "Nested region");
    assert!(para.contains("<w:commentRangeStart"), "{para}");
}

/// `move` names the cell as the reason it cannot reach the paragraph.
#[test]
fn move_refuses_a_cell_paragraph_as_in_a_cell() {
    let doc = Document::parse(&docx()).expect("parse");
    let (body, cell, _) = ids(&doc);
    for op in [
        json!({"op":"move","target":cell,"destination":{"anchor":body,"position":"after"}}),
        json!({"op":"move","target":body,"destination":{"anchor":cell,"position":"after"}}),
    ] {
        let Err(err) = apply(&doc, op.clone()) else {
            panic!("{op}: move cannot reach a cell paragraph")
        };
        let message = err.to_string();
        assert!(
            message.contains("table cell") && !message.contains("not found"),
            "{op}: {message}"
        );
        assert_eq!(err.code, ErrorCode::UnsupportedEdit, "{op}: {message}");
        assert!(
            message.contains(&format!("'{cell}'")),
            "{op}: the refusal names the cell paragraph: {message}"
        );
    }
}

/// An image in a cell can be re-laid-out and have its media replaced by the
/// drawing id the reads show, as an image in a body paragraph can.
#[test]
fn an_image_in_a_cell_is_reached_by_the_image_ops() {
    let doc = Document::parse(&docx()).expect("parse");
    let (_, cell, _) = ids(&doc);
    let with_image = apply(
        &doc,
        json!({"op":"insert_image","target":cell,"bytes_base64":PNG_1X1,"format":"png","cx":91440,"cy":91440,"alt_text":"dot"}),
    )
    .expect("insert an image in the cell");
    let reopened = Document::parse(
        &with_image
            .serialize(&ExportOptions::default())
            .expect("save"),
    )
    .expect("reopen");
    let (_, cell, _) = ids(&reopened);
    let snapshot = reopened.snapshot();
    let BlockNode::Table(table) = &snapshot.canonical.blocks[1].block else {
        panic!("the table")
    };
    let BlockNode::Paragraph(p) = &table.rows[0].cells[0].blocks[0] else {
        panic!("the cell paragraph")
    };
    let drawing = p
        .segments
        .iter()
        .flat_map(|s| &s.inlines)
        .find_map(|inline| match inline {
            stemma::domain::InlineNode::OpaqueInline(o)
                if matches!(o.kind, stemma::domain::OpaqueKind::Drawing) =>
            {
                Some(o.id.to_string())
            }
            _ => None,
        })
        .expect("the cell paragraph holds the drawing");

    let cropped = apply(
        &reopened,
        json!({"op":"set_image_layout","target":cell,"drawing_id":drawing,"crop":{"left":10000}}),
    )
    .expect("crop the cell image");
    let xml = document_xml(&cropped);
    assert!(
        paragraph_xml(&xml, "Cell region").contains(r#"<a:srcRect l="10000""#),
        "the cell image is cropped: {xml}"
    );

    apply(
        &reopened,
        json!({"op":"replace_image","target":cell,"drawing_id":drawing,"bytes_base64":PNG_1X1,"format":"png"}),
    )
    .expect("replace the cell image's media");
}
