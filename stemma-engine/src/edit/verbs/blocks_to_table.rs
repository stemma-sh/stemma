//! `BlocksToTable` — convert a contiguous run of top-level paragraphs (e.g. a
//! bullet list) into a TABLE, as a single composed tracked change.
//!
//! ## The tracked representation (done honestly)
//!
//! There is no OOXML primitive for "a paragraph turned into a table row". So we
//! do NOT invent one. The conversion is composed from the two tracked-change
//! shapes the engine already emits, which gives clean projections for free:
//!
//! - the new table is staged as a tracked **insert** (the exact path
//!   `EditStep::InsertParagraphs` uses for a `BlockSpec::Table` — inserted rows
//!   get `<w:trPr><w:ins/></w:trPr>`, inserted cell runs get `<w:ins>`);
//! - the source paragraph range is marked as a tracked **delete** (the exact
//!   path `EditStep::DeleteBlockRange` uses — run `<w:del>`/`<w:delText>` plus
//!   paragraph-mark deletion).
//!
//! Therefore:
//! - **accept-all** => the table only (the source paragraphs are gone);
//! - **reject-all** => the original paragraphs verbatim (the table is gone).
//!
//! This rides the ONE materializer (Invariant M): this module only *builds the
//! input* — a [`TableBlockSpec`] and the resolved range — and the dispatch arm
//! in `edit/mod.rs` feeds it through `resolve_table_spec` + the existing insert
//! loop + `apply_delete_block_range`. No tracked primitive is added.
//!
//! ## Validate at the edge (CLAUDE.md "no silent fallbacks")
//!
//! - every block in `[from..=to]` must be a top-level **paragraph** with Normal
//!   tracking (else `BlocksToTableNonParagraph` / the standard editability
//!   errors). A non-paragraph in the range has no text to project into cells.
//! - **opaque preservation**: a source paragraph carrying an opaque inline
//!   (drawing/field/hyperlink/footnote/comment ref) is refused
//!   (`BlocksToTableOpaqueInline`). We project only *visible text* into cells, so
//!   an opaque would be silently lost on accept-all. We never destroy one.
//! - column count: a `header` fixes it at `header.len()` columns — each body row
//!   is split into at most that many cells and short rows padded with empty
//!   trailing cells (lossless; extra delimiters fold into the last cell). Without
//!   a header the first row's split count fixes the grid and every later row must
//!   match it (else `BlocksToTableSplitMismatch`). An empty header list or an
//!   empty delimiter is `BlocksToTableEmptySpec`.

use super::super::{
    BlockSpec, EditError, ParagraphBlockSpec, ParagraphContent, TableBlockSpec, TableCellSpec,
    TableRowSpec, find_block_index, paragraph_visible_text,
};
use crate::domain::{BlockNode, CanonDoc, InlineNode, NodeId, TrackingStatus};
use crate::edit::ContentFragment;
use crate::vocabulary::default_body_role_id;

/// The validated plan for one `BlocksToTable` step: the resolved source range
/// (inclusive block indices) and the table spec to insert. Built entirely from
/// already-validated inputs so the dispatch arm only has to stage the tracked
/// insert + delete.
pub(crate) struct BlocksToTablePlan {
    /// Inclusive start index of the source paragraph range in `doc.blocks`.
    pub start: usize,
    /// Inclusive end index of the source paragraph range in `doc.blocks`.
    pub end: usize,
    /// The table to stage as a tracked insert.
    pub table: TableBlockSpec,
}

