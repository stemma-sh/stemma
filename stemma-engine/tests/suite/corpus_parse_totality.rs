//! Parse totality: does `import_docx` handle every file in the stress corpus
//! without panicking?
//!
//! For valid files: parse must succeed (error = our bug).
//! For invalid files: parse may return an error, but must not panic/crash.
//!
//! Run:
//!   RUST_MIN_STACK=67108864 \
//!   cargo test --release --test corpus_parse_totality -- --ignored --nocapture

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use rayon::prelude::*;

use stemma::{DocxRuntime, SimpleRuntime};

use crate::common;

// ── Result tracking ──────────────────────────────────────────────────────

struct ParseResult {
    path: String,
    pool: &'static str, // "valid" or "invalid"
    outcome: Outcome,
}

#[allow(dead_code)]
enum Outcome {
    Ok,
    Err(String),
    Panic(String),
}

// ── Test ─────────────────────────────────────────────────────────────────

#[test]
#[ignore = "corpus parse totality — run on demand"]
fn parse_totality() {
    let corpus_dir = common::stress_dir().join("docxcorpus");
    assert!(
        corpus_dir.is_dir(),
        "docxcorpus not found at {}",
        corpus_dir.display()
    );

    let valid_dir = corpus_dir.join("valid");
    let invalid_dir = corpus_dir.join("invalid");

    // Discover files
    eprintln!("Scanning for .docx files ...");
    let valid_files = discover_docx(&valid_dir, "valid");
    let invalid_files = discover_docx(&invalid_dir, "invalid");
    let total = valid_files.len() + invalid_files.len();
    eprintln!(
        "Found {} files ({} valid, {} invalid)",
        total,
        valid_files.len(),
        invalid_files.len()
    );

    let all_files: Vec<(PathBuf, &'static str)> = valid_files
        .into_iter()
        .map(|p| (p, "valid"))
        .chain(invalid_files.into_iter().map(|p| (p, "invalid")))
        .collect();

    // Process in parallel
    let failures: Mutex<Vec<ParseResult>> = Mutex::new(Vec::new());
    let processed = AtomicUsize::new(0);
    let ok_count = AtomicUsize::new(0);
    let err_count = AtomicUsize::new(0);
    let panic_count = AtomicUsize::new(0);
    let start = Instant::now();

    all_files.par_iter().for_each(|(path, pool)| {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                failures.lock().unwrap().push(ParseResult {
                    path: path.display().to_string(),
                    pool,
                    outcome: Outcome::Err(format!("read error: {e}")),
                });
                err_count.fetch_add(1, Ordering::Relaxed);
                return;
            }
        };

        let rel_path = path
            .strip_prefix(&corpus_dir)
            .unwrap_or(path)
            .display()
            .to_string();

        // Catch panics — the core invariant is "never panic"
        let result = std::panic::catch_unwind(|| {
            let runtime = SimpleRuntime::new();
            runtime.import_docx(&bytes)
        });

        match result {
            Ok(Ok(_import)) => {
                ok_count.fetch_add(1, Ordering::Relaxed);
            }
            Ok(Err(e)) => {
                // Error return is acceptable for invalid files, but a bug for valid files
                if *pool == "valid" {
                    failures.lock().unwrap().push(ParseResult {
                        path: rel_path,
                        pool,
                        outcome: Outcome::Err(format!("{:?}", e)),
                    });
                }
                err_count.fetch_add(1, Ordering::Relaxed);
            }
            Err(panic_info) => {
                let msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                    s.to_string()
                } else if let Some(s) = panic_info.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "unknown panic".to_string()
                };
                failures.lock().unwrap().push(ParseResult {
                    path: rel_path,
                    pool,
                    outcome: Outcome::Panic(msg),
                });
                panic_count.fetch_add(1, Ordering::Relaxed);
            }
        }

        let n = processed.fetch_add(1, Ordering::Relaxed) + 1;
        if n.is_multiple_of(5000) {
            let elapsed = start.elapsed().as_secs_f64();
            let rate = n as f64 / elapsed;
            let eta = (total - n) as f64 / rate / 60.0;
            eprintln!("  [{n:>7}/{total:>7}] {rate:.0} files/s | ETA {eta:.0}m");
        }
    });

    let elapsed = start.elapsed();
    let ok = ok_count.load(Ordering::Relaxed);
    let errs = err_count.load(Ordering::Relaxed);
    let panics = panic_count.load(Ordering::Relaxed);

    eprintln!();
    eprintln!(
        "=== Parse Totality Results ({:.0}s) ===",
        elapsed.as_secs_f64()
    );
    eprintln!("  Total:   {total:>8}");
    eprintln!("  OK:      {ok:>8}");
    eprintln!("  Errors:  {errs:>8}");
    eprintln!("  Panics:  {panics:>8}");

    let failures = failures.into_inner().unwrap();

    // Report panics (always bugs)
    let panics_list: Vec<_> = failures
        .iter()
        .filter(|f| matches!(f.outcome, Outcome::Panic(_)))
        .collect();
    if !panics_list.is_empty() {
        eprintln!();
        eprintln!("=== PANICS ({}) — these are bugs ===", panics_list.len());
        for f in &panics_list {
            if let Outcome::Panic(msg) = &f.outcome {
                eprintln!("  [{}] {} — {}", f.pool, f.path, msg);
            }
        }
    }

    // Report valid-file errors (likely bugs)
    let valid_errs: Vec<_> = failures
        .iter()
        .filter(|f| f.pool == "valid" && matches!(f.outcome, Outcome::Err(_)))
        .collect();
    if !valid_errs.is_empty() {
        eprintln!();
        eprintln!(
            "=== VALID-FILE ERRORS ({}) — likely bugs ===",
            valid_errs.len()
        );
        for f in valid_errs.iter().take(50) {
            if let Outcome::Err(msg) = &f.outcome {
                let short = if msg.len() > 120 { &msg[..120] } else { msg };
                eprintln!("  {} — {}", f.path, short);
            }
        }
        if valid_errs.len() > 50 {
            eprintln!("  ... and {} more", valid_errs.len() - 50);
        }
    }

    // Hard gate: no panics allowed, ever
    assert_eq!(panics, 0, "{panics} files caused panics — see above");
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn discover_docx(dir: &Path, _label: &str) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if dir.is_dir() {
        collect_docx_recursive(dir, &mut files);
    }
    files.sort();
    files
}

fn collect_docx_recursive(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_docx_recursive(&path, out);
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("docx"))
        {
            out.push(path);
        }
    }
}
