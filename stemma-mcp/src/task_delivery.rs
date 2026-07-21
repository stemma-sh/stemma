use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rmcp::schemars;
use serde::Deserialize;
use serde_json::json;
use stemma_artifacts::{
    ArtifactIdentity, DeclaredTaskEffect, ManifestArtifact, ManifestIdentity, OutputArtifact,
    ReadArtifact, TASK_MANIFEST_SCHEMA_V1, TaskAuditBinding, TaskBarrierPolicy,
    TaskEffectOperation, TaskEffectStatus, TaskInputRole, TaskManifest, TaskManifestEffect,
    TaskManifestInput, TaskManifestProducer, TaskManifestStatus, TaskManifestTarget, TaskMatchMode,
    TaskReplacementScope, commit_task_manifest,
};

use super::{
    CallToolResult, CoreBarrierPolicy, CoreReplacementItem, CoreReplacementMatchMode,
    SERVER_VERSION, StemmaServer, artifact_fail, fail_json,
};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct TaskDeclarationArg {
    pub task_id: String,
    pub manifest_path: String,
    #[serde(default)]
    pub inputs: Vec<TaskInputArg>,
    pub targets: Vec<TaskTargetArg>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct TaskInputArg {
    pub path: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct TaskTargetArg {
    pub path: String,
    pub effects: Vec<TaskEffectArg>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct TaskEffectArg {
    pub effect_id: String,
    pub op: TaskEffectOperationArg,
    pub find: String,
    pub replace: String,
    #[serde(default)]
    pub match_mode: TaskMatchModeArg,
    #[serde(default)]
    pub scope: TaskEffectScopeArg,
    pub expected_matches: usize,
    #[serde(default)]
    pub on_barrier_match: TaskBarrierPolicyArg,
}

#[derive(Clone, Copy, Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum TaskEffectOperationArg {
    ReplaceText,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum TaskMatchModeArg {
    #[default]
    Exact,
    NormalizeWs,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum TaskBarrierPolicyArg {
    #[default]
    Skip,
    Fail,
}

#[derive(Clone, Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct TaskEffectScopeArg {
    #[serde(default)]
    pub block_id: Option<String>,
    #[serde(default)]
    pub from_block_id: Option<String>,
    #[serde(default)]
    pub to_block_id: Option<String>,
}

#[derive(Clone, Debug)]
pub(super) enum EffectOutcome {
    Pending { last_failure: Option<String> },
    Applied { revision_ids: Vec<u32> },
    Unverifiable { reason: String },
}

#[derive(Clone, Debug)]
pub(super) struct TaskEffectState {
    pub declaration: DeclaredTaskEffect,
    pub outcome: EffectOutcome,
}

#[derive(Clone, Debug)]
pub(super) struct TaskTargetState {
    pub portable_path: PathBuf,
    pub input_identity: ArtifactIdentity,
    pub input_manifest: ManifestIdentity,
    pub doc_id: Option<String>,
    pub output: Option<stemma_artifacts::ManifestArtifact>,
    pub output_identity: Option<ArtifactIdentity>,
    pub audit_binding: Option<stemma_artifacts::TaskAuditBinding>,
    pub effects: Vec<TaskEffectState>,
}

#[derive(Clone, Debug)]
pub(super) struct TaskState {
    pub task_id: String,
    pub manifest_path: String,
    pub manifest_resolved_path: PathBuf,
    pub inputs: Vec<TaskManifestInput>,
    pub input_identities: Vec<ArtifactIdentity>,
    pub target_order: Vec<PathBuf>,
    pub targets: HashMap<PathBuf, TaskTargetState>,
    pub terminated: bool,
}

#[derive(Clone, Debug)]
pub(super) struct TaskDocBinding {
    pub task_id: String,
    pub target_path: PathBuf,
}

#[derive(Default)]
pub(super) struct TaskRegistry {
    pub tasks: HashMap<String, TaskState>,
    pub docs: HashMap<String, TaskDocBinding>,
}

pub(super) struct PendingTaskDeclaration {
    pub task: TaskState,
    pub source: ReadArtifact,
}

pub(super) struct TaskWorklistBinding {
    pub task_id: String,
    pub target_path: PathBuf,
    pub effect_ids: Vec<String>,
}

pub(super) enum TaskSaveOutcome {
    NotFinal,
    Complete {
        task_id: String,
        manifest: OutputArtifact,
    },
    Partial {
        task_id: String,
        manifest: OutputArtifact,
        unsatisfied_effects: Vec<String>,
    },
}

pub(super) enum TaskWriteFailureOutcome {
    Retryable,
    Partial {
        task_id: String,
        manifest: OutputArtifact,
        unsatisfied_effects: Vec<String>,
    },
}

impl StemmaServer {
    pub(super) fn task_doc_binding(&self, doc_id: &str) -> Option<TaskDocBinding> {
        self.tasks
            .lock()
            .expect("tasks mutex poisoned")
            .docs
            .get(doc_id)
            .cloned()
    }

    pub(super) fn prepare_task_save(
        &self,
        doc_id: &str,
        output_path: &str,
    ) -> Result<Option<TaskDocBinding>, CallToolResult> {
        let Some(binding) = self.task_doc_binding(doc_id) else {
            return Ok(None);
        };
        let output_resolved_path = self
            .artifacts
            .resolve_new_path(output_path)
            .map_err(artifact_fail)?;
        let registry = self.tasks.lock().expect("tasks mutex poisoned");
        let task = registry
            .tasks
            .get(&binding.task_id)
            .expect("doc binding always names an existing task");
        if task.terminated {
            return Err(task_error(
                "task_terminated",
                &task.task_id,
                "a terminated task cannot save another output",
            ));
        }
        if output_resolved_path == task.manifest_resolved_path {
            return Err(task_error(
                "task_output_alias",
                &task.task_id,
                "output path aliases the task manifest path",
            ));
        }
        let target = task
            .targets
            .get(&binding.target_path)
            .expect("doc binding always names an existing target");
        if target.output.is_some() {
            return Err(task_error(
                "task_target_already_saved",
                &task.task_id,
                format!(
                    "target {:?} already has a committed output",
                    target.portable_path
                ),
            ));
        }
        Ok(Some(binding))
    }

    pub(super) fn task_protected_sources(
        &self,
        binding: Option<&TaskDocBinding>,
        mut session_sources: Vec<ArtifactIdentity>,
    ) -> Result<Vec<ArtifactIdentity>, CallToolResult> {
        let Some(binding) = binding else {
            return Ok(session_sources);
        };
        let registry = self.tasks.lock().expect("tasks mutex poisoned");
        let task = registry
            .tasks
            .get(&binding.task_id)
            .expect("doc binding always names an existing task");
        session_sources.extend(task.input_identities.iter().cloned());
        session_sources.extend(
            task.targets
                .values()
                .map(|target| target.input_identity.clone()),
        );
        session_sources.extend(
            task.targets
                .values()
                .filter_map(|target| target.output_identity.clone()),
        );
        let mut seen = HashSet::new();
        session_sources.retain(|identity| seen.insert(identity.resolved_path.clone()));
        Ok(session_sources)
    }

    pub(super) fn record_task_save(
        &self,
        binding: Option<&TaskDocBinding>,
        output: &OutputArtifact,
        audit_binding: TaskAuditBinding,
        committed_revision_ids: &HashSet<u32>,
    ) -> Result<TaskSaveOutcome, CallToolResult> {
        let Some(binding) = binding else {
            return Ok(TaskSaveOutcome::NotFinal);
        };
        let mut registry = self.tasks.lock().expect("tasks mutex poisoned");
        let task = registry
            .tasks
            .get_mut(&binding.task_id)
            .expect("doc binding always names an existing task");
        let manifest_parent = task
            .manifest_resolved_path
            .parent()
            .expect("a resolved manifest path always has a parent");
        let target = task
            .targets
            .get_mut(&binding.target_path)
            .expect("doc binding always names an existing target");
        if target.output.is_some() {
            return Err(task_error(
                "task_target_already_saved",
                &task.task_id,
                format!(
                    "target {:?} already has a committed output",
                    target.portable_path
                ),
            ));
        }
        for effect in &mut target.effects {
            let EffectOutcome::Applied { revision_ids } = &effect.outcome else {
                continue;
            };
            let missing: Vec<u32> = revision_ids
                .iter()
                .copied()
                .filter(|revision_id| !committed_revision_ids.contains(revision_id))
                .collect();
            if !missing.is_empty() {
                effect.outcome = EffectOutcome::Unverifiable {
                    reason: format!(
                        "effect_unverifiable: committed output audit is missing revision identities {missing:?}"
                    ),
                };
            }
        }
        target.output = Some(portable_artifact(manifest_parent, &output.identity)?);
        target.output_identity = Some(output.identity.clone());
        target.audit_binding = Some(audit_binding);

        if task.targets.values().any(|target| target.output.is_none()) {
            return Ok(TaskSaveOutcome::NotFinal);
        }

        let (status, committed, unsatisfied_effects) =
            commit_terminal_task(&self.artifacts, task, None)?;

        Ok(match status {
            TaskManifestStatus::Complete => TaskSaveOutcome::Complete {
                task_id: task.task_id.clone(),
                manifest: committed,
            },
            TaskManifestStatus::Partial => TaskSaveOutcome::Partial {
                task_id: task.task_id.clone(),
                manifest: committed,
                unsatisfied_effects,
            },
        })
    }

    pub(super) fn record_task_write_failure(
        &self,
        binding: Option<&TaskDocBinding>,
        failure: &str,
    ) -> Result<TaskWriteFailureOutcome, CallToolResult> {
        let Some(binding) = binding else {
            return Ok(TaskWriteFailureOutcome::Retryable);
        };
        let mut registry = self.tasks.lock().expect("tasks mutex poisoned");
        let task = registry
            .tasks
            .get_mut(&binding.task_id)
            .expect("doc binding always names an existing task");
        if !task.targets.values().any(|target| target.output.is_some()) {
            return Ok(TaskWriteFailureOutcome::Retryable);
        }
        let (_, manifest, unsatisfied_effects) =
            commit_terminal_task(&self.artifacts, task, Some((&binding.target_path, failure)))?;
        Ok(TaskWriteFailureOutcome::Partial {
            task_id: task.task_id.clone(),
            manifest,
            unsatisfied_effects,
        })
    }

    pub(super) fn refuse_direct_task_mutation(
        &self,
        doc_id: &str,
        tool: &str,
    ) -> Option<CallToolResult> {
        self.task_doc_binding(doc_id).map(|binding| {
            task_error(
                "task_requires_declared_effect",
                &binding.task_id,
                format!(
                    "{tool} cannot mutate task-bound doc_id {doc_id:?}; use execute_plan with a declaration-matched replacement_worklist"
                ),
            )
        })
    }

    pub(super) fn validate_task_worklist(
        &self,
        doc_id: &str,
        items: &[CoreReplacementItem],
    ) -> Result<Option<TaskWorklistBinding>, CallToolResult> {
        let mut registry = self.tasks.lock().expect("tasks mutex poisoned");
        let Some(binding) = registry.docs.get(doc_id).cloned() else {
            if let Some(item) = items.iter().find(|item| item.effect_id.is_some()) {
                return Err(fail_json(json!({
                    "code": "effect_id_without_task",
                    "error": format!(
                        "effect_id {:?} is valid only for a task-bound doc_id",
                        item.effect_id
                    ),
                    "doc_id": doc_id,
                })));
            }
            return Ok(None);
        };
        let task = registry
            .tasks
            .get_mut(&binding.task_id)
            .expect("doc binding always names an existing task");
        if task.terminated {
            return Err(task_error(
                "task_terminated",
                &task.task_id,
                "a terminated task cannot execute another effect",
            ));
        }
        let target = task
            .targets
            .get(&binding.target_path)
            .expect("doc binding always names an existing target");
        let mut call_ids = HashSet::new();
        let mut effect_ids = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let Some(effect_id) = item.effect_id.as_deref() else {
                return Err(task_error(
                    "missing_effect_id",
                    &task.task_id,
                    format!("replacement_worklist item {index} must name its declared effect_id"),
                ));
            };
            if !call_ids.insert(effect_id) {
                return Err(task_error(
                    "duplicate_effect_id",
                    &task.task_id,
                    format!("effect_id {effect_id:?} appears twice in one worklist"),
                ));
            }
            let Some(effect) = target
                .effects
                .iter()
                .find(|effect| effect.declaration.effect_id == effect_id)
            else {
                return Err(task_error(
                    "unknown_effect_id",
                    &task.task_id,
                    format!(
                        "effect_id {effect_id:?} is not declared for target {:?}",
                        target.portable_path
                    ),
                ));
            };
            if !matches!(effect.outcome, EffectOutcome::Pending { .. }) {
                return Err(task_error(
                    "effect_already_executed",
                    &task.task_id,
                    format!("effect_id {effect_id:?} already has a recorded outcome"),
                ));
            }
            validate_item_against_declaration(item, &effect.declaration).map_err(|detail| {
                task_error(
                    "effect_declaration_mismatch",
                    &task.task_id,
                    format!(
                        "replacement_worklist item {index} / effect_id {effect_id:?}: {detail}"
                    ),
                )
            })?;
            effect_ids.push(effect_id.to_string());
        }
        Ok(Some(TaskWorklistBinding {
            task_id: binding.task_id,
            target_path: binding.target_path,
            effect_ids,
        }))
    }

    pub(super) fn record_task_worklist_outcomes(
        &self,
        binding: &TaskWorklistBinding,
        result: &mut CallToolResult,
    ) -> Result<(), CallToolResult> {
        if result.is_error == Some(true) {
            return Ok(());
        }
        let rows = result
            .structured_content
            .as_ref()
            .and_then(|payload| payload.get("items"))
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| {
                task_error(
                    "task_outcome_missing",
                    &binding.task_id,
                    "replacement worklist returned no complete item outcomes",
                )
            })?;
        if rows.len() != binding.effect_ids.len() {
            return Err(task_error(
                "task_outcome_mismatch",
                &binding.task_id,
                format!(
                    "worklist returned {} outcomes for {} declared effects",
                    rows.len(),
                    binding.effect_ids.len()
                ),
            ));
        }

        let mut registry = self.tasks.lock().expect("tasks mutex poisoned");
        let task = registry
            .tasks
            .get_mut(&binding.task_id)
            .expect("validated task still exists while session gate is held");
        let target = task
            .targets
            .get_mut(&binding.target_path)
            .expect("validated target still exists while session gate is held");
        for (row, effect_id) in rows.iter().zip(&binding.effect_ids) {
            let effect = target
                .effects
                .iter_mut()
                .find(|effect| effect.declaration.effect_id == *effect_id)
                .expect("validated effect remains present");
            let status = row
                .get("status")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    task_error(
                        "task_outcome_invalid",
                        &binding.task_id,
                        format!("effect {effect_id:?} outcome has no string status"),
                    )
                })?;
            if status != "applied" {
                if !matches!(status, "mismatch" | "no_match" | "error") {
                    return Err(task_error(
                        "task_outcome_invalid",
                        &binding.task_id,
                        format!("effect {effect_id:?} returned unknown status {status:?}"),
                    ));
                }
                let detail = row
                    .get("error")
                    .or_else(|| row.get("diagnosis"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("the declared replacement did not apply");
                effect.outcome = EffectOutcome::Pending {
                    last_failure: Some(format!("execute_plan outcome {status}: {detail}")),
                };
                continue;
            }
            let revision_values = row
                .get("revision_ids")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| {
                    task_error(
                        "task_outcome_invalid",
                        &binding.task_id,
                        format!("applied effect {effect_id:?} returned no revision_ids array"),
                    )
                })?;
            let mut revision_ids = Vec::with_capacity(revision_values.len());
            for value in revision_values {
                let identity = value.as_u64().ok_or_else(|| {
                    task_error(
                        "task_outcome_invalid",
                        &binding.task_id,
                        format!("effect {effect_id:?} returned non-integer revision identity"),
                    )
                })?;
                revision_ids.push(u32::try_from(identity).map_err(|_| {
                    task_error(
                        "task_outcome_invalid",
                        &binding.task_id,
                        format!("effect {effect_id:?} revision identity {identity} exceeds u32"),
                    )
                })?);
            }
            if revision_ids.is_empty() || revision_ids.contains(&0) {
                effect.outcome = EffectOutcome::Unverifiable {
                    reason: "effect_unverifiable: applied replacement minted no non-zero revision identity"
                        .to_string(),
                };
            } else {
                effect.outcome = EffectOutcome::Applied {
                    revision_ids: revision_ids.clone(),
                };
            }
        }
        Ok(())
    }

    pub(super) fn prepare_task_declaration(
        &self,
        declaration: TaskDeclarationArg,
        opened_path: &str,
    ) -> Result<PendingTaskDeclaration, CallToolResult> {
        if declaration.task_id.trim().is_empty() {
            return Err(task_error(
                "invalid_task_declaration",
                "",
                "task_id must be a non-empty string",
            ));
        }
        if declaration.manifest_path.trim().is_empty() {
            return Err(task_error(
                "invalid_task_declaration",
                &declaration.task_id,
                "manifest_path must be a non-empty path",
            ));
        }
        if declaration.targets.is_empty() {
            return Err(task_error(
                "invalid_task_declaration",
                &declaration.task_id,
                "a task must declare at least one target",
            ));
        }
        if self
            .tasks
            .lock()
            .expect("tasks mutex poisoned")
            .tasks
            .contains_key(&declaration.task_id)
        {
            return Err(task_error(
                "task_already_declared",
                &declaration.task_id,
                "task_id is already known to this server and cannot be restated",
            ));
        }

        let manifest_resolved_path = self
            .artifacts
            .resolve_new_path(&declaration.manifest_path)
            .map_err(artifact_fail)?;
        let manifest_parent = manifest_resolved_path
            .parent()
            .expect("a resolved destination always has a parent");

        let mut all_paths = HashSet::new();
        let mut input_states = Vec::with_capacity(declaration.inputs.len());
        let mut input_identities = Vec::with_capacity(declaration.inputs.len());
        for input in declaration.inputs {
            let source =
                self.read_source(&input.path, "task_read_only_source", self.max_doc_bytes())?;
            let identity = source.identity().clone();
            if !all_paths.insert(identity.resolved_path.clone()) {
                return Err(task_error(
                    "duplicate_task_path",
                    &declaration.task_id,
                    format!("path {:?} is declared more than once", input.path),
                ));
            }
            if identity.resolved_path == manifest_resolved_path {
                return Err(task_error(
                    "task_manifest_alias",
                    &declaration.task_id,
                    "manifest_path aliases a declared input",
                ));
            }
            input_states.push(TaskManifestInput {
                artifact: portable_artifact(manifest_parent, &identity)?,
                role: TaskInputRole::ReadOnlySource,
            });
            input_identities.push(identity);
        }

        let mut target_order = Vec::with_capacity(declaration.targets.len());
        let mut targets = HashMap::with_capacity(declaration.targets.len());
        let mut opened_source = None;
        let mut effect_ids = HashSet::new();
        let opened_resolved_path = self
            .read_source(opened_path, "task_open_target", self.max_doc_bytes())?
            .identity()
            .resolved_path
            .clone();
        for target in declaration.targets {
            if target.effects.is_empty() {
                return Err(task_error(
                    "invalid_task_declaration",
                    &declaration.task_id,
                    format!("target {:?} declares no effects", target.path),
                ));
            }
            let source =
                self.read_source(&target.path, "task_target_docx", self.max_doc_bytes())?;
            let identity = source.identity().clone();
            if !all_paths.insert(identity.resolved_path.clone()) {
                return Err(task_error(
                    "duplicate_task_path",
                    &declaration.task_id,
                    format!(
                        "target path {:?} aliases another declared path",
                        target.path
                    ),
                ));
            }
            if identity.resolved_path == manifest_resolved_path {
                return Err(task_error(
                    "task_manifest_alias",
                    &declaration.task_id,
                    "manifest_path aliases a declared target",
                ));
            }

            let mut effects = Vec::with_capacity(target.effects.len());
            for effect in target.effects {
                if !effect_ids.insert(effect.effect_id.clone()) {
                    return Err(task_error(
                        "duplicate_effect_id",
                        &declaration.task_id,
                        format!(
                            "effect_id {:?} is declared more than once",
                            effect.effect_id
                        ),
                    ));
                }
                effects.push(TaskEffectState {
                    declaration: effect.into_domain(&declaration.task_id)?,
                    outcome: EffectOutcome::Pending { last_failure: None },
                });
            }

            let resolved_path = identity.resolved_path.clone();
            if resolved_path == opened_resolved_path {
                opened_source = Some(source);
            }
            let portable_path = portable_path(manifest_parent, &resolved_path)?;
            let input_manifest = ManifestIdentity::from_identity(&identity).map_err(|error| {
                task_error(
                    "invalid_task_declaration",
                    &declaration.task_id,
                    error.to_string(),
                )
            })?;
            target_order.push(resolved_path.clone());
            targets.insert(
                resolved_path.clone(),
                TaskTargetState {
                    portable_path,
                    input_identity: identity,
                    input_manifest,
                    doc_id: None,
                    output: None,
                    output_identity: None,
                    audit_binding: None,
                    effects,
                },
            );
        }

        let Some(source) = opened_source else {
            return Err(task_error(
                "undeclared_task_target",
                &declaration.task_id,
                format!("open path {opened_path:?} is not one of the declared targets"),
            ));
        };
        Ok(PendingTaskDeclaration {
            task: TaskState {
                task_id: declaration.task_id,
                manifest_path: declaration.manifest_path,
                manifest_resolved_path,
                inputs: input_states,
                input_identities,
                target_order,
                targets,
                terminated: false,
            },
            source,
        })
    }

    pub(super) fn prepare_existing_task_open(
        &self,
        task_id: &str,
        path: &str,
    ) -> Result<ReadArtifact, CallToolResult> {
        let source = self.read_source(path, "task_target_docx", self.max_doc_bytes())?;
        let identity = source.identity();
        let registry = self.tasks.lock().expect("tasks mutex poisoned");
        let Some(task) = registry.tasks.get(task_id) else {
            return Err(task_error(
                "unknown_task_id",
                task_id,
                "declare the complete task on the first task-bearing open_docx call",
            ));
        };
        if task.terminated {
            return Err(task_error(
                "task_terminated",
                task_id,
                "a terminated task cannot open another session",
            ));
        }
        let Some(target) = task.targets.get(&identity.resolved_path) else {
            return Err(task_error(
                "undeclared_task_target",
                task_id,
                format!("path {path:?} is not a declared target"),
            ));
        };
        if let Some(doc_id) = &target.doc_id {
            return Err(task_error(
                "task_target_already_open",
                task_id,
                format!(
                    "target {path:?} is already bound to doc_id {doc_id:?}; one target has one live task session"
                ),
            ));
        }
        if identity.bytes != target.input_identity.bytes
            || identity.digest != target.input_identity.digest
        {
            return Err(task_error(
                "task_input_drift",
                task_id,
                format!(
                    "target {path:?} no longer matches its declaration-time hash (expected {}, got {})",
                    target.input_identity.digest.hex, identity.digest.hex
                ),
            ));
        }
        Ok(source)
    }

    pub(super) fn register_task_doc(
        &self,
        pending: Option<PendingTaskDeclaration>,
        task_id: Option<&str>,
        target_path: &Path,
        doc_id: &str,
    ) -> Result<(), CallToolResult> {
        let mut registry = self.tasks.lock().expect("tasks mutex poisoned");
        if let Some(mut pending) = pending {
            let id = pending.task.task_id.clone();
            let target = pending
                .task
                .targets
                .get_mut(target_path)
                .expect("prepared declaration contains opened target");
            target.doc_id = Some(doc_id.to_string());
            registry.docs.insert(
                doc_id.to_string(),
                TaskDocBinding {
                    task_id: id.clone(),
                    target_path: target_path.to_path_buf(),
                },
            );
            registry.tasks.insert(id, pending.task);
            return Ok(());
        }
        let task_id = task_id.expect("existing task registration carries task_id");
        let task = registry
            .tasks
            .get_mut(task_id)
            .expect("existing task was validated before import");
        let target = task
            .targets
            .get_mut(target_path)
            .expect("existing task target was validated before import");
        target.doc_id = Some(doc_id.to_string());
        registry.docs.insert(
            doc_id.to_string(),
            TaskDocBinding {
                task_id: task_id.to_string(),
                target_path: target_path.to_path_buf(),
            },
        );
        Ok(())
    }
}

