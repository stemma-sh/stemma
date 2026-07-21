//! RFC-0003: granular structural table ops (`table_op` insert/delete row/column,
//! merge) must SUCCEED on a fully-formatted wild-style table and round-trip its
//! formatting byte-identically. Before RFC-0003 these ops routed through the
//! blanket `validate_base_table_v4_compatible` guard, which refused any table
//! carrying non-default borders/shading/widths/style — i.e. ~100% of real
//! tables.
//!
//! The fix is model-honest: `build_target` clones the base `TableNode` and
//! mutates the clone, and `apply_table_structure_changed` carries `tblPr` plus
//! every matched/deleted row's `trPr`/`tcPr` through unchanged — so the
//! structural edit cannot drop formatting. The only base state still refused is
//! an UNRESOLVED tracked change (`TableMidRedline`), because the structural diff
//! can't layer a fresh revision over an in-flight one.
//!
//! Domain rules encoded (OOXML §17.13 accept/reject + formatting preservation):
//!   - `apply` SUCCEEDS on a formatted base (no `TableHasFormattingNotInSpec`);
//!   - `accept_all` yields the intended structure; `reject_all` yields the base;
//!   - the table's `tblPr` and every surviving cell's `tcPr` are byte-identical
//!     to the base; `tblGrid` stays length-consistent across column ops;
//!   - a base carrying an unresolved tracked change is refused (`TableMidRedline`).

use stemma::accept_all;
use stemma::domain::*;
use stemma::edit::{EditError, EditTransaction, apply_transaction};
use stemma::edit_v4::parse_transaction;
use stemma::reject_all_with_styles;

// ─── Doc-construction helpers (formatting-carrying) ──────────────────────────

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
            rpr_authored: RunRprAuthored::default(),
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
        literal_prefix_rpr_authored: RunRprAuthored::default(),
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

fn yellow_shading() -> Shading {
    Shading {
        fill: Some("FFFF00".to_string()),
        val: Some(ShadingPattern::Clear),
        color: Some("auto".to_string()),
        extra_attrs: Vec::new(),
    }
}

fn single_borders() -> BorderSet {
    let edge = Border {
        style: BorderStyle::Single,
        color: Some("000000".to_string()),
        size: Some(4),
        space: Some(0),
        extra_attrs: Vec::new(),
    };
    BorderSet {
        top: Some(edge.clone()),
        bottom: Some(edge.clone()),
        left: Some(edge.clone()),
        right: Some(edge.clone()),
        inside_h: Some(edge.clone()),
        inside_v: Some(edge),
    }
}

/// A cell carrying NON-DEFAULT formatting (shading + explicit width + vAlign).
fn formatted_cell(id: &str, text: &str) -> TableCellNode {
    TableCellNode {
        id: NodeId::from(id),
        blocks: vec![BlockNode::from(text_para(&format!("{id}_p"), text))],
        grid_span: 1,
        v_merge: VerticalMerge::None,
        formatting: CellFormatting {
            shading: Some(yellow_shading()),
            v_align: Some(VerticalAlignment::Center),
            width: Some(TableMeasurement {
                w: 2400,
                width_type: WidthType::Dxa,
                pct_literal: None,
            }),
            ..CellFormatting::default()
        },
        formatting_change: None,
        tracking_status: None,
        row_sdt_wrapper: None,
        content_sdt_wraps: Vec::new(),
        cnf_style: None,
        hide_mark: false,
        preserved: Vec::new(),
    }
}

fn formatted_row(id: &str, cells: Vec<TableCellNode>) -> TableRowNode {
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
        cant_split: true,
        jc: None,
        w_before: None,
        w_after: None,
        cnf_style: None,
        tbl_pr_ex: None,
        cell_spacing: None,
        preserved: Vec::new(),
    }
}

/// Table-level formatting (style + borders + width + grid). `grid_cols` is set
/// to `n_cols` real widths so the column-op grid maintenance is exercised.
fn formatted_table_formatting(n_cols: usize) -> TableFormatting {
    TableFormatting {
        style_id: Some("TableGrid".into()),
        borders: Some(single_borders()),
        width: Some(TableMeasurement {
            w: 5000,
            width_type: WidthType::Pct,
            pct_literal: None,
        }),
        grid_cols: (0..n_cols).map(|i| 1200 + (i as u32) * 100).collect(),
        ..TableFormatting::default()
    }
}

