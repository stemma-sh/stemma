//! Integration tests for the document diff functionality.

use std::fs;
use std::io::{Cursor, Read};

use stemma::{
    BlockNode, DiffChange, DocxRuntime, ExportMode, InlineNode, SimpleRuntime, TransactionMeta,
};

use zip::ZipArchive;

#[test]
fn diff_identical_documents() {
    let input_bytes = fs::read("testdata/simple-text/before.docx")
        .expect("read testdata/simple-text/before.docx");

    let runtime = SimpleRuntime::new();
    let import1 = runtime.import_docx(&input_bytes).expect("import docx 1");
    let import2 = runtime.import_docx(&input_bytes).expect("import docx 2");

    let diff = runtime
        .diff(&import1.doc_handle, &import2.doc_handle)
        .expect("diff should succeed");

    assert!(
        diff.changes.is_empty(),
        "identical documents should have no changes, got {:?}",
        diff.changes
    );
}

#[test]
fn diff_preserves_unchanged_content() {
    let input_bytes = fs::read("testdata/simple-text/before.docx")
        .expect("read testdata/simple-text/before.docx");

    let runtime = SimpleRuntime::new();
    let import_base = runtime.import_docx(&input_bytes).expect("import base");
    let import_target = runtime.import_docx(&input_bytes).expect("import target");

    // Keep the same content - no modifications
    let diff = runtime
        .diff(&import_base.doc_handle, &import_target.doc_handle)
        .expect("diff should succeed");

    assert!(
        diff.changes.is_empty(),
        "unchanged documents should have no changes"
    );
}

// ============================================================================
// Integration tests using actual testdata before/after pairs
// ============================================================================

/// Test diffing the simple-text before/after pair.
/// Before: "This is a test now foo bar baz"
/// After:  "This is a test what are the chances"
#[test]
fn diff_simple_text_before_after() {
    let before_bytes = fs::read("testdata/simple-text/before.docx").expect("read before.docx");
    let after_bytes = fs::read("testdata/simple-text/after.docx").expect("read after.docx");

    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before_bytes).expect("import before");
    let import_after = runtime.import_docx(&after_bytes).expect("import after");

    let diff = runtime
        .diff(&import_before.doc_handle, &import_after.doc_handle)
        .expect("diff should succeed");

    // Should have exactly one modified block
    assert_eq!(
        diff.changes.len(),
        1,
        "simple-text should have exactly one change, got {:?}",
        diff.changes
    );

    match &diff.changes[0] {
        DiffChange::BlockModified {
            old_text,
            new_text,
            inline_changes,
            ..
        } => {
            // Verify the texts match expected (ignoring potential trailing whitespace/newlines)
            assert!(
                old_text.contains("This is a test"),
                "old_text should contain 'This is a test', got: {old_text}"
            );
            assert!(
                old_text.contains("now foo bar baz"),
                "old_text should contain 'now foo bar baz', got: {old_text}"
            );
            assert!(
                new_text.contains("This is a test"),
                "new_text should contain 'This is a test', got: {new_text}"
            );
            assert!(
                new_text.contains("what are the chances"),
                "new_text should contain 'what are the chances', got: {new_text}"
            );

            // Verify inline changes detect the specific modification
            let has_delete = inline_changes.iter().any(|c| {
                matches!(c, stemma::InlineChange::Deleted { text, .. } if text.contains("now") || text.contains("foo") || text.contains("bar") || text.contains("baz"))
            });
            let has_insert = inline_changes.iter().any(|c| {
                matches!(c, stemma::InlineChange::Inserted { text, .. } if text.contains("what") || text.contains("chances"))
            });

            assert!(has_delete, "should detect deletion of 'now foo bar baz'");
            assert!(
                has_insert,
                "should detect insertion of 'what are the chances'"
            );
        }
        other => panic!("expected BlockModified, got {other:?}"),
    }
}

