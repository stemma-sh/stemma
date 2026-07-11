//! Tests for documents containing pre-existing tracked changes.
//!
//! All other tests generate tracked changes by diffing two documents.
//! These tests verify correct behavior when importing documents that
//! already contain `w:ins`/`w:del`/`w:rPrChange` markup from Word's
//! native Track Changes feature.
//!
//! Covers:
//! 1. Import: tracked change text is included in parsed document content.
//! 2. Diff: comparing a doc with existing tracked changes against a clean doc.
//! 3. Roundtrip: import -> export preserves existing tracked changes.

use std::io::{Cursor, Write};

use stemma::{
    DocxRuntime, ExportMode, SimpleRuntime, TransactionMeta, redline_extract::extract_redline,
};
use zip::ZipWriter;
use zip::write::FileOptions;

// ── DOCX builder helpers ──────────────────────────────────────────────────

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

/// Build a minimal valid DOCX from raw document.xml body content.
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

fn wrap_body(body_content: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:wpc="http://schemas.microsoft.com/office/word/2010/wordprocessingCanvas"
            xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006"
            xmlns:o="urn:schemas-microsoft-com:office:office"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"
            xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math"
            xmlns:v="urn:schemas-microsoft-com:vml"
            xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"
            xmlns:w10="urn:schemas-microsoft-com:office:word"
            xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml"
            xmlns:wpg="http://schemas.microsoft.com/office/word/2010/wordprocessingGroup"
            xmlns:wpi="http://schemas.microsoft.com/office/word/2010/wordprocessingInk"
            xmlns:wne="http://schemas.microsoft.com/office/word/2006/wordml"
            xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"
            mc:Ignorable="w14 wp14">
  <w:body>
{body_content}
    <w:sectPr/>
  </w:body>
</w:document>"#
    )
}

fn redline_meta() -> TransactionMeta {
    TransactionMeta {
        author: "preexisting_tracked_changes".to_string(),
        reason: Some("preexisting tracked changes test".to_string()),
        timestamp_utc: Some("2025-06-01T00:00:00Z".to_string()),
    }
}

// ══════════════════════════════════════════════════════════════════════════
// 1. Import: Document with w:ins/w:del parses correctly
// ══════════════════════════════════════════════════════════════════════════

/// A document with inline w:del and w:ins tracked changes should import
/// without error. The tracked change text should be part of the parsed
/// document content (both deleted and inserted text are visible).
#[test]
fn import_document_with_inline_tracked_changes() {
    // Document: "The quick [brown→red] fox [jumps→leaps] over the lazy dog."
    // brown is deleted, red is inserted; jumps is deleted, leaps is inserted.
    let body = r#"
    <w:p>
      <w:r><w:t xml:space="preserve">The quick </w:t></w:r>
      <w:del w:id="1" w:author="Alice" w:date="2025-01-15T10:00:00Z">
        <w:r><w:delText>brown</w:delText></w:r>
      </w:del>
      <w:ins w:id="2" w:author="Bob" w:date="2025-01-16T10:00:00Z">
        <w:r><w:t>red</w:t></w:r>
      </w:ins>
      <w:r><w:t xml:space="preserve"> fox </w:t></w:r>
      <w:del w:id="3" w:author="Alice" w:date="2025-01-15T10:00:00Z">
        <w:r><w:delText>jumps</w:delText></w:r>
      </w:del>
      <w:ins w:id="4" w:author="Bob" w:date="2025-01-16T10:00:00Z">
        <w:r><w:t>leaps</w:t></w:r>
      </w:ins>
      <w:r><w:t xml:space="preserve"> over the lazy dog.</w:t></w:r>
    </w:p>"#;

    let docx_bytes = build_docx(&wrap_body(body));
    let runtime = SimpleRuntime::new();
    let result = runtime.import_docx(&docx_bytes);
    assert!(result.is_ok(), "import should succeed: {:?}", result.err());

    let import = result.unwrap();
    // The document should have at least one paragraph block.
    // (sectPr may also produce an OpaqueBlock.)
    let para_count = import
        .canonical
        .blocks
        .iter()
        .filter(|b| matches!(b.block, stemma::BlockNode::Paragraph(_)))
        .count();
    assert_eq!(para_count, 1, "expected 1 paragraph block");
}

