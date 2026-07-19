//! WordprocessingML move sequences with a later independent edit.
//!
//! The model ties inline move-range carriers into one move identity while
//! keeping a later author's edit independently selectable. The two nested move
//! mixtures Word itself produces are concrete model states and remain correct
//! in either selective-resolution order.

use std::collections::HashSet;
use std::io::Write;

use stemma::api::Document;
use stemma::{ExportOptions, Resolution, ResolveSelectionAction, RevisionKind};

fn make_docx_with_body(body: &str) -> Vec<u8> {
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
        zip.start_file("[Content_Types].xml", options).unwrap();
        zip.write_all(content_types.as_bytes()).unwrap();
        zip.start_file("_rels/.rels", options).unwrap();
        zip.write_all(root_rels.as_bytes()).unwrap();
        zip.start_file("word/_rels/document.xml.rels", options)
            .unwrap();
        zip.write_all(document_rels.as_bytes()).unwrap();
        zip.start_file("word/document.xml", options).unwrap();
        zip.write_all(document.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    bytes
}

fn visible_paragraphs(document: &Document) -> Vec<String> {
    document
        .read()
        .blocks
        .iter()
        .filter(|block| !block.text.is_empty())
        .map(|block| block.text.clone())
        .collect()
}

fn revision_id(document: &Document, author: &str, kind: RevisionKind) -> u32 {
    stemma::enumerate_revisions(&document.snapshot().canonical)
        .into_iter()
        .find(|revision| revision.author.as_deref() == Some(author) && revision.kind == kind)
        .unwrap_or_else(|| panic!("missing {author} {kind:?}"))
        .revision_id
}

const MOVE_THEN_INSERT: &str = concat!(
    r#"<w:p><w:pPr><w:rPr><w:moveFrom w:id="0" w:author="Mover" w:date="2026-01-01T00:00:00Z"/></w:rPr></w:pPr>"#,
    r#"<w:moveFromRangeStart w:id="1" w:author="Mover" w:date="2026-01-01T00:00:00Z" w:name="move1"/>"#,
    r#"<w:moveFrom w:id="2" w:author="Mover" w:date="2026-01-01T00:00:00Z"><w:r><w:t>Alpha.</w:t></w:r></w:moveFrom></w:p>"#,
    r#"<w:moveFromRangeEnd w:id="1"/>"#,
    r#"<w:p><w:r><w:t>Middle.</w:t></w:r></w:p>"#,
    r#"<w:p><w:r><w:t>Omega.</w:t></w:r></w:p>"#,
    r#"<w:p><w:pPr><w:rPr><w:moveTo w:id="3" w:author="Mover" w:date="2026-01-01T00:00:00Z"/></w:rPr></w:pPr>"#,
    r#"<w:moveToRangeStart w:id="4" w:author="Mover" w:date="2026-01-01T00:00:00Z" w:name="move1"/>"#,
    r#"<w:moveTo w:id="5" w:author="Mover" w:date="2026-01-01T00:00:00Z"><w:r><w:t>Alpha.</w:t></w:r></w:moveTo>"#,
    r#"<w:ins w:id="6" w:author="Editor" w:date="2026-01-02T00:00:00Z"><w:r><w:t xml:space="preserve"> amended</w:t></w:r></w:ins></w:p>"#,
    r#"<w:moveToRangeEnd w:id="4"/>"#,
);

#[test]
fn rejecting_move_carries_later_destination_edit_back_to_source() {
    let document = Document::parse(&make_docx_with_body(MOVE_THEN_INSERT)).expect("parse");
    let move_id = revision_id(&document, "Mover", RevisionKind::Move);
    let editor_id = revision_id(&document, "Editor", RevisionKind::Insert);

    let resolved = document
        .project(Resolution::Selective {
            ids: HashSet::from([move_id]),
            action: ResolveSelectionAction::Reject,
        })
        .expect("reject move")
        .project(Resolution::Selective {
            ids: HashSet::from([editor_id]),
            action: ResolveSelectionAction::Accept,
        })
        .expect("accept later edit");

    assert_eq!(
        visible_paragraphs(&resolved),
        vec!["Alpha. amended", "Middle.", "Omega."]
    );
    assert!(stemma::enumerate_revisions(&resolved.snapshot().canonical).is_empty());

    let bytes = resolved
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let xml = String::from_utf8(
        stemma::docx::DocxArchive::read(&bytes)
            .unwrap()
            .get("word/document.xml")
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(!xml.contains("moveFrom") && !xml.contains("moveTo"));

    // A second full projection over the clean snapshot is a no-op. This pins
    // the rebuilt body template to the newly serialized package, not stale
    // pre-resolution wrappers from the previous snapshot.
    let again = resolved
        .project(Resolution::RejectAll)
        .expect("idempotent reject");
    assert_eq!(visible_paragraphs(&again), visible_paragraphs(&resolved));
    let again_bytes = again
        .serialize(&ExportOptions::default())
        .expect("serialize again");
    assert_eq!(
        visible_paragraphs(&Document::parse(&again_bytes).unwrap()),
        visible_paragraphs(&resolved)
    );
}

#[test]
fn accepting_move_keeps_later_edit_at_destination() {
    let document = Document::parse(&make_docx_with_body(MOVE_THEN_INSERT)).expect("parse");
    let move_id = revision_id(&document, "Mover", RevisionKind::Move);
    let editor_id = revision_id(&document, "Editor", RevisionKind::Insert);
    let resolved = document
        .project(Resolution::Selective {
            ids: HashSet::from([move_id]),
            action: ResolveSelectionAction::Accept,
        })
        .unwrap()
        .project(Resolution::Selective {
            ids: HashSet::from([editor_id]),
            action: ResolveSelectionAction::Reject,
        })
        .unwrap();
    assert_eq!(
        visible_paragraphs(&resolved),
        vec!["Middle.", "Omega.", "Alpha."]
    );
}

const INSERT_THEN_MOVE: &str = concat!(
    r#"<w:p><w:pPr><w:rPr><w:ins w:id="0" w:author="Origin" w:date="2026-01-01T00:00:00Z"/><w:moveFrom w:id="1" w:author="Mover" w:date="2026-01-02T00:00:00Z"/></w:rPr></w:pPr>"#,
    r#"<w:moveFromRangeStart w:id="2" w:author="Mover" w:date="2026-01-02T00:00:00Z" w:name="move2"/>"#,
    r#"<w:moveFrom w:id="3" w:author="Mover" w:date="2026-01-02T00:00:00Z"><w:ins w:id="4" w:author="Origin" w:date="2026-01-01T00:00:00Z"><w:r><w:t>Inserted.</w:t></w:r></w:ins></w:moveFrom>"#,
    r#"<w:moveFromRangeEnd w:id="2"/></w:p>"#,
    r#"<w:p><w:r><w:t>Middle.</w:t></w:r></w:p>"#,
    r#"<w:p><w:pPr><w:rPr><w:moveTo w:id="5" w:author="Mover" w:date="2026-01-02T00:00:00Z"/></w:rPr></w:pPr>"#,
    r#"<w:moveToRangeStart w:id="6" w:author="Mover" w:date="2026-01-02T00:00:00Z" w:name="move2"/>"#,
    r#"<w:moveTo w:id="7" w:author="Mover" w:date="2026-01-02T00:00:00Z"><w:r><w:t>Inserted.</w:t></w:r></w:moveTo>"#,
    r#"<w:moveToRangeEnd w:id="6"/></w:p>"#,
);

const DELETE_IN_MOVE_DESTINATION: &str = concat!(
    r#"<w:p><w:pPr><w:rPr><w:moveFrom w:id="0" w:author="Mover" w:date="2026-01-01T00:00:00Z"/></w:rPr></w:pPr>"#,
    r#"<w:moveFromRangeStart w:id="1" w:author="Mover" w:date="2026-01-01T00:00:00Z" w:name="move3"/>"#,
    r#"<w:moveFrom w:id="2" w:author="Mover" w:date="2026-01-01T00:00:00Z"><w:r><w:t>Alpha.</w:t></w:r></w:moveFrom>"#,
    r#"<w:moveFromRangeEnd w:id="1"/></w:p>"#,
    r#"<w:p><w:r><w:t>Middle.</w:t></w:r></w:p>"#,
    r#"<w:p><w:pPr><w:rPr><w:moveTo w:id="3" w:author="Mover" w:date="2026-01-01T00:00:00Z"/></w:rPr></w:pPr>"#,
    r#"<w:moveToRangeStart w:id="4" w:author="Mover" w:date="2026-01-01T00:00:00Z" w:name="move3"/>"#,
    r#"<w:moveTo w:id="5" w:author="Mover" w:date="2026-01-01T00:00:00Z"><w:r><w:t>Al</w:t></w:r><w:del w:id="6" w:author="Editor" w:date="2026-01-02T00:00:00Z"><w:r><w:delText>pha</w:delText></w:r></w:del><w:r><w:t>.</w:t></w:r></w:moveTo>"#,
    r#"<w:moveToRangeEnd w:id="4"/></w:p>"#,
);

fn resolve_author(
    document: Document,
    author: &str,
    kind: RevisionKind,
    action: ResolveSelectionAction,
) -> Document {
    let id = revision_id(&document, author, kind);
    document
        .project(Resolution::Selective {
            ids: HashSet::from([id]),
            action,
        })
        .unwrap()
}

#[test]
fn accepting_move_settles_source_only_origin_at_destination() {
    let document = Document::parse(&make_docx_with_body(INSERT_THEN_MOVE)).unwrap();
    let moved = resolve_author(
        document,
        "Mover",
        RevisionKind::Move,
        ResolveSelectionAction::Accept,
    );
    assert_eq!(visible_paragraphs(&moved), vec!["Middle.", "Inserted."]);
    assert!(stemma::enumerate_revisions(&moved.snapshot().canonical).is_empty());
}

#[test]
fn rejecting_inserted_paragraph_origin_settles_into_pending_move() {
    let document = Document::parse(&make_docx_with_body(INSERT_THEN_MOVE)).unwrap();
    let rejected = resolve_author(
        document,
        "Origin",
        RevisionKind::Insert,
        ResolveSelectionAction::Reject,
    );
    let revisions = stemma::enumerate_revisions(&rejected.snapshot().canonical);
    assert!(
        revisions
            .iter()
            .all(|revision| revision.kind == RevisionKind::Move)
    );
    let moved = resolve_author(
        rejected,
        "Mover",
        RevisionKind::Move,
        ResolveSelectionAction::Accept,
    );
    assert_eq!(visible_paragraphs(&moved), vec!["Middle.", "Inserted."]);
}

#[test]
fn accepting_origin_then_rejecting_move_restores_it_at_source() {
    let document = Document::parse(&make_docx_with_body(INSERT_THEN_MOVE)).unwrap();
    let accepted_origin = resolve_author(
        document,
        "Origin",
        RevisionKind::Insert,
        ResolveSelectionAction::Accept,
    );
    let rejected_move = resolve_author(
        accepted_origin,
        "Mover",
        RevisionKind::Move,
        ResolveSelectionAction::Reject,
    );
    assert_eq!(
        visible_paragraphs(&rejected_move),
        vec!["Inserted.", "Middle."]
    );
    assert!(stemma::enumerate_revisions(&rejected_move.snapshot().canonical).is_empty());
}

#[test]
fn reject_all_restores_inserted_then_moved_paragraph_at_source() {
    let document = Document::parse(&make_docx_with_body(INSERT_THEN_MOVE)).unwrap();
    let rejected = document.project(Resolution::RejectAll).unwrap();
    assert_eq!(visible_paragraphs(&rejected), vec!["Inserted.", "Middle."]);
    assert!(stemma::enumerate_revisions(&rejected.snapshot().canonical).is_empty());
}

#[test]
fn destination_deletion_remains_independent_after_accepting_move() {
    let document = Document::parse(&make_docx_with_body(DELETE_IN_MOVE_DESTINATION)).unwrap();
    let moved = resolve_author(
        document,
        "Mover",
        RevisionKind::Move,
        ResolveSelectionAction::Accept,
    );
    let restored = resolve_author(
        moved,
        "Editor",
        RevisionKind::Delete,
        ResolveSelectionAction::Reject,
    );
    assert_eq!(visible_paragraphs(&restored), vec!["Middle.", "Alpha."]);
}

#[test]
fn rejecting_move_maps_its_destination_deletion_back_to_source() {
    let document = Document::parse(&make_docx_with_body(DELETE_IN_MOVE_DESTINATION)).unwrap();
    let restored = resolve_author(
        document,
        "Mover",
        RevisionKind::Move,
        ResolveSelectionAction::Reject,
    );
    let accepted_deletion = resolve_author(
        restored,
        "Editor",
        RevisionKind::Delete,
        ResolveSelectionAction::Accept,
    );
    assert_eq!(
        visible_paragraphs(&accepted_deletion),
        vec!["Al.", "Middle."]
    );

    let document = Document::parse(&make_docx_with_body(DELETE_IN_MOVE_DESTINATION)).unwrap();
    let restored = resolve_author(
        document,
        "Mover",
        RevisionKind::Move,
        ResolveSelectionAction::Reject,
    );
    let rejected_deletion = resolve_author(
        restored,
        "Editor",
        RevisionKind::Delete,
        ResolveSelectionAction::Reject,
    );
    assert_eq!(
        visible_paragraphs(&rejected_deletion),
        vec!["Alpha.", "Middle."]
    );

    let document = Document::parse(&make_docx_with_body(DELETE_IN_MOVE_DESTINATION)).unwrap();
    let accepted_deletion = resolve_author(
        document,
        "Editor",
        RevisionKind::Delete,
        ResolveSelectionAction::Accept,
    );
    let restored = resolve_author(
        accepted_deletion,
        "Mover",
        RevisionKind::Move,
        ResolveSelectionAction::Reject,
    );
    assert_eq!(visible_paragraphs(&restored), vec!["Al.", "Middle."]);
}

/// LibreOffice's emission of the same move: the range-start marker is nested
/// INSIDE the run-level moveFrom/moveTo container as its first child (Word
/// writes it as a direct paragraph child), the range-end is a paragraph-level
/// sibling after the container, and there is no paragraph-mark move marker.
/// The pairing key is the marker's `w:name` either way (§17.13.5.24/.26), so
/// the importer must derive the same single atomic move identity.
const LO_NESTED_RANGE_MOVE: &str = concat!(
    r#"<w:p><w:moveFrom w:id="0" w:author="Mover" w:date="2026-01-01T00:00:00Z">"#,
    r#"<w:moveFromRangeStart w:id="0" w:author="Mover" w:date="2026-01-01T00:00:00Z" w:name="move1"/>"#,
    r#"<w:bookmarkStart w:id="1" w:name="LoMoveBookmark"/>"#,
    r#"<w:r><w:t>Alpha.</w:t></w:r></w:moveFrom>"#,
    r#"<w:moveFromRangeEnd w:id="0"/><w:bookmarkEnd w:id="1"/></w:p>"#,
    r#"<w:p><w:r><w:t>Middle.</w:t></w:r></w:p>"#,
    r#"<w:p><w:r><w:t>Omega.</w:t></w:r></w:p>"#,
    r#"<w:p><w:moveTo w:id="2" w:author="Mover" w:date="2026-01-01T00:00:00Z">"#,
    r#"<w:moveToRangeStart w:id="2" w:author="Mover" w:date="2026-01-01T00:00:00Z" w:name="move1"/>"#,
    r#"<w:r><w:t>Alpha.</w:t></w:r></w:moveTo>"#,
    r#"<w:moveToRangeEnd w:id="2"/></w:p>"#,
);

#[test]
fn lo_nested_range_move_derives_one_atomic_move_identity() {
    let document = Document::parse(&make_docx_with_body(LO_NESTED_RANGE_MOVE)).expect("parse");
    let records = stemma::enumerate_revisions(&document.snapshot().canonical);
    let moves: Vec<_> = records
        .iter()
        .filter(|r| r.kind == RevisionKind::Move)
        .collect();
    assert_eq!(
        moves.len(),
        1,
        "paired moveFrom/moveTo carriers with one w:name are ONE atomic move: {records:?}"
    );
    assert!(
        records
            .iter()
            .all(|r| !matches!(r.kind, RevisionKind::Insert | RevisionKind::Delete)),
        "no move carrier may degrade to an independently-tearable ins/del: {records:?}"
    );
}

#[test]
fn lo_nested_range_move_resolves_atomically_in_both_directions() {
    let document = Document::parse(&make_docx_with_body(LO_NESTED_RANGE_MOVE)).expect("parse");
    let move_id = revision_id(&document, "Mover", RevisionKind::Move);

    let rejected = document
        .project(Resolution::Selective {
            ids: HashSet::from([move_id]),
            action: ResolveSelectionAction::Reject,
        })
        .expect("reject move by id");
    assert_eq!(
        visible_paragraphs(&rejected),
        vec!["Alpha.", "Middle.", "Omega."],
        "rejecting the move restores the origin"
    );
    assert!(stemma::enumerate_revisions(&rejected.snapshot().canonical).is_empty());

    let accepted = document
        .project(Resolution::Selective {
            ids: HashSet::from([move_id]),
            action: ResolveSelectionAction::Accept,
        })
        .expect("accept move by id");
    assert_eq!(
        visible_paragraphs(&accepted),
        vec!["Middle.", "Omega.", "Alpha."],
        "accepting the move keeps the destination"
    );
    assert!(stemma::enumerate_revisions(&accepted.snapshot().canonical).is_empty());
}
