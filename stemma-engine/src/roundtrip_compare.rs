//! Structural comparison of two `CanonDoc` instances for roundtrip fidelity testing.
//!
//! Compares semantically meaningful fields and ignores ephemeral data
//! (node IDs, proof refs, synthesized text, computed hashes, raw XML blobs).

use crate::domain::*;

/// A single difference found between two CanonDoc instances.
#[derive(Debug, Clone)]
pub struct Difference {
    /// Dotted path to the field that differs (e.g., "blocks[0].paragraph.style_id").
    pub path: String,
    /// Human-readable description of the left value.
    pub left: String,
    /// Human-readable description of the right value.
    pub right: String,
}

impl std::fmt::Display for Difference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: left={}, right={}", self.path, self.left, self.right)
    }
}

/// Compare ONE pair of blocks under the same fidelity-vs-ephemeral
/// classification as [`compare_canon_docs`] (node ids and other parse-time
/// artifacts are addressing, not content). This is the equality the audit's
/// untouched proof runs per paired block (`crate::audit`): two independent
/// parses of the same untouched content must compare equal even though the
/// importer renumbered their internal ids.
pub fn compare_tracked_block_pair(a: &TrackedBlock, b: &TrackedBlock) -> Vec<Difference> {
    let mut diffs = Vec::new();
    compare_tracked_blocks(
        &mut diffs,
        "block",
        std::slice::from_ref(a),
        std::slice::from_ref(b),
    );
    diffs
}

/// Compare two CanonDoc instances structurally, ignoring ephemeral fields.
/// Returns a list of differences (empty = structurally equivalent).
pub fn compare_canon_docs(a: &CanonDoc, b: &CanonDoc) -> Vec<Difference> {
    let mut diffs = Vec::new();
    // Exhaustive destructure of CanonDoc: a new top-level field fails to compile
    // until it is classified as fidelity or justified ephemeral.
    let CanonDoc {
        id: _,   // ephemeral NodeId
        meta: _, // schema/version + docx fingerprint — provenance, not content
        blocks,
        headers,
        footers,
        footnotes,
        endnotes,
        comments,
        comments_extended,
        body_section_properties,
        body_section_property_change,
        compat_settings,
        even_and_odd_headers,
        document_background,
        document_protection,
    } = a;
    let CanonDoc {
        id: _,
        meta: _,
        blocks: b_blocks,
        headers: b_headers,
        footers: b_footers,
        footnotes: b_footnotes,
        endnotes: b_endnotes,
        comments: b_comments,
        comments_extended: b_comments_extended,
        body_section_properties: b_body_section_properties,
        body_section_property_change: b_body_section_property_change,
        compat_settings: b_compat_settings,
        even_and_odd_headers: b_even_and_odd_headers,
        document_background: b_document_background,
        document_protection: b_document_protection,
    } = b;
    compare_tracked_blocks(&mut diffs, "blocks", blocks, b_blocks);
    compare_opt_section_properties(
        &mut diffs,
        "body_section_properties",
        body_section_properties,
        b_body_section_properties,
    );
    compare_opt_section_property_change(
        &mut diffs,
        "body_section_property_change",
        body_section_property_change,
        b_body_section_property_change,
    );
    compare_compat_settings(
        &mut diffs,
        "compat_settings",
        compat_settings,
        b_compat_settings,
    );
    compare_val(
        &mut diffs,
        "even_and_odd_headers",
        even_and_odd_headers,
        b_even_and_odd_headers,
    );
    compare_val(
        &mut diffs,
        "document_background",
        document_background,
        b_document_background,
    );
    compare_val(
        &mut diffs,
        "document_protection",
        document_protection,
        b_document_protection,
    );
    compare_headers(&mut diffs, "headers", headers, b_headers);
    compare_footers(&mut diffs, "footers", footers, b_footers);
    compare_footnotes(&mut diffs, "footnotes", footnotes, b_footnotes);
    compare_endnotes(&mut diffs, "endnotes", endnotes, b_endnotes);
    compare_comments(&mut diffs, "comments", comments, b_comments);
    compare_comments_extended(
        &mut diffs,
        "comments_extended",
        comments_extended,
        b_comments_extended,
    );
    diffs
}

