//! Table diffing: compares canonical tables and produces aligned diff results.
//!
//! Uses patience diff on row/column signatures to align rows between tables,
//! then performs cell-level diffing within aligned rows.

use similar::{Algorithm, DiffOp};

use crate::domain::{
    BlockNode, CanonicalCell, CanonicalTable, InlineChange, InlineNode, NestedTableDiff,
    StyleProps, TableNode, TrackedSegment, TrackingStatus,
};
use crate::table::canonicalize_table;

/// Result of diffing two canonical tables.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableDiff {
    /// The canonicalized old table.
    pub old_table: CanonicalTable,
    /// The canonicalized new table.
    pub new_table: CanonicalTable,
    /// Row-level alignment between old and new tables.
    pub row_alignment: Vec<RowAlignment>,
    /// Cell-level diffs (for matched rows).
    pub cell_diffs: Vec<CellDiff>,
}

/// Alignment of a single row between old and new tables.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowAlignment {
    /// Row exists in both tables at these indices.
    Matched { old_row: usize, new_row: usize },
    /// Row was deleted from old table.
    Deleted { old_row: usize },
    /// Row was inserted in new table.
    Inserted { new_row: usize },
}

/// Diff result for a single cell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellDiff {
    /// Index in old_table.cells (None if inserted).
    pub old_cell_idx: Option<usize>,
    /// Index in new_table.cells (None if deleted).
    pub new_cell_idx: Option<usize>,
    /// Type of change.
    pub diff_type: CellDiffType,
    /// Word-level text diff (for Modified cells with paragraph content).
    pub text_diff: Option<Vec<InlineChange>>,
    /// Diffs for nested tables within this cell (for Modified cells).
    pub nested_table_diffs: Vec<NestedTableDiff>,
}

/// Type of cell change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CellDiffType {
    /// Cell content unchanged.
    Unchanged,
    /// Cell text modified.
    Modified,
    /// Cell inserted in new table.
    Inserted,
    /// Cell deleted from old table.
    Deleted,
    /// Cell merge (rowspan/colspan) changed but text may be same.
    MergeChanged,
}

/// Diff two tables and produce a structured TableDiff.
pub fn diff_tables(old_table: &TableNode, new_table: &TableNode) -> Result<TableDiff, String> {
    let old_canonical = canonicalize_table(old_table)?;
    let new_canonical = canonicalize_table(new_table)?;

    Ok(diff_canonical_tables(old_canonical, new_canonical))
}

/// Diff two already-canonicalized tables.
pub fn diff_canonical_tables(old_table: CanonicalTable, new_table: CanonicalTable) -> TableDiff {
    // Align rows using patience diff on row signatures
    let row_alignment = align_rows(&old_table, &new_table);

    // Compute cell-level diffs for matched rows
    let cell_diffs = compute_cell_diffs(&old_table, &new_table, &row_alignment);

    TableDiff {
        old_table,
        new_table,
        row_alignment,
        cell_diffs,
    }
}

/// Align rows between two tables using patience diff on signatures.
pub fn align_rows(old: &CanonicalTable, new: &CanonicalTable) -> Vec<RowAlignment> {
    // Compute row signatures
    let old_sigs: Vec<String> = (0..old.n_rows).map(|r| old.row_signature(r)).collect();
    let new_sigs: Vec<String> = (0..new.n_rows).map(|r| new.row_signature(r)).collect();

    // Use similar's diff with patience algorithm
    let old_refs: Vec<&str> = old_sigs.iter().map(|s| s.as_str()).collect();
    let new_refs: Vec<&str> = new_sigs.iter().map(|s| s.as_str()).collect();

    let diff = similar::capture_diff_slices_deadline(
        Algorithm::Patience,
        &old_refs,
        &new_refs,
        None, // No deadline
    );

    let mut alignments = Vec::new();

    for op in diff {
        match op {
            DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => {
                for i in 0..len {
                    alignments.push(RowAlignment::Matched {
                        old_row: old_index + i,
                        new_row: new_index + i,
                    });
                }
            }
            DiffOp::Delete {
                old_index, old_len, ..
            } => {
                for i in 0..old_len {
                    alignments.push(RowAlignment::Deleted {
                        old_row: old_index + i,
                    });
                }
            }
            DiffOp::Insert {
                new_index, new_len, ..
            } => {
                for i in 0..new_len {
                    alignments.push(RowAlignment::Inserted {
                        new_row: new_index + i,
                    });
                }
            }
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                // Try to match similar rows within the replace block instead
                // of blindly converting everything to Delete+Insert.
                let replace_alignments = match_replace_block(
                    &old_sigs[old_index..old_index + old_len],
                    &new_sigs[new_index..new_index + new_len],
                    old_index,
                    new_index,
                );
                alignments.extend(replace_alignments);
            }
        }
    }

    // Post-alignment quality check: reclassify matched rows with very low
    // cell-by-cell similarity as Delete + Insert. This catches cases where
    // Patience diff matched rows with identical signatures but completely
    // different actual content (common with short, repetitive row text).
    let mut validated = Vec::with_capacity(alignments.len() + 8);
    for alignment in alignments {
        match alignment {
            RowAlignment::Matched { old_row, new_row } => {
                let sim = row_cell_similarity(old, new, old_row, new_row);
                if sim < ROW_QUALITY_THRESHOLD {
                    validated.push(RowAlignment::Deleted { old_row });
                    validated.push(RowAlignment::Inserted { new_row });
                } else {
                    validated.push(alignment);
                }
            }
            other => validated.push(other),
        }
    }
    validated
}

/// Minimum similarity ratio (0.0–1.0) for two row signatures to be considered
/// a match within a Replace block. Below this threshold the rows are treated as
/// unrelated (Delete + Insert).
const SIMILARITY_THRESHOLD: f64 = 0.5;

