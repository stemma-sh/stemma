//! The comment read surface: every comment with its author, date, text,
//! resolved flag and thread position, from the full view's comment
//! projection.

/// A package with a comment thread (comment 1, replied to by comment 2) and a
/// separate resolved comment 3, anchored in one paragraph. Thread links and
/// the resolved flag live in the comments-extended part (§17.13.4,
/// `w15:paraIdParent`, `w15:done`).
fn commented_parts() -> Vec<(&'static str, String)> {
    let document = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:commentRangeStart w:id="1"/><w:commentRangeStart w:id="2"/><w:r><w:t xml:space="preserve">The fee is payable monthly</w:t></w:r><w:commentRangeEnd w:id="1"/><w:r><w:commentReference w:id="1"/></w:r><w:commentRangeEnd w:id="2"/><w:r><w:commentReference w:id="2"/></w:r><w:r><w:t xml:space="preserve"> and </w:t></w:r><w:commentRangeStart w:id="3"/><w:r><w:t>in advance</w:t></w:r><w:commentRangeEnd w:id="3"/><w:r><w:commentReference w:id="3"/></w:r><w:r><w:t>.</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;
    let comments = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml"><w:comment w:id="1" w:author="Priya" w:date="2026-10-01T09:00:00Z"><w:p w14:paraId="1A2B3C01"><w:r><w:t>Should this be quarterly?</w:t></w:r></w:p></w:comment><w:comment w:id="2" w:author="Tomas" w:date="2026-10-02T10:30:00Z"><w:p w14:paraId="1A2B3C02"><w:r><w:t>Monthly was agreed.</w:t></w:r></w:p></w:comment><w:comment w:id="3" w:author="Priya" w:date="2026-10-03T11:00:00Z"><w:p w14:paraId="1A2B3C03"><w:r><w:t>Confirm the advance wording.</w:t></w:r></w:p></w:comment></w:comments>"#;
    let extended = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w15:commentsEx xmlns:w15="http://schemas.microsoft.com/office/word/2012/wordml"><w15:commentEx w15:paraId="1A2B3C01" w15:done="0"/><w15:commentEx w15:paraId="1A2B3C02" w15:paraIdParent="1A2B3C01" w15:done="0"/><w15:commentEx w15:paraId="1A2B3C03" w15:done="1"/></w15:commentsEx>"#;
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/comments.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"/><Override PartName="/word/commentsExtended.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.commentsExtended+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="comments.xml"/><Relationship Id="rId3" Type="http://schemas.microsoft.com/office/2011/relationships/commentsExtended" Target="commentsExtended.xml"/></Relationships>"#;
    vec![
        ("[Content_Types].xml", content_types.to_string()),
        ("_rels/.rels", rels.to_string()),
        ("word/_rels/document.xml.rels", doc_rels.to_string()),
        ("word/document.xml", document.to_string()),
        ("word/comments.xml", comments.to_string()),
        ("word/commentsExtended.xml", extended.to_string()),
    ]
}

fn commented_docx() -> Vec<u8> {
    use stemma::docx::{DocxArchive, DocxFile};
    DocxArchive::from_parts(
        commented_parts()
            .into_iter()
            .map(|(name, data)| DocxFile {
                name: name.to_string(),
                data: data.into_bytes(),
            })
            .collect(),
    )
    .write()
    .expect("write commented package")
}

#[test]
fn comment_readings_report_a_thread_and_a_resolved_comment() {
    let document = stemma::api::Document::parse(&commented_docx()).expect("parse");
    let readings = stemma_diff::comment_readings(&document.snapshot().canonical);
    assert_eq!(
        serde_json::to_value(&readings).expect("serialize"),
        serde_json::json!([
            {"id": "1", "author": "Priya", "date": "2026-10-01T09:00:00Z",
             "text": "Should this be quarterly?", "resolved": false,
             "parent_id": null, "parent_para_id": null},
            {"id": "2", "author": "Tomas", "date": "2026-10-02T10:30:00Z",
             "text": "Monthly was agreed.", "resolved": false,
             "parent_id": "1", "parent_para_id": "1A2B3C01"},
            {"id": "3", "author": "Priya", "date": "2026-10-03T11:00:00Z",
             "text": "Confirm the advance wording.", "resolved": true,
             "parent_id": null, "parent_para_id": null}
        ])
    );
}

/// A thread link that names no comment stays visible: the reply is not
/// silently reported as a thread root.
#[test]
fn a_dangling_thread_link_keeps_its_raw_parent() {
    let parts: Vec<(&str, String)> = commented_parts()
        .into_iter()
        .map(|(name, data)| {
            (
                name,
                data.replace(
                    "w15:paraIdParent=\"1A2B3C01\"",
                    "w15:paraIdParent=\"0BADC0DE\"",
                ),
            )
        })
        .collect();
    let bytes = stemma::docx::DocxArchive::from_parts(
        parts
            .into_iter()
            .map(|(name, data)| stemma::docx::DocxFile {
                name: name.to_string(),
                data: data.into_bytes(),
            })
            .collect(),
    )
    .write()
    .expect("write");
    let document = stemma::api::Document::parse(&bytes).expect("parse");
    let readings = stemma_diff::comment_readings(&document.snapshot().canonical);
    assert_eq!(readings[1].parent_id, None);
    assert_eq!(readings[1].parent_para_id.as_deref(), Some("0BADC0DE"));
}
