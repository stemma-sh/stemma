//! `stemma` — a thin command-line interface to the DOCX engine.
//!
//! The focused path applies an approved worklist to an existing DOCX as native
//! tracked changes. Maintenance verbs compare, extract, read, resolve, and
//! validate.
//!
//! Design contract (CLAUDE.md): parse at the edges, no silent fallbacks. Every
//! failure exits nonzero with a one-line actionable message on stderr naming
//! what failed and which file/id; user input never panics. stdout carries data,
//! stderr carries diagnostics. General verbs drive the stable
//! [`stemma::api::Document`] facade. The experimental worklist command also uses
//! the tracked-native replacement planner until field evidence justifies a
//! shared application facade.

mod apply;
mod verify_task;

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{ArgGroup, Parser, Subcommand};
use serde::Serialize;
use serde_json::json;
use stemma::api::{BlockRole, Document, validate};
use stemma::audit::RevisionDisposition;
use stemma::tracked_model::RevisionKind;
use stemma::{ExportOptions, Resolution, ResolveSelectionAction};
use stemma_artifacts::{
    ArtifactDisposition, ArtifactIdentity, CollisionPolicy, DigestAlgorithm, OutputArtifact,
    PathAuthority,
};

/// `compare --author NAME` attributes every discovered revision to NAME
/// (`diff_as`); omitting it leaves the redline anonymous (`diff`). See the
/// `--author` note in `docs/reference/cli.md`.
#[derive(Parser)]
#[command(
    name = "stemma",
    version,
    about = "Compact inspect, execute, and verify workflows for tracked-change DOCX.",
    long_about = "Inspect a DOCX through compact revision-aware Markdown, execute an \
                  exact-input-bound plan as native tracked changes, independently verify \
                  any before/after pair, and access maintenance compare/extract/resolve verbs."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Apply an explicit approved worklist and create a native Word redline.
    #[command(visible_alias = "execute")]
    Apply {
        /// The existing document to change. It is never modified.
        input: PathBuf,
        /// A stemma.worklist.v0 JSON file.
        #[arg(long, visible_alias = "plan", value_name = "FILE")]
        worklist: PathBuf,
        /// Where to create the redline DOCX. Refuses any existing path or input.
        #[arg(short = 'o', long = "out")]
        out: PathBuf,
        /// Durable JSON receipt path (default: <out>.receipt.json).
        #[arg(long, value_name = "FILE")]
        receipt: Option<PathBuf>,
        /// Create a non-deliverable partial redline when any item is refused.
        #[arg(long)]
        emit_partial: bool,
    },

    /// Inspect a DOCX through the compact, revision-aware agent projection.
    Inspect {
        /// The document to inspect.
        file: PathBuf,
        /// Output format. Markdown is the compact default; JSON wraps the same
        /// projection with its exact input identity and summary.
        #[arg(long, value_enum, default_value_t = InspectFormat::Markdown)]
        format: InspectFormat,
    },

    /// Verify a before/after pair under the tracked-delivery policy.
    Verify {
        /// The protected baseline document.
        before: PathBuf,
        /// The result to verify. It may have been produced by any tool.
        after: PathBuf,
        /// Verification policy. v0 requires valid tracked-only change,
        /// preservation of pending revisions, and a clean untouched proof.
        #[arg(long, value_enum, default_value_t = VerifyPolicy::TrackedDeliveryV0)]
        policy: VerifyPolicy,
    },

    /// Verify an evidence-carrying task delivery from its files alone.
    VerifyTask {
        /// The create-once task manifest emitted by stemma-mcp.
        manifest: PathBuf,
        /// Resolve manifest artifact paths from this directory instead of the
        /// manifest's directory.
        #[arg(long, value_name = "DIR")]
        root: Option<PathBuf>,
    },

    /// Diff two files into a tracked-changes redline (reject-all == base,
    /// accept-all == target).
    Compare {
        /// The baseline document (the "before").
        base: PathBuf,
        /// The revised document (the "after").
        target: PathBuf,
        /// Where to create the redline DOCX. Refuses any existing path or input.
        #[arg(short = 'o', long = "out")]
        out: PathBuf,
        /// Attribute every discovered revision to NAME (`w:author`). Omit for an
        /// anonymous redline. An empty NAME is refused — omit the flag instead.
        #[arg(long, value_name = "NAME")]
        author: Option<String>,
        /// Output format: text (human summary on stderr, empty stdout) or json
        /// (a stemma.compare_receipt.v0 on stdout; the summary stays on stderr).
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },

    /// Read a document's body: plain text, or structured JSON with blocks and
    /// pending tracked changes.
    Extract {
        /// The document to read.
        file: PathBuf,
        /// Output format.
        #[arg(long, value_enum, default_value_t = ExtractFormat::Text)]
        format: ExtractFormat,
    },

    /// Emit the full structured read model in one call: every block with its
    /// per-segment tracked status (the redline, machine-readable), plus the
    /// complete pending-revision census. JSON on stdout; stemma.read.v0.
    Read {
        /// The document to read.
        file: PathBuf,
    },

    /// Resolve tracked changes and write the result. Exactly one disposition is
    /// required.
    #[command(group(ArgGroup::new("disposition").required(true).multiple(false)))]
    Resolve {
        /// The document whose tracked changes to resolve.
        file: PathBuf,
        /// Where to create the resolved DOCX. Refuses any existing path or input.
        #[arg(short = 'o', long = "out")]
        out: PathBuf,

        /// Accept every pending tracked change.
        #[arg(long, group = "disposition")]
        accept_all: bool,
        /// Reject every pending tracked change (restore the prior state).
        #[arg(long, group = "disposition")]
        reject_all: bool,
        /// Accept every change authored by NAME.
        #[arg(long, value_name = "NAME", group = "disposition")]
        accept_author: Option<String>,
        /// Reject every change authored by NAME.
        #[arg(long, value_name = "NAME", group = "disposition")]
        reject_author: Option<String>,
        /// Accept the changes with these revision ids (comma-separated).
        #[arg(long, value_name = "IDS", value_delimiter = ',', group = "disposition")]
        accept_ids: Vec<u32>,
        /// Reject the changes with these revision ids (comma-separated).
        #[arg(long, value_name = "IDS", value_delimiter = ',', group = "disposition")]
        reject_ids: Vec<u32>,
        /// Resolve per a stemma.resolution_plan.v0 JSON file: mixed
        /// accept/reject (authors, ids, and a `rest` disposition) in one call.
        #[arg(long, value_name = "FILE", group = "disposition")]
        plan: Option<PathBuf>,

        /// Plan and report the full outcome without writing any output.
        #[arg(long)]
        dry_run: bool,
        /// Output format: text (human summary on stderr, empty stdout) or json
        /// (a stemma.resolve_receipt.v0 on stdout; the summary stays on stderr).
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },

    /// Parse and validate a document; print block/revision counts on success.
    Validate {
        /// The document to validate.
        file: PathBuf,
        /// Output format: text (an OK line on stdout) or json (a
        /// stemma.validate.v0 result on stdout, for valid and invalid alike).
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum ExtractFormat {
    Text,
    Json,
}

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum InspectFormat {
    Markdown,
    Json,
}

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum VerifyPolicy {
    TrackedDeliveryV0,
}

/// The machine-output switch the receipt-emitting verbs (`compare`, `resolve`,
/// `validate`) share. Text keeps stdout for data the verb already prints (or
/// empty) and the human summary on stderr; Json puts a schema-tagged receipt on
/// stdout. The summary stays on stderr either way: stdout carries data.
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

fn main() -> ExitCode {
    // clap handles --help/--version and usage errors itself (exit code 2).
    let cli = Cli::parse();
    match run(cli.command) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: Command) -> Result<ExitCode, String> {
    let artifacts = PathAuthority::explicit()
        .map_err(|e| format!("cannot establish filesystem authority: {e}"))?;
    let result = match command {
        Command::Apply {
            input,
            worklist,
            out,
            receipt,
            emit_partial,
        } => {
            return apply::apply_worklist(
                &artifacts,
                &input,
                &worklist,
                &out,
                receipt.as_deref(),
                emit_partial,
            )
            .map(apply::ApplyStatus::exit_code);
        }
        Command::Inspect { file, format } => inspect(&artifacts, &file, format),
        Command::Verify {
            before,
            after,
            policy,
        } => return verify(&artifacts, &before, &after, policy),
        Command::VerifyTask { manifest, root } => {
            return Ok(verify_task::verify_task(
                &artifacts,
                &manifest,
                root.as_deref(),
            ));
        }
        Command::Compare {
            base,
            target,
            out,
            author,
            format,
        } => compare(&artifacts, &base, &target, &out, author.as_deref(), format),
        Command::Extract { file, format } => extract(&artifacts, &file, format),
        Command::Read { file } => read_cmd(&artifacts, &file),
        Command::Resolve {
            file,
            out,
            accept_all,
            reject_all,
            accept_author,
            reject_author,
            accept_ids,
            reject_ids,
            plan,
            dry_run,
            format,
        } => resolve(
            &artifacts,
            &file,
            &out,
            Disposition::from_flags(
                accept_all,
                reject_all,
                accept_author,
                reject_author,
                accept_ids,
                reject_ids,
                plan,
            )?,
            dry_run,
            format,
        ),
        Command::Validate { file, format } => return validate_cmd(&artifacts, &file, format),
    };
    result.map(|()| ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// compact inspect / verify
// ---------------------------------------------------------------------------

/// The `inspect --format json` envelope. v1 renamed the summary integers from
/// v0's `blocks`/`pending_revisions` to `*_count`: the read model's `blocks` is
/// an ARRAY, and reusing that key for a count invited consumers to conflate the
/// two shapes. The extended-Markdown projection (and its `@stemma inspect.v0`
/// header line) is unchanged — only this JSON wrapper is versioned here.
#[derive(Serialize)]
struct InspectJson {
    schema: &'static str,
    input: CompactIdentity,
    block_count: usize,
    pending_revision_count: usize,
    projection: String,
}

#[derive(Serialize)]
struct CompactIdentity {
    bytes: u64,
    sha256: String,
}

impl From<&ArtifactIdentity> for CompactIdentity {
    fn from(identity: &ArtifactIdentity) -> Self {
        Self {
            bytes: identity.bytes,
            sha256: identity.digest.hex.clone(),
        }
    }
}

fn inspect(artifacts: &PathAuthority, file: &Path, format: InspectFormat) -> Result<(), String> {
    let (doc, input) = parse_doc(artifacts, file, "input_docx")?;
    let view = doc.read();
    let blocks = view.blocks.len();
    let revisions = pending_revisions(&doc).len();
    let projection = doc.to_markdown();
    match format {
        InspectFormat::Markdown => {
            let header = format!(
                "@stemma inspect.v0 sha256={} bytes={} blocks={} pending_revisions={}",
                input.digest.hex, input.bytes, blocks, revisions
            );
            if projection.is_empty() {
                print_line(&header)
            } else {
                print_line(&format!("{header}\n\n{projection}"))
            }
        }
        InspectFormat::Json => {
            let payload = InspectJson {
                schema: "stemma.inspect.v1",
                input: CompactIdentity::from(&input),
                block_count: blocks,
                pending_revision_count: revisions,
                projection,
            };
            let encoded = serde_json::to_string_pretty(&payload)
                .map_err(|e| format!("cannot encode inspection for {}: {e}", file.display()))?;
            print_line(&encoded)
        }
    }
}

fn verify(
    artifacts: &PathAuthority,
    before_path: &Path,
    after_path: &Path,
    policy: VerifyPolicy,
) -> Result<ExitCode, String> {
    let before = artifacts
        .read_source(before_path, "before_docx", None)
        .map_err(|e| e.to_string())?;
    let after = artifacts
        .read_source(after_path, "after_docx", None)
        .map_err(|e| e.to_string())?;
    let report = stemma::api::audit(before.bytes(), after.bytes()).map_err(|e| {
        format!(
            "cannot audit {} against {}: {e}",
            after_path.display(),
            before_path.display()
        )
    })?;

    let modified_preexisting = report
        .preexisting_revisions
        .iter()
        .filter(|row| !matches!(row.disposition, RevisionDisposition::Untouched))
        .count();
    let policy_pass = match policy {
        VerifyPolicy::TrackedDeliveryV0 => {
            report.validator.ok
                && report.direct_changes.is_empty()
                && report.untouched.violations.is_empty()
                && modified_preexisting == 0
        }
    };

    let after_doc = Document::parse(after.bytes())
        .map_err(|e| format!("{}: not a valid DOCX ({e})", after_path.display()))?;
    let accepted = after_doc
        .project(Resolution::AcceptAll)
        .and_then(|doc| doc.serialize(&ExportOptions::default()))
        .map_err(|e| format!("cannot produce accepted verification projection: {e}"))?;
    let rejected = after_doc
        .project(Resolution::RejectAll)
        .and_then(|doc| doc.serialize(&ExportOptions::default()))
        .map_err(|e| format!("cannot produce rejected verification projection: {e}"))?;

    let direct: Vec<_> = report
        .direct_changes
        .iter()
        .map(|change| {
            json!({
                "story": format!("{:?}", change.story),
                "kind": change.kind.as_str(),
                "block_id": change.block_id.as_ref().map(ToString::to_string),
                "old_excerpt": change.old_excerpt,
                "new_excerpt": change.new_excerpt,
                "coincides_with_resolution": change.coincides_with_resolution,
            })
        })
        .collect();
    let validator_issues: Vec<_> = report
        .validator
        .issues
        .iter()
        .map(|issue| {
            json!({
                "code": format!("{:?}", issue.code),
                "message": issue.message,
                "context": issue.context,
            })
        })
        .collect();
    let payload = json!({
        "schema": "stemma.verify.v0",
        "policy": "tracked-delivery-v0",
        "status": if policy_pass { "pass" } else { "fail" },
        "before": CompactIdentity::from(before.identity()),
        "after": CompactIdentity::from(after.identity()),
        "summary": {
            "new_revisions": report.new_revisions.len(),
            "preexisting_revisions": report.preexisting_revisions.len(),
            "modified_or_resolved_preexisting": modified_preexisting,
            "direct_changes": report.direct_changes.len(),
            "untouched_violations": report.untouched.violations.len(),
            "validator_ok": report.validator.ok,
        },
        "projections": {
            "accepted": digest_payload(&accepted),
            "rejected": digest_payload(&rejected),
        },
        "direct_changes": direct,
        "untouched": {
            "verified_blocks": report.untouched.verified_blocks,
            "parts": report.untouched.parts,
            "violations": report.untouched.violations.iter().map(|v| json!({
                "story": format!("{:?}", v.story),
                "kind": format!("{:?}", v.kind),
                "detail": v.detail,
            })).collect::<Vec<_>>(),
        },
        "validator": {
            "ok": report.validator.ok,
            "issues": validator_issues,
        },
    });
    let encoded = serde_json::to_string_pretty(&payload)
        .map_err(|e| format!("cannot encode verification result: {e}"))?;
    print_line(&encoded)?;
    Ok(if policy_pass {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(3)
    })
}

fn digest_payload(bytes: &[u8]) -> serde_json::Value {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    json!({
        "bytes": bytes.len(),
        "sha256": format!("{digest:x}"),
    })
}

// ---------------------------------------------------------------------------
// compare
// ---------------------------------------------------------------------------

/// The `compare --format json` receipt: exact input identities, the committed
/// output, and the full census of discovered revisions (the same rows `extract
/// --format json` would enumerate on the output, ids included — so a consumer
/// can drive `resolve` without a second read).
#[derive(Serialize)]
struct CompareReceipt {
    schema: &'static str,
    base: ArtifactIdentity,
    target: ArtifactIdentity,
    /// The attribution requested via `--author`; `null` for an anonymous
    /// redline (whose revisions then carry the empty author group `""`).
    author: Option<String>,
    revisions: Vec<RevisionJson>,
    output: OutputArtifact,
}

fn compare(
    artifacts: &PathAuthority,
    base: &Path,
    target: &Path,
    out: &Path,
    author: Option<&str>,
    format: OutputFormat,
) -> Result<(), String> {
    let (base_doc, base_artifact) = parse_doc(artifacts, base, "base_docx")?;
    let (target_doc, target_artifact) = parse_doc(artifacts, target, "target_docx")?;

    // `--author NAME` attributes the discovered revisions (`diff_as`); omitting
    // it leaves the redline anonymous (`diff`). Same round-trip either way.
    let redline = match author {
        Some(name) => base_doc.diff_as(&target_doc, name),
        None => base_doc.diff(&target_doc),
    }
    .map_err(|e| {
        format!(
            "cannot diff {} against {}: {e}",
            base.display(),
            target.display()
        )
    })?;

    let bytes = serialize(&redline, out)?;
    let output = write_output(
        artifacts,
        out,
        "output_redline",
        &bytes,
        &[base_artifact.clone(), target_artifact.clone()],
    )?;

    let revisions = pending_revisions(&redline);
    let count = revisions.len();
    eprintln!(
        "wrote redline to {} ({count} tracked revision{}); {}",
        out.display(),
        if count == 1 { "" } else { "s" },
        output_summary(&output),
    );
    if format == OutputFormat::Json {
        let receipt = CompareReceipt {
            schema: "stemma.compare_receipt.v0",
            base: base_artifact,
            target: target_artifact,
            author: author.map(str::to_string),
            revisions: revisions.into_iter().map(RevisionJson::from).collect(),
            output,
        };
        let encoded = serde_json::to_string_pretty(&receipt)
            .map_err(|e| format!("cannot encode compare receipt for {}: {e}", out.display()))?;
        print_line(&encoded)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// extract
// ---------------------------------------------------------------------------

fn extract(artifacts: &PathAuthority, file: &Path, format: ExtractFormat) -> Result<(), String> {
    let (doc, _input) = parse_doc(artifacts, file, "input_docx")?;
    match format {
        ExtractFormat::Text => print_line(&doc.to_text()),
        ExtractFormat::Json => {
            let view = doc.read();
            let payload = ExtractJson {
                blocks: view.blocks.iter().map(BlockJson::from_view).collect(),
                revisions: pending_revisions(&doc)
                    .into_iter()
                    .map(RevisionJson::from)
                    .collect(),
            };
            let text = serde_json::to_string_pretty(&payload)
                .map_err(|e| format!("cannot encode JSON for {}: {e}", file.display()))?;
            print_line(&text)
        }
    }
}

#[derive(Serialize)]
struct ExtractJson {
    blocks: Vec<BlockJson>,
    revisions: Vec<RevisionJson>,
}

#[derive(Serialize)]
struct BlockJson {
    id: String,
    role: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    heading_level: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    style_id: Option<String>,
    text: String,
}

impl BlockJson {
    fn from_view(block: &stemma::api::BlockView) -> BlockJson {
        let heading_level = match block.role {
            BlockRole::Heading { level } => Some(level),
            _ => None,
        };
        BlockJson {
            id: block.id.to_string(),
            role: role_label(&block.role),
            heading_level,
            style_id: block.style_id.clone(),
            text: block.text.clone(),
        }
    }
}

#[derive(Serialize)]
struct RevisionJson {
    revision_id: u32,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    author: Option<String>,
    /// The change's `w:date` (ISO-8601), omitted when the source markup
    /// carries none — same absent-field convention as `author`.
    #[serde(skip_serializing_if = "Option::is_none")]
    date: Option<String>,
    block_id: String,
    excerpt: String,
}

impl From<PendingRevision> for RevisionJson {
    fn from(r: PendingRevision) -> RevisionJson {
        RevisionJson {
            revision_id: r.id,
            kind: r.kind.as_str(),
            author: r.author,
            date: r.date,
            block_id: r.block_id,
            excerpt: r.excerpt,
        }
    }
}

fn role_label(role: &BlockRole) -> &'static str {
    match role {
        BlockRole::Paragraph => "paragraph",
        BlockRole::Heading { .. } => "heading",
        BlockRole::Table => "table",
        BlockRole::Opaque => "opaque",
    }
}

// ---------------------------------------------------------------------------
// read
// ---------------------------------------------------------------------------

/// The `read` payload: the engine's read model serialized whole. `blocks` is
/// [`stemma::api::DocumentView::blocks`] verbatim — per-segment tracked status
/// (`Inserted`/`Deleted` with their `RevisionView`), marks, span handles,
/// guards, cells — the same typed view `docs/reference/read-model.md`
/// documents. `revisions` is the canonical census (identical rows to `extract
/// --format json`), carried alongside because the segment view alone omits
/// formatting-change records.
#[derive(Serialize)]
struct ReadJson {
    schema: &'static str,
    input: ArtifactIdentity,
    blocks: Vec<stemma::api::BlockView>,
    revisions: Vec<RevisionJson>,
}

fn read_cmd(artifacts: &PathAuthority, file: &Path) -> Result<(), String> {
    let (doc, input) = parse_doc(artifacts, file, "input_docx")?;
    let view = doc.read();
    let payload = ReadJson {
        schema: "stemma.read.v0",
        input,
        blocks: view.blocks,
        revisions: pending_revisions(&doc)
            .into_iter()
            .map(RevisionJson::from)
            .collect(),
    };
    let encoded = serde_json::to_string_pretty(&payload)
        .map_err(|e| format!("cannot encode read model for {}: {e}", file.display()))?;
    print_line(&encoded)
}

// ---------------------------------------------------------------------------
// resolve
// ---------------------------------------------------------------------------

/// The single, validated disposition a `resolve` invocation carries. clap's
/// `disposition` ArgGroup guarantees exactly one flag was supplied; this maps
/// that to the closed set of actions, so an impossible combination is
/// unrepresentable rather than checked downstream.
enum Disposition {
    AcceptAll,
    RejectAll,
    AcceptAuthor(String),
    RejectAuthor(String),
    AcceptIds(Vec<u32>),
    RejectIds(Vec<u32>),
    /// A `stemma.resolution_plan.v0` file, read and validated inside `resolve`
    /// (the path is in the same ArgGroup as every other disposition flag).
    PlanFile(PathBuf),
}

impl Disposition {
    #[allow(clippy::too_many_arguments)]
    fn from_flags(
        accept_all: bool,
        reject_all: bool,
        accept_author: Option<String>,
        reject_author: Option<String>,
        accept_ids: Vec<u32>,
        reject_ids: Vec<u32>,
        plan: Option<PathBuf>,
    ) -> Result<Disposition, String> {
        if accept_all {
            Ok(Disposition::AcceptAll)
        } else if reject_all {
            Ok(Disposition::RejectAll)
        } else if let Some(name) = accept_author {
            Ok(Disposition::AcceptAuthor(name))
        } else if let Some(name) = reject_author {
            Ok(Disposition::RejectAuthor(name))
        } else if !accept_ids.is_empty() {
            Ok(Disposition::AcceptIds(accept_ids))
        } else if !reject_ids.is_empty() {
            Ok(Disposition::RejectIds(reject_ids))
        } else if let Some(path) = plan {
            Ok(Disposition::PlanFile(path))
        } else {
            // clap's `required` ArgGroup makes this unreachable via the CLI; kept
            // as an explicit error rather than a panic (no silent fallbacks).
            Err(
                "no disposition given: pass one of --accept-all, --reject-all, \
                 --accept-author, --reject-author, --accept-ids, --reject-ids, \
                 or --plan"
                    .to_string(),
            )
        }
    }
}

const RESOLUTION_PLAN_SCHEMA: &str = "stemma.resolution_plan.v0";

/// A `stemma.resolution_plan.v0` file: a mixed disposition in one invocation.
/// Selectors address the SELECTABLE census (revision id != 0) exactly like the
/// author/id flags; census-only records (id 0) are outside plan scope and stay
/// pending. Unknown fields are refused (`deny_unknown_fields`) — a misspelled
/// selector must never silently select nothing.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolutionPlanFile {
    schema: String,
    #[serde(default)]
    accept: PlanSelectors,
    #[serde(default)]
    reject: PlanSelectors,
    /// What happens to every selectable pending revision no selector matched.
    /// The product-approved default is `leave` (documented in the CLI
    /// reference): a plan only resolves what it names unless told otherwise.
    #[serde(default)]
    rest: RestDisposition,
}

#[derive(Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanSelectors {
    /// Author groups, matched exactly; `""` is the empty-author group. Every
    /// named author must have at least one pending change (fail loud).
    #[serde(default)]
    authors: Vec<String>,
    /// Revision ids; every id must be pending and selectable (fail loud).
    #[serde(default)]
    ids: Vec<u32>,
}

#[derive(Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum RestDisposition {
    #[default]
    Leave,
    Accept,
    Reject,
}

/// What a `resolve` invocation will actually execute, planned entirely against
/// the input's live census before any projection runs. The two total forms are
/// kept distinct from the selective form because they are the engine's total
/// resolutions: they also resolve census-only records (id 0), which no
/// selective set can address.
enum Execution {
    AcceptAll,
    RejectAll,
    Selective {
        accept: HashSet<u32>,
        reject: HashSet<u32>,
    },
}

/// The `resolve --format json` receipt: which pending revisions this call
/// accepted and rejected (full census rows from the INPUT document), what is
/// still pending in the result, and the committed output. `output` is `null`
/// exactly when `--dry-run` skipped the write.
#[derive(Serialize)]
struct ResolveReceipt {
    schema: &'static str,
    input: ArtifactIdentity,
    dry_run: bool,
    accepted: Vec<RevisionJson>,
    rejected: Vec<RevisionJson>,
    remaining: Vec<RevisionJson>,
    output: Option<OutputArtifact>,
}

fn resolve(
    artifacts: &PathAuthority,
    file: &Path,
    out: &Path,
    disposition: Disposition,
    dry_run: bool,
    format: OutputFormat,
) -> Result<(), String> {
    let (doc, input_artifact) = parse_doc(artifacts, file, "input_docx")?;

    let pending = pending_revisions(&doc);
    // The output must never clobber any artifact the resolution read — the
    // input always, and the plan file when the disposition came from one.
    let mut protected = vec![input_artifact.clone()];
    let execution = match &disposition {
        Disposition::PlanFile(path) => {
            let (plan, plan_artifact) = read_resolution_plan(artifacts, path)?;
            protected.push(plan_artifact);
            plan_selective_sets(&plan, &pending, file, path)?
        }
        flag => plan_flag_execution(flag, &pending, file)?,
    };

    let resolved = execute_resolution(&doc, &execution, file)?;

    // Receipt rows partition the INPUT census: the total resolutions cover
    // every pending row (census-only id-0 records included); a selective
    // execution covers exactly its id sets.
    let (accepted, rejected): (Vec<RevisionJson>, Vec<RevisionJson>) = match &execution {
        Execution::AcceptAll => (
            pending.into_iter().map(RevisionJson::from).collect(),
            vec![],
        ),
        Execution::RejectAll => (
            vec![],
            pending.into_iter().map(RevisionJson::from).collect(),
        ),
        Execution::Selective { accept, reject } => {
            let mut accepted = Vec::new();
            let mut rejected = Vec::new();
            for row in pending {
                if accept.contains(&row.id) {
                    accepted.push(RevisionJson::from(row));
                } else if reject.contains(&row.id) {
                    rejected.push(RevisionJson::from(row));
                }
            }
            (accepted, rejected)
        }
    };
    let remaining: Vec<RevisionJson> = pending_revisions(&resolved)
        .into_iter()
        .map(RevisionJson::from)
        .collect();

    let output = if dry_run {
        eprintln!(
            "dry run: would write resolved document to {}; {} accepted, {} rejected, {} remaining pending; no output written",
            out.display(),
            accepted.len(),
            rejected.len(),
            remaining.len(),
        );
        None
    } else {
        let bytes = serialize(&resolved, out)?;
        let output = write_output(artifacts, out, "output_resolved_docx", &bytes, &protected)?;
        eprintln!(
            "wrote resolved document to {}; {}",
            out.display(),
            output_summary(&output)
        );
        Some(output)
    };

    if format == OutputFormat::Json {
        let receipt = ResolveReceipt {
            schema: "stemma.resolve_receipt.v0",
            input: input_artifact,
            dry_run,
            accepted,
            rejected,
            remaining,
            output,
        };
        let encoded = serde_json::to_string_pretty(&receipt)
            .map_err(|e| format!("cannot encode resolve receipt for {}: {e}", file.display()))?;
        print_line(&encoded)?;
    }
    Ok(())
}

/// Turn a flag disposition plus the document's live pending revisions into an
/// [`Execution`], failing loud when the selection would match nothing (an
/// unknown id or an author with no changes) — never a silent no-op.
fn plan_flag_execution(
    disposition: &Disposition,
    pending: &[PendingRevision],
    file: &Path,
) -> Result<Execution, String> {
    // id 0 is the census-only sentinel (reported, never selectable) — it must
    // not satisfy the non-empty check nor be offered to the selective resolver.
    let known: HashSet<u32> = pending.iter().map(|r| r.id).filter(|id| *id != 0).collect();

    let selective =
        |accept: HashSet<u32>, reject: HashSet<u32>| Execution::Selective { accept, reject };
    match disposition {
        Disposition::AcceptAll => {
            require_nonempty(&known, file)?;
            Ok(Execution::AcceptAll)
        }
        Disposition::RejectAll => {
            require_nonempty(&known, file)?;
            Ok(Execution::RejectAll)
        }
        Disposition::AcceptAuthor(name) => Ok(selective(
            ids_by_author(pending, name, file)?,
            HashSet::new(),
        )),
        Disposition::RejectAuthor(name) => Ok(selective(
            HashSet::new(),
            ids_by_author(pending, name, file)?,
        )),
        Disposition::AcceptIds(ids) => Ok(selective(check_ids(ids, &known, file)?, HashSet::new())),
        Disposition::RejectIds(ids) => Ok(selective(HashSet::new(), check_ids(ids, &known, file)?)),
        Disposition::PlanFile(_) => Err(
            "internal: a plan-file disposition reached the flag planner (programmer bug)"
                .to_string(),
        ),
    }
}

/// Read and schema-check a resolution plan through the artifact authority, so
/// the plan file joins the protected sources the output must not alias.
fn read_resolution_plan(
    artifacts: &PathAuthority,
    path: &Path,
) -> Result<(ResolutionPlanFile, ArtifactIdentity), String> {
    let source = artifacts
        .read_source(path, "resolution_plan", None)
        .map_err(|e| e.to_string())?;
    let plan: ResolutionPlanFile = serde_json::from_slice(source.bytes()).map_err(|e| {
        format!(
            "{}: not a valid {RESOLUTION_PLAN_SCHEMA} document: {e}",
            path.display()
        )
    })?;
    if plan.schema != RESOLUTION_PLAN_SCHEMA {
        return Err(format!(
            "{}: unsupported plan schema {:?} (expected {RESOLUTION_PLAN_SCHEMA:?})",
            path.display(),
            plan.schema
        ));
    }
    Ok((plan, source.identity().clone()))
}

/// Expand a validated plan into disjoint accept/reject id sets against the
/// live census. Every selector must match (fail loud), an id selected by both
/// sides is a contradiction (fail loud), and a plan that resolves nothing is
/// an error — never a silent unchanged copy.
fn plan_selective_sets(
    plan: &ResolutionPlanFile,
    pending: &[PendingRevision],
    file: &Path,
    plan_path: &Path,
) -> Result<Execution, String> {
    let known: HashSet<u32> = pending.iter().map(|r| r.id).filter(|id| *id != 0).collect();

    let expand = |selectors: &PlanSelectors| -> Result<HashSet<u32>, String> {
        let mut ids = HashSet::new();
        for author in &selectors.authors {
            ids.extend(ids_by_author(pending, author, file)?);
        }
        ids.extend(check_ids(&selectors.ids, &known, file)?);
        Ok(ids)
    };
    let mut accept = expand(&plan.accept)?;
    let mut reject = expand(&plan.reject)?;

    let mut contested: Vec<u32> = accept.intersection(&reject).copied().collect();
    if !contested.is_empty() {
        contested.sort_unstable();
        return Err(format!(
            "{}: revision id(s) {} are selected by both accept and reject",
            plan_path.display(),
            contested
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }

    let rest: Vec<u32> = known
        .iter()
        .copied()
        .filter(|id| !accept.contains(id) && !reject.contains(id))
        .collect();
    match plan.rest {
        RestDisposition::Leave => {}
        RestDisposition::Accept => accept.extend(rest),
        RestDisposition::Reject => reject.extend(rest),
    }

    if accept.is_empty() && reject.is_empty() {
        return Err(format!(
            "{}: plan resolves nothing in {} (no selector matched and rest is \"leave\")",
            plan_path.display(),
            file.display(),
        ));
    }
    Ok(Execution::Selective { accept, reject })
}

/// Run the planned execution. A mixed selective plan is two engine
/// projections — accepts first, then rejects — with an explicit id-durability
/// check at the phase boundary: revision identities are content-derived and
/// survive a projection (see the CLI reference's id-durability contract), but
/// if accepting a change re-keyed a survivor selected for rejection, this
/// fails loud rather than rejecting the wrong revision.
fn execute_resolution(
    doc: &Document,
    execution: &Execution,
    file: &Path,
) -> Result<Document, String> {
    let cannot = |e| format!("cannot resolve tracked changes in {}: {e}", file.display());
    match execution {
        Execution::AcceptAll => doc.project(Resolution::AcceptAll).map_err(cannot),
        Execution::RejectAll => doc.project(Resolution::RejectAll).map_err(cannot),
        Execution::Selective { accept, reject } => {
            let mut current: Option<Document> = None;
            if !accept.is_empty() {
                current = Some(
                    doc.project(Resolution::Selective {
                        ids: accept.clone(),
                        action: ResolveSelectionAction::Accept,
                    })
                    .map_err(cannot)?,
                );
            }
            if !reject.is_empty() {
                let base = current.as_ref().unwrap_or(doc);
                if !accept.is_empty() {
                    let still: HashSet<u32> =
                        pending_revisions(base).iter().map(|r| r.id).collect();
                    let mut missing: Vec<u32> = reject
                        .iter()
                        .copied()
                        .filter(|id| !still.contains(id))
                        .collect();
                    if !missing.is_empty() {
                        missing.sort_unstable();
                        return Err(format!(
                            "revision id(s) {} selected for reject are no longer pending after \
                             the accept phase in {} (an accepted change re-keyed them); re-read \
                             the ids and re-plan",
                            missing
                                .iter()
                                .map(u32::to_string)
                                .collect::<Vec<_>>()
                                .join(", "),
                            file.display(),
                        ));
                    }
                }
                current = Some(
                    base.project(Resolution::Selective {
                        ids: reject.clone(),
                        action: ResolveSelectionAction::Reject,
                    })
                    .map_err(cannot)?,
                );
            }
            // Planning guarantees at least one non-empty side; reaching here
            // with neither is a programmer bug, reported rather than unwrapped.
            current.ok_or_else(|| {
                "internal: selective execution with empty accept and reject sets (programmer bug)"
                    .to_string()
            })
        }
    }
}

fn require_nonempty(known: &HashSet<u32>, file: &Path) -> Result<(), String> {
    if known.is_empty() {
        return Err(format!(
            "no pending tracked changes to resolve in {}",
            file.display()
        ));
    }
    Ok(())
}

fn ids_by_author(
    pending: &[PendingRevision],
    author: &str,
    file: &Path,
) -> Result<HashSet<u32>, String> {
    let ids: HashSet<u32> = pending
        .iter()
        .filter(|r| r.author.as_deref() == Some(author))
        .map(|r| r.id)
        .filter(|id| *id != 0)
        .collect();
    if ids.is_empty() {
        return Err(format!(
            "no pending tracked changes by author {author:?} in {}{}",
            file.display(),
            known_authors_hint(pending),
        ));
    }
    Ok(ids)
}

fn known_authors_hint(pending: &[PendingRevision]) -> String {
    // A blank `w:author` (Word anonymization, and the attribution `diff`
    // stamps) is a real, selectable author group — its selector token is the
    // empty string, so the hint shows `"" (empty author)` rather than a
    // made-up placeholder the user would type verbatim and miss with.
    // Census-only records (id 0) carry no author and are not selectable, so
    // they stay out of the hint.
    let mut authors: Vec<&str> = pending
        .iter()
        .filter(|r| r.id != 0)
        .map(|r| match r.author.as_deref() {
            Some(name) if !name.is_empty() => name,
            _ => "\"\" (empty author)",
        })
        .collect();
    authors.sort_unstable();
    authors.dedup();
    if authors.is_empty() {
        String::new()
    } else {
        format!(" (authors present: {})", authors.join(", "))
    }
}

fn check_ids(requested: &[u32], known: &HashSet<u32>, file: &Path) -> Result<HashSet<u32>, String> {
    let missing: Vec<u32> = requested
        .iter()
        .copied()
        .filter(|id| !known.contains(id))
        .collect();
    if !missing.is_empty() {
        let mut present: Vec<u32> = known.iter().copied().collect();
        present.sort_unstable();
        let present = if present.is_empty() {
            "none".to_string()
        } else {
            present
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        };
        return Err(format!(
            "revision id(s) {} not found in {} (pending ids: {present})",
            missing
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            file.display(),
        ));
    }
    Ok(requested.iter().copied().collect())
}

// ---------------------------------------------------------------------------
// validate
// ---------------------------------------------------------------------------

/// The `validate --format json` result. Valid and invalid are BOTH structured
/// results on stdout (mirroring `verify`'s pass/fail contract): `status` is
/// `"ok"` or `"invalid"`, `issues` is empty exactly when `ok`, and the process
/// still exits `1` on `"invalid"`. Only an operational failure (unreadable
/// file, not a DOCX at all) stays on the `error:` stderr path.
#[derive(Serialize)]
struct ValidateJson {
    schema: &'static str,
    status: &'static str,
    input: ArtifactIdentity,
    block_count: usize,
    pending_revision_count: usize,
    issues: Vec<ValidateIssueJson>,
}

#[derive(Serialize)]
struct ValidateIssueJson {
    code: String,
    message: String,
    context: Option<String>,
}

fn validate_cmd(
    artifacts: &PathAuthority,
    file: &Path,
    format: OutputFormat,
) -> Result<ExitCode, String> {
    let input = artifacts
        .read_source(file, "input_docx", None)
        .map_err(|e| e.to_string())?;
    let doc = Document::parse(input.bytes())
        .map_err(|e| format!("{}: not a valid DOCX ({e})", file.display()))?;

    let report = validate(input.bytes());
    let view = doc.read();
    let blocks = view.blocks.len();
    let revisions = pending_revisions(&doc).len();

    match format {
        OutputFormat::Text => {
            if !report.ok {
                let details = report
                    .issues
                    .iter()
                    .map(|issue| match &issue.context {
                        Some(ctx) => format!("{:?}: {} [{ctx}]", issue.code, issue.message),
                        None => format!("{:?}: {}", issue.code, issue.message),
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
                return Err(format!("{}: invalid DOCX — {details}", file.display()));
            }
            print_line(&format!(
                "OK: {} — {blocks} block{}, {revisions} pending revision{}; bytes={} sha256={}",
                file.display(),
                if blocks == 1 { "" } else { "s" },
                if revisions == 1 { "" } else { "s" },
                input.identity().bytes,
                input.identity().digest.hex,
            ))?;
            Ok(ExitCode::SUCCESS)
        }
        OutputFormat::Json => {
            let payload = ValidateJson {
                schema: "stemma.validate.v0",
                status: if report.ok { "ok" } else { "invalid" },
                input: input.identity().clone(),
                block_count: blocks,
                pending_revision_count: revisions,
                issues: report
                    .issues
                    .iter()
                    .map(|issue| ValidateIssueJson {
                        code: format!("{:?}", issue.code),
                        message: issue.message.clone(),
                        context: issue.context.clone(),
                    })
                    .collect(),
            };
            let encoded = serde_json::to_string_pretty(&payload).map_err(|e| {
                format!(
                    "cannot encode validation result for {}: {e}",
                    file.display()
                )
            })?;
            print_line(&encoded)?;
            Ok(if report.ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
    }
}

// ---------------------------------------------------------------------------
// revision enumeration (shared by extract / resolve / validate)
// ---------------------------------------------------------------------------

/// One pending tracked change, enumerated from the engine's canonical census
/// ([`Document::revisions`]) in document order. Revision ids are the
/// engine-minted identities the selective resolver addresses (never raw wire
/// ids); `id == 0` marks a census-only record that is reported but not
/// individually selectable.
struct PendingRevision {
    id: u32,
    author: Option<String>,
    date: Option<String>,
    kind: RevisionKind,
    block_id: String,
    excerpt: String,
}

/// Every pending revision, once, in first-seen document order — the engine's
/// canonical census, NOT a re-derivation from the segment view (the view
/// carries no formatting-change records, so a view-derived count silently
/// understates the pending state). A revision id can surface across several
/// carriers; we keep the first occurrence's block and excerpt as its
/// representative, exactly as a reviewer reads it.
fn pending_revisions(doc: &Document) -> Vec<PendingRevision> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for r in doc.revisions() {
        // Census-only records share id 0; each is its own row, so only real
        // identities deduplicate.
        if r.revision_id != 0 && !seen.insert(r.revision_id) {
            continue;
        }
        out.push(PendingRevision {
            id: r.revision_id,
            author: r.author,
            date: r.date,
            kind: r.kind,
            block_id: r.block_id.to_string(),
            excerpt: excerpt(&r.excerpt),
        });
    }
    out
}

/// A short, single-line excerpt: whitespace-collapsed and capped, so a JSON
/// revision row stays a preview rather than dumping a whole paragraph.
fn excerpt(text: &str) -> String {
    const MAX: usize = 100;
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= MAX {
        return flat;
    }
    let mut truncated: String = flat.chars().take(MAX).collect();
    truncated.push('…');
    truncated
}

// ---------------------------------------------------------------------------
// shared edges: reading, parsing, serializing, writing
// ---------------------------------------------------------------------------

/// Write one line of data to stdout, tolerating a closed downstream reader.
///
/// `println!` panics on a write error; a reader like `head` closing the pipe
/// early is normal Unix behavior, not a bug, so a broken pipe exits cleanly
/// (like any well-behaved filter) rather than panicking. Any other write error
/// is a real, reportable failure.
fn print_line(text: &str) -> Result<(), String> {
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    match handle
        .write_all(text.as_bytes())
        .and_then(|()| handle.write_all(b"\n"))
    {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => std::process::exit(0),
        Err(e) => Err(format!("cannot write to stdout: {e}")),
    }
}

fn parse_doc(
    artifacts: &PathAuthority,
    path: &Path,
    role: &str,
) -> Result<(Document, ArtifactIdentity), String> {
    let input = artifacts
        .read_source(path, role, None)
        .map_err(|e| e.to_string())?;
    let doc = Document::parse(input.bytes())
        .map_err(|e| format!("{}: not a valid DOCX ({e})", path.display()))?;
    Ok((doc, input.identity().clone()))
}

fn serialize(doc: &Document, out: &Path) -> Result<Vec<u8>, String> {
    doc.serialize(&ExportOptions::default())
        .map_err(|e| format!("cannot serialize output for {}: {e}", out.display()))
}

fn write_output(
    artifacts: &PathAuthority,
    path: &Path,
    role: &str,
    bytes: &[u8],
    protected_sources: &[ArtifactIdentity],
) -> Result<OutputArtifact, String> {
    artifacts
        .commit_new(path, role, bytes, protected_sources)
        .map_err(|e| e.to_string())
}

fn output_summary(output: &OutputArtifact) -> String {
    let collision_policy = match output.collision_policy {
        CollisionPolicy::CreateNew => "create_new",
        _ => "unknown",
    };
    let disposition = match output.disposition {
        ArtifactDisposition::Created => "created",
        _ => "unknown",
    };
    let digest_algorithm = match output.identity.digest.algorithm {
        DigestAlgorithm::Sha256 => "sha256",
        _ => "unknown_digest",
    };
    format!(
        "bytes={} {digest_algorithm}={} collision_policy={collision_policy} disposition={disposition}",
        output.identity.bytes, output.identity.digest.hex,
    )
}