/// Compute character-level similarity ratio between two strings.
/// Returns a value in [0.0, 1.0] where 1.0 means identical.
///
/// Uses the length of the longest common subsequence (LCS) relative to the
/// longer string. This is simple, allocation-light, and good enough for
/// comparing row signatures which are typically short.
fn text_similarity(a: &str, b: &str) -> f64 {
    if a == b {
        return 1.0;
    }
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let max_len = a_chars.len().max(b_chars.len());
    if max_len == 0 {
        return 1.0; // both empty
    }

    // LCS via classic DP (row-optimised: only two rows kept).
    let (short, long) = if a_chars.len() <= b_chars.len() {
        (&a_chars, &b_chars)
    } else {
        (&b_chars, &a_chars)
    };
    let mut prev = vec![0u32; short.len() + 1];
    let mut curr = vec![0u32; short.len() + 1];
    for &lc in long.iter() {
        for (j, &sc) in short.iter().enumerate() {
            curr[j + 1] = if lc == sc {
                prev[j] + 1
            } else {
                prev[j + 1].max(curr[j])
            };
        }
        std::mem::swap(&mut prev, &mut curr);
        curr.iter_mut().for_each(|v| *v = 0);
    }
    let lcs_len = prev[short.len()] as f64;
    lcs_len / max_len as f64
}

/// Minimum cell-by-cell similarity for a matched row pair to be kept.
/// Below this threshold the match is reclassified as Delete + Insert.
/// Deliberately low: only catches clearly wrong alignments where most cells
/// are completely unrelated.
const ROW_QUALITY_THRESHOLD: f64 = 0.3;

/// Compute cell-by-cell similarity for a matched row pair.
///
/// Walks each column and compares anchor cell text between old_row and new_row.
/// Returns the average `text_similarity` across all columns that have at least
/// one anchor cell. Columns where neither side has an anchor are skipped.
fn row_cell_similarity(
    old: &CanonicalTable,
    new: &CanonicalTable,
    old_row: usize,
    new_row: usize,
) -> f64 {
    let max_cols = old.n_cols.max(new.n_cols);
    let mut total_sim = 0.0;
    let mut count = 0usize;

    for col in 0..max_cols {
        let old_cell = if col < old.n_cols && old.is_anchor(old_row, col) {
            old.cell_at(old_row, col)
        } else {
            None
        };
        let new_cell = if col < new.n_cols && new.is_anchor(new_row, col) {
            new.cell_at(new_row, col)
        } else {
            None
        };

        match (old_cell, new_cell) {
            (Some(oc), Some(nc)) => {
                total_sim += text_similarity(&oc.text, &nc.text);
                count += 1;
            }
            (Some(_), None) | (None, Some(_)) => {
                // One side has content, the other doesn't — 0.0 similarity.
                total_sim += 0.0;
                count += 1;
            }
            (None, None) => {
                // Both empty (e.g. spanned positions) — skip.
            }
        }
    }

    if count == 0 {
        1.0 // Both rows entirely empty — treat as match.
    } else {
        total_sim / count as f64
    }
}

/// Within a Replace block, greedily match old rows to new rows by signature
/// similarity. Returns a properly-ordered sequence of alignments:
///
/// - Compute all pairwise similarities and sort by score descending.
/// - Assign pairs from highest-to-lowest score (both row and column only assigned once).
/// - This avoids the greedy-in-document-order bias where an earlier old row
///   steals a new row that would be a near-perfect match for a later old row.
/// - Unmatched old rows → `Deleted`, unmatched new rows → `Inserted`.
/// - Output order interleaves deletions, matches, and insertions so that the
///   alignment sequence stays in document order.
fn match_replace_block(
    old_sigs: &[String],
    new_sigs: &[String],
    old_base: usize,
    new_base: usize,
) -> Vec<RowAlignment> {
    let n_old = old_sigs.len();
    let n_new = new_sigs.len();

    // Score-first matching: compute all pairwise similarities, sort by score
    // descending, then assign greedily. This prevents an earlier old row with
    // moderate similarity from displacing a later old row that is a near-perfect
    // match for the same new row.
    let mut candidates: Vec<(f64, usize, usize)> = Vec::with_capacity(n_old * n_new);
    for (i, old_sig) in old_sigs.iter().enumerate() {
        for (j, new_sig) in new_sigs.iter().enumerate() {
            let score = text_similarity(old_sig, new_sig);
            if score > SIMILARITY_THRESHOLD {
                candidates.push((score, i, j));
            }
        }
    }
    // Sort descending by score; break ties by (i, j) for determinism.
    candidates.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap()
            .then(a.1.cmp(&b.1))
            .then(a.2.cmp(&b.2))
    });

    // matched_new[j] = Some(i) means new row j is matched to old row i.
    let mut matched_new: Vec<Option<usize>> = vec![None; n_new];
    // For each old row: None = unmatched (deleted), Some(j) = matched to new row j.
    let mut old_to_new: Vec<Option<usize>> = vec![None; n_old];

    for (_, i, j) in candidates {
        if old_to_new[i].is_none() && matched_new[j].is_none() {
            old_to_new[i] = Some(j);
            matched_new[j] = Some(i);
        }
    }

    // Resolve crossing matches: the greedy score-first matching can produce
    // non-monotonic j values (old[0]→new[2], old[1]→new[0]). Crossing
    // matches cause wrong row ordering after merge — the target content
    // ends up at the wrong position. Keep only the Longest Increasing
    // Subsequence (LIS) of matched j values; demote the rest to
    // Delete + Insert so row order is preserved.
    let matched_pairs: Vec<(usize, usize)> = (0..n_old)
        .filter_map(|i| old_to_new[i].map(|j| (i, j)))
        .collect();

    if matched_pairs.len() > 1 {
        let lis_indices = longest_increasing_subsequence(&matched_pairs);
        let lis_set: std::collections::HashSet<usize> = lis_indices.into_iter().collect();

        for (idx, &(i, j)) in matched_pairs.iter().enumerate() {
            if !lis_set.contains(&idx) {
                // Demote this crossing match: unlink it so the output loop
                // treats old[i] as Deleted and new[j] as Inserted.
                old_to_new[i] = None;
                matched_new[j] = None;
            }
        }
    }

    // Build output in document order. With crossings resolved, matched j
    // values are monotonically increasing and the cursor-based walk is safe.
    let mut result = Vec::with_capacity(n_old + n_new);
    let mut next_new: usize = 0;

    for (i, mapping) in old_to_new.iter().enumerate() {
        match mapping {
            Some(j_match) => {
                // Emit unmatched new rows that come before j_match.
                while next_new < *j_match {
                    if matched_new[next_new].is_none() {
                        result.push(RowAlignment::Inserted {
                            new_row: new_base + next_new,
                        });
                    }
                    next_new += 1;
                }
                result.push(RowAlignment::Matched {
                    old_row: old_base + i,
                    new_row: new_base + *j_match,
                });
                next_new = *j_match + 1;
            }
            None => {
                result.push(RowAlignment::Deleted {
                    old_row: old_base + i,
                });
            }
        }
    }

    // Emit remaining unmatched new rows.
    while next_new < n_new {
        if matched_new[next_new].is_none() {
            result.push(RowAlignment::Inserted {
                new_row: new_base + next_new,
            });
        }
        next_new += 1;
    }

    result
}

