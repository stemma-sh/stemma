//! Conservative canonical delta used only by the engine audit.
//!
//! This is deliberately not a document comparison algorithm. Audit compares a
//! baseline with a later state of the same document and needs a total account
//! of changed regions, not inferred lineage or review presentation. Blocks are
//! paired by shared session identity or explicit insertion carriers plus
//! baseline positions for independent parses. Pairing never
//! exempts a matched block from content comparison.

use crate::domain::{BlockNode, CanonDoc, HeaderFooterKind, NodeId, TrackedBlock, TrackingStatus};
use crate::tracked_model::{block_node_id, extract_block_text_for_hash};

/// Provenance of block IDs, supplied by the caller rather than guessed from
/// overlapping numeric IDs. Independent imports allocate their own IDs.
#[derive(Clone, Copy)]
pub(crate) enum BlockIdentity {
    SharedSession,
    IndependentParses,
}

#[derive(PartialEq, Eq)]
enum BlockKey {
    Session(NodeId),
    BaselinePosition(usize),
    Insertion { revision: u32, occurrence: usize },
}

fn block_keys(blocks: &[TrackedBlock], identity: BlockIdentity) -> Vec<BlockKey> {
    let mut baseline_position = 0;
    let mut occurrences = std::collections::HashMap::<u32, usize>::new();
    blocks
        .iter()
        .map(|block| match identity {
            BlockIdentity::SharedSession => BlockKey::Session(block_node_id(&block.block)),
            BlockIdentity::IndependentParses => {
                // Explicit whole-block insertions do not consume a baseline
                // position. No text similarity, inferred moves, or import-local
                // numeric IDs establish lineage between independently read files.
                let insertion = match &block.status {
                    TrackingStatus::Inserted(revision) => Some(revision.identity),
                    TrackingStatus::InsertedThenDeleted(stacked) => Some(stacked.inserted.identity),
                    TrackingStatus::Normal | TrackingStatus::Deleted(_) => None,
                };
                if let Some(revision) = insertion {
                    let occurrence = occurrences.entry(revision).or_default();
                    let key = BlockKey::Insertion {
                        revision,
                        occurrence: *occurrence,
                    };
                    *occurrence += 1;
                    key
                } else {
                    let key = BlockKey::BaselinePosition(baseline_position);
                    baseline_position += 1;
                    key
                }
            }
        })
        .collect()
}

#[allow(clippy::large_enum_variant)]
pub(crate) enum AuditDelta {
    BlockDeleted {
        block_id: NodeId,
        old_text: String,
    },
    BlockInserted {
        block: BlockNode,
    },
    BlockModified {
        block_id: NodeId,
        old_text: String,
        new_text: String,
        new_block: BlockNode,
    },
    TableStructureChanged {
        table_id: NodeId,
        target_table_id: NodeId,
        old_text: String,
        new_text: String,
    },
    HeaderModified {
        kind: HeaderFooterKind,
        base_part_name: String,
        target_part_name: String,
        block_changes: Vec<AuditDelta>,
    },
    HeaderDeleted {
        kind: HeaderFooterKind,
        part_name: String,
        blocks: Vec<BlockNode>,
    },
    HeaderInserted {
        kind: HeaderFooterKind,
        part_name: String,
        blocks: Vec<BlockNode>,
    },
    FooterModified {
        kind: HeaderFooterKind,
        base_part_name: String,
        target_part_name: String,
        block_changes: Vec<AuditDelta>,
    },
    FooterDeleted {
        kind: HeaderFooterKind,
        part_name: String,
        blocks: Vec<BlockNode>,
    },
    FooterInserted {
        kind: HeaderFooterKind,
        part_name: String,
        blocks: Vec<BlockNode>,
    },
    FootnoteModified {
        id: String,
        block_changes: Vec<AuditDelta>,
    },
    FootnoteDeleted {
        id: String,
        blocks: Vec<BlockNode>,
    },
    FootnoteInserted {
        id: String,
        blocks: Vec<BlockNode>,
    },
    EndnoteModified {
        id: String,
        block_changes: Vec<AuditDelta>,
    },
    EndnoteDeleted {
        id: String,
        blocks: Vec<BlockNode>,
    },
    EndnoteInserted {
        id: String,
        blocks: Vec<BlockNode>,
    },
    CommentModified {
        id: String,
        block_changes: Vec<AuditDelta>,
    },
    CommentDeleted {
        id: String,
        blocks: Vec<BlockNode>,
    },
    CommentInserted {
        id: String,
        blocks: Vec<BlockNode>,
    },
}

