//! Scout: find paragraph moves that the diff reports as separate delete + insert
//! instead of a single move operation.
//!
//! Run with:
//! ```
//! RUST_MIN_STACK=67108864 cargo test --test scout_orphan_moves -- --ignored --nocapture
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
                .collect();
            Some(text)
        }
        _ => None,
    }
}

/// Normalize text: lowercase, collapse whitespace, trim.
fn normalize(s: &str) -> String {
    s.split_whitespace()
        .map(|w| w.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Compute word overlap ratio between two texts.
/// Returns fraction of words in common relative to the larger set.
fn word_overlap(a: &str, b: &str) -> f64 {
    let words_a: Vec<&str> = a.split_whitespace().collect();
    let words_b: Vec<&str> = b.split_whitespace().collect();
    if words_a.is_empty() || words_b.is_empty() {
        return 0.0;
    }
    let set_a: HashSet<&str> = words_a.iter().copied().collect();
    let set_b: HashSet<&str> = words_b.iter().copied().collect();
    let intersection = set_a.intersection(&set_b).count();
    let max_size = words_a.len().max(words_b.len());
    intersection as f64 / max_size as f64
}

/// Truncate a string to at most `max_chars` characters, appending "..." if truncated.
fn preview(s: &str, max_chars: usize) -> String {
    if s.len() <= max_chars {
        s.to_string()
    } else {
        let boundary = s
            .char_indices()
            .nth(max_chars)
            .map(|(i, _)| i)
            .unwrap_or(s.len());
        format!("{}...", &s[..boundary])
    }
}

struct OrphanMove {
    sample: String,
    distance: usize,
    deleted_preview: String,
    inserted_preview: String,
    overlap: f64,
}

#[test]
#[ignore = "diagnostic scout — run with --ignored --nocapture"]
fn scout_orphan_moves() {
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

    let mut total_samples = 0usize;
    let mut skipped: Vec<(String, String)> = Vec::new();
    let mut orphan_moves: Vec<OrphanMove> = Vec::new();

    for entry in &entries {
        let dir_path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        let before_path = dir_path.join("before.docx");
        let after_path = dir_path.join("after.docx");

        if !before_path.exists() || !after_path.exists() {
            continue;
        }

        total_samples += 1;

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

        // Collect deleted paragraphs (without move_id) with their position index.
        let mut deletions: Vec<(usize, String)> = Vec::new();
        // Collect inserted paragraphs (without move_id) with their position index.
        let mut insertions: Vec<(usize, String)> = Vec::new();

        for (idx, change) in diff.changes.iter().enumerate() {
            match change {
                DiffChange::BlockDeleted {
                    old_block,
                    move_id: None,
                    ..
                } => {
                    if let Some(text) = paragraph_text(old_block) {
                        let norm = normalize(&text);
                        if norm.len() >= 30 {
                            deletions.push((idx, norm));
                        }
                    }
                }
                DiffChange::BlockInserted {
                    block,
                    move_id: None,
                    ..
                } => {
                    if let Some(text) = paragraph_text(block) {
                        let norm = normalize(&text);
                        if norm.len() >= 30 {
                            insertions.push((idx, norm));
                        }
                    }
                }
                _ => {}
            }
        }

        // Check all deleted+inserted pairs that are far apart.
        for (del_idx, del_text) in &deletions {
            for (ins_idx, ins_text) in &insertions {
                let distance = if *ins_idx > *del_idx {
                    ins_idx - del_idx
                } else {
                    del_idx - ins_idx
                };

                if distance <= 30 {
                    continue;
                }

                // Check exact match first (fast path).
                if del_text == ins_text {
                    orphan_moves.push(OrphanMove {
                        sample: name.clone(),
                        distance,
                        deleted_preview: preview(del_text, 80),
                        inserted_preview: preview(ins_text, 80),
                        overlap: 1.0,
                    });
                    continue;
                }

                // Check word overlap.
                let overlap = word_overlap(del_text, ins_text);
                if overlap > 0.90 {
                    orphan_moves.push(OrphanMove {
                        sample: name.clone(),
                        distance,
                        deleted_preview: preview(del_text, 80),
                        inserted_preview: preview(ins_text, 80),
                        overlap,
                    });
                }
            }
        }
    }

    // --- Report ---

    println!("\n{}", "=".repeat(60));
    println!("ORPHAN MOVE SCOUT REPORT");
    println!("{}\n", "=".repeat(60));

    if !skipped.is_empty() {
        println!("SKIPPED ({}):", skipped.len());
        for (name, reason) in &skipped {
            println!("  {name}: {reason}");
        }
        println!();
    }

    if orphan_moves.is_empty() {
        println!("No orphan moves found across {total_samples} samples.");
    } else {
        // Sort by overlap descending (most suspicious first).
        orphan_moves.sort_by(|a, b| {
            b.overlap
                .partial_cmp(&a.overlap)
                .unwrap()
                .then(b.distance.cmp(&a.distance))
        });

        println!(
            "ORPHAN MOVES FOUND: {} (across {total_samples} samples)\n",
            orphan_moves.len()
        );
        for (i, m) in orphan_moves.iter().enumerate() {
            println!(
                "  #{}: [{}] distance={} overlap={:.2}",
                i + 1,
                m.sample,
                m.distance,
                m.overlap
            );
            println!("    DEL: {}", m.deleted_preview);
            println!("    INS: {}", m.inserted_preview);
            println!();
        }
    }

    println!(
        "SUMMARY: {total_samples} samples processed, {} skipped, {} orphan moves found",
        skipped.len(),
        orphan_moves.len()
    );
}