/// Validate the step and build the [`BlocksToTablePlan`]. Pure: it does not
/// mutate `doc`. Every failure is an explicit, addressed `EditError`.
pub(crate) fn plan(
    doc: &CanonDoc,
    from_block_id: &NodeId,
    to_block_id: &NodeId,
    delimiter: &str,
    header: Option<&[String]>,
    step_index: usize,
) -> Result<BlocksToTablePlan, EditError> {
    if delimiter.is_empty() {
        return Err(EditError::BlocksToTableEmptySpec {
            reason: "delimiter is empty",
            step_index,
        });
    }
    if let Some(cells) = header
        && cells.is_empty()
    {
        return Err(EditError::BlocksToTableEmptySpec {
            reason: "header was supplied but has no cells",
            step_index,
        });
    }

    // Cell paragraphs need an explicit role that resolves against the document's
    // vocabulary (the table resolver runs `resolve_paragraph_spec`, which refuses
    // a role-less inserted paragraph). We use the document's most frequent plain
    // body role — never fabricate one. A document with no resolvable body role is
    // refused rather than guessed at.
    let cell_role = default_body_role_id(doc).ok_or(EditError::BlocksToTableEmptySpec {
        reason: "the document has no resolvable body paragraph role to use for table cells",
        step_index,
    })?;

    // Resolve the source range. Order the endpoints (a caller may pass them
    // reversed) the same way `DeleteBlockRange` does.
    let from_idx =
        find_block_index(&doc.blocks, from_block_id).ok_or_else(|| EditError::BlockNotFound {
            block_id: from_block_id.clone(),
            step_index,
        })?;
    let to_idx =
        find_block_index(&doc.blocks, to_block_id).ok_or_else(|| EditError::BlockNotFound {
            block_id: to_block_id.clone(),
            step_index,
        })?;
    let (start, end) = if from_idx <= to_idx {
        (from_idx, to_idx)
    } else {
        (to_idx, from_idx)
    };

    // Each source paragraph -> one body row. We first collect the raw visible
    // text per row (validating editability + opaque-freedom along the way), then
    // split once the column count is fixed.
    let mut body_rows: Vec<String> = Vec::with_capacity(end - start + 1);
    for tracked_block in &doc.blocks[start..=end] {
        // Block-level tracking must be Normal (no inserted/deleted source block).
        match &tracked_block.status {
            TrackingStatus::Normal => {}
            TrackingStatus::Inserted(_) => {
                return Err(EditError::BlockHasTrackedStatus {
                    block_id: block_id_of(&tracked_block.block).clone(),
                    status: "inserted",
                    step_index,
                });
            }
            TrackingStatus::Deleted(_) => {
                return Err(EditError::BlockHasTrackedStatus {
                    block_id: block_id_of(&tracked_block.block).clone(),
                    status: "deleted",
                    step_index,
                });
            }
            TrackingStatus::InsertedThenDeleted(_) => {
                return Err(EditError::BlockHasTrackedStatus {
                    block_id: block_id_of(&tracked_block.block).clone(),
                    status: "inserted_then_deleted",
                    step_index,
                });
            }
        }

        let para = match &tracked_block.block {
            BlockNode::Paragraph(p) => p,
            BlockNode::Table(_) => {
                return Err(EditError::BlocksToTableNonParagraph {
                    block_id: block_id_of(&tracked_block.block).clone(),
                    actual_kind: "table",
                    step_index,
                });
            }
            BlockNode::OpaqueBlock(_) => {
                return Err(EditError::BlocksToTableNonParagraph {
                    block_id: block_id_of(&tracked_block.block).clone(),
                    actual_kind: "opaque_block",
                    step_index,
                });
            }
        };

        // Segment-level tracking must be Normal.
        for segment in &para.segments {
            if segment.status != TrackingStatus::Normal {
                return Err(EditError::ParagraphContainsTrackedSegments {
                    block_id: para.id.clone(),
                    step_index,
                });
            }
        }

        // Opaque preservation: refuse rather than drop an opaque inline. Visible
        // text would carry across, but the opaque (drawing/field/hyperlink/...)
        // would vanish on accept-all.
        for segment in &para.segments {
            for inline in &segment.inlines {
                if matches!(inline, InlineNode::OpaqueInline(_)) {
                    return Err(EditError::BlocksToTableOpaqueInline {
                        block_id: para.id.clone(),
                        step_index,
                    });
                }
            }
        }

        // Defer the split until the column count is fixed (it depends on the
        // header, which may impose more columns than the delimiter yields).
        body_rows.push(paragraph_visible_text(para));
    }

    // Column count, and the rectangularity contract, depend on whether a header
    // was supplied:
    //
    // - **With a header**: the header fixes the grid at N = header.len() columns.
    //   Each body paragraph is split into AT MOST N cells (`splitn`), then the
    //   trailing cells are padded with empty strings to reach N. This is the
    //   defined "convert this list into an N-column table with these headers"
    //   semantics — lossless (no text is ever dropped; the delimiter merely may
    //   not appear in a given row), and the padding is an intentional,
    //   documented part of the contract, not a silent best-effort.
    //
    // - **Without a header**: there is no authoritative column count, so we adopt
    //   the FIRST row's split count and REQUIRE every subsequent row to split
    //   into exactly that many cells (else `BlocksToTableSplitMismatch`). A
    //   varying split with no header is genuinely ambiguous, so we refuse rather
    //   than guess.
    let columns = match header {
        Some(cells) => cells.len(),
        None => split_cells(&body_rows[0], delimiter).len(),
    };

    let mut split_rows: Vec<Vec<String>> = Vec::with_capacity(body_rows.len());
    for (row_offset, text) in body_rows.iter().enumerate() {
        let cells = if header.is_some() {
            // splitn(columns, …): at most `columns` cells; any extra delimiters
            // fold into the final cell (no text lost). Then pad to `columns`.
            let mut cells: Vec<String> = text
                .splitn(columns, delimiter)
                .map(|c| c.trim().to_string())
                .collect();
            while cells.len() < columns {
                cells.push(String::new());
            }
            cells
        } else {
            let cells = split_cells(text, delimiter);
            if cells.len() != columns {
                let para_id = block_id_of(&doc.blocks[start + row_offset].block).clone();
                return Err(EditError::BlocksToTableSplitMismatch {
                    block_id: para_id,
                    expected_columns: columns,
                    actual_columns: cells.len(),
                    text: text.clone(),
                    step_index,
                });
            }
            cells
        };
        split_rows.push(cells);
    }
    let body_rows = split_rows;

    // Build the spec: an optional header row, then one row per source paragraph.
    let mut rows: Vec<TableRowSpec> = Vec::with_capacity(body_rows.len() + 1);
    if let Some(header_cells) = header {
        rows.push(TableRowSpec {
            cells: header_cells
                .iter()
                .map(|text| text_cell(text, &cell_role))
                .collect(),
            is_header: true,
            height: None,
            height_rule: None,
        });
    }
    for cells in &body_rows {
        rows.push(TableRowSpec {
            cells: cells
                .iter()
                .map(|text| text_cell(text, &cell_role))
                .collect(),
            is_header: false,
            height: None,
            height_rule: None,
        });
    }

    Ok(BlocksToTablePlan {
        start,
        end,
        table: TableBlockSpec {
            rows,
            formatting: None,
        },
    })
}

