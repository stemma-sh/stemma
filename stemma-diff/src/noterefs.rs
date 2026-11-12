//! One note per note reference in a redline.
//!
//! A footnote or endnote reference (`w:footnoteReference` /
//! `w:endnoteReference`, ISO 29500-1 §17.11.14 / §17.11.7) names its note by
//! id, and each note belongs to one reference: two references naming one
//! note is a package Word repairs ("the file appears to be corrupted").
//! A comparison makes two copies of one reference when content holding it is
//! deleted in one place and inserted in another (a moved paragraph, sentence
//! or table row read as deletion plus insertion): the deleted copy comes
//! from the base, the inserted one from the target, and both carry the note
//! id the two documents share.
//!
//! The deleted copy keeps the base note, with the base note's content: it is
//! the base's reference, and Reject All restores it. Each inserted copy gets
//! a note of its own, a new id holding the target note's content, inserted
//! with it (its blocks tracked as inserted, as for a note the target adds):
//! Accept All keeps it. A note whose references are all inserted keeps its id
//! for the first one. Two references of one note that are neither inserted
//! cannot both come from one well-formed base, and are refused.

use std::collections::HashMap;

use crate::domain::{
    BlockNode, CanonDoc, InlineNode, OpaqueKind, RevisionInfo, TrackedBlock, TrackingStatus,
};
use crate::materialize::{MergeError, MergeErrorKind, next_revision};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum NoteKind {
    Footnote,
    Endnote,
}

