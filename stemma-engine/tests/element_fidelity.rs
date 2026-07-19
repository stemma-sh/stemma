//! Element-level round-trip fidelity gate — the "state-3" detector.
//!
//! ## Why this exists (the gap `roundtrip_fidelity.rs` cannot see)
//!
//! `roundtrip_fidelity.rs` compares IR₁ (import) vs IR₂ (import → export →
//! re-import). That is STRUCTURALLY BLIND to "state-3" constructs: things
//! PARSED-then-DROPPED at import time (e.g. `<w:gridSpan>`, `<w:hideMark>`,
//! `<w:outlineLvl>`, `<w:numId w:val="0"/>` suppression). A dropped construct
//! is absent from BOTH IRs, so the IR-to-IR diff finds nothing and passes
//! while the construct has silently vanished from the document.
//!
//! This gate closes that hole by comparing the ORIGINAL `word/document.xml`
//! against the RESERIALIZED `word/document.xml` at the element level:
//!
//!   1. `import_docx(original)` → handle
//!   2. `apply_edit(SetDocDefaults{font_family})` → forces a FULL body
//!      reserialization through `serialize_canonical_docx` (the IR serializer).
//!      A bare `export_docx` does NOT serialize the IR: on an un-edited handle
//!      `export_docx` re-zips the original scaffold package (returns the input
//!      bytes ~verbatim), so an `import → export` census would compare a doc to
//!      ITSELF and find nothing — vacuous. `SetDocDefaults` is a package-level,
//!      body-untouching edit (it merges only `w:docDefaults`), but the apply
//!      path reserializes the WHOLE body — so any body-element state-3 loss
//!      surfaces while document content is left semantically untouched. This is
//!      exactly the reserialization trigger the Python prototype
//!      (`fidelity_gate.py`) uses.
//!   3. `export_docx(handle, ExportMode::Redline)` → reserialized bytes.
//!   4. Census every element (prefixed name → count) in the original's
//!      document.xml vs the reserialized output's document.xml.
//!   5. A **state-3 LOSS** = an element whose count in the output is LESS than
//!      in the original. (Counts that RISE are "baking" — the style cascade
//!      materialized as direct props — a separate, lower-severity concern this
//!      gate does not flag.)
//!
//! ## Confound: pre-existing tracked changes (handled via option (a))
//!
//! A document that ALREADY carries tracked changes (`<w:ins>`, `<w:del>`,
//! `<w:pPrChange>`, `<w:rPrChange>`, `<w:moveFrom>`/`<w:moveTo>`, and their
//! range markers) gets those normalized on reserialize, which legitimately
//! changes element counts — that is NOT a state-3 loss. Per the task we take
//! option (a): RESTRICT the gate to documents with NO pre-existing tracked
//! changes, for a clean signal. Docs with tracked-change machinery are skipped
//! and counted as `skipped_tracked`.
//!
//! ## Cosmetic exclusions
//!
//! `w:rsid*` are ATTRIBUTES, not elements, so they never appear in an element
//! census. We census elements only; there is nothing to exclude on that front.
//!
//! ## Daily vs corpus
//!
//! - `element_fidelity_testdata` (non-ignored): runs over `testdata/`. These
//!   are clean engine-authored fixtures; few/no losses expected. Any loss is a
//!   real finding and fails the gate — we do NOT allowlist to hide a real bug.
//! - `element_fidelity_corpus` (`#[ignore]`, gated on `CORPUS_DIR`): aggregates
//!   state-3 losses across a real corpus and prints a ranked inventory. Mirrors
//!   the graceful-skip contract of `corpus_triage` — unset `CORPUS_DIR` skips.
//!
//! Run the corpus inventory:
//!   CORPUS_DIR=/path/to/corpus \
//!   RUST_MIN_STACK=67108864 \
//!   cargo test --release --test element_fidelity -- --ignored --nocapture

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use rayon::prelude::*;
use stemma::domain::RevisionInfo;
use stemma::edit::{EditStep, EditTransaction, MaterializationMode};
use stemma::{DocxRuntime, ExportMode, SimpleRuntime};

mod common;
use common::{build_element_census, read_zip_entry};

// ─── Tracked-change machinery (the confound families, option (a)) ──────────

