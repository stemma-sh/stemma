//! Authored paragraph/row identity contract (MS-DOCX §2.2.4 `w14:paraId`).
//!
//! `w14:paraId` identifies both `w:p` and `w:tr` elements, and the values share
//! one namespace. When a source document persists these identities, every
//! paragraph and row the engine authors must persist one too, unique across
//! the document. Word treats a package where some paragraphs/rows lack an id as
//! an identity family to regenerate, discarding every existing paraId (and the
//! document context) on open — even when the user then rejects the proposal.
//!
//! The contract, per authored owner:
//!   - each new paragraph (top-level or in a cell) and each new row carries a
//!     paraId after save and after Accept/Reject projections;
//!   - no two owners share a value;
//!   - every source paraId survives unchanged.

use std::collections::HashSet;

use stemma::api::Document;
use stemma::edit_v4::parse_transaction;
use stemma::{ExportOptions, Resolution};

const SOURCE_IDS: [&str; 8] = [
    "10000001", "10000002", "10000003", "10000004", "10000005", "10000006", "10000007", "10000008",
];

fn zip_docx(parts: &[(&str, &str)]) -> Vec<u8> {
    use std::io::Write;
    use zip::write::FileOptions;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        for (name, content) in parts {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

fn cell(para_id: &str, text: &str) -> String {
    format!(
        r#"<w:tc><w:tcPr><w:tcW w:w="2400" w:type="dxa"/></w:tcPr><w:p w14:paraId="{para_id}"><w:r><w:t>{text}</w:t></w:r></w:p></w:tc>"#
    )
}

/// Paragraph, 2×2 table, paragraph; every `w:p` and `w:tr` persists a paraId.
fn source_docx() -> Vec<u8> {
    let body = format!(
        r#"<w:p w14:paraId="10000001"><w:r><w:t>Intro.</w:t></w:r></w:p><w:tbl><w:tblPr><w:tblW w:w="4800" w:type="dxa"/></w:tblPr><w:tblGrid><w:gridCol w:w="2400"/><w:gridCol w:w="2400"/></w:tblGrid><w:tr w14:paraId="10000002">{}{}</w:tr><w:tr w14:paraId="10000005">{}{}</w:tr></w:tbl><w:p w14:paraId="10000006"><w:r><w:t>Outro.</w:t></w:r></w:p>"#,
        cell("10000003", "a"),
        cell("10000004", "b"),
        cell("10000007", "total"),
        cell("10000008", ""),
    );
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    zip_docx(&[
        (
            "[Content_Types].xml",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#,
        ),
        (
            "_rels/.rels",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#,
        ),
        (
            "word/_rels/document.xml.rels",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#,
        ),
        ("word/document.xml", &document_xml),
    ])
}

/// Every `<w:p>` / `<w:tr>` start tag in document.xml, with its paraId if any.
fn owner_para_ids(docx: &[u8]) -> Vec<(String, Option<String>)> {
    let archive = stemma::docx::DocxArchive::read(docx).expect("read package");
    let xml = String::from_utf8(archive.get("word/document.xml").unwrap().to_vec()).unwrap();
    let mut owners = Vec::new();
    for (start, _) in xml.match_indices('<') {
        let rest = &xml[start + 1..];
        let name_end = rest
            .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
            .unwrap_or(rest.len());
        let name = &rest[..name_end];
        if name != "w:p" && name != "w:tr" {
            continue;
        }
        let tag = &rest[..rest.find('>').expect("unterminated tag")];
        let para_id = tag.split("w14:paraId=\"").nth(1).map(|tail| {
            tail.split('"')
                .next()
                .expect("unterminated attr")
                .to_string()
        });
        owners.push((name.to_string(), para_id));
    }
    owners
}

fn assert_identity_contract(label: &str, docx: &[u8], expect_source: &[&str]) {
    let owners = owner_para_ids(docx);
    let missing: Vec<_> = owners.iter().filter(|(_, id)| id.is_none()).collect();
    assert!(
        missing.is_empty(),
        "{label}: every w:p/w:tr must persist w14:paraId; missing on {} of {} owners: {owners:?}",
        missing.len(),
        owners.len()
    );
    let ids: Vec<&str> = owners.iter().filter_map(|(_, id)| id.as_deref()).collect();
    let unique: HashSet<&str> = ids.iter().copied().collect();
    assert_eq!(
        unique.len(),
        ids.len(),
        "{label}: paraIds must be unique across paragraphs and rows: {owners:?}"
    );
    for source in expect_source {
        assert!(
            unique.contains(source),
            "{label}: source paraId {source} must survive: {owners:?}"
        );
    }
}

fn table_id(doc: &Document) -> String {
    doc.snapshot()
        .canonical
        .blocks
        .iter()
        .find_map(|tb| match &tb.block {
            stemma::domain::BlockNode::Table(t) => Some(t.id.0.to_string()),
            _ => None,
        })
        .expect("fixture has a table")
}

fn first_para_id(doc: &Document) -> String {
    doc.snapshot()
        .canonical
        .blocks
        .iter()
        .find_map(|tb| match &tb.block {
            stemma::domain::BlockNode::Paragraph(p) => Some(p.id.0.to_string()),
            _ => None,
        })
        .expect("fixture has a paragraph")
}

/// Apply `ops_json` in `mode`, then check the contract on the saved proposal,
/// its reopen, and the Accept/Reject projections of the reopened document.
fn check(label: &str, mode: &str, ops_json: impl Fn(&Document) -> String) {
    let source = Document::parse(&source_docx()).expect("parse source");
    let json = format!(
        r#"{{ "ops": [{}], "materialization_mode": "{mode}", "revision": {{ "author": "Counsel" }} }}"#,
        ops_json(&source)
    );
    let txn = parse_transaction(&json)
        .expect("schema")
        .into_edit_transaction()
        .expect("adapter");
    let edited = source.apply(&txn).expect("apply");
    let saved = edited.serialize(&ExportOptions::default()).expect("save");
    assert_identity_contract(&format!("{label}/{mode}: saved"), &saved, &SOURCE_IDS);

    let reopened = Document::parse(&saved).expect("reopen");
    let resaved = reopened
        .serialize(&ExportOptions::default())
        .expect("resave");
    assert_eq!(
        owner_para_ids(&saved),
        owner_para_ids(&resaved),
        "{label}/{mode}: reopen must not re-mint paragraph/row identity"
    );

    let accepted = reopened
        .project(Resolution::AcceptAll)
        .and_then(|d| d.serialize(&ExportOptions::default()))
        .expect("accept");
    assert_identity_contract(&format!("{label}/{mode}: accepted"), &accepted, &SOURCE_IDS);

    let rejected = reopened
        .project(Resolution::RejectAll)
        .and_then(|d| d.serialize(&ExportOptions::default()))
        .expect("reject");
    assert_identity_contract(&format!("{label}/{mode}: rejected"), &rejected, &SOURCE_IDS);
}

fn insert_table(doc: &Document) -> String {
    let cell = |text: &str| {
        format!(
            r#"{{"content":[{{"type":"paragraph","role":"body_text","content":[{{"type":"text","text":"{text}"}}]}}]}}"#
        )
    };
    format!(
        r#"{{"op":"insert","target":{{"anchor":"{}","position":"after"}},"content":[{{"type":"table","content":[{{"content":[{},{}]}},{{"content":[{},{}]}}]}}]}}"#,
        first_para_id(doc),
        cell("x1"),
        cell("y1"),
        cell("x2"),
        cell("y2"),
    )
}

fn insert_row(doc: &Document) -> String {
    format!(
        r#"{{"op":"table_op","target":"{}","table_op":{{"kind":"insert_row","ref_row":0,"position":"after","cells":["c","d"]}}}}"#,
        table_id(doc)
    )
}

#[test]
fn inserted_table_persists_row_and_cell_paragraph_identity() {
    for mode in ["tracked_change", "direct"] {
        check("insert table", mode, insert_table);
    }
}

#[test]
fn inserted_row_persists_row_and_cell_paragraph_identity() {
    for mode in ["tracked_change", "direct"] {
        check("insert_row", mode, insert_row);
    }
}

/// Two identical rows in one transaction must not collide.
#[test]
fn identical_inserted_rows_receive_distinct_identities() {
    for mode in ["tracked_change", "direct"] {
        check("insert_row x2", mode, |doc| {
            format!("{},{}", insert_row(doc), insert_row(doc))
        });
    }
}

#[test]
fn inserted_column_persists_new_cell_paragraph_identity() {
    for mode in ["tracked_change", "direct"] {
        check("insert_column", mode, |doc| {
            format!(
                r#"{{"op":"table_op","target":"{}","table_op":{{"kind":"insert_column","ref_col":0,"position":"after"}}}}"#,
                table_id(doc)
            )
        });
    }
}
