//! The comparison fixpoint, over every before/after fixture pair in the
//! engine's test data.
//!
//! A redline is correct when each terminal reading *is* the input it stands
//! for. After a save and reopen, Accept All must compare to zero semantic
//! changes against the target and Reject All against the base. Matching text
//! is not enough: this also catches formatting, image, and annotation drift,
//! which a text-only check passes.
//!
//! Some pairs need a change no native Word revision can carry, and comparison
//! refuses them with `UnsupportedEdit` instead of emitting a lossy redline.
//! Those refusals are part of the contract and are listed in [`REFUSED`]
//! with their reasons, so a pair that starts or stops being refused fails
//! here.

use std::path::{Path, PathBuf};

use stemma::ExportOptions;
use stemma::api::{Document, Resolution};
use stemma::runtime::ErrorCode;

/// Fixture pairs whose comparison is refused, with the refusal's message.
const REFUSED: &[(&str, &str)] = &[
    (
        "image-math-combined",
        "side-only block equations have no qualified native Word carrier",
    ),
    (
        "math-equations",
        "side-only block equations have no qualified native Word carrier",
    ),
    (
        "math-wc021",
        "side-only block equations have no qualified native Word carrier",
    ),
    (
        "opaque-roundtrip",
        "table-row replacement cannot be followed by another change in the same story",
    ),
    (
        "safe-us-vs-canada",
        "package reconciliation cannot switch differing document defaults between native Word terminals",
    ),
    (
        "safe-us-vs-cayman",
        "package reconciliation cannot switch differing document defaults between native Word terminals",
    ),
    (
        "safe-valcap-vs-discount",
        "package reconciliation cannot switch differing document defaults between native Word terminals",
    ),
    (
        "safe-valcap-vs-mfn",
        "package reconciliation cannot switch differing document defaults between native Word terminals",
    ),
];

fn testdata_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../stemma-engine/testdata")
}

/// Every directory under `root` holding both `before.docx` and `after.docx`,
/// named by its path relative to `root`.
fn fixture_pairs(root: &Path) -> Vec<(String, PathBuf)> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
        let entries = std::fs::read_dir(dir)
            .unwrap_or_else(|err| panic!("read fixture directory {}: {err}", dir.display()));
        for entry in entries {
            let path = entry.expect("fixture directory entry").path();
            if !path.is_dir() {
                continue;
            }
            if path.join("before.docx").is_file() && path.join("after.docx").is_file() {
                let name = path
                    .strip_prefix(root)
                    .expect("fixture lies under the test data root")
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((name, path.clone()));
            }
            walk(root, &path, out);
        }
    }
    let mut pairs = Vec::new();
    walk(root, root, &mut pairs);
    pairs.sort();
    pairs
}

/// `None` when the pair holds the fixpoint; otherwise what went wrong.
fn check_pair(name: &str, dir: &Path) -> Option<String> {
    let parse = |file: &str| {
        let bytes = std::fs::read(dir.join(file))
            .unwrap_or_else(|err| panic!("{name}: read {file}: {err}"));
        Document::parse(&bytes).unwrap_or_else(|err| panic!("{name}: parse {file}: {err:?}"))
    };
    let base = parse("before.docx");
    let target = parse("after.docx");
    let refusal = REFUSED
        .iter()
        .find(|(refused, _)| *refused == name)
        .map(|(_, message)| *message);

    let redline = match (stemma_diff::diff_as(&base, &target, "fixpoint"), refusal) {
        (Ok(redline), None) => redline,
        (Err(err), Some(message))
            if err.code == ErrorCode::UnsupportedEdit && err.message.contains(message) =>
        {
            return None;
        }
        (Ok(_), Some(message)) => {
            return Some(format!(
                "expected the refusal {message:?}, but comparison succeeded"
            ));
        }
        (Err(err), _) => return Some(format!("comparison failed: {err:?}")),
    };
    let bytes = match redline.serialize(&ExportOptions::default()) {
        Ok(bytes) => bytes,
        Err(err) => return Some(format!("redline does not serialize: {err:?}")),
    };
    let reopened = Document::parse(&bytes).unwrap_or_else(|err| panic!("{name}: reopen: {err:?}"));
    let mut problems = Vec::new();
    for (label, projection, input) in [
        ("Accept All", Resolution::AcceptAll, &target),
        ("Reject All", Resolution::RejectAll, &base),
    ] {
        // Each reading is saved and reopened, like the input it is compared
        // against: the comparison is between two documents as Word receives
        // them, not between two in-memory spellings of one.
        let reading = reopened
            .project(projection)
            .and_then(|reading| reading.serialize(&ExportOptions::default()))
            .and_then(|bytes| Document::parse(&bytes))
            .unwrap_or_else(|err| panic!("{name}: {label}: {err:?}"));
        match stemma_diff::diff_detailed(&reading, input) {
            Ok(remaining) if remaining.semantic_change_count == 0 => {}
            Ok(remaining) => problems.push(format!(
                "{label} differs from its input by {} change(s)",
                remaining.semantic_change_count
            )),
            Err(err) => problems.push(format!("{label} does not re-compare: {err:?}")),
        }
    }
    (!problems.is_empty()).then(|| problems.join("; "))
}

#[test]
fn every_fixture_redline_reproduces_both_inputs() {
    let root = testdata_root();
    let pairs = fixture_pairs(&root);
    assert!(
        pairs.len() >= 60,
        "expected the engine's fixture pairs under {}, found {}",
        root.display(),
        pairs.len()
    );
    for (refused, _) in REFUSED {
        assert!(
            pairs.iter().any(|(name, _)| name == refused),
            "REFUSED names {refused:?}, which is no fixture pair"
        );
    }

    let workers = std::thread::available_parallelism().map_or(1, usize::from);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let failures = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some((name, dir)) = pairs.get(index) else {
                        break;
                    };
                    if let Some(problem) = check_pair(name, dir) {
                        failures.lock().unwrap().push(format!("{name}: {problem}"));
                    }
                }
            });
        }
    });

    let mut failures = failures.into_inner().unwrap();
    failures.sort();
    assert!(
        failures.is_empty(),
        "{} of {} fixture pairs break the comparison fixpoint:\n{}",
        failures.len(),
        pairs.len(),
        failures.join("\n")
    );
}
