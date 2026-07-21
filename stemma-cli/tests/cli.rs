//! Integration tests driving the built `stemma` binary against real engine
//! fixtures. Each test shells out to the CLI (never the library) and, where a
//! round-trip is the contract, reopens the output with the engine to verify by
//! CONTENT — accept-all of a compare is the target, reject-all is the base.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use stemma::api::Document;
use stemma::audit::{DirectChangeKind, RevisionDisposition};
use stemma::edit_v4::parse_transaction;
use stemma::{ExportOptions, StoryScope};
use stemma_artifacts::{
    DeclaredTaskEffect, ManifestArtifact, ManifestIdentity, PathAuthority, TASK_MANIFEST_SCHEMA_V1,
    TaskAuditBinding, TaskAuditCounts, TaskAuditScope, TaskAuditStatus, TaskAuditVerdict,
    TaskBarrierPolicy, TaskEffectOperation, TaskEffectStatus, TaskManifest, TaskManifestEffect,
    TaskManifestProducer, TaskManifestStatus, TaskManifestTarget, TaskMatchMode,
    TaskReplacementScope, encode_task_manifest,
};

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

fn run_in(current_dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_stemma"))
        .current_dir(current_dir)
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

fn write_worklist(path: &Path, author: &str, changes: serde_json::Value) {
    write_worklist_for(path, &fixture("simple-text/before.docx"), author, changes);
}

fn write_worklist_for(path: &Path, input: &Path, author: &str, changes: serde_json::Value) {
    let authority = PathAuthority::explicit().unwrap();
    let input = authority.read_source(input, "test_input", None).unwrap();
    let bytes = serde_json::to_vec_pretty(&serde_json::json!({
        "schema": "stemma.worklist.v0",
        "input": {
            "sha256": input.identity().digest.hex,
            "bytes": input.identity().bytes,
        },
        "author": author,
        "changes": changes,
    }))
    .expect("encode worklist");
    std::fs::write(path, bytes).expect("write worklist");
}

fn receipt_for(output: &Path) -> PathBuf {
    PathBuf::from(format!("{}.receipt.json", output.display()))
}

fn canonical_set_sha256(rows: &[serde_json::Value]) -> String {
    use sha2::{Digest, Sha256};

    fn encode(value: &serde_json::Value, out: &mut String) {
        match value {
            serde_json::Value::Null => out.push_str("null"),
            serde_json::Value::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
            serde_json::Value::Number(value) => out.push_str(&value.to_string()),
            serde_json::Value::String(value) => {
                out.push_str(&serde_json::to_string(value).unwrap())
            }
            serde_json::Value::Array(values) => {
                out.push('[');
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    encode(value, out);
                }
                out.push(']');
            }
            serde_json::Value::Object(values) => {
                out.push('{');
                let mut keys: Vec<_> = values.keys().collect();
                keys.sort_unstable();
                for (index, key) in keys.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(key).unwrap());
                    out.push(':');
                    encode(&values[*key], out);
                }
                out.push('}');
            }
        }
    }

    let mut encoded = String::new();
    encode(&serde_json::Value::Array(rows.to_vec()), &mut encoded);
    format!("{:x}", Sha256::digest(encoded.as_bytes()))
}

fn task_manifest_for(
    before_path: &Path,
    after_path: &Path,
    status: TaskManifestStatus,
) -> TaskManifest {
    let authority = PathAuthority::explicit().unwrap();
    let before = authority
        .read_source(before_path, "target_input", None)
        .unwrap();
    let after = authority
        .read_source(after_path, "target_output", None)
        .unwrap();
    let report = stemma::api::audit(before.bytes(), after.bytes()).unwrap();
    let baseline_validation = stemma::api::validate(before.bytes());
    let new_validator_issues = report
        .validator
        .issues
        .iter()
        .filter(|issue| !baseline_validation.issues.contains(issue))
        .count() as u64;
    let changed_prior_revisions = report
        .preexisting_revisions
        .iter()
        .filter(|prior| !matches!(prior.disposition, RevisionDisposition::Untouched))
        .count() as u64;
    let unexplained_direct_changes = report
        .direct_changes
        .iter()
        .filter(|change| {
            !matches!(&change.story, StoryScope::Comment { .. })
                && !(change.kind == DirectChangeKind::BlockModified
                    && change.old_excerpt == change.new_excerpt)
        })
        .count() as u64;
    let counts = TaskAuditCounts {
        new_revisions: report.new_revisions.len() as u64,
        direct_changes: report.direct_changes.len() as u64,
        unexplained_direct_changes,
        preexisting_revisions: report.preexisting_revisions.len() as u64,
        changed_prior_revisions,
        expected_changed_prior_revisions: 0,
        unexpected_changed_prior_revisions: changed_prior_revisions,
        untouched_violations: report.untouched.violations.len() as u64,
        validator_issues: report.validator.issues.len() as u64,
        new_validator_issues,
    };
    let blocking_finding_count = counts.unexplained_direct_changes
        + counts.unexpected_changed_prior_revisions
        + counts.untouched_violations
        + counts.new_validator_issues;
    assert_eq!(blocking_finding_count, 0, "test delivery must be clean");
    let verdict = TaskAuditVerdict {
        status: TaskAuditStatus::Pass,
        deliverable: true,
        blocking_finding_count,
    };
    let decision = serde_json::json!({"counts": counts, "verdict": verdict});
    let (effect_status, minted_revision_ids, reason) = match status {
        TaskManifestStatus::Complete => (
            TaskEffectStatus::Satisfied,
            report
                .new_revisions
                .iter()
                .map(|revision| revision.revision_id)
                .collect(),
            None,
        ),
        TaskManifestStatus::Partial => (
            TaskEffectStatus::Unsatisfied,
            Vec::new(),
            Some("declared effect was not executed".to_string()),
        ),
    };
    TaskManifest {
        schema: TASK_MANIFEST_SCHEMA_V1.to_string(),
        task_id: "offline-verification-test".to_string(),
        status,
        producer: TaskManifestProducer {
            name: "stemma-mcp".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            build: env!("CARGO_PKG_VERSION").to_string(),
        },
        inputs: vec![],
        targets: vec![TaskManifestTarget {
            path: before_path.file_name().unwrap().into(),
            input: ManifestIdentity::from_identity(before.identity()).unwrap(),
            doc_id: Some("doc-offline".to_string()),
            output: Some(
                ManifestArtifact::from_identity(after_path.file_name().unwrap(), after.identity())
                    .unwrap(),
            ),
            audit_binding: Some(TaskAuditBinding {
                doc_id: "doc-offline".to_string(),
                scope: TaskAuditScope::DeclaredTaskToSavedOutput,
                output_sha256: after.identity().digest.hex.clone(),
                set_sha256: canonical_set_sha256(std::slice::from_ref(&decision)),
                counts,
                verdict,
            }),
            effects: vec![TaskManifestEffect {
                declaration: DeclaredTaskEffect {
                    effect_id: "e1".to_string(),
                    op: TaskEffectOperation::ReplaceText,
                    find: "foo bar".to_string(),
                    replace: "review-ready language".to_string(),
                    match_mode: TaskMatchMode::Exact,
                    scope: TaskReplacementScope::BodyAndTables,
                    expected_matches: 1,
                    on_barrier_match: TaskBarrierPolicy::Skip,
                },
                status: effect_status,
                minted_revision_ids,
                reason,
            }],
        }],
    }
}

