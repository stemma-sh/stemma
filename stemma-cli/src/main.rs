//! `stemma` — a thin command-line interface to the DOCX engine.
//!
//! A zero-integration path to the engine's core verbs for adopters who are not
//! writing Rust: compare two files into a tracked-changes redline, extract the
//! body as text or JSON, resolve tracked changes, and validate a package.
//!
//! Design contract (CLAUDE.md): parse at the edges, no silent fallbacks. Every
//! failure exits nonzero with a one-line actionable message on stderr naming
//! what failed and which file/id; user input never panics. stdout carries data,
//! stderr carries diagnostics. The CLI drives ONLY the stable
//! [`stemma::api::Document`] facade.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{ArgGroup, Parser, Subcommand};
use serde::Serialize;
use stemma::api::{BlockRole, Document, DocumentView, SegmentView, TrackStatus, validate};
use stemma::{ExportOptions, Resolution, ResolveSelectionAction};

/// `compare --author NAME` attributes every discovered revision to NAME
/// (`diff_as`); omitting it leaves the redline anonymous (`diff`). See the
/// `--author` note in `docs/reference/cli.md`.
#[derive(Parser)]
#[command(
    name = "stemma",
    version,
    about = "Tracked-change DOCX operations from the command line.",
    long_about = "Compare two DOCX files into a tracked-changes redline, extract the \
                  body as text/JSON, resolve tracked changes, and validate a package. \
                  Drives the stemma engine's stable Document facade."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Diff two files into a tracked-changes redline (reject-all == base,
    /// accept-all == target).
    Compare {
        /// The baseline document (the "before").
        base: PathBuf,
        /// The revised document (the "after").
        target: PathBuf,
        /// Where to write the redline DOCX. Overwrites an existing file; refuses
        /// to overwrite either input.
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
        /// Where to write the resolved DOCX. Overwrites an existing file;
        /// refuses to overwrite the input.
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

fn main() -> ExitCode {
    // clap handles --help/--version and usage errors itself (exit code 2).
    let cli = Cli::parse();
    match run(cli.command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: Command) -> Result<(), String> {
    match command {
        Command::Compare {
            base,
            target,
            out,
            author,
        } => compare(&base, &target, &out, author.as_deref()),
        Command::Extract { file, format } => extract(&file, format),
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
        Command::Validate { file } => validate_cmd(&file),
    }
}

// ---------------------------------------------------------------------------
// compare
// ---------------------------------------------------------------------------

fn compare(base: &Path, target: &Path, out: &Path, author: Option<&str>) -> Result<(), String> {
    let base_doc = parse_doc(base)?;
    let target_doc = parse_doc(target)?;

    refuse_output_over_input(base, out)?;
    refuse_output_over_input(target, out)?;

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
    write_output(out, &bytes)?;

    let count = pending_revisions(&redline.read()).len();
    eprintln!(
        "wrote redline to {} ({count} tracked revision{})",
        out.display(),
        if count == 1 { "" } else { "s" }
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// extract
// ---------------------------------------------------------------------------

fn extract(file: &Path, format: ExtractFormat) -> Result<(), String> {
    let doc = parse_doc(file)?;
    match format {
        ExtractFormat::Text => print_line(&doc.to_text()),
        ExtractFormat::Json => {
            let view = doc.read();
            let payload = ExtractJson {
                blocks: view.blocks.iter().map(BlockJson::from_view).collect(),
                revisions: pending_revisions(&view)
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
            kind: r.kind.label(),
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

fn resolve(file: &Path, out: &Path, disposition: Disposition) -> Result<(), String> {
    let doc = parse_doc(file)?;
    refuse_output_over_input(file, out)?;

    let pending = pending_revisions(&doc.read());
    let resolution = plan_resolution(&disposition, &pending, file)?;

    let resolved = doc
        .project(resolution)
        .map_err(|e| format!("cannot resolve tracked changes in {}: {e}", file.display()))?;

    let bytes = serialize(&resolved, out)?;
    write_output(out, &bytes)?;

    eprintln!("wrote resolved document to {}", out.display());
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
    let known: HashSet<u32> = pending.iter().map(|r| r.id).collect();

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

fn validate_cmd(file: &Path) -> Result<(), String> {
    let bytes = read_bytes(file)?;
    let doc = Document::parse(&bytes)
        .map_err(|e| format!("{}: not a valid DOCX ({e})", file.display()))?;

    let report = validate(&bytes);
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
    let revisions = pending_revisions(&view).len();
    print_line(&format!(
        "OK: {} — {blocks} block{}, {revisions} pending revision{}",
        file.display(),
        if blocks == 1 { "" } else { "s" },
        if revisions == 1 { "" } else { "s" },
    ))
}

// ---------------------------------------------------------------------------
// revision enumeration (shared by extract / resolve / validate)
// ---------------------------------------------------------------------------

/// One pending tracked change, enumerated from the read projection in document
/// order. Revision ids are session handles read from the live view (never reused
/// from raw XML), matching how the engine's own examples enumerate revisions.
struct PendingRevision {
    id: u32,
    author: Option<String>,
    kind: RevKind,
    block_id: String,
    excerpt: String,
}

#[derive(Clone, Copy)]
enum RevKind {
    Insert,
    Delete,
}

impl RevKind {
    fn label(self) -> &'static str {
        match self {
            RevKind::Insert => "insert",
            RevKind::Delete => "delete",
        }
    }
}

/// Every pending revision, once, in first-seen document order. A revision id can
/// surface across several spans; we keep the first occurrence's block and
/// excerpt as its representative, exactly as a reviewer reads it.
fn pending_revisions(view: &DocumentView) -> Vec<PendingRevision> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();

    for block in &view.blocks {
        let block_id = block.id.to_string();
        let mut record = |status: &TrackStatus, text: &str| {
            for (rev, kind) in status_entries(status) {
                if seen.insert(rev.revision_id) {
                    out.push(PendingRevision {
                        id: rev.revision_id,
                        author: rev.author.clone(),
                        kind,
                        block_id: block_id.clone(),
                        excerpt: excerpt(text),
                    });
                }
            }
        };

        record(&block.block_status, &block.text);
        record(&block.paragraph_mark_status, &block.text);
        for segment in &block.segments {
            match segment {
                SegmentView::Text { status, text, .. } => record(status, text),
                SegmentView::Opaque { status, text, .. } => {
                    record(status, text.as_deref().unwrap_or(""))
                }
            }
        }
    }
    out
}

/// The `(revision, kind)` pairs a single tracked status carries. A stacked
/// insert-then-delete surfaces both its revisions (D5), each keyed by its own id.
fn status_entries(status: &TrackStatus) -> Vec<(stemma::api::RevisionView, RevKind)> {
    match status {
        TrackStatus::Normal => vec![],
        TrackStatus::Inserted(r) => vec![(r.clone(), RevKind::Insert)],
        TrackStatus::Deleted(r) => vec![(r.clone(), RevKind::Delete)],
        TrackStatus::InsertedThenDeleted { inserted, deleted } => vec![
            (inserted.clone(), RevKind::Insert),
            (deleted.clone(), RevKind::Delete),
        ],
    }
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

fn read_bytes(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}

fn parse_doc(path: &Path) -> Result<Document, String> {
    let bytes = read_bytes(path)?;
    Document::parse(&bytes).map_err(|e| format!("{}: not a valid DOCX ({e})", path.display()))
}

fn serialize(doc: &Document, out: &Path) -> Result<Vec<u8>, String> {
    doc.serialize(&ExportOptions::default())
        .map_err(|e| format!("cannot serialize output for {}: {e}", out.display()))
}

fn write_output(path: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Refuse to write output over one of the inputs (same canonical path).
/// Overwriting an unrelated existing file is allowed (standard tool behavior);
/// clobbering the input in place is not.
fn refuse_output_over_input(input: &Path, output: &Path) -> Result<(), String> {
    let input_canon = input
        .canonicalize()
        .map_err(|e| format!("cannot resolve {}: {e}", input.display()))?;
    let output_canon = canonical_output(output)?;
    if input_canon == output_canon {
        return Err(format!(
            "refusing to overwrite the input file {}: choose a different --out path",
            input.display()
        ));
    }
    Ok(())
}

/// Canonicalize an output path that may not exist yet: resolve its parent
/// directory (which must exist) and rejoin the file name.
fn canonical_output(output: &Path) -> Result<PathBuf, String> {
    if let Ok(existing) = output.canonicalize() {
        return Ok(existing);
    }
    let parent = match output.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let file_name = output
        .file_name()
        .ok_or_else(|| format!("output path {} has no file name", output.display()))?;
    let parent_canon = parent
        .canonicalize()
        .map_err(|e| format!("cannot resolve output directory {}: {e}", parent.display()))?;
    Ok(parent_canon.join(file_name))
}
