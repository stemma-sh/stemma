//! HTML read projection.
//!
//! A pure, one-way renderer of [`DocumentView`] (one of the "many read
//! projections" in the engine's domain-model §4), modeled on
//! [`crate::extended_markdown`]. It is *honest* (nothing addressable is lost)
//! but it is **not pixel fidelity**: tables and opaque blocks render as
//! addressable placeholder `<div>`s carrying their flattened text, never a
//! rich visual reproduction. This is the surface for an HTML-rendering reader
//! (a chat UI, a preview pane), not a substitute for the DOCX itself.
//!
//! Contract:
//! - **Every block id surfaces** as an `id`/`data-id` attribute, so any block
//!   is addressable from the HTML.
//! - **All text is HTML-escaped** (`&`, `<`, `>`, `"`), at the edge, so block
//!   text can never inject markup.
//! - `Heading{level}` → `<h1>`..`<h6>` (a level above 6 clamps to `<h6>` with a
//!   `data-level` carrying the true level — honest, not silently lost);
//!   `Paragraph` → `<p>`; `Table`/`Opaque` block → a `<div data-id data-kind>`
//!   placeholder carrying the block's flattened text (placeholder honesty,
//!   parallel to extended-markdown's `<obj>`).
//! - Meaningful marks → `<strong>/<em>/<u>/<s>/<sub>/<sup>`.
//! - Tracked spans → `<ins data-rev data-author>` / `<del data-rev data-author>`.
//! - Each opaque inline anchor → **exactly one** addressable
//!   `<span class="anchor" data-id data-kind>` element (parallel to the
//!   one-U+FFFC-per-anchor rule in [`crate::view::to_plain_text`]): never
//!   dropped, never expanded into two elements. A hard break is one such
//!   anchor (`data-kind="hard_break"`) whose span additionally wraps a real
//!   `<br/>`, so it both stays addressable and actually breaks the line for an
//!   HTML consumer. The three comment markers surface the same way
//!   (`data-kind="comment"` / `"comment_range_start"` / `"comment_range_end"`).

use crate::view::{
    BlockRole, BlockView, DocumentView, OpaqueAnchorKind, SegmentView, TextMark, TrackStatus,
};

/// Render a [`DocumentView`] as HTML.
pub fn to_html(view: &DocumentView) -> String {
    to_html_blocks(&view.blocks)
}

/// Render a slice of blocks (a section / window) as HTML. Same per-block markup
/// as [`to_html`]; used by windowed reads so a windowed render is exactly the
/// slice of the full render.
pub fn to_html_blocks(blocks: &[BlockView]) -> String {
    let mut out = String::new();
    for (i, block) in blocks.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        write_block(&mut out, block);
    }
    out
}

fn write_block(out: &mut String, block: &BlockView) {
    let id = block.id.to_string();
    match &block.role {
        BlockRole::Heading { level } => {
            // h1..h6; a level above 6 clamps to h6 but keeps the true level on
            // data-level so it is recorded, not silently lost.
            let tag_level = (*level).clamp(1, 6);
            out.push_str(&format!("<h{tag_level} id=\"{}\"", escape_attr(&id)));
            if *level > 6 {
                out.push_str(&format!(" data-level=\"{level}\""));
            }
            write_block_status_attr(out, &block.block_status);
            out.push('>');
            write_literal_prefix(out, block.literal_prefix.as_deref());
            write_segments(out, &block.segments);
            out.push_str(&format!("</h{tag_level}>"));
        }
        BlockRole::Paragraph => {
            out.push_str(&format!("<p id=\"{}\"", escape_attr(&id)));
            write_block_status_attr(out, &block.block_status);
            out.push('>');
            write_literal_prefix(out, block.literal_prefix.as_deref());
            write_segments(out, &block.segments);
            out.push_str("</p>");
        }
        BlockRole::Table => {
            write_placeholder_block(out, &id, "table", None, &block.text, &block.block_status)
        }
        BlockRole::Opaque => write_placeholder_block(
            out,
            &id,
            "opaque",
            block.opaque_label.as_deref(),
            &block.text,
            &block.block_status,
        ),
    }
}

