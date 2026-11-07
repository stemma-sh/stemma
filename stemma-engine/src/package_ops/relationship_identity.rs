//! Package-independent identity for opaque inline content.
//!
//! Relationship ids are package-local transport (ISO 29500-2 §9.3): one
//! picture is `r:embed="rId4"` in one package and `rId8` in another, and two
//! different pictures can both be `rId4`. An opaque's `content_hash` is the
//! SHA-256 of its raw XML — the transport bytes, which every in-place rewrite
//! keeps current — so two documents cannot be compared by it once either
//! carries a relationship reference: a renumbering would read as an edit and a
//! replaced image behind an unchanged id would read as no change.
//!
//! Word also mints drawing identifiers as it saves (`wp14:anchorId`,
//! `wp14:editId`; MS-DOCX §2.3.4, §2.3.5), so a document Word merely re-saved
//! would compare as having changed every picture.
//!
//! [`resolve`] rewrites the hashes of a comparison-local copy of a document to
//! its package-independent identity: every relationship reference is hashed
//! as what it resolves to through its own story part's relationships (an
//! internal part's bytes plus that part's own relationship closure, or an
//! external target), and Word's drawing session identifiers are left out.
//! [`restore_transport_hashes`] is the exact inverse for
//! content that leaves the comparison, so no document outside it ever carries
//! a hash that its own raw XML would not reproduce.

use std::collections::HashMap;

use xmltree::{Element, XMLNode};

use crate::docx_package::{DocxPackage, RelationshipSet};
use crate::domain::{BlockNode, CanonDoc, InlineNode, OpaqueInlineNode, TrackedBlock};
use crate::import::sha256_hex;
use crate::runtime::{
    COMMENTS_REL_TYPE, ENDNOTES_REL_TYPE, ErrorCode, ErrorDetails, FOOTNOTES_REL_TYPE, RuntimeError,
};
use crate::word_xml;

/// Replace each transport-bearing opaque's `content_hash` in `doc` with its
/// identity resolved through `package`, the package `doc` was read from.
/// Opaques without relationship references or drawing session identifiers
/// keep their raw-XML hash, which is already package-independent.
pub(crate) fn resolve(package: &DocxPackage, doc: &mut CanonDoc) -> Result<(), RuntimeError> {
    let mut resolver = Resolver {
        package,
        part_identities: HashMap::new(),
    };
    let main_part = package.main_document_part_name().to_string();
    resolver.resolve_story(&main_part, &mut doc.blocks)?;
    for header in &mut doc.headers {
        let part = package
            .document_rels
            .resolve_internal_target(&header.part_name);
        resolver.resolve_story(&part, &mut header.blocks)?;
    }
    for footer in &mut doc.footers {
        let part = package
            .document_rels
            .resolve_internal_target(&footer.part_name);
        resolver.resolve_story(&part, &mut footer.blocks)?;
    }
    let note_part = |rel_type: &str| -> Option<String> {
        package
            .document_rels
            .entries
            .iter()
            .find(|rel| rel.rel_type == rel_type)
            .map(|rel| package.document_rels.resolve_internal_target(&rel.target))
    };
    if let Some(part) = note_part(FOOTNOTES_REL_TYPE) {
        for note in &mut doc.footnotes {
            resolver.resolve_story(&part, &mut note.blocks)?;
        }
    }
    if let Some(part) = note_part(ENDNOTES_REL_TYPE) {
        for note in &mut doc.endnotes {
            resolver.resolve_story(&part, &mut note.blocks)?;
        }
    }
    if let Some(part) = note_part(COMMENTS_REL_TYPE) {
        for comment in &mut doc.comments {
            resolver.resolve_story(&part, &mut comment.blocks)?;
        }
    }
    Ok(())
}

/// Return every transport-bearing opaque in `doc` to its transport hash, the
/// SHA-256 of its raw XML.
pub(crate) fn restore_transport_hashes(doc: &mut CanonDoc) -> Result<(), RuntimeError> {
    let stories = std::iter::once(&mut doc.blocks)
        .chain(doc.headers.iter_mut().map(|story| &mut story.blocks))
        .chain(doc.footers.iter_mut().map(|story| &mut story.blocks))
        .chain(doc.footnotes.iter_mut().map(|story| &mut story.blocks))
        .chain(doc.endnotes.iter_mut().map(|story| &mut story.blocks))
        .chain(doc.comments.iter_mut().map(|story| &mut story.blocks));
    for blocks in stories {
        for opaque in hashed_opaques(blocks) {
            let raw = opaque
                .raw_xml
                .as_ref()
                .expect("hashed opaques carry raw XML");
            let root = parse_opaque(opaque, raw, "restoring its transport hash")?;
            if has_transport_attribute(&root) {
                opaque.content_hash = crate::import::opaque_content_hash(&opaque.kind, raw);
            }
        }
    }
    Ok(())
}

