//! Story-targeting addressing grammar for the edit engine.
//!
//! A header / footer story reuses the body's `p_{n}` block-id namespace, so a
//! bare block id like `"p_1"` is **not** unique across these stories — the same
//! id can name a block in the body and in `header1.xml`. To make a bare-id
//! story lookup impossible at the type level, every resolver here carries a
//! [`StoryRef`] alongside the [`NodeId`]: you cannot resolve a block without
//! first naming the story it lives in.
//!
//! This is an **edit-layer** type. It is deliberately distinct from
//! `crate::domain::StoryScope` (the IR serde type): keeping them separate means
//! the edit grammar can evolve without touching the IR serialization contract.
//!
//! Scope note: these resolvers serve the header/footer editing verbs
//! (`verbs::headers_footers`), which are the call sites. Footnote / endnote /
//! comment stories are addressed by their domain `w:id` (not a bare block id)
//! directly inside their own verbs (`verbs::footnotes`, `verbs::comments`):
//! those verbs `find` the story by id within the correct collection and operate
//! on it wholesale, so a colliding bare block id is structurally impossible to
//! confuse — they need no `StoryRef`-keyed block resolver. (Proven by
//! `tests/story_addressing.rs::edit_note_with_colliding_body_block_id_lands_in_the_note_not_the_body`.)

use crate::domain::{CanonDoc, HeaderFooterKind, NodeId, TrackedBlock};

use super::{EditError, find_block_index};

/// Which story a block address targets.
///
/// `Header`/`Footer` carry the story's part name — the canonical,
/// document-independent identifier the IR uses for a header/footer story.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoryRef {
    /// The main document body (`doc.blocks`).
    Body,
    /// A header story, addressed by its part name (e.g. "header1.xml"). The
    /// part name is the canonical, document-independent identifier the IR uses
    /// for a header/footer story (see [`crate::domain::HeaderStory::part_name`]).
    /// Header stories reuse the body's `p_{n}` block-id namespace, so the part
    /// name is required to disambiguate.
    Header(String),
    /// A footer story, addressed by its part name (e.g. "footer1.xml").
    Footer(String),
}

impl std::fmt::Display for StoryRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoryRef::Body => f.write_str("body"),
            StoryRef::Header(part) => write!(f, "header '{part}'"),
            StoryRef::Footer(part) => write!(f, "footer '{part}'"),
        }
    }
}

/// Resolve a header/footer story addressed by `(kind, part_name)`. A header or
/// footer story can be named two ways:
/// - by exact `part_name` ([`StoryRef::Header`] / [`StoryRef::Footer`] carry it
///   verbatim), or
/// - by `kind` (Default / First / Even) — the verb edge resolves the kind to a
///   part name via these helpers when the caller addressed the story by kind.
///
/// We keep the kind→part resolution here (next to the resolver) so the verb
/// stays a thin adapter. A kind with no matching story is the caller's
/// responsibility to surface as [`EditError::StoryNotFound`] /
/// `HeaderFooterRefNotResolvable`.
pub(crate) fn header_part_for_kind(doc: &CanonDoc, kind: &HeaderFooterKind) -> Option<String> {
    doc.headers
        .iter()
        .find(|h| &h.kind == kind)
        .map(|h| h.part_name.clone())
}

pub(crate) fn footer_part_for_kind(doc: &CanonDoc, kind: &HeaderFooterKind) -> Option<String> {
    doc.footers
        .iter()
        .find(|f| &f.kind == kind)
        .map(|f| f.part_name.clone())
}

