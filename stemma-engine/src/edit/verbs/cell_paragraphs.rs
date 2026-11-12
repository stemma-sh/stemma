//! Paragraph insertion inside a cell: preserve the containing table, inherit
//! the local host, and reuse the native paragraph insertion carrier.

use super::super::{
    BlockSpec, EditError, InsertOrderState, InsertPosition, ParagraphPath,
    assign_inserted_block_ids, block_at, block_at_mut, check_ancestor_table_tracking,
    find_paragraph_path, fresh_inserted_para_id, note_after_insert, resolve_after_anchor,
    resolve_block_spec,
};
use crate::domain::{
    BlockNode, CanonDoc, NodeId, ParagraphNode, RevisionInfo, TableCellNode, TrackingStatus,
};

fn cell<'a>(doc: &'a CanonDoc, path: &ParagraphPath) -> &'a TableCellNode {
    let (last, parents) = path.descent.split_last().expect("cell path has a descent");
    let parent = ParagraphPath {
        top_block: path.top_block,
        descent: parents.to_vec(),
    };
    let BlockNode::Table(table) = block_at(doc, &parent) else {
        unreachable!("cell parent is a table")
    };
    &table.rows[last.row_idx].cells[last.cell_idx]
}

fn cell_mut<'a>(doc: &'a mut CanonDoc, path: &ParagraphPath) -> &'a mut TableCellNode {
    let (last, parents) = path.descent.split_last().expect("cell path has a descent");
    let parent = ParagraphPath {
        top_block: path.top_block,
        descent: parents.to_vec(),
    };
    let BlockNode::Table(table) = block_at_mut(doc, &parent) else {
        unreachable!("cell parent is a table")
    };
    &mut table.rows[last.row_idx].cells[last.cell_idx]
}

fn host<'a>(
    doc: &'a CanonDoc,
    path: &ParagraphPath,
    position: InsertPosition,
) -> Option<&'a ParagraphNode> {
    let index = path.descent.last().expect("cell path").block_in_cell_idx;
    let blocks = &cell(doc, path).blocks;
    let paragraph_at = |index| match blocks.get(index)? {
        BlockNode::Paragraph(p) => Some(p.as_ref()),
        _ => None,
    };
    match position {
        InsertPosition::Before => paragraph_at(index),
        InsertPosition::After => paragraph_at(index + 1).or_else(|| paragraph_at(index)),
    }
}

#[allow(clippy::too_many_arguments)]
pub(in crate::edit) fn apply(
    doc: &mut CanonDoc,
    anchor: &NodeId,
    path: &ParagraphPath,
    position: InsertPosition,
    specs: &[BlockSpec],
    order: &mut InsertOrderState,
    mut tracking: Option<(&RevisionInfo, &mut u32)>,
    step_index: usize,
) -> Result<(), EditError> {
    check_ancestor_table_tracking(doc, path, anchor, step_index)?;
    let refuse = |reason: &str| EditError::UnsupportedParagraphStructure {
        block_id: anchor.clone(),
        reason: reason.into(),
        step_index,
    };
    if !cell(doc, path).content_sdt_wraps.is_empty() {
        return Err(refuse(
            "cell paragraph insertion across content-control ranges is not supported",
        ));
    }
    let mut resolved = Vec::with_capacity(specs.len());
    for spec in specs {
        if !matches!(spec, BlockSpec::Paragraph(_)) {
            return Err(refuse(
                "cell-anchored insertion currently requires paragraph blocks",
            ));
        }
        let block = resolve_block_spec(doc, spec, step_index, host(doc, path, position))?;
        let BlockNode::Paragraph(paragraph) = &block else {
            unreachable!("paragraph specification")
        };
        if paragraph.literal_prefix.is_some() {
            return Err(refuse(
                "cell insertion cannot renumber literal text labels; use native list numbering",
            ));
        }
        resolved.push(block);
    }
    let effective = match position {
        InsertPosition::Before => anchor.clone(),
        InsertPosition::After => resolve_after_anchor(anchor, order),
    };
    let effective_path =
        find_paragraph_path(doc, &effective).ok_or_else(|| EditError::BlockNotFound {
            block_id: effective.clone(),
            step_index,
        })?;
    // The insertion-order map is internal state: it must point to a sibling,
    // never to a block in a different cell after a preceding operation.
    let same_cell = std::ptr::eq(cell(doc, path), cell(doc, &effective_path));
    assert!(
        same_cell,
        "cell insertion order anchor {effective} escaped the original cell {anchor}"
    );
    let index = effective_path
        .descent
        .last()
        .expect("cell path")
        .block_in_cell_idx
        + usize::from(position == InsertPosition::After);
    let mut last_id = None;
    let first_annotation = tracking.as_ref().map(|(_, counter)| **counter);
    let is_tail_append = index == cell(doc, path).blocks.len() && !resolved.is_empty();
    if is_tail_append && tracking.is_some() {
        let BlockNode::Paragraph(anchor) = &cell(doc, path).blocks[index - 1] else {
            unreachable!("validated paragraph anchor")
        };
        if anchor
            .para_mark_status
            .as_ref()
            .is_some_and(|status| *status != TrackingStatus::Normal)
        {
            return Err(refuse(
                "cell-tail append requires an untracked final paragraph mark",
            ));
        }
    }
    for (index, mut block) in (index..).zip(resolved) {
        let para_id = fresh_inserted_para_id(doc, &block);
        let id = assign_inserted_block_ids(&doc.blocks, &mut block);
        let BlockNode::Paragraph(paragraph) = &mut block else {
            unreachable!("paragraph specification")
        };
        paragraph.para_id = Some(para_id);
        if let Some((revision, counter)) = &mut tracking {
            crate::tracked_model::mark_paragraph_inserted(paragraph, revision, counter);
        }
        cell_mut(doc, path).blocks.insert(index, block);
        last_id = Some(id);
    }
    // Word moves a bookmark's or permission range's start onto a paragraph
    // inserted before the first paragraph it covers, and a permission end
    // inside the anchor to the end of a paragraph inserted after it (checked
    // in cells and in the body).
    if let Some(count) = last_id.as_ref().map(|_| specs.len()) {
        let blocks = &mut cell_mut(doc, path).blocks;
        match position {
            InsertPosition::Before => {
                let (before, after) = blocks.split_at_mut(index + count);
                if let (BlockNode::Paragraph(first_new), Some(BlockNode::Paragraph(anchor))) =
                    (&mut before[index], after.first_mut())
                {
                    super::super::hand_leading_bookmark_starts(anchor, first_new);
                }
            }
            InsertPosition::After if index > 0 => {
                let (before, after) = blocks.split_at_mut(index);
                if let (Some(BlockNode::Paragraph(anchor)), BlockNode::Paragraph(last_new)) =
                    (before.last_mut(), &mut after[count - 1])
                {
                    super::super::hand_trailing_permission_ends(anchor, last_new);
                }
            }
            InsertPosition::After => {}
        }
    }
    if is_tail_append && let Some((revision, counter)) = &mut tracking {
        crate::tracked_model::normalize_cell_append_mark(
            cell_mut(doc, path),
            index,
            revision,
            counter,
        );
    }
    if let Some((revision, counter)) = tracking
        && let Some(first) = first_annotation
        && last_id.is_some()
    {
        super::super::group_authored_annotations(doc, first..*counter, revision)?;
    }
    if position == InsertPosition::After
        && let Some(id) = last_id
    {
        note_after_insert(anchor, id, order);
    }
    Ok(())
}
