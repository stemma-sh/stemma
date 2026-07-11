//! Identity invariant: `diff(A, A)` must produce zero changes, and
//! `redline(A, A)` must have zero tracked-change markup.
//!
//! This catches normalization asymmetries in parsing (two imports of the same
//! bytes produce different CanonDocs) and phantom tracked-change markup
//! injected by the redline pipeline when there are no actual differences.

use std::collections::BTreeSet;
use std::fs;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use rayon::prelude::*;
use stemma::{
    DocxRuntime, ExportMode, SimpleRuntime, TransactionMeta,
    redline_extract::{RedlineSpan, extract_redline},
    runtime::{ErrorCode, RuntimeError, ValidatorLevel, gate_serialized_bytes},
};

use crate::common;

// ── helpers ──────────────────────────────────────────────────────────────

fn redline_meta() -> TransactionMeta {
    TransactionMeta {
        author: "identity_invariant".to_string(),
        reason: Some("identity invariant test".to_string()),
        timestamp_utc: Some("2025-06-01T00:00:00Z".to_string()),
    }
}

/// Identity-sweep skip policy for engine errors on wild stress docs.
///
/// Two error classes are honest, documented outcomes rather than invariant
/// violations:
/// - `UnsupportedEdit`: a designed refusal (e.g. compare refuses quarantined
///   nested-tracked-change blocks rather than comparing dishonestly).
/// - `ValidationFailed` when the INPUT bytes already fail the Blocking gate:
///   the input is invalid OOXML (dangling rels, wrong content types, …) — the
///   engine refuses to emit an invalid package rather than launder it, and an
///   invalid input is not identity-testable.
///
/// Everything else (including `ValidationFailed` on a clean input — i.e. the
/// engine ITSELF corrupted the package) stays a failure.
fn identity_skip_reason(err: &RuntimeError, input_bytes: &[u8]) -> Option<String> {
    match err.code {
        ErrorCode::UnsupportedEdit => Some(format!(
            "documented refusal: {}",
            err.message.lines().next().unwrap_or("")
        )),
        ErrorCode::ValidationFailed => {
            match gate_serialized_bytes(input_bytes, ValidatorLevel::Blocking) {
                Err(input_err) => Some(format!(
                    "input already fails the Blocking gate: {}",
                    input_err.message.lines().nth(1).unwrap_or("")
                )),
                Ok(()) => None,
            }
        }
        _ => None,
    }
}

/// Collect unique DOCX paths from fixture pairs (both before.docx and after.docx).
fn discover_fixture_docs() -> Vec<String> {
    let mut paths = BTreeSet::new();

    // Top-level testdata
    if let Ok(entries) = fs::read_dir("testdata") {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                for name in ["before.docx", "after.docx"] {
                    let docx = path.join(name);
                    if docx.exists() {
                        paths.insert(docx.to_string_lossy().to_string());
                    }
                }
            }
        }
    }

    // Synthesized testdata
    if let Ok(entries) = fs::read_dir("testdata/synthesized") {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                for name in ["before.docx", "after.docx"] {
                    let docx = path.join(name);
                    if docx.exists() {
                        paths.insert(docx.to_string_lossy().to_string());
                    }
                }
            }
        }
    }

    paths.into_iter().collect()
}

// ── stress doc discovery ─────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Deserialize)]
struct FixtureExpectation {
    path: String,
    expected_outcome: String,
    #[allow(dead_code)]
    expected_reason: String,
}

#[derive(Debug, serde::Deserialize)]
struct StressManifest {
    #[allow(dead_code)]
    version: u32,
    fixtures: Vec<FixtureExpectation>,
}

/// Load stress manifest and return paths of parseable docs (pass_supported,
/// fail_regression, and renderer_unloadable fail_unsupported).
fn discover_parseable_stress_docs() -> Vec<String> {
    let manifest_path = common::stress_dir().join("manifest.json");
    let raw = match fs::read_to_string(&manifest_path) {
        Ok(raw) => raw,
        Err(_) => return Vec::new(),
    };
    let manifest: StressManifest = match serde_json::from_str(&raw) {
        Ok(m) => m,
        Err(_) => return Vec::new(),
    };

    let stress_dir = common::stress_dir();
    let mut paths: Vec<String> = manifest
        .fixtures
        .into_iter()
        .filter(|f| {
            matches!(
                f.expected_outcome.as_str(),
                "pass_supported" | "fail_regression"
            ) || (f.expected_outcome == "fail_unsupported"
                && f.expected_reason == "renderer_unloadable")
        })
        .map(|f| {
            f.path
                .strip_prefix("stress/")
                .map(|rel| stress_dir.join(rel).to_string_lossy().to_string())
                .unwrap_or(f.path)
        })
        .collect();
    paths.sort();
    paths
}

