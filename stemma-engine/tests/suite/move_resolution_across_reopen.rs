//! Moves after a partial resolution, across save and reopen.
//!
//! A tracked move (§17.13.5.22–28) is one revision with one identity. A
//! selective resolution of another revision beside or inside it leaves the
//! move pending; saving and reopening that state must give back the same
//! move: the same id, the same readings, the same spelling of every mark.

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

fn resolve(doc: &Document, id: u32, action: ResolveSelectionAction) -> Document {
    doc.project(Resolution::Selective {
        ids: HashSet::from([id]),
        action,
    })
    .expect("selective resolution")
}

fn texts(doc: &Document) -> Vec<String> {
    doc.read()
        .blocks
        .iter()
        .map(|block| block.text.clone())
        .filter(|text| !text.is_empty())
        .collect()
}

/// A whole-paragraph move whose destination an Editor then edited by
/// deleting "pha" (`w:del` inside the `w:moveTo`).
const DELETION_IN_MOVED_PARAGRAPH: &str = concat!(
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

/// Accepting the Editor's deletion settles it in both copies of the moved
/// text; the move itself is untouched, so it keeps its id in memory and
/// after save and reopen, and either resolution of it still works there.
#[test]
fn accepting_a_destination_deletion_keeps_the_moves_id_across_reopen() {
    let original = Document::parse(&docx(DELETION_IN_MOVED_PARAGRAPH)).expect("parse");
    let move_id = id_of(&original, "Mover", RevisionKind::Move);
    let accepted = resolve(
        &original,
        id_of(&original, "Editor", RevisionKind::Delete),
        ResolveSelectionAction::Accept,
    );
    let reopened = reopen(&accepted);
    for (label, doc) in [("in memory", &accepted), ("reopened", &reopened)] {
        let revisions = stemma::enumerate_revisions(&doc.snapshot().canonical);
        assert_eq!(
            revisions
                .iter()
                .map(|r| (r.revision_id, r.kind, r.excerpt.as_str()))
                .collect::<Vec<_>>(),
            vec![(move_id, RevisionKind::Move, "moved: Al.")],
            "{label}: one pending move, same id, holding the settled text"
        );
        assert_eq!(
            texts(&resolve(doc, move_id, ResolveSelectionAction::Accept)),
            vec!["Middle.", "Al."],
            "{label}"
        );
        assert_eq!(
            texts(&resolve(doc, move_id, ResolveSelectionAction::Reject)),
            vec!["Al.", "Middle."],
            "{label}"
        );
    }
}

/// The document's last paragraph moved to the top. Word cannot track the
/// document-final paragraph mark (§17.13.5.15: the final mark is never
/// deleted), so the source paragraph's mark carries no `w:moveFrom`; only its
/// runs and the move range do.
const FINAL_PARAGRAPH_MOVED_UP: &str = concat!(
    r#"<w:moveToRangeStart w:id="1" w:author="Mover" w:date="2026-01-01T00:00:00Z" w:name="move1"/>"#,
    r#"<w:p><w:pPr><w:rPr><w:moveTo w:id="2" w:author="Mover" w:date="2026-01-01T00:00:00Z"/></w:rPr></w:pPr>"#,
    r#"<w:moveTo w:id="3" w:author="Mover" w:date="2026-01-01T00:00:00Z"><w:r><w:t>Closing words.</w:t></w:r></w:moveTo></w:p>"#,
    r#"<w:moveToRangeEnd w:id="1"/>"#,
    r#"<w:p><w:r><w:t xml:space="preserve">Opening words </w:t></w:r><w:ins w:id="9" w:author="Editor" w:date="2026-01-02T00:00:00Z"><w:r><w:t>here</w:t></w:r></w:ins></w:p>"#,
    r#"<w:moveFromRangeStart w:id="4" w:author="Mover" w:date="2026-01-01T00:00:00Z" w:name="move1"/>"#,
    r#"<w:p><w:moveFrom w:id="5" w:author="Mover" w:date="2026-01-01T00:00:00Z"><w:r><w:t>Closing words.</w:t></w:r></w:moveFrom></w:p>"#,
    r#"<w:moveFromRangeEnd w:id="4"/>"#,
);

/// The final paragraph's mark, as the model spells it.
fn final_mark(doc: &Document) -> Option<stemma::TrackingStatus> {
    let blocks = &doc.snapshot().canonical.blocks;
    let last =
        stemma::tracked_model::document_final_paragraph_index(blocks).expect("a final paragraph");
    match &blocks[last].block {
        stemma::BlockNode::Paragraph(paragraph) => paragraph.para_mark_status.clone(),
        _ => unreachable!(),
    }
}

/// The untracked final mark of a moved-from paragraph reads the same on
/// import, after resolving an unrelated revision, and after reopening that:
/// never a pending deletion, which the final mark cannot carry.
#[test]
fn a_moved_final_paragraphs_mark_has_one_spelling() {
    let original = Document::parse(&docx(FINAL_PARAGRAPH_MOVED_UP)).expect("parse");
    let editor = id_of(&original, "Editor", RevisionKind::Insert);
    let resolved = resolve(&original, editor, ResolveSelectionAction::Reject);
    let reopened = reopen(&resolved);
    let spellings = [
        ("imported", final_mark(&original)),
        ("resolved", final_mark(&resolved)),
        ("reopened", final_mark(&reopened)),
    ];
    for (label, mark) in &spellings {
        assert_eq!(
            mark.as_ref(),
            Some(&stemma::TrackingStatus::Normal),
            "{label}: the final mark is untracked: {spellings:?}"
        );
    }
    for (label, doc) in [("resolved", &resolved), ("reopened", &reopened)] {
        let move_id = id_of(doc, "Mover", RevisionKind::Move);
        assert_eq!(
            texts(&resolve(doc, move_id, ResolveSelectionAction::Accept)),
            vec!["Closing words.", "Opening words "],
            "{label}"
        );
        assert_eq!(
            texts(&resolve(doc, move_id, ResolveSelectionAction::Reject)),
            vec!["Opening words ", "Closing words."],
            "{label}"
        );
    }
}

/// Origin inserted "beta" into a paragraph; Mover then moved the whole
/// paragraph. Word writes the insertion inside the source's `w:moveFrom`
/// (text inserted, then moved away) and plain inside the `w:moveTo`.
const INSERTION_THEN_PARAGRAPH_MOVED: &str = concat!(
    r#"<w:p><w:pPr><w:rPr><w:moveFrom w:id="0" w:author="Mover" w:date="2026-01-02T00:00:00Z"/></w:rPr></w:pPr>"#,
    r#"<w:moveFromRangeStart w:id="1" w:author="Mover" w:date="2026-01-02T00:00:00Z" w:name="move1"/>"#,
    r#"<w:moveFrom w:id="2" w:author="Mover" w:date="2026-01-02T00:00:00Z"><w:r><w:t xml:space="preserve">Alpha </w:t></w:r>"#,
    r#"<w:ins w:id="3" w:author="Origin" w:date="2026-01-01T00:00:00Z"><w:r><w:t>beta</w:t></w:r></w:ins></w:moveFrom>"#,
    r#"<w:moveFromRangeEnd w:id="1"/></w:p>"#,
    r#"<w:p><w:r><w:t>Middle.</w:t></w:r></w:p>"#,
    r#"<w:p><w:pPr><w:rPr><w:moveTo w:id="4" w:author="Mover" w:date="2026-01-02T00:00:00Z"/></w:rPr></w:pPr>"#,
    r#"<w:moveToRangeStart w:id="5" w:author="Mover" w:date="2026-01-02T00:00:00Z" w:name="move1"/>"#,
    r#"<w:moveTo w:id="6" w:author="Mover" w:date="2026-01-02T00:00:00Z"><w:r><w:t>Alpha beta</w:t></w:r></w:moveTo>"#,
    r#"<w:moveToRangeEnd w:id="5"/></w:p>"#,
);

/// Accepting Origin's insertion settles it into the pending move, as it
/// already does when the move is of runs: the inserted words are moved text
/// now, so accepting the move takes them to the destination and rejecting
/// it keeps them at the source. In memory and after reopen alike.
#[test]
fn accepting_an_insertion_inside_a_moved_paragraph_settles_it_into_the_move() {
    let original = Document::parse(&docx(INSERTION_THEN_PARAGRAPH_MOVED)).expect("parse");
    let origin = id_of(&original, "Origin", RevisionKind::Insert);
    let move_id = id_of(&original, "Mover", RevisionKind::Move);
    let accepted = resolve(&original, origin, ResolveSelectionAction::Accept);
    let reopened = reopen(&accepted);
    for (label, doc) in [("in memory", &accepted), ("reopened", &reopened)] {
        let revisions = stemma::enumerate_revisions(&doc.snapshot().canonical);
        assert!(
            revisions.len() == 1
                && revisions[0].kind == RevisionKind::Move
                && revisions[0].revision_id == move_id
                && revisions[0].excerpt == "moved: Alpha beta",
            "{label}: only the move is pending, with its id {move_id}: {revisions:?}"
        );
        assert_eq!(
            texts(&doc.project(Resolution::AcceptAll).unwrap()),
            vec!["Middle.", "Alpha beta"],
            "{label}: accepting the move leaves no copy of the words at the source"
        );
        assert_eq!(
            texts(&doc.project(Resolution::RejectAll).unwrap()),
            vec!["Alpha beta", "Middle."],
            "{label}"
        );
    }
}

/// The same moved paragraph, after which an Editor deleted "pha " at the
/// destination (`w:del` inside the `w:moveTo`).
const INSERTION_MOVE_THEN_DESTINATION_DELETION: &str = concat!(
    r#"<w:p><w:pPr><w:rPr><w:moveFrom w:id="0" w:author="Mover" w:date="2026-01-02T00:00:00Z"/></w:rPr></w:pPr>"#,
    r#"<w:moveFromRangeStart w:id="1" w:author="Mover" w:date="2026-01-02T00:00:00Z" w:name="move1"/>"#,
    r#"<w:moveFrom w:id="2" w:author="Mover" w:date="2026-01-02T00:00:00Z"><w:r><w:t xml:space="preserve">Alpha </w:t></w:r>"#,
    r#"<w:ins w:id="3" w:author="Origin" w:date="2026-01-01T00:00:00Z"><w:r><w:t>beta</w:t></w:r></w:ins></w:moveFrom>"#,
    r#"<w:moveFromRangeEnd w:id="1"/></w:p>"#,
    r#"<w:p><w:r><w:t>Middle.</w:t></w:r></w:p>"#,
    r#"<w:p><w:pPr><w:rPr><w:moveTo w:id="4" w:author="Mover" w:date="2026-01-02T00:00:00Z"/></w:rPr></w:pPr>"#,
    r#"<w:moveToRangeStart w:id="5" w:author="Mover" w:date="2026-01-02T00:00:00Z" w:name="move1"/>"#,
    r#"<w:moveTo w:id="6" w:author="Mover" w:date="2026-01-02T00:00:00Z"><w:r><w:t>Al</w:t></w:r>"#,
    r#"<w:del w:id="7" w:author="Editor" w:date="2026-01-03T00:00:00Z"><w:r><w:delText xml:space="preserve">pha </w:delText></w:r></w:del>"#,
    r#"<w:r><w:t>beta</w:t></w:r></w:moveTo>"#,
    r#"<w:moveToRangeEnd w:id="5"/></w:p>"#,
);

/// Rejecting the Editor's deletion gives "pha " back to the destination as
/// moved text, part of the move's own destination run; the move keeps its
/// id through the later resolution of Origin's insertion and after reopen.
#[test]
fn rejecting_a_destination_deletion_keeps_the_moves_id_across_reopen() {
    let original = Document::parse(&docx(INSERTION_MOVE_THEN_DESTINATION_DELETION)).expect("parse");
    let move_id = id_of(&original, "Mover", RevisionKind::Move);
    let restored = resolve(
        &original,
        id_of(&original, "Editor", RevisionKind::Delete),
        ResolveSelectionAction::Reject,
    );
    let settled = resolve(
        &restored,
        id_of(&restored, "Origin", RevisionKind::Insert),
        ResolveSelectionAction::Accept,
    );
    let reopened = reopen(&settled);
    for (label, doc) in [("in memory", &settled), ("reopened", &reopened)] {
        let revisions = stemma::enumerate_revisions(&doc.snapshot().canonical);
        assert_eq!(
            revisions
                .iter()
                .map(|r| (r.revision_id, r.kind, r.excerpt.as_str()))
                .collect::<Vec<_>>(),
            vec![(move_id, RevisionKind::Move, "moved: Alpha beta")],
            "{label}"
        );
        assert_eq!(
            texts(&resolve(doc, move_id, ResolveSelectionAction::Accept)),
            vec!["Middle.", "Alpha beta"],
            "{label}"
        );
    }
}

/// Both copies of a move hold the same text, so a deletion inside the
/// destination strikes the same characters of the source: by position, not
/// by searching for its words. "pha" occurs twice in the moved paragraph;
/// the Editor deleted the second one.
const REPEATED_WORDS_DELETED_AT_DESTINATION: &str = concat!(
    r#"<w:p><w:pPr><w:rPr><w:moveFrom w:id="0" w:author="Mover" w:date="2026-01-01T00:00:00Z"/></w:rPr></w:pPr>"#,
    r#"<w:moveFromRangeStart w:id="1" w:author="Mover" w:date="2026-01-01T00:00:00Z" w:name="move1"/>"#,
    r#"<w:moveFrom w:id="2" w:author="Mover" w:date="2026-01-01T00:00:00Z"><w:r><w:t>Alpha pha.</w:t></w:r></w:moveFrom>"#,
    r#"<w:moveFromRangeEnd w:id="1"/></w:p>"#,
    r#"<w:p><w:r><w:t>Middle.</w:t></w:r></w:p>"#,
    r#"<w:p><w:pPr><w:rPr><w:moveTo w:id="3" w:author="Mover" w:date="2026-01-01T00:00:00Z"/></w:rPr></w:pPr>"#,
    r#"<w:moveToRangeStart w:id="4" w:author="Mover" w:date="2026-01-01T00:00:00Z" w:name="move1"/>"#,
    r#"<w:moveTo w:id="5" w:author="Mover" w:date="2026-01-01T00:00:00Z"><w:r><w:t xml:space="preserve">Alpha </w:t></w:r>"#,
    r#"<w:del w:id="6" w:author="Editor" w:date="2026-01-02T00:00:00Z"><w:r><w:delText>pha</w:delText></w:r></w:del>"#,
    r#"<w:r><w:t>.</w:t></w:r></w:moveTo>"#,
    r#"<w:moveToRangeEnd w:id="4"/></w:p>"#,
);

/// The destination deletion "pha be" runs across moved text and words
/// Origin inserted before the move.
const DESTINATION_DELETION_ACROSS_AN_ORIGIN_INSERTION: &str = concat!(
    r#"<w:p><w:pPr><w:rPr><w:moveFrom w:id="0" w:author="Mover" w:date="2026-01-02T00:00:00Z"/></w:rPr></w:pPr>"#,
    r#"<w:moveFromRangeStart w:id="1" w:author="Mover" w:date="2026-01-02T00:00:00Z" w:name="move1"/>"#,
    r#"<w:moveFrom w:id="2" w:author="Mover" w:date="2026-01-02T00:00:00Z"><w:r><w:t xml:space="preserve">Alpha </w:t></w:r>"#,
    r#"<w:ins w:id="3" w:author="Origin" w:date="2026-01-01T00:00:00Z"><w:r><w:t>beta</w:t></w:r></w:ins></w:moveFrom>"#,
    r#"<w:moveFromRangeEnd w:id="1"/></w:p>"#,
    r#"<w:p><w:r><w:t>Middle.</w:t></w:r></w:p>"#,
    r#"<w:p><w:pPr><w:rPr><w:moveTo w:id="4" w:author="Mover" w:date="2026-01-02T00:00:00Z"/></w:rPr></w:pPr>"#,
    r#"<w:moveToRangeStart w:id="5" w:author="Mover" w:date="2026-01-02T00:00:00Z" w:name="move1"/>"#,
    r#"<w:moveTo w:id="6" w:author="Mover" w:date="2026-01-02T00:00:00Z"><w:r><w:t>Al</w:t></w:r>"#,
    r#"<w:del w:id="7" w:author="Editor" w:date="2026-01-03T00:00:00Z"><w:r><w:delText>pha be</w:delText></w:r></w:del>"#,
    r#"<w:r><w:t>ta</w:t></w:r></w:moveTo>"#,
    r#"<w:moveToRangeEnd w:id="5"/></w:p>"#,
);

/// Every order of resolving the move and the destination deletion, in
/// memory and with a save and reopen between the two steps, lands on the
/// reading the two decisions define.
fn assert_move_and_deletion_orders(body: &str, kept_at_source: &str, deletion_accepted: &str) {
    let original = Document::parse(&docx(body)).expect("parse");
    let cases = [
        // (first, second, expected)
        (
            "Mover",
            "Editor",
            ResolveSelectionAction::Reject,
            ResolveSelectionAction::Accept,
            vec![deletion_accepted, "Middle."],
        ),
        (
            "Editor",
            "Mover",
            ResolveSelectionAction::Accept,
            ResolveSelectionAction::Reject,
            vec![deletion_accepted, "Middle."],
        ),
        (
            "Mover",
            "Editor",
            ResolveSelectionAction::Reject,
            ResolveSelectionAction::Reject,
            vec![kept_at_source, "Middle."],
        ),
        (
            "Editor",
            "Mover",
            ResolveSelectionAction::Accept,
            ResolveSelectionAction::Accept,
            vec!["Middle.", deletion_accepted],
        ),
    ];
    let kind = |author: &str| {
        if author == "Mover" {
            RevisionKind::Move
        } else {
            RevisionKind::Delete
        }
    };
    for (first, second, first_action, second_action, expected) in cases {
        let one = resolve(
            &original,
            id_of(&original, first, kind(first)),
            first_action,
        );
        let reopened = reopen(&one);
        for (label, doc) in [("in memory", &one), ("reopened", &reopened)] {
            let two = resolve(doc, id_of(doc, second, kind(second)), second_action);
            assert_eq!(
                texts(&two),
                expected,
                "{label}: {first_action:?} {first}, then {second_action:?} {second}"
            );
        }
    }
    assert_eq!(
        texts(&original.project(Resolution::AcceptAll).expect("accept all")),
        vec!["Middle.", deletion_accepted]
    );
    assert_eq!(
        texts(&original.project(Resolution::RejectAll).expect("reject all")),
        vec![kept_at_source, "Middle."]
    );
}

#[test]
fn a_destination_deletion_maps_onto_the_source_by_position() {
    assert_move_and_deletion_orders(
        REPEATED_WORDS_DELETED_AT_DESTINATION,
        "Alpha pha.",
        "Alpha .",
    );
}

#[test]
fn a_destination_deletion_across_an_origin_insertion_resolves_in_every_order() {
    assert_move_and_deletion_orders(
        DESTINATION_DELETION_ACROSS_AN_ORIGIN_INSERTION,
        "Alpha beta",
        "Alta",
    );
}

/// Resolving the move settles Origin's insertion with it (it is moved text
/// from then on), so the insertion leaves the census without having been
/// selected: a cascade, which the engine names rather than leaving silent.
#[test]
fn resolving_a_move_names_the_insertion_it_settles_as_a_cascade() {
    let original = Document::parse(&docx(INSERTION_THEN_PARAGRAPH_MOVED)).expect("parse");
    let origin = id_of(&original, "Origin", RevisionKind::Insert);
    let move_id = id_of(&original, "Mover", RevisionKind::Move);
    for action in [
        ResolveSelectionAction::Accept,
        ResolveSelectionAction::Reject,
    ] {
        let resolved = resolve(&original, move_id, action);
        assert!(
            stemma::enumerate_revisions(&resolved.snapshot().canonical).is_empty(),
            "{action:?}: the insertion went with the move"
        );
        let cascaded = stemma::tracked_model::cascaded_resolution_ids(
            &original.snapshot().canonical,
            &HashSet::from([move_id]),
            action,
        );
        assert_eq!(cascaded, HashSet::from([origin]), "{action:?}");
    }
}

/// A `w:moveFrom`/`w:moveTo` pair written without the move range markers
/// that name it (§17.13.5.23–28 pair the halves by the range's `w:name`), and
/// an Editor's deletion inside the destination. Nothing pairs the halves, so
/// each reads as what its container is: a deletion and an insertion; the
/// Editor's deletion then strikes inserted text, the stacked state Word
/// writes as `w:del` inside `w:ins` (§17.13.5.14): Reject All removes the
/// inserted text and the deletion with it, Accept All keeps neither.
const UNNAMED_MOVE_WITH_DESTINATION_DELETION: &str = concat!(
    r#"<w:p><w:r><w:t xml:space="preserve">Front. </w:t></w:r>"#,
    r#"<w:moveFrom w:id="11" w:author="Mover" w:date="2026-01-01T00:00:00Z"><w:r><w:t>travelling words</w:t></w:r></w:moveFrom>"#,
    r#"<w:r><w:t xml:space="preserve"> Centre. </w:t></w:r>"#,
    r#"<w:moveTo w:id="13" w:author="Mover" w:date="2026-01-01T00:00:00Z"><w:r><w:t xml:space="preserve">travelling </w:t></w:r>"#,
    r#"<w:del w:id="14" w:author="Editor" w:date="2026-01-02T00:00:00Z"><w:r><w:delText>words</w:delText></w:r></w:del></w:moveTo>"#,
    r#"<w:r><w:t xml:space="preserve"> Back.</w:t></w:r></w:p>"#,
);

#[test]
fn a_deletion_inside_an_unnamed_move_destination_goes_with_the_inserted_text() {
    let original = Document::parse(&docx(UNNAMED_MOVE_WITH_DESTINATION_DELETION)).expect("parse");
    let reopened = reopen(&original);
    for (label, doc) in [("imported", &original), ("reopened", &reopened)] {
        assert_eq!(
            texts(&doc.project(Resolution::RejectAll).expect("reject all")),
            vec!["Front. travelling words Centre.  Back."],
            "{label}: Reject All leaves no copy of the deleted word at the destination"
        );
        assert_eq!(
            texts(&doc.project(Resolution::AcceptAll).expect("accept all")),
            vec!["Front.  Centre. travelling  Back."],
            "{label}"
        );
        let editor = id_of(doc, "Editor", RevisionKind::Delete);
        assert_eq!(
            texts(
                &resolve(doc, editor, ResolveSelectionAction::Reject)
                    .project(Resolution::RejectAll)
                    .unwrap()
            ),
            vec!["Front. travelling words Centre.  Back."],
            "{label}: the restored word is still the destination's inserted text"
        );
    }
}