/// An `r_rows × c_cols` grid of formatted cells with real per-column widths.
fn formatted_grid(rows: usize, cols: usize) -> TableNode {
    let mut trows = Vec::new();
    for r in 0..rows {
        let cells = (0..cols)
            .map(|c| formatted_cell(&format!("t1_r{r}c{c}"), &format!("r{r}c{c}")))
            .collect();
        trows.push(formatted_row(&format!("t1_r{r}"), cells));
    }
    TableNode {
        id: NodeId::from("t1"),
        rows: trows,
        structure_hash: String::new(),
        formatting: formatted_table_formatting(cols),
        formatting_change: None,
    }
}

fn doc_with(table: TableNode) -> CanonDoc {
    CanonDoc {
        id: NodeId::from("doc1"),
        blocks: vec![
            normal_tracked_block(BlockNode::from(text_para("body", "body"))),
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

fn try_apply(doc: &CanonDoc, json: &str) -> Result<CanonDoc, EditError> {
    let txn = translate(json);
    apply_transaction(doc, &txn).map(|r| r.0)
}

fn table_op_json(target: &str, op_body: &str) -> String {
    format!(
        r#"{{ "ops": [{{ "op": "table_op", "target": "{target}", "table_op": {op_body} }}],
            "revision": {{ "author": "Counsel" }} }}"#
    )
}

fn table_op_json_direct(target: &str, op_body: &str) -> String {
    format!(
        r#"{{ "ops": [{{ "op": "table_op", "target": "{target}", "table_op": {op_body} }}],
            "materialization_mode": "direct",
            "revision": {{ "author": "Counsel" }} }}"#
    )
}

fn find_table<'a>(doc: &'a CanonDoc, id: &str) -> &'a TableNode {
    let nid = NodeId::from(id);
    doc.blocks
        .iter()
        .find_map(|tb| match &tb.block {
            BlockNode::Table(t) if t.id == nid => Some(t),
            _ => None,
        })
        .expect("table present")
}

/// Look up a surviving cell by its id anywhere in the table.
fn cell_by_id<'a>(table: &'a TableNode, id: &str) -> Option<&'a TableCellNode> {
    let nid = NodeId::from(id);
    table
        .rows
        .iter()
        .flat_map(|r| r.cells.iter())
        .find(|c| c.id == nid)
}

/// Assert `tblPr` (table-level formatting) is byte-identical to the base.
fn assert_table_formatting_preserved(doc: &CanonDoc, base: &TableNode) {
    let t = find_table(doc, "t1");
    assert_eq!(
        t.formatting.style_id, base.formatting.style_id,
        "tblStyle must be byte-preserved"
    );
    assert_eq!(
        t.formatting.borders, base.formatting.borders,
        "tblBorders must be byte-preserved"
    );
    assert_eq!(
        t.formatting.width, base.formatting.width,
        "tblW must be byte-preserved"
    );
}

/// Assert every base cell (by id) that survives carries its original tcPr.
fn assert_surviving_cells_formatting_preserved(doc: &CanonDoc, base: &TableNode) {
    let t = find_table(doc, "t1");
    for base_row in &base.rows {
        for base_cell in &base_row.cells {
            if let Some(now) = cell_by_id(t, &base_cell.id.0) {
                assert_eq!(
                    now.formatting, base_cell.formatting,
                    "cell {} tcPr (shading/width/vAlign) must be byte-preserved",
                    base_cell.id.0
                );
            }
        }
    }
}

// ─── InsertRow ───────────────────────────────────────────────────────────────

#[test]
fn insert_row_on_formatted_table_succeeds_and_preserves_formatting() {
    let base = formatted_grid(2, 2);
    let doc = doc_with(base.clone());

    let edited = try_apply(
        &doc,
        &table_op_json(
            "t1",
            r#"{ "kind": "insert_row", "ref_row": 1, "position": "after" }"#,
        ),
    )
    .expect("insert_row on a formatted table must not be refused (RFC-0003)");

    let mut accepted = edited.clone();
    accept_all(&mut accepted);
    assert_eq!(
        find_table(&accepted, "t1").rows.len(),
        3,
        "accept yields a third row"
    );
    assert_table_formatting_preserved(&accepted, &base);
    assert_surviving_cells_formatting_preserved(&accepted, &base);

    let mut rejected = edited.clone();
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        find_table(&rejected, "t1").rows.len(),
        2,
        "reject restores the 2-row base"
    );
    assert_table_formatting_preserved(&rejected, &base);
    assert_surviving_cells_formatting_preserved(&rejected, &base);
}

