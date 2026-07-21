//! Manual-markup detection and conversion.
//!
//! Some counterparties simulate Word's tracked-change markup by hand:
//! deletions become `strike`-formatted runs, insertions become colored
//! (typically red) runs. From an OOXML perspective these are ordinary
//! formatted runs; the audit pipeline cannot represent them as tracked
//! changes, so they're invisible to provenance reasoning and to any
//! Word-native review workflow.
//!
//! This module:
//!
//!   * Walks the IR with a small, narrow heuristic (see `classify_run`)
//!     and produces a `ManualMarkupReport` that surfaces the finding to
//!     the audit results page.
//!   * Converts detected manual markup into proper
//!     `TrackingStatus::Inserted` / `TrackingStatus::Deleted` segments,
//!     authored explicitly by a caller-supplied name. The conversion
//!     reuses `classify_run` and is the inverse of the detection step,
//!     so re-running detection on a converted document returns
//!     `detected: false` (idempotence).
//!
//! ## Heuristic (deliberately narrow)
//!
//! * **Deletion**: `style_props.strike == On` (and the run is not
//!   already inside a `TrackingStatus::Deleted` ancestor segment).
//! * **Insertion**: `style_props.color` is set to a redline-like color
//!   (`FF0000`, `C00000`, plus a small list of common red/orange/blue
//!   reviewer pens) AND it is not the document's dominant body color.
//!
//! Both classes early-out when the enclosing segment is already a
//! tracked `Inserted` / `Deleted` — native tracked changes carry their
//! own `w:rPr` color/strike inside `w:ins`/`w:del` and are not manual
//! markup.
//!
//! Detection requires at least 2 hits across at least 2 paragraphs
//! before reporting `detected: true`. A single accidentally-strike or
//! accidentally-red word is not enough to claim "this counterparty
//! simulated a redline".

use crate::domain::{
    BlockNode, CanonDoc, IStr, InlineNode, MarkValue, RevisionInfo, StyleProps, TableNode,
    TrackedBlock, TrackedSegment, TrackingStatus,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Allowed insertion colors (uppercase, no `#` prefix).
///
/// Restricted on purpose: any colored run is a candidate, but only a
/// short list of "reviewer pen" colors triggers a hit. We err toward
/// false negatives — flagging body text as a manual insertion would be
/// worse than missing one, since the user can always Reply and add it
/// by hand if the heuristic doesn't catch it.
const REDLINE_COLOR_PALETTE: &[&str] = &[
    // Reds — by far the most common manual-insertion color.
    "FF0000", "C00000", "B00000", "A00000", "800000", "CC0000", "DC143C", "B22222", "8B0000",
    // Oranges — sometimes used for "soft" insertions.
    "FF6600", "E36C0A", "ED7D31",
    // Blues — used by some reviewers, especially when red is reserved
    // for the original sender.
    "0000FF", "0070C0", "1F497D", "2E74B5",
];

/// Detected pattern for a single run.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum ManualMarkupKind {
    /// Strikethrough run (manual deletion).
    Deletion,
    /// Colored run (manual insertion).
    Insertion { color: IStr },
}

/// Aggregate report shape returned to the audit pipeline.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct ManualMarkupReport {
    /// True iff the report meets the minimum signal threshold
    /// (>= 2 total hits across >= 2 paragraphs).
    pub detected: bool,
    /// Total number of runs classified as manual insertions.
    pub insertion_count: usize,
    /// Total number of runs classified as manual deletions.
    pub deletion_count: usize,
    /// Number of paragraphs containing at least one classified run.
    pub paragraphs_affected: usize,
    /// Sample text from the first paragraph that contains a hit.
    /// Truncated to 120 chars; used by the UI banner for context.
    pub sample_text: Option<String>,
    /// The body's dominant color (if computable). Used to
    /// suppress hits whose color matches the body baseline.
    pub dominant_color: Option<String>,
    /// Per-paragraph hit summaries, one per paragraph that contains
    /// at least one classified run. Drives the service layer's
    /// proposal materialization (one EditProposal per entry).
    /// Always populated when ``detected`` is true; preserves the
    /// document order of the affected paragraphs.
    #[serde(default)]
    pub paragraph_hits: Vec<ParagraphMarkupHit>,
}

/// Per-paragraph manual-markup summary used both by the audit banner
/// (for context) and by the service layer (one EditProposal per
/// entry, scoped to this paragraph's id).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ParagraphMarkupHit {
    /// Stable IR id of the paragraph (matches FullDocBlock.block_id).
    pub paragraph_id: String,
    /// Number of insertion runs in this paragraph.
    pub insertion_count: usize,
    /// Number of deletion runs in this paragraph.
    pub deletion_count: usize,
    /// Truncated sample of the first matched run's text in this
    /// paragraph (<=120 chars). Surfaces in the proposal preview so
    /// the user knows what they're approving.
    pub sample_text: Option<String>,
}

