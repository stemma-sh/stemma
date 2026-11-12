//! A pending deleted mark left on the body's final paragraph by a selective
//! resolution (Word's final-mark ruling).
//!
//! Word, resolving everything or one revision at a time alike:
//! - a pending deleted mark on the final paragraph is a no-op on Reject and
//!   on Accept: Accept leaves the paragraph as it is, with no join and no
//!   empty paragraph added after it;
//! - accepting the deletion of a whole final paragraph (its text and mark)
//!   removes it, and the previous paragraph's mark becomes final, again
//!   with no empty paragraph added;
//! - the same for an inserted final mark: a no-op on Accept and on Reject,
//!   and rejecting a whole inserted final paragraph removes it with no empty
//!   paragraph added. A tracked final mark never makes an empty tail.

use std::collections::HashSet;
use std::io::Write;

use stemma::api::Document;
use stemma::{ExportOptions, Resolution, ResolveSelectionAction};

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

/// Alice deleted the mark of "Middle." (a pending join); Bob then deleted
/// the whole last paragraph.
const JOIN_THEN_LAST_PARAGRAPH_DELETED: &str = concat!(
    r#"<w:p><w:r><w:t>Opening.</w:t></w:r></w:p>"#,
    r#"<w:p><w:pPr><w:rPr><w:del w:id="1" w:author="Alice" w:date="2026-01-01T00:00:00Z"/></w:rPr></w:pPr><w:r><w:t>Middle.</w:t></w:r></w:p>"#,
    r#"<w:p><w:pPr><w:rPr><w:del w:id="2" w:author="Bob" w:date="2026-01-02T00:00:00Z"/></w:rPr></w:pPr>"#,
    r#"<w:del w:id="3" w:author="Bob" w:date="2026-01-02T00:00:00Z"><w:r><w:delText>Closing.</w:delText></w:r></w:del></w:p>"#,
);

fn paragraphs(doc: &Document) -> Vec<String> {
    doc.read()
        .blocks
        .iter()
        .map(|block| block.text.clone())
        .collect()
}

fn ids_of(doc: &Document, author: &str) -> HashSet<u32> {
    stemma::enumerate_revisions(&doc.snapshot().canonical)
        .into_iter()
        .filter(|r| r.author.as_deref() == Some(author))
        .map(|r| r.revision_id)
        .collect()
}

#[test]
fn a_deleted_mark_left_final_by_a_resolution_adds_no_empty_paragraph() {
    let original = Document::parse(&docx(JOIN_THEN_LAST_PARAGRAPH_DELETED)).expect("parse");
    // Accepting Bob's deletion removes "Closing."; "Middle." is final and its
    // pending deleted mark has nothing to join.
    let bob_accepted = original
        .project(Resolution::Selective {
            ids: ids_of(&original, "Bob"),
            action: ResolveSelectionAction::Accept,
        })
        .expect("accept Bob");
    let reopened =
        Document::parse(&bob_accepted.serialize(&ExportOptions::default()).unwrap()).unwrap();
    for (label, doc) in [("in memory", &bob_accepted), ("reopened", &reopened)] {
        assert_eq!(paragraphs(doc), vec!["Opening.", "Middle."], "{label}");
        for (name, resolution) in [
            ("Accept All", Resolution::AcceptAll),
            ("Reject All", Resolution::RejectAll),
        ] {
            let resolved = doc.project(resolution).expect("resolve");
            assert_eq!(
                paragraphs(&resolved),
                vec!["Opening.", "Middle."],
                "{label} {name}: no join, no empty paragraph after it"
            );
        }
    }
    // Resolving everything at once gives the same document.
    assert_eq!(
        paragraphs(&original.project(Resolution::AcceptAll).unwrap()),
        vec!["Opening.", "Middle."]
    );
    assert_eq!(
        paragraphs(&original.project(Resolution::RejectAll).unwrap()),
        vec!["Opening.", "Middle.", "Closing."]
    );
}

/// Alice split "Middle." from the paragraph after it (her inserted mark);
/// Bob then added the whole last paragraph.
const SPLIT_THEN_LAST_PARAGRAPH_INSERTED: &str = concat!(
    r#"<w:p><w:r><w:t>Opening.</w:t></w:r></w:p>"#,
    r#"<w:p><w:pPr><w:rPr><w:ins w:id="1" w:author="Alice" w:date="2026-01-01T00:00:00Z"/></w:rPr></w:pPr><w:r><w:t>Middle.</w:t></w:r></w:p>"#,
    r#"<w:p><w:pPr><w:rPr><w:ins w:id="2" w:author="Bob" w:date="2026-01-02T00:00:00Z"/></w:rPr></w:pPr>"#,
    r#"<w:ins w:id="3" w:author="Bob" w:date="2026-01-02T00:00:00Z"><w:r><w:t>Closing.</w:t></w:r></w:ins></w:p>"#,
);