fn commit_terminal_task(
    artifacts: &stemma_artifacts::PathAuthority,
    task: &mut TaskState,
    write_failure: Option<(&Path, &str)>,
) -> Result<(TaskManifestStatus, OutputArtifact, Vec<String>), CallToolResult> {
    let mut unsatisfied_effects = Vec::new();
    let mut manifest_targets = Vec::with_capacity(task.target_order.len());
    for path in &task.target_order {
        let target = task
            .targets
            .get(path)
            .expect("target order contains only declared targets");
        let missing_output_reason = if target.output.is_none() {
            Some(match write_failure {
                Some((failed_path, failure)) if failed_path == path => {
                    format!("target output was not committed: {failure}")
                }
                _ => "target output was not committed before task termination".to_string(),
            })
        } else {
            None
        };
        let effects = target
            .effects
            .iter()
            .map(|effect| {
                let (status, minted_revision_ids, reason) =
                    if let Some(reason) = &missing_output_reason {
                        unsatisfied_effects.push(effect.declaration.effect_id.clone());
                        (
                            TaskEffectStatus::Unsatisfied,
                            Vec::new(),
                            Some(reason.clone()),
                        )
                    } else {
                        match &effect.outcome {
                            EffectOutcome::Pending { last_failure } => {
                                unsatisfied_effects.push(effect.declaration.effect_id.clone());
                                (
                                    TaskEffectStatus::Unsatisfied,
                                    Vec::new(),
                                    Some(last_failure.clone().unwrap_or_else(|| {
                                        "no execute_plan named this effect".to_string()
                                    })),
                                )
                            }
                            EffectOutcome::Applied { revision_ids } => {
                                (TaskEffectStatus::Satisfied, revision_ids.clone(), None)
                            }
                            EffectOutcome::Unverifiable { reason } => {
                                unsatisfied_effects.push(effect.declaration.effect_id.clone());
                                (
                                    TaskEffectStatus::Unsatisfied,
                                    Vec::new(),
                                    Some(reason.clone()),
                                )
                            }
                        }
                    };
                TaskManifestEffect {
                    declaration: effect.declaration.clone(),
                    status,
                    minted_revision_ids,
                    reason,
                }
            })
            .collect();
        manifest_targets.push(TaskManifestTarget {
            path: target.portable_path.clone(),
            input: target.input_manifest.clone(),
            doc_id: target.doc_id.clone(),
            output: target.output.clone(),
            audit_binding: target.audit_binding.clone(),
            effects,
        });
    }
    let all_outputs_committed = task.targets.values().all(|target| target.output.is_some());
    let status = if all_outputs_committed && unsatisfied_effects.is_empty() {
        TaskManifestStatus::Complete
    } else {
        TaskManifestStatus::Partial
    };
    let manifest = TaskManifest {
        schema: TASK_MANIFEST_SCHEMA_V1.to_string(),
        task_id: task.task_id.clone(),
        status,
        producer: TaskManifestProducer {
            name: "stemma-mcp".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            build: SERVER_VERSION.to_string(),
        },
        inputs: task.inputs.clone(),
        targets: manifest_targets,
    };
    let mut protected_sources = task.input_identities.clone();
    protected_sources.extend(
        task.targets
            .values()
            .map(|target| target.input_identity.clone()),
    );
    protected_sources.extend(
        task.targets
            .values()
            .filter_map(|target| target.output_identity.clone()),
    );
    let committed = commit_task_manifest(
        artifacts,
        &task.manifest_path,
        &manifest,
        &protected_sources,
    )
    .map_err(|error| {
        task_error(
            "task_manifest_commit_failed",
            &task.task_id,
            format!(
                "the task reached a terminal state but its create-once manifest could not be committed: {error}"
            ),
        )
    })?;
    task.terminated = true;
    Ok((status, committed, unsatisfied_effects))
}

