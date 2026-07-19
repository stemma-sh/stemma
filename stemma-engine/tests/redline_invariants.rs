//! Content-level invariant tests for redline tracked changes.
//!
//! Unlike the existing structural tests ("does the XML contain `<w:del>`?"),
//! these tests verify **content correctness**:
//!
//! - **Diff layer**: `inline_changes` must reconstruct both `old_text` and `new_text`
//! - **Redline XML layer**: accept-all / reject-all must reproduce target / base text

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::{collections::BTreeMap, fs};

mod common;

use rayon::prelude::*;
use stemma::{
    BlockNode, CanonDoc, DiffChange, DocxRuntime, ExportMode, InlineChange, InlineNode, Mark,
    MarkValue, NoteType, RevisionInfo, SimpleRuntime, TrackingStatus, TransactionMeta, accept_all,
    diff_documents, merge_diff, redline_extract::extract_redline,
};

// ── helpers ──────────────────────────────────────────────────────────────

fn redline_meta() -> TransactionMeta {
    TransactionMeta {
        author: "redline_invariants".to_string(),
        reason: Some("invariant test".to_string()),
        timestamp_utc: Some("2025-06-01T00:00:00Z".to_string()),
    }
}

/// Replicates the private `extract_inline_text` from diff.rs.
/// Concatenates text from InlineNode::Text (with Caps normalization),
/// `\n` for HardBreak, and `\u{FFFC}` for OpaqueInline.
fn extract_inline_text(inlines: &[InlineNode]) -> String {
    let mut out = String::new();
    for inline in inlines {
        match inline {
            InlineNode::Text(t) => {
                if t.style_props.caps == MarkValue::On {
                    out.push_str(&t.text.to_uppercase());
                } else {
                    out.push_str(&t.text);
                }
            }
            InlineNode::HardBreak(_) => out.push('\n'),
            InlineNode::OpaqueInline(_) => out.push('\u{FFFC}'),
            InlineNode::Decoration(_)
            | InlineNode::CommentRangeStart { .. }
            | InlineNode::CommentRangeEnd { .. }
            | InlineNode::CommentReference { .. } => {}
        }
    }
    out
}

/// Extract the "accepted" view of a paragraph's text.
///
/// When a document has pre-existing tracked changes (w:ins/w:del), the
/// paragraph segments carry `Inserted` / `Deleted` statuses. The
/// user-visible text in Word is the *accepted* projection: Normal +
/// Inserted segments (Deleted content is shown as strikethrough and is
/// not part of the current document state).
///
/// If all segments are Normal (no tracked changes), this falls back to
/// `rendered_text` (which includes numbering prefixes) for backward
/// compatibility with non-tracked-change documents.
fn extract_accepted_paragraph_text(p: &stemma::ParagraphNode) -> String {
    let has_tracked_segments = p.segments.iter().any(|s| {
        matches!(
            s.status,
            TrackingStatus::Inserted(_) | TrackingStatus::Deleted(_)
        )
    });

    if has_tracked_segments {
        // Extract only Normal + Inserted inlines (the "accepted" view).
        let accepted_inlines: Vec<&InlineNode> = p
            .segments
            .iter()
            .filter(|s| !matches!(s.status, TrackingStatus::Deleted(_)))
            .flat_map(|s| s.inlines.iter())
            .collect();
        let mut out = String::new();
        // Re-add numbering prefix if present, since inlines have it stripped.
        if let Some(ref prefix) = p.literal_prefix {
            out.push_str(prefix);
            out.push('\t');
        }
        for inline in &accepted_inlines {
            match inline {
                InlineNode::Text(t) => {
                    if t.style_props.caps == MarkValue::On {
                        out.push_str(&t.text.to_uppercase());
                    } else {
                        out.push_str(&t.text);
                    }
                }
                InlineNode::HardBreak(_) => out.push('\n'),
                InlineNode::OpaqueInline(_) => out.push('\u{FFFC}'),
                // Zero-width markers — no text contribution
                InlineNode::Decoration(_)
                | InlineNode::CommentRangeStart { .. }
                | InlineNode::CommentRangeEnd { .. }
                | InlineNode::CommentReference { .. } => {}
            }
        }
        out
    } else {
        // No tracked changes — use rendered_text (includes numbering prefix)
        // or fall back to extracting from all inlines.
        let inlines = p.all_inlines_owned();
        p.rendered_text
            .clone()
            .unwrap_or_else(|| extract_inline_text(&inlines))
    }
}

/// Extract plain text from each paragraph in a CanonDoc, in document order.
/// Tables are walked recursively to collect cell paragraphs.
/// Produces the "accepted" view: for documents with pre-existing tracked
/// changes, only Normal + Inserted content is included (Deleted is skipped).
fn canon_paragraph_texts(doc: &CanonDoc) -> Vec<String> {
    let mut texts = Vec::new();
    collect_block_texts(&doc.blocks, &mut texts);
    texts
}

fn collect_block_texts(blocks: &[stemma::TrackedBlock], texts: &mut Vec<String>) {
    for tracked in blocks {
        // Skip block-level deletions (whole paragraphs deleted via tracked changes).
        if matches!(tracked.status, TrackingStatus::Deleted(_)) {
            continue;
        }
        match &tracked.block {
            BlockNode::Paragraph(p) => {
                texts.push(extract_accepted_paragraph_text(p));
            }
            BlockNode::Table(t) => {
                for row in &t.rows {
                    for cell in &row.cells {
                        collect_cell_block_texts(&cell.blocks, texts);
                    }
                }
            }
            BlockNode::OpaqueBlock(_) => {}
        }
    }
}