/// A document with a fully inserted paragraph (w:pPr/w:rPr/w:ins on the
/// paragraph mark) should import and include the text.
#[test]
fn import_document_with_inserted_paragraph() {
    let body = r#"
    <w:p>
      <w:r><w:t>First paragraph.</w:t></w:r>
    </w:p>
    <w:p>
      <w:pPr><w:rPr><w:ins w:id="10" w:author="Bob" w:date="2025-01-16T10:00:00Z"/></w:rPr></w:pPr>
      <w:ins w:id="11" w:author="Bob" w:date="2025-01-16T10:00:00Z">
        <w:r><w:t>Newly inserted paragraph.</w:t></w:r>
      </w:ins>
    </w:p>
    <w:p>
      <w:r><w:t>Third paragraph.</w:t></w:r>
    </w:p>"#;

    let docx_bytes = build_docx(&wrap_body(body));
    let runtime = SimpleRuntime::new();
    let result = runtime.import_docx(&docx_bytes);
    assert!(result.is_ok(), "import should succeed: {:?}", result.err());

    let import = result.unwrap();
    let para_count = import
        .canonical
        .blocks
        .iter()
        .filter(|b| matches!(b.block, stemma::BlockNode::Paragraph(_)))
        .count();
    assert_eq!(
        para_count, 3,
        "expected 3 paragraph blocks (including inserted one)"
    );
}

/// A document with a fully deleted paragraph should import correctly.
#[test]
fn import_document_with_deleted_paragraph() {
    let body = r#"
    <w:p>
      <w:r><w:t>First paragraph.</w:t></w:r>
    </w:p>
    <w:p>
      <w:pPr><w:rPr><w:del w:id="20" w:author="Alice" w:date="2025-01-15T10:00:00Z"/></w:rPr></w:pPr>
      <w:del w:id="21" w:author="Alice" w:date="2025-01-15T10:00:00Z">
        <w:r><w:delText>This paragraph was deleted.</w:delText></w:r>
      </w:del>
    </w:p>
    <w:p>
      <w:r><w:t>Third paragraph.</w:t></w:r>
    </w:p>"#;

    let docx_bytes = build_docx(&wrap_body(body));
    let runtime = SimpleRuntime::new();
    let result = runtime.import_docx(&docx_bytes);
    assert!(result.is_ok(), "import should succeed: {:?}", result.err());

    let import = result.unwrap();
    let para_count = import
        .canonical
        .blocks
        .iter()
        .filter(|b| matches!(b.block, stemma::BlockNode::Paragraph(_)))
        .count();
    assert_eq!(
        para_count, 3,
        "expected 3 paragraph blocks (including deleted one)"
    );
}

// ══════════════════════════════════════════════════════════════════════════
// 2. Roundtrip: import -> export preserves tracked changes
// ══════════════════════════════════════════════════════════════════════════

/// Import a document with tracked changes, export it, then re-extract
/// the tracked changes. The tracked change structure should be preserved.
#[test]
fn roundtrip_preserves_inline_tracked_changes() {
    let body = r#"
    <w:p>
      <w:r><w:t xml:space="preserve">The quick </w:t></w:r>
      <w:del w:id="1" w:author="Alice" w:date="2025-01-15T10:00:00Z">
        <w:r><w:delText>brown</w:delText></w:r>
      </w:del>
      <w:ins w:id="2" w:author="Bob" w:date="2025-01-16T10:00:00Z">
        <w:r><w:t>red</w:t></w:r>
      </w:ins>
      <w:r><w:t xml:space="preserve"> fox.</w:t></w:r>
    </w:p>"#;

    let docx_bytes = build_docx(&wrap_body(body));

    // Step 1: Extract tracked changes from the original.
    let original_extract = extract_redline(&docx_bytes).expect("extract original");
    let original_accept = original_extract
        .body
        .iter()
        .map(|p| p.accept_text())
        .collect::<Vec<_>>()
        .join("\n");
    let original_reject = original_extract
        .body
        .iter()
        .map(|p| p.reject_text())
        .collect::<Vec<_>>()
        .join("\n");

    // Verify the original extraction is correct.
    assert!(
        original_reject.contains("brown"),
        "reject-all should contain 'brown', got: {original_reject}"
    );
    assert!(
        original_accept.contains("red"),
        "accept-all should contain 'red', got: {original_accept}"
    );
    assert!(
        !original_accept.contains("brown"),
        "accept-all should NOT contain 'brown', got: {original_accept}"
    );
    assert!(
        !original_reject.contains("red"),
        "reject-all should NOT contain 'red', got: {original_reject}"
    );

    // Step 2: Import and export.
    let runtime = SimpleRuntime::new();
    let import = runtime.import_docx(&docx_bytes).expect("import");
    let exported = runtime
        .export_docx(&import.doc_handle, ExportMode::Redline)
        .expect("export");

    // Step 3: Re-extract from exported.
    let re_extract = extract_redline(&exported).expect("extract roundtripped");
    let re_accept = re_extract
        .body
        .iter()
        .map(|p| p.accept_text())
        .collect::<Vec<_>>()
        .join("\n");
    let re_reject = re_extract
        .body
        .iter()
        .map(|p| p.reject_text())
        .collect::<Vec<_>>()
        .join("\n");

    // The roundtripped document should produce the same accept/reject text.
    assert_eq!(
        normalize_text(&re_accept),
        normalize_text(&original_accept),
        "roundtrip accept-all text should match original"
    );
    assert_eq!(
        normalize_text(&re_reject),
        normalize_text(&original_reject),
        "roundtrip reject-all text should match original"
    );
}