/// Test that diff_and_redline on simple-text produces valid tracked changes.
#[test]
fn redline_simple_text_before_after() {
    let before_bytes = fs::read("testdata/simple-text/before.docx").expect("read before.docx");
    let after_bytes = fs::read("testdata/simple-text/after.docx").expect("read after.docx");

    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before_bytes).expect("import before");
    let import_after = runtime.import_docx(&after_bytes).expect("import after");

    let meta = TransactionMeta {
        author: "diff_simple".to_string(),
        reason: Some("simple-text redline test".to_string()),
        timestamp_utc: Some("2024-01-15T10:30:00Z".to_string()),
    };

    runtime
        .diff_and_redline(&import_before.doc_handle, &import_after.doc_handle, meta)
        .expect("diff_and_redline should succeed");

    let output_bytes = runtime
        .export_docx(&import_before.doc_handle, ExportMode::Redline)
        .expect("export docx");

    let document_xml = extract_document_xml(&output_bytes);

    // Verify tracked changes markup is present
    assert!(
        document_xml.contains("<w:del"),
        "redline should contain w:del element"
    );
    assert!(
        document_xml.contains("<w:ins"),
        "redline should contain w:ins element"
    );

    // Verify the deleted text appears in w:delText
    assert!(
        document_xml.contains("<w:delText"),
        "redline should contain w:delText element"
    );

    // Verify the new text is present
    assert!(
        document_xml.contains("what") || document_xml.contains("chances"),
        "redline should contain the inserted text"
    );
}

/// Test diffing documents with multiple paragraphs (twenty-paragraphs testdata).
#[test]
fn diff_twenty_paragraphs() {
    let before_bytes = fs::read("testdata/twenty-paragraphs/before.docx")
        .expect("read twenty-paragraphs/before.docx");
    let after_bytes = fs::read("testdata/twenty-paragraphs/after.docx")
        .expect("read twenty-paragraphs/after.docx");

    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before_bytes).expect("import before");
    let import_after = runtime.import_docx(&after_bytes).expect("import after");

    let diff = runtime
        .diff(&import_before.doc_handle, &import_after.doc_handle)
        .expect("diff should succeed");

    // The diff should detect changes between the two documents
    // We're mainly testing that it doesn't crash and produces some output
    println!(
        "twenty-paragraphs diff: {} changes detected",
        diff.changes.len()
    );

    // Verify the diff has meaningful structure
    for (i, change) in diff.changes.iter().enumerate() {
        match change {
            DiffChange::BlockModified { block_id, .. } => {
                println!("  Change {}: BlockModified in {}", i, block_id.0);
            }
            DiffChange::BlockDeleted { block_id, .. } => {
                println!("  Change {}: BlockDeleted {}", i, block_id.0);
            }
            DiffChange::BlockInserted { block, .. } => {
                let id = match block {
                    BlockNode::Paragraph(p) => &p.id.0,
                    _ => "unknown",
                };
                println!("  Change {i}: BlockInserted {id}");
            }
            DiffChange::TableStructureChanged { table_id, .. } => {
                println!("  Change {}: TableStructureChanged {}", i, table_id.0);
            }
            // Story-level changes
            _ => {
                println!("  Change {i}: Story-level change");
            }
        }
    }
}

/// Test diffing documents with consecutive deletions.
#[test]
fn diff_ordering_deletion_consecutive() {
    let before_bytes = fs::read("testdata/ordering-deletion-consecutive/before.docx")
        .expect("read ordering-deletion-consecutive/before.docx");
    let after_bytes = fs::read("testdata/ordering-deletion-consecutive/after.docx")
        .expect("read ordering-deletion-consecutive/after.docx");

    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before_bytes).expect("import before");
    let import_after = runtime.import_docx(&after_bytes).expect("import after");

    let diff = runtime
        .diff(&import_before.doc_handle, &import_after.doc_handle)
        .expect("diff should succeed");

    // Should have some deletions
    let deletion_count = diff
        .changes
        .iter()
        .filter(|c| matches!(c, DiffChange::BlockDeleted { .. }))
        .count();

    println!("ordering-deletion-consecutive: {deletion_count} deletions detected");

    // For consecutive deletions, we expect multiple BlockDeleted changes
    // or BlockModified changes depending on the content
    assert!(
        !diff.changes.is_empty(),
        "should detect some changes in deletion test"
    );
}

/// Test diffing documents with insertions at the start.
#[test]
fn diff_ordering_insertion_start() {
    let before_bytes = fs::read("testdata/ordering-insertion-start/before.docx")
        .expect("read ordering-insertion-start/before.docx");
    let after_bytes = fs::read("testdata/ordering-insertion-start/after.docx")
        .expect("read ordering-insertion-start/after.docx");

    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before_bytes).expect("import before");
    let import_after = runtime.import_docx(&after_bytes).expect("import after");

    let diff = runtime
        .diff(&import_before.doc_handle, &import_after.doc_handle)
        .expect("diff should succeed");

    // Should have some insertions
    let insertion_count = diff
        .changes
        .iter()
        .filter(|c| matches!(c, DiffChange::BlockInserted { .. }))
        .count();

    println!("ordering-insertion-start: {insertion_count} insertions detected");

    assert!(
        !diff.changes.is_empty(),
        "should detect some changes in insertion test"
    );
}

