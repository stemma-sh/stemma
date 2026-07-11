//! Integration test: run the full redline pipeline on all synthesized fixtures
//! and the opaque-roundtrip sample, then validate the output DOCX bytes.
//!
//! Asserts zero validation errors.

use stemma::{
    DocxRuntime, ExportMode, SimpleRuntime, TransactionMeta, docx_validate::validate_docx,
};

use crate::common;

const TESTDATA: &str = "testdata/synthesized";

// ── helpers ──────────────────────────────────────────────────────────────

fn redline_meta() -> TransactionMeta {
    TransactionMeta {
        author: "test_docx_validate_integration".to_string(),
        reason: Some("validation test".to_string()),
        timestamp_utc: Some("2025-01-15T10:30:00Z".to_string()),
    }
}

fn make_redline(before_path: &str, after_path: &str) -> Vec<u8> {
    let before =
        std::fs::read(before_path).unwrap_or_else(|err| panic!("read {before_path}: {err}"));
    let after = std::fs::read(after_path).unwrap_or_else(|err| panic!("read {after_path}: {err}"));

    let runtime = SimpleRuntime::new();

    let ib = runtime
        .import_docx(&before)
        .unwrap_or_else(|err| panic!("import {before_path}: {err:?}"));
    let ia = runtime
        .import_docx(&after)
        .unwrap_or_else(|err| panic!("import {after_path}: {err:?}"));

    let apply = runtime
        .diff_and_redline(&ib.doc_handle, &ia.doc_handle, redline_meta())
        .unwrap_or_else(|err| panic!("diff_and_redline failed: {err:?}"));
    assert!(apply.applied, "redline must be applied");

    runtime
        .export_docx(&ib.doc_handle, ExportMode::Redline)
        .unwrap_or_else(|err| panic!("export_docx failed: {err:?}"))
}

fn assert_no_validation_errors(docx_bytes: &[u8], label: &str) {
    let validation = validate_docx(docx_bytes);
    let errors: Vec<String> = validation
        .errors()
        .map(|f| format!("[{}] {}: {}", f.rule_id, f.location, f.message))
        .collect();
    assert!(
        errors.is_empty(),
        "{label}: DOCX validation found {} error(s):\n{}",
        errors.len(),
        errors.join("\n")
    );
}

// ── synthesized fixture tests ────────────────────────────────────────────

#[test]
fn validate_text_substitution() {
    let bytes = make_redline(
        &format!("{TESTDATA}/text-substitution/before.docx"),
        &format!("{TESTDATA}/text-substitution/after.docx"),
    );
    assert_no_validation_errors(&bytes, "text-substitution");
}

#[test]
fn validate_paragraph_insertion() {
    let bytes = make_redline(
        &format!("{TESTDATA}/paragraph-insertion/before.docx"),
        &format!("{TESTDATA}/paragraph-insertion/after.docx"),
    );
    assert_no_validation_errors(&bytes, "paragraph-insertion");
}

#[test]
fn validate_paragraph_deletion() {
    let bytes = make_redline(
        &format!("{TESTDATA}/paragraph-deletion/before.docx"),
        &format!("{TESTDATA}/paragraph-deletion/after.docx"),
    );
    assert_no_validation_errors(&bytes, "paragraph-deletion");
}

#[test]
fn validate_mixed_paragraph_changes() {
    let bytes = make_redline(
        &format!("{TESTDATA}/mixed-paragraph-changes/before.docx"),
        &format!("{TESTDATA}/mixed-paragraph-changes/after.docx"),
    );
    assert_no_validation_errors(&bytes, "mixed-paragraph-changes");
}

#[test]
fn validate_table_cell_text() {
    let bytes = make_redline(
        &format!("{TESTDATA}/table-cell-text/before.docx"),
        &format!("{TESTDATA}/table-cell-text/after.docx"),
    );
    assert_no_validation_errors(&bytes, "table-cell-text");
}

#[test]
fn validate_table_row_addition() {
    let bytes = make_redline(
        &format!("{TESTDATA}/table-row-addition/before.docx"),
        &format!("{TESTDATA}/table-row-addition/after.docx"),
    );
    assert_no_validation_errors(&bytes, "table-row-addition");
}

#[test]
fn validate_table_row_deletion() {
    let bytes = make_redline(
        &format!("{TESTDATA}/table-row-deletion/before.docx"),
        &format!("{TESTDATA}/table-row-deletion/after.docx"),
    );
    assert_no_validation_errors(&bytes, "table-row-deletion");
}

#[test]
fn validate_header_modification() {
    let bytes = make_redline(
        &format!("{TESTDATA}/header-modification/before.docx"),
        &format!("{TESTDATA}/header-modification/after.docx"),
    );
    assert_no_validation_errors(&bytes, "header-modification");
}

#[test]
fn validate_footer_modification() {
    let bytes = make_redline(
        &format!("{TESTDATA}/footer-modification/before.docx"),
        &format!("{TESTDATA}/footer-modification/after.docx"),
    );
    assert_no_validation_errors(&bytes, "footer-modification");
}

#[test]
fn validate_opaque_redline_hyperlink() {
    let bytes = make_redline(
        &format!("{TESTDATA}/opaque-redline-hyperlink/before.docx"),
        &format!("{TESTDATA}/opaque-redline-hyperlink/after.docx"),
    );
    assert_no_validation_errors(&bytes, "opaque-redline-hyperlink");
}

#[test]
fn validate_opaque_redline_field() {
    let bytes = make_redline(
        &format!("{TESTDATA}/opaque-redline-field/before.docx"),
        &format!("{TESTDATA}/opaque-redline-field/after.docx"),
    );
    assert_no_validation_errors(&bytes, "opaque-redline-field");
}

