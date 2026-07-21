use std::collections::HashSet;
use std::path::Path;
use std::process::ExitCode;

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use stemma::audit::{DirectChangeKind, RevisionDisposition};
use stemma_artifacts::{
    ManifestArtifact, ManifestIdentity, PathAuthority, TaskAuditBinding, TaskAuditCounts,
    TaskAuditStatus, TaskAuditVerdict, TaskEffectStatus, TaskManifest, TaskManifestStatus,
    decode_task_manifest,
};

const VERIFIED_COMPLETE: u8 = 0;
const VERIFIED_PARTIAL: u8 = 1;
const VERIFICATION_FAILED: u8 = 2;
const USAGE_OR_IO: u8 = 3;

/// Verify a task manifest without any session state. This function owns its
/// exit-code projection because a verified partial is neither success nor a
/// contradiction, and both differ from usage/IO failure.
pub(crate) fn verify_task(
    manifest_authority: &PathAuthority,
    manifest_path: &Path,
    root: Option<&Path>,
) -> ExitCode {
    match verify_task_inner(manifest_authority, manifest_path, root) {
        Ok(summary) => {
            let status = summary.status;
            match serde_json::to_string_pretty(&summary) {
                Ok(encoded) => println!("{encoded}"),
                Err(error) => {
                    eprintln!("error: cannot encode task verification result: {error}");
                    return ExitCode::from(USAGE_OR_IO);
                }
            }
            match status {
                TaskManifestStatus::Complete => ExitCode::from(VERIFIED_COMPLETE),
                TaskManifestStatus::Partial => ExitCode::from(VERIFIED_PARTIAL),
            }
        }
        Err(TaskVerificationError::Verification(message)) => {
            eprintln!("error: task verification failed: {message}");
            ExitCode::from(VERIFICATION_FAILED)
        }
        Err(TaskVerificationError::UsageOrIo(message)) => {
            eprintln!("error: cannot verify task: {message}");
            ExitCode::from(USAGE_OR_IO)
        }
    }
}

#[derive(Serialize)]
struct VerificationSummary {
    schema: &'static str,
    task_id: String,
    status: TaskManifestStatus,
    inputs_verified: usize,
    targets_verified: usize,
    outputs_verified: usize,
    effects_verified: usize,
    unsatisfied_effects: Vec<String>,
}

enum TaskVerificationError {
    Verification(String),
    UsageOrIo(String),
}

fn verify_task_inner(
    manifest_authority: &PathAuthority,
    manifest_path: &Path,
    root: Option<&Path>,
) -> Result<VerificationSummary, TaskVerificationError> {
    let manifest_artifact = manifest_authority
        .read_source(manifest_path, "task_manifest", None)
        .map_err(|error| TaskVerificationError::UsageOrIo(error.to_string()))?;
    let manifest = decode_task_manifest(manifest_artifact.bytes())
        .map_err(|error| TaskVerificationError::UsageOrIo(error.to_string()))?;

    let artifact_root = match root {
        Some(root) => root.to_path_buf(),
        None => manifest_artifact
            .identity()
            .resolved_path
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| {
                TaskVerificationError::UsageOrIo(format!(
                    "manifest {} has no parent directory",
                    manifest_path.display()
                ))
            })?,
    };
    let artifacts = PathAuthority::explicit_at(&artifact_root).map_err(|error| {
        TaskVerificationError::UsageOrIo(format!(
            "cannot establish artifact root {}: {error}",
            artifact_root.display()
        ))
    })?;

    for input in &manifest.inputs {
        verify_artifact(&artifacts, &input.artifact, "read-only input")?;
    }

    let mut outputs_verified = 0usize;
    let mut effects_verified = 0usize;
    let mut unsatisfied_effects = Vec::new();
    for target in &manifest.targets {
        let before = verify_identity(&artifacts, &target.path, &target.input, "target input")?;
        match (&target.output, &target.audit_binding) {
            (Some(output), Some(binding)) => {
                let after = verify_artifact(&artifacts, output, "target output")?;
                verify_output_audit(
                    &manifest,
                    target.path.as_path(),
                    before.bytes(),
                    after.bytes(),
                    binding,
                )?;
                let report =
                    stemma::api::audit(before.bytes(), after.bytes()).map_err(|error| {
                        TaskVerificationError::Verification(format!(
                            "cannot audit target {}: {error}",
                            target.path.display()
                        ))
                    })?;
                let present: HashSet<u32> = report
                    .new_revisions
                    .iter()
                    .map(|revision| revision.revision_id)
                    .collect();
                for effect in &target.effects {
                    match effect.status {
                        TaskEffectStatus::Satisfied => {
                            let missing: Vec<u32> = effect
                                .minted_revision_ids
                                .iter()
                                .copied()
                                .filter(|identity| !present.contains(identity))
                                .collect();
                            if !missing.is_empty() {
                                return Err(TaskVerificationError::Verification(format!(
                                    "target {} effect {:?} is missing revision identities {:?}",
                                    target.path.display(),
                                    effect.declaration.effect_id,
                                    missing
                                )));
                            }
                            effects_verified += 1;
                        }
                        TaskEffectStatus::Unsatisfied => {
                            unsatisfied_effects.push(effect.declaration.effect_id.clone());
                        }
                    }
                }
                outputs_verified += 1;
            }
            (None, None) => {
                for effect in &target.effects {
                    if effect.status == TaskEffectStatus::Satisfied {
                        return Err(TaskVerificationError::Verification(format!(
                            "target {} has no committed output but effect {:?} claims satisfied",
                            target.path.display(),
                            effect.declaration.effect_id
                        )));
                    }
                    unsatisfied_effects.push(effect.declaration.effect_id.clone());
                }
            }
            _ => unreachable!("manifest invariant validation pairs output and audit binding"),
        }
    }

    Ok(VerificationSummary {
        schema: "stemma.task_verification.v1",
        task_id: manifest.task_id,
        status: manifest.status,
        inputs_verified: manifest.inputs.len(),
        targets_verified: manifest.targets.len(),
        outputs_verified,
        effects_verified,
        unsatisfied_effects,
    })
}

