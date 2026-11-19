//! A tracked deletion nested inside a tracked insertion stays where it is
//! across a save.
//!
//! ECMA-376 lets a `w:ins` contain a `w:del` (CT_RunTrackChange admits
//! EG_RunLevelElements, which include `w:del`). Word writes this shape when
//! text inside an insertion was later deleted, e.g. a hyphen removed from an
//! inserted word:
//!
//! ```xml
//! <w:ins w:id="2"><w:r>…non</w:r><w:del w:id="3"><w:r><w:delText>-</w:delText></w:r></w:del><w:r>binding…</w:r></w:ins>
//! ```
//!
//! That is ONE insertion (one `w:ins` annotation) with one deletion inside
//! it. Saving the document must not change which revisions it holds: the
//! insertion must not come back as several insertions (one before the
//! deletion, one stacked on the deleted text, one after it). Word may omit
//! `w:date` on its revisions, so the shape is checked with and without one.

use std::io::{Cursor, Write};

use stemma::api::Document;
use stemma::edit::{
    BarrierPolicy, EditTransaction, ExpectedMatches, MatchMode, MaterializationMode,
    ReplaceTextOptions, ReplaceTextScope, plan_replace_text,
};
use stemma::tracked_model::RevisionKind;
use stemma::{Resolution, RevisionInfo};
use zip::ZipWriter;
use zip::write::FileOptions;

fn docx(body: &str) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let options = FileOptions::default();
    zip.start_file("[Content_Types].xml", options).unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#).unwrap();
    zip.start_file("_rels/.rels", options).unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#).unwrap();
    zip.start_file("word/_rels/document.xml.rels", options)
        .unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#).unwrap();
    zip.start_file("word/document.xml", options).unwrap();
    zip.write_all(
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
        )
        .as_bytes(),
    )
    .unwrap();
    zip.finish().unwrap().into_inner()
}

fn text_run(text: &str) -> String {
    format!(r#"<w:r><w:t xml:space="preserve">{text}</w:t></w:r>"#)
}

/// An inserted paragraph (inserted mark, inserted content) between two
/// untracked ones. Inside the insertion, `deleted` was deleted again.
fn inserted_paragraph_with_inner_deletion(who: &str, deleted: &str) -> String {
    format!(
        concat!(
            "<w:p>{opening}</w:p>",
            r#"<w:p><w:pPr><w:rPr><w:ins w:id="1" {who}/></w:rPr></w:pPr>"#,
            r#"<w:ins w:id="2" {who}>{before}"#,
            r#"<w:del w:id="3" {who}><w:r><w:delText xml:space="preserve">{deleted}</w:delText></w:r></w:del>"#,
            r#"{after}</w:ins></w:p>"#,
            "<w:p>{closing}</w:p>",
        ),
        who = who,
        deleted = deleted,
        opening = text_run("The ferry leaves at noon."),
        // A spelling marker inside the insertion, as Word leaves them.
        before = format!(
            r#"{}<w:proofErr w:type="spellStart"/>{}<w:proofErr w:type="spellEnd"/>"#,
            text_run("Tickets "),
            text_run("are non")
        ),
        after = format!("{}{}", text_run("refundable on "), text_run("weekdays.")),
        closing = text_run("Bicycles ride free."),
    )
}

/// One entry per carrier record: (identity, kind, excerpt). A revision whose
/// content is split by an inner change is reported as several records that
/// share one identity; the pending revisions are the distinct identities.
fn records(document: &Document) -> Vec<(u32, RevisionKind, String)> {
    let mut records: Vec<(u32, RevisionKind, String)> =
        stemma::tracked_model::enumerate_revisions(&document.snapshot().canonical)
            .into_iter()
            .map(|record| (record.revision_id, record.kind, record.excerpt))
            .collect();
    records.sort_unstable_by(|a, b| (a.0, &a.2).cmp(&(b.0, &b.2)));
    records
}

fn identities(records: &[(u32, RevisionKind, String)]) -> Vec<u32> {
    let mut ids: Vec<u32> = records.iter().map(|record| record.0).collect();
    ids.dedup();
    ids
}

/// The identity of the content insertion (`w:ins` id 2). Every inserted
/// piece of the paragraph's text belongs to it: the text on both sides of
/// the inner deletion and the deleted text under it. Panics if they differ.
fn content_insertion(records: &[(u32, RevisionKind, String)], deleted: &str) -> u32 {
    let pieces: Vec<u32> = records
        .iter()
        .filter(|(_, kind, excerpt)| {
            *kind == RevisionKind::Insert
                && !excerpt.starts_with('\u{b6}')
                && (excerpt.contains("are non")
                    || excerpt.contains("refundable")
                    || excerpt == deleted)
        })
        .map(|record| record.0)
        .collect();
    assert!(
        pieces.len() >= 2,
        "the content insertion is enumerated: {records:#?}"
    );
    assert!(
        pieces.iter().all(|id| *id == pieces[0]),
        "one insertion across the inner deletion: {records:#?}"
    );
    pieces[0]
}

/// A tracked one-word replacement in the opening paragraph, which holds no
/// revision. It makes the save write the document out rather than return the
/// bytes it was opened from.
fn replace_noon_with_dusk(document: &Document) -> Document {
    let options = ReplaceTextOptions {
        old: "noon".to_string(),
        new: "dusk".to_string(),
        author: "Olle".to_string(),
        scope: ReplaceTextScope::WholeDoc,
        expected: ExpectedMatches::Count(1),
        match_mode: MatchMode::Exact,
        on_barrier_match: BarrierPolicy::Skip,
    };
    let plan = plan_replace_text(document.snapshot().canonical.as_ref(), &options)
        .expect("the replacement plans");
    document
        .apply(&EditTransaction {
            steps: plan.steps,
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: RevisionInfo {
                revision_id: 0,
                identity: 0,
                author: Some(options.author.clone()),
                date: None,
                apply_op_id: None,
                date_utc: None,
            },
        })
        .expect("the replacement applies")
}

/// Save and reopen from the markup alone. A save also writes the engine's
/// own record of revision identities (`word/stemmaRevisionIds.xml`), which a
/// reopen adopts when it still describes the document; other editors do not
/// keep it, so the revisions must come back from the `w:ins`/`w:del` markup
/// itself. The part is emptied before reopening (kept, so the package
/// still holds every part its relationships name).
fn save(document: &Document) -> Document {
    let saved = document
        .serialize(&stemma::ExportOptions::default())
        .expect("save");
    let mut archive = zip::ZipArchive::new(Cursor::new(saved)).expect("the save is a zip");
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).expect("zip entry");
        let name = entry.name().to_string();
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut bytes).expect("read entry");
        if name == "word/stemmaRevisionIds.xml" {
            bytes = br#"<revisionIds xmlns="http://stemma.sh/2026/revisionIds"/>"#.to_vec();
        }
        zip.start_file(name, FileOptions::default()).unwrap();
        zip.write_all(&bytes).unwrap();
    }
    let markup_only = zip.finish().unwrap().into_inner();
    Document::parse(&markup_only).expect("the saved document loads")
}

