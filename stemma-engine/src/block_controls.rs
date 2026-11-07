//! Block content controls as per-block membership.
//!
//! A story stores its block-level content controls (`w:sdt`, §17.5.2) as
//! `BlockSdtWrap`s opened on their first block, with a span. That spelling
//! cannot survive a change to the block list: a wrapper whose first block is
//! removed disappears with it, and a span counts blocks that may no longer be
//! there. Anything that removes or rearranges blocks therefore reads each
//! block's membership first (which controls enclose it, outermost first),
//! changes the blocks, and rebuilds the wrappers from the membership. A
//! control keeps exactly the blocks of its own that remain; one with none
//! left is gone, which is what resolving all of its content means.

use std::collections::HashMap;

use crate::domain::{BlockSdtWrap, CanonDoc, NestedBlockSdtWrap, NodeId, SdtWrapper, TrackedBlock};

/// The controls of a block list, and which of them encloses each block.
#[derive(Clone, Debug, Default)]
pub struct ControlMembership {
    /// Every control, by instance index.
    pub controls: Vec<SdtWrapper>,
    /// Each block's enclosing control instances, outermost first, by block id.
    /// A block outside every control has an empty path.
    pub paths: HashMap<NodeId, Vec<usize>>,
    /// The block ids in story order.
    pub order: Vec<NodeId>,
}

/// The id of a block in a block list.
pub fn tracked_block_id(tracked: &TrackedBlock) -> &NodeId {
    match &tracked.block {
        crate::domain::BlockNode::Paragraph(paragraph) => &paragraph.id,
        crate::domain::BlockNode::Table(table) => &table.id,
        crate::domain::BlockNode::OpaqueBlock(opaque) => &opaque.id,
    }
}

/// Read the controls of `blocks` as per-block membership.
pub fn membership(blocks: &[TrackedBlock]) -> ControlMembership {
    fn enter(
        wrap: &BlockSdtWrap,
        start: usize,
        paths: &mut [Vec<usize>],
        controls: &mut Vec<SdtWrapper>,
    ) {
        let instance = controls.len();
        controls.push(wrap.wrapper.clone());
        for path in &mut paths[start..start + wrap.span] {
            path.push(instance);
        }
        for nested in &wrap.nested {
            enter(&nested.control, start + nested.start, paths, controls);
        }
    }
    let mut controls = Vec::new();
    let mut paths = vec![Vec::new(); blocks.len()];
    for (start, tracked) in blocks.iter().enumerate() {
        if let Some(wrap) = &tracked.block_sdt_wrap {
            enter(wrap, start, &mut paths, &mut controls);
        }
    }
    let order: Vec<NodeId> = blocks.iter().map(tracked_block_id).cloned().collect();
    ControlMembership {
        paths: order.iter().cloned().zip(paths).collect(),
        order,
        controls,
    }
}

/// The blocks that end a block content control, each with the first block of
/// the innermost control it ends. Word treats the last paragraph mark inside
/// a control like a story's final mark: no revision on it can be resolved
/// (checked against Word 16).
pub fn control_ends(blocks: &[TrackedBlock]) -> HashMap<usize, usize> {
    fn ends(wrap: &BlockSdtWrap, start: usize, out: &mut HashMap<usize, usize>) {
        let end = start + wrap.span - 1;
        let innermost = out.get(&end).map_or(start, |&other| other.max(start));
        out.insert(end, innermost);
        for nested in &wrap.nested {
            ends(&nested.control, start + nested.start, out);
        }
    }
    let mut out = HashMap::new();
    for (start, tracked) in blocks.iter().enumerate() {
        if let Some(wrap) = &tracked.block_sdt_wrap {
            ends(wrap, start, &mut out);
        }
    }
    out
}

/// Rebuild the wrappers of `blocks` from per-block control paths.
///
/// Consecutive blocks that share a control instance at a depth form that
/// control. A control's blocks must stay contiguous: an instance that reappears
/// after other blocks is a broken hierarchy, refused by the caller's error.
pub fn rebuild(
    blocks: &mut [TrackedBlock],
    paths: &[Vec<usize>],
    controls: &[SdtWrapper],
) -> Result<(), String> {
    fn build(
        paths: &[Vec<usize>],
        controls: &[SdtWrapper],
        lo: usize,
        hi: usize,
        depth: usize,
        closed: &mut Vec<usize>,
    ) -> Result<Vec<(usize, BlockSdtWrap)>, String> {
        let mut out = Vec::new();
        let mut index = lo;
        while index < hi {
            let Some(&instance) = paths[index].get(depth) else {
                index += 1;
                continue;
            };
            if closed.contains(&instance) {
                return Err(format!(
                    "content control {instance} resumes at block {index} after other blocks"
                ));
            }
            let mut end = index + 1;
            while end < hi && paths[end].get(depth) == Some(&instance) {
                end += 1;
            }
            closed.push(instance);
            let nested = build(paths, controls, index, end, depth + 1, closed)?
                .into_iter()
                .map(|(start, control)| NestedBlockSdtWrap {
                    start: start - index,
                    control,
                })
                .collect();
            out.push((
                index,
                BlockSdtWrap {
                    wrapper: controls[instance].clone(),
                    span: end - index,
                    nested,
                },
            ));
            index = end;
        }
        Ok(out)
    }
    assert_eq!(blocks.len(), paths.len(), "one control path per block");
    for tracked in blocks.iter_mut() {
        tracked.block_sdt_wrap = None;
    }
    for (start, wrap) in build(paths, controls, 0, blocks.len(), 0, &mut Vec::new())? {
        blocks[start].block_sdt_wrap = Some(wrap);
    }
    Ok(())
}