/// A Table / Opaque block: an addressable placeholder `<div>` carrying the
/// flattened block text (HTML-escaped). Placeholder honesty — the content is
/// legible and addressable, but this is not a rich visual reproduction.
///
/// `label` is the opaque block's specific identity (`BlockView::opaque_label`
/// — e.g. `"quarantined_nested_tracked_changes"`), surfaced as `data-label`
/// when present so a bulk HTML read distinguishes a quarantined block from any
/// other opaque one, not just from `data-kind="opaque"` alone. `None` for a
/// `Table` (which has no `opaque_label`).
fn write_placeholder_block(
    out: &mut String,
    id: &str,
    kind: &str,
    label: Option<&str>,
    text: &str,
    block_status: &TrackStatus,
) {
    out.push_str(&format!(
        "<div data-id=\"{}\" data-kind=\"{kind}\"",
        escape_attr(id)
    ));
    if let Some(label) = label {
        out.push_str(&format!(" data-label=\"{}\"", escape_attr(label)));
    }
    write_block_status_attr(out, block_status);
    out.push('>');
    out.push_str(&escape_text(text));
    out.push_str("</div>");
}

/// A whole-block tracked insert/delete is flagged as a `data-block-status`
/// attribute, so the reader sees the block itself is a tracked change.
fn write_block_status_attr(out: &mut String, status: &TrackStatus) {
    match status {
        TrackStatus::Inserted(_) => out.push_str(" data-block-status=\"inserted\""),
        TrackStatus::Deleted(_) => out.push_str(" data-block-status=\"deleted\""),
        TrackStatus::InsertedThenDeleted { .. } => {
            out.push_str(" data-block-status=\"inserted_then_deleted\"")
        }
        TrackStatus::Normal => {}
    }
}

/// Emit a paragraph's typed-in enumeration label (`"1."`, `"(a)"`) ahead of its
/// body, as `<span data-numbering-text="...">{label}\t</span>`. The label is
/// real text Word reads (the serializer emits it as a run); it lives outside
/// `segments` (in `literal_prefix`), so it is rendered here, untracked and
/// HTML-escaped. The `data-numbering-text` attribute mirrors the frontend's
/// own enumeration hook so the marker is distinguishable from body text. No-op
/// when the paragraph carries no literal prefix.
fn write_literal_prefix(out: &mut String, label: Option<&str>) {
    let Some(label) = label else { return };
    out.push_str(&format!(
        "<span data-numbering-text=\"{}\">{}\t</span>",
        escape_attr(label),
        escape_text(label),
    ));
}

fn write_segments(out: &mut String, segments: &[SegmentView]) {
    for seg in segments {
        write_segment(out, seg);
    }
}

fn write_segment(out: &mut String, seg: &SegmentView) {
    match seg {
        SegmentView::Text {
            text,
            status,
            marks,
            ..
        } => {
            let inner = apply_marks(&escape_text(text), marks);
            wrap_tracked(out, status, &inner);
        }
        SegmentView::Opaque {
            id,
            kind,
            status,
            text,
            ..
        } => {
            // Exactly one addressable anchor element per opaque inline. A hard
            // break additionally carries a real `<br/>` inside the anchor span
            // so an HTML consumer actually sees the line separation (the span
            // wrapper alone renders nothing) while staying the one addressable
            // element the rest of this projection promises.
            let inner = match kind {
                OpaqueAnchorKind::HardBreak => "<br/>".to_string(),
                _ => text.as_deref().map(escape_text).unwrap_or_default(),
            };
            let anchor = format!(
                "<span class=\"anchor\" data-id=\"{}\" data-kind=\"{}\">{inner}</span>",
                escape_attr(&id.to_string()),
                anchor_kind(*kind),
            );
            wrap_tracked(out, status, &anchor);
        }
    }
}

