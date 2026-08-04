//! `SetRunFormatting` verb: tracked run-formatting (w:rPrChange).
//!
//! Reference test for the `edit/verbs/<verb>.rs` pattern. Covers the authoring
//! path (EditStep), the v4 wire path (`set_format`), and the fail-loud
//! preconditions. The accept/reject assertions encode the post-conditions from
//! domain-model.md §11: accept-all keeps the new formatting, reject-all restores
//! the original.

use std::collections::HashSet;
use stemma::domain::*;
use stemma::edit::*;
use stemma::{
    ResolveSelectionAction, accept_all, enumerate_revisions, reject_all_with_styles,
    resolve_selected_revisions_with_styles,
};

// ─── Helpers (minimal, mirrors tests/edit_basic.rs) ──────────────────────────

fn make_text(id: &str, text: &str) -> InlineNode {
    InlineNode::from(TextNode {
        id: NodeId::from(id),
        text_role: None,
        text: text.to_string(),
        marks: Vec::new(),
        style_props: StyleProps::default(),
        rpr_authored: stemma::domain::RunRprAuthored::default(),
        source_run_attrs: Vec::new(),
        formatting_change: None,
    })
}

fn make_para(id: &str, segments: Vec<TrackedSegment>) -> ParagraphNode {
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
        segments,
        block_text_hash: None,
        numbering: None,
        has_direct_numbering: true,
        numbering_suppressed: false,
        materialized_numbering: None,
        rendered_text: None,
        literal_prefix: None,
        literal_prefix_trailing_rpr: None,
        literal_prefix_marks: Vec::new(),
        literal_prefix_style_props: StyleProps::default(),
        literal_prefix_rpr_authored: stemma::domain::RunRprAuthored::default(),
        literal_prefix_leading_rpr: None,
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
        paragraph_mark_rfonts: Default::default(),
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

fn make_doc(blocks: Vec<TrackedBlock>) -> CanonDoc {
    CanonDoc {
        id: NodeId::from("doc1"),
        blocks,
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

/// Single paragraph whose text is one Normal run.
fn doc_one_run(text: &str) -> CanonDoc {
    let para = make_para("p1", normal_segment(vec![make_text("p1_t1", text)]));
    make_doc(vec![normal_tracked_block(BlockNode::from(para))])
}

/// Single paragraph, one Normal run, whose run carries a preserved
/// (unmodeled) rPr remainder — e.g. an imported `w:eastAsianLayout` this
/// engine doesn't model. Never authored by an edit; only ever carried
/// through from a parsed source part (see `domain::PreservedProp`).
fn doc_one_run_with_preserved(text: &str) -> CanonDoc {
    let mut text_node = match make_text("p1_t1", text) {
        InlineNode::Text(t) => *t,
        _ => unreachable!(),
    };
    text_node.style_props.preserved = vec![PreservedProp {
        name: "w:eastAsianLayout".to_string(),
        raw_xml: r#"<w:eastAsianLayout w:combine="1"/>"#.to_string(),
    }];
    let para = make_para("p1", normal_segment(vec![InlineNode::from(text_node)]));
    make_doc(vec![normal_tracked_block(BlockNode::from(para))])
}

fn test_revision() -> RevisionInfo {
    RevisionInfo {
        revision_id: 100,
        identity: 0,
        author: Some("Test Author".to_string()),
        date: Some("2026-03-28T00:00:00Z".to_string()),
        apply_op_id: None,
    }
}

fn format_tx(block_id: &str, expect: &str, marks: InlineMarkSet) -> EditTransaction {
    EditTransaction {
        steps: vec![EditStep::SetRunFormatting {
            block_id: NodeId::from(block_id),
            expect: expect.to_string(),
            semantic_hash: None,
            marks,
            style: RunStyleEdit::default(),
            rationale: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    }
}

fn bold() -> InlineMarkSet {
    InlineMarkSet {
        bold: true,
        ..Default::default()
    }
}

fn get_para<'a>(doc: &'a CanonDoc, block_id: &str) -> &'a ParagraphNode {
    let nid = NodeId::from(block_id);
    for tb in &doc.blocks {
        if let BlockNode::Paragraph(p) = &tb.block
            && p.id == nid
        {
            return p;
        }
    }
    panic!("paragraph '{block_id}' not found");
}

fn para_text(doc: &CanonDoc, block_id: &str) -> String {
    let mut out = String::new();
    for seg in &get_para(doc, block_id).segments {
        for inl in &seg.inlines {
            if let InlineNode::Text(t) = inl {
                out.push_str(&t.text);
            }
        }
    }
    out
}

fn runs(para: &ParagraphNode) -> Vec<&TextNode> {
    para.segments
        .iter()
        .flat_map(|s| s.inlines.iter())
        .filter_map(|i| match i {
            InlineNode::Text(t) => Some(t.as_ref()),
            _ => None,
        })
        .collect()
}

fn run_with_text<'a>(para: &'a ParagraphNode, text: &str) -> Option<&'a TextNode> {
    runs(para).into_iter().find(|t| t.text == text)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[test]
fn bolds_matched_span_and_leaves_text_intact() {
    let doc = doc_one_run("The Confidential Information is protected.");
    let tx = format_tx("p1", "Confidential", bold());
    let result = apply_transaction(&doc, &tx).unwrap().0;

    // Visible text is unchanged — formatting is not a text edit.
    assert_eq!(
        para_text(&result, "p1"),
        "The Confidential Information is protected."
    );

    let para = get_para(&result, "p1");
    // The matched span was split out into its own run, bolded, with the
    // previous (empty) rPr recorded as a tracked change.
    let confidential = run_with_text(para, "Confidential").expect("split run for matched span");
    assert!(confidential.marks.contains(&Mark::Bold));
    let fc = confidential
        .formatting_change
        .as_ref()
        .expect("tracked rPrChange recorded");
    assert!(fc.previous_marks.is_empty(), "previous rPr had no bold");

    // Surrounding runs are untouched (no bold, no change).
    for t in runs(para) {
        if t.text != "Confidential" {
            assert!(!t.marks.contains(&Mark::Bold));
            assert!(t.formatting_change.is_none());
        }
    }
}

#[test]
fn accept_keeps_bold_reject_restores_original() {
    let doc = doc_one_run("The Confidential Information is protected.");
    let tx = format_tx("p1", "Confidential", bold());
    let edited = apply_transaction(&doc, &tx).unwrap().0;

    // Accept-all: the bold stays, the tracked change is resolved away.
    let mut accepted = edited.clone();
    accept_all(&mut accepted);
    let para = get_para(&accepted, "p1");
    let confidential = run_with_text(para, "Confidential").expect("bolded run survives accept");
    assert!(confidential.marks.contains(&Mark::Bold));
    assert!(runs(para).iter().all(|t| t.formatting_change.is_none()));
    assert_eq!(
        para_text(&accepted, "p1"),
        "The Confidential Information is protected."
    );

    // Reject-all: no run is bold, text intact, no residual change.
    let mut rejected = edited.clone();
    reject_all_with_styles(&mut rejected, None);
    let para = get_para(&rejected, "p1");
    assert!(runs(para).iter().all(|t| !t.marks.contains(&Mark::Bold)));
    assert!(runs(para).iter().all(|t| t.formatting_change.is_none()));
    assert_eq!(
        para_text(&rejected, "p1"),
        "The Confidential Information is protected."
    );
}

/// A tracked formatting edit (bold) on a run that also carries a preserved
/// (unmodeled) rPr remainder must not disturb that remainder on EITHER
/// projection: `SetRunFormatting` mutates specific marks/style fields in
/// place rather than replacing `style_props` wholesale, so the preserved
/// vec rides through untouched — on accept (new formatting kept) and on
/// reject (original formatting restored) alike.
#[test]
fn preserved_rpr_remainder_survives_tracked_formatting_edit_both_ways() {
    let doc = doc_one_run_with_preserved("The Confidential Information is protected.");
    let tx = format_tx("p1", "Confidential", bold());
    let edited = apply_transaction(&doc, &tx).unwrap().0;

    // Right after the edit: the matched (now-bold) run still carries the
    // preserved prop, and so does its recorded pre-edit snapshot.
    let para = get_para(&edited, "p1");
    let confidential = run_with_text(para, "Confidential").expect("split run for matched span");
    assert_eq!(
        confidential.style_props.preserved.len(),
        1,
        "preserved remainder must survive being split into its own run"
    );
    let fc = confidential
        .formatting_change
        .as_ref()
        .expect("tracked rPrChange recorded");
    assert_eq!(
        fc.previous_style_props.preserved.len(),
        1,
        "the pre-edit snapshot must also carry the preserved remainder"
    );

    // Accept-all: new (bold) formatting wins, preserved remainder stays.
    let mut accepted = edited.clone();
    accept_all(&mut accepted);
    let para = get_para(&accepted, "p1");
    let confidential = run_with_text(para, "Confidential").expect("bolded run survives accept");
    assert!(confidential.marks.contains(&Mark::Bold));
    assert_eq!(
        confidential.style_props.preserved.len(),
        1,
        "preserved remainder must survive accept-all"
    );

    // Reject-all: original (non-bold) formatting restored, preserved
    // remainder is part of that restored original and must still be there.
    let mut rejected = edited.clone();
    reject_all_with_styles(&mut rejected, None);
    let para = get_para(&rejected, "p1");
    let confidential =
        run_with_text(para, "Confidential").expect("run survives reject with original text");
    assert!(!confidential.marks.contains(&Mark::Bold));
    assert_eq!(
        confidential.style_props.preserved.len(),
        1,
        "preserved remainder must survive reject-all"
    );
}

#[test]
fn stale_expect_fails_loud() {
    let doc = doc_one_run("The Confidential Information is protected.");
    let tx = format_tx("p1", "Nonexistent Phrase", bold());
    match apply_transaction(&doc, &tx) {
        Err(EditError::ExpectMismatch { .. }) => {}
        other => panic!("expected ExpectMismatch, got {other:?}"),
    }
}

#[test]
fn empty_marks_fails_loud() {
    let doc = doc_one_run("The Confidential Information is protected.");
    let tx = format_tx("p1", "Confidential", InlineMarkSet::default());
    match apply_transaction(&doc, &tx) {
        Err(EditError::NoFormattingRequested { .. }) => {}
        other => panic!("expected NoFormattingRequested, got {other:?}"),
    }
}

#[test]
fn span_crossing_an_opaque_is_not_matched() {
    // "see " <opaque> " the clause" — the match must lie within one contiguous
    // run, so a phrase spanning the opaque is not found (fail loud).
    let para = make_para(
        "p1",
        normal_segment(vec![
            make_text("p1_a", "see "),
            InlineNode::from(OpaqueInlineNode {
                id: NodeId::from("p1_op"),
                kind: OpaqueKind::Drawing,
                opaque_ref: "opaque_p1_op".to_string(),
                proof_ref: ProofRef {
                    part: DocPart::DocumentXml,
                    block_id: NodeId::from("p1"),
                    docx_anchor: String::new(),
                },
                wrapper_marks: Vec::new(),
                wrapper_style_props: StyleProps::default(),
                source_run_attrs: Vec::new(),
                joins_following_text_run: false,
                raw_xml: Some(b"<w:drawing/>".to_vec()),
                content_hash: None,
            }),
            make_text("p1_b", " the clause"),
        ]),
    );
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);
    let tx = format_tx("p1", "see  the clause", bold());
    match apply_transaction(&doc, &tx) {
        Err(EditError::ExpectMismatch { .. }) => {}
        other => panic!("expected ExpectMismatch for cross-opaque span, got {other:?}"),
    }
}

#[test]
fn v4_set_format_wire_path() {
    use stemma::edit_v4::parse_transaction;

    let doc = doc_one_run("The Confidential Information is protected.");
    let json = r#"{
        "ops": [{
            "op": "set_format",
            "target": "p1",
            "expect": "Confidential",
            "marks": [{"type": "bold"}]
        }],
        "revision": {"author": "Agent"}
    }"#;
    let v4 = parse_transaction(json).expect("schema valid");
    let txn = v4.into_edit_transaction().expect("adapter ok");
    let result = apply_transaction(&doc, &txn).unwrap().0;

    let para = get_para(&result, "p1");
    let confidential = run_with_text(para, "Confidential").expect("bolded run");
    assert!(confidential.marks.contains(&Mark::Bold));
    assert!(confidential.formatting_change.is_some());
}