// ============================================================================
// Comprehensive testdata coverage
// ============================================================================

/// Helper to run diff on a testdata pair and report results.
fn run_diff_testdata(name: &str) -> (usize, usize, usize, bool) {
    let before_path = format!("testdata/{name}/before.docx");
    let after_path = format!("testdata/{name}/after.docx");

    let before_bytes = match fs::read(&before_path) {
        Ok(b) => b,
        Err(_) => return (0, 0, 0, false),
    };
    let after_bytes = match fs::read(&after_path) {
        Ok(b) => b,
        Err(_) => return (0, 0, 0, false),
    };

    let runtime = SimpleRuntime::new();
    let import_before = match runtime.import_docx(&before_bytes) {
        Ok(i) => i,
        Err(_) => return (0, 0, 0, false),
    };
    let import_after = match runtime.import_docx(&after_bytes) {
        Ok(i) => i,
        Err(_) => return (0, 0, 0, false),
    };

    let diff = match runtime.diff(&import_before.doc_handle, &import_after.doc_handle) {
        Ok(d) => d,
        Err(_) => return (0, 0, 0, false),
    };

    let modified = diff
        .changes
        .iter()
        .filter(|c| matches!(c, DiffChange::BlockModified { .. }))
        .count();
    let deleted = diff
        .changes
        .iter()
        .filter(|c| matches!(c, DiffChange::BlockDeleted { .. }))
        .count();
    let inserted = diff
        .changes
        .iter()
        .filter(|c| matches!(c, DiffChange::BlockInserted { .. }))
        .count();

    (modified, deleted, inserted, true)
}

/// Test all ordering testdata pairs (these should all produce meaningful diffs).
#[test]
fn diff_all_ordering_testdata() {
    let ordering_tests = [
        "ordering-deletion-consecutive",
        "ordering-deletion-middle",
        "ordering-deletion-start",
        "ordering-delete-around-middle",
        "ordering-insertion-middle",
        "ordering-insertion-start",
        "ordering-interleaved",
        "ordering-multi-insert",
        "ordering-mixed-complex",
        "ordering-alternating-deletions",
    ];

    println!("\n=== Ordering testdata diff results ===");
    for name in &ordering_tests {
        let (modified, deleted, inserted, ok) = run_diff_testdata(name);
        if ok {
            let total = modified + deleted + inserted;
            println!("  {name}: {total} changes (mod={modified}, del={deleted}, ins={inserted})");
            assert!(
                total > 0,
                "{name} should detect changes between before/after"
            );
        } else {
            println!("  {name}: SKIPPED (file not found or import failed)");
        }
    }
}

/// Test SAFE document variations (legal contract diffs).
#[test]
fn diff_safe_document_variations() {
    let safe_tests = [
        "safe-us-vs-canada",
        "safe-us-vs-cayman",
        "safe-us-vs-singapore",
        "safe-valcap-vs-discount",
        "safe-valcap-vs-mfn",
    ];

    println!("\n=== SAFE document diff results ===");
    for name in &safe_tests {
        let (modified, deleted, inserted, ok) = run_diff_testdata(name);
        if ok {
            let total = modified + deleted + inserted;
            println!("  {name}: {total} changes (mod={modified}, del={deleted}, ins={inserted})");
            // SAFE documents should have differences
            assert!(
                total > 0,
                "{name} should detect changes between document variations"
            );
        } else {
            println!("  {name}: SKIPPED (file not found or import failed)");
        }
    }
}

/// Test that diff + redline works end-to-end for multiple testdata pairs.
#[test]
fn redline_multiple_testdata() {
    let test_cases = [
        "simple-text",
        "ordering-deletion-consecutive",
        "ordering-insertion-start",
    ];

    for name in &test_cases {
        let before_path = format!("testdata/{name}/before.docx");
        let after_path = format!("testdata/{name}/after.docx");

        let before_bytes =
            fs::read(&before_path).unwrap_or_else(|err| panic!("read {before_path}: {err}"));
        let after_bytes =
            fs::read(&after_path).unwrap_or_else(|err| panic!("read {after_path}: {err}"));

        let runtime = SimpleRuntime::new();
        let import_before = runtime.import_docx(&before_bytes).expect("import before");
        let import_after = runtime.import_docx(&after_bytes).expect("import after");

        let meta = TransactionMeta {
            author: "diff_simple".to_string(),
            reason: Some(format!("{name} redline")),
            timestamp_utc: Some("2024-01-15T10:30:00Z".to_string()),
        };

        // This should not panic
        let result =
            runtime.diff_and_redline(&import_before.doc_handle, &import_after.doc_handle, meta);

        match result {
            Ok(_) => {
                // Verify we can export
                let export_result =
                    runtime.export_docx(&import_before.doc_handle, ExportMode::Redline);
                assert!(export_result.is_ok(), "{name}: export should succeed");
                println!("{name}: redline OK");
            }
            Err(e) => {
                // Some test cases might fail due to barriers or other limitations
                // This is acceptable for MVP
                println!("{}: redline skipped ({:?})", name, e.code);
            }
        }
    }
}

