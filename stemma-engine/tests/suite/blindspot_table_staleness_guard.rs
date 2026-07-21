//! Blindspot regression: the Table block **staleness guard** must change when a
//! structure-preserving cell-text edit changes a cell.
//!
//! CONTRACT (the single-staleness-mechanism contract, pinned in
//! `stemma-engine/src/semantic_hash.rs`):
//!   "If the block changed between read and write, the hash differs and the op
//!    fails loud (StaleEdit). The precondition and the staleness check are the
//!    same object."
//! and, for the text axis, the guard "pins exactly what a text edit reasons
//! about: the visible reading of the block".
//!
//! A `set_cell_text` edit changes the VISIBLE READING of the block (a cell's
//! text). The guard is the read-side value (`BlockView::guard`,
//! `block_semantic_hash_for_block`) that a subsequent write carries to prove it
//! addressed a FRESH snapshot. Therefore: after a cell-text edit, the block's
//! guard MUST differ from the pre-edit guard. If it does not, a write built
//! against the pre-edit reading would pass the staleness check against a block
//! whose visible cell text has drifted underneath it — a silent stale apply,
//! which the "no silent fallbacks" contract forbids.
//!
//! SUSPECTED DEFECT (semantic_hash.rs:174, import.rs:3461, table_ops.rs:143):
//! a Table block's guard is `table:{structure_hash}`, and
//! `compute_table_structure_hash` hashes ONLY structure (row/cell counts, grid
//! offsets, gridSpan, vMerge) — never cell text. So a structure-preserving
//! cell-text edit leaves the guard byte-identical.
//!
//! This test reads the guard, applies a structure-preserving `set_cell_text`,
//! re-reads the guard, and asserts they DIFFER. It also contrasts with a
//! paragraph block, whose guard correctly tracks text, to show the table case is
//! the outlier, not a property of the guard scheme itself.

use stemma::domain::*;
use stemma::edit::{EditTransaction, apply_transaction};
use stemma::edit_v4::parse_transaction;
use stemma::semantic_hash::block_semantic_hash_for_block;
use stemma::view::build_document_view_from_canon;

// ─── Doc-construction helpers (verbatim from tests/table_set_cell_text.rs) ────

fn text_para(id: &str, text: &str) -> ParagraphNode {
    ParagraphNode {
        id: NodeId::from(id),
        style_id: None,
        align: None,
        has_direct_align: false,
        indent: None,
        has_direct_indent: false,
        authored_indent: None,
        spacing: None,
        has_direct_spacing: false,
        authored_spacing: None,
        borders: None,
        keep_next: None,
        keep_lines: None,
        page_break_before: false,
        widow_control: None,
        contextual_spacing: None,
        shading: None,
        has_direct_keep_next: true,
        has_direct_keep_lines: true,
        has_direct_page_break_before: true,
        has_direct_widow_control: true,
        has_direct_contextual_spacing: true,
        has_direct_shading: true,
        has_direct_borders: true,
        tab_stops: vec![],
        effective_tab_stops_rel: vec![],
        segments: normal_segment(vec![InlineNode::from(TextNode {
            id: NodeId::from(format!("{id}_t")),
            text_role: None,
            text: text.to_string(),
            marks: vec![],
            style_props: StyleProps::default(),
            rpr_authored: stemma::domain::RunRprAuthored::default(),
            source_run_attrs: Vec::new(),
            formatting_change: None,
        })]),
        block_text_hash: None,
        numbering: None,
        has_direct_numbering: true,
        numbering_suppressed: false,
        materialized_numbering: None,
        rendered_text: None,
        literal_prefix: None,
        literal_prefix_marks: Vec::new(),
        literal_prefix_style_props: StyleProps::default(),
        literal_prefix_rpr_authored: stemma::domain::RunRprAuthored::default(),
        literal_prefix_leading_rpr: None,
        literal_prefix_trailing_rpr: None,
        literal_prefix_leading_tab_twips: None,
        literal_prefix_leading_tab_count: 0,
        literal_prefix_leading_ws: String::new(),
        literal_prefix_trailing_ws: String::new(),
        literal_prefix_has_trailing_tab: false,
        literal_prefix_trailing_tab_stop_twips: None,
        outline_lvl: None,
        heading_level: None,
        para_mark_status: None,
        paragraph_mark_marks: vec![],
        paragraph_mark_style_props: StyleProps::default(),
        paragraph_mark_rpr_off: Default::default(),
        para_split: false,
        section_property_change: None,
        formatting_change: None,
        section_properties: None,
        mirror_indents: None,
        auto_space_de: None,
        auto_space_dn: None,
        bidi: None,
        text_alignment: None,
        suppress_auto_hyphens: None,
        snap_to_grid: None,
        overflow_punct: None,
        adjust_right_ind: None,
        word_wrap: None,
        frame_pr: None,
        para_id: None,
        text_id: None,
        text_direction: None,
        cnf_style: None,
        preserved_ppr: Vec::new(),
    }
}