// ─── run-formatting-extended: value-bearing rPr (color/highlight/font) ─────────

fn format_style_tx(block_id: &str, expect: &str, style: RunStyleEdit) -> EditTransaction {
    EditTransaction {
        steps: vec![EditStep::SetRunFormatting {
            block_id: NodeId::from(block_id),
            expect: expect.to_string(),
            semantic_hash: None,
            marks: InlineMarkSet::default(),
            style,
            rationale: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    }
}

fn color_highlight_font() -> RunStyleEdit {
    RunStyleEdit {
        color: Some("FF0000".into()),
        highlight: Some(HighlightColor::Yellow),
        font_family: Some("Arial".into()),
        font_size_half_points: Some(28),
        char_spacing: None,
    }
}

#[test]
fn sets_value_bearing_props_as_tracked_rpr_change() {
    let doc = doc_one_run("The Confidential Information is protected.");
    let tx = format_style_tx("p1", "Confidential", color_highlight_font());
    let result = apply_transaction(&doc, &tx).unwrap().0;

    let para = get_para(&result, "p1");
    let confidential = run_with_text(para, "Confidential").expect("split run for matched span");
    // The live rPr carries the new value-bearing properties.
    assert_eq!(confidential.style_props.color.as_deref(), Some("FF0000"));
    assert_eq!(
        confidential.style_props.highlight,
        Some(HighlightColor::Yellow)
    );
    assert_eq!(
        confidential.style_props.font_family.as_deref(),
        Some("Arial")
    );
    assert_eq!(confidential.style_props.font_size, Some(28));
    // The previous (empty) rPr is recorded as the tracked change snapshot.
    let fc = confidential
        .formatting_change
        .as_ref()
        .expect("tracked rPrChange recorded");
    assert_eq!(fc.previous_style_props, StyleProps::default());

    // Surrounding runs are untouched.
    for t in runs(para) {
        if t.text != "Confidential" {
            assert!(t.style_props.color.is_none());
            assert!(t.style_props.highlight.is_none());
            assert!(t.formatting_change.is_none());
        }
    }
}

#[test]
fn accept_keeps_value_props_reject_restores_originals() {
    let doc = doc_one_run("The Confidential Information is protected.");
    let tx = format_style_tx("p1", "Confidential", color_highlight_font());
    let edited = apply_transaction(&doc, &tx).unwrap().0;

    // Accept-all keeps the new value-bearing props and clears the change.
    let mut accepted = edited.clone();
    accept_all(&mut accepted);
    let conf =
        run_with_text(get_para(&accepted, "p1"), "Confidential").expect("run survives accept");
    assert_eq!(conf.style_props.color.as_deref(), Some("FF0000"));
    assert_eq!(conf.style_props.highlight, Some(HighlightColor::Yellow));
    assert_eq!(conf.style_props.font_family.as_deref(), Some("Arial"));
    assert_eq!(conf.style_props.font_size, Some(28));
    assert!(conf.formatting_change.is_none());

    // Reject-all restores the original (empty) StyleProps on every run.
    let mut rejected = edited;
    reject_all_with_styles(&mut rejected, None);
    for t in runs(get_para(&rejected, "p1")) {
        assert_eq!(
            t.style_props,
            StyleProps::default(),
            "reject-all must restore original StyleProps on run '{}'",
            t.text
        );
        assert!(t.formatting_change.is_none());
    }
}

#[test]
fn invalid_color_fails_loud() {
    let doc = doc_one_run("The Confidential Information is protected.");
    let tx = format_style_tx(
        "p1",
        "Confidential",
        RunStyleEdit {
            color: Some("not-a-color".into()),
            ..Default::default()
        },
    );
    match apply_transaction(&doc, &tx) {
        Err(EditError::InvalidColorValue { .. }) => {}
        other => panic!("expected InvalidColorValue, got {other:?}"),
    }
}

#[test]
fn zero_font_size_fails_loud() {
    let doc = doc_one_run("The Confidential Information is protected.");
    let tx = format_style_tx(
        "p1",
        "Confidential",
        RunStyleEdit {
            font_size_half_points: Some(0),
            ..Default::default()
        },
    );
    match apply_transaction(&doc, &tx) {
        Err(EditError::InvalidFontSize { .. }) => {}
        other => panic!("expected InvalidFontSize, got {other:?}"),
    }
}

#[test]
fn empty_marks_and_empty_style_fails_loud() {
    let doc = doc_one_run("The Confidential Information is protected.");
    let tx = format_style_tx("p1", "Confidential", RunStyleEdit::default());
    match apply_transaction(&doc, &tx) {
        Err(EditError::NoFormattingRequested { .. }) => {}
        other => panic!("expected NoFormattingRequested, got {other:?}"),
    }
}

#[test]
fn v4_set_format_wire_path_with_value_props() {
    use stemma::edit_v4::parse_transaction;

    let doc = doc_one_run("The Confidential Information is protected.");
    let json = r#"{
        "ops": [{
            "op": "set_format",
            "target": "p1",
            "expect": "Confidential",
            "marks": [],
            "color": "FF0000",
            "highlight": "yellow",
            "font_family": "Arial",
            "font_size_half_points": 28
        }],
        "revision": {"author": "Agent"}
    }"#;
    let v4 = parse_transaction(json).expect("schema valid");
    let txn = v4.into_edit_transaction().expect("adapter ok");
    let result = apply_transaction(&doc, &txn).unwrap().0;

    let conf = run_with_text(get_para(&result, "p1"), "Confidential").expect("styled run");
    assert_eq!(conf.style_props.color.as_deref(), Some("FF0000"));
    assert_eq!(conf.style_props.highlight, Some(HighlightColor::Yellow));
    assert_eq!(conf.style_props.font_family.as_deref(), Some("Arial"));
    assert_eq!(conf.style_props.font_size, Some(28));
    assert!(conf.formatting_change.is_some());
}

#[test]
fn v4_set_format_unknown_highlight_fails_loud() {
    use stemma::edit_v4::parse_transaction;

    let json = r#"{
        "ops": [{
            "op": "set_format",
            "target": "p1",
            "expect": "Confidential",
            "marks": [],
            "highlight": "chartreuse"
        }],
        "revision": {"author": "Agent"}
    }"#;
    let v4 = parse_transaction(json).expect("schema valid");
    match v4.into_edit_transaction() {
        Err(stemma::edit_v4::AdapterError::UnsupportedHighlightColor { .. }) => {}
        other => panic!("expected UnsupportedHighlightColor, got {other:?}"),
    }
}

