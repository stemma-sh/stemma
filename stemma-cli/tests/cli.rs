//! Integration tests driving the built `stemma` binary against real engine
//! fixtures. Each test shells out to the CLI (never the library) and, where a
//! round-trip is the contract, reopens the output with the engine to verify by
//! CONTENT — accept-all of a compare is the target, reject-all is the base.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use stemma::ExportOptions;
use stemma::api::Document;
use stemma::edit_v4::parse_transaction;

/// Absolute path to a fixture under the engine's testdata tree.
fn fixture(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../stemma-engine/testdata")
        .join(rel)
}

/// Run the built binary with `args`, returning the captured output.
fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_stemma"))
        .args(args)
        .output()
        .expect("spawn stemma binary")
}

fn text_of(path: &Path) -> String {
    let bytes = std::fs::read(path).expect("read output docx");
    Document::parse(&bytes)
        .expect("parse output docx")
        .to_text()
}

/// The accept-all / reject-all plain-text readings of a document on disk.
fn readings(path: &Path) -> (String, String) {
    let bytes = std::fs::read(path).expect("read docx");
    let doc = Document::parse(&bytes).expect("parse docx");
    let accepted = doc.read_accepted().expect("accept-all").to_text();
    let rejected = doc.read_rejected().expect("reject-all").to_text();
    (accepted, rejected)
}

#[test]
fn compare_produces_a_reviewable_redline() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("redline.docx");

    let output = run(&[
        "compare",
        fixture("simple-text/before.docx").to_str().unwrap(),
        fixture("simple-text/after.docx").to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "compare should succeed: {output:?}"
    );
    assert!(out.exists(), "compare must write the redline file");

    // The round-trip contract: reject-all reconstructs the base, accept-all the
    // target. This proves the redline carries the discovered changes.
    let base = text_of(&fixture("simple-text/before.docx"));
    let target = text_of(&fixture("simple-text/after.docx"));
    let (accepted, rejected) = readings(&out);
    assert_eq!(rejected, base, "reject-all == base");
    assert_eq!(accepted, target, "accept-all == target");
}

#[test]
fn resolve_accept_all_yields_the_target_text() {
    let dir = tempfile::tempdir().unwrap();
    let redline = dir.path().join("redline.docx");
    let accepted = dir.path().join("accepted.docx");

    assert!(
        run(&[
            "compare",
            fixture("simple-text/before.docx").to_str().unwrap(),
            fixture("simple-text/after.docx").to_str().unwrap(),
            "-o",
            redline.to_str().unwrap(),
        ])
        .status
        .success()
    );

    let output = run(&[
        "resolve",
        redline.to_str().unwrap(),
        "-o",
        accepted.to_str().unwrap(),
        "--accept-all",
    ]);
    assert!(
        output.status.success(),
        "resolve --accept-all should succeed: {output:?}"
    );

    let target = text_of(&fixture("simple-text/after.docx"));
    assert_eq!(
        text_of(&accepted),
        target,
        "accept-all of the redline == target"
    );
}

#[test]
fn resolve_reject_all_yields_the_base_text() {
    let dir = tempfile::tempdir().unwrap();
    let redline = dir.path().join("redline.docx");
    let rejected = dir.path().join("rejected.docx");

    assert!(
        run(&[
            "compare",
            fixture("simple-text/before.docx").to_str().unwrap(),
            fixture("simple-text/after.docx").to_str().unwrap(),
            "-o",
            redline.to_str().unwrap(),
        ])
        .status
        .success()
    );

    let output = run(&[
        "resolve",
        redline.to_str().unwrap(),
        "-o",
        rejected.to_str().unwrap(),
        "--reject-all",
    ]);
    assert!(
        output.status.success(),
        "resolve --reject-all should succeed: {output:?}"
    );

    let base = text_of(&fixture("simple-text/before.docx"));
    assert_eq!(
        text_of(&rejected),
        base,
        "reject-all of the redline == base"
    );
}

#[test]
fn resolve_accept_author_keeps_that_authors_change() {
    // A redline carrying one named author's tracked change, authored through the
    // engine so the CLI has a real by-author selection to resolve.
    let dir = tempfile::tempdir().unwrap();
    let redlined = dir.path().join("named-redline.docx");
    let resolved = dir.path().join("resolved.docx");
    write_named_redline(&redlined, "Alice", "Alice's revised clause.");

    let output = run(&[
        "resolve",
        redlined.to_str().unwrap(),
        "-o",
        resolved.to_str().unwrap(),
        "--accept-author",
        "Alice",
    ]);
    assert!(
        output.status.success(),
        "resolve --accept-author should succeed: {output:?}"
    );
    assert!(
        text_of(&resolved).contains("Alice's revised clause."),
        "accepting Alice's change keeps her new text"
    );
}

