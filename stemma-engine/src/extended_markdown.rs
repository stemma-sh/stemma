//! Extended-markdown comprehension projection.
//!
//! The agent-facing read surface: prose that reads like a contract, with light
//! inline tags so every block and opaque object is addressable and tracked
//! changes are faithful. It is a pure, one-way renderer of [`DocumentView`]
//! (one of the "many read projections" in the engine's domain-model §4) —
//! honest (nothing addressable is lost) and
//! compact (no structural closing tags; newlines delimit blocks; low-signal run
//! properties are left to the detail view).
//!
//! Format (one block per logical unit):
//!
//! ```text
//! #p_7 role=para
//! The Receiving Party shall, for a period of 30 days, treat as <b>Confidential
//! Information</b> the materials in Section 5<field id=x_12>Section 5</field><fn id=f_3/>.
//! ```
//!
//! Tags: `#<id> role=...` block header; `<b>/<i>/<u>/<s>/<sub>/<sup>` meaningful
//! marks; `<fn>/<en>/<field>/<link>/<img>/<eq>/<comment>/<cc>/<br>/<obj>` opaque
//! anchors (self-closing unless they carry display text); `<ins>/<del>` tracked
//! spans. A content control renders as `<cc id=.. tag="..">value</cc>` (the
//! `tag` is the discovery key); an image carries `alt=".."` when present. A
//! hard line/page/column break renders as `<br id=../>`; a comment's range
//! boundaries (as opposed to its visible reference, `<comment id=../>`) render
//! as `<comment_start id=../>` / `<comment_end id=../>`.

use crate::view::{
    BlockRole, BlockView, DocumentView, OpaqueAnchorKind, OpaqueMetadata, SegmentView, TextMark,
    TrackStatus,
};

/// Render a [`DocumentView`] as the extended-markdown comprehension surface.
pub fn to_extended_markdown(view: &DocumentView) -> String {
    to_extended_markdown_blocks(&view.blocks)
}

/// Render a slice of blocks (a section / window) as extended markdown. Same
/// format as [`to_extended_markdown`]; used by windowed reads like `get_section`.
pub fn to_extended_markdown_blocks(blocks: &[BlockView]) -> String {
    let mut out = String::new();
    for (i, block) in blocks.iter().enumerate() {
        if i > 0 {
            out.push_str("\n\n");
        }
        write_block(&mut out, block);
    }
    out
}

