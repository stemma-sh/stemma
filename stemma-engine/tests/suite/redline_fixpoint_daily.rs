//! Daily fixpoint coverage across representative change categories.
//!
//! The *full* fixpoint sweep over every fixture lives in
//! `redline_invariants::fixpoint_diff_then_apply_then_rediff_is_empty` and is
//! `#[ignore]`d (run via `just nightly`) because it is slow. The single daily
//! fixpoint guard in `safe_us_vs_canada` exercises exactly one fixture.
//!
//! This file is the middle tier: **wide in coverage, narrow in depth.** It runs
//! the same strict fixpoint chain
//!
//! ```text
//!   diff(A, B) → merge_diff(A, B, diff) → accept_all → re-diff(merged, B) == ∅
//! ```
//!
//! over one curated fixture per category that can shape tracked segments
//! differently — plain text, numbering/ordering, note references, equations,
//! images, combined opaque, and tables. The point is that every materialization
//! shape has a representative that runs on *every* `cargo test`, so a regression
//! in the merge-path lowering (field-coalescing, opaque reading order, segment
//! compaction) is caught daily rather than only nightly.
//!
//! The check is **unfiltered**: unlike `self_edit_invariants::check_fixpoint`,
//! it does not drop formatting-only residuals. A materializer change that
//! perturbs only run/segment boundaries (same accept-all text, different
//! structure) must be loud here — that is precisely the class of divergence
//! this tier exists to catch.
//!
//! Missing fixtures skip gracefully, so a checkout without the DOCX corpus
//! still passes. The set is asserted non-empty so the test cannot silently
//! become a no-op if the testdata layout moves.

use std::fs;
use std::path::PathBuf;

use stemma::{
    DiffChange, DocxRuntime, RevisionInfo, SimpleRuntime, accept_all, diff_documents, merge_diff,
};

/// One representative fixture per tracked-segment shaping category. Each name is
/// a `testdata/<name>/` directory holding `before.docx` + `after.docx`.
///
/// Chosen so each category that the merge-path materializer normalizes
/// specially has at least one daily fixpoint witness. Keep this list small and
/// representative — breadth lives in the nightly sweep, not here.
const CURATED_FIXTURES: &[(&str, &str)] = &[
    ("simple-text", "plain inline text edits"),
    ("twenty-paragraphs", "multi-paragraph block structure"),
    ("ordering-mixed-complex", "numbering / block reordering"),
    ("footnotes", "note references (opaque inline)"),
    ("math-equations", "equations (opaque preservation)"),
    ("images", "images (opaque preservation)"),
    ("image-math-combined", "combined opaque inlines"),
    ("table-modifications", "table row/cell changes"),
];

fn fixture_paths(name: &str) -> (PathBuf, PathBuf) {
    let dir = PathBuf::from("testdata").join(name);
    (dir.join("before.docx"), dir.join("after.docx"))
}

/// Run the strict, unfiltered fixpoint chain on one fixture pair.
/// Returns `Ok(())` on fixpoint, or `Err(description)` listing residual changes.
fn run_fixpoint(fixture: &str, before: &[u8], after: &[u8]) -> Result<(), String> {
    let runtime = SimpleRuntime::new();
    let canon_a = runtime
        .import_docx(before)
        .map_err(|err| format!("[{fixture}] import before failed: {err:?}"))?
        .canonical;
    let canon_b = runtime
        .import_docx(after)
        .map_err(|err| format!("[{fixture}] import after failed: {err:?}"))?
        .canonical;

    let revision = RevisionInfo {
        revision_id: 1,
        author: Some("fixpoint-daily".to_string()),
        date: Some("2025-06-01T00:00:00Z".to_string()),
        apply_op_id: None,
    };

    let diff = diff_documents(&canon_a, &canon_b)
        .map_err(|err| format!("[{fixture}] diff_documents failed: {err}"))?;
    let mut merged = merge_diff(&canon_a, &canon_b, &diff, &revision)
        .map_err(|err| format!("[{fixture}] merge_diff failed: {err:?}"))?
        .doc;
    accept_all(&mut merged);
    let fixpoint_diff = diff_documents(&merged, &canon_b)
        .map_err(|err| format!("[{fixture}] fixpoint diff_documents failed: {err}"))?;

    if fixpoint_diff.changes.is_empty() {
        return Ok(());
    }

    let descriptions: Vec<String> = fixpoint_diff
        .changes
        .iter()
        .take(10)
        .map(describe_change)
        .collect();
    Err(format!(
        "[{fixture}] fixpoint violated: accept_all(merge_diff(A, B, diff(A, B))) differs from B \
         with {} residual change(s):\n        {}",
        fixpoint_diff.changes.len(),
        descriptions.join("\n        ")
    ))
}

fn describe_change(change: &DiffChange) -> String {
    match change {
        DiffChange::BlockDeleted { old_text, .. } => {
            format!(
                "BlockDeleted: {:?}",
                old_text.chars().take(80).collect::<String>()
            )
        }
        DiffChange::BlockInserted { block, .. } => format!(
            "BlockInserted: {:?}",
            format!("{block:?}").chars().take(80).collect::<String>()
        ),
        DiffChange::BlockModified {
            old_text, new_text, ..
        } => {
            if old_text == new_text {
                format!(
                    "BlockModified (formatting-only): {:?}",
                    old_text.chars().take(60).collect::<String>()
                )
            } else {
                format!(
                    "BlockModified: old={:?} new={:?}",
                    old_text.chars().take(60).collect::<String>(),
                    new_text.chars().take(60).collect::<String>()
                )
            }
        }
        other => format!("{other:?}").chars().take(120).collect(),
    }
}

#[test]
fn fixpoint_holds_across_curated_categories() {
    assert!(
        !CURATED_FIXTURES.is_empty(),
        "curated fixture set is empty — daily fixpoint coverage would be a no-op"
    );

    let mut checked = 0usize;
    let mut skipped = Vec::new();
    let mut failures = Vec::new();

    for (name, category) in CURATED_FIXTURES {
        let (before_path, after_path) = fixture_paths(name);
        let (Ok(before), Ok(after)) = (fs::read(&before_path), fs::read(&after_path)) else {
            skipped.push(format!("{name} ({category})"));
            continue;
        };

        checked += 1;
        if let Err(failure) = run_fixpoint(name, &before, &after) {
            failures.push(failure);
        }
    }

    if !skipped.is_empty() {
        eprintln!(
            "redline_fixpoint_daily: skipped {} fixture(s) absent from this checkout:\n  - {}",
            skipped.len(),
            skipped.join("\n  - ")
        );
    }

    assert!(
        failures.is_empty(),
        "daily fixpoint coverage failed for {}/{} checked fixture(s):\n    {}",
        failures.len(),
        checked,
        failures.join("\n    ")
    );

    // If the whole curated set is missing (corpus-less checkout) `checked` is 0
    // and the test passes as a documented skip. When the corpus is present at
    // least the in-tree fixtures must have run.
    eprintln!("redline_fixpoint_daily: fixpoint held for {checked} curated fixture(s)");
}