#[test]
fn v4_set_format_invalid_color_fails_loud() {
    use stemma::edit_v4::parse_transaction;

    let json = r##"{
        "ops": [{
            "op": "set_format",
            "target": "p1",
            "expect": "Confidential",
            "marks": [],
            "color": "#FF0000"
        }],
        "revision": {"author": "Agent"}
    }"##;
    let v4 = parse_transaction(json).expect("schema valid");
    match v4.into_edit_transaction() {
        Err(stemma::edit_v4::AdapterError::InvalidColorValue { .. }) => {}
        other => panic!("expected InvalidColorValue, got {other:?}"),
    }
}

#[test]
fn v4_set_format_empty_request_is_schema_rejected() {
    use stemma::edit_v4::{SchemaError, parse_transaction};

    let json = r#"{
        "ops": [{
            "op": "set_format",
            "target": "p1",
            "expect": "Confidential",
            "marks": []
        }],
        "revision": {"author": "Agent"}
    }"#;
    match parse_transaction(json) {
        Err(SchemaError::EmptyFormatMarks { .. }) => {}
        other => panic!("expected EmptyFormatMarks, got {other:?}"),
    }
}

#[test]
fn v4_object_marks_can_turn_bold_off_and_reject_restores_directness() {
    use stemma::edit_v4::parse_transaction;

    let mut text = match make_text("p1_t1", "Bold term") {
        InlineNode::Text(text) => *text,
        _ => unreachable!(),
    };
    text.marks.push(Mark::Bold);
    text.rpr_authored.bold = true;
    let para = make_para("p1", normal_segment(vec![InlineNode::from(text)]));
    let base = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);
    let wire = r#"{
        "ops": [{
            "op": "set_format",
            "target": "p1",
            "expect": "Bold",
            "marks": {"bold": false}
        }],
        "revision": {"author": "Agent"}
    }"#;
    let txn = parse_transaction(wire)
        .expect("object-form marks parse")
        .into_edit_transaction()
        .expect("object-form marks translate");
    let edited = apply_transaction(&base, &txn).expect("bold-off applies").0;
    let current = run_with_text(get_para(&edited, "p1"), "Bold").expect("target run");
    assert!(!current.marks.contains(&Mark::Bold));
    assert!(current.rpr_authored.bold);
    assert!(current.rpr_authored.bold_off);
    assert!(current.formatting_change.is_some());

    let mut accepted = edited.clone();
    accept_all(&mut accepted);
    let accepted_run = run_with_text(get_para(&accepted, "p1"), "Bold").unwrap();
    assert!(!accepted_run.marks.contains(&Mark::Bold));
    assert!(accepted_run.rpr_authored.bold_off);

    let mut rejected = edited;
    reject_all_with_styles(&mut rejected, None);
    let rejected_run = run_with_text(get_para(&rejected, "p1"), "Bold").unwrap();
    assert!(rejected_run.marks.contains(&Mark::Bold));
    assert!(rejected_run.rpr_authored.bold);
    assert!(!rejected_run.rpr_authored.bold_off);
}

