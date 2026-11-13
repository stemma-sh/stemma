//! Text inserted and then moved away (Word's W10 ruling).
//!
//! Base "A B." / "C D."; "fresh " is inserted after "A " and then moved after
//! "C ". Word writes the insertion inside the source's `w:moveFrom`.
//!
//! LAW NOTE: for a ranged move whose source holds an insertion, Word's Reject
//! All is NOT the base: the move returns and the nested insertion's text
//! survives at the source as plain text ("A fresh B." / "C D."). Word is the
//! definition of Reject for that file, so these tests check "equals Word",
//! not "equals the base". (No Stemma producer writes this shape.)

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

fn inserted_then_moved(inserter: &str, inserted_date: &str) -> String {
    let m = r#"w:author="Mover" w:date="2026-01-02T00:00:00Z""#;
    format!(
        concat!(
            r#"<w:p><w:r><w:t>Opening.</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t xml:space="preserve">A </w:t></w:r><w:moveFromRangeStart w:id="1" {m} w:name="move1"/>"#,
            r#"<w:moveFrom w:id="2" {m}><w:ins w:id="3" w:author="{inserter}" w:date="{date}"><w:r><w:t xml:space="preserve">fresh </w:t></w:r></w:ins></w:moveFrom>"#,
            r#"<w:moveFromRangeEnd w:id="1"/><w:r><w:t>B.</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t xml:space="preserve">C </w:t></w:r><w:moveToRangeStart w:id="4" {m} w:name="move1"/>"#,
            r#"<w:moveTo w:id="5" {m}><w:r><w:t xml:space="preserve">fresh </w:t></w:r></w:moveTo>"#,
            r#"<w:moveToRangeEnd w:id="4"/><w:r><w:t>D.</w:t></w:r></w:p>"#,
        ),
        m = m,
        inserter = inserter,
        date = inserted_date,
    )
}

fn texts(doc: &Document) -> Vec<String> {
    doc.read()
        .blocks
        .iter()
        .map(|block| block.text.clone())
        .collect()
}

fn ids(doc: &Document, author: &str) -> HashSet<u32> {
    stemma::enumerate_revisions(&doc.snapshot().canonical)
        .into_iter()
        .filter(|r| r.author.as_deref() == Some(author))
        .map(|r| r.revision_id)
        .collect()
}

fn resolve(doc: &Document, author: &str, action: ResolveSelectionAction) -> Document {
    doc.project(Resolution::Selective {
        ids: ids(doc, author),
        action,
    })
    .unwrap_or_else(|e| panic!("{action:?} {author}: {e}"))
}

#[test]
fn reject_all_and_accept_all_equal_word_for_an_insertion_moved_away() {
    // The insertion by the mover itself (Word's probe) and by another author.
    for (inserter, date) in [
        ("Mover", "2026-01-02T00:00:00Z"),
        ("Inserter", "2026-01-01T00:00:00Z"),
    ] {
        let doc = Document::parse(&docx(&inserted_then_moved(inserter, date))).expect("parse");
        let mut kinds: Vec<&str> = stemma::enumerate_revisions(&doc.snapshot().canonical)
            .into_iter()
            .map(|r| r.kind.as_str())
            .collect();
        kinds.sort_unstable();
        assert_eq!(
            kinds,
            vec!["insert", "move"],
            "{inserter}: Word lists the insertion apart from the move"
        );
        assert_eq!(
            texts(&doc.project(Resolution::RejectAll).unwrap()),
            vec!["Opening.", "A fresh B.", "C D."],
            "{inserter}: equals Word's Reject All (not the base)"
        );
        assert_eq!(
            texts(&doc.project(Resolution::AcceptAll).unwrap()),
            vec!["Opening.", "A B.", "C fresh D."],
            "{inserter}"
        );
    }
}

