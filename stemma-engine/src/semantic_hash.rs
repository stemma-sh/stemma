//! The **block staleness guard**: a content hash over a block's character
//! stream — each character paired with its tracked-status class — and its
//! preserved-anchor inventory/order.
//!
//! This hash is the single staleness mechanism of the write path. A read surfaces it as
//! [`crate::view::BlockView::guard`]; a write op carries the same value as its
//! `guard`. If the block changed between read and write, the hash differs and the
//! op fails loud (`StaleEdit`). The precondition and the staleness check are the
//! same object.
//!
//! # Guard schemes (versioned)
//!
//! The guard string carries its scheme so a stored guard validates under the
//! formula it was minted with ([`check_block_guard`] dispatches on the prefix):
//!
//! - **v2 (current; `"v2:"` prefix)** hashes the **(visible char,
//!   status-class) stream** of ALL segments — Normal, Inserted AND Deleted —
//!   plus the class-tagged anchor inventory. The reader of the markup sees
//!   tracked text *as tracked*, so the guard answers "has what I was looking
//!   at changed?" for tracked meaning, not just bytes: stacking a deletion
//!   over inserted text moves the guard, resolving a tombstone moves the
//!   guard, and every such transition also shifts span-handle ordinals — the
//!   stale-handle class the guard is the designated gate against. Classes are
//!   status-only (`n`/`i`/`d`), never author/date: re-attribution does not
//!   move the guard.
//! - **v1 (legacy; bare hex, no prefix)** hashed only the text of non-Deleted
//!   segments, status-blind. It is still computed for validation of stored v1
//!   guards (`block_semantic_hash_for_block`), with its documented blind
//!   spots: a status change over identical bytes, and accepting a tombstone,
//!   did not move a v1 guard.
//!
//! Both schemes are insensitive to segment boundaries and node ids
//! (same-class adjacent runs coalesce before hashing) and to formatting.
//!
//! # Coverage (v2)
//!
//! Over ALL segments, in document order:
//! - every (visible char, status-class) run — adjacent same-class text
//!   coalesced, so segmentation topology never matters;
//! - every preserved-anchor atom — `(status-class, opaque_id, opaque_kind)`
//!   for opaque inlines, the synthetic `hard_break` atom for hard breaks.
//!
//! So the guard pins exactly what a text edit reasons about: the tracked
//! reading of the block AND the identity + order of the anchors that must
//! survive a rewrite (principle 5, opaque preservation).
//!
//! # What it deliberately EXCLUDES — and why that is safe
//!
//! The guard does **not** hash per-run/paragraph formatting (bold, color, size,
//! alignment, indentation), style ids, comments, or revision metadata — this
//! also covers a run's preserved rPr remainder (`StyleProps::preserved`,
//! unmodeled formatting children like w:eastAsianLayout captured verbatim so
//! they survive re-serialization) and a paragraph's preserved pPr remainder
//! (`ParagraphNode::preserved_ppr`, unmodeled children like
//! w:suppressLineNumbers captured the same way): both are formatting, so the
//! same exclusion and argument below apply to them. Excluding formatting is a
//! deliberate open item, not an oversight — and it is safe given the current
//! verb set, by this argument:
//!
//! The only verbs that mutate formatting are `SetRunFormatting` /
//! `SetParagraphFormatting` (v4 `set_format` / `set_para_format`). Each carries
//! its OWN per-target precondition and emits a *tracked* `w:rPrChange` /
//! `w:pPrChange` — it never silently rewrites formatting in place. So a
//! concurrent formatting-only change cannot corrupt a text edit guarded by this
//! hash: it lands as its own reviewable tracked change on a disjoint axis
//! (formatting), while the text-edit guard still pins the text + anchors it
//! actually depends on. A text edit and a formatting edit on the same block
//! compose as two independent tracked changes; neither's correctness rests on the
//! other's axis being in the guard.
//!
//! If a future verb were to mutate formatting *untracked and in place* (silently),
//! this exclusion would no longer be safe and the guard's coverage would have to
//! grow to include the formatting axis. That is the contract this comment pins:
//! the exclusion is justified *only* under "formatting mutations are tracked and
//! self-guarded", not asserted unconditionally.

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::domain::{
    BlockNode, FullDocBlock, InlineChange, InlineChangeSegmentType, InlineNode, OpaqueKind,
    OpaqueSegmentKind, ParagraphNode, TableNode, TrackingStatus,
};

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
    /// v2: a coalesced run of visible characters sharing one tracked-status
    /// class (`"n"`/`"i"`/`"d"`).
    ClassedText {
        class: &'static str,
        text: String,
    },
    /// v2: a preserved anchor with the status class of the segment it sits in.
    ClassedOpaque {
        class: &'static str,
        opaque_id: String,
        opaque_kind: String,
    },
    /// One cell of a table, identified by its row-major position, carrying the
    /// content hash of each block inside the cell. Folding this into the table
    /// guard makes a structure-preserving `SetCellText` move the guard — the
    /// visible reading of a Table block includes its cells' text.
    TableCell {
        row: usize,
        col: usize,
        block_hashes: Vec<String>,
    },
}

