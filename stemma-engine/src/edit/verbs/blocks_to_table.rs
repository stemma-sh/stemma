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
//! - **cell content is the source content**: each cell is cut from its source
//!   paragraph's own runs, so run formatting, line breaks and footnote/endnote
//!   references come along. A tracked conversion leaves the source paragraphs
//!   in place as a deletion, so each carried note reference gets its own copy
//!   of the note (a new id, the same content): Word repairs a file in which one
//!   note is referenced twice, even once deleted and once inserted. Accept-all
//!   keeps the copy, reject-all the original; the reference graph drops the
//!   other. A direct conversion moves the references.
//! - **opaque preservation**: any other opaque inline (drawing/field/hyperlink/
//!   comment reference) is refused (`BlocksToTableOpaqueInline`): copying one
//!   needs its own identity rules, and dropping one would lose it on accept-all.
//!   Bookmarks and comment ranges are not carried into cells.
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
use crate::domain::{
    BlockNode, CanonDoc, InlineNode, NodeId, OpaqueKind, TrackedSegment, TrackingStatus,
};
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
    /// The table to stage as a tracked insert. Its body cells hold their text
    /// only; [`carry_cell_content`] replaces it with the source content.
    pub table: TableBlockSpec,
    /// Each body row's cells as the source inlines they were cut from.
    body_cells: Vec<Vec<Vec<InlineNode>>>,
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
    let mut body_rows: Vec<Vec<InlineNode>> = Vec::with_capacity(end - start + 1);
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

        // The content a cell carries: text, breaks and note references. Any
        // other opaque is refused rather than dropped (it would vanish on
        // accept-all).
        let mut carried = Vec::new();
        for inline in para.segments.iter().flat_map(|segment| &segment.inlines) {
            match inline {
                InlineNode::Text(_) | InlineNode::HardBreak(_) => carried.push(inline.clone()),
                InlineNode::OpaqueInline(opaque)
                    if matches!(
                        opaque.kind,
                        OpaqueKind::FootnoteReference(_) | OpaqueKind::EndnoteReference(_)
                    ) =>
                {
                    carried.push(inline.clone());
                }
                InlineNode::OpaqueInline(_) => {
                    return Err(EditError::BlocksToTableOpaqueInline {
                        block_id: para.id.clone(),
                        step_index,
                    });
                }
                InlineNode::Decoration(_)
                | InlineNode::CommentRangeStart { .. }
                | InlineNode::CommentRangeEnd { .. } => {}
            }
        }
        debug_assert_eq!(inline_text(&carried), paragraph_visible_text(para));

        // Defer the split until the column count is fixed (it depends on the
        // header, which may impose more columns than the delimiter yields).
        body_rows.push(carried);
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
        None => cell_ranges(&inline_text(&body_rows[0]), delimiter, None).len(),
    };

    let mut body_cells: Vec<Vec<Vec<InlineNode>>> = Vec::with_capacity(body_rows.len());
    for (row_offset, inlines) in body_rows.iter().enumerate() {
        let text = inline_text(inlines);
        let ranges = if header.is_some() {
            // At most `columns` cells; any extra delimiters fold into the final
            // cell (no text lost). Short rows are padded below.
            cell_ranges(&text, delimiter, Some(columns))
        } else {
            let ranges = cell_ranges(&text, delimiter, None);
            if ranges.len() != columns {
                let para_id = block_id_of(&doc.blocks[start + row_offset].block).clone();
                return Err(EditError::BlocksToTableSplitMismatch {
                    block_id: para_id,
                    expected_columns: columns,
                    actual_columns: ranges.len(),
                    text,
                    step_index,
                });
            }
            ranges
        };
        let mut cells: Vec<Vec<InlineNode>> = ranges
            .iter()
            .map(|range| slice_inlines(inlines, &text, range))
            .collect();
        // A break or note reference inside a delimiter belongs to no cell.
        let anchored = |inlines: &[InlineNode]| {
            inlines
                .iter()
                .filter(|inline| !matches!(inline, InlineNode::Text(_)))
                .count()
        };
        if cells.iter().map(|cell| anchored(cell)).sum::<usize>() != anchored(inlines) {
            return Err(EditError::UnsupportedParagraphStructure {
                block_id: block_id_of(&doc.blocks[start + row_offset].block).clone(),
                step_index,
                reason: format!(
                    "a line break or note reference sits inside the delimiter {delimiter:?}, \
                     so it belongs to no cell; choose a delimiter that does not contain it"
                ),
            });
        }
        while cells.len() < columns {
            cells.push(Vec::new());
        }
        body_cells.push(cells);
    }

    // Build the spec: an optional header row, then one row per source paragraph.
    let mut rows: Vec<TableRowSpec> = Vec::with_capacity(body_rows.len() + 1);
    if let Some(header_cells) = header {
        rows.push(TableRowSpec {
            cant_split: None,
            cells: header_cells
                .iter()
                .map(|text| text_cell(text, &cell_role))
                .collect(),
            is_header: true,
            height: None,
            height_rule: None,
        });
    }
    for cells in &body_cells {
        rows.push(TableRowSpec {
            cant_split: None,
            cells: cells
                .iter()
                .map(|inlines| text_cell(&inline_text(inlines), &cell_role))
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
        body_cells,
    })
}