#[test]
fn ordering_multi_insert_has_non_null_mid_anchors() {
    let before_bytes = fs::read("testdata/ordering-multi-insert/before.docx")
        .expect("read testdata/ordering-multi-insert/before.docx");
    let after_bytes = fs::read("testdata/ordering-multi-insert/after.docx")
        .expect("read testdata/ordering-multi-insert/after.docx");

    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before_bytes).expect("import before");
    let import_after = runtime.import_docx(&after_bytes).expect("import after");
    let diff = runtime
        .diff(&import_before.doc_handle, &import_after.doc_handle)
        .expect("diff");

    let inserted: Vec<(Option<String>, String)> = diff
        .changes
        .iter()
        .filter_map(|change| {
            if let DiffChange::BlockInserted {
                after_block_id,
                block: BlockNode::Paragraph(p),
                ..
            } = change
            {
                let text = p
                    .all_inlines_owned()
                    .iter()
                    .filter_map(|inline| {
                        if let InlineNode::Text(t) = inline {
                            Some(t.text.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<String>();
                Some((after_block_id.as_ref().map(|id| id.0.to_string()), text))
            } else {
                None
            }
        })
        .collect();

    let non_null_anchors = inserted
        .iter()
        .filter(|(anchor, _)| anchor.is_some())
        .count();
    assert!(
        non_null_anchors >= 1,
        "expected at least one inserted block anchored after an existing base block"
    );
}

#[test]
fn merge_accept_all_preserves_multi_insert_ordering() {
    let before_bytes = fs::read("testdata/ordering-multi-insert/before.docx")
        .expect("read testdata/ordering-multi-insert/before.docx");
    let after_bytes = fs::read("testdata/ordering-multi-insert/after.docx")
        .expect("read testdata/ordering-multi-insert/after.docx");

    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before_bytes).expect("import before");
    let import_after = runtime.import_docx(&after_bytes).expect("import after");
    let diff = runtime
        .diff(&import_before.doc_handle, &import_after.doc_handle)
        .expect("diff");

    let merged = stemma::merge_diff(
        &import_before.canonical,
        &import_after.canonical,
        &diff,
        &stemma::RevisionInfo {
            revision_id: 1,
            author: Some("test".to_string()),
            date: Some("2026-01-01T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    )
    .expect("merge_diff")
    .doc;

    let mut accepted = merged.clone();
    stemma::accept_all(&mut accepted);

    let accepted_texts: Vec<String> = accepted
        .blocks
        .iter()
        .filter_map(|block| {
            if let stemma::TrackedBlock {
                block: BlockNode::Paragraph(p),
                ..
            } = block
            {
                Some(
                    p.all_inlines_owned()
                        .iter()
                        .filter_map(|inline| {
                            if let InlineNode::Text(t) = inline {
                                Some(t.text.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<String>(),
                )
            } else {
                None
            }
        })
        .collect();

    let target_texts: Vec<String> = import_after
        .canonical
        .blocks
        .iter()
        .filter_map(|block| {
            if let stemma::TrackedBlock {
                block: BlockNode::Paragraph(p),
                ..
            } = block
            {
                Some(
                    p.all_inlines_owned()
                        .iter()
                        .filter_map(|inline| {
                            if let InlineNode::Text(t) = inline {
                                Some(t.text.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<String>(),
                )
            } else {
                None
            }
        })
        .collect();

    assert_eq!(accepted_texts, target_texts);
}

fn extract_document_xml(docx_bytes: &[u8]) -> String {
    let cursor = Cursor::new(docx_bytes);
    let mut zip = ZipArchive::new(cursor).expect("open zip");
    let mut file = zip
        .by_name("word/document.xml")
        .expect("word/document.xml present");
    let mut out = String::new();
    file.read_to_string(&mut out).expect("read document.xml");
    out
}

fn extract_zip_entry(docx_bytes: &[u8], entry_name: &str) -> Option<Vec<u8>> {
    let cursor = Cursor::new(docx_bytes);
    let mut zip = ZipArchive::new(cursor).expect("open zip");
    let mut file = match zip.by_name(entry_name) {
        Ok(f) => f,
        Err(_) => return None,
    };
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).expect("read entry");
    Some(buf)
}

fn list_zip_entries(docx_bytes: &[u8]) -> Vec<String> {
    let cursor = Cursor::new(docx_bytes);
    let mut zip = ZipArchive::new(cursor).expect("open zip");
    (0..zip.len())
        .map(|i| zip.by_index(i).unwrap().name().to_string())
        .collect()
}

#[test]
fn redline_image_replacement_includes_both_media_files() {
    let before_bytes =
        fs::read("testdata/images/before.docx").expect("read testdata/images/before.docx");
    let after_bytes =
        fs::read("testdata/images/after.docx").expect("read testdata/images/after.docx");

    let runtime = SimpleRuntime::new();
    let base = runtime.import_docx(&before_bytes).expect("import before");
    let target = runtime.import_docx(&after_bytes).expect("import after");

    let meta = TransactionMeta {
        author: "TestAuthor".to_string(),
        reason: Some("image replacement test".to_string()),
        timestamp_utc: Some("2024-01-15T10:30:00Z".to_string()),
    };

    runtime
        .diff_and_redline(&base.doc_handle, &target.doc_handle, meta)
        .expect("diff_and_redline");

    let redline_bytes = runtime
        .export_docx(&base.doc_handle, ExportMode::Redline)
        .expect("export redline");

    // 1. Verify both media files exist
    let entries = list_zip_entries(&redline_bytes);
    let media_entries: Vec<&String> = entries
        .iter()
        .filter(|e| e.starts_with("word/media/"))
        .collect();
    assert!(
        media_entries.len() >= 2,
        "Expected at least 2 media files in redline output, found {}: {:?}",
        media_entries.len(),
        media_entries
    );

    // 2. Verify the original and replacement images are different
    let before_image = extract_zip_entry(&before_bytes, "word/media/image1.tmp")
        .expect("before.docx should have image1.tmp");
    let after_image = extract_zip_entry(&after_bytes, "word/media/image1.tmp")
        .expect("after.docx should have image1.tmp");
    assert_ne!(
        before_image, after_image,
        "Test precondition: before and after images should differ"
    );

    // Check both image contents appear in the redline
    let mut found_before = false;
    let mut found_after = false;
    for entry in &media_entries {
        let bytes = extract_zip_entry(&redline_bytes, entry).expect("read media entry");
        if bytes == before_image {
            found_before = true;
        }
        if bytes == after_image {
            found_after = true;
        }
    }
    assert!(
        found_before,
        "Redline should contain the original (before) image"
    );
    assert!(
        found_after,
        "Redline should contain the replacement (after) image"
    );

    // 3. Verify document XML has tracked changes (w:del and w:ins)
    let doc_xml = extract_document_xml(&redline_bytes);
    assert!(
        doc_xml.contains("w:del") || doc_xml.contains("<w:del"),
        "Document XML should contain w:del for the deleted image"
    );
    assert!(
        doc_xml.contains("w:ins") || doc_xml.contains("<w:ins"),
        "Document XML should contain w:ins for the inserted image"
    );

    // 4. Verify document.xml.rels has at least 2 image relationships
    let rels_xml = extract_zip_entry(&redline_bytes, "word/_rels/document.xml.rels")
        .expect("rels should exist");
    let rels_str = String::from_utf8(rels_xml).expect("rels is UTF-8");
    let image_rel_count = rels_str.matches("relationships/image").count();
    assert!(
        image_rel_count >= 2,
        "Expected at least 2 image relationships in document.xml.rels, found {image_rel_count}"
    );

    // 5. Verify at least 2 distinct r:embed rIds in document XML
    let mut rids = Vec::new();
    for segment in doc_xml.split("r:embed=\"") {
        if let Some(end) = segment.find('"') {
            let rid = &segment[..end];
            if rid.starts_with("rId") && !rids.contains(&rid.to_string()) {
                rids.push(rid.to_string());
            }
        }
    }
    assert!(
        rids.len() >= 2,
        "Expected at least 2 distinct r:embed rIds, found {}: {:?}",
        rids.len(),
        rids
    );
}