fn hash_atoms(atoms: Vec<HashAtom>) -> String {
    let json = serde_json::to_vec(&atoms).expect("semantic hash atoms must serialize");
    let digest = Sha256::digest(json);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
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

fn inline_opaque_kind_name(kind: &OpaqueKind) -> String {
    match kind {
        OpaqueKind::Drawing => "drawing".to_string(),
        OpaqueKind::SmartArt => "smart_art".to_string(),
        OpaqueKind::Sdt => "sdt".to_string(),
        OpaqueKind::Field(_) => "field".to_string(),
        OpaqueKind::OmmlBlock | OpaqueKind::OmmlInline => "omml".to_string(),
        OpaqueKind::Ruby => "ruby".to_string(),
        OpaqueKind::Hyperlink(_) => "hyperlink".to_string(),
        OpaqueKind::CommentReference(_) => "comment_reference".to_string(),
        OpaqueKind::FootnoteReference(_) => "footnote_reference".to_string(),
        OpaqueKind::EndnoteReference(_) => "endnote_reference".to_string(),
        OpaqueKind::SmartTag => "smart_tag".to_string(),
        OpaqueKind::Sym(_) => "sym".to_string(),
        OpaqueKind::Ptab => "ptab".to_string(),
        OpaqueKind::CustomXml => "custom_xml".to_string(),
        OpaqueKind::Unknown(name) => format!("unknown:{name}"),
        // Never appears inline (body-level quarantine only); defensive label.
        OpaqueKind::QuarantinedNestedTracking => "quarantined_nested_tracked_changes".to_string(),
    }
}

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
            } => {
                if *segment_type != InlineChangeSegmentType::Delete {
                    atoms.push(HashAtom::Opaque {
                        opaque_id: opaque_id.clone(),
                        opaque_kind: opaque_kind_name(kind),
                    });
                }
            }
        }
    }
    hash_atoms(atoms)
}

/// Scheme prefix of the v2 guard formula. v1 guards are bare hex.
pub const GUARD_SCHEME_V2_PREFIX: &str = "v2:";

/// Mint the current (v2) block guard: `"v2:" + sha256` of the block's
/// (visible char, status-class) stream plus class-tagged anchor inventory.
/// This is what the read view surfaces as [`crate::view::BlockView::guard`].
pub fn block_guard(block: &BlockNode) -> String {
    format!("{GUARD_SCHEME_V2_PREFIX}{}", block_guard_v2_hash(block))
}