fn dangling_hyperlink_docx() -> Vec<u8> {
    use std::io::Write as _;
    use zip::write::FileOptions;

    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let document_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
    let document = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body><w:p><w:hyperlink r:id="rId99"><w:r><w:t>dangling link</w:t></w:r></w:hyperlink></w:p><w:p><w:r><w:t>editable text</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;

    let mut bytes = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
        let options: FileOptions = FileOptions::default();
        for (name, contents) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/_rels/document.xml.rels", document_rels),
            ("word/document.xml", document),
        ] {
            zip.start_file(name, options).unwrap();
            zip.write_all(contents.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    bytes
}

#[test]
fn apply_turns_an_approved_worklist_into_a_verified_redline() {
    let dir = tempfile::tempdir().unwrap();
    let worklist = dir.path().join("worklist.json");
    let out = dir.path().join("redline.docx");
    write_worklist(
        &worklist,
        "Approved Reviewer",
        serde_json::json!([{
            "id": "change-1",
            "old": "foo bar",
            "new": "review-ready language",
            "expected_matches": 1
        }]),
    );

    let input = fixture("simple-text/before.docx");
    let output = run(&[
        "apply",
        input.to_str().unwrap(),
        "--worklist",
        worklist.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "apply should succeed: {output:?}");
    assert!(out.exists(), "apply creates the redline");
    assert!(receipt_for(&out).exists(), "apply persists its receipt");

    let receipt: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout is the apply receipt");
    let durable_receipt: serde_json::Value =
        serde_json::from_slice(&std::fs::read(receipt_for(&out)).unwrap()).unwrap();
    assert_eq!(receipt, durable_receipt);
    assert_eq!(receipt["schema"], "stemma.apply_receipt.v0");
    assert_eq!(receipt["status"], "complete");
    assert_eq!(receipt["summary"]["total"], 1);
    assert_eq!(receipt["summary"]["applied"], 1);
    assert_eq!(receipt["summary"]["refused"], 0);
    assert_eq!(receipt["items"][0]["id"], "change-1");
    assert_eq!(receipt["items"][0]["status"], "applied");
    assert_eq!(receipt["items"][0]["actual_matches"], 1);
    assert_eq!(receipt["items"][0]["created_revisions"], 2);
    assert_eq!(
        receipt["items"][0]["scope"]["kind"],
        "all_top_level_body_paragraphs"
    );
    assert_eq!(receipt["items"][0]["match_mode"], "exact");
    assert_eq!(receipt["verification"]["validator_ok"], true);
    assert_eq!(receipt["verification"]["direct_changes"], 0);
    assert_eq!(receipt["verification"]["untouched_violations"], 0);
    assert_eq!(
        receipt["verification"]["artifact_stage"],
        "serialized_output"
    );
    assert_eq!(
        receipt["verification"]["output_sha256"],
        receipt["output"]["digest"]["hex"]
    );
    assert_eq!(
        receipt["coverage"]["supported"],
        serde_json::json!(["top_level_body_paragraphs"])
    );
    assert_eq!(
        receipt["coverage"]["conditional_detection"],
        serde_json::json!(["top_level_table_cells_for_default_scope"])
    );
    assert_eq!(
        receipt["coverage"]["unsearched"].as_array().unwrap().len(),
        7
    );
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("revision_id"),
        "durable apply receipts must not publish session-local revision ids"
    );
    assert_eq!(receipt["producer"]["build"], env!("CARGO_PKG_VERSION"));
    let executable = PathAuthority::explicit()
        .unwrap()
        .read_source(
            Path::new(env!("CARGO_BIN_EXE_stemma")),
            "verification_executable",
            None,
        )
        .unwrap();
    assert_eq!(
        receipt["producer"]["executable"]["digest"]["hex"],
        executable.identity().digest.hex
    );
    assert_eq!(
        receipt["producer"]["executable"]["bytes"],
        executable.identity().bytes
    );
    assert_eq!(
        receipt["output"]["persistence_confirmation"]["required_process_exit"],
        0
    );
    assert_eq!(
        receipt["output"]["persistence_confirmation"]["requires_identity_match"],
        true
    );
    assert_eq!(
        receipt["output"]["digest"]["hex"],
        PathAuthority::explicit()
            .unwrap()
            .read_source(&out, "verification", None)
            .unwrap()
            .identity()
            .digest
            .hex
    );

    let original = text_of(&input);
    let (accepted, rejected) = readings(&out);
    assert!(accepted.contains("review-ready language"));
    assert!(!accepted.contains("foo bar"));
    assert_eq!(
        rejected, original,
        "reject-all restores the exact input text"
    );
}

#[test]
fn apply_executes_multiple_scoped_items_under_one_worklist_author() {
    let dir = tempfile::tempdir().unwrap();
    let worklist = dir.path().join("worklist.json");
    let out = dir.path().join("redline.docx");
    let receipt_path = dir.path().join("execution-receipt.json");
    write_worklist(
        &worklist,
        "Approved Reviewer",
        serde_json::json!([
            {
                "id": "normalized-scoped-change",
                "old": "This is a test",
                "new": "This is an approved test",
                "match_mode": "normalize_ws",
                "scope": {"block_id": "p_1"}
            },
            {
                "id": "second-change-same-author",
                "old": "foo bar",
                "new": "review-ready language"
            }
        ]),
    );

    let output = run(&[
        "apply",
        fixture("simple-text/before.docx").to_str().unwrap(),
        "--worklist",
        worklist.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--receipt",
        receipt_path.to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "both items should apply: {output:?}"
    );
    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let durable_receipt: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&receipt_path).unwrap()).unwrap();
    assert_eq!(receipt, durable_receipt);
    assert!(!receipt_for(&out).exists());
    assert_eq!(receipt["status"], "complete");
    assert_eq!(receipt["summary"]["applied"], 2);
    assert_eq!(receipt["items"][0]["status"], "applied");
    assert_eq!(receipt["items"][1]["status"], "applied");
    assert_eq!(receipt["items"][0]["match_mode"], "normalize_ws");
    assert_eq!(receipt["items"][0]["scope"]["kind"], "block");
    let (accepted, rejected) = readings(&out);
    assert!(accepted.contains("This is an approved test"));
    assert!(accepted.contains("review-ready language"));
    assert_eq!(rejected, text_of(&fixture("simple-text/before.docx")));
}