#[test]
fn v4_object_marks_cover_all_eight_tri_state_properties() {
    use stemma::edit_v4::parse_transaction;

    let wire = r#"{
        "ops": [{
            "op": "set_format",
            "target": "p1",
            "expect": "term",
            "marks": {
                "bold": false,
                "italic": true,
                "underline": false,
                "strike": true,
                "subscript": false,
                "superscript": true,
                "caps": false,
                "small_caps": true
            }
        }],
        "revision": {"author": "Agent"}
    }"#;
    let txn = parse_transaction(wire)
        .expect("all object marks parse")
        .into_edit_transaction()
        .expect("all object marks translate");
    let EditStep::SetRunFormatting { marks, .. } = &txn.steps[0] else {
        panic!("expected run-format step")
    };
    assert!(marks.bold_off);
    assert!(marks.italic);
    assert!(marks.underline_off);
    assert!(marks.strike);
    assert!(marks.subscript_off);
    assert!(marks.superscript);
    assert!(marks.caps_off);
    assert!(marks.small_caps);
}

#[test]
fn v4_refuses_mixed_and_empty_object_mark_forms() {
    use stemma::edit_v4::{SchemaError, parse_transaction};

    let mixed = r#"{
        "ops": [{
            "op": "set_format",
            "target": "p1",
            "expect": "term",
            "marks": {"bold": true},
            "caps": false
        }],
        "revision": {"author": "Agent"}
    }"#;
    assert!(matches!(
        parse_transaction(mixed),
        Err(SchemaError::MixedFormatMarkForms { .. })
    ));

    let empty = r#"{
        "ops": [{
            "op": "set_format",
            "target": "p1",
            "expect": "term",
            "marks": {}
        }],
        "revision": {"author": "Agent"}
    }"#;
    assert!(matches!(
        parse_transaction(empty),
        Err(SchemaError::EmptyFormatMarks { .. })
    ));
}

