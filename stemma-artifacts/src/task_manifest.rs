//! Durable task-manifest model shared by producers and offline verifiers.
//!
//! The manifest is evidence, not narration: every state is explicit, every
//! artifact is bound by SHA-256, satisfied effects carry non-zero revision
//! identities, and an inconsistent complete/partial verdict is refused before
//! serialization.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{ArtifactError, ArtifactIdentity, DigestAlgorithm, OutputArtifact, PathAuthority};

/// The only task-manifest schema understood by this release.
pub const TASK_MANIFEST_SCHEMA_V1: &str = "stemma.task_manifest.v1";

/// A delivery task's terminal state.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskManifestStatus {
    Complete,
    Partial,
}

/// Producer identity carried by a task manifest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TaskManifestProducer {
    pub name: String,
    pub version: String,
    pub build: String,
}

/// Why a non-target input was incorporated into a task.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskInputRole {
    ReadOnlySource,
}

/// Portable exact-byte identity. `path` is resolved relative to the manifest
/// directory (or the verifier's explicit root), never treated as an identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestArtifact {
    pub path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
}

/// Exact byte identity without a second path. Target baselines use this shape
/// because their one authoritative path is the enclosing target path.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestIdentity {
    pub bytes: u64,
    pub sha256: String,
}

impl ManifestIdentity {
    pub fn from_identity(identity: &ArtifactIdentity) -> Result<Self, TaskManifestInvariantError> {
        if identity.digest.algorithm != DigestAlgorithm::Sha256 {
            return Err(TaskManifestInvariantError::UnsupportedDigest);
        }
        let value = Self {
            bytes: identity.bytes,
            sha256: identity.digest.hex.clone(),
        };
        value.validate("artifact identity")?;
        Ok(value)
    }

    fn validate(&self, context: &str) -> Result<(), TaskManifestInvariantError> {
        validate_sha256(&self.sha256, context)
    }
}

impl ManifestArtifact {
    /// Construct from a host-bound identity while choosing the portable path
    /// deliberately. Only SHA-256 identities are admissible in schema v1.
    pub fn from_identity(
        path: impl Into<PathBuf>,
        identity: &ArtifactIdentity,
    ) -> Result<Self, TaskManifestInvariantError> {
        if identity.digest.algorithm != DigestAlgorithm::Sha256 {
            return Err(TaskManifestInvariantError::UnsupportedDigest);
        }
        let artifact = Self {
            path: path.into(),
            bytes: identity.bytes,
            sha256: identity.digest.hex.clone(),
        };
        artifact.validate("artifact")?;
        Ok(artifact)
    }

    fn validate(&self, context: &str) -> Result<(), TaskManifestInvariantError> {
        validate_portable_path(&self.path, context)?;
        validate_sha256(&self.sha256, context)
    }
}

/// One task input that is read but never mutated.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TaskManifestInput {
    #[serde(flatten)]
    pub artifact: ManifestArtifact,
    pub role: TaskInputRole,
}

/// Literal replacement matching policy.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskMatchMode {
    Exact,
    NormalizeWs,
}

/// What to do when a literal match crosses an opaque/revision barrier.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskBarrierPolicy {
    Skip,
    Fail,
}

/// A replacement's normalized target scope. This is a sum type so a partial
/// range or mixed block/range scope cannot enter the core model.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskReplacementScope {
    BodyAndTables,
    Block {
        block_id: String,
    },
    Range {
        from_block_id: String,
        to_block_id: String,
    },
}

/// The only v1-declarable effect: one exact-count tracked replacement item.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskEffectOperation {
    ReplaceText,
}

/// The only v1-declarable effect: one exact-count tracked replacement item.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeclaredTaskEffect {
    pub effect_id: String,
    pub op: TaskEffectOperation,
    pub find: String,
    pub replace: String,
    pub match_mode: TaskMatchMode,
    pub scope: TaskReplacementScope,
    pub expected_matches: usize,
    pub on_barrier_match: TaskBarrierPolicy,
}

