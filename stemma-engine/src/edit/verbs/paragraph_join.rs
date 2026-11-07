//! Join a paragraph with the paragraph right after it by deleting the first's
//! paragraph mark (§17.13.5.15), as Delete at the end of a paragraph does in
//! Word, in the body or within one table cell. As in Word, the joined paragraph
//! takes the first paragraph's paragraph properties and the surviving mark keeps
//! its own run properties; tracked, the second paragraph's old properties are
//! recorded as a formatting change, so Reject gives back both as they were. A
//! direct join performs the same merge at once.

use super::super::{
    EditError, MaterializationMode, advisory_expect_miss, block_at, check_ancestor_table_tracking,
    check_block_guard, expect_matches_paragraph, find_paragraph_path, paragraph_visible_text,
    validate_block_is_editable,
};
use super::cell_paragraph_delete::{cell, cell_mut};
use crate::domain::{
    BlockNode, CanonDoc, NodeId, ParagraphFormattingChange, ParagraphNode, RevisionInfo,
    TrackingStatus,
};

#[allow(clippy::too_many_arguments)]
pub(in crate::edit) fn apply(
    doc: &mut CanonDoc,
    block_id: &NodeId,
    with_block_id: &NodeId,
    expect: &str,
    semantic_hash: Option<&str>,
    mode: MaterializationMode,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
    step_index: usize,
) -> Result<(), EditError> {
    let path = find_paragraph_path(doc, block_id).ok_or_else(|| EditError::BlockNotFound {
        block_id: block_id.clone(),
        step_index,
    })?;
    let block = block_at(doc, &path);
    let BlockNode::Paragraph(first) = block else {
        return Err(EditError::NotAParagraph {
            block_id: block_id.clone(),
            actual_kind: match block {
                BlockNode::Table(_) => "table",
                _ => "opaque_block",
            },
            step_index,
        });
    };
    check_expect(block, first, block_id, expect, semantic_hash, step_index)?;

    // The block right after the first, in the same container.
    let (index, next) = if path.is_top_level() {
        validate_block_is_editable(&doc.blocks[path.top_block], step_index)?;
        let next = doc.blocks.get(path.top_block + 1);
        if let Some(next) = next {
            validate_block_is_editable(next, step_index)?;
        }
        (path.top_block, next.map(|tracked| &tracked.block))
    } else {
        check_ancestor_table_tracking(doc, &path, block_id, step_index)?;
        let container = cell(doc, &path);
        if !container.content_sdt_wraps.is_empty() {
            return Err(EditError::UnsupportedParagraphStructure {
                block_id: block_id.clone(),
                reason: "joining paragraphs inside a cell with content controls is not supported"
                    .into(),
                step_index,
            });
        }
        let index = path.descent.last().expect("cell path").block_in_cell_idx;
        (index, container.blocks.get(index + 1))
    };
    let next_paragraph = next.and_then(|block| match block {
        BlockNode::Paragraph(p) => Some(p),
        _ => None,
    });
    if next_paragraph.map(|p| &p.id) != Some(with_block_id) {
        return Err(EditError::JoinNeedsFollowingParagraph {
            block_id: block_id.clone(),
            with_block_id: with_block_id.clone(),
            next: next_paragraph.map(|p| p.id.clone()),
            step_index,
        });
    }
    let first = match block_at(doc, &path) {
        BlockNode::Paragraph(p) => p,
        _ => unreachable!("resolved above"),
    };
    for p in [first, next_paragraph.expect("checked above")] {
        if p.segments
            .iter()
            .any(|s| s.status != TrackingStatus::Normal)
            || p.para_mark_status
                .as_ref()
                .is_some_and(|s| !matches!(s, TrackingStatus::Normal))
        {
            return Err(EditError::ParagraphContainsTrackedSegments {
                block_id: p.id.clone(),
                step_index,
            });
        }
    }
    let next_paragraph = next_paragraph.expect("checked above");
    if next_paragraph.formatting_change.is_some() {
        return Err(EditError::UnsupportedParagraphStructure {
            block_id: with_block_id.clone(),
            reason: "the paragraph to join into has a tracked formatting change; resolve it first"
                .into(),
            step_index,
        });
    }
    let first_paragraph = first.clone();

    match (mode, path.is_top_level()) {
        (MaterializationMode::Direct, true) => {
            adopt_first_paragraph_properties(
                &first_paragraph,
                following_mut(&mut doc.blocks[index + 1].block),
                revision,
            );
            crate::tracked_model::join_paragraph_into_following(&mut doc.blocks, index);
        }
        (MaterializationMode::Direct, false) => {
            let blocks = &mut cell_mut(doc, &path).blocks;
            adopt_first_paragraph_properties(
                &first_paragraph,
                following_mut(&mut blocks[index + 1]),
                revision,
            );
            crate::tracked_model::join_cell_paragraph_into_following(blocks, index);
        }
        (MaterializationMode::TrackedChange, top_level) => {
            let BlockNode::Paragraph(first) = super::super::block_at_mut(doc, &path) else {
                unreachable!("resolved above")
            };
            first.para_mark_status = Some(TrackingStatus::Deleted(
                crate::tracked_model::next_revision(revision, rev_counter),
            ));
            let following = if top_level {
                following_mut(&mut doc.blocks[index + 1].block)
            } else {
                following_mut(&mut cell_mut(doc, &path).blocks[index + 1])
            };
            let format_revision = crate::tracked_model::next_revision(revision, rev_counter);
            following.formatting_change =
                adopt_first_paragraph_properties(&first_paragraph, following, &format_revision);
        }
    }
    Ok(())
}

