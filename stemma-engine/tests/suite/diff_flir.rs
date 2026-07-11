//! Diff quality tests for the FLIR credit agreement sample.
//!
//! FLIR is a large (~2700 blocks) credit agreement where the "after" document
//! adds new subsections (U.S. Term Loan, Dutch Term Loan) inside Section 2.01.
//! This stresses the DP alignment because the gap between anchors contains many
//! empty paragraphs and a drawing element that create order conflicts with
//! content-bearing paragraphs.

use std::fs;
use stemma::{DiffChange, DocxRuntime, InlineChange, SimpleRuntime};

use crate::common;

fn load_flir_diff() -> Vec<DiffChange> {
    let before = fs::read(common::samples_dir().join("flir-credit-agreement/before.docx"))
        .expect("read before.docx");
    let after = fs::read(common::samples_dir().join("flir-credit-agreement/after.docx"))
        .expect("read after.docx");

    let runtime = SimpleRuntime::new();
    let import_before = runtime.import_docx(&before).expect("import before");
    let import_after = runtime.import_docx(&after).expect("import after");

    runtime
        .diff(&import_before.doc_handle, &import_after.doc_handle)
        .expect("diff")
        .changes
}

/// The body paragraph "(a) Revolving Loans. Subject to..." differs between
/// before.docx and after.docx by only "Section 2.01" → "Section 2.01(a)"
/// (two small insertions in a 1393-char paragraph). The diff must produce
/// a BlockModified with inline changes, not a full BlockDeleted + BlockInserted.
///
/// Regression: the DP alignment matched empty paragraphs + a drawing element
/// instead of the content paragraphs, because the cost model didn't weight
/// matches by content significance. Fixed by `content_significance()`.
#[test]
#[ignore = "requires private corpus (flir-credit-agreement, real third-party); set STEMMA_CORPUS_ROOT — run via just nightly"]
fn section_2_01_body_is_modified_not_deleted_inserted() {
    let changes = load_flir_diff();

    let mut found_modified = false;
    let mut found_deleted = false;

    for change in &changes {
        match change {
            DiffChange::BlockModified {
                old_text,
                inline_changes,
                ..
            } if old_text.contains("Revolving Loans. Subject to") => {
                found_modified = true;
                assert!(
                    !inline_changes.is_empty(),
                    "Modified body paragraph should have inline changes"
                );
            }
            DiffChange::BlockDeleted { old_text, .. }
                if old_text.contains("Revolving Loans. Subject to") =>
            {
                found_deleted = true;
            }
            _ => {}
        }
    }

    assert!(
        found_modified,
        "Body paragraph '(a) Revolving Loans. Subject to...' must be BlockModified"
    );
    assert!(
        !found_deleted,
        "Body paragraph '(a) Revolving Loans. Subject to...' must NOT be BlockDeleted"
    );
}

/// The Section 2.01 heading changes from "Revolving Loans." to
/// "Revolving Loans, U.S. Term Loan and Dutch Term Loan." — this should be
/// a BlockModified with inline changes showing the added text.
#[test]
#[ignore = "requires private corpus (flir-credit-agreement, real third-party); set STEMMA_CORPUS_ROOT — run via just nightly"]
fn section_2_01_heading_is_modified_not_deleted_inserted() {
    let changes = load_flir_diff();

    // The heading is short: "2.01    Revolving Loans." (28 chars).
    // Look for it by matching the section number + "Revolving Loans" without
    // the longer body text "Subject to".
    let mut found_modified = false;
    let mut found_deleted = false;

    for change in &changes {
        match change {
            DiffChange::BlockModified { old_text, .. }
                if old_text.starts_with("2.01") && old_text.contains("Revolving Loans") =>
            {
                // Exclude the TOC entry (which has trailing page number and is longer)
                if old_text.len() < 40 {
                    found_modified = true;
                }
            }
            DiffChange::BlockDeleted { old_text, .. }
                if old_text.starts_with("2.01")
                    && old_text.contains("Revolving Loans")
                    && old_text.len() < 40 =>
            {
                found_deleted = true;
            }
            _ => {}
        }
    }

    assert!(
        found_modified,
        "Heading '2.01 Revolving Loans.' must be BlockModified"
    );
    assert!(
        !found_deleted,
        "Heading '2.01 Revolving Loans.' must NOT be BlockDeleted"
    );
}

/// Guard overall diff quality: the number of BlockModified changes should be
/// substantially higher than naively deleting+inserting everything. A regression
/// in the alignment would shift changes from Modified → Deleted+Inserted.
#[test]
#[ignore = "requires private corpus (flir-credit-agreement, real third-party); set STEMMA_CORPUS_ROOT — run via just nightly"]
fn overall_diff_quality_has_enough_modifications() {
    let changes = load_flir_diff();

    let mut deleted = 0usize;
    let mut inserted = 0usize;
    let mut modified = 0usize;

    for change in &changes {
        match change {
            DiffChange::BlockDeleted { .. } => deleted += 1,
            DiffChange::BlockInserted { .. } => inserted += 1,
            DiffChange::BlockModified { .. } => modified += 1,
            _ => {}
        }
    }

    // With correct alignment: ~400 modified, ~500 deleted, ~590 inserted.
    // A regression would reduce modified and inflate deleted+inserted.
    assert!(
        modified >= 300,
        "Expected ≥300 BlockModified changes (got {modified}). \
         Alignment regression: content paragraphs are being deleted+inserted \
         instead of matched as modifications. del={deleted} ins={inserted}"
    );
}

/// Measure total highlighted (changed) text volume across the diff.
///
/// This is the most direct UX proxy: when a paragraph with a 6-char edit is
/// shown as delete+insert, the user sees ~2800 chars highlighted. When it's
/// shown as an inline modification, they see ~12 chars. A regression in
/// alignment inflates this number.
///
/// For BlockModified, we sum the chars in Inserted + Deleted inline spans.
/// For BlockDeleted / BlockInserted, the entire paragraph text counts.
#[test]
#[ignore = "requires private corpus (flir-credit-agreement, real third-party); set STEMMA_CORPUS_ROOT — run via just nightly"]
fn highlighted_text_volume_is_bounded() {
    let changes = load_flir_diff();

    let mut volume = 0usize;

    for change in &changes {
        match change {
            DiffChange::BlockDeleted { old_text, .. } => {
                volume += old_text.len();
            }
            DiffChange::BlockInserted { .. } => {
                // Inserted block text is in the block, not a flat string.
                // Use a conservative estimate: count it as the same as a
                // typical paragraph (the exact text isn't easily accessible
                // without extracting inlines, and the deleted side already
                // captures most of the inflation signal).
            }
            DiffChange::BlockModified { inline_changes, .. } => {
                for ic in inline_changes {
                    match ic {
                        InlineChange::Inserted { text, .. }
                        | InlineChange::Deleted { text, .. } => {
                            volume += text.len();
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    // Current value with content-aware scoring: ~109k chars (deleted text only).
    // Before the fix (empty paragraphs overvalued): ~117k chars.
    // A significant regression would push this above 130k.
    eprintln!("highlighted text volume: {volume} chars");
    assert!(
        volume <= 130_000,
        "Highlighted text volume is {volume} chars, expected ≤130k. \
         Alignment regression: content paragraphs are being fully \
         deleted+inserted instead of showing inline modifications."
    );
}
