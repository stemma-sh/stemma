//! Round-trip fidelity for the rPr disciplined-preservation remainder
//! (`domain::PreservedProp`).
//!
//! Import must not silently lose document content: an rPr child element this
//! engine does not model (e.g. `w:eastAsianLayout`, or a foreign-namespace
//! extension like `w14:glow`) is captured verbatim at import and re-emitted
//! at its Annex-A position on serialization — the same guarantee structural
//! content already has via `AtomKind::Widget { raw_xml }`.
//!
//! Uses a hermetic in-memory `.docx` (no corpus fixtures) so this runs daily.

use std::io::{Cursor, Read, Write};

use stemma::api::Document;
use stemma::edit::*;
use stemma::runtime::{ValidatorLevel, gate_serialized_bytes};
use stemma::{ExportOptions, RevisionInfo};
use zip::ZipWriter;
use zip::write::FileOptions;

// ── DOCX builder helpers (same minimal-package style as other hermetic
//    tracked-change tests; kept local so this file is self-contained) ────────

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

/// A paragraph whose single run's rPr carries an unmodeled `w:eastAsianLayout`
/// (a real Annex-A rPr child stemma doesn't model) AND a foreign-namespace
/// `w14:glow` extension with its own nested child — both must be captured and
/// survive an edit elsewhere in the document, untouched.
fn preserved_bearing_body() -> &'static str {
    r#"
    <w:p>
      <w:r>
        <w:rPr>
          <w:eastAsianLayout w:combine="1"/>
          <w14:glow w14:rad="63500"><w14:srgbClr w14:val="4F81BD"/></w14:glow>
        </w:rPr>
        <w:t>Untouched preserved run.</w:t>
      </w:r>
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
//     run's preserved rPr children survive, in Annex-A order, and the output
//     passes the Full (schema-ordering) validator gate.
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn preserved_rpr_children_survive_an_unrelated_edit_and_validate_full() {
    let docx = build_docx(&wrap_body(preserved_bearing_body()));
    let doc = Document::parse(&docx).expect("parse preserved-bearing doc");

    // The SECOND paragraph's block id — the target of an edit that has
    // nothing to do with the first paragraph's rPr content.
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

    // Both preserved children survive on the UNTOUCHED run, verbatim content.
    assert!(
        xml.contains("w:eastAsianLayout") && xml.contains(r#"w:combine="1""#),
        "preserved w:eastAsianLayout must survive re-serialization: {xml}"
    );
    assert!(
        xml.contains("w14:glow")
            && xml.contains(r#"w14:rad="63500""#)
            && xml.contains("w14:srgbClr")
            && xml.contains(r#"w14:val="4F81BD""#),
        "preserved w14:glow (with its nested child) must survive re-serialization: {xml}"
    );

    // Annex-A order: w:eastAsianLayout sits between w:rFonts-less run's other
    // siblings — here, simply pinned as preceding the (order-agnostic,
    // appended-at-end) foreign w14:glow.
    let ea_pos = pos(&xml, "w:eastAsianLayout");
    let glow_pos = pos(&xml, "w14:glow");
    assert!(
        ea_pos < glow_pos,
        "table-known w:eastAsianLayout (Annex A position) must precede the \
         unrecognized-name w14:glow (appended at end of rPr): {xml}"
    );

    // The output is schema-ordering-valid at the strictest gate: this is the
    // proof that placement (not just presence) is Annex-A-correct.
    gate_serialized_bytes(&bytes, ValidatorLevel::Full)
        .expect("output with re-emitted preserved rPr children must pass Full validation");
}

// ════════════════════════════════════════════════════════════════════════════
// (b) Re-parsing the exported bytes reconstructs the SAME preserved content
//     (not just present as a text substring, but present on reimport).
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn preserved_rpr_children_survive_reimport_after_unrelated_edit() {
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
    // and the reimported document must still carry both preserved children.
    let reimported = Document::parse(&bytes).expect("reimport must succeed");
    let re_bytes = reimported
        .serialize(&ExportOptions::default())
        .expect("reserialize");
    let re_xml = document_xml_of(&re_bytes);
    assert!(
        re_xml.contains("w:eastAsianLayout"),
        "preserved w:eastAsianLayout must survive a second round-trip: {re_xml}"
    );
    assert!(
        re_xml.contains("w14:glow"),
        "preserved w14:glow must survive a second round-trip: {re_xml}"
    );
}
