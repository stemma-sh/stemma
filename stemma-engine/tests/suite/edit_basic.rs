//! Edit engine tests: basic roundtrip correctness (Category 1),
//! preserved inline preservation (Category 2), precondition failures (Category 3),
//! formatting fidelity (Category 4), tracked change structure (Category 5),
//! multi-step transactions (Category 7), edge cases (Category 8).

use stemma::domain::*;
use stemma::edit::*;
use stemma::{accept_all, reject_all_with_styles};

// ─── Test helpers ────────────────────────────────────────────────────────────

/// Create a minimal ParagraphNode with the given id and segments.
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

/// Create a simple text node.
fn make_text(id: &str, text: &str) -> InlineNode {
    InlineNode::from(TextNode {
        id: NodeId::from(id),
        text_role: None,
        text: text.to_string(),
        marks: Vec::new(),
        style_props: StyleProps::default(),
        rpr_authored: stemma::domain::RunRprAuthored::default(),
        formatting_change: None,
    })
}

/// Create a text node with specific marks.
fn make_text_with_marks(id: &str, text: &str, marks: Vec<Mark>) -> InlineNode {
    InlineNode::from(TextNode {
        id: NodeId::from(id),
        text_role: None,
        text: text.to_string(),
        marks,
        style_props: StyleProps::default(),
        rpr_authored: stemma::domain::RunRprAuthored::default(),
        formatting_change: None,
    })
}

/// Create an opaque inline node.
fn make_opaque(id: &str) -> InlineNode {
    InlineNode::from(OpaqueInlineNode {
        id: NodeId::from(id),
        kind: OpaqueKind::Drawing,
        opaque_ref: format!("opaque_{id}"),
        proof_ref: ProofRef {
            part: DocPart::DocumentXml,
            block_id: NodeId::from("p1"),
            docx_anchor: String::new(),
        },
        wrapper_marks: Vec::new(),
        wrapper_style_props: StyleProps::default(),
        raw_xml: Some(b"<w:drawing/>".to_vec()),
        content_hash: None,
    })
}

/// Create a hard break node.
fn make_hard_break(id: &str) -> InlineNode {
    InlineNode::HardBreak(HardBreakNode {
        id: NodeId::from(id),
        break_type: BreakType::TextWrapping,
    })
}

/// Create a hyperlink opaque inline node.
fn make_hyperlink(id: &str, url: &str, text: &str) -> InlineNode {
    InlineNode::from(OpaqueInlineNode {
        id: NodeId::from(id),
        kind: OpaqueKind::Hyperlink(HyperlinkData {
            url: Some(url.to_string()),
            anchor: None,
            text: text.to_string(),
            r_id: Some("rId1".to_string()),
            runs: vec![HyperlinkRun {
                text: text.to_string(),
                rpr_xml: None,
                status: TrackingStatus::Normal,
            }],
            extra_attrs: vec![],
        }),
        opaque_ref: format!("hyperlink_{id}"),
        proof_ref: ProofRef {
            part: DocPart::DocumentXml,
            block_id: NodeId::from("p1"),
            docx_anchor: String::new(),
        },
        wrapper_marks: Vec::new(),
        wrapper_style_props: StyleProps::default(),
        raw_xml: Some(
            b"<w:hyperlink r:id=\"rId1\"><w:r><w:t>link</w:t></w:r></w:hyperlink>".to_vec(),
        ),
        content_hash: None,
    })
}

/// Create a field opaque inline node.
fn make_field(id: &str, kind: FieldKind) -> InlineNode {
    InlineNode::from(OpaqueInlineNode {
        id: NodeId::from(id),
        kind: OpaqueKind::Field(FieldData {
            field_kind: kind,
            instruction_text: Some("PAGE".to_string()),
            result_text: Some("1".to_string()),
            semantic: None,
        }),
        opaque_ref: format!("field_{id}"),
        proof_ref: ProofRef {
            part: DocPart::DocumentXml,
            block_id: NodeId::from("p1"),
            docx_anchor: String::new(),
        },
        wrapper_marks: Vec::new(),
        wrapper_style_props: StyleProps::default(),
        raw_xml: Some(b"<w:fldChar w:fldCharType=\"begin\"/>".to_vec()),
        content_hash: None,
    })
}

/// Create an SDT opaque inline node.
fn make_sdt(id: &str) -> InlineNode {
    InlineNode::from(OpaqueInlineNode {
        id: NodeId::from(id),
        kind: OpaqueKind::Sdt,
        opaque_ref: format!("sdt_{id}"),
        proof_ref: ProofRef {
            part: DocPart::DocumentXml,
            block_id: NodeId::from("p1"),
            docx_anchor: String::new(),
        },
        wrapper_marks: Vec::new(),
        wrapper_style_props: StyleProps::default(),
        raw_xml: Some(
            b"<w:sdt><w:sdtContent><w:r><w:t>content</w:t></w:r></w:sdtContent></w:sdt>".to_vec(),
        ),
        content_hash: None,
    })
}

/// Create a decoration (bookmark) node.
#[allow(dead_code)]
fn make_decoration(id: &str) -> InlineNode {
    InlineNode::from(DecorationNode {
        id: NodeId::from(id),
        kind: DecorationType::Bookmark,
        opaque_ref: format!("bm_{id}"),
        proof_ref: ProofRef {
            part: DocPart::DocumentXml,
            block_id: NodeId::from("p1"),
            docx_anchor: String::new(),
        },
        wrapper_marks: Vec::new(),
        wrapper_style_props: Default::default(),
        raw_xml: Some(b"<w:bookmarkStart/>".to_vec()),
        origin: None,
    })
}

/// Create a CanonDoc with the given blocks.
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

/// Create a simple single-paragraph CanonDoc with Normal text.
fn make_simple_doc(para_id: &str, text: &str) -> CanonDoc {
    let para = make_para(
        para_id,
        normal_segment(vec![make_text(&format!("{para_id}_t1"), text)]),
    );
    make_doc(vec![normal_tracked_block(BlockNode::from(para))])
}

/// Standard revision info for tests.
fn test_revision() -> RevisionInfo {
    RevisionInfo {
        revision_id: 100,
        identity: 0,
        author: Some("Test Author".to_string()),
        date: Some("2026-03-28T00:00:00Z".to_string()),
        apply_op_id: None,
    }
}

/// Create a simple replace transaction.
fn replace_transaction(block_id: &str, expect: &str, content: ParagraphContent) -> EditTransaction {
    EditTransaction {
        steps: vec![EditStep::ReplaceParagraphText {
            block_id: NodeId::from(block_id),
            rationale: None,
            replacement_role: None,
            expect: expect.to_string(),
            semantic_hash: None,
            content,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    }
}

/// Simple text-only paragraph content (no preserved inlines).
fn text_content(text: &str) -> ParagraphContent {
    ParagraphContent {
        fragments: vec![ContentFragment::Text(text.to_string())],
    }
}

/// Get the visible text of a paragraph after accept_all.
fn accepted_text(doc: &CanonDoc, block_id: &str) -> String {
    let mut doc = doc.clone();
    accept_all(&mut doc);
    para_text(&doc, block_id)
}

/// Get the visible text of a paragraph after reject_all.
fn rejected_text(doc: &CanonDoc, block_id: &str) -> String {
    let mut doc = doc.clone();
    reject_all_with_styles(&mut doc, None);
    para_text(&doc, block_id)
}

/// Get the full visible text of a paragraph (all segments, ignoring status).
fn para_text(doc: &CanonDoc, block_id: &str) -> String {
    let block = doc
        .blocks
        .iter()
        .find(|tb| match &tb.block {
            BlockNode::Paragraph(p) => p.id == NodeId::from(block_id),
            _ => false,
        })
        .expect("block not found");
    match &block.block {
        BlockNode::Paragraph(p) => {
            let mut text = String::new();
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::Text(t) = inline {
                        text.push_str(&t.text);
                    }
                }
            }
            text
        }
        _ => panic!("not a paragraph"),
    }
}

