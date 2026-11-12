use std::collections::{HashMap, HashSet};
use stemma::edit::unique_inserted_block_id;
pub(crate) use stemma::tracked_model::next_revision;
use stemma::tracked_model::{
    collect_split_tracking_boundary_prefix_ids, materialize_numbering_prefix_in_place,
    materialize_split_tracking_boundary_prefixes, sync_literal_prefix_geometry,
};

use crate::domain::{
    BlockNode, BlockProvenance, CanonDoc, CommentStory, DiffChange, EndnoteStory, FooterStory,
    FootnoteStory, HeaderStory, InlineChange, InlineChangeSegmentType, InlineNode, Mark,
    MaterializedPrefixKind, NestedTableDiffKind, NodeId, OpaqueKind, ParagraphNode, RevisionInfo,
    SectionProperties, SectionPropertyChange, StyleProps, TableNode, TableRowAlignment, TextNode,
    TextRole, TrackedBlock, TrackedSegment, TrackingStatus, materialized_prefix_node_id,
};
use crate::table::extract_inlines_text;
use crate::tracked_model::{
    debug_assert_body_invariants, materialize_block_tracked_prefixes, reject_paragraph_formatting,
};
// Shared with the engine: one implementation of the change pipeline.
use stemma::tracked_model::inject_structural_marker_changes;
use stemma::tracked_model::{
    apply_cell_formatting_change, apply_paragraph_diff_in_cell,
    apply_target_non_numbering_paragraph_properties, block_id, collect_opaques, find_block_index,
    make_prefix_text_node, materialized_prefix_text, plan_prefix_materialization,
    prune_empty_text_inlines,
};
// Shared with the engine: one implementation of the change pipeline.
pub(crate) use stemma::tracked_model::{
    ChangePlanResolver, MergeError, MergeErrorKind, apply_table_structure_changed,
};
use stemma::tracked_model::{
    apply_nested_table_cells_modified, apply_nested_table_structure_changed,
    inline_changes_to_segments_with_opaques, refuse_unstable_cnf_style_change,
};
pub(crate) use stemma::tracked_model::{
    mark_whole_block_deleted, normalize_final_mark_attribution,
};

// Bound the physical population while keeping the limit above the qualified
// large-history witness. The previous 256 limit was inferred from an artifact
// that reused property-change wire IDs; Word's pathological behavior was not a
// valid population witness. A uniquely identified 600-carrier Stemma shape is
// qualified separately before this boundary is promoted.
const MAX_PROPERTY_HISTORY_CARRIERS_PER_STORY: usize = 1024;

pub(crate) struct ComparisonPlanResolver;