/// Resolve a [`StoryRef`] to its mutable block vector.
///
/// A missing header/footer part is a hard error
/// ([`EditError::StoryNotFound`]) carrying the story and step — never a
/// silent fallback to the body or the first story.
pub(crate) fn story_blocks_mut<'a>(
    doc: &'a mut CanonDoc,
    story: &StoryRef,
    step_index: usize,
) -> Result<&'a mut Vec<TrackedBlock>, EditError> {
    match story {
        StoryRef::Body => Ok(&mut doc.blocks),
        StoryRef::Header(part) => doc
            .headers
            .iter_mut()
            .find(|s| &s.part_name == part)
            .map(|s| &mut s.blocks)
            .ok_or_else(|| EditError::StoryNotFound {
                story: story.clone(),
                step_index,
            }),
        StoryRef::Footer(part) => doc
            .footers
            .iter_mut()
            .find(|s| &s.part_name == part)
            .map(|s| &mut s.blocks)
            .ok_or_else(|| EditError::StoryNotFound {
                story: story.clone(),
                step_index,
            }),
    }
}

/// Shared read path: resolve a [`StoryRef`] to its block slice.
///
/// Same not-found contract as [`story_blocks_mut`].
pub(crate) fn story_blocks<'a>(
    doc: &'a CanonDoc,
    story: &StoryRef,
    step_index: usize,
) -> Result<&'a [TrackedBlock], EditError> {
    match story {
        StoryRef::Body => Ok(&doc.blocks),
        StoryRef::Header(part) => doc
            .headers
            .iter()
            .find(|s| &s.part_name == part)
            .map(|s| s.blocks.as_slice())
            .ok_or_else(|| EditError::StoryNotFound {
                story: story.clone(),
                step_index,
            }),
        StoryRef::Footer(part) => doc
            .footers
            .iter()
            .find(|s| &s.part_name == part)
            .map(|s| s.blocks.as_slice())
            .ok_or_else(|| EditError::StoryNotFound {
                story: story.clone(),
                step_index,
            }),
    }
}

