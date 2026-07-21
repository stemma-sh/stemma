//! Integration tests for `table_op.set_cell_text` on REAL (formatted) tables.
//!
//! THE BUG these tests pin (round-3 finding): `set_cell_text` used to route
//! through the WHOLE-TABLE replace schema (`lower_table_target` /
//! `validate_base_table_v4_compatible`), which REFUSES any table carrying
//! non-default borders / shading / widths — because a whole-table REPLACE would
//! silently drop that formatting. Every real report/lease table has formatting,
//! so `set_cell_text` was unusable on exactly the tables that matter.
//!
//! THE FIX (model-honest): editing ONE cell's text does NOT touch the table's /
//! row's / cell's formatting, so it must NOT route through the replace schema.
//! `set_cell_text` is now an in-place cell-paragraph-text edit that replaces only
//! the target cell's paragraph text through the SAME materializer
//! `ReplaceParagraphText` uses — producing a normal tracked `w:ins`/`w:del`
//! inside the cell while `tblPr`, `trPr`, every `tcPr`, and every other cell are
//! byte-preserved.
//!
//! Domain rules encoded (OOXML §17.13 accept/reject + formatting preservation):
//!   - `apply` SUCCEEDS on a formatted base (no `TableHasFormattingNotInSpec`);
//!   - `accept_all` == the doc with ONLY the target cell's text changed;
//!   - `reject_all` == the original (the cell edit is a normal tracked change);
//!   - the table's formatting and EVERY other cell's formatting are byte-identical
//!     before/after; only the target cell's paragraph TEXT changed;
//!   - opaque inlines inside the edited cell are preserved (an edit that would
//!     drop one is refused, not silently applied);
//!   - the logical-grid `{row, col}` address (the one `read_block.cells` exposes)
//!     resolves to the same cell on write — interior-of-a-merged-cell and
//!     vMerge-continuation targets fail loud.

use stemma::accept_all;
use stemma::domain::*;
use stemma::edit::{EditTransaction, apply_transaction};
use stemma::edit_v4::parse_transaction;
use stemma::reject_all_with_styles;

// ─── Doc-construction helpers ────────────────────────────────────────────────

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

/// A non-default cell shading (yellow fill) — the kind of formatting the old
/// whole-table guard refused.
fn yellow_shading() -> Shading {
    Shading {
        fill: Some("FFFF00".to_string()),
        val: Some(ShadingPattern::Clear),
        color: Some("auto".to_string()),
        extra_attrs: Vec::new(),
    }
}

/// A non-default single-line border on every edge.
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