fn write_block(out: &mut String, block: &BlockView) {
    // Header: id + structural role (+ heading level, + style when present).
    out.push('#');
    out.push_str(&block.id.to_string());
    match &block.role {
        BlockRole::Paragraph => out.push_str(" role=para"),
        BlockRole::Heading { level } => {
            out.push_str(" role=heading level=");
            out.push_str(&level.to_string());
        }
        BlockRole::Table => out.push_str(" role=table"),
        BlockRole::Opaque => out.push_str(" role=opaque"),
    }
    if let Some(style) = &block.style_id
        && !style.is_empty()
    {
        out.push_str(" style=");
        out.push_str(style);
    }
    // A whole-block tracked insert/delete is flagged on the header so the model
    // sees the block itself is a tracked change, not just its contents.
    match &block.block_status {
        TrackStatus::Inserted(_) => out.push_str(" status=inserted"),
        TrackStatus::Deleted(_) => out.push_str(" status=deleted"),
        TrackStatus::InsertedThenDeleted { .. } => out.push_str(" status=inserted_then_deleted"),
        TrackStatus::Normal => {}
    }
    out.push('\n');

    // Body.
    match &block.role {
        // Tables and opaque blocks are placeholders: the model may read them but
        // not author them directly. Show a placeholder tag plus the flattened
        // text so the content is still legible (richer table projection is a
        // documented extension point).
        BlockRole::Table => {
            out.push_str(&format!("<obj id={} kind=table/>", block.id));
            if !block.text.is_empty() {
                out.push('\n');
                out.push_str(&block.text);
            }
        }
        BlockRole::Opaque => {
            // `opaque_label` is the block's specific identity (e.g.
            // "quarantined_nested_tracked_changes") — surfaced as `label=..`
            // so a bulk read distinguishes it from any other opaque block, not
            // just from `kind=opaque` alone (a quarantined block must never
            // read as indistinguishable ordinary content).
            match &block.opaque_label {
                Some(label) => out.push_str(&format!(
                    "<obj id={} kind=opaque label={}/>",
                    block.id,
                    attr_quote(label)
                )),
                None => out.push_str(&format!("<obj id={} kind=opaque/>", block.id)),
            }
        }
        BlockRole::Paragraph | BlockRole::Heading { .. } => {
            // The typed-in enumeration label (`"1."`, `"(a)"`) is real text Word
            // reads but lives outside `segments` (in `literal_prefix`); emit it
            // ahead of the body as plain `"{label}\t"` so the redline/markdown
            // read shows what Word shows. It is untracked structural text — no
            // <ins>/<del> wrapper, and not a span the model can target.
            if let Some(label) = &block.literal_prefix {
                out.push_str(label);
                out.push('\t');
            }
            for seg in &block.segments {
                write_segment(out, seg);
            }
        }
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
            let inner = apply_marks(text, marks);
            match status {
                TrackStatus::Normal => out.push_str(&inner),
                TrackStatus::Inserted(rev) => {
                    out.push_str(&format!(
                        "<ins id={}{}>{}</ins>",
                        rev.revision_id,
                        by(rev.author.as_deref()),
                        inner
                    ));
                }
                TrackStatus::Deleted(rev) => {
                    out.push_str(&format!(
                        "<del id={}{}>{}</del>",
                        rev.revision_id,
                        by(rev.author.as_deref()),
                        inner
                    ));
                }
                // The stacked state nests, mirroring the markup it came from:
                // honest, compact, and a shape models know.
                TrackStatus::InsertedThenDeleted { inserted, deleted } => {
                    out.push_str(&format!(
                        "<ins id={}{}><del id={}{}>{}</del></ins>",
                        inserted.revision_id,
                        by(inserted.author.as_deref()),
                        deleted.revision_id,
                        by(deleted.author.as_deref()),
                        inner
                    ));
                }
            }
        }
        SegmentView::Opaque {
            id,
            kind,
            status,
            text,
            metadata,
            ..
        } => {
            let tag = opaque_tag(&id.to_string(), *kind, text.as_deref(), metadata.as_ref());
            match status {
                TrackStatus::Normal => out.push_str(&tag),
                TrackStatus::Inserted(rev) => {
                    out.push_str(&format!("<ins id={}>{}</ins>", rev.revision_id, tag))
                }
                TrackStatus::Deleted(rev) => {
                    out.push_str(&format!("<del id={}>{}</del>", rev.revision_id, tag))
                }
                TrackStatus::InsertedThenDeleted { inserted, deleted } => out.push_str(&format!(
                    "<ins id={}><del id={}>{}</del></ins>",
                    inserted.revision_id, deleted.revision_id, tag
                )),
            }
        }
    }
}

/// Wrap text in the tags for its meaningful marks. Nesting order is fixed and
/// irrelevant to rendering; it only needs to be deterministic.
fn apply_marks(text: &str, marks: &[TextMark]) -> String {
    let mut s = text.to_string();
    for (mark, tag) in [
        (TextMark::Subscript, "sub"),
        (TextMark::Superscript, "sup"),
        (TextMark::Strike, "s"),
        (TextMark::Underline, "u"),
        (TextMark::Italic, "i"),
        (TextMark::Bold, "b"),
    ] {
        if marks.contains(&mark) {
            s = format!("<{tag}>{s}</{tag}>");
        }
    }
    s
}