#[test]
fn apply_defaults_to_no_docx_when_any_item_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let worklist = dir.path().join("worklist.json");
    let out = dir.path().join("partial-redline.docx");
    write_worklist(
        &worklist,
        "Approved Reviewer",
        serde_json::json!([
            {
                "id": "applies",
                "old": "foo bar",
                "new": "updated phrase",
                "expected_matches": 1
            },
            {
                "id": "refuses",
                "old": "text that is not present",
                "new": "must not appear",
                "expected_matches": 1
            }
        ]),
    );

    let output = run(&[
        "apply",
        fixture("simple-text/before.docx").to_str().unwrap(),
        "--worklist",
        worklist.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(3), "partial apply is not green");
    assert!(!out.exists(), "partial output is fail-closed by default");
    assert!(receipt_for(&out).exists());
    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(receipt["status"], "partial");
    assert_eq!(receipt["deliverable"], false);
    assert!(receipt["output"].is_null());
    assert_eq!(receipt["summary"]["applied"], 1);
    assert_eq!(receipt["summary"]["refused"], 1);
    assert_eq!(receipt["items"][0]["status"], "applied");
    assert_eq!(receipt["items"][1]["status"], "refused");
    assert_eq!(receipt["items"][1]["code"], "match_count_mismatch");
    assert_eq!(receipt["items"][1]["actual_matches"], 0);
    assert_eq!(receipt["verification"]["new_revisions"], 2);
}

#[test]
fn apply_receipt_keeps_every_worklist_outcome_inline_above_evidence_caps() {
    const N: usize = 70;
    let dir = tempfile::tempdir().unwrap();
    let worklist = dir.path().join("large-worklist.json");
    let out = dir.path().join("must-not-exist.docx");
    let changes: Vec<serde_json::Value> = (0..N)
        .map(|index| {
            serde_json::json!({
                "id": format!("missing-{index}"),
                "old": format!("phrase that is absent {index}"),
                "new": format!("replacement {index}"),
                "expected_matches": 1,
            })
        })
        .collect();
    write_worklist(
        &worklist,
        "Decision Plane Test",
        serde_json::Value::Array(changes),
    );

    let output = run(&[
        "apply",
        fixture("simple-text/before.docx").to_str().unwrap(),
        "--worklist",
        worklist.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(3));
    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(receipt["summary"]["total"], N);
    assert_eq!(receipt["summary"]["refused"], N);
    assert_eq!(
        receipt["items"].as_array().map(Vec::len),
        Some(N),
        "per-item outcomes are decision data and must never be capped: {receipt}"
    );
    assert!(
        receipt.get("items_evidence").is_none(),
        "decision outcomes are structurally inline, not a cappable evidence set"
    );
}

#[test]
fn apply_emits_a_non_deliverable_partial_docx_only_when_explicitly_requested() {
    let dir = tempfile::tempdir().unwrap();
    let worklist = dir.path().join("worklist.json");
    let out = dir.path().join("partial-redline.docx");
    write_worklist(
        &worklist,
        "Approved Reviewer",
        serde_json::json!([
            {"id": "applies", "old": "foo bar", "new": "updated phrase"},
            {"id": "refuses", "old": "text that is not present", "new": "unused"}
        ]),
    );

    let output = run(&[
        "apply",
        fixture("simple-text/before.docx").to_str().unwrap(),
        "--worklist",
        worklist.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--emit-partial",
    ]);
    assert_eq!(output.status.code(), Some(3));
    assert!(out.exists());
    assert!(receipt_for(&out).exists());
    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(receipt["status"], "partial");
    assert_eq!(receipt["deliverable"], false);
    assert_eq!(receipt["emit_partial_requested"], true);
    assert_eq!(receipt["output"]["role"], "output_partial_redline");
    assert_eq!(
        receipt["output"]["persistence_confirmation"]["required_process_exit"],
        3
    );
    assert!(text_of(&out).contains("updated phrase"));
    assert!(!text_of(&out).contains("unused"));
}

#[test]
fn apply_with_no_applicable_items_creates_no_docx() {
    let dir = tempfile::tempdir().unwrap();
    let worklist = dir.path().join("worklist.json");
    let out = dir.path().join("must-not-exist.docx");
    write_worklist(
        &worklist,
        "Approved Reviewer",
        serde_json::json!([{
            "id": "missing",
            "old": "not in the document",
            "new": "unused",
            "expected_matches": 1
        }]),
    );

    let output = run(&[
        "apply",
        fixture("simple-text/before.docx").to_str().unwrap(),
        "--worklist",
        worklist.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--emit-partial",
    ]);
    assert_eq!(output.status.code(), Some(3));
    assert!(!out.exists());
    assert!(receipt_for(&out).exists());
    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(receipt["status"], "partial");
    assert!(receipt["output"].is_null());
    assert!(receipt["verification"].is_null());
}

#[test]
fn apply_refuses_an_existing_receipt_destination_before_creating_a_docx() {
    let dir = tempfile::tempdir().unwrap();
    let worklist = dir.path().join("worklist.json");
    let out = dir.path().join("must-not-exist.docx");
    let receipt_path = dir.path().join("existing-receipt.json");
    write_worklist(
        &worklist,
        "Approved Reviewer",
        serde_json::json!([{
            "id": "change-1",
            "old": "foo bar",
            "new": "review-ready language"
        }]),
    );
    std::fs::write(&receipt_path, b"keep me").unwrap();

    let output = run(&[
        "apply",
        fixture("simple-text/before.docx").to_str().unwrap(),
        "--worklist",
        worklist.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--receipt",
        receipt_path.to_str().unwrap(),
    ]);

    assert_eq!(output.status.code(), Some(1));
    assert!(!out.exists());
    assert_eq!(std::fs::read(&receipt_path).unwrap(), b"keep me");
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("create-new only"));
}

#[test]
fn apply_refuses_to_use_a_source_as_its_receipt_destination() {
    let dir = tempfile::tempdir().unwrap();
    let worklist = dir.path().join("worklist.json");
    let out = dir.path().join("must-not-exist.docx");
    write_worklist(
        &worklist,
        "Approved Reviewer",
        serde_json::json!([{
            "id": "change-1",
            "old": "foo bar",
            "new": "review-ready language"
        }]),
    );
    let original_worklist = std::fs::read(&worklist).unwrap();

    let output = run(&[
        "apply",
        fixture("simple-text/before.docx").to_str().unwrap(),
        "--worklist",
        worklist.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--receipt",
        worklist.to_str().unwrap(),
    ]);

    assert_eq!(output.status.code(), Some(1));
    assert!(!out.exists());
    assert_eq!(std::fs::read(&worklist).unwrap(), original_worklist);
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("protected source"));
}

#[test]
fn apply_refuses_receipt_and_docx_paths_that_normalize_to_one_destination() {
    let dir = tempfile::tempdir().unwrap();
    let alias_parent = dir.path().join("alias-parent");
    std::fs::create_dir(&alias_parent).unwrap();
    let worklist = dir.path().join("worklist.json");
    let out = dir.path().join("must-not-exist.docx");
    let receipt_alias = alias_parent.join("../must-not-exist.docx");
    write_worklist(
        &worklist,
        "Approved Reviewer",
        serde_json::json!([{
            "id": "change-1",
            "old": "foo bar",
            "new": "review-ready language"
        }]),
    );

    let output = run(&[
        "apply",
        fixture("simple-text/before.docx").to_str().unwrap(),
        "--worklist",
        worklist.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--receipt",
        receipt_alias.to_str().unwrap(),
    ]);

    assert_eq!(output.status.code(), Some(1));
    assert!(!out.exists());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("same destination"));
}

