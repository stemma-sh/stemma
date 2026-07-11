//! Nested tracked changes at import: the entry-door contract.
//!
//! The supported one-level `ins`/`del` pair — EITHER markup order — parses
//! into the stacked state (`TrackingStatus::InsertedThenDeleted`); see
//! `spec_stacked_revisions.rs` for its semantics. This file pins what happens
//! at the entry door for everything else:
//!
//!   - same-type nesting (`ins`-in-`ins`, `del`-in-`del`) is invalid OOXML
//!     (validator rule I-TC-003) and refuses at import;
//!   - UNSUPPORTED mixes (a move container nested with anything; nesting
//!     deeper than one level) quarantine the body item as a byte-faithful,
//!     reason-coded opaque block — never a silent drop, never a lying read
//!     view;
//!   - paragraph-mark revision markers (`w:pPr/w:rPr/w:del`) are property-bag
//!     markers, not content containers — negative control;
//!   - while a quarantined block exists: editing it refuses and compare
//!     refuses (their outputs would misrepresent the quarantined content);
//!     resolution — all-or-nothing AND selective — carries the placeholder
//!     through un-resolved: the quarantine is a disciplined isolation
//!     boundary (selectable ids are enumerate-minted and enumerate reports
//!     quarantined interiors census-only, so a selection is provably
//!     disjoint from the quarantine), and the census keeps disclosing it.
//!
//! Daily, corpus-free.

use stemma::api::Document;
use stemma::edit::{
    ContentFragment, EditStep, EditTransaction, MaterializationMode, ParagraphContent,
};
use stemma::{Resolution, ResolveSelectionAction, RevisionInfo};

// ─── Fixtures ──────────────────────────────────────────────────────────────

/// An UNSUPPORTED nested mix: a move container with a nested insertion.
/// (The supported ins/del pair parses — see spec_stacked_revisions.rs.)
const MOVE_MIX_P: &str = r#"<w:p><w:r><w:t xml:space="preserve">Start </w:t></w:r><w:moveTo w:id="3" w:author="Mover" w:date="2026-01-01T00:00:00Z"><w:ins w:id="4" w:author="AuthorA" w:date="2026-01-02T00:00:00Z"><w:r><w:t xml:space="preserve">mixed </w:t></w:r></w:ins></w:moveTo><w:r><w:t xml:space="preserve">end.</w:t></w:r></w:p>"#;

const PLAIN_P: &str =
    r#"<w:p><w:r><w:t xml:space="preserve">An ordinary second paragraph.</w:t></w:r></w:p>"#;

fn make_docx_with_body(body_inner: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
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

fn revision() -> RevisionInfo {
    RevisionInfo {
        revision_id: 50,
        author: Some("quarantine-test".to_string()),
        date: Some("2026-06-09T00:00:00Z".to_string()),
        apply_op_id: None,
    }
}

fn text_content(s: &str) -> ParagraphContent {
    ParagraphContent {
        fragments: vec![ContentFragment::Text(s.to_string())],
    }
}

fn document_xml_of(docx: &[u8]) -> String {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(docx.to_vec())).expect("zip");
    let mut s = String::new();
    use std::io::Read;
    zip.by_name("word/document.xml")
        .expect("document.xml")
        .read_to_string(&mut s)
        .expect("utf8");
    s
}

// ─── Unsupported mixes quarantine ────────────────────────────────────────────

#[test]
fn unsupported_nested_mix_imports_as_quarantined_opaque_block() {
    let bytes = make_docx_with_body(&format!("{MOVE_MIX_P}{PLAIN_P}"));
    let doc = Document::parse(&bytes).expect("a quarantinable doc still opens");
    let view = doc.read();

    assert_eq!(view.blocks.len(), 2, "both body items present");
    let quarantined = &view.blocks[0];
    assert_eq!(quarantined.role, stemma::view::BlockRole::Opaque);
    assert_eq!(
        quarantined.opaque_label.as_deref(),
        Some("quarantined_nested_tracked_changes"),
        "the placeholder names its reason"
    );
    assert_eq!(
        quarantined.text, "",
        "a quarantined block exposes no text (it would misrepresent the contested state)"
    );
    assert_eq!(view.blocks[1].text, "An ordinary second paragraph.");
}

