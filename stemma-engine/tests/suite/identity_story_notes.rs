//! Story-block accept/reject identity, post the domain/serialize carve-outs.
//!
//! The story domain types (FootnoteStory / EndnoteStory / CommentStory) moved
//! to `domain/story.rs`; the materializer and accept_all / reject_all still
//! operate on `TrackedBlock` / `TrackedSegment` regardless of which story a
//! block lives in. This test pins that behaviour: a document whose **only**
//! tracked changes live inside a footnote, an endnote, and a comment story
//! must accept to the inserted text and reject to the deleted text, with the
//! stories retained either way.
//!
//! These are post-conditions derived from the tracked-change model (accept =
//! take inserted, drop deleted; reject = take deleted, drop inserted), not a
//! transcription of current output.

use stemma::{
    BlockNode, CanonDoc, CommentStory, EndnoteStory, FootnoteStory, InlineNode, NodeId, NoteType,
    ParagraphNode, RevisionInfo, StyleProps, TextNode, TrackedBlock, TrackedSegment,
    TrackingStatus, accept_all, normal_tracked_block, reject_all_with_styles,
};

fn rev() -> RevisionInfo {
    RevisionInfo {
        revision_id: 7,
        identity: 0,
        author: Some("story-identity".to_string()),
        date: Some("2026-06-01T00:00:00Z".to_string()),
        apply_op_id: None,
    }
}

fn text(id: &str, t: &str) -> InlineNode {
    InlineNode::from(TextNode {
        id: NodeId::from(id),
        text_role: None,
        text: t.to_string(),
        marks: vec![],
        style_props: StyleProps::default(),
        rpr_authored: stemma::domain::RunRprAuthored::default(),
        formatting_change: None,
    })
}

fn seg(status: TrackingStatus, inlines: Vec<InlineNode>) -> TrackedSegment {
    TrackedSegment { status, inlines }
}

/// A paragraph with three segments: a Normal anchor, an Inserted span, and a
/// Deleted span. Accept keeps Normal + Inserted; reject keeps Normal +
/// Deleted. The Normal anchor guarantees the block never empties, so the
/// story is retained in both directions.
fn tracked_paragraph(id: &str) -> ParagraphNode {
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
        segments: vec![
            seg(
                TrackingStatus::Normal,
                vec![text(&format!("{id}_n"), "keep ")],
            ),
            seg(
                TrackingStatus::Inserted(rev()),
                vec![text(&format!("{id}_i"), "ins")],
            ),
            seg(
                TrackingStatus::Deleted(rev()),
                vec![text(&format!("{id}_d"), "del")],
            ),
        ],
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
        formatting_change: None,
        preserved_ppr: Vec::new(),
    }
}

fn tracked_block(id: &str) -> TrackedBlock {
    normal_tracked_block(BlockNode::from(tracked_paragraph(id)))
}

/// Document body is Normal; the only tracked changes are inside the stories.
fn doc_with_tracked_stories() -> CanonDoc {
    CanonDoc {
        id: NodeId::from("doc-story-identity"),
        // Body has one all-Normal block: no body tracked changes.
        blocks: vec![normal_tracked_block(BlockNode::from(ParagraphNode {
            id: NodeId::from("body_p1"),
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
            segments: vec![seg(TrackingStatus::Normal, vec![text("body_t", "body")])],
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
            formatting_change: None,
            preserved_ppr: Vec::new(),
        }))],
        meta: stemma::DocMeta {
            schema_version: stemma::SCHEMA_VERSION_V0.to_string(),
            docx_fingerprint: stemma::DocFingerprint("story-identity".to_string()),
            internal_ids_version: stemma::INTERNAL_IDS_VERSION_V0.to_string(),
        },
        headers: vec![],
        footers: vec![],
        footnotes: vec![FootnoteStory {
            id: "1".to_string(),
            note_type: NoteType::Normal,
            blocks: vec![tracked_block("fn_p1")],
            content_hash: String::new(),
        }],
        endnotes: vec![EndnoteStory {
            id: "1".to_string(),
            note_type: NoteType::Normal,
            blocks: vec![tracked_block("en_p1")],
            content_hash: String::new(),
        }],
        comments: vec![CommentStory {
            id: "1".to_string(),
            author: Some("reviewer".to_string()),
            date: Some("2026-06-01T00:00:00Z".to_string()),
            blocks: vec![tracked_block("cm_p1")],
            content_hash: String::new(),
            tracking_status: None,
        }],
        comments_extended: vec![],
        body_section_properties: None,
        body_section_property_change: None,
        compat_settings: stemma::CompatSettings::default(),
        even_and_odd_headers: None,
        document_background: None,
        document_protection: None,
    }
}

/// Visible text of the first paragraph in a block list.
fn first_para_text(blocks: &[TrackedBlock]) -> String {
    let BlockNode::Paragraph(p) = &blocks.first().expect("at least one block").block else {
        panic!("expected paragraph block");
    };
    let mut out = String::new();
    for s in &p.segments {
        for inline in &s.inlines {
            if let InlineNode::Text(t) = inline {
                out.push_str(&t.text);
            }
        }
    }
    out
}

/// accept_all: each story keeps Normal + Inserted text, drops Deleted; the
/// stories themselves are retained (non-empty blocks).
#[test]
fn accept_all_takes_inserted_in_every_story() {
    let mut doc = doc_with_tracked_stories();
    accept_all(&mut doc);

    assert_eq!(doc.footnotes.len(), 1, "footnote story retained on accept");
    assert_eq!(doc.endnotes.len(), 1, "endnote story retained on accept");
    assert_eq!(doc.comments.len(), 1, "comment story retained on accept");

    assert_eq!(first_para_text(&doc.footnotes[0].blocks), "keep ins");
    assert_eq!(first_para_text(&doc.endnotes[0].blocks), "keep ins");
    assert_eq!(first_para_text(&doc.comments[0].blocks), "keep ins");

    // No tracked segments survive accept in any story.
    for blocks in [
        &doc.footnotes[0].blocks,
        &doc.endnotes[0].blocks,
        &doc.comments[0].blocks,
    ] {
        let BlockNode::Paragraph(p) = &blocks[0].block else {
            panic!("paragraph");
        };
        assert!(
            p.segments
                .iter()
                .all(|s| s.status == TrackingStatus::Normal),
            "accept_all must leave only Normal segments in the story"
        );
    }
}

/// reject_all: each story keeps Normal + Deleted text, drops Inserted; the
/// stories themselves are retained.
#[test]
fn reject_all_takes_deleted_in_every_story() {
    let mut doc = doc_with_tracked_stories();
    reject_all_with_styles(&mut doc, None);

    assert_eq!(doc.footnotes.len(), 1, "footnote story retained on reject");
    assert_eq!(doc.endnotes.len(), 1, "endnote story retained on reject");
    assert_eq!(doc.comments.len(), 1, "comment story retained on reject");

    assert_eq!(first_para_text(&doc.footnotes[0].blocks), "keep del");
    assert_eq!(first_para_text(&doc.endnotes[0].blocks), "keep del");
    assert_eq!(first_para_text(&doc.comments[0].blocks), "keep del");

    for blocks in [
        &doc.footnotes[0].blocks,
        &doc.endnotes[0].blocks,
        &doc.comments[0].blocks,
    ] {
        let BlockNode::Paragraph(p) = &blocks[0].block else {
            panic!("paragraph");
        };
        assert!(
            p.segments
                .iter()
                .all(|s| s.status == TrackingStatus::Normal),
            "reject_all must leave only Normal segments in the story"
        );
    }
}
