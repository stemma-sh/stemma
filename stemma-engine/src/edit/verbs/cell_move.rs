//! `move` of paragraphs into, out of, and between table cells
//! (§17.13.5.21–.28), as Word records them.
//!
//! Word (16) records a cut and paste with Track Changes on as a move only when
//! the cut lies outside a table cell: a paragraph moved from the body into a
//! cell is a `w:moveFrom` in the body and a `w:moveTo` in the cell, paired by
//! the range name, and resolves as one unit. A cut taken from inside a cell
//! (to the same cell, another cell, or the body) Word records as a tracked
//! deletion at the source and a tracked insertion at the destination, two
//! revisions; so does this verb, and the receipt says so. Word never cuts a
//! cell's end mark: a moved last paragraph leaves an empty paragraph in the
//! cell. Refused: moving a cell's whole content (that is the row's job:
//! `move_row`), landing after a cell's last paragraph (Word pastes at the cell
//! end inside that paragraph), and a table into a cell.
//!
//! A cell paragraph has no tracked-block wrapper, so a moveTo half there
//! carries the move's status on its runs and mark and the move group on
//! [`ParagraphNode::cell_move_id`](crate::domain::ParagraphNode::cell_move_id).

use std::collections::HashSet;

use super::super::{
    ApplyBlockContext, EditError, InsertPosition, MovedSourceInfo, ParagraphPath, block_at,
    block_id_of, check_ancestor_table_tracking, check_destination_anchor_not_moved,
    collect_block_node_ids, expect_matches_paragraph, find_block_index, find_paragraph_path,
    group_authored_annotations, next_move_id, normalize_block_range_indices, note_after_insert,
    paragraph_visible_text, reassign_cloned_block_ids, resolve_after_anchor,
    validate_block_is_editable, validate_move_expect,
};
use super::cell_paragraph_delete::{cell, cell_mut};
use crate::domain::{BlockNode, CanonDoc, NodeId, TrackedBlock, TrackingStatus};
use crate::tracked_model::{mark_paragraph_deleted, mark_paragraph_inserted, next_revision};

/// Where the moved paragraphs are taken from.
enum Source {
    Body {
        start: usize,
        end: usize,
    },
    Cell {
        path: ParagraphPath,
        start: usize,
        end: usize,
    },
}

/// Where the moved paragraphs land: before index `index` of the body, or of
/// the cell `path` leads into.
enum Destination {
    Body { index: usize },
    Cell { path: ParagraphPath, index: usize },
}

/// Whether `id` names a block inside a table cell.
pub(in crate::edit) fn in_cell(doc: &CanonDoc, id: &NodeId) -> bool {
    find_block_index(&doc.blocks, id).is_none()
        && find_paragraph_path(doc, id).is_some_and(|path| !path.is_top_level())
}