impl DeclaredTaskEffect {
    fn validate(&self, target: &Path) -> Result<(), TaskManifestInvariantError> {
        validate_label(&self.effect_id, "effect_id")?;
        if self.find.is_empty() {
            return Err(TaskManifestInvariantError::EmptyEffectFind {
                effect_id: self.effect_id.clone(),
            });
        }
        if self.find == self.replace {
            return Err(TaskManifestInvariantError::NoOpDeclaredEffect {
                effect_id: self.effect_id.clone(),
            });
        }
        if self.expected_matches == 0 {
            return Err(TaskManifestInvariantError::ZeroExpectedMatches {
                effect_id: self.effect_id.clone(),
            });
        }
        match &self.scope {
            TaskReplacementScope::BodyAndTables => {}
            TaskReplacementScope::Block { block_id } => {
                validate_label(block_id, "scope.block_id")?;
            }
            TaskReplacementScope::Range {
                from_block_id,
                to_block_id,
            } => {
                validate_label(from_block_id, "scope.from_block_id")?;
                validate_label(to_block_id, "scope.to_block_id")?;
            }
        }
        if target.as_os_str().is_empty() {
            return Err(TaskManifestInvariantError::InvalidPath {
                context: "effect target".to_string(),
                path: target.to_path_buf(),
                reason: "path is empty".to_string(),
            });
        }
        Ok(())
    }
}

/// Whether one declared effect survived into its committed output.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskEffectStatus {
    Satisfied,
    Unsatisfied,
}

/// One declaration joined to its execution and committed-artifact evidence.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TaskManifestEffect {
    pub declaration: DeclaredTaskEffect,
    pub status: TaskEffectStatus,
    pub minted_revision_ids: Vec<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl TaskManifestEffect {
    fn validate(&self, target: &Path) -> Result<(), TaskManifestInvariantError> {
        self.declaration.validate(target)?;
        let mut ids = HashSet::new();
        for id in &self.minted_revision_ids {
            if *id == 0 {
                return Err(TaskManifestInvariantError::ZeroRevisionIdentity {
                    effect_id: self.declaration.effect_id.clone(),
                });
            }
            if !ids.insert(*id) {
                return Err(TaskManifestInvariantError::DuplicateRevisionIdentity {
                    effect_id: self.declaration.effect_id.clone(),
                    revision_id: *id,
                });
            }
        }
        match self.status {
            TaskEffectStatus::Satisfied => {
                if self.minted_revision_ids.is_empty() {
                    return Err(TaskManifestInvariantError::SatisfiedWithoutIdentity {
                        effect_id: self.declaration.effect_id.clone(),
                    });
                }
                if self.reason.is_some() {
                    return Err(TaskManifestInvariantError::SatisfiedWithReason {
                        effect_id: self.declaration.effect_id.clone(),
                    });
                }
            }
            TaskEffectStatus::Unsatisfied => {
                if !self.minted_revision_ids.is_empty() {
                    return Err(TaskManifestInvariantError::UnsatisfiedWithIdentity {
                        effect_id: self.declaration.effect_id.clone(),
                    });
                }
                if self.reason.as_deref().is_none_or(str::is_empty) {
                    return Err(TaskManifestInvariantError::UnsatisfiedWithoutReason {
                        effect_id: self.declaration.effect_id.clone(),
                    });
                }
            }
        }
        Ok(())
    }
}

/// Exact audit counts bound into a successful v0.3+ save receipt.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TaskAuditCounts {
    pub new_revisions: u64,
    pub direct_changes: u64,
    pub unexplained_direct_changes: u64,
    pub preexisting_revisions: u64,
    pub changed_prior_revisions: u64,
    pub expected_changed_prior_revisions: u64,
    pub unexpected_changed_prior_revisions: u64,
    pub untouched_violations: u64,
    pub validator_issues: u64,
    pub new_validator_issues: u64,
}

/// Deliverability decision committed by `save_docx`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskAuditStatus {
    Pass,
}

/// Deliverability decision committed by `save_docx`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TaskAuditVerdict {
    pub status: TaskAuditStatus,
    pub deliverable: bool,
    pub blocking_finding_count: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskAuditScope {
    DeclaredTaskToSavedOutput,
}

/// Audit receipt joining one open-session baseline to committed output bytes.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TaskAuditBinding {
    pub doc_id: String,
    pub scope: TaskAuditScope,
    pub output_sha256: String,
    pub set_sha256: String,
    pub counts: TaskAuditCounts,
    pub verdict: TaskAuditVerdict,
}

