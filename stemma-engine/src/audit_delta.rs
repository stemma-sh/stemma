//! Conservative canonical delta used only by the engine audit.
//!
//! This is deliberately not a document comparison algorithm. Audit compares a
//! baseline with a later state of the same document and needs a total account
//! of changed regions, not inferred lineage or review presentation. Blocks are
//! paired only at the same story position; a structural mismatch is reported
//! as deletion plus insertion. That may be less compact, but it cannot hide a
//! change behind a heuristic match.

use crate::domain::{BlockNode, CanonDoc, HeaderFooterKind, NodeId, TrackedBlock};
use crate::tracked_model::{block_node_id, extract_block_text_for_hash};

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

pub(crate) fn audit_changes(before: &CanonDoc, after: &CanonDoc) -> Vec<AuditDelta> {
    let mut changes = block_changes(&before.blocks, &after.blocks);

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
                    block_changes: block_changes(&source.blocks, &target.blocks),
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
                    block_changes: block_changes(&source.blocks, &target.blocks),
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
        |story| &story.id,
        |story| &story.blocks,
        |id, block_changes| AuditDelta::CommentModified { id, block_changes },
        |id, blocks| AuditDelta::CommentDeleted { id, blocks },
        |id, blocks| AuditDelta::CommentInserted { id, blocks },
    );

    changes
}

fn block_changes(before: &[TrackedBlock], after: &[TrackedBlock]) -> Vec<AuditDelta> {
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
                // reminted decoration references.
                if old_text == new_text {
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
                block_changes(blocks(source), blocks(target)),
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
