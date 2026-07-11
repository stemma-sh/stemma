//! Integration tests for the clone_handle functionality.

use std::fs;

use stemma::{DocxRuntime, ExportMode, SimpleRuntime, TransactionMeta};

#[test]
fn clone_handle_creates_independent_copy() {
    let input_bytes = fs::read("testdata/simple-text/before.docx").expect("read testdata");

    let runtime = SimpleRuntime::new();
    let original = runtime.import_docx(&input_bytes).expect("import");

    // Clone the handle
    let cloned_handle = runtime.clone_handle(&original.doc_handle).expect("clone");

    // Verify clone has different handle ID
    assert_ne!(original.doc_handle.0, cloned_handle.0);

    // Verify clone has same content (fingerprint)
    let original_view = runtime.view(&original.doc_handle).expect("view original");
    let cloned_view = runtime.view(&cloned_handle).expect("view clone");
    assert_eq!(original_view.fingerprint, cloned_view.fingerprint);
}

#[test]
fn clone_can_be_used_for_redline() {
    let base_bytes = fs::read("testdata/simple-text/before.docx").expect("read base");
    let target_bytes = fs::read("testdata/simple-text/after.docx").expect("read target");

    let runtime = SimpleRuntime::new();
    let base = runtime.import_docx(&base_bytes).expect("import base");
    let target = runtime.import_docx(&target_bytes).expect("import target");

    // Clone handles for redline (the production pattern)
    let base_clone = runtime.clone_handle(&base.doc_handle).expect("clone base");
    let target_clone = runtime
        .clone_handle(&target.doc_handle)
        .expect("clone target");

    // Use clones for diff_and_redline
    let meta = TransactionMeta {
        author: "clone_handle".to_string(),
        reason: Some("test".to_string()),
        timestamp_utc: None,
    };
    let result = runtime
        .diff_and_redline(&base_clone, &target_clone, meta)
        .expect("redline should succeed");

    // Verify redline was applied
    assert!(result.applied);

    // Export the redlined document
    let exported = runtime
        .export_docx(&base_clone, ExportMode::Redline)
        .expect("export");
    assert!(!exported.is_empty());

    // Original handles should still be valid and unchanged
    let original_view = runtime.view(&base.doc_handle).expect("view original");
    assert_eq!(original_view.fingerprint, base.fingerprint);
}

#[test]
fn clone_invalid_handle_returns_error() {
    let runtime = SimpleRuntime::new();
    let fake_handle = stemma::DocHandle("nonexistent".to_string());

    let result = runtime.clone_handle(&fake_handle);
    assert!(result.is_err());
}