/// Split a paragraph's visible text into trimmed cells on EVERY occurrence of
/// `delimiter` (the unbounded split, used for the no-header column-count
/// inference and the no-header rectangularity check). Cells are trimmed of
/// surrounding whitespace so a `"Term — definition"` line yields
/// `["Term", "definition"]`, not `["Term ", " definition"]`.
fn split_cells(text: &str, delimiter: &str) -> Vec<String> {
    text.split(delimiter)
        .map(|cell| cell.trim().to_string())
        .collect()
}

/// A single-paragraph table cell carrying one plain-text fragment. An empty
/// `text` still produces a paragraph (a blank cell), never an empty cell — the
/// table resolver requires every cell to hold at least one block.
fn text_cell(text: &str, role: &str) -> TableCellSpec {
    TableCellSpec {
        content: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
            role: Some(role.to_string()),
            content: ParagraphContent {
                fragments: vec![ContentFragment::Text(text.to_string())],
            },
            restart_numbering: false,
            list: None,
        })],
        merge_h: None,
        merge_v: None,
        formatting: None,
    }
}

/// Local copy of the module-private `block_id_of`, kept here so the verb stays
/// self-contained (it only needs the id for error reporting).
fn block_id_of(block: &BlockNode) -> &NodeId {
    match block {
        BlockNode::Paragraph(p) => &p.id,
        BlockNode::Table(t) => &t.id,
        BlockNode::OpaqueBlock(o) => &o.id,
    }
}

/// Stamp a freshly-resolved (Normal) table as a tracked INSERT all the way down:
/// every row's `tracking_status` becomes `Inserted` (serializer emits
/// `<w:trPr><w:ins/></w:trPr>`), every cell's `tracking_status` becomes
/// `Inserted` (`<w:cellIns>`), and every cell paragraph's segments + paragraph
/// mark become `Inserted` (cell runs wrapped in `<w:ins>`, the paragraph mark in
/// `<w:rPr><w:ins/></w:rPr>`). This is BOTH levels of tracking the task calls for
/// — row-level so Word removes the row on reject, and run-level so the change is
/// a faithful tracked insertion at every granularity (and any tracked-change
/// reader, not just Word's row-aware reject, projects it correctly).
///
/// `revision` is the shared insert revision for the whole table. All stamped
/// statuses carry it (a single tracked insertion, one author/date).
pub(crate) fn stamp_table_inserted(table: &mut crate::domain::TableNode, revision: TrackingStatus) {
    for row in &mut table.rows {
        row.tracking_status = Some(revision.clone());
        for cell in &mut row.cells {
            cell.tracking_status = Some(revision.clone());
            stamp_cell_blocks_inserted(&mut cell.blocks, &revision);
        }
    }
}

fn stamp_cell_blocks_inserted(blocks: &mut [BlockNode], revision: &TrackingStatus) {
    for block in blocks {
        match block {
            BlockNode::Paragraph(p) => {
                for seg in &mut p.segments {
                    seg.status = revision.clone();
                }
                p.para_mark_status = Some(revision.clone());
            }
            BlockNode::Table(t) => {
                for row in &mut t.rows {
                    row.tracking_status = Some(revision.clone());
                    for cell in &mut row.cells {
                        cell.tracking_status = Some(revision.clone());
                        stamp_cell_blocks_inserted(&mut cell.blocks, revision);
                    }
                }
            }
            BlockNode::OpaqueBlock(_) => {}
        }
    }
}