struct Resolver<'a> {
    package: &'a DocxPackage,
    /// Memoized identity per internal part path.
    part_identities: HashMap<String, String>,
}

impl Resolver<'_> {
    fn resolve_story(
        &mut self,
        story_part: &str,
        blocks: &mut [TrackedBlock],
    ) -> Result<(), RuntimeError> {
        let rels = part_relationships(self.package, story_part);
        for opaque in hashed_opaques(blocks) {
            let raw = opaque
                .raw_xml
                .as_ref()
                .expect("hashed opaques carry raw XML");
            let mut root = parse_opaque(opaque, raw, "resolving its relationship identity")?;
            let resolved = self.replace_relationship_ids(&mut root, rels)?;
            if remove_session_ids(&mut root) | resolved {
                opaque.content_hash = Some(sha256_hex(&word_xml::serialize_raw_fragment(&root)));
            }
        }
        Ok(())
    }

    /// Replace every relationship-id attribute value in `element` with the
    /// identity it resolves to. Returns whether any attribute was replaced.
    fn replace_relationship_ids(
        &mut self,
        element: &mut Element,
        rels: Option<&RelationshipSet>,
    ) -> Result<bool, RuntimeError> {
        let mut replaced = false;
        for (name, value) in &mut element.attributes {
            if is_relationship_id_attribute(name) {
                *value = self.relationship_identity(rels, value, &mut Vec::new())?;
                replaced = true;
            }
        }
        for child in &mut element.children {
            if let XMLNode::Element(child) = child {
                replaced |= self.replace_relationship_ids(child, rels)?;
            }
        }
        Ok(replaced)
    }

    /// The identity of relationship `rid` in `rels`.
    ///
    /// A reference that resolves to nothing is an observable state of the
    /// input (Word renders a missing picture), so it has an identity of its
    /// own rather than an error or the raw id.
    fn relationship_identity(
        &mut self,
        rels: Option<&RelationshipSet>,
        rid: &str,
        visiting: &mut Vec<String>,
    ) -> Result<String, RuntimeError> {
        let Some((rels, relationship)) =
            rels.and_then(|rels| rels.find_by_id(rid).map(|rel| (rels, rel)))
        else {
            return Ok("relationship:missing".to_string());
        };
        if !crate::docx_package::relationship_target_mode_is_internal(
            relationship.target_mode.as_deref(),
        ) {
            return Ok(format!(
                "relationship:external:{}:{}",
                relationship.rel_type, relationship.target
            ));
        }
        let path = rels.resolve_internal_target(&relationship.target);
        let part = self.part_identity(&path, visiting)?;
        Ok(format!("relationship:{}:{part}", relationship.rel_type))
    }

    /// Hash of an internal part's bytes and, recursively, the identities of
    /// the parts it references (a chart's embedded workbook, say). A part's
    /// own relationship ids stay in the hash: they are spelled inside its
    /// bytes, and importing a part keeps its relationship part verbatim.
    fn part_identity(
        &mut self,
        path: &str,
        visiting: &mut Vec<String>,
    ) -> Result<String, RuntimeError> {
        if let Some(identity) = self.part_identities.get(path) {
            return Ok(identity.clone());
        }
        if visiting.iter().any(|open| open == path) {
            return Ok("part:cycle".to_string());
        }
        let Some(bytes) = self.package.get_part(path) else {
            return Ok("part:missing".to_string());
        };
        let mut identity = sha256_hex(bytes);
        if let Some(rels) = part_relationships(self.package, path) {
            visiting.push(path.to_string());
            for relationship in &rels.entries {
                let target = self.relationship_identity(Some(rels), &relationship.id, visiting)?;
                identity.push_str(&format!(";{}={target}", relationship.id));
            }
            visiting.pop();
            identity = sha256_hex(identity.as_bytes());
        }
        self.part_identities
            .insert(path.to_string(), identity.clone());
        Ok(identity)
    }
}