/// Wrap rendered inner HTML in `<ins>`/`<del>` carrying the revision metadata,
/// or emit it bare when the span is `Normal`.
fn wrap_tracked(out: &mut String, status: &TrackStatus, inner: &str) {
    match status {
        TrackStatus::Normal => out.push_str(inner),
        TrackStatus::Inserted(rev) => out.push_str(&format!(
            "<ins data-rev=\"{}\"{}>{inner}</ins>",
            rev.revision_id,
            author_attr(rev.author.as_deref())
        )),
        TrackStatus::Deleted(rev) => out.push_str(&format!(
            "<del data-rev=\"{}\"{}>{inner}</del>",
            rev.revision_id,
            author_attr(rev.author.as_deref())
        )),
        TrackStatus::InsertedThenDeleted { inserted, deleted } => out.push_str(&format!(
            "<ins data-rev=\"{}\"{}><del data-rev=\"{}\"{}>{inner}</del></ins>",
            inserted.revision_id,
            author_attr(inserted.author.as_deref()),
            deleted.revision_id,
            author_attr(deleted.author.as_deref())
        )),
    }
}

/// Wrap text in the tags for its meaningful marks. `text` is already escaped.
/// Nesting order is fixed and deterministic.
fn apply_marks(text: &str, marks: &[TextMark]) -> String {
    let mut s = text.to_string();
    for (mark, tag) in [
        (TextMark::Subscript, "sub"),
        (TextMark::Superscript, "sup"),
        (TextMark::Strike, "s"),
        (TextMark::Underline, "u"),
        (TextMark::Italic, "em"),
        (TextMark::Bold, "strong"),
    ] {
        if marks.contains(&mark) {
            s = format!("<{tag}>{s}</{tag}>");
        }
    }
    s
}

fn anchor_kind(kind: OpaqueAnchorKind) -> &'static str {
    match kind {
        OpaqueAnchorKind::Drawing => "image",
        OpaqueAnchorKind::Equation => "equation",
        OpaqueAnchorKind::Hyperlink => "hyperlink",
        OpaqueAnchorKind::Field => "field",
        OpaqueAnchorKind::FootnoteRef => "footnote_ref",
        OpaqueAnchorKind::EndnoteRef => "endnote_ref",
        OpaqueAnchorKind::Comment => "comment",
        OpaqueAnchorKind::ContentControl => "content_control",
        OpaqueAnchorKind::HardBreak => "hard_break",
        OpaqueAnchorKind::CommentRangeStart => "comment_range_start",
        OpaqueAnchorKind::CommentRangeEnd => "comment_range_end",
        OpaqueAnchorKind::Other => "other",
    }
}

fn author_attr(author: Option<&str>) -> String {
    match author {
        Some(a) => format!(" data-author=\"{}\"", escape_attr(a)),
        None => String::new(),
    }
}

/// HTML-escape text content: `&`, `<`, `>` (and `"` for safety even in text).
fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            other => out.push(other),
        }
    }
    out
}

