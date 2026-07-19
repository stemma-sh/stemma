//! Round-trip and reject fidelity for the pPrChange previous-pPr
//! disciplined-preservation remainder (`domain::PreservedProp`).
//!
//! `w:pPrChange` (§17.13.5.29) requires the inner w:pPr to be a COMPLETE
//! snapshot of the previous paragraph properties. `extract_ppr_change` only
//! ever parsed four of them (jc/ind/spacing/rPr) with direct `extract_*`
//! calls — any other inner-pPr child (e.g. w:suppressLineNumbers, w:keepNext,
//! w:pBdr) was silently dropped, and reject only restored the four modeled
//! props. This file pins the fix: such children are captured verbatim at
//! import, survive re-serialization, and are restored onto the paragraph's
//! own pPr remainder on reject.
//!
//! Uses a hermetic in-memory `.docx` (no corpus fixtures) so this runs daily.

use std::io::{Cursor, Read, Write};

use stemma::RevisionKind;
use stemma::api::Document;
use stemma::edit::*;
use stemma::runtime::Resolution;
use stemma::tracked_model::enumerate_revisions;
use stemma::{ExportOptions, RevisionInfo};
use zip::ZipWriter;
use zip::write::FileOptions;

// ── DOCX builder helpers (same minimal-package style as
//    spec_rpr_preserved_remainder.rs; kept local so this file is
//    self-contained) ──────────────────────────────────────────────────────

const CONTENT_TYPES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

const RELS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

const WORD_RELS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
</Relationships>"#;

fn build_docx(document_xml: &str) -> Vec<u8> {
    let cursor = Cursor::new(Vec::new());
    let mut zip = ZipWriter::new(cursor);
    let options = FileOptions::default();

    zip.start_file("[Content_Types].xml", options).unwrap();
    zip.write_all(CONTENT_TYPES_XML.as_bytes()).unwrap();
    zip.start_file("_rels/.rels", options).unwrap();
    zip.write_all(RELS_XML.as_bytes()).unwrap();
    zip.start_file("word/_rels/document.xml.rels", options)
        .unwrap();
    zip.write_all(WORD_RELS_XML.as_bytes()).unwrap();
    zip.start_file("word/document.xml", options).unwrap();
    zip.write_all(document_xml.as_bytes()).unwrap();

    let cursor = zip.finish().unwrap();
    cursor.into_inner()
}

/// A paragraph carrying a pre-existing `w:pPrChange` (as Word would have
/// written it) whose previous pPr mixes MODELED children (jc, ind — with
/// character-unit indent attrs to also pin the indentation-narrowing fix)
/// with UNMODELED ones this parser has no typed field for:
/// `w:suppressLineNumbers`, `w:keepNext`, and a `w:pBdr` block.
fn preserved_bearing_body() -> &'static str {
    r#"
    <w:p>
      <w:pPr>
        <w:jc w:val="center"/>
        <w:ind w:left="720"/>
        <w:pPrChange w:id="99" w:author="Word User" w:date="2026-06-01T00:00:00Z">
          <w:pPr>
            <w:suppressLineNumbers/>
            <w:keepNext/>
            <w:pBdr>
              <w:top w:val="single" w:sz="4" w:space="1" w:color="auto"/>
            </w:pBdr>
            <w:jc w:val="left"/>
            <w:ind w:left="1440" w:startChars="200" w:endChars="100" w:firstLineChars="50"/>
          </w:pPr>
        </w:pPrChange>
      </w:pPr>
      <w:r><w:t>Untouched pPrChange paragraph.</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>The Confidential Information is protected.</w:t></w:r>
    </w:p>"#
}

fn wrap_body(body_content: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006"
            xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml"
            mc:Ignorable="w14">
  <w:body>
{body_content}
    <w:sectPr/>
  </w:body>
</w:document>"#
    )
}

fn document_xml_of(docx: &[u8]) -> String {
    let mut z = zip::ZipArchive::new(Cursor::new(docx.to_vec())).expect("zip");
    let mut f = z.by_name("word/document.xml").expect("document.xml");
    let mut xml = String::new();
    f.read_to_string(&mut xml).expect("read");
    xml
}

/// Byte offset of the first occurrence of `needle` in `xml`.
fn pos(xml: &str, needle: &str) -> usize {
    xml.find(needle)
        .unwrap_or_else(|| panic!("'{needle}' not found in: {xml}"))
}