#[test]
fn apply_preserves_preexisting_revisions() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("existing-redline.docx");
    let worklist = dir.path().join("worklist.json");
    let out = dir.path().join("layered-redline.docx");
    write_named_redline(&input, "Prior Counsel", "Counsel's tracked replacement.");
    write_worklist_for(
        &worklist,
        &input,
        "Approved Reviewer",
        serde_json::json!([{
            "id": "change-other-paragraph",
            "old": "much longer sequence",
            "new": "substantially revised sequence",
            "expected_matches": 1
        }]),
    );

    let output = run(&[
        "apply",
        input.to_str().unwrap(),
        "--worklist",
        worklist.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "layered apply succeeds: {output:?}"
    );
    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        receipt["verification"]["preexisting_revisions_preserved"]
            .as_u64()
            .is_some_and(|count| count > 0),
        "receipt proves prior revisions survived"
    );

    let extracted = run(&["extract", out.to_str().unwrap(), "--format", "json"]);
    let value: serde_json::Value = serde_json::from_slice(&extracted.stdout).unwrap();
    let revisions = value["revisions"].as_array().unwrap();
    assert!(
        revisions
            .iter()
            .any(|revision| revision["author"] == "Prior Counsel")
    );
    assert!(
        revisions
            .iter()
            .any(|revision| revision["author"] == "Approved Reviewer")
    );

    let verify = run(&["verify", input.to_str().unwrap(), out.to_str().unwrap()]);
    assert!(
        verify.status.success(),
        "delivery verification must agree with the successful apply receipt: {verify:?}"
    );
    let verification: serde_json::Value =
        serde_json::from_slice(&verify.stdout).expect("verification JSON");
    assert_eq!(
        verification["summary"]["modified_or_resolved_preexisting"],
        0
    );
    assert_eq!(
        verification["summary"]["preexisting_revisions"],
        receipt["verification"]["preexisting_revisions_preserved"]
    );
}

