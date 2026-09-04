//! Guards for comparison-owned read projections.

use serde::Serialize;
use sha2::{Digest, Sha256};
use stemma::domain::{InlineChange, InlineChangeSegmentType, OpaqueSegmentKind};

use crate::FullDocBlock;

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum HashAtom {
    Text {
        text: String,
    },
    Opaque {
        opaque_id: String,
        opaque_kind: String,
    },
}

/// Hash the accepted reading of one comparison-projection block.
pub fn block_semantic_hash_for_full_doc_block(block: &FullDocBlock) -> String {
    let mut atoms = Vec::new();
    for segment in &block.segments {
        match segment {
            InlineChange::Deleted { .. } => {}
            InlineChange::Unchanged { text, .. } | InlineChange::Inserted { text, .. } => {
                atoms.push(HashAtom::Text { text: text.clone() });
            }
            InlineChange::Opaque {
                segment_type,
                kind,
                opaque_id,
                ..
            } if *segment_type != InlineChangeSegmentType::Delete => {
                atoms.push(HashAtom::Opaque {
                    opaque_id: opaque_id.clone(),
                    opaque_kind: opaque_kind_name(kind),
                });
            }
            InlineChange::Opaque { .. } => {}
        }
    }
    let bytes = serde_json::to_vec(&atoms).expect("comparison hash atoms must serialize");
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn opaque_kind_name(kind: &OpaqueSegmentKind) -> String {
    match kind {
        OpaqueSegmentKind::Drawing => "drawing".to_string(),
        OpaqueSegmentKind::Omml => "omml".to_string(),
        OpaqueSegmentKind::Hyperlink => "hyperlink".to_string(),
        OpaqueSegmentKind::Field => "field".to_string(),
        OpaqueSegmentKind::Sdt => "sdt".to_string(),
        OpaqueSegmentKind::Ruby => "ruby".to_string(),
        OpaqueSegmentKind::SmartArt => "smart_art".to_string(),
        OpaqueSegmentKind::CommentReference => "comment_reference".to_string(),
        OpaqueSegmentKind::FootnoteReference => "footnote_reference".to_string(),
        OpaqueSegmentKind::EndnoteReference => "endnote_reference".to_string(),
        OpaqueSegmentKind::SmartTag => "smart_tag".to_string(),
        OpaqueSegmentKind::Sym => "sym".to_string(),
        OpaqueSegmentKind::Ptab => "ptab".to_string(),
        OpaqueSegmentKind::CustomXml => "custom_xml".to_string(),
        OpaqueSegmentKind::Unknown(name) => format!("unknown:{name}"),
    }
}