/// Compute the Longest Increasing Subsequence of matched pair j-values.
///
/// Given matched pairs `[(i0, j0), (i1, j1), ...]` sorted by `i` (old index),
/// returns the indices into the `pairs` slice that form the longest subsequence
/// where `j` values are strictly increasing. This is used to resolve crossing
/// matches — pairs not in the LIS are demoted to Delete + Insert.
fn longest_increasing_subsequence(pairs: &[(usize, usize)]) -> Vec<usize> {
    let n = pairs.len();
    if n == 0 {
        return vec![];
    }

    // tails[k] = index into pairs of the smallest j that ends an increasing
    // subsequence of length k+1.
    let mut tails: Vec<usize> = Vec::with_capacity(n);
    // prev[i] = index of the predecessor of pairs[i] in the LIS.
    let mut prev: Vec<Option<usize>> = vec![None; n];

    for idx in 0..n {
        let j = pairs[idx].1;

        // Binary search for the leftmost tail with j >= current j.
        let pos = tails.partition_point(|&tail_idx| pairs[tail_idx].1 < j);

        if pos == tails.len() {
            tails.push(idx);
        } else {
            tails[pos] = idx;
        }

        if pos > 0 {
            prev[idx] = Some(tails[pos - 1]);
        }
    }

    // Reconstruct the LIS by following prev pointers from the last tail.
    let mut result = Vec::with_capacity(tails.len());
    let mut current = *tails.last().unwrap();
    loop {
        result.push(current);
        match prev[current] {
            Some(p) => current = p,
            None => break,
        }
    }
    result.reverse();
    result
}

/// Compute cell-level diffs for aligned rows.
fn compute_cell_diffs(
    old: &CanonicalTable,
    new: &CanonicalTable,
    row_alignment: &[RowAlignment],
) -> Vec<CellDiff> {
    let mut cell_diffs = Vec::new();

    for alignment in row_alignment {
        match alignment {
            RowAlignment::Matched { old_row, new_row } => {
                // Diff cells in matched rows
                let row_diffs = diff_row_cells(old, new, *old_row, *new_row);
                cell_diffs.extend(row_diffs);
            }
            RowAlignment::Deleted { old_row } => {
                // All cells in deleted row are deleted
                for col in 0..old.n_cols {
                    if old.is_anchor(*old_row, col)
                        && let Some(cell) = old.cell_at(*old_row, col)
                    {
                        let cell_idx = find_cell_index(old, cell);
                        cell_diffs.push(CellDiff {
                            old_cell_idx: cell_idx,
                            new_cell_idx: None,
                            diff_type: CellDiffType::Deleted,
                            text_diff: None,
                            nested_table_diffs: Vec::new(),
                        });
                    }
                }
            }
            RowAlignment::Inserted { new_row } => {
                // All cells in inserted row are inserted
                for col in 0..new.n_cols {
                    if new.is_anchor(*new_row, col)
                        && let Some(cell) = new.cell_at(*new_row, col)
                    {
                        let cell_idx = find_cell_index(new, cell);
                        cell_diffs.push(CellDiff {
                            old_cell_idx: None,
                            new_cell_idx: cell_idx,
                            diff_type: CellDiffType::Inserted,
                            text_diff: None,
                            nested_table_diffs: Vec::new(),
                        });
                    }
                }
            }
        }
    }

    cell_diffs
}

/// Diff cells within matched rows.
fn diff_row_cells(
    old: &CanonicalTable,
    new: &CanonicalTable,
    old_row: usize,
    new_row: usize,
) -> Vec<CellDiff> {
    let mut diffs = Vec::new();
    let max_cols = old.n_cols.max(new.n_cols);

    for col in 0..max_cols {
        let old_cell = if col < old.n_cols {
            old.cell_at(old_row, col)
                .filter(|_| old.is_anchor(old_row, col))
        } else {
            None
        };

        let new_cell = if col < new.n_cols {
            new.cell_at(new_row, col)
                .filter(|_| new.is_anchor(new_row, col))
        } else {
            None
        };

        match (old_cell, new_cell) {
            (Some(old_c), Some(new_c)) => {
                let old_idx = find_cell_index(old, old_c);
                let new_idx = find_cell_index(new, new_c);

                // Check for merge changes
                let merge_changed =
                    old_c.rowspan != new_c.rowspan || old_c.colspan != new_c.colspan;

                // Check for text changes
                let text_changed = old_c.text != new_c.text;

                if text_changed {
                    // Compute paragraph-level text diff (only for paragraph text).
                    let para_text_old = extract_paragraph_text(&old_c.blocks);
                    let para_text_new = extract_paragraph_text(&new_c.blocks);
                    let text_diff = if para_text_old != para_text_new {
                        Some(diff_cell_text(&para_text_old, &para_text_new))
                    } else {
                        None
                    };

                    // Compute nested table diffs.
                    let nested_table_diffs = diff_cell_nested_tables(&old_c.blocks, &new_c.blocks);

                    diffs.push(CellDiff {
                        old_cell_idx: old_idx,
                        new_cell_idx: new_idx,
                        diff_type: CellDiffType::Modified,
                        text_diff,
                        nested_table_diffs,
                    });
                } else if merge_changed {
                    diffs.push(CellDiff {
                        old_cell_idx: old_idx,
                        new_cell_idx: new_idx,
                        diff_type: CellDiffType::MergeChanged,
                        text_diff: None,
                        nested_table_diffs: Vec::new(),
                    });
                } else {
                    diffs.push(CellDiff {
                        old_cell_idx: old_idx,
                        new_cell_idx: new_idx,
                        diff_type: CellDiffType::Unchanged,
                        text_diff: None,
                        nested_table_diffs: Vec::new(),
                    });
                }
            }
            (Some(old_c), None) => {
                let old_idx = find_cell_index(old, old_c);
                diffs.push(CellDiff {
                    old_cell_idx: old_idx,
                    new_cell_idx: None,
                    diff_type: CellDiffType::Deleted,
                    text_diff: None,
                    nested_table_diffs: Vec::new(),
                });
            }
            (None, Some(new_c)) => {
                let new_idx = find_cell_index(new, new_c);
                diffs.push(CellDiff {
                    old_cell_idx: None,
                    new_cell_idx: new_idx,
                    diff_type: CellDiffType::Inserted,
                    text_diff: None,
                    nested_table_diffs: Vec::new(),
                });
            }
            (None, None) => {
                // Both empty, skip
            }
        }
    }

    diffs
}

