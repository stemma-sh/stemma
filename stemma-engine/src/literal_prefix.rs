//! Expand source prefix fragments without moving their live range boundaries.
use crate::domain::{
    InlineNode, LiteralPrefixMarker, LiteralPrefixSourceRun, ParagraphNode, TextNode,
    TrackedSegment, TrackingStatus,
};

/// Source geometry to restore after the same literal label is re-extracted.
pub(crate) struct PrefixGeometry {
    label: String,
    leading: Option<i32>,
    trailing: Option<i32>,
}

impl PrefixGeometry {
    /// Re-extraction lacks the source style/tab-stop context. Retain its
    /// proven geometry only when the same literal label survived projection.
    pub(crate) fn restore(self, paragraph: &mut ParagraphNode) {
        if paragraph.literal_prefix.as_deref() == Some(self.label.as_str()) {
            paragraph.literal_prefix_leading_tab_twips = self.leading;
            paragraph.literal_prefix_trailing_tab_stop_twips = self.trailing;
        }
    }
}

pub(crate) fn has_positioned_prefix(paragraph: &ParagraphNode) -> bool {
    paragraph.literal_prefix.is_some()
        && paragraph
            .literal_prefix_leading_rpr
            .as_deref()
            .is_some_and(|p| p.source_runs.iter().any(|s| s.preceding_marker.is_some()))
}

/// Rewrite source-position references BEFORE their live owners are removed.
/// A fragment follows the nearest surviving predecessor, or the paragraph
/// start if every earlier marker was removed. This preserves text order;
/// it does not discard or guess a missing reference at serialization time.
pub(crate) fn remove_marker_references(
    paragraph: &mut ParagraphNode,
    removed: &[LiteralPrefixMarker],
) {
    if removed.is_empty() {
        return;
    }
    let mut preceding = None;
    let mut replacements = Vec::new();
    for marker in paragraph
        .segments
        .iter()
        .flat_map(|s| &s.inlines)
        .filter_map(LiteralPrefixMarker::from_inline)
    {
        if removed.contains(&marker) {
            replacements.push((marker, preceding.clone()));
        } else {
            preceding = Some(marker);
        }
    }
    for marker in removed {
        assert_eq!(
            replacements.iter().filter(|(key, _)| key == marker).count(),
            1,
            "removed prefix marker must have one live owner in {}",
            paragraph.id.0
        );
    }
    if let Some(prefix) = &mut paragraph.literal_prefix_leading_rpr {
        for source in &mut prefix.source_runs {
            if let Some(marker) = &source.preceding_marker
                && let Some((_, predecessor)) = replacements.iter().find(|(key, _)| key == marker)
            {
                source.preceding_marker = predecessor.clone();
            }
        }
    }
}

pub(crate) fn expand_positioned_prefix(paragraph: &mut ParagraphNode) -> Option<PrefixGeometry> {
    expand_positioned_prefix_with_status(paragraph, TrackingStatus::Normal)
}

pub(crate) fn expand_positioned_prefix_with_status(
    paragraph: &mut ParagraphNode,
    prefix_status: TrackingStatus,
) -> Option<PrefixGeometry> {
    if !has_positioned_prefix(paragraph) {
        return None;
    }
    let sources = paragraph
        .literal_prefix_leading_rpr
        .as_deref()
        .expect("positioned prefix requires source-run provenance")
        .source_runs
        .clone();
    Some(expand_source_runs(paragraph, sources, prefix_status))
}

/// Return a hoisted typed label to the paragraph text it was hoisted from, as
/// untracked characters ahead of the body. A typed label is the paragraph's
/// own text (§17.3.3.31 `w:t`), not numbering: an edit that rewrites the label
/// rewrites these characters, and the redline records that like any other
/// text change. The consumed source runs come back exactly as they were typed;
/// a model without source-run provenance gets one run in the label's
/// formatting, as the serializer would write it.
pub(crate) fn inline_literal_prefix(paragraph: &mut ParagraphNode) {
    let label = paragraph
        .literal_prefix
        .clone()
        .unwrap_or_else(|| panic!("{} has no typed label to inline", paragraph.id.0));
    let recorded = paragraph
        .literal_prefix_leading_rpr
        .as_deref()
        .map(|provenance| provenance.source_runs.clone())
        .unwrap_or_default();
    let sources = if recorded.is_empty() {
        vec![LiteralPrefixSourceRun {
            text: crate::tracked_model::materialized_prefix_text(&label, paragraph),
            marks: paragraph.literal_prefix_marks.clone(),
            style_props: paragraph.literal_prefix_style_props.clone(),
            rpr_authored: paragraph.literal_prefix_rpr_authored,
            source_run_attrs: Vec::new(),
            joins_body: false,
            preceding_marker: None,
        }]
    } else {
        recorded
    };
    expand_source_runs(paragraph, sources, TrackingStatus::Normal);
}

