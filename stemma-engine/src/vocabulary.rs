//! Vocabulary extraction — groups paragraphs by formatting pattern and assigns
//! human-readable "roles" so an LLM can reference roles instead of raw formatting.

use std::collections::HashMap;

use serde::Serialize;

use crate::domain::{
    Alignment, BlockNode, CanonDoc, HeadingLevel, InlineNode, Mark, MarkValue, NodeId,
    ParagraphNode, TableNode, TrackedBlock,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const INDENT_BUCKET_TW: i32 = 72; // ~0.05 inch
#[cfg(test)]
const SPACING_BUCKET_TW: i32 = 60; // ~3pt

/// Font-size threshold (in half-points) above which centered+bold text is
/// promoted to "title" rather than "centered_heading".
const TITLE_FONT_SIZE_THRESHOLD: u32 = 28;

/// Minimum occurrences for the defined-term heuristic to fire.
const DEFINED_TERM_MIN_OCCURRENCES: usize = 3;

/// Font sizes within this many half-points of the document default are
/// normalized to the default (avoids splitting on explicit-11pt vs inherited-11pt).
const FONT_SIZE_MERGE_TOLERANCE: u32 = 1;

/// The universal set of inline marks every document vocabulary exposes.
const INLINE_MARKS: &[&str] = &[
    "bold",
    "italic",
    "underline",
    "strike",
    "subscript",
    "superscript",
];

// ---------------------------------------------------------------------------
// Public output types
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
pub struct DocumentVocabulary {
    pub paragraph_roles: Vec<ParagraphRole>,
    pub inline_roles: Vec<InlineRole>,
    pub table_roles: Vec<TableRole>,
    /// Universal inline marks: bold, italic, underline, strike, subscript, superscript.
    pub inline_marks: Vec<&'static str>,
}

pub use crate::numbering::NumberingSource;

/// How frequently a role appears in the document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleFrequency {
    /// ≥20% of paragraphs — the dominant formatting in the document.
    Primary,
    /// 5–19% of paragraphs.
    Common,
    /// 3+ occurrences but <5%.
    Minor,
    /// 1–2 occurrences — likely a one-off (title, signature, notice).
    Rare,
}