/// Extract text only from paragraph blocks (not nested tables).
fn extract_paragraph_text(blocks: &[BlockNode]) -> String {
    use crate::table::extract_cell_text;
    let para_blocks: Vec<_> = blocks
        .iter()
        .filter(|b| matches!(b, BlockNode::Paragraph(_)))
        .cloned()
        .collect();
    extract_cell_text(&para_blocks)
}

/// Diff nested tables within a cell's blocks.
///
/// Walks block pairs positionally and produces `NestedTableDiff` entries
/// for any `BlockNode::Table` pairs that differ.
fn diff_cell_nested_tables(
    old_blocks: &[BlockNode],
    new_blocks: &[BlockNode],
) -> Vec<NestedTableDiff> {
    use crate::diff::diff_nested_tables;
    old_blocks
        .iter()
        .zip(new_blocks.iter())
        .enumerate()
        .filter_map(|(idx, (old_b, new_b))| {
            if let (BlockNode::Table(old_t), BlockNode::Table(new_t)) = (old_b, new_b) {
                match diff_nested_tables(old_t, new_t, idx) {
                    Ok(Some(diff)) => Some(diff),
                    Ok(None) => None,
                    Err(e) => {
                        tracing::warn!("failed to diff nested table at block index {idx}: {e}");
                        None
                    }
                }
            } else {
                None
            }
        })
        .collect()
}

/// Find the index of a cell in the table's cells vector.
fn find_cell_index(table: &CanonicalTable, cell: &CanonicalCell) -> Option<usize> {
    table.cells.iter().position(|c| c.id == cell.id)
}

/// Diff cell text at token level.
fn diff_cell_text(old_text: &str, new_text: &str) -> Vec<InlineChange> {
    use crate::diff::{cleanup_inline_changes, tokenize};
    use similar::{Algorithm, ChangeTag, TextDiff};

    let old_tokens = tokenize(old_text);
    let new_tokens = tokenize(new_text);
    let diff = TextDiff::configure()
        .algorithm(Algorithm::Patience)
        .diff_slices(&old_tokens, &new_tokens);
    let mut changes = Vec::new();

    for change in diff.iter_all_changes() {
        let text = change.to_string_lossy().into_owned();
        if text.is_empty() {
            continue;
        }

        match change.tag() {
            ChangeTag::Equal => {
                changes.push(InlineChange::Unchanged {
                    text,
                    marks: Vec::new(),
                    style_props: StyleProps::default(),
                    formatting_change: None,
                });
            }
            ChangeTag::Delete => {
                changes.push(InlineChange::Deleted {
                    text,
                    marks: Vec::new(),
                    style_props: StyleProps::default(),
                    formatting_change: None,
                    rev_id: 0,
                });
            }
            ChangeTag::Insert => {
                changes.push(InlineChange::Inserted {
                    text,
                    marks: Vec::new(),
                    style_props: StyleProps::default(),
                    formatting_change: None,
                    rev_id: 0,
                });
            }
        }
    }

    cleanup_inline_changes(changes)
}

// ---------------------------------------------------------------------------
// Tracked-table text extraction
//
// Reads the "before" or "after" text view of a table that carries tracked
// segments and row/cell tracking statuses. Used by the diff pipeline to
// decide whether a tracked table needs a diff entry at all, and to align
// its tracked content with adjacent atoms.
// ---------------------------------------------------------------------------

fn tracked_text_from_inlines(inlines: &[InlineNode]) -> String {
    let mut out = String::new();
    for inline in inlines {
        match inline {
            InlineNode::Text(t) => out.push_str(&t.text),
            InlineNode::HardBreak(_) => out.push('\n'),
            _ => {}
        }
    }
    out
}

fn tracked_text_from_segments_filtered(
    segments: &[TrackedSegment],
    filter: impl Fn(&TrackingStatus) -> bool,
) -> String {
    let mut out = String::new();
    for seg in segments {
        if filter(&seg.status) {
            out.push_str(&tracked_text_from_inlines(&seg.inlines));
        }
    }
    out
}

/// Extract "old" text from a table's tracked segments (Normal + Deleted, skip Inserted).
/// Matches the separator conventions of `diff::extract_table_text`.
pub fn extract_tracked_table_old_text(table: &TableNode) -> String {
    let mut out = String::new();
    for row in &table.rows {
        // Inserted rows are not in the base; neither are stacked
        // (inserted-then-deleted) rows — their content never existed there.
        if matches!(
            row.tracking_status,
            Some(TrackingStatus::Inserted(_)) | Some(TrackingStatus::InsertedThenDeleted(_))
        ) {
            continue;
        }
        for cell in &row.cells {
            for block in &cell.blocks {
                if let BlockNode::Paragraph(p) = block {
                    let text = tracked_text_from_segments_filtered(&p.segments, |status| {
                        !matches!(
                            status,
                            TrackingStatus::Inserted(_) | TrackingStatus::InsertedThenDeleted(_)
                        )
                    });
                    if !text.trim().is_empty() {
                        if !out.is_empty() {
                            out.push(' ');
                        }
                        out.push_str(&text);
                    }
                }
            }
        }
    }
    out
}

/// Extract "new" text from a table's tracked segments (Normal + Inserted, skip Deleted).
/// Matches the separator conventions of `diff::extract_table_text`.
pub fn extract_tracked_table_new_text(table: &TableNode) -> String {
    let mut out = String::new();
    for row in &table.rows {
        // Deleted rows leave the accepted reading; so do stacked rows
        // (accepting the deletion settles the insertion's claim).
        if matches!(
            row.tracking_status,
            Some(TrackingStatus::Deleted(_)) | Some(TrackingStatus::InsertedThenDeleted(_))
        ) {
            continue;
        }
        for cell in &row.cells {
            for block in &cell.blocks {
                if let BlockNode::Paragraph(p) = block {
                    let text = tracked_text_from_segments_filtered(&p.segments, |status| {
                        !matches!(
                            status,
                            TrackingStatus::Deleted(_) | TrackingStatus::InsertedThenDeleted(_)
                        )
                    });
                    if !text.trim().is_empty() {
                        if !out.is_empty() {
                            out.push(' ');
                        }
                        out.push_str(&text);
                    }
                }
            }
        }
    }
    out
}