impl TaskAuditBinding {
    fn validate(&self, output: &ManifestArtifact) -> Result<(), TaskManifestInvariantError> {
        validate_label(&self.doc_id, "audit_binding.doc_id")?;
        validate_sha256(&self.output_sha256, "audit_binding.output_sha256")?;
        validate_sha256(&self.set_sha256, "audit_binding.set_sha256")?;
        if self.output_sha256 != output.sha256 {
            return Err(TaskManifestInvariantError::AuditOutputMismatch {
                output: output.sha256.clone(),
                audit: self.output_sha256.clone(),
            });
        }
        let blocking_finding_count = self.counts.unexplained_direct_changes
            + self.counts.unexpected_changed_prior_revisions
            + self.counts.untouched_violations
            + self.counts.new_validator_issues;
        if self.verdict.blocking_finding_count != blocking_finding_count {
            return Err(TaskManifestInvariantError::AuditVerdictCountMismatch {
                verdict: self.verdict.blocking_finding_count,
                counts: blocking_finding_count,
            });
        }
        if !self.verdict.deliverable || self.verdict.blocking_finding_count != 0 {
            return Err(TaskManifestInvariantError::CommittedOutputNotDeliverable);
        }
        Ok(())
    }
}

/// One mutable task target, from hash-bound input to optional committed output.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TaskManifestTarget {
    pub path: PathBuf,
    pub input: ManifestIdentity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<ManifestArtifact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audit_binding: Option<TaskAuditBinding>,
    pub effects: Vec<TaskManifestEffect>,
}

impl TaskManifestTarget {
    fn validate(&self) -> Result<(), TaskManifestInvariantError> {
        validate_portable_path(&self.path, "target.path")?;
        self.input.validate("target.input")?;
        if let Some(doc_id) = &self.doc_id {
            validate_label(doc_id, "target.doc_id")?;
        }
        match (&self.output, &self.audit_binding) {
            (Some(output), Some(binding)) => {
                output.validate("target.output")?;
                binding.validate(output)?;
                if self.doc_id.as_deref() != Some(binding.doc_id.as_str()) {
                    return Err(TaskManifestInvariantError::AuditDocMismatch);
                }
            }
            (None, None) => {}
            _ => return Err(TaskManifestInvariantError::IncompleteOutputBinding),
        }
        if self.effects.is_empty() {
            return Err(TaskManifestInvariantError::TargetWithoutEffects {
                target: self.path.clone(),
            });
        }
        let mut claimed_revision_ids = HashSet::new();
        for effect in &self.effects {
            effect.validate(&self.path)?;
            for revision_id in &effect.minted_revision_ids {
                if !claimed_revision_ids.insert(*revision_id) {
                    return Err(TaskManifestInvariantError::RevisionIdentityClaimedTwice {
                        target: self.path.clone(),
                        revision_id: *revision_id,
                    });
                }
            }
        }
        Ok(())
    }
}

/// Complete durable receipt for one task.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TaskManifest {
    pub schema: String,
    pub task_id: String,
    pub status: TaskManifestStatus,
    pub producer: TaskManifestProducer,
    pub inputs: Vec<TaskManifestInput>,
    pub targets: Vec<TaskManifestTarget>,
}

impl TaskManifest {
    /// Validate every cross-field invariant before writing or trusting a
    /// decoded manifest.
    pub fn validate(&self) -> Result<(), TaskManifestInvariantError> {
        if self.schema != TASK_MANIFEST_SCHEMA_V1 {
            return Err(TaskManifestInvariantError::UnknownSchema(
                self.schema.clone(),
            ));
        }
        validate_label(&self.task_id, "task_id")?;
        validate_label(&self.producer.name, "producer.name")?;
        validate_label(&self.producer.version, "producer.version")?;
        validate_label(&self.producer.build, "producer.build")?;
        if self.targets.is_empty() {
            return Err(TaskManifestInvariantError::NoTargets);
        }

        let mut paths = HashSet::new();
        for input in &self.inputs {
            input.artifact.validate("input")?;
            if !paths.insert(input.artifact.path.clone()) {
                return Err(TaskManifestInvariantError::DuplicateArtifactPath(
                    input.artifact.path.clone(),
                ));
            }
        }

        let mut effect_ids = HashSet::new();
        let mut all_satisfied = true;
        let mut all_committed = true;
        for target in &self.targets {
            target.validate()?;
            if !paths.insert(target.path.clone()) {
                return Err(TaskManifestInvariantError::DuplicateArtifactPath(
                    target.path.clone(),
                ));
            }
            all_committed &= target.output.is_some();
            for effect in &target.effects {
                let effect_id = effect.declaration.effect_id.clone();
                if !effect_ids.insert(effect_id.clone()) {
                    return Err(TaskManifestInvariantError::DuplicateEffectId(effect_id));
                }
                all_satisfied &= effect.status == TaskEffectStatus::Satisfied;
            }
        }

        let complete = all_satisfied && all_committed;
        match (self.status, complete) {
            (TaskManifestStatus::Complete, true) | (TaskManifestStatus::Partial, false) => Ok(()),
            (TaskManifestStatus::Complete, false) => Err(TaskManifestInvariantError::FalseComplete),
            (TaskManifestStatus::Partial, true) => Err(TaskManifestInvariantError::FalsePartial),
        }
    }
}

