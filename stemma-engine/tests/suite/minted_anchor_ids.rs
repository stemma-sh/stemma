//! Every object an edit creates gets an id of its own.
//!
//! Domain rule (read-model.md): an anchor's `id` is its durable id, the
//! selector the anchor ops address, and a cell paragraph's `block_id` is that
//! paragraph's own id. So every id a read shows names one object in the
//! document. A second content control, image, equation or cross-reference
//! inserted into a paragraph, a link authored in a later transaction, and the
//! cell paragraphs of a second inserted table must each get an id no other
//! object holds, and an op addressed to that id must reach that object.

use std::collections::HashSet;

use stemma::api::Document;
use stemma::domain::{BlockNode, InlineNode, OpaqueKind};

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";

fn docx(body: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
    use std::io::Write;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions = zip::write::FileOptions::default();
        for (name, data) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/document.xml", document_xml.as_str()),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

fn paragraphs(texts: &[&str]) -> Document {
    let body: String = texts
        .iter()
        .map(|t| format!(r#"<w:p><w:r><w:t xml:space="preserve">{t}</w:t></w:r></w:p>"#))
        .collect();
    Document::parse(&docx(&body)).expect("parse")
}

fn apply(doc: &Document, op: serde_json::Value) -> Document {
    let json = serde_json::json!({"ops":[op.clone()],"materialization_mode":"direct",
        "revision":{"author":"Editor"}})
    .to_string();
    let txn = stemma::edit_v4::parse_transaction(&json)
        .unwrap()
        .into_edit_transaction()
        .unwrap();
    doc.apply(&txn)
        .unwrap_or_else(|e| panic!("{op} applies: {e}"))
}

/// (paragraph id, anchor id, anchor kind) of every opaque anchor in the body,
/// table cells included, in document order.
fn anchors(doc: &Document) -> Vec<(String, String, OpaqueKind)> {
    fn walk(block: &BlockNode, out: &mut Vec<(String, String, OpaqueKind)>) {
        match block {
            BlockNode::Paragraph(p) => {
                for inline in p.segments.iter().flat_map(|s| &s.inlines) {
                    if let InlineNode::OpaqueInline(o) = inline {
                        out.push((p.id.to_string(), o.id.to_string(), o.kind.clone()));
                    }
                }
            }
            BlockNode::Table(t) => {
                for cell in t.rows.iter().flat_map(|r| &r.cells) {
                    for b in &cell.blocks {
                        walk(b, out);
                    }
                }
            }
            BlockNode::OpaqueBlock(_) => {}
        }
    }
    let mut out = Vec::new();
    for tb in &doc.snapshot().canonical.blocks {
        walk(&tb.block, &mut out);
    }
    out
}

fn assert_unique(ids: &[String]) {
    let distinct: HashSet<&String> = ids.iter().collect();
    assert_eq!(distinct.len(), ids.len(), "ids repeat: {ids:?}");
}

fn ids_of_kind(doc: &Document, kind: fn(&OpaqueKind) -> bool) -> Vec<String> {
    anchors(doc)
        .into_iter()
        .filter(|(_, _, k)| kind(k))
        .map(|(_, id, _)| id)
        .collect()
}

#[test]
fn a_second_content_control_in_a_paragraph_gets_its_own_id_and_takes_its_own_value() {
    let doc = paragraphs(&["Between the Supplier and the Customer."]);
    let wrap = |doc: &Document, expect: &str, tag: &str| {
        apply(
            doc,
            serde_json::json!({"op":"wrap_content_control","target":"p_1",
            "expect":expect,"tag":tag,"control":{"kind":"plain_text"}}),
        )
    };
    let doc = wrap(&wrap(&doc, "Supplier", "party"), "Customer", "counterparty");
    let ids = ids_of_kind(&doc, |k| matches!(k, OpaqueKind::Sdt));
    assert_eq!(ids.len(), 2);
    assert_unique(&ids);

    // The second control in reading order wraps "Customer"; its id fills it.
    let filled = apply(
        &doc,
        serde_json::json!({"op":"set_content_control_value","target":"p_1",
        "sdt_id":ids[1],"text":"Acme Ltd"}),
    );
    let control_xml = |id: &str| -> String {
        let BlockNode::Paragraph(p) = &filled.snapshot().canonical.blocks[0].block else {
            panic!("p_1 is a paragraph");
        };
        p.segments
            .iter()
            .flat_map(|s| &s.inlines)
            .find_map(|inline| match inline {
                InlineNode::OpaqueInline(o) if o.id.to_string() == id => {
                    Some(String::from_utf8(o.raw_xml.clone().unwrap_or_default()).unwrap())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("control {id} is in p_1"))
    };
    assert!(control_xml(&ids[0]).contains("Supplier"));
    assert!(control_xml(&ids[1]).contains("Acme Ltd"));
    assert!(!control_xml(&ids[1]).contains("Customer"));
}

#[test]
fn a_second_image_equation_or_cross_reference_in_a_paragraph_gets_its_own_id() {
    let doc = paragraphs(&["See the Definitions and the Schedule for x."]);
    let twice = |op: serde_json::Value, second: serde_json::Value| apply(&apply(&doc, op), second);

    let images = twice(
        serde_json::json!({"op":"insert_image","target":"p_1","bytes_base64":PNG,
            "format":"png","alt_text":"first"}),
        serde_json::json!({"op":"insert_image","target":"p_1","bytes_base64":PNG,
            "format":"png","alt_text":"second"}),
    );
    let ids = ids_of_kind(&images, |k| matches!(k, OpaqueKind::Drawing));
    assert_eq!(ids.len(), 2);
    assert_unique(&ids);

    let m = "http://schemas.openxmlformats.org/officeDocument/2006/math";
    let omml = |t: &str| format!(r#"<m:oMath xmlns:m="{m}"><m:r><m:t>{t}</m:t></m:r></m:oMath>"#);
    let equations = twice(
        serde_json::json!({"op":"insert_equation","target":"p_1","expect":"Definitions",
            "omml":omml("a"),"placement":"inline"}),
        serde_json::json!({"op":"insert_equation","target":"p_1","expect":"Schedule",
            "omml":omml("b"),"placement":"inline"}),
    );
    let ids = ids_of_kind(&equations, |k| matches!(k, OpaqueKind::OmmlInline));
    assert_eq!(ids.len(), 2);
    assert_unique(&ids);

    let xrefs = twice(
        serde_json::json!({"op":"insert_cross_ref","target":"p_1","expect":"Definitions",
            "bookmark":"defs","ref_kind":"ref","as_hyperlink":true}),
        serde_json::json!({"op":"insert_cross_ref","target":"p_1","expect":"Schedule",
            "bookmark":"sched","ref_kind":"ref","as_hyperlink":true}),
    );
    let ids: Vec<String> = anchors(&xrefs).into_iter().map(|(_, id, _)| id).collect();
    assert_eq!(ids.len(), 2, "two cross-reference fields: {ids:?}");
    assert_unique(&ids);
}

fn link_ids(doc: &Document) -> Vec<(String, String)> {
    anchors(doc)
        .into_iter()
        .filter(|(_, _, k)| matches!(k, OpaqueKind::Hyperlink(_)))
        .map(|(p, id, _)| (p, id))
        .collect()
}

fn replace_with_link(doc: &Document, target: &str, expect: &str, href: &str) -> Document {
    apply(
        doc,
        serde_json::json!({"op":"replace","target":target,"expect":expect,
        "content":{"type":"paragraph","content":[{"type":"text","text":"See "},
            {"type":"hyperlink","attrs":{"href":href},
             "content":[{"type":"text","text":"the terms"}]}]}}),
    )
}

#[test]
fn links_authored_in_separate_transactions_get_their_own_ids() {
    let doc = paragraphs(&["See A.", "See B."]);
    let doc = replace_with_link(&doc, "p_1", "See A.", "https://example.com/a");
    let doc = replace_with_link(&doc, "p_2", "See B.", "https://example.com/b");
    let links = link_ids(&doc);
    assert_eq!(links.len(), 2);
    let ids: Vec<String> = links.iter().map(|(_, id)| id.clone()).collect();
    assert_unique(&ids);

    // The id read on p_2 retargets p_2's link, not p_1's.
    let (_, p2_link) = links.iter().find(|(p, _)| p == "p_2").unwrap();
    let retargeted = apply(
        &doc,
        serde_json::json!({"op":"set_attr","target":p2_link,
        "attrs":{"href":"https://example.com/c"},"expect_href":"https://example.com/b"}),
    );
    let bytes = retargeted
        .serialize(&stemma::ExportOptions::default())
        .unwrap();
    let rels = String::from_utf8(
        stemma::docx::DocxArchive::read(&bytes)
            .unwrap()
            .get("word/_rels/document.xml.rels")
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(rels.contains("https://example.com/a"), "{rels}");
    assert!(rels.contains("https://example.com/c"), "{rels}");

    // A later transaction keeping p_1's link and authoring a second one.
    let (_, p1_link) = links.iter().find(|(p, _)| p == "p_1").unwrap();
    let both = apply(
        &doc,
        serde_json::json!({"op":"replace","target":"p_1","expect":"See",
        "content":{"type":"paragraph","content":[{"type":"text","text":"See "},
            {"type":"opaque_ref","attrs":{"id":p1_link}},
            {"type":"text","text":" and "},
            {"type":"hyperlink","attrs":{"href":"https://example.com/d"},
             "content":[{"type":"text","text":"the annex"}]}]}}),
    );
    let ids: Vec<String> = link_ids(&both).into_iter().map(|(_, id)| id).collect();
    assert_eq!(ids.len(), 3, "{ids:?}");
    assert_unique(&ids);
}

fn one_cell_table(text: &str) -> serde_json::Value {
    serde_json::json!({"type":"table","content":[{"content":[{"content":[
        {"type":"paragraph","content":[{"type":"text","text":text}]}]}]}]})
}

/// Each table's cell paragraph ids, in document order.
fn cell_paragraph_ids(doc: &Document) -> Vec<Vec<String>> {
    doc.snapshot()
        .canonical
        .blocks
        .iter()
        .filter_map(|tb| match &tb.block {
            BlockNode::Table(t) => Some(
                t.rows
                    .iter()
                    .flat_map(|r| &r.cells)
                    .flat_map(|c| &c.blocks)
                    .filter_map(|b| match b {
                        BlockNode::Paragraph(p) => Some(p.id.to_string()),
                        _ => None,
                    })
                    .collect(),
            ),
            _ => None,
        })
        .collect()
}

#[test]
fn the_cell_paragraphs_of_a_second_inserted_table_get_their_own_ids() {
    let doc = paragraphs(&["Alpha", "Beta"]);
    let insert_after = |doc: &Document, anchor: &str, tables: Vec<serde_json::Value>| {
        apply(
            doc,
            serde_json::json!({"op":"insert","target":{"anchor":anchor,"position":"after"},
            "content":tables}),
        )
    };
    let doc = insert_after(&doc, "p_1", vec![one_cell_table("A1")]);
    let doc = insert_after(&doc, "p_2", vec![one_cell_table("B1")]);
    let cells = cell_paragraph_ids(&doc);
    assert_eq!(cells.len(), 2);
    let all: Vec<String> = cells.concat();
    assert_unique(&all);

    // The id read in the second table edits the second table.
    let edited = apply(
        &doc,
        serde_json::json!({"op":"replace","target":cells[1][0],"expect":"B1",
        "content":{"type":"paragraph","content":[{"type":"text","text":"B2"}]}}),
    );
    assert_eq!(
        edited.read_accepted().unwrap().to_text(),
        "Alpha\n\nA1\n\nBeta\n\nB2"
    );

    // Two tables in one insert.
    let doc = insert_after(
        &paragraphs(&["Alpha"]),
        "p_1",
        vec![one_cell_table("A1"), one_cell_table("B1")],
    );
    assert_unique(&cell_paragraph_ids(&doc).concat());
}

#[test]
fn paragraphs_inserted_with_links_give_each_link_its_own_id() {
    let doc = paragraphs(&["Alpha"]);
    let insert = |doc: &Document, href: &str| {
        apply(
            doc,
            serde_json::json!({"op":"insert","target":{"anchor":"p_1","position":"after"},
            "content":[{"type":"paragraph","attrs":{"format_from":"p_1"},"content":[{"type":"text","text":"See "},
                {"type":"hyperlink","attrs":{"href":href},
                 "content":[{"type":"text","text":"the terms"}]}]}]}),
        )
    };
    let doc = insert(
        &insert(&doc, "https://example.com/a"),
        "https://example.com/b",
    );
    let ids: Vec<String> = link_ids(&doc).into_iter().map(|(_, id)| id).collect();
    assert_eq!(ids.len(), 2, "{ids:?}");
    assert_unique(&ids);
}

/// Each table's id, in document order.
fn table_ids(doc: &Document) -> Vec<String> {
    doc.snapshot()
        .canonical
        .blocks
        .iter()
        .filter_map(|tb| match &tb.block {
            BlockNode::Table(t) => Some(t.id.to_string()),
            _ => None,
        })
        .collect()
}

fn two_cell_table(first: &str, second: &str) -> serde_json::Value {
    serde_json::json!({"type":"table","content":[{"content":[
        {"content":[{"type":"paragraph","content":[{"type":"text","text":first}]}]},
        {"content":[{"type":"paragraph","content":[{"type":"text","text":second}]}]}]}]})
}

#[test]
fn the_cell_paragraphs_of_a_whole_table_replacement_get_their_own_ids() {
    let doc = paragraphs(&["Alpha", "Beta"]);
    let insert_after = |doc: &Document, anchor: &str, table: serde_json::Value| {
        apply(
            doc,
            serde_json::json!({"op":"insert","target":{"anchor":anchor,"position":"after"},
            "content":[table]}),
        )
    };
    let doc = insert_after(&doc, "p_1", one_cell_table("A1"));
    let doc = insert_after(&doc, "p_2", one_cell_table("B1"));
    let second = table_ids(&doc)[1].clone();
    let replaced = apply(
        &doc,
        serde_json::json!({"op":"replace","target":second,
        "content":two_cell_table("C1","C2")}),
    );
    let cells = cell_paragraph_ids(&replaced);
    assert_eq!(cells.len(), 2);
    assert_eq!(cells[1].len(), 2);
    assert_unique(&cells.concat());

    // Each id read in the replacement edits its own cell.
    let edited = apply(
        &replaced,
        serde_json::json!({"op":"replace","target":cells[1][1],"expect":"C2",
        "content":{"type":"paragraph","content":[{"type":"text","text":"C3"}]}}),
    );
    assert_eq!(
        edited.read_accepted().unwrap().to_text(),
        "Alpha\n\nA1\n\nBeta\n\nC1 C3"
    );
}