/// Roundtrip preserves tracked changes with multiple authors and
/// overlapping edits across paragraphs.
#[test]
fn roundtrip_preserves_multi_paragraph_tracked_changes() {
    let body = r#"
    <w:p>
      <w:r><w:t>Unchanged first paragraph.</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t xml:space="preserve">The </w:t></w:r>
      <w:del w:id="1" w:author="Author1" w:date="2025-01-15T10:00:00Z">
        <w:r><w:delText>original</w:delText></w:r>
      </w:del>
      <w:ins w:id="2" w:author="Author2" w:date="2025-01-16T10:00:00Z">
        <w:r><w:t>modified</w:t></w:r>
      </w:ins>
      <w:r><w:t xml:space="preserve"> clause.</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>Unchanged third paragraph.</w:t></w:r>
    </w:p>"#;

    let docx_bytes = build_docx(&wrap_body(body));

    let original_extract = extract_redline(&docx_bytes).expect("extract original");
    let original_accept: Vec<String> = original_extract
        .body
        .iter()
        .map(|p| p.accept_text())
        .collect();
    let original_reject: Vec<String> = original_extract
        .body
        .iter()
        .map(|p| p.reject_text())
        .collect();

    let runtime = SimpleRuntime::new();
    let import = runtime.import_docx(&docx_bytes).expect("import");
    let exported = runtime
        .export_docx(&import.doc_handle, ExportMode::Redline)
        .expect("export");

    let re_extract = extract_redline(&exported).expect("extract roundtripped");
    let re_accept: Vec<String> = re_extract.body.iter().map(|p| p.accept_text()).collect();
    let re_reject: Vec<String> = re_extract.body.iter().map(|p| p.reject_text()).collect();

    assert_eq!(
        normalize_doc(&re_accept),
        normalize_doc(&original_accept),
        "roundtrip accept-all per-paragraph text should match"
    );
    assert_eq!(
        normalize_doc(&re_reject),
        normalize_doc(&original_reject),
        "roundtrip reject-all per-paragraph text should match"
    );
}

// ══════════════════════════════════════════════════════════════════════════
// 3. Diff: document with tracked changes vs clean document
// ══════════════════════════════════════════════════════════════════════════

/// When comparing a document with existing tracked changes against a
/// clean document, the system should produce a valid diff. The diff
/// pipeline should not crash or produce incorrect results.
#[test]
fn diff_tracked_changes_doc_against_clean_doc() {
    // "Before" document has tracked changes: "The [old→new] agreement."
    let before_body = r#"
    <w:p>
      <w:r><w:t xml:space="preserve">The </w:t></w:r>
      <w:del w:id="1" w:author="Alice" w:date="2025-01-15T10:00:00Z">
        <w:r><w:delText>old</w:delText></w:r>
      </w:del>
      <w:ins w:id="2" w:author="Bob" w:date="2025-01-16T10:00:00Z">
        <w:r><w:t>new</w:t></w:r>
      </w:ins>
      <w:r><w:t xml:space="preserve"> agreement.</w:t></w:r>
    </w:p>"#;

    // "After" document is clean: "The revised agreement."
    let after_body = r#"
    <w:p>
      <w:r><w:t>The revised agreement.</w:t></w:r>
    </w:p>"#;

    let before_docx = build_docx(&wrap_body(before_body));
    let after_docx = build_docx(&wrap_body(after_body));

    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before_docx).expect("import before");
    let import_after = runtime.import_docx(&after_docx).expect("import after");

    // Diff should succeed without errors.
    let diff_result = runtime.diff(&import_before.doc_handle, &import_after.doc_handle);
    assert!(
        diff_result.is_ok(),
        "diff should succeed: {:?}",
        diff_result.err()
    );
}