fn opaque_tag(
    id: &str,
    kind: OpaqueAnchorKind,
    text: Option<&str>,
    metadata: Option<&OpaqueMetadata>,
) -> String {
    match kind {
        OpaqueAnchorKind::Hyperlink => match text {
            Some(t) => format!("<link id={id}>{t}</link>"),
            None => format!("<link id={id}/>"),
        },
        OpaqueAnchorKind::Field => match text {
            Some(t) => format!("<field id={id}>{t}</field>"),
            None => format!("<field id={id}/>"),
        },
        OpaqueAnchorKind::FootnoteRef => format!("<fn id={id}/>"),
        OpaqueAnchorKind::EndnoteRef => format!("<en id={id}/>"),
        // A content control surfaces its `tag` (the discovery key an agent
        // matches on) inline; alias and control kind cost tokens on every bulk
        // read and live in `read_block` instead. `<obj>` becomes `<cc>` ONLY
        // for SDTs — most `<obj>` anchors are genuinely opaque.
        OpaqueAnchorKind::ContentControl => {
            let tag_attr = match metadata {
                Some(OpaqueMetadata::ContentControl { tag: Some(t), .. }) => {
                    format!(" tag={}", attr_quote(t))
                }
                _ => String::new(),
            };
            match text {
                Some(t) => format!("<cc id={id}{tag_attr}>{t}</cc>"),
                None => format!("<cc id={id}{tag_attr}/>"),
            }
        }
        OpaqueAnchorKind::Drawing => {
            // Add alt text when present (short, high-value for accessibility);
            // extent stays out of markdown (available in `read_block`).
            match metadata {
                Some(OpaqueMetadata::Drawing {
                    alt_text: Some(alt),
                    ..
                }) => format!("<img id={id} alt={}/>", attr_quote(alt)),
                _ => format!("<img id={id}/>"),
            }
        }
        OpaqueAnchorKind::Equation => format!("<eq id={id}/>"),
        // The reference marker (also what an imported `commentReference`
        // projects to) and the range boundaries share the id space but are
        // distinct tags, so a reader can tell "here's the comment" from
        // "here's where the commented range starts/ends".
        OpaqueAnchorKind::Comment => format!("<comment id={id}/>"),
        OpaqueAnchorKind::CommentRangeStart => format!("<comment_start id={id}/>"),
        OpaqueAnchorKind::CommentRangeEnd => format!("<comment_end id={id}/>"),
        OpaqueAnchorKind::HardBreak => format!("<br id={id}/>"),
        OpaqueAnchorKind::Other => format!("<obj id={id}/>"),
    }
}

/// Quote an attribute value, escaping `"`, `<`, `>`, `&` so the tag stays
/// well-formed and unambiguous (a `tag`/`alt` may contain spaces or quotes).
fn attr_quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