fn assert_save_keeps_revisions(who: &str, deleted: &str) {
    let body = inserted_paragraph_with_inner_deletion(who, deleted);
    let original = Document::parse(&docx(&body)).expect("the document loads");
    let insertion = content_insertion(&records(&original), deleted);

    let edited = replace_noon_with_dusk(&original);
    // The edit itself leaves every prior revision as it was.
    for prior in edited.review().expect("review").preexisting_revisions {
        assert_eq!(
            prior.disposition,
            stemma::audit::RevisionDisposition::Untouched,
            "{:?}",
            prior.record
        );
    }
    let before = records(&edited);
    let reopened = save(&edited);
    let after = records(&reopened);

    // Every revision the document held is still there, unchanged, and the
    // only new ones are the replacement's.
    for identity in identities(&records(&original)) {
        assert!(
            identities(&after).contains(&identity),
            "revision {identity} of the original is gone after the save: {after:#?}"
        );
    }
    assert_eq!(
        identities(&before).len(),
        identities(&after).len(),
        "pending revision count across a save: before {before:#?} after {after:#?}"
    );
    assert_eq!(
        identities(&before),
        identities(&after),
        "revision identities across a save: before {before:#?} after {after:#?}"
    );
    assert_eq!(insertion, content_insertion(&after, deleted));

    // A second save changes nothing either.
    assert_eq!(identities(&after), identities(&records(&save(&reopened))));

    // Accept All keeps the insertion and applies the deletion inside it;
    // Reject All removes the inserted paragraph (content and mark), and with
    // it the deleted text. Both hold before and after the save.
    for (label, document) in [("edited", &edited), ("saved", &reopened)] {
        assert_eq!(
            document
                .project(Resolution::AcceptAll)
                .expect("accept all")
                .to_text(),
            "The ferry leaves at dusk.\n\nTickets are nonrefundable on weekdays.\n\nBicycles ride free.",
            "Accept All ({label})"
        );
        assert_eq!(
            document
                .project(Resolution::RejectAll)
                .expect("reject all")
                .to_text(),
            "The ferry leaves at noon.\n\nBicycles ride free.",
            "Reject All ({label})"
        );
    }
}

#[test]
fn an_undated_insertion_with_a_deletion_inside_keeps_its_revisions_across_a_save() {
    assert_save_keeps_revisions(r#"w:author="Petra""#, "-");
}

#[test]
fn a_dated_insertion_with_a_deletion_inside_keeps_its_revisions_across_a_save() {
    assert_save_keeps_revisions(r#"w:author="Petra" w:date="2024-05-06T07:08:00Z""#, " ");
}