/// Outcome of running `convert_manual_markup` on a document.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct ConversionReport {
    pub insertions_converted: usize,
    pub deletions_converted: usize,
    pub paragraphs_touched: usize,
}

/// Classify a run by its style props. Returns `None` if the run is
/// not a manual-markup candidate. Caller is responsible for the
/// "ancestor is already tracked" early-out.
pub fn classify_run(props: &StyleProps, dominant_color: Option<&str>) -> Option<ManualMarkupKind> {
    // Deletion takes precedence over insertion when both are present.
    // A run that is both struck-through AND colored red is most
    // sensibly a deletion; we'd rather convert it to a single `w:del`
    // than emit a confusing `w:ins` of strike-through text. This
    // matches how reviewers actually use the conventions.
    if matches!(props.strike, MarkValue::On) || matches!(props.double_strike, MarkValue::On) {
        return Some(ManualMarkupKind::Deletion);
    }

    let color = props.color.as_deref()?;
    let normalized = normalize_color_hex(color)?;

    // Skip if the color matches the document's dominant body color
    // (the run is not a redline, just a paragraph that happens to use
    // a non-default style). Without this guard, every body run in a
    // doc whose default text color is red would falsely trigger.
    if let Some(dom) = dominant_color
        && normalized.eq_ignore_ascii_case(dom)
    {
        return None;
    }

    if !REDLINE_COLOR_PALETTE
        .iter()
        .any(|c| normalized.eq_ignore_ascii_case(c))
    {
        return None;
    }

    Some(ManualMarkupKind::Insertion {
        color: IStr::from(normalized.as_str()),
    })
}

/// Normalize a color hex string to uppercase 6-char form, stripping a
/// leading `#`. Returns `None` for `"auto"` and other non-hex values.
fn normalize_color_hex(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_start_matches('#');
    if trimmed.eq_ignore_ascii_case("auto") {
        return None;
    }
    if trimmed.len() != 6 {
        return None;
    }
    if !trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(trimmed.to_ascii_uppercase())
}

/// Threshold (as a fraction of all body runs) above which an
/// explicit color is treated as the document's baseline body color.
///
/// Set high (90%) on purpose. We're protecting against documents
/// whose true baseline body color happens to be in our redline
/// palette — those overwhelmingly use that color *throughout*. A
/// handful of colored runs in an otherwise plain document (the
/// case we want to detect) won't cross this threshold.
const DOMINANT_COLOR_RATIO: f32 = 0.9;

/// Compute the dominant explicit color across all body runs that are
/// NOT already inside a tracked `Inserted` / `Deleted` ancestor. Used
/// as a "style baseline" so we don't flag every body run in a
/// document where the default font color happens to be in our
/// redline palette.
///
/// A color is considered dominant only if it appears on more than
/// `DOMINANT_COLOR_RATIO` of all body text runs (colored or not).
/// This makes sure isolated colored runs that all happen to share a
/// color don't get suppressed — the suppression is for documents
/// with a true non-default body color, not for the "two red words"
/// case we're trying to detect.
///
/// Returns `None` when no color crosses the dominance threshold,
/// which is the common case.
fn compute_dominant_color(doc: &CanonDoc) -> Option<String> {
    let mut color_counts: HashMap<String, usize> = HashMap::new();
    let mut total_runs: usize = 0;
    for tb in &doc.blocks {
        if !matches!(tb.status, TrackingStatus::Normal) {
            continue;
        }
        walk_block_for_color_counts(&tb.block, &mut color_counts, &mut total_runs);
    }
    if total_runs == 0 {
        return None;
    }
    let (color, count) = color_counts.into_iter().max_by_key(|(_, n)| *n)?;
    let ratio = count as f32 / total_runs as f32;
    if ratio > DOMINANT_COLOR_RATIO {
        Some(color)
    } else {
        None
    }
}

fn walk_block_for_color_counts(
    block: &BlockNode,
    counts: &mut HashMap<String, usize>,
    total_runs: &mut usize,
) {
    match block {
        BlockNode::Paragraph(p) => {
            for seg in &p.segments {
                if !matches!(seg.status, TrackingStatus::Normal) {
                    continue;
                }
                for inl in &seg.inlines {
                    if let InlineNode::Text(t) = inl {
                        *total_runs += 1;
                        if let Some(c) = t.style_props.color.as_deref()
                            && let Some(norm) = normalize_color_hex(c)
                        {
                            *counts.entry(norm).or_insert(0) += 1;
                        }
                    }
                }
            }
        }
        BlockNode::Table(t) => walk_table_for_color_counts(t, counts, total_runs),
        BlockNode::OpaqueBlock(_) => {}
    }
}