/// Check whether a table has any tracked changes (any non-Normal segments or
/// non-Normal row/cell tracking status).
pub fn table_has_tracked_changes(table: &TableNode) -> bool {
    for row in &table.rows {
        if row.tracking_status.is_some() {
            return true;
        }
        for cell in &row.cells {
            if cell.tracking_status.is_some() {
                return true;
            }
            for block in &cell.blocks {
                if let BlockNode::Paragraph(p) = block {
                    for seg in &p.segments {
                        if !matches!(seg.status, TrackingStatus::Normal) {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        BlockNode, CellFormatting, InlineNode, NodeId, ParagraphNode, StyleProps, TableCellNode,
        TableFormatting, TableRowNode, TextNode, VerticalMerge, normal_segment,
    };

    fn make_text_cell(id: &str, text: &str) -> TableCellNode {
        TableCellNode {
            id: NodeId::from(id.to_string()),
            blocks: vec![BlockNode::from(ParagraphNode {
                id: NodeId::from(format!("{}_p", id)),
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
                    id: NodeId::from(format!("{}_t", id)),
                    text_role: None,
                    text: text.to_string(),
                    marks: vec![],
                    style_props: StyleProps::default(),
                    rpr_authored: crate::domain::RunRprAuthored::default(),
                    source_run_attrs: Vec::new(),
                    formatting_change: None,
                })]),
                block_text_hash: None,
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
            })],
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
        }
    }

    fn make_table(rows: Vec<Vec<&str>>) -> TableNode {
        TableNode {
            id: NodeId::from("tbl_0"),
            rows: rows
                .into_iter()
                .enumerate()
                .map(|(r, cells)| TableRowNode {
                    id: NodeId::from(format!("tbl_0_r{}", r)),
                    cells: cells
                        .into_iter()
                        .enumerate()
                        .map(|(c, text)| make_text_cell(&format!("c{}_{}", r, c), text))
                        .collect(),
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
                })
                .collect(),
            structure_hash: String::new(),
            formatting: TableFormatting::default(),
            formatting_change: None,
        }
    }

    #[test]
    fn test_identical_tables() {
        let old = make_table(vec![vec!["A", "B"], vec!["C", "D"]]);
        let new = make_table(vec![vec!["A", "B"], vec!["C", "D"]]);

        let diff = diff_tables(&old, &new).expect("diff should succeed");

        // All rows should be matched
        assert_eq!(diff.row_alignment.len(), 2);
        for alignment in &diff.row_alignment {
            assert!(matches!(alignment, RowAlignment::Matched { .. }));
        }

        // All cells should be unchanged
        for cell_diff in &diff.cell_diffs {
            assert_eq!(cell_diff.diff_type, CellDiffType::Unchanged);
        }
    }

    #[test]
    fn test_row_insertion() {
        let old = make_table(vec![vec!["A", "B"], vec!["C", "D"]]);
        let new = make_table(vec![
            vec!["A", "B"],
            vec!["X", "Y"], // Inserted
            vec!["C", "D"],
        ]);

        let diff = diff_tables(&old, &new).expect("diff should succeed");

        // Should have: Matched(0,0), Inserted(1), Matched(1,2)
        let matched_count = diff
            .row_alignment
            .iter()
            .filter(|a| matches!(a, RowAlignment::Matched { .. }))
            .count();
        let inserted_count = diff
            .row_alignment
            .iter()
            .filter(|a| matches!(a, RowAlignment::Inserted { .. }))
            .count();

        assert_eq!(matched_count, 2);
        assert_eq!(inserted_count, 1);
    }

    #[test]
    fn test_row_deletion() {
        let old = make_table(vec![
            vec!["A", "B"],
            vec!["X", "Y"], // Will be deleted
            vec!["C", "D"],
        ]);
        let new = make_table(vec![vec!["A", "B"], vec!["C", "D"]]);

        let diff = diff_tables(&old, &new).expect("diff should succeed");

        // Should have: Matched(0,0), Deleted(1), Matched(2,1)
        let matched_count = diff
            .row_alignment
            .iter()
            .filter(|a| matches!(a, RowAlignment::Matched { .. }))
            .count();
        let deleted_count = diff
            .row_alignment
            .iter()
            .filter(|a| matches!(a, RowAlignment::Deleted { .. }))
            .count();

        assert_eq!(matched_count, 2);
        assert_eq!(deleted_count, 1);
    }

    #[test]
    fn test_cell_text_change() {
        // When a single cell changes, the row signature changes and patience
        // diff produces a Replace. The similarity-based matching in
        // match_replace_block should recognise the rows as similar and emit
        // Matched, allowing cell-level inline diffing.
        let old = make_table(vec![
            vec!["Header1", "Header2", "Header3"],
            vec!["Data A", "Data B", "Data C"],
        ]);
        let new = make_table(vec![
            vec!["Header1", "Header2", "Header3"],
            vec!["Data A", "Modified B", "Data C"],
        ]);

        let diff = diff_tables(&old, &new).expect("diff should succeed");

        // Both rows should be matched (row 0 identical, row 1 similar enough).
        assert_eq!(diff.row_alignment.len(), 2);
        assert!(
            matches!(
                diff.row_alignment[0],
                RowAlignment::Matched {
                    old_row: 0,
                    new_row: 0
                }
            ),
            "First row (headers) should be matched"
        );
        assert!(
            matches!(
                diff.row_alignment[1],
                RowAlignment::Matched {
                    old_row: 1,
                    new_row: 1
                }
            ),
            "Second row should be matched via similarity"
        );

        // The cell diff for the modified cell should be Modified with inline
        // text diff, not a pair of Deleted+Inserted.
        let modified_cells: Vec<_> = diff
            .cell_diffs
            .iter()
            .filter(|c| c.diff_type == CellDiffType::Modified)
            .collect();
        assert_eq!(
            modified_cells.len(),
            1,
            "Exactly one cell should be Modified"
        );
        assert!(
            modified_cells[0].text_diff.is_some(),
            "Modified cell should have inline text diff"
        );
    }

    #[test]
    fn test_word_level_diff() {
        let changes = diff_cell_text("hello world", "hello universe");

        // Should have: "hello " unchanged, "world" deleted, "universe" inserted
        let has_unchanged = changes
            .iter()
            .any(|c| matches!(c, InlineChange::Unchanged { text, .. } if text.contains("hello")));
        let has_deleted = changes
            .iter()
            .any(|c| matches!(c, InlineChange::Deleted { text, .. } if text.contains("world")));
        let has_inserted = changes
            .iter()
            .any(|c| matches!(c, InlineChange::Inserted { text, .. } if text.contains("universe")));

        assert!(has_unchanged, "Should have unchanged 'hello'");
        assert!(has_deleted, "Should have deleted 'world'");
        assert!(has_inserted, "Should have inserted 'universe'");
    }

    // ---- text_similarity tests ----

    #[test]
    fn test_similarity_identical() {
        assert_eq!(text_similarity("abc", "abc"), 1.0);
    }

    #[test]
    fn test_similarity_completely_different() {
        let s = text_similarity("abc", "xyz");
        assert!(
            s < SIMILARITY_THRESHOLD,
            "completely different strings should be below threshold, got {s}"
        );
    }

    #[test]
    fn test_similarity_empty() {
        assert_eq!(text_similarity("", ""), 1.0);
        assert_eq!(text_similarity("a", ""), 0.0);
        assert_eq!(text_similarity("", "b"), 0.0);
    }

    #[test]
    fn test_similarity_partial_overlap() {
        // "Data A|Data B|Data C" vs "Data A|Modified B|Data C"
        // Significant overlap so should be well above 0.5
        let a = "Data A|Data B|Data C";
        let b = "Data A|Modified B|Data C";
        let s = text_similarity(a, b);
        assert!(
            s > SIMILARITY_THRESHOLD,
            "partial overlap should be above threshold, got {s}"
        );
    }

    // ---- match_replace_block tests ----

    #[test]
    fn test_replace_1_to_1_similar() {
        // Single old row similar to single new row → Matched
        let old_sigs = vec!["Data A|Data B|Data C".to_string()];
        let new_sigs = vec!["Data A|Modified B|Data C".to_string()];
        let result = match_replace_block(&old_sigs, &new_sigs, 0, 0);
        assert_eq!(result.len(), 1);
        assert!(matches!(
            result[0],
            RowAlignment::Matched {
                old_row: 0,
                new_row: 0
            }
        ));
    }

    #[test]
    fn test_replace_1_to_1_dissimilar() {
        // Completely different → Delete + Insert
        let old_sigs = vec!["aaaa|bbbb|cccc".to_string()];
        let new_sigs = vec!["xxxx|yyyy|zzzz".to_string()];
        let result = match_replace_block(&old_sigs, &new_sigs, 0, 0);
        assert_eq!(result.len(), 2);
        assert!(matches!(result[0], RowAlignment::Deleted { old_row: 0 }));
        assert!(matches!(result[1], RowAlignment::Inserted { new_row: 0 }));
    }

    #[test]
    fn test_replace_2_to_1_one_similar() {
        // Two old rows, one new row. Old row 1 is similar to the new row.
        let old_sigs = vec![
            "completely different row".to_string(),
            "Data A|Data B|Data C".to_string(),
        ];
        let new_sigs = vec!["Data A|Modified B|Data C".to_string()];
        let result = match_replace_block(&old_sigs, &new_sigs, 5, 3);

        // old row 0 (index 5) → Deleted
        // old row 1 (index 6) matched to new row 0 (index 3)
        assert_eq!(result.len(), 2);
        assert!(matches!(result[0], RowAlignment::Deleted { old_row: 5 }));
        assert!(matches!(
            result[1],
            RowAlignment::Matched {
                old_row: 6,
                new_row: 3
            }
        ));
    }

    #[test]
    fn test_replace_n_to_n_all_similar() {
        // All rows are similar positionally → all Matched
        let old_sigs = vec!["Row1 A|Row1 B".to_string(), "Row2 C|Row2 D".to_string()];
        let new_sigs = vec![
            "Row1 A|Row1 Modified".to_string(),
            "Row2 C|Row2 Modified".to_string(),
        ];
        let result = match_replace_block(&old_sigs, &new_sigs, 0, 0);
        assert_eq!(result.len(), 2);
        assert!(matches!(
            result[0],
            RowAlignment::Matched {
                old_row: 0,
                new_row: 0
            }
        ));
        assert!(matches!(
            result[1],
            RowAlignment::Matched {
                old_row: 1,
                new_row: 1
            }
        ));
    }

    #[test]
    fn test_replace_interleaved_order() {
        // 1 old, 2 new. Old matches new[1]. new[0] should be Inserted before
        // the match. Uses pipe-separated cell text like real row signatures.
        let old_sigs = vec!["Item A|Price 100|In Stock".to_string()];
        let new_sigs = vec![
            "Brand New|Different|Row".to_string(),
            "Item A|Price 200|In Stock".to_string(),
        ];
        let result = match_replace_block(&old_sigs, &new_sigs, 0, 0);
        assert_eq!(result.len(), 2);
        // Unmatched new[0] comes first (Inserted), then the match.
        assert!(matches!(result[0], RowAlignment::Inserted { new_row: 0 }));
        assert!(matches!(
            result[1],
            RowAlignment::Matched {
                old_row: 0,
                new_row: 1
            }
        ));
    }

    /// Simultaneous insert + delete: one row inserted, one row deleted.
    /// Row count stays the same but content shifts. Patience diff should
    /// detect the shift and produce Inserted + Deleted alignments, not
    /// treat all rows as Matched with modified content.
    #[test]
    fn test_simultaneous_insert_and_delete() {
        // Before: rows "111", "222", "333", "444", "555", "666"
        // After:  rows "111", "1a",  "222", "333", "444", "555"
        // "1a" inserted, "666" deleted
        let old = make_table(vec![
            vec!["111"],
            vec!["222"],
            vec!["333"],
            vec!["444"],
            vec!["555"],
            vec!["666"],
        ]);
        let new = make_table(vec![
            vec!["111"],
            vec!["1a"],
            vec!["222"],
            vec!["333"],
            vec!["444"],
            vec!["555"],
        ]);

        let diff = diff_tables(&old, &new).expect("diff should succeed");

        // Count alignment types
        let matched = diff
            .row_alignment
            .iter()
            .filter(|a| matches!(a, RowAlignment::Matched { .. }))
            .count();
        let inserted = diff
            .row_alignment
            .iter()
            .filter(|a| matches!(a, RowAlignment::Inserted { .. }))
            .count();
        let deleted = diff
            .row_alignment
            .iter()
            .filter(|a| matches!(a, RowAlignment::Deleted { .. }))
            .count();

        assert_eq!(
            inserted, 1,
            "should have 1 inserted row ('1a'), got {inserted}"
        );
        assert_eq!(
            deleted, 1,
            "should have 1 deleted row ('666'), got {deleted}"
        );
        assert_eq!(matched, 5, "should have 5 matched rows, got {matched}");
    }

    /// Helper to create a cell that contains a nested table (plus a paragraph).
    fn make_nested_table_cell(
        id: &str,
        para_text: &str,
        inner_rows: Vec<Vec<&str>>,
    ) -> TableCellNode {
        let inner_table = make_table(inner_rows);
        // Adjust inner table ID to be unique based on cell
        let inner_table = TableNode {
            id: NodeId::from(format!("{}_inner_tbl", id)),
            ..inner_table
        };
        let para = BlockNode::from(ParagraphNode {
            id: NodeId::from(format!("{}_p", id)),
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
                id: NodeId::from(format!("{}_t", id)),
                text_role: None,
                text: para_text.to_string(),
                marks: vec![],
                style_props: StyleProps::default(),
                rpr_authored: crate::domain::RunRprAuthored::default(),
                source_run_attrs: Vec::new(),
                formatting_change: None,
            })]),
            block_text_hash: None,
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
        });

        TableCellNode {
            id: NodeId::from(id.to_string()),
            blocks: vec![para, BlockNode::from(inner_table)],
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
        }
    }

    #[test]
    fn test_nested_table_text_change_detected() {
        // Outer table has one row with one cell containing a nested table.
        // The nested table's inner cell text changes.
        let old = TableNode {
            id: NodeId::from("outer"),
            rows: vec![TableRowNode {
                id: NodeId::from("outer_r0"),
                cells: vec![make_nested_table_cell(
                    "outer_c0",
                    "Header",
                    vec![vec!["Inner A", "Inner B"]],
                )],
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
        };

        let new = TableNode {
            id: NodeId::from("outer"),
            rows: vec![TableRowNode {
                id: NodeId::from("outer_r0"),
                cells: vec![make_nested_table_cell(
                    "outer_c0",
                    "Header",
                    vec![vec!["Inner A", "Modified B"]],
                )],
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
        };

        let diff = diff_tables(&old, &new).expect("diff should succeed");

        // The outer row should be matched (same structure).
        assert_eq!(diff.row_alignment.len(), 1);
        assert!(matches!(
            diff.row_alignment[0],
            RowAlignment::Matched {
                old_row: 0,
                new_row: 0
            }
        ));

        // The cell should be Modified because inner table text changed.
        let modified: Vec<_> = diff
            .cell_diffs
            .iter()
            .filter(|c| c.diff_type == CellDiffType::Modified)
            .collect();
        assert_eq!(modified.len(), 1, "should have 1 modified cell");

        // The modified cell should have nested_table_diffs (not just text_diff).
        let cell_diff = modified[0];
        assert!(
            !cell_diff.nested_table_diffs.is_empty(),
            "modified cell should have nested table diffs"
        );

        // The paragraph text didn't change, so text_diff should be None.
        assert!(
            cell_diff.text_diff.is_none(),
            "paragraph text didn't change so text_diff should be None"
        );
    }

    #[test]
    fn test_nested_table_both_para_and_table_change() {
        // Both the paragraph text and nested table content change.
        let old = TableNode {
            id: NodeId::from("outer"),
            rows: vec![TableRowNode {
                id: NodeId::from("outer_r0"),
                cells: vec![make_nested_table_cell(
                    "outer_c0",
                    "Old Header",
                    vec![vec!["Inner A"]],
                )],
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
        };

        let new = TableNode {
            id: NodeId::from("outer"),
            rows: vec![TableRowNode {
                id: NodeId::from("outer_r0"),
                cells: vec![make_nested_table_cell(
                    "outer_c0",
                    "New Header",
                    vec![vec!["Inner Z"]],
                )],
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
        };

        let diff = diff_tables(&old, &new).expect("diff should succeed");

        let modified: Vec<_> = diff
            .cell_diffs
            .iter()
            .filter(|c| c.diff_type == CellDiffType::Modified)
            .collect();
        assert_eq!(modified.len(), 1);

        let cell_diff = modified[0];
        // Both text_diff and nested_table_diffs should be present.
        assert!(
            cell_diff.text_diff.is_some(),
            "paragraph text changed so text_diff should be Some"
        );
        assert!(
            !cell_diff.nested_table_diffs.is_empty(),
            "nested table content changed so nested_table_diffs should be non-empty"
        );
    }

    #[test]
    fn test_nested_table_unchanged() {
        // Nested table content is the same — no diffs should be generated.
        let old = TableNode {
            id: NodeId::from("outer"),
            rows: vec![TableRowNode {
                id: NodeId::from("outer_r0"),
                cells: vec![make_nested_table_cell(
                    "outer_c0",
                    "Header",
                    vec![vec!["Inner A", "Inner B"]],
                )],
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
        };

        // Same content
        let new = old.clone();

        let diff = diff_tables(&old, &new).expect("diff should succeed");

        // All cells should be unchanged.
        for cell_diff in &diff.cell_diffs {
            assert_eq!(
                cell_diff.diff_type,
                CellDiffType::Unchanged,
                "cells should be unchanged when nested table is identical"
            );
            assert!(cell_diff.nested_table_diffs.is_empty());
        }
    }

    // ---- row_cell_similarity tests ----

    #[test]
    fn test_row_cell_similarity_identical() {
        let table = make_table(vec![vec!["A", "B", "C"]]);
        let canonical = canonicalize_table(&table).expect("canonicalize");
        let sim = row_cell_similarity(&canonical, &canonical, 0, 0);
        assert_eq!(sim, 1.0, "identical rows should have similarity 1.0");
    }

    #[test]
    fn test_row_cell_similarity_completely_different() {
        let old = make_table(vec![vec!["Alpha", "Beta", "Gamma"]]);
        let new = make_table(vec![vec!["1", "2", "3"]]);
        let old_c = canonicalize_table(&old).expect("canonicalize old");
        let new_c = canonicalize_table(&new).expect("canonicalize new");
        let sim = row_cell_similarity(&old_c, &new_c, 0, 0);
        assert!(
            sim < ROW_QUALITY_THRESHOLD,
            "completely different rows should be below quality threshold, got {sim}"
        );
    }

    #[test]
    fn test_row_cell_similarity_partial() {
        let old = make_table(vec![vec!["Same", "Different A", "Same"]]);
        let new = make_table(vec![vec!["Same", "Different B", "Same"]]);
        let old_c = canonicalize_table(&old).expect("canonicalize old");
        let new_c = canonicalize_table(&new).expect("canonicalize new");
        let sim = row_cell_similarity(&old_c, &new_c, 0, 0);
        // Two of three cells identical (1.0), one partially similar.
        // Average should be well above the quality threshold.
        assert!(
            sim > ROW_QUALITY_THRESHOLD,
            "partially matching rows should be above quality threshold, got {sim}"
        );
    }

    #[test]
    fn test_quality_check_reclassifies_bad_match() {
        // Create two tables where rows have identical signatures (short text)
        // but the actual row content is different. Patience diff will match
        // them as Equal. The quality check should reclassify them.
        //
        // Old: row 0 = ["Header"], row 1 = ["i.", "Alpha content here"]
        // New: row 0 = ["Header"], row 1 = ["i.", "Completely other text"]
        //
        // Row 1 signatures are identical ("i. | Alpha content here" vs
        // "i. | Completely other text"), but the cell text differs significantly
        // enough that the signature-level match is misleading.
        //
        // However, to truly trigger the quality check, we need rows that
        // Patience matches as Equal (identical signatures) but have different
        // actual content. We simulate this with tables where row signatures
        // collide.
        let old = make_table(vec![
            vec!["i.", ""],
            vec!["ii.", "Content A that is very long and distinctive"],
        ]);
        let new = make_table(vec![
            vec!["i.", "Completely new and unrelated text in this cell"],
            vec!["ii.", "Content A that is very long and distinctive"],
        ]);

        let old_c = canonicalize_table(&old).expect("canonicalize old");
        let new_c = canonicalize_table(&new).expect("canonicalize new");

        let alignments = align_rows(&old_c, &new_c);

        // Row with "ii." text should be matched to the corresponding row.
        // Row 0 old ("i." + "") vs Row 0 new ("i." + "Completely new...")
        // may or may not be matched depending on Patience's choice.
        // The important thing is: no matched pair should have very low similarity.
        for alignment in &alignments {
            if let RowAlignment::Matched { old_row, new_row } = alignment {
                let sim = row_cell_similarity(&old_c, &new_c, *old_row, *new_row);
                assert!(
                    sim >= ROW_QUALITY_THRESHOLD,
                    "matched row pair (old={old_row}, new={new_row}) has similarity {sim} \
                     below quality threshold {ROW_QUALITY_THRESHOLD}"
                );
            }
        }
    }

    /// Crossing matches: greedy score-first matching can produce
    /// old_to_new mappings where j values are non-monotonic. The output
    /// loop must handle this without emitting duplicate Inserted rows.
    #[test]
    fn test_replace_crossing_matches_no_duplicate_inserts() {
        // Construct a Replace block where score-first matching produces
        // crossings: old[0] best-matches new[2], old[1] best-matches new[0].
        // Old sigs chosen so old[0] is very similar to new[2] and old[1]
        // is very similar to new[0], but the document order is crossed.
        let old_sigs = vec![
            "AAAA BBBB CCCC".to_string(), // best match → new[2]
            "XXXX YYYY ZZZZ".to_string(), // best match → new[0]
        ];
        let new_sigs = vec![
            "XXXX YYYY ZZZZ".to_string(),       // new[0] — matches old[1]
            "completely different".to_string(), // new[1] — unmatched
            "AAAA BBBB CCCC".to_string(),       // new[2] — matches old[0]
        ];

        let result = match_replace_block(&old_sigs, &new_sigs, 10, 20);

        // Verify invariant: each new row index appears at most once.
        let mut seen_new: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for alignment in &result {
            let new_idx = match alignment {
                RowAlignment::Inserted { new_row } => Some(*new_row),
                RowAlignment::Matched { new_row, .. } => Some(*new_row),
                RowAlignment::Deleted { .. } => None,
            };
            if let Some(idx) = new_idx {
                assert!(
                    seen_new.insert(idx),
                    "new row {idx} appears more than once in alignment: {result:?}"
                );
            }
        }

        // Also verify: each old row index appears exactly once.
        let mut seen_old: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for alignment in &result {
            let old_idx = match alignment {
                RowAlignment::Deleted { old_row } => Some(*old_row),
                RowAlignment::Matched { old_row, .. } => Some(*old_row),
                RowAlignment::Inserted { .. } => None,
            };
            if let Some(idx) = old_idx {
                assert!(
                    seen_old.insert(idx),
                    "old row {idx} appears more than once in alignment: {result:?}"
                );
            }
        }

        // Crossing matches are resolved by LIS: one of the two matches is
        // demoted to Delete+Insert. new[1] (originally unmatched) plus the
        // demoted match = 2 Inserted entries.
        let inserted_count = result
            .iter()
            .filter(|a| matches!(a, RowAlignment::Inserted { .. }))
            .count();
        assert_eq!(
            inserted_count, 2,
            "expected 2 Inserted (1 unmatched + 1 demoted crossing), got {inserted_count}: {result:?}"
        );

        // The surviving match should have monotonic new indices relative to
        // any prior match — i.e., there are no crossings.
        let matched_js: Vec<usize> = result
            .iter()
            .filter_map(|a| match a {
                RowAlignment::Matched { new_row, .. } => Some(*new_row),
                _ => None,
            })
            .collect();
        for w in matched_js.windows(2) {
            assert!(
                w[0] < w[1],
                "matched new_row indices not monotonically increasing: {matched_js:?}"
            );
        }
    }
}