/// Validate a caller-provided guard against a block, under the scheme the
/// guard was minted with: `"v2:…"` validates with the v2 formula; a bare hex
/// string is a legacy v1 guard and validates with the v1 formula it was
/// minted under (stored guards keep working; the v1 blind spots apply only to
/// those callers). On mismatch, returns the block's CURRENT guard computed
/// under the SAME scheme, so `StaleEdit` messages compare like with like.
pub fn check_block_guard(block: &BlockNode, provided: &str) -> Result<(), String> {
    let actual = if provided.starts_with(GUARD_SCHEME_V2_PREFIX) {
        block_guard(block)
    } else {
        block_semantic_hash_for_block(block)
    };
    if actual == provided {
        Ok(())
    } else {
        Err(actual)
    }
}

fn status_class(status: &TrackingStatus) -> &'static str {
    match status {
        TrackingStatus::Normal => "n",
        TrackingStatus::Inserted(_) => "i",
        TrackingStatus::Deleted(_) => "d",
        // The stacked state is its own class: B stacking a deletion over A's
        // insertion changes "i" chars to "s" chars, which is exactly the
        // transition the v2 guard exists to catch.
        TrackingStatus::InsertedThenDeleted(_) => "s",
    }
}

fn block_guard_v2_hash(block: &BlockNode) -> String {
    match block {
        BlockNode::Paragraph(paragraph) => paragraph_guard_v2_hash(paragraph),
        BlockNode::Table(table) => table_guard_v2_hash(table),
        BlockNode::OpaqueBlock(block) => hash_atoms(vec![HashAtom::Opaque {
            opaque_id: block.id.0.to_string(),
            opaque_kind: format!("opaque:{}", block.opaque_ref),
        }]),
    }
}

/// The v2 paragraph guard: the (char, status-class) stream over ALL segments.
/// Adjacent same-class text coalesces into one atom, so the guard is
/// insensitive to segment boundaries and node ids by construction — two
/// adjacent insertions by different authors hash identically to one merged
/// insertion (classes are status-only; attribution never moves the guard).
fn paragraph_guard_v2_hash(paragraph: &ParagraphNode) -> String {
    let mut atoms: Vec<HashAtom> = Vec::new();
    let mut run_class: &'static str = "n";
    let mut run_text = String::new();
    let flush = |atoms: &mut Vec<HashAtom>, class: &'static str, text: &mut String| {
        if !text.is_empty() {
            atoms.push(HashAtom::ClassedText {
                class,
                text: std::mem::take(text),
            });
        }
    };
    for segment in &paragraph.segments {
        let class = status_class(&segment.status);
        if class != run_class {
            flush(&mut atoms, run_class, &mut run_text);
            run_class = class;
        }
        for inline in &segment.inlines {
            match inline {
                InlineNode::Text(text) => run_text.push_str(&text.text),
                InlineNode::OpaqueInline(opaque) => {
                    flush(&mut atoms, run_class, &mut run_text);
                    atoms.push(HashAtom::ClassedOpaque {
                        class,
                        opaque_id: opaque.id.0.to_string(),
                        opaque_kind: inline_opaque_kind_name(&opaque.kind),
                    });
                }
                InlineNode::HardBreak(break_node) => {
                    flush(&mut atoms, run_class, &mut run_text);
                    atoms.push(HashAtom::ClassedOpaque {
                        class,
                        opaque_id: break_node.id.0.to_string(),
                        opaque_kind: "hard_break".to_string(),
                    });
                }
                InlineNode::Decoration(_)
                | InlineNode::CommentRangeStart { .. }
                | InlineNode::CommentRangeEnd { .. }
                | InlineNode::CommentReference { .. } => {}
            }
        }
    }
    flush(&mut atoms, run_class, &mut run_text);
    hash_atoms(atoms)
}