fn plain_cell(id: &str, text: &str) -> TableCellNode {
    TableCellNode {
        id: NodeId::from(id),
        blocks: vec![BlockNode::from(text_para(&format!("{id}_p"), text))],
        grid_span: 1,
        v_merge: VerticalMerge::None,
        formatting: CellFormatting::default(),
        formatting_change: None,
        tracking_status: None,
        row_sdt_wrapper: None,
        content_sdt_wraps: Vec::new(),
        cnf_style: None,
        hide_mark: false,
        preserved: Vec::new(),
    }
}

fn row(id: &str, cells: Vec<TableCellNode>) -> TableRowNode {
    TableRowNode {
        id: NodeId::from(id),
        cells,
        grid_before: 0,
        grid_after: 0,
        tracking_status: None,
        is_header: false,
        height: Some(360),
        height_rule: Some(HeightRule::AtLeast),
        formatting_change: None,
        para_id: None,
        text_id: None,
        cant_split: false,
        jc: None,
        w_before: None,
        w_after: None,
        cnf_style: None,
        tbl_pr_ex: None,
        cell_spacing: None,
        preserved: Vec::new(),
    }
}

/// A 1-row, 2-cell table with KNOWN cell text. The structure is what makes the
/// bug reproducible: a cell-text edit leaves the structure (and thus the
/// structure hash that the Table guard is built from) untouched. `structure_hash`
/// is left empty here exactly as the sibling table tests do — the guard
/// comparison is before-vs-after on the SAME table, so any stable structure-hash
/// value is fine; what matters is whether a cell-text edit moves it.
fn one_row_table() -> TableNode {
    TableNode {
        id: NodeId::from("t1"),
        rows: vec![row(
            "t1_r0",
            vec![
                plain_cell("t1_r0c0", "Alpha"),
                plain_cell("t1_r0c1", "Beta"),
            ],
        )],
        structure_hash: String::new(),
        formatting: TableFormatting::default(),
        formatting_change: None,
    }
}

fn doc_with(table: TableNode) -> CanonDoc {
    CanonDoc {
        id: NodeId::from("doc1"),
        blocks: vec![
            normal_tracked_block(BlockNode::from(text_para("body", "body text here"))),
            normal_tracked_block(BlockNode::from(table)),
        ],
        meta: DocMeta {
            schema_version: SCHEMA_VERSION_V0.to_string(),
            docx_fingerprint: DocFingerprint("test".to_string()),
            internal_ids_version: INTERNAL_IDS_VERSION_V0.to_string(),
        },
        headers: vec![],
        footers: vec![],
        footnotes: vec![],
        endnotes: vec![],
        comments: vec![],
        comments_extended: vec![],
        body_section_properties: None,
        body_section_property_change: None,
        compat_settings: CompatSettings::default(),
        even_and_odd_headers: None,
        document_background: None,
        document_protection: None,
    }
}

fn translate(json: &str) -> EditTransaction {
    parse_transaction(json)
        .expect("schema check passes")
        .into_edit_transaction()
        .expect("adapter succeeds")
}