/// The relationships `part` declares, from its own relationships part.
fn part_relationships<'a>(package: &'a DocxPackage, part: &str) -> Option<&'a RelationshipSet> {
    if part.eq_ignore_ascii_case(package.main_document_part_name()) {
        return Some(&package.document_rels);
    }
    let rels_path = crate::docx_package::rels_part_path(part);
    package
        .story_rels
        .iter()
        .find(|(stored, _)| stored.eq_ignore_ascii_case(&rels_path))
        .map(|(_, rels)| rels)
}

/// Opaques whose identity is their raw-XML hash. Typed identities (note
/// references, hyperlinks) carry no hash and are compared by their fields.
fn hashed_opaques(blocks: &mut [TrackedBlock]) -> Vec<&mut OpaqueInlineNode> {
    let mut out = Vec::new();
    for block in blocks {
        collect_opaques(&mut block.block, &mut out);
    }
    out.retain(|opaque| opaque.content_hash.is_some() && opaque.raw_xml.is_some());
    out
}

fn collect_opaques<'a>(block: &'a mut BlockNode, out: &mut Vec<&'a mut OpaqueInlineNode>) {
    match block {
        BlockNode::Paragraph(paragraph) => {
            for segment in &mut paragraph.segments {
                for inline in &mut segment.inlines {
                    if let InlineNode::OpaqueInline(opaque) = inline {
                        out.push(opaque);
                    }
                }
            }
        }
        BlockNode::Table(table) => {
            for row in &mut table.rows {
                for cell in &mut row.cells {
                    for cell_block in &mut cell.blocks {
                        collect_opaques(cell_block, out);
                    }
                }
            }
        }
        BlockNode::OpaqueBlock(_) => {}
    }
}

fn parse_opaque(
    opaque: &OpaqueInlineNode,
    raw: &[u8],
    purpose: &str,
) -> Result<Element, RuntimeError> {
    word_xml::parse_raw_fragment(raw).map_err(|err| RuntimeError {
        code: ErrorCode::InvalidDocx,
        message: format!(
            "opaque {} raw XML does not parse while {purpose}: {err}",
            opaque.opaque_ref
        ),
        details: ErrorDetails {
            context: Some("relationship_identity".to_string()),
            ..ErrorDetails::default()
        },
    })
}

fn has_transport_attribute(element: &Element) -> bool {
    element
        .attributes
        .keys()
        .any(|name| is_relationship_id_attribute(name) || is_session_id_attribute(name))
        || element.children.iter().any(|child| match child {
            XMLNode::Element(child) => has_transport_attribute(child),
            _ => false,
        })
}

/// Drop Word's drawing session identifiers from `element`'s subtree. Returns
/// whether any was present.
fn remove_session_ids(element: &mut Element) -> bool {
    let before = element.attributes.len();
    element
        .attributes
        .retain(|name, _| !is_session_id_attribute(name));
    let mut removed = element.attributes.len() != before;
    for child in &mut element.children {
        if let XMLNode::Element(child) = child {
            removed |= remove_session_ids(child);
        }
    }
    removed
}

/// `wp14:anchorId` and `wp14:editId`: identifiers Word assigns to a drawing as
/// it saves, not drawing content.
fn is_session_id_attribute(name: &xmltree::AttributeName) -> bool {
    name.namespace.as_deref()
        == Some("http://schemas.microsoft.com/office/word/2010/wordprocessingDrawing")
        && matches!(name.local_name.as_str(), "anchorId" | "editId")
}

/// Every attribute in the officeDocument relationships namespace is an
/// `ST_RelationshipId` (`r:embed`, `r:link`, `r:id`, SmartArt's `r:dm`/`r:lo`/
/// `r:qs`/`r:cs`, …); VML's `o:relid` carries the same reference for legacy
/// pictures.
fn is_relationship_id_attribute(name: &xmltree::AttributeName) -> bool {
    let Some(namespace) = name.namespace.as_deref() else {
        return false;
    };
    let namespace = word_xml::canonicalize_ooxml_namespace(namespace);
    namespace == "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
        || (namespace == "urn:schemas-microsoft-com:office:office" && name.local_name == "relid")
}