#[test]
fn an_inserted_mark_left_final_by_a_resolution_adds_no_empty_paragraph() {
    let original = Document::parse(&docx(SPLIT_THEN_LAST_PARAGRAPH_INSERTED)).expect("parse");
    // Rejecting Bob's paragraph removes it; "Middle." is final and its
    // pending inserted mark has nothing to split off.
    let bob_rejected = original
        .project(Resolution::Selective {
            ids: ids_of(&original, "Bob"),
            action: ResolveSelectionAction::Reject,
        })
        .expect("reject Bob");
    let reopened =
        Document::parse(&bob_rejected.serialize(&ExportOptions::default()).unwrap()).unwrap();
    for (label, doc) in [("in memory", &bob_rejected), ("reopened", &reopened)] {
        assert_eq!(paragraphs(doc), vec!["Opening.", "Middle."], "{label}");
        for (name, resolution) in [
            ("Accept All", Resolution::AcceptAll),
            ("Reject All", Resolution::RejectAll),
        ] {
            let resolved = doc.project(resolution).expect("resolve");
            assert_eq!(
                paragraphs(&resolved),
                vec!["Opening.", "Middle."],
                "{label} {name}: no empty paragraph after it"
            );
        }
    }
    assert_eq!(
        paragraphs(&original.project(Resolution::RejectAll).unwrap()),
        vec!["Opening.", "Middle."]
    );
    assert_eq!(
        paragraphs(&original.project(Resolution::AcceptAll).unwrap()),
        vec!["Opening.", "Middle.", "Closing."]
    );
}

// ─── The settled mark is reported, never silent ──────────────────────────────
//
// Every revision a resolution removes is either selected or reported as
// cascaded. Settling the stranded final mark removes the pending revision it
// carried (Alice's), which the caller did not select: the receipt names it.

fn resolve_through_runtime(
    body: &str,
    author: &str,
    action: ResolveSelectionAction,
) -> (Vec<u32>, HashSet<u32>, HashSet<u32>) {
    use stemma::{DocxRuntime, SimpleRuntime};
    let bytes = docx(body);
    let document = Document::parse(&bytes).expect("parse");
    let selected = ids_of(&document, author);
    let alice = ids_of(&document, "Alice");
    let runtime = SimpleRuntime::new();
    let import = runtime.import_docx(&bytes).expect("import");
    let result = runtime
        .resolve_tracked_revisions(&import.doc_handle, &selected, action)
        .expect("resolve");
    let remaining: HashSet<u32> = stemma::enumerate_revisions(&result.canonical)
        .into_iter()
        .map(|r| r.revision_id)
        .collect();
    (result.cascaded_revision_ids, alice, remaining)
}

#[test]
fn accepting_the_last_paragraphs_deletion_reports_the_settled_deleted_mark() {
    let (cascaded, alice, remaining) = resolve_through_runtime(
        JOIN_THEN_LAST_PARAGRAPH_DELETED,
        "Bob",
        ResolveSelectionAction::Accept,
    );
    assert_eq!(alice.len(), 1, "Alice has one revision: {alice:?}");
    assert!(
        alice.is_disjoint(&remaining),
        "the resolution removes Alice's mark: {remaining:?}"
    );
    assert_eq!(
        cascaded,
        alice.into_iter().collect::<Vec<_>>(),
        "the receipt names the mark the settle removed"
    );
}

#[test]
fn rejecting_the_last_paragraphs_insertion_reports_the_settled_inserted_mark() {
    let (cascaded, alice, remaining) = resolve_through_runtime(
        SPLIT_THEN_LAST_PARAGRAPH_INSERTED,
        "Bob",
        ResolveSelectionAction::Reject,
    );
    assert_eq!(alice.len(), 1, "Alice has one revision: {alice:?}");
    assert!(
        alice.is_disjoint(&remaining),
        "the resolution removes Alice's mark: {remaining:?}"
    );
    assert_eq!(
        cascaded,
        alice.into_iter().collect::<Vec<_>>(),
        "the receipt names the mark the settle removed"
    );
}

/// A resolution that leaves the pending mark where it was (not final)
/// removes nothing unselected and reports no cascade.
#[test]
fn a_mark_that_stays_non_final_is_not_reported() {
    let (cascaded, alice, remaining) = resolve_through_runtime(
        JOIN_THEN_LAST_PARAGRAPH_DELETED,
        "Bob",
        ResolveSelectionAction::Reject,
    );
    assert!(alice.is_subset(&remaining), "{remaining:?}");
    assert!(cascaded.is_empty(), "{cascaded:?}");
}
