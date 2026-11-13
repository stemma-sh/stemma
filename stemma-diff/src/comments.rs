//! Comments are annotations, as in Word.
//!
//! A comment (`w:comment`, ISO 29500-1 §17.13.4.2) is anchored in a story
//! by a range (`w:commentRangeStart` / `w:commentRangeEnd`) and a reference
//! (`w:commentReference`). Word's own compare carries comments untracked,
//! and Word does not honour a tracked insertion of a comment anchored on
//! text both documents keep: it ignores `w:ins` around the comment markers
//! and around a reference-only run, so its Reject All empties such a
//! comment but leaves it anchored.
//!
//! A named departure from the laws, therefore: a comment the target adds on
//! text both documents keep (its range covers kept text, or its reference
//! follows kept text) is carried untracked: plain range markers, reference
//! and comment. Accept All and Reject All both keep it. A comment the target
//! adds on inserted text rides inside the inserted content, so rejecting
//! that content removes it (Word does the same), and a removed comment
//! stays a tracked deletion of its anchors.

use stemma::domain::{BlockNode, CanonDoc, InlineNode, OpaqueKind, TrackedBlock, TrackingStatus};

use crate::model::DiffChange;

/// One inline of a story in document order, as the annotation rule reads
/// it: kept or inserted text, or a comment marker.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Item {
    /// Text (or another object with width), and whether it is kept (in a
    /// segment neither inserted nor deleted).
    Text {
        kept: bool,
        inserted: bool,
    },
    Start(String),
    End(String),
    Reference(String),
}

/// A block, row or cell inserted or deleted whole carries its status on
/// itself, not on its segments (they stay `Normal`): text inside it is
/// inserted or deleted with it.
fn story_items(blocks: &[TrackedBlock], out: &mut Vec<Item>) {
    fn within<'a>(
        outer: &'a TrackingStatus,
        own: Option<&'a TrackingStatus>,
    ) -> &'a TrackingStatus {
        match (outer, own) {
            (TrackingStatus::Normal, Some(own)) => own,
            _ => outer,
        }
    }
    fn block_items(block: &BlockNode, status: &TrackingStatus, out: &mut Vec<Item>) {
        match block {
            BlockNode::Paragraph(paragraph) => {
                for segment in &paragraph.segments {
                    let status = within(status, Some(&segment.status));
                    let kept = *status == TrackingStatus::Normal;
                    let inserted = matches!(status, TrackingStatus::Inserted(_));
                    for inline in &segment.inlines {
                        out.push(match inline {
                            InlineNode::CommentRangeStart { id } => Item::Start(id.clone()),
                            InlineNode::CommentRangeEnd { id } => Item::End(id.clone()),
                            InlineNode::OpaqueInline(o) => match &o.kind {
                                OpaqueKind::CommentReference(r) => {
                                    Item::Reference(r.reference_id.clone())
                                }
                                _ => Item::Text { kept, inserted },
                            },
                            InlineNode::Text(t) if t.text.is_empty() => continue,
                            InlineNode::Text(_) | InlineNode::HardBreak(_) => {
                                Item::Text { kept, inserted }
                            }
                            InlineNode::Decoration(_) => continue,
                        });
                    }
                }
            }
            BlockNode::Table(table) => {
                for row in &table.rows {
                    let status = within(status, row.tracking_status.as_ref());
                    for cell in &row.cells {
                        let status = within(status, cell.tracking_status.as_ref());
                        for inner in &cell.blocks {
                            block_items(inner, status, out);
                        }
                    }
                }
            }
            BlockNode::OpaqueBlock(_) => {}
        }
    }
    for tracked in blocks {
        block_items(&tracked.block, &tracked.status, out);
    }
}

/// Whether the comment `id`, which the target adds, is anchored on text
/// both documents keep: its range covers kept text, or (a range covering no
/// text, or none) the text before its reference is kept. `None` when the
/// story holds no reference of the comment.
fn anchored_on_kept_text(items: &[Item], id: &str) -> Option<bool> {
    let reference = items
        .iter()
        .position(|item| *item == Item::Reference(id.to_string()))?;
    let start = items
        .iter()
        .position(|item| *item == Item::Start(id.to_string()));
    let end = items
        .iter()
        .position(|item| *item == Item::End(id.to_string()));
    if let (Some(start), Some(end)) = (start, end)
        && start < end
    {
        let covered: Vec<&Item> = items[start + 1..end]
            .iter()
            .filter(|item| matches!(item, Item::Text { .. }))
            .collect();
        if !covered.is_empty() {
            return Some(
                covered
                    .iter()
                    .any(|item| matches!(item, Item::Text { kept: true, .. })),
            );
        }
    }
    Some(
        items[..reference]
            .iter()
            .rev()
            .find_map(|item| match item {
                Item::Text { kept, inserted } => Some(*kept || !*inserted),
                _ => None,
            })
            .unwrap_or(true),
    )
}