/// Resolving one revision at a time, in Word's probe orders.
#[test]
fn resolving_an_insertion_moved_away_one_revision_at_a_time_equals_word() {
    use ResolveSelectionAction::{Accept, Reject};
    let doc = Document::parse(&docx(&inserted_then_moved(
        "Inserter",
        "2026-01-01T00:00:00Z",
    )))
    .expect("parse");

    // Rejecting the insertion takes the text from the source; what remains at
    // the destination is the mover's plain insertion, no longer a move.
    let ins_rejected = resolve(&doc, "Inserter", Reject);
    assert_eq!(texts(&ins_rejected), vec!["Opening.", "A B.", "C fresh D."]);
    let left: Vec<RevisionKind> = stemma::enumerate_revisions(&ins_rejected.snapshot().canonical)
        .into_iter()
        .map(|r| r.kind)
        .collect();
    assert_eq!(left, vec![RevisionKind::Insert]);
    assert_eq!(
        texts(&resolve(&ins_rejected, "Mover", Reject)),
        vec!["Opening.", "A B.", "C D."]
    );
    let reopened =
        Document::parse(&ins_rejected.serialize(&ExportOptions::default()).unwrap()).unwrap();
    assert_eq!(
        texts(&reopened.project(Resolution::RejectAll).unwrap()),
        vec!["Opening.", "A B.", "C D."],
        "after reopen"
    );

    // Rejecting the move first returns it with the inserted text, which then
    // has nothing left to resolve.
    let move_rejected = resolve(&doc, "Mover", Reject);
    assert_eq!(
        texts(&move_rejected),
        vec!["Opening.", "A fresh B.", "C D."]
    );
    assert!(stemma::enumerate_revisions(&move_rejected.snapshot().canonical).is_empty());

    // Accepting the insertion settles it into the move, still pending.
    let ins_accepted = resolve(&doc, "Inserter", Accept);
    let left: Vec<RevisionKind> = stemma::enumerate_revisions(&ins_accepted.snapshot().canonical)
        .into_iter()
        .map(|r| r.kind)
        .collect();
    assert_eq!(left, vec![RevisionKind::Move]);
    assert_eq!(
        texts(&resolve(&doc, "Mover", Accept)),
        vec!["Opening.", "A B.", "C fresh D."]
    );
}

/// The mover then deleted the moved text at the destination (`w:del` inside
/// the `w:moveTo`, same author and date as the move): Word lists the
/// deletion apart from the move, and its Reject All restores the source with
/// no copy left at the destination.
#[test]
fn a_deletion_by_the_mover_inside_the_destination_equals_word() {
    let m = r#"w:author="Mover" w:date="2026-01-02T00:00:00Z""#;
    let body = format!(
        concat!(
            r#"<w:p><w:r><w:t>Opening.</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t xml:space="preserve">A </w:t></w:r><w:moveFromRangeStart w:id="1" {m} w:name="move1"/>"#,
            r#"<w:moveFrom w:id="2" {m}><w:r><w:t xml:space="preserve">fresh </w:t></w:r></w:moveFrom>"#,
            r#"<w:moveFromRangeEnd w:id="1"/><w:r><w:t>B.</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t xml:space="preserve">C </w:t></w:r><w:moveToRangeStart w:id="4" {m} w:name="move1"/>"#,
            r#"<w:moveTo w:id="5" {m}><w:del w:id="6" {m}><w:r><w:delText xml:space="preserve">fresh </w:delText></w:r></w:del></w:moveTo>"#,
            r#"<w:moveToRangeEnd w:id="4"/><w:r><w:t>D.</w:t></w:r></w:p>"#,
        ),
        m = m
    );
    let doc = Document::parse(&docx(&body)).expect("parse");
    assert_eq!(
        texts(&doc.project(Resolution::RejectAll).unwrap()),
        vec!["Opening.", "A fresh B.", "C D."]
    );
    assert_eq!(
        texts(&doc.project(Resolution::AcceptAll).unwrap()),
        vec!["Opening.", "A B.", "C D."]
    );
}

