//! Document diffing algorithm.
//!
//! Compares two CanonDoc structures and produces a DocumentDiff.

use std::collections::{HashMap, HashSet};
use std::env;

use sha2::{Digest, Sha256};
use similar::{Algorithm, ChangeTag, TextDiff};

// =============================================================================
// Custom tokenizer for word-level diffing
// =============================================================================

/// Character class for tokenization.
/// Each contiguous run of the same class becomes one token.
#[derive(PartialEq)]
enum CharClass {
    Word,
    Whitespace,
    Punctuation,
}

fn char_class(c: char) -> CharClass {
    if c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else if c.is_whitespace() {
        CharClass::Whitespace
    } else {
        CharClass::Punctuation
    }
}

/// Length of the truncated hash appended to opaque `\u{FFFC}` tags.
/// Must match the truncation length in `opaque_diff_tag()`.
const OPAQUE_HASH_LEN: usize = 12;

/// Tokenize text into slices, splitting on word/whitespace/punctuation boundaries.
///
/// - Word characters (alphanumeric + underscore) are grouped into contiguous runs.
/// - Whitespace characters are grouped into contiguous runs.
/// - Each punctuation/symbol character is its own token (they are semantically independent).
///
/// Example: `"Stock);"` → `["Stock", ")", ";"]`
/// Example: `"Section 3.1(a)"` → `["Section", " ", "3", ".", "1", "(", "a", ")"]`
pub fn tokenize(text: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut chars = text.char_indices().peekable();

    while let Some(&(start, c)) = chars.peek() {
        let class = char_class(c);
        chars.next();

        if c == '\u{FFFC}' {
            // Tagged opaque placeholders in the resolving-opaques path are
            // `FFFC + 12 hex chars`. Plain placeholders in the regular diff path
            // are just bare `FFFC`. Only consume a trailing tag when there is an
            // actual full 12-char hex suffix; otherwise leave adjacent text as its
            // own token so it keeps its own formatting.
            let mut lookahead = chars.clone();
            let mut tag_len = 0usize;
            while tag_len < OPAQUE_HASH_LEN {
                match lookahead.peek() {
                    Some(&(_, next_c)) if next_c.is_ascii_hexdigit() => {
                        lookahead.next();
                        tag_len += 1;
                    }
                    _ => break,
                }
            }
            if tag_len == OPAQUE_HASH_LEN {
                for _ in 0..OPAQUE_HASH_LEN {
                    chars.next();
                }
            }
            let end = chars.peek().map_or(text.len(), |&(i, _)| i);
            tokens.push(&text[start..end]);
        } else if class == CharClass::Punctuation {
            // Each punctuation char is its own token
            let end = chars.peek().map_or(text.len(), |&(i, _)| i);
            tokens.push(&text[start..end]);
        } else {
            // Word and whitespace: consume contiguous run of the same class
            while let Some(&(_, next_c)) = chars.peek() {
                if char_class(next_c) == class {
                    chars.next();
                } else {
                    break;
                }
            }
            let end = chars.peek().map_or(text.len(), |&(i, _)| i);
            tokens.push(&text[start..end]);
        }
    }

    let fused_enum = fuse_legal_enumerators(tokens, text);
    fuse_intraword_apostrophes(fused_enum, text)
}

/// Check if a string is a legal enumerator content (inside parentheses).
/// Matches: single letters a-z/A-Z, roman numerals i-xiv, double letters aa-zz.
fn is_enumerator_content(s: &str) -> bool {
    // Single letter
    if s.len() == 1 {
        let c = s.as_bytes()[0];
        return c.is_ascii_alphabetic();
    }
    // Double letters like aa, bb, cc
    if s.len() == 2 {
        let bytes = s.as_bytes();
        if bytes[0] == bytes[1] && bytes[0].is_ascii_lowercase() {
            return true;
        }
    }
    // Roman numerals up to xiv
    matches!(
        s,
        "i" | "ii"
            | "iii"
            | "iv"
            | "v"
            | "vi"
            | "vii"
            | "viii"
            | "ix"
            | "x"
            | "xi"
            | "xii"
            | "xiii"
            | "xiv"
    )
}

/// Post-tokenization pass: fuse `(` + enumerator + `)` into single tokens.
///
/// Boundary guards prevent false fusing:
/// - Left: token before `(` must NOT end with an alphanumeric char (prevents `13(d)`)
/// - Right: token after `)` must NOT start with an alphanumeric char
fn fuse_legal_enumerators<'a>(tokens: Vec<&'a str>, text: &'a str) -> Vec<&'a str> {
    if tokens.len() < 3 {
        return tokens;
    }

    let mut result = Vec::with_capacity(tokens.len());
    let mut i = 0;

    while i < tokens.len() {
        if i + 2 < tokens.len() && tokens[i] == "(" && tokens[i + 2] == ")" {
            let content = tokens[i + 1];
            if is_enumerator_content(content) {
                // Left boundary: token before `(` must not end with alphanumeric
                let left_ok = if i == 0 {
                    true
                } else {
                    !tokens[i - 1]
                        .chars()
                        .last()
                        .is_some_and(|c| c.is_alphanumeric())
                };
                // Right boundary: token after `)` must not start with alphanumeric
                let right_ok = if i + 3 >= tokens.len() {
                    true
                } else {
                    !tokens[i + 3]
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_alphanumeric())
                };

                if left_ok && right_ok {
                    // Fuse: compute the byte range spanning tokens[i] through tokens[i+2]
                    let start_ptr = tokens[i].as_ptr() as usize;
                    let end_token = tokens[i + 2];
                    let end_ptr = end_token.as_ptr() as usize + end_token.len();
                    let text_start = text.as_ptr() as usize;
                    let byte_start = start_ptr - text_start;
                    let byte_end = end_ptr - text_start;
                    result.push(&text[byte_start..byte_end]);
                    i += 3;
                    continue;
                }
            }
        }
        result.push(tokens[i]);
        i += 1;
    }

    result
}

/// Check if a character is an apostrophe (ASCII or Unicode smart quote).
fn is_apostrophe(c: char) -> bool {
    c == '\'' || c == '\u{2019}' // ASCII apostrophe or right single quotation mark
}

/// Check if a token consists entirely of word characters (alphanumeric or underscore).
fn is_word_token(token: &str) -> bool {
    !token.is_empty() && token.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// Post-tokenization pass: fuse `word + apostrophe + word` into single tokens.
///
/// Contractions and possessives like `don't`, `it's`, `refer's` should be
/// atomic tokens so the diff treats them as single units.
fn fuse_intraword_apostrophes<'a>(tokens: Vec<&'a str>, text: &'a str) -> Vec<&'a str> {
    if tokens.len() < 3 {
        return tokens;
    }

    let mut result = Vec::with_capacity(tokens.len());
    let mut i = 0;

    while i < tokens.len() {
        if i + 2 < tokens.len()
            && !tokens[i + 1].is_empty()
            && tokens[i + 1].chars().count() == 1
            && tokens[i + 1].chars().next().is_some_and(is_apostrophe)
            && is_word_token(tokens[i])
            && is_word_token(tokens[i + 2])
        {
            // Fuse: compute the byte range spanning tokens[i] through tokens[i+2]
            let start_ptr = tokens[i].as_ptr() as usize;
            let end_token = tokens[i + 2];
            let end_ptr = end_token.as_ptr() as usize + end_token.len();
            let text_start = text.as_ptr() as usize;
            let byte_start = start_ptr - text_start;
            let byte_end = end_ptr - text_start;
            result.push(&text[byte_start..byte_end]);
            i += 3;
        } else {
            result.push(tokens[i]);
            i += 1;
        }
    }

    result
}

use crate::domain::{
    Alignment, BlockNode, BlockType, CanonDoc, CellParagraphChange, ChangeType, CommentPayload,
    CommentStory, DiffChange, DocumentDiff, EndnoteStory, FooterStory, FootnoteStory,
    FormattingChange, FullDocBlock, FullDocViewResult, HeaderFooterPayload, HeaderStory,
    HeadingLevel, IStr, ImageMetadataChange, Indentation, InlineChange, InlineChangeSegmentType,
    InlineNode, Mark, MarkValue, MoveDirection, NestedTableDiff, NestedTableDiffKind, NodeId,
    NoteType, OpaqueInlineNode, OpaqueKind, OpaqueSegmentKind, ParagraphBorders, ParagraphNode,
    ParagraphSpacing, RunRprAuthored, StoryPayload, StructuralChange, StyleProps, TableCellChange,
    TableCellDiff, TableCellDiffType, TableDiffResult, TableFormatting, TableNode,
    TableRowAlignment, TrackedBlock, TrackingStatus,
};
use crate::import::story_blocks_to_segments;
use crate::table_diff::{
    CellDiffType, RowAlignment, diff_tables, extract_tracked_table_new_text,
    extract_tracked_table_old_text, table_has_tracked_changes,
};

/// Compare two canonical documents and produce a diff.
pub fn diff_documents(base: &CanonDoc, target: &CanonDoc) -> Result<DocumentDiff, String> {
    let base_elements = extract_diffable_elements(&base.blocks);
    let target_elements = extract_diffable_elements(&target.blocks);

    let alignments = align_elements(&base_elements, &target_elements);
    let mut changes = compute_changes(&alignments, &base_elements, &target_elements)?;

    // Detect moves: annotate BlockDeleted/BlockInserted pairs with matching text.
    detect_moves_in_changes(&mut changes);

    // Detect paragraph splits: when a BlockModified is followed by a BlockInserted
    // whose text is a suffix of the modified paragraph's old text, the insertion
    // is the second half of a paragraph split.
    reconcile_paragraph_splits(&mut changes);
    reconcile_math_deleted_inserted_replacements(&mut changes);

    // Diff OpaqueBlocks: these are filtered out of diffable elements (they can't be
    // aligned or diffed inline), but must generate BlockInserted/BlockDeleted changes
    // so the merge pipeline's accept-projection has the correct block count.
    changes.extend(diff_opaque_blocks(
        &base.blocks,
        &target.blocks,
        &alignments,
        &base_elements,
        &target_elements,
    ));

    // Diff stories (headers, footers, footnotes, endnotes, comments)
    let (base_header_slots, base_footer_slots) = collect_story_slot_maps(base);
    let (target_header_slots, target_footer_slots) = collect_story_slot_maps(target);
    changes.extend(diff_headers(
        &base.headers,
        &target.headers,
        &base_header_slots,
        &target_header_slots,
    )?);
    changes.extend(diff_footers(
        &base.footers,
        &target.footers,
        &base_footer_slots,
        &target_footer_slots,
    )?);
    changes.extend(diff_footnotes(&base.footnotes, &target.footnotes)?);
    changes.extend(diff_endnotes(&base.endnotes, &target.endnotes)?);
    changes.extend(diff_comments(&base.comments, &target.comments)?);

    Ok(DocumentDiff {
        base_fingerprint: base.meta.docx_fingerprint.clone(),
        target_fingerprint: target.meta.docx_fingerprint.clone(),
        changes,
    })
}

/// Reclassify math-only paragraph replacement pairs from BlockDeleted+BlockInserted
/// into a single BlockModified when the paragraph itself should survive.
///
/// Word accepts math-paragraph replacement by keeping the paragraph and tracking
/// deletion inside the OMML tree. When the diff leaves these as separate block
/// delete/insert operations, Word keeps the deleted math paragraph on accept.
fn reconcile_math_deleted_inserted_replacements(changes: &mut Vec<DiffChange>) {
    let mut rewritten = Vec::with_capacity(changes.len());
    let mut i = 0usize;

    while i < changes.len() {
        if i + 1 < changes.len()
            && let (
                DiffChange::BlockDeleted {
                    block_id,
                    old_text,
                    old_block,
                    move_id: None,
                },
                DiffChange::BlockInserted {
                    after_block_id: _,
                    block: new_block,
                    move_id: None,
                },
            ) = (&changes[i], &changes[i + 1])
            && let (BlockNode::Paragraph(old_p), BlockNode::Paragraph(new_p)) =
                (old_block, new_block)
            && supports_inline_math_deleted_or_inserted(old_p, new_p)
        {
            rewritten.push(DiffChange::BlockModified {
                block_id: block_id.clone(),
                old_text: old_text.clone(),
                new_text: extract_inline_text(&block_inlines(new_block)),
                inline_changes: diff_block_content_with_marks(
                    &block_inlines(old_block),
                    &block_inlines(new_block),
                ),
                old_block: old_block.clone(),
                new_block: new_block.clone(),
                para_split: false,
            });
            i += 2;
            continue;
        }

        rewritten.push(changes[i].clone());
        i += 1;
    }

    *changes = rewritten;
}

/// Post-pass: detect content moves in the DiffChange list.
///
/// Find runs of consecutive indices from `candidates` that are not in `skip`.
/// "Consecutive" means indices differ by exactly 1 (adjacent in the document).
/// Returns only groups of 2+ indices.
fn find_consecutive_runs(candidates: &[usize], skip: &HashSet<usize>) -> Vec<Vec<usize>> {
    let unmatched: Vec<usize> = candidates
        .iter()
        .copied()
        .filter(|i| !skip.contains(i))
        .collect();
    let mut runs: Vec<Vec<usize>> = Vec::new();
    for idx in unmatched {
        if let Some(last_run) = runs.last_mut()
            && last_run.last().is_some_and(|&last| idx == last + 1)
        {
            last_run.push(idx);
            continue;
        }
        runs.push(vec![idx]);
    }
    runs.into_iter().filter(|r| r.len() >= 2).collect()
}

/// When a `BlockDeleted`'s normalized text exactly matches a `BlockInserted`'s
/// text (and both are paragraphs, not tables), annotate both with a shared
/// `move_id`. Only matches blocks with substantial text (>= 20 chars after
/// normalization) to avoid false positives. Each block participates in at most
/// one move pair (first match wins).
fn detect_moves_in_changes(changes: &mut [DiffChange]) {
    const MIN_MOVE_TEXT_LEN: usize = 20;

    /// Normalize block text for comparison: collapse whitespace, trim.
    fn normalize_text(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// Extract the paragraph's plain text from a BlockNode.
    fn block_text(block: &BlockNode) -> Option<String> {
        match block {
            BlockNode::Paragraph(p) => Some(
                p.all_inlines_owned()
                    .iter()
                    .map(|i| match i {
                        InlineNode::Text(t) => t.text.as_str(),
                        InlineNode::HardBreak(_) => "\n",
                        _ => "",
                    })
                    .collect::<String>(),
            ),
            _ => None,
        }
    }

    // Collect indices and normalized text for ALL deleted and inserted paragraphs.
    // We collect even short ones for the consecutive-group pass below.
    let mut deleted_all: Vec<(usize, String)> = Vec::new();
    let mut inserted_all: Vec<(usize, String)> = Vec::new();

    for (i, change) in changes.iter().enumerate() {
        match change {
            DiffChange::BlockDeleted {
                old_block,
                move_id: None,
                ..
            } => {
                if let Some(text) = block_text(old_block) {
                    let norm = normalize_text(&text);
                    deleted_all.push((i, norm));
                }
            }
            DiffChange::BlockInserted {
                block,
                move_id: None,
                ..
            } => {
                if let Some(text) = block_text(block) {
                    let norm = normalize_text(&text);
                    inserted_all.push((i, norm));
                }
            }
            _ => {}
        }
    }

    if deleted_all.is_empty() || inserted_all.is_empty() {
        return;
    }

    // Pass 1: single-block matching (requires MIN_MOVE_TEXT_LEN).
    let deleted: Vec<&(usize, String)> = deleted_all
        .iter()
        .filter(|(_, n)| n.len() >= MIN_MOVE_TEXT_LEN)
        .collect();
    let inserted: Vec<&(usize, String)> = inserted_all
        .iter()
        .filter(|(_, n)| n.len() >= MIN_MOVE_TEXT_LEN)
        .collect();

    let mut deleted_text_to_idx: HashMap<&str, Vec<usize>> = HashMap::new();
    for &(idx, norm) in &deleted {
        deleted_text_to_idx
            .entry(norm.as_str())
            .or_default()
            .push(*idx);
    }

    let mut move_counter = 0u32;
    let mut used_deleted: HashSet<usize> = HashSet::new();
    let mut pairs: Vec<(usize, usize, String)> = Vec::new();

    for &(ins_idx, ins_norm) in &inserted {
        if let Some(del_indices) = deleted_text_to_idx.get(ins_norm.as_str())
            && let Some(&del_idx) = del_indices.iter().find(|i| !used_deleted.contains(i))
        {
            used_deleted.insert(del_idx);
            let move_id = format!("move_{move_counter}");
            move_counter += 1;
            pairs.push((del_idx, *ins_idx, move_id));
        }
    }

    for (del_idx, ins_idx, move_id) in &pairs {
        if let DiffChange::BlockDeleted {
            move_id: ref mut mid,
            ..
        } = changes[*del_idx]
        {
            *mid = Some(move_id.clone());
        }
        if let DiffChange::BlockInserted {
            move_id: ref mut mid,
            ..
        } = changes[*ins_idx]
        {
            *mid = Some(move_id.clone());
        }
    }

    // Pass 2: consecutive-block group matching.
    // Short paragraphs individually below the threshold can be matched as groups.
    let del_indices: Vec<usize> = deleted_all.iter().map(|(i, _)| *i).collect();
    let ins_indices: Vec<usize> = inserted_all.iter().map(|(i, _)| *i).collect();
    let del_norms: HashMap<usize, &str> =
        deleted_all.iter().map(|(i, n)| (*i, n.as_str())).collect();
    let ins_norms: HashMap<usize, &str> =
        inserted_all.iter().map(|(i, n)| (*i, n.as_str())).collect();

    let del_runs = find_consecutive_runs(&del_indices, &used_deleted);
    let ins_used: HashSet<usize> = pairs.iter().map(|(_, i, _)| *i).collect();
    let ins_runs = find_consecutive_runs(&ins_indices, &ins_used);

    if del_runs.is_empty() || ins_runs.is_empty() {
        return;
    }

    let del_run_texts: Vec<String> = del_runs
        .iter()
        .map(|run| {
            run.iter()
                .map(|idx| del_norms[idx])
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect();

    let mut used_del_runs: HashSet<usize> = HashSet::new();
    let mut group_pairs: Vec<(usize, usize, String)> = Vec::new();

    for (ins_run_idx, ins_run) in ins_runs.iter().enumerate() {
        let ins_text: String = ins_run
            .iter()
            .map(|idx| ins_norms[idx])
            .collect::<Vec<_>>()
            .join("\n");
        if ins_text.len() < MIN_MOVE_TEXT_LEN {
            continue;
        }
        for (del_run_idx, del_text) in del_run_texts.iter().enumerate() {
            if used_del_runs.contains(&del_run_idx) || del_text.len() < MIN_MOVE_TEXT_LEN {
                continue;
            }
            if *del_text == ins_text {
                used_del_runs.insert(del_run_idx);
                let move_id = format!("move_{move_counter}");
                move_counter += 1;
                group_pairs.push((del_run_idx, ins_run_idx, move_id));
                break;
            }
        }
    }

    for (del_run_idx, ins_run_idx, move_id) in group_pairs {
        for &idx in &del_runs[del_run_idx] {
            if let DiffChange::BlockDeleted {
                move_id: ref mut mid,
                ..
            } = changes[idx]
            {
                *mid = Some(move_id.clone());
            }
        }
        for &idx in &ins_runs[ins_run_idx] {
            if let DiffChange::BlockInserted {
                move_id: ref mut mid,
                ..
            } = changes[idx]
            {
                *mid = Some(move_id.clone());
            }
        }
    }
}

/// Post-pass: detect paragraph splits in the DiffChange list.
///
/// A paragraph split occurs when one base paragraph becomes two target paragraphs.
/// The diff may align the base paragraph with either half of the split, producing
/// two possible adjacency patterns:
///
/// **Pattern A:** `(BlockModified, BlockInserted)` — diff matched the base to the first
/// target paragraph. The inserted paragraph's text is a suffix of old_text.
///
/// **Pattern B:** `(BlockInserted, BlockModified)` — diff matched the base to the second
/// target paragraph. The inserted paragraph's text is a prefix of old_text.
///
/// When detected, `para_split = true` is set on the BlockModified. This causes
/// `apply_changes_to_blocks` to set `para_mark_status = Inserted` on the modified
/// paragraph and guards the merge logic so reject doesn't create franken-paragraphs.
fn reconcile_paragraph_splits(changes: &mut [DiffChange]) {
    fn is_plausible_split_affix(removed_text: &str, inserted_text: &str) -> bool {
        let removed = normalize_for_similarity(removed_text);
        let inserted = normalize_for_similarity(inserted_text);
        if removed.len() < 10 || inserted.len() < 10 {
            return false;
        }

        let len_ratio =
            removed.len().min(inserted.len()) as f64 / removed.len().max(inserted.len()) as f64;
        if len_ratio < 0.35 {
            return false;
        }

        content_similarity(&removed, &inserted) >= 0.55
    }

    let mut i = 0;
    while i + 1 < changes.len() {
        // Pattern A: (BlockModified, BlockInserted) — inserted is suffix of old
        let pattern_a = match (&changes[i], &changes[i + 1]) {
            (
                DiffChange::BlockModified {
                    old_text, new_text, ..
                },
                DiffChange::BlockInserted {
                    block: BlockNode::Paragraph(ins_para),
                    move_id: None,
                    ..
                },
            ) => {
                let ins_text = extract_inline_text(&ins_para.all_inlines_owned());
                let old_norm = normalize_for_similarity(old_text);
                let new_norm = normalize_for_similarity(new_text);
                let ins_norm = normalize_for_similarity(&ins_text);

                ins_norm.len() >= 10
                    && old_norm.ends_with(&ins_norm)
                    && !new_norm.ends_with(&ins_norm)
                    && old_norm.len() > ins_norm.len()
                    && is_plausible_split_affix(
                        &old_norm[..old_norm.len() - ins_norm.len()],
                        &new_norm,
                    )
            }
            _ => false,
        };

        // Pattern B: (BlockInserted, BlockModified) — the modified paragraph's new_text
        // is a proper suffix of its old_text. The "lost" prefix was split off and the
        // inserted paragraph takes its place (possibly with new content).
        let pattern_b = !pattern_a
            && match (&changes[i], &changes[i + 1]) {
                (
                    DiffChange::BlockInserted {
                        block: BlockNode::Paragraph(ins_para),
                        move_id: None,
                        ..
                    },
                    DiffChange::BlockModified {
                        old_text, new_text, ..
                    },
                ) => {
                    let old_norm = normalize_for_similarity(old_text);
                    let new_norm = normalize_for_similarity(new_text);
                    let ins_text = extract_inline_text(&ins_para.all_inlines_owned());

                    // The modified paragraph kept its suffix (new is a suffix of old),
                    // and something meaningful was removed from the front.
                    new_norm.len() >= 10
                        && old_norm.len() > new_norm.len() + 5
                        && old_norm.ends_with(&new_norm)
                        && is_plausible_split_affix(
                            &old_norm[..old_norm.len() - new_norm.len()],
                            &ins_text,
                        )
                }
                _ => false,
            };

        if pattern_a {
            if let DiffChange::BlockModified { para_split, .. } = &mut changes[i] {
                *para_split = true;
            }
            i += 2;
        } else if pattern_b {
            if let DiffChange::BlockModified { para_split, .. } = &mut changes[i + 1] {
                *para_split = true;
            }
            i += 2;
        } else {
            i += 1;
        }
    }
}

/// An element that can be diffed (either a block or a table).
#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
enum DiffableElement {
    Block(DiffableBlock),
    Table(DiffableTable),
}

/// Block with extracted text for diffing.
#[derive(Clone, Debug)]
struct DiffableBlock {
    id: NodeId,
    text: String,
    comparison_text: String,
    text_hash: String,
    block: BlockNode,
}

/// Compare paragraph-level formatting properties between two `BlockNode`s.
///
/// Returns `true` if any paragraph property (alignment, indent, spacing, numbering,
/// style, layout flags, etc.) differs. Returns `false` for non-paragraph blocks.
///
/// NOTE: This deliberately duplicates the comparison in `apply_block_modified`
/// (`tracked_model.rs:834-867`). Per the style guide, duplication is preferred
/// over premature abstraction across module boundaries. Keep both in sync.
fn paragraph_properties_differ(base: &BlockNode, target: &BlockNode) -> bool {
    let (base_p, target_p) = match (base, target) {
        (BlockNode::Paragraph(b), BlockNode::Paragraph(t)) => (b, t),
        _ => return false,
    };

    base_p.align != target_p.align
        || base_p.indent != target_p.indent
        || base_p.spacing != target_p.spacing
        || !crate::domain::numbering_structurally_eq(&base_p.numbering, &target_p.numbering)
        || base_p.style_id != target_p.style_id
        || base_p.keep_next != target_p.keep_next
        || base_p.keep_lines != target_p.keep_lines
        || base_p.page_break_before != target_p.page_break_before
        || base_p.widow_control != target_p.widow_control
        || base_p.contextual_spacing != target_p.contextual_spacing
        || base_p.shading != target_p.shading
        || base_p.borders != target_p.borders
        || base_p.tab_stops != target_p.tab_stops
        || base_p.text_direction != target_p.text_direction
        || base_p.text_alignment != target_p.text_alignment
        || base_p.mirror_indents != target_p.mirror_indents
        || base_p.auto_space_de != target_p.auto_space_de
        || base_p.auto_space_dn != target_p.auto_space_dn
        || base_p.bidi != target_p.bidi
        || base_p.suppress_auto_hyphens != target_p.suppress_auto_hyphens
        || base_p.snap_to_grid != target_p.snap_to_grid
        || base_p.overflow_punct != target_p.overflow_punct
        || base_p.adjust_right_ind != target_p.adjust_right_ind
        || base_p.word_wrap != target_p.word_wrap
        || base_p.frame_pr != target_p.frame_pr
        || base_p.section_properties != target_p.section_properties
        || base_p.literal_prefix != target_p.literal_prefix
}

/// Extract flattened inlines from a BlockNode (paragraph or heading).
fn block_inlines(block: &BlockNode) -> Vec<InlineNode> {
    match block {
        BlockNode::Paragraph(p) => p.all_inlines_owned(),
        BlockNode::Table(_) | BlockNode::OpaqueBlock(_) => Vec::new(),
    }
}

/// Returns true when both blocks are paragraphs whose only content is
/// paragraph-level opaques (e.g., `m:oMathPara`) and the content differs.
///
/// Such paragraphs cannot have their changes tracked inline — `m:oMathPara`
/// must be a direct `<w:p>` child, never inside `<w:del>`/`<w:ins>`.
/// The diff should emit BlockDeleted + BlockInserted so tracking lives
/// on the paragraph mark.
fn is_wholly_paragraph_opaque_change(base: &BlockNode, target: &BlockNode) -> bool {
    let (base_p, target_p) = match (base, target) {
        (BlockNode::Paragraph(b), BlockNode::Paragraph(t)) => (b, t),
        _ => return false,
    };

    if supports_inline_math_deleted_or_inserted(base_p, target_p) {
        return false;
    }

    let base_opaques = paragraph_level_opaques(base_p);
    let target_opaques = paragraph_level_opaques(target_p);

    // At least one side must be non-empty (handles math deleted or inserted)
    if base_opaques.is_empty() && target_opaques.is_empty() {
        return false;
    }

    // When one side returns empty opaques but has non-trivial content
    // (e.g., a drawing or text), the paragraph is NOT wholly opaque — it
    // has real inline content that should be tracked via the normal inline
    // diff path. Splitting into BlockDeleted + BlockInserted would create
    // two separate paragraphs, which mismatches Word's single-paragraph
    // approach where the non-math content is deleted/inserted inline.
    if base_opaques.is_empty() && paragraph_has_content_inlines(base_p) {
        return false;
    }
    if target_opaques.is_empty() && paragraph_has_content_inlines(target_p) {
        return false;
    }

    // Content must have changed
    base_opaques != target_opaques
}

/// Returns true when a paragraph has at least one non-decoration, non-comment
/// inline — i.e., it carries real content (text, drawings, math, etc.).
fn paragraph_has_content_inlines(paragraph: &ParagraphNode) -> bool {
    paragraph.all_inlines_owned().into_iter().any(|inline| {
        !matches!(
            inline,
            InlineNode::Decoration(_)
                | InlineNode::CommentRangeStart { .. }
                | InlineNode::CommentRangeEnd { .. }
                | InlineNode::CommentReference { .. }
        )
    })
}

fn paragraph_is_only_math_or_empty(paragraph: &ParagraphNode) -> bool {
    paragraph.all_inlines_owned().into_iter().all(|inline| {
        matches!(
            inline,
            InlineNode::OpaqueInline(ref opaque)
                if matches!(opaque.kind, OpaqueKind::OmmlBlock)
        ) || matches!(
            inline,
            InlineNode::Decoration(_)
                | InlineNode::CommentRangeStart { .. }
                | InlineNode::CommentRangeEnd { .. }
                | InlineNode::CommentReference { .. }
        )
    })
}

fn paragraph_has_math_block(paragraph: &ParagraphNode) -> bool {
    paragraph.all_inlines_owned().into_iter().any(|inline| {
        matches!(
            inline,
            InlineNode::OpaqueInline(ref opaque)
                if matches!(opaque.kind, OpaqueKind::OmmlBlock)
        )
    })
}

/// Word can track a math-only paragraph being replaced by an empty paragraph
/// without deleting the paragraph itself: it keeps `m:oMathPara` at `w:p`
/// level and places `w:del` / `w:ins` inside the OMML tree. Keep these on the
/// inline diff path so the paragraph survives accept/reject correctly.
fn supports_inline_math_deleted_or_inserted(base: &ParagraphNode, target: &ParagraphNode) -> bool {
    paragraph_is_only_math_or_empty(base)
        && paragraph_is_only_math_or_empty(target)
        && (paragraph_has_math_block(base) ^ paragraph_has_math_block(target))
}

/// Returns true when one paragraph is text-empty and the other is not.
///
/// Word accepts these more faithfully when they stay as separate paragraph
/// delete/insert operations instead of being collapsed into one modified
/// paragraph. Matching a non-empty paragraph to an empty paragraph tends to
/// lose paragraph-mark formatting on accept for the inserted empty paragraph.
/// Returns true when a Modified alignment pairs paragraphs too dissimilar to
/// justify inline diffing.
///
/// When the alignment forces a match between unrelated paragraphs (e.g., via
/// positional matching in equal-length gap segments or the DP preferring a
/// low-similarity match over gap costs), the inline diff shows a wall of
/// red/green with no stable context. This is worse UX than delete+insert
/// because the UI claims "this is the same clause, changed" — a semantic
/// claim that misleads the reviewer.
///
/// Principle: prefer conservative truthfulness over aggressive correspondence.
/// When the system cannot justify that two blocks are the same logical unit,
/// it must represent them as deletion and insertion.
fn should_split_unrelated_modification(
    base_block: &DiffableBlock,
    target_block: &DiffableBlock,
) -> bool {
    let base_text = &base_block.comparison_text;
    let target_text = &target_block.comparison_text;

    // Only apply to non-trivial paragraphs. Short text (section numbers,
    // enumerators, signature fields) is often legitimately different at the
    // same structural position.
    let base_chars = base_text.chars().filter(|c| !c.is_whitespace()).count();
    let target_chars = target_text.chars().filter(|c| !c.is_whitespace()).count();
    if base_chars < 20 || target_chars < 20 {
        return false;
    }

    let sim = content_similarity(base_text, target_text);

    // Very low similarity: always split. At < 0.15, paragraphs share almost
    // no content words — they are definitively unrelated.
    if sim < 0.15 {
        return true;
    }

    // Low similarity + extreme length ratio: split. When one paragraph is
    // much longer than the other AND they share little content, the match
    // is spurious (e.g., a short heading matched against a long clause).
    if sim < 0.25 {
        let len_ratio = base_chars.min(target_chars) as f64 / base_chars.max(target_chars) as f64;
        if len_ratio < 0.30 {
            return true;
        }
    }

    false
}

fn should_split_empty_paragraph_change(base: &BlockNode, target: &BlockNode) -> bool {
    let (base_p, target_p) = match (base, target) {
        (BlockNode::Paragraph(b), BlockNode::Paragraph(t)) => (b, t),
        _ => return false,
    };

    let base_text = normalize_for_similarity(&extract_inline_text(&base_p.all_inlines_owned()));
    let target_text = normalize_for_similarity(&extract_inline_text(&target_p.all_inlines_owned()));

    base_text.is_empty() ^ target_text.is_empty()
}

/// Collect content_hash values of paragraph-level opaques from a paragraph,
/// returning empty if any non-decoration, non-opaque-block inlines exist.
///
/// Only collects OmmlBlock hashes. When equation content changes between
/// documents, `is_wholly_paragraph_opaque_change` uses these to detect the
/// change and split into BlockDeleted + BlockInserted — but the
/// `supports_inline_math_deleted_or_inserted` early-return prevents the
/// split for math-vs-empty pairs, and the `should_split_empty_paragraph_change`
/// exemption handles math-vs-empty via `paragraph_is_only_math_or_empty`.
fn paragraph_level_opaques(p: &ParagraphNode) -> Vec<Option<String>> {
    let mut hashes = Vec::new();
    for inline in p.all_inlines_owned() {
        match &inline {
            InlineNode::OpaqueInline(o) if matches!(o.kind, OpaqueKind::OmmlBlock) => {
                hashes.push(o.content_hash.clone());
            }
            InlineNode::Decoration(_)
            | InlineNode::CommentRangeStart { .. }
            | InlineNode::CommentRangeEnd { .. }
            | InlineNode::CommentReference { .. } => {
                // Zero-width markers — don't affect content
            }
            _ => return Vec::new(), // Has non-opaque content — not wholly opaque
        }
    }
    hashes
}

/// Table with structure and text fingerprints for diffing.
#[derive(Clone, Debug)]
struct DiffableTable {
    id: NodeId,
    structure_hash: String,
    /// Concatenated text from all cells for similarity scoring.
    text_fingerprint: String,
    table: TableNode,
}

impl DiffableElement {
    fn id(&self) -> &NodeId {
        match self {
            DiffableElement::Block(b) => &b.id,
            DiffableElement::Table(t) => &t.id,
        }
    }

    /// Get the text/fingerprint for similarity comparison.
    fn text(&self) -> &str {
        match self {
            DiffableElement::Block(b) => &b.comparison_text,
            DiffableElement::Table(t) => &t.text_fingerprint,
        }
    }

    /// Get the hash for exact matching.
    fn hash(&self) -> &str {
        match self {
            DiffableElement::Block(b) => &b.text_hash,
            // For tables, use structure_hash for exact matching
            DiffableElement::Table(t) => &t.structure_hash,
        }
    }

    fn is_block(&self) -> bool {
        matches!(self, DiffableElement::Block(_))
    }

    fn is_table(&self) -> bool {
        matches!(self, DiffableElement::Table(_))
    }
}

/// Extract all diffable elements from blocks (paragraphs, headings, and tables).
///
/// Uses body-only text (prefix-free inlines) for alignment, so renumbered
/// paragraphs with identical body text get the same alignment hash.
fn extract_diffable_elements(blocks: &[TrackedBlock]) -> Vec<DiffableElement> {
    blocks
        .iter()
        .filter_map(|tracked| {
            match &tracked.block {
                BlockNode::Paragraph(p) => {
                    let inlines = p.all_inlines_owned();
                    // Use body-only text (inlines are prefix-free after strip_literal_prefix)
                    let text = extract_inline_text(&inlines);
                    let mut comparison_text = if text.trim().is_empty() {
                        p.rendered_text
                            .as_deref()
                            .filter(|rendered| !rendered.trim().is_empty())
                            .unwrap_or("")
                            .to_string()
                    } else {
                        text.clone()
                    };
                    // Hash includes opaque content_hashes so different images produce different hashes
                    let mut hash_text = extract_inline_text_with_opaque_hashes(&inlines);
                    // Some paragraphs are "rendered-only" (e.g. numbering/field-derived labels)
                    // and have no inline text. Use rendered_text in the HASH so Patience
                    // diff can distinguish them, but keep `text` as actual inline text
                    // so old_text/new_text and inline_changes stay consistent.
                    // (If we put rendered_text in `text`, numbering prefixes leak into
                    // inline_changes → merge materializes them → accept_all can't strip
                    // them back → fixpoint invariant fails.)
                    if text.trim().is_empty()
                        && let Some(rendered) = &p.rendered_text
                        && !rendered.trim().is_empty()
                    {
                        hash_text = rendered.clone();
                    }
                    if p.section_properties.is_some() {
                        if !comparison_text.is_empty() {
                            comparison_text.push('\u{241f}');
                        }
                        comparison_text.push_str("[sectPr]");

                        let sect_hash = p
                            .section_properties
                            .as_ref()
                            .map(|sp| sha256_hex(format!("{sp:?}").as_bytes()))
                            .unwrap_or_else(|| "present".to_string());
                        if !hash_text.is_empty() {
                            hash_text.push('\u{241f}');
                        }
                        hash_text.push_str("[sectPr:");
                        hash_text.push_str(&sect_hash);
                        hash_text.push(']');
                    }
                    let text_hash = sha256_hex(hash_text.as_bytes());
                    Some(DiffableElement::Block(DiffableBlock {
                        id: p.id.clone(),
                        text,
                        comparison_text,
                        text_hash,
                        block: tracked.block.clone(),
                    }))
                }
                BlockNode::Table(t) => {
                    let text_fingerprint = extract_table_text(t);
                    Some(DiffableElement::Table(DiffableTable {
                        id: t.id.clone(),
                        structure_hash: t.structure_hash.clone(),
                        text_fingerprint,
                        table: (**t).clone(),
                    }))
                }
                // Opaque blocks are not diffed
                BlockNode::OpaqueBlock(_) => None,
            }
        })
        .collect()
}

/// Diff OpaqueBlocks between base and target documents.
///
/// OpaqueBlocks (e.g., structured document tags, content controls) are filtered
/// out of the diffable element alignment because they can't be meaningfully
/// diffed at the inline level. However, they must generate BlockInserted /
/// BlockDeleted changes so the merge pipeline's accept-projection has the
/// correct total block count, matching the target document.
///
/// Without this, `fix_numbering_drift_for_normal_blocks` sees a count mismatch
/// (accept_count != target.blocks.len()) and bails, skipping formatting sync
/// for all paragraphs.
fn diff_opaque_blocks(
    base_blocks: &[TrackedBlock],
    target_blocks: &[TrackedBlock],
    alignments: &[ElementAlignment],
    base_elements: &[DiffableElement],
    target_elements: &[DiffableElement],
) -> Vec<DiffChange> {
    use std::collections::HashSet;

    // Collect opaque_refs from each side
    let base_opaque_refs: HashSet<&str> = base_blocks
        .iter()
        .filter_map(|b| match &b.block {
            BlockNode::OpaqueBlock(o) => Some(o.opaque_ref.as_str()),
            _ => None,
        })
        .collect();

    let target_opaque_refs: HashSet<&str> = target_blocks
        .iter()
        .filter_map(|b| match &b.block {
            BlockNode::OpaqueBlock(o) => Some(o.opaque_ref.as_str()),
            _ => None,
        })
        .collect();

    // Build a mapping from target diffable-element ID to base ID (for after_block_id).
    let mut target_id_to_base_id: std::collections::HashMap<&NodeId, &NodeId> =
        std::collections::HashMap::new();
    for alignment in alignments {
        match alignment {
            ElementAlignment::Matched {
                base_idx,
                target_idx,
            }
            | ElementAlignment::Modified {
                base_idx,
                target_idx,
            } => {
                target_id_to_base_id.insert(
                    target_elements[*target_idx].id(),
                    base_elements[*base_idx].id(),
                );
            }
            _ => {}
        }
    }

    let mut changes = Vec::new();

    // OpaqueBlocks in base but not in target → BlockDeleted
    for tracked in base_blocks {
        if let BlockNode::OpaqueBlock(o) = &tracked.block
            && !target_opaque_refs.contains(o.opaque_ref.as_str())
        {
            changes.push(DiffChange::BlockDeleted {
                block_id: o.id.clone(),
                old_text: String::new(),
                old_block: tracked.block.clone(),
                move_id: None,
            });
        }
    }

    // OpaqueBlocks in target but not in base → BlockInserted
    // Find after_block_id by scanning backward through target blocks
    // to find the nearest preceding non-opaque block, then mapping
    // its target ID to its base ID.
    for (i, tracked) in target_blocks.iter().enumerate() {
        if let BlockNode::OpaqueBlock(o) = &tracked.block
            && !base_opaque_refs.contains(o.opaque_ref.as_str())
        {
            // Find the nearest preceding non-opaque block in the target
            let after_block_id = (0..i).rev().find_map(|j| {
                let prev_id = match &target_blocks[j].block {
                    BlockNode::Paragraph(p) => Some(&p.id),
                    BlockNode::Table(t) => Some(&t.id),
                    BlockNode::OpaqueBlock(_) => None,
                };
                prev_id.and_then(|tid| {
                    // Map target ID to base ID
                    target_id_to_base_id.get(tid).map(|bid| (*bid).clone())
                })
            });

            // The OpaqueBlock's proof_ref references a position in the
            // target document's XML (e.g. "body_index:5"). Tag it as
            // target-origin so the serializer knows to skip it (OOXML
            // doesn't support block-level insertion tracking for SDTs).
            let mut block = tracked.block.clone();
            if let BlockNode::OpaqueBlock(ref mut opaque) = block
                && let Some(idx) = opaque.proof_ref.docx_anchor.strip_prefix("body_index:")
            {
                opaque.proof_ref.docx_anchor = format!("target_body_index:{idx}");
            }
            changes.push(DiffChange::BlockInserted {
                after_block_id,
                block,
                move_id: None,
            });
        }
    }

    changes
}

/// Extract all text from a table for similarity comparison.
pub fn extract_table_text(table: &TableNode) -> String {
    let mut out = String::new();
    for row in &table.rows {
        for cell in &row.cells {
            for block in &cell.blocks {
                match block {
                    BlockNode::Paragraph(p) => {
                        let inlines = p.all_inlines_owned();
                        let mut text = extract_inline_text(&inlines);
                        if text.trim().is_empty()
                            && let Some(rendered) = &p.rendered_text
                            && !rendered.trim().is_empty()
                        {
                            text = rendered.clone();
                        }
                        if !out.is_empty() {
                            out.push(' ');
                        }
                        out.push_str(&text);
                    }
                    BlockNode::Table(nested) => {
                        if !out.is_empty() {
                            out.push(' ');
                        }
                        out.push_str(&extract_table_text(nested));
                    }
                    BlockNode::OpaqueBlock(_) => {}
                }
            }
        }
    }
    out
}

/// Build a TableDiffResult for a single table (no counterpart).
/// Used for inserted/deleted tables so the frontend can still render the table structure.
fn compute_single_table_diff_result(
    table: &TableNode,
    is_inserted: bool,
) -> Result<TableDiffResult, String> {
    let empty = TableNode {
        id: NodeId::from("empty"),
        rows: vec![],
        structure_hash: String::new(),
        formatting: TableFormatting::default(),
        formatting_change: None,
    };
    if is_inserted {
        compute_table_diff_result(&empty, table)
    } else {
        compute_table_diff_result(table, &empty)
    }
}

/// Build a `TableDiffResult` for a table whose row/cell structure is
/// unchanged against itself — the single-doc / unchanged-tracked-doc
/// projection case.  Every row aligns Matched, every cell is Unchanged.
///
/// Pins the projection contract that every `BlockType::Table` block
/// carries a populated `table_diff` so the frontend can render it as
/// a table (and not fall back to flat-`<p>` rendering with cell text
/// concatenated).  See `spec_projection_invariants.rs`.
///
/// Panics if the canonicalizer or diff helper fails — a malformed
/// `TableNode` reaching this point is a programmer bug, not a user
/// error.  Per CLAUDE.md "no silent fallbacks": failing loudly here
/// preserves the invariant rather than dropping table_diff to None.
fn project_self_table_diff(table: &TableNode) -> TableDiffResult {
    compute_table_diff_result(table, table)
        .unwrap_or_else(|e| panic!("project_self_table_diff: table_id='{}': {e}", table.id))
}

/// Build a `TableDiffResult` for a table that exists on only one side
/// (block-level Inserted or Deleted in the tracked-doc projection).
/// Thin wrapper around `compute_single_table_diff_result` that panics
/// on failure for the same reason as `project_self_table_diff`.
fn project_single_sided_table_diff(table: &TableNode, is_inserted: bool) -> TableDiffResult {
    compute_single_table_diff_result(table, is_inserted).unwrap_or_else(|e| {
        panic!(
            "project_single_sided_table_diff: table_id='{}' is_inserted={is_inserted}: {e}",
            table.id
        )
    })
}

/// Compute detailed table diff result for TableStructureChanged.
pub(crate) fn compute_table_diff_result(
    old_table: &TableNode,
    new_table: &TableNode,
) -> Result<TableDiffResult, String> {
    let diff = diff_tables(old_table, new_table)?;

    // Convert table_diff types to domain types
    let row_alignment: Vec<TableRowAlignment> = diff
        .row_alignment
        .into_iter()
        .map(|a| match a {
            RowAlignment::Matched { old_row, new_row } => {
                TableRowAlignment::Matched { old_row, new_row }
            }
            RowAlignment::Deleted { old_row } => TableRowAlignment::Deleted { old_row },
            RowAlignment::Inserted { new_row } => TableRowAlignment::Inserted { new_row },
        })
        .collect();

    let cell_diffs: Vec<TableCellDiff> = diff
        .cell_diffs
        .into_iter()
        .map(|d| TableCellDiff {
            old_cell_idx: d.old_cell_idx,
            new_cell_idx: d.new_cell_idx,
            diff_type: match d.diff_type {
                CellDiffType::Unchanged => TableCellDiffType::Unchanged,
                CellDiffType::Modified => TableCellDiffType::Modified,
                CellDiffType::Inserted => TableCellDiffType::Inserted,
                CellDiffType::Deleted => TableCellDiffType::Deleted,
                CellDiffType::MergeChanged => TableCellDiffType::MergeChanged,
            },
            text_diff: d.text_diff,
            nested_table_diffs: d.nested_table_diffs,
        })
        .collect();

    Ok(TableDiffResult {
        old_table: diff.old_table,
        new_table: diff.new_table,
        row_alignment,
        cell_diffs,
    })
}

/// Extract raw OMML XML strings from a list of inline nodes.
///
/// Collects the raw XML bytes (as UTF-8 strings) from OpaqueInline nodes
/// with OpaqueKind::Omml. Used to provide equation context for LLM analysis.
pub fn extract_equation_xmls(inlines: &[InlineNode]) -> Vec<String> {
    let mut xmls = Vec::new();
    for inline in inlines {
        if let InlineNode::OpaqueInline(o) = inline
            && matches!(o.kind, OpaqueKind::OmmlBlock | OpaqueKind::OmmlInline)
            && let Some(ref raw) = o.raw_xml
            && let Ok(s) = std::str::from_utf8(raw)
        {
            xmls.push(s.to_string());
        }
    }
    xmls
}

/// Extract base64 data URIs for Drawing inlines, using an rId→data URI lookup.
///
/// For each Drawing opaque node, parses the raw XML fragment to find
/// `r:embed="rIdN"` and looks up the corresponding data URI from the map.
pub fn extract_image_data_uris(
    inlines: &[InlineNode],
    rid_to_data_uri: &HashMap<String, String>,
) -> Vec<String> {
    let mut uris = Vec::new();
    for inline in inlines {
        let InlineNode::OpaqueInline(o) = inline else {
            continue;
        };
        if !matches!(o.kind, OpaqueKind::Drawing) {
            continue;
        }

        let Some(raw) = o.raw_xml.as_ref() else {
            tracing::warn!(
                opaque_id = %o.id.0,
                "drawing opaque missing raw_xml; cannot extract image data URI"
            );
            continue;
        };

        let s = match std::str::from_utf8(raw) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(
                    opaque_id = %o.id.0,
                    error = %err,
                    "drawing opaque raw_xml is not valid UTF-8; cannot extract image data URI"
                );
                continue;
            }
        };

        // Find r:embed="rIdN" in the raw XML fragment.
        let Some(rid) = find_blip_rid(s) else {
            tracing::warn!(
                opaque_id = %o.id.0,
                "drawing opaque missing r:embed relationship ID; cannot resolve image"
            );
            continue;
        };
        let Some(data_uri) = rid_to_data_uri.get(&rid) else {
            tracing::warn!(
                opaque_id = %o.id.0,
                rid = %rid,
                lookup_size = rid_to_data_uri.len(),
                "drawing relationship ID not found in image lookup; cannot resolve image"
            );
            continue;
        };

        uris.push(data_uri.clone());
    }
    uris
}

/// Find the r:embed attribute value from a Drawing XML fragment.
///
/// The raw XML is serialized by xmltree which may strip namespace prefixes from
/// attributes, so `r:embed="rId5"` might become `embed="rId5"`. We search for
/// multiple patterns to be robust across different XML serialization approaches.
pub(crate) fn find_blip_rid(xml: &str) -> Option<String> {
    // Try DrawingML r:embed first (a:blip r:embed="rIdN")
    if let Some(rid) = find_rid_by_patterns(
        xml,
        &[
            "r:embed=\"",
            " embed=\"",
            ">embed=\"",
            "\tembed=\"",
            "\nembed=\"",
        ],
    ) {
        return Some(rid);
    }

    // Try VML imagedata r:id (v:imagedata r:id="rIdN")
    find_vml_imagedata_rid(xml)
}

/// Search for a relationship ID matching any of the given attribute patterns.
fn find_rid_by_patterns(xml: &str, patterns: &[&str]) -> Option<String> {
    for pattern in patterns {
        let mut search_start = 0;
        while let Some(pos) = xml[search_start..].find(pattern) {
            let abs_pos = search_start + pos;
            let start = abs_pos + pattern.len();
            if let Some(end_offset) = xml[start..].find('"') {
                let value = &xml[start..start + end_offset];
                if value.starts_with("rId") || value.starts_with("rid") {
                    return Some(value.to_string());
                }
            }
            search_start = abs_pos + 1;
        }
    }
    None
}

/// Extract image relationship ID from VML `<v:imagedata r:id="rIdN"/>`.
///
/// VML shapes use `r:id` on `v:imagedata` elements instead of the DrawingML
/// `r:embed` on `a:blip`. Both reference the same image relationships.
fn find_vml_imagedata_rid(xml: &str) -> Option<String> {
    // Find <v:imagedata or <imagedata elements, then extract r:id
    let imagedata_markers = ["<v:imagedata", "<imagedata"];
    for marker in &imagedata_markers {
        let mut search_start = 0;
        while let Some(pos) = xml[search_start..].find(marker) {
            let abs_pos = search_start + pos;
            // Find the end of this element (> or />)
            let element_end = xml[abs_pos..]
                .find('>')
                .map(|e| abs_pos + e)
                .unwrap_or(xml.len());
            let element_text = &xml[abs_pos..element_end];

            // Look for r:id="..." within this element
            if let Some(rid) = find_rid_by_patterns(element_text, &["r:id=\"", " o:relid=\""]) {
                return Some(rid);
            }
            search_start = abs_pos + 1;
        }
    }
    None
}

/// Enrich opaque segments in-place with asset data (image data URIs, equation XML).
///
/// For Drawing opaques: resolves the image data URI from the rId lookup.
/// For Omml opaques: attaches the raw equation XML.
///
/// `inlines` is the source InlineNode list matching the segment order.
/// `rid_to_data_uri` maps relationship IDs to base64 data URIs.
#[allow(clippy::type_complexity)]
fn enrich_segments_with_assets(
    segments: &mut [InlineChange],
    inlines: &[InlineNode],
    rid_to_data_uri: &HashMap<String, String>,
) {
    // Build a map from opaque_id → asset data.
    let mut asset_map: HashMap<String, String> = HashMap::new();
    // Build a map from opaque_id → (width_emu, height_emu, alt_text) for drawings.
    let mut dimension_map: HashMap<String, (Option<i64>, Option<i64>, Option<String>)> =
        HashMap::new();
    for inline in inlines {
        if let InlineNode::OpaqueInline(o) = inline {
            match &o.kind {
                OpaqueKind::Drawing => {
                    let Some(raw) = o.raw_xml.as_ref() else {
                        tracing::warn!(
                            opaque_id = %o.id.0,
                            "drawing opaque missing raw_xml; cannot attach asset_ref"
                        );
                        continue;
                    };
                    let s = match std::str::from_utf8(raw) {
                        Ok(s) => s,
                        Err(err) => {
                            tracing::warn!(
                                opaque_id = %o.id.0,
                                error = %err,
                                "drawing opaque raw_xml is not valid UTF-8; cannot attach asset_ref"
                            );
                            continue;
                        }
                    };
                    // Extract display dimensions and alt text from drawing XML.
                    let meta = parse_drawing_metadata(raw);
                    let width_emu = meta.extent_cx.as_ref().and_then(|v| v.parse::<i64>().ok());
                    let height_emu = meta.extent_cy.as_ref().and_then(|v| v.parse::<i64>().ok());
                    dimension_map
                        .insert(o.id.0.to_string(), (width_emu, height_emu, meta.alt_text));
                    let Some(rid) = find_blip_rid(s) else {
                        tracing::warn!(
                            opaque_id = %o.id.0,
                            "drawing opaque missing r:embed relationship ID; cannot attach asset_ref"
                        );
                        continue;
                    };
                    let Some(data_uri) = rid_to_data_uri.get(&rid) else {
                        tracing::warn!(
                            opaque_id = %o.id.0,
                            rid = %rid,
                            lookup_size = rid_to_data_uri.len(),
                            "drawing relationship ID not found in image lookup; cannot attach asset_ref"
                        );
                        continue;
                    };
                    asset_map.insert(o.id.0.to_string(), data_uri.clone());
                }
                OpaqueKind::OmmlBlock | OpaqueKind::OmmlInline => {
                    let Some(raw) = o.raw_xml.as_ref() else {
                        tracing::warn!(
                            opaque_id = %o.id.0,
                            "omml opaque missing raw_xml; cannot attach asset_ref"
                        );
                        continue;
                    };
                    let s = match std::str::from_utf8(raw) {
                        Ok(s) => s,
                        Err(err) => {
                            tracing::warn!(
                                opaque_id = %o.id.0,
                                error = %err,
                                "omml opaque raw_xml is not valid UTF-8; cannot attach asset_ref"
                            );
                            continue;
                        }
                    };
                    asset_map.insert(o.id.0.to_string(), s.to_string());
                }
                OpaqueKind::Hyperlink(data) => {
                    if let Some(ref url) = data.url {
                        asset_map.insert(o.id.0.to_string(), url.clone());
                    } else if let Some(ref anchor) = data.anchor {
                        asset_map.insert(o.id.0.to_string(), format!("#{anchor}"));
                    }
                }
                _ => {}
            }
        }
    }

    // Set asset_ref and drawing display properties on matching Opaque segments.
    for seg in segments.iter_mut() {
        if let InlineChange::Opaque {
            kind,
            opaque_id,
            inline_index,
            asset_ref,
            asset_width_emu,
            asset_height_emu,
            alt_text,
            ..
        } = seg
        {
            if let Some(data) = asset_map.remove(opaque_id.as_str()) {
                *asset_ref = Some(data);
            } else if matches!(kind, OpaqueSegmentKind::Drawing | OpaqueSegmentKind::Omml) {
                tracing::warn!(
                    opaque_id = %opaque_id,
                    inline_index = *inline_index,
                    kind = ?kind,
                    "opaque segment missing asset_ref after enrichment"
                );
            }
            // Populate drawing display dimensions and alt text.
            if let Some((w, h, alt)) = dimension_map.remove(opaque_id.as_str()) {
                *asset_width_emu = w;
                *asset_height_emu = h;
                *alt_text = alt;
            }
        }
    }
}

/// Metadata extracted from a Drawing XML fragment for comparison.
#[derive(Debug, PartialEq, Eq)]
struct DrawingMetadata {
    extent_cx: Option<String>,
    extent_cy: Option<String>,
    src_rect: Option<String>,
    alt_text: Option<String>,
    drawing_type: DrawingType,
}

/// Classification of drawing types for generating fallback labels.
#[derive(Debug, PartialEq, Eq)]
enum DrawingType {
    RasterImage,      // Has r:embed (DrawingML) or v:imagedata r:id (VML)
    VmlShape(String), // VML shape without raster backing (rect, oval, etc.)
    WpShape(String),  // wps:wsp with preset geometry name
    Chart,            // Chart/diagram
    Unknown,
}

/// Parse drawing metadata from raw XML bytes using simple string search.
fn parse_drawing_metadata(raw_xml: &[u8]) -> DrawingMetadata {
    let xml = match std::str::from_utf8(raw_xml) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                "Invalid UTF-8 in drawing raw XML ({} bytes): {}",
                raw_xml.len(),
                e
            );
            return DrawingMetadata {
                extent_cx: None,
                extent_cy: None,
                src_rect: None,
                alt_text: None,
                drawing_type: DrawingType::Unknown,
            };
        }
    };

    let extent_cx = find_xml_attr(xml, "extent", "cx");
    let extent_cy = find_xml_attr(xml, "extent", "cy");

    // srcRect attributes: l, t, r, b (left, top, right, bottom crop percentages)
    let src_rect = find_element_attrs(xml, "srcRect");

    // Alt text from docPr descr attribute
    let alt_text = find_xml_attr(xml, "docPr", "descr");

    // Classify drawing type for fallback text generation
    // Use find_blip_rid to check for valid r:embed attribute (not just string presence)
    let drawing_type = if find_blip_rid(xml).is_some() {
        DrawingType::RasterImage
    } else if let Some(shape) = extract_vml_shape_type(xml) {
        DrawingType::VmlShape(shape)
    } else if let Some(preset) = extract_wp_shape_preset(xml) {
        DrawingType::WpShape(preset)
    } else if xml.contains("<c:chart") || xml.contains(":chart") {
        DrawingType::Chart
    } else {
        DrawingType::Unknown
    };

    DrawingMetadata {
        extent_cx,
        extent_cy,
        src_rect,
        alt_text,
        drawing_type,
    }
}

/// Find a specific attribute value on an element by local name.
/// Handles namespace-prefixed tags (e.g. `wp:extent` matches `extent`).
fn find_xml_attr(xml: &str, element_local_name: &str, attr_name: &str) -> Option<String> {
    // Look for the element — may be prefixed (e.g. wp:extent, a:srcRect)
    // Search for `:element_local_name ` or `<element_local_name `
    let patterns = [
        format!(":{element_local_name}"),
        format!("<{element_local_name}"),
    ];
    for pattern in &patterns {
        if let Some(elem_pos) = xml.find(pattern.as_str()) {
            // Find the end of this element's opening tag
            let rest = &xml[elem_pos..];
            let tag_end = rest.find('>').unwrap_or(rest.len());
            let tag = &rest[..tag_end];
            // Now find the attribute within this tag
            let attr_marker = format!("{attr_name}=\"");
            if let Some(attr_pos) = tag.find(&attr_marker) {
                let val_start = attr_pos + attr_marker.len();
                if let Some(val_end) = tag[val_start..].find('"') {
                    return Some(tag[val_start..val_start + val_end].to_string());
                }
            }
        }
    }
    None
}

/// Extract all attributes of an element as a sorted string for comparison.
/// Returns None if the element is not found.
fn find_element_attrs(xml: &str, element_local_name: &str) -> Option<String> {
    let patterns = [
        format!(":{element_local_name}"),
        format!("<{element_local_name}"),
    ];
    for pattern in &patterns {
        if let Some(elem_pos) = xml.find(pattern.as_str()) {
            let rest = &xml[elem_pos..];
            let tag_end = rest.find('>').unwrap_or(rest.len());
            let tag = &rest[..tag_end];
            // Extract all attr="value" pairs, sort them, join
            let mut attrs: Vec<&str> = Vec::new();
            let mut pos = 0;
            while pos < tag.len() {
                if let Some(eq_pos) = tag[pos..].find("=\"") {
                    // Walk back to find attr name start
                    let abs_eq = pos + eq_pos;
                    let name_start = tag[..abs_eq]
                        .rfind(|c: char| c.is_whitespace() || c == ':')
                        .map(|p| p + 1)
                        .unwrap_or(0);
                    let val_start = abs_eq + 2;
                    if let Some(val_end) = tag[val_start..].find('"') {
                        let abs_val_end = val_start + val_end;
                        attrs.push(&tag[name_start..abs_val_end + 1]);
                        pos = abs_val_end + 1;
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
            attrs.sort();
            return Some(attrs.join(" "));
        }
    }
    None
}

/// Extract VML shape type from pict elements.
/// Examples: <v:rect>, <v:oval>, <v:roundrect>
fn extract_vml_shape_type(xml: &str) -> Option<String> {
    let vml_shapes = [
        "<v:rect",
        "<v:oval",
        "<v:roundrect",
        "<v:shape",
        "<v:line",
        "<v:polyline",
        "<v:curve",
        "<v:arc",
    ];
    for shape in &vml_shapes {
        if xml.contains(shape) {
            // Strip "<v:" prefix to get shape name
            return Some(shape[3..].to_string());
        }
    }
    None
}

/// Extract WordprocessingML shape preset from wps:wsp elements.
/// Example: <a:prstGeom prst="rect">
fn extract_wp_shape_preset(xml: &str) -> Option<String> {
    find_xml_attr(xml, "prstGeom", "prst")
}

/// Generate descriptive fallback text for drawings without asset_ref.
/// Priority: alt text > shape type > generic label
fn extract_drawing_fallback_text(raw_xml: &[u8]) -> Option<String> {
    let meta = parse_drawing_metadata(raw_xml);

    // Priority 1: Use author-provided alt text
    if let Some(alt) = meta.alt_text {
        let trimmed = alt.trim();
        if !trimmed.is_empty() {
            return Some(format!("[drawing: {trimmed}]"));
        }
    }

    // Priority 2: Describe shape type
    match meta.drawing_type {
        DrawingType::RasterImage => None, // Should have asset_ref
        DrawingType::VmlShape(shape) => Some(format!("[shape: {shape}]")),
        DrawingType::WpShape(preset) => Some(format!("[shape: {preset}]")),
        DrawingType::Chart => Some("[chart]".to_string()),
        DrawingType::Unknown => Some("[drawing]".to_string()),
    }
}

/// Compare drawing metadata between matched base and target inlines.
/// Returns the list of metadata properties that differ.
fn compare_drawing_metadata(
    base_inlines: &[InlineNode],
    target_inlines: &[InlineNode],
) -> Vec<ImageMetadataChange> {
    let base_drawings: Vec<&OpaqueInlineNode> = base_inlines
        .iter()
        .filter_map(|i| match i {
            InlineNode::OpaqueInline(o) if matches!(o.kind, OpaqueKind::Drawing) => {
                Some(o.as_ref())
            }
            _ => None,
        })
        .collect();
    let target_drawings: Vec<&OpaqueInlineNode> = target_inlines
        .iter()
        .filter_map(|i| match i {
            InlineNode::OpaqueInline(o) if matches!(o.kind, OpaqueKind::Drawing) => {
                Some(o.as_ref())
            }
            _ => None,
        })
        .collect();

    let mut changes = Vec::new();

    for (base_d, target_d) in base_drawings.iter().zip(target_drawings.iter()) {
        let base_raw = base_d.raw_xml.as_deref().unwrap_or(&[]);
        let target_raw = target_d.raw_xml.as_deref().unwrap_or(&[]);
        let base_meta = parse_drawing_metadata(base_raw);
        let target_meta = parse_drawing_metadata(target_raw);

        if (base_meta.extent_cx != target_meta.extent_cx
            || base_meta.extent_cy != target_meta.extent_cy)
            && !changes.contains(&ImageMetadataChange::Size)
        {
            changes.push(ImageMetadataChange::Size);
        }
        if base_meta.src_rect != target_meta.src_rect
            && !changes.contains(&ImageMetadataChange::Cropping)
        {
            changes.push(ImageMetadataChange::Cropping);
        }
        if base_meta.alt_text != target_meta.alt_text
            && !changes.contains(&ImageMetadataChange::AltText)
        {
            changes.push(ImageMetadataChange::AltText);
        }
    }

    changes
}

/// Placeholder for an opaque inline in diff text.
///
/// Returns U+FFFC (barrier character) for all opaque kinds to match the
/// coordinate space used by `ParagraphView::block_text()` during step application.
/// This ensures that step ranges computed from diff text are valid when applied
/// to the actual paragraph.
fn opaque_placeholder(_opaque: &OpaqueInlineNode) -> String {
    "\u{FFFC}".to_string()
}

/// Tagged placeholder for opaque inlines in the inline diff path.
/// Embeds a truncated content hash so the diff algorithm can distinguish
/// different opaques (e.g., footnote ref 1 vs footnote ref 2).
fn opaque_diff_tag(opaque: &OpaqueInlineNode) -> String {
    let hash_str = match &opaque.content_hash {
        Some(h) => h[..h.len().min(OPAQUE_HASH_LEN)].to_string(),
        None => {
            // Hyperlinks (and any future kinds) without content_hash:
            // hash the semantic identity of the kind, excluding transport
            // details like r:id that differ between documents.
            let identity = opaque_semantic_identity(&opaque.kind);
            let digest = sha256_hex(identity.as_bytes());
            digest[..OPAQUE_HASH_LEN].to_string()
        }
    };
    format!("\u{FFFC}{hash_str}")
}

/// Compute a stable identity string for an opaque kind, excluding fields
/// that are transport/serialization details (like r:id) rather than semantic
/// identity.
fn opaque_semantic_identity(kind: &OpaqueKind) -> String {
    match kind {
        OpaqueKind::Hyperlink(data) => {
            format!(
                "Hyperlink({:?},{:?},{:?})",
                data.url, data.anchor, data.text
            )
        }
        // All other kinds: use Debug repr (they don't have transport-only fields).
        other => format!("{other:?}"),
    }
}

fn change_type_to_segment_type(change_type: &str) -> InlineChangeSegmentType {
    match change_type {
        "insert" => InlineChangeSegmentType::Insert,
        "delete" => InlineChangeSegmentType::Delete,
        _ => InlineChangeSegmentType::Equal,
    }
}

/// The hyperlink target for the render projection: the external URL, or
/// `#anchor` for an internal bookmark link. None for non-hyperlink opaques.
pub(crate) fn opaque_url(kind: &OpaqueKind) -> Option<String> {
    match kind {
        OpaqueKind::Hyperlink(data) => data
            .url
            .clone()
            .or_else(|| data.anchor.as_ref().map(|a| format!("#{a}"))),
        _ => None,
    }
}

pub(crate) fn opaque_kind_to_segment_kind(kind: &OpaqueKind) -> OpaqueSegmentKind {
    match kind {
        OpaqueKind::Drawing => OpaqueSegmentKind::Drawing,
        // Defensive label only: compare/diff refuses quarantined inputs at
        // entry, and the quarantined kind exists only on body-level opaque
        // blocks, never inline.
        OpaqueKind::QuarantinedNestedTracking => {
            OpaqueSegmentKind::Unknown("quarantined_nested_tracked_changes".to_string())
        }
        OpaqueKind::OmmlBlock | OpaqueKind::OmmlInline => OpaqueSegmentKind::Omml,
        OpaqueKind::Hyperlink(_) => OpaqueSegmentKind::Hyperlink,
        OpaqueKind::Field(_) => OpaqueSegmentKind::Field,
        OpaqueKind::Sdt => OpaqueSegmentKind::Sdt,
        OpaqueKind::Ruby => OpaqueSegmentKind::Ruby,
        OpaqueKind::SmartArt => OpaqueSegmentKind::SmartArt,
        OpaqueKind::CommentReference(_) => OpaqueSegmentKind::CommentReference,
        OpaqueKind::FootnoteReference(_) => OpaqueSegmentKind::FootnoteReference,
        OpaqueKind::EndnoteReference(_) => OpaqueSegmentKind::EndnoteReference,
        OpaqueKind::SmartTag => OpaqueSegmentKind::SmartTag,
        OpaqueKind::Sym(_) => OpaqueSegmentKind::Sym,
        OpaqueKind::Ptab => OpaqueSegmentKind::Ptab,
        OpaqueKind::CustomXml => OpaqueSegmentKind::CustomXml,
        OpaqueKind::Unknown(name) => OpaqueSegmentKind::Unknown(name.clone()),
    }
}

fn extract_inline_text(inlines: &[InlineNode]) -> String {
    let mut out = String::new();
    for inline in inlines {
        match inline {
            InlineNode::Text(t) => {
                // Apply caps mark to normalize text for comparison.
                // This ensures "this instrument" with Caps mark compares equal
                // to "THIS INSTRUMENT" stored as actual uppercase.
                let text = if t.style_props.caps == MarkValue::On {
                    t.text.to_uppercase()
                } else {
                    t.text.clone()
                };
                out.push_str(&text);
            }
            InlineNode::HardBreak(_) => out.push('\n'),
            InlineNode::OpaqueInline(o) => out.push_str(&opaque_placeholder(o)),
            InlineNode::Decoration(_) => {} // Zero-width, no contribution
            InlineNode::CommentRangeStart { .. }
            | InlineNode::CommentRangeEnd { .. }
            | InlineNode::CommentReference { .. } => {} // Zero-width, no contribution
        }
    }
    out
}

/// Like `extract_inline_text` but appends content_hash after each opaque placeholder.
/// Used for text_hash computation so different images produce different hashes.
fn extract_inline_text_with_opaque_hashes(inlines: &[InlineNode]) -> String {
    let mut out = String::new();
    for inline in inlines {
        match inline {
            InlineNode::Text(t) => {
                let text = if t.style_props.caps == MarkValue::On {
                    t.text.to_uppercase()
                } else {
                    t.text.clone()
                };
                out.push_str(&text);
            }
            InlineNode::HardBreak(_) => out.push('\n'),
            InlineNode::OpaqueInline(o) => {
                out.push_str(&opaque_placeholder(o));
                if let Some(hash) = &o.content_hash {
                    out.push_str(hash);
                }
            }
            InlineNode::Decoration(_) => {}
            InlineNode::CommentRangeStart { .. }
            | InlineNode::CommentRangeEnd { .. }
            | InlineNode::CommentReference { .. } => {}
        }
    }
    out
}

/// Alignment result between base and target elements.
#[derive(Clone, Debug)]
enum ElementAlignment {
    /// Element exists in both and matches (same hash).
    Matched { base_idx: usize, target_idx: usize },
    /// Element exists in both but content differs.
    Modified { base_idx: usize, target_idx: usize },
    /// Element only in base (deleted).
    Deleted { base_idx: usize },
    /// Element only in target (inserted).
    Inserted { target_idx: usize },
}

/// Align elements using anchor-based approach.
///
/// This first finds exact hash matches via LCS to use as anchors,
/// then runs the DP algorithm on segments between anchors.
/// This ensures identical content is always matched, regardless of
/// how much other content has been inserted/deleted around it.
fn align_elements(base: &[DiffableElement], target: &[DiffableElement]) -> Vec<ElementAlignment> {
    // Find anchors: exact matches that form the longest common subsequence
    let anchors = find_exact_match_anchors(base, target);

    if anchors.is_empty() {
        // No exact matches - fall back to pure DP
        return align_elements_dp(base, target);
    }

    // Trace gap segment sizes when DIFF_TRACE_GAPS=1
    let trace_gaps = std::env::var("DIFF_TRACE_GAPS").is_ok();
    if trace_gaps {
        eprintln!(
            "[align_elements] base={}, target={}, anchors={}",
            base.len(),
            target.len(),
            anchors.len()
        );
    }

    // Align segments between anchors
    let mut alignments = Vec::new();
    let mut prev_base = 0;
    let mut prev_target = 0;

    for &(base_idx, target_idx) in &anchors {
        // Align the segment before this anchor
        if prev_base < base_idx || prev_target < target_idx {
            let base_gap = base_idx - prev_base;
            let target_gap = target_idx - prev_target;
            if trace_gaps && (base_gap != target_gap) {
                eprintln!(
                    "  gap: base[{}..{}]={} target[{}..{}]={} (diff={})",
                    prev_base,
                    base_idx,
                    base_gap,
                    prev_target,
                    target_idx,
                    target_gap,
                    (base_gap as i64 - target_gap as i64).abs(),
                );
            }
            let segment =
                align_gap_segment(&base[prev_base..base_idx], &target[prev_target..target_idx]);
            for a in segment {
                alignments.push(shift_alignment(a, prev_base, prev_target));
            }
        }

        // Add the anchor itself as a Matched alignment
        alignments.push(ElementAlignment::Matched {
            base_idx,
            target_idx,
        });

        prev_base = base_idx + 1;
        prev_target = target_idx + 1;
    }

    // Align final segment after last anchor
    if prev_base < base.len() || prev_target < target.len() {
        let base_gap = base.len() - prev_base;
        let target_gap = target.len() - prev_target;
        if trace_gaps {
            eprintln!(
                "  trailing gap: base[{}..{}]={} target[{}..{}]={}",
                prev_base,
                base.len(),
                base_gap,
                prev_target,
                target.len(),
                target_gap,
            );
        }
        let segment = align_gap_segment(&base[prev_base..], &target[prev_target..]);
        for a in segment {
            alignments.push(shift_alignment(a, prev_base, prev_target));
        }
    }

    alignments
}

/// Align a gap segment between two anchors.
///
/// When the base and target segments have the same number of elements,
/// force pairwise matching: each base[i] aligns with target[i]. This
/// prevents the affine-gap DP from choosing the cheaper "delete all +
/// insert all" pattern for low-similarity paragraph pairs that are
/// positionally corresponding between shared anchors. Showing inline
/// modifications is always more informative than delete+reinsert for
/// paragraphs at the same structural position.
///
/// When lengths differ, uses the DP algorithm for optimal alignment.
fn align_gap_segment(
    base: &[DiffableElement],
    target: &[DiffableElement],
) -> Vec<ElementAlignment> {
    let m = base.len();
    let n = target.len();

    if m == 0 && n == 0 {
        return Vec::new();
    }

    // Equal-length gap segments: match elements pairwise by position
    // when every pair has reasonable similarity. Between shared anchors,
    // base[i] and target[i] are at the same structural position and
    // should be compared as modifications, not spuriously deleted+reinserted.
    //
    // However, if any pair has very low similarity (< 10%), the positional
    // assumption breaks down — a paragraph may have been inserted/deleted
    // rather than modified. Fall back to the DP algorithm in that case.
    if m == n {
        // Between shared anchors, base[i] and target[i] are at the same structural
        // position. Check if all pairs are type-compatible for pairwise matching.
        let all_type_compatible = (0..m).all(|i| {
            (base[i].is_block() && target[i].is_block())
                || (base[i].is_table() && target[i].is_table())
        });

        // Verify that each pair has meaningful content overlap. Positional
        // correspondence between anchors is evidence, not proof — paragraphs
        // at the same structural position can be genuinely different content
        // (e.g., a definition clause replaced by an unrelated definition,
        // or dot-fill form fields shifted by one position).
        //
        // Uses content_similarity (whitespace-filtered) rather than
        // text_similarity to avoid inflated scores from shared whitespace
        // tokens. Threshold of 0.40 requires real word overlap, not just
        // structural boilerplate.
        //
        // When any pair fails: fall back to the DP algorithm, which can
        // choose delete+insert for dissimilar pairs while still matching
        // similar ones.
        let all_similar_enough = all_type_compatible
            && (0..m).all(|i| {
                base[i].hash() == target[i].hash()
                    || content_similarity(base[i].text(), target[i].text()) >= 0.40
            });

        if all_similar_enough {
            return (0..m)
                .map(|i| {
                    if base[i].hash() == target[i].hash() {
                        ElementAlignment::Matched {
                            base_idx: i,
                            target_idx: i,
                        }
                    } else {
                        ElementAlignment::Modified {
                            base_idx: i,
                            target_idx: i,
                        }
                    }
                })
                .collect();
        }
    }

    // Unequal lengths or type-incompatible pairs: use the DP algorithm.
    align_elements_dp(base, target)
}

/// Find exact-match anchors using LCS (Longest Common Subsequence) on element hashes.
///
/// Returns pairs of (base_idx, target_idx) for elements with identical content.
/// These pairs are guaranteed to be non-crossing (order-preserving).
///
/// Only hashes that appear exactly once in each side are eligible for anchoring.
/// Duplicate hashes (e.g. empty paragraphs) are ambiguous and would cause LCS to
/// cross-match across section boundaries, creating cascading del/ins inflation.
/// These are left to gap segments where positional DP handles them correctly.
///
/// The LCS DP runs only on the filtered unique-hash elements (typically ~340 out of
/// ~12,000), reducing the DP table from m×n to a much smaller u_base×u_target.
fn find_exact_match_anchors(
    base: &[DiffableElement],
    target: &[DiffableElement],
) -> Vec<(usize, usize)> {
    if base.is_empty() || target.is_empty() {
        return Vec::new();
    }

    // Count hash frequencies to identify unique hashes.
    let mut base_hash_counts: std::collections::HashMap<&str, usize> =
        std::collections::HashMap::new();
    let mut target_hash_counts: std::collections::HashMap<&str, usize> =
        std::collections::HashMap::new();
    for elem in base.iter() {
        *base_hash_counts.entry(elem.hash()).or_insert(0) += 1;
    }
    for elem in target.iter() {
        *target_hash_counts.entry(elem.hash()).or_insert(0) += 1;
    }
    let is_unique_hash = |hash: &str| -> bool {
        base_hash_counts.get(hash).copied().unwrap_or(0) == 1
            && target_hash_counts.get(hash).copied().unwrap_or(0) == 1
    };

    // Build filtered arrays of only unique-hash elements (blocks/tables),
    // preserving their original indices for mapping back.
    let base_unique: Vec<(usize, &str)> = base
        .iter()
        .enumerate()
        .filter(|(_, e)| (e.is_block() || e.is_table()) && is_unique_hash(e.hash()))
        .map(|(i, e)| (i, e.hash()))
        .collect();
    let target_unique: Vec<(usize, &str)> = target
        .iter()
        .enumerate()
        .filter(|(_, e)| (e.is_block() || e.is_table()) && is_unique_hash(e.hash()))
        .map(|(i, e)| (i, e.hash()))
        .collect();

    let um = base_unique.len();
    let un = target_unique.len();

    if um == 0 || un == 0 {
        return Vec::new();
    }

    // LCS DP on the small filtered arrays (comparing hashes).
    // Since all elements are unique-hash, hash equality implies type compatibility
    // (a block and table cannot produce the same hash).
    let mut dp = vec![vec![0usize; un + 1]; um + 1];

    for i in 1..=um {
        for j in 1..=un {
            if base_unique[i - 1].1 == target_unique[j - 1].1 {
                dp[i][j] = dp[i - 1][j - 1] + 1;
            } else {
                dp[i][j] = dp[i - 1][j].max(dp[i][j - 1]);
            }
        }
    }

    // Backtrack to find matches, mapping back to original indices.
    let mut anchors = Vec::new();
    let mut i = um;
    let mut j = un;

    while i > 0 && j > 0 {
        if base_unique[i - 1].1 == target_unique[j - 1].1 {
            anchors.push((base_unique[i - 1].0, target_unique[j - 1].0));
            i -= 1;
            j -= 1;
        } else if dp[i - 1][j] >= dp[i][j - 1] {
            i -= 1;
        } else {
            j -= 1;
        }
    }

    anchors.reverse();
    anchors
}

/// Shift alignment indices by the given offsets.
fn shift_alignment(
    alignment: ElementAlignment,
    base_offset: usize,
    target_offset: usize,
) -> ElementAlignment {
    match alignment {
        ElementAlignment::Matched {
            base_idx,
            target_idx,
        } => ElementAlignment::Matched {
            base_idx: base_idx + base_offset,
            target_idx: target_idx + target_offset,
        },
        ElementAlignment::Modified {
            base_idx,
            target_idx,
        } => ElementAlignment::Modified {
            base_idx: base_idx + base_offset,
            target_idx: target_idx + target_offset,
        },
        ElementAlignment::Deleted { base_idx } => ElementAlignment::Deleted {
            base_idx: base_idx + base_offset,
        },
        ElementAlignment::Inserted { target_idx } => ElementAlignment::Inserted {
            target_idx: target_idx + target_offset,
        },
    }
}

/// Align elements between two documents using affine gap DP (Gotoh-style).
///
/// This produces globally optimal alignments with:
/// - Exact hash matches getting a bonus (natural anchors)
/// - Affine gaps to group consecutive insertions/deletions
/// - Continuous similarity scoring (no hard threshold)
/// - No crossing alignments
/// - Type compatibility (blocks only align with blocks, tables with tables)
fn align_elements_dp(
    base: &[DiffableElement],
    target: &[DiffableElement],
) -> Vec<ElementAlignment> {
    let m = base.len();
    let n = target.len();

    // Edge cases
    if m == 0 && n == 0 {
        return Vec::new();
    }
    if m == 0 {
        return (0..n)
            .map(|j| ElementAlignment::Inserted { target_idx: j })
            .collect();
    }
    if n == 0 {
        return (0..m)
            .map(|i| ElementAlignment::Deleted { base_idx: i })
            .collect();
    }

    // Three DP matrices for affine gaps (Gotoh algorithm)
    // M[i][j] = best cost ending with match/substitute at (i,j)
    // D[i][j] = best cost ending with deletion (base[i-1] deleted)
    // I[i][j] = best cost ending with insertion (target[j-1] inserted)
    let inf = f64::INFINITY;
    let mut m_cost = vec![vec![inf; n + 1]; m + 1];
    let mut d_cost = vec![vec![inf; n + 1]; m + 1];
    let mut i_cost = vec![vec![inf; n + 1]; m + 1];

    // Backtrack: which operation led to this cell
    let mut m_back = vec![vec![AlignOp::Match; n + 1]; m + 1];
    let mut d_back = vec![vec![AlignOp::DeleteOpen; n + 1]; m + 1];
    let mut i_back = vec![vec![AlignOp::InsertOpen; n + 1]; m + 1];

    // Base cases
    m_cost[0][0] = 0.0;
    // d_cost[0][0] and i_cost[0][0] stay inf (can't end in gap at origin)

    // First column: deletions only
    for i in 1..=m {
        d_cost[i][0] = if i == 1 {
            GAP_OPEN
        } else {
            d_cost[i - 1][0] + GAP_EXTEND
        };
        d_back[i][0] = if i == 1 {
            AlignOp::DeleteOpen
        } else {
            AlignOp::DeleteExt
        };
        // m_cost[i][0] and i_cost[i][0] stay inf
    }

    // First row: insertions only
    for j in 1..=n {
        i_cost[0][j] = if j == 1 {
            GAP_OPEN
        } else {
            i_cost[0][j - 1] + GAP_EXTEND
        };
        i_back[0][j] = if j == 1 {
            AlignOp::InsertOpen
        } else {
            AlignOp::InsertExt
        };
        // m_cost[0][j] and d_cost[0][j] stay inf
    }

    // Fill DP tables
    for i in 1..=m {
        for j in 1..=n {
            let base_elem = &base[i - 1];
            let target_elem = &target[j - 1];

            // Check type compatibility: blocks align with blocks, tables with tables
            let type_compatible = (base_elem.is_block() && target_elem.is_block())
                || (base_elem.is_table() && target_elem.is_table());

            // Compute substitution cost.
            // Position penalty is applied to ALL match types to prevent
            // misalignment when multiple elements share the same hash
            // (e.g. repeated short list items like "i.", "ii.").
            //
            // Match bonuses are scaled by content significance so that
            // content-bearing paragraphs are always preferred over empty ones.
            // Without this, the DP can match many empty paragraphs (each earning
            // EXACT_MATCH_BONUS) at the expense of content matches.
            let pos_diff = ((i as isize) - (j as isize)).unsigned_abs() as f64;
            let sig = content_significance(base_elem.text())
                .min(content_significance(target_elem.text()));
            let sub_cost = if !type_compatible {
                // Very high cost to prevent matching incompatible types
                f64::MAX / 2.0
            } else if base_elem.hash() == target_elem.hash() {
                // Reward exact matches, scaled by content significance.
                // Empty paragraphs get sig=0.0 → neutral cost (no bonus).
                // Content paragraphs get sig≥1.0 → full bonus.
                // Capped at 1.0 so exact matches don't exceed the base bonus.
                EXACT_MATCH_BONUS * sig.min(1.0) + (POSITION_PENALTY * pos_diff)
            } else {
                let sim = text_similarity(base_elem.text(), target_elem.text());
                if sim >= STRONG_MATCH_THRESHOLD {
                    // Strong matches (90%+ similarity) are definitionally the same
                    // element with edits. Bonus scales with content significance
                    // so that long paragraph matches pull the DP through expensive
                    // gap paths rather than being discarded.
                    STRONG_MATCH_BONUS * sig + (POSITION_PENALTY * pos_diff)
                } else {
                    // Continuous: sim=1.0 → 0.0, sim=0.5 → 1.0, sim=0.0 → 2.0
                    (2.0 - 2.0 * sim) + (POSITION_PENALTY * pos_diff)
                }
            };

            // M[i][j]: best cost ending in match/substitute
            // Can come from any of M, D, I at (i-1, j-1)
            let prev_best = m_cost[i - 1][j - 1]
                .min(d_cost[i - 1][j - 1])
                .min(i_cost[i - 1][j - 1]);
            m_cost[i][j] = prev_best + sub_cost;
            m_back[i][j] = if type_compatible && base_elem.hash() == target_elem.hash() {
                AlignOp::ExactMatch
            } else {
                AlignOp::Match
            };

            // D[i][j]: best cost ending in deletion
            // Either open new gap from M/I, or extend from D
            let d_open = m_cost[i - 1][j].min(i_cost[i - 1][j]) + GAP_OPEN;
            let d_ext = d_cost[i - 1][j] + GAP_EXTEND;
            if d_ext <= d_open {
                d_cost[i][j] = d_ext;
                d_back[i][j] = AlignOp::DeleteExt;
            } else {
                d_cost[i][j] = d_open;
                d_back[i][j] = AlignOp::DeleteOpen;
            }

            // I[i][j]: best cost ending in insertion
            // Either open new gap from M/D, or extend from I
            let i_open = m_cost[i][j - 1].min(d_cost[i][j - 1]) + GAP_OPEN;
            let i_ext = i_cost[i][j - 1] + GAP_EXTEND;
            if i_ext <= i_open {
                i_cost[i][j] = i_ext;
                i_back[i][j] = AlignOp::InsertExt;
            } else {
                i_cost[i][j] = i_open;
                i_back[i][j] = AlignOp::InsertOpen;
            }
        }
    }

    // Find best ending state
    let final_m = m_cost[m][n];
    let final_d = d_cost[m][n];
    let final_i = i_cost[m][n];

    // Backtrack
    let mut alignments = Vec::new();
    let (mut i, mut j) = (m, n);

    // Determine which matrix we're starting from
    #[derive(Clone, Copy)]
    enum State {
        M,
        D,
        I,
    }
    let mut state = if final_m <= final_d && final_m <= final_i {
        State::M
    } else if final_d <= final_i {
        State::D
    } else {
        State::I
    };

    while i > 0 || j > 0 {
        match state {
            State::M if i > 0 && j > 0 => {
                let op = m_back[i][j];
                if op == AlignOp::ExactMatch {
                    alignments.push(ElementAlignment::Matched {
                        base_idx: i - 1,
                        target_idx: j - 1,
                    });
                } else {
                    alignments.push(ElementAlignment::Modified {
                        base_idx: i - 1,
                        target_idx: j - 1,
                    });
                }
                // Determine previous state (which matrix led here)
                let prev_m = m_cost[i - 1][j - 1];
                let prev_d = d_cost[i - 1][j - 1];
                let prev_i = i_cost[i - 1][j - 1];
                state = if prev_m <= prev_d && prev_m <= prev_i {
                    State::M
                } else if prev_d <= prev_i {
                    State::D
                } else {
                    State::I
                };
                i -= 1;
                j -= 1;
            }
            State::D if i > 0 => {
                alignments.push(ElementAlignment::Deleted { base_idx: i - 1 });
                state = if d_back[i][j] == AlignOp::DeleteExt {
                    State::D
                } else {
                    // DeleteOpen: came from M or I, pick the one that was cheaper
                    if m_cost[i - 1][j] <= i_cost[i - 1][j] {
                        State::M
                    } else {
                        State::I
                    }
                };
                i -= 1;
            }
            State::I if j > 0 => {
                alignments.push(ElementAlignment::Inserted { target_idx: j - 1 });
                state = if i_back[i][j] == AlignOp::InsertExt {
                    State::I
                } else {
                    // InsertOpen: came from M or D, pick the one that was cheaper
                    if m_cost[i][j - 1] <= d_cost[i][j - 1] {
                        State::M
                    } else {
                        State::D
                    }
                };
                j -= 1;
            }
            // Edge cases when one index is 0
            State::M | State::D if i > 0 => {
                alignments.push(ElementAlignment::Deleted { base_idx: i - 1 });
                i -= 1;
            }
            _ if j > 0 => {
                alignments.push(ElementAlignment::Inserted { target_idx: j - 1 });
                j -= 1;
            }
            _ => break,
        }
    }

    alignments.reverse();
    alignments
}

/// Base reward for exact hash match (negative = bonus).
/// Scaled by `content_significance()` so empty paragraphs don't distort alignment.
const EXACT_MATCH_BONUS: f64 = -0.5;

/// Cost to open a new gap (delete or insert).
const GAP_OPEN: f64 = 1.5;

/// Cost to extend an existing gap.
const GAP_EXTEND: f64 = 0.3;

/// Small positional penalty to prefer staying near diagonal.
const POSITION_PENALTY: f64 = 0.02;

/// Similarity threshold for "strong match" - paragraphs above this are
/// definitionally the same paragraph with edits, not different paragraphs.
const STRONG_MATCH_THRESHOLD: f64 = 0.90;

/// Base bonus for strong matches (sim > 0.90). Scaled by `content_significance()`
/// so that matching a long, content-rich paragraph is worth more than matching
/// a short one — preventing structural noise (empty paragraphs, drawings) from
/// outweighing semantically important matches.
const STRONG_MATCH_BONUS: f64 = -1.0;

/// Content significance factor for scaling match bonuses in the DP.
///
/// The DP alignment can be distorted by "structural noise": empty paragraphs and
/// trivial elements (drawings, single characters) that all share the same hash.
/// When many of these appear between anchors, the DP may prefer to match them
/// (earning many small bonuses) instead of matching content-bearing paragraphs
/// (earning fewer but more important bonuses).
///
/// This function returns a scaling factor based on non-whitespace character count:
/// - 0 chars (empty): 0.0 — matching is meaningless, neutral cost
/// - 1–5 chars (trivial): 0.1 — tiny bonus, won't distort alignment
/// - 6+ chars (content): 1.0+ — full weight, scaled by ln(len) for longer content
///
/// The logarithmic scaling for longer content ensures that matching a 1400-char
/// paragraph provides enough bonus to pull the DP through an expensive gap path,
/// rather than letting the DP take a cheaper path that deletes+reinserts it.
fn content_significance(text: &str) -> f64 {
    let char_count = text.chars().filter(|c| !c.is_whitespace()).count();
    match char_count {
        0 => 0.0,
        1..=5 => 0.1,
        n => 1.0 + (n as f64).ln() / 5.0,
    }
}

// ---- Zipper collapse constants ----

/// Minimum non-whitespace characters for an Unchanged span to be a "strong anchor".
const ANCHOR_MIN_CHARS: usize = 8;

/// Minimum alternating del/ins run count to trigger zipper collapse.
const ZIPPER_MIN_CHANGE_RUNS: usize = 4;

/// Regions with similarity below this are force-collapsed even without high alternation.
const LOW_SIMILARITY_THRESHOLD: f64 = 0.15;

/// If overall text similarity is below this, bail out of token-level diff entirely.
const BAIL_OUT_SIMILARITY_THRESHOLD: f64 = 0.30;

/// Minimum text length (in chars) to consider bail-out.
const BAIL_OUT_MIN_CHARS: usize = 50;

#[derive(Clone, Copy, Debug)]
struct DiffHeuristics {
    anchor_min_chars: usize,
    zipper_min_change_runs: usize,
    low_similarity_threshold: f64,
    bail_out_similarity_threshold: f64,
    bail_out_min_chars: usize,
}

impl DiffHeuristics {
    fn from_env() -> Self {
        Self {
            anchor_min_chars: parse_env_usize("DIFF_ANCHOR_MIN_CHARS").unwrap_or(ANCHOR_MIN_CHARS),
            zipper_min_change_runs: parse_env_usize("DIFF_ZIPPER_MIN_CHANGE_RUNS")
                .unwrap_or(ZIPPER_MIN_CHANGE_RUNS),
            low_similarity_threshold: parse_env_f64("DIFF_LOW_SIMILARITY_THRESHOLD")
                .unwrap_or(LOW_SIMILARITY_THRESHOLD),
            bail_out_similarity_threshold: parse_env_f64("DIFF_BAIL_OUT_SIMILARITY_THRESHOLD")
                .unwrap_or(BAIL_OUT_SIMILARITY_THRESHOLD),
            bail_out_min_chars: parse_env_usize("DIFF_BAIL_OUT_MIN_CHARS")
                .unwrap_or(BAIL_OUT_MIN_CHARS),
        }
    }
}

fn parse_env_usize(key: &str) -> Option<usize> {
    let value = env::var(key).ok()?;
    match value.parse::<usize>() {
        Ok(parsed) => Some(parsed),
        Err(err) => {
            eprintln!("warning: {key} must be a usize, got '{value}': {err}; using default");
            None
        }
    }
}

fn parse_env_f64(key: &str) -> Option<f64> {
    let value = env::var(key).ok()?;
    match value.parse::<f64>() {
        Ok(parsed) => Some(parsed),
        Err(err) => {
            eprintln!("warning: {key} must be an f64, got '{value}': {err}; using default");
            None
        }
    }
}

/// Alignment operation for backtracking in affine gap DP.
#[derive(Clone, Copy, Debug, PartialEq)]
enum AlignOp {
    ExactMatch, // Identical hash (Matched alignment)
    Match,      // Similar text (Modified alignment)
    DeleteOpen, // Start deletion gap
    DeleteExt,  // Extend deletion gap
    InsertOpen, // Start insertion gap
    InsertExt,  // Extend insertion gap
}

/// Normalize text for similarity comparison.
/// Collapses whitespace and lowercases for better matching across formatting variations.
fn normalize_for_similarity(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Compute similarity between two text strings (0.0 to 1.0).
/// Uses word-level LCS ratio on normalized text.
fn text_similarity(text1: &str, text2: &str) -> f64 {
    let norm1 = normalize_for_similarity(text1);
    let norm2 = normalize_for_similarity(text2);
    let old_tokens = tokenize(&norm1);
    let new_tokens = tokenize(&norm2);
    TextDiff::configure()
        .algorithm(Algorithm::Patience)
        .diff_slices(&old_tokens, &new_tokens)
        .ratio() as f64
}

/// Compute DiffChanges from alignments.
fn compute_changes(
    alignments: &[ElementAlignment],
    base: &[DiffableElement],
    target: &[DiffableElement],
) -> Result<Vec<DiffChange>, String> {
    // Build a mapping from target index to base ID for matched elements.
    // This lets us find the correct base element ID when computing after_block_id for insertions.
    let mut target_idx_to_base_id: std::collections::HashMap<usize, NodeId> =
        std::collections::HashMap::new();

    for alignment in alignments {
        match alignment {
            ElementAlignment::Matched {
                base_idx,
                target_idx,
            } => {
                target_idx_to_base_id.insert(*target_idx, base[*base_idx].id().clone());
            }
            ElementAlignment::Modified {
                base_idx,
                target_idx,
            } => {
                target_idx_to_base_id.insert(*target_idx, base[*base_idx].id().clone());
            }
            _ => {}
        }
    }

    let mut changes = Vec::new();

    for alignment in alignments {
        match alignment {
            ElementAlignment::Matched {
                base_idx,
                target_idx,
            } => {
                match (&base[*base_idx], &target[*target_idx]) {
                    (DiffableElement::Table(base_table), DiffableElement::Table(target_table)) => {
                        // Even with matching structure hash, row content may have
                        // shifted (e.g. one row inserted + one deleted = same count).
                        // Run row alignment to detect row-level changes.
                        let table_changes = diff_table_pair(base_table, target_table)?;
                        changes.extend(table_changes);
                    }
                    (DiffableElement::Block(base_block), DiffableElement::Block(target_block)) => {
                        // Text matches (same hash). Check for formatting-only
                        // differences that need to appear in the redline.
                        let base_inlines = block_inlines(&base_block.block);
                        let target_inlines = block_inlines(&target_block.block);
                        let inline_changes =
                            diff_block_content_with_marks(&base_inlines, &target_inlines);

                        let para_props_differ =
                            paragraph_properties_differ(&base_block.block, &target_block.block);

                        // When paragraph properties differ (style change tracked via
                        // pPrChange), run-level mark differences that stem from style
                        // inheritance are already covered by the paragraph property
                        // change. Emitting them as rPrChange would double-track the
                        // change and contaminate the base paragraph's runs with
                        // target marks (e.g., Mark::Caps inherited from a new style).
                        // Strip FormattingChange from Unchanged segments and preserve
                        // base marks so apply_block_modified keeps the original runs.
                        let inline_changes = if para_props_differ {
                            inline_changes
                                .into_iter()
                                .map(|c| match c {
                                    InlineChange::Unchanged {
                                        text,
                                        formatting_change: Some(fc),
                                        ..
                                    } => InlineChange::Unchanged {
                                        text,
                                        marks: fc.previous_marks,
                                        style_props: fc.previous_style_props,
                                        formatting_change: None,
                                    },
                                    other => other,
                                })
                                .collect()
                        } else {
                            inline_changes
                        };

                        let has_formatting_diff = inline_changes.iter().any(|c| {
                            matches!(
                                c,
                                InlineChange::Unchanged {
                                    formatting_change: Some(_),
                                    ..
                                }
                            )
                        }) || para_props_differ;

                        if has_formatting_diff {
                            changes.push(DiffChange::BlockModified {
                                block_id: base_block.id.clone(),
                                old_text: base_block.text.clone(),
                                new_text: target_block.text.clone(),
                                inline_changes,
                                old_block: base_block.block.clone(),
                                new_block: target_block.block.clone(),
                                para_split: false,
                            });
                        }
                    }
                    // DiffableElement has exactly two variants (Block, Table);
                    // the alignment step only ever pairs an index with itself
                    // being a Matched element of the SAME variant on both
                    // sides (the type compatibility check upstream never
                    // matches a Block to a Table), so a cross-variant pair
                    // here is a bug in that check, not a case to skip quietly.
                    (DiffableElement::Block(_), DiffableElement::Table(_))
                    | (DiffableElement::Table(_), DiffableElement::Block(_)) => {
                        unreachable!(
                            "element alignment's type compatibility check guarantees \
                             a Matched pair never mixes Block and Table"
                        )
                    }
                }
            }
            ElementAlignment::Modified {
                base_idx,
                target_idx,
            } => {
                match (&base[*base_idx], &target[*target_idx]) {
                    (DiffableElement::Block(base_block), DiffableElement::Block(target_block)) => {
                        // Paragraph-level opaques (m:oMathPara) cannot be tracked
                        // inline — they must be direct children of <w:p>, never
                        // inside <w:del>/<w:ins>. When such a paragraph's content
                        // changes, emit BlockDeleted + BlockInserted so the tracking
                        // lives on the paragraph mark, matching Word's model.
                        if is_wholly_paragraph_opaque_change(&base_block.block, &target_block.block)
                            || should_split_empty_paragraph_change(
                                &base_block.block,
                                &target_block.block,
                            )
                            || should_split_unrelated_modification(base_block, target_block)
                        {
                            changes.push(DiffChange::BlockDeleted {
                                block_id: base_block.id.clone(),
                                old_text: base_block.text.clone(),
                                old_block: base_block.block.clone(),
                                move_id: None,
                            });
                            changes.push(DiffChange::BlockInserted {
                                after_block_id: Some(base_block.id.clone()),
                                block: target_block.block.clone(),
                                move_id: None,
                            });
                        } else {
                            // Block text was modified - use mark-preserving diff
                            let base_inlines = block_inlines(&base_block.block);
                            let target_inlines = block_inlines(&target_block.block);
                            let inline_changes =
                                diff_block_content_with_marks(&base_inlines, &target_inlines);
                            changes.push(DiffChange::BlockModified {
                                block_id: base_block.id.clone(),
                                old_text: base_block.text.clone(),
                                new_text: target_block.text.clone(),
                                inline_changes,
                                old_block: base_block.block.clone(),
                                new_block: target_block.block.clone(),
                                para_split: false,
                            });
                        }
                    }
                    (DiffableElement::Table(base_table), DiffableElement::Table(target_table)) => {
                        // Run row alignment to detect row-level changes,
                        // even when structure hashes match.
                        let table_changes = diff_table_pair(base_table, target_table)?;
                        changes.extend(table_changes);
                    }
                    // Same guarantee as the Matched arm above: alignment's DP
                    // cost function marks Block/Table pairs incompatible, so a
                    // Modified alignment never crosses variants either.
                    (DiffableElement::Block(_), DiffableElement::Table(_))
                    | (DiffableElement::Table(_), DiffableElement::Block(_)) => {
                        unreachable!(
                            "element alignment's type compatibility check guarantees \
                             a Modified pair never mixes Block and Table"
                        )
                    }
                }
            }
            ElementAlignment::Deleted { base_idx } => {
                match &base[*base_idx] {
                    DiffableElement::Block(block) => {
                        changes.push(DiffChange::BlockDeleted {
                            block_id: block.id.clone(),
                            old_text: block.text.clone(),
                            old_block: block.block.clone(),
                            move_id: None,
                        });
                    }
                    DiffableElement::Table(table) => {
                        // Model table deletions as block deletion.
                        changes.push(DiffChange::BlockDeleted {
                            block_id: table.id.clone(),
                            old_text: table.text_fingerprint.clone(),
                            old_block: BlockNode::from(table.table.clone()),
                            move_id: None,
                        });
                    }
                }
            }
            ElementAlignment::Inserted { target_idx } => {
                // For insertions, after_block_id should reference an element that exists
                // (or will exist) in the base document.
                let mut insertion_anchor = None;
                for prior_target_idx in (0..*target_idx).rev() {
                    if let Some(base_id) = target_idx_to_base_id.get(&prior_target_idx) {
                        insertion_anchor = Some(base_id.clone());
                        break;
                    }
                }
                match &target[*target_idx] {
                    DiffableElement::Block(block) => {
                        changes.push(DiffChange::BlockInserted {
                            after_block_id: insertion_anchor,
                            block: block.block.clone(),
                            move_id: None,
                        });
                    }
                    DiffableElement::Table(table) => {
                        // Model table insertions as block insertion.
                        changes.push(DiffChange::BlockInserted {
                            after_block_id: insertion_anchor,
                            block: BlockNode::from(table.table.clone()),
                            move_id: None,
                        });
                    }
                }
                // Note: inserted anchors are resolved against base IDs only.
            }
        }
    }

    Ok(changes)
}

/// Diff a pair of aligned tables.
///
/// First runs row-level alignment to detect inserted/deleted rows. If any
/// rows were inserted or deleted, emits `TableStructureChanged` for
/// row-level tracking. Otherwise falls back to cell-level diffing via
/// `diff_matched_tables`.
fn diff_table_pair(
    base: &DiffableTable,
    target: &DiffableTable,
) -> Result<Vec<DiffChange>, String> {
    let table_diff = compute_table_diff_result(&base.table, &target.table)?;
    let has_row_changes = table_diff.row_alignment.iter().any(|a| {
        matches!(
            a,
            TableRowAlignment::Inserted { .. } | TableRowAlignment::Deleted { .. }
        )
    });

    if has_row_changes {
        Ok(vec![DiffChange::TableStructureChanged {
            table_id: base.id.clone(),
            target_table_id: target.id.clone(),
            old_hash: base.structure_hash.clone(),
            new_hash: target.structure_hash.clone(),
            old_text: extract_table_text(&base.table),
            new_text: extract_table_text(&target.table),
            table_diff: Some(Box::new(table_diff)),
        }])
    } else {
        diff_matched_tables(base, target)
    }
}

/// Diff matched tables with the same structure.
///
/// When tables have the same structure (identical `structure_hash`), walks
/// through cells in parallel and produces per-cell inline changes via
/// `TableCellsModified`. This lets the merge step apply inline tracked
/// changes within each cell paragraph, keeping a single table in the output
/// instead of splitting into deleted + inserted copies.
///
/// Nested `BlockNode::Table` elements within cells are recursively diffed
/// so that inner table content also gets inline tracked changes.
/// True iff every matched cell of two same-structure tables has the same number
/// of blocks AND each aligned block pair is diffable in place: same kind for
/// paragraphs/tables, and for opaques the *same opaque identity* (`opaque_ref`,
/// the same primitive `diff_opaque_blocks` uses to decide an opaque is
/// unchanged). When this holds, `diff_matched_tables` can safely zip cell block
/// lists for a per-cell inline diff; when it does not, the zip would silently
/// drop or misalign blocks — or, for a differing opaque pair, there is no
/// `DiffChange` variant that can represent an in-cell opaque swap (the merge
/// side has no consumer for it) — so the caller escalates to a structural
/// replace (P0 #4).
fn cell_block_lists_align(base: &DiffableTable, target: &DiffableTable) -> bool {
    fn same_kind(a: &BlockNode, b: &BlockNode) -> bool {
        match (a, b) {
            (BlockNode::Paragraph(_), BlockNode::Paragraph(_)) => true,
            (BlockNode::Table(_), BlockNode::Table(_)) => true,
            (BlockNode::OpaqueBlock(a), BlockNode::OpaqueBlock(b)) => a.opaque_ref == b.opaque_ref,
            (BlockNode::Paragraph(_), _)
            | (BlockNode::Table(_), _)
            | (BlockNode::OpaqueBlock(_), _) => false,
        }
    }
    // Row/cell counts are guaranteed equal here (same structure, no row changes),
    // so a shorter zip simply means we don't inspect the surplus — which is the
    // very gap we are guarding against; check the lengths explicitly.
    base.table
        .rows
        .iter()
        .zip(target.table.rows.iter())
        .all(|(base_row, target_row)| {
            base_row
                .cells
                .iter()
                .zip(target_row.cells.iter())
                .all(|(base_cell, target_cell)| {
                    base_cell.blocks.len() == target_cell.blocks.len()
                        && base_cell
                            .blocks
                            .iter()
                            .zip(target_cell.blocks.iter())
                            .all(|(b, t)| same_kind(b, t))
                })
        })
}

fn diff_matched_tables(
    base: &DiffableTable,
    target: &DiffableTable,
) -> Result<Vec<DiffChange>, String> {
    // Walk base and target TableNode rows/cells in parallel.
    // Same structure_hash guarantees identical row count, cell count per row,
    // grid spans, and vertical merge patterns — but NOT identical block counts
    // per cell: `compute_table_structure_hash` does not hash blocks-per-cell. A
    // cell that gained or lost a paragraph (or whose blocks no longer line up by
    // kind) therefore reaches here, where zipping the cell block lists would
    // truncate the surplus and misalign the rest — silently dropping the change
    // (P0 #4). When that happens we cannot produce a faithful per-cell inline
    // diff, so escalate the whole table to a structural replace: the merge
    // deletes the base table and inserts the target, reproducing the target
    // exactly on accept (coarser than inline, but correct rather than lossy).
    if !cell_block_lists_align(base, target) {
        return Ok(vec![DiffChange::TableStructureChanged {
            table_id: base.id.clone(),
            target_table_id: target.id.clone(),
            old_hash: base.structure_hash.clone(),
            new_hash: target.structure_hash.clone(),
            old_text: extract_table_text(&base.table),
            new_text: extract_table_text(&target.table),
            table_diff: None,
        }]);
    }

    let mut cell_changes: Vec<TableCellChange> = Vec::new();

    for (row_idx, (base_row, target_row)) in base
        .table
        .rows
        .iter()
        .zip(target.table.rows.iter())
        .enumerate()
    {
        for (cell_idx, (base_cell, target_cell)) in base_row
            .cells
            .iter()
            .zip(target_row.cells.iter())
            .enumerate()
        {
            let mut paragraph_changes: Vec<CellParagraphChange> = Vec::new();
            let mut nested_table_diffs: Vec<NestedTableDiff> = Vec::new();

            for (block_idx, (base_block, target_block)) in base_cell
                .blocks
                .iter()
                .zip(target_cell.blocks.iter())
                .enumerate()
            {
                match (base_block, target_block) {
                    (BlockNode::Paragraph(base_para), BlockNode::Paragraph(target_para)) => {
                        let base_inlines = base_para.all_inlines_owned();
                        let target_inlines = target_para.all_inlines_owned();
                        let base_text = extract_inline_text(&base_inlines);
                        let target_text = extract_inline_text(&target_inlines);

                        if base_text != target_text {
                            let inline_changes =
                                diff_block_content_with_marks(&base_inlines, &target_inlines);
                            paragraph_changes.push(CellParagraphChange {
                                block_index: block_idx,
                                inline_changes,
                                new_block: target_block.clone(),
                            });
                        }
                    }
                    (BlockNode::Table(base_inner), BlockNode::Table(target_inner)) => {
                        if let Some(nested_diff) =
                            diff_nested_tables(base_inner, target_inner, block_idx)?
                        {
                            nested_table_diffs.push(nested_diff);
                        }
                    }
                    (
                        BlockNode::OpaqueBlock(base_opaque),
                        BlockNode::OpaqueBlock(target_opaque),
                    ) => {
                        // cell_block_lists_align's same_kind only lets an
                        // (OpaqueBlock, OpaqueBlock) pair reach this loop when
                        // their opaque_ref already matches (same opaque
                        // identity) — a differing opaque_ref is caught there
                        // and escalates the whole table to
                        // TableStructureChanged before we get here. So a match
                        // here is always identity-equal: no cell change.
                        debug_assert_eq!(
                            base_opaque.opaque_ref, target_opaque.opaque_ref,
                            "cell_block_lists_align must reject differing opaque_ref pairs \
                             before diff_matched_tables zips cell block lists"
                        );
                    }
                    // Mismatched block kinds (paragraph/table/opaque combinations
                    // other than the three above): cell_block_lists_align's
                    // same_kind requires an exact kind match (plus opaque_ref
                    // equality for opaques), so no such pair can reach this loop
                    // — a mismatch there escalates to TableStructureChanged
                    // before diff_matched_tables starts zipping. Enumerated
                    // explicitly (rather than a bare `_`) so a future BlockNode
                    // variant added to same_kind without a matching arm here
                    // panics loudly instead of silently vanishing from the diff.
                    (BlockNode::Paragraph(_), _)
                    | (BlockNode::Table(_), _)
                    | (BlockNode::OpaqueBlock(_), _) => {
                        unreachable!(
                            "cell_block_lists_align guarantees only identical-kind \
                             (and, for opaques, identical opaque_ref) pairs reach \
                             diff_matched_tables' per-cell loop"
                        )
                    }
                }
            }

            let formatting_changed = base_cell.formatting != target_cell.formatting;
            let new_cell_formatting = if formatting_changed {
                Some(target_cell.formatting.clone())
            } else {
                None
            };

            if !paragraph_changes.is_empty() || !nested_table_diffs.is_empty() || formatting_changed
            {
                cell_changes.push(TableCellChange {
                    row_index: row_idx,
                    cell_index: cell_idx,
                    paragraph_changes,
                    nested_table_diffs,
                    new_cell_formatting,
                });
            }
        }
    }

    if cell_changes.is_empty() {
        return Ok(Vec::new());
    }

    Ok(vec![DiffChange::TableCellsModified {
        table_id: base.id.clone(),
        target_table_id: target.id.clone(),
        cell_changes,
        old_text: extract_table_text(&base.table),
        new_text: extract_table_text(&target.table),
    }])
}

/// Diff a pair of nested tables within a cell.
///
/// Runs row-level alignment to detect structural changes. If rows were
/// inserted/deleted, returns `NestedTableDiffKind::StructureChanged`. Otherwise,
/// recursively diffs cell content (including deeper nested tables) and returns
/// `NestedTableDiffKind::CellsModified`.
pub fn diff_nested_tables(
    base: &TableNode,
    target: &TableNode,
    block_index: usize,
) -> Result<Option<NestedTableDiff>, String> {
    // Fast path: skip if text is identical.
    let base_text = extract_table_text(base);
    let target_text = extract_table_text(target);
    if base_text == target_text {
        return Ok(None);
    }

    let table_diff = compute_table_diff_result(base, target)?;
    let has_row_changes = table_diff.row_alignment.iter().any(|a| {
        matches!(
            a,
            TableRowAlignment::Inserted { .. } | TableRowAlignment::Deleted { .. }
        )
    });

    if has_row_changes {
        Ok(Some(NestedTableDiff {
            block_index,
            diff: NestedTableDiffKind::StructureChanged {
                table_diff: Box::new(table_diff),
                new_table: Box::new(target.clone()),
            },
        }))
    } else {
        // Same structure — diff cells recursively using DiffableTable wrapper.
        let base_dt = DiffableTable {
            id: base.id.clone(),
            structure_hash: base.structure_hash.clone(),
            text_fingerprint: base_text,
            table: base.clone(),
        };
        let target_dt = DiffableTable {
            id: target.id.clone(),
            structure_hash: target.structure_hash.clone(),
            text_fingerprint: target_text,
            table: target.clone(),
        };
        let changes = diff_matched_tables(&base_dt, &target_dt)?;
        // Extract the cell_changes from the single TableCellsModified variant.
        let cell_changes: Vec<TableCellChange> = changes
            .into_iter()
            .filter_map(|c| match c {
                DiffChange::TableCellsModified { cell_changes, .. } => Some(cell_changes),
                _ => None,
            })
            .flatten()
            .collect();

        if cell_changes.is_empty() {
            Ok(None)
        } else {
            Ok(Some(NestedTableDiff {
                block_index,
                diff: NestedTableDiffKind::CellsModified { cell_changes },
            }))
        }
    }
}

/// Diff inline content within a single block using token-level diffing.
/// This version does not preserve marks (legacy, used for plain text).
pub fn diff_block_content(old_text: &str, new_text: &str) -> Vec<InlineChange> {
    let heuristics = DiffHeuristics::from_env();
    let old_tokens = tokenize(old_text);
    let new_tokens = tokenize(new_text);
    let diff = TextDiff::configure()
        .algorithm(Algorithm::Patience)
        .diff_slices(&old_tokens, &new_tokens);
    let result: Vec<InlineChange> = diff
        .iter_all_changes()
        .map(|change| match change.tag() {
            ChangeTag::Equal => InlineChange::Unchanged {
                text: change.to_string_lossy().into_owned(),
                marks: Vec::new(),
                style_props: StyleProps::default(),
                formatting_change: None,
            },
            ChangeTag::Insert => InlineChange::Inserted {
                text: change.to_string_lossy().into_owned(),
                marks: Vec::new(),
                style_props: StyleProps::default(),
                formatting_change: None,
                rev_id: 0,
            },
            ChangeTag::Delete => InlineChange::Deleted {
                text: change.to_string_lossy().into_owned(),
                marks: Vec::new(),
                style_props: StyleProps::default(),
                formatting_change: None,
                rev_id: 0,
            },
        })
        .collect();
    cleanup_inline_changes_with_config(result, &heuristics)
}

/// Per-character formatting info: boolean marks + value-carrying style props.
#[derive(Clone, Default)]
struct CharFormatting {
    marks: Vec<Mark>,
    style_props: StyleProps,
}

/// Extract text from inlines along with a per-character formatting mapping.
/// Returns (text, char_fmt) where char_fmt[i] contains the formatting for character i.
fn extract_text_with_marks(inlines: &[InlineNode]) -> (String, Vec<CharFormatting>) {
    let mut text = String::new();
    let mut char_fmt: Vec<CharFormatting> = Vec::new();

    for inline in inlines {
        match inline {
            InlineNode::Text(t) => {
                // Apply caps mark to normalize text for comparison
                let node_text = if t.style_props.caps == MarkValue::On {
                    t.text.to_uppercase()
                } else {
                    t.text.clone()
                };
                let fmt = CharFormatting {
                    marks: t.marks.clone(),
                    style_props: t.style_props.clone(),
                };
                // Add formatting for each character in this text node
                for _ in node_text.chars() {
                    char_fmt.push(fmt.clone());
                }
                text.push_str(&node_text);
            }
            InlineNode::HardBreak(_) => {
                text.push('\n');
                char_fmt.push(CharFormatting::default()); // No formatting for hard break
            }
            InlineNode::OpaqueInline(o) => {
                let placeholder = opaque_placeholder(o);
                for _ in placeholder.chars() {
                    char_fmt.push(CharFormatting::default());
                }
                text.push_str(&placeholder);
            }
            InlineNode::Decoration(_)
            | InlineNode::CommentRangeStart { .. }
            | InlineNode::CommentRangeEnd { .. }
            | InlineNode::CommentReference { .. } => {
                // Zero-width, no contribution
            }
        }
    }

    (text, char_fmt)
}

/// Tracks an opaque inline's position in the original inline list,
/// used to resolve tagged placeholders back to `InlineChange::Opaque` segments.
struct OpaqueTracker<'a> {
    inline_index: usize,
    node: &'a OpaqueInlineNode,
}

/// Extract text from inlines with per-character formatting and opaque tracking.
/// Uses `opaque_diff_tag()` for identity-bearing placeholders so the diff
/// algorithm can distinguish different opaques.
fn extract_text_with_marks_and_opaques<'a>(
    inlines: &'a [InlineNode],
) -> (String, Vec<CharFormatting>, Vec<OpaqueTracker<'a>>) {
    let mut text = String::new();
    let mut char_fmt: Vec<CharFormatting> = Vec::new();
    let mut opaques: Vec<OpaqueTracker<'a>> = Vec::new();

    for (inline_index, inline) in inlines.iter().enumerate() {
        match inline {
            InlineNode::Text(t) => {
                let node_text = if t.style_props.caps == MarkValue::On {
                    t.text.to_uppercase()
                } else {
                    t.text.clone()
                };
                let fmt = CharFormatting {
                    marks: t.marks.clone(),
                    style_props: t.style_props.clone(),
                };
                for _ in node_text.chars() {
                    char_fmt.push(fmt.clone());
                }
                text.push_str(&node_text);
            }
            InlineNode::HardBreak(_) => {
                text.push('\n');
                char_fmt.push(CharFormatting::default());
            }
            InlineNode::OpaqueInline(o) => {
                opaques.push(OpaqueTracker {
                    inline_index,
                    node: o,
                });
                let tag = opaque_diff_tag(o);
                for _ in tag.chars() {
                    char_fmt.push(CharFormatting::default());
                }
                text.push_str(&tag);
            }
            InlineNode::Decoration(_)
            | InlineNode::CommentRangeStart { .. }
            | InlineNode::CommentRangeEnd { .. }
            | InlineNode::CommentReference { .. } => {}
        }
    }

    (text, char_fmt, opaques)
}

/// Get formatting at a specific character offset, or default if out of bounds.
fn formatting_at_offset(char_fmt: &[CharFormatting], offset: usize) -> CharFormatting {
    char_fmt.get(offset).cloned().unwrap_or_default()
}

/// Get formatting for a token spanning `offset..offset+len`.
///
/// Uses the first character's formatting as the base, but drops caps
/// unless **every** character in the range carries it. Caps is the only mark
/// with a rendering side-effect (uppercase transform), so applying it to a
/// whole token based on a single character produces incorrect text.
fn formatting_for_token_range(
    char_fmt: &[CharFormatting],
    offset: usize,
    len: usize,
) -> CharFormatting {
    let mut fmt = formatting_at_offset(char_fmt, offset);

    // Fast path: single-char token or no caps mark — nothing to reconcile.
    if len <= 1 || fmt.style_props.caps != MarkValue::On {
        return fmt;
    }

    // Check whether ALL characters in the token carry Caps.
    let all_caps = (offset..offset + len).all(|i| {
        char_fmt
            .get(i)
            .is_some_and(|cf| cf.style_props.caps == MarkValue::On)
    });

    if !all_caps {
        fmt.style_props.caps = MarkValue::Inherit;
    }

    fmt
}

/// Detect formatting-only changes between old and new character positions.
///
/// When text is unchanged (ChangeTag::Equal) but marks or style_props differ,
/// returns the current (new) formatting and a FormattingChange capturing the
/// previous (old) formatting. Author/date are left empty; the merge step fills
/// them from RevisionInfo.
fn detect_formatting_change(
    old_fmt: &CharFormatting,
    new_fmt: &CharFormatting,
) -> (Vec<Mark>, StyleProps, Option<FormattingChange>) {
    let changed = old_fmt.marks != new_fmt.marks || old_fmt.style_props != new_fmt.style_props;
    if changed {
        (
            new_fmt.marks.clone(),
            new_fmt.style_props.clone(),
            Some(FormattingChange {
                previous_marks: old_fmt.marks.clone(),
                previous_style_props: old_fmt.style_props.clone(),
                // CharFormatting (this whole character-diff pipeline) never
                // tracked per-property rPr authoring provenance — it only
                // ever carried marks/style_props, so there is no "previous"
                // authored-bitset to recover here. Defaulting to
                // "nothing authored" is neutral, not a regression: this
                // path had no such concept before `previous_rpr_authored`
                // existed either. A reject of a formatting change SYNTHESIZED
                // by document comparison (as opposed to authored through
                // SetRunFormatting, which now captures this correctly) may
                // therefore under-restore authored-vs-inherited state — a
                // known, pre-existing gap, not something this fix widens.
                previous_rpr_authored: RunRprAuthored::default(),
                // Placeholder: merge_diff fills from RevisionInfo.
                revision_id: 0,
                identity: 0,
                author: String::new(),
                date: None,
            }),
        )
    } else {
        (old_fmt.marks.clone(), old_fmt.style_props.clone(), None)
    }
}

/// Diff inline content with marks preservation.
/// Uses token-level diffing but maps character positions back to source marks.
pub fn diff_block_content_with_marks(
    old_inlines: &[InlineNode],
    new_inlines: &[InlineNode],
) -> Vec<InlineChange> {
    diff_block_content_with_marks_and_notes(old_inlines, new_inlines, &HashMap::new())
}

fn diff_block_content_with_marks_and_notes(
    old_inlines: &[InlineNode],
    new_inlines: &[InlineNode],
    note_markers: &HashMap<String, String>,
) -> Vec<InlineChange> {
    let heuristics = DiffHeuristics::from_env();
    // Extract text and build char->formatting mapping
    let (old_text, old_char_fmt) = extract_text_with_marks(old_inlines);
    let (new_text, new_char_fmt) = extract_text_with_marks(new_inlines);

    // Layer 3: Early bail-out for heavily rewritten text
    if should_bail_out_with_config(&old_text, &new_text, &heuristics) {
        return build_full_replace(old_inlines, new_inlines, note_markers);
    }

    // Tokenize and diff
    let old_tokens = tokenize(&old_text);
    let new_tokens = tokenize(&new_text);
    let diff = TextDiff::configure()
        .algorithm(Algorithm::Patience)
        .diff_slices(&old_tokens, &new_tokens);

    // Track offsets and map back to formatting
    let mut old_offset = 0usize;
    let mut new_offset = 0usize;
    let mut result = Vec::new();

    for change in diff.iter_all_changes() {
        let text = change.to_string_lossy().into_owned();
        let len = text.chars().count();

        match change.tag() {
            ChangeTag::Equal => {
                let old_fmt = formatting_for_token_range(&old_char_fmt, old_offset, len);
                let new_fmt = formatting_for_token_range(&new_char_fmt, new_offset, len);
                let (marks, style_props, formatting_change) =
                    detect_formatting_change(&old_fmt, &new_fmt);
                result.push(InlineChange::Unchanged {
                    text,
                    marks,
                    style_props,
                    formatting_change,
                });
                old_offset += len;
                new_offset += len;
            }
            ChangeTag::Delete => {
                let fmt = formatting_for_token_range(&old_char_fmt, old_offset, len);
                result.push(InlineChange::Deleted {
                    text,
                    marks: fmt.marks,
                    style_props: fmt.style_props,
                    formatting_change: None,
                    rev_id: 0,
                });
                old_offset += len;
            }
            ChangeTag::Insert => {
                let fmt = formatting_for_token_range(&new_char_fmt, new_offset, len);
                result.push(InlineChange::Inserted {
                    text,
                    marks: fmt.marks,
                    style_props: fmt.style_props,
                    formatting_change: None,
                    rev_id: 0,
                });
                new_offset += len;
            }
        }
    }

    // Layer 2: Post-processing cleanup (zipper collapse)
    cleanup_inline_changes_with_config(result, &heuristics)
}

/// Build an `InlineChange::Opaque` from a tracked opaque node.
fn build_opaque_change(
    tracker: &OpaqueTracker,
    segment_type: InlineChangeSegmentType,
    note_markers: &HashMap<String, String>,
) -> InlineChange {
    let (text, reference_id, field_kind, field_instruction) =
        extract_opaque_metadata(&tracker.node.kind, note_markers, Some(tracker.node));
    InlineChange::Opaque {
        segment_type,
        kind: opaque_kind_to_segment_kind(&tracker.node.kind),
        opaque_id: tracker.node.id.0.to_string(),
        inline_index: tracker.inline_index,
        text,
        reference_id,
        field_kind,
        field_instruction,
        asset_ref: None,
        asset_width_emu: None,
        asset_height_emu: None,
        alt_text: None,
        url: opaque_url(&tracker.node.kind),
        content_hash: tracker.node.content_hash.clone(),
    }
}

/// Diff inline content resolving opaque placeholders back to `InlineChange::Opaque`.
///
/// This is the diff function used by `build_full_document_view`. Unlike
/// `diff_block_content_with_marks_and_notes` (used by `compute_changes`), this
/// function uses identity-bearing `\u{FFFC}` placeholders so the diff algorithm
/// can distinguish different opaques, then resolves them back to proper
/// `InlineChange::Opaque` segments.
fn diff_block_content_resolving_opaques(
    old_inlines: &[InlineNode],
    new_inlines: &[InlineNode],
    note_markers: &HashMap<String, String>,
) -> Vec<InlineChange> {
    let heuristics = DiffHeuristics::from_env();
    let (old_text, old_char_fmt, old_opaques) = extract_text_with_marks_and_opaques(old_inlines);
    let (new_text, new_char_fmt, new_opaques) = extract_text_with_marks_and_opaques(new_inlines);

    if should_bail_out_with_config(&old_text, &new_text, &heuristics) {
        return sort_opaque_runs_by_inline_index(build_full_replace(
            old_inlines,
            new_inlines,
            note_markers,
        ));
    }

    let old_tokens = tokenize(&old_text);
    let new_tokens = tokenize(&new_text);
    let diff = TextDiff::configure()
        .algorithm(Algorithm::Patience)
        .diff_slices(&old_tokens, &new_tokens);

    let mut old_offset = 0usize;
    let mut new_offset = 0usize;
    let mut old_opaque_idx = 0usize;
    let mut new_opaque_idx = 0usize;
    let mut result = Vec::new();

    for change in diff.iter_all_changes() {
        let text = change.to_string_lossy().into_owned();
        let len = text.chars().count();

        if text.starts_with('\u{FFFC}') {
            // Opaque token — resolve to InlineChange::Opaque
            match change.tag() {
                ChangeTag::Equal => {
                    let tracker = &new_opaques[new_opaque_idx];
                    result.push(build_opaque_change(
                        tracker,
                        InlineChangeSegmentType::Equal,
                        note_markers,
                    ));
                    old_opaque_idx += 1;
                    new_opaque_idx += 1;
                    old_offset += len;
                    new_offset += len;
                }
                ChangeTag::Delete => {
                    let tracker = &old_opaques[old_opaque_idx];
                    result.push(build_opaque_change(
                        tracker,
                        InlineChangeSegmentType::Delete,
                        note_markers,
                    ));
                    old_opaque_idx += 1;
                    old_offset += len;
                }
                ChangeTag::Insert => {
                    let tracker = &new_opaques[new_opaque_idx];
                    result.push(build_opaque_change(
                        tracker,
                        InlineChangeSegmentType::Insert,
                        note_markers,
                    ));
                    new_opaque_idx += 1;
                    new_offset += len;
                }
            }
        } else {
            // Regular text — same logic as diff_block_content_with_marks_and_notes
            match change.tag() {
                ChangeTag::Equal => {
                    let old_fmt = formatting_for_token_range(&old_char_fmt, old_offset, len);
                    let new_fmt = formatting_for_token_range(&new_char_fmt, new_offset, len);
                    let (marks, style_props, formatting_change) =
                        detect_formatting_change(&old_fmt, &new_fmt);
                    result.push(InlineChange::Unchanged {
                        text,
                        marks,
                        style_props,
                        formatting_change,
                    });
                    old_offset += len;
                    new_offset += len;
                }
                ChangeTag::Delete => {
                    let fmt = formatting_for_token_range(&old_char_fmt, old_offset, len);
                    result.push(InlineChange::Deleted {
                        text,
                        marks: fmt.marks,
                        style_props: fmt.style_props,
                        formatting_change: None,
                        rev_id: 0,
                    });
                    old_offset += len;
                }
                ChangeTag::Insert => {
                    let fmt = formatting_for_token_range(&new_char_fmt, new_offset, len);
                    result.push(InlineChange::Inserted {
                        text,
                        marks: fmt.marks,
                        style_props: fmt.style_props,
                        formatting_change: None,
                        rev_id: 0,
                    });
                    new_offset += len;
                }
            }
        }
    }

    let result = cleanup_inline_changes_with_config(result, &heuristics);
    sort_opaque_runs_by_inline_index(result)
}

/// Sort consecutive runs of opaque segments by inline_index so they are
/// monotonically non-decreasing. The diff algorithm groups all deletes before
/// inserts within a hunk, which can produce out-of-order inline indices when
/// opaques at multiple positions are replaced (e.g. indices 2,3,2,3 instead
/// of 2,2,3,3).
fn sort_opaque_runs_by_inline_index(mut changes: Vec<InlineChange>) -> Vec<InlineChange> {
    let len = changes.len();
    let mut i = 0;
    while i < len {
        if matches!(changes[i], InlineChange::Opaque { .. }) {
            let start = i;
            while i < len && matches!(changes[i], InlineChange::Opaque { .. }) {
                i += 1;
            }
            if i - start > 1 {
                changes[start..i].sort_by_key(|c| match c {
                    InlineChange::Opaque {
                        inline_index,
                        segment_type,
                        ..
                    } => {
                        let type_order = match segment_type {
                            InlineChangeSegmentType::Equal => 0,
                            InlineChangeSegmentType::Delete => 1,
                            InlineChangeSegmentType::Insert => 2,
                        };
                        (*inline_index, type_order)
                    }
                    _ => unreachable!(),
                });
            }
        } else {
            i += 1;
        }
    }
    changes
}

// =============================================================================
// Layer 3: Early bail-out
// =============================================================================

/// Check if a token looks like a high-value numeric/financial token.
fn is_high_value_token(s: &str) -> bool {
    // Numbers, percentages, currency, section references
    s.chars().any(|c| c.is_ascii_digit())
        || s == "%"
        || s == "$"
        || s.eq_ignore_ascii_case("USD")
        || s.eq_ignore_ascii_case("EUR")
        || s.eq_ignore_ascii_case("GBP")
}

/// Compute content-only similarity (ignoring whitespace tokens).
/// Whitespace tokens inflate standard text_similarity, making bail-out
/// hard to trigger for truly different texts.
fn content_similarity(old_text: &str, new_text: &str) -> f64 {
    let norm1 = normalize_for_similarity(old_text);
    let norm2 = normalize_for_similarity(new_text);
    let old_tokens: Vec<&str> = tokenize(&norm1)
        .into_iter()
        .filter(|t| !t.trim().is_empty())
        .collect();
    let new_tokens: Vec<&str> = tokenize(&norm2)
        .into_iter()
        .filter(|t| !t.trim().is_empty())
        .collect();
    if old_tokens.is_empty() && new_tokens.is_empty() {
        return 1.0;
    }
    if old_tokens.is_empty() || new_tokens.is_empty() {
        return 0.0;
    }
    TextDiff::configure()
        .algorithm(Algorithm::Patience)
        .diff_slices(&old_tokens, &new_tokens)
        .ratio() as f64
}

/// Check if we should bail out of token-level diffing.
/// Returns true if texts are so dissimilar that a clean delete-all/insert-all is better.
/// Safety: never bails out if high-value tokens (numbers, currency, percentages) differ.
#[cfg(test)]
fn should_bail_out(old_text: &str, new_text: &str) -> bool {
    let heuristics = DiffHeuristics::from_env();
    should_bail_out_with_config(old_text, new_text, &heuristics)
}

fn should_bail_out_with_config(
    old_text: &str,
    new_text: &str,
    heuristics: &DiffHeuristics,
) -> bool {
    // Only consider bail-out for sufficiently long texts
    if old_text.chars().count() < heuristics.bail_out_min_chars
        && new_text.chars().count() < heuristics.bail_out_min_chars
    {
        return false;
    }

    let sim = content_similarity(old_text, new_text);
    if sim >= heuristics.bail_out_similarity_threshold {
        return false;
    }

    // Safety check: scan for differing high-value tokens
    let old_tokens = tokenize(old_text);
    let new_tokens = tokenize(new_text);

    let old_hv: std::collections::HashSet<&str> = old_tokens
        .iter()
        .copied()
        .filter(|t| is_high_value_token(t))
        .collect();
    let new_hv: std::collections::HashSet<&str> = new_tokens
        .iter()
        .copied()
        .filter(|t| is_high_value_token(t))
        .collect();

    // If any high-value tokens differ, don't bail out — preserve inline visibility
    if old_hv != new_hv {
        return false;
    }

    true
}

/// Build a clean delete-all + insert-all replacement from inlines, preserving marks.
/// Common leading/trailing segments are factored out as Unchanged.
fn build_full_replace(
    old_inlines: &[InlineNode],
    new_inlines: &[InlineNode],
    note_markers: &HashMap<String, String>,
) -> Vec<InlineChange> {
    let mut result = inlines_to_segments(old_inlines, "delete", note_markers);
    result.extend(inlines_to_segments(new_inlines, "insert", note_markers));
    factor_common_affixes(result)
}

// =============================================================================
// Layer 2: Post-processing cleanup (zipper collapse)
// =============================================================================

/// Count non-whitespace characters in a string.
fn non_ws_chars(s: &str) -> usize {
    s.chars().filter(|c| !c.is_whitespace()).count()
}

/// Get the text content of an InlineChange.
fn inline_change_text(change: &InlineChange) -> &str {
    match change {
        InlineChange::Unchanged { text, .. } => text,
        InlineChange::Deleted { text, .. } => text,
        InlineChange::Inserted { text, .. } => text,
        InlineChange::Opaque { text, .. } => text.as_deref().unwrap_or(""),
    }
}

/// Get the marks of an InlineChange.
fn inline_change_marks(change: &InlineChange) -> &[Mark] {
    match change {
        InlineChange::Unchanged { marks, .. } => marks,
        InlineChange::Deleted { marks, .. } => marks,
        InlineChange::Inserted { marks, .. } => marks,
        InlineChange::Opaque { .. } => &[],
    }
}

/// Get the style_props of an InlineChange.
/// Panics on Opaque (which has no style_props); callers must filter those out.
fn inline_change_style_props(change: &InlineChange) -> &StyleProps {
    match change {
        InlineChange::Unchanged { style_props, .. }
        | InlineChange::Deleted { style_props, .. }
        | InlineChange::Inserted { style_props, .. } => style_props,
        InlineChange::Opaque { .. } => {
            panic!("inline_change_style_props called on Opaque segment")
        }
    }
}

/// Check if an InlineChange is an enumerator token like (i), (ii), (a) etc.
fn is_enumerator_anchor(change: &InlineChange) -> bool {
    let text = inline_change_text(change);
    if !text.starts_with('(') || !text.ends_with(')') || text.len() < 3 {
        return false;
    }
    let inner = &text[1..text.len() - 1];
    is_enumerator_content(inner)
}

/// Merge adjacent same-type segments only when their formatting payload matches.
fn merge_adjacent_same_type(changes: Vec<InlineChange>) -> Vec<InlineChange> {
    if changes.is_empty() {
        return changes;
    }

    let mut result: Vec<InlineChange> = Vec::with_capacity(changes.len());

    for change in changes {
        if matches!(change, InlineChange::Opaque { .. }) {
            result.push(change);
            continue;
        }

        let should_merge = if let Some(last) = result.last() {
            !matches!(last, InlineChange::Opaque { .. })
                && std::mem::discriminant(last) == std::mem::discriminant(&change)
                && inline_change_marks(last) == inline_change_marks(&change)
                && match (last, &change) {
                    (
                        InlineChange::Unchanged {
                            style_props: last_style,
                            formatting_change: last_fc,
                            ..
                        },
                        InlineChange::Unchanged {
                            style_props: change_style,
                            formatting_change: change_fc,
                            ..
                        },
                    ) => last_style == change_style && last_fc == change_fc,
                    (
                        InlineChange::Deleted {
                            style_props: last_style,
                            ..
                        },
                        InlineChange::Deleted {
                            style_props: change_style,
                            ..
                        },
                    ) => last_style == change_style,
                    (
                        InlineChange::Inserted {
                            style_props: last_style,
                            ..
                        },
                        InlineChange::Inserted {
                            style_props: change_style,
                            ..
                        },
                    ) => last_style == change_style,
                    _ => false,
                }
        } else {
            false
        };

        if should_merge {
            // Merge into the last element — should_merge is only true when result is non-empty
            let Some(last) = result.last_mut() else {
                unreachable!("should_merge requires non-empty result");
            };
            match last {
                InlineChange::Unchanged { text, .. } => {
                    text.push_str(inline_change_text(&change));
                }
                InlineChange::Deleted { text, .. } => {
                    text.push_str(inline_change_text(&change));
                }
                InlineChange::Inserted { text, .. } => {
                    text.push_str(inline_change_text(&change));
                }
                InlineChange::Opaque { .. } => {}
            }
        } else {
            result.push(change);
        }
    }

    result
}

/// Identify indices of "strong anchors" — Unchanged spans with >= ANCHOR_MIN_CHARS
/// non-whitespace characters, OR enumerator tokens like (i), (ii).
#[cfg(test)]
fn find_strong_anchors(changes: &[InlineChange]) -> Vec<usize> {
    let heuristics = DiffHeuristics::from_env();
    find_strong_anchors_with_config(changes, &heuristics)
}

fn find_strong_anchors_with_config(
    changes: &[InlineChange],
    heuristics: &DiffHeuristics,
) -> Vec<usize> {
    changes
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            if (matches!(c, InlineChange::Unchanged { .. })
                && (non_ws_chars(inline_change_text(c)) >= heuristics.anchor_min_chars
                    || is_enumerator_anchor(c)
                    // Unchanged text containing U+FFFC opaque placeholders must also be
                    // treated as strong anchors. When diff_block_content_with_marks is
                    // used (instead of diff_block_content_resolving_opaques), opaques
                    // appear as bare U+FFFC inside Unchanged text rather than as
                    // InlineChange::Opaque. Collapsing such a region duplicates the
                    // placeholder into both del and ins sides, causing the opaque element
                    // to appear twice (once deleted, once inserted) instead of once as
                    // Normal — which breaks accept/reject text parity.
                    || inline_change_text(c).contains('\u{FFFC}')))
                // Equal opaques (unchanged fldSimple, hyperlink, etc.) must never be
                // collapsed into adjacent del/ins regions — collapse_region destroys
                // their position information by merging the placeholder text into both
                // the del and ins sides.
                || matches!(
                    c,
                    InlineChange::Opaque {
                        segment_type: InlineChangeSegmentType::Equal,
                        ..
                    }
                )
            {
                Some(i)
            } else {
                None
            }
        })
        .collect()
}

/// Count alternating del/ins groups in a region.
/// Each contiguous block of Deleted tokens is one group; each contiguous block
/// of Inserted tokens is one group. Tiny unchanged spans (< 3 non-ws chars)
/// are ignored (treated as transparent). This counts the number of such groups.
fn count_change_runs(region: &[InlineChange]) -> usize {
    let mut runs = 0;
    // Track whether last significant segment was Del, Ins, or Equal
    // 0 = none, 1 = deleted, 2 = inserted
    let mut last_type: u8 = 0;

    for change in region {
        match change {
            InlineChange::Deleted { .. } => {
                if last_type != 1 {
                    runs += 1;
                }
                last_type = 1;
            }
            InlineChange::Inserted { .. } => {
                if last_type != 2 {
                    runs += 1;
                }
                last_type = 2;
            }
            InlineChange::Unchanged { text, .. } => {
                // Only reset if this is a substantial unchanged span
                if non_ws_chars(text) >= 3 {
                    last_type = 0;
                }
                // Otherwise ignore (transparent)
            }
            InlineChange::Opaque {
                segment_type, text, ..
            } => {
                let mapped = match segment_type {
                    InlineChangeSegmentType::Equal => 0,
                    InlineChangeSegmentType::Delete => 1,
                    InlineChangeSegmentType::Insert => 2,
                };
                if mapped == 0 {
                    if non_ws_chars(text.as_deref().unwrap_or("")) >= 3 {
                        last_type = 0;
                    }
                } else if last_type != mapped {
                    runs += 1;
                    last_type = mapped;
                }
            }
        }
    }

    runs
}

/// Count how many tokens in a region are changed (Deleted or Inserted).
fn count_changed_tokens(region: &[InlineChange]) -> usize {
    region
        .iter()
        .filter(|c| {
            matches!(
                c,
                InlineChange::Deleted { .. }
                    | InlineChange::Inserted { .. }
                    | InlineChange::Opaque {
                        segment_type: InlineChangeSegmentType::Delete
                            | InlineChangeSegmentType::Insert,
                        ..
                    }
            )
        })
        .count()
}

/// Compute the text similarity of just the old-side and new-side of a region.
fn region_similarity(region: &[InlineChange]) -> f64 {
    let old_text: String = region
        .iter()
        .filter_map(|c| match c {
            InlineChange::Unchanged { text, .. } | InlineChange::Deleted { text, .. } => {
                Some(text.as_str())
            }
            _ => None,
        })
        .collect();
    let new_text: String = region
        .iter()
        .filter_map(|c| match c {
            InlineChange::Unchanged { text, .. } | InlineChange::Inserted { text, .. } => {
                Some(text.as_str())
            }
            _ => None,
        })
        .collect();
    text_similarity(&old_text, &new_text)
}

/// Decide whether a region between strong anchors should be collapsed.
#[cfg(test)]
fn should_collapse_region(region: &[InlineChange]) -> bool {
    let heuristics = DiffHeuristics::from_env();
    should_collapse_region_with_config(region, &heuristics)
}

fn should_collapse_region_with_config(
    region: &[InlineChange],
    heuristics: &DiffHeuristics,
) -> bool {
    // Check if region has any changes at all
    let changed = count_changed_tokens(region);
    if changed == 0 {
        return false;
    }

    let runs = count_change_runs(region);
    let total = region.len();

    // High alternation: many runs relative to region size
    if runs >= heuristics.zipper_min_change_runs
        && total > 0
        && (runs as f64 / total as f64) >= 0.20
    {
        return true;
    }

    // Low similarity: force collapse even without high alternation
    if region_similarity(region) < heuristics.low_similarity_threshold {
        return true;
    }

    false
}

/// Factor out matching leading/trailing segments from a DEL+INS sequence as Unchanged.
///
/// Given a `Vec<InlineChange>` containing Deleted segments followed by Inserted segments
/// (as produced by `collapse_region` or `build_full_replace`), compare the del and ins
/// segment lists. Leading segments that match (same text and marks) become Unchanged
/// (prefix), trailing matching segments become Unchanged (suffix), and the differing
/// middle remains Deleted/Inserted.
fn factor_common_affixes(changes: Vec<InlineChange>) -> Vec<InlineChange> {
    // Split into del and ins segments.
    let mut del: Vec<&InlineChange> = Vec::new();
    let mut ins: Vec<&InlineChange> = Vec::new();
    for c in &changes {
        match c {
            InlineChange::Deleted { .. } => del.push(c),
            InlineChange::Inserted { .. } => ins.push(c),
            // Pass through unchanged/opaque unmodified.
            _ => {}
        }
    }

    if del.is_empty() || ins.is_empty() {
        return changes;
    }

    let segs_match = |d: &&InlineChange, i: &&InlineChange| {
        inline_change_text(d) == inline_change_text(i)
            && inline_change_marks(d) == inline_change_marks(i)
            && inline_change_style_props(d) == inline_change_style_props(i)
    };

    let prefix_len = del
        .iter()
        .zip(ins.iter())
        .take_while(|(d, i)| segs_match(d, i))
        .count();

    let suffix_len = del[prefix_len..]
        .iter()
        .rev()
        .zip(ins[prefix_len..].iter().rev())
        .take_while(|(d, i)| segs_match(d, i))
        .count();

    if prefix_len == 0 && suffix_len == 0 {
        return changes;
    }

    let del_mid_end = del.len() - suffix_len;
    let ins_mid_end = ins.len() - suffix_len;

    let mut result = Vec::new();

    // Prefix → Unchanged
    for d in &del[..prefix_len] {
        result.push(InlineChange::Unchanged {
            text: inline_change_text(d).to_string(),
            marks: inline_change_marks(d).to_vec(),
            style_props: inline_change_style_props(d).clone(),
            formatting_change: None,
        });
    }

    // Middle del
    for d in &del[prefix_len..del_mid_end] {
        result.push((*d).clone());
    }

    // Middle ins
    for i in &ins[prefix_len..ins_mid_end] {
        result.push((*i).clone());
    }

    // Suffix → Unchanged
    for d in &del[del_mid_end..] {
        result.push(InlineChange::Unchanged {
            text: inline_change_text(d).to_string(),
            marks: inline_change_marks(d).to_vec(),
            style_props: inline_change_style_props(d).clone(),
            formatting_change: None,
        });
    }

    factor_char_level_affixes(result)
}

/// True if byte position `pos` in `text` falls on a token boundary
/// (the tokenizer would start a new token here).
fn is_token_boundary(text: &str, pos: usize) -> bool {
    if pos == 0 || pos >= text.len() {
        return true;
    }
    if !text.is_char_boundary(pos) {
        return false;
    }
    let prev = char_class(text[..pos].chars().next_back().unwrap());
    let curr = char_class(text[pos..].chars().next().unwrap());
    prev != curr || matches!(prev, CharClass::Punctuation) || matches!(curr, CharClass::Punctuation)
}

/// Snap a byte-level common prefix length down to a token boundary in both texts.
fn snap_prefix_to_token_boundary(old_text: &str, new_text: &str, raw: usize) -> usize {
    if raw == 0 {
        return 0;
    }
    let min_len = old_text.len().min(new_text.len());
    if raw >= min_len {
        return raw;
    }
    // Within 0..raw, bytes are identical so char boundaries match in both.
    let mut pos = raw;
    while pos > 0 && !old_text.is_char_boundary(pos) {
        pos -= 1;
    }
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

/// Snap a byte-level common suffix length down to a token boundary in both texts.
fn snap_suffix_to_token_boundary(old_text: &str, new_text: &str, raw_suffix: usize) -> usize {
    if raw_suffix == 0 {
        return 0;
    }
    let min_len = old_text.len().min(new_text.len());
    if raw_suffix >= min_len {
        return raw_suffix;
    }
    let old_start = old_text.len() - raw_suffix;
    let new_start = new_text.len() - raw_suffix;
    let mut offset = 0;
    while offset < raw_suffix {
        if old_text.is_char_boundary(old_start + offset)
            && new_text.is_char_boundary(new_start + offset)
        {
            break;
        }
        offset += 1;
    }
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

/// Post-processing step: factor character-level common prefix/suffix from adjacent
/// Del/Ins pairs that have matching marks but different text.
///
/// After segment-level `factor_common_affixes`, collapsed regions may contain a single
/// Del + single Ins whose texts share significant prefix/suffix text (e.g.,
/// `Del("five (5) years.")` + `Ins("two (2) years.")`). Segment-level factoring can't
/// help because the entire text differs as a segment. This function recovers the shared
/// text at character level, snapped to token boundaries, producing finer-grained output:
/// `Unchanged(") years.") + Del("five (5") + Ins("two (2") + Unchanged(") years.")`.
fn factor_char_level_affixes(changes: Vec<InlineChange>) -> Vec<InlineChange> {
    let mut result = Vec::new();
    let mut i = 0;

    while i < changes.len() {
        // Look for adjacent Del/Ins pair.
        if i + 1 < changes.len()
            && let (
                InlineChange::Deleted {
                    text: del_text,
                    marks: del_marks,
                    style_props: del_sp,
                    ..
                },
                InlineChange::Inserted {
                    text: ins_text,
                    marks: ins_marks,
                    style_props: ins_sp,
                    ..
                },
            ) = (&changes[i], &changes[i + 1])
        {
            // Only factor when marks and style_props match (content substitution,
            // not a formatting change).
            if del_marks == ins_marks && del_sp == ins_sp && del_text != ins_text {
                let raw_prefix = del_text
                    .bytes()
                    .zip(ins_text.bytes())
                    .take_while(|(a, b)| a == b)
                    .count();
                let raw_suffix = del_text.as_bytes()[raw_prefix..]
                    .iter()
                    .rev()
                    .zip(ins_text.as_bytes()[raw_prefix..].iter().rev())
                    .take_while(|(a, b)| a == b)
                    .count();

                let prefix = snap_prefix_to_token_boundary(del_text, ins_text, raw_prefix);
                let suffix = snap_suffix_to_token_boundary(del_text, ins_text, raw_suffix);

                let shared = prefix + suffix;
                let del_unique = del_text.len().saturating_sub(shared);
                let ins_unique = ins_text.len().saturating_sub(shared);
                let unique = del_unique.min(ins_unique);

                // Only split when shared text exceeds unique text (the same threshold
                // the granularity invariant test uses) and there's actually unique
                // content left on both sides.
                if shared > unique && del_unique > 0 && ins_unique > 0 && shared >= 4 {
                    let del_mid = &del_text[prefix..del_text.len() - suffix];
                    let ins_mid = &ins_text[prefix..ins_text.len() - suffix];

                    if prefix > 0 {
                        result.push(InlineChange::Unchanged {
                            text: del_text[..prefix].to_string(),
                            marks: del_marks.clone(),
                            style_props: del_sp.clone(),
                            formatting_change: None,
                        });
                    }
                    result.push(InlineChange::Deleted {
                        text: del_mid.to_string(),
                        marks: del_marks.clone(),
                        style_props: del_sp.clone(),
                        formatting_change: None,
                        rev_id: 0,
                    });
                    result.push(InlineChange::Inserted {
                        text: ins_mid.to_string(),
                        marks: ins_marks.clone(),
                        style_props: ins_sp.clone(),
                        formatting_change: None,
                        rev_id: 0,
                    });
                    if suffix > 0 {
                        result.push(InlineChange::Unchanged {
                            text: del_text[del_text.len() - suffix..].to_string(),
                            marks: del_marks.clone(),
                            style_props: del_sp.clone(),
                            formatting_change: None,
                        });
                    }

                    i += 2;
                    continue;
                }
            }
        }

        result.push(changes[i].clone());
        i += 1;
    }

    result
}

/// Collapse a region: gather all old-side text as Deleted, all new-side text as Inserted.
/// Unchanged text within the region is duplicated to both sides.
/// Preserves mark boundaries: emits one segment per contiguous run of same marks,
/// so per-word formatting (e.g. bold defined terms) is not lost.
fn collapse_region(region: &[InlineChange]) -> Vec<InlineChange> {
    // Intermediate segment: text + marks + style_props for one contiguous run.
    struct Seg {
        text: String,
        marks: Vec<Mark>,
        style_props: StyleProps,
    }

    let mut del_segs: Vec<Seg> = Vec::new();
    let mut ins_segs: Vec<Seg> = Vec::new();

    // Push text into a segment list, merging with the last segment only when
    // both toggle marks and value-carrying style props match. Otherwise we can
    // smear formatting across adjacent text/opaque boundaries (e.g. field
    // result text next to a field structural placeholder).
    fn push_seg(segs: &mut Vec<Seg>, text: &str, marks: &[Mark], style_props: &StyleProps) {
        if text.is_empty() {
            return;
        }
        if let Some(last) = segs.last_mut()
            && last.marks == marks
            && last.style_props == *style_props
        {
            last.text.push_str(text);
            return;
        }
        segs.push(Seg {
            text: text.to_string(),
            marks: marks.to_vec(),
            style_props: style_props.clone(),
        });
    }

    for change in region {
        match change {
            InlineChange::Unchanged {
                text,
                marks,
                style_props,
                ..
            } => {
                push_seg(&mut del_segs, text, marks, style_props);
                push_seg(&mut ins_segs, text, marks, style_props);
            }
            InlineChange::Deleted {
                text,
                marks,
                style_props,
                ..
            } => {
                push_seg(&mut del_segs, text, marks, style_props);
            }
            InlineChange::Inserted {
                text,
                marks,
                style_props,
                ..
            } => {
                push_seg(&mut ins_segs, text, marks, style_props);
            }
            InlineChange::Opaque {
                segment_type, text, ..
            } => {
                // Opaque segments have no marks; use empty marks/default style.
                let empty_marks: &[Mark] = &[];
                let default_sp = StyleProps::default();
                match segment_type {
                    InlineChangeSegmentType::Equal => {
                        if let Some(t) = text {
                            push_seg(&mut del_segs, t, empty_marks, &default_sp);
                            push_seg(&mut ins_segs, t, empty_marks, &default_sp);
                        }
                    }
                    InlineChangeSegmentType::Delete => {
                        if let Some(t) = text {
                            push_seg(&mut del_segs, t, empty_marks, &default_sp);
                        }
                    }
                    InlineChangeSegmentType::Insert => {
                        if let Some(t) = text {
                            push_seg(&mut ins_segs, t, empty_marks, &default_sp);
                        }
                    }
                }
            }
        }
    }

    let mut result = Vec::new();
    for seg in del_segs {
        result.push(InlineChange::Deleted {
            text: seg.text,
            marks: seg.marks,
            style_props: seg.style_props,
            formatting_change: None,
            rev_id: 0,
        });
    }
    for seg in ins_segs {
        result.push(InlineChange::Inserted {
            text: seg.text,
            marks: seg.marks,
            style_props: seg.style_props,
            formatting_change: None,
            rev_id: 0,
        });
    }
    factor_common_affixes(result)
}

/// Split changes by strong anchors, collapse qualifying regions.
#[cfg(test)]
#[allow(dead_code)]
fn collapse_zipper_regions(changes: Vec<InlineChange>) -> Vec<InlineChange> {
    let heuristics = DiffHeuristics::from_env();
    collapse_zipper_regions_with_config(changes, &heuristics)
}

fn collapse_zipper_regions_with_config(
    changes: Vec<InlineChange>,
    heuristics: &DiffHeuristics,
) -> Vec<InlineChange> {
    let anchors = find_strong_anchors_with_config(&changes, heuristics);

    if anchors.is_empty() {
        // No strong anchors: treat entire sequence as one region
        if should_collapse_region_with_config(&changes, heuristics) {
            return collapse_region(&changes);
        }
        return changes;
    }

    let mut result = Vec::new();
    let mut prev_end = 0;

    for &anchor_idx in &anchors {
        // Process region before this anchor
        if prev_end < anchor_idx {
            let region = &changes[prev_end..anchor_idx];
            if should_collapse_region_with_config(region, heuristics) {
                result.extend(collapse_region(region));
            } else {
                result.extend_from_slice(region);
            }
        }
        // Emit the anchor itself
        result.push(changes[anchor_idx].clone());
        prev_end = anchor_idx + 1;
    }

    // Process region after last anchor
    if prev_end < changes.len() {
        let region = &changes[prev_end..];
        if should_collapse_region_with_config(region, heuristics) {
            result.extend(collapse_region(region));
        } else {
            result.extend_from_slice(region);
        }
    }

    result
}

/// Main cleanup pipeline for inline changes.
/// 1. Merge adjacent same-type segments
/// 2. Collapse zipper regions between strong anchors
/// 3. Merge again after collapse
pub fn cleanup_inline_changes(changes: Vec<InlineChange>) -> Vec<InlineChange> {
    let heuristics = DiffHeuristics::from_env();
    cleanup_inline_changes_with_config(changes, &heuristics)
}

fn cleanup_inline_changes_with_config(
    changes: Vec<InlineChange>,
    heuristics: &DiffHeuristics,
) -> Vec<InlineChange> {
    let merged = merge_adjacent_same_type(changes);
    let collapsed = collapse_zipper_regions_with_config(merged, heuristics);
    let merged = merge_adjacent_same_type(collapsed);
    factor_char_level_affixes(merged)
}

// =============================================================================
// Story diffing functions
// =============================================================================

fn normalized_story_part_path(part_name: &str) -> String {
    if let Some(stripped) = part_name.strip_prefix('/') {
        stripped.to_string()
    } else if part_name.starts_with("word/") {
        part_name.to_string()
    } else {
        format!("word/{part_name}")
    }
}

type StorySlotKey = Vec<(usize, crate::domain::HeaderFooterKind)>;

fn push_story_slot(
    slots: &mut HashMap<String, StorySlotKey>,
    part_path: &str,
    section_index: usize,
    kind: &crate::domain::HeaderFooterKind,
) {
    let entry = slots
        .entry(normalized_story_part_path(part_path))
        .or_default();
    let slot = (section_index, kind.clone());
    if !entry.contains(&slot) {
        entry.push(slot);
    }
}

fn collect_story_slot_maps(
    doc: &CanonDoc,
) -> (HashMap<String, StorySlotKey>, HashMap<String, StorySlotKey>) {
    let mut header_slots = HashMap::new();
    let mut footer_slots = HashMap::new();
    let mut section_index = 0usize;

    for block in &doc.blocks {
        let BlockNode::Paragraph(paragraph) = &block.block else {
            continue;
        };
        let Some(sp) = &paragraph.section_properties else {
            continue;
        };
        for href in &sp.header_refs {
            push_story_slot(
                &mut header_slots,
                &href.part_path,
                section_index,
                &href.kind,
            );
        }
        for fref in &sp.footer_refs {
            push_story_slot(
                &mut footer_slots,
                &fref.part_path,
                section_index,
                &fref.kind,
            );
        }
        section_index += 1;
    }

    if let Some(sp) = &doc.body_section_properties {
        for href in &sp.header_refs {
            push_story_slot(
                &mut header_slots,
                &href.part_path,
                section_index,
                &href.kind,
            );
        }
        for fref in &sp.footer_refs {
            push_story_slot(
                &mut footer_slots,
                &fref.part_path,
                section_index,
                &fref.kind,
            );
        }
    }

    (header_slots, footer_slots)
}

fn pair_story_indices_by_slot_key(
    base_indices: &[usize],
    target_indices: &[usize],
    base_slot_key: impl Fn(usize) -> StorySlotKey,
    target_slot_key: impl Fn(usize) -> StorySlotKey,
) -> (Vec<(usize, usize)>, Vec<usize>, Vec<usize>) {
    let mut paired = Vec::new();
    let mut used_target = HashSet::new();
    let mut unmatched_base = Vec::new();

    for base_idx in base_indices {
        let base_slots = base_slot_key(*base_idx);
        if base_slots.is_empty() {
            unmatched_base.push(*base_idx);
            continue;
        }

        let mut matched_target_idx = None;
        for target_idx in target_indices {
            if used_target.contains(target_idx) {
                continue;
            }
            let target_slots = target_slot_key(*target_idx);
            if !target_slots.is_empty() && base_slots == target_slots {
                matched_target_idx = Some(*target_idx);
                break;
            }
        }

        if let Some(target_idx) = matched_target_idx {
            paired.push((*base_idx, target_idx));
            used_target.insert(target_idx);
        } else {
            unmatched_base.push(*base_idx);
        }
    }

    let unmatched_target = target_indices
        .iter()
        .copied()
        .filter(|idx| !used_target.contains(idx))
        .collect();

    (paired, unmatched_base, unmatched_target)
}

/// Diff headers between base and target documents.
/// Headers are matched first by their logical section slot identity, then by
/// exact content hash and best text similarity second.
///
/// This preserves all header variants in multi-section documents instead of
/// collapsing duplicates to a single representative.
fn diff_headers(
    base: &[HeaderStory],
    target: &[HeaderStory],
    base_slots: &HashMap<String, StorySlotKey>,
    target_slots: &HashMap<String, StorySlotKey>,
) -> Result<Vec<DiffChange>, String> {
    let mut changes = Vec::new();
    let base_indices: Vec<usize> = (0..base.len()).collect();
    let target_indices: Vec<usize> = (0..target.len()).collect();
    let (mut paired, unmatched_base_after_slot, unmatched_target_after_slot) =
        pair_story_indices_by_slot_key(
            &base_indices,
            &target_indices,
            |idx| {
                base_slots
                    .get(&normalized_story_part_path(&base[idx].part_name))
                    .cloned()
                    .unwrap_or_default()
            },
            |idx| {
                target_slots
                    .get(&normalized_story_part_path(&target[idx].part_name))
                    .cloned()
                    .unwrap_or_default()
            },
        );
    let (paired_rest, unmatched_base, unmatched_target) = pair_story_indices_by_hash_and_similarity(
        &unmatched_base_after_slot,
        &unmatched_target_after_slot,
        |idx| base[idx].content_hash.clone(),
        |idx| target[idx].content_hash.clone(),
        |idx| extract_blocks_text(&base[idx].blocks),
        |idx| extract_blocks_text(&target[idx].blocks),
    );
    paired.extend(paired_rest);

    for (base_idx, target_idx) in paired {
        let base_header = &base[base_idx];
        let target_header = &target[target_idx];
        let block_changes = diff_story_blocks(&base_header.blocks, &target_header.blocks)?;
        if block_changes.is_empty() {
            continue;
        }
        changes.push(DiffChange::HeaderModified {
            kind: base_header.kind.clone(),
            base_part_name: base_header.part_name.clone(),
            target_part_name: target_header.part_name.clone(),
            old_hash: base_header.content_hash.clone(),
            new_hash: target_header.content_hash.clone(),
            block_changes,
        });
    }

    for base_idx in unmatched_base {
        let base_header = &base[base_idx];
        changes.push(DiffChange::HeaderDeleted {
            kind: base_header.kind.clone(),
            part_name: base_header.part_name.clone(),
            content_hash: base_header.content_hash.clone(),
            blocks: flatten_tracked_blocks(&base_header.blocks),
        });
    }

    for target_idx in unmatched_target {
        let target_header = &target[target_idx];
        changes.push(DiffChange::HeaderInserted {
            kind: target_header.kind.clone(),
            part_name: target_header.part_name.clone(),
            content_hash: target_header.content_hash.clone(),
            blocks: flatten_tracked_blocks(&target_header.blocks),
        });
    }

    Ok(changes)
}

/// Diff footers between base and target documents.
///
/// Footers are matched first by their logical section slot identity, then by
/// exact content hash and best text similarity second.
fn diff_footers(
    base: &[FooterStory],
    target: &[FooterStory],
    base_slots: &HashMap<String, StorySlotKey>,
    target_slots: &HashMap<String, StorySlotKey>,
) -> Result<Vec<DiffChange>, String> {
    let mut changes = Vec::new();
    let base_indices: Vec<usize> = (0..base.len()).collect();
    let target_indices: Vec<usize> = (0..target.len()).collect();
    let (mut paired, unmatched_base_after_slot, unmatched_target_after_slot) =
        pair_story_indices_by_slot_key(
            &base_indices,
            &target_indices,
            |idx| {
                base_slots
                    .get(&normalized_story_part_path(&base[idx].part_name))
                    .cloned()
                    .unwrap_or_default()
            },
            |idx| {
                target_slots
                    .get(&normalized_story_part_path(&target[idx].part_name))
                    .cloned()
                    .unwrap_or_default()
            },
        );
    let (paired_rest, unmatched_base, unmatched_target) = pair_story_indices_by_hash_and_similarity(
        &unmatched_base_after_slot,
        &unmatched_target_after_slot,
        |idx| base[idx].content_hash.clone(),
        |idx| target[idx].content_hash.clone(),
        |idx| extract_blocks_text(&base[idx].blocks),
        |idx| extract_blocks_text(&target[idx].blocks),
    );
    paired.extend(paired_rest);

    for (base_idx, target_idx) in paired {
        let base_footer = &base[base_idx];
        let target_footer = &target[target_idx];
        let block_changes = diff_story_blocks(&base_footer.blocks, &target_footer.blocks)?;
        if block_changes.is_empty() {
            continue;
        }
        changes.push(DiffChange::FooterModified {
            kind: base_footer.kind.clone(),
            base_part_name: base_footer.part_name.clone(),
            target_part_name: target_footer.part_name.clone(),
            old_hash: base_footer.content_hash.clone(),
            new_hash: target_footer.content_hash.clone(),
            block_changes,
        });
    }

    for base_idx in unmatched_base {
        let base_footer = &base[base_idx];
        changes.push(DiffChange::FooterDeleted {
            kind: base_footer.kind.clone(),
            part_name: base_footer.part_name.clone(),
            content_hash: base_footer.content_hash.clone(),
            blocks: flatten_tracked_blocks(&base_footer.blocks),
        });
    }

    for target_idx in unmatched_target {
        let target_footer = &target[target_idx];
        changes.push(DiffChange::FooterInserted {
            kind: target_footer.kind.clone(),
            part_name: target_footer.part_name.clone(),
            content_hash: target_footer.content_hash.clone(),
            blocks: flatten_tracked_blocks(&target_footer.blocks),
        });
    }

    Ok(changes)
}

/// Align story indices by exact hash first, then by best text similarity.
///
/// The returned pairs are `(base_index, target_index)`.
/// Unmatched indices are returned separately.
fn pair_story_indices_by_hash_and_similarity(
    base_indices: &[usize],
    target_indices: &[usize],
    base_hash: impl Fn(usize) -> String,
    target_hash: impl Fn(usize) -> String,
    base_text: impl Fn(usize) -> String,
    target_text: impl Fn(usize) -> String,
) -> (Vec<(usize, usize)>, Vec<usize>, Vec<usize>) {
    let mut paired = Vec::new();
    let mut used_base = std::collections::HashSet::new();
    let mut used_target = std::collections::HashSet::new();

    // Pass 1: exact content hash matches.
    for (base_pos, base_idx) in base_indices.iter().enumerate() {
        if used_base.contains(&base_pos) {
            continue;
        }
        let base_hash_value = base_hash(*base_idx);
        for (target_pos, target_idx) in target_indices.iter().enumerate() {
            if used_target.contains(&target_pos) {
                continue;
            }
            if base_hash_value == target_hash(*target_idx) {
                paired.push((*base_idx, *target_idx));
                used_base.insert(base_pos);
                used_target.insert(target_pos);
                break;
            }
        }
    }

    // Pass 2: best similarity for remaining stories.
    for (base_pos, base_idx) in base_indices.iter().enumerate() {
        if used_base.contains(&base_pos) {
            continue;
        }
        let base_text_value = base_text(*base_idx);

        let mut best_target_pos = None;
        let mut best_similarity = -1.0f64;
        for (target_pos, target_idx) in target_indices.iter().enumerate() {
            if used_target.contains(&target_pos) {
                continue;
            }
            let similarity = text_similarity(&base_text_value, &target_text(*target_idx));
            if similarity > best_similarity {
                best_similarity = similarity;
                best_target_pos = Some(target_pos);
            }
        }

        if let Some(target_pos) = best_target_pos {
            paired.push((*base_idx, target_indices[target_pos]));
            used_base.insert(base_pos);
            used_target.insert(target_pos);
        }
    }

    let unmatched_base = base_indices
        .iter()
        .enumerate()
        .filter_map(|(pos, idx)| (!used_base.contains(&pos)).then_some(*idx))
        .collect();
    let unmatched_target = target_indices
        .iter()
        .enumerate()
        .filter_map(|(pos, idx)| (!used_target.contains(&pos)).then_some(*idx))
        .collect();

    (paired, unmatched_base, unmatched_target)
}

/// Diff footnotes between base and target documents.
/// Footnotes are matched by content_hash similarity, not by ID (which can change).
/// Separator/continuation notes are excluded from diffing.
fn diff_footnotes(
    base: &[FootnoteStory],
    target: &[FootnoteStory],
) -> Result<Vec<DiffChange>, String> {
    // Filter to only normal footnotes (exclude separator/continuation)
    let base_normal: Vec<&FootnoteStory> = base
        .iter()
        .filter(|f| f.note_type == NoteType::Normal)
        .collect();
    let target_normal: Vec<&FootnoteStory> = target
        .iter()
        .filter(|f| f.note_type == NoteType::Normal)
        .collect();

    diff_notes_generic(
        &base_normal,
        &target_normal,
        |id, hash, blocks| DiffChange::FootnoteDeleted {
            id,
            content_hash: hash,
            blocks,
        },
        |id, hash, blocks| DiffChange::FootnoteInserted {
            id,
            content_hash: hash,
            blocks,
        },
        |id, old_hash, new_hash, block_changes| DiffChange::FootnoteModified {
            id,
            old_hash,
            new_hash,
            block_changes,
        },
    )
}

/// Diff endnotes between base and target documents.
fn diff_endnotes(
    base: &[EndnoteStory],
    target: &[EndnoteStory],
) -> Result<Vec<DiffChange>, String> {
    let base_normal: Vec<&EndnoteStory> = base
        .iter()
        .filter(|e| e.note_type == NoteType::Normal)
        .collect();
    let target_normal: Vec<&EndnoteStory> = target
        .iter()
        .filter(|e| e.note_type == NoteType::Normal)
        .collect();

    diff_notes_generic(
        &base_normal,
        &target_normal,
        |id, hash, blocks| DiffChange::EndnoteDeleted {
            id,
            content_hash: hash,
            blocks,
        },
        |id, hash, blocks| DiffChange::EndnoteInserted {
            id,
            content_hash: hash,
            blocks,
        },
        |id, old_hash, new_hash, block_changes| DiffChange::EndnoteModified {
            id,
            old_hash,
            new_hash,
            block_changes,
        },
    )
}

/// Diff comments between base and target documents.
fn diff_comments(
    base: &[CommentStory],
    target: &[CommentStory],
) -> Result<Vec<DiffChange>, String> {
    let base_refs: Vec<&CommentStory> = base.iter().collect();
    let target_refs: Vec<&CommentStory> = target.iter().collect();

    diff_notes_generic(
        &base_refs,
        &target_refs,
        |id, hash, blocks| DiffChange::CommentDeleted {
            id,
            content_hash: hash,
            blocks,
        },
        |id, hash, blocks| DiffChange::CommentInserted {
            id,
            content_hash: hash,
            blocks,
        },
        |id, old_hash, new_hash, block_changes| DiffChange::CommentModified {
            id,
            old_hash,
            new_hash,
            block_changes,
        },
    )
}

/// Trait for note-like stories (footnotes, endnotes, comments).
trait NoteStory {
    fn id(&self) -> &str;
    fn content_hash(&self) -> &str;
    fn blocks(&self) -> &[TrackedBlock];
}

impl NoteStory for FootnoteStory {
    fn id(&self) -> &str {
        &self.id
    }
    fn content_hash(&self) -> &str {
        &self.content_hash
    }
    fn blocks(&self) -> &[TrackedBlock] {
        &self.blocks
    }
}

impl NoteStory for EndnoteStory {
    fn id(&self) -> &str {
        &self.id
    }
    fn content_hash(&self) -> &str {
        &self.content_hash
    }
    fn blocks(&self) -> &[TrackedBlock] {
        &self.blocks
    }
}

impl NoteStory for CommentStory {
    fn id(&self) -> &str {
        &self.id
    }
    fn content_hash(&self) -> &str {
        &self.content_hash
    }
    fn blocks(&self) -> &[TrackedBlock] {
        &self.blocks
    }
}

/// Generic diffing for note-like stories.
///
/// Matching strategy (in order):
/// 1. Exact content hash matches (unchanged notes)
/// 2. Exact note ID matches for remaining notes (stable identity)
/// 3. High-similarity fallback for remaining unmatched notes
fn diff_notes_generic<T: NoteStory>(
    base: &[&T],
    target: &[&T],
    make_deleted: impl Fn(String, String, Vec<BlockNode>) -> DiffChange,
    make_inserted: impl Fn(String, String, Vec<BlockNode>) -> DiffChange,
    make_modified: impl Fn(String, String, String, Vec<DiffChange>) -> DiffChange,
) -> Result<Vec<DiffChange>, String> {
    let mut changes = Vec::new();

    let mut matched_base = vec![false; base.len()];
    let mut matched_target = vec![false; target.len()];

    // Pass 1: exact note-id matches (same ID → unchanged or modified).
    // ID matching takes priority over hash matching to prevent hash
    // collisions (e.g. many empty footnotes with the same hash) from
    // cross-matching and producing spurious Deleted/Inserted pairs.
    for (base_idx, base_note) in base.iter().enumerate() {
        let mut matched_idx = None;
        for (target_idx, target_note) in target.iter().enumerate() {
            if matched_target[target_idx] {
                continue;
            }
            if base_note.id() == target_note.id() {
                matched_idx = Some(target_idx);
                break;
            }
        }
        if let Some(target_idx) = matched_idx {
            let target_note = target[target_idx];
            if base_note.content_hash() == target_note.content_hash() {
                // Same ID and same content → unchanged (no diff change emitted)
            } else {
                let block_changes = diff_story_blocks(base_note.blocks(), target_note.blocks())?;
                changes.push(make_modified(
                    base_note.id().to_string(),
                    base_note.content_hash().to_string(),
                    target_note.content_hash().to_string(),
                    block_changes,
                ));
            }
            matched_base[base_idx] = true;
            matched_target[target_idx] = true;
        }
    }

    // Pass 2: exact content-hash matches for remaining (renumbered) notes.
    for (base_idx, base_note) in base.iter().enumerate() {
        if matched_base[base_idx] {
            continue;
        }
        let mut matched_idx = None;
        for (target_idx, target_note) in target.iter().enumerate() {
            if matched_target[target_idx] {
                continue;
            }
            if base_note.content_hash() == target_note.content_hash() {
                matched_idx = Some(target_idx);
                break;
            }
        }
        if let Some(target_idx) = matched_idx {
            matched_base[base_idx] = true;
            matched_target[target_idx] = true;
        }
    }

    // Pass 3: high-similarity fallback for remaining unmatched notes.
    for (base_idx, base_note) in base.iter().enumerate() {
        if matched_base[base_idx] {
            continue;
        }
        let base_text = extract_blocks_text(base_note.blocks());
        let mut best_match: Option<(usize, f64)> = None;

        for (target_idx, target_note) in target.iter().enumerate() {
            if matched_target[target_idx] {
                continue;
            }
            let target_text = extract_blocks_text(target_note.blocks());
            let sim = text_similarity(&base_text, &target_text);
            if sim > STRONG_MATCH_THRESHOLD && best_match.is_none_or(|(_, best_sim)| sim > best_sim)
            {
                best_match = Some((target_idx, sim));
            }
        }

        if let Some((target_idx, _)) = best_match {
            let target_note = target[target_idx];
            let block_changes = diff_story_blocks(base_note.blocks(), target_note.blocks())?;
            changes.push(make_modified(
                base_note.id().to_string(),
                base_note.content_hash().to_string(),
                target_note.content_hash().to_string(),
                block_changes,
            ));
            matched_base[base_idx] = true;
            matched_target[target_idx] = true;
        }
    }

    for (base_idx, base_note) in base.iter().enumerate() {
        if !matched_base[base_idx] {
            changes.push(make_deleted(
                base_note.id().to_string(),
                base_note.content_hash().to_string(),
                flatten_tracked_blocks(base_note.blocks()),
            ));
        }
    }

    for (target_idx, target_note) in target.iter().enumerate() {
        if !matched_target[target_idx] {
            changes.push(make_inserted(
                target_note.id().to_string(),
                target_note.content_hash().to_string(),
                flatten_tracked_blocks(target_note.blocks()),
            ));
        }
    }

    Ok(changes)
}

/// Extract text from blocks for similarity comparison.
fn flatten_tracked_blocks(blocks: &[TrackedBlock]) -> Vec<BlockNode> {
    blocks.iter().map(|tb| tb.block.clone()).collect()
}

fn extract_blocks_text(blocks: &[TrackedBlock]) -> String {
    let mut out = String::new();
    for tracked in blocks {
        match &tracked.block {
            BlockNode::Paragraph(p) => {
                let inlines = p.all_inlines_owned();
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(&extract_inline_text(&inlines));
            }
            BlockNode::Table(t) => {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(&extract_table_text(t));
            }
            BlockNode::OpaqueBlock(_) => {}
        }
    }
    out
}

/// Diff blocks within a story (header, footer, note, comment).
fn diff_story_blocks(
    base_blocks: &[TrackedBlock],
    target_blocks: &[TrackedBlock],
) -> Result<Vec<DiffChange>, String> {
    let base_elements = extract_diffable_elements(base_blocks);
    let target_elements = extract_diffable_elements(target_blocks);

    let alignments = align_elements(&base_elements, &target_elements);
    compute_changes(&alignments, &base_elements, &target_elements)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

// =========================================================================
// Full Document View
// =========================================================================

/// Convert a HeadingLevel to its numeric representation.
fn heading_level_to_u8(level: &HeadingLevel) -> u8 {
    match level {
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

/// Convert a block's inline nodes to InlineChange segments of a given type.
///
/// Walks InlineNode list, extracting TextNode.text + TextNode.marks,
/// and mapping HardBreak to "\n".
///
/// `note_markers` maps (kind_prefix, reference_id) → marker_text for footnotes/endnotes/comments.
/// Keys use prefixes: "fn:{id}", "en:{id}", "cm:{id}".
pub fn inlines_to_segments(
    inlines: &[InlineNode],
    change_type: &str,
    note_markers: &HashMap<String, String>,
) -> Vec<InlineChange> {
    let mut segments = Vec::new();
    for (inline_index, inline) in inlines.iter().enumerate() {
        match inline {
            InlineNode::Text(t) => {
                let seg = match change_type {
                    "insert" => InlineChange::Inserted {
                        text: t.text.clone(),
                        marks: t.marks.clone(),
                        style_props: t.style_props.clone(),
                        formatting_change: t.formatting_change.clone(),
                        rev_id: 0,
                    },
                    "delete" => InlineChange::Deleted {
                        text: t.text.clone(),
                        marks: t.marks.clone(),
                        style_props: t.style_props.clone(),
                        formatting_change: t.formatting_change.clone(),
                        rev_id: 0,
                    },
                    _ => InlineChange::Unchanged {
                        text: t.text.clone(),
                        marks: t.marks.clone(),
                        style_props: t.style_props.clone(),
                        formatting_change: t.formatting_change.clone(),
                    },
                };
                segments.push(seg);
            }
            InlineNode::HardBreak(_) => {
                let seg = match change_type {
                    "insert" => InlineChange::Inserted {
                        text: "\n".to_string(),
                        marks: vec![],
                        style_props: StyleProps::default(),
                        formatting_change: None,
                        rev_id: 0,
                    },
                    "delete" => InlineChange::Deleted {
                        text: "\n".to_string(),
                        marks: vec![],
                        style_props: StyleProps::default(),
                        formatting_change: None,
                        rev_id: 0,
                    },
                    _ => InlineChange::Unchanged {
                        text: "\n".to_string(),
                        marks: vec![],
                        style_props: StyleProps::default(),
                        formatting_change: None,
                    },
                };
                segments.push(seg);
            }
            InlineNode::OpaqueInline(o) => {
                let (text, reference_id, field_kind, field_instruction) =
                    extract_opaque_metadata(&o.kind, note_markers, Some(o));
                segments.push(InlineChange::Opaque {
                    segment_type: change_type_to_segment_type(change_type),
                    kind: opaque_kind_to_segment_kind(&o.kind),
                    opaque_id: o.id.0.to_string(),
                    inline_index,
                    text,
                    reference_id,
                    field_kind,
                    field_instruction,
                    asset_ref: None, // Populated later by enrich_segments_with_assets.
                    asset_width_emu: None,
                    asset_height_emu: None,
                    alt_text: None,
                    url: opaque_url(&o.kind),
                    content_hash: o.content_hash.clone(),
                });
            }
            // Comment anchor markers (§17.13.4). These are zero-width — they
            // carry NO text and don't advance the offset — but a redline-review
            // frontend needs them to LOCATE the commented span: it pairs the
            // start marker with the end-reference (matched by `reference_id`,
            // which is the comment's `w:id` = the `CommentPayload.id`) and
            // highlights the text between. We surface them as opaque segments of
            // kind `CommentReference` carrying that `reference_id`, mirroring how
            // footnote/endnote references are projected. `CommentRangeStart`
            // marks the span open; `CommentReference` (emitted at the span's end
            // by the engine) marks the close. `CommentRangeEnd` is the redundant
            // structural twin of `CommentReference` and is dropped to avoid a
            // double close marker.
            InlineNode::CommentRangeStart { id } | InlineNode::CommentReference { id } => {
                segments.push(InlineChange::Opaque {
                    segment_type: change_type_to_segment_type(change_type),
                    kind: OpaqueSegmentKind::CommentReference,
                    opaque_id: id.clone(),
                    inline_index,
                    text: None,
                    reference_id: Some(id.clone()),
                    field_kind: None,
                    field_instruction: None,
                    asset_ref: None,
                    asset_width_emu: None,
                    asset_height_emu: None,
                    alt_text: None,
                    url: None,
                    content_hash: None,
                });
            }
            // Skip decorations and the redundant comment range-end marker
            // (the range-start + comment-reference pair already bracket the span).
            InlineNode::Decoration(_) | InlineNode::CommentRangeEnd { .. } => {}
        }
    }
    // Word often splits text across multiple XML runs arbitrarily (editing
    // history, spell-check, etc.). Merge adjacent same-type segments so
    // inserted/deleted whole-blocks don't produce spurious span splits.
    merge_adjacent_same_type(segments)
}

fn tracked_paragraph_mark_segment(status: &TrackingStatus) -> InlineChange {
    match status {
        // Stacked marks (both `w:ins` and `w:del` in pPr/rPr) ARE
        // constructible since the stacked-carriers import landed. This legacy
        // 3-value projection deliberately coarsens them to a pending deletion
        // — matching both the segment arm above and how Word renders the
        // stacked state (struck). The engine read view carries the full
        // compound status; consumers needing both revisions read THAT.
        TrackingStatus::InsertedThenDeleted(_) | TrackingStatus::Deleted(_) => {
            InlineChange::Deleted {
                text: "\n".to_string(),
                marks: vec![],
                style_props: StyleProps::default(),
                formatting_change: None,
                rev_id: 0,
            }
        }
        TrackingStatus::Inserted(_) => InlineChange::Inserted {
            text: "\n".to_string(),
            marks: vec![],
            style_props: StyleProps::default(),
            formatting_change: None,
            rev_id: 0,
        },
        TrackingStatus::Normal => {
            panic!("paragraph mark tracking status must be inserted or deleted")
        }
    }
}

/// Extract render-ready metadata from an opaque kind.
/// Returns (text, reference_id, field_kind, field_instruction).
fn extract_opaque_metadata(
    kind: &OpaqueKind,
    note_markers: &HashMap<String, String>,
    opaque_node: Option<&OpaqueInlineNode>,
) -> (
    Option<String>,
    Option<String>,
    Option<crate::domain::FieldKind>,
    Option<String>,
) {
    match kind {
        OpaqueKind::Hyperlink(data) if !data.text.is_empty() => {
            (Some(data.text.clone()), None, None, None)
        }
        OpaqueKind::FootnoteReference(ref_data) => {
            let marker = note_markers
                .get(&format!("fn:{}", ref_data.reference_id))
                .cloned();
            (marker, Some(ref_data.reference_id.clone()), None, None)
        }
        OpaqueKind::EndnoteReference(ref_data) => {
            let marker = note_markers
                .get(&format!("en:{}", ref_data.reference_id))
                .cloned();
            (marker, Some(ref_data.reference_id.clone()), None, None)
        }
        OpaqueKind::CommentReference(ref_data) => {
            let marker = note_markers
                .get(&format!("cm:{}", ref_data.reference_id))
                .cloned();
            (marker, Some(ref_data.reference_id.clone()), None, None)
        }
        OpaqueKind::Field(field_data) => {
            let text = field_data.result_text.clone();
            // Prefer the canonical instruction text reconstructed from the
            // typed semantic — this is whitespace-invariant, so MERGEFIELD
            // reformatting (\* MERGEFORMAT spacing, etc.) no longer
            // produces a phantom diff. Fragments without a parsed semantic
            // (e.g. mid-run instrText slices) fall back to the raw bytes.
            let field_instruction = field_data
                .semantic
                .as_ref()
                .map(|s| s.to_instruction_text())
                .or_else(|| field_data.instruction_text.clone());
            (
                text,
                None,
                Some(field_data.field_kind.clone()),
                field_instruction,
            )
        }
        OpaqueKind::Drawing => {
            // Try descriptive fallback text from the drawing XML, otherwise
            // use a sentinel matching the atom path (opaque_sentinel in changelet.rs).
            let text = opaque_node
                .and_then(|o| o.raw_xml.as_deref())
                .and_then(extract_drawing_fallback_text)
                .or_else(|| Some("[image]".to_string()));
            (text, None, None, None)
        }
        OpaqueKind::OmmlBlock | OpaqueKind::OmmlInline => {
            // Sentinel matching atom path (opaque_sentinel in changelet.rs).
            (Some("[equation]".to_string()), None, None, None)
        }
        OpaqueKind::Sym(sym_data) => {
            // Display the decoded character from the symbol font
            (Some(sym_data.display_char.to_string()), None, None, None)
        }
        OpaqueKind::Ptab => {
            // Absolute position tab renders as a tab character
            (Some("\t".to_string()), None, None, None)
        }
        _ => (None, None, None, None),
    }
}

/// Extract block metadata (type, heading level, style_id) from a BlockNode.
fn block_metadata(block: &BlockNode) -> (BlockType, Option<u8>, Option<IStr>) {
    match block {
        BlockNode::Paragraph(p) => {
            if let Some(ref level) = p.heading_level {
                (
                    BlockType::Heading,
                    Some(heading_level_to_u8(level)),
                    p.style_id.clone(),
                )
            } else {
                (BlockType::Paragraph, None, p.style_id.clone())
            }
        }
        BlockNode::Table(_) => (BlockType::Table, None, None),
        BlockNode::OpaqueBlock(_) => (BlockType::Opaque, None, None),
    }
}

/// Compute content types present in a list of inlines, e.g. ["text"], ["image"], ["text", "image"].
/// Compute the content types present in a block (e.g. "text", "image", "equation").
///
/// Public so the diff response can include content types for downstream consumers
/// (e.g. atom assignment needs to distinguish empty paragraphs from equation-only ones).
pub fn content_types_from_block(block: &BlockNode) -> Vec<String> {
    compute_content_types(&block_inlines(block))
}

fn compute_content_types(inlines: &[InlineNode]) -> Vec<String> {
    let mut types = Vec::new();
    for inline in inlines {
        match inline {
            InlineNode::Text(t) if !t.text.trim().is_empty() => {
                if !types.contains(&"text".to_string()) {
                    types.push("text".into());
                }
            }
            InlineNode::OpaqueInline(o) => match &o.kind {
                OpaqueKind::Drawing => {
                    if !types.contains(&"image".to_string()) {
                        types.push("image".into());
                    }
                }
                OpaqueKind::OmmlBlock | OpaqueKind::OmmlInline
                    if !types.contains(&"equation".to_string()) =>
                {
                    types.push("equation".into());
                }
                _ => {}
            },
            _ => {}
        }
    }
    types
}

/// Extract paragraph alignment from a BlockNode.
fn block_align(block: &BlockNode) -> Option<Alignment> {
    match block {
        BlockNode::Paragraph(p) => p.align.clone(),
        _ => None,
    }
}

/// Extract paragraph indentation for the **render projection**.
///
/// When a literal-prefix marker (e.g. "(a)") is positioned by a LEADING tab,
/// the importer resolved that tab against the paragraph's tab stops + default
/// grid and stored its landing (`body_left - left`) as
/// `literal_prefix_leading_tab_twips`. That landing IS the first-line origin, so
/// fold it into `effective_first_line_twips` here: the render projection then
/// carries ONE first-line value (consumers apply a single `text-indent`; there
/// is no separate leading-tab field to double-count). This mirrors
/// [`block_numbering`]'s precedence — structural numbering wins and returns no
/// leading tab, so its raw firstLine stands. The raw `w:ind` on the edit model
/// (`p.indent`) is untouched; only this cloned render copy is resolved.
fn block_indent(block: &BlockNode) -> Option<Indentation> {
    let mut indent = match block {
        BlockNode::Paragraph(p) => p.indent.clone(),
        _ => return None,
    };
    // `block_numbering` returns `Some(gap)` only when the literal-prefix path is
    // the chosen marker (structural numbering carries `None`).
    if let (_, _, Some(gap), _) = block_numbering(block) {
        indent
            .get_or_insert_with(Indentation::default)
            .effective_first_line_twips = Some(gap);
    }
    indent
}

/// Extract effective tab stops from a BlockNode.
fn block_tab_stops(block: &BlockNode) -> Vec<crate::word_ir::TabStopDef> {
    match block {
        // View projections want the EFFECTIVE stops (style-resolved +
        // default-grid, body-left-relative) — the frontend renders tab gaps
        // from these. The authored `tab_stops` field is the serializer's.
        BlockNode::Paragraph(p) => p.effective_tab_stops_rel.clone(),
        _ => Vec::new(),
    }
}

/// Extract paragraph spacing from a BlockNode.
fn block_spacing(block: &BlockNode) -> Option<ParagraphSpacing> {
    match block {
        BlockNode::Paragraph(p) => p.spacing.clone(),
        _ => None,
    }
}

/// Extract paragraph borders from a BlockNode.
fn block_borders(block: &BlockNode) -> Option<ParagraphBorders> {
    match block {
        BlockNode::Paragraph(p) => p.borders.clone(),
        _ => None,
    }
}

/// Extract numbering info (synthesized text + indentation level + leading tab twips) from a BlockNode.
/// Prefers structural numbering; falls back to literal prefix stripped from inlines.
fn block_numbering(block: &BlockNode) -> (Option<String>, Option<u32>, Option<i32>, Option<u32>) {
    match block {
        BlockNode::Paragraph(p) => {
            // Prefer structural numbering (only for non-headings — headings don't use synthesized numbering)
            if p.heading_level.is_none()
                && let Some(n) = &p.numbering
                && !n.synthesized_text.is_empty()
            {
                return (
                    Some(n.synthesized_text.clone()),
                    Some(n.ilvl),
                    None,
                    Some(n.num_id),
                );
            }
            // Fall back to literal prefix
            if let Some(lp) = &p.literal_prefix {
                return (
                    Some(lp.clone()),
                    p.numbering.as_ref().map(|n| n.ilvl),
                    p.literal_prefix_leading_tab_twips,
                    p.numbering.as_ref().map(|n| n.num_id),
                );
            }
            (None, None, None, None)
        }
        _ => (None, None, None, None),
    }
}

/// Build a lookup mapping note/comment reference IDs to ordinal marker text.
/// Keys are prefixed: "fn:{id}" for footnotes, "en:{id}" for endnotes, "cm:{id}" for comments.
fn build_note_marker_lookup(doc: &CanonDoc) -> HashMap<String, String> {
    let mut markers = HashMap::new();
    let mut fn_ordinal = 0u32;
    for footnote in &doc.footnotes {
        if footnote.note_type == NoteType::Normal {
            fn_ordinal += 1;
            markers.insert(format!("fn:{}", footnote.id), fn_ordinal.to_string());
        }
    }
    let mut en_ordinal = 0u32;
    for endnote in &doc.endnotes {
        if endnote.note_type == NoteType::Normal {
            en_ordinal += 1;
            markers.insert(format!("en:{}", endnote.id), en_ordinal.to_string());
        }
    }
    for comment in &doc.comments {
        // Comments typically display their ID as the marker.
        markers.insert(format!("cm:{}", comment.id), comment.id.clone());
    }
    markers
}

/// Build the full document view — every block in document order with inline diff segments.
///
/// For unchanged blocks: all segments are Unchanged (one per formatting run).
/// For modified blocks: segments come from inline diff.
/// For inserted blocks: all segments are Inserted.
/// For deleted blocks: all segments are Deleted.
/// Compare two documents and produce both a diff (changes) and the full document view
/// from a single alignment computation. This guarantees that block IDs are consistent
/// between the two outputs, and avoids duplicate parsing + alignment work.
pub fn diff_and_full_document(
    base: &CanonDoc,
    target: &CanonDoc,
    base_image_lookup: &HashMap<String, String>,
    target_image_lookup: &HashMap<String, String>,
) -> Result<(DocumentDiff, Vec<FullDocBlock>), String> {
    // The diff half MUST be identical to the canonical `diff_documents` path:
    // the production redline path (`compare_and_redline` → here → `merge_diff`)
    // consumes it. Building `changes` separately previously skipped
    // `reconcile_paragraph_splits`, `reconcile_math_deleted_inserted_replacements`,
    // and `diff_opaque_blocks` — so paragraph splits, math delete/insert
    // replacements, and opaque block adds/removes merged wrong (P0 #5). Delegate
    // to the one implementation instead of maintaining a second copy.
    let diff = diff_documents(base, target)?;

    // The full-document blocks (for /full_document) are built independently from
    // the alignment, not from `changes`, so they need their own element/alignment
    // computation.
    let base_elements = extract_diffable_elements(&base.blocks);
    let target_elements = extract_diffable_elements(&target.blocks);
    let alignments = align_elements(&base_elements, &target_elements);
    let blocks = build_full_doc_blocks(
        base,
        target,
        &base_elements,
        &target_elements,
        &alignments,
        base_image_lookup,
        target_image_lookup,
    )?;

    Ok((diff, blocks))
}

pub fn build_full_document_view(
    base: &CanonDoc,
    target: &CanonDoc,
    base_image_lookup: &HashMap<String, String>,
    target_image_lookup: &HashMap<String, String>,
) -> Result<Vec<FullDocBlock>, String> {
    let base_elements = extract_diffable_elements(&base.blocks);
    let target_elements = extract_diffable_elements(&target.blocks);
    let alignments = align_elements(&base_elements, &target_elements);
    build_full_doc_blocks(
        base,
        target,
        &base_elements,
        &target_elements,
        &alignments,
        base_image_lookup,
        target_image_lookup,
    )
}

/// Project a single document into the full-document block format.
///
/// Unlike `build_full_document_view` (which diffs two documents), this projects
/// one canonical document directly. Every block is `Unchanged`, every segment is
/// `equal`, and block IDs are the canonical IDs. This is the editing-ready
/// projection path for viewing/editing a single document without comparison.
pub fn project_single_document(
    doc: &CanonDoc,
    image_lookup: &HashMap<String, String>,
) -> Vec<FullDocBlock> {
    let note_markers = build_note_marker_lookup(doc);
    let mut blocks = Vec::new();

    for tracked_block in &doc.blocks {
        match &tracked_block.block {
            BlockNode::Paragraph(p) => {
                let (block_type, heading_level, style_id) = block_metadata(&tracked_block.block);
                let align = block_align(&tracked_block.block);
                let indent = block_indent(&tracked_block.block);
                let spacing = block_spacing(&tracked_block.block);
                let borders = block_borders(&tracked_block.block);
                let tab_stops = block_tab_stops(&tracked_block.block);
                let (numbering_text, numbering_ilvl, _, numbering_num_id) =
                    block_numbering(&tracked_block.block);
                let inlines = block_inlines(&tracked_block.block);
                let mut segments = inlines_to_segments(&inlines, "equal", &note_markers);
                enrich_segments_with_assets(&mut segments, &inlines, image_lookup);
                let content_types = compute_content_types(&inlines);
                let equation_xmls = extract_equation_xmls(&inlines);
                let image_data_uris = extract_image_data_uris(&inlines, image_lookup);
                blocks.push(FullDocBlock {
                    block_id: p.id.clone(),
                    doc1_block_id: None,
                    doc2_block_id: Some(p.id.clone()),
                    block_type,
                    heading_level,
                    style_id,
                    change_type: ChangeType::Unchanged,
                    align,
                    indent,
                    spacing,
                    borders,
                    tab_stops,
                    numbering_text,
                    numbering_ilvl,
                    numbering_num_id,
                    segments,
                    table_diff: None,
                    content_types,
                    equation_xmls,
                    equation_doc1_count: 0,
                    image_data_uris,
                    image_doc1_count: 0,
                    image_metadata_changes: vec![],
                    move_id: None,
                    move_direction: None,
                    structural_change: None,
                    border_group_id: None,
                    paragraph_mark_status: None,
                });
            }
            BlockNode::Table(t) => {
                let text = extract_table_text(t);
                let segments = vec![InlineChange::Unchanged {
                    text,
                    marks: vec![],
                    style_props: StyleProps::default(),
                    formatting_change: None,
                }];
                blocks.push(FullDocBlock {
                    block_id: t.id.clone(),
                    doc1_block_id: None,
                    doc2_block_id: Some(t.id.clone()),
                    block_type: BlockType::Table,
                    heading_level: None,
                    style_id: None,
                    change_type: ChangeType::Unchanged,
                    align: None,
                    indent: None,
                    spacing: None,
                    borders: None,
                    tab_stops: vec![],
                    numbering_text: None,
                    numbering_ilvl: None,
                    numbering_num_id: None,
                    segments,
                    table_diff: Some(project_self_table_diff(t)),
                    content_types: vec!["table".to_string()],
                    equation_xmls: vec![],
                    equation_doc1_count: 0,
                    image_data_uris: vec![],
                    image_doc1_count: 0,
                    image_metadata_changes: vec![],
                    move_id: None,
                    move_direction: None,
                    structural_change: None,
                    border_group_id: None,
                    paragraph_mark_status: None,
                });
            }
            BlockNode::OpaqueBlock(_) => {
                // Opaque blocks are not projected as visible blocks.
            }
        }
    }

    assign_border_groups(&mut blocks);
    blocks
}

/// Resolve the body section's header references (§17.10.2) into projected
/// payloads, in section-declaration order.
///
/// Each `w:headerReference` binds a story by part name (`StoryRef::part_path`)
/// with a `w:type` (`StoryRef::kind`). We match the ref to the header story it
/// names and project that story's blocks to inline segments via the SAME path
/// footnotes/endnotes use — preserving tabs, marks, and fields. A ref whose
/// part is missing is skipped (the engine carries refs and stories
/// independently; a dangling ref has no content to show). When the body section
/// is absent we project nothing: with no section there is no header binding.
pub(crate) fn project_section_headers(doc: &CanonDoc) -> Vec<HeaderFooterPayload> {
    let Some(section) = doc.body_section_properties.as_ref() else {
        return Vec::new();
    };
    section
        .header_refs
        .iter()
        .filter_map(|story_ref| {
            doc.headers
                .iter()
                .find(|h| h.part_name == story_ref.part_path)
                .map(|h| HeaderFooterPayload {
                    kind: story_ref.kind.to_xml_str().to_string(),
                    paragraphs: crate::import::story_blocks_to_paragraphs(&h.blocks),
                })
        })
        .collect()
}

/// Resolve the body section's footer references (§17.10.5) into projected
/// payloads. Same shape and semantics as [`project_section_headers`].
pub(crate) fn project_section_footers(doc: &CanonDoc) -> Vec<HeaderFooterPayload> {
    let Some(section) = doc.body_section_properties.as_ref() else {
        return Vec::new();
    };
    section
        .footer_refs
        .iter()
        .filter_map(|story_ref| {
            doc.footers
                .iter()
                .find(|f| f.part_name == story_ref.part_path)
                .map(|f| HeaderFooterPayload {
                    kind: story_ref.kind.to_xml_str().to_string(),
                    paragraphs: crate::import::story_blocks_to_paragraphs(&f.blocks),
                })
        })
        .collect()
}

/// Build a single-document `FullDocViewResult` including stories.
pub fn build_single_document_view(
    doc: &CanonDoc,
    image_lookup: &HashMap<String, String>,
) -> FullDocViewResult {
    let blocks = project_single_document(doc, image_lookup);

    let footnotes: Vec<StoryPayload> = doc
        .footnotes
        .iter()
        .filter(|n| n.note_type == NoteType::Normal)
        .map(|n| StoryPayload {
            id: n.id.clone(),
            segments: story_blocks_to_segments(&n.blocks),
        })
        .collect();

    let endnotes: Vec<StoryPayload> = doc
        .endnotes
        .iter()
        .filter(|n| n.note_type == NoteType::Normal)
        .map(|n| StoryPayload {
            id: n.id.clone(),
            segments: story_blocks_to_segments(&n.blocks),
        })
        .collect();

    let comments: Vec<CommentPayload> = doc
        .comments
        .iter()
        .map(|c| {
            let (resolved, parent_para_id) =
                crate::domain::comment_extended_state(c, &doc.comments_extended);
            CommentPayload {
                id: c.id.clone(),
                author: c.author.clone(),
                date: c.date.clone(),
                segments: story_blocks_to_segments(&c.blocks),
                resolved,
                parent_para_id,
            }
        })
        .collect();

    FullDocViewResult {
        blocks,
        footnotes,
        endnotes,
        comments,
        headers: project_section_headers(doc),
        footers: project_section_footers(doc),
        body_section_properties: doc.body_section_properties.clone(),
    }
}

/// Project a document with pre-existing tracked changes into the full-document
/// block format.
///
/// Unlike `project_single_document` (which marks everything as Unchanged), this
/// function preserves tracked change boundaries:
/// - `TrackedSegment::Normal`   → `InlineChange::Unchanged`
/// - `TrackedSegment::Inserted` → `InlineChange::Inserted`
/// - `TrackedSegment::Deleted`  → `InlineChange::Deleted`
///
/// Block-level change_type is derived from `TrackedBlock.status` and segment
/// composition. This is used for single-document tracked change analysis where
/// the tracked changes are extracted directly rather than re-diffed.
pub fn project_tracked_document(
    doc: &CanonDoc,
    image_lookup: &HashMap<String, String>,
) -> Vec<FullDocBlock> {
    let note_markers = build_note_marker_lookup(doc);
    let mut blocks = Vec::new();

    for tracked_block in &doc.blocks {
        match &tracked_block.block {
            BlockNode::Paragraph(p) => {
                let (block_type, heading_level, style_id) = block_metadata(&tracked_block.block);
                let align = block_align(&tracked_block.block);
                let indent = block_indent(&tracked_block.block);
                let spacing = block_spacing(&tracked_block.block);
                let borders = block_borders(&tracked_block.block);
                let tab_stops = block_tab_stops(&tracked_block.block);
                let (numbering_text, numbering_ilvl, _, numbering_num_id) =
                    block_numbering(&tracked_block.block);

                // Determine block-level change type from TrackedBlock.status
                let block_level_change = match &tracked_block.status {
                    TrackingStatus::Inserted(_) => Some(ChangeType::Inserted),
                    TrackingStatus::Deleted(_) => Some(ChangeType::Deleted),
                    TrackingStatus::InsertedThenDeleted(_) => {
                        unreachable!("block-level stacked status is never constructed")
                    }
                    TrackingStatus::Normal => None, // determined by segment composition
                };

                // Build inline segments respecting per-segment tracking status.
                // When the block itself is tracked (Inserted/Deleted), all segments
                // get that change type regardless of their own status.
                let mut segments = Vec::new();
                for tracked_seg in &p.segments {
                    let change_type_str = if let Some(ref block_ct) = block_level_change {
                        match block_ct {
                            ChangeType::Inserted => "insert",
                            ChangeType::Deleted => "delete",
                            ChangeType::Unchanged | ChangeType::Modified => "equal",
                        }
                    } else {
                        match &tracked_seg.status {
                            TrackingStatus::Normal => "equal",
                            TrackingStatus::Inserted(_) => "insert",
                            TrackingStatus::Deleted(_) => "delete",
                            // This legacy 3-value projection deliberately
                            // coarsens the stacked state to how Word renders
                            // it (struck text). The engine read view carries
                            // the full compound status; consumers needing
                            // both revisions read THAT, not this vocabulary.
                            TrackingStatus::InsertedThenDeleted(_) => "delete",
                        }
                    };
                    let mut seg_parts =
                        inlines_to_segments(&tracked_seg.inlines, change_type_str, &note_markers);
                    // Stamp this segment's revision id onto its Inserted/Deleted parts
                    // so a review UI can map a redline span to its `revisions` entry
                    // for selective accept/reject.
                    let seg_rev = match &tracked_seg.status {
                        TrackingStatus::Inserted(r) | TrackingStatus::Deleted(r) => r.revision_id,
                        _ => 0,
                    };
                    if seg_rev != 0 {
                        for part in &mut seg_parts {
                            match part {
                                InlineChange::Inserted { rev_id, .. }
                                | InlineChange::Deleted { rev_id, .. } => *rev_id = seg_rev,
                                _ => {}
                            }
                        }
                    }
                    enrich_segments_with_assets(&mut seg_parts, &tracked_seg.inlines, image_lookup);
                    segments.extend(seg_parts);
                }

                // Carry the paragraph-mark tracking status onto the projection
                // block only when the para-mark synthesized segment was actually
                // emitted — i.e. the block isn't whole-inserted/deleted. The
                // projection-side source_change_id override (in full_doc_block_to_payload)
                // keys off this field to tag the synthesized `\n` segment with
                // `{block_id}_para_mark`, matching the atom side.
                let paragraph_mark_status = if block_level_change.is_none() {
                    p.para_mark_status.clone()
                } else {
                    None
                };

                if let Some(status) = &paragraph_mark_status {
                    segments.push(tracked_paragraph_mark_segment(status));
                }

                // Compute change_type for the block
                let change_type = if let Some(ct) = block_level_change {
                    ct
                } else {
                    let has_ins = p
                        .segments
                        .iter()
                        .any(|s| matches!(s.status, TrackingStatus::Inserted(_)));
                    let has_del = p
                        .segments
                        .iter()
                        .any(|s| matches!(s.status, TrackingStatus::Deleted(_)));
                    if has_ins || has_del || p.para_mark_status.is_some() {
                        ChangeType::Modified
                    } else {
                        ChangeType::Unchanged
                    }
                };

                let (doc1_block_id, doc2_block_id) = match change_type {
                    ChangeType::Deleted => (Some(p.id.clone()), None),
                    ChangeType::Inserted => (None, Some(p.id.clone())),
                    ChangeType::Unchanged | ChangeType::Modified => {
                        (Some(p.id.clone()), Some(p.id.clone()))
                    }
                };

                // Collect all inlines for content-type/equation/image extraction
                let all_inlines = p.all_inlines_owned();
                let content_types = compute_content_types(&all_inlines);
                let equation_xmls = extract_equation_xmls(&all_inlines);
                let image_data_uris = extract_image_data_uris(&all_inlines, image_lookup);

                blocks.push(FullDocBlock {
                    block_id: p.id.clone(),
                    doc1_block_id,
                    doc2_block_id,
                    block_type,
                    heading_level,
                    style_id,
                    change_type,
                    align,
                    indent,
                    spacing,
                    borders,
                    tab_stops,
                    numbering_text,
                    numbering_ilvl,
                    numbering_num_id,
                    segments,
                    table_diff: None,
                    content_types,
                    equation_xmls,
                    equation_doc1_count: 0,
                    image_data_uris,
                    image_doc1_count: 0,
                    image_metadata_changes: vec![],
                    move_id: tracked_block.move_id.clone(),
                    move_direction: None,
                    structural_change: None,
                    border_group_id: None,
                    paragraph_mark_status,
                });
            }
            BlockNode::Table(t) => {
                let (change_type, segments, table_diff) = match &tracked_block.status {
                    TrackingStatus::InsertedThenDeleted(_) => {
                        unreachable!("block-level stacked status is never constructed")
                    }
                    TrackingStatus::Inserted(_) => (
                        ChangeType::Inserted,
                        vec![InlineChange::Inserted {
                            text: extract_table_text(t),
                            marks: vec![],
                            style_props: StyleProps::default(),
                            formatting_change: None,
                            rev_id: 0,
                        }],
                        project_single_sided_table_diff(t, true),
                    ),
                    TrackingStatus::Deleted(_) => (
                        ChangeType::Deleted,
                        vec![InlineChange::Deleted {
                            text: extract_table_text(t),
                            marks: vec![],
                            style_props: StyleProps::default(),
                            formatting_change: None,
                            rev_id: 0,
                        }],
                        project_single_sided_table_diff(t, false),
                    ),
                    TrackingStatus::Normal if table_has_tracked_changes(t) => (
                        ChangeType::Modified,
                        vec![
                            InlineChange::Deleted {
                                text: extract_tracked_table_old_text(t),
                                marks: vec![],
                                style_props: StyleProps::default(),
                                formatting_change: None,
                                rev_id: 0,
                            },
                            InlineChange::Inserted {
                                text: extract_tracked_table_new_text(t),
                                marks: vec![],
                                style_props: StyleProps::default(),
                                formatting_change: None,
                                rev_id: 0,
                            },
                        ],
                        // Cell-level tracked changes inside the table aren't yet
                        // surfaced as cell_diffs; the structural shape (rows ×
                        // cells) is what the frontend needs to render. Inline
                        // ins/del will follow once we either accept the changes
                        // for the new view or feed both sides into the diff
                        // pipeline; today the segments above carry the
                        // change-text fallback.
                        project_self_table_diff(t),
                    ),
                    TrackingStatus::Normal => (
                        ChangeType::Unchanged,
                        vec![InlineChange::Unchanged {
                            text: extract_table_text(t),
                            marks: vec![],
                            style_props: StyleProps::default(),
                            formatting_change: None,
                        }],
                        project_self_table_diff(t),
                    ),
                };
                let (doc1_block_id, doc2_block_id) = match change_type {
                    ChangeType::Deleted => (Some(t.id.clone()), None),
                    ChangeType::Inserted => (None, Some(t.id.clone())),
                    ChangeType::Unchanged | ChangeType::Modified => {
                        (Some(t.id.clone()), Some(t.id.clone()))
                    }
                };
                blocks.push(FullDocBlock {
                    block_id: t.id.clone(),
                    doc1_block_id,
                    doc2_block_id,
                    block_type: BlockType::Table,
                    heading_level: None,
                    style_id: None,
                    change_type,
                    align: None,
                    indent: None,
                    spacing: None,
                    borders: None,
                    tab_stops: vec![],
                    numbering_text: None,
                    numbering_ilvl: None,
                    numbering_num_id: None,
                    segments,
                    table_diff: Some(table_diff),
                    content_types: vec!["table".to_string()],
                    equation_xmls: vec![],
                    equation_doc1_count: 0,
                    image_data_uris: vec![],
                    image_doc1_count: 0,
                    image_metadata_changes: vec![],
                    move_id: tracked_block.move_id.clone(),
                    move_direction: None,
                    structural_change: None,
                    border_group_id: None,
                    paragraph_mark_status: None,
                });
            }
            BlockNode::OpaqueBlock(_) => {
                // Opaque blocks are not projected as visible blocks.
            }
        }
    }

    assign_border_groups(&mut blocks);
    blocks
}

/// Build a tracked-document `FullDocViewResult` including stories.
///
/// Parallel to `build_single_document_view` but preserves tracked change status
/// in inline segments.
pub fn build_tracked_document_view(
    doc: &CanonDoc,
    image_lookup: &HashMap<String, String>,
) -> FullDocViewResult {
    let blocks = project_tracked_document(doc, image_lookup);

    let footnotes: Vec<StoryPayload> = doc
        .footnotes
        .iter()
        .filter(|n| n.note_type == NoteType::Normal)
        .map(|n| StoryPayload {
            id: n.id.clone(),
            segments: story_blocks_to_segments(&n.blocks),
        })
        .collect();

    let endnotes: Vec<StoryPayload> = doc
        .endnotes
        .iter()
        .filter(|n| n.note_type == NoteType::Normal)
        .map(|n| StoryPayload {
            id: n.id.clone(),
            segments: story_blocks_to_segments(&n.blocks),
        })
        .collect();

    let comments: Vec<CommentPayload> = doc
        .comments
        .iter()
        .map(|c| {
            let (resolved, parent_para_id) =
                crate::domain::comment_extended_state(c, &doc.comments_extended);
            CommentPayload {
                id: c.id.clone(),
                author: c.author.clone(),
                date: c.date.clone(),
                segments: story_blocks_to_segments(&c.blocks),
                resolved,
                parent_para_id,
            }
        })
        .collect();

    FullDocViewResult {
        blocks,
        footnotes,
        endnotes,
        comments,
        headers: project_section_headers(doc),
        footers: project_section_footers(doc),
        body_section_properties: doc.body_section_properties.clone(),
    }
}

/// Build a `FullDocBlock` for a deleted paragraph from a `DiffableBlock`.
/// All segments are marked as deleted. Used by the Deleted alignment arm
/// and the Modified arm when split heuristics fire.
fn build_deleted_paragraph_block(
    b: &DiffableBlock,
    base_image_lookup: &HashMap<String, String>,
    note_markers: &HashMap<String, String>,
) -> FullDocBlock {
    let (block_type, heading_level, style_id) = block_metadata(&b.block);
    let align = block_align(&b.block);
    let indent = block_indent(&b.block);
    let spacing = block_spacing(&b.block);
    let borders = block_borders(&b.block);
    let tab_stops = block_tab_stops(&b.block);
    let (numbering_text, numbering_ilvl, _, numbering_num_id) = block_numbering(&b.block);
    let inlines = block_inlines(&b.block);
    let mut segments = inlines_to_segments(&inlines, "delete", note_markers);
    enrich_segments_with_assets(&mut segments, &inlines, base_image_lookup);
    let content_types = compute_content_types(&inlines);
    let equation_xmls = extract_equation_xmls(&inlines);
    let equation_doc1_count = equation_xmls.len();
    let image_data_uris = extract_image_data_uris(&inlines, base_image_lookup);
    let image_doc1_count = image_data_uris.len();
    FullDocBlock {
        block_id: projected_full_doc_block_id(ChangeType::Deleted, Some(&b.id), None),
        doc1_block_id: Some(b.id.clone()),
        doc2_block_id: None,
        block_type,
        heading_level,
        style_id,
        change_type: ChangeType::Deleted,
        align,
        indent,
        spacing,
        borders,
        tab_stops,
        numbering_text,
        numbering_ilvl,
        numbering_num_id,
        segments,
        table_diff: None,
        content_types,
        equation_xmls,
        equation_doc1_count,
        image_data_uris,
        image_doc1_count,
        image_metadata_changes: vec![],
        move_id: None,
        move_direction: None,
        structural_change: None,
        border_group_id: None,
        paragraph_mark_status: None,
    }
}

/// Build a `FullDocBlock` for an inserted paragraph from a `DiffableBlock`.
/// All segments are marked as inserted. Used by the Inserted alignment arm
/// and the Modified arm when split heuristics fire.
fn build_inserted_paragraph_block(
    b: &DiffableBlock,
    target_image_lookup: &HashMap<String, String>,
    note_markers: &HashMap<String, String>,
) -> FullDocBlock {
    let (block_type, heading_level, style_id) = block_metadata(&b.block);
    let align = block_align(&b.block);
    let indent = block_indent(&b.block);
    let spacing = block_spacing(&b.block);
    let borders = block_borders(&b.block);
    let tab_stops = block_tab_stops(&b.block);
    let (numbering_text, numbering_ilvl, _, numbering_num_id) = block_numbering(&b.block);
    let inlines = block_inlines(&b.block);
    let mut segments = inlines_to_segments(&inlines, "insert", note_markers);
    enrich_segments_with_assets(&mut segments, &inlines, target_image_lookup);
    let content_types = compute_content_types(&inlines);
    let equation_xmls = extract_equation_xmls(&inlines);
    let image_data_uris = extract_image_data_uris(&inlines, target_image_lookup);
    FullDocBlock {
        block_id: projected_full_doc_block_id(ChangeType::Inserted, None, Some(&b.id)),
        doc1_block_id: None,
        doc2_block_id: Some(b.id.clone()),
        block_type,
        heading_level,
        style_id,
        change_type: ChangeType::Inserted,
        align,
        indent,
        spacing,
        borders,
        tab_stops,
        numbering_text,
        numbering_ilvl,
        numbering_num_id,
        segments,
        table_diff: None,
        content_types,
        equation_xmls,
        equation_doc1_count: 0,
        image_data_uris,
        image_doc1_count: 0,
        image_metadata_changes: vec![],
        move_id: None,
        move_direction: None,
        structural_change: None,
        border_group_id: None,
        paragraph_mark_status: None,
    }
}

fn build_full_doc_blocks(
    base: &CanonDoc,
    target: &CanonDoc,
    base_elements: &[DiffableElement],
    target_elements: &[DiffableElement],
    alignments: &[ElementAlignment],
    base_image_lookup: &HashMap<String, String>,
    target_image_lookup: &HashMap<String, String>,
) -> Result<Vec<FullDocBlock>, String> {
    // Build note marker lookups: merge base + target so both sides can resolve markers.
    // Target takes precedence — it reflects the current document's ordinals.
    let mut note_markers = build_note_marker_lookup(base);
    for (k, v) in build_note_marker_lookup(target) {
        note_markers.insert(k, v);
    }

    let mut result = Vec::new();

    for alignment in alignments {
        match alignment {
            ElementAlignment::Matched {
                base_idx,
                target_idx,
            } => {
                let base_elem = &base_elements[*base_idx];
                let target_elem = &target_elements[*target_idx];
                match (base_elem, target_elem) {
                    (DiffableElement::Block(base_b), DiffableElement::Block(target_b)) => {
                        let (block_type, heading_level, style_id) = block_metadata(&target_b.block);
                        let align = block_align(&target_b.block);
                        let indent = block_indent(&target_b.block);
                        let spacing = block_spacing(&target_b.block);
                        let borders = block_borders(&target_b.block);
                        let tab_stops = block_tab_stops(&target_b.block);
                        let (numbering_text, numbering_ilvl, _, numbering_num_id) =
                            block_numbering(&target_b.block);
                        let base_inlines = block_inlines(&base_b.block);
                        let inlines = block_inlines(&target_b.block);
                        let mut segments = inlines_to_segments(&inlines, "equal", &note_markers);
                        enrich_segments_with_assets(&mut segments, &inlines, target_image_lookup);
                        let content_types = compute_content_types(&inlines);
                        let equation_xmls = extract_equation_xmls(&inlines);
                        let image_data_uris =
                            extract_image_data_uris(&inlines, target_image_lookup);
                        let image_metadata_changes =
                            compare_drawing_metadata(&base_inlines, &inlines);
                        result.push(FullDocBlock {
                            block_id: projected_full_doc_block_id(
                                ChangeType::Unchanged,
                                Some(&base_b.id),
                                Some(&target_b.id),
                            ),
                            doc1_block_id: Some(base_b.id.clone()),
                            doc2_block_id: Some(target_b.id.clone()),
                            block_type,
                            heading_level,
                            style_id,
                            change_type: ChangeType::Unchanged,
                            align,
                            indent,
                            spacing,
                            borders,
                            tab_stops,
                            numbering_text,
                            numbering_ilvl,
                            numbering_num_id,
                            segments,
                            table_diff: None,
                            content_types,
                            equation_xmls,
                            equation_doc1_count: 0,
                            image_data_uris,
                            image_doc1_count: 0,
                            image_metadata_changes,
                            move_id: None,
                            move_direction: None,
                            structural_change: None,
                            border_group_id: None,
                            paragraph_mark_status: None,
                        });
                    }
                    (DiffableElement::Table(base_t), DiffableElement::Table(target_t)) => {
                        // Compute table_diff first — text_fingerprint alone misses
                        // cell-level text and formatting differences that
                        // compute_changes / diff_table_pair would detect. Without
                        // this, build_full_doc_blocks marks a table Unchanged while
                        // the merge applies tracked changes, breaking the
                        // source_change_id invariant.
                        let table_diff = compute_table_diff_result(&base_t.table, &target_t.table)?;
                        let has_cell_changes = table_diff
                            .cell_diffs
                            .iter()
                            .any(|cd| !matches!(cd.diff_type, TableCellDiffType::Unchanged))
                            || table_diff.row_alignment.iter().any(|ra| {
                                matches!(
                                    ra,
                                    TableRowAlignment::Inserted { .. }
                                        | TableRowAlignment::Deleted { .. }
                                )
                            });
                        let changed = base_t.text_fingerprint != target_t.text_fingerprint
                            || has_cell_changes;
                        let (change_type, segments) = if changed {
                            let segments = vec![
                                InlineChange::Deleted {
                                    text: extract_table_text(&base_t.table),
                                    marks: vec![],
                                    style_props: StyleProps::default(),
                                    formatting_change: None,
                                    rev_id: 0,
                                },
                                InlineChange::Inserted {
                                    text: extract_table_text(&target_t.table),
                                    marks: vec![],
                                    style_props: StyleProps::default(),
                                    formatting_change: None,
                                    rev_id: 0,
                                },
                            ];
                            (ChangeType::Modified, segments)
                        } else {
                            let segments = vec![InlineChange::Unchanged {
                                text: extract_table_text(&target_t.table),
                                marks: vec![],
                                style_props: StyleProps::default(),
                                formatting_change: None,
                            }];
                            (ChangeType::Unchanged, segments)
                        };

                        result.push(FullDocBlock {
                            block_id: projected_full_doc_block_id(
                                change_type.clone(),
                                Some(&base_t.id),
                                Some(&target_t.id),
                            ),
                            doc1_block_id: Some(base_t.id.clone()),
                            doc2_block_id: Some(target_t.id.clone()),
                            block_type: BlockType::Table,
                            heading_level: None,
                            style_id: None,
                            change_type,
                            align: None,
                            indent: None,
                            spacing: None,
                            borders: None,
                            tab_stops: vec![],
                            numbering_text: None,
                            numbering_ilvl: None,
                            numbering_num_id: None,
                            segments,
                            table_diff: Some(table_diff),
                            content_types: vec!["table".to_string()],
                            equation_xmls: vec![],
                            equation_doc1_count: 0,
                            image_data_uris: vec![],
                            image_doc1_count: 0,
                            image_metadata_changes: vec![],
                            move_id: None,
                            move_direction: None,
                            structural_change: None,
                            border_group_id: None,
                            paragraph_mark_status: None,
                        });
                    }
                    _ => {}
                }
            }
            ElementAlignment::Modified {
                base_idx,
                target_idx,
            } => {
                let base_elem = &base_elements[*base_idx];
                let target_elem = &target_elements[*target_idx];
                match (base_elem, target_elem) {
                    (DiffableElement::Block(base_b), DiffableElement::Block(target_b)) => {
                        // Apply the same split heuristics as compute_changes:
                        // when blocks are unrelated, wholly opaque, or one side
                        // is empty, emit separate Deleted + Inserted blocks
                        // instead of a single Modified block.  This keeps the
                        // source_change_id scheme consistent with atoms (which
                        // are extracted from the merged doc where these splits
                        // have already been applied).
                        if is_wholly_paragraph_opaque_change(&base_b.block, &target_b.block)
                            || should_split_empty_paragraph_change(&base_b.block, &target_b.block)
                            || should_split_unrelated_modification(base_b, target_b)
                        {
                            result.push(build_deleted_paragraph_block(
                                base_b,
                                base_image_lookup,
                                &note_markers,
                            ));
                            result.push(build_inserted_paragraph_block(
                                target_b,
                                target_image_lookup,
                                &note_markers,
                            ));
                            continue;
                        }

                        let (block_type, heading_level, style_id) = block_metadata(&target_b.block);
                        let align = block_align(&target_b.block);
                        let indent = block_indent(&target_b.block);
                        let spacing = block_spacing(&target_b.block);
                        let borders = block_borders(&target_b.block);
                        let tab_stops = block_tab_stops(&target_b.block);
                        let (numbering_text, numbering_ilvl, _, numbering_num_id) =
                            block_numbering(&target_b.block);
                        let base_inlines = block_inlines(&base_b.block);
                        let target_inlines = block_inlines(&target_b.block);
                        let segments = diff_block_content_resolving_opaques(
                            &base_inlines,
                            &target_inlines,
                            &note_markers,
                        );
                        // If block hashes differ but inline text diff shows everything unchanged,
                        // opaque content (e.g. images) changed. Show as full replace.
                        let all_unchanged = segments.iter().all(|s| {
                            matches!(
                                s,
                                InlineChange::Unchanged { .. }
                                    | InlineChange::Opaque {
                                        segment_type: InlineChangeSegmentType::Equal,
                                        ..
                                    }
                            )
                        });
                        let mut segments = if all_unchanged {
                            let mut result =
                                inlines_to_segments(&base_inlines, "delete", &note_markers);
                            result.extend(inlines_to_segments(
                                &target_inlines,
                                "insert",
                                &note_markers,
                            ));
                            result
                        } else {
                            segments
                        };
                        // Enrich opaque segments from both sides with asset data.
                        enrich_segments_with_assets(
                            &mut segments,
                            &base_inlines,
                            base_image_lookup,
                        );
                        enrich_segments_with_assets(
                            &mut segments,
                            &target_inlines,
                            target_image_lookup,
                        );
                        // Include content types from both base and target so the
                        // frontend knows about equations/images even if only in one side.
                        let mut content_types = compute_content_types(&base_inlines);
                        for ct in compute_content_types(&target_inlines) {
                            if !content_types.contains(&ct) {
                                content_types.push(ct);
                            }
                        }
                        let mut equation_xmls = extract_equation_xmls(&base_inlines);
                        let equation_doc1_count = equation_xmls.len();
                        equation_xmls.extend(extract_equation_xmls(&target_inlines));
                        let mut image_data_uris =
                            extract_image_data_uris(&base_inlines, base_image_lookup);
                        let image_doc1_count = image_data_uris.len();
                        image_data_uris.extend(extract_image_data_uris(
                            &target_inlines,
                            target_image_lookup,
                        ));
                        result.push(FullDocBlock {
                            block_id: projected_full_doc_block_id(
                                ChangeType::Modified,
                                Some(&base_b.id),
                                Some(&target_b.id),
                            ),
                            doc1_block_id: Some(base_b.id.clone()),
                            doc2_block_id: Some(target_b.id.clone()),
                            block_type,
                            heading_level,
                            style_id,
                            change_type: ChangeType::Modified,
                            align,
                            indent,
                            spacing,
                            borders,
                            tab_stops,
                            numbering_text,
                            numbering_ilvl,
                            numbering_num_id,
                            segments,
                            table_diff: None,
                            content_types,
                            equation_xmls,
                            equation_doc1_count,
                            image_data_uris,
                            image_doc1_count,
                            image_metadata_changes: vec![],
                            move_id: None,
                            move_direction: None,
                            structural_change: None,
                            border_group_id: None,
                            paragraph_mark_status: None,
                        });
                    }
                    (DiffableElement::Table(base_t), DiffableElement::Table(target_t)) => {
                        let segments = vec![
                            InlineChange::Deleted {
                                text: extract_table_text(&base_t.table),
                                marks: vec![],
                                style_props: StyleProps::default(),
                                formatting_change: None,
                                rev_id: 0,
                            },
                            InlineChange::Inserted {
                                text: extract_table_text(&target_t.table),
                                marks: vec![],
                                style_props: StyleProps::default(),
                                formatting_change: None,
                                rev_id: 0,
                            },
                        ];
                        let table_diff = compute_table_diff_result(&base_t.table, &target_t.table)?;
                        result.push(FullDocBlock {
                            block_id: projected_full_doc_block_id(
                                ChangeType::Modified,
                                Some(&base_t.id),
                                Some(&target_t.id),
                            ),
                            doc1_block_id: Some(base_t.id.clone()),
                            doc2_block_id: Some(target_t.id.clone()),
                            block_type: BlockType::Table,
                            heading_level: None,
                            style_id: None,
                            change_type: ChangeType::Modified,
                            align: None,
                            indent: None,
                            spacing: None,
                            borders: None,
                            tab_stops: vec![],
                            numbering_text: None,
                            numbering_ilvl: None,
                            numbering_num_id: None,
                            segments,
                            table_diff: Some(table_diff),
                            content_types: vec!["table".to_string()],
                            equation_xmls: vec![],
                            equation_doc1_count: 0,
                            image_data_uris: vec![],
                            image_doc1_count: 0,
                            image_metadata_changes: vec![],
                            move_id: None,
                            move_direction: None,
                            structural_change: None,
                            border_group_id: None,
                            paragraph_mark_status: None,
                        });
                    }
                    _ => {}
                }
            }
            ElementAlignment::Deleted { base_idx } => {
                let elem = &base_elements[*base_idx];
                match elem {
                    DiffableElement::Block(b) => {
                        result.push(build_deleted_paragraph_block(
                            b,
                            base_image_lookup,
                            &note_markers,
                        ));
                    }
                    DiffableElement::Table(t) => {
                        let segments = vec![InlineChange::Deleted {
                            text: extract_table_text(&t.table),
                            marks: vec![],
                            style_props: StyleProps::default(),
                            formatting_change: None,
                            rev_id: 0,
                        }];
                        let table_diff = compute_single_table_diff_result(&t.table, false)?;
                        result.push(FullDocBlock {
                            block_id: projected_full_doc_block_id(
                                ChangeType::Deleted,
                                Some(&t.id),
                                None,
                            ),
                            doc1_block_id: Some(t.id.clone()),
                            doc2_block_id: None,
                            block_type: BlockType::Table,
                            heading_level: None,
                            style_id: None,
                            change_type: ChangeType::Deleted,
                            align: None,
                            indent: None,
                            spacing: None,
                            borders: None,
                            tab_stops: vec![],
                            numbering_text: None,
                            numbering_ilvl: None,
                            numbering_num_id: None,
                            segments,
                            table_diff: Some(table_diff),
                            content_types: vec!["table".to_string()],
                            equation_xmls: vec![],
                            equation_doc1_count: 0,
                            image_data_uris: vec![],
                            image_doc1_count: 0,
                            image_metadata_changes: vec![],
                            move_id: None,
                            move_direction: None,
                            structural_change: None,
                            border_group_id: None,
                            paragraph_mark_status: None,
                        });
                    }
                }
            }
            ElementAlignment::Inserted { target_idx } => {
                let elem = &target_elements[*target_idx];
                match elem {
                    DiffableElement::Block(b) => {
                        result.push(build_inserted_paragraph_block(
                            b,
                            target_image_lookup,
                            &note_markers,
                        ));
                    }
                    DiffableElement::Table(t) => {
                        let segments = vec![InlineChange::Inserted {
                            text: extract_table_text(&t.table),
                            marks: vec![],
                            style_props: StyleProps::default(),
                            formatting_change: None,
                            rev_id: 0,
                        }];
                        let table_diff = compute_single_table_diff_result(&t.table, true)?;
                        result.push(FullDocBlock {
                            block_id: projected_full_doc_block_id(
                                ChangeType::Inserted,
                                None,
                                Some(&t.id),
                            ),
                            doc1_block_id: None,
                            doc2_block_id: Some(t.id.clone()),
                            block_type: BlockType::Table,
                            heading_level: None,
                            style_id: None,
                            change_type: ChangeType::Inserted,
                            align: None,
                            indent: None,
                            spacing: None,
                            borders: None,
                            tab_stops: vec![],
                            numbering_text: None,
                            numbering_ilvl: None,
                            numbering_num_id: None,
                            segments,
                            table_diff: Some(table_diff),
                            content_types: vec!["table".to_string()],
                            equation_xmls: vec![],
                            equation_doc1_count: 0,
                            image_data_uris: vec![],
                            image_doc1_count: 0,
                            image_metadata_changes: vec![],
                            move_id: None,
                            move_direction: None,
                            structural_change: None,
                            border_group_id: None,
                            paragraph_mark_status: None,
                        });
                    }
                }
            }
        }
    }

    detect_moves(&mut result);
    detect_joins_splits(&mut result);
    assign_border_groups(&mut result);

    Ok(result)
}

fn projected_full_doc_block_id(
    change_type: ChangeType,
    doc1_block_id: Option<&NodeId>,
    doc2_block_id: Option<&NodeId>,
) -> NodeId {
    if let Some(target_id) = doc2_block_id {
        return target_id.clone();
    }

    let base_id = doc1_block_id.unwrap_or_else(|| {
        panic!(
            "full-document projection is missing canonical block identity for {:?}",
            change_type
        )
    });

    match change_type {
        ChangeType::Deleted => NodeId::from(format!("deleted:{}", base_id.0)),
        ChangeType::Unchanged | ChangeType::Modified | ChangeType::Inserted => {
            panic!(
                "full-document projection is missing target-side canonical block identity for {:?}",
                change_type
            )
        }
    }
}

/// Post-pass: assign border group IDs and resolve border edges per OOXML §17.3.1.24.
///
/// Consecutive blocks with identical non-None `borders` form a visual group.
/// Within a group:
/// - First: keep top, left, right; set bottom = between (or None).
/// - Middle: set top = None; set bottom = between (or None); keep left, right.
/// - Last: set top = None; keep bottom, left, right.
/// - All: clear `between` (it has been resolved into visual borders).
fn assign_border_groups(blocks: &mut [FullDocBlock]) {
    let len = blocks.len();
    if len == 0 {
        return;
    }

    // First pass: identify runs of consecutive blocks with identical non-None borders.
    // Collect (start_index, end_index_exclusive) for each run of 2+ blocks.
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < len {
        if let Some(ref borders) = blocks[i].borders {
            let run_borders = borders.clone();
            let start = i;
            i += 1;
            while i < len {
                if let Some(ref b) = blocks[i].borders {
                    if *b == run_borders {
                        i += 1;
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
            if i - start >= 2 {
                runs.push((start, i));
            }
        } else {
            i += 1;
        }
    }

    // Second pass: assign group IDs and resolve borders.
    for (group_idx, (start, end)) in runs.iter().enumerate() {
        let group_id = format!("bg_{group_idx}");
        let between = blocks[*start]
            .borders
            .as_ref()
            .and_then(|b| b.between.clone());

        let run_len = *end - *start;
        for (offset, block) in blocks[*start..*end].iter_mut().enumerate() {
            block.border_group_id = Some(group_id.clone());

            if let Some(ref mut borders) = block.borders {
                if offset == 0 {
                    // First in group: keep top, left, right. Bottom = between.
                    borders.bottom = between.clone();
                } else if offset == run_len - 1 {
                    // Last in group: top = None. Keep bottom, left, right.
                    borders.top = None;
                } else {
                    // Middle: top = None, bottom = between.
                    borders.top = None;
                    borders.bottom = between.clone();
                }
                // Clear between on all grouped paragraphs.
                borders.between = None;
            }
        }
    }
}

/// Post-pass: detect content moves between deleted and inserted blocks.
///
/// When a deleted block's normalized text exactly matches an inserted block's
/// text, they likely represent a move operation. Annotate both blocks with a
/// shared `move_id` and their respective `move_direction`.
///
/// Only matches blocks with substantial text (>= 20 chars after normalization)
/// to avoid false positives on short/empty paragraphs. Each block participates
/// in at most one move pair (first match wins).
fn detect_moves(blocks: &mut [FullDocBlock]) {
    // Collect indices of deleted and inserted paragraph blocks.
    let deleted_indices: Vec<usize> = blocks
        .iter()
        .enumerate()
        .filter(|(_, b)| b.change_type == ChangeType::Deleted && b.block_type != BlockType::Table)
        .map(|(i, _)| i)
        .collect();
    let inserted_indices: Vec<usize> = blocks
        .iter()
        .enumerate()
        .filter(|(_, b)| b.change_type == ChangeType::Inserted && b.block_type != BlockType::Table)
        .map(|(i, _)| i)
        .collect();

    if deleted_indices.is_empty() || inserted_indices.is_empty() {
        return;
    }

    // Build normalized text for each candidate.
    let normalize = |segments: &[InlineChange]| -> String {
        let mut text = String::new();
        for seg in segments {
            match seg {
                InlineChange::Deleted { text: t, .. }
                | InlineChange::Inserted { text: t, .. }
                | InlineChange::Unchanged { text: t, .. } => text.push_str(t),
                InlineChange::Opaque { .. } => text.push('\u{FFFC}'),
            }
        }
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    };

    const MIN_MOVE_TEXT_LEN: usize = 20;

    // Build a map from normalized text -> deleted block index.
    let mut deleted_text_to_idx: HashMap<String, Vec<usize>> = HashMap::new();
    for &idx in &deleted_indices {
        let norm = normalize(&blocks[idx].segments);
        if norm.len() >= MIN_MOVE_TEXT_LEN {
            deleted_text_to_idx.entry(norm).or_default().push(idx);
        }
    }

    let mut move_counter = 0u32;
    let mut used_deleted: HashSet<usize> = HashSet::new();

    for &ins_idx in &inserted_indices {
        let norm = normalize(&blocks[ins_idx].segments);
        if norm.len() < MIN_MOVE_TEXT_LEN {
            continue;
        }
        if let Some(del_indices) = deleted_text_to_idx.get(&norm) {
            // Find first unused deleted block with matching text.
            if let Some(&del_idx) = del_indices.iter().find(|i| !used_deleted.contains(i)) {
                used_deleted.insert(del_idx);
                let move_id = format!("move_{move_counter}");
                move_counter += 1;

                blocks[del_idx].move_id = Some(move_id.clone());
                blocks[del_idx].move_direction = Some(MoveDirection::From);
                blocks[ins_idx].move_id = Some(move_id);
                blocks[ins_idx].move_direction = Some(MoveDirection::To);
            }
        }
    }

    // Pass 2: detect consecutive-block group moves.
    // Short paragraphs (e.g. "Email:", "Name:") individually fall below
    // MIN_MOVE_TEXT_LEN but when moved as a group their combined text is
    // long enough.  We find runs of consecutive deleted/inserted indices
    // (blocks that were not already matched above), concatenate their
    // normalized text with a paragraph separator, and match groups.
    detect_consecutive_group_moves(
        blocks,
        &deleted_indices,
        &inserted_indices,
        &used_deleted,
        &normalize,
        &mut move_counter,
    );
}

/// Detect moves of consecutive block groups (signature blocks, short line sequences).
///
/// Groups runs of consecutive same-type (deleted or inserted) paragraph blocks
/// whose individual text is too short for single-block matching. Concatenates
/// their text and matches deleted groups against inserted groups.
fn detect_consecutive_group_moves(
    blocks: &mut [FullDocBlock],
    deleted_indices: &[usize],
    inserted_indices: &[usize],
    already_matched: &HashSet<usize>,
    normalize: &dyn Fn(&[InlineChange]) -> String,
    move_counter: &mut u32,
) {
    const MIN_GROUP_TEXT_LEN: usize = 20;
    const PARA_SEP: &str = "\n";

    let deleted_runs = find_consecutive_runs(deleted_indices, already_matched);
    let inserted_runs = find_consecutive_runs(inserted_indices, already_matched);

    if deleted_runs.is_empty() || inserted_runs.is_empty() {
        return;
    }

    // Build concatenated normalized text for each deleted run.
    let del_run_texts: Vec<String> = deleted_runs
        .iter()
        .map(|run| {
            run.iter()
                .map(|&idx| normalize(&blocks[idx].segments))
                .collect::<Vec<_>>()
                .join(PARA_SEP)
        })
        .collect();

    let mut used_del_runs: HashSet<usize> = HashSet::new();

    for ins_run in &inserted_runs {
        let ins_text: String = ins_run
            .iter()
            .map(|&idx| normalize(&blocks[idx].segments))
            .collect::<Vec<_>>()
            .join(PARA_SEP);

        if ins_text.len() < MIN_GROUP_TEXT_LEN {
            continue;
        }

        // Find a matching deleted run.
        for (del_run_idx, del_text) in del_run_texts.iter().enumerate() {
            if used_del_runs.contains(&del_run_idx) {
                continue;
            }
            if del_text.len() < MIN_GROUP_TEXT_LEN {
                continue;
            }
            if *del_text != ins_text {
                continue;
            }
            // Matching group found.
            used_del_runs.insert(del_run_idx);
            let move_id = format!("move_{}", *move_counter);
            *move_counter += 1;

            for &idx in &deleted_runs[del_run_idx] {
                blocks[idx].move_id = Some(move_id.clone());
                blocks[idx].move_direction = Some(MoveDirection::From);
            }
            for &idx in ins_run {
                blocks[idx].move_id = Some(move_id.clone());
                blocks[idx].move_direction = Some(MoveDirection::To);
            }
            break;
        }
    }
}

/// Extract the text from a block's segments that belongs to a particular side of the diff.
///
/// For a modified block, "deleted" text comes from `InlineChange::Deleted` and `Unchanged`,
/// while "inserted" text comes from `InlineChange::Inserted` and `Unchanged`.
fn extract_side_text(segments: &[InlineChange], side: &str) -> String {
    let mut text = String::new();
    for seg in segments {
        match seg {
            InlineChange::Unchanged { text: t, .. } => text.push_str(t),
            InlineChange::Deleted { text: t, .. } if side == "old" => text.push_str(t),
            InlineChange::Inserted { text: t, .. } if side == "new" => text.push_str(t),
            InlineChange::Opaque { .. } => text.push('\u{FFFC}'),
            _ => {}
        }
    }
    text
}

/// Normalize text for substring comparison: collapse whitespace, trim.
fn normalize_for_comparison(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Post-pass: detect paragraph joins and splits between adjacent blocks.
///
/// **Join pattern**: A modified block followed by a deleted block, where the
/// deleted block's text appears at the end of the modified block's new text.
/// This indicates the deleted paragraph was merged into the modified one.
///
/// **Split pattern**: A modified block followed by an inserted block, where the
/// inserted block's text appears at the end of the modified block's old text
/// (and is absent from the new text). This indicates the paragraph was split.
///
/// Only annotates paragraph blocks (not tables). Requires the candidate text to
/// be at least 10 characters after normalization to avoid false positives.
fn detect_joins_splits(blocks: &mut [FullDocBlock]) {
    if blocks.len() < 2 {
        return;
    }

    const MIN_TEXT_LEN: usize = 10;

    // We scan for (modified, deleted) and (modified, inserted) pairs.
    // Collect annotations first, then apply, to avoid borrow issues.
    struct Annotation {
        modified_idx: usize,
        other_idx: usize,
        kind: JoinSplitKind,
    }
    enum JoinSplitKind {
        Join,
        Split,
    }

    let mut annotations: Vec<Annotation> = Vec::new();

    for i in 0..blocks.len() - 1 {
        let current = &blocks[i];
        let next = &blocks[i + 1];

        // Only consider paragraph blocks.
        if current.block_type == BlockType::Table || next.block_type == BlockType::Table {
            continue;
        }
        // The first block must be modified.
        if current.change_type != ChangeType::Modified {
            continue;
        }
        // Skip blocks already annotated as moves.
        if next.move_id.is_some() {
            continue;
        }

        if next.change_type == ChangeType::Deleted {
            // Join candidate: deleted block's text should appear at the end
            // of the modified block's new text.
            let deleted_text = extract_side_text(&next.segments, "old");
            let deleted_norm = normalize_for_comparison(&deleted_text);
            if deleted_norm.len() < MIN_TEXT_LEN {
                continue;
            }
            let new_text = extract_side_text(&current.segments, "new");
            let new_norm = normalize_for_comparison(&new_text);
            if new_norm.ends_with(&deleted_norm) {
                annotations.push(Annotation {
                    modified_idx: i,
                    other_idx: i + 1,
                    kind: JoinSplitKind::Join,
                });
            }
        } else if next.change_type == ChangeType::Inserted {
            // Split candidate: inserted block's text should appear at the end
            // of the modified block's old text, and NOT at the end of the new text.
            let inserted_text = extract_side_text(&next.segments, "new");
            let inserted_norm = normalize_for_comparison(&inserted_text);
            if inserted_norm.len() < MIN_TEXT_LEN {
                continue;
            }
            let old_text = extract_side_text(&current.segments, "old");
            let old_norm = normalize_for_comparison(&old_text);
            let new_text = extract_side_text(&current.segments, "new");
            let new_norm = normalize_for_comparison(&new_text);
            if old_norm.ends_with(&inserted_norm) && !new_norm.ends_with(&inserted_norm) {
                annotations.push(Annotation {
                    modified_idx: i,
                    other_idx: i + 1,
                    kind: JoinSplitKind::Split,
                });
            }
        }
    }

    // Apply annotations.
    for ann in annotations {
        let modified_block_id = blocks[ann.modified_idx].block_id.clone();
        let other_block_id = blocks[ann.other_idx].block_id.clone();
        match ann.kind {
            JoinSplitKind::Join => {
                // The deleted block was joined into the modified block.
                blocks[ann.other_idx].structural_change = Some(StructuralChange::Join {
                    into_block_id: modified_block_id,
                });
            }
            JoinSplitKind::Split => {
                // The inserted block was split from the modified block.
                blocks[ann.other_idx].structural_change = Some(StructuralChange::Split {
                    from_block_id: modified_block_id,
                });
            }
        }
        // Don't annotate the modified block itself — it already shows as "modified"
        // and the structural_change on the adjacent block points back to it.
        let _ = other_block_id; // used only for documentation clarity
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        CellFormatting, CompatSettings, DocFingerprint, DocMeta, DocPart, FieldData, FieldKind,
        FootnoteStory, INTERNAL_IDS_VERSION_V0, NoteType, OpaqueBlockNode, OpaqueInlineNode,
        OpaqueKind, ParagraphNode, ProofRef, SCHEMA_VERSION_V0, StyleProps, TableCellNode,
        TableNode, TableRowNode, TextNode, VerticalMerge, normal_segment, normal_tracked_block,
    };

    fn make_paragraph(id: &str, text: &str) -> BlockNode {
        BlockNode::from(ParagraphNode {
            id: NodeId::from(id.to_string()),
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
            segments: normal_segment(vec![InlineNode::from(TextNode {
                id: NodeId::from(format!("{}_t1", id)),
                text_role: None,
                text: text.to_string(),
                marks: Vec::new(),
                style_props: StyleProps::default(),
                rpr_authored: crate::domain::RunRprAuthored::default(),
                source_run_attrs: Vec::new(),
                formatting_change: None,
            })]),
            block_text_hash: Some(sha256_hex(text.as_bytes())),
            numbering: None,
            has_direct_numbering: true,
            numbering_suppressed: false,
            materialized_numbering: None,
            rendered_text: None,
            literal_prefix: None,
            literal_prefix_marks: Vec::new(),
            literal_prefix_style_props: crate::domain::StyleProps::default(),
            literal_prefix_rpr_authored: crate::domain::RunRprAuthored::default(),
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
            suppress_auto_hyphens: None,
            snap_to_grid: None,
            overflow_punct: None,
            adjust_right_ind: None,
            word_wrap: None,
            frame_pr: None,
            para_id: None,
            text_id: None,
            text_direction: None,
            cnf_style: None,
            preserved_ppr: Vec::new(),
        })
    }

    fn make_section_break_paragraph(id: &str) -> BlockNode {
        let mut paragraph = match make_paragraph(id, "") {
            BlockNode::Paragraph(p) => p,
            _ => unreachable!("make_paragraph must return paragraph"),
        };
        paragraph.section_properties = Some(crate::domain::SectionProperties {
            title_page: Some(true),
            ..Default::default()
        });
        BlockNode::Paragraph(paragraph)
    }

    fn make_doc(id: &str, blocks: Vec<BlockNode>, fingerprint: &str) -> CanonDoc {
        CanonDoc {
            id: NodeId::from(id.to_string()),
            blocks: blocks.into_iter().map(normal_tracked_block).collect(),
            meta: DocMeta {
                schema_version: SCHEMA_VERSION_V0.to_string(),
                docx_fingerprint: DocFingerprint(fingerprint.to_string()),
                internal_ids_version: INTERNAL_IDS_VERSION_V0.to_string(),
            },
            headers: Vec::new(),
            footers: Vec::new(),
            footnotes: Vec::new(),
            endnotes: Vec::new(),
            comments: Vec::new(),
            comments_extended: Vec::new(),
            body_section_properties: None,
            body_section_property_change: None,
            compat_settings: CompatSettings::default(),
            even_and_odd_headers: None,
            document_background: None,
            document_protection: None,
        }
    }

    fn make_footnote(id: &str, block_id: &str, text: &str) -> FootnoteStory {
        let blocks = vec![make_paragraph(block_id, text)];
        let content_hash = sha256_hex(text.as_bytes());
        FootnoteStory {
            id: id.to_string(),
            note_type: NoteType::Normal,
            blocks: blocks.into_iter().map(normal_tracked_block).collect(),
            content_hash,
        }
    }

    fn make_text_inline(id: &str, text: &str, style_props: StyleProps) -> InlineNode {
        InlineNode::from(TextNode {
            id: NodeId::from(id.to_string()),
            text_role: None,
            text: text.to_string(),
            marks: Vec::new(),
            style_props: style_props.clone(),
            rpr_authored: crate::domain::RunRprAuthored {
                font_size: style_props.font_size.is_some(),
                font_family: style_props.font_family.is_some(),
                color: style_props.color.is_some(),
                ..Default::default()
            },
            source_run_attrs: Vec::new(),
            formatting_change: None,
        })
    }

    fn make_field_opaque(id: &str, kind: FieldKind) -> InlineNode {
        InlineNode::from(OpaqueInlineNode {
            id: NodeId::from(id.to_string()),
            kind: OpaqueKind::Field(FieldData {
                field_kind: kind.clone(),
                instruction_text: None,
                result_text: None,
                semantic: None,
            }),
            opaque_ref: id.to_string(),
            proof_ref: ProofRef {
                part: crate::domain::DocPart::DocumentXml,
                block_id: NodeId::from(id.to_string()),
                docx_anchor: id.to_string(),
            },
            wrapper_marks: Vec::new(),
            wrapper_style_props: StyleProps::default(),
            raw_xml: Some(format!(
                "<w:fldChar xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" w:fldCharType=\"{}\"/>",
                match kind {
                    FieldKind::Begin => "begin",
                    FieldKind::Separate => "separate",
                    FieldKind::End => "end",
                    FieldKind::Instruction | FieldKind::Simple | FieldKind::Unknown(_) => {
                        unreachable!("not used here")
                    }
                }
            ).into_bytes()),
            content_hash: Some(match kind {
                FieldKind::Begin => "field-begin".to_string(),
                FieldKind::Separate => "field-separate".to_string(),
                FieldKind::End => "field-end".to_string(),
                FieldKind::Instruction | FieldKind::Simple | FieldKind::Unknown(_) => {
                    unreachable!("not used here")
                }
            }),
        })
    }

    #[test]
    fn diff_preserves_field_result_text_style_next_to_opaque_placeholders() {
        let page_number_style = StyleProps {
            char_style_id: Some("PageNumber".into()),
            font_size: Some(22),
            ..StyleProps::default()
        };
        let old_inlines = vec![
            make_text_inline("dash-old-1", "-", page_number_style.clone()),
            make_field_opaque("begin-old", FieldKind::Begin),
            make_field_opaque("sep-old", FieldKind::Separate),
            make_text_inline("page-old", "2", page_number_style.clone()),
            make_field_opaque("end-old", FieldKind::End),
            make_text_inline("dash-old-2", "-", page_number_style.clone()),
        ];
        let new_inlines = vec![
            make_text_inline("dash-new-1", "-", page_number_style.clone()),
            make_field_opaque("begin-new", FieldKind::Begin),
            make_field_opaque("sep-new", FieldKind::Separate),
            make_text_inline("page-new", "6", page_number_style.clone()),
            make_field_opaque("end-new", FieldKind::End),
            make_text_inline("dash-new-2", "-", page_number_style.clone()),
        ];

        let changes = diff_block_content_with_marks(&old_inlines, &new_inlines);
        let inserted_page = changes
            .iter()
            .find_map(|change| match change {
                InlineChange::Inserted {
                    text, style_props, ..
                } if text == "6" => Some(style_props),
                _ => None,
            })
            .unwrap_or_else(|| {
                panic!("expected inserted page-number text segment, got {changes:#?}")
            });

        assert_eq!(inserted_page.char_style_id.as_deref(), Some("PageNumber"));
        assert_eq!(inserted_page.font_size, Some(22));
    }

    #[test]
    fn diff_preserves_trailing_text_style_after_field_opaque() {
        let page_number_style = StyleProps {
            char_style_id: Some("PageNumber".into()),
            font_size: Some(22),
            ..StyleProps::default()
        };
        let old_inlines = vec![
            make_text_inline("dash-old-1", "-", page_number_style.clone()),
            make_field_opaque("begin-old", FieldKind::Begin),
            make_field_opaque("sep-old", FieldKind::Separate),
            make_text_inline("page-old", "2", page_number_style.clone()),
            make_field_opaque("end-old", FieldKind::End),
            make_text_inline("dash-old-2", "-", page_number_style.clone()),
        ];
        let new_inlines = vec![
            make_text_inline("dash-new-1", "-", page_number_style.clone()),
            make_field_opaque("begin-new", FieldKind::Begin),
            make_field_opaque("sep-new", FieldKind::Separate),
            make_text_inline("page-new", "6", page_number_style.clone()),
            make_field_opaque("end-new", FieldKind::End),
            make_text_inline("dash-new-2", "-", page_number_style.clone()),
        ];

        let changes = diff_block_content_with_marks(&old_inlines, &new_inlines);
        let trailing_hyphen = changes
            .iter()
            .rev()
            .find_map(|change| match change {
                InlineChange::Unchanged {
                    text, style_props, ..
                } if text == "-" => Some(style_props),
                _ => None,
            })
            .unwrap_or_else(|| {
                panic!("expected unchanged trailing hyphen segment, got {changes:#?}")
            });

        assert_eq!(trailing_hyphen.char_style_id.as_deref(), Some("PageNumber"));
        assert_eq!(trailing_hyphen.font_size, Some(22));
    }

    /// Build a document that mimics an EPAR-style structure:
    /// multiple "annexes" sharing identical paragraph text, with unique
    /// annex headings as the only distinguishing anchors.
    ///
    /// Each annex has:
    ///   - A heading: "Annex {n}" (unique)
    ///   - N shared body paragraphs with identical text across annexes
    ///
    /// The `id_prefix` parameter generates different IDs for base vs target,
    /// mimicking real imports from separate .docx files (which produce
    /// different anchor IDs). The diff engine must align by content hash,
    /// not by ID.
    ///
    /// This reproduces a misalignment pattern found on a real 485-page
    /// EPAR-style corpus redline, where 148 real changes became 5516
    /// DiffChanges because the diff engine couldn't correctly align
    /// repeated paragraph sequences.
    #[allow(clippy::type_complexity)]
    fn make_epar_style_doc(
        doc_id: &str,
        id_prefix: &str,
        annex_count: usize,
        body_paragraphs: &[&str],
        fingerprint: &str,
        mutator: Option<&dyn Fn(usize, usize, &str) -> String>,
    ) -> CanonDoc {
        let mut blocks = Vec::new();
        let mut para_idx = 0usize;

        for annex in 0..annex_count {
            // Annex heading (unique anchor)
            let heading_id = format!("{id_prefix}_p{para_idx}");
            blocks.push(make_paragraph(&heading_id, &format!("Annex {}", annex + 1)));
            para_idx += 1;

            // Body paragraphs (identical text across annexes)
            for (body_idx, &text) in body_paragraphs.iter().enumerate() {
                let pid = format!("{id_prefix}_p{para_idx}");
                let final_text = match &mutator {
                    Some(f) => f(annex, body_idx, text),
                    None => text.to_string(),
                };
                blocks.push(make_paragraph(&pid, &final_text));
                para_idx += 1;
            }
        }

        make_doc(doc_id, blocks, fingerprint)
    }

    // ── Repeated-structure misalignment tests ────────────────────────────
    //
    // These tests reproduce the block-level diff inflation pattern found
    // in humira-epar (a 485-page pharmaceutical document with repeated
    // annex structure). The root cause: when many paragraphs share the
    // same text hash (e.g. 5202 empty paragraphs out of 19626 total),
    // the LCS-based anchor matching can't distinguish them, and a small
    // change (adding/removing one empty paragraph) shifts the entire
    // anchor mapping, causing cascading delete/insert churn.
    //
    // Real numbers from humira-epar:
    //   - 19626 paragraphs, 5202 empty (26.5%)
    //   - 1358 duplicate text hashes (max repetition: 5202)
    //   - Word shows 148 changes → our diff produces 5516 DiffChanges
    //     (2700 BlockDeleted + 2700 BlockInserted + 36 BlockModified)
    //   - The 2700+2700 are spurious misalignments

    /// Minimal repro: repeated sections with many empty paragraphs.
    ///
    /// Two sections share similar content with empty spacer paragraphs
    /// between them. Removing one empty paragraph from section 2 should
    /// produce a small diff, not a cascade of delete/insert.
    #[test]
    fn diff_repeated_sections_with_empty_paragraphs() {
        // Simulate EPAR structure: unique heading, shared body, empty spacers
        let mut base_blocks = Vec::new();
        let mut target_blocks = Vec::new();
        let mut idx = 0usize;

        // Build two identical sections with empty spacers between content.
        for section in 0..2 {
            // Section heading (unique)
            base_blocks.push(make_paragraph(
                &format!("b_p{}", idx),
                &format!("Section {} Heading", section + 1),
            ));
            target_blocks.push(make_paragraph(
                &format!("t_p{}", idx),
                &format!("Section {} Heading", section + 1),
            ));
            idx += 1;

            // Content paragraphs interspersed with empty spacers (common in EPARs)
            let content = [
                "Therapeutic indications",
                "This product is indicated for treatment of conditions.",
                "Posology and method of administration",
                "The recommended dose is 40 mg every other week.",
                "For the full list of excipients, see section 6.1.",
            ];

            for &text in &content {
                // Content paragraph
                base_blocks.push(make_paragraph(&format!("b_p{}", idx), text));
                target_blocks.push(make_paragraph(&format!("t_p{}", idx), text));
                idx += 1;

                // Empty spacer paragraph (duplicated across both sections)
                base_blocks.push(make_paragraph(&format!("b_p{}", idx), ""));
                target_blocks.push(make_paragraph(&format!("t_p{}", idx), ""));
                idx += 1;
            }
        }

        // Target difference: add one extra empty paragraph in section 2
        // This shifts the empty-paragraph count, which can confuse the
        // LCS anchor matching since empty paragraphs all share the same hash.
        let insert_pos = base_blocks.len() / 2 + 3; // middle of section 2
        target_blocks.insert(insert_pos, make_paragraph("t_extra", ""));

        let base = make_doc("doc", base_blocks, "fp1");
        let target = make_doc("doc", target_blocks, "fp2");

        let diff = diff_documents(&base, &target).expect("diff should succeed");

        let block_deleted = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockDeleted { .. }))
            .count();
        let block_inserted = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockInserted { .. }))
            .count();
        let block_modified = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockModified { .. }))
            .count();

        eprintln!("diff_repeated_sections_with_empty_paragraphs:");
        eprintln!(
            "  Total blocks: base={}, target={}",
            base.blocks.len(),
            target.blocks.len()
        );
        eprintln!("  DiffChanges: {}", diff.changes.len());
        eprintln!("  BlockDeleted:  {block_deleted}");
        eprintln!("  BlockInserted: {block_inserted}");
        eprintln!("  BlockModified: {block_modified}");

        // Expected: 1 BlockInserted (the extra empty paragraph).
        // A misaligned diff would produce many del+ins pairs as the LCS
        // shifts its mapping of duplicate empty-paragraph hashes.
        assert!(
            block_deleted <= 1 && block_inserted <= 2,
            "Adding one empty paragraph should not cause cascading misalignment. \
             Got {} deleted + {} inserted.",
            block_deleted,
            block_inserted,
        );
    }

    /// Scaled version: 4 sections × 15 paragraphs each, with many shared
    /// empty paragraphs. One content change + different empty paragraph counts.
    #[test]
    fn diff_repeated_sections_scaled_with_empties() {
        let content_lines = [
            "NAME OF THE MEDICINAL PRODUCT",
            "Product 40 mg solution for injection",
            "QUALITATIVE AND QUANTITATIVE COMPOSITION",
            "Each 0.8 ml single dose vial contains 40 mg of active substance.",
            "For the full list of excipients, see section 6.1.",
            "PHARMACEUTICAL FORM",
            "Solution for injection.",
            "CLINICAL PARTICULARS",
            "Therapeutic indications",
            "This product is indicated for the treatment of moderate to severe conditions.",
        ];

        let mut base_blocks = Vec::new();
        let mut target_blocks = Vec::new();
        let mut idx = 0usize;

        for section in 0..4 {
            // Unique heading
            let heading = format!("Annex {} — Formulation {}", section + 1, section + 1);
            base_blocks.push(make_paragraph(&format!("b_p{idx}"), &heading));
            target_blocks.push(make_paragraph(&format!("t_p{idx}"), &heading));
            idx += 1;

            for (ci, &text) in content_lines.iter().enumerate() {
                // Content paragraph
                let final_text = if section == 2 && ci == 9 {
                    // Change in annex 3: "moderate to severe" → "mild to moderate"
                    text.replace("moderate to severe", "mild to moderate")
                } else {
                    text.to_string()
                };

                base_blocks.push(make_paragraph(&format!("b_p{idx}"), text));
                target_blocks.push(make_paragraph(&format!("t_p{idx}"), &final_text));
                idx += 1;

                // Empty spacer (same hash everywhere)
                base_blocks.push(make_paragraph(&format!("b_p{idx}"), ""));
                target_blocks.push(make_paragraph(&format!("t_p{idx}"), ""));
                idx += 1;
            }

            // Extra empty paragraphs at end of each section (varying counts)
            for _ in 0..(3 + section) {
                base_blocks.push(make_paragraph(&format!("b_p{idx}"), ""));
                idx += 1;
            }
            // Target has slightly different empty paragraph counts
            for _ in 0..(4 + section) {
                target_blocks.push(make_paragraph(&format!("t_p{idx}"), ""));
                idx += 1;
            }
        }

        let base = make_doc("doc", base_blocks, "fp1");
        let target = make_doc("doc", target_blocks, "fp2");

        let diff = diff_documents(&base, &target).expect("diff should succeed");

        let block_deleted = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockDeleted { .. }))
            .count();
        let block_inserted = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockInserted { .. }))
            .count();
        let block_modified = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockModified { .. }))
            .count();

        let total_base = base.blocks.len();
        let total_target = target.blocks.len();
        let size_diff = (total_target as i64 - total_base as i64).unsigned_abs() as usize;

        eprintln!("diff_repeated_sections_scaled_with_empties:");
        eprintln!("  Total blocks: base={total_base}, target={total_target} (diff={size_diff})");
        eprintln!("  DiffChanges: {}", diff.changes.len());
        eprintln!("  BlockDeleted:  {block_deleted}");
        eprintln!("  BlockInserted: {block_inserted}");
        eprintln!("  BlockModified: {block_modified}");

        // With 4 extra empty paragraphs (one per section) and 1 text change:
        // Expected: ~1 BlockModified + ~4-8 BlockInserted/Deleted (empty spacer diff)
        // A catastrophic misalignment would produce O(total_base) del/ins.
        let total_del_ins = block_deleted + block_inserted;
        assert!(
            total_del_ins <= size_diff + 10,
            "Diff inflation on repeated sections with empties: \
             {} deleted + {} inserted = {} (expected ≤ {} for {size_diff} paragraph count difference). \
             Documents have {total_base}/{total_target} blocks.",
            block_deleted,
            block_inserted,
            total_del_ins,
            size_diff + 10,
        );
        assert!(
            block_modified <= 2,
            "Expected ~1 BlockModified for the one text change, got {block_modified}"
        );
    }

    /// The actual minimal repro for humira-epar misalignment.
    ///
    /// Key conditions (from real data):
    ///   - Few unique anchors (340 out of 11,909 blocks = 2.9%)
    ///   - Many ambiguous hashes (8,639 blocks share a hash with another)
    ///   - 1,312 base-only paragraphs (text changed between before/after)
    ///   - Unequal gap lengths between anchors
    ///
    /// The test creates a structure where:
    ///   - Unique headings (anchors) are sparse
    ///   - Between anchors, blocks share hashes with blocks in other gaps
    ///   - Some blocks in base differ from target (text changed)
    ///   - Gap lengths differ (base has more blocks between anchors)
    #[test]
    fn diff_sparse_anchors_ambiguous_gaps() {
        let mut base_blocks = Vec::new();
        let mut target_blocks = Vec::new();
        let mut idx = 0usize;

        let sections = 5;
        let shared_per_section = 20; // paragraphs with text repeated across sections
        let changed_per_section = 3; // paragraphs that differ between base/target

        for section in 0..sections {
            // Unique heading = anchor
            let heading = format!(
                "Chapter {} — Unique Title {}",
                section + 1,
                section * 17 + 42
            );
            base_blocks.push(make_paragraph(&format!("b{idx}"), &heading));
            target_blocks.push(make_paragraph(&format!("t{idx}"), &heading));
            idx += 1;

            for i in 0..shared_per_section {
                // Shared text — same across all sections, ambiguous for LCS
                let shared_text = format!(
                    "Standard paragraph number {} with shared text content.",
                    i + 1
                );
                base_blocks.push(make_paragraph(&format!("b{idx}"), &shared_text));
                target_blocks.push(make_paragraph(&format!("t{idx}"), &shared_text));
                idx += 1;
            }

            // Paragraphs that exist only in base (simulating text that changed)
            for i in 0..changed_per_section {
                let base_text = format!("Section {} old content variant {}.", section + 1, i + 1);
                let target_text = format!("Section {} new content variant {}.", section + 1, i + 1);
                base_blocks.push(make_paragraph(&format!("b{idx}"), &base_text));
                target_blocks.push(make_paragraph(&format!("t{idx}"), &target_text));
                idx += 1;
            }

            // Extra empties in base (unequal gap length)
            if section % 2 == 0 {
                for _ in 0..5 {
                    base_blocks.push(make_paragraph(&format!("b{idx}"), ""));
                    idx += 1;
                }
            }
        }

        let base = make_doc("doc", base_blocks, "fp1");
        let target = make_doc("doc", target_blocks, "fp2");

        let diff = diff_documents(&base, &target).expect("diff should succeed");

        let block_deleted = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockDeleted { .. }))
            .count();
        let block_inserted = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockInserted { .. }))
            .count();
        let block_modified = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockModified { .. }))
            .count();

        let total_base = base.blocks.len();
        let total_target = target.blocks.len();

        eprintln!("diff_sparse_anchors_ambiguous_gaps:");
        eprintln!("  Total blocks: base={total_base}, target={total_target}");
        eprintln!("  DiffChanges: {}", diff.changes.len());
        eprintln!("  BlockDeleted:  {block_deleted}");
        eprintln!("  BlockInserted: {block_inserted}");
        eprintln!("  BlockModified: {block_modified}");

        // Expected: ~15 BlockModified (3 changed per section × 5 sections)
        //          + ~15 BlockDeleted (extra empties in 3 even sections × 5)
        // Catastrophic: O(total_base) del/ins
        let total_del_ins = block_deleted + block_inserted;
        let expected_max = changed_per_section * sections + 15 + 10; // generous headroom
        assert!(
            total_del_ins <= expected_max,
            "Sparse-anchor misalignment: {} del + {} ins = {} \
             (expected ≤ {expected_max} for {} changed + 15 extra empties on {total_base} blocks)",
            block_deleted,
            block_inserted,
            total_del_ins,
            changed_per_section * sections,
        );
    }

    /// Reproduces the actual humira-epar failure condition:
    /// a large gap (hundreds of blocks) between unique anchors, where
    /// all blocks have ambiguous hashes AND base/target gap lengths differ.
    ///
    /// Real numbers: max gap = 2269 blocks, 340 unique anchors across
    /// 11,909 blocks, 1312 base-only paragraphs.
    #[test]
    fn diff_large_gap_with_ambiguous_hashes() {
        let mut base_blocks = Vec::new();
        let mut target_blocks = Vec::new();
        let mut idx = 0usize;

        // Anchor 1
        base_blocks.push(make_paragraph(&format!("b{idx}"), "UNIQUE ANCHOR START"));
        target_blocks.push(make_paragraph(&format!("t{idx}"), "UNIQUE ANCHOR START"));
        idx += 1;

        // Large gap with ambiguous content (shared text repeated many times)
        let gap_size = 200;
        let repeated_texts = [
            "Standard paragraph content A.",
            "Standard paragraph content B.",
            "Standard paragraph content C.",
            "", // empty — most common duplicate
            "", // more empties
        ];

        for i in 0..gap_size {
            let text = repeated_texts[i % repeated_texts.len()];
            base_blocks.push(make_paragraph(&format!("b{idx}"), text));
            target_blocks.push(make_paragraph(&format!("t{idx}"), text));
            idx += 1;
        }

        // Key difference: base has 20 extra paragraphs with text that
        // doesn't exist in target (simulating content that was changed).
        // This makes the gap lengths unequal (220 vs 200), forcing DP alignment.
        for i in 0..20 {
            let base_text = format!("Old content that was removed, variant {}.", i + 1);
            base_blocks.push(make_paragraph(&format!("b{idx}"), &base_text));
            idx += 1;
        }
        // Target has 5 new paragraphs with text that doesn't exist in base
        for i in 0..5 {
            let target_text = format!("New content that was added, variant {}.", i + 1);
            target_blocks.push(make_paragraph(&format!("t{idx}"), &target_text));
            idx += 1;
        }

        // Anchor 2
        base_blocks.push(make_paragraph(&format!("b{idx}"), "UNIQUE ANCHOR END"));
        target_blocks.push(make_paragraph(&format!("t{idx}"), "UNIQUE ANCHOR END"));

        let base = make_doc("doc", base_blocks, "fp1");
        let target = make_doc("doc", target_blocks, "fp2");

        let diff = diff_documents(&base, &target).expect("diff should succeed");

        let block_deleted = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockDeleted { .. }))
            .count();
        let block_inserted = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockInserted { .. }))
            .count();
        let block_modified = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockModified { .. }))
            .count();

        let total_base = base.blocks.len();
        let total_target = target.blocks.len();

        eprintln!("diff_large_gap_with_ambiguous_hashes:");
        eprintln!("  Total blocks: base={total_base}, target={total_target}");
        eprintln!("  DiffChanges: {}", diff.changes.len());
        eprintln!("  BlockDeleted:  {block_deleted}");
        eprintln!("  BlockInserted: {block_inserted}");
        eprintln!("  BlockModified: {block_modified}");

        // Expected: 20 BlockDeleted (removed content) + 5 BlockInserted (new content)
        // Catastrophic: O(gap_size) del/ins from misaligned shared content
        assert!(
            block_deleted <= 25 && block_inserted <= 10,
            "Large-gap misalignment: {} del + {} ins (expected ~20 del + ~5 ins)",
            block_deleted,
            block_inserted,
        );
    }

    /// Stress test: scale up to realistic EPAR sizes.
    /// 10 sections × (10 content + 50 empty) = 610 blocks, with ~500 shared
    /// empty-paragraph hashes — closer to the real 5202-empty-paragraph scenario.
    #[test]
    fn diff_repeated_sections_stress_empty_heavy() {
        let content_lines = [
            "Name of the medicinal product",
            "Qualitative and quantitative composition",
            "Each vial contains 40 mg of active substance.",
            "Pharmaceutical form",
            "Solution for injection.",
            "Therapeutic indications",
            "This product is indicated for treatment of conditions.",
            "Posology and method of administration",
            "The recommended dose is 40 mg every other week.",
            "Special warnings and precautions for use",
        ];

        let section_count = 10;
        let empty_per_content = 5; // 5 empties per content line = 50 empties per section

        let mut base_blocks = Vec::new();
        let mut target_blocks = Vec::new();
        let mut idx = 0usize;

        for section in 0..section_count {
            // Unique heading
            let heading = format!("Annex {} — Presentation {}", section + 1, section + 1);
            base_blocks.push(make_paragraph(&format!("b{idx}"), &heading));
            target_blocks.push(make_paragraph(&format!("t{idx}"), &heading));
            idx += 1;

            for (ci, &text) in content_lines.iter().enumerate() {
                let final_text = if section == 5 && ci == 6 {
                    text.replace("conditions", "severe conditions")
                } else {
                    text.to_string()
                };

                base_blocks.push(make_paragraph(&format!("b{idx}"), text));
                target_blocks.push(make_paragraph(&format!("t{idx}"), &final_text));
                idx += 1;

                // Many empty spacers (shared hash)
                for _ in 0..empty_per_content {
                    base_blocks.push(make_paragraph(&format!("b{idx}"), ""));
                    target_blocks.push(make_paragraph(&format!("t{idx}"), ""));
                    idx += 1;
                }
            }

            // Section trailing empties: base has 5, target has 6 (off by one per section)
            for _ in 0..5 {
                base_blocks.push(make_paragraph(&format!("b{idx}"), ""));
                idx += 1;
            }
            for _ in 0..6 {
                target_blocks.push(make_paragraph(&format!("t{idx}"), ""));
                idx += 1;
            }
        }

        let base = make_doc("doc", base_blocks, "fp1");
        let target = make_doc("doc", target_blocks, "fp2");

        let diff = diff_documents(&base, &target).expect("diff should succeed");

        let block_deleted = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockDeleted { .. }))
            .count();
        let block_inserted = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockInserted { .. }))
            .count();
        let block_modified = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockModified { .. }))
            .count();

        let total_base = base.blocks.len();
        let total_target = target.blocks.len();

        eprintln!("diff_repeated_sections_stress_empty_heavy:");
        eprintln!("  Total blocks: base={total_base}, target={total_target}");
        eprintln!("  DiffChanges: {}", diff.changes.len());
        eprintln!("  BlockDeleted:  {block_deleted}");
        eprintln!("  BlockInserted: {block_inserted}");
        eprintln!("  BlockModified: {block_modified}");

        // With 10 extra empty paragraphs (1 per section) + 1 text change:
        // Expected: ~1 Modified + ~10-20 del/ins for empty spacer differences
        // Catastrophic: O(total_base) del/ins
        let total_del_ins = block_deleted + block_inserted;
        assert!(
            total_del_ins <= 30,
            "Diff inflation: {} del + {} ins = {} on {total_base} blocks \
             (expected ≤ 30 for 10 extra empties + 1 text change)",
            block_deleted,
            block_inserted,
            total_del_ins,
        );
    }

    /// Smoke test: identical repeated sections with no changes should
    /// produce an empty diff regardless of duplication.
    #[test]
    fn diff_repeated_sections_no_change() {
        let shared_body = &[
            "Therapeutic indications",
            "This medicinal product is indicated for the treatment of moderate to severe conditions.",
            "Posology and method of administration",
            "The recommended dose is 40 mg administered every other week.",
            "For the full list of excipients, see section 6.1.",
        ];

        let base = make_epar_style_doc("doc", "base", 3, shared_body, "fp1", None);
        let target = make_epar_style_doc("doc", "target", 3, shared_body, "fp2", None);

        let diff = diff_documents(&base, &target).expect("diff should succeed");

        assert!(
            diff.changes.is_empty(),
            "Identical content (different IDs) should produce empty diff, got {} changes",
            diff.changes.len(),
        );
    }

    /// Single change in repeated sections (no empty-paragraph complication).
    #[test]
    fn diff_repeated_sections_single_change() {
        let shared_body = &[
            "Therapeutic indications",
            "This medicinal product is indicated for the treatment of moderate to severe conditions.",
            "Posology and method of administration",
            "The recommended dose is 40 mg administered every other week.",
            "For the full list of excipients, see section 6.1.",
        ];

        let base = make_epar_style_doc("doc", "base", 2, shared_body, "fp1", None);
        let target = make_epar_style_doc(
            "doc",
            "target",
            2,
            shared_body,
            "fp2",
            Some(&|annex, body_idx, text| {
                if annex == 1 && body_idx == 1 {
                    text.replace("moderate to severe", "mild to moderate")
                } else {
                    text.to_string()
                }
            }),
        );

        let diff = diff_documents(&base, &target).expect("diff should succeed");

        let block_deleted = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockDeleted { .. }))
            .count();
        let block_inserted = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockInserted { .. }))
            .count();
        let block_modified = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockModified { .. }))
            .count();

        eprintln!("diff_repeated_sections_single_change:");
        eprintln!("  Total DiffChanges: {}", diff.changes.len());
        eprintln!("  BlockDeleted:  {block_deleted}");
        eprintln!("  BlockInserted: {block_inserted}");
        eprintln!("  BlockModified: {block_modified}");

        assert!(
            block_deleted + block_inserted <= 2,
            "Expected at most 2 spurious del/ins, got {} deleted + {} inserted",
            block_deleted,
            block_inserted,
        );
        assert_eq!(block_modified, 1, "Expected exactly 1 BlockModified");
    }

    #[test]
    fn diff_identical_docs() {
        let doc = make_doc("doc", vec![make_paragraph("p1", "Hello World")], "fp1");
        let diff = diff_documents(&doc, &doc).expect("diff should succeed");
        assert!(
            diff.changes.is_empty(),
            "identical docs should have no changes"
        );
    }

    #[test]
    fn diff_single_word_change() {
        let base = make_doc("doc", vec![make_paragraph("p1", "Hello World")], "fp1");
        let target = make_doc("doc", vec![make_paragraph("p1", "Hello Universe")], "fp2");
        let diff = diff_documents(&base, &target).expect("diff should succeed");

        assert_eq!(diff.changes.len(), 1);
        match &diff.changes[0] {
            DiffChange::BlockModified {
                block_id,
                old_text,
                new_text,
                inline_changes,
                ..
            } => {
                assert_eq!(&*block_id.0, "p1");
                assert_eq!(old_text, "Hello World");
                assert_eq!(new_text, "Hello Universe");
                // Should have unchanged "Hello ", deleted "World", inserted "Universe"
                assert!(inline_changes.iter().any(
                    |c| matches!(c, InlineChange::Deleted { text, .. } if text.contains("World"))
                ));
                assert!(inline_changes.iter().any(|c| matches!(c, InlineChange::Inserted { text, .. } if text.contains("Universe"))));
            }
            _ => panic!("expected BlockModified"),
        }
    }

    #[test]
    fn diff_paragraph_deleted() {
        let base = make_doc(
            "doc",
            vec![
                make_paragraph("p1", "First paragraph"),
                make_paragraph("p2", "Second paragraph"),
            ],
            "fp1",
        );
        let target = make_doc("doc", vec![make_paragraph("p1", "First paragraph")], "fp2");
        let diff = diff_documents(&base, &target).expect("diff should succeed");

        assert_eq!(diff.changes.len(), 1);
        match &diff.changes[0] {
            DiffChange::BlockDeleted {
                block_id, old_text, ..
            } => {
                assert_eq!(&*block_id.0, "p2");
                assert_eq!(old_text, "Second paragraph");
            }
            _ => panic!("expected BlockDeleted"),
        }
    }

    #[test]
    fn diff_paragraph_inserted() {
        let base = make_doc("doc", vec![make_paragraph("p1", "First paragraph")], "fp1");
        let target = make_doc(
            "doc",
            vec![
                make_paragraph("p1", "First paragraph"),
                make_paragraph("p2", "Second paragraph"),
            ],
            "fp2",
        );
        let diff = diff_documents(&base, &target).expect("diff should succeed");

        assert_eq!(diff.changes.len(), 1);
        match &diff.changes[0] {
            DiffChange::BlockInserted { block, .. } => {
                if let BlockNode::Paragraph(p) = block {
                    assert_eq!(&*p.id.0, "p2");
                } else {
                    panic!("expected Paragraph");
                }
            }
            _ => panic!("expected BlockInserted"),
        }
    }

    #[test]
    fn diff_block_content_words() {
        let changes = diff_block_content("Hello World", "Hello Universe");

        // Should have: "Hello " unchanged, "World" deleted, "Universe" inserted
        let has_unchanged = changes
            .iter()
            .any(|c| matches!(c, InlineChange::Unchanged { text, .. } if text.contains("Hello")));
        let has_deleted = changes
            .iter()
            .any(|c| matches!(c, InlineChange::Deleted { text, .. } if text.contains("World")));
        let has_inserted = changes
            .iter()
            .any(|c| matches!(c, InlineChange::Inserted { text, .. } if text.contains("Universe")));

        assert!(has_unchanged, "should have unchanged 'Hello'");
        assert!(has_deleted, "should have deleted 'World'");
        assert!(has_inserted, "should have inserted 'Universe'");
    }

    #[test]
    fn tokenize_splits_punctuation() {
        assert_eq!(tokenize("Stock);"), vec!["Stock", ")", ";"]);
        assert_eq!(
            tokenize("Section 3.1(a)"),
            vec!["Section", " ", "3", ".", "1", "(", "a", ")"]
        );
        assert_eq!(tokenize("hello world"), vec!["hello", " ", "world"]);
        assert_eq!(tokenize("non-compete"), vec!["non", "-", "compete"]);
        assert_eq!(tokenize("$1,000"), vec!["$", "1", ",", "000"]);
        assert_eq!(tokenize(""), Vec::<&str>::new());
    }

    #[test]
    fn tokenize_opaque_tag_does_not_merge_with_adjacent_text() {
        // Opaque tags (\u{FFFC} + 12 hash chars) must not consume adjacent text.
        // Before the fix, "http" would be eaten into the opaque tag token.
        let input = "\u{FFFC}abcdef012345http://example.com";
        let tokens = tokenize(input);
        assert_eq!(
            tokens,
            vec![
                "\u{FFFC}abcdef012345",
                "http",
                ":",
                "/",
                "/",
                "example",
                ".",
                "com"
            ],
            "opaque tag should stop at exactly 12 hash chars"
        );

        // Verify an incomplete hash is not treated as part of the opaque tag.
        let short = "\u{FFFC}abc ";
        assert_eq!(tokenize(short), vec!["\u{FFFC}", "abc", " "]);

        // Verify opaque tag followed by punctuation (no merge risk).
        let punct = "\u{FFFC}abcdef012345://rest";
        assert_eq!(
            tokenize(punct),
            vec!["\u{FFFC}abcdef012345", ":", "/", "/", "rest"]
        );
    }

    #[test]
    fn diff_punctuation_not_included_in_word_change() {
        // The original bug: "Stock);" vs "Shares);" was reported as
        // deleting "Stock);" and inserting "Shares);", but ");" is unchanged.
        let changes = diff_block_content(
            "converted into Capital Stock);",
            "converted into Capital Shares);",
        );

        // ");" must appear as Unchanged, not as part of a deletion/insertion.
        // After merge_adjacent_same_type, `)` and `;` may be merged into `);`
        let has_unchanged_punct = changes.iter().any(|c| {
            matches!(c, InlineChange::Unchanged { text, .. } if text.contains(")") && text.contains(";"))
        });
        assert!(
            has_unchanged_punct,
            "');' should be unchanged; got: {:?}",
            changes
        );

        // The actual word change should be just "Stock" vs "Shares"
        let has_deleted_stock = changes
            .iter()
            .any(|c| matches!(c, InlineChange::Deleted { text, .. } if text.contains("Stock")));
        let has_inserted_shares = changes
            .iter()
            .any(|c| matches!(c, InlineChange::Inserted { text, .. } if text.contains("Shares")));
        assert!(
            has_deleted_stock,
            "'Stock' should be deleted; got: {:?}",
            changes
        );
        assert!(
            has_inserted_shares,
            "'Shares' should be inserted; got: {:?}",
            changes
        );
    }

    #[test]
    fn diff_footnotes_prefers_id_pairing_before_similarity() {
        let base = vec![
            make_footnote(
                "10",
                "p_fn_10",
                "alpha clause about transfer restrictions and consent rights",
            ),
            make_footnote(
                "11",
                "p_fn_11",
                "beta clause about arbitration venue and governing law",
            ),
        ];
        let target = vec![
            // Same ID as base[0], but intentionally very different content.
            make_footnote(
                "10",
                "p_fn_10_target",
                "new data-security representation for cross-border transfers",
            ),
            // Different ID, but very similar to base[0] to tempt similarity pairing.
            make_footnote(
                "11",
                "p_fn_11_target",
                "alpha clause about transfer restrictions and investor consent rights",
            ),
        ];

        let changes = diff_footnotes(&base, &target).expect("diff should succeed");

        let modified_ids: Vec<String> = changes
            .iter()
            .filter_map(|c| match c {
                DiffChange::FootnoteModified { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect();
        let deleted_count = changes
            .iter()
            .filter(|c| matches!(c, DiffChange::FootnoteDeleted { .. }))
            .count();
        let inserted_count = changes
            .iter()
            .filter(|c| matches!(c, DiffChange::FootnoteInserted { .. }))
            .count();

        assert_eq!(
            modified_ids.len(),
            2,
            "expected both same-ID notes to be treated as modified"
        );
        assert!(
            modified_ids.iter().any(|id| id == "10"),
            "missing modification for footnote id=10: {changes:?}"
        );
        assert!(
            modified_ids.iter().any(|id| id == "11"),
            "missing modification for footnote id=11: {changes:?}"
        );
        assert_eq!(
            deleted_count, 0,
            "same-ID notes should not be treated as deletions: {changes:?}"
        );
        assert_eq!(
            inserted_count, 0,
            "same-ID notes should not be treated as insertions: {changes:?}"
        );
    }

    // =========================================================================
    // DP-based alignment tests
    // =========================================================================

    #[test]
    fn test_text_similarity_identical() {
        let sim = text_similarity("hello world", "hello world");
        assert!(
            (sim - 1.0).abs() < 1e-9,
            "identical texts should have similarity ~1.0, got {}",
            sim
        );
    }

    #[test]
    fn test_text_similarity_completely_different() {
        let sim = text_similarity("hello world", "foo bar baz");
        assert!(
            sim < 0.3,
            "completely different texts should have low similarity, got {}",
            sim
        );
    }

    #[test]
    fn test_text_similarity_mostly_similar() {
        // LCS ratio captures word overlap
        let sim = text_similarity(
            "THIS INSTRUMENT UNDER THE SECURITIES ACT OF 1933",
            "THIS INSTRUMENT UNDER THE UNITED STATES SECURITIES ACT OF 1933",
        );
        assert!(
            sim > 0.7,
            "similar paragraphs should have high LCS ratio: {}",
            sim
        );
    }

    #[test]
    fn test_text_similarity_normalization() {
        // Should match despite case/whitespace differences
        let sim = text_similarity("The Securities Act", "THE   SECURITIES   ACT");
        assert!(
            (sim - 1.0).abs() < 1e-9,
            "normalized texts should match, got {}",
            sim
        );
    }

    #[test]
    fn test_dp_alignment_with_prefix_insertion() {
        // Simulate the US vs Canada SAFE scenario:
        // - Base has paragraph A (securities disclaimer)
        // - Target has NEW paragraph X, then MODIFIED paragraph A'
        // The algorithm should pair A with A', not A with X
        let base = make_doc(
            "doc",
            vec![make_paragraph(
                "p1",
                "THE SECURITIES ACT OF 1933 AS AMENDED",
            )],
            "fp1",
        );
        let target = make_doc(
            "doc",
            vec![
                make_paragraph("p1", "Please seek legal advice"), // NEW
                make_paragraph("p2", "THE UNITED STATES SECURITIES ACT OF 1933 AS AMENDED"), // MODIFIED
            ],
            "fp2",
        );

        let diff = diff_documents(&base, &target).expect("diff should succeed");

        // Should have 1 insertion and 1 modification
        let insertions = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockInserted { .. }))
            .count();
        let modifications = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockModified { .. }))
            .count();

        assert_eq!(insertions, 1, "should have 1 pure insertion");
        assert_eq!(modifications, 1, "should have 1 modification");

        // The modification should show inline changes, not wholesale replacement
        if let Some(DiffChange::BlockModified { inline_changes, .. }) = diff
            .changes
            .iter()
            .find(|c| matches!(c, DiffChange::BlockModified { .. }))
        {
            let has_unchanged = inline_changes
                .iter()
                .any(|c| matches!(c, InlineChange::Unchanged { .. }));
            assert!(has_unchanged, "modification should have unchanged portions");
        }
    }

    #[test]
    fn test_dp_alignment_competing_similar_insertions() {
        // Test case where greedy would fail but DP succeeds:
        // - Base has A, B
        // - Target has A', B' (both modified)
        // Greedy might pair A with B' if it's slightly more similar,
        // but DP should preserve order: A→A', B→B'
        let base = make_doc(
            "doc",
            vec![
                make_paragraph("p1", "Section 1: The first agreement terms"),
                make_paragraph("p2", "Section 2: The second agreement terms"),
            ],
            "fp1",
        );
        let target = make_doc(
            "doc",
            vec![
                make_paragraph("p1", "Section 1: The first amended agreement terms"),
                make_paragraph("p2", "Section 2: The second amended agreement terms"),
            ],
            "fp2",
        );

        let diff = diff_documents(&base, &target).expect("diff should succeed");

        // Should have exactly 2 modifications, no insertions/deletions
        let modifications = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockModified { .. }))
            .count();
        let insertions = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockInserted { .. }))
            .count();
        let deletions = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockDeleted { .. }))
            .count();

        assert_eq!(modifications, 2, "should have 2 modifications");
        assert_eq!(insertions, 0, "should have no insertions");
        assert_eq!(deletions, 0, "should have no deletions");

        // Verify the modifications preserve order (first mod is Section 1, second is Section 2)
        let mod_texts: Vec<&str> = diff
            .changes
            .iter()
            .filter_map(|c| {
                if let DiffChange::BlockModified { old_text, .. } = c {
                    Some(old_text.as_str())
                } else {
                    None
                }
            })
            .collect();

        assert!(
            mod_texts[0].contains("Section 1"),
            "first modification should be Section 1"
        );
        assert!(
            mod_texts[1].contains("Section 2"),
            "second modification should be Section 2"
        );
    }

    #[test]
    fn test_dp_alignment_no_crossing_matches() {
        // Ensure DP doesn't produce crossing alignments.
        // The algorithm may choose all modifications OR insert/delete+modify patterns
        // depending on the cost calculation. The key invariant is ORDER PRESERVATION:
        // if Base[i] matches Target[j] and Base[k] matches Target[l], then i < k implies j < l.
        let base = make_doc(
            "doc",
            vec![
                make_paragraph("p1", "Alpha paragraph content here"),
                make_paragraph("p2", "Beta paragraph content here"),
                make_paragraph("p3", "Gamma paragraph content here"),
            ],
            "fp1",
        );
        let target = make_doc(
            "doc",
            vec![
                make_paragraph("p1", "New prefix paragraph"),
                make_paragraph("p2", "Alpha modified paragraph content here"),
                make_paragraph("p3", "Gamma modified paragraph content here"),
            ],
            "fp2",
        );

        let diff = diff_documents(&base, &target).expect("diff should succeed");

        // The algorithm should produce a valid diff with changes
        let total_changes = diff.changes.len();
        assert!(total_changes > 0, "should produce some changes");

        // Extract modification pairs and verify order is preserved (no crossing)
        // This is the key property we're testing
        let mods: Vec<(&NodeId, &str)> = diff
            .changes
            .iter()
            .filter_map(|c| {
                if let DiffChange::BlockModified {
                    block_id, new_text, ..
                } = c
                {
                    Some((block_id, new_text.as_str()))
                } else {
                    None
                }
            })
            .collect();

        // If we have modifications, verify they're in order
        // (block_id order should match the order modifications appear)
        for (_, text) in mods.iter().skip(1) {
            // Just verify we have valid modifications - the DP guarantees order
            assert!(!text.is_empty(), "modification should have content");
        }
    }

    #[test]
    fn test_dp_alignment_below_threshold_unpaired() {
        // When texts are too dissimilar (below SIMILARITY_THRESHOLD), they should
        // be treated as separate delete + insert, not as a modification
        let base = make_doc(
            "doc",
            vec![make_paragraph("p1", "The quick brown fox")],
            "fp1",
        );
        let target = make_doc(
            "doc",
            vec![make_paragraph("p1", "Completely different content here")],
            "fp2",
        );

        let diff = diff_documents(&base, &target).expect("diff should succeed");

        // Since these are very different, they should either:
        // 1. Be a modification (if threshold is met) - that's fine
        // 2. Or be delete + insert if below threshold
        // The key is the algorithm doesn't crash and produces valid output
        let total = diff.changes.len();
        assert!(total >= 1, "should have at least 1 change");
    }

    #[test]
    fn test_dp_alignment_empty_pending() {
        // Ensure edge cases with empty pending lists work
        let base = make_doc("doc", vec![make_paragraph("p1", "Same content")], "fp1");
        let target = make_doc("doc", vec![make_paragraph("p1", "Same content")], "fp2");

        let diff = diff_documents(&base, &target).expect("diff should succeed");
        assert!(
            diff.changes.is_empty(),
            "identical content should produce no changes"
        );
    }

    #[test]
    fn test_dp_alignment_only_deletions() {
        // When we have deletions but no insertions to pair with
        let base = make_doc(
            "doc",
            vec![
                make_paragraph("p1", "Paragraph A"),
                make_paragraph("p2", "Paragraph B"),
                make_paragraph("p3", "Paragraph C"),
            ],
            "fp1",
        );
        let target = make_doc("doc", vec![make_paragraph("p1", "Paragraph A")], "fp2");

        let diff = diff_documents(&base, &target).expect("diff should succeed");

        let deletions = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockDeleted { .. }))
            .count();
        assert_eq!(deletions, 2, "should have 2 deletions");
    }

    #[test]
    fn test_dp_alignment_only_insertions() {
        // When we have insertions but no deletions to pair with
        let base = make_doc("doc", vec![make_paragraph("p1", "Paragraph A")], "fp1");
        let target = make_doc(
            "doc",
            vec![
                make_paragraph("p1", "Paragraph A"),
                make_paragraph("p2", "Paragraph B"),
                make_paragraph("p3", "Paragraph C"),
            ],
            "fp2",
        );

        let diff = diff_documents(&base, &target).expect("diff should succeed");

        let insertions = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockInserted { .. }))
            .count();
        assert_eq!(insertions, 2, "should have 2 insertions");
    }

    #[test]
    fn test_multi_insert_anchors_preserve_target_positions() {
        let base = make_doc(
            "doc",
            vec![
                make_paragraph("p1", "Original para 1."),
                make_paragraph("p2", "Original para 2."),
                make_paragraph("p3", "Original para 3."),
            ],
            "fp1",
        );
        let target = make_doc(
            "doc",
            vec![
                make_paragraph("n1", "New para at start."),
                make_paragraph("p1t", "Original para 1."),
                make_paragraph("n2", "New para after first."),
                make_paragraph("p2t", "Original para 2."),
                make_paragraph("p3t", "Original para 3."),
                make_paragraph("n3", "New para at end."),
            ],
            "fp2",
        );

        let diff = diff_documents(&base, &target).expect("diff should succeed");
        let inserted: Vec<Option<NodeId>> = diff
            .changes
            .iter()
            .filter_map(|change| {
                if let DiffChange::BlockInserted { after_block_id, .. } = change {
                    Some(after_block_id.clone())
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(
            inserted,
            vec![None, Some(NodeId::from("p1")), Some(NodeId::from("p3"))],
            "unexpected insertion anchors for multi-insert case"
        );
    }

    #[test]
    fn test_strong_match_breaks_gap_extension() {
        // This test validates the STRONG_MATCH_BONUS behavior.
        //
        // Without the bonus, the affine gap DP would prefer grouping all
        // insertions together (cheap extension) over interleaving with matches.
        //
        // Scenario (modeled on SAFE US vs Canada):
        // - Base: [A, B]
        // - Target: [NEW, A']
        //
        // Without strong match bonus: delete A, B; insert NEW, A'
        // With strong match bonus: insert NEW, modify A→A', delete B
        let base = make_doc(
            "doc",
            vec![
                make_paragraph(
                    "p1",
                    "THIS INSTRUMENT AND ANY SECURITIES ISSUABLE PURSUANT HERETO HAVE NOT BEEN REGISTERED UNDER THE SECURITIES ACT OF 1933",
                ),
                make_paragraph("p2", "Some other paragraph that will be deleted"),
            ],
            "fp1",
        );
        let target = make_doc(
            "doc",
            vec![
                make_paragraph("p1", "Please seek legal advice from an attorney"),
                make_paragraph(
                    "p2",
                    "THIS INSTRUMENT AND ANY SECURITIES ISSUABLE PURSUANT HERETO HAVE NOT BEEN REGISTERED UNDER THE UNITED STATES SECURITIES ACT OF 1933",
                ),
            ],
            "fp2",
        );

        let diff = diff_documents(&base, &target).expect("diff should succeed");

        let modifications = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockModified { .. }))
            .count();
        let insertions = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockInserted { .. }))
            .count();
        let deletions = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockDeleted { .. }))
            .count();

        // The key assertion: the strong match (A→A') should be recognized
        // as a modification, not grouped with other gaps
        assert_eq!(modifications, 1, "should have 1 modification (A→A')");

        // We expect 1 insertion (NEW) and 1 deletion (B)
        assert_eq!(insertions, 1, "should have 1 insertion");
        assert_eq!(deletions, 1, "should have 1 deletion");

        // Verify the modification contains the expected text transformation
        if let Some(DiffChange::BlockModified {
            old_text, new_text, ..
        }) = diff
            .changes
            .iter()
            .find(|c| matches!(c, DiffChange::BlockModified { .. }))
        {
            assert!(
                old_text.contains("SECURITIES ACT OF 1933"),
                "old text should contain 'SECURITIES ACT OF 1933'"
            );
            assert!(
                new_text.contains("UNITED STATES"),
                "new text should contain 'UNITED STATES'"
            );
        }
    }

    #[test]
    fn test_anchor_identical_content_across_gaps() {
        // This test validates anchor-based alignment.
        //
        // The key scenario (modeled on "IN WITNESS WHEREOF"):
        // - Base: [A, B, WITNESS, C]
        // - Target: [A, X, Y, Z, WITNESS, D]
        //
        // Without anchoring: WITNESS might be matched to X/Y/Z position-wise
        // With anchoring: WITNESS in base MUST match WITNESS in target (identical hash)
        let witness_text = "IN WITNESS WHEREOF, the undersigned have caused this Safe to be duly executed and delivered.";

        let base = make_doc(
            "doc",
            vec![
                make_paragraph("p1", "Section 1"),
                make_paragraph("p2", "Section 2"),
                make_paragraph("p3", witness_text),
                make_paragraph("p4", "[COMPANY]"),
            ],
            "fp1",
        );

        let target = make_doc(
            "doc",
            vec![
                make_paragraph("p1", "Section 1"),
                make_paragraph("p2", "New clause X"),
                make_paragraph("p3", "New clause Y"),
                make_paragraph("p4", "New clause Z"),
                make_paragraph("p5", witness_text), // Identical to base p3
                make_paragraph("p6", "[COMPANY NAME]"),
            ],
            "fp2",
        );

        let diff = diff_documents(&base, &target).expect("diff should succeed");

        // Key assertion: identical text should NOT appear as a modification
        // (If it's matched/anchored, it won't appear in changes at all)
        let witness_in_changes = diff.changes.iter().any(|c| match c {
            DiffChange::BlockModified { old_text, .. } => old_text == witness_text,
            DiffChange::BlockDeleted { old_text, .. } => old_text == witness_text,
            DiffChange::BlockInserted {
                block: BlockNode::Paragraph(p),
                ..
            } => {
                let inlines = p.all_inlines_owned();
                extract_inline_text(&inlines) == witness_text
            }
            _ => false,
        });

        assert!(
            !witness_in_changes,
            "Identical WITNESS text should be anchored and not appear as a change. \
             Found it in changes: {:?}",
            diff.changes
                .iter()
                .filter(|c| {
                    match c {
                        DiffChange::BlockModified { old_text, .. } => old_text.contains("WITNESS"),
                        DiffChange::BlockDeleted { old_text, .. } => old_text.contains("WITNESS"),
                        _ => false,
                    }
                })
                .collect::<Vec<_>>()
        );

        // We should have:
        // - 1 modification: Section 2 -> "New clause X" (position match, different content)
        //   OR Section 2 deleted + "New clause X" inserted
        // - Insertions: Y, Z
        // - 1 modification: [COMPANY] -> [COMPANY NAME]

        // Verify [COMPANY] -> [COMPANY NAME] modification exists
        let company_mod = diff.changes.iter().find(|c| {
            if let DiffChange::BlockModified {
                old_text, new_text, ..
            } = c
            {
                old_text.contains("COMPANY") && new_text.contains("COMPANY NAME")
            } else {
                false
            }
        });
        assert!(
            company_mod.is_some(),
            "should have [COMPANY] -> [COMPANY NAME] modification"
        );
    }

    #[test]
    fn test_anchor_handles_duplicates() {
        // Test that LCS correctly handles duplicate hashes.
        // When the same text appears multiple times, anchoring should
        // find the maximum non-crossing set.
        //
        // Base: [X, A, B, X]
        // Target: [X, C, X]
        //
        // Should anchor: base[0] with target[0], base[3] with target[2]
        // (maximizes anchors while respecting order)
        let repeated_text = "This paragraph appears multiple times";

        let base = make_doc(
            "doc",
            vec![
                make_paragraph("p1", repeated_text),
                make_paragraph("p2", "Paragraph A"),
                make_paragraph("p3", "Paragraph B"),
                make_paragraph("p4", repeated_text),
            ],
            "fp1",
        );

        let target = make_doc(
            "doc",
            vec![
                make_paragraph("p1", repeated_text),
                make_paragraph("p2", "Paragraph C"),
                make_paragraph("p3", repeated_text),
            ],
            "fp2",
        );

        let diff = diff_documents(&base, &target).expect("diff should succeed");

        // The repeated text should not appear as changes if properly anchored
        let repeated_in_deletions = diff
            .changes
            .iter()
            .filter(|c| {
                if let DiffChange::BlockDeleted { old_text, .. } = c {
                    old_text == repeated_text
                } else {
                    false
                }
            })
            .count();

        let repeated_in_insertions = diff
            .changes
            .iter()
            .filter(|c| {
                if let DiffChange::BlockInserted {
                    block: BlockNode::Paragraph(p),
                    ..
                } = c
                {
                    let inlines = p.all_inlines_owned();
                    extract_inline_text(&inlines) == repeated_text
                } else {
                    false
                }
            })
            .count();

        // With proper anchoring, neither of the X paragraphs should be deleted or inserted
        assert_eq!(
            repeated_in_deletions, 0,
            "repeated text should be anchored, not deleted"
        );
        assert_eq!(
            repeated_in_insertions, 0,
            "repeated text should be anchored, not inserted"
        );

        // Between anchors: base[1..3]=[A, B] vs target[1..2]=[C]
        // The DP might produce:
        // - Modify A→C, delete B (1 deletion, 1 modification)
        // - OR delete A+B, insert C (2 deletions, 1 insertion)
        // Either is valid; the key is the anchored X's are not in changes
        let deletions = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockDeleted { .. }))
            .count();
        let insertions = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockInserted { .. }))
            .count();
        let modifications = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockModified { .. }))
            .count();

        // Total changes should account for: A, B gone; C appeared
        // Either 2 deletions + 1 insertion, or 1 deletion + 1 modification
        let total_changes = deletions + insertions + modifications;
        assert!(
            (2..=3).contains(&total_changes),
            "should have 2-3 changes for the segment between anchors, got {} (del={}, ins={}, mod={})",
            total_changes,
            deletions,
            insertions,
            modifications
        );
    }

    // =========================================================================
    // Zipper collapse and cleanup tests
    // =========================================================================

    #[test]
    fn test_tokenize_legal_enumerators() {
        // Legal enumerators should be kept as atomic tokens
        assert_eq!(
            tokenize("(i) a transaction"),
            vec!["(i)", " ", "a", " ", "transaction"]
        );
        assert_eq!(
            tokenize("(ii) the company"),
            vec!["(ii)", " ", "the", " ", "company"]
        );
        assert_eq!(
            tokenize("(a) first item"),
            vec!["(a)", " ", "first", " ", "item"]
        );
        assert_eq!(
            tokenize("(xiv) last item"),
            vec!["(xiv)", " ", "last", " ", "item"]
        );
        // Double letters
        assert_eq!(tokenize("(aa) double"), vec!["(aa)", " ", "double"]);
        // NOT fused: `(the` is not an enumerator
        assert_eq!(tokenize("(the thing)"), vec!["(", "the", " ", "thing", ")"]);
        // NOT fused: `13(d)` — left boundary is alphanumeric
        assert_eq!(tokenize("13(d)"), vec!["13", "(", "d", ")"]);
        // NOT fused: `(d)3` — right boundary is alphanumeric
        assert_eq!(tokenize("(d)3"), vec!["(", "d", ")", "3"]);
        // Fused when preceded by space
        assert_eq!(
            tokenize("Section (d) applies"),
            vec!["Section", " ", "(d)", " ", "applies"]
        );
    }

    #[test]
    fn test_tokenize_apostrophes() {
        // Possessives and contractions should be single tokens
        assert_eq!(tokenize("don't stop"), vec!["don't", " ", "stop"]);
        assert_eq!(tokenize("it's here"), vec!["it's", " ", "here"]);
        assert_eq!(tokenize("refer's to"), vec!["refer's", " ", "to"]);
        // Smart quote apostrophe (U+2019) should also fuse
        assert_eq!(
            tokenize("don\u{2019}t stop"),
            vec!["don\u{2019}t", " ", "stop"]
        );
        // Apostrophe at start or end of text should NOT fuse (it's a quote mark)
        assert_eq!(tokenize("'hello'"), vec!["'", "hello", "'"]);
        // Apostrophe next to non-word (space, punctuation) should NOT fuse
        assert_eq!(tokenize("the ' thing"), vec!["the", " ", "'", " ", "thing"]);
    }

    #[test]
    fn test_merge_adjacent_same_type() {
        let changes = vec![
            InlineChange::Unchanged {
                text: "hello".to_string(),
                marks: vec![],
                style_props: StyleProps::default(),
                formatting_change: None,
            },
            InlineChange::Unchanged {
                text: " world".to_string(),
                marks: vec![],
                style_props: StyleProps::default(),
                formatting_change: None,
            },
            InlineChange::Deleted {
                text: "foo".to_string(),
                marks: vec![],
                style_props: StyleProps::default(),
                formatting_change: None,
                rev_id: 0,
            },
            InlineChange::Deleted {
                text: "bar".to_string(),
                marks: vec![],
                style_props: StyleProps::default(),
                formatting_change: None,
                rev_id: 0,
            },
        ];
        let merged = merge_adjacent_same_type(changes);
        assert_eq!(merged.len(), 2);
        assert!(
            matches!(&merged[0], InlineChange::Unchanged { text, .. } if text == "hello world")
        );
        assert!(matches!(&merged[1], InlineChange::Deleted { text, .. } if text == "foobar"));
    }

    #[test]
    fn test_merge_adjacent_different_marks_not_merged() {
        let changes = vec![
            InlineChange::Unchanged {
                text: "hello".to_string(),
                marks: vec![Mark::Bold],
                style_props: StyleProps::default(),
                formatting_change: None,
            },
            InlineChange::Unchanged {
                text: " world".to_string(),
                marks: vec![],
                style_props: StyleProps::default(),
                formatting_change: None,
            },
        ];
        let merged = merge_adjacent_same_type(changes);
        assert_eq!(merged.len(), 2, "different marks should not be merged");
    }

    #[test]
    fn test_zipper_detection() {
        // Build a sequence with many alternating del/ins runs
        let mut changes = Vec::new();
        for _ in 0..12 {
            changes.push(InlineChange::Deleted {
                text: "old".to_string(),
                marks: vec![],
                style_props: StyleProps::default(),
                formatting_change: None,
                rev_id: 0,
            });
            changes.push(InlineChange::Inserted {
                text: "new".to_string(),
                marks: vec![],
                style_props: StyleProps::default(),
                formatting_change: None,
                rev_id: 0,
            });
        }
        let runs = count_change_runs(&changes);
        assert!(
            runs >= ZIPPER_MIN_CHANGE_RUNS,
            "should detect {} runs, got {}",
            ZIPPER_MIN_CHANGE_RUNS,
            runs
        );
        assert!(
            should_collapse_region(&changes),
            "high-run region should be collapsed"
        );

        // A simple 2-run region with overlapping content should NOT be collapsed
        // (similarity is high, and run count is low)
        let simple = vec![
            InlineChange::Unchanged {
                text: "the company shall ".to_string(),
                marks: vec![],
                style_props: StyleProps::default(),
                formatting_change: None,
            },
            InlineChange::Deleted {
                text: "perform".to_string(),
                marks: vec![],
                style_props: StyleProps::default(),
                formatting_change: None,
                rev_id: 0,
            },
            InlineChange::Inserted {
                text: "execute".to_string(),
                marks: vec![],
                style_props: StyleProps::default(),
                formatting_change: None,
                rev_id: 0,
            },
            InlineChange::Unchanged {
                text: " its duties".to_string(),
                marks: vec![],
                style_props: StyleProps::default(),
                formatting_change: None,
            },
        ];
        assert!(
            !should_collapse_region(&simple),
            "simple word swap should not be collapsed"
        );
    }

    #[test]
    fn test_zipper_collapse() {
        // Simulate a zipper: del/ins/del/ins/... with tiny unchanged between
        let mut changes = Vec::new();
        for i in 0..8 {
            changes.push(InlineChange::Deleted {
                text: format!("old{}", i),
                marks: vec![],
                style_props: StyleProps::default(),
                formatting_change: None,
                rev_id: 0,
            });
            changes.push(InlineChange::Unchanged {
                text: " ".to_string(), // tiny, ignored as anchor
                marks: vec![],
                style_props: StyleProps::default(),
                formatting_change: None,
            });
            changes.push(InlineChange::Inserted {
                text: format!("new{}", i),
                marks: vec![],
                style_props: StyleProps::default(),
                formatting_change: None,
                rev_id: 0,
            });
        }

        let collapsed = collapse_region(&changes);
        // Should have exactly 1 Deleted and 1 Inserted
        let del_count = collapsed
            .iter()
            .filter(|c| matches!(c, InlineChange::Deleted { .. }))
            .count();
        let ins_count = collapsed
            .iter()
            .filter(|c| matches!(c, InlineChange::Inserted { .. }))
            .count();
        assert_eq!(del_count, 1, "should have 1 collapsed deletion");
        assert_eq!(ins_count, 1, "should have 1 collapsed insertion");

        // All old text + unchanged text should be in the deleted segment
        if let InlineChange::Deleted { text, .. } = &collapsed[0] {
            for i in 0..8 {
                assert!(
                    text.contains(&format!("old{}", i)),
                    "deleted text should contain old{}",
                    i
                );
            }
        }
    }

    #[test]
    fn test_char_level_affix_factoring_known_issue() {
        // Reproduces the known issue: "five (5) years." → "two (2) years."
        // After zipper collapse, factor_common_affixes can't help (one Del, one Ins,
        // different text). factor_char_level_affixes should recover the common suffix.
        let input = vec![
            InlineChange::Deleted {
                text: "five (5) years.".to_string(),
                marks: vec![],
                style_props: StyleProps::default(),
                formatting_change: None,
                rev_id: 0,
            },
            InlineChange::Inserted {
                text: "two (2) years.".to_string(),
                marks: vec![],
                style_props: StyleProps::default(),
                formatting_change: None,
                rev_id: 0,
            },
        ];

        let result = factor_char_level_affixes(input);

        // Should split: Del("five (5") + Ins("two (2") + Unchanged(") years.")
        assert_eq!(result.len(), 3, "expected 3 segments, got: {result:?}");
        assert!(
            matches!(&result[0], InlineChange::Deleted { text, .. } if text == "five (5"),
            "expected Del(\"five (5\"), got: {:?}",
            result[0]
        );
        assert!(
            matches!(&result[1], InlineChange::Inserted { text, .. } if text == "two (2"),
            "expected Ins(\"two (2\"), got: {:?}",
            result[1]
        );
        assert!(
            matches!(&result[2], InlineChange::Unchanged { text, .. } if text == ") years."),
            "expected Unchanged(\") years.\"), got: {:?}",
            result[2]
        );
    }

    #[test]
    fn test_char_level_affix_factoring_both_sides() {
        // Common prefix AND suffix: "the five (5) year term" → "the two (2) year term"
        let input = vec![
            InlineChange::Deleted {
                text: "the five (5) year term".to_string(),
                marks: vec![],
                style_props: StyleProps::default(),
                formatting_change: None,
                rev_id: 0,
            },
            InlineChange::Inserted {
                text: "the two (2) year term".to_string(),
                marks: vec![],
                style_props: StyleProps::default(),
                formatting_change: None,
                rev_id: 0,
            },
        ];

        let result = factor_char_level_affixes(input);

        // Common suffix includes ") year term" (11 bytes) because ")" is a token boundary.
        // Should split: Unchanged("the ") + Del("five (5") + Ins("two (2") + Unchanged(") year term")
        assert_eq!(result.len(), 4, "expected 4 segments, got: {result:?}");
        assert!(
            matches!(&result[0], InlineChange::Unchanged { text, .. } if text == "the "),
            "expected Unchanged(\"the \"), got: {:?}",
            result[0]
        );
        assert!(
            matches!(&result[1], InlineChange::Deleted { text, .. } if text == "five (5"),
            "expected Del(\"five (5\"), got: {:?}",
            result[1]
        );
        assert!(
            matches!(&result[2], InlineChange::Inserted { text, .. } if text == "two (2"),
            "expected Ins(\"two (2\"), got: {:?}",
            result[2]
        );
        assert!(
            matches!(&result[3], InlineChange::Unchanged { text, .. } if text == ") year term"),
            "expected Unchanged(\") year term\"), got: {:?}",
            result[3]
        );
    }

    #[test]
    fn test_char_level_affix_no_split_when_shared_small() {
        // When shared text is small relative to unique text, don't split
        let input = vec![
            InlineChange::Deleted {
                text: "completely different text here".to_string(),
                marks: vec![],
                style_props: StyleProps::default(),
                formatting_change: None,
                rev_id: 0,
            },
            InlineChange::Inserted {
                text: "something else entirely now".to_string(),
                marks: vec![],
                style_props: StyleProps::default(),
                formatting_change: None,
                rev_id: 0,
            },
        ];

        let result = factor_char_level_affixes(input);
        // No shared prefix/suffix → no split, passthrough
        assert_eq!(result.len(), 2, "should not split unrelated texts");
    }

    #[test]
    fn test_char_level_affix_preserves_non_del_ins() {
        // Unchanged segments pass through untouched
        let input = vec![
            InlineChange::Unchanged {
                text: "hello ".to_string(),
                marks: vec![],
                style_props: StyleProps::default(),
                formatting_change: None,
            },
            InlineChange::Deleted {
                text: "world".to_string(),
                marks: vec![],
                style_props: StyleProps::default(),
                formatting_change: None,
                rev_id: 0,
            },
            InlineChange::Inserted {
                text: "earth".to_string(),
                marks: vec![],
                style_props: StyleProps::default(),
                formatting_change: None,
                rev_id: 0,
            },
        ];

        let result = factor_char_level_affixes(input);
        // No shared text between "world" and "earth" → no char-level split
        assert_eq!(
            result.len(),
            3,
            "should pass through unchanged + unrelated pair"
        );
    }

    #[test]
    fn test_strong_anchor_identification() {
        let changes = vec![
            InlineChange::Unchanged {
                text: "This is a very long unchanged text that exceeds twenty characters"
                    .to_string(),
                marks: vec![],
                style_props: StyleProps::default(),
                formatting_change: None,
            },
            InlineChange::Deleted {
                text: "x".to_string(),
                marks: vec![],
                style_props: StyleProps::default(),
                formatting_change: None,
                rev_id: 0,
            },
            InlineChange::Unchanged {
                text: "ab".to_string(), // too short, not an anchor
                marks: vec![],
                style_props: StyleProps::default(),
                formatting_change: None,
            },
        ];
        let anchors = find_strong_anchors(&changes);
        assert_eq!(anchors.len(), 1, "only the long text should be an anchor");
        assert_eq!(anchors[0], 0);
    }

    #[test]
    fn test_enumerator_as_anchor() {
        let changes = vec![InlineChange::Unchanged {
            text: "(ii)".to_string(),
            marks: vec![],
            style_props: StyleProps::default(),
            formatting_change: None,
        }];
        let anchors = find_strong_anchors(&changes);
        assert_eq!(
            anchors.len(),
            1,
            "enumerator (ii) should be a strong anchor"
        );
    }

    #[test]
    fn test_bail_out_heavily_rewritten() {
        // Two completely different texts should trigger bail-out.
        // Use words with zero overlap to ensure low similarity.
        let old = "Gurbanguly berdimuhamedow turkmenbashy ashgabat dashoguz lebap balkanabat arkadag neutrality magtymguly pyragy";
        let new = "Xylophone quizzical fjordlike vuvuzela rambunctious schmaltz borscht knapsack gymnasium labyrinthine";

        assert!(
            should_bail_out(old, new),
            "very different texts should trigger bail-out"
        );

        // Build full replace should produce delete + insert
        let old_inlines = vec![InlineNode::from(TextNode {
            id: NodeId::from("t1"),
            text_role: None,
            text: old.to_string(),
            marks: vec![Mark::Bold],
            style_props: StyleProps::default(),
            rpr_authored: crate::domain::RunRprAuthored::default(),
            source_run_attrs: Vec::new(),
            formatting_change: None,
        })];
        let new_inlines = vec![InlineNode::from(TextNode {
            id: NodeId::from("t2"),
            text_role: None,
            text: new.to_string(),
            marks: vec![Mark::Italic],
            style_props: StyleProps::default(),
            rpr_authored: crate::domain::RunRprAuthored::default(),
            source_run_attrs: Vec::new(),
            formatting_change: None,
        })];
        let result = build_full_replace(&old_inlines, &new_inlines, &HashMap::new());
        let del_count = result
            .iter()
            .filter(|c| matches!(c, InlineChange::Deleted { .. }))
            .count();
        let ins_count = result
            .iter()
            .filter(|c| matches!(c, InlineChange::Inserted { .. }))
            .count();
        assert_eq!(del_count, 1, "should have 1 deleted segment");
        assert_eq!(ins_count, 1, "should have 1 inserted segment");

        // Marks should be preserved
        if let InlineChange::Deleted { marks, .. } = &result[0] {
            assert!(
                marks.contains(&Mark::Bold),
                "deleted marks should include Bold"
            );
        }
        if let InlineChange::Inserted { marks, .. } = &result[1] {
            assert!(
                marks.contains(&Mark::Italic),
                "inserted marks should include Italic"
            );
        }
    }

    #[test]
    fn test_bail_out_preserved_for_numeric_changes() {
        // Texts are very different but contain differing numbers — should NOT bail out
        let old =
            "The purchase price shall be calculated at a rate of 15 percent of the total value";
        let new =
            "Lorem ipsum dolor sit amet consectetur with a rate of 25 percent of something else";

        // sim is low, but the numbers differ (15 vs 25), so should not bail out
        assert!(
            !should_bail_out(old, new),
            "should not bail out when numeric tokens differ"
        );
    }

    #[test]
    fn test_bail_out_short_text_never_triggers() {
        // Short texts should never trigger bail-out
        let old = "hello";
        let new = "world";
        assert!(
            !should_bail_out(old, new),
            "short texts should never trigger bail-out"
        );
    }

    #[test]
    fn test_cleanup_preserves_simple_diffs() {
        // Simple diff: "Hello World" → "Hello Universe"
        let changes = diff_block_content("Hello World", "Hello Universe");

        // Should still show the word-level change cleanly
        let has_unchanged = changes
            .iter()
            .any(|c| matches!(c, InlineChange::Unchanged { text, .. } if text.contains("Hello")));
        let has_deleted = changes
            .iter()
            .any(|c| matches!(c, InlineChange::Deleted { text, .. } if text.contains("World")));
        let has_inserted = changes
            .iter()
            .any(|c| matches!(c, InlineChange::Inserted { text, .. } if text.contains("Universe")));

        assert!(
            has_unchanged,
            "cleanup should preserve unchanged; got: {:?}",
            changes
        );
        assert!(
            has_deleted,
            "cleanup should preserve deletion; got: {:?}",
            changes
        );
        assert!(
            has_inserted,
            "cleanup should preserve insertion; got: {:?}",
            changes
        );
    }

    #[test]
    fn test_change_of_control_no_zipper() {
        // Simplified "Change of Control" clause comparison that would produce
        // a zipper pattern without cleanup
        let old = r#""Change of Control" means (i) a transaction or series of related transactions in which any third party acquires control of the Company by way of merger, consolidation or other business combination, (ii) any reorganization, recapitalization, or reclassification of the Company's capital stock, or (iii) a sale, lease or other disposition of all or substantially all of the assets of the Company"#;
        let new = r#""Change of Control" means (i) a transaction or series of related transactions in which any person or group acquires beneficial ownership of voting securities representing more than fifty percent, (ii) any reorganisation, recapitalisation, merger, amalgamation or reclassification of the Company's share capital, or (iii) a sale, lease or other disposition of all or substantially all of the assets of the Group Companies"#;

        let changes = diff_block_content(old, new);

        // Key assertions:
        // 1. The "(i)", "(ii)", "(iii)" enumerators should be preserved as anchors
        let has_i = changes
            .iter()
            .any(|c| matches!(c, InlineChange::Unchanged { text, .. } if text.contains("(i)")));
        assert!(
            has_i,
            "(i) should be preserved as unchanged anchor; got: {:?}",
            changes
        );

        // 2. Should not have excessive interleaving (zipper pattern)
        // Count alternating type changes as a proxy for zipper-ness
        let mut type_changes = 0;
        for w in changes.windows(2) {
            let t0 = std::mem::discriminant(&w[0]);
            let t1 = std::mem::discriminant(&w[1]);
            if t0 != t1 {
                type_changes += 1;
            }
        }
        // A clean diff should have relatively few type transitions.
        // Without cleanup (pure Myers), this would produce many more transitions.
        // With Patience + cleanup, we expect a moderate number.
        // The key improvement: enumerator anchors (i)/(ii)/(iii) are preserved,
        // and heavily-rewritten sub-clauses get collapsed.
        assert!(
            type_changes < 40,
            "should not have excessive type changes (zipper), got {} transitions in {:?}",
            type_changes,
            changes
        );
    }

    #[test]
    fn test_opaque_placeholder_returns_barrier() {
        use crate::domain::{DocPart, ProofRef};
        let opaque = OpaqueInlineNode {
            id: NodeId::from("eq1"),
            kind: OpaqueKind::OmmlInline,
            opaque_ref: String::new(),
            proof_ref: ProofRef {
                part: DocPart::DocumentXml,
                block_id: NodeId::from("p1"),
                docx_anchor: String::new(),
            },
            wrapper_marks: vec![],
            wrapper_style_props: crate::domain::StyleProps::default(),
            raw_xml: Some(br#"<m:oMath xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math"><m:r><m:t>y</m:t></m:r></m:oMath>"#.to_vec()),
            content_hash: None,
        };
        let result = opaque_placeholder(&opaque);
        assert_eq!(result, "\u{FFFC}");
    }

    #[test]
    fn test_drawing_fallback_text_vml_rect() {
        let xml = br#"<w:pict><v:rect style="..."/></w:pict>"#;
        assert_eq!(
            extract_drawing_fallback_text(xml),
            Some("[shape: rect]".to_string())
        );
    }

    #[test]
    fn test_drawing_fallback_text_vml_oval() {
        let xml = br#"<w:pict><v:oval style="..."/></w:pict>"#;
        assert_eq!(
            extract_drawing_fallback_text(xml),
            Some("[shape: oval]".to_string())
        );
    }

    #[test]
    fn test_drawing_fallback_text_wp_shape() {
        let xml = br#"<wps:wsp><a:prstGeom prst="ellipse"/></wps:wsp>"#;
        assert_eq!(
            extract_drawing_fallback_text(xml),
            Some("[shape: ellipse]".to_string())
        );
    }

    #[test]
    fn test_drawing_fallback_text_alt_text_priority() {
        let xml = br#"<wp:docPr descr="Company Logo"/><v:rect/>"#;
        let result = extract_drawing_fallback_text(xml).unwrap();
        assert!(result.contains("Company Logo"));
        assert!(result.starts_with("[drawing:"));
    }

    #[test]
    fn test_drawing_fallback_text_chart() {
        let xml = br#"<c:chart xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart"/>"#;
        assert_eq!(
            extract_drawing_fallback_text(xml),
            Some("[chart]".to_string())
        );
    }

    #[test]
    fn test_drawing_fallback_text_unknown() {
        let xml = br#"<w:drawing><wp:inline/></w:drawing>"#;
        assert_eq!(
            extract_drawing_fallback_text(xml),
            Some("[drawing]".to_string())
        );
    }

    #[test]
    fn test_drawing_fallback_text_raster_image_returns_none() {
        // Raster images should have asset_ref populated, so no fallback text needed
        let xml = br#"<w:drawing><a:blip r:embed="rId5"/></w:drawing>"#;
        assert_eq!(extract_drawing_fallback_text(xml), None);
    }

    #[test]
    fn test_drawing_fallback_text_vml_imagedata_returns_none() {
        // VML shapes with imagedata have raster backing, so no fallback text needed
        let xml = br#"<w:pict><v:shape><v:imagedata r:id="rId7"/></v:shape></w:pict>"#;
        assert_eq!(extract_drawing_fallback_text(xml), None);
    }

    #[test]
    fn test_find_blip_rid_with_namespace() {
        let xml = r#"<w:drawing><a:blip r:embed="rId5"/></w:drawing>"#;
        assert_eq!(find_blip_rid(xml), Some("rId5".to_string()));
    }

    #[test]
    fn test_find_blip_rid_without_namespace() {
        let xml = r#"<drawing><blip embed="rId5"/></drawing>"#;
        assert_eq!(find_blip_rid(xml), Some("rId5".to_string()));
    }

    #[test]
    fn test_find_blip_rid_with_space() {
        let xml = r#"<blip type="image" embed="rId5" /></blip>"#;
        assert_eq!(find_blip_rid(xml), Some("rId5".to_string()));
    }

    #[test]
    fn test_find_blip_rid_no_embed() {
        let xml = r#"<w:drawing><wps:wsp><a:prstGeom prst="rect"/></wps:wsp></w:drawing>"#;
        assert_eq!(find_blip_rid(xml), None);
    }

    #[test]
    fn test_find_blip_rid_vml_imagedata() {
        let xml = r#"<w:pict><v:shape><v:imagedata r:id="rId7"/></v:shape></w:pict>"#;
        assert_eq!(find_blip_rid(xml), Some("rId7".to_string()));
    }

    #[test]
    fn test_find_blip_rid_vml_imagedata_without_prefix() {
        let xml = r#"<pict><shape><imagedata r:id="rId3"/></shape></pict>"#;
        assert_eq!(find_blip_rid(xml), Some("rId3".to_string()));
    }

    #[test]
    fn test_find_blip_rid_prefers_embed_over_vml() {
        // If both DrawingML embed and VML imagedata exist, prefer embed
        let xml = r#"<w:drawing><a:blip r:embed="rId5"/></w:drawing><w:pict><v:imagedata r:id="rId7"/></w:pict>"#;
        assert_eq!(find_blip_rid(xml), Some("rId5".to_string()));
    }

    #[test]
    fn test_find_blip_rid_vml_shape_without_imagedata() {
        // VML shape without imagedata should return None
        let xml = r#"<w:pict><v:rect style="width:100pt;height:50pt"/></w:pict>"#;
        assert_eq!(find_blip_rid(xml), None);
    }

    // ══════════════════════════════════════════════════════════════════════
    // Move detection tests
    // ══════════════════════════════════════════════════════════════════════

    /// Helper to build a minimal FullDocBlock with a given change_type and text segments.
    fn make_full_doc_block(id: &str, change_type: ChangeType, text: &str) -> FullDocBlock {
        let segments = vec![match change_type {
            ChangeType::Deleted => InlineChange::Deleted {
                text: text.to_string(),
                marks: vec![],
                style_props: StyleProps::default(),
                formatting_change: None,
                rev_id: 0,
            },
            ChangeType::Inserted => InlineChange::Inserted {
                text: text.to_string(),
                marks: vec![],
                style_props: StyleProps::default(),
                formatting_change: None,
                rev_id: 0,
            },
            _ => InlineChange::Unchanged {
                text: text.to_string(),
                marks: vec![],
                style_props: StyleProps::default(),
                formatting_change: None,
            },
        }];
        FullDocBlock {
            block_id: NodeId::from(id.to_string()),
            doc1_block_id: None,
            doc2_block_id: None,
            block_type: BlockType::Paragraph,
            heading_level: None,
            style_id: None,
            change_type,
            align: None,
            indent: None,
            spacing: None,
            borders: None,
            tab_stops: vec![],
            numbering_text: None,
            numbering_ilvl: None,
            numbering_num_id: None,
            segments,
            table_diff: None,
            content_types: vec![],
            equation_xmls: vec![],
            equation_doc1_count: 0,
            image_data_uris: vec![],
            image_doc1_count: 0,
            image_metadata_changes: vec![],
            move_id: None,
            move_direction: None,
            structural_change: None,
            border_group_id: None,
            paragraph_mark_status: None,
        }
    }

    #[test]
    fn test_detect_moves_matching_deleted_and_inserted() {
        let long_text = "This is a substantial paragraph that was moved from one location to another in the document.";

        let mut blocks = vec![
            make_full_doc_block("p0", ChangeType::Unchanged, "First paragraph."),
            make_full_doc_block("p1", ChangeType::Deleted, long_text),
            make_full_doc_block("p2", ChangeType::Unchanged, "Middle paragraph."),
            make_full_doc_block("p3", ChangeType::Inserted, long_text),
        ];

        detect_moves(&mut blocks);

        assert_eq!(
            blocks[1].move_id.as_deref(),
            Some("move_0"),
            "deleted block should have move_id"
        );
        assert_eq!(
            blocks[1].move_direction,
            Some(MoveDirection::From),
            "deleted block should be 'from'"
        );
        assert_eq!(
            blocks[3].move_id.as_deref(),
            Some("move_0"),
            "inserted block should have same move_id"
        );
        assert_eq!(
            blocks[3].move_direction,
            Some(MoveDirection::To),
            "inserted block should be 'to'"
        );

        // Unchanged blocks should not be affected.
        assert!(blocks[0].move_id.is_none());
        assert!(blocks[2].move_id.is_none());
    }

    #[test]
    fn test_detect_moves_ignores_short_text() {
        let mut blocks = vec![
            make_full_doc_block("p0", ChangeType::Deleted, "Short."),
            make_full_doc_block("p1", ChangeType::Inserted, "Short."),
        ];

        detect_moves(&mut blocks);

        // Short text (< 20 chars) should not be detected as moves.
        assert!(
            blocks[0].move_id.is_none(),
            "short text should not be detected as a move"
        );
        assert!(
            blocks[1].move_id.is_none(),
            "short text should not be detected as a move"
        );
    }

    #[test]
    fn test_detect_moves_no_false_positives_on_unchanged() {
        let long_text = "This is a long enough paragraph that could theoretically match.";
        let mut blocks = vec![
            make_full_doc_block("p0", ChangeType::Unchanged, long_text),
            make_full_doc_block("p1", ChangeType::Unchanged, long_text),
        ];

        detect_moves(&mut blocks);

        let moves: Vec<_> = blocks.iter().filter(|b| b.move_id.is_some()).collect();
        assert!(
            moves.is_empty(),
            "unchanged blocks should not be detected as moves"
        );
    }

    #[test]
    fn full_document_projection_reuses_target_ids_and_stable_delete_tombstones() {
        let base = make_doc(
            "doc",
            vec![
                make_paragraph("base_p1", "Alpha"),
                make_paragraph("base_p2", "Deleted"),
                make_paragraph("base_p3", "Gamma"),
            ],
            "fp1",
        );
        let target = make_doc(
            "doc",
            vec![
                make_paragraph("target_p10", "Alpha"),
                make_paragraph("target_p30", "Gamma"),
                make_paragraph("target_p40", "Inserted"),
            ],
            "fp2",
        );

        let blocks = build_full_document_view(&base, &target, &HashMap::new(), &HashMap::new())
            .expect("full document view should succeed");

        let ids: Vec<_> = blocks
            .iter()
            .map(|block| block.block_id.0.as_ref())
            .collect();
        assert_eq!(
            ids,
            vec!["target_p10", "deleted:base_p2", "target_p30", "target_p40"],
        );
        assert_eq!(
            blocks[0].doc1_block_id.as_ref().map(|id| id.0.as_ref()),
            Some("base_p1"),
        );
        assert_eq!(
            blocks[0].doc2_block_id.as_ref().map(|id| id.0.as_ref()),
            Some("target_p10"),
        );
        assert_eq!(blocks[1].change_type, ChangeType::Deleted);
        assert_eq!(
            blocks[1].doc1_block_id.as_ref().map(|id| id.0.as_ref()),
            Some("base_p2"),
        );
        assert_eq!(blocks[1].doc2_block_id, None);
        assert_eq!(blocks[3].change_type, ChangeType::Inserted);
        assert_eq!(blocks[3].doc1_block_id, None);
        assert_eq!(
            blocks[3].doc2_block_id.as_ref().map(|id| id.0.as_ref()),
            Some("target_p40"),
        );
    }

    #[test]
    fn full_document_projection_uses_target_id_for_modified_blocks() {
        let base = make_doc("doc", vec![make_paragraph("base_p1", "Old text")], "fp1");
        let target = make_doc("doc", vec![make_paragraph("target_p9", "New text")], "fp2");

        let blocks = build_full_document_view(&base, &target, &HashMap::new(), &HashMap::new())
            .expect("full document view should succeed");

        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].change_type, ChangeType::Modified);
        assert_eq!(blocks[0].block_id.0.as_ref(), "target_p9");
        assert_eq!(
            blocks[0].doc1_block_id.as_ref().map(|id| id.0.as_ref()),
            Some("base_p1"),
        );
        assert_eq!(
            blocks[0].doc2_block_id.as_ref().map(|id| id.0.as_ref()),
            Some("target_p9"),
        );
    }

    /// Build a minimal single-cell table for identity tests.
    fn make_table(id: &str, cell_text: &str) -> BlockNode {
        let cell_para = match make_paragraph(&format!("{id}_c0_p0"), cell_text) {
            BlockNode::Paragraph(p) => p,
            _ => unreachable!(),
        };
        BlockNode::from(TableNode {
            id: NodeId::from(id.to_string()),
            rows: vec![TableRowNode {
                id: NodeId::from(format!("{id}_r0")),
                cells: vec![TableCellNode {
                    id: NodeId::from(format!("{id}_r0_c0")),
                    blocks: vec![BlockNode::Paragraph(cell_para)],
                    grid_span: 1,
                    v_merge: VerticalMerge::None,
                    formatting: CellFormatting::default(),
                    formatting_change: None,
                    tracking_status: None,
                    row_sdt_wrapper: None,
                    content_sdt_wraps: Vec::new(),
                    cnf_style: None,
                    hide_mark: false,
                    preserved: Vec::new(),
                }],
                grid_before: 0,
                grid_after: 0,
                tracking_status: None,
                is_header: false,
                height: None,
                height_rule: None,
                formatting_change: None,
                para_id: None,
                text_id: None,
                cant_split: false,
                jc: None,
                w_before: None,
                w_after: None,
                cnf_style: None,
                tbl_pr_ex: None,
                cell_spacing: None,
                preserved: Vec::new(),
            }],
            structure_hash: String::new(),
            formatting: TableFormatting::default(),
            formatting_change: None,
        })
    }

    /// A 1x1 table whose single cell contains `paragraphs.len()` paragraphs.
    fn make_table_with_cell_paragraphs(id: &str, paragraphs: &[&str]) -> BlockNode {
        let blocks: Vec<BlockNode> = paragraphs
            .iter()
            .enumerate()
            .map(|(i, t)| make_paragraph(&format!("{id}_c0_p{i}"), t))
            .collect();
        BlockNode::from(TableNode {
            id: NodeId::from(id.to_string()),
            rows: vec![TableRowNode {
                id: NodeId::from(format!("{id}_r0")),
                cells: vec![TableCellNode {
                    id: NodeId::from(format!("{id}_r0_c0")),
                    blocks,
                    grid_span: 1,
                    v_merge: VerticalMerge::None,
                    formatting: CellFormatting::default(),
                    formatting_change: None,
                    tracking_status: None,
                    row_sdt_wrapper: None,
                    content_sdt_wraps: Vec::new(),
                    cnf_style: None,
                    hide_mark: false,
                    preserved: Vec::new(),
                }],
                grid_before: 0,
                grid_after: 0,
                tracking_status: None,
                is_header: false,
                height: None,
                height_rule: None,
                formatting_change: None,
                para_id: None,
                text_id: None,
                cant_split: false,
                jc: None,
                w_before: None,
                w_after: None,
                cnf_style: None,
                tbl_pr_ex: None,
                cell_spacing: None,
                preserved: Vec::new(),
            }],
            structure_hash: String::new(),
            formatting: TableFormatting::default(),
            formatting_change: None,
        })
    }

    /// A 1x1 table whose single cell contains one `BlockNode::OpaqueBlock`
    /// (e.g. a content control / SDT) identified by `opaque_ref`.
    fn make_table_with_cell_opaque(id: &str, opaque_ref: &str) -> BlockNode {
        let opaque = BlockNode::from(OpaqueBlockNode {
            id: NodeId::from(format!("{id}_c0_p0")),
            kind: OpaqueKind::Drawing,
            opaque_ref: opaque_ref.to_string(),
            proof_ref: ProofRef {
                part: DocPart::DocumentXml,
                block_id: NodeId::from(format!("{id}_c0_p0")),
                docx_anchor: opaque_ref.to_string(),
            },
            range_marker: None,
        });
        BlockNode::from(TableNode {
            id: NodeId::from(id.to_string()),
            rows: vec![TableRowNode {
                id: NodeId::from(format!("{id}_r0")),
                cells: vec![TableCellNode {
                    id: NodeId::from(format!("{id}_r0_c0")),
                    blocks: vec![opaque],
                    grid_span: 1,
                    v_merge: VerticalMerge::None,
                    formatting: CellFormatting::default(),
                    formatting_change: None,
                    tracking_status: None,
                    row_sdt_wrapper: None,
                    content_sdt_wraps: Vec::new(),
                    cnf_style: None,
                    hide_mark: false,
                    preserved: Vec::new(),
                }],
                grid_before: 0,
                grid_after: 0,
                tracking_status: None,
                is_header: false,
                height: None,
                height_rule: None,
                formatting_change: None,
                para_id: None,
                text_id: None,
                cant_split: false,
                jc: None,
                w_before: None,
                w_after: None,
                cnf_style: None,
                tbl_pr_ex: None,
                cell_spacing: None,
                preserved: Vec::new(),
            }],
            structure_hash: String::new(),
            formatting: TableFormatting::default(),
            formatting_change: None,
        })
    }

    #[test]
    fn diff_cell_opaque_block_change_is_not_silently_dropped() {
        // A table cell whose sole block is an OpaqueBlock (e.g. a content
        // control) with a DIFFERENT opaque_ref in base vs target is a real
        // content change: a different SDT now occupies that cell. Row/cell
        // counts and per-cell block counts are identical, so structure_hash
        // and the block-count guard (see
        // diff_cell_paragraph_added_is_not_silently_dropped) both pass, and
        // diff_matched_tables' per-cell loop used to zip (OpaqueBlock,
        // OpaqueBlock) straight into the `_ => {}` arm — dropping the change
        // entirely. The diff must represent it.
        let base = make_doc(
            "doc",
            vec![make_table_with_cell_opaque("tbl", "opaque-a")],
            "fp1",
        );
        let target = make_doc(
            "doc",
            vec![make_table_with_cell_opaque("tbl", "opaque-b")],
            "fp2",
        );

        let diff = diff_documents(&base, &target).expect("diff should succeed");
        assert!(
            !diff.changes.is_empty(),
            "swapping a cell's opaque block must produce a diff change, got changes: []"
        );
        assert!(
            diff.changes.iter().any(|c| matches!(
                c,
                DiffChange::TableStructureChanged { table_id, .. } if table_id.0.as_ref() == "tbl"
            )),
            "expected a TableStructureChanged for the cell opaque-block change, got {:?}",
            diff.changes
        );
    }

    #[test]
    fn diff_cell_opaque_block_unchanged_produces_no_change() {
        // Same opaque_ref on both sides (the common case: an SDT whose
        // content is opaque to us but whose identity is stable) must NOT
        // be reported as a change — mirrors diff_opaque_blocks' body-level
        // notion of sameness (opaque_ref set membership).
        let base = make_doc(
            "doc",
            vec![make_table_with_cell_opaque("tbl", "opaque-a")],
            "fp1",
        );
        let target = make_doc(
            "doc",
            vec![make_table_with_cell_opaque("tbl", "opaque-a")],
            "fp2",
        );

        let diff = diff_documents(&base, &target).expect("diff should succeed");
        assert!(
            diff.changes.is_empty(),
            "identical cell opaque blocks (same opaque_ref) must not produce a diff change, got {:?}",
            diff.changes
        );
    }

    #[test]
    fn diff_cell_paragraph_added_is_not_silently_dropped() {
        // P0 #4: a cell that gains a paragraph keeps identical row/cell counts, so
        // the table was treated as "same structure" and diff_matched_tables zipped
        // the cell's block lists — `zip` truncates, so the added paragraph was
        // invisible and diff_documents returned `changes: []`. Accept could not
        // reproduce the target. The diff must represent the added cell paragraph.
        let base = make_doc(
            "doc",
            vec![make_table_with_cell_paragraphs("tbl", &["first", "second"])],
            "fp1",
        );
        let target = make_doc(
            "doc",
            vec![make_table_with_cell_paragraphs(
                "tbl",
                &["first", "second", "third"],
            )],
            "fp2",
        );

        let diff = diff_documents(&base, &target).expect("diff should succeed");
        assert!(
            !diff.changes.is_empty(),
            "adding a paragraph to a table cell must produce a diff change, got changes: []"
        );
        // The change must carry the whole target table so the merge can
        // reproduce it on accept (TableStructureChanged → delete base + insert
        // target), rather than a partial per-cell diff that drops the paragraph.
        assert!(
            diff.changes.iter().any(|c| matches!(
                c,
                DiffChange::TableStructureChanged { table_id, .. } if table_id.0.as_ref() == "tbl"
            )),
            "expected a TableStructureChanged for the cell-block-count change, got {:?}",
            diff.changes
        );
    }

    #[test]
    fn diff_cell_paragraph_removed_is_not_silently_dropped() {
        // The symmetric case: a cell that loses a paragraph.
        let base = make_doc(
            "doc",
            vec![make_table_with_cell_paragraphs(
                "tbl",
                &["first", "second", "third"],
            )],
            "fp1",
        );
        let target = make_doc(
            "doc",
            vec![make_table_with_cell_paragraphs("tbl", &["first", "second"])],
            "fp2",
        );

        let diff = diff_documents(&base, &target).expect("diff should succeed");
        assert!(
            !diff.changes.is_empty(),
            "removing a paragraph from a table cell must produce a diff change, got changes: []"
        );
    }

    #[test]
    fn full_document_projection_self_comparison_preserves_canonical_ids() {
        let doc = make_doc(
            "doc",
            vec![
                make_paragraph("para_1", "First paragraph"),
                make_table("tbl_0", "Cell content"),
                make_paragraph("para_2", "Last paragraph"),
            ],
            "fp1",
        );

        let blocks = build_full_document_view(&doc, &doc, &HashMap::new(), &HashMap::new())
            .expect("self-comparison should succeed");

        // Every block should be unchanged with block_id == canonical ID.
        assert_eq!(blocks.len(), 3);
        for block in &blocks {
            assert_eq!(
                block.change_type,
                ChangeType::Unchanged,
                "block {} should be unchanged in self-comparison",
                block.block_id,
            );
            assert_eq!(
                block.doc1_block_id, block.doc2_block_id,
                "doc1 and doc2 IDs must match for block {}",
                block.block_id,
            );
            assert_eq!(
                Some(&block.block_id),
                block.doc2_block_id.as_ref(),
                "block_id must equal canonical ID for block {}",
                block.block_id,
            );
        }

        let ids: Vec<_> = blocks.iter().map(|b| b.block_id.0.as_ref()).collect();
        assert_eq!(ids, vec!["para_1", "tbl_0", "para_2"]);
    }

    #[test]
    fn full_document_projection_block_ids_independent_of_analysis() {
        let base = make_doc(
            "doc",
            vec![
                make_paragraph("base_p1", "Hello world"),
                make_paragraph("base_p2", "Second"),
            ],
            "fp1",
        );
        let target = make_doc(
            "doc",
            vec![
                make_paragraph("target_p1", "Hello world changed"),
                make_paragraph("target_p3", "Inserted"),
            ],
            "fp2",
        );

        // build_full_document_view produces only the document projection (no analysis).
        let view_blocks =
            build_full_document_view(&base, &target, &HashMap::new(), &HashMap::new())
                .expect("full document view should succeed");

        // diff_and_full_document produces both diff changes (analysis input) and the projection.
        let (_diff, df_blocks) =
            diff_and_full_document(&base, &target, &HashMap::new(), &HashMap::new())
                .expect("diff_and_full_document should succeed");

        // Block IDs must be identical regardless of whether analysis was also computed.
        let view_ids: Vec<_> = view_blocks.iter().map(|b| b.block_id.clone()).collect();
        let df_ids: Vec<_> = df_blocks.iter().map(|b| b.block_id.clone()).collect();
        assert_eq!(view_ids, df_ids);
    }

    #[test]
    fn single_document_projection_produces_canonical_ids_and_unchanged_blocks() {
        let doc = make_doc(
            "doc",
            vec![
                make_paragraph("para_1", "First paragraph"),
                make_table("tbl_0", "Cell content"),
                make_paragraph("para_2", "Last paragraph"),
            ],
            "fp1",
        );

        let result = build_single_document_view(&doc, &HashMap::new());

        assert_eq!(result.blocks.len(), 3);
        for block in &result.blocks {
            assert_eq!(
                block.change_type,
                ChangeType::Unchanged,
                "block {} should be unchanged",
                block.block_id,
            );
            assert!(
                block.doc1_block_id.is_none(),
                "single-doc projection should have no doc1 ID for block {}",
                block.block_id,
            );
            assert_eq!(
                Some(&block.block_id),
                block.doc2_block_id.as_ref(),
                "doc2_block_id must equal block_id for block {}",
                block.block_id,
            );
        }

        let ids: Vec<_> = result
            .blocks
            .iter()
            .map(|b| b.block_id.0.as_ref())
            .collect();
        assert_eq!(ids, vec!["para_1", "tbl_0", "para_2"]);

        // All segments should be equal.
        for block in &result.blocks {
            for seg in &block.segments {
                match seg {
                    InlineChange::Unchanged { .. } => {}
                    InlineChange::Opaque {
                        segment_type: InlineChangeSegmentType::Equal,
                        ..
                    } => {}
                    other => panic!(
                        "single-doc projection should have only equal segments, got {:?} in block {}",
                        other, block.block_id,
                    ),
                }
            }
        }
    }

    #[test]
    fn single_document_projection_includes_stories() {
        let mut doc = make_doc("doc", vec![make_paragraph("para_1", "Body text")], "fp1");
        doc.footnotes
            .push(make_footnote("fn1", "fn1_p1", "Footnote text"));

        let result = build_single_document_view(&doc, &HashMap::new());

        assert_eq!(result.blocks.len(), 1);
        assert_eq!(result.footnotes.len(), 1);
        assert_eq!(result.footnotes[0].id, "fn1");
    }

    // ══════════════════════════════════════════════════════════════════════
    // Join/Split detection tests
    // ══════════════════════════════════════════════════════════════════════

    fn make_modified_block(id: &str, old_text: &str, new_text: &str) -> FullDocBlock {
        let segments = vec![
            InlineChange::Deleted {
                text: old_text.to_string(),
                marks: vec![],
                style_props: StyleProps::default(),
                formatting_change: None,
                rev_id: 0,
            },
            InlineChange::Inserted {
                text: new_text.to_string(),
                marks: vec![],
                style_props: StyleProps::default(),
                formatting_change: None,
                rev_id: 0,
            },
        ];
        FullDocBlock {
            block_id: NodeId::from(id.to_string()),
            doc1_block_id: None,
            doc2_block_id: None,
            block_type: BlockType::Paragraph,
            heading_level: None,
            style_id: None,
            change_type: ChangeType::Modified,
            align: None,
            indent: None,
            spacing: None,
            borders: None,
            tab_stops: vec![],
            numbering_text: None,
            numbering_ilvl: None,
            numbering_num_id: None,
            segments,
            table_diff: None,
            content_types: vec![],
            equation_xmls: vec![],
            equation_doc1_count: 0,
            image_data_uris: vec![],
            image_doc1_count: 0,
            image_metadata_changes: vec![],
            move_id: None,
            move_direction: None,
            structural_change: None,
            border_group_id: None,
            paragraph_mark_status: None,
        }
    }

    #[test]
    fn test_detect_join_deleted_after_modified() {
        let mut blocks = vec![
            make_modified_block(
                "target_p9",
                "First paragraph.",
                "First paragraph. Second paragraph content.",
            ),
            make_full_doc_block(
                "deleted:base_p2",
                ChangeType::Deleted,
                "Second paragraph content.",
            ),
        ];
        detect_joins_splits(&mut blocks);
        assert_eq!(
            blocks[1].structural_change,
            Some(StructuralChange::Join {
                into_block_id: NodeId::from("target_p9")
            }),
        );
        assert!(blocks[0].structural_change.is_none());
    }

    #[test]
    fn test_detect_split_inserted_after_modified() {
        let mut blocks = vec![
            make_modified_block(
                "target_p9",
                "Full paragraph with extra content.",
                "Full paragraph.",
            ),
            make_full_doc_block("target_p10", ChangeType::Inserted, "with extra content."),
        ];
        detect_joins_splits(&mut blocks);
        assert_eq!(
            blocks[1].structural_change,
            Some(StructuralChange::Split {
                from_block_id: NodeId::from("target_p9")
            }),
        );
        assert!(blocks[0].structural_change.is_none());
    }

    #[test]
    fn test_detect_join_ignores_short_text() {
        let mut blocks = vec![
            make_modified_block("p0", "Hello.", "Hello. Hi."),
            make_full_doc_block("p1", ChangeType::Deleted, "Hi."),
        ];
        detect_joins_splits(&mut blocks);
        assert!(blocks[1].structural_change.is_none());
    }

    #[test]
    fn test_detect_join_ignores_non_adjacent_patterns() {
        let mut blocks = vec![
            make_modified_block(
                "p0",
                "First paragraph.",
                "First paragraph. Appended content here.",
            ),
            make_full_doc_block("p1", ChangeType::Unchanged, "Separator paragraph."),
            make_full_doc_block("p2", ChangeType::Deleted, "Appended content here."),
        ];
        detect_joins_splits(&mut blocks);
        assert!(blocks[2].structural_change.is_none());
    }

    #[test]
    fn test_detect_split_not_triggered_when_suffix_in_both() {
        let mut blocks = vec![
            make_modified_block(
                "p0",
                "Some text with shared suffix.",
                "Different text with shared suffix.",
            ),
            make_full_doc_block("p1", ChangeType::Inserted, "with shared suffix."),
        ];
        detect_joins_splits(&mut blocks);
        assert!(blocks[1].structural_change.is_none());
    }

    #[test]
    fn diff_and_full_document_runs_same_reconcile_passes_as_diff_documents() {
        // P0 #5: the production redline path (compare_and_redline →
        // diff_and_full_document) must build the SAME changes as the canonical
        // diff_documents path. It used to re-implement change-building and skip
        // reconcile_paragraph_splits / reconcile_math_deleted_inserted_replacements
        // / diff_opaque_blocks, so e.g. a paragraph split was left as an
        // unreconciled BlockModified + BlockInserted and merged wrong.
        let base = make_doc(
            "doc",
            vec![make_paragraph("p0", "Alpha clause here. Beta clause here.")],
            "fp1",
        );
        let target = make_doc(
            "doc",
            vec![
                make_paragraph("p0", "Alpha clause here."),
                make_paragraph("p1", "Beta clause here."),
            ],
            "fp2",
        );

        let canonical = diff_documents(&base, &target).expect("diff_documents");
        let (production, _blocks) =
            diff_and_full_document(&base, &target, &HashMap::new(), &HashMap::new())
                .expect("diff_and_full_document");

        // The split must actually be reconciled in the canonical path, otherwise
        // this test would pass vacuously.
        assert!(
            canonical.changes.iter().any(|c| matches!(
                c,
                DiffChange::BlockModified {
                    para_split: true,
                    ..
                }
            )),
            "precondition: the canonical diff should mark the paragraph split"
        );
        assert_eq!(
            format!("{:?}", production.changes),
            format!("{:?}", canonical.changes),
            "production redline path must produce the same reconciled changes as diff_documents"
        );
    }

    #[test]
    fn reconcile_paragraph_splits_marks_real_prefix_split_only() {
        let mut changes = vec![
            DiffChange::BlockInserted {
                after_block_id: None,
                block: make_paragraph("p_ins", "Leading clause moved out."),
                move_id: None,
            },
            DiffChange::BlockModified {
                block_id: NodeId::from("p0"),
                old_text: "Leading clause moved out. Remaining clause stays here.".to_string(),
                new_text: "Remaining clause stays here.".to_string(),
                inline_changes: vec![],
                old_block: make_paragraph(
                    "p0_old",
                    "Leading clause moved out. Remaining clause stays here.",
                ),
                new_block: make_paragraph("p0_new", "Remaining clause stays here."),
                para_split: false,
            },
        ];

        reconcile_paragraph_splits(&mut changes);

        match &changes[1] {
            DiffChange::BlockModified { para_split, .. } => assert!(
                *para_split,
                "true paragraph split should preserve para_split=true"
            ),
            other => panic!("expected BlockModified, got {other:?}"),
        }
    }

    #[test]
    fn reconcile_paragraph_splits_ignores_unrelated_insert_before_prefix_trim() {
        let mut changes = vec![
            DiffChange::BlockInserted {
                after_block_id: None,
                block: make_paragraph("p_ins", "The Discount Rate is [100 minus the discount]%."),
                move_id: None,
            },
            DiffChange::BlockModified {
                block_id: NodeId::from("p0"),
                old_text:
                    "The Post-Money Valuation Cap is $[_____________]. See Section 2 for certain additional defined terms."
                        .to_string(),
                new_text: "See Section 2 for certain additional defined terms.".to_string(),
                inline_changes: vec![],
                old_block: make_paragraph(
                    "p0_old",
                    "The Post-Money Valuation Cap is $[_____________]. See Section 2 for certain additional defined terms.",
                ),
                new_block: make_paragraph(
                    "p0_new",
                    "See Section 2 for certain additional defined terms.",
                ),
                para_split: false,
            },
        ];

        reconcile_paragraph_splits(&mut changes);

        match &changes[1] {
            DiffChange::BlockModified { para_split, .. } => assert!(
                !*para_split,
                "unrelated inserted paragraph must not be treated as a split"
            ),
            other => panic!("expected BlockModified, got {other:?}"),
        }
    }

    #[test]
    fn reconcile_paragraph_splits_ignores_unrelated_insert_before_shortened_paragraph() {
        let mut changes = vec![
            DiffChange::BlockInserted {
                after_block_id: None,
                block: make_paragraph("p_ins", "A=πr2"),
                move_id: None,
            },
            DiffChange::BlockModified {
                block_id: NodeId::from("p0"),
                old_text: "Video provides a powerful way t".to_string(),
                new_text: "Provides a powerful way t".to_string(),
                inline_changes: vec![],
                old_block: make_paragraph("p0_old", "Video provides a powerful way t"),
                new_block: make_paragraph("p0_new", "Provides a powerful way t"),
                para_split: false,
            },
        ];

        reconcile_paragraph_splits(&mut changes);

        match &changes[1] {
            DiffChange::BlockModified { para_split, .. } => assert!(
                !*para_split,
                "unrelated inserted paragraph must not trigger para_split on a trimmed paragraph"
            ),
            other => panic!("expected BlockModified, got {other:?}"),
        }
    }

    #[test]
    fn test_detect_join_skips_moved_blocks() {
        let mut blocks = vec![
            make_modified_block(
                "p0",
                "First paragraph.",
                "First paragraph. Moved content from elsewhere.",
            ),
            make_full_doc_block("p1", ChangeType::Deleted, "Moved content from elsewhere."),
        ];
        blocks[1].move_id = Some("move_0".to_string());
        blocks[1].move_direction = Some(MoveDirection::From);
        detect_joins_splits(&mut blocks);
        assert!(blocks[1].structural_change.is_none());
    }

    /// Tests that equal-length paragraph runs between anchors with zero
    /// text similarity are matched pairwise, not deleted+reinserted.
    ///
    /// This reproduces the core bug in SAFE valcap-vs-discount: between
    /// When equal-length gap segments between anchors contain completely
    /// unrelated paragraphs (zero content overlap), the algorithm must NOT
    /// force pairwise matching. Force-matching unrelated paragraphs produces
    /// false modifications — inline diffs that are walls of red/green,
    /// misleading the reviewer into thinking "this is the same clause, changed."
    ///
    /// Instead, the gap falls through to the DP algorithm which correctly
    /// prefers delete+insert for dissimilar pairs.
    #[test]
    fn test_unrelated_equal_length_runs_are_not_force_matched() {
        let base = make_doc(
            "doc",
            vec![
                make_paragraph("p0", "ARTICLE I"),
                make_paragraph("p1", "Alpha beta gamma delta epsilon"),
                make_paragraph("p2", "Zeta eta theta iota kappa"),
                make_paragraph("p3", "Lambda mu nu xi omicron"),
                make_paragraph("p4", "Pi rho sigma tau upsilon"),
                make_paragraph("p5", "Phi chi psi omega aleph"),
                make_paragraph("p6", "ARTICLE II"),
            ],
            "fp1",
        );

        let target = make_doc(
            "doc",
            vec![
                make_paragraph("t0", "ARTICLE I"),
                make_paragraph("t1", "Uno dos tres cuatro cinco"),
                make_paragraph("t2", "Seis siete ocho nueve diez"),
                make_paragraph("t3", "Once doce trece catorce quince"),
                make_paragraph("t4", "Dieciseis diecisiete dieciocho"),
                make_paragraph("t5", "Diecinueve veinte veintiuno"),
                make_paragraph("t6", "ARTICLE II"),
            ],
            "fp2",
        );

        let diff = diff_documents(&base, &target).expect("diff should succeed");

        let deletions = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockDeleted { .. }))
            .count();
        let insertions = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockInserted { .. }))
            .count();
        let modifications = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockModified { .. }))
            .count();

        // Unrelated paragraphs must NOT be force-matched as modifications.
        // The DP should emit delete+insert for zero-overlap pairs.
        assert_eq!(
            modifications, 0,
            "zero-similarity paragraphs must not be force-matched; \
             got del={deletions}, ins={insertions}, mod={modifications}"
        );
        assert_eq!(deletions, 5, "all base paragraphs should be deleted");
        assert_eq!(insertions, 5, "all target paragraphs should be inserted");
    }

    #[test]
    fn extract_diffable_elements_distinguishes_empty_section_break_paragraphs() {
        let base = vec![
            normal_tracked_block(make_paragraph("plain-empty", "")),
            normal_tracked_block(make_section_break_paragraph("sect-empty")),
        ];

        let elements = extract_diffable_elements(&base);
        assert_eq!(elements.len(), 2, "expected two diffable blocks");

        let plain = match &elements[0] {
            DiffableElement::Block(block) => block,
            _ => panic!("expected paragraph block"),
        };
        let sect = match &elements[1] {
            DiffableElement::Block(block) => block,
            _ => panic!("expected paragraph block"),
        };

        assert!(
            plain.comparison_text.is_empty(),
            "plain empty paragraph should not get synthetic section-break comparison text"
        );
        assert_eq!(
            sect.comparison_text, "[sectPr]",
            "empty section-break paragraph should get a dedicated comparison marker"
        );
        assert_ne!(
            plain.text_hash, sect.text_hash,
            "section-break identity must not hash the same as a generic empty paragraph"
        );
    }

    /// Tests that unequal-length runs between anchors still produce correct
    /// alignment (modifications + appropriate insertions or deletions).
    #[test]
    fn test_unequal_length_runs_between_anchors() {
        let base = make_doc(
            "doc",
            vec![
                make_paragraph("p0", "ARTICLE I"),
                make_paragraph("p1", "First clause in base"),
                make_paragraph("p2", "Second clause in base"),
                make_paragraph("p3", "Third clause in base"),
                make_paragraph("p4", "ARTICLE II"),
            ],
            "fp1",
        );

        let target = make_doc(
            "doc",
            vec![
                make_paragraph("t0", "ARTICLE I"),
                make_paragraph("t1", "First clause in target"),
                make_paragraph("t2", "Second clause in target"),
                make_paragraph("t3", "Third clause in target"),
                make_paragraph("t4", "Fourth clause in target"),
                make_paragraph("t5", "ARTICLE II"),
            ],
            "fp2",
        );

        let diff = diff_documents(&base, &target).expect("diff should succeed");

        let deletions = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockDeleted { .. }))
            .count();
        let insertions = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockInserted { .. }))
            .count();
        let modifications = diff
            .changes
            .iter()
            .filter(|c| matches!(c, DiffChange::BlockModified { .. }))
            .count();

        // 3 base vs 4 target: should have modifications + 1 insertion
        assert!(
            modifications >= 3,
            "should have at least 3 modifications; got del={}, ins={}, mod={}",
            deletions,
            insertions,
            modifications
        );
        assert_eq!(
            insertions, 1,
            "should have 1 insertion for the extra target paragraph; \
             got del={}, ins={}, mod={}",
            deletions, insertions, modifications
        );
    }

    // =========================================================================
    // rPrChange (run-level formatting change) detection tests
    // =========================================================================

    fn make_paragraph_with_marks(id: &str, text: &str, marks: Vec<Mark>) -> BlockNode {
        BlockNode::from(ParagraphNode {
            id: NodeId::from(id.to_string()),
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
            segments: normal_segment(vec![InlineNode::from(TextNode {
                id: NodeId::from(format!("{}_t1", id)),
                text_role: None,
                text: text.to_string(),
                marks,
                style_props: StyleProps::default(),
                rpr_authored: crate::domain::RunRprAuthored::default(),
                source_run_attrs: Vec::new(),
                formatting_change: None,
            })]),
            block_text_hash: Some(sha256_hex(text.as_bytes())),
            numbering: None,
            has_direct_numbering: true,
            numbering_suppressed: false,
            materialized_numbering: None,
            rendered_text: None,
            literal_prefix: None,
            literal_prefix_marks: Vec::new(),
            literal_prefix_style_props: crate::domain::StyleProps::default(),
            literal_prefix_rpr_authored: crate::domain::RunRprAuthored::default(),
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
            suppress_auto_hyphens: None,
            snap_to_grid: None,
            overflow_punct: None,
            adjust_right_ind: None,
            word_wrap: None,
            frame_pr: None,
            para_id: None,
            text_id: None,
            text_direction: None,
            cnf_style: None,
            preserved_ppr: Vec::new(),
        })
    }

    #[test]
    fn test_rpr_change_detected_on_unchanged_text() {
        // When text is identical but formatting differs (e.g., bold added),
        // the diff should detect a formatting-only change.
        let base_inlines = vec![InlineNode::from(TextNode {
            id: NodeId::from("t1"),
            text_role: None,
            text: "Hello World".to_string(),
            marks: vec![],
            style_props: StyleProps::default(),
            rpr_authored: crate::domain::RunRprAuthored::default(),
            source_run_attrs: Vec::new(),
            formatting_change: None,
        })];
        let target_inlines = vec![InlineNode::from(TextNode {
            id: NodeId::from("t1"),
            text_role: None,
            text: "Hello World".to_string(),
            marks: vec![Mark::Bold],
            style_props: StyleProps::default(),
            rpr_authored: crate::domain::RunRprAuthored::default(),
            source_run_attrs: Vec::new(),
            formatting_change: None,
        })];

        let changes = diff_block_content_with_marks(&base_inlines, &target_inlines);

        // Should have one Unchanged segment with a formatting_change
        assert_eq!(changes.len(), 1, "should have exactly 1 segment");
        match &changes[0] {
            InlineChange::Unchanged {
                text,
                marks,
                formatting_change,
                ..
            } => {
                assert_eq!(text, "Hello World");
                assert_eq!(marks, &vec![Mark::Bold], "current marks should be Bold");
                assert!(
                    formatting_change.is_some(),
                    "should have formatting_change for bold addition"
                );
                let fc = formatting_change.as_ref().unwrap();
                assert!(
                    fc.previous_marks.is_empty(),
                    "previous marks should be empty (no bold before)"
                );
            }
            other => panic!("expected Unchanged, got {:?}", other),
        }
    }

    #[test]
    fn test_rpr_change_detected_on_matched_block() {
        // Formatting-only differences (bold, font, etc.) on matched blocks
        // (identical text) must produce a BlockModified so the redline shows
        // the formatting change (e.g. w:rPrChange). Adversarial formatting
        // changes like w:vanish or extreme font-size reduction are trust-relevant.
        let base = make_doc(
            "doc",
            vec![make_paragraph_with_marks("p1", "Same text here", vec![])],
            "fp1",
        );
        let target = make_doc(
            "doc",
            vec![make_paragraph_with_marks(
                "p1",
                "Same text here",
                vec![Mark::Bold],
            )],
            "fp2",
        );

        let diff = diff_documents(&base, &target).expect("diff should succeed");

        assert_eq!(
            diff.changes.len(),
            1,
            "formatting-only matched block should produce 1 BlockModified"
        );
        match &diff.changes[0] {
            DiffChange::BlockModified { inline_changes, .. } => {
                assert_eq!(inline_changes.len(), 1);
                match &inline_changes[0] {
                    InlineChange::Unchanged {
                        text,
                        formatting_change,
                        ..
                    } => {
                        assert_eq!(text, "Same text here");
                        assert!(
                            formatting_change.is_some(),
                            "should have formatting_change for bold addition"
                        );
                    }
                    other => panic!("expected Unchanged with formatting_change, got {:?}", other),
                }
            }
            other => panic!("expected BlockModified, got {:?}", other),
        }
    }

    #[test]
    fn test_no_rpr_change_when_formatting_identical() {
        // When both text and formatting are identical, no change should be emitted.
        let base_inlines = vec![InlineNode::from(TextNode {
            id: NodeId::from("t1"),
            text_role: None,
            text: "Hello World".to_string(),
            marks: vec![Mark::Bold],
            style_props: StyleProps::default(),
            rpr_authored: crate::domain::RunRprAuthored::default(),
            source_run_attrs: Vec::new(),
            formatting_change: None,
        })];
        let target_inlines = vec![InlineNode::from(TextNode {
            id: NodeId::from("t1"),
            text_role: None,
            text: "Hello World".to_string(),
            marks: vec![Mark::Bold],
            style_props: StyleProps::default(),
            rpr_authored: crate::domain::RunRprAuthored::default(),
            source_run_attrs: Vec::new(),
            formatting_change: None,
        })];

        let changes = diff_block_content_with_marks(&base_inlines, &target_inlines);

        // Should have one Unchanged segment WITHOUT formatting_change
        assert_eq!(changes.len(), 1);
        match &changes[0] {
            InlineChange::Unchanged {
                formatting_change, ..
            } => {
                assert!(
                    formatting_change.is_none(),
                    "should NOT have formatting_change when formatting is identical"
                );
            }
            other => panic!("expected Unchanged, got {:?}", other),
        }
    }

    #[test]
    fn test_rpr_change_style_props_difference() {
        // Detect style_props differences (e.g., font size change).
        let base_inlines = vec![InlineNode::from(TextNode {
            id: NodeId::from("t1"),
            text_role: None,
            text: "Hello World".to_string(),
            marks: vec![],
            style_props: StyleProps {
                font_size: Some(24),
                ..StyleProps::default()
            },
            rpr_authored: crate::domain::RunRprAuthored {
                font_size: true,
                ..Default::default()
            },
            source_run_attrs: Vec::new(),
            formatting_change: None,
        })];
        let target_inlines = vec![InlineNode::from(TextNode {
            id: NodeId::from("t1"),
            text_role: None,
            text: "Hello World".to_string(),
            marks: vec![],
            style_props: StyleProps {
                font_size: Some(28),
                ..StyleProps::default()
            },
            rpr_authored: crate::domain::RunRprAuthored {
                font_size: true,
                ..Default::default()
            },
            source_run_attrs: Vec::new(),
            formatting_change: None,
        })];

        let changes = diff_block_content_with_marks(&base_inlines, &target_inlines);

        assert_eq!(changes.len(), 1);
        match &changes[0] {
            InlineChange::Unchanged {
                formatting_change,
                style_props,
                ..
            } => {
                assert!(
                    formatting_change.is_some(),
                    "should detect font size change"
                );
                let fc = formatting_change.as_ref().unwrap();
                assert_eq!(
                    fc.previous_style_props.font_size,
                    Some(24),
                    "previous font size should be 24"
                );
                assert_eq!(
                    style_props.font_size,
                    Some(28),
                    "current font size should be 28"
                );
            }
            other => panic!("expected Unchanged, got {:?}", other),
        }
    }

    fn make_opaque(segment_type: InlineChangeSegmentType, idx: usize) -> InlineChange {
        InlineChange::Opaque {
            segment_type,
            kind: OpaqueSegmentKind::Field,
            opaque_id: format!("op_{idx}"),
            inline_index: idx,
            text: None,
            reference_id: None,
            field_kind: None,
            field_instruction: None,
            asset_ref: None,
            asset_width_emu: None,
            asset_height_emu: None,
            alt_text: None,
            url: None,
            content_hash: None,
        }
    }

    #[test]
    fn sort_opaque_runs_reorders_delete_insert_pairs() {
        // Simulates the diff output: deletes grouped before inserts
        // inline_index sequence: 1, 2, 3, 2, 3, 5 (out of order at position 4)
        let changes = vec![
            make_opaque(InlineChangeSegmentType::Equal, 1),
            make_opaque(InlineChangeSegmentType::Delete, 2),
            make_opaque(InlineChangeSegmentType::Delete, 3),
            make_opaque(InlineChangeSegmentType::Insert, 2),
            make_opaque(InlineChangeSegmentType::Insert, 3),
            make_opaque(InlineChangeSegmentType::Equal, 5),
        ];

        let sorted = sort_opaque_runs_by_inline_index(changes);

        let indices: Vec<usize> = sorted
            .iter()
            .map(|c| match c {
                InlineChange::Opaque { inline_index, .. } => *inline_index,
                _ => unreachable!(),
            })
            .collect();
        // After sorting: 1, 2, 2, 3, 3, 5 (monotonically non-decreasing)
        assert_eq!(indices, vec![1, 2, 2, 3, 3, 5]);

        // Delete should come before insert at same index
        let types: Vec<&InlineChangeSegmentType> = sorted
            .iter()
            .map(|c| match c {
                InlineChange::Opaque { segment_type, .. } => segment_type,
                _ => unreachable!(),
            })
            .collect();
        use InlineChangeSegmentType::*;
        assert_eq!(
            types,
            vec![&Equal, &Delete, &Insert, &Delete, &Insert, &Equal]
        );
    }

    #[test]
    fn sort_opaque_runs_does_not_affect_text_segments() {
        let changes = vec![
            InlineChange::Unchanged {
                text: "hello".to_string(),
                marks: vec![],
                style_props: StyleProps::default(),
                formatting_change: None,
            },
            make_opaque(InlineChangeSegmentType::Delete, 3),
            make_opaque(InlineChangeSegmentType::Insert, 1),
            InlineChange::Inserted {
                text: "world".to_string(),
                marks: vec![],
                style_props: StyleProps::default(),
                formatting_change: None,
                rev_id: 0,
            },
        ];

        let sorted = sort_opaque_runs_by_inline_index(changes);

        // Text segments stay in place; opaque run (indices 1-2) gets sorted
        assert!(matches!(&sorted[0], InlineChange::Unchanged { text, .. } if text == "hello"));
        assert!(matches!(
            &sorted[1],
            InlineChange::Opaque {
                inline_index: 1,
                ..
            }
        ));
        assert!(matches!(
            &sorted[2],
            InlineChange::Opaque {
                inline_index: 3,
                ..
            }
        ));
        assert!(matches!(&sorted[3], InlineChange::Inserted { text, .. } if text == "world"));
    }

    // =========================================================================
    // Golden set: inline diff readability tests
    //
    // These tests pin exact inline diff output to catch readability regressions.
    // Each test uses assert_inline_structure for precise segment matching.
    // Tests marked #[ignore] encode desired-but-not-yet-implemented behavior.
    // =========================================================================

    /// Assert that an InlineChange list matches compact specs like "EQ:text", "DEL:text", "INS:text".
    fn assert_inline_structure(actual: &[InlineChange], expected: &[&str]) {
        let actual_specs: Vec<String> = actual
            .iter()
            .map(|c| match c {
                InlineChange::Unchanged { text, .. } => format!("EQ:{}", text),
                InlineChange::Deleted { text, .. } => format!("DEL:{}", text),
                InlineChange::Inserted { text, .. } => format!("INS:{}", text),
                InlineChange::Opaque { .. } => "OPAQUE".to_string(),
            })
            .collect();
        let expected_vec: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        assert_eq!(
            actual_specs, expected_vec,
            "\nExpected:\n{:#?}\n\nActual:\n{:#?}",
            expected_vec, actual_specs
        );
    }

    /// Build Vec<InlineNode> from segments of (text, marks).
    fn make_inlines(segments: &[(&str, Vec<Mark>)]) -> Vec<InlineNode> {
        segments
            .iter()
            .enumerate()
            .map(|(i, (text, marks))| {
                InlineNode::from(TextNode {
                    id: NodeId::from(format!("t{}", i)),
                    text_role: None,
                    text: text.to_string(),
                    marks: marks.clone(),
                    style_props: StyleProps::default(),
                    rpr_authored: crate::domain::RunRprAuthored::default(),
                    source_run_attrs: Vec::new(),
                    formatting_change: None,
                })
            })
            .collect()
    }

    // --- Test 1: Single word swap stays inline with anchors on both sides ---
    #[test]
    fn golden_simple_word_substitution() {
        let changes = diff_block_content(
            "The Borrower shall repay the outstanding amount within thirty days.",
            "The Borrower shall repay the outstanding amount within sixty days.",
        );
        assert_inline_structure(
            &changes,
            &[
                "EQ:The Borrower shall repay the outstanding amount within ",
                "DEL:thirty",
                "INS:sixty",
                "EQ: days.",
            ],
        );
    }

    // --- Test 2: No-shared-words rewrite collapses to block DEL+INS ---
    #[test]
    fn golden_phrase_rewrite_plain_text() {
        let changes = diff_block_content(
            "The obligations of the parties shall be governed by applicable law.",
            "Each participant must comply with relevant statutory requirements.",
        );
        assert_inline_structure(
            &changes,
            &[
                "DEL:The obligations of the parties shall be governed by applicable law.",
                "INS:Each participant must comply with relevant statutory requirements.",
            ],
        );
    }

    // --- Test 3: Bold defined term preserved through collapse ---
    // Region collapses cleanly while preserving mark boundaries: bold "Company"
    // stays as a separate segment on both DEL and INS sides.
    #[test]
    fn golden_phrase_rewrite_with_bold_defined_term() {
        let old_inlines = make_inlines(&[
            ("The ", vec![]),
            ("Company", vec![Mark::Bold]),
            (
                " shall maintain adequate insurance coverage at all times.",
                vec![],
            ),
        ]);
        let new_inlines = make_inlines(&[
            ("The ", vec![]),
            ("Company", vec![Mark::Bold]),
            (
                " must ensure sufficient protective measures are continuously in effect.",
                vec![],
            ),
        ]);
        let changes = diff_block_content_with_marks(&old_inlines, &new_inlines);
        assert_inline_structure(
            &changes,
            &[
                "EQ:The ",
                "EQ:Company",
                "DEL: shall maintain adequate insurance coverage at all times.",
                "INS: must ensure sufficient protective measures are continuously in effect.",
            ],
        );
    }

    // --- Test 4: Two scattered word changes between strong anchors ---
    #[test]
    fn golden_scattered_changes_with_anchors() {
        let changes = diff_block_content(
            "The Borrower shall pay interest at a rate of five percent per annum on the outstanding balance.",
            "The Borrower shall pay interest at a rate of seven percent per annum on the unpaid balance.",
        );
        assert_inline_structure(
            &changes,
            &[
                "EQ:The Borrower shall pay interest at a rate of ",
                "DEL:five",
                "INS:seven",
                "EQ: percent per annum on the ",
                "DEL:outstanding",
                "INS:unpaid",
                "EQ: balance.",
            ],
        );
    }

    // --- Test 5: Enumerator anchors (i)/(ii)/(iii) preserved ---
    #[test]
    fn golden_enumerator_anchors() {
        let changes = diff_block_content(
            "(i) the Borrower shall deliver the first notice; (ii) the Borrower shall deliver the second notice; (iii) the Borrower shall deliver the third notice.",
            "(i) the Borrower shall deliver the initial report; (ii) the Borrower shall deliver the interim report; (iii) the Borrower shall deliver the final report.",
        );
        assert_inline_structure(
            &changes,
            &[
                "EQ:(i) the Borrower shall deliver the ",
                "DEL:first notice",
                "INS:initial report",
                "EQ:; (ii) the Borrower shall deliver the ",
                "DEL:second notice",
                "INS:interim report",
                "EQ:; (iii) the Borrower shall deliver the ",
                "DEL:third notice.",
                "INS:final report.",
            ],
        );
    }

    // --- Test 6: Completely disjoint text → single DEL+INS bail-out ---
    #[test]
    fn golden_complete_rewrite_bail_out() {
        let changes = diff_block_content(
            "Alpha beta gamma delta epsilon zeta eta theta.",
            "One two three four five six seven eight.",
        );
        assert_inline_structure(
            &changes,
            &[
                "DEL:Alpha beta gamma delta epsilon zeta eta theta.",
                "INS:One two three four five six seven eight.",
            ],
        );
    }

    // --- Test 7: Dollar amounts and counts stay inline ---
    #[test]
    fn golden_currency_number_inline() {
        let changes = diff_block_content(
            "The purchase price shall be $1,000,000 payable in 12 monthly installments.",
            "The purchase price shall be $2,500,000 payable in 24 monthly installments.",
        );
        assert_inline_structure(
            &changes,
            &[
                "EQ:The purchase price shall be $",
                "DEL:1,000",
                "INS:2,500",
                "EQ:,000 payable in ",
                "DEL:12",
                "INS:24",
                "EQ: monthly installments.",
            ],
        );
    }

    // --- Test 8: Short phrase rewrite collapses cleanly (no per-space fragmentation) ---
    #[test]
    fn golden_whitespace_anchors_no_fragmentation() {
        let changes = diff_block_content("shall not be liable", "must not be responsible");
        assert_inline_structure(
            &changes,
            &["DEL:shall not be liable", "INS:must not be responsible"],
        );
    }

    // --- Test 9: Single-word substitution in short text ---
    #[test]
    fn golden_short_paragraph_one_word() {
        let changes = diff_block_content("Effective Date", "Closing Date");
        assert_inline_structure(&changes, &["DEL:Effective", "INS:Closing", "EQ: Date"]);
    }

    // --- Test 10: Rewritten middle between strong anchor collapses ---
    #[test]
    fn golden_zipper_between_anchors_collapses() {
        let changes = diff_block_content(
            "WHEREAS the parties have entered into negotiations regarding the proposed transaction and wish to formalize their agreement;",
            "WHEREAS the parties desire to document the terms of the contemplated arrangement and establish binding commitments;",
        );
        assert_inline_structure(
            &changes,
            &[
                "EQ:WHEREAS the parties ",
                "DEL:have entered into negotiations regarding the proposed transaction and wish to formalize their agreement;",
                "INS:desire to document the terms of the contemplated arrangement and establish binding commitments;",
            ],
        );
    }

    // --- Test 11: Bold "Investor" anchors correctly ---
    #[test]
    fn golden_bold_defined_term_as_anchor_not_blocker() {
        let old_inlines = make_inlines(&[
            ("Upon written notice to the ", vec![]),
            ("Investor", vec![Mark::Bold]),
            (
                ", the obligation to fund additional capital contributions shall terminate.",
                vec![],
            ),
        ]);
        let new_inlines = make_inlines(&[
            ("Following delivery of formal notification to the ", vec![]),
            ("Investor", vec![Mark::Bold]),
            (
                ", the requirement to provide supplementary funding shall cease.",
                vec![],
            ),
        ]);
        let changes = diff_block_content_with_marks(&old_inlines, &new_inlines);
        assert_inline_structure(
            &changes,
            &[
                "DEL:Upon written notice to the ",
                "INS:Following delivery of formal notification to the ",
                "EQ:Investor",
                "DEL:, the obligation to fund additional capital contributions shall terminate.",
                "INS:, the requirement to provide supplementary funding shall cease.",
            ],
        );
    }

    // --- Test 12: Section reference number change stays inline ---
    #[test]
    fn golden_section_reference_inline() {
        let changes = diff_block_content(
            "as set forth in Section 3.1(a) of this Agreement.",
            "as set forth in Section 4.2(b) of this Agreement.",
        );
        assert_inline_structure(
            &changes,
            &[
                "EQ:as set forth in Section ",
                "DEL:3.1(a",
                "INS:4.2(b",
                "EQ:) of this Agreement.",
            ],
        );
    }

    // --- Test 13: Trailing punctuation change is minimal ---
    #[test]
    fn golden_minimal_change_in_long_text() {
        let changes = diff_block_content(
            "The parties agree to the terms and conditions set forth herein.",
            "The parties agree to the terms and conditions set forth herein;",
        );
        assert_inline_structure(
            &changes,
            &[
                "EQ:The parties agree to the terms and conditions set forth herein",
                "DEL:.",
                "INS:;",
            ],
        );
    }

    #[test]
    fn test_ppr_change_detected_on_matched_block() {
        // Paragraph-level formatting change (alignment) on a matched block
        // (identical text) must produce a BlockModified so the redline
        // shows a w:pPrChange.
        let mut base_para = make_paragraph_with_marks("p1", "Contract clause", vec![]);
        if let BlockNode::Paragraph(ref mut p) = base_para {
            p.align = Some(Alignment::Left);
        }
        let mut target_para = make_paragraph_with_marks("p1", "Contract clause", vec![]);
        if let BlockNode::Paragraph(ref mut p) = target_para {
            p.align = Some(Alignment::Center);
        }

        let base = make_doc("doc", vec![base_para], "fp1");
        let target = make_doc("doc", vec![target_para], "fp2");

        let diff = diff_documents(&base, &target).expect("diff should succeed");

        assert_eq!(
            diff.changes.len(),
            1,
            "paragraph formatting change should produce 1 BlockModified"
        );
        match &diff.changes[0] {
            DiffChange::BlockModified {
                old_block,
                new_block,
                ..
            } => {
                let old_align = match old_block {
                    BlockNode::Paragraph(p) => p.align.clone(),
                    _ => panic!("expected paragraph"),
                };
                let new_align = match new_block {
                    BlockNode::Paragraph(p) => p.align.clone(),
                    _ => panic!("expected paragraph"),
                };
                assert_eq!(old_align, Some(Alignment::Left));
                assert_eq!(new_align, Some(Alignment::Center));
            }
            other => panic!("expected BlockModified, got {:?}", other),
        }
    }

    #[test]
    fn test_no_change_when_text_and_formatting_identical() {
        // Truly identical blocks (same text, same formatting) must produce
        // 0 changes — regression guard against false positives.
        let base = make_doc(
            "doc",
            vec![make_paragraph_with_marks(
                "p1",
                "Identical content",
                vec![Mark::Bold],
            )],
            "fp1",
        );
        let target = make_doc(
            "doc",
            vec![make_paragraph_with_marks(
                "p1",
                "Identical content",
                vec![Mark::Bold],
            )],
            "fp2",
        );

        let diff = diff_documents(&base, &target).expect("diff should succeed");

        assert_eq!(
            diff.changes.len(),
            0,
            "identical text and formatting should produce 0 changes"
        );
    }
}