#[allow(clippy::too_many_arguments)]
pub(in crate::edit) fn apply(
    doc: &mut CanonDoc,
    from: &NodeId,
    to: &NodeId,
    dest_anchor: &NodeId,
    dest_position: InsertPosition,
    expect: Option<&str>,
    semantic_hash: Option<&str>,
    ctx: &mut ApplyBlockContext<'_>,
) -> Result<(), EditError> {
    let step_index = ctx.step_index;
    let refuse = |block_id: &NodeId, reason: &str| EditError::UnsupportedParagraphStructure {
        block_id: block_id.clone(),
        reason: reason.to_string(),
        step_index,
    };
    let source = source(doc, from, to, expect, semantic_hash, step_index)?;
    check_destination_anchor_not_moved(doc, dest_anchor, ctx)?;
    let destination = destination(doc, dest_anchor, dest_position, ctx)?;

    if let (
        Source::Cell {
            path: source,
            start,
            end,
        },
        Destination::Cell {
            path: target,
            index,
        },
    ) = (&source, &destination)
        && std::ptr::eq(cell(doc, source), cell(doc, target))
        && (*start..=*end + 1).contains(index)
    {
        return Err(EditError::MoveDestinationInsideSource {
            from_block_id: from.clone(),
            to_block_id: to.clone(),
            dest_anchor_id: dest_anchor.clone(),
            step_index,
        });
    }
    if let (Source::Body { start, end }, Destination::Cell { .. }) = (&source, &destination) {
        for tracked in &doc.blocks[*start..=*end] {
            if !matches!(tracked.block, BlockNode::Paragraph(_)) {
                return Err(refuse(
                    block_id_of(&tracked.block),
                    "only paragraphs move into a table cell; this range holds a table or \
                     another block",
                ));
            }
        }
    }

    // The moved paragraphs as they read now, before the source is marked.
    let mut clones: Vec<BlockNode> = match &source {
        Source::Body { start, end } => doc.blocks[*start..=*end]
            .iter()
            .map(|tracked| tracked.block.clone())
            .collect(),
        Source::Cell { path, start, end } => cell(doc, path).blocks[*start..=*end].to_vec(),
    };
    let source_ids: Vec<NodeId> = clones.iter().map(|b| block_id_of(b).clone()).collect();

    // Fresh paragraph identities (MS-DOCX §2.6.2.3) and node ids for the
    // destination copies; the source shadows keep theirs so Reject restores
    // the source.
    let mut paragraph_ids = HashSet::new();
    for tracked in &doc.blocks {
        super::table_row_resources::collect_paragraph_ids(&tracked.block, &mut paragraph_ids)
            .map_err(|reason| EditError::UnsupportedBlockMoveIdentity {
                block_id: block_id_of(&tracked.block).clone(),
                reason,
                step_index,
            })?;
    }
    for block in &mut clones {
        super::table_row_resources::remint_paragraph_ids(block, &mut paragraph_ids).map_err(
            |reason| EditError::UnsupportedBlockMoveIdentity {
                block_id: block_id_of(block).clone(),
                reason,
                step_index,
            },
        )?;
    }
    let mut used_ids = HashSet::new();
    for tracked in &doc.blocks {
        collect_block_node_ids(&tracked.block, &mut used_ids);
    }
    let clone_ids: Vec<NodeId> = clones
        .iter_mut()
        .map(|block| reassign_cloned_block_ids(block, &mut used_ids))
        .collect();

    let move_id = next_move_id(step_index, *ctx.rev_counter);

    // Mark the source moved away (a body source) or deleted (a cell source).
    // Neither changes any index.
    let deletion_annotations = *ctx.rev_counter;
    match &source {
        Source::Body { start, end } => {
            for tracked in &mut doc.blocks[*start..=*end] {
                tracked.status =
                    TrackingStatus::Deleted(next_revision(ctx.revision, ctx.rev_counter));
                tracked.move_id = Some(move_id.clone());
                if let BlockNode::Paragraph(p) = &mut tracked.block {
                    p.para_mark_status = Some(TrackingStatus::Deleted(next_revision(
                        ctx.revision,
                        ctx.rev_counter,
                    )));
                }
            }
        }
        Source::Cell { path, start, end } => {
            let blocks = &mut cell_mut(doc, path).blocks;
            let last_in_cell = *end + 1 == blocks.len();
            for (index, block) in blocks.iter_mut().enumerate().take(*end + 1).skip(*start) {
                let BlockNode::Paragraph(p) = block else {
                    unreachable!("a cell source holds only paragraphs (checked)");
                };
                if last_in_cell && index == *end {
                    // The cell's end mark stays: its text goes, an empty
                    // paragraph remains (Word, cutting the cell's last
                    // paragraph's text).
                    delete_paragraph_text(p, ctx);
                } else {
                    mark_paragraph_deleted(p, ctx.revision, ctx.rev_counter);
                }
            }
        }
    }
    let insertion_annotations = *ctx.rev_counter;
    let cell_source = matches!(source, Source::Cell { .. });

    // Insert the copies, moved to.
    match &destination {
        Destination::Body { index } => {
            for (offset, block) in clones.into_iter().enumerate() {
                doc.blocks.insert(
                    index + offset,
                    TrackedBlock {
                        status: TrackingStatus::Inserted(next_revision(
                            ctx.revision,
                            ctx.rev_counter,
                        )),
                        block,
                        move_id: (!cell_source).then(|| move_id.clone()),
                        block_sdt_wrap: None,
                    },
                );
            }
        }
        Destination::Cell { path, index } => {
            let mut copies = Vec::with_capacity(clones.len());
            for mut block in clones {
                let BlockNode::Paragraph(p) = &mut block else {
                    unreachable!("only paragraphs move into a cell (checked)");
                };
                mark_paragraph_inserted(p, ctx.revision, ctx.rev_counter);
                if !cell_source {
                    p.cell_move_id = Some(move_id.clone());
                }
                copies.push(block);
            }
            let blocks = &mut cell_mut(doc, path).blocks;
            blocks.splice(*index..*index, copies);
        }
    }
    if cell_source {
        // Two revisions, as Word records the cut and paste: the deletion and
        // the insertion, each one proposal over all its carriers.
        group_authored_annotations(
            doc,
            deletion_annotations..insertion_annotations,
            ctx.revision,
        )?;
        group_authored_annotations(doc, insertion_annotations..*ctx.rev_counter, ctx.revision)?;
    }

    for (source_id, copy_id) in source_ids.iter().zip(&clone_ids) {
        ctx.order_state.moved_this_transaction.insert(
            source_id.clone(),
            MovedSourceInfo {
                step_index,
                copy_block_id: copy_id.clone(),
            },
        );
    }
    if dest_position == InsertPosition::After
        && let Some(last) = clone_ids.last()
    {
        note_after_insert(dest_anchor, last.clone(), ctx.order_state);
    }
    Ok(())
}