fn validate_item_against_declaration(
    item: &CoreReplacementItem,
    declaration: &DeclaredTaskEffect,
) -> Result<(), String> {
    if item.replace_all {
        return Err("replace_all is not declarable in task manifest v1; use an exact expected_matches count".to_string());
    }
    let expected_matches = item.expected_matches.unwrap_or(1);
    let scope = match item.scope.as_ref() {
        None => TaskReplacementScope::BodyAndTables,
        Some(scope) => match (
            scope.block_id.as_ref(),
            scope.from_block_id.as_ref(),
            scope.to_block_id.as_ref(),
        ) {
            (None, None, None) => TaskReplacementScope::BodyAndTables,
            (Some(block_id), None, None) => TaskReplacementScope::Block {
                block_id: block_id.clone(),
            },
            (None, Some(from_block_id), Some(to_block_id)) => TaskReplacementScope::Range {
                from_block_id: from_block_id.clone(),
                to_block_id: to_block_id.clone(),
            },
            _ => return Err("scope is not a valid block or complete range".to_string()),
        },
    };
    let match_mode = match item.match_mode {
        CoreReplacementMatchMode::Exact => TaskMatchMode::Exact,
        CoreReplacementMatchMode::NormalizeWs => TaskMatchMode::NormalizeWs,
    };
    let on_barrier_match = match item.on_barrier_match {
        CoreBarrierPolicy::Skip => TaskBarrierPolicy::Skip,
        CoreBarrierPolicy::Fail => TaskBarrierPolicy::Fail,
    };
    let mut mismatches = Vec::new();
    if item.old != declaration.find {
        mismatches.push("find");
    }
    if item.new != declaration.replace {
        mismatches.push("replace");
    }
    if match_mode != declaration.match_mode {
        mismatches.push("match_mode");
    }
    if scope != declaration.scope {
        mismatches.push("scope");
    }
    if expected_matches != declaration.expected_matches {
        mismatches.push("expected_matches");
    }
    if on_barrier_match != declaration.on_barrier_match {
        mismatches.push("on_barrier_match");
    }
    if mismatches.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "fields {} do not match the fixed declaration",
            mismatches.join(", ")
        ))
    }
}