#[test]
fn quarantined_block_round_trips_byte_faithfully_through_edits_elsewhere() {
    let bytes = make_docx_with_body(&format!("{MOVE_MIX_P}{PLAIN_P}"));
    let doc = Document::parse(&bytes).expect("parse");
    let plain_id = doc.read().blocks[1].id.clone();

    let edited = doc
        .apply(&EditTransaction {
            steps: vec![EditStep::ReplaceParagraphText {
                block_id: plain_id,
                rationale: None,
                replacement_role: None,
                expect: "ordinary".to_string(),
                semantic_hash: None,
                content: text_content("An edited second paragraph."),
            }],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: revision(),
        })
        .expect("editing a non-quarantined block works");
    let saved = edited
        .serialize(&stemma::ExportOptions::default())
        .expect("serialize");
    let xml = document_xml_of(&saved);

    let move_chunk = xml
        .split("<w:moveTo ")
        .find(|c| c.contains("Mover"))
        .expect("the move container survives verbatim");
    let move_body = move_chunk.split("</w:moveTo>").next().expect("closes");
    assert!(
        move_body.contains("<w:ins ") && move_body.contains("AuthorA"),
        "the nested insertion must survive inside the move container verbatim: {move_body}"
    );
}

// ─── Same-type nesting: invalid OOXML refuses at the entry door ──────────────

#[test]
fn same_type_nesting_refuses_at_import() {
    let ins_in_ins = r#"<w:p><w:ins w:id="1" w:author="A" w:date="2026-01-01T00:00:00Z"><w:r><w:t>a</w:t></w:r><w:ins w:id="2" w:author="B" w:date="2026-01-02T00:00:00Z"><w:r><w:t>b</w:t></w:r></w:ins></w:ins></w:p>"#;
    let err = Document::parse(&make_docx_with_body(ins_in_ins))
        .err()
        .expect("ins-in-ins is invalid OOXML (I-TC-003) and must refuse at import");
    assert!(
        err.message.contains("ins") && err.message.to_lowercase().contains("nest"),
        "the refusal must name the same-type nesting: {}",
        err.message
    );

    let del_in_del = r#"<w:p><w:del w:id="1" w:author="A" w:date="2026-01-01T00:00:00Z"><w:r><w:delText>a</w:delText></w:r><w:del w:id="2" w:author="B" w:date="2026-01-02T00:00:00Z"><w:r><w:delText>b</w:delText></w:r></w:del></w:del></w:p>"#;
    let err = Document::parse(&make_docx_with_body(del_in_del))
        .err()
        .expect("del-in-del is invalid OOXML (I-TC-003) and must refuse at import");
    assert!(
        err.message.contains("del") && err.message.to_lowercase().contains("nest"),
        "the refusal must name the same-type nesting: {}",
        err.message
    );
}

// ─── Negative control: paragraph-mark markers are not content containers ─────

#[test]
fn paragraph_mark_revision_markers_do_not_quarantine() {
    let body = r#"<w:p><w:pPr><w:rPr><w:del w:id="9" w:author="A" w:date="2026-01-01T00:00:00Z"/></w:rPr></w:pPr><w:r><w:t xml:space="preserve">Base </w:t></w:r><w:ins w:id="1" w:author="A" w:date="2026-01-01T00:00:00Z"><w:r><w:t>added</w:t></w:r></w:ins></w:p>"#;
    let doc = Document::parse(&make_docx_with_body(body)).expect("parse");
    let view = doc.read();
    assert_eq!(
        view.blocks[0].role,
        stemma::view::BlockRole::Paragraph,
        "paragraph-mark markers must not trip the quarantine"
    );
    assert_eq!(view.blocks[0].text, "Base added");
}

// ─── Quarantined blocks refuse edits, selective resolution, and compare ──────

