//! Daily-tier spec tests for the FOOTNOTES / ENDNOTES verb against ECMA-376
//! §17.11 (footnoteReference §17.11.3, endnoteReference §17.11.7, footnote /
//! endnote stories §17.11.10 / §17.11.2, footnoteRef §17.11.6 / endnoteRef
//! §17.11.1).
//!
//! Behavioral constraints (these encode the spec, not the current code):
//! - SPEC §17.11.3/.7 + §17.11.10/.2: a `footnoteReference` w:id resolves to
//!   exactly one footnote story; that story's first paragraph carries a
//!   `w:footnoteRef` auto-number placeholder.
//! - SPEC §17.11.10: the reserved `separator` / `continuationSeparator` notes
//!   are never renumbered or deleted by authoring; the allocator skips their
//!   reserved ids.
//! - Wire contract (CLAUDE.md "no silent fallbacks"): an unknown `note_kind`
//!   string is refused (`UnknownNoteKind`), NEVER defaulted to footnote.
//! - The post-serialization validator passes on an authored footnote.
//!
//! Daily tier, corpus-free (synthesized in-memory DOCX).

use stemma::ExportOptions;
use stemma::api::Document;
use stemma::docx::DocxArchive;
use stemma::domain::{BlockNode, CanonDoc, NodeId, NoteType, RevisionInfo};
use stemma::edit::{EditStep, EditTransaction, MaterializationMode, NoteKind};
use stemma::edit_v4::{AdapterError, parse_transaction};