/// Put the label's consumed source runs back into the inline stream, each
/// after the marker that preceded it (or at the paragraph start), with
/// `prefix_status`, and clear the hoisted label.
fn expand_source_runs(
    paragraph: &mut ParagraphNode,
    sources: Vec<LiteralPrefixSourceRun>,
    prefix_status: TrackingStatus,
) -> PrefixGeometry {
    let geometry = PrefixGeometry {
        label: paragraph.literal_prefix.clone().expect("a hoisted label"),
        leading: paragraph.literal_prefix_leading_tab_twips,
        trailing: paragraph.literal_prefix_trailing_tab_stop_twips,
    };
    for (index, source) in sources.iter().enumerate().rev() {
        let location = if let Some(marker) = &source.preceding_marker {
            let found: Vec<_> = paragraph
                .segments
                .iter()
                .enumerate()
                .flat_map(|(s, segment)| {
                    segment
                        .inlines
                        .iter()
                        .enumerate()
                        .filter_map(move |(i, inline)| {
                            (LiteralPrefixMarker::from_inline(inline).as_ref() == Some(marker))
                                .then_some((s, i + 1))
                        })
                })
                .collect();
            assert_eq!(
                found.len(),
                1,
                "prefix boundary {marker:?} must have one live owner in {}",
                paragraph.id.0
            );
            found[0]
        } else {
            (0, 0)
        };
        let (segment_index, inline_index) = location;
        if source.joins_body
            && let Some(segment) = paragraph.segments.get_mut(segment_index)
            && segment.status == prefix_status
            && let Some(InlineNode::Text(text)) = segment.inlines.get_mut(inline_index)
            && (
                &text.marks,
                &text.style_props,
                text.rpr_authored,
                &text.source_run_attrs,
            ) == (
                &source.marks,
                &source.style_props,
                source.rpr_authored,
                &source.source_run_attrs,
            )
        {
            text.text.insert_str(0, &source.text);
            continue;
        }
        let inline = InlineNode::from(TextNode {
            id: crate::domain::NodeId::from(format!(
                "{}_positioned_prefix_{index}",
                paragraph.id.0
            )),
            text_role: None,
            text: source.text.clone(),
            marks: source.marks.clone(),
            style_props: source.style_props.clone(),
            rpr_authored: source.rpr_authored,
            source_run_attrs: source.source_run_attrs.clone(),
            formatting_change: None,
        });
        if paragraph.segments.is_empty() {
            paragraph.segments.push(TrackedSegment {
                status: prefix_status.clone(),
                inlines: vec![inline],
            });
        } else if paragraph.segments[segment_index].status == prefix_status {
            paragraph.segments[segment_index]
                .inlines
                .insert(inline_index, inline);
        } else {
            // A common label fragment cannot acquire the adjacent body's
            // revision merely because it once shared a source run with it.
            let status = paragraph.segments[segment_index].status.clone();
            let tail = paragraph.segments[segment_index]
                .inlines
                .split_off(inline_index);
            let insertion = usize::from(inline_index != 0) + segment_index;
            if inline_index == 0 {
                paragraph.segments.remove(segment_index);
            }
            paragraph.segments.insert(
                insertion,
                TrackedSegment {
                    status: prefix_status.clone(),
                    inlines: vec![inline],
                },
            );
            if !tail.is_empty() {
                paragraph.segments.insert(
                    insertion + 1,
                    TrackedSegment {
                        status,
                        inlines: tail,
                    },
                );
            }
        }
    }
    paragraph.literal_prefix = None;
    paragraph.literal_prefix_leading_rpr = None;
    paragraph.literal_prefix_trailing_rpr = None;
    paragraph.rendered_text = None;
    geometry
}
