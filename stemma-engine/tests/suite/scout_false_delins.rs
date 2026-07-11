//! Scout for false delete+insert pairs in the diff output.
//!
//! Finds cases where the diff algorithm shows a paragraph as fully deleted
//! and fully inserted when it should be an inline modification. These are
//! "obviously the same paragraph" cases where word overlap is high.
//!
//! Run with:
//! ```
//! RUST_MIN_STACK=67108864 cargo test --test scout_false_delins -- --ignored --nocapture
//! ```

use std::collections::HashSet;
use std::fs;
use std::panic;
use std::path::PathBuf;

use stemma::{BlockNode, DiffChange, DocxRuntime, InlineNode, SimpleRuntime};

/// Extract plain text from a BlockNode::Paragraph by iterating all_inlines().
fn paragraph_text(block: &BlockNode) -> Option<String> {
    match block {
        BlockNode::Paragraph(p) => {
            let text: String = p
                .all_inlines()
                .filter_map(|inline| match inline {
                    InlineNode::Text(t) => Some(t.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            Some(text)
        }
        _ => None,
    }
}

/// Word-overlap similarity: shared words / total unique words.
fn word_overlap_similarity(a: &str, b: &str) -> f64 {
    let words_a: HashSet<&str> = a.split_whitespace().collect();
    let words_b: HashSet<&str> = b.split_whitespace().collect();

    if words_a.is_empty() && words_b.is_empty() {
        return 0.0;
    }

    let shared = words_a.intersection(&words_b).count();
    let total = words_a.union(&words_b).count();

    if total == 0 {
        0.0
    } else {
        shared as f64 / total as f64
    }
}

/// Truncate a string to at most `max` chars, appending "..." if truncated.
fn preview(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.replace('\n', "\\n")
    } else {
        let truncated: String = s.chars().take(max).collect();
        format!("{}...", truncated.replace('\n', "\\n"))
    }
}

struct FalseDelIns {
    sample: String,
    similarity: f64,
    deleted_preview: String,
    inserted_preview: String,
}

#[test]
#[ignore = "diagnostic sweep — run with --ignored --nocapture"]
fn scout_false_delete_insert_pairs() {
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

    let mut hits: Vec<FalseDelIns> = Vec::new();
    let mut total_scanned = 0usize;
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

        total_scanned += 1;

        // Collect deleted and inserted paragraph entries with their positions.
        let mut deletions: Vec<(usize, &str)> = Vec::new(); // (index, old_text)
        let mut insertions: Vec<(usize, String)> = Vec::new(); // (index, text from block)

        for (i, change) in diff.changes.iter().enumerate() {
            match change {
                DiffChange::BlockDeleted {
                    old_text,
                    old_block,
                    ..
                } => {
                    // Only paragraphs, not tables.
                    if matches!(old_block, BlockNode::Paragraph(_)) {
                        deletions.push((i, old_text.as_str()));
                    }
                }
                DiffChange::BlockInserted { block, .. } => {
                    if let Some(text) = paragraph_text(block) {
                        insertions.push((i, text));
                    }
                }
                _ => {}
            }
        }

        // For each deleted paragraph, check nearby inserted paragraphs.
        for &(del_idx, del_text) in &deletions {
            if del_text.len() < 50 {
                continue;
            }

            for (ins_idx, ins_text) in &insertions {
                // Within 30 positions.
                let distance = if *ins_idx > del_idx {
                    ins_idx - del_idx
                } else {
                    del_idx - ins_idx
                };
                if distance > 30 {
                    continue;
                }

                let sim = word_overlap_similarity(del_text, ins_text);
                if sim > 0.50 {
                    hits.push(FalseDelIns {
                        sample: name.clone(),
                        similarity: sim,
                        deleted_preview: preview(del_text, 80),
                        inserted_preview: preview(ins_text, 80),
                    });
                }
            }
        }
    }

    // Sort by similarity descending (worst offenders first).
    hits.sort_by(|a, b| b.similarity.partial_cmp(&a.similarity).unwrap());

    // Print skipped samples.
    if !skipped.is_empty() {
        eprintln!();
        eprintln!("=== SKIPPED SAMPLES ===");
        for (name, reason) in &skipped {
            eprintln!("  SKIP  {name}: {reason}");
        }
        eprintln!();
    }

    // Print hits.
    eprintln!();
    eprintln!("=== FALSE DELETE+INSERT PAIRS (similarity > 0.50, deleted text >= 50 chars) ===");
    eprintln!();

    if hits.is_empty() {
        eprintln!("  (none found)");
    } else {
        for (i, hit) in hits.iter().enumerate() {
            eprintln!(
                "  #{:<3} sample={:<40} similarity={:.3}",
                i + 1,
                hit.sample,
                hit.similarity
            );
            eprintln!("       DEL: {}", hit.deleted_preview);
            eprintln!("       INS: {}", hit.inserted_preview);
            eprintln!();
        }
    }

    eprintln!("=== SUMMARY ===");
    eprintln!("  Total samples scanned:          {total_scanned}");
    eprintln!("  Skipped:                        {}", skipped.len());
    eprintln!("  Total false del+ins pairs found: {}", hits.len());
    eprintln!();
}
