use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::sync::Mutex;
use std::thread;

use rayon::prelude::*;
use serde::Deserialize;
use stemma::{DocxRuntime, ExportMode, SimpleRuntime, TransactionMeta};

mod common;

fn stress_root() -> std::path::PathBuf {
    common::stress_dir()
}

fn manifest_path() -> std::path::PathBuf {
    common::stress_dir().join("manifest.json")
}

/// Manifest path prefix for external corpus entries.  Manifest entries under
/// `stress/corpus/` are resolved to `STRESS_CORPUS_DIR` instead of the local
/// stress directory.  The corpus is too large to commit (~83k files).
const CORPUS_PREFIX: &str = "stress/corpus/";

/// Manifest path prefix for all stress fixtures.
const STRESS_PREFIX: &str = "stress/";

/// Directories under `stress/` that are fetched by `fetch-corpus.sh` but do not
/// yet have manifest entries.  The test excludes them from disk discovery so
/// they don't cause false "missing in manifest" failures.  When manifest entries
/// are added for a directory, remove it from this list.
const UNMANIFESTED_DIRS: &[&str] = &["stress/docxcorpus/", "stress/edgar/", "stress/powertools/"];

fn corpus_dir() -> String {
    std::env::var("STRESS_CORPUS_DIR").unwrap_or_else(|_| {
        panic!(
            "STRESS_CORPUS_DIR environment variable is not set.\n\
             The stress corpus (~83k files) lives outside the repo.\n\
             Set STRESS_CORPUS_DIR to the directory containing the .docx files\n\
             (e.g., STRESS_CORPUS_DIR=/path/to/corpus)."
        )
    })
}