// ─── DeleteRow ─────────────────────────────────────────────────────────────

#[test]
fn delete_row_on_formatted_table_succeeds_and_preserves_formatting() {
    let base = formatted_grid(3, 2);
    let doc = doc_with(base.clone());

    let edited = try_apply(
        &doc,
        &table_op_json("t1", r#"{ "kind": "delete_row", "row_index": 1 }"#),
    )
    .expect("delete_row on a formatted table must not be refused (RFC-0003)");

    let mut accepted = edited.clone();
    accept_all(&mut accepted);
    assert_eq!(
        find_table(&accepted, "t1").rows.len(),
        2,
        "accept drops the deleted row"
    );
    // Surviving rows r0 and r2 keep their cell formatting.
    assert_table_formatting_preserved(&accepted, &base);
    assert_surviving_cells_formatting_preserved(&accepted, &base);

    let mut rejected = edited.clone();
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        find_table(&rejected, "t1").rows.len(),
        3,
        "reject restores the deleted row"
    );
    assert_surviving_cells_formatting_preserved(&rejected, &base);
}

// ─── MergeCells ──────────────────────────────────────────────────────────────

#[test]
fn merge_cells_on_formatted_table_succeeds_and_preserves_anchor_formatting() {
    let base = formatted_grid(2, 2);
    let doc = doc_with(base.clone());

    let edited = try_apply(
        &doc,
        &table_op_json(
            "t1",
            r#"{ "kind": "merge_cells", "start_row": 0, "start_col": 0, "end_row": 0, "end_col": 1 }"#,
        ),
    )
    .expect("merge_cells on a formatted table must not be refused (RFC-0003)");

    let mut accepted = edited.clone();
    accept_all(&mut accepted);
    // The anchor cell keeps its formatting; table formatting is preserved.
    assert_table_formatting_preserved(&accepted, &base);
    let anchor = cell_by_id(find_table(&accepted, "t1"), "t1_r0c0").expect("anchor survives");
    assert_eq!(
        anchor.formatting,
        formatted_cell("t1_r0c0", "r0c0").formatting,
        "merge anchor keeps its tcPr"
    );
    assert!(anchor.grid_span >= 2, "anchor spans the merged columns");

    let mut rejected = edited.clone();
    reject_all_with_styles(&mut rejected, None);
    assert_surviving_cells_formatting_preserved(&rejected, &base);
}

// ─── Column ops on an explicit grid ─────────────────────────────────────────
//
// Both DIRECT and TRACKED column ops keep `tblGrid` length-consistent with the
// column count and every surviving column keeps its width. DIRECT replaces the
// table wholesale; TRACKED marks the exact column's cells (w:cellIns/w:cellDel)
// and the accept/reject projection drops the matching gridCol on resolution
// (`apply_tracked_column_op` + `uniformly_removed_columns`, RFC-0003).

#[test]
fn insert_column_direct_on_formatted_table_maintains_grid_cols() {
    let base = formatted_grid(2, 3);
    assert_eq!(base.formatting.grid_cols.len(), 3);
    let doc = doc_with(base.clone());

    let edited = try_apply(
        &doc,
        &table_op_json_direct(
            "t1",
            r#"{ "kind": "insert_column", "ref_col": 1, "position": "after" }"#,
        ),
    )
    .expect("direct insert_column must apply on a formatted table");

    let t = find_table(&edited, "t1");
    let cols = t.rows[0].cells.len();
    assert_eq!(cols, 4, "a fourth column is added");
    assert_eq!(
        t.formatting.grid_cols.len(),
        cols,
        "tblGrid must stay length-consistent with the column count"
    );
    // The inserted gridCol inherited the reference column's width.
    assert_eq!(t.formatting.grid_cols[2], base.formatting.grid_cols[1]);
    assert_table_formatting_preserved(&edited, &base);
    assert_surviving_cells_formatting_preserved(&edited, &base);
}

