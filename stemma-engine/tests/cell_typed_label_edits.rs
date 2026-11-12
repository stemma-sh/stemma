//! A typed list label ("1." then a tab, no `w:numPr`) is text the author
//! wrote at the start of a paragraph. When a tracked edit deletes or inserts
//! a whole table-cell paragraph, the label belongs to that deletion or
//! insertion: Reject All restores the cell as it was and Accept All gives
//! the edited cell, labels included, in memory and after save and reopen,
//! and resolving one such revision on its own leaves the other intact.

use std::io::Write;

use stemma::api::{Document, ResolveSelectionAction};
use stemma::domain::{BlockNode, InlineNode, NodeId};
use stemma::{ExportOptions, Resolution};

fn cell_docx(cell_paragraphs: &[&str]) -> Vec<u8> {
    let cell: String = cell_paragraphs
        .iter()
        .map(|text| {
            let runs: String = text
                .split('\t')
                .enumerate()
                .map(|(i, part)| {
                    let tab = if i > 0 { "<w:tab/>" } else { "" };
                    format!(r#"<w:r>{tab}<w:t xml:space="preserve">{part}</w:t></w:r>"#)
                })
                .collect();
            format!("<w:p>{runs}</w:p>")
        })
        .collect();
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Meeting notes</w:t></w:r></w:p><w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="8000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="8000" w:type="dxa"/></w:tcPr>{cell}</w:tc></w:tr></w:tbl><w:p/><w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"></Relationships>"#;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = zip::write::FileOptions::default();
        for (name, part) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/_rels/document.xml.rels", doc_rels),
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
    let bytes = document
        .serialize(&ExportOptions::default())
        .expect("serialize");
    Document::parse(&bytes).expect("reopen")
}

fn cell_paragraph_ids(document: &Document) -> Vec<NodeId> {
    let BlockNode::Table(table) = &document.snapshot().canonical.blocks[1].block else {
        panic!("the second block is the table");
    };
    table.rows[0].cells[0]
        .blocks
        .iter()
        .map(|block| match block {
            BlockNode::Paragraph(p) => p.id.clone(),
            _ => panic!("a cell block that is not a paragraph"),
        })
        .collect()
}

/// The cell's paragraphs as written: typed label (with its whitespace) and
/// text.
fn cell_reading(document: &Document) -> Vec<String> {
    let BlockNode::Table(table) = &document.snapshot().canonical.blocks[1].block else {
        panic!("the second block is the table");
    };
    table.rows[0].cells[0]
        .blocks
        .iter()
        .map(|block| {
            let BlockNode::Paragraph(p) = block else {
                panic!("a cell block that is not a paragraph");
            };
            let mut out = String::new();
            if let Some(label) = &p.literal_prefix {
                out.push_str(&p.literal_prefix_leading_ws);
                out.push_str(label);
                out.push_str(&p.literal_prefix_trailing_ws);
            }
            for inline in p.all_inlines() {
                if let InlineNode::Text(t) = inline {
                    out.push_str(&t.text);
                }
            }
            out
        })
        .collect()
}

/// The reading of `cell` as a document of its own, saved and reopened.
fn expected(cell: &[&str]) -> Vec<String> {
    cell_reading(&reopened(
        &Document::parse(&cell_docx(cell)).expect("parse expected"),
    ))
}

fn reading(document: &Document, resolution: Resolution) -> Vec<String> {
    let resolved = reopened(&document.project(resolution).expect("resolve"));
    assert!(resolved.revisions().is_empty(), "a resolved reading");
    cell_reading(&resolved)
}

fn apply(document: &Document, ops: serde_json::Value) -> Document {
    let json = serde_json::json!({ "revision": {"author": "Reviewer"}, "ops": ops }).to_string();
    let transaction = stemma::edit_v4::parse_transaction(&json)
        .expect("schema accepts the ops")
        .into_edit_transaction()
        .expect("ops translate");
    document.apply(&transaction).expect("apply edit")
}

fn assert_laws(edited: &Document, base: &[&str], target: &[&str]) {
    let reopened_edit = reopened(edited);
    for (name, doc) in [("in memory", edited), ("reopened", &reopened_edit)] {
        assert_eq!(
            reading(doc, Resolution::RejectAll),
            expected(base),
            "{name}: Reject All restores the cell"
        );
        assert_eq!(
            reading(doc, Resolution::AcceptAll),
            expected(target),
            "{name}: Accept All gives the edited cell"
        );
    }
}

const CELL: &[&str] = &[
    "Opening words from the chair",
    "1.\tUpdate from the regional office",
    "2.\tStaffing over the winter months",
    "Closing remarks by the chair",
];

#[test]
fn a_tracked_deletion_of_a_labelled_cell_paragraph_takes_its_label() {
    let base = Document::parse(&cell_docx(CELL)).expect("parse");
    let ids = cell_paragraph_ids(&base);
    let edited = apply(
        &base,
        serde_json::json!([{ "op": "delete", "target": ids[1].0, "expect": "Update from the regional office" }]),
    );
    assert_laws(
        &edited,
        CELL,
        &[CELL[0], "2.\tStaffing over the winter months", CELL[3]],
    );
}

/// The insertion verb writes the typed label as inserted text from the
/// start, so this held before the producer rule; it guards the law.
#[test]
fn a_tracked_insertion_of_a_labelled_cell_paragraph_brings_its_label() {
    let base = Document::parse(&cell_docx(CELL)).expect("parse");
    let ids = cell_paragraph_ids(&base);
    let edited = apply(
        &base,
        serde_json::json!([{
            "op": "insert",
            "target": {"anchor": ids[2].0, "position": "after"},
            "content": [{"type": "paragraph",
                "content": [{"type": "text", "text": "3.\tBudget for the next quarter"}]}]
        }]),
    );
    assert_laws(
        &edited,
        CELL,
        &[
            CELL[0],
            CELL[1],
            CELL[2],
            "3.\tBudget for the next quarter",
            CELL[3],
        ],
    );
}

/// Two deletions, two revisions: rejecting one alone restores that
/// paragraph with its label and leaves the other deletion pending (the
/// resolution projection keeps each label with its own proposal).
#[test]
fn resolving_one_labelled_cell_deletion_leaves_the_other() {
    let base = Document::parse(&cell_docx(CELL)).expect("parse");
    let ids = cell_paragraph_ids(&base);
    let first = apply(
        &base,
        serde_json::json!([{ "op": "delete", "target": ids[1].0, "expect": "Update from the regional office" }]),
    );
    let edited = apply(
        &first,
        serde_json::json!([{ "op": "delete", "target": ids[2].0, "expect": "Staffing over the winter months" }]),
    );
    let revisions = edited.revisions();
    let first_ids: Vec<u32> = first.revisions().iter().map(|r| r.revision_id).collect();
    let second: Vec<u32> = revisions
        .iter()
        .map(|r| r.revision_id)
        .filter(|id| !first_ids.contains(id))
        .collect();
    assert!(
        !second.is_empty(),
        "the second deletion is its own revision"
    );
    let one_rejected = edited
        .project(Resolution::Selective {
            ids: second.iter().copied().collect(),
            action: ResolveSelectionAction::Reject,
        })
        .expect("reject the second deletion");
    // `assert_laws` checks it in memory and after save and reopen.
    assert_laws(
        &one_rejected,
        CELL,
        &[CELL[0], "2.\tStaffing over the winter months", CELL[3]],
    );
}
