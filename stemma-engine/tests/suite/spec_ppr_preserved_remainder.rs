//! Round-trip fidelity for the pPr disciplined-preservation remainder
//! (`domain::ParagraphNode::preserved_ppr`).
//!
//! Import must not silently lose document content: a pPr child element this
//! engine does not model (e.g. `w:suppressLineNumbers`, `w:kinsoku`, or a
//! foreign-namespace extension) is captured verbatim at import and re-emitted
//! at its Annex-A position on serialization — the same guarantee the rPr
//! remainder has (see `spec_rpr_preserved_remainder.rs`) and structural
//! content already has via `AtomKind::Widget { raw_xml }`.
//!
//! Uses a hermetic in-memory `.docx` (no corpus fixtures) so this runs daily.

use std::io::{Cursor, Read, Write};

use stemma::api::Document;
use stemma::domain::{CanonDoc, NodeId};
use stemma::edit::*;
use stemma::runtime::{ValidatorLevel, gate_serialized_bytes};
use stemma::{ExportOptions, RevisionInfo, accept_all, reject_all_with_styles};
use zip::ZipWriter;
use zip::write::FileOptions;

// ── DOCX builder helpers (same minimal-package style as
//    spec_rpr_preserved_remainder.rs; kept local so this file is
//    self-contained) ─────────────────────────────────────────────────────────

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

/// A paragraph whose pPr carries an unmodeled `w:suppressLineNumbers` and
/// `w:kinsoku` (real Annex-A pPr children stemma doesn't model) AND a
/// foreign-namespace extension — all three must be captured and survive an
/// edit elsewhere in the document, untouched.
fn preserved_bearing_body() -> &'static str {
    r#"
    <w:p>
      <w:pPr>
        <w:suppressLineNumbers/>
        <w:kinsoku w:val="0"/>
        <w14:customPPr w14:val="1"/>
      </w:pPr>
      <w:r><w:t>Untouched preserved paragraph.</w:t></w:r>
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

