//! A pending paragraph-mark revision beside an edit of the paragraph's text.
//!
//! A paragraph mark's `w:ins`/`w:del` (§17.13.5.16/15) records a paragraph
//! break someone added or removed. Its content is the break, not the words
//! of the paragraph that ends there, so another editor changing those words
//! leaves the mark revision as it was.

use std::io::Write;
use stemma::ExportOptions;

use stemma::api::Document;

fn docx(body: &str) -> Vec<u8> {
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let document_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
    let mut bytes = Vec::new();
    {
        use zip::write::FileOptions;
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
        let options: FileOptions = FileOptions::default();
        for (name, data) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", root_rels),
            ("word/_rels/document.xml.rels", document_rels),
            ("word/document.xml", document.as_str()),
        ] {
            zip.start_file(name, options).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    bytes
}

/// P6: a paragraph whose mark is another author's pending insertion (Word
/// splits a paragraph with tracking on: the new break is `w:ins` in the
/// mark's `w:rPr`, §17.13.5.16). The mark revision is the break itself;
/// typing words elsewhere in the paragraph does not edit it.
const SPLIT_BY_ALICE: &str = concat!(
    r#"<w:p><w:pPr><w:rPr><w:ins w:id="1" w:author="Alice" w:date="2026-01-01T00:00:00Z"/></w:rPr></w:pPr>"#,
    r#"<w:r><w:t>First half of the clause.</w:t></w:r></w:p>"#,
    r#"<w:p><w:r><w:t>Second half of the clause.</w:t></w:r></w:p>"#,
);

/// The same with the break removed: Alice's pending join (`w:del` on the
/// mark).
const JOINED_BY_ALICE: &str = concat!(
    r#"<w:p><w:pPr><w:rPr><w:del w:id="1" w:author="Alice" w:date="2026-01-01T00:00:00Z"/></w:rPr></w:pPr>"#,
    r#"<w:r><w:t>First half of the clause.</w:t></w:r></w:p>"#,
    r#"<w:p><w:r><w:t>Second half of the clause.</w:t></w:r></w:p>"#,
);

#[test]
fn editing_text_beside_another_authors_mark_leaves_that_revision_untouched() {
    for body in [SPLIT_BY_ALICE, JOINED_BY_ALICE] {
        edit_beside_alices_mark(body);
    }
}

fn edit_beside_alices_mark(body: &str) {
    let original = Document::parse(&docx(body)).expect("parse");
    let before = original.serialize(&ExportOptions::default()).unwrap();
    let id = original.read().blocks[0].id.to_string();
    let json = serde_json::json!({"ops":[{"op":"replace","target":id,"expect":"First half",
        "content":{"type":"paragraph","content":[{"type":"text","text":"First part of the clause."}]}}],
        "revision":{"author":"Bob"}})
    .to_string();
    let edited = original
        .apply(
            &stemma::edit_v4::parse_transaction(&json)
                .unwrap()
                .into_edit_transaction()
                .unwrap(),
        )
        .expect("edit applies");
    let after = edited.serialize(&ExportOptions::default()).unwrap();
    let report = stemma::api::audit(&before, &after).expect("audit");
    let alice: Vec<_> = report
        .preexisting_revisions
        .iter()
        .filter(|prior| prior.record.author.as_deref() == Some("Alice"))
        .collect();
    assert_eq!(alice.len(), 1, "{report:#?}");
    assert_eq!(
        alice[0].disposition,
        stemma::audit::RevisionDisposition::Untouched,
        "Alice's paragraph break is untouched by Bob's words: {report:#?}"
    );
    assert!(
        report
            .new_revisions
            .iter()
            .all(|record| record.author.as_deref() == Some("Bob")),
        "{report:#?}"
    );
    let in_memory = edited.review().expect("review");
    assert_eq!(
        in_memory.preexisting_revisions,
        report.preexisting_revisions
    );
}