// ── identity invariant: fixture docs ─────────────────────────────────────

/// For every unique DOCX in the fixture suite, importing the same bytes twice
/// and diffing must produce zero changes, and redlining must produce zero
/// tracked-change markup.
#[test]
#[ignore] // Slow fixture sweep — run via `just identity` or `just nightly`
fn diff_identity_zero_changes() {
    let docs = discover_fixture_docs();
    assert!(!docs.is_empty(), "no fixture docs found");

    let mut total_checked = 0;
    let mut failures = Vec::new();

    for doc_path in &docs {
        let bytes = fs::read(doc_path).unwrap_or_else(|err| panic!("[{doc_path}] read: {err}"));

        let runtime = SimpleRuntime::new();

        let import_a = match runtime.import_docx(&bytes) {
            Ok(r) => r,
            Err(err) => {
                // Skip docs that fail to parse — parse totality is tested elsewhere.
                eprintln!("[{doc_path}] skipping (import failed): {err:?}");
                continue;
            }
        };
        let import_b = match runtime.import_docx(&bytes) {
            Ok(r) => r,
            Err(err) => {
                eprintln!("[{doc_path}] skipping (second import failed): {err:?}");
                continue;
            }
        };

        // Check 1: diff must produce zero changes.
        let diff = runtime
            .diff(&import_a.doc_handle, &import_b.doc_handle)
            .unwrap_or_else(|err| panic!("[{doc_path}] diff: {err:?}"));

        if !diff.changes.is_empty() {
            failures.push(format!(
                "[{doc_path}] diff(A, A) produced {} change(s): {:?}",
                diff.changes.len(),
                diff.changes
                    .iter()
                    .take(5)
                    .map(|c| format!("{c:?}").chars().take(200).collect::<String>())
                    .collect::<Vec<_>>()
            ));
        }

        // Check 2: redline must have no tracked-change markup.
        let apply = runtime
            .diff_and_redline(&import_a.doc_handle, &import_b.doc_handle, redline_meta())
            .unwrap_or_else(|err| panic!("[{doc_path}] diff_and_redline: {err:?}"));
        assert!(
            apply.applied,
            "[{doc_path}] redline must be marked as applied"
        );

        let exported = runtime
            .export_docx(&import_a.doc_handle, ExportMode::Redline)
            .unwrap_or_else(|err| panic!("[{doc_path}] export_docx: {err:?}"));

        let extract = extract_redline(&exported)
            .unwrap_or_else(|err| panic!("[{doc_path}] extract_redline: {err}"));

        let has_tracked_changes = extract.body.iter().any(|para| {
            para.spans
                .iter()
                .any(|s| matches!(s, RedlineSpan::Inserted(_) | RedlineSpan::Deleted(_)))
        });

        if has_tracked_changes {
            let tracked: Vec<_> = extract
                .body
                .iter()
                .flat_map(|p| p.spans.iter())
                .filter(|s| matches!(s, RedlineSpan::Inserted(_) | RedlineSpan::Deleted(_)))
                .take(5)
                .collect();
            failures.push(format!(
                "[{doc_path}] redline(A, A) has tracked-change markup: {tracked:?}"
            ));
        }

        total_checked += 1;
    }

    assert!(
        total_checked > 0,
        "expected at least one fixture doc to be checked"
    );
    eprintln!(
        "identity invariant: checked {} fixture docs, {} failures",
        total_checked,
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "identity invariant violated in {} doc(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

// ── identity invariant: stress docs ──────────────────────────────────────

#[test]
#[ignore = "expensive stress identity invariant suite"]
fn stress_diff_identity_zero_changes() {
    let result = thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(stress_diff_identity_zero_changes_inner)
        .expect("failed to spawn thread")
        .join();

    match result {
        Ok(()) => {}
        Err(panic_payload) => std::panic::resume_unwind(panic_payload),
    }
}

fn stress_diff_identity_zero_changes_inner() {
    let docs = discover_parseable_stress_docs();
    assert!(!docs.is_empty(), "no parseable stress docs found");

    let pool = rayon::ThreadPoolBuilder::new()
        .stack_size(64 * 1024 * 1024)
        .build()
        .expect("failed to build rayon thread pool");

    let total_checked = AtomicUsize::new(0);
    let failures = Mutex::new(Vec::<String>::new());

    pool.install(|| {
        docs.par_iter().for_each(|doc_path| {
            let bytes = match fs::read(doc_path) {
                Ok(b) => b,
                Err(err) => {
                    eprintln!("[{doc_path}] skipping (read failed): {err}");
                    return;
                }
            };

            let runtime = SimpleRuntime::new();

            let import_a = match runtime.import_docx(&bytes) {
                Ok(r) => r,
                Err(err) => {
                    eprintln!("[{doc_path}] skipping (import failed): {err:?}");
                    return;
                }
            };
            let import_b = match runtime.import_docx(&bytes) {
                Ok(r) => r,
                Err(err) => {
                    eprintln!("[{doc_path}] skipping (second import failed): {err:?}");
                    return;
                }
            };

            // Check 1: diff must produce zero changes.
            let diff = match runtime.diff(&import_a.doc_handle, &import_b.doc_handle) {
                Ok(d) => d,
                Err(err) => {
                    if let Some(reason) = identity_skip_reason(&err, &bytes) {
                        eprintln!("[{doc_path}] skipping ({reason})");
                    } else {
                        failures
                            .lock()
                            .unwrap()
                            .push(format!("[{doc_path}] diff failed: {err:?}"));
                    }
                    return;
                }
            };

            if !diff.changes.is_empty() {
                failures.lock().unwrap().push(format!(
                    "[{doc_path}] diff(A, A) produced {} change(s)",
                    diff.changes.len(),
                ));
            }

            // Check 2: redline must have no tracked-change markup.
            let apply = match runtime.diff_and_redline(
                &import_a.doc_handle,
                &import_b.doc_handle,
                redline_meta(),
            ) {
                Ok(a) => a,
                Err(err) => {
                    if let Some(reason) = identity_skip_reason(&err, &bytes) {
                        eprintln!("[{doc_path}] skipping ({reason})");
                    } else {
                        failures
                            .lock()
                            .unwrap()
                            .push(format!("[{doc_path}] diff_and_redline failed: {err:?}"));
                    }
                    return;
                }
            };

            if !apply.applied {
                failures
                    .lock()
                    .unwrap()
                    .push(format!("[{doc_path}] redline must be marked as applied"));
                return;
            }

            let exported = match runtime.export_docx(&import_a.doc_handle, ExportMode::Redline) {
                Ok(e) => e,
                Err(err) => {
                    failures
                        .lock()
                        .unwrap()
                        .push(format!("[{doc_path}] export_docx failed: {err:?}"));
                    return;
                }
            };

            let extract = match extract_redline(&exported) {
                Ok(e) => e,
                Err(err) => {
                    failures
                        .lock()
                        .unwrap()
                        .push(format!("[{doc_path}] extract_redline failed: {err}"));
                    return;
                }
            };

            let has_tracked_changes = extract.body.iter().any(|para| {
                para.spans
                    .iter()
                    .any(|s| matches!(s, RedlineSpan::Inserted(_) | RedlineSpan::Deleted(_)))
            });

            if has_tracked_changes {
                failures.lock().unwrap().push(format!(
                    "[{doc_path}] redline(A, A) has tracked-change markup"
                ));
            }

            total_checked.fetch_add(1, Ordering::Relaxed);
        });
    });

    let total_checked = total_checked.load(Ordering::Relaxed);
    let failures = failures.into_inner().unwrap();
    assert!(
        total_checked > 0,
        "expected at least one stress doc to be checked"
    );
    eprintln!(
        "stress identity invariant: checked {} stress docs, {} failures",
        total_checked,
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "stress identity invariant violated in {} doc(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
}