/// Delete a paragraph's text but not its mark (the cell's end mark).
fn delete_paragraph_text(p: &mut crate::domain::ParagraphNode, ctx: &mut ApplyBlockContext<'_>) {
    let inlines: Vec<_> = p
        .segments
        .drain(..)
        .flat_map(|segment| segment.inlines)
        .collect();
    if !inlines.is_empty() {
        p.segments = vec![crate::domain::TrackedSegment {
            status: TrackingStatus::Deleted(next_revision(ctx.revision, ctx.rev_counter)),
            inlines,
        }];
    }
}

fn source(
    doc: &CanonDoc,
    from: &NodeId,
    to: &NodeId,
    expect: Option<&str>,
    semantic_hash: Option<&str>,
    step_index: usize,
) -> Result<Source, EditError> {
    let refuse = |block_id: &NodeId, reason: &str| EditError::UnsupportedParagraphStructure {
        block_id: block_id.clone(),
        reason: reason.to_string(),
        step_index,
    };
    let (from_cell, to_cell) = (in_cell(doc, from), in_cell(doc, to));
    if !from_cell && !to_cell {
        validate_move_expect(doc, from, expect, semantic_hash, step_index)?;
        let (start, end) = normalize_block_range_indices(doc, from, to, step_index)?;
        for tracked in &doc.blocks[start..=end] {
            validate_block_is_editable(tracked, step_index)?;
        }
        return Ok(Source::Body { start, end });
    }
    let not_found = |id: &NodeId| EditError::BlockNotFound {
        block_id: id.clone(),
        step_index,
    };
    let from_path = find_paragraph_path(doc, from).ok_or_else(|| not_found(from))?;
    let to_path = find_paragraph_path(doc, to).ok_or_else(|| not_found(to))?;
    if from_cell != to_cell || !std::ptr::eq(cell(doc, &from_path), cell(doc, &to_path)) {
        return Err(refuse(
            from,
            "a moved range taken from a table cell must start and end in that one cell",
        ));
    }
    check_ancestor_table_tracking(doc, &from_path, from, step_index)?;
    let first = from_path
        .descent
        .last()
        .expect("cell path")
        .block_in_cell_idx;
    let last = to_path.descent.last().expect("cell path").block_in_cell_idx;
    let (start, end) = (first.min(last), first.max(last));
    let container = cell(doc, &from_path);
    if !container.content_sdt_wraps.is_empty() {
        return Err(refuse(
            from,
            "a move out of a table cell that holds content controls is not supported",
        ));
    }
    if start == 0 && end + 1 == container.blocks.len() {
        return Err(refuse(
            from,
            "the range is the cell's whole content, which is not a paragraph move (Word \
             selects the whole cell); to move content with its cell, move the row with \
             move_row",
        ));
    }
    for block in &container.blocks[start..=end] {
        let BlockNode::Paragraph(p) = block else {
            return Err(refuse(
                block_id_of(block),
                "only paragraphs move out of a table cell; this range holds a table or \
                 another block",
            ));
        };
        if p.segments
            .iter()
            .any(|segment| segment.status != TrackingStatus::Normal)
            || p.para_mark_status
                .as_ref()
                .is_some_and(|status| *status != TrackingStatus::Normal)
        {
            return Err(EditError::ParagraphContainsTrackedSegments {
                block_id: p.id.clone(),
                step_index,
            });
        }
    }
    let block = block_at(doc, &from_path);
    let BlockNode::Paragraph(p) = block else {
        unreachable!("checked: the range holds only paragraphs")
    };
    if let Some(expected) = semantic_hash {
        crate::semantic_hash::check_block_guard(block, expected).map_err(|actual| {
            EditError::BlockSemanticHashMismatch {
                block_id: from.clone(),
                expected: expected.to_string(),
                actual,
                step_index,
            }
        })?;
    }
    if let Some(expect) = expect
        && !expect_matches_paragraph(p, expect)
    {
        return Err(EditError::ExpectMismatch {
            block_id: from.clone(),
            expected: expect.to_string(),
            actual_text: paragraph_visible_text(p),
            step_index,
        });
    }
    Ok(Source::Cell {
        path: from_path,
        start,
        end,
    })
}