pub(crate) fn audit_changes(
    before: &CanonDoc,
    after: &CanonDoc,
    identity: BlockIdentity,
) -> Vec<AuditDelta> {
    let mut changes = block_changes(&before.blocks, &after.blocks, identity);

    // A kind is a rendering role, not a story identity. Multiple sections may
    // bind distinct physical parts as their `default` (or `first` / `even`)
    // story. Audit is a same-document delta, so the canonical part name is the
    // stable identity; a role change on the same part is conservatively
    // reported as removal plus insertion.
    for source in &before.headers {
        match after
            .headers
            .iter()
            .find(|target| target.part_name == source.part_name && target.kind == source.kind)
        {
            Some(target) if target.blocks != source.blocks => {
                changes.push(AuditDelta::HeaderModified {
                    kind: source.kind.clone(),
                    base_part_name: source.part_name.clone(),
                    target_part_name: target.part_name.clone(),
                    block_changes: block_changes(&source.blocks, &target.blocks, identity),
                });
            }
            None => changes.push(AuditDelta::HeaderDeleted {
                kind: source.kind.clone(),
                part_name: source.part_name.clone(),
                blocks: plain_blocks(&source.blocks),
            }),
            _ => {}
        }
    }
    for target in &after.headers {
        if !before
            .headers
            .iter()
            .any(|source| source.part_name == target.part_name && source.kind == target.kind)
        {
            changes.push(AuditDelta::HeaderInserted {
                kind: target.kind.clone(),
                part_name: target.part_name.clone(),
                blocks: plain_blocks(&target.blocks),
            });
        }
    }

    for source in &before.footers {
        match after
            .footers
            .iter()
            .find(|target| target.part_name == source.part_name && target.kind == source.kind)
        {
            Some(target) if target.blocks != source.blocks => {
                changes.push(AuditDelta::FooterModified {
                    kind: source.kind.clone(),
                    base_part_name: source.part_name.clone(),
                    target_part_name: target.part_name.clone(),
                    block_changes: block_changes(&source.blocks, &target.blocks, identity),
                });
            }
            None => changes.push(AuditDelta::FooterDeleted {
                kind: source.kind.clone(),
                part_name: source.part_name.clone(),
                blocks: plain_blocks(&source.blocks),
            }),
            _ => {}
        }
    }
    for target in &after.footers {
        if !before
            .footers
            .iter()
            .any(|source| source.part_name == target.part_name && source.kind == target.kind)
        {
            changes.push(AuditDelta::FooterInserted {
                kind: target.kind.clone(),
                part_name: target.part_name.clone(),
                blocks: plain_blocks(&target.blocks),
            });
        }
    }

    append_note_changes(
        &mut changes,
        &before.footnotes,
        &after.footnotes,
        identity,
        |story| &story.id,
        |story| &story.blocks,
        |id, block_changes| AuditDelta::FootnoteModified { id, block_changes },
        |id, blocks| AuditDelta::FootnoteDeleted { id, blocks },
        |id, blocks| AuditDelta::FootnoteInserted { id, blocks },
    );
    append_note_changes(
        &mut changes,
        &before.endnotes,
        &after.endnotes,
        identity,
        |story| &story.id,
        |story| &story.blocks,
        |id, block_changes| AuditDelta::EndnoteModified { id, block_changes },
        |id, blocks| AuditDelta::EndnoteDeleted { id, blocks },
        |id, blocks| AuditDelta::EndnoteInserted { id, blocks },
    );
    append_note_changes(
        &mut changes,
        &before.comments,
        &after.comments,
        identity,
        |story| &story.id,
        |story| &story.blocks,
        |id, block_changes| AuditDelta::CommentModified { id, block_changes },
        |id, blocks| AuditDelta::CommentDeleted { id, blocks },
        |id, blocks| AuditDelta::CommentInserted { id, blocks },
    );

    changes
}

