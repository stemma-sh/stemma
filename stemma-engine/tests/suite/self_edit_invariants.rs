//! Self-edit metamorphic testing: for each parseable single doc, create
//! programmatic modifications (delete paragraph, replace word, insert paragraph),
//! then run canonical-space invariants on each (original, modified) pair.
//!
//! This exercises the diff/merge/accept/reject pipeline on documents that
//! currently only get parse/roundtrip/identity-redline testing.

use std::collections::BTreeSet;
use std::fs;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use rayon::prelude::*;
use stemma::{
    BlockNode, CanonDoc, DiffChange, DocxRuntime, InlineChange, InlineNode, MarkValue, NodeId,
    RevisionInfo, SimpleRuntime, TrackedBlock, TrackingStatus, accept_all, diff_documents,
    merge_diff, reject_all_with_styles,
};

use crate::common;

// ── helpers ──────────────────────────────────────────────────────────────

fn test_revision() -> RevisionInfo {
    RevisionInfo {
        revision_id: 1,
        identity: 0,
        author: Some("self-edit-test".to_string()),
        date: Some("2025-06-01T00:00:00Z".to_string()),
        apply_op_id: None,
    }
}

/// Replicates `extract_inline_text` from redline_invariants.rs.
fn extract_inline_text(inlines: &[InlineNode]) -> String {
    let mut out = String::new();
    for inline in inlines {
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

fn truncate_for_display(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &s[..end])
    }
}

// ── doc discovery ────────────────────────────────────────────────────────

/// Collect unique DOCX paths from fixture pairs.
fn discover_fixture_docs() -> Vec<String> {
    let mut paths = BTreeSet::new();

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

#[derive(Debug, Clone, serde::Deserialize)]
struct FixtureExpectation {
    path: String,
    expected_outcome: String,
    expected_reason: String,
}

#[derive(Debug, serde::Deserialize)]
struct StressManifest {
    #[allow(dead_code)]
    version: u32,
    fixtures: Vec<FixtureExpectation>,
}

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
            // Resolve manifest path (stress/...) to absolute path
            f.path
                .strip_prefix("stress/")
                .map(|rel| stress_dir.join(rel).to_string_lossy().to_string())
                .unwrap_or(f.path)
        })
        .collect();
    paths.sort();
    paths
}

// ── self-edit modifications ──────────────────────────────────────────────

/// Describes a modification applied to a CanonDoc.
#[derive(Debug)]
enum EditKind {
    DeleteParagraph,
    ReplaceWord,
    InsertParagraph,
}

