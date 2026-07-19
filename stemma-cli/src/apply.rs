use std::collections::{BTreeSet, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use stemma::api::Document;
use stemma::audit::RevisionDisposition;
use stemma::edit::{
    BarrierPolicy, EditTransaction, ExpectedMatches, MatchMode, MaterializationMode,
    ReplaceTextError, ReplaceTextOptions, ReplaceTextScope, plan_replace_text,
    unreached_cell_matches,
};
use stemma::tracked_model::enumerate_revisions;
use stemma::{ErrorCode, ExportOptions, RevisionInfo};
use stemma_artifacts::{ArtifactIdentity, CollisionPolicy, OutputArtifact, PathAuthority};

const WORKLIST_SCHEMA: &str = "stemma.worklist.v0";
const RECEIPT_SCHEMA: &str = "stemma.apply_receipt.v0";
const MAX_WORKLIST_BYTES: u64 = 1024 * 1024;
const MAX_CHANGES: usize = 100;
const BUILD_STAMP: &str = match option_env!("STEMMA_BUILD_STAMP") {
    Some(stamp) => stamp,
    None => env!("CARGO_PKG_VERSION"),
};

#[derive(Debug)]
pub(crate) enum ApplyStatus {
    Complete,
    Incomplete,
}

impl ApplyStatus {
    pub(crate) fn exit_code(self) -> ExitCode {
        match self {
            ApplyStatus::Complete => ExitCode::SUCCESS,
            // Exit 2 belongs to clap usage errors. Exit 3 means the command ran
            // and emitted a receipt, but at least one worklist item was refused.
            ApplyStatus::Incomplete => ExitCode::from(3),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorklistV0 {
    schema: String,
    input: InputBindingV0,
    author: String,
    changes: Vec<ChangeV0>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputBindingV0 {
    sha256: String,
    bytes: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangeV0 {
    id: String,
    old: String,
    new: String,
    #[serde(default = "default_expected_matches")]
    expected_matches: usize,
    #[serde(default)]
    scope: Option<ScopeV0>,
    #[serde(default)]
    match_mode: MatchModeV0,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopeV0 {
    #[serde(default)]
    block_id: Option<String>,
    #[serde(default)]
    from_block_id: Option<String>,
    #[serde(default)]
    to_block_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum MatchModeV0 {
    #[default]
    Exact,
    NormalizeWs,
}

#[derive(Debug)]
struct PreparedWorklist {
    input: InputBindingV0,
    author: String,
    changes: Vec<PreparedChange>,
}

#[derive(Debug)]
struct PreparedChange {
    id: String,
    old: String,
    new: String,
    expected_matches: usize,
    scope: ReplaceTextScope,
    receipt_scope: ScopeReceipt,
    match_mode: MatchModeV0,
    engine_match_mode: MatchMode,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ScopeReceipt {
    AllTopLevelBodyParagraphs,
    Block {
        block_id: String,
    },
    Range {
        from_block_id: String,
        to_block_id: String,
    },
}

#[derive(Serialize)]
struct ApplyReceipt {
    schema: &'static str,
    producer: ProducerReceipt,
    status: ReceiptStatus,
    deliverable: bool,
    emit_partial_requested: bool,
    input: ArtifactIdentity,
    worklist: ArtifactIdentity,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<OutputReceipt>,
    summary: SummaryReceipt,
    coverage: CoverageReceipt,
    items: CompleteItemReceipts,
    #[serde(skip_serializing_if = "Option::is_none")]
    verification: Option<VerificationReceipt>,
}

#[derive(Serialize)]
struct ProducerReceipt {
    name: &'static str,
    version: &'static str,
    build: &'static str,
    executable: ExecutableReceipt,
    ruleset: &'static str,
    verification_profile: &'static str,
}

impl ProducerReceipt {
    fn collect() -> Result<Self, String> {
        Ok(Self {
            name: "stemma-cli",
            version: env!("CARGO_PKG_VERSION"),
            build: BUILD_STAMP,
            executable: executable_receipt()?,
            ruleset: "stemma.worklist.v0",
            verification_profile: "stemma.worklist_verification.v0",
        })
    }
}

#[derive(Serialize)]
struct ExecutableReceipt {
    digest: OutputDigestReceipt,
    bytes: u64,
}

#[derive(Clone, Serialize)]
struct OutputReceipt {
    role: &'static str,
    supplied_path: PathBuf,
    digest: OutputDigestReceipt,
    bytes: u64,
    collision_policy: CollisionPolicy,
    persistence_confirmation: PersistenceConfirmationReceipt,
}

#[derive(Clone, Serialize)]
struct OutputDigestReceipt {
    algorithm: &'static str,
    hex: String,
}

#[derive(Clone, Serialize)]
struct PersistenceConfirmationReceipt {
    required_process_exit: u8,
    requires_output_presence: bool,
    requires_identity_match: bool,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum ReceiptStatus {
    Complete,
    Partial,
}

#[derive(Serialize)]
struct SummaryReceipt {
    total: usize,
    applied: usize,
    refused: usize,
}

#[derive(Serialize)]
struct CoverageReceipt {
    supported: [&'static str; 1],
    default_scope: &'static str,
    conditional_detection: [&'static str; 1],
    unsearched: [&'static str; 7],
}

impl Default for CoverageReceipt {
    fn default() -> Self {
        Self {
            supported: ["top_level_body_paragraphs"],
            default_scope: "all_top_level_body_paragraphs",
            conditional_detection: ["top_level_table_cells_for_default_scope"],
            unsearched: [
                "nested_table_cells",
                "headers",
                "footers",
                "footnotes",
                "endnotes",
                "comments",
                "textboxes",
            ],
        }
    }
}

#[derive(Serialize)]
struct ItemReceipt {
    id: String,
    status: ItemStatus,
    expected_matches: usize,
    scope: ScopeReceipt,
    match_mode: MatchModeV0,
    #[serde(skip_serializing_if = "Option::is_none")]
    actual_matches: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    matches: Vec<MatchReceipt>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    changed_block_ids: Vec<String>,
    created_revisions: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    normalization_applied: Vec<&'static str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unreachable_matches: Vec<UnreachableMatchReceipt>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    diagnosis: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

/// Worklist outcomes are decision-plane data: every submitted item must remain
/// inline. This transparent wrapper deliberately has no cap/paging operation;
/// construction asserts the one-submission/one-outcome invariant.
#[derive(Serialize)]
#[serde(transparent)]
struct CompleteItemReceipts(Vec<ItemReceipt>);

impl CompleteItemReceipts {
    fn new(submitted: usize, items: Vec<ItemReceipt>) -> Self {
        assert_eq!(
            items.len(),
            submitted,
            "worklist outcome count must equal submitted item count"
        );
        Self(items)
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum ItemStatus {
    Applied,
    Refused,
}

#[derive(Serialize)]
struct MatchReceipt {
    block_id: String,
    excerpt: String,
}

#[derive(Serialize)]
struct UnreachableMatchReceipt {
    region: &'static str,
    block_id: String,
    row: usize,
    col: usize,
    excerpt: String,
}

#[derive(Serialize)]
struct VerificationReceipt {
    profile: &'static str,
    validator_level: &'static str,
    validator_ok: bool,
    direct_changes: usize,
    untouched_violations: usize,
    untouched_verified_blocks: usize,
    untouched_parts: Vec<&'static str>,
    preexisting_revisions_preserved: usize,
    new_revisions: usize,
}

pub(crate) fn apply_worklist(
    artifacts: &PathAuthority,
    input_path: &Path,
    worklist_path: &Path,
    output_path: &Path,
    receipt_path: Option<&Path>,
    emit_partial: bool,
) -> Result<ApplyStatus, String> {
    apply_worklist_with_before_output_commit(
        artifacts,
        input_path,
        worklist_path,
        output_path,
        receipt_path,
        emit_partial,
        || Ok(()),
    )
}

fn apply_worklist_with_before_output_commit<F>(
    artifacts: &PathAuthority,
    input_path: &Path,
    worklist_path: &Path,
    output_path: &Path,
    receipt_path: Option<&Path>,
    emit_partial: bool,
    before_output_commit: F,
) -> Result<ApplyStatus, String>
where
    F: FnOnce() -> Result<(), String>,
{
    let input = artifacts
        .read_source(input_path, "input_docx", None)
        .map_err(|e| e.to_string())?;
    let mut document = Document::parse(input.bytes())
        .map_err(|e| format!("{}: not a valid DOCX ({e})", input_path.display()))?;
    let input_identity = input.identity().clone();

    let worklist_artifact = artifacts
        .read_source(worklist_path, "input_worklist", Some(MAX_WORKLIST_BYTES))
        .map_err(|e| e.to_string())?;
    let worklist_identity = worklist_artifact.identity().clone();
    let worklist = parse_worklist(worklist_artifact.bytes(), worklist_path)?;
    verify_input_binding(&worklist.input, &input_identity, worklist_path)?;
    let receipt_path = receipt_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_receipt_path(output_path));
    if destinations_are_same(&receipt_path, output_path)? {
        return Err(
            "receipt path and DOCX output path resolve to the same destination".to_string(),
        );
    }
    match std::fs::symlink_metadata(output_path) {
        Ok(_) => {
            return Err(format!(
                "refusing to replace existing output {}",
                output_path.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "cannot inspect output destination {}: {error}",
                output_path.display()
            ));
        }
    }

    let submitted = worklist.changes.len();
    let mut outcomes = Vec::with_capacity(submitted);
    let mut attributed_revision_ids = BTreeSet::new();
    for change in worklist.changes {
        let unreachable = if matches!(change.scope, ReplaceTextScope::WholeDoc) {
            unreached_cell_matches(
                document.snapshot().canonical.as_ref(),
                &change.old,
                change.engine_match_mode,
            )
        } else {
            Vec::new()
        };
        if !unreachable.is_empty() {
            outcomes.push(ItemReceipt {
                id: change.id,
                status: ItemStatus::Refused,
                expected_matches: change.expected_matches,
                scope: change.receipt_scope,
                match_mode: change.match_mode,
                actual_matches: None,
                matches: Vec::new(),
                changed_block_ids: Vec::new(),
                created_revisions: 0,
                normalization_applied: Vec::new(),
                unreachable_matches: unreachable
                    .into_iter()
                    .map(|site| UnreachableMatchReceipt {
                        region: "table_cell",
                        block_id: site.table_id.to_string(),
                        row: site.row,
                        col: site.col,
                        excerpt: site.excerpt,
                    })
                    .collect(),
                diagnosis: Vec::new(),
                code: Some("unreachable_match".to_string()),
                message: Some(
                    "the requested text also occurs in a table cell outside worklist.v0 coverage"
                        .to_string(),
                ),
            });
            continue;
        }

        let options = ReplaceTextOptions {
            old: change.old,
            new: change.new,
            author: worklist.author.clone(),
            scope: change.scope,
            expected: ExpectedMatches::Count(change.expected_matches),
            match_mode: change.engine_match_mode,
            on_barrier_match: BarrierPolicy::Fail,
        };
        let plan = match plan_replace_text(document.snapshot().canonical.as_ref(), &options) {
            Ok(plan) => plan,
            Err(ReplaceTextError::MatchCountMismatch {
                expected: _,
                actual,
                sites,
                diagnosis,
            }) => {
                outcomes.push(ItemReceipt {
                    id: change.id,
                    status: ItemStatus::Refused,
                    expected_matches: change.expected_matches,
                    scope: change.receipt_scope,
                    match_mode: change.match_mode,
                    actual_matches: Some(actual),
                    matches: sites
                        .into_iter()
                        .map(|site| MatchReceipt {
                            block_id: site.block_id.to_string(),
                            excerpt: site.excerpt,
                        })
                        .collect(),
                    changed_block_ids: Vec::new(),
                    created_revisions: 0,
                    normalization_applied: Vec::new(),
                    unreachable_matches: Vec::new(),
                    diagnosis,
                    code: Some("match_count_mismatch".to_string()),
                    message: Some(format!(
                        "expected {} body match(es), found {actual}; if one site is \
                         intended, narrow old_text or add scope to single it out — raise \
                         expected_matches only after verifying every listed match is \
                         intended",
                        change.expected_matches
                    )),
                });
                continue;
            }
            Err(ReplaceTextError::Engine(error)) => {
                outcomes.push(refused_engine_item(
                    change.id,
                    change.expected_matches,
                    change.receipt_scope,
                    change.match_mode,
                    "planning_refused",
                    error.to_string(),
                ));
                continue;
            }
        };
        if plan.steps.is_empty() {
            outcomes.push(refused_engine_item(
                change.id,
                change.expected_matches,
                change.receipt_scope,
                change.match_mode,
                "no_op",
                "the requested replacement would not change the document".to_string(),
            ));
            continue;
        }

        let matches: Vec<MatchReceipt> = plan
            .matches
            .iter()
            .map(|site| MatchReceipt {
                block_id: site.block_id.to_string(),
                excerpt: site.excerpt.clone(),
            })
            .collect();
        let changed_block_ids: Vec<String> = plan
            .matches
            .iter()
            .map(|site| site.block_id.to_string())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let normalization_applied = plan
            .normalization_applied
            .iter()
            .map(|class| class.as_str())
            .collect();
        let before_revision_ids = revision_ids(&document);
        let transaction = EditTransaction {
            steps: plan.steps,
            summary: Some(format!("approved worklist item {}", change.id)),
            materialization_mode: MaterializationMode::TrackedChange,
            revision: RevisionInfo {
                revision_id: 0,
                identity: 0,
                author: Some(worklist.author.clone()),
                date: None,
                apply_op_id: Some(change.id.clone()),
            },
        };
        let edited = match document.apply_authored(&transaction, false) {
            Ok(edited) => edited,
            Err(error) => {
                outcomes.push(refused_engine_item(
                    change.id,
                    change.expected_matches,
                    change.receipt_scope,
                    change.match_mode,
                    runtime_error_code(error.code),
                    error.message,
                ));
                continue;
            }
        };
        let after_revision_ids = revision_ids(&edited);
        let created_revision_ids: Vec<u32> = after_revision_ids
            .difference(&before_revision_ids)
            .copied()
            .collect();
        if created_revision_ids.is_empty() {
            return Err(format!(
                "worklist item {:?} changed the document without creating a tracked revision",
                change.id
            ));
        }
        attributed_revision_ids.extend(created_revision_ids.iter().copied());
        document = edited;
        outcomes.push(ItemReceipt {
            id: change.id,
            status: ItemStatus::Applied,
            expected_matches: change.expected_matches,
            scope: change.receipt_scope,
            match_mode: change.match_mode,
            actual_matches: Some(matches.len()),
            matches,
            changed_block_ids,
            created_revisions: created_revision_ids.len(),
            normalization_applied,
            unreachable_matches: Vec::new(),
            diagnosis: Vec::new(),
            code: None,
            message: None,
        });
    }

    let applied = outcomes
        .iter()
        .filter(|item| matches!(item.status, ItemStatus::Applied))
        .count();
    let refused = outcomes.len() - applied;
    let status = if refused == 0 {
        ReceiptStatus::Complete
    } else {
        ReceiptStatus::Partial
    };
    let deliverable = refused == 0;
    let should_emit_output = applied > 0 && (deliverable || emit_partial);

    let verification = if applied == 0 {
        None
    } else {
        let report = document
            .review()
            .map_err(|e| format!("cannot verify worklist result: {e}"))?;
        verify_report(&report)?;
        let audited_revision_ids: BTreeSet<u32> = report
            .new_revisions
            .iter()
            .map(|revision| revision.revision_id)
            .collect();
        if audited_revision_ids != attributed_revision_ids {
            return Err(format!(
                "worklist receipt mismatch: attributed revision count {} differs from audit count {}",
                attributed_revision_ids.len(),
                audited_revision_ids.len()
            ));
        }
        Some(VerificationReceipt {
            profile: "stemma.worklist_verification.v0",
            validator_level: "blocking",
            validator_ok: report.validator.ok,
            direct_changes: report.direct_changes.len(),
            untouched_violations: report.untouched.violations.len(),
            untouched_verified_blocks: report.untouched.verified_blocks,
            untouched_parts: report.untouched.parts,
            preexisting_revisions_preserved: report.preexisting_revisions.len(),
            new_revisions: audited_revision_ids.len(),
        })
    };

    let serialized_output =
        if should_emit_output {
            Some(document.serialize(&ExportOptions::default()).map_err(|e| {
                format!("cannot serialize output for {}: {e}", output_path.display())
            })?)
        } else {
            None
        };
    let output_receipt = serialized_output.as_deref().map(|bytes| OutputReceipt {
        role: if deliverable {
            "output_redline"
        } else {
            "output_partial_redline"
        },
        supplied_path: output_path.to_path_buf(),
        digest: digest_bytes(bytes),
        bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        collision_policy: CollisionPolicy::CreateNew,
        persistence_confirmation: PersistenceConfirmationReceipt {
            required_process_exit: if deliverable { 0 } else { 3 },
            requires_output_presence: true,
            requires_identity_match: true,
        },
    });
    let items = CompleteItemReceipts::new(submitted, outcomes);
    let receipt = ApplyReceipt {
        schema: RECEIPT_SCHEMA,
        producer: ProducerReceipt::collect()?,
        status,
        deliverable,
        emit_partial_requested: emit_partial,
        input: input_identity,
        worklist: worklist_identity,
        output: output_receipt.clone(),
        summary: SummaryReceipt {
            total: submitted,
            applied,
            refused,
        },
        coverage: CoverageReceipt::default(),
        items,
        verification,
    };
    let receipt_json = encode_receipt(&receipt)?;
    let receipt_output = artifacts
        .commit_new(
            &receipt_path,
            "output_receipt",
            receipt_json.as_bytes(),
            &[receipt.input.clone(), receipt.worklist.clone()],
        )
        .map_err(|e| e.to_string())?;

    let committed_output = if let (Some(bytes), Some(expected)) =
        (serialized_output.as_deref(), output_receipt.as_ref())
    {
        before_output_commit().map_err(|error| {
            format!(
                "{error}; diagnostic receipt remains at {}",
                receipt_path.display()
            )
        })?;
        let committed = artifacts
            .commit_new(
                output_path,
                expected.role,
                bytes,
                &[
                    receipt.input.clone(),
                    receipt.worklist.clone(),
                    receipt_output.identity.clone(),
                ],
            )
            .map_err(|e| {
                format!(
                    "{e}; diagnostic receipt remains at {}",
                    receipt_path.display()
                )
            })?;
        if committed.identity.digest.hex != expected.digest.hex
            || committed.identity.bytes != expected.bytes
        {
            return Err("committed output identity differs from its durable receipt".to_string());
        }
        Some(committed)
    } else {
        None
    };

    emit_receipt(&receipt_json, &receipt_path);
    let receipt_summary = format!(
        "receipt={} bytes={} sha256={}",
        receipt_path.display(),
        receipt_output.identity.bytes,
        receipt_output.identity.digest.hex
    );
    if let Some(output) = committed_output {
        eprintln!(
            "wrote {} to {} ({applied} applied, {refused} refused); {}; {receipt_summary}",
            if deliverable {
                "approved-worklist redline"
            } else {
                "non-deliverable partial redline"
            },
            output_path.display(),
            output_summary(&output),
        );
    } else {
        eprintln!(
            "worklist incomplete: {applied} applied, {refused} refused; no redline created; {receipt_summary}"
        );
    }

    Ok(if deliverable {
        ApplyStatus::Complete
    } else {
        ApplyStatus::Incomplete
    })
}

fn parse_worklist(bytes: &[u8], path: &Path) -> Result<PreparedWorklist, String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|e| format!("{}: worklist is not UTF-8 JSON ({e})", path.display()))?;
    let worklist: WorklistV0 = serde_json::from_str(text)
        .map_err(|e| format!("{}: invalid worklist JSON ({e})", path.display()))?;
    if worklist.schema != WORKLIST_SCHEMA {
        return Err(format!(
            "{}: unsupported worklist schema {:?}; expected {WORKLIST_SCHEMA:?}",
            path.display(),
            worklist.schema
        ));
    }
    validate_input_binding(&worklist.input, path)?;
    if worklist.author.trim().is_empty() {
        return Err(format!(
            "{}: worklist author must be non-empty",
            path.display()
        ));
    }
    if worklist.changes.is_empty() {
        return Err(format!(
            "{}: worklist changes must be non-empty",
            path.display()
        ));
    }
    if worklist.changes.len() > MAX_CHANGES {
        return Err(format!(
            "{}: worklist has {} changes; v0 allows at most {MAX_CHANGES}",
            path.display(),
            worklist.changes.len()
        ));
    }

    let mut ids = HashSet::new();
    let mut changes = Vec::with_capacity(worklist.changes.len());
    for (index, change) in worklist.changes.into_iter().enumerate() {
        if change.id.trim().is_empty() {
            return Err(format!(
                "{}: changes[{index}].id must be non-empty",
                path.display()
            ));
        }
        if !ids.insert(change.id.clone()) {
            return Err(format!(
                "{}: duplicate worklist item id {:?}",
                path.display(),
                change.id
            ));
        }
        if change.old.is_empty() {
            return Err(format!(
                "{}: changes[{index}].old must be non-empty",
                path.display()
            ));
        }
        if change.old == change.new {
            return Err(format!(
                "{}: changes[{index}] has identical old and new text",
                path.display()
            ));
        }
        if change.expected_matches == 0 {
            return Err(format!(
                "{}: changes[{index}].expected_matches must be positive",
                path.display()
            ));
        }
        let (scope, receipt_scope) = parse_scope(change.scope, path, index)?;
        let engine_match_mode = match change.match_mode {
            MatchModeV0::Exact => MatchMode::Exact,
            MatchModeV0::NormalizeWs => MatchMode::NormalizeWs,
        };
        changes.push(PreparedChange {
            id: change.id,
            old: change.old,
            new: change.new,
            expected_matches: change.expected_matches,
            scope,
            receipt_scope,
            match_mode: change.match_mode,
            engine_match_mode,
        });
    }
    Ok(PreparedWorklist {
        input: worklist.input,
        author: worklist.author,
        changes,
    })
}

fn validate_input_binding(binding: &InputBindingV0, path: &Path) -> Result<(), String> {
    if binding.sha256.len() != 64
        || !binding
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(format!(
            "{}: input.sha256 must be 64 lowercase hexadecimal characters",
            path.display()
        ));
    }
    Ok(())
}

fn verify_input_binding(
    expected: &InputBindingV0,
    actual: &ArtifactIdentity,
    path: &Path,
) -> Result<(), String> {
    if expected.sha256 != actual.digest.hex || expected.bytes != actual.bytes {
        return Err(format!(
            "{}: input binding mismatch (expected bytes={} sha256={}, actual bytes={} sha256={})",
            path.display(),
            expected.bytes,
            expected.sha256,
            actual.bytes,
            actual.digest.hex
        ));
    }
    Ok(())
}

fn default_receipt_path(output_path: &Path) -> PathBuf {
    let mut receipt = output_path.as_os_str().to_os_string();
    receipt.push(".receipt.json");
    PathBuf::from(receipt)
}

fn destinations_are_same(left: &Path, right: &Path) -> Result<bool, String> {
    let left = resolve_destination_for_comparison(left)?;
    let right = resolve_destination_for_comparison(right)?;
    if left == right {
        return Ok(true);
    }
    #[cfg(windows)]
    {
        return Ok(left
            .to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy()));
    }
    #[cfg(not(windows))]
    Ok(false)
}

fn resolve_destination_for_comparison(path: &Path) -> Result<PathBuf, String> {
    let file_name = path
        .file_name()
        .ok_or_else(|| format!("destination {} has no file name", path.display()))?;
    let parent = path.parent().filter(|part| !part.as_os_str().is_empty());
    let parent = parent.unwrap_or_else(|| Path::new("."));
    let resolved_parent = std::fs::canonicalize(parent).map_err(|error| {
        format!(
            "cannot resolve destination parent {}: {error}",
            parent.display()
        )
    })?;
    Ok(resolved_parent.join(file_name))
}

fn parse_scope(
    scope: Option<ScopeV0>,
    path: &Path,
    index: usize,
) -> Result<(ReplaceTextScope, ScopeReceipt), String> {
    let Some(scope) = scope else {
        return Ok((
            ReplaceTextScope::WholeDoc,
            ScopeReceipt::AllTopLevelBodyParagraphs,
        ));
    };
    match (scope.block_id, scope.from_block_id, scope.to_block_id) {
        (Some(block_id), None, None) if !block_id.is_empty() => Ok((
            ReplaceTextScope::SingleBlock(stemma::NodeId::from(block_id.as_str())),
            ScopeReceipt::Block { block_id },
        )),
        (None, Some(from), Some(to)) if !from.is_empty() && !to.is_empty() => Ok((
            ReplaceTextScope::Range {
                from: stemma::NodeId::from(from.as_str()),
                to: stemma::NodeId::from(to.as_str()),
            },
            ScopeReceipt::Range {
                from_block_id: from,
                to_block_id: to,
            },
        )),
        _ => Err(format!(
            "{}: changes[{index}].scope must contain either block_id or both from_block_id and to_block_id",
            path.display()
        )),
    }
}

fn revision_ids(document: &Document) -> BTreeSet<u32> {
    enumerate_revisions(document.snapshot().canonical.as_ref())
        .into_iter()
        .map(|revision| revision.revision_id)
        .collect()
}

fn refused_engine_item(
    id: String,
    expected_matches: usize,
    scope: ScopeReceipt,
    match_mode: MatchModeV0,
    code: &str,
    message: String,
) -> ItemReceipt {
    ItemReceipt {
        id,
        status: ItemStatus::Refused,
        expected_matches,
        scope,
        match_mode,
        actual_matches: None,
        matches: Vec::new(),
        changed_block_ids: Vec::new(),
        created_revisions: 0,
        normalization_applied: Vec::new(),
        unreachable_matches: Vec::new(),
        diagnosis: Vec::new(),
        code: Some(code.to_string()),
        message: Some(message),
    }
}

fn runtime_error_code(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::StaleEdit => "stale_edit",
        ErrorCode::UnsupportedEdit => "unsupported_edit",
        ErrorCode::AnchorNotFound => "anchor_not_found",
        ErrorCode::InvalidRange => "invalid_range",
        ErrorCode::OpaqueDestroyed => "opaque_destroyed",
        ErrorCode::NoOpEdit => "no_op_edit",
        ErrorCode::PrefixDuplicatesLabel => "prefix_duplicates_label",
        ErrorCode::AmbiguousAnchorAfterMove => "ambiguous_anchor_after_move",
        ErrorCode::InvalidDocx => "invalid_docx",
        ErrorCode::InvalidSnapshot => "invalid_snapshot",
        ErrorCode::InternalError => "internal_error",
        ErrorCode::ValidationFailed => "validation_failed",
        ErrorCode::AuthorImpersonation => "author_impersonation",
    }
}

fn verify_report(report: &stemma::audit::AuditReport) -> Result<(), String> {
    if !report.validator.ok {
        return Err("worklist result failed structural validation".to_string());
    }
    if !report.direct_changes.is_empty() {
        return Err(format!(
            "worklist result contains {} untracked direct change(s)",
            report.direct_changes.len()
        ));
    }
    if !report.untouched.violations.is_empty() {
        return Err(format!(
            "worklist result contains {} unexplained untouched-scope violation(s)",
            report.untouched.violations.len()
        ));
    }
    let changed_preexisting = report
        .preexisting_revisions
        .iter()
        .filter(|revision| !matches!(revision.disposition, RevisionDisposition::Untouched))
        .count();
    if changed_preexisting != 0 {
        return Err(format!(
            "worklist result changed or resolved {changed_preexisting} pre-existing revision(s)"
        ));
    }
    Ok(())
}

fn digest_bytes(bytes: &[u8]) -> OutputDigestReceipt {
    OutputDigestReceipt {
        algorithm: "sha256",
        hex: format!("{:x}", Sha256::digest(bytes)),
    }
}

fn executable_receipt() -> Result<ExecutableReceipt, String> {
    let path = std::env::current_exe()
        .map_err(|error| format!("cannot identify the running stemma executable: {error}"))?;
    let mut file = std::fs::File::open(&path)
        .map_err(|error| format!("cannot open the running stemma executable: {error}"))?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("cannot hash the running stemma executable: {error}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        total = total
            .checked_add(u64::try_from(read).unwrap_or(u64::MAX))
            .ok_or_else(|| "running stemma executable is too large to identify".to_string())?;
    }
    Ok(ExecutableReceipt {
        digest: OutputDigestReceipt {
            algorithm: "sha256",
            hex: format!("{:x}", hasher.finalize()),
        },
        bytes: total,
    })
}

fn encode_receipt(receipt: &ApplyReceipt) -> Result<String, String> {
    serde_json::to_string_pretty(receipt)
        .map(|mut json| {
            json.push('\n');
            json
        })
        .map_err(|e| format!("cannot encode apply receipt: {e}"))
}

fn emit_receipt(json: &str, receipt_path: &Path) {
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    if let Err(error) = handle.write_all(json.as_bytes()) {
        eprintln!(
            "warning: cannot mirror apply receipt to stdout ({error}); durable receipt is at {}",
            receipt_path.display()
        );
    }
}

fn output_summary(output: &OutputArtifact) -> String {
    format!(
        "bytes={} sha256={} collision_policy=create_new disposition=created",
        output.identity.bytes, output.identity.digest.hex
    )
}

fn default_expected_matches() -> usize {
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_receipt_failure_leaves_only_an_unconfirmed_diagnostic_receipt() {
        let directory = tempfile::tempdir().unwrap();
        let input_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../stemma-engine/testdata/simple-text/before.docx");
        let worklist_path = directory.path().join("worklist.json");
        let output_path = directory.path().join("must-not-exist.docx");
        let receipt_path = directory.path().join("diagnostic-receipt.json");
        let artifacts = PathAuthority::explicit().unwrap();
        let input = artifacts
            .read_source(&input_path, "test_input", None)
            .unwrap();
        let worklist = serde_json::json!({
            "schema": WORKLIST_SCHEMA,
            "input": {
                "sha256": input.identity().digest.hex,
                "bytes": input.identity().bytes,
            },
            "author": "Approved Reviewer",
            "changes": [{
                "id": "change-1",
                "old": "foo bar",
                "new": "review-ready language"
            }]
        });
        std::fs::write(
            &worklist_path,
            serde_json::to_vec_pretty(&worklist).unwrap(),
        )
        .unwrap();

        let error = apply_worklist_with_before_output_commit(
            &artifacts,
            &input_path,
            &worklist_path,
            &output_path,
            Some(&receipt_path),
            false,
            || Err("injected failure before DOCX commit".to_string()),
        )
        .expect_err("the injected boundary must fail the command");

        assert!(error.contains("injected failure before DOCX commit"));
        assert!(error.contains("diagnostic receipt remains"));
        assert!(!output_path.exists());
        let receipt: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&receipt_path).unwrap()).unwrap();
        assert_eq!(receipt["status"], "complete");
        assert_eq!(receipt["deliverable"], true);
        assert_eq!(
            receipt["output"]["persistence_confirmation"]["required_process_exit"],
            0
        );
        assert_eq!(
            receipt["output"]["persistence_confirmation"]["requires_output_presence"],
            true
        );
    }
}