/// A cell carrying NON-DEFAULT formatting (shading + an explicit width).
fn formatted_cell(id: &str, text: &str) -> TableCellNode {
    TableCellNode {
        id: NodeId::from(id),
        blocks: vec![BlockNode::from(text_para(&format!("{id}_p"), text))],
        grid_span: 1,
        v_merge: VerticalMerge::None,
        formatting: CellFormatting {
            shading: Some(yellow_shading()),
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

/// A 2×2 table with table-level borders + per-cell shading/width — i.e. the kind
/// of REAL formatted table the old `set_cell_text` refused.
fn formatted_table() -> TableNode {
    TableNode {
        id: NodeId::from("t1"),
        rows: vec![
            row(
                "t1_r0",
                vec![
                    formatted_cell("t1_r0c0", "r0c0"),
                    formatted_cell("t1_r0c1", "r0c1"),
                ],
            ),
            row(
                "t1_r1",
                vec![
                    formatted_cell("t1_r1c0", "r1c0"),
                    formatted_cell("t1_r1c1", "r1c1"),
                ],
            ),
        ],
        structure_hash: String::new(),
        formatting: TableFormatting {
            borders: Some(single_borders()),
            width: Some(TableMeasurement {
                w: 5000,
                width_type: WidthType::Pct,
                pct_literal: None,
            }),
            ..TableFormatting::default()
        },
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

fn set_cell_text_json(target: &str, row: usize, col: usize, text: &str) -> String {
    format!(
        r#"{{ "ops": [{{ "op": "table_op", "target": "{target}",
            "table_op": {{ "kind": "set_cell_text", "row_index": {row}, "col_index": {col}, "text": "{text}" }} }}],
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

/// Live (accept-all-style) text of a cell, dropping Deleted segments.
fn cell_live_text(cell: &TableCellNode) -> String {
    let mut out = String::new();
    for block in &cell.blocks {
        if let BlockNode::Paragraph(p) = block {
            for seg in &p.segments {
                if matches!(seg.status, TrackingStatus::Deleted(_)) {
                    continue;
                }
                for i in &seg.inlines {
                    if let InlineNode::Text(t) = i {
                        out.push_str(&t.text);
                    }
                }
            }
        }
    }
    out
}

// ─── The core bug: apply must SUCCEED on a formatted table ───────────────────

#[test]
fn set_cell_text_on_formatted_table_does_not_refuse() {
    let doc = doc_with(formatted_table());
    let txn = translate(&set_cell_text_json("t1", 0, 1, "REVISED"));
    // The old code returned ErrorCode::InvalidRange / TableHasFormattingNotInSpec
    // here. The fix must let it through.
    let edited = apply_transaction(&doc, &txn);
    assert!(
        edited.is_ok(),
        "set_cell_text on a formatted table must NOT be refused (was \
         TableHasFormattingNotInSpec), got {:?}",
        edited.err()
    );
}

#[test]
fn set_cell_text_accept_is_target_reject_is_base() {
    let doc = doc_with(formatted_table());
    let txn = translate(&set_cell_text_json("t1", 0, 1, "REVISED"));
    let edited = apply_transaction(&doc, &txn).expect("apply").0;

    // accept_all => only the target cell's text changed.
    let mut accepted = edited.clone();
    accept_all(&mut accepted);
    let at = find_table(&accepted, "t1");
    assert_eq!(cell_live_text(&at.rows[0].cells[0]), "r0c0");
    assert_eq!(cell_live_text(&at.rows[0].cells[1]), "REVISED");
    assert_eq!(cell_live_text(&at.rows[1].cells[0]), "r1c0");
    assert_eq!(cell_live_text(&at.rows[1].cells[1]), "r1c1");

    // reject_all => the original base text (the edit is a normal tracked change).
    let mut rejected = edited.clone();
    reject_all_with_styles(&mut rejected, None);
    let rt = find_table(&rejected, "t1");
    assert_eq!(cell_live_text(&rt.rows[0].cells[1]), "r0c1");
}

// ─── Formatting preservation: tblPr / tcPr / other cells byte-identical ──────

#[test]
fn set_cell_text_preserves_all_formatting_byte_identical() {
    let base = formatted_table();
    let doc = doc_with(base.clone());
    let txn = translate(&set_cell_text_json("t1", 0, 1, "REVISED"));
    let edited = apply_transaction(&doc, &txn).expect("apply").0;
    let et = find_table(&edited, "t1");

    // Table-level formatting (tblPr: borders/width) is byte-identical.
    assert_eq!(
        et.formatting, base.formatting,
        "table-level formatting must be byte-preserved"
    );

    // EVERY cell's formatting (tcPr: shading/width) is byte-identical, including
    // the EDITED one — only its paragraph TEXT changed, not its tcPr. Every
    // row's trPr (height) is byte-identical too.
    for (r, brow) in base.rows.iter().enumerate() {
        assert_eq!(
            et.rows[r].height, brow.height,
            "row {r} trPr height preserved"
        );
        assert_eq!(
            et.rows[r].height_rule, brow.height_rule,
            "row {r} trPr height_rule preserved"
        );
        for (c, bcell) in brow.cells.iter().enumerate() {
            assert_eq!(
                et.rows[r].cells[c].formatting, bcell.formatting,
                "row {r} cell {c} tcPr (shading/width) must be byte-preserved"
            );
        }
    }

    // Every cell EXCEPT the target is byte-identical as a whole node (id,
    // blocks, formatting) — the edit touched nothing else.
    assert_eq!(et.rows[0].cells[0], base.rows[0].cells[0]);
    assert_eq!(et.rows[1].cells[0], base.rows[1].cells[0]);
    assert_eq!(et.rows[1].cells[1], base.rows[1].cells[1]);

    // The target cell's tcPr survived; only its paragraph picked up tracked
    // segments (so it is NOT node-equal to the base).
    assert_eq!(
        et.rows[0].cells[1].formatting, base.rows[0].cells[1].formatting,
        "target cell tcPr preserved"
    );
    assert_ne!(
        et.rows[0].cells[1], base.rows[0].cells[1],
        "target cell paragraph text changed (tracked)"
    );
}

#[test]
fn set_cell_text_produces_inline_tracked_change_in_cell() {
    let doc = doc_with(formatted_table());
    let txn = translate(&set_cell_text_json("t1", 0, 1, "REVISED"));
    let edited = apply_transaction(&doc, &txn).expect("apply").0;
    let et = find_table(&edited, "t1");

    // The edited cell's paragraph carries BOTH a Deleted segment (old "r0c1")
    // and an Inserted segment (new "REVISED") — a normal in-cell tracked change.
    let para = match &et.rows[0].cells[1].blocks[0] {
        BlockNode::Paragraph(p) => p,
        _ => panic!("cell holds a paragraph"),
    };
    let has_deleted = para
        .segments
        .iter()
        .any(|s| matches!(s.status, TrackingStatus::Deleted(_)));
    let has_inserted = para
        .segments
        .iter()
        .any(|s| matches!(s.status, TrackingStatus::Inserted(_)));
    assert!(
        has_deleted && has_inserted,
        "in-cell edit must be a tracked ins/del, segments: {:?}",
        para.segments.iter().map(|s| &s.status).collect::<Vec<_>>()
    );
}

// ─── Logical-grid addressing (matches read_block.cells) ──────────────────────

#[test]
fn set_cell_text_addresses_logical_grid_column_across_a_span() {
    // Row 0: a span-2 anchor at logical col 0, then a plain cell at logical col 2.
    let mut wide = formatted_cell("t1_r0c0", "WIDE");
    wide.grid_span = 2;
    let table = TableNode {
        id: NodeId::from("t1"),
        rows: vec![
            row("t1_r0", vec![wide, plain_cell("t1_r0c2", "right")]),
            row(
                "t1_r1",
                vec![
                    plain_cell("t1_r1c0", "a"),
                    plain_cell("t1_r1c1", "b"),
                    plain_cell("t1_r1c2", "c"),
                ],
            ),
        ],
        structure_hash: String::new(),
        formatting: TableFormatting {
            borders: Some(single_borders()),
            ..TableFormatting::default()
        },
        formatting_change: None,
    };
    let doc = doc_with(table);

    // Logical col 2 in row 0 is the cell AFTER the span-2 anchor — addressing it
    // by logical column (not physical index 1) must hit "right".
    let txn = translate(&set_cell_text_json("t1", 0, 2, "RIGHT2"));
    let edited = apply_transaction(&doc, &txn).expect("apply").0;
    let mut accepted = edited.clone();
    accept_all(&mut accepted);
    let at = find_table(&accepted, "t1");
    assert_eq!(
        cell_live_text(&at.rows[0].cells[0]),
        "WIDE",
        "anchor untouched"
    );
    assert_eq!(
        cell_live_text(&at.rows[0].cells[1]),
        "RIGHT2",
        "logical col 2 resolved to the post-span cell"
    );
}

#[test]
fn set_cell_text_interior_of_merged_cell_refused() {
    // Row 0: span-2 anchor at logical col 0 (occupies cols 0 AND 1). Logical
    // col 1 is the INTERIOR of the merged cell — not the start of any cell — so
    // addressing it must fail loud, not snap to the anchor.
    let mut wide = plain_cell("t1_r0c0", "WIDE");
    wide.grid_span = 2;
    let table = TableNode {
        id: NodeId::from("t1"),
        rows: vec![
            row("t1_r0", vec![wide]),
            row(
                "t1_r1",
                vec![plain_cell("t1_r1c0", "a"), plain_cell("t1_r1c1", "b")],
            ),
        ],
        structure_hash: String::new(),
        formatting: TableFormatting::default(),
        formatting_change: None,
    };
    let doc = doc_with(table);
    let txn = translate(&set_cell_text_json("t1", 0, 1, "X"));
    let err = apply_transaction(&doc, &txn).unwrap_err();
    assert!(
        format!("{err:?}").contains("TableColumnIndexOutOfRange"),
        "interior of a merged cell must be out-of-range, got {err:?}"
    );
}

#[test]
fn set_cell_text_on_vmerge_continuation_refused() {
    // Col 0 is vertically merged: row 0 Restart (the anchor with the text),
    // row 1 Continue (no content of its own). Editing the continuation is
    // ambiguous — refuse and point at the anchor.
    let mut anchor = plain_cell("t1_r0c0", "MERGED");
    anchor.v_merge = VerticalMerge::Restart;
    let mut cont = plain_cell("t1_r1c0", "");
    cont.v_merge = VerticalMerge::Continue;
    let table = TableNode {
        id: NodeId::from("t1"),
        rows: vec![
            row("t1_r0", vec![anchor, plain_cell("t1_r0c1", "x")]),
            row("t1_r1", vec![cont, plain_cell("t1_r1c1", "y")]),
        ],
        structure_hash: String::new(),
        formatting: TableFormatting::default(),
        formatting_change: None,
    };
    let doc = doc_with(table);
    let txn = translate(&set_cell_text_json("t1", 1, 0, "X"));
    let err = apply_transaction(&doc, &txn).unwrap_err();
    assert!(
        format!("{err:?}").contains("TableCellNotEditable"),
        "vMerge continuation must be refused, got {err:?}"
    );
}

#[test]
fn set_cell_text_row_out_of_range_refused() {
    let doc = doc_with(formatted_table());
    let txn = translate(&set_cell_text_json("t1", 9, 0, "X"));
    let err = apply_transaction(&doc, &txn).unwrap_err();
    assert!(
        format!("{err:?}").contains("TableRowIndexOutOfRange"),
        "expected TableRowIndexOutOfRange, got {err:?}"
    );
}

// ─── Opaque preservation inside the cell ─────────────────────────────────────

#[test]
fn set_cell_text_dropping_a_cell_opaque_is_refused() {
    // A cell whose paragraph holds an opaque inline (e.g. a field). A plain-text
    // set_cell_text carries no opaque, so it would DROP it — that must fail loud
    // (OpaqueDestroyed), not silently delete the opaque.
    let opaque = InlineNode::from(OpaqueInlineNode {
        id: NodeId::from("t1_r0c0_field"),
        kind: OpaqueKind::Field(stemma::FieldData {
            field_kind: stemma::FieldKind::Simple,
            instruction_text: Some("PAGE".to_string()),
            result_text: Some("1".to_string()),
            semantic: None,
        }),
        opaque_ref: "field_t1_r0c0".to_string(),
        proof_ref: ProofRef {
            part: stemma::DocPart::DocumentXml,
            block_id: NodeId::from("t1_r0c0_p"),
            docx_anchor: String::new(),
        },
        wrapper_marks: Vec::new(),
        wrapper_style_props: StyleProps::default(),
        raw_xml: Some(
            br#"<w:fldSimple w:instr="PAGE"><w:r><w:t>1</w:t></w:r></w:fldSimple>"#.to_vec(),
        ),
        content_hash: None,
    });
    let mut para = text_para("t1_r0c0_p", "before ");
    if let Some(seg) = para.segments.first_mut() {
        seg.inlines.push(opaque);
    }
    let cell = TableCellNode {
        id: NodeId::from("t1_r0c0"),
        blocks: vec![BlockNode::from(para)],
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
    };
    let table = TableNode {
        id: NodeId::from("t1"),
        rows: vec![row("t1_r0", vec![cell, plain_cell("t1_r0c1", "x")])],
        structure_hash: String::new(),
        formatting: TableFormatting::default(),
        formatting_change: None,
    };
    let doc = doc_with(table);
    let txn = translate(&set_cell_text_json("t1", 0, 0, "replacement"));
    let err = apply_transaction(&doc, &txn).unwrap_err();
    assert!(
        format!("{err:?}").contains("Opaque"),
        "dropping a cell opaque must be refused (OpaqueDestroyed), got {err:?}"
    );
}