#[derive(Clone, Debug, Serialize)]
pub struct ParagraphRole {
    pub id: String,
    pub description: String,
    pub exemplar: NodeId,
    /// Short text snippet from the exemplar paragraph (≤80 chars, for LLM context).
    pub exemplar_text: String,
    pub frequency: RoleFrequency,
    pub has_numbering: bool,
    pub numbering_source: Option<NumberingSource>,
    pub count: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct InlineRole {
    pub id: String,
    pub description: String,
    pub exemplar_para: NodeId,
    pub exemplar_run_index: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct TableRole {
    pub id: String,
    pub description: String,
    pub exemplar: NodeId,
    pub count: usize,
}

// ---------------------------------------------------------------------------
// Internal signature types
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct FormattingSignature {
    style_id: Option<String>,
    heading_level: Option<u8>,
    has_numbering: bool,
    numbering_ilvl: Option<u32>,
    numbering_kind: NumberingKind,
    /// Alignment discriminant; None normalized to Left (0).
    align: u8,
    /// Left indent, quantized. Only included for unstyled paragraphs (style_id
    /// is None or "Normal"). For styled paragraphs, indent is a direct-formatting
    /// override that shouldn't create a separate role.
    indent_left_bucket: i32,
    dominant_bold: bool,
    dominant_all_caps: bool,
    /// Font size relative to document default. None = default or near-default.
    /// Only Some when the paragraph's dominant size differs meaningfully.
    dominant_font_size: Option<u32>,
    has_borders: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum NumberingKind {
    None,
    Bullet,
    Numbered,
}

/// Collected info about a single paragraph during the walk.
struct ParagraphInfo {
    node_id: NodeId,
    signature: FormattingSignature,
    numbering_source: Option<NumberingSource>,
    /// Short text snippet for the exemplar (≤80 chars).
    text_snippet: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct TableSignature {
    style_id: Option<String>,
    col_count: usize,
    has_borders: bool,
    has_header_row: bool,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// The shared paragraph-grouping core: walk the document, group paragraphs by
/// formatting signature (document-order deterministic), and assign each group
/// its descriptive role id. Both [`extract_vocabulary`] (which projects each
/// group into a [`ParagraphRole`]) and [`paragraph_role_ids`] (which maps every
/// paragraph `NodeId` to its group's role id) consume this one pass, so the role
/// vocabulary an `insert` op validates against is *exactly* the per-block role
/// token the read view surfaces — no second derivation that could drift.
struct GroupedParagraphs {
    para_infos: Vec<ParagraphInfo>,
    table_infos: Vec<(NodeId, TableSignature)>,
    /// One entry per signature group, in document order; `indices` index into
    /// `para_infos`.
    groups: Vec<(FormattingSignature, Vec<usize>)>,
    /// `final_ids[i]` is the role id of `groups[i]`.
    final_ids: Vec<String>,
    default_font_size: Option<u32>,
}

fn group_paragraphs(doc: &CanonDoc) -> GroupedParagraphs {
    // Pass 0: Compute document-level default font size.
    let default_font_size = compute_default_font_size(doc);

    // Step 1: Walk all blocks (recursing into tables), using default font size
    // for normalization.
    let mut para_infos: Vec<ParagraphInfo> = Vec::new();
    let mut table_infos: Vec<(NodeId, TableSignature)> = Vec::new();
    walk_tracked_blocks(
        &doc.blocks,
        false,
        default_font_size,
        &mut para_infos,
        &mut table_infos,
    );

    // Step 2: Group paragraphs by signature.
    let mut sig_groups: HashMap<FormattingSignature, Vec<usize>> = HashMap::new();
    for (idx, info) in para_infos.iter().enumerate() {
        sig_groups
            .entry(info.signature.clone())
            .or_default()
            .push(idx);
    }

    // Sort groups by first occurrence (document order) for deterministic output.
    let mut groups: Vec<(FormattingSignature, Vec<usize>)> = sig_groups.into_iter().collect();
    groups.sort_by_key(|(_, indices)| indices[0]);

    // Compute base names and resolve collisions into final ids.
    let base_names: Vec<String> = groups
        .iter()
        .map(|(sig, _)| name_paragraph_group(sig))
        .collect();
    let final_ids = assign_descriptive_ids(&base_names, &groups);

    GroupedParagraphs {
        para_infos,
        table_infos,
        groups,
        final_ids,
        default_font_size,
    }
}

/// Map every paragraph's `NodeId` to the role id an `insert`/`replace` op
/// accepts for a paragraph of that formatting. This is the SAME grouping
/// [`extract_vocabulary`] uses, so a role token surfaced per block (read view)
/// is guaranteed to resolve in [`crate::edit::resolve_paragraph_spec`]'s role
/// lookup. Paragraphs inside tables are included (the walk recurses into cells).
pub fn paragraph_role_ids(doc: &CanonDoc) -> HashMap<NodeId, String> {
    let grouped = group_paragraphs(doc);
    let mut out = HashMap::new();
    for (i, (_, indices)) in grouped.groups.iter().enumerate() {
        let role_id = &grouped.final_ids[i];
        for &idx in indices {
            out.insert(grouped.para_infos[idx].node_id.clone(), role_id.clone());
        }
    }
    out
}

/// The role id an `insert`/`replace` op accepts when the agent wants the
/// document's default body paragraph and has no specific block to copy. This is
/// the id of the most frequent non-heading, non-numbered role — the same token
/// `paragraph_role_ids` assigns to a plain `Normal` paragraph. Returns `None`
/// only for a document with no projectable body role (e.g. a doc that is all
/// headings/tables), so the caller fails loud rather than inventing a role.
pub fn default_body_role_id(doc: &CanonDoc) -> Option<String> {
    let vocab = extract_vocabulary(doc);
    // Prefer the most frequent plain body role (no heading, no numbering).
    vocab
        .paragraph_roles
        .iter()
        .filter(|r| !r.has_numbering)
        .max_by_key(|r| r.count)
        .or_else(|| vocab.paragraph_roles.iter().max_by_key(|r| r.count))
        .map(|r| r.id.clone())
}

/// Extract a `DocumentVocabulary` from a canonical document.
pub fn extract_vocabulary(doc: &CanonDoc) -> DocumentVocabulary {
    let GroupedParagraphs {
        para_infos,
        table_infos,
        groups,
        final_ids,
        default_font_size,
    } = group_paragraphs(doc);

    // Step 3+4+5+6: Name each group, select exemplar, generate description,
    // determine numbering source.
    let mut paragraph_roles: Vec<ParagraphRole> = Vec::new();

    let total_paras = para_infos.len();

    for (i, (sig, indices)) in groups.iter().enumerate() {
        let description = describe_role(sig, default_font_size);
        let exemplar_idx = indices[0];
        let exemplar = para_infos[exemplar_idx].node_id.clone();
        let exemplar_text = pick_exemplar_text(indices, &para_infos);
        let has_numbering = sig.has_numbering;
        let numbering_source = determine_numbering_source(indices, &para_infos);
        let count = indices.len();
        let frequency = compute_frequency(count, total_paras);

        paragraph_roles.push(ParagraphRole {
            id: final_ids[i].clone(),
            description,
            exemplar,
            exemplar_text,
            frequency,
            has_numbering,
            numbering_source,
            count,
        });
    }

    // Step 9: Empty-document fallback.
    if paragraph_roles.is_empty() {
        paragraph_roles.push(ParagraphRole {
            id: "body_text".to_string(),
            description: "body paragraph".to_string(),
            exemplar: NodeId::from("synthetic_body_text"),
            exemplar_text: String::new(),
            frequency: RoleFrequency::Primary,
            has_numbering: false,
            numbering_source: None,
            count: 0,
        });
    }

    // Step 7: Extract inline roles.
    let inline_roles = extract_inline_roles(doc);

    // Step 8: Extract table roles.
    let table_roles = extract_table_roles(&table_infos);

    DocumentVocabulary {
        paragraph_roles,
        inline_roles,
        table_roles,
        inline_marks: INLINE_MARKS.to_vec(),
    }
}

// ---------------------------------------------------------------------------
// Pass 0: Document-level default font size
// ---------------------------------------------------------------------------

/// Compute the most common font size across all text nodes (by char count).
/// Returns None if no explicit font sizes found.
fn compute_default_font_size(doc: &CanonDoc) -> Option<u32> {
    let mut size_counts: HashMap<u32, usize> = HashMap::new();
    visit_paragraphs_tracked(&doc.blocks, &mut |p| {
        for inline in p.all_inlines_owned() {
            if let InlineNode::Text(t) = &inline
                && let Some(sz) = t.style_props.font_size
            {
                let len = t.text.chars().count();
                if len > 0 {
                    *size_counts.entry(sz).or_default() += len;
                }
            }
        }
    });
    size_counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(sz, _)| sz)
}

/// Normalize a paragraph's dominant font size against the document default.
/// Returns None if the size is absent or within tolerance of the default
/// (meaning "default/body text size"). Returns Some(size) only when the
/// paragraph uses a meaningfully different size (headings, footnotes, etc.).
fn normalize_font_size(raw: Option<u32>, default: Option<u32>) -> Option<u32> {
    let size = raw?;
    let Some(def) = default else {
        return Some(size);
    };
    if size.abs_diff(def) <= FONT_SIZE_MERGE_TOLERANCE {
        None // Close enough to default — treat as default.
    } else {
        Some(size)
    }
}

// ---------------------------------------------------------------------------
// Step 1: Walk blocks
// ---------------------------------------------------------------------------

fn walk_tracked_blocks(
    blocks: &[TrackedBlock],
    in_table: bool,
    default_font_size: Option<u32>,
    para_infos: &mut Vec<ParagraphInfo>,
    table_infos: &mut Vec<(NodeId, TableSignature)>,
) {
    for tracked in blocks {
        walk_block(
            &tracked.block,
            in_table,
            default_font_size,
            para_infos,
            table_infos,
        );
    }
}

fn walk_bare_blocks(
    blocks: &[BlockNode],
    in_table: bool,
    default_font_size: Option<u32>,
    para_infos: &mut Vec<ParagraphInfo>,
    table_infos: &mut Vec<(NodeId, TableSignature)>,
) {
    for block in blocks {
        walk_block(block, in_table, default_font_size, para_infos, table_infos);
    }
}

fn walk_block(
    block: &BlockNode,
    in_table: bool,
    default_font_size: Option<u32>,
    para_infos: &mut Vec<ParagraphInfo>,
    table_infos: &mut Vec<(NodeId, TableSignature)>,
) {
    match block {
        BlockNode::Paragraph(p) => {
            para_infos.push(build_paragraph_info(p, in_table, default_font_size));
        }
        BlockNode::Table(t) => {
            table_infos.push((t.id.clone(), build_table_signature(t)));
            // Recurse into table cells — paragraphs inside are in_table=true.
            for row in &t.rows {
                for cell in &row.cells {
                    walk_bare_blocks(
                        &cell.blocks,
                        true,
                        default_font_size,
                        para_infos,
                        table_infos,
                    );
                }
            }
        }
        BlockNode::OpaqueBlock(_) => {}
    }
}

// ---------------------------------------------------------------------------
// Signature extraction
// ---------------------------------------------------------------------------

fn build_paragraph_info(
    p: &ParagraphNode,
    in_table: bool,
    default_font_size: Option<u32>,
) -> ParagraphInfo {
    let inlines = p.all_inlines_owned();

    // Compute text-weighted dominant properties.
    let mut total_chars: usize = 0;
    let mut bold_chars: usize = 0;
    let mut caps_chars: usize = 0;
    let mut font_size_counts: HashMap<u32, usize> = HashMap::new();

    for inline in &inlines {
        if let InlineNode::Text(t) = inline {
            let len = t.text.chars().count();
            if len == 0 {
                continue;
            }
            total_chars += len;

            if t.marks.contains(&Mark::Bold) {
                bold_chars += len;
            }

            // All-caps: either caps MarkValue::On or actual uppercase content.
            let is_caps_mark = t.style_props.caps == MarkValue::On;
            let is_actual_upper = !is_caps_mark
                && t.text
                    .chars()
                    .all(|c| !c.is_alphabetic() || c.is_uppercase());
            // Only count actual-upper if there's at least one alpha char.
            let has_alpha = t.text.chars().any(|c| c.is_alphabetic());
            if is_caps_mark || (is_actual_upper && has_alpha) {
                caps_chars += len;
            }

            if let Some(sz) = t.style_props.font_size {
                *font_size_counts.entry(sz).or_default() += len;
            }
        }
    }

    let dominant_bold = total_chars > 0 && bold_chars * 2 > total_chars;
    let dominant_all_caps = total_chars > 0 && caps_chars * 2 > total_chars;
    let raw_font_size = font_size_counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(sz, _)| sz);
    let dominant_font_size = normalize_font_size(raw_font_size, default_font_size);

    // Numbering.
    let has_auto_numbering = p.numbering.is_some();
    let has_literal_prefix = p.literal_prefix.is_some();
    let has_numbering = has_auto_numbering || has_literal_prefix;

    let numbering_ilvl = if has_auto_numbering {
        p.numbering.as_ref().map(|n| n.ilvl)
    } else {
        // Literal prefixes don't carry ilvl; treat as ilvl 0.
        if has_literal_prefix { Some(0) } else { None }
    };

    let numbering_kind = classify_numbering(p);

    let numbering_source = if has_auto_numbering {
        Some(NumberingSource::Auto)
    } else if has_literal_prefix {
        Some(NumberingSource::LiteralPrefix)
    } else {
        None
    };

    // Alignment: None normalizes to Left.
    let align = alignment_discriminant(p.align.as_ref());

    // Indentation — only include for unstyled paragraphs (None or "Normal").
    // For styled paragraphs, indent is a direct-formatting override that
    // shouldn't create a separate role. Also zero out for table-internal
    // paragraphs (cell indent is layout noise).
    let is_unstyled = p
        .style_id
        .as_ref()
        .map(|s| s.as_ref() == "Normal")
        .unwrap_or(true);
    let indent_left_bucket = if in_table || !is_unstyled {
        0
    } else {
        p.indent
            .as_ref()
            .and_then(|i| i.left)
            .map(|v| quantize(v, INDENT_BUCKET_TW))
            .unwrap_or(0)
    };

    // Heading level.
    let heading_level = p.heading_level.as_ref().map(heading_level_to_u8);

    // Borders.
    let has_borders = p.borders.is_some();

    let signature = FormattingSignature {
        style_id: p.style_id.as_ref().map(|s| s.to_string()),
        heading_level,
        has_numbering,
        numbering_ilvl,
        numbering_kind,
        align,
        indent_left_bucket,
        dominant_bold,
        dominant_all_caps,
        dominant_font_size,
        has_borders,
    };

    // Exemplar text snippet: numbering prefix + body text, truncated.
    let text_snippet = extract_text_snippet(p);

    ParagraphInfo {
        node_id: p.id.clone(),
        signature,
        numbering_source,
        text_snippet,
    }
}

fn classify_numbering(p: &ParagraphNode) -> NumberingKind {
    if let Some(ref num) = p.numbering {
        return classify_numbering_text(&num.synthesized_text);
    }
    if let Some(ref prefix) = p.literal_prefix {
        return classify_numbering_text(prefix);
    }
    NumberingKind::None
}

const MAX_SNIPPET_LEN: usize = 80;

/// Extract a short text snippet from a paragraph for LLM context.
/// Includes numbering/literal prefix + body text, truncated to MAX_SNIPPET_LEN chars.
fn extract_text_snippet(p: &ParagraphNode) -> String {
    let mut out = String::new();

    // Numbering prefix.
    if let Some(ref num) = p.numbering {
        out.push_str(&num.synthesized_text);
    }
    if let Some(ref prefix) = p.literal_prefix {
        out.push_str(prefix);
    }

    // Body text from inlines.
    for inline in p.all_inlines_owned() {
        match &inline {
            InlineNode::Text(t) => out.push_str(&t.text),
            InlineNode::HardBreak(_) => out.push(' '),
            _ => {}
        }
        if out.len() > MAX_SNIPPET_LEN + 20 {
            break; // Enough to truncate from.
        }
    }

    let trimmed = out.trim();
    if trimmed.len() <= MAX_SNIPPET_LEN {
        trimmed.to_string()
    } else {
        // Truncate at char boundary.
        let mut end = MAX_SNIPPET_LEN;
        while end > 0 && !trimmed.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &trimmed[..end])
    }
}

/// Pick the best exemplar text from a group of paragraphs.
/// Prefers the first non-empty snippet (empty paragraphs are spacers).
fn pick_exemplar_text(indices: &[usize], infos: &[ParagraphInfo]) -> String {
    // First non-empty snippet.
    for &idx in indices {
        let s = &infos[idx].text_snippet;
        if !s.is_empty() {
            return s.clone();
        }
    }
    // All empty — return empty.
    String::new()
}

fn compute_frequency(count: usize, total: usize) -> RoleFrequency {
    if total == 0 {
        return RoleFrequency::Rare;
    }
    let pct = count * 100 / total;
    if pct >= 20 {
        RoleFrequency::Primary
    } else if pct >= 5 {
        RoleFrequency::Common
    } else if count >= 3 {
        RoleFrequency::Minor
    } else {
        RoleFrequency::Rare
    }
}

fn classify_numbering_text(text: &str) -> NumberingKind {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return NumberingKind::None;
    }
    // Bullet characters.
    const BULLET_CHARS: &[char] = &[
        '•', '◦', '■', '□', '▪', '▫', '●', '○', '–', '―', '—', '‣', '⁃', '∙',
    ];
    if trimmed
        .chars()
        .all(|c| BULLET_CHARS.contains(&c) || c.is_whitespace())
    {
        return NumberingKind::Bullet;
    }
    // Otherwise treat as numbered (digits, letters, roman numerals with punctuation).
    NumberingKind::Numbered
}

fn alignment_discriminant(align: Option<&Alignment>) -> u8 {
    match align {
        None | Some(Alignment::Left) => 0,
        Some(Alignment::Center) => 1,
        Some(Alignment::Right) => 2,
        Some(Alignment::Justify) => 3,
        Some(Alignment::Distribute) => 4,
        Some(Alignment::HighKashida) => 5,
        Some(Alignment::LowKashida) => 6,
        Some(Alignment::MediumKashida) => 7,
        Some(Alignment::NumTab) => 8,
        Some(Alignment::ThaiDistribute) => 9,
    }
}

fn heading_level_to_u8(hl: &HeadingLevel) -> u8 {
    match hl {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
        HeadingLevel::H7 => 7,
        HeadingLevel::H8 => 8,
        HeadingLevel::H9 => 9,
    }
}

/// Round to nearest bucket boundary. Ties (halfway) round away from zero.
fn quantize(value: i32, bucket: i32) -> i32 {
    if value >= 0 {
        ((value + bucket / 2) / bucket) * bucket
    } else {
        -((-value + bucket / 2) / bucket * bucket)
    }
}

// ---------------------------------------------------------------------------
// Step 3: Name each group (heuristic cascade)
// ---------------------------------------------------------------------------

fn name_paragraph_group(sig: &FormattingSignature) -> String {
    // 1. Named style (not "Normal").
    if let Some(ref style) = sig.style_id
        && style != "Normal"
    {
        let base = style_id_to_snake(style);
        return maybe_append_boxed(base, sig.has_borders);
    }

    // 2. Heading level.
    if let Some(level) = sig.heading_level {
        let name = match level {
            1 if (sig.align == 1) && (sig.dominant_bold || sig.dominant_all_caps) => {
                "title".to_string()
            }
            1 if sig.has_numbering => "section_heading".to_string(),
            1 => "heading_1".to_string(),
            n => format!("heading_{n}"),
        };
        return maybe_append_boxed(name, sig.has_borders);
    }

    // 3. Pattern heuristics.
    let is_centered = sig.align == 1;
    let is_bold_or_caps = sig.dominant_bold || sig.dominant_all_caps;
    let is_large = sig
        .dominant_font_size
        .map(|sz| sz >= TITLE_FONT_SIZE_THRESHOLD)
        .unwrap_or(false);

    if is_centered && is_bold_or_caps && is_large {
        return maybe_append_boxed("title".to_string(), sig.has_borders);
    }
    if is_centered && is_bold_or_caps {
        return maybe_append_boxed("centered_heading".to_string(), sig.has_borders);
    }

    // Numbering-based names.
    match sig.numbering_kind {
        NumberingKind::Numbered => {
            let name = match sig.numbering_ilvl {
                Some(0) if sig.dominant_bold => "section_heading",
                Some(0) => "numbered_item",
                Some(1) => "sub_item",
                _ if sig.numbering_ilvl.unwrap_or(0) >= 2 => "sub_sub_item",
                _ => "numbered_item",
            };
            return maybe_append_boxed(name.to_string(), sig.has_borders);
        }
        NumberingKind::Bullet => {
            let name = match sig.numbering_ilvl {
                Some(0) | None => "bullet_item",
                _ => "bullet_sub_item",
            };
            return maybe_append_boxed(name.to_string(), sig.has_borders);
        }
        NumberingKind::None => {}
    }

    // Body text variants.
    let is_left_or_justify = sig.align == 0 || sig.align == 3;
    if is_left_or_justify && !sig.dominant_bold && sig.indent_left_bucket > 0 {
        return maybe_append_boxed("indented_body".to_string(), sig.has_borders);
    }
    if is_left_or_justify && !sig.dominant_bold {
        return maybe_append_boxed("body_text".to_string(), sig.has_borders);
    }

    // 4. Fallback — will get deduplication suffix.
    maybe_append_boxed("paragraph_group".to_string(), sig.has_borders)
}

fn maybe_append_boxed(name: String, has_borders: bool) -> String {
    if has_borders {
        format!("{name}_boxed")
    } else {
        name
    }
}

fn style_id_to_snake(id: &str) -> String {
    let mut out = String::with_capacity(id.len() + 4);
    let mut prev_was_lower = false;
    for ch in id.chars() {
        if ch.is_uppercase() && prev_was_lower {
            out.push('_');
        }
        // Replace hyphens and spaces with underscores.
        if ch == '-' || ch == ' ' {
            out.push('_');
            prev_was_lower = false;
        } else {
            out.push(ch.to_ascii_lowercase());
            prev_was_lower = ch.is_lowercase();
        }
    }
    out
}

/// Assign final IDs to paragraph groups. Groups with unique base names keep them.
/// Groups that collide get a descriptive suffix based on the first distinguishing
/// signature field, falling back to numeric _2, _3, etc.
fn assign_descriptive_ids(
    base_names: &[String],
    groups: &[(FormattingSignature, Vec<usize>)],
) -> Vec<String> {
    // Find which base names collide.
    let mut name_indices: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, name) in base_names.iter().enumerate() {
        name_indices.entry(name.as_str()).or_default().push(i);
    }