#[test]
fn editing_a_quarantined_block_refuses() {
    let bytes = make_docx_with_body(&format!("{MOVE_MIX_P}{PLAIN_P}"));
    let doc = Document::parse(&bytes).expect("parse");
    let quarantined_id = doc.read().blocks[0].id.clone();

    let err = doc
        .apply(&EditTransaction {
            steps: vec![EditStep::ReplaceParagraphText {
                block_id: quarantined_id,
                rationale: None,
                replacement_role: None,
                expect: "mixed".to_string(),
                semantic_hash: None,
                content: text_content("rewritten"),
            }],
            summary: None,
            materialization_mode: MaterializationMode::TrackedChange,
            revision: revision(),
        })
        .err()
        .expect("editing a quarantined block must refuse");
    assert_eq!(err.code, stemma::ErrorCode::UnsupportedEdit, "{err:?}");
}

#[test]
fn selective_resolution_of_visible_ids_works_alongside_a_quarantined_block() {
    // Domain rule (D4 revised): the quarantine is a disciplined isolation
    // boundary. A selection of enumerate-visible ids is provably disjoint
    // from it (quarantined interiors are census-only id 0), so resolving an
    // UNRELATED body revision must succeed, leave the quarantined bytes
    // untouched, and keep the quarantine census-visible.
    let tracked_plain = r#"<w:p><w:r><w:t xml:space="preserve">Other </w:t></w:r><w:ins w:id="7" w:author="C" w:date="2026-03-01T00:00:00Z"><w:r><w:t>visible</w:t></w:r></w:ins></w:p>"#;
    let bytes = make_docx_with_body(&format!("{MOVE_MIX_P}{tracked_plain}"));
    let doc = Document::parse(&bytes).expect("parse");

    let projected = doc
        .project(Resolution::Selective {
            ids: std::collections::HashSet::from([7u32]),
            action: ResolveSelectionAction::Accept,
        })
        .expect("selective resolution of a visible id must work despite the quarantine");

    // The selected insertion is accepted: its text stays, its marker is gone.
    let out = projected
        .serialize(&stemma::ExportOptions::default())
        .expect("serialize");
    let xml = document_xml_of(&out);
    assert!(xml.contains("visible"), "accepted text stays");
    assert!(
        !xml.contains(r#"<w:ins w:id="7""#),
        "the accepted insertion's marker is resolved away"
    );

    // The quarantined block is untouched: its bytes (the unsupported nested
    // mix) survive verbatim and the placeholder still reads as quarantined.
    assert!(
        xml.contains(r#"<w:moveTo w:id="3""#) && xml.contains(r#"<w:ins w:id="4""#),
        "quarantined bytes carried through un-resolved"
    );
    let view = projected.read();
    assert_eq!(
        view.blocks[0].opaque_label.as_deref(),
        Some("quarantined_nested_tracked_changes"),
        "the quarantine stays disclosed on the read view"
    );
}

#[test]
fn selective_resolution_still_refuses_ids_the_census_never_minted() {
    // The completeness check is the quarantine's enforcement line: an id
    // that matches no visible carrier — including anything living inside
    // quarantined bytes (e.g. the nested w:ins w:id="4") — refuses loud.
    let bytes = make_docx_with_body(MOVE_MIX_P);
    let doc = Document::parse(&bytes).expect("parse");

    let err = doc
        .project(Resolution::Selective {
            ids: std::collections::HashSet::from([4u32]),
            action: ResolveSelectionAction::Accept,
        })
        .err()
        .expect("an id living only inside quarantined bytes must refuse");
    assert_eq!(err.code, stemma::ErrorCode::InvalidRange, "{err:?}");
}

#[test]
fn accept_all_and_reject_all_reads_keep_working_and_keep_the_placeholder() {
    let bytes = make_docx_with_body(&format!("{MOVE_MIX_P}{PLAIN_P}"));
    let doc = Document::parse(&bytes).expect("parse");

    let accepted = doc.read_accepted().expect("accept-all read works").read();
    assert_eq!(
        accepted.blocks[0].role,
        stemma::view::BlockRole::Opaque,
        "the placeholder survives the accept-all projection un-resolved"
    );
    let rejected = doc.read_rejected().expect("reject-all read works").read();
    assert_eq!(rejected.blocks[0].role, stemma::view::BlockRole::Opaque);
}

#[test]
fn compare_refuses_when_either_input_contains_a_quarantined_block() {
    use stemma::{DocxRuntime, SimpleRuntime, TransactionMeta};

    let quarantined = make_docx_with_body(&format!("{MOVE_MIX_P}{PLAIN_P}"));
    let clean = make_docx_with_body(PLAIN_P);
    let runtime = SimpleRuntime::new();
    let base = runtime.import_docx(&quarantined).expect("import base");
    let target = runtime.import_docx(&clean).expect("import target");
    let meta = TransactionMeta {
        author: "quarantine-test".to_string(),
        reason: None,
        timestamp_utc: None,
    };

    let err = runtime
        .compare_and_redline(&base.doc_handle, &target.doc_handle, meta.clone())
        .err()
        .expect("compare with a quarantined base must refuse");
    assert!(err.message.contains("quarantined"), "{}", err.message);

    let err = runtime
        .compare_and_redline(&target.doc_handle, &base.doc_handle, meta)
        .err()
        .expect("compare with a quarantined target must refuse");
    assert!(err.message.contains("quarantined"), "{}", err.message);
}

// ─── Story boundaries ────────────────────────────────────────────────────────

/// A textbox's content (`w:txbxContent`) is a SEPARATE STORY: tracked-change
/// nesting rules apply per story (ECMA-376 §17.13.5 constrains a revision's
/// ancestors within its own story), so an `<w:ins>` inside a textbox inside an
/// inserted run is legal OOXML — real corpus documents carry this shape. The
/// nesting scan must not cross the boundary: no refusal, no quarantine. The
/// drawing itself is an opaque widget, preserved verbatim.
#[test]
fn tracked_change_inside_a_textbox_story_is_not_nesting() {
    let textbox_p = r#"<w:p><w:ins w:id="1" w:author="AuthorA" w:date="2026-01-01T00:00:00Z"><w:r><w:pict><v:shape xmlns:v="urn:schemas-microsoft-com:vml"><v:textbox><w:txbxContent><w:p><w:ins w:id="2" w:author="AuthorB" w:date="2026-01-02T00:00:00Z"><w:r><w:t>boxed insertion</w:t></w:r></w:ins></w:p></w:txbxContent></v:textbox></v:shape></w:pict></w:r><w:r><w:t xml:space="preserve">outer inserted text</w:t></w:r></w:ins></w:p>"#;
    let bytes = make_docx_with_body(&format!("{textbox_p}{PLAIN_P}"));
    let doc = Document::parse(&bytes)
        .expect("an insertion within a textbox story inside an inserted run is legal");

    let view = doc.read();
    assert_eq!(view.blocks.len(), 2);
    // The paragraph is a real tracked paragraph, not a quarantine placeholder.
    assert!(view.blocks[0].opaque_label.is_none(), "not quarantined");
    assert!(view.blocks[0].text.contains("outer inserted text"));

    // Byte-faithful roundtrip: the textbox story (including its inner
    // revision) survives verbatim inside the opaque drawing.
    let out = doc
        .serialize(&stemma::ExportOptions::default())
        .expect("serialize");
    let xml = document_xml_of(&out);
    assert!(xml.contains("boxed insertion"), "textbox story preserved");
    assert!(
        xml.contains(r#"<w:ins w:id="2" w:author="AuthorB""#),
        "inner story revision preserved verbatim"
    );
}

/// Negative control for the boundary rule: REAL same-type nesting in the SAME
/// story still refuses at the entry door.
#[test]
fn same_type_nesting_in_the_same_story_still_refuses() {
    let bad = r#"<w:p><w:ins w:id="1" w:author="A" w:date="2026-01-01T00:00:00Z"><w:ins w:id="2" w:author="B" w:date="2026-01-02T00:00:00Z"><w:r><w:t>doubly inserted</w:t></w:r></w:ins></w:ins></w:p>"#;
    let bytes = make_docx_with_body(&format!("{bad}{PLAIN_P}"));
    let err = match Document::parse(&bytes) {
        Err(e) => e,
        Ok(_) => panic!("same-type nesting in one story must refuse"),
    };
    assert!(err.message.contains("same-type nesting"), "{}", err.message);
}