fn compare_comments_extended(
    diffs: &mut Vec<Difference>,
    path: &str,
    a: &[CommentExtended],
    b: &[CommentExtended],
) {
    if a.len() != b.len() {
        diff(diffs, &format!("{path}.len"), a.len(), b.len());
        return;
    }
    for (i, (ca, cb)) in a.iter().zip(b.iter()).enumerate() {
        let p = format!("{path}[{i}]");
        // Exhaustive destructure of CommentExtended (threading/resolve sidecar).
        let CommentExtended {
            para_id,
            para_id_parent,
            done,
        } = ca;
        let CommentExtended {
            para_id: b_para_id,
            para_id_parent: b_para_id_parent,
            done: b_done,
        } = cb;
        compare_val(diffs, &format!("{p}.para_id"), para_id, b_para_id);
        compare_opt(
            diffs,
            &format!("{p}.para_id_parent"),
            para_id_parent,
            b_para_id_parent,
        );
        compare_val(diffs, &format!("{p}.done"), done, b_done);
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn diff(
    diffs: &mut Vec<Difference>,
    path: &str,
    left: impl std::fmt::Debug,
    right: impl std::fmt::Debug,
) {
    diffs.push(Difference {
        path: path.to_string(),
        left: format!("{left:?}"),
        right: format!("{right:?}"),
    });
}

fn compare_opt<T: PartialEq + std::fmt::Debug>(
    diffs: &mut Vec<Difference>,
    path: &str,
    a: &Option<T>,
    b: &Option<T>,
) {
    if a != b {
        diff(diffs, path, a, b);
    }
}

fn compare_val<T: PartialEq + std::fmt::Debug>(
    diffs: &mut Vec<Difference>,
    path: &str,
    a: &T,
    b: &T,
) {
    if a != b {
        diff(diffs, path, a, b);
    }
}

// ---------------------------------------------------------------------------
// CompatSettings
// ---------------------------------------------------------------------------

fn compare_compat_settings(
    diffs: &mut Vec<Difference>,
    path: &str,
    a: &CompatSettings,
    b: &CompatSettings,
) {
    if a != b {
        diff(diffs, path, a, b);
    }
}

// ---------------------------------------------------------------------------
// SectionProperties
// ---------------------------------------------------------------------------

fn compare_opt_section_properties(
    diffs: &mut Vec<Difference>,
    path: &str,
    a: &Option<SectionProperties>,
    b: &Option<SectionProperties>,
) {
    match (a, b) {
        (None, None) => {}
        (Some(_), None) | (None, Some(_)) => diff(diffs, path, a.is_some(), b.is_some()),
        (Some(a), Some(b)) => {
            // SectionProperties already has a custom PartialEq that ignores `raw`
            if a != b {
                diff(diffs, path, a, b);
            }
        }
    }
}

fn compare_opt_section_property_change(
    diffs: &mut Vec<Difference>,
    path: &str,
    a: &Option<SectionPropertyChange>,
    b: &Option<SectionPropertyChange>,
) {
    match (a, b) {
        (None, None) => {}
        (Some(_), None) | (None, Some(_)) => diff(diffs, path, a.is_some(), b.is_some()),
        (Some(a_val), Some(b_val)) => {
            compare_revision_info(
                diffs,
                &format!("{path}.revision"),
                &a_val.revision,
                &b_val.revision,
            );
            // previous_properties_raw is raw XML — compare as bytes
            compare_val(
                diffs,
                &format!("{path}.previous_properties_raw.len"),
                &a_val.previous_properties_raw.len(),
                &b_val.previous_properties_raw.len(),
            );
        }
    }
}

fn compare_revision_info(
    diffs: &mut Vec<Difference>,
    path: &str,
    a: &RevisionInfo,
    b: &RevisionInfo,
) {
    compare_val(
        diffs,
        &format!("{path}.revision_id"),
        &a.revision_id,
        &b.revision_id,
    );
    compare_opt(diffs, &format!("{path}.author"), &a.author, &b.author);
    compare_opt(diffs, &format!("{path}.date"), &a.date, &b.date);
}

// ---------------------------------------------------------------------------
// Stories
// ---------------------------------------------------------------------------

fn compare_headers(diffs: &mut Vec<Difference>, path: &str, a: &[HeaderStory], b: &[HeaderStory]) {
    if a.len() != b.len() {
        diff(diffs, &format!("{path}.len"), a.len(), b.len());
        return;
    }
    for (i, (ha, hb)) in a.iter().zip(b.iter()).enumerate() {
        let p = format!("{path}[{i}]");
        // Exhaustive destructure of HeaderStory.
        let HeaderStory {
            part_name,
            kind,
            blocks,
            content_hash: _, // computed digest for cross-document alignment
            synthesized: _,
        } = ha;
        let HeaderStory {
            part_name: b_part_name,
            kind: b_kind,
            blocks: b_blocks,
            content_hash: _,
            synthesized: _,
        } = hb;
        compare_val(diffs, &format!("{p}.part_name"), part_name, b_part_name);
        compare_val(diffs, &format!("{p}.kind"), kind, b_kind);
        compare_tracked_blocks(diffs, &format!("{p}.blocks"), blocks, b_blocks);
    }
}

fn compare_footers(diffs: &mut Vec<Difference>, path: &str, a: &[FooterStory], b: &[FooterStory]) {
    if a.len() != b.len() {
        diff(diffs, &format!("{path}.len"), a.len(), b.len());
        return;
    }
    for (i, (fa, fb)) in a.iter().zip(b.iter()).enumerate() {
        let p = format!("{path}[{i}]");
        // Exhaustive destructure of FooterStory.
        let FooterStory {
            part_name,
            kind,
            blocks,
            content_hash: _, // computed digest for cross-document alignment
            synthesized: _,
        } = fa;
        let FooterStory {
            part_name: b_part_name,
            kind: b_kind,
            blocks: b_blocks,
            content_hash: _,
            synthesized: _,
        } = fb;
        compare_val(diffs, &format!("{p}.part_name"), part_name, b_part_name);
        compare_val(diffs, &format!("{p}.kind"), kind, b_kind);
        compare_tracked_blocks(diffs, &format!("{p}.blocks"), blocks, b_blocks);
    }
}

fn compare_footnotes(
    diffs: &mut Vec<Difference>,
    path: &str,
    a: &[FootnoteStory],
    b: &[FootnoteStory],
) {
    if a.len() != b.len() {
        diff(diffs, &format!("{path}.len"), a.len(), b.len());
        return;
    }
    for (i, (na, nb)) in a.iter().zip(b.iter()).enumerate() {
        let p = format!("{path}[{i}]");
        // Exhaustive destructure of FootnoteStory.
        let FootnoteStory {
            id,
            note_type,
            blocks,
            content_hash: _, // computed digest for cross-document alignment
        } = na;
        let FootnoteStory {
            id: b_id,
            note_type: b_note_type,
            blocks: b_blocks,
            content_hash: _,
        } = nb;
        compare_val(diffs, &format!("{p}.id"), id, b_id);
        compare_val(diffs, &format!("{p}.note_type"), note_type, b_note_type);
        compare_tracked_blocks(diffs, &format!("{p}.blocks"), blocks, b_blocks);
    }
}

fn compare_endnotes(
    diffs: &mut Vec<Difference>,
    path: &str,
    a: &[EndnoteStory],
    b: &[EndnoteStory],
) {
    if a.len() != b.len() {
        diff(diffs, &format!("{path}.len"), a.len(), b.len());
        return;
    }
    for (i, (na, nb)) in a.iter().zip(b.iter()).enumerate() {
        let p = format!("{path}[{i}]");
        // Exhaustive destructure of EndnoteStory.
        let EndnoteStory {
            id,
            note_type,
            blocks,
            content_hash: _, // computed digest for cross-document alignment
        } = na;
        let EndnoteStory {
            id: b_id,
            note_type: b_note_type,
            blocks: b_blocks,
            content_hash: _,
        } = nb;
        compare_val(diffs, &format!("{p}.id"), id, b_id);
        compare_val(diffs, &format!("{p}.note_type"), note_type, b_note_type);
        compare_tracked_blocks(diffs, &format!("{p}.blocks"), blocks, b_blocks);
    }
}

fn compare_comments(
    diffs: &mut Vec<Difference>,
    path: &str,
    a: &[CommentStory],
    b: &[CommentStory],
) {
    if a.len() != b.len() {
        diff(diffs, &format!("{path}.len"), a.len(), b.len());
        return;
    }
    for (i, (ca, cb)) in a.iter().zip(b.iter()).enumerate() {
        let p = format!("{path}[{i}]");
        // Exhaustive destructure of CommentStory.
        let CommentStory {
            id,
            author,
            date,
            blocks,
            content_hash: _, // computed digest for cross-document alignment
            tracking_status,
        } = ca;
        let CommentStory {
            id: b_id,
            author: b_author,
            date: b_date,
            blocks: b_blocks,
            content_hash: _,
            tracking_status: b_tracking_status,
        } = cb;
        compare_val(diffs, &format!("{p}.id"), id, b_id);
        compare_opt(diffs, &format!("{p}.author"), author, b_author);
        compare_opt(diffs, &format!("{p}.date"), date, b_date);
        compare_tracked_blocks(diffs, &format!("{p}.blocks"), blocks, b_blocks);
        compare_opt(
            diffs,
            &format!("{p}.tracking_status"),
            tracking_status,
            b_tracking_status,
        );
    }
}

// ---------------------------------------------------------------------------
// Tracked blocks
// ---------------------------------------------------------------------------

fn compare_tracked_blocks(
    diffs: &mut Vec<Difference>,
    path: &str,
    a: &[TrackedBlock],
    b: &[TrackedBlock],
) {
    if a.len() != b.len() {
        diff(diffs, &format!("{path}.len"), a.len(), b.len());
        return;
    }
    for (i, (ba, bb)) in a.iter().zip(b.iter()).enumerate() {
        let p = format!("{path}[{i}]");
        // Exhaustive destructure so a new TrackedBlock field fails to compile
        // until it is classified as fidelity or justified ephemeral.
        let TrackedBlock {
            status,
            block,
            move_id,
            block_sdt_wrap,
        } = ba;
        let TrackedBlock {
            status: b_status,
            block: b_block,
            move_id: b_move_id,
            block_sdt_wrap: b_block_sdt_wrap,
        } = bb;
        compare_tracking_status(diffs, &format!("{p}.status"), status, b_status);
        compare_opt(diffs, &format!("{p}.move_id"), move_id, b_move_id);
        compare_opt(
            diffs,
            &format!("{p}.block_sdt_wrap"),
            block_sdt_wrap,
            b_block_sdt_wrap,
        );
        compare_block_nodes(diffs, &p, block, b_block);
    }
}

fn compare_tracking_status(
    diffs: &mut Vec<Difference>,
    path: &str,
    a: &TrackingStatus,
    b: &TrackingStatus,
) {
    // Compare the variant and revision info, but don't require exact revision_id match
    // since IDs may be renumbered during serialization
    match (a, b) {
        (TrackingStatus::Normal, TrackingStatus::Normal) => {}
        (TrackingStatus::Inserted(ra), TrackingStatus::Inserted(rb)) => {
            compare_tracking_revision_info(diffs, &format!("{path}.inserted"), ra, rb);
        }
        (TrackingStatus::Deleted(ra), TrackingStatus::Deleted(rb)) => {
            compare_tracking_revision_info(diffs, &format!("{path}.deleted"), ra, rb);
        }
        (TrackingStatus::InsertedThenDeleted(ra), TrackingStatus::InsertedThenDeleted(rb)) => {
            compare_tracking_revision_info(
                diffs,
                &format!("{path}.inserted_then_deleted.inserted"),
                &ra.inserted,
                &rb.inserted,
            );
            compare_tracking_revision_info(
                diffs,
                &format!("{path}.inserted_then_deleted.deleted"),
                &ra.deleted,
                &rb.deleted,
            );
        }
        _ => diff(diffs, path, a, b),
    }
}

fn compare_tracking_revision_info(
    diffs: &mut Vec<Difference>,
    path: &str,
    a: &RevisionInfo,
    b: &RevisionInfo,
) {
    // The engine identity is the semantic key and is stable across
    // save/reopen. Raw wire ids and apply-operation ids are boundary-local and
    // deliberately ignored.
    compare_val(diffs, &format!("{path}.identity"), &a.identity, &b.identity);
    compare_opt(diffs, &format!("{path}.author"), &a.author, &b.author);
    compare_opt(diffs, &format!("{path}.date"), &a.date, &b.date);
}

// ---------------------------------------------------------------------------
// Block nodes
// ---------------------------------------------------------------------------

fn compare_block_nodes(diffs: &mut Vec<Difference>, path: &str, a: &BlockNode, b: &BlockNode) {
    match (a, b) {
        (BlockNode::Paragraph(pa), BlockNode::Paragraph(pb)) => {
            compare_paragraphs(diffs, &format!("{path}.paragraph"), pa, pb);
        }
        (BlockNode::Table(ta), BlockNode::Table(tb)) => {
            compare_tables(diffs, &format!("{path}.table"), ta, tb);
        }
        (BlockNode::OpaqueBlock(oa), BlockNode::OpaqueBlock(ob)) => {
            compare_opaque_blocks(diffs, &format!("{path}.opaque_block"), oa, ob);
        }
        _ => {
            diff(
                diffs,
                &format!("{path}.variant"),
                block_variant_name(a),
                block_variant_name(b),
            );
        }
    }
}

fn block_variant_name(b: &BlockNode) -> &'static str {
    match b {
        BlockNode::Paragraph(_) => "Paragraph",
        BlockNode::Table(_) => "Table",
        BlockNode::OpaqueBlock(_) => "OpaqueBlock",
    }
}

// ---------------------------------------------------------------------------
// Paragraphs
// ---------------------------------------------------------------------------

fn compare_paragraphs(
    diffs: &mut Vec<Difference>,
    path: &str,
    a: &ParagraphNode,
    b: &ParagraphNode,
) {
    // Exhaustive destructure: every field of ParagraphNode is named here, so
    // adding a 45th field forces a compile error until the author decides
    // whether it is roundtrip fidelity (compare it) or ephemeral (bind `_`
    // with a justification). No silent omissions.
    let ParagraphNode {
        id: _, // ephemeral NodeId, reassigned on every parse
        style_id,
        align,
        has_direct_align: _, // parse-time provenance flag, not document content
        indent,
        has_direct_indent: _, // parse-time provenance flag
        // AUTHORED-direct indent/spacing: the verbatim pPr the serializer emits.
        // Compared for round-trip fidelity (the effective `indent`/`spacing` is
        // also compared, as a resolved cross-check).
        authored_indent,
        spacing,
        has_direct_spacing: _, // parse-time provenance flag
        authored_spacing,
        borders,
        keep_next,
        keep_lines,
        page_break_before,
        widow_control,
        contextual_spacing,
        shading,
        has_direct_keep_next: _,
        has_direct_keep_lines: _,
        has_direct_page_break_before: _,
        has_direct_widow_control: _,
        has_direct_contextual_spacing: _,
        has_direct_shading: _,
        has_direct_borders: _,
        tab_stops,
        // Derived view value (style resolution + default-grid synthesis,
        // body-left-relative); never serialized, so never compared.
        effective_tab_stops_rel: _,
        segments,
        block_text_hash: _, // computed hash of segment text, derived not authored
        numbering,
        has_direct_numbering: _, // parse-time provenance flag; numPr emission cross-checked via `numbering`
        numbering_suppressed,
        // Derived cache: the pre-materialization numbering snapshot. Restored
        // into `numbering` during projection, so comparing `numbering` already
        // covers the structural fidelity; this is a transient bookkeeping field.
        materialized_numbering: _,
        rendered_text: _, // computed at parse time for diffing, not document content
        literal_prefix,
        literal_prefix_marks,
        literal_prefix_style_props,
        literal_prefix_rpr_authored: _, // ephemeral run-rPr provenance, not content
        // mixed content/provenance; emitted XML is compared by the fidelity gate
        literal_prefix_leading_rpr: _,
        literal_prefix_trailing_rpr: _,
        literal_prefix_leading_tab_twips,
        literal_prefix_leading_tab_count,
        literal_prefix_leading_ws,
        literal_prefix_trailing_ws,
        literal_prefix_has_trailing_tab,
        literal_prefix_trailing_tab_stop_twips,
        outline_lvl,
        heading_level,
        para_mark_status,
        paragraph_mark_marks,
        paragraph_mark_style_props,
        paragraph_mark_rpr_off,
        para_split,
        section_property_change,
        formatting_change,
        section_properties,
        mirror_indents,
        auto_space_de,
        auto_space_dn,
        bidi,
        text_alignment,
        text_direction,
        suppress_auto_hyphens,
        snap_to_grid,
        overflow_punct,
        adjust_right_ind,
        word_wrap,
        frame_pr,
        para_id: _, // optional w14:paraId identity hex; renumbered on serialize
        text_id: _, // optional w14:textId identity hex; renumbered on serialize
        cnf_style,
        preserved_ppr,
    } = a;
    let ParagraphNode {
        id: _,
        style_id: b_style_id,
        align: b_align,
        has_direct_align: _,
        indent: b_indent,
        has_direct_indent: _,
        authored_indent: b_authored_indent,
        spacing: b_spacing,
        has_direct_spacing: _,
        authored_spacing: b_authored_spacing,
        borders: b_borders,
        keep_next: b_keep_next,
        keep_lines: b_keep_lines,
        page_break_before: b_page_break_before,
        widow_control: b_widow_control,
        contextual_spacing: b_contextual_spacing,
        shading: b_shading,
        has_direct_keep_next: _,
        has_direct_keep_lines: _,
        has_direct_page_break_before: _,
        has_direct_widow_control: _,
        has_direct_contextual_spacing: _,
        has_direct_shading: _,
        has_direct_borders: _,
        tab_stops: b_tab_stops,
        effective_tab_stops_rel: _, // derived view value, never compared
        segments: b_segments,
        block_text_hash: _,
        numbering: b_numbering,
        has_direct_numbering: _,
        numbering_suppressed: b_numbering_suppressed,
        materialized_numbering: _,
        rendered_text: _,
        literal_prefix: b_literal_prefix,
        literal_prefix_marks: b_literal_prefix_marks,
        literal_prefix_style_props: b_literal_prefix_style_props,
        literal_prefix_rpr_authored: _, // ephemeral run-rPr provenance, not content
        // mixed content/provenance; emitted XML is compared by the fidelity gate
        literal_prefix_leading_rpr: _,
        literal_prefix_trailing_rpr: _,
        literal_prefix_leading_tab_twips: b_literal_prefix_leading_tab_twips,
        literal_prefix_leading_tab_count: b_literal_prefix_leading_tab_count,
        literal_prefix_leading_ws: b_literal_prefix_leading_ws,
        literal_prefix_trailing_ws: b_literal_prefix_trailing_ws,
        literal_prefix_has_trailing_tab: b_literal_prefix_has_trailing_tab,
        literal_prefix_trailing_tab_stop_twips: b_literal_prefix_trailing_tab_stop_twips,
        outline_lvl: b_outline_lvl,
        heading_level: b_heading_level,
        para_mark_status: b_para_mark_status,
        paragraph_mark_marks: b_paragraph_mark_marks,
        paragraph_mark_style_props: b_paragraph_mark_style_props,
        paragraph_mark_rpr_off: b_paragraph_mark_rpr_off,
        para_split: b_para_split,
        section_property_change: b_section_property_change,
        formatting_change: b_formatting_change,
        section_properties: b_section_properties,
        mirror_indents: b_mirror_indents,
        auto_space_de: b_auto_space_de,
        auto_space_dn: b_auto_space_dn,
        bidi: b_bidi,
        text_alignment: b_text_alignment,
        text_direction: b_text_direction,
        suppress_auto_hyphens: b_suppress_auto_hyphens,
        snap_to_grid: b_snap_to_grid,
        overflow_punct: b_overflow_punct,
        adjust_right_ind: b_adjust_right_ind,
        word_wrap: b_word_wrap,
        frame_pr: b_frame_pr,
        para_id: _,
        text_id: _,
        cnf_style: b_cnf_style,
        preserved_ppr: b_preserved_ppr,
    } = b;

    compare_opt(diffs, &format!("{path}.style_id"), style_id, b_style_id);
    compare_opt(diffs, &format!("{path}.align"), align, b_align);
    compare_opt(diffs, &format!("{path}.indent"), indent, b_indent);
    compare_opt(
        diffs,
        &format!("{path}.authored_indent"),
        authored_indent,
        b_authored_indent,
    );
    compare_opt(diffs, &format!("{path}.spacing"), spacing, b_spacing);
    compare_opt(
        diffs,
        &format!("{path}.authored_spacing"),
        authored_spacing,
        b_authored_spacing,
    );
    compare_opt(diffs, &format!("{path}.borders"), borders, b_borders);
    compare_opt(diffs, &format!("{path}.keep_next"), keep_next, b_keep_next);
    compare_opt(
        diffs,
        &format!("{path}.keep_lines"),
        keep_lines,
        b_keep_lines,
    );
    compare_val(
        diffs,
        &format!("{path}.page_break_before"),
        page_break_before,
        b_page_break_before,
    );
    compare_opt(
        diffs,
        &format!("{path}.widow_control"),
        widow_control,
        b_widow_control,
    );
    compare_opt(
        diffs,
        &format!("{path}.contextual_spacing"),
        contextual_spacing,
        b_contextual_spacing,
    );
    compare_opt(diffs, &format!("{path}.shading"), shading, b_shading);
    compare_val(diffs, &format!("{path}.tab_stops"), tab_stops, b_tab_stops);
    compare_tracked_segments(diffs, &format!("{path}.segments"), segments, b_segments);
    compare_numbering(diffs, &format!("{path}.numbering"), numbering, b_numbering);
    compare_val(
        diffs,
        &format!("{path}.numbering_suppressed"),
        numbering_suppressed,
        b_numbering_suppressed,
    );
    compare_opt(
        diffs,
        &format!("{path}.literal_prefix"),
        literal_prefix,
        b_literal_prefix,
    );
    compare_val(
        diffs,
        &format!("{path}.literal_prefix_marks"),
        literal_prefix_marks,
        b_literal_prefix_marks,
    );
    compare_val(
        diffs,
        &format!("{path}.literal_prefix_style_props"),
        literal_prefix_style_props,
        b_literal_prefix_style_props,
    );
    compare_opt(
        diffs,
        &format!("{path}.literal_prefix_leading_tab_twips"),
        literal_prefix_leading_tab_twips,
        b_literal_prefix_leading_tab_twips,
    );
    compare_val(
        diffs,
        &format!("{path}.literal_prefix_leading_tab_count"),
        literal_prefix_leading_tab_count,
        b_literal_prefix_leading_tab_count,
    );
    compare_val(
        diffs,
        &format!("{path}.literal_prefix_leading_ws"),
        literal_prefix_leading_ws,
        b_literal_prefix_leading_ws,
    );
    compare_val(
        diffs,
        &format!("{path}.literal_prefix_trailing_ws"),
        literal_prefix_trailing_ws,
        b_literal_prefix_trailing_ws,
    );
    compare_val(
        diffs,
        &format!("{path}.literal_prefix_has_trailing_tab"),
        literal_prefix_has_trailing_tab,
        b_literal_prefix_has_trailing_tab,
    );
    compare_opt(
        diffs,
        &format!("{path}.literal_prefix_trailing_tab_stop_twips"),
        literal_prefix_trailing_tab_stop_twips,
        b_literal_prefix_trailing_tab_stop_twips,
    );
    compare_opt(
        diffs,
        &format!("{path}.outline_lvl"),
        outline_lvl,
        b_outline_lvl,
    );
    compare_opt(
        diffs,
        &format!("{path}.heading_level"),
        heading_level,
        b_heading_level,
    );
    match (para_mark_status, b_para_mark_status) {
        (None, None) => {}
        (Some(a), Some(b)) => {
            compare_tracking_status(diffs, &format!("{path}.para_mark_status"), a, b)
        }
        _ => diff(
            diffs,
            &format!("{path}.para_mark_status"),
            para_mark_status,
            b_para_mark_status,
        ),
    }
    compare_val(
        diffs,
        &format!("{path}.paragraph_mark_marks"),
        paragraph_mark_marks,
        b_paragraph_mark_marks,
    );
    compare_val(
        diffs,
        &format!("{path}.paragraph_mark_style_props"),
        paragraph_mark_style_props,
        b_paragraph_mark_style_props,
    );
    compare_val(
        diffs,
        &format!("{path}.paragraph_mark_rpr_off"),
        paragraph_mark_rpr_off,
        b_paragraph_mark_rpr_off,
    );
    compare_val(
        diffs,
        &format!("{path}.para_split"),
        para_split,
        b_para_split,
    );
    compare_opt_section_property_change(
        diffs,
        &format!("{path}.section_property_change"),
        section_property_change,
        b_section_property_change,
    );
    compare_opt_formatting_change(
        diffs,
        &format!("{path}.formatting_change"),
        formatting_change,
        b_formatting_change,
    );
    compare_opt_section_properties(
        diffs,
        &format!("{path}.section_properties"),
        section_properties,
        b_section_properties,
    );
    compare_val(
        diffs,
        &format!("{path}.mirror_indents"),
        mirror_indents,
        b_mirror_indents,
    );
    compare_opt(
        diffs,
        &format!("{path}.auto_space_de"),
        auto_space_de,
        b_auto_space_de,
    );
    compare_opt(
        diffs,
        &format!("{path}.auto_space_dn"),
        auto_space_dn,
        b_auto_space_dn,
    );
    compare_val(diffs, &format!("{path}.bidi"), bidi, b_bidi);
    compare_opt(
        diffs,
        &format!("{path}.text_alignment"),
        text_alignment,
        b_text_alignment,
    );
    compare_opt(
        diffs,
        &format!("{path}.text_direction"),
        text_direction,
        b_text_direction,
    );
    compare_opt(
        diffs,
        &format!("{path}.suppress_auto_hyphens"),
        suppress_auto_hyphens,
        b_suppress_auto_hyphens,
    );
    compare_opt(
        diffs,
        &format!("{path}.snap_to_grid"),
        snap_to_grid,
        b_snap_to_grid,
    );
    compare_opt(
        diffs,
        &format!("{path}.overflow_punct"),
        overflow_punct,
        b_overflow_punct,
    );
    compare_opt(
        diffs,
        &format!("{path}.adjust_right_ind"),
        adjust_right_ind,
        b_adjust_right_ind,
    );
    compare_opt(diffs, &format!("{path}.word_wrap"), word_wrap, b_word_wrap);
    compare_opt(diffs, &format!("{path}.frame_pr"), frame_pr, b_frame_pr);
    compare_opt(diffs, &format!("{path}.cnf_style"), cnf_style, b_cnf_style);
    compare_val(
        diffs,
        &format!("{path}.preserved_ppr"),
        preserved_ppr,
        b_preserved_ppr,
    );
}

fn compare_numbering(
    diffs: &mut Vec<Difference>,
    path: &str,
    a: &Option<NumberingInfo>,
    b: &Option<NumberingInfo>,
) {
    match (a, b) {
        (None, None) => {}
        (Some(_), None) | (None, Some(_)) => diff(diffs, path, a.is_some(), b.is_some()),
        (Some(na), Some(nb)) => {
            // Exhaustive destructure of NumberingInfo.
            let NumberingInfo {
                num_id,
                ilvl,
                // Derived counter value (e.g. "1.", "(a)") synthesized at parse
                // time from the numbering definitions; it drifts as list items
                // are added/removed and is not authored fidelity.
                synthesized_text: _,
                // Derived from the referenced numbering format (numFmt="bullet"),
                // not stored on the paragraph; recomputed from num_id/ilvl.
                is_bullet: _,
                // Transient serializer instruction: a pending restart that is
                // materialized into a fresh w:num at write time and then cleared.
                restart_numbering: _,
            } = na;
            let NumberingInfo {
                num_id: b_num_id,
                ilvl: b_ilvl,
                synthesized_text: _,
                is_bullet: _,
                restart_numbering: _,
            } = nb;
            compare_val(diffs, &format!("{path}.num_id"), num_id, b_num_id);
            compare_val(diffs, &format!("{path}.ilvl"), ilvl, b_ilvl);
        }
    }
}

fn compare_opt_formatting_change(
    diffs: &mut Vec<Difference>,
    path: &str,
    a: &Option<ParagraphFormattingChange>,
    b: &Option<ParagraphFormattingChange>,
) {
    match (a, b) {
        (None, None) => {}
        (Some(_), None) | (None, Some(_)) => diff(diffs, path, a.is_some(), b.is_some()),
        (Some(fa), Some(fb)) => {
            compare_opt(
                diffs,
                &format!("{path}.previous_alignment"),
                &fa.previous_alignment,
                &fb.previous_alignment,
            );
            compare_opt(
                diffs,
                &format!("{path}.previous_indentation"),
                &fa.previous_indentation,
                &fb.previous_indentation,
            );
            compare_opt(
                diffs,
                &format!("{path}.previous_spacing"),
                &fa.previous_spacing,
                &fb.previous_spacing,
            );
            compare_numbering(
                diffs,
                &format!("{path}.previous_numbering"),
                &fa.previous_numbering,
                &fb.previous_numbering,
            );
            compare_val(
                diffs,
                &format!("{path}.previous_numbering_explicitly_absent"),
                &fa.previous_numbering_explicitly_absent,
                &fb.previous_numbering_explicitly_absent,
            );
            compare_val(
                diffs,
                &format!("{path}.previous_paragraph_mark_marks"),
                &fa.previous_paragraph_mark_marks,
                &fb.previous_paragraph_mark_marks,
            );
            compare_val(
                diffs,
                &format!("{path}.previous_paragraph_mark_style_props"),
                &fa.previous_paragraph_mark_style_props,
                &fb.previous_paragraph_mark_style_props,
            );
            compare_val(
                diffs,
                &format!("{path}.previous_paragraph_mark_rpr_off"),
                &fa.previous_paragraph_mark_rpr_off,
                &fb.previous_paragraph_mark_rpr_off,
            );
            compare_val(diffs, &format!("{path}.author"), &fa.author, &fb.author);
            compare_opt(diffs, &format!("{path}.date"), &fa.date, &fb.date);
        }
    }
}

// ---------------------------------------------------------------------------
// Tracked segments (paragraph content)
// ---------------------------------------------------------------------------

fn compare_tracked_segments(
    diffs: &mut Vec<Difference>,
    path: &str,
    a: &[TrackedSegment],
    b: &[TrackedSegment],
) {
    // Coalesce consecutive EQUAL-status segments before comparing: segments
    // exist to partition inlines by tracking status, so a boundary between
    // same-status neighbors carries no document meaning — the importer
    // legitimately opens a fresh segment for a structurally-anchored marker
    // (a between-blocks bookmarkEnd, see `structural_range_decoration`) that
    // a rebuild folds back into its neighbor. Same status, same inline
    // sequence ⇒ same content. Statuses compare by full value, so segments
    // of DIFFERENT revisions (or different kinds) never merge.
    fn coalesce(segs: &[TrackedSegment]) -> Vec<(&TrackingStatus, Vec<&InlineNode>)> {
        let mut out: Vec<(&TrackingStatus, Vec<&InlineNode>)> = Vec::new();
        for seg in segs {
            // Exhaustive destructure: a new TrackedSegment field must be handled.
            let TrackedSegment { status, inlines } = seg;
            if let Some((last_status, last_inlines)) = out.last_mut()
                && *last_status == status
            {
                last_inlines.extend(inlines.iter());
                continue;
            }
            out.push((status, inlines.iter().collect()));
        }
        out
    }
    let ca = coalesce(a);
    let cb = coalesce(b);

    if ca.len() != cb.len() {
        diff(diffs, &format!("{path}.len"), ca.len(), cb.len());
        return;
    }
    for (i, ((status, inlines), (b_status, b_inlines))) in ca.iter().zip(cb.iter()).enumerate() {
        let p = format!("{path}[{i}]");
        compare_tracking_status(diffs, &format!("{p}.status"), status, b_status);
        if inlines.len() != b_inlines.len() {
            diff(
                diffs,
                &format!("{p}.inlines.len"),
                inlines.len(),
                b_inlines.len(),
            );
            continue;
        }
        for (j, (ia, ib)) in inlines.iter().zip(b_inlines.iter()).enumerate() {
            compare_inline_node(diffs, &format!("{p}.inlines[{j}]"), ia, ib);
        }
    }
}

// ---------------------------------------------------------------------------
// Inline nodes
// ---------------------------------------------------------------------------

fn compare_inline_node(diffs: &mut Vec<Difference>, path: &str, a: &InlineNode, b: &InlineNode) {
    match (a, b) {
        (InlineNode::Text(ta), InlineNode::Text(tb)) => {
            // Exhaustive destructure of TextNode: a new run-level field must be
            // classified rather than silently dropped.
            let TextNode {
                id: _, // ephemeral NodeId
                text_role,
                text,
                marks,
                style_props,
                rpr_authored: _, // parse-time provenance flag, not content
                source_run_attrs,
                formatting_change,
            } = ta.as_ref();
            let TextNode {
                id: _,
                text_role: b_text_role,
                text: b_text,
                marks: b_marks,
                style_props: b_style_props,
                rpr_authored: _,
                source_run_attrs: b_source_run_attrs,
                formatting_change: b_formatting_change,
            } = tb.as_ref();
            compare_opt(diffs, &format!("{path}.text_role"), text_role, b_text_role);
            compare_val(diffs, &format!("{path}.text"), text, b_text);
            compare_val(diffs, &format!("{path}.marks"), marks, b_marks);
            compare_val(
                diffs,
                &format!("{path}.style_props"),
                style_props,
                b_style_props,
            );
            compare_val(
                diffs,
                &format!("{path}.source_run_attrs"),
                source_run_attrs,
                b_source_run_attrs,
            );
            compare_opt_text_formatting_change(
                diffs,
                &format!("{path}.formatting_change"),
                formatting_change,
                b_formatting_change,
            );
        }
        (InlineNode::HardBreak(ha), InlineNode::HardBreak(hb)) => {
            // Exhaustive destructure of HardBreakNode.
            let HardBreakNode {
                id: _, // ephemeral NodeId
                break_type,
                joins_following_text_run,
            } = ha;
            let HardBreakNode {
                id: _,
                break_type: b_break_type,
                joins_following_text_run: b_joins_following_text_run,
            } = hb;
            compare_val(
                diffs,
                &format!("{path}.break_type"),
                break_type,
                b_break_type,
            );
            compare_val(
                diffs,
                &format!("{path}.joins_following_text_run"),
                joins_following_text_run,
                b_joins_following_text_run,
            );
        }
        (InlineNode::OpaqueInline(oa), InlineNode::OpaqueInline(ob)) => {
            compare_opaque_inlines(diffs, path, oa, ob);
        }
        (InlineNode::Decoration(da), InlineNode::Decoration(db)) => {
            compare_decorations(diffs, path, da, db);
        }
        (
            InlineNode::CommentRangeStart { id: a_id },
            InlineNode::CommentRangeStart { id: b_id },
        ) => {
            compare_val(diffs, &format!("{path}.comment_range_start.id"), a_id, b_id);
        }
        (InlineNode::CommentRangeEnd { id: a_id }, InlineNode::CommentRangeEnd { id: b_id }) => {
            compare_val(diffs, &format!("{path}.comment_range_end.id"), a_id, b_id);
        }
        (InlineNode::CommentReference { id: a_id }, InlineNode::CommentReference { id: b_id }) => {
            compare_val(diffs, &format!("{path}.comment_reference.id"), a_id, b_id);
        }
        _ => {
            diff(
                diffs,
                &format!("{path}.variant"),
                inline_variant_name(a),
                inline_variant_name(b),
            );
        }
    }
}

fn inline_variant_name(n: &InlineNode) -> &'static str {
    match n {
        InlineNode::Text(_) => "Text",
        InlineNode::HardBreak(_) => "HardBreak",
        InlineNode::OpaqueInline(_) => "OpaqueInline",
        InlineNode::Decoration(_) => "Decoration",
        InlineNode::CommentRangeStart { .. } => "CommentRangeStart",
        InlineNode::CommentRangeEnd { .. } => "CommentRangeEnd",
        InlineNode::CommentReference { .. } => "CommentReference",
    }
}

