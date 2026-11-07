//! Physical resource ownership for native inserted rows, after structural planning.
use crate::domain::{
    BlockNode, CanonDoc, InlineNode, NodeId, NoteType, OpaqueKind, StoryScope, TableDiffResult,
    TableNode, TableRowAlignment, TrackedBlock,
};
use crate::edit::EditError;
use sha2::{Digest, Sha256};
use std::collections::HashSet;

struct Preparation<'a> {
    doc: &'a mut CanonDoc,
    block_id: &'a NodeId,
    step: usize,
}

impl Preparation<'_> {
    fn error(&self, reason: impl Into<String>) -> EditError {
        EditError::UnsupportedParagraphStructure {
            block_id: self.block_id.clone(),
            step_index: self.step,
            reason: format!("inserted-row resource: {}", reason.into()),
        }
    }

    fn allocate(&self) -> Result<String, EditError> {
        let mut used = HashSet::new();
        for id in self
            .doc
            .footnotes
            .iter()
            .map(|s| &s.id)
            .chain(self.doc.endnotes.iter().map(|s| &s.id))
        {
            let value: i32 = id
                .parse()
                .map_err(|_| self.error(format!("non-numeric note id {id}")))?;
            used.insert(value);
        }
        // Word's note-reference transport space is signed 16-bit. Reserved
        // separator ids are never allocated; scan the available positive space.
        (1..=i16::MAX as i32)
            .find(|id| !used.contains(id))
            .map(|id| id.to_string())
            .ok_or_else(|| self.error("positive note id space exhausted"))
    }

    fn clone_blocks(&self, blocks: &mut [TrackedBlock], footnote: bool) -> Result<(), EditError> {
        fn inspect(block: &BlockNode) -> Result<(), String> {
            match block {
                BlockNode::OpaqueBlock(_) => {
                    return Err(
                        "source-template-only note block cannot be cloned independently".into(),
                    );
                }
                BlockNode::Paragraph(p) => {
                    for inline in p.segments.iter().flat_map(|s| &s.inlines) {
                        let raw = match inline {
                            InlineNode::OpaqueInline(o) => {
                                if matches!(
                                    o.kind,
                                    OpaqueKind::FootnoteReference(_)
                                        | OpaqueKind::EndnoteReference(_)
                                        | OpaqueKind::CommentReference(_)
                                ) {
                                    return Err("note body contains another reference requiring resource remapping".into());
                                }
                                o.raw_xml.as_deref()
                            }
                            InlineNode::Decoration(d) => d.raw_xml.as_deref(),
                            InlineNode::CommentRangeStart { .. }
                            | InlineNode::CommentRangeEnd { .. } => {
                                return Err(
                                    "note contains comment anchors requiring resource remapping"
                                        .into(),
                                );
                            }
                            _ => None,
                        };
                        if let Some(raw) = raw {
                            let element = crate::word_xml::parse_raw_fragment(raw)
                                .map_err(|e| format!("invalid note payload: {e}"))?;
                            fn owned(element: &xmltree::Element) -> Option<String> {
                                if element.attributes.keys().any(|name| {
                                    name.local_name == "paraId"
                                        && name.namespace.as_deref() == Some("http://schemas.microsoft.com/office/word/2010/wordml")
                                }) {
                                    return Some("opaque paragraph identity".into());
                                }
                                // These carry external/paired identities, not
                                // merely formatting. Copying them needs a graph
                                // remap, so refuse before mutating any content.
                                if [
                                    "bookmarkStart",
                                    "bookmarkEnd",
                                    "commentRangeStart",
                                    "commentRangeEnd",
                                    "commentReference",
                                    "permStart",
                                    "permEnd",
                                    "docPr",
                                    "cNvPr",
                                    "sdt",
                                    "footnoteReference",
                                    "endnoteReference",
                                    "ins",
                                    "del",
                                    "moveFrom",
                                    "moveTo",
                                    "rPrChange",
                                    "pPrChange",
                                    "tblPrChange",
                                    "trPrChange",
                                    "tcPrChange",
                                ]
                                .contains(&element.name.as_str())
                                {
                                    return Some(element.name.clone());
                                }
                                element.children.iter().find_map(|c| match c {
                                    xmltree::XMLNode::Element(c) => owned(c),
                                    _ => None,
                                })
                            }
                            if let Some(name) = owned(&element) {
                                return Err(format!(
                                    "note contains {name}; its resource identities require explicit remapping"
                                ));
                            }
                        }
                    }
                }
                BlockNode::Table(t) => {
                    if !t.range_markers.is_empty() {
                        return Err("note table has range boundaries requiring remapping".into());
                    }
                    for cell in t.rows.iter().flat_map(|r| &r.cells) {
                        if cell.row_sdt_wrapper.is_some() || !cell.content_sdt_wraps.is_empty() {
                            return Err("note table contains content-control identities requiring remapping".into());
                        }
                        for block in &cell.blocks {
                            inspect(block)?;
                        }
                    }
                }
            }
            Ok(())
        }
        for block in blocks.iter() {
            if block.block_sdt_wrap.is_some() {
                return Err(
                    self.error("note has a content-control envelope requiring identity remapping")
                );
            }
            inspect(&block.block).map_err(|reason| self.error(reason))?;
        }
        let mut used = HashSet::new();
        for block in self
            .doc
            .blocks
            .iter()
            .chain(self.doc.headers.iter().flat_map(|s| &s.blocks))
            .chain(self.doc.footers.iter().flat_map(|s| &s.blocks))
            .chain(self.doc.footnotes.iter().flat_map(|s| &s.blocks))
            .chain(self.doc.endnotes.iter().flat_map(|s| &s.blocks))
            .chain(self.doc.comments.iter().flat_map(|s| &s.blocks))
        {
            super::super::collect_block_node_ids(&block.block, &mut used);
        }
        let mut paragraph_ids = HashSet::new();
        let existing: Vec<_> = if footnote {
            self.doc.footnotes.iter().flat_map(|s| &s.blocks).collect()
        } else {
            self.doc.endnotes.iter().flat_map(|s| &s.blocks).collect()
        };
        for block in existing {
            collect_paragraph_ids(&block.block, &mut paragraph_ids)
                .map_err(|reason| self.error(reason))?;
        }
        for block in blocks {
            super::super::reassign_cloned_block_ids(&mut block.block, &mut used);
            remint_paragraph_ids(&mut block.block, &mut paragraph_ids)
                .map_err(|reason| self.error(reason))?;
        }
        Ok(())
    }

    fn clone_note(&mut self, footnote: bool, id: &str) -> Result<String, EditError> {
        let scope = if footnote {
            StoryScope::Footnote { id: id.into() }
        } else {
            StoryScope::Endnote { id: id.into() }
        };
        if crate::tracked_model::enumerate_revisions(self.doc)
            .iter()
            .any(|r| r.location == scope)
        {
            return Err(self.error(format!(
                "note {id} contains pending revisions; resolve them before cloning its resource"
            )));
        }
        let new_id = self.allocate()?;
        if footnote {
            let mut note = self
                .doc
                .footnotes
                .iter()
                .find(|s| s.id == id)
                .cloned()
                .ok_or_else(|| self.error(format!("missing footnote {id}")))?;
            if note.note_type != NoteType::Normal {
                return Err(self.error(format!("footnote {id} is reserved")));
            }
            self.clone_blocks(&mut note.blocks, true)?;
            note.id = new_id.clone();
            self.doc.footnotes.push(note);
        } else {
            let mut note = self
                .doc
                .endnotes
                .iter()
                .find(|s| s.id == id)
                .cloned()
                .ok_or_else(|| self.error(format!("missing endnote {id}")))?;
            if note.note_type != NoteType::Normal {
                return Err(self.error(format!("endnote {id} is reserved")));
            }
            self.clone_blocks(&mut note.blocks, false)?;
            note.id = new_id.clone();
            self.doc.endnotes.push(note);
        }
        Ok(new_id)
    }

    fn block(&mut self, block: &mut BlockNode) -> Result<(), EditError> {
        match block {
            BlockNode::Paragraph(p) => {
                for inline in p.segments.iter_mut().flat_map(|s| &mut s.inlines) {
                    let InlineNode::OpaqueInline(opaque) = inline else {
                        continue;
                    };
                    let (footnote, reference) = match &mut opaque.kind {
                        OpaqueKind::FootnoteReference(r) => (true, r),
                        OpaqueKind::EndnoteReference(r) => (false, r),
                        _ => {
                            if let Some(raw) = &opaque.raw_xml {
                                let root =
                                    crate::word_xml::parse_raw_fragment(raw).map_err(|e| {
                                        self.error(format!("invalid reference container: {e}"))
                                    })?;
                                let refs = crate::import::note_resource_references(&root)
                                    .map_err(|e| self.error(e.to_string()))?;
                                if !refs.footnotes.is_empty() || !refs.endnotes.is_empty() {
                                    return Err(self.error("note reference inside an opaque container requires explicit transport remapping"));
                                }
                            }
                            continue;
                        }
                    };
                    let old_id = reference.reference_id.clone();
                    let new_id = self.clone_note(footnote, &old_id)?;
                    if let Some(raw) = &mut opaque.raw_xml {
                        let mut root = crate::word_xml::parse_raw_fragment(raw).map_err(|e| {
                            self.error(format!("invalid note reference {old_id}: {e}"))
                        })?;
                        let tag = if footnote {
                            "footnoteReference"
                        } else {
                            "endnoteReference"
                        };
                        if root.name != tag
                            || crate::xml_attrs::attr_get(&root, "w:id") != Some(&old_id)
                        {
                            return Err(self.error(format!(
                                "note {old_id} typed identity disagrees with its raw reference"
                            )));
                        }
                        crate::xml_attrs::attr_set(&mut root, "w:id", &new_id);
                        *raw = crate::word_xml::serialize_raw_fragment(&root);
                        opaque.content_hash = Some(format!("{:x}", Sha256::digest(&*raw)));
                    }
                    reference.reference_id = new_id;
                }
            }
            BlockNode::Table(t) => {
                for block in t
                    .rows
                    .iter_mut()
                    .flat_map(|r| &mut r.cells)
                    .flat_map(|c| &mut c.blocks)
                {
                    self.block(block)?;
                }
            }
            BlockNode::OpaqueBlock(o) => {
                if !o.note_references.footnotes.is_empty() || !o.note_references.endnotes.is_empty()
                {
                    return Err(
                        self.error("source-template-only row content contains note references")
                    );
                }
            }
        }
        Ok(())
    }
}