#[test]
fn apply_validates_the_entire_worklist_before_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let worklist = dir.path().join("invalid-worklist.json");
    let out = dir.path().join("must-not-exist.docx");
    write_worklist(
        &worklist,
        "Approved Reviewer",
        serde_json::json!([
            {"id": "duplicate", "old": "foo", "new": "first"},
            {"id": "duplicate", "old": "bar", "new": "second"}
        ]),
    );

    let output = run(&[
        "apply",
        fixture("simple-text/before.docx").to_str().unwrap(),
        "--worklist",
        worklist.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    assert_ne!(
        output.status.code(),
        Some(3),
        "schema failure is not partial execution"
    );
    assert!(!out.exists());
    assert!(
        output.stdout.is_empty(),
        "no execution receipt is fabricated"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("duplicate worklist item id"));
}

#[test]
fn apply_refuses_a_worklist_bound_to_different_input_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let worklist = dir.path().join("wrong-input-worklist.json");
    let out = dir.path().join("must-not-exist.docx");
    write_worklist_for(
        &worklist,
        &fixture("simple-text/after.docx"),
        "Approved Reviewer",
        serde_json::json!([{
            "id": "change-1",
            "old": "foo bar",
            "new": "must not apply"
        }]),
    );

    let output = run(&[
        "apply",
        fixture("simple-text/before.docx").to_str().unwrap(),
        "--worklist",
        worklist.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(!out.exists());
    assert!(!receipt_for(&out).exists());
    assert!(String::from_utf8_lossy(&output.stderr).contains("input binding mismatch"));
}

#[test]
fn apply_rejects_invalid_input_binding_shapes_before_execution() {
    let directory = tempfile::tempdir().unwrap();
    let input = PathAuthority::explicit()
        .unwrap()
        .read_source(fixture("simple-text/before.docx"), "test_input", None)
        .unwrap();
    let changes = serde_json::json!([{
        "id": "change-1",
        "old": "foo bar",
        "new": "must not apply"
    }]);
    let invalid = [
        (
            "missing-input",
            serde_json::json!({
                "schema": "stemma.worklist.v0",
                "author": "Approved Reviewer",
                "changes": changes.clone(),
            }),
        ),
        (
            "uppercase-sha",
            serde_json::json!({
                "schema": "stemma.worklist.v0",
                "input": {
                    "sha256": input.identity().digest.hex.to_uppercase(),
                    "bytes": input.identity().bytes,
                },
                "author": "Approved Reviewer",
                "changes": changes.clone(),
            }),
        ),
        (
            "non-integer-bytes",
            serde_json::json!({
                "schema": "stemma.worklist.v0",
                "input": {
                    "sha256": input.identity().digest.hex.clone(),
                    "bytes": "11431",
                },
                "author": "Approved Reviewer",
                "changes": changes,
            }),
        ),
    ];

    for (case, worklist) in invalid {
        let worklist_path = directory.path().join(format!("{case}.json"));
        let output_path = directory.path().join(format!("{case}.docx"));
        std::fs::write(
            &worklist_path,
            serde_json::to_vec_pretty(&worklist).unwrap(),
        )
        .unwrap();
        let output = run(&[
            "apply",
            fixture("simple-text/before.docx").to_str().unwrap(),
            "--worklist",
            worklist_path.to_str().unwrap(),
            "-o",
            output_path.to_str().unwrap(),
        ]);
        assert_eq!(output.status.code(), Some(1), "case {case}: {output:?}");
        assert!(output.stdout.is_empty(), "case {case}");
        assert!(!output_path.exists(), "case {case}");
        assert!(!receipt_for(&output_path).exists(), "case {case}");
    }
}

#[test]
fn apply_refuses_to_write_the_redline_over_its_docx_input() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("protected-input.docx");
    let worklist = directory.path().join("worklist.json");
    std::fs::copy(fixture("simple-text/before.docx"), &input).unwrap();
    write_worklist_for(
        &worklist,
        &input,
        "Approved Reviewer",
        serde_json::json!([{
            "id": "change-1",
            "old": "foo bar",
            "new": "must not apply"
        }]),
    );
    let original = std::fs::read(&input).unwrap();

    let output = run(&[
        "apply",
        input.to_str().unwrap(),
        "--worklist",
        worklist.to_str().unwrap(),
        "-o",
        input.to_str().unwrap(),
    ]);

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(std::fs::read(&input).unwrap(), original);
    assert!(!receipt_for(&input).exists());
}

#[test]
fn apply_refuses_a_detected_table_cell_match_instead_of_claiming_completion() {
    let dir = tempfile::tempdir().unwrap();
    let worklist = dir.path().join("worklist.json");
    let out = dir.path().join("must-not-exist.docx");
    write_worklist_for(
        &worklist,
        &fixture("table-changes/before.docx"),
        "Approved Reviewer",
        serde_json::json!([{
            "id": "table-change",
            "old": "In the second row.",
            "new": "Revised table language.",
            "expected_matches": 1
        }]),
    );

    let output = run(&[
        "apply",
        fixture("table-changes/before.docx").to_str().unwrap(),
        "--worklist",
        worklist.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(3));
    assert!(!out.exists());
    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(receipt["items"][0]["status"], "refused");
    assert_eq!(receipt["items"][0]["code"], "unreachable_match");
    assert_eq!(
        receipt["items"][0]["unreachable_matches"][0]["region"],
        "table_cell"
    );
}

#[test]
fn apply_refuses_to_impersonate_an_existing_revision_author() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("existing-redline.docx");
    let worklist = dir.path().join("worklist.json");
    let out = dir.path().join("must-not-exist.docx");
    write_named_redline(&input, "Prior Counsel", "Counsel's tracked replacement.");
    write_worklist_for(
        &worklist,
        &input,
        "Prior Counsel",
        serde_json::json!([{
            "id": "impersonating-change",
            "old": "much longer sequence",
            "new": "different sequence",
            "expected_matches": 1
        }]),
    );

    let output = run(&[
        "apply",
        input.to_str().unwrap(),
        "--worklist",
        worklist.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(3));
    assert!(!out.exists());
    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(receipt["items"][0]["status"], "refused");
    assert!(
        receipt["items"][0]["message"]
            .as_str()
            .is_some_and(|message| message.contains("already authors revisions")),
        "receipt explains the author collision: {receipt}"
    );
}

#[test]
fn apply_protects_the_worklist_from_output_aliasing() {
    let dir = tempfile::tempdir().unwrap();
    let worklist = dir.path().join("worklist.json");
    write_worklist(
        &worklist,
        "Approved Reviewer",
        serde_json::json!([{
            "id": "change-1",
            "old": "foo bar",
            "new": "updated phrase",
            "expected_matches": 1
        }]),
    );
    let original_worklist = std::fs::read(&worklist).unwrap();

    let output = run(&[
        "apply",
        fixture("simple-text/before.docx").to_str().unwrap(),
        "--worklist",
        worklist.to_str().unwrap(),
        "-o",
        worklist.to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    assert_ne!(output.status.code(), Some(3));
    assert!(
        output.stdout.is_empty(),
        "no receipt claims a committed output"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("existing output"));
    assert_eq!(std::fs::read(&worklist).unwrap(), original_worklist);
}

#[test]
fn apply_keeps_the_durable_receipt_authoritative_when_stdout_is_closed() {
    let dir = tempfile::tempdir().unwrap();
    let worklist = dir.path().join("worklist.json");
    let out = dir.path().join("redline.docx");
    write_worklist(
        &worklist,
        "Approved Reviewer",
        serde_json::json!([{
            "id": "change-1",
            "old": "foo bar",
            "new": "review-ready language"
        }]),
    );

    let mut child = Command::new(env!("CARGO_BIN_EXE_stemma"))
        .args([
            "apply",
            fixture("simple-text/before.docx").to_str().unwrap(),
            "--worklist",
            worklist.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn stemma binary");
    drop(child.stdout.take());
    let output = child.wait_with_output().expect("wait for stemma binary");

    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("cannot mirror apply receipt to stdout"),
        "stderr points to the durable receipt: {output:?}"
    );
    assert!(out.exists());
    let receipt: serde_json::Value =
        serde_json::from_slice(&std::fs::read(receipt_for(&out)).unwrap()).unwrap();
    assert_eq!(receipt["status"], "complete");
    assert_eq!(receipt["deliverable"], true);
    let (accepted, rejected) = readings(&out);
    assert!(accepted.contains("review-ready language"));
    assert_eq!(rejected, text_of(&fixture("simple-text/before.docx")));
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
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    let identity = PathAuthority::explicit()
        .unwrap()
        .read_source(&out, "verification", None)
        .unwrap();
    assert!(
        stderr.contains("sha256=")
            && stderr.contains("collision_policy=create_new")
            && stderr.contains("disposition=created")
            && stderr.contains(&identity.identity().digest.hex),
        "write diagnostic carries exact identity and policy: {stderr}"
    );

    // The round-trip contract: reject-all reconstructs the base, accept-all the
    // target. This proves the redline carries the discovered changes.
    let base = text_of(&fixture("simple-text/before.docx"));
    let target = text_of(&fixture("simple-text/after.docx"));
    let (accepted, rejected) = readings(&out);
    assert_eq!(rejected, base, "reject-all == base");
    assert_eq!(accepted, target, "accept-all == target");
}

#[test]
fn compare_accepts_relative_inputs_and_output() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(
        fixture("simple-text/before.docx"),
        dir.path().join("before.docx"),
    )
    .expect("copy base");
    std::fs::copy(
        fixture("simple-text/after.docx"),
        dir.path().join("after.docx"),
    )
    .expect("copy target");

    let output = run_in(
        dir.path(),
        &["compare", "before.docx", "after.docx", "-o", "redline.docx"],
    );
    assert!(
        output.status.success(),
        "relative CLI paths should resolve from the invocation directory: {output:?}"
    );
    let redline = dir.path().join("redline.docx");
    assert!(redline.exists());
    let (accepted, rejected) = readings(&redline);
    assert_eq!(
        accepted,
        text_of(&dir.path().join("after.docx")),
        "relative target remains the accept-all projection"
    );
    assert_eq!(
        rejected,
        text_of(&dir.path().join("before.docx")),
        "relative base remains the reject-all projection"
    );
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
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    let identity = PathAuthority::explicit()
        .unwrap()
        .read_source(&accepted, "verification", None)
        .unwrap();
    assert!(
        stderr.contains(&format!("sha256={}", identity.identity().digest.hex))
            && stderr.contains(&format!("bytes={}", identity.identity().bytes))
            && stderr.contains("collision_policy=create_new")
            && stderr.contains("disposition=created"),
        "resolve diagnostic carries the exact committed identity and policy: {stderr}"
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
    assert!(stdout.contains("bytes=11431"));
    assert!(
        stdout.contains("sha256=2cdfb8ecd1a27ef7132ebbaa1f718d6705ea6532bf3b155c09bfd7e87d410667")
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
fn blocking_validation_failure_creates_no_output() {
    let dir = tempfile::tempdir().unwrap();
    let invalid = dir.path().join("dangling-link.docx");
    let out = dir.path().join("must-not-exist.docx");
    let bytes = dangling_hyperlink_docx();
    Document::parse(&bytes).expect("fixture imports so compare reaches the output gate");
    assert!(
        !stemma::api::validate(&bytes).ok,
        "fixture must carry a blocking dangling-relationship defect"
    );
    std::fs::write(&invalid, bytes).expect("write invalid fixture");

    let output = run(&[
        "compare",
        invalid.to_str().unwrap(),
        invalid.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert!(
        !output.status.success(),
        "the blocking validator must refuse the output"
    );
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(
        stderr.contains("I-REL-001") || stderr.contains("ValidationFailed"),
        "failure identifies the blocking validation gate: {stderr}"
    );
    assert!(
        !out.exists(),
        "validation happens before create-new persistence"
    );
}

#[test]
fn apply_blocking_validation_failure_creates_no_docx() {
    let dir = tempfile::tempdir().unwrap();
    let invalid = dir.path().join("dangling-link.docx");
    let worklist = dir.path().join("worklist.json");
    let out = dir.path().join("must-not-exist.docx");
    let receipt = receipt_for(&out);
    let bytes = dangling_hyperlink_docx();
    Document::parse(&bytes).expect("fixture imports so apply reaches delivery verification");
    assert!(
        !stemma::api::validate(&bytes).ok,
        "fixture must carry a blocking dangling-relationship defect"
    );
    std::fs::write(&invalid, bytes).expect("write invalid fixture");
    write_worklist_for(
        &worklist,
        &invalid,
        "Approved Reviewer",
        serde_json::json!([{
            "id": "change-1",
            "old": "editable text",
            "new": "still tracked but structurally invalid",
            "expected_matches": 1
        }]),
    );

    let output = run(&[
        "apply",
        invalid.to_str().unwrap(),
        "--worklist",
        worklist.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert!(
        !output.status.success(),
        "delivery verification must refuse a blocking validator failure"
    );
    let result: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout is the refusal receipt");
    assert_eq!(result["status"], "partial");
    assert_eq!(result["deliverable"], false);
    assert_eq!(result["items"][0]["code"], "validation_failed");
    assert!(
        result["items"][0]["message"]
            .as_str()
            .is_some_and(|message| message.contains("I-REL-001")),
        "receipt identifies the blocking validation finding: {result}"
    );
    assert!(!out.exists(), "validation happens before DOCX commit");
    assert!(
        receipt.exists(),
        "the non-deliverable refusal remains observable in its durable receipt"
    );
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

    let original = std::fs::read(&redline).expect("read input before refusal");
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
        stderr.contains("aliases protected source"),
        "the refusal is explicit: {stderr:?}"
    );
    assert_eq!(
        std::fs::read(&redline).expect("input survives refusal"),
        original
    );
}

#[test]
fn refuses_to_replace_an_existing_unrelated_output() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("existing.docx");
    let sentinel = b"pre-existing destination";
    std::fs::write(&out, sentinel).expect("write sentinel");

    let output = run(&[
        "compare",
        fixture("simple-text/before.docx").to_str().unwrap(),
        fixture("simple-text/after.docx").to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert!(!output.status.success(), "existing output must be refused");
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(
        stderr.contains("refusing to replace existing output")
            && stderr.contains("create-new only"),
        "collision error names the enforced policy: {stderr}"
    );
    assert_eq!(
        std::fs::read(&out).expect("destination survives refusal"),
        sentinel
    );
}

#[test]
fn resolve_also_refuses_an_existing_output() {
    let dir = tempfile::tempdir().unwrap();
    let redline = dir.path().join("redline.docx");
    let out = dir.path().join("resolved.docx");
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
    let sentinel = b"keep this destination";
    std::fs::write(&out, sentinel).expect("write sentinel");

    let output = run(&[
        "resolve",
        redline.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--accept-all",
    ]);
    assert!(!output.status.success());
    assert_eq!(std::fs::read(&out).unwrap(), sentinel);
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

#[test]
fn inspect_is_a_compact_identity_bound_revision_aware_projection() {
    let input = fixture("simple-text/before.docx");
    let output = run(&["inspect", input.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "inspect should succeed: {output:?}"
    );
    let text = String::from_utf8(output.stdout).expect("utf8 inspection");
    let identity = PathAuthority::explicit()
        .unwrap()
        .read_source(&input, "verification", None)
        .unwrap();
    assert!(text.starts_with("@stemma inspect.v0 "));
    assert!(text.contains(&format!("sha256={}", identity.identity().digest.hex)));
    assert!(text.contains(&format!("bytes={}", identity.identity().bytes)));
    assert!(
        text.contains("\n\n#p_"),
        "projection carries addressable blocks: {text}"
    );
    assert!(
        text.contains("foo bar"),
        "projection carries readable content: {text}"
    );

    let json_output = run(&["inspect", input.to_str().unwrap(), "--format", "json"]);
    assert!(json_output.status.success());
    let payload: serde_json::Value =
        serde_json::from_slice(&json_output.stdout).expect("inspection JSON");
    assert_eq!(payload["schema"], "stemma.inspect.v0");
    assert_eq!(payload["input"]["sha256"], identity.identity().digest.hex);
    assert!(payload["projection"].as_str().unwrap().contains("foo bar"));
}

#[test]
fn inspect_projection_is_smaller_than_the_structured_read_on_a_multi_block_doc() {
    let input = fixture("twenty-paragraphs/before.docx");
    let compact = run(&["inspect", input.to_str().unwrap()]);
    let structured = run(&["extract", input.to_str().unwrap(), "--format", "json"]);
    assert!(compact.status.success() && structured.status.success());
    assert!(
        compact.stdout.len() < structured.stdout.len(),
        "compact projection should reduce complete-read bytes: inspect={} extract_json={}",
        compact.stdout.len(),
        structured.stdout.len()
    );
}

#[test]
fn execute_alias_and_verify_form_one_tracked_delivery_flow() {
    let dir = tempfile::tempdir().unwrap();
    let input = fixture("simple-text/before.docx");
    let worklist = dir.path().join("plan.json");
    let redline = dir.path().join("redline.docx");
    write_worklist(
        &worklist,
        "Compact Front End",
        serde_json::json!([{
            "id": "change-1",
            "old": "foo bar",
            "new": "review-ready language",
            "expected_matches": 1
        }]),
    );

    let execute = run(&[
        "execute",
        input.to_str().unwrap(),
        "--plan",
        worklist.to_str().unwrap(),
        "-o",
        redline.to_str().unwrap(),
    ]);
    assert!(
        execute.status.success(),
        "execute alias should succeed: {execute:?}"
    );

    let verify = run(&["verify", input.to_str().unwrap(), redline.to_str().unwrap()]);
    assert!(
        verify.status.success(),
        "tracked delivery should verify: {verify:?}"
    );
    let receipt: serde_json::Value =
        serde_json::from_slice(&verify.stdout).expect("verification JSON");
    assert_eq!(receipt["schema"], "stemma.verify.v0");
    assert_eq!(receipt["policy"], "tracked-delivery-v0");
    assert_eq!(receipt["status"], "pass");
    assert_eq!(receipt["summary"]["direct_changes"], 0);
    assert_eq!(receipt["summary"]["modified_or_resolved_preexisting"], 0);
    assert_eq!(receipt["summary"]["validator_ok"], true);
    assert!(receipt["summary"]["new_revisions"].as_u64().unwrap() > 0);
    assert!(
        receipt["projections"]["accepted"]["sha256"]
            .as_str()
            .unwrap()
            .len()
            == 64
    );
    assert!(
        receipt["projections"]["rejected"]["sha256"]
            .as_str()
            .unwrap()
            .len()
            == 64
    );
}

#[test]
fn verify_refuses_to_certify_an_untracked_replacement() {
    let output = run(&[
        "verify",
        fixture("simple-text/before.docx").to_str().unwrap(),
        fixture("simple-text/after.docx").to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(3));
    let receipt: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("failure remains structured JSON");
    assert_eq!(receipt["status"], "fail");
    assert!(receipt["summary"]["direct_changes"].as_u64().unwrap() > 0);
}

// ---------------------------------------------------------------------------
// formatting-change revisions on the CLI surface
//
// Domain rule: a `w:rPrChange` (§17.13.5.31) is a pending tracked change —
// Word counts it, accept keeps the new run properties, reject restores the
// embedded previous ones. The CLI's read and resolve surfaces must agree
// with the engine's single revision census; a narrower re-derivation that
// only sees insert/delete silently hides formatting-only revisions and
// breaks author-scoped resolution ("resolve everything by X" must never
// leave X's formatting changes pending behind a success claim).
// ---------------------------------------------------------------------------

/// The document.xml bytes of a docx on disk.
fn document_xml(path: &Path) -> String {
    let bytes = std::fs::read(path).expect("read docx");
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("zip");
    let mut part = zip.by_name("word/document.xml").expect("main part");
    let mut out = String::new();
    std::io::Read::read_to_string(&mut part, &mut out).expect("utf8 xml");
    out
}

#[test]
fn inspect_counts_a_formatting_only_revision_as_pending() {
    let input = fixture("spec-compliance/tracked-changes/rpr-change/input.docx");
    let output = run(&["inspect", input.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "inspect should succeed: {output:?}"
    );
    let text = String::from_utf8(output.stdout).expect("utf8 inspection");
    assert!(
        text.contains("pending_revisions=1"),
        "a sole w:rPrChange is one pending revision, not zero: {text}"
    );

    let json = run(&["extract", input.to_str().unwrap(), "--format", "json"]);
    assert!(json.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&json.stdout).expect("json");
    let revisions = payload["revisions"].as_array().expect("revisions array");
    assert_eq!(
        revisions.len(),
        1,
        "structured read must disclose the formatting change: {payload}"
    );
    assert_eq!(revisions[0]["kind"], "format_run");
    assert_eq!(revisions[0]["author"], "Spec Test");
}

#[test]
fn resolve_accepts_and_rejects_a_formatting_only_document() {
    let input = fixture("spec-compliance/tracked-changes/rpr-change/input.docx");
    let dir = tempfile::tempdir().unwrap();

    let accepted = dir.path().join("accepted.docx");
    let output = run(&[
        "resolve",
        input.to_str().unwrap(),
        "--accept-all",
        "-o",
        accepted.to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "accept-all must resolve a formatting-only document: {output:?}"
    );
    assert!(
        !document_xml(&accepted).contains("<w:rPrChange"),
        "accept drops the change history and keeps the new formatting"
    );

    let rejected = dir.path().join("rejected.docx");
    let output = run(&[
        "resolve",
        input.to_str().unwrap(),
        "--reject-all",
        "-o",
        rejected.to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "reject-all must resolve a formatting-only document: {output:?}"
    );
    assert!(
        !document_xml(&rejected).contains("<w:rPrChange"),
        "reject restores the previous run properties and drops the marker"
    );
}

#[test]
fn resolve_by_author_never_leaves_that_authors_formatting_changes_pending() {
    // ins + del + rPrChange, all by "Spec Test". Author-scoped resolution
    // must cover all three: a success claim with the rPrChange still
    // pending is a silent item omission.
    let input = fixture("spec-compliance/tracked-changes/preexisting-ins-del/input.docx");
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("author-accepted.docx");
    let output = run(&[
        "resolve",
        input.to_str().unwrap(),
        "--accept-author",
        "Spec Test",
        "-o",
        out.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "author resolve: {output:?}");
    let xml = document_xml(&out);
    assert!(
        !xml.contains("<w:rPrChange") && !xml.contains("<w:ins ") && !xml.contains("<w:del "),
        "no pending change by the resolved author may remain: {xml}"
    );
}

/// Nested same-name smart tags (Word's own place > PlaceName emission) must
/// survive an apply-path rebuild with nesting intact — pairing wrapper
/// markers by element name alone flattens them and strands an empty close
/// marker in the output (wild witness: a school-letterhead paragraph,
/// untouched by the worklist, rebuilt flat).
#[test]
fn apply_preserves_nested_same_name_smart_tags_in_untouched_paragraphs() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("nested.docx");
    let doc_xml = concat!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>"#,
        r#"<w:p><w:r><w:t>Opening paragraph to edit.</w:t></w:r></w:p>"#,
        r#"<w:p><w:smartTag w:uri="urn:schemas-microsoft-com:office:smarttags" w:element="place">"#,
        r#"<w:smartTag w:uri="urn:schemas-microsoft-com:office:smarttags" w:element="PlaceName">"#,
        r#"<w:r><w:t>Bolivar-Richburg</w:t></w:r></w:smartTag>"#,
        r#"<w:r><w:t xml:space="preserve"> </w:t></w:r>"#,
        r#"<w:smartTag w:uri="urn:schemas-microsoft-com:office:smarttags" w:element="PlaceType">"#,
        r#"<w:r><w:t>School</w:t></w:r></w:smartTag>"#,
        r#"</w:smartTag></w:p>"#,
        r#"<w:sectPr/></w:body></w:document>"#,
    );
    {
        use std::io::Write;
        use zip::write::FileOptions;
        let file = std::fs::File::create(&input).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts: FileOptions = FileOptions::default();
        zip.start_file("[Content_Types].xml", opts).unwrap();
        zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#).unwrap();
        zip.start_file("_rels/.rels", opts).unwrap();
        zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#).unwrap();
        zip.start_file("word/_rels/document.xml.rels", opts)
            .unwrap();
        zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#).unwrap();
        zip.start_file("word/document.xml", opts).unwrap();
        zip.write_all(doc_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }

    let worklist = dir.path().join("worklist.json");
    write_worklist_for(
        &worklist,
        &input,
        "Approved Reviewer",
        serde_json::json!([{
            "id": "c1",
            "old": "Opening",
            "new": "Edited",
            "expected_matches": 1
        }]),
    );
    let out = dir.path().join("out.docx");
    let output = run(&[
        "apply",
        input.to_str().unwrap(),
        "--worklist",
        worklist.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "apply: {output:?}");

    let xml = document_xml(&out);
    let place = xml.find(r#"w:element="place""#).expect("outer place tag");
    let place_close_span = &xml[place..];
    assert!(
        place_close_span.find(r#"w:element="PlaceName""#).is_some()
            && place_close_span.find(r#"w:element="PlaceType""#).is_some(),
        "inner wrappers must stay inside the outer place tag: {xml}"
    );
    assert!(
        !xml.contains("stemmaWrapperClose"),
        "transient polarity attribute must never reach output: {xml}"
    );
    assert!(
        !xml.contains(
            r#"<w:smartTag w:uri="urn:schemas-microsoft-com:office:smarttags" w:element="place" />"#
        ),
        "no stranded empty close marker: {xml}"
    );
    // Structural check: the outer place element CONTAINS both inner tags.
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(std::fs::read(&out).unwrap())).unwrap();
    let mut raw = String::new();
    std::io::Read::read_to_string(&mut zip.by_name("word/document.xml").unwrap(), &mut raw)
        .unwrap();
    let root = xmltree::Element::parse(std::io::Cursor::new(raw.as_bytes())).expect("xml");
    let body = root.get_child("body").expect("body");
    let mut found = false;
    for child in &body.children {
        let xmltree::XMLNode::Element(p) = child else {
            continue;
        };
        for pc in &p.children {
            let xmltree::XMLNode::Element(tag) = pc else {
                continue;
            };
            if tag.name != "smartTag" {
                continue;
            }
            let inner: Vec<String> = tag
                .children
                .iter()
                .filter_map(|c| match c {
                    xmltree::XMLNode::Element(e) if e.name == "smartTag" => e
                        .attributes
                        .iter()
                        .find(|(k, _)| k.local_name == "element")
                        .map(|(_, v)| v.clone()),
                    _ => None,
                })
                .collect();
            if inner == vec!["PlaceName".to_string(), "PlaceType".to_string()] {
                found = true;
            }
        }
    }
    assert!(found, "outer place wraps PlaceName then PlaceType: {xml}");
}

/// §17.3.2.40 is tri-state: an EXPLICIT `w:u w:val="none"` on a
/// hyperlink-field run overrides the Hyperlink character style's inherited
/// underline. The worklist apply path rebuilds every paragraph; the field
/// runs' wrapper rPr must re-emit the explicit "none" — dropping it flips
/// the cached field result back to underlined, a visible change on an
/// untouched paragraph (and a false delivery-verification failure).
#[test]
fn apply_keeps_explicit_underline_none_on_untouched_field_runs() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("ulink.docx");
    let field_rpr = r#"<w:rPr><w:rStyle w:val="Hyperlink"/><w:u w:val="none"/></w:rPr>"#;
    let doc_xml = format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>"#,
            r#"<w:p><w:r><w:t>Opening paragraph to edit.</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t xml:space="preserve">See </w:t></w:r>"#,
            r#"<w:r>{rpr}<w:fldChar w:fldCharType="begin"/></w:r>"#,
            r#"<w:r>{rpr}<w:instrText xml:space="preserve"> HYPERLINK "https://example.org" </w:instrText></w:r>"#,
            r#"<w:r>{rpr}<w:fldChar w:fldCharType="separate"/></w:r>"#,
            r#"<w:r>{rpr}<w:t>example.org</w:t></w:r>"#,
            r#"<w:r>{rpr}<w:fldChar w:fldCharType="end"/></w:r></w:p>"#,
            r#"<w:sectPr/></w:body></w:document>"#,
        ),
        rpr = field_rpr
    );
    {
        use std::io::Write;
        use zip::write::FileOptions;
        let file = std::fs::File::create(&input).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts: FileOptions = FileOptions::default();
        zip.start_file("[Content_Types].xml", opts).unwrap();
        zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#).unwrap();
        zip.start_file("_rels/.rels", opts).unwrap();
        zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#).unwrap();
        zip.start_file("word/_rels/document.xml.rels", opts)
            .unwrap();
        zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#).unwrap();
        zip.start_file("word/document.xml", opts).unwrap();
        zip.write_all(doc_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }

    let worklist = dir.path().join("worklist.json");
    write_worklist_for(
        &worklist,
        &input,
        "Approved Reviewer",
        serde_json::json!([{
            "id": "c1",
            "old": "Opening",
            "new": "Edited",
            "expected_matches": 1
        }]),
    );
    let out = dir.path().join("out.docx");
    let output = run(&[
        "apply",
        input.to_str().unwrap(),
        "--worklist",
        worklist.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "apply: {output:?}");
    let xml = document_xml(&out).replace(" />", "/>");
    assert!(
        xml.contains(r#"<w:u w:val="none"/>"#),
        "the explicit underline-off override must survive the rebuild: {xml}"
    );

    // And the delivery verifier must agree the field paragraph is untouched.
    let verify = run(&["verify", input.to_str().unwrap(), out.to_str().unwrap()]);
    assert!(
        verify.status.success(),
        "verify must pass on an honest delivery: {verify:?}"
    );
}

#[test]
fn verify_task_projects_complete_partial_mismatch_and_schema_to_distinct_exit_codes() {
    let dir = tempfile::tempdir().unwrap();
    let before = dir.path().join("before.docx");
    let after = dir.path().join("after.docx");
    let worklist = dir.path().join("worklist.json");
    std::fs::copy(fixture("simple-text/before.docx"), &before).unwrap();
    write_worklist_for(
        &worklist,
        &before,
        "Task Verifier Test",
        serde_json::json!([{
            "id": "e1",
            "old": "foo bar",
            "new": "review-ready language",
            "expected_matches": 1
        }]),
    );
    let apply = run(&[
        "apply",
        before.to_str().unwrap(),
        "--worklist",
        worklist.to_str().unwrap(),
        "-o",
        after.to_str().unwrap(),
    ]);
    assert!(apply.status.success(), "test delivery creation: {apply:?}");

    let manifest_dir = dir.path().join("manifests");
    std::fs::create_dir(&manifest_dir).unwrap();
    let complete_path = manifest_dir.join("complete.json");
    std::fs::write(
        &complete_path,
        encode_task_manifest(&task_manifest_for(
            &before,
            &after,
            TaskManifestStatus::Complete,
        ))
        .unwrap(),
    )
    .unwrap();
    let complete = run(&[
        "verify-task",
        complete_path.to_str().unwrap(),
        "--root",
        dir.path().to_str().unwrap(),
    ]);
    assert_eq!(complete.status.code(), Some(0), "{complete:?}");
    let complete_receipt: serde_json::Value = serde_json::from_slice(&complete.stdout).unwrap();
    assert_eq!(complete_receipt["status"], "complete");
    assert_eq!(complete_receipt["effects_verified"], 1);

    let partial_path = manifest_dir.join("partial.json");
    std::fs::write(
        &partial_path,
        encode_task_manifest(&task_manifest_for(
            &before,
            &after,
            TaskManifestStatus::Partial,
        ))
        .unwrap(),
    )
    .unwrap();
    let partial = run(&[
        "verify-task",
        partial_path.to_str().unwrap(),
        "--root",
        dir.path().to_str().unwrap(),
    ]);
    assert_eq!(partial.status.code(), Some(1), "{partial:?}");
    let partial_receipt: serde_json::Value = serde_json::from_slice(&partial.stdout).unwrap();
    assert_eq!(partial_receipt["status"], "partial");
    assert_eq!(
        partial_receipt["unsatisfied_effects"],
        serde_json::json!(["e1"])
    );

    let original_after = std::fs::read(&after).unwrap();
    std::fs::write(&after, b"altered after manifest creation").unwrap();
    let mismatch = run(&[
        "verify-task",
        complete_path.to_str().unwrap(),
        "--root",
        dir.path().to_str().unwrap(),
    ]);
    assert_eq!(mismatch.status.code(), Some(2), "{mismatch:?}");
    assert!(String::from_utf8_lossy(&mismatch.stderr).contains("does not match manifest identity"));
    std::fs::write(&after, original_after).unwrap();

    let unknown_path = manifest_dir.join("unknown.json");
    let mut unknown: serde_json::Value = serde_json::from_slice(
        &encode_task_manifest(&task_manifest_for(
            &before,
            &after,
            TaskManifestStatus::Complete,
        ))
        .unwrap(),
    )
    .unwrap();
    unknown["schema"] = serde_json::json!("stemma.task_manifest.v99");
    std::fs::write(&unknown_path, serde_json::to_vec(&unknown).unwrap()).unwrap();
    let unknown = run(&[
        "verify-task",
        unknown_path.to_str().unwrap(),
        "--root",
        dir.path().to_str().unwrap(),
    ]);
    assert_eq!(unknown.status.code(), Some(3), "{unknown:?}");
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("unknown task manifest schema"));
}