fn compare_opt_text_formatting_change(
    diffs: &mut Vec<Difference>,
    path: &str,
    a: &Option<FormattingChange>,
    b: &Option<FormattingChange>,
) {
    match (a, b) {
        (None, None) => {}
        (Some(_), None) | (None, Some(_)) => diff(diffs, path, a.is_some(), b.is_some()),
        (Some(fa), Some(fb)) => {
            compare_val(
                diffs,
                &format!("{path}.previous_marks"),
                &fa.previous_marks,
                &fb.previous_marks,
            );
            compare_val(
                diffs,
                &format!("{path}.previous_style_props"),
                &fa.previous_style_props,
                &fb.previous_style_props,
            );
            compare_val(diffs, &format!("{path}.author"), &fa.author, &fb.author);
            compare_opt(diffs, &format!("{path}.date"), &fa.date, &fb.date);
        }
    }
}

fn compare_opaque_inlines(
    diffs: &mut Vec<Difference>,
    path: &str,
    a: &OpaqueInlineNode,
    b: &OpaqueInlineNode,
) {
    // Exhaustive destructure of OpaqueInlineNode.
    let OpaqueInlineNode {
        id: _, // ephemeral NodeId
        kind,
        opaque_ref: _, // internal store reference, reassigned on parse
        proof_ref: _,  // ephemeral proof bookkeeping
        wrapper_marks,
        wrapper_style_props,
        // raw_xml carries the verbatim bytes; its identity is summarized by
        // content_hash, which we DO compare. Comparing raw bytes directly would
        // be redundant and noisier (equal hash ⇒ equal bytes).
        raw_xml: _,
        content_hash,
    } = a;
    let OpaqueInlineNode {
        id: _,
        kind: b_kind,
        opaque_ref: _,
        proof_ref: _,
        wrapper_marks: b_wrapper_marks,
        wrapper_style_props: b_wrapper_style_props,
        raw_xml: _,
        content_hash: b_content_hash,
    } = b;
    compare_val(diffs, &format!("{path}.kind"), kind, b_kind);
    compare_val(
        diffs,
        &format!("{path}.wrapper_marks"),
        wrapper_marks,
        b_wrapper_marks,
    );
    compare_val(
        diffs,
        &format!("{path}.wrapper_style_props"),
        wrapper_style_props,
        b_wrapper_style_props,
    );
    compare_opt(
        diffs,
        &format!("{path}.content_hash"),
        content_hash,
        b_content_hash,
    );
}