// MS-DOCX §2.6.2.3: transport paragraph identities are positive 31-bit
// numbers, unique within the note part. Snapshot-local NodeIds are separate.
fn reserve_paragraph_id(id: &str, used: &mut HashSet<u32>) -> Result<(), String> {
    let value = u32::from_str_radix(id, 16)
        .map_err(|_| format!("invalid persisted paragraph identity {id}"))?;
    if value == 0 || value >= 0x8000_0000 {
        return Err(format!(
            "persisted paragraph identity {id} is outside the valid range"
        ));
    }
    used.insert(value);
    Ok(())
}

fn collect_raw_paragraph_ids(raw: &[u8], used: &mut HashSet<u32>) -> Result<(), String> {
    let root = crate::word_xml::parse_raw_fragment(raw)
        .map_err(|e| format!("invalid paragraph identity payload: {e}"))?;
    for id in crate::import::paragraph_transport_ids(&root) {
        reserve_paragraph_id(&id, used)?;
    }
    Ok(())
}

pub(in crate::edit) fn collect_paragraph_ids(
    block: &BlockNode,
    used: &mut HashSet<u32>,
) -> Result<(), String> {
    match block {
        BlockNode::Paragraph(p) => {
            if let Some(id) = &p.para_id {
                reserve_paragraph_id(id, used)?;
            }
            for inline in p.segments.iter().flat_map(|s| &s.inlines) {
                let raw = match inline {
                    InlineNode::OpaqueInline(o) => o.raw_xml.as_deref(),
                    InlineNode::Decoration(d) => d.raw_xml.as_deref(),
                    _ => None,
                };
                if let Some(raw) = raw {
                    collect_raw_paragraph_ids(raw, used)?;
                }
            }
        }
        BlockNode::Table(t) => {
            for row in &t.rows {
                if let Some(id) = &row.para_id {
                    reserve_paragraph_id(id, used)?;
                }
                for block in row.cells.iter().flat_map(|c| &c.blocks) {
                    collect_paragraph_ids(block, used)?;
                }
            }
        }
        BlockNode::OpaqueBlock(o) => {
            for id in &o.source_paragraph_ids {
                reserve_paragraph_id(id, used)?;
            }
        }
    }
    Ok(())
}