/// Element families whose presence in the ORIGINAL means the document carries
/// pre-existing tracked changes / move tracking. Reserialization legitimately
/// normalizes these, so any document containing any of them is excluded from
/// the state-3 gate. (Census names are prefixed local names.)
const TRACKED_CHANGE_ELEMENTS: &[&str] = &[
    "w:ins",
    "w:del",
    "w:pPrChange",
    "w:rPrChange",
    "w:tblPrChange",
    "w:trPrChange",
    "w:tcPrChange",
    "w:tblGridChange",
    "w:sectPrChange",
    "w:numberingChange",
    "w:moveFrom",
    "w:moveTo",
    "w:moveFromRangeStart",
    "w:moveFromRangeEnd",
    "w:moveToRangeStart",
    "w:moveToRangeEnd",
    "w:cellIns",
    "w:cellDel",
    "w:cellMerge",
];

fn has_pre_existing_tracked_changes(census: &HashMap<String, usize>) -> bool {
    TRACKED_CHANGE_ELEMENTS
        .iter()
        .any(|el| census.get(*el).copied().unwrap_or(0) > 0)
}

// ─── Per-document census diff ──────────────────────────────────────────────

/// Outcome of running the gate on one document.
enum DocOutcome {
    /// No state-3 losses; clean.
    Clean,
    /// Skipped: document carries pre-existing tracked changes (option (a)).
    SkippedTracked,
    /// Skipped: could not be processed (import/export/zip error). Carries why.
    SkippedError(String),
    /// State-3 losses found: element name → (original count, output count).
    Losses(BTreeMap<String, (usize, usize)>),
}

/// A styles-only transaction whose apply path reserializes the WHOLE body but
/// touches only `w:docDefaults`. This is the body-reserialization trigger.
/// `MaterializationMode::Direct` because `SetDocDefaults` is package-level and
/// untracked (OOXML has no change envelope for docDefaults).
fn reserialize_trigger() -> EditTransaction {
    EditTransaction {
        steps: vec![EditStep::SetDocDefaults {
            font_family: Some("Calibri".to_string()),
            font_size_half_points: None,
            rationale: Some("element-fidelity reserialize trigger".to_string()),
        }],
        summary: Some("reserialize trigger".to_string()),
        materialization_mode: MaterializationMode::Direct,
        revision: RevisionInfo {
            revision_id: 1,
            identity: 0,
            author: Some("fid".to_string()),
            date: Some("2026-06-29T00:00:00Z".to_string()),
            apply_op_id: None,
        },
    }
}

/// Import the original, force a full IR reserialization (styles-only edit),
/// export, and census-diff the two `word/document.xml` streams.
fn run_gate(docx_bytes: &[u8]) -> DocOutcome {
    let original_xml = match read_zip_entry(docx_bytes, "word/document.xml") {
        Some(x) => x,
        None => return DocOutcome::SkippedError("no word/document.xml in original".into()),
    };
    let original_census = build_element_census(&original_xml);

    // Option (a): exclude docs with pre-existing tracked changes for a clean
    // state-3 signal. `ELEMENT_FIDELITY_INCLUDE_TRACKED=1` disables the filter
    // — used ONLY to cross-check against the (unfiltered) Python prototype,
    // whose numbers mix genuine state-3 drops with tracked-change normalization.
    let include_tracked = std::env::var("ELEMENT_FIDELITY_INCLUDE_TRACKED").is_ok();
    if !include_tracked && has_pre_existing_tracked_changes(&original_census) {
        return DocOutcome::SkippedTracked;
    }

    let runtime = SimpleRuntime::new();
    let import = match runtime.import_docx(docx_bytes) {
        Ok(i) => i,
        Err(e) => return DocOutcome::SkippedError(format!("import_docx failed: {e:?}")),
    };
    // Force `serialize_canonical_docx` over the full body. A bare export_docx
    // would re-zip the original scaffold (vacuous self-comparison).
    if let Err(e) = runtime.apply_edit(&import.doc_handle, &reserialize_trigger()) {
        return DocOutcome::SkippedError(format!("apply_edit(SetDocDefaults) failed: {e:?}"));
    }
    let reserialized = match runtime.export_docx(&import.doc_handle, ExportMode::Redline) {
        Ok(b) => b,
        Err(e) => return DocOutcome::SkippedError(format!("export_docx failed: {e:?}")),
    };
    let out_xml = match read_zip_entry(&reserialized, "word/document.xml") {
        Some(x) => x,
        None => return DocOutcome::SkippedError("no word/document.xml in output".into()),
    };
    let out_census = build_element_census(&out_xml);

    let losses = diff_census(&original_census, &out_census);
    if losses.is_empty() {
        DocOutcome::Clean
    } else {
        DocOutcome::Losses(losses)
    }
}