fn walk_table_for_color_counts(
    table: &TableNode,
    counts: &mut HashMap<String, usize>,
    total_runs: &mut usize,
) {
    for row in &table.rows {
        for cell in &row.cells {
            for block in &cell.blocks {
                walk_block_for_color_counts(block, counts, total_runs);
            }
        }
    }
}

/// Detect manual-markup hits across the document body. Header /
/// footer / footnote stories are intentionally excluded — they're
/// chrome and the heuristic noise is higher there.
pub fn detect_manual_markup(doc: &CanonDoc) -> ManualMarkupReport {
    let dominant = compute_dominant_color(doc);
    let dom_ref = dominant.as_deref();

    let mut report = ManualMarkupReport {
        dominant_color: dominant.clone(),
        ..ManualMarkupReport::default()
    };
    let mut sample: Option<String> = None;

    for tb in &doc.blocks {
        // Skip the entire block if its TrackedBlock-level status is
        // already Inserted / Deleted — every nested run is a native
        // tracked change.
        if !matches!(tb.status, TrackingStatus::Normal) {
            continue;
        }
        scan_block(&tb.block, dom_ref, &mut report, &mut sample);
    }
    report.paragraphs_affected = report.paragraph_hits.len();
    report.sample_text = sample;
    let total = report.insertion_count + report.deletion_count;
    report.detected = total >= 2 && report.paragraphs_affected >= 2;
    report
}

fn scan_block(
    block: &BlockNode,
    dominant_color: Option<&str>,
    report: &mut ManualMarkupReport,
    sample: &mut Option<String>,
) {
    match block {
        BlockNode::Paragraph(p) => {
            let mut paragraph_ins = 0usize;
            let mut paragraph_del = 0usize;
            let mut paragraph_sample: Option<String> = None;
            for seg in &p.segments {
                // Per heuristic: skip runs already inside a tracked
                // ancestor. Native tracked changes are handled by the
                // tracked-changes pipeline, not by manual-markup
                // detection.
                if !matches!(seg.status, TrackingStatus::Normal) {
                    continue;
                }
                for inl in &seg.inlines {
                    let InlineNode::Text(t) = inl else { continue };
                    let Some(kind) = classify_run(&t.style_props, dominant_color) else {
                        continue;
                    };
                    match kind {
                        ManualMarkupKind::Insertion { .. } => paragraph_ins += 1,
                        ManualMarkupKind::Deletion => paragraph_del += 1,
                    }
                    if paragraph_sample.is_none() && !t.text.trim().is_empty() {
                        let text = t.text.trim();
                        let truncated: String = text.chars().take(120).collect();
                        paragraph_sample = Some(truncated);
                    }
                }
            }
            if paragraph_ins == 0 && paragraph_del == 0 {
                return;
            }
            report.insertion_count += paragraph_ins;
            report.deletion_count += paragraph_del;
            if sample.is_none() {
                sample.clone_from(&paragraph_sample);
            }
            report.paragraph_hits.push(ParagraphMarkupHit {
                paragraph_id: p.id.0.to_string(),
                insertion_count: paragraph_ins,
                deletion_count: paragraph_del,
                sample_text: paragraph_sample,
            });
        }
        BlockNode::Table(t) => scan_table(t, dominant_color, report, sample),
        BlockNode::OpaqueBlock(_) => {}
    }
}

fn scan_table(
    table: &TableNode,
    dominant_color: Option<&str>,
    report: &mut ManualMarkupReport,
    sample: &mut Option<String>,
) {
    for row in &table.rows {
        if row
            .tracking_status
            .as_ref()
            .is_some_and(|s| !matches!(s, TrackingStatus::Normal))
        {
            continue;
        }
        for cell in &row.cells {
            if cell
                .tracking_status
                .as_ref()
                .is_some_and(|s| !matches!(s, TrackingStatus::Normal))
            {
                continue;
            }
            for block in &cell.blocks {
                scan_block(block, dominant_color, report, sample);
            }
        }
    }
}

/// Convert manual-markup runs into proper tracked-change segments.
///
/// For each detected run, the run is split out of its enclosing
/// `Normal` segment into its own `Inserted` / `Deleted` segment with
/// the supplied `RevisionInfo`. The strike/color marks that triggered
/// detection are cleared from the new run's `style_props`, since they
/// are now redundant (the wrapping segment carries the meaning).
///
/// Adjacent runs in the same paragraph that classify the same way are
/// coalesced into a single tracked segment to avoid 1-character
/// `w:ins` / `w:del` runs.
///
/// `paragraph_filter` — when `Some`, only paragraphs whose id appears
/// in the set are converted; the rest are passed through unchanged.
/// `None` converts every detected run in the document body. Used by
/// the proposal-accept path to scope conversion to a single paragraph
/// at a time.
///
/// Returns the number of runs converted. Does NOT touch runs that are
/// already inside a tracked-change segment (idempotence).
pub fn convert_manual_markup(
    doc: &mut CanonDoc,
    author: &str,
    date: &str,
    revision_id: u32,
    apply_op_id: Option<String>,
    paragraph_filter: Option<&std::collections::HashSet<String>>,
) -> ConversionReport {
    assert!(
        !author.trim().is_empty(),
        "convert_manual_markup: author must be a non-empty string"
    );
    let dominant = compute_dominant_color(doc);
    let dom_ref = dominant.as_deref();

    let mut report = ConversionReport::default();
    for tb in doc.blocks.iter_mut() {
        convert_block(
            tb,
            dom_ref,
            author,
            date,
            revision_id,
            apply_op_id.as_deref(),
            paragraph_filter,
            &mut report,
        );
    }
    // H7: this is a revision PRODUCER (manual color/strike markup → real
    // tracked changes); mint stable identities for what it created.
    crate::import::mint_identities(doc);
    // H2: one unified body-state validator after the direct manual-markup
    // materialization producer.
    crate::tracked_model::debug_assert_body_invariants(doc, "convert_manual_markup");
    report
}