fn unrelated_edit_txn(target_block: stemma::domain::NodeId) -> EditTransaction {
    EditTransaction {
        steps: vec![EditStep::SetRunFormatting {
            block_id: target_block,
            expect: "Confidential".to_string(),
            semantic_hash: None,
            marks: InlineMarkSet {
                bold: true,
                ..Default::default()
            },
            style: RunStyleEdit::default(),
            rationale: None,
        }],
        summary: Some("unrelated formatting edit".to_string()),
        materialization_mode: MaterializationMode::Direct,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("Spec".to_string()),
            date: Some("2026-07-02T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

// ════════════════════════════════════════════════════════════════════════════
// (a) Import -> unrelated tracked edit elsewhere -> serialize: the untouched
//     paragraph's pPrChange still carries its unmodeled previous-pPr
//     children, verbatim, in Annex-A order.
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn preserved_ppr_change_children_survive_an_unrelated_edit() {
    let docx = build_docx(&wrap_body(preserved_bearing_body()));
    let doc = Document::parse(&docx).expect("parse preserved-bearing doc");

    let view = doc.read();
    let target_block = view.blocks.get(1).expect("second paragraph").id.clone();
    let edited = doc.apply(&unrelated_edit_txn(target_block)).expect("apply");
    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let xml = document_xml_of(&bytes);

    // Sanity: the unrelated edit actually landed (real edit + serialize path,
    // not scaffold passthrough — the no-edit path re-zips and proves nothing).
    assert!(
        xml.contains("<w:b") && xml.contains("Confidential"),
        "sanity: the unrelated bold edit must be present in the output: {xml}"
    );

    let ppr_change_pos = pos(&xml, "<w:pPrChange");
    let inner_ppr_end = xml[ppr_change_pos..]
        .find("</w:pPrChange>")
        .map(|i| ppr_change_pos + i)
        .expect("pPrChange close tag");
    let inner = &xml[ppr_change_pos..inner_ppr_end];

    assert!(
        inner.contains("w:suppressLineNumbers"),
        "preserved w:suppressLineNumbers must survive inside pPrChange's inner pPr: {inner}"
    );
    assert!(
        inner.contains("w:keepNext"),
        "preserved w:keepNext must survive inside pPrChange's inner pPr: {inner}"
    );
    assert!(
        inner.contains("w:pBdr") && inner.contains(r#"w:color="auto""#),
        "preserved w:pBdr (with its edge attrs) must survive inside pPrChange's inner pPr: {inner}"
    );

    // Annex-A order (PPR_ORDER): suppressLineNumbers precedes pBdr precedes
    // spacing/ind/jc — here pinned as suppressLineNumbers before pBdr before jc.
    let sln_pos = pos(inner, "w:suppressLineNumbers");
    let pbdr_pos = pos(inner, "w:pBdr");
    let jc_pos = pos(inner, "w:jc");
    assert!(
        sln_pos < pbdr_pos && pbdr_pos < jc_pos,
        "preserved children must land at their Annex-A position relative to \
         the modeled jc: {inner}"
    );

    // The indentation-narrowing fix: character-unit indent attrs on the
    // PREVIOUS pPr must round-trip too (not just twips).
    // Emitted with the transitional names (leftChars/rightChars) to match the
    // w:left twips the serializer normalizes to; leftChars is the alias of the
    // input's startChars.
    assert!(
        inner.contains(r#"w:leftChars="200""#)
            && inner.contains(r#"w:rightChars="100""#)
            && inner.contains(r#"w:firstLineChars="50""#),
        "previous indentation's character-unit attrs must survive (not be \
         narrowed to None): {inner}"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// (b) Reimport after the edit reconstructs the same preserved content — not
//     just present as a text substring, but present on reimport.
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn preserved_ppr_change_children_survive_reimport_after_unrelated_edit() {
    let docx = build_docx(&wrap_body(preserved_bearing_body()));
    let doc = Document::parse(&docx).expect("parse");
    let view = doc.read();
    let target_block = view.blocks.get(1).expect("second paragraph").id.clone();
    let edited = doc.apply(&unrelated_edit_txn(target_block)).expect("apply");
    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");

    let reimported = Document::parse(&bytes).expect("reimport must succeed");
    let re_bytes = reimported
        .serialize(&ExportOptions::default())
        .expect("reserialize");
    let re_xml = document_xml_of(&re_bytes);
    assert!(
        re_xml.contains("w:suppressLineNumbers"),
        "preserved w:suppressLineNumbers must survive a second round-trip: {re_xml}"
    );
    assert!(
        re_xml.contains("w:pBdr"),
        "preserved w:pBdr must survive a second round-trip: {re_xml}"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// (c) Reject: the tracked change is removed, and its previous-pPr preserved
//     remainder (plus the character-unit indentation) lands on the restored
//     paragraph's OWN pPr.
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn rejecting_a_ppr_change_restores_its_preserved_children_onto_the_paragraph() {
    let docx = build_docx(&wrap_body(preserved_bearing_body()));
    let doc = Document::parse(&docx).expect("parse");

    let resolved = doc
        .project(Resolution::RejectAll)
        .expect("reject-all must succeed");
    let bytes = resolved
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let xml = document_xml_of(&bytes);

    // The tracked-change record itself is gone.
    assert!(
        !xml.contains("w:pPrChange"),
        "reject-all must remove the pPrChange record: {xml}"
    );

    // The restored paragraph's OWN pPr carries what the snapshot said —
    // including the previously-unmodeled children.
    let p_start = pos(&xml, "Untouched pPrChange paragraph");
    let ppr_start = xml[..p_start]
        .rfind("<w:pPr>")
        .expect("paragraph's own pPr");
    let ppr_end = xml[ppr_start..]
        .find("</w:pPr>")
        .map(|i| ppr_start + i)
        .expect("pPr close");
    let restored_ppr = &xml[ppr_start..ppr_end];

    assert!(
        restored_ppr.contains("w:suppressLineNumbers"),
        "reject must restore preserved w:suppressLineNumbers onto the paragraph's pPr: {restored_ppr}"
    );
    assert!(
        restored_ppr.contains("w:keepNext"),
        "reject must restore preserved w:keepNext onto the paragraph's pPr: {restored_ppr}"
    );
    assert!(
        restored_ppr.contains("w:pBdr"),
        "reject must restore preserved w:pBdr onto the paragraph's pPr: {restored_ppr}"
    );
    assert!(
        restored_ppr.contains(r#"w:val="left""#),
        "reject must restore the previous (left) alignment: {restored_ppr}"
    );
    assert!(
        restored_ppr.contains(r#"w:leftChars="200""#)
            && restored_ppr.contains(r#"w:rightChars="100""#),
        "reject must restore the previous character-unit indentation \
         (the narrowing fix), not drop it to twips-only: {restored_ppr}"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// (d) Accept: the snapshot (and its preserved remainder) is discarded
//     entirely — it must not leak onto the accepted paragraph.
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn accepting_a_ppr_change_discards_its_preserved_remainder() {
    let docx = build_docx(&wrap_body(preserved_bearing_body()));
    let doc = Document::parse(&docx).expect("parse");

    let resolved = doc
        .project(Resolution::AcceptAll)
        .expect("accept-all must succeed");
    let bytes = resolved
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let xml = document_xml_of(&bytes);

    assert!(
        !xml.contains("w:pPrChange"),
        "accept-all must remove the pPrChange record: {xml}"
    );
    // The CURRENT (post-change) state has neither suppressLineNumbers nor
    // keepNext nor pBdr authored on it — those only ever lived in the
    // snapshot, so accept (which just discards the record) must not
    // resurrect them.
    assert!(
        !xml.contains("w:suppressLineNumbers"),
        "accept must not leak the snapshot's preserved remainder onto the \
         accepted paragraph: {xml}"
    );
    assert!(
        !xml.contains("w:pBdr"),
        "accept must not leak the snapshot's preserved pBdr onto the \
         accepted paragraph: {xml}"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// (e) Sanity: the revision is enumerable like any other formatting change
//     (unaffected by the preserved-remainder fix, but confirms the fixture
//     imports as a real tracked change and not silently-dropped garbage).
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn the_preserved_bearing_ppr_change_is_enumerable() {
    let docx = build_docx(&wrap_body(preserved_bearing_body()));
    let doc = Document::parse(&docx).expect("parse");
    let records = enumerate_revisions(&doc.snapshot().canonical);
    let fmt: Vec<_> = records
        .iter()
        .filter(|r| r.kind == RevisionKind::FormatParagraph)
        .collect();
    assert_eq!(
        fmt.len(),
        1,
        "the pPrChange must enumerate as exactly one formatting revision: {records:?}"
    );
    // H7: it carries a real minted identity (the resolvable handle), never the
    // legacy 0 sentinel — the wire id 99 from the fixture is not the handle.
    assert_ne!(
        fmt[0].revision_id, 0,
        "an imported tracked change is enumerable with a real identity: {records:?}"
    );
}