fn collect_cell_block_texts(blocks: &[BlockNode], texts: &mut Vec<String>) {
    for block in blocks {
        match block {
            BlockNode::Paragraph(p) => {
                texts.push(extract_accepted_paragraph_text(p));
            }
            BlockNode::Table(t) => {
                for row in &t.rows {
                    for cell in &row.cells {
                        collect_cell_block_texts(&cell.blocks, texts);
                    }
                }
            }
            BlockNode::OpaqueBlock(_) => {}
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum StoryKind {
    Header,
    Footer,
    Footnotes,
    Endnotes,
}

impl StoryKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Header => "header",
            Self::Footer => "footer",
            Self::Footnotes => "footnotes",
            Self::Endnotes => "endnotes",
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum StoryProjection {
    Accept,
    Reject,
}

const STORY_KINDS: [StoryKind; 4] = [
    StoryKind::Header,
    StoryKind::Footer,
    StoryKind::Footnotes,
    StoryKind::Endnotes,
];

fn empty_story_map() -> BTreeMap<StoryKind, Vec<String>> {
    let mut out = BTreeMap::new();
    for kind in STORY_KINDS {
        out.insert(kind, Vec::new());
    }
    out
}

fn classify_story_path(path: &str) -> Option<StoryKind> {
    let p = path.to_ascii_lowercase();
    // "word/footnotes.xml" starts with "word/footer" - check footnotes first.
    if p.contains("footnotes") {
        Some(StoryKind::Footnotes)
    } else if p.contains("endnotes") {
        Some(StoryKind::Endnotes)
    } else if p.contains("header") {
        Some(StoryKind::Header)
    } else if p.contains("footer") {
        Some(StoryKind::Footer)
    } else {
        None
    }
}

fn normalize_story_paragraph(text: &str) -> Option<String> {
    // Opaque inline marker is an internal projection detail, not visible text.
    // Story invariants compare user-visible content, so strip it.
    let visible = text.replace('\u{FFFC}', "");
    // Strip hyperlink targets and field-code punctuation artifacts that are not
    // user-visible story text.
    let filtered = visible
        .split_whitespace()
        .filter(|token| {
            !token.starts_with("http://") && !token.starts_with("https://") && *token != "."
        })
        .collect::<Vec<_>>()
        .join(" ");
    let norm = normalize_for_comparison(&filtered);
    if norm.is_empty() {
        return None;
    }
    // A paragraph consisting solely of bare page-number digits (cached PAGE
    // field display value) is a field display artifact, not meaningful story
    // content. The redline DOCX includes footers from both base and target
    // documents, so these cached values can appear as "extra" paragraphs that
    // don't correspond to any paragraph in the canonical base or target import.
    // Filter them out — they carry no user-visible content.
    if is_field_display_artifact(&norm) {
        return None;
    }
    Some(norm)
}

/// Returns true if the text is a cached field display value (e.g., a bare page
/// number like "2" or a field-marker-only paragraph like "- 2 -" where the
/// only non-punctuation content is digits).
fn is_field_display_artifact(text: &str) -> bool {
    // Pure digits (e.g., "2", "15") — cached PAGE field display.
    text.chars().all(|c| c.is_ascii_digit())
}

fn canonical_story_paragraph_multiset(doc: &CanonDoc) -> BTreeMap<StoryKind, Vec<String>> {
    let mut out = empty_story_map();

    for header in &doc.headers {
        let mut texts = Vec::new();
        collect_block_texts(&header.blocks, &mut texts);
        for text in texts {
            if let Some(norm) = normalize_story_paragraph(&text) {
                out.get_mut(&StoryKind::Header).unwrap().push(norm);
            }
        }
    }

    for footer in &doc.footers {
        let mut texts = Vec::new();
        collect_block_texts(&footer.blocks, &mut texts);
        for text in texts {
            if let Some(norm) = normalize_story_paragraph(&text) {
                out.get_mut(&StoryKind::Footer).unwrap().push(norm);
            }
        }
    }

    for footnote in &doc.footnotes {
        if footnote.note_type != NoteType::Normal {
            continue;
        }
        let mut texts = Vec::new();
        collect_block_texts(&footnote.blocks, &mut texts);
        for text in texts {
            if let Some(norm) = normalize_story_paragraph(&text) {
                out.get_mut(&StoryKind::Footnotes).unwrap().push(norm);
            }
        }
    }

    for endnote in &doc.endnotes {
        if endnote.note_type != NoteType::Normal {
            continue;
        }
        let mut texts = Vec::new();
        collect_block_texts(&endnote.blocks, &mut texts);
        for text in texts {
            if let Some(norm) = normalize_story_paragraph(&text) {
                out.get_mut(&StoryKind::Endnotes).unwrap().push(norm);
            }
        }
    }

    for texts in out.values_mut() {
        texts.sort();
    }
    out
}

fn extracted_story_paragraph_multiset(
    extract: &stemma::redline_extract::RedlineExtract,
    projection: StoryProjection,
) -> BTreeMap<StoryKind, Vec<String>> {
    let mut out = empty_story_map();

    for (path, paragraphs) in &extract.stories {
        let Some(kind) = classify_story_path(path) else {
            continue;
        };
        for para in paragraphs {
            let projected = match projection {
                StoryProjection::Accept => para.accept_text(),
                StoryProjection::Reject => para.reject_text(),
            };
            if let Some(norm) = normalize_story_paragraph(&projected) {
                out.get_mut(&kind).unwrap().push(norm);
            }
        }
    }

    for texts in out.values_mut() {
        texts.sort();
    }
    out
}

fn story_multiset_counts(texts: &[String]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for text in texts {
        *counts.entry(text.clone()).or_insert(0) += 1;
    }
    counts
}

fn truncate_for_diff(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}

fn story_multiset_diff(expected: &[String], actual: &[String]) -> String {
    let expected_counts = story_multiset_counts(expected);
    let actual_counts = story_multiset_counts(actual);
    let mut lines = Vec::new();

    let mut shown = 0usize;
    for (text, exp_count) in &expected_counts {
        let got = actual_counts.get(text).copied().unwrap_or(0);
        if got >= *exp_count {
            continue;
        }
        lines.push(format!(
            "missing {}x {:?}",
            exp_count - got,
            truncate_for_diff(text, 300)
        ));
        shown += 1;
        if shown >= 3 {
            break;
        }
    }

    shown = 0;
    for (text, got_count) in &actual_counts {
        let exp = expected_counts.get(text).copied().unwrap_or(0);
        if exp >= *got_count {
            continue;
        }
        lines.push(format!(
            "extra {}x {:?}",
            got_count - exp,
            truncate_for_diff(text, 300)
        ));
        shown += 1;
        if shown >= 3 {
            break;
        }
    }

    if lines.is_empty() {
        "no count delta details".to_string()
    } else {
        lines.join("; ")
    }
}

/// Run the full redline pipeline and return exported DOCX bytes.
/// Hint the allocator to return freed pages to the OS. On glibc this calls
/// `malloc_trim(0)`; elsewhere it's a no-op. This is critical for large
/// documents where the allocator retains freed pages, inflating RSS past
/// the cgroup memory limit.
fn release_freed_memory() {
    #[cfg(target_os = "linux")]
    {
        unsafe extern "C" {
            fn malloc_trim(pad: usize) -> std::ffi::c_int;
        }
        unsafe { malloc_trim(0) };
    }
}

fn run_redline_pipeline(before_path: &str, after_path: &str) -> Vec<u8> {
    let runtime = SimpleRuntime::new();

    // Import both documents, keeping only the doc handles.
    // The import canonical docs are dropped to free memory before
    // diff_and_redline rebuilds them via view().
    let (before_handle, after_handle) = {
        let before_bytes =
            fs::read(before_path).unwrap_or_else(|err| panic!("read {before_path}: {err}"));
        let after_bytes =
            fs::read(after_path).unwrap_or_else(|err| panic!("read {after_path}: {err}"));

        let import_before = runtime
            .import_docx(&before_bytes)
            .unwrap_or_else(|err| panic!("import {before_path}: {err:?}"));
        let import_after = runtime
            .import_docx(&after_bytes)
            .unwrap_or_else(|err| panic!("import {after_path}: {err:?}"));

        (import_before.doc_handle, import_after.doc_handle)
        // import canonical docs + raw bytes dropped here
    };

    // Force the allocator to return freed pages so subsequent phases
    // start from a lower RSS baseline.
    release_freed_memory();

    let apply = runtime
        .diff_and_redline(&before_handle, &after_handle, redline_meta())
        .unwrap_or_else(|err| panic!("diff_and_redline failed: {err:?}"));
    assert!(apply.applied, "redline must be marked as applied");

    // Drop the apply result (contains another canonical) before exporting.
    drop(apply);
    release_freed_memory();

    let exported = runtime
        .export_docx(&before_handle, ExportMode::Redline)
        .unwrap_or_else(|err| panic!("export_docx failed: {err:?}"));
    assert!(!exported.is_empty(), "exported DOCX must not be empty");

    exported
}

// ── discover fixture pairs ───────────────────────────────────────────────

fn discover_fixture_pairs() -> Vec<(String, String, String)> {
    let mut pairs = Vec::new();

    // Top-level testdata
    if let Ok(entries) = fs::read_dir("testdata") {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let fixture_name = path.file_name().unwrap().to_string_lossy().to_string();
                let before = path.join("before.docx");
                let after = path.join("after.docx");
                if before.exists() && after.exists() {
                    pairs.push((
                        fixture_name,
                        before.to_string_lossy().to_string(),
                        after.to_string_lossy().to_string(),
                    ));
                }
            }
        }
    }

    // Synthesized testdata
    if let Ok(entries) = fs::read_dir("testdata/synthesized") {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let before = path.join("before.docx");
                let after = path.join("after.docx");
                if before.exists() && after.exists() {
                    let name = format!(
                        "synthesized/{}",
                        path.file_name().unwrap().to_string_lossy()
                    );
                    pairs.push((
                        name,
                        before.to_string_lossy().to_string(),
                        after.to_string_lossy().to_string(),
                    ));
                }
            }
        }
    }

    // Production samples (backend/samples/) — only add fixtures not already in testdata
    if let Ok(entries) = fs::read_dir(common::samples_dir()) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let fixture_name = path.file_name().unwrap().to_string_lossy().to_string();
                // Skip if already discovered from testdata/
                if pairs.iter().any(|(name, _, _)| *name == fixture_name) {
                    continue;
                }
                let before = path.join("before.docx");
                let after = path.join("after.docx");
                if before.exists() && after.exists() {
                    pairs.push((
                        fixture_name,
                        before.to_string_lossy().to_string(),
                        after.to_string_lossy().to_string(),
                    ));
                }
            }
        }
    }

    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    pairs
}

