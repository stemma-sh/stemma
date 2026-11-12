//! A tracked edit of a hyperlink's display text writes its deletion and its
//! insertion as two revisions with their own `w:id`s: a revision id is
//! unique within a part (ECMA-376 §17.13.1). Checked in a body paragraph and
//! in a table cell, through save and reopen.

use std::collections::HashMap;

use stemma::ExportOptions;
use stemma::api::Document;

use crate::common::{docx_with_body_and_comments, read_zip_entry};

const BODY: &str = r#"<w:p><w:r><w:t xml:space="preserve">Read </w:t></w:r><w:hyperlink w:anchor="rules"><w:r><w:t>the old rules</w:t></w:r></w:hyperlink><w:r><w:t>.</w:t></w:r></w:p><w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="4000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t xml:space="preserve">Cell </w:t></w:r><w:hyperlink w:anchor="rules"><w:r><w:t>stale label</w:t></w:r></w:hyperlink></w:p></w:tc></w:tr></w:tbl><w:p/>"#;

/// The revision `w:id`s of `w:ins`/`w:del` elements in a part, counted.
fn revision_ids(xml: &str) -> HashMap<String, usize> {
    let mut ids = HashMap::new();
    for tag in ["<w:ins ", "<w:del "] {
        for rest in xml.split(tag).skip(1) {
            let id = rest
                .split("w:id=\"")
                .nth(1)
                .and_then(|v| v.split('"').next())
                .expect("a revision carries its id")
                .to_string();
            *ids.entry(id).or_insert(0) += 1;
        }
    }
    ids
}

fn link_ids(doc: &Document) -> Vec<String> {
    let view = serde_json::to_value(doc.read()).unwrap();
    let mut out = Vec::new();
    for block in view["blocks"].as_array().unwrap() {
        let mut segments: Vec<&serde_json::Value> =
            block["segments"].as_array().unwrap().iter().collect();
        for cell in block["cells"].as_array().into_iter().flatten() {
            for paragraph in cell["paragraphs"].as_array().unwrap() {
                segments.extend(paragraph["segments"].as_array().unwrap());
            }
        }
        for segment in segments {
            let opaque = segment.get("Opaque");
            if let Some(opaque) = opaque
                && opaque["kind"] == "Hyperlink"
            {
                let id = opaque
                    .get("id")
                    .or_else(|| opaque.get("opaque_id"))
                    .unwrap();
                out.push(id.as_str().unwrap().to_string());
            }
        }
    }
    out
}

#[test]
fn a_tracked_link_edit_gives_its_deletion_and_insertion_their_own_ids() {
    let doc = Document::parse(&docx_with_body_and_comments(BODY, &[])).expect("parse");
    let links = link_ids(&doc);
    assert_eq!(links.len(), 2, "{links:?}");
    let json = serde_json::json!({
        "ops": [
            {"op": "replace", "target": links[0], "expect": "the old rules",
             "content": {"type": "hyperlink", "attrs": {"anchor": "rules"},
                 "content": [{"type": "text", "text": "the new rules"}]}},
            {"op": "replace", "target": links[1], "expect": "stale label",
             "content": {"type": "hyperlink", "attrs": {"anchor": "rules"},
                 "content": [{"type": "text", "text": "fresh label"}]}},
        ],
        "revision": {"author": "Editor", "date": "2026-11-05T19:00:00Z"},
    })
    .to_string();
    let edited = doc
        .apply(
            &stemma::edit_v4::parse_transaction(&json)
                .unwrap()
                .into_edit_transaction()
                .unwrap(),
        )
        .expect("tracked link edits");
    let saved = edited.serialize(&ExportOptions::default()).expect("save");
    let reopened = Document::parse(&saved).expect("reopen");
    for bytes in [
        saved.clone(),
        reopened.serialize(&ExportOptions::default()).unwrap(),
    ] {
        let ids = revision_ids(&read_zip_entry(&bytes, "word/document.xml").unwrap());
        assert_eq!(ids.len(), 4, "two deletions and two insertions: {ids:?}");
        assert!(ids.values().all(|&n| n == 1), "no id repeats: {ids:?}");
    }
}
