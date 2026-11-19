//! A tracked insertion that carries footnote references keeps its revision
//! identity when the document is edited elsewhere and saved.
//!
//! Shape: a body `w:ins` holding two `w:footnoteReference` runs between text
//! runs, and two notes inserted by the same author at the same time. Each
//! note story opens with an empty paragraph whose mark is inserted (Word
//! writes it with its own `w14:paraId`), followed by the inserted note text,
//! and closes with an untracked paragraph (a story's final pilcrow carries no
//! revision).
//!
//! Domain rule: a revision's identity is the address a reviewer resolves by
//! (RFC-0004 §H7). It is unique within a document and stable across every
//! edit that does not touch that revision, and across save and reopen. A
//! one-word tracked replacement in another paragraph adds exactly its own
//! deletion and insertion and changes nothing about the revisions already
//! pending — so the identity set after the edit is the original set plus
//! those two, and a save and reopen reproduces it exactly.

use std::collections::BTreeSet;
use std::io::{Cursor, Write};

use stemma::ExportOptions;
use stemma::api::Document;
use zip::ZipWriter;
use zip::write::FileOptions;

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const W14: &str = "http://schemas.microsoft.com/office/word/2010/wordml";
const WHO: &str = r#"w:author="Maja" w:date="2024-05-06T07:08:00Z""#;

fn text_run(text: &str) -> String {
    format!(r#"<w:r><w:t xml:space="preserve">{text}</w:t></w:r>"#)
}

fn note_reference(id: u32) -> String {
    format!(r#"<w:r><w:footnoteReference w:id="{id}"/></w:r>"#)
}

/// An inserted note: empty inserted-mark paragraph, inserted text paragraph,
/// untracked final paragraph. `first_wire` seeds the note's distinct wire ids.
fn inserted_note(id: u32, first_wire: u32, para_id: u32, text: &str) -> String {
    format!(
        concat!(
            r#"<w:footnote w:id="{id}">"#,
            r#"<w:p w14:paraId="{empty:08X}"><w:pPr><w:rPr><w:ins w:id="{w0}" {who}/></w:rPr></w:pPr></w:p>"#,
            r#"<w:p w14:paraId="{body:08X}"><w:pPr><w:rPr><w:ins w:id="{w1}" {who}/></w:rPr></w:pPr>"#,
            r#"<w:ins w:id="{w2}" {who}><w:r><w:footnoteRef/></w:r></w:ins>"#,
            r#"<w:ins w:id="{w3}" {who}>{text}</w:ins></w:p>"#,
            r#"<w:p w14:paraId="{tail:08X}"/>"#,
            r#"</w:footnote>"#
        ),
        id = id,
        empty = para_id,
        body = para_id + 1,
        tail = para_id + 2,
        w0 = first_wire,
        w1 = first_wire + 1,
        w2 = first_wire + 2,
        w3 = first_wire + 3,
        who = WHO,
        text = text_run(text),
    )
}

fn docx() -> Vec<u8> {
    let body = format!(
        concat!(
            r#"<w:p w14:paraId="00000A01">{intro}</w:p>"#,
            r#"<w:p w14:paraId="00000A02">{tugs}<w:ins w:id="10" {who}>"#,
            "{ref2}{pilots}{ref3}{crews}",
            r#"</w:ins>{stop}</w:p>"#,
        ),
        intro = text_run("The harbour opens at six."),
        tugs = text_run("Tugs"),
        who = WHO,
        ref2 = note_reference(2),
        pilots = text_run(", pilots"),
        ref3 = note_reference(3),
        crews = text_run(" and crews"),
        stop = text_run("."),
    );
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="{W}" xmlns:w14="{W14}"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let footnotes_xml = format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
            r#"<w:footnotes xmlns:w="{w}" xmlns:w14="{w14}">"#,
            r#"<w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote>"#,
            r#"<w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote>"#,
            "{note2}{note3}</w:footnotes>"
        ),
        w = W,
        w14 = W14,
        note2 = inserted_note(2, 20, 0x0B01, "Pilots board at the outer buoy."),
        note3 = inserted_note(3, 30, 0x0C01, "Crews change at noon."),
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/footnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let document_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdFn" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes" Target="footnotes.xml"/></Relationships>"#;

    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let options = FileOptions::default();
    for (name, data) in [
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", rels),
        ("word/_rels/document.xml.rels", document_rels),
        ("word/document.xml", document_xml.as_str()),
        ("word/footnotes.xml", footnotes_xml.as_str()),
    ] {
        zip.start_file(name, options).unwrap();
        zip.write_all(data.as_bytes()).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn identities(document: &Document) -> BTreeSet<u32> {
    stemma::tracked_model::enumerate_revisions(&document.snapshot().canonical)
        .iter()
        .map(|record| record.revision_id)
        .collect()
}

/// Replace "six" with "seven" in the first paragraph, tracked, as another
/// reviewer.
fn edit_elsewhere(document: &Document) -> Document {
    let json = serde_json::json!({"ops":[{"op":"replace","target":"p_1",
        "expect":"The harbour opens at six.","content":{"type":"paragraph",
            "content":[{"type":"text","text":"The harbour opens at seven."}]}}],
        "revision":{"author":"Editor"}})
    .to_string();
    let transaction = stemma::edit_v4::parse_transaction(&json)
        .expect("the transaction parses")
        .into_edit_transaction()
        .expect("the transaction lowers");
    document.apply(&transaction).expect("the edit applies")
}

fn reopen(document: &Document) -> Document {
    let saved = document
        .serialize(&ExportOptions::default())
        .expect("the document saves");
    Document::parse(&saved).expect("the saved document loads")
}

#[test]
fn a_save_keeps_the_identity_of_an_insertion_carrying_note_references() {
    let original = Document::parse(&docx()).expect("the document loads");
    assert_eq!(identities(&reopen(&original)), identities(&original));
}

#[test]
fn an_edit_elsewhere_keeps_the_identity_of_an_insertion_carrying_note_references() {
    let original = Document::parse(&docx()).expect("the document loads");
    let before = identities(&original);
    let edited = edit_elsewhere(&original);
    let after = identities(&edited);

    let gone: Vec<_> = before.difference(&after).collect();
    assert!(
        gone.is_empty(),
        "pending revisions changed identity: {gone:?}"
    );
    let added: Vec<_> = stemma::tracked_model::enumerate_revisions(&edited.snapshot().canonical)
        .into_iter()
        .filter(|record| !before.contains(&record.revision_id))
        .map(|record| (record.author, record.excerpt))
        .collect();
    assert_eq!(
        added,
        vec![
            (Some("Editor".to_string()), "six".to_string()),
            (Some("Editor".to_string()), "seven".to_string()),
        ],
        "the edit adds exactly its own deletion and insertion"
    );

    assert_eq!(
        identities(&reopen(&edited)),
        after,
        "a save reproduces them"
    );
}
