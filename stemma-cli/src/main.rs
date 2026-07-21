//! `stemma` — a thin command-line interface to the DOCX engine.
//!
//! The focused path applies an approved worklist to an existing DOCX as native
//! tracked changes. Maintenance verbs compare, extract, resolve, and validate.
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
    },

    /// Parse and validate a document; print block/revision counts on success.
    Validate {
        /// The document to validate.
        file: PathBuf,
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
        } => compare(&artifacts, &base, &target, &out, author.as_deref()),
        Command::Extract { file, format } => extract(&artifacts, &file, format),
        Command::Resolve {
            file,
            out,
            accept_all,
            reject_all,
            accept_author,
            reject_author,
            accept_ids,
            reject_ids,
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
            )?,
        ),
        Command::Validate { file } => validate_cmd(&artifacts, &file),
    };
    result.map(|()| ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// compact inspect / verify
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct InspectJson {
    schema: &'static str,
    input: CompactIdentity,
    blocks: usize,
    pending_revisions: usize,
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
                schema: "stemma.inspect.v0",
                input: CompactIdentity::from(&input),
                blocks,
                pending_revisions: revisions,
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

fn compare(
    artifacts: &PathAuthority,
    base: &Path,
    target: &Path,
    out: &Path,
    author: Option<&str>,
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
        &[base_artifact, target_artifact],
    )?;

    let count = pending_revisions(&redline).len();
    eprintln!(
        "wrote redline to {} ({count} tracked revision{}); {}",
        out.display(),
        if count == 1 { "" } else { "s" },
        output_summary(&output),
    );
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
    block_id: String,
    excerpt: String,
}

impl From<PendingRevision> for RevisionJson {
    fn from(r: PendingRevision) -> RevisionJson {
        RevisionJson {
            revision_id: r.id,
            kind: r.kind.as_str(),
            author: r.author,
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
}

impl Disposition {
    fn from_flags(
        accept_all: bool,
        reject_all: bool,
        accept_author: Option<String>,
        reject_author: Option<String>,
        accept_ids: Vec<u32>,
        reject_ids: Vec<u32>,
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
        } else {
            // clap's `required` ArgGroup makes this unreachable via the CLI; kept
            // as an explicit error rather than a panic (no silent fallbacks).
            Err(
                "no disposition given: pass one of --accept-all, --reject-all, \
                 --accept-author, --reject-author, --accept-ids, or --reject-ids"
                    .to_string(),
            )
        }
    }
}

fn resolve(
    artifacts: &PathAuthority,
    file: &Path,
    out: &Path,
    disposition: Disposition,
) -> Result<(), String> {
    let (doc, input_artifact) = parse_doc(artifacts, file, "input_docx")?;

    let pending = pending_revisions(&doc);
    let resolution = plan_resolution(&disposition, &pending, file)?;

    let resolved = doc
        .project(resolution)
        .map_err(|e| format!("cannot resolve tracked changes in {}: {e}", file.display()))?;

    let bytes = serialize(&resolved, out)?;
    let output = write_output(
        artifacts,
        out,
        "output_resolved_docx",
        &bytes,
        &[input_artifact],
    )?;

    eprintln!(
        "wrote resolved document to {}; {}",
        out.display(),
        output_summary(&output)
    );
    Ok(())
}

/// Turn a validated [`Disposition`] plus the document's live pending revisions
/// into an engine [`Resolution`], failing loud when the selection would match
/// nothing (an unknown id or an author with no changes) — never a silent no-op.
fn plan_resolution(
    disposition: &Disposition,
    pending: &[PendingRevision],
    file: &Path,
) -> Result<Resolution, String> {
    // id 0 is the census-only sentinel (reported, never selectable) — it must
    // not satisfy the non-empty check nor be offered to the selective resolver.
    let known: HashSet<u32> = pending.iter().map(|r| r.id).filter(|id| *id != 0).collect();

    match disposition {
        Disposition::AcceptAll => {
            require_nonempty(&known, file)?;
            Ok(Resolution::AcceptAll)
        }
        Disposition::RejectAll => {
            require_nonempty(&known, file)?;
            Ok(Resolution::RejectAll)
        }
        Disposition::AcceptAuthor(name) => Ok(Resolution::Selective {
            ids: ids_by_author(pending, name, file)?,
            action: ResolveSelectionAction::Accept,
        }),
        Disposition::RejectAuthor(name) => Ok(Resolution::Selective {
            ids: ids_by_author(pending, name, file)?,
            action: ResolveSelectionAction::Reject,
        }),
        Disposition::AcceptIds(ids) => Ok(Resolution::Selective {
            ids: check_ids(ids, &known, file)?,
            action: ResolveSelectionAction::Accept,
        }),
        Disposition::RejectIds(ids) => Ok(Resolution::Selective {
            ids: check_ids(ids, &known, file)?,
            action: ResolveSelectionAction::Reject,
        }),
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
    // A tracked change with no `w:author`, or a blank one (Word anonymization,
    // and the attribution `diff` stamps), reads as `<anonymous>` rather than an
    // empty token.
    let mut authors: Vec<&str> = pending
        .iter()
        .map(|r| match r.author.as_deref() {
            Some(name) if !name.is_empty() => name,
            _ => "<anonymous>",
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

fn validate_cmd(artifacts: &PathAuthority, file: &Path) -> Result<(), String> {
    let input = artifacts
        .read_source(file, "input_docx", None)
        .map_err(|e| e.to_string())?;
    let doc = Document::parse(input.bytes())
        .map_err(|e| format!("{}: not a valid DOCX ({e})", file.display()))?;

    let report = validate(input.bytes());
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

    let view = doc.read();
    let blocks = view.blocks.len();
    let revisions = pending_revisions(&doc).len();
    print_line(&format!(
        "OK: {} — {blocks} block{}, {revisions} pending revision{}; bytes={} sha256={}",
        file.display(),
        if blocks == 1 { "" } else { "s" },
        if revisions == 1 { "" } else { "s" },
        input.identity().bytes,
        input.identity().digest.hex,
    ))
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