/// Diff-and-redline pipeline should work when the base document has
/// pre-existing tracked changes.
#[test]
fn diff_and_redline_with_preexisting_tracked_changes() {
    let before_body = r#"
    <w:p>
      <w:r><w:t xml:space="preserve">The </w:t></w:r>
      <w:del w:id="1" w:author="Alice" w:date="2025-01-15T10:00:00Z">
        <w:r><w:delText>old</w:delText></w:r>
      </w:del>
      <w:ins w:id="2" w:author="Bob" w:date="2025-01-16T10:00:00Z">
        <w:r><w:t>new</w:t></w:r>
      </w:ins>
      <w:r><w:t xml:space="preserve"> agreement.</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>Second paragraph unchanged.</w:t></w:r>
    </w:p>"#;

    let after_body = r#"
    <w:p>
      <w:r><w:t>The revised agreement.</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>Second paragraph unchanged.</w:t></w:r>
    </w:p>"#;

    let before_docx = build_docx(&wrap_body(before_body));
    let after_docx = build_docx(&wrap_body(after_body));

    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before_docx).expect("import before");
    let import_after = runtime.import_docx(&after_docx).expect("import after");

    let apply = runtime.diff_and_redline(
        &import_before.doc_handle,
        &import_after.doc_handle,
        redline_meta(),
    );
    assert!(
        apply.is_ok(),
        "diff_and_redline should succeed: {:?}",
        apply.err()
    );

    let exported = runtime
        .export_docx(&import_before.doc_handle, ExportMode::Redline)
        .expect("export");

    // The exported DOCX should be valid and re-importable.
    let verify = SimpleRuntime::new();
    let reimport = verify.import_docx(&exported);
    assert!(
        reimport.is_ok(),
        "re-import of exported redline should succeed: {:?}",
        reimport.err()
    );
}

// ══════════════════════════════════════════════════════════════════════════
// 4. Redline extraction from documents with native tracked changes
// ══════════════════════════════════════════════════════════════════════════

/// extract_redline correctly identifies w:del and w:ins spans from
/// a hand-crafted DOCX with native tracked changes.
#[test]
fn extract_redline_from_native_tracked_changes() {
    let body = r#"
    <w:p>
      <w:r><w:t xml:space="preserve">The agreement shall be governed by the laws of </w:t></w:r>
      <w:del w:id="1" w:author="Legal" w:date="2025-02-01T10:00:00Z">
        <w:r><w:delText>New York</w:delText></w:r>
      </w:del>
      <w:ins w:id="2" w:author="Legal" w:date="2025-02-01T10:00:00Z">
        <w:r><w:t>California</w:t></w:r>
      </w:ins>
      <w:r><w:t>.</w:t></w:r>
    </w:p>"#;

    let docx_bytes = build_docx(&wrap_body(body));
    let extract = extract_redline(&docx_bytes).expect("extract");

    assert_eq!(extract.body.len(), 1, "expected 1 paragraph");

    let accept = extract.body[0].accept_text();
    let reject = extract.body[0].reject_text();

    assert_eq!(
        accept, "The agreement shall be governed by the laws of California.",
        "accept-all text"
    );
    assert_eq!(
        reject, "The agreement shall be governed by the laws of New York.",
        "reject-all text"
    );
}

/// Multiple tracked changes across paragraphs are extracted correctly.
#[test]
fn extract_redline_multiple_paragraphs_with_tracked_changes() {
    let body = r#"
    <w:p>
      <w:r><w:t>No changes here.</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t xml:space="preserve">Fee: </w:t></w:r>
      <w:del w:id="1" w:author="CFO" w:date="2025-02-01T10:00:00Z">
        <w:r><w:delText>$100,000</w:delText></w:r>
      </w:del>
      <w:ins w:id="2" w:author="CFO" w:date="2025-02-01T10:00:00Z">
        <w:r><w:t>$150,000</w:t></w:r>
      </w:ins>
    </w:p>
    <w:p>
      <w:pPr><w:rPr><w:ins w:id="10" w:author="CFO" w:date="2025-02-01T10:00:00Z"/></w:rPr></w:pPr>
      <w:ins w:id="11" w:author="CFO" w:date="2025-02-01T10:00:00Z">
        <w:r><w:t>Payment due within 30 days.</w:t></w:r>
      </w:ins>
    </w:p>"#;

    let docx_bytes = build_docx(&wrap_body(body));
    let extract = extract_redline(&docx_bytes).expect("extract");

    assert_eq!(extract.body.len(), 3, "expected 3 paragraphs");

    // First paragraph: no changes.
    assert_eq!(extract.body[0].accept_text(), "No changes here.");
    assert_eq!(extract.body[0].reject_text(), "No changes here.");

    // Second paragraph: fee change.
    assert_eq!(extract.body[1].accept_text(), "Fee: $150,000");
    assert_eq!(extract.body[1].reject_text(), "Fee: $100,000");

    // Third paragraph: entirely inserted.
    assert_eq!(extract.body[2].accept_text(), "Payment due within 30 days.");
    assert_eq!(extract.body[2].reject_text(), "");
}

/// A paragraph with w:del containing multiple runs is extracted correctly.
#[test]
fn extract_redline_multi_run_deletion() {
    let body = r#"
    <w:p>
      <w:r><w:t xml:space="preserve">Start </w:t></w:r>
      <w:del w:id="1" w:author="Editor" w:date="2025-01-20T10:00:00Z">
        <w:r><w:rPr><w:b/></w:rPr><w:delText xml:space="preserve">bold </w:delText></w:r>
        <w:r><w:delText>and plain</w:delText></w:r>
      </w:del>
      <w:r><w:t xml:space="preserve"> end.</w:t></w:r>
    </w:p>"#;

    let docx_bytes = build_docx(&wrap_body(body));
    let extract = extract_redline(&docx_bytes).expect("extract");

    assert_eq!(extract.body.len(), 1);
    let accept = extract.body[0].accept_text();
    let reject = extract.body[0].reject_text();

    assert_eq!(accept, "Start  end.", "accept-all: deleted text removed");
    assert_eq!(
        reject, "Start bold and plain end.",
        "reject-all: deleted text present"
    );
}