/// Try to produce a modified CanonDoc for each applicable edit kind.
/// Returns a vec of (edit_kind, modified_doc) pairs.
fn generate_self_edits(canon: &CanonDoc) -> Vec<(EditKind, CanonDoc)> {
    let mut edits = Vec::new();

    // Delete paragraph: remove first non-trivial paragraph block if doc has 2+ blocks.
    //
    // Skip when the chosen paragraph is the document-final block AND nothing it
    // could merge into precedes it (i.e. the block before is not a paragraph, or
    // it is the only block). A document cannot lose its terminal paragraph mark:
    // deleting the final paragraph after a table leaves an empty terminal
    // paragraph (Word's behavior, and the engine's tracked delete), so the naive
    // `blocks.remove(idx)` target — which ends the doc in a table — does not
    // model a valid resolved document, and the accept-all-equals-target
    // invariant is ill-defined there. (A final paragraph PRECEDED by a paragraph
    // merges into it and still matches naive removal, so that case is kept.)
    if canon.blocks.len() >= 2
        && let Some(idx) = find_nontrivial_paragraph_index(&canon.blocks)
    {
        let deletes_terminal_mark = idx == canon.blocks.len() - 1
            && (idx == 0 || !matches!(canon.blocks[idx - 1].block, BlockNode::Paragraph(_)));
        if !deletes_terminal_mark {
            let mut modified = canon.clone();
            modified.blocks.remove(idx);
            edits.push((EditKind::DeleteParagraph, modified));
        }
    }

    // Replace word: find first paragraph with 2+ word text, replace first word.
    if let Some((block_idx, seg_idx, inline_idx)) = find_replaceable_text(canon) {
        let mut modified = canon.clone();
        if let BlockNode::Paragraph(ref mut para) = modified.blocks[block_idx].block
            && let InlineNode::Text(ref mut text_node) = para.segments[seg_idx].inlines[inline_idx]
        {
            let words: Vec<&str> = text_node.text.split_whitespace().collect();
            if words.len() >= 2 {
                let mut new_words = vec!["REPLACED"];
                new_words.extend_from_slice(&words[1..]);
                text_node.text = new_words.join(" ");
                // Update rendered_text and clear hash since content changed.
                para.rendered_text = None;
                para.block_text_hash = None;
            }
        }
        edits.push((EditKind::ReplaceWord, modified));
    }

    // Insert paragraph: clone a paragraph and append with modified NodeId.
    if let Some(idx) = find_nontrivial_paragraph_index(&canon.blocks) {
        let mut modified = canon.clone();
        if let BlockNode::Paragraph(ref source_para) = canon.blocks[idx].block {
            let mut new_para = source_para.clone();
            new_para.id = NodeId::from(format!("{}__selfins", source_para.id.0));
            // Clear hash since this is a new "different" paragraph.
            new_para.block_text_hash = None;
            modified.blocks.push(TrackedBlock {
                status: TrackingStatus::Normal,
                block: BlockNode::Paragraph(new_para),
                move_id: None,
                block_sdt_wrap: None,
            });
        }
        edits.push((EditKind::InsertParagraph, modified));
    }

    edits
}

/// Find index of first non-trivial paragraph (has non-empty text).
/// Falls back to last block if all are trivial.
fn find_nontrivial_paragraph_index(blocks: &[TrackedBlock]) -> Option<usize> {
    for (i, tracked) in blocks.iter().enumerate() {
        if let BlockNode::Paragraph(p) = &tracked.block {
            let text = p
                .rendered_text
                .clone()
                .unwrap_or_else(|| extract_inline_text(&p.all_inlines_owned()));
            if !text.trim().is_empty() {
                return Some(i);
            }
        }
    }
    // Fall back to last paragraph if all are empty.
    blocks
        .iter()
        .rposition(|tb| matches!(&tb.block, BlockNode::Paragraph(_)))
}

/// Find first TextNode with 2+ whitespace-separated words.
/// Returns (block_idx, segment_idx, inline_idx).
fn find_replaceable_text(canon: &CanonDoc) -> Option<(usize, usize, usize)> {
    for (bi, tracked) in canon.blocks.iter().enumerate() {
        if let BlockNode::Paragraph(para) = &tracked.block {
            for (si, seg) in para.segments.iter().enumerate() {
                for (ii, inline) in seg.inlines.iter().enumerate() {
                    if let InlineNode::Text(t) = inline {
                        let word_count = t.text.split_whitespace().count();
                        if word_count >= 2 {
                            return Some((bi, si, ii));
                        }
                    }
                }
            }
        }
    }
    None
}

// ── invariant checks ─────────────────────────────────────────────────────