#[test]
fn delete_column_direct_on_formatted_table_maintains_grid_cols() {
    let base = formatted_grid(2, 3);
    let doc = doc_with(base.clone());

    let edited = try_apply(
        &doc,
        &table_op_json_direct("t1", r#"{ "kind": "delete_column", "col_index": 1 }"#),
    )
    .expect("direct delete_column must apply on a formatted table");

    let t = find_table(&edited, "t1");
    let cols = t.rows[0].cells.len();
    assert_eq!(cols, 2, "a column is dropped");
    assert_eq!(
        t.formatting.grid_cols.len(),
        cols,
        "tblGrid must stay length-consistent with the column count"
    );
    // The middle gridCol width is the one removed.
    assert_eq!(
        t.formatting.grid_cols,
        vec![base.formatting.grid_cols[0], base.formatting.grid_cols[2]]
    );
    assert_table_formatting_preserved(&edited, &base);
}

#[test]
fn tracked_insert_column_on_explicit_grid_keeps_grid_consistent() {
    let base = formatted_grid(2, 3); // grid_cols = [1200, 1300, 1400]
    let doc = doc_with(base.clone());

    let edited = try_apply(
        &doc,
        &table_op_json(
            "t1",
            r#"{ "kind": "insert_column", "ref_col": 1, "position": "after" }"#,
        ),
    )
    .expect("tracked insert_column over an explicit grid now succeeds (RFC-0003)");

    // Suggested state: 4 physical columns, grid_cols length 4 (consistent).
    let t = find_table(&edited, "t1");
    assert_eq!(t.rows[0].cells.len(), 4);
    assert_eq!(
        t.formatting.grid_cols.len(),
        4,
        "grid_cols must match the 4 physical columns pre-resolution"
    );

    // Accept: the inserted column stays; grid stays length 4; the new gridCol
    // inherited the reference column's (col 1) width.
    let mut accepted = edited.clone();
    accept_all(&mut accepted);
    let t = find_table(&accepted, "t1");
    assert_eq!(t.rows[0].cells.len(), 4);
    assert_eq!(t.formatting.grid_cols.len(), 4);
    assert_eq!(t.formatting.grid_cols[2], base.formatting.grid_cols[1]);
    assert_table_formatting_preserved(&accepted, &base);

    // Reject: the inserted column AND its gridCol vanish → back to the base grid.
    let mut rejected = edited.clone();
    reject_all_with_styles(&mut rejected, None);
    let t = find_table(&rejected, "t1");
    assert_eq!(t.rows[0].cells.len(), 3, "reject restores 3 columns");
    assert_eq!(
        t.formatting.grid_cols, base.formatting.grid_cols,
        "reject restores the original tblGrid exactly"
    );
}

#[test]
fn tracked_delete_column_on_explicit_grid_keeps_grid_consistent() {
    let base = formatted_grid(2, 3); // grid_cols = [1200, 1300, 1400]
    let doc = doc_with(base.clone());

    let edited = try_apply(
        &doc,
        &table_op_json("t1", r#"{ "kind": "delete_column", "col_index": 1 }"#),
    )
    .expect("tracked delete_column over an explicit grid now succeeds (RFC-0003)");

    // Suggested state: the deleted cell is still physically present (w:cellDel),
    // so 3 columns and grid_cols length 3.
    let t = find_table(&edited, "t1");
    assert_eq!(t.formatting.grid_cols.len(), 3);

    // Accept: the deleted column AND its gridCol vanish → the middle width goes.
    let mut accepted = edited.clone();
    accept_all(&mut accepted);
    let t = find_table(&accepted, "t1");
    assert_eq!(t.rows[0].cells.len(), 2, "accept drops the column");
    assert_eq!(
        t.formatting.grid_cols,
        vec![base.formatting.grid_cols[0], base.formatting.grid_cols[2]],
        "accept drops the deleted column's gridCol (the middle width)"
    );

    // Reject: the column survives; grid unchanged.
    let mut rejected = edited.clone();
    reject_all_with_styles(&mut rejected, None);
    let t = find_table(&rejected, "t1");
    assert_eq!(t.rows[0].cells.len(), 3, "reject keeps all 3 columns");
    assert_eq!(t.formatting.grid_cols, base.formatting.grid_cols);
}

// ─── The narrow guard: refuse only a mid-redline base ───────────────────────

#[test]
fn structural_op_on_table_with_pending_cell_change_refuses() {
    let mut base = formatted_grid(2, 2);
    // Simulate an unresolved tracked tcPrChange on one cell.
    base.rows[0].cells[0].formatting_change = Some(CellFormattingChange {
        previous_width: None,
        previous_borders: None,
        previous_shading: None,
        previous_v_align: None,
        previous_margins: None,
        previous_no_wrap: None,
        previous_text_direction: None,
        previous_tc_fit_text: None,
        revision_id: 1,
        identity: 0,
        author: "Prior".to_string(),
        date: None,
    });
    let doc = doc_with(base);

    let err = try_apply(
        &doc,
        &table_op_json(
            "t1",
            r#"{ "kind": "insert_row", "ref_row": 0, "position": "after" }"#,
        ),
    )
    .expect_err("a structural edit over an in-flight tcPrChange must be refused");
    match err {
        EditError::TableMidRedline { location, .. } => {
            assert!(
                location.contains("cell[0]") && location.contains("tcPrChange"),
                "location should point at the in-flight change, got {location:?}"
            );
        }
        other => panic!("expected TableMidRedline, got {other:?}"),
    }
}

// ─── replace(table) carries base formatting (RFC-0003 Phase 4) ──────────────

/// A `replace` op that rewrites t1 with a 2×2 grid of the given texts.
fn replace_2x2_json(texts: [[&str; 2]; 2]) -> String {
    let cell = |t: &str| {
        format!(
            r#"{{ "content": [{{ "type": "paragraph", "role": "body_text", "content": [{{ "type": "text", "text": "{t}" }}] }}] }}"#
        )
    };
    let row = |r: [&str; 2]| format!(r#"{{ "content": [{}, {}] }}"#, cell(r[0]), cell(r[1]));
    format!(
        r#"{{ "ops": [{{ "op": "replace", "target": "t1",
            "content": {{ "type": "table", "content": [{}, {}] }} }}],
            "revision": {{ "author": "Counsel" }} }}"#,
        row(texts[0]),
        row(texts[1])
    )
}