fn all_para_texts(doc: &CanonDoc) -> Vec<String> {
    doc.blocks
        .iter()
        .filter_map(|tb| match &tb.block {
            BlockNode::Paragraph(p) => Some(
                p.segments
                    .iter()
                    .flat_map(|seg| seg.inlines.iter())
                    .filter_map(|inline| match inline {
                        InlineNode::Text(t) => Some(t.text.as_str()),
                        _ => None,
                    })
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect()
}

/// Get the paragraph node from a doc by block_id.
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

// ─── Category 1: Roundtrip correctness ──────────────────────────────────────

#[test]
fn replace_simple_text_accept_yields_new_text() {
    // I2: accept_all(edited_doc) produces the replacement text
    let doc = make_simple_doc("p1", "hello world");
    let tx = replace_transaction("p1", "hello", text_content("goodbye world"));
    let result = apply_transaction(&doc, &tx).unwrap().0;
    assert_eq!(accepted_text(&result, "p1"), "goodbye world");
}

#[test]
fn replace_simple_text_reject_yields_old_text() {
    // I3: reject_all(edited_doc) produces the original text
    let doc = make_simple_doc("p1", "hello world");
    let tx = replace_transaction("p1", "hello", text_content("goodbye world"));
    let result = apply_transaction(&doc, &tx).unwrap().0;
    assert_eq!(rejected_text(&result, "p1"), "hello world");
}

#[test]
fn replace_word_in_middle() {
    let doc = make_simple_doc("p1", "the quick brown fox");
    let tx = replace_transaction("p1", "quick", text_content("the slow brown fox"));
    let result = apply_transaction(&doc, &tx).unwrap().0;

    assert_eq!(accepted_text(&result, "p1"), "the slow brown fox");
    assert_eq!(rejected_text(&result, "p1"), "the quick brown fox");
}

#[test]
fn replace_adds_text() {
    let doc = make_simple_doc("p1", "hello world");
    let tx = replace_transaction("p1", "hello", text_content("hello beautiful world"));
    let result = apply_transaction(&doc, &tx).unwrap().0;

    assert_eq!(accepted_text(&result, "p1"), "hello beautiful world");
    assert_eq!(rejected_text(&result, "p1"), "hello world");
}

#[test]
fn replace_removes_text() {
    let doc = make_simple_doc("p1", "hello beautiful world");
    let tx = replace_transaction("p1", "beautiful", text_content("hello world"));
    let result = apply_transaction(&doc, &tx).unwrap().0;

    assert_eq!(accepted_text(&result, "p1"), "hello world");
    assert_eq!(rejected_text(&result, "p1"), "hello beautiful world");
}

// ─── Category 2: Preserved inline preservation ─────────────────────────────

#[test]
fn replace_preserves_opaque_inline() {
    // I1: OpaqueInlineNode survives editing
    let opaque = make_opaque("op1");
    let para = make_para(
        "p1",
        normal_segment(vec![
            make_text("t1", "before "),
            opaque.clone(),
            make_text("t2", " after"),
        ]),
    );
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    let content = ParagraphContent {
        fragments: vec![
            ContentFragment::Text("modified ".to_string()),
            ContentFragment::PreservedInlineRef(NodeId::from("op1")),
            ContentFragment::Text(" changed".to_string()),
        ],
    };
    let tx = replace_transaction("p1", "before", content);
    let result = apply_transaction(&doc, &tx).unwrap().0;

    // Check the opaque survived
    let para = get_para(&result, "p1");
    let has_opaque = para.segments.iter().any(|s| {
        s.inlines
            .iter()
            .any(|i| matches!(i, InlineNode::OpaqueInline(o) if o.id == NodeId::from("op1")))
    });
    assert!(has_opaque, "opaque inline must survive editing");

    // Check roundtrip
    assert_eq!(accepted_text(&result, "p1"), "modified  changed");
    assert_eq!(rejected_text(&result, "p1"), "before  after");
}

#[test]
fn replace_preserves_hard_break() {
    // I1: HardBreakNode survives editing
    let hb = make_hard_break("hb1");
    let para = make_para(
        "p1",
        normal_segment(vec![
            make_text("t1", "line one"),
            hb.clone(),
            make_text("t2", "line two"),
        ]),
    );
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    let content = ParagraphContent {
        fragments: vec![
            ContentFragment::Text("first line".to_string()),
            ContentFragment::PreservedInlineRef(NodeId::from("hb1")),
            ContentFragment::Text("second line".to_string()),
        ],
    };
    let tx = replace_transaction("p1", "line one", content);
    let result = apply_transaction(&doc, &tx).unwrap().0;

    let para = get_para(&result, "p1");
    let has_hb = para.segments.iter().any(|s| {
        s.inlines
            .iter()
            .any(|i| matches!(i, InlineNode::HardBreak(h) if h.id == NodeId::from("hb1")))
    });
    assert!(has_hb, "hard break must survive editing");
}

#[test]
fn replace_missing_opaque_fails() {
    let para = make_para(
        "p1",
        normal_segment(vec![
            make_text("t1", "before "),
            make_opaque("op1"),
            make_text("t2", " after"),
        ]),
    );
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    // Content that drops the opaque
    let content = text_content("before after");
    let tx = replace_transaction("p1", "before", content);
    let err = apply_transaction(&doc, &tx).unwrap_err();
    assert!(
        matches!(err, EditError::OpaqueDestroyed { .. }),
        "expected OpaqueDestroyed, got: {err}"
    );
}

/// Spec test: dropping an opaque footnote-ref token must return the
/// `OpaqueDestroyed` typed error with the exact missing id, the
/// matching inline kind, and a paragraph preview — not a generic
/// range error. This typed-error wire shape is the contract the LLM
/// retry loop and the Python backend branch on.
#[test]
fn replace_destroying_opaque_inline_returns_typed_error() {
    // Paragraph: "See Section 5[opaque op_1] for details."
    // The LLM proposes "See Section 5 for details." — dropping op_1.
    let para = make_para(
        "p_7",
        normal_segment(vec![
            make_text("t1", "See Section 5"),
            make_opaque("op_1"),
            make_text("t2", " for details."),
        ]),
    );
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    let content = text_content("See Section 5 for details.");
    let tx = replace_transaction("p_7", "See Section 5", content);
    let err = apply_transaction(&doc, &tx).unwrap_err();

    let EditError::OpaqueDestroyed {
        step_index,
        target_block_id,
        missing_opaque_ids,
        missing_inline_kinds,
        original_text_preview,
    } = err
    else {
        panic!("expected OpaqueDestroyed, got: {err}");
    };
    assert_eq!(step_index, 0, "first step is at index 0");
    assert_eq!(
        target_block_id,
        NodeId::from("p_7"),
        "target_block_id must be the paragraph whose opaque is destroyed"
    );
    assert_eq!(
        missing_opaque_ids,
        vec!["op_1".to_string()],
        "the dropped opaque's id must be reported"
    );
    // make_opaque() constructs OpaqueKind::Drawing, so the kind label
    // must be "drawing" — not a generic "opaque" catch-all. The specific
    // discriminant is what the retry prompt needs to understand what
    // kind of node was destroyed.
    assert_eq!(
        missing_inline_kinds,
        vec!["drawing"],
        "kind label must reflect the specific OpaqueKind discriminant"
    );
    assert!(
        original_text_preview.contains("See Section 5"),
        "preview must contain paragraph text, got: {original_text_preview:?}"
    );
    // Preview renders the opaque using its kind label (`[footnote]`-style),
    // as the typed validation-error wire shape specifies. The node ID is not
    // in the preview; it travels separately in missing_opaque_ids above.
    assert!(
        original_text_preview.contains("[drawing]"),
        "preview must render the opaque with its kind label, got: {original_text_preview:?}"
    );
}

/// When multiple opaques are dropped in a single replace, the typed
/// error must report ALL of them — not just the first — so the retry
/// loop can fix the content in one pass.
#[test]
fn replace_destroying_multiple_opaques_reports_all_missing_ids() {
    let para = make_para(
        "p_1",
        normal_segment(vec![
            make_text("t1", "alpha "),
            make_opaque("op_1"),
            make_text("t2", " beta "),
            make_opaque("op_2"),
            make_text("t3", " gamma"),
        ]),
    );
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    // Replacement drops BOTH opaques
    let content = text_content("alpha beta gamma");
    let tx = replace_transaction("p_1", "alpha", content);
    let err = apply_transaction(&doc, &tx).unwrap_err();

    let EditError::OpaqueDestroyed {
        missing_opaque_ids, ..
    } = err
    else {
        panic!("expected OpaqueDestroyed, got: {err}");
    };
    assert_eq!(
        missing_opaque_ids,
        vec!["op_1".to_string(), "op_2".to_string()],
        "both destroyed opaques must be reported, in original order"
    );
}

#[test]
fn replace_nonexistent_inline_ref_fails() {
    let doc = make_simple_doc("p1", "hello world");

    let content = ParagraphContent {
        fragments: vec![
            ContentFragment::Text("hello ".to_string()),
            ContentFragment::PreservedInlineRef(NodeId::from("nonexistent")),
            ContentFragment::Text(" world".to_string()),
        ],
    };
    let tx = replace_transaction("p1", "hello", content);
    let err = apply_transaction(&doc, &tx).unwrap_err();
    assert!(
        matches!(err, EditError::PreservedInlineNotFound { .. }),
        "expected PreservedInlineNotFound, got: {err}"
    );
}

#[test]
fn replace_duplicate_inline_ref_fails() {
    let para = make_para(
        "p1",
        normal_segment(vec![
            make_text("t1", "a "),
            make_opaque("op1"),
            make_text("t2", " b"),
        ]),
    );
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    let content = ParagraphContent {
        fragments: vec![
            ContentFragment::PreservedInlineRef(NodeId::from("op1")),
            ContentFragment::Text(" middle ".to_string()),
            ContentFragment::PreservedInlineRef(NodeId::from("op1")),
        ],
    };
    let tx = replace_transaction("p1", "a", content);
    let err = apply_transaction(&doc, &tx).unwrap_err();
    assert!(
        matches!(err, EditError::DuplicatePreservedInlineRef { .. }),
        "expected DuplicatePreservedInlineRef, got: {err}"
    );
}

#[test]
fn replace_reordered_inlines_fails() {
    let para = make_para(
        "p1",
        normal_segment(vec![
            make_text("t1", "a "),
            make_opaque("op1"),
            make_text("t2", " b "),
            make_opaque("op2"),
            make_text("t3", " c"),
        ]),
    );
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    // Swap the order of op1 and op2
    let content = ParagraphContent {
        fragments: vec![
            ContentFragment::Text("a ".to_string()),
            ContentFragment::PreservedInlineRef(NodeId::from("op2")),
            ContentFragment::Text(" b ".to_string()),
            ContentFragment::PreservedInlineRef(NodeId::from("op1")),
            ContentFragment::Text(" c".to_string()),
        ],
    };
    let tx = replace_transaction("p1", "a", content);
    let err = apply_transaction(&doc, &tx).unwrap_err();
    assert!(
        matches!(err, EditError::PreservedInlineOrderChanged { .. }),
        "expected PreservedInlineOrderChanged, got: {err}"
    );
}

#[test]
fn replace_ref_to_text_node_fails() {
    let doc = make_simple_doc("p1", "hello world");

    // Reference the text node by its ID
    let content = ParagraphContent {
        fragments: vec![ContentFragment::PreservedInlineRef(NodeId::from("p1_t1"))],
    };
    let tx = replace_transaction("p1", "hello", content);
    let err = apply_transaction(&doc, &tx).unwrap_err();
    assert!(
        matches!(err, EditError::NotAPreservedInline { .. }),
        "expected NotAPreservedInline, got: {err}"
    );
}

// ─── Category 3: Precondition failures ──────────────────────────────────────

#[test]
fn replace_block_not_found() {
    let doc = make_simple_doc("p1", "hello");
    let tx = replace_transaction("p999", "hello", text_content("goodbye"));
    let err = apply_transaction(&doc, &tx).unwrap_err();
    assert!(
        matches!(err, EditError::BlockNotFound { .. }),
        "expected BlockNotFound, got: {err}"
    );
}

#[test]
fn replace_not_a_paragraph() {
    let table = BlockNode::from(TableNode {
        id: NodeId::from("tbl1"),
        rows: vec![],
        structure_hash: String::new(),
        formatting: TableFormatting::default(),
        formatting_change: None,
    });
    let doc = make_doc(vec![normal_tracked_block(table)]);
    let tx = replace_transaction("tbl1", "text", text_content("new"));
    let err = apply_transaction(&doc, &tx).unwrap_err();
    assert!(
        matches!(
            err,
            EditError::NotAParagraph {
                actual_kind: "table",
                ..
            }
        ),
        "expected NotAParagraph(table), got: {err}"
    );
}

#[test]
fn replace_expect_mismatch() {
    let doc = make_simple_doc("p1", "hello world");
    let tx = replace_transaction("p1", "nonexistent substring", text_content("new"));
    let err = apply_transaction(&doc, &tx).unwrap_err();
    assert!(
        matches!(err, EditError::ExpectMismatch { .. }),
        "expected ExpectMismatch, got: {err}"
    );
}

#[test]
fn replace_deleted_block_fails() {
    let para = make_para("p1", normal_segment(vec![make_text("t1", "hello")]));
    let doc = make_doc(vec![TrackedBlock {
        status: TrackingStatus::Deleted(test_revision()),
        block: BlockNode::from(para),
        move_id: None,
        block_sdt_wrap: None,
    }]);
    let tx = replace_transaction("p1", "hello", text_content("new"));
    let err = apply_transaction(&doc, &tx).unwrap_err();
    assert!(
        matches!(
            err,
            EditError::BlockHasTrackedStatus {
                status: "deleted",
                ..
            }
        ),
        "expected BlockHasTrackedStatus(deleted), got: {err}"
    );
}

/// A TRACKED re-edit of a paragraph that is itself a PENDING block insertion must
/// NOT accept the insertion for the user — the block stays Inserted and the text
/// edit stacks within it. (DIRECT mode still auto-resolves; see the
/// `_in_direct_mode_` test below.) accept-all keeps the block with the new text;
/// reject-all rejects the still-pending insertion, removing the block.
#[test]
fn replace_inserted_block_keeps_it_pending_in_tracked_mode() {
    let para = make_para("p1", normal_segment(vec![make_text("t1", "hello")]));
    let doc = make_doc(vec![TrackedBlock {
        status: TrackingStatus::Inserted(test_revision()),
        block: BlockNode::from(para),
        move_id: None,
        block_sdt_wrap: None,
    }]);
    let tx = replace_transaction("p1", "hello", text_content("new"));
    let result = apply_transaction(&doc, &tx).unwrap().0;
    assert!(
        matches!(result.blocks[0].status, TrackingStatus::Inserted(_)),
        "the pending block insertion is preserved, not auto-accepted"
    );
    assert_eq!(
        accepted_text(&result, "p1"),
        "new",
        "accept-all = the new text"
    );
    let mut rejected = result.clone();
    reject_all_with_styles(&mut rejected, None);
    assert!(
        !rejected
            .blocks
            .iter()
            .any(|tb| matches!(&tb.block, BlockNode::Paragraph(p) if p.id == NodeId::from("p1"))),
        "reject-all rejects the still-pending inserted block — the block is removed"
    );
}

#[test]
fn replace_inserted_block_in_direct_mode_rewrites_current_working_text() {
    let para = make_para(
        "p1",
        normal_segment(vec![make_text("t1", "hello inserted")]),
    );
    let doc = make_doc(vec![TrackedBlock {
        status: TrackingStatus::Inserted(test_revision()),
        block: BlockNode::from(para),
        move_id: None,
        block_sdt_wrap: None,
    }]);
    let mut tx = replace_transaction("p1", "hello", text_content("rewritten directly"));
    tx.materialization_mode = MaterializationMode::Direct;

    let result = apply_transaction(&doc, &tx).unwrap().0;

    assert_eq!(para_text(&result, "p1"), "rewritten directly");
    assert_eq!(accepted_text(&result, "p1"), "rewritten directly");
    assert_eq!(rejected_text(&result, "p1"), "rewritten directly");
    assert!(matches!(result.blocks[0].status, TrackingStatus::Normal));
    let BlockNode::Paragraph(para) = &result.blocks[0].block else {
        panic!("expected paragraph block");
    };
    assert!(
        para.segments
            .iter()
            .all(|segment| matches!(segment.status, TrackingStatus::Normal)),
        "direct edit should project tracked segments to normal"
    );
}

/// v3: replacing a paragraph that already has tracked segments (mix of
/// Normal + Inserted + Deleted) auto-resolves them by accepting all
/// changes first. The "accepted" view ("kept inserted") becomes the
/// base text, and the new replacement diffs against that.
#[test]
fn replace_paragraph_with_existing_tracked_segments_auto_resolves_and_applies() {
    let segments = vec![
        TrackedSegment {
            status: TrackingStatus::Normal,
            inlines: vec![make_text("t1", "kept ")],
        },
        TrackedSegment {
            status: TrackingStatus::Inserted(test_revision()),
            inlines: vec![make_text("t2", "inserted")],
        },
    ];
    let para = make_para("p1", segments);
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);
    let tx = replace_transaction("p1", "kept", text_content("new"));
    let result = apply_transaction(&doc, &tx).unwrap().0;
    assert_eq!(
        accepted_text(&result, "p1"),
        "new",
        "after auto-resolve + replace, accepting gives the new text"
    );
}

#[test]
fn replace_paragraph_with_existing_tracked_segments_in_direct_mode_projects_working_surface() {
    let segments = vec![
        TrackedSegment {
            status: TrackingStatus::Normal,
            inlines: vec![make_text("t1", "kept ")],
        },
        TrackedSegment {
            status: TrackingStatus::Deleted(test_revision()),
            inlines: vec![make_text("t2", "old ")],
        },
        TrackedSegment {
            status: TrackingStatus::Inserted(test_revision()),
            inlines: vec![make_text("t3", "new")],
        },
    ];
    let para = make_para("p1", segments);
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);
    let mut tx = replace_transaction("p1", "kept", text_content("fully rewritten"));
    tx.materialization_mode = MaterializationMode::Direct;

    let result = apply_transaction(&doc, &tx).unwrap().0;

    assert_eq!(para_text(&result, "p1"), "fully rewritten");
    assert_eq!(accepted_text(&result, "p1"), "fully rewritten");
    assert_eq!(rejected_text(&result, "p1"), "fully rewritten");
    let BlockNode::Paragraph(para) = &result.blocks[0].block else {
        panic!("expected paragraph block");
    };
    assert!(
        para.segments
            .iter()
            .all(|segment| matches!(segment.status, TrackingStatus::Normal)),
        "direct edit should leave no tracked paragraph segments behind"
    );
}

#[test]
fn expect_spanning_anchor_boundary_fails() {
    // "before after" spans across the opaque — should fail
    let para = make_para(
        "p1",
        normal_segment(vec![
            make_text("t1", "before"),
            make_opaque("op1"),
            make_text("t2", "after"),
        ]),
    );
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    let content = ParagraphContent {
        fragments: vec![
            ContentFragment::Text("before".to_string()),
            ContentFragment::PreservedInlineRef(NodeId::from("op1")),
            ContentFragment::Text("after".to_string()),
        ],
    };
    let tx = replace_transaction("p1", "beforeafter", content);
    let err = apply_transaction(&doc, &tx).unwrap_err();
    assert!(
        matches!(err, EditError::ExpectMismatch { .. }),
        "expected ExpectMismatch for cross-anchor expect, got: {err}"
    );
}

// ─── Category 4: Formatting fidelity ────────────────────────────────────────

#[test]
fn unchanged_text_preserves_marks() {
    // I4: in a real edit, the KEPT portion of the paragraph retains its
    // original marks and style_props. (A whole-paragraph replace to identical
    // text is now a NoOpEdit — see `identity_replacement_fails_loud_as_no_op`;
    // I4 is about the kept text in a genuine edit.)
    let bold_text = make_text_with_marks("t1", "bold text stays", vec![Mark::Bold]);
    let para = make_para("p1", normal_segment(vec![bold_text]));
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    // A real edit: append " here". Plain-text content inherits the bold
    // exemplar, so the kept words stay bold and the inserted word does too.
    let tx = replace_transaction("p1", "bold", text_content("bold text stays here"));
    let result = apply_transaction(&doc, &tx).unwrap().0;

    let para = get_para(&result, "p1");
    for seg in &para.segments {
        for inline in &seg.inlines {
            if let InlineNode::Text(t) = inline
                && !t.text.is_empty()
            {
                assert!(
                    t.marks.contains(&Mark::Bold),
                    "kept/inserted bold text should carry Bold mark, but '{}' has marks: {:?}",
                    t.text,
                    t.marks
                );
            }
        }
    }
}

#[test]
fn same_text_plain_replace_over_bold_unformats_as_tracked_change() {
    // A SAME-TEXT all-plain replace over a MARKED paragraph is an editor UN-FORMAT
    // (the content specifies no marks where the run is bold), not a no-op: it
    // produces a surgical tracked rPrChange that REMOVES the mark. Reject restores
    // the bold. (A genuine no-op — plain over a PLAIN paragraph — still fails loud:
    // identity_replacement_fails_loud_as_no_op. A genuine text EDIT over bold still
    // INHERITS the bold: unchanged_text_preserves_marks / I4. The distinguishing
    // factor is whether the text changed.)
    let bold_text = make_text_with_marks("t1", "bold text stays", vec![Mark::Bold]);
    let para = make_para("p1", normal_segment(vec![bold_text]));
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    let tx = replace_transaction("p1", "bold", text_content("bold text stays"));
    let result = apply_transaction(&doc, &tx)
        .expect("un-format (plain over bold, same text) applies as a tracked change")
        .0;

    let para = get_para(&result, "p1");
    let run = para
        .segments
        .iter()
        .flat_map(|s| &s.inlines)
        .find_map(|i| match i {
            InlineNode::Text(t) if !t.text.is_empty() => Some(t),
            _ => None,
        })
        .expect("a text run");
    assert!(
        !run.marks.contains(&Mark::Bold),
        "un-format removed Bold, got {:?}",
        run.marks
    );
    let fc = run
        .formatting_change
        .as_ref()
        .expect("the removal is recorded as a tracked rPrChange");
    assert!(
        fc.previous_marks.contains(&Mark::Bold),
        "previous_marks HAD Bold (reject restores it), got {:?}",
        fc.previous_marks
    );
}

#[test]
fn new_text_inside_bold_run_inherits_bold() {
    // Left-sibling inheritance: new text inherits from preceding kept/deleted text
    let para = make_para(
        "p1",
        normal_segment(vec![make_text_with_marks(
            "t1",
            "bold text here",
            vec![Mark::Bold],
        )]),
    );
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    let tx = replace_transaction("p1", "bold", text_content("bold new text here"));
    let result = apply_transaction(&doc, &tx).unwrap().0;

    // Check that inserted text inherited Bold
    let para = get_para(&result, "p1");
    for seg in &para.segments {
        if matches!(seg.status, TrackingStatus::Inserted(_)) {
            for inline in &seg.inlines {
                if let InlineNode::Text(t) = inline {
                    assert!(
                        t.marks.contains(&Mark::Bold),
                        "inserted text '{}' should inherit Bold from left sibling, \
                         but has marks: {:?}",
                        t.text,
                        t.marks
                    );
                }
            }
        }
    }
}

#[test]
fn new_text_at_start_inherits_from_right() {
    // When there's no left context, inherit from right neighbor
    let para = make_para(
        "p1",
        normal_segment(vec![make_text_with_marks(
            "t1",
            "italic text",
            vec![Mark::Italic],
        )]),
    );
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    let tx = replace_transaction("p1", "italic", text_content("prefix italic text"));
    let result = apply_transaction(&doc, &tx).unwrap().0;

    let para = get_para(&result, "p1");
    for seg in &para.segments {
        if matches!(seg.status, TrackingStatus::Inserted(_)) {
            for inline in &seg.inlines {
                if let InlineNode::Text(t) = inline {
                    assert!(
                        t.marks.contains(&Mark::Italic),
                        "inserted text '{}' at start should inherit Italic from right, \
                         but has marks: {:?}",
                        t.text,
                        t.marks
                    );
                }
            }
        }
    }
}

// ─── Category 5: Tracked change structure ───────────────────────────────────

#[test]
fn replace_produces_correct_tracked_segments() {
    let doc = make_simple_doc("p1", "hello world");
    let tx = replace_transaction("p1", "hello", text_content("goodbye world"));
    let result = apply_transaction(&doc, &tx).unwrap().0;

    let para = get_para(&result, "p1");

    // Should have: deleted "hello" + inserted "goodbye" + normal " world"
    // (or similar structure depending on exact diff alignment)
    let has_deleted = para
        .segments
        .iter()
        .any(|s| matches!(s.status, TrackingStatus::Deleted(_)));
    let has_inserted = para
        .segments
        .iter()
        .any(|s| matches!(s.status, TrackingStatus::Inserted(_)));
    let has_normal = para
        .segments
        .iter()
        .any(|s| matches!(s.status, TrackingStatus::Normal));

    assert!(has_deleted, "should have Deleted segments");
    assert!(has_inserted, "should have Inserted segments");
    assert!(has_normal, "should have Normal segments for unchanged text");
}

#[test]
fn no_empty_segments() {
    // I7: every TrackedSegment has at least one InlineNode
    let doc = make_simple_doc("p1", "hello world");
    let tx = replace_transaction("p1", "hello", text_content("goodbye world"));
    let result = apply_transaction(&doc, &tx).unwrap().0;

    let para = get_para(&result, "p1");
    for (i, seg) in para.segments.iter().enumerate() {
        assert!(
            !seg.inlines.is_empty(),
            "segment {i} is empty (status: {:?})",
            seg.status
        );
    }
}

#[test]
fn adjacent_same_status_segments_are_merged() {
    // I8: consecutive segments with identical status are merged
    let doc = make_simple_doc("p1", "aaa bbb ccc");
    let tx = replace_transaction("p1", "aaa", text_content("xxx yyy zzz"));
    let result = apply_transaction(&doc, &tx).unwrap().0;

    let para = get_para(&result, "p1");

    // Check no two adjacent segments have the same status
    for pair in para.segments.windows(2) {
        assert_ne!(
            pair[0].status, pair[1].status,
            "adjacent segments should not have identical status: {:?} and {:?}",
            pair[0].status, pair[1].status
        );
    }
}

#[test]
fn revision_ids_are_unique() {
    let doc = make_simple_doc("p1", "hello world");
    let tx = replace_transaction("p1", "hello", text_content("goodbye cruel world"));
    let result = apply_transaction(&doc, &tx).unwrap().0;

    let para = get_para(&result, "p1");
    let mut rev_ids: Vec<u32> = Vec::new();
    for seg in &para.segments {
        match &seg.status {
            TrackingStatus::Inserted(rev) | TrackingStatus::Deleted(rev) => {
                rev_ids.push(rev.revision_id);
            }
            _ => {}
        }
    }

    let unique: std::collections::HashSet<u32> = rev_ids.iter().copied().collect();
    assert_eq!(
        rev_ids.len(),
        unique.len(),
        "revision IDs must be unique, got: {rev_ids:?}"
    );
}

// ─── Category 7: Multi-step transactions ────────────────────────────────────

#[test]
fn two_replaces_on_different_paragraphs() {
    let p1 = make_para("p1", normal_segment(vec![make_text("t1", "first para")]));
    let p2 = make_para("p2", normal_segment(vec![make_text("t2", "second para")]));
    let doc = make_doc(vec![
        normal_tracked_block(BlockNode::from(p1)),
        normal_tracked_block(BlockNode::from(p2)),
    ]);

    let tx = EditTransaction {
        steps: vec![
            EditStep::ReplaceParagraphText {
                block_id: NodeId::from("p1"),
                rationale: None,
                replacement_role: None,
                expect: "first".to_string(),
                semantic_hash: None,
                content: text_content("modified first para"),
            },
            EditStep::ReplaceParagraphText {
                block_id: NodeId::from("p2"),
                rationale: None,
                replacement_role: None,
                expect: "second".to_string(),
                semantic_hash: None,
                content: text_content("modified second para"),
            },
        ],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    let result = apply_transaction(&doc, &tx).unwrap().0;
    assert_eq!(accepted_text(&result, "p1"), "modified first para");
    assert_eq!(accepted_text(&result, "p2"), "modified second para");
    assert_eq!(rejected_text(&result, "p1"), "first para");
    assert_eq!(rejected_text(&result, "p2"), "second para");
}

#[test]
fn step2_depends_on_step1_output() {
    // Step 1 changes the paragraph, step 2 uses the modified text as expect
    let doc = make_simple_doc("p1", "original text");

    let tx = EditTransaction {
        steps: vec![
            EditStep::ReplaceParagraphText {
                block_id: NodeId::from("p1"),
                rationale: None,
                replacement_role: None,
                expect: "original".to_string(),
                semantic_hash: None,
                content: text_content("intermediate text"),
            },
            // This step's expect matches the result of step 1 after accept
            // But wait — the paragraph now has tracked changes from step 1,
            // so step 2 should fail (ParagraphContainsTrackedSegments)
        ],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    let result = apply_transaction(&doc, &tx);
    // Step 1 introduces tracked changes, so if we tried a step 2 on the same
    // paragraph, it would fail. This tests that step 1 succeeds alone.
    assert!(result.is_ok());
}

#[test]
fn failing_step_rejects_entire_transaction() {
    let doc = make_simple_doc("p1", "hello world");

    let tx = EditTransaction {
        steps: vec![
            EditStep::ReplaceParagraphText {
                block_id: NodeId::from("p1"),
                rationale: None,
                replacement_role: None,
                expect: "hello".to_string(),
                semantic_hash: None,
                content: text_content("goodbye world"),
            },
            EditStep::ReplaceParagraphText {
                block_id: NodeId::from("p999"),
                rationale: None,
                replacement_role: None,
                expect: "nope".to_string(),
                semantic_hash: None,
                content: text_content("won't work"),
            },
        ],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    let err = apply_transaction(&doc, &tx).unwrap_err();
    assert!(matches!(
        err,
        EditError::BlockNotFound { step_index: 1, .. }
    ));

    // Original document should be untouched (we verify by checking the original
    // doc is still the same — apply_transaction takes &CanonDoc, not &mut)
    assert_eq!(para_text(&doc, "p1"), "hello world");
}

#[test]
fn insert_after_adds_inserted_paragraph_in_order() {
    let p1 = make_para("p1", normal_segment(vec![make_text("t1", "first para")]));
    let p2 = make_para("p2", normal_segment(vec![make_text("t2", "second para")]));
    let doc = make_doc(vec![
        normal_tracked_block(BlockNode::from(p1)),
        normal_tracked_block(BlockNode::from(p2)),
    ]);

    let tx = EditTransaction {
        steps: vec![EditStep::InsertParagraphs {
            anchor_block_id: NodeId::from("p1"),
            position: InsertPosition::After,
            rationale: None,
            blocks: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
                role: Some("body_text".to_string()),
                content: parse_paragraph_markup("inserted para").unwrap(),
                restart_numbering: false,
                list: None,
            })],
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    let result = apply_transaction(&doc, &tx).unwrap().0;
    assert_eq!(
        all_para_texts(&result),
        vec!["first para", "inserted para", "second para"]
    );
    assert!(matches!(
        result.blocks[1].status,
        TrackingStatus::Inserted(_)
    ));

    let mut accepted = result.clone();
    accept_all(&mut accepted);
    assert_eq!(
        all_para_texts(&accepted),
        vec!["first para", "inserted para", "second para"]
    );

    let mut rejected = result;
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(all_para_texts(&rejected), vec!["first para", "second para"]);
}

#[test]
fn insert_toc_block_creates_generated_simple_field_paragraph() {
    let p1 = make_para("p1", normal_segment(vec![make_text("t1", "first para")]));
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(p1))]);

    let tx = EditTransaction {
        steps: vec![EditStep::InsertParagraphs {
            anchor_block_id: NodeId::from("p1"),
            position: InsertPosition::After,
            rationale: None,
            blocks: vec![BlockSpec::Toc(TocBlockSpec {
                role: Some("body_text".to_string()),
                levels: TocLevelsSpec { from: 1, to: 3 },
                include_hyperlinks: true,
                hide_page_numbers_in_web: true,
                use_outline_levels: true,
            })],
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    let result = apply_transaction(&doc, &tx).unwrap().0;
    let inserted = result
        .blocks
        .iter()
        .find_map(|tb| {
            if matches!(tb.status, TrackingStatus::Inserted(_))
                && let BlockNode::Paragraph(p) = &tb.block
            {
                Some(p)
            } else {
                None
            }
        })
        .expect("inserted TOC paragraph present");

    let fields: Vec<&FieldData> = inserted
        .segments
        .iter()
        .flat_map(|s| s.inlines.iter())
        .filter_map(|inline| match inline {
            InlineNode::OpaqueInline(opaque) => match &opaque.kind {
                OpaqueKind::Field(field) => Some(field),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(fields.len(), 1, "expected one generated TOC field");
    assert_eq!(fields[0].field_kind, FieldKind::Simple);
    assert_eq!(
        fields[0].instruction_text.as_deref(),
        Some("TOC \\o \"1-3\" \\h \\z \\u")
    );
    assert_eq!(
        fields[0].semantic,
        Some(FieldSemantic::Toc(TocFieldSpec {
            levels: TocLevelsSpec { from: 1, to: 3 },
            include_hyperlinks: true,
            hide_page_numbers_in_web: true,
            use_outline_levels: true,
        }))
    );
}

#[test]
fn insert_with_bold_mark_produces_styled_text_node() {
    // Universal inline mark: <bold>LOUD</bold> soft → two TextNodes: one with
    // Mark::Bold, one plain.
    let p1 = make_para("p1", normal_segment(vec![make_text("t1", "first para")]));
    let p2 = make_para("p2", normal_segment(vec![make_text("t2", "second para")]));
    let doc = make_doc(vec![
        normal_tracked_block(BlockNode::from(p1)),
        normal_tracked_block(BlockNode::from(p2)),
    ]);

    let tx = EditTransaction {
        steps: vec![EditStep::InsertParagraphs {
            anchor_block_id: NodeId::from("p1"),
            position: InsertPosition::After,
            rationale: None,
            blocks: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
                role: Some("body_text".to_string()),
                content: parse_paragraph_markup("<bold>LOUD</bold> soft").unwrap(),
                restart_numbering: false,
                list: None,
            })],
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    let result = apply_transaction(&doc, &tx).unwrap().0;
    let inserted = result
        .blocks
        .iter()
        .find_map(|tb| {
            if matches!(tb.status, TrackingStatus::Inserted(_))
                && let BlockNode::Paragraph(p) = &tb.block
            {
                Some(p)
            } else {
                None
            }
        })
        .expect("inserted paragraph present");

    let text_nodes: Vec<&TextNode> = inserted
        .segments
        .iter()
        .flat_map(|s| s.inlines.iter())
        .filter_map(|i| match i {
            InlineNode::Text(t) => Some(t.as_ref()),
            _ => None,
        })
        .collect();
    assert_eq!(
        text_nodes.len(),
        2,
        "expected two TextNodes (bold + plain), got {}: {text_nodes:?}",
        text_nodes.len()
    );
    assert_eq!(text_nodes[0].text, "LOUD");
    assert!(
        text_nodes[0].marks.contains(&Mark::Bold),
        "first TextNode must carry Mark::Bold, got: {:?}",
        text_nodes[0].marks
    );
    assert_eq!(text_nodes[1].text, " soft");
    assert!(
        !text_nodes[1].marks.contains(&Mark::Bold),
        "second TextNode must not carry Mark::Bold, got: {:?}",
        text_nodes[1].marks
    );
}

#[test]
fn insert_with_strike_applies_style_props() {
    // <strike> sets StyleProps.strike = MarkValue::On (not a Mark variant).
    let p1 = make_para("p1", normal_segment(vec![make_text("t1", "first para")]));
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(p1))]);

    let tx = EditTransaction {
        steps: vec![EditStep::InsertParagraphs {
            anchor_block_id: NodeId::from("p1"),
            position: InsertPosition::After,
            rationale: None,
            blocks: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
                role: Some("body_text".to_string()),
                content: parse_paragraph_markup("<strike>gone</strike>").unwrap(),
                restart_numbering: false,
                list: None,
            })],
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    let result = apply_transaction(&doc, &tx).unwrap().0;
    let inserted = result.blocks.iter().find_map(|tb| {
        if matches!(tb.status, TrackingStatus::Inserted(_))
            && let BlockNode::Paragraph(p) = &tb.block
        {
            Some(p)
        } else {
            None
        }
    });
    let inserted = inserted.expect("inserted paragraph present");
    let text = inserted
        .segments
        .iter()
        .flat_map(|s| s.inlines.iter())
        .find_map(|i| match i {
            InlineNode::Text(t) => Some(t),
            _ => None,
        })
        .expect("text node present");
    assert_eq!(text.text, "gone");
    assert_eq!(text.style_props.strike, MarkValue::On);
}

#[test]
fn insert_rejects_preserved_inline_ref() {
    // Inserts create new paragraphs; referencing an existing `<opaque>` by
    // id is meaningless because there's no source paragraph.
    let p1 = make_para("p1", normal_segment(vec![make_text("t1", "first para")]));
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(p1))]);

    let tx = EditTransaction {
        steps: vec![EditStep::InsertParagraphs {
            anchor_block_id: NodeId::from("p1"),
            position: InsertPosition::After,
            rationale: None,
            blocks: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
                role: Some("body_text".to_string()),
                content: parse_paragraph_markup(r#"hello <opaque id="op1"/> world"#).unwrap(),
                restart_numbering: false,
                list: None,
            })],
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    let err = apply_transaction(&doc, &tx).unwrap_err();
    assert!(
        matches!(err, EditError::UnsupportedParagraphStructure { .. }),
        "expected UnsupportedParagraphStructure, got: {err}"
    );
}

#[test]
fn delete_range_marks_blocks_deleted() {
    let p1 = make_para("p1", normal_segment(vec![make_text("t1", "first para")]));
    let p2 = make_para("p2", normal_segment(vec![make_text("t2", "second para")]));
    let p3 = make_para("p3", normal_segment(vec![make_text("t3", "third para")]));
    let doc = make_doc(vec![
        normal_tracked_block(BlockNode::from(p1)),
        normal_tracked_block(BlockNode::from(p2)),
        normal_tracked_block(BlockNode::from(p3)),
    ]);

    let tx = EditTransaction {
        steps: vec![EditStep::DeleteBlockRange {
            from_block_id: NodeId::from("p1"),
            to_block_id: NodeId::from("p2"),
            rationale: None,
            expect: "first".to_string(),
            semantic_hash: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    let result = apply_transaction(&doc, &tx).unwrap().0;
    assert!(matches!(
        result.blocks[0].status,
        TrackingStatus::Deleted(_)
    ));
    assert!(matches!(
        result.blocks[1].status,
        TrackingStatus::Deleted(_)
    ));
    assert_eq!(
        all_para_texts(&result),
        vec!["first para", "second para", "third para"]
    );

    let mut accepted = result.clone();
    accept_all(&mut accepted);
    assert_eq!(all_para_texts(&accepted), vec!["third para"]);

    let mut rejected = result;
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        all_para_texts(&rejected),
        vec!["first para", "second para", "third para"]
    );
}

#[test]
fn replace_block_range_with_marks_falls_back_to_structural() {
    // Single-block-to-single-block replacement would normally take the
    // inline-diff path, but mark-bearing replacements must fall back to
    // structural replace (block delete + block insert) so the marks are
    // applied to the new TextNodes via resolve_paragraph_spec. The old
    // block ends up Deleted, the new block Inserted with Mark::Bold.
    let p1 = make_para("p1", normal_segment(vec![make_text("t1", "hello world")]));
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(p1))]);

    let tx = EditTransaction {
        steps: vec![EditStep::ReplaceBlockRange {
            from_block_id: NodeId::from("p1"),
            to_block_id: NodeId::from("p1"),
            rationale: None,
            expect: "hello".to_string(),
            semantic_hash: None,
            blocks: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
                role: Some("body_text".to_string()),
                content: parse_paragraph_markup("<bold>important</bold> notice").unwrap(),
                restart_numbering: false,
                list: None,
            })],
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    let result = apply_transaction(&doc, &tx).unwrap().0;
    // Structural replace: original p1 deleted, new block inserted after.
    assert!(matches!(
        result.blocks[0].status,
        TrackingStatus::Deleted(_)
    ));
    let inserted_block = result.blocks.iter().skip(1).find_map(|tb| {
        if matches!(tb.status, TrackingStatus::Inserted(_))
            && let BlockNode::Paragraph(p) = &tb.block
        {
            Some(p)
        } else {
            None
        }
    });
    let inserted = inserted_block.expect("structural replace must insert a new paragraph");
    let text_nodes: Vec<&TextNode> = inserted
        .segments
        .iter()
        .flat_map(|s| s.inlines.iter())
        .filter_map(|i| match i {
            InlineNode::Text(t) => Some(t.as_ref()),
            _ => None,
        })
        .collect();
    assert_eq!(text_nodes.len(), 2);
    assert_eq!(text_nodes[0].text, "important");
    assert!(
        text_nodes[0].marks.contains(&Mark::Bold),
        "first TextNode must be bold, got {:?}",
        text_nodes[0].marks
    );
    assert_eq!(text_nodes[1].text, " notice");
    assert!(!text_nodes[1].marks.contains(&Mark::Bold));
}

#[test]
fn replace_block_range_structurally_deletes_old_and_inserts_new_blocks() {
    let p1 = make_para("p1", normal_segment(vec![make_text("t1", "first para")]));
    let p2 = make_para("p2", normal_segment(vec![make_text("t2", "second para")]));
    let p3 = make_para("p3", normal_segment(vec![make_text("t3", "third para")]));
    let doc = make_doc(vec![
        normal_tracked_block(BlockNode::from(p1)),
        normal_tracked_block(BlockNode::from(p2)),
        normal_tracked_block(BlockNode::from(p3)),
    ]);

    let tx = EditTransaction {
        steps: vec![EditStep::ReplaceBlockRange {
            from_block_id: NodeId::from("p1"),
            to_block_id: NodeId::from("p2"),
            rationale: None,
            expect: "first".to_string(),
            semantic_hash: None,
            blocks: vec![
                BlockSpec::Paragraph(ParagraphBlockSpec {
                    role: Some("body_text".to_string()),
                    content: parse_paragraph_markup("merged para one").unwrap(),
                    restart_numbering: false,
                    list: None,
                }),
                BlockSpec::Paragraph(ParagraphBlockSpec {
                    role: Some("body_text".to_string()),
                    content: parse_paragraph_markup("merged para two").unwrap(),
                    restart_numbering: false,
                    list: None,
                }),
            ],
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    let result = apply_transaction(&doc, &tx).unwrap().0;
    assert!(matches!(
        result.blocks[0].status,
        TrackingStatus::Deleted(_)
    ));
    assert!(matches!(
        result.blocks[1].status,
        TrackingStatus::Deleted(_)
    ));
    assert!(matches!(
        result.blocks[2].status,
        TrackingStatus::Inserted(_)
    ));
    assert!(matches!(
        result.blocks[3].status,
        TrackingStatus::Inserted(_)
    ));

    let mut accepted = result.clone();
    accept_all(&mut accepted);
    assert_eq!(
        all_para_texts(&accepted),
        vec!["merged para one", "merged para two", "third para"]
    );

    let mut rejected = result;
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        all_para_texts(&rejected),
        vec!["first para", "second para", "third para"]
    );
}

#[test]
fn insert_paragraph_at_position() {
    // InsertPosition::Before/After places a new paragraph at the right index,
    // marks it Inserted, and accept/reject identity holds.
    let p1 = make_para("p1", normal_segment(vec![make_text("t1", "alpha")]));
    let p2 = make_para("p2", normal_segment(vec![make_text("t2", "beta")]));
    let doc = make_doc(vec![
        normal_tracked_block(BlockNode::from(p1)),
        normal_tracked_block(BlockNode::from(p2)),
    ]);

    // Insert before p2 (equivalent to after p1 but exercises the Before branch).
    let tx = EditTransaction {
        steps: vec![EditStep::InsertParagraphs {
            anchor_block_id: NodeId::from("p2"),
            position: InsertPosition::Before,
            rationale: Some("Bridge clause".to_string()),
            blocks: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
                role: Some("body_text".to_string()),
                content: parse_paragraph_markup("middle").unwrap(),
                restart_numbering: false,
                list: None,
            })],
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    let result = apply_transaction(&doc, &tx).unwrap().0;
    assert_eq!(
        all_para_texts(&result),
        vec!["alpha", "middle", "beta"],
        "insert-before must place the new paragraph at the anchor's index"
    );
    assert!(
        matches!(result.blocks[0].status, TrackingStatus::Normal),
        "anchor's predecessor must remain Normal"
    );
    assert!(
        matches!(result.blocks[1].status, TrackingStatus::Inserted(_)),
        "new paragraph must be wrapped in TrackedBlock::Inserted"
    );
    assert!(
        matches!(result.blocks[2].status, TrackingStatus::Normal),
        "anchor block must remain Normal"
    );

    // I2: accept_all keeps the insertion.
    let mut accepted = result.clone();
    accept_all(&mut accepted);
    assert_eq!(all_para_texts(&accepted), vec!["alpha", "middle", "beta"]);

    // I3: reject_all drops the insertion and restores the original sequence.
    let mut rejected = result;
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(all_para_texts(&rejected), vec!["alpha", "beta"]);
}

#[test]
fn delete_paragraph() {
    // Single-paragraph delete marks the target as TrackingStatus::Deleted
    // and accept/reject identity holds.
    let p1 = make_para("p1", normal_segment(vec![make_text("t1", "keep me")]));
    let p2 = make_para(
        "p2",
        normal_segment(vec![make_text("t2", "delete this clause")]),
    );
    let p3 = make_para("p3", normal_segment(vec![make_text("t3", "keep me too")]));
    let doc = make_doc(vec![
        normal_tracked_block(BlockNode::from(p1)),
        normal_tracked_block(BlockNode::from(p2)),
        normal_tracked_block(BlockNode::from(p3)),
    ]);

    let tx = EditTransaction {
        steps: vec![EditStep::DeleteBlockRange {
            from_block_id: NodeId::from("p2"),
            to_block_id: NodeId::from("p2"),
            rationale: Some("Removed by user request".to_string()),
            expect: "delete this".to_string(),
            semantic_hash: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    let result = apply_transaction(&doc, &tx).unwrap().0;
    assert!(
        matches!(result.blocks[0].status, TrackingStatus::Normal),
        "unaffected paragraph p1 must stay Normal"
    );
    assert!(
        matches!(result.blocks[1].status, TrackingStatus::Deleted(_)),
        "target paragraph p2 must be marked Deleted"
    );
    assert!(
        matches!(result.blocks[2].status, TrackingStatus::Normal),
        "unaffected paragraph p3 must stay Normal"
    );
    // The block stays in the tree — it is only projected away during accept.
    assert_eq!(
        all_para_texts(&result),
        vec!["keep me", "delete this clause", "keep me too"]
    );

    // I2: accept_all removes the deleted block.
    let mut accepted = result.clone();
    accept_all(&mut accepted);
    assert_eq!(all_para_texts(&accepted), vec!["keep me", "keep me too"]);

    // I3: reject_all restores the original sequence.
    let mut rejected = result;
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        all_para_texts(&rejected),
        vec!["keep me", "delete this clause", "keep me too"]
    );
}

#[test]
fn delete_paragraph_with_opaque_content_preserved() {
    // Opaque inlines (e.g. hyperlinks, fields, drawings) embedded in a
    // deleted paragraph must ride along with the paragraph — they are not
    // destroyed. On accept the block is projected away; on reject the
    // paragraph and its opaque nodes reappear unchanged.
    let para = make_para(
        "p1",
        normal_segment(vec![
            make_text("t1", "See "),
            make_hyperlink("hl1", "https://example.com/ref", "reference"),
            make_text("t2", " for details, and page "),
            make_field("fld1", FieldKind::Simple),
            make_text("t3", "."),
        ]),
    );
    let keeper = make_para(
        "p2",
        normal_segment(vec![make_text("t4", "unrelated clause")]),
    );
    let doc = make_doc(vec![
        normal_tracked_block(BlockNode::from(para)),
        normal_tracked_block(BlockNode::from(keeper)),
    ]);

    let tx = EditTransaction {
        steps: vec![EditStep::DeleteBlockRange {
            from_block_id: NodeId::from("p1"),
            to_block_id: NodeId::from("p1"),
            rationale: None,
            expect: "See ".to_string(),
            semantic_hash: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    let result = apply_transaction(&doc, &tx).unwrap().0;
    assert!(
        matches!(result.blocks[0].status, TrackingStatus::Deleted(_)),
        "paragraph with opaque inlines must be markable Deleted"
    );

    // The opaque nodes must still be in the deleted paragraph — not
    // extracted, orphaned, or destroyed.
    let para_after = match &result.blocks[0].block {
        BlockNode::Paragraph(p) => p,
        _ => panic!("expected paragraph"),
    };
    let opaque_count: usize = para_after
        .segments
        .iter()
        .flat_map(|seg| seg.inlines.iter())
        .filter(|inline| matches!(inline, InlineNode::OpaqueInline(_)))
        .count();
    assert_eq!(
        opaque_count, 2,
        "both opaque inlines (hyperlink + field) must ride along with the deleted paragraph"
    );

    // I3: reject_all restores the paragraph with all opaque inlines intact.
    let mut rejected = result.clone();
    reject_all_with_styles(&mut rejected, None);
    let rejected_para = get_para(&rejected, "p1");
    let rejected_opaque_count: usize = rejected_para
        .segments
        .iter()
        .flat_map(|seg| seg.inlines.iter())
        .filter(|inline| matches!(inline, InlineNode::OpaqueInline(_)))
        .count();
    assert_eq!(
        rejected_opaque_count, 2,
        "reject_all must restore both opaque inlines"
    );
    assert_eq!(para_text(&rejected, "p1"), "See  for details, and page .");

    // I2: accept_all drops the deleted block entirely.
    let mut accepted = result;
    accept_all(&mut accepted);
    assert_eq!(all_para_texts(&accepted), vec!["unrelated clause"]);
}

#[test]
fn insert_with_stale_anchor_fails() {
    // An insert step targeting a non-existent anchor block must fail with
    // a typed BlockNotFound error (no silent fallback to "append at end").
    let p1 = make_para("p1", normal_segment(vec![make_text("t1", "only block")]));
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(p1))]);

    let tx = EditTransaction {
        steps: vec![EditStep::InsertParagraphs {
            anchor_block_id: NodeId::from("p_does_not_exist"),
            position: InsertPosition::After,
            rationale: None,
            blocks: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
                role: Some("body_text".to_string()),
                content: parse_paragraph_markup("orphan").unwrap(),
                restart_numbering: false,
                list: None,
            })],
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    let err = apply_transaction(&doc, &tx).unwrap_err();
    match err {
        EditError::BlockNotFound {
            ref block_id,
            step_index,
        } => {
            assert_eq!(
                block_id,
                &NodeId::from("p_does_not_exist"),
                "error must identify the missing anchor"
            );
            assert_eq!(step_index, 0, "error must identify the failing step");
        }
        other => panic!("expected BlockNotFound, got: {other}"),
    }

    // The document must be unchanged: transactions are all-or-nothing.
    // `apply_transaction` never mutates its input, so the original `doc` still
    // holds exactly the block it started with.
    assert_eq!(all_para_texts(&doc), vec!["only block"]);
}

#[test]
fn delete_with_expect_mismatch_fails() {
    // Delete's `expect` must be a substring of the target paragraph's visible
    // text. If the substring isn't present, the step fails with ExpectMismatch.
    let p1 = make_para(
        "p1",
        normal_segment(vec![make_text("t1", "The parties agree to arbitrate.")]),
    );
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(p1))]);

    let tx = EditTransaction {
        steps: vec![EditStep::DeleteBlockRange {
            from_block_id: NodeId::from("p1"),
            to_block_id: NodeId::from("p1"),
            rationale: None,
            // Wrong: this substring is not in the paragraph.
            expect: "jury trial".to_string(),
            semantic_hash: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    let err = apply_transaction(&doc, &tx).unwrap_err();
    match err {
        EditError::ExpectMismatch {
            ref block_id,
            ref expected,
            step_index,
            ..
        } => {
            assert_eq!(block_id, &NodeId::from("p1"));
            assert_eq!(expected, "jury trial");
            assert_eq!(step_index, 0);
        }
        other => panic!("expected ExpectMismatch, got: {other}"),
    }

    // Document must be unchanged after a failed transaction.
    let para = get_para(&doc, "p1");
    assert_eq!(para.segments[0].status, TrackingStatus::Normal);
}

// ─── Category 8: Edge cases ─────────────────────────────────────────────────

#[test]
fn identity_replacement_fails_loud_as_no_op() {
    // I9 (new contract): replacing with identical content changes nothing, so
    // the op must FAIL LOUD with NoOpEdit rather than report success
    // (CLAUDE.md "no silent fallbacks"). A no-op reported as applied is the bug.
    let doc = make_simple_doc("p1", "hello world");
    let tx = replace_transaction("p1", "hello", text_content("hello world"));
    let err = apply_transaction(&doc, &tx)
        .expect_err("identity replace must fail loud, not silently report success");
    assert!(
        matches!(err, EditError::NoOpEdit { step_index: 0, .. }),
        "expected EditError::NoOpEdit at step 0, got {err:?}"
    );
}

#[test]
fn replace_all_text_in_paragraph() {
    let doc = make_simple_doc("p1", "old text");
    let tx = replace_transaction("p1", "old", text_content("completely new"));
    let result = apply_transaction(&doc, &tx).unwrap().0;

    assert_eq!(accepted_text(&result, "p1"), "completely new");
    assert_eq!(rejected_text(&result, "p1"), "old text");
}

#[test]
fn replace_empty_paragraph_with_text() {
    let para = make_para("p1", normal_segment(vec![make_text("t1", "")]));
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    let tx = replace_transaction("p1", "", text_content("new text"));
    let result = apply_transaction(&doc, &tx).unwrap().0;

    assert_eq!(accepted_text(&result, "p1"), "new text");
}

#[test]
fn replace_with_non_ascii_text() {
    let doc = make_simple_doc("p1", "日本語テキスト");
    let tx = replace_transaction("p1", "日本語", text_content("中文テキスト"));
    let result = apply_transaction(&doc, &tx).unwrap().0;

    assert_eq!(accepted_text(&result, "p1"), "中文テキスト");
    assert_eq!(rejected_text(&result, "p1"), "日本語テキスト");
}

#[test]
fn replace_whitespace_only_changes() {
    let doc = make_simple_doc("p1", "hello  world");
    let tx = replace_transaction("p1", "hello", text_content("hello world"));
    let result = apply_transaction(&doc, &tx).unwrap().0;

    assert_eq!(accepted_text(&result, "p1"), "hello world");
    assert_eq!(rejected_text(&result, "p1"), "hello  world");
}

#[test]
fn block_id_preserved_after_edit() {
    // I5: the edited paragraph keeps its original block_id
    let doc = make_simple_doc("p1", "hello world");
    let tx = replace_transaction("p1", "hello", text_content("goodbye world"));
    let result = apply_transaction(&doc, &tx).unwrap().0;

    let para = get_para(&result, "p1");
    assert_eq!(para.id, NodeId::from("p1"));
}

#[test]
fn multiple_paragraphs_only_target_is_modified() {
    let p1 = make_para("p1", normal_segment(vec![make_text("t1", "paragraph one")]));
    let p2 = make_para("p2", normal_segment(vec![make_text("t2", "paragraph two")]));
    let doc = make_doc(vec![
        normal_tracked_block(BlockNode::from(p1)),
        normal_tracked_block(BlockNode::from(p2)),
    ]);

    let tx = replace_transaction("p1", "paragraph", text_content("modified one"));
    let result = apply_transaction(&doc, &tx).unwrap().0;

    // p2 should be completely untouched
    let p2 = get_para(&result, "p2");
    assert_eq!(p2.segments.len(), 1);
    assert_eq!(p2.segments[0].status, TrackingStatus::Normal);
    assert_eq!(para_text(&result, "p2"), "paragraph two");
}

#[test]
fn opaque_with_text_between_two_opaques() {
    // Paragraph: text1 [opaque1] text2 [opaque2] text3
    let para = make_para(
        "p1",
        normal_segment(vec![
            make_text("t1", "first "),
            make_opaque("op1"),
            make_text("t2", " second "),
            make_opaque("op2"),
            make_text("t3", " third"),
        ]),
    );
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    // Change middle section only
    let content = ParagraphContent {
        fragments: vec![
            ContentFragment::Text("first ".to_string()),
            ContentFragment::PreservedInlineRef(NodeId::from("op1")),
            ContentFragment::Text(" modified ".to_string()),
            ContentFragment::PreservedInlineRef(NodeId::from("op2")),
            ContentFragment::Text(" third".to_string()),
        ],
    };
    let tx = replace_transaction("p1", "second", content);
    let result = apply_transaction(&doc, &tx).unwrap().0;

    assert_eq!(accepted_text(&result, "p1"), "first  modified  third");
    assert_eq!(rejected_text(&result, "p1"), "first  second  third");

    // Both opaques must survive
    let para = get_para(&result, "p1");
    let opaque_ids: Vec<String> = para
        .segments
        .iter()
        .flat_map(|s| s.inlines.iter())
        .filter_map(|i| match i {
            InlineNode::OpaqueInline(o) => Some(o.id.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(opaque_ids, vec!["op1", "op2"]);
}

// ─── Category 9: Special inline structures (mutation coverage) ──────────────

#[test]
fn edit_preserves_hyperlink_opaque() {
    let para = make_para(
        "p1",
        normal_segment(vec![
            make_text("t1", "see "),
            make_hyperlink("link1", "https://example.com", "Example"),
            make_text("t2", " for details"),
        ]),
    );
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    let content = ParagraphContent {
        fragments: vec![
            ContentFragment::Text("refer to ".to_string()),
            ContentFragment::PreservedInlineRef(NodeId::from("link1")),
            ContentFragment::Text(" for more info".to_string()),
        ],
    };
    let tx = replace_transaction("p1", "see", content);
    let result = apply_transaction(&doc, &tx).unwrap().0;

    // Hyperlink must survive with correct kind and data
    let para = get_para(&result, "p1");
    let hyperlinks: Vec<_> = para
        .segments
        .iter()
        .flat_map(|s| s.inlines.iter())
        .filter_map(|i| match i {
            InlineNode::OpaqueInline(o) => match &o.kind {
                OpaqueKind::Hyperlink(data) => Some((o.id.clone(), data.clone())),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(hyperlinks.len(), 1);
    assert_eq!(hyperlinks[0].0, NodeId::from("link1"));
    assert_eq!(hyperlinks[0].1.url, Some("https://example.com".to_string()));
    assert_eq!(hyperlinks[0].1.text, "Example");

    // Roundtrip text
    assert_eq!(accepted_text(&result, "p1"), "refer to  for more info");
    assert_eq!(rejected_text(&result, "p1"), "see  for details");
}

#[test]
fn edit_preserves_hyperlink_accept_reject() {
    let para = make_para(
        "p1",
        normal_segment(vec![
            make_text("t1", "click "),
            make_hyperlink("link1", "https://test.org", "here"),
            make_text("t2", " now"),
        ]),
    );
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    let content = ParagraphContent {
        fragments: vec![
            ContentFragment::Text("tap ".to_string()),
            ContentFragment::PreservedInlineRef(NodeId::from("link1")),
            ContentFragment::Text(" please".to_string()),
        ],
    };
    let tx = replace_transaction("p1", "click", content);
    let result = apply_transaction(&doc, &tx).unwrap().0;

    // Accept gives new text around the hyperlink
    assert_eq!(accepted_text(&result, "p1"), "tap  please");
    // Reject gives old text around the hyperlink
    assert_eq!(rejected_text(&result, "p1"), "click  now");

    // Hyperlink survives in both accept and reject views
    let para = get_para(&result, "p1");
    let has_hyperlink = para.segments.iter().any(|s| {
        s.inlines
            .iter()
            .any(|i| matches!(i, InlineNode::OpaqueInline(o) if matches!(&o.kind, OpaqueKind::Hyperlink(_))))
    });
    assert!(has_hyperlink, "hyperlink must survive editing");
}

#[test]
fn edit_preserves_field_opaques() {
    let para = make_para(
        "p1",
        normal_segment(vec![
            make_text("t1", "page "),
            make_field("f_begin", FieldKind::Begin),
            make_field("f_sep", FieldKind::Separate),
            make_field("f_end", FieldKind::End),
            make_text("t2", " footer"),
        ]),
    );
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    let content = ParagraphContent {
        fragments: vec![
            ContentFragment::Text("section ".to_string()),
            ContentFragment::PreservedInlineRef(NodeId::from("f_begin")),
            ContentFragment::PreservedInlineRef(NodeId::from("f_sep")),
            ContentFragment::PreservedInlineRef(NodeId::from("f_end")),
            ContentFragment::Text(" header".to_string()),
        ],
    };
    let tx = replace_transaction("p1", "page", content);
    let result = apply_transaction(&doc, &tx).unwrap().0;

    // All three field opaques must survive
    let para = get_para(&result, "p1");
    let field_ids: Vec<String> = para
        .segments
        .iter()
        .flat_map(|s| s.inlines.iter())
        .filter_map(|i| match i {
            InlineNode::OpaqueInline(o) => match &o.kind {
                OpaqueKind::Field(_) => Some(o.id.to_string()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(field_ids, vec!["f_begin", "f_sep", "f_end"]);

    assert_eq!(accepted_text(&result, "p1"), "section  header");
    assert_eq!(rejected_text(&result, "p1"), "page  footer");
}

#[test]
fn edit_preserves_field_data() {
    let para = make_para(
        "p1",
        normal_segment(vec![
            make_text("t1", "value: "),
            make_field("f1", FieldKind::Simple),
            make_text("t2", " end"),
        ]),
    );
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    let content = ParagraphContent {
        fragments: vec![
            ContentFragment::Text("result: ".to_string()),
            ContentFragment::PreservedInlineRef(NodeId::from("f1")),
            ContentFragment::Text(" done".to_string()),
        ],
    };
    let tx = replace_transaction("p1", "value:", content);
    let result = apply_transaction(&doc, &tx).unwrap().0;

    // Verify FieldData is preserved unchanged
    let para = get_para(&result, "p1");
    let field_data: Vec<_> = para
        .segments
        .iter()
        .flat_map(|s| s.inlines.iter())
        .filter_map(|i| match i {
            InlineNode::OpaqueInline(o) => match &o.kind {
                OpaqueKind::Field(data) => Some(data.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(field_data.len(), 1);
    assert_eq!(field_data[0].field_kind, FieldKind::Simple);
    assert_eq!(field_data[0].instruction_text, Some("PAGE".to_string()));
    assert_eq!(field_data[0].result_text, Some("1".to_string()));
}

#[test]
fn edit_preserves_sdt_opaque() {
    let para = make_para(
        "p1",
        normal_segment(vec![
            make_text("t1", "title: "),
            make_sdt("sdt1"),
            make_text("t2", " end"),
        ]),
    );
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    let content = ParagraphContent {
        fragments: vec![
            ContentFragment::Text("heading: ".to_string()),
            ContentFragment::PreservedInlineRef(NodeId::from("sdt1")),
            ContentFragment::Text(" done".to_string()),
        ],
    };
    let tx = replace_transaction("p1", "title:", content);
    let result = apply_transaction(&doc, &tx).unwrap().0;

    // SDT must survive
    let para = get_para(&result, "p1");
    let has_sdt = para.segments.iter().any(|s| {
        s.inlines
            .iter()
            .any(|i| matches!(i, InlineNode::OpaqueInline(o) if matches!(&o.kind, OpaqueKind::Sdt)))
    });
    assert!(has_sdt, "SDT opaque must survive editing");

    assert_eq!(accepted_text(&result, "p1"), "heading:  done");
    assert_eq!(rejected_text(&result, "p1"), "title:  end");
}

#[test]
fn edit_preserves_bookmark_decorations() {
    // Bookmarks are DecorationNode, not OpaqueInlineNode.
    // They are zero-width pass-through nodes that must survive editing
    // automatically — they are NOT referenced via PreservedInlineRef.
    let para = make_para(
        "p1",
        normal_segment(vec![
            make_text("t1", "before "),
            make_decoration("bm1"),
            make_text("t2", "after"),
        ]),
    );
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    let content = text_content("modified text");
    let tx = replace_transaction("p1", "before", content);
    let result = apply_transaction(&doc, &tx).unwrap().0;

    // Bookmark decoration must survive as a zero-width pass-through
    let para = get_para(&result, "p1");
    let has_bookmark = para.segments.iter().any(|s| {
        s.inlines
            .iter()
            .any(|i| matches!(i, InlineNode::Decoration(d) if d.id == NodeId::from("bm1")))
    });
    assert!(
        has_bookmark,
        "bookmark decoration must survive editing as zero-width pass-through"
    );
}

#[test]
fn edit_preserves_mixed_inline_types() {
    // Paragraph with hyperlink + bookmark + field + SDT, all surrounded by text.
    let para = make_para(
        "p1",
        normal_segment(vec![
            make_text("t1", "start "),
            make_hyperlink("link1", "https://example.com", "Link"),
            make_text("t2", " mid1 "),
            make_decoration("bm1"),
            make_text("t3", "mid2 "),
            make_field("f1", FieldKind::Simple),
            make_text("t4", " mid3 "),
            make_sdt("sdt1"),
            make_text("t5", " end"),
        ]),
    );
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    let content = ParagraphContent {
        fragments: vec![
            ContentFragment::Text("begin ".to_string()),
            ContentFragment::PreservedInlineRef(NodeId::from("link1")),
            ContentFragment::Text(" a ".to_string()),
            ContentFragment::Text("b ".to_string()),
            ContentFragment::PreservedInlineRef(NodeId::from("f1")),
            ContentFragment::Text(" c ".to_string()),
            ContentFragment::PreservedInlineRef(NodeId::from("sdt1")),
            ContentFragment::Text(" finish".to_string()),
        ],
    };
    let tx = replace_transaction("p1", "start", content);
    let result = apply_transaction(&doc, &tx).unwrap().0;

    let para = get_para(&result, "p1");

    // Hyperlink survived
    let has_hyperlink = para.segments.iter().any(|s| {
        s.inlines
            .iter()
            .any(|i| matches!(i, InlineNode::OpaqueInline(o) if matches!(&o.kind, OpaqueKind::Hyperlink(_))))
    });
    assert!(has_hyperlink, "hyperlink must survive mixed editing");

    // Bookmark survived (zero-width pass-through)
    let has_bookmark = para.segments.iter().any(|s| {
        s.inlines
            .iter()
            .any(|i| matches!(i, InlineNode::Decoration(d) if d.id == NodeId::from("bm1")))
    });
    assert!(has_bookmark, "bookmark must survive mixed editing");

    // Field survived
    let has_field = para.segments.iter().any(|s| {
        s.inlines.iter().any(
            |i| matches!(i, InlineNode::OpaqueInline(o) if matches!(&o.kind, OpaqueKind::Field(_))),
        )
    });
    assert!(has_field, "field must survive mixed editing");

    // SDT survived
    let has_sdt = para.segments.iter().any(|s| {
        s.inlines
            .iter()
            .any(|i| matches!(i, InlineNode::OpaqueInline(o) if matches!(&o.kind, OpaqueKind::Sdt)))
    });
    assert!(has_sdt, "SDT must survive mixed editing");
}

#[test]
fn edit_missing_hyperlink_fails() {
    let para = make_para(
        "p1",
        normal_segment(vec![
            make_text("t1", "see "),
            make_hyperlink("link1", "https://example.com", "Example"),
            make_text("t2", " here"),
        ]),
    );
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    // Replacement text omits the hyperlink reference
    let content = text_content("see here");
    let tx = replace_transaction("p1", "see", content);
    let err = apply_transaction(&doc, &tx).unwrap_err();
    assert!(
        matches!(err, EditError::OpaqueDestroyed { .. }),
        "expected OpaqueDestroyed when hyperlink is dropped, got: {err}"
    );
}

#[test]
fn edit_missing_field_fails() {
    let para = make_para(
        "p1",
        normal_segment(vec![
            make_text("t1", "page "),
            make_field("f1", FieldKind::Begin),
            make_text("t2", " end"),
        ]),
    );
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    // Replacement text omits the field reference
    let content = text_content("page end");
    let tx = replace_transaction("p1", "page", content);
    let err = apply_transaction(&doc, &tx).unwrap_err();
    assert!(
        matches!(err, EditError::OpaqueDestroyed { .. }),
        "expected OpaqueDestroyed when field is dropped, got: {err}"
    );
}

#[test]
fn edit_preserves_multiple_hyperlinks() {
    let para = make_para(
        "p1",
        normal_segment(vec![
            make_text("t1", "see "),
            make_hyperlink("link1", "https://one.com", "First"),
            make_text("t2", " and "),
            make_hyperlink("link2", "https://two.com", "Second"),
            make_text("t3", " links"),
        ]),
    );
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    let content = ParagraphContent {
        fragments: vec![
            ContentFragment::Text("check ".to_string()),
            ContentFragment::PreservedInlineRef(NodeId::from("link1")),
            ContentFragment::Text(" or ".to_string()),
            ContentFragment::PreservedInlineRef(NodeId::from("link2")),
            ContentFragment::Text(" refs".to_string()),
        ],
    };
    let tx = replace_transaction("p1", "see", content);
    let result = apply_transaction(&doc, &tx).unwrap().0;

    // Both hyperlinks must survive with distinct data
    let para = get_para(&result, "p1");
    let hyperlinks: Vec<_> = para
        .segments
        .iter()
        .flat_map(|s| s.inlines.iter())
        .filter_map(|i| match i {
            InlineNode::OpaqueInline(o) => match &o.kind {
                OpaqueKind::Hyperlink(data) => Some((o.id.clone(), data.url.clone())),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(hyperlinks.len(), 2);
    assert_eq!(hyperlinks[0].0, NodeId::from("link1"));
    assert_eq!(hyperlinks[0].1, Some("https://one.com".to_string()));
    assert_eq!(hyperlinks[1].0, NodeId::from("link2"));
    assert_eq!(hyperlinks[1].1, Some("https://two.com".to_string()));

    assert_eq!(accepted_text(&result, "p1"), "check  or  refs");
    assert_eq!(rejected_text(&result, "p1"), "see  and  links");
}

// ─── Defensive numbering-prefix strip (Category 9) ──────────────────────────
//
// Today's import path materializes list-generated numbering (w:numPr) into the
// paragraph's run text, AND sets `numbering_text` metadata. The frontend
// renders the metadata via `[data-numbering-text]::before`, so visible output
// becomes `"{number}\t{runs}"`. When the llm_view sends the runs to the LLM,
// the LLM sees the numbering as regular text and typically echoes it back in
// its `replace.text` — which, after apply, would render as `"2.\t2.FEES"`
// (doubled). The defensive strip in `apply_transaction` drops the duplicate.
//
// The rule is byte-exact and format-agnostic: strip from the first Text
// fragment only if the target's visible text starts with `materialized_
// numbering_prefix(para)` AND the fragment starts with the same exact string.
// Renumbering intents (LLM emits a different prefix) pass through unchanged.

/// Helper: paragraph with structural numbering whose synthesized text is
/// materialized into the first run. Mirrors how Word-imported numbered
/// headings currently round-trip through the import path.
fn make_numbered_para(id: &str, synthesized: &str, body: &str) -> ParagraphNode {
    let mut para = make_para(
        id,
        normal_segment(vec![
            make_text(&format!("{id}_t_num"), synthesized),
            make_text(&format!("{id}_t_body"), body),
        ]),
    );
    para.numbering = Some(NumberingInfo {
        num_id: 1,
        ilvl: 0,
        synthesized_text: synthesized.to_string(),
        is_bullet: false,
        restart_numbering: false,
    });
    para
}

/// Arabic numbering: target "2.FEES AND PAYMENT" with numbering "2."; LLM
/// echoes "2.FEES" in its replacement. NEW CONTRACT (was: silently strip):
/// the engine REFUSES with PrefixDuplicatesLabel rather than quietly dropping
/// the echoed "2." — the same contract the span path enforces, so the agent
/// learns the numbering is already present (CLAUDE.md "no silent fallbacks").
#[test]
fn replace_with_arabic_numbering_duplicated_prefix_is_refused() {
    let para = make_numbered_para("p1", "2.", "FEES AND PAYMENT");
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    let content = text_content("2.FEES");
    let tx = replace_transaction("p1", "2.FEES AND PAYMENT", content);
    let err = apply_transaction(&doc, &tx)
        .expect_err("a replacement echoing the numbering label must be refused, not stripped");
    match err {
        EditError::PrefixDuplicatesLabel {
            block_id, label, ..
        } => {
            assert_eq!(block_id, NodeId::from("p1"));
            assert_eq!(label, "2.");
        }
        other => panic!("expected PrefixDuplicatesLabel, got {other:?}"),
    }
}

/// Roman numbering: same scenario but with "II." as the materialized prefix.
/// Proves the rule is format-agnostic — we never parse "what counts as a
/// numbering prefix," just compare byte-for-byte against the synthesized text.
#[test]
fn replace_with_roman_numbering_duplicated_prefix_is_refused() {
    let para = make_numbered_para("p1", "II.", "ARBITRATION");
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    let content = text_content("II.MEDIATION");
    let tx = replace_transaction("p1", "II.ARBITRATION", content);
    let err = apply_transaction(&doc, &tx).expect_err("echoed roman label must be refused");
    assert!(
        matches!(err, EditError::PrefixDuplicatesLabel { ref label, .. } if label == "II."),
        "expected PrefixDuplicatesLabel for 'II.', got {err:?}"
    );
}

/// Renumbering intent: LLM deliberately emits a DIFFERENT prefix ("3.") in
/// its replacement. The strip must not fire — `replace.text` doesn't start
/// with the target's `numbering_text` ("2."), so we leave the LLM's intent
/// intact even though the rendered result would still be doubled. That's a
/// deeper model issue we explicitly don't try to solve here.
#[test]
fn replace_with_renumber_intent_passes_through_unchanged() {
    let para = make_numbered_para("p1", "2.", "FEES AND PAYMENT");
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    let content = text_content("3.FEES AND PAYMENT");
    let tx = replace_transaction("p1", "2.FEES AND PAYMENT", content);
    let result = apply_transaction(&doc, &tx).unwrap().0;

    // "3." is not the materialized prefix — passes through verbatim.
    assert_eq!(accepted_text(&result, "p1"), "3.FEES AND PAYMENT");
}

/// Clean-runs case: target paragraph has numbering metadata ("2.") but the
/// runs themselves are clean ("FEES AND PAYMENT") — numbering is rendered from
/// metadata, not materialized into the runs. The LLM still sees the prefix in
/// the projection and echoes it ("2. FEES"). NEW CONTRACT (was: silently strip):
/// the duplication is REFUSED with PrefixDuplicatesLabel regardless of whether
/// the runs currently contain the prefix — the guard compares the replacement's
/// leading text against the paragraph's numbering label, which lives in
/// metadata.
#[test]
fn replace_duplicated_prefix_refused_even_when_target_runs_are_clean() {
    // Target with numbering metadata but clean run text (no "2." prefix
    // in the runs themselves).
    let mut para = make_para(
        "p1",
        normal_segment(vec![make_text("p1_t", "FEES AND PAYMENT")]),
    );
    para.numbering = Some(NumberingInfo {
        num_id: 1,
        ilvl: 0,
        synthesized_text: "2.".to_string(),
        is_bullet: false,
        restart_numbering: false,
    });
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    let content = text_content("2. FEES");
    let tx = replace_transaction("p1", "FEES AND PAYMENT", content);
    let err = apply_transaction(&doc, &tx)
        .expect_err("echoed prefix must be refused even when the runs are clean");
    assert!(
        matches!(err, EditError::PrefixDuplicatesLabel { ref label, .. } if label == "2."),
        "expected PrefixDuplicatesLabel for '2.', got {err:?}"
    );
}

/// Symmetric-rule test: target has numbering metadata ("2."), but the
/// LLM's replacement does NOT start with that prefix (it's just "FEES
/// CHANGED"). Condition on `replace.text.starts_with(prefix)` fails,
/// so we pass the replacement through unchanged. This proves the strip
/// only fires when the LLM is echoing the prefix — an LLM that
/// correctly omitted the auto-numbering prefix is not retroactively
/// "fixed" by the engine.
#[test]
fn replace_where_llm_omits_numbering_prefix_passes_through_unchanged() {
    let para = make_numbered_para("p1", "2.", "FEES AND PAYMENT");
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    // LLM already knows "2." is auto-generated and emits just the body.
    // The strip must not fire: `replace.text.starts_with("2.")` is false.
    let content = text_content("FEES CHANGED");
    let tx = replace_transaction("p1", "FEES AND PAYMENT", content);
    let result = apply_transaction(&doc, &tx).unwrap().0;

    // The diff marks the original "2.FEES AND PAYMENT" as Deleted and
    // "FEES CHANGED" as Inserted (they share no useful common prefix —
    // the "2." was only in the old). After accept_all, the runs are just
    // "FEES CHANGED"; at render time the `[data-numbering-text]::before`
    // CSS rule still prepends "2.\t" visually. The defensive strip never
    // ran (replacement doesn't start with "2."), so "CHANGED" is exactly
    // what the LLM asked for — no engine-side rewrite.
    assert_eq!(accepted_text(&result, "p1"), "FEES CHANGED");
}

// ─── apply_op_id propagation ─────────────────────────────────────────────────

/// INVARIANT: every tracked segment produced by a single apply_transaction
/// call must carry the transaction's `apply_op_id` on its RevisionInfo.
///
/// This is the backend half of the "one apply = one op id" guarantee that
/// the frontend's review-queue scoping depends on. If this test fails, it
/// means the engine is silently dropping the apply_op_id somewhere during
/// ReplaceParagraphText's tracked-change materialization, and the frontend
/// will see "applied=true but queue is empty".
#[test]
fn apply_op_id_is_stamped_on_every_tracked_segment_the_apply_produces() {
    let doc = make_simple_doc("p1", "original text");

    let tx = EditTransaction {
        steps: vec![EditStep::ReplaceParagraphText {
            block_id: NodeId::from("p1"),
            rationale: None,
            replacement_role: None,
            expect: "original text".to_string(),
            semantic_hash: None,
            content: text_content("rewritten text"),
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: RevisionInfo {
            revision_id: 0,
            identity: 0,
            author: Some("Andreas".to_string()),
            date: Some("2026-04-13T00:00:00Z".to_string()),
            apply_op_id: Some("op_test_xyz".to_string()),
        },
    };

    let result = apply_transaction(&doc, &tx).expect("apply must succeed").0;

    // Walk every block → paragraph → segment → tracking status and verify
    // every Inserted/Deleted RevisionInfo carries our apply_op_id. If the
    // engine dropped it anywhere, a segment will have `apply_op_id: None`.
    let mut tracked_segment_count = 0usize;
    for block in &result.blocks {
        if let BlockNode::Paragraph(p) = &block.block {
            for seg in &p.segments {
                match &seg.status {
                    TrackingStatus::Inserted(rev) | TrackingStatus::Deleted(rev) => {
                        tracked_segment_count += 1;
                        assert_eq!(
                            rev.apply_op_id.as_deref(),
                            Some("op_test_xyz"),
                            "tracked segment missing apply_op_id: status={:?}",
                            seg.status,
                        );
                    }
                    TrackingStatus::InsertedThenDeleted(_) => {
                        unreachable!("this fixture's edits never produce stacked segments")
                    }
                    TrackingStatus::Normal => {}
                }
            }
        }
    }

    assert!(
        tracked_segment_count > 0,
        "apply_transaction produced no tracked segments — did the edit apply correctly?"
    );
}

// NOTE: the "changelet extraction preserves apply_op_id" invariant lives with
// the consuming application (`apply_op_id_changelet_pipeline.rs`), where the changelet layer
// (clause + changelet, app-layer concerns not shipped with stemma) is exercised
// end-to-end through a real DOCX. It is not an engine-level concern, so it does
// not live in the stemma engine suite.

// ─── Move step tests (the `move` op) ─────────────────────────────────────────

#[test]
fn move_single_block_pairs_source_and_destination() {
    // Spec: source becomes Deleted with a move_id, destination is a
    // clone marked Inserted with the SAME move_id. Both halves must
    // carry the id so the serializer can emit paired
    // w:moveFromRange / w:moveToRange bookmarks.
    let p1 = make_para("p1", normal_segment(vec![make_text("t1", "alpha")]));
    let p2 = make_para("p2", normal_segment(vec![make_text("t2", "beta")]));
    let p3 = make_para("p3", normal_segment(vec![make_text("t3", "gamma")]));
    let doc = make_doc(vec![
        normal_tracked_block(BlockNode::from(p1)),
        normal_tracked_block(BlockNode::from(p2)),
        normal_tracked_block(BlockNode::from(p3)),
    ]);

    // Move p1 to after p3: "alpha, beta, gamma" → "beta, gamma, alpha".
    let tx = EditTransaction {
        steps: vec![EditStep::MoveBlockRange {
            from_block_id: NodeId::from("p1"),
            to_block_id: NodeId::from("p1"),
            dest_anchor_id: NodeId::from("p3"),
            dest_position: InsertPosition::After,
            rationale: Some("Move alpha to the end".to_string()),
            expect: None,
            semantic_hash: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    let result = apply_transaction(&doc, &tx).unwrap().0;

    // The source (p1) must still be present in order, now Deleted.
    let source_block = result
        .blocks
        .iter()
        .find(|tb| match &tb.block {
            BlockNode::Paragraph(p) => p.id == NodeId::from("p1"),
            _ => false,
        })
        .expect("source p1 still present as Deleted shadow");
    assert!(
        matches!(source_block.status, TrackingStatus::Deleted(_)),
        "source block must be marked Deleted, got {:?}",
        source_block.status
    );
    let source_move_id = source_block
        .move_id
        .as_ref()
        .expect("source block must carry a move_id")
        .clone();

    // Exactly one Inserted block should exist, and it must carry the
    // same move_id.
    let inserted_blocks: Vec<&TrackedBlock> = result
        .blocks
        .iter()
        .filter(|tb| matches!(tb.status, TrackingStatus::Inserted(_)))
        .collect();
    assert_eq!(
        inserted_blocks.len(),
        1,
        "exactly one destination block must be inserted, got {}",
        inserted_blocks.len()
    );
    let dest_block = inserted_blocks[0];
    assert_eq!(
        dest_block.move_id.as_deref(),
        Some(source_move_id.as_str()),
        "destination must share the source's move_id"
    );

    // The destination must be a deep clone of the source content
    // (same text, distinct block id).
    let dest_para = match &dest_block.block {
        BlockNode::Paragraph(p) => p,
        _ => panic!("expected paragraph destination"),
    };
    assert_ne!(
        dest_para.id,
        NodeId::from("p1"),
        "destination must have a fresh id so it doesn't collide with the source"
    );
    let dest_text: String = dest_para
        .segments
        .iter()
        .flat_map(|s| s.inlines.iter())
        .filter_map(|i| match i {
            InlineNode::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(dest_text, "alpha");

    // Accept-all projection: source vanishes, destination lands after p3.
    let mut accepted = result.clone();
    accept_all(&mut accepted);
    assert_eq!(
        all_para_texts(&accepted),
        vec!["beta", "gamma", "alpha"],
        "accept must yield the post-move order"
    );

    // Reject-all projection: original order is restored.
    let mut rejected = result;
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        all_para_texts(&rejected),
        vec!["alpha", "beta", "gamma"],
        "reject must restore the pre-move order"
    );
}

/// Collect every block id (top-level and table-nested) in the doc.
fn all_block_ids(doc: &CanonDoc) -> Vec<String> {
    fn walk(block: &BlockNode, out: &mut Vec<String>) {
        match block {
            BlockNode::Paragraph(p) => out.push(p.id.0.to_string()),
            BlockNode::OpaqueBlock(o) => out.push(o.id.0.to_string()),
            BlockNode::Table(t) => {
                out.push(t.id.0.to_string());
                for row in &t.rows {
                    for cell in &row.cells {
                        for b in &cell.blocks {
                            walk(b, out);
                        }
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    for tb in &doc.blocks {
        walk(&tb.block, &mut out);
    }
    out
}

#[test]
fn move_table_reassigns_nested_cell_ids_and_refuses_edit_to_deleted_shadow() {
    // P0 #3: moving a table left the moved clone's nested cell-paragraph ids
    // identical to the still-present Deleted source ("shadow"). find_paragraph_path
    // resolves a block id to the FIRST match in document order, so a later edit
    // targeting the cell paragraph hit the shadow and was silently lost on accept.
    //
    // Correct behavior:
    //   (1) the clone's nested ids are made unique (no collision with the shadow);
    //   (2) editing the original cell-paragraph id — which now belongs to the
    //       Deleted shadow — is REFUSED (you cannot edit deleted content), instead
    //       of silently mutating the shadow.
    let table = make_table(
        "tbl1",
        vec![make_table_row(
            "r1",
            vec![make_table_cell(
                "c1",
                vec![BlockNode::from(make_para(
                    "cp1",
                    normal_segment(vec![make_text("cp1_t1", "cell text")]),
                ))],
            )],
        )],
    );
    let anchor = make_para("anchor", normal_segment(vec![make_text("a_t1", "anchor")]));
    let doc = make_doc(vec![
        normal_tracked_block(table),
        normal_tracked_block(BlockNode::from(anchor)),
    ]);

    // Move the table to after the anchor: [tbl1, anchor] → [tbl1(Deleted), anchor, clone(Inserted)].
    let tx = EditTransaction {
        steps: vec![EditStep::MoveBlockRange {
            from_block_id: NodeId::from("tbl1"),
            to_block_id: NodeId::from("tbl1"),
            dest_anchor_id: NodeId::from("anchor"),
            dest_position: InsertPosition::After,
            rationale: None,
            expect: None,
            semantic_hash: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };
    let moved = apply_transaction(&doc, &tx).expect("move applies").0;

    // (1) No id collision: "cp1" occurs exactly once across the whole doc — the
    // Deleted shadow keeps the original id, the Inserted clone got a fresh one.
    let ids = all_block_ids(&moved);
    let cp1_count = ids.iter().filter(|id| id.as_str() == "cp1").count();
    assert_eq!(
        cp1_count, 1,
        "moved clone must not duplicate the source's nested cell-paragraph id; ids = {ids:?}"
    );
    // The clone's cell paragraph exists under a distinct, fresh id.
    assert!(
        ids.iter().any(|id| id != "cp1" && id.starts_with("cp1")),
        "clone cell paragraph should have a fresh id derived from cp1; ids = {ids:?}"
    );

    // (2) Editing the original cell-paragraph id (now the Deleted shadow) is refused.
    let edit_tx = replace_transaction("cp1", "cell", text_content("edited cell text"));
    let err = apply_transaction(&moved, &edit_tx).expect_err(
        "editing a cell inside the Deleted move shadow must be refused, not silently applied",
    );
    assert!(
        matches!(
            err,
            EditError::BlockHasTrackedStatus {
                status: "deleted",
                ..
            }
        ),
        "expected BlockHasTrackedStatus(deleted) for an edit inside the deleted table shadow, got: {err}"
    );
}

#[test]
fn move_multi_block_range_shares_single_move_id() {
    // Moving a range [p1..=p2] to after p4 must produce:
    //   source: p1 Deleted + p2 Deleted, both with move_id = X
    //   dest:   clone of p1 Inserted + clone of p2 Inserted, both with move_id = X
    let p1 = make_para("p1", normal_segment(vec![make_text("t1", "first")]));
    let p2 = make_para("p2", normal_segment(vec![make_text("t2", "second")]));
    let p3 = make_para("p3", normal_segment(vec![make_text("t3", "third")]));
    let p4 = make_para("p4", normal_segment(vec![make_text("t4", "fourth")]));
    let doc = make_doc(vec![
        normal_tracked_block(BlockNode::from(p1)),
        normal_tracked_block(BlockNode::from(p2)),
        normal_tracked_block(BlockNode::from(p3)),
        normal_tracked_block(BlockNode::from(p4)),
    ]);

    let tx = EditTransaction {
        steps: vec![EditStep::MoveBlockRange {
            from_block_id: NodeId::from("p1"),
            to_block_id: NodeId::from("p2"),
            dest_anchor_id: NodeId::from("p4"),
            dest_position: InsertPosition::After,
            rationale: None,
            expect: None,
            semantic_hash: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    let result = apply_transaction(&doc, &tx).unwrap().0;

    // Collect every block that carries a move_id (there should be 4:
    // two deleted sources + two inserted clones, all sharing one id).
    let move_blocks: Vec<&TrackedBlock> = result
        .blocks
        .iter()
        .filter(|tb| tb.move_id.is_some())
        .collect();
    assert_eq!(
        move_blocks.len(),
        4,
        "expected 4 blocks carrying move_id (2 source + 2 dest), got {}",
        move_blocks.len()
    );
    let shared_move_id = move_blocks[0].move_id.clone().unwrap();
    for b in &move_blocks {
        assert_eq!(
            b.move_id.as_ref(),
            Some(&shared_move_id),
            "all move-paired blocks must share a single move_id"
        );
    }

    // Split into source (Deleted) and destination (Inserted) halves.
    let deleted: Vec<&TrackedBlock> = move_blocks
        .iter()
        .copied()
        .filter(|tb| matches!(tb.status, TrackingStatus::Deleted(_)))
        .collect();
    let inserted: Vec<&TrackedBlock> = move_blocks
        .iter()
        .copied()
        .filter(|tb| matches!(tb.status, TrackingStatus::Inserted(_)))
        .collect();
    assert_eq!(deleted.len(), 2);
    assert_eq!(inserted.len(), 2);

    // Accept leaves "third, fourth, first, second".
    let mut accepted = result.clone();
    accept_all(&mut accepted);
    assert_eq!(
        all_para_texts(&accepted),
        vec!["third", "fourth", "first", "second"]
    );

    // Reject restores the original order.
    let mut rejected = result;
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        all_para_texts(&rejected),
        vec!["first", "second", "third", "fourth"]
    );
}

#[test]
fn move_rejects_destination_inside_source() {
    // Moving [p1..=p3] into p2 is undefined — the source and
    // destination ranges would overlap. The engine must reject.
    let p1 = make_para("p1", normal_segment(vec![make_text("t1", "a")]));
    let p2 = make_para("p2", normal_segment(vec![make_text("t2", "b")]));
    let p3 = make_para("p3", normal_segment(vec![make_text("t3", "c")]));
    let doc = make_doc(vec![
        normal_tracked_block(BlockNode::from(p1)),
        normal_tracked_block(BlockNode::from(p2)),
        normal_tracked_block(BlockNode::from(p3)),
    ]);

    let tx = EditTransaction {
        steps: vec![EditStep::MoveBlockRange {
            from_block_id: NodeId::from("p1"),
            to_block_id: NodeId::from("p3"),
            dest_anchor_id: NodeId::from("p2"),
            dest_position: InsertPosition::After,
            rationale: None,
            expect: None,
            semantic_hash: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    let err = apply_transaction(&doc, &tx).unwrap_err();
    assert!(
        matches!(err, EditError::MoveDestinationInsideSource { .. }),
        "expected MoveDestinationInsideSource, got: {err}"
    );
}

// ─── SetAttr step tests (the `set_attr` op) ──────────────────────────────────

#[test]
fn set_attr_records_pprchange_and_applies_exemplar_numbering() {
    // Two numbered paragraphs provide a "numbered_role" exemplar, and
    // one body paragraph is the target we promote into that role. Set
    // the body paragraph's role to the numbered one → previous pPr
    // captured as formatting_change, new pPr copied from the exemplar,
    // and the target now carries structural numbering.
    let h1 = make_numbered_para("h1", "1.", "First heading");
    let h2 = make_numbered_para("h2", "2.", "Second heading");
    let body = make_para(
        "body",
        normal_segment(vec![make_text("body_t", "plain body paragraph")]),
    );
    let doc = make_doc(vec![
        normal_tracked_block(BlockNode::from(h1)),
        normal_tracked_block(BlockNode::from(h2)),
        normal_tracked_block(BlockNode::from(body)),
    ]);

    // Pick the actual numbered role from the vocabulary — the clusterer
    // names roles heuristically and the exact id can drift with
    // implementation changes, so we look it up by numbering_source ==
    // Auto rather than hardcoding "numbered_heading" / similar.
    let vocab = stemma::vocabulary::extract_vocabulary(&doc);
    let numbered_role_id = vocab
        .paragraph_roles
        .iter()
        .find(|r| {
            r.has_numbering && r.numbering_source == Some(stemma::vocabulary::NumberingSource::Auto)
        })
        .expect("test doc must expose a numbered role")
        .id
        .clone();

    let tx = EditTransaction {
        steps: vec![EditStep::SetBlockRangeAttr {
            from_block_id: NodeId::from("body"),
            to_block_id: NodeId::from("body"),
            role: numbered_role_id,
            rationale: Some("Promote to heading".to_string()),
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    let result = apply_transaction(&doc, &tx).unwrap().0;
    let body_para = get_para(&result, "body");

    // formatting_change must be present; numbering must have been
    // copied from the exemplar; text must be untouched.
    let fc = body_para
        .formatting_change
        .as_ref()
        .expect("set_attr must attach a ParagraphFormattingChange");
    // The previous numbering was None (body text had no numPr).
    assert!(fc.previous_numbering.is_none());
    assert!(fc.previous_numbering_explicitly_absent);
    assert!(
        body_para.numbering.is_some(),
        "target paragraph must now carry the exemplar's numbering"
    );
    let text: String = body_para
        .segments
        .iter()
        .flat_map(|s| s.inlines.iter())
        .filter_map(|i| match i {
            InlineNode::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        text, "plain body paragraph",
        "text content must be untouched by set_attr"
    );

    // Accept: paragraph keeps new pPr, formatting_change is cleared.
    let mut accepted = result.clone();
    accept_all(&mut accepted);
    let accepted_body = get_para(&accepted, "body");
    assert!(accepted_body.formatting_change.is_none());
    assert!(
        accepted_body.numbering.is_some(),
        "accept must keep the new numbering reference"
    );

    // Reject: paragraph restores previous pPr (no numbering).
    let mut rejected = result;
    reject_all_with_styles(&mut rejected, None);
    let rejected_body = get_para(&rejected, "body");
    assert!(rejected_body.formatting_change.is_none());
    assert!(
        rejected_body.numbering.is_none(),
        "reject must restore the original no-numbering state"
    );
}

#[test]
fn set_attr_range_applies_to_every_block_in_range() {
    // Two body paragraphs promoted to the numbered role — both must
    // carry a formatting_change and the new numbering reference.
    let h1 = make_numbered_para("h1", "1.", "First heading");
    let body1 = make_para(
        "body1",
        normal_segment(vec![make_text("b1_t", "body paragraph one")]),
    );
    let body2 = make_para(
        "body2",
        normal_segment(vec![make_text("b2_t", "body paragraph two")]),
    );
    let doc = make_doc(vec![
        normal_tracked_block(BlockNode::from(h1)),
        normal_tracked_block(BlockNode::from(body1)),
        normal_tracked_block(BlockNode::from(body2)),
    ]);

    let vocab = stemma::vocabulary::extract_vocabulary(&doc);
    let numbered_role_id = vocab
        .paragraph_roles
        .iter()
        .find(|r| {
            r.has_numbering && r.numbering_source == Some(stemma::vocabulary::NumberingSource::Auto)
        })
        .expect("test doc must expose a numbered role")
        .id
        .clone();

    let tx = EditTransaction {
        steps: vec![EditStep::SetBlockRangeAttr {
            from_block_id: NodeId::from("body1"),
            to_block_id: NodeId::from("body2"),
            role: numbered_role_id,
            rationale: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    let result = apply_transaction(&doc, &tx).unwrap().0;
    for id in &["body1", "body2"] {
        let p = get_para(&result, id);
        assert!(
            p.formatting_change.is_some(),
            "{id} must have formatting_change after range set_attr"
        );
        assert!(
            p.numbering.is_some(),
            "{id} must have numbering copied from exemplar"
        );
    }

    // The exemplar h1 must be untouched.
    let h1_after = get_para(&result, "h1");
    assert!(
        h1_after.formatting_change.is_none(),
        "untargeted exemplar must not be mutated"
    );
}

#[test]
fn set_attr_rejects_unknown_role() {
    let body = make_para(
        "body",
        normal_segment(vec![make_text("body_t", "body text")]),
    );
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(body))]);

    let tx = EditTransaction {
        steps: vec![EditStep::SetBlockRangeAttr {
            from_block_id: NodeId::from("body"),
            to_block_id: NodeId::from("body"),
            role: "nonexistent_role".to_string(),
            rationale: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    let err = apply_transaction(&doc, &tx).unwrap_err();
    assert!(
        matches!(err, EditError::ParagraphRoleNotFound { .. }),
        "expected ParagraphRoleNotFound, got: {err}"
    );
}

#[test]
fn set_attr_no_op_when_role_unchanged() {
    // Applying set_attr with the paragraph's CURRENT role must be a
    // no-op: no formatting_change attached, no visual noise in review.
    let h1 = make_numbered_para("h1", "1.", "First heading");
    let h2 = make_numbered_para("h2", "2.", "Second heading");
    let doc = make_doc(vec![
        normal_tracked_block(BlockNode::from(h1)),
        normal_tracked_block(BlockNode::from(h2)),
    ]);

    let vocab = stemma::vocabulary::extract_vocabulary(&doc);
    let numbered_role_id = vocab
        .paragraph_roles
        .iter()
        .find(|r| {
            r.has_numbering && r.numbering_source == Some(stemma::vocabulary::NumberingSource::Auto)
        })
        .expect("test doc must expose a numbered role")
        .id
        .clone();

    // Apply the SAME role back to h1 — must be a no-op.
    let tx = EditTransaction {
        steps: vec![EditStep::SetBlockRangeAttr {
            from_block_id: NodeId::from("h1"),
            to_block_id: NodeId::from("h1"),
            role: numbered_role_id,
            rationale: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    let result = apply_transaction(&doc, &tx).unwrap().0;
    let h1_after = get_para(&result, "h1");
    assert!(
        h1_after.formatting_change.is_none(),
        "set_attr with unchanged role must not emit a formatting_change"
    );
}

// ─── Literal-prefix insert tests ─────────────────────────────────────────────
//
// The inserted paragraph's literal_prefix placeholder (cloned from the
// exemplar) gets reassigned to the next-in-sequence label based on the
// nearest preceding sibling. We intentionally DO NOT renumber
// downstream siblings — see the comment block above
// `adjust_literal_prefixes_after_insert` in src/edit.rs.

/// Paragraph with a literal numbering prefix set.
fn make_literal_prefix_para(id: &str, prefix: &str, body: &str) -> ParagraphNode {
    let mut p = make_para(
        id,
        normal_segment(vec![make_text(&format!("{id}_t"), body)]),
    );
    p.literal_prefix = Some(prefix.to_string());
    p
}

fn prefix_of(doc: &CanonDoc, block_id: &str) -> Option<String> {
    doc.blocks.iter().find_map(|tb| match &tb.block {
        BlockNode::Paragraph(p) if p.id == NodeId::from(block_id) => p.literal_prefix.clone(),
        _ => None,
    })
}

fn block_id_at(doc: &CanonDoc, idx: usize) -> String {
    match &doc.blocks[idx].block {
        BlockNode::Paragraph(p) => p.id.0.to_string(),
        BlockNode::Table(t) => t.id.0.to_string(),
        BlockNode::OpaqueBlock(o) => o.id.0.to_string(),
    }
}

#[test]
fn insert_literal_prefix_gets_anchor_plus_one_arabic() {
    // Bundle "1. alpha, 2. beta"; insert after "2." with role numbered_item.
    // Expect: new paragraph takes label "3." (anchor 2 + 1). The existing
    // paragraphs retain their labels — renumbering is the caller's job.
    let p1 = make_literal_prefix_para("p1", "1.", "alpha");
    let p2 = make_literal_prefix_para("p2", "2.", "beta");
    let doc = make_doc(vec![
        normal_tracked_block(BlockNode::from(p1)),
        normal_tracked_block(BlockNode::from(p2)),
    ]);

    let tx = EditTransaction {
        steps: vec![EditStep::InsertParagraphs {
            anchor_block_id: NodeId::from("p2"),
            position: InsertPosition::After,
            rationale: None,
            blocks: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
                role: Some("numbered_item".to_string()),
                content: parse_paragraph_markup("gamma").unwrap(),
                restart_numbering: false,
                list: None,
            })],
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    let result = apply_transaction(&doc, &tx).unwrap().0;
    assert_eq!(all_para_texts(&result), vec!["alpha", "beta", "gamma"]);
    assert_eq!(prefix_of(&result, "p1").as_deref(), Some("1."));
    assert_eq!(prefix_of(&result, "p2").as_deref(), Some("2."));
    let inserted_id = block_id_at(&result, 2);
    assert_eq!(
        prefix_of(&result, &inserted_id).as_deref(),
        Some("3."),
        "inserted paragraph must take next-in-sequence label"
    );
}

#[test]
fn insert_literal_prefix_in_middle_does_not_renumber_downstream() {
    // "1. alpha, 2. beta, 3. gamma"; insert after "1.". The inserted
    // paragraph takes "2."; downstream "2. beta" and "3. gamma" are
    // intentionally left alone so reject_all is clean and the caller
    // can chain explicit renumber edits if desired.
    let p1 = make_literal_prefix_para("p1", "1.", "alpha");
    let p2 = make_literal_prefix_para("p2", "2.", "beta");
    let p3 = make_literal_prefix_para("p3", "3.", "gamma");
    let doc = make_doc(vec![
        normal_tracked_block(BlockNode::from(p1)),
        normal_tracked_block(BlockNode::from(p2)),
        normal_tracked_block(BlockNode::from(p3)),
    ]);

    let tx = EditTransaction {
        steps: vec![EditStep::InsertParagraphs {
            anchor_block_id: NodeId::from("p1"),
            position: InsertPosition::After,
            rationale: None,
            blocks: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
                role: Some("numbered_item".to_string()),
                content: parse_paragraph_markup("bridge").unwrap(),
                restart_numbering: false,
                list: None,
            })],
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    let result = apply_transaction(&doc, &tx).unwrap().0;
    let inserted_id = block_id_at(&result, 1);
    assert_eq!(
        all_para_texts(&result),
        vec!["alpha", "bridge", "beta", "gamma"]
    );
    assert_eq!(prefix_of(&result, "p1").as_deref(), Some("1."));
    assert_eq!(
        prefix_of(&result, &inserted_id).as_deref(),
        Some("2."),
        "inserted paragraph takes anchor + 1"
    );
    assert_eq!(
        prefix_of(&result, "p2").as_deref(),
        Some("2."),
        "downstream sibling must keep its original label (no silent renumber)"
    );
    assert_eq!(prefix_of(&result, "p3").as_deref(), Some("3."));
}

#[test]
fn insert_literal_prefix_letter_sequence() {
    // "(a) first, (b) second" + insert after "(a)" → new label "(b)".
    let p1 = make_literal_prefix_para("p1", "(a)", "first");
    let p2 = make_literal_prefix_para("p2", "(b)", "second");
    let doc = make_doc(vec![
        normal_tracked_block(BlockNode::from(p1)),
        normal_tracked_block(BlockNode::from(p2)),
    ]);

    let tx = EditTransaction {
        steps: vec![EditStep::InsertParagraphs {
            anchor_block_id: NodeId::from("p1"),
            position: InsertPosition::After,
            rationale: None,
            blocks: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
                role: Some("numbered_item".to_string()),
                content: parse_paragraph_markup("middle").unwrap(),
                restart_numbering: false,
                list: None,
            })],
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    let result = apply_transaction(&doc, &tx).unwrap().0;
    let inserted_id = block_id_at(&result, 1);
    assert_eq!(prefix_of(&result, &inserted_id).as_deref(), Some("(b)"));
}

#[test]
fn insert_literal_prefix_unsupported_format_errors() {
    // Roman numerals aren't in the supported format set — the adjust
    // pass surfaces a structured error so the LLM retry loop (or the
    // user) gets a clear signal about what formats are supported.
    let p1 = make_literal_prefix_para("p1", "i.", "first");
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(p1))]);

    let tx = EditTransaction {
        steps: vec![EditStep::InsertParagraphs {
            anchor_block_id: NodeId::from("p1"),
            position: InsertPosition::After,
            rationale: None,
            blocks: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
                role: Some("numbered_item".to_string()),
                content: parse_paragraph_markup("next").unwrap(),
                restart_numbering: false,
                list: None,
            })],
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    // "i." parses as a single-letter lowercase (letter i = 9), so it is
    // supported and becomes "j.". Swap to a definitely-unsupported form.
    let result = apply_transaction(&doc, &tx);
    let inserted_id = match &result {
        Ok((res, _pending)) => block_id_at(res, 1),
        Err(e) => panic!("expected success for single-letter 'i.', got error: {e:?}"),
    };
    assert_eq!(
        prefix_of(&result.as_ref().unwrap().0, &inserted_id).as_deref(),
        Some("j.")
    );

    // Now with a hierarchical label — that one must fail.
    let p1 = make_literal_prefix_para("p1", "1.1", "nested");
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(p1))]);
    let result = apply_transaction(&doc, &tx);
    match result {
        Err(err) => {
            let msg = err.to_string();
            assert!(
                msg.contains("supported formats") || msg.contains("Arabic"),
                "error must name the supported formats: {msg}"
            );
        }
        Ok(_) => panic!("hierarchical '1.1' must not be silently accepted"),
    }
}

#[test]
fn insert_literal_prefix_reject_all_restores_original() {
    // After tracked-change insert, reject_all must drop the inserted
    // paragraph and leave the existing literal prefixes untouched.
    let p1 = make_literal_prefix_para("p1", "1.", "alpha");
    let p2 = make_literal_prefix_para("p2", "2.", "beta");
    let doc = make_doc(vec![
        normal_tracked_block(BlockNode::from(p1)),
        normal_tracked_block(BlockNode::from(p2)),
    ]);

    let tx = EditTransaction {
        steps: vec![EditStep::InsertParagraphs {
            anchor_block_id: NodeId::from("p1"),
            position: InsertPosition::After,
            rationale: None,
            blocks: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
                role: Some("numbered_item".to_string()),
                content: parse_paragraph_markup("middle").unwrap(),
                restart_numbering: false,
                list: None,
            })],
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    let result = apply_transaction(&doc, &tx).unwrap().0;
    let mut rejected = result;
    reject_all_with_styles(&mut rejected, None);
    assert_eq!(
        all_para_texts(&rejected),
        vec!["alpha", "beta"],
        "reject must drop the inserted paragraph"
    );
    assert_eq!(prefix_of(&rejected, "p1").as_deref(), Some("1."));
    assert_eq!(prefix_of(&rejected, "p2").as_deref(), Some("2."));
}

// ─── Category 9: Smart/straight punctuation folding in `expect` matching ────
//
// Word documents frequently contain curly quotes, en/em dashes, and the
// single-glyph ellipsis (often produced by Word's autocorrect). LLM-emitted
// `expect` strings use ASCII punctuation. A byte-exact `contains` would reject
// every such pair even though the visible text matches — we normalize both
// sides before comparing.

#[test]
fn replace_accepts_ascii_apostrophe_against_curly_quote_in_doc() {
    // Document contains U+2019 (RIGHT SINGLE QUOTATION MARK), expect is ASCII '.
    // The diff must also fold both forms before deciding what is changed so the
    // unchanged curly apostrophe survives the replace (the LLM emitting ASCII '
    // in `replace.text` must not silently rewrite the document's typography).
    let doc = make_simple_doc("p1", "the parties\u{2019} agreement stands");
    let tx = replace_transaction(
        "p1",
        "the parties' agreement",
        text_content("the parties' agreement is binding"),
    );
    let result = apply_transaction(&doc, &tx)
        .expect("ASCII apostrophe in `expect` must match curly apostrophe in document")
        .0;
    assert_eq!(
        accepted_text(&result, "p1"),
        "the parties\u{2019} agreement is binding"
    );
}

#[test]
fn replace_accepts_ascii_apostrophe_against_backtick_in_doc() {
    // Edge case: backtick (U+0060) is visually quote-ish in some sources.
    let doc = make_simple_doc("p1", "tom\u{0060}s draft");
    let tx = replace_transaction("p1", "tom's", text_content("tom's final"));
    let result = apply_transaction(&doc, &tx)
        .expect("ASCII apostrophe in `expect` must match backtick in document")
        .0;
    assert_eq!(accepted_text(&result, "p1"), "tom's final");
}

#[test]
fn replace_accepts_ascii_double_quote_against_curly_doubles_in_doc() {
    // Document has U+201C / U+201D, expect uses ASCII ". Like the apostrophe
    // case, the unchanged curly doubles must survive in the kept output —
    // the diff folds quote variants before alignment so they are not flagged
    // as Delete+Insert.
    let doc = make_simple_doc("p1", "\u{201C}force majeure\u{201D} clause");
    let tx = replace_transaction(
        "p1",
        "\"force majeure\" clause",
        text_content("\"force majeure\" clause is struck"),
    );
    let result = apply_transaction(&doc, &tx)
        .expect("ASCII double quotes in `expect` must match curly doubles in document")
        .0;
    assert_eq!(
        accepted_text(&result, "p1"),
        "\u{201C}force majeure\u{201D} clause is struck"
    );
}

#[test]
fn replace_accepts_ascii_hyphen_against_en_dash_in_doc() {
    // Document has an en dash (U+2013), expect has ASCII hyphen.
    let doc = make_simple_doc("p1", "pages 10\u{2013}20 of the exhibit");
    let tx = replace_transaction(
        "p1",
        "pages 10-20",
        text_content("pages 10 through 20 of the exhibit"),
    );
    let result = apply_transaction(&doc, &tx)
        .expect("ASCII hyphen in `expect` must match en dash in document")
        .0;
    assert_eq!(
        accepted_text(&result, "p1"),
        "pages 10 through 20 of the exhibit"
    );
}

#[test]
fn replace_accepts_ascii_hyphen_against_em_dash_in_doc() {
    // Document has an em dash (U+2014), expect has ASCII hyphen.
    let doc = make_simple_doc("p1", "the cap\u{2014}not the floor\u{2014}binds");
    let tx = replace_transaction(
        "p1",
        "cap-not the floor-binds",
        text_content("cap (not the floor) binds"),
    );
    let result = apply_transaction(&doc, &tx)
        .expect("ASCII hyphen in `expect` must match em dash in document")
        .0;
    assert_eq!(accepted_text(&result, "p1"), "cap (not the floor) binds");
}

#[test]
fn replace_accepts_three_dots_against_ellipsis_glyph_in_doc() {
    // Document has single-glyph ellipsis U+2026, expect has three ASCII dots.
    let doc = make_simple_doc("p1", "see exhibit A\u{2026} and exhibit B");
    let tx = replace_transaction(
        "p1",
        "exhibit A... and exhibit B",
        text_content("exhibit A and exhibit B (attached)"),
    );
    let result = apply_transaction(&doc, &tx)
        .expect("three ASCII dots in `expect` must match single-glyph ellipsis in document")
        .0;
    assert_eq!(
        accepted_text(&result, "p1"),
        "exhibit A and exhibit B (attached)"
    );
}

#[test]
fn replace_accepts_ascii_space_against_nbsp_in_doc() {
    // Document uses a non-breaking space (U+00A0); expect uses ASCII ' '.
    // The NBSP carries layout intent (keeps "Section" and "5.2" on the same
    // line), so the unchanged NBSP must survive the replace even when the
    // LLM emits an ASCII space in `replace.text`.
    let doc = make_simple_doc("p1", "Section\u{00A0}5.2 is amended");
    let tx = replace_transaction(
        "p1",
        "Section 5.2 is amended",
        text_content("Section 5.2 is deleted"),
    );
    let result = apply_transaction(&doc, &tx)
        .expect("ASCII space in `expect` must match NBSP in document")
        .0;
    assert_eq!(
        accepted_text(&result, "p1"),
        "Section\u{00A0}5.2 is deleted"
    );
}

#[test]
fn delete_accepts_ascii_apostrophe_against_curly_quote_in_doc() {
    // The same normalization must apply to delete-step precondition too.
    let p1 = make_para(
        "p1",
        normal_segment(vec![make_text("t1", "the parties\u{2019} waiver is final")]),
    );
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(p1))]);

    let tx = EditTransaction {
        steps: vec![EditStep::DeleteBlockRange {
            from_block_id: NodeId::from("p1"),
            to_block_id: NodeId::from("p1"),
            rationale: None,
            expect: "the parties' waiver".to_string(),
            semantic_hash: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    apply_transaction(&doc, &tx)
        .expect("delete-step `expect` must normalize curly apostrophe the same way replace does");
}

#[test]
fn replace_does_not_flag_curly_apostrophes_in_unchanged_regions() {
    // Regression: when the LLM's `replace.text` carries the FULL paragraph
    // text (as the Reply schema requires) and uses ASCII apostrophes
    // throughout, the diff inside apply_replace_paragraph_text used to flag
    // every curly→straight apostrophe in the unchanged surrounding prose as
    // its own Delete+Insert pair. The user-visible proposal preview ends up
    // showing spurious "apostrophe changed" markers next to the one real
    // edit. The diff must fold typographic variants before alignment so
    // unchanged apostrophes stay Normal.
    //
    // Scenario mirrors the reported bug: a paragraph with multiple curly
    // apostrophes where the LLM only meant to swap "ten (10) " for
    // "five (5) business ".
    let original = "Defendant\u{2019}s first set of interrogatories \
                    were served on April 1, 2025. Plaintiff\u{2019}s \
                    responses were due within ten (10) days of service.";
    let llm_replacement = "Defendant's first set of interrogatories \
                           were served on April 1, 2025. Plaintiff's \
                           responses were due within five (5) business \
                           days of service.";
    let llm_expect = "within ten (10) days";

    let doc = make_simple_doc("p1", original);
    let tx = replace_transaction("p1", llm_expect, text_content(llm_replacement));
    let result = apply_transaction(&doc, &tx)
        .expect("apply should succeed")
        .0;

    let para = get_para(&result, "p1");

    // The only Deleted segment should be the literal "ten (10)" phrase (the
    // trailing boundary space stays Normal, outside the tracked envelope).
    // Anything else — particularly the curly apostrophes — must be Normal.
    let deleted_text: String = para
        .segments
        .iter()
        .filter(|s| matches!(s.status, TrackingStatus::Deleted(_)))
        .flat_map(|s| s.inlines.iter())
        .filter_map(|i| match i {
            InlineNode::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        deleted_text, "ten (10)",
        "only the targeted phrase should be marked Deleted; \
         the unchanged boundary space and curly apostrophes in surrounding \
         prose must stay Normal (outside the tracked envelope)"
    );

    let inserted_text: String = para
        .segments
        .iter()
        .filter(|s| matches!(s.status, TrackingStatus::Inserted(_)))
        .flat_map(|s| s.inlines.iter())
        .filter_map(|i| match i {
            InlineNode::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        inserted_text, "five (5) business",
        "only the replacement phrase should be Inserted; \
         apostrophes from unchanged prose must not appear here"
    );

    // Accepted text keeps the document's typographic apostrophes intact.
    assert_eq!(
        accepted_text(&result, "p1"),
        "Defendant\u{2019}s first set of interrogatories \
         were served on April 1, 2025. Plaintiff\u{2019}s \
         responses were due within five (5) business days of service."
    );

    // Reject must reproduce the ORIGINAL verbatim — the real Word-fidelity
    // invariant. This is what makes the boundary placement above correct:
    // unchanged tokens (incl. the " days" boundary space) live outside the
    // tracked envelope, so reject restores the source exactly.
    assert_eq!(
        rejected_text(&result, "p1"),
        "Defendant\u{2019}s first set of interrogatories \
         were served on April 1, 2025. Plaintiff\u{2019}s \
         responses were due within ten (10) days of service."
    );
}

#[test]
fn replace_expect_mismatch_still_fires_on_genuinely_different_text() {
    // Normalization must not mask real mismatches: a completely different
    // expect string must still yield ExpectMismatch, not a false positive.
    let doc = make_simple_doc("p1", "the parties\u{2019} agreement");
    let tx = replace_transaction("p1", "jury trial waiver", text_content("some replacement"));
    let err = apply_transaction(&doc, &tx).unwrap_err();
    assert!(
        matches!(err, EditError::ExpectMismatch { .. }),
        "expected ExpectMismatch for genuinely-different expect, got: {err}"
    );
}

// ─── Category 9: Paragraphs inside table cells ──────────────────────────────
//
// The edit engine descends into table cells to locate paragraphs by their
// stable block IDs. All editing primitives (validation, inline diff,
// segment reconstruction, accept/reject) operate on the same paragraph
// model regardless of whether it lives at the top level or inside a cell.

fn make_table_cell(id: &str, blocks: Vec<BlockNode>) -> TableCellNode {
    TableCellNode {
        id: NodeId::from(id),
        blocks,
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

fn make_table_row(id: &str, cells: Vec<TableCellNode>) -> TableRowNode {
    TableRowNode {
        id: NodeId::from(id),
        cells,
        grid_before: 0,
        grid_after: 0,
        tracking_status: None,
        is_header: false,
        height: None,
        height_rule: None,
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

fn make_table(id: &str, rows: Vec<TableRowNode>) -> BlockNode {
    BlockNode::from(TableNode {
        id: NodeId::from(id),
        rows,
        structure_hash: String::new(),
        formatting: TableFormatting::default(),
        formatting_change: None,
    })
}

/// Build a doc with one top-level table containing a single 1x1 cell with
/// one paragraph. Useful for the simplest in-cell tests.
fn doc_with_cell_paragraph(table_id: &str, para_id: &str, text: &str) -> CanonDoc {
    let para = make_para(
        para_id,
        normal_segment(vec![make_text(&format!("{para_id}_t1"), text)]),
    );
    let cell = make_table_cell("c1", vec![BlockNode::from(para)]);
    let row = make_table_row("r1", vec![cell]);
    let table = make_table(table_id, vec![row]);
    make_doc(vec![normal_tracked_block(table)])
}

/// Locate a paragraph by id anywhere in the doc (including inside cells)
/// and return its visible text.
fn para_text_anywhere(doc: &CanonDoc, block_id: &str) -> String {
    let nid = NodeId::from(block_id);
    fn search<'a>(blocks: &'a [BlockNode], nid: &NodeId) -> Option<&'a ParagraphNode> {
        for block in blocks {
            match block {
                BlockNode::Paragraph(p) if p.id == *nid => return Some(p),
                BlockNode::Table(t) => {
                    for row in &t.rows {
                        for cell in &row.cells {
                            if let Some(p) = search(&cell.blocks, nid) {
                                return Some(p);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        None
    }
    let top_blocks: Vec<BlockNode> = doc.blocks.iter().map(|tb| tb.block.clone()).collect();
    let p = search(&top_blocks, &nid).expect("paragraph not found");
    let mut text = String::new();
    for seg in &p.segments {
        for inline in &seg.inlines {
            if let InlineNode::Text(t) = inline {
                text.push_str(&t.text);
            }
        }
    }
    text
}

#[test]
fn replace_paragraph_inside_table_cell_accept() {
    // I2 extended: accept_all after editing a paragraph inside a table cell
    // produces the replacement text. The cell wrapper and table structure
    // are preserved.
    let doc = doc_with_cell_paragraph("tbl1", "cp1", "hello world");
    let tx = replace_transaction("cp1", "hello", text_content("goodbye world"));
    let mut result = apply_transaction(&doc, &tx).expect("edit applies").0;
    accept_all(&mut result);
    assert_eq!(para_text_anywhere(&result, "cp1"), "goodbye world");
    // Table itself is still present at the top level.
    assert!(matches!(&result.blocks[0].block, BlockNode::Table(_)));
}

#[test]
fn replace_paragraph_inside_table_cell_reject() {
    // I3 extended: reject_all restores the original text inside the cell.
    let doc = doc_with_cell_paragraph("tbl1", "cp1", "hello world");
    let tx = replace_transaction("cp1", "hello", text_content("goodbye world"));
    let mut result = apply_transaction(&doc, &tx).expect("edit applies").0;
    reject_all_with_styles(&mut result, None);
    assert_eq!(para_text_anywhere(&result, "cp1"), "hello world");
}

#[test]
fn replace_paragraph_in_nested_table_cell() {
    // The engine must descend into nested tables. Build a 1x1 outer table
    // whose only cell contains a 1x1 inner table whose only cell contains
    // the target paragraph.
    let inner_para = make_para(
        "ip1",
        normal_segment(vec![make_text("ip1_t1", "deep text")]),
    );
    let inner_cell = make_table_cell("ic1", vec![BlockNode::from(inner_para)]);
    let inner_row = make_table_row("ir1", vec![inner_cell]);
    let inner_table = make_table("inner_tbl", vec![inner_row]);
    let outer_cell = make_table_cell("oc1", vec![inner_table]);
    let outer_row = make_table_row("or1", vec![outer_cell]);
    let outer_table = make_table("outer_tbl", vec![outer_row]);
    let doc = make_doc(vec![normal_tracked_block(outer_table)]);

    let tx = replace_transaction("ip1", "deep", text_content("shallower text"));
    let mut result = apply_transaction(&doc, &tx).expect("edit applies").0;
    accept_all(&mut result);
    assert_eq!(para_text_anywhere(&result, "ip1"), "shallower text");
}

#[test]
fn replace_targeting_table_block_still_rejected() {
    // Targeting the table block (not a paragraph inside it) must still
    // fail with NotAParagraph. The new descent logic only finds paragraphs.
    let doc = doc_with_cell_paragraph("tbl1", "cp1", "hello");
    let tx = replace_transaction("tbl1", "hello", text_content("new"));
    let err = apply_transaction(&doc, &tx).unwrap_err();
    assert!(
        matches!(
            err,
            EditError::NotAParagraph {
                actual_kind: "table",
                ..
            }
        ),
        "expected NotAParagraph(table) when targeting the table itself, got: {err}"
    );
}

#[test]
fn replace_in_cell_rejects_when_row_is_tracked_deleted() {
    // The MVP rule: a paragraph inside a tracked-deleted row cannot be
    // edited. The user must accept/reject the row deletion first.
    let para = make_para("cp1", normal_segment(vec![make_text("cp1_t1", "hello")]));
    let cell = make_table_cell("c1", vec![BlockNode::from(para)]);
    let mut row = make_table_row("r1", vec![cell]);
    row.tracking_status = Some(TrackingStatus::Deleted(test_revision()));
    let table = make_table("tbl1", vec![row]);
    let doc = make_doc(vec![normal_tracked_block(table)]);

    let tx = replace_transaction("cp1", "hello", text_content("new"));
    let err = apply_transaction(&doc, &tx).unwrap_err();
    assert!(
        matches!(
            err,
            EditError::BlockHasTrackedStatus {
                status: "deleted",
                ..
            }
        ),
        "expected BlockHasTrackedStatus(deleted) for tracked-deleted row, got: {err}"
    );
}

#[test]
fn replace_in_cell_rejects_when_cell_is_tracked_inserted() {
    // Same rule applies to cell-level tracking.
    let para = make_para("cp1", normal_segment(vec![make_text("cp1_t1", "hello")]));
    let mut cell = make_table_cell("c1", vec![BlockNode::from(para)]);
    cell.tracking_status = Some(TrackingStatus::Inserted(test_revision()));
    let row = make_table_row("r1", vec![cell]);
    let table = make_table("tbl1", vec![row]);
    let doc = make_doc(vec![normal_tracked_block(table)]);

    let tx = replace_transaction("cp1", "hello", text_content("new"));
    let err = apply_transaction(&doc, &tx).unwrap_err();
    assert!(
        matches!(
            err,
            EditError::BlockHasTrackedStatus {
                status: "inserted",
                ..
            }
        ),
        "expected BlockHasTrackedStatus(inserted) for tracked-inserted cell, got: {err}"
    );
}

#[test]
fn replace_in_cell_preserves_opaque_inlines() {
    // I1: opaque inlines inside cell paragraphs are still preserved across
    // edits. Same anchor inventory rules apply.
    let inlines = vec![
        make_text("t1", "before "),
        make_hyperlink("h1", "https://example.com", "link"),
        make_text("t2", " after"),
    ];
    let para = make_para("cp1", normal_segment(inlines));
    let cell = make_table_cell("c1", vec![BlockNode::from(para)]);
    let row = make_table_row("r1", vec![cell]);
    let table = make_table("tbl1", vec![row]);
    let doc = make_doc(vec![normal_tracked_block(table)]);

    // Replacement that omits the hyperlink anchor fails as before.
    let tx = replace_transaction("cp1", "before ", text_content("rewritten "));
    let err = apply_transaction(&doc, &tx).unwrap_err();
    assert!(
        matches!(err, EditError::OpaqueDestroyed { .. }),
        "expected OpaqueDestroyed when in-cell edit drops a hyperlink, got: {err}"
    );

    // Replacement that preserves the hyperlink anchor succeeds.
    let content = ParagraphContent {
        fragments: vec![
            ContentFragment::Text("rewritten ".to_string()),
            ContentFragment::PreservedInlineRef(NodeId::from("h1")),
            ContentFragment::Text(" tail".to_string()),
        ],
    };
    let tx_ok = replace_transaction("cp1", "before ", content);
    let mut result = apply_transaction(&doc, &tx_ok)
        .expect("in-cell edit applies")
        .0;
    accept_all(&mut result);
    assert_eq!(para_text_anywhere(&result, "cp1"), "rewritten  tail");
}

// ─── Category 10: Hyperlink text replacement ────────────────────────────────
//
// `ReplaceHyperlinkText` targets a hyperlink opaque by its NodeId and
// rewrites its display text. The hyperlink envelope (URL, anchor, r_id)
// and the enclosing paragraph structure are preserved. Tracked changes
// land *inside* the hyperlink: matched runs become Deleted, the new text
// is added as a single Inserted run. Accept/reject project these runs
// via `project_block_for_accept_reject`.

fn replace_hyperlink_transaction(
    hyperlink_id: &str,
    expect: &str,
    new_text: &str,
) -> EditTransaction {
    EditTransaction {
        steps: vec![EditStep::ReplaceHyperlinkText {
            hyperlink_id: NodeId::from(hyperlink_id),
            rationale: None,
            expect: expect.to_string(),
            new_text: new_text.to_string(),
            expect_href: None,
            expect_anchor: None,
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    }
}

/// Find a hyperlink opaque anywhere in the doc by id, return its current
/// display data.
fn get_hyperlink<'a>(doc: &'a CanonDoc, hyperlink_id: &str) -> &'a HyperlinkData {
    let nid = NodeId::from(hyperlink_id);
    for tb in &doc.blocks {
        if let BlockNode::Paragraph(p) = &tb.block {
            for seg in &p.segments {
                for inline in &seg.inlines {
                    if let InlineNode::OpaqueInline(o) = inline
                        && o.id == nid
                        && let OpaqueKind::Hyperlink(data) = &o.kind
                    {
                        return data;
                    }
                }
            }
        }
    }
    panic!("hyperlink '{hyperlink_id}' not found");
}

/// Build a doc with one paragraph that contains a single hyperlink.
fn doc_with_hyperlink(para_id: &str, hyperlink_id: &str, url: &str, text: &str) -> CanonDoc {
    let para = make_para(
        para_id,
        normal_segment(vec![
            make_text(&format!("{para_id}_t1"), "before "),
            make_hyperlink(hyperlink_id, url, text),
            make_text(&format!("{para_id}_t2"), " after"),
        ]),
    );
    make_doc(vec![normal_tracked_block(BlockNode::from(para))])
}

#[test]
fn replace_hyperlink_text_accept_yields_new_text() {
    let doc = doc_with_hyperlink("p1", "h1", "https://example.com", "old link");
    let tx = replace_hyperlink_transaction("h1", "old link", "new link");
    let mut result = apply_transaction(&doc, &tx)
        .expect("hyperlink edit applies")
        .0;
    accept_all(&mut result);
    let data = get_hyperlink(&result, "h1");
    assert_eq!(data.text, "new link");
    assert_eq!(data.url.as_deref(), Some("https://example.com"));
    // Runs collapse to a single Normal run with the new text.
    assert_eq!(data.runs.len(), 1);
    assert_eq!(data.runs[0].text, "new link");
}

#[test]
fn replace_hyperlink_text_reject_yields_old_text() {
    let doc = doc_with_hyperlink("p1", "h1", "https://example.com", "old link");
    let tx = replace_hyperlink_transaction("h1", "old link", "new link");
    let mut result = apply_transaction(&doc, &tx)
        .expect("hyperlink edit applies")
        .0;
    reject_all_with_styles(&mut result, None);
    let data = get_hyperlink(&result, "h1");
    assert_eq!(data.text, "old link");
    assert_eq!(data.url.as_deref(), Some("https://example.com"));
}

/// Selective-resolution counterpart to `replace_hyperlink_text_accept_yields_new_text`.
/// `ReplaceHyperlinkText` records its edit as per-run `TrackingStatus` inside
/// `HyperlinkData.runs` (not a segment-level status), and the Deleted/Inserted
/// runs it produces share ONE revision id (the transaction's `revision`, see
/// `rewrite_hyperlink_runs`). Selectively accepting that id — the real path
/// the MCP `accept_changes` verb drives — must reach that layer exactly like
/// `accept_all` does, not silently leave the hyperlink pending.
#[test]
fn resolve_selected_revisions_accepts_hyperlink_edit_by_id() {
    let doc = doc_with_hyperlink("p1", "h1", "https://example.com", "old link");
    let tx = replace_hyperlink_transaction("h1", "old link", "new link");
    let mut result = apply_transaction(&doc, &tx)
        .expect("hyperlink edit applies")
        .0;
    // H7: the resolver addresses revisions by their minted identity, not the
    // wire revision_id.
    let revision_id = get_hyperlink(&result, "h1")
        .runs
        .iter()
        .find_map(|r| match &r.status {
            TrackingStatus::Inserted(rev) => Some(rev.identity),
            _ => None,
        })
        .expect("edit records an Inserted run");

    stemma::resolve_selected_revisions_with_styles(
        &mut result,
        &std::collections::HashSet::from([revision_id]),
        stemma::ResolveSelectionAction::Accept,
        None,
    )
    .expect("the edit's own revision id must resolve");

    let data = get_hyperlink(&result, "h1");
    assert_eq!(data.text, "new link");
    assert_eq!(data.runs.len(), 1);
    assert_eq!(data.runs[0].text, "new link");
    assert_eq!(data.runs[0].status, TrackingStatus::Normal);
}

/// Selective-resolution counterpart to `replace_hyperlink_text_reject_yields_old_text`.
#[test]
fn resolve_selected_revisions_rejects_hyperlink_edit_by_id() {
    let doc = doc_with_hyperlink("p1", "h1", "https://example.com", "old link");
    let tx = replace_hyperlink_transaction("h1", "old link", "new link");
    let mut result = apply_transaction(&doc, &tx)
        .expect("hyperlink edit applies")
        .0;
    // H7: the resolver addresses revisions by their minted identity, not the
    // wire revision_id.
    let revision_id = get_hyperlink(&result, "h1")
        .runs
        .iter()
        .find_map(|r| match &r.status {
            TrackingStatus::Deleted(rev) => Some(rev.identity),
            _ => None,
        })
        .expect("edit records a Deleted run");

    stemma::resolve_selected_revisions_with_styles(
        &mut result,
        &std::collections::HashSet::from([revision_id]),
        stemma::ResolveSelectionAction::Reject,
        None,
    )
    .expect("the edit's own revision id must resolve");

    let data = get_hyperlink(&result, "h1");
    assert_eq!(data.text, "old link");
    assert_eq!(data.runs.len(), 1);
    assert_eq!(data.runs[0].text, "old link");
    assert_eq!(data.runs[0].status, TrackingStatus::Normal);
}

/// Domain rule (the completeness half of the fix): a selected id that
/// matches no carrier anywhere in the document must refuse loudly, listing
/// the unmatched id, rather than silently reporting success while mutating
/// nothing.
#[test]
fn resolve_selected_revisions_rejects_nonexistent_hyperlink_revision_id() {
    let doc = doc_with_hyperlink("p1", "h1", "https://example.com", "old link");
    let tx = replace_hyperlink_transaction("h1", "old link", "new link");
    let mut result = apply_transaction(&doc, &tx)
        .expect("hyperlink edit applies")
        .0;
    let before = result.clone();

    let err = stemma::resolve_selected_revisions_with_styles(
        &mut result,
        &std::collections::HashSet::from([999_999_u32]),
        stemma::ResolveSelectionAction::Accept,
        None,
    )
    .expect_err("an id matching no carrier must be refused");

    assert_eq!(err, vec![999_999_u32]);
    assert_eq!(result, before, "a refused request must mutate nothing");
}

#[test]
fn replace_hyperlink_text_preserves_url_and_anchor() {
    // Internal-anchor hyperlinks must keep their anchor on edit.
    let para = make_para(
        "p1",
        normal_segment(vec![InlineNode::from(OpaqueInlineNode {
            id: NodeId::from("h1"),
            kind: OpaqueKind::Hyperlink(HyperlinkData {
                url: None,
                anchor: Some("Section_5".to_string()),
                text: "See Section 5".to_string(),
                r_id: None,
                runs: vec![HyperlinkRun {
                    text: "See Section 5".to_string(),
                    rpr_xml: None,
                    status: TrackingStatus::Normal,
                }],
                extra_attrs: vec![("w:tooltip".to_string(), "click for details".to_string())],
            }),
            opaque_ref: "hyperlink_h1".to_string(),
            proof_ref: ProofRef {
                part: DocPart::DocumentXml,
                block_id: NodeId::from("p1"),
                docx_anchor: String::new(),
            },
            wrapper_marks: Vec::new(),
            wrapper_style_props: StyleProps::default(),
            raw_xml: None,
            content_hash: None,
        })]),
    );
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    let tx = replace_hyperlink_transaction("h1", "Section 5", "Section 6");
    let mut result = apply_transaction(&doc, &tx)
        .expect("hyperlink edit applies")
        .0;
    accept_all(&mut result);
    let data = get_hyperlink(&result, "h1");
    assert_eq!(data.text, "See Section 6");
    assert_eq!(data.anchor.as_deref(), Some("Section_5"));
    assert!(data.url.is_none());
    // Tooltip and other extra attrs survive.
    assert_eq!(data.extra_attrs.len(), 1);
    assert_eq!(data.extra_attrs[0].0, "w:tooltip");
}

#[test]
fn replace_hyperlink_text_mid_word_splits_correctly() {
    let doc = doc_with_hyperlink("p1", "h1", "https://example.com", "click here for details");
    let tx = replace_hyperlink_transaction("h1", "here", "now");
    let mut result = apply_transaction(&doc, &tx)
        .expect("hyperlink edit applies")
        .0;
    accept_all(&mut result);
    let data = get_hyperlink(&result, "h1");
    assert_eq!(data.text, "click now for details");
}

#[test]
fn replace_hyperlink_text_records_tracked_change_before_projection() {
    let doc = doc_with_hyperlink("p1", "h1", "https://example.com", "old");
    let tx = replace_hyperlink_transaction("h1", "old", "new");
    let result = apply_transaction(&doc, &tx)
        .expect("hyperlink edit applies")
        .0;
    let data = get_hyperlink(&result, "h1");
    // Before any accept/reject projection: a Deleted run for "old" plus
    // an Inserted run for "new". The pre-projection `text` field reflects
    // the surviving (non-deleted) text — only the inserted run survives,
    // so `text` is "new".
    let statuses: Vec<&TrackingStatus> = data.runs.iter().map(|r| &r.status).collect();
    assert!(
        statuses
            .iter()
            .any(|s| matches!(s, TrackingStatus::Deleted(_))),
        "expected at least one Deleted run, got statuses: {statuses:?}"
    );
    assert!(
        statuses
            .iter()
            .any(|s| matches!(s, TrackingStatus::Inserted(_))),
        "expected at least one Inserted run, got statuses: {statuses:?}"
    );
}

#[test]
fn replace_hyperlink_expect_mismatch_returns_error() {
    let doc = doc_with_hyperlink("p1", "h1", "https://example.com", "click here");
    let tx = replace_hyperlink_transaction("h1", "missing", "new");
    let err = apply_transaction(&doc, &tx).unwrap_err();
    assert!(
        matches!(err, EditError::ExpectMismatch { .. }),
        "expected ExpectMismatch for missing substring, got: {err}"
    );
}

#[test]
fn replace_hyperlink_nonexistent_id_returns_hyperlink_not_found() {
    let doc = doc_with_hyperlink("p1", "h1", "https://example.com", "click here");
    let tx = replace_hyperlink_transaction("no_such_id", "click", "tap");
    let err = apply_transaction(&doc, &tx).unwrap_err();
    assert!(
        matches!(err, EditError::HyperlinkNotFound { .. }),
        "expected HyperlinkNotFound for unknown id, got: {err}"
    );
}

#[test]
fn replace_hyperlink_targeting_non_hyperlink_returns_not_a_hyperlink() {
    // Build a doc where "f1" is a field opaque, not a hyperlink.
    let para = make_para(
        "p1",
        normal_segment(vec![
            make_text("p1_t1", "before "),
            make_field("f1", FieldKind::Simple),
            make_text("p1_t2", " after"),
        ]),
    );
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);
    let tx = replace_hyperlink_transaction("f1", "PAGE", "new");
    let err = apply_transaction(&doc, &tx).unwrap_err();
    assert!(
        matches!(
            err,
            EditError::NotAHyperlink {
                actual_kind: "field",
                ..
            }
        ),
        "expected NotAHyperlink(field), got: {err}"
    );
}

#[test]
fn replace_hyperlink_with_existing_tracked_change_is_rejected() {
    // A hyperlink that already has tracked changes in its runs is off-
    // limits for the MVP. The caller must accept/reject first.
    let mut existing = HyperlinkData {
        url: Some("https://example.com".to_string()),
        anchor: None,
        text: "click here".to_string(),
        r_id: Some("rId1".to_string()),
        runs: vec![
            HyperlinkRun {
                text: "click ".to_string(),
                rpr_xml: None,
                status: TrackingStatus::Normal,
            },
            HyperlinkRun {
                text: "here".to_string(),
                rpr_xml: None,
                status: TrackingStatus::Inserted(test_revision()),
            },
        ],
        extra_attrs: vec![],
    };
    // Keep `text` in sync.
    existing.text = existing.runs.iter().map(|r| r.text.as_str()).collect();

    let para = make_para(
        "p1",
        normal_segment(vec![InlineNode::from(OpaqueInlineNode {
            id: NodeId::from("h1"),
            kind: OpaqueKind::Hyperlink(existing),
            opaque_ref: "hyperlink_h1".to_string(),
            proof_ref: ProofRef {
                part: DocPart::DocumentXml,
                block_id: NodeId::from("p1"),
                docx_anchor: String::new(),
            },
            wrapper_marks: Vec::new(),
            wrapper_style_props: StyleProps::default(),
            raw_xml: None,
            content_hash: None,
        })]),
    );
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    let tx = replace_hyperlink_transaction("h1", "here", "there");
    let err = apply_transaction(&doc, &tx).unwrap_err();
    assert!(
        matches!(err, EditError::HyperlinkContainsTrackedChanges { .. }),
        "expected HyperlinkContainsTrackedChanges, got: {err}"
    );
}

#[test]
fn replace_hyperlink_text_inside_table_cell() {
    // The hyperlink edit must also work when the enclosing paragraph
    // lives inside a table cell.
    let para = make_para(
        "cp1",
        normal_segment(vec![
            make_text("cp1_t1", "see "),
            make_hyperlink("h1", "https://example.com", "documentation"),
        ]),
    );
    let cell = make_table_cell("c1", vec![BlockNode::from(para)]);
    let row = make_table_row("r1", vec![cell]);
    let table = make_table("tbl1", vec![row]);
    let doc = make_doc(vec![normal_tracked_block(table)]);

    let tx = replace_hyperlink_transaction("h1", "documentation", "the docs");
    let mut result = apply_transaction(&doc, &tx)
        .expect("in-cell hyperlink edit applies")
        .0;
    accept_all(&mut result);
    // After accept, the cell paragraph contains "see " followed by the
    // hyperlink whose new text is "the docs".
    let nid = NodeId::from("h1");
    let mut found = None;
    for tb in &result.blocks {
        if let BlockNode::Table(t) = &tb.block {
            for row in &t.rows {
                for cell in &row.cells {
                    for block in &cell.blocks {
                        if let BlockNode::Paragraph(p) = block {
                            for seg in &p.segments {
                                for inline in &seg.inlines {
                                    if let InlineNode::OpaqueInline(o) = inline
                                        && o.id == nid
                                        && let OpaqueKind::Hyperlink(data) = &o.kind
                                    {
                                        found = Some(data.text.clone());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    assert_eq!(found.as_deref(), Some("the docs"));
}

#[test]
fn replace_hyperlink_preserves_run_formatting_on_kept_text() {
    // A hyperlink whose displayed text spans two runs, only the second
    // of which is bold. Replacing only the bold portion should keep the
    // unbold prefix unchanged and yield a new inserted run inheriting
    // formatting from the nearest kept/deleted neighbor.
    let bold_rpr = b"<w:rPr><w:b/></w:rPr>".to_vec();
    let mut data = HyperlinkData {
        url: Some("https://example.com".to_string()),
        anchor: None,
        text: String::new(),
        r_id: Some("rId1".to_string()),
        runs: vec![
            HyperlinkRun {
                text: "click ".to_string(),
                rpr_xml: None,
                status: TrackingStatus::Normal,
            },
            HyperlinkRun {
                text: "HERE".to_string(),
                rpr_xml: Some(bold_rpr.clone()),
                status: TrackingStatus::Normal,
            },
        ],
        extra_attrs: vec![],
    };
    data.text = data.runs.iter().map(|r| r.text.as_str()).collect();

    let para = make_para(
        "p1",
        normal_segment(vec![InlineNode::from(OpaqueInlineNode {
            id: NodeId::from("h1"),
            kind: OpaqueKind::Hyperlink(data),
            opaque_ref: "hyperlink_h1".to_string(),
            proof_ref: ProofRef {
                part: DocPart::DocumentXml,
                block_id: NodeId::from("p1"),
                docx_anchor: String::new(),
            },
            wrapper_marks: Vec::new(),
            wrapper_style_props: StyleProps::default(),
            raw_xml: None,
            content_hash: None,
        })]),
    );
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    let tx = replace_hyperlink_transaction("h1", "HERE", "THERE");
    let mut result = apply_transaction(&doc, &tx)
        .expect("hyperlink edit applies")
        .0;
    accept_all(&mut result);
    let data = get_hyperlink(&result, "h1");
    assert_eq!(data.text, "click THERE");
    // The new inserted run inherits the bold rPr from its left neighbor
    // (the matched bold run that was marked deleted).
    let inserted = data
        .runs
        .iter()
        .find(|r| r.text == "THERE")
        .expect("inserted run present after accept");
    assert_eq!(inserted.rpr_xml.as_deref(), Some(bold_rpr.as_slice()));
}

// ─── Inserted paragraphs must not clone the exemplar's section break ─────────
//
// `resolve_paragraph_spec` builds an inserted paragraph by cloning the role
// exemplar and rebuilding only its `segments`. Every other pPr field rides
// along on the clone. When the chosen exemplar is a section-final paragraph it
// carries a mid-document `w:sectPr` (`section_properties`) plus the identity
// attributes `w14:paraId` / `w14:textId`. None of those are FORMATTING — they
// are the exemplar's position in the document — so the insert must strip them.
// Domain rule: an inserted paragraph inherits the exemplar's formatting, never
// its position-bound, one-place-only state. A new paragraph is never a section
// boundary and must not reuse another paragraph's unique id.

/// Build a paragraph carrying a mid-document section break (a continuous
/// `w:sectPr`) plus w14 identity attributes — the shape of a section-final
/// paragraph that role selection picks as an exemplar (`indices[0]`).
fn section_break_para(id: &str, text: &str) -> ParagraphNode {
    let mut p = make_para(
        id,
        normal_segment(vec![make_text(&format!("{id}_t"), text)]),
    );
    p.section_properties = Some(SectionProperties {
        section_type: Some(SectionType::Continuous),
        page_width: Some(12240),
        page_height: Some(15840),
        margin_top: Some(1440),
        margin_bottom: Some(1440),
        ..SectionProperties::default()
    });
    p.para_id = Some("11112222".to_string());
    p.text_id = Some("33334444".to_string());
    p
}

/// Count paragraph-level section breaks (`section_properties`) across the body,
/// including inside table cells. One `SectionProperties` serializes to exactly
/// one `<w:sectPr>`, so this is the IR-level equivalent of the serialized
/// sectPr count.
fn section_break_count(doc: &CanonDoc) -> usize {
    fn count_block(block: &BlockNode) -> usize {
        match block {
            BlockNode::Paragraph(p) => usize::from(p.section_properties.is_some()),
            BlockNode::Table(t) => t
                .rows
                .iter()
                .flat_map(|r| r.cells.iter())
                .flat_map(|c| c.blocks.iter())
                .map(count_block)
                .sum(),
            BlockNode::OpaqueBlock(_) => 0,
        }
    }
    doc.blocks.iter().map(|tb| count_block(&tb.block)).sum()
}

#[test]
fn insert_after_section_break_exemplar_does_not_duplicate_sectpr() {
    // p1 is the section-final paragraph (carries the mid-doc break) and, being
    // the first paragraph of the default body role's group, is the exemplar
    // that `resolve_paragraph_spec` clones. p2 follows in the next section.
    let p1 = section_break_para("p1", "section one");
    let p2 = make_para("p2", normal_segment(vec![make_text("p2_t", "section two")]));
    let doc = make_doc(vec![
        normal_tracked_block(BlockNode::from(p1)),
        normal_tracked_block(BlockNode::from(p2)),
    ]);
    assert_eq!(
        section_break_count(&doc),
        1,
        "fixture has one section break"
    );

    // "insert two list items" after the section-final paragraph.
    let tx = EditTransaction {
        steps: vec![EditStep::InsertParagraphs {
            anchor_block_id: NodeId::from("p1"),
            position: InsertPosition::After,
            rationale: None,
            blocks: vec![
                BlockSpec::Paragraph(ParagraphBlockSpec {
                    role: Some("default".to_string()),
                    content: parse_paragraph_markup("first item").unwrap(),
                    restart_numbering: false,
                    list: None,
                }),
                BlockSpec::Paragraph(ParagraphBlockSpec {
                    role: Some("default".to_string()),
                    content: parse_paragraph_markup("second item").unwrap(),
                    restart_numbering: false,
                    list: None,
                }),
            ],
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    let result = apply_transaction(&doc, &tx).unwrap().0;

    // Post-condition: the insert added no section breaks. Without the fix each
    // inserted paragraph clones p1's `section_properties`, so the count is 3.
    assert_eq!(
        section_break_count(&result),
        1,
        "inserting paragraphs must not add section breaks"
    );

    // Each inserted paragraph carries no position-bound state: no sectPr, and
    // no cloned w14 identity (which would collide with p1's).
    let inserted: Vec<&ParagraphNode> = result
        .blocks
        .iter()
        .filter(|tb| matches!(tb.status, TrackingStatus::Inserted(_)))
        .filter_map(|tb| match &tb.block {
            BlockNode::Paragraph(p) => Some(p.as_ref()),
            _ => None,
        })
        .collect();
    assert_eq!(inserted.len(), 2, "two inserted paragraphs");
    for p in inserted {
        assert!(
            p.section_properties.is_none(),
            "inserted paragraph must not carry a section break"
        );
        assert!(
            p.para_id.is_none(),
            "inserted paragraph must not clone the exemplar's w14:paraId"
        );
        assert!(
            p.text_id.is_none(),
            "inserted paragraph must not clone the exemplar's w14:textId"
        );
    }
}

#[test]
fn insert_table_cell_paragraph_does_not_clone_exemplar_sectpr() {
    // The in-cell path: a `BlockSpec::Table` whose cell contains a paragraph
    // spec resolves that paragraph through the SAME exemplar clone. On a wild
    // doc this shipped a cell-level `w:sectPr` whose footerReference rel is not
    // registered for the cell part — serialize died with I-REL-001. Here we
    // assert the resolved cell paragraph carries no section break.
    let p1 = section_break_para("p1", "section one");
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(p1))]);

    let tx = EditTransaction {
        steps: vec![EditStep::InsertParagraphs {
            anchor_block_id: NodeId::from("p1"),
            position: InsertPosition::After,
            rationale: None,
            blocks: vec![BlockSpec::Table(TableBlockSpec {
                formatting: None,
                rows: vec![TableRowSpec {
                    is_header: false,
                    height: None,
                    height_rule: None,
                    cells: vec![TableCellSpec {
                        content: vec![BlockSpec::Paragraph(ParagraphBlockSpec {
                            role: Some("default".to_string()),
                            content: parse_paragraph_markup("cell text").unwrap(),
                            restart_numbering: false,
                            list: None,
                        })],
                        merge_h: None,
                        merge_v: None,
                        formatting: None,
                    }],
                }],
            })],
        }],
        summary: None,
        materialization_mode: MaterializationMode::TrackedChange,
        revision: test_revision(),
    };

    let result = apply_transaction(&doc, &tx).unwrap().0;

    // Only p1's original break survives; the cell paragraph carries none.
    assert_eq!(
        section_break_count(&result),
        1,
        "table-cell insert must not add a section break"
    );
}

// ─── `expect` search space is the VISIBLE PENDING text ───────────────────────
// The `expect` precondition is a staleness guard over the pending state a caller
// edits: Normal ∪ Inserted segments, never struck (`Deleted`) text. These pin
// that boundary — an `expect` that survives only inside a struck original must
// refuse (not silently overwrite the pending change), while a re-edit of one's
// own pending insertion, and an edit anchored on still-visible Normal text, must
// still apply.

/// A stale re-edit whose `expect` token exists ONLY inside a `Deleted` segment
/// must refuse with `ExpectMismatch` — the struck original is not the pending
/// text being edited. Without the visible-text fix this APPLIES and silently
/// overwrites the pending sentinel insertion.
#[test]
fn stale_expect_matching_only_deleted_segment_refuses() {
    // Pending state after a first tracked replace: the original "Events" is
    // struck (Deleted), a sentinel is inserted. "Events" now lives ONLY in the
    // struck segment.
    let segments = vec![
        TrackedSegment {
            status: TrackingStatus::Deleted(test_revision()),
            inlines: vec![make_text("t1", "Events happen here")],
        },
        TrackedSegment {
            status: TrackingStatus::Inserted(test_revision()),
            inlines: vec![make_text("t2", "QQP2SENTQQ happen here")],
        },
    ];
    let para = make_para("p1", segments);
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    let tx = replace_transaction("p1", "Events", text_content("QQP2STALEQQ"));
    let err = apply_transaction(&doc, &tx).unwrap_err();
    assert!(
        matches!(err, EditError::ExpectMismatch { .. }),
        "expect that matches only struck (Deleted) text must refuse, got {err:?}"
    );
}

/// A re-edit of one's OWN pending insertion still applies: `expect` matching an
/// `Inserted` (visible-pending) segment is intentional support for editing
/// not-yet-accepted text. Guards against over-correcting the fix to exclude
/// `Inserted` alongside `Deleted`.
#[test]
fn expect_matching_inserted_pending_segment_still_applies() {
    let segments = vec![TrackedSegment {
        status: TrackingStatus::Inserted(test_revision()),
        inlines: vec![make_text("t1", "freshly inserted text")],
    }];
    let para = make_para("p1", segments);
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    let tx = replace_transaction("p1", "freshly", text_content("rewritten"));
    let result = apply_transaction(&doc, &tx)
        .expect("expect matching own pending Inserted text applies (re-edit)")
        .0;
    assert_eq!(accepted_text(&result, "p1"), "rewritten");
}

/// A legitimate `expect` anchored on still-visible `Normal` text applies even
/// when the paragraph ALSO carries a struck `Deleted` segment — the deleted text
/// is simply not part of the search space, but the visible text still is.
#[test]
fn expect_matching_visible_normal_text_applies_despite_deleted_segment() {
    let segments = vec![
        TrackedSegment {
            status: TrackingStatus::Normal,
            inlines: vec![make_text("t1", "kept ")],
        },
        TrackedSegment {
            status: TrackingStatus::Deleted(test_revision()),
            inlines: vec![make_text("t2", "struck original")],
        },
        TrackedSegment {
            status: TrackingStatus::Inserted(test_revision()),
            inlines: vec![make_text("t3", "pending replacement")],
        },
    ];
    let para = make_para("p1", segments);
    let doc = make_doc(vec![normal_tracked_block(BlockNode::from(para))]);

    let tx = replace_transaction("p1", "kept", text_content("new body"));
    let result = apply_transaction(&doc, &tx)
        .expect("expect matching visible Normal text applies despite a deleted segment")
        .0;
    assert_eq!(accepted_text(&result, "p1"), "new body");
}