/// Resolve a manifest path to a filesystem path, redirecting `stress/corpus/`
/// entries to the external corpus directory and `stress/` entries to the
/// absolute stress directory.
fn resolve_fixture_path(manifest_path: &str) -> String {
    if let Some(relative) = manifest_path.strip_prefix(CORPUS_PREFIX) {
        let dir = corpus_dir();
        format!("{}/{}", dir.trim_end_matches('/'), relative)
    } else if let Some(relative) = manifest_path.strip_prefix(STRESS_PREFIX) {
        stress_root().join(relative).to_string_lossy().to_string()
    } else {
        manifest_path.to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ExpectedOutcome {
    PassSupported,
    PassUnclear,
    PassNegative,
    FailRegression,
    FailUnsupported,
}

#[derive(Debug, Clone, Deserialize)]
struct FixtureExpectation {
    path: String,
    expected_outcome: ExpectedOutcome,
    expected_reason: String,
    notes: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StressManifest {
    version: u32,
    fixtures: Vec<FixtureExpectation>,
}

#[test]
#[ignore = "requires private stress corpus; set STEMMA_CORPUS_ROOT, run via just nightly"]
fn stress_manifest_integrity() {
    let manifest = load_manifest().unwrap_or_else(|err| panic!("{err}"));
    let manifest_path = manifest_path();
    assert_eq!(
        manifest.version,
        1,
        "unsupported stress manifest version {} in {}",
        manifest.version,
        manifest_path.display()
    );

    let mut fixtures_by_path: BTreeMap<String, FixtureExpectation> = BTreeMap::new();
    for fixture in manifest.fixtures {
        assert!(
            fixture.path.starts_with("stress/"),
            "manifest path must start with stress/: {}",
            fixture.path
        );
        assert!(
            fixture.path.ends_with(".docx"),
            "manifest path must be a .docx file: {}",
            fixture.path
        );
        let old = fixtures_by_path.insert(fixture.path.clone(), fixture.clone());
        assert!(
            old.is_none(),
            "duplicate manifest entry for {}",
            fixture.path
        );
    }

    let corpus_dir_env = std::env::var("STRESS_CORPUS_DIR").ok();
    let stress_root = stress_root();
    let mut discovered = collect_docx_paths_under_root(&stress_root, STRESS_PREFIX)
        .unwrap_or_else(|err| panic!("failed to discover stress fixtures: {err}"));
    // When STRESS_CORPUS_DIR is not set, remove any on-disk stress/corpus/ files
    // from discovery — these are stale copies, not the authoritative corpus.
    if corpus_dir_env.is_none() {
        discovered.retain(|p| !p.starts_with(CORPUS_PREFIX));
    }
    // Exclude directories fetched by fetch-corpus.sh that don't have manifest
    // entries yet.  Without this filter, every file in these dirs shows up as
    // "missing in manifest".
    discovered.retain(|p| !UNMANIFESTED_DIRS.iter().any(|dir| p.starts_with(dir)));

    // The corpus (~83k files) lives outside the repo. STRESS_CORPUS_DIR must
    // point to it if the manifest references corpus entries.
    let has_corpus_entries = fixtures_by_path
        .keys()
        .any(|p| p.starts_with(CORPUS_PREFIX));
    match &corpus_dir_env {
        Some(corpus_dir) => {
            let corpus_path = Path::new(corpus_dir.as_str());
            assert!(
                corpus_path.is_dir(),
                "STRESS_CORPUS_DIR={corpus_dir} is not a directory"
            );
            let corpus_files = collect_docx_paths_under_root(corpus_path, "")
                .unwrap_or_else(|err| panic!("failed to discover corpus fixtures: {err}"));
            for file_path in corpus_files {
                // Remap /path/to/corpus/foo.docx → stress/corpus/foo.docx
                let filename = Path::new(&file_path)
                    .file_name()
                    .expect("corpus file has no filename")
                    .to_string_lossy();
                discovered.push(format!("{CORPUS_PREFIX}{filename}"));
            }
        }
        None if has_corpus_entries => {
            let corpus_count = fixtures_by_path
                .keys()
                .filter(|p| p.starts_with(CORPUS_PREFIX))
                .count();
            eprintln!(
                "WARNING: STRESS_CORPUS_DIR is not set — skipping {corpus_count} corpus entries.\n\
                 Set STRESS_CORPUS_DIR to validate the full manifest\n\
                 (e.g., STRESS_CORPUS_DIR=/path/to/corpus)."
            );
            // Remove corpus entries from the manifest set so they don't
            // appear as stale (we can't check them without the corpus).
            fixtures_by_path.retain(|p, _| !p.starts_with(CORPUS_PREFIX));
        }
        None => {} // No corpus entries in manifest — nothing to do.
    }

    let discovered_set: BTreeSet<String> = discovered.into_iter().collect();
    let manifest_set: BTreeSet<String> = fixtures_by_path.keys().cloned().collect();

    let missing_in_manifest: Vec<String> =
        discovered_set.difference(&manifest_set).cloned().collect();
    let stale_in_manifest: Vec<String> =
        manifest_set.difference(&discovered_set).cloned().collect();

    assert!(
        missing_in_manifest.is_empty(),
        "manifest missing {} fixture(s), first entries: {:?}",
        missing_in_manifest.len(),
        missing_in_manifest.iter().take(10).collect::<Vec<_>>()
    );
    assert!(
        stale_in_manifest.is_empty(),
        "manifest has {} stale fixture(s), first entries: {:?}",
        stale_in_manifest.len(),
        stale_in_manifest.iter().take(10).collect::<Vec<_>>()
    );
}

#[test]
#[ignore = "expensive stress parse suite"]
fn stress_parse_contract() {
    let fixtures = sorted_fixtures().unwrap_or_else(|err| panic!("{err}"));

    let pool = rayon::ThreadPoolBuilder::new()
        .stack_size(64 * 1024 * 1024)
        .build()
        .expect("failed to build rayon thread pool");

    let failures = Mutex::new(Vec::<String>::new());

    pool.install(|| {
        fixtures.par_iter().for_each(|fixture| {
            let resolved_path = resolve_fixture_path(&fixture.path);
            let bytes = match fs::read(&resolved_path) {
                Ok(bytes) => bytes,
                Err(err) => {
                    failures
                        .lock()
                        .unwrap()
                        .push(format!("{}: read failed: {err}", fixture.path));
                    return;
                }
            };

            let result = roundtrip_docx(&bytes);
            let observed_reason = result.as_ref().err().map(|err| classify_parse_reason(err));

            match fixture.expected_outcome {
                ExpectedOutcome::PassSupported
                | ExpectedOutcome::PassUnclear
                | ExpectedOutcome::FailRegression => {
                    if let Err(err) = &result {
                        failures.lock().unwrap().push(format!(
                            "{}: expected parse+export success for {:?}, got error: {}",
                            fixture.path, fixture.expected_outcome, err
                        ));
                    }
                }
                ExpectedOutcome::PassNegative => match &result {
                    Ok(_) => failures.lock().unwrap().push(format!(
                        "{}: expected parse failure ({}) but parse+export succeeded",
                        fixture.path, fixture.expected_reason
                    )),
                    Err(err) => {
                        let reason = classify_parse_reason(err);
                        if reason != fixture.expected_reason {
                            failures.lock().unwrap().push(format!(
                                "{}: expected reason {}, got {} (raw: {})",
                                fixture.path, fixture.expected_reason, reason, err
                            ));
                        }
                    }
                },
                ExpectedOutcome::FailUnsupported => match &result {
                    Ok(_) => failures.lock().unwrap().push(format!(
                        "{}: expected unsupported parse failure ({}) but parse+export succeeded",
                        fixture.path, fixture.expected_reason
                    )),
                    Err(err) => {
                        let reason = classify_parse_reason(err);
                        if reason != fixture.expected_reason {
                            failures.lock().unwrap().push(format!(
                                "{}: expected reason {}, got {} (raw: {})",
                                fixture.path, fixture.expected_reason, reason, err
                            ));
                        }
                    }
                },
            }

            if fixture.notes.is_some() && observed_reason.as_deref() == Some("unknown_parse_error")
            {
                failures.lock().unwrap().push(format!(
                    "{}: parse failed with unknown reason despite manifest notes",
                    fixture.path
                ));
            }
        });
    });

    let failures = failures.into_inner().unwrap();
    assert!(
        failures.is_empty(),
        "stress parse contract mismatches ({}):\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

#[test]
#[ignore = "expensive stress redline suite"]
fn stress_redline_identity_contract() {
    // The diff pipeline can be deeply recursive on large documents, so run
    // the test body on a thread with a generous stack (32 MB).
    let result = thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(stress_redline_identity_contract_inner)
        .expect("failed to spawn thread")
        .join();

    match result {
        Ok(()) => {}
        Err(panic_payload) => std::panic::resume_unwind(panic_payload),
    }
}

fn stress_redline_identity_contract_inner() {
    let fixtures = sorted_fixtures().unwrap_or_else(|err| panic!("{err}"));

    let pool = rayon::ThreadPoolBuilder::new()
        .stack_size(64 * 1024 * 1024)
        .build()
        .expect("failed to build rayon thread pool");

    let failures = Mutex::new(Vec::<String>::new());

    pool.install(|| {
        fixtures.par_iter().for_each(|fixture| {
            match fixture.expected_outcome {
                ExpectedOutcome::PassSupported | ExpectedOutcome::FailRegression => {}
                ExpectedOutcome::PassUnclear => {}
                _ => return,
            }

            let resolved_path = resolve_fixture_path(&fixture.path);
            let bytes = match fs::read(&resolved_path) {
                Ok(bytes) => bytes,
                Err(err) => {
                    failures
                        .lock()
                        .unwrap()
                        .push(format!("{}: read failed: {err}", fixture.path));
                    return;
                }
            };

            let meta = TransactionMeta {
                author: "stress_manifest".to_string(),
                reason: Some("stress redline identity".to_string()),
                timestamp_utc: Some("2024-01-15T10:30:00Z".to_string()),
            };

            if let Err(err) = redline_identity_docx(&bytes, meta) {
                failures.lock().unwrap().push(format!(
                    "{}: identity redline failed for {:?}: {}",
                    fixture.path, fixture.expected_outcome, err
                ));
            }
        });
    });

    let failures = failures.into_inner().unwrap();
    assert!(
        failures.is_empty(),
        "stress redline identity contract mismatches ({}):\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

fn roundtrip_docx(docx_bytes: &[u8]) -> Result<Vec<u8>, String> {
    let runtime = SimpleRuntime::new();
    let import = runtime
        .import_docx(docx_bytes)
        .map_err(|err| format!("import_docx failed: {err:?}"))?;

    runtime
        .export_docx(&import.doc_handle, ExportMode::Redline)
        .map_err(|err| format!("export_docx failed: {err:?}"))
}

fn redline_identity_docx(docx_bytes: &[u8], meta: TransactionMeta) -> Result<Vec<u8>, String> {
    let runtime = SimpleRuntime::new();

    let base = runtime
        .import_docx(docx_bytes)
        .map_err(|err| format!("import base failed: {err:?}"))?;

    let target = runtime
        .import_docx(docx_bytes)
        .map_err(|err| format!("import target failed: {err:?}"))?;

    runtime
        .diff_and_redline(&base.doc_handle, &target.doc_handle, meta)
        .map_err(|err| format!("diff_and_redline failed: {err:?}"))?;

    let exported = runtime
        .export_docx(&base.doc_handle, ExportMode::Redline)
        .map_err(|err| format!("export_docx failed: {err:?}"))?;

    let verify_runtime = SimpleRuntime::new();
    verify_runtime
        .import_docx(&exported)
        .map_err(|err| format!("re-import of exported bytes failed: {err:?}"))?;

    Ok(exported)
}

fn classify_parse_reason(error: &str) -> String {
    if error.contains("unknown run-level element: AlternateContent") {
        return "unsupported_alternate_content".to_string();
    }
    if error.contains("unknown run-level element: ruby") {
        return "unsupported_ruby".to_string();
    }
    if error.contains("unknown run-level element: delInstrText") {
        return "unsupported_delinstrtext".to_string();
    }
    if error.contains("unknown paragraph-level element: moveTo") {
        return "unsupported_move_to".to_string();
    }
    if error.contains("unknown paragraph-level element: oMath") {
        return "unsupported_omath".to_string();
    }
    if error.contains("wordprocessingml parse error: Malformed XML") {
        return "malformed_xml".to_string();
    }
    if error.contains("docx io error: Invalid checksum") {
        return "invalid_checksum".to_string();
    }
    if error.contains("docx read failed: invalid Zip archive") {
        return "invalid_zip".to_string();
    }
    if error.contains("missing word/_rels/document.xml.rels") {
        return "missing_rels".to_string();
    }
    if error.contains("missing word/document.xml") {
        return "missing_document_xml".to_string();
    }
    if error.contains("unknown paragraph-level element: subDoc") {
        return "unsupported_subdoc".to_string();
    }
    if error.contains("must contain at least one row") {
        return "empty_table".to_string();
    }
    if error.contains("body elements, expected exactly 1") {
        return "malformed_multiple_body".to_string();
    }
    if error.contains("unknown run-level element: pgNum") {
        return "unsupported_pgnum".to_string();
    }
    if error.contains("unknown run-level element: bookmarkStart") {
        return "unsupported_bookmark_run_level".to_string();
    }
    if error.contains("missing required tracked change attribute") {
        return "malformed_tracked_change".to_string();
    }
    if error.contains("missing required Target attribute") {
        return "malformed_rels".to_string();
    }
    if error.contains("invalid width value ''")
        || error.contains("out of range in tcW")
        || error.contains("out of range in tblW")
    {
        return "invalid_tcw_width".to_string();
    }
    if error.contains("invalid width value '100%'") {
        return "invalid_width_percent".to_string();
    }
    if error.contains("unknown paragraph-level element: rPr") {
        return "bare_rpr_in_paragraph".to_string();
    }
    if error.contains("unknown paragraph-level element: pStyle") {
        return "bare_pstyle_in_paragraph".to_string();
    }
    if error.contains("unknown paragraph-level element: dir") {
        return "unsupported_bidi_dir".to_string();
    }
    if error.contains("unknown paragraph-level element: bdo") {
        return "unsupported_bidi_bdo".to_string();
    }
    // Nested <w:r> inside another run — "unknown run-level element: r"
    // Must check after more specific "run-level element: r..." patterns above.
    if error.contains("unknown run-level element: r\"")
        || error.contains("unknown run-level element: r}")
        || (error.contains("unknown run-level element: r")
            && !error.contains("run-level element: rP")
            && !error.contains("run-level element: ru"))
    {
        return "nested_run".to_string();
    }
    if error.contains("unknown run-level element: rPrChange") {
        return "bare_rprchange_in_run".to_string();
    }
    if error.contains("unknown run-level element: rPr") {
        return "bare_rpr_in_run".to_string();
    }
    if error.contains("failed to fill whole buffer") {
        return "truncated_zip".to_string();
    }
    if error.contains("archive contains") && error.contains("max 1000") {
        return "archive_entry_limit".to_string();
    }
    if error.contains("unknown BorderStyle") {
        return "invalid_border_style".to_string();
    }
    "unknown_parse_error".to_string()
}

fn sorted_fixtures() -> Result<Vec<FixtureExpectation>, String> {
    let manifest = load_manifest()?;
    let mut fixtures = manifest.fixtures;
    if std::env::var("STRESS_CORPUS_DIR").is_err() {
        let corpus_count = fixtures
            .iter()
            .filter(|f| f.path.starts_with(CORPUS_PREFIX))
            .count();
        if corpus_count > 0 {
            eprintln!(
                "SKIP {} corpus fixture(s) in stress suites: STRESS_CORPUS_DIR is not set",
                corpus_count
            );
            fixtures.retain(|f| !f.path.starts_with(CORPUS_PREFIX));
        }
    }
    fixtures.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(fixtures)
}

fn load_manifest() -> Result<StressManifest, String> {
    let path = manifest_path();
    let raw = fs::read_to_string(&path)
        .map_err(|err| format!("read {} failed: {err}", path.display()))?;
    serde_json::from_str(&raw).map_err(|err| format!("parse {} failed: {err}", path.display()))
}

fn collect_docx_paths_under_root(root: &Path, prefix: &str) -> Result<Vec<String>, String> {
    if !root.exists() {
        return Err(format!("stress root {} does not exist", root.display()));
    }
    if !root.is_dir() {
        return Err(format!("stress root {} is not a directory", root.display()));
    }

    let mut stack = vec![root.to_path_buf()];
    let mut paths = Vec::new();

    while let Some(dir) = stack.pop() {
        let entries = fs::read_dir(&dir)
            .map_err(|err| format!("read_dir {} failed: {err}", dir.display()))?;
        for entry in entries {
            let entry = entry
                .map_err(|err| format!("read_dir entry in {} failed: {err}", dir.display()))?;
            let file_type = entry
                .file_type()
                .map_err(|err| format!("file_type {} failed: {err}", entry.path().display()))?;
            let path = entry.path();
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if file_type.is_file() && path.extension().is_some_and(|ext| ext == "docx") {
                // Produce manifest-format paths: strip the root and prepend the prefix.
                let relative = path.strip_prefix(root).unwrap_or(&path);
                let normalized = normalize_path(relative);
                if prefix.is_empty() {
                    paths.push(normalized);
                } else {
                    paths.push(format!("{prefix}{normalized}"));
                }
            }
        }
    }

    paths.sort();
    Ok(paths)
}

fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
