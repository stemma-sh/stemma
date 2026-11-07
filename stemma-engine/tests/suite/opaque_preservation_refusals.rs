//! An edit that would destroy a paragraph's preserved inline is refused.
//!
//! Domain rule (operations.md `replace`; `edit_note`): a whole-paragraph
//! replacement carries the paragraph's preserved inlines — content controls,
//! images, fields, line breaks — as `opaque_ref`s, every one of them. One it
//! leaves out would be destroyed, so the edit is refused `OpaqueDestroyed`,
//! naming what is missing. A note's body is plain text with no way to carry a
//! preserved inline, so `edit_note` on a note holding one is refused the same
//! way. Neither may panic: a refusal is the outcome, whatever the paragraph
//! holds.

use stemma::api::Document;
use stemma::domain::{BlockNode, InlineNode};
use stemma::edit::{EditError, EditTransaction, apply_transaction};

fn package(document_xml: &str, footnotes_xml: Option<&str>) -> Vec<u8> {
    let (footnotes_override, footnotes_rel) = match footnotes_xml {
        Some(_) => (
            r#"<Override PartName="/word/footnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"/>"#,
            r#"<Relationship Id="rIdFn" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes" Target="footnotes.xml"/>"#,
        ),
        None => ("", ""),
    };
    let content_types = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>{footnotes_override}</Types>"#
    );
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{footnotes_rel}</Relationships>"#
    );
    use std::io::Write;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions = zip::write::FileOptions::default();
        let mut parts = vec![
            ("[Content_Types].xml", content_types.as_str()),
            ("_rels/.rels", rels),
            ("word/_rels/document.xml.rels", doc_rels.as_str()),
            ("word/document.xml", document_xml),
        ];
        if let Some(footnotes) = footnotes_xml {
            parts.push(("word/footnotes.xml", footnotes));
        }
        for (name, data) in parts {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn transaction(op: serde_json::Value) -> EditTransaction {
    let json = serde_json::json!({"ops":[op],"materialization_mode":"direct",
        "revision":{"author":"Editor"}})
    .to_string();
    stemma::edit_v4::parse_transaction(&json)
        .unwrap()
        .into_edit_transaction()
        .unwrap()
}

/// The opaque inline ids of body paragraph `p_1`, in reading order.
fn opaque_ids(doc: &Document) -> Vec<String> {
    let BlockNode::Paragraph(p) = &doc.snapshot().canonical.blocks[0].block else {
        panic!("p_1 is a paragraph");
    };
    p.segments
        .iter()
        .flat_map(|s| &s.inlines)
        .filter_map(|inline| match inline {
            InlineNode::OpaqueInline(o) => Some(o.id.to_string()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_replace_naming_one_of_two_content_controls_is_refused_naming_the_other() {
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}"><w:body><w:p><w:r><w:t xml:space="preserve">Clause 1: the Supplier shall deliver goods to the Customer.</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#
    );
    let doc = Document::parse(&package(&xml, None)).expect("parse");
    let wrap = |doc: &Document, expect: &str, tag: &str| {
        doc.apply(&transaction(
            serde_json::json!({"op":"wrap_content_control",
            "target":"p_1","expect":expect,"tag":tag,"control":{"kind":"plain_text"}}),
        ))
        .unwrap_or_else(|e| panic!("wrapping {expect:?} applies: {e}"))
    };
    let doc = wrap(&wrap(&doc, "Supplier", "party"), "Customer", "counterparty");
    let ids = opaque_ids(&doc);
    assert_eq!(ids.len(), 2, "two content controls: {ids:?}");

    let replace = transaction(serde_json::json!({"op":"replace","target":"p_1",
        "expect":"Clause 1","content":{"type":"paragraph","content":[
            {"type":"text","text":"Article 1: the "},
            {"type":"opaque_ref","attrs":{"id":ids[0]}},
            {"type":"text","text":" shall deliver goods to the "}]}}));
    let err = apply_transaction(&doc.snapshot().canonical, &replace)
        .expect_err("leaving out the second control destroys it");
    let EditError::OpaqueDestroyed {
        missing_opaque_ids, ..
    } = err
    else {
        panic!("expected OpaqueDestroyed, got {err:?}");
    };
    assert_eq!(missing_opaque_ids, vec![ids[1].clone()]);
}

#[test]
fn edit_note_on_a_note_holding_a_line_break_is_refused() {
    let footnotes = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:footnotes xmlns:w="{W}"><w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote><w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote><w:footnote w:id="1"><w:p><w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:footnoteRef/></w:r><w:r><w:t xml:space="preserve"> First line.</w:t><w:br/><w:t>Second line.</w:t></w:r></w:p></w:footnote></w:footnotes>"#
    );
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}"><w:body><w:p><w:r><w:t>The certificate is required.</w:t></w:r><w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:footnoteReference w:id="1"/></w:r></w:p><w:sectPr/></w:body></w:document>"#
    );
    let doc = Document::parse(&package(&xml, Some(&footnotes))).expect("parse");
    let edit = transaction(serde_json::json!({"op":"edit_note","note_id":"1",
        "note_kind":"footnote","body":"Replaced note."}));
    let err = apply_transaction(&doc.snapshot().canonical, &edit)
        .expect_err("the plain-text body cannot carry the line break");
    let EditError::OpaqueDestroyed {
        missing_inline_kinds,
        ..
    } = err
    else {
        panic!("expected OpaqueDestroyed, got {err:?}");
    };
    assert_eq!(missing_inline_kinds, vec!["hard_break"]);
}