    let mut result = vec![String::new(); base_names.len()];
    let mut used_ids: HashMap<String, usize> = HashMap::new();

    for (name, indices) in &name_indices {
        if indices.len() == 1 {
            // Unique — use as-is.
            result[indices[0]] = name.to_string();
            used_ids.insert(name.to_string(), 1);
        } else {
            // Collision — try descriptive suffixes.
            let sigs: Vec<&FormattingSignature> = indices.iter().map(|&i| &groups[i].0).collect();
            let suffixes = compute_descriptive_suffixes(&sigs);

            for (j, &idx) in indices.iter().enumerate() {
                let candidate = if suffixes[j].is_empty() {
                    name.to_string()
                } else {
                    format!("{name}_{}", suffixes[j])
                };
                let id = dedup_id(&candidate, &mut used_ids);
                result[idx] = id;
            }
        }
    }

    result
}

/// Given a set of signatures that all mapped to the same base name, compute
/// a descriptive suffix for each that distinguishes it from the others.
fn compute_descriptive_suffixes(sigs: &[&FormattingSignature]) -> Vec<String> {
    let n = sigs.len();
    let mut suffixes = vec![String::new(); n];

    // Try each distinguishing feature in priority order.
    // Pick the first feature that partitions at least some of the collisions.

    // 1. Font size (e.g., "12pt", "14pt").
    if try_suffix_by(sigs, &mut suffixes, |sig| {
        sig.dominant_font_size.map(|hp| {
            let pts = hp as f64 / 2.0;
            if pts == pts.floor() {
                format!("{}pt", pts as u32)
            } else {
                format!("{pts:.1}pt")
            }
        })
    }) {
        return suffixes;
    }

    // 2. Alignment.
    if try_suffix_by(sigs, &mut suffixes, |sig| match sig.align {
        0 => None, // left is default, don't suffix
        1 => Some("centered".into()),
        2 => Some("right".into()),
        3 => Some("justified".into()),
        _ => Some("other_align".into()),
    }) {
        return suffixes;
    }

    // 3. Indent.
    if try_suffix_by(sigs, &mut suffixes, |sig| {
        if sig.indent_left_bucket != 0 {
            Some(format!("indent_{}tw", sig.indent_left_bucket))
        } else {
            None
        }
    }) {
        return suffixes;
    }

    // 4. Bold.
    if try_suffix_by(sigs, &mut suffixes, |sig| {
        if sig.dominant_bold {
            Some("bold".into())
        } else {
            None
        }
    }) {
        return suffixes;
    }

    // 5. All caps.
    if try_suffix_by(sigs, &mut suffixes, |sig| {
        if sig.dominant_all_caps {
            Some("caps".into())
        } else {
            None
        }
    }) {
        return suffixes;
    }

    // Fallback: no descriptive suffix found — dedup_id will add _2, _3, etc.
    suffixes
}