/// The v2 table guard: structure hash + per-cell v2 block guards, mirroring
/// the v1 table fold but recursing with the v2 formula.
fn table_guard_v2_hash(table: &TableNode) -> String {
    let mut atoms = vec![HashAtom::Text {
        text: format!("table:{}", table.structure_hash),
    }];
    for (row_idx, row) in table.rows.iter().enumerate() {
        for (col_idx, cell) in row.cells.iter().enumerate() {
            let block_hashes = cell.blocks.iter().map(block_guard_v2_hash).collect();
            atoms.push(HashAtom::TableCell {
                row: row_idx,
                col: col_idx,
                block_hashes,
            });
        }
    }
    hash_atoms(atoms)
}

/// The LEGACY (v1) guard formula: visible text of non-Deleted segments +
/// anchor inventory, status-blind. Kept for validating stored v1 guards
/// (see [`check_block_guard`]); new guards are minted by [`block_guard`].
pub fn block_semantic_hash_for_paragraph(paragraph: &ParagraphNode) -> String {
    let mut atoms = Vec::new();
    for segment in &paragraph.segments {
        if matches!(
            segment.status,
            TrackingStatus::Deleted(_) | TrackingStatus::InsertedThenDeleted(_)
        ) {
            // v1 is the FROZEN legacy formula: it skipped pending-deleted
            // text, and the stacked state is pending-deleted. Total so v1
            // guards stay checkable; new guards are v2.
            continue;
        }
        for inline in &segment.inlines {
            match inline {
                InlineNode::Text(text) => atoms.push(HashAtom::Text {
                    text: text.text.clone(),
                }),
                InlineNode::OpaqueInline(opaque) => atoms.push(HashAtom::Opaque {
                    opaque_id: opaque.id.0.to_string(),
                    opaque_kind: inline_opaque_kind_name(&opaque.kind),
                }),
                InlineNode::HardBreak(break_node) => atoms.push(HashAtom::Opaque {
                    opaque_id: break_node.id.0.to_string(),
                    opaque_kind: "hard_break".to_string(),
                }),
                InlineNode::Decoration(_)
                | InlineNode::CommentRangeStart { .. }
                | InlineNode::CommentRangeEnd { .. }
                | InlineNode::CommentReference { .. } => {}
            }
        }
    }
    hash_atoms(atoms)
}

/// Hash a Table block's guard: its structure hash PLUS the visible content of
/// every cell, in row-major order.
///
/// The structure hash (`compute_table_structure_hash`) pins the grid shape (row/
/// cell counts, grid offsets, gridSpan, vMerge) but deliberately excludes cell
/// text. A `SetCellText` edit is structure-preserving, so on its own the
/// structure hash cannot detect it. Because the visible reading of a Table block
/// includes its cells' text, the guard must also fold each cell's content — built
/// by recursing into `block_semantic_hash_for_block` for each block inside the
/// cell, exactly the same way paragraph content is hashed elsewhere. This keeps
/// the structure hash intact (still folded in first) and is deterministic and
/// order-stable (row-major over cells, then block order within a cell).
fn block_semantic_hash_for_table(table: &TableNode) -> String {
    let mut atoms = vec![HashAtom::Text {
        text: format!("table:{}", table.structure_hash),
    }];
    for (row_idx, row) in table.rows.iter().enumerate() {
        for (col_idx, cell) in row.cells.iter().enumerate() {
            let block_hashes = cell
                .blocks
                .iter()
                .map(block_semantic_hash_for_block)
                .collect();
            atoms.push(HashAtom::TableCell {
                row: row_idx,
                col: col_idx,
                block_hashes,
            });
        }
    }
    hash_atoms(atoms)
}

pub fn block_semantic_hash_for_block(block: &BlockNode) -> String {
    match block {
        BlockNode::Paragraph(paragraph) => block_semantic_hash_for_paragraph(paragraph),
        BlockNode::Table(table) => block_semantic_hash_for_table(table),
        BlockNode::OpaqueBlock(block) => hash_atoms(vec![HashAtom::Opaque {
            opaque_id: block.id.0.to_string(),
            opaque_kind: format!("opaque:{}", block.opaque_ref),
        }]),
    }
}