fn block_alignment(
    before: &[TrackedBlock],
    after: &[TrackedBlock],
    identity: BlockIdentity,
) -> Vec<similar::DiffOp> {
    use similar::algorithms::{Capture, Replace, myers};
    let old = block_keys(before, identity);
    let new = block_keys(after, identity);
    let mut capture = Replace::new(Capture::new());
    myers::diff(&mut capture, &old, 0..old.len(), &new, 0..new.len())
        .expect("infallible audit alignment capture");
    capture.into_inner().into_ops()
}

/// Counterparts used by metadata accounting, under the SAME correspondence
/// policy as the block delta. Numeric IDs from independent imports are not
/// usable as a cross-document lookup key.
pub(crate) fn paired_block_indices(
    before: &[TrackedBlock],
    after: &[TrackedBlock],
    identity: BlockIdentity,
) -> Vec<(usize, usize)> {
    use similar::DiffOp;
    let mut pairs = Vec::new();
    for operation in block_alignment(before, after, identity) {
        match operation {
            DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => {
                pairs.extend((0..len).map(|n| (old_index + n, new_index + n)));
            }
            DiffOp::Replace {
                old_index,
                new_index,
                old_len,
                new_len,
            } if matches!(identity, BlockIdentity::IndependentParses) => {
                pairs.extend((0..old_len.min(new_len)).map(|n| (old_index + n, new_index + n)));
            }
            _ => {}
        }
    }
    pairs
}

fn block_changes(
    before: &[TrackedBlock],
    after: &[TrackedBlock],
    identity: BlockIdentity,
) -> Vec<AuditDelta> {
    use similar::DiffOp;
    let mut changes = Vec::new();
    for operation in block_alignment(before, after, identity) {
        match operation {
            DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => changes.extend(positional_block_changes(
                &before[old_index..old_index + len],
                &after[new_index..new_index + len],
            )),
            DiffOp::Delete {
                old_index, old_len, ..
            } => {
                changes.extend(
                    before[old_index..old_index + old_len]
                        .iter()
                        .map(|b| deleted(&b.block)),
                );
            }
            DiffOp::Insert {
                new_index, new_len, ..
            } => {
                changes.extend(
                    after[new_index..new_index + new_len]
                        .iter()
                        .map(|b| inserted(&b.block)),
                );
            }
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                let removed = &before[old_index..old_index + old_len];
                let added = &after[new_index..new_index + new_len];
                match identity {
                    // Different session IDs are different blocks, not a
                    // modification of whichever neighbor happens to align.
                    BlockIdentity::SharedSession => {
                        changes.extend(removed.iter().map(|b| deleted(&b.block)));
                        changes.extend(added.iter().map(|b| inserted(&b.block)));
                    }
                    // Without shared IDs, compare unmatched regions
                    // conservatively; no content-based lineage is inferred.
                    BlockIdentity::IndependentParses => {
                        changes.extend(positional_block_changes(removed, added))
                    }
                }
            }
        }
    }
    changes
}

