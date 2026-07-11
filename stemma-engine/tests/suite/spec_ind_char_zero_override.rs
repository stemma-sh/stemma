//! Character-unit indentation (`w:ind` @leftChars/@rightChars/@firstLineChars/
//! @hangingChars, §17.3.1.12) must round-trip — including an explicit `="0"`.
//!
//! CT_Ind carries both twips attributes (left/right/firstLine/hanging) and their
//! character-unit siblings (the East Asian layout variant). Real
//! Word-authored documents — CJK ones routinely — write `w:leftChars="0"` on a
//! numbered paragraph to CANCEL a character indent it would otherwise inherit
//! from its numbering or style (MS-OI29500 2.1.44a). The engine used to read
//! only the strict-schema `startChars`/`endChars` names and to filter zero
//! values as "not specified"; a wild witness carrying only `<w:ind
//! w:leftChars="0"/>` was therefore modelled as "no indent", so a
//! whole-document rebuild (e.g. an edit to a DIFFERENT paragraph) re-emitted its
//! pPr with no `w:ind` at all. Word then re-applied the inherited character
//! indent and the untouched paragraph's left margin visibly jumped.
//!
//! These tests pin the domain rule: the transitional `leftChars`/`rightChars`
//! aliases are read, and an explicit `="0"` is a first-class override that is
//! preserved and re-emitted. Hermetic in-memory `.docx` (no corpus fixtures) so
//! it runs daily.

use std::io::{Cursor, Read, Write};

use stemma::api::Document;
use stemma::edit::*;
use stemma::{DocxRuntime, ExportOptions, RevisionInfo, SimpleRuntime};
use zip::ZipWriter;
use zip::write::FileOptions;

use crate::common;

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
    zip.finish().unwrap().into_inner()
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

/// Import in-memory `.docx` bytes to a canonical document so paragraph model
/// values (e.g. `indent.start_chars`) can be inspected directly.
fn import_bytes(bytes: &[u8]) -> stemma::CanonDoc {
    let runtime = SimpleRuntime::new();
    let import = runtime.import_docx(bytes).expect("import in-memory docx");
    let view = runtime.view(&import.doc_handle).expect("view");
    std::sync::Arc::unwrap_or_clone(view.canonical)
}

// ────────────────────────────────────────────────────────────────────────────
// (a) Round-trip: leftChars="0" + firstLineChars="200" survive parse→serialize,
//     both in the model and on re-emit.
// ────────────────────────────────────────────────────────────────────────────