fn selected_fixture_filter() -> Option<String> {
    match std::env::var("REDLINE_INVARIANT_FIXTURE") {
        Ok(raw) => {
            let trimmed = raw.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        }
        Err(std::env::VarError::NotPresent) => None,
        Err(err) => panic!("failed to read REDLINE_INVARIANT_FIXTURE: {err}"),
    }
}

fn include_excluded_fixtures() -> bool {
    match std::env::var("REDLINE_INVARIANT_INCLUDE_EXCLUDED") {
        Ok(raw) => {
            let v = raw.trim().to_ascii_lowercase();
            matches!(v.as_str(), "1" | "true" | "yes" | "on")
        }
        Err(std::env::VarError::NotPresent) => false,
        Err(err) => panic!("failed to read REDLINE_INVARIANT_INCLUDE_EXCLUDED: {err}"),
    }
}

fn fixture_pair_by_name(name: &str) -> Option<(String, String, String)> {
    discover_fixture_pairs()
        .into_iter()
        .find(|(n, _, _)| n == name)
}

// ══════════════════════════════════════════════════════════════════════════
// Group 1: Diff inline_changes invariant
// ══════════════════════════════════════════════════════════════════════════

/// For every BlockModified in the diff, the inline_changes must reconstruct
/// both old_text and new_text exactly.
///
/// - concat(Unchanged.text + Deleted.text, in order) == old_text
/// - concat(Unchanged.text + Inserted.text, in order) == new_text
#[test]
#[ignore] // Slow fixture sweep — run via `just nightly`
fn diff_inline_changes_reconstruct_old_and_new_text() {
    let pairs = discover_fixture_pairs();
    assert!(!pairs.is_empty(), "no fixture pairs found");

    let total_checked = AtomicUsize::new(0);
    let failures = Mutex::new(Vec::<String>::new());

    pairs
        .par_iter()
        .for_each(|(name, before_path, after_path)| {
            let before_bytes =
                fs::read(before_path).unwrap_or_else(|err| panic!("[{name}] read before: {err}"));
            let after_bytes =
                fs::read(after_path).unwrap_or_else(|err| panic!("[{name}] read after: {err}"));

            let runtime = SimpleRuntime::new();

            let import_before = runtime
                .import_docx(&before_bytes)
                .unwrap_or_else(|err| panic!("[{name}] import before: {err:?}"));
            let import_after = runtime
                .import_docx(&after_bytes)
                .unwrap_or_else(|err| panic!("[{name}] import after: {err:?}"));

            let diff = runtime
                .diff(&import_before.doc_handle, &import_after.doc_handle)
                .unwrap_or_else(|err| panic!("[{name}] diff: {err:?}"));

            for (i, change) in diff.changes.iter().enumerate() {
                if let DiffChange::BlockModified {
                    old_text,
                    new_text,
                    inline_changes,
                    ..
                } = change
                {
                    // All opaque segments occupy a single U+FFFC in extract_inline_text
                    // (via opaque_placeholder()), regardless of display text (e.g. Sym "a").
                    let reconstructed_old: String = inline_changes
                        .iter()
                        .filter_map(|c| match c {
                            InlineChange::Unchanged { text, .. }
                            | InlineChange::Deleted { text, .. } => Some(text.as_str()),
                            InlineChange::Inserted { .. } => None,
                            InlineChange::Opaque {
                                segment_type:
                                    stemma::InlineChangeSegmentType::Equal
                                    | stemma::InlineChangeSegmentType::Delete,
                                ..
                            } => Some("\u{FFFC}"),
                            InlineChange::Opaque { .. } => None,
                        })
                        .collect();

                    let reconstructed_new: String = inline_changes
                        .iter()
                        .filter_map(|c| match c {
                            InlineChange::Unchanged { text, .. }
                            | InlineChange::Inserted { text, .. } => Some(text.as_str()),
                            InlineChange::Deleted { .. } => None,
                            InlineChange::Opaque {
                                segment_type:
                                    stemma::InlineChangeSegmentType::Equal
                                    | stemma::InlineChangeSegmentType::Insert,
                                ..
                            } => Some("\u{FFFC}"),
                            InlineChange::Opaque { .. } => None,
                        })
                        .collect();

                    if &reconstructed_old != old_text {
                        failures.lock().unwrap().push(format!(
                            "[{name}] change #{i}: reconstructed old text from inline_changes \
                         does not match old_text.\n  \
                         inline_changes: {inline_changes:?}"
                        ));
                    }
                    if &reconstructed_new != new_text {
                        failures.lock().unwrap().push(format!(
                            "[{name}] change #{i}: reconstructed new text from inline_changes \
                         does not match new_text.\n  \
                         inline_changes: {inline_changes:?}"
                        ));
                    }
                    total_checked.fetch_add(1, Ordering::Relaxed);
                }
            }
        });

    let total_checked = total_checked.load(Ordering::Relaxed);
    let failures = failures.into_inner().unwrap();
    assert!(
        total_checked > 0,
        "expected at least one BlockModified across all fixtures"
    );
    eprintln!(
        "diff_inline_changes invariant: checked {total_checked} BlockModified changes across {} fixture pairs",
        pairs.len()
    );
    assert!(
        failures.is_empty(),
        "diff inline_changes reconstruction failures ({}):\n{}",
        failures.len(),
        failures
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

// ══════════════════════════════════════════════════════════════════════════
// Group 2: Redline XML accept/reject invariant
// ══════════════════════════════════════════════════════════════════════════

/// Fixtures excluded from accept/reject invariant.
/// Each exclusion must have a documented reason.
///
/// Two root causes affect these fixtures:
///
/// 1. **Numbering definition drift** — The redline DOCX contains the target's
///    numbering definitions. Paragraphs that are unchanged by the diff (Normal)
///    can gain or lose numbering prefixes when the numbering definitions differ
///    between base and target. The merge pipeline materializes prefix changes
///    for *modified* paragraphs, but unchanged paragraphs silently acquire the
///    target's numbering. Fix requires materializing numbering prefix changes
///    for all affected paragraphs when definitions change, not just modified ones.
///
/// 2. **Duplicate fingerprint misalignment** — Documents with many short,
///    identical numbered-list items ("i.", "ii.", etc.) cause fingerprint
///    collisions in the Patience diff. The same base paragraph can be matched
///    as Normal at one position and also emitted as Deleted at another,
///    producing duplicate content in the reject view. Fix requires positional
///    disambiguation or a different alignment strategy.
///
/// 3. **Diff alignment + OpaqueBlock interaction** -- The diff incorrectly
///    identifies 2 target paragraphs as BlockModified instead of BlockInserted +
///    BlockDeleted. The numbering walk then pairs these with wrong target paragraphs,
///    causing double numbering prefixes in the serialized output.
const REDLINE_ACCEPT_REJECT_EXCLUSIONS: &[&str] = &[];

/// Whole-document accept/reject invariant for redline DOCX.
///
/// Uses `extract_redline` to parse the exported DOCX into `RedlineExtract`,
/// then reconstructs accept-all / reject-all text and compares against the
/// canonical base and target documents.
///
/// All discovered fixtures are tested by default; only documented exclusions
/// are skipped (see `REDLINE_ACCEPT_REJECT_EXCLUSIONS`).
#[test]
#[ignore] // Slow fixture sweep — run via `just nightly`
fn redline_xml_accept_reject_invariant() {
    let all_pairs = discover_fixture_pairs();
    assert!(!all_pairs.is_empty(), "no fixture pairs found");
    let fixture_filter = selected_fixture_filter();
    let include_excluded = include_excluded_fixtures();

    // Validate that every exclusion corresponds to an actual fixture.
    let all_names: Vec<&str> = all_pairs.iter().map(|(n, _, _)| n.as_str()).collect();
    for excl in REDLINE_ACCEPT_REJECT_EXCLUSIONS {
        assert!(
            all_names.contains(excl),
            "exclusion list entry {excl:?} does not match any discovered fixture.\n\
             Available: {all_names:?}"
        );
    }

    // Partition into testable vs excluded before the parallel section.
    let excluded_names: Vec<&str> = all_pairs
        .iter()
        .filter(|(name, _, _)| {
            if let Some(filter) = &fixture_filter {
                name == filter || name.contains(filter.as_str())
            } else {
                true
            }
        })
        .filter(|(name, _, _)| REDLINE_ACCEPT_REJECT_EXCLUSIONS.contains(&name.as_str()))
        .map(|(name, _, _)| name.as_str())
        .collect();

    let testable_pairs: Vec<&(String, String, String)> = all_pairs
        .iter()
        .filter(|(name, _, _)| {
            if let Some(filter) = &fixture_filter {
                name == filter || name.contains(filter.as_str())
            } else {
                true
            }
        })
        .filter(|(name, _, _)| {
            include_excluded || !REDLINE_ACCEPT_REJECT_EXCLUSIONS.contains(&name.as_str())
        })
        .collect();

    let total_fixtures_checked = AtomicUsize::new(0);
    let failures = Mutex::new(Vec::<String>::new());

    testable_pairs
        .par_iter()
        .for_each(|(fixture_name, before_path, after_path)| {
            // Import base and target to get canonical paragraph texts.
            // Scoped so runtime + canonical docs are dropped before run_redline_pipeline
            // creates its own runtime, avoiding holding two full pipelines in memory.
            let (base_texts, target_texts) = {
                let before_bytes = fs::read(before_path)
                    .unwrap_or_else(|err| panic!("[{fixture_name}] read before: {err}"));
                let after_bytes = fs::read(after_path)
                    .unwrap_or_else(|err| panic!("[{fixture_name}] read after: {err}"));

                let runtime = SimpleRuntime::new();
                let import_before = runtime
                    .import_docx(&before_bytes)
                    .unwrap_or_else(|err| panic!("[{fixture_name}] import before: {err:?}"));
                let import_after = runtime
                    .import_docx(&after_bytes)
                    .unwrap_or_else(|err| panic!("[{fixture_name}] import after: {err:?}"));

                let base_texts = canon_paragraph_texts(&import_before.canonical);
                let target_texts = canon_paragraph_texts(&import_after.canonical);
                (base_texts, target_texts)
            };
            release_freed_memory();

            // Run redline pipeline and extract
            let exported = run_redline_pipeline(before_path, after_path);
            let extract = extract_redline(&exported)
                .unwrap_or_else(|err| panic!("[{fixture_name}] extract_redline: {err}"));

            let rejected: Vec<String> = extract.body.iter().map(|p| p.reject_text()).collect();
            let accepted: Vec<String> = extract.body.iter().map(|p| p.accept_text()).collect();

            // Normalize: join paragraphs, collapse whitespace
            let rejected_doc = normalize_doc_text(&rejected);
            let accepted_doc = normalize_doc_text(&accepted);
            let base_doc = normalize_doc_text(&base_texts);
            let target_doc = normalize_doc_text(&target_texts);

            if rejected_doc != base_doc {
                failures.lock().unwrap().push(format!(
                    "[{fixture_name}] reject-all document text does not match base document"
                ));
            }
            if accepted_doc != target_doc {
                failures.lock().unwrap().push(format!(
                    "[{fixture_name}] accept-all document text does not match target document"
                ));
            }

            total_fixtures_checked.fetch_add(1, Ordering::Relaxed);
        });

    let total_fixtures_checked = total_fixtures_checked.load(Ordering::Relaxed);
    let failures = failures.into_inner().unwrap();
    assert!(
        total_fixtures_checked > 0,
        "expected at least one fixture to be checked"
    );
    eprintln!(
        "redline XML accept/reject invariant: checked {} fixtures, excluded {} ({}), filter={:?}",
        total_fixtures_checked,
        excluded_names.len(),
        excluded_names.join(", "),
        fixture_filter,
    );
    assert!(
        failures.is_empty(),
        "redline accept/reject invariant failures ({}):\n{}",
        failures.len(),
        failures
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Story-level accept/reject invariant for exported redline DOCX.
///
/// This is a first-party product invariant (not LO parity):
/// - reject-all story text must match base canonical stories
/// - accept-all story text must match target canonical stories
///
/// Comparison is done as paragraph multisets per story kind
/// (header/footer/footnotes/endnotes) to avoid path-order coupling.
#[test]
#[ignore] // Slow fixture sweep — run via `just nightly`
fn redline_xml_story_accept_reject_invariant() {
    let all_pairs = discover_fixture_pairs();
    assert!(!all_pairs.is_empty(), "no fixture pairs found");
    let fixture_filter = selected_fixture_filter();

    let testable_pairs: Vec<&(String, String, String)> = all_pairs
        .iter()
        .filter(|(name, _, _)| {
            if let Some(filter) = &fixture_filter {
                name == filter || name.contains(filter.as_str())
            } else {
                true
            }
        })
        .collect();

    let total_fixtures_checked = AtomicUsize::new(0);
    let fixtures_with_stories = AtomicUsize::new(0);
    let failures = Mutex::new(Vec::<String>::new());

    testable_pairs.par_iter().for_each(|(fixture_name, before_path, after_path)| {
        // Scoped so runtime + canonical docs are dropped before run_redline_pipeline
        // creates its own runtime, avoiding holding two full pipelines in memory.
        let (expected_reject, expected_accept) = {
            let before_bytes = fs::read(before_path)
                .unwrap_or_else(|err| panic!("[{fixture_name}] read before: {err}"));
            let after_bytes =
                fs::read(after_path).unwrap_or_else(|err| panic!("[{fixture_name}] read after: {err}"));

            let runtime = SimpleRuntime::new();
            let import_before = runtime
                .import_docx(&before_bytes)
                .unwrap_or_else(|err| panic!("[{fixture_name}] import before: {err:?}"));
            let import_after = runtime
                .import_docx(&after_bytes)
                .unwrap_or_else(|err| panic!("[{fixture_name}] import after: {err:?}"));

            let expected_reject = canonical_story_paragraph_multiset(&import_before.canonical);
            let expected_accept = canonical_story_paragraph_multiset(&import_after.canonical);
            (expected_reject, expected_accept)
        };
        release_freed_memory();

        let exported = run_redline_pipeline(before_path, after_path);
        let extract = extract_redline(&exported)
            .unwrap_or_else(|err| panic!("[{fixture_name}] extract_redline: {err}"));
        let actual_reject = extracted_story_paragraph_multiset(&extract, StoryProjection::Reject);
        let actual_accept = extracted_story_paragraph_multiset(&extract, StoryProjection::Accept);

        let has_story_content = STORY_KINDS.iter().copied().any(|kind| {
            !expected_reject.get(&kind).unwrap().is_empty()
                || !expected_accept.get(&kind).unwrap().is_empty()
                || !actual_reject.get(&kind).unwrap().is_empty()
                || !actual_accept.get(&kind).unwrap().is_empty()
        });
        if has_story_content {
            fixtures_with_stories.fetch_add(1, Ordering::Relaxed);
        }

        let mut local_failures = Vec::new();
        for kind in STORY_KINDS {
            let expected_reject_kind = expected_reject.get(&kind).unwrap();
            let expected_accept_kind = expected_accept.get(&kind).unwrap();
            let actual_reject_kind = actual_reject.get(&kind).unwrap();
            let actual_accept_kind = actual_accept.get(&kind).unwrap();

            if actual_reject_kind != expected_reject_kind {
                local_failures.push(format!(
                    "[{fixture_name}] {} reject-all story text mismatch: expected_count={}, actual_count={}, {}",
                    kind.as_str(),
                    expected_reject_kind.len(),
                    actual_reject_kind.len(),
                    story_multiset_diff(expected_reject_kind, actual_reject_kind),
                ));
            }
            if actual_accept_kind != expected_accept_kind {
                local_failures.push(format!(
                    "[{fixture_name}] {} accept-all story text mismatch: expected_count={}, actual_count={}, {}",
                    kind.as_str(),
                    expected_accept_kind.len(),
                    actual_accept_kind.len(),
                    story_multiset_diff(expected_accept_kind, actual_accept_kind),
                ));
            }
        }
        if !local_failures.is_empty() {
            failures.lock().unwrap().extend(local_failures);
        }

        total_fixtures_checked.fetch_add(1, Ordering::Relaxed);
    });

    let total_fixtures_checked = total_fixtures_checked.load(Ordering::Relaxed);
    let fixtures_with_stories = fixtures_with_stories.load(Ordering::Relaxed);
    let failures = failures.into_inner().unwrap();
    assert!(
        total_fixtures_checked > 0,
        "expected at least one fixture to be checked"
    );
    eprintln!(
        "redline XML story accept/reject invariant: checked {total_fixtures_checked} fixtures; {fixtures_with_stories} with story content; filter={fixture_filter:?}"
    );
    assert!(
        failures.is_empty(),
        "story accept/reject regressions ({}):\n{}",
        failures.len(),
        failures
            .iter()
            .take(40)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Targeted regression test: when footnotes are introduced only in target,
/// accept-all must include them and reject-all must exclude them.
#[test]
fn story_accept_reject_invariant_for_inserted_footnotes_fixture() {
    let (fixture_name, before_path, after_path) =
        fixture_pair_by_name("footnotes").expect("missing fixture pair: footnotes");

    // Scoped so runtime + canonical docs are dropped before run_redline_pipeline.
    let (expected_reject, expected_accept) = {
        let before_bytes = fs::read(&before_path)
            .unwrap_or_else(|err| panic!("[{fixture_name}] read before: {err}"));
        let after_bytes = fs::read(&after_path)
            .unwrap_or_else(|err| panic!("[{fixture_name}] read after: {err}"));

        let runtime = SimpleRuntime::new();
        let import_before = runtime
            .import_docx(&before_bytes)
            .unwrap_or_else(|err| panic!("[{fixture_name}] import before: {err:?}"));
        let import_after = runtime
            .import_docx(&after_bytes)
            .unwrap_or_else(|err| panic!("[{fixture_name}] import after: {err:?}"));

        let expected_reject = canonical_story_paragraph_multiset(&import_before.canonical);
        let expected_accept = canonical_story_paragraph_multiset(&import_after.canonical);
        (expected_reject, expected_accept)
    };
    release_freed_memory();

    let exported = run_redline_pipeline(&before_path, &after_path);
    let extract = extract_redline(&exported)
        .unwrap_or_else(|err| panic!("[{fixture_name}] extract_redline: {err}"));
    let actual_reject = extracted_story_paragraph_multiset(&extract, StoryProjection::Reject);
    let actual_accept = extracted_story_paragraph_multiset(&extract, StoryProjection::Accept);

    let expected_reject_footnotes = expected_reject.get(&StoryKind::Footnotes).unwrap();
    let expected_accept_footnotes = expected_accept.get(&StoryKind::Footnotes).unwrap();
    let actual_reject_footnotes = actual_reject.get(&StoryKind::Footnotes).unwrap();
    let actual_accept_footnotes = actual_accept.get(&StoryKind::Footnotes).unwrap();

    assert_eq!(
        actual_reject_footnotes,
        expected_reject_footnotes,
        "[{fixture_name}] footnotes reject-all mismatch: {}",
        story_multiset_diff(expected_reject_footnotes, actual_reject_footnotes)
    );
    assert_eq!(
        actual_accept_footnotes,
        expected_accept_footnotes,
        "[{fixture_name}] footnotes accept-all mismatch: {}",
        story_multiset_diff(expected_accept_footnotes, actual_accept_footnotes)
    );
}

/// Join paragraph texts and normalize for whole-document comparison.
/// Filters empty paragraphs and collapses whitespace.
fn normalize_doc_text(para_texts: &[String]) -> String {
    para_texts
        .iter()
        .map(|t| normalize_for_comparison(t))
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Normalize text for comparison: collapse whitespace, trim.
fn normalize_for_comparison(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ── common prefix/suffix helpers ─────────────────────────────────────────

/// Byte length of the longest common prefix between two strings.
fn common_prefix_len(a: &str, b: &str) -> usize {
    a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count()
}

/// Byte length of the longest common suffix between two strings,
/// excluding any overlap with the already-matched prefix.
fn common_suffix_len(a: &str, b: &str, prefix_len: usize) -> usize {
    let a_rest = &a.as_bytes()[prefix_len..];
    let b_rest = &b.as_bytes()[prefix_len..];
    a_rest
        .iter()
        .rev()
        .zip(b_rest.iter().rev())
        .take_while(|(x, y)| x == y)
        .count()
}

/// Character class matching the diff tokenizer in `diff.rs`.
///
/// - Word: alphanumeric + underscore (grouped into contiguous runs)
/// - Whitespace: whitespace chars (grouped into contiguous runs)
/// - Punctuation: everything else (each char is its own token)
fn token_char_class(c: char) -> u8 {
    if c.is_alphanumeric() || c == '_' {
        0 // Word
    } else if c.is_whitespace() {
        1 // Whitespace
    } else {
        2 // Punctuation
    }
}

/// True if byte position `pos` in `text` falls on a token boundary
/// (the diff tokenizer would start a new token here).
fn is_token_boundary(text: &str, pos: usize) -> bool {
    if pos == 0 || pos >= text.len() {
        return true;
    }
    if !text.is_char_boundary(pos) {
        return false;
    }
    let prev = token_char_class(text[..pos].chars().next_back().unwrap());
    let curr = token_char_class(text[pos..].chars().next().unwrap());
    // Different classes → always a boundary.
    // Punctuation → each char is its own token → always a boundary.
    prev != curr || prev == 2 || curr == 2
}

/// Snap a byte-level common prefix length down to a token boundary.
///
/// The diff operates at token granularity, so the byte-level common prefix
/// may land mid-token. We walk backward to the nearest position that is a
/// token boundary in BOTH texts.
fn word_aligned_prefix(old_text: &str, new_text: &str, raw: usize) -> usize {
    if raw == 0 {
        return 0;
    }
    let min_len = old_text.len().min(new_text.len());
    if raw >= min_len {
        return raw;
    }
    // Within 0..raw, bytes are identical in both texts, so char boundaries match.
    // Snap raw back to a char boundary first.
    let mut pos = raw;
    while pos > 0 && !old_text.is_char_boundary(pos) {
        pos -= 1;
    }
    // Walk backward to find a token boundary in both texts.
    while pos > 0 {
        if is_token_boundary(old_text, pos) && is_token_boundary(new_text, pos) {
            return pos;
        }
        pos -= 1;
        while pos > 0 && !old_text.is_char_boundary(pos) {
            pos -= 1;
        }
    }
    0
}

/// Snap a byte-level common suffix length down to a token boundary.
///
/// The suffix starts at `text.len() - raw_suffix`. We walk forward to the
/// nearest position that is a token boundary in BOTH texts.
fn word_aligned_suffix(old_text: &str, new_text: &str, raw_suffix: usize) -> usize {
    if raw_suffix == 0 {
        return 0;
    }
    let min_len = old_text.len().min(new_text.len());
    if raw_suffix >= min_len {
        return raw_suffix;
    }
    let old_start = old_text.len() - raw_suffix;
    let new_start = new_text.len() - raw_suffix;
    // Snap forward to a char boundary in both texts.
    let mut offset = 0;
    while offset < raw_suffix {
        if old_text.is_char_boundary(old_start + offset)
            && new_text.is_char_boundary(new_start + offset)
        {
            break;
        }
        offset += 1;
    }
    // Walk forward to find a token boundary in both texts.
    while offset < raw_suffix {
        let old_pos = old_start + offset;
        let new_pos = new_start + offset;
        if is_token_boundary(old_text, old_pos) && is_token_boundary(new_text, new_pos) {
            return raw_suffix - offset;
        }
        offset += 1;
        while offset < raw_suffix && !old_text.is_char_boundary(old_start + offset) {
            offset += 1;
        }
    }
    0
}

// ══════════════════════════════════════════════════════════════════════════
// Group 3: Diff inline_changes granularity invariant
// ══════════════════════════════════════════════════════════════════════════

/// **Domain rule**: within a single substitution (adjacent Del/Ins pair with the
/// same formatting marks), the diff must not "eat" shared text into the
/// changed spans.
///
/// We check each (Deleted, Inserted) pair for common prefix/suffix (word-aligned,
/// since the diff is token-level). A substantial shared region that wasn't
/// extracted as Unchanged is a granularity failure.
///
/// **Principled exemptions** (each maps to a documented diff-engine behavior):
///
/// 1. *Bail-out*: the diff engine intentionally does a full replace when both
///    texts are >= 50 chars and content similarity < 30% (with a high-value-token
///    safety check). At the block level this produces zero Unchanged segments.
///    We skip those blocks entirely — they're an intentional quality trade-off.
///
/// 2. *Token boundaries*: the diff operates at token granularity, so byte-level
///    shared text that falls mid-token cannot be preserved. Prefix/suffix
///    lengths are snapped to token boundaries (matching the tokenizer's char
///    classes: Word, Whitespace, Punctuation) in both old and new text.
///
/// 3. *Cross-mark pairs*: a Del/Ins pair with *different* marks is a formatting
///    change, not a content substitution. We skip these — the shared text may
///    correctly belong to an adjacent Unchanged segment with different marks.
///
/// 4. *Fused tokens*: the tokenizer fuses certain patterns into single tokens
///    (e.g., intraword apostrophes: "Company's" → one token). When such a
///    fused token changes, the diff must replace it whole. We detect this when
///    the shared prefix and suffix are entirely word characters — the change
///    is within a single token boundary.
#[test]
#[ignore] // Slow fixture sweep — run via `just nightly`
fn diff_inline_changes_preserve_common_prefix_suffix() {
    let pairs = discover_fixture_pairs();
    assert!(!pairs.is_empty(), "no fixture pairs found");

    fn get_marks(c: &InlineChange) -> &[Mark] {
        match c {
            InlineChange::Unchanged { marks, .. }
            | InlineChange::Deleted { marks, .. }
            | InlineChange::Inserted { marks, .. } => marks,
            InlineChange::Opaque { .. } => &[],
        }
    }

    let total_pairs_checked = AtomicUsize::new(0);
    let failures = Mutex::new(Vec::<String>::new());

    pairs
        .par_iter()
        .for_each(|(name, before_path, after_path)| {
            let before_bytes =
                fs::read(before_path).unwrap_or_else(|err| panic!("[{name}] read before: {err}"));
            let after_bytes =
                fs::read(after_path).unwrap_or_else(|err| panic!("[{name}] read after: {err}"));

            let runtime = SimpleRuntime::new();

            let import_before = runtime
                .import_docx(&before_bytes)
                .unwrap_or_else(|err| panic!("[{name}] import before: {err:?}"));
            let import_after = runtime
                .import_docx(&after_bytes)
                .unwrap_or_else(|err| panic!("[{name}] import after: {err:?}"));

            let diff = runtime
                .diff(&import_before.doc_handle, &import_after.doc_handle)
                .unwrap_or_else(|err| panic!("[{name}] diff: {err:?}"));

            for (i, change) in diff.changes.iter().enumerate() {
                if let DiffChange::BlockModified { inline_changes, .. } = change {
                    // Exemption 1: bail-out blocks have zero Unchanged segments.
                    let has_unchanged = inline_changes
                        .iter()
                        .any(|c| matches!(c, InlineChange::Unchanged { .. }));
                    if !has_unchanged {
                        continue;
                    }

                    // Check each adjacent (Deleted, Inserted) pair.
                    for (j, window) in inline_changes.windows(2).enumerate() {
                        let (del_text, del_marks, ins_text, ins_marks) =
                            match (&window[0], &window[1]) {
                                (
                                    InlineChange::Deleted { text: dt, .. },
                                    InlineChange::Inserted { text: it, .. },
                                ) => (
                                    dt.as_str(),
                                    get_marks(&window[0]),
                                    it.as_str(),
                                    get_marks(&window[1]),
                                ),
                                _ => continue,
                            };

                        // Exemption 3: skip cross-mark pairs (formatting change, not content sub).
                        if del_marks != ins_marks {
                            continue;
                        }

                        let raw_prefix = common_prefix_len(del_text, ins_text);
                        let raw_suffix = common_suffix_len(del_text, ins_text, raw_prefix);
                        let prefix = word_aligned_prefix(del_text, ins_text, raw_prefix);
                        let suffix = word_aligned_suffix(del_text, ins_text, raw_suffix);

                        // Skip trivial overlap (less than ~one word on each side).
                        if prefix < 4 && suffix < 4 {
                            continue;
                        }

                        // When shared text covers the entire shorter text, the texts
                        // differ by at most one token — a token-level replacement is correct.
                        let shorter = del_text.len().min(ins_text.len());
                        if prefix + suffix >= shorter {
                            continue;
                        }

                        // Exemption 4: fused tokens. When the shared prefix and suffix
                        // are entirely word characters, the change is within a single
                        // fused token (e.g., apostrophe substitution in "Company's").
                        let prefix_text = &del_text[..prefix];
                        let suffix_text = &del_text[del_text.len() - suffix..];
                        let all_word = |s: &str| s.chars().all(|c| c.is_alphanumeric() || c == '_');
                        if all_word(prefix_text) && all_word(suffix_text) {
                            continue;
                        }

                        let shared = prefix + suffix;
                        let del_unique = del_text.len() - shared;
                        let ins_unique = ins_text.len() - shared;
                        let unique = del_unique.min(ins_unique);

                        total_pairs_checked.fetch_add(1, Ordering::Relaxed);

                        if shared > unique {
                            failures.lock().unwrap().push(format!(
                                "[{name}] change #{i} pair #{j}: Del/Ins share {shared} bytes \
                             but only change {unique} bytes (marks: {del_marks:?}).\n  \
                             deleted: {del_text:?}\n  inserted: {ins_text:?}"
                            ));
                        }
                    }
                }
            }
        });

    let total_pairs_checked = total_pairs_checked.load(Ordering::Relaxed);
    let failures = failures.into_inner().unwrap();
    assert!(
        total_pairs_checked > 0,
        "expected at least one Del/Ins pair with non-trivial shared text to check"
    );
    eprintln!(
        "diff inline_changes granularity invariant: checked {total_pairs_checked} Del/Ins \
         pairs across {} fixture pairs",
        pairs.len()
    );
    assert!(
        failures.is_empty(),
        "diff inline_changes granularity failures ({}):\n{}",
        failures.len(),
        failures
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

// ══════════════════════════════════════════════════════════════════════════
// Group 4: Fixpoint invariant — diff → merge → accept_all → re-diff is empty
// ══════════════════════════════════════════════════════════════════════════

/// Fixpoint invariant: `diff(accept_all(merge_diff(A, B, diff(A, B))), B) == empty`
///
/// This verifies that `diff → merge → accept_all` faithfully transforms A into B
/// in canonical space (no XML serialization). It catches bugs where the diff and
/// apply compensate for each other — accept/reject on XML might pass while the
/// canonical model is subtly wrong.
///
/// This invariant must hold for **all** fixtures — there are no exclusions.
#[test]
#[ignore] // Slow fixture sweep — run via `just nightly`
fn fixpoint_diff_then_apply_then_rediff_is_empty() {
    let all_pairs = discover_fixture_pairs();
    assert!(!all_pairs.is_empty(), "no fixture pairs found");
    let fixture_filter = selected_fixture_filter();

    let revision = RevisionInfo {
        revision_id: 1,
        identity: 0,
        author: Some("fixpoint-test".to_string()),
        date: Some("2025-06-01T00:00:00Z".to_string()),
        apply_op_id: None,
    };

    let testable_pairs: Vec<&(String, String, String)> = all_pairs
        .iter()
        .filter(|(name, _, _)| {
            if let Some(filter) = &fixture_filter {
                name == filter || name.contains(filter.as_str())
            } else {
                true
            }
        })
        .collect();

    let total_fixtures_checked = AtomicUsize::new(0);
    let failures = Mutex::new(Vec::<String>::new());

    testable_pairs
        .par_iter()
        .for_each(|(fixture_name, before_path, after_path)| {
            let before_bytes = fs::read(before_path)
                .unwrap_or_else(|err| panic!("[{fixture_name}] read before: {err}"));
            let after_bytes = fs::read(after_path)
                .unwrap_or_else(|err| panic!("[{fixture_name}] read after: {err}"));

            let runtime = SimpleRuntime::new();

            let import_before = runtime
                .import_docx(&before_bytes)
                .unwrap_or_else(|err| panic!("[{fixture_name}] import before: {err:?}"));
            let import_after = runtime
                .import_docx(&after_bytes)
                .unwrap_or_else(|err| panic!("[{fixture_name}] import after: {err:?}"));

            let canon_a = &import_before.canonical;
            let canon_b = &import_after.canonical;

            // Step 1: diff A → B
            let diff = diff_documents(canon_a, canon_b)
                .unwrap_or_else(|err| panic!("[{fixture_name}] diff_documents failed: {err}"));

            // Step 2: merge diff into A with tracked changes
            let mut merged = merge_diff(canon_a, canon_b, &diff, &revision)
                .unwrap_or_else(|err| panic!("[{fixture_name}] merge_diff failed: {err:?}"))
                .doc;

            // Step 3: accept all tracked changes
            accept_all(&mut merged);

            // Step 4: re-diff — should be empty
            let fixpoint_diff = diff_documents(&merged, canon_b).unwrap_or_else(|err| {
                panic!("[{fixture_name}] fixpoint diff_documents failed: {err}")
            });

            if !fixpoint_diff.changes.is_empty() {
                let descriptions: Vec<String> = fixpoint_diff
                    .changes
                    .iter()
                    .take(10)
                    .map(|c| match c {
                        DiffChange::BlockDeleted { old_text, .. } => {
                            format!("BlockDeleted: {:?}", truncate_for_diff(old_text, 120))
                        }
                        DiffChange::BlockInserted { block, .. } => {
                            format!(
                                "BlockInserted: {:?}",
                                format!("{block:?}").chars().take(120).collect::<String>()
                            )
                        }
                        DiffChange::BlockModified {
                            old_text,
                            new_text,
                            old_block,
                            new_block,
                            inline_changes,
                            ..
                        } => {
                            let text_identical = old_text == new_text;
                            let mut desc = if text_identical {
                                format!(
                                    "BlockModified (formatting-only): text={:?}",
                                    truncate_for_diff(old_text, 60)
                                )
                            } else {
                                format!(
                                    "BlockModified: old={:?} new={:?}",
                                    truncate_for_diff(old_text, 60),
                                    truncate_for_diff(new_text, 60)
                                )
                            };
                            // For formatting-only diffs, show what specifically differs
                            if text_identical
                                && let (BlockNode::Paragraph(ap), BlockNode::Paragraph(tp)) =
                                    (old_block, new_block)
                            {
                                // Check paragraph-level properties
                                if ap.style_id != tp.style_id {
                                    desc.push_str(&format!(
                                        "\n      pPr.style: {:?} vs {:?}",
                                        ap.style_id, tp.style_id
                                    ));
                                }
                                if !stemma::numbering_structurally_eq(&ap.numbering, &tp.numbering)
                                {
                                    desc.push_str(&format!(
                                        "\n      pPr.numbering: {:?} vs {:?}",
                                        ap.numbering.as_ref().map(|n| n.num_id),
                                        tp.numbering.as_ref().map(|n| n.num_id)
                                    ));
                                }
                                if ap.spacing != tp.spacing {
                                    desc.push_str("\n      pPr.spacing differs");
                                }
                                if ap.indent != tp.indent {
                                    desc.push_str("\n      pPr.indent differs");
                                }
                                // Check inline formatting — find first differing run
                                for ic in inline_changes {
                                    if let InlineChange::Unchanged {
                                        text,
                                        formatting_change: Some(fc),
                                        marks,
                                        style_props,
                                        ..
                                    } = ic
                                    {
                                        let mark_diff = if &fc.previous_marks != marks {
                                            format!(" marks:{:?}→{:?}", fc.previous_marks, marks)
                                        } else {
                                            String::new()
                                        };
                                        let font_ea_diff = if fc.previous_style_props.font_east_asia
                                            != style_props.font_east_asia
                                            || fc.previous_style_props.font_east_asia_theme
                                                != style_props.font_east_asia_theme
                                        {
                                            format!(
                                                " font_ea:{:?}/{:?}→{:?}/{:?}",
                                                fc.previous_style_props.font_east_asia,
                                                fc.previous_style_props.font_east_asia_theme,
                                                style_props.font_east_asia,
                                                style_props.font_east_asia_theme
                                            )
                                        } else {
                                            String::new()
                                        };
                                        desc.push_str(&format!(
                                            "\n      rPr on {:?}:{}{}",
                                            truncate_for_diff(text, 20),
                                            mark_diff,
                                            font_ea_diff
                                        ));
                                        break;
                                    }
                                }
                            }
                            desc
                        }
                        DiffChange::TableStructureChanged {
                            table_id,
                            old_hash,
                            new_hash,
                            ..
                        } => {
                            format!(
                                "TableStructureChanged: id={} old_hash={}.. new_hash={}..",
                                table_id.0,
                                &old_hash[..8.min(old_hash.len())],
                                &new_hash[..8.min(new_hash.len())]
                            )
                        }
                        other => format!("{other:?}").chars().take(200).collect(),
                    })
                    .collect();

                failures.lock().unwrap().push(format!(
                    "[{fixture_name}] {} residual change(s):\n    {}",
                    fixpoint_diff.changes.len(),
                    descriptions.join("\n    ")
                ));
            }

            total_fixtures_checked.fetch_add(1, Ordering::Relaxed);
        });

    let total_fixtures_checked = total_fixtures_checked.load(Ordering::Relaxed);
    let failures = failures.into_inner().unwrap();
    assert!(
        total_fixtures_checked > 0,
        "expected at least one fixture to be checked"
    );
    eprintln!(
        "fixpoint invariant: checked {} fixtures, {} failures, filter={:?}",
        total_fixtures_checked,
        failures.len(),
        fixture_filter,
    );
    assert!(
        failures.is_empty(),
        "fixpoint invariant violated: accept_all(merge_diff(A, B, diff(A, B))) differs from B \
         in {} fixture(s):\n{}",
        failures.len(),
        failures
            .iter()
            .take(40)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

// NOTE: the corpus-wide `source_change_id_atoms_match_full_doc_segments_sweep`
// invariant tests the app-layer changelet/source_change_id projection, which is
// not part of the stemma engine. It now lives with the
// consuming application's source_change_id invariant tests.