impl ChangePlanResolver for ComparisonPlanResolver {
    fn paragraph_changes(&self, before: &[InlineNode], after: &[InlineNode]) -> Vec<InlineChange> {
        crate::compiler::cell_paragraph_changes(before, after)
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

/// Make the inserted paragraph `merged_id`, the second part of a paragraph
/// split, keep its mark: the base paragraph's mark, which the split leaves
/// in place. Its content stays inserted, under the revision it was inserted
/// with.
fn keep_split_second_part_mark(
    blocks: &mut [TrackedBlock],
    merged_id: &NodeId,
    context: &str,
) -> Result<(), MergeError> {
    let invalid = |message: String| MergeError {
        kind: MergeErrorKind::InvalidPlan,
        message,
        context: context.to_string(),
    };
    let tracked = blocks
        .iter_mut()
        .find(|tb| block_id(&tb.block) == merged_id)
        .ok_or_else(|| {
            invalid(format!(
                "the second part of a paragraph split, {}, is not in the merge",
                merged_id.0
            ))
        })?;
    let TrackingStatus::Inserted(inserted) = tracked.status.clone() else {
        return Err(invalid(format!(
            "the second part of a paragraph split, {}, is not an inserted block",
            merged_id.0
        )));
    };
    if !matches!(tracked.block, BlockNode::Paragraph(_)) {
        return Err(invalid(format!(
            "the second part of a paragraph split, {}, is not a paragraph",
            merged_id.0
        )));
    }
    // A typed label is a view over untracked text: it becomes part of the
    // inserted content, as on a block-tracked paragraph.
    if matches!(&tracked.block, BlockNode::Paragraph(p) if p.literal_prefix.is_some()) {
        stemma::tracked_model::materialize_numbering_prefix_in_place(&mut tracked.block);
    }
    let BlockNode::Paragraph(paragraph) = &mut tracked.block else {
        unreachable!("checked above: the second part is a paragraph");
    };
    for segment in &mut paragraph.segments {
        if segment.status != TrackingStatus::Normal {
            return Err(invalid(format!(
                "the second part of a paragraph split, {}, holds a {:?} segment",
                merged_id.0, segment.status
            )));
        }
        segment.status = TrackingStatus::Inserted(inserted.clone());
    }
    paragraph.para_mark_status = None;
    tracked.status = TrackingStatus::Normal;
    Ok(())
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

fn set_block_id(block: &mut BlockNode, id: NodeId) {
    match block {
        BlockNode::Paragraph(p) => p.id = id,
        BlockNode::Table(t) => t.id = id,
        BlockNode::OpaqueBlock(o) => o.id = id,
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
                paragraph.direct_numbering_omits_ilvl = false;
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
            paragraph.direct_numbering_omits_ilvl = new_para.direct_numbering_omits_ilvl;
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

    inject_structural_marker_changes(
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
    // The first part of a paragraph split (`para_split`) was just applied:
    // the next change is its second part, which keeps the paragraph's mark.
    let mut split_second_part_pending = false;
    for change in changes {
        if split_second_part_pending
            && !matches!(
                change,
                DiffChange::BlockInserted {
                    block: BlockNode::Paragraph(_),
                    move_id: None,
                    ..
                }
            )
        {
            return Err(MergeError {
                kind: MergeErrorKind::InvalidPlan,
                message:
                    "a paragraph split's first part is not followed by its inserted second part"
                        .to_string(),
                context: context.to_string(),
            });
        }
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
                if split_second_part_pending {
                    split_second_part_pending = false;
                    keep_split_second_part_mark(blocks, &merged_id, context)?;
                }
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
                // The first part of a split: its mark is the inserted one.
                // Rejecting it joins the paragraph to its second part, whose
                // content is rejected while its mark (the base paragraph's)
                // stays, so the paragraph reads whole again.
                if *para_split {
                    let Some(idx) = find_block_index(blocks, block_id) else {
                        return Err(MergeError {
                            kind: MergeErrorKind::InvalidPlan,
                            message: format!(
                                "the first part of a paragraph split, {}, is not in the merge",
                                block_id.0
                            ),
                            context: context.to_string(),
                        });
                    };
                    let BlockNode::Paragraph(p) = &mut blocks[idx].block else {
                        return Err(MergeError {
                            kind: MergeErrorKind::InvalidPlan,
                            message: format!(
                                "the first part of a paragraph split, {}, is not a paragraph",
                                block_id.0
                            ),
                            context: context.to_string(),
                        });
                    };
                    p.para_mark_status = Some(TrackingStatus::Inserted(next_revision(
                        revision,
                        rev_counter,
                    )));
                    split_second_part_pending = true;
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
    crate::controls::rebuild_merged_controls(
        &mut merged.blocks,
        &base.blocks,
        &target.blocks,
        &diff.aligned_blocks,
        &provenance.0,
    )
    .map_err(|reason| MergeError {
        kind: MergeErrorKind::UnsupportedEdit,
        message: format!(
            "content controls changed around shared content, which no tracked change can \
             express: {reason}"
        ),
        context: "body".to_string(),
    })?;
    stemma::tracked_model::normalize_control_final_marks(
        &mut merged.blocks,
        revision,
        &mut rev_counter,
    );

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

    // A reference copied into deleted and inserted content names one note
    // twice: each inserted copy gets a note of its own (`noterefs`).
    crate::noterefs::split_shared_note_references(
        &mut merged,
        base,
        target,
        revision,
        &mut rev_counter,
    )?;
    // A comment added on kept text is an annotation, carried untracked
    // (`comments`).
    crate::comments::carry_added_comments_on_kept_text(&mut merged, target, &diff.changes)
        .map_err(|message| MergeError {
            kind: MergeErrorKind::InvalidPlan,
            message,
            context: "comments".to_string(),
        })?;

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

    // A cell paragraph this merge made a whole insertion or deletion (its
    // mark and all its text tracked; a cell paragraph has no block status)
    // carries its typed label inside that proposal, like a block-tracked
    // paragraph below. It is not a retained label beside a tracked body, so
    // the split-boundary rule that follows must not keep its label normal.
    let produced_cell_proposals =
        stemma::tracked_model::materialize_produced_cell_proposal_prefixes(
            &mut merged,
            &stemma::tracked_model::hoisted_label_cell_proposal_ids(base),
        );
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
        .filter(|id| !produced_cell_proposals.contains(*id))
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
    // the change so the serializer can emit a sectPrChange element. Editing-
    // session provenance alone is not a section change.
    if base
        .body_section_properties
        .as_ref()
        .map(crate::domain::SectionProperties::without_provenance)
        != target
            .body_section_properties
            .as_ref()
            .map(crate::domain::SectionProperties::without_provenance)
    {
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
                range_markers: Vec::new(),
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
    use crate::domain::RunRprAuthored;

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
    use crate::domain::CellFormatting;
    use crate::domain::RunRprAuthored;
    use crate::domain::{TableCellNode, TableDiffResult};
    use stemma::tracked_model::{CellContentRelation, reconcile_cell_blocks};

    fn table(id: &str) -> TableNode {
        TableNode {
            range_markers: Vec::new(),
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
            cell_merge: None,
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
            source_row_attrs: Vec::new(),
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
            cant_split: None,
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
formatting_change: None,
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
                wrapper_rpr_authored: crate::domain::RunRprAuthored::ALL,
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
formatting_change: None,
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
                wrapper_rpr_authored: crate::domain::RunRprAuthored::ALL,
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
                rpr_authored: RunRprAuthored::default(),
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