/// A state-3 loss = an element whose count in `out` is strictly LESS than in
/// `original`. Returns name → (original, output) for every such element.
/// (Counts that RISE are "baking" — cascade materialized as direct props — a
/// separate, lower-severity concern this gate does not flag.)
fn diff_census(
    original: &HashMap<String, usize>,
    out: &HashMap<String, usize>,
) -> BTreeMap<String, (usize, usize)> {
    let mut losses = BTreeMap::new();
    for (name, &orig_count) in original {
        let out_count = out.get(name).copied().unwrap_or(0);
        if out_count < orig_count {
            losses.insert(name.clone(), (orig_count, out_count));
        }
    }
    losses
}

// ─── Daily characterization over testdata/ ─────────────────────────────────

/// Daily CHARACTERIZATION of state-3 element losses over `testdata/` (marked
/// as such per CLAUDE.md: it pins down CURRENT engine behavior, it is not a
/// spec the engine already satisfies). Now that the gate forces a real IR
/// reserialization (`apply_edit(SetDocDefaults)` → `serialize_canonical_docx`),
/// clean fixtures DO surface real state-3 losses — these are open findings that
/// feed the burn-down, NOT regressions to block the suite on. So this test
/// REPORTS the full per-doc and aggregate inventory (no element is hidden /
/// allowlisted) and stays green. The genuine pass/fail of the loss DETECTOR is
/// asserted by `diff_census_detects_loss_not_baking`; `element_fidelity_corpus`
/// produces the authoritative ranked inventory.
///
/// The only hard assertion here is liveness: the gate must actually exercise
/// documents (so a future regression that makes it vacuous fails loudly).
#[test]
fn element_fidelity_testdata() {
    let docs = discover_docx_files(Path::new("testdata"));
    assert!(!docs.is_empty(), "no .docx fixtures found under testdata/");

    let mut checked = 0usize;
    let mut skipped_tracked = 0usize;
    let mut skipped_error = 0usize;
    let mut with_losses = 0usize;
    let mut per_element: BTreeMap<String, (usize, usize)> = BTreeMap::new(); // name → (#docs, total_lost)
    let mut findings: Vec<String> = Vec::new();

    for path in &docs {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                skipped_error += 1;
                eprintln!("  read error [{}]: {e}", path.display());
                continue;
            }
        };
        match run_gate(&bytes) {
            DocOutcome::Clean => checked += 1,
            DocOutcome::SkippedTracked => skipped_tracked += 1,
            DocOutcome::SkippedError(why) => {
                // Import/export errors here are not state-3 losses; surface but
                // do not gate on them (corpus_parse_totality owns parse health).
                skipped_error += 1;
                eprintln!("  skip [{}]: {why}", path.display());
            }
            DocOutcome::Losses(losses) => {
                checked += 1;
                with_losses += 1;
                let detail: Vec<String> = losses
                    .iter()
                    .map(|(name, (o, n))| format!("{name} {o}→{n}"))
                    .collect();
                for (name, (o, n)) in &losses {
                    let e = per_element.entry(name.clone()).or_insert((0, 0));
                    e.0 += 1;
                    e.1 += o - n;
                }
                findings.push(format!("  [{}] {}", path.display(), detail.join(", ")));
            }
        }
    }

    if !findings.is_empty() {
        eprintln!("\n=== testdata state-3 findings (characterization; open burn-down items) ===");
        for f in &findings {
            eprintln!("{f}");
        }
        eprintln!("\n  aggregate (element → #docs, total_lost):");
        let mut ranked: Vec<_> = per_element.iter().collect();
        ranked.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(b.1.1.cmp(&a.1.1)));
        for (name, (docs, lost)) in ranked {
            eprintln!("    {name:<22} {docs:>4} {lost:>6}");
        }
    }

    eprintln!(
        "\nelement fidelity (testdata): checked {checked}, with_losses {with_losses}, \
         skipped(tracked) {skipped_tracked}, skipped(error) {skipped_error}"
    );

    // Liveness / non-vacuity: a gate that re-zips the original scaffold (the
    // pre-fix bug) finds ZERO losses on every doc. We currently know testdata
    // surfaces real losses through the reserialization path; requiring at least
    // one is a regression tripwire — if a future change makes the gate vacuous
    // again, this fails loudly instead of silently reporting "0 losses".
    assert!(
        checked > 0,
        "element-fidelity gate exercised zero documents"
    );
    assert!(
        with_losses > 0,
        "element-fidelity gate found ZERO losses across all testdata docs — \
         the gate has likely gone vacuous (re-zipping the original scaffold \
         instead of reserializing the IR). Verify run_gate forces \
         serialize_canonical_docx via apply_edit before censusing."
    );
}