impl TaskEffectArg {
    fn into_domain(self, task_id: &str) -> Result<DeclaredTaskEffect, CallToolResult> {
        let scope = match (
            self.scope.block_id,
            self.scope.from_block_id,
            self.scope.to_block_id,
        ) {
            (None, None, None) => TaskReplacementScope::BodyAndTables,
            (Some(block_id), None, None) => TaskReplacementScope::Block { block_id },
            (None, Some(from_block_id), Some(to_block_id)) => TaskReplacementScope::Range {
                from_block_id,
                to_block_id,
            },
            _ => {
                return Err(task_error(
                    "invalid_task_declaration",
                    task_id,
                    format!(
                        "effect {:?} scope must be empty, block_id, or a complete from/to range",
                        self.effect_id
                    ),
                ));
            }
        };
        let declaration = DeclaredTaskEffect {
            effect_id: self.effect_id,
            op: match self.op {
                TaskEffectOperationArg::ReplaceText => TaskEffectOperation::ReplaceText,
            },
            find: self.find,
            replace: self.replace,
            match_mode: match self.match_mode {
                TaskMatchModeArg::Exact => TaskMatchMode::Exact,
                TaskMatchModeArg::NormalizeWs => TaskMatchMode::NormalizeWs,
            },
            scope,
            expected_matches: self.expected_matches,
            on_barrier_match: match self.on_barrier_match {
                TaskBarrierPolicyArg::Skip => TaskBarrierPolicy::Skip,
                TaskBarrierPolicyArg::Fail => TaskBarrierPolicy::Fail,
            },
        };
        if declaration.effect_id.trim().is_empty()
            || declaration.find.is_empty()
            || declaration.find == declaration.replace
            || declaration.expected_matches == 0
        {
            return Err(task_error(
                "invalid_task_declaration",
                task_id,
                format!(
                    "effect {:?} requires non-empty effect_id/find, different find/replace text, and expected_matches > 0",
                    declaration.effect_id
                ),
            ));
        }
        Ok(declaration)
    }
}

fn portable_artifact(
    manifest_parent: &Path,
    identity: &ArtifactIdentity,
) -> Result<ManifestArtifact, CallToolResult> {
    let path = portable_path(manifest_parent, &identity.resolved_path)?;
    ManifestArtifact::from_identity(path, identity).map_err(|error| {
        fail_json(json!({"code": "invalid_task_artifact", "error": error.to_string()}))
    })
}

pub(super) fn portable_path(
    manifest_parent: &Path,
    artifact_path: &Path,
) -> Result<PathBuf, CallToolResult> {
    pathdiff::diff_paths(artifact_path, manifest_parent).ok_or_else(|| {
        fail_json(json!({
            "code": "task_path_not_portable",
            "error": format!(
                "cannot express artifact path {:?} relative to manifest directory {:?}",
                artifact_path, manifest_parent
            ),
        }))
    })
}

fn task_error(code: &str, task_id: &str, message: impl Into<String>) -> CallToolResult {
    fail_json(json!({
        "code": code,
        "error": message.into(),
        "task_id": task_id,
    }))
}