/// The load-bearing identity inside a decoration's verbatim bytes: the
/// `w:name` attribute value (bookmarks §17.13.6.2, move ranges
/// §17.13.5.24/.26). None for name-less decorations (proofErr,
/// lastRenderedPageBreak, note refs, …) — for those, `kind` plus the
/// wrapper fields are the whole comparable identity.
fn decoration_name(raw_xml: &Option<Vec<u8>>) -> Option<String> {
    let raw = String::from_utf8_lossy(raw_xml.as_deref()?);
    let start = raw.find("w:name=\"")? + "w:name=\"".len();
    let rest = &raw[start..];
    Some(rest[..rest.find('"')?].to_string())
}

fn compare_decorations(
    diffs: &mut Vec<Difference>,
    path: &str,
    a: &DecorationNode,
    b: &DecorationNode,
) {
    // Exhaustive destructure of DecorationNode.
    let DecorationNode {
        id: _, // ephemeral NodeId
        kind,
        // opaque_ref is an internal store reference minted from the global
        // inline counter (`paragraph:<id>:deco:<n>`); it renumbers whenever
        // ANY earlier inline content shifts, so comparing it indicts
        // untouched blocks after a legitimate edit elsewhere (same class as
        // OpaqueInlineNode's skipped opaque_ref). The decoration's real
        // identity is the `w:name` inside its verbatim bytes — compared
        // below via `decoration_name`.
        opaque_ref: _,
        proof_ref: _, // ephemeral proof bookkeeping
        // wrapper rPr of the host run for run-level decorations (footnoteRef/
        // endnoteRef/separator/…) — real fidelity: the note-reference style,
        // fonts and size the auto-number renders in.
        wrapper_marks,
        wrapper_style_props,
        joins_following_text_run,
        // raw_xml is the verbatim element bytes. The load-bearing identity
        // inside them is the `w:name` attribute (bookmarks §17.13.6.2, move
        // ranges §17.13.5.24/.26) — compared below. The numeric `w:id` is a
        // disposable pairing key the serializer remints, and the remaining
        // bytes can legitimately re-serialize with cosmetic differences.
        raw_xml,
        // origin is a serializer-side bookmark-id policy hint, not document
        // content; it steers id assignment at write time and is recomputed.
        origin: _,
    } = a;
    let DecorationNode {
        id: _,
        kind: b_kind,
        opaque_ref: _,
        proof_ref: _,
        wrapper_marks: b_wrapper_marks,
        wrapper_style_props: b_wrapper_style_props,
        joins_following_text_run: b_joins_following_text_run,
        raw_xml: b_raw_xml,
        origin: _,
    } = b;
    compare_val(diffs, &format!("{path}.decoration.kind"), kind, b_kind);
    compare_opt(
        diffs,
        &format!("{path}.decoration.name"),
        &decoration_name(raw_xml),
        &decoration_name(b_raw_xml),
    );
    compare_val(
        diffs,
        &format!("{path}.decoration.wrapper_marks"),
        wrapper_marks,
        b_wrapper_marks,
    );
    compare_val(
        diffs,
        &format!("{path}.decoration.wrapper_style_props"),
        wrapper_style_props,
        b_wrapper_style_props,
    );
    compare_val(
        diffs,
        &format!("{path}.decoration.joins_following_text_run"),
        joins_following_text_run,
        b_joins_following_text_run,
    );
}

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

