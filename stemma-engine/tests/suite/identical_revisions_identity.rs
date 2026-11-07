//! Identical changes made one after the other get distinct identities.
//!
//! Domain rule: every pending revision has an identity of its own, stable
//! across save and reopen, so it can be resolved alone. A reviewer who
//! changes "30 days" to "45 days" in clause 7 and then, in a second edit, in
//! clause 3 makes two pairs of revisions that look alike (same author, date
//! and text) and sit in different places; the second edit must not be
//! refused because the first already holds the identity its content would
//! derive, and each pair must stay resolvable on its own after reopening.

use std::collections::HashSet;

use stemma::ExportOptions;
use stemma::api::{Document, Resolution};
use stemma::tracked_model::ResolveSelectionAction;

fn docx() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Clause 3: payment within 30 days.</w:t></w:r></w:p><w:p><w:r><w:t>Clause 7: refund within 30 days.</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    use std::io::Write;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions = zip::write::FileOptions::default();
        for (name, data) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/document.xml", document_xml),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

fn change(doc: &Document, target: &str, text: &str) -> Document {
    let json = serde_json::json!({"ops":[{"op":"replace","target":target,"expect":"30 days",
        "content":{"type":"paragraph","content":[{"type":"text","text":text}]}}],
        "revision":{"author":"Reviewer","date":"2026-11-06T09:00:00Z"}})
    .to_string();
    doc.apply(
        &stemma::edit_v4::parse_transaction(&json)
            .unwrap()
            .into_edit_transaction()
            .unwrap(),
    )
    .unwrap_or_else(|e| panic!("the change to {target} applies: {e}"))
}

fn identities(doc: &Document) -> Vec<u32> {
    let mut ids: Vec<u32> = doc.revisions().iter().map(|r| r.revision_id).collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

#[test]
fn a_later_identical_change_earlier_in_the_document_gets_its_own_identity() {
    let doc = Document::parse(&docx()).expect("parse");
    let clause7 = change(&doc, "p_2", "Clause 7: refund within 45 days.");
    let both = change(&clause7, "p_1", "Clause 3: payment within 45 days.");
    assert_eq!(
        both.read_accepted().unwrap().to_text(),
        "Clause 3: payment within 45 days.\n\nClause 7: refund within 45 days."
    );

    let ids = identities(&both);
    let in_clause = |block: &str| -> HashSet<u32> {
        both.revisions()
            .iter()
            .filter(|r| r.block_id.to_string() == block)
            .map(|r| r.revision_id)
            .collect()
    };
    let (clause3_ids, clause7_ids) = (in_clause("p_1"), in_clause("p_2"));
    assert!(
        clause3_ids.is_disjoint(&clause7_ids),
        "the two clauses' revisions are told apart: {clause3_ids:?} {clause7_ids:?}"
    );

    let reopened =
        Document::parse(&both.serialize(&ExportOptions::default()).unwrap()).expect("reopen");
    assert_eq!(
        identities(&reopened),
        ids,
        "the identities survive save and reopen"
    );

    let only_clause7 = reopened
        .project(Resolution::Selective {
            ids: clause7_ids,
            action: ResolveSelectionAction::Accept,
        })
        .expect("accept clause 7 alone");
    let rejected = only_clause7.read_rejected().unwrap().to_text();
    assert_eq!(
        rejected, "Clause 3: payment within 30 days.\n\nClause 7: refund within 45 days.",
        "accepting clause 7's revisions resolves clause 7 only"
    );
}