/// Move comment `id`'s markers (range start, range end, reference) out of
/// tracked segments into untracked ones, in place.
fn untrack_comment_markers(blocks: &mut [TrackedBlock], id: &str) -> Result<(), String> {
    fn is_marker(inline: &InlineNode, id: &str) -> bool {
        match inline {
            InlineNode::CommentRangeStart { id: i } | InlineNode::CommentRangeEnd { id: i } => {
                i == id
            }
            InlineNode::OpaqueInline(o) => {
                matches!(&o.kind, OpaqueKind::CommentReference(r) if r.reference_id == id)
            }
            _ => false,
        }
    }
    fn untrack(block: &mut BlockNode, id: &str) -> Result<(), String> {
        match block {
            BlockNode::Paragraph(paragraph) => {
                let mut out = Vec::with_capacity(paragraph.segments.len());
                for segment in std::mem::take(&mut paragraph.segments) {
                    if segment.status == TrackingStatus::Normal
                        || !segment.inlines.iter().any(|i| is_marker(i, id))
                    {
                        out.push(segment);
                        continue;
                    }
                    if !matches!(segment.status, TrackingStatus::Inserted(_)) {
                        return Err(format!(
                            "comment {id}: an added comment's marker sits in a {:?} segment of paragraph {}",
                            segment.status, paragraph.id.0
                        ));
                    }
                    let mut run: Vec<InlineNode> = Vec::new();
                    for inline in segment.inlines {
                        if is_marker(&inline, id) {
                            if !run.is_empty() {
                                out.push(stemma::domain::TrackedSegment {
                                    status: segment.status.clone(),
                                    inlines: std::mem::take(&mut run),
                                });
                            }
                            out.push(stemma::domain::TrackedSegment {
                                status: TrackingStatus::Normal,
                                inlines: vec![inline],
                            });
                        } else {
                            run.push(inline);
                        }
                    }
                    if !run.is_empty() {
                        out.push(stemma::domain::TrackedSegment {
                            status: segment.status,
                            inlines: run,
                        });
                    }
                }
                paragraph.segments = out;
                Ok(())
            }
            BlockNode::Table(table) => {
                for row in &mut table.rows {
                    for cell in &mut row.cells {
                        for inner in &mut cell.blocks {
                            untrack(inner, id)?;
                        }
                    }
                }
                Ok(())
            }
            BlockNode::OpaqueBlock(_) => Ok(()),
        }
    }
    for tracked in blocks {
        untrack(&mut tracked.block, id)?;
    }
    Ok(())
}

/// Carry every comment the plan adds on text both documents keep untracked
/// (see the module documentation), after the plan is materialized.
pub(crate) fn carry_added_comments_on_kept_text(
    merged: &mut CanonDoc,
    target: &CanonDoc,
    changes: &[DiffChange],
) -> Result<(), String> {
    let inserted: Vec<&str> = changes
        .iter()
        .filter_map(|change| match change {
            DiffChange::CommentInserted { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    for id in inserted {
        let mut on_kept = None;
        let stories: Vec<&mut Vec<TrackedBlock>> = std::iter::once(&mut merged.blocks)
            .chain(merged.headers.iter_mut().map(|s| &mut s.blocks))
            .chain(merged.footers.iter_mut().map(|s| &mut s.blocks))
            .chain(merged.footnotes.iter_mut().map(|s| &mut s.blocks))
            .chain(merged.endnotes.iter_mut().map(|s| &mut s.blocks))
            .collect();
        for blocks in stories {
            let mut items = Vec::new();
            story_items(blocks, &mut items);
            if let Some(kept) = anchored_on_kept_text(&items, id) {
                on_kept = Some(kept);
                if kept {
                    untrack_comment_markers(blocks, id)?;
                }
                break;
            }
        }
        // No reference in a paragraph (a body-level marker only): the
        // comment keeps the tracked carrier.
        if on_kept != Some(true) {
            continue;
        }
        let story = merged
            .comments
            .iter_mut()
            .find(|story| story.id == id)
            .ok_or_else(|| format!("comment {id}: the plan inserts it but the redline lacks it"))?;
        let target_story = target
            .comments
            .iter()
            .find(|story| story.id == id)
            .ok_or_else(|| format!("comment {id}: the plan inserts it but the target lacks it"))?;
        story.blocks = target_story.blocks.clone();
        story.tracking_status = None;
        copy_extended_record(merged, target, id)?;
    }
    Ok(())
}

/// Give an added, untracked comment its target `commentsExtended` record,
/// with its parent (a reply's thread) mapped to the redline's comment.
fn copy_extended_record(merged: &mut CanonDoc, target: &CanonDoc, id: &str) -> Result<(), String> {
    let Some(para_id) = target
        .comments
        .iter()
        .find(|story| story.id == id)
        .and_then(|story| story.last_para_id())
    else {
        return Ok(());
    };
    let Some(record) = target
        .comments_extended
        .iter()
        .find(|r| r.para_id == para_id)
    else {
        return Ok(());
    };
    if merged
        .comments_extended
        .iter()
        .any(|r| r.para_id == para_id)
    {
        return Err(format!(
            "comment {id}: its paragraph id {para_id} already keys another comment's record"
        ));
    }
    let parent = match &record.para_id_parent {
        None => None,
        Some(parent) => {
            let parent_id = target
                .comments
                .iter()
                .find(|story| story.last_para_id() == Some(parent.as_str()))
                .map(|story| story.id.clone())
                .ok_or_else(|| {
                    format!("comment {id}: its parent {parent} is not a target comment")
                })?;
            let mapped = merged
                .comments
                .iter()
                .find(|story| story.id == parent_id)
                .and_then(|story| story.last_para_id())
                .ok_or_else(|| {
                    format!("comment {id}: its parent comment {parent_id} has no paragraph id in the redline")
                })?;
            Some(mapped.to_string())
        }
    };
    merged
        .comments_extended
        .push(stemma::domain::CommentExtended {
            para_id: para_id.to_string(),
            para_id_parent: parent,
            done: record.done,
        });
    Ok(())
}
