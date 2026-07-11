//! Diagnostic sweep: measures diff UX quality across all sample fixtures.
//!
//! Run with:
//! ```
//! RUST_MIN_STACK=67108864 cargo test --test diff_quality_sweep -- --ignored --nocapture
//! ```

use crate::common;

use std::fs;
use std::panic;

use stemma::{DiffChange, DocxRuntime, InlineChange, SimpleRuntime};

/// Per-sample quality metrics.
struct SampleMetrics {
    name: String,
    modified: usize,
    deleted: usize,
    inserted: usize,
    highlight_volume: usize,
    mod_ratio: f64,
}

/// Compute highlight volume (total chars of highlighted/changed text):
/// - BlockDeleted: chars in old_text
/// - BlockModified: chars in Inserted + Deleted inline spans
fn compute_highlight_volume(changes: &[DiffChange]) -> usize {
    let mut volume = 0usize;
    for change in changes {
        match change {
            DiffChange::BlockDeleted { old_text, .. } => {
                volume += old_text.len();
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
    volume
}

#[test]
#[ignore = "diagnostic sweep — run with --ignored --nocapture"]
fn quality_sweep_all_samples() {
    let samples_dir = common::samples_dir();

    let mut entries: Vec<_> = fs::read_dir(&samples_dir)
        .unwrap_or_else(|e| panic!("cannot read samples dir {}: {}", samples_dir.display(), e))
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
        .collect();

    entries.sort_by_key(|e| e.file_name());

    let mut metrics: Vec<SampleMetrics> = Vec::new();
    let mut skipped: Vec<(String, String)> = Vec::new();

    for entry in &entries {
        let dir_path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        let before_path = dir_path.join("before.docx");
        let after_path = dir_path.join("after.docx");

        if !before_path.exists() || !after_path.exists() {
            continue;
        }

        let before_bytes = match fs::read(&before_path) {
            Ok(b) => b,
            Err(e) => {
                skipped.push((name, format!("read before.docx: {e}")));
                continue;
            }
        };
        let after_bytes = match fs::read(&after_path) {
            Ok(b) => b,
            Err(e) => {
                skipped.push((name, format!("read after.docx: {e}")));
                continue;
            }
        };

        // Catch panics so one bad sample doesn't stop the sweep.
        let result = panic::catch_unwind(|| {
            let runtime = SimpleRuntime::new();
            let import_before = runtime.import_docx(&before_bytes)?;
            let import_after = runtime.import_docx(&after_bytes)?;
            let diff = runtime.diff(&import_before.doc_handle, &import_after.doc_handle)?;
            Ok::<_, stemma::RuntimeError>(diff)
        });

        let diff = match result {
            Ok(Ok(d)) => d,
            Ok(Err(e)) => {
                skipped.push((name, format!("runtime error: {e:?}")));
                continue;
            }
            Err(panic_info) => {
                let msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                    s.to_string()
                } else if let Some(s) = panic_info.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "unknown panic".to_string()
                };
                skipped.push((name, format!("panic: {msg}")));
                continue;
            }
        };

        let mut modified = 0usize;
        let mut deleted = 0usize;
        let mut inserted = 0usize;

        for change in &diff.changes {
            match change {
                DiffChange::BlockModified { .. } => modified += 1,
                DiffChange::BlockDeleted { .. } => deleted += 1,
                DiffChange::BlockInserted { .. } => inserted += 1,
                _ => { /* table/header/footer/footnote/endnote/comment changes */ }
            }
        }

        let total = modified + deleted + inserted;
        let mod_ratio = if total > 0 {
            modified as f64 / total as f64
        } else {
            1.0 // no changes = perfect (nothing to show)
        };

        let highlight_volume = compute_highlight_volume(&diff.changes);

        metrics.push(SampleMetrics {
            name,
            modified,
            deleted,
            inserted,
            highlight_volume,
            mod_ratio,
        });
    }

    // Sort by mod_ratio ascending (worst quality first).
    metrics.sort_by(|a, b| a.mod_ratio.partial_cmp(&b.mod_ratio).unwrap());

    // Print skipped samples.
    if !skipped.is_empty() {
        eprintln!();
        eprintln!("=== SKIPPED SAMPLES ===");
        for (name, reason) in &skipped {
            eprintln!("  SKIP  {name}: {reason}");
        }
        eprintln!();
    }

    // Print table header.
    eprintln!();
    eprintln!(
        "{:<45} {:>5} {:>5} {:>5} {:>10} {:>9}",
        "SAMPLE", "MOD", "DEL", "INS", "HL_VOL", "MOD_RATIO"
    );
    eprintln!("{}", "-".repeat(85));

    for m in &metrics {
        eprintln!(
            "{:<45} {:>5} {:>5} {:>5} {:>10} {:>9.3}",
            m.name, m.modified, m.deleted, m.inserted, m.highlight_volume, m.mod_ratio
        );
    }

    eprintln!("{}", "-".repeat(85));

    // Summary stats.
    let total_samples = metrics.len();
    let total_highlight_volume: usize = metrics.iter().map(|m| m.highlight_volume).sum();

    let median_mod_ratio = if metrics.is_empty() {
        f64::NAN
    } else {
        let ratios: Vec<f64> = metrics.iter().map(|m| m.mod_ratio).collect();
        let mid = ratios.len() / 2;
        if ratios.len().is_multiple_of(2) {
            (ratios[mid - 1] + ratios[mid]) / 2.0
        } else {
            ratios[mid]
        }
    };

    eprintln!();
    eprintln!("SUMMARY");
    eprintln!("  Total samples processed: {total_samples}");
    eprintln!("  Skipped:                 {}", skipped.len());
    eprintln!("  Median mod_ratio:        {median_mod_ratio:.3}");
    eprintln!("  Total highlight_volume:  {total_highlight_volume}");
    eprintln!();
}