fn destination(
    doc: &CanonDoc,
    anchor: &NodeId,
    position: InsertPosition,
    ctx: &ApplyBlockContext<'_>,
) -> Result<Destination, EditError> {
    let step_index = ctx.step_index;
    let refuse = |reason: &str| EditError::UnsupportedParagraphStructure {
        block_id: anchor.clone(),
        reason: reason.to_string(),
        step_index,
    };
    let effective = match position {
        InsertPosition::After => resolve_after_anchor(anchor, ctx.order_state),
        InsertPosition::Before => anchor.clone(),
    };
    if let Some(index) = find_block_index(&doc.blocks, &effective) {
        return Ok(Destination::Body {
            index: match position {
                InsertPosition::Before => index,
                InsertPosition::After => index + 1,
            },
        });
    }
    let path = find_paragraph_path(doc, &effective).ok_or_else(|| EditError::BlockNotFound {
        block_id: effective.clone(),
        step_index,
    })?;
    check_ancestor_table_tracking(doc, &path, &effective, step_index)?;
    let container = cell(doc, &path);
    let at = path.descent.last().expect("cell path").block_in_cell_idx;
    if let BlockNode::Paragraph(p) = &container.blocks[at]
        && p.cell_move_id.is_some()
        && matches!(
            p.para_mark_status,
            Some(TrackingStatus::Deleted(_) | TrackingStatus::InsertedThenDeleted(_))
        )
    {
        return Err(EditError::AmbiguousAnchorAfterMove {
            anchor_id: anchor.clone(),
            moved_by_step_index: None,
            moved_to_block_id: None,
            step_index,
        });
    }
    if !container.content_sdt_wraps.is_empty() {
        return Err(refuse(
            "a move into a table cell that holds content controls is not supported",
        ));
    }
    let index = match position {
        InsertPosition::Before => at,
        InsertPosition::After => at + 1,
    };
    if index == container.blocks.len() {
        return Err(refuse(
            "the last paragraph of a table cell holds the cell's end mark: a move cannot \
             land after it (Word pastes at the cell end inside that paragraph); move the \
             paragraphs before it instead",
        ));
    }
    Ok(Destination::Cell { path, index })
}
