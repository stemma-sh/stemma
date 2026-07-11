//! Daily VALIDATOR-CLEAN SWEEP over every checked-in testdata DOCX.
//!
//! This is the hermetic stand-in for invariant #13 (Word-open-clean): for every
//! fixture under `testdata/`, we `serialize(parse(A))` and assert the
//! post-serialization linker (`docx_validate::validate_docx`) reports ZERO
//! blocking findings, gated exactly as the disk/MCP save path gates
//! (`ValidatorLevel::Blocking` → `BLOCKING_RULES`).
//!
//! It mirrors how `roundtrip_fidelity.rs` (#12) sweeps the same fixture set, but
//! checks a different invariant: the bytes we emit are *structurally clean* by
//! the rules Word would otherwise repair, rather than IR-stable.
//!
//! Why this matters: the RELATIONAL invariants and the Word-clean class do not
//! run daily over real structures unless something sweeps every fixture through
//! the Blocking gate. Without this, a structural regression that produces
//! Word-repairable output (bad child ordering, dangling relationship target,
//! missing content-type override) can land green because no daily test exercises
//! the Blocking validator over the corpus.

use std::collections::BTreeSet;
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};

use rayon::prelude::*;
use stemma::api::Document;
use stemma::{ExportMode, ExportOptions, ValidatorLevel};

/// Fixtures expected to fail import (intentionally invalid OOXML). These never
/// reach the serializer, so they cannot be swept for clean output. Kept in sync
/// with `roundtrip_fidelity.rs`'s list — both sweeps skip the same inputs.
const EXPECTED_IMPORT_FAILURES: &[&str] = &[
    // Empty table cell violates CT_Tc content model (requires at least one w:p).
    "testdata/spec-compliance/edge-cases/empty-table-cells/input.docx",
    // Empty table (zero rows) violates §17.4.37 (requires non-zero rows).
    "testdata/spec-compliance/edge-cases/empty-table-zero-rows/input.docx",
];

/// Exemption is by fixture identity, not by host path spelling: discovered
/// paths carry the platform separator (`\` on Windows), while the entries
/// above are canonical `/`. A raw `ends_with` misses every entry on Windows
/// and un-exempts the intentionally-invalid fixtures.
fn is_expected_import_failure(fixture_path: &str) -> bool {
    let unix_path = fixture_path.replace('\\', "/");
    EXPECTED_IMPORT_FAILURES
        .iter()
        .any(|e| unix_path.ends_with(e))
}

#[test]
fn expected_import_failure_matching_is_separator_agnostic() {
    assert!(is_expected_import_failure(
        "testdata/spec-compliance/edge-cases/empty-table-zero-rows/input.docx"
    ));
    assert!(is_expected_import_failure(
        r"testdata\spec-compliance\edge-cases\empty-table-zero-rows\input.docx"
    ));
    assert!(!is_expected_import_failure(
        r"testdata\spec-compliance\edge-cases\other\input.docx"
    ));
}

/// Every checked-in fixture, once parsed and re-serialized, must pass the
/// Blocking post-serialization validator with zero blocking findings.
///
/// Post-condition: `Document::serialize(.., Blocking)` returns `Ok` for every
/// fixture. A blocking finding here means the serializer emitted OOXML that Word
/// would refuse / repair — a real structural regression, not a fidelity nit.
#[test]
fn validator_clean_sweep_all_fixtures() {
    let docs = discover_all_fixture_docs();
    assert!(!docs.is_empty(), "no fixture docs found under testdata/");

    let checked = AtomicUsize::new(0);

    let failures: Vec<String> = docs
        .par_iter()
        .filter(|fixture_path| !is_expected_import_failure(fixture_path))
        .filter_map(|fixture_path| {
            let result = serialize_clean(fixture_path);
            checked.fetch_add(1, Ordering::Relaxed);
            result.err()
        })
        .collect();

    let checked = checked.load(Ordering::Relaxed);
    eprintln!(
        "validator clean sweep: checked {checked} fixtures, {} blocking failure(s)",
        failures.len()
    );

    assert!(
        failures.is_empty(),
        "validator clean-sweep violations ({} of {checked}):\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// Parse a fixture and serialize it through the Blocking gate. `Err` carries a
/// per-fixture context string (the blocking findings or the failing stage).
fn serialize_clean(fixture_path: &str) -> Result<(), String> {
    let bytes =
        fs::read(fixture_path).map_err(|err| format!("[{fixture_path}] read failed: {err}"))?;

    let doc =
        Document::parse(&bytes).map_err(|err| format!("[{fixture_path}] parse failed: {err:?}"))?;

    doc.serialize(&ExportOptions {
        mode: ExportMode::Redline,
        // Blocking is exactly the gate the disk/MCP save path uses. A blocking
        // finding maps to a Word "needs repair" class.
        validator_level: ValidatorLevel::Blocking,
        validator: None,
    })
    .map(|_| ())
    .map_err(|err| format!("[{fixture_path}] {}", err.message))
}

// ---------------------------------------------------------------------------
// Fixture discovery (mirrors roundtrip_fidelity.rs)
// ---------------------------------------------------------------------------

fn discover_all_fixture_docs() -> Vec<String> {
    let mut paths = BTreeSet::new();
    collect_docx_files("testdata", &mut paths);
    paths.into_iter().collect()
}

fn collect_docx_files(dir: &str, out: &mut BTreeSet<String>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_docx_files(&path.to_string_lossy(), out);
        } else if path.extension().is_some_and(|ext| ext == "docx") {
            out.insert(path.to_string_lossy().to_string());
        }
    }
}
