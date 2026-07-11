//! Scout for false and noisy BlockModified entries across all sample fixtures.
//!
//! - **False modification**: old_text and new_text have word-overlap similarity < 0.30
//!   (unrelated paragraphs forced into alignment). Only flagged when old_text >= 30 chars.
//! - **Noisy modification**: highlight ratio > 0.80 (nearly everything is Inserted/Deleted,
//!   very little Unchanged). Only flagged when total text >= 50 chars and the block is NOT
//!   already a false modification.
//!
//! Run with:
//! ```
//! RUST_MIN_STACK=67108864 cargo test --test scout_noisy_mods -- --ignored --nocapture
//! ```

use std::collections::HashSet;
use std::fs;
use std::panic;
use std::path::PathBuf;

use stemma::{DiffChange, DocxRuntime, InlineChange, SimpleRuntime};

struct Hit {
    sample: String,
    old_preview: String,
    new_preview: String,
    similarity: f64,
    highlight_ratio: f64,
}

/// Word-overlap Jaccard similarity: |intersection| / |union| over whitespace-split words.
fn word_similarity(a: &str, b: &str) -> f64 {
    let words_a: HashSet<&str> = a.split_whitespace().collect();
    let words_b: HashSet<&str> = b.split_whitespace().collect();
    let union_size = words_a.union(&words_b).count();
    if union_size == 0 {
        return 1.0; // both empty → identical
    }
    let intersection_size = words_a.intersection(&words_b).count();
    intersection_size as f64 / union_size as f64
}

/// Highlight ratio: chars in Inserted + Deleted spans / total chars across all spans.
fn highlight_ratio(inline_changes: &[InlineChange]) -> f64 {
    let mut highlighted = 0usize;
    let mut total = 0usize;
    for ic in inline_changes {
        match ic {
            InlineChange::Inserted { text, .. } | InlineChange::Deleted { text, .. } => {
                highlighted += text.len();
                total += text.len();
            }
            InlineChange::Unchanged { text, .. } => {
                total += text.len();
            }
            InlineChange::Opaque { .. } => {
                // Opaque segments (drawings, equations, etc.) — skip for char counting.
            }
        }
    }
    if total == 0 {
        return 0.0;
    }
    highlighted as f64 / total as f64
}

fn preview(s: &str, max: usize) -> String {
    let clean: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if clean.len() <= max {
        clean
    } else {
        format!("{}...", &clean[..max.min(clean.len())])
    }
}

/// Total text length across all inline spans (Unchanged + Inserted + Deleted).
fn total_inline_text_len(inline_changes: &[InlineChange]) -> usize {
    let mut total = 0usize;
    for ic in inline_changes {
        match ic {
            InlineChange::Inserted { text, .. }
            | InlineChange::Deleted { text, .. }
            | InlineChange::Unchanged { text, .. } => {
                total += text.len();
            }
            InlineChange::Opaque { .. } => {}
        }
    }
    total
}

#[test]
#[ignore = "diagnostic scout — run with --ignored --nocapture"]
fn scout_noisy_mods() {
    let samples_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("backend")
        .join("samples");

    let mut entries: Vec<_> = fs::read_dir(&samples_dir)
        .unwrap_or_else(|e| panic!("cannot read samples dir {}: {}", samples_dir.display(), e))
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
        .collect();

    entries.sort_by_key(|e| e.file_name());

    let mut false_mods: Vec<Hit> = Vec::new();
    let mut noisy_mods: Vec<Hit> = Vec::new();
    let mut skipped: Vec<(String, String)> = Vec::new();
    let mut total_samples = 0usize;

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

        total_samples += 1;

        for change in &diff.changes {
            if let DiffChange::BlockModified {
                old_text,
                new_text,
                inline_changes,
                ..
            } = change
            {
                let sim = word_similarity(old_text, new_text);
                let hr = highlight_ratio(inline_changes);
                let total_len = total_inline_text_len(inline_changes);

                let is_false_mod = sim < 0.30 && old_text.len() >= 30;

                if is_false_mod {
                    false_mods.push(Hit {
                        sample: name.clone(),
                        old_preview: preview(old_text, 60),
                        new_preview: preview(new_text, 60),
                        similarity: sim,
                        highlight_ratio: hr,
                    });
                } else if hr > 0.80 && total_len >= 50 {
                    noisy_mods.push(Hit {
                        sample: name.clone(),
                        old_preview: preview(old_text, 60),
                        new_preview: preview(new_text, 60),
                        similarity: sim,
                        highlight_ratio: hr,
                    });
                }
            }
        }
    }

    // Sort false mods by similarity ascending (worst first).
    false_mods.sort_by(|a, b| a.similarity.partial_cmp(&b.similarity).unwrap());

    // Sort noisy mods by highlight_ratio descending (worst first).
    noisy_mods.sort_by(|a, b| b.highlight_ratio.partial_cmp(&a.highlight_ratio).unwrap());

    // === Print results ===

    if !skipped.is_empty() {
        eprintln!();
        eprintln!("=== SKIPPED SAMPLES ===");
        for (name, reason) in &skipped {
            eprintln!("  SKIP  {name}: {reason}");
        }
    }

    eprintln!();
    eprintln!("=== FALSE MODIFICATIONS (similarity < 0.30, old_text >= 30 chars) ===");
    eprintln!();
    if false_mods.is_empty() {
        eprintln!("  (none)");
    } else {
        eprintln!(
            "  {:<35} {:>6} {:>6}  {:<62} {:<62}",
            "SAMPLE", "SIM", "HL_R", "OLD_TEXT", "NEW_TEXT"
        );
        eprintln!("  {}", "-".repeat(175));
        for h in &false_mods {
            eprintln!(
                "  {:<35} {:>6.3} {:>6.3}  {:<62} {:<62}",
                h.sample, h.similarity, h.highlight_ratio, h.old_preview, h.new_preview
            );
        }
    }

    eprintln!();
    eprintln!(
        "=== NOISY MODIFICATIONS (highlight_ratio > 0.80, total >= 50 chars, not false mod) ==="
    );
    eprintln!();
    if noisy_mods.is_empty() {
        eprintln!("  (none)");
    } else {
        eprintln!(
            "  {:<35} {:>6} {:>6}  {:<62} {:<62}",
            "SAMPLE", "SIM", "HL_R", "OLD_TEXT", "NEW_TEXT"
        );
        eprintln!("  {}", "-".repeat(175));
        for h in &noisy_mods {
            eprintln!(
                "  {:<35} {:>6.3} {:>6.3}  {:<62} {:<62}",
                h.sample, h.similarity, h.highlight_ratio, h.old_preview, h.new_preview
            );
        }
    }

    eprintln!();
    eprintln!("=== SUMMARY ===");
    eprintln!("  Total samples processed: {total_samples}");
    eprintln!("  Skipped:                 {}", skipped.len());
    eprintln!("  False modifications:     {}", false_mods.len());
    eprintln!("  Noisy modifications:     {}", noisy_mods.len());
    eprintln!();
}