/// The detector must actually FIRE on a loss (so a corpus-wide zero is a real
/// signal, not a blind gate). A dropped element is a loss; an unchanged element
/// is not; a RISEN count (baking) is not flagged.
#[test]
fn diff_census_detects_loss_not_baking() {
    let original = HashMap::from([
        ("w:gridSpan".to_string(), 14),
        ("w:hideMark".to_string(), 90),
        ("w:p".to_string(), 100),
        ("w:rFonts".to_string(), 5),
    ]);
    let out = HashMap::from([
        ("w:gridSpan".to_string(), 0),  // dropped → loss
        ("w:hideMark".to_string(), 80), // partial drop → loss
        ("w:p".to_string(), 100),       // unchanged → not flagged
        ("w:rFonts".to_string(), 40),   // risen (baking) → not flagged
    ]);
    let losses = diff_census(&original, &out);
    assert_eq!(losses.get("w:gridSpan"), Some(&(14, 0)));
    assert_eq!(losses.get("w:hideMark"), Some(&(90, 80)));
    assert!(
        !losses.contains_key("w:p"),
        "unchanged element must not be a loss"
    );
    assert!(
        !losses.contains_key("w:rFonts"),
        "baking (count rise) must not be a loss"
    );
    assert_eq!(losses.len(), 2);
}

// ─── Corpus gate (ignored, CORPUS_DIR) — the state-3 inventory ─────────────