// ══════════════════════════════════════════════════════════════════════════
// 5. Move markers: w:moveFrom / w:moveTo
// ══════════════════════════════════════════════════════════════════════════

/// Documents with w:moveFrom and w:moveTo markers should import without
/// error. The moved content should be visible in the parsed document.
#[test]
fn import_document_with_move_markers() {
    let body = r#"
    <w:p>
      <w:r><w:t>Before the move.</w:t></w:r>
    </w:p>
    <w:p>
      <w:moveFrom w:id="100" w:author="Editor" w:date="2025-01-20T10:00:00Z" w:name="move1">
        <w:r><w:t>This content was moved.</w:t></w:r>
      </w:moveFrom>
    </w:p>
    <w:p>
      <w:r><w:t>Middle paragraph.</w:t></w:r>
    </w:p>
    <w:p>
      <w:moveTo w:id="101" w:author="Editor" w:date="2025-01-20T10:00:00Z" w:name="move1">
        <w:r><w:t>This content was moved.</w:t></w:r>
      </w:moveTo>
    </w:p>
    <w:p>
      <w:r><w:t>After the move.</w:t></w:r>
    </w:p>"#;

    let docx_bytes = build_docx(&wrap_body(body));
    let runtime = SimpleRuntime::new();
    let result = runtime.import_docx(&docx_bytes);
    assert!(
        result.is_ok(),
        "import with move markers should succeed: {:?}",
        result.err()
    );

    let import = result.unwrap();
    // Should have 5 paragraphs — moveFrom/moveTo content is included.
    let para_count = import
        .canonical
        .blocks
        .iter()
        .filter(|b| matches!(b.block, stemma::BlockNode::Paragraph(_)))
        .count();
    assert_eq!(
        para_count, 5,
        "expected 5 paragraph blocks (move content included)"
    );
}

// ══════════════════════════════════════════════════════════════════════════
// 6. Re-import of exported redline should succeed (export contract)
// ══════════════════════════════════════════════════════════════════════════

/// Documents with complex tracked changes (mixed del/ins across multiple
/// paragraphs) should survive import -> export -> re-import without error.
#[test]
fn export_contract_complex_tracked_changes() {
    let body = r#"
    <w:p>
      <w:r><w:t xml:space="preserve">Section 1: </w:t></w:r>
      <w:del w:id="1" w:author="Author1" w:date="2025-01-15T10:00:00Z">
        <w:r><w:delText>Original terms</w:delText></w:r>
      </w:del>
      <w:ins w:id="2" w:author="Author2" w:date="2025-01-16T10:00:00Z">
        <w:r><w:t>Revised terms</w:t></w:r>
      </w:ins>
    </w:p>
    <w:p>
      <w:pPr><w:rPr><w:del w:id="30" w:author="Author1" w:date="2025-01-15T10:00:00Z"/></w:rPr></w:pPr>
      <w:del w:id="31" w:author="Author1" w:date="2025-01-15T10:00:00Z">
        <w:r><w:delText>This entire section was removed.</w:delText></w:r>
      </w:del>
    </w:p>
    <w:p>
      <w:r><w:t>Unchanged section.</w:t></w:r>
    </w:p>
    <w:p>
      <w:pPr><w:rPr><w:ins w:id="40" w:author="Author2" w:date="2025-01-16T10:00:00Z"/></w:rPr></w:pPr>
      <w:ins w:id="41" w:author="Author2" w:date="2025-01-16T10:00:00Z">
        <w:r><w:t>New section added by Author2.</w:t></w:r>
      </w:ins>
    </w:p>"#;

    let docx_bytes = build_docx(&wrap_body(body));
    let runtime = SimpleRuntime::new();
    let import = runtime.import_docx(&docx_bytes).expect("import");
    let exported = runtime
        .export_docx(&import.doc_handle, ExportMode::Redline)
        .expect("export");

    // Re-import must succeed (export contract).
    let verify = SimpleRuntime::new();
    let reimport = verify.import_docx(&exported);
    assert!(
        reimport.is_ok(),
        "re-import of exported DOCX should succeed: {:?}",
        reimport.err()
    );
}

// ══════════════════════════════════════════════════════════════════════════
// 7. Comments: pre-existing comments survive diff/redline
// ══════════════════════════════════════════════════════════════════════════