#[test]
fn extract_json_parses_and_carries_blocks_and_revisions() {
    let dir = tempfile::tempdir().unwrap();
    let redline = dir.path().join("redline.docx");
    assert!(
        run(&[
            "compare",
            fixture("simple-text/before.docx").to_str().unwrap(),
            fixture("simple-text/after.docx").to_str().unwrap(),
            "-o",
            redline.to_str().unwrap(),
        ])
        .status
        .success()
    );

    let output = run(&["extract", redline.to_str().unwrap(), "--format", "json"]);
    assert!(
        output.status.success(),
        "extract json should succeed: {output:?}"
    );

    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout is valid JSON");
    assert!(
        value["blocks"].as_array().is_some_and(|b| !b.is_empty()),
        "json carries a non-empty blocks array"
    );
    let revisions = value["revisions"].as_array().expect("revisions array");
    assert!(
        !revisions.is_empty(),
        "a redline's json carries pending revisions"
    );
    // Each revision row has the documented shape.
    for rev in revisions {
        assert!(rev["revision_id"].is_number());
        assert!(rev["kind"].is_string());
        assert!(rev["block_id"].is_string());
        assert!(rev["excerpt"].is_string());
    }
}

#[test]
fn compare_author_attributes_the_discovered_revisions() {
    // `compare --author NAME` attributes every discovered revision to NAME; the
    // attribution surfaces in `extract --format json`'s revision rows and the
    // diff round-trip is unchanged.
    let dir = tempfile::tempdir().unwrap();
    let redline = dir.path().join("redline.docx");
    let output = run(&[
        "compare",
        fixture("simple-text/before.docx").to_str().unwrap(),
        fixture("simple-text/after.docx").to_str().unwrap(),
        "-o",
        redline.to_str().unwrap(),
        "--author",
        "Reviewer",
    ]);
    assert!(
        output.status.success(),
        "compare --author should succeed: {output:?}"
    );

    let json = run(&["extract", redline.to_str().unwrap(), "--format", "json"]);
    assert!(
        json.status.success(),
        "extract json should succeed: {json:?}"
    );
    let value: serde_json::Value =
        serde_json::from_slice(&json.stdout).expect("stdout is valid JSON");
    let revisions = value["revisions"].as_array().expect("revisions array");
    assert!(!revisions.is_empty(), "the redline carries revisions");
    assert!(
        revisions
            .iter()
            .all(|r| r["author"].as_str() == Some("Reviewer")),
        "every revision row must be attributed to the supplied author: {revisions:?}"
    );

    // Attribution does not disturb the diff round-trip.
    let base = text_of(&fixture("simple-text/before.docx"));
    let target = text_of(&fixture("simple-text/after.docx"));
    let (accepted, rejected) = readings(&redline);
    assert_eq!(rejected, base, "reject-all == base");
    assert_eq!(accepted, target, "accept-all == target");
}

#[test]
fn compare_empty_author_is_refused() {
    // No silent fallback to anonymous: an explicit empty `--author` is refused.
    let dir = tempfile::tempdir().unwrap();
    let redline = dir.path().join("redline.docx");
    let output = run(&[
        "compare",
        fixture("simple-text/before.docx").to_str().unwrap(),
        fixture("simple-text/after.docx").to_str().unwrap(),
        "-o",
        redline.to_str().unwrap(),
        "--author",
        "",
    ]);
    assert!(
        !output.status.success(),
        "an empty --author must fail, not silently produce an anonymous redline"
    );
    assert!(
        !redline.exists(),
        "no redline is written when the author is refused"
    );
}