#[test]
fn duplicate_format_expect_refuses_without_mutation() {
    let base = doc_one_run("term and term");
    let error = apply_transaction(&base, &format_tx("p1", "term", bold()))
        .expect_err("duplicate target must refuse");
    assert!(matches!(
        error,
        EditError::AmbiguousFormatTarget { occurrences: 2, .. }
    ));
    assert_eq!(para_text(&base, "p1"), "term and term");
    assert!(
        runs(get_para(&base, "p1"))
            .iter()
            .all(|run| run.formatting_change.is_none())
    );
}

#[test]
fn no_effect_substring_refuses_without_splitting_the_run() {
    let mut text = match make_text("p1_t1", "prefix target suffix") {
        InlineNode::Text(text) => *text,
        _ => unreachable!(),
    };
    text.marks.push(Mark::Bold);
    text.rpr_authored.bold = true;
    let para = make_para("p1", normal_segment(vec![InlineNode::from(text)]));
    let base = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);
    let before = base.clone();

    let error = apply_transaction(&base, &format_tx("p1", "target", bold()))
        .expect_err("reasserting the exact direct format is a no-op");
    assert!(matches!(error, EditError::NoFormattingChange { .. }));
    assert_eq!(base, before, "a refused no-op leaves the source untouched");
    assert_eq!(runs(get_para(&base, "p1")).len(), 1);
}