/// Try a suffix generator on each signature. If the resulting suffixes
/// produce at least one disambiguation (i.e. not all the same), apply them
/// and return true. Otherwise leave suffixes unchanged and return false.
fn try_suffix_by(
    sigs: &[&FormattingSignature],
    suffixes: &mut [String],
    make_suffix: impl Fn(&FormattingSignature) -> Option<String>,
) -> bool {
    let candidates: Vec<Option<String>> = sigs.iter().map(|s| make_suffix(s)).collect();
    // Check if there's at least 2 distinct values.
    let distinct: std::collections::HashSet<_> = candidates.iter().collect();
    if distinct.len() < 2 {
        return false;
    }
    for (i, c) in candidates.into_iter().enumerate() {
        if let Some(s) = c {
            suffixes[i] = s;
        }
    }
    true
}

/// Deduplicate a role id. First use gets the base name; subsequent uses get _2, _3, etc.
fn dedup_id(base: &str, used: &mut HashMap<String, usize>) -> String {
    let counter = used.entry(base.to_string()).or_insert(0);
    *counter += 1;
    if *counter == 1 {
        base.to_string()
    } else {
        format!("{base}_{counter}")
    }
}

// ---------------------------------------------------------------------------
// Step 5: Generate description
// ---------------------------------------------------------------------------