/// Give every inserted copy of a note reference that shares its note with
/// another reference a note of its own (see the module documentation).
pub(crate) fn split_shared_note_references(
    merged: &mut CanonDoc,
    base: &CanonDoc,
    target: &CanonDoc,
    revision: &RevisionInfo,
    rev_counter: &mut u32,
) -> Result<(), MergeError> {
    // Every reference in the body, in document order: (kind, id, inserted).
    let mut occurrences = Vec::new();
    for tracked in &merged.blocks {
        let inserted = is_inserted(&tracked.status);
        visit_block(&tracked.block, inserted, &mut |kind, id, inserted| {
            occurrences.push((kind, id.to_string(), inserted));
        });
    }

    // Which occurrence of each note keeps it.
    let mut by_note: HashMap<(NoteKind, String), Vec<usize>> = HashMap::new();
    for (index, (kind, id, _)) in occurrences.iter().enumerate() {
        by_note.entry((*kind, id.clone())).or_default().push(index);
    }
    let mut keys: Vec<&(NoteKind, String)> = by_note
        .iter()
        .filter(|(_, indices)| indices.len() > 1)
        .map(|(key, _)| key)
        .collect();
    if keys.is_empty() {
        return Ok(());
    }
    keys.sort_by_key(|(kind, id)| (*kind == NoteKind::Endnote, id.clone()));

    let mut next_id = next_note_id(merged)?;
    let mut renames: HashMap<usize, String> = HashMap::new();
    for key in keys {
        let indices = &by_note[key];
        let owner = indices
            .iter()
            .copied()
            .find(|&index| !occurrences[index].2)
            .unwrap_or(indices[0]);
        let (kind, id) = key;
        for &index in indices {
            if index == owner {
                continue;
            }
            if !occurrences[index].2 {
                return Err(MergeError {
                    kind: MergeErrorKind::UnsupportedEdit,
                    message: format!(
                        "two {kind:?} references to note {id} that are not inserted copies: a note belongs to one reference"
                    ),
                    context: format!("note_id={id}"),
                });
            }
            // The inserted copy came from the target: its note there.
            let blocks = note_blocks(target, *kind, id).ok_or_else(|| MergeError {
                kind: MergeErrorKind::InvalidPlan,
                message: format!(
                    "an inserted {kind:?} reference names note {id}, which the target does not hold"
                ),
                context: format!("note_id={id}"),
            })?;
            let new_id = next_id.to_string();
            next_id += 1;
            let inserted_blocks: Vec<TrackedBlock> = blocks
                .iter()
                .map(|tb| TrackedBlock {
                    status: TrackingStatus::Inserted(next_revision(revision, rev_counter)),
                    block: tb.block.clone(),
                    move_id: None,
                    block_sdt_wrap: None,
                })
                .collect();
            add_note(merged, target, *kind, id, &new_id, inserted_blocks)?;
            renames.insert(index, new_id);
        }
        // A deleted owner is the base's reference: its note reads as the
        // base's, whatever change the plan tracked inside it.
        if !occurrences[owner].2
            && let Some(base_blocks) = note_blocks(base, *kind, id)
        {
            set_note_blocks(merged, *kind, id, base_blocks.to_vec());
        }
    }

    // Rename the inserted copies, visiting in the same order.
    let mut index = 0usize;
    let mut failure = None;
    for tracked in &mut merged.blocks {
        visit_block_mut(&mut tracked.block, &mut |opaque| {
            if let Some(new_id) = renames.get(&index)
                && let Err(error) = rename_reference(opaque, new_id)
            {
                failure.get_or_insert(error);
            }
            index += 1;
        });
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn is_inserted(status: &TrackingStatus) -> bool {
    matches!(status, TrackingStatus::Inserted(_))
}

fn reference(inline: &InlineNode) -> Option<(NoteKind, &str)> {
    let InlineNode::OpaqueInline(opaque) = inline else {
        return None;
    };
    match &opaque.kind {
        OpaqueKind::FootnoteReference(data) => Some((NoteKind::Footnote, &data.reference_id)),
        OpaqueKind::EndnoteReference(data) => Some((NoteKind::Endnote, &data.reference_id)),
        _ => None,
    }
}

/// Visit the note references of a block in document order, each with
/// whether it is inserted (its segment, row or block is).
fn visit_block(block: &BlockNode, inserted: bool, f: &mut dyn FnMut(NoteKind, &str, bool)) {
    match block {
        BlockNode::Paragraph(paragraph) => {
            for segment in &paragraph.segments {
                let inserted = inserted || is_inserted(&segment.status);
                for inline in &segment.inlines {
                    if let Some((kind, id)) = reference(inline) {
                        f(kind, id, inserted);
                    }
                }
            }
        }
        BlockNode::Table(table) => {
            for row in &table.rows {
                let inserted = inserted || row.tracking_status.as_ref().is_some_and(is_inserted);
                for cell in &row.cells {
                    let inserted =
                        inserted || cell.tracking_status.as_ref().is_some_and(is_inserted);
                    for inner in &cell.blocks {
                        visit_block(inner, inserted, f);
                    }
                }
            }
        }
        BlockNode::OpaqueBlock(_) => {}
    }
}

/// [`visit_block`]'s order, handing each note reference over mutably.
fn visit_block_mut(block: &mut BlockNode, f: &mut dyn FnMut(&mut crate::domain::OpaqueInlineNode)) {
    match block {
        BlockNode::Paragraph(paragraph) => {
            for segment in &mut paragraph.segments {
                for inline in &mut segment.inlines {
                    if reference(inline).is_some()
                        && let InlineNode::OpaqueInline(opaque) = inline
                    {
                        f(opaque);
                    }
                }
            }
        }
        BlockNode::Table(table) => {
            for row in &mut table.rows {
                for cell in &mut row.cells {
                    for inner in &mut cell.blocks {
                        visit_block_mut(inner, f);
                    }
                }
            }
        }
        BlockNode::OpaqueBlock(_) => {}
    }
}

/// Point a note reference at note `new_id`: its typed id and the `w:id` of
/// its reference element in its XML.
fn rename_reference(
    opaque: &mut crate::domain::OpaqueInlineNode,
    new_id: &str,
) -> Result<(), MergeError> {
    match &mut opaque.kind {
        OpaqueKind::FootnoteReference(data) | OpaqueKind::EndnoteReference(data) => {
            data.reference_id = new_id.to_string();
        }
        _ => unreachable!("only note references are renamed"),
    }
    let Some(raw) = &opaque.raw_xml else {
        return Ok(());
    };
    let invalid = |message: String| MergeError {
        kind: MergeErrorKind::InvalidPlan,
        message,
        context: format!("note reference {}", opaque.id.0),
    };
    let mut element = xmltree::Element::parse(raw.as_slice())
        .map_err(|e| invalid(format!("note reference XML could not be parsed: {e}")))?;
    fn rewrite(element: &mut xmltree::Element, new_id: &str) -> bool {
        let mut changed = false;
        if matches!(
            element.name.as_str(),
            "footnoteReference" | "endnoteReference"
        ) {
            for (name, value) in element.attributes.iter_mut() {
                if name.local_name == "id" {
                    *value = new_id.to_string();
                    changed = true;
                }
            }
        }
        for child in &mut element.children {
            if let xmltree::XMLNode::Element(child) = child {
                changed |= rewrite(child, new_id);
            }
        }
        changed
    }
    if !rewrite(&mut element, new_id) {
        return Err(invalid(
            "note reference XML holds no reference element with an id".to_string(),
        ));
    }
    let mut rewritten = Vec::new();
    element
        .write_with_config(
            &mut rewritten,
            xmltree::EmitterConfig::new().write_document_declaration(false),
        )
        .map_err(|e| invalid(format!("note reference XML could not be written: {e}")))?;
    opaque.raw_xml = Some(rewritten);
    Ok(())
}

/// The next unused note id: one more than every footnote and endnote id
/// (they share Word's numeric note-id space).
fn next_note_id(doc: &CanonDoc) -> Result<i64, MergeError> {
    let mut max = 0i64;
    for id in doc
        .footnotes
        .iter()
        .map(|note| note.id.as_str())
        .chain(doc.endnotes.iter().map(|note| note.id.as_str()))
    {
        let n: i64 = id.parse().map_err(|_| MergeError {
            kind: MergeErrorKind::UnsupportedEdit,
            message: format!("note id {id:?} is not numeric; no new note id can follow it"),
            context: format!("note_id={id}"),
        })?;
        max = max.max(n);
    }
    Ok(max + 1)
}

fn note_blocks<'d>(doc: &'d CanonDoc, kind: NoteKind, id: &str) -> Option<&'d [TrackedBlock]> {
    match kind {
        NoteKind::Footnote => doc
            .footnotes
            .iter()
            .find(|note| note.id == id)
            .map(|note| note.blocks.as_slice()),
        NoteKind::Endnote => doc
            .endnotes
            .iter()
            .find(|note| note.id == id)
            .map(|note| note.blocks.as_slice()),
    }
}

