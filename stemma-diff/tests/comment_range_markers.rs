//! The full render view brackets a commented span with both of the comment's
//! range markers, so a reader can find the span even when it runs across
//! paragraphs.
//!
//! Domain rule (§17.13.4.3-5): a comment's range is the content between its
//! `w:commentRangeStart` and its `w:commentRangeEnd`; the `w:commentReference`
//! is a separate mark that need not sit at the range end. The view shows
//! each as its own zero-width anchor with the comment id as `reference_id`.

use std::io::Write;

use stemma::api::Document;

fn docx(body: &str) -> Vec<u8> {
    let w = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="{w}"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let comments_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:comments xmlns:w="{w}"><w:comment w:id="4" w:author="Reviewer" w:date="2026-01-01T00:00:00Z"><w:p><w:r><w:t>Both sentences need work.</w:t></w:r></w:p></w:comment></w:comments>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/comments.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="comments.xml"/></Relationships>"#;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = zip::write::FileOptions::default();
        for (name, part) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/document.xml", document_xml.as_str()),
            ("word/comments.xml", comments_xml.as_str()),
        ] {
            zip.start_file(name, opts).expect("start part");
            zip.write_all(part.as_bytes()).expect("write part");
        }
        zip.finish().expect("finish package");
    }
    buf
}

/// Each block's segments as a compact stream: text as is, an anchor as
/// `[kind opaque_id ref]`.
fn streams(doc: &Document) -> Vec<String> {
    let view = stemma_diff::tracked_document_view(doc).expect("full view");
    view.blocks
        .iter()
        .map(|block| {
            let block = serde_json::to_value(block).unwrap();
            block["segments"]
                .as_array()
                .unwrap()
                .iter()
                .map(|segment| match segment.get("Opaque") {
                    Some(o) => format!(
                        "[{} {} {}]",
                        o["kind"].as_str().unwrap_or("?"),
                        o["opaque_id"].as_str().unwrap_or("?"),
                        o["reference_id"].as_str().unwrap_or("-"),
                    ),
                    None => segment
                        .as_object()
                        .and_then(|variant| variant.values().next())
                        .and_then(|data| data["text"].as_str())
                        .unwrap_or_default()
                        .to_string(),
                })
                .collect()
        })
        .collect()
}

#[test]
fn a_range_across_paragraphs_shows_its_start_and_its_end() {
    let doc = Document::parse(&docx(
        r#"<w:p><w:r><w:t xml:space="preserve">First </w:t></w:r><w:commentRangeStart w:id="4"/><w:r><w:t>sentence.</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve">Second</w:t></w:r><w:commentRangeEnd w:id="4"/><w:r><w:commentReference w:id="4"/></w:r><w:r><w:t xml:space="preserve"> sentence.</w:t></w:r></w:p>"#,
    ))
    .expect("parse");
    let streams = streams(&doc);
    assert_eq!(
        streams[0],
        "First [CommentRangeStart comment_4_start 4]sentence."
    );
    let reference = streams[1]
        .split("[CommentReference ")
        .nth(1)
        .and_then(|rest| rest.split(' ').next())
        .expect("the reference is its own anchor")
        .to_string();
    assert_eq!(
        streams[1],
        format!(
            "Second[CommentRangeEnd comment_4_end 4][CommentReference {reference} 4] sentence."
        )
    );
}