fn describe_role(sig: &FormattingSignature, default_font_size: Option<u32>) -> String {
    let mut tokens: Vec<String> = Vec::new();

    // Alignment.
    match sig.align {
        0 => tokens.push("left-aligned".into()),
        1 => tokens.push("centered".into()),
        2 => tokens.push("right-aligned".into()),
        3 => tokens.push("justified".into()),
        _ => tokens.push("aligned".into()),
    }

    // Numbering.
    match sig.numbering_kind {
        NumberingKind::Bullet => tokens.push("bulleted".into()),
        NumberingKind::Numbered => tokens.push("numbered".into()),
        NumberingKind::None => {}
    }

    // Bold.
    if sig.dominant_bold {
        tokens.push("bold".into());
    }

    // All caps.
    if sig.dominant_all_caps {
        tokens.push("all caps".into());
    }

    // Font size — show when non-default.
    if let Some(half_pts) = sig.dominant_font_size {
        let pts = half_pts as f64 / 2.0;
        if pts == pts.floor() {
            tokens.push(format!("{}pt", pts as u32));
        } else {
            tokens.push(format!("{pts:.1}pt"));
        }
    } else if let Some(def) = default_font_size {
        // Show the default size for context but mark it.
        let pts = def as f64 / 2.0;
        if pts == pts.floor() {
            tokens.push(format!("{}pt", pts as u32));
        } else {
            tokens.push(format!("{pts:.1}pt"));
        }
    }

    // Style.
    if let Some(ref style) = sig.style_id
        && style != "Normal"
    {
        tokens.push(format!("style:{style}"));
    }

    // Heading level.
    if let Some(level) = sig.heading_level {
        tokens.push(format!("heading level {level}"));
    }

    // Indent — suppress when numbering is present (hanging indent is noise).
    if sig.indent_left_bucket != 0 && !sig.has_numbering {
        tokens.push(format!("indent {}tw", sig.indent_left_bucket));
    }

    // Borders.
    if sig.has_borders {
        tokens.push("bordered".into());
    }

    if tokens.is_empty() {
        "body paragraph".into()
    } else {
        tokens.join(", ")
    }
}

// ---------------------------------------------------------------------------
// Step 6: Determine numbering_source per role
// ---------------------------------------------------------------------------

fn determine_numbering_source(
    indices: &[usize],
    infos: &[ParagraphInfo],
) -> Option<NumberingSource> {
    let mut has_auto = false;
    let mut has_literal = false;
    for &idx in indices {
        match &infos[idx].numbering_source {
            Some(NumberingSource::Auto) => has_auto = true,
            Some(NumberingSource::LiteralPrefix) => has_literal = true,
            None => {}
        }
    }

    if !has_auto && !has_literal {
        None
    } else if has_auto {
        // Prefer auto (even if mixed).
        Some(NumberingSource::Auto)
    } else {
        Some(NumberingSource::LiteralPrefix)
    }
}

// ---------------------------------------------------------------------------
// Step 7: Extract inline roles
// ---------------------------------------------------------------------------

