//! Write-path consistency for a literal-prefix paragraph: what `read_block`
//! shows as a paragraph's text must be usable VERBATIM as a replace `expect`.
//!
//! Provenance: the read-surface fix (task #6, commit cb17adcf4) made the typed-in
//! enumeration label visible — `BlockView.text` for a hoisted `literal_prefix`
//! paragraph now reads `"1.\tEvents"`, matching what Word reads. But the WRITE
//! path's `expect` precondition is matched against `extract_text_sections`
//! (`edit/mod.rs`), which walks `para.segments` ONLY and so sees the body-only
//! `"Events"` — the prefix is in the `literal_prefix` field, not in a segment.
//!
//! That reintroduces, on the write path, exactly the invisible divergence task #6
//! exists to kill: an agent that copies its read (`"1.\tEvents"`) straight into
//! `expect` gets `ExpectMismatch`, because the gate compares against `"Events"`.
//! read and write must agree about the paragraph's text.
//!
//! The DESIRED behavior (this test): the read string round-trips as `expect`.
//! The approved task #8 contract: the `expect` matchable text becomes
//! prefix-aware (label-inclusive), so BOTH the prefix-inclusive read string and
//! the body-only form match as substrings — the agent's faithful copy of what it
//! read is accepted. (Separately, replacement `content` that re-types the label
//! is REFUSED with a teaching error; this test deliberately keeps `content`
//! body-only so it isolates the `expect` round-trip.) The fix belongs to the edit
//! seams (not the read view); this test is the gate that proves it.

use std::io::{Cursor, Write};

use stemma::domain::{NodeId, RevisionInfo};
use stemma::edit::{
    ContentFragment, EditStep, EditTransaction, MaterializationMode, ParagraphContent,
    apply_transaction,
};
use stemma::view::build_document_view_from_canon;
use stemma::{DocxRuntime, SimpleRuntime};
use zip::ZipWriter;
use zip::write::FileOptions;

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
    let options: FileOptions = FileOptions::default();
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

/// One paragraph whose visible text is `"1.\tEvents"` so the importer's
/// `strip_literal_prefix` hoists `"1."` into `ParagraphNode::literal_prefix` and
/// leaves `"Events"` as the body segment — the hoisted-prefix shape this test
/// is about. (`&#9;` is the tab the detector consumes as the prefix separator.)
fn paragraph_with_literal_prefix() -> Vec<u8> {
    let body = r#"
    <w:p>
      <w:pPr><w:pStyle w:val="ListParagraph"/></w:pPr>
      <w:r><w:t xml:space="preserve">1.&#9;Events</w:t></w:r>
    </w:p>"#;
    let doc = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
{body}
    <w:sectPr/>
  </w:body>
</w:document>"#
    );
    build_docx(&doc)
}

fn test_revision() -> RevisionInfo {
    RevisionInfo {
        revision_id: 100,
        author: Some("ReaderDocs".to_string()),
        date: Some("2026-06-12T00:00:00Z".to_string()),
        apply_op_id: None,
    }
}

#[test]
fn read_block_text_round_trips_as_replace_expect() {
    let docx = paragraph_with_literal_prefix();
    let rt = SimpleRuntime::new();
    let import = rt.import_docx(&docx).expect("import");
    let canon = import.canonical.as_ref().clone();

    // What an agent reads for this paragraph (the same projection read_block uses).
    let view = build_document_view_from_canon(&canon);
    let block = view
        .blocks
        .iter()
        .find(|b| b.text.contains("Events"))
        .expect("the literal-prefix paragraph is present");
    let block_id = block.id.to_string();
    let read_text = block.text.clone();

    // Precondition for this test to mean anything: the read DID surface the label
    // (that is the task #6 fix). If this regresses, the read side broke.
    assert_eq!(
        read_text, "1.\tEvents",
        "read_block must surface the restored label"
    );

    // The agent copies what it read verbatim into `expect` and rewrites the BODY
    // (the label stays attached on its own — the replacement is the new body text
    // only, so this isolates the `expect` round-trip and does NOT trip the
    // separate content-duplicates-label refuse rule that task #8 also lands).
    let tx = EditTransaction {
        steps: vec![EditStep::ReplaceParagraphText {
            block_id: NodeId::from(block_id.as_str()),
            rationale: None,
            replacement_role: None,
            // VERBATIM the read string — this is the round-trip under test.
            expect: read_text.clone(),
            semantic_hash: None,
            content: ParagraphContent {
                fragments: vec![ContentFragment::Text("Meetings".to_string())],
            },
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    let result = apply_transaction(&canon, &tx);
    assert!(
        result.is_ok(),
        "the text read_block returned ({read_text:?}) must be usable verbatim as \
         `expect` (the approved task #8 contract makes the expect matchable text \
         prefix-aware); the write path rejected it: {:?}",
        result.err()
    );
}

/// EXPECT IS A THIRD SURFACE on EVERY path that takes it: delete, like replace,
/// must accept the prefix-inclusive read text as its `expect`. The delete path
/// validates `expect` via `validate_expect_on_block`, which used to compare
/// against the body-only visible text and so rejected "1.\tEvents". The unified
/// prefix-aware rule (expect_matches_paragraph) fixes both paths identically.
#[test]
fn read_block_text_round_trips_as_delete_expect() {
    let docx = paragraph_with_literal_prefix();
    let rt = SimpleRuntime::new();
    let import = rt.import_docx(&docx).expect("import");
    let canon = import.canonical.as_ref().clone();

    let view = build_document_view_from_canon(&canon);
    let block = view
        .blocks
        .iter()
        .find(|b| b.text.contains("Events"))
        .expect("the literal-prefix paragraph is present");
    let block_id = NodeId::from(block.id.to_string().as_str());
    let read_text = block.text.clone();
    assert_eq!(read_text, "1.\tEvents", "read_block surfaces the label");

    // Delete the paragraph, pinning it with the VERBATIM read text as `expect`.
    let tx = EditTransaction {
        steps: vec![EditStep::DeleteBlockRange {
            from_block_id: block_id.clone(),
            to_block_id: block_id,
            rationale: None,
            expect: read_text.clone(),
            semantic_hash: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    let result = apply_transaction(&canon, &tx);
    assert!(
        result.is_ok(),
        "delete must accept the read string ({read_text:?}) as `expect` verbatim, same \
         prefix-aware rule as replace; got: {:?}",
        result.err()
    );
}