fn remint_paragraph_id(id: &mut Option<String>, used: &mut HashSet<u32>) -> Result<(), String> {
    if id.is_some() {
        let value = (1..0x8000_0000u32)
            .find(|value| !used.contains(value))
            .ok_or("paragraph identity space exhausted")?;
        used.insert(value);
        *id = Some(format!("{value:08X}"));
    }
    Ok(())
}

pub(in crate::edit) fn remint_paragraph_ids(
    block: &mut BlockNode,
    used: &mut HashSet<u32>,
) -> Result<(), String> {
    match block {
        BlockNode::Paragraph(p) => {
            for inline in p.segments.iter().flat_map(|s| &s.inlines) {
                let raw = match inline {
                    InlineNode::OpaqueInline(o) => o.raw_xml.as_deref(),
                    InlineNode::Decoration(d) => d.raw_xml.as_deref(),
                    _ => None,
                };
                if let Some(raw) = raw {
                    let mut embedded_ids = HashSet::new();
                    collect_raw_paragraph_ids(raw, &mut embedded_ids)?;
                    if !embedded_ids.is_empty() {
                        return Err(format!(
                            "inserted paragraph {} contains opaque paragraph identities requiring explicit remapping",
                            p.id.0
                        ));
                    }
                }
            }
            remint_paragraph_id(&mut p.para_id, used)?;
        }
        BlockNode::Table(t) => {
            for row in &mut t.rows {
                remint_paragraph_id(&mut row.para_id, used)?;
                for block in row.cells.iter_mut().flat_map(|c| &mut c.blocks) {
                    remint_paragraph_ids(block, used)?;
                }
            }
        }
        BlockNode::OpaqueBlock(o) => {
            if !o.source_paragraph_ids.is_empty() {
                return Err(format!(
                    "inserted source-template block {} contains paragraph identities requiring explicit remapping",
                    o.id.0
                ));
            }
        }
    }
    Ok(())
}