fn extract_inline_roles(doc: &CanonDoc) -> Vec<InlineRole> {
    let mut roles: Vec<InlineRole> = Vec::new();
    let mut used_ids: HashMap<String, usize> = HashMap::new();

    // (a) Character styles.
    let mut char_style_first: HashMap<String, (NodeId, usize)> = HashMap::new();

    visit_paragraphs_tracked(&doc.blocks, &mut |p| {
        for (run_idx, inline) in p.all_inlines_owned().iter().enumerate() {
            if let InlineNode::Text(t) = inline
                && let Some(ref cs_id) = t.style_props.char_style_id
            {
                let key = cs_id.to_string();
                char_style_first
                    .entry(key)
                    .or_insert_with(|| (p.id.clone(), run_idx));
            }
        }
    });

    // Sort by style id for deterministic order.
    let mut char_styles: Vec<(String, (NodeId, usize))> = char_style_first.into_iter().collect();
    char_styles.sort_by(|a, b| a.0.cmp(&b.0));

    for (style_id, (para_id, run_idx)) in char_styles {
        let id = dedup_id(&style_id_to_snake(&style_id), &mut used_ids);
        roles.push(InlineRole {
            id,
            description: format!("character style: {style_id}"),
            exemplar_para: para_id,
            exemplar_run_index: run_idx,
        });
    }

    // (b) Defined-term heuristic: bold runs inside quotation marks with Capitalized Words.
    let mut defined_term_candidates: Vec<(NodeId, usize)> = Vec::new();
    visit_paragraphs_tracked(&doc.blocks, &mut |p| {
        let inlines = p.all_inlines_owned();
        for (run_idx, inline) in inlines.iter().enumerate() {
            if let InlineNode::Text(t) = inline {
                if !t.marks.contains(&Mark::Bold) {
                    continue;
                }
                if !is_capitalized_words(&t.text) {
                    continue;
                }
                let prev_char = preceding_text_char(&inlines, run_idx);
                let next_char = following_text_char(&inlines, run_idx);
                let in_quotes = matches!(prev_char, Some('\u{201C}' | '"'))
                    && matches!(next_char, Some('\u{201D}' | '"'));
                if in_quotes {
                    defined_term_candidates.push((p.id.clone(), run_idx));
                }
            }
        }
    });

    if defined_term_candidates.len() >= DEFINED_TERM_MIN_OCCURRENCES {
        let (para_id, run_idx) = defined_term_candidates[0].clone();
        roles.push(InlineRole {
            id: dedup_id("defined_term", &mut used_ids),
            description: "bold text in quotation marks, capitalized words".to_string(),
            exemplar_para: para_id,
            exemplar_run_index: run_idx,
        });
    }

    roles
}

fn visit_paragraphs_tracked(blocks: &[TrackedBlock], visitor: &mut dyn FnMut(&ParagraphNode)) {
    for tracked in blocks {
        visit_paragraphs_block(&tracked.block, visitor);
    }
}

fn visit_paragraphs_block(block: &BlockNode, visitor: &mut dyn FnMut(&ParagraphNode)) {
    match block {
        BlockNode::Paragraph(p) => visitor(p),
        BlockNode::Table(t) => {
            for row in &t.rows {
                for cell in &row.cells {
                    for b in &cell.blocks {
                        visit_paragraphs_block(b, visitor);
                    }
                }
            }
        }
        BlockNode::OpaqueBlock(_) => {}
    }
}

/// Check if text consists of Capitalized Words (each word starts with uppercase).
fn is_capitalized_words(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }
    for word in trimmed.split_whitespace() {
        let first = word.chars().next().unwrap();
        if first.is_alphabetic() && !first.is_uppercase() {
            return false;
        }
    }
    true
}

fn preceding_text_char(inlines: &[InlineNode], run_idx: usize) -> Option<char> {
    for i in (0..run_idx).rev() {
        if let InlineNode::Text(t) = &inlines[i] {
            return t.text.chars().last();
        }
    }
    None
}

