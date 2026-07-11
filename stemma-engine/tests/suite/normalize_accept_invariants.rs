//! Invariant tests for the production accept path (`normalize_docx`).
//!
//! When a user uploads a document with pre-existing tracked changes
//! (w:ins, w:del, w:rPrChange, etc.), the production pipeline accepts
//! those changes via `normalize::normalize_if_needed()` before building
//! the canonical model.
//!
//! These tests verify that path works correctly on real-world documents
//! (EMA EPAR pharmaceutical regulatory documents with native Word tracked
//! changes).
//!
//! Invariants:
//! 1. **Structural**: `normalize_docx(tc)` and `reject_all_docx(tc)` both
//!    produce zero remaining revisions.
//! 2. **Pipeline**: The full diff pipeline succeeds when a tracked-changes
//!    document is used as base input.
//! 3. **Text fidelity** (nightly): accept-all text from the tracked-changes
//!    document matches the canonical text of `after.docx`; reject-all text
//!    matches `before.docx`.

use rayon::prelude::*;
use stemma::docx::DocxArchive;
use stemma::normalize::{normalize_docx, preflight_scan, reject_all_docx};
use stemma::{
    BlockNode, DocxRuntime, ExportMode, InlineNode, MarkValue, SimpleRuntime, TrackingStatus,
    TransactionMeta,
};

use crate::common;

// ── discovery ────────────────────────────────────────────────────────────

/// Discover sample directories that contain a `tracked-changes.docx`.
/// Returns `(name, tracked_changes_path, before_path, after_path)` tuples.
/// Only directories with all three files are included.
fn discover_tracked_change_samples() -> Vec<(String, String, String, String)> {
    let samples_dir = common::samples_dir();
    let mut samples = Vec::new();

    let entries = std::fs::read_dir(&samples_dir)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", samples_dir.display()));

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let tc = path.join("tracked-changes.docx");
        let before = path.join("before.docx");
        let after = path.join("after.docx");

        if tc.exists() && before.exists() && after.exists() {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            samples.push((
                name,
                tc.to_string_lossy().to_string(),
                before.to_string_lossy().to_string(),
                after.to_string_lossy().to_string(),
            ));
        }
    }

    samples.sort_by(|a, b| a.0.cmp(&b.0));
    samples
}

// ── helpers ──────────────────────────────────────────────────────────────

fn redline_meta() -> TransactionMeta {
    TransactionMeta {
        author: "normalize_accept_invariants".to_string(),
        reason: Some("normalize accept invariant test".to_string()),
        timestamp_utc: Some("2025-06-01T00:00:00Z".to_string()),
    }
}

/// Extract paragraph texts from a canonical doc (accepted view).
fn canon_paragraph_texts(doc: &stemma::CanonDoc) -> Vec<String> {
    let mut texts = Vec::new();
    collect_block_texts(&doc.blocks, &mut texts);
    texts
}

fn collect_block_texts(blocks: &[stemma::TrackedBlock], texts: &mut Vec<String>) {
    for tracked in blocks {
        if matches!(tracked.status, TrackingStatus::Deleted(_)) {
            continue;
        }
        match &tracked.block {
            BlockNode::Paragraph(p) => {
                texts.push(extract_paragraph_text(p));
            }
            BlockNode::Table(t) => {
                for row in &t.rows {
                    if matches!(row.tracking_status, Some(TrackingStatus::Deleted(_))) {
                        continue;
                    }
                    for cell in &row.cells {
                        collect_cell_block_texts(&cell.blocks, texts);
                    }
                }
            }
            BlockNode::OpaqueBlock(_) => {}
        }
    }
}

fn collect_cell_block_texts(blocks: &[BlockNode], texts: &mut Vec<String>) {
    for block in blocks {
        match block {
            BlockNode::Paragraph(p) => {
                texts.push(extract_paragraph_text(p));
            }
            BlockNode::Table(t) => {
                for row in &t.rows {
                    for cell in &row.cells {
                        collect_cell_block_texts(&cell.blocks, texts);
                    }
                }
            }
            BlockNode::OpaqueBlock(_) => {}
        }
    }
}