#[test]
fn span_refuses_when_any_covered_run_has_an_independent_format_revision() {
    let alpha = make_text("alpha", "Alpha ");
    let mut beta = match make_text("beta", "Beta") {
        InlineNode::Text(text) => *text,
        _ => unreachable!(),
    };
    beta.formatting_change = Some(FormattingChange {
        revision_id: 41,
        identity: 4100,
        previous_marks: Vec::new(),
        previous_style_props: StyleProps::default(),
        previous_rpr_authored: RunRprAuthored::default(),
        author: "Earlier reviewer".to_string(),
        date: None,
    });
    let para = make_para("p1", normal_segment(vec![alpha, InlineNode::from(beta)]));
    let base = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);
    let transaction = EditTransaction {
        steps: vec![
            EditStep::SetRunFormatting {
                block_id: NodeId::from("p1"),
                expect: "Alpha".to_string(),
                semantic_hash: None,
                marks: bold(),
                style: RunStyleEdit::default(),
                rationale: None,
            },
            EditStep::SetRunFormatting {
                block_id: NodeId::from("p1"),
                expect: "Alpha Beta".to_string(),
                semantic_hash: None,
                marks: InlineMarkSet {
                    italic: true,
                    ..Default::default()
                },
                style: RunStyleEdit::default(),
                rationale: None,
            },
        ],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    let error = apply_transaction(&base, &transaction)
        .expect_err("one same-transaction run cannot mask a later conflict");
    assert!(matches!(
        error,
        EditError::FormatRevisionConflict {
            existing_revision,
            ..
        } if existing_revision.identity == 4100
    ));
    assert_eq!(
        runs(get_para(&base, "p1"))[0].formatting_change,
        None,
        "the whole transaction rolls back"
    );
}

