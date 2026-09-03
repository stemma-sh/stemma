//! Comparison-owned plans and read projections.
//!
//! These types describe inferred relationships between two independently
//! authored documents. They deliberately live downstream of `stemma`: the
//! engine owns Word document state and native edit carriers, not the claim that
//! one document evolved into another.

use serde::{Deserialize, Serialize};
use stemma::domain::{
    Alignment, BlockNode, DocFingerprint, HeaderFooterKind, IStr, Indentation, InlineChange,
    NodeId, ParagraphBorders, ParagraphSpacing, SectionProperties, TrackingStatus,
};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub(crate) struct BlockProvenance {
    pub base_block_id: Option<NodeId>,
    pub target_block_id: Option<NodeId>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DocumentDiff {
    pub base_fingerprint: DocFingerprint,
    pub target_fingerprint: DocFingerprint,
    pub changes: Vec<DiffChange>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
pub enum DiffChange {
    BlockDeleted {
        block_id: NodeId,
        old_text: String,
        old_block: BlockNode,
        move_id: Option<String>,
    },
    BlockInserted {
        after_block_id: Option<NodeId>,
        block: BlockNode,
        move_id: Option<String>,
    },
    BlockModified {
        block_id: NodeId,
        old_text: String,
        new_text: String,
        inline_changes: Vec<InlineChange>,
        old_block: BlockNode,
        new_block: BlockNode,
        para_split: bool,
    },
    TableStructureChanged {
        table_id: NodeId,
        target_table_id: NodeId,
        old_hash: String,
        new_hash: String,
        old_text: String,
        new_text: String,
        table_diff: Option<Box<stemma::domain::TableDiffResult>>,
    },
    TableCellsModified {
        table_id: NodeId,
        target_table_id: NodeId,
        cell_changes: Vec<stemma::domain::TableCellChange>,
        old_text: String,
        new_text: String,
    },
    HeaderModified {
        kind: HeaderFooterKind,
        base_part_name: String,
        target_part_name: String,
        old_hash: String,
        new_hash: String,
        block_changes: Vec<DiffChange>,
    },
    HeaderDeleted {
        kind: HeaderFooterKind,
        part_name: String,
        content_hash: String,
        blocks: Vec<BlockNode>,
    },
    HeaderInserted {
        kind: HeaderFooterKind,
        part_name: String,
        content_hash: String,
        blocks: Vec<BlockNode>,
    },
    FooterModified {
        kind: HeaderFooterKind,
        base_part_name: String,
        target_part_name: String,
        old_hash: String,
        new_hash: String,
        block_changes: Vec<DiffChange>,
    },
    FooterDeleted {
        kind: HeaderFooterKind,
        part_name: String,
        content_hash: String,
        blocks: Vec<BlockNode>,
    },
    FooterInserted {
        kind: HeaderFooterKind,
        part_name: String,
        content_hash: String,
        blocks: Vec<BlockNode>,
    },
    FootnoteModified {
        id: String,
        old_hash: String,
        new_hash: String,
        block_changes: Vec<DiffChange>,
    },
    FootnoteDeleted {
        id: String,
        content_hash: String,
        blocks: Vec<BlockNode>,
    },
    FootnoteInserted {
        id: String,
        content_hash: String,
        blocks: Vec<BlockNode>,
    },
    EndnoteModified {
        id: String,
        old_hash: String,
        new_hash: String,
        block_changes: Vec<DiffChange>,
    },
    EndnoteDeleted {
        id: String,
        content_hash: String,
        blocks: Vec<BlockNode>,
    },
    EndnoteInserted {
        id: String,
        content_hash: String,
        blocks: Vec<BlockNode>,
    },
    CommentModified {
        id: String,
        old_hash: String,
        new_hash: String,
        block_changes: Vec<DiffChange>,
    },
    CommentDeleted {
        id: String,
        content_hash: String,
        blocks: Vec<BlockNode>,
    },
    CommentInserted {
        id: String,
        content_hash: String,
        blocks: Vec<BlockNode>,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum BlockType {
    Paragraph,
    Heading,
    Table,
    Opaque,
}

impl BlockType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Paragraph => "paragraph",
            Self::Heading => "heading",
            Self::Table => "table",
            Self::Opaque => "opaque",
        }
    }
}

impl std::fmt::Display for BlockType {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum ChangeType {
    Unchanged,
    Modified,
    Inserted,
    Deleted,
}

impl ChangeType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unchanged => "unchanged",
            Self::Modified => "modified",
            Self::Inserted => "inserted",
            Self::Deleted => "deleted",
        }
    }
}

impl std::fmt::Display for ChangeType {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum ImageMetadataChange {
    Size,
    Cropping,
    AltText,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum MoveDirection {
    From,
    To,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum StructuralChange {
    Join { into_block_id: NodeId },
    Split { from_block_id: NodeId },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct FullDocBlock {
    pub block_id: NodeId,
    pub doc1_block_id: Option<NodeId>,
    pub doc2_block_id: Option<NodeId>,
    pub block_type: BlockType,
    pub heading_level: Option<u8>,
    pub style_id: Option<IStr>,
    pub change_type: ChangeType,
    pub align: Option<Alignment>,
    pub indent: Option<Indentation>,
    pub spacing: Option<ParagraphSpacing>,
    pub borders: Option<ParagraphBorders>,
    pub tab_stops: Vec<stemma::TabStopDef>,
    pub numbering_text: Option<String>,
    pub numbering_ilvl: Option<u32>,
    pub numbering_num_id: Option<u32>,
    pub segments: Vec<InlineChange>,
    pub table_diff: Option<stemma::domain::TableDiffResult>,
    pub content_types: Vec<String>,
    pub equation_xmls: Vec<String>,
    pub equation_doc1_count: usize,
    pub image_data_uris: Vec<String>,
    pub image_doc1_count: usize,
    pub image_metadata_changes: Vec<ImageMetadataChange>,
    pub move_id: Option<String>,
    pub move_direction: Option<MoveDirection>,
    pub structural_change: Option<StructuralChange>,
    pub border_group_id: Option<String>,
    pub paragraph_mark_status: Option<TrackingStatus>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct FullDocViewResult {
    pub blocks: Vec<FullDocBlock>,
    pub footnotes: Vec<StoryPayload>,
    pub endnotes: Vec<StoryPayload>,
    pub comments: Vec<CommentPayload>,
    pub headers: Vec<HeaderFooterPayload>,
    pub footers: Vec<HeaderFooterPayload>,
    pub body_section_properties: Option<SectionProperties>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct StoryPayload {
    pub id: String,
    pub segments: Vec<InlineChange>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct HeaderFooterParagraph {
    pub align: Option<Alignment>,
    pub tab_stops: Vec<stemma::TabStopDef>,
    pub segments: Vec<InlineChange>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct HeaderFooterPayload {
    pub kind: String,
    pub paragraphs: Vec<HeaderFooterParagraph>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CommentPayload {
    pub id: String,
    pub author: Option<String>,
    pub date: Option<String>,
    pub segments: Vec<InlineChange>,
    pub resolved: bool,
    pub parent_para_id: Option<String>,
}