/// Parse and validate an exact schema-v1 manifest. Unknown or malformed states
/// never degrade to an empty/default receipt.
pub fn decode_task_manifest(bytes: &[u8]) -> Result<TaskManifest, TaskManifestDecodeError> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(TaskManifestDecodeError::InvalidJson)?;
    let schema = value
        .get("schema")
        .and_then(serde_json::Value::as_str)
        .ok_or(TaskManifestDecodeError::MissingSchema)?;
    if schema != TASK_MANIFEST_SCHEMA_V1 {
        return Err(TaskManifestDecodeError::UnknownSchema(schema.to_string()));
    }
    let manifest: TaskManifest =
        serde_json::from_value(value).map_err(TaskManifestDecodeError::InvalidJson)?;
    manifest
        .validate()
        .map_err(TaskManifestDecodeError::Invariant)?;
    Ok(manifest)
}

/// Validate and serialize one manifest in a deterministic, human-readable
/// representation. The trailing newline is part of the committed bytes.
pub fn encode_task_manifest(manifest: &TaskManifest) -> Result<Vec<u8>, TaskManifestEncodeError> {
    manifest.validate()?;
    let mut bytes = serde_json::to_vec_pretty(manifest)?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Validate and create one manifest without overwriting any existing path.
pub fn commit_task_manifest(
    authority: &PathAuthority,
    path: impl AsRef<Path>,
    manifest: &TaskManifest,
    protected_sources: &[ArtifactIdentity],
) -> Result<OutputArtifact, TaskManifestCommitError> {
    let bytes = encode_task_manifest(manifest)?;
    authority
        .commit_new(path, "task_manifest", &bytes, protected_sources)
        .map_err(TaskManifestCommitError::Artifact)
}

#[derive(Debug, Error)]
pub enum TaskManifestInvariantError {
    #[error("unknown task manifest schema {0:?}")]
    UnknownSchema(String),
    #[error("task manifest must declare at least one target")]
    NoTargets,
    #[error("{context} must be a non-empty string")]
    EmptyLabel { context: String },
    #[error("{context} path {path:?} is invalid: {reason}")]
    InvalidPath {
        context: String,
        path: PathBuf,
        reason: String,
    },
    #[error("{context} SHA-256 must be exactly 64 lowercase hexadecimal characters")]
    InvalidSha256 { context: String },
    #[error("schema v1 supports only SHA-256 artifact identities")]
    UnsupportedDigest,
    #[error("artifact path {0:?} appears more than once")]
    DuplicateArtifactPath(PathBuf),
    #[error("effect_id {0:?} appears more than once")]
    DuplicateEffectId(String),
    #[error("effect {effect_id:?} has an empty find string")]
    EmptyEffectFind { effect_id: String },
    #[error("effect {effect_id:?} declares identical find and replacement text")]
    NoOpDeclaredEffect { effect_id: String },
    #[error("effect {effect_id:?} must expect at least one match")]
    ZeroExpectedMatches { effect_id: String },
    #[error("effect {effect_id:?} claims revision identity 0")]
    ZeroRevisionIdentity { effect_id: String },
    #[error("effect {effect_id:?} repeats revision identity {revision_id}")]
    DuplicateRevisionIdentity { effect_id: String, revision_id: u32 },
    #[error(
        "revision identity {revision_id} on target {target:?} is claimed by more than one effect"
    )]
    RevisionIdentityClaimedTwice { target: PathBuf, revision_id: u32 },
    #[error("satisfied effect {effect_id:?} has no revision identity")]
    SatisfiedWithoutIdentity { effect_id: String },
    #[error("satisfied effect {effect_id:?} carries a failure reason")]
    SatisfiedWithReason { effect_id: String },
    #[error("unsatisfied effect {effect_id:?} must carry an actionable reason")]
    UnsatisfiedWithoutReason { effect_id: String },
    #[error("unsatisfied effect {effect_id:?} cannot claim a revision identity")]
    UnsatisfiedWithIdentity { effect_id: String },
    #[error("target {target:?} declares no effects")]
    TargetWithoutEffects { target: PathBuf },
    #[error("target output and audit binding must either both be present or both be absent")]
    IncompleteOutputBinding,
    #[error("audit binding doc_id does not match the target doc_id")]
    AuditDocMismatch,
    #[error("audit output hash {audit} does not match committed output hash {output}")]
    AuditOutputMismatch { output: String, audit: String },
    #[error("a committed target output must carry a passing deliverability verdict")]
    CommittedOutputNotDeliverable,
    #[error("audit verdict reports {verdict} blocking findings but its counts contain {counts}")]
    AuditVerdictCountMismatch { verdict: u64, counts: u64 },
    #[error("manifest claims complete although at least one target or effect is incomplete")]
    FalseComplete,
    #[error("manifest claims partial although every target and effect is complete")]
    FalsePartial,
}