fn positional_block_changes(before: &[TrackedBlock], after: &[TrackedBlock]) -> Vec<AuditDelta> {
    let mut changes = Vec::new();
    let shared = before.len().min(after.len());
    for index in 0..shared {
        let source = &before[index];
        let target = &after[index];
        if source == target {
            continue;
        }
        match (&source.block, &target.block) {
            (BlockNode::Table(source_table), BlockNode::Table(target_table)) => {
                // Table identity and cached digests are addressing/derived
                // state, not an edit. Use the same exhaustive fidelity
                // comparison as the untouched proof, including cell content.
                if crate::roundtrip_compare::compare_tracked_block_pair(source, target).is_empty() {
                    continue;
                }
                changes.push(AuditDelta::TableStructureChanged {
                    table_id: source_table.id.clone(),
                    target_table_id: target_table.id.clone(),
                    old_text: extract_block_text_for_hash(&source.block),
                    new_text: extract_block_text_for_hash(&target.block),
                });
            }
            (BlockNode::Paragraph(_), BlockNode::Paragraph(_))
            | (BlockNode::OpaqueBlock(_), BlockNode::OpaqueBlock(_)) => {
                let old_text = extract_block_text_for_hash(&source.block);
                let new_text = extract_block_text_for_hash(&target.block);
                // Audit is intentionally conservative about same-text
                // authored differences. They remain unclaimed here so the
                // exhaustive untouched proof classifies them using the full
                // block comparator. Treating every parse-level inequality as
                // a direct edit would falsely indict revision wrappers and
                // reminted decoration references. A comment reference is not
                // text either: adding or removing a comment is reported as
                // its comment story, so it is left out of this comparison.
                if text_without_comment_references(&source.block)
                    == text_without_comment_references(&target.block)
                {
                    continue;
                }
                changes.push(AuditDelta::BlockModified {
                    block_id: block_node_id(&source.block),
                    old_text,
                    new_text,
                    new_block: target.block.clone(),
                });
            }
            _ => {
                changes.push(deleted(&source.block));
                changes.push(inserted(&target.block));
            }
        }
    }
    for source in &before[shared..] {
        changes.push(deleted(&source.block));
    }
    for target in &after[shared..] {
        changes.push(inserted(&target.block));
    }
    changes
}

fn deleted(block: &BlockNode) -> AuditDelta {
    AuditDelta::BlockDeleted {
        block_id: block_node_id(block),
        old_text: extract_block_text_for_hash(block),
    }
}

fn inserted(block: &BlockNode) -> AuditDelta {
    AuditDelta::BlockInserted {
        block: block.clone(),
    }
}

fn plain_blocks(blocks: &[TrackedBlock]) -> Vec<BlockNode> {
    blocks.iter().map(|tracked| tracked.block.clone()).collect()
}

#[allow(clippy::too_many_arguments)]
fn append_note_changes<T>(
    changes: &mut Vec<AuditDelta>,
    before: &[T],
    after: &[T],
    identity: BlockIdentity,
    id: impl Fn(&T) -> &String,
    blocks: impl Fn(&T) -> &Vec<TrackedBlock>,
    modified: impl Fn(String, Vec<AuditDelta>) -> AuditDelta,
    deleted: impl Fn(String, Vec<BlockNode>) -> AuditDelta,
    inserted: impl Fn(String, Vec<BlockNode>) -> AuditDelta,
) {
    for source in before {
        match after.iter().find(|target| id(target) == id(source)) {
            Some(target) if blocks(target) != blocks(source) => changes.push(modified(
                id(source).clone(),
                block_changes(blocks(source), blocks(target), identity),
            )),
            None => changes.push(deleted(id(source).clone(), plain_blocks(blocks(source)))),
            _ => {}
        }
    }
    for target in after {
        if !before.iter().any(|source| id(source) == id(target)) {
            changes.push(inserted(id(target).clone(), plain_blocks(blocks(target))));
        }
    }
}

/// A block's hash text with its comment references removed (they render as
/// object placeholders there but carry no document text).
fn text_without_comment_references(block: &BlockNode) -> String {
    let BlockNode::Paragraph(paragraph) = block else {
        return extract_block_text_for_hash(block);
    };
    let mut paragraph = paragraph.clone();
    for segment in &mut paragraph.segments {
        segment.inlines.retain(|inline| {
            !matches!(
                inline,
                crate::domain::InlineNode::OpaqueInline(opaque)
                    if matches!(opaque.kind, crate::domain::OpaqueKind::CommentReference(_))
            )
        });
    }
    extract_block_text_for_hash(&BlockNode::Paragraph(paragraph))
}