#[test]
fn unrelated_pending_segment_does_not_lock_normal_format_target() {
    let inserted_revision = RevisionInfo {
        revision_id: 7,
        identity: 77,
        author: Some("Other".to_string()),
        date: None,
        apply_op_id: None,
    };
    let para = make_para(
        "p1",
        vec![
            TrackedSegment {
                status: TrackingStatus::Normal,
                inlines: vec![make_text("normal", "target")],
            },
            TrackedSegment {
                status: TrackingStatus::Inserted(inserted_revision.clone()),
                inlines: vec![make_text("inserted", " unrelated")],
            },
        ],
    );
    let base = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);
    let mut direct = format_tx("p1", "target", bold());
    direct.materialization_mode = MaterializationMode::Direct;
    let direct_edited = apply_transaction(&base, &direct)
        .expect("unrelated pending text does not lock a normal direct target")
        .0;
    assert!(
        run_with_text(get_para(&direct_edited, "p1"), "target")
            .unwrap()
            .marks
            .contains(&Mark::Bold)
    );
    assert_eq!(
        get_para(&direct_edited, "p1").segments[1].status,
        TrackingStatus::Inserted(inserted_revision.clone())
    );

    let edited = apply_transaction(&base, &format_tx("p1", "target", bold()))
        .expect("normal target remains independently editable")
        .0;
    assert!(
        run_with_text(get_para(&edited, "p1"), "target")
            .unwrap()
            .marks
            .contains(&Mark::Bold)
    );
    assert_eq!(
        get_para(&edited, "p1").segments[1].status,
        TrackingStatus::Inserted(inserted_revision)
    );
}

#[test]
fn first_format_revision_on_pending_insertion_is_independently_resolvable() {
    let insertion = RevisionInfo {
        revision_id: 7,
        identity: 70,
        author: Some("Inserter".to_string()),
        date: None,
        apply_op_id: None,
    };
    let para = make_para(
        "p1",
        vec![TrackedSegment {
            status: TrackingStatus::Inserted(insertion.clone()),
            inlines: vec![make_text("target", "target")],
        }],
    );
    let base = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);
    let edited = apply_transaction(&base, &format_tx("p1", "target", bold()))
        .expect("Word-proven first format proposal on inserted text applies")
        .0;

    let records = enumerate_revisions(&edited);
    let format_identity = records
        .iter()
        .find(|record| record.kind == stemma::RevisionKind::FormatRun)
        .map(|record| record.revision_id)
        .expect("format revision is independently enumerable");
    assert_ne!(format_identity, insertion.identity);

    let mut accepted = edited.clone();
    resolve_selected_revisions_with_styles(
        &mut accepted,
        &HashSet::from([format_identity]),
        ResolveSelectionAction::Accept,
        None,
    )
    .expect("accept only format revision");
    let accepted_segment = &get_para(&accepted, "p1").segments[0];
    assert_eq!(
        accepted_segment.status,
        TrackingStatus::Inserted(insertion.clone())
    );
    let accepted_run = run_with_text(get_para(&accepted, "p1"), "target").unwrap();
    assert!(accepted_run.marks.contains(&Mark::Bold));
    assert!(accepted_run.formatting_change.is_none());

    let mut rejected = edited;
    resolve_selected_revisions_with_styles(
        &mut rejected,
        &HashSet::from([format_identity]),
        ResolveSelectionAction::Reject,
        None,
    )
    .expect("reject only format revision");
    let rejected_segment = &get_para(&rejected, "p1").segments[0];
    assert_eq!(rejected_segment.status, TrackingStatus::Inserted(insertion));
    let rejected_run = run_with_text(get_para(&rejected, "p1"), "target").unwrap();
    assert!(!rejected_run.marks.contains(&Mark::Bold));
    assert!(rejected_run.formatting_change.is_none());
}