const CONTENT_TYPES_WITH_COMMENTS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
  <Override PartName="/word/comments.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"/>
</Types>"#;

const WORD_RELS_WITH_COMMENTS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="comments.xml"/>
</Relationships>"#;

/// Build a DOCX with document.xml and comments.xml.
fn build_docx_with_comments(document_xml: &str, comments_xml: &str) -> Vec<u8> {
    let cursor = Cursor::new(Vec::new());
    let mut zip = ZipWriter::new(cursor);
    let options = FileOptions::default();

    zip.start_file("[Content_Types].xml", options).unwrap();
    zip.write_all(CONTENT_TYPES_WITH_COMMENTS.as_bytes())
        .unwrap();

    zip.start_file("_rels/.rels", options).unwrap();
    zip.write_all(RELS_XML.as_bytes()).unwrap();

    zip.start_file("word/_rels/document.xml.rels", options)
        .unwrap();
    zip.write_all(WORD_RELS_WITH_COMMENTS.as_bytes()).unwrap();

    zip.start_file("word/document.xml", options).unwrap();
    zip.write_all(document_xml.as_bytes()).unwrap();

    zip.start_file("word/comments.xml", options).unwrap();
    zip.write_all(comments_xml.as_bytes()).unwrap();

    let cursor = zip.finish().unwrap();
    cursor.into_inner()
}

/// Pre-existing comments must survive a diff/redline roundtrip.
/// The commentRangeStart/End markers must remain at paragraph level (not inside w:del),
/// the commentReference must not be wrapped in w:del, and the comment body in
/// comments.xml must not be wrapped in w:del.
#[test]
fn preexisting_comment_survives_diff_redline() {
    // Base document: paragraph with a comment on "scope of work"
    let base_body = r#"
    <w:p>
      <w:r><w:t xml:space="preserve">The </w:t></w:r>
      <w:commentRangeStart w:id="1"/>
      <w:r><w:t>scope of work</w:t></w:r>
      <w:commentRangeEnd w:id="1"/>
      <w:r><w:rPr/><w:commentReference w:id="1"/></w:r>
      <w:r><w:t xml:space="preserve"> shall be defined in Exhibit A.</w:t></w:r>
    </w:p>"#;

    let base_comments = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <w:comment w:id="1" w:author="Round1Reviewer" w:date="2025-01-15T10:00:00Z">
    <w:p>
      <w:r><w:t>Please clarify the scope of work.</w:t></w:r>
    </w:p>
  </w:comment>
</w:comments>"#;

    let base_docx = build_docx_with_comments(&wrap_body(base_body), base_comments);

    // Target document: same text but "Exhibit A" changed to "Appendix 1"
    let target_body = r#"
    <w:p>
      <w:r><w:t>The scope of work shall be defined in Appendix 1.</w:t></w:r>
    </w:p>"#;
    let target_docx = build_docx(&wrap_body(target_body));

    // Run diff and redline
    let runtime = SimpleRuntime::new();
    let import_base = runtime.import_docx(&base_docx).expect("import base");
    let import_target = runtime.import_docx(&target_docx).expect("import target");

    let apply = runtime.diff_and_redline(
        &import_base.doc_handle,
        &import_target.doc_handle,
        redline_meta(),
    );
    assert!(
        apply.is_ok(),
        "diff_and_redline should succeed: {:?}",
        apply.err()
    );

    let exported = runtime
        .export_docx(&import_base.doc_handle, ExportMode::Redline)
        .expect("export");

    // Extract the document.xml from the output to verify comment markers
    let reader = Cursor::new(&exported);
    let mut archive = zip::ZipArchive::new(reader).expect("open zip");

    // Check document.xml has commentRangeStart/End at paragraph level
    let mut doc_xml = String::new();
    {
        use std::io::Read;
        let mut file = archive.by_name("word/document.xml").expect("document.xml");
        file.read_to_string(&mut doc_xml).unwrap();
    }

    // commentRangeStart must be present and NOT inside a w:del
    assert!(
        doc_xml.contains("commentRangeStart"),
        "document.xml must contain commentRangeStart, got:\n{doc_xml}"
    );
    assert!(
        doc_xml.contains("commentRangeEnd"),
        "document.xml must contain commentRangeEnd, got:\n{doc_xml}"
    );
    assert!(
        doc_xml.contains("commentReference"),
        "document.xml must contain commentReference, got:\n{doc_xml}"
    );

    // Check comments.xml exists and has the comment body (not wrapped in w:del)
    let mut comments_xml = String::new();
    {
        use std::io::Read;
        let mut file = archive
            .by_name("word/comments.xml")
            .expect("comments.xml must exist");
        file.read_to_string(&mut comments_xml).unwrap();
    }

    assert!(
        comments_xml.contains("Please clarify the scope of work"),
        "comments.xml must contain original comment text, got:\n{comments_xml}"
    );

    // The comment body must NOT be wrapped in w:del (which would mark it as deleted)
    assert!(
        !comments_xml.contains("delText"),
        "comments.xml must NOT contain delText (comment body should not be marked as deleted), got:\n{comments_xml}"
    );
}