fn compare_tables(diffs: &mut Vec<Difference>, path: &str, a: &TableNode, b: &TableNode) {
    // Exhaustive destructure of TableNode.
    let TableNode {
        id: _, // ephemeral NodeId
        rows,
        structure_hash: _, // computed digest of row/column/merge layout
        formatting,
        formatting_change,
    } = a;
    let TableNode {
        id: _,
        rows: b_rows,
        structure_hash: _,
        formatting: b_formatting,
        formatting_change: b_formatting_change,
    } = b;
    compare_val(
        diffs,
        &format!("{path}.formatting"),
        formatting,
        b_formatting,
    );
    compare_opt_table_formatting_change(
        diffs,
        &format!("{path}.formatting_change"),
        formatting_change,
        b_formatting_change,
    );
    compare_table_rows(diffs, &format!("{path}.rows"), rows, b_rows);
}

fn compare_opt_table_formatting_change(
    diffs: &mut Vec<Difference>,
    path: &str,
    a: &Option<TableFormattingChange>,
    b: &Option<TableFormattingChange>,
) {
    match (a, b) {
        (None, None) => {}
        (Some(_), None) | (None, Some(_)) => diff(diffs, path, a.is_some(), b.is_some()),
        (Some(fa), Some(fb)) => {
            compare_opt(
                diffs,
                &format!("{path}.previous_width"),
                &fa.previous_width,
                &fb.previous_width,
            );
            compare_opt(
                diffs,
                &format!("{path}.previous_borders"),
                &fa.previous_borders,
                &fb.previous_borders,
            );
            compare_opt(
                diffs,
                &format!("{path}.previous_default_cell_margins"),
                &fa.previous_default_cell_margins,
                &fb.previous_default_cell_margins,
            );
            compare_val(diffs, &format!("{path}.author"), &fa.author, &fb.author);
            compare_opt(diffs, &format!("{path}.date"), &fa.date, &fb.date);
        }
    }
}