#[test]
fn pending_deletion_format_target_has_a_specific_refusal() {
    let deletion = RevisionInfo {
        revision_id: 8,
        identity: 80,
        author: Some("Deleter".to_string()),
        date: None,
        apply_op_id: None,
    };
    let para = make_para(
        "p1",
        vec![TrackedSegment {
            status: TrackingStatus::Deleted(deletion),
            inlines: vec![make_text("target", "target")],
        }],
    );
    let base = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);
    let error = apply_transaction(&base, &format_tx("p1", "target", bold()))
        .expect_err("formatting deleted text remains outside the authoring contract");
    assert!(matches!(
        error,
        EditError::FormatTargetNotEditable {
            tracking_status,
            ..
        } if tracking_status == "deleted"
    ));
}

#[test]
fn detail_view_exposes_pending_format_revision_on_exact_span() {
    let base = doc_one_run("target tail");
    let edited = apply_transaction(&base, &format_tx("p1", "target", bold()))
        .expect("format proposal applies")
        .0;
    let view = stemma::view::build_document_view_from_canon(&edited);
    let target = view.blocks[0]
        .segments
        .iter()
        .find_map(|segment| match segment {
            stemma::view::SegmentView::Text {
                text,
                format_revision,
                ..
            } if text == "target" => format_revision.as_ref(),
            _ => None,
        })
        .expect("target span carries format revision metadata");
    assert_ne!(target.revision_id, 0, "read exposes stable identity");
    assert_eq!(target.author.as_deref(), Some("Test Author"));
}

#[test]
fn pending_insertion_with_format_revision_reports_both_identities() {
    let base = doc_one_run("target");
    let mut pending = apply_transaction(&base, &format_tx("p1", "target", bold()))
        .expect("first format proposal applies")
        .0;
    let container = RevisionInfo {
        revision_id: 40,
        identity: 400,
        author: Some("Inserter".to_string()),
        date: None,
        apply_op_id: None,
    };
    let BlockNode::Paragraph(paragraph) = &mut pending.blocks[0].block else {
        unreachable!()
    };
    paragraph.segments[0].status = TrackingStatus::Inserted(container.clone());
    let before = pending.clone();

    let error = apply_transaction(
        &pending,
        &format_tx(
            "p1",
            "target",
            InlineMarkSet {
                italic: true,
                ..Default::default()
            },
        ),
    )
    .expect_err("a second format proposal on inserted text refuses");
    let EditError::FormatRevisionConflict {
        existing_revision,
        container_revision,
        tracking_status,
        ..
    } = error
    else {
        panic!("expected typed format conflict")
    };
    assert_eq!(tracking_status, "inserted");
    assert_ne!(existing_revision.identity, 0);
    assert_eq!(*container_revision, Some(container));
    assert_eq!(pending, before, "conflict refuses before mutation");
}

/// Marks are a set, so the model must hold exactly one representation of a
/// given set. Import emits them in declaration order; applying formatting in a
/// different order must reach the same vector, or an edited document stops
/// comparing equal to its own reopened form.
#[test]
fn marks_reach_canonical_order_whatever_order_they_are_applied() {
    use stemma::domain::Mark;

    let ordered = vec![Mark::Bold, Mark::Italic, Mark::Underline];
    let mut sorted = ordered.clone();
    sorted.sort();
    assert_eq!(
        ordered, sorted,
        "declaration order is the canonical order the model stores"
    );

    // Underline applied to an already bold+italic run, then italic applied to a
    // bold+underline run: both are the same set and must be the same vector.
    let mut applied_last = vec![Mark::Bold, Mark::Italic];
    if let Err(at) = applied_last.binary_search(&Mark::Underline) {
        applied_last.insert(at, Mark::Underline);
    }
    let mut applied_middle = vec![Mark::Bold, Mark::Underline];
    if let Err(at) = applied_middle.binary_search(&Mark::Italic) {
        applied_middle.insert(at, Mark::Italic);
    }
    assert_eq!(
        applied_last, applied_middle,
        "the same mark set must have one representation regardless of the order \
         the marks were applied in"
    );
}