#[test]
fn extract_text_prints_the_body() {
    let output = run(&[
        "extract",
        fixture("simple-text/before.docx").to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "extract text should succeed: {output:?}"
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(!stdout.trim().is_empty(), "extract text prints the body");
}

#[test]
fn validate_reports_ok_on_a_valid_fixture() {
    let output = run(&[
        "validate",
        fixture("simple-text/before.docx").to_str().unwrap(),
    ]);
    assert!(output.status.success(), "validate should pass: {output:?}");
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(
        stdout.starts_with("OK:"),
        "validate prints an OK line: {stdout:?}"
    );
    assert!(
        stdout.contains("block"),
        "the OK line reports a block count"
    );
}

#[test]
fn garbage_input_fails_actionably_without_panicking() {
    let dir = tempfile::tempdir().unwrap();
    let garbage = dir.path().join("garbage.docx");
    std::fs::write(&garbage, b"this is not a docx package").unwrap();

    let output = run(&["validate", garbage.to_str().unwrap()]);
    assert!(!output.status.success(), "garbage input must fail");
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(
        stderr.starts_with("error:"),
        "one-line actionable error: {stderr:?}"
    );
    assert!(
        stderr.contains("garbage.docx"),
        "the error names the offending file: {stderr:?}"
    );
    assert!(!stderr.contains("panicked"), "must not panic: {stderr:?}");
}

#[test]
fn unknown_revision_id_is_a_named_error() {
    let dir = tempfile::tempdir().unwrap();
    let redline = dir.path().join("redline.docx");
    let out = dir.path().join("out.docx");
    assert!(
        run(&[
            "compare",
            fixture("simple-text/before.docx").to_str().unwrap(),
            fixture("simple-text/after.docx").to_str().unwrap(),
            "-o",
            redline.to_str().unwrap(),
        ])
        .status
        .success()
    );

    let output = run(&[
        "resolve",
        redline.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--accept-ids",
        "99999",
    ]);
    assert!(
        !output.status.success(),
        "an unknown id must fail, not no-op"
    );
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(
        stderr.contains("99999"),
        "the error names the missing id: {stderr:?}"
    );
    assert!(!out.exists(), "no output is written on a failed selection");
}

#[test]
fn refuses_to_write_output_over_the_input() {
    let dir = tempfile::tempdir().unwrap();
    let redline = dir.path().join("redline.docx");
    assert!(
        run(&[
            "compare",
            fixture("simple-text/before.docx").to_str().unwrap(),
            fixture("simple-text/after.docx").to_str().unwrap(),
            "-o",
            redline.to_str().unwrap(),
        ])
        .status
        .success()
    );

    let output = run(&[
        "resolve",
        redline.to_str().unwrap(),
        "-o",
        redline.to_str().unwrap(),
        "--accept-all",
    ]);
    assert!(
        !output.status.success(),
        "overwriting the input must be refused"
    );
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(
        stderr.contains("refusing to overwrite"),
        "the refusal is explicit: {stderr:?}"
    );
}

#[test]
fn resolve_requires_a_disposition() {
    let dir = tempfile::tempdir().unwrap();
    let redline = dir.path().join("redline.docx");
    let out = dir.path().join("out.docx");
    assert!(
        run(&[
            "compare",
            fixture("simple-text/before.docx").to_str().unwrap(),
            fixture("simple-text/after.docx").to_str().unwrap(),
            "-o",
            redline.to_str().unwrap(),
        ])
        .status
        .success()
    );

    // No disposition flag: clap's required ArgGroup rejects the invocation.
    let output = run(&[
        "resolve",
        redline.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert!(!output.status.success(), "a disposition is required");
    assert!(
        !out.exists(),
        "nothing is written when the invocation is rejected"
    );
}

/// Author one tracked whole-paragraph replacement as `author`, via the engine's
/// v4 wire path, and write the resulting redline to `path`. Mirrors the
/// `resolve_a_redline` example's seeding helper.
fn write_named_redline(path: &Path, author: &str, replacement: &str) {
    const ORIGINAL: &str = "This is a very interesting test that includes lots of stuff.";
    let bytes = std::fs::read(fixture("paragraphs/before.docx")).unwrap();
    let doc = Document::parse(&bytes).expect("parse fixture");
    let view = doc.read();
    let target = view
        .blocks
        .iter()
        .find(|b| b.text.contains(ORIGINAL))
        .expect("fixture contains the seed paragraph");
    let txn_json = format!(
        r#"{{ "ops": [ {{ "op": "replace", "target": "{}", "guard": "{}",
               "content": {{ "type": "paragraph",
                             "content": [ {{ "type": "text", "text": "{replacement}" }} ] }} }} ],
             "revision": {{ "author": "{author}" }} }}"#,
        target.id, target.guard
    );
    let txn = parse_transaction(&txn_json)
        .expect("v4 schema-valid")
        .into_edit_transaction()
        .expect("v4 -> EditTransaction");
    let edited = doc.apply(&txn).expect("apply tracked replace");
    let out = edited
        .serialize(&ExportOptions::default())
        .expect("serialize redline");
    std::fs::write(path, out).expect("write named redline");
}
