//! Edits written next to another author's tracked move.
//!
//! ECMA-376 §17.13.5.22/24: `w:moveFrom`/`w:moveTo` wrap the runs one move
//! revision relocated. A run a later editor types *beside* the source is not
//! part of that move: it is that editor's own `w:ins`, a sibling of the
//! `w:moveFrom`. Word's Accept All keeps it, Reject All drops it, and
//! rejecting the editor's own revision removes it while the move stays
//! pending.

use std::collections::HashSet;
use std::io::Write;

use stemma::api::Document;
use stemma::{ExportOptions, Resolution, ResolveSelectionAction, RevisionKind};

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

/// One paragraph, an inline move from "Front." to the tail.
const INLINE_MOVE: &str = concat!(
    r#"<w:p><w:r><w:t xml:space="preserve">Front. </w:t></w:r>"#,
    r#"<w:moveFromRangeStart w:id="10" w:author="Mover" w:date="2026-01-01T00:00:00Z" w:name="move1"/>"#,
    r#"<w:moveFrom w:id="11" w:author="Mover" w:date="2026-01-01T00:00:00Z"><w:r><w:t>travelling words</w:t></w:r></w:moveFrom>"#,
    r#"<w:moveFromRangeEnd w:id="10"/>"#,
    r#"<w:r><w:t xml:space="preserve"> Centre. </w:t></w:r>"#,
    r#"<w:moveToRangeStart w:id="12" w:author="Mover" w:date="2026-01-01T00:00:00Z" w:name="move1"/>"#,
    r#"<w:moveTo w:id="13" w:author="Mover" w:date="2026-01-01T00:00:00Z"><w:r><w:t>travelling words</w:t></w:r></w:moveTo>"#,
    r#"<w:moveToRangeEnd w:id="12"/>"#,
    r#"<w:r><w:t xml:space="preserve"> Back.</w:t></w:r></w:p>"#,
);

fn replace_first_paragraph(doc: &Document, author: &str, text: &str) -> Document {
    let id = doc.read().blocks[0].id.to_string();
    let json = serde_json::json!({"ops":[{"op":"replace","target":id,"expect":"Front.",
        "content":{"type":"paragraph","content":[{"type":"text","text":text}]}}],
        "revision":{"author":author}})
    .to_string();
    doc.apply(
        &stemma::edit_v4::parse_transaction(&json)
            .unwrap()
            .into_edit_transaction()
            .unwrap(),
    )
    .expect("replace applies")
}

fn first_text(doc: &Document) -> String {
    doc.read().blocks[0].text.clone()
}

fn reopen(doc: &Document) -> Document {
    Document::parse(&doc.serialize(&ExportOptions::default()).expect("serialize")).expect("reopen")
}

fn id_of(doc: &Document, author: &str, kind: RevisionKind) -> u32 {
    let revisions = stemma::enumerate_revisions(&doc.snapshot().canonical);
    revisions
        .iter()
        .find(|r| r.author.as_deref() == Some(author) && r.kind == kind)
        .unwrap_or_else(|| panic!("no {author} {kind:?} in {revisions:?}"))
        .revision_id
}

/// The original's readings: the move rejected, and the move accepted.
const REJECTED: &str = "Front. travelling words Centre.  Back.";
const ACCEPTED: &str = "Front.  Centre. travelling words Back.";