/// Restore `membership` on `blocks` after blocks were removed (a resolution,
/// which creates none).
pub fn restore_after_removal(blocks: &mut [TrackedBlock], membership: &ControlMembership) {
    restore_after_edit(blocks, membership)
        .expect("removing blocks keeps every control's remaining blocks contiguous");
}

/// Restore `membership` on `blocks` after an edit changed the block list.
///
/// A block that was there before keeps its controls. A run of new blocks
/// joins the controls of the blocks it replaced (the original blocks removed
/// between its kept neighbours), else of the kept block before it: an
/// insertion after a control's last block is the next block inside that
/// control, and one before its first block comes before the control, at the
/// start of a story too. `Err` names a control whose blocks the edit
/// separated.
pub fn restore_after_edit(
    blocks: &mut [TrackedBlock],
    membership: &ControlMembership,
) -> Result<(), String> {
    if membership.controls.is_empty() {
        return Ok(());
    }
    let before = &membership.order;
    let position: HashMap<&NodeId, usize> = before
        .iter()
        .enumerate()
        .map(|(index, id)| (id, index))
        .collect();
    let ids: Vec<NodeId> = blocks.iter().map(tracked_block_id).cloned().collect();
    let present: std::collections::HashSet<&NodeId> = ids.iter().collect();
    let kept = |index: usize| position.get(&ids[index]).copied();
    let mut paths: Vec<Vec<usize>> = Vec::with_capacity(blocks.len());
    let mut index = 0;
    while index < blocks.len() {
        if kept(index).is_some() {
            paths.push(membership.paths[&ids[index]].clone());
            index += 1;
            continue;
        }
        let run_start = index;
        while index < blocks.len() && kept(index).is_none() {
            index += 1;
        }
        let previous = run_start.checked_sub(1).and_then(kept);
        let next = (index < blocks.len()).then(|| kept(index)).flatten();
        let removed_from = previous.map_or(0, |at| at + 1);
        let removed_to = next.unwrap_or(before.len());
        let path = before[removed_from..removed_to.max(removed_from)]
            .iter()
            .find(|id| !present.contains(id))
            .map(|id| membership.paths[id].clone())
            .or_else(|| previous.map(|at| membership.paths[&before[at]].clone()))
            .unwrap_or_default();
        paths.extend(std::iter::repeat_n(path, index - run_start));
    }
    rebuild(blocks, &paths, &membership.controls)
}

/// The block content controls of every story in a document, by story.
#[derive(Clone, Debug, Default)]
pub struct DocumentControls(Vec<(String, ControlMembership)>);

/// The stories of a document as (key, blocks), body first.
fn stories(doc: &mut CanonDoc) -> Vec<(String, &mut Vec<TrackedBlock>)> {
    let mut out = vec![("body".to_string(), &mut doc.blocks)];
    out.extend(
        doc.headers
            .iter_mut()
            .map(|story| (format!("header:{}", story.part_name), &mut story.blocks)),
    );
    out.extend(
        doc.footers
            .iter_mut()
            .map(|story| (format!("footer:{}", story.part_name), &mut story.blocks)),
    );
    out.extend(
        doc.footnotes
            .iter_mut()
            .map(|story| (format!("footnote:{}", story.id), &mut story.blocks)),
    );
    out.extend(
        doc.endnotes
            .iter_mut()
            .map(|story| (format!("endnote:{}", story.id), &mut story.blocks)),
    );
    out.extend(
        doc.comments
            .iter_mut()
            .map(|story| (format!("comment:{}", story.id), &mut story.blocks)),
    );
    out
}

/// Read the controls of every story that has any.
pub fn read_document(doc: &CanonDoc) -> DocumentControls {
    let mut stories: Vec<(String, &[TrackedBlock])> = vec![("body".to_string(), &doc.blocks)];
    stories.extend(
        doc.headers
            .iter()
            .map(|story| (format!("header:{}", story.part_name), &story.blocks[..])),
    );
    stories.extend(
        doc.footers
            .iter()
            .map(|story| (format!("footer:{}", story.part_name), &story.blocks[..])),
    );
    stories.extend(
        doc.footnotes
            .iter()
            .map(|story| (format!("footnote:{}", story.id), &story.blocks[..])),
    );
    stories.extend(
        doc.endnotes
            .iter()
            .map(|story| (format!("endnote:{}", story.id), &story.blocks[..])),
    );
    stories.extend(
        doc.comments
            .iter()
            .map(|story| (format!("comment:{}", story.id), &story.blocks[..])),
    );
    DocumentControls(
        stories
            .into_iter()
            .filter(|(_, blocks)| {
                blocks
                    .iter()
                    .any(|tracked| tracked.block_sdt_wrap.is_some())
            })
            .map(|(key, blocks)| (key, membership(blocks)))
            .collect(),
    )
}

/// Restore the controls read by [`read_document`] after an edit, story by
/// story (a story the edit removed has nothing to restore).
pub fn restore_document(doc: &mut CanonDoc, controls: &DocumentControls) -> Result<(), String> {
    if controls.0.is_empty() {
        return Ok(());
    }
    for (key, blocks) in stories(doc) {
        if let Some((_, membership)) = controls.0.iter().find(|(story, _)| *story == key) {
            restore_after_edit(blocks, membership).map_err(|reason| format!("{key}: {reason}"))?;
        }
    }
    Ok(())
}
