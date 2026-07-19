use std::collections::{HashMap, HashSet};

use sha2::{Digest, Sha256};
use similar::{Algorithm, DiffOp};

use crate::diff::{diff_block_content_with_marks, diff_nested_tables};
use crate::domain::{
    BlockNode, BlockProvenance, CanonDoc, CellFormatting, CellFormattingChange, CommentStory,
    DiffChange, EndnoteStory, FieldData, FieldKind, FooterStory, FootnoteStory, HeaderStory,
    InlineChange, InlineChangeSegmentType, InlineNode, Mark, MaterializedPrefixKind,
    NestedTableDiffKind, NodeId, OpaqueKind, ParagraphFormattingChange, ParagraphNode,
    RevisionInfo, RunRprAuthored, SectionProperties, SectionPropertyChange, StackedRevision,
    StoryScope, StyleProps, TableCellNode, TableDiffResult, TableNode, TableRowAlignment, TextNode,
    TextRole, TrackedBlock, TrackedSegment, TrackingStatus, is_materialized_prefix_text,
    materialized_prefix_node_id,
};
use crate::import::strip_literal_prefix;
use crate::table::extract_inlines_text;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeError {
    pub message: String,
    pub context: String,
}

/// Result of merging two documents with tracked changes.
///
/// Contains the merged document (with TrackedSegments) and a provenance map
/// linking each changed merged block back to its source-document identities.
pub struct MergeResult {
    pub doc: CanonDoc,
    /// Provenance for every merged block derived from a DiffChange.
    /// Keyed by merged block ID. Unchanged blocks are absent.
    pub block_provenance: BlockProvenanceMap,
}

/// Maps merged block IDs to their source-document provenance.
///
/// Typed wrapper enforcing invariants: modified blocks have both IDs,
/// deleted blocks have base only, inserted blocks have target only.
pub struct BlockProvenanceMap(HashMap<NodeId, BlockProvenance>);

impl Default for BlockProvenanceMap {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockProvenanceMap {
    pub fn new() -> Self {
        Self(HashMap::new())
    }

    /// Record provenance for a modified block (exists in both base and target).
    pub fn insert_modified(&mut self, merged_id: NodeId, base_id: NodeId, target_id: NodeId) {
        self.0.insert(
            merged_id,
            BlockProvenance {
                base_block_id: Some(base_id),
                target_block_id: Some(target_id),
            },
        );
    }

    /// Record provenance for a deleted block (exists in base only).
    pub fn insert_deleted(&mut self, merged_id: NodeId, base_id: NodeId) {
        self.0.insert(
            merged_id,
            BlockProvenance {
                base_block_id: Some(base_id),
                target_block_id: None,
            },
        );
    }

    /// Record provenance for an inserted block (exists in target only).
    pub fn insert_inserted(&mut self, merged_id: NodeId, target_id: NodeId) {
        self.0.insert(
            merged_id,
            BlockProvenance {
                base_block_id: None,
                target_block_id: Some(target_id),
            },
        );
    }

    /// Get the base (original) document block ID for anchoring delete atoms.
    pub fn base_block_id(&self, merged_id: &NodeId) -> Option<&NodeId> {
        self.0.get(merged_id).and_then(|p| p.base_block_id.as_ref())
    }

    /// Get the target (modified) document block ID for anchoring insert atoms.
    pub fn target_block_id(&self, merged_id: &NodeId) -> Option<&NodeId> {
        self.0
            .get(merged_id)
            .and_then(|p| p.target_block_id.as_ref())
    }

    /// Check whether a merged block has provenance recorded.
    pub fn contains(&self, merged_id: &NodeId) -> bool {
        self.0.contains_key(merged_id)
    }

    /// Build an identity provenance map from a tracked CanonDoc.
    ///
    /// For the single-doc pipeline, there is no base/target distinction — every
    /// block maps to itself.  Inserted blocks get target-only provenance,
    /// deleted blocks get base-only, and Normal blocks (which may contain inline
    /// tracked changes) get both.  This lets `extract_tracked_atoms_from_merged`
    /// work unchanged for the single-doc case.
    pub fn identity_from_tracked_doc(doc: &CanonDoc) -> Self {
        let mut map = Self::new();
        for tb in &doc.blocks {
            let id = match &tb.block {
                BlockNode::Paragraph(p) => &p.id,
                BlockNode::Table(t) => &t.id,
                BlockNode::OpaqueBlock(o) => &o.id,
            };
            match &tb.status {
                TrackingStatus::Inserted(_) => {
                    map.insert_inserted(id.clone(), id.clone());
                }
                TrackingStatus::Deleted(_) => {
                    map.insert_deleted(id.clone(), id.clone());
                }
                TrackingStatus::InsertedThenDeleted(_) => unreachable!(
                    "block-level stacked status is never constructed (3a is inline-only; \
                     nested body containers quarantine at import)"
                ),
                TrackingStatus::Normal => {
                    map.insert_modified(id.clone(), id.clone(), id.clone());
                }
            }
        }
        map
    }
}

#[derive(Default)]
struct InsertOrderState {
    start_tail: Option<NodeId>,
    by_anchor: HashMap<NodeId, NodeId>,
}

fn block_id(block: &BlockNode) -> &NodeId {
    match block {
        BlockNode::Paragraph(p) => &p.id,
        BlockNode::Table(t) => &t.id,
        BlockNode::OpaqueBlock(o) => &o.id,
    }
}

fn set_block_id(block: &mut BlockNode, id: NodeId) {
    match block {
        BlockNode::Paragraph(p) => p.id = id,
        BlockNode::Table(t) => t.id = id,
        BlockNode::OpaqueBlock(o) => o.id = id,
    }
}

fn find_block_index(blocks: &[TrackedBlock], id: &NodeId) -> Option<usize> {
    blocks.iter().position(|tb| block_id(&tb.block) == id)
}

/// Allocate a unique revision ID by advancing the counter.
/// Each tracked change element (w:ins, w:del) in OOXML requires a unique w:id
/// (ISO 29500-1 §17.13.5). This helper creates a RevisionInfo with the current
/// counter value and then increments it for the next caller.
pub(crate) fn next_revision(base: &RevisionInfo, counter: &mut u32) -> RevisionInfo {
    let rev = RevisionInfo {
        revision_id: *counter,
        identity: 0,
        author: base.author.clone(),
        date: base.date.clone(),
        apply_op_id: base.apply_op_id.clone(),
    };
    *counter += 1;
    rev
}

fn unique_inserted_block_id(blocks: &[TrackedBlock], original_id: &NodeId) -> NodeId {
    if find_block_index(blocks, original_id).is_none() {
        return original_id.clone();
    }
    let mut suffix = 1usize;
    loop {
        let candidate = NodeId::from(format!("{}__ins{}", original_id.0, suffix));
        if find_block_index(blocks, &candidate).is_none() {
            return candidate;
        }
        suffix += 1;
    }
}

fn insert_after_index(blocks: &[TrackedBlock], id: &NodeId) -> Option<usize> {
    find_block_index(blocks, id).map(|idx| idx + 1)
}

fn normalize_insert_position(
    anchor: &Option<NodeId>,
    order_state: &InsertOrderState,
) -> Option<NodeId> {
    match anchor {
        None => order_state.start_tail.clone(),
        Some(anchor_id) => order_state
            .by_anchor
            .get(anchor_id)
            .cloned()
            .or_else(|| Some(anchor_id.clone())),
    }
}

fn note_insert_position(
    original_anchor: &Option<NodeId>,
    inserted_id: NodeId,
    order_state: &mut InsertOrderState,
) {
    match original_anchor {
        None => order_state.start_tail = Some(inserted_id),
        Some(anchor) => {
            order_state.by_anchor.insert(anchor.clone(), inserted_id);
        }
    }
}

/// Like the former `paragraph_text_to_inlines` but carries style_props and formatting_change
/// through to the created TextNodes. Used when the diff detects formatting-only
/// changes (rPrChange) on unchanged text.
fn paragraph_text_to_inlines_with_formatting(
    paragraph_id: &NodeId,
    segment_index: usize,
    text: &str,
    marks: &[crate::domain::Mark],
    style_props: &StyleProps,
    formatting_change: Option<crate::domain::FormattingChange>,
) -> Vec<InlineNode> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut inline_index = 0usize;

    let style_props = style_props.clone();

    let flush_text =
        |buf: &mut String,
         out: &mut Vec<InlineNode>,
         inline_index: &mut usize,
         style_props: &StyleProps,
         formatting_change: &Option<crate::domain::FormattingChange>| {
            if !buf.is_empty() {
                out.push(InlineNode::from(crate::domain::TextNode {
                    id: NodeId::from(format!(
                        "{}_seg{}_t{}",
                        paragraph_id.0, segment_index, *inline_index
                    )),
                    text_role: None,
                    text: std::mem::take(buf),
                    marks: marks.to_vec(),
                    style_props: style_props.clone(),
                    rpr_authored: crate::domain::RunRprAuthored::from_effective(marks, style_props),
                    formatting_change: formatting_change.clone(),
                }));
                *inline_index += 1;
            }
        };

    for ch in text.chars() {
        if ch == '\n' {
            flush_text(
                &mut buf,
                &mut out,
                &mut inline_index,
                &style_props,
                &formatting_change,
            );
            out.push(InlineNode::HardBreak(crate::domain::HardBreakNode {
                id: NodeId::from(format!(
                    "{}_seg{}_br{}",
                    paragraph_id.0, segment_index, inline_index
                )),
                break_type: crate::domain::BreakType::TextWrapping,
            }));
            inline_index += 1;
        } else {
            buf.push(ch);
        }
    }
    flush_text(
        &mut buf,
        &mut out,
        &mut inline_index,
        &style_props,
        &formatting_change,
    );
    out
}

fn prune_empty_text_inlines(segments: &mut Vec<TrackedSegment>) {
    for segment in segments.iter_mut() {
        segment.inlines.retain(|inline| match inline {
            InlineNode::Text(text) => !text.text.is_empty(),
            _ => true,
        });
    }
    segments.retain(|segment| !segment.inlines.is_empty());
}

/// Collect opaque inline nodes from a paragraph's inlines, in order.
fn collect_opaques(block: &BlockNode) -> Vec<crate::domain::OpaqueInlineNode> {
    match block {
        BlockNode::Paragraph(p) => p
            .all_inlines()
            .filter_map(|inline| match inline {
                InlineNode::OpaqueInline(o) => Some((**o).clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Returns the `FieldKind` of an inline node if it is a field opaque.
fn inline_field_kind(inline: &InlineNode) -> Option<&FieldKind> {
    match inline {
        InlineNode::OpaqueInline(opaque) => match &opaque.kind {
            OpaqueKind::Field(data) => Some(&data.field_kind),
            _ => None,
        },
        _ => None,
    }
}

fn auto_field_instruction(text: &str) -> bool {
    let first = text
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    matches!(first.as_str(), "PAGE" | "NUMPAGES" | "SECTIONPAGES")
}

fn range_contains_auto_field_instruction(
    flat: &[(InlineNode, TrackingStatus)],
    start: usize,
    end: usize,
) -> bool {
    flat[start..=end].iter().any(|(inline, _)| match inline {
        InlineNode::OpaqueInline(opaque) => match &opaque.kind {
            OpaqueKind::Field(data) if data.field_kind == FieldKind::Instruction => data
                .instruction_text
                .as_deref()
                .is_some_and(auto_field_instruction),
            _ => false,
        },
        _ => false,
    })
}

/// Coalesce field character sequences that got split across tracking boundaries.
///
/// OOXML field character sequences (fldChar begin / instrText / fldChar separate /
/// result / fldChar end) must have their structural elements (Begin, Instruction,
/// Separate, End) at the same XML nesting level. When the diff splits these across
/// tracked-change containers (e.g., Begin is Normal but Separate is inside w:del),
/// Word treats the result as corruption.
///
/// This function identifies balanced Begin...End field ranges. When field structural
/// opaques within a range have inconsistent tracking statuses, it:
///   1. Drops Deleted copies of field structural opaques (Begin, Instruction, Separate, End).
///   2. Normalizes remaining field structural opaques to Normal status.
///   3. Leaves non-field content (result text between Separate and End) with its
///      original tracking status, so genuine text differences remain tracked.
///   4. Drops Deleted non-field content that was paired with a deleted Separate
///      (since we're keeping only the Inserted/Normal version of the field).
///
/// This pass is shared by both materializers (Invariant M, domain-model §6). It is
/// heavily guarded and returns its input unchanged unless an auto-updating field
/// range (PAGE/NUMPAGES/SECTIONPAGES) has split structural opaques or tracked
/// cached-result text. On ordinary edit-path output, where fields are preserved as
/// Normal, this is a no-op, so running it on both paths makes the pass set
/// identical without changing well-formed output.
pub(crate) fn coalesce_split_field_sequences(segments: Vec<TrackedSegment>) -> Vec<TrackedSegment> {
    // Flatten into (inline, status) pairs.
    let mut flat: Vec<(InlineNode, TrackingStatus)> = Vec::new();
    for seg in &segments {
        for inline in &seg.inlines {
            flat.push((inline.clone(), seg.status.clone()));
        }
    }

    if flat.is_empty() {
        return segments;
    }

    // Identify field sequence ranges using a depth stack.
    // Each entry in `field_ranges` is (start_index, end_index) inclusive.
    let mut field_ranges: Vec<(usize, usize)> = Vec::new();
    let mut stack: Vec<usize> = Vec::new(); // stack of Begin indices

    for (i, (inline, _)) in flat.iter().enumerate() {
        if let Some(kind) = inline_field_kind(inline) {
            match kind {
                FieldKind::Begin => {
                    stack.push(i);
                }
                FieldKind::End => {
                    if let Some(begin_idx) = stack.pop() {
                        field_ranges.push((begin_idx, i));
                    }
                    // If stack is empty, this is an unmatched End — leave it alone.
                }
                _ => {} // Instruction, Separate, Simple are interior — no action needed
            }
        }
    }
    // Any unmatched Begin entries stay on the stack — leave them alone.

    if field_ranges.is_empty() {
        return segments;
    }

    let candidate_ranges: Vec<(usize, usize)> = field_ranges
        .into_iter()
        .filter(|&(start, end)| range_contains_auto_field_instruction(&flat, start, end))
        .collect();

    if candidate_ranges.is_empty() {
        return segments;
    }

    // Check whether any auto-updating field range has split structural opaques
    // or tracked cached result text. Word treats PAGE/NUMPAGES result text as
    // computed output rather than user-authored tracked text.
    let mut needs_repair = false;
    for &(start, end) in &candidate_ranges {
        // Check if field structural opaques have inconsistent statuses.
        let mut structural_statuses: Vec<std::mem::Discriminant<TrackingStatus>> = Vec::new();
        for item in flat[start..=end].iter() {
            if inline_field_kind(&item.0).is_some() {
                structural_statuses.push(std::mem::discriminant(&item.1));
            }
        }
        if structural_statuses.len() > 1 && !structural_statuses.windows(2).all(|w| w[0] == w[1]) {
            needs_repair = true;
            break;
        }
        if flat[start..=end].iter().any(|(inline, status)| {
            inline_field_kind(inline).is_none() && !matches!(status, TrackingStatus::Normal)
        }) {
            needs_repair = true;
            break;
        }
    }

    if !needs_repair {
        return segments;
    }

    // Mark elements for removal or status change.
    // We process in reverse order of field_ranges to keep indices valid,
    // but since we only mark and rebuild, order doesn't matter.
    let mut to_remove: HashSet<usize> = HashSet::new();
    let mut to_normalize: HashSet<usize> = HashSet::new();

    for &(start, end) in &candidate_ranges {
        // Check if structural field opaques in this range have inconsistent statuses.
        let mut structural_statuses: Vec<(usize, std::mem::Discriminant<TrackingStatus>)> =
            Vec::new();
        for (i, item) in flat[start..=end].iter().enumerate() {
            if inline_field_kind(&item.0).is_some() {
                structural_statuses.push((start + i, std::mem::discriminant(&item.1)));
            }
        }

        let all_structural_same = structural_statuses.windows(2).all(|w| w[0].1 == w[1].1);
        let structural_all_normal = structural_statuses
            .iter()
            .all(|(idx, _)| matches!(flat[*idx].1, TrackingStatus::Normal));
        let structural_all_deleted = structural_statuses
            .iter()
            .all(|(idx, _)| matches!(flat[*idx].1, TrackingStatus::Deleted(_)));
        let tracked_non_field_content = flat[start..=end].iter().any(|(inline, status)| {
            inline_field_kind(inline).is_none() && !matches!(status, TrackingStatus::Normal)
        });

        if structural_all_deleted {
            for idx in start..=end {
                to_remove.insert(idx);
            }
            continue;
        }

        let should_repair_structure = !all_structural_same;
        let should_normalize_cached_result = structural_all_normal && tracked_non_field_content;

        if !should_repair_structure && !should_normalize_cached_result {
            continue;
        }

        // Field structure is split. Strategy:
        // - Keep exactly one copy of each structural kind (Begin, Instruction, Separate, End).
        //   Prefer Inserted/Normal over Deleted. If there's an Inserted copy, keep that as Normal.
        //   If there's only a Deleted copy, keep that as Normal (the field exists in base).
        // - For non-field content between Separate and End (result text):
        //   Keep tracked differences (Deleted old text / Inserted new text), BUT
        //   if all result text ends up removed, keep Normal result text.
        //
        // Walk through the range and identify duplicates per structural kind.
        // A "duplicate" is when there are two copies of the same FieldKind with different statuses
        // (e.g., Deleted Separate + Inserted Separate).

        // Group structural opaques by FieldKind.
        let mut begin_indices: Vec<usize> = Vec::new();
        let mut instruction_indices: Vec<usize> = Vec::new();
        let mut separate_indices: Vec<usize> = Vec::new();
        let mut end_indices: Vec<usize> = Vec::new();

        for (offset, (inline, _)) in flat[start..=end].iter().enumerate() {
            if let Some(kind) = inline_field_kind(inline) {
                let idx = start + offset;
                match kind {
                    FieldKind::Begin => begin_indices.push(idx),
                    FieldKind::Instruction => instruction_indices.push(idx),
                    FieldKind::Separate => separate_indices.push(idx),
                    FieldKind::End => end_indices.push(idx),
                    FieldKind::Simple => {} // Simple fields are self-contained, shouldn't appear here
                    // An unknown-type fldChar is not a begin/separate/end
                    // structural boundary, so it never participates in the
                    // structural dedup repair: leave it untouched (opaque).
                    FieldKind::Unknown(_) => {}
                }
            }
        }

        if should_repair_structure {
            // For each structural kind, keep the best copy (Inserted > Normal > Deleted) and remove others.
            for indices in [
                &begin_indices,
                &instruction_indices,
                &separate_indices,
                &end_indices,
            ] {
                if indices.len() <= 1 {
                    // Single copy — just normalize it.
                    for &idx in indices {
                        to_normalize.insert(idx);
                    }
                    continue;
                }

                // Multiple copies — pick the best one.
                let mut best_idx = indices[0];
                let mut best_priority = match &flat[indices[0]].1 {
                    TrackingStatus::Inserted(_) => 2,
                    TrackingStatus::Normal => 1,
                    // Stacked content is pending-deleted: lowest survival
                    // priority, same as Deleted.
                    TrackingStatus::Deleted(_) | TrackingStatus::InsertedThenDeleted(_) => 0,
                };
                for &idx in &indices[1..] {
                    let priority = match &flat[idx].1 {
                        TrackingStatus::Inserted(_) => 2,
                        TrackingStatus::Normal => 1,
                        TrackingStatus::Deleted(_) | TrackingStatus::InsertedThenDeleted(_) => 0,
                    };
                    if priority > best_priority {
                        best_idx = idx;
                        best_priority = priority;
                    }
                }

                // Keep the best, remove the rest.
                for &idx in indices {
                    if idx == best_idx {
                        to_normalize.insert(idx);
                    } else {
                        to_remove.insert(idx);
                    }
                }
            }

            // Handle non-field content (result text) between deleted structural opaques.
            // If a Separate is being removed (Deleted duplicate), also remove the non-field
            // content that follows it until the next structural opaque or end of range.
            for &sep_idx in &separate_indices {
                if !to_remove.contains(&sep_idx) {
                    continue;
                }
                // This Separate is being removed. Remove non-field content after it
                // until we hit another field opaque or end of range.
                for idx in (sep_idx + 1)..=end {
                    if inline_field_kind(&flat[idx].0).is_some() {
                        break; // Stop at next structural opaque.
                    }
                    // Only remove if it has the same status as the removed Separate
                    // (i.e., it's the "deleted" result text paired with the deleted Separate).
                    if std::mem::discriminant(&flat[idx].1)
                        == std::mem::discriminant(&flat[sep_idx].1)
                    {
                        to_remove.insert(idx);
                    }
                }
            }

            // Similarly, handle removed Instruction opaques: remove non-field content after them.
            for &instr_idx in &instruction_indices {
                if !to_remove.contains(&instr_idx) {
                    continue;
                }
                for idx in (instr_idx + 1)..=end {
                    if inline_field_kind(&flat[idx].0).is_some() {
                        break;
                    }
                    if std::mem::discriminant(&flat[idx].1)
                        == std::mem::discriminant(&flat[instr_idx].1)
                    {
                        to_remove.insert(idx);
                    }
                }
            }
        }

        // For each contiguous span of non-field content between structural field
        // markers, keep only the highest-priority copy (Inserted > Normal > Deleted)
        // and normalize it to Normal. Cached auto-field result text should not be
        // emitted as tracked changes.
        let mut idx = start;
        while idx <= end {
            if inline_field_kind(&flat[idx].0).is_some() {
                idx += 1;
                continue;
            }

            let span_start = idx;
            while idx <= end && inline_field_kind(&flat[idx].0).is_none() {
                idx += 1;
            }
            let span_end = idx;

            let span_indices: Vec<usize> = (span_start..span_end)
                .filter(|i| !to_remove.contains(i))
                .collect();
            if span_indices.is_empty() {
                continue;
            }

            let best_priority = span_indices
                .iter()
                .map(|&i| match &flat[i].1 {
                    TrackingStatus::Inserted(_) => 2,
                    TrackingStatus::Normal => 1,
                    TrackingStatus::Deleted(_) | TrackingStatus::InsertedThenDeleted(_) => 0,
                })
                .max()
                .unwrap_or(1);

            for &content_idx in &span_indices {
                let priority = match &flat[content_idx].1 {
                    TrackingStatus::Inserted(_) => 2,
                    TrackingStatus::Normal => 1,
                    TrackingStatus::Deleted(_) | TrackingStatus::InsertedThenDeleted(_) => 0,
                };
                if priority == best_priority {
                    to_normalize.insert(content_idx);
                } else {
                    to_remove.insert(content_idx);
                }
            }
        }
    }

    // Handle orphaned Deleted structural opaques immediately adjacent to an
    // auto-field candidate range. These arise when the diff leaves the "old"
    // field shell outside the matched Begin...End range but the surviving auto
    // field lives in the adjacent candidate range.
    for &(start, end) in &candidate_ranges {
        let mut idx = start;
        while idx > 0 {
            let probe = idx - 1;
            if to_remove.contains(&probe) {
                idx = probe;
                continue;
            }
            let Some(kind) = inline_field_kind(&flat[probe].0) else {
                break;
            };
            if !matches!(
                kind,
                FieldKind::Begin | FieldKind::Instruction | FieldKind::Separate | FieldKind::End
            ) || !matches!(flat[probe].1, TrackingStatus::Deleted(_))
            {
                break;
            }
            to_remove.insert(probe);
            idx = probe;
        }

        let mut idx = end + 1;
        while idx < flat.len() {
            if to_remove.contains(&idx) {
                idx += 1;
                continue;
            }
            let Some(kind) = inline_field_kind(&flat[idx].0) else {
                break;
            };
            if !matches!(
                kind,
                FieldKind::Begin | FieldKind::Instruction | FieldKind::Separate | FieldKind::End
            ) || !matches!(flat[idx].1, TrackingStatus::Deleted(_))
            {
                break;
            }
            to_remove.insert(idx);
            idx += 1;
        }
    }

    if to_remove.is_empty() && to_normalize.is_empty() {
        return segments;
    }

    // Rebuild: remove marked elements and normalize marked statuses.
    let mut rebuilt: Vec<(InlineNode, TrackingStatus)> = Vec::new();
    for (i, (inline, status)) in flat.into_iter().enumerate() {
        if to_remove.contains(&i) {
            continue;
        }
        let status = if to_normalize.contains(&i) {
            TrackingStatus::Normal
        } else {
            status
        };
        rebuilt.push((inline, status));
    }

    // Re-group into contiguous segments by status.
    let mut result: Vec<TrackedSegment> = Vec::new();
    for (inline, status) in rebuilt {
        if let Some(last) = result.last_mut()
            && last.status == status
        {
            last.inlines.push(inline);
            continue;
        }
        result.push(TrackedSegment {
            status,
            inlines: vec![inline],
        });
    }
    result
}

/// Convert inline changes to tracked segments, reconstructing opaque nodes
/// from `U+FFFC` placeholders in the diff text.
///
/// `base_opaques` and `target_opaques` provide the original opaque nodes
/// in order. When a `U+FFFC` appears in Unchanged/Deleted text, the next
/// opaque from `base_opaques` is used. For Inserted text, the next from
/// `target_opaques`. For Unchanged, both cursors advance.
fn inline_changes_to_segments_with_opaques(
    paragraph_id: &NodeId,
    inline_changes: &[InlineChange],
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    base_opaques: &[crate::domain::OpaqueInlineNode],
    target_opaques: &[crate::domain::OpaqueInlineNode],
) -> Result<Vec<TrackedSegment>, MergeError> {
    let mut segments = Vec::new();
    let mut base_opaque_idx = 0usize;
    let mut target_opaque_idx = 0usize;

    for (segment_index, change) in inline_changes.iter().enumerate() {
        let (status, text, marks, style_props, formatting_change) = match change {
            InlineChange::Unchanged {
                text,
                marks,
                style_props,
                formatting_change,
            } => {
                // Fill identity on FormattingChange from revision info. Each
                // formatting change is its own revision — mint a fresh id from
                // the SAME counter the ins/del statuses use (a shared id across
                // two changes would break selector addressing and trip the
                // validator's I-ANN-001 on output).
                let fc = formatting_change
                    .as_ref()
                    .map(|fc| crate::domain::FormattingChange {
                        previous_marks: fc.previous_marks.clone(),
                        previous_style_props: fc.previous_style_props.clone(),
                        previous_rpr_authored: fc.previous_rpr_authored,
                        revision_id: next_revision(revision, rev_counter).revision_id,
                        identity: 0,
                        author: revision.author.clone().unwrap_or_default(),
                        date: revision.date.clone(),
                    });
                (TrackingStatus::Normal, text, marks, style_props, fc)
            }
            InlineChange::Inserted {
                text,
                marks,
                style_props,
                ..
            } => (
                TrackingStatus::Inserted(next_revision(revision, rev_counter)),
                text,
                marks,
                style_props,
                None,
            ),
            InlineChange::Deleted {
                text,
                marks,
                style_props,
                ..
            } => (
                TrackingStatus::Deleted(next_revision(revision, rev_counter)),
                text,
                marks,
                style_props,
                None,
            ),
            InlineChange::Opaque { segment_type, .. } => {
                // Opaque changes (images, equations, fields) are handled directly
                // here rather than through the text+U+FFFC path.
                match segment_type {
                    InlineChangeSegmentType::Delete => {
                        let opaque =
                            base_opaques
                                .get(base_opaque_idx)
                                .ok_or_else(|| MergeError {
                                    message: format!(
                                        "base opaque index {} out of range (have {})",
                                        base_opaque_idx,
                                        base_opaques.len()
                                    ),
                                    context: format!(
                                        "paragraph {}, InlineChange::Opaque Delete",
                                        paragraph_id.0
                                    ),
                                })?;
                        base_opaque_idx += 1;
                        segments.push(TrackedSegment {
                            status: TrackingStatus::Deleted(next_revision(revision, rev_counter)),
                            inlines: vec![InlineNode::from(opaque.clone())],
                        });
                    }
                    InlineChangeSegmentType::Insert => {
                        let opaque =
                            target_opaques
                                .get(target_opaque_idx)
                                .ok_or_else(|| MergeError {
                                    message: format!(
                                        "target opaque index {} out of range (have {})",
                                        target_opaque_idx,
                                        target_opaques.len()
                                    ),
                                    context: format!(
                                        "paragraph {}, InlineChange::Opaque Insert",
                                        paragraph_id.0
                                    ),
                                })?;
                        target_opaque_idx += 1;
                        segments.push(TrackedSegment {
                            status: TrackingStatus::Inserted(next_revision(revision, rev_counter)),
                            inlines: vec![InlineNode::from(opaque.clone())],
                        });
                    }
                    InlineChangeSegmentType::Equal => {
                        let base_opaque =
                            base_opaques
                                .get(base_opaque_idx)
                                .ok_or_else(|| MergeError {
                                    message: format!(
                                        "base opaque index {} out of range (have {})",
                                        base_opaque_idx,
                                        base_opaques.len()
                                    ),
                                    context: format!(
                                        "paragraph {}, InlineChange::Opaque Equal (base)",
                                        paragraph_id.0
                                    ),
                                })?;
                        let target_opaque =
                            target_opaques
                                .get(target_opaque_idx)
                                .ok_or_else(|| MergeError {
                                    message: format!(
                                        "target opaque index {} out of range (have {})",
                                        target_opaque_idx,
                                        target_opaques.len()
                                    ),
                                    context: format!(
                                        "paragraph {}, InlineChange::Opaque Equal (target)",
                                        paragraph_id.0
                                    ),
                                })?;
                        base_opaque_idx += 1;
                        target_opaque_idx += 1;

                        let content_changed =
                            match (&base_opaque.content_hash, &target_opaque.content_hash) {
                                (Some(bh), Some(th)) => bh != th,
                                _ => false,
                            };

                        if content_changed {
                            if matches!(
                                base_opaque.kind,
                                OpaqueKind::OmmlBlock | OpaqueKind::OmmlInline
                            ) && matches!(
                                target_opaque.kind,
                                OpaqueKind::OmmlBlock | OpaqueKind::OmmlInline
                            ) {
                                // Math is treated as opaque: emit only the new
                                // equation as Normal. m:oMathPara cannot appear
                                // inside w:del/w:ins (schema-invalid), so we
                                // cannot represent the old equation as deleted
                                // at the inline level. Word handles this by
                                // deleting/inserting the entire paragraph.
                                segments.push(TrackedSegment {
                                    status: TrackingStatus::Normal,
                                    inlines: vec![InlineNode::from(target_opaque.clone())],
                                });
                            } else {
                                segments.push(TrackedSegment {
                                    status: TrackingStatus::Deleted(next_revision(
                                        revision,
                                        rev_counter,
                                    )),
                                    inlines: vec![InlineNode::from(base_opaque.clone())],
                                });
                                segments.push(TrackedSegment {
                                    status: TrackingStatus::Inserted(next_revision(
                                        revision,
                                        rev_counter,
                                    )),
                                    inlines: vec![InlineNode::from(target_opaque.clone())],
                                });
                            }
                        } else {
                            // Content is the same — emit as Normal
                            segments.push(TrackedSegment {
                                status: TrackingStatus::Normal,
                                inlines: vec![InlineNode::from(base_opaque.clone())],
                            });
                        }
                    }
                }
                continue;
            }
        };

        // Check if this text contains U+FFFC opaque placeholders
        if text.contains('\u{FFFC}') {
            if matches!(status, TrackingStatus::Normal) {
                // For Normal segments with opaques, detect content changes
                // (e.g. replaced images) and emit Deleted+Inserted when needed.
                let new_segs = reconstruct_opaques_with_change_detection(
                    paragraph_id,
                    segment_index,
                    text,
                    marks,
                    style_props,
                    formatting_change,
                    revision,
                    rev_counter,
                    base_opaques,
                    target_opaques,
                    &mut base_opaque_idx,
                    &mut target_opaque_idx,
                );
                segments.extend(new_segs);
            } else {
                // Deleted or Inserted — use straightforward reconstruction
                let inlines = reconstruct_inlines_with_opaques(
                    paragraph_id,
                    segment_index,
                    text,
                    marks,
                    style_props,
                    &status,
                    base_opaques,
                    target_opaques,
                    &mut base_opaque_idx,
                    &mut target_opaque_idx,
                );
                if !inlines.is_empty() {
                    segments.push(TrackedSegment { status, inlines });
                }
            }
        } else {
            // No opaques — standard text reconstruction with formatting
            let inlines = paragraph_text_to_inlines_with_formatting(
                paragraph_id,
                segment_index,
                text,
                marks,
                style_props,
                formatting_change,
            );
            if inlines.is_empty() {
                continue;
            }
            segments.push(TrackedSegment { status, inlines });
        }
    }
    // Normalize only auto-updating field sequences (PAGE / NUMPAGES / SECTIONPAGES).
    // Word does not track cached result changes for these fields; keeping the
    // field structure/result as Normal avoids corrupt accepted output while
    // leaving user-authored field results (e.g. HYPERLINK display text) untouched.
    let segments = coalesce_split_field_sequences(segments);
    let segments = normalize_paragraph_opaque_reading_order(segments);
    // Invariant M (domain-model §6): apply the edit path's compaction here too,
    // as the final pass, so both materializers normalize segment boundaries
    // identically. Runs last so the field/opaque passes above still see the
    // un-merged boundaries they depend on.
    let mut segments = segments;
    crate::edit::normalize_segments(&mut segments);
    Ok(segments)
}

fn is_paragraph_level_opaque_for_tracking(inline: &InlineNode) -> bool {
    match inline {
        InlineNode::OpaqueInline(opaque) => matches!(
            &opaque.kind,
            OpaqueKind::Hyperlink(_)
                | OpaqueKind::Field(FieldData {
                    field_kind: FieldKind::Simple,
                    ..
                })
                | OpaqueKind::OmmlBlock
        ),
        _ => false,
    }
}

fn paragraph_opaque_dedup_key_for_tracking(inline: &InlineNode) -> Option<String> {
    match inline {
        InlineNode::OpaqueInline(opaque) => match &opaque.kind {
            OpaqueKind::Hyperlink(data) => Some(format!(
                "hyperlink:{:?}:{:?}:{:?}",
                data.url, data.anchor, data.text
            )),
            OpaqueKind::Field(data) if data.field_kind == FieldKind::Simple => Some(format!(
                "fldSimple:{:?}:{:?}",
                data.instruction_text, data.result_text
            )),
            OpaqueKind::OmmlBlock => Some(format!("omml-block:{:?}", opaque.content_hash)),
            _ => None,
        },
        _ => None,
    }
}

fn segment_is_only_paragraph_opaques_for_tracking(segment: &TrackedSegment) -> bool {
    !segment.inlines.is_empty()
        && segment
            .inlines
            .iter()
            .all(is_paragraph_level_opaque_for_tracking)
}

fn segments_share_paragraph_opaques_for_tracking(a: &TrackedSegment, b: &TrackedSegment) -> bool {
    let a_keys: Vec<String> = a
        .inlines
        .iter()
        .filter_map(paragraph_opaque_dedup_key_for_tracking)
        .collect();
    if a_keys.is_empty() {
        return false;
    }
    let b_keys: Vec<String> = b
        .inlines
        .iter()
        .filter_map(paragraph_opaque_dedup_key_for_tracking)
        .collect();
    a_keys == b_keys
}

/// Opaque reading-order pass, shared by both materializers (Invariant M,
/// domain-model §6). Only rewrites the specific Deleted/Normal/Inserted segment
/// pattern produced when a paragraph-level opaque (hyperlink/simple-field/omml)
/// is moved across a tracked change; every other segment is copied through
/// unchanged. On edit-path output (which never produces that delete-then-
/// reinsert opaque shape) it is a structural no-op.
pub(crate) fn normalize_paragraph_opaque_reading_order(
    segments: Vec<TrackedSegment>,
) -> Vec<TrackedSegment> {
    let mut result = Vec::new();
    let mut i = 0usize;

    while i < segments.len() {
        if i + 4 < segments.len() {
            let s0 = &segments[i];
            let s1 = &segments[i + 1];
            let s2 = &segments[i + 2];
            let s3 = &segments[i + 3];
            let s4 = &segments[i + 4];

            if matches!(s0.status, TrackingStatus::Deleted(_))
                && matches!(s2.status, TrackingStatus::Deleted(_))
                && matches!(s3.status, TrackingStatus::Inserted(_))
                && matches!(s4.status, TrackingStatus::Inserted(_))
                && matches!(s1.status, TrackingStatus::Normal)
                && segment_is_only_paragraph_opaques_for_tracking(s1)
            {
                result.push(s0.clone());
                result.push(s3.clone());
                result.push(s1.clone());
                result.push(s2.clone());
                result.push(s4.clone());
                i += 5;
                continue;
            }
        }

        if i + 5 < segments.len() {
            let s0 = &segments[i];
            let s1 = &segments[i + 1];
            let s2 = &segments[i + 2];
            let s3 = &segments[i + 3];
            let s4 = &segments[i + 4];
            let s5 = &segments[i + 5];

            if matches!(s0.status, TrackingStatus::Deleted(_))
                && matches!(s1.status, TrackingStatus::Deleted(_))
                && matches!(s2.status, TrackingStatus::Deleted(_))
                && matches!(s3.status, TrackingStatus::Inserted(_))
                && matches!(s4.status, TrackingStatus::Inserted(_))
                && matches!(s5.status, TrackingStatus::Inserted(_))
                && segment_is_only_paragraph_opaques_for_tracking(s1)
                && segment_is_only_paragraph_opaques_for_tracking(s4)
                && segments_share_paragraph_opaques_for_tracking(s1, s4)
            {
                let normal_opaque = TrackedSegment {
                    status: TrackingStatus::Normal,
                    inlines: s4.inlines.clone(),
                };
                result.push(s0.clone());
                result.push(s3.clone());
                result.push(normal_opaque);
                result.push(s2.clone());
                result.push(s5.clone());
                i += 6;
                continue;
            }
        }

        result.push(segments[i].clone());
        i += 1;
    }

    result
}

/// Reconstruct inline nodes from diff text that contains `U+FFFC` placeholders.
///
/// Splits the text at each `U+FFFC` boundary, creates `TextNode`s for the text
/// parts and substitutes the original `OpaqueInlineNode`s for each placeholder.
#[allow(clippy::too_many_arguments)]
fn reconstruct_inlines_with_opaques(
    paragraph_id: &NodeId,
    segment_index: usize,
    text: &str,
    marks: &[crate::domain::Mark],
    style_props: &StyleProps,
    status: &TrackingStatus,
    base_opaques: &[crate::domain::OpaqueInlineNode],
    target_opaques: &[crate::domain::OpaqueInlineNode],
    base_opaque_idx: &mut usize,
    target_opaque_idx: &mut usize,
) -> Vec<InlineNode> {
    let parts: Vec<&str> = text.split('\u{FFFC}').collect();
    let mut result = Vec::new();
    let mut inline_idx = 0usize;

    for (i, part) in parts.iter().enumerate() {
        // Emit text part, preserving style_props from the InlineChange so
        // that accept_all produces runs matching the canonical target formatting.
        // We deliberately clear rpr_authored because these props are inherited
        // from the paragraph style, not applied directly to the run.
        if !part.is_empty() {
            let mut text_inlines = paragraph_text_to_inlines_with_formatting(
                paragraph_id,
                segment_index * 1000 + inline_idx,
                part,
                marks,
                style_props,
                None, // no formatting_change for Deleted/Inserted segments
            );
            for inline in &mut text_inlines {
                if let InlineNode::Text(t) = inline {
                    t.rpr_authored = crate::domain::RunRprAuthored::default();
                }
            }
            inline_idx += text_inlines.len();
            result.extend(text_inlines);
        }

        // Emit opaque after each split boundary (except after the last part)
        if i < parts.len() - 1 {
            let opaque = match status {
                TrackingStatus::Normal => {
                    // Unchanged: consume from both lists
                    let o = base_opaques.get(*base_opaque_idx).cloned();
                    *base_opaque_idx += 1;
                    *target_opaque_idx += 1;
                    o
                }
                TrackingStatus::Deleted(_) => {
                    let o = base_opaques.get(*base_opaque_idx).cloned();
                    *base_opaque_idx += 1;
                    o
                }
                TrackingStatus::Inserted(_) => {
                    let o = target_opaques.get(*target_opaque_idx).cloned();
                    *target_opaque_idx += 1;
                    o
                }
                TrackingStatus::InsertedThenDeleted(_) => unreachable!(
                    "the merge differ emits only Normal/Inserted/Deleted; stacked \
                     segments come from import or the splice, never from merge"
                ),
            };

            if let Some(opaque_node) = opaque {
                result.push(InlineNode::from(opaque_node));
            }
            inline_idx += 1;
        }
    }

    result
}

/// Reconstruct inlines for a Normal segment that contains opaque placeholders,
/// detecting when an opaque's content has changed (e.g. image replacement).
///
/// For each `U+FFFC` placeholder, compares `content_hash` between the base and
/// target opaque. If they match, emits a single Normal segment. If they differ,
/// emits a Deleted segment (with the base opaque) followed by an Inserted segment
/// (with the target opaque), producing proper tracked changes in the output.
#[allow(clippy::too_many_arguments)]
fn reconstruct_opaques_with_change_detection(
    paragraph_id: &NodeId,
    segment_index: usize,
    text: &str,
    marks: &[crate::domain::Mark],
    style_props: &StyleProps,
    formatting_change: Option<crate::domain::FormattingChange>,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    base_opaques: &[crate::domain::OpaqueInlineNode],
    target_opaques: &[crate::domain::OpaqueInlineNode],
    base_opaque_idx: &mut usize,
    target_opaque_idx: &mut usize,
) -> Vec<TrackedSegment> {
    let parts: Vec<&str> = text.split('\u{FFFC}').collect();
    let mut segments: Vec<TrackedSegment> = Vec::new();

    // We accumulate normal inlines until we encounter a changed opaque,
    // at which point we flush the accumulated normal inlines, emit del+ins,
    // and start a new normal accumulator.
    let mut normal_inlines: Vec<InlineNode> = Vec::new();

    for (i, part) in parts.iter().enumerate() {
        // Emit text part into normal accumulator, preserving style_props
        // and formatting_change from the InlineChange so that accept_all
        // produces runs matching the canonical target formatting.
        if !part.is_empty() {
            let text_inlines = paragraph_text_to_inlines_with_formatting(
                paragraph_id,
                segment_index * 1000 + i * 10,
                part,
                marks,
                style_props,
                formatting_change.clone(),
            );
            normal_inlines.extend(text_inlines);
        }

        // Emit opaque after each split boundary (except after the last part)
        if i < parts.len() - 1 {
            let base_opaque = base_opaques.get(*base_opaque_idx).cloned();
            let target_opaque = target_opaques.get(*target_opaque_idx).cloned();
            *base_opaque_idx += 1;
            *target_opaque_idx += 1;

            let content_changed = match (&base_opaque, &target_opaque) {
                (Some(b), Some(t)) => match (&b.content_hash, &t.content_hash) {
                    (Some(bh), Some(th)) => bh != th,
                    _ => false,
                },
                _ => false,
            };

            if content_changed {
                // Flush any accumulated normal inlines
                if !normal_inlines.is_empty() {
                    segments.push(TrackedSegment {
                        status: TrackingStatus::Normal,
                        inlines: std::mem::take(&mut normal_inlines),
                    });
                }

                // Emit deleted base opaque + inserted target opaque.
                // Note: paragraph-level opaques (OmmlBlock) should never
                // reach here — the diff layer reclassifies wholly-opaque
                // paragraph changes as BlockDeleted + BlockInserted.
                if let Some(bo) = base_opaque {
                    segments.push(TrackedSegment {
                        status: TrackingStatus::Deleted(next_revision(revision, rev_counter)),
                        inlines: vec![InlineNode::from(bo)],
                    });
                }
                if let Some(to) = target_opaque {
                    segments.push(TrackedSegment {
                        status: TrackingStatus::Inserted(next_revision(revision, rev_counter)),
                        inlines: vec![InlineNode::from(to)],
                    });
                }
            } else {
                // Same opaque payload — keep it as normal content, but adopt the
                // target wrapper formatting so accept-all matches the target's
                // direct run properties around fldChar/instrText-like shells.
                if let Some(opaque_node) = target_opaque.or(base_opaque) {
                    normal_inlines.push(InlineNode::from(opaque_node));
                }
            }
        }
    }

    // Flush remaining normal inlines
    if !normal_inlines.is_empty() {
        segments.push(TrackedSegment {
            status: TrackingStatus::Normal,
            inlines: normal_inlines,
        });
    }

    segments
}

fn apply_block_deleted(
    blocks: &mut [TrackedBlock],
    block_id_to_delete: &NodeId,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    context: &str,
) -> Result<(), MergeError> {
    let Some(idx) = find_block_index(blocks, block_id_to_delete) else {
        return Err(MergeError {
            message: "block_id for deletion not found in base model".to_string(),
            context: format!("{context}:{}", block_id_to_delete.0),
        });
    };
    let tb = &mut blocks[idx];
    tb.status = TrackingStatus::Deleted(next_revision(revision, rev_counter));
    if let BlockNode::Paragraph(p) = &mut tb.block {
        p.para_mark_status = Some(TrackingStatus::Deleted(next_revision(
            revision,
            rev_counter,
        )));
    }
    Ok(())
}

fn apply_block_inserted(
    blocks: &mut Vec<TrackedBlock>,
    after_block_id: &Option<NodeId>,
    block: &BlockNode,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    order_state: &mut InsertOrderState,
    context: &str,
) -> Result<(), MergeError> {
    let normalized_anchor = normalize_insert_position(after_block_id, order_state);
    let insert_idx = match normalized_anchor {
        None => 0usize,
        Some(anchor) => insert_after_index(blocks, &anchor).ok_or_else(|| MergeError {
            message: "insert anchor not found in base model".to_string(),
            context: format!("{context}:{}", anchor.0),
        })?,
    };
    let mut inserted_block = block.clone();
    // Numbering definitions are now merged from the target DOCX into the base
    // by `merge_target_numbering` in serialize_canonical_docx, so inserted
    // paragraphs keep their w:numPr references. We no longer materialize
    // numbering as literal text since the definitions will be available.
    let inserted_id = unique_inserted_block_id(blocks, block_id(&inserted_block));
    set_block_id(&mut inserted_block, inserted_id.clone());
    blocks.insert(
        insert_idx,
        TrackedBlock {
            status: TrackingStatus::Inserted(next_revision(revision, rev_counter)),
            block: inserted_block,
            move_id: None,
            block_sdt_wrap: None,
        },
    );
    note_insert_position(after_block_id, inserted_id, order_state);
    Ok(())
}

/// Compute the effective user-visible text prefix for a paragraph.
///
/// Returns the synthesized numbering text (from auto-numbering) or literal prefix,
/// whichever is present. This is what appears before the paragraph body text.
fn effective_text_prefix(p: &ParagraphNode) -> Option<&str> {
    if let Some(n) = &p.numbering
        && !n.synthesized_text.is_empty()
    {
        return Some(&n.synthesized_text);
    }
    p.literal_prefix.as_deref()
}

#[derive(Debug, Clone)]
struct PrefixMaterializationPlan {
    old_prefix: Option<String>,
    new_prefix: Option<String>,
    deleted_kind: Option<MaterializedPrefixKind>,
    inserted_kind: Option<MaterializedPrefixKind>,
    emit_deleted_prefix: bool,
    emit_inserted_prefix: bool,
    target_uses_structural_numbering: bool,
}

fn plan_prefix_materialization(
    source: &ParagraphNode,
    target: &ParagraphNode,
) -> Option<PrefixMaterializationPlan> {
    let source_uses_structural_numbering = source.numbering.is_some();
    let target_uses_structural_numbering = target.numbering.is_some();
    let both_structural = source_uses_structural_numbering && target_uses_structural_numbering;
    let source_has_prefix = source_uses_structural_numbering || source.literal_prefix.is_some();
    let target_adds_structural = !source_has_prefix && target_uses_structural_numbering;
    if both_structural || target_adds_structural {
        return None;
    }

    let old_prefix = effective_text_prefix(source).map(str::to_owned);
    let new_prefix = effective_text_prefix(target).map(str::to_owned);
    let prefix_text_changed = old_prefix != new_prefix;

    // Word accept/reject needs explicit inline prefix text when the visible
    // prefix lives in paragraph text on one side but in w:numPr on the other.
    // Comparing the label text alone is not enough; representation changes are
    // semantic for redline because reject cannot recover a baked prefix from
    // a dropped numPr, and accept cannot recover a baked target prefix from
    // paragraph properties alone.
    let emit_deleted_prefix = if source.literal_prefix.is_some() {
        prefix_text_changed || target_uses_structural_numbering || target.literal_prefix.is_none()
    } else if source_uses_structural_numbering {
        !target_uses_structural_numbering
    } else {
        false
    };
    let emit_inserted_prefix = target.literal_prefix.is_some()
        && (prefix_text_changed
            || source_uses_structural_numbering
            || source.literal_prefix.is_none());

    if !emit_deleted_prefix && !emit_inserted_prefix {
        return None;
    }

    Some(PrefixMaterializationPlan {
        old_prefix,
        new_prefix,
        deleted_kind: emit_deleted_prefix.then_some(if source_uses_structural_numbering {
            MaterializedPrefixKind::StructuralDeleted
        } else {
            MaterializedPrefixKind::LiteralDeleted
        }),
        inserted_kind: emit_inserted_prefix.then_some(MaterializedPrefixKind::LiteralInserted),
        emit_deleted_prefix,
        emit_inserted_prefix,
        target_uses_structural_numbering,
    })
}

/// Build the inline text that re-inlines a hoisted literal prefix into the
/// body: `leading_ws + label + trailing_ws`. Import captured the surrounding
/// whitespace VERBATIM (`literal_prefix_leading_ws` / `literal_prefix_trailing_ws`,
/// XML 1.0 §2.10 significant whitespace), so the materializer must re-emit it
/// verbatim — reconstructing it from the lossy boolean model
/// (`has_trailing_tab` + `leading_tab_count`) drops a plain-space separator
/// whenever the label is preceded by leading tabs (e.g. `\t\t\t\t28. pluku`,
/// where the ".28"/body separator is a single space in the same run). A model
/// that never captured the verbatim strings (legacy) falls back to the old
/// boolean reconstruction.
fn materialized_prefix_text(prefix: &str, source: &ParagraphNode) -> String {
    let mut text = String::new();
    // Leading whitespace verbatim (spaces and tabs in source order).
    if source.literal_prefix_leading_ws.is_empty() {
        for _ in 0..source.literal_prefix_leading_tab_count {
            text.push('\t');
        }
    } else {
        text.push_str(&source.literal_prefix_leading_ws);
    }
    text.push_str(prefix.trim());
    // Separator whitespace verbatim; legacy models without it fall back to the
    // historical reconstruction (a tab when the separator was a tab, else a
    // single space only when there were no leading tabs).
    if !source.literal_prefix_trailing_ws.is_empty() {
        text.push_str(&source.literal_prefix_trailing_ws);
    } else if source.literal_prefix_has_trailing_tab {
        text.push('\t');
    } else if source.literal_prefix_leading_tab_count == 0 {
        text.push(' ');
    }
    text
}

fn sync_literal_prefix_geometry(target: &ParagraphNode, paragraph: &mut ParagraphNode) {
    paragraph.literal_prefix_marks = target.literal_prefix_marks.clone();
    paragraph.literal_prefix_style_props = target.literal_prefix_style_props.clone();
    paragraph.literal_prefix_rpr_authored = target.literal_prefix_rpr_authored;
    paragraph.literal_prefix_leading_tab_twips = target.literal_prefix_leading_tab_twips;
    paragraph.literal_prefix_leading_tab_count = target.literal_prefix_leading_tab_count;
    paragraph.literal_prefix_leading_ws = target.literal_prefix_leading_ws.clone();
    paragraph.literal_prefix_trailing_ws = target.literal_prefix_trailing_ws.clone();
    paragraph.literal_prefix_has_trailing_tab = target.literal_prefix_has_trailing_tab;
    paragraph.literal_prefix_trailing_tab_stop_twips =
        target.literal_prefix_trailing_tab_stop_twips;
}

#[derive(Clone)]
struct PositionedStructuralMarker {
    offset: usize,
    inline: InlineNode,
}

fn inline_text_width(inline: &InlineNode) -> usize {
    match inline {
        InlineNode::Text(t) => t.text.chars().count(),
        InlineNode::HardBreak(_) => 1,
        InlineNode::OpaqueInline(_) => 1,
        InlineNode::Decoration(_)
        | InlineNode::CommentRangeStart { .. }
        | InlineNode::CommentRangeEnd { .. }
        | InlineNode::CommentReference { .. } => 0,
    }
}

fn is_comment_marker(inline: &InlineNode) -> bool {
    matches!(
        inline,
        InlineNode::CommentRangeStart { .. }
            | InlineNode::CommentRangeEnd { .. }
            | InlineNode::CommentReference { .. }
    )
}

fn structural_marker_identity(inline: &InlineNode, offset: usize) -> Option<String> {
    match inline {
        InlineNode::Decoration(d) => Some(format!(
            "deco:{offset}:{}",
            String::from_utf8_lossy(d.raw_xml.as_deref().unwrap_or_default())
        )),
        InlineNode::CommentRangeStart { id } => Some(format!("comment-start:{offset}:{id}")),
        InlineNode::CommentRangeEnd { id } => Some(format!("comment-end:{offset}:{id}")),
        InlineNode::CommentReference { id } => Some(format!("comment-ref:{offset}:{id}")),
        _ => None,
    }
}

fn collect_positioned_structural_markers(
    segments: &[TrackedSegment],
) -> Vec<PositionedStructuralMarker> {
    let mut markers = Vec::new();
    let mut offset = 0usize;
    for seg in segments {
        for inline in &seg.inlines {
            match inline {
                InlineNode::Decoration(_)
                | InlineNode::CommentRangeStart { .. }
                | InlineNode::CommentRangeEnd { .. }
                | InlineNode::CommentReference { .. } => {
                    markers.push(PositionedStructuralMarker {
                        offset,
                        inline: inline.clone(),
                    });
                }
                _ => {
                    offset += inline_text_width(inline);
                }
            }
        }
    }
    markers
}

pub(crate) fn inject_structural_markers_at_offsets(
    final_segments: &mut Vec<TrackedSegment>,
    original_segments: &[TrackedSegment],
    target_segments: Option<&[TrackedSegment]>,
) {
    let mut markers = collect_positioned_structural_markers(original_segments);
    let mut seen: HashSet<String> = markers
        .iter()
        .filter_map(|marker| structural_marker_identity(&marker.inline, marker.offset))
        .collect();

    if let Some(target_segments) = target_segments {
        for marker in collect_positioned_structural_markers(target_segments) {
            let Some(identity) = structural_marker_identity(&marker.inline, marker.offset) else {
                continue;
            };
            if seen.insert(identity) {
                let mut inline = marker.inline;
                if let InlineNode::Decoration(ref mut d) = inline {
                    d.origin = Some("target".to_string());
                }
                markers.push(PositionedStructuralMarker {
                    offset: marker.offset,
                    inline,
                });
            }
        }
    }

    if markers.is_empty() {
        return;
    }

    markers.sort_by_key(|marker| marker.offset);
    let mut pending = std::collections::VecDeque::from(markers);
    let mut text_offset = 0usize;

    for seg in final_segments.iter_mut() {
        let mut new_inlines = Vec::with_capacity(seg.inlines.len());
        for inline in seg.inlines.drain(..) {
            // Markers at or before this inline's start position go first.
            while let Some(marker) = pending.front() {
                if marker.offset <= text_offset {
                    new_inlines.push(pending.pop_front().unwrap().inline);
                } else {
                    break;
                }
            }
            let width = inline_text_width(&inline);
            // A COMMENT marker whose offset falls STRICTLY inside a text run must
            // split it, so an interior comment-range boundary lands on the right
            // character even when the run was coalesced (the diff produces one Text
            // node for the whole unchanged span). Decorations keep their existing
            // boundary placement (no split), so non-comment redline output is
            // unchanged. Markers at the end boundary are left for the next inline /
            // the trailing sweep below.
            let split_here = |m: &PositionedStructuralMarker| {
                m.offset > text_offset
                    && m.offset < text_offset + width
                    && is_comment_marker(&m.inline)
            };
            if let InlineNode::Text(t) = &inline
                && pending.front().is_some_and(split_here)
            {
                let chars: Vec<char> = t.text.chars().collect();
                let mut consumed = 0usize;
                while pending.front().is_some_and(split_here) {
                    let marker = pending.pop_front().unwrap();
                    let cut = marker.offset - text_offset;
                    if cut > consumed {
                        let mut piece = (**t).clone();
                        piece.id = NodeId::new(format!("{}_cm{consumed}", t.id));
                        piece.text = chars[consumed..cut].iter().collect();
                        new_inlines.push(InlineNode::Text(Box::new(piece)));
                        consumed = cut;
                    }
                    new_inlines.push(marker.inline);
                }
                let mut tail = (**t).clone();
                tail.id = NodeId::new(format!("{}_cm{consumed}", t.id));
                tail.text = chars[consumed..].iter().collect();
                new_inlines.push(InlineNode::Text(Box::new(tail)));
            } else {
                new_inlines.push(inline);
            }
            text_offset += width;
        }
        while let Some(marker) = pending.front() {
            if marker.offset <= text_offset {
                new_inlines.push(pending.pop_front().unwrap().inline);
            } else {
                break;
            }
        }
        seg.inlines = new_inlines;
    }

    if !pending.is_empty() {
        if let Some(last) = final_segments.last_mut() {
            last.inlines
                .extend(pending.into_iter().map(|marker| marker.inline));
        } else {
            final_segments.push(TrackedSegment {
                status: TrackingStatus::Normal,
                inlines: pending.into_iter().map(|marker| marker.inline).collect(),
            });
        }
    }
}

fn strip_materialized_prefix_geometry(text: &str) -> (String, u8, bool) {
    let leading_tab_count = text.bytes().take_while(|b| *b == b'\t').count() as u8;
    let has_trailing_tab = text.ends_with('\t');
    let text_without_leading = text.trim_start_matches('\t');
    let label = if has_trailing_tab {
        text_without_leading
            .strip_suffix('\t')
            .unwrap_or(text_without_leading)
    } else {
        text_without_leading
            .strip_suffix(' ')
            .unwrap_or(text_without_leading)
    };
    (label.to_string(), leading_tab_count, has_trailing_tab)
}

/// Create a prefix `TextNode` that inherits direct formatting (marks,
/// style_props, has_direct_* flags) from `source`'s first content run.
/// `formatting_change` is deliberately NOT copied — the prefix itself
/// has no tracked formatting change.
fn make_prefix_text_node(
    id: NodeId,
    kind: MaterializedPrefixKind,
    text: String,
    source: &ParagraphNode,
) -> TextNode {
    if source.literal_prefix.is_some() {
        return TextNode {
            id,
            text_role: Some(TextRole::MaterializedPrefix(kind)),
            text,
            marks: source.literal_prefix_marks.clone(),
            style_props: source.literal_prefix_style_props.clone(),
            rpr_authored: source.literal_prefix_rpr_authored,
            formatting_change: None,
        };
    }

    match source.first_content_text_node() {
        Some(t) => TextNode {
            id,
            text_role: Some(TextRole::MaterializedPrefix(kind)),
            text,
            marks: t.marks.clone(),
            style_props: t.style_props.clone(),
            rpr_authored: t.rpr_authored,
            formatting_change: None,
        },
        None => TextNode {
            id,
            text_role: Some(TextRole::MaterializedPrefix(kind)),
            text,
            marks: Vec::new(),
            style_props: StyleProps::default(),
            rpr_authored: RunRprAuthored::default(),
            formatting_change: None,
        },
    }
}

fn apply_block_modified(
    blocks: &mut [TrackedBlock],
    block_id_to_modify: &NodeId,
    inline_changes: &[InlineChange],
    new_block: &BlockNode,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    context: &str,
) -> Result<(), MergeError> {
    let Some(idx) = find_block_index(blocks, block_id_to_modify) else {
        return Err(MergeError {
            message: "block_id for modification not found in base model".to_string(),
            context: format!("{context}:{}", block_id_to_modify.0),
        });
    };

    // Collect opaque nodes and original segments before taking the mutable borrow on paragraph
    let base_opaques = collect_opaques(&blocks[idx].block);
    let target_opaques = collect_opaques(new_block);
    let original_segments: Vec<TrackedSegment> = match &blocks[idx].block {
        BlockNode::Paragraph(p) => p.segments.clone(),
        _ => Vec::new(),
    };

    let tb = &mut blocks[idx];
    let paragraph = match &mut tb.block {
        BlockNode::Paragraph(p) => p,
        _ => {
            return Err(MergeError {
                message: "BlockModified references non-paragraph block".to_string(),
                context: format!("{context}:{}", block_id_to_modify.0),
            });
        }
    };

    let mut new_segments = inline_changes_to_segments_with_opaques(
        &paragraph.id,
        inline_changes,
        revision,
        rev_counter,
        &base_opaques,
        &target_opaques,
    )?;

    // Save original numbering before prefix handling may clear it — we need
    // the original value for formatting change comparison below.
    let original_numbering = paragraph.numbering.clone();
    let original_has_prefix = paragraph.numbering.is_some() || paragraph.literal_prefix.is_some();
    let mut prefix_was_materialized = false;

    // Prefix materialization: only needed when `literal_prefix` (baked text)
    // is involved.  When both sides have structural numbering (w:numPr), the
    // prefix is generated by Word from the numbering definition — no inline
    // text to track.  Counter drift (same numId/ilvl, different counter value)
    // is handled implicitly by list reordering.
    let new_para = match new_block {
        BlockNode::Paragraph(p) => Some(p),
        _ => None,
    };
    if let Some(new_para) = new_para {
        // Prefix materialization is only needed when paragraph properties
        // alone cannot preserve the visible prefix through redline export:
        // literal prefixes on either side, or a source-side structural
        // numbering prefix that disappears on the target side. When both
        // sides retain structural numbering, Word can synthesize the counter
        // from numPr. When the target only adds structural numbering, let
        // pPrChange record it so the numbering counter stays structural.
        if let Some(plan) = plan_prefix_materialization(paragraph, new_para) {
            let mut prefix_segments = Vec::new();
            if plan.emit_deleted_prefix
                && let Some(old_p) = &plan.old_prefix
                && let Some(kind) = plan.deleted_kind
            {
                prefix_segments.push(TrackedSegment {
                    status: TrackingStatus::Deleted(next_revision(revision, rev_counter)),
                    inlines: vec![InlineNode::from(make_prefix_text_node(
                        materialized_prefix_node_id(&paragraph.id, kind),
                        kind,
                        materialized_prefix_text(old_p, paragraph),
                        paragraph,
                    ))],
                });
            }
            if plan.emit_inserted_prefix
                && let Some(new_p) = &plan.new_prefix
                && let Some(kind) = plan.inserted_kind
            {
                prefix_segments.push(TrackedSegment {
                    status: TrackingStatus::Inserted(next_revision(revision, rev_counter)),
                    inlines: vec![InlineNode::from(make_prefix_text_node(
                        materialized_prefix_node_id(&paragraph.id, kind),
                        kind,
                        materialized_prefix_text(new_p, new_para),
                        new_para,
                    ))],
                });
            }
            // INVARIANT: prefix segments are always first in the segment list.
            // `changelet::is_prefix_segment` depends on this ordering to
            // filter prefix segments during tracked atom extraction.
            prefix_segments.append(&mut new_segments);
            new_segments = prefix_segments;

            paragraph.literal_prefix = None;
            sync_literal_prefix_geometry(new_para, paragraph);
            if plan.target_uses_structural_numbering {
                // Keep the target's structural numbering in pPr. The old baked
                // prefix still needs to be visible as deleted text, but
                // materializing the new prefix would degrade accept-all back
                // into literal text instead of the target numPr.
                paragraph.materialized_numbering = None;
            } else {
                // Clear metadata to prevent the serializer from double-emitting
                // the prefix. Save materialized_numbering so accept_all can
                // restore structural numbering when the new prefix was
                // synthesized from numbering.
                paragraph.materialized_numbering = new_para.numbering.clone();
                paragraph.numbering = None;
                prefix_was_materialized = true;
            }
        }
    }

    // Detect paragraph formatting changes (pPrChange): compare ALL formatting
    // properties between the base and target paragraphs. Per §17.13.5.29 the
    // snapshot must be COMPLETE, so we capture every property the old paragraph had.
    // Use `original_numbering` for comparison because the prefix handling above
    // may have cleared `paragraph.numbering`.
    // Numbering comparison is structural (num_id + ilvl only) — counter drift
    // from list reordering is not a formatting change.
    if let Some(new_para) = new_para {
        let align_changed = paragraph.align != new_para.align;
        let indent_changed = paragraph.indent != new_para.indent;
        let spacing_changed = paragraph.spacing != new_para.spacing;
        let numbering_changed =
            !crate::domain::numbering_structurally_eq(&original_numbering, &new_para.numbering);
        let style_changed = paragraph.style_id != new_para.style_id;
        let keep_next_changed = paragraph.keep_next != new_para.keep_next;
        let keep_lines_changed = paragraph.keep_lines != new_para.keep_lines;
        let page_break_before_changed = paragraph.page_break_before != new_para.page_break_before;
        let widow_control_changed = paragraph.widow_control != new_para.widow_control;
        let contextual_spacing_changed =
            paragraph.contextual_spacing != new_para.contextual_spacing;
        let shading_changed = paragraph.shading != new_para.shading;
        let borders_changed = paragraph.borders != new_para.borders;
        let tab_stops_changed = paragraph.tab_stops != new_para.tab_stops;
        let text_direction_changed = paragraph.text_direction != new_para.text_direction;
        let text_alignment_changed = paragraph.text_alignment != new_para.text_alignment;
        let mirror_indents_changed = paragraph.mirror_indents != new_para.mirror_indents;
        let bidi_changed = paragraph.bidi != new_para.bidi;
        let suppress_auto_hyphens_changed =
            paragraph.suppress_auto_hyphens != new_para.suppress_auto_hyphens;
        let snap_to_grid_changed = paragraph.snap_to_grid != new_para.snap_to_grid;
        let overflow_punct_changed = paragraph.overflow_punct != new_para.overflow_punct;
        let adjust_right_ind_changed = paragraph.adjust_right_ind != new_para.adjust_right_ind;
        let word_wrap_changed = paragraph.word_wrap != new_para.word_wrap;
        let frame_pr_changed = paragraph.frame_pr != new_para.frame_pr;
        let section_properties_changed =
            paragraph.section_properties != new_para.section_properties;
        let cnf_style_changed = paragraph.cnf_style != new_para.cnf_style;
        let heading_level_changed = paragraph.heading_level != new_para.heading_level;

        let any_changed = align_changed
            || indent_changed
            || spacing_changed
            || numbering_changed
            || style_changed
            || keep_next_changed
            || keep_lines_changed
            || page_break_before_changed
            || widow_control_changed
            || contextual_spacing_changed
            || shading_changed
            || borders_changed
            || tab_stops_changed
            || text_direction_changed
            || text_alignment_changed
            || mirror_indents_changed
            || bidi_changed
            || suppress_auto_hyphens_changed
            || snap_to_grid_changed
            || overflow_punct_changed
            || adjust_right_ind_changed
            || word_wrap_changed
            || frame_pr_changed
            || section_properties_changed
            || cnf_style_changed
            || heading_level_changed;

        if any_changed {
            // Signal "base had no numbering at all" when the base paragraph had
            // neither structural numbering nor a literal prefix, and numbering is
            // being added. The serializer emits numId=0 in pPrChange so the
            // extraction can distinguish this from "base had a literal prefix
            // replaced by structural numbering" (where the current numPr should
            // still be counted in the reject view).
            let numbering_explicitly_absent = original_numbering.is_none()
                && new_para.numbering.is_some()
                && !original_has_prefix;
            paragraph.formatting_change = Some(ParagraphFormattingChange {
                revision_id: revision.revision_id,
                identity: 0,
                previous_alignment: paragraph.align.clone(),
                // Snapshot AUTHORED-direct indent/spacing (previous DIRECT pPr),
                // not resolved effective — see snapshot_paragraph_formatting.
                previous_indentation: paragraph
                    .authored_indent
                    .clone()
                    .or_else(|| paragraph.indent.clone()),
                previous_spacing: paragraph
                    .authored_spacing
                    .clone()
                    .or_else(|| paragraph.spacing.clone()),
                previous_numbering: original_numbering.clone(),
                previous_numbering_explicitly_absent: numbering_explicitly_absent,
                previous_style_id: paragraph.style_id.clone(),
                previous_keep_next: paragraph.keep_next,
                previous_keep_lines: paragraph.keep_lines,
                previous_page_break_before: paragraph.page_break_before,
                previous_widow_control: paragraph.widow_control,
                previous_contextual_spacing: paragraph.contextual_spacing,
                previous_shading: paragraph.shading.clone(),
                previous_borders: paragraph.borders.clone(),
                previous_tab_stops: paragraph.tab_stops.clone(),
                previous_literal_prefix_leading_tab_twips: paragraph
                    .literal_prefix_leading_tab_twips,
                previous_literal_prefix_trailing_tab_stop_twips: paragraph
                    .literal_prefix_trailing_tab_stop_twips,
                previous_paragraph_mark_marks: paragraph.paragraph_mark_marks.clone(),
                previous_paragraph_mark_style_props: paragraph.paragraph_mark_style_props.clone(),
                previous_paragraph_mark_rpr_off: paragraph.paragraph_mark_rpr_off,
                previous_text_direction: paragraph.text_direction.clone(),
                previous_text_alignment: paragraph.text_alignment.clone(),
                previous_mirror_indents: paragraph.mirror_indents,
                previous_auto_space_de: paragraph.auto_space_de,
                previous_auto_space_dn: paragraph.auto_space_dn,
                previous_bidi: paragraph.bidi,
                previous_suppress_auto_hyphens: paragraph.suppress_auto_hyphens,
                previous_snap_to_grid: paragraph.snap_to_grid,
                previous_overflow_punct: paragraph.overflow_punct,
                previous_adjust_right_ind: paragraph.adjust_right_ind,
                previous_word_wrap: paragraph.word_wrap,
                previous_frame_pr: paragraph.frame_pr.clone(),
                previous_preserved_ppr: paragraph.preserved_ppr.clone(),
                author: revision.author.clone().unwrap_or_default(),
                date: revision.date.clone(),
            });

            // Update paragraph properties to new values
            paragraph.align = new_para.align.clone();
            paragraph.has_direct_align = new_para.has_direct_align;
            paragraph.indent = new_para.indent.clone();
            paragraph.has_direct_indent = new_para.has_direct_indent;
            paragraph.authored_indent = new_para.authored_indent.clone();
            paragraph.spacing = new_para.spacing.clone();
            paragraph.has_direct_spacing = new_para.has_direct_spacing;
            paragraph.authored_spacing = new_para.authored_spacing.clone();
            paragraph.style_id = new_para.style_id.clone();
            paragraph.keep_next = new_para.keep_next;
            paragraph.keep_lines = new_para.keep_lines;
            paragraph.page_break_before = new_para.page_break_before;
            paragraph.widow_control = new_para.widow_control;
            paragraph.contextual_spacing = new_para.contextual_spacing;
            paragraph.shading = new_para.shading.clone();
            paragraph.borders = new_para.borders.clone();
            paragraph.has_direct_keep_next = new_para.has_direct_keep_next;
            paragraph.has_direct_keep_lines = new_para.has_direct_keep_lines;
            paragraph.has_direct_page_break_before = new_para.has_direct_page_break_before;
            paragraph.has_direct_widow_control = new_para.has_direct_widow_control;
            paragraph.has_direct_contextual_spacing = new_para.has_direct_contextual_spacing;
            paragraph.has_direct_shading = new_para.has_direct_shading;
            paragraph.has_direct_borders = new_para.has_direct_borders;
            paragraph.tab_stops = new_para.tab_stops.clone();
            paragraph.effective_tab_stops_rel = new_para.effective_tab_stops_rel.clone();
            paragraph.text_direction = new_para.text_direction.clone();
            paragraph.text_alignment = new_para.text_alignment.clone();
            paragraph.mirror_indents = new_para.mirror_indents;
            paragraph.bidi = new_para.bidi;
            paragraph.suppress_auto_hyphens = new_para.suppress_auto_hyphens;
            paragraph.snap_to_grid = new_para.snap_to_grid;
            paragraph.overflow_punct = new_para.overflow_punct;
            paragraph.adjust_right_ind = new_para.adjust_right_ind;
            paragraph.word_wrap = new_para.word_wrap;
            paragraph.frame_pr = new_para.frame_pr.clone();
            paragraph.cnf_style = new_para.cnf_style.clone();
            paragraph.heading_level = new_para.heading_level.clone();
            paragraph.section_property_change = new_para.section_property_change.clone();
            paragraph.section_properties = new_para.section_properties.clone();
            paragraph.paragraph_mark_marks = new_para.paragraph_mark_marks.clone();
            paragraph.paragraph_mark_style_props = new_para.paragraph_mark_style_props.clone();
            paragraph.paragraph_mark_rpr_off = new_para.paragraph_mark_rpr_off;
            sync_literal_prefix_geometry(new_para, paragraph);
            // Only update numbering when the prefix was not already materialized
            // as tracked inline content — otherwise we'd re-introduce the numPr.
            if numbering_changed && !prefix_was_materialized {
                paragraph.numbering = new_para.numbering.clone();
                // Carry the target's numbering PROVENANCE so the emission gate
                // matches the new numbering (a target that authored a direct
                // numPr emits one; inherited numbering does not).
                paragraph.has_direct_numbering = new_para.has_direct_numbering;
                // Clear literal_prefix when gaining structural numbering to
                // prevent the serializer from writing both a text run for the
                // literal prefix AND a numPr in pPr.
                if new_para.numbering.is_some() {
                    paragraph.literal_prefix = None;
                    paragraph.literal_prefix_leading_tab_twips = None;
                    paragraph.literal_prefix_leading_tab_count = 0;
                    paragraph.literal_prefix_has_trailing_tab = false;
                    paragraph.literal_prefix_trailing_tab_stop_twips = None;
                }
            }
        }
    }

    // Sync literal_prefix to target value. literal_prefix is a model
    // property (prefix text stripped from inline content during import),
    // not a tracked formatting property. When the diff detects a prefix
    // change, the inline diff handles the text change; literal_prefix
    // must match the target so accept_all produces the target state.
    if !prefix_was_materialized && let BlockNode::Paragraph(new_p) = new_block {
        paragraph.literal_prefix = new_p.literal_prefix.clone();
        sync_literal_prefix_geometry(new_p, paragraph);
        paragraph.paragraph_mark_marks = new_p.paragraph_mark_marks.clone();
        paragraph.paragraph_mark_style_props = new_p.paragraph_mark_style_props.clone();
        paragraph.paragraph_mark_rpr_off = new_p.paragraph_mark_rpr_off;
    }

    inject_structural_markers_at_offsets(
        &mut new_segments,
        &original_segments,
        new_para.map(|p| p.segments.as_slice()),
    );

    prune_empty_text_inlines(&mut new_segments);
    paragraph.segments = new_segments;
    Ok(())
}

/// Record a cell formatting change (tcPrChange) on a cell when the new
/// formatting differs. Captures the previous formatting as a snapshot, then
/// updates the cell to the new formatting.
fn apply_cell_formatting_change(
    cell: &mut TableCellNode,
    new_formatting: &CellFormatting,
    revision: &RevisionInfo,
) {
    if cell.formatting != *new_formatting {
        cell.formatting_change = Some(CellFormattingChange {
            revision_id: revision.revision_id,
            identity: 0,
            previous_width: cell.formatting.width.clone(),
            previous_borders: cell.formatting.borders.clone(),
            previous_shading: cell.formatting.shading.clone(),
            previous_v_align: cell.formatting.v_align.clone(),
            previous_margins: cell.formatting.margins.clone(),
            previous_no_wrap: cell.formatting.no_wrap,
            previous_text_direction: cell.formatting.text_direction.clone(),
            previous_tc_fit_text: cell.formatting.tc_fit_text,
            author: revision.author.clone().unwrap_or_default(),
            date: revision.date.clone(),
        });
        cell.formatting = new_formatting.clone();
    }
}

/// Apply per-cell inline changes to a table in-place.
///
/// The table stays as a single `TrackingStatus::Normal` block. For each
/// changed cell, the affected paragraphs get their segments replaced with
/// tracked inline changes (same logic as `apply_block_modified`).
fn apply_table_cells_modified(
    blocks: &mut [TrackedBlock],
    table_id: &NodeId,
    cell_changes: &[crate::domain::TableCellChange],
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    context: &str,
) -> Result<(), MergeError> {
    let idx = find_block_index(blocks, table_id).ok_or_else(|| MergeError {
        message: "table_id for cell modification not found in base model".to_string(),
        context: format!("{context}:{}", table_id.0),
    })?;
    let table = match &mut blocks[idx].block {
        BlockNode::Table(t) => t,
        _ => {
            return Err(MergeError {
                message: "TableCellsModified references non-table block".to_string(),
                context: format!("{context}:{}", table_id.0),
            });
        }
    };

    for cell_change in cell_changes {
        let n_rows = table.rows.len();
        let row = table
            .rows
            .get_mut(cell_change.row_index)
            .ok_or_else(|| MergeError {
                message: format!(
                    "row index {} out of bounds (table has {n_rows} rows)",
                    cell_change.row_index,
                ),
                context: format!("{context}:{}:row{}", table_id.0, cell_change.row_index),
            })?;
        let n_cells = row.cells.len();
        let cell = row
            .cells
            .get_mut(cell_change.cell_index)
            .ok_or_else(|| MergeError {
                message: format!(
                    "cell index {} out of bounds (row has {n_cells} cells)",
                    cell_change.cell_index,
                ),
                context: format!(
                    "{context}:{}:row{}:cell{}",
                    table_id.0, cell_change.row_index, cell_change.cell_index
                ),
            })?;

        // Apply cell formatting change (tcPrChange) if formatting differs.
        if let Some(new_formatting) = &cell_change.new_cell_formatting {
            apply_cell_formatting_change(cell, new_formatting, revision);
        }

        for para_change in &cell_change.paragraph_changes {
            let n_blocks = cell.blocks.len();
            let block = cell
                .blocks
                .get_mut(para_change.block_index)
                .ok_or_else(|| MergeError {
                    message: format!(
                        "block index {} out of bounds (cell has {n_blocks} blocks)",
                        para_change.block_index,
                    ),
                    context: format!(
                        "{context}:{}:row{}:cell{}:block{}",
                        table_id.0,
                        cell_change.row_index,
                        cell_change.cell_index,
                        para_change.block_index
                    ),
                })?;

            let old_block = block.clone();
            let paragraph = match block {
                BlockNode::Paragraph(p) => p,
                _ => {
                    return Err(MergeError {
                        message:
                            "TableCellsModified paragraph change references non-paragraph block"
                                .to_string(),
                        context: format!(
                            "{context}:{}:row{}:cell{}:block{}",
                            table_id.0,
                            cell_change.row_index,
                            cell_change.cell_index,
                            para_change.block_index
                        ),
                    });
                }
            };
            let new_para = match &para_change.new_block {
                BlockNode::Paragraph(p) => p,
                _ => unreachable!("validated paragraph target block"),
            };
            apply_paragraph_diff_in_cell(
                paragraph,
                new_para,
                &old_block,
                &para_change.new_block,
                revision,
                rev_counter,
                context,
            )?;
        }

        // Apply nested table diffs within this cell.
        for nested_diff in &cell_change.nested_table_diffs {
            let n_blocks = cell.blocks.len();
            let block = cell
                .blocks
                .get_mut(nested_diff.block_index)
                .ok_or_else(|| MergeError {
                    message: format!(
                        "nested table block index {} out of bounds (cell has {n_blocks} blocks)",
                        nested_diff.block_index,
                    ),
                    context: format!(
                        "{context}:{}:row{}:cell{}:nested_tbl{}",
                        table_id.0,
                        cell_change.row_index,
                        cell_change.cell_index,
                        nested_diff.block_index
                    ),
                })?;

            let inner_table = match block {
                BlockNode::Table(t) => t,
                _ => {
                    return Err(MergeError {
                        message: "nested table diff references non-table block".to_string(),
                        context: format!(
                            "{context}:{}:row{}:cell{}:block{}",
                            table_id.0,
                            cell_change.row_index,
                            cell_change.cell_index,
                            nested_diff.block_index
                        ),
                    });
                }
            };

            match &nested_diff.diff {
                NestedTableDiffKind::StructureChanged {
                    table_diff,
                    new_table,
                } => {
                    apply_nested_table_structure_changed(
                        inner_table,
                        new_table,
                        table_diff,
                        revision,
                        rev_counter,
                        &format!(
                            "{context}:{}:row{}:cell{}:nested_tbl{}",
                            table_id.0,
                            cell_change.row_index,
                            cell_change.cell_index,
                            nested_diff.block_index
                        ),
                    )?;
                }
                NestedTableDiffKind::CellsModified { cell_changes } => {
                    apply_nested_table_cells_modified(
                        inner_table,
                        cell_changes,
                        revision,
                        rev_counter,
                        &format!(
                            "{context}:{}:row{}:cell{}:nested_tbl{}",
                            table_id.0,
                            cell_change.row_index,
                            cell_change.cell_index,
                            nested_diff.block_index
                        ),
                    )?;
                }
            }
        }
    }

    Ok(())
}

/// Apply per-cell inline changes to a nested table in-place.
///
/// Same logic as `apply_table_cells_modified` but operates directly on a
/// `&mut TableNode` instead of looking up a table by ID in the block list.
fn apply_nested_table_cells_modified(
    table: &mut TableNode,
    cell_changes: &[crate::domain::TableCellChange],
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    context: &str,
) -> Result<(), MergeError> {
    for cell_change in cell_changes {
        let n_rows = table.rows.len();
        let row = table
            .rows
            .get_mut(cell_change.row_index)
            .ok_or_else(|| MergeError {
                message: format!(
                    "row index {} out of bounds (nested table has {n_rows} rows)",
                    cell_change.row_index,
                ),
                context: format!("{context}:row{}", cell_change.row_index),
            })?;
        let n_cells = row.cells.len();
        let cell = row
            .cells
            .get_mut(cell_change.cell_index)
            .ok_or_else(|| MergeError {
                message: format!(
                    "cell index {} out of bounds (row has {n_cells} cells)",
                    cell_change.cell_index,
                ),
                context: format!(
                    "{context}:row{}:cell{}",
                    cell_change.row_index, cell_change.cell_index
                ),
            })?;

        // Apply cell formatting change (tcPrChange) if formatting differs.
        if let Some(new_formatting) = &cell_change.new_cell_formatting {
            apply_cell_formatting_change(cell, new_formatting, revision);
        }

        for para_change in &cell_change.paragraph_changes {
            let n_blocks = cell.blocks.len();
            let block = cell
                .blocks
                .get_mut(para_change.block_index)
                .ok_or_else(|| MergeError {
                    message: format!(
                        "block index {} out of bounds (cell has {n_blocks} blocks)",
                        para_change.block_index,
                    ),
                    context: format!(
                        "{context}:row{}:cell{}:block{}",
                        cell_change.row_index, cell_change.cell_index, para_change.block_index
                    ),
                })?;

            let base_opaques = collect_opaques(block);
            let target_opaques = collect_opaques(&para_change.new_block);

            let paragraph = match block {
                BlockNode::Paragraph(p) => p,
                _ => {
                    return Err(MergeError {
                        message:
                            "nested table cell paragraph change references non-paragraph block"
                                .to_string(),
                        context: format!(
                            "{context}:row{}:cell{}:block{}",
                            cell_change.row_index, cell_change.cell_index, para_change.block_index
                        ),
                    });
                }
            };

            let new_segments = inline_changes_to_segments_with_opaques(
                &paragraph.id,
                &para_change.inline_changes,
                revision,
                rev_counter,
                &base_opaques,
                &target_opaques,
            )?;
            let mut new_segments = new_segments;
            prune_empty_text_inlines(&mut new_segments);
            paragraph.segments = new_segments;
        }

        // Recurse into nested tables within this cell.
        for nested_diff in &cell_change.nested_table_diffs {
            let n_blocks = cell.blocks.len();
            let block = cell
                .blocks
                .get_mut(nested_diff.block_index)
                .ok_or_else(|| MergeError {
                    message: format!(
                        "nested table block index {} out of bounds (cell has {n_blocks} blocks)",
                        nested_diff.block_index,
                    ),
                    context: format!(
                        "{context}:row{}:cell{}:nested_tbl{}",
                        cell_change.row_index, cell_change.cell_index, nested_diff.block_index
                    ),
                })?;

            let inner_table = match block {
                BlockNode::Table(t) => t,
                _ => {
                    return Err(MergeError {
                        message: "nested table diff references non-table block".to_string(),
                        context: format!(
                            "{context}:row{}:cell{}:block{}",
                            cell_change.row_index, cell_change.cell_index, nested_diff.block_index
                        ),
                    });
                }
            };

            match &nested_diff.diff {
                NestedTableDiffKind::StructureChanged {
                    table_diff,
                    new_table,
                } => {
                    apply_nested_table_structure_changed(
                        inner_table,
                        new_table,
                        table_diff,
                        revision,
                        rev_counter,
                        &format!(
                            "{context}:row{}:cell{}:nested_tbl{}",
                            cell_change.row_index, cell_change.cell_index, nested_diff.block_index
                        ),
                    )?;
                }
                NestedTableDiffKind::CellsModified {
                    cell_changes: nested_changes,
                } => {
                    apply_nested_table_cells_modified(
                        inner_table,
                        nested_changes,
                        revision,
                        rev_counter,
                        &format!(
                            "{context}:row{}:cell{}:nested_tbl{}",
                            cell_change.row_index, cell_change.cell_index, nested_diff.block_index
                        ),
                    )?;
                }
            }
        }
    }
    Ok(())
}

/// Apply row-level tracked changes for a nested table structure change.
///
/// Same logic as `apply_table_structure_changed` but operates directly on a
/// `&mut TableNode` instead of looking up by ID in the block list.
fn apply_nested_table_structure_changed(
    table: &mut TableNode,
    target_table: &TableNode,
    diff: &TableDiffResult,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    context: &str,
) -> Result<(), MergeError> {
    let base_table = table.clone();

    let mut merged_rows = Vec::new();
    for alignment in &diff.row_alignment {
        match alignment {
            TableRowAlignment::Deleted { old_row } => {
                // Whole-row deletion — row marker + cell content only, no
                // per-cell `w:cellDel` (see `mark_whole_row_deleted`).
                let mut row = base_table.rows[*old_row].clone();
                mark_whole_row_deleted(&mut row, revision, rev_counter);
                merged_rows.push(row);
            }
            TableRowAlignment::Inserted { new_row } => {
                // Whole-row insertion — row marker only (see
                // `mark_whole_row_inserted`).
                let mut row = target_table.rows[*new_row].clone();
                mark_whole_row_inserted(&mut row, revision, rev_counter);
                merged_rows.push(row);
            }
            TableRowAlignment::Matched { old_row, new_row } => {
                let mut row = base_table.rows[*old_row].clone();
                let new_row_ref = &target_table.rows[*new_row];
                let max_cells = row.cells.len().max(new_row_ref.cells.len());
                let mut merged_cells = Vec::new();

                for cell_idx in 0..max_cells {
                    if cell_idx < row.cells.len() && cell_idx < new_row_ref.cells.len() {
                        let mut cell = row.cells[cell_idx].clone();
                        let new_cell_ref = &new_row_ref.cells[cell_idx];
                        // Adopt the target cell's structural merge attributes
                        // (see apply_table_structure_changed for rationale):
                        // gridSpan/vMerge are not tracked-change axes, so the
                        // accepted result must match the target's grid shape,
                        // otherwise a dropped restart anchor leaves orphan
                        // <w:vMerge/> continue cells.
                        cell.grid_span = new_cell_ref.grid_span;
                        cell.v_merge = new_cell_ref.v_merge.clone();
                        apply_cell_formatting_change(&mut cell, &new_cell_ref.formatting, revision);
                        reconcile_cell_blocks(
                            &mut cell,
                            new_cell_ref,
                            revision,
                            rev_counter,
                            context,
                        )?;
                        merged_cells.push(cell);
                    } else if cell_idx < row.cells.len() {
                        let mut cell = row.cells[cell_idx].clone();
                        cell.tracking_status = Some(TrackingStatus::Deleted(next_revision(
                            revision,
                            rev_counter,
                        )));
                        mark_cell_content_deleted(&mut cell, revision, rev_counter);
                        merged_cells.push(cell);
                    } else {
                        let mut cell = new_row_ref.cells[cell_idx].clone();
                        cell.tracking_status = Some(TrackingStatus::Inserted(next_revision(
                            revision,
                            rev_counter,
                        )));
                        merged_cells.push(cell);
                    }
                }

                row.cells = merged_cells;
                merged_rows.push(row);
            }
        }
    }

    table.rows = merged_rows;
    table.structure_hash = target_table.structure_hash.clone();
    Ok(())
}

fn apply_changes_to_blocks(
    blocks: &mut Vec<TrackedBlock>,
    changes: &[DiffChange],
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    context: &str,
    target_tables_by_id: Option<&mut HashMap<String, BlockNode>>,
    provenance: &mut BlockProvenanceMap,
) -> Result<(), MergeError> {
    let mut insert_order = InsertOrderState::default();
    let mut target_tables_by_id = target_tables_by_id;
    for change in changes {
        match change {
            DiffChange::BlockDeleted {
                block_id, move_id, ..
            } => {
                apply_block_deleted(blocks, block_id, revision, rev_counter, context)?;
                provenance.insert_deleted(block_id.clone(), block_id.clone());
                if let Some(mid) = move_id
                    && let Some(idx) = find_block_index(blocks, block_id)
                {
                    blocks[idx].move_id = Some(mid.clone());
                }
            }
            DiffChange::BlockInserted {
                after_block_id,
                block,
                move_id,
            } => {
                let original_target_id = block_id(block).clone();
                apply_block_inserted(
                    blocks,
                    after_block_id,
                    block,
                    revision,
                    rev_counter,
                    &mut insert_order,
                    context,
                )?;
                // The merged block may have been renamed (original_id → original_id__insN).
                // Find its actual merged ID by scanning backwards for the just-inserted block.
                let merged_id = blocks
                    .iter()
                    .rev()
                    .find(|tb| {
                        matches!(&tb.status, TrackingStatus::Inserted(_))
                            && block_id(&tb.block).0.starts_with(&*original_target_id.0)
                    })
                    .map(|tb| block_id(&tb.block).clone())
                    .unwrap_or_else(|| original_target_id.clone());
                provenance.insert_inserted(merged_id, original_target_id);
                if let Some(mid) = move_id {
                    for tb in blocks.iter_mut().rev() {
                        if matches!(&tb.status, TrackingStatus::Inserted(_))
                            && tb.move_id.is_none()
                            && block_id(&tb.block).0.starts_with(&*block_id(block).0)
                        {
                            tb.move_id = Some(mid.clone());
                            break;
                        }
                    }
                }
            }
            DiffChange::BlockModified {
                block_id,
                inline_changes,
                new_block,
                para_split,
                ..
            } => {
                let target_id = match new_block {
                    BlockNode::Paragraph(p) => p.id.clone(),
                    BlockNode::Table(t) => t.id.clone(),
                    BlockNode::OpaqueBlock(o) => o.id.clone(),
                };
                apply_block_modified(
                    blocks,
                    block_id,
                    inline_changes,
                    new_block,
                    revision,
                    rev_counter,
                    context,
                )?;
                provenance.insert_modified(block_id.clone(), block_id.clone(), target_id);
                if *para_split
                    && let Some(idx) = find_block_index(blocks, block_id)
                    && let BlockNode::Paragraph(p) = &mut blocks[idx].block
                {
                    p.para_split = true;
                    p.para_mark_status = Some(TrackingStatus::Inserted(next_revision(
                        revision,
                        rev_counter,
                    )));
                }
            }
            DiffChange::TableStructureChanged {
                table_id: base_table_id,
                target_table_id,
                table_diff,
                ..
            } => {
                provenance.insert_modified(
                    base_table_id.clone(),
                    base_table_id.clone(),
                    target_table_id.clone(),
                );

                let Some(target_tables_by_id) = target_tables_by_id.as_deref_mut() else {
                    return Err(MergeError {
                        message: "table structure merge requires target table lookup".to_string(),
                        context: format!("{context}:{}", base_table_id.0),
                    });
                };

                let inserted_table =
                    target_tables_by_id
                        .remove(&*target_table_id.0)
                        .ok_or_else(|| MergeError {
                            message: format!(
                                "target table for structure change not found (base={}, target={})",
                                base_table_id.0, target_table_id.0
                            ),
                            context: format!("{context}:{}", base_table_id.0),
                        })?;

                let target_table = match &inserted_table {
                    BlockNode::Table(t) => Some(t),
                    _ => None,
                };

                if let (Some(diff), Some(target_tbl)) = (table_diff, target_table) {
                    apply_table_structure_changed(
                        blocks,
                        base_table_id,
                        target_tbl,
                        diff,
                        revision,
                        rev_counter,
                        context,
                    )?;
                } else {
                    apply_block_deleted(blocks, base_table_id, revision, rev_counter, context)?;
                    apply_block_inserted(
                        blocks,
                        &Some(base_table_id.clone()),
                        &inserted_table,
                        revision,
                        rev_counter,
                        &mut insert_order,
                        context,
                    )?;
                }
            }
            DiffChange::TableCellsModified {
                table_id,
                target_table_id,
                cell_changes,
                ..
            } => {
                provenance.insert_modified(
                    table_id.clone(),
                    table_id.clone(),
                    target_table_id.clone(),
                );
                apply_table_cells_modified(
                    blocks,
                    table_id,
                    cell_changes,
                    revision,
                    rev_counter,
                    context,
                )?;
            }
            // Story-level diff changes never reach this match: this function
            // only merges a body/cell block list. Header/footer stories are
            // applied by `apply_story_changes`; footnote/endnote/comment
            // stories by `apply_note_changes`. Enumerated explicitly (not
            // `_`) so a future `DiffChange` variant fails to compile here
            // instead of silently no-op'ing.
            DiffChange::HeaderModified { .. }
            | DiffChange::HeaderDeleted { .. }
            | DiffChange::HeaderInserted { .. }
            | DiffChange::FooterModified { .. }
            | DiffChange::FooterDeleted { .. }
            | DiffChange::FooterInserted { .. }
            | DiffChange::FootnoteModified { .. }
            | DiffChange::FootnoteDeleted { .. }
            | DiffChange::FootnoteInserted { .. }
            | DiffChange::EndnoteModified { .. }
            | DiffChange::EndnoteDeleted { .. }
            | DiffChange::EndnoteInserted { .. }
            | DiffChange::CommentModified { .. }
            | DiffChange::CommentDeleted { .. }
            | DiffChange::CommentInserted { .. } => {}
        }
    }
    annotate_paragraph_mark_status(blocks, revision, rev_counter);
    Ok(())
}

/// Annotate `para_mark_status` on the last Normal paragraph when all blocks
/// after it to the end of the list are non-Normal (Inserted or Deleted).
///
/// OOXML rule: a paragraph's mark status reflects what happens to the boundary
/// between it and its successor. When a Normal paragraph is the last Normal
/// block and only tracked-change blocks follow it to the end, Word marks its ¶
/// with the status of the immediately following block.
///
/// Only applies to `BlockNode::Paragraph` — tables and opaque blocks are skipped.
fn annotate_paragraph_mark_status(
    blocks: &mut [TrackedBlock],
    revision: &RevisionInfo,
    rev_counter: &mut u32,
) {
    fn paragraph_has_visible_content(paragraph: &ParagraphNode) -> bool {
        paragraph
            .literal_prefix
            .as_deref()
            .is_some_and(|prefix| !prefix.is_empty())
            || !extract_inlines_text(&paragraph.all_inlines_owned()).is_empty()
    }

    fn is_empty_trailing_paragraph(block: &TrackedBlock, inserted: bool) -> bool {
        let status_matches = if inserted {
            matches!(block.status, TrackingStatus::Inserted(_))
        } else {
            matches!(block.status, TrackingStatus::Deleted(_))
        };
        if !status_matches {
            return false;
        }
        match &block.block {
            BlockNode::Paragraph(paragraph) => !paragraph_has_visible_content(paragraph),
            _ => false,
        }
    }

    // Scan backwards to find the last Normal paragraph.
    // "Normal" at the block level means TrackedBlock.status is Normal
    // (Modified paragraphs also have Normal block status).
    let mut last_normal_para_idx: Option<usize> = None;
    for i in (0..blocks.len()).rev() {
        if matches!(blocks[i].status, TrackingStatus::Normal) {
            if matches!(blocks[i].block, BlockNode::Paragraph(_)) {
                last_normal_para_idx = Some(i);
            }
            // Whether it's a paragraph, table, or opaque — we found a Normal
            // block, so all earlier Normal blocks have a Normal block after them.
            break;
        }
    }

    let Some(idx) = last_normal_para_idx else {
        return;
    };

    // The next block must exist and be non-Normal.
    let next_idx = idx + 1;
    if next_idx >= blocks.len() {
        return;
    }

    let is_inserted = matches!(blocks[next_idx].status, TrackingStatus::Inserted(_));
    let is_deleted = matches!(blocks[next_idx].status, TrackingStatus::Deleted(_));
    if !is_inserted && !is_deleted {
        return;
    }

    // A trailing run of empty inserted/deleted paragraphs at story end should
    // disappear independently on reject/accept. They do not imply that the
    // preceding surviving paragraph's mark changed.
    if blocks[next_idx..]
        .iter()
        .all(|block| is_empty_trailing_paragraph(block, is_inserted))
    {
        return;
    }

    if let BlockNode::Paragraph(p) = &mut blocks[idx].block {
        // Don't overwrite an existing annotation (e.g., from apply_block_deleted
        // or the para_split path which sets its own mark).
        if p.para_mark_status.is_none() {
            let mark = if is_inserted {
                TrackingStatus::Inserted(next_revision(revision, rev_counter))
            } else {
                TrackingStatus::Deleted(next_revision(revision, rev_counter))
            };
            p.para_mark_status = Some(mark);
        }
    }
}

/// Enforce the invariant that the DOCUMENT-FINAL paragraph mark never carries a
/// tracked mark insertion or deletion.
///
/// Word cannot resolve a revision on the document-final paragraph mark: accept
/// of a paragraph-mark *insertion* merges the paragraph with the FOLLOWING one,
/// and the final mark has no follower, so accept-all leaves the revision pending
/// forever (the reviewer sees "accept all changes" fail to clear the document);
/// the mark-*deletion* twin has the same defect. The attribution belongs on the
/// PRECEDING mark instead — which is exactly what Word itself produces when you
/// press Enter at, or delete, the end of the last paragraph: the newly created
/// mark terminates the OLD text and the pre-existing final mark slides down to
/// terminate the new final paragraph.
///
/// This runs once per tracked-change edit, after the mint sites, on the BODY
/// block list only (a header/footer/cell final mark is not the document-final
/// mark). It preserves the accept/reject TEXT the engine projects — only the
/// physical mark that carries the marker moves (and, in the non-default case,
/// the pilcrow rPr that terminates the final paragraph, matching Word's
/// physical rotation).
pub(crate) fn normalize_final_mark_attribution(
    blocks: &mut [TrackedBlock],
    revision: &RevisionInfo,
    rev_counter: &mut u32,
) {
    let Some(last) = blocks.len().checked_sub(1) else {
        return;
    };
    // A body always ends in a paragraph; a trailing table/opaque is neither
    // valid nor something we can re-attribute a paragraph mark to.
    let BlockNode::Paragraph(final_para) = &blocks[last].block else {
        return;
    };
    // A move tail needs the move-aware rule. The moveTo DESTINATION copy that
    // ends the document leaves an insertion-class mark on the document-final
    // pilcrow (Word can't resolve it, exactly like a plain insert tail) — shift
    // it to the anchor. The moveFrom SHADOW (Deleted + move_id) sits at its
    // ORIGINAL position and is resolved by the moveFromRange pairing there, so
    // leave it and its paired half untouched.
    if blocks[last].move_id.is_some() {
        if matches!(blocks[last].status, TrackingStatus::Inserted(_)) {
            normalize_moved_final_mark(blocks, last, revision, rev_counter);
        }
        return;
    }
    // What serialize would emit for the final pilcrow: the paragraph's own
    // para_mark_status, else the block-level status (see
    // `serialize::serialize_paragraph_node`). `Some(Normal)` (an already
    // suppressed mark) short-circuits to Normal → nothing to do.
    let effective = final_para
        .para_mark_status
        .clone()
        .unwrap_or_else(|| blocks[last].status.clone());
    match effective {
        TrackingStatus::Inserted(_) => {
            normalize_inserted_final_mark(blocks, last, revision, rev_counter);
        }
        TrackingStatus::Deleted(_) => {
            normalize_deleted_final_mark(blocks, last, revision, rev_counter);
        }
        // A stacked final mark is never minted by the producers this runs after;
        // leave any pre-existing one untouched.
        TrackingStatus::Normal | TrackingStatus::InsertedThenDeleted(_) => {}
    }
}

/// The shared anchor rule for the tail-mark normalizers: a moveFrom SHADOW
/// (the block-`Deleted` half of a move, sitting at the source position) is
/// resolved by its own `moveFromRange` pairing, so a tail producer must leave it
/// untouched. EVERY other anchor — a surviving paragraph, OR a prior tail
/// producer's moveTo DESTINATION copy (block-`Inserted` + `move_id`) — is a
/// legitimate place to attribute the new break: its pilcrow is exactly the break
/// this producer introduces, and shifting the mark there touches only the
/// pilcrow marker, never any move's run-level pairing.
fn is_move_from_shadow(tb: &TrackedBlock) -> bool {
    matches!(tb.status, TrackingStatus::Deleted(_)) && tb.move_id.is_some()
}

/// Insert tail: the final paragraph is a freshly-inserted block. Keep it a
/// block-level insertion (its runs stay a tracked insertion) but suppress the
/// mark marker and give it the anchor's original pilcrow rPr — the pre-existing
/// final mark slides down to terminate it. The break AFTER the anchor becomes
/// the newly-inserted one, so the insertion marker moves to the anchor's mark.
///
/// The anchor is the paragraph immediately before this insert's contiguous
/// block-`Inserted` run. Usually a surviving paragraph, but an insert AFTER a
/// prior move-to-end lands after that move's moveTo DESTINATION copy
/// (block-`Inserted` + `move_id`) — which the walk-back stops at (it only walks
/// PLAIN inserts). That destination copy is still the right anchor: its pilcrow
/// is exactly the break this insert introduces, and attributing a PLAIN
/// insertion there touches only the pilcrow marker, never the move's run-level
/// `w:moveTo`/`w:moveFromRange` pairing (un-suppressing the marker it carried as
/// the previous final mark is fine — it is no longer document-final). Only a
/// moveFrom SHADOW anchor (block-`Deleted` + `move_id`) is left to its own
/// pairing — the SAME anchor rule `normalize_moved_final_mark` uses.
fn normalize_inserted_final_mark(
    blocks: &mut [TrackedBlock],
    last: usize,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
) {
    // Only the block-level-Inserted append shape (InsertParagraphs / diff
    // append) is produced by the mint sites this runs after.
    if !matches!(blocks[last].status, TrackingStatus::Inserted(_)) {
        return;
    }
    // Walk back over the contiguous run of block-Inserted paragraphs ending at
    // `last`; the anchor is the surviving paragraph immediately before it.
    let mut run_start = last;
    while run_start > 0
        && matches!(blocks[run_start - 1].status, TrackingStatus::Inserted(_))
        && matches!(blocks[run_start - 1].block, BlockNode::Paragraph(_))
        && blocks[run_start - 1].move_id.is_none()
    {
        run_start -= 1;
    }
    if run_start == 0 {
        // The entire document is a tracked insertion — no pre-existing anchor
        // mark to carry the attribution, and no original final mark to
        // preserve. Leave it as minted.
        return;
    }
    let anchor_idx = run_start - 1;
    if is_move_from_shadow(&blocks[anchor_idx]) {
        return; // resolved by its own moveFromRange pairing — see is_move_from_shadow
    }
    let BlockNode::Paragraph(anchor) = &blocks[anchor_idx].block else {
        return; // anchor is a table/opaque — cannot carry a paragraph mark.
    };
    let anchor_marks = anchor.paragraph_mark_marks.clone();
    let anchor_style = anchor.paragraph_mark_style_props.clone();
    let anchor_off = anchor.paragraph_mark_rpr_off;

    // Final paragraph: suppress its own mark marker and adopt the original final
    // mark's formatting. It stays a block-level Inserted paragraph (runs remain
    // inserted); only the pilcrow is untracked.
    if let BlockNode::Paragraph(p) = &mut blocks[last].block {
        p.para_mark_status = Some(TrackingStatus::Normal);
        p.paragraph_mark_marks = anchor_marks;
        p.paragraph_mark_style_props = anchor_style;
        p.paragraph_mark_rpr_off = anchor_off;
    }

    // Intermediate inserted paragraphs keep their own inserted break (driven by
    // block-level status): clear any leftover suppression left by a prior
    // normalization (a later insert re-appended past a once-final paragraph).
    for block in &mut blocks[run_start..last] {
        if let BlockNode::Paragraph(p) = &mut block.block
            && matches!(p.para_mark_status, Some(TrackingStatus::Normal))
        {
            p.para_mark_status = None;
        }
    }

    // The anchor's mark is now the newly-inserted break. Attribute the insertion
    // there unless a producer (the diff append path's
    // `annotate_paragraph_mark_status`) already did.
    if let BlockNode::Paragraph(a) = &mut blocks[anchor_idx].block
        && !matches!(a.para_mark_status, Some(TrackingStatus::Inserted(_)))
    {
        a.para_mark_status = Some(TrackingStatus::Inserted(next_revision(
            revision,
            rev_counter,
        )));
    }
}

/// Move tail: the document ends at a moveTo DESTINATION copy (block-level
/// `Inserted` + `move_id`). The final moved-in paragraph is the document-final
/// mark, so its pilcrow carries an insertion-class mark Word cannot resolve —
/// the same defect a plain insert tail has. Apply the insert-tail rule: suppress
/// the moved-in final pilcrow's marker and give it the anchor's original mark
/// rPr (the pre-existing final mark slides down to terminate it), and attribute
/// the newly-inserted break to the anchor's mark.
///
/// Only the pilcrow attribution moves. The block stays a block-level moveTo
/// insertion (its runs remain wrapped in `w:moveTo`, the `w:moveToRange`
/// start/end markers and their `w:name` pairing are unchanged), so the move pair
/// stays resolvable and reject-all — which drops the moveTo copy and restores the
/// moveFrom shadow at its original position — reproduces the original order
/// exactly. The anchor's break is a PLAIN insertion, not part of the move: the
/// anchor is an ordinary surviving paragraph, and our accept/reject projections
/// (which key off `para_mark_status`) yield the identical text either way — on
/// accept neither an `Inserted` anchor mark nor a `Normal` final mark merges; on
/// reject the anchor's `Inserted` mark would merge into the following paragraph,
/// but that paragraph is the moveTo copy the same reject removes, so the merge is
/// a no-op and the anchor stays the final paragraph.
///
/// TWO CONSECUTIVE MOVES to the document end make the anchor the PREVIOUS move's
/// destination copy (block-`Inserted` + a DIFFERENT `move_id`), not a surviving
/// paragraph. That copy is still the right anchor: it is the paragraph
/// immediately before this move's destination run, so its pilcrow is exactly the
/// break this move introduces. Shifting the plain insertion onto it touches only
/// the pilcrow marker — never either move's run-level `w:moveTo`/`w:moveFromRange`
/// pairing — so both moves stay independently resolvable, and reject-of-this-move
/// still merges the anchor's break into the moveTo copy the same reject removes
/// (a no-op). Only a moveFrom SHADOW anchor (block-`Deleted` + `move_id`) is left
/// untouched, its mark resolved by its own pairing.
fn normalize_moved_final_mark(
    blocks: &mut [TrackedBlock],
    last: usize,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
) {
    let Some(move_id) = blocks[last].move_id.clone() else {
        return;
    };
    // Walk back over the contiguous run of THIS move's destination copies
    // (block-Inserted paragraphs sharing `move_id`) ending at `last`; the anchor
    // is the surviving paragraph immediately before the run.
    let mut run_start = last;
    while run_start > 0
        && matches!(blocks[run_start - 1].status, TrackingStatus::Inserted(_))
        && matches!(blocks[run_start - 1].block, BlockNode::Paragraph(_))
        && blocks[run_start - 1].move_id.as_deref() == Some(move_id.as_str())
    {
        run_start -= 1;
    }
    if run_start == 0 {
        // The moved range opens the document — no pre-existing anchor mark to
        // carry the attribution, and no original final mark to preserve. Leave it
        // as minted.
        return;
    }
    let anchor_idx = run_start - 1;
    if is_move_from_shadow(&blocks[anchor_idx]) {
        return; // resolved by its own moveFromRange pairing — see is_move_from_shadow
    }
    let BlockNode::Paragraph(anchor) = &blocks[anchor_idx].block else {
        return; // anchor is a table/opaque — cannot carry a paragraph mark.
    };
    let anchor_marks = anchor.paragraph_mark_marks.clone();
    let anchor_style = anchor.paragraph_mark_style_props.clone();
    let anchor_off = anchor.paragraph_mark_rpr_off;

    // Final moved-in paragraph: suppress its own pilcrow marker and adopt the
    // original final mark's formatting. It stays a block-level moveTo insertion
    // (runs remain moved); only the pilcrow is untracked.
    if let BlockNode::Paragraph(p) = &mut blocks[last].block {
        p.para_mark_status = Some(TrackingStatus::Normal);
        p.paragraph_mark_marks = anchor_marks;
        p.paragraph_mark_style_props = anchor_style;
        p.paragraph_mark_rpr_off = anchor_off;
    }

    // The anchor's mark is now the newly-inserted break — a plain insertion (the
    // anchor is a normal surviving paragraph, not part of the move pair).
    if let BlockNode::Paragraph(a) = &mut blocks[anchor_idx].block
        && !matches!(a.para_mark_status, Some(TrackingStatus::Inserted(_)))
    {
        a.para_mark_status = Some(TrackingStatus::Inserted(next_revision(
            revision,
            rev_counter,
        )));
    }
}

/// Delete tail: the final paragraph is being tracked-deleted. Turn it into the
/// surviving final mark — a block-level Normal paragraph whose runs are wrapped
/// as a tracked DELETION and whose pilcrow is untracked (keeping its own rPr, as
/// it IS the pre-existing final mark) — and attribute the mark-deletion to the
/// break BEFORE the deleted run (the preceding paragraph's mark), which is the
/// break that actually disappears on accept. When the deleted run starts the
/// document there is no preceding paragraph: the final paragraph becomes the
/// empty survivor (deleting every paragraph leaves one empty mark) and no extra
/// mark-deletion is added.
fn normalize_deleted_final_mark(
    blocks: &mut [TrackedBlock],
    last: usize,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
) {
    if !matches!(blocks[last].status, TrackingStatus::Deleted(_)) {
        return;
    }
    let mut run_start = last;
    while run_start > 0
        && matches!(blocks[run_start - 1].status, TrackingStatus::Deleted(_))
        && matches!(blocks[run_start - 1].block, BlockNode::Paragraph(_))
        && blocks[run_start - 1].move_id.is_none()
    {
        run_start -= 1;
    }

    // Convert the final paragraph into the surviving final mark.
    if let BlockNode::Paragraph(p) = &mut blocks[last].block {
        mark_final_paragraph_runs_deleted(p, revision, rev_counter);
        p.para_mark_status = None;
    }
    blocks[last].status = TrackingStatus::Normal;

    if run_start > 0 && blocks[run_start - 1].move_id.is_none() {
        let anchor_idx = run_start - 1;
        if let BlockNode::Paragraph(a) = &mut blocks[anchor_idx].block
            && !matches!(a.para_mark_status, Some(TrackingStatus::Deleted(_)))
        {
            a.para_mark_status = Some(TrackingStatus::Deleted(next_revision(
                revision,
                rev_counter,
            )));
        }
    }
}

/// Wrap a paragraph's runs as a tracked deletion in place (the segment-level
/// equivalent of the block-level `Deleted` status the delete mint sites set),
/// so the final paragraph can drop back to block-level `Normal` and survive as
/// the document's final mark. Per-segment so a pre-existing insertion stacks
/// rather than being silently un-tracked.
fn mark_final_paragraph_runs_deleted(
    p: &mut ParagraphNode,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
) {
    for seg in &mut p.segments {
        seg.status = match &seg.status {
            TrackingStatus::Normal => TrackingStatus::Deleted(next_revision(revision, rev_counter)),
            TrackingStatus::Inserted(ins) => {
                TrackingStatus::InsertedThenDeleted(Box::new(StackedRevision {
                    inserted: ins.clone(),
                    deleted: next_revision(revision, rev_counter),
                }))
            }
            // Already carries a deletion — leave its resolution intact.
            other => other.clone(),
        };
    }
}

/// Re-establish the document-final-mark invariant AFTER a SELECTIVE resolution.
///
/// [`normalize_final_mark_attribution`] runs once, at edit-mint time, so the
/// bytes an edit producer emits never leave a tracked insertion/deletion on the
/// document-final pilcrow (Word cannot resolve one — accept-all leaves it
/// pending forever; see that function's doc comment). A SELECTIVE resolution
/// (`Resolution::Selective`) re-runs neither the mint sites nor that pass, yet
/// it reshapes the body: it can strip the edit-time suppression off a
/// still-tracked trailing paragraph, or reject away every follower so an anchor
/// that still carries a pending mark-insertion becomes the final paragraph.
/// Either way the projected body violates the invariant. This pass restores it
/// on the projected BODY blocks (a header/footer/cell final mark is not the
/// document-final mark, exactly as the mint-time pass is body-only).
///
/// Two shapes arise, and — unlike the mint-time pass — NEITHER mints a fresh
/// revision (a revision "that was never in the enumeration" is its own bug):
///
///  * **Suppression stripped.** The final block is itself a block-level
///    `Inserted`/`Deleted` paragraph whose pilcrow the edit-time pass had
///    suppressed (`para_mark_status = Some(Normal)`, adopting the original final
///    mark's rPr). The selective projection re-normalized that `Some(Normal)` to
///    `None`, re-exposing the block-level status on the final pilcrow. Re-apply
///    the suppression — the block stays tracked (its runs keep their status),
///    only the pilcrow marker is suppressed, and the break INTRODUCING this
///    paragraph is already tracked on the preceding mark (or that preceding
///    paragraph's own block status), so no attribution has to move. A moveTo
///    DESTINATION copy that ends the document is the same shape (block-level
///    `Inserted` + `move_id`, pilcrow suppressed at mint by
///    `normalize_moved_final_mark`); re-suppressing its pilcrow leaves the move
///    pairing (`move_id`, `moveToRange` markers, its runs' `w:moveTo` wrapping)
///    entirely untouched — only the terminating marker is suppressed.
///
///  * **Anchor stranded as final.** Every trailing inserted/deleted paragraph
///    was resolved away, leaving a SURVIVING (`Normal`-block) paragraph whose
///    own `para_mark_status` is still a pending `Inserted`/`Deleted` mark — the
///    break that once introduced the now-gone followers. A tracked break on the
///    final paragraph means "there is one more (now empty) paragraph after this
///    one": materialize exactly that. Append an empty, untracked-mark paragraph
///    that becomes the new document-final mark (adopting the surviving
///    paragraph's final-mark rPr); the pending mark stays put, now a NON-final
///    break introducing the empty tail. This is the model's honest reading of
///    the mark (it is precisely the edit-time "the pre-existing final mark
///    slides down to terminate the new final paragraph" shape) and it keeps the
///    revision resolvable — accepting it keeps the empty trailing paragraph,
///    rejecting it merges the empty tail back and restores the original final
///    paragraph — with no id invented and no other projection changed.
fn renormalize_final_mark_after_selective(blocks: &mut Vec<TrackedBlock>) {
    let Some(last) = blocks.len().checked_sub(1) else {
        return;
    };
    // Only a trailing paragraph carries the document-final mark.
    let BlockNode::Paragraph(final_para) = &blocks[last].block else {
        return;
    };
    // What serialize emits for the final pilcrow (see `effective_final_mark` in
    // the sentinel tests): the paragraph's own `para_mark_status`, else the
    // block-level status. Only a pending insertion/deletion is forbidden here.
    let effective = final_para
        .para_mark_status
        .clone()
        .unwrap_or_else(|| blocks[last].status.clone());
    let mark_is_pending = matches!(
        effective,
        TrackingStatus::Inserted(_) | TrackingStatus::Deleted(_)
    );
    if !mark_is_pending {
        return;
    }

    let block_level_tracked = matches!(
        blocks[last].status,
        TrackingStatus::Inserted(_) | TrackingStatus::Deleted(_)
    );
    if block_level_tracked {
        // Suppression stripped: re-suppress the final pilcrow. The paragraph
        // stays a block-level insertion/deletion (its runs keep their status);
        // only the terminating mark is suppressed.
        if let BlockNode::Paragraph(p) = &mut blocks[last].block {
            p.para_mark_status = Some(TrackingStatus::Normal);
        }
        return;
    }

    // Anchor stranded as final: the surviving final paragraph carries a pending
    // break with no follower. Materialize the empty tail paragraph that break
    // introduces, and make it the untracked document-final mark.
    let tail = empty_tail_after_stranded_break(final_para, blocks);
    blocks.push(TrackedBlock {
        block: BlockNode::from(tail),
        status: TrackingStatus::Normal,
        move_id: None,
        block_sdt_wrap: None,
    });
}

/// Build the empty untracked-mark paragraph that a stranded pending break on
/// `stranded` (the surviving would-be-final paragraph) introduces. It inherits
/// `stranded`'s final-mark formatting (paragraph-mark rPr and paragraph
/// formatting) so the document-final pilcrow keeps its look, but carries NO
/// content and NONE of `stranded`'s own pending revisions or content-bearing
/// state (those stay on `stranded`). Its id is fresh and unique document-wide.
fn empty_tail_after_stranded_break(
    stranded: &ParagraphNode,
    blocks: &[TrackedBlock],
) -> ParagraphNode {
    let mut used = HashSet::new();
    for tb in blocks {
        collect_body_block_ids(&tb.block, &mut used);
    }
    let mut suffix = 0usize;
    let tail_id = loop {
        let candidate = NodeId::from(format!("{}__tailmark{suffix}", stranded.id.0));
        if !used.contains(&candidate) {
            break candidate;
        }
        suffix += 1;
    };

    let mut tail = stranded.clone();
    tail.id = tail_id;
    // Empty content; the paragraph exists only to carry the final mark.
    tail.segments = Vec::new();
    // The new document-final pilcrow is untracked.
    tail.para_mark_status = None;
    // Every content- or revision-bearing field stays on `stranded`; the tail
    // must not duplicate a pending revision or a unique wire id.
    tail.formatting_change = None;
    tail.section_property_change = None;
    tail.section_properties = None;
    tail.para_split = false;
    tail.para_id = None;
    tail.text_id = None;
    tail.block_text_hash = None;
    tail.rendered_text = None;
    tail.numbering = None;
    tail.numbering_suppressed = false;
    tail.materialized_numbering = None;
    tail.literal_prefix = None;
    tail.literal_prefix_marks = Vec::new();
    tail.literal_prefix_style_props = StyleProps::default();
    tail.literal_prefix_rpr_authored = RunRprAuthored::default();
    tail.literal_prefix_leading_rpr = None;
    tail.literal_prefix_trailing_rpr = None;
    tail.literal_prefix_leading_ws = String::new();
    tail.literal_prefix_trailing_ws = String::new();
    tail.preserved_ppr = Vec::new();
    tail
}

/// Collect every block-addressable `NodeId` in `block` — its own id plus, for a
/// table, all nested row/cell/cell-block ids — so a materialized tail paragraph
/// can be given a document-wide-unique id. Twin of `edit::collect_block_node_ids`.
fn collect_body_block_ids(block: &BlockNode, used: &mut HashSet<NodeId>) {
    match block {
        BlockNode::Paragraph(p) => {
            used.insert(p.id.clone());
        }
        BlockNode::OpaqueBlock(o) => {
            used.insert(o.id.clone());
        }
        BlockNode::Table(t) => {
            used.insert(t.id.clone());
            for row in &t.rows {
                used.insert(row.id.clone());
                for cell in &row.cells {
                    used.insert(cell.id.clone());
                    for b in &cell.blocks {
                        collect_body_block_ids(b, used);
                    }
                }
            }
        }
    }
}

/// Apply row-level tracked changes for a table structure change.
///
/// Instead of deleting the entire old table and inserting the entire new table,
/// this produces a single merged table where:
/// - Deleted rows are marked with `tracking_status = Deleted` and cell content
///   is wrapped in deleted tracked segments.
/// - Inserted rows are taken from the target table with `tracking_status = Inserted`.
/// - Matched rows are preserved, with cell-level inline tracked changes applied
///   for cells whose text differs.
pub(crate) fn apply_table_structure_changed(
    blocks: &mut [TrackedBlock],
    table_id: &NodeId,
    target_table: &TableNode,
    diff: &TableDiffResult,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    context: &str,
) -> Result<(), MergeError> {
    let idx = find_block_index(blocks, table_id).ok_or_else(|| MergeError {
        message: "table for structure change not found in base model".to_string(),
        context: format!("{context}:{}", table_id.0),
    })?;

    let base_table = match &blocks[idx].block {
        BlockNode::Table(t) => t.clone(),
        _ => {
            return Err(MergeError {
                message: "TableStructureChanged references non-table block".to_string(),
                context: format!("{context}:{}", table_id.0),
            });
        }
    };

    // Build merged rows following the row alignment order.
    let mut merged_rows = Vec::new();
    for alignment in &diff.row_alignment {
        match alignment {
            TableRowAlignment::Deleted { old_row } => {
                // Whole-row deletion: the row-level `w:trPr/w:del` marker + a
                // content deletion inside each cell. No per-cell `w:cellDel`
                // (see `mark_whole_row_deleted`).
                let mut row = base_table.rows[*old_row].clone();
                mark_whole_row_deleted(&mut row, revision, rev_counter);
                merged_rows.push(row);
            }
            TableRowAlignment::Inserted { new_row } => {
                // Whole-row insertion: the row-level `w:trPr/w:ins` marker only
                // (see `mark_whole_row_inserted`).
                let mut row = target_table.rows[*new_row].clone();
                mark_whole_row_inserted(&mut row, revision, rev_counter);
                merged_rows.push(row);
            }
            TableRowAlignment::Matched { old_row, new_row } => {
                // Start from the base row and apply cell-level changes.
                let mut row = base_table.rows[*old_row].clone();
                let new_row_ref = &target_table.rows[*new_row];

                // Pair old cells with new cells positionally within the row.
                let max_cells = row.cells.len().max(new_row_ref.cells.len());
                let mut merged_cells = Vec::new();

                for cell_idx in 0..max_cells {
                    if cell_idx < row.cells.len() && cell_idx < new_row_ref.cells.len() {
                        // Both old and new have a cell at this position.
                        let mut cell = row.cells[cell_idx].clone();
                        let new_cell_ref = &new_row_ref.cells[cell_idx];
                        // Adopt the target cell's structural merge attributes.
                        // gridSpan (horizontal merge) and vMerge (vertical merge)
                        // are not tracked-change axes — once we are on the
                        // TableStructureChanged path the accepted result must
                        // structurally equal the target. If we kept the base
                        // cell's v_merge/grid_span, a target restart anchor could
                        // be lost while the continue cells below it survive,
                        // producing an orphan <w:vMerge/> continue (an invalid
                        // grid). See canonicalize_table's restart-anchor check.
                        cell.grid_span = new_cell_ref.grid_span;
                        cell.v_merge = new_cell_ref.v_merge.clone();
                        apply_cell_formatting_change(&mut cell, &new_cell_ref.formatting, revision);
                        reconcile_cell_blocks(
                            &mut cell,
                            new_cell_ref,
                            revision,
                            rev_counter,
                            context,
                        )?;
                        merged_cells.push(cell);
                    } else if cell_idx < row.cells.len() {
                        // Cell exists only in old row (deleted cell in matched row).
                        let mut cell = row.cells[cell_idx].clone();
                        cell.tracking_status = Some(TrackingStatus::Deleted(next_revision(
                            revision,
                            rev_counter,
                        )));
                        mark_cell_content_deleted(&mut cell, revision, rev_counter);
                        merged_cells.push(cell);
                    } else {
                        // Cell exists only in new row (inserted cell in matched row).
                        let mut cell = new_row_ref.cells[cell_idx].clone();
                        cell.tracking_status = Some(TrackingStatus::Inserted(next_revision(
                            revision,
                            rev_counter,
                        )));
                        merged_cells.push(cell);
                    }
                }

                row.cells = merged_cells;
                merged_rows.push(row);
            }
        }
    }

    // Replace the base table's rows with merged rows and update the hash.
    let merged_table = TableNode {
        id: base_table.id.clone(),
        rows: merged_rows,
        structure_hash: target_table.structure_hash.clone(),
        formatting: base_table.formatting.clone(),
        formatting_change: base_table.formatting_change.clone(),
    };
    blocks[idx].block = BlockNode::from(merged_table);
    Ok(())
}

/// Type-tagged fingerprint for block-level alignment within a cell.
fn block_fingerprint(block: &BlockNode) -> String {
    match block {
        BlockNode::Paragraph(p) => {
            let inline_text = extract_inlines_text(&p.all_inlines_owned());
            // When inline text is empty, fall back to rendered_text then literal_prefix.
            // This mirrors extract_block_text in table.rs — paragraphs whose content
            // lives entirely in the prefix (e.g., "i." list items) would otherwise
            // all collide on empty text.
            let text = if inline_text.trim().is_empty() {
                p.rendered_text
                    .as_deref()
                    .filter(|t| !t.trim().is_empty())
                    .map(str::to_owned)
                    .or_else(|| p.literal_prefix.clone())
                    .unwrap_or(inline_text)
            } else {
                inline_text
            };
            let (num_id, ilvl) = p
                .numbering
                .as_ref()
                .map_or((u32::MAX, u32::MAX), |n| (n.num_id, n.ilvl));
            format!("P:{num_id}:{ilvl}:{text}")
        }
        BlockNode::Table(t) => format!("T:{}", t.structure_hash),
        BlockNode::OpaqueBlock(o) => format!("O:{}", o.opaque_ref),
    }
}

/// Apply tracked inline changes to a paragraph within a cell, following the
/// same pattern as `apply_block_modified` and `apply_table_cells_modified`:
/// inline diff → segment conversion → prefix handling → structural markers.
///
/// Operates on a mutable paragraph + immutable new paragraph reference.
fn apply_paragraph_diff_in_cell(
    paragraph: &mut ParagraphNode,
    new_para: &ParagraphNode,
    old_block: &BlockNode,
    new_block: &BlockNode,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    context: &str,
) -> Result<(), MergeError> {
    let old_inlines = paragraph.all_inlines_owned();
    let new_inlines = new_para.all_inlines_owned();
    let inline_changes = diff_block_content_with_marks(&old_inlines, &new_inlines);

    let base_opaques = collect_opaques(old_block);
    let target_opaques = collect_opaques(new_block);
    let original_segments = paragraph.segments.clone();

    let new_segments = inline_changes_to_segments_with_opaques(
        &paragraph.id,
        &inline_changes,
        revision,
        rev_counter,
        &base_opaques,
        &target_opaques,
    )
    .map_err(|e| MergeError {
        message: format!("cell paragraph inline diff failed: {}", e.message),
        context: context.to_string(),
    })?;

    // Save original numbering before prefix handling may clear it.
    let original_numbering = paragraph.numbering.clone();
    let original_has_prefix = paragraph.numbering.is_some() || paragraph.literal_prefix.is_some();
    let mut prefix_was_materialized = false;

    // Prefix materialization is only needed when paragraph properties cannot
    // preserve the visible prefix through redline export: literal prefixes
    // on either side, or a source-side structural numbering prefix that
    // disappears on the target side. When both sides keep structural
    // numbering, Word synthesizes the counter. When the target only adds
    // structural numbering, let pPrChange record it so numbering remains
    // structural for downstream counters.
    let mut final_segments = new_segments;
    if let Some(plan) = plan_prefix_materialization(paragraph, new_para) {
        let mut prefix_segments = Vec::new();
        if plan.emit_deleted_prefix
            && let Some(old_p) = &plan.old_prefix
            && let Some(kind) = plan.deleted_kind
        {
            prefix_segments.push(TrackedSegment {
                status: TrackingStatus::Deleted(next_revision(revision, rev_counter)),
                inlines: vec![InlineNode::from(make_prefix_text_node(
                    materialized_prefix_node_id(&paragraph.id, kind),
                    kind,
                    materialized_prefix_text(old_p, paragraph),
                    paragraph,
                ))],
            });
        }
        if plan.emit_inserted_prefix
            && let Some(new_p) = &plan.new_prefix
            && let Some(kind) = plan.inserted_kind
        {
            prefix_segments.push(TrackedSegment {
                status: TrackingStatus::Inserted(next_revision(revision, rev_counter)),
                inlines: vec![InlineNode::from(make_prefix_text_node(
                    materialized_prefix_node_id(&paragraph.id, kind),
                    kind,
                    materialized_prefix_text(new_p, new_para),
                    new_para,
                ))],
            });
        }
        prefix_segments.append(&mut final_segments);
        final_segments = prefix_segments;
        paragraph.literal_prefix = None;
        sync_literal_prefix_geometry(new_para, paragraph);
        if plan.target_uses_structural_numbering {
            paragraph.materialized_numbering = None;
        } else {
            paragraph.numbering = None;
            prefix_was_materialized = true;
        }
    }

    // Paragraph formatting changes (same as apply_block_modified).
    // Use original_numbering for comparison (prefix handling may have cleared numbering).
    // Structural comparison (num_id + ilvl only) — counter drift is not a formatting change.
    let align_changed = paragraph.align != new_para.align;
    let indent_changed = paragraph.indent != new_para.indent;
    let spacing_changed = paragraph.spacing != new_para.spacing;
    let numbering_changed =
        !crate::domain::numbering_structurally_eq(&original_numbering, &new_para.numbering);
    let style_changed = paragraph.style_id != new_para.style_id;
    let keep_next_changed = paragraph.keep_next != new_para.keep_next;
    let keep_lines_changed = paragraph.keep_lines != new_para.keep_lines;
    let page_break_before_changed = paragraph.page_break_before != new_para.page_break_before;
    let widow_control_changed = paragraph.widow_control != new_para.widow_control;
    let contextual_spacing_changed = paragraph.contextual_spacing != new_para.contextual_spacing;
    let shading_changed = paragraph.shading != new_para.shading;
    let borders_changed = paragraph.borders != new_para.borders;
    let tab_stops_changed = paragraph.tab_stops != new_para.tab_stops;
    let text_direction_changed = paragraph.text_direction != new_para.text_direction;
    let text_alignment_changed = paragraph.text_alignment != new_para.text_alignment;
    let mirror_indents_changed = paragraph.mirror_indents != new_para.mirror_indents;
    let bidi_changed = paragraph.bidi != new_para.bidi;
    let suppress_auto_hyphens_changed =
        paragraph.suppress_auto_hyphens != new_para.suppress_auto_hyphens;
    let snap_to_grid_changed = paragraph.snap_to_grid != new_para.snap_to_grid;
    let overflow_punct_changed = paragraph.overflow_punct != new_para.overflow_punct;
    let adjust_right_ind_changed = paragraph.adjust_right_ind != new_para.adjust_right_ind;
    let word_wrap_changed = paragraph.word_wrap != new_para.word_wrap;
    let frame_pr_changed = paragraph.frame_pr != new_para.frame_pr;
    let section_properties_changed = paragraph.section_properties != new_para.section_properties;

    let any_changed = align_changed
        || indent_changed
        || spacing_changed
        || numbering_changed
        || style_changed
        || keep_next_changed
        || keep_lines_changed
        || page_break_before_changed
        || widow_control_changed
        || contextual_spacing_changed
        || shading_changed
        || borders_changed
        || tab_stops_changed
        || text_direction_changed
        || text_alignment_changed
        || mirror_indents_changed
        || bidi_changed
        || suppress_auto_hyphens_changed
        || snap_to_grid_changed
        || overflow_punct_changed
        || adjust_right_ind_changed
        || word_wrap_changed
        || frame_pr_changed
        || section_properties_changed;

    if any_changed {
        let numbering_explicitly_absent =
            original_numbering.is_none() && new_para.numbering.is_some() && !original_has_prefix;
        paragraph.formatting_change = Some(ParagraphFormattingChange {
            revision_id: revision.revision_id,
            identity: 0,
            previous_alignment: paragraph.align.clone(),
            // Snapshot AUTHORED-direct indent/spacing (previous DIRECT pPr),
            // not resolved effective — see snapshot_paragraph_formatting.
            previous_indentation: paragraph
                .authored_indent
                .clone()
                .or_else(|| paragraph.indent.clone()),
            previous_spacing: paragraph
                .authored_spacing
                .clone()
                .or_else(|| paragraph.spacing.clone()),
            previous_numbering: original_numbering.clone(),
            previous_numbering_explicitly_absent: numbering_explicitly_absent,
            previous_style_id: paragraph.style_id.clone(),
            previous_keep_next: paragraph.keep_next,
            previous_keep_lines: paragraph.keep_lines,
            previous_page_break_before: paragraph.page_break_before,
            previous_widow_control: paragraph.widow_control,
            previous_contextual_spacing: paragraph.contextual_spacing,
            previous_shading: paragraph.shading.clone(),
            previous_borders: paragraph.borders.clone(),
            previous_tab_stops: paragraph.tab_stops.clone(),
            previous_literal_prefix_leading_tab_twips: paragraph.literal_prefix_leading_tab_twips,
            previous_literal_prefix_trailing_tab_stop_twips: paragraph
                .literal_prefix_trailing_tab_stop_twips,
            previous_paragraph_mark_marks: paragraph.paragraph_mark_marks.clone(),
            previous_paragraph_mark_style_props: paragraph.paragraph_mark_style_props.clone(),
            previous_paragraph_mark_rpr_off: paragraph.paragraph_mark_rpr_off,
            previous_text_direction: paragraph.text_direction.clone(),
            previous_text_alignment: paragraph.text_alignment.clone(),
            previous_mirror_indents: paragraph.mirror_indents,
            previous_auto_space_de: paragraph.auto_space_de,
            previous_auto_space_dn: paragraph.auto_space_dn,
            previous_bidi: paragraph.bidi,
            previous_suppress_auto_hyphens: paragraph.suppress_auto_hyphens,
            previous_snap_to_grid: paragraph.snap_to_grid,
            previous_overflow_punct: paragraph.overflow_punct,
            previous_adjust_right_ind: paragraph.adjust_right_ind,
            previous_word_wrap: paragraph.word_wrap,
            previous_frame_pr: paragraph.frame_pr.clone(),
            previous_preserved_ppr: paragraph.preserved_ppr.clone(),
            author: revision.author.clone().unwrap_or_default(),
            date: revision.date.clone(),
        });
        paragraph.align = new_para.align.clone();
        paragraph.has_direct_align = new_para.has_direct_align;
        paragraph.indent = new_para.indent.clone();
        paragraph.has_direct_indent = new_para.has_direct_indent;
        paragraph.authored_indent = new_para.authored_indent.clone();
        paragraph.spacing = new_para.spacing.clone();
        paragraph.has_direct_spacing = new_para.has_direct_spacing;
        paragraph.authored_spacing = new_para.authored_spacing.clone();
        paragraph.style_id = new_para.style_id.clone();
        paragraph.keep_next = new_para.keep_next;
        paragraph.keep_lines = new_para.keep_lines;
        paragraph.page_break_before = new_para.page_break_before;
        paragraph.widow_control = new_para.widow_control;
        paragraph.contextual_spacing = new_para.contextual_spacing;
        paragraph.shading = new_para.shading.clone();
        paragraph.borders = new_para.borders.clone();
        paragraph.has_direct_keep_next = new_para.has_direct_keep_next;
        paragraph.has_direct_keep_lines = new_para.has_direct_keep_lines;
        paragraph.has_direct_page_break_before = new_para.has_direct_page_break_before;
        paragraph.has_direct_widow_control = new_para.has_direct_widow_control;
        paragraph.has_direct_contextual_spacing = new_para.has_direct_contextual_spacing;
        paragraph.has_direct_shading = new_para.has_direct_shading;
        paragraph.has_direct_borders = new_para.has_direct_borders;
        paragraph.tab_stops = new_para.tab_stops.clone();
        paragraph.effective_tab_stops_rel = new_para.effective_tab_stops_rel.clone();
        paragraph.text_direction = new_para.text_direction.clone();
        paragraph.text_alignment = new_para.text_alignment.clone();
        paragraph.mirror_indents = new_para.mirror_indents;
        paragraph.bidi = new_para.bidi;
        paragraph.suppress_auto_hyphens = new_para.suppress_auto_hyphens;
        paragraph.snap_to_grid = new_para.snap_to_grid;
        paragraph.overflow_punct = new_para.overflow_punct;
        paragraph.adjust_right_ind = new_para.adjust_right_ind;
        paragraph.word_wrap = new_para.word_wrap;
        paragraph.frame_pr = new_para.frame_pr.clone();
        paragraph.section_property_change = new_para.section_property_change.clone();
        paragraph.section_properties = new_para.section_properties.clone();
        if numbering_changed && !prefix_was_materialized {
            paragraph.numbering = new_para.numbering.clone();
            // Carry the target's numbering provenance (see apply_block_modified).
            paragraph.has_direct_numbering = new_para.has_direct_numbering;
            if new_para.numbering.is_some() {
                paragraph.literal_prefix = None;
            }
        }
    }

    inject_structural_markers_at_offsets(
        &mut final_segments,
        &original_segments,
        Some(new_para.segments.as_slice()),
    );

    prune_empty_text_inlines(&mut final_segments);
    paragraph.segments = final_segments;
    Ok(())
}

/// Mark a single paragraph block as deleted: wrap all inlines in a Deleted
/// segment and mark the para_mark as deleted.
fn mark_paragraph_deleted(
    para: &mut ParagraphNode,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
) {
    let all_inlines: Vec<InlineNode> = para
        .segments
        .iter()
        .flat_map(|s| s.inlines.clone())
        .collect();
    if !all_inlines.is_empty() {
        para.segments = vec![TrackedSegment {
            status: TrackingStatus::Deleted(next_revision(revision, rev_counter)),
            inlines: all_inlines,
        }];
    }
    para.para_mark_status = Some(TrackingStatus::Deleted(next_revision(
        revision,
        rev_counter,
    )));
}

/// Mark a single paragraph block as inserted: wrap all inlines in an Inserted
/// segment and mark the para_mark as inserted.
fn mark_paragraph_inserted(
    para: &mut ParagraphNode,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
) {
    let all_inlines: Vec<InlineNode> = para
        .segments
        .iter()
        .flat_map(|s| s.inlines.clone())
        .collect();
    if !all_inlines.is_empty() {
        para.segments = vec![TrackedSegment {
            status: TrackingStatus::Inserted(next_revision(revision, rev_counter)),
            inlines: all_inlines,
        }];
    }
    para.para_mark_status = Some(TrackingStatus::Inserted(next_revision(
        revision,
        rev_counter,
    )));
}

/// Mark a WHOLE ROW as tracked-deleted: the row-level `w:trPr/w:del` structural
/// marker (§17.13.5.13) plus a tracked deletion of each cell's *content*. It
/// deliberately does NOT set the cell's own `tracking_status` (which serializes
/// as `w:cellDel`, §17.13.5.1).
///
/// Model rule (Word parity + the selective-resolution invariant): `w:cellDel`
/// marks a cell deleted WITHIN a surviving row — a column delete or a cell
/// merge. Real Word never emits it on the cells of a row that is itself deleted;
/// the row's `w:trPr/w:del` subsumes them (see the `row_del_*` word-compliance
/// fixtures: a deleted row's `<w:tcPr>` carries no `cellDel`). Minting a per-cell
/// `cellDel` here would also make a cell-less row *representable*: selective
/// resolution of one cell's `cellDel` in isolation would physically drop that
/// cell while the row (its marker unresolved) survives, producing a `<w:tr>`
/// with zero `<w:tc>` — invalid per `CT_Row` (§17.4.72), which the engine's own
/// importer refuses. Leaving the cells markerless makes that state
/// unrepresentable: only the row marker removes cells, and it removes the whole
/// row atomically.
fn mark_whole_row_deleted(
    row: &mut crate::domain::TableRowNode,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
) {
    row.tracking_status = Some(TrackingStatus::Deleted(next_revision(
        revision,
        rev_counter,
    )));
    for cell in &mut row.cells {
        mark_cell_content_deleted(cell, revision, rev_counter);
    }
}

/// Insert counterpart of [`mark_whole_row_deleted`]: the row-level `w:trPr/w:ins`
/// marker (§17.13.5.17) only. No per-cell `w:cellIns` (§17.13.5.2), for the same
/// two reasons — Word does not emit it on the cells of a wholly-inserted row, and
/// resolving one cell's `cellIns` in isolation (reject) would strip that cell out
/// of a still-inserted row, yielding a cell-less `<w:tr>`.
fn mark_whole_row_inserted(
    row: &mut crate::domain::TableRowNode,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
) {
    row.tracking_status = Some(TrackingStatus::Inserted(next_revision(
        revision,
        rev_counter,
    )));
}

/// Mark a table as deleted: the row-level structural marker on every row (see
/// [`mark_whole_row_deleted`] for why cells stay markerless).
fn mark_table_deleted(table: &mut TableNode, revision: &RevisionInfo, rev_counter: &mut u32) {
    for row in &mut table.rows {
        mark_whole_row_deleted(row, revision, rev_counter);
    }
}

/// Mark a table as inserted: the row-level structural marker on every row (see
/// [`mark_whole_row_inserted`]).
fn mark_table_inserted(table: &mut TableNode, revision: &RevisionInfo, rev_counter: &mut u32) {
    for row in &mut table.rows {
        mark_whole_row_inserted(row, revision, rev_counter);
    }
}

/// Reconcile cell blocks by aligning old and new blocks at the block level,
/// then applying per-paragraph tracked changes. This preserves paragraph
/// boundaries and opaque blocks that the flat text_diff approach loses.
///
/// For matched paragraphs, applies the full reconciliation pattern (inline
/// diff, prefix changes, formatting changes, structural markers) — same as
/// `apply_block_modified` and `apply_table_cells_modified`.
fn reconcile_cell_blocks(
    cell: &mut TableCellNode,
    new_cell: &TableCellNode,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    context: &str,
) -> Result<(), MergeError> {
    let old_fps: Vec<String> = cell.blocks.iter().map(block_fingerprint).collect();
    let new_fps: Vec<String> = new_cell.blocks.iter().map(block_fingerprint).collect();

    // Fast path: identical fingerprints means no changes.
    if old_fps == new_fps {
        return Ok(());
    }

    let ops = similar::capture_diff_slices_deadline(Algorithm::Patience, &old_fps, &new_fps, None);

    // Post-diff invariant: no base-side index should appear in both Equal and
    // Delete/Replace ops. If it does, the diff alignment is broken and we must
    // reclassify the overlapping Equal entries as Insert+Delete pairs.
    let ops = {
        let mut equal_indices = HashSet::new();
        let mut removed_indices = HashSet::new();
        for op in &ops {
            match op {
                DiffOp::Equal { old_index, len, .. } => {
                    for i in 0..*len {
                        equal_indices.insert(old_index + i);
                    }
                }
                DiffOp::Delete {
                    old_index, old_len, ..
                }
                | DiffOp::Replace {
                    old_index, old_len, ..
                } => {
                    for i in 0..*old_len {
                        removed_indices.insert(old_index + i);
                    }
                }
                DiffOp::Insert { .. } => {}
            }
        }
        let overlap: HashSet<usize> = equal_indices
            .intersection(&removed_indices)
            .copied()
            .collect();
        if overlap.is_empty() {
            ops
        } else {
            debug_assert!(
                false,
                "post-diff invariant violation in {context}: base indices {overlap:?} appear in both Equal and Delete/Replace ops"
            );
            // Release-mode recovery: rebuild ops, splitting overlapping Equal
            // entries into Delete+Insert pairs.
            let mut fixed_ops = Vec::with_capacity(ops.len());
            for op in ops {
                match op {
                    DiffOp::Equal {
                        old_index,
                        new_index,
                        len,
                    } => {
                        // Split into contiguous runs of clean vs overlapping indices.
                        let mut i = 0;
                        while i < len {
                            if overlap.contains(&(old_index + i)) {
                                // Collect contiguous overlapping range.
                                let start = i;
                                while i < len && overlap.contains(&(old_index + i)) {
                                    i += 1;
                                }
                                let span = i - start;
                                // Emit as Delete (old side) + Insert (new side).
                                fixed_ops.push(DiffOp::Replace {
                                    old_index: old_index + start,
                                    old_len: span,
                                    new_index: new_index + start,
                                    new_len: span,
                                });
                            } else {
                                // Collect contiguous clean range.
                                let start = i;
                                while i < len && !overlap.contains(&(old_index + i)) {
                                    i += 1;
                                }
                                fixed_ops.push(DiffOp::Equal {
                                    old_index: old_index + start,
                                    new_index: new_index + start,
                                    len: i - start,
                                });
                            }
                        }
                    }
                    other => fixed_ops.push(other),
                }
            }
            fixed_ops
        }
    };

    let mut merged_blocks: Vec<BlockNode> = Vec::new();

    for op in &ops {
        match op {
            DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => {
                for i in 0..*len {
                    let old_block = &cell.blocks[old_index + i];
                    let new_block = &new_cell.blocks[new_index + i];

                    // Safety net: if fingerprint matched but numbering ilvl or num_id
                    // differs, the alignment paired paragraphs at different structural
                    // positions. Reclassify as Delete + Insert.
                    if let (BlockNode::Paragraph(op), BlockNode::Paragraph(np)) =
                        (old_block, new_block)
                    {
                        let old_ilvl = op.numbering.as_ref().map(|n| n.ilvl);
                        let new_ilvl = np.numbering.as_ref().map(|n| n.ilvl);
                        let old_num_id = op.numbering.as_ref().map(|n| n.num_id);
                        let new_num_id = np.numbering.as_ref().map(|n| n.num_id);
                        if old_ilvl != new_ilvl || old_num_id != new_num_id {
                            let mut del = old_block.clone();
                            if let BlockNode::Paragraph(p) = &mut del {
                                mark_paragraph_deleted(p, revision, rev_counter);
                            }
                            merged_blocks.push(del);
                            let mut ins = new_block.clone();
                            if let BlockNode::Paragraph(p) = &mut ins {
                                mark_paragraph_inserted(p, revision, rev_counter);
                            }
                            merged_blocks.push(ins);
                            continue;
                        }
                    }

                    match (old_block, new_block) {
                        (BlockNode::Paragraph(old_p), BlockNode::Paragraph(new_p)) => {
                            let mut para = old_p.clone();
                            let ctx = format!("{context}:cell:{}:para:{}", cell.id.0, old_p.id.0);
                            apply_paragraph_diff_in_cell(
                                &mut para,
                                new_p,
                                old_block,
                                new_block,
                                revision,
                                rev_counter,
                                &ctx,
                            )?;
                            merged_blocks.push(BlockNode::Paragraph(para));
                        }
                        (BlockNode::Table(old_t), BlockNode::Table(new_t)) => {
                            let mut inner = old_t.clone();
                            if let Some(nested_diff) =
                                diff_nested_tables(old_t, new_t, 0).map_err(|e| MergeError {
                                    message: format!("nested table diff failed: {e}"),
                                    context: format!(
                                        "{context}:cell:{}:table:{}",
                                        cell.id.0, old_t.id.0
                                    ),
                                })?
                            {
                                match &nested_diff.diff {
                                    NestedTableDiffKind::StructureChanged {
                                        table_diff: ntd,
                                        new_table: nt,
                                    } => {
                                        apply_nested_table_structure_changed(
                                            &mut inner,
                                            nt,
                                            ntd,
                                            revision,
                                            rev_counter,
                                            context,
                                        )?;
                                    }
                                    NestedTableDiffKind::CellsModified { cell_changes: nc } => {
                                        apply_nested_table_cells_modified(
                                            &mut inner,
                                            nc,
                                            revision,
                                            rev_counter,
                                            context,
                                        )?;
                                    }
                                }
                            }
                            merged_blocks.push(BlockNode::Table(inner));
                        }
                        _ => {
                            // Same fingerprint but different block types shouldn't
                            // happen; keep old block unchanged.
                            merged_blocks.push(old_block.clone());
                        }
                    }
                }
            }
            DiffOp::Delete {
                old_index, old_len, ..
            } => {
                for i in 0..*old_len {
                    let mut block = cell.blocks[old_index + i].clone();
                    match &mut block {
                        BlockNode::Paragraph(p) => mark_paragraph_deleted(p, revision, rev_counter),
                        BlockNode::Table(t) => mark_table_deleted(t, revision, rev_counter),
                        BlockNode::OpaqueBlock(_) => {}
                    }
                    merged_blocks.push(block);
                }
            }
            DiffOp::Insert {
                new_index, new_len, ..
            } => {
                for i in 0..*new_len {
                    let mut block = new_cell.blocks[new_index + i].clone();
                    match &mut block {
                        BlockNode::Paragraph(p) => {
                            mark_paragraph_inserted(p, revision, rev_counter)
                        }
                        BlockNode::Table(t) => mark_table_inserted(t, revision, rev_counter),
                        BlockNode::OpaqueBlock(_) => {}
                    }
                    merged_blocks.push(block);
                }
            }
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                for i in 0..*old_len {
                    let mut block = cell.blocks[old_index + i].clone();
                    match &mut block {
                        BlockNode::Paragraph(p) => mark_paragraph_deleted(p, revision, rev_counter),
                        BlockNode::Table(t) => mark_table_deleted(t, revision, rev_counter),
                        BlockNode::OpaqueBlock(_) => {}
                    }
                    merged_blocks.push(block);
                }
                for i in 0..*new_len {
                    let mut block = new_cell.blocks[new_index + i].clone();
                    match &mut block {
                        BlockNode::Paragraph(p) => {
                            mark_paragraph_inserted(p, revision, rev_counter)
                        }
                        BlockNode::Table(t) => mark_table_inserted(t, revision, rev_counter),
                        BlockNode::OpaqueBlock(_) => {}
                    }
                    merged_blocks.push(block);
                }
            }
        }
    }

    cell.blocks = merged_blocks;
    Ok(())
}

/// Mark all content within a cell as deleted tracked segments.
///
/// Handles both paragraphs (wrapping inline content in Deleted segments)
/// and nested tables (marking all rows and their cells as Deleted, recursively).
pub(crate) fn mark_cell_content_deleted(
    cell: &mut TableCellNode,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
) {
    // A cell always retains a structural final paragraph (`CT_Tc`, §17.4.66,
    // requires block content), so its LAST paragraph mark can never be
    // tracked-deleted: there is no paragraph across the cell boundary to merge
    // into, and real Word never emits `w:pPr/w:rPr/w:del` on a deleted cell's
    // final paragraph (see the `deleted_table_row` cross-path fixture — the
    // cell content is `w:del`, the paragraph mark is not). Minting one produces
    // a POISON marker with no faithful single wire encoding: after a selective
    // resolution strands it (e.g. rejecting the row marker + cell content but
    // not this mark), `serialize`→`normalize_docx` (the wire accept) keeps the
    // surviving paragraph while `project(AcceptAll)` (the model accept) resolves
    // it differently — the wire/model accept divergence. So skip the final
    // paragraph mark. Interior paragraph marks (multi-paragraph cells) are still
    // deletable — those joins ARE wire-representable and agree across paths.
    let last_para_idx = cell
        .blocks
        .iter()
        .rposition(|b| matches!(b, BlockNode::Paragraph(_)));
    for (idx, block) in cell.blocks.iter_mut().enumerate() {
        match block {
            BlockNode::Paragraph(p) => {
                // Convert all existing segments to deleted.
                let all_inlines: Vec<InlineNode> =
                    p.segments.iter().flat_map(|s| s.inlines.clone()).collect();
                if !all_inlines.is_empty() {
                    p.segments = vec![TrackedSegment {
                        status: TrackingStatus::Deleted(next_revision(revision, rev_counter)),
                        inlines: all_inlines,
                    }];
                }
                if Some(idx) != last_para_idx {
                    p.para_mark_status = Some(TrackingStatus::Deleted(next_revision(
                        revision,
                        rev_counter,
                    )));
                }
            }
            BlockNode::Table(t) => {
                // A nested table inside a deleted cell is deleted the SAME way a
                // top-level table is: the row-level `w:trPr/w:del` marker plus each
                // cell's content — and deliberately NO per-cell `w:cellDel` (see
                // `mark_whole_row_deleted`). Minting a per-cell `cellDel` here would
                // reintroduce the cell-less-row hazard the top-level fix closed:
                // selectively resolving one nested cell's `cellDel` without its row
                // marker drops the cell out of a surviving row, and the serializer
                // refuses the resulting `<w:tr>` with zero `<w:tc>` (CT_Row, §17.4.72).
                mark_table_deleted(t, revision, rev_counter);
            }
            BlockNode::OpaqueBlock(_) => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_story_changes<T>(
    base_stories: &mut Vec<T>,
    target_stories: &[T],
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    changes: &[DiffChange],
    story_name: &str,
    get_key: impl Fn(&T) -> &str,
    set_key: impl Fn(&mut T, String),
    get_blocks_mut: impl Fn(&mut T) -> &mut Vec<TrackedBlock>,
    get_blocks: impl Fn(&T) -> &Vec<TrackedBlock>,
) -> Result<(), MergeError>
where
    T: Clone,
{
    fn story_will_move_away(
        pending_renames: &[(usize, String)],
        story_idx: usize,
        part_name: &str,
    ) -> bool {
        pending_renames
            .iter()
            .any(|(idx, target)| *idx == story_idx && target != part_name)
    }

    let mut pending_renames: Vec<(usize, String)> = Vec::new();

    for change in changes {
        match change {
            DiffChange::HeaderModified {
                base_part_name,
                target_part_name,
                block_changes,
                ..
            } => {
                if story_name != "header" {
                    continue;
                }
                let Some(story_idx) = base_stories
                    .iter()
                    .position(|s| get_key(s) == base_part_name)
                else {
                    return Err(MergeError {
                        message: format!("{story_name} story part not found for modification"),
                        context: base_part_name.clone(),
                    });
                };
                let story = &mut base_stories[story_idx];
                apply_changes_to_blocks(
                    get_blocks_mut(story),
                    block_changes,
                    revision,
                    rev_counter,
                    &format!("{story_name}:{base_part_name}"),
                    None,
                    &mut BlockProvenanceMap::new(),
                )?;
                pending_renames.push((story_idx, target_part_name.clone()));
            }
            DiffChange::FooterModified {
                base_part_name,
                target_part_name,
                block_changes,
                ..
            } => {
                if story_name != "footer" {
                    continue;
                }
                let Some(story_idx) = base_stories
                    .iter()
                    .position(|s| get_key(s) == base_part_name)
                else {
                    return Err(MergeError {
                        message: format!("{story_name} story part not found for modification"),
                        context: base_part_name.clone(),
                    });
                };
                let story = &mut base_stories[story_idx];
                apply_changes_to_blocks(
                    get_blocks_mut(story),
                    block_changes,
                    revision,
                    rev_counter,
                    &format!("{story_name}:{base_part_name}"),
                    None,
                    &mut BlockProvenanceMap::new(),
                )?;
                pending_renames.push((story_idx, target_part_name.clone()));
            }
            DiffChange::HeaderDeleted { part_name, .. } => {
                if story_name != "header" {
                    continue;
                }
                let Some(story) = base_stories.iter_mut().find(|s| get_key(s) == part_name) else {
                    return Err(MergeError {
                        message: format!("{story_name} story part not found for deletion"),
                        context: part_name.clone(),
                    });
                };
                for block in get_blocks_mut(story).iter_mut() {
                    block.status = TrackingStatus::Deleted(next_revision(revision, rev_counter));
                    if let BlockNode::Paragraph(p) = &mut block.block {
                        p.para_mark_status = Some(TrackingStatus::Deleted(next_revision(
                            revision,
                            rev_counter,
                        )));
                    }
                }
            }
            DiffChange::FooterDeleted { part_name, .. } => {
                if story_name != "footer" {
                    continue;
                }
                let Some(story) = base_stories.iter_mut().find(|s| get_key(s) == part_name) else {
                    return Err(MergeError {
                        message: format!("{story_name} story part not found for deletion"),
                        context: part_name.clone(),
                    });
                };
                for block in get_blocks_mut(story).iter_mut() {
                    block.status = TrackingStatus::Deleted(next_revision(revision, rev_counter));
                    if let BlockNode::Paragraph(p) = &mut block.block {
                        p.para_mark_status = Some(TrackingStatus::Deleted(next_revision(
                            revision,
                            rev_counter,
                        )));
                    }
                }
            }
            DiffChange::HeaderInserted { part_name, .. } => {
                if story_name != "header" {
                    continue;
                }
                let Some(target_story) = target_stories.iter().find(|s| get_key(s) == part_name)
                else {
                    return Err(MergeError {
                        message: format!(
                            "{story_name} story part not found in target for insertion"
                        ),
                        context: part_name.clone(),
                    });
                };
                let inserted_blocks: Vec<TrackedBlock> = get_blocks(target_story)
                    .iter()
                    .map(|tb| TrackedBlock {
                        status: TrackingStatus::Inserted(next_revision(revision, rev_counter)),
                        block: tb.block.clone(),
                        move_id: None,
                        block_sdt_wrap: None,
                    })
                    .collect();
                let reusable_story_idx = base_stories
                    .iter()
                    .enumerate()
                    .find(|(idx, s)| {
                        get_key(s) == part_name
                            && !story_will_move_away(&pending_renames, *idx, part_name)
                    })
                    .map(|(idx, _)| idx);
                if let Some(story_idx) = reusable_story_idx {
                    let base_story = &mut base_stories[story_idx];
                    *get_blocks_mut(base_story) = inserted_blocks;
                } else {
                    let mut inserted_story = target_story.clone();
                    *get_blocks_mut(&mut inserted_story) = inserted_blocks;
                    base_stories.push(inserted_story);
                }
            }
            DiffChange::FooterInserted { part_name, .. } => {
                if story_name != "footer" {
                    continue;
                }
                let Some(target_story) = target_stories.iter().find(|s| get_key(s) == part_name)
                else {
                    return Err(MergeError {
                        message: format!(
                            "{story_name} story part not found in target for insertion"
                        ),
                        context: part_name.clone(),
                    });
                };
                let inserted_blocks: Vec<TrackedBlock> = get_blocks(target_story)
                    .iter()
                    .map(|tb| TrackedBlock {
                        status: TrackingStatus::Inserted(next_revision(revision, rev_counter)),
                        block: tb.block.clone(),
                        move_id: None,
                        block_sdt_wrap: None,
                    })
                    .collect();
                let reusable_story_idx = base_stories
                    .iter()
                    .enumerate()
                    .find(|(idx, s)| {
                        get_key(s) == part_name
                            && !story_will_move_away(&pending_renames, *idx, part_name)
                    })
                    .map(|(idx, _)| idx);
                if let Some(story_idx) = reusable_story_idx {
                    let base_story = &mut base_stories[story_idx];
                    *get_blocks_mut(base_story) = inserted_blocks;
                } else {
                    let mut inserted_story = target_story.clone();
                    *get_blocks_mut(&mut inserted_story) = inserted_blocks;
                    base_stories.push(inserted_story);
                }
            }
            // Block/table changes never appear at this level: they're
            // nested inside `HeaderModified`/`FooterModified.block_changes`
            // and applied via the `apply_changes_to_blocks` call above.
            // Footnote/endnote/comment stories are applied by the sibling
            // `apply_note_changes`. Enumerated explicitly (not `_`) so a
            // future `DiffChange` variant fails to compile here instead of
            // silently no-op'ing.
            DiffChange::BlockDeleted { .. }
            | DiffChange::BlockInserted { .. }
            | DiffChange::BlockModified { .. }
            | DiffChange::TableStructureChanged { .. }
            | DiffChange::TableCellsModified { .. }
            | DiffChange::FootnoteModified { .. }
            | DiffChange::FootnoteDeleted { .. }
            | DiffChange::FootnoteInserted { .. }
            | DiffChange::EndnoteModified { .. }
            | DiffChange::EndnoteDeleted { .. }
            | DiffChange::EndnoteInserted { .. }
            | DiffChange::CommentModified { .. }
            | DiffChange::CommentDeleted { .. }
            | DiffChange::CommentInserted { .. } => {}
        }
    }

    for (story_idx, to) in pending_renames {
        let Some(story) = base_stories.get_mut(story_idx) else {
            return Err(MergeError {
                message: format!("{story_name} story index not found for rename"),
                context: story_idx.to_string(),
            });
        };
        if get_key(story) != to {
            set_key(story, to);
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn apply_note_changes<T>(
    base_stories: &mut Vec<T>,
    target_stories: &[T],
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    changes: &[DiffChange],
    story_name: &str,
    get_id: impl Fn(&T) -> &str,
    get_blocks_mut: impl Fn(&mut T) -> &mut Vec<TrackedBlock>,
    get_blocks: impl Fn(&T) -> &Vec<TrackedBlock>,
) -> Result<(), MergeError>
where
    T: Clone,
{
    for change in changes {
        match change {
            DiffChange::FootnoteModified {
                id, block_changes, ..
            } => {
                if story_name != "footnote" {
                    continue;
                }
                let Some(story) = base_stories.iter_mut().find(|s| get_id(s) == id) else {
                    return Err(MergeError {
                        message: format!("{story_name} not found for modification"),
                        context: id.clone(),
                    });
                };
                apply_changes_to_blocks(
                    get_blocks_mut(story),
                    block_changes,
                    revision,
                    rev_counter,
                    &format!("{story_name}:{id}"),
                    None,
                    &mut BlockProvenanceMap::new(),
                )?;
            }
            DiffChange::EndnoteModified {
                id, block_changes, ..
            } => {
                if story_name != "endnote" {
                    continue;
                }
                let Some(story) = base_stories.iter_mut().find(|s| get_id(s) == id) else {
                    return Err(MergeError {
                        message: format!("{story_name} not found for modification"),
                        context: id.clone(),
                    });
                };
                apply_changes_to_blocks(
                    get_blocks_mut(story),
                    block_changes,
                    revision,
                    rev_counter,
                    &format!("{story_name}:{id}"),
                    None,
                    &mut BlockProvenanceMap::new(),
                )?;
            }
            DiffChange::CommentModified {
                id, block_changes, ..
            } => {
                if story_name != "comment" {
                    continue;
                }
                let Some(story) = base_stories.iter_mut().find(|s| get_id(s) == id) else {
                    return Err(MergeError {
                        message: format!("{story_name} not found for modification"),
                        context: id.clone(),
                    });
                };
                apply_changes_to_blocks(
                    get_blocks_mut(story),
                    block_changes,
                    revision,
                    rev_counter,
                    &format!("{story_name}:{id}"),
                    None,
                    &mut BlockProvenanceMap::new(),
                )?;
            }
            DiffChange::FootnoteDeleted { id, .. } => {
                if story_name != "footnote" {
                    continue;
                }
                let Some(story) = base_stories.iter_mut().find(|s| get_id(s) == id) else {
                    return Err(MergeError {
                        message: format!("{story_name} not found for deletion"),
                        context: id.clone(),
                    });
                };
                for block in get_blocks_mut(story).iter_mut() {
                    block.status = TrackingStatus::Deleted(next_revision(revision, rev_counter));
                    if let BlockNode::Paragraph(p) = &mut block.block {
                        p.para_mark_status = Some(TrackingStatus::Deleted(next_revision(
                            revision,
                            rev_counter,
                        )));
                    }
                }
            }
            DiffChange::EndnoteDeleted { id, .. } => {
                if story_name != "endnote" {
                    continue;
                }
                let Some(story) = base_stories.iter_mut().find(|s| get_id(s) == id) else {
                    return Err(MergeError {
                        message: format!("{story_name} not found for deletion"),
                        context: id.clone(),
                    });
                };
                for block in get_blocks_mut(story).iter_mut() {
                    block.status = TrackingStatus::Deleted(next_revision(revision, rev_counter));
                    if let BlockNode::Paragraph(p) = &mut block.block {
                        p.para_mark_status = Some(TrackingStatus::Deleted(next_revision(
                            revision,
                            rev_counter,
                        )));
                    }
                }
            }
            DiffChange::CommentDeleted { .. } => {
                // Handled at the call site (merge_diff) by setting
                // CommentStory::tracking_status, not by marking blocks.
            }
            DiffChange::FootnoteInserted { id, .. } => {
                if story_name != "footnote" {
                    continue;
                }
                let Some(target_story) = target_stories.iter().find(|s| get_id(s) == id) else {
                    return Err(MergeError {
                        message: format!("{story_name} not found in target for insertion"),
                        context: id.clone(),
                    });
                };
                let inserted_blocks: Vec<TrackedBlock> = get_blocks(target_story)
                    .iter()
                    .map(|tb| TrackedBlock {
                        status: TrackingStatus::Inserted(next_revision(revision, rev_counter)),
                        block: tb.block.clone(),
                        move_id: None,
                        block_sdt_wrap: None,
                    })
                    .collect();
                if let Some(base_story) = base_stories.iter_mut().find(|s| get_id(s) == id) {
                    *get_blocks_mut(base_story) = inserted_blocks;
                } else {
                    let mut inserted_story = target_story.clone();
                    *get_blocks_mut(&mut inserted_story) = inserted_blocks;
                    base_stories.push(inserted_story);
                }
            }
            DiffChange::EndnoteInserted { id, .. } => {
                if story_name != "endnote" {
                    continue;
                }
                let Some(target_story) = target_stories.iter().find(|s| get_id(s) == id) else {
                    return Err(MergeError {
                        message: format!("{story_name} not found in target for insertion"),
                        context: id.clone(),
                    });
                };
                let inserted_blocks: Vec<TrackedBlock> = get_blocks(target_story)
                    .iter()
                    .map(|tb| TrackedBlock {
                        status: TrackingStatus::Inserted(next_revision(revision, rev_counter)),
                        block: tb.block.clone(),
                        move_id: None,
                        block_sdt_wrap: None,
                    })
                    .collect();
                if let Some(base_story) = base_stories.iter_mut().find(|s| get_id(s) == id) {
                    *get_blocks_mut(base_story) = inserted_blocks;
                } else {
                    let mut inserted_story = target_story.clone();
                    *get_blocks_mut(&mut inserted_story) = inserted_blocks;
                    base_stories.push(inserted_story);
                }
            }
            DiffChange::CommentInserted { id, .. } => {
                if story_name != "comment" {
                    continue;
                }
                let Some(target_story) = target_stories.iter().find(|s| get_id(s) == id) else {
                    return Err(MergeError {
                        message: format!("{story_name} not found in target for insertion"),
                        context: id.clone(),
                    });
                };
                let inserted_blocks: Vec<TrackedBlock> = get_blocks(target_story)
                    .iter()
                    .map(|tb| TrackedBlock {
                        status: TrackingStatus::Inserted(next_revision(revision, rev_counter)),
                        block: tb.block.clone(),
                        move_id: None,
                        block_sdt_wrap: None,
                    })
                    .collect();
                if let Some(base_story) = base_stories.iter_mut().find(|s| get_id(s) == id) {
                    *get_blocks_mut(base_story) = inserted_blocks;
                } else {
                    let mut inserted_story = target_story.clone();
                    *get_blocks_mut(&mut inserted_story) = inserted_blocks;
                    base_stories.push(inserted_story);
                }
            }
            // Block/table changes never appear at this level: they're
            // nested inside `Footnote`/`Endnote`/`CommentModified.block_changes`
            // and applied via the `apply_changes_to_blocks` call above.
            // Header/footer stories are applied by the sibling
            // `apply_story_changes`. Enumerated explicitly (not `_`) so a
            // future `DiffChange` variant fails to compile here instead of
            // silently no-op'ing.
            DiffChange::BlockDeleted { .. }
            | DiffChange::BlockInserted { .. }
            | DiffChange::BlockModified { .. }
            | DiffChange::TableStructureChanged { .. }
            | DiffChange::TableCellsModified { .. }
            | DiffChange::HeaderModified { .. }
            | DiffChange::HeaderDeleted { .. }
            | DiffChange::HeaderInserted { .. }
            | DiffChange::FooterModified { .. }
            | DiffChange::FooterDeleted { .. }
            | DiffChange::FooterInserted { .. } => {}
        }
    }
    Ok(())
}

pub fn merge_diff(
    base: &CanonDoc,
    target: &CanonDoc,
    diff: &crate::domain::DocumentDiff,
    revision: &RevisionInfo,
) -> Result<MergeResult, MergeError> {
    // Single counter for the entire merge operation. Every tracked change
    // element (w:ins, w:del, w:moveFrom, w:moveTo) must receive a unique
    // revision ID (ISO 29500-1 §17.13.5). Starting from revision.revision_id
    // and threading this counter through all sub-functions prevents reuse.
    let mut rev_counter = revision.revision_id;

    let mut merged = base.clone();
    let mut provenance = BlockProvenanceMap::new();
    let mut target_tables_by_id: HashMap<String, BlockNode> = HashMap::new();
    for tracked in &target.blocks {
        if let BlockNode::Table(table) = &tracked.block {
            target_tables_by_id.insert(table.id.0.to_string(), BlockNode::Table(table.clone()));
        }
    }
    apply_changes_to_blocks(
        &mut merged.blocks,
        &diff.changes,
        revision,
        &mut rev_counter,
        "body",
        Some(&mut target_tables_by_id),
        &mut provenance,
    )?;
    // The document-final paragraph mark can never carry a resolvable tracked
    // insertion/deletion (Word leaves it pending on accept-all): re-attribute a
    // trailing append/delete to the preceding mark. Body only.
    normalize_final_mark_attribution(&mut merged.blocks, revision, &mut rev_counter);

    apply_story_changes(
        &mut merged.headers,
        &target.headers,
        revision,
        &mut rev_counter,
        &diff.changes,
        "header",
        |story: &HeaderStory| &story.part_name,
        |story: &mut HeaderStory, part_name| story.part_name = part_name,
        |story: &mut HeaderStory| &mut story.blocks,
        |story: &HeaderStory| &story.blocks,
    )?;
    apply_story_changes(
        &mut merged.footers,
        &target.footers,
        revision,
        &mut rev_counter,
        &diff.changes,
        "footer",
        |story: &FooterStory| &story.part_name,
        |story: &mut FooterStory, part_name| story.part_name = part_name,
        |story: &mut FooterStory| &mut story.blocks,
        |story: &FooterStory| &story.blocks,
    )?;
    apply_note_changes(
        &mut merged.footnotes,
        &target.footnotes,
        revision,
        &mut rev_counter,
        &diff.changes,
        "footnote",
        |story: &FootnoteStory| &story.id,
        |story: &mut FootnoteStory| &mut story.blocks,
        |story: &FootnoteStory| &story.blocks,
    )?;
    apply_note_changes(
        &mut merged.endnotes,
        &target.endnotes,
        revision,
        &mut rev_counter,
        &diff.changes,
        "endnote",
        |story: &EndnoteStory| &story.id,
        |story: &mut EndnoteStory| &mut story.blocks,
        |story: &EndnoteStory| &story.blocks,
    )?;
    apply_note_changes(
        &mut merged.comments,
        &target.comments,
        revision,
        &mut rev_counter,
        &diff.changes,
        "comment",
        |story: &CommentStory| &story.id,
        |story: &mut CommentStory| &mut story.blocks,
        |story: &CommentStory| &story.blocks,
    )?;
    // Mark deleted comment stories at the story level (not block level) so
    // accept_all removes them without causing w:del/w:delText in serialized XML.
    for change in &diff.changes {
        if let DiffChange::CommentDeleted { id, .. } = change
            && let Some(story) = merged.comments.iter_mut().find(|s| s.id == *id)
        {
            story.tracking_status = Some(TrackingStatus::Deleted(next_revision(
                revision,
                &mut rev_counter,
            )));
        }
    }

    // Post-processing: fix numbering drift for Normal paragraphs whose auto-
    // numbering prefix shifted due to insertions/deletions between base and
    // target. Uses the diff changes to find each merged block's target
    // counterpart by ID rather than positional walk.
    fix_numbering_drift_for_normal_blocks(
        &mut merged.blocks,
        &diff.changes,
        revision,
        &mut rev_counter,
    );

    // Materialize numbering in story blocks (headers, footers, footnotes,
    // endnotes) so w:numPr never survives into serialized redline XML.
    materialize_numbering_in_story_blocks(&mut merged);

    // Track section property changes (w:sectPrChange §17.13.5.32).
    //
    // Clear the base's parsed sectPrChange — if the base archive already had one,
    // it lives inside the raw sectPr element and will be preserved opaquely by the
    // serializer when we don't set a new change here.
    merged.body_section_property_change = None;

    // When the target's body section properties differ from the base, record
    // the change so the serializer can emit a sectPrChange element.
    if base.body_section_properties != target.body_section_properties {
        if let Some(ref base_sp) = base.body_section_properties {
            // CT_SectPrBase does not include EG_HdrFtrReferences, so
            // header/footer refs are excluded when freezing the previous
            // state for a sectPrChange.
            let mut sp_for_change = base_sp.clone();
            sp_for_change.header_refs.clear();
            sp_for_change.footer_refs.clear();
            let element =
                crate::runtime::section_properties_to_element(&sp_for_change, None, None, None);
            let mut previous_properties_raw = Vec::new();
            let config = xmltree::EmitterConfig::new().write_document_declaration(false);
            let _ = element.write_with_config(&mut previous_properties_raw, config);
            merged.body_section_properties = target.body_section_properties.clone();
            merged.body_section_property_change = Some(SectionPropertyChange {
                revision: next_revision(revision, &mut rev_counter),
                previous_properties_raw,
            });
        } else if target.body_section_properties.is_some() {
            // Base had no section properties but target does — use target's values.
            // No previous properties to record (sectPrChange requires a previous state).
            merged.body_section_properties = target.body_section_properties.clone();
        }
    }

    // H2: one unified body-state validator after the diff/redline merge
    // producer (post normalize_final_mark_attribution).
    debug_assert_body_invariants(&merged, "merge_diff");
    Ok(MergeResult {
        doc: merged,
        block_provenance: provenance,
    })
}

/// Process numbering prefixes in story blocks (headers, footers,
/// footnotes, endnotes) that contain tracked changes.  Structural `w:numPr`
/// is preserved; only `literal_prefix` text is materialized as inline content.
/// Stories without any inserted/deleted blocks are left untouched.
fn materialize_numbering_in_story_blocks(doc: &mut CanonDoc) {
    fn has_tracked_changes(blocks: &[TrackedBlock]) -> bool {
        blocks
            .iter()
            .any(|b| !matches!(b.status, TrackingStatus::Normal))
    }

    fn materialize_blocks(blocks: &mut [TrackedBlock]) {
        for tb in blocks.iter_mut() {
            materialize_numbering_prefix_in_place(&mut tb.block);
        }
    }

    for story in &mut doc.headers {
        if has_tracked_changes(&story.blocks) {
            materialize_blocks(&mut story.blocks);
        }
    }
    for story in &mut doc.footers {
        if has_tracked_changes(&story.blocks) {
            materialize_blocks(&mut story.blocks);
        }
    }
    for story in &mut doc.footnotes {
        if has_tracked_changes(&story.blocks) {
            materialize_blocks(&mut story.blocks);
        }
    }
    for story in &mut doc.endnotes {
        if has_tracked_changes(&story.blocks) {
            materialize_blocks(&mut story.blocks);
        }
    }
}

/// Sync paragraph-level properties from target, excluding numbering,
/// literal_prefix, and inline formatting.
///
/// Used for BlockModified paragraphs that were already processed by
/// `apply_block_modified`. Those paragraphs have:
/// - Correct numbering state (cleared if prefix was materialized)
/// - Correct inline segments (with tracked changes from the inline diff)
///
/// Re-syncing numbering would undo prefix materialization (double prefix).
/// Re-syncing inline formatting would corrupt tracked segments (the
/// character-by-character walk assumes both sides have identical text).
fn sync_non_numbering_properties(para: &mut ParagraphNode, target_para: &ParagraphNode) {
    para.style_id = target_para.style_id.clone();
    para.align = target_para.align.clone();
    para.has_direct_align = target_para.has_direct_align;
    para.indent = target_para.indent.clone();
    para.has_direct_indent = target_para.has_direct_indent;
    para.spacing = target_para.spacing.clone();
    para.has_direct_spacing = target_para.has_direct_spacing;
    para.borders = target_para.borders.clone();
    para.keep_next = target_para.keep_next;
    para.keep_lines = target_para.keep_lines;
    para.page_break_before = target_para.page_break_before;
    para.widow_control = target_para.widow_control;
    para.contextual_spacing = target_para.contextual_spacing;
    para.shading = target_para.shading.clone();
    para.tab_stops = target_para.tab_stops.clone();
    para.heading_level = target_para.heading_level.clone();
    para.mirror_indents = target_para.mirror_indents;
    para.bidi = target_para.bidi;
    para.text_alignment = target_para.text_alignment.clone();
    para.text_direction = target_para.text_direction.clone();
    para.suppress_auto_hyphens = target_para.suppress_auto_hyphens;
    para.snap_to_grid = target_para.snap_to_grid;
    para.overflow_punct = target_para.overflow_punct;
    para.adjust_right_ind = target_para.adjust_right_ind;
    para.word_wrap = target_para.word_wrap;
    para.frame_pr = target_para.frame_pr.clone();
    para.cnf_style = target_para.cnf_style.clone();
    para.section_property_change = target_para.section_property_change.clone();
    para.section_properties = target_para.section_properties.clone();
    para.paragraph_mark_marks = target_para.paragraph_mark_marks.clone();
    para.paragraph_mark_style_props = target_para.paragraph_mark_style_props.clone();
    para.paragraph_mark_rpr_off = target_para.paragraph_mark_rpr_off;
    para.auto_space_de = target_para.auto_space_de;
    para.auto_space_dn = target_para.auto_space_dn;
    // Numbering structure (num_id, ilvl) intentionally NOT synced —
    // apply_block_modified already set them correctly and may have
    // intentionally cleared numbering during prefix materialization.
    // However, sync the counter value (synthesized_text) which can
    // differ even for structurally-identical numbering because it
    // depends on the paragraph's position in the numbering sequence.
    if let (Some(num), Some(target_num)) = (&mut para.numbering, &target_para.numbering) {
        num.synthesized_text = target_num.synthesized_text.clone();
    }

    // Keep visible text runs aligned with the target's formatting even when
    // the paragraph also contains Deleted segments. `sync_inline_formatting`
    // skips Deleted text and bails out if visible character counts diverge.
    sync_inline_formatting(&mut para.segments, &target_para.segments);
}

/// Sync inline formatting from target segments onto base segments.
///
/// Walks both segment lists in parallel by character offset. For each character
/// position, looks up the target TextNode's marks and style_props and applies
/// them to the base TextNode. When run boundaries differ between base and target,
/// the base keeps its run structure but each run gets the target formatting for
/// its character range.
fn sync_inline_formatting(
    base_segments: &mut [TrackedSegment],
    target_segments: &[TrackedSegment],
) {
    // Build a flat list of (marks, style_props, has_direct_*) from target text nodes.
    struct TargetFmt {
        text: String,
        marks: Vec<Mark>,
        style_props: StyleProps,
        rpr_authored: crate::domain::RunRprAuthored,
    }
    let target_fmt: Vec<TargetFmt> = target_segments
        .iter()
        .flat_map(|seg| seg.inlines.iter())
        .filter_map(|inline| match inline {
            InlineNode::Text(t) => Some(TargetFmt {
                text: t.text.clone(),
                marks: t.marks.clone(),
                style_props: t.style_props.clone(),
                rpr_authored: t.rpr_authored,
            }),
            _ => None,
        })
        .collect();

    fn apply_target_fmt(text: &mut TextNode, fmt: &TargetFmt) {
        text.marks = fmt.marks.clone();
        text.style_props = fmt.style_props.clone();
        text.rpr_authored = fmt.rpr_authored;
    }

    fn split_prefix_chars(text: &str, chars: usize) -> (String, String) {
        if chars == 0 {
            return (String::new(), text.to_string());
        }
        let split_byte = text
            .char_indices()
            .nth(chars)
            .map(|(idx, _)| idx)
            .unwrap_or(text.len());
        (
            text[..split_byte].to_string(),
            text[split_byte..].to_string(),
        )
    }

    // If there's exactly one target text node and the visible base text is
    // identical, apply its formatting to the visible base text nodes.
    let base_visible_char_count: usize = base_segments
        .iter()
        .filter(|seg| matches!(seg.status, TrackingStatus::Normal))
        .flat_map(|seg| seg.inlines.iter())
        .filter_map(|inline| match inline {
            InlineNode::Text(t) => Some(t.text.chars().count()),
            _ => None,
        })
        .sum();

    if target_fmt.len() == 1
        && base_segments
            .iter()
            .all(|seg| matches!(seg.status, TrackingStatus::Normal))
        && base_visible_char_count == target_fmt[0].text.chars().count()
    {
        let fmt = &target_fmt[0];
        for seg in base_segments.iter_mut() {
            for inline in &mut seg.inlines {
                if let InlineNode::Text(t) = inline {
                    apply_target_fmt(t, fmt);
                }
            }
        }
        return;
    }

    // When run counts match AND text at each position matches, sync 1:1 by position.
    // Run boundaries can differ between merged (diff-engine tokens) and target (import-time
    // runs) even when overall text is identical. When counts match by coincidence but text
    // doesn't align, the 1:1 path would apply marks from the wrong target run. Fall through
    // to character-offset sync in that case.
    let base_texts: Vec<&str> = base_segments
        .iter()
        .flat_map(|seg| seg.inlines.iter())
        .filter_map(|inline| match inline {
            InlineNode::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect();

    let boundaries_align = base_texts.len() == target_fmt.len()
        && base_texts
            .iter()
            .zip(target_fmt.iter())
            .all(|(b, t)| *b == t.text);

    if boundaries_align {
        let mut target_idx = 0;
        for seg in base_segments.iter_mut() {
            for inline in &mut seg.inlines {
                if let InlineNode::Text(t) = inline
                    && target_idx < target_fmt.len()
                {
                    let fmt = &target_fmt[target_idx];
                    apply_target_fmt(t, fmt);
                    target_idx += 1;
                }
            }
        }
        return;
    }

    // Run counts differ — do character-offset based sync.
    // Build a per-character formatting map from target, then apply to base.
    let mut target_char_fmt: Vec<usize> = Vec::new(); // index into target_fmt for each char
    let mut fmt_idx = 0;
    for seg in target_segments.iter() {
        for inline in &seg.inlines {
            if let InlineNode::Text(t) = inline {
                for _ in t.text.chars() {
                    target_char_fmt.push(fmt_idx);
                }
                fmt_idx += 1;
            }
        }
    }

    // Only count characters from the accept-all projection (Normal + Inserted
    // segments). Deleted segments contain base text that has no target counterpart,
    // so including them would inflate the count and cause a mismatch bail-out.
    let base_accept_char_count: usize = base_segments
        .iter()
        .filter(|seg| !matches!(seg.status, TrackingStatus::Deleted(_)))
        .flat_map(|seg| seg.inlines.iter())
        .filter_map(|inline| match inline {
            InlineNode::Text(t) => Some(t.text.chars().count()),
            _ => None,
        })
        .sum();

    if base_accept_char_count != target_char_fmt.len() {
        // Character counts don't match even after excluding Deleted segments.
        // Bail out safely rather than corrupt formatting.
        return;
    }

    // Split visible base text nodes to the target's run boundaries and apply the
    // corresponding target formatting to each chunk. Deleted segments keep their
    // original text and formatting because they have no target counterpart.
    let mut target_idx = 0usize;
    let mut chars_consumed_in_target = 0usize;
    for seg in base_segments.iter_mut() {
        let is_deleted = matches!(seg.status, TrackingStatus::Deleted(_));
        if is_deleted {
            continue;
        }

        let mut rewritten = Vec::with_capacity(seg.inlines.len());
        for inline in std::mem::take(&mut seg.inlines) {
            match inline {
                InlineNode::Text(text) => {
                    let mut remaining = text.text.clone();
                    let mut chunk_index = 0usize;
                    while !remaining.is_empty() {
                        if target_idx >= target_fmt.len() {
                            seg.inlines = rewritten;
                            return;
                        }
                        let fmt = &target_fmt[target_idx];
                        let fmt_len = fmt.text.chars().count();
                        let remaining_in_fmt = fmt_len.saturating_sub(chars_consumed_in_target);
                        if remaining_in_fmt == 0 {
                            target_idx += 1;
                            chars_consumed_in_target = 0;
                            continue;
                        }

                        let remaining_chars = remaining.chars().count();
                        let take_chars = remaining_chars.min(remaining_in_fmt);
                        let (chunk_text, tail) = split_prefix_chars(&remaining, take_chars);
                        remaining = tail;

                        let mut chunk = text.clone();
                        chunk.text = chunk_text;
                        if chunk_index > 0 {
                            chunk.id = NodeId::from(format!("{}__fmt{}", text.id, chunk_index));
                            chunk.formatting_change = None;
                        }
                        apply_target_fmt(&mut chunk, fmt);
                        rewritten.push(InlineNode::Text(chunk));

                        chars_consumed_in_target += take_chars;
                        if chars_consumed_in_target == fmt_len {
                            target_idx += 1;
                            chars_consumed_in_target = 0;
                        }
                        chunk_index += 1;
                    }
                }
                other => rewritten.push(other),
            }
        }
        seg.inlines = rewritten;
    }

    if target_idx != target_fmt.len() || chars_consumed_in_target != 0 {
        // Visible text did not fully align with the target text runs.
        // Preserve the partially-updated segments rather than guessing further.
        return;
    }

    // Sanity-check that the first character of each visible chunk still maps to
    // the same target formatting we consumed above. This keeps the previous
    // offset-based invariant explicit for debugging.
    let mut char_offset = 0usize;
    for seg in base_segments.iter_mut() {
        if matches!(seg.status, TrackingStatus::Deleted(_)) {
            continue;
        }
        for inline in &mut seg.inlines {
            if let InlineNode::Text(t) = inline {
                let len = t.text.chars().count();
                if len > 0 && char_offset < target_char_fmt.len() {
                    let target_fmt_idx = target_char_fmt[char_offset];
                    if target_fmt_idx < target_fmt.len() {
                        apply_target_fmt(t, &target_fmt[target_fmt_idx]);
                    }
                }
                char_offset += len;
            }
        }
    }
}

/// Sync target formatting for paragraphs inside table cells.
///
/// Walks base and target tables in parallel (rows → cells → blocks) and calls
/// `sync_target_formatting` for each matched paragraph pair. Gracefully skips
/// when structures don't align (different row/cell/block counts).
fn sync_target_formatting_in_table(table: &mut TableNode, target_table: &TableNode) {
    if table.rows.len() != target_table.rows.len() {
        return;
    }
    for (row, target_row) in table.rows.iter_mut().zip(target_table.rows.iter()) {
        if matches!(
            &row.tracking_status,
            Some(TrackingStatus::Inserted(_)) | Some(TrackingStatus::Deleted(_))
        ) {
            continue;
        }
        if row.cells.len() != target_row.cells.len() {
            continue;
        }
        for (cell, target_cell) in row.cells.iter_mut().zip(target_row.cells.iter()) {
            if matches!(
                &cell.tracking_status,
                Some(TrackingStatus::Inserted(_)) | Some(TrackingStatus::Deleted(_))
            ) {
                continue;
            }
            if cell.blocks.len() != target_cell.blocks.len() {
                continue;
            }
            for (block, target_block) in cell.blocks.iter_mut().zip(target_cell.blocks.iter()) {
                match (block, target_block) {
                    (BlockNode::Paragraph(para), BlockNode::Paragraph(target_para)) => {
                        // Use sync_non_numbering_properties (not sync_target_formatting)
                        // to avoid restoring numPr from the target when the base didn't
                        // have it. Cell-paragraph diffs are handled by
                        // apply_paragraph_diff_in_cell; non-modified paragraphs should
                        // keep their base numbering state.
                        sync_non_numbering_properties(para, target_para);
                    }
                    (BlockNode::Table(nested), BlockNode::Table(nested_target)) => {
                        sync_target_formatting_in_table(nested, nested_target);
                    }
                    _ => {}
                }
            }
        }
    }
}
/// Fix numbering drift for Normal (unchanged) paragraphs.
///
/// When paragraphs are inserted or deleted, auto-numbering on surrounding Normal
/// paragraphs may shift (e.g., section "3." becomes "4."). The diff doesn't emit
/// changes for these since the body text is identical. This function walks merged
/// blocks alongside target blocks to detect and materialize prefix differences as
/// tracked inline content.
fn fix_numbering_drift_for_normal_blocks(
    merged_blocks: &mut [TrackedBlock],
    changes: &[DiffChange],
    _revision: &RevisionInfo,
    _rev_counter: &mut u32,
) {
    // Build an ID-based lookup from diff changes: for each BlockModified or
    // TableStructureChanged/TableCellsModified, we know which target block
    // corresponds to each base block. This replaces the previous positional
    // walk which broke when paragraph splits shifted block indices.
    let mut target_block_by_id: HashMap<NodeId, BlockNode> = HashMap::new();
    for change in changes {
        if let DiffChange::BlockModified {
            block_id,
            new_block,
            ..
        } = change
        {
            target_block_by_id.insert(block_id.clone(), new_block.clone());
        }
    }

    for merged in merged_blocks.iter_mut() {
        // Skip OpaqueBlocks — they are not serialized into the redline
        // and don't carry numbering.
        if matches!(merged.block, BlockNode::OpaqueBlock(_)) {
            continue;
        }
        match &merged.status {
            TrackingStatus::Deleted(_) | TrackingStatus::InsertedThenDeleted(_) => {
                // Deleted (and stacked, which is pending-deleted) blocks have
                // no target counterpart. Materialize numbering prefix so the
                // serializer doesn't emit w:numPr (which extraction would
                // re-synthesize with a drifted counter in the reject view).
                materialize_numbering_prefix_in_place(&mut merged.block);
            }
            TrackingStatus::Inserted(_) => {
                // Inserted blocks are target blocks — materialize numbering
                // prefix so the serializer doesn't emit w:numPr.
                materialize_numbering_prefix_in_place(&mut merged.block);
            }
            TrackingStatus::Normal => {
                let block_id = match &merged.block {
                    BlockNode::Paragraph(p) => &p.id,
                    BlockNode::Table(t) => &t.id,
                    BlockNode::OpaqueBlock(o) => &o.id,
                };

                if let Some(target_block) = target_block_by_id.get(block_id) {
                    // BlockModified blocks were already processed by
                    // apply_block_modified (inline diffs, prefix materialization,
                    // formatting changes). Sync remaining paragraph properties
                    // that apply_block_modified doesn't cover, but do NOT
                    // overwrite numbering — apply_block_modified intentionally
                    // cleared it when materializing the prefix. Also skip
                    // sync_inline_formatting (it would corrupt post-merge
                    // tracked segments).
                    match (&mut merged.block, target_block) {
                        (BlockNode::Paragraph(para), BlockNode::Paragraph(target_para)) => {
                            sync_non_numbering_properties(para, target_para);
                        }
                        (BlockNode::Table(table), BlockNode::Table(target_table)) => {
                            sync_target_formatting_in_table(table, target_table);
                        }
                        _ => {}
                    }
                } else {
                    // Block was not modified by the diff — it's unchanged.
                    // Materialize literal_prefix as inline text (structural
                    // numbering is preserved for Word to synthesize).
                    materialize_numbering_prefix_in_place(&mut merged.block);
                }
            }
        }
    }
}

/// Materialize numbering prefix for a block in-place, without comparing to a
/// target.  Used for Deleted and Inserted blocks where there is no target
/// counterpart to compare against.
///
/// Structural numbering (`w:numPr`) is preserved — Word handles counter
/// synthesis, and `extract_redline`'s two-phase approach produces correct
/// numbers.  Only `literal_prefix` (baked text) is materialized as inline text.
fn materialize_numbering_prefix_in_place(block: &mut BlockNode) {
    match block {
        BlockNode::Paragraph(para) => {
            if para.numbering.is_none() && para.literal_prefix.is_none() {
                return;
            }
            // Structural numbering is preserved (including bullets).
            // Word generates the label from the numbering definition.
            if para.numbering.is_some() {
                return;
            }
            // Only literal_prefix remains — materialize it as inline text.
            let prefix = effective_text_prefix(para).map(str::to_owned);
            if let Some(pfx) = &prefix
                && !pfx.trim().is_empty()
            {
                let prefix_segment = TrackedSegment {
                    status: TrackingStatus::Normal,
                    inlines: vec![InlineNode::from(make_prefix_text_node(
                        materialized_prefix_node_id(&para.id, MaterializedPrefixKind::Structural),
                        MaterializedPrefixKind::Structural,
                        materialized_prefix_text(pfx, para),
                        para,
                    ))],
                };
                let mut new_segments = vec![prefix_segment];
                new_segments.append(&mut para.segments);
                para.segments = new_segments;
            }
            para.materialized_numbering = para.numbering.take();
            para.literal_prefix = None;
        }
        BlockNode::Table(table) => {
            materialize_numbering_in_table_cells(table);
        }
        BlockNode::OpaqueBlock(_) => {}
    }
}

/// Walk table cells and materialize `literal_prefix` numbering prefixes for
/// cell-level paragraphs.  Structural numbering (`w:numPr`) is preserved —
/// Word handles counter synthesis and `extract_redline`'s two-phase approach
/// produces correct numbers.
fn materialize_numbering_in_table_cells(table: &mut TableNode) {
    for row in &mut table.rows {
        if matches!(
            &row.tracking_status,
            Some(TrackingStatus::Inserted(_)) | Some(TrackingStatus::Deleted(_))
        ) {
            continue;
        }
        for cell in &mut row.cells {
            if matches!(
                &cell.tracking_status,
                Some(TrackingStatus::Inserted(_)) | Some(TrackingStatus::Deleted(_))
            ) {
                continue;
            }
            for block in &mut cell.blocks {
                let BlockNode::Paragraph(para) = block else {
                    continue;
                };
                if para.numbering.is_none() && para.literal_prefix.is_none() {
                    continue;
                }
                // Structural numbering is preserved.
                if para.numbering.is_some() {
                    continue;
                }
                // Only literal_prefix — materialize as inline text.
                let base_prefix = effective_text_prefix(para).map(str::to_owned);
                if let Some(pfx) = &base_prefix
                    && !pfx.trim().is_empty()
                {
                    let prefix_segment = TrackedSegment {
                        status: TrackingStatus::Normal,
                        inlines: vec![InlineNode::from(make_prefix_text_node(
                            materialized_prefix_node_id(
                                &para.id,
                                MaterializedPrefixKind::Structural,
                            ),
                            MaterializedPrefixKind::Structural,
                            materialized_prefix_text(pfx, para),
                            para,
                        ))],
                    };
                    let mut new_segments = vec![prefix_segment];
                    new_segments.append(&mut para.segments);
                    para.segments = new_segments;
                }
                para.numbering = None;
                para.literal_prefix = None;
            }
        }
    }
}

/// `preserve_formatting_change`: when true, the paragraph's `formatting_change`
/// (pPrChange) and `section_property_change` records are LEFT INTACT. This is
/// used only by the text-edit-prep projection
/// (`project_block_for_text_edit_prep`), which flattens tracked ins/del text to
/// give a new text edit a clean base WITHOUT accepting a prior tracked
/// paragraph-property change — accepting it would break tracked-change
/// reversibility (ECMA-376 §17.13.5.29). All accept/reject callers pass `false`
/// (the change record has already been resolved upstream, so clearing here is a
/// no-op for them).
/// Local name of a `customXml*Range{Start,End}` decoration, or `None`.
fn custom_xml_range_marker_local(deco: &crate::domain::DecorationNode) -> Option<String> {
    let raw = deco.raw_xml.as_deref()?;
    let el = crate::word_xml::parse_raw_fragment(raw).ok()?;
    let local = crate::import::local_element_name(&el);
    if local.starts_with("customXml")
        && (local.ends_with("RangeStart") || local.ends_with("RangeEnd"))
    {
        Some(local.to_string())
    } else {
        None
    }
}

/// Word's rule for what happens to a `customXml`/`smartTag` wrapper and the
/// `customXml*Range` markers around it when a tracked change is resolved
/// (accept or reject). The single named decision point for §17.13.5.6/.7,
/// matched to the markup real Word produces on resolve: the cases are mutually exclusive
/// and the same on BOTH accept and reject (the range marks the *wrapper markup
/// itself* as the revision, so resolving either way drops the transient
/// markup). See `spec_custom_xml_range_resolution_markup.rs` for the gold.
enum WrapperFate {
    /// PLAIN wrapper: no `customXml*Range` encloses it. Word keeps the wrapper
    /// verbatim on both resolutions; the renest pass re-wraps its content.
    /// (An *emptied* plain wrapper — content fully deleted by an inner `w:del`,
    /// e.g. the smartTag accept case — is handled downstream: the wrapper
    /// re-nests around no content and is dropped as empty, not here.)
    Keep,
    /// RANGE-GOVERNED wrapper: enclosed by a `customXmlIns/Del/MoveFrom/MoveToRange`
    /// pair. The range marks the wrapper markup as the revision, so resolving it
    /// (either way) drops the wrapper AND the range markers, leaving the inner
    /// content (already resolved by the revision machinery above).
    CollapseWithRange,
}

/// Accept/reject resolution for `customXml*Range`-governed wrappers
/// (§17.13.5.4-.11) — applies [`WrapperFate`] to every wrapper / range marker
/// in the paragraph's flat inline stream (across segments).
///
/// A `customXml*Range` that encloses NO wrapper (e.g. a table-level range over
/// rows) is left intact: there is no wrapper markup for it to resolve, and
/// dropping it has no well-defined meaning.
fn resolve_custom_xml_range_governed_wrappers(p: &mut ParagraphNode) {
    // Fast path: nothing to do unless the paragraph carries a customXml*Range
    // marker at all (the common case is none).
    let has_range_marker = p.segments.iter().any(|seg| {
        seg.inlines.iter().any(|i| {
            matches!(i, InlineNode::Decoration(d) if custom_xml_range_marker_local(d).is_some())
        })
    });
    if !has_range_marker {
        return;
    }

    // Helper: the range marker's id, if this decoration is one.
    let range_id = |deco: &crate::domain::DecorationNode| -> Option<String> {
        custom_xml_range_marker_local(deco)?;
        deco.raw_xml
            .as_deref()
            .and_then(|raw| crate::word_xml::parse_raw_fragment(raw).ok())
            .and_then(|el| crate::xml_attrs::attr_get(&el, "id").cloned())
    };

    // PASS 1: determine which range ids actually GOVERN a CustomXmlWrapper —
    // i.e. enclose at least one wrapper marker. Only those ranges (and their
    // wrappers) collapse on resolution. A customXml*Range that encloses no
    // wrapper (e.g. a table-level range over rows) is left intact: there is no
    // wrapper markup for it to resolve, and dropping it would tear nothing but
    // also has no well-defined meaning here.
    let mut open: Vec<String> = Vec::new();
    let mut governing: std::collections::HashSet<String> = std::collections::HashSet::new();
    for seg in &p.segments {
        for inline in &seg.inlines {
            let InlineNode::Decoration(deco) = inline else {
                continue;
            };
            if let Some(local) = custom_xml_range_marker_local(deco) {
                let id = range_id(deco).unwrap_or_default();
                if local.ends_with("RangeStart") {
                    open.push(id);
                } else if let Some(pos) = open.iter().rposition(|x| *x == id) {
                    open.remove(pos);
                }
            } else if matches!(
                deco.kind,
                crate::domain::DecorationType::CustomXmlWrapper
                    | crate::domain::DecorationType::CustomXmlWrapperEnd
            ) {
                // Every currently-open range governs this wrapper (both the
                // open and close polarity markers of the decomposed pair).
                for id in &open {
                    governing.insert(id.clone());
                }
            }
        }
    }
    if governing.is_empty() {
        return;
    }

    // Classify a wrapper by whether a GOVERNING range currently encloses it.
    let wrapper_fate = |open: &[String]| -> WrapperFate {
        if open.iter().any(|id| governing.contains(id)) {
            WrapperFate::CollapseWithRange
        } else {
            WrapperFate::Keep
        }
    };

    // PASS 2: apply the fate. A governing range's markers drop; a wrapper a
    // governing range encloses collapses. Non-governing range markers and
    // plain (un-enclosed) wrappers survive.
    let mut open: Vec<String> = Vec::new();
    for seg in &mut p.segments {
        seg.inlines.retain(|inline| {
            let InlineNode::Decoration(deco) = inline else {
                return true;
            };
            if let Some(local) = custom_xml_range_marker_local(deco) {
                let id = range_id(deco).unwrap_or_default();
                let is_governing = governing.contains(&id);
                if local.ends_with("RangeStart") {
                    open.push(id);
                } else if let Some(pos) = open.iter().rposition(|x| *x == id) {
                    open.remove(pos);
                }
                // A governing range's markers are part of the resolved revision.
                return !is_governing;
            }
            if matches!(
                deco.kind,
                crate::domain::DecorationType::CustomXmlWrapper
                    | crate::domain::DecorationType::CustomXmlWrapperEnd
            ) {
                return match wrapper_fate(&open) {
                    WrapperFate::Keep => true,
                    WrapperFate::CollapseWithRange => false,
                };
            }
            true
        });
    }
}

fn normalize_paragraph_after_projection(
    paragraph: &mut ParagraphNode,
    preserve_formatting_change: bool,
) {
    paragraph
        .segments
        .retain(|segment| !segment.inlines.is_empty());
    for segment in &mut paragraph.segments {
        segment.status = TrackingStatus::Normal;
    }
    paragraph.para_mark_status = None;

    // Invariant: after projection, a paragraph has EITHER structural `numbering`
    // (prefix derived at render time) OR materialized prefix inline nodes,
    // never both. If both are present, text extraction sees the prefix twice.
    //
    // When `numbering` is present, strip any materialized prefix nodes —
    // they are redundant (numbering wins).
    // When `numbering` is absent, re-extract materialized prefix nodes into
    // `literal_prefix` so the canonical form matches a freshly-parsed document.
    if paragraph.literal_prefix.is_none() {
        let mut all_inlines: Vec<InlineNode> = paragraph
            .segments
            .drain(..)
            .flat_map(|s| s.inlines)
            .collect();
        if paragraph.numbering.is_some() {
            // Numbering is present — strip materialized prefix nodes without
            // promoting them to literal_prefix (numbering already carries the
            // prefix semantics).
            strip_materialized_prefix_by_id_discard(&mut all_inlines);
        } else {
            // First try explicit materialized-prefix markers. This is more
            // precise than pattern matching — e.g. a bullet "•" followed by a
            // tab won't accidentally consume the tab that belongs to body text.
            strip_materialized_prefix_by_id(&mut all_inlines, paragraph);

            // If the prefix was materialized from structural numbering
            // (materialized_numbering is set), restore numbering instead of
            // keeping literal_prefix.  This ensures the projected document
            // matches the target's representation (structural numPr) rather
            // than degrading to literal_prefix.
            if let Some(saved) = paragraph.materialized_numbering.take() {
                if paragraph.literal_prefix.is_some() {
                    // strip_materialized_prefix_by_id promoted the materialized
                    // prefix text
                    // to literal_prefix — undo that and restore numbering.
                    paragraph.literal_prefix = None;
                    paragraph.literal_prefix_leading_tab_twips = None;
                    paragraph.literal_prefix_leading_tab_count = 0;
                    paragraph.literal_prefix_has_trailing_tab = false;
                    paragraph.literal_prefix_trailing_tab_stop_twips = None;
                    paragraph.numbering = Some(saved);
                }
                // If literal_prefix is still None, strip_materialized_prefix_by_id
                // didn't find a materialized prefix node — the prefix was
                // already absent (e.g. empty prefix). Still restore numbering so the
                // paragraph keeps its structural numbering identity.
                else {
                    paragraph.literal_prefix_leading_tab_twips = None;
                    paragraph.literal_prefix_leading_tab_count = 0;
                    paragraph.literal_prefix_has_trailing_tab = false;
                    paragraph.literal_prefix_trailing_tab_stop_twips = None;
                    paragraph.numbering = Some(saved);
                }
            }

            // If ID-based stripping didn't find anything, fall back to pattern
            // matching for truly literal prefixes (inline text that happens to
            // look like enumeration labels).
            if paragraph.literal_prefix.is_none()
                && paragraph.numbering.is_none()
                && let Some(prefix) = strip_literal_prefix(&mut all_inlines)
            {
                paragraph.literal_prefix = Some(prefix.label);
                paragraph.literal_prefix_marks = prefix.marks;
                paragraph.literal_prefix_style_props = prefix.style_props;
                paragraph.literal_prefix_rpr_authored = prefix.rpr_authored;
                paragraph.literal_prefix_leading_tab_twips = if prefix.has_leading_tab {
                    // In tracked model projection we don't have tab stop context,
                    // so we store a sentinel; the diff pipeline uses the ParagraphNode's
                    // value which was computed during initial parse with full context.
                    // For projected paragraphs (base side), the leading tab flag is
                    // informational — the gap was already computed on the target side.
                    Some(0)
                } else {
                    None
                };
                paragraph.literal_prefix_leading_tab_count = prefix.leading_tab_count;
                paragraph.literal_prefix_has_trailing_tab = prefix.has_trailing_tab;
                paragraph.literal_prefix_trailing_tab_stop_twips = if prefix.has_trailing_tab {
                    Some(0)
                } else {
                    None
                };
            }
        }
        if !all_inlines.is_empty() {
            paragraph.segments.push(TrackedSegment {
                status: TrackingStatus::Normal,
                inlines: all_inlines,
            });
        }
    }

    // Clear tracked formatting changes — they've been resolved by the
    // projection. Exception: the text-edit-prep projection keeps the record so a
    // prior pPrChange stays reversible (see this fn's doc + callers).
    if !preserve_formatting_change {
        paragraph.formatting_change = None;
    }

    // Recompute rendered_text from the projected state.  During import,
    // rendered_text = "{prefix}\t{body}" for paragraphs with numbering or
    // literal_prefix.  After projection the prefix may have been re-extracted
    // into literal_prefix — reconstruct rendered_text so that downstream
    // diff hashing stays consistent with freshly-imported documents.
    if let Some(pfx) = &paragraph.literal_prefix {
        let body: String = paragraph
            .segments
            .iter()
            .flat_map(|s| &s.inlines)
            .filter_map(|i| match i {
                InlineNode::Text(t) => Some(t.text.as_str()),
                InlineNode::HardBreak(_) => Some("\n"),
                _ => None,
            })
            .collect();
        paragraph.rendered_text = Some(format!("{pfx}\t{body}"));
    } else if let Some(ref num) = paragraph.numbering {
        // Structural numbering — reconstruct rendered_text from the
        // synthesized text.  When numbering was restored from
        // materialized_numbering the import-time rendered_text is stale
        // (it was computed for the base before materialization cleared it).
        let body: String = paragraph
            .segments
            .iter()
            .flat_map(|s| &s.inlines)
            .filter_map(|i| match i {
                InlineNode::Text(t) => Some(t.text.as_str()),
                InlineNode::HardBreak(_) => Some("\n"),
                _ => None,
            })
            .collect();
        paragraph.rendered_text = Some(format!("{}\t{body}", num.synthesized_text));
    } else {
        // No prefix or numbering — clear rendered_text.
        paragraph.rendered_text = None;
    }
}

fn leading_materialized_prefix_text_index(inlines: &[InlineNode]) -> Option<usize> {
    let mut prefix_idx = None;
    for (idx, inline) in inlines.iter().enumerate() {
        match inline {
            InlineNode::Text(t) if is_materialized_prefix_text(t) => {
                prefix_idx.get_or_insert(idx);
            }
            InlineNode::Decoration(_)
            | InlineNode::CommentRangeStart { .. }
            | InlineNode::CommentRangeEnd { .. }
            | InlineNode::CommentReference { .. } => continue,
            _ => break,
        }
    }
    prefix_idx
}

/// Strip a materialized numbering prefix identified by its explicit prefix kind.
///
/// During `fix_numbering_drift_for_normal_blocks`, numbering prefixes are
/// materialized as inline text with an explicit materialized-prefix kind.
/// When `strip_literal_prefix` can't detect the prefix via pattern matching
/// (e.g. Roman numerals, multi-level numbering), we fall back to that marker.
fn strip_materialized_prefix_by_id(inlines: &mut Vec<InlineNode>, paragraph: &mut ParagraphNode) {
    let Some(prefix_idx) = leading_materialized_prefix_text_index(inlines) else {
        return;
    };
    // Extract the prefix label (materialization adds trailing space: "{pfx_trimmed} ")
    if let Some(InlineNode::Text(t)) = inlines.get(prefix_idx) {
        let (label, leading_tab_count, has_trailing_tab) =
            strip_materialized_prefix_geometry(&t.text);
        if !label.is_empty() {
            paragraph.literal_prefix = Some(label);
            paragraph.literal_prefix_leading_tab_twips = if leading_tab_count > 0 {
                paragraph.literal_prefix_leading_tab_twips.or(Some(0))
            } else {
                None
            };
            paragraph.literal_prefix_leading_tab_count = leading_tab_count;
            paragraph.literal_prefix_has_trailing_tab = has_trailing_tab;
            paragraph.literal_prefix_trailing_tab_stop_twips = if has_trailing_tab {
                paragraph.literal_prefix_trailing_tab_stop_twips.or(Some(0))
            } else {
                None
            };
        }
    }
    inlines.remove(prefix_idx);
}

/// Strip a materialized numbering prefix identified by its explicit prefix kind,
/// discarding the prefix text entirely (not promoting it to `literal_prefix`).
///
/// Used when structural `numbering` is present — the materialized prefix is
/// redundant and keeping it would violate the invariant that a projected
/// paragraph never has both `numbering` and materialized prefix inline nodes.
fn strip_materialized_prefix_by_id_discard(inlines: &mut Vec<InlineNode>) {
    if let Some(prefix_idx) = leading_materialized_prefix_text_index(inlines) {
        inlines.remove(prefix_idx);
    }
}

/// Maps a tracked-change status to its `(has_ins_mark, has_del_mark)` class,
/// the plain-bool input the shared resolution rules take (`None` == `Normal`).
/// The stacked state carries both marks; all others carry at most one.
fn tracked_status_marks(status: &TrackingStatus) -> (bool, bool) {
    match status {
        TrackingStatus::Normal => (false, false),
        TrackingStatus::Inserted(_) => (true, false),
        TrackingStatus::Deleted(_) => (false, true),
        TrackingStatus::InsertedThenDeleted(_) => (true, true),
    }
}

/// Whether a paragraph's para_mark_status means "merge into next paragraph"
/// for the given accept/reject direction. Consults the shared join rule (see
/// `resolution_rules::para_mark_join_needed`) — the byte path's
/// `join_mark_resolved_paragraphs` (normalize.rs) consults the same rule so
/// the two cannot drift on which paragraphs join.
fn para_mark_needs_merge(status: &Option<TrackingStatus>, keep_inserted: bool) -> bool {
    let (has_ins, has_del) = status.as_ref().map_or((false, false), tracked_status_marks);
    crate::resolution_rules::para_mark_join_needed(has_ins, has_del, keep_inserted)
}

/// Zero-width body-level markers a paragraph-mark merge may step over.
/// Body-level `w:bookmarkEnd`, range delimiters, proof errors etc. import as
/// `OpaqueBlock(Unknown(tag))`; they occupy no space in the flow, so removing
/// a paragraph break joins ACROSS them (Word does), while any other block —
/// a table, an sdt, a quarantined item — blocks the join. The element kinds
/// are the shared `resolution_rules::ZERO_WIDTH_BODY_MARKER_NAMES` enumeration;
/// the byte path's `is_zero_width_body_marker` (normalize.rs) consults the same
/// list over raw XML siblings, so the two cannot diverge on which joins happen.
/// Extraction stays model-local: strip the namespace prefix off the imported
/// `OpaqueBlock(Unknown(tag))` and check membership by local name.
fn is_zero_width_marker_block(block: &BlockNode) -> bool {
    let BlockNode::OpaqueBlock(o) = block else {
        return false;
    };
    let OpaqueKind::Unknown(name) = &o.kind else {
        return false;
    };
    let local = name.rsplit(':').next().unwrap_or(name);
    crate::resolution_rules::is_zero_width_body_marker_name(local)
}

/// Merge paragraphs in a `Vec<BlockNode>` (used for table cell contents).
///
/// ECMA-376 §17.13.5.15 / §17.13.5.20: When a paragraph mark deletion is
/// accepted (or a paragraph mark insertion is rejected), the paragraph's
/// inline content merges into the FOLLOWING paragraph. The following
/// paragraph's properties (pPr) win.
///
/// The merge steps over zero-width marker blocks (see
/// `is_zero_width_marker_block`) but any other intervening block — a table,
/// an sdt, a quarantined item — prevents it. If the marked paragraph is last
/// (no following paragraph), it stays as-is.
fn merge_marked_paragraphs_bare(blocks: &mut Vec<BlockNode>, keep_inserted: bool) {
    let mut i = 0;
    while i < blocks.len() {
        let needs_merge = match &blocks[i] {
            BlockNode::Paragraph(p) => {
                para_mark_needs_merge(&p.para_mark_status, keep_inserted) && !p.para_split // splits carry full old text in inline changes — no merge needed
            }
            _ => false,
        };
        if !needs_merge {
            i += 1;
            continue;
        }
        // Find the join target: the next paragraph, stepping over zero-width
        // markers and any table this resolution empties of every row, but
        // stopping at any other block (real content blocks the join).
        let mut next_para_idx = None;
        for (offset, b) in blocks[(i + 1)..].iter().enumerate() {
            if matches!(b, BlockNode::Table(t) if table_emptied_by_accept_reject(t, keep_inserted))
            {
                continue;
            }
            if matches!(b, BlockNode::Paragraph(_)) {
                next_para_idx = Some(i + 1 + offset);
                break;
            }
            if is_zero_width_marker_block(b) {
                continue;
            }
            break;
        }
        let Some(j) = next_para_idx else {
            // No join target (a surviving table follows, or this is the last
            // block). Drop a donor the resolution leaves empty — an empty husk
            // is not what Word keeps; a donor with surviving content stays.
            let drop_empty = i + 1 < blocks.len()
                && matches!(&blocks[i],
                    BlockNode::Paragraph(p) if paragraph_emptied_by_accept_reject(p, keep_inserted));
            if drop_empty {
                blocks.remove(i);
            } else {
                i += 1;
            }
            continue;
        };
        // Take segments from paragraph i and prepend to paragraph j.
        let donor_segments = match &mut blocks[i] {
            BlockNode::Paragraph(p) => std::mem::take(&mut p.segments),
            _ => unreachable!(),
        };
        match &mut blocks[j] {
            BlockNode::Paragraph(target) => {
                let mut merged = donor_segments;
                merged.append(&mut target.segments);
                target.segments = merged;
            }
            _ => unreachable!(),
        }
        // Remove the donor paragraph.
        blocks.remove(i);
        // Don't increment i — the next element shifted into position i.
    }
}

/// Whether a tracked block will survive the block-level retain filter.
/// Consults the shared class-survival rule (see
/// `resolution_rules::tracked_class_survives`) — the same rule the byte path's
/// `table_emptied_by_resolution` / `paragraph_emptied_by_resolution`
/// (normalize.rs) consult.
fn block_survives_retain(status: &TrackingStatus, keep_inserted: bool) -> bool {
    let (has_ins, has_del) = tracked_status_marks(status);
    crate::resolution_rules::tracked_class_survives(has_ins, has_del, keep_inserted)
}

/// Whether a table row survives the full accept/reject retain filter (the row
/// `retain` in `project_block_inner`'s `BlockNode::Table` arm). Shared with
/// `table_emptied_by_accept_reject` so the merge's "join ACROSS a vanishing
/// table" decision cannot drift from projection's "drop the rowless shell"
/// decision — if they disagreed, the merge could join across a table that then
/// survived, silently deleting its rows.
fn row_survives_accept_reject(status: &Option<TrackingStatus>, keep_inserted: bool) -> bool {
    let (has_ins, has_del) = status.as_ref().map_or((false, false), tracked_status_marks);
    crate::resolution_rules::tracked_class_survives(has_ins, has_del, keep_inserted)
}

/// A table the accept/reject resolution empties COMPLETELY: it had rows and the
/// retain filter drops every one, so `project_blocks_for_accept_reject` then
/// removes the rowless shell (Word parity). A table that was already rowless
/// (valid per CT_Tbl, row group `minOccurs="0"`) is untouched by the resolution
/// and does NOT vanish.
///
/// A paragraph-mark merge must join ACROSS such a vanishing table, because Word
/// does: rejecting the paragraph-mark insertions that split one logical
/// paragraph around inserted, all-tracked tables rejoins it into one paragraph
/// (§17.13.5.20 — rejecting an inserted paragraph mark removes it, joining the
/// content with the following paragraph; the interleaved tables disappear on the
/// same reject, so "following" is the next surviving paragraph past them).
///
/// The "had rows and none survive" composition — and its per-row survival — is
/// the shared `resolution_rules::table_emptied_by_resolution`, the same rule the
/// byte path's `table_emptied_by_resolution` (normalize.rs) consults; extraction
/// stays model-local (each row's marks from its `tracking_status`).
fn table_emptied_by_accept_reject(t: &TableNode, keep_inserted: bool) -> bool {
    let rows = t.rows.iter().map(|row| {
        row.tracking_status
            .as_ref()
            .map_or((false, false), tracked_status_marks)
    });
    crate::resolution_rules::table_emptied_by_resolution(rows, keep_inserted)
}

/// Whether a top-level block is removed ENTIRELY by the accept/reject
/// resolution — either dropped at the block level, or a table emptied of every
/// row. A paragraph-mark merge steps over such blocks when searching for its
/// join target (they occupy no position in the resolved flow).
fn tracked_block_removed_by_accept_reject(tb: &TrackedBlock, keep_inserted: bool) -> bool {
    !block_survives_retain(&tb.status, keep_inserted)
        || matches!(&tb.block, BlockNode::Table(t) if table_emptied_by_accept_reject(t, keep_inserted))
}

/// Whether the accept/reject resolution EMPTIES this paragraph — it had inline
/// content and the retain filter drops every bit of it — and it carries no
/// structural content (a hoisted list label, or section properties) that must
/// survive.
///
/// A paragraph-mark merge that removes this paragraph's break but finds NO join
/// target (a surviving table follows) DROPS a donor the resolution emptied: a
/// fully-inserted paragraph rejected, or a fully-deleted paragraph accepted, has
/// no content to carry and its mark is resolved away, so Word removes it rather
/// than leaving an empty husk (wild-witnessed — an inserted "Table" heading
/// before a retained table vanishes on reject). A donor with surviving content
/// instead stays as its own paragraph, its mark becoming that paragraph's
/// terminating mark.
///
/// Crucially this fires ONLY when the paragraph HAD content that the resolution
/// removed — a paragraph that was ALREADY empty in the base (e.g. a trailing
/// empty paragraph whose mark merely became inserted to append a NEW paragraph
/// after it) is base content: rejecting the insertion removes what was appended,
/// not the base paragraph, so it must survive.
///
/// The caller also suppresses the drop when the donor is the LAST block in its
/// container: removing a non-last block never changes what the container ends
/// with (a valid input stays valid), but a cell / body must still END with a
/// paragraph, so an emptied terminal husk is kept as that terminating paragraph.
fn paragraph_emptied_by_accept_reject(p: &ParagraphNode, keep_inserted: bool) -> bool {
    if p.literal_prefix.is_some() || p.section_properties.is_some() {
        return false;
    }
    let had_content = p.segments.iter().any(|seg| !seg.inlines.is_empty());
    let keeps_content = p
        .segments
        .iter()
        .any(|seg| block_survives_retain(&seg.status, keep_inserted) && !seg.inlines.is_empty());
    had_content && !keeps_content
}

/// Merge paragraphs in a `Vec<TrackedBlock>` (used for top-level document blocks).
///
/// Same semantics as `merge_marked_paragraphs_bare` but operates on TrackedBlock wrappers.
/// Only merges paragraphs whose block-level status means they survive the retain
/// filter -- paragraphs that will be removed entirely (e.g. block status=Deleted
/// on accept) should not trigger a merge even if para_mark_status is also Deleted.
/// Move the donor paragraph's content to the FRONT of the join target,
/// keeping the user-visible literal labels where their bytes actually are
/// (§17.13.5.15 / §17.13.5.19 joins keep the FOLLOWING paragraph's
/// properties, but the donor's content comes first):
///
/// - the donor's hoisted `literal_prefix` becomes the merged paragraph's
///   `literal_prefix` — its literal text starts the merged content, which is
///   exactly what a reimport of the byte-path output would hoist;
/// - the target's own hoisted `literal_prefix` no longer sits at the start
///   of the merged paragraph, so it is re-materialized as plain inline text
///   at the head of the target's old content (where its literal bytes live);
/// - structural numbering needs nothing here: its visible number lives in
///   paragraph PROPERTIES, which the join resolves to the target's (Word
///   drops the donor's number on a join too).
fn merge_paragraph_into_following(donor: &mut ParagraphNode, target: &mut ParagraphNode) {
    let mut merged = std::mem::take(&mut donor.segments);

    if let Some(label) = target.literal_prefix.take() {
        let text = materialized_prefix_text(&label, target);
        merged.push(TrackedSegment {
            status: TrackingStatus::Normal,
            inlines: vec![InlineNode::from(TextNode {
                id: NodeId::from(format!("{}_premerge_prefix", target.id.0)),
                text_role: None,
                text,
                marks: target.literal_prefix_marks.clone(),
                style_props: target.literal_prefix_style_props.clone(),
                rpr_authored: target.literal_prefix_rpr_authored,
                formatting_change: None,
            })],
        });
    }
    merged.append(&mut target.segments);
    target.segments = merged;

    if donor.literal_prefix.is_some() {
        target.literal_prefix = donor.literal_prefix.take();
        sync_literal_prefix_geometry(donor, target);
    } else {
        // No incoming label: clear any geometry left over from the
        // materialized target prefix.
        target.literal_prefix_leading_tab_twips = None;
        target.literal_prefix_leading_tab_count = 0;
        target.literal_prefix_has_trailing_tab = false;
        target.literal_prefix_trailing_tab_stop_twips = None;
    }
}

fn merge_marked_paragraphs_tracked(blocks: &mut Vec<TrackedBlock>, keep_inserted: bool) {
    let mut i = 0;
    while i < blocks.len() {
        // Only consider paragraphs that survive at block level AND have a
        // para_mark_status that triggers merging.
        let needs_merge = match &blocks[i].block {
            BlockNode::Paragraph(p) => {
                block_survives_retain(&blocks[i].status, keep_inserted)
                    && para_mark_needs_merge(&p.para_mark_status, keep_inserted)
                    && !p.para_split // splits carry full old text in inline changes — no merge needed
            }
            _ => false,
        };
        if !needs_merge {
            i += 1;
            continue;
        }
        // Find the join target: the next SURVIVING paragraph, stepping over
        // blocks this resolution removes entirely (a dropped block-level
        // wrapper, or a table emptied of every row) and zero-width markers, but
        // stopping at any other surviving block (real content blocks the join).
        let mut next_para_idx = None;
        for (offset, b) in blocks[(i + 1)..].iter().enumerate() {
            if tracked_block_removed_by_accept_reject(b, keep_inserted) {
                continue;
            }
            if matches!(&b.block, BlockNode::Paragraph(_)) {
                next_para_idx = Some(i + 1 + offset);
                break;
            }
            if is_zero_width_marker_block(&b.block) {
                continue;
            }
            // Hit surviving flow content (a table with surviving rows, an sdt,
            // …) — the join is blocked.
            break;
        }
        let Some(j) = next_para_idx else {
            // No join target (a surviving table follows, or this is the last
            // block). Drop a donor the resolution leaves empty (a fully-inserted
            // paragraph rejected / fully-deleted accepted) rather than keeping an
            // empty husk; a donor with surviving content stays as its own
            // paragraph.
            let drop_empty = i + 1 < blocks.len()
                && matches!(&blocks[i].block,
                    BlockNode::Paragraph(p) if paragraph_emptied_by_accept_reject(p, keep_inserted));
            if drop_empty {
                blocks.remove(i);
            } else {
                i += 1;
            }
            continue;
        };
        // Move paragraph i's content (and label) to the front of paragraph j.
        let (left, right) = blocks.split_at_mut(j);
        match (&mut left[i].block, &mut right[0].block) {
            (BlockNode::Paragraph(donor), BlockNode::Paragraph(target)) => {
                merge_paragraph_into_following(donor, target);
            }
            _ => unreachable!(),
        }
        // Remove the donor block.
        blocks.remove(i);
        // Don't increment i — the next element shifted into position i.
    }
}

/// Project the tracked-change runs inside a hyperlink for accept/reject.
///
/// On accept (`keep_inserted = true`): drop `Deleted` runs, clear `Inserted`
/// runs to `Normal`. On reject (`keep_inserted = false`): drop `Inserted`
/// runs, clear `Deleted` runs to `Normal`.
///
/// The hyperlink envelope (URL, anchor, r_id, extra_attrs) is preserved.
/// `HyperlinkData.text` is refreshed from the surviving run texts so it
/// stays in sync with `runs` (see `HyperlinkData.text` docstring).
fn project_hyperlink_runs(data: &mut crate::domain::HyperlinkData, keep_inserted: bool) {
    data.runs.retain(|run| match &run.status {
        TrackingStatus::Normal => true,
        TrackingStatus::Inserted(_) => keep_inserted,
        TrackingStatus::Deleted(_) => !keep_inserted,
        // Drops in both full resolutions (origin rules).
        TrackingStatus::InsertedThenDeleted(_) => false,
    });
    for run in &mut data.runs {
        run.status = TrackingStatus::Normal;
    }
    data.text = data.runs.iter().map(|r| r.text.as_str()).collect();
}

pub(crate) fn project_block_for_accept_reject(block: &mut BlockNode, keep_inserted: bool) {
    project_block_inner(
        block,
        keep_inserted,
        /*preserve_formatting_change=*/ false,
        // Callers of this thin wrapper (edit verbs) always ACCEPT (keep_inserted
        // = true), which never changes a paragraph's style, so no style table is
        // needed to re-resolve style-inherited run marks. The reject paths that
        // DO change the style thread their `StyleDefinitions` in explicitly.
        /*style_defs=*/
        None,
    );
}

/// Flatten pre-existing tracked ins/del TEXT (and block/row/cell) segments into
/// a clean Normal slate for a subsequent direct text edit, while PRESERVING any
/// recorded `*PrChange` formatting-change record (pPrChange/rPrChange/tblPrChange/
/// trPrChange/tcPrChange).
///
/// This is an ACCEPT projection for text only. The text-edit handlers
/// (`ReplaceParagraphText`/`ReplaceSpanText`) call this to get a clean base to
/// diff against, but a prior tracked paragraph-property change must NOT be
/// silently accepted: doing so would break the tracked-change reversibility
/// invariant (ECMA-376 §17.13.5.29) — `reject_all` could no longer restore the
/// original formatting. By keeping `formatting_change` intact, the paragraph the
/// text edit produces still reverts BOTH text and formatting on reject, and
/// keeps BOTH on accept.
pub(crate) fn project_block_for_text_edit_prep(block: &mut BlockNode) {
    project_block_inner(
        block, /*keep_inserted=*/ true, /*preserve_formatting_change=*/ true,
        // Text-edit prep is an accept-for-text projection that PRESERVES the
        // formatting-change record and never alters `style_id` — nothing to
        // re-resolve.
        /*style_defs=*/
        None,
    );
}

fn project_block_inner(
    block: &mut BlockNode,
    keep_inserted: bool,
    preserve_formatting_change: bool,
    style_defs: Option<&crate::styles::StyleDefinitions>,
) {
    match block {
        BlockNode::Paragraph(p) => {
            // Snapshot the style before resolution so we can detect a
            // pPrChange-driven style swap and re-resolve style-inherited run
            // marks against the resulting style (see below).
            let style_before = p.style_id.clone();
            // Handle paragraph formatting change (pPrChange, §17.13.5.29).
            // The child pPr inside pPrChange is the COMPLETE previous state.
            if !keep_inserted {
                // Reject: restore all previous properties from pPrChange.
                reject_paragraph_formatting(p);
            } else if !preserve_formatting_change {
                // Accept: keep current properties, discard the change record.
                // (When preparing for a direct text edit, the record is KEPT so
                // the prior pPrChange stays reversible — see
                // `project_block_for_text_edit_prep`.)
                p.formatting_change = None;
            }

            // Handle a mid-document section-break change (w:sectPrChange inside
            // the paragraph's w:sectPr, §17.13.5.32). On reject, restore the
            // previous section properties from the raw snapshot; on accept, keep
            // the new properties and drop the change record.
            if !keep_inserted {
                if let Some(change) = p.section_property_change.take()
                    && let Some(prev) =
                        parse_previous_section_properties(&change.previous_properties_raw)
                {
                    p.section_properties = Some(prev);
                }
            } else if !preserve_formatting_change {
                p.section_property_change = None;
            }

            p.segments.retain(|segment| match segment.status {
                TrackingStatus::Normal => true,
                TrackingStatus::Inserted(_) => keep_inserted,
                TrackingStatus::Deleted(_) => !keep_inserted,
                // The stacked state drops in BOTH full resolutions (origin
                // rules 2 and 3): accept-all accepts the deletion; reject-all
                // rejects the insertion and the nested deletion goes with it.
                TrackingStatus::InsertedThenDeleted(_) => false,
            });

            // Handle run-level formatting changes (rPrChange) on text nodes,
            // and project tracked changes inside hyperlink display text
            // (`HyperlinkData.runs[*].status`). The hyperlink envelope is
            // preserved; only its inner runs are filtered by status.
            for segment in &mut p.segments {
                for inline in &mut segment.inlines {
                    match inline {
                        InlineNode::Text(t) => {
                            if !keep_inserted {
                                reject_text_formatting(t);
                            } else if !preserve_formatting_change {
                                t.formatting_change = None;
                            }
                        }
                        InlineNode::OpaqueInline(opaque) => {
                            match &mut opaque.kind {
                                crate::domain::OpaqueKind::Hyperlink(data) => {
                                    project_hyperlink_runs(data, keep_inserted);
                                    // The opaque carries its own raw_xml cache.
                                    // Clear it so the next serialize rebuilds from
                                    // the projected `runs` rather than re-emitting
                                    // the pre-projection XML.
                                    opaque.raw_xml = None;
                                    opaque.content_hash = None;
                                }
                                // QuarantinedNestedTracking is bare by contract
                                // (no raw_xml to resolve; its revisions are
                                // deliberately unmodeled and the selector already
                                // refuses to touch it), so it stays a no-op.
                                crate::domain::OpaqueKind::QuarantinedNestedTracking => {}
                                // EVERY other opaque kind that carries raw_xml can
                                // legally hold `w:ins`/`w:del` inside it (textbox
                                // `txbxContent`, content-control `sdtContent`,
                                // fldSimple result, inline customXml/smartTag/ruby,
                                // and Unknown), and the byte path
                                // (`normalize_docx`/`reject_all_docx`) resolves all
                                // of them UNIFORMLY. Descend with the same byte
                                // resolver so the IR projection agrees: clean
                                // opaques come back Clean (left byte-verbatim, zero
                                // blast radius), and an unparseable fragment that
                                // still carries a revision marker is refused at the
                                // `project` preflight (`first_unparseable_opaque_
                                // with_revisions`) rather than silently passed
                                // through. Selective resolution still does NOT
                                // address inner-opaque revisions (they are not
                                // enumerable as changelets); full accept/reject now
                                // does, and the two stay consistent.
                                _ => {
                                    if let Some(raw) = opaque.raw_xml.as_deref() {
                                        match crate::normalize::resolve_opaque_fragment_revisions(
                                            raw,
                                            keep_inserted,
                                        ) {
                                            crate::normalize::FragmentResolution::Resolved(
                                                resolved,
                                            ) => {
                                                opaque.content_hash =
                                                    Some(crate::import::sha256_hex(&resolved));
                                                opaque.raw_xml = Some(resolved);
                                            }
                                            // Clean → leave verbatim.
                                            // UnparseableWithRevisions → already
                                            // refused at the preflight, so the
                                            // projection never reaches here for such
                                            // a fragment; leave it untouched.
                                            crate::normalize::FragmentResolution::Clean
                                            | crate::normalize::FragmentResolution::UnparseableWithRevisions => {}
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }

            // Resolve customXml*Range-governed wrappers (§17.13.5.4-.11). A
            // customXmlIns/Del/MoveFrom/MoveToRange marks the customXml/smartTag
            // WRAPPER markup itself as the revision, so accepting OR rejecting it
            // removes the range markers AND the wrapper they govern, leaving the
            // inner content (already resolved by the revision machinery above).
            // Word does this on both accept and reject. A
            // plain (un-ranged) wrapper is left intact — only ranged wrappers
            // collapse.
            resolve_custom_xml_range_governed_wrappers(p);

            // A reverted (or applied) paragraph-style change swaps `style_id`
            // underneath runs whose `style_props` were resolved against the old
            // style at import — re-run the cascade so style-inherited marks
            // (caps, bold, fonts, …) match the resulting style. Runs are
            // re-resolved AFTER their own rPrChange rejects above so we operate
            // on each run's final direct rPr. Only fires when the style actually
            // changed (so accept, which keeps the style, is untouched) and when
            // the caller supplied the style table.
            if let Some(style_defs) = style_defs
                && p.style_id != style_before
            {
                reresolve_paragraph_style_inherited_marks(p, style_defs);
            }

            normalize_paragraph_after_projection(p, preserve_formatting_change);
        }
        BlockNode::Table(t) => {
            // Handle table formatting change (tblPrChange, §17.13.5.34).
            if !keep_inserted {
                reject_table_formatting(t);
            } else if !preserve_formatting_change {
                t.formatting_change = None;
            }

            // Filter rows by tracking status: on accept, remove deleted rows
            // and keep inserted rows (clearing their status). On reject, do
            // the opposite. Shared with `table_emptied_by_accept_reject` (the
            // paragraph-mark merge's "does this whole table vanish?" test) so
            // the two never disagree.
            t.rows
                .retain(|row| row_survives_accept_reject(&row.tracking_status, keep_inserted));

            // tblGrid reconciliation for tracked COLUMN ops: capture (before the
            // cell-retain below clears tracking) which whole columns this
            // resolution drops, so the per-column `gridCol` widths stay in
            // lock-step with the surviving column count. No-op unless the table
            // has an explicit grid AND a whole column was uniformly added/removed
            // (a genuine tracked column op) — see `uniformly_removed_columns`.
            let removed_columns = if t.formatting.grid_cols.is_empty() {
                Vec::new()
            } else {
                uniformly_removed_columns(&t.rows, keep_inserted)
            };

            for row in &mut t.rows {
                row.tracking_status = None;

                // Handle row formatting change (trPrChange, §17.13.5.36).
                if !keep_inserted {
                    reject_row_formatting(row);
                } else if !preserve_formatting_change {
                    row.formatting_change = None;
                }

                // Filter cells by tracking status (same logic).
                row.cells.retain(|cell| match &cell.tracking_status {
                    None => true,
                    Some(TrackingStatus::Normal) => true,
                    Some(TrackingStatus::Inserted(_)) => keep_inserted,
                    Some(TrackingStatus::Deleted(_)) => !keep_inserted,
                    Some(TrackingStatus::InsertedThenDeleted(_)) => false,
                });
                for cell in &mut row.cells {
                    cell.tracking_status = None;

                    // Handle cell formatting change (tcPrChange, §17.13.5.37).
                    if !keep_inserted {
                        reject_cell_formatting(cell);
                    } else if !preserve_formatting_change {
                        cell.formatting_change = None;
                    }

                    // Merge paragraphs with marked para marks before
                    // filtering/projecting (the merge needs to see
                    // para_mark_status before it's cleared).
                    merge_marked_paragraphs_bare(&mut cell.blocks, keep_inserted);
                    // Filter cell blocks by para_mark_status: on accept, remove
                    // Deleted paragraphs; on reject, remove Inserted paragraphs.
                    // This handles paragraphs added via reconcile_cell_blocks
                    // during table structure change merges.
                    cell.blocks.retain(|block| match block {
                        BlockNode::Paragraph(p) => match &p.para_mark_status {
                            None | Some(TrackingStatus::Normal) => true,
                            Some(TrackingStatus::Inserted(_)) => keep_inserted,
                            Some(TrackingStatus::Deleted(_)) => !keep_inserted,
                            Some(TrackingStatus::InsertedThenDeleted(_)) => false,
                        },
                        _ => true,
                    });
                    // A nested table the resolution empties of every row is
                    // dropped, exactly like a body-level table in
                    // `project_blocks_for_accept_reject` (Word parity) — a merge
                    // just joined the cell paragraphs across it, so the rowless
                    // shell must not linger.
                    let nested_had_rows: Vec<bool> = cell
                        .blocks
                        .iter()
                        .map(|b| matches!(b, BlockNode::Table(t) if !t.rows.is_empty()))
                        .collect();
                    for nested in &mut cell.blocks {
                        project_block_inner(
                            nested,
                            keep_inserted,
                            preserve_formatting_change,
                            style_defs,
                        );
                    }
                    drop_emptied_nested_tables(&mut cell.blocks, &nested_had_rows);
                }
            }

            // Drop the tblGrid entries for columns this resolution removed
            // (descending, so earlier indices stay valid). Keeps `grid_cols.len()`
            // equal to the surviving column count for a tracked column op.
            for &col in removed_columns.iter().rev() {
                if col < t.formatting.grid_cols.len() {
                    t.formatting.grid_cols.remove(col);
                }
            }
        }
        BlockNode::OpaqueBlock(_) => {}
    }
}

/// Does a cell survive this resolution? (accept keeps inserted, drops deleted;
/// reject the opposite). Mirrors the cell-retain match in the projection.
fn cell_survives_status(status: &Option<TrackingStatus>, keep_inserted: bool) -> bool {
    match status {
        None | Some(TrackingStatus::Normal) => true,
        Some(TrackingStatus::Inserted(_)) => keep_inserted,
        Some(TrackingStatus::Deleted(_)) => !keep_inserted,
        Some(TrackingStatus::InsertedThenDeleted(_)) => false,
    }
}

/// For a SIMPLE grid (all rows same cell count, no `gridBefore/After`, no
/// `gridSpan>1` / `vMerge`), the physical column indices whose cell does NOT
/// survive this resolution in EVERY row — i.e. a whole column that a tracked
/// column op added (Inserted) then rejected, or deleted (Deleted) then accepted.
///
/// Returns empty for a non-simple grid (column identity is ambiguous across
/// spans) and when no column is *uniformly* removed, so the `tblGrid` is left
/// untouched for every resolution except a genuine tracked column op — a
/// single-row tracked cell (e.g. from a `replace`) never makes a whole column
/// "removed", so this stays a no-op there. Keeps `tblGrid` length-consistent
/// with the column count through accept AND reject (RFC-0003, tracked column
/// ops over an explicit grid).
fn uniformly_removed_columns(
    rows: &[crate::domain::TableRowNode],
    keep_inserted: bool,
) -> Vec<usize> {
    let Some(first) = rows.first() else {
        return Vec::new();
    };
    let n = first.cells.len();
    let simple = rows.iter().all(|r| {
        r.cells.len() == n
            && r.grid_before == 0
            && r.grid_after == 0
            && r.cells
                .iter()
                .all(|c| c.grid_span.max(1) == 1 && c.v_merge == crate::domain::VerticalMerge::None)
    });
    if !simple {
        return Vec::new();
    }
    (0..n)
        .filter(|&i| {
            rows.iter()
                .all(|r| !cell_survives_status(&r.cells[i].tracking_status, keep_inserted))
        })
        .collect()
}

/// Remove nested tables a resolution emptied of every row: `had_rows[i]` was
/// captured before projection, `blocks[i]` is now rowless. Mirrors the
/// body-level rowless-table drop; a table that was ALREADY rowless (had_rows =
/// false) is untouched and survives (CT_Tbl row group `minOccurs="0"`).
fn drop_emptied_nested_tables(blocks: &mut Vec<BlockNode>, had_rows: &[bool]) {
    let mut idx = 0;
    blocks.retain(|b| {
        let keep = !matches!(b, BlockNode::Table(t) if t.rows.is_empty() && had_rows[idx]);
        idx += 1;
        keep
    });
}

fn project_blocks_for_accept_reject(
    blocks: &mut Vec<TrackedBlock>,
    keep_inserted: bool,
    style_defs: Option<&crate::styles::StyleDefinitions>,
) {
    // Resolve the two Word-reachable move mixtures through the same explicit
    // origin transitions as selective projection. Full projection otherwise
    // sees only the inner status: Reject-All would incorrectly restore a
    // destination-only deletion as a stray paragraph, while Accept-All would
    // keep both clones of an inserted-then-moved paragraph.
    let inserted_move_origins = inserted_move_origin_plans(blocks);
    let move_destination_deletions = move_destination_deletion_plans(blocks);
    let mut all_move_mixture_ids = HashSet::new();
    for plan in &inserted_move_origins {
        all_move_mixture_ids.insert(plan.origin.identity);
        all_move_mixture_ids.insert(plan.move_revision.identity);
    }
    for plan in &move_destination_deletions {
        all_move_mixture_ids.insert(plan.move_revision.identity);
        all_move_mixture_ids.extend(plan.deletions.iter().map(|(revision, _)| revision.identity));
    }
    let action = if keep_inserted {
        ResolveSelectionAction::Accept
    } else {
        ResolveSelectionAction::Reject
    };
    apply_move_destination_deletion_cascade(
        blocks,
        action,
        &all_move_mixture_ids,
        &move_destination_deletions,
    );
    if keep_inserted {
        apply_inserted_move_origin_cascade(
            blocks,
            action,
            &all_move_mixture_ids,
            &inserted_move_origins,
        );
    } else {
        settle_inserted_move_origins_for_reject_all(blocks, &inserted_move_origins);
    }

    // Snapshot the story's inline range-pair markers BEFORE resolution, so a
    // pair torn by the drop below (one half removed with a resolved revision,
    // the other surviving) can be collapsed back to a point rather than left
    // orphaned (see `collapse_resolution_torn_range_markers`).
    let range_pair_inventory = capture_range_pair_inventory(blocks);

    // ECMA-376 §17.13.5.15 / §17.13.5.20: Merge paragraphs whose paragraph
    // mark deletion is accepted (or insertion is rejected) BEFORE filtering
    // and normalizing, because normalize clears para_mark_status.
    merge_marked_paragraphs_tracked(blocks, keep_inserted);
    blocks.retain(|tb| match tb.status {
        TrackingStatus::Normal => true,
        TrackingStatus::Inserted(_) => keep_inserted,
        TrackingStatus::Deleted(_) => !keep_inserted,
        // Drops in both full resolutions (origin rules); block-level stacked
        // status is never constructed today, but the rule is total.
        TrackingStatus::InsertedThenDeleted(_) => false,
    });
    // Word parity: when the resolution drops EVERY row of a table (all rows
    // were tracked), Word removes the now-rowless table shell too, so record
    // which tables still have rows going into projection. A table that was
    // ALREADY rowless is a valid state (CT_Tbl's row group is minOccurs="0";
    // Word opens such tables without repair), is untouched by the
    // resolution, and must survive it.
    let had_rows: Vec<bool> = blocks
        .iter()
        .map(|tb| match &tb.block {
            BlockNode::Table(t) => !t.rows.is_empty(),
            _ => false,
        })
        .collect();
    for tb in blocks.iter_mut() {
        tb.status = TrackingStatus::Normal;
        // Call `project_block_inner` directly (rather than the style-agnostic
        // `project_block_for_accept_reject` wrapper) so the style table threads
        // through to the pPrChange-reject re-resolution.
        project_block_inner(
            &mut tb.block,
            keep_inserted,
            /*preserve_formatting_change=*/ false,
            style_defs,
        );
    }
    let mut idx = 0;
    blocks.retain(|tb| {
        let keep = match &tb.block {
            BlockNode::Table(t) => !t.rows.is_empty() || !had_rows[idx],
            _ => true,
        };
        idx += 1;
        keep
    });

    // Re-pair any range marker the projection tore.
    collapse_resolution_torn_range_markers(blocks, &range_pair_inventory);
}

// ============================================================================
// Torn range-marker collapse (bookmark / comment-range / permission)
// ============================================================================
//
// Domain rule (ECMA-376 §17.13.6 pairing; Word behavior verified against real
// Word). A bookmark / comment range / permission range marks a SPAN of content
// with a start and an end paired by a part-local id. The two halves may straddle
// a tracked-change boundary — wild Word-authored documents place a
// `bookmarkStart` as a paragraph child OUTSIDE a `w:ins` while its paired
// `bookmarkEnd` sits INSIDE it. Resolving that insertion (reject) removes the
// content holding the end while the base-origin start survives, tearing the pair
// — schema-invalid, and the post-serialization guard refuses to emit it.
//
// Dropping the survivor too would delete base content NO revision proposed
// removing (fix-at-symptom). Instead we COLLAPSE the range to a point at the
// survivor: re-insert the removed half adjacent to the surviving half, so the
// marker survives as an empty range. That is exactly what Word does when the
// interior of a bookmarked range is deleted. The collapse runs at RESOLUTION
// time (here), NOT at the post-serialization guard, so the guard still catches a
// genuine merge-pipeline tear as the engine bug it is.
//
// Only INLINE markers are covered — `Decoration` (bookmark/permission, carrying
// their own bytes) and the typed comment-range nodes (carrying their own id).
// Move ranges (`moveFrom`/`moveTo`) and customXml ranges are excluded: they ARE
// the markup of a move / wrapper revision and resolve as a unit with it (see
// `is_move_range_marker`, `resolve_custom_xml_range_governed_wrappers`), so a
// resolution never strands one half against unrelated surviving content.
// Block-level markers (`OpaqueBlock`, whose bytes live behind an opaque_ref)
// are not collapsed here.

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum RangePairFamily {
    Bookmark,
    CommentRange,
    Permission,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum RangeRole {
    Start,
    End,
}

/// Map a paired range-marker element's local name to its family and start/end
/// role, or `None` if it is not a collapsible paired range marker.
fn range_pair_role_of(local: &str) -> Option<(RangePairFamily, RangeRole)> {
    match local {
        "bookmarkStart" => Some((RangePairFamily::Bookmark, RangeRole::Start)),
        "bookmarkEnd" => Some((RangePairFamily::Bookmark, RangeRole::End)),
        "permStart" => Some((RangePairFamily::Permission, RangeRole::Start)),
        "permEnd" => Some((RangePairFamily::Permission, RangeRole::End)),
        _ => None,
    }
}

/// Classify one inline node as a half of a collapsible paired range marker:
/// `(family, part-local pairing id, role)`.
fn classify_range_pair_marker(inline: &InlineNode) -> Option<(RangePairFamily, String, RangeRole)> {
    match inline {
        InlineNode::CommentRangeStart { id } => {
            Some((RangePairFamily::CommentRange, id.clone(), RangeRole::Start))
        }
        InlineNode::CommentRangeEnd { id } => {
            Some((RangePairFamily::CommentRange, id.clone(), RangeRole::End))
        }
        InlineNode::Decoration(deco) => {
            match deco.kind {
                crate::domain::DecorationType::Bookmark
                | crate::domain::DecorationType::PermissionRange => {}
                _ => return None,
            }
            let raw = deco.raw_xml.as_deref()?;
            let el = crate::word_xml::parse_raw_fragment(raw).ok()?;
            let (family, role) = range_pair_role_of(crate::import::local_element_name(&el))?;
            let id = crate::xml_attrs::attr_get(&el, "id")?.clone();
            Some((family, id, role))
        }
        _ => None,
    }
}

type RangePairKey = (RangePairFamily, String);

/// One captured half of a range pair. An `Inline` half is a paragraph-inline
/// marker a resolution can physically remove (and that we can re-insert to
/// collapse a torn pair); an `OpaqueBlock` half is a body-level marker preserved
/// as a verbatim-spliced opaque block. The opaque block has `Normal` status and
/// is never removed by resolution, so it only ever plays the SURVIVOR — but it
/// must be visible to the pairing, or its inline partner's removal would orphan
/// it.
enum RangeHalf {
    Inline(InlineNode),
    OpaqueBlock,
}

impl RangeHalf {
    fn inline(&self) -> Option<&InlineNode> {
        match self {
            RangeHalf::Inline(n) => Some(n),
            RangeHalf::OpaqueBlock => None,
        }
    }
}

/// The captured start/end halves of a range pair, taken before resolution so a
/// dropped half can be re-materialized adjacent to the surviving half.
#[derive(Default)]
struct RangePairCapture {
    start: Option<RangeHalf>,
    end: Option<RangeHalf>,
}

/// Map a body-level opaque marker block's stored identity into the repair's
/// `(family, id, role)` vocabulary. `None` for any opaque block that is not a
/// paired range marker.
fn opaque_range_marker(
    o: &crate::domain::OpaqueBlockNode,
) -> Option<(RangePairFamily, String, RangeRole)> {
    let m = o.range_marker.as_ref()?;
    let family = match m.family {
        crate::domain::RangeMarkerFamily::Bookmark => RangePairFamily::Bookmark,
        crate::domain::RangeMarkerFamily::CommentRange => RangePairFamily::CommentRange,
        crate::domain::RangeMarkerFamily::Permission => RangePairFamily::Permission,
    };
    let role = match m.role {
        crate::domain::RangeMarkerRole::Start => RangeRole::Start,
        crate::domain::RangeMarkerRole::End => RangeRole::End,
    };
    Some((family, m.id.clone(), role))
}

/// Snapshot every range-pair marker in a story — paragraph inlines (recursing
/// table cells) AND body-level opaque marker blocks.
fn capture_range_pair_inventory(
    blocks: &[TrackedBlock],
) -> HashMap<RangePairKey, RangePairCapture> {
    fn assign(slot: &mut RangePairCapture, role: RangeRole, half: RangeHalf) {
        match role {
            RangeRole::Start => slot.start = Some(half),
            RangeRole::End => slot.end = Some(half),
        }
    }
    fn visit(block: &BlockNode, map: &mut HashMap<RangePairKey, RangePairCapture>) {
        match block {
            BlockNode::Paragraph(p) => {
                for seg in &p.segments {
                    for inline in &seg.inlines {
                        if let Some((family, id, role)) = classify_range_pair_marker(inline) {
                            let slot = map.entry((family, id)).or_default();
                            assign(slot, role, RangeHalf::Inline(inline.clone()));
                        }
                    }
                }
            }
            BlockNode::Table(t) => {
                for row in &t.rows {
                    for cell in &row.cells {
                        for tb in &cell.blocks {
                            visit(tb, map);
                        }
                    }
                }
            }
            BlockNode::OpaqueBlock(o) => {
                if let Some((family, id, role)) = opaque_range_marker(o) {
                    let slot = map.entry((family, id)).or_default();
                    assign(slot, role, RangeHalf::OpaqueBlock);
                }
            }
        }
    }
    let mut map = HashMap::new();
    for tb in blocks {
        visit(&tb.block, &mut map);
    }
    map
}

/// Presence of each range pair's start/end AFTER resolution: `(start, end)`.
fn survivor_range_pairs(blocks: &[TrackedBlock]) -> HashMap<RangePairKey, (bool, bool)> {
    fn mark(slot: &mut (bool, bool), role: RangeRole) {
        match role {
            RangeRole::Start => slot.0 = true,
            RangeRole::End => slot.1 = true,
        }
    }
    fn visit(block: &BlockNode, map: &mut HashMap<RangePairKey, (bool, bool)>) {
        match block {
            BlockNode::Paragraph(p) => {
                for seg in &p.segments {
                    for inline in &seg.inlines {
                        if let Some((family, id, role)) = classify_range_pair_marker(inline) {
                            mark(map.entry((family, id)).or_insert((false, false)), role);
                        }
                    }
                }
            }
            BlockNode::Table(t) => {
                for row in &t.rows {
                    for cell in &row.cells {
                        for tb in &cell.blocks {
                            visit(tb, map);
                        }
                    }
                }
            }
            BlockNode::OpaqueBlock(o) => {
                if let Some((family, id, role)) = opaque_range_marker(o) {
                    mark(map.entry((family, id)).or_insert((false, false)), role);
                }
            }
        }
    }
    let mut map = HashMap::new();
    for tb in blocks {
        visit(&tb.block, &mut map);
    }
    map
}

/// A torn pair's repair, keyed by the SURVIVOR's identity (family, pairing id,
/// surviving role). The value is the missing half to re-materialize adjacent to
/// that survivor. Keying by the survivor lets the single insertion pass below
/// place every partner by direct lookup instead of re-walking the story per pair.
type RepairKey = (RangePairFamily, String, RangeRole);

/// Re-pair every range marker the projection tore: for each pair that was WHOLE
/// before resolution but now has exactly one surviving half, insert the missing
/// half adjacent to the survivor (collapsing the range to a point).
///
/// Complexity is O(D + T) in the story's content size D and its torn-pair count
/// T: one pass (`survivor_range_pairs`) records which halves survive, the torn
/// pairs are planned in O(T) keyed by the survivor, then a SINGLE insertion pass
/// re-materializes every missing half by direct lookup. The historical shape
/// re-walked the whole story once per torn pair — O(T·D) — which made accept /
/// reject quadratic on redline- and bookmark-heavy documents (H3-M2).
fn collapse_resolution_torn_range_markers(
    blocks: &mut [TrackedBlock],
    inventory: &HashMap<RangePairKey, RangePairCapture>,
) {
    if inventory.is_empty() {
        return;
    }
    let survivors = survivor_range_pairs(blocks);
    // Plan every torn pair up front, keyed by the SURVIVOR's identity so the
    // insertion passes below can match it by direct lookup. Inline and
    // opaque-block survivors are planned separately: the missing half re-enters
    // an inline survivor's OWN segment, but an opaque-block survivor's ADJACENT
    // paragraph, so the two need different insertion walks.
    let mut inline_repairs: HashMap<RepairKey, InlineNode> = HashMap::new();
    let mut opaque_repairs: HashMap<RepairKey, InlineNode> = HashMap::new();
    for (key, capture) in inventory {
        // Only pairs that were whole before this resolution can be TORN by it —
        // a half that was already lone in the input is the document's own state
        // and passes through untouched (mirrors the guard's inherited-vs-
        // introduced discipline).
        let (Some(start_half), Some(end_half)) = (&capture.start, &capture.end) else {
            continue;
        };
        let (start_now, end_now) = survivors.get(key).copied().unwrap_or((false, false));
        // Both survive → range merely shrank, still paired. Both gone → the
        // whole range was inside resolved content, correctly removed. Neither is
        // a tear.
        if start_now == end_now {
            continue;
        }
        // Exactly one half survives. The MISSING half must be the inline one: an
        // opaque marker block has Normal status and is never removed, so it can
        // only ever be the survivor. Re-insert the removed inline half adjacent
        // to whatever survived — an inline survivor or a body-level opaque
        // marker block.
        let (survivor_role, survivor_half, missing_half) = if start_now {
            (RangeRole::Start, start_half, end_half)
        } else {
            (RangeRole::End, end_half, start_half)
        };
        let Some(partner) = missing_half.inline() else {
            // Both halves were opaque — impossible to tear (neither is removed);
            // defensively skip rather than fabricate.
            continue;
        };
        let repair_key = (key.0, key.1.clone(), survivor_role);
        match survivor_half {
            RangeHalf::Inline(_) => {
                inline_repairs.insert(repair_key, partner.clone());
            }
            RangeHalf::OpaqueBlock => {
                opaque_repairs.insert(repair_key, partner.clone());
            }
        }
    }
    if !inline_repairs.is_empty() {
        insert_inline_partners(blocks, &inline_repairs);
    }
    if !opaque_repairs.is_empty() {
        insert_opaque_partners(blocks, &opaque_repairs);
    }
}

/// Single-walk twin of the historical per-pair `insert_partner_adjacent`: place
/// each planned inline partner adjacent to its surviving inline half in ONE walk
/// of `blocks` (recursing table cells). A partner collapses the range to a point
/// — right AFTER a surviving Start, right BEFORE a surviving End. Within one
/// segment, all insertions are applied highest-index-first so each recorded
/// survivor index stays valid until it is consumed (multi-insert stability).
fn insert_inline_partners(blocks: &mut [TrackedBlock], repairs: &HashMap<RepairKey, InlineNode>) {
    fn insert_in_segment(seg: &mut TrackedSegment, repairs: &HashMap<RepairKey, InlineNode>) {
        // Collect (insertion index, partner) for every survivor in this segment,
        // scanning left-to-right, then apply descending so an earlier insertion
        // never shifts a not-yet-used index. Both steps are position-driven, so
        // the result does not depend on the repair map's iteration order.
        let mut planned: Vec<(usize, InlineNode)> = Vec::new();
        for idx in 0..seg.inlines.len() {
            let Some((family, id, role)) = classify_range_pair_marker(&seg.inlines[idx]) else {
                continue;
            };
            // The repair map is keyed by the SURVIVING role, so a hit means this
            // marker is the survivor and `role` is its (surviving) role.
            let Some(partner) = repairs.get(&(family, id, role)) else {
                continue;
            };
            // Stamp the survivor's origin onto the re-inserted half so the
            // bookmark id policy treats both identically (a base survivor keeps
            // its id verbatim; the collapsed partner must too, or the pair would
            // remap apart).
            let survivor_origin = match &seg.inlines[idx] {
                InlineNode::Decoration(d) => d.origin.clone(),
                _ => None,
            };
            let mut partner = partner.clone();
            if let InlineNode::Decoration(d) = &mut partner {
                d.origin = survivor_origin;
            }
            let at = match role {
                RangeRole::Start => idx + 1,
                RangeRole::End => idx,
            };
            planned.push((at, partner));
        }
        planned.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
        for (at, partner) in planned {
            seg.inlines.insert(at, partner);
        }
    }
    fn visit(block: &mut BlockNode, repairs: &HashMap<RepairKey, InlineNode>) {
        match block {
            BlockNode::Paragraph(p) => {
                for seg in &mut p.segments {
                    insert_in_segment(seg, repairs);
                }
            }
            BlockNode::Table(t) => {
                for row in &mut t.rows {
                    for cell in &mut row.cells {
                        for block in &mut cell.blocks {
                            visit(block, repairs);
                        }
                    }
                }
            }
            BlockNode::OpaqueBlock(_) => {}
        }
    }
    for tb in blocks.iter_mut() {
        visit(&mut tb.block, repairs);
    }
}

/// Single-walk twin of the historical per-pair `insert_partner_adjacent_to_opaque_block`:
/// collapse to a point when the SURVIVING half is a body-level opaque marker
/// block (its inline partner was removed by resolution). The re-materialized
/// inline partner goes into the paragraph adjacent to the opaque block — right
/// AFTER it for a surviving Start, right BEFORE it for a surviving End. Opaque
/// markers are direct `w:body` children (never inside a table cell), so only
/// top-level blocks are searched, in document order (deterministic). Inserting a
/// segment never changes block indices, so targets collected in the read pass
/// stay valid through the write pass.
fn insert_opaque_partners(blocks: &mut [TrackedBlock], repairs: &HashMap<RepairKey, InlineNode>) {
    let is_para = |tb: &TrackedBlock| matches!(tb.block, BlockNode::Paragraph(_));
    // Read pass (immutable): locate each planned opaque survivor and the adjacent
    // paragraph its partner collapses into.
    let mut planned: Vec<(usize, RangeRole, InlineNode)> = Vec::new();
    for (i, tb) in blocks.iter().enumerate() {
        let BlockNode::OpaqueBlock(o) = &tb.block else {
            continue;
        };
        let Some((family, id, role)) = opaque_range_marker(o) else {
            continue;
        };
        let Some(partner) = repairs.get(&(family, id, role)) else {
            continue;
        };
        // The nearest paragraph on the collapse side: after the anchor for a
        // surviving Start (End collapses to just past it), before it for an End.
        let target = match role {
            RangeRole::Start => (i + 1..blocks.len()).find(|&j| is_para(&blocks[j])),
            RangeRole::End => (0..i).rev().find(|&j| is_para(&blocks[j])),
        };
        if let Some(target) = target {
            planned.push((target, role, partner.clone()));
        }
    }
    // Write pass (mutable): re-materialize each partner as a point segment.
    for (target, role, partner) in planned {
        let BlockNode::Paragraph(p) = &mut blocks[target].block else {
            unreachable!("target index was filtered to a paragraph");
        };
        let point_segment = TrackedSegment {
            status: TrackingStatus::Normal,
            inlines: vec![partner],
        };
        match role {
            RangeRole::Start => p.segments.insert(0, point_segment),
            RangeRole::End => p.segments.push(point_segment),
        }
    }
}

/// A snapshot of a story's whole range-marker pairs, taken BEFORE a physical
/// removal so [`repair_torn_range_markers`] can tell a pair the removal TORE
/// (one half gone) from a half that was already lone in the input.
///
/// This is the reuse seam for the DIRECT edit path. Accept/reject resolution
/// removes content by projection and repairs torn pairs inline (see
/// `collapse_resolution_torn_range_markers`). A Direct-mode edit that PHYSICALLY
/// removes content — a table row/column delete, a block-range delete, a
/// block-range replace's delete leg — has the same failure mode: the removed
/// content may hold one half of a bookmark/comment/permission pair whose other
/// half lives in surviving content, tearing the pair (ECMA-376 §17.13.6). The
/// post-serialization pairing guard then refuses the document. Wrapping the edit
/// with a snapshot/repair collapses that torn pair to a point at the survivor —
/// what Word does when a bookmarked range's interior is deleted — through the
/// SAME capture/collapse code, so the two paths cannot drift.
pub(crate) struct RangeMarkerSnapshot(HashMap<RangePairKey, RangePairCapture>);

/// Capture the whole range-marker pairs of `blocks` before a physical removal.
pub(crate) fn snapshot_range_markers(blocks: &[TrackedBlock]) -> RangeMarkerSnapshot {
    RangeMarkerSnapshot(capture_range_pair_inventory(blocks))
}

/// Collapse to a point any pair `snapshot` recorded as whole that `blocks` now
/// leaves with exactly one surviving half. A no-op when nothing tore.
pub(crate) fn repair_torn_range_markers(
    blocks: &mut [TrackedBlock],
    snapshot: &RangeMarkerSnapshot,
) {
    collapse_resolution_torn_range_markers(blocks, &snapshot.0);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolveSelectionAction {
    Accept,
    Reject,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SelectedTrackingOutcome {
    KeepTracked,
    KeepNormal,
    Drop,
    /// The content survives with a DIFFERENT tracked status — the stacked
    /// state's partial resolutions (origin rules 1 and 4):
    /// accepting the insertion leaves the deletion pending over base text;
    /// rejecting the deletion restores the plain insertion.
    Transition(TrackingStatus),
}

fn selected_tracking_outcome(
    status: &TrackingStatus,
    action: ResolveSelectionAction,
    selected_revision_ids: &HashSet<u32>,
) -> SelectedTrackingOutcome {
    match status {
        TrackingStatus::Normal => SelectedTrackingOutcome::KeepNormal,
        TrackingStatus::Inserted(rev) => {
            if !selected_revision_ids.contains(&rev.identity) {
                SelectedTrackingOutcome::KeepTracked
            } else if action == ResolveSelectionAction::Accept {
                SelectedTrackingOutcome::KeepNormal
            } else {
                SelectedTrackingOutcome::Drop
            }
        }
        TrackingStatus::Deleted(rev) => {
            if !selected_revision_ids.contains(&rev.identity) {
                SelectedTrackingOutcome::KeepTracked
            } else if action == ResolveSelectionAction::Accept {
                SelectedTrackingOutcome::Drop
            } else {
                SelectedTrackingOutcome::KeepNormal
            }
        }
        // The stacked state resolves by the four origin rules ("a deletion
        // remembers what it deletes"), enumerated below.
        // A single call carries ONE action over a SET of ids, so mixed
        // resolutions are two sequential calls; the rules commute.
        TrackingStatus::InsertedThenDeleted(sr) => {
            let ins_selected = selected_revision_ids.contains(&sr.inserted.identity);
            let del_selected = selected_revision_ids.contains(&sr.deleted.identity);
            match (ins_selected, del_selected, action) {
                (false, false, _) => SelectedTrackingOutcome::KeepTracked,
                // Rule 3: accept the deletion => the text is gone, regardless
                // of what happens to the insertion (accept-both also drops).
                (_, true, ResolveSelectionAction::Accept) => SelectedTrackingOutcome::Drop,
                // Rule 2: reject the insertion => drop; the nested deletion
                // goes with it (the Word cascade — enumerated by the caller).
                (true, _, ResolveSelectionAction::Reject) => SelectedTrackingOutcome::Drop,
                // Rule 1: accept ONLY the insertion => its content becomes
                // base; the deletion now targets base text.
                (true, false, ResolveSelectionAction::Accept) => {
                    SelectedTrackingOutcome::Transition(TrackingStatus::Deleted(sr.deleted.clone()))
                }
                // Rule 4: reject ONLY the deletion => restore the origin
                // state: a plain pending insertion.
                (false, true, ResolveSelectionAction::Reject) => {
                    SelectedTrackingOutcome::Transition(TrackingStatus::Inserted(
                        sr.inserted.clone(),
                    ))
                }
            }
        }
    }
}

fn selected_optional_tracking_outcome(
    status: &Option<TrackingStatus>,
    action: ResolveSelectionAction,
    selected_revision_ids: &HashSet<u32>,
) -> SelectedTrackingOutcome {
    match status {
        Some(status) => selected_tracking_outcome(status, action, selected_revision_ids),
        None => SelectedTrackingOutcome::KeepNormal,
    }
}

/// Whether `status` carries a revision id present in `selected_revision_ids`
/// — i.e. whether this status is one the selector will actually act on,
/// regardless of `action` (membership, not the accept/reject branch, is what
/// decides "touched"). Used to gate cache invalidation: a hyperlink whose
/// runs carry no selected id must be left byte-identical, not just
/// logically unchanged (the canonicalization-fidelity concern — an
/// unrelated selective resolve call must not force a hyperlink's cached
/// `raw_xml` to be rebuilt from `runs`).
fn status_carries_selected_id(
    status: &TrackingStatus,
    selected_revision_ids: &HashSet<u32>,
) -> bool {
    match status {
        TrackingStatus::Normal => false,
        TrackingStatus::Inserted(r) | TrackingStatus::Deleted(r) => {
            selected_revision_ids.contains(&r.identity)
        }
        TrackingStatus::InsertedThenDeleted(sr) => {
            selected_revision_ids.contains(&sr.inserted.identity)
                || selected_revision_ids.contains(&sr.deleted.identity)
        }
    }
}

/// Selective counterpart to `project_hyperlink_runs`: resolves only the runs
/// whose status carries a selected revision id, transitioning each exactly as
/// `selected_tracking_outcome` prescribes (so a stacked run resolves via the
/// same D1 origin rules as everything else) and leaving unselected runs'
/// tracked status untouched. Mirrors the segment-level selective machinery at
/// `project_block_for_selected_resolution` so full and selective resolution
/// cannot diverge on a hyperlink's inner runs.
fn project_hyperlink_runs_for_selected_resolution(
    data: &mut crate::domain::HyperlinkData,
    action: ResolveSelectionAction,
    selected_revision_ids: &HashSet<u32>,
) {
    data.runs.retain(|run| {
        selected_tracking_outcome(&run.status, action, selected_revision_ids)
            != SelectedTrackingOutcome::Drop
    });
    for run in &mut data.runs {
        match selected_tracking_outcome(&run.status, action, selected_revision_ids) {
            SelectedTrackingOutcome::KeepNormal => {
                run.status = TrackingStatus::Normal;
            }
            SelectedTrackingOutcome::Transition(new_status) => {
                run.status = new_status;
            }
            SelectedTrackingOutcome::KeepTracked | SelectedTrackingOutcome::Drop => {}
        }
    }
    data.text = data.runs.iter().map(|r| r.text.as_str()).collect();
}

fn para_mark_needs_selected_merge(
    status: &Option<TrackingStatus>,
    action: ResolveSelectionAction,
    selected_revision_ids: &HashSet<u32>,
) -> bool {
    // The paragraphs join exactly when this resolution DROPS the break:
    // accepting its deletion, rejecting its insertion, or (for a stacked
    // mark) any origin rule that settles both claims against the break.
    selected_optional_tracking_outcome(status, action, selected_revision_ids)
        == SelectedTrackingOutcome::Drop
}

fn paragraph_has_unresolved_tracking(paragraph: &ParagraphNode) -> bool {
    if matches!(
        paragraph.para_mark_status,
        Some(TrackingStatus::Inserted(_))
            | Some(TrackingStatus::Deleted(_))
            | Some(TrackingStatus::InsertedThenDeleted(_))
    ) {
        return true;
    }
    paragraph
        .segments
        .iter()
        .any(|segment| !matches!(segment.status, TrackingStatus::Normal))
}

fn merge_marked_paragraphs_bare_selected(
    blocks: &mut Vec<BlockNode>,
    action: ResolveSelectionAction,
    selected_revision_ids: &HashSet<u32>,
) {
    let mut i = 0;
    while i < blocks.len() {
        let needs_merge = match &blocks[i] {
            BlockNode::Paragraph(p) => {
                para_mark_needs_selected_merge(&p.para_mark_status, action, selected_revision_ids)
                    && !p.para_split
            }
            _ => false,
        };
        if !needs_merge {
            i += 1;
            continue;
        }
        // Step over zero-width markers and any table this selection empties of
        // every row, but stop at any other block (real content blocks the
        // join) — mirrors the full bare path and the tracked selective path.
        let mut next_para_idx = None;
        for (offset, b) in blocks[(i + 1)..].iter().enumerate() {
            if matches!(b, BlockNode::Table(t) if table_emptied_by_selected(t, action, selected_revision_ids))
            {
                continue;
            }
            if matches!(b, BlockNode::Paragraph(_)) {
                next_para_idx = Some(i + 1 + offset);
                break;
            }
            if is_zero_width_marker_block(b) {
                continue;
            }
            break;
        }
        let Some(j) = next_para_idx else {
            // Blocked merge: drop a donor this selection leaves empty rather
            // than keeping an empty husk; a donor with surviving content stays.
            let drop_empty = i + 1 < blocks.len()
                && matches!(&blocks[i],
                    BlockNode::Paragraph(p) if paragraph_emptied_by_selected(p, action, selected_revision_ids));
            if drop_empty {
                blocks.remove(i);
            } else {
                i += 1;
            }
            continue;
        };
        let donor_segments = match &mut blocks[i] {
            BlockNode::Paragraph(p) => std::mem::take(&mut p.segments),
            _ => unreachable!(),
        };
        match &mut blocks[j] {
            BlockNode::Paragraph(target) => {
                let mut merged = donor_segments;
                merged.append(&mut target.segments);
                target.segments = merged;
            }
            _ => unreachable!(),
        }
        blocks.remove(i);
    }
}

fn tracked_block_survives_selected(
    status: &TrackingStatus,
    action: ResolveSelectionAction,
    selected_revision_ids: &HashSet<u32>,
) -> bool {
    selected_tracking_outcome(status, action, selected_revision_ids)
        != SelectedTrackingOutcome::Drop
}

/// Selective-path counterpart of `table_emptied_by_accept_reject`: a table this
/// selection empties completely (it had rows and every one is dropped by the
/// selection), so `project_blocks_for_selected_resolution` removes the rowless
/// shell. A paragraph-mark merge joins ACROSS such a table, matching the full
/// accept/reject path and Word.
fn table_emptied_by_selected(
    t: &TableNode,
    action: ResolveSelectionAction,
    selected_revision_ids: &HashSet<u32>,
) -> bool {
    !t.rows.is_empty()
        && t.rows.iter().all(|row| {
            selected_optional_tracking_outcome(&row.tracking_status, action, selected_revision_ids)
                == SelectedTrackingOutcome::Drop
        })
}

/// Whether a top-level block is removed ENTIRELY by this selection — dropped at
/// the block level, or a table emptied of every row. The paragraph-mark merge
/// steps over such blocks when searching for its join target.
fn tracked_block_removed_by_selected(
    tb: &TrackedBlock,
    action: ResolveSelectionAction,
    selected_revision_ids: &HashSet<u32>,
) -> bool {
    !tracked_block_survives_selected(&tb.status, action, selected_revision_ids)
        || matches!(&tb.block, BlockNode::Table(t) if table_emptied_by_selected(t, action, selected_revision_ids))
}

/// Selective-path counterpart of `paragraph_emptied_by_accept_reject`: whether
/// this selection EMPTIES the paragraph — it had inline content and the
/// selection drops every bit of it — with no structural content to survive. A
/// paragraph that was already empty in the base is NOT emptied and must survive.
fn paragraph_emptied_by_selected(
    p: &ParagraphNode,
    action: ResolveSelectionAction,
    selected_revision_ids: &HashSet<u32>,
) -> bool {
    if p.literal_prefix.is_some() || p.section_properties.is_some() {
        return false;
    }
    let had_content = p.segments.iter().any(|seg| !seg.inlines.is_empty());
    let keeps_content = p.segments.iter().any(|seg| {
        selected_tracking_outcome(&seg.status, action, selected_revision_ids)
            != SelectedTrackingOutcome::Drop
            && !seg.inlines.is_empty()
    });
    had_content && !keeps_content
}

fn merge_marked_paragraphs_tracked_selected(
    blocks: &mut Vec<TrackedBlock>,
    action: ResolveSelectionAction,
    selected_revision_ids: &HashSet<u32>,
) {
    let mut i = 0;
    while i < blocks.len() {
        let needs_merge = match &blocks[i].block {
            BlockNode::Paragraph(p) => {
                tracked_block_survives_selected(&blocks[i].status, action, selected_revision_ids)
                    && para_mark_needs_selected_merge(
                        &p.para_mark_status,
                        action,
                        selected_revision_ids,
                    )
                    && !p.para_split
            }
            _ => false,
        };
        if !needs_merge {
            i += 1;
            continue;
        }
        // Same join-target search as the full accept/reject path: step over
        // blocks this selection removes entirely (a dropped block-level wrapper
        // or a table emptied of every row) and zero-width markers, stopping at
        // any other surviving block (real content blocks the join).
        let mut next_para_idx = None;
        for (offset, b) in blocks[(i + 1)..].iter().enumerate() {
            if tracked_block_removed_by_selected(b, action, selected_revision_ids) {
                continue;
            }
            if matches!(&b.block, BlockNode::Paragraph(_)) {
                next_para_idx = Some(i + 1 + offset);
                break;
            }
            if is_zero_width_marker_block(&b.block) {
                continue;
            }
            break;
        }
        let Some(j) = next_para_idx else {
            // Blocked merge: drop a donor this selection leaves empty rather
            // than keeping an empty husk; a donor with surviving content stays.
            let drop_empty = i + 1 < blocks.len()
                && matches!(&blocks[i].block,
                    BlockNode::Paragraph(p) if paragraph_emptied_by_selected(p, action, selected_revision_ids));
            if drop_empty {
                blocks.remove(i);
            } else {
                i += 1;
            }
            continue;
        };
        let (left, right) = blocks.split_at_mut(j);
        match (&mut left[i].block, &mut right[0].block) {
            (BlockNode::Paragraph(donor), BlockNode::Paragraph(target)) => {
                merge_paragraph_into_following(donor, target);
            }
            _ => unreachable!(),
        }
        blocks.remove(i);
    }
}

// ─── Canonical revision enumeration ──────────────────────────────────────────

/// WHICH OOXML change element carries a pending revision — the census `kind`.
///
/// Insert/delete cover every `w:ins`/`w:del` carrier (inline segments, whole
/// blocks, table rows/cells, paragraph marks, hyperlink runs, comment
/// stories); the format variants name the specific `*PrChange` element, so a
/// caller can tell "run formatting changed" from "the section layout changed"
/// without parsing the human excerpt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RevisionKind {
    /// `w:ins` — inserted content.
    Insert,
    /// `w:del` — deleted content.
    Delete,
    /// `w:rPrChange` — run formatting change (§17.13.5.31).
    FormatRun,
    /// `w:pPrChange` — paragraph formatting change (§17.13.5.29).
    FormatParagraph,
    /// `w:tblPrChange` — table formatting change (§17.13.5.34).
    FormatTable,
    /// `w:trPrChange` — table row formatting change (§17.13.5.37).
    FormatRow,
    /// `w:tcPrChange` — table cell formatting change (§17.13.5.36).
    FormatCell,
    /// `w:sectPrChange` — section formatting change (§17.13.5.32), both the
    /// body-level and mid-document paragraph-level carriers.
    FormatSection,
    /// A tracked change living INSIDE opaque content (a textbox's
    /// `w:txbxContent`, an inline content control, an embedded object, or a
    /// quarantined stacked block). The KIND is retained so consumers know the
    /// change lives inside opaque content, but resolvability is now a property of
    /// the record's `revision_id`, not the kind (RFC-0002 §Phase-3b): a
    /// WELL-FORMED interior revision (a top-level `w:ins`/`w:del`/`w:moveFrom`/
    /// `w:moveTo` with a real `w:id`) carries that id and IS individually
    /// resolvable — the selective resolver descends into `raw_xml` by id. An
    /// unresolvable one (stacked, `*PrChange`, id-less, unparseable) carries
    /// `revision_id == 0` (census-only). Either way it is enumerated, so the
    /// inventory never lies.
    OpaqueInterior,
    /// A tracked MOVE (`w:moveFrom`/`w:moveTo`, §17.13.5.21-26) — ONE user
    /// intention owning several wire carriers (source content + source pilcrow,
    /// destination clone). Under RFC-0004 §H7 all those carriers share one
    /// engine-minted identity and the move enumerates as ONE record of this
    /// kind, so selecting it resolves the whole move atomically (matching Word,
    /// where accepting a move is one action).
    Move,
}

impl RevisionKind {
    /// The wire name, as emitted by `list_revisions` rows and audit reports.
    pub fn as_str(self) -> &'static str {
        match self {
            RevisionKind::Insert => "insert",
            RevisionKind::Delete => "delete",
            RevisionKind::FormatRun => "format_run",
            RevisionKind::FormatParagraph => "format_paragraph",
            RevisionKind::FormatTable => "format_table",
            RevisionKind::FormatRow => "format_row",
            RevisionKind::FormatCell => "format_cell",
            RevisionKind::FormatSection => "format_section",
            RevisionKind::OpaqueInterior => "opaque_interior",
            RevisionKind::Move => "move",
        }
    }

    /// Parse a wire name back to the kind. `None` for anything that is not
    /// exactly one of the wire names — the caller decides how to refuse.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "insert" => RevisionKind::Insert,
            "delete" => RevisionKind::Delete,
            "format_run" => RevisionKind::FormatRun,
            "format_paragraph" => RevisionKind::FormatParagraph,
            "format_table" => RevisionKind::FormatTable,
            "format_row" => RevisionKind::FormatRow,
            "format_cell" => RevisionKind::FormatCell,
            "format_section" => RevisionKind::FormatSection,
            "opaque_interior" => RevisionKind::OpaqueInterior,
            "move" => RevisionKind::Move,
            _ => return None,
        })
    }

    /// True for every `*PrChange` variant (the old collapsed "format" group).
    /// `OpaqueInterior` is NOT a formatting change (it is a whole-carrier marker
    /// for markup inside opaque content), so it is excluded.
    pub fn is_format(self) -> bool {
        matches!(
            self,
            RevisionKind::FormatRun
                | RevisionKind::FormatParagraph
                | RevisionKind::FormatTable
                | RevisionKind::FormatRow
                | RevisionKind::FormatCell
                | RevisionKind::FormatSection
        )
    }

    /// Whether this kind is a MODELED (non-opaque-interior) revision. Retained
    /// for callers that want the kind distinction; it is NO LONGER the
    /// resolvability predicate. Since RFC-0002 §Phase-3b resolvability is a
    /// property of the RECORD — `RevisionRecord::revision_id != 0` — because a
    /// well-formed `OpaqueInterior` revision is now individually resolvable too.
    /// The enumerate↔resolvable agreement is keyed on `revision_id != 0`.
    pub fn is_modeled(self) -> bool {
        !matches!(self, RevisionKind::OpaqueInterior)
    }
}

impl std::fmt::Display for RevisionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One enumerable pending revision — the census row the resolution surface is
/// built on. THE CONTRACT (spec_revision_enumeration.rs): every revision in
/// the serialized markup appears here, because an un-enumerable revision is
/// one no selector can resolve ("accept all of author X" would silently leave
/// its markup behind).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevisionRecord {
    /// The ENGINE-MINTED revision IDENTITY (RFC-0004 §H7) — the address
    /// `Resolution::Selective` resolves and the resolvability predicate:
    /// `!= 0` ⟺ individually selectable (in `resolvable_revision_ids`). 0 marks
    /// a reported-but-never-selectable record: legacy pre-identity records
    /// (snapshots serialized before identity existed), and UNRESOLVABLE
    /// opaque-interior records (stacked / `*PrChange` / id-less / unparseable).
    /// A WELL-FORMED `OpaqueInterior` revision carries its real id and IS
    /// resolvable (RFC-0002 §Phase-3b). A caller must not feed a 0 to
    /// accept/reject. NOTE: unique WITHIN a `Document` instance, stable across
    /// its projections, and stable across save/reopen of a semantically
    /// unchanged Stemma-produced artifact. Producer-neutral audit uses explicit
    /// correspondence and never treats [`RevisionRecord::wire_id`] as a key.
    pub revision_id: u32,
    /// The raw OOXML wire `w:id` this revision imported with (`0` for an
    /// opaque-interior census-only record with no numeric id). A per-element
    /// annotation Word does NOT keep unique — NEVER an address (that is
    /// `revision_id`). Kept for diagnostics only. For a collapsed `Move` record
    /// it is the first carrier's wire id.
    pub wire_id: u32,
    /// `w:author`, when the markup carried one.
    pub author: Option<String>,
    /// ISO-8601 date, when carried.
    pub date: Option<String>,
    /// Which OOXML change element carries this revision.
    pub kind: RevisionKind,
    /// The TOP-LEVEL block hosting the revision (table revisions report the
    /// table's block id). For a body-level `w:sectPrChange`, which has no
    /// hosting block, this is the sentinel id `"body_section"` — synthetic,
    /// never a real paragraph/table id, and disambiguated by `location`
    /// (`StoryScope::Body`) for any caller that needs to tell it apart from
    /// an actual block.
    pub block_id: NodeId,
    /// WHICH story this revision lives in — the main body, a header/footer
    /// by part path, a footnote/endnote/comment by id. A caller cannot
    /// resolve or display a revision without knowing this: story block ids
    /// are only addressable through their story (see `edit::story_addr`),
    /// and a body-level `w:sectPrChange` has no block id to address it by
    /// at all.
    pub location: StoryScope,
    /// Human excerpt: affected visible text, or a descriptor for non-text
    /// revisions ("¶ paragraph mark", "formatting", row/cell labels).
    pub excerpt: String,
}

/// The sentinel `block_id` for a body-level `w:sectPrChange`, which has no
/// hosting block. Never collides with a real id (paragraph/table ids come
/// from the importer's `p_N`/`tbl_N` counters or authoring-time allocation,
/// never this literal string); disambiguated from a real block id by
/// `RevisionRecord::location` being `StoryScope::Body` with
/// `kind == RevisionKind::FormatSection`.
const BODY_SECTION_SENTINEL_BLOCK_ID: &str = "body_section";

/// The sentinel `block_id` for a comment STORY-level tracking status
/// (`CommentStory::tracking_status` — the whole-comment insert/delete marker
/// accept_all/reject_all act on), which applies to the story, not to any one
/// block inside it. Same non-collision argument as
/// [`BODY_SECTION_SENTINEL_BLOCK_ID`]; disambiguated by
/// `RevisionRecord::location` being `StoryScope::Comment`.
const COMMENT_STORY_SENTINEL_BLOCK_ID: &str = "comment_story";

/// Walk every revision in the document, in document order: the BODY (block
/// statuses, inline segment statuses — both legs of a stacked pair —,
/// hyperlink run statuses, paragraph marks, run/paragraph/table/row/cell
/// formatting changes, section-property changes — the body-level
/// `w:sectPrChange` under a sentinel block id and the mid-document
/// paragraph-level sibling under its host paragraph —, table row/cell
/// structural statuses, and cell-interior content, recursively through
/// nested tables), plus every STORY: headers, footers, footnotes, endnotes,
/// and comments (their blocks carry the identical `TrackedBlock`/
/// `TrackingStatus` shape as body paragraphs — the same walk, re-tagged with
/// the story's `StoryScope`; a comment's whole-story tracking status is
/// reported under the `comment_story` sentinel block id).
///
/// For the RESOLVABLE kinds this walks exactly the carrier set
/// `resolvable_revision_ids` accepts — EVERY story: body, headers, footers,
/// footnotes, endnotes, and comments (including the comment STORY-level
/// tracking status, under a sentinel block id). Those two must not diverge, or
/// a caller gains a revision it cannot resolve, or resolves one the listing
/// hides (`enumerate_revisions_ids_agree_with_resolvable_revision_ids` pins the
/// agreement, on the `revision_id != 0` set — which now includes well-formed
/// interior revisions, RFC-0002 §Phase-3b).
///
/// It ALSO emits [`RevisionKind::OpaqueInterior`] records for tracked changes
/// hiding inside opaque content — a textbox's `w:txbxContent`, an embedded
/// object, or a quarantined stacked-revision block — which the modeled tree
/// never enters. These are honest census entries (visible so the inventory does
/// not lie) but NOT resolvable: they carry `revision_id == 0` and are
/// deliberately absent from `resolvable_revision_ids`, so the agreement above
/// holds on the resolvable subset while the census stays complete. This is the
/// single enumeration the MCP's `list_revisions`, accept/reject selector
/// lowering, and the audit census share.
pub fn enumerate_revisions(doc: &CanonDoc) -> Vec<RevisionRecord> {
    let mut out = Vec::new();
    enumerate_blocks_revisions(&mut out, &doc.blocks, &StoryScope::Body);
    // (move-group collapse happens after every carrier record is built — see the
    // `collapse_move_records` call at the tail.)
    if let Some(change) = &doc.body_section_property_change {
        out.push(RevisionRecord {
            revision_id: change.revision.identity,
            wire_id: change.revision.revision_id,
            author: change.revision.author.clone(),
            date: change.revision.date.clone(),
            kind: RevisionKind::FormatSection,
            block_id: NodeId::from(BODY_SECTION_SENTINEL_BLOCK_ID),
            location: StoryScope::Body,
            excerpt: "section formatting".to_string(),
        });
    }
    for story in &doc.headers {
        enumerate_blocks_revisions(
            &mut out,
            &story.blocks,
            &StoryScope::Header {
                part_path: story.part_name.clone(),
                kind: story.kind.clone(),
            },
        );
    }
    for story in &doc.footers {
        enumerate_blocks_revisions(
            &mut out,
            &story.blocks,
            &StoryScope::Footer {
                part_path: story.part_name.clone(),
                kind: story.kind.clone(),
            },
        );
    }
    for story in &doc.footnotes {
        enumerate_blocks_revisions(
            &mut out,
            &story.blocks,
            &StoryScope::Footnote {
                id: story.id.clone(),
            },
        );
    }
    for story in &doc.endnotes {
        enumerate_blocks_revisions(
            &mut out,
            &story.blocks,
            &StoryScope::Endnote {
                id: story.id.clone(),
            },
        );
    }
    for story in &doc.comments {
        let location = StoryScope::Comment {
            id: story.id.clone(),
        };
        // The whole-comment tracking status (the marker comment_delete writes
        // and accept_all/reject_all resolve) has no hosting block — report it
        // under the story sentinel, mirroring the body-level sectPrChange.
        if let Some(status) = &story.tracking_status {
            push_status_records(
                &mut out,
                status,
                &NodeId::from(COMMENT_STORY_SENTINEL_BLOCK_ID),
                &location,
                &comment_text_excerpt(story),
            );
        }
        enumerate_blocks_revisions(&mut out, &story.blocks, &location);
    }
    // Opaque-interior census: a SINGLE post-pass over the one shared opaque-
    // interior walk (`opaque_targets::visit_opaque_interiors`) — the same walk
    // text-target discovery uses, so the census and the edit surface can never
    // disagree on which opaques carry an interior (RFC-0002 §decision-1). A
    // well-formed interior carrier whose id uniquely identifies it
    // document-wide reports that real id (individually selectable); everything
    // else — stacked, *PrChange, move-pair, id-less, or an id shared with
    // another revision (`classify_interior_ids`) — reports the census-only
    // sentinel 0. Without this the inventory could report zero while a textbox
    // hides twenty live tracked changes.
    let interior_demoted = classify_interior_ids(doc).demoted;
    crate::opaque_targets::visit_opaque_interiors(doc, &mut |block_id, location, interior| {
        match interior {
            crate::opaque_targets::OpaqueInteriorRef::Inline(o) => {
                // A hyperlink's tracked runs are already modeled and resolvable
                // (surfaced in the main walk); only NON-hyperlink opaques keep
                // interior revisions in `raw_xml`.
                if !matches!(o.kind, OpaqueKind::Hyperlink(_))
                    && let Some(raw) = &o.raw_xml
                {
                    // Attribute a TEXTBOX's interior revisions to a distinct
                    // TextFrame story (Word's "text frame") so a per-story consumer
                    // sees them, not a false zero (RFC-0002 §Phase-3). A
                    // drawing carrying interior tracked changes IS a textbox; other
                    // opaque kinds (SDT, object) keep their hosting story.
                    let text_frame =
                        matches!(o.kind, OpaqueKind::Drawing).then(|| StoryScope::TextFrame {
                            anchor: o.id.clone(),
                        });
                    let loc = text_frame.as_ref().unwrap_or(location);
                    enumerate_opaque_interior_revisions(
                        &mut out,
                        raw,
                        block_id,
                        loc,
                        &interior_demoted,
                    );
                }
            }
            crate::opaque_targets::OpaqueInteriorRef::Block(o) => {
                enumerate_opaque_block_interior(&mut out, o, block_id, location);
            }
        }
    });
    collapse_move_records(&mut out, doc);
    out
}

/// The engine-minted identities that belong to a MOVE group: every status
/// carrier (block/segment/paragraph-mark) of a `TrackedBlock` that carries a
/// `move_id`. Because [`import::mint_identities`] groups a move's carriers onto
/// one identity, this is exactly the set of identities that enumerate as a
/// single `RevisionKind::Move` record. A `*PrChange` on a moved paragraph is a
/// separate intention (its own identity) and is deliberately NOT collected —
/// the mint walk never puts a formatting change in a move group.
fn move_group_identities(doc: &CanonDoc) -> HashSet<u32> {
    fn collect_status(status: &TrackingStatus, out: &mut HashMap<u32, u8>) {
        match status {
            TrackingStatus::Normal => {}
            TrackingStatus::Inserted(r) => {
                *out.entry(r.identity).or_default() |= 1;
            }
            TrackingStatus::Deleted(r) => {
                *out.entry(r.identity).or_default() |= 2;
            }
            TrackingStatus::InsertedThenDeleted(sr) => {
                *out.entry(sr.inserted.identity).or_default() |= 1;
                *out.entry(sr.deleted.identity).or_default() |= 2;
            }
        }
    }
    fn collect_move_block(tb: &TrackedBlock, out: &mut HashMap<u32, u8>) {
        collect_status(&tb.status, out);
        if let BlockNode::Paragraph(p) = &tb.block {
            for seg in &p.segments {
                collect_status(&seg.status, out);
            }
            if let Some(status) = &p.para_mark_status {
                collect_status(status, out);
            }
        }
    }
    fn walk(blocks: &[TrackedBlock], out: &mut HashMap<u32, u8>) {
        for tb in blocks {
            if tb.move_id.is_some() {
                collect_move_block(tb, out);
            }
        }
    }
    let mut polarities = HashMap::new();
    walk(&doc.blocks, &mut polarities);
    for story in &doc.headers {
        walk(&story.blocks, &mut polarities);
    }
    for story in &doc.footers {
        walk(&story.blocks, &mut polarities);
    }
    for story in &doc.footnotes {
        walk(&story.blocks, &mut polarities);
    }
    for story in &doc.endnotes {
        walk(&story.blocks, &mut polarities);
    }
    for story in &doc.comments {
        walk(&story.blocks, &mut polarities);
    }
    polarities
        .into_iter()
        .filter_map(|(identity, flags)| (flags == 3).then_some(identity))
        .collect()
}

/// Collapse the several per-carrier records of a move (source content, source
/// pilcrow, destination clone — all sharing one minted identity) into ONE
/// `RevisionKind::Move` record (RFC-0004 §H7 acceptance: "a move enumerates as
/// one record"). The record keeps the first-in-document-order carrier's block
/// id / location / authorship as its representative and its visible text as the
/// carrier detail; every further carrier of that identity is dropped, so the
/// move counts once. Non-move records pass through untouched. Idempotent for a
/// document with no moves (the identity set is empty).
fn collapse_move_records(out: &mut Vec<RevisionRecord>, doc: &CanonDoc) {
    let move_ids = move_group_identities(doc);
    if move_ids.is_empty() {
        return;
    }
    let mut seen: HashSet<u32> = HashSet::new();
    let mut collapsed = Vec::with_capacity(out.len());
    for rec in out.drain(..) {
        if move_ids.contains(&rec.revision_id) {
            if seen.insert(rec.revision_id) {
                let excerpt = format!("moved: {}", rec.excerpt);
                collapsed.push(RevisionRecord {
                    kind: RevisionKind::Move,
                    excerpt,
                    ..rec
                });
            }
            // Further carriers of the same move identity are subsumed.
        } else {
            collapsed.push(rec);
        }
    }
    *out = collapsed;
}

/// Excerpt for a whole-comment revision: the comment's visible text, block by
/// block — the same extraction `block_text_excerpt` uses for body blocks.
fn comment_text_excerpt(story: &crate::domain::CommentStory) -> String {
    let mut out = String::new();
    for tb in &story.blocks {
        out.push_str(&extract_block_text_for_hash(&tb.block));
        out.push(' ');
    }
    out.trim_end().to_string()
}

/// The shared per-block-list walk `enumerate_revisions` runs once for the
/// body and once per footnote/endnote story — identical traversal, only the
/// `location` tag differs (see `enumerate_revisions`'s doc comment for why
/// this generalizes rather than needing a parallel walk).
fn enumerate_blocks_revisions(
    out: &mut Vec<RevisionRecord>,
    blocks: &[TrackedBlock],
    location: &StoryScope,
) {
    for tb in blocks {
        let block_id = block_node_id(&tb.block);
        push_status_records(
            out,
            &tb.status,
            &block_id,
            location,
            &block_text_excerpt(&tb.block),
        );
        match &tb.block {
            BlockNode::Paragraph(p) => {
                enumerate_paragraph_revisions(out, p, &block_id, location);
            }
            BlockNode::Table(t) => {
                enumerate_table_revisions(out, t, &block_id, location);
            }
            BlockNode::OpaqueBlock(_) => {
                // Opaque-block interiors are censused in the shared post-pass in
                // `enumerate_revisions` (see the OpaqueInline arm's note).
            }
        }
    }
}

/// Walk one table's own revisions: its formatting change, each row/cell's
/// tracking status and formatting change, and cell-interior content.
/// Cell-interior paragraphs are reported under THEIR OWN id (not the
/// table's), so the review UI can target them for resolve/patch like a body
/// paragraph. A cell-interior table has no `TrackedBlock` wrapper of its own
/// (only top-level body blocks carry one) and so has no block-level status
/// to report — but it has its own formatting change and rows/cells, walked
/// here recursively under ITS OWN id. This mirrors `resolvable_revision_ids`'s
/// `visit_block` recursion into nested tables, so the two walks cannot
/// diverge on what a nested table carries.
fn enumerate_table_revisions(
    out: &mut Vec<RevisionRecord>,
    t: &TableNode,
    block_id: &NodeId,
    location: &StoryScope,
) {
    if let Some(fc) = &t.formatting_change {
        out.push(RevisionRecord {
            revision_id: fc.identity,
            wire_id: fc.revision_id,
            author: some_author(&fc.author),
            date: fc.date.clone(),
            kind: RevisionKind::FormatTable,
            block_id: block_id.clone(),
            location: location.clone(),
            excerpt: "table formatting".to_string(),
        });
    }
    for (ri, row) in t.rows.iter().enumerate() {
        if let Some(status) = &row.tracking_status {
            push_status_records(
                out,
                status,
                block_id,
                location,
                &format!("row[{ri}]: {}", row_text_excerpt(row)),
            );
        }
        if let Some(fc) = &row.formatting_change {
            out.push(RevisionRecord {
                revision_id: fc.identity,
                wire_id: fc.revision_id,
                author: some_author(&fc.author),
                date: fc.date.clone(),
                kind: RevisionKind::FormatRow,
                block_id: block_id.clone(),
                location: location.clone(),
                excerpt: format!("row[{ri}] formatting"),
            });
        }
        for (ci, cell) in row.cells.iter().enumerate() {
            if let Some(status) = &cell.tracking_status {
                push_status_records(out, status, block_id, location, &format!("cell[{ri},{ci}]"));
            }
            if let Some(fc) = &cell.formatting_change {
                out.push(RevisionRecord {
                    revision_id: fc.identity,
                    wire_id: fc.revision_id,
                    author: some_author(&fc.author),
                    date: fc.date.clone(),
                    kind: RevisionKind::FormatCell,
                    block_id: block_id.clone(),
                    location: location.clone(),
                    excerpt: format!("cell[{ri},{ci}] formatting"),
                });
            }
            for nested in &cell.blocks {
                match nested {
                    BlockNode::Paragraph(np) => {
                        enumerate_paragraph_revisions(out, np, &np.id, location);
                    }
                    BlockNode::Table(nt) => {
                        enumerate_table_revisions(out, nt, &nt.id, location);
                    }
                    BlockNode::OpaqueBlock(_) => {
                        // Censused in the shared post-pass (see enumerate_revisions).
                    }
                }
            }
        }
    }
}

fn enumerate_paragraph_revisions(
    out: &mut Vec<RevisionRecord>,
    p: &ParagraphNode,
    block_id: &NodeId,
    location: &StoryScope,
) {
    for segment in &p.segments {
        let text: String = segment
            .inlines
            .iter()
            .filter_map(|i| match i {
                InlineNode::Text(t) => Some(t.text.clone()),
                // A drawing-only deletion carries no text — label it so the review
                // card reads "[image]" instead of a blank excerpt.
                InlineNode::OpaqueInline(o)
                    if matches!(o.kind, crate::domain::OpaqueKind::Drawing) =>
                {
                    Some("[image]".to_string())
                }
                _ => None,
            })
            .collect();
        push_status_records(out, &segment.status, block_id, location, &text);
        for inline in &segment.inlines {
            match inline {
                InlineNode::Text(t) => {
                    if let Some(fc) = &t.formatting_change {
                        out.push(RevisionRecord {
                            revision_id: fc.identity,
                            wire_id: fc.revision_id,
                            author: some_author(&fc.author),
                            date: fc.date.clone(),
                            kind: RevisionKind::FormatRun,
                            block_id: block_id.clone(),
                            location: location.clone(),
                            excerpt: format!("run formatting: {}", t.text),
                        });
                    }
                }
                // A hyperlink's display text is tracked per-run, one layer
                // below the enclosing segment (`HyperlinkData.runs[*].status`
                // — the layer `ReplaceHyperlinkText` writes to, see the
                // type-level docs on `HyperlinkData`). Walk it here so those
                // ids reach the same census as ordinary text, mirroring
                // `resolvable_revision_ids`'s `visit_paragraph` — the
                // resolver already accepts these ids, so the listing must
                // surface them too. Label the excerpt "hyperlink: <run
                // text>" so the review card names the carrier.
                InlineNode::OpaqueInline(o) => {
                    if let crate::domain::OpaqueKind::Hyperlink(data) = &o.kind {
                        for run in &data.runs {
                            push_status_records(
                                out,
                                &run.status,
                                block_id,
                                location,
                                &format!("hyperlink: {}", run.text),
                            );
                        }
                    }
                    // Any OTHER opaque inline keeps its interior as verbatim
                    // `raw_xml` the modeled walk never enters; those interior
                    // revisions are censused in a single post-pass over
                    // `opaque_targets::visit_opaque_interiors` (see
                    // `enumerate_revisions`) — the ONE opaque-interior walk both
                    // this census and text-target discovery share, so they can
                    // never disagree on which opaques carry an interior.
                }
                _ => {}
            }
        }
    }
    if let Some(status) = &p.para_mark_status {
        push_status_records(out, status, block_id, location, "\u{00b6} paragraph mark");
    }
    if let Some(fc) = &p.formatting_change {
        out.push(RevisionRecord {
            revision_id: fc.identity,
            wire_id: fc.revision_id,
            author: some_author(&fc.author),
            date: fc.date.clone(),
            kind: RevisionKind::FormatParagraph,
            block_id: block_id.clone(),
            location: location.clone(),
            excerpt: "paragraph formatting".to_string(),
        });
    }
    // A mid-document section break's own w:sectPrChange (§17.13.5.32) — the
    // paragraph-level sibling of `doc.body_section_property_change`. Same
    // struct (`SectionPropertyChange`), found in passing while closing the
    // body-level gap: it has a real hosting block (this paragraph), so no
    // sentinel id is needed here.
    if let Some(change) = &p.section_property_change {
        out.push(RevisionRecord {
            revision_id: change.revision.identity,
            wire_id: change.revision.revision_id,
            author: change.revision.author.clone(),
            date: change.revision.date.clone(),
            kind: RevisionKind::FormatSection,
            block_id: block_id.clone(),
            location: location.clone(),
            excerpt: "section formatting".to_string(),
        });
    }
}

fn push_status_records(
    out: &mut Vec<RevisionRecord>,
    status: &TrackingStatus,
    block_id: &NodeId,
    location: &StoryScope,
    excerpt: &str,
) {
    let mut push = |r: &RevisionInfo, kind: RevisionKind| {
        out.push(RevisionRecord {
            revision_id: r.identity,
            wire_id: r.revision_id,
            author: r.author.clone(),
            date: r.date.clone(),
            kind,
            block_id: block_id.clone(),
            location: location.clone(),
            excerpt: excerpt.to_string(),
        });
    };
    match status {
        TrackingStatus::Normal => {}
        TrackingStatus::Inserted(r) => push(r, RevisionKind::Insert),
        TrackingStatus::Deleted(r) => push(r, RevisionKind::Delete),
        TrackingStatus::InsertedThenDeleted(sr) => {
            push(&sr.inserted, RevisionKind::Insert);
            push(&sr.deleted, RevisionKind::Delete);
        }
    }
}

fn some_author(author: &str) -> Option<String> {
    if author.is_empty() {
        None
    } else {
        Some(author.to_string())
    }
}

/// Build one opaque-interior census record. `kind == OpaqueInterior` always
/// (consumers know the change lives inside a textbox/SDT), but the `revision_id`
/// is now meaningful: a RESOLVABLE interior revision (a top-level, non-stacked
/// `w:ins`/`w:del`/`w:moveFrom`/`w:moveTo` with a numeric `w:id`) carries that
/// real id and is individually selectable; an unresolvable one (stacked,
/// `*PrChange`, id-less, or an unparseable fragment) carries `0` — the
/// never-selectable sentinel (RFC-0002 §Phase-3b). Resolvability is thus a
/// property of the markup, not of who authored it (which does not survive a
/// serialize/reload) — see the enumerate↔resolvable agreement.
fn opaque_interior_record(
    revision_id: u32,
    block_id: &NodeId,
    location: &StoryScope,
    author: Option<String>,
    date: Option<String>,
    excerpt: String,
) -> RevisionRecord {
    RevisionRecord {
        revision_id,
        wire_id: revision_id,
        author,
        date,
        kind: RevisionKind::OpaqueInterior,
        block_id: block_id.clone(),
        location: location.clone(),
        excerpt,
    }
}

/// One tracked-change carrier found inside an opaque fragment, classified for
/// resolvability. The SINGLE source of truth shared by the census
/// (`walk_opaque_interior_revisions`), the resolvable-id set
/// (`resolvable_interior_ids`), and id allocation (`max` scan) so they cannot
/// disagree on which interior revisions are selectable.
pub(crate) struct FragmentCarrier {
    /// Local name (`ins`/`del`/`moveFrom`/`moveTo`/`*PrChange`).
    pub name: &'static str,
    /// The `w:id`, if numeric.
    pub id: Option<u32>,
    /// Resolvable-by-id: a top-level (non-stacked) CONTENT revision with an id.
    pub resolvable: bool,
    pub author: Option<String>,
    pub date: Option<String>,
}

/// The interior carrier kinds that are individually resolvable BY ID:
/// `w:ins`/`w:del` only. A move is a PAIR (`w:moveFrom` + `w:moveTo`, each with
/// its own id, plus range markers) — resolving one half by id would orphan the
/// counterpart and its `w:move*Range*` delimiters, producing markup Word has to
/// repair. Interior moves therefore stay census-only (id 0) in v1; the bulk
/// accept-all/reject-all descent resolves them pair-correctly.
fn is_by_id_resolvable_name(name: &str) -> bool {
    matches!(name, "ins" | "del")
}

/// Walk a parsed opaque fragment, calling `f` for every tracked-change carrier,
/// classified. `has_rev_ancestor` marks a carrier nested inside another carrier
/// (stacked) — those are never individually resolvable.
pub(crate) fn visit_fragment_carriers(
    element: &xmltree::Element,
    has_rev_ancestor: bool,
    f: &mut impl FnMut(FragmentCarrier),
) {
    let mut child_has_ancestor = has_rev_ancestor;
    if let Some(name) = crate::normalize::REVISION_ELEMENT_LOCAL_NAMES
        .iter()
        .find(|name| crate::word_xml::is_w_tag(element, name))
    {
        let id = crate::xml_attrs::attr_get(element, "w:id").and_then(|v| v.parse::<u32>().ok());
        // Resolvable = a top-level `w:ins`/`w:del` with a NON-ZERO id. Id 0 is the
        // never-selectable sentinel (a wire-`w:id="0"` interior revision stays
        // census-only — it is genuinely not individually addressable). Moves are
        // pair-carriers and stay census-only (see `is_by_id_resolvable_name`).
        // NOTE this is a PER-FRAGMENT verdict; whether the id actually SELECTS
        // the carrier is additionally a DOC-GLOBAL uniqueness question — see
        // `classify_interior_ids` (wild ids are not normalized inside opaque
        // raw_xml, so an id shared with a body revision or another interior
        // carrier does not identify anything).
        let resolvable =
            is_by_id_resolvable_name(name) && !has_rev_ancestor && id.is_some_and(|i| i != 0);
        f(FragmentCarrier {
            name,
            id,
            resolvable,
            author: crate::xml_attrs::attr_get(element, "w:author").cloned(),
            date: crate::xml_attrs::attr_get(element, "w:date").cloned(),
        });
        child_has_ancestor = true;
    }
    for child in &element.children {
        if let xmltree::XMLNode::Element(child_el) = child {
            visit_fragment_carriers(child_el, child_has_ancestor, f);
        }
    }
}

/// Doc-global classification of interior (opaque `raw_xml`) revision ids.
///
/// Import normalizes BODY/story revision ids (wire-0 minting, duplicate
/// renumbering — `import::for_each_revision_id_mut`) but deliberately never
/// rewrites opaque `raw_xml`, so interior carriers keep their raw wild wire
/// ids. Wild documents DO carry duplicate `w:id`s (merges, non-Word
/// producers), so an interior id is only an IDENTITY — and only then
/// selectable — when exactly one interior carrier in the whole document bears
/// it and no body/story carrier claims the same id. Anything else is demoted
/// to census-only (revision_id 0): reported honestly, resolvable via
/// accept-all/reject-all, never individually addressable. Without this rule a
/// caller accepting body revision 5 would silently also resolve a textbox
/// interior `w:ins w:id="5"` it never selected.
///
/// This is the ONE place the demotion is computed; `resolvable_revision_ids`
/// (selection membership), `enumerate_revisions` (census records), and the
/// selective projection (which interior ids may descend) all consume it, so
/// they cannot disagree.
pub(crate) struct InteriorIdClassification {
    /// Interior ids that uniquely identify one interior carrier document-wide:
    /// the only interior ids that are individually selectable.
    pub selectable: HashSet<u32>,
    /// Resolvable-shaped interior ids demoted to census-only because the id is
    /// shared (with a body/story revision or another interior carrier).
    pub demoted: HashSet<u32>,
}

pub(crate) fn classify_interior_ids(doc: &CanonDoc) -> InteriorIdClassification {
    let body = body_resolvable_revision_ids(doc);
    // Count EVERY interior carrier id (stacked and *PrChange included): an id
    // shared between a top-level w:ins and a stacked carrier does not uniquely
    // identify either. The walk is the shared census walk
    // (`opaque_targets::visit_opaque_interiors`), so the id population seen
    // here is exactly the population the census reports. Hyperlink interiors
    // are excluded exactly as the census excludes them: their runs are typed
    // and their ids live in the body set already.
    let mut occurrences: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    let mut resolvable_shaped: HashSet<u32> = HashSet::new();
    crate::opaque_targets::visit_opaque_interiors(doc, &mut |_, _, iref| {
        if let crate::opaque_targets::OpaqueInteriorRef::Inline(o) = iref
            && !matches!(o.kind, crate::domain::OpaqueKind::Hyperlink(_))
            && let Some(raw) = &o.raw_xml
            && crate::normalize::has_revision_markup_bytes(raw)
            && let Ok(root) = crate::word_xml::parse_raw_fragment(raw)
        {
            visit_fragment_carriers(&root, false, &mut |c| {
                if let Some(id) = c.id
                    && id != 0
                {
                    *occurrences.entry(id).or_insert(0) += 1;
                    if c.resolvable {
                        resolvable_shaped.insert(id);
                    }
                }
            });
        }
    });
    let mut selectable = HashSet::new();
    let mut demoted = HashSet::new();
    for id in resolvable_shaped {
        if occurrences[&id] == 1 && !body.contains(&id) {
            selectable.insert(id);
        } else {
            demoted.insert(id);
        }
    }
    InteriorIdClassification {
        selectable,
        demoted,
    }
}

/// The maximum `w:id` on ANY revision carrier in a fragment (resolvable or not),
/// so id allocation mints strictly above every interior id and can never collide
/// with a pre-existing one (`max_revision_id` does not otherwise see interiors).
pub(crate) fn max_interior_id(raw_xml: &[u8]) -> u32 {
    if !crate::normalize::has_revision_markup_bytes(raw_xml) {
        return 0;
    }
    let Ok(root) = crate::word_xml::parse_raw_fragment(raw_xml) else {
        return 0;
    };
    let mut max = 0;
    visit_fragment_carriers(&root, false, &mut |c| {
        if let Some(id) = c.id {
            max = max.max(id);
        }
    });
    max
}

/// Census the tracked changes hiding inside one opaque INLINE wrapper's
/// verbatim-preserved `raw_xml` (a textbox's `w:txbxContent`, an embedded
/// object, a drawing, an SDT, …). These revisions are real markup in the
/// serialized document, but the modeled tree the rest of `enumerate_revisions`
/// walks never enters opaque content — so without this scan a document could
/// carry twenty live tracked changes inside a textbox while the census reports
/// zero, a silent lie a "nothing left to resolve" consumer would trust.
///
/// One [`RevisionKind::OpaqueInterior`] record is emitted per carrier element
/// found, surfacing its `w:author`/`w:date` where present. They are reported,
/// never individually resolved: selective resolution cannot reach them (that
/// would need full opaque descent — a future RFC); the point here is only that
/// the INVENTORY stops lying.
///
/// A cheap byte gate ([`normalize::has_revision_markup_bytes`], the same carrier
/// inventory the normalizer uses) skips the parse entirely for the overwhelming
/// majority of opaque wrappers that carry no revision markup at all. If the gate
/// matches but the fragment fails to parse, that is NOT reported as zero — we
/// already know markup is present, so one honest detail-unavailable record is
/// emitted rather than silently dropping it.
fn enumerate_opaque_interior_revisions(
    out: &mut Vec<RevisionRecord>,
    raw_xml: &[u8],
    block_id: &NodeId,
    location: &StoryScope,
    interior_demoted: &HashSet<u32>,
) {
    if !crate::normalize::has_revision_markup_bytes(raw_xml) {
        return;
    }
    let Ok(root) = crate::word_xml::parse_raw_fragment(raw_xml) else {
        out.push(opaque_interior_record(
            0,
            block_id,
            location,
            None,
            None,
            "inside embedded content (textbox/object); carries tracked changes \
             that could not be parsed — not individually resolvable"
                .to_string(),
        ));
        return;
    };
    // The byte gate is an over-approximation (a marker string could sit in an
    // attribute value or comment); the element walk below is authoritative, so
    // a gate-hit with no real carrier element correctly emits nothing.
    walk_opaque_interior_revisions(&root, out, block_id, location, interior_demoted);
}

/// Emit an [`RevisionKind::OpaqueInterior`] record for each tracked-change
/// carrier in a parsed opaque fragment ([`visit_fragment_carriers`] — the shared
/// classifier). A resolvable carrier (top-level `w:ins`/`w:del`/`w:moveFrom`/
/// `w:moveTo` with a numeric `w:id`) carries that real id and is individually
/// selectable; a stacked / `*PrChange` / id-less one carries `0` (census-only).
/// One record per carrier: a `w:del` nested inside a `w:ins` yields two, matching
/// the two revisions present (both id 0 — stacked is not individually resolvable).
fn walk_opaque_interior_revisions(
    element: &xmltree::Element,
    out: &mut Vec<RevisionRecord>,
    block_id: &NodeId,
    location: &StoryScope,
    interior_demoted: &HashSet<u32>,
) {
    visit_fragment_carriers(element, false, &mut |c| {
        // Two demotions to the census-only sentinel: the per-fragment shape
        // verdict (stacked / *PrChange / move-pair / id-less), and the
        // doc-global id-uniqueness verdict (`classify_interior_ids` — an id
        // shared with another revision identifies nothing).
        let shape_resolvable_id = if c.resolvable { c.id.unwrap_or(0) } else { 0 };
        let (revision_id, resolvability) =
            if shape_resolvable_id != 0 && !interior_demoted.contains(&shape_resolvable_id) {
                (shape_resolvable_id, "individually resolvable")
            } else if shape_resolvable_id != 0 {
                (
                    0,
                    "id shared with another revision — not individually \
                     resolvable (accept-all/reject-all still applies)",
                )
            } else {
                (0, "not individually resolvable")
            };
        out.push(opaque_interior_record(
            revision_id,
            block_id,
            location,
            c.author,
            c.date,
            format!(
                "inside embedded content (textbox/object); w:{} — {resolvability}",
                c.name
            ),
        ));
    });
}

/// Census opaque BLOCK interiors. Unlike opaque inlines, an opaque block keeps
/// its bytes in the source package (re-fetched at serialize), NOT in the
/// `CanonDoc` — so there is no `raw_xml` here to scan. The one block kind we can
/// still be honest about is [`OpaqueKind::QuarantinedNestedTracking`]: the KIND
/// itself is proof the block wraps stacked tracked-change markup (`w:del` inside
/// `w:ins`) the IR could not represent. Report its presence so the census does
/// not silently drop it. The exact count and authors are unavailable without the
/// bytes (a future full-descent RFC); an honest "at least one unresolvable
/// revision lives here" beats a silent zero.
fn enumerate_opaque_block_interior(
    out: &mut Vec<RevisionRecord>,
    opaque: &crate::domain::OpaqueBlockNode,
    block_id: &NodeId,
    location: &StoryScope,
) {
    if matches!(opaque.kind, OpaqueKind::QuarantinedNestedTracking) {
        out.push(opaque_interior_record(
            0,
            block_id,
            location,
            None,
            None,
            "inside quarantined nested tracked-change content; not individually \
             resolvable"
                .to_string(),
        ));
    }
}

pub(crate) fn block_node_id(block: &BlockNode) -> NodeId {
    match block {
        BlockNode::Paragraph(p) => p.id.clone(),
        BlockNode::Table(t) => t.id.clone(),
        BlockNode::OpaqueBlock(o) => o.id.clone(),
    }
}

fn block_text_excerpt(block: &BlockNode) -> String {
    extract_block_text_for_hash(block)
}

fn row_text_excerpt(row: &crate::domain::TableRowNode) -> String {
    let mut out = String::new();
    for cell in &row.cells {
        for nested in &cell.blocks {
            out.push_str(&extract_block_text_for_hash(nested));
            out.push(' ');
        }
    }
    out.trim_end().to_string()
}

// ─── Formatting-change resolution helpers ────────────────────────────────────
// Rejecting a tracked formatting change restores the COMPLETE previous state
// the change record snapshotted; accepting just discards the record. These are
// the single restore implementations — the full-resolution path
// (accept_all/reject_all) and the selective-by-id path both call them.

// Each `reject_*_formatting` EXHAUSTIVELY destructures its `*FormattingChange`,
// so every `previous_*` field must be consciously restored or explicitly
// skipped (with a reason). A new "previous" field then fails to compile until
// it is handled — it cannot be silently dropped on reject. This is the
// reversibility contract for tracked formatting changes (§17.13.5.29/.34/.36/.37):
// reject restores the prior state EXACTLY. (Same compiler-enforced discipline as
// the exhaustive roundtrip comparator.)
pub(crate) fn reject_paragraph_formatting(p: &mut ParagraphNode) {
    let Some(fc) = p.formatting_change.take() else {
        return;
    };
    let crate::domain::ParagraphFormattingChange {
        previous_alignment,
        previous_indentation,
        previous_spacing,
        previous_numbering,
        // A serialization hint (numId=0 vs prefix disambiguation), not a separate
        // live field: the live numbering is restored via `previous_numbering`.
        previous_numbering_explicitly_absent: _,
        previous_style_id,
        previous_keep_next,
        previous_keep_lines,
        previous_page_break_before,
        previous_widow_control,
        previous_contextual_spacing,
        previous_shading,
        previous_borders,
        previous_tab_stops,
        previous_literal_prefix_leading_tab_twips,
        previous_literal_prefix_trailing_tab_stop_twips,
        previous_paragraph_mark_marks,
        previous_paragraph_mark_style_props,
        previous_paragraph_mark_rpr_off,
        previous_text_direction,
        previous_text_alignment,
        previous_mirror_indents,
        previous_auto_space_de,
        previous_auto_space_dn,
        previous_bidi,
        previous_suppress_auto_hyphens,
        previous_snap_to_grid,
        previous_overflow_punct,
        previous_adjust_right_ind,
        previous_word_wrap,
        previous_frame_pr,
        previous_preserved_ppr,
        // Revision identity/attribution — not restorable formatting state.
        revision_id: _,
        identity: _,
        author: _,
        date: _,
    } = fc;
    // The snapshot is the previous DIRECT formatting (§17.13.5.29), so a
    // present previous value IS direct authorship: the has_direct_* emission
    // gates must be re-derived from the restored values, or the serializer
    // drops a restored property the current (discarded) state didn't author.
    p.has_direct_align = previous_alignment.is_some();
    p.has_direct_indent = previous_indentation.is_some();
    p.has_direct_spacing = previous_spacing.is_some();
    p.has_direct_keep_next = previous_keep_next.is_some();
    p.has_direct_keep_lines = previous_keep_lines.is_some();
    p.has_direct_page_break_before = previous_page_break_before;
    p.has_direct_widow_control = previous_widow_control.is_some();
    p.has_direct_contextual_spacing = previous_contextual_spacing.is_some();
    p.has_direct_shading = previous_shading.is_some();
    p.has_direct_borders = previous_borders.is_some();
    p.align = previous_alignment;
    // `previous_indentation`/`previous_spacing` are the previous AUTHORED-direct
    // pPr (from the snapshot or the inner-pPr parse), so they restore BOTH the
    // authored field the serializer emits AND the effective projection — for a
    // reject the two coincide (there is no live cascade to re-resolve here, same
    // as import of a direct-only paragraph).
    p.authored_indent = previous_indentation.clone();
    p.authored_spacing = previous_spacing.clone();
    p.indent = previous_indentation;
    p.spacing = previous_spacing;
    // Restore the numbering emission gate to match the base numbering. Like the
    // authored_indent restore above, reject treats the restored value as direct
    // (there is no live cascade to re-resolve here — the pPrChange inner pPr is
    // the previous DIRECT pPr).
    p.has_direct_numbering = previous_numbering.is_some();
    p.numbering = previous_numbering;
    p.style_id = previous_style_id;
    p.keep_next = previous_keep_next;
    p.keep_lines = previous_keep_lines;
    p.page_break_before = previous_page_break_before;
    p.widow_control = previous_widow_control;
    p.contextual_spacing = previous_contextual_spacing;
    p.shading = previous_shading;
    p.borders = previous_borders;
    p.tab_stops = previous_tab_stops;
    p.literal_prefix_leading_tab_twips = previous_literal_prefix_leading_tab_twips;
    p.literal_prefix_trailing_tab_stop_twips = previous_literal_prefix_trailing_tab_stop_twips;
    p.paragraph_mark_marks = previous_paragraph_mark_marks;
    p.paragraph_mark_style_props = previous_paragraph_mark_style_props;
    p.paragraph_mark_rpr_off = previous_paragraph_mark_rpr_off;
    p.text_direction = previous_text_direction;
    p.text_alignment = previous_text_alignment;
    p.mirror_indents = previous_mirror_indents;
    p.auto_space_de = previous_auto_space_de;
    p.auto_space_dn = previous_auto_space_dn;
    p.bidi = previous_bidi;
    p.suppress_auto_hyphens = previous_suppress_auto_hyphens;
    p.snap_to_grid = previous_snap_to_grid;
    p.overflow_punct = previous_overflow_punct;
    p.adjust_right_ind = previous_adjust_right_ind;
    p.word_wrap = previous_word_wrap;
    p.frame_pr = previous_frame_pr;
    // Replace, not merge: the pPrChange snapshot IS the complete previous pPr
    // (§17.13.5.29), so its preserved remainder is the paragraph's ENTIRE
    // unmodeled remainder after reject — not the union with whatever the
    // about-to-be-discarded current state happened to carry.
    p.preserved_ppr = previous_preserved_ppr;
}

/// Re-resolve every run in `p` against the paragraph's CURRENT `style_id`.
///
/// A run's `style_props` bake in the marks it inherits from the paragraph style
/// (caps, bold, fonts, …) at import time. When accept/reject reverts or applies
/// a tracked paragraph-style change (`w:pPrChange`, §17.13.5.29) the paragraph's
/// `style_id` changes underneath its runs, leaving those style-inherited marks
/// stale — e.g. rejecting a change to a caps-bearing style would still render
/// the text uppercase, and rejecting a change AWAY from a caps style would fail
/// to restore the uppercasing. Re-running the cascade fixes both directions.
///
/// Only reachable when the caller has the document's `StyleDefinitions` (the
/// runtime projection paths); a bare `CanonDoc` carries no style table, and a
/// document with no styles part has no style-inherited marks to re-resolve.
fn reresolve_paragraph_style_inherited_marks(
    p: &mut ParagraphNode,
    style_defs: &crate::styles::StyleDefinitions,
) {
    // Clone the style id up front: the run loop below borrows `p` mutably.
    // Mirror import (ISO 29500-1 §17.7.4.17): an unstyled paragraph implicitly
    // references the default paragraph style, so runs must resolve against it —
    // otherwise re-resolution would drop the default style's rPr contribution
    // and diverge from a fresh baseline import.
    let para_style_id = p
        .style_id
        .as_deref()
        .or_else(|| style_defs.default_para_style_id())
        .map(|s| s.to_string());
    let para_style_id = para_style_id.as_deref();
    // A run whose direct/highlight/style tokens re-resolve to something the
    // converter rejects would mean our own serializer or the document's
    // styles.xml produced an invalid token — impossible for an already-imported
    // document (import runs the identical conversion and would have failed
    // there). Surface it loudly rather than silently keeping the stale marks.
    let expect_ctx = "re-resolving run style props against a reverted/applied paragraph style: \
         the document already imported, so its style and run tokens are valid";
    for segment in &mut p.segments {
        for inline in &mut segment.inlines {
            if let InlineNode::Text(t) = inline {
                crate::import::reresolve_run_style_props(
                    &mut t.marks,
                    &mut t.style_props,
                    t.rpr_authored,
                    style_defs,
                    para_style_id,
                )
                .expect(expect_ctx);
            }
        }
    }
    // The stripped numbering/label prefix is re-emitted as its own run and so
    // inherits from the paragraph style exactly like a body run.
    if p.literal_prefix.is_some() {
        crate::import::reresolve_run_style_props(
            &mut p.literal_prefix_marks,
            &mut p.literal_prefix_style_props,
            p.literal_prefix_rpr_authored,
            style_defs,
            para_style_id,
        )
        .expect(expect_ctx);
    }
}

/// Reject a run-level `w:rPrChange`: restores marks, style_props, AND
/// `rpr_authored` — all three, not just the first two. The serializer emits
/// `<w:rPr>` children by consulting `rpr_authored` (which properties were
/// AUTHORED directly on this run), not by re-deriving it from `marks`/
/// `style_props` — restoring only the latter two left `rpr_authored` stuck
/// at its post-edit value, so the serializer kept re-emitting the (reverted,
/// should-be-gone) formatting into the saved file even though the typed
/// model was already correct in memory. Across the whole formatting-
/// change-carrier class, table/row/cell formatting have no such
/// separate provenance bitset, so they were never exposed to this failure
/// mode — this is specific to runs.
pub(crate) fn reject_text_formatting(t: &mut crate::domain::TextNode) {
    let Some(fc) = t.formatting_change.take() else {
        return;
    };
    let crate::domain::FormattingChange {
        previous_marks,
        previous_style_props,
        previous_rpr_authored,
        revision_id: _,
        identity: _,
        author: _,
        date: _,
    } = fc;
    t.marks = previous_marks;
    t.style_props = previous_style_props;
    t.rpr_authored = previous_rpr_authored;
}

pub(crate) fn reject_table_formatting(t: &mut crate::domain::TableNode) {
    let Some(fc) = t.formatting_change.take() else {
        return;
    };
    let crate::domain::TableFormattingChange {
        previous_width,
        previous_borders,
        previous_default_cell_margins,
        revision_id: _,
        identity: _,
        author: _,
        date: _,
    } = fc;
    t.formatting.width = previous_width;
    t.formatting.borders = previous_borders;
    t.formatting.default_cell_margins = previous_default_cell_margins;
}

pub(crate) fn reject_row_formatting(row: &mut crate::domain::TableRowNode) {
    let Some(fc) = row.formatting_change.take() else {
        return;
    };
    let crate::domain::RowFormattingChange {
        previous_height,
        previous_height_rule,
        revision_id: _,
        identity: _,
        author: _,
        date: _,
    } = fc;
    row.height = previous_height;
    row.height_rule = previous_height_rule;
}

pub(crate) fn reject_cell_formatting(cell: &mut crate::domain::TableCellNode) {
    let Some(fc) = cell.formatting_change.take() else {
        return;
    };
    let crate::domain::CellFormattingChange {
        previous_width,
        previous_borders,
        previous_shading,
        previous_v_align,
        previous_margins,
        previous_no_wrap,
        previous_text_direction,
        previous_tc_fit_text,
        revision_id: _,
        identity: _,
        author: _,
        date: _,
    } = fc;
    cell.formatting.width = previous_width;
    cell.formatting.borders = previous_borders;
    cell.formatting.shading = previous_shading;
    cell.formatting.v_align = previous_v_align;
    cell.formatting.margins = previous_margins;
    cell.formatting.no_wrap = previous_no_wrap;
    cell.formatting.text_direction = previous_text_direction;
    cell.formatting.tc_fit_text = previous_tc_fit_text;
}

fn project_block_for_selected_resolution(
    block: &mut BlockNode,
    action: ResolveSelectionAction,
    selected_revision_ids: &HashSet<u32>,
    interior_selected: &HashSet<u32>,
    style_defs: Option<&crate::styles::StyleDefinitions>,
) {
    match block {
        BlockNode::Paragraph(p) => {
            // Snapshot the style so a selectively-rejected pPrChange that swaps
            // it triggers run re-resolution below, exactly like the full path.
            let style_before = p.style_id.clone();
            p.segments.retain(|segment| {
                selected_tracking_outcome(&segment.status, action, selected_revision_ids)
                    != SelectedTrackingOutcome::Drop
            });
            for segment in &mut p.segments {
                match selected_tracking_outcome(&segment.status, action, selected_revision_ids) {
                    SelectedTrackingOutcome::KeepNormal => {
                        segment.status = TrackingStatus::Normal;
                    }
                    SelectedTrackingOutcome::Transition(new_status) => {
                        segment.status = new_status;
                    }
                    SelectedTrackingOutcome::KeepTracked | SelectedTrackingOutcome::Drop => {}
                }
            }

            match selected_optional_tracking_outcome(
                &p.para_mark_status,
                action,
                selected_revision_ids,
            ) {
                SelectedTrackingOutcome::KeepNormal => {
                    p.para_mark_status = None;
                }
                SelectedTrackingOutcome::Transition(new_status) => {
                    p.para_mark_status = Some(new_status);
                }
                // Drop = the mark's revision is resolved, so the paragraph break
                // goes away. The structural merge already ran in
                // `merge_marked_paragraphs_tracked_selected`; if it was BLOCKED
                // (a table/section follows, or this is the last block) the
                // paragraph SURVIVES, so the now-resolved status must be cleared
                // rather than left pending — otherwise `paragraph_has_unresolved_
                // tracking` keeps `normalize_paragraph_after_projection` from
                // running and the serializer re-emits the mark under a FRESH id
                // (a revision that "was never in the enumeration"). Mirrors the
                // full path's unconditional clear in
                // `normalize_paragraph_after_projection`.
                SelectedTrackingOutcome::Drop => {
                    p.para_mark_status = None;
                }
                SelectedTrackingOutcome::KeepTracked => {}
            }

            // Tracked formatting changes resolve by id too: the paragraph's
            // pPrChange and each text node's rPrChange.
            if let Some(fc) = &p.formatting_change
                && fc.revision_id != 0
                && selected_revision_ids.contains(&fc.identity)
            {
                match action {
                    ResolveSelectionAction::Accept => p.formatting_change = None,
                    ResolveSelectionAction::Reject => reject_paragraph_formatting(p),
                }
            }
            // A mid-document section break's own w:sectPrChange
            // (§17.13.5.32) — the paragraph-level sibling of
            // `doc.body_section_property_change` (see
            // `project_body_section_for_accept_reject`, which resolves the
            // BODY one for AcceptAll/RejectAll). Accept keeps the LIVE
            // section_properties (already the new state — `apply_set_page_
            // setup` mutates it at authoring time) and drops the record;
            // reject restores the snapshot the record carries.
            if let Some(change) = &p.section_property_change
                && change.revision.revision_id != 0
                && selected_revision_ids.contains(&change.revision.identity)
            {
                match action {
                    ResolveSelectionAction::Accept => p.section_property_change = None,
                    ResolveSelectionAction::Reject => {
                        if let Some(change) = p.section_property_change.take()
                            && let Some(prev) =
                                parse_previous_section_properties(&change.previous_properties_raw)
                        {
                            p.section_properties = Some(prev);
                        }
                        // If the raw snapshot fails to parse, leave the current
                        // properties and the (now-taken) record dropped rather
                        // than fabricate a default — same defensive stance as
                        // `project_body_section_for_accept_reject`'s reject arm
                        // (a sectPrChange the engine authored always round-trips;
                        // this is a defensive branch, not an expected path).
                    }
                }
            }
            // Revisions carried by a hyperlink's per-run status (§17.13.5.15/.20
            // over `HyperlinkData.runs[*].status` — the layer `ReplaceHyperlinkText`
            // writes to, see the type-level docs on `HyperlinkData`) resolve by id
            // too, mirroring the full-projection handling in
            // `project_hyperlink_runs` so the two paths cannot diverge.
            for segment in &mut p.segments {
                for inline in &mut segment.inlines {
                    match inline {
                        InlineNode::Text(t) => {
                            if let Some(fc) = &t.formatting_change
                                && fc.revision_id != 0
                                && selected_revision_ids.contains(&fc.identity)
                            {
                                match action {
                                    ResolveSelectionAction::Accept => t.formatting_change = None,
                                    ResolveSelectionAction::Reject => reject_text_formatting(t),
                                }
                            }
                        }
                        InlineNode::OpaqueInline(opaque) => {
                            if let crate::domain::OpaqueKind::Hyperlink(data) = &mut opaque.kind
                                && data.runs.iter().any(|run| {
                                    status_carries_selected_id(&run.status, selected_revision_ids)
                                })
                            {
                                project_hyperlink_runs_for_selected_resolution(
                                    data,
                                    action,
                                    selected_revision_ids,
                                );
                                // Same cache-invalidation rule as the full path
                                // (`project_block_inner`): the opaque's raw_xml
                                // cache is stale once `runs` changed under it.
                                opaque.raw_xml = None;
                                opaque.content_hash = None;
                            } else if !interior_selected.is_empty()
                                && opaque
                                    .raw_xml
                                    .as_deref()
                                    .is_some_and(crate::normalize::has_revision_markup_bytes)
                            {
                                // Descend by id into the opaque fragment and resolve
                                // just the SELECTED interior revisions (RFC-0002
                                // §Phase-3b); every other carrier stays verbatim.
                                // `interior_selected` — NOT the full selected set —
                                // holds only ids `classify_interior_ids` proved
                                // uniquely identify one interior carrier, so a body
                                // id can never resolve a same-numbered interior
                                // carrier here. A non-matching fragment resolves to
                                // Clean (a selected id living in a different
                                // opaque), so this is a no-op there. Rewrite
                                // raw_xml + content_hash exactly as the full
                                // accept/reject descent does.
                                let accept = matches!(action, ResolveSelectionAction::Accept);
                                match crate::normalize::resolve_fragment_selected(
                                    opaque.raw_xml.as_deref().unwrap(),
                                    interior_selected,
                                    accept,
                                ) {
                                    crate::normalize::FragmentResolution::Resolved(bytes) => {
                                        opaque.content_hash =
                                            Some(crate::import::sha256_hex(&bytes));
                                        opaque.raw_xml = Some(bytes);
                                    }
                                    // The selected ids live in a different opaque.
                                    crate::normalize::FragmentResolution::Clean => {}
                                    // Unreachable by construction: an unparseable
                                    // fragment contributes no ids to
                                    // `classify_interior_ids`, so no selected id can
                                    // name one — a hit here is a programmer bug in
                                    // that agreement, not a document condition.
                                    crate::normalize::FragmentResolution::UnparseableWithRevisions => {
                                        debug_assert!(
                                            false,
                                            "selected interior id reached an unparseable fragment"
                                        );
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }

            // A selectively-rejected paragraph-style change swaps `style_id`
            // under runs resolved against the old style — re-run the cascade so
            // their style-inherited marks match, mirroring the full path.
            if let Some(style_defs) = style_defs
                && p.style_id != style_before
            {
                reresolve_paragraph_style_inherited_marks(p, style_defs);
            }

            if !paragraph_has_unresolved_tracking(p) && p.formatting_change.is_none() {
                normalize_paragraph_after_projection(p, /*preserve_formatting_change=*/ false);
            }
        }
        BlockNode::Table(t) => {
            if let Some(fc) = &t.formatting_change
                && fc.revision_id != 0
                && selected_revision_ids.contains(&fc.identity)
            {
                match action {
                    ResolveSelectionAction::Accept => t.formatting_change = None,
                    ResolveSelectionAction::Reject => reject_table_formatting(t),
                }
            }
            t.rows.retain(|row| {
                selected_optional_tracking_outcome(
                    &row.tracking_status,
                    action,
                    selected_revision_ids,
                ) != SelectedTrackingOutcome::Drop
            });
            for row in &mut t.rows {
                match selected_optional_tracking_outcome(
                    &row.tracking_status,
                    action,
                    selected_revision_ids,
                ) {
                    SelectedTrackingOutcome::KeepNormal => {
                        row.tracking_status = None;
                    }
                    SelectedTrackingOutcome::Transition(new_status) => {
                        row.tracking_status = Some(new_status);
                    }
                    SelectedTrackingOutcome::KeepTracked | SelectedTrackingOutcome::Drop => {}
                }

                if let Some(fc) = &row.formatting_change
                    && fc.revision_id != 0
                    && selected_revision_ids.contains(&fc.identity)
                {
                    match action {
                        ResolveSelectionAction::Accept => row.formatting_change = None,
                        ResolveSelectionAction::Reject => reject_row_formatting(row),
                    }
                }

                row.cells.retain(|cell| {
                    selected_optional_tracking_outcome(
                        &cell.tracking_status,
                        action,
                        selected_revision_ids,
                    ) != SelectedTrackingOutcome::Drop
                });
                for cell in &mut row.cells {
                    match selected_optional_tracking_outcome(
                        &cell.tracking_status,
                        action,
                        selected_revision_ids,
                    ) {
                        SelectedTrackingOutcome::KeepNormal => {
                            cell.tracking_status = None;
                        }
                        SelectedTrackingOutcome::Transition(new_status) => {
                            cell.tracking_status = Some(new_status);
                        }
                        SelectedTrackingOutcome::KeepTracked | SelectedTrackingOutcome::Drop => {}
                    }

                    if let Some(fc) = &cell.formatting_change
                        && fc.revision_id != 0
                        && selected_revision_ids.contains(&fc.identity)
                    {
                        match action {
                            ResolveSelectionAction::Accept => cell.formatting_change = None,
                            ResolveSelectionAction::Reject => reject_cell_formatting(cell),
                        }
                    }

                    merge_marked_paragraphs_bare_selected(
                        &mut cell.blocks,
                        action,
                        selected_revision_ids,
                    );
                    cell.blocks.retain(|nested| match nested {
                        BlockNode::Paragraph(p) => {
                            selected_optional_tracking_outcome(
                                &p.para_mark_status,
                                action,
                                selected_revision_ids,
                            ) != SelectedTrackingOutcome::Drop
                        }
                        _ => true,
                    });
                    // Drop a nested table this selection emptied of every row,
                    // matching the full accept/reject path and the body-level
                    // rowless-table drop.
                    let nested_had_rows: Vec<bool> = cell
                        .blocks
                        .iter()
                        .map(|b| matches!(b, BlockNode::Table(t) if !t.rows.is_empty()))
                        .collect();
                    for nested in &mut cell.blocks {
                        project_block_for_selected_resolution(
                            nested,
                            action,
                            selected_revision_ids,
                            interior_selected,
                            style_defs,
                        );
                    }
                    drop_emptied_nested_tables(&mut cell.blocks, &nested_had_rows);
                }
            }
        }
        BlockNode::OpaqueBlock(_) => {}
    }
}

fn project_blocks_for_selected_resolution(
    blocks: &mut Vec<TrackedBlock>,
    action: ResolveSelectionAction,
    selected_revision_ids: &HashSet<u32>,
    interior_selected: &HashSet<u32>,
    style_defs: Option<&crate::styles::StyleDefinitions>,
) {
    let resolved_move_names = selected_move_names(blocks, selected_revision_ids);
    let move_plans = selected_move_plans(blocks, selected_revision_ids);
    let inserted_move_origins = inserted_move_origin_plans(blocks);
    let move_destination_deletions = move_destination_deletion_plans(blocks);
    neutralize_selected_move_paragraph_marks(blocks, &move_plans, selected_revision_ids);
    apply_move_destination_deletion_cascade(
        blocks,
        action,
        selected_revision_ids,
        &move_destination_deletions,
    );
    apply_inserted_move_origin_cascade(
        blocks,
        action,
        selected_revision_ids,
        &inserted_move_origins,
    );
    // Snapshot inline range-pair markers BEFORE resolution so a pair this
    // selection tears (one half dropped with a resolved revision, the other
    // surviving) can be collapsed back to a point — identical to the full
    // accept/reject path (see `collapse_resolution_torn_range_markers`).
    let range_pair_inventory = capture_range_pair_inventory(blocks);

    merge_marked_paragraphs_tracked_selected(blocks, action, selected_revision_ids);
    blocks.retain(|tb| tracked_block_survives_selected(&tb.status, action, selected_revision_ids));
    // Same Word-parity rule as project_blocks_for_accept_reject: only a table
    // whose rows were all dropped BY this resolution is removed; a table that
    // was already rowless (valid per CT_Tbl, row group minOccurs="0") stays.
    let had_rows: Vec<bool> = blocks
        .iter()
        .map(|tb| match &tb.block {
            BlockNode::Table(t) => !t.rows.is_empty(),
            _ => false,
        })
        .collect();
    for tb in blocks.iter_mut() {
        if selected_tracking_outcome(&tb.status, action, selected_revision_ids)
            == SelectedTrackingOutcome::KeepNormal
        {
            tb.status = TrackingStatus::Normal;
        }
        project_block_for_selected_resolution(
            &mut tb.block,
            action,
            selected_revision_ids,
            interior_selected,
            style_defs,
        );
    }
    let mut idx = 0;
    blocks.retain(|tb| {
        let keep = match &tb.block {
            BlockNode::Table(t) => !t.rows.is_empty() || !had_rows[idx],
            _ => true,
        };
        idx += 1;
        keep
    });

    collapse_resolution_torn_range_markers(blocks, &range_pair_inventory);
    resolve_selected_move_layout(blocks, action, &move_plans);
    clear_resolved_move_markup(blocks, &resolved_move_names);
}

#[derive(Clone)]
struct SelectedMovePlan {
    source_id: NodeId,
    destination_id: NodeId,
}

/// Word represents "insert paragraph, then move it" asymmetrically: the
/// originating insertion is nested in the moveFrom SOURCE, while the moveTo
/// clone carries only the move. The linked pair is nevertheless one logical
/// piece of content. This plan makes that provenance transition explicit:
/// rejecting the origin removes both clones; accepting the move transfers the
/// still-pending origin to the surviving destination.
#[derive(Clone)]
struct InsertedMoveOriginPlan {
    source_id: NodeId,
    move_revision: RevisionInfo,
    origin: RevisionInfo,
}

#[derive(Clone)]
struct MoveDestinationDeletionPlan {
    source_id: NodeId,
    destination_id: NodeId,
    move_revision: RevisionInfo,
    deletions: Vec<(RevisionInfo, String)>,
}

/// A deletion nested in moveTo strikes the destination CLONE, not the untouched
/// source copy. If the deletion is rejected, that restored text is still moveTo
/// content; if the move is rejected, the destination-only deletion cascades
/// away with the clone. Represent both transitions before the ordinary status
/// projector sees the inner deletion in isolation.
fn move_destination_deletion_plans(blocks: &[TrackedBlock]) -> Vec<MoveDestinationDeletionPlan> {
    let mut plans = Vec::new();
    for tracked in blocks {
        if tracked.move_id.is_none() {
            continue;
        }
        let BlockNode::Paragraph(paragraph) = &tracked.block else {
            continue;
        };
        let Some(TrackingStatus::Inserted(move_revision)) = &paragraph.para_mark_status else {
            continue;
        };
        let deletions: Vec<(RevisionInfo, String)> = paragraph
            .segments
            .iter()
            .filter_map(|segment| match &segment.status {
                TrackingStatus::Deleted(revision)
                    if revision.identity != move_revision.identity =>
                {
                    Some((revision.clone(), extract_inlines_text(&segment.inlines)))
                }
                _ => None,
            })
            .collect();
        let source_id = blocks.iter().find_map(|candidate| {
            if candidate.move_id != tracked.move_id {
                return None;
            }
            let BlockNode::Paragraph(source) = &candidate.block else {
                return None;
            };
            matches!(&source.para_mark_status,
                Some(TrackingStatus::Deleted(revision))
                    if revision.identity == move_revision.identity)
            .then(|| block_id(&candidate.block).clone())
        });
        if !deletions.is_empty()
            && let Some(source_id) = source_id
        {
            plans.push(MoveDestinationDeletionPlan {
                source_id,
                destination_id: block_id(&tracked.block).clone(),
                move_revision: move_revision.clone(),
                deletions,
            });
        }
    }
    plans
}

fn apply_move_destination_deletion_cascade(
    blocks: &mut [TrackedBlock],
    action: ResolveSelectionAction,
    selected_revision_ids: &HashSet<u32>,
    plans: &[MoveDestinationDeletionPlan],
) {
    for plan in plans {
        let deletion_identities: HashSet<u32> = plan
            .deletions
            .iter()
            .map(|(revision, _)| revision.identity)
            .collect();
        let rejecting_move = action == ResolveSelectionAction::Reject
            && selected_revision_ids.contains(&plan.move_revision.identity);
        let rejecting_inner_deletion = action == ResolveSelectionAction::Reject
            && !selected_revision_ids.is_disjoint(&deletion_identities);
        let accepting_inner_deletion = action == ResolveSelectionAction::Accept
            && !selected_revision_ids.is_disjoint(&deletion_identities);
        if !rejecting_move && !rejecting_inner_deletion && !accepting_inner_deletion {
            continue;
        }
        if rejecting_move || accepting_inner_deletion {
            let Some(source) = blocks
                .iter_mut()
                .find(|block| block_id(&block.block) == &plan.source_id)
            else {
                continue;
            };
            let BlockNode::Paragraph(source_paragraph) = &mut source.block else {
                continue;
            };
            for (revision, text) in &plan.deletions {
                if accepting_inner_deletion && !selected_revision_ids.contains(&revision.identity) {
                    continue;
                }
                assert!(
                    overlay_unique_text_status(
                        source_paragraph,
                        text,
                        TrackingStatus::Deleted(revision.clone()),
                    ),
                    "move-destination deletion must map uniquely onto its source clone"
                );
            }
        }

        // Accepting the inner deletion is now reflected on BOTH logical
        // clones; the ordinary projector removes its carriers from each. Only
        // reject transitions need to reclassify destination text as moveTo.
        if accepting_inner_deletion && !rejecting_move {
            continue;
        }

        let Some(destination) = blocks
            .iter_mut()
            .find(|block| block_id(&block.block) == &plan.destination_id)
        else {
            continue;
        };
        let BlockNode::Paragraph(paragraph) = &mut destination.block else {
            continue;
        };
        for segment in &mut paragraph.segments {
            if matches!(&segment.status,
                TrackingStatus::Deleted(revision)
                    if deletion_identities.contains(&revision.identity))
            {
                segment.status = TrackingStatus::Inserted(plan.move_revision.clone());
            }
        }
    }
}

/// Apply `status` to the one occurrence of `needle` in a paragraph's text,
/// preserving run formatting and every non-text inline. Returns false when the
/// move clone does not provide an unambiguous content-identity mapping.
fn overlay_unique_text_status(
    paragraph: &mut ParagraphNode,
    needle: &str,
    status: TrackingStatus,
) -> bool {
    if needle.is_empty() {
        return false;
    }
    let haystack = extract_inlines_text(&paragraph.all_inlines_owned());
    let matches: Vec<usize> = haystack
        .match_indices(needle)
        .map(|(offset, _)| offset)
        .collect();
    if matches.len() != 1 {
        return false;
    }
    let start = haystack[..matches[0]].chars().count();
    let end = start + needle.chars().count();
    let mut cursor = 0usize;
    let mut rebuilt: Vec<TrackedSegment> = Vec::new();

    let push =
        |rebuilt: &mut Vec<TrackedSegment>, inline: InlineNode, inline_status: TrackingStatus| {
            if let Some(last) = rebuilt.last_mut()
                && last.status == inline_status
            {
                last.inlines.push(inline);
            } else {
                rebuilt.push(TrackedSegment {
                    status: inline_status,
                    inlines: vec![inline],
                });
            }
        };

    for segment in std::mem::take(&mut paragraph.segments) {
        for inline in segment.inlines {
            let InlineNode::Text(text) = inline else {
                push(&mut rebuilt, inline, segment.status.clone());
                continue;
            };
            let len = text.text.chars().count();
            let overlap_start = start.saturating_sub(cursor).min(len);
            let overlap_end = end.saturating_sub(cursor).min(len);
            if overlap_start >= overlap_end {
                cursor += len;
                push(&mut rebuilt, InlineNode::Text(text), segment.status.clone());
                continue;
            }
            let chars: Vec<char> = text.text.chars().collect();
            for (part_index, (part, part_status)) in [
                (&chars[..overlap_start], segment.status.clone()),
                (&chars[overlap_start..overlap_end], status.clone()),
                (&chars[overlap_end..], segment.status.clone()),
            ]
            .into_iter()
            .enumerate()
            {
                if part.is_empty() {
                    continue;
                }
                let mut split = text.clone();
                split.id = NodeId::from(format!("{}_mvsplit_{part_index}", text.id.0));
                split.text = part.iter().collect();
                push(&mut rebuilt, InlineNode::Text(split), part_status);
            }
            cursor += len;
        }
    }
    paragraph.segments = rebuilt;
    true
}

fn inserted_move_origin_plans(blocks: &[TrackedBlock]) -> Vec<InsertedMoveOriginPlan> {
    let mut plans = Vec::new();
    for source in blocks {
        let Some(move_name) = source.move_id.as_deref() else {
            continue;
        };
        let BlockNode::Paragraph(source_paragraph) = &source.block else {
            continue;
        };
        let Some(TrackingStatus::InsertedThenDeleted(stacked)) = &source_paragraph.para_mark_status
        else {
            continue;
        };
        // The deletion layer is the moveFrom pilcrow; the insertion layer is
        // the paragraph's origin. Require material source content to carry the
        // same origin so a coincidental stacked pilcrow cannot activate this
        // transition.
        let source_text = extract_inlines_text(&source_paragraph.all_inlines_owned());
        if source_text.is_empty()
            || !source_paragraph.segments.iter().any(|segment| {
                matches!(&segment.status,
                    TrackingStatus::Inserted(revision)
                        if revision.identity == stacked.inserted.identity)
                    && !extract_inlines_text(&segment.inlines).is_empty()
            })
        {
            continue;
        }
        let destination = blocks.iter().find(|candidate| {
            if candidate.move_id.as_deref() != Some(move_name) {
                return false;
            }
            let BlockNode::Paragraph(paragraph) = &candidate.block else {
                return false;
            };
            let destination_is_move = matches!(
                &paragraph.para_mark_status,
                Some(TrackingStatus::Inserted(revision))
                    if revision.identity == stacked.deleted.identity
            );
            destination_is_move
                && extract_inlines_text(&paragraph.all_inlines_owned()) == source_text
        });
        if destination.is_some() {
            plans.push(InsertedMoveOriginPlan {
                source_id: block_id(&source.block).clone(),
                move_revision: stacked.deleted.clone(),
                origin: stacked.inserted.clone(),
            });
        }
    }
    plans
}

fn apply_inserted_move_origin_cascade(
    blocks: &mut [TrackedBlock],
    action: ResolveSelectionAction,
    selected_revision_ids: &HashSet<u32>,
    plans: &[InsertedMoveOriginPlan],
) {
    for plan in plans {
        let origin_selected = selected_revision_ids.contains(&plan.origin.identity);
        let accepting_move = action == ResolveSelectionAction::Accept
            && selected_revision_ids.contains(&plan.move_revision.identity);
        if origin_selected || accepting_move {
            // Once the inserted paragraph is a pending move, resolving its
            // origin in either direction settles that origin INTO the move;
            // it does not decide whether the logical paragraph exists. Word's
            // later move decision alone chooses source vs destination.
            // Accepting the move also settles the source-only origin instead
            // of transferring a pending insertion marker to the clone.
            if let Some(source) = blocks
                .iter_mut()
                .find(|block| block_id(&block.block) == &plan.source_id)
                && let BlockNode::Paragraph(paragraph) = &mut source.block
            {
                for segment in &mut paragraph.segments {
                    if matches!(&segment.status,
                        TrackingStatus::Inserted(revision)
                            if revision.identity == plan.origin.identity)
                    {
                        segment.status = TrackingStatus::Deleted(plan.move_revision.clone());
                    }
                }
                paragraph.para_mark_status =
                    Some(TrackingStatus::Deleted(plan.move_revision.clone()));
            }
        }
    }
}

/// Word's one-shot Reject-All restores the paragraph at its source and settles
/// the nested origin with the rejected move.
fn settle_inserted_move_origins_for_reject_all(
    blocks: &mut [TrackedBlock],
    plans: &[InsertedMoveOriginPlan],
) {
    for plan in plans {
        let Some(source) = blocks
            .iter_mut()
            .find(|block| block_id(&block.block) == &plan.source_id)
        else {
            continue;
        };
        let BlockNode::Paragraph(paragraph) = &mut source.block else {
            continue;
        };
        for segment in &mut paragraph.segments {
            if matches!(&segment.status,
                TrackingStatus::Inserted(revision)
                    if revision.identity == plan.origin.identity)
            {
                segment.status = TrackingStatus::Deleted(plan.move_revision.clone());
            }
        }
        paragraph.para_mark_status = Some(TrackingStatus::Deleted(plan.move_revision.clone()));
    }
}

fn neutralize_selected_move_paragraph_marks(
    blocks: &mut [TrackedBlock],
    plans: &[SelectedMovePlan],
    selected_revision_ids: &HashSet<u32>,
) {
    for plan in plans {
        for block in blocks.iter_mut().filter(|block| {
            let id = block_id(&block.block);
            id == &plan.source_id || id == &plan.destination_id
        }) {
            let BlockNode::Paragraph(paragraph) = &mut block.block else {
                continue;
            };
            if paragraph
                .para_mark_status
                .as_ref()
                .is_some_and(|status| status_carries_selected_id(status, selected_revision_ids))
            {
                paragraph.para_mark_status = None;
            }
        }
    }
}

fn selected_move_plans(
    blocks: &[TrackedBlock],
    selected_revision_ids: &HashSet<u32>,
) -> Vec<SelectedMovePlan> {
    fn status_has(status: &TrackingStatus, identity: u32, inserted: bool) -> bool {
        match status {
            TrackingStatus::Normal => false,
            TrackingStatus::Inserted(revision) => inserted && revision.identity == identity,
            TrackingStatus::Deleted(revision) => !inserted && revision.identity == identity,
            TrackingStatus::InsertedThenDeleted(stacked) => {
                if inserted {
                    stacked.inserted.identity == identity
                } else {
                    stacked.deleted.identity == identity
                }
            }
        }
    }
    fn block_has(block: &TrackedBlock, identity: u32, inserted: bool) -> bool {
        if status_has(&block.status, identity, inserted) {
            return true;
        }
        let BlockNode::Paragraph(paragraph) = &block.block else {
            return false;
        };
        paragraph
            .segments
            .iter()
            .any(|segment| status_has(&segment.status, identity, inserted))
            || paragraph
                .para_mark_status
                .as_ref()
                .is_some_and(|status| status_has(status, identity, inserted))
    }

    let mut plans = Vec::new();
    let names = selected_move_names(blocks, selected_revision_ids);
    for name in names {
        let identities: HashSet<u32> = blocks
            .iter()
            .filter(|block| block.move_id.as_deref() == Some(name.as_str()))
            .flat_map(|block| {
                let mut ids = Vec::new();
                let mut collect = |status: &TrackingStatus| match status {
                    TrackingStatus::Normal => {}
                    TrackingStatus::Inserted(revision) | TrackingStatus::Deleted(revision) => {
                        ids.push(revision.identity)
                    }
                    TrackingStatus::InsertedThenDeleted(stacked) => {
                        ids.push(stacked.inserted.identity);
                        ids.push(stacked.deleted.identity);
                    }
                };
                collect(&block.status);
                if let BlockNode::Paragraph(paragraph) = &block.block {
                    for segment in &paragraph.segments {
                        collect(&segment.status);
                    }
                    if let Some(status) = &paragraph.para_mark_status {
                        collect(status);
                    }
                }
                ids
            })
            .filter(|identity| selected_revision_ids.contains(identity))
            .collect();
        for identity in identities {
            let source = blocks
                .iter()
                .find(|block| {
                    block.move_id.as_deref() == Some(name.as_str())
                        && block_has(block, identity, false)
                })
                .map(|block| block_id(&block.block));
            let destination = blocks
                .iter()
                .find(|block| {
                    block.move_id.as_deref() == Some(name.as_str())
                        && block_has(block, identity, true)
                })
                .map(|block| block_id(&block.block));
            if let (Some(source_id), Some(destination_id)) = (source, destination) {
                plans.push(SelectedMovePlan {
                    source_id: source_id.clone(),
                    destination_id: destination_id.clone(),
                });
                break;
            }
        }
    }
    plans
}

fn resolve_selected_move_layout(
    blocks: &mut Vec<TrackedBlock>,
    action: ResolveSelectionAction,
    plans: &[SelectedMovePlan],
) {
    fn remove_move_decorations(paragraph: &mut ParagraphNode) {
        for segment in &mut paragraph.segments {
            segment.inlines.retain(|inline| {
                !matches!(
                    inline,
                    InlineNode::Decoration(decoration)
                        if decoration.kind == crate::domain::DecorationType::MoveRange
                )
            });
        }
        paragraph
            .segments
            .retain(|segment| !segment.inlines.is_empty());
    }
    fn paragraph_has_content(paragraph: &ParagraphNode) -> bool {
        paragraph
            .segments
            .iter()
            .any(|segment| !segment.inlines.is_empty())
    }

    for plan in plans {
        let source_index = blocks
            .iter()
            .position(|block| block_id(&block.block) == &plan.source_id);
        let destination_index = blocks
            .iter()
            .position(|block| block_id(&block.block) == &plan.destination_id);
        let (Some(source_index), Some(destination_index)) = (source_index, destination_index)
        else {
            continue;
        };

        match action {
            ResolveSelectionAction::Reject => {
                let destination_segments = match &mut blocks[destination_index].block {
                    BlockNode::Paragraph(paragraph) => {
                        remove_move_decorations(paragraph);
                        std::mem::take(&mut paragraph.segments)
                    }
                    _ => Vec::new(),
                };
                if let BlockNode::Paragraph(source) = &mut blocks[source_index].block {
                    remove_move_decorations(source);
                    source.segments.extend(destination_segments);
                }
                blocks.remove(destination_index);
            }
            ResolveSelectionAction::Accept => {
                if let BlockNode::Paragraph(source) = &mut blocks[source_index].block {
                    remove_move_decorations(source);
                }
                if let BlockNode::Paragraph(destination) = &mut blocks[destination_index].block {
                    remove_move_decorations(destination);
                }
                let source_empty = matches!(
                    &blocks[source_index].block,
                    BlockNode::Paragraph(paragraph) if !paragraph_has_content(paragraph)
                );
                if source_empty {
                    blocks.remove(source_index);
                }
            }
        }
    }
}

fn selected_move_names(
    blocks: &[TrackedBlock],
    selected_revision_ids: &HashSet<u32>,
) -> HashSet<String> {
    fn record(status: &TrackingStatus, polarities: &mut HashMap<u32, u8>) {
        match status {
            TrackingStatus::Normal => {}
            TrackingStatus::Inserted(revision) => {
                *polarities.entry(revision.identity).or_default() |= 1;
            }
            TrackingStatus::Deleted(revision) => {
                *polarities.entry(revision.identity).or_default() |= 2;
            }
            TrackingStatus::InsertedThenDeleted(stacked) => {
                *polarities.entry(stacked.inserted.identity).or_default() |= 1;
                *polarities.entry(stacked.deleted.identity).or_default() |= 2;
            }
        }
    }

    let mut by_name: HashMap<String, HashMap<u32, u8>> = HashMap::new();
    for block in blocks {
        let Some(move_name) = &block.move_id else {
            continue;
        };
        let polarities = by_name.entry(move_name.clone()).or_default();
        record(&block.status, polarities);
        if let BlockNode::Paragraph(paragraph) = &block.block {
            for segment in &paragraph.segments {
                record(&segment.status, polarities);
            }
            if let Some(status) = &paragraph.para_mark_status {
                record(status, polarities);
            }
        }
    }

    by_name
        .into_iter()
        .filter_map(|(name, polarities)| {
            polarities
                .into_iter()
                .any(|(identity, flags)| flags == 3 && selected_revision_ids.contains(&identity))
                .then_some(name)
        })
        .collect()
}

fn clear_resolved_move_markup(blocks: &mut [TrackedBlock], resolved_move_names: &HashSet<String>) {
    if resolved_move_names.is_empty() {
        return;
    }
    for block in blocks {
        if !block
            .move_id
            .as_ref()
            .is_some_and(|name| resolved_move_names.contains(name))
        {
            continue;
        }
        block.move_id = None;
        if let BlockNode::Paragraph(paragraph) = &mut block.block {
            for segment in &mut paragraph.segments {
                segment.inlines.retain(|inline| {
                    !matches!(
                        inline,
                        InlineNode::Decoration(decoration)
                            if decoration.kind == crate::domain::DecorationType::MoveRange
                    )
                });
            }
            paragraph
                .paragraph_mark_style_props
                .preserved
                .retain(|property| property.name != "w:moveFrom" && property.name != "w:moveTo");
        }
    }
}

/// The revision ids resolved AS A CASCADE by applying `action` to
/// `selected_revision_ids` (cascades are enumerated, never
/// silent): for every stacked segment whose drop is entailed by resolving
/// only ONE member of its pair, the OTHER member's id. An id named here may
/// still have pending segments elsewhere (revisions span segments) — it names
/// a revision AFFECTED by the cascade, not necessarily fully resolved.
pub fn cascaded_resolution_ids(
    doc: &CanonDoc,
    selected_revision_ids: &HashSet<u32>,
    action: ResolveSelectionAction,
) -> HashSet<u32> {
    fn visit_status(
        status: &TrackingStatus,
        ids: &HashSet<u32>,
        action: ResolveSelectionAction,
        out: &mut HashSet<u32>,
    ) {
        if let TrackingStatus::InsertedThenDeleted(sr) = status {
            let ins_sel = ids.contains(&sr.inserted.identity);
            let del_sel = ids.contains(&sr.deleted.identity);
            match (ins_sel, del_sel, action) {
                // Rejecting the insertion discards the pending deletion
                // with it (the Word cascade).
                (true, false, ResolveSelectionAction::Reject) => {
                    out.insert(sr.deleted.identity);
                }
                // Accepting the deletion settles the insertion's claim on
                // this range — the content is gone either way.
                (false, true, ResolveSelectionAction::Accept) => {
                    out.insert(sr.inserted.identity);
                }
                _ => {}
            }
        }
    }
    fn visit_paragraph(
        p: &ParagraphNode,
        ids: &HashSet<u32>,
        action: ResolveSelectionAction,
        out: &mut HashSet<u32>,
    ) {
        for segment in &p.segments {
            visit_status(&segment.status, ids, action, out);
        }
        if let Some(status) = &p.para_mark_status {
            visit_status(status, ids, action, out);
        }
    }
    fn visit_blocks(
        blocks: &[TrackedBlock],
        ids: &HashSet<u32>,
        action: ResolveSelectionAction,
        out: &mut HashSet<u32>,
    ) {
        for tb in blocks {
            visit_block(&tb.block, ids, action, out);
        }
    }
    fn visit_block(
        block: &BlockNode,
        ids: &HashSet<u32>,
        action: ResolveSelectionAction,
        out: &mut HashSet<u32>,
    ) {
        match block {
            BlockNode::Paragraph(p) => visit_paragraph(p, ids, action, out),
            BlockNode::Table(t) => {
                for row in &t.rows {
                    if let Some(status) = &row.tracking_status {
                        visit_status(status, ids, action, out);
                    }
                    for cell in &row.cells {
                        if let Some(status) = &cell.tracking_status {
                            visit_status(status, ids, action, out);
                        }
                        for nested in &cell.blocks {
                            visit_block(nested, ids, action, out);
                        }
                    }
                }
            }
            BlockNode::OpaqueBlock(_) => {}
        }
    }
    let mut out = HashSet::new();
    visit_blocks(&doc.blocks, selected_revision_ids, action, &mut out);
    for story in &doc.headers {
        visit_blocks(&story.blocks, selected_revision_ids, action, &mut out);
    }
    for story in &doc.footers {
        visit_blocks(&story.blocks, selected_revision_ids, action, &mut out);
    }
    for story in &doc.footnotes {
        visit_blocks(&story.blocks, selected_revision_ids, action, &mut out);
    }
    for story in &doc.endnotes {
        visit_blocks(&story.blocks, selected_revision_ids, action, &mut out);
    }
    for story in &doc.comments {
        visit_blocks(&story.blocks, selected_revision_ids, action, &mut out);
    }
    out
}

/// Every revision id `resolve_selected_revisions` is able to act on: the
/// carrier set `project_block_for_selected_resolution` (and the story-level
/// code around it) walks — block/row/cell tracking status (both legs of a
/// stacked pair), paragraph marks, pPrChange/rPrChange/tblPrChange/
/// trPrChange/tcPrChange, hyperlink run status, comment-story tracking
/// status, and section-property changes (the body-level `w:sectPrChange`
/// resolved in `resolve_selected_revisions`'s tail, and the mid-document
/// paragraph-level sibling). Membership here is action-independent (matching
/// `selected_tracking_outcome`'s membership check, which is evaluated before
/// the accept/reject branch), so "present here" and "the resolver will touch
/// it" are the same fact.
///
/// This is the completeness oracle for the domain rule: a selected id that
/// resolves to nothing is a caller error (stale, mistyped, or an unhandled
/// carrier), never a silent no-op. See `resolve_selected_revisions`.
fn resolvable_revision_ids(doc: &CanonDoc) -> HashSet<u32> {
    let mut out = body_resolvable_revision_ids(doc);
    // Interior (opaque raw_xml) revisions: only the ids that uniquely identify
    // one interior carrier document-wide are selectable (RFC-0002 §Phase-3b +
    // the duplicate-wild-id demotion — see `classify_interior_ids`). Must
    // agree with the enumerate census.
    out.extend(classify_interior_ids(doc).selectable);
    out
}

/// The BODY/story half of [`resolvable_revision_ids`]: every carrier the typed
/// model addresses by id (statuses, formatting changes, hyperlink runs,
/// sectPrChange), excluding opaque-interior carriers. This is exactly the id
/// population `import::for_each_revision_id_mut` normalizes at import — the
/// mirror test binds the two walks.
fn body_resolvable_revision_ids(doc: &CanonDoc) -> HashSet<u32> {
    fn visit_status(status: &TrackingStatus, out: &mut HashSet<u32>) {
        match status {
            TrackingStatus::Normal => {}
            TrackingStatus::Inserted(r) | TrackingStatus::Deleted(r) => {
                out.insert(r.identity);
            }
            TrackingStatus::InsertedThenDeleted(sr) => {
                out.insert(sr.inserted.identity);
                out.insert(sr.deleted.identity);
            }
        }
    }
    fn visit_optional_status(status: &Option<TrackingStatus>, out: &mut HashSet<u32>) {
        if let Some(status) = status {
            visit_status(status, out);
        }
    }
    // A pre-identity formatting change (legacy snapshot, or one never passed
    // through the mint walk) carries `identity == 0` — never selectable, so the
    // resolver's `*PrChange` match guards on `identity != 0`; mirror that guard.
    fn visit_formatting_change_id(identity: u32, out: &mut HashSet<u32>) {
        if identity != 0 {
            out.insert(identity);
        }
    }
    fn visit_paragraph(p: &ParagraphNode, out: &mut HashSet<u32>) {
        // A mid-document section break's own sectPrChange — resolved inline
        // in `project_block_for_selected_resolution`, so it must be a member
        // here or selecting it would be refused as unresolvable.
        if let Some(change) = &p.section_property_change {
            visit_formatting_change_id(change.revision.identity, out);
        }
        for segment in &p.segments {
            visit_status(&segment.status, out);
            for inline in &segment.inlines {
                match inline {
                    InlineNode::Text(t) => {
                        if let Some(fc) = &t.formatting_change {
                            visit_formatting_change_id(fc.identity, out);
                        }
                    }
                    InlineNode::OpaqueInline(opaque) => {
                        // Hyperlink runs are typed/modeled (resolved via the
                        // typed path); NON-hyperlink opaque interiors are the
                        // classified-interior half, added by the caller
                        // (`resolvable_revision_ids`) from
                        // `classify_interior_ids` — not here, so this walk
                        // stays the exact mirror of the import mint walk.
                        if let crate::domain::OpaqueKind::Hyperlink(data) = &opaque.kind {
                            for run in &data.runs {
                                visit_status(&run.status, out);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        visit_optional_status(&p.para_mark_status, out);
        if let Some(fc) = &p.formatting_change {
            visit_formatting_change_id(fc.identity, out);
        }
    }
    fn visit_blocks(blocks: &[TrackedBlock], out: &mut HashSet<u32>) {
        for tb in blocks {
            visit_status(&tb.status, out);
            visit_block(&tb.block, out);
        }
    }
    fn visit_block(block: &BlockNode, out: &mut HashSet<u32>) {
        match block {
            BlockNode::Paragraph(p) => visit_paragraph(p, out),
            BlockNode::Table(t) => {
                if let Some(fc) = &t.formatting_change {
                    visit_formatting_change_id(fc.identity, out);
                }
                for row in &t.rows {
                    visit_optional_status(&row.tracking_status, out);
                    if let Some(fc) = &row.formatting_change {
                        visit_formatting_change_id(fc.identity, out);
                    }
                    for cell in &row.cells {
                        visit_optional_status(&cell.tracking_status, out);
                        if let Some(fc) = &cell.formatting_change {
                            visit_formatting_change_id(fc.identity, out);
                        }
                        for nested in &cell.blocks {
                            visit_block(nested, out);
                        }
                    }
                }
            }
            BlockNode::OpaqueBlock(_) => {}
        }
    }
    let mut out = HashSet::new();
    visit_blocks(&doc.blocks, &mut out);
    for story in &doc.headers {
        visit_blocks(&story.blocks, &mut out);
    }
    for story in &doc.footers {
        visit_blocks(&story.blocks, &mut out);
    }
    for story in &doc.footnotes {
        visit_blocks(&story.blocks, &mut out);
    }
    for story in &doc.endnotes {
        visit_blocks(&story.blocks, &mut out);
    }
    for story in &doc.comments {
        visit_optional_status(&story.tracking_status, &mut out);
        visit_blocks(&story.blocks, &mut out);
    }
    // The body-level `w:sectPrChange` lives outside `doc.blocks`; it is
    // resolved by `resolve_selected_revisions`'s tail, so it is a member of
    // the carrier set — mirroring `enumerate_revisions`'s sentinel record.
    if let Some(change) = &doc.body_section_property_change {
        visit_formatting_change_id(change.revision.identity, &mut out);
    }
    out
}

/// Resolve every selected revision id across the whole document — the body,
/// every story, AND the body-level `w:sectPrChange` (which lives outside
/// `doc.blocks`, so the shared per-block walk can't reach it).
///
/// Refuses up front — mutating nothing — if any selected id matches no
/// carrier `resolvable_revision_ids` can see: a caller-supplied id that
/// resolves to nothing (stale, mistyped, or living in a carrier this
/// selector doesn't handle) is an error, not a silent success. Returns the
/// sorted list of unmatched ids as `Err` on refusal.
///
/// On success, returns `Ok(Some(keep_new))` when the body-level
/// section-properties change was among the resolved ids (`true` = accepted,
/// `false` = rejected) — `Ok(None)` otherwise (no such change, or it wasn't
/// selected this call). The caller (`EditSnapshot::project`) uses this to
/// also resolve the RAW `sectPr` cache the serializer's verbatim path reads,
/// mirroring exactly what `AcceptAll`/`RejectAll` already do for that cache
/// — see `project`'s own comment on why the model-level and byte-level
/// resolutions must agree.
/// STYLE-TABLE-FREE and therefore DEGRADED for a selectively-rejected
/// paragraph-style change exactly as the bare [`reject_all`] is: `w:pStyle`
/// reverts but the runs' style-inherited marks are not re-resolved. Use
/// [`resolve_selected_revisions_with_styles`] (or the runtime projection) when
/// the selection may reject a tracked paragraph-style change.
#[deprecated(
    note = "style-table-free and DEGRADED when the selection rejects a tracked \
            paragraph-style change (leaves style-inherited run marks baked). Use \
            resolve_selected_revisions_with_styles(doc, ids, action, \
            style_table_from_docx(bytes)?.as_ref()) for fidelity; pass None only \
            for a provably style-less doc."
)]
pub fn resolve_selected_revisions(
    doc: &mut CanonDoc,
    selected_revision_ids: &HashSet<u32>,
    action: ResolveSelectionAction,
) -> Result<Option<bool>, Vec<u32>> {
    resolve_selected_revisions_with_style_defs(doc, selected_revision_ids, action, None)
}

/// [`resolve_selected_revisions`] with the document's style table, so a
/// selectively-rejected paragraph-style change (`w:pPrChange`) re-resolves each
/// affected run's style-inherited marks against the restored style. See
/// [`reject_all_with_styles`]; obtain `styles` from
/// [`crate::style_table_from_docx`].
pub fn resolve_selected_revisions_with_styles(
    doc: &mut CanonDoc,
    selected_revision_ids: &HashSet<u32>,
    action: ResolveSelectionAction,
    styles: Option<&crate::styles::StyleTable>,
) -> Result<Option<bool>, Vec<u32>> {
    resolve_selected_revisions_with_style_defs(
        doc,
        selected_revision_ids,
        action,
        styles.map(|s| &s.0),
    )
}

/// `resolve_selected_revisions` with the document's `StyleDefinitions`, so a
/// selectively-rejected paragraph-style change (`w:pPrChange`) re-resolves
/// style-inherited run marks against the restored style.
pub(crate) fn resolve_selected_revisions_with_style_defs(
    doc: &mut CanonDoc,
    selected_revision_ids: &HashSet<u32>,
    action: ResolveSelectionAction,
    style_defs: Option<&crate::styles::StyleDefinitions>,
) -> Result<Option<bool>, Vec<u32>> {
    let resolvable = resolvable_revision_ids(doc);
    let mut unresolved: Vec<u32> = selected_revision_ids
        .difference(&resolvable)
        .copied()
        .collect();
    if !unresolved.is_empty() {
        unresolved.sort_unstable();
        return Err(unresolved);
    }
    // The ids the opaque-interior descent may resolve: selected AND classified
    // as uniquely identifying one interior carrier. The body projection keeps
    // the full selected set; the descent must never see a body id — a wild
    // interior carrier sharing a selected body id (`classify_interior_ids`'
    // demotion case) would otherwise be silently co-resolved.
    let interior_selected: HashSet<u32> = classify_interior_ids(doc)
        .selectable
        .intersection(selected_revision_ids)
        .copied()
        .collect();

    project_blocks_for_selected_resolution(
        &mut doc.blocks,
        action,
        selected_revision_ids,
        &interior_selected,
        style_defs,
    );
    // Re-establish the document-final-mark invariant the projection can break
    // (suppression stripped off a tracked tail, or an anchor stranded as final
    // once its followers were resolved away). Body only — a header/footer/cell
    // final mark is not the document-final mark.
    renormalize_final_mark_after_selective(&mut doc.blocks);
    for story in &mut doc.headers {
        project_blocks_for_selected_resolution(
            &mut story.blocks,
            action,
            selected_revision_ids,
            &interior_selected,
            style_defs,
        );
        story.content_hash = recompute_content_hash(&story.blocks);
    }
    for story in &mut doc.footers {
        project_blocks_for_selected_resolution(
            &mut story.blocks,
            action,
            selected_revision_ids,
            &interior_selected,
            style_defs,
        );
        story.content_hash = recompute_content_hash(&story.blocks);
    }
    for story in &mut doc.footnotes {
        project_blocks_for_selected_resolution(
            &mut story.blocks,
            action,
            selected_revision_ids,
            &interior_selected,
            style_defs,
        );
        story.content_hash = recompute_content_hash(&story.blocks);
    }
    for story in &mut doc.endnotes {
        project_blocks_for_selected_resolution(
            &mut story.blocks,
            action,
            selected_revision_ids,
            &interior_selected,
            style_defs,
        );
        story.content_hash = recompute_content_hash(&story.blocks);
    }
    for story in &mut doc.comments {
        let story_outcome = selected_optional_tracking_outcome(
            &story.tracking_status,
            action,
            selected_revision_ids,
        );
        if story_outcome == SelectedTrackingOutcome::KeepNormal {
            story.tracking_status = None;
        }
        project_blocks_for_selected_resolution(
            &mut story.blocks,
            action,
            selected_revision_ids,
            &interior_selected,
            style_defs,
        );
        story.content_hash = recompute_content_hash(&story.blocks);
    }
    doc.comments.retain(|story| {
        let story_outcome = selected_optional_tracking_outcome(
            &story.tracking_status,
            action,
            selected_revision_ids,
        );
        story_outcome != SelectedTrackingOutcome::Drop && !story.blocks.is_empty()
    });
    // Same orphan cleanup as accept_all/reject_all: a selective resolution
    // can empty a footnote/endnote story's blocks (e.g. resolving a tracked
    // InsertNote's story-body revision) with no story-level tracking field to
    // also drop the entry.
    doc.footnotes.retain(|s| !s.blocks.is_empty());
    doc.endnotes.retain(|s| !s.blocks.is_empty());

    // Body-level section-properties change (§17.13.5.32): the paragraph-level
    // (mid-document) sibling is resolved inline in
    // `project_block_for_selected_resolution` because it has a real hosting
    // block; this one is a `CanonDoc`-level field with none, so it's handled
    // here instead — same reasoning as `enumerate_revisions` emitting it with
    // a sentinel `block_id`.
    let should_resolve = matches!(
        &doc.body_section_property_change,
        Some(change)
            if change.revision.revision_id != 0
                && selected_revision_ids.contains(&change.revision.identity)
    );
    if !should_resolve {
        return Ok(None);
    }
    let change = doc
        .body_section_property_change
        .take()
        .expect("just matched Some above");
    let keep_new = action == ResolveSelectionAction::Accept;
    if !keep_new
        && let Some(prev) = parse_previous_section_properties(&change.previous_properties_raw)
    {
        doc.body_section_properties = Some(prev);
    }
    // If the raw snapshot fails to parse on reject, leave the current
    // properties in place rather than fabricate a default — same defensive
    // stance as `project_body_section_for_accept_reject`'s reject arm (a
    // sectPrChange the engine authored always round-trips; this is a
    // defensive branch, not an expected path).
    Ok(Some(keep_new))
}

/// Recompute `content_hash` for a story from its projected blocks.
///
/// After `accept_all`/`reject_all` mutates story blocks, the hash is stale.
/// Without recomputation, `diff_documents` sees a hash mismatch and reports
/// a spurious story change even when the block content is identical.
fn recompute_content_hash(blocks: &[TrackedBlock]) -> String {
    let mut hasher = Sha256::new();
    for tb in blocks {
        let text = extract_block_text_for_hash(&tb.block);
        hasher.update(text.as_bytes());
        hasher.update(b"|");
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Extract text from a block for hashing (mirrors `runtime::extract_block_text`).
pub(crate) fn extract_block_text_for_hash(block: &BlockNode) -> String {
    match block {
        BlockNode::Paragraph(p) => {
            let mut out = String::new();
            for inline in p.all_inlines() {
                match inline {
                    InlineNode::Text(t) => out.push_str(&t.text),
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
        BlockNode::Table(t) => {
            let mut out = String::new();
            for row in &t.rows {
                for cell in &row.cells {
                    for nested in &cell.blocks {
                        out.push_str(&extract_block_text_for_hash(nested));
                        out.push(' ');
                    }
                }
            }
            out
        }
        BlockNode::OpaqueBlock(_) => String::new(),
    }
}

/// One author's share of a document's PENDING revisions — the disclosure
/// unit for compare's flatten contract (see `ViewResult::flattened_pending_revisions`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingRevisionAuthor {
    /// `w:author` of the revisions; `None` when the markup carried none.
    pub author: Option<String>,
    /// Distinct revisions by this author: distinct `revision_id`s across all
    /// tracked carriers (segments, paragraph marks, rows, cells, hyperlink
    /// runs, block containers, every story), with the stacked state
    /// contributing BOTH its revisions, plus one per tracked
    /// formatting-change record (`*PrChange` — stored without an id).
    pub revision_count: u32,
}

/// Summarize the pending revisions a document carries, grouped by author —
/// what an accept-all flattening (e.g. `view()` feeding compare) consumes.
/// Quarantined opaque blocks are excluded: the model preserves their raw
/// bytes verbatim, revisions included, so nothing in them is consumed.
pub fn pending_revision_authors(doc: &CanonDoc) -> Vec<PendingRevisionAuthor> {
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Collector {
        ids: HashSet<(Option<String>, u32)>,
        formatting_records: BTreeMap<Option<String>, u32>,
    }

    impl Collector {
        fn status(&mut self, status: &TrackingStatus) {
            match status {
                TrackingStatus::Normal => {}
                TrackingStatus::Inserted(r) | TrackingStatus::Deleted(r) => {
                    self.ids.insert((r.author.clone(), r.revision_id));
                }
                TrackingStatus::InsertedThenDeleted(sr) => {
                    self.ids
                        .insert((sr.inserted.author.clone(), sr.inserted.revision_id));
                    self.ids
                        .insert((sr.deleted.author.clone(), sr.deleted.revision_id));
                }
            }
        }
        fn formatting_record(&mut self, author: &str) {
            *self
                .formatting_records
                .entry(Some(author.to_string()))
                .or_insert(0) += 1;
        }
        fn block(&mut self, block: &BlockNode) {
            match block {
                BlockNode::Paragraph(p) => {
                    if let Some(s) = &p.para_mark_status {
                        self.status(s);
                    }
                    if let Some(fc) = &p.formatting_change {
                        self.formatting_record(&fc.author);
                    }
                    for seg in &p.segments {
                        self.status(&seg.status);
                        for inline in &seg.inlines {
                            match inline {
                                InlineNode::Text(t) => {
                                    if let Some(fc) = &t.formatting_change {
                                        self.formatting_record(&fc.author);
                                    }
                                }
                                InlineNode::OpaqueInline(o) => {
                                    if let crate::domain::OpaqueKind::Hyperlink(data) = &o.kind {
                                        for run in &data.runs {
                                            self.status(&run.status);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                BlockNode::Table(t) => {
                    if let Some(fc) = &t.formatting_change {
                        self.formatting_record(&fc.author);
                    }
                    for row in &t.rows {
                        if let Some(s) = &row.tracking_status {
                            self.status(s);
                        }
                        if let Some(fc) = &row.formatting_change {
                            self.formatting_record(&fc.author);
                        }
                        for cell in &row.cells {
                            if let Some(s) = &cell.tracking_status {
                                self.status(s);
                            }
                            if let Some(fc) = &cell.formatting_change {
                                self.formatting_record(&fc.author);
                            }
                            for nested in &cell.blocks {
                                self.block(nested);
                            }
                        }
                    }
                }
                BlockNode::OpaqueBlock(_) => {}
            }
        }
        fn tracked_blocks(&mut self, blocks: &[TrackedBlock]) {
            for tb in blocks {
                self.status(&tb.status);
                self.block(&tb.block);
            }
        }
    }

    let mut c = Collector::default();
    c.tracked_blocks(&doc.blocks);
    for story in &doc.headers {
        c.tracked_blocks(&story.blocks);
    }
    for story in &doc.footers {
        c.tracked_blocks(&story.blocks);
    }
    for story in &doc.footnotes {
        c.tracked_blocks(&story.blocks);
    }
    for story in &doc.endnotes {
        c.tracked_blocks(&story.blocks);
    }
    for story in &doc.comments {
        c.tracked_blocks(&story.blocks);
    }

    let mut counts = c.formatting_records;
    for (author, _) in c.ids {
        *counts.entry(author).or_insert(0) += 1;
    }
    counts
        .into_iter()
        .map(|(author, revision_count)| PendingRevisionAuthor {
            author,
            revision_count,
        })
        .collect()
}

pub fn accept_all(doc: &mut CanonDoc) {
    // Accept keeps each paragraph's CURRENT style (it only drops the pPrChange
    // record), so no run mark needs re-resolving — the style table is not needed.
    project_blocks_for_accept_reject(&mut doc.blocks, true, None);
    for story in &mut doc.headers {
        project_blocks_for_accept_reject(&mut story.blocks, true, None);
        story.content_hash = recompute_content_hash(&story.blocks);
    }
    for story in &mut doc.footers {
        project_blocks_for_accept_reject(&mut story.blocks, true, None);
        story.content_hash = recompute_content_hash(&story.blocks);
    }
    for story in &mut doc.footnotes {
        project_blocks_for_accept_reject(&mut story.blocks, true, None);
        story.content_hash = recompute_content_hash(&story.blocks);
    }
    for story in &mut doc.endnotes {
        project_blocks_for_accept_reject(&mut story.blocks, true, None);
        story.content_hash = recompute_content_hash(&story.blocks);
    }
    for story in &mut doc.comments {
        project_blocks_for_accept_reject(&mut story.blocks, true, None);
        story.content_hash = recompute_content_hash(&story.blocks);
    }
    // Remove comment stories whose blocks were all projected away, or that
    // were marked as deleted at the story level (CommentDeleted diff change).
    doc.comments.retain(|s| {
        !s.blocks.is_empty() && !matches!(s.tracking_status, Some(TrackingStatus::Deleted(_)))
    });
    // Same cleanup for footnote/endnote stories: a tracked `DeleteNote`
    // (accept) or an `InsertNote` (reject) can project a story's only block
    // away, leaving `blocks: []` with no comment-style story-level tracking
    // to also drop the entry — without this retain the empty story lingers
    // in `doc.footnotes`/`doc.endnotes` as an orphan with no reference and no
    // content (CLAUDE.md "no silent fallback": an empty note is not a valid
    // resting state, so it must not survive resolution).
    doc.footnotes.retain(|s| !s.blocks.is_empty());
    doc.endnotes.retain(|s| !s.blocks.is_empty());
    // Body-level section change (w:sectPrChange, §17.13.5.32): accept keeps the
    // new section properties and drops the change record.
    project_body_section_for_accept_reject(doc, true);
}

/// Accept/reject a body-level `w:sectPrChange` (§17.13.5.32) at the model layer.
///
/// On accept (`keep_new = true`) the new `body_section_properties` are kept and
/// the change record is dropped. On reject the previous `w:sectPr` recorded in
/// `previous_properties_raw` is parsed back into `SectionProperties` and
/// restored. This is the model-level counterpart to the byte-level
/// accept/reject `normalize.rs` does on the serialized wrapper, so the
/// reject-all == baseline / accept-all == target invariant holds on the IR too.
fn project_body_section_for_accept_reject(doc: &mut CanonDoc, keep_new: bool) {
    let Some(change) = doc.body_section_property_change.take() else {
        return;
    };
    if keep_new {
        // Accept: keep doc.body_section_properties as-is; the change is gone.
        return;
    }
    // Reject: restore the previous section properties from the raw snapshot.
    if let Some(prev) = parse_previous_section_properties(&change.previous_properties_raw) {
        doc.body_section_properties = Some(prev);
    }
    // If the raw failed to parse we leave the current props in place rather than
    // dropping the section entirely — but a sectPrChange we authored always
    // round-trips, so this is a defensive branch, not an expected path.

    // Prune the BLANK, now-unreferenced header/footer stories whose creating
    // reference this reject just removed. `CreateHeader`/`CreateFooter` author a
    // blank story plus a sectPrChange that adds its reference; rejecting the
    // change drops the reference (CT_SectPrBase carries no EG_HdrFtrReferences,
    // so the restored previous sectPr has none), and the blank backing story
    // would otherwise persist as an orphan part — diverging from the original.
    // The prune is tightly scoped: a story is removed ONLY when it is both
    // (a) referenced by NO section anywhere in the doc AND (b) blank (carries no
    // visible text), so a document's pre-existing content-bearing orphan headers
    // are never touched.
    // §17.10.5 blank-synthesis is DERIVED view-state (never serialized):
    // drop it wholesale and recompute after the restore — the restored
    // previous sectPr carries only AUTHORED refs (synthesized refs never
    // serialize into history), so gap-filling over stale derived stories
    // would duplicate them.
    doc.headers.retain(|h| !h.synthesized);
    doc.footers.retain(|f| !f.synthesized);
    let strip_synth = |sp: &mut SectionProperties| {
        sp.header_refs.retain(|r| !r.synthesized);
        sp.footer_refs.retain(|r| !r.synthesized);
    };
    if let Some(sp) = doc.body_section_properties.as_mut() {
        strip_synth(sp);
    }
    prune_blank_unreferenced_stories(doc);
    crate::import::synthesize_blank_headers_for_first_section(doc);
    crate::import::synthesize_blank_footers_for_first_section(doc);
}

/// Remove header/footer stories that are both unreferenced by any section and
/// blank. See `project_body_section_for_accept_reject` for why this runs only on
/// the reject path of a body sectPrChange.
fn prune_blank_unreferenced_stories(doc: &mut CanonDoc) {
    let referenced: std::collections::HashSet<String> = section_referenced_part_paths(doc);
    doc.headers
        .retain(|h| referenced.contains(&h.part_name) || story_has_visible_text(&h.blocks));
    doc.footers
        .retain(|f| referenced.contains(&f.part_name) || story_has_visible_text(&f.blocks));
}

/// Every header/footer part path referenced by the body section or by any
/// paragraph-level (mid-document) section break.
fn section_referenced_part_paths(doc: &CanonDoc) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let mut collect = |sp: &SectionProperties| {
        for r in sp.header_refs.iter().chain(sp.footer_refs.iter()) {
            out.insert(r.part_path.clone());
        }
    };
    if let Some(sp) = &doc.body_section_properties {
        collect(sp);
    }
    for tb in &doc.blocks {
        if let BlockNode::Paragraph(p) = &tb.block
            && let Some(sp) = &p.section_properties
        {
            collect(sp);
        }
    }
    out
}

/// True when any block in the story carries visible text (a non-empty rendered
/// paragraph). A story authored blank by `CreateHeader`/`CreateFooter` (a single
/// empty paragraph) reports `false`.
fn story_has_visible_text(blocks: &[TrackedBlock]) -> bool {
    blocks.iter().any(|tb| match &tb.block {
        BlockNode::Paragraph(p) => {
            p.segments
                .iter()
                .flat_map(|s| s.inlines.iter())
                .any(|i| match i {
                    InlineNode::Text(t) => !t.text.trim().is_empty(),
                    // Any opaque/decoration inline counts as content (a field, image,
                    // page-number run, etc.) — never prune a story that carries one.
                    _ => true,
                })
        }
        // A table or opaque block is content.
        _ => true,
    })
}

/// Parse a raw `w:sectPr` fragment (the inner element of a `w:sectPrChange`)
/// back into typed [`SectionProperties`]. Returns `None` if the bytes do not
/// parse — the caller decides how to handle that.
///
/// The snapshot is produced by `section_properties_to_element(.., resolve_rid =
/// None)`, which writes each header/footer reference's `r:id` as the literal
/// part_path (the raw-XML convention). So to round-trip those references we feed
/// `parse_section_properties` an IDENTITY relationship map (each `r:id` found in
/// the fragment maps to itself). Without it the refs would be silently dropped
/// (the empty-map "rId not found, skipping" branch), which would lose a section's
/// pre-existing header/footer references on reject.
fn parse_previous_section_properties(raw: &[u8]) -> Option<SectionProperties> {
    let el = crate::word_xml::parse_raw_fragment(raw).ok()?;
    let mut rel_lookup = std::collections::HashMap::new();
    collect_identity_story_rids(&el, &mut rel_lookup);
    Some(crate::word_ir::parse_section_properties(&el, &rel_lookup))
}

/// Walk a raw `w:sectPr` element's `headerReference` / `footerReference` children
/// and map each `r:id` value to itself, so the part_path the snapshot wrote as
/// the `r:id` round-trips back into the parsed `StoryRef.part_path`.
fn collect_identity_story_rids(
    sect_pr: &xmltree::Element,
    rel_lookup: &mut std::collections::HashMap<String, String>,
) {
    for child in &sect_pr.children {
        if let xmltree::XMLNode::Element(el) = child
            && (crate::word_xml::is_w_tag(el, "headerReference")
                || crate::word_xml::is_w_tag(el, "footerReference"))
            && let Some(rid) = crate::xml_attrs::attr_get(el, "id")
        {
            rel_lookup.insert(rid.clone(), rid.clone());
        }
    }
}

/// Reject every tracked change, resolving the document to its baseline.
///
/// STYLE-TABLE-FREE and therefore DEGRADED for one case: rejecting a tracked
/// paragraph-style change (`w:pPrChange`, §17.13.5.29) reverts `w:pStyle` but
/// does NOT re-resolve the runs' style-inherited marks (caps, bold, fonts, …),
/// because those were baked against the style table at import time and undoing
/// the baking needs that table (which a bare [`CanonDoc`] does not carry). The
/// result is a run that renders e.g. uppercase after its caps-bearing style was
/// rejected. This is correct for a document with no `word/styles.xml` (nothing
/// to inherit) and for style-less test fixtures, but WRONG for an imported
/// document that carries such a change.
///
/// For full fidelity use [`reject_all_with_styles`] with the document's
/// [`crate::style_table_from_docx`], or the runtime projection
/// ([`crate::api::Document::read_rejected`] / [`EditSnapshot::project`]), both of
/// which re-resolve automatically.
#[deprecated(
    note = "style-table-free and DEGRADED for imported docs carrying a tracked \
            paragraph-style change (leaves style-inherited run marks baked). Use \
            reject_all_with_styles(doc, style_table_from_docx(bytes)?.as_ref()) \
            for fidelity; pass None only for a provably style-less doc."
)]
pub fn reject_all(doc: &mut CanonDoc) {
    reject_all_with_style_defs(doc, None);
}

/// [`reject_all`] with the document's style table, so rejecting a tracked
/// paragraph-style change (`w:pPrChange`) ALSO re-resolves each affected run's
/// style-inherited marks (caps, bold, fonts, …) against the RESTORED style. This
/// is the correct entry point whenever the document may carry a tracked
/// paragraph-style change; the bare [`reject_all`] leaves those marks baked
/// against the rejected style.
///
/// Obtain `styles` from [`crate::style_table_from_docx`] (parse the same DOCX
/// bytes the [`CanonDoc`] was imported from). Pass `None` only when the document
/// provably has no style table — in which case this is identical to
/// [`reject_all`].
pub fn reject_all_with_styles(doc: &mut CanonDoc, styles: Option<&crate::styles::StyleTable>) {
    reject_all_with_style_defs(doc, styles.map(|s| &s.0));
}

pub(crate) fn reject_all_with_style_defs(
    doc: &mut CanonDoc,
    style_defs: Option<&crate::styles::StyleDefinitions>,
) {
    project_blocks_for_accept_reject(&mut doc.blocks, false, style_defs);
    for story in &mut doc.headers {
        project_blocks_for_accept_reject(&mut story.blocks, false, style_defs);
        story.content_hash = recompute_content_hash(&story.blocks);
    }
    for story in &mut doc.footers {
        project_blocks_for_accept_reject(&mut story.blocks, false, style_defs);
        story.content_hash = recompute_content_hash(&story.blocks);
    }
    for story in &mut doc.footnotes {
        project_blocks_for_accept_reject(&mut story.blocks, false, style_defs);
        story.content_hash = recompute_content_hash(&story.blocks);
    }
    for story in &mut doc.endnotes {
        project_blocks_for_accept_reject(&mut story.blocks, false, style_defs);
        story.content_hash = recompute_content_hash(&story.blocks);
    }
    for story in &mut doc.comments {
        // On reject, clear story-level deletion tracking (keep the comment).
        story.tracking_status = None;
        project_blocks_for_accept_reject(&mut story.blocks, false, style_defs);
        story.content_hash = recompute_content_hash(&story.blocks);
    }
    // Remove comment stories whose blocks were all projected away.
    doc.comments.retain(|s| !s.blocks.is_empty());
    // Same cleanup for footnote/endnote stories (see accept_all's identical
    // retain for why an empty story must not survive resolution): a tracked
    // `InsertNote`, on reject, projects its Inserted story block away,
    // leaving `blocks: []` with no orphan cleanup otherwise.
    doc.footnotes.retain(|s| !s.blocks.is_empty());
    doc.endnotes.retain(|s| !s.blocks.is_empty());
    // Body-level section change (w:sectPrChange, §17.13.5.32): reject restores
    // the prior section properties and drops the change record.
    project_body_section_for_accept_reject(doc, false);
}

// =============================================================================
// Unified post-producer body-state validator (hardening H2)
// =============================================================================
//
// Five recent campaign waves kept hitting the SAME meta-bug: a body-state
// invariant that one producer establishes (a mint site) is quietly violated by
// another producer — most often the accept/reject/selective RESOLUTION
// projections, which reshape the body without re-running the mint-time
// normalizers. Each instance was fixed at its own site. This validator removes
// the *class*: it collects the invariants into one pass that runs (as a
// debug-assert) after EVERY producer of body state, so a regression in any
// producer surfaces immediately, named, instead of days later as a corrupt
// output.
//
// Each invariant here has an AUTHORITATIVE statement elsewhere in the engine;
// this module re-checks it, it does not re-decide it:
//
//  1. FINAL-MARK RULE — the document-final paragraph mark never carries a
//     pending tracked insertion/deletion (Word cannot resolve one on the final
//     pilcrow). Authoritative: `normalize_final_mark_attribution` and the doc
//     comment on `renormalize_final_mark_after_selective`. The single exemption
//     mirrors serialize-time semantics: a moveFrom SHADOW (block-`Deleted` +
//     `move_id`) at the document end is resolved by its own `moveFromRange`
//     pairing, not this rule.
//
//  2. RANGE-MARKER WELL-FORMEDNESS — every bookmark / commentRange / permission
//     start/end half in the body carries a non-empty pairing id. Walks the same
//     inventory as the torn-range repair (`classify_range_pair_marker`,
//     `opaque_range_marker`). NARROWED (see below) from the task's "balance":
//     the count/pairing balance is a SERIALIZE-time invariant
//     (`enforce_story_bookmark_integrity`), not a model one — the model
//     legitimately carries transient unbalanced states.
//
//  3. TABLE STRUCTURAL COHERENCE — no row without a cell (CT_Row, §17.4.72);
//     a whole-row tracked op carries the row marker only, never a per-cell
//     `cellIns`/`cellDel` (`mark_whole_row_deleted`/`_inserted`); a cell's FINAL
//     paragraph mark is never tracked-deleted (`mark_cell_content_deleted`, the
//     W5-F7 poison marker).
//
//  4. MARK-SUPPRESSION CONSISTENCY — a `para_mark_status == Some(Normal)`
//     pilcrow suppression appears only on a shape that legitimately carries it:
//     a tracked block (block-`Inserted`/`Deleted`) or a move half. Minted only
//     by the tail normalizers (`normalize_inserted_final_mark`,
//     `normalize_moved_final_mark`, `renormalize_final_mark_after_selective`).
//
//  5. STACKED-STATE COHERENCE — `InsertedThenDeleted` is an inline/run state
//     (a `w:del` nested in a `w:ins` on runs); it is never constructed at BLOCK
//     level. Authoritative: `diff::project_tracked_document`'s
//     `unreachable!("block-level stacked status is never constructed")`.
//
// # Narrowings made during calibration
//
// Each of these was forced by running the full suite + fixture corpus through
// the validator and finding the initial (stronger) statement firing on a
// LEGITIMATE producer output. The narrowed statement is the strongest one that
// holds across every current legitimate producer.
//
// * **Invariant 2 is model WELL-FORMEDNESS, not count balance — the balance is
//   a serialize-time invariant.** No count-based statement holds at the model
//   level: `w:id` is not unique across bookmarks (a same-id, different-name
//   collision is deliberately kept — `redline_bookmark_identity::
//   t3_genuine_id_collision...`, which false-fired "≤1 each" at 2/2); a LONE
//   half is the input's own, deliberately preserved
//   (`collapse_resolution_torn_range_markers`: "a half already lone in the input
//   passes through untouched"); and the redline model carries a TRANSIENT
//   unbalanced state — an end marker riding both the deleted and the inserted
//   copy of an edited paragraph gives 1 start / 2 ends in the model
//   (`redline_bookmark_identity::t2_pair_spanning...`) — which the serializer
//   reconciles. The real balance ("every emitted bookmarkStart has exactly one
//   bookmarkEnd") is enforced on the SERIALIZED bytes by
//   `enforce_story_bookmark_integrity`, which diffs against the inputs to tell
//   an introduced tear from an inherited one — something a single-doc model
//   check cannot do. What DOES hold at the model level: no producer emits an
//   UNIDENTIFIED (empty-id) marker.
//
// * **Invariant 3(b) is DELETE-only.** A wholly-inserted row whose cells ALSO
//   carry a per-cell `w:cellIns` is a legitimate authoring shape (the
//   table-insert paths build it; rejecting the row removes it wholesale, so no
//   cell-less-row hazard). Only the DELETE case — a `w:cellDel` on a cell of a
//   wholly-deleted row — is the corruption `mark_whole_row_deleted` guards
//   against (selective reject strips the cell out of a surviving row).
//
// * **Invariant 1 (final-mark) is a POST-RESOLUTION property, not a universal
//   one.** A `w:del`/`w:ins` on the document-final pilcrow is VALID Word markup
//   that opens clean (`spec_para_mark_rpr_word_compliance`), and the merge/apply
//   mint sites legitimately leave one in the whole-document-insertion edge case
//   (`normalize_inserted_final_mark`'s early return when the whole body is
//   inserted). What the RESOLUTION projections guarantee is that they never
//   STRAND one. So invariant 1 lives in [`assert_resolution_body_invariants`]
//   (wired after `project` only), NOT in the universal [`assert_body_invariants`]
//   used by the other producers and the public surface. Reporting it as a public
//   `api::validate` issue would wrongly flag conformant documents.
//
// * **The universal invariants (2-5) are POST-TRANSFORM properties; `import` is
//   held only to the ones it cannot construct.** Import's contract is
//   byte-faithful representation of arbitrary — possibly non-conformant — input,
//   so a range-balance or table-coherence violation at import may be the INPUT's
//   own. The debug-assert wraps the transforming producers (apply_transaction,
//   project, merge_diff, convert_manual_markup); `import` uses the scoped
//   [`assert_import_body_invariants`] (mark-suppression + stacked-state, which
//   import never constructs a violation of), and the input-fidelity boundary is
//   otherwise covered by the public `api::validate` surface, which REPORTS
//   violations as issues rather than panicking.

/// The five body-state invariants this validator checks. Named so a violation
/// says which contract broke, not just where.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyInvariant {
    /// The document-final paragraph mark carries no pending tracked change.
    FinalMark,
    /// Range-marker (bookmark/comment/permission) halves are well-formed (each
    /// carries an identity). The count/pairing BALANCE is a serialize-time
    /// property, not a model one — see [`check_range_marker_wellformed`].
    RangeMarkerWellFormed,
    /// Table rows/cells satisfy the CT_Row / CT_Tc structural rules.
    TableStructuralCoherence,
    /// `para_mark_status == Some(Normal)` suppression only on legitimate shapes.
    MarkSuppressionConsistency,
    /// `InsertedThenDeleted` never appears at block level.
    StackedStateCoherence,
}

impl BodyInvariant {
    /// Stable kebab-case name used in violation messages.
    pub fn name(self) -> &'static str {
        match self {
            Self::FinalMark => "final-mark-rule",
            Self::RangeMarkerWellFormed => "range-marker-wellformed",
            Self::TableStructuralCoherence => "table-structural-coherence",
            Self::MarkSuppressionConsistency => "mark-suppression-consistency",
            Self::StackedStateCoherence => "stacked-state-coherence",
        }
    }
}

/// One body-state invariant violation: which invariant broke, the addressable
/// block id when there is one, and an actionable detail (what is wrong and the
/// spec/producer it points back to — CLAUDE.md "errors are actionable").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvariantViolation {
    pub invariant: BodyInvariant,
    pub block_id: Option<String>,
    pub detail: String,
}

impl std::fmt::Display for InvariantViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.block_id {
            Some(id) => write!(f, "[{}] block {id}: {}", self.invariant.name(), self.detail),
            None => write!(f, "[{}]: {}", self.invariant.name(), self.detail),
        }
    }
}

/// Run every body-state invariant over `doc`, collecting all violations.
///
/// This is the single unified validator (hardening H2). It is compiled in every
/// build: transforming producers call it through
/// [`debug_assert_body_invariants`] (a no-op in release), and the public
/// validation surface (`api::validate`) runs it explicitly to REPORT violations
/// as issues. It never mutates and never panics — the caller decides whether a
/// violation is fatal (a producer bug) or reportable (non-conformant input).
pub(crate) fn assert_body_invariants(doc: &CanonDoc) -> Result<(), Vec<InvariantViolation>> {
    let mut violations = Vec::new();
    check_range_marker_wellformed(&doc.blocks, &mut violations);
    check_table_coherence(&doc.blocks, &mut violations);
    check_mark_suppression(&doc.blocks, &mut violations);
    check_stacked_state(&doc.blocks, &mut violations);
    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

/// The post-RESOLUTION body-state contract: everything [`assert_body_invariants`]
/// checks, PLUS the final-mark rule (invariant 1).
///
/// The final-mark rule is deliberately NOT in the universal set: it is a
/// property the resolution projections must uphold, not one every producer does.
/// A `w:del`/`w:ins` on the document-final pilcrow is valid Word markup that
/// opens clean (`spec_para_mark_rpr_word_compliance`), and the merge/apply mint
/// sites legitimately leave one in the whole-document-insertion edge case (see
/// `normalize_inserted_final_mark`'s early return). What the RESOLUTION
/// projections guarantee is that they never STRAND one: accept/reject-all fully
/// resolve it, and `renormalize_final_mark_after_selective` re-establishes it
/// after a selective projection. So this fuller check is wired only after
/// `project`.
fn assert_resolution_body_invariants(doc: &CanonDoc) -> Result<(), Vec<InvariantViolation>> {
    let mut violations = Vec::new();
    check_final_mark(&doc.blocks, &mut violations);
    check_range_marker_wellformed(&doc.blocks, &mut violations);
    check_table_coherence(&doc.blocks, &mut violations);
    check_mark_suppression(&doc.blocks, &mut violations);
    check_stacked_state(&doc.blocks, &mut violations);
    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

/// Debug-build safety net wrapping a body-state producer: panics (naming the
/// producer and every violation) if the just-produced `doc` breaks an
/// invariant. Compiles to nothing in release (`debug_assertions` gate), so it
/// costs a shipped build zero — the release check is the explicit
/// `api::validate` surface.
#[cfg(debug_assertions)]
pub(crate) fn debug_assert_body_invariants(doc: &CanonDoc, producer: &str) {
    if let Err(violations) = assert_body_invariants(doc) {
        let rendered = violations
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join("\n  ");
        panic!("body-state invariant violation(s) after {producer}:\n  {rendered}");
    }
}

#[cfg(not(debug_assertions))]
#[inline(always)]
pub(crate) fn debug_assert_body_invariants(_doc: &CanonDoc, _producer: &str) {}

/// Resolution-scoped twin of [`debug_assert_body_invariants`]: the universal set
/// PLUS the final-mark rule (see [`assert_resolution_body_invariants`]). Wired
/// after `project`, where the accept/reject/selective projections must never
/// strand a tracked final mark.
#[cfg(debug_assertions)]
pub(crate) fn debug_assert_resolution_body_invariants(doc: &CanonDoc, producer: &str) {
    if let Err(violations) = assert_resolution_body_invariants(doc) {
        let rendered = violations
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join("\n  ");
        panic!("body-state invariant violation(s) after {producer}:\n  {rendered}");
    }
}

#[cfg(not(debug_assertions))]
#[inline(always)]
pub(crate) fn debug_assert_resolution_body_invariants(_doc: &CanonDoc, _producer: &str) {}

/// The subset of the body-state invariants that IMPORT is held to.
///
/// Import's contract is byte-faithful representation of arbitrary — possibly
/// non-conformant — input, so a violation of the final-mark rule (1),
/// range-marker balance (2) or table coherence (3) at import may be the INPUT's
/// own defect, not an engine bug (those are established/normalized by the
/// mint-time producers downstream, and reported honestly by `api::validate`).
/// The two invariants below are different: import NEVER constructs a
/// `Some(Normal)` pilcrow suppression or a block-level `InsertedThenDeleted`, so
/// their appearance after import IS an import bug — worth a debug panic.
fn assert_import_body_invariants(doc: &CanonDoc) -> Result<(), Vec<InvariantViolation>> {
    let mut violations = Vec::new();
    check_mark_suppression(&doc.blocks, &mut violations);
    check_stacked_state(&doc.blocks, &mut violations);
    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

/// Import-scoped twin of [`debug_assert_body_invariants`]: checks only the
/// invariants import cannot legitimately construct a violation of (see
/// [`assert_import_body_invariants`]).
#[cfg(debug_assertions)]
pub(crate) fn debug_assert_import_body_invariants(doc: &CanonDoc, producer: &str) {
    if let Err(violations) = assert_import_body_invariants(doc) {
        let rendered = violations
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join("\n  ");
        panic!("body-state invariant violation(s) after {producer}:\n  {rendered}");
    }
}

#[cfg(not(debug_assertions))]
#[inline(always)]
pub(crate) fn debug_assert_import_body_invariants(_doc: &CanonDoc, _producer: &str) {}

/// A short human word for a tracking status, for violation messages.
fn tracking_status_word(status: &TrackingStatus) -> &'static str {
    match status {
        TrackingStatus::Normal => "normal",
        TrackingStatus::Inserted(_) => "insertion",
        TrackingStatus::Deleted(_) => "deletion",
        TrackingStatus::InsertedThenDeleted(_) => "inserted-then-deleted",
    }
}

/// Invariant 1: the document-final paragraph mark carries no pending tracked
/// change. Mirrors `renormalize_final_mark_after_selective`'s forbidden state
/// (`effective` mark is `Inserted`/`Deleted`) and `normalize_final_mark_
/// attribution`'s single exemption (a document-final moveFrom shadow).
fn check_final_mark(blocks: &[TrackedBlock], out: &mut Vec<InvariantViolation>) {
    let Some(last) = blocks.len().checked_sub(1) else {
        return;
    };
    let tb = &blocks[last];
    let BlockNode::Paragraph(p) = &tb.block else {
        // A body always ends in a paragraph; a trailing table/opaque is a
        // separate structural concern, not the final-mark rule.
        return;
    };
    // What serialize emits for the final pilcrow: the paragraph's own
    // para_mark_status, else the block-level status.
    let effective = p
        .para_mark_status
        .clone()
        .unwrap_or_else(|| tb.status.clone());
    if !matches!(
        effective,
        TrackingStatus::Inserted(_) | TrackingStatus::Deleted(_)
    ) {
        return;
    }
    // Exemption: a moveFrom SHADOW (block-level Deleted + move_id) ending the
    // document is resolved by its own moveFromRange pairing (see
    // `is_move_from_shadow` / `normalize_final_mark_attribution`).
    if matches!(tb.status, TrackingStatus::Deleted(_)) && tb.move_id.is_some() {
        return;
    }
    out.push(InvariantViolation {
        invariant: BodyInvariant::FinalMark,
        block_id: Some(p.id.0.to_string()),
        detail: format!(
            "document-final paragraph mark carries a pending {}; Word cannot resolve a tracked \
             change on the final pilcrow (accept-all leaves it pending forever). The mint-time \
             normalizers must re-attribute it (see normalize_final_mark_attribution / \
             renormalize_final_mark_after_selective).",
            tracking_status_word(&effective),
        ),
    });
}

/// Invariant 2: every range-marker half in the body model is IDENTIFIED — it
/// carries a non-empty pairing id. Walks the same inventory as the torn-range
/// repair (inline decorations via `classify_range_pair_marker`, typed
/// comment-range inlines, and body-level opaque marker blocks via
/// `opaque_range_marker`).
///
/// NARROWED, during calibration, from the count/pairing "balance" the task named
/// down to model-level well-formedness — because the count balance is a
/// SERIALIZE-TIME invariant, not a model one, and every count-based statement
/// false-fires on a legitimate model:
///  * `w:id` is not unique across bookmarks — two bookmarks may share an id with
///    different names, a collision the engine deliberately keeps
///    (`redline_bookmark_identity::t3_genuine_id_collision...`).
///  * a LONE half is the input's own state, deliberately preserved
///    (`collapse_resolution_torn_range_markers`: "a half already lone in the
///    input passes through untouched").
///  * the redline model legitimately carries a TRANSIENT unbalanced state — an
///    end marker riding BOTH the deleted and the inserted copy of an edited
///    paragraph (`redline_bookmark_identity::t2_pair_spanning_unchanged_and_
///    edited_paragraph_stays_paired` produces 1 start / 2 ends in the model),
///    which the serializer reconciles.
///
/// So there is no model-level count invariant to enforce. The actual pairing
/// BALANCE — "every emitted bookmarkStart has exactly one bookmarkEnd" — is
/// checked on the SERIALIZED bytes by `enforce_story_bookmark_integrity` (which
/// can, and this validator cannot, distinguish an introduced tear from an
/// inherited one by diffing against the inputs). What holds at the MODEL level,
/// and is what this checks, is that no producer emits an UNIDENTIFIED marker: a
/// bookmark/comment/permission range half with an empty id is malformed
/// (§17.13.6 — the id is the pairing key) and no legitimate producer emits one.
fn check_range_marker_wellformed(blocks: &[TrackedBlock], out: &mut Vec<InvariantViolation>) {
    fn check_id(
        family: RangePairFamily,
        id: &str,
        role: RangeRole,
        out: &mut Vec<InvariantViolation>,
    ) {
        if id.trim().is_empty() {
            out.push(InvariantViolation {
                invariant: BodyInvariant::RangeMarkerWellFormed,
                block_id: None,
                detail: format!(
                    "{family:?} range-marker {role:?} half carries an empty pairing id; a \
                     bookmark/comment/permission range half must be identified (the id is its \
                     pairing key, §17.13.6)."
                ),
            });
        }
    }
    fn visit(block: &BlockNode, out: &mut Vec<InvariantViolation>) {
        match block {
            BlockNode::Paragraph(p) => {
                for seg in &p.segments {
                    for inline in &seg.inlines {
                        if let Some((family, id, role)) = classify_range_pair_marker(inline) {
                            check_id(family, &id, role, out);
                        }
                    }
                }
            }
            BlockNode::Table(t) => {
                for row in &t.rows {
                    for cell in &row.cells {
                        for tb in &cell.blocks {
                            visit(tb, out);
                        }
                    }
                }
            }
            BlockNode::OpaqueBlock(o) => {
                if let Some((family, id, role)) = opaque_range_marker(o) {
                    check_id(family, &id, role, out);
                }
            }
        }
    }
    for tb in blocks {
        visit(&tb.block, out);
    }
}

/// Invariant 3: table structural coherence, over every table (recursing into
/// nested cell tables).
fn check_table_coherence(blocks: &[TrackedBlock], out: &mut Vec<InvariantViolation>) {
    fn visit(block: &BlockNode, out: &mut Vec<InvariantViolation>) {
        let BlockNode::Table(t) = block else {
            return;
        };
        for row in &t.rows {
            // (a) CT_Row requires at least one w:tc (§17.4.72).
            if row.cells.is_empty() {
                out.push(InvariantViolation {
                    invariant: BodyInvariant::TableStructuralCoherence,
                    block_id: Some(row.id.0.to_string()),
                    detail: "table row has no cells; CT_Row requires at least one w:tc \
                             (§17.4.72) — a cell-less <w:tr> is rejected by Word."
                        .to_string(),
                });
            }
            // (b) A wholly-DELETED row uses the row-level w:trPr/w:del marker
            // only; its cells must NOT carry a per-cell w:cellDel
            // (`mark_whole_row_deleted`). The specific hazard: selectively
            // rejecting one cell's cellDel WITHOUT the row marker strips that
            // cell out of a surviving row, and the serializer refuses the
            // resulting cell-less <w:tr> (CT_Row, §17.4.72).
            //
            // NARROWED to the DELETE case: a wholly-INSERTED row whose cells
            // ALSO carry a per-cell w:cellIns is a legitimate authoring shape
            // (the table-authoring/insert paths build it, and rejecting the row
            // removes it wholesale — no cell-less-row hazard), so the insert
            // side is deliberately not flagged.
            if matches!(row.tracking_status, Some(TrackingStatus::Deleted(_))) {
                for cell in &row.cells {
                    if matches!(cell.tracking_status, Some(TrackingStatus::Deleted(_))) {
                        out.push(InvariantViolation {
                            invariant: BodyInvariant::TableStructuralCoherence,
                            block_id: Some(cell.id.0.to_string()),
                            detail: "cell carries a per-cell w:cellDel inside a wholly-deleted \
                                     row; a whole-row deletion uses the row marker only and leaves \
                                     cells markerless (selectively rejecting one cell's cellDel \
                                     would strip it from a surviving row, yielding a cell-less \
                                     <w:tr>). See mark_whole_row_deleted."
                                .to_string(),
                        });
                    }
                }
            }
            // (c) RETIRED as a document-STATE invariant (oracle-verified): a
            // tracked-DELETED final cell paragraph mark is a legal PENDING
            // state in the wild — automated Word pipelines author it when
            // tracked-deleting whole cell contents, and desktop Word opens
            // such documents valid and unrepaired, clears the cell content on
            // accept (retaining the structural final paragraph, CT_Tc
            // §17.4.66) and restores it on reject; the engine's projections
            // match (spec_wild_tolerated_markup). The rule survives where it
            // is true: the engine's OWN producers never author the state —
            // that is mark_cell_content_deleted's contract, pinned by its
            // spec tests (W5-F7 / accept-equivalence suite). Do NOT
            // re-strengthen this into the validator: it condemned real
            // redlined legal documents at the door.

            // Recurse into nested tables held in each cell.
            for cell in &row.cells {
                for b in &cell.blocks {
                    visit(b, out);
                }
            }
        }
    }
    for tb in blocks {
        visit(&tb.block, out);
    }
}

/// Invariant 4: a `para_mark_status == Some(Normal)` pilcrow suppression is
/// legitimate only on a tracked block (block-`Inserted`/`Deleted`) or a move
/// half — the shapes the tail normalizers mint it on. On a plain Normal,
/// non-move block it is a meaningless (and divergence-prone) suppression.
fn check_mark_suppression(blocks: &[TrackedBlock], out: &mut Vec<InvariantViolation>) {
    for tb in blocks {
        let BlockNode::Paragraph(p) = &tb.block else {
            continue;
        };
        if !matches!(p.para_mark_status, Some(TrackingStatus::Normal)) {
            continue;
        }
        let block_tracked = matches!(
            tb.status,
            TrackingStatus::Inserted(_)
                | TrackingStatus::Deleted(_)
                | TrackingStatus::InsertedThenDeleted(_)
        );
        if !block_tracked && tb.move_id.is_none() {
            out.push(InvariantViolation {
                invariant: BodyInvariant::MarkSuppressionConsistency,
                block_id: Some(p.id.0.to_string()),
                detail: "paragraph mark is suppressed (para_mark_status = Some(Normal)) on a \
                         non-tracked, non-move block; the suppression is only minted on \
                         block-Inserted/Deleted paragraphs or move halves (the tail \
                         normalizers)."
                    .to_string(),
            });
        }
    }
}

/// Invariant 5: `InsertedThenDeleted` is inline/run-only and is never
/// constructed at block level (see `diff::project_tracked_document`'s
/// `unreachable!`).
fn check_stacked_state(blocks: &[TrackedBlock], out: &mut Vec<InvariantViolation>) {
    for tb in blocks {
        if matches!(tb.status, TrackingStatus::InsertedThenDeleted(_)) {
            out.push(InvariantViolation {
                invariant: BodyInvariant::StackedStateCoherence,
                block_id: Some(block_node_id(&tb.block).0.to_string()),
                detail: "block-level TrackingStatus::InsertedThenDeleted; the stacked \
                         insert-then-delete state is inline/run-only (a w:del nested in a w:ins \
                         on runs) and is never constructed at block level."
                    .to_string(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::domain::{
        CompatSettings, DecorationNode, DecorationType, DocFingerprint, DocMeta, DocPart,
        DocumentDiff, FieldData, FieldKind, HyperlinkData, HyperlinkRun, INTERNAL_IDS_VERSION_V0,
        IStr, Mark, MaterializedPrefixKind, NumberingInfo, OpaqueInlineNode, OpaqueKind, ProofRef,
        RevisionInfo, SCHEMA_VERSION_V0, StackedRevision, StyleProps, TextNode, TextRole,
        TrackingStatus, is_materialized_prefix_text, materialized_prefix_node_id, normal_segment,
        normal_tracked_block,
    };
    use crate::{DocxRuntime, SimpleRuntime};

    /// §17.13.5.37: rejecting a `w:tcPrChange` must restore the prior cell
    /// properties EXACTLY — including no-wrap, text-direction, and fit-text,
    /// which the typed reject previously captured but never restored (audit
    /// #13). The exhaustive destructure in `reject_cell_formatting` now makes
    /// dropping any `previous_*` field a compile error.
    #[test]
    fn reject_cell_formatting_restores_no_wrap_text_direction_and_fit_text() {
        use crate::domain::{
            CellFormatting, CellFormattingChange, TableCellNode, TextDirection, VerticalMerge,
        };
        let mut cell = TableCellNode {
            id: NodeId::from("c1"),
            blocks: vec![],
            grid_span: 1,
            v_merge: VerticalMerge::None,
            // Current ("new") cell formatting.
            formatting: CellFormatting {
                no_wrap: Some(false),
                text_direction: Some(TextDirection::LrTb),
                tc_fit_text: Some(false),
                ..Default::default()
            },
            formatting_change: Some(CellFormattingChange {
                previous_width: None,
                previous_borders: None,
                previous_shading: None,
                previous_v_align: None,
                previous_margins: None,
                previous_no_wrap: Some(true),
                previous_text_direction: Some(TextDirection::TbRl),
                previous_tc_fit_text: Some(true),
                revision_id: 7,
                identity: 7,
                author: "A".to_string(),
                date: None,
            }),
            tracking_status: None,
            row_sdt_wrapper: None,
            content_sdt_wraps: Vec::new(),
            cnf_style: None,
            hide_mark: false,
            preserved: Vec::new(),
        };
        reject_cell_formatting(&mut cell);
        assert_eq!(
            cell.formatting.no_wrap,
            Some(true),
            "reject must restore the previous no_wrap (§17.13.5.37)"
        );
        assert_eq!(
            cell.formatting.text_direction,
            Some(TextDirection::TbRl),
            "reject must restore the previous text_direction"
        );
        assert_eq!(
            cell.formatting.tc_fit_text,
            Some(true),
            "reject must restore the previous tc_fit_text"
        );
        assert!(
            cell.formatting_change.is_none(),
            "the formatting change is consumed on reject"
        );
    }

    /// §17.13.5.29: rejecting a `w:pPrChange` must restore the prior paragraph
    /// state EXACTLY — including the paragraph-mark formatting and the literal-
    /// prefix tab geometry, which the typed reject previously captured but never
    /// restored (audit #17).
    #[test]
    fn reject_paragraph_formatting_restores_para_mark_and_literal_prefix_tabs() {
        use crate::domain::ParagraphFormattingChange;
        let BlockNode::Paragraph(mut p) = make_paragraph("p1", "x") else {
            panic!("make_paragraph returns a paragraph");
        };
        // Current ("new") state: no para-mark marks, no prefix tabs.
        p.paragraph_mark_marks = vec![];
        p.literal_prefix_leading_tab_twips = None;
        p.literal_prefix_trailing_tab_stop_twips = None;
        p.formatting_change = Some(ParagraphFormattingChange {
            previous_alignment: None,
            previous_indentation: None,
            previous_spacing: None,
            previous_numbering: None,
            previous_numbering_explicitly_absent: false,
            previous_style_id: None,
            previous_keep_next: None,
            previous_keep_lines: None,
            previous_page_break_before: false,
            previous_widow_control: None,
            previous_contextual_spacing: None,
            previous_shading: None,
            previous_borders: None,
            previous_tab_stops: vec![],
            previous_literal_prefix_leading_tab_twips: Some(720),
            previous_literal_prefix_trailing_tab_stop_twips: Some(1440),
            previous_paragraph_mark_marks: vec![Mark::Bold],
            previous_paragraph_mark_style_props: StyleProps::default(),
            previous_paragraph_mark_rpr_off: Default::default(),
            previous_text_direction: None,
            previous_text_alignment: None,
            previous_mirror_indents: None,
            previous_auto_space_de: None,
            previous_auto_space_dn: None,
            previous_bidi: None,
            previous_suppress_auto_hyphens: None,
            previous_snap_to_grid: None,
            previous_overflow_punct: None,
            previous_adjust_right_ind: None,
            previous_word_wrap: None,
            previous_frame_pr: None,
            previous_preserved_ppr: vec![],
            revision_id: 3,
            identity: 3,
            author: "A".to_string(),
            date: None,
        });
        reject_paragraph_formatting(&mut p);
        assert_eq!(
            p.paragraph_mark_marks,
            vec![Mark::Bold],
            "reject must restore the previous paragraph-mark marks"
        );
        assert_eq!(
            p.literal_prefix_leading_tab_twips,
            Some(720),
            "reject must restore the previous literal-prefix leading tab geometry"
        );
        assert_eq!(
            p.literal_prefix_trailing_tab_stop_twips,
            Some(1440),
            "reject must restore the previous literal-prefix trailing tab geometry"
        );
    }

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
                id: NodeId::from(format!("{id}_t1")),
                text_role: None,
                text: text.to_string(),
                marks: Vec::new(),
                style_props: StyleProps::default(),
                rpr_authored: RunRprAuthored::default(),
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
        })
    }

    fn make_field_inline(
        id: &str,
        field_kind: FieldKind,
        instruction_text: Option<&str>,
    ) -> InlineNode {
        InlineNode::from(OpaqueInlineNode {
            id: NodeId::from(id.to_string()),
            kind: OpaqueKind::Field(FieldData {
                field_kind: field_kind.clone(),
                instruction_text: instruction_text.map(|s| s.to_string()),
                result_text: None,
                semantic: None,
            }),
            opaque_ref: id.to_string(),
            proof_ref: ProofRef {
                part: DocPart::DocumentXml,
                block_id: NodeId::from(id.to_string()),
                docx_anchor: id.to_string(),
            },
            wrapper_marks: Vec::new(),
            wrapper_style_props: StyleProps::default(),
            raw_xml: None,
            content_hash: None,
        })
    }

    fn make_text_inline(id: &str, text: &str) -> InlineNode {
        InlineNode::from(TextNode {
            id: NodeId::from(id.to_string()),
            text_role: None,
            text: text.to_string(),
            marks: Vec::new(),
            style_props: StyleProps::default(),
            rpr_authored: RunRprAuthored::default(),
            formatting_change: None,
        })
    }

    /// A hyperlink opaque carrying the given runs verbatim — the shape
    /// `rewrite_hyperlink_runs` (`EditStep::ReplaceHyperlinkText`) produces:
    /// the enclosing segment stays Normal, and per-run `TrackingStatus`
    /// carries the tracked edit (see the layering invariant on
    /// `HyperlinkData`).
    fn make_hyperlink_inline(id: &str, url: &str, runs: Vec<HyperlinkRun>) -> InlineNode {
        let text = runs.iter().map(|r| r.text.as_str()).collect();
        InlineNode::from(OpaqueInlineNode {
            id: NodeId::from(id.to_string()),
            kind: OpaqueKind::Hyperlink(HyperlinkData {
                url: Some(url.to_string()),
                anchor: None,
                text,
                r_id: Some("rId1".to_string()),
                runs,
                extra_attrs: Vec::new(),
            }),
            opaque_ref: id.to_string(),
            proof_ref: ProofRef {
                part: DocPart::DocumentXml,
                block_id: NodeId::from(id.to_string()),
                docx_anchor: id.to_string(),
            },
            wrapper_marks: Vec::new(),
            wrapper_style_props: StyleProps::default(),
            raw_xml: None,
            content_hash: None,
        })
    }

    fn make_simple_field_inline(id: &str, instruction_text: &str, result_text: &str) -> InlineNode {
        InlineNode::from(OpaqueInlineNode {
            id: NodeId::from(id.to_string()),
            kind: OpaqueKind::Field(FieldData {
                field_kind: FieldKind::Simple,
                instruction_text: Some(instruction_text.to_string()),
                result_text: Some(result_text.to_string()),
                semantic: None,
            }),
            opaque_ref: id.to_string(),
            proof_ref: ProofRef {
                part: DocPart::DocumentXml,
                block_id: NodeId::from(id.to_string()),
                docx_anchor: id.to_string(),
            },
            wrapper_marks: Vec::new(),
            wrapper_style_props: StyleProps::default(),
            raw_xml: None,
            content_hash: Some(format!("field:{instruction_text}:{result_text}")),
        })
    }

    fn segment_debug(segments: &[TrackedSegment]) -> Vec<String> {
        segments
            .iter()
            .map(|segment| {
                let status = match segment.status {
                    TrackingStatus::Normal => "N",
                    TrackingStatus::Deleted(_) => "D",
                    TrackingStatus::Inserted(_) => "I",
                    TrackingStatus::InsertedThenDeleted(_) => "S",
                };
                let body = segment
                    .inlines
                    .iter()
                    .map(|inline| match inline {
                        InlineNode::Text(text) => format!("text:{:?}", text.text),
                        InlineNode::OpaqueInline(opaque) => match &opaque.kind {
                            OpaqueKind::Field(data) if data.field_kind == FieldKind::Simple => {
                                format!(
                                    "fldSimple:{:?}:{:?}",
                                    data.instruction_text, data.result_text
                                )
                            }
                            OpaqueKind::Hyperlink(_) => "hyperlink".to_string(),
                            OpaqueKind::OmmlBlock => "omml-block".to_string(),
                            _ => "opaque".to_string(),
                        },
                        _ => "other".to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join("|");
                format!("{status}:{body}")
            })
            .collect()
    }

    #[test]
    fn normalize_paragraph_opaque_centered_window_reorders_to_reading_order() {
        let deleted = TrackingStatus::Deleted(RevisionInfo {
            revision_id: 1,
            identity: 1,
            author: Some("tester".to_string()),
            date: None,
            apply_op_id: None,
        });
        let inserted = TrackingStatus::Inserted(RevisionInfo {
            revision_id: 2,
            identity: 2,
            author: Some("tester".to_string()),
            date: None,
            apply_op_id: None,
        });

        let segments = vec![
            TrackedSegment {
                status: deleted.clone(),
                inlines: vec![make_text_inline("d1", "Contract dated ")],
            },
            TrackedSegment {
                status: TrackingStatus::Normal,
                inlines: vec![make_simple_field_inline(
                    "field",
                    " DATE ",
                    "January 15, 2025",
                )],
            },
            TrackedSegment {
                status: deleted,
                inlines: vec![make_text_inline("d2", " is hereby executed")],
            },
            TrackedSegment {
                status: inserted.clone(),
                inlines: vec![make_text_inline("i1", "Agreement effective ")],
            },
            TrackedSegment {
                status: inserted,
                inlines: vec![make_text_inline("i2", " is now in force")],
            },
        ];

        let normalized = normalize_paragraph_opaque_reading_order(segments);
        assert_eq!(
            segment_debug(&normalized),
            vec![
                "D:text:\"Contract dated \"".to_string(),
                "I:text:\"Agreement effective \"".to_string(),
                "N:fldSimple:Some(\" DATE \"):Some(\"January 15, 2025\")".to_string(),
                "D:text:\" is hereby executed\"".to_string(),
                "I:text:\" is now in force\"".to_string(),
            ]
        );
    }

    #[test]
    fn normalize_paragraph_opaque_mirrored_window_collapses_to_single_normal_opaque() {
        let deleted = TrackingStatus::Deleted(RevisionInfo {
            revision_id: 1,
            identity: 1,
            author: Some("tester".to_string()),
            date: None,
            apply_op_id: None,
        });
        let inserted = TrackingStatus::Inserted(RevisionInfo {
            revision_id: 2,
            identity: 2,
            author: Some("tester".to_string()),
            date: None,
            apply_op_id: None,
        });

        let segments = vec![
            TrackedSegment {
                status: deleted.clone(),
                inlines: vec![make_text_inline("d1", "Contract dated ")],
            },
            TrackedSegment {
                status: deleted.clone(),
                inlines: vec![make_simple_field_inline(
                    "field-del",
                    " DATE ",
                    "January 15, 2025",
                )],
            },
            TrackedSegment {
                status: deleted,
                inlines: vec![make_text_inline("d2", " is hereby executed")],
            },
            TrackedSegment {
                status: inserted.clone(),
                inlines: vec![make_text_inline("i1", "Agreement effective ")],
            },
            TrackedSegment {
                status: inserted.clone(),
                inlines: vec![make_simple_field_inline(
                    "field-ins",
                    " DATE ",
                    "January 15, 2025",
                )],
            },
            TrackedSegment {
                status: inserted,
                inlines: vec![make_text_inline("i2", " is now in force")],
            },
        ];

        let normalized = normalize_paragraph_opaque_reading_order(segments);
        assert_eq!(
            segment_debug(&normalized),
            vec![
                "D:text:\"Contract dated \"".to_string(),
                "I:text:\"Agreement effective \"".to_string(),
                "N:fldSimple:Some(\" DATE \"):Some(\"January 15, 2025\")".to_string(),
                "D:text:\" is hereby executed\"".to_string(),
                "I:text:\" is now in force\"".to_string(),
            ]
        );
    }

    /// Wrap a `<w:body>` inner fragment in a minimal, self-contained OPC
    /// package. Hermetic: builds the bytes in-process so the daily gate never
    /// depends on the gitignored corpus. Must NOT embed corpus content.
    fn build_minimal_docx(body_inner: &str) -> Vec<u8> {
        use std::io::Write;
        use zip::write::FileOptions;

        let document_xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_inner}<w:sectPr/></w:body></w:document>"#
        );
        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
        let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
        let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;

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
            zip.write_all(document_xml.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        buf
    }

    #[test]
    fn opaque_roundtrip_field_segments_are_canonical_in_merged_model() {
        // Synthetic single-field fixture, built in-process so the daily gate is
        // genuinely corpus-free. The before/after paragraphs share an identical
        // `fldSimple DATE` opaque inline, surrounded by text that is fully
        // replaced. The diff/merge pipeline must keep the shared field as a
        // single Normal segment, delete the old surrounding text, and insert the
        // new surrounding text — the canonical 5-segment field window.
        let field =
            r#"<w:fldSimple w:instr=" DATE "><w:r><w:t>January 15, 2025</w:t></w:r></w:fldSimple>"#;
        let before_body = format!(
            r#"<w:p><w:r><w:t xml:space="preserve">Contract dated </w:t></w:r>{field}<w:r><w:t xml:space="preserve"> is hereby executed</w:t></w:r></w:p>"#
        );
        let after_body = format!(
            r#"<w:p><w:r><w:t xml:space="preserve">Agreement effective </w:t></w:r>{field}<w:r><w:t xml:space="preserve"> is now in force</w:t></w:r></w:p>"#
        );
        let before = build_minimal_docx(&before_body);
        let after = build_minimal_docx(&after_body);
        let runtime = SimpleRuntime::new();
        let before_import = runtime.import_docx(&before).expect("import before");
        let after_import = runtime.import_docx(&after).expect("import after");

        let diff = crate::diff::diff_documents(&before_import.canonical, &after_import.canonical)
            .expect("diff documents");
        let revision = RevisionInfo {
            revision_id: 1,
            identity: 1,
            author: Some("tester".to_string()),
            date: None,
            apply_op_id: None,
        };
        let merged = merge_diff(
            &before_import.canonical,
            &after_import.canonical,
            &diff,
            &revision,
        )
        .expect("merge diff")
        .doc;

        let paragraph = merged
            .blocks
            .iter()
            .find_map(|tracked| match &tracked.block {
                BlockNode::Paragraph(paragraph)
                    if paragraph
                        .segments
                        .iter()
                        .flat_map(|segment| segment.inlines.iter())
                        .any(|inline| {
                            matches!(
                                inline,
                                InlineNode::OpaqueInline(o)
                                    if matches!(
                                        o.kind,
                                        OpaqueKind::Field(FieldData {
                                            field_kind: FieldKind::Simple,
                                            ..
                                        })
                                    )
                            )
                        }) =>
                {
                    Some(paragraph)
                }
                _ => None,
            })
            .expect("merged fldSimple paragraph");

        let summary = segment_debug(&paragraph.segments);
        // Canonical 5-segment field window:
        //   0: deleted divergent leading text
        //   1: inserted divergent leading text
        //   2: the shared `fldSimple` stays Normal, carrying the common
        //      surrounding whitespace the word-diff matched on both sides
        //   3: deleted divergent trailing text
        //   4: inserted divergent trailing text
        // The load-bearing invariant is that the opaque field is preserved as a
        // single Normal segment and is never split across delete/insert sides.
        assert_eq!(
            summary.len(),
            5,
            "expected canonical 5-segment field window: {summary:?}"
        );
        assert_eq!(summary[0], "D:text:\"Contract dated\"");
        assert_eq!(summary[1], "I:text:\"Agreement effective\"");
        assert!(
            summary[2].starts_with("N:"),
            "third segment should be Normal (shared content): {summary:?}"
        );
        assert!(
            summary[2].contains("fldSimple:Some(\" DATE \"):Some(\"January 15, 2025\")"),
            "third segment must carry the shared normal fldSimple intact: {summary:?}"
        );
        assert!(
            !summary[0].contains("fldSimple")
                && !summary[1].contains("fldSimple")
                && !summary[3].contains("fldSimple")
                && !summary[4].contains("fldSimple"),
            "the opaque field must never leak onto a delete/insert segment: {summary:?}"
        );
        assert_eq!(summary[3], "D:text:\"hereby executed\"");
        assert_eq!(summary[4], "I:text:\"now in force\"");
    }

    #[test]
    fn coalesce_auto_page_field_normalizes_structural_and_result_tracking() {
        let deleted = TrackingStatus::Deleted(RevisionInfo {
            revision_id: 1,
            identity: 1,
            author: Some("tester".to_string()),
            date: None,
            apply_op_id: None,
        });
        let inserted = TrackingStatus::Inserted(RevisionInfo {
            revision_id: 2,
            identity: 2,
            author: Some("tester".to_string()),
            date: None,
            apply_op_id: None,
        });

        let segments = vec![
            TrackedSegment {
                status: TrackingStatus::Normal,
                inlines: vec![
                    make_field_inline("begin", FieldKind::Begin, None),
                    make_field_inline("instr", FieldKind::Instruction, Some(" PAGE ")),
                ],
            },
            TrackedSegment {
                status: deleted.clone(),
                inlines: vec![
                    make_field_inline("sep-del", FieldKind::Separate, None),
                    make_text_inline("result-del", "2"),
                ],
            },
            TrackedSegment {
                status: inserted.clone(),
                inlines: vec![
                    make_field_inline("sep-ins", FieldKind::Separate, None),
                    make_text_inline("result-ins", "6"),
                ],
            },
            TrackedSegment {
                status: TrackingStatus::Normal,
                inlines: vec![make_field_inline("end", FieldKind::End, None)],
            },
        ];

        let repaired = coalesce_split_field_sequences(segments);
        let flat: Vec<(String, &TrackingStatus)> = repaired
            .iter()
            .flat_map(|segment| {
                segment.inlines.iter().map(move |inline| {
                    let label = match inline {
                        InlineNode::Text(text) => text.text.clone(),
                        InlineNode::OpaqueInline(opaque) => match &opaque.kind {
                            OpaqueKind::Field(data) => format!("field:{:?}", data.field_kind),
                            _ => "opaque".to_string(),
                        },
                        _ => "other".to_string(),
                    };
                    (label, &segment.status)
                })
            })
            .collect();

        assert_eq!(
            flat,
            vec![
                ("field:Begin".to_string(), &TrackingStatus::Normal),
                ("field:Instruction".to_string(), &TrackingStatus::Normal),
                ("field:Separate".to_string(), &TrackingStatus::Normal),
                ("6".to_string(), &TrackingStatus::Normal),
                ("field:End".to_string(), &TrackingStatus::Normal),
            ]
        );
    }

    #[test]
    fn coalesce_auto_page_field_normalizes_tracked_result_with_stable_structure() {
        let deleted = TrackingStatus::Deleted(RevisionInfo {
            revision_id: 1,
            identity: 1,
            author: Some("tester".to_string()),
            date: None,
            apply_op_id: None,
        });
        let inserted = TrackingStatus::Inserted(RevisionInfo {
            revision_id: 2,
            identity: 2,
            author: Some("tester".to_string()),
            date: None,
            apply_op_id: None,
        });

        let segments = vec![
            TrackedSegment {
                status: TrackingStatus::Normal,
                inlines: vec![
                    make_field_inline("begin", FieldKind::Begin, None),
                    make_field_inline("instr", FieldKind::Instruction, Some(" PAGE ")),
                    make_field_inline("sep", FieldKind::Separate, None),
                ],
            },
            TrackedSegment {
                status: deleted,
                inlines: vec![make_text_inline("result-del", "2")],
            },
            TrackedSegment {
                status: inserted,
                inlines: vec![make_text_inline("result-ins", "6")],
            },
            TrackedSegment {
                status: TrackingStatus::Normal,
                inlines: vec![make_field_inline("end", FieldKind::End, None)],
            },
        ];

        let repaired = coalesce_split_field_sequences(segments);
        let flat: Vec<(String, &TrackingStatus)> = repaired
            .iter()
            .flat_map(|segment| {
                segment.inlines.iter().map(move |inline| {
                    let label = match inline {
                        InlineNode::Text(text) => text.text.clone(),
                        InlineNode::OpaqueInline(opaque) => match &opaque.kind {
                            OpaqueKind::Field(data) => format!("field:{:?}", data.field_kind),
                            _ => "opaque".to_string(),
                        },
                        _ => "other".to_string(),
                    };
                    (label, &segment.status)
                })
            })
            .collect();

        assert_eq!(
            flat,
            vec![
                ("field:Begin".to_string(), &TrackingStatus::Normal),
                ("field:Instruction".to_string(), &TrackingStatus::Normal),
                ("field:Separate".to_string(), &TrackingStatus::Normal),
                ("6".to_string(), &TrackingStatus::Normal),
                ("field:End".to_string(), &TrackingStatus::Normal),
            ]
        );
    }

    #[test]
    fn coalesce_auto_page_field_removes_orphaned_deleted_begin() {
        let deleted = TrackingStatus::Deleted(RevisionInfo {
            revision_id: 1,
            identity: 1,
            author: Some("tester".to_string()),
            date: None,
            apply_op_id: None,
        });
        let inserted = TrackingStatus::Inserted(RevisionInfo {
            revision_id: 2,
            identity: 2,
            author: Some("tester".to_string()),
            date: None,
            apply_op_id: None,
        });

        let segments = vec![
            TrackedSegment {
                status: deleted,
                inlines: vec![make_field_inline("begin-del", FieldKind::Begin, None)],
            },
            TrackedSegment {
                status: TrackingStatus::Normal,
                inlines: vec![
                    make_field_inline("begin", FieldKind::Begin, None),
                    make_field_inline("instr", FieldKind::Instruction, Some(" PAGE ")),
                    make_field_inline("sep", FieldKind::Separate, None),
                ],
            },
            TrackedSegment {
                status: inserted,
                inlines: vec![make_text_inline("result-ins", "6")],
            },
            TrackedSegment {
                status: TrackingStatus::Normal,
                inlines: vec![make_field_inline("end", FieldKind::End, None)],
            },
        ];

        let repaired = coalesce_split_field_sequences(segments);
        let flat: Vec<(String, &TrackingStatus)> = repaired
            .iter()
            .flat_map(|segment| {
                segment.inlines.iter().map(move |inline| {
                    let label = match inline {
                        InlineNode::Text(text) => text.text.clone(),
                        InlineNode::OpaqueInline(opaque) => match &opaque.kind {
                            OpaqueKind::Field(data) => format!("field:{:?}", data.field_kind),
                            _ => "opaque".to_string(),
                        },
                        _ => "other".to_string(),
                    };
                    (label, &segment.status)
                })
            })
            .collect();

        assert_eq!(
            flat,
            vec![
                ("field:Begin".to_string(), &TrackingStatus::Normal),
                ("field:Instruction".to_string(), &TrackingStatus::Normal),
                ("field:Separate".to_string(), &TrackingStatus::Normal),
                ("6".to_string(), &TrackingStatus::Normal),
                ("field:End".to_string(), &TrackingStatus::Normal),
            ]
        );
    }

    #[test]
    fn coalesce_auto_page_field_drops_uniform_deleted_field_range() {
        let deleted = TrackingStatus::Deleted(RevisionInfo {
            revision_id: 1,
            identity: 1,
            author: Some("tester".to_string()),
            date: None,
            apply_op_id: None,
        });

        let segments = vec![
            TrackedSegment {
                status: deleted.clone(),
                inlines: vec![
                    make_field_inline("begin", FieldKind::Begin, None),
                    make_field_inline("instr", FieldKind::Instruction, Some(" PAGE ")),
                    make_field_inline("sep", FieldKind::Separate, None),
                    make_text_inline("result-del", "2"),
                    make_field_inline("end", FieldKind::End, None),
                ],
            },
            TrackedSegment {
                status: TrackingStatus::Normal,
                inlines: vec![make_text_inline("suffix", "-")],
            },
        ];

        let repaired = coalesce_split_field_sequences(segments);
        let flat: Vec<(String, &TrackingStatus)> = repaired
            .iter()
            .flat_map(|segment| {
                segment.inlines.iter().map(move |inline| {
                    let label = match inline {
                        InlineNode::Text(text) => text.text.clone(),
                        InlineNode::OpaqueInline(opaque) => match &opaque.kind {
                            OpaqueKind::Field(data) => format!("field:{:?}", data.field_kind),
                            _ => "opaque".to_string(),
                        },
                        _ => "other".to_string(),
                    };
                    (label, &segment.status)
                })
            })
            .collect();

        assert_eq!(
            flat,
            vec![("-".to_string(), &TrackingStatus::Normal)],
            "uniformly deleted auto-updating field shell/result should be dropped entirely"
        );
    }

    #[test]
    fn safe_footer_page_field_result_is_normalized_in_fixture_path() {
        let before = fs::read("testdata/safe-us-vs-singapore/before.docx").expect("read before");
        let after = fs::read("testdata/safe-us-vs-singapore/after.docx").expect("read after");
        let runtime = SimpleRuntime::new();
        let before_import = runtime.import_docx(&before).expect("import before");
        let after_import = runtime.import_docx(&after).expect("import after");

        let before_para = before_import
            .canonical
            .footers
            .iter()
            .find(|footer| footer.part_name == "footer1.xml")
            .and_then(|footer| {
                footer
                    .blocks
                    .iter()
                    .find_map(|tracked| match &tracked.block {
                        BlockNode::Paragraph(p) => Some(p),
                        _ => None,
                    })
            })
            .expect("before footer page-number paragraph");
        let after_para = after_import
            .canonical
            .footers
            .iter()
            .find(|footer| footer.part_name == "footer1.xml")
            .and_then(|footer| {
                footer
                    .blocks
                    .iter()
                    .find_map(|tracked| match &tracked.block {
                        BlockNode::Paragraph(p) => Some(p),
                        _ => None,
                    })
            })
            .expect("after footer page-number paragraph");

        let inline_changes = crate::diff::diff_block_content_with_marks(
            &before_para.all_inlines_owned(),
            &after_para.all_inlines_owned(),
        );
        let base_opaques = collect_opaques(&BlockNode::Paragraph(before_para.clone()));
        let target_opaques = collect_opaques(&BlockNode::Paragraph(after_para.clone()));
        let revision = RevisionInfo {
            revision_id: 1,
            identity: 1,
            author: Some("tester".to_string()),
            date: None,
            apply_op_id: None,
        };
        let mut rev_counter = 2;
        let segments = inline_changes_to_segments_with_opaques(
            &before_para.id,
            &inline_changes,
            &revision,
            &mut rev_counter,
            &base_opaques,
            &target_opaques,
        )
        .expect("segments");

        let inserted_texts: Vec<String> = segments
            .iter()
            .filter(|segment| matches!(segment.status, TrackingStatus::Inserted(_)))
            .flat_map(|segment| segment.inlines.iter())
            .filter_map(|inline| match inline {
                InlineNode::Text(text) => Some(text.text.clone()),
                _ => None,
            })
            .collect();

        let repaired = coalesce_split_field_sequences(segments.clone());
        let repaired_inserted_texts: Vec<String> = repaired
            .iter()
            .filter(|segment| matches!(segment.status, TrackingStatus::Inserted(_)))
            .flat_map(|segment| segment.inlines.iter())
            .filter_map(|inline| match inline {
                InlineNode::Text(text) => Some(text.text.clone()),
                _ => None,
            })
            .collect();
        let flat_debug: Vec<String> = segments
            .iter()
            .flat_map(|segment| {
                segment.inlines.iter().map(move |inline| match inline {
                    InlineNode::Text(text) => format!("{:?}:text:{}", segment.status, text.text),
                    InlineNode::OpaqueInline(opaque) => match &opaque.kind {
                        OpaqueKind::Field(data) => format!(
                            "{:?}:field:{:?}:{:?}",
                            segment.status, data.field_kind, data.instruction_text
                        ),
                        _ => format!("{:?}:opaque", segment.status),
                    },
                    _ => format!("{:?}:other", segment.status),
                })
            })
            .collect();
        let flat_pairs: Vec<(InlineNode, TrackingStatus)> = segments
            .iter()
            .flat_map(|segment| {
                segment
                    .inlines
                    .iter()
                    .cloned()
                    .map(move |inline| (inline, segment.status.clone()))
            })
            .collect();
        let mut stack = Vec::new();
        let mut field_ranges = Vec::new();
        for (idx, (inline, _)) in flat_pairs.iter().enumerate() {
            if let Some(kind) = inline_field_kind(inline) {
                match kind {
                    FieldKind::Begin => stack.push(idx),
                    FieldKind::End => {
                        if let Some(begin_idx) = stack.pop() {
                            field_ranges.push((begin_idx, idx));
                        }
                    }
                    _ => {}
                }
            }
        }
        let range_debug: Vec<String> = field_ranges
            .iter()
            .map(|&(start, end)| {
                format!(
                    "{start}-{end}:auto={}",
                    range_contains_auto_field_instruction(&flat_pairs, start, end)
                )
            })
            .collect();

        assert!(
            !inserted_texts.iter().any(|text| text == "6"),
            "auto PAGE field result should be normalized, got inserted texts {inserted_texts:?}; explicit coalesce => {repaired_inserted_texts:?}; flat={flat_debug:?}; ranges={range_debug:?}"
        );
    }

    fn make_doc(blocks: Vec<BlockNode>) -> CanonDoc {
        CanonDoc {
            id: NodeId::from("doc"),
            blocks: blocks.into_iter().map(normal_tracked_block).collect(),
            meta: DocMeta {
                schema_version: SCHEMA_VERSION_V0.to_string(),
                docx_fingerprint: DocFingerprint("fp".to_string()),
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

    // =========================================================================
    // Hardening H2: body-state invariant validator
    //
    // These NEGATIVE tests hand-build an invalid `CanonDoc` DIRECTLY (bypassing
    // every producer) so each invariant's detector is exercised in isolation and
    // proven to name the violation precisely. The POSITIVE calibration — that
    // the whole test corpus passes with the debug-asserts live on every producer
    // path — is the full suite itself.
    // =========================================================================

    fn h2_rev(id: u32) -> RevisionInfo {
        RevisionInfo {
            revision_id: id,
            identity: 0,
            author: Some("t".into()),
            date: None,
            apply_op_id: None,
        }
    }

    /// A body `CanonDoc` from raw `TrackedBlock`s — bypasses the producers so a
    /// test can hand-build a state they would never emit.
    fn h2_doc(blocks: Vec<TrackedBlock>) -> CanonDoc {
        let mut doc = make_doc(vec![]);
        doc.blocks = blocks;
        doc
    }

    fn h2_para(id: &str) -> ParagraphNode {
        ParagraphNode::new_story_body(id, "x", None)
    }

    fn h2_cell(
        id: &str,
        blocks: Vec<BlockNode>,
        tracking: Option<TrackingStatus>,
    ) -> crate::domain::TableCellNode {
        crate::domain::TableCellNode {
            id: NodeId::from(id),
            blocks,
            grid_span: 1,
            v_merge: crate::domain::VerticalMerge::None,
            formatting: crate::domain::CellFormatting::default(),
            formatting_change: None,
            tracking_status: tracking,
            row_sdt_wrapper: None,
            content_sdt_wraps: Vec::new(),
            cnf_style: None,
            hide_mark: false,
            preserved: Vec::new(),
        }
    }

    fn h2_row(
        id: &str,
        cells: Vec<crate::domain::TableCellNode>,
        tracking: Option<TrackingStatus>,
    ) -> crate::domain::TableRowNode {
        crate::domain::TableRowNode {
            id: NodeId::from(id),
            cells,
            grid_before: 0,
            grid_after: 0,
            tracking_status: tracking,
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
        }
    }

    fn h2_table(id: &str, rows: Vec<crate::domain::TableRowNode>) -> BlockNode {
        BlockNode::from(TableNode {
            id: NodeId::from(id),
            rows,
            structure_hash: String::new(),
            formatting: crate::domain::TableFormatting::default(),
            formatting_change: None,
        })
    }

    /// Invariant 1 is RESOLUTION-scoped: a pending final mark is flagged by
    /// `assert_resolution_body_invariants` but NOT by the universal
    /// `assert_body_invariants` (merge/apply legitimately leave one, and it is
    /// valid Word markup).
    #[test]
    fn h2_neg_final_mark_pending_is_resolution_scoped() {
        let mut p = h2_para("p_last");
        p.para_mark_status = Some(TrackingStatus::Inserted(h2_rev(1)));
        let doc = h2_doc(vec![normal_tracked_block(BlockNode::from(p))]);

        assert!(
            assert_body_invariants(&doc).is_ok(),
            "the universal set must NOT check the final-mark rule"
        );
        let violations = assert_resolution_body_invariants(&doc).unwrap_err();
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].invariant, BodyInvariant::FinalMark);
        assert_eq!(violations[0].block_id.as_deref(), Some("p_last"));
    }

    /// A document-final moveFrom shadow (block-`Deleted` + `move_id`) is EXEMPT
    /// from the final-mark rule — its mark resolves via the moveFromRange pairing.
    #[test]
    fn h2_pos_final_mark_movefrom_shadow_exempt() {
        let tb = TrackedBlock {
            status: TrackingStatus::Deleted(h2_rev(1)),
            block: BlockNode::from(h2_para("p_shadow")),
            move_id: Some("m1".to_string()),
            block_sdt_wrap: None,
        };
        assert!(assert_resolution_body_invariants(&h2_doc(vec![tb])).is_ok());
    }

    /// Invariant 2: a range-marker half with an empty pairing id is malformed.
    #[test]
    fn h2_neg_range_marker_empty_id() {
        let mut p = h2_para("p");
        p.segments = vec![TrackedSegment {
            status: TrackingStatus::Normal,
            inlines: vec![InlineNode::CommentRangeStart { id: String::new() }],
        }];
        let doc = h2_doc(vec![normal_tracked_block(BlockNode::from(p))]);
        let violations = assert_body_invariants(&doc).unwrap_err();
        assert!(
            violations
                .iter()
                .any(|v| v.invariant == BodyInvariant::RangeMarkerWellFormed),
            "empty-id comment-range marker must be flagged: {violations:?}"
        );
    }

    /// Invariant 2 does NOT fire on a legitimate same-id collision or an
    /// identified lone half (both carry non-empty ids).
    #[test]
    fn h2_pos_range_marker_collision_and_lone_ok() {
        let mut p = h2_para("p");
        p.segments = vec![TrackedSegment {
            status: TrackingStatus::Normal,
            inlines: vec![
                InlineNode::CommentRangeStart {
                    id: "7".to_string(),
                },
                InlineNode::CommentRangeStart {
                    id: "7".to_string(),
                },
                InlineNode::CommentRangeEnd {
                    id: "7".to_string(),
                },
                // a lone end for a different id — the input's own state.
                InlineNode::CommentRangeEnd {
                    id: "9".to_string(),
                },
            ],
        }];
        let doc = h2_doc(vec![normal_tracked_block(BlockNode::from(p))]);
        assert!(assert_body_invariants(&doc).is_ok());
    }

    /// Invariant 3(a): CT_Row requires at least one cell.
    #[test]
    fn h2_neg_table_row_without_cells() {
        let table = h2_table("t", vec![h2_row("r0", vec![], None)]);
        let doc = h2_doc(vec![normal_tracked_block(table)]);
        let violations = assert_body_invariants(&doc).unwrap_err();
        assert_eq!(violations.len(), 1);
        assert_eq!(
            violations[0].invariant,
            BodyInvariant::TableStructuralCoherence
        );
        assert_eq!(violations[0].block_id.as_deref(), Some("r0"));
    }

    /// Invariant 3(b): a wholly-DELETED row must not carry a per-cell cellDel.
    #[test]
    fn h2_neg_table_celldel_in_deleted_row() {
        let cell = h2_cell(
            "c0",
            vec![BlockNode::from(h2_para("cp"))],
            Some(TrackingStatus::Deleted(h2_rev(2))),
        );
        let row = h2_row("r0", vec![cell], Some(TrackingStatus::Deleted(h2_rev(1))));
        let doc = h2_doc(vec![normal_tracked_block(h2_table("t", vec![row]))]);
        let violations = assert_body_invariants(&doc).unwrap_err();
        assert!(
            violations
                .iter()
                .any(|v| v.invariant == BodyInvariant::TableStructuralCoherence
                    && v.block_id.as_deref() == Some("c0")),
            "{violations:?}"
        );
    }

    /// Invariant 3(b) is DELETE-only: a wholly-INSERTED row whose cells carry a
    /// per-cell cellIns is a legitimate authoring shape, NOT flagged.
    #[test]
    fn h2_pos_table_cellins_in_inserted_row_ok() {
        let cell = h2_cell(
            "c0",
            vec![BlockNode::from(h2_para("cp"))],
            Some(TrackingStatus::Inserted(h2_rev(2))),
        );
        let row = h2_row("r0", vec![cell], Some(TrackingStatus::Inserted(h2_rev(1))));
        let doc = h2_doc(vec![normal_tracked_block(h2_table("t", vec![row]))]);
        assert!(assert_body_invariants(&doc).is_ok());
    }

    /// Invariant 3(c): a cell's final paragraph mark is never tracked-deleted.
    #[test]
    fn h2_neg_table_cell_final_mark_deleted() {
        // RETIRED negative (oracle-verified, wave campaign): a tracked-
        // DELETED final cell paragraph mark is a legal PENDING state real
        // Word pipelines author and desktop Word opens valid, resolves
        // accept-clears/reject-restores. The rule survives only as
        // mark_cell_content_deleted's producer contract (W5-F7). This
        // sentinel now pins the RELAXATION: the validator must NOT flag the
        // state (re-strengthening it condemned real redlined legal
        // documents at the door).
        let mut cp = h2_para("cp");
        cp.para_mark_status = Some(TrackingStatus::Deleted(h2_rev(1)));
        let cell = h2_cell("c0", vec![BlockNode::from(cp)], None);
        let row = h2_row("r0", vec![cell], None);
        let doc = h2_doc(vec![normal_tracked_block(h2_table("t", vec![row]))]);
        assert!(
            assert_body_invariants(&doc).is_ok(),
            "the pending wild state is not a body-state violation"
        );
    }

    /// Invariant 4: `para_mark_status == Some(Normal)` suppression on a plain
    /// Normal, non-move block is meaningless and flagged.
    #[test]
    fn h2_neg_mark_suppression_on_normal_block() {
        let mut p = h2_para("p");
        p.para_mark_status = Some(TrackingStatus::Normal);
        let doc = h2_doc(vec![normal_tracked_block(BlockNode::from(p))]);
        let violations = assert_body_invariants(&doc).unwrap_err();
        assert!(
            violations
                .iter()
                .any(|v| v.invariant == BodyInvariant::MarkSuppressionConsistency
                    && v.block_id.as_deref() == Some("p")),
            "{violations:?}"
        );
    }

    /// Invariant 4 does NOT fire when the suppression rides a tracked block.
    #[test]
    fn h2_pos_mark_suppression_on_inserted_block_ok() {
        let mut p = h2_para("p");
        p.para_mark_status = Some(TrackingStatus::Normal);
        let tb = TrackedBlock {
            status: TrackingStatus::Inserted(h2_rev(1)),
            block: BlockNode::from(p),
            move_id: None,
            block_sdt_wrap: None,
        };
        // Universal set (final-mark not checked) sees a legitimate suppression.
        assert!(assert_body_invariants(&h2_doc(vec![tb])).is_ok());
    }

    /// Invariant 5: `InsertedThenDeleted` at block level is never constructible.
    #[test]
    fn h2_neg_stacked_state_at_block_level() {
        let tb = TrackedBlock {
            status: TrackingStatus::InsertedThenDeleted(Box::new(StackedRevision {
                inserted: h2_rev(1),
                deleted: h2_rev(2),
            })),
            block: BlockNode::from(h2_para("p")),
            move_id: None,
            block_sdt_wrap: None,
        };
        let doc = h2_doc(vec![tb]);
        let violations = assert_body_invariants(&doc).unwrap_err();
        assert!(
            violations
                .iter()
                .any(|v| v.invariant == BodyInvariant::StackedStateCoherence
                    && v.block_id.as_deref() == Some("p")),
            "{violations:?}"
        );
    }

    /// A fully clean document passes both the universal and resolution sets.
    #[test]
    fn h2_pos_clean_doc_passes() {
        let doc = h2_doc(vec![normal_tracked_block(BlockNode::from(h2_para("p")))]);
        assert!(assert_body_invariants(&doc).is_ok());
        assert!(assert_resolution_body_invariants(&doc).is_ok());
    }

    /// CANARY: the debug wrapper the producers call actually PANICS on a
    /// violating document — proving the net is live, not silently compiled away.
    /// Together with the wiring (one `debug_assert_*` call at each of the five
    /// producer exits) this stands in for a per-path canary, since a correct
    /// producer cannot itself be made to emit a violation from a test.
    #[cfg(debug_assertions)]
    #[test]
    fn h2_canary_debug_wrapper_panics_on_violation() {
        let tb = TrackedBlock {
            status: TrackingStatus::InsertedThenDeleted(Box::new(StackedRevision {
                inserted: h2_rev(1),
                deleted: h2_rev(2),
            })),
            block: BlockNode::from(h2_para("p")),
            move_id: None,
            block_sdt_wrap: None,
        };
        let doc = h2_doc(vec![tb]);
        let prior = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {})); // keep the expected panic quiet
        let caught = std::panic::catch_unwind(|| debug_assert_body_invariants(&doc, "canary"));
        std::panic::set_hook(prior);
        assert!(
            caught.is_err(),
            "debug_assert_body_invariants must panic on a violating doc"
        );
    }

    fn flatten_text(doc: &CanonDoc) -> String {
        doc.blocks
            .iter()
            .flat_map(|tb| match &tb.block {
                BlockNode::Paragraph(p) => p
                    .all_inlines()
                    .filter_map(|inline| match inline {
                        InlineNode::Text(t) => Some(t.text.clone()),
                        InlineNode::HardBreak(_) => Some("\n".to_string()),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            })
            .collect::<Vec<_>>()
            .join("")
    }

    fn tracked_text_segment(id: &str, text: &str, status: TrackingStatus) -> TrackedSegment {
        TrackedSegment {
            status,
            inlines: vec![make_text_inline(id, text)],
        }
    }

    #[test]
    fn resolve_selected_revisions_accepts_only_selected_substitution() {
        let rev1 = RevisionInfo {
            revision_id: 1,
            identity: 1,
            author: Some("AI".to_string()),
            date: None,
            apply_op_id: None,
        };
        let rev2 = RevisionInfo {
            revision_id: 2,
            identity: 2,
            author: Some("AI".to_string()),
            date: None,
            apply_op_id: None,
        };
        let mut block = make_paragraph("p1", "");
        let paragraph = match &mut block {
            BlockNode::Paragraph(p) => p,
            _ => unreachable!(),
        };
        paragraph.segments = vec![
            tracked_text_segment("p1_old", "old", TrackingStatus::Deleted(rev1.clone())),
            tracked_text_segment("p1_new", "new", TrackingStatus::Inserted(rev1)),
            tracked_text_segment("p1_mid", " / ", TrackingStatus::Normal),
            tracked_text_segment("p1_left", "left", TrackingStatus::Deleted(rev2.clone())),
            tracked_text_segment("p1_right", "right", TrackingStatus::Inserted(rev2)),
        ];
        let mut doc = make_doc(vec![block]);

        resolve_selected_revisions_with_styles(
            &mut doc,
            &HashSet::from([1_u32]),
            ResolveSelectionAction::Accept,
            None,
        )
        .unwrap();

        let paragraph = match &doc.blocks[0].block {
            BlockNode::Paragraph(p) => p,
            _ => unreachable!(),
        };
        let segments: Vec<(String, &'static str)> = paragraph
            .segments
            .iter()
            .map(|segment| {
                let text = segment
                    .inlines
                    .iter()
                    .filter_map(|inline| match inline {
                        InlineNode::Text(t) => Some(t.text.clone()),
                        _ => None,
                    })
                    .collect::<String>();
                let status = match segment.status {
                    TrackingStatus::Normal => "normal",
                    TrackingStatus::Deleted(_) => "deleted",
                    TrackingStatus::Inserted(_) => "inserted",
                    TrackingStatus::InsertedThenDeleted(_) => "inserted_then_deleted",
                };
                (text, status)
            })
            .collect();

        assert_eq!(
            segments,
            vec![
                ("new".to_string(), "normal"),
                (" / ".to_string(), "normal"),
                ("left".to_string(), "deleted"),
                ("right".to_string(), "inserted"),
            ]
        );
    }

    #[test]
    fn resolve_selected_revisions_rejects_only_selected_substitution() {
        let rev1 = RevisionInfo {
            revision_id: 11,
            identity: 11,
            author: Some("AI".to_string()),
            date: None,
            apply_op_id: None,
        };
        let rev2 = RevisionInfo {
            revision_id: 12,
            identity: 12,
            author: Some("AI".to_string()),
            date: None,
            apply_op_id: None,
        };
        let mut block = make_paragraph("p1", "");
        let paragraph = match &mut block {
            BlockNode::Paragraph(p) => p,
            _ => unreachable!(),
        };
        paragraph.segments = vec![
            tracked_text_segment("p1_old", "old", TrackingStatus::Deleted(rev1.clone())),
            tracked_text_segment("p1_new", "new", TrackingStatus::Inserted(rev1)),
            tracked_text_segment("p1_mid", " / ", TrackingStatus::Normal),
            tracked_text_segment("p1_left", "left", TrackingStatus::Deleted(rev2.clone())),
            tracked_text_segment("p1_right", "right", TrackingStatus::Inserted(rev2)),
        ];
        let mut doc = make_doc(vec![block]);

        resolve_selected_revisions_with_styles(
            &mut doc,
            &HashSet::from([11_u32]),
            ResolveSelectionAction::Reject,
            None,
        )
        .unwrap();

        let paragraph = match &doc.blocks[0].block {
            BlockNode::Paragraph(p) => p,
            _ => unreachable!(),
        };
        let segments: Vec<(String, &'static str)> = paragraph
            .segments
            .iter()
            .map(|segment| {
                let text = segment
                    .inlines
                    .iter()
                    .filter_map(|inline| match inline {
                        InlineNode::Text(t) => Some(t.text.clone()),
                        _ => None,
                    })
                    .collect::<String>();
                let status = match segment.status {
                    TrackingStatus::Normal => "normal",
                    TrackingStatus::Deleted(_) => "deleted",
                    TrackingStatus::Inserted(_) => "inserted",
                    TrackingStatus::InsertedThenDeleted(_) => "inserted_then_deleted",
                };
                (text, status)
            })
            .collect();

        assert_eq!(
            segments,
            vec![
                ("old".to_string(), "normal"),
                (" / ".to_string(), "normal"),
                ("left".to_string(), "deleted"),
                ("right".to_string(), "inserted"),
            ]
        );
    }

    /// The bug this guards: `ReplaceHyperlinkText` (suggesting mode) records
    /// its edit as per-run `TrackingStatus` inside `HyperlinkData.runs` (the
    /// layer documented on `HyperlinkData`), not as a segment-level status.
    /// Selective resolution must reach that layer the same way accept_all /
    /// reject_all do (`project_hyperlink_runs`): accepting the revision id
    /// must update the display text and clear the tracked status, not leave
    /// the hyperlink untouched.
    #[test]
    fn resolve_selected_revisions_accepts_hyperlink_run_by_id() {
        let rev = RevisionInfo {
            revision_id: 41,
            identity: 41,
            author: Some("AI".to_string()),
            date: None,
            apply_op_id: None,
        };
        let mut block = make_paragraph("p1", "");
        let paragraph = match &mut block {
            BlockNode::Paragraph(p) => p,
            _ => unreachable!(),
        };
        paragraph.segments = normal_segment(vec![make_hyperlink_inline(
            "h1",
            "https://example.com",
            vec![
                HyperlinkRun {
                    text: "old".to_string(),
                    rpr_xml: None,
                    status: TrackingStatus::Deleted(rev.clone()),
                },
                HyperlinkRun {
                    text: "new".to_string(),
                    rpr_xml: None,
                    status: TrackingStatus::Inserted(rev),
                },
            ],
        )]);
        let mut doc = make_doc(vec![block]);

        resolve_selected_revisions_with_styles(
            &mut doc,
            &HashSet::from([41_u32]),
            ResolveSelectionAction::Accept,
            None,
        )
        .unwrap();

        let paragraph = match &doc.blocks[0].block {
            BlockNode::Paragraph(p) => p,
            _ => unreachable!(),
        };
        let InlineNode::OpaqueInline(opaque) = &paragraph.segments[0].inlines[0] else {
            panic!("expected the hyperlink opaque");
        };
        let OpaqueKind::Hyperlink(data) = &opaque.kind else {
            panic!("expected a hyperlink");
        };
        assert_eq!(data.text, "new");
        assert_eq!(data.runs.len(), 1);
        assert_eq!(data.runs[0].text, "new");
        assert_eq!(data.runs[0].status, TrackingStatus::Normal);
    }

    /// Reject counterpart: the deleted run is restored, the inserted run
    /// (and its markup) is dropped.
    #[test]
    fn resolve_selected_revisions_rejects_hyperlink_run_by_id() {
        let rev = RevisionInfo {
            revision_id: 42,
            identity: 42,
            author: Some("AI".to_string()),
            date: None,
            apply_op_id: None,
        };
        let mut block = make_paragraph("p1", "");
        let paragraph = match &mut block {
            BlockNode::Paragraph(p) => p,
            _ => unreachable!(),
        };
        paragraph.segments = normal_segment(vec![make_hyperlink_inline(
            "h1",
            "https://example.com",
            vec![
                HyperlinkRun {
                    text: "old".to_string(),
                    rpr_xml: None,
                    status: TrackingStatus::Deleted(rev.clone()),
                },
                HyperlinkRun {
                    text: "new".to_string(),
                    rpr_xml: None,
                    status: TrackingStatus::Inserted(rev),
                },
            ],
        )]);
        let mut doc = make_doc(vec![block]);

        resolve_selected_revisions_with_styles(
            &mut doc,
            &HashSet::from([42_u32]),
            ResolveSelectionAction::Reject,
            None,
        )
        .unwrap();

        let paragraph = match &doc.blocks[0].block {
            BlockNode::Paragraph(p) => p,
            _ => unreachable!(),
        };
        let InlineNode::OpaqueInline(opaque) = &paragraph.segments[0].inlines[0] else {
            panic!("expected the hyperlink opaque");
        };
        let OpaqueKind::Hyperlink(data) = &opaque.kind else {
            panic!("expected a hyperlink");
        };
        assert_eq!(data.text, "old");
        assert_eq!(data.runs.len(), 1);
        assert_eq!(data.runs[0].text, "old");
        assert_eq!(data.runs[0].status, TrackingStatus::Normal);
    }

    /// Domain rule: a selected id that matches no carrier in the document
    /// (stale, mistyped, or living in a carrier this selector doesn't
    /// handle) must refuse loudly and mutate nothing — never a silent
    /// no-op success. A mixed request (one resolvable id, one not) refuses
    /// as a whole, reporting only the unmatched id(s).
    #[test]
    fn resolve_selected_revisions_rejects_nonexistent_id() {
        let rev = RevisionInfo {
            revision_id: 1,
            identity: 1,
            author: Some("AI".to_string()),
            date: None,
            apply_op_id: None,
        };
        let mut block = make_paragraph("p1", "");
        let paragraph = match &mut block {
            BlockNode::Paragraph(p) => p,
            _ => unreachable!(),
        };
        paragraph.segments = vec![tracked_text_segment(
            "p1_t1",
            "text",
            TrackingStatus::Inserted(rev),
        )];
        let mut doc = make_doc(vec![block]);
        let before = doc.clone();

        let err = resolve_selected_revisions_with_styles(
            &mut doc,
            &HashSet::from([1_u32, 999_u32]),
            ResolveSelectionAction::Accept,
            None,
        )
        .unwrap_err();

        assert_eq!(err, vec![999_u32]);
        assert_eq!(doc, before, "a refused request must mutate nothing");
    }

    /// THE GAP: `enumerate_revisions` only ever inspected segment-level
    /// status and `InlineNode::Text` formatting_change — it never descended
    /// into `HyperlinkData.runs[*].status`, the layer `ReplaceHyperlinkText`
    /// (suggesting mode) tracks a display-text edit on (the enclosing
    /// segment stays `Normal`; see `make_hyperlink_inline`).
    /// `resolve_selected_revisions` can already resolve such an id
    /// (`resolvable_revision_ids` walks `HyperlinkData.runs`), so before this
    /// fix the id was resolvable but had no legitimate way to be discovered:
    /// `list_revisions` would never report it, so "accept revision 41" was
    /// something no caller could ever legally ask for.
    #[test]
    fn enumerate_revisions_includes_hyperlink_run_status() {
        let rev = RevisionInfo {
            revision_id: 41,
            identity: 41,
            author: Some("AI".to_string()),
            date: Some("2026-06-12T00:00:00Z".to_string()),
            apply_op_id: None,
        };
        let mut block = make_paragraph("p1", "");
        let paragraph = match &mut block {
            BlockNode::Paragraph(p) => p,
            _ => unreachable!(),
        };
        paragraph.segments = normal_segment(vec![make_hyperlink_inline(
            "h1",
            "https://example.com",
            vec![
                HyperlinkRun {
                    text: "old".to_string(),
                    rpr_xml: None,
                    status: TrackingStatus::Deleted(rev.clone()),
                },
                HyperlinkRun {
                    text: "new".to_string(),
                    rpr_xml: None,
                    status: TrackingStatus::Inserted(rev),
                },
            ],
        )]);
        let doc = make_doc(vec![block]);

        let records = enumerate_revisions(&doc);
        let hyperlink_records: Vec<&RevisionRecord> =
            records.iter().filter(|r| r.revision_id == 41).collect();
        assert_eq!(
            hyperlink_records.len(),
            2,
            "the hyperlink run's delete leg and insert leg must both be \
             enumerated (same as an ordinary text substitution's two legs); \
             records: {records:?}"
        );
        let kinds: Vec<RevisionKind> = hyperlink_records.iter().map(|r| r.kind).collect();
        assert_eq!(kinds, vec![RevisionKind::Delete, RevisionKind::Insert]);
        for r in &hyperlink_records {
            assert_eq!(r.author.as_deref(), Some("AI"));
            assert!(
                r.excerpt.contains("old") || r.excerpt.contains("new"),
                "excerpt should name the hyperlink run's text, got {:?}",
                r.excerpt
            );
        }
    }

    /// THE DOMAIN RULE this pins: the set of revision ids `enumerate_revisions`
    /// surfaces and the set `resolvable_revision_ids` accepts must agree —
    /// every enumerated id is resolvable, and every resolvable id is
    /// enumerated. Builds one document exercising every carrier kind both
    /// walks know about (block status, stacked segment status, run
    /// formatting, hyperlink run status, paragraph mark status, paragraph/
    /// table/row/cell formatting changes, row/cell tracking status,
    /// cell-interior content recursing into a nested table, EVERY story —
    /// header, footer, footnote, endnote, comment interior, and the
    /// whole-comment tracking status — and the body-level `w:sectPrChange`)
    /// and asserts set-equality. A future carrier added to one walk but not
    /// the other — like the hyperlink-run gap or the header/footer/comment
    /// gap this file once closed — fails this test loudly
    /// instead of silently drifting.
    /// Builds a `CanonDoc` carrying one revision in EVERY carrier kind both the
    /// enumeration walk (`enumerate_revisions`), the resolution walk
    /// (`resolvable_revision_ids`), and the mint mirror
    /// (`import::for_each_revision_id_mut`) know about: block status, stacked
    /// segment status, run/paragraph/table/row/cell formatting changes,
    /// hyperlink-run status, paragraph-mark status, row/cell tracking status,
    /// cell-interior content recursing into a nested table, EVERY story
    /// (header, footer, footnote, endnote, comment interior + whole-comment
    /// status), and the body-level `w:sectPrChange`. Ids are 1..=24 and all
    /// NONZERO on purpose — so `resolvable_revision_ids`'s `!= 0` sentinel skip
    /// can never mask a structural coverage difference between the walks.
    fn doc_with_every_revision_carrier() -> CanonDoc {
        // This fixture is hand-built (it never passes through the import/apply
        // mint walk), so it declares each carrier's engine identity directly.
        // It uses identity == wire id, which is a valid minted layout for a
        // collision-free document and keeps the existing id-keyed assertions
        // meaningful.
        fn rev(id: u32) -> RevisionInfo {
            RevisionInfo {
                revision_id: id,
                identity: id,
                author: Some("AI".to_string()),
                date: None,
                apply_op_id: None,
            }
        }

        // --- A paragraph exercising every paragraph-level carrier. ---
        let mut top_para = match make_paragraph("p1", "") {
            BlockNode::Paragraph(p) => p,
            _ => unreachable!(),
        };
        top_para.segments = vec![
            TrackedSegment {
                status: TrackingStatus::InsertedThenDeleted(Box::new(StackedRevision {
                    inserted: rev(2),
                    deleted: rev(3),
                })),
                inlines: vec![make_text_inline("p1_stacked", "stacked")],
            },
            {
                let mut formatted = tracked_text_segment("p1_fmt", "bold", TrackingStatus::Normal);
                let InlineNode::Text(t) = &mut formatted.inlines[0] else {
                    unreachable!()
                };
                t.formatting_change = Some(crate::domain::FormattingChange {
                    previous_marks: vec![],
                    previous_style_props: StyleProps::default(),
                    previous_rpr_authored: crate::domain::RunRprAuthored::default(),
                    revision_id: 4,
                    identity: 4,
                    author: "AI".to_string(),
                    date: None,
                });
                formatted
            },
            normal_segment(vec![make_hyperlink_inline(
                "h1",
                "https://example.com",
                vec![
                    HyperlinkRun {
                        text: "old".to_string(),
                        rpr_xml: None,
                        status: TrackingStatus::Deleted(rev(5)),
                    },
                    HyperlinkRun {
                        text: "new".to_string(),
                        rpr_xml: None,
                        status: TrackingStatus::Inserted(rev(6)),
                    },
                ],
            )])
            .remove(0),
        ];
        top_para.para_mark_status = Some(TrackingStatus::Inserted(rev(7)));
        top_para.formatting_change = Some(ParagraphFormattingChange {
            previous_alignment: None,
            previous_indentation: None,
            previous_spacing: None,
            previous_numbering: None,
            previous_numbering_explicitly_absent: false,
            previous_style_id: None,
            previous_keep_next: None,
            previous_keep_lines: None,
            previous_page_break_before: false,
            previous_widow_control: None,
            previous_contextual_spacing: None,
            previous_shading: None,
            previous_borders: None,
            previous_tab_stops: vec![],
            previous_literal_prefix_leading_tab_twips: None,
            previous_literal_prefix_trailing_tab_stop_twips: None,
            previous_paragraph_mark_marks: vec![],
            previous_paragraph_mark_style_props: StyleProps::default(),
            previous_paragraph_mark_rpr_off: Default::default(),
            previous_text_direction: None,
            previous_text_alignment: None,
            previous_mirror_indents: None,
            previous_auto_space_de: None,
            previous_auto_space_dn: None,
            previous_bidi: None,
            previous_suppress_auto_hyphens: None,
            previous_snap_to_grid: None,
            previous_overflow_punct: None,
            previous_adjust_right_ind: None,
            previous_word_wrap: None,
            previous_frame_pr: None,
            previous_preserved_ppr: vec![],
            revision_id: 8,
            identity: 8,
            author: "AI".to_string(),
            date: None,
        });

        // --- A table exercising every table-level carrier, plus a nested
        // paragraph and a nested table inside a cell. ---
        use crate::domain::{
            CellFormatting, CellFormattingChange, RowFormattingChange, TableFormatting,
            TableFormattingChange, TableNode, TableRowNode, VerticalMerge,
        };

        fn make_simple_cell(id: &str, blocks: Vec<BlockNode>) -> TableCellNode {
            TableCellNode {
                id: NodeId::from(id.to_string()),
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
            }
        }
        fn make_simple_row(id: &str, cells: Vec<TableCellNode>) -> TableRowNode {
            TableRowNode {
                id: NodeId::from(id.to_string()),
                cells,
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
            }
        }

        // Nested table (depth 2): its own tblPrChange, row tracking_status,
        // and cell tracking_status — proves the recursion the fix added
        // reaches a nested table's own carriers, not just its existence.
        let mut nested_cell = make_simple_cell("nc0", vec![]);
        nested_cell.tracking_status = Some(TrackingStatus::Deleted(rev(17)));
        let mut nested_row = make_simple_row("nr0", vec![nested_cell]);
        nested_row.tracking_status = Some(TrackingStatus::Inserted(rev(16)));
        let nested_table = TableNode {
            id: NodeId::from("nested_tbl"),
            rows: vec![nested_row],
            structure_hash: "nested-hash".to_string(),
            formatting: TableFormatting::default(),
            formatting_change: Some(TableFormattingChange {
                previous_width: None,
                previous_borders: None,
                previous_default_cell_margins: None,
                revision_id: 15,
                identity: 15,
                author: "AI".to_string(),
                date: None,
            }),
        };

        // Nested paragraph inside the same cell, carrying its own tracked
        // segment under its own id.
        let mut nested_para = match make_paragraph("np0", "") {
            BlockNode::Paragraph(p) => p,
            _ => unreachable!(),
        };
        nested_para.segments = vec![tracked_text_segment(
            "np0_t1",
            "nested",
            TrackingStatus::Deleted(rev(14)),
        )];

        let mut top_cell = make_simple_cell(
            "c0",
            vec![
                BlockNode::Paragraph(nested_para),
                BlockNode::from(nested_table),
            ],
        );
        top_cell.tracking_status = Some(TrackingStatus::Inserted(rev(12)));
        top_cell.formatting_change = Some(CellFormattingChange {
            previous_width: None,
            previous_borders: None,
            previous_shading: None,
            previous_v_align: None,
            previous_margins: None,
            previous_no_wrap: None,
            previous_text_direction: None,
            previous_tc_fit_text: None,
            revision_id: 13,
            identity: 13,
            author: "AI".to_string(),
            date: None,
        });

        let mut top_row = make_simple_row("r0", vec![top_cell]);
        top_row.tracking_status = Some(TrackingStatus::Deleted(rev(10)));
        top_row.formatting_change = Some(RowFormattingChange {
            previous_height: None,
            previous_height_rule: None,
            revision_id: 11,
            identity: 11,
            author: "AI".to_string(),
            date: None,
        });

        let top_table = TableNode {
            id: NodeId::from("top_tbl"),
            rows: vec![top_row],
            structure_hash: "top-hash".to_string(),
            formatting: TableFormatting::default(),
            formatting_change: Some(TableFormattingChange {
                previous_width: None,
                previous_borders: None,
                previous_default_cell_margins: None,
                revision_id: 9,
                identity: 9,
                author: "AI".to_string(),
                date: None,
            }),
        };

        // --- An opaque inline (a textbox drawing) whose verbatim `raw_xml`
        // carries a tracked change inside its `w:txbxContent`, plus an opaque
        // block quarantined for stacked revisions. Both are OpaqueInterior:
        // enumerated (visible) but NEVER resolvable, so their ids must NOT
        // appear in `resolvable_revision_ids`. They exercise the split the
        // assertion below pins. ---
        {
            let textbox = InlineNode::OpaqueInline(Box::new(crate::domain::OpaqueInlineNode {
                id: NodeId::from("p1_textbox"),
                kind: OpaqueKind::Drawing,
                opaque_ref: "p1:widget:0".to_string(),
                proof_ref: crate::domain::ProofRef {
                    part: crate::domain::DocPart::DocumentXml,
                    block_id: NodeId::from("p1"),
                    docx_anchor: String::new(),
                },
                wrapper_marks: vec![],
                wrapper_style_props: StyleProps::default(),
                raw_xml: Some(
                    br#"<w:drawing><wps:txbx><w:txbxContent><w:p><w:ins w:id="900" w:author="Vanessa" w:date="2016-11-24T18:36:00Z"><w:r><w:t>inside</w:t></w:r></w:ins></w:p></w:txbxContent></wps:txbx></w:drawing>"#.to_vec(),
                ),
                content_hash: None,
            }));
            top_para.segments.push(TrackedSegment {
                status: TrackingStatus::Normal,
                inlines: vec![textbox],
            });
        }

        let quarantined_block = BlockNode::from(crate::domain::OpaqueBlockNode {
            id: NodeId::from("quarantined_1"),
            kind: OpaqueKind::QuarantinedNestedTracking,
            opaque_ref: "body_item_2".to_string(),
            proof_ref: crate::domain::ProofRef {
                part: crate::domain::DocPart::DocumentXml,
                block_id: NodeId::from("quarantined_1"),
                docx_anchor: "body_index:2".to_string(),
            },
            range_marker: None,
        });

        let mut doc = make_doc(vec![
            BlockNode::Paragraph(top_para),
            BlockNode::from(top_table),
            quarantined_block,
        ]);
        // Block-level status on the top-level paragraph (id 1).
        doc.blocks[0].status = TrackingStatus::Inserted(rev(1));

        // --- One revision per STORY kind (ids 18–23), plus the body-level
        // sectPrChange (24): the story and section-property carriers the
        // enumeration covers. ---
        fn story_para(id: &str, text: &str, status: TrackingStatus) -> TrackedBlock {
            let mut p = match make_paragraph(id, "") {
                BlockNode::Paragraph(p) => p,
                _ => unreachable!(),
            };
            p.segments = vec![tracked_text_segment(&format!("{id}_t1"), text, status)];
            normal_tracked_block(BlockNode::Paragraph(p))
        }
        use crate::domain::{
            CommentStory, EndnoteStory, FooterStory, FootnoteStory, HeaderFooterKind, HeaderStory,
            NoteType, SectionPropertyChange,
        };
        doc.headers.push(HeaderStory {
            part_name: "header1.xml".to_string(),
            kind: HeaderFooterKind::Default,
            blocks: vec![story_para(
                "h_p1",
                "header ins",
                TrackingStatus::Inserted(rev(18)),
            )],
            content_hash: "h-hash".to_string(),
            synthesized: false,
        });
        doc.footers.push(FooterStory {
            part_name: "footer1.xml".to_string(),
            kind: HeaderFooterKind::Default,
            blocks: vec![story_para(
                "f_p1",
                "footer del",
                TrackingStatus::Deleted(rev(19)),
            )],
            content_hash: "f-hash".to_string(),
            synthesized: false,
        });
        doc.footnotes.push(FootnoteStory {
            id: "1".to_string(),
            note_type: NoteType::Normal,
            blocks: vec![story_para(
                "fn_p1",
                "footnote ins",
                TrackingStatus::Inserted(rev(20)),
            )],
            content_hash: "fn-hash".to_string(),
        });
        doc.endnotes.push(EndnoteStory {
            id: "1".to_string(),
            note_type: NoteType::Normal,
            blocks: vec![story_para(
                "en_p1",
                "endnote del",
                TrackingStatus::Deleted(rev(21)),
            )],
            content_hash: "en-hash".to_string(),
        });
        doc.comments.push(CommentStory {
            id: "1".to_string(),
            author: Some("AI".to_string()),
            date: None,
            blocks: vec![story_para(
                "cm_p1",
                "comment interior ins",
                TrackingStatus::Inserted(rev(22)),
            )],
            content_hash: "cm-hash".to_string(),
            // The whole-comment marker (what comment_delete writes) — reported
            // under the comment_story sentinel block id.
            tracking_status: Some(TrackingStatus::Deleted(rev(23))),
        });
        doc.body_section_property_change = Some(SectionPropertyChange {
            revision: rev(24),
            previous_properties_raw: b"<w:sectPr/>".to_vec(),
        });

        doc
    }

    #[test]
    fn enumerate_revisions_ids_agree_with_resolvable_revision_ids() {
        let doc = doc_with_every_revision_carrier();

        let records = enumerate_revisions(&doc);

        // The agreement, generalized for RFC-0002 §Phase-3b: resolvability is now
        // `revision_id != 0` (the never-selectable sentinel), a property of the
        // markup — NOT the kind. Every enumerated record with a non-zero id must
        // be in `resolvable_revision_ids`, and vice versa. This now INCLUDES a
        // well-formed interior revision (the textbox insert, id 900), which became
        // individually selectable; the quarantined block stays id 0.
        let resolvable_enumerated: HashSet<u32> = records
            .iter()
            .filter(|r| r.revision_id != 0)
            .map(|r| r.revision_id)
            .collect();
        let resolvable = resolvable_revision_ids(&doc);

        let mut expected: HashSet<u32> = (1..=24).collect();
        expected.insert(900); // the textbox interior insert — now resolvable
        assert_eq!(
            resolvable_enumerated, expected,
            "enumerate_revisions must surface exactly the fixture's resolvable carriers \
             (incl. the interior revision 900)"
        );
        assert_eq!(
            resolvable_enumerated, resolvable,
            "enumerate_revisions and resolvable_revision_ids must agree on the \
             RESOLVABLE (id != 0) set — an id enumerated-but-unresolvable is a dead \
             listing, an id resolvable-but-unenumerated is one no caller can discover"
        );

        // The two opaque-interior carriers (a textbox insert + a quarantined
        // stacked block) are BOTH enumerated (the census does not lie), but now
        // split on resolvability: the well-formed textbox insert carries its real
        // id 900 and IS in the resolvable set; the quarantined block stays id 0
        // (census-only). Kind stays OpaqueInterior for both — consumers still know
        // the change lives inside opaque content.
        let opaque: Vec<&RevisionRecord> = records
            .iter()
            .filter(|r| r.kind == RevisionKind::OpaqueInterior)
            .collect();
        assert_eq!(opaque.len(), 2, "got: {opaque:?}");
        // The textbox record: real id, resolvable, AND attributed to a TextFrame
        // story (RFC-0002 §Phase-3).
        let textbox_record = opaque
            .iter()
            .find(|r| r.author.as_deref() == Some("Vanessa"))
            .expect("opaque-interior census must surface the interior markup's author");
        assert_eq!(
            textbox_record.revision_id, 900,
            "textbox interior insert carries its real id"
        );
        assert!(
            resolvable.contains(&900),
            "a well-formed interior revision is resolvable"
        );
        assert!(
            matches!(textbox_record.location, StoryScope::TextFrame { .. }),
            "textbox interior revisions attributed to TextFrame, got {:?}",
            textbox_record.location
        );
        // The quarantined block stays the never-selectable sentinel.
        let quarantined = opaque
            .iter()
            .find(|r| r.author.is_none())
            .expect("quarantined block record");
        assert_eq!(
            quarantined.revision_id, 0,
            "quarantined interior stays census-only"
        );
        assert!(
            !resolvable.contains(&0),
            "the sentinel id never enters the resolvable set"
        );
    }

    /// THE DOMAIN RULE: a tracked change inside opaque content (here a textbox's
    /// `w:txbxContent`) is part of the honest census — it must be ENUMERATED so a
    /// "nothing left to resolve" consumer is not lied to — yet it is not
    /// INDIVIDUALLY resolvable: it carries no selectable id, so a selective
    /// resolve-by-id can never name it. Alongside an ordinary body insert,
    /// enumerate must list BOTH with distinguished kinds, only the body one
    /// appears in `resolvable_revision_ids`, and a selective reject of the body
    /// id clears the body insert while the opaque-interior record persists
    /// (selective resolution does not descend into opaque bytes — reporting it
    /// as gone would be the lie this closes).
    ///
    /// THE DOMAIN RULE (RFC-0002 §Phase-3b): a well-formed tracked change inside
    /// opaque content (here a textbox's `w:txbxContent`) is INDIVIDUALLY
    /// resolvable — it carries its real `w:id`, appears in `resolvable_revision_ids`,
    /// and a selective accept/reject of that id descends into the fragment and
    /// resolves JUST it, leaving other carriers pending. Resolvability is a
    /// property of the markup (well-formed, id'd), not of who authored it — the
    /// provenance-based split the RFC first proposed does not survive a
    /// serialize/reload, so it is not the model.
    #[test]
    fn opaque_interior_revision_is_individually_resolvable() {
        // Fresh doc per resolution (selective resolve mutates in place).
        let build = || {
            let mut block = make_paragraph("p1", "");
            let paragraph = match &mut block {
                BlockNode::Paragraph(p) => p,
                _ => unreachable!(),
            };
            let textbox = InlineNode::OpaqueInline(Box::new(crate::domain::OpaqueInlineNode {
                id: NodeId::from("p1_textbox"),
                kind: OpaqueKind::Drawing,
                opaque_ref: "p1:widget:0".to_string(),
                proof_ref: crate::domain::ProofRef {
                    part: crate::domain::DocPart::DocumentXml,
                    block_id: NodeId::from("p1"),
                    docx_anchor: String::new(),
                },
                wrapper_marks: vec![],
                wrapper_style_props: StyleProps::default(),
                raw_xml: Some(
                    br#"<w:drawing><wps:txbx><w:txbxContent><w:p><w:r><w:t>Fix </w:t></w:r><w:ins w:id="900" w:author="Vanessa"><w:r><w:t>this</w:t></w:r></w:ins></w:p></w:txbxContent></wps:txbx></w:drawing>"#.to_vec(),
                ),
                content_hash: None,
            }));
            paragraph.segments = vec![
                TrackedSegment {
                    status: TrackingStatus::Normal,
                    inlines: vec![textbox],
                },
                tracked_text_segment(
                    "p1_ins",
                    "added",
                    TrackingStatus::Inserted(RevisionInfo {
                        revision_id: 42,
                        identity: 42,
                        author: Some("USER".to_string()),
                        date: None,
                        apply_op_id: None,
                    }),
                ),
            ];
            make_doc(vec![block])
        };
        let interior_texts = |doc: &CanonDoc| -> Vec<String> {
            crate::opaque_targets::opaque_text_targets(doc)
                .into_iter()
                .map(|t| t.text)
                .collect()
        };

        // Enumerated with its real id, and resolvable alongside the body id 42.
        let doc = build();
        let opaque: Vec<_> = enumerate_revisions(&doc)
            .into_iter()
            .filter(|r| r.kind == RevisionKind::OpaqueInterior)
            .collect();
        assert_eq!(opaque.len(), 1);
        assert_eq!(
            opaque[0].revision_id, 900,
            "interior insert carries its real id"
        );
        assert_eq!(opaque[0].author.as_deref(), Some("Vanessa"));
        let resolvable = resolvable_revision_ids(&doc);
        assert!(resolvable.contains(&42) && resolvable.contains(&900));

        // Selective REJECT of just the body id 42 leaves the interior pending.
        let mut d = build();
        resolve_selected_revisions_with_styles(
            &mut d,
            &std::iter::once(42).collect(),
            ResolveSelectionAction::Reject,
            None,
        )
        .unwrap();
        assert!(
            enumerate_revisions(&d)
                .iter()
                .any(|r| r.kind == RevisionKind::OpaqueInterior),
            "interior revision not selected → still pending"
        );
        assert_eq!(
            interior_texts(&d),
            vec!["Fix this"],
            "interior text unchanged (as-shown)"
        );

        // Selective REJECT of the interior id 900 resolves JUST it: the tracked
        // insert is undone inside the fragment, and no interior revision remains.
        let mut d = build();
        resolve_selected_revisions_with_styles(
            &mut d,
            &std::iter::once(900).collect(),
            ResolveSelectionAction::Reject,
            None,
        )
        .unwrap();
        assert!(
            !enumerate_revisions(&d)
                .iter()
                .any(|r| r.kind == RevisionKind::OpaqueInterior),
            "the interior insert was rejected → no interior revision remains"
        );
        assert_eq!(
            interior_texts(&d),
            vec!["Fix "],
            "rejected insert removed from the textbox"
        );
        assert!(
            enumerate_revisions(&d).iter().any(|r| r.revision_id == 42),
            "the unselected body insert stays pending"
        );

        // Selective ACCEPT of 900 keeps the inserted text as normal content.
        let mut d = build();
        resolve_selected_revisions_with_styles(
            &mut d,
            &std::iter::once(900).collect(),
            ResolveSelectionAction::Accept,
            None,
        )
        .unwrap();
        assert!(
            !enumerate_revisions(&d)
                .iter()
                .any(|r| r.kind == RevisionKind::OpaqueInterior),
            "accepted interior insert leaves no pending revision"
        );
        assert_eq!(
            interior_texts(&d),
            vec!["Fix this"],
            "accepted text becomes normal"
        );
    }

    /// Test-fixture textbox: an opaque inline Drawing whose `raw_xml` wraps the
    /// given `w:txbxContent` paragraph children. Wild interior ids are NOT
    /// normalized at import, so collision fixtures write raw `w:id`s directly.
    fn textbox_inline(node_id: &str, txbx_paragraph_children: &str) -> InlineNode {
        InlineNode::OpaqueInline(Box::new(crate::domain::OpaqueInlineNode {
            id: NodeId::from(node_id),
            kind: OpaqueKind::Drawing,
            opaque_ref: format!("{node_id}:widget:0"),
            proof_ref: crate::domain::ProofRef {
                part: crate::domain::DocPart::DocumentXml,
                block_id: NodeId::from(node_id),
                docx_anchor: String::new(),
            },
            wrapper_marks: vec![],
            wrapper_style_props: StyleProps::default(),
            raw_xml: Some(
                format!(
                    "<w:drawing><wps:txbx><w:txbxContent><w:p>{txbx_paragraph_children}</w:p></w:txbxContent></wps:txbx></w:drawing>"
                )
                .into_bytes(),
            ),
            content_hash: None,
        }))
    }

    fn doc_with_textboxes(body_status: Option<TrackingStatus>, boxes: &[&str]) -> CanonDoc {
        let mut block = make_paragraph("p1", "");
        let paragraph = match &mut block {
            BlockNode::Paragraph(p) => p,
            _ => unreachable!(),
        };
        let mut segments = Vec::new();
        if let Some(status) = body_status {
            segments.push(tracked_text_segment("p1_body", "body change", status));
        }
        for (i, interior) in boxes.iter().enumerate() {
            segments.push(TrackedSegment {
                status: TrackingStatus::Normal,
                inlines: vec![textbox_inline(&format!("p1_tb{i}"), interior)],
            });
        }
        paragraph.segments = segments;
        make_doc(vec![block])
    }

    /// THE DOMAIN RULE (duplicate-wild-id demotion): interior ids are raw wire
    /// ids (import never rewrites opaque raw_xml), so an interior id that
    /// COLLIDES with a body revision id does not identify anything. The body
    /// carrier keeps its claim to the id (the typed model minted/owns it); the
    /// interior twin is demoted to census-only — and, critically, a selective
    /// resolve of the body id must leave the interior fragment byte-untouched.
    /// Before this rule, accepting body id 5 silently co-resolved a textbox
    /// `w:ins w:id="5"` the caller never selected.
    #[test]
    fn interior_id_colliding_with_body_id_is_demoted_and_never_co_resolved() {
        let interior = r#"<w:ins w:id="5" w:author="Wild"><w:r><w:t>twin</w:t></w:r></w:ins>"#;
        let build = || {
            doc_with_textboxes(
                Some(TrackingStatus::Inserted(RevisionInfo {
                    revision_id: 5,
                    identity: 5,
                    author: Some("USER".to_string()),
                    date: None,
                    apply_op_id: None,
                })),
                &[interior],
            )
        };

        let doc = build();
        let records = enumerate_revisions(&doc);
        let interior_record = records
            .iter()
            .find(|r| r.kind == RevisionKind::OpaqueInterior)
            .expect("interior census record");
        assert_eq!(
            interior_record.revision_id, 0,
            "colliding interior id demoted to the census-only sentinel"
        );
        assert!(
            interior_record
                .excerpt
                .contains("id shared with another revision"),
            "demotion reason is spelled out, got: {}",
            interior_record.excerpt
        );
        assert!(
            resolvable_revision_ids(&doc).contains(&5),
            "the body carrier keeps its claim to the id"
        );

        let raw_before = interior_raw_xml(&doc);
        let mut d = build();
        resolve_selected_revisions_with_styles(
            &mut d,
            &std::iter::once(5).collect(),
            ResolveSelectionAction::Accept,
            None,
        )
        .unwrap();
        assert!(
            enumerate_revisions(&d)
                .iter()
                .all(|r| r.kind == RevisionKind::OpaqueInterior || r.revision_id != 5),
            "the selected body insert resolved"
        );
        assert_eq!(
            interior_raw_xml(&d),
            raw_before,
            "the interior twin was NOT selected — its fragment must stay byte-identical"
        );
        // With the body id-5 carrier resolved away, the interior twin is now the
        // only id-5 carrier in the document — so it re-enumerates as resolvable
        // under its real id. Demotion is a property of the CURRENT id
        // population, re-derived per enumerate, not a permanent stain.
        let interior_after: Vec<_> = enumerate_revisions(&d)
            .into_iter()
            .filter(|r| r.kind == RevisionKind::OpaqueInterior)
            .collect();
        assert_eq!(
            interior_after.len(),
            1,
            "interior revision still honestly pending"
        );
        assert_eq!(
            interior_after[0].revision_id, 5,
            "with the collision gone, the interior id uniquely identifies again"
        );
    }

    fn interior_raw_xml(doc: &CanonDoc) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        crate::opaque_targets::visit_opaque_interiors(doc, &mut |_, _, iref| {
            if let crate::opaque_targets::OpaqueInteriorRef::Inline(o) = iref
                && let Some(raw) = &o.raw_xml
            {
                out.push(raw.clone());
            }
        });
        out
    }

    /// Same rule across opaques: two textboxes whose interior carriers share a
    /// wire id — the id identifies neither, so BOTH are census-only and a
    /// selection of the id is refused loudly (never "resolve both").
    #[test]
    fn interior_ids_duplicated_across_opaques_are_demoted_and_refused() {
        let a = r#"<w:ins w:id="7" w:author="A"><w:r><w:t>one</w:t></w:r></w:ins>"#;
        let b = r#"<w:ins w:id="7" w:author="B"><w:r><w:t>two</w:t></w:r></w:ins>"#;
        let mut doc = doc_with_textboxes(None, &[a, b]);

        let records = enumerate_revisions(&doc);
        let interior: Vec<_> = records
            .iter()
            .filter(|r| r.kind == RevisionKind::OpaqueInterior)
            .collect();
        assert_eq!(interior.len(), 2);
        assert!(
            interior.iter().all(|r| r.revision_id == 0),
            "both duplicate-id carriers demoted, got: {interior:?}"
        );
        assert!(!resolvable_revision_ids(&doc).contains(&7));

        let err = resolve_selected_revisions_with_styles(
            &mut doc,
            &std::iter::once(7).collect(),
            ResolveSelectionAction::Accept,
            None,
        )
        .expect_err("selecting a demoted id must refuse loudly");
        assert_eq!(err, vec![7]);
    }

    /// Same rule within one fragment: two top-level carriers sharing an id.
    #[test]
    fn interior_ids_duplicated_within_one_fragment_are_demoted() {
        let interior = r#"<w:ins w:id="9" w:author="A"><w:r><w:t>x</w:t></w:r></w:ins><w:ins w:id="9" w:author="A"><w:r><w:t>y</w:t></w:r></w:ins>"#;
        let mut doc = doc_with_textboxes(None, &[interior]);

        assert!(!resolvable_revision_ids(&doc).contains(&9));
        let err = resolve_selected_revisions_with_styles(
            &mut doc,
            &std::iter::once(9).collect(),
            ResolveSelectionAction::Accept,
            None,
        )
        .expect_err("duplicate in-fragment id must refuse");
        assert_eq!(err, vec![9]);
    }

    /// A stacked carrier sharing its host's id demotes the id (it identifies
    /// two carriers), and — resolver hardening — `resolve_fragment_selected`
    /// never matches a carrier nested inside another carrier even when handed
    /// the id directly: promotion keeps stacked markup verbatim-pending.
    #[test]
    fn stacked_carrier_sharing_an_id_demotes_it_and_resolver_never_descends() {
        let interior = r#"<w:ins w:id="5" w:author="A"><w:r><w:t>kept </w:t></w:r><w:del w:id="5" w:author="B"><w:r><w:delText>gone</w:delText></w:r></w:del></w:ins>"#;
        let doc = doc_with_textboxes(None, &[interior]);
        assert!(
            !resolvable_revision_ids(&doc).contains(&5),
            "an id shared between host and stacked carrier identifies neither"
        );

        // Belt: even handed the id directly, the fragment resolver only acts on
        // the TOP-LEVEL carrier and promotes its children verbatim — the
        // stacked w:del (still pending markup) survives the accept.
        let raw = br#"<w:txbxContent><w:p><w:ins w:id="5" w:author="A"><w:r><w:t>kept </w:t></w:r><w:del w:id="5" w:author="B"><w:r><w:delText>gone</w:delText></w:r></w:del></w:ins></w:p></w:txbxContent>"#;
        let resolved = match crate::normalize::resolve_fragment_selected(
            raw,
            &std::iter::once(5).collect(),
            true,
        ) {
            crate::normalize::FragmentResolution::Resolved(bytes) => bytes,
            other => panic!("expected Resolved, got {other:?}"),
        };
        let out = String::from_utf8(resolved).unwrap();
        assert!(
            out.contains("<w:del") && out.contains("gone"),
            "stacked deletion stays pending markup, got: {out}"
        );
        assert!(
            !out.contains("<w:ins"),
            "top-level insert accepted (unwrapped)"
        );
    }

    /// Interior moves are pair-carriers: never individually selectable (one
    /// half by id would orphan its counterpart and `w:move*Range*` markers).
    /// Census-only; the bulk accept/reject descent still resolves them.
    #[test]
    fn interior_move_carriers_stay_census_only() {
        let interior =
            r#"<w:moveTo w:id="11" w:author="A"><w:r><w:t>moved here</w:t></w:r></w:moveTo>"#;
        let mut doc = doc_with_textboxes(None, &[interior]);

        let records = enumerate_revisions(&doc);
        let record = records
            .iter()
            .find(|r| r.kind == RevisionKind::OpaqueInterior)
            .expect("move censused");
        assert_eq!(record.revision_id, 0, "interior move is census-only");
        assert!(!resolvable_revision_ids(&doc).contains(&11));
        let err = resolve_selected_revisions_with_styles(
            &mut doc,
            &std::iter::once(11).collect(),
            ResolveSelectionAction::Accept,
            None,
        )
        .expect_err("interior move id must refuse selection");
        assert_eq!(err, vec![11]);
    }

    /// A parsed wire `w:id="0"` interior carrier is the documented
    /// never-selectable sentinel — census-only, not silently dropped.
    #[test]
    fn parsed_wire_zero_interior_id_stays_census_only() {
        let interior = r#"<w:ins w:id="0" w:author="A"><w:r><w:t>zero</w:t></w:r></w:ins>"#;
        let doc = doc_with_textboxes(None, &[interior]);
        let records = enumerate_revisions(&doc);
        let record = records
            .iter()
            .find(|r| r.kind == RevisionKind::OpaqueInterior)
            .expect("wire-0 carrier censused");
        assert_eq!(record.revision_id, 0);
        assert!(!resolvable_revision_ids(&doc).contains(&0));
    }

    /// SIBLING drift guard to the agreement test above. `import::
    /// for_each_revision_id_mut` — the mutable walk that mints wire-`w:id="0"`
    /// revisions at import — is a HAND-MAINTAINED mirror of the read-only
    /// `resolvable_revision_ids` walk. Nothing but this test binds them: if a
    /// future change adds a carrier to the resolvable set (as the story
    /// work and the hyperlink-run fix each did) but forgets the mint
    /// mirror, wire-0 revisions on that carrier would silently regress to
    /// unresolvable. Over the fixture that exercises every carrier kind, the
    /// two walks must visit the IDENTICAL id set (all ids nonzero, so the
    /// resolvable `!= 0` skip cannot hide a divergence). A drift makes the
    /// sets differ and names the missing carrier id.
    #[test]
    fn for_each_revision_id_mut_mirrors_resolvable_revision_ids() {
        let mut doc = doc_with_every_revision_carrier();

        let resolvable = resolvable_revision_ids(&doc);

        let mut mirrored: HashSet<u32> = HashSet::new();
        crate::import::for_each_revision_id_mut(&mut doc, &mut |id| {
            mirrored.insert(*id);
        });

        // Interior (opaque raw_xml) revisions carry real ids already — descent-
        // minted or Word-written — so they are resolvable WITHOUT the import wire-0
        // mint walk touching them (RFC-0002 §Phase-3b). The mint mirror must cover
        // every BODY/story carrier exactly: `body_resolvable_revision_ids` is that
        // set by definition, and the full resolvable set is it plus the classified
        // interior ids (the fixture's textbox insert, id 900).
        let interior = classify_interior_ids(&doc);
        assert!(
            interior.selectable.contains(&900),
            "fixture's interior revision 900 is uniquely-selectable"
        );
        assert_eq!(
            resolvable,
            &body_resolvable_revision_ids(&doc) | &interior.selectable,
            "resolvable set must be exactly body ∪ classified-interior"
        );

        assert_eq!(
            mirrored,
            body_resolvable_revision_ids(&doc),
            "import::for_each_revision_id_mut (the wire-0 mint walk) and \
             body_resolvable_revision_ids must visit the SAME BODY carrier set — a \
             carrier in one but not the other means either a wire-0 revision \
             that mints but can't resolve, or one that resolves but never mints"
        );
    }

    /// THE DOMAIN RULE this pins: the internal `revision_id == 0` sentinel is
    /// still `enumerate`-reported yet resolver-REFUSED — "reported, never
    /// selectable" — for a GENUINELY id-less legacy formatting change (a
    /// pre-identity snapshot deserialized with `#[serde(default)]`, constructed
    /// directly here). This is the ONE legitimate enumerate↔resolvable
    /// divergence, and it must survive the wire-id-0 import fix
    /// (`import::mint_wire_zero_revision_ids`), which mints wire-0 to a real id
    /// but never touches an in-memory legacy blob (it never re-enters import).
    /// Over-minting this away would silently change a documented contract.
    #[test]
    fn legacy_zero_formatting_change_is_enumerated_but_not_resolvable() {
        let mut para = match make_paragraph("p1", "") {
            BlockNode::Paragraph(p) => p,
            _ => unreachable!(),
        };
        let mut seg = tracked_text_segment("t1", "text", TrackingStatus::Normal);
        let InlineNode::Text(t) = &mut seg.inlines[0] else {
            unreachable!()
        };
        t.formatting_change = Some(crate::domain::FormattingChange {
            previous_marks: vec![],
            previous_style_props: StyleProps::default(),
            previous_rpr_authored: crate::domain::RunRprAuthored::default(),
            revision_id: 0,
            identity: 0,
            author: "legacy".to_string(),
            date: None,
        });
        para.segments = vec![seg];
        let doc = make_doc(vec![BlockNode::Paragraph(para)]);

        let enumerated: Vec<u32> = enumerate_revisions(&doc)
            .into_iter()
            .map(|r| r.revision_id)
            .collect();
        assert_eq!(
            enumerated,
            vec![0],
            "the legacy formatting change IS reported, under the sentinel id 0"
        );
        assert!(
            !resolvable_revision_ids(&doc).contains(&0),
            "the legacy sentinel id 0 must stay UNSELECTABLE — reported, never resolvable"
        );
    }

    #[test]
    fn merge_diff_marks_inline_insert_delete_segments() {
        let base = make_doc(vec![make_paragraph("p1", "hello world")]);
        let target = make_doc(vec![make_paragraph("p1", "hello brave world")]);
        let diff = DocumentDiff {
            base_fingerprint: base.meta.docx_fingerprint.clone(),
            target_fingerprint: target.meta.docx_fingerprint.clone(),
            changes: vec![DiffChange::BlockModified {
                block_id: NodeId::from("p1"),
                old_text: "hello world".to_string(),
                new_text: "hello brave world".to_string(),
                inline_changes: vec![
                    InlineChange::Unchanged {
                        text: "hello ".to_string(),
                        marks: vec![Mark::Bold],
                        style_props: StyleProps::default(),
                        formatting_change: None,
                    },
                    InlineChange::Inserted {
                        text: "brave ".to_string(),
                        marks: vec![],
                        style_props: StyleProps::default(),
                        formatting_change: None,
                        rev_id: 0,
                    },
                    InlineChange::Unchanged {
                        text: "world".to_string(),
                        marks: vec![],
                        style_props: StyleProps::default(),
                        formatting_change: None,
                    },
                ],
                old_block: make_paragraph("p1", "hello world"),
                new_block: make_paragraph("p1", "hello brave world"),
                para_split: false,
            }],
        };
        let revision = RevisionInfo {
            revision_id: 1,
            identity: 1,
            author: Some("test".to_string()),
            date: Some("2026-02-12T00:00:00Z".to_string()),
            apply_op_id: None,
        };

        let merged = merge_diff(&base, &target, &diff, &revision)
            .expect("merge")
            .doc;
        let paragraph = match &merged.blocks[0].block {
            BlockNode::Paragraph(p) => p,
            _ => panic!("expected paragraph"),
        };
        assert_eq!(paragraph.segments.len(), 3);
        assert!(matches!(
            paragraph.segments[1].status,
            TrackingStatus::Inserted(_)
        ));
    }

    #[test]
    fn accept_reject_projection_roundtrip_text() {
        let base = make_doc(vec![make_paragraph("p1", "hello world")]);
        let target = make_doc(vec![make_paragraph("p1", "hello brave world")]);
        let diff = DocumentDiff {
            base_fingerprint: base.meta.docx_fingerprint.clone(),
            target_fingerprint: target.meta.docx_fingerprint.clone(),
            changes: vec![DiffChange::BlockModified {
                block_id: NodeId::from("p1"),
                old_text: "hello world".to_string(),
                new_text: "hello brave world".to_string(),
                inline_changes: vec![
                    InlineChange::Unchanged {
                        text: "hello ".to_string(),
                        marks: vec![],
                        style_props: StyleProps::default(),
                        formatting_change: None,
                    },
                    InlineChange::Inserted {
                        text: "brave ".to_string(),
                        marks: vec![],
                        style_props: StyleProps::default(),
                        formatting_change: None,
                        rev_id: 0,
                    },
                    InlineChange::Unchanged {
                        text: "world".to_string(),
                        marks: vec![],
                        style_props: StyleProps::default(),
                        formatting_change: None,
                    },
                ],
                old_block: make_paragraph("p1", "hello world"),
                new_block: make_paragraph("p1", "hello brave world"),
                para_split: false,
            }],
        };
        let revision = RevisionInfo {
            revision_id: 1,
            identity: 1,
            author: Some("test".to_string()),
            date: Some("2026-02-12T00:00:00Z".to_string()),
            apply_op_id: None,
        };

        let merged = merge_diff(&base, &target, &diff, &revision)
            .expect("merge")
            .doc;

        let mut accepted = merged.clone();
        accept_all(&mut accepted);
        assert_eq!(flatten_text(&accepted), "hello brave world");

        let mut rejected = merged;
        reject_all_with_styles(&mut rejected, None);
        assert_eq!(flatten_text(&rejected), "hello world");
    }

    #[test]
    fn normalize_projection_promotes_decorated_materialized_literal_prefix() {
        let mut paragraph = match make_paragraph("p1", "hello world") {
            BlockNode::Paragraph(p) => p,
            _ => panic!("expected paragraph"),
        };
        paragraph.segments = vec![TrackedSegment {
            status: TrackingStatus::Normal,
            inlines: vec![
                InlineNode::CommentRangeStart {
                    id: "comment_1".to_string(),
                },
                InlineNode::from(TextNode {
                    id: materialized_prefix_node_id(
                        &paragraph.id,
                        MaterializedPrefixKind::LiteralDeleted,
                    ),
                    text_role: Some(TextRole::MaterializedPrefix(
                        MaterializedPrefixKind::LiteralDeleted,
                    )),
                    text: "1. ".to_string(),
                    marks: Vec::new(),
                    style_props: StyleProps::default(),
                    rpr_authored: RunRprAuthored::default(),
                    formatting_change: None,
                }),
                InlineNode::from(TextNode {
                    id: NodeId::from("p1_body"),
                    text_role: None,
                    text: "hello world".to_string(),
                    marks: Vec::new(),
                    style_props: StyleProps::default(),
                    rpr_authored: RunRprAuthored::default(),
                    formatting_change: None,
                }),
            ],
        }];

        normalize_paragraph_after_projection(&mut paragraph, false);

        assert_eq!(paragraph.literal_prefix.as_deref(), Some("1."));
        assert_eq!(paragraph.segments.len(), 1);
        assert!(matches!(
            paragraph.segments[0].inlines.first(),
            Some(InlineNode::CommentRangeStart { .. })
        ));
        assert!(
            paragraph.segments[0]
                .inlines
                .iter()
                .all(|inline| match inline {
                    InlineNode::Text(t) => !is_materialized_prefix_text(t),
                    _ => true,
                }),
            "projection must strip materialized prefix text even when zero-width markers precede it"
        );
    }

    #[test]
    fn normalize_projection_discards_decorated_materialized_prefix_when_numbering_present() {
        let mut paragraph = match make_paragraph("p1", "hello world") {
            BlockNode::Paragraph(p) => p,
            _ => panic!("expected paragraph"),
        };
        let numbering = NumberingInfo {
            num_id: 7,
            ilvl: 0,
            synthesized_text: "1.".to_string(),
            is_bullet: false,
            restart_numbering: false,
        };
        paragraph.numbering = Some(numbering.clone());
        paragraph.segments = vec![TrackedSegment {
            status: TrackingStatus::Normal,
            inlines: vec![
                InlineNode::CommentRangeStart {
                    id: "comment_1".to_string(),
                },
                InlineNode::from(TextNode {
                    id: materialized_prefix_node_id(
                        &paragraph.id,
                        MaterializedPrefixKind::Structural,
                    ),
                    text_role: Some(TextRole::MaterializedPrefix(
                        MaterializedPrefixKind::Structural,
                    )),
                    text: "1. ".to_string(),
                    marks: Vec::new(),
                    style_props: StyleProps::default(),
                    rpr_authored: RunRprAuthored::default(),
                    formatting_change: None,
                }),
                InlineNode::from(TextNode {
                    id: NodeId::from("p1_body"),
                    text_role: None,
                    text: "hello world".to_string(),
                    marks: Vec::new(),
                    style_props: StyleProps::default(),
                    rpr_authored: RunRprAuthored::default(),
                    formatting_change: None,
                }),
            ],
        }];

        normalize_paragraph_after_projection(&mut paragraph, false);

        assert_eq!(paragraph.numbering, Some(numbering));
        assert_eq!(paragraph.literal_prefix, None);
        assert!(matches!(
            paragraph.segments[0].inlines.first(),
            Some(InlineNode::CommentRangeStart { .. })
        ));
        assert!(
            paragraph.segments[0]
                .inlines
                .iter()
                .all(|inline| match inline {
                    InlineNode::Text(t) => !is_materialized_prefix_text(t),
                    _ => true,
                }),
            "projection must discard materialized prefix text while preserving leading zero-width markers"
        );
    }

    #[test]
    fn block_modified_materializes_deleted_structural_prefix_when_numbering_is_removed() {
        let mut source = match make_paragraph("p1", "Instructions concerning specific positions") {
            BlockNode::Paragraph(p) => p,
            _ => panic!("expected paragraph"),
        };
        source.style_id = Some(IStr::from("Numberedtitlelevel3"));
        source.numbering = Some(NumberingInfo {
            num_id: 210,
            ilvl: 0,
            synthesized_text: "(3)".to_string(),
            is_bullet: false,
            restart_numbering: false,
        });
        source.rendered_text = Some("(3)\tInstructions concerning specific positions".to_string());

        let target = match make_paragraph("p1", "Instructions concerning specific positions") {
            BlockNode::Paragraph(p) => p,
            _ => panic!("expected paragraph"),
        };
        let inline_changes = crate::diff::diff_block_content_with_marks(
            &source.all_inlines_owned(),
            &target.all_inlines_owned(),
        );

        let mut blocks = vec![TrackedBlock {
            status: TrackingStatus::Normal,
            block: BlockNode::Paragraph(source),
            move_id: None,
            block_sdt_wrap: None,
        }];
        let revision = RevisionInfo {
            revision_id: 1,
            identity: 1,
            author: Some("tester".to_string()),
            date: Some("2026-04-10T00:00:00Z".to_string()),
            apply_op_id: None,
        };
        let mut rev_counter = 2;

        apply_block_modified(
            &mut blocks,
            &NodeId::from("p1"),
            &inline_changes,
            &BlockNode::Paragraph(target),
            &revision,
            &mut rev_counter,
            "test",
        )
        .expect("apply_block_modified");

        let paragraph = match &blocks[0].block {
            BlockNode::Paragraph(p) => p,
            _ => panic!("expected paragraph"),
        };

        assert_eq!(paragraph.numbering, None);
        assert_eq!(
            paragraph
                .formatting_change
                .as_ref()
                .and_then(|change| change.previous_numbering.as_ref())
                .map(|numbering| numbering.synthesized_text.as_str()),
            Some("(3)")
        );
        assert_eq!(
            paragraph.segments.len(),
            2,
            "deleted prefix + unchanged body"
        );
        assert!(matches!(
            paragraph.segments[0].status,
            TrackingStatus::Deleted(_)
        ));
        let prefix_text = match &paragraph.segments[0].inlines[0] {
            InlineNode::Text(text) => text,
            other => panic!("expected prefix text, got {other:?}"),
        };
        assert_eq!(
            prefix_text.text_role,
            Some(TextRole::MaterializedPrefix(
                MaterializedPrefixKind::StructuralDeleted
            ))
        );
        assert_eq!(prefix_text.text, "(3) ");
        assert!(matches!(
            paragraph.segments[1].status,
            TrackingStatus::Normal
        ));
        assert_eq!(
            crate::table::extract_inlines_text(&paragraph.segments[1].inlines),
            "Instructions concerning specific positions"
        );
    }

    #[test]
    fn table_cells_modified_materializes_deleted_structural_prefix_when_numbering_is_removed() {
        use crate::domain::{
            CellFormatting, CellParagraphChange, TableCellChange, TableCellNode, TableFormatting,
            TableNode, TableRowNode, VerticalMerge,
        };

        fn make_simple_cell(id: &str, blocks: Vec<BlockNode>) -> TableCellNode {
            TableCellNode {
                id: NodeId::from(id.to_string()),
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
            }
        }

        let mut source = match make_paragraph("p1", "Instructions concerning specific positions") {
            BlockNode::Paragraph(p) => p,
            _ => panic!("expected paragraph"),
        };
        source.style_id = Some(IStr::from("Numberedtitlelevel3"));
        source.numbering = Some(NumberingInfo {
            num_id: 210,
            ilvl: 0,
            synthesized_text: "(3)".to_string(),
            is_bullet: false,
            restart_numbering: false,
        });
        source.rendered_text = Some("(3)\tInstructions concerning specific positions".to_string());

        let mut target = match make_paragraph("p1", "Instructions concerning specific positions") {
            BlockNode::Paragraph(p) => p,
            _ => panic!("expected paragraph"),
        };
        target.style_id = Some(IStr::from("Numberedtitlelevel3"));

        let inline_changes = crate::diff::diff_block_content_with_marks(
            &source.all_inlines_owned(),
            &target.all_inlines_owned(),
        );

        let table = TableNode {
            id: NodeId::from("tbl1"),
            rows: vec![TableRowNode {
                id: NodeId::from("tbl1_r0"),
                cells: vec![make_simple_cell("c0", vec![BlockNode::Paragraph(source)])],
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
            structure_hash: "tbl-hash".to_string(),
            formatting: TableFormatting::default(),
            formatting_change: None,
        };

        let mut blocks = vec![TrackedBlock {
            status: TrackingStatus::Normal,
            block: BlockNode::from(table),
            move_id: None,
            block_sdt_wrap: None,
        }];
        let revision = RevisionInfo {
            revision_id: 1,
            identity: 1,
            author: Some("tester".to_string()),
            date: Some("2026-04-10T00:00:00Z".to_string()),
            apply_op_id: None,
        };
        let mut rev_counter = 2;

        apply_table_cells_modified(
            &mut blocks,
            &NodeId::from("tbl1"),
            &[TableCellChange {
                row_index: 0,
                cell_index: 0,
                paragraph_changes: vec![CellParagraphChange {
                    block_index: 0,
                    inline_changes,
                    new_block: BlockNode::Paragraph(target),
                }],
                nested_table_diffs: vec![],
                new_cell_formatting: None,
            }],
            &revision,
            &mut rev_counter,
            "test",
        )
        .expect("apply_table_cells_modified");

        let paragraph = match &blocks[0].block {
            BlockNode::Table(table) => match &table.rows[0].cells[0].blocks[0] {
                BlockNode::Paragraph(p) => p,
                _ => panic!("expected paragraph"),
            },
            _ => panic!("expected table"),
        };

        assert_eq!(paragraph.numbering, None);
        assert_eq!(
            paragraph
                .formatting_change
                .as_ref()
                .and_then(|change| change.previous_numbering.as_ref())
                .map(|numbering| numbering.synthesized_text.as_str()),
            Some("(3)")
        );
        assert_eq!(
            paragraph.segments.len(),
            2,
            "deleted prefix + unchanged body"
        );
        let prefix_text = match &paragraph.segments[0].inlines[0] {
            InlineNode::Text(text) => text,
            other => panic!("expected prefix text, got {other:?}"),
        };
        assert_eq!(
            prefix_text.text_role,
            Some(TextRole::MaterializedPrefix(
                MaterializedPrefixKind::StructuralDeleted
            ))
        );
        assert_eq!(prefix_text.text, "(3) ");
        assert!(matches!(
            paragraph.segments[1].status,
            TrackingStatus::Normal
        ));
        assert_eq!(
            crate::table::extract_inlines_text(&paragraph.segments[1].inlines),
            "Instructions concerning specific positions"
        );
    }

    #[test]
    fn ppr_change_detected_on_alignment_change() {
        use crate::domain::Alignment;

        let mut base_para = make_paragraph("p1", "hello");
        if let BlockNode::Paragraph(p) = &mut base_para {
            p.align = Some(Alignment::Left);
            p.has_direct_align = true;
        }
        let mut target_para = make_paragraph("p1", "hello");
        if let BlockNode::Paragraph(p) = &mut target_para {
            p.align = Some(Alignment::Center);
            p.has_direct_align = true;
        }

        let base = make_doc(vec![base_para.clone()]);
        let target = make_doc(vec![target_para.clone()]);
        let diff = DocumentDiff {
            base_fingerprint: base.meta.docx_fingerprint.clone(),
            target_fingerprint: target.meta.docx_fingerprint.clone(),
            changes: vec![DiffChange::BlockModified {
                block_id: NodeId::from("p1"),
                old_text: "hello".to_string(),
                new_text: "hello".to_string(),
                inline_changes: vec![InlineChange::Unchanged {
                    text: "hello".to_string(),
                    marks: vec![],
                    style_props: StyleProps::default(),
                    formatting_change: None,
                }],
                old_block: base_para,
                new_block: target_para,
                para_split: false,
            }],
        };
        let revision = RevisionInfo {
            revision_id: 1,
            identity: 1,
            author: Some("test-author".to_string()),
            date: Some("2026-02-28T00:00:00Z".to_string()),
            apply_op_id: None,
        };

        let merged = merge_diff(&base, &target, &diff, &revision)
            .expect("merge")
            .doc;
        let para = match &merged.blocks[0].block {
            BlockNode::Paragraph(p) => p,
            _ => panic!("expected paragraph"),
        };

        // Paragraph should now have Center alignment
        assert_eq!(para.align, Some(Alignment::Center));
        // And a formatting_change recording the old Left alignment
        let fc = para.formatting_change.as_ref().expect("expected pPrChange");
        assert_eq!(fc.previous_alignment, Some(Alignment::Left));
        assert_eq!(fc.previous_indentation, None);
        assert_eq!(fc.previous_spacing, None);
        assert_eq!(fc.author, "test-author");
        assert_eq!(fc.date, Some("2026-02-28T00:00:00Z".to_string()));
    }

    #[test]
    fn no_ppr_change_when_formatting_identical() {
        use crate::domain::Alignment;

        let mut base_para = make_paragraph("p1", "hello");
        if let BlockNode::Paragraph(p) = &mut base_para {
            p.align = Some(Alignment::Left);
        }
        let mut target_para = make_paragraph("p1", "hello");
        if let BlockNode::Paragraph(p) = &mut target_para {
            p.align = Some(Alignment::Left);
        }

        let base = make_doc(vec![base_para.clone()]);
        let target = make_doc(vec![target_para.clone()]);
        let diff = DocumentDiff {
            base_fingerprint: base.meta.docx_fingerprint.clone(),
            target_fingerprint: target.meta.docx_fingerprint.clone(),
            changes: vec![DiffChange::BlockModified {
                block_id: NodeId::from("p1"),
                old_text: "hello".to_string(),
                new_text: "hello".to_string(),
                inline_changes: vec![InlineChange::Unchanged {
                    text: "hello".to_string(),
                    marks: vec![],
                    style_props: StyleProps::default(),
                    formatting_change: None,
                }],
                old_block: base_para,
                new_block: target_para,
                para_split: false,
            }],
        };
        let revision = RevisionInfo {
            revision_id: 1,
            identity: 1,
            author: Some("test-author".to_string()),
            date: Some("2026-02-28T00:00:00Z".to_string()),
            apply_op_id: None,
        };

        let merged = merge_diff(&base, &target, &diff, &revision)
            .expect("merge")
            .doc;
        let para = match &merged.blocks[0].block {
            BlockNode::Paragraph(p) => p,
            _ => panic!("expected paragraph"),
        };

        // No formatting change when alignment is the same
        assert!(para.formatting_change.is_none());
    }

    #[test]
    fn ppr_change_detected_on_spacing_change() {
        use crate::domain::ParagraphSpacing;

        let mut base_para = make_paragraph("p1", "hello");
        if let BlockNode::Paragraph(p) = &mut base_para {
            p.spacing = Some(ParagraphSpacing {
                before: Some(240),
                after: Some(120),
                before_lines: None,
                after_lines: None,
                before_autospacing: None,
                after_autospacing: None,
                line: None,
                line_rule: None,
            });
            p.has_direct_spacing = true;
        }
        let mut target_para = make_paragraph("p1", "hello");
        if let BlockNode::Paragraph(p) = &mut target_para {
            p.spacing = Some(ParagraphSpacing {
                before: Some(480),
                after: Some(120),
                before_lines: None,
                after_lines: None,
                before_autospacing: None,
                after_autospacing: None,
                line: None,
                line_rule: None,
            });
            p.has_direct_spacing = true;
        }

        let base = make_doc(vec![base_para.clone()]);
        let target = make_doc(vec![target_para.clone()]);
        let diff = DocumentDiff {
            base_fingerprint: base.meta.docx_fingerprint.clone(),
            target_fingerprint: target.meta.docx_fingerprint.clone(),
            changes: vec![DiffChange::BlockModified {
                block_id: NodeId::from("p1"),
                old_text: "hello".to_string(),
                new_text: "hello".to_string(),
                inline_changes: vec![InlineChange::Unchanged {
                    text: "hello".to_string(),
                    marks: vec![],
                    style_props: StyleProps::default(),
                    formatting_change: None,
                }],
                old_block: base_para,
                new_block: target_para,
                para_split: false,
            }],
        };
        let revision = RevisionInfo {
            revision_id: 1,
            identity: 1,
            author: Some("test-author".to_string()),
            date: Some("2026-02-28T00:00:00Z".to_string()),
            apply_op_id: None,
        };

        let merged = merge_diff(&base, &target, &diff, &revision)
            .expect("merge")
            .doc;
        let para = match &merged.blocks[0].block {
            BlockNode::Paragraph(p) => p,
            _ => panic!("expected paragraph"),
        };

        // Paragraph should now have the new spacing
        assert_eq!(para.spacing.as_ref().unwrap().before, Some(480));
        // And a formatting_change recording the old spacing
        let fc = para.formatting_change.as_ref().expect("expected pPrChange");
        assert_eq!(fc.previous_spacing.as_ref().unwrap().before, Some(240));
        assert_eq!(fc.previous_alignment, None);
        assert_eq!(fc.author, "test-author");
    }

    /// Bookmarks (Decoration nodes) must survive the diff/merge pipeline when
    /// a paragraph's text is modified. The diff algorithm operates on text
    /// and drops zero-width decorations; merge_diff must re-inject them.
    #[test]
    fn merge_diff_preserves_bookmark_decorations() {
        // Build a base paragraph with a bookmark decoration alongside text.
        let bookmark_raw = b"<w:bookmarkStart xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" w:id=\"5\" w:name=\"_Ref_Clause_4_2\"/>";
        let bookmark_end_raw = b"<w:bookmarkEnd xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" w:id=\"5\"/>";
        let bookmark_start = InlineNode::from(DecorationNode {
            id: NodeId::from("p1_deco_1"),
            kind: DecorationType::Bookmark,
            opaque_ref: "paragraph:p1:deco:1".to_string(),
            proof_ref: ProofRef {
                part: DocPart::DocumentXml,
                block_id: NodeId::from("p1_deco_1"),
                docx_anchor: "paragraph:p1:deco:1".to_string(),
            },
            wrapper_marks: Vec::new(),
            wrapper_style_props: StyleProps::default(),
            raw_xml: Some(bookmark_raw.to_vec()),
            origin: None,
        });
        let bookmark_end = InlineNode::from(DecorationNode {
            id: NodeId::from("p1_deco_2"),
            kind: DecorationType::Bookmark,
            opaque_ref: "paragraph:p1:deco:2".to_string(),
            proof_ref: ProofRef {
                part: DocPart::DocumentXml,
                block_id: NodeId::from("p1_deco_2"),
                docx_anchor: "paragraph:p1:deco:2".to_string(),
            },
            wrapper_marks: Vec::new(),
            wrapper_style_props: StyleProps::default(),
            raw_xml: Some(bookmark_end_raw.to_vec()),
            origin: None,
        });

        let base_para = BlockNode::from(ParagraphNode {
            id: NodeId::from("p1"),
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
            segments: vec![TrackedSegment {
                status: TrackingStatus::Normal,
                inlines: vec![
                    bookmark_start,
                    InlineNode::from(TextNode {
                        id: NodeId::from("p1_t1"),
                        text_role: None,
                        text: "hello world".to_string(),
                        marks: Vec::new(),
                        style_props: StyleProps::default(),
                        rpr_authored: RunRprAuthored::default(),
                        formatting_change: None,
                    }),
                    bookmark_end,
                ],
            }],
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

        let base = make_doc(vec![base_para]);
        let target = make_doc(vec![make_paragraph("p1", "hello brave world")]);

        let diff = DocumentDiff {
            base_fingerprint: base.meta.docx_fingerprint.clone(),
            target_fingerprint: target.meta.docx_fingerprint.clone(),
            changes: vec![DiffChange::BlockModified {
                block_id: NodeId::from("p1"),
                old_text: "hello world".to_string(),
                new_text: "hello brave world".to_string(),
                inline_changes: vec![
                    InlineChange::Unchanged {
                        text: "hello ".to_string(),
                        marks: vec![],
                        style_props: StyleProps::default(),
                        formatting_change: None,
                    },
                    InlineChange::Inserted {
                        text: "brave ".to_string(),
                        marks: vec![],
                        style_props: StyleProps::default(),
                        formatting_change: None,
                        rev_id: 0,
                    },
                    InlineChange::Unchanged {
                        text: "world".to_string(),
                        marks: vec![],
                        style_props: StyleProps::default(),
                        formatting_change: None,
                    },
                ],
                old_block: make_paragraph("p1", "hello world"),
                new_block: make_paragraph("p1", "hello brave world"),
                para_split: false,
            }],
        };

        let revision = RevisionInfo {
            revision_id: 1,
            identity: 1,
            author: Some("test".to_string()),
            date: Some("2026-01-01T00:00:00Z".to_string()),
            apply_op_id: None,
        };

        let merged = merge_diff(&base, &target, &diff, &revision)
            .expect("merge")
            .doc;
        let paragraph = match &merged.blocks[0].block {
            BlockNode::Paragraph(p) => p,
            _ => panic!("expected paragraph"),
        };

        // Count bookmark decorations in the merged paragraph.
        let bookmark_count = paragraph
            .all_inlines()
            .filter(
                |i| matches!(i, InlineNode::Decoration(d) if d.kind == DecorationType::Bookmark),
            )
            .count();

        assert_eq!(
            bookmark_count,
            2,
            "Both bookmarkStart and bookmarkEnd decorations must survive the diff/merge \
             pipeline. Found {} bookmark decoration(s) in merged paragraph. \
             Segments: {:?}",
            bookmark_count,
            paragraph
                .segments
                .iter()
                .map(|s| s.inlines.len())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn merge_diff_tracks_nested_table_cell_changes() {
        use crate::domain::{
            CellFormatting, TableCellNode, TableFormatting, TableNode, TableRowNode, VerticalMerge,
        };

        fn make_simple_cell(id: &str, blocks: Vec<BlockNode>) -> TableCellNode {
            TableCellNode {
                id: NodeId::from(id.to_string()),
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
            }
        }

        fn make_simple_table(id: &str, rows: Vec<Vec<TableCellNode>>) -> TableNode {
            TableNode {
                id: NodeId::from(id.to_string()),
                rows: rows
                    .into_iter()
                    .enumerate()
                    .map(|(r, cells)| TableRowNode {
                        id: NodeId::from(format!("{id}_r{r}")),
                        cells,
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
                structure_hash: "hash1".to_string(),
                formatting: TableFormatting::default(),
                formatting_change: None,
            }
        }

        // Build inner table (base): 1 row, 1 cell with "old text"
        let inner_base = make_simple_table(
            "inner",
            vec![vec![make_simple_cell(
                "inner_c0",
                vec![make_paragraph("inner_p0", "old text")],
            )]],
        );

        // Build outer table (base): 1 row, 1 cell containing a paragraph + the inner table
        let outer_base = make_simple_table(
            "outer",
            vec![vec![make_simple_cell(
                "outer_c0",
                vec![
                    make_paragraph("outer_p0", "outer text"),
                    BlockNode::from(inner_base),
                ],
            )]],
        );

        // Build inner table (target): same structure, different text
        let inner_target = make_simple_table(
            "inner",
            vec![vec![make_simple_cell(
                "inner_c0",
                vec![make_paragraph("inner_p0", "new text")],
            )]],
        );

        // Build outer table (target): same structure, same outer text
        let outer_target = make_simple_table(
            "outer",
            vec![vec![make_simple_cell(
                "outer_c0",
                vec![
                    make_paragraph("outer_p0", "outer text"),
                    BlockNode::from(inner_target),
                ],
            )]],
        );

        let base = make_doc(vec![BlockNode::from(outer_base)]);
        let target = make_doc(vec![BlockNode::from(outer_target)]);

        let diff = crate::diff::diff_documents(&base, &target).expect("diff should succeed");

        let revision = RevisionInfo {
            revision_id: 1,
            identity: 1,
            author: Some("test".to_string()),
            date: Some("2024-01-01T00:00:00Z".to_string()),
            apply_op_id: None,
        };

        let merged = merge_diff(&base, &target, &diff, &revision)
            .expect("merge should succeed")
            .doc;

        // Find the outer table in the merged doc
        let outer_table = match &merged.blocks[0].block {
            BlockNode::Table(t) => t,
            _ => panic!("expected outer table"),
        };

        // Get the first cell of the outer table
        let outer_cell = &outer_table.rows[0].cells[0];

        // The second block should be the inner table
        let inner_table = match &outer_cell.blocks[1] {
            BlockNode::Table(t) => t,
            _ => panic!("expected inner table at block index 1"),
        };

        // Get the paragraph in the inner table's first cell
        let inner_para = match &inner_table.rows[0].cells[0].blocks[0] {
            BlockNode::Paragraph(p) => p,
            _ => panic!("expected paragraph in inner table cell"),
        };

        // The inner paragraph should have tracked changes (not just old text unchanged)
        let has_tracked_changes = inner_para.segments.iter().any(|s| {
            matches!(
                s.status,
                TrackingStatus::Inserted(_) | TrackingStatus::Deleted(_)
            )
        });

        assert!(
            has_tracked_changes,
            "inner table paragraph should have tracked changes (ins/del segments), \
             but found only: {:?}",
            inner_para
                .segments
                .iter()
                .map(|s| format!(
                    "{:?}: {}",
                    s.status,
                    s.inlines
                        .iter()
                        .filter_map(|i| match i {
                            InlineNode::Text(t) => Some(t.text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("")
                ))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn merge_diff_generates_sect_pr_change_on_margin_change() {
        use crate::domain::SectionProperties;

        let base_sp = SectionProperties {
            page_width: Some(12240),
            page_height: Some(15840),
            orientation: None,
            columns: None,
            column_space: None,
            column_defs: Vec::new(),
            margin_top: Some(1440),
            margin_bottom: Some(1440),
            margin_left: Some(1440),
            margin_right: Some(1440),
            header_distance: None,
            footer_distance: None,
            gutter: None,
            rtl_gutter: None,
            section_type: None,
            page_borders: None,
            line_numbering: None,
            v_align: None,
            text_direction: None,
            page_number_type: None,
            doc_grid_type: None,
            doc_grid_line_pitch: None,
            doc_grid_char_space: None,
            title_page: None,
            bidi: None,
            form_prot: None,
            no_endnote: None,
            paper_size_code: None,
            column_separator: None,
            equal_width: None,
            footnote_pr: None,
            endnote_pr: None,
            header_refs: Vec::new(),
            footer_refs: Vec::new(),
            paper_source: None,
            printer_settings_rid: None,
        };

        let mut target_sp = base_sp.clone();
        target_sp.margin_top = Some(720); // Changed from 1440 to 720

        let mut base = make_doc(vec![make_paragraph("p1", "hello")]);
        base.body_section_properties = Some(base_sp.clone());

        let mut target = make_doc(vec![make_paragraph("p1", "hello")]);
        target.body_section_properties = Some(target_sp.clone());

        let diff = crate::diff::diff_documents(&base, &target).expect("diff should succeed");

        let revision = RevisionInfo {
            revision_id: 1,
            identity: 1,
            author: Some("test".to_string()),
            date: Some("2024-01-01T00:00:00Z".to_string()),
            apply_op_id: None,
        };

        let merged = merge_diff(&base, &target, &diff, &revision)
            .expect("merge should succeed")
            .doc;

        // The merged doc should have target's section properties
        assert_eq!(
            merged.body_section_properties,
            Some(target_sp),
            "merged doc should have target section properties"
        );

        // The merged doc should have a sectPrChange recording the base state
        assert!(
            merged.body_section_property_change.is_some(),
            "merged doc should have body_section_property_change when margins differ"
        );

        let change = merged.body_section_property_change.unwrap();
        assert_eq!(change.revision.author.as_deref(), Some("test"));
        assert!(
            !change.previous_properties_raw.is_empty(),
            "previous_properties_raw should contain the base sectPr XML"
        );
    }

    /// When a nested table has rows added/deleted (structure changed), the merge
    /// should produce row-level tracking on the inner table AND accept_all should
    /// produce output matching the target.
    #[test]
    fn nested_table_structure_changed_accept_all_projects_correctly() {
        use crate::domain::{
            CellFormatting, TableCellNode, TableFormatting, TableNode, TableRowNode, VerticalMerge,
        };

        fn make_simple_cell(id: &str, blocks: Vec<BlockNode>) -> TableCellNode {
            TableCellNode {
                id: NodeId::from(id.to_string()),
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
            }
        }

        fn make_table(id: &str, rows: Vec<Vec<TableCellNode>>, hash: &str) -> TableNode {
            TableNode {
                id: NodeId::from(id.to_string()),
                rows: rows
                    .into_iter()
                    .enumerate()
                    .map(|(r, cells)| TableRowNode {
                        id: NodeId::from(format!("{id}_r{r}")),
                        cells,
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
                structure_hash: hash.to_string(),
                formatting: TableFormatting::default(),
                formatting_change: None,
            }
        }

        // Base inner table: 2 rows ("row A", "row B")
        let inner_base = make_table(
            "inner",
            vec![
                vec![make_simple_cell(
                    "ic0",
                    vec![make_paragraph("ip0", "row A")],
                )],
                vec![make_simple_cell(
                    "ic1",
                    vec![make_paragraph("ip1", "row B")],
                )],
            ],
            "inner_hash_2row",
        );

        // Target inner table: 3 rows ("row A", "row B changed", "row C" new)
        let inner_target = make_table(
            "inner",
            vec![
                vec![make_simple_cell(
                    "ic0",
                    vec![make_paragraph("ip0", "row A")],
                )],
                vec![make_simple_cell(
                    "ic1",
                    vec![make_paragraph("ip1", "row B changed")],
                )],
                vec![make_simple_cell(
                    "ic2",
                    vec![make_paragraph("ip2", "row C")],
                )],
            ],
            "inner_hash_3row",
        );

        // Outer table (same structure, both base and target have 1 row x 1 cell)
        let outer_base = make_table(
            "outer",
            vec![vec![make_simple_cell(
                "oc0",
                vec![
                    make_paragraph("op0", "heading"),
                    BlockNode::from(inner_base),
                ],
            )]],
            "outer_hash",
        );

        let outer_target = make_table(
            "outer",
            vec![vec![make_simple_cell(
                "oc0",
                vec![
                    make_paragraph("op0", "heading"),
                    BlockNode::from(inner_target),
                ],
            )]],
            "outer_hash",
        );

        let base = make_doc(vec![BlockNode::from(outer_base)]);
        let target = make_doc(vec![BlockNode::from(outer_target)]);

        let diff = crate::diff::diff_documents(&base, &target).expect("diff should succeed");

        let revision = RevisionInfo {
            revision_id: 1,
            identity: 1,
            author: Some("test".to_string()),
            date: Some("2024-01-01T00:00:00Z".to_string()),
            apply_op_id: None,
        };

        let merged = merge_diff(&base, &target, &diff, &revision)
            .expect("merge should succeed")
            .doc;

        // The merged inner table should have tracked changes (inserted/deleted rows
        // or inline tracked segments within matched cells).
        let outer_table = match &merged.blocks[0].block {
            BlockNode::Table(t) => t,
            _ => panic!("expected outer table"),
        };
        let inner_table = match &outer_table.rows[0].cells[0].blocks[1] {
            BlockNode::Table(t) => t,
            _ => panic!("expected inner table at block index 1"),
        };

        let has_any_tracking = inner_table.rows.iter().any(|r| {
            r.tracking_status.is_some()
                || r.cells.iter().any(|c| {
                    c.blocks.iter().any(|b| {
                        if let BlockNode::Paragraph(p) = b {
                            p.segments.iter().any(|s| {
                                matches!(
                                    s.status,
                                    TrackingStatus::Inserted(_) | TrackingStatus::Deleted(_)
                                )
                            })
                        } else {
                            false
                        }
                    })
                })
        });
        assert!(
            has_any_tracking,
            "merged inner table should have tracked changes"
        );

        // Accept-all should project the table correctly:
        // - Keep inserted rows (clear tracking status)
        // - Remove deleted rows
        // - For matched rows with inline changes, keep inserted text and remove deleted
        let mut accepted = merged.clone();
        accept_all(&mut accepted);

        let accepted_outer = match &accepted.blocks[0].block {
            BlockNode::Table(t) => t,
            _ => panic!("expected outer table after accept_all"),
        };
        let accepted_inner = match &accepted_outer.rows[0].cells[0].blocks[1] {
            BlockNode::Table(t) => t,
            _ => panic!("expected inner table after accept_all"),
        };

        // All rows should have None tracking status after accept.
        for (i, row) in accepted_inner.rows.iter().enumerate() {
            assert!(
                row.tracking_status.is_none(),
                "accepted row {i} should have no tracking_status, got: {:?}",
                row.tracking_status
            );
            // Cell tracking status should also be cleared.
            for (ci, cell) in row.cells.iter().enumerate() {
                assert!(
                    cell.tracking_status.is_none(),
                    "accepted row {i} cell {ci} should have no tracking_status, got: {:?}",
                    cell.tracking_status
                );
            }
        }
    }

    /// OOXML §17.4.37: tables must have a non-zero number of rows.
    /// When accept/reject drops all rows from a table, the table block
    /// must be removed entirely rather than leaving a spec-invalid 0-row table.
    #[test]
    fn reject_all_removes_table_with_all_inserted_rows() {
        use crate::domain::{
            CellFormatting, TableCellNode, TableFormatting, TableNode, TableRowNode, VerticalMerge,
        };

        let revision = RevisionInfo {
            revision_id: 1,
            identity: 1,
            author: Some("test".to_string()),
            date: Some("2026-01-01T00:00:00Z".to_string()),
            apply_op_id: None,
        };

        let make_inserted_row = |id: &str, cell_id: &str, para_id: &str, text: &str| TableRowNode {
            id: NodeId::from(id.to_string()),
            cells: vec![TableCellNode {
                id: NodeId::from(cell_id.to_string()),
                blocks: vec![make_paragraph(para_id, text)],
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
            tracking_status: Some(TrackingStatus::Inserted(revision.clone())),
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
        };

        let table = BlockNode::from(TableNode {
            id: NodeId::from("tbl_0"),
            rows: vec![
                make_inserted_row("tbl_0_r0", "c00", "p_c00", "cell A"),
                make_inserted_row("tbl_0_r1", "c10", "p_c10", "cell B"),
            ],
            formatting: TableFormatting::default(),
            formatting_change: None,
            structure_hash: String::new(),
        });

        let mut doc = make_doc(vec![
            make_paragraph("p0", "before table"),
            table,
            make_paragraph("p1", "after table"),
        ]);

        reject_all_with_styles(&mut doc, None);

        // Both rows were inserted → rejected → table has 0 rows → table removed.
        assert_eq!(
            doc.blocks.len(),
            2,
            "should have 2 blocks (paragraphs only), got {}",
            doc.blocks.len()
        );
        for tb in &doc.blocks {
            assert!(
                matches!(&tb.block, BlockNode::Paragraph(_)),
                "remaining blocks should be paragraphs, not tables"
            );
        }
    }

    /// ISO 29500-1 §17.13.5: each w:ins/w:del element must have a unique w:id.
    /// A paragraph with 3+ inline changes must produce tracked segments with
    /// distinct revision IDs — no two tracked segments may share the same ID.
    #[test]
    fn inline_changes_produce_unique_revision_ids() {
        let base = make_doc(vec![make_paragraph(
            "p1",
            "The quick brown fox jumps over the lazy dog",
        )]);
        let target = make_doc(vec![make_paragraph(
            "p1",
            "The slow red fox leaps over the sleepy dog",
        )]);

        // "quick" deleted, "slow" inserted, "brown" deleted, "red" inserted,
        // "jumps" deleted, "leaps" inserted, "lazy" deleted, "sleepy" inserted.
        let diff = DocumentDiff {
            base_fingerprint: base.meta.docx_fingerprint.clone(),
            target_fingerprint: target.meta.docx_fingerprint.clone(),
            changes: vec![DiffChange::BlockModified {
                block_id: NodeId::from("p1"),
                old_text: "The quick brown fox jumps over the lazy dog".to_string(),
                new_text: "The slow red fox leaps over the sleepy dog".to_string(),
                inline_changes: vec![
                    InlineChange::Unchanged {
                        text: "The ".to_string(),
                        marks: vec![],
                        style_props: StyleProps::default(),
                        formatting_change: None,
                    },
                    InlineChange::Deleted {
                        text: "quick brown".to_string(),
                        marks: vec![],
                        style_props: StyleProps::default(),
                        formatting_change: None,
                        rev_id: 0,
                    },
                    InlineChange::Inserted {
                        text: "slow red".to_string(),
                        marks: vec![],
                        style_props: StyleProps::default(),
                        formatting_change: None,
                        rev_id: 0,
                    },
                    InlineChange::Unchanged {
                        text: " fox ".to_string(),
                        marks: vec![],
                        style_props: StyleProps::default(),
                        formatting_change: None,
                    },
                    InlineChange::Deleted {
                        text: "jumps".to_string(),
                        marks: vec![],
                        style_props: StyleProps::default(),
                        formatting_change: None,
                        rev_id: 0,
                    },
                    InlineChange::Inserted {
                        text: "leaps".to_string(),
                        marks: vec![],
                        style_props: StyleProps::default(),
                        formatting_change: None,
                        rev_id: 0,
                    },
                    InlineChange::Unchanged {
                        text: " over the ".to_string(),
                        marks: vec![],
                        style_props: StyleProps::default(),
                        formatting_change: None,
                    },
                    InlineChange::Deleted {
                        text: "lazy".to_string(),
                        marks: vec![],
                        style_props: StyleProps::default(),
                        formatting_change: None,
                        rev_id: 0,
                    },
                    InlineChange::Inserted {
                        text: "sleepy".to_string(),
                        marks: vec![],
                        style_props: StyleProps::default(),
                        formatting_change: None,
                        rev_id: 0,
                    },
                    InlineChange::Unchanged {
                        text: " dog".to_string(),
                        marks: vec![],
                        style_props: StyleProps::default(),
                        formatting_change: None,
                    },
                ],
                old_block: make_paragraph("p1", "The quick brown fox jumps over the lazy dog"),
                new_block: make_paragraph("p1", "The slow red fox leaps over the sleepy dog"),
                para_split: false,
            }],
        };

        let revision = RevisionInfo {
            revision_id: 100,
            identity: 100,
            author: Some("test-author".to_string()),
            date: Some("2026-03-12T00:00:00Z".to_string()),
            apply_op_id: None,
        };

        let merged = merge_diff(&base, &target, &diff, &revision)
            .expect("merge must succeed")
            .doc;
        let paragraph = match &merged.blocks[0].block {
            BlockNode::Paragraph(p) => p,
            _ => panic!("expected paragraph"),
        };

        // Collect revision IDs from all tracked (non-Normal) segments.
        let tracked_rev_ids: Vec<u32> = paragraph
            .segments
            .iter()
            .filter_map(|seg| match &seg.status {
                TrackingStatus::Inserted(rev) | TrackingStatus::Deleted(rev) => {
                    Some(rev.revision_id)
                }
                TrackingStatus::InsertedThenDeleted(sr) => Some(sr.deleted.revision_id),
                TrackingStatus::Normal => None,
            })
            .collect();

        // We have 6 tracked changes (3 deletions + 3 insertions).
        assert!(
            tracked_rev_ids.len() >= 6,
            "expected at least 6 tracked segments, got {}",
            tracked_rev_ids.len()
        );

        // Every revision ID must be unique.
        let unique: std::collections::HashSet<u32> = tracked_rev_ids.iter().copied().collect();
        assert_eq!(
            unique.len(),
            tracked_rev_ids.len(),
            "revision IDs must be unique across tracked segments, but found duplicates: {:?}",
            tracked_rev_ids
        );
    }

    /// Regression: revision IDs must be unique across multiple
    /// modified paragraphs in the same document, not just within a single
    /// paragraph. Before the fix, each `apply_block_modified` call reset
    /// its counter to `revision.revision_id`, causing collisions.
    #[test]
    fn revision_ids_unique_across_multiple_paragraphs() {
        let base = make_doc(vec![
            make_paragraph("p1", "hello world"),
            make_paragraph("p2", "foo bar baz"),
        ]);
        let target = make_doc(vec![
            make_paragraph("p1", "hello brave world"),
            make_paragraph("p2", "foo quux baz"),
        ]);
        let diff = DocumentDiff {
            base_fingerprint: base.meta.docx_fingerprint.clone(),
            target_fingerprint: target.meta.docx_fingerprint.clone(),
            changes: vec![
                DiffChange::BlockModified {
                    block_id: NodeId::from("p1"),
                    old_text: "hello world".to_string(),
                    new_text: "hello brave world".to_string(),
                    inline_changes: vec![
                        InlineChange::Unchanged {
                            text: "hello ".to_string(),
                            marks: vec![],
                            style_props: StyleProps::default(),
                            formatting_change: None,
                        },
                        InlineChange::Inserted {
                            text: "brave ".to_string(),
                            marks: vec![],
                            style_props: StyleProps::default(),
                            formatting_change: None,
                            rev_id: 0,
                        },
                        InlineChange::Unchanged {
                            text: "world".to_string(),
                            marks: vec![],
                            style_props: StyleProps::default(),
                            formatting_change: None,
                        },
                    ],
                    old_block: make_paragraph("p1", "hello world"),
                    new_block: make_paragraph("p1", "hello brave world"),
                    para_split: false,
                },
                DiffChange::BlockModified {
                    block_id: NodeId::from("p2"),
                    old_text: "foo bar baz".to_string(),
                    new_text: "foo quux baz".to_string(),
                    inline_changes: vec![
                        InlineChange::Unchanged {
                            text: "foo ".to_string(),
                            marks: vec![],
                            style_props: StyleProps::default(),
                            formatting_change: None,
                        },
                        InlineChange::Deleted {
                            text: "bar".to_string(),
                            marks: vec![],
                            style_props: StyleProps::default(),
                            formatting_change: None,
                            rev_id: 0,
                        },
                        InlineChange::Inserted {
                            text: "quux".to_string(),
                            marks: vec![],
                            style_props: StyleProps::default(),
                            formatting_change: None,
                            rev_id: 0,
                        },
                        InlineChange::Unchanged {
                            text: " baz".to_string(),
                            marks: vec![],
                            style_props: StyleProps::default(),
                            formatting_change: None,
                        },
                    ],
                    old_block: make_paragraph("p2", "foo bar baz"),
                    new_block: make_paragraph("p2", "foo quux baz"),
                    para_split: false,
                },
            ],
        };
        let revision = RevisionInfo {
            revision_id: 10,
            identity: 10,
            author: Some("test".to_string()),
            date: Some("2026-03-12T00:00:00Z".to_string()),
            apply_op_id: None,
        };

        let merged = merge_diff(&base, &target, &diff, &revision)
            .expect("merge")
            .doc;

        // Collect all revision IDs from every tracked segment in every paragraph.
        let mut all_rev_ids: Vec<u32> = Vec::new();
        for tb in &merged.blocks {
            if let BlockNode::Paragraph(p) = &tb.block {
                // Block-level tracking status
                if let TrackingStatus::Inserted(rev) | TrackingStatus::Deleted(rev) = &tb.status {
                    all_rev_ids.push(rev.revision_id);
                }
                // Segment-level tracking status
                for seg in &p.segments {
                    if let TrackingStatus::Inserted(rev) | TrackingStatus::Deleted(rev) =
                        &seg.status
                    {
                        all_rev_ids.push(rev.revision_id);
                    }
                }
                // Para mark status
                if let Some(TrackingStatus::Inserted(rev) | TrackingStatus::Deleted(rev)) =
                    &p.para_mark_status
                {
                    all_rev_ids.push(rev.revision_id);
                }
            }
        }

        // Should have at least 3 tracked changes (1 insert in p1 + 1 delete + 1 insert in p2).
        assert!(
            all_rev_ids.len() >= 3,
            "expected at least 3 tracked changes, got {}",
            all_rev_ids.len()
        );

        // Every revision ID must be unique across the entire document.
        let unique: std::collections::HashSet<u32> = all_rev_ids.iter().copied().collect();
        assert_eq!(
            unique.len(),
            all_rev_ids.len(),
            "revision IDs must be unique across all paragraphs, but found duplicates: {:?}",
            all_rev_ids
        );
    }

    /// Regression: revision IDs must be unique across block-level
    /// deletions, insertions, and inline modifications in the same diff.
    #[test]
    fn revision_ids_unique_across_block_delete_insert_and_modify() {
        let base = make_doc(vec![
            make_paragraph("p1", "first paragraph"),
            make_paragraph("p2", "second paragraph"),
            make_paragraph("p3", "third paragraph"),
        ]);
        let target = make_doc(vec![
            make_paragraph("p1", "first modified paragraph"),
            // p2 is deleted
            make_paragraph("p3", "third paragraph"),
            make_paragraph("p4", "new fourth paragraph"),
        ]);
        let diff = DocumentDiff {
            base_fingerprint: base.meta.docx_fingerprint.clone(),
            target_fingerprint: target.meta.docx_fingerprint.clone(),
            changes: vec![
                DiffChange::BlockModified {
                    block_id: NodeId::from("p1"),
                    old_text: "first paragraph".to_string(),
                    new_text: "first modified paragraph".to_string(),
                    inline_changes: vec![
                        InlineChange::Unchanged {
                            text: "first ".to_string(),
                            marks: vec![],
                            style_props: StyleProps::default(),
                            formatting_change: None,
                        },
                        InlineChange::Inserted {
                            text: "modified ".to_string(),
                            marks: vec![],
                            style_props: StyleProps::default(),
                            formatting_change: None,
                            rev_id: 0,
                        },
                        InlineChange::Unchanged {
                            text: "paragraph".to_string(),
                            marks: vec![],
                            style_props: StyleProps::default(),
                            formatting_change: None,
                        },
                    ],
                    old_block: make_paragraph("p1", "first paragraph"),
                    new_block: make_paragraph("p1", "first modified paragraph"),
                    para_split: false,
                },
                DiffChange::BlockDeleted {
                    block_id: NodeId::from("p2"),
                    old_text: "second paragraph".to_string(),
                    old_block: make_paragraph("p2", "second paragraph"),
                    move_id: None,
                },
                DiffChange::BlockInserted {
                    after_block_id: Some(NodeId::from("p3")),
                    block: make_paragraph("p4", "new fourth paragraph"),
                    move_id: None,
                },
            ],
        };
        let revision = RevisionInfo {
            revision_id: 1,
            identity: 1,
            author: Some("test".to_string()),
            date: Some("2026-03-12T00:00:00Z".to_string()),
            apply_op_id: None,
        };

        let merged = merge_diff(&base, &target, &diff, &revision)
            .expect("merge")
            .doc;

        // Collect ALL revision IDs from the entire merged document.
        let mut all_rev_ids: Vec<u32> = Vec::new();
        for tb in &merged.blocks {
            if let TrackingStatus::Inserted(rev) | TrackingStatus::Deleted(rev) = &tb.status {
                all_rev_ids.push(rev.revision_id);
            }
            if let BlockNode::Paragraph(p) = &tb.block {
                for seg in &p.segments {
                    if let TrackingStatus::Inserted(rev) | TrackingStatus::Deleted(rev) =
                        &seg.status
                    {
                        all_rev_ids.push(rev.revision_id);
                    }
                }
                if let Some(TrackingStatus::Inserted(rev) | TrackingStatus::Deleted(rev)) =
                    &p.para_mark_status
                {
                    all_rev_ids.push(rev.revision_id);
                }
            }
        }

        // Should have at least 3: 1 inline insert in p1, block delete for p2
        // (with para_mark), block insert for p4.
        assert!(
            all_rev_ids.len() >= 3,
            "expected at least 3 tracked changes, got {} => {:?}",
            all_rev_ids.len(),
            all_rev_ids
        );

        // Every revision ID must be unique.
        let unique: std::collections::HashSet<u32> = all_rev_ids.iter().copied().collect();
        assert_eq!(
            unique.len(),
            all_rev_ids.len(),
            "revision IDs must be unique across block-level and inline changes, but found duplicates: {:?}",
            all_rev_ids
        );
    }

    #[test]
    fn block_fingerprint_differentiates_num_id() {
        // Two paragraphs with the same literal_prefix ("i.") but different num_id
        // must produce different fingerprints. Previously they collided because
        // the fingerprint used only ilvl + text, missing num_id.
        let mut p1 = match make_paragraph("p1", "") {
            BlockNode::Paragraph(p) => p,
            _ => unreachable!(),
        };
        p1.literal_prefix = Some("i.".to_string());
        p1.numbering = Some(NumberingInfo {
            num_id: 1,
            ilvl: 0,
            synthesized_text: "i.".to_string(),
            is_bullet: false,
            restart_numbering: false,
        });

        let mut p2 = match make_paragraph("p2", "") {
            BlockNode::Paragraph(p) => p,
            _ => unreachable!(),
        };
        p2.literal_prefix = Some("i.".to_string());
        p2.numbering = Some(NumberingInfo {
            num_id: 2,
            ilvl: 0,
            synthesized_text: "i.".to_string(),
            is_bullet: false,
            restart_numbering: false,
        });

        let fp1 = block_fingerprint(&BlockNode::Paragraph(p1));
        let fp2 = block_fingerprint(&BlockNode::Paragraph(p2));
        assert_ne!(
            fp1, fp2,
            "paragraphs with different num_id must produce different fingerprints: {fp1} vs {fp2}"
        );
        // Both should contain their num_id
        assert!(
            fp1.starts_with("P:1:0:"),
            "fingerprint should include num_id: {fp1}"
        );
        assert!(
            fp2.starts_with("P:2:0:"),
            "fingerprint should include num_id: {fp2}"
        );
    }

    #[test]
    fn block_fingerprint_falls_back_to_literal_prefix() {
        // A paragraph with empty inline text but a literal_prefix should use
        // the prefix in the fingerprint, not empty string.
        let mut p = match make_paragraph("p1", "") {
            BlockNode::Paragraph(p) => p,
            _ => unreachable!(),
        };
        p.literal_prefix = Some("i.".to_string());

        let fp = block_fingerprint(&BlockNode::Paragraph(p));
        assert!(
            fp.contains("i."),
            "fingerprint should fall back to literal_prefix when inline text is empty: {fp}"
        );
    }

    #[test]
    fn block_fingerprint_falls_back_to_rendered_text() {
        // rendered_text takes priority over literal_prefix when inline text is empty.
        let mut p = match make_paragraph("p1", "") {
            BlockNode::Paragraph(p) => p,
            _ => unreachable!(),
        };
        p.rendered_text = Some("i.\tsome body".to_string());
        p.literal_prefix = Some("i.".to_string());

        let fp = block_fingerprint(&BlockNode::Paragraph(p));
        assert!(
            fp.contains("i.\tsome body"),
            "fingerprint should prefer rendered_text over literal_prefix: {fp}"
        );
    }

    /// Spec: ECMA-376 §17.4.84 (vMerge). A `<w:vMerge w:val="continue"/>` cell
    /// is only valid when a `<w:vMerge w:val="restart"/>` anchor exists above it
    /// in the same column. When the TableStructureChanged merge path matches a
    /// base row to a target row, the merged cell must adopt the *target* cell's
    /// structural merge attributes (gridSpan / vMerge). Otherwise a target
    /// restart anchor introduced on a matched row is silently dropped while the
    /// continue cells on the inserted rows below survive — producing an orphan
    /// continue that canonicalize_table rejects.
    ///
    /// Regression for the edgar-ameren / edgar-nwpx fixpoint failures
    /// ("<w:vMerge/> continue ... has no preceding restart anchor").
    #[test]
    fn spec_structure_changed_matched_row_adopts_target_vmerge_anchor() {
        use crate::domain::{
            CellFormatting, TableCellNode, TableFormatting, TableRowNode, VerticalMerge,
        };
        use crate::table::canonicalize_table;

        fn cell(id: &str, text: &str, v_merge: VerticalMerge) -> TableCellNode {
            TableCellNode {
                id: NodeId::from(id.to_string()),
                blocks: vec![make_paragraph(&format!("{id}_p"), text)],
                grid_span: 1,
                v_merge,
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

        fn row(id: &str, cells: Vec<TableCellNode>) -> TableRowNode {
            TableRowNode {
                id: NodeId::from(id.to_string()),
                cells,
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
            }
        }

        // Base: one row, single column, no vertical merge.
        let base_table = TableNode {
            id: NodeId::from("tbl_x"),
            rows: vec![row("r0", vec![cell("b0", "Anchor", VerticalMerge::None)])],
            structure_hash: "base".to_string(),
            formatting: TableFormatting::default(),
            formatting_change: None,
        };

        // Target: the matched row's cell becomes a restart anchor, and a new
        // row below continues the vertical merge.
        let target_table = TableNode {
            id: NodeId::from("tbl_x"),
            rows: vec![
                row("r0", vec![cell("t0", "Anchor", VerticalMerge::Restart)]),
                row("r1", vec![cell("t1", "", VerticalMerge::Continue)]),
            ],
            structure_hash: "target".to_string(),
            formatting: TableFormatting::default(),
            formatting_change: None,
        };

        let diff = TableDiffResult {
            old_table: canonicalize_table(&base_table).expect("canon base"),
            new_table: canonicalize_table(&target_table).expect("canon target"),
            row_alignment: vec![
                TableRowAlignment::Matched {
                    old_row: 0,
                    new_row: 0,
                },
                TableRowAlignment::Inserted { new_row: 1 },
            ],
            cell_diffs: Vec::new(),
        };

        let mut blocks = vec![TrackedBlock {
            status: TrackingStatus::Normal,
            block: BlockNode::from(base_table),
            move_id: None,
            block_sdt_wrap: None,
        }];
        let revision = RevisionInfo {
            revision_id: 1,
            identity: 1,
            author: Some("tester".to_string()),
            date: Some("2026-05-31T00:00:00Z".to_string()),
            apply_op_id: None,
        };
        let mut rev_counter = 2;

        apply_table_structure_changed(
            &mut blocks,
            &NodeId::from("tbl_x"),
            &target_table,
            &diff,
            &revision,
            &mut rev_counter,
            "test",
        )
        .expect("structure change should apply");

        let merged = match &blocks[0].block {
            BlockNode::Table(t) => t,
            _ => panic!("expected table"),
        };

        // The matched-row cell must carry the target's restart anchor.
        assert_eq!(
            merged.rows[0].cells[0].v_merge,
            VerticalMerge::Restart,
            "matched row cell must adopt target vMerge=restart anchor"
        );
        assert_eq!(
            merged.rows[1].cells[0].v_merge,
            VerticalMerge::Continue,
            "inserted row cell keeps target vMerge=continue"
        );

        // And the merged table must canonicalize without an orphan-continue error.
        canonicalize_table(merged).expect("merged table must have a valid vMerge grid");
    }

    // ─── RFC-0004 §H7: engine-minted revision identity ───────────────────────

    /// A pre-identity paragraph formatting change (pPrChange) carrying the given
    /// wire id, all "previous" fields empty — the minimal witness of a
    /// `w:pPrChange` carrier for the collision test. `identity: 0` so the import
    /// mint walk assigns its real identity.
    fn h7_bare_ppr_change(wire_id: u32, author: &str) -> crate::domain::ParagraphFormattingChange {
        crate::domain::ParagraphFormattingChange {
            previous_alignment: None,
            previous_indentation: None,
            previous_spacing: None,
            previous_numbering: None,
            previous_numbering_explicitly_absent: false,
            previous_style_id: None,
            previous_keep_next: None,
            previous_keep_lines: None,
            previous_page_break_before: false,
            previous_widow_control: None,
            previous_contextual_spacing: None,
            previous_shading: None,
            previous_borders: None,
            previous_tab_stops: vec![],
            previous_literal_prefix_leading_tab_twips: None,
            previous_literal_prefix_trailing_tab_stop_twips: None,
            previous_paragraph_mark_marks: vec![],
            previous_paragraph_mark_style_props: StyleProps::default(),
            previous_paragraph_mark_rpr_off: Default::default(),
            previous_text_direction: None,
            previous_text_alignment: None,
            previous_mirror_indents: None,
            previous_auto_space_de: None,
            previous_auto_space_dn: None,
            previous_bidi: None,
            previous_suppress_auto_hyphens: None,
            previous_snap_to_grid: None,
            previous_overflow_punct: None,
            previous_adjust_right_ind: None,
            previous_word_wrap: None,
            previous_frame_pr: None,
            previous_preserved_ppr: vec![],
            revision_id: wire_id,
            identity: 0,
            author: author.to_string(),
            date: None,
        }
    }

    fn h7_rev(wire_id: u32, author: &str) -> RevisionInfo {
        RevisionInfo {
            revision_id: wire_id,
            identity: 0,
            author: Some(author.to_string()),
            date: None,
            apply_op_id: None,
        }
    }

    fn h7_para_block(id: &str, text: &str) -> Box<ParagraphNode> {
        match make_paragraph(id, text) {
            BlockNode::Paragraph(p) => p,
            _ => unreachable!(),
        }
    }

    /// A three-block document reproducing the wild collision the H7 triage found
    /// (`legal__0f53c780`): ONE wire `w:id` (8) carried by BOTH a MOVE's source
    /// paragraph-mark AND an unrelated `w:pPrChange` on a different paragraph.
    /// Built pre-mint (every `identity == 0`); the caller runs the real import
    /// mint walk.
    fn h7_collision_doc() -> CanonDoc {
        // Move SOURCE: content wire 7, paragraph-mark wire 8, move group "mv1".
        let mut src_para = h7_para_block("src", "Moved");
        src_para.segments = vec![tracked_text_segment(
            "src_t",
            "Moved",
            TrackingStatus::Deleted(h7_rev(7, "Mover")),
        )];
        src_para.para_mark_status = Some(TrackingStatus::Deleted(h7_rev(8, "Mover")));
        let src = TrackedBlock {
            status: TrackingStatus::Normal,
            block: BlockNode::Paragraph(src_para),
            move_id: Some("mv1".to_string()),
            block_sdt_wrap: None,
        };
        // Move DESTINATION clone: content wire 9, same move group.
        let mut dst_para = h7_para_block("src__ins1", "Moved");
        dst_para.segments = vec![tracked_text_segment(
            "dst_t",
            "Moved",
            TrackingStatus::Inserted(h7_rev(9, "Mover")),
        )];
        let dst = TrackedBlock {
            status: TrackingStatus::Normal,
            block: BlockNode::Paragraph(dst_para),
            move_id: Some("mv1".to_string()),
            block_sdt_wrap: None,
        };
        // Unrelated pPrChange COLLIDING on wire id 8 (different author).
        let mut fmt_para = h7_para_block("fmt", "Formatted");
        fmt_para.formatting_change = Some(h7_bare_ppr_change(8, "Formatter"));
        let fmt = normal_tracked_block(BlockNode::Paragraph(fmt_para));

        let mut doc = make_doc(vec![]);
        doc.blocks = vec![src, dst, fmt];
        doc
    }

    /// Acceptance 1: two unrelated revisions sharing ONE wire `w:id` (8) — a
    /// move paragraph-mark and a pPrChange — enumerate as TWO DISTINCT minted
    /// identities, and `Selective` on one resolves ONLY that one.
    ///
    /// Sentinel: the two identities are minted independently of the shared wire
    /// id 8 — the pre-H7 model (identity == wire id) would give both carriers id
    /// 8 and a single selection would hit both.
    #[test]
    fn h7_wire_id_collision_yields_distinct_identities_and_isolated_resolution() {
        let mut doc = h7_collision_doc();
        crate::import::mint_identities(&mut doc);

        let records = enumerate_revisions(&doc);
        let move_rec = records
            .iter()
            .find(|r| r.kind == RevisionKind::Move)
            .expect("the move enumerates as one Move record");
        let ppr_rec = records
            .iter()
            .find(|r| r.kind == RevisionKind::FormatParagraph)
            .expect("the pPrChange enumerates as a FormatParagraph record");

        let move_id = move_rec.revision_id;
        let ppr_id = ppr_rec.revision_id;
        assert_ne!(
            move_id, ppr_id,
            "the move and the pPrChange share wire id 8 but must get DISTINCT identities"
        );
        assert_ne!(move_id, 8, "identity is minted, not the wire id");
        assert_ne!(ppr_id, 8, "identity is minted, not the wire id");

        // Selective-accept ONLY the pPrChange identity.
        resolve_selected_revisions_with_styles(
            &mut doc,
            &HashSet::from([ppr_id]),
            ResolveSelectionAction::Accept,
            None,
        )
        .expect("resolving the pPrChange identity alone must succeed");

        let after = enumerate_revisions(&doc);
        assert!(
            after.iter().any(|r| r.revision_id == move_id),
            "the move must STILL be pending after resolving only the colliding pPrChange \
             (pre-H7 the shared wire id 8 would have resolved both)"
        );
        assert!(
            !after.iter().any(|r| r.revision_id == ppr_id),
            "the pPrChange must be resolved (gone)"
        );
    }

    /// Acceptance 2: a move enumerates as ONE record whose several wire carriers
    /// (source content + source pilcrow + destination clone) all share ONE
    /// minted identity.
    #[test]
    fn h7_move_enumerates_as_one_record_with_shared_identity() {
        let mut doc = h7_collision_doc();
        crate::import::mint_identities(&mut doc);

        let records = enumerate_revisions(&doc);
        let move_recs: Vec<_> = records
            .iter()
            .filter(|r| r.kind == RevisionKind::Move)
            .collect();
        assert_eq!(
            move_recs.len(),
            1,
            "the move's content + pilcrow + clone collapse to ONE Move record: {records:?}"
        );

        // All three carriers (wire 7, 8, 9) carry the SAME minted identity.
        let move_identity = move_recs[0].revision_id;
        let mut carrier_identities = std::collections::HashSet::new();
        crate::import::for_each_rev_carrier_mut(&mut doc, &mut |c| {
            if c.move_group.as_deref() == Some("mv1") {
                carrier_identities.insert(*c.identity);
            }
        });
        assert_eq!(
            carrier_identities,
            HashSet::from([move_identity]),
            "every carrier of the move group shares the one move identity"
        );
    }

    /// Acceptance 3, companion: the SAME identity-stability guarantee for a
    /// NON-move fully-deleted paragraph. Its content-delete and paragraph-mark
    /// delete share one intention (one `w:del` id → one identity); when
    /// re-projection canonicalizes the fully-deleted paragraph into a whole-block
    /// `Deleted`, that identity must survive — the exact enforcement the move
    /// case gets, proven here for a plain delete so the guarantee is not
    /// move-specific.
    #[test]
    fn h7_non_move_whole_block_delete_keeps_identity_across_reprojection() {
        // Block 0: a non-move paragraph fully deleted — content + paragraph mark
        // both Deleted under ONE wire id (5), same author/date → one identity.
        let mut del_para = h7_para_block("del", "gone");
        del_para.segments = vec![tracked_text_segment(
            "del_t",
            "gone",
            TrackingStatus::Deleted(h7_rev(5, "Del")),
        )];
        del_para.para_mark_status = Some(TrackingStatus::Deleted(h7_rev(5, "Del")));
        let del_block = normal_tracked_block(BlockNode::Paragraph(del_para));
        // Block 1: an unrelated pending insert we will resolve to force a
        // re-projection over the whole document.
        let mut ins_para = h7_para_block("ins", "added");
        ins_para.segments = vec![tracked_text_segment(
            "ins_t",
            "added",
            TrackingStatus::Inserted(h7_rev(6, "Ins")),
        )];
        let ins_block = normal_tracked_block(BlockNode::Paragraph(ins_para));

        let mut doc = make_doc(vec![]);
        doc.blocks = vec![del_block, ins_block];
        crate::import::mint_identities(&mut doc);

        let del_id = enumerate_revisions(&doc)
            .iter()
            .find(|r| r.kind == RevisionKind::Delete)
            .expect("the deleted paragraph enumerates")
            .revision_id;
        let ins_id = enumerate_revisions(&doc)
            .iter()
            .find(|r| r.kind == RevisionKind::Insert)
            .expect("the insert enumerates")
            .revision_id;
        assert_ne!(del_id, 0);
        assert_ne!(del_id, ins_id);

        // Resolve ONLY the unrelated insert; the deleted paragraph is untouched.
        resolve_selected_revisions_with_styles(
            &mut doc,
            &HashSet::from([ins_id]),
            ResolveSelectionAction::Accept,
            None,
        )
        .expect("resolve the unrelated insert");

        let after: HashSet<u32> = enumerate_revisions(&doc)
            .iter()
            .map(|r| r.revision_id)
            .collect();
        assert!(
            after.contains(&del_id),
            "the non-move deleted paragraph must keep its identity {del_id} after re-projection \
             canonicalizes it to a whole-block Deleted; got {after:?}"
        );
        assert!(
            !after.contains(&ins_id),
            "the resolved insert's id disappears"
        );
    }

    /// Acceptance 3: enumerate → selectively resolve a NON-move subset →
    /// re-enumerate: every still-pending revision keeps its minted identity, the
    /// resolved one disappears, and NO identity changes value or splits/coalesces
    /// (this is the W5-T4 granularity wart, killed by the shared move identity).
    #[test]
    fn h7_identity_is_stable_across_selective_reprojection() {
        let mut doc = h7_collision_doc();
        crate::import::mint_identities(&mut doc);

        let before: HashSet<u32> = enumerate_revisions(&doc)
            .iter()
            .map(|r| r.revision_id)
            .collect();
        let move_id = enumerate_revisions(&doc)
            .iter()
            .find(|r| r.kind == RevisionKind::Move)
            .expect("move record")
            .revision_id;
        let ppr_id = enumerate_revisions(&doc)
            .iter()
            .find(|r| r.kind == RevisionKind::FormatParagraph)
            .expect("pPrChange record")
            .revision_id;

        // Resolve ONLY the non-move revision.
        resolve_selected_revisions_with_styles(
            &mut doc,
            &HashSet::from([ppr_id]),
            ResolveSelectionAction::Reject,
            None,
        )
        .expect("resolve the pPrChange");

        let after: HashSet<u32> = enumerate_revisions(&doc)
            .iter()
            .map(|r| r.revision_id)
            .collect();

        assert!(
            after.contains(&move_id),
            "the still-pending move keeps its exact identity across re-projection"
        );
        assert!(
            !after.contains(&ppr_id),
            "the resolved pPrChange's id disappears"
        );
        // No id changed value or split/coalesced: `after` is `before` minus the
        // one resolved id, nothing more.
        let mut expected = before.clone();
        expected.remove(&ppr_id);
        assert_eq!(
            after, expected,
            "the enumerate id SET is stable: exactly the resolved id vanished, no id \
             re-valued or split/coalesced (before={before:?} after={after:?})"
        );
    }
}