#[allow(clippy::too_many_arguments)]
fn convert_block(
    tb: &mut TrackedBlock,
    dominant_color: Option<&str>,
    author: &str,
    date: &str,
    revision_id: u32,
    apply_op_id: Option<&str>,
    paragraph_filter: Option<&std::collections::HashSet<String>>,
    report: &mut ConversionReport,
) {
    if !matches!(tb.status, TrackingStatus::Normal) {
        return;
    }
    convert_block_inner(
        &mut tb.block,
        dominant_color,
        author,
        date,
        revision_id,
        apply_op_id,
        paragraph_filter,
        report,
    );
}

#[allow(clippy::too_many_arguments)]
fn convert_block_inner(
    block: &mut BlockNode,
    dominant_color: Option<&str>,
    author: &str,
    date: &str,
    revision_id: u32,
    apply_op_id: Option<&str>,
    paragraph_filter: Option<&std::collections::HashSet<String>>,
    report: &mut ConversionReport,
) {
    match block {
        BlockNode::Paragraph(p) => {
            // Honor the optional paragraph filter — proposal-scoped
            // accepts only convert one paragraph at a time.
            if let Some(filter) = paragraph_filter
                && !filter.contains(p.id.0.as_ref())
            {
                return;
            }
            let touched = convert_paragraph_segments(
                &mut p.segments,
                dominant_color,
                author,
                date,
                revision_id,
                apply_op_id,
                report,
            );
            if touched {
                report.paragraphs_touched += 1;
            }
        }
        BlockNode::Table(table) => {
            for row in table.rows.iter_mut() {
                if row
                    .tracking_status
                    .as_ref()
                    .is_some_and(|s| !matches!(s, TrackingStatus::Normal))
                {
                    continue;
                }
                for cell in row.cells.iter_mut() {
                    if cell
                        .tracking_status
                        .as_ref()
                        .is_some_and(|s| !matches!(s, TrackingStatus::Normal))
                    {
                        continue;
                    }
                    for block in cell.blocks.iter_mut() {
                        convert_block_inner(
                            block,
                            dominant_color,
                            author,
                            date,
                            revision_id,
                            apply_op_id,
                            paragraph_filter,
                            report,
                        );
                    }
                }
            }
        }
        BlockNode::OpaqueBlock(_) => {}
    }
}