/// Find a block's index within a named story, reusing the body
/// [`find_block_index`] logic over the resolved slice.
///
/// Resolves the story first (so an unknown story id surfaces as
/// [`EditError::StoryNotFound`]); a present story missing the block surfaces
/// as [`EditError::StoryBlockNotFound`]. Both carry the story + step so the
/// `(StoryRef, NodeId)` pair that failed is always visible.
pub(crate) fn find_block_index_in_story(
    doc: &CanonDoc,
    story: &StoryRef,
    block_id: &NodeId,
    step_index: usize,
) -> Result<usize, EditError> {
    let blocks = story_blocks(doc, story, step_index)?;
    find_block_index(blocks, block_id).ok_or_else(|| EditError::StoryBlockNotFound {
        story: story.clone(),
        block_id: block_id.clone(),
        step_index,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        BlockNode, DocPart, HeaderFooterKind, HeaderStory, NodeId, OpaqueBlockNode, OpaqueKind,
        ProofRef, TrackedBlock, TrackingStatus,
    };

    fn empty_doc() -> CanonDoc {
        CanonDoc {
            id: NodeId::from("doc"),
            blocks: vec![],
            meta: crate::domain::DocMeta {
                schema_version: crate::domain::SCHEMA_VERSION_V0.to_string(),
                docx_fingerprint: crate::domain::DocFingerprint("test".to_string()),
                internal_ids_version: crate::domain::INTERNAL_IDS_VERSION_V0.to_string(),
            },
            headers: vec![],
            footers: vec![],
            footnotes: vec![],
            endnotes: vec![],
            comments: vec![],
            comments_extended: vec![],
            body_section_properties: None,
            body_section_property_change: None,
            compat_settings: crate::domain::CompatSettings::default(),
            even_and_odd_headers: None,
            document_background: None,
            document_protection: None,
        }
    }

    /// A minimal block carrying `id`. Block kind is irrelevant to the story
    /// resolver — it addresses by id over the resolved slice — so an opaque
    /// block keeps the fixture small and avoids the large `ParagraphNode`
    /// field list.
    fn id_block(id: &str) -> TrackedBlock {
        TrackedBlock {
            status: TrackingStatus::Normal,
            block: BlockNode::from(OpaqueBlockNode {
                id: NodeId::from(id),
                kind: OpaqueKind::Drawing,
                opaque_ref: format!("{id}:opaque"),
                proof_ref: ProofRef {
                    part: DocPart::DocumentXml,
                    block_id: NodeId::from(id),
                    docx_anchor: format!("{id}:opaque"),
                },
                range_marker: None,
            }),
            move_id: None,
            block_sdt_wrap: None,
        }
    }

    fn header(part_name: &str, block_id: &str) -> HeaderStory {
        HeaderStory {
            part_name: part_name.to_string(),
            kind: HeaderFooterKind::Default,
            blocks: vec![id_block(block_id)],
            content_hash: String::new(),
            synthesized: false,
        }
    }

    /// Two header stories whose first block shares the bare id "p_1" must resolve
    /// independently by part name — `Header("header2.xml")` must address the
    /// *second* story's block, not the first that happens to share the block id.
    /// (Header/footer stories reuse the body's `p_{n}` namespace, so the part
    /// name is the only thing disambiguating a colliding bare id.)
    #[test]
    fn story_ref_disambiguates_shared_block_id() {
        let mut doc = empty_doc();
        doc.headers = vec![header("header1.xml", "p_1"), header("header2.xml", "p_1")];

        // Same bare block id "p_1" lives in both header stories.
        let idx1 = find_block_index_in_story(
            &doc,
            &StoryRef::Header("header1.xml".to_string()),
            &NodeId::from("p_1"),
            0,
        )
        .expect("header1 has p_1");
        let idx2 = find_block_index_in_story(
            &doc,
            &StoryRef::Header("header2.xml".to_string()),
            &NodeId::from("p_1"),
            0,
        )
        .expect("header2 has p_1");
        assert_eq!(idx1, 0);
        assert_eq!(idx2, 0);

        // The resolved slices are the *distinct* stories: the resolver for the
        // second part must point at the second story.
        let blocks2 = story_blocks(&doc, &StoryRef::Header("header2.xml".to_string()), 0)
            .expect("header2 resolves");
        assert_eq!(blocks2.len(), 1);
        let ptr2 = blocks2.as_ptr();
        let direct2 = doc.headers[1].blocks.as_ptr();
        assert_eq!(
            ptr2, direct2,
            "Header(\"header2.xml\") must resolve to the second header story"
        );
    }

    /// An unknown story part is a hard error — never a silent fallback to the
    /// body or the first story.
    #[test]
    fn unknown_story_id_is_story_not_found() {
        let mut doc = empty_doc();
        doc.headers = vec![header("header1.xml", "p_1")];

        let err = find_block_index_in_story(
            &doc,
            &StoryRef::Header("header99.xml".to_string()),
            &NodeId::from("p_1"),
            7,
        )
        .expect_err("unknown header part must error");
        match err {
            EditError::StoryNotFound { story, step_index } => {
                assert_eq!(story, StoryRef::Header("header99.xml".to_string()));
                assert_eq!(step_index, 7);
            }
            other => panic!("expected StoryNotFound, got {other:?}"),
        }
    }

    /// A present story missing the block surfaces as StoryBlockNotFound with
    /// the failing `(StoryRef, NodeId)` pair.
    #[test]
    fn present_story_missing_block_is_story_block_not_found() {
        let mut doc = empty_doc();
        doc.headers = vec![header("header1.xml", "p_1")];

        let err = find_block_index_in_story(
            &doc,
            &StoryRef::Header("header1.xml".to_string()),
            &NodeId::from("p_999"),
            3,
        )
        .expect_err("missing block must error");
        match err {
            EditError::StoryBlockNotFound {
                story,
                block_id,
                step_index,
            } => {
                assert_eq!(story, StoryRef::Header("header1.xml".to_string()));
                assert_eq!(block_id, NodeId::from("p_999"));
                assert_eq!(step_index, 3);
            }
            other => panic!("expected StoryBlockNotFound, got {other:?}"),
        }
    }
}