fn verify_artifact(
    authority: &PathAuthority,
    expected: &ManifestArtifact,
    context: &str,
) -> Result<stemma_artifacts::ReadArtifact, TaskVerificationError> {
    verify_identity_fields(
        authority,
        &expected.path,
        expected.bytes,
        &expected.sha256,
        context,
    )
}

fn verify_identity(
    authority: &PathAuthority,
    path: &Path,
    expected: &ManifestIdentity,
    context: &str,
) -> Result<stemma_artifacts::ReadArtifact, TaskVerificationError> {
    verify_identity_fields(authority, path, expected.bytes, &expected.sha256, context)
}

fn verify_identity_fields(
    authority: &PathAuthority,
    path: &Path,
    expected_bytes: u64,
    expected_sha256: &str,
    context: &str,
) -> Result<stemma_artifacts::ReadArtifact, TaskVerificationError> {
    let actual = authority
        .read_source(path, context.replace(' ', "_"), None)
        .map_err(|error| {
            TaskVerificationError::Verification(format!(
                "cannot read {context} {}: {error}",
                path.display()
            ))
        })?;
    if actual.identity().bytes != expected_bytes || actual.identity().digest.hex != expected_sha256
    {
        return Err(TaskVerificationError::Verification(format!(
            "{context} {} does not match manifest identity: expected {} bytes/sha256 {}, got {} bytes/sha256 {}",
            path.display(),
            expected_bytes,
            expected_sha256,
            actual.identity().bytes,
            actual.identity().digest.hex
        )));
    }
    Ok(actual)
}

fn verify_output_audit(
    manifest: &TaskManifest,
    target_path: &Path,
    before: &[u8],
    after: &[u8],
    binding: &TaskAuditBinding,
) -> Result<(), TaskVerificationError> {
    let report = stemma::api::audit(before, after).map_err(|error| {
        TaskVerificationError::Verification(format!(
            "cannot audit target {}: {error}",
            target_path.display()
        ))
    })?;
    let baseline_validation = stemma::api::validate(before);
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
            !matches!(&change.story, stemma::StoryScope::Comment { .. })
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
    if counts != binding.counts {
        return Err(TaskVerificationError::Verification(format!(
            "target {} audit counts differ from the manifest: expected {:?}, recomputed {:?}",
            target_path.display(),
            binding.counts,
            counts
        )));
    }
    let blocking_finding_count = counts.unexplained_direct_changes
        + counts.unexpected_changed_prior_revisions
        + counts.untouched_violations
        + counts.new_validator_issues;
    let verdict = TaskAuditVerdict {
        status: TaskAuditStatus::Pass,
        deliverable: blocking_finding_count == 0,
        blocking_finding_count,
    };
    if verdict != binding.verdict {
        return Err(TaskVerificationError::Verification(format!(
            "target {} audit verdict differs from the manifest",
            target_path.display()
        )));
    }

    let decision = json!({"counts": counts, "verdict": verdict});
    let set_sha256 = canonical_set_sha256(std::slice::from_ref(&decision));
    if set_sha256 != binding.set_sha256 {
        return Err(TaskVerificationError::Verification(format!(
            "target {} audit commitment differs from the manifest",
            target_path.display()
        )));
    }
    if manifest.status == TaskManifestStatus::Complete && !binding.verdict.deliverable {
        return Err(TaskVerificationError::Verification(format!(
            "complete task target {} is not deliverable",
            target_path.display()
        )));
    }
    Ok(())
}

fn canonical_set_sha256(rows: &[Value]) -> String {
    fn canonical_json(value: &Value, out: &mut String) {
        match value {
            Value::Null => out.push_str("null"),
            Value::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
            Value::Number(value) => out.push_str(&value.to_string()),
            Value::String(value) => out.push_str(&serde_json::to_string(value).unwrap()),
            Value::Array(values) => {
                out.push('[');
                for (index, value) in values.iter().enumerate() {
                    if index != 0 {
                        out.push(',');
                    }
                    canonical_json(value, out);
                }
                out.push(']');
            }
            Value::Object(values) => {
                out.push('{');
                let mut keys: Vec<_> = values.keys().collect();
                keys.sort_unstable();
                for (index, key) in keys.iter().enumerate() {
                    if index != 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(key).unwrap());
                    out.push(':');
                    canonical_json(&values[*key], out);
                }
                out.push('}');
            }
        }
    }

    let mut canonical = String::new();
    canonical_json(&Value::Array(rows.to_vec()), &mut canonical);
    format!("{:x}", Sha256::digest(canonical.as_bytes()))
}