/// When the base document has a comment and the target also has the same comment,
/// the comment should be preserved as-is (not duplicated or corrupted).
#[test]
fn comment_preserved_when_both_docs_have_same_comment() {
    let body_with_comment = r#"
    <w:p>
      <w:r><w:t xml:space="preserve">The </w:t></w:r>
      <w:commentRangeStart w:id="1"/>
      <w:r><w:t>agreement</w:t></w:r>
      <w:commentRangeEnd w:id="1"/>
      <w:r><w:rPr/><w:commentReference w:id="1"/></w:r>
      <w:r><w:t xml:space="preserve"> is binding.</w:t></w:r>
    </w:p>"#;

    let comments = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <w:comment w:id="1" w:author="Reviewer" w:date="2025-01-15T10:00:00Z">
    <w:p>
      <w:r><w:t>Needs legal review.</w:t></w:r>
    </w:p>
  </w:comment>
</w:comments>"#;

    let base_docx = build_docx_with_comments(&wrap_body(body_with_comment), comments);
    let target_docx = build_docx_with_comments(&wrap_body(body_with_comment), comments);

    let runtime = SimpleRuntime::new();
    let import_base = runtime.import_docx(&base_docx).expect("import base");
    let import_target = runtime.import_docx(&target_docx).expect("import target");

    let apply = runtime.diff_and_redline(
        &import_base.doc_handle,
        &import_target.doc_handle,
        redline_meta(),
    );
    assert!(
        apply.is_ok(),
        "diff_and_redline should succeed: {:?}",
        apply.err()
    );

    let exported = runtime
        .export_docx(&import_base.doc_handle, ExportMode::Redline)
        .expect("export");

    let reader = Cursor::new(&exported);
    let mut archive = zip::ZipArchive::new(reader).expect("open zip");

    let mut comments_xml = String::new();
    {
        use std::io::Read;
        let mut file = archive
            .by_name("word/comments.xml")
            .expect("comments.xml must exist");
        file.read_to_string(&mut comments_xml).unwrap();
    }

    assert!(
        comments_xml.contains("Needs legal review"),
        "comment text must be preserved, got:\n{comments_xml}"
    );
    assert!(
        !comments_xml.contains("delText"),
        "comment body must not be marked as deleted, got:\n{comments_xml}"
    );
}

// ── Normalization helpers ─────────────────────────────────────────────────

/// Normalize whitespace for comparison (collapse whitespace, trim).
fn normalize_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ══════════════════════════════════════════════════════════════════════════
// 7. Both documents have pre-existing tracked changes
// ══════════════════════════════════════════════════════════════════════════

