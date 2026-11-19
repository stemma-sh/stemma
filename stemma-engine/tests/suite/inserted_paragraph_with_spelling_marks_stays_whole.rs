//! An edit elsewhere in the document leaves a whole inserted paragraph that
//! holds spelling or grammar markers as it was.
//!
//! Word records proofing state inside the text it marks, so an inserted
//! paragraph often carries `w:proofErr` markers (§17.13.8.1) inside its
//! `w:ins`, and bookmarks the same way. They are not content and not
//! revisions: the paragraph is still one insertion of its content plus its
//! paragraph mark. A tracked edit in ANOTHER paragraph must not change that
//! revision, so a review of the session reports it untouched, and a save
//! leaves the same revisions in the file.

use std::io::{Cursor, Write};

use stemma::api::Document;
use stemma::audit::RevisionDisposition;
use stemma::edit::{
    BarrierPolicy, EditTransaction, ExpectedMatches, MatchMode, MaterializationMode,
    ReplaceTextOptions, ReplaceTextScope, plan_replace_text,
};
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

/// An untracked paragraph, then an inserted paragraph (mark and content)
/// whose `w:ins` holds `markers` between its runs, then an untracked one.
fn body(who: &str, markers: &str) -> String {
    format!(
        concat!(
            "<w:p>{opening}</w:p>",
            r#"<w:p><w:pPr><w:rPr><w:ins w:id="1" {who}/></w:rPr></w:pPr>"#,
            r#"<w:ins w:id="2" {who}>{first}{markers}{second}</w:ins></w:p>"#,
            "<w:p>{closing}</w:p>",
        ),
        who = who,
        markers = markers,
        opening = text_run("The ferry leaves at noon."),
        first = text_run("Lockers are "),
        second = text_run("available on deck."),
        closing = text_run("Bicycles ride free."),
    )
}

/// A tracked one-word replacement in the opening paragraph.
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

fn identities(document: &Document) -> Vec<u32> {
    let mut ids: Vec<u32> =
        stemma::tracked_model::enumerate_revisions(&document.snapshot().canonical)
            .into_iter()
            .map(|record| record.revision_id)
            .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

fn assert_edit_elsewhere_leaves_insertion_alone(who: &str, markers: &str) {
    let body = body(who, markers);
    let original = Document::parse(&docx(&body)).expect("the document loads");
    let edited = replace_noon_with_dusk(&original);

    let review = edited.review().expect("review");
    for prior in &review.preexisting_revisions {
        assert_eq!(
            prior.disposition,
            RevisionDisposition::Untouched,
            "an edit in another paragraph changed {:?}",
            prior.record
        );
    }
    // The replacement is a deletion and an insertion; nothing else is new.
    assert_eq!(review.new_revisions.len(), 2, "{:#?}", review.new_revisions);

    let saved = edited
        .serialize(&stemma::ExportOptions::default())
        .expect("save");
    let reopened = Document::parse(&saved).expect("the saved document loads");
    assert_eq!(identities(&edited), identities(&reopened));
    for identity in identities(&original) {
        assert!(identities(&reopened).contains(&identity));
    }

    for document in [&edited, &reopened] {
        assert_eq!(
            document
                .project(Resolution::AcceptAll)
                .expect("accept all")
                .to_text(),
            "The ferry leaves at dusk.\n\nLockers are available on deck.\n\nBicycles ride free."
        );
        assert_eq!(
            document
                .project(Resolution::RejectAll)
                .expect("reject all")
                .to_text(),
            "The ferry leaves at noon.\n\nBicycles ride free."
        );
    }
}

#[test]
fn an_inserted_paragraph_with_a_grammar_marker_survives_an_edit_elsewhere() {
    assert_edit_elsewhere_leaves_insertion_alone(
        r#"w:author="Petra""#,
        r#"<w:proofErr w:type="gramStart"/>"#,
    );
}

#[test]
fn an_inserted_paragraph_with_spelling_and_grammar_markers_survives_an_edit_elsewhere() {
    assert_edit_elsewhere_leaves_insertion_alone(
        r#"w:author="Petra" w:date="2024-05-06T07:08:00Z""#,
        r#"<w:proofErr w:type="spellStart"/><w:proofErr w:type="spellEnd"/><w:proofErr w:type="gramEnd"/>"#,
    );
}

#[test]
fn an_inserted_paragraph_with_a_bookmark_survives_an_edit_elsewhere() {
    assert_edit_elsewhere_leaves_insertion_alone(
        r#"w:author="Petra""#,
        r#"<w:bookmarkStart w:id="7" w:name="lockers"/><w:bookmarkEnd w:id="7"/>"#,
    );
}
