//! `expect` matches the block's read `text`.
//!
//! Domain rule (docs/reference/read-model.md): a block's `text` is "what a
//! plain `expect` matches against", and docs/reference/operations.md shapes
//! write `expect` as the text currently in the block. A paragraph's inline
//! anchors — comment range markers and references, hyperlinks, fields, line
//! breaks, proofing marks — add no characters to `text` and must not stop an
//! `expect` copied from it from matching. An operation that acts on a span of
//! text runs (`set_format`) also spans zero-width markers; where a span runs
//! across an object it cannot act on, the refusal names that object instead of
//! reporting the text as missing.

use stemma::api::Document;

const COMMENTS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:comment w:id="0" w:author="Reviewer"><w:p><w:r><w:t>Check this.</w:t></w:r></w:p></w:comment></w:comments>"#;

fn docx(body: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/comments.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="comments.xml"/></Relationships>"#;
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
            ("word/comments.xml", COMMENTS_XML),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

/// One paragraph per anchor shape, each followed by a plain paragraph so a
/// delete never removes the document's last paragraph.
const SHAPES: &[(&str, &str)] = &[
    (
        "comment range and reference",
        r#"<w:p><w:r><w:t xml:space="preserve">Body region </w:t></w:r><w:commentRangeStart w:id="0"/><w:r><w:t>alpha</w:t></w:r><w:commentRangeEnd w:id="0"/><w:r><w:commentReference w:id="0"/></w:r><w:r><w:t xml:space="preserve"> wording here.</w:t></w:r></w:p>"#,
    ),
    (
        "hyperlink",
        r#"<w:p><w:r><w:t xml:space="preserve">See the </w:t></w:r><w:hyperlink w:anchor="terms"><w:r><w:t>body link text</w:t></w:r></w:hyperlink><w:r><w:t xml:space="preserve"> now.</w:t></w:r></w:p>"#,
    ),
    (
        "simple field",
        r#"<w:p><w:r><w:t xml:space="preserve">Page </w:t></w:r><w:fldSimple w:instr=" PAGE "><w:r><w:t>3</w:t></w:r></w:fldSimple><w:r><w:t xml:space="preserve"> of the deed.</w:t></w:r></w:p>"#,
    ),
    (
        "line break",
        r#"<w:p><w:r><w:t>First line</w:t><w:br/><w:t>second line</w:t></w:r></w:p>"#,
    ),
    (
        "proofing marks",
        r#"<w:p><w:r><w:t xml:space="preserve">un </w:t></w:r><w:proofErr w:type="spellStart"/><w:r><w:t>mundo</w:t></w:r><w:proofErr w:type="spellEnd"/><w:r><w:t xml:space="preserve"> libre</w:t></w:r></w:p>"#,
    ),
];

fn apply(doc: &Document, op: serde_json::Value) -> Result<Document, String> {
    let json = serde_json::json!({"ops":[op],"revision":{"author":"Editor"}}).to_string();
    doc.apply(
        &stemma::edit_v4::parse_transaction(&json)
            .unwrap()
            .into_edit_transaction()
            .unwrap(),
    )
    .map_err(|e| e.to_string())
}

fn shape_doc(paragraph: &str) -> Document {
    Document::parse(&docx(&format!(
        "{paragraph}<w:p><w:r><w:t>Tail.</w:t></w:r></w:p>"
    )))
    .expect("parse")
}

#[test]
fn delete_accepts_the_blocks_own_text_as_expect() {
    for (shape, paragraph) in SHAPES {
        let doc = shape_doc(paragraph);
        let block = doc.read().blocks[0].clone();
        apply(
            &doc,
            serde_json::json!({"op":"delete","target":block.id.to_string(),"expect":block.text}),
        )
        .unwrap_or_else(|e| panic!("{shape}: expect {:?} is the block text: {e}", block.text));
    }
}

#[test]
fn replace_accepts_the_blocks_own_text_as_expect() {
    for (shape, paragraph) in SHAPES {
        let doc = shape_doc(paragraph);
        let block = doc.read().blocks[0].clone();
        // The replacement keeps every anchor where it is (each opaque segment
        // by id) and changes the last text segment's last word.
        let segments = serde_json::to_value(&block.segments).unwrap();
        let mut content: Vec<serde_json::Value> = Vec::new();
        for segment in segments.as_array().unwrap() {
            if let Some(text) = segment.get("Text") {
                content.push(serde_json::json!({"type":"text","text":text["text"]}));
            } else {
                let opaque = &segment["Opaque"];
                content.push(serde_json::json!({"type":"opaque_ref","attrs":{"id":opaque["id"]}}));
            }
        }
        let last_text = content
            .iter_mut()
            .rev()
            .find(|c| c["type"] == "text")
            .expect("the paragraph has text");
        let edited = format!("{} Amended.", last_text["text"].as_str().unwrap());
        last_text["text"] = serde_json::Value::String(edited);
        apply(
            &doc,
            serde_json::json!({"op":"replace","target":block.id.to_string(),"expect":block.text,
                "content":{"type":"paragraph","content":content}}),
        )
        .unwrap_or_else(|e| panic!("{shape}: expect {:?} is the block text: {e}", block.text));
    }
}

#[test]
fn set_format_spans_zero_width_markers() {
    for (shape, paragraph, expect) in [
        ("proofing marks", SHAPES[4].1, "mundo libre"),
        ("comment range", SHAPES[0].1, "region alpha"),
    ] {
        let doc = shape_doc(paragraph);
        let target = doc.read().blocks[0].id.to_string();
        let edited = apply(
            &doc,
            serde_json::json!({"op":"set_format","target":target,"expect":expect,
                "marks":{"bold":true}}),
        )
        .unwrap_or_else(|e| panic!("{shape}: {expect:?} is in the block text: {e}"));
        let accepted = edited.read_accepted().expect("accept");
        let bold: String = accepted.read().blocks[0]
            .segments
            .iter()
            .filter_map(|segment| match segment {
                stemma::view::SegmentView::Text { text, marks, .. }
                    if marks.iter().any(|m| format!("{m:?}") == "Bold") =>
                {
                    Some(text.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(bold, expect, "{shape}: exactly the expect text is bold");
    }
}

#[test]
fn set_format_across_a_hyperlink_names_the_link() {
    let doc = shape_doc(SHAPES[1].1);
    let block = doc.read().blocks[0].clone();
    let link_id = block
        .segments
        .iter()
        .find_map(|segment| match segment {
            stemma::view::SegmentView::Opaque { id, .. } => Some(id.to_string()),
            _ => None,
        })
        .expect("the hyperlink is an anchor");
    let Err(error) = apply(
        &doc,
        serde_json::json!({"op":"set_format","target":block.id.to_string(),"expect":block.text,
            "marks":{"bold":true}}),
    ) else {
        panic!("a link's text is not a run set_format can reach");
    };
    assert!(
        error.contains("hyperlink") && error.contains(&link_id),
        "the refusal says the span runs across the link {link_id}: {error}"
    );
}
