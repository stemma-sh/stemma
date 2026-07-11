//! Dump all false-modification candidates (BlockModified where old and new text
//! are very different) to a JSONL file for human/LLM review.
//!
//! Run with:
//! ```
//! RUST_MIN_STACK=67108864 cargo test --test dump_false_mods -- --ignored --nocapture
//! ```

use std::collections::HashSet;
use std::fs;
use std::io::{BufWriter, Write};
use std::panic;
use std::path::PathBuf;

use serde::Serialize;
use stemma::{DiffChange, DocxRuntime, InlineChange, SimpleRuntime};

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

/// Build inline summary string: unchanged text as-is, deletions as `[-text-]`,
/// insertions as `[+text+]`, opaques as `[opaque]`.
fn build_inline_summary(inline_changes: &[InlineChange]) -> String {
    let mut out = String::new();
    for ic in inline_changes {
        match ic {
            InlineChange::Unchanged { text, .. } => out.push_str(text),
            InlineChange::Deleted { text, .. } => {
                out.push_str("[-");
                out.push_str(text);
                out.push_str("-]");
            }
            InlineChange::Inserted { text, .. } => {
                out.push_str("[+");
                out.push_str(text);
                out.push_str("+]");
            }
            InlineChange::Opaque { .. } => {
                out.push_str("[opaque]");
            }
        }
    }
    out
}

/// Extract a text preview from a DiffChange (for context fields).
fn change_text_preview(change: &DiffChange, max_len: usize) -> String {
    let text = match change {
        DiffChange::BlockDeleted { old_text, .. } => old_text.as_str(),
        DiffChange::BlockInserted { .. } => {
            return "[inserted block]".to_string();
        }
        DiffChange::BlockModified {
            old_text, new_text, ..
        } => {
            // Show both old and new
            let combined = format!("old: {} | new: {}", old_text, new_text);
            return truncate_str(&combined, max_len);
        }
        DiffChange::TableStructureChanged { old_text, .. } => old_text.as_str(),
        _ => return String::new(),
    };
    truncate_str(text, max_len)
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        // Find a char boundary
        let mut end = max_len;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &s[..end])
    }
}

#[derive(Serialize)]
struct FalseModEntry {
    sample: String,
    old_text: String,
    new_text: String,
    word_similarity: f64,
    old_len: usize,
    new_len: usize,
    inline_summary: String,
    context_before: Option<String>,
    context_after: Option<String>,
}

#[test]
#[ignore = "diagnostic dump — run with --ignored --nocapture"]
fn dump_false_mods_for_review() {
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

    let output_path = "/tmp/false_mods_dump.jsonl";
    let file = fs::File::create(output_path)
        .unwrap_or_else(|e| panic!("cannot create {output_path}: {e}"));
    let mut writer = BufWriter::new(file);

    let mut total_candidates = 0usize;
    let mut total_samples = 0usize;
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

        let changes = &diff.changes;
        for (idx, change) in changes.iter().enumerate() {
            if let DiffChange::BlockModified {
                old_text,
                new_text,
                inline_changes,
                ..
            } = change
            {
                let sim = word_similarity(old_text, new_text);

                if sim < 0.30 && old_text.len() >= 30 {
                    let inline_summary = build_inline_summary(inline_changes);

                    let context_before = if idx > 0 {
                        Some(change_text_preview(&changes[idx - 1], 200))
                    } else {
                        None
                    };
                    let context_after = if idx + 1 < changes.len() {
                        Some(change_text_preview(&changes[idx + 1], 200))
                    } else {
                        None
                    };

                    let entry = FalseModEntry {
                        sample: name.clone(),
                        old_text: old_text.clone(),
                        new_text: new_text.clone(),
                        word_similarity: sim,
                        old_len: old_text.len(),
                        new_len: new_text.len(),
                        inline_summary,
                        context_before,
                        context_after,
                    };

                    let json_line =
                        serde_json::to_string(&entry).expect("failed to serialize FalseModEntry");
                    writeln!(writer, "{}", json_line).expect("failed to write to output file");

                    total_candidates += 1;
                }
            }
        }
    }

    writer.flush().expect("failed to flush output file");

    if !skipped.is_empty() {
        eprintln!();
        eprintln!("=== SKIPPED SAMPLES ===");
        for (name, reason) in &skipped {
            eprintln!("  SKIP  {name}: {reason}");
        }
    }

    eprintln!();
    eprintln!("=== SUMMARY ===");
    eprintln!("  Total samples processed: {total_samples}");
    eprintln!("  Skipped:                 {}", skipped.len());
    eprintln!("  False mod candidates:    {total_candidates}");
    eprintln!("  Output:                  {output_path}");
    eprintln!();
}