/// For each `Normal` segment, walk its inlines and split out
/// classified runs into new `Inserted` / `Deleted` segments.
/// Coalesces adjacent same-classification runs in the same paragraph
/// to avoid sub-word `w:ins` / `w:del` chunking.
///
/// Returns true if at least one run was converted in this paragraph.
fn convert_paragraph_segments(
    segments: &mut Vec<TrackedSegment>,
    dominant_color: Option<&str>,
    author: &str,
    date: &str,
    revision_id: u32,
    apply_op_id: Option<&str>,
    report: &mut ConversionReport,
) -> bool {
    let mut new_segments: Vec<TrackedSegment> = Vec::with_capacity(segments.len());
    let mut touched = false;
    for seg in segments.drain(..) {
        if !matches!(seg.status, TrackingStatus::Normal) {
            // Pre-existing tracked segment — pass through unchanged.
            // Idempotence: nothing inside is a manual-markup
            // candidate.
            new_segments.push(seg);
            continue;
        }
        // Walk inlines, building a "current group" of consecutive
        // inlines that share a classification (Normal vs Insertion vs
        // Deletion). Flush the group when the classification changes.
        let mut current: Vec<InlineNode> = Vec::new();
        let mut current_kind: GroupKind = GroupKind::Normal;
        let inlines = seg.inlines;
        for inl in inlines {
            let kind = match &inl {
                InlineNode::Text(t) => match classify_run(&t.style_props, dominant_color) {
                    Some(ManualMarkupKind::Deletion) => GroupKind::Deleted,
                    Some(ManualMarkupKind::Insertion { .. }) => GroupKind::Inserted,
                    None => GroupKind::Normal,
                },
                _ => GroupKind::Normal,
            };
            if kind != current_kind && !current.is_empty() {
                flush_group(
                    &mut new_segments,
                    std::mem::take(&mut current),
                    current_kind,
                    author,
                    date,
                    revision_id,
                    apply_op_id,
                );
            }
            current_kind = kind;
            // For a converted run, strip the strike / color marks so
            // the wrapping tracked segment is the sole carrier of
            // meaning.
            let prepared = match (kind, inl) {
                (GroupKind::Deleted, InlineNode::Text(mut t)) => {
                    t.style_props.strike = MarkValue::Inherit;
                    t.style_props.double_strike = MarkValue::Inherit;
                    t.rpr_authored.color = t.rpr_authored.color && t.style_props.color.is_some();
                    InlineNode::Text(t)
                }
                (GroupKind::Inserted, InlineNode::Text(mut t)) => {
                    t.style_props.color = None;
                    t.style_props.color_theme = None;
                    t.rpr_authored.color = false;
                    t.rpr_authored.color_theme = false;
                    InlineNode::Text(t)
                }
                (_, other) => other,
            };
            if matches!(current_kind, GroupKind::Deleted | GroupKind::Inserted) {
                touched = true;
                match current_kind {
                    GroupKind::Deleted => report.deletions_converted += 1,
                    GroupKind::Inserted => report.insertions_converted += 1,
                    GroupKind::Normal => unreachable!(),
                }
            }
            current.push(prepared);
        }
        if !current.is_empty() {
            flush_group(
                &mut new_segments,
                current,
                current_kind,
                author,
                date,
                revision_id,
                apply_op_id,
            );
        }
    }
    *segments = new_segments;
    touched
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GroupKind {
    Normal,
    Inserted,
    Deleted,
}

fn flush_group(
    out: &mut Vec<TrackedSegment>,
    inlines: Vec<InlineNode>,
    kind: GroupKind,
    author: &str,
    date: &str,
    revision_id: u32,
    apply_op_id: Option<&str>,
) {
    let status = match kind {
        GroupKind::Normal => TrackingStatus::Normal,
        GroupKind::Inserted => TrackingStatus::Inserted(RevisionInfo {
            revision_id,
            identity: 0,
            author: Some(author.to_string()),
            date: Some(date.to_string()),
            apply_op_id: apply_op_id.map(|s| s.to_string()),
        }),
        GroupKind::Deleted => TrackingStatus::Deleted(RevisionInfo {
            revision_id,
            identity: 0,
            author: Some(author.to_string()),
            date: Some(date.to_string()),
            apply_op_id: apply_op_id.map(|s| s.to_string()),
        }),
    };
    out.push(TrackedSegment { status, inlines });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::*;

    fn empty_doc() -> CanonDoc {
        CanonDoc {
            id: NodeId::from("doc"),
            blocks: vec![],
            meta: DocMeta {
                schema_version: "test".to_string(),
                docx_fingerprint: DocFingerprint("fp".to_string()),
                internal_ids_version: "v0".to_string(),
            },
            headers: vec![],
            footers: vec![],
            footnotes: vec![],
            endnotes: vec![],
            comments: vec![],
            comments_extended: vec![],
            body_section_properties: None,
            body_section_property_change: None,
            compat_settings: CompatSettings::default(),
            even_and_odd_headers: None,
            document_background: None,
            document_protection: None,
        }
    }

    fn run(text: &str, props: StyleProps) -> InlineNode {
        InlineNode::from(TextNode {
            id: NodeId::from(format!("t-{text}")),
            text_role: None,
            text: text.to_string(),
            marks: vec![],
            style_props: props,
            rpr_authored: RunRprAuthored::default(),
            source_run_attrs: Vec::new(),
            formatting_change: None,
        })
    }

    fn red_props() -> StyleProps {
        StyleProps {
            color: Some(IStr::from("FF0000")),
            ..StyleProps::default()
        }
    }

    fn strike_props() -> StyleProps {
        StyleProps {
            strike: MarkValue::On,
            ..StyleProps::default()
        }
    }

    fn para_id(idx: usize) -> NodeId {
        NodeId::from(format!("p-{idx}"))
    }

    fn paragraph(idx: usize, segments: Vec<TrackedSegment>) -> TrackedBlock {
        TrackedBlock {
            status: TrackingStatus::Normal,
            block: BlockNode::from(ParagraphNode {
                id: para_id(idx),
                style_id: None,
                align: None,
                has_direct_align: false,
                indent: None,
                has_direct_indent: false,
                authored_indent: None,
                spacing: None,
                has_direct_spacing: false,
                authored_spacing: None,
                borders: None,
                keep_next: None,
                keep_lines: None,
                page_break_before: false,
                widow_control: None,
                contextual_spacing: None,
                shading: None,
                has_direct_keep_next: true,
                has_direct_keep_lines: true,
                has_direct_page_break_before: true,
                has_direct_widow_control: true,
                has_direct_contextual_spacing: true,
                has_direct_shading: true,
                has_direct_borders: true,
                tab_stops: vec![],
                effective_tab_stops_rel: vec![],
                segments,
                block_text_hash: None,
                numbering: None,
                has_direct_numbering: true,
                numbering_suppressed: false,
                materialized_numbering: None,
                rendered_text: None,
                literal_prefix: None,
                literal_prefix_marks: vec![],
                literal_prefix_style_props: StyleProps::default(),
                literal_prefix_rpr_authored: RunRprAuthored::default(),
                literal_prefix_leading_rpr: None,
                literal_prefix_trailing_rpr: None,
                literal_prefix_leading_tab_twips: None,
                literal_prefix_leading_tab_count: 0,
                literal_prefix_leading_ws: String::new(),
                literal_prefix_trailing_ws: String::new(),
                literal_prefix_has_trailing_tab: false,
                literal_prefix_trailing_tab_stop_twips: None,
                outline_lvl: None,
                heading_level: None,
                para_mark_status: None,
                paragraph_mark_marks: vec![],
                paragraph_mark_style_props: StyleProps::default(),
                paragraph_mark_rpr_off: Default::default(),
                para_split: false,
                section_property_change: None,
                formatting_change: None,
                section_properties: None,
                mirror_indents: None,
                auto_space_de: None,
                auto_space_dn: None,
                bidi: None,
                text_alignment: None,
                text_direction: None,
                suppress_auto_hyphens: None,
                snap_to_grid: None,
                overflow_punct: None,
                adjust_right_ind: None,
                word_wrap: None,
                frame_pr: None,
                para_id: None,
                text_id: None,
                cnf_style: None,
                preserved_ppr: Vec::new(),
            }),
            move_id: None,
            block_sdt_wrap: None,
        }
    }

    fn normal_segment(inlines: Vec<InlineNode>) -> TrackedSegment {
        TrackedSegment {
            status: TrackingStatus::Normal,
            inlines,
        }
    }

    #[test]
    fn classify_pure_red_insertion() {
        let kind = classify_run(&red_props(), None);
        match kind {
            Some(ManualMarkupKind::Insertion { color }) => {
                assert_eq!(color.as_ref(), "FF0000");
            }
            _ => panic!("expected Insertion, got {kind:?}"),
        }
    }

    #[test]
    fn classify_pure_strike_deletion() {
        let kind = classify_run(&strike_props(), None);
        assert_eq!(kind, Some(ManualMarkupKind::Deletion));
    }

    #[test]
    fn strike_plus_color_classifies_as_deletion() {
        // Heuristic ranks deletion above insertion when both signals
        // are present. Spec rationale: a struck-through red word is
        // most reasonably a deletion in someone's manual review
        // convention.
        let mut props = strike_props();
        props.color = Some(IStr::from("FF0000"));
        assert_eq!(classify_run(&props, None), Some(ManualMarkupKind::Deletion));
    }

    #[test]
    fn dominant_body_color_suppresses_insertion_hits() {
        // If the document's baseline text color is FF0000, a red run
        // is not a manual insertion — it's just body text.
        let kind = classify_run(&red_props(), Some("FF0000"));
        assert!(kind.is_none());
    }

    #[test]
    fn auto_color_does_not_classify() {
        let props = StyleProps {
            color: Some(IStr::from("auto")),
            ..StyleProps::default()
        };
        assert!(classify_run(&props, None).is_none());
    }

    #[test]
    fn non_palette_color_does_not_classify() {
        // Green is not in the redline palette.
        let props = StyleProps {
            color: Some(IStr::from("00FF00")),
            ..StyleProps::default()
        };
        assert!(classify_run(&props, None).is_none());
    }

    #[test]
    fn detect_threshold_requires_two_paragraphs_and_two_hits() {
        // Single-paragraph hits MUST NOT flip `detected` even when
        // there are two of them — the threshold guards against
        // a paragraph that happens to use a single colored phrase
        // for emphasis. Includes plenty of plain runs so the red
        // runs stay below the dominant-color threshold.
        let mut doc = empty_doc();
        doc.blocks = vec![paragraph(
            0,
            vec![normal_segment(vec![
                run("plain1 ", StyleProps::default()),
                run("plain2 ", StyleProps::default()),
                run("a", red_props()),
                run(" plain3 ", StyleProps::default()),
                run("b", red_props()),
            ])],
        )];
        let r = detect_manual_markup(&doc);
        assert!(
            !r.detected,
            "single-paragraph hits should not flip detected"
        );
        assert_eq!(r.insertion_count, 2);
        assert_eq!(r.paragraphs_affected, 1);
    }

    #[test]
    fn detect_two_paragraphs_two_hits_triggers() {
        let mut doc = empty_doc();
        doc.blocks = vec![
            paragraph(0, vec![normal_segment(vec![run("a", red_props())])]),
            paragraph(1, vec![normal_segment(vec![run("b", strike_props())])]),
        ];
        let r = detect_manual_markup(&doc);
        assert!(r.detected);
        assert_eq!(r.insertion_count, 1);
        assert_eq!(r.deletion_count, 1);
        assert_eq!(r.paragraphs_affected, 2);
    }

    #[test]
    fn detect_skips_runs_inside_tracked_segments() {
        // Native tracked-change content carries its own w:rPr inside
        // w:ins / w:del. We must NOT double-count those as manual
        // markup, otherwise a tracked deletion would generate both a
        // tracked-change card AND a manual-markup banner.
        let mut doc = empty_doc();
        let tracked_seg = TrackedSegment {
            status: TrackingStatus::Deleted(RevisionInfo {
                revision_id: 1,
                identity: 0,
                author: Some("Native".to_string()),
                date: None,
                apply_op_id: None,
            }),
            // Even though this run carries strike + red color, it is
            // already inside a Deleted segment and must not be
            // counted.
            inlines: vec![run("native", {
                let mut p = strike_props();
                p.color = Some(IStr::from("FF0000"));
                p
            })],
        };
        doc.blocks = vec![paragraph(0, vec![tracked_seg])];
        let r = detect_manual_markup(&doc);
        assert!(!r.detected);
        assert_eq!(r.insertion_count, 0);
        assert_eq!(r.deletion_count, 0);
    }

    #[test]
    fn convert_idempotent_after_first_pass() {
        let mut doc = empty_doc();
        doc.blocks = vec![
            paragraph(
                0,
                vec![normal_segment(vec![
                    run("hello ", StyleProps::default()),
                    run("world", red_props()),
                ])],
            ),
            paragraph(
                1,
                vec![normal_segment(vec![
                    run("foo", strike_props()),
                    run(" bar", StyleProps::default()),
                ])],
            ),
        ];
        let report = convert_manual_markup(
            &mut doc,
            "Reviewer",
            "2026-05-06T00:00:00Z",
            100,
            None,
            None,
        );
        assert_eq!(report.insertions_converted, 1);
        assert_eq!(report.deletions_converted, 1);
        assert_eq!(report.paragraphs_touched, 2);

        // Detect should now report nothing.
        let after = detect_manual_markup(&doc);
        assert!(!after.detected);
        assert_eq!(after.insertion_count, 0);
        assert_eq!(after.deletion_count, 0);

        // A second conversion pass should be a no-op.
        let second = convert_manual_markup(
            &mut doc,
            "Reviewer",
            "2026-05-06T00:00:00Z",
            101,
            None,
            None,
        );
        assert_eq!(second.insertions_converted, 0);
        assert_eq!(second.deletions_converted, 0);
    }

    #[test]
    fn convert_coalesces_adjacent_same_kind_runs() {
        // Two adjacent red runs in the same paragraph must produce
        // ONE Inserted segment, not two — to avoid sub-word w:ins
        // chunking which Word renders as separate insertions.
        let mut doc = empty_doc();
        doc.blocks = vec![paragraph(
            0,
            vec![normal_segment(vec![
                run("baseline ", StyleProps::default()),
                run("red ", red_props()),
                run("again", red_props()),
            ])],
        )];
        // Need two paragraphs for the detection threshold but the
        // coalescing assertion is purely on the converted segments
        // for paragraph 0.
        doc.blocks.push(paragraph(
            1,
            vec![normal_segment(vec![run("x", red_props())])],
        ));

        convert_manual_markup(
            &mut doc,
            "Reviewer",
            "2026-05-06T00:00:00Z",
            100,
            None,
            None,
        );

        let BlockNode::Paragraph(p0) = &doc.blocks[0].block else {
            panic!("expected paragraph")
        };
        // Expect: [Normal("baseline ")] + [Inserted("red " + "again")].
        assert_eq!(p0.segments.len(), 2);
        assert!(matches!(p0.segments[0].status, TrackingStatus::Normal));
        assert!(matches!(p0.segments[1].status, TrackingStatus::Inserted(_)));
        assert_eq!(p0.segments[1].inlines.len(), 2);
    }

    #[test]
    fn convert_clears_strike_and_color_on_converted_runs() {
        let mut doc = empty_doc();
        doc.blocks = vec![
            paragraph(0, vec![normal_segment(vec![run("ins", red_props())])]),
            paragraph(1, vec![normal_segment(vec![run("del", strike_props())])]),
        ];
        convert_manual_markup(
            &mut doc,
            "Reviewer",
            "2026-05-06T00:00:00Z",
            100,
            None,
            None,
        );

        let BlockNode::Paragraph(p0) = &doc.blocks[0].block else {
            unreachable!()
        };
        let InlineNode::Text(t0) = &p0.segments[0].inlines[0] else {
            unreachable!()
        };
        assert!(t0.style_props.color.is_none(), "color must be cleared");

        let BlockNode::Paragraph(p1) = &doc.blocks[1].block else {
            unreachable!()
        };
        let InlineNode::Text(t1) = &p1.segments[0].inlines[0] else {
            unreachable!()
        };
        assert_eq!(
            t1.style_props.strike,
            MarkValue::Inherit,
            "strike must be cleared"
        );
    }

    #[test]
    #[should_panic(expected = "author must be a non-empty string")]
    fn convert_rejects_empty_author() {
        let mut doc = empty_doc();
        convert_manual_markup(&mut doc, "   ", "2026-05-06T00:00:00Z", 100, None, None);
    }

    #[test]
    fn detect_populates_one_paragraph_hit_per_affected_paragraph() {
        // Per-paragraph hit list must contain one entry per affected
        // paragraph in document order. The service layer materializes
        // one EditProposal per entry, so missing or duplicate entries
        // would produce a wrong proposal queue.
        let mut doc = empty_doc();
        doc.blocks = vec![
            paragraph(0, vec![normal_segment(vec![run("a", red_props())])]),
            paragraph(
                1,
                vec![normal_segment(vec![
                    run("b", strike_props()),
                    run("c", red_props()),
                ])],
            ),
        ];
        let report = detect_manual_markup(&doc);
        assert!(report.detected);
        assert_eq!(report.paragraph_hits.len(), 2);
        // Order matches document order.
        assert_eq!(report.paragraph_hits[0].paragraph_id, "p-0");
        assert_eq!(report.paragraph_hits[0].insertion_count, 1);
        assert_eq!(report.paragraph_hits[0].deletion_count, 0);
        assert_eq!(report.paragraph_hits[1].paragraph_id, "p-1");
        assert_eq!(report.paragraph_hits[1].insertion_count, 1);
        assert_eq!(report.paragraph_hits[1].deletion_count, 1);
        // Every paragraph that contains a non-whitespace hit must carry
        // a sample_text. The Python receiver
        // (`_parse_paragraph_markup_hit`) and the audit-page banner
        // both read this; an empty hit list with non-zero
        // ``paragraphs_affected`` is the bug
        // ``audit-manual-markup-paragraph-hits-empty`` documents.
        assert_eq!(report.paragraph_hits[0].sample_text.as_deref(), Some("a"),);
        assert!(
            report.paragraph_hits[1].sample_text.is_some(),
            "second paragraph must also carry a sample"
        );
        // Aggregate ``paragraphs_affected`` must equal the wire length
        // of ``paragraph_hits`` — they are two views of the same data
        // and divergence is the bug described above.
        assert_eq!(report.paragraphs_affected, report.paragraph_hits.len());
    }

    #[test]
    fn convert_paragraph_filter_only_touches_listed_paragraphs() {
        // Proposal-scoped accept supplies a single-element filter so
        // accepting one proposal at a time only converts its
        // paragraph, leaving others untouched until their own
        // proposals are accepted. Plain runs included so the
        // dominant-color guard cannot suppress the red runs.
        let mut doc = empty_doc();
        doc.blocks = vec![
            paragraph(
                0,
                vec![normal_segment(vec![
                    run("plain0 ", StyleProps::default()),
                    run("ins0", red_props()),
                ])],
            ),
            paragraph(
                1,
                vec![normal_segment(vec![
                    run("plain1 ", StyleProps::default()),
                    run("ins1", red_props()),
                ])],
            ),
            paragraph(
                2,
                vec![normal_segment(vec![run(
                    "plain2 plain plain plain plain",
                    StyleProps::default(),
                )])],
            ),
        ];
        let mut filter = std::collections::HashSet::new();
        filter.insert("p-1".to_string());
        let report = convert_manual_markup(
            &mut doc,
            "Reviewer",
            "2026-05-06T00:00:00Z",
            100,
            None,
            Some(&filter),
        );
        assert_eq!(report.paragraphs_touched, 1);
        assert_eq!(report.insertions_converted, 1);

        // Paragraph 0 still has its original Normal segment with the
        // colored run untouched (would be re-detected if scanned).
        let BlockNode::Paragraph(p0) = &doc.blocks[0].block else {
            unreachable!()
        };
        assert_eq!(p0.segments.len(), 1);
        assert!(matches!(p0.segments[0].status, TrackingStatus::Normal));

        // Paragraph 1 now has its colored run wrapped in an Inserted
        // segment.
        let BlockNode::Paragraph(p1) = &doc.blocks[1].block else {
            unreachable!()
        };
        assert!(
            p1.segments
                .iter()
                .any(|s| matches!(s.status, TrackingStatus::Inserted(_)))
        );
    }
}