fn compare_table_rows(
    diffs: &mut Vec<Difference>,
    path: &str,
    a: &[TableRowNode],
    b: &[TableRowNode],
) {
    if a.len() != b.len() {
        diff(diffs, &format!("{path}.len"), a.len(), b.len());
        return;
    }
    for (i, (ra, rb)) in a.iter().zip(b.iter()).enumerate() {
        let p = format!("{path}[{i}]");
        // Exhaustive destructure of TableRowNode.
        let TableRowNode {
            id: _, // ephemeral NodeId
            cells,
            grid_before,
            grid_after,
            tracking_status,
            is_header,
            height,
            height_rule,
            formatting_change,
            para_id: _, // optional w14:paraId identity hex; renumbered on serialize
            text_id: _, // optional w14:textId identity hex; renumbered on serialize
            cant_split,
            jc,
            w_before,
            w_after,
            cnf_style,
            tbl_pr_ex,
            cell_spacing,
            preserved,
        } = ra;
        let TableRowNode {
            id: _,
            cells: b_cells,
            grid_before: b_grid_before,
            grid_after: b_grid_after,
            tracking_status: b_tracking_status,
            is_header: b_is_header,
            height: b_height,
            height_rule: b_height_rule,
            formatting_change: b_formatting_change,
            para_id: _,
            text_id: _,
            cant_split: b_cant_split,
            jc: b_jc,
            w_before: b_w_before,
            w_after: b_w_after,
            cnf_style: b_cnf_style,
            tbl_pr_ex: b_tbl_pr_ex,
            cell_spacing: b_cell_spacing,
            preserved: b_preserved,
        } = rb;
        compare_val(
            diffs,
            &format!("{p}.grid_before"),
            grid_before,
            b_grid_before,
        );
        compare_val(diffs, &format!("{p}.grid_after"), grid_after, b_grid_after);
        compare_opt(
            diffs,
            &format!("{p}.tracking_status"),
            tracking_status,
            b_tracking_status,
        );
        compare_val(diffs, &format!("{p}.is_header"), is_header, b_is_header);
        compare_opt(diffs, &format!("{p}.height"), height, b_height);
        compare_opt(
            diffs,
            &format!("{p}.height_rule"),
            height_rule,
            b_height_rule,
        );
        compare_opt_row_formatting_change(
            diffs,
            &format!("{p}.formatting_change"),
            formatting_change,
            b_formatting_change,
        );
        compare_val(diffs, &format!("{p}.cant_split"), cant_split, b_cant_split);
        compare_opt(diffs, &format!("{p}.jc"), jc, b_jc);
        compare_opt(diffs, &format!("{p}.w_before"), w_before, b_w_before);
        compare_opt(diffs, &format!("{p}.w_after"), w_after, b_w_after);
        compare_opt(diffs, &format!("{p}.cnf_style"), cnf_style, b_cnf_style);
        compare_opt(diffs, &format!("{p}.tbl_pr_ex"), tbl_pr_ex, b_tbl_pr_ex);
        compare_opt(
            diffs,
            &format!("{p}.cell_spacing"),
            cell_spacing,
            b_cell_spacing,
        );
        compare_val(diffs, &format!("{p}.preserved"), preserved, b_preserved);
        compare_table_cells(diffs, &format!("{p}.cells"), cells, b_cells);
    }
}

fn compare_opt_row_formatting_change(
    diffs: &mut Vec<Difference>,
    path: &str,
    a: &Option<RowFormattingChange>,
    b: &Option<RowFormattingChange>,
) {
    match (a, b) {
        (None, None) => {}
        (Some(_), None) | (None, Some(_)) => diff(diffs, path, a.is_some(), b.is_some()),
        (Some(fa), Some(fb)) => {
            compare_opt(
                diffs,
                &format!("{path}.previous_height"),
                &fa.previous_height,
                &fb.previous_height,
            );
            compare_opt(
                diffs,
                &format!("{path}.previous_height_rule"),
                &fa.previous_height_rule,
                &fb.previous_height_rule,
            );
            compare_val(diffs, &format!("{path}.author"), &fa.author, &fb.author);
            compare_opt(diffs, &format!("{path}.date"), &fa.date, &fb.date);
        }
    }
}

fn compare_table_cells(
    diffs: &mut Vec<Difference>,
    path: &str,
    a: &[TableCellNode],
    b: &[TableCellNode],
) {
    if a.len() != b.len() {
        diff(diffs, &format!("{path}.len"), a.len(), b.len());
        return;
    }
    for (i, (ca, cb)) in a.iter().zip(b.iter()).enumerate() {
        let p = format!("{path}[{i}]");
        // Exhaustive destructure of TableCellNode.
        let TableCellNode {
            id: _, // ephemeral NodeId
            blocks,
            grid_span,
            v_merge,
            formatting,
            formatting_change,
            tracking_status,
            row_sdt_wrapper,
            content_sdt_wraps,
            cnf_style,
            hide_mark,
            preserved,
        } = ca;
        let TableCellNode {
            id: _,
            blocks: b_blocks,
            grid_span: b_grid_span,
            v_merge: b_v_merge,
            formatting: b_formatting,
            formatting_change: b_formatting_change,
            tracking_status: b_tracking_status,
            row_sdt_wrapper: b_row_sdt_wrapper,
            content_sdt_wraps: b_content_sdt_wraps,
            cnf_style: b_cnf_style,
            hide_mark: b_hide_mark,
            preserved: b_preserved,
        } = cb;
        compare_val(diffs, &format!("{p}.grid_span"), grid_span, b_grid_span);
        compare_val(diffs, &format!("{p}.v_merge"), v_merge, b_v_merge);
        compare_val(diffs, &format!("{p}.formatting"), formatting, b_formatting);
        compare_opt_cell_formatting_change(
            diffs,
            &format!("{p}.formatting_change"),
            formatting_change,
            b_formatting_change,
        );
        compare_opt(
            diffs,
            &format!("{p}.tracking_status"),
            tracking_status,
            b_tracking_status,
        );
        // row_sdt_wrapper: compare presence only. The wrapper bytes are raw
        // `w:sdtPr` XML preserved verbatim for roundtrip; presence is the
        // structural fidelity the comparator asserts.
        compare_opt_sdt_wrapper(
            diffs,
            &format!("{p}.row_sdt_wrapper"),
            row_sdt_wrapper,
            b_row_sdt_wrapper,
        );
        // content_sdt_wraps: compare the range shape (count + each start/span).
        // The span is load-bearing structure — a wrap that grew its span is
        // exactly the swallowed-sibling regression — so it is compared, unlike
        // the opaque wrapper bytes.
        compare_val(
            diffs,
            &format!("{p}.content_sdt_wraps.len"),
            &content_sdt_wraps.len(),
            &b_content_sdt_wraps.len(),
        );
        for (i, (a_wrap, b_wrap)) in content_sdt_wraps
            .iter()
            .zip(b_content_sdt_wraps.iter())
            .enumerate()
        {
            compare_val(
                diffs,
                &format!("{p}.content_sdt_wraps[{i}].start"),
                &a_wrap.start,
                &b_wrap.start,
            );
            compare_val(
                diffs,
                &format!("{p}.content_sdt_wraps[{i}].span"),
                &a_wrap.span,
                &b_wrap.span,
            );
        }
        compare_opt(diffs, &format!("{p}.cnf_style"), cnf_style, b_cnf_style);
        compare_val(diffs, &format!("{p}.hide_mark"), hide_mark, b_hide_mark);
        compare_val(diffs, &format!("{p}.preserved"), preserved, b_preserved);
        compare_block_node_list(diffs, &format!("{p}.blocks"), blocks, b_blocks);
    }
}