#[test]
fn replace_same_shape_preserves_table_and_cell_formatting() {
    let base = formatted_grid(2, 2);
    let doc = doc_with(base.clone());

    let edited = try_apply(&doc, &replace_2x2_json([["X0", "X1"], ["X2", "X3"]]))
        .expect("replace on a formatted table must be accepted (RFC-0003)");

    // The suggested (pre-resolution) doc already carries the base formatting.
    assert_table_formatting_preserved(&edited, &base);

    let mut accepted = edited.clone();
    accept_all(&mut accepted);
    assert_table_formatting_preserved(&accepted, &base);
    // Matched cells keep the base cell tcPr (shading/width/vAlign) — the carried
    // target equals the base, so no spurious tcPrChange is authored.
    let t = find_table(&accepted, "t1");
    for row in &t.rows {
        for cell in &row.cells {
            assert_eq!(
                cell.formatting,
                formatted_cell("x", "x").formatting,
                "replaced cell keeps the base tcPr"
            );
        }
    }

    let mut rejected = edited.clone();
    reject_all_with_styles(&mut rejected, None);
    assert_table_formatting_preserved(&rejected, &base);
}

// ─── Item 1: caller-SET formatting on the replace wire ──────────────────────

/// A `replace` that sets a table style + cell (0,0) shading via `attrs`.
fn replace_with_formatting_json(direct: bool) -> String {
    let para = |t: &str| {
        format!(
            r#"{{"type":"paragraph","role":"body_text","content":[{{"type":"text","text":"{t}"}}]}}"#
        )
    };
    let mode = if direct {
        r#""materialization_mode": "direct","#
    } else {
        ""
    };
    format!(
        r#"{{ "ops": [{{ "op": "replace", "target": "t1",
            "content": {{ "type": "table",
                "attrs": {{ "style": "LightGrid" }},
                "content": [
                    {{ "content": [
                        {{ "attrs": {{ "shading": {{ "fill": "FF0000" }} }}, "content": [{}] }},
                        {{ "content": [{}] }}
                    ] }},
                    {{ "content": [ {{ "content": [{}] }}, {{ "content": [{}] }} ] }}
                ] }} }}],
            {} "revision": {{ "author": "Counsel" }} }}"#,
        para("X0"),
        para("X1"),
        para("X2"),
        para("X3"),
        mode
    )
}