/// Give every footnote/endnote reference in `block` its own copy of its note
/// (a new id, the same content), for content that is inserted beside the
/// tracked deletion of its source: one note referenced twice, even once deleted
/// and once inserted, is a file Word repairs.
pub(crate) fn copy_referenced_notes(
    doc: &mut CanonDoc,
    block: &mut BlockNode,
    block_id: &NodeId,
    step: usize,
) -> Result<(), EditError> {
    Preparation {
        doc,
        block_id,
        step,
    }
    .block(block)
}

pub(crate) fn prepare(
    doc: &mut CanonDoc,
    target: &TableNode,
    plan: &TableDiffResult,
    block_id: &NodeId,
    step: usize,
) -> Result<TableNode, EditError> {
    let mut target = target.clone();
    if !plan
        .row_alignment
        .iter()
        .any(|row| matches!(row, TableRowAlignment::Inserted { .. }))
    {
        return Ok(target);
    }
    let mut preparation = Preparation {
        doc,
        block_id,
        step,
    };
    // Reserve every physical identity in the current story part, including
    // untouched opaque payloads and identities present only in the target.
    let mut paragraph_ids = HashSet::new();
    for tracked in &preparation.doc.blocks {
        collect_paragraph_ids(&tracked.block, &mut paragraph_ids)
            .map_err(|reason| preparation.error(reason))?;
    }
    collect_paragraph_ids(&BlockNode::from(target.clone()), &mut paragraph_ids)
        .map_err(|reason| preparation.error(reason))?;
    for row in &plan.row_alignment {
        if let TableRowAlignment::Inserted { new_row } = row {
            remint_paragraph_id(&mut target.rows[*new_row].para_id, &mut paragraph_ids)
                .map_err(|reason| preparation.error(reason))?;
            for block in target.rows[*new_row]
                .cells
                .iter_mut()
                .flat_map(|c| &mut c.blocks)
            {
                remint_paragraph_ids(block, &mut paragraph_ids)
                    .map_err(|reason| preparation.error(reason))?;
                preparation.block(block)?;
            }
        }
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_paragraph_transport_ids_are_numeric_and_namespace_qualified() {
        let mut used = HashSet::new();
        collect_raw_paragraph_ids(
            br#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:alias="http://schemas.microsoft.com/office/word/2010/wordml" xmlns:other="urn:unrelated" alias:paraId="0000000a"><w:r other:paraId="FFFFFFFF"/></w:p>"#,
            &mut used,
        ).unwrap();
        reserve_paragraph_id("0000000A", &mut used).unwrap();
        assert_eq!(used, HashSet::from([10]));
        for invalid in ["0", "80000000", "FFFFFFFF", "not-hex"] {
            assert!(reserve_paragraph_id(invalid, &mut used).is_err());
        }
    }

    #[test]
    fn cloned_paragraph_identity_is_fresh_but_text_version_is_preserved() {
        let mut paragraph = crate::domain::ParagraphNode::new_story_body(
            "note-paragraph",
            "Unchanged text",
            Some("00000001".into()),
        );
        paragraph.text_id = Some("00000009".into());
        let mut block = BlockNode::Paragraph(Box::new(paragraph));
        let mut used = HashSet::from([1, 2, 3]);
        remint_paragraph_ids(&mut block, &mut used).unwrap();
        let BlockNode::Paragraph(paragraph) = block else {
            unreachable!()
        };
        assert_eq!(paragraph.para_id.as_deref(), Some("00000004"));
        assert_eq!(paragraph.text_id.as_deref(), Some("00000009"));
        assert_eq!(used, HashSet::from([1, 2, 3, 4]));
    }

    #[test]
    fn untouched_opaque_block_reserves_its_source_paragraph_ids() {
        let raw = crate::word_xml::parse_raw_fragment(br#"<w:sdt xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml"><w:sdtContent><w:p w14:paraId="00000001"/><w:p w14:paraId="00000002"/></w:sdtContent></w:sdt>"#).unwrap();
        let block = BlockNode::from(crate::domain::OpaqueBlockNode {
            id: NodeId::from("opaque"),
            kind: OpaqueKind::Sdt,
            opaque_ref: "source-template".into(),
            proof_ref: crate::domain::ProofRef {
                part: crate::domain::DocPart::DocumentXml,
                block_id: NodeId::from("opaque"),
                docx_anchor: "body_index:0".into(),
            },
            note_references: Default::default(),
            range_marker: None,
            source_paragraph_ids: crate::import::paragraph_transport_ids(&raw),
        });
        let before = block.clone();
        let mut used = HashSet::new();
        collect_paragraph_ids(&block, &mut used).unwrap();
        assert_eq!(used, HashSet::from([1, 2]));
        let mut id = Some("00000001".into());
        remint_paragraph_id(&mut id, &mut used).unwrap();
        assert_eq!(id.as_deref(), Some("00000003"));
        assert_eq!(
            block, before,
            "inventory must not rewrite untouched templates"
        );
        assert!(
            remint_paragraph_ids(&mut block.clone(), &mut used).is_err(),
            "cloning opaque identities needs a payload remapper, not an inventory rewrite"
        );
    }
}