fn by(author: Option<&str>) -> String {
    match author {
        Some(a) => format!(" by=\"{a}\""),
        None => String::new(),
    }
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

    #[test]
    fn renders_ids_marks_anchors_and_tracked_spans() {
        let view = DocumentView {
            blocks: vec![
                BlockView {
                    id: NodeId::from("p_1"),
                    role: BlockRole::Heading { level: 2 },
                    style_id: Some("Heading2".to_string()),
                    role_token: None,
                    list: None,
                    cells: Vec::new(),
                    table: None,
                    text: "Confidentiality".to_string(),
                    block_status: TrackStatus::Normal,
                    paragraph_mark_status: TrackStatus::Normal,
                    guard: String::new(),
                    literal_prefix: None,
                    segments: vec![SegmentView::Text {
                        text: "Confidentiality".to_string(),
                        status: TrackStatus::Normal,
                        marks: vec![],
                        handle: None,
                    }],
                    opaque_label: None,
                },
                BlockView {
                    id: NodeId::from("p_2"),
                    role: BlockRole::Paragraph,
                    style_id: None,
                    role_token: None,
                    list: None,
                    cells: Vec::new(),
                    table: None,
                    text: "The term Confidential Information means data; see Section 5."
                        .to_string(),
                    block_status: TrackStatus::Normal,
                    paragraph_mark_status: TrackStatus::Normal,
                    guard: String::new(),
                    literal_prefix: None,
                    segments: vec![
                        SegmentView::Text {
                            text: "The term ".to_string(),
                            status: TrackStatus::Normal,
                            marks: vec![],
                            handle: None,
                        },
                        SegmentView::Text {
                            text: "Confidential Information".to_string(),
                            status: TrackStatus::Normal,
                            marks: vec![TextMark::Bold],
                            handle: None,
                        },
                        SegmentView::Text {
                            text: " means data; see ".to_string(),
                            status: TrackStatus::Normal,
                            marks: vec![],
                            handle: None,
                        },
                        SegmentView::Opaque {
                            id: NodeId::from("x_12"),
                            kind: OpaqueAnchorKind::Field,
                            status: TrackStatus::Normal,
                            text: Some("Section 5".to_string()),
                            handle: None,
                            metadata: None,
                        },
                        SegmentView::Text {
                            text: " for 30".to_string(),
                            status: TrackStatus::Deleted(rev(5, "Counsel")),
                            marks: vec![],
                            handle: None,
                        },
                        SegmentView::Text {
                            text: " for 60".to_string(),
                            status: TrackStatus::Inserted(rev(5, "Counsel")),
                            marks: vec![],
                            handle: None,
                        },
                    ],
                    opaque_label: None,
                },
            ],
        };

        let md = to_extended_markdown(&view);

        // Block headers carry id, role, level, style.
        assert!(
            md.contains("#p_1 role=heading level=2 style=Heading2"),
            "{md}"
        );
        assert!(md.contains("#p_2 role=para"), "{md}");
        // Meaningful marks render as light tags.
        assert!(md.contains("<b>Confidential Information</b>"), "{md}");
        // Opaque field is an addressable anchor carrying its label, not dropped.
        assert!(md.contains("<field id=x_12>Section 5</field>"), "{md}");
        // Tracked spans render as ins/del with revision id and author.
        assert!(
            md.contains(r#"<del id=5 by="Counsel"> for 30</del>"#),
            "{md}"
        );
        assert!(
            md.contains(r#"<ins id=5 by="Counsel"> for 60</ins>"#),
            "{md}"
        );
        // Blocks are newline-delimited, no closing block tags.
        assert!(
            md.contains("\n\n#p_2"),
            "blocks separated by blank line: {md}"
        );
    }

    /// A one-paragraph view whose single segment is the given opaque anchor —
    /// the minimal fixture for the opaque-tag rendering tests.
    fn one_opaque_block(seg: SegmentView) -> DocumentView {
        DocumentView {
            blocks: vec![BlockView {
                id: NodeId::from("p_1"),
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
                segments: vec![seg],
                opaque_label: None,
            }],
        }
    }

    fn cc_segment(id: &str, tag: Option<&str>, value: Option<&str>) -> SegmentView {
        SegmentView::Opaque {
            id: NodeId::from(id),
            kind: OpaqueAnchorKind::ContentControl,
            status: TrackStatus::Normal,
            text: value.map(str::to_string),
            handle: None,
            metadata: Some(OpaqueMetadata::ContentControl {
                tag: tag.map(str::to_string),
                alias: Some("Tenant Name".to_string()),
                control: crate::view::SdtControlKind::PlainText,
                display_text: value.map(str::to_string),
                list_items: Vec::new(),
                checked: None,
            }),
        }
    }

    #[test]
    fn content_control_renders_cc_tag_value() {
        // §2.8: a content control surfaces <cc id=.. tag="..">value</cc>. The tag
        // is the discovery key; alias/control are NOT in the markdown (read_block
        // carries them).
        let view = one_opaque_block(cc_segment("o_42", Some("TenantName"), Some("Acme Corp")));
        let md = to_extended_markdown(&view);
        assert!(
            md.contains(r#"<cc id=o_42 tag="TenantName">Acme Corp</cc>"#),
            "{md}"
        );
        assert!(
            !md.contains("alias"),
            "alias must NOT appear in markdown: {md}"
        );
    }

    #[test]
    fn content_control_empty_value_self_closes() {
        let view = one_opaque_block(cc_segment("o_9", Some("Empty"), None));
        let md = to_extended_markdown(&view);
        assert!(md.contains(r#"<cc id=o_9 tag="Empty"/>"#), "{md}");
    }

    #[test]
    fn literal_prefix_label_leads_the_body_in_markdown() {
        // The redline/markdown read must show the typed-in enumeration label as
        // real leading text ("1.\t"), untracked (no <ins>/<del>), ahead of the
        // body segment — so an agent reviewing the redline reads what Word reads.
        let view = DocumentView {
            blocks: vec![BlockView {
                id: NodeId::from("p_1"),
                role: BlockRole::Heading { level: 1 },
                style_id: None,
                role_token: None,
                list: None,
                cells: Vec::new(),
                table: None,
                text: "1.\tEvents".to_string(),
                block_status: TrackStatus::Normal,
                paragraph_mark_status: TrackStatus::Normal,
                guard: String::new(),
                literal_prefix: Some("1.".to_string()),
                segments: vec![SegmentView::Text {
                    text: "Events".to_string(),
                    status: TrackStatus::Normal,
                    marks: vec![],
                    handle: None,
                }],
                opaque_label: None,
            }],
        };
        let md = to_extended_markdown(&view);
        assert!(md.contains("#p_1 role=heading level=1"), "{md}");
        assert!(
            md.contains("1.\tEvents"),
            "label leads the body as plain text: {md:?}"
        );
        assert!(
            !md.contains("<ins") && !md.contains("<del"),
            "the restored label is untracked, not a tracked change: {md:?}"
        );
    }

    #[test]
    fn image_renders_alt_when_present() {
        let with_alt = one_opaque_block(SegmentView::Opaque {
            id: NodeId::from("o_7"),
            kind: OpaqueAnchorKind::Drawing,
            status: TrackStatus::Normal,
            text: None,
            handle: None,
            metadata: Some(OpaqueMetadata::Drawing {
                extent_cx_emu: Some(1143000),
                extent_cy_emu: Some(685800),
                alt_text: Some("Acme logo".to_string()),
                embed_rid: Some("rId5".to_string()),
                textbox_text: None,
            }),
        });
        let md = to_extended_markdown(&with_alt);
        assert!(md.contains(r#"<img id=o_7 alt="Acme logo"/>"#), "{md}");
        // Extent stays OUT of markdown (available in read_block).
        assert!(
            !md.contains("1143000"),
            "extent must not be in markdown: {md}"
        );
    }

    #[test]
    fn image_no_alt_unchanged() {
        // Back-compat: an image with no alt text renders byte-identical to today.
        let no_alt = one_opaque_block(SegmentView::Opaque {
            id: NodeId::from("o_7"),
            kind: OpaqueAnchorKind::Drawing,
            status: TrackStatus::Normal,
            text: None,
            handle: None,
            metadata: Some(OpaqueMetadata::Drawing {
                extent_cx_emu: None,
                extent_cy_emu: None,
                alt_text: None,
                embed_rid: None,
                textbox_text: None,
            }),
        });
        let md = to_extended_markdown(&no_alt);
        assert!(md.contains("<img id=o_7/>"), "{md}");
        assert!(!md.contains("alt="), "no alt attribute when absent: {md}");
    }

    #[test]
    fn bare_opaque_unchanged() {
        // A smart tag (Other) still renders <obj id=../> — only SDTs become <cc>.
        let view = one_opaque_block(SegmentView::Opaque {
            id: NodeId::from("o_3"),
            kind: OpaqueAnchorKind::Other,
            status: TrackStatus::Normal,
            text: None,
            handle: None,
            metadata: None,
        });
        let md = to_extended_markdown(&view);
        assert!(md.contains("<obj id=o_3/>"), "{md}");
    }

    #[test]
    fn hard_break_and_comment_range_markers_render_distinct_tags() {
        // Parity check (view.rs's own test exercises the real IR walk; this
        // pins the markdown tag vocabulary for each new kind directly).
        let br = one_opaque_block(SegmentView::Opaque {
            id: NodeId::from("b_1"),
            kind: OpaqueAnchorKind::HardBreak,
            status: TrackStatus::Normal,
            text: None,
            handle: None,
            metadata: None,
        });
        assert!(to_extended_markdown(&br).contains("<br id=b_1/>"));

        let start = one_opaque_block(SegmentView::Opaque {
            id: NodeId::from("0"),
            kind: OpaqueAnchorKind::CommentRangeStart,
            status: TrackStatus::Normal,
            text: None,
            handle: None,
            metadata: Some(OpaqueMetadata::NoteReference {
                reference_id: "0".to_string(),
            }),
        });
        assert!(to_extended_markdown(&start).contains("<comment_start id=0/>"));

        let end = one_opaque_block(SegmentView::Opaque {
            id: NodeId::from("0"),
            kind: OpaqueAnchorKind::CommentRangeEnd,
            status: TrackStatus::Normal,
            text: None,
            handle: None,
            metadata: Some(OpaqueMetadata::NoteReference {
                reference_id: "0".to_string(),
            }),
        });
        assert!(to_extended_markdown(&end).contains("<comment_end id=0/>"));

        // The reference marker uses the SAME tag an imported commentReference
        // already renders as — true parity, not a second vocabulary.
        let reference = one_opaque_block(SegmentView::Opaque {
            id: NodeId::from("0"),
            kind: OpaqueAnchorKind::Comment,
            status: TrackStatus::Normal,
            text: None,
            handle: None,
            metadata: Some(OpaqueMetadata::NoteReference {
                reference_id: "0".to_string(),
            }),
        });
        assert!(to_extended_markdown(&reference).contains("<comment id=0/>"));
    }

    #[test]
    fn opaque_block_label_surfaces_in_markdown() {
        // Domain rule: a quarantined-nested-tracked-changes block must never
        // read as indistinguishable from any other opaque block. Before this
        // fix, `<obj id=.. kind=opaque/>` carried no further signal.
        let view = DocumentView {
            blocks: vec![BlockView {
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
            }],
        };
        let md = to_extended_markdown(&view);
        assert!(
            md.contains(r#"label="quarantined_nested_tracked_changes""#),
            "the quarantine identity must reach the markdown render: {md}"
        );

        // No silent fallback: absence of a label omits the attribute rather
        // than inventing an empty one.
        let mut unlabeled = view;
        unlabeled.blocks[0].opaque_label = None;
        let md = to_extended_markdown(&unlabeled);
        assert!(!md.contains("label="), "{md}");
    }
}