// ════════════════════════════════════════════════════════════════════════════
// (a) Import -> unrelated tracked edit elsewhere -> serialize: the untouched
//     paragraph's preserved pPr children survive, in Annex-A order, and the
//     output passes the Full (schema-ordering) validator gate.
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn preserved_ppr_children_survive_an_unrelated_edit_and_validate_full() {
    let docx = build_docx(&wrap_body(preserved_bearing_body()));
    let doc = Document::parse(&docx).expect("parse preserved-bearing doc");

    // The SECOND paragraph's block id — the target of an edit that has
    // nothing to do with the first paragraph's pPr content.
    let view = doc.read();
    let target_block = view.blocks.get(1).expect("second paragraph").id.clone();

    let txn = EditTransaction {
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
            author: Some("Spec".to_string()),
            date: Some("2026-07-02T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    };

    let edited = doc.apply(&txn).expect("apply unrelated edit");
    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");

    // The unrelated edit actually landed (sanity check we exercised the real
    // edit + serialize_canonical_docx path, not scaffold passthrough).
    let xml = document_xml_of(&bytes);
    assert!(
        xml.contains("<w:b") && xml.contains("Confidential"),
        "sanity: the unrelated bold edit must be present in the output: {xml}"
    );

    // All three preserved children survive on the UNTOUCHED paragraph, verbatim.
    assert!(
        xml.contains("w:suppressLineNumbers"),
        "preserved w:suppressLineNumbers must survive re-serialization: {xml}"
    );
    assert!(
        xml.contains("w:kinsoku") && xml.contains(r#"w:val="0""#),
        "preserved w:kinsoku must survive re-serialization: {xml}"
    );
    assert!(
        xml.contains("w14:customPPr") && xml.contains(r#"w14:val="1""#),
        "preserved foreign w14:customPPr must survive re-serialization: {xml}"
    );

    // Annex-A order: w:suppressLineNumbers (position 7) precedes w:kinsoku
    // (position 12); the foreign, unrecognized-name w14:customPPr is
    // appended at the end of pPr, after both.
    let sln_pos = pos(&xml, "w:suppressLineNumbers");
    let kinsoku_pos = pos(&xml, "w:kinsoku");
    let custom_pos = pos(&xml, "w14:customPPr");
    assert!(
        sln_pos < kinsoku_pos,
        "table-known w:suppressLineNumbers (Annex A position 7) must precede \
         table-known w:kinsoku (Annex A position 12): {xml}"
    );
    assert!(
        kinsoku_pos < custom_pos,
        "table-known w:kinsoku must precede the unrecognized-name w14:customPPr \
         (appended at end of pPr): {xml}"
    );

    // The output is schema-ordering-valid at the strictest gate: this is the
    // proof that placement (not just presence) is Annex-A-correct.
    gate_serialized_bytes(&bytes, ValidatorLevel::Full)
        .expect("output with re-emitted preserved pPr children must pass Full validation");
}

// ════════════════════════════════════════════════════════════════════════════
// (b) Re-parsing the exported bytes reconstructs the SAME preserved content
//     (not just present as a text substring, but present on reimport).
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn preserved_ppr_children_survive_reimport_after_unrelated_edit() {
    let docx = build_docx(&wrap_body(preserved_bearing_body()));
    let doc = Document::parse(&docx).expect("parse");
    let view = doc.read();
    let target_block = view.blocks.get(1).expect("second paragraph").id.clone();

    let txn = EditTransaction {
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
        summary: None,
        materialization_mode: MaterializationMode::Direct,
        revision: RevisionInfo {
            revision_id: 1,
            author: Some("Spec".to_string()),
            date: Some("2026-07-02T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    };
    let edited = doc.apply(&txn).expect("apply");
    let bytes = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");

    // Reimporting the exported bytes must succeed (not just look right as text)
    // and the reimported document must still carry all three preserved children.
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
        re_xml.contains("w:kinsoku"),
        "preserved w:kinsoku must survive a second round-trip: {re_xml}"
    );
    assert!(
        re_xml.contains("w14:customPPr"),
        "preserved w14:customPPr must survive a second round-trip: {re_xml}"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// (c) A tracked text edit ON the paragraph carrying preserved pPr props is
//     orthogonal formatting content: it must survive the edit, and survive
//     both accept-all and reject-all — the preserved remainder is not part
//     of what a text-only edit or a tracked-change resolution touches.
// ════════════════════════════════════════════════════════════════════════════

fn text_content(text: &str) -> ParagraphContent {
    ParagraphContent {
        fragments: vec![ContentFragment::Text(text.to_string())],
    }
}

fn preserved_ppr_of<'a>(
    canon: &'a CanonDoc,
    block_id: &NodeId,
) -> &'a [stemma::domain::PreservedProp] {
    for tb in &canon.blocks {
        if let stemma::domain::BlockNode::Paragraph(p) = &tb.block
            && &p.id == block_id
        {
            return &p.preserved_ppr;
        }
    }
    panic!("block {block_id:?} not found or not a paragraph");
}

#[test]
fn preserved_ppr_survives_a_tracked_text_edit_and_both_resolutions() {
    let docx = build_docx(&wrap_body(preserved_bearing_body()));
    let doc = Document::parse(&docx).expect("parse");
    let base: CanonDoc = (*doc.snapshot().canonical).clone();
    let block_id = doc.read().blocks[0].id.clone();

    let expected_names = vec![
        "w:suppressLineNumbers".to_string(),
        "w:kinsoku".to_string(),
        "w14:customPPr".to_string(),
    ];
    let base_names: Vec<String> = preserved_ppr_of(&base, &block_id)
        .iter()
        .map(|p| p.name.clone())
        .collect();
    assert_eq!(
        base_names, expected_names,
        "sanity: base paragraph must carry all three preserved pPr children"
    );

    let txn = EditTransaction {
        steps: vec![EditStep::ReplaceParagraphText {
            block_id: block_id.clone(),
            rationale: None,
            replacement_role: None,
            expect: "Untouched preserved paragraph.".to_string(),
            semantic_hash: None,
            content: text_content("Edited preserved paragraph."),
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 1,
            author: Some("Spec".to_string()),
            date: Some("2026-07-02T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    };

    let (edited, _pending) = apply_transaction(&base, &txn).expect("apply tracked text edit");
    let edited_names: Vec<String> = preserved_ppr_of(&edited, &block_id)
        .iter()
        .map(|p| p.name.clone())
        .collect();
    assert_eq!(
        edited_names, expected_names,
        "a tracked text edit on the paragraph must not disturb its preserved pPr remainder"
    );

    let mut accepted = edited.clone();
    accept_all(&mut accepted);
    let accepted_names: Vec<String> = preserved_ppr_of(&accepted, &block_id)
        .iter()
        .map(|p| p.name.clone())
        .collect();
    assert_eq!(
        accepted_names, expected_names,
        "accept_all must not drop the preserved pPr remainder"
    );

    let mut rejected = edited.clone();
    reject_all_with_styles(&mut rejected, None);
    let rejected_names: Vec<String> = preserved_ppr_of(&rejected, &block_id)
        .iter()
        .map(|p| p.name.clone())
        .collect();
    assert_eq!(
        rejected_names, expected_names,
        "reject_all must not drop the preserved pPr remainder"
    );
}