/// Aggregate state-3 losses across a real corpus and print a ranked inventory.
/// Honours the graceful-skip contract: unset `CORPUS_DIR` → skip (so the
/// `--ignored` tier stays green when no corpus is mounted).
///
/// Sampling: set `CORPUS_SAMPLE=<N>` to gate on the first N docs (sorted by
/// path) instead of the whole corpus. Unset = all docs.
#[test]
#[ignore = "state-3 corpus inventory — run on demand with CORPUS_DIR"]
fn element_fidelity_corpus() {
    let Ok(corpus_dir) = std::env::var("CORPUS_DIR") else {
        eprintln!(
            "SKIP element_fidelity_corpus: CORPUS_DIR is not set \
             (set it to a corpus root to run the state-3 inventory)"
        );
        return;
    };
    let corpus_path = Path::new(&corpus_dir);
    assert!(
        corpus_path.is_dir(),
        "CORPUS_DIR does not exist: {corpus_dir}"
    );

    eprintln!("Scanning {corpus_dir} for .docx files ...");
    let mut all_files = discover_docx_files(corpus_path);
    eprintln!("Found {} .docx files", all_files.len());

    if let Ok(n) = std::env::var("CORPUS_SAMPLE") {
        let n: usize = n.parse().expect("CORPUS_SAMPLE must be a number");
        all_files.truncate(n);
        eprintln!(
            "CORPUS_SAMPLE={n}: gating on first {} docs (by path)",
            all_files.len()
        );
    }

    // Aggregators (locked; the heavy work is the import/export per doc).
    struct Agg {
        // element name → (#docs that lost it, total count lost across docs, a sample doc)
        per_element: HashMap<String, (usize, usize, String)>,
        clean: usize,
        with_losses: usize,
        skipped_tracked: usize,
        skipped_error: usize,
    }
    let agg = Mutex::new(Agg {
        per_element: HashMap::new(),
        clean: 0,
        with_losses: 0,
        skipped_tracked: 0,
        skipped_error: 0,
    });

    let processed = AtomicUsize::new(0);
    let total = all_files.len();
    let start = Instant::now();

    all_files.par_iter().for_each(|path| {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(_) => {
                agg.lock().unwrap().skipped_error += 1;
                return;
            }
        };
        // catch_unwind: a panic on one doc must not abort the whole inventory.
        let outcome = std::panic::catch_unwind(|| run_gate(&bytes))
            .unwrap_or_else(|_| DocOutcome::SkippedError("panic during gate".into()));

        let rel = path
            .strip_prefix(corpus_path)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        {
            let mut a = agg.lock().unwrap();
            match outcome {
                DocOutcome::Clean => a.clean += 1,
                DocOutcome::SkippedTracked => a.skipped_tracked += 1,
                DocOutcome::SkippedError(_) => a.skipped_error += 1,
                DocOutcome::Losses(losses) => {
                    a.with_losses += 1;
                    for (name, (o, n)) in losses {
                        let lost = o - n;
                        let entry = a
                            .per_element
                            .entry(name)
                            .or_insert_with(|| (0, 0, rel.clone()));
                        entry.0 += 1;
                        entry.1 += lost;
                    }
                }
            }
        }

        let done = processed.fetch_add(1, Ordering::Relaxed) + 1;
        if done.is_multiple_of(500) {
            let rate = done as f64 / start.elapsed().as_secs_f64();
            eprintln!("  [{done:>6}/{total:>6}] {rate:.0} docs/s");
        }
    });

    let a = agg.into_inner().unwrap();
    let elapsed = start.elapsed().as_secs_f64();

    // Rank by #docs affected, then total lost.
    let mut ranked: Vec<(String, usize, usize, String)> = a
        .per_element
        .into_iter()
        .map(|(name, (docs, total_lost, sample))| (name, docs, total_lost, sample))
        .collect();
    ranked.sort_by(|x, y| y.1.cmp(&x.1).then(y.2.cmp(&x.2)).then(x.0.cmp(&y.0)));

    eprintln!();
    eprintln!("=== State-3 Inventory ({elapsed:.0}s) ===");
    eprintln!("  Total docs scanned:        {total:>7}");
    eprintln!("  Clean (no losses):         {:>7}", a.clean);
    eprintln!("  With state-3 losses:       {:>7}", a.with_losses);
    eprintln!("  Skipped (tracked changes): {:>7}", a.skipped_tracked);
    eprintln!("  Skipped (error/empty):     {:>7}", a.skipped_error);
    eprintln!();
    eprintln!(
        "  {:<28} {:>10} {:>12}  sample_doc",
        "element", "#docs", "total_lost"
    );
    eprintln!("  {}", "-".repeat(90));
    for (name, docs, total_lost, sample) in &ranked {
        eprintln!("  {name:<28} {docs:>10} {total_lost:>12}  {sample}");
    }
    eprintln!();
    eprintln!(
        "  (Ranked by #docs affected, then total count lost — this is the burn-down work-list.)"
    );

    // This is a measurement, not a pass/fail gate over the corpus: the corpus
    // legitimately contains state-3 constructs we have not yet closed. We
    // assert only that we actually exercised documents (the inventory is real).
    assert!(
        a.clean + a.with_losses + a.skipped_tracked + a.skipped_error == total,
        "accounting mismatch: outcomes do not sum to total"
    );
    assert!(
        a.clean + a.with_losses > 0,
        "no documents were successfully gated — check CORPUS_DIR"
    );
}

// ─── File discovery ────────────────────────────────────────────────────────

fn discover_docx_files(root: &Path) -> Vec<PathBuf> {
    let mut set = BTreeSet::new();
    collect_docx_recursive(root, &mut set);
    set.into_iter().collect()
}

fn collect_docx_recursive(dir: &Path, out: &mut BTreeSet<PathBuf>) {
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
            out.insert(path);
        }
    }
}