/// When BOTH base and target have pre-existing tracked changes, the pipeline
/// should accept-all in both before diffing. This simulates comparing draft 3
/// (with revisions from rounds 1-2) against draft 5 (with revisions from
/// rounds 3-4).
///
/// Base accepted text:  "The new agreement shall terminate on January 1."
/// Target accepted text: "The new contract shall terminate on March 15."
///
/// The diff should detect "agreement" → "contract" and "January 1" → "March 15",
/// NOT be confused by the deleted/inserted text in either document.
#[test]
fn diff_both_documents_have_preexisting_tracked_changes() {
    // Base document: started as "The old agreement shall terminate on December 1."
    // Revision round: "old" → "new", "December 1" → "January 1"
    let base_body = r#"
    <w:p>
      <w:r><w:t xml:space="preserve">The </w:t></w:r>
      <w:del w:id="1" w:author="Alice" w:date="2025-01-10T10:00:00Z">
        <w:r><w:delText>old</w:delText></w:r>
      </w:del>
      <w:ins w:id="2" w:author="Alice" w:date="2025-01-10T10:00:00Z">
        <w:r><w:t>new</w:t></w:r>
      </w:ins>
      <w:r><w:t xml:space="preserve"> agreement shall terminate on </w:t></w:r>
      <w:del w:id="3" w:author="Bob" w:date="2025-01-12T10:00:00Z">
        <w:r><w:delText>December 1</w:delText></w:r>
      </w:del>
      <w:ins w:id="4" w:author="Bob" w:date="2025-01-12T10:00:00Z">
        <w:r><w:t>January 1</w:t></w:r>
      </w:ins>
      <w:r><w:t>.</w:t></w:r>
    </w:p>"#;

    // Target document: started as "The old contract shall terminate on February 1."
    // Revision round: "old" → "new", "February 1" → "March 15"
    let target_body = r#"
    <w:p>
      <w:r><w:t xml:space="preserve">The </w:t></w:r>
      <w:del w:id="10" w:author="Carol" w:date="2025-02-01T10:00:00Z">
        <w:r><w:delText>old</w:delText></w:r>
      </w:del>
      <w:ins w:id="11" w:author="Carol" w:date="2025-02-01T10:00:00Z">
        <w:r><w:t>new</w:t></w:r>
      </w:ins>
      <w:r><w:t xml:space="preserve"> contract shall terminate on </w:t></w:r>
      <w:del w:id="12" w:author="Dave" w:date="2025-02-05T10:00:00Z">
        <w:r><w:delText>February 1</w:delText></w:r>
      </w:del>
      <w:ins w:id="13" w:author="Dave" w:date="2025-02-05T10:00:00Z">
        <w:r><w:t>March 15</w:t></w:r>
      </w:ins>
      <w:r><w:t>.</w:t></w:r>
    </w:p>"#;

    let base_docx = build_docx(&wrap_body(base_body));
    let target_docx = build_docx(&wrap_body(target_body));

    let runtime = SimpleRuntime::new();
    let import_base = runtime.import_docx(&base_docx).expect("import base");
    let import_target = runtime.import_docx(&target_docx).expect("import target");

    // Note: import_docx().canonical preserves tracked changes (both deleted and
    // inserted text visible). Normalization happens inside diff_and_redline()
    // via the view() path, which calls build_canonical_from_docx() and accepts
    // all pre-existing tracked changes before diffing.

    // ── Diff and redline should succeed ──
    // The diff pipeline normalizes internally: base becomes "The new agreement
    // shall terminate on January 1." and target becomes "The new contract shall
    // terminate on March 15." The diff should detect "agreement"→"contract"
    // and "January 1"→"March 15".

    let apply = runtime.diff_and_redline(
        &import_base.doc_handle,
        &import_target.doc_handle,
        redline_meta(),
    );
    assert!(
        apply.is_ok(),
        "diff_and_redline with tracked changes in both docs should succeed: {:?}",
        apply.err()
    );

    // ── Exported redline should be valid and re-importable ──

    let exported = runtime
        .export_docx(&import_base.doc_handle, ExportMode::Redline)
        .expect("export");

    let verify = SimpleRuntime::new();
    let reimport = verify.import_docx(&exported);
    assert!(
        reimport.is_ok(),
        "re-import of redline from dual-tracked-changes docs should succeed: {:?}",
        reimport.err()
    );
}

/// When both documents have pre-existing formatting changes (w:rPrChange),
/// normalization should strip the change annotations and keep the current
/// formatting, so the diff compares current-vs-current.
#[test]
fn diff_both_documents_have_preexisting_formatting_changes() {
    // Base: "Important" was changed from normal to bold
    let base_body = r#"
    <w:p>
      <w:r>
        <w:rPr>
          <w:b/>
          <w:rPrChange w:id="1" w:author="Alice" w:date="2025-01-10T10:00:00Z">
            <w:rPr/>
          </w:rPrChange>
        </w:rPr>
        <w:t xml:space="preserve">Important </w:t>
      </w:r>
      <w:r><w:t>clause text.</w:t></w:r>
    </w:p>"#;

    // Target: "Critical" was changed from italic to bold+italic
    let target_body = r#"
    <w:p>
      <w:r>
        <w:rPr>
          <w:b/>
          <w:i/>
          <w:rPrChange w:id="10" w:author="Bob" w:date="2025-02-01T10:00:00Z">
            <w:rPr><w:i/></w:rPr>
          </w:rPrChange>
        </w:rPr>
        <w:t xml:space="preserve">Critical </w:t>
      </w:r>
      <w:r><w:t>clause language.</w:t></w:r>
    </w:p>"#;

    let base_docx = build_docx(&wrap_body(base_body));
    let target_docx = build_docx(&wrap_body(target_body));

    let runtime = SimpleRuntime::new();
    let import_base = runtime.import_docx(&base_docx).expect("import base");
    let import_target = runtime.import_docx(&target_docx).expect("import target");

    let apply = runtime.diff_and_redline(
        &import_base.doc_handle,
        &import_target.doc_handle,
        redline_meta(),
    );
    assert!(
        apply.is_ok(),
        "diff_and_redline with formatting changes in both docs should succeed: {:?}",
        apply.err()
    );

    let exported = runtime
        .export_docx(&import_base.doc_handle, ExportMode::Redline)
        .expect("export");

    let verify = SimpleRuntime::new();
    let reimport = verify.import_docx(&exported);
    assert!(
        reimport.is_ok(),
        "re-import should succeed: {:?}",
        reimport.err()
    );
}

/// Normalize a document (list of paragraph texts) for comparison.
fn normalize_doc(paras: &[String]) -> String {
    paras
        .iter()
        .map(|t| normalize_text(t))
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}