#[test]
fn leftchars_zero_and_firstlinechars_roundtrip() {
    let body = r#"
    <w:p>
      <w:pPr>
        <w:ind w:leftChars="0" w:firstLineChars="200"/>
      </w:pPr>
      <w:r><w:t>Alpha</w:t></w:r>
    </w:p>"#;
    let docx = build_docx(&wrap_body(body));

    // Model: the explicit zero is preserved (Some(0), distinct from None), and
    // firstLineChars is kept as its own value.
    let doc = import_bytes(&docx);
    let paras = common::all_paragraphs(&doc);
    let ind = paras[0]
        .indent
        .as_ref()
        .expect("paragraph must carry an indent");
    assert_eq!(
        ind.start_chars,
        Some(0),
        "explicit leftChars=0 must be preserved as Some(0), not dropped to None"
    );
    assert_eq!(
        ind.first_line_chars,
        Some(200),
        "firstLineChars=200 must be preserved"
    );

    // Re-emit: w:ind carries both chars attrs (transitional leftChars name).
    let parsed = Document::parse(&docx).expect("parse");
    let out = parsed
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let xml = document_xml_of(&out);
    assert!(
        xml.contains(r#"w:leftChars="0""#),
        "re-emitted w:ind must keep the explicit leftChars=0: {xml}"
    );
    assert!(
        xml.contains(r#"w:firstLineChars="200""#),
        "re-emitted w:ind must keep firstLineChars=200: {xml}"
    );

    // And it survives a second import (the value is stable, not first-pass only).
    let reimported = import_bytes(&out);
    let reparas = common::all_paragraphs(&reimported);
    let reind = reparas[0].indent.as_ref().expect("reimport indent");
    assert_eq!(
        reind.start_chars,
        Some(0),
        "leftChars=0 stable across a round-trip"
    );
    assert_eq!(
        reind.first_line_chars,
        Some(200),
        "firstLineChars stable across a round-trip"
    );
}

// ────────────────────────────────────────────────────────────────────────────
// (c) Precedence is representable: chars and twips values are stored distinctly,
//     never merged. `left="720"` and `leftChars="0"` coexist in the model.
// ────────────────────────────────────────────────────────────────────────────

#[test]
fn twips_and_chars_indent_stored_distinctly() {
    let body = r#"
    <w:p>
      <w:pPr>
        <w:ind w:left="720" w:leftChars="0"/>
      </w:pPr>
      <w:r><w:t>Beta</w:t></w:r>
    </w:p>"#;
    let docx = build_docx(&wrap_body(body));
    let doc = import_bytes(&docx);
    let paras = common::all_paragraphs(&doc);
    let ind = paras[0].indent.as_ref().expect("indent present");
    // Both are kept side by side — no lossy merge into a single "effective" value.
    // A consumer applies the precedence rule (non-zero chars wins; leftChars=0
    // means fall back to the twip left) without the parser having discarded either.
    assert_eq!(ind.left, Some(720), "twip left=720 must be preserved");
    assert_eq!(
        ind.start_chars,
        Some(0),
        "leftChars=0 must be preserved distinctly from the twip value"
    );
}

// ────────────────────────────────────────────────────────────────────────────
// (b) Rebuild survival (the witness): editing a DIFFERENT paragraph and
//     re-serializing the whole document must NOT strip an untouched paragraph's
//     explicit leftChars=0.
// ────────────────────────────────────────────────────────────────────────────

#[test]
fn untouched_paragraph_keeps_leftchars_zero_after_unrelated_edit() {
    // P0: the witness — a paragraph whose only direct indent is leftChars=0.
    // P1: an ordinary paragraph we will edit, forcing a full re-serialize.
    let body = r#"
    <w:p>
      <w:pPr>
        <w:ind w:leftChars="0"/>
      </w:pPr>
      <w:r><w:t>Alpha</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>Confidential</w:t></w:r>
    </w:p>"#;
    let docx = build_docx(&wrap_body(body));
    let doc = Document::parse(&docx).expect("parse");

    let view = doc.read();
    let target = view.blocks.get(1).expect("second paragraph").id.clone();
    let txn = EditTransaction {
        steps: vec![EditStep::SetRunFormatting {
            block_id: target,
            expect: "Confidential".to_string(),
            semantic_hash: None,
            marks: InlineMarkSet {
                bold: true,
                ..Default::default()
            },
            style: RunStyleEdit::default(),
            rationale: None,
        }],
        summary: Some("unrelated edit to a different paragraph".to_string()),
        materialization_mode: MaterializationMode::Direct,
        revision: RevisionInfo {
            revision_id: 1,
            author: Some("Spec".to_string()),
            date: Some("2026-07-06T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    };
    let edited = doc.apply(&txn).expect("apply unrelated edit");
    let out = edited
        .serialize(&ExportOptions::default())
        .expect("serialize");
    let xml = document_xml_of(&out);

    // Sanity: the edit landed (real serialize path, not passthrough).
    assert!(
        xml.contains("<w:b") && xml.contains("Confidential"),
        "sanity: the unrelated bold edit must be present: {xml}"
    );
    // The untouched paragraph still carries its explicit leftChars=0, so Word
    // will not re-apply an inherited character indent.
    let alpha = xml.find("Alpha").expect("Alpha paragraph present");
    let ppr_start = xml[..alpha].rfind("<w:pPr>").expect("Alpha's pPr");
    let ppr = &xml[ppr_start..alpha];
    assert!(
        ppr.contains(r#"w:leftChars="0""#),
        "untouched paragraph must keep its explicit leftChars=0 after an unrelated \
         rebuild (dropping it would let Word re-apply the inherited char indent): {ppr}"
    );
}