// ── real sample tests ───────────────────────────────────────────────────

#[test]
fn validate_opaque_roundtrip_sample() {
    let samples = std::path::PathBuf::from("testdata");
    let bytes = make_redline(
        &samples
            .join("opaque-roundtrip/before.docx")
            .to_string_lossy(),
        &samples
            .join("opaque-roundtrip/after.docx")
            .to_string_lossy(),
    );
    assert_no_validation_errors(&bytes, "opaque-roundtrip");
}

#[test]
fn validate_header_footer_sample() {
    let samples = common::samples_dir();
    let before = samples.join("header-footer/before.docx");
    let after = samples.join("header-footer/after.docx");
    if !before.exists() {
        eprintln!(
            "SKIP: {} not found (worktree without sample)",
            before.display()
        );
        return;
    }
    let bytes = make_redline(&before.to_string_lossy(), &after.to_string_lossy());
    assert_no_validation_errors(&bytes, "header-footer");
}

// ── I-ORD-*: element ordering compliance ─────────────────────────────────

fn assert_no_ordering_warnings(docx_bytes: &[u8], label: &str) {
    let validation = validate_docx(docx_bytes);
    let ordering: Vec<String> = validation
        .findings
        .iter()
        .filter(|f| f.rule_id.starts_with("I-ORD"))
        .map(|f| format!("[{}] {}: {}", f.rule_id, f.location, f.message))
        .collect();
    assert!(
        ordering.is_empty(),
        "{label}: serializer produced {} ordering violation(s):\n{}",
        ordering.len(),
        ordering.join("\n")
    );
}

#[test]
fn ordering_text_substitution() {
    let bytes = make_redline(
        &format!("{TESTDATA}/text-substitution/before.docx"),
        &format!("{TESTDATA}/text-substitution/after.docx"),
    );
    assert_no_ordering_warnings(&bytes, "text-substitution");
}

#[test]
fn ordering_paragraph_insertion() {
    let bytes = make_redline(
        &format!("{TESTDATA}/paragraph-insertion/before.docx"),
        &format!("{TESTDATA}/paragraph-insertion/after.docx"),
    );
    assert_no_ordering_warnings(&bytes, "paragraph-insertion");
}

#[test]
fn ordering_paragraph_deletion() {
    let bytes = make_redline(
        &format!("{TESTDATA}/paragraph-deletion/before.docx"),
        &format!("{TESTDATA}/paragraph-deletion/after.docx"),
    );
    assert_no_ordering_warnings(&bytes, "paragraph-deletion");
}

#[test]
fn ordering_mixed_paragraph_changes() {
    let bytes = make_redline(
        &format!("{TESTDATA}/mixed-paragraph-changes/before.docx"),
        &format!("{TESTDATA}/mixed-paragraph-changes/after.docx"),
    );
    assert_no_ordering_warnings(&bytes, "mixed-paragraph-changes");
}

#[test]
fn ordering_table_cell_text() {
    let bytes = make_redline(
        &format!("{TESTDATA}/table-cell-text/before.docx"),
        &format!("{TESTDATA}/table-cell-text/after.docx"),
    );
    assert_no_ordering_warnings(&bytes, "table-cell-text");
}

#[test]
fn ordering_table_row_addition() {
    let bytes = make_redline(
        &format!("{TESTDATA}/table-row-addition/before.docx"),
        &format!("{TESTDATA}/table-row-addition/after.docx"),
    );
    assert_no_ordering_warnings(&bytes, "table-row-addition");
}

#[test]
fn ordering_table_row_deletion() {
    let bytes = make_redline(
        &format!("{TESTDATA}/table-row-deletion/before.docx"),
        &format!("{TESTDATA}/table-row-deletion/after.docx"),
    );
    assert_no_ordering_warnings(&bytes, "table-row-deletion");
}

#[test]
fn ordering_header_modification() {
    let bytes = make_redline(
        &format!("{TESTDATA}/header-modification/before.docx"),
        &format!("{TESTDATA}/header-modification/after.docx"),
    );
    assert_no_ordering_warnings(&bytes, "header-modification");
}

#[test]
fn ordering_footer_modification() {
    let bytes = make_redline(
        &format!("{TESTDATA}/footer-modification/before.docx"),
        &format!("{TESTDATA}/footer-modification/after.docx"),
    );
    assert_no_ordering_warnings(&bytes, "footer-modification");
}

#[test]
fn ordering_opaque_redline_hyperlink() {
    let bytes = make_redline(
        &format!("{TESTDATA}/opaque-redline-hyperlink/before.docx"),
        &format!("{TESTDATA}/opaque-redline-hyperlink/after.docx"),
    );
    assert_no_ordering_warnings(&bytes, "opaque-redline-hyperlink");
}

#[test]
fn ordering_opaque_redline_field() {
    let bytes = make_redline(
        &format!("{TESTDATA}/opaque-redline-field/before.docx"),
        &format!("{TESTDATA}/opaque-redline-field/after.docx"),
    );
    assert_no_ordering_warnings(&bytes, "opaque-redline-field");
}

#[test]
fn ordering_opaque_roundtrip_sample() {
    let samples = std::path::PathBuf::from("testdata");
    let bytes = make_redline(
        &samples
            .join("opaque-roundtrip/before.docx")
            .to_string_lossy(),
        &samples
            .join("opaque-roundtrip/after.docx")
            .to_string_lossy(),
    );
    assert_no_ordering_warnings(&bytes, "opaque-roundtrip");
}