fn document_xml(doc: &Document) -> String {
    String::from_utf8(
        stemma::docx::DocxArchive::read(&doc.serialize(&ExportOptions::default()).unwrap())
            .unwrap()
            .get("word/document.xml")
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

/// The Editor's text `added` is the Editor's own insertion beside the
/// Mover's move, in memory and after save and reopen: Accept All keeps it,
/// Reject All drops it, rejecting it alone leaves the move pending with its
/// id, and it is never written inside the move's containers.
fn assert_own_insertion_beside_move(edited: &Document, added: &str, accepted: &str) {
    let reopened = reopen(edited);
    for (label, doc) in [("in memory", edited), ("reopened", &reopened)] {
        let all_accepted = doc.project(Resolution::AcceptAll).expect("accept all");
        assert_eq!(
            first_text(&all_accepted),
            accepted,
            "{label}: Accept All keeps the new text"
        );
        let all_rejected = doc.project(Resolution::RejectAll).expect("reject all");
        assert_eq!(
            first_text(&all_rejected),
            REJECTED,
            "{label}: Reject All drops the new text and restores the move source"
        );
        let editor = id_of(doc, "Editor", RevisionKind::Insert);
        let move_id = id_of(doc, "Mover", RevisionKind::Move);
        let own_rejected = doc
            .project(Resolution::Selective {
                ids: HashSet::from([editor]),
                action: ResolveSelectionAction::Reject,
            })
            .expect("reject own insertion");
        let remaining = stemma::enumerate_revisions(&own_rejected.snapshot().canonical);
        assert!(
            remaining
                .iter()
                .all(|r| r.author.as_deref() == Some("Mover"))
                && remaining.iter().any(|r| r.revision_id == move_id),
            "{label}: only the move remains, with its id {move_id}: {remaining:?}"
        );
        assert_eq!(
            first_text(&own_rejected.project(Resolution::AcceptAll).unwrap()),
            ACCEPTED,
            "{label}: the rejected insertion is gone from the accepted reading"
        );
    }
    let xml = document_xml(edited);
    for container in ["moveFrom", "moveTo"] {
        let start = xml
            .find(&format!("<w:{container} "))
            .unwrap_or_else(|| panic!("{container} in {xml}"));
        let end = start + xml[start..].find(&format!("</w:{container}>")).unwrap();
        assert!(
            !xml[start..end].contains(added),
            "the insertion is not written inside the Mover's {container}: {xml}"
        );
    }
}

/// P1: text typed directly before the move's source is the editor's own
/// insertion, not part of the move.
#[test]
fn insertion_right_before_a_move_source_is_the_editors_own_revision() {
    let original = Document::parse(&docx(INLINE_MOVE)).expect("parse");
    let text = "Front. NEW  Centre. travelling words Back.";
    let edited = replace_first_paragraph(&original, "Editor", text);
    assert_own_insertion_beside_move(&edited, "NEW", text);
}

/// The same position through the span splice (`replace_text`).
#[test]
fn span_insertion_right_before_a_move_source_is_the_editors_own_revision() {
    let original = Document::parse(&docx(INLINE_MOVE)).expect("parse");
    let canonical = original.snapshot().canonical.as_ref().clone();
    let plan = stemma::edit::plan_replace_text(
        &canonical,
        &stemma::edit::ReplaceTextOptions {
            old: "Front.".to_string(),
            new: "Front. NEW".to_string(),
            author: "Editor".to_string(),
            scope: stemma::edit::ReplaceTextScope::WholeDoc,
            expected: stemma::edit::ExpectedMatches::Count(1),
            match_mode: stemma::edit::MatchMode::Exact,
            on_barrier_match: stemma::edit::BarrierPolicy::Skip,
        },
    )
    .expect("plan");
    let edited = original
        .apply(&stemma::edit::EditTransaction {
            steps: plan.steps,
            summary: None,
            materialization_mode: stemma::edit::MaterializationMode::TrackedChange,
            revision: stemma::RevisionInfo {
                revision_id: 0,
                identity: 0,
                author: Some("Editor".to_string()),
                date: None,
                apply_op_id: None,
            },
        })
        .expect("replace_text applies");
    assert_own_insertion_beside_move(&edited, "NEW", "Front. NEW  Centre. travelling words Back.");
}

/// The neighbouring positions, which 0.6.0 and 0.7.0 both kept outside the
/// move: after the source, and either side of the destination.
#[test]
fn insertions_at_the_other_move_edges_are_the_editors_own_revisions() {
    for text in [
        "Front.  NEW Centre. travelling words Back.",
        "Front.  Centre. NEW travelling words Back.",
        "Front.  Centre. travelling words NEW Back.",
    ] {
        let original = Document::parse(&docx(INLINE_MOVE)).expect("parse");
        let edited = replace_first_paragraph(&original, "Editor", text);
        assert_own_insertion_beside_move(&edited, "NEW", text);
    }
}