fn extract_paragraph_text(p: &stemma::ParagraphNode) -> String {
    let inlines: Vec<&InlineNode> = p
        .segments
        .iter()
        .filter(|s| !matches!(s.status, TrackingStatus::Deleted(_)))
        .flat_map(|s| s.inlines.iter())
        .collect();
    let mut out = String::new();
    if let Some(ref prefix) = p.literal_prefix {
        out.push_str(prefix);
        out.push('\t');
    }
    for inline in &inlines {
        match inline {
            InlineNode::Text(t) => {
                if t.style_props.caps == MarkValue::On {
                    out.push_str(&t.text.to_uppercase());
                } else {
                    out.push_str(&t.text);
                }
            }
            InlineNode::HardBreak(_) => out.push('\n'),
            InlineNode::OpaqueInline(_) => out.push('\u{FFFC}'),
            InlineNode::Decoration(_)
            | InlineNode::CommentRangeStart { .. }
            | InlineNode::CommentRangeEnd { .. }
            | InlineNode::CommentReference { .. } => {}
        }
    }
    out
}

/// Collapse whitespace and join paragraphs for comparison.
fn normalize_doc_text(paras: &[String]) -> String {
    paras
        .iter()
        .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

// ══════════════════════════════════════════════════════════════════════════
// 1. Structural: normalize and reject both produce zero revisions
// ══════════════════════════════════════════════════════════════════════════

/// Every `tracked-changes.docx` sample must contain tracked changes that
/// both `normalize_docx` (accept-all) and `reject_all_docx` resolve to zero.
/// Samples are processed in parallel via rayon.
#[test]
#[ignore = "corpus sweep over backend samples; set STEMMA_CORPUS_ROOT, run via just nightly"]
fn normalize_and_reject_produce_zero_revisions() {
    let samples = discover_tracked_change_samples();
    assert!(!samples.is_empty(), "no tracked-change samples found");

    samples.par_iter().for_each(|(name, tc_path, _, _)| {
        let start = std::time::Instant::now();

        let bytes =
            std::fs::read(tc_path).unwrap_or_else(|e| panic!("[{name}] read {tc_path}: {e}"));
        let archive =
            DocxArchive::read(&bytes).unwrap_or_else(|e| panic!("[{name}] parse DOCX: {e:?}"));

        // Preflight must detect revisions.
        let pre = preflight_scan(&archive).unwrap_or_else(|e| panic!("[{name}] preflight: {e:?}"));
        assert!(
            pre.totals.revisions.total() > 0,
            "[{name}] tracked-changes.docx should have revisions, found 0"
        );

        // Accept-all must resolve to zero.
        let (normalized, result) =
            normalize_docx(&archive).unwrap_or_else(|e| panic!("[{name}] normalize: {e:?}"));
        assert!(
            result.revisions_resolved > 0,
            "[{name}] normalize should resolve revisions"
        );

        let post = preflight_scan(&normalized)
            .unwrap_or_else(|e| panic!("[{name}] post-normalize preflight: {e:?}"));
        assert_eq!(
            post.totals.revisions.total(),
            0,
            "[{name}] should have zero revisions after normalize (accept-all)"
        );

        // Reject-all must also resolve to zero.
        let (rejected, reject_result) =
            reject_all_docx(&archive).unwrap_or_else(|e| panic!("[{name}] reject_all: {e:?}"));
        assert!(
            reject_result.revisions_resolved > 0,
            "[{name}] reject_all should resolve revisions"
        );

        let post_reject = preflight_scan(&rejected)
            .unwrap_or_else(|e| panic!("[{name}] post-reject preflight: {e:?}"));
        assert_eq!(
            post_reject.totals.revisions.total(),
            0,
            "[{name}] should have zero revisions after reject-all"
        );

        eprintln!("  [{name}] normalize + reject OK ({:.1?})", start.elapsed());
    });

    eprintln!(
        "normalize_and_reject_produce_zero_revisions: checked {} samples",
        samples.len()
    );
}

// ══════════════════════════════════════════════════════════════════════════
// 2. Pipeline: diff succeeds when tracked-changes.docx is used as base
// ══════════════════════════════════════════════════════════════════════════

/// The full diff-and-redline pipeline must succeed when the base document
/// has pre-existing tracked changes. This exercises the production path:
/// `import_docx` → `diff_and_redline` → `view()` → `normalize_if_needed`.
///
/// Uses `catch_unwind` to collect all failures across samples rather than
/// aborting on the first one.
#[test]
#[ignore] // Slow fixture sweep — run via `just normalize-accept`
fn diff_pipeline_succeeds_with_tracked_changes_base() {
    let samples = discover_tracked_change_samples();
    assert!(!samples.is_empty(), "no tracked-change samples found");

    let mut total_checked = 0;
    let mut failures = Vec::new();

    for (name, tc_path, _, after_path) in &samples {
        eprintln!("  [{name}] starting pipeline test...");

        let tc_bytes = std::fs::read(tc_path).unwrap_or_else(|e| panic!("[{name}] read tc: {e}"));
        let after_bytes =
            std::fs::read(after_path).unwrap_or_else(|e| panic!("[{name}] read after: {e}"));

        let name_owned = name.clone();
        let result = std::panic::catch_unwind(move || {
            let runtime = SimpleRuntime::new();
            let import_tc = runtime
                .import_docx(&tc_bytes)
                .map_err(|e| format!("[{name_owned}] import tracked-changes.docx: {e:?}"))?;
            let import_after = runtime
                .import_docx(&after_bytes)
                .map_err(|e| format!("[{name_owned}] import after.docx: {e:?}"))?;

            runtime
                .diff_and_redline(
                    &import_tc.doc_handle,
                    &import_after.doc_handle,
                    redline_meta(),
                )
                .map_err(|e| {
                    format!("[{name_owned}] diff_and_redline(tracked-changes, after): {e:?}")
                })?;

            let exported = runtime
                .export_docx(&import_tc.doc_handle, ExportMode::Redline)
                .map_err(|e| format!("[{name_owned}] export: {e:?}"))?;

            if exported.is_empty() {
                return Err(format!("[{name_owned}] exported DOCX is empty"));
            }

            let verify = SimpleRuntime::new();
            verify
                .import_docx(&exported)
                .map_err(|e| format!("[{name_owned}] re-import of exported redline: {e:?}"))?;

            Ok::<_, String>(())
        });

        match result {
            Ok(Ok(())) => {
                eprintln!("  [{name}] OK");
            }
            Ok(Err(msg)) => {
                eprintln!("  [{name}] FAILED: {msg}");
                failures.push(msg);
            }
            Err(panic_info) => {
                let msg = if let Some(s) = panic_info.downcast_ref::<String>() {
                    format!("[{name}] PANIC: {s}")
                } else if let Some(s) = panic_info.downcast_ref::<&str>() {
                    format!("[{name}] PANIC: {s}")
                } else {
                    format!("[{name}] PANIC: (unknown)")
                };
                eprintln!("  {msg}");
                failures.push(msg);
            }
        }

        total_checked += 1;
    }

    eprintln!(
        "diff_pipeline_succeeds_with_tracked_changes_base: checked {}, failures {}",
        total_checked,
        failures.len(),
    );

    assert!(
        failures.is_empty(),
        "pipeline failures on {} of {} samples:\n{}",
        failures.len(),
        total_checked,
        failures.join("\n"),
    );
}

// ══════════════════════════════════════════════════════════════════════════
// 3. Text fidelity: accept-all text matches after.docx, reject-all
//    matches before.docx
// ══════════════════════════════════════════════════════════════════════════

/// Fixtures excluded from text fidelity comparison.
///
/// Word's Compare Documents can misclassify "before" content as "inserted"
/// (w:ins). When we correctly reject all changes, that misclassified content
/// gets dropped, causing reject_all(tc) to have less text than before.docx.
/// This is a Word comparison imperfection, not a reject_all_docx bug.
/// See the TEXT_FIDELITY_EXCLUSIONS comments below for per-fixture details.
const TEXT_FIDELITY_EXCLUSIONS: &[&str] = &[
    "humira-epar",           // OOMs in debug mode (~2.7MB)
    "annex-ii-instructions", // Word misclassifies ~2467 chars of before content as w:ins
    "annex-it-solutions",    // Word misclassifies ~2593 chars of before content as w:ins
];

/// Accept-all of `tracked-changes.docx` must produce the same paragraph
/// text as `after.docx`. Reject-all must match `before.docx`.
///
/// This is the core text-fidelity invariant for the production accept path.
/// It verifies that `normalize_docx` (accept) and `reject_all_docx` (reject)
/// faithfully transform tracked-change documents into the expected versions.
#[test]
#[ignore] // Slow fixture sweep — run via `just normalize-accept`
fn normalize_text_fidelity_matches_before_after() {
    let samples = discover_tracked_change_samples();
    assert!(!samples.is_empty(), "no tracked-change samples found");

    let mut total_checked = 0;
    let mut failures = Vec::new();

    for (name, tc_path, before_path, after_path) in &samples {
        if TEXT_FIDELITY_EXCLUSIONS.contains(&name.as_str()) {
            eprintln!("  [{name}] SKIPPED (exclusion list)");
            continue;
        }

        let tc_bytes = std::fs::read(tc_path).unwrap_or_else(|e| panic!("[{name}] read tc: {e}"));
        let before_bytes =
            std::fs::read(before_path).unwrap_or_else(|e| panic!("[{name}] read before: {e}"));
        let after_bytes =
            std::fs::read(after_path).unwrap_or_else(|e| panic!("[{name}] read after: {e}"));

        let tc_archive =
            DocxArchive::read(&tc_bytes).unwrap_or_else(|e| panic!("[{name}] parse tc: {e:?}"));

        // ── Accept-all: normalize(tc) should match after.docx ──────────

        let (accepted_archive, _) = normalize_docx(&tc_archive)
            .unwrap_or_else(|e| panic!("[{name}] normalize_docx: {e:?}"));
        let accepted_bytes = accepted_archive
            .write()
            .unwrap_or_else(|e| panic!("[{name}] write accepted: {e:?}"));

        let runtime = SimpleRuntime::new();
        let import_accepted = runtime
            .import_docx(&accepted_bytes)
            .unwrap_or_else(|e| panic!("[{name}] import accepted: {e:?}"));
        let import_after = runtime
            .import_docx(&after_bytes)
            .unwrap_or_else(|e| panic!("[{name}] import after: {e:?}"));

        let accepted_text = normalize_doc_text(&canon_paragraph_texts(&import_accepted.canonical));
        let after_text = normalize_doc_text(&canon_paragraph_texts(&import_after.canonical));

        if accepted_text != after_text {
            failures.push(format!(
                "[{name}] accept-all text DIFFERS from after.docx \
                 (accepted_len={}, after_len={})",
                accepted_text.len(),
                after_text.len(),
            ));
        }

        // ── Reject-all: reject(tc) should match before.docx ───────────

        let (rejected_archive, _) = reject_all_docx(&tc_archive)
            .unwrap_or_else(|e| panic!("[{name}] reject_all_docx: {e:?}"));
        let rejected_bytes = rejected_archive
            .write()
            .unwrap_or_else(|e| panic!("[{name}] write rejected: {e:?}"));

        let runtime2 = SimpleRuntime::new();
        let import_rejected = runtime2
            .import_docx(&rejected_bytes)
            .unwrap_or_else(|e| panic!("[{name}] import rejected: {e:?}"));
        let import_before = runtime2
            .import_docx(&before_bytes)
            .unwrap_or_else(|e| panic!("[{name}] import before: {e:?}"));

        let rejected_text = normalize_doc_text(&canon_paragraph_texts(&import_rejected.canonical));
        let before_text = normalize_doc_text(&canon_paragraph_texts(&import_before.canonical));

        if rejected_text != before_text {
            failures.push(format!(
                "[{name}] reject-all text DIFFERS from before.docx \
                 (rejected_len={}, before_len={})",
                rejected_text.len(),
                before_text.len(),
            ));
        }

        total_checked += 1;
    }

    eprintln!(
        "normalize_text_fidelity: checked {} samples, {} failures",
        total_checked,
        failures.len(),
    );

    assert!(
        failures.is_empty(),
        "text fidelity violations:\n{}",
        failures.join("\n"),
    );
}
