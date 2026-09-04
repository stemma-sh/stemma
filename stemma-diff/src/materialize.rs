use std::collections::{HashMap, HashSet};

use similar::{Algorithm, DiffOp};

use crate::domain::{
    BlockNode, BlockProvenance, CanonDoc, CellFormatting, CommentStory, DiffChange, EndnoteStory,
    FieldData, FieldKind, FooterStory, FootnoteStory, HeaderStory, InlineChange,
    InlineChangeSegmentType, InlineNode, Mark, MaterializedPrefixKind, NestedTableDiffKind, NodeId,
    OpaqueKind, ParagraphFormattingChange, ParagraphNode, RevisionInfo, RunRprAuthored,
    SectionProperties, SectionPropertyChange, StackedRevision, StyleProps, TableCellNode,
    TableDiffResult, TableNode, TableRowAlignment, TextNode, TextRole, TrackedBlock,
    TrackedSegment, TrackingStatus, materialized_prefix_node_id,
};
use crate::table::extract_inlines_text;
use crate::tracked_model::{
    debug_assert_body_invariants, materialize_block_tracked_prefixes, reject_paragraph_formatting,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeErrorKind {
    /// The inferred comparison requires a native carrier or composition that
    /// Stemma has not qualified. No output was produced.
    UnsupportedEdit,
    /// The compiler produced a plan the materializer could not apply to the
    /// validated source model. This is an internal correctness failure, not a
    /// user-facing capability boundary.
    InvalidPlan,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeError {
    pub kind: MergeErrorKind,
    pub message: String,
    pub context: String,
}

// Bound the physical population while keeping the limit above the qualified
// large-history witness. The previous 256 limit was inferred from an artifact
// that reused property-change wire IDs; Word's pathological behavior was not a
// valid population witness. A uniquely identified 600-carrier Stemma shape is
// qualified separately before this boundary is promoted.
const MAX_PROPERTY_HISTORY_CARRIERS_PER_STORY: usize = 1024;

/// Completes local before/after regions that a caller's edit plan leaves
/// unresolved.
///
/// The engine owns native tracked-change materialization, but it does not own
/// document comparison. A caller that inferred a whole-document change plan
/// must therefore supply the two local inference operations needed while
/// materializing nested table stories. Explicit-edit callers can provide the
/// same operations without introducing a dependency from the engine back to a
/// comparison subsystem.
pub trait ChangePlanResolver {
    fn paragraph_changes(&self, before: &[InlineNode], after: &[InlineNode]) -> Vec<InlineChange>;

    fn nested_table_change(
        &self,
        before: &TableNode,
        after: &TableNode,
        block_index: usize,
    ) -> Result<Option<crate::domain::NestedTableDiff>, String>;
}

pub(crate) struct ComparisonPlanResolver;

impl ChangePlanResolver for ComparisonPlanResolver {
    fn paragraph_changes(&self, before: &[InlineNode], after: &[InlineNode]) -> Vec<InlineChange> {
        crate::compiler::diff_block_content_resolving_opaques(before, after, &HashMap::new())
    }

    fn nested_table_change(
        &self,
        before: &TableNode,
        after: &TableNode,
        block_index: usize,
    ) -> Result<Option<crate::domain::NestedTableDiff>, String> {
        crate::compiler::diff_nested_tables(before, after, block_index)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CellContentRelation {
    Related,
    UnrelatedReplacement,
}

struct UnrelatedContentResolver<'a>(&'a dyn ChangePlanResolver);

impl ChangePlanResolver for UnrelatedContentResolver<'_> {
    fn paragraph_changes(&self, before: &[InlineNode], after: &[InlineNode]) -> Vec<InlineChange> {
        self.0
            .paragraph_changes(before, after)
            .into_iter()
            .flat_map(|change| match change {
                InlineChange::Unchanged {
                    text,
                    marks,
                    style_props,
                    formatting_change,
                } => {
                    let (previous_marks, previous_style_props) =
                        formatting_change.as_ref().map_or_else(
                            || (marks.clone(), style_props.clone()),
                            |change| {
                                (
                                    change.previous_marks.clone(),
                                    change.previous_style_props.clone(),
                                )
                            },
                        );
                    vec![
                        InlineChange::Deleted {
                            text: text.clone(),
                            marks: previous_marks,
                            style_props: previous_style_props,
                            formatting_change: None,
                            rev_id: 0,
                        },
                        InlineChange::Inserted {
                            text,
                            marks,
                            style_props,
                            formatting_change: None,
                            rev_id: 0,
                        },
                    ]
                }
                InlineChange::Opaque {
                    segment_type: InlineChangeSegmentType::Equal,
                    ..
                } => {
                    let mut deleted = change.clone();
                    let InlineChange::Opaque { segment_type, .. } = &mut deleted else {
                        unreachable!("matched opaque change must remain opaque")
                    };
                    *segment_type = InlineChangeSegmentType::Delete;
                    let mut inserted = change;
                    let InlineChange::Opaque { segment_type, .. } = &mut inserted else {
                        unreachable!("matched opaque change must remain opaque")
                    };
                    *segment_type = InlineChangeSegmentType::Insert;
                    vec![deleted, inserted]
                }
                other => vec![other],
            })
            .collect()
    }

    fn nested_table_change(
        &self,
        _before: &TableNode,
        _after: &TableNode,
        block_index: usize,
    ) -> Result<Option<crate::domain::NestedTableDiff>, String> {
        Err(format!(
            "unrelated cell replacement unexpectedly shared nested table block {block_index}"
        ))
    }
}

/// Result of compiling a comparison into tracked engine state.
pub struct MergeResult {
    pub doc: CanonDoc,
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
                    source_run_attrs: Vec::new(),
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
                type_is_explicit: false,
                clear: None,
                wrapper_marks: marks.to_vec(),
                wrapper_style_props: style_props.clone(),
                wrapper_rpr_authored: crate::domain::RunRprAuthored::from_effective(
                    marks,
                    &style_props,
                ),
                source_run_attrs: Vec::new(),
                formatting_change: formatting_change.clone(),
                joins_following_text_run: false,
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
fn push_opaque_replacement_segments(
    segments: &mut Vec<TrackedSegment>,
    base: &crate::domain::OpaqueInlineNode,
    target: &crate::domain::OpaqueInlineNode,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
) {
    let drawing_replacement =
        matches!(base.kind, OpaqueKind::Drawing) && matches!(target.kind, OpaqueKind::Drawing);

    // Word emits drawing replacements with the target first. The order is
    // visually significant for overlapping inline/block drawings: consumers
    // paint the later deleted drawing over the inserted drawing when the
    // source is emitted first. Other opaque kinds retain their established
    // delete-then-insert ordering.
    if drawing_replacement {
        segments.push(TrackedSegment {
            status: TrackingStatus::Inserted(next_revision(revision, rev_counter)),
            inlines: vec![InlineNode::from(target.clone())],
        });
        segments.push(TrackedSegment {
            status: TrackingStatus::Deleted(next_revision(revision, rev_counter)),
            inlines: vec![InlineNode::from(base.clone())],
        });
    } else {
        segments.push(TrackedSegment {
            status: TrackingStatus::Deleted(next_revision(revision, rev_counter)),
            inlines: vec![InlineNode::from(base.clone())],
        });
        segments.push(TrackedSegment {
            status: TrackingStatus::Inserted(next_revision(revision, rev_counter)),
            inlines: vec![InlineNode::from(target.clone())],
        });
    }
}

/// Whether two positionally aligned opaque nodes carry different semantic
/// payloads and therefore need a tracked replacement pair.
///
/// `content_hash` is authoritative when both nodes have one. Hyperlinks are
/// modeled without that hash, so compare their semantic fields while excluding
/// the package-local relationship id. A one-sided hash is not evidence of
/// equality and is conservatively represented as a replacement.
fn opaque_payload_changed(
    base: &crate::domain::OpaqueInlineNode,
    target: &crate::domain::OpaqueInlineNode,
) -> bool {
    let kinds_equal = match (&base.kind, &target.kind) {
        (OpaqueKind::Hyperlink(base), OpaqueKind::Hyperlink(target)) => {
            base.url == target.url
                && base.anchor == target.anchor
                && base.text == target.text
                && base.runs == target.runs
                && base.extra_attrs == target.extra_attrs
        }
        (base, target) => base == target,
    };
    if !kinds_equal {
        return true;
    }

    match (&base.content_hash, &target.content_hash) {
        (Some(base), Some(target)) => base != target,
        (None, None) => false,
        _ => true,
    }
}

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
                        carrier: fc.carrier,
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
            InlineChange::Opaque {
                segment_type, kind, ..
            } => {
                // The public diff projection represents both zero-width comment
                // range starts and comment references as CommentReference
                // opaque segments. They are not OpaqueInlineNodes and therefore
                // must not consume either opaque cursor. The structural-marker
                // reconciliation below re-injects the lossless start/end/ref
                // nodes from the base and target paragraphs at their offsets.
                if *kind == crate::domain::OpaqueSegmentKind::CommentReference {
                    continue;
                }
                // Opaque changes (images, equations, fields) are handled directly
                // here rather than through the text+U+FFFC path.
                match segment_type {
                    InlineChangeSegmentType::Delete => {
                        let opaque =
                            base_opaques
                                .get(base_opaque_idx)
                                .ok_or_else(|| MergeError {
                                    kind: MergeErrorKind::InvalidPlan,
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
                                    kind: MergeErrorKind::InvalidPlan,
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
                                    kind: MergeErrorKind::InvalidPlan,
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
                                    kind: MergeErrorKind::InvalidPlan,
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

                        let content_changed = opaque_payload_changed(base_opaque, target_opaque);

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
                                push_opaque_replacement_segments(
                                    &mut segments,
                                    base_opaque,
                                    target_opaque,
                                    revision,
                                    rev_counter,
                                );
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
    // Invariant M (domain-model §6): apply the edit path's segment normalization
    // here too, as the final pass, so both materializers normalize tracking
    // boundaries identically. Runs last so the field/opaque passes above still
    // see the unmerged segment boundaries they depend on.
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
/// emits a tracked replacement pair. Drawing replacements follow Word's
/// insertion-then-deletion order; other opaque kinds retain deletion-then-insertion.
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
    // at which point we flush the accumulated normal inlines, emit the pair,
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
                (Some(base), Some(target)) => opaque_payload_changed(base, target),
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

                // Emit the tracked opaque replacement pair.
                // Note: paragraph-level opaques (OmmlBlock) should never
                // reach here — the diff layer reclassifies wholly-opaque
                // paragraph changes as BlockDeleted + BlockInserted.
                let bo = base_opaque
                    .as_ref()
                    .expect("content_changed requires a base opaque");
                let to = target_opaque
                    .as_ref()
                    .expect("content_changed requires a target opaque");
                push_opaque_replacement_segments(&mut segments, bo, to, revision, rev_counter);
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
            kind: MergeErrorKind::InvalidPlan,
            message: "block_id for deletion not found in base model".to_string(),
            context: format!("{context}:{}", block_id_to_delete.0),
        });
    };
    mark_whole_block_deleted(&mut blocks[idx], revision, rev_counter);
    Ok(())
}

/// Mark one complete block deleted in the public canonical model.
pub fn mark_whole_block_deleted(
    tracked_block: &mut TrackedBlock,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
) {
    tracked_block.status = TrackingStatus::Deleted(next_revision(revision, rev_counter));
    match &mut tracked_block.block {
        BlockNode::Paragraph(paragraph) => {
            paragraph.para_mark_status = Some(TrackingStatus::Deleted(next_revision(
                revision,
                rev_counter,
            )));
        }
        BlockNode::Table(table) => mark_table_deleted(table, revision, rev_counter),
        // Body/story opaque blocks use the enclosing TrackedBlock status as
        // their whole-object carrier. Cell-story blocks are bare BlockNode
        // values and have separate fail-fast lowering helpers below.
        BlockNode::OpaqueBlock(_) => {}
    }
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
            kind: MergeErrorKind::InvalidPlan,
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
    let source_uses_structural_numbering = source.has_resolved_numbering();
    let target_uses_structural_numbering = target.has_resolved_numbering();
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
    // prefix is literal paragraph text on either terminal side. Structural
    // source numbering is different: the complete previous w:numPr lives in
    // w:pPrChange, so materializing that label as deleted text would represent
    // it twice. Reject would restore both the numPr and the inline label.
    // Comparing label text alone is therefore not enough; representation is
    // part of the redline semantics.
    let emit_deleted_prefix = source.literal_prefix.is_some()
        && (prefix_text_changed
            || target_uses_structural_numbering
            || target.literal_prefix.is_none());
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
    paragraph.literal_prefix_leading_rpr = target.literal_prefix_leading_rpr.clone();
    paragraph.literal_prefix_trailing_rpr = target.literal_prefix_trailing_rpr.clone();
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
        InlineNode::OpaqueInline(opaque)
            if matches!(opaque.kind, OpaqueKind::CommentReference(_)) =>
        {
            0
        }
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
    ) || matches!(
        inline,
        InlineNode::OpaqueInline(opaque)
            if matches!(opaque.kind, OpaqueKind::CommentReference(_))
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
        InlineNode::OpaqueInline(opaque) => match &opaque.kind {
            OpaqueKind::CommentReference(reference) => {
                Some(format!("comment-ref:{offset}:{}", reference.reference_id))
            }
            _ => None,
        },
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
                InlineNode::OpaqueInline(opaque)
                    if matches!(opaque.kind, OpaqueKind::CommentReference(_)) =>
                {
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

/// Comparison-specific structural-marker injection.
///
/// Comment ids have already been reconciled across the two package namespaces.
/// A marker present on both sides is therefore the same surviving annotation;
/// a base-only marker is deleted and a target-only marker is inserted. Keeping
/// that polarity on the marker itself is essential at zero-width boundaries:
/// borrowing whichever text segment happens to be adjacent cross-wires
/// colliding comments and leaves orphan anchors after accept/reject.
fn inject_structural_markers_for_compare(
    final_segments: &mut Vec<TrackedSegment>,
    original_segments: &[TrackedSegment],
    target_segments: &[TrackedSegment],
    revision: &RevisionInfo,
    rev_counter: &mut u32,
) {
    #[derive(Clone)]
    struct TrackedMarker {
        offset: usize,
        inline: InlineNode,
        status: Option<TrackingStatus>,
    }

    fn push(out: &mut Vec<TrackedSegment>, inline: InlineNode, status: TrackingStatus) {
        if let Some(last) = out.last_mut()
            && last.status == status
        {
            last.inlines.push(inline);
        } else {
            out.push(TrackedSegment {
                status,
                inlines: vec![inline],
            });
        }
    }

    let base_markers = collect_positioned_structural_markers(original_segments);
    let target_markers = collect_positioned_structural_markers(target_segments);
    if !base_markers
        .iter()
        .chain(&target_markers)
        .any(|marker| is_comment_marker(&marker.inline))
    {
        inject_structural_markers_at_offsets(
            final_segments,
            original_segments,
            Some(target_segments),
        );
        return;
    }
    let base_keys: HashSet<String> = base_markers
        .iter()
        .filter_map(|marker| structural_marker_identity(&marker.inline, marker.offset))
        .collect();
    let target_keys: HashSet<String> = target_markers
        .iter()
        .filter_map(|marker| structural_marker_identity(&marker.inline, marker.offset))
        .collect();

    let mut markers = Vec::new();
    for marker in base_markers {
        let key = structural_marker_identity(&marker.inline, marker.offset);
        let status = if is_comment_marker(&marker.inline) {
            if key.as_ref().is_some_and(|key| target_keys.contains(key)) {
                Some(TrackingStatus::Normal)
            } else {
                Some(TrackingStatus::Deleted(next_revision(
                    revision,
                    rev_counter,
                )))
            }
        } else {
            None
        };
        markers.push(TrackedMarker {
            offset: marker.offset,
            inline: marker.inline,
            status,
        });
    }
    for marker in target_markers {
        let Some(key) = structural_marker_identity(&marker.inline, marker.offset) else {
            continue;
        };
        if base_keys.contains(&key) {
            continue;
        }
        let mut inline = marker.inline;
        let status = if is_comment_marker(&inline) {
            Some(TrackingStatus::Inserted(next_revision(
                revision,
                rev_counter,
            )))
        } else {
            if let InlineNode::Decoration(ref mut decoration) = inline {
                decoration.origin = Some("target".to_string());
            }
            None
        };
        markers.push(TrackedMarker {
            offset: marker.offset,
            inline,
            status,
        });
    }

    if markers.is_empty() {
        return;
    }
    markers.sort_by_key(|marker| marker.offset);
    let mut pending = std::collections::VecDeque::from(markers);

    // Inline conversion intentionally omits these markers before this pass.
    // Removing defensively makes the producer idempotent if a future path
    // starts carrying one through directly.
    for segment in final_segments.iter_mut() {
        segment.inlines.retain(|inline| !is_comment_marker(inline));
    }

    let old_segments = std::mem::take(final_segments);
    let mut rebuilt = Vec::new();
    let mut text_offset = 0usize;
    let mut last_status = TrackingStatus::Normal;

    for segment in old_segments {
        last_status = segment.status.clone();
        for inline in segment.inlines {
            while pending
                .front()
                .is_some_and(|marker| marker.offset <= text_offset)
            {
                let marker = pending.pop_front().expect("front existed");
                push(
                    &mut rebuilt,
                    marker.inline,
                    marker.status.unwrap_or_else(|| segment.status.clone()),
                );
            }

            let width = inline_text_width(&inline);
            let marker_is_inside = |marker: &TrackedMarker| {
                marker.offset > text_offset && marker.offset < text_offset + width
            };
            if let InlineNode::Text(text) = &inline
                && pending.front().is_some_and(marker_is_inside)
            {
                let chars: Vec<char> = text.text.chars().collect();
                let mut consumed = 0usize;
                while pending.front().is_some_and(marker_is_inside) {
                    let marker = pending.pop_front().expect("front existed");
                    let cut = marker.offset - text_offset;
                    if cut > consumed {
                        let mut piece = (**text).clone();
                        piece.id = NodeId::new(format!("{}_cm{consumed}", text.id));
                        piece.text = chars[consumed..cut].iter().collect();
                        push(
                            &mut rebuilt,
                            InlineNode::Text(Box::new(piece)),
                            segment.status.clone(),
                        );
                    }
                    push(
                        &mut rebuilt,
                        marker.inline,
                        marker.status.unwrap_or_else(|| segment.status.clone()),
                    );
                    consumed = cut;
                }
                let mut tail = (**text).clone();
                tail.id = NodeId::new(format!("{}_cm{consumed}", text.id));
                tail.text = chars[consumed..].iter().collect();
                push(
                    &mut rebuilt,
                    InlineNode::Text(Box::new(tail)),
                    segment.status.clone(),
                );
            } else {
                push(&mut rebuilt, inline, segment.status.clone());
            }
            text_offset += width;
        }
    }

    while let Some(marker) = pending.pop_front() {
        let status = marker.status.unwrap_or_else(|| last_status.clone());
        push(&mut rebuilt, marker.inline, status);
    }
    *final_segments = rebuilt;
}

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
            source_run_attrs: Vec::new(),
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
            source_run_attrs: t.source_run_attrs.clone(),
            formatting_change: None,
        },
        None => TextNode {
            id,
            text_role: Some(TextRole::MaterializedPrefix(kind)),
            text,
            marks: Vec::new(),
            style_props: StyleProps::default(),
            rpr_authored: RunRprAuthored::default(),
            source_run_attrs: Vec::new(),
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
            kind: MergeErrorKind::InvalidPlan,
            message: "block_id for modification not found in base model".to_string(),
            context: format!("{context}:{}", block_id_to_modify.0),
        });
    };

    if let (BlockNode::Paragraph(source), BlockNode::Paragraph(target)) =
        (&blocks[idx].block, new_block)
    {
        refuse_unstable_cnf_style_change(source, target, context)?;
    }

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
                kind: MergeErrorKind::InvalidPlan,
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

    // Prefix materialization: only needed when `literal_prefix` (baked text)
    // is involved.  When both sides have structural numbering (w:numPr), the
    // prefix is generated by Word from the numbering definition — no inline
    // text to track.  Counter drift (same numId/ilvl, different counter value)
    // is handled implicitly by list reordering.
    let new_para = match new_block {
        BlockNode::Paragraph(p) => Some(p),
        _ => None,
    };
    let numbering_changed = new_para
        .is_some_and(|target| !crate::domain::paragraph_numbering_state_eq(paragraph, target));
    let paragraph_properties_changed = new_para
        .is_some_and(|target| !crate::domain::paragraph_property_state_eq(paragraph, target));
    let paragraph_mark_properties_changed = new_para
        .is_some_and(|target| !crate::domain::paragraph_mark_property_state_eq(paragraph, target));
    let original_formatting_snapshot =
        (paragraph_properties_changed || paragraph_mark_properties_changed).then(|| {
            let property_revision = next_revision(revision, rev_counter);
            let mut snapshot =
                crate::edit::snapshot_paragraph_formatting(paragraph, &property_revision);
            snapshot.carrier = match (
                paragraph_properties_changed,
                paragraph_mark_properties_changed,
            ) {
                (true, false) => {
                    crate::domain::ParagraphFormattingChangeCarrier::ParagraphProperties
                }
                (false, true) => {
                    crate::domain::ParagraphFormattingChangeCarrier::ParagraphMarkProperties
                }
                (true, true) => {
                    crate::domain::ParagraphFormattingChangeCarrier::ParagraphAndMarkProperties
                }
                (false, false) => unreachable!("formatting snapshot requires a changed carrier"),
            };
            snapshot
        });
    let mut prefix_was_materialized = false;
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
                paragraph.has_direct_numbering = new_para.has_direct_numbering;
                paragraph.numbering_suppressed = new_para.numbering_suppressed;
                prefix_was_materialized = true;
            }
        }
    }

    // Detect paragraph formatting changes (pPrChange): compare ALL formatting
    // properties between the base and target paragraphs. Per §17.13.5.29 the
    // snapshot must be COMPLETE, so we capture every property the old paragraph had.
    // `numbering_changed` was captured before prefix handling because that
    // lowering step can deliberately clear the effective numbering payload.
    // It includes direct/inherited/suppressed provenance, not just list coords.
    if let Some(new_para) = new_para
        && (paragraph_properties_changed || paragraph_mark_properties_changed)
    {
        paragraph.formatting_change = original_formatting_snapshot;

        if paragraph_properties_changed {
            // Update ordinary paragraph properties to the target state. This
            // helper also copies the mark state when both carriers changed.
            apply_target_non_numbering_paragraph_properties(paragraph, new_para);
            sync_literal_prefix_geometry(new_para, paragraph);
        } else {
            // A paragraph-mark rPrChange is independently native and must not
            // manufacture an empty pPrChange merely because its parent pPr
            // container exists.
            paragraph.paragraph_mark_marks = new_para.paragraph_mark_marks.clone();
            paragraph.paragraph_mark_style_props = new_para.paragraph_mark_style_props.clone();
            paragraph.paragraph_mark_rfonts = new_para.paragraph_mark_rfonts.clone();
            paragraph.paragraph_mark_rpr_off = new_para.paragraph_mark_rpr_off;
        }
        // Only update numbering when the prefix was not already materialized
        // as tracked inline content — otherwise we'd re-introduce the numPr.
        if numbering_changed && !prefix_was_materialized {
            paragraph.numbering = new_para.numbering.clone();
            // Carry the target's numbering PROVENANCE so the emission gate
            // matches the new numbering (a target that authored a direct
            // numPr emits one; inherited numbering does not).
            paragraph.has_direct_numbering = new_para.has_direct_numbering;
            paragraph.numbering_suppressed = new_para.numbering_suppressed;
            // Clear literal_prefix when gaining structural numbering to
            // prevent the serializer from writing both a text run for the
            // literal prefix AND a numPr in pPr.
            if new_para.has_resolved_numbering() {
                paragraph.literal_prefix = None;
                paragraph.literal_prefix_leading_tab_twips = None;
                paragraph.literal_prefix_leading_tab_count = 0;
                paragraph.literal_prefix_has_trailing_tab = false;
                paragraph.literal_prefix_trailing_tab_stop_twips = None;
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
        paragraph.paragraph_mark_rfonts = new_p.paragraph_mark_rfonts.clone();
        paragraph.paragraph_mark_rpr_off = new_p.paragraph_mark_rpr_off;
    }

    inject_structural_markers_for_compare(
        &mut new_segments,
        &original_segments,
        new_para.map_or(&[], |p| p.segments.as_slice()),
        revision,
        rev_counter,
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
    rev_counter: &mut u32,
) {
    if cell.formatting != *new_formatting {
        let carrier = next_revision(revision, rev_counter);
        cell.formatting_change = Some(crate::edit::snapshot_cell_formatting(cell, &carrier));
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
    resolver: &dyn ChangePlanResolver,
) -> Result<(), MergeError> {
    let idx = find_block_index(blocks, table_id).ok_or_else(|| MergeError {
        kind: MergeErrorKind::InvalidPlan,
        message: "table_id for cell modification not found in base model".to_string(),
        context: format!("{context}:{}", table_id.0),
    })?;
    let table = match &mut blocks[idx].block {
        BlockNode::Table(t) => t,
        _ => {
            return Err(MergeError {
                kind: MergeErrorKind::InvalidPlan,
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
                kind: MergeErrorKind::InvalidPlan,
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
                kind: MergeErrorKind::InvalidPlan,
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
            apply_cell_formatting_change(cell, new_formatting, revision, rev_counter);
        }

        for para_change in &cell_change.paragraph_changes {
            let n_blocks = cell.blocks.len();
            let block = cell
                .blocks
                .get_mut(para_change.block_index)
                .ok_or_else(|| MergeError {
                    kind: MergeErrorKind::InvalidPlan,
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
                        kind: MergeErrorKind::InvalidPlan,
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
                resolver,
            )?;
        }

        // Apply nested table diffs within this cell.
        for nested_diff in &cell_change.nested_table_diffs {
            let n_blocks = cell.blocks.len();
            let block = cell
                .blocks
                .get_mut(nested_diff.block_index)
                .ok_or_else(|| MergeError {
                    kind: MergeErrorKind::InvalidPlan,
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
                        kind: MergeErrorKind::InvalidPlan,
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
                        resolver,
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
                        resolver,
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
    resolver: &dyn ChangePlanResolver,
) -> Result<(), MergeError> {
    for cell_change in cell_changes {
        let n_rows = table.rows.len();
        let row = table
            .rows
            .get_mut(cell_change.row_index)
            .ok_or_else(|| MergeError {
                kind: MergeErrorKind::InvalidPlan,
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
                kind: MergeErrorKind::InvalidPlan,
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
            apply_cell_formatting_change(cell, new_formatting, revision, rev_counter);
        }

        for para_change in &cell_change.paragraph_changes {
            let n_blocks = cell.blocks.len();
            let block = cell
                .blocks
                .get_mut(para_change.block_index)
                .ok_or_else(|| MergeError {
                    kind: MergeErrorKind::InvalidPlan,
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
                        kind: MergeErrorKind::InvalidPlan,
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
                    kind: MergeErrorKind::InvalidPlan,
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
                        kind: MergeErrorKind::InvalidPlan,
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
                        resolver,
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
                        resolver,
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
    resolver: &dyn ChangePlanResolver,
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
            TableRowAlignment::Matched { old_row, new_row }
            | TableRowAlignment::Replacement { old_row, new_row } => {
                let content_relation = if matches!(alignment, TableRowAlignment::Replacement { .. })
                {
                    CellContentRelation::UnrelatedReplacement
                } else {
                    CellContentRelation::Related
                };
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
                        apply_cell_formatting_change(
                            &mut cell,
                            &new_cell_ref.formatting,
                            revision,
                            rev_counter,
                        );
                        reconcile_cell_blocks(
                            &mut cell,
                            new_cell_ref,
                            revision,
                            rev_counter,
                            context,
                            resolver,
                            content_relation,
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

// This is the comparison compiler's explicit orchestration boundary. The
// arguments are distinct domain inputs; grouping them would create an opaque
// context object solely to satisfy an arbitrary lint threshold.
#[allow(clippy::too_many_arguments)]
fn apply_changes_to_blocks(
    blocks: &mut Vec<TrackedBlock>,
    changes: &[DiffChange],
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    context: &str,
    target_tables_by_id: Option<&mut HashMap<String, BlockNode>>,
    provenance: &mut BlockProvenanceMap,
    resolver: &dyn ChangePlanResolver,
) -> Result<(), MergeError> {
    // A final source table that disappears entirely has no physical block that
    // survives both readings. Word resolves that table carrier and a
    // target-only empty paragraph independently, but its required end-of-body
    // sentinel can produce an extra paragraph (and the complete-row replacement
    // arm can hang on Review UI Reject All). This is a carrier-composition
    // boundary, not a general table limitation: the same table change resolves
    // when an untracked paragraph follows it. Refuse before mutating the model
    // until one native composition is witnessed for both terminal operations.
    if context == "body" && has_unqualified_final_table_removal(changes, blocks) {
        return Err(MergeError {
            kind: MergeErrorKind::UnsupportedEdit,
            message: "complete removal of a document-final table cannot be composed with an inserted final empty paragraph"
                .to_string(),
            context: format!(
                "{context}: native Word does not reliably resolve the combined final-table and final-paragraph carriers"
            ),
        });
    }

    // A complete row replacement resolves in Word when it is the story's only
    // change. An unrelated physical-shell replacement can follow independent
    // changes, but native Word Review Reject All hangs when another change
    // follows that table in the same story. Keep the witnessed ordering
    // boundary and refuse the unsafe composition before minting revision state.
    if has_unqualified_row_replacement_composition(changes) {
        return Err(MergeError {
            kind: MergeErrorKind::UnsupportedEdit,
            message: "table-row replacement cannot be followed by another change in the same story"
                .to_string(),
            context: format!(
                "{context}: native Word Reject All does not reliably resolve a later carrier after an unrelated table-row replacement"
            ),
        });
    }

    let property_history_carriers = property_history_carrier_count(changes);
    if property_history_carriers > MAX_PROPERTY_HISTORY_CARRIERS_PER_STORY {
        return Err(MergeError {
            kind: MergeErrorKind::UnsupportedEdit,
            message: format!(
                "comparison requires {property_history_carriers} paragraph/run property-history carriers; the supported per-story safety limit is {MAX_PROPERTY_HISTORY_CARRIERS_PER_STORY}"
            ),
            context: format!(
                "{context}: native Word Review UI resolution is not reliable above the supported property-history safety limit"
            ),
        });
    }

    // A complete `w:sdt` nested in `w:ins` is schema-valid, but desktop Word
    // does not reject it as one atomic content-control insertion: Reject All
    // can remove the surrounding paragraph changes while leaving the control's
    // active content and the insertion revision behind. Tracking only the
    // control's text would lose the target's authored content-control state,
    // so it is not an exact carrier.
    // Refuse before mutating the model until a native carrier is witnessed.
    let contains_inserted_inline_sdt = changes.iter().any(|change| match change {
        DiffChange::BlockInserted { block, .. } => block_contains_inline_sdt(block),
        DiffChange::BlockModified { inline_changes, .. } => inline_changes.iter().any(|change| {
            matches!(
                change,
                InlineChange::Opaque {
                    segment_type: InlineChangeSegmentType::Insert,
                    kind: crate::domain::OpaqueSegmentKind::Sdt,
                    ..
                }
            )
        }),
        _ => false,
    });
    if contains_inserted_inline_sdt {
        return Err(MergeError {
            kind: MergeErrorKind::UnsupportedEdit,
            message:
                "side-only inline content-control insertion has no qualified native Word carrier"
                    .to_string(),
            context: format!(
                "{context}: Word Reject All does not remove a complete w:sdt nested in w:ins"
            ),
        });
    }

    // Word opens `w:ins|w:del > m:oMathPara` without repair but does not apply
    // that wrapper as a whole-object revision: Reject can retain the equation
    // and merge it into a surviving paragraph. Until a distinct native carrier
    // is qualified, a side-only display equation cannot be materialized as an
    // apparently reversible comparison change.
    let contains_side_only_block_math = changes.iter().any(|change| match change {
        DiffChange::BlockDeleted { old_block, .. } => block_contains_block_math(old_block),
        DiffChange::BlockInserted { block, .. } => block_contains_block_math(block),
        _ => false,
    });
    if contains_side_only_block_math {
        return Err(MergeError {
            kind: MergeErrorKind::UnsupportedEdit,
            message: "side-only block equations have no qualified native Word carrier".to_string(),
            context: format!(
                "{context}: Word does not reliably resolve a tracked m:oMathPara wrapper"
            ),
        });
    }

    // Native Word can open this composition cleanly yet hang indefinitely on
    // Reject All: a move range in a story together with a separately inserted
    // table whose structural carrier is `w:trPr/w:ins`. Each carrier resolves
    // independently, but their combined terminal operation is not qualified.
    // Refuse before mutating the model; do not turn an unsafe composition into
    // an apparent successful comparison.
    let contains_move = changes.iter().any(|change| {
        matches!(
            change,
            DiffChange::BlockDeleted {
                move_id: Some(_),
                ..
            } | DiffChange::BlockInserted {
                move_id: Some(_),
                ..
            }
        )
    });
    let contains_inserted_table = changes.iter().any(|change| {
        matches!(
            change,
            DiffChange::BlockInserted {
                block: BlockNode::Table(_),
                ..
            }
        )
    });
    if contains_move && contains_inserted_table {
        return Err(MergeError {
            kind: MergeErrorKind::UnsupportedEdit,
            message: "move ranges cannot be composed with an inserted table in one story"
                .to_string(),
            context: format!(
                "{context}: native Word Reject All does not reliably resolve the combined move and row-insertion carriers"
            ),
        });
    }

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
                        kind: MergeErrorKind::InvalidPlan,
                        message: "table structure merge requires target table lookup".to_string(),
                        context: format!("{context}:{}", base_table_id.0),
                    });
                };

                let inserted_table =
                    target_tables_by_id
                        .remove(&*target_table_id.0)
                        .ok_or_else(|| MergeError {
                            kind: MergeErrorKind::InvalidPlan,
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
                        resolver,
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
                    resolver,
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
    let modified_block_ids: HashSet<NodeId> = changes
        .iter()
        .filter_map(|change| match change {
            DiffChange::BlockModified { block_id, .. } => Some(block_id.clone()),
            _ => None,
        })
        .collect();
    restore_source_properties_on_deleted_mark_donors(blocks, &modified_block_ids);
    Ok(())
}

fn has_unqualified_final_table_removal(changes: &[DiffChange], blocks: &[TrackedBlock]) -> bool {
    let Some(final_base_table_id) = blocks.last().and_then(|block| match &block.block {
        BlockNode::Table(table) => Some(&table.id),
        _ => None,
    }) else {
        return false;
    };

    let removes_final_table = changes.iter().any(|change| match change {
        DiffChange::BlockDeleted {
            block_id,
            old_block: BlockNode::Table(_),
            move_id: None,
            ..
        } => block_id == final_base_table_id,
        DiffChange::TableStructureChanged { table_id, .. } => {
            table_id == final_base_table_id && is_complete_row_replacement(change)
        }
        _ => false,
    });
    if !removes_final_table {
        return false;
    }

    let Some(DiffChange::BlockInserted {
        block: BlockNode::Paragraph(paragraph),
        move_id: None,
        ..
    }) = changes
        .iter()
        .rev()
        .find(|change| matches!(change, DiffChange::BlockInserted { .. }))
    else {
        return false;
    };
    paragraph
        .literal_prefix
        .as_deref()
        .is_none_or(str::is_empty)
        && paragraph
            .all_inlines()
            .all(|inline| matches!(inline, InlineNode::Text(text) if text.text.is_empty()))
}

fn is_complete_row_replacement(change: &DiffChange) -> bool {
    let DiffChange::TableStructureChanged {
        table_diff: Some(diff),
        ..
    } = change
    else {
        return false;
    };
    let has_deleted = diff
        .row_alignment
        .iter()
        .any(|row| matches!(row, TableRowAlignment::Deleted { .. }));
    let has_inserted = diff
        .row_alignment
        .iter()
        .any(|row| matches!(row, TableRowAlignment::Inserted { .. }));
    let has_survivor = diff.row_alignment.iter().any(|row| {
        matches!(
            row,
            TableRowAlignment::Matched { .. } | TableRowAlignment::Replacement { .. }
        )
    });
    has_deleted && has_inserted && !has_survivor
}

fn is_unrelated_row_replacement(change: &DiffChange) -> bool {
    matches!(
        change,
        DiffChange::TableStructureChanged {
            table_diff: Some(diff),
            ..
        } if diff
            .row_alignment
            .iter()
            .any(|row| matches!(row, TableRowAlignment::Replacement { .. }))
    )
}

fn has_unqualified_row_replacement_composition(changes: &[DiffChange]) -> bool {
    if changes.len() > 1 && changes.iter().any(is_complete_row_replacement) {
        return true;
    }

    let mut saw_unrelated_row_replacement = false;
    for change in changes {
        if is_unrelated_row_replacement(change) {
            if saw_unrelated_row_replacement {
                return true;
            }
            saw_unrelated_row_replacement = true;
        } else if saw_unrelated_row_replacement {
            return true;
        }
    }
    false
}

fn property_history_carrier_count(changes: &[DiffChange]) -> usize {
    changes
        .iter()
        .map(|change| {
            let DiffChange::BlockModified {
                inline_changes,
                old_block,
                new_block,
                ..
            } = change
            else {
                return 0;
            };
            let paragraph_carriers = match (old_block, new_block) {
                (BlockNode::Paragraph(old), BlockNode::Paragraph(new)) => {
                    usize::from(
                        !crate::domain::paragraph_property_state_eq(old, new)
                            || old.literal_prefix != new.literal_prefix,
                    ) + usize::from(!crate::domain::paragraph_mark_property_state_eq(old, new))
                }
                _ => 0,
            };
            let run_carriers = inline_changes
                .iter()
                .filter(|change| {
                    matches!(
                        change,
                        InlineChange::Unchanged {
                            formatting_change: Some(change),
                            ..
                        } if change.is_physical_revision()
                    )
                })
                .count();
            paragraph_carriers.saturating_add(run_carriers)
        })
        .sum()
}

fn block_contains_block_math(block: &BlockNode) -> bool {
    let BlockNode::Paragraph(paragraph) = block else {
        return false;
    };
    paragraph.segments.iter().any(|segment| {
        segment.inlines.iter().any(|inline| {
            matches!(
                inline,
                InlineNode::OpaqueInline(opaque) if opaque.kind == OpaqueKind::OmmlBlock
            )
        })
    })
}

fn block_contains_inline_sdt(block: &BlockNode) -> bool {
    match block {
        BlockNode::Paragraph(paragraph) => paragraph.segments.iter().any(|segment| {
            segment.inlines.iter().any(|inline| {
                matches!(
                    inline,
                    InlineNode::OpaqueInline(opaque) if opaque.kind == OpaqueKind::Sdt
                )
            })
        }),
        BlockNode::Table(table) => table.rows.iter().any(|row| {
            row.cells
                .iter()
                .any(|cell| cell.blocks.iter().any(block_contains_inline_sdt))
        }),
        BlockNode::OpaqueBlock(_) => false,
    }
}

/// Keep source paragraph properties on a modified paragraph whose mark is
/// tracked-deleted.
///
/// Accepting a paragraph-mark deletion joins this donor into the following
/// paragraph, whose `pPr` wins (§17.13.5.15). Target properties temporarily
/// copied onto the donor are therefore semantically dead on Accept and actively
/// harmful on Reject: a `pPrChange` restores the source `numPr` while the
/// materialized deleted number also becomes live text, duplicating the label.
/// Word Compare keeps the donor's source properties and lets the structural
/// join select the following target paragraph's properties.
///
/// Scope this correction to blocks modified by the current diff. Pre-existing
/// paragraph-mark and formatting revisions in the input remain untouched.
fn restore_source_properties_on_deleted_mark_donors(
    blocks: &mut [TrackedBlock],
    modified_block_ids: &HashSet<NodeId>,
) {
    for tracked in blocks {
        let BlockNode::Paragraph(paragraph) = &mut tracked.block else {
            continue;
        };
        if !modified_block_ids.contains(&paragraph.id)
            || !matches!(paragraph.para_mark_status, Some(TrackingStatus::Deleted(_)))
            || paragraph.formatting_change.is_none()
        {
            continue;
        }

        reject_paragraph_formatting(paragraph);
        for segment in &mut paragraph.segments {
            segment.inlines.retain(|inline| {
                !matches!(
                    inline,
                    InlineNode::Text(text)
                        if text.text_role
                            == Some(TextRole::MaterializedPrefix(
                                MaterializedPrefixKind::StructuralDeleted,
                            ))
                )
            });
        }
        paragraph
            .segments
            .retain(|segment| !segment.inlines.is_empty());
    }
}

/// Annotate `para_mark_status` on the last Normal paragraph when all blocks
/// after it to the end of the list have one homogeneous non-Normal status.
///
/// OOXML rule: a paragraph's mark status reflects what happens to the boundary
/// between it and its successor. When a Normal paragraph is the last Normal
/// block and only inserted blocks or only deleted blocks follow it to the end,
/// Word marks its ¶ with that shared status. A mixed inserted/deleted suffix
/// has no single boundary disposition: assigning the first block's status to
/// the surviving paragraph can make native Reject discard source content.
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

    // A paragraph-mark revision carries a chain of paragraph boundaries. A
    // table (or another non-paragraph block) has its own structural carrier;
    // marking the preceding paragraph's pilcrow as inserted incorrectly
    // couples those carriers even when inserted paragraphs occur between
    // them. Desktop Word can open that shape but Reject All fails when it
    // reaches the inserted table behind the inserted pilcrow.
    if blocks[next_idx..]
        .iter()
        .any(|block| !matches!(block.block, BlockNode::Paragraph(_)))
    {
        return;
    }

    let is_inserted = matches!(blocks[next_idx].status, TrackingStatus::Inserted(_));
    let is_deleted = matches!(blocks[next_idx].status, TrackingStatus::Deleted(_));
    if !is_inserted && !is_deleted {
        return;
    }

    // A paragraph mark has one disposition. It can represent a suffix made
    // entirely of insertions or entirely of deletions, but not a suffix that
    // crosses from one disposition to the other. Looking only at the next
    // block made an inserted target region followed by deleted source content
    // mark the surviving source paragraph as inserted; native Word then
    // removed that paragraph on Reject.
    let suffix_has_one_disposition = blocks[next_idx..].iter().all(|block| {
        if is_inserted {
            matches!(block.status, TrackingStatus::Inserted(_))
        } else {
            matches!(block.status, TrackingStatus::Deleted(_))
        }
    });
    if !suffix_has_one_disposition {
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
pub fn normalize_final_mark_attribution(
    blocks: &mut [TrackedBlock],
    revision: &RevisionInfo,
    rev_counter: &mut u32,
) {
    let Some(last) = document_final_paragraph_index(blocks) else {
        return;
    };
    let BlockNode::Paragraph(final_para) = &blocks[last].block else {
        unreachable!("document_final_paragraph_index only returns paragraphs");
    };
    // What serialize would emit for the final pilcrow: the paragraph's own
    // para_mark_status, else the block-level status (see
    // `serialize::serialize_paragraph_node`). `Some(Normal)` (an already
    // suppressed mark) short-circuits to Normal → nothing to do.
    let effective = final_para
        .para_mark_status
        .clone()
        .unwrap_or_else(|| blocks[last].status.clone());
    // A move tail needs the move-aware rule. The moveTo DESTINATION copy that
    // ends the document leaves an insertion-class mark on the document-final
    // pilcrow (Word can't resolve it, exactly like a plain insert tail) — shift
    // it to the anchor. The moveFrom SHADOW (Deleted + move_id) sits at its
    // ORIGINAL position and is resolved by the moveFromRange pairing there, so
    // leave it and its paired half untouched. Critically, an ALREADY-suppressed
    // move tail has effective Normal and must remain untouched by later edits;
    // re-normalizing it would mint an unrelated break on its old anchor.
    if blocks[last].move_id.is_some() {
        if matches!(effective, TrackingStatus::Inserted(_))
            && matches!(blocks[last].status, TrackingStatus::Inserted(_))
        {
            normalize_moved_final_mark(blocks, last, revision, rev_counter);
        }
        return;
    }
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

/// Locate the paragraph whose pilcrow is the document-final paragraph mark.
///
/// Word permits zero-width range-marker halves as direct `w:body` children
/// after the last `w:p` (for example a `w:bookmarkEnd` whose range closes at
/// the end of the document). Import represents those byte-faithfully as typed
/// opaque blocks. They do not render a block or displace the last paragraph's
/// pilcrow, so skip only those identified range markers. A trailing table,
/// content control, or unknown opaque block is not silently treated as
/// zero-width: its final-mark semantics are not established here.
pub fn document_final_paragraph_index(blocks: &[TrackedBlock]) -> Option<usize> {
    for (index, block) in blocks.iter().enumerate().rev() {
        match &block.block {
            BlockNode::Paragraph(_) => return Some(index),
            BlockNode::OpaqueBlock(opaque) if opaque.range_marker.is_some() => {}
            BlockNode::Table(_) | BlockNode::OpaqueBlock(_) => return None,
        }
    }
    None
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
        // `Some(Normal)` is the explicit suppression left when an EARLIER
        // tail insertion adopted the then-document-final mark. A later append
        // must stop there and use that paragraph as its anchor; consuming it
        // into the new run can walk back to a table/opaque and strand the new
        // final pilcrow. Paragraphs minted together in the current insertion
        // batch have no such pre-existing suppression.
        && !matches!(
            &blocks[run_start - 1].block,
            BlockNode::Paragraph(p)
                if matches!(p.para_mark_status, Some(TrackingStatus::Normal))
        )
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
    let BlockNode::Paragraph(_anchor) = &blocks[anchor_idx].block else {
        return; // anchor is a table/opaque — cannot carry a paragraph mark.
    };
    // Final paragraph: suppress its own mark marker. Its CURRENT pilcrow
    // formatting remains target-owned; `record_displaced_final_mark_properties`
    // records the source anchor as the previous pPr state. That is the exact
    // Word shape documented below: current target properties plus a previous
    // source snapshot. Overwriting the current pilcrow first would make Accept
    // unable to reach the target when the two marks differ.
    if let BlockNode::Paragraph(p) = &mut blocks[last].block {
        p.para_mark_status = Some(TrackingStatus::Normal);
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

    record_displaced_final_mark_properties(blocks, last, anchor_idx);
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
/// Record the original final paragraph's properties on the paragraph that
/// inherits its mark.
///
/// When the final-mark rule hands the document-final pilcrow to a different
/// paragraph, that paragraph now stands where the original final paragraph
/// stood — and on reject its content merges into whatever properties it
/// carries. Keeping only its own `pPr` makes the reject unreachable: the
/// original final paragraph's style is nowhere in the wire, so no consumer,
/// Word included, can restore it.
///
/// Word's own encoding, observed 2026-07-25 by comparing a paragraph moved to
/// the end of a document (Word 16.0 `CompareDocuments`): the surviving final
/// paragraph keeps its OWN `pStyle` and carries a `w:pPrChange` whose inner
/// `pPr` is the anchor's. Accept then keeps the moved content's style and
/// reject restores the original final paragraph's — both endpoints exact.
/// This mirrors that shape. A paragraph that already carries a formatting
/// change is left alone: its own pending pPrChange is the caller's proposal,
/// not ours to overwrite.
fn record_displaced_final_mark_properties(
    blocks: &mut [TrackedBlock],
    last: usize,
    anchor_idx: usize,
) {
    // The record belongs to the SAME proposal that displaced the mark, so it
    // carries that block's existing tracking revision rather than a freshly
    // minted one. Displacing the final mark is one intention — the same rule
    // that gives an inserted note's two carriers one identity — and minting a
    // second identity here would grow the revision inventory on every move or
    // insertion landing at the end of a document, so an unselected id could
    // vanish under a selective resolution that never named it.
    let carrier = match &blocks[last].status {
        TrackingStatus::Inserted(revision) => revision.clone(),
        // Only an insertion-class carrier reaches the final-mark rules.
        _ => return,
    };
    let BlockNode::Paragraph(anchor) = &blocks[anchor_idx].block else {
        return;
    };
    let mut previous = crate::edit::snapshot_paragraph_formatting(anchor, &carrier);
    // The anchor may already carry the paragraph-property delta produced by
    // this comparison. Rejecting a tail insertion merges the tail into that
    // anchor, so the displaced final mark must restore the anchor's SOURCE
    // projection, not its target-current properties. Otherwise Reject All
    // resolves the anchor's pPrChange and then immediately overwrites that
    // restoration with the target-current snapshot copied onto the tail.
    if let Some(anchor_change) = &anchor.formatting_change {
        previous.previous = anchor_change.previous.clone();
    }
    let BlockNode::Paragraph(final_para) = &blocks[last].block else {
        return;
    };
    if final_para.formatting_change.is_some() {
        return;
    }
    // Identical properties need no record — Word emits none when a split
    // leaves both halves under the same style.
    let current = crate::edit::snapshot_paragraph_formatting(final_para, &carrier);
    if current == previous {
        return;
    }
    if let BlockNode::Paragraph(p) = &mut blocks[last].block {
        p.formatting_change = Some(ParagraphFormattingChange {
            identity: carrier.identity,
            ..previous
        });
    }
}

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
    let BlockNode::Paragraph(_anchor) = &blocks[anchor_idx].block else {
        return; // anchor is a table/opaque — cannot carry a paragraph mark.
    };

    // Final moved-in paragraph: suppress its own pilcrow marker while retaining
    // its target-current pilcrow formatting. The source anchor is carried as
    // the previous state by `record_displaced_final_mark_properties`.
    if let BlockNode::Paragraph(p) = &mut blocks[last].block {
        p.para_mark_status = Some(TrackingStatus::Normal);
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

    record_displaced_final_mark_properties(blocks, last, anchor_idx);
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
    let deleted_revision = match &blocks[last].status {
        TrackingStatus::Deleted(revision) => revision.clone(),
        _ => unreachable!("validated deleted final paragraph"),
    };
    let BlockNode::Paragraph(original_final) = &blocks[last].block else {
        unreachable!("document-final paragraph index must reference a paragraph");
    };
    let original_final_properties =
        crate::edit::snapshot_paragraph_formatting(original_final, &deleted_revision);
    let mut run_start = last;
    while run_start > 0
        && blocks[run_start - 1].move_id.is_none()
        && block_vanishes_on_accept(&blocks[run_start - 1])
    {
        run_start -= 1;
    }

    // Convert the final paragraph into the surviving final mark. A hoisted
    // literal prefix is source body text too; materialize it before the block
    // tombstone is lowered to segment deletions, otherwise the label remains a
    // Normal run and survives Accept while the rest of the paragraph vanishes.
    materialize_numbering_prefix_in_place(&mut blocks[last].block);
    if let BlockNode::Paragraph(p) = &mut blocks[last].block {
        mark_final_paragraph_runs_deleted(p, revision, rev_counter);
        p.para_mark_status = None;
    }
    blocks[last].status = TrackingStatus::Normal;

    if run_start > 0 && blocks[run_start - 1].move_id.is_none() {
        let anchor_idx = run_start - 1;
        let accepted_final_properties = match &blocks[anchor_idx].block {
            BlockNode::Paragraph(paragraph) => Some(paragraph.clone()),
            BlockNode::Table(_) | BlockNode::OpaqueBlock(_) => None,
        };
        let block_insertion = match &blocks[anchor_idx].status {
            TrackingStatus::Inserted(inserted) => Some(inserted.clone()),
            _ => None,
        };
        if let BlockNode::Paragraph(anchor) = &mut blocks[anchor_idx].block {
            anchor.para_mark_status = match anchor.para_mark_status.take() {
                Some(TrackingStatus::Deleted(deleted)) => Some(TrackingStatus::Deleted(deleted)),
                Some(TrackingStatus::InsertedThenDeleted(stacked)) => {
                    Some(TrackingStatus::InsertedThenDeleted(stacked))
                }
                Some(TrackingStatus::Inserted(inserted)) => Some(
                    TrackingStatus::InsertedThenDeleted(Box::new(StackedRevision {
                        inserted,
                        deleted: next_revision(revision, rev_counter),
                    })),
                ),
                None if block_insertion.is_some() => Some(TrackingStatus::InsertedThenDeleted(
                    Box::new(StackedRevision {
                        inserted: block_insertion.expect("matched inserted block status"),
                        deleted: next_revision(revision, rev_counter),
                    }),
                )),
                Some(TrackingStatus::Normal) | None => Some(TrackingStatus::Deleted(
                    next_revision(revision, rev_counter),
                )),
            };
        }

        // A source-only deleted tail can follow a target-current paragraph in
        // a replacement region. The physical final paragraph must retain the
        // source's pre-existing final mark, but its CURRENT pPr must be the
        // accepted terminal's final pPr. Otherwise Accept leaves an empty
        // source-formatted sentinel after the target reading. Carry the source
        // final pPr as the exact previous snapshot and copy the anchor's
        // target-current pPr onto the sentinel; Reject restores the former,
        // Accept keeps the latter. This is the delete-tail dual of
        // `record_displaced_final_mark_properties`.
        if let Some(accepted_final_properties) = accepted_final_properties {
            let BlockNode::Paragraph(final_paragraph) = &mut blocks[last].block else {
                unreachable!("validated final paragraph");
            };
            apply_target_non_numbering_paragraph_properties(
                final_paragraph,
                &accepted_final_properties,
            );
            final_paragraph.numbering = accepted_final_properties.numbering.clone();
            final_paragraph.has_direct_numbering = accepted_final_properties.has_direct_numbering;
            final_paragraph.numbering_suppressed = accepted_final_properties.numbering_suppressed;
            final_paragraph.materialized_numbering =
                accepted_final_properties.materialized_numbering.clone();

            let current =
                crate::edit::snapshot_paragraph_formatting(final_paragraph, &deleted_revision);
            final_paragraph.formatting_change =
                (current != original_final_properties).then_some(original_final_properties);
        }
    }
}

/// Whether native Accept removes a top-level block before a paragraph-mark
/// join searches for its following paragraph.
///
/// This mirrors the engine's resolution rule at the comparison materializer
/// boundary. Besides a block-level deletion, a table vanishes when it had rows
/// and every row is deletion-class. Typed zero-width range markers occupy no
/// body position and therefore do not block the search either.
fn block_vanishes_on_accept(block: &TrackedBlock) -> bool {
    if matches!(
        block.status,
        TrackingStatus::Deleted(_) | TrackingStatus::InsertedThenDeleted(_)
    ) {
        return true;
    }
    match &block.block {
        BlockNode::Table(table) => {
            !table.rows.is_empty()
                && table.rows.iter().all(|row| {
                    matches!(
                        row.tracking_status,
                        Some(TrackingStatus::Deleted(_))
                            | Some(TrackingStatus::InsertedThenDeleted(_))
                    )
                })
        }
        BlockNode::OpaqueBlock(opaque) => opaque.range_marker.is_some(),
        BlockNode::Paragraph(_) => false,
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
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_table_structure_changed(
    blocks: &mut [TrackedBlock],
    table_id: &NodeId,
    target_table: &TableNode,
    diff: &TableDiffResult,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    context: &str,
    resolver: &dyn ChangePlanResolver,
) -> Result<(), MergeError> {
    let idx = find_block_index(blocks, table_id).ok_or_else(|| MergeError {
        kind: MergeErrorKind::InvalidPlan,
        message: "table for structure change not found in base model".to_string(),
        context: format!("{context}:{}", table_id.0),
    })?;

    let base_table = match &blocks[idx].block {
        BlockNode::Table(t) => t.clone(),
        _ => {
            return Err(MergeError {
                kind: MergeErrorKind::InvalidPlan,
                message: "TableStructureChanged references non-table block".to_string(),
                context: format!("{context}:{}", table_id.0),
            });
        }
    };

    // Row insertion/deletion carriers do not switch the owning table's
    // `w:tblPr`. Keeping the source table shell while adopting target rows is
    // therefore exact only when both readings have the same outer table
    // properties. A separate, Word-qualified `w:tblPrChange` must be composed
    // before this boundary can accept differing formatting.
    if base_table.formatting != target_table.formatting {
        return Err(MergeError {
            kind: MergeErrorKind::UnsupportedEdit,
            message: "table structure change cannot also switch outer table properties".to_string(),
            context: format!(
                "{context}:{}: an independent table-property carrier is required",
                table_id.0
            ),
        });
    }

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
            TableRowAlignment::Matched { old_row, new_row }
            | TableRowAlignment::Replacement { old_row, new_row } => {
                let content_relation = if matches!(alignment, TableRowAlignment::Replacement { .. })
                {
                    CellContentRelation::UnrelatedReplacement
                } else {
                    CellContentRelation::Related
                };
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
                        apply_cell_formatting_change(
                            &mut cell,
                            &new_cell_ref.formatting,
                            revision,
                            rev_counter,
                        );
                        reconcile_cell_blocks(
                            &mut cell,
                            new_cell_ref,
                            revision,
                            rev_counter,
                            context,
                            resolver,
                            content_relation,
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
#[allow(clippy::too_many_arguments)]
fn apply_paragraph_diff_in_cell(
    paragraph: &mut ParagraphNode,
    new_para: &ParagraphNode,
    old_block: &BlockNode,
    new_block: &BlockNode,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    context: &str,
    resolver: &dyn ChangePlanResolver,
) -> Result<(), MergeError> {
    refuse_unstable_cnf_style_change(paragraph, new_para, context)?;

    let old_inlines = paragraph.all_inlines_owned();
    let new_inlines = new_para.all_inlines_owned();
    let inline_changes = resolver.paragraph_changes(&old_inlines, &new_inlines);

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
        kind: MergeErrorKind::InvalidPlan,
        message: format!("cell paragraph inline diff failed: {}", e.message),
        context: context.to_string(),
    })?;

    let numbering_changed = !crate::domain::paragraph_numbering_state_eq(paragraph, new_para);
    let paragraph_properties_changed =
        !crate::domain::paragraph_property_state_eq(paragraph, new_para);
    let paragraph_mark_properties_changed =
        !crate::domain::paragraph_mark_property_state_eq(paragraph, new_para);
    let original_formatting_snapshot =
        (paragraph_properties_changed || paragraph_mark_properties_changed).then(|| {
            let property_revision = next_revision(revision, rev_counter);
            let mut snapshot =
                crate::edit::snapshot_paragraph_formatting(paragraph, &property_revision);
            snapshot.carrier = match (
                paragraph_properties_changed,
                paragraph_mark_properties_changed,
            ) {
                (true, false) => {
                    crate::domain::ParagraphFormattingChangeCarrier::ParagraphProperties
                }
                (false, true) => {
                    crate::domain::ParagraphFormattingChangeCarrier::ParagraphMarkProperties
                }
                (true, true) => {
                    crate::domain::ParagraphFormattingChangeCarrier::ParagraphAndMarkProperties
                }
                (false, false) => unreachable!("formatting snapshot requires a changed carrier"),
            };
            snapshot
        });
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
            paragraph.has_direct_numbering = new_para.has_direct_numbering;
            paragraph.numbering_suppressed = new_para.numbering_suppressed;
            prefix_was_materialized = true;
        }
    }

    // Paragraph formatting changes (same as apply_block_modified).
    // Captured before prefix handling; includes authored direct/suppressed state.
    if paragraph_properties_changed || paragraph_mark_properties_changed {
        paragraph.formatting_change = original_formatting_snapshot;
        if paragraph_properties_changed {
            apply_target_non_numbering_paragraph_properties(paragraph, new_para);
        } else {
            paragraph.paragraph_mark_marks = new_para.paragraph_mark_marks.clone();
            paragraph.paragraph_mark_style_props = new_para.paragraph_mark_style_props.clone();
            paragraph.paragraph_mark_rfonts = new_para.paragraph_mark_rfonts.clone();
            paragraph.paragraph_mark_rpr_off = new_para.paragraph_mark_rpr_off;
        }
        if numbering_changed && !prefix_was_materialized {
            paragraph.numbering = new_para.numbering.clone();
            // Carry the target's numbering provenance (see apply_block_modified).
            paragraph.has_direct_numbering = new_para.has_direct_numbering;
            paragraph.numbering_suppressed = new_para.numbering_suppressed;
            if new_para.has_resolved_numbering() {
                paragraph.literal_prefix = None;
            }
        }
    }

    inject_structural_markers_for_compare(
        &mut final_segments,
        &original_segments,
        new_para.segments.as_slice(),
        revision,
        rev_counter,
    );

    prune_empty_text_inlines(&mut final_segments);
    paragraph.segments = final_segments;
    Ok(())
}

/// Apply the complete non-numbering `w:pPr` state from `target`.
///
/// Numbering is deliberately excluded because prefix lowering may represent
/// that state partly as tracked inline content. Keeping the remaining
/// paragraph-property transition in one concrete helper prevents body and
/// table-cell materialization from drifting apart.
fn apply_target_non_numbering_paragraph_properties(
    paragraph: &mut ParagraphNode,
    target: &ParagraphNode,
) {
    paragraph.style_id = target.style_id.clone();
    paragraph.align = target.align.clone();
    paragraph.has_direct_align = target.has_direct_align;
    paragraph.indent = target.indent.clone();
    paragraph.has_direct_indent = target.has_direct_indent;
    paragraph.authored_indent = target.authored_indent.clone();
    paragraph.spacing = target.spacing.clone();
    paragraph.has_direct_spacing = target.has_direct_spacing;
    paragraph.authored_spacing = target.authored_spacing.clone();
    paragraph.borders = target.borders.clone();
    paragraph.has_direct_borders = target.has_direct_borders;
    paragraph.keep_next = target.keep_next;
    paragraph.has_direct_keep_next = target.has_direct_keep_next;
    paragraph.keep_lines = target.keep_lines;
    paragraph.has_direct_keep_lines = target.has_direct_keep_lines;
    paragraph.page_break_before = target.page_break_before;
    paragraph.has_direct_page_break_before = target.has_direct_page_break_before;
    paragraph.widow_control = target.widow_control;
    paragraph.has_direct_widow_control = target.has_direct_widow_control;
    paragraph.contextual_spacing = target.contextual_spacing;
    paragraph.has_direct_contextual_spacing = target.has_direct_contextual_spacing;
    paragraph.shading = target.shading.clone();
    paragraph.has_direct_shading = target.has_direct_shading;
    paragraph.tab_stops = target.tab_stops.clone();
    paragraph.effective_tab_stops_rel = target.effective_tab_stops_rel.clone();
    paragraph.outline_lvl = target.outline_lvl;
    paragraph.heading_level = target.heading_level.clone();
    paragraph.mirror_indents = target.mirror_indents;
    paragraph.auto_space_de = target.auto_space_de;
    paragraph.auto_space_dn = target.auto_space_dn;
    paragraph.bidi = target.bidi;
    paragraph.text_alignment = target.text_alignment.clone();
    paragraph.text_direction = target.text_direction.clone();
    paragraph.suppress_auto_hyphens = target.suppress_auto_hyphens;
    paragraph.snap_to_grid = target.snap_to_grid;
    paragraph.overflow_punct = target.overflow_punct;
    paragraph.adjust_right_ind = target.adjust_right_ind;
    paragraph.word_wrap = target.word_wrap;
    paragraph.frame_pr = target.frame_pr.clone();
    paragraph.paragraph_mark_marks = target.paragraph_mark_marks.clone();
    paragraph.paragraph_mark_style_props = target.paragraph_mark_style_props.clone();
    paragraph.paragraph_mark_rfonts = target.paragraph_mark_rfonts.clone();
    paragraph.paragraph_mark_rpr_off = target.paragraph_mark_rpr_off;
    paragraph.section_property_change = target.section_property_change.clone();
    paragraph.section_properties = target.section_properties.clone();
    paragraph.cnf_style = target.cnf_style.clone();
    paragraph.preserved_ppr = target.preserved_ppr.clone();
}

/// A distinct prior/current `w:cnfStyle` cannot be carried by a stable Word
/// `w:pPrChange`: Word normalizes the previous snapshot to the current value
/// when saving. Refuse before constructing a redline whose Reject projection
/// cannot remain equal to the source document.
fn refuse_unstable_cnf_style_change(
    source: &ParagraphNode,
    target: &ParagraphNode,
    context: &str,
) -> Result<(), MergeError> {
    if source.cnf_style == target.cnf_style {
        return Ok(());
    }

    Err(MergeError {
        kind: MergeErrorKind::UnsupportedEdit,
        message: "paragraph cnfStyle change has no stable Word pPrChange encoding".to_string(),
        context: format!(
            "{context}: source paragraph {} and target paragraph {} have distinct cnfStyle; \
             Word rewrites the previous cnfStyle to the current value, so Accept/Reject \
             equivalence cannot be guaranteed",
            source.id.0, target.id.0
        ),
    })
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

/// Lower one whole side-only block inside a table-cell story.
///
/// Cell stories store bare [`BlockNode`] values, so they cannot use a
/// [`TrackedBlock`] insertion wrapper. Paragraphs therefore carry insertion
/// on their content and mark, while tables carry it on every row. This is a
/// physical encoding operation only; callers must already hold a verified
/// side-only semantic region.
fn reconcile_cell_blocks(
    cell: &mut TableCellNode,
    new_cell: &TableCellNode,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    context: &str,
    resolver: &dyn ChangePlanResolver,
    content_relation: CellContentRelation,
) -> Result<(), MergeError> {
    let old_fps: Vec<String> = cell.blocks.iter().map(block_fingerprint).collect();
    let new_fps: Vec<String> = new_cell.blocks.iter().map(block_fingerprint).collect();

    // Fast path: identical fingerprints means no changes.
    if content_relation == CellContentRelation::Related && old_fps == new_fps {
        return Ok(());
    }

    let ops = match content_relation {
        CellContentRelation::Related => {
            similar::capture_diff_slices_deadline(Algorithm::Patience, &old_fps, &new_fps, None)
        }
        CellContentRelation::UnrelatedReplacement => vec![DiffOp::Replace {
            old_index: 0,
            old_len: cell.blocks.len(),
            new_index: 0,
            new_len: new_cell.blocks.len(),
        }],
    };
    let unrelated_resolver = UnrelatedContentResolver(resolver);
    let content_resolver: &dyn ChangePlanResolver = match content_relation {
        CellContentRelation::Related => resolver,
        CellContentRelation::UnrelatedReplacement => &unrelated_resolver,
    };

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
                                content_resolver,
                            )?;
                            merged_blocks.push(BlockNode::Paragraph(para));
                        }
                        (BlockNode::Table(old_t), BlockNode::Table(new_t)) => {
                            let mut inner = old_t.clone();
                            if let Some(nested_diff) = content_resolver
                                .nested_table_change(old_t, new_t, 0)
                                .map_err(|e| MergeError {
                                    kind: MergeErrorKind::InvalidPlan,
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
                                            content_resolver,
                                        )?;
                                    }
                                    NestedTableDiffKind::CellsModified { cell_changes: nc } => {
                                        apply_nested_table_cells_modified(
                                            &mut inner,
                                            nc,
                                            revision,
                                            rev_counter,
                                            context,
                                            content_resolver,
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
                // Every table cell requires one final paragraph. If the diff
                // independently deletes and inserts that boundary, native
                // Word Reject keeps the source paragraph and strands the
                // emptied target paragraph beside it. Reuse the source-final
                // physical paragraph for the target-final paragraph instead.
                // Only the boundary is shared: source content remains deleted
                // and target content remains inserted through the ordinary
                // paragraph diff.
                let shares_required_final_paragraph = content_relation
                    == CellContentRelation::UnrelatedReplacement
                    && *old_len > 0
                    && *new_len > 0
                    && old_index + old_len == cell.blocks.len()
                    && new_index + new_len == new_cell.blocks.len()
                    && matches!(
                        cell.blocks[old_index + old_len - 1],
                        BlockNode::Paragraph(_)
                    )
                    && matches!(
                        new_cell.blocks[new_index + new_len - 1],
                        BlockNode::Paragraph(_)
                    );
                let old_side_only_len = if shares_required_final_paragraph {
                    old_len - 1
                } else {
                    *old_len
                };
                let new_side_only_len = if shares_required_final_paragraph {
                    new_len - 1
                } else {
                    *new_len
                };

                for i in 0..old_side_only_len {
                    let mut block = cell.blocks[old_index + i].clone();
                    match &mut block {
                        BlockNode::Paragraph(p) => mark_paragraph_deleted(p, revision, rev_counter),
                        BlockNode::Table(t) => mark_table_deleted(t, revision, rev_counter),
                        BlockNode::OpaqueBlock(_) => {}
                    }
                    merged_blocks.push(block);
                }
                for i in 0..new_side_only_len {
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

                if shares_required_final_paragraph {
                    let old_block = &cell.blocks[old_index + old_len - 1];
                    let new_block = &new_cell.blocks[new_index + new_len - 1];
                    let (BlockNode::Paragraph(old_paragraph), BlockNode::Paragraph(new_paragraph)) =
                        (old_block, new_block)
                    else {
                        unreachable!("qualified cell-final boundary must contain paragraphs")
                    };
                    let mut paragraph = old_paragraph.clone();
                    let paragraph_context = format!(
                        "{context}:cell:{}:final-paragraph:{}",
                        cell.id.0, old_paragraph.id.0
                    );
                    apply_paragraph_diff_in_cell(
                        &mut paragraph,
                        new_paragraph,
                        old_block,
                        new_block,
                        revision,
                        rev_counter,
                        &paragraph_context,
                        content_resolver,
                    )?;
                    merged_blocks.push(BlockNode::Paragraph(paragraph));
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
                // Delete the CURRENT reading without erasing any pre-existing
                // revision layer. In particular, an inserted run inside a row
                // that is now deleted is `InsertedThenDeleted`: rejecting the
                // row deletion restores the pending insertion, after which
                // rejecting that insertion still removes it. Flattening every
                // segment into one fresh `Deleted` carrier loses the inner
                // insertion and makes reject-all retain text that was absent
                // from the baseline.
                for segment in &mut p.segments {
                    segment.status = match &segment.status {
                        TrackingStatus::Normal => {
                            TrackingStatus::Deleted(next_revision(revision, rev_counter))
                        }
                        TrackingStatus::Inserted(inserted) => {
                            TrackingStatus::InsertedThenDeleted(Box::new(StackedRevision {
                                inserted: inserted.clone(),
                                deleted: next_revision(revision, rev_counter),
                            }))
                        }
                        // Already-deleted content is outside the current
                        // reading, so this row deletion does not target it.
                        other => other.clone(),
                    };
                }
                if Some(idx) != last_para_idx {
                    p.para_mark_status = Some(match &p.para_mark_status {
                        Some(TrackingStatus::Inserted(inserted)) => {
                            TrackingStatus::InsertedThenDeleted(Box::new(StackedRevision {
                                inserted: inserted.clone(),
                                deleted: next_revision(revision, rev_counter),
                            }))
                        }
                        Some(TrackingStatus::Deleted(_))
                        | Some(TrackingStatus::InsertedThenDeleted(_)) => p
                            .para_mark_status
                            .clone()
                            .expect("matched an existing paragraph-mark status"),
                        Some(TrackingStatus::Normal) | None => {
                            TrackingStatus::Deleted(next_revision(revision, rev_counter))
                        }
                    });
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
    resolver: &dyn ChangePlanResolver,
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
                        kind: MergeErrorKind::InvalidPlan,
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
                    resolver,
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
                        kind: MergeErrorKind::InvalidPlan,
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
                    resolver,
                )?;
                pending_renames.push((story_idx, target_part_name.clone()));
            }
            DiffChange::HeaderDeleted { part_name, .. } => {
                if story_name != "header" {
                    continue;
                }
                let Some(story) = base_stories.iter_mut().find(|s| get_key(s) == part_name) else {
                    return Err(MergeError {
                        kind: MergeErrorKind::InvalidPlan,
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
                        kind: MergeErrorKind::InvalidPlan,
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
                        kind: MergeErrorKind::InvalidPlan,
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
                        kind: MergeErrorKind::InvalidPlan,
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
                kind: MergeErrorKind::InvalidPlan,
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
    resolver: &dyn ChangePlanResolver,
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
                        kind: MergeErrorKind::InvalidPlan,
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
                    resolver,
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
                        kind: MergeErrorKind::InvalidPlan,
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
                    resolver,
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
                        kind: MergeErrorKind::InvalidPlan,
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
                    resolver,
                )?;
            }
            DiffChange::FootnoteDeleted { id, .. } => {
                if story_name != "footnote" {
                    continue;
                }
                let Some(story) = base_stories.iter_mut().find(|s| get_id(s) == id) else {
                    return Err(MergeError {
                        kind: MergeErrorKind::InvalidPlan,
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
                        kind: MergeErrorKind::InvalidPlan,
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
                        kind: MergeErrorKind::InvalidPlan,
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
                        kind: MergeErrorKind::InvalidPlan,
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
                        kind: MergeErrorKind::InvalidPlan,
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

/// Encode the two effective `titlePg` readings explicitly when they differ.
///
/// Word interprets a `sectPrChange` relative to the current section. For this
/// toggle, omission is the effective off state in an ordinary section, but it
/// is not a reliable way to switch an on-state through a section revision:
/// both the current and previous readings need a physical on/off value.
fn word_relative_section_states(
    base: Option<&SectionProperties>,
    target: Option<&SectionProperties>,
) -> Result<(SectionProperties, SectionProperties), MergeError> {
    let (Some(base), Some(target)) = (base, target) else {
        return Err(MergeError {
            kind: MergeErrorKind::UnsupportedEdit,
            message: "body section presence transitions have no qualified native Word carrier"
                .to_string(),
            context: "sectPrChange requires a current section and does not track adding or removing the body sectPr"
                .to_string(),
        });
    };
    let mut previous = base.clone();
    let mut current = target.clone();
    let base_title_page = base.title_page.unwrap_or(false);
    let target_title_page = target.title_page.unwrap_or(false);
    if base_title_page != target_title_page {
        previous.title_page = Some(base_title_page);
        current.title_page = Some(target_title_page);
    }
    Ok((previous, current))
}

/// Reconcile the package-local comment-id namespaces before comparing two
/// documents.
///
/// `w:id` on comments and their three body anchors is only a link inside one
/// package. Two unrelated documents may both call different comments `0`.
/// Treating that numeric collision as identity cross-wires the target anchor
/// to the base story and can emit duplicate marker triples that Word and
/// LibreOffice refuse. Comments that demonstrably match keep the base id;
/// every other target comment gets an id that is free in the combined
/// namespace. The target story and every typed anchor are rewritten together.
#[doc(hidden)]
pub fn materialize_change_plan(
    base: &CanonDoc,
    target: &CanonDoc,
    diff: &crate::domain::DocumentDiff,
    revision: &RevisionInfo,
    resolver: &dyn ChangePlanResolver,
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
        resolver,
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
        resolver,
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
        resolver,
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
        resolver,
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
        resolver,
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
        resolver,
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

    // A merge can combine a retained manual label with a deletion whose first
    // source run begins at the label/body separator. `literal_prefix` cannot
    // represent that mixed tracking boundary by itself: serialization rejoins
    // the separator to the deleted body, and reopen otherwise sees a different
    // segment shape. Restore the explicit normal-prefix + tracked-body model at
    // the producer boundary.
    // Only paragraphs this merge PUT INTO that shape qualify. A paragraph the
    // diff merely edited keeps its label in `literal_prefix`, where the
    // serializer and the redline reader both expect it; materializing every
    // changed paragraph moves the label into the body run stream and changes
    // run grouping the caller never asked to change. One already split in the
    // base is the source document's own state and is likewise left alone.
    let mut base_split_ids = HashSet::new();
    collect_split_tracking_boundary_prefix_ids(&base.blocks, &mut base_split_ids);
    let mut merged_split_ids = HashSet::new();
    collect_split_tracking_boundary_prefix_ids(&merged.blocks, &mut merged_split_ids);
    let produced_split_ids: HashSet<NodeId> = merged_split_ids
        .difference(&base_split_ids)
        .cloned()
        .collect();
    materialize_split_tracking_boundary_prefixes(&mut merged.blocks, &produced_split_ids);
    materialize_block_tracked_prefixes(&mut merged.blocks);

    // Track section property changes (w:sectPrChange §17.13.5.32).
    //
    // Clear the base's parsed sectPrChange — if the base archive already had one,
    // it lives inside the raw sectPr element and will be preserved opaquely by the
    // serializer when we don't set a new change here.
    merged.body_section_property_change = None;

    // When the target's body section properties differ from the base, record
    // the change so the serializer can emit a sectPrChange element.
    if base.body_section_properties != target.body_section_properties {
        // CT_SectPrBase does not include EG_HdrFtrReferences, so
        // header/footer refs are excluded when freezing the previous
        // state for a sectPrChange.
        let (mut sp_for_change, current_sp) = word_relative_section_states(
            base.body_section_properties.as_ref(),
            target.body_section_properties.as_ref(),
        )?;
        sp_for_change.header_refs.clear();
        sp_for_change.footer_refs.clear();
        let element =
            crate::runtime::section_properties_to_element(&sp_for_change, None, None, None);
        let mut previous_properties_raw = Vec::new();
        let config = xmltree::EmitterConfig::new().write_document_declaration(false);
        let _ = element.write_with_config(&mut previous_properties_raw, config);
        merged.body_section_properties = Some(current_sp);
        merged.body_section_property_change = Some(SectionPropertyChange {
            revision: next_revision(revision, &mut rev_counter),
            previous_properties_raw,
        });
    }

    // H2: one unified body-state validator after the diff/redline merge
    // producer (post normalize_final_mark_attribution).
    debug_assert_body_invariants(&merged, "merge_diff");
    Ok(MergeResult { doc: merged })
}

fn collect_split_tracking_boundary_prefix_ids(blocks: &[TrackedBlock], ids: &mut HashSet<NodeId>) {
    fn visit_block(block: &BlockNode, outer_tracked: bool, ids: &mut HashSet<NodeId>) {
        match block {
            BlockNode::Paragraph(paragraph) => {
                let source_runs = paragraph
                    .literal_prefix_leading_rpr
                    .as_deref()
                    .map(|provenance| provenance.source_runs.as_slice())
                    .unwrap_or_default();
                if source_runs
                    .iter()
                    .position(|source| source.joins_body)
                    .is_some_and(|index| index > 0)
                    && (outer_tracked
                        || (paragraph
                            .para_mark_status
                            .as_ref()
                            .is_some_and(|status| !matches!(status, TrackingStatus::Normal))
                            && paragraph
                                .segments
                                .iter()
                                .any(|segment| !matches!(segment.status, TrackingStatus::Normal))))
                {
                    ids.insert(paragraph.id.clone());
                }
            }
            BlockNode::Table(table) => {
                for row in &table.rows {
                    for cell in &row.cells {
                        for nested in &cell.blocks {
                            visit_block(nested, false, ids);
                        }
                    }
                }
            }
            BlockNode::OpaqueBlock(_) => {}
        }
    }

    for tracked in blocks {
        visit_block(
            &tracked.block,
            !matches!(tracked.status, TrackingStatus::Normal),
            ids,
        );
    }
}

fn materialize_split_tracking_boundary_prefixes(
    blocks: &mut [TrackedBlock],
    changed_ids: &HashSet<NodeId>,
) {
    fn clear_literal_prefix(paragraph: &mut ParagraphNode) {
        paragraph.literal_prefix = None;
        paragraph.literal_prefix_marks.clear();
        paragraph.literal_prefix_style_props = StyleProps::default();
        paragraph.literal_prefix_rpr_authored = RunRprAuthored::default();
        paragraph.literal_prefix_leading_rpr = None;
        paragraph.literal_prefix_trailing_rpr = None;
        paragraph.literal_prefix_leading_tab_twips = None;
        paragraph.literal_prefix_leading_tab_count = 0;
        paragraph.literal_prefix_leading_ws.clear();
        paragraph.literal_prefix_trailing_ws.clear();
        paragraph.literal_prefix_has_trailing_tab = false;
        paragraph.literal_prefix_trailing_tab_stop_twips = None;
        paragraph.rendered_text = None;
    }

    fn source_inline(
        paragraph_id: &NodeId,
        index: usize,
        source: &crate::domain::LiteralPrefixSourceRun,
    ) -> InlineNode {
        InlineNode::from(TextNode {
            id: NodeId::from(format!(
                "{}_tracking_boundary_prefix_{index}",
                paragraph_id.0
            )),
            text_role: None,
            text: source.text.clone(),
            marks: source.marks.clone(),
            style_props: source.style_props.clone(),
            rpr_authored: source.rpr_authored,
            source_run_attrs: source.source_run_attrs.clone(),
            formatting_change: None,
        })
    }

    fn visit_paragraph(
        paragraph: &mut ParagraphNode,
        outer_tracked: bool,
        changed_ids: &HashSet<NodeId>,
    ) {
        if !changed_ids.contains(&paragraph.id) || paragraph.literal_prefix.is_none() {
            return;
        }
        let source_runs = paragraph
            .literal_prefix_leading_rpr
            .as_deref()
            .map(|provenance| provenance.source_runs.clone())
            .unwrap_or_default();
        let Some(join_index) = source_runs.iter().position(|source| source.joins_body) else {
            return;
        };
        if join_index == 0 || source_runs[..join_index].iter().any(|run| run.joins_body) {
            return;
        }
        let tracked_segment_index = paragraph.segments.iter().position(|segment| {
            !matches!(segment.status, TrackingStatus::Normal)
                && segment
                    .inlines
                    .iter()
                    .any(|inline| matches!(inline, InlineNode::Text(_)))
        });
        if tracked_segment_index.is_none() && outer_tracked {
            let Some(segment_index) = paragraph.segments.iter().position(|segment| {
                segment
                    .inlines
                    .iter()
                    .any(|inline| matches!(inline, InlineNode::Text(_)))
            }) else {
                return;
            };
            let prefix_inlines: Vec<_> = source_runs
                .iter()
                .enumerate()
                .map(|(index, source)| source_inline(&paragraph.id, index, source))
                .collect();
            paragraph.segments[segment_index]
                .inlines
                .splice(0..0, prefix_inlines);
            clear_literal_prefix(paragraph);
            return;
        }
        let Some(segment_index) = tracked_segment_index else {
            return;
        };

        let normal_inlines = source_runs[..join_index]
            .iter()
            .enumerate()
            .map(|(index, source)| source_inline(&paragraph.id, index, source))
            .collect();
        let tracked_inlines: Vec<_> = source_runs[join_index..]
            .iter()
            .enumerate()
            .map(|(index, source)| source_inline(&paragraph.id, join_index + index, source))
            .collect();
        paragraph.segments[segment_index]
            .inlines
            .splice(0..0, tracked_inlines);
        paragraph.segments.insert(
            segment_index,
            TrackedSegment {
                status: TrackingStatus::Normal,
                inlines: normal_inlines,
            },
        );
        clear_literal_prefix(paragraph);
    }

    fn visit_block(block: &mut BlockNode, outer_tracked: bool, changed_ids: &HashSet<NodeId>) {
        match block {
            BlockNode::Paragraph(paragraph) => {
                visit_paragraph(paragraph, outer_tracked, changed_ids)
            }
            BlockNode::Table(table) => {
                for row in &mut table.rows {
                    for cell in &mut row.cells {
                        for nested in &mut cell.blocks {
                            visit_block(nested, false, changed_ids);
                        }
                    }
                }
            }
            BlockNode::OpaqueBlock(_) => {}
        }
    }

    for tracked in blocks {
        visit_block(
            &mut tracked.block,
            !matches!(tracked.status, TrackingStatus::Normal),
            changed_ids,
        );
    }
}

/// Normalize the mixed wire shape where a retained manual label ends in one
/// source run and its separator shares the following tracked body run. This is
/// an import-edge transformation and must run before revision identities are
/// minted, so the identity digest is derived from the stable explicit segment
/// model that serialization will reproduce.
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
fn sync_non_numbering_properties(
    para: &mut ParagraphNode,
    target_para: &ParagraphNode,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
) {
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
    para.paragraph_mark_rfonts = target_para.paragraph_mark_rfonts.clone();
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
    sync_inline_formatting(
        &mut para.segments,
        &target_para.segments,
        revision,
        rev_counter,
    );
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
    revision: &RevisionInfo,
    rev_counter: &mut u32,
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

    let target_formatting_is_uniform = target_fmt.first().is_some_and(|first| {
        target_fmt.iter().all(|fmt| {
            fmt.marks == first.marks
                && fmt.style_props == first.style_props
                && fmt.rpr_authored == first.rpr_authored
        })
    });
    let target_visible_char_count: usize =
        target_fmt.iter().map(|fmt| fmt.text.chars().count()).sum();

    if target_formatting_is_uniform
        && base_segments
            .iter()
            .all(|seg| matches!(seg.status, TrackingStatus::Normal))
        && base_visible_char_count == target_visible_char_count
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
                            if let Some(formatting_change) = &mut chunk.formatting_change {
                                formatting_change.revision_id =
                                    next_revision(revision, rev_counter).revision_id;
                                formatting_change.identity = 0;
                            }
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
fn sync_target_formatting_in_table(
    table: &mut TableNode,
    target_table: &TableNode,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
) {
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
                        sync_non_numbering_properties(para, target_para, revision, rev_counter);
                    }
                    (BlockNode::Table(nested), BlockNode::Table(nested_target)) => {
                        sync_target_formatting_in_table(
                            nested,
                            nested_target,
                            revision,
                            rev_counter,
                        );
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
    revision: &RevisionInfo,
    rev_counter: &mut u32,
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
                            sync_non_numbering_properties(para, target_para, revision, rev_counter);
                        }
                        (BlockNode::Table(table), BlockNode::Table(target_table)) => {
                            sync_target_formatting_in_table(
                                table,
                                target_table,
                                revision,
                                rev_counter,
                            );
                        }
                        _ => {}
                    }
                } else {
                    // Block was not modified by the diff — preserve it exactly.
                    // In particular, keep a hoisted literal prefix structural:
                    // materializing it here replaces its imported source-run
                    // witness with one synthetic TextNode, so a later rebuild
                    // cannot retain the original w:r boundaries. Prefixes only
                    // need materialization when a changed/inserted/deleted block
                    // must express them as revision content.
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
            if !para.has_resolved_numbering() && para.literal_prefix.is_none() {
                return;
            }
            // Structural numbering is preserved (including bullets).
            // Word generates the label from the numbering definition.
            if para.has_resolved_numbering() {
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
                if !para.has_resolved_numbering() && para.literal_prefix.is_none() {
                    continue;
                }
                // Structural numbering is preserved.
                if para.has_resolved_numbering() {
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

#[cfg(test)]
mod paragraph_mark_status_tests {
    use super::*;

    fn revision(id: u32) -> RevisionInfo {
        RevisionInfo {
            revision_id: id,
            author: Some("Reviewer".to_string()),
            date: None,
            apply_op_id: None,
            identity: id,
        }
    }

    fn paragraph(id: &str, text: &str, status: TrackingStatus) -> TrackedBlock {
        TrackedBlock {
            status,
            block: ParagraphNode::new_story_body(id, text, None).into(),
            move_id: None,
            block_sdt_wrap: None,
        }
    }

    fn table(id: &str, status: TrackingStatus) -> TrackedBlock {
        TrackedBlock {
            status,
            block: TableNode {
                id: NodeId::from(id.to_string()),
                rows: Vec::new(),
                structure_hash: "empty".to_string(),
                formatting: Default::default(),
                formatting_change: None,
            }
            .into(),
            move_id: None,
            block_sdt_wrap: None,
        }
    }

    #[test]
    fn homogeneous_inserted_suffix_marks_the_surviving_boundary() {
        let base_revision = revision(10);
        let mut counter = 0;
        let mut blocks = vec![
            paragraph("source", "Source", TrackingStatus::Normal),
            paragraph(
                "target-one",
                "Target one",
                TrackingStatus::Inserted(revision(11)),
            ),
            paragraph(
                "target-two",
                "Target two",
                TrackingStatus::Inserted(revision(12)),
            ),
        ];

        annotate_paragraph_mark_status(&mut blocks, &base_revision, &mut counter);

        let BlockNode::Paragraph(source) = &blocks[0].block else {
            panic!("test source block must remain a paragraph");
        };
        assert!(matches!(
            source.para_mark_status,
            Some(TrackingStatus::Inserted(_))
        ));
    }

    #[test]
    fn mixed_inserted_deleted_suffix_does_not_mark_the_surviving_boundary() {
        let base_revision = revision(20);
        let mut counter = 0;
        let mut blocks = vec![
            paragraph("source-heading", "Book Catalog", TrackingStatus::Normal),
            paragraph(
                "target-region",
                "Replacement table introduction",
                TrackingStatus::Inserted(revision(21)),
            ),
            paragraph(
                "source-region",
                "Deleted source catalog",
                TrackingStatus::Deleted(revision(22)),
            ),
        ];

        annotate_paragraph_mark_status(&mut blocks, &base_revision, &mut counter);

        let BlockNode::Paragraph(source) = &blocks[0].block else {
            panic!("test source block must remain a paragraph");
        };
        assert_eq!(source.para_mark_status, None);
        assert_eq!(counter, 0, "a refused annotation must mint no revision");
    }

    #[test]
    fn inserted_table_suffix_does_not_mark_the_surviving_paragraph_boundary() {
        let base_revision = revision(30);
        let mut counter = 0;
        let mut blocks = vec![
            paragraph("source", "Source", TrackingStatus::Normal),
            table("target-table", TrackingStatus::Inserted(revision(31))),
            paragraph("target-tail", "", TrackingStatus::Inserted(revision(32))),
        ];

        annotate_paragraph_mark_status(&mut blocks, &base_revision, &mut counter);

        let BlockNode::Paragraph(source) = &blocks[0].block else {
            panic!("test source block must remain a paragraph");
        };
        assert_eq!(source.para_mark_status, None);
        assert_eq!(
            counter, 0,
            "a paragraph-to-table boundary must mint no paragraph revision"
        );
    }

    #[test]
    fn inserted_paragraphs_before_a_table_do_not_mark_the_surviving_boundary() {
        let base_revision = revision(40);
        let mut counter = 0;
        let mut blocks = vec![
            paragraph("source", "Source", TrackingStatus::Normal),
            paragraph(
                "target-paragraph",
                "Target",
                TrackingStatus::Inserted(revision(41)),
            ),
            table("target-table", TrackingStatus::Inserted(revision(42))),
            paragraph("target-tail", "", TrackingStatus::Inserted(revision(43))),
        ];

        annotate_paragraph_mark_status(&mut blocks, &base_revision, &mut counter);

        let BlockNode::Paragraph(source) = &blocks[0].block else {
            panic!("test source block must remain a paragraph");
        };
        assert_eq!(source.para_mark_status, None);
        assert_eq!(
            counter, 0,
            "an inserted table must keep its carrier independent of paragraph marks"
        );
    }
}

#[cfg(test)]
mod run_formatting_history_tests {
    use super::*;

    fn revision() -> RevisionInfo {
        RevisionInfo {
            revision_id: 10,
            identity: 10,
            author: Some("Reviewer".to_string()),
            date: None,
            apply_op_id: None,
        }
    }

    fn text(id: &str, value: &str, font: Option<&str>) -> InlineNode {
        let style_props = StyleProps {
            font_family: font.map(Into::into),
            ..StyleProps::default()
        };
        InlineNode::from(TextNode {
            id: NodeId::from(id.to_string()),
            text_role: None,
            text: value.to_string(),
            marks: Vec::new(),
            style_props,
            rpr_authored: RunRprAuthored {
                font_family: font.is_some(),
                ..RunRprAuthored::default()
            },
            source_run_attrs: Vec::new(),
            formatting_change: None,
        })
    }

    fn hard_break(id: &str, font: Option<&str>) -> InlineNode {
        InlineNode::HardBreak(crate::domain::HardBreakNode {
            id: NodeId::from(id.to_string()),
            break_type: crate::domain::BreakType::TextWrapping,
            type_is_explicit: false,
            clear: None,
            wrapper_marks: Vec::new(),
            wrapper_style_props: StyleProps {
                font_family: font.map(Into::into),
                ..StyleProps::default()
            },
            wrapper_rpr_authored: RunRprAuthored {
                font_family: font.is_some(),
                ..RunRprAuthored::default()
            },
            source_run_attrs: Vec::new(),
            formatting_change: None,
            joins_following_text_run: false,
        })
    }

    #[test]
    fn run_history_covers_text_and_each_break_when_target_run_boundaries_differ() {
        let mut source = ParagraphNode::new_story_body("paragraph", "unused", None);
        source.segments = crate::domain::normal_segment(vec![
            text("source-one", "Alpha", Some("Arial")),
            text("source-two", "Beta", Some("Arial")),
            hard_break("source-break-one", Some("Arial")),
            hard_break("source-break-two", Some("Arial")),
        ]);
        let mut target = ParagraphNode::new_story_body("paragraph", "unused", None);
        target.segments = crate::domain::normal_segment(vec![
            text("target-one", "AlphaBe", None),
            text("target-two", "ta", None),
            hard_break("target-break-one", None),
            hard_break("target-break-two", None),
        ]);

        let inline_changes = ComparisonPlanResolver
            .paragraph_changes(&source.all_inlines_owned(), &target.all_inlines_owned());
        let mut blocks = vec![TrackedBlock {
            status: TrackingStatus::Normal,
            block: source.clone().into(),
            move_id: None,
            block_sdt_wrap: None,
        }];
        let mut revision_counter = revision().revision_id;

        apply_block_modified(
            &mut blocks,
            &source.id,
            &inline_changes,
            &target.clone().into(),
            &revision(),
            &mut revision_counter,
            "body",
        )
        .expect("formatting history should materialize");
        let BlockNode::Paragraph(materialized) = &mut blocks[0].block else {
            panic!("fixture must remain a paragraph")
        };
        sync_non_numbering_properties(materialized, &target, &revision(), &mut revision_counter);

        let carriers: Vec<&crate::domain::FormattingChange> = materialized
            .segments
            .iter()
            .flat_map(|segment| segment.inlines.iter())
            .filter_map(|inline| match inline {
                InlineNode::Text(text) => text.formatting_change.as_ref(),
                InlineNode::HardBreak(hard_break) => hard_break.formatting_change.as_ref(),
                _ => None,
            })
            .collect();
        assert_eq!(carriers.len(), 3, "text and both breaks need history");
        let ids: std::collections::HashSet<u32> =
            carriers.iter().map(|change| change.revision_id).collect();
        assert_eq!(
            ids.len(),
            carriers.len(),
            "each physical carrier needs a unique id"
        );
        assert!(carriers.iter().all(|change| {
            change.previous_style_props.font_family.as_deref() == Some("Arial")
                && change.previous_rpr_authored.font_family
        }));

        let text_nodes: Vec<&TextNode> = materialized
            .segments
            .iter()
            .flat_map(|segment| segment.inlines.iter())
            .filter_map(|inline| match inline {
                InlineNode::Text(text) => Some(text.as_ref()),
                _ => None,
            })
            .collect();
        assert_eq!(
            text_nodes.len(),
            1,
            "lexical target run boundaries are not semantic"
        );
        assert_eq!(text_nodes[0].text, "AlphaBeta");
        assert_eq!(text_nodes[0].style_props.font_family, None);
        assert!(!text_nodes[0].rpr_authored.font_family);
    }
}

#[cfg(test)]
mod table_structure_carrier_tests {
    use super::*;

    fn table(id: &str) -> TableNode {
        TableNode {
            id: NodeId::from(id.to_string()),
            rows: Vec::new(),
            structure_hash: "empty".to_string(),
            formatting: Default::default(),
            formatting_change: None,
        }
    }

    fn revision() -> RevisionInfo {
        RevisionInfo {
            revision_id: 17,
            identity: 17,
            author: Some("Reviewer".to_string()),
            date: None,
            apply_op_id: None,
        }
    }

    fn cell(id: &str) -> TableCellNode {
        TableCellNode {
            id: NodeId::from(id.to_string()),
            blocks: Vec::new(),
            grid_span: 1,
            v_merge: stemma::domain::VerticalMerge::None,
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

    fn row(id: &str, cell_texts: &[&str]) -> crate::domain::TableRowNode {
        crate::domain::TableRowNode {
            id: NodeId::from(id.to_string()),
            cells: cell_texts
                .iter()
                .enumerate()
                .map(|(index, text)| {
                    let mut cell = cell(&format!("{id}-cell-{index}"));
                    let paragraph_id = format!("{id}-paragraph-{index}");
                    cell.blocks
                        .push(ParagraphNode::new_story_body(&paragraph_id, text, None).into());
                    cell
                })
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
            tbl_pr_ex_change: None,
            cell_spacing: None,
            preserved: Vec::new(),
        }
    }

    fn block_math_paragraph(id: &str) -> ParagraphNode {
        let mut paragraph = ParagraphNode::new_story_body(id, "", None);
        paragraph.segments = vec![TrackedSegment {
            status: TrackingStatus::Normal,
            inlines: vec![InlineNode::from(crate::domain::OpaqueInlineNode {
                id: NodeId::from(format!("{id}-math")),
                kind: OpaqueKind::OmmlBlock,
                opaque_ref: format!("{id}-math-ref"),
                proof_ref: crate::domain::ProofRef {
                    part: crate::domain::DocPart::DocumentXml,
                    block_id: NodeId::from(id.to_string()),
                    docx_anchor: "math".to_string(),
                },
                wrapper_marks: Vec::new(),
                wrapper_style_props: StyleProps::default(),
                source_run_attrs: Vec::new(),
                joins_following_text_run: false,
                raw_xml: Some(
                    br#"<m:oMathPara xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math"><m:oMath><m:r><m:t>x</m:t></m:r></m:oMath></m:oMathPara>"#
                        .to_vec(),
                ),
                content_hash: None,
            })],
        }];
        paragraph
    }

    fn inline_sdt_paragraph(id: &str) -> ParagraphNode {
        let mut paragraph = ParagraphNode::new_story_body(id, "", None);
        paragraph.segments = vec![TrackedSegment {
            status: TrackingStatus::Normal,
            inlines: vec![InlineNode::from(crate::domain::OpaqueInlineNode {
                id: NodeId::from(format!("{id}-sdt")),
                kind: OpaqueKind::Sdt,
                opaque_ref: format!("{id}-sdt-ref"),
                proof_ref: crate::domain::ProofRef {
                    part: crate::domain::DocPart::DocumentXml,
                    block_id: NodeId::from(id.to_string()),
                    docx_anchor: "inline-sdt".to_string(),
                },
                wrapper_marks: Vec::new(),
                wrapper_style_props: StyleProps::default(),
                source_run_attrs: Vec::new(),
                joins_following_text_run: false,
                raw_xml: Some(
                    br#"<w:sdt xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:sdtPr><w:tag w:val="control"/><w:id w:val="701"/></w:sdtPr><w:sdtContent><w:r><w:t>CONTROL</w:t></w:r></w:sdtContent></w:sdt>"#
                        .to_vec(),
                ),
                content_hash: None,
            })],
        }];
        paragraph
    }

    #[test]
    fn row_carrier_refuses_an_unrepresented_outer_table_property_change() {
        let source = table("source-table");
        let mut target = table("target-table");
        target.formatting.indent = Some(720);
        let diff = crate::compiler::compute_table_diff_result(&source, &target)
            .expect("empty table diff should be representable");
        let mut blocks = vec![TrackedBlock {
            status: TrackingStatus::Normal,
            block: BlockNode::Table(Box::new(source)),
            move_id: None,
            block_sdt_wrap: None,
        }];
        let mut revision_counter = 0;

        let error = apply_table_structure_changed(
            &mut blocks,
            &NodeId::from("source-table".to_string()),
            &target,
            &diff,
            &revision(),
            &mut revision_counter,
            "body",
            &ComparisonPlanResolver,
        )
        .expect_err("row carriers cannot silently retain source tblPr state");

        assert!(error.message.contains("outer table properties"));
        assert!(error.context.contains("table-property carrier"));
        assert_eq!(revision_counter, 0, "refusal must mint no revisions");
    }

    #[test]
    fn separate_cell_property_changes_receive_separate_revision_ids() {
        let mut first = cell("first-cell");
        let mut second = cell("second-cell");
        let target_formatting = CellFormatting {
            no_wrap: Some(true),
            ..CellFormatting::default()
        };
        let mut revision_counter = 0;

        apply_cell_formatting_change(
            &mut first,
            &target_formatting,
            &revision(),
            &mut revision_counter,
        );
        apply_cell_formatting_change(
            &mut second,
            &target_formatting,
            &revision(),
            &mut revision_counter,
        );

        let first_id = first
            .formatting_change
            .as_ref()
            .expect("first cell should carry tcPrChange")
            .revision_id;
        let second_id = second
            .formatting_change
            .as_ref()
            .expect("second cell should carry tcPrChange")
            .revision_id;
        assert_ne!(first_id, second_id);
        assert_eq!(revision_counter, 2);
    }

    #[test]
    fn separate_paragraph_property_changes_receive_separate_revision_ids() {
        let first_source = ParagraphNode::new_story_body("first", "First", None);
        let second_source = ParagraphNode::new_story_body("second", "Second", None);
        let mut first_target = first_source.clone();
        first_target.style_id = Some("TargetStyle".into());
        let mut second_target = second_source.clone();
        second_target.style_id = Some("TargetStyle".into());
        let mut blocks = vec![
            TrackedBlock {
                status: TrackingStatus::Normal,
                block: first_source.clone().into(),
                move_id: None,
                block_sdt_wrap: None,
            },
            TrackedBlock {
                status: TrackingStatus::Normal,
                block: second_source.clone().into(),
                move_id: None,
                block_sdt_wrap: None,
            },
        ];
        let mut revision_counter = 0;

        for (source, target) in [
            (&first_source, &first_target),
            (&second_source, &second_target),
        ] {
            let inline_changes = ComparisonPlanResolver
                .paragraph_changes(&source.all_inlines_owned(), &target.all_inlines_owned());
            apply_block_modified(
                &mut blocks,
                &source.id,
                &inline_changes,
                &target.clone().into(),
                &revision(),
                &mut revision_counter,
                "body",
            )
            .expect("paragraph property change should materialize");
        }

        let ids: Vec<u32> = blocks
            .iter()
            .map(|tracked| match &tracked.block {
                BlockNode::Paragraph(paragraph) => {
                    paragraph
                        .formatting_change
                        .as_ref()
                        .expect("paragraph should carry pPrChange")
                        .revision_id
                }
                _ => panic!("test fixture should contain only paragraphs"),
            })
            .collect();
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1]);
        assert_eq!(revision_counter, 2);
    }

    #[test]
    fn replaced_cell_final_paragraph_reuses_the_required_boundary() {
        let mut source = cell("cell");
        source
            .blocks
            .push(ParagraphNode::new_story_body("source-final", "Source text", None).into());
        let mut target = cell("cell");
        target
            .blocks
            .push(ParagraphNode::new_story_body("target-final", "Target text", None).into());
        let mut revision_counter = 0;

        reconcile_cell_blocks(
            &mut source,
            &target,
            &revision(),
            &mut revision_counter,
            "table",
            &ComparisonPlanResolver,
            CellContentRelation::UnrelatedReplacement,
        )
        .expect("cell-final paragraph replacement should be representable");

        assert_eq!(source.blocks.len(), 1, "the cell keeps one final boundary");
        let BlockNode::Paragraph(paragraph) = &source.blocks[0] else {
            panic!("the required final cell block must remain a paragraph")
        };
        assert_eq!(paragraph.para_mark_status, None);
        assert!(
            paragraph
                .segments
                .iter()
                .any(|segment| matches!(segment.status, TrackingStatus::Deleted(_)))
        );
        assert!(
            paragraph
                .segments
                .iter()
                .any(|segment| matches!(segment.status, TrackingStatus::Inserted(_)))
        );
    }

    #[test]
    fn unrelated_replacement_row_shares_only_physical_shells() {
        let mut source = table("source-table");
        source
            .rows
            .push(row("source-row", &["Shared token source", "Coincidental"]));
        let mut target = table("target-table");
        target
            .rows
            .push(row("target-row", &["Shared token target", "Coincidental"]));
        let diff = TableDiffResult {
            old_table: crate::table::canonicalize_table(&source).expect("canonical source"),
            new_table: crate::table::canonicalize_table(&target).expect("canonical target"),
            row_alignment: vec![TableRowAlignment::Replacement {
                old_row: 0,
                new_row: 0,
            }],
            cell_diffs: Vec::new(),
        };
        let mut blocks = vec![TrackedBlock {
            status: TrackingStatus::Normal,
            block: source.into(),
            move_id: None,
            block_sdt_wrap: None,
        }];
        let mut revision_counter = 0;

        apply_table_structure_changed(
            &mut blocks,
            &NodeId::from("source-table"),
            &target,
            &diff,
            &revision(),
            &mut revision_counter,
            "body",
            &ComparisonPlanResolver,
        )
        .expect("plain unrelated rows should use the bounded replacement carrier");

        let BlockNode::Table(materialized) = &blocks[0].block else {
            panic!("fixture must remain one table")
        };
        assert_eq!(materialized.rows.len(), 1);
        for cell in &materialized.rows[0].cells {
            let BlockNode::Paragraph(paragraph) = &cell.blocks[0] else {
                panic!("fixture cells contain one paragraph")
            };
            assert_eq!(paragraph.para_mark_status, None);
            assert!(
                paragraph
                    .segments
                    .iter()
                    .any(|segment| { matches!(segment.status, TrackingStatus::Deleted(_)) }),
                "replacement paragraph has no deleted source payload: {:?}",
                paragraph.segments
            );
            assert!(
                paragraph
                    .segments
                    .iter()
                    .any(|segment| { matches!(segment.status, TrackingStatus::Inserted(_)) }),
                "replacement paragraph has no inserted target payload: {:?}",
                paragraph.segments
            );
            assert!(paragraph.segments.iter().all(|segment| {
                !matches!(segment.status, TrackingStatus::Normal)
                    || segment.inlines.iter().all(
                        |inline| !matches!(inline, InlineNode::Text(text) if !text.text.is_empty()),
                    )
            }));
        }
    }

    #[test]
    fn move_and_inserted_table_composition_refuses_before_materialization() {
        let source = ParagraphNode::new_story_body("source", "source", None);
        let target = ParagraphNode::new_story_body("target", "source", None);
        let changes = vec![
            DiffChange::BlockDeleted {
                block_id: source.id.clone(),
                old_text: "source".to_string(),
                old_block: source.clone().into(),
                move_id: Some("move-1".to_string()),
            },
            DiffChange::BlockInserted {
                after_block_id: None,
                block: target.into(),
                move_id: Some("move-1".to_string()),
            },
            DiffChange::BlockInserted {
                after_block_id: None,
                block: table("inserted-table").into(),
                move_id: None,
            },
        ];
        let mut blocks = vec![TrackedBlock {
            status: TrackingStatus::Normal,
            block: source.into(),
            move_id: None,
            block_sdt_wrap: None,
        }];
        let original = blocks.clone();
        let mut revision_counter = 0;
        let mut provenance = BlockProvenanceMap::new();

        let error = apply_changes_to_blocks(
            &mut blocks,
            &changes,
            &revision(),
            &mut revision_counter,
            "body",
            None,
            &mut provenance,
            &ComparisonPlanResolver,
        )
        .expect_err("unqualified carrier composition must refuse");

        assert!(error.message.contains("move ranges"));
        assert!(error.message.contains("inserted table"));
        assert!(error.context.contains("Reject All"));
        assert_eq!(blocks, original, "refusal must precede model mutation");
        assert_eq!(revision_counter, 0, "refusal must mint no revisions");
    }

    fn complete_row_replacement() -> TableDiffResult {
        fn canonical_table(id: &str) -> crate::domain::CanonicalTable {
            crate::domain::CanonicalTable {
                id: NodeId::from(id.to_string()),
                n_rows: 1,
                n_cols: 0,
                cells: Vec::new(),
                owner_grid: vec![Vec::new()],
                formatting: Default::default(),
                row_tracking: vec![None],
                row_para_ids: vec![None],
            }
        }

        TableDiffResult {
            old_table: canonical_table("source-table"),
            new_table: canonical_table("target-table"),
            row_alignment: vec![
                TableRowAlignment::Deleted { old_row: 0 },
                TableRowAlignment::Inserted { new_row: 0 },
            ],
            cell_diffs: Vec::new(),
        }
    }

    fn final_table_replacement_changes() -> Vec<DiffChange> {
        vec![
            DiffChange::TableStructureChanged {
                table_id: NodeId::from("source-table".to_string()),
                target_table_id: NodeId::from("target-table".to_string()),
                old_hash: "source".to_string(),
                new_hash: "target".to_string(),
                old_text: "source".to_string(),
                new_text: "target".to_string(),
                table_diff: Some(Box::new(complete_row_replacement())),
            },
            DiffChange::BlockInserted {
                after_block_id: Some(NodeId::from("source-table".to_string())),
                block: ParagraphNode::new_story_body("target-tail", "", None).into(),
                move_id: None,
            },
        ]
    }

    #[test]
    fn final_table_row_replacement_and_empty_paragraph_refuses_before_materialization() {
        let mut blocks = vec![TrackedBlock {
            status: TrackingStatus::Normal,
            block: table("source-table").into(),
            move_id: None,
            block_sdt_wrap: None,
        }];
        let original = blocks.clone();
        let mut revision_counter = 0;
        let mut provenance = BlockProvenanceMap::new();

        let error = apply_changes_to_blocks(
            &mut blocks,
            &final_table_replacement_changes(),
            &revision(),
            &mut revision_counter,
            "body",
            None,
            &mut provenance,
            &ComparisonPlanResolver,
        )
        .expect_err("unqualified document-final carrier composition must refuse");

        assert!(error.message.contains("document-final table"));
        assert!(error.message.contains("final empty paragraph"));
        assert!(error.context.contains("final-table"));
        assert_eq!(blocks, original, "refusal must precede model mutation");
        assert_eq!(revision_counter, 0, "refusal must mint no revisions");
    }

    #[test]
    fn whole_final_table_deletion_and_empty_paragraph_refuses_before_materialization() {
        let source_table: BlockNode = table("source-table").into();
        let mut blocks = vec![TrackedBlock {
            status: TrackingStatus::Normal,
            block: source_table.clone(),
            move_id: None,
            block_sdt_wrap: None,
        }];
        let changes = vec![
            DiffChange::BlockInserted {
                after_block_id: None,
                block: table("target-table").into(),
                move_id: None,
            },
            DiffChange::BlockInserted {
                after_block_id: Some(NodeId::from("target-table".to_string())),
                block: ParagraphNode::new_story_body("target-tail", "", None).into(),
                move_id: None,
            },
            DiffChange::BlockDeleted {
                block_id: NodeId::from("source-table".to_string()),
                old_text: "source".to_string(),
                old_block: source_table,
                move_id: None,
            },
        ];
        let original = blocks.clone();
        let mut revision_counter = 0;
        let mut provenance = BlockProvenanceMap::new();

        let error = apply_changes_to_blocks(
            &mut blocks,
            &changes,
            &revision(),
            &mut revision_counter,
            "body",
            None,
            &mut provenance,
            &ComparisonPlanResolver,
        )
        .expect_err("whole final-table removal plus a final empty paragraph must refuse");

        assert!(error.message.contains("document-final table"));
        assert!(error.message.contains("final empty paragraph"));
        assert_eq!(blocks, original, "refusal must precede model mutation");
        assert_eq!(revision_counter, 0, "refusal must mint no revisions");
    }

    #[test]
    fn standalone_complete_row_replacement_remains_supported() {
        let changes = vec![DiffChange::TableStructureChanged {
            table_id: NodeId::from("source-table".to_string()),
            target_table_id: NodeId::from("target-table".to_string()),
            old_hash: "source".to_string(),
            new_hash: "target".to_string(),
            old_text: "source".to_string(),
            new_text: "target".to_string(),
            table_diff: Some(Box::new(complete_row_replacement())),
        }];

        assert!(!has_unqualified_row_replacement_composition(&changes));
    }

    #[test]
    fn partial_row_change_can_compose_with_an_independent_story_change() {
        let mut partial = complete_row_replacement();
        partial.row_alignment = vec![
            TableRowAlignment::Matched {
                old_row: 0,
                new_row: 0,
            },
            TableRowAlignment::Inserted { new_row: 1 },
        ];
        let changes = vec![
            DiffChange::TableStructureChanged {
                table_id: NodeId::from("source-table".to_string()),
                target_table_id: NodeId::from("target-table".to_string()),
                old_hash: "source".to_string(),
                new_hash: "target".to_string(),
                old_text: "source".to_string(),
                new_text: "target".to_string(),
                table_diff: Some(Box::new(partial)),
            },
            DiffChange::BlockInserted {
                after_block_id: Some(NodeId::from("source-table".to_string())),
                block: ParagraphNode::new_story_body("target-tail", "Target tail", None).into(),
                move_id: None,
            },
        ];

        assert!(!has_unqualified_row_replacement_composition(&changes));
    }

    fn unrelated_row_replacement_change() -> DiffChange {
        let mut diff = complete_row_replacement();
        diff.row_alignment = vec![TableRowAlignment::Replacement {
            old_row: 0,
            new_row: 0,
        }];
        DiffChange::TableStructureChanged {
            table_id: NodeId::from("source-table"),
            target_table_id: NodeId::from("target-table"),
            old_hash: "source".to_string(),
            new_hash: "target".to_string(),
            old_text: "source".to_string(),
            new_text: "target".to_string(),
            table_diff: Some(Box::new(diff)),
        }
    }

    #[test]
    fn unrelated_row_replacement_accepts_preceding_story_changes() {
        let changes = vec![
            DiffChange::BlockInserted {
                after_block_id: None,
                block: ParagraphNode::new_story_body("target-one", "One", None).into(),
                move_id: None,
            },
            DiffChange::BlockInserted {
                after_block_id: Some(NodeId::from("target-one")),
                block: ParagraphNode::new_story_body("target-two", "Two", None).into(),
                move_id: None,
            },
            DiffChange::BlockInserted {
                after_block_id: Some(NodeId::from("target-two")),
                block: ParagraphNode::new_story_body("target-three", "Three", None).into(),
                move_id: None,
            },
            unrelated_row_replacement_change(),
        ];

        assert!(!has_unqualified_row_replacement_composition(&changes));
    }

    #[test]
    fn unrelated_row_replacement_refuses_a_following_story_change() {
        let changes = vec![
            unrelated_row_replacement_change(),
            DiffChange::BlockInserted {
                after_block_id: Some(NodeId::from("source-table")),
                block: ParagraphNode::new_story_body("target-one", "One", None).into(),
                move_id: None,
            },
        ];

        assert!(has_unqualified_row_replacement_composition(&changes));
    }

    #[test]
    fn excessive_property_history_population_refuses_before_materialization() {
        let source = ParagraphNode::new_story_body("source", "Same text", None);
        let mut target = source.clone();
        target.style_id = Some("TargetStyle".into());
        let change = DiffChange::BlockModified {
            block_id: source.id.clone(),
            old_text: "Same text".to_string(),
            new_text: "Same text".to_string(),
            inline_changes: vec![InlineChange::Unchanged {
                text: "Same text".to_string(),
                marks: Vec::new(),
                style_props: StyleProps::default(),
                formatting_change: Some(crate::domain::FormattingChange {
                    carrier: crate::domain::RunFormattingChangeCarrier::RunProperties,
                    previous_marks: Vec::new(),
                    previous_style_props: StyleProps::default(),
                    previous_rpr_authored: RunRprAuthored::default(),
                    revision_id: 0,
                    author: String::new(),
                    date: None,
                    identity: 0,
                }),
            }],
            old_block: source.clone().into(),
            new_block: target.into(),
            para_split: false,
        };
        let changes = vec![change; MAX_PROPERTY_HISTORY_CARRIERS_PER_STORY / 2 + 1];
        assert_eq!(
            property_history_carrier_count(&changes),
            MAX_PROPERTY_HISTORY_CARRIERS_PER_STORY + 2
        );

        let mut blocks = vec![TrackedBlock {
            status: TrackingStatus::Normal,
            block: source.into(),
            move_id: None,
            block_sdt_wrap: None,
        }];
        let original = blocks.clone();
        let mut revision_counter = 0;
        let mut provenance = BlockProvenanceMap::new();
        let error = apply_changes_to_blocks(
            &mut blocks,
            &changes,
            &revision(),
            &mut revision_counter,
            "body",
            None,
            &mut provenance,
            &ComparisonPlanResolver,
        )
        .expect_err("unqualified property-history population must refuse");

        assert_eq!(error.kind, MergeErrorKind::UnsupportedEdit);
        assert!(error.message.contains("property-history carriers"));
        assert!(
            error
                .message
                .contains(&MAX_PROPERTY_HISTORY_CARRIERS_PER_STORY.to_string())
        );
        assert!(error.context.contains("Review UI"));
        assert_eq!(blocks, original, "refusal must precede model mutation");
        assert_eq!(revision_counter, 0, "refusal must mint no revisions");
    }

    #[test]
    fn complete_row_replacement_and_independent_story_change_refuse_before_materialization() {
        let trailing = ParagraphNode::new_story_body("source-tail", "Source tail", None);
        let changes = vec![
            DiffChange::TableStructureChanged {
                table_id: NodeId::from("source-table".to_string()),
                target_table_id: NodeId::from("target-table".to_string()),
                old_hash: "source".to_string(),
                new_hash: "target".to_string(),
                old_text: "source".to_string(),
                new_text: "target".to_string(),
                table_diff: Some(Box::new(complete_row_replacement())),
            },
            DiffChange::BlockInserted {
                after_block_id: Some(trailing.id.clone()),
                block: ParagraphNode::new_story_body("target-tail", "Target tail", None).into(),
                move_id: None,
            },
        ];
        let mut blocks = vec![
            TrackedBlock {
                status: TrackingStatus::Normal,
                block: table("source-table").into(),
                move_id: None,
                block_sdt_wrap: None,
            },
            TrackedBlock {
                status: TrackingStatus::Normal,
                block: trailing.into(),
                move_id: None,
                block_sdt_wrap: None,
            },
        ];
        let original = blocks.clone();
        let mut revision_counter = 0;
        let mut provenance = BlockProvenanceMap::new();

        let error = apply_changes_to_blocks(
            &mut blocks,
            &changes,
            &revision(),
            &mut revision_counter,
            "body",
            None,
            &mut provenance,
            &ComparisonPlanResolver,
        )
        .expect_err("unqualified carrier composition must refuse");

        assert!(error.message.contains("table-row replacement"));
        assert!(error.message.contains("followed by another change"));
        assert!(error.context.contains("Reject All"));
        assert_eq!(blocks, original, "refusal must precede model mutation");
        assert_eq!(revision_counter, 0, "refusal must mint no revisions");
    }

    #[test]
    fn side_only_block_math_refuses_before_materialization() {
        let source = ParagraphNode::new_story_body("source", "source", None);
        let changes = vec![DiffChange::BlockInserted {
            after_block_id: Some(source.id.clone()),
            block: block_math_paragraph("target-math").into(),
            move_id: None,
        }];
        let mut blocks = vec![TrackedBlock {
            status: TrackingStatus::Normal,
            block: source.into(),
            move_id: None,
            block_sdt_wrap: None,
        }];
        let original = blocks.clone();
        let mut revision_counter = 0;
        let mut provenance = BlockProvenanceMap::new();

        let error = apply_changes_to_blocks(
            &mut blocks,
            &changes,
            &revision(),
            &mut revision_counter,
            "body",
            None,
            &mut provenance,
            &ComparisonPlanResolver,
        )
        .expect_err("an unqualified whole block-math carrier must refuse");

        assert!(error.message.contains("block equations"));
        assert!(error.context.contains("oMathPara"));
        assert_eq!(blocks, original, "refusal must precede model mutation");
        assert_eq!(revision_counter, 0, "refusal must mint no revisions");
    }

    #[test]
    fn side_only_inline_sdt_insertion_refuses_before_materialization() {
        let source = ParagraphNode::new_story_body("source", "source", None);
        let changes = vec![DiffChange::BlockInserted {
            after_block_id: Some(source.id.clone()),
            block: inline_sdt_paragraph("target-sdt").into(),
            move_id: None,
        }];
        let mut blocks = vec![TrackedBlock {
            status: TrackingStatus::Normal,
            block: source.into(),
            move_id: None,
            block_sdt_wrap: None,
        }];
        let original = blocks.clone();
        let mut revision_counter = 0;
        let mut provenance = BlockProvenanceMap::new();

        let error = apply_changes_to_blocks(
            &mut blocks,
            &changes,
            &revision(),
            &mut revision_counter,
            "body",
            None,
            &mut provenance,
            &ComparisonPlanResolver,
        )
        .expect_err("an unqualified inline content-control insertion must refuse");

        assert!(error.message.contains("content-control insertion"));
        assert!(error.context.contains("w:sdt"));
        assert_eq!(blocks, original, "refusal must precede model mutation");
        assert_eq!(revision_counter, 0, "refusal must mint no revisions");
    }

    #[test]
    fn title_page_transition_materializes_both_relative_readings() {
        let absent = SectionProperties::default();
        let enabled = SectionProperties {
            title_page: Some(true),
            ..SectionProperties::default()
        };

        let (previous, current) = word_relative_section_states(Some(&absent), Some(&enabled))
            .expect("two present sections have a relative carrier");
        assert_eq!(previous.title_page, Some(false));
        assert_eq!(current.title_page, Some(true));

        let (previous, current) = word_relative_section_states(Some(&enabled), Some(&absent))
            .expect("two present sections have a relative carrier");
        assert_eq!(previous.title_page, Some(true));
        assert_eq!(current.title_page, Some(false));
    }

    #[test]
    fn unchanged_title_page_preserves_authored_absence() {
        let absent = SectionProperties::default();
        let (previous, current) = word_relative_section_states(Some(&absent), Some(&absent))
            .expect("two present sections have a relative carrier");
        assert_eq!(previous.title_page, None);
        assert_eq!(current.title_page, None);
    }

    #[test]
    fn body_section_presence_transition_refuses() {
        let section = SectionProperties::default();

        let removed = word_relative_section_states(Some(&section), None)
            .expect_err("removing body sectPr has no qualified carrier");
        let added = word_relative_section_states(None, Some(&section))
            .expect_err("adding body sectPr has no qualified carrier");

        assert!(removed.message.contains("presence transitions"));
        assert!(added.message.contains("presence transitions"));
    }
}