fn following_text_char(inlines: &[InlineNode], run_idx: usize) -> Option<char> {
    for inline in &inlines[(run_idx + 1)..] {
        if let InlineNode::Text(t) = inline {
            return t.text.chars().next();
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Step 8: Extract table roles
// ---------------------------------------------------------------------------

fn build_table_signature(t: &TableNode) -> TableSignature {
    let col_count = t.formatting.grid_cols.len();
    let has_borders = t.formatting.borders.is_some();

    let has_header_row = if let Some(first_row) = t.rows.first() {
        first_row.is_header || first_row_looks_like_header(first_row)
    } else {
        false
    };

    TableSignature {
        style_id: t.formatting.style_id.as_ref().map(|s| s.to_string()),
        col_count,
        has_borders,
        has_header_row,
    }
}

fn first_row_looks_like_header(row: &crate::domain::TableRowNode) -> bool {
    let has_shading = row.cells.iter().any(|c| c.formatting.shading.is_some());
    if has_shading {
        return true;
    }

    let mut total_chars: usize = 0;
    let mut bold_chars: usize = 0;
    for cell in &row.cells {
        for block in &cell.blocks {
            if let BlockNode::Paragraph(p) = block {
                for inline in p.all_inlines_owned() {
                    if let InlineNode::Text(t) = &inline {
                        let len = t.text.chars().count();
                        total_chars += len;
                        if t.marks.contains(&Mark::Bold) {
                            bold_chars += len;
                        }
                    }
                }
            }
        }
    }

    total_chars > 0 && bold_chars == total_chars
}

fn extract_table_roles(table_infos: &[(NodeId, TableSignature)]) -> Vec<TableRole> {
    let mut sig_groups: HashMap<&TableSignature, Vec<usize>> = HashMap::new();
    for (idx, (_, sig)) in table_infos.iter().enumerate() {
        sig_groups.entry(sig).or_default().push(idx);
    }

    let mut groups: Vec<(&TableSignature, Vec<usize>)> = sig_groups.into_iter().collect();
    groups.sort_by_key(|(_, indices)| indices[0]);

    let mut used_ids: HashMap<String, usize> = HashMap::new();
    let mut roles: Vec<TableRole> = Vec::new();

    for (sig, indices) in &groups {
        let base_name = name_table_group(sig);
        let id = dedup_id(&base_name, &mut used_ids);
        let description = describe_table(sig);
        let exemplar = table_infos[indices[0]].0.clone();

        roles.push(TableRole {
            id,
            description,
            exemplar,
            count: indices.len(),
        });
    }

    roles
}

fn name_table_group(sig: &TableSignature) -> String {
    if let Some(ref style) = sig.style_id {
        return style_id_to_snake(style);
    }
    if sig.has_header_row {
        "headed_table".to_string()
    } else {
        "table".to_string()
    }
}

fn describe_table(sig: &TableSignature) -> String {
    let mut tokens: Vec<String> = Vec::new();

    tokens.push(format!("{} columns", sig.col_count));

    if let Some(ref style) = sig.style_id {
        tokens.push(format!("style:{style}"));
    }

    if sig.has_header_row {
        tokens.push("with header row".into());
    }

    if sig.has_borders {
        tokens.push("bordered".into());
    }

    tokens.join(", ")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantize_positive_values() {
        assert_eq!(quantize(0, INDENT_BUCKET_TW), 0);
        assert_eq!(quantize(72, INDENT_BUCKET_TW), 72);
        assert_eq!(quantize(100, INDENT_BUCKET_TW), 72);
        assert_eq!(quantize(108, INDENT_BUCKET_TW), 144);
        assert_eq!(quantize(144, INDENT_BUCKET_TW), 144);
        assert_eq!(quantize(36, INDENT_BUCKET_TW), 72);
        assert_eq!(quantize(35, INDENT_BUCKET_TW), 0);
    }

    #[test]
    fn quantize_negative_values() {
        assert_eq!(quantize(-72, INDENT_BUCKET_TW), -72);
        assert_eq!(quantize(-100, INDENT_BUCKET_TW), -72);
        assert_eq!(quantize(-108, INDENT_BUCKET_TW), -144);
        assert_eq!(quantize(-36, INDENT_BUCKET_TW), -72);
        assert_eq!(quantize(-35, INDENT_BUCKET_TW), 0);
    }

    #[test]
    fn quantize_spacing() {
        assert_eq!(quantize(120, SPACING_BUCKET_TW), 120);
        assert_eq!(quantize(130, SPACING_BUCKET_TW), 120);
        assert_eq!(quantize(150, SPACING_BUCKET_TW), 180);
        assert_eq!(quantize(0, SPACING_BUCKET_TW), 0);
    }

    #[test]
    fn numbering_kind_bullets() {
        assert_eq!(classify_numbering_text("•"), NumberingKind::Bullet);
        assert_eq!(classify_numbering_text("◦"), NumberingKind::Bullet);
        assert_eq!(classify_numbering_text("■"), NumberingKind::Bullet);
        assert_eq!(classify_numbering_text("–"), NumberingKind::Bullet);
        assert_eq!(classify_numbering_text("―"), NumberingKind::Bullet);
    }

    #[test]
    fn numbering_kind_numbered() {
        assert_eq!(classify_numbering_text("1."), NumberingKind::Numbered);
        assert_eq!(classify_numbering_text("(a)"), NumberingKind::Numbered);
        assert_eq!(classify_numbering_text("iv."), NumberingKind::Numbered);
        assert_eq!(classify_numbering_text("A)"), NumberingKind::Numbered);
    }

    #[test]
    fn numbering_kind_empty() {
        assert_eq!(classify_numbering_text(""), NumberingKind::None);
        assert_eq!(classify_numbering_text("  "), NumberingKind::None);
    }

    #[test]
    fn alignment_none_equals_left() {
        assert_eq!(
            alignment_discriminant(None),
            alignment_discriminant(Some(&Alignment::Left))
        );
    }

    #[test]
    fn alignment_discriminants_are_distinct() {
        let all = [
            Alignment::Left,
            Alignment::Center,
            Alignment::Right,
            Alignment::Justify,
        ];
        let discs: Vec<u8> = all
            .iter()
            .map(|a| alignment_discriminant(Some(a)))
            .collect();
        for i in 0..discs.len() {
            for j in (i + 1)..discs.len() {
                assert_ne!(discs[i], discs[j], "{:?} and {:?} collide", all[i], all[j]);
            }
        }
    }

    #[test]
    fn capitalized_words_detection() {
        assert!(is_capitalized_words("Defined Term"));
        assert!(is_capitalized_words("ALLCAPS TERM"));
        assert!(is_capitalized_words("Single"));
        assert!(!is_capitalized_words("lower case"));
        assert!(!is_capitalized_words("mixed Case words"));
        assert!(!is_capitalized_words(""));
    }

    #[test]
    fn style_id_to_snake_cases() {
        assert_eq!(style_id_to_snake("Heading1"), "heading1");
        assert_eq!(style_id_to_snake("ListParagraph"), "list_paragraph");
        assert_eq!(style_id_to_snake("Title"), "title");
        assert_eq!(style_id_to_snake("TOC-Heading"), "toc_heading");
        assert_eq!(style_id_to_snake("BodyText"), "body_text");
    }

    #[test]
    fn describe_role_body() {
        let sig = FormattingSignature {
            style_id: None,
            heading_level: None,
            has_numbering: false,
            numbering_ilvl: None,
            numbering_kind: NumberingKind::None,
            align: 0,
            indent_left_bucket: 0,
            dominant_bold: false,
            dominant_all_caps: false,
            dominant_font_size: None,
            has_borders: false,
        };
        let desc = describe_role(&sig, Some(22));
        assert!(desc.contains("left-aligned"), "got: {desc}");
        assert!(desc.contains("11pt"), "default font size shown: {desc}");
    }

    #[test]
    fn describe_role_centered_bold_caps() {
        let sig = FormattingSignature {
            style_id: None,
            heading_level: None,
            has_numbering: false,
            numbering_ilvl: None,
            numbering_kind: NumberingKind::None,
            align: 1,
            indent_left_bucket: 0,
            dominant_bold: true,
            dominant_all_caps: true,
            dominant_font_size: Some(28),
            has_borders: false,
        };
        let desc = describe_role(&sig, Some(22));
        assert!(desc.contains("centered"), "got: {desc}");
        assert!(desc.contains("bold"), "got: {desc}");
        assert!(desc.contains("all caps"), "got: {desc}");
        assert!(desc.contains("14pt"), "got: {desc}");
    }

    #[test]
    fn describe_role_numbered_suppresses_indent() {
        let sig = FormattingSignature {
            style_id: None,
            heading_level: None,
            has_numbering: true,
            numbering_ilvl: Some(0),
            numbering_kind: NumberingKind::Numbered,
            align: 3,
            indent_left_bucket: -720,
            dominant_bold: false,
            dominant_all_caps: false,
            dominant_font_size: None,
            has_borders: false,
        };
        let desc = describe_role(&sig, Some(22));
        assert!(
            !desc.contains("indent"),
            "indent should be suppressed for numbered: {desc}"
        );
        assert!(desc.contains("numbered"), "got: {desc}");
    }

    #[test]
    fn font_size_normalization() {
        // None → None (no font info).
        assert_eq!(normalize_font_size(None, None), None);
        // None → None (has default but paragraph has no font).
        assert_eq!(normalize_font_size(None, Some(22)), None);
        // Exact match → None (merge with default).
        assert_eq!(normalize_font_size(Some(22), Some(22)), None);
        // Within tolerance → None.
        assert_eq!(normalize_font_size(Some(23), Some(22)), None);
        assert_eq!(normalize_font_size(Some(21), Some(22)), None);
        // Outside tolerance → preserved.
        assert_eq!(normalize_font_size(Some(24), Some(22)), Some(24));
        assert_eq!(normalize_font_size(Some(28), Some(22)), Some(28));
        assert_eq!(normalize_font_size(Some(18), Some(22)), Some(18));
    }

    #[test]
    fn dedup_id_increments() {
        let mut used = HashMap::new();
        assert_eq!(dedup_id("body_text", &mut used), "body_text");
        assert_eq!(dedup_id("body_text", &mut used), "body_text_2");
        assert_eq!(dedup_id("body_text", &mut used), "body_text_3");
        assert_eq!(dedup_id("heading", &mut used), "heading");
    }

    #[test]
    fn descriptive_suffix_by_font_size() {
        let base = FormattingSignature {
            style_id: None,
            heading_level: None,
            has_numbering: false,
            numbering_ilvl: None,
            numbering_kind: NumberingKind::None,
            align: 0,
            indent_left_bucket: 0,
            dominant_bold: false,
            dominant_all_caps: false,
            dominant_font_size: None,
            has_borders: false,
        };
        let sig_12pt = FormattingSignature {
            dominant_font_size: Some(24),
            ..base.clone()
        };
        let sigs = vec![&base, &sig_12pt];
        let suffixes = compute_descriptive_suffixes(&sigs);
        assert_eq!(suffixes[0], ""); // default size → no suffix
        assert_eq!(suffixes[1], "12pt");
    }

    #[test]
    fn descriptive_suffix_by_alignment() {
        let base = FormattingSignature {
            style_id: None,
            heading_level: None,
            has_numbering: false,
            numbering_ilvl: None,
            numbering_kind: NumberingKind::None,
            align: 0,
            indent_left_bucket: 0,
            dominant_bold: false,
            dominant_all_caps: false,
            dominant_font_size: None,
            has_borders: false,
        };
        let justified = FormattingSignature {
            align: 3,
            ..base.clone()
        };
        let sigs = vec![&base, &justified];
        let suffixes = compute_descriptive_suffixes(&sigs);
        assert_eq!(suffixes[0], ""); // left is default → no suffix
        assert_eq!(suffixes[1], "justified");
    }

    #[test]
    fn empty_doc_gets_body_text_fallback() {
        let doc = CanonDoc {
            id: NodeId::from("test"),
            blocks: vec![],
            meta: crate::domain::DocMeta {
                schema_version: crate::domain::SCHEMA_VERSION_V0.to_string(),
                docx_fingerprint: crate::domain::DocFingerprint("test".to_string()),
                internal_ids_version: crate::domain::INTERNAL_IDS_VERSION_V0.to_string(),
            },
            headers: vec![],
            footers: vec![],
            footnotes: vec![],
            endnotes: vec![],
            comments: vec![],
            comments_extended: vec![],
            body_section_properties: None,
            body_section_property_change: None,
            compat_settings: Default::default(),
            even_and_odd_headers: None,
            document_background: None,
            document_protection: None,
        };

        let vocab = extract_vocabulary(&doc);
        assert_eq!(vocab.paragraph_roles.len(), 1);
        assert_eq!(vocab.paragraph_roles[0].id, "body_text");
        assert_eq!(vocab.paragraph_roles[0].count, 0);
        assert!(!vocab.paragraph_roles[0].description.is_empty());
        assert_eq!(vocab.inline_marks.len(), 6);
    }

    #[test]
    fn naming_cascade_style_takes_priority() {
        let sig = FormattingSignature {
            style_id: Some("ListParagraph".to_string()),
            heading_level: None,
            has_numbering: true,
            numbering_ilvl: Some(0),
            numbering_kind: NumberingKind::Numbered,
            align: 0,
            indent_left_bucket: 720,
            dominant_bold: false,
            dominant_all_caps: false,
            dominant_font_size: None,
            has_borders: false,
        };
        assert_eq!(name_paragraph_group(&sig), "list_paragraph");
    }

    #[test]
    fn naming_heading_level_patterns() {
        let sig = FormattingSignature {
            style_id: Some("Normal".to_string()),
            heading_level: Some(1),
            has_numbering: false,
            numbering_ilvl: None,
            numbering_kind: NumberingKind::None,
            align: 1,
            indent_left_bucket: 0,
            dominant_bold: true,
            dominant_all_caps: false,
            dominant_font_size: None,
            has_borders: false,
        };
        assert_eq!(name_paragraph_group(&sig), "title");

        let sig2 = FormattingSignature {
            heading_level: Some(1),
            has_numbering: true,
            numbering_kind: NumberingKind::Numbered,
            numbering_ilvl: Some(0),
            align: 0,
            dominant_bold: false,
            dominant_all_caps: false,
            ..sig.clone()
        };
        assert_eq!(name_paragraph_group(&sig2), "section_heading");
    }

    #[test]
    fn borders_append_boxed() {
        let sig = FormattingSignature {
            style_id: None,
            heading_level: None,
            has_numbering: false,
            numbering_ilvl: None,
            numbering_kind: NumberingKind::None,
            align: 0,
            indent_left_bucket: 0,
            dominant_bold: false,
            dominant_all_caps: false,
            dominant_font_size: None,
            has_borders: true,
        };
        assert_eq!(name_paragraph_group(&sig), "body_text_boxed");
    }

    /// A minimal `Normal`-styled DOCX (no styles part), for the agreement test.
    fn make_normal_docx(paras: &[&str]) -> Vec<u8> {
        let mut body = String::from(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>"#,
        );
        for p in paras {
            body.push_str(&format!(r#"<w:p><w:r><w:t>{p}</w:t></w:r></w:p>"#));
        }
        body.push_str("<w:sectPr/></w:body></w:document>");
        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
        let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
        let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
        use std::io::Write;
        use zip::write::FileOptions;
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts: FileOptions = FileOptions::default();
            zip.start_file("[Content_Types].xml", opts).unwrap();
            zip.write_all(content_types.as_bytes()).unwrap();
            zip.start_file("_rels/.rels", opts).unwrap();
            zip.write_all(rels.as_bytes()).unwrap();
            zip.start_file("word/_rels/document.xml.rels", opts)
                .unwrap();
            zip.write_all(doc_rels.as_bytes()).unwrap();
            zip.start_file("word/document.xml", opts).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        buf
    }

    #[test]
    fn paragraph_role_ids_agree_with_the_vocabulary_the_insert_op_validates_against() {
        // Invariant (the discoverability fix's foundation): the per-paragraph
        // role id `paragraph_role_ids` surfaces is ALWAYS a real id in
        // `extract_vocabulary().paragraph_roles` — the exact set the insert op
        // looks up. If these two derivations could disagree, the read view
        // would surface a token the insert op rejects.
        let docx = make_normal_docx(&["First body paragraph.", "Second body paragraph."]);
        let doc = crate::api::Document::parse(&docx).expect("parse");
        let canon = &doc.snapshot().canonical;

        let vocab = extract_vocabulary(canon);
        let valid_ids: std::collections::HashSet<&str> = vocab
            .paragraph_roles
            .iter()
            .map(|r| r.id.as_str())
            .collect();

        let role_ids = paragraph_role_ids(canon);
        assert!(!role_ids.is_empty(), "a doc with paragraphs has role ids");
        for (node, role_id) in &role_ids {
            assert!(
                valid_ids.contains(role_id.as_str()),
                "role id '{role_id}' for {node} must be a valid vocabulary role; \
                 valid: {valid_ids:?}"
            );
        }

        // And the document's body role resolves (the "default"/"body" alias).
        let body = default_body_role_id(canon).expect("a Normal doc has a body role");
        assert!(valid_ids.contains(body.as_str()));
    }
}