fn compare_opt_cell_formatting_change(
    diffs: &mut Vec<Difference>,
    path: &str,
    a: &Option<CellFormattingChange>,
    b: &Option<CellFormattingChange>,
) {
    match (a, b) {
        (None, None) => {}
        (Some(_), None) | (None, Some(_)) => diff(diffs, path, a.is_some(), b.is_some()),
        (Some(fa), Some(fb)) => {
            compare_opt(
                diffs,
                &format!("{path}.previous_width"),
                &fa.previous_width,
                &fb.previous_width,
            );
            compare_opt(
                diffs,
                &format!("{path}.previous_borders"),
                &fa.previous_borders,
                &fb.previous_borders,
            );
            compare_opt(
                diffs,
                &format!("{path}.previous_shading"),
                &fa.previous_shading,
                &fb.previous_shading,
            );
            compare_opt(
                diffs,
                &format!("{path}.previous_v_align"),
                &fa.previous_v_align,
                &fb.previous_v_align,
            );
            compare_opt(
                diffs,
                &format!("{path}.previous_margins"),
                &fa.previous_margins,
                &fb.previous_margins,
            );
            compare_opt(
                diffs,
                &format!("{path}.previous_no_wrap"),
                &fa.previous_no_wrap,
                &fb.previous_no_wrap,
            );
            compare_opt(
                diffs,
                &format!("{path}.previous_text_direction"),
                &fa.previous_text_direction,
                &fb.previous_text_direction,
            );
            compare_opt(
                diffs,
                &format!("{path}.previous_tc_fit_text"),
                &fa.previous_tc_fit_text,
                &fb.previous_tc_fit_text,
            );
            compare_val(diffs, &format!("{path}.author"), &fa.author, &fb.author);
            compare_opt(diffs, &format!("{path}.date"), &fa.date, &fb.date);
        }
    }
}

fn compare_opt_sdt_wrapper(
    diffs: &mut Vec<Difference>,
    path: &str,
    a: &Option<SdtWrapper>,
    b: &Option<SdtWrapper>,
) {
    // Only compare presence, not raw XML content
    match (a, b) {
        (None, None) | (Some(_), Some(_)) => {}
        _ => diff(diffs, path, a.is_some(), b.is_some()),
    }
}

fn compare_block_node_list(
    diffs: &mut Vec<Difference>,
    path: &str,
    a: &[BlockNode],
    b: &[BlockNode],
) {
    if a.len() != b.len() {
        diff(diffs, &format!("{path}.len"), a.len(), b.len());
        return;
    }
    for (i, (ba, bb)) in a.iter().zip(b.iter()).enumerate() {
        compare_block_nodes(diffs, &format!("{path}[{i}]"), ba, bb);
    }
}

// ---------------------------------------------------------------------------
// Opaque blocks
// ---------------------------------------------------------------------------