#[test]
fn tracked_replace_with_spec_formatting_is_refused() {
    // A TRACKED replace can't represent caller-set table formatting as a
    // reversible tracked change (would break reject-all == base), so it fails
    // loud, pointing to direct mode / the Set*Formatting verbs.
    let doc = doc_with(formatted_grid(2, 2));
    let err = try_apply(&doc, &replace_with_formatting_json(false))
        .expect_err("tracked replace with spec formatting must be refused (RFC-0003 Item 1)");
    assert!(
        matches!(err, EditError::TableSpecFormattingRequiresDirect { .. }),
        "expected TableSpecFormattingRequiresDirect, got {err:?}"
    );
}

#[test]
fn direct_replace_with_spec_formatting_wins_and_fills_from_base() {
    // Base: formatted 2×2 (style=TableGrid, borders, per-cell yellow shd+width).
    let base = formatted_grid(2, 2);
    let doc = doc_with(base.clone());
    let edited = try_apply(&doc, &replace_with_formatting_json(true))
        .expect("direct replace with caller-set formatting applies");
    let t = find_table(&edited, "t1");

    // Table style is the SPEC's (wins over base's TableGrid); base borders the
    // spec left unset are still preserved (fill-if-default carry).
    assert_eq!(t.formatting.style_id.as_deref(), Some("LightGrid"));
    assert_eq!(
        t.formatting.borders, base.formatting.borders,
        "base tblBorders preserved where the spec didn't override"
    );
    // Cell (0,0): the spec's red shading.
    assert_eq!(
        t.rows[0].cells[0]
            .formatting
            .shading
            .as_ref()
            .and_then(|s| s.fill.as_deref()),
        Some("FF0000")
    );
    // Cell (0,1): spec set nothing → inherits the base cell's yellow shd + width.
    assert_eq!(
        t.rows[0].cells[1]
            .formatting
            .shading
            .as_ref()
            .and_then(|s| s.fill.as_deref()),
        Some("FFFF00")
    );
    assert!(t.rows[0].cells[1].formatting.width.is_some());
}

#[test]
fn insert_table_with_spec_formatting_carries_the_look() {
    // Insert a NEW formatted table after the body paragraph. The whole table is
    // a tracked insert, so its formatting is part of it (reject removes the
    // table); spec formatting on insert is allowed in tracked mode.
    let doc = doc_with(formatted_grid(1, 1));
    let json = r#"{ "ops": [{ "op": "insert",
        "target": { "anchor": "body", "position": "after" },
        "content": [{ "type": "table",
            "attrs": { "style": "LightGrid" },
            "content": [{ "content": [
                { "attrs": { "shading": { "fill": "00FF00" } },
                  "content": [{"type":"paragraph","role":"body_text","content":[{"type":"text","text":"new"}]}] }
            ] }] }] }],
        "revision": { "author": "Counsel" } }"#;
    let edited = try_apply(&doc, json).expect("insert formatted table applies");
    let mut acc = edited.clone();
    accept_all(&mut acc);
    // Find the inserted table (the one that is not the pre-existing "t1").
    let inserted = acc
        .blocks
        .iter()
        .find_map(|b| match &b.block {
            BlockNode::Table(t) if t.id.0.as_ref() != "t1" => Some(t),
            _ => None,
        })
        .expect("inserted table present after accept");
    assert_eq!(inserted.formatting.style_id.as_deref(), Some("LightGrid"));
    assert_eq!(
        inserted.rows[0].cells[0]
            .formatting
            .shading
            .as_ref()
            .and_then(|s| s.fill.as_deref()),
        Some("00FF00")
    );
}

#[test]
fn structural_op_on_table_with_tracked_row_refuses() {
    let mut base = formatted_grid(2, 2);
    base.rows[1].tracking_status = Some(TrackingStatus::Inserted(RevisionInfo {
        revision_id: 7,
        identity: 0,
        author: Some("Prior".to_string()),
        date: None,
        apply_op_id: None,
    }));
    let doc = doc_with(base);

    let err = try_apply(
        &doc,
        &table_op_json("t1", r#"{ "kind": "delete_row", "row_index": 0 }"#),
    )
    .expect_err("a structural edit over a tracked row ins/del must be refused");
    assert!(matches!(err, EditError::TableMidRedline { .. }));
}