/// The whole paragraph "Fresh line." was inserted, then moved from after
/// "Opening." to after "Middle.". Word (checked) reads each order as below:
/// resolving the move resolves the inserted text with it, but the source's
/// inserted paragraph break outlives a rejected move as the inserter's own
/// pending insertion, so Reject All joins the text onto "Middle.".
fn paragraph_inserted_then_moved() -> String {
    let i = r#"w:author="Inserter" w:date="2026-01-01T00:00:00Z""#;
    let m = r#"w:author="Mover" w:date="2026-01-02T00:00:00Z""#;
    format!(
        concat!(
            r#"<w:p><w:r><w:t>Opening.</w:t></w:r></w:p>"#,
            r#"<w:p><w:pPr><w:rPr><w:ins w:id="1" {i}/><w:moveFrom w:id="2" {m}/></w:rPr></w:pPr>"#,
            r#"<w:moveFromRangeStart w:id="3" {m} w:name="move1"/>"#,
            r#"<w:moveFrom w:id="4" {m}><w:ins w:id="5" {i}><w:r><w:t>Fresh line.</w:t></w:r></w:ins></w:moveFrom>"#,
            r#"<w:moveFromRangeEnd w:id="3"/></w:p>"#,
            r#"<w:p><w:r><w:t>Middle.</w:t></w:r></w:p>"#,
            r#"<w:p><w:pPr><w:rPr><w:moveTo w:id="6" {m}/></w:rPr></w:pPr>"#,
            r#"<w:moveToRangeStart w:id="7" {m} w:name="move1"/>"#,
            r#"<w:moveTo w:id="8" {m}><w:r><w:t>Fresh line.</w:t></w:r></w:moveTo>"#,
            r#"<w:moveToRangeEnd w:id="7"/></w:p>"#,
            r#"<w:p><w:r><w:t>Tail.</w:t></w:r></w:p>"#,
        ),
        i = i,
        m = m
    )
}

fn pending(doc: &Document) -> Vec<(String, &'static str)> {
    let mut rows: Vec<(String, &'static str)> =
        stemma::enumerate_revisions(&doc.snapshot().canonical)
            .into_iter()
            .map(|r| (r.author.unwrap_or_default(), r.kind.as_str()))
            .collect();
    rows.sort();
    rows.dedup();
    rows
}

#[test]
fn an_inserted_paragraph_moved_away_resolves_in_each_order_as_word_does() {
    use ResolveSelectionAction::{Accept, Reject};
    let doc = Document::parse(&docx(&paragraph_inserted_then_moved())).expect("parse");
    let base = ["Opening.", "Middle.", "Tail."];
    let moved = ["Opening.", "Middle.", "Fresh line.", "Tail."];
    let returned = ["Opening.", "Fresh line.", "Middle.", "Tail."];
    let joined = ["Opening.", "Fresh line.Middle.", "Tail."];

    assert_eq!(texts(&doc.project(Resolution::AcceptAll).unwrap()), moved);
    assert_eq!(texts(&doc.project(Resolution::RejectAll).unwrap()), joined);

    let ins_rejected = resolve(&doc, "Inserter", Reject);
    assert_eq!(texts(&ins_rejected), moved);
    assert_eq!(
        pending(&ins_rejected),
        vec![("Mover".to_string(), "insert")]
    );
    assert_eq!(texts(&resolve(&ins_rejected, "Mover", Reject)), base);

    let move_rejected = resolve(&doc, "Mover", Reject);
    assert_eq!(texts(&move_rejected), returned);
    assert_eq!(
        pending(&move_rejected),
        vec![("Inserter".to_string(), "insert")]
    );
    assert_eq!(texts(&resolve(&move_rejected, "Inserter", Reject)), joined);

    let ins_accepted = resolve(&doc, "Inserter", Accept);
    assert_eq!(pending(&ins_accepted), vec![("Mover".to_string(), "move")]);
    let move_accepted = resolve(&doc, "Mover", Accept);
    assert_eq!(texts(&move_accepted), moved);
    assert!(pending(&move_accepted).is_empty());
}