/// Check diff reconstruction: for each BlockModified, inline_changes must
/// reconstruct both old_text and new_text.
fn check_diff_reconstruction(
    doc_path: &str,
    edit_kind: &EditKind,
    diff: &stemma::DocumentDiff,
) -> Vec<String> {
    let mut failures = Vec::new();

    for (i, change) in diff.changes.iter().enumerate() {
        if let DiffChange::BlockModified {
            old_text,
            new_text,
            inline_changes,
            ..
        } = change
        {
            let reconstructed_old: String = inline_changes
                .iter()
                .filter_map(|c| match c {
                    InlineChange::Unchanged { text, .. } | InlineChange::Deleted { text, .. } => {
                        Some(text.as_str())
                    }
                    InlineChange::Inserted { .. } => None,
                    InlineChange::Opaque {
                        segment_type:
                            stemma::InlineChangeSegmentType::Equal
                            | stemma::InlineChangeSegmentType::Delete,
                        text,
                        ..
                    } => text.as_deref(),
                    InlineChange::Opaque { .. } => None,
                })
                .collect();

            let reconstructed_new: String = inline_changes
                .iter()
                .filter_map(|c| match c {
                    InlineChange::Unchanged { text, .. } | InlineChange::Inserted { text, .. } => {
                        Some(text.as_str())
                    }
                    InlineChange::Deleted { .. } => None,
                    InlineChange::Opaque {
                        segment_type:
                            stemma::InlineChangeSegmentType::Equal
                            | stemma::InlineChangeSegmentType::Insert,
                        text,
                        ..
                    } => text.as_deref(),
                    InlineChange::Opaque { .. } => None,
                })
                .collect();

            if &reconstructed_old != old_text {
                failures.push(format!(
                    "[{doc_path}] [{edit_kind:?}] change #{i}: reconstructed old != old_text. \
                     old={:?}, reconstructed={:?}",
                    truncate_for_display(old_text, 120),
                    truncate_for_display(&reconstructed_old, 120),
                ));
            }
            if &reconstructed_new != new_text {
                failures.push(format!(
                    "[{doc_path}] [{edit_kind:?}] change #{i}: reconstructed new != new_text. \
                     new={:?}, reconstructed={:?}",
                    truncate_for_display(new_text, 120),
                    truncate_for_display(&reconstructed_new, 120),
                ));
            }
        }
    }

    failures
}

/// Returns true when a DiffChange is formatting-only (identical text, no
/// insertions or deletions).  The merge/accept cycle can introduce subtle
/// formatting mismatches on Normal blocks (run-boundary reconstruction,
/// numbering materialization) that don't affect text content.  These are
/// excluded from the fixpoint / accept-reject residual checks.
fn is_formatting_only_change(change: &DiffChange) -> bool {
    match change {
        DiffChange::BlockModified {
            old_text,
            new_text,
            inline_changes,
            ..
        } => {
            old_text == new_text
                && inline_changes
                    .iter()
                    .all(|c| matches!(c, InlineChange::Unchanged { .. }))
        }
        _ => false,
    }
}

/// Check fixpoint: diff → merge → accept_all → re-diff must be empty.
fn check_fixpoint(
    doc_path: &str,
    edit_kind: &EditKind,
    canon_b: &CanonDoc,
    merged: &CanonDoc,
) -> Vec<String> {
    let mut failures = Vec::new();

    let mut accepted = merged.clone();
    accept_all(&mut accepted);

    let fixpoint_diff = diff_documents(&accepted, canon_b).expect("diff should succeed");
    // Filter out formatting-only residuals — the merge/accept cycle can introduce
    // run-boundary mismatches that don't affect text content.
    let content_changes: Vec<_> = fixpoint_diff
        .changes
        .iter()
        .filter(|c| !is_formatting_only_change(c))
        .collect();
    if !content_changes.is_empty() {
        let descriptions: Vec<String> = content_changes
            .iter()
            .take(5)
            .map(|c| match c {
                DiffChange::BlockDeleted { old_text, .. } => {
                    format!("BlockDeleted: {:?}", truncate_for_display(old_text, 80))
                }
                DiffChange::BlockInserted { block, .. } => {
                    format!(
                        "BlockInserted: {:?}",
                        format!("{block:?}").chars().take(80).collect::<String>()
                    )
                }
                DiffChange::BlockModified {
                    old_text, new_text, ..
                } => {
                    format!(
                        "BlockModified: old={:?} new={:?}",
                        truncate_for_display(old_text, 60),
                        truncate_for_display(new_text, 60)
                    )
                }
                other => format!("{other:?}").chars().take(120).collect(),
            })
            .collect();

        failures.push(format!(
            "[{doc_path}] [{edit_kind:?}] fixpoint violated: {} residual change(s):\n    {}",
            content_changes.len(),
            descriptions.join("\n    ")
        ));
    }

    failures
}