/// HTML-escape an attribute value: same set as text plus `"` (already covered).
fn escape_attr(s: &str) -> String {
    escape_text(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::NodeId;
    use crate::view::RevisionView;

    fn rev(id: u32, author: &str) -> RevisionView {
        RevisionView {
            revision_id: id,
            author: Some(author.to_string()),
            date: None,
            apply_op_id: None,
        }
    }

    fn text_seg(text: &str, marks: Vec<TextMark>) -> SegmentView {
        SegmentView::Text {
            text: text.to_string(),
            status: TrackStatus::Normal,
            marks,
            handle: None,
        }
    }

    fn para(id: &str, segments: Vec<SegmentView>) -> BlockView {
        BlockView {
            id: NodeId::from(id),
            role: BlockRole::Paragraph,
            style_id: None,
            role_token: None,
            list: None,
            cells: Vec::new(),
            table: None,
            text: String::new(),
            block_status: TrackStatus::Normal,
            paragraph_mark_status: TrackStatus::Normal,
            guard: String::new(),
            literal_prefix: None,
            segments,
            opaque_label: None,
        }
    }

    #[test]
    fn headings_map_to_h1_through_h6_and_clamp_above_six() {
        // Domain rule: a Heading{level} renders as <h{level}> for 1..=6; a level
        // above 6 (HeadingLevel goes to H9) clamps to <h6> but keeps the true
        // level on data-level so it is recorded, never silently lost.
        for level in 1u8..=6 {
            let block = BlockView {
                id: NodeId::from(format!("p_{level}")),
                role: BlockRole::Heading { level },
                style_id: None,
                role_token: None,
                list: None,
                cells: Vec::new(),
                table: None,
                text: String::new(),
                block_status: TrackStatus::Normal,
                paragraph_mark_status: TrackStatus::Normal,
                guard: String::new(),
                literal_prefix: None,
                segments: vec![text_seg("H", vec![])],
                opaque_label: None,
            };
            let html = to_html_blocks(std::slice::from_ref(&block));
            assert!(html.starts_with(&format!("<h{level} ")), "{html}");
            assert!(html.ends_with(&format!("</h{level}>")), "{html}");
            assert!(
                !html.contains("data-level"),
                "no clamp marker for {level}: {html}"
            );
        }

        let block = BlockView {
            id: NodeId::from("p_9"),
            role: BlockRole::Heading { level: 9 },
            style_id: None,
            role_token: None,
            list: None,
            cells: Vec::new(),
            table: None,
            text: String::new(),
            block_status: TrackStatus::Normal,
            paragraph_mark_status: TrackStatus::Normal,
            guard: String::new(),
            literal_prefix: None,
            segments: vec![text_seg("Deep", vec![])],
            opaque_label: None,
        };
        let html = to_html_blocks(std::slice::from_ref(&block));
        assert!(html.starts_with("<h6 "), "clamped to h6: {html}");
        assert!(
            html.contains("data-level=\"9\""),
            "true level recorded: {html}"
        );
    }

    #[test]
    fn all_text_is_html_escaped() {
        // Domain rule: block text can never inject markup; every &, <, >, " in
        // visible text is escaped at the edge.
        let block = para("p_1", vec![text_seg("a < b && c > d \"x\"", vec![])]);
        let html = to_html_blocks(std::slice::from_ref(&block));
        assert!(
            html.contains("a &lt; b &amp;&amp; c &gt; d &quot;x&quot;"),
            "text must be escaped: {html}"
        );
        assert!(!html.contains("a < b"), "raw < must not survive: {html}");
    }

    #[test]
    fn exactly_one_addressable_anchor_element_per_opaque_segment() {
        // Domain rule (parallel to the one-U+FFFC-per-anchor rule): each opaque
        // SegmentView surfaces as EXACTLY one addressable
        // <span class="anchor" data-id ...>, never dropped, never doubled.
        let block = para(
            "p_1",
            vec![
                text_seg("See ", vec![]),
                SegmentView::Opaque {
                    id: NodeId::from("x_12"),
                    kind: OpaqueAnchorKind::Field,
                    status: TrackStatus::Normal,
                    text: Some("Section 5".to_string()),
                    handle: None,
                    metadata: None,
                },
                text_seg(" now", vec![]),
            ],
        );
        let html = to_html_blocks(std::slice::from_ref(&block));
        assert_eq!(
            html.matches("class=\"anchor\"").count(),
            1,
            "exactly one anchor element per opaque segment: {html}"
        );
        assert!(
            html.contains("data-id=\"x_12\""),
            "anchor is addressable: {html}"
        );
        assert!(html.contains("data-kind=\"field\""), "{html}");
    }

    #[test]
    fn marks_and_tracked_spans_render() {
        let block = para(
            "p_1",
            vec![
                text_seg("plain ", vec![]),
                text_seg("bold", vec![TextMark::Bold]),
                SegmentView::Text {
                    text: " added".to_string(),
                    status: TrackStatus::Inserted(rev(5, "Counsel")),
                    marks: vec![],
                    handle: None,
                },
                SegmentView::Text {
                    text: " removed".to_string(),
                    status: TrackStatus::Deleted(rev(5, "Counsel")),
                    marks: vec![],
                    handle: None,
                },
            ],
        );
        let html = to_html_blocks(std::slice::from_ref(&block));
        assert!(html.contains("<strong>bold</strong>"), "{html}");
        assert!(
            html.contains("<ins data-rev=\"5\" data-author=\"Counsel\"> added</ins>"),
            "{html}"
        );
        assert!(
            html.contains("<del data-rev=\"5\" data-author=\"Counsel\"> removed</del>"),
            "{html}"
        );
    }

    #[test]
    fn every_block_id_surfaces_as_id_or_data_id() {
        let blocks = vec![
            para("p_1", vec![text_seg("body", vec![])]),
            BlockView {
                id: NodeId::from("t_2"),
                role: BlockRole::Table,
                style_id: None,
                role_token: None,
                list: None,
                cells: Vec::new(),
                table: None,
                text: "cell text".to_string(),
                block_status: TrackStatus::Normal,
                paragraph_mark_status: TrackStatus::Normal,
                guard: String::new(),
                literal_prefix: None,
                segments: vec![],
                opaque_label: None,
            },
        ];
        let html = to_html_blocks(&blocks);
        assert!(html.contains("id=\"p_1\""), "paragraph id surfaces: {html}");
        assert!(
            html.contains("data-id=\"t_2\""),
            "table id surfaces: {html}"
        );
        assert!(html.contains("data-kind=\"table\""), "{html}");
        assert!(
            html.contains("cell text"),
            "placeholder carries text: {html}"
        );
    }

    #[test]
    fn literal_prefix_renders_as_a_leading_numbering_span() {
        // The HTML read surfaces the typed-in enumeration label ahead of the
        // body as a distinguishable, HTML-escaped <span data-numbering-text>,
        // mirroring the frontend's enumeration hook. The label is real text Word
        // reads; it is not one of the body segments.
        let mut block = para("p_5", vec![text_seg("Events", vec![])]);
        block.role = BlockRole::Heading { level: 1 };
        block.literal_prefix = Some("1.".to_string());
        let html = to_html_blocks(&[block]);
        assert!(
            html.contains(r#"<span data-numbering-text="1.">1.&#9;</span>"#)
                || html.contains("<span data-numbering-text=\"1.\">1.\t</span>"),
            "leading numbering span with the label + tab: {html}"
        );
        assert!(html.contains("Events"), "body still rendered: {html}");
    }

    #[test]
    fn opaque_block_label_surfaces_as_data_label() {
        // Domain rule: a quarantined-nested-tracked-changes block must never
        // read as indistinguishable from any other opaque block (docs/
        // domain-model quarantine contract). `BlockView::opaque_label` carries
        // that identity; before this fix it reached `opaque_label` but never
        // the bulk HTML render (`data-kind="opaque"` with no further signal).
        let block = BlockView {
            id: NodeId::from("o_1"),
            role: BlockRole::Opaque,
            style_id: None,
            role_token: None,
            list: None,
            cells: Vec::new(),
            table: None,
            text: String::new(),
            block_status: TrackStatus::Normal,
            paragraph_mark_status: TrackStatus::Normal,
            guard: String::new(),
            literal_prefix: None,
            segments: vec![],
            opaque_label: Some("quarantined_nested_tracked_changes".to_string()),
        };
        let html = to_html_blocks(&[block]);
        assert!(html.contains("data-kind=\"opaque\""), "{html}");
        assert!(
            html.contains("data-label=\"quarantined_nested_tracked_changes\""),
            "the quarantine identity must reach the bulk render: {html}"
        );
    }

    #[test]
    fn opaque_block_with_no_label_omits_data_label() {
        // No silent fallback: absence of a label is absence of the attribute,
        // never an invented empty `data-label=""`.
        let block = BlockView {
            id: NodeId::from("o_2"),
            role: BlockRole::Opaque,
            style_id: None,
            role_token: None,
            list: None,
            cells: Vec::new(),
            table: None,
            text: String::new(),
            block_status: TrackStatus::Normal,
            paragraph_mark_status: TrackStatus::Normal,
            guard: String::new(),
            literal_prefix: None,
            segments: vec![],
            opaque_label: None,
        };
        let html = to_html_blocks(&[block]);
        assert!(!html.contains("data-label"), "{html}");
    }
}
