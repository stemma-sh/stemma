//! A pending insertion of a comment's annotation mark survives an edit
//! elsewhere in the document.
//!
//! Word writes a comment inserted with change tracking on as a `w:ins` around
//! the comment's `w:annotationRef` run (§17.13.5.18, §17.13.4.1): the mark is
//! the inserted content, the comment text that follows may or may not be in
//! the same insertion. The insertion is a revision of the comment story, so
//! an edit to an unrelated body paragraph must save it exactly as it was:
//! cli.md's id durability rule says the id of a revision whose own content is
//! untouched survives serialize and reopen.

use stemma::ExportOptions;
use stemma::StoryScope;
use stemma::api::Document;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn docx(comment_paragraph: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W_NS}"><w:body><w:p><w:r><w:t>Edit me here.</w:t></w:r></w:p><w:p><w:commentRangeStart w:id="0"/><w:r><w:t>Anchored words.</w:t></w:r><w:commentRangeEnd w:id="0"/><w:r><w:commentReference w:id="0"/></w:r></w:p><w:sectPr/></w:body></w:document>"#
    );
    let comments_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:comments xmlns:w="{W_NS}"><w:comment w:id="0" w:author="Reviewer" w:date="2026-01-01T00:00:00Z" w:initials="R">{comment_paragraph}</w:comment></w:comments>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/comments.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let document_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="comments.xml"/></Relationships>"#;
    use std::io::Write;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions = zip::write::FileOptions::default();
        for (name, data) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/_rels/document.xml.rels", document_rels),
            ("word/document.xml", document_xml.as_str()),
            ("word/comments.xml", comments_xml.as_str()),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

/// The comment story's revisions (identity, kind) of `doc`.
fn comment_revisions(doc: &Document) -> Vec<(u32, String)> {
    doc.revisions()
        .into_iter()
        .filter(|record| matches!(record.location, StoryScope::Comment { .. }))
        .map(|record| (record.revision_id, format!("{:?}", record.kind)))
        .collect()
}

/// Edit the first body paragraph, save, and return the saved bytes.
fn saved_after_body_edit(doc: &Document) -> Vec<u8> {
    let paragraph = doc.read().blocks[0].id.to_string();
    let json = serde_json::json!({"ops":[{"op":"replace","target":paragraph,
        "expect":"Edit me here.","content":{"type":"paragraph",
            "content":[{"type":"text","text":"Edit me there."}]}}],
        "revision":{"author":"Clerk"}})
    .to_string();
    let edited = doc
        .apply(
            &stemma::edit_v4::parse_transaction(&json)
                .unwrap()
                .into_edit_transaction()
                .unwrap(),
        )
        .expect("the body paragraph is edited");
    edited.serialize(&ExportOptions::default()).expect("save")
}

fn comments_part(bytes: &[u8]) -> String {
    String::from_utf8(
        stemma::docx::DocxArchive::read(bytes)
            .unwrap()
            .get("word/comments.xml")
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

#[test]
fn an_inserted_annotation_mark_keeps_its_insertion_after_a_body_edit() {
    let original = Document::parse(&docx(
        r#"<w:p><w:ins w:id="2" w:author="Reviewer" w:date="2026-01-01T00:00:00Z"><w:r><w:annotationRef/></w:r></w:ins><w:r><w:t>Why these words?</w:t></w:r></w:p>"#,
    ))
    .expect("parse");
    let before = comment_revisions(&original);
    assert_eq!(
        before.len(),
        1,
        "the comment story has one pending insertion: {before:?}"
    );

    let saved = saved_after_body_edit(&original);
    let comments = comments_part(&saved);
    let ins = comments
        .find("<w:ins ")
        .unwrap_or_else(|| panic!("the insertion is saved: {comments}"));
    let mark = comments
        .find("<w:annotationRef")
        .unwrap_or_else(|| panic!("the annotation mark is saved: {comments}"));
    let ins_end = comments[ins..].find("</w:ins>").unwrap() + ins;
    assert!(
        ins < mark && mark < ins_end,
        "the annotation mark is still inside the insertion: {comments}"
    );
    assert!(
        !comments[ins..ins_end].contains("Why these words?"),
        "the comment text, which was not inserted, stays outside it: {comments}"
    );

    let reopened = Document::parse(&saved).expect("reopen");
    assert_eq!(
        comment_revisions(&reopened),
        before,
        "the comment's revision survives the save"
    );
}