fn following_mut(block: &mut BlockNode) -> &mut ParagraphNode {
    match block {
        BlockNode::Paragraph(p) => p,
        _ => unreachable!("the following block was checked to be a paragraph"),
    }
}

/// Give `following` the paragraph properties of `first`, keeping its own
/// paragraph-mark run properties, as Word's join does (observed with the Word
/// oracle). Returns `following`'s own previous properties as the formatting
/// change a tracked join records, or None when the two already agree.
fn adopt_first_paragraph_properties(
    first: &ParagraphNode,
    following: &mut ParagraphNode,
    revision: &RevisionInfo,
) -> Option<ParagraphFormattingChange> {
    let own = super::super::snapshot_paragraph_formatting(following, revision);
    let mut adopted = super::super::snapshot_paragraph_formatting(first, revision);
    adopted.previous.paragraph_mark = own.previous.paragraph_mark.clone();
    if adopted.previous == own.previous {
        return None;
    }
    // Rejecting a formatting change restores its snapshot exactly: the one
    // exhaustive restore path, so every paragraph property moves.
    following.formatting_change = Some(adopted);
    crate::tracked_model::reject_paragraph_formatting(following);
    Some(own)
}

/// `expect` on the first paragraph, authoritative unless a guard is given
/// (then advisory), as `delete`'s.
fn check_expect(
    block: &BlockNode,
    first: &ParagraphNode,
    block_id: &NodeId,
    expect: &str,
    semantic_hash: Option<&str>,
    step_index: usize,
) -> Result<(), EditError> {
    if let Some(expected) = semantic_hash {
        check_block_guard(block, expected).map_err(|actual| {
            EditError::BlockSemanticHashMismatch {
                block_id: block_id.clone(),
                expected: expected.into(),
                actual,
                step_index,
            }
        })?;
        if !expect_matches_paragraph(first, expect) {
            advisory_expect_miss(block_id, expect, step_index);
        }
        return Ok(());
    }
    if !expect_matches_paragraph(first, expect) {
        return Err(EditError::ExpectMismatch {
            block_id: block_id.clone(),
            expected: expect.into(),
            actual_text: paragraph_visible_text(first),
            step_index,
        });
    }
    Ok(())
}