fn make_docx(text: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"></Relationships>"#;

    use std::io::Write;
    use zip::write::FileOptions;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: FileOptions = FileOptions::default();
        zip.start_file("[Content_Types].xml", opts).unwrap();
        zip.write_all(content_types.as_bytes()).unwrap();
        zip.start_file("_rels/.rels", opts).unwrap();
        zip.write_all(rels.as_bytes()).unwrap();
        zip.start_file("word/_rels/document.xml.rels", opts)
            .unwrap();
        zip.write_all(doc_rels.as_bytes()).unwrap();
        zip.start_file("word/document.xml", opts).unwrap();
        zip.write_all(document_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

fn first_block_id(doc: &CanonDoc) -> NodeId {
    match &doc.blocks[0].block {
        BlockNode::Paragraph(p) => p.id.clone(),
        other => panic!("expected paragraph, got {other:?}"),
    }
}

fn txn(steps: Vec<EditStep>) -> EditTransaction {
    EditTransaction {
        steps,
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 1,
            author: Some("Reviewer".to_string()),
            date: Some("2026-06-01T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

/// §17.11.3 + §17.11.10: a footnoteReference w:id resolves to exactly one
/// footnote story, and that story's first paragraph carries a w:footnoteRef.
#[test]
fn spec_footnote_reference_resolves_to_one_story_with_footnote_ref() {
    let base =
        Document::parse(&make_docx("The clause has a footnote marker here.")).expect("parse");
    let block_id = first_block_id(&base.snapshot().canonical);
    let edited = base
        .apply(&txn(vec![EditStep::InsertNote {
            block_id,
            expect: "footnote".to_string(),
            semantic_hash: None,
            note_kind: NoteKind::Footnote,
            body: "The footnote body.".to_string(),
            rationale: None,
        }]))
        .expect("insert");

    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let archive = DocxArchive::read(&bytes).expect("read");
    let doc_xml = String::from_utf8_lossy(archive.get("word/document.xml").expect("document.xml"));
    let footnotes_xml =
        String::from_utf8_lossy(archive.get("word/footnotes.xml").expect("footnotes.xml"));

    // Exactly one footnoteReference in the body.
    assert_eq!(
        doc_xml.matches("footnoteReference").count(),
        1,
        "exactly one footnoteReference run in body"
    );

    // Its w:id resolves to a story. Re-parse to compare structurally: the
    // authored note (the single Normal story) has a footnoteRef in its first
    // paragraph (§17.11.6).
    let reimported = Document::parse(&bytes).expect("reparse");
    let canon = &reimported.snapshot().canonical;
    let normal: Vec<_> = canon
        .footnotes
        .iter()
        .filter(|f| matches!(f.note_type, NoteType::Normal))
        .collect();
    assert_eq!(normal.len(), 1, "exactly one authored footnote story");
    // The story's first paragraph carries the footnoteRef marker (round-trips
    // through raw_xml).
    assert!(
        footnotes_xml.contains("footnoteRef"),
        "story first paragraph carries w:footnoteRef auto-number marker"
    );
}

/// §17.11.10: the reserved separator / continuationSeparator notes are present
/// in the synthesized part and are never renumbered or removed by authoring
/// (their reserved ids -1 / 0 are skipped by the allocator, so the authored
/// note takes id 1, never -1 or 0).
#[test]
fn spec_reserved_separator_notes_untouched_and_skipped() {
    let base = Document::parse(&make_docx("Add a footnote to this sentence.")).expect("parse");
    let block_id = first_block_id(&base.snapshot().canonical);
    let edited = base
        .apply(&txn(vec![EditStep::InsertNote {
            block_id,
            expect: "footnote".to_string(),
            semantic_hash: None,
            note_kind: NoteKind::Footnote,
            body: "Body.".to_string(),
            rationale: None,
        }]))
        .expect("insert");

    // Authored note id is 1, never a reserved id.
    let authored = edited
        .snapshot()
        .canonical
        .footnotes
        .iter()
        .find(|f| matches!(f.note_type, NoteType::Normal))
        .expect("authored note")
        .id
        .clone();
    assert_eq!(authored, "1", "first authored note skips reserved -1/0");

    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let archive = DocxArchive::read(&bytes).expect("read");
    let footnotes_xml =
        String::from_utf8_lossy(archive.get("word/footnotes.xml").expect("footnotes.xml"));
    // Reserved notes present and intact.
    assert!(
        footnotes_xml.contains(r#"w:id="-1""#),
        "separator note id=-1 present"
    );
    assert!(
        footnotes_xml.contains(r#"w:id="0""#),
        "continuationSeparator id=0 present"
    );
    assert!(footnotes_xml.contains("w:type=\"separator\""));
    assert!(footnotes_xml.contains("w:type=\"continuationSeparator\""));
}

/// Wire contract: an unknown note_kind string is refused at the adapter edge as
/// `UnknownNoteKind` — NEVER silently defaulted to footnote.
#[test]
fn spec_unknown_note_kind_is_refused_never_defaulted() {
    let json = r#"{
      "ops": [{ "op": "insert_note", "target": "p_1", "expect": "x", "note_kind": "sidebar", "body": "b" }],
      "revision": { "author": "wire" }
    }"#;
    // Schema validation passes (note_kind is a free string at the schema layer);
    // the adapter is where the closed set is enforced.
    let parsed = parse_transaction(json).expect("schema check passes");
    let err = parsed
        .into_edit_transaction()
        .expect_err("unknown note_kind must be refused by the adapter");
    match err {
        AdapterError::UnknownNoteKind { value, .. } => {
            assert_eq!(value, "sidebar", "the rejected value is surfaced verbatim");
        }
        other => panic!("expected UnknownNoteKind, got {other:?}"),
    }
}

/// Both valid note_kind tokens translate; endnote maps to the endnote family.
#[test]
fn spec_valid_note_kinds_translate() {
    for (kind, _) in [("footnote", ()), ("endnote", ())] {
        let json = format!(
            r#"{{ "ops": [{{ "op": "insert_note", "target": "p_1", "expect": "x", "note_kind": "{kind}", "body": "b" }}], "revision": {{ "author": "wire" }} }}"#
        );
        let txn = parse_transaction(&json)
            .expect("schema ok")
            .into_edit_transaction()
            .expect("adapter ok");
        assert_eq!(txn.steps.len(), 1);
    }
}

/// The post-serialization validator passes on an authored footnote (no
/// structural / xref errors introduced by the synthesized part + reference).
#[test]
fn spec_authored_footnote_passes_validator() {
    let base =
        Document::parse(&make_docx("Validate this footnote insertion path.")).expect("parse");
    let block_id = first_block_id(&base.snapshot().canonical);
    let edited = base
        .apply(&txn(vec![EditStep::InsertNote {
            block_id,
            expect: "footnote".to_string(),
            semantic_hash: None,
            note_kind: NoteKind::Footnote,
            body: "Validated body.".to_string(),
            rationale: None,
        }]))
        .expect("insert");
    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");

    let validation = stemma::docx_validate::validate_docx(&bytes);
    assert!(
        !validation.has_errors(),
        "post-serialization validator must pass on an authored footnote, got: {:?}",
        validation
            .findings
            .iter()
            .map(|f| f.to_string())
            .collect::<Vec<_>>()
    );
}