fn compare_opaque_blocks(
    diffs: &mut Vec<Difference>,
    path: &str,
    a: &OpaqueBlockNode,
    b: &OpaqueBlockNode,
) {
    // Exhaustive destructure of OpaqueBlockNode.
    let OpaqueBlockNode {
        id: _, // ephemeral NodeId
        kind,
        opaque_ref: _,   // internal store reference, reassigned on parse
        proof_ref: _,    // ephemeral proof bookkeeping
        range_marker: _, // derived from the same bytes; not an independent axis
    } = a;
    let OpaqueBlockNode {
        id: _,
        kind: b_kind,
        opaque_ref: _,
        proof_ref: _,
        range_marker: _,
    } = b;
    compare_val(diffs, &format!("{path}.kind"), kind, b_kind);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn revision_info(wire_id: u32, identity: u32, author: &str, date: &str) -> RevisionInfo {
        RevisionInfo {
            revision_id: wire_id,
            author: Some(author.to_string()),
            date: Some(date.to_string()),
            apply_op_id: Some(format!("apply-{wire_id}")),
            identity,
        }
    }

    #[test]
    fn stacked_revision_comparison_keys_identity_not_wire_id() {
        let left = TrackingStatus::InsertedThenDeleted(Box::new(StackedRevision {
            inserted: revision_info(10, 1000, "Alice", "2026-01-01T00:00:00Z"),
            deleted: revision_info(11, 1001, "Bob", "2026-01-02T00:00:00Z"),
        }));
        let right = TrackingStatus::InsertedThenDeleted(Box::new(StackedRevision {
            inserted: revision_info(110, 1000, "Alice", "2026-01-01T00:00:00Z"),
            deleted: revision_info(111, 1001, "Bob", "2026-01-02T00:00:00Z"),
        }));
        let mut diffs = Vec::new();
        compare_tracking_status(&mut diffs, "status", &left, &right);
        assert!(
            diffs.is_empty(),
            "wire-id reminting is diagnostic only: {diffs:?}"
        );

        let changed = TrackingStatus::InsertedThenDeleted(Box::new(StackedRevision {
            inserted: revision_info(210, 2000, "Mallory", "2026-01-01T00:00:00Z"),
            deleted: revision_info(211, 1001, "Bob", "2026-01-02T00:00:00Z"),
        }));
        compare_tracking_status(&mut diffs, "status", &left, &changed);
        assert!(
            diffs
                .iter()
                .any(|difference| difference.path.ends_with("inserted.author")),
            "authored metadata remains fidelity: {diffs:?}"
        );
        assert!(
            diffs
                .iter()
                .any(|difference| difference.path.ends_with("inserted.identity")),
            "engine identity remains semantic fidelity: {diffs:?}"
        );
    }

    fn empty_canon_doc() -> CanonDoc {
        CanonDoc {
            id: NodeId::from("test"),
            blocks: Vec::new(),
            meta: DocMeta {
                schema_version: "0.1".to_string(),
                docx_fingerprint: DocFingerprint("test".to_string()),
                internal_ids_version: "0.1".to_string(),
            },
            headers: Vec::new(),
            footers: Vec::new(),
            footnotes: Vec::new(),
            endnotes: Vec::new(),
            comments: Vec::new(),
            comments_extended: Vec::new(),
            body_section_properties: None,
            body_section_property_change: None,
            compat_settings: CompatSettings::default(),
            even_and_odd_headers: None,
            document_background: None,
            document_protection: None,
        }
    }

    #[test]
    fn identical_docs_produce_no_diffs() {
        let a = empty_canon_doc();
        let b = empty_canon_doc();
        let diffs = compare_canon_docs(&a, &b);
        assert!(diffs.is_empty(), "expected no diffs, got: {diffs:?}");
    }

    #[test]
    fn different_node_ids_are_ignored() {
        let mut a = empty_canon_doc();
        let mut b = empty_canon_doc();
        a.id = NodeId::from("id_a");
        b.id = NodeId::from("id_b");
        // Different meta fingerprints should also be ignored
        a.meta.docx_fingerprint = DocFingerprint("fp_a".to_string());
        b.meta.docx_fingerprint = DocFingerprint("fp_b".to_string());
        let diffs = compare_canon_docs(&a, &b);
        assert!(
            diffs.is_empty(),
            "node IDs and meta should be ignored, got: {diffs:?}"
        );
    }

    #[test]
    fn different_block_count_detected() {
        let mut a = empty_canon_doc();
        let b = empty_canon_doc();
        a.blocks
            .push(normal_tracked_block(BlockNode::from(ParagraphNode {
                id: NodeId::from("p1"),
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
                tab_stops: Vec::new(),
                effective_tab_stops_rel: vec![],
                segments: Vec::new(),
                block_text_hash: None,
                numbering: None,
                has_direct_numbering: true,
                numbering_suppressed: false,
                materialized_numbering: None,
                rendered_text: None,
                literal_prefix: None,
                literal_prefix_marks: Vec::new(),
                literal_prefix_style_props: crate::domain::StyleProps::default(),
                literal_prefix_rpr_authored: crate::domain::RunRprAuthored::default(),
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
            })));
        let diffs = compare_canon_docs(&a, &b);
        assert_eq!(diffs.len(), 1);
        assert!(
            diffs[0].path.contains("blocks.len"),
            "expected blocks.len diff, got: {}",
            diffs[0].path
        );
    }

    /// A minimal, all-defaults `ParagraphNode`. Tests clone this and mutate a
    /// single field so the assertion isolates exactly one fidelity property.
    fn base_paragraph() -> ParagraphNode {
        ParagraphNode {
            id: NodeId::from("p1"),
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
            tab_stops: Vec::new(),
            effective_tab_stops_rel: vec![],
            segments: Vec::new(),
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

    fn doc_with_paragraph(p: ParagraphNode) -> CanonDoc {
        let mut doc = empty_canon_doc();
        doc.blocks.push(normal_tracked_block(BlockNode::from(p)));
        doc
    }

    /// `w:bidi` (right-to-left paragraph layout, §17.3.1.6) is real document
    /// fidelity: a paragraph laid out RTL must serialize back as RTL. Two
    /// paragraphs identical except for `bidi` are NOT equivalent, so the
    /// comparator must report a difference. This previously slipped through
    /// because `compare_paragraphs` silently omitted the field.
    #[test]
    fn bidi_difference_detected() {
        let mut a = base_paragraph();
        a.bidi = Some(true);
        let b = base_paragraph(); // bidi = None
        let diffs = compare_canon_docs(&doc_with_paragraph(a), &doc_with_paragraph(b));
        assert!(
            diffs.iter().any(|d| d.path.contains("bidi")),
            "expected a bidi difference, got: {diffs:?}"
        );
    }

    /// `w:textDirection` (§17.3.1.40) controls glyph flow within the paragraph
    /// (e.g. vertical East-Asian text). Changing it changes how the document
    /// reads, so a roundtrip must preserve it and the comparator must catch a
    /// mismatch.
    #[test]
    fn text_direction_difference_detected() {
        let mut a = base_paragraph();
        a.text_direction = Some(TextDirection::TbRl);
        let b = base_paragraph(); // text_direction = None
        let diffs = compare_canon_docs(&doc_with_paragraph(a), &doc_with_paragraph(b));
        assert!(
            diffs.iter().any(|d| d.path.contains("text_direction")),
            "expected a text_direction difference, got: {diffs:?}"
        );
    }

    /// `w:framePr` (§17.3.1.11) positions the paragraph as a text frame. A
    /// framed paragraph and a non-framed one lay out completely differently, so
    /// presence of a frame is fidelity the comparator must report.
    #[test]
    fn frame_pr_difference_detected() {
        let mut a = base_paragraph();
        a.frame_pr = Some(FrameProperties {
            width: Some(2000),
            height: None,
            h_rule: None,
            h_space: None,
            v_space: None,
            wrap: None,
            v_anchor: None,
            h_anchor: None,
            x: None,
            x_align: None,
            y: None,
            y_align: None,
            extra_attrs: Vec::new(),
        });
        let b = base_paragraph(); // frame_pr = None
        let diffs = compare_canon_docs(&doc_with_paragraph(a), &doc_with_paragraph(b));
        assert!(
            diffs.iter().any(|d| d.path.contains("frame_pr")),
            "expected a frame_pr difference, got: {diffs:?}"
        );
    }

    /// `w:textAlignment` (§17.3.1.39) sets vertical glyph alignment on each
    /// line. It is direct paragraph formatting that a roundtrip preserves.
    #[test]
    fn text_alignment_difference_detected() {
        let mut a = base_paragraph();
        a.text_alignment = Some(TextAlignment::Center);
        let b = base_paragraph();
        let diffs = compare_canon_docs(&doc_with_paragraph(a), &doc_with_paragraph(b));
        assert!(
            diffs.iter().any(|d| d.path.contains("text_alignment")),
            "expected a text_alignment difference, got: {diffs:?}"
        );
    }

    /// `w:cnfStyle` (§17.3.1.8) records which table conditional formats apply to
    /// a paragraph inside a cell. Dropping it on roundtrip changes the rendered
    /// banding/heading emphasis, so it is fidelity the comparator must report.
    #[test]
    fn cnf_style_difference_detected() {
        let mut a = base_paragraph();
        a.cnf_style = Some(CnfStyle {
            val: Some("100000000000".to_string()),
            first_row: true,
            last_row: false,
            first_column: false,
            last_column: false,
            odd_v_band: false,
            even_v_band: false,
            odd_h_band: false,
            even_h_band: false,
            first_row_first_column: false,
            first_row_last_column: false,
            last_row_first_column: false,
            last_row_last_column: false,
        });
        let b = base_paragraph();
        let diffs = compare_canon_docs(&doc_with_paragraph(a), &doc_with_paragraph(b));
        assert!(
            diffs.iter().any(|d| d.path.contains("cnf_style")),
            "expected a cnf_style difference, got: {diffs:?}"
        );
    }

    /// A paragraph's preserved pPr remainder (unmodeled children like
    /// w:suppressLineNumbers, captured verbatim at import so they survive
    /// re-serialization) is real document content. Two paragraphs that
    /// differ only in which unmodeled children they carry are NOT
    /// equivalent, so the comparator must report a difference.
    #[test]
    fn preserved_ppr_difference_detected() {
        let mut a = base_paragraph();
        a.preserved_ppr = vec![crate::domain::PreservedProp {
            name: "w:suppressLineNumbers".to_string(),
            raw_xml: "<w:suppressLineNumbers/>".to_string(),
        }];
        let b = base_paragraph(); // preserved_ppr = empty
        let diffs = compare_canon_docs(&doc_with_paragraph(a), &doc_with_paragraph(b));
        assert!(
            diffs.iter().any(|d| d.path.contains("preserved_ppr")),
            "expected a preserved_ppr difference, got: {diffs:?}"
        );
    }

    #[test]
    fn synthesized_text_ignored_in_numbering() {
        let mut a = empty_canon_doc();
        let mut b = empty_canon_doc();

        let make_para = |synth: &str| -> TrackedBlock {
            normal_tracked_block(BlockNode::from(ParagraphNode {
                id: NodeId::from("p1"),
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
                tab_stops: Vec::new(),
                effective_tab_stops_rel: vec![],
                segments: Vec::new(),
                block_text_hash: None,
                numbering: Some(NumberingInfo {
                    num_id: 1,
                    ilvl: 0,
                    synthesized_text: synth.to_string(),
                    is_bullet: false,
                    restart_numbering: false,
                }),
                has_direct_numbering: true,
                numbering_suppressed: false,
                materialized_numbering: None,
                rendered_text: None,
                literal_prefix: None,
                literal_prefix_marks: Vec::new(),
                literal_prefix_style_props: crate::domain::StyleProps::default(),
                literal_prefix_rpr_authored: crate::domain::RunRprAuthored::default(),
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
            }))
        };

        a.blocks.push(make_para("1."));
        b.blocks.push(make_para("2."));

        let diffs = compare_canon_docs(&a, &b);
        assert!(
            diffs.is_empty(),
            "synthesized_text should be ignored, got: {diffs:?}"
        );
    }
}

#[cfg(test)]
mod segment_coalescing_tests {
    use super::*;
    use crate::domain::{
        DecorationNode, DecorationType, DocPart, NodeId, ParagraphNode, ProofRef, StyleProps,
        TextNode, TrackedSegment, TrackingStatus,
    };

    fn text(t: &str) -> InlineNode {
        InlineNode::from(TextNode {
            id: NodeId::from("t"),
            text_role: None,
            text: t.to_string(),
            marks: Vec::new(),
            style_props: StyleProps::default(),
            rpr_authored: Default::default(),
            source_run_attrs: Vec::new(),
            formatting_change: None,
        })
    }

    fn bookmark_end() -> InlineNode {
        InlineNode::from(DecorationNode {
            id: NodeId::from("d"),
            kind: DecorationType::Bookmark,
            opaque_ref: "p:deco:1".to_string(),
            proof_ref: ProofRef {
                part: DocPart::DocumentXml,
                block_id: NodeId::from("d"),
                docx_anchor: String::new(),
            },
            wrapper_marks: Vec::new(),
            wrapper_style_props: StyleProps::default(),
            joins_following_text_run: false,
            raw_xml: Some(br#"<w:bookmarkEnd w:id="2"/>"#.to_vec()),
            origin: None,
        })
    }

    fn para(segments: Vec<TrackedSegment>) -> ParagraphNode {
        let mut p = ParagraphNode::new_story_body("p", "", None);
        p.segments = segments;
        p
    }

    fn seg(status: TrackingStatus, inlines: Vec<InlineNode>) -> TrackedSegment {
        TrackedSegment { status, inlines }
    }

    /// Segment boundaries between EQUAL-status neighbors are parse
    /// artifacts: segments exist to partition inlines by tracking status,
    /// and the importer legitimately opens a fresh segment for a
    /// structurally-anchored marker (e.g. a between-blocks bookmarkEnd)
    /// that a rebuild folds back into its neighbor. Same status, same
    /// inline sequence ⇒ same content.
    #[test]
    fn adjacent_equal_status_segments_compare_equal_to_their_merge() {
        let split = para(vec![
            seg(TrackingStatus::Normal, vec![text("GOVERNMENT NOTICES")]),
            seg(TrackingStatus::Normal, vec![bookmark_end()]),
        ]);
        let merged = para(vec![seg(
            TrackingStatus::Normal,
            vec![text("GOVERNMENT NOTICES"), bookmark_end()],
        )]);
        let mut diffs = Vec::new();
        compare_paragraphs(&mut diffs, "block[0].paragraph", &split, &merged);
        assert!(diffs.is_empty(), "{diffs:?}");
    }

    /// The guard in the other direction: DIFFERENT statuses never coalesce —
    /// a deleted run folded into a normal segment is a real content change.
    #[test]
    fn different_status_segments_still_differ_from_a_merge() {
        let split = para(vec![
            seg(TrackingStatus::Normal, vec![text("Keep")]),
            seg(
                TrackingStatus::Deleted(crate::domain::RevisionInfo {
                    revision_id: 5,
                    identity: 5,
                    author: Some("A".to_string()),
                    date: None,
                    apply_op_id: None,
                }),
                vec![text("Gone")],
            ),
        ]);
        let merged = para(vec![seg(
            TrackingStatus::Normal,
            vec![text("Keep"), text("Gone")],
        )]);
        let mut diffs = Vec::new();
        compare_paragraphs(&mut diffs, "block[0].paragraph", &split, &merged);
        assert!(!diffs.is_empty(), "a status change is real content");
    }
}
