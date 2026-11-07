//! Interior cell paragraph deletion uses the existing native deletion law.
//! A cell's final paragraph mark cannot be joined across a cell boundary.

use super::super::{
    EditError, ParagraphPath, advisory_expect_miss, block_at, block_at_mut,
    check_ancestor_table_tracking, check_block_guard, expect_matches_paragraph,
    find_paragraph_path, paragraph_visible_text,
};
use crate::domain::{BlockNode, CanonDoc, NodeId, RevisionInfo, TableCellNode, TrackingStatus};

pub(in crate::edit) fn cell<'a>(doc: &'a CanonDoc, path: &ParagraphPath) -> &'a TableCellNode {
    let (last, parents) = path.descent.split_last().expect("cell paragraph path");
    let parent = ParagraphPath {
        top_block: path.top_block,
        descent: parents.to_vec(),
    };
    let BlockNode::Table(table) = block_at(doc, &parent) else {
        unreachable!("cell parent")
    };
    &table.rows[last.row_idx].cells[last.cell_idx]
}

pub(in crate::edit) fn cell_mut<'a>(
    doc: &'a mut CanonDoc,
    path: &ParagraphPath,
) -> &'a mut TableCellNode {
    let (last, parents) = path.descent.split_last().expect("cell paragraph path");
    let parent = ParagraphPath {
        top_block: path.top_block,
        descent: parents.to_vec(),
    };
    let BlockNode::Table(table) = block_at_mut(doc, &parent) else {
        unreachable!("cell parent")
    };
    &mut table.rows[last.row_idx].cells[last.cell_idx]
}

#[allow(clippy::too_many_arguments)]
pub(in crate::edit) fn apply(
    doc: &mut CanonDoc,
    from: &NodeId,
    to: &NodeId,
    path: &ParagraphPath,
    expect: &str,
    semantic_hash: Option<&str>,
    tracking: Option<(&RevisionInfo, &mut u32)>,
    step_index: usize,
) -> Result<(), EditError> {
    let refuse = |reason: &str| EditError::UnsupportedParagraphStructure {
        block_id: from.clone(),
        reason: reason.into(),
        step_index,
    };
    check_ancestor_table_tracking(doc, path, from, step_index)?;
    let end_path = find_paragraph_path(doc, to).ok_or_else(|| EditError::BlockNotFound {
        block_id: to.clone(),
        step_index,
    })?;
    if end_path.descent.is_empty() || !std::ptr::eq(cell(doc, path), cell(doc, &end_path)) {
        return Err(refuse(
            "paragraph deletion endpoints must belong to the same table cell",
        ));
    }
    let first = path.descent.last().expect("cell path").block_in_cell_idx;
    let last = end_path
        .descent
        .last()
        .expect("cell path")
        .block_in_cell_idx;
    let (start, end) = (first.min(last), first.max(last));
    let container = cell(doc, path);
    if !container.content_sdt_wraps.is_empty() {
        return Err(refuse(
            "cell paragraph deletion across content-control ranges is not supported",
        ));
    }
    if !matches!(container.blocks.get(end + 1), Some(BlockNode::Paragraph(_))) {
        return Err(refuse(
            "a table cell always keeps one paragraph (ECMA-376 §17.4.66), so its last \
             paragraph cannot be deleted; to remove what it says, delete its text and the \
             empty paragraph stays; deleting the row also deletes the row's other cells",
        ));
    }
    for block in &container.blocks[start..=end] {
        let BlockNode::Paragraph(p) = block else {
            return Err(refuse(
                "cell paragraph deletion cannot span tables or opaque blocks",
            ));
        };
        if p.segments
            .iter()
            .any(|s| s.status != TrackingStatus::Normal)
            || p.para_mark_status.as_ref().is_some_and(|s| {
                !matches!(s, TrackingStatus::Normal)
                    && !(tracking.is_some() && matches!(s, TrackingStatus::Inserted(_)))
            })
        {
            return Err(EditError::ParagraphContainsTrackedSegments {
                block_id: p.id.clone(),
                step_index,
            });
        }
    }
    let block = block_at(doc, path);
    let BlockNode::Paragraph(p) = block else {
        unreachable!("paragraph path")
    };
    if let Some(expected) = semantic_hash {
        check_block_guard(block, expected).map_err(|actual| {
            EditError::BlockSemanticHashMismatch {
                block_id: from.clone(),
                expected: expected.into(),
                actual,
                step_index,
            }
        })?;
        if !expect_matches_paragraph(p, expect) {
            advisory_expect_miss(from, expect, step_index);
        }
    } else if !expect_matches_paragraph(p, expect) {
        return Err(EditError::ExpectMismatch {
            block_id: from.clone(),
            expected: expect.into(),
            actual_text: paragraph_visible_text(p),
            step_index,
        });
    }
    let container = cell_mut(doc, path);
    if let Some((revision, counter)) = tracking {
        let first_annotation = *counter;
        for block in &mut container.blocks[start..=end] {
            let BlockNode::Paragraph(p) = block else {
                unreachable!("validated paragraph range")
            };
            crate::tracked_model::mark_paragraph_deleted(p, revision, counter);
        }
        super::super::group_authored_annotations(doc, first_annotation..*counter, revision)?;
    } else {
        container.blocks.drain(start..=end);
    }
    Ok(())
}