fn set_note_blocks(doc: &mut CanonDoc, kind: NoteKind, id: &str, blocks: Vec<TrackedBlock>) {
    match kind {
        NoteKind::Footnote => {
            if let Some(note) = doc.footnotes.iter_mut().find(|note| note.id == id) {
                note.blocks = blocks;
            }
        }
        NoteKind::Endnote => {
            if let Some(note) = doc.endnotes.iter_mut().find(|note| note.id == id) {
                note.blocks = blocks;
            }
        }
    }
}

/// Add note `new_id` to `merged`: the target's note `source_id` (its type and
/// attributes) with `blocks`.
fn add_note(
    merged: &mut CanonDoc,
    target: &CanonDoc,
    kind: NoteKind,
    source_id: &str,
    new_id: &str,
    blocks: Vec<TrackedBlock>,
) -> Result<(), MergeError> {
    let missing = || MergeError {
        kind: MergeErrorKind::InvalidPlan,
        message: format!("the target holds no {kind:?} {source_id}"),
        context: format!("note_id={source_id}"),
    };
    match kind {
        NoteKind::Footnote => {
            let mut note = target
                .footnotes
                .iter()
                .find(|note| note.id == source_id)
                .ok_or_else(missing)?
                .clone();
            note.id = new_id.to_string();
            note.blocks = blocks;
            merged.footnotes.push(note);
        }
        NoteKind::Endnote => {
            let mut note = target
                .endnotes
                .iter()
                .find(|note| note.id == source_id)
                .ok_or_else(missing)?
                .clone();
            note.id = new_id.to_string();
            note.blocks = blocks;
            merged.endnotes.push(note);
        }
    }
    Ok(())
}