/// Check canonical accept/reject via diff_documents:
/// - accept_all(merge_diff(A, B)) must diff-equal B
/// - reject_all(merge_diff(A, B)) must diff-equal A
///
/// We use `diff_documents` rather than text comparison because `rendered_text`
/// (which includes synthesized numbering prefixes) is an import-time field
/// that merge_diff/accept_all/reject_all don't recalculate. The diff engine
/// resolves numbering internally, so diff-equality is the correct check.
fn check_accept_reject(
    doc_path: &str,
    edit_kind: &EditKind,
    canon_a: &CanonDoc,
    canon_b: &CanonDoc,
    merged: &CanonDoc,
) -> Vec<String> {
    let mut failures = Vec::new();

    // Accept all → must diff-equal canon_b.
    let mut accepted = merged.clone();
    accept_all(&mut accepted);
    let accept_diff = diff_documents(&accepted, canon_b).expect("diff should succeed");
    let accept_content: Vec<_> = accept_diff
        .changes
        .iter()
        .filter(|c| !is_formatting_only_change(c))
        .collect();
    if !accept_content.is_empty() {
        let descriptions: Vec<String> = accept_content
            .iter()
            .take(5)
            .map(|c| format_diff_change(c))
            .collect();
        failures.push(format!(
            "[{doc_path}] [{edit_kind:?}] accept_all differs from target: {} change(s):\n    {}",
            accept_content.len(),
            descriptions.join("\n    ")
        ));
    }

    // Reject all → must diff-equal canon_a.
    let mut rejected = merged.clone();
    reject_all_with_styles(&mut rejected, None);
    let reject_diff = diff_documents(&rejected, canon_a).expect("diff should succeed");
    let reject_content: Vec<_> = reject_diff
        .changes
        .iter()
        .filter(|c| !is_formatting_only_change(c))
        .collect();
    if !reject_content.is_empty() {
        let descriptions: Vec<String> = reject_content
            .iter()
            .take(5)
            .map(|c| format_diff_change(c))
            .collect();
        failures.push(format!(
            "[{doc_path}] [{edit_kind:?}] reject_all differs from base: {} change(s):\n    {}",
            reject_content.len(),
            descriptions.join("\n    ")
        ));
    }

    failures
}

fn format_diff_change(c: &DiffChange) -> String {
    match c {
        DiffChange::BlockDeleted { old_text, .. } => {
            format!("BlockDeleted: {:?}", truncate_for_display(old_text, 80))
        }
        DiffChange::BlockInserted { block, .. } => {
            format!(
                "BlockInserted: {:?}",
                format!("{block:?}").chars().take(80).collect::<String>()
            )
        }
        DiffChange::BlockModified {
            old_text, new_text, ..
        } => {
            format!(
                "BlockModified: old={:?} new={:?}",
                truncate_for_display(old_text, 60),
                truncate_for_display(new_text, 60)
            )
        }
        other => format!("{other:?}").chars().take(120).collect(),
    }
}

// ── test runner ──────────────────────────────────────────────────────────

