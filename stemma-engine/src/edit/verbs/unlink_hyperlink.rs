//! Explicit hyperlink removal: delete the complete object, insert its authored
//! display runs as ordinary text. Uses existing native segment primitives;
//! unrelated revisions are neither projected nor modified.
use super::super::{
    EditError, MaterializationMode, apply_opaque_delete, block_at, block_at_mut, block_id_of,
    check_ancestor_table_tracking, find_hyperlink_path, next_revision, normalize_segments,
    split_segments_at_flat_range, tracking_status_label,
};
use crate::domain::{
    BlockNode, CanonDoc, InlineNode, NodeId, OpaqueKind, RevisionInfo, TrackedSegment,
    TrackingStatus,
};

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply(
    doc: &mut CanonDoc,
    hyperlink_id: &NodeId,
    expect: &str,
    revision: &RevisionInfo,
    counter: &mut u32,
    mode: MaterializationMode,
    step_index: usize,
) -> Result<(), EditError> {
    let path =
        find_hyperlink_path(doc, hyperlink_id).ok_or_else(|| EditError::HyperlinkNotFound {
            hyperlink_id: hyperlink_id.clone(),
            step_index,
        })?;
    // Only the target's containing structural state must be stable. The
    // paragraph-wide replacement guard would wrongly forbid unrelated runs'
    // pending edits, which this exact-object operation leaves untouched.
    if let Some(status) = tracking_status_label(&doc.blocks[path.top_block].status) {
        return Err(EditError::BlockHasTrackedStatus {
            block_id: block_id_of(&doc.blocks[path.top_block].block).clone(),
            status,
            step_index,
        });
    }
    let BlockNode::Paragraph(para) = block_at(doc, &path) else {
        unreachable!()
    };
    let para_id = para.id.clone();
    if !path.is_top_level() {
        check_ancestor_table_tracking(doc, &path, &para_id, step_index)?;
    }
    let refuse = |reason: String| EditError::UnsupportedHyperlinkUnlink {
        hyperlink_id: hyperlink_id.clone(),
        reason,
        step_index,
    };
    let (index, status, data) = para
        .segments
        .iter()
        .flat_map(|segment| {
            segment
                .inlines
                .iter()
                .map(move |inline| (&segment.status, inline))
        })
        .enumerate()
        .find_map(|(index, (status, inline))| match inline {
            InlineNode::OpaqueInline(opaque) if &opaque.id == hyperlink_id => {
                let OpaqueKind::Hyperlink(data) = &opaque.kind else {
                    unreachable!()
                };
                Some((index, status, data))
            }
            _ => None,
        })
        .expect("located hyperlink");
    if *status != TrackingStatus::Normal {
        return Err(refuse(
            "target link already has a pending whole-object revision".into(),
        ));
    }
    if data.text != expect {
        return Err(EditError::ExpectMismatch {
            block_id: hyperlink_id.clone(),
            expected: expect.into(),
            actual_text: data.text.clone(),
            step_index,
        });
    }
    let prefix = NodeId::from(format!("{}_unlink_{}", hyperlink_id.0, counter));
    let text = crate::import::unlinked_text_inlines(data, &prefix)
        .map_err(|error| refuse(error.message))?;
    let BlockNode::Paragraph(para) = block_at_mut(doc, &path) else {
        unreachable!()
    };
    apply_opaque_delete(para, index, mode, revision, counter);
    let (position, status) = match mode {
        MaterializationMode::TrackedChange => (
            index + 1,
            TrackingStatus::Inserted(next_revision(revision, counter)),
        ),
        MaterializationMode::Direct => (index, TrackingStatus::Normal),
    };
    let (mut head, empty, tail) = split_segments_at_flat_range(&para.segments, position, position);
    assert!(
        empty.is_empty(),
        "zero-width insertion has no replaced segment"
    );
    head.push(TrackedSegment {
        status,
        inlines: text,
    });
    head.extend(tail);
    normalize_segments(&mut head);
    para.segments = head;
    Ok(())
}