fn set_cell_text_json(target: &str, row: usize, col: usize, text: &str) -> String {
    format!(
        r#"{{ "ops": [{{ "op": "table_op", "target": "{target}",
            "table_op": {{ "kind": "set_cell_text", "row_index": {row}, "col_index": {col}, "text": "{text}" }} }}],
            "revision": {{ "author": "Counsel" }} }}"#
    )
}

/// The guard for the block with id `block_id`, as the READ surface exposes it
/// (`BlockView::guard`) — the same value a write op carries.
fn block_guard(doc: &CanonDoc, block_id: &str) -> String {
    let view = build_document_view_from_canon(doc);
    view.blocks
        .iter()
        .find(|b| b.id.to_string() == block_id)
        .unwrap_or_else(|| panic!("block {block_id} present in view"))
        .guard
        .clone()
}

// ─── The blindspot: table guard must track cell-text drift ───────────────────

#[test]
fn table_guard_changes_when_cell_text_changes() {
    let doc = doc_with(one_row_table());

    // Read the table block's guard BEFORE the edit (the value a write op would
    // carry to prove freshness).
    let guard_before = block_guard(&doc, "t1");

    // Apply a STRUCTURE-PRESERVING cell-text edit: change r0c0 "Alpha" -> "ZULU".
    // Row/cell counts, grid, gridSpan, vMerge are all unchanged.
    let txn = set_cell_text_json("t1", 0, 0, "ZULU");
    let (edited, _) = apply_transaction(&doc, &translate(&txn)).expect("set_cell_text applies");

    // Re-read the guard. The visible reading of the block changed (a cell's text
    // drifted), so the guard MUST change — otherwise a write built against the
    // pre-edit reading silently passes the staleness check.
    let guard_after = block_guard(&edited, "t1");

    assert_ne!(
        guard_before, guard_after,
        "table block guard MUST change when a cell's text changes; it stayed \
         byte-identical ({guard_before}), so a stale SetCellText would pass the \
         staleness check (semantic_hash.rs:174 hashes only structure_hash, \
         import.rs:3461 excludes cell text)"
    );
}

#[test]
fn table_guard_via_engine_hash_changes_with_cell_text() {
    // Same assertion at the engine staleness-check level: `apply` (table_ops.rs)
    // compares `block_semantic_hash_for_block(&block)` against the carried guard.
    // That exact function must distinguish the pre- and post-edit table block.
    let doc = doc_with(one_row_table());
    let table_before = &doc.blocks[1].block;
    let hash_before = block_semantic_hash_for_block(table_before);

    let txn = set_cell_text_json("t1", 0, 1, "OMEGA");
    let (edited, _) = apply_transaction(&doc, &translate(&txn)).expect("set_cell_text applies");
    let table_after = &edited.blocks[1].block;
    let hash_after = block_semantic_hash_for_block(table_after);

    assert_ne!(
        hash_before, hash_after,
        "block_semantic_hash_for_block must differ after a cell-text edit \
         (the engine's staleness check uses exactly this); equal hashes mean a \
         stale base passes the guard"
    );
}

#[test]
fn paragraph_guard_changes_with_text_baseline() {
    // CONTROL: a paragraph block's guard DOES track text (semantic_hash.rs hashes
    // each visible text atom). This proves the table failure above is a
    // table-specific blindspot, not a property of the guard scheme — the guard
    // is supposed to change on a visible-text edit.
    let mut doc = doc_with(one_row_table());
    let para_before = &doc.blocks[0].block;
    let hash_before = block_semantic_hash_for_block(para_before);

    // Mutate the paragraph's text directly (structure-preserving content change).
    if let BlockNode::Paragraph(p) = &mut doc.blocks[0].block
        && let Some(seg) = p.segments.first_mut()
        && let Some(InlineNode::Text(t)) = seg.inlines.first_mut()
    {
        t.text = "completely different body".to_string();
    }
    let hash_after = block_semantic_hash_for_block(&doc.blocks[0].block);

    assert_ne!(
        hash_before, hash_after,
        "sanity: a paragraph guard must change when its text changes"
    );
}