fn run_self_edit_invariants_on_docs(docs: &[String]) -> (usize, Vec<String>) {
    let mut total_checked = 0;
    let mut failures = Vec::new();

    for doc_path in docs {
        let bytes = match fs::read(doc_path) {
            Ok(b) => b,
            Err(err) => {
                eprintln!("[{doc_path}] skipping (read failed): {err}");
                continue;
            }
        };

        let runtime = SimpleRuntime::new();
        let import = match runtime.import_docx(&bytes) {
            Ok(r) => r,
            Err(err) => {
                eprintln!("[{doc_path}] skipping (import failed): {err:?}");
                continue;
            }
        };

        let canon_a = &import.canonical;
        let edits = generate_self_edits(canon_a);

        if edits.is_empty() {
            eprintln!("[{doc_path}] skipping (no applicable edits)");
            continue;
        }

        let revision = test_revision();

        for (edit_kind, canon_b) in &edits {
            let diff = diff_documents(canon_a, canon_b).expect("diff should succeed");

            failures.extend(check_diff_reconstruction(doc_path, edit_kind, &diff));

            let merged = match merge_diff(canon_a, canon_b, &diff, &revision) {
                Ok(m) => m.doc,
                Err(err) => {
                    failures.push(format!(
                        "[{doc_path}] [{edit_kind:?}] merge_diff failed: {err:?}"
                    ));
                    continue;
                }
            };

            failures.extend(check_fixpoint(doc_path, edit_kind, canon_b, &merged));
            failures.extend(check_accept_reject(
                doc_path, edit_kind, canon_a, canon_b, &merged,
            ));
        }

        total_checked += 1;
    }

    (total_checked, failures)
}

// ── fixture test ─────────────────────────────────────────────────────────

#[test]
fn self_edit_invariants() {
    let docs = discover_fixture_docs();
    assert!(!docs.is_empty(), "no fixture docs found");

    let (total_checked, failures) = run_self_edit_invariants_on_docs(&docs);

    assert!(
        total_checked > 0,
        "expected at least one fixture doc to be checked"
    );
    eprintln!(
        "self-edit invariants: checked {} fixture docs, {} failures",
        total_checked,
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "self-edit invariants violated ({}):\n{}",
        failures.len(),
        failures
            .iter()
            .take(40)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

// ── stress test ──────────────────────────────────────────────────────────

#[test]
#[ignore = "expensive stress self-edit invariant suite"]
fn stress_self_edit_invariants() {
    let result = thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(stress_self_edit_invariants_inner)
        .expect("failed to spawn thread")
        .join();

    match result {
        Ok(()) => {}
        Err(panic_payload) => std::panic::resume_unwind(panic_payload),
    }
}

fn stress_self_edit_invariants_inner() {
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
            let import = match runtime.import_docx(&bytes) {
                Ok(r) => r,
                Err(err) => {
                    eprintln!("[{doc_path}] skipping (import failed): {err:?}");
                    return;
                }
            };

            // Use the view() path which normalizes pre-existing tracked changes.
            // The fixpoint invariant assumes a clean CanonDoc — documents with
            // unresolved tracked changes would cause accept_all to resolve both
            // pre-existing and self-edit changes, breaking the symmetry with canon_b.
            let view = match runtime.view(&import.doc_handle) {
                Ok(v) => v,
                Err(err) => {
                    eprintln!("[{doc_path}] skipping (view failed): {err:?}");
                    return;
                }
            };

            let canon_a = &view.canonical;
            let edits = generate_self_edits(canon_a);

            if edits.is_empty() {
                eprintln!("[{doc_path}] skipping (no applicable edits)");
                return;
            }

            let revision = test_revision();
            let mut local_failures = Vec::new();

            for (edit_kind, canon_b) in &edits {
                let diff = diff_documents(canon_a, canon_b).expect("diff should succeed");

                local_failures.extend(check_diff_reconstruction(doc_path, edit_kind, &diff));

                let merged = match merge_diff(canon_a, canon_b, &diff, &revision) {
                    Ok(m) => m.doc,
                    Err(err) => {
                        local_failures.push(format!(
                            "[{doc_path}] [{edit_kind:?}] merge_diff failed: {err:?}"
                        ));
                        continue;
                    }
                };

                local_failures.extend(check_fixpoint(doc_path, edit_kind, canon_b, &merged));
                local_failures.extend(check_accept_reject(
                    doc_path, edit_kind, canon_a, canon_b, &merged,
                ));
            }

            if !local_failures.is_empty() {
                failures.lock().unwrap().extend(local_failures);
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
        "stress self-edit invariants: checked {} stress docs, {} failures",
        total_checked,
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "stress self-edit invariants violated ({}):\n{}",
        failures.len(),
        failures
            .iter()
            .take(40)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