/// Replace each body cell's text with the source content it was cut from. The
/// resolved table holds one paragraph per cell (built from the cell's text by
/// [`plan`]); the carried inlines keep their formatting and get ids of the
/// cell paragraph's own.
pub(crate) fn carry_cell_content(table: &mut BlockNode, plan: &BlocksToTablePlan) {
    let BlockNode::Table(table) = table else {
        unreachable!("blocks_to_table resolves to a table");
    };
    let header_rows = table.rows.len() - plan.body_cells.len();
    for (row, cells) in table.rows[header_rows..].iter_mut().zip(&plan.body_cells) {
        assert_eq!(row.cells.len(), cells.len(), "one cell per planned column");
        for (cell, inlines) in row.cells.iter_mut().zip(cells) {
            let [BlockNode::Paragraph(paragraph)] = cell.blocks.as_mut_slice() else {
                unreachable!("a planned cell holds one paragraph");
            };
            assert_eq!(
                paragraph_visible_text(paragraph),
                inline_text(inlines),
                "the cell was built from the text of the inlines it carries"
            );
            let mut inlines = inlines.clone();
            for (index, inline) in inlines.iter_mut().enumerate() {
                let id = NodeId::from(format!("{}_carried_{index}", paragraph.id.0));
                match inline {
                    InlineNode::Text(text) => text.id = id,
                    InlineNode::HardBreak(hard_break) => hard_break.id = id,
                    InlineNode::OpaqueInline(opaque) => opaque.id = id,
                    InlineNode::Decoration(_)
                    | InlineNode::CommentRangeStart { .. }
                    | InlineNode::CommentRangeEnd { .. } => {
                        unreachable!("only text, breaks and note references are carried")
                    }
                }
            }
            paragraph.segments = if inlines.is_empty() {
                Vec::new()
            } else {
                vec![TrackedSegment {
                    status: TrackingStatus::Normal,
                    inlines,
                }]
            };
        }
    }
}

/// The text of `inlines`, as [`paragraph_visible_text`] reads a paragraph.
fn inline_text(inlines: &[InlineNode]) -> String {
    inlines
        .iter()
        .filter_map(|inline| match inline {
            InlineNode::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect()
}

/// A cell's place in its row's text (byte offsets): `outer` runs from the end
/// of the previous delimiter to the start of the next, `text` is `outer`
/// without surrounding whitespace, so a `"Term — definition"` line yields
/// `"Term"` and `"definition"`.
struct CellRange {
    outer: (usize, usize),
    text: (usize, usize),
}

/// The cells `text` splits into on `delimiter`: on every occurrence, or with
/// `limit`, into at most that many cells (later delimiters stay in the last).
fn cell_ranges(text: &str, delimiter: &str, limit: Option<usize>) -> Vec<CellRange> {
    let mut bounds = Vec::new();
    let mut from = 0;
    for (at, _) in text.match_indices(delimiter) {
        if limit.is_some_and(|limit| bounds.len() + 1 == limit) {
            break;
        }
        bounds.push((from, at));
        from = at + delimiter.len();
    }
    bounds.push((from, text.len()));
    bounds
        .into_iter()
        .map(|(start, end)| {
            let cell = &text[start..end];
            let text_start = start + (cell.len() - cell.trim_start().len());
            let text_end = (start + cell.trim_end().len()).max(text_start);
            CellRange {
                outer: (start, end),
                text: (text_start, text_end),
            }
        })
        .collect()
}

/// The part of a row's inlines that makes up one cell: the text inside the
/// cell's trimmed range, cut at its byte offsets with the formatting it has,
/// and every break or note reference positioned inside the cell.
fn slice_inlines(inlines: &[InlineNode], text: &str, range: &CellRange) -> Vec<InlineNode> {
    let mut cell = Vec::new();
    let mut at = 0;
    for inline in inlines {
        match inline {
            InlineNode::Text(node) => {
                let (start, end) = (at, at + node.text.len());
                at = end;
                let from = start.max(range.text.0);
                let to = end.min(range.text.1);
                if from < to {
                    let mut piece = node.clone();
                    piece.text = text[from..to].to_string();
                    cell.push(InlineNode::Text(piece));
                }
            }
            _ if range.outer.0 <= at && at <= range.outer.1 => cell.push(inline.clone()),
            _ => {}
        }
    }
    cell
}

/// A single-paragraph table cell carrying one plain-text fragment. An empty
/// `text` still produces a paragraph (a blank cell), never an empty cell — the
/// table resolver requires every cell to hold at least one block.
fn text_cell(text: &str, role: &str) -> TableCellSpec {
    TableCellSpec {
        content: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
            tab_stops: None,
            format_from: None,
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