#[derive(Debug, Error)]
pub enum TaskManifestDecodeError {
    #[error("task manifest is not valid schema JSON: {0}")]
    InvalidJson(serde_json::Error),
    #[error("task manifest has no string schema field")]
    MissingSchema,
    #[error("unknown task manifest schema {0:?}")]
    UnknownSchema(String),
    #[error("invalid task manifest: {0}")]
    Invariant(TaskManifestInvariantError),
}

#[derive(Debug, Error)]
pub enum TaskManifestEncodeError {
    #[error("invalid task manifest: {0}")]
    Invariant(#[from] TaskManifestInvariantError),
    #[error("cannot serialize task manifest: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Error)]
pub enum TaskManifestCommitError {
    #[error(transparent)]
    Encode(#[from] TaskManifestEncodeError),
    #[error(transparent)]
    Artifact(ArtifactError),
}

fn validate_label(value: &str, context: &str) -> Result<(), TaskManifestInvariantError> {
    if value.trim().is_empty() {
        return Err(TaskManifestInvariantError::EmptyLabel {
            context: context.to_string(),
        });
    }
    Ok(())
}

fn validate_sha256(value: &str, context: &str) -> Result<(), TaskManifestInvariantError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(TaskManifestInvariantError::InvalidSha256 {
            context: context.to_string(),
        });
    }
    Ok(())
}

fn validate_portable_path(path: &Path, context: &str) -> Result<(), TaskManifestInvariantError> {
    if path.as_os_str().is_empty() {
        return Err(TaskManifestInvariantError::InvalidPath {
            context: context.to_string(),
            path: path.to_path_buf(),
            reason: "path is empty".to_string(),
        });
    }
    if path.is_absolute() {
        return Err(TaskManifestInvariantError::InvalidPath {
            context: context.to_string(),
            path: path.to_path_buf(),
            reason: "schema v1 paths must be relative for relocation".to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha(byte: char) -> String {
        std::iter::repeat_n(byte, 64).collect()
    }

    fn effect(id: &str, status: TaskEffectStatus) -> TaskManifestEffect {
        TaskManifestEffect {
            declaration: DeclaredTaskEffect {
                effect_id: id.to_string(),
                op: TaskEffectOperation::ReplaceText,
                find: "old".to_string(),
                replace: "new".to_string(),
                match_mode: TaskMatchMode::Exact,
                scope: TaskReplacementScope::BodyAndTables,
                expected_matches: 1,
                on_barrier_match: TaskBarrierPolicy::Skip,
            },
            status,
            minted_revision_ids: if status == TaskEffectStatus::Satisfied {
                vec![7]
            } else {
                vec![]
            },
            reason: if status == TaskEffectStatus::Unsatisfied {
                Some("not attempted".to_string())
            } else {
                None
            },
        }
    }

    fn binding(doc_id: &str, output_sha: &str) -> TaskAuditBinding {
        TaskAuditBinding {
            doc_id: doc_id.to_string(),
            scope: TaskAuditScope::DeclaredTaskToSavedOutput,
            output_sha256: output_sha.to_string(),
            set_sha256: sha('c'),
            counts: TaskAuditCounts {
                new_revisions: 1,
                direct_changes: 0,
                unexplained_direct_changes: 0,
                preexisting_revisions: 0,
                changed_prior_revisions: 0,
                expected_changed_prior_revisions: 0,
                unexpected_changed_prior_revisions: 0,
                untouched_violations: 0,
                validator_issues: 0,
                new_validator_issues: 0,
            },
            verdict: TaskAuditVerdict {
                status: TaskAuditStatus::Pass,
                deliverable: true,
                blocking_finding_count: 0,
            },
        }
    }

    fn manifest(status: TaskManifestStatus, effect_status: TaskEffectStatus) -> TaskManifest {
        let output_sha = sha('b');
        TaskManifest {
            schema: TASK_MANIFEST_SCHEMA_V1.to_string(),
            task_id: "t1".to_string(),
            status,
            producer: TaskManifestProducer {
                name: "stemma-mcp".to_string(),
                version: "0.4.0".to_string(),
                build: "0.4.0+g123456789abc".to_string(),
            },
            inputs: vec![],
            targets: vec![TaskManifestTarget {
                path: "input.docx".into(),
                input: ManifestIdentity {
                    bytes: 10,
                    sha256: sha('a'),
                },
                doc_id: Some("d1".to_string()),
                output: if status == TaskManifestStatus::Complete {
                    Some(ManifestArtifact {
                        path: "output.docx".into(),
                        bytes: 11,
                        sha256: output_sha.clone(),
                    })
                } else {
                    None
                },
                audit_binding: if status == TaskManifestStatus::Complete {
                    Some(binding("d1", &output_sha))
                } else {
                    None
                },
                effects: vec![effect("e1", effect_status)],
            }],
        }
    }

    #[test]
    fn complete_round_trips_and_is_strictly_validated() {
        let expected = manifest(TaskManifestStatus::Complete, TaskEffectStatus::Satisfied);
        let bytes = encode_task_manifest(&expected).expect("encode");
        assert_eq!(decode_task_manifest(&bytes).expect("decode"), expected);
        assert!(bytes.ends_with(b"\n"));
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["targets"][0]["path"], "input.docx");
        assert!(
            value["targets"][0]["input"].get("path").is_none(),
            "the enclosing target path is the baseline's only path binding"
        );
        assert_eq!(
            value["targets"][0]["effects"][0]["declaration"]["op"],
            "replace_text"
        );
        assert_eq!(
            value["targets"][0]["audit_binding"]["scope"],
            "declared_task_to_saved_output"
        );
    }

    #[test]
    fn false_complete_and_false_partial_are_refused() {
        let false_complete = manifest(TaskManifestStatus::Complete, TaskEffectStatus::Unsatisfied);
        assert!(matches!(
            false_complete.validate(),
            Err(TaskManifestInvariantError::FalseComplete)
        ));

        let false_partial = manifest(TaskManifestStatus::Partial, TaskEffectStatus::Satisfied);
        let target = &mut false_partial.clone();
        target.targets[0].output = Some(ManifestArtifact {
            path: "output.docx".into(),
            bytes: 11,
            sha256: sha('b'),
        });
        target.targets[0].audit_binding = Some(binding("d1", &sha('b')));
        assert!(matches!(
            target.validate(),
            Err(TaskManifestInvariantError::FalsePartial)
        ));
    }

    #[test]
    fn unknown_fields_and_unknown_schema_fail_decode() {
        let mut value = serde_json::to_value(manifest(
            TaskManifestStatus::Complete,
            TaskEffectStatus::Satisfied,
        ))
        .unwrap();
        value["surprise"] = serde_json::json!(true);
        let error = decode_task_manifest(&serde_json::to_vec(&value).unwrap()).unwrap_err();
        assert!(matches!(error, TaskManifestDecodeError::InvalidJson(_)));

        value.as_object_mut().unwrap().remove("surprise");
        value["schema"] = serde_json::json!("stemma.task_manifest.v99");
        let error = decode_task_manifest(&serde_json::to_vec(&value).unwrap()).unwrap_err();
        assert!(matches!(error, TaskManifestDecodeError::UnknownSchema(_)));
    }

    #[test]
    fn one_revision_identity_cannot_satisfy_two_effects() {
        let mut invalid = manifest(TaskManifestStatus::Complete, TaskEffectStatus::Satisfied);
        invalid.targets[0]
            .effects
            .push(effect("e2", TaskEffectStatus::Satisfied));
        assert!(matches!(
            invalid.validate(),
            Err(TaskManifestInvariantError::RevisionIdentityClaimedTwice { revision_id: 7, .. })
        ));
    }
}
